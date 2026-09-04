//! Narrow PMU clock primitive shared with in-guest Reverie backends.

use reverie::Errno;

use crate::perf::Builder;
use crate::perf::PerfCounter;
use crate::timer::PmuConfig;

/// Linux siginfo poll reasons, not poll(2) event-mask bits.
const PERF_POLL_IN: i32 = 1;
const PERF_POLL_HUP: i32 = 6;

/// A retired-conditional-branch counter owned and read by the current thread.
///
/// Unlike the ptrace timer, this counter never delivers a signal and never
/// reads another thread.  The same thread that owns the PMU event reads it via
/// `rdpmc`, which is the binding required by [`PerfCounter::ctr_value_rdpmc`].
/// In-guest backends use it to sample the guest clock at an instrumentation
/// trampoline boundary and then deduct branches retired by their own handler.
#[derive(Debug)]
pub struct InGuestRcbCounter {
    counter: PerfCounter,
}

impl InGuestRcbCounter {
    /// Create and enable an RCB clock for the calling thread.
    pub fn current_thread() -> Result<Self, Errno> {
        Self::current_thread_with_optional_syscall_gate(None)
    }

    /// Create the same current-thread RCB clock through a caller-supplied raw
    /// syscall gate. In-guest backends use this after installing seccomp so the
    /// counter's perf-event, mmap, and ioctl setup cannot recursively enter the
    /// Tool that is currently rebuilding fork-child state.
    ///
    /// # Safety
    ///
    /// The gate must preserve Linux x86-64 syscall argument/result semantics
    /// and remain callable for the lifetime of the returned counter.
    pub unsafe fn current_thread_with_syscall_gate(
        raw_syscall: unsafe fn(i64, [u64; 6]) -> i64,
    ) -> Result<Self, Errno> {
        Self::current_thread_with_optional_syscall_gate(Some(raw_syscall))
    }

    fn current_thread_with_optional_syscall_gate(
        raw_syscall: Option<unsafe fn(i64, [u64; 6]) -> i64>,
    ) -> Result<Self, Errno> {
        Self::current_thread_with_config(PmuConfig::try_new(), raw_syscall)
    }

    fn current_thread_with_config(
        config: Option<PmuConfig>,
        raw_syscall: Option<unsafe fn(i64, [u64; 6]) -> i64>,
    ) -> Result<Self, Errno> {
        let config = config.ok_or(Errno::ENODEV)?;
        let mut builder = Builder::new(0, -1);
        builder
            .sample_period(0)
            .event(config.rcb_event())
            .fast_reads(true);
        let counter = if let Some(raw_syscall) = raw_syscall {
            builder.create_with_raw_syscall(raw_syscall)?
        } else {
            builder.create()?
        };
        counter.reset()?;
        counter.enable()?;
        Ok(Self { counter })
    }

    /// Read the calling thread's current RCB count without a syscall whenever
    /// the kernel exposes the live PMU counter to user space.
    #[inline(always)]
    pub fn read(&self) -> Result<u64, Errno> {
        self.counter.ctr_value_rdpmc()
    }

    /// A bounded boundary read on the owning thread. Unavailability, PMU
    /// descheduling, and seqlock contention are explicit, never repaired by
    /// reducing time. Unlike `read`, this cannot fall back to a syscall.
    #[inline(always)]
    pub fn sample_once(&self) -> crate::InGuestRcbSample {
        self.counter.sample_rdpmc_once()
    }
}

/// A reusable, thread-bound notification counter, separate from the RCB clock.
///
/// Like the ptrace timer, this uses PERIOD/ENABLE rather than accumulating
/// REFRESH credits. The logical deadline is one-shot; the PMU notifications are
/// wakeups, not proof that a deadline was reached. Pending notifications are not
/// drained by DISABLE. They are reconciled against the current guest-only clock,
/// with early wakeups rearmed and overshoot reported explicitly. Deferred handler
/// work must carry its captured arm ID; the kernel siginfo does not carry it.
/// Keep the fd protected for the lifetime of this timer. Closing it and reusing
/// the number still requires the caller's signal teardown/drain protocol.
///
/// Construction and destruction belong outside signal handlers. After setup,
/// arm/disarm and source matching have no locks, allocation, lazy initialization,
/// retry loops, or panic paths. The supplied syscall gate must share those
/// properties. The caller must exclude reentrant access while changing state.
#[derive(Debug)]
pub struct InGuestRcbTimer {
    counter: PerfCounter,
    signal: reverie::Signal,
    generation: u64,
    deadline: Option<crate::InGuestRcbDeadline>,
    skid_margin: u64,
    thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl InGuestRcbTimer {
    /// Create a disabled notification event for the calling thread.
    ///
    /// # Safety
    /// The trusted gate must preserve raw Linux syscall semantics and remain
    /// callable, async-signal-safe and non-reentrant for this object's lifetime.
    /// Reserve/block the signal and protect the fd before arming. Fork children
    /// must reconstruct thread-local PMU state, not use an inherited timer.
    pub unsafe fn current_thread_with_syscall_gate(
        raw_syscall: unsafe fn(i64, [u64; 6]) -> i64,
        signal: reverie::Signal,
    ) -> Result<Self, Errno> {
        Self::current_thread_with_config(PmuConfig::try_new(), raw_syscall, signal)
    }

    fn current_thread_with_config(
        config: Option<PmuConfig>,
        raw_syscall: unsafe fn(i64, [u64; 6]) -> i64,
        signal: reverie::Signal,
    ) -> Result<Self, Errno> {
        if matches!(signal, reverie::Signal::SIGKILL | reverie::Signal::SIGSTOP) {
            return Err(Errno::EINVAL);
        }
        let config = config.ok_or(Errno::ENODEV)?;
        let thread = Errno::from_ret(unsafe { raw_syscall(libc::SYS_gettid, [0; 6]) } as usize)?;
        let mut builder = Builder::new(0, -1);
        builder.event(config.rcb_event()).sample_period(1);
        let counter = builder.create_with_raw_syscall(raw_syscall)?;
        counter.set_signal_delivery(reverie::Tid::from_raw(thread as i32), signal)?;
        Ok(Self {
            counter,
            signal,
            generation: 0,
            deadline: None,
            skid_margin: config.skid_margin(),
            thread_bound: std::marker::PhantomData,
        })
    }

    /// Replace the request with a deadline `rcbs` after an accounted guest clock.
    /// Returns a local arm ID, not a kernel-carried notification identity.
    /// Zero/overflow leave the previous arm unchanged. Control failures cancel
    /// the logical request; the caller must fail closed, even if DISABLE failed.
    /// Programming touches only the notification counter, never the guest clock.
    pub fn arm_after(&mut self, clock: u64, rcbs: u64) -> Result<u64, Errno> {
        if rcbs == 0 {
            return Err(Errno::EINVAL);
        }
        let deadline = crate::InGuestRcbDeadline::new(clock, rcbs).ok_or(Errno::EOVERFLOW)?;
        let generation = self.generation.checked_add(1).ok_or(Errno::EOVERFLOW)?;
        self.disarm()?;
        self.generation = generation;
        self.program_notification(rcbs)?;
        self.deadline = Some(deadline);
        Ok(generation)
    }

    fn program_notification(&self, remaining: u64) -> Result<(), Errno> {
        self.counter.reset()?;
        self.counter
            .set_period(remaining.saturating_sub(self.skid_margin).max(1))?;
        self.counter.enable()
    }

    /// Cancel the logical deadline and disable notifications. Idempotent on
    /// success, including before the first arm. Does not drain pending signals.
    pub fn disarm(&mut self) -> Result<(), Errno> {
        self.deadline = None;
        self.counter.disable()
    }

    /// Capture this at handler entry and carry it with any deferred callback.
    pub fn arm_id(&self) -> Option<u64> {
        self.deadline.map(|_| self.generation)
    }

    /// Reconcile an owned notification with the current accounted guest clock.
    /// Call only after `matches_notification`, with exclusive non-reentrant
    /// access. Old arm IDs and cancelled requests never disable a newer arm.
    /// An early kernel signal may have the current ID: the clock check, not the
    /// ID, makes this safe. Far-early wakeups rearm for the same absolute target.
    /// Within the empirical skid margin, leave notifications disabled and return
    /// Remaining for caller-owned precise stepping. Recheck after stepping.
    /// Reached/Overshot consume the request exactly once; Overshot is NOT success.
    pub fn observe_notification(
        &mut self,
        arm: u64,
        clock: u64,
    ) -> Result<crate::InGuestRcbDeadlineStatus, Errno> {
        use crate::InGuestRcbDeadlineStatus;

        let Some(deadline) = self.deadline.filter(|_| arm == self.generation) else {
            return Ok(InGuestRcbDeadlineStatus::Cancelled);
        };
        self.deadline = None;
        self.counter.disable()?;
        let status = deadline.status(clock);
        if let InGuestRcbDeadlineStatus::Remaining(remaining) = status {
            if remaining > self.skid_margin {
                self.program_notification(remaining)?;
            }
            self.deadline = Some(deadline);
        }
        Ok(status)
    }

    /// Match only the configured signal and perf poll source. This does not
    /// authenticate freshness, deadline readiness, or userspace-queued events.
    pub fn matches_notification(&self, signal: i32, code: i32, fd: i32) -> bool {
        signal == self.signal as i32
            && (code == PERF_POLL_IN || code == PERF_POLL_HUP)
            && fd == self.raw_fd()
    }

    /// The caller must protect this fd against guest close/dup/replacement.
    pub fn raw_fd(&self) -> i32 {
        self.counter.raw_fd()
    }

    /// An empirical correction margin, NOT a guaranteed maximum skid bound.
    pub fn skid_margin_hint(&self) -> u64 {
        self.skid_margin
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    thread_local! {
        static CALLS: std::cell::RefCell<Vec<(i64, u64, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
        static FAIL_PERIOD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static FAIL_COMMAND: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    }

    unsafe fn gate(syscall: i64, arguments: [u64; 6]) -> i64 {
        use perf_event_open_sys::bindings as perf;
        let argument = if syscall == libc::SYS_fcntl && arguments[1] == 15 {
            assert_eq!(unsafe { *(arguments[2] as *const i32) }, 0);
            unsafe { *((arguments[2] as *const i32).add(1)) as u64 }
        } else if syscall == libc::SYS_ioctl && arguments[1] == perf::PERIOD as u64 {
            unsafe { *(arguments[2] as *const u64) }
        } else {
            arguments[2]
        };
        CALLS.with_borrow_mut(|calls| calls.push((syscall, arguments[1], argument)));
        if syscall == libc::SYS_ioctl && FAIL_COMMAND.get() == Some(arguments[1]) {
            -(libc::EIO as i64)
        } else if syscall == libc::SYS_perf_event_open {
            let attributes = unsafe { &*(arguments[0] as *const perf::perf_event_attr) };
            assert_eq!(attributes.disabled(), 1);
            assert_eq!(attributes.pinned(), 1);
            assert_eq!(arguments[1], 0);
            71
        } else if syscall == libc::SYS_gettid {
            123
        } else if syscall == libc::SYS_ioctl
            && arguments[1] == perf::PERIOD as u64
            && FAIL_PERIOD.get()
        {
            -(libc::EINVAL as i64)
        } else {
            0
        }
    }

    fn notification_timer() -> InGuestRcbTimer {
        CALLS.with_borrow_mut(Vec::clear);
        FAIL_PERIOD.set(false);
        FAIL_COMMAND.set(None);
        let counter = Builder::new(0, -1)
            .sample_period(1)
            .create_with_raw_syscall(gate)
            .unwrap();
        InGuestRcbTimer {
            counter,
            signal: reverie::Signal::SIGSTKFLT,
            generation: 0,
            deadline: None,
            skid_margin: 100,
            thread_bound: std::marker::PhantomData,
        }
    }

    #[test]
    fn construction_is_disabled_and_signal_setup_uses_trusted_gate() {
        CALLS.with_borrow_mut(Vec::clear);
        FAIL_PERIOD.set(false);
        let timer = InGuestRcbTimer::current_thread_with_config(
            PmuConfig::try_from_family_model(0x06, 0x3c),
            gate,
            reverie::Signal::SIGSTKFLT,
        )
        .unwrap();
        assert_eq!(timer.arm_id(), None);
        assert_eq!(timer.raw_fd(), 71);
        CALLS.with_borrow(|calls| {
            assert_eq!(calls.len(), 5);
            assert_eq!(calls[0].0, libc::SYS_gettid);
            assert_eq!(calls[1].0, libc::SYS_perf_event_open);
            assert_eq!(calls[2], (libc::SYS_fcntl, 15, 123));
            assert_eq!(
                calls[3],
                (libc::SYS_fcntl, libc::F_SETFL as u64, libc::O_ASYNC as u64)
            );
            assert_eq!(
                calls[4],
                (libc::SYS_fcntl, 10, reverie::Signal::SIGSTKFLT as u64)
            );
        });
    }

    #[test]
    fn reusable_notifications_use_trusted_period_enable_and_disable() {
        use perf_event_open_sys::bindings as perf;
        let mut timer = notification_timer();
        assert_eq!(timer.arm_after(1000, 0), Err(Errno::EINVAL));
        assert_eq!(timer.arm_after(1000, 123), Ok(1));
        assert_eq!(timer.disarm(), Ok(()));
        assert_eq!(timer.arm_after(2000, 456), Ok(2));
        CALLS.with_borrow(|calls| {
            assert_eq!(
                &calls[1..],
                &[
                    (libc::SYS_ioctl, perf::DISABLE as u64, 0),
                    (libc::SYS_ioctl, perf::RESET as u64, 0),
                    (libc::SYS_ioctl, perf::PERIOD as u64, 23),
                    (libc::SYS_ioctl, perf::ENABLE as u64, 0),
                    (libc::SYS_ioctl, perf::DISABLE as u64, 0),
                    (libc::SYS_ioctl, perf::DISABLE as u64, 0),
                    (libc::SYS_ioctl, perf::RESET as u64, 0),
                    (libc::SYS_ioctl, perf::PERIOD as u64, 356),
                    (libc::SYS_ioctl, perf::ENABLE as u64, 0),
                ]
            );
        });
    }

    #[test]
    fn failed_period_cancels_without_enabling_and_allows_retry() {
        let mut timer = notification_timer();
        FAIL_PERIOD.set(true);
        assert_eq!(timer.arm_after(1000, 123), Err(Errno::EINVAL));
        assert_eq!(timer.arm_id(), None);
        CALLS.with_borrow(|calls| assert_eq!(calls.len(), 4));
        assert_eq!(timer.disarm(), Ok(()));
        FAIL_PERIOD.set(false);
        assert_eq!(timer.arm_after(1000, 123), Ok(2));
    }

    #[test]
    fn notification_requires_signal_poll_source_and_protected_fd() {
        let timer = notification_timer();
        let signal = reverie::Signal::SIGSTKFLT as i32;
        assert!(timer.matches_notification(signal, PERF_POLL_HUP, 71));
        assert!(timer.matches_notification(signal, PERF_POLL_IN, 71));
        assert!(!timer.matches_notification(signal, libc::SI_USER, 71));
        assert!(!timer.matches_notification(signal, libc::SI_TKILL, 71));
        assert!(!timer.matches_notification(signal, PERF_POLL_HUP, 72));
        assert!(!timer.matches_notification(signal + 1, PERF_POLL_HUP, 71));
        assert!(!timer.matches_notification(signal, i32::from(libc::POLLHUP), 71));
    }

    #[test]
    fn cancelled_and_old_callbacks_do_not_touch_the_new_arm() {
        use crate::InGuestRcbDeadlineStatus::Cancelled;
        let mut timer = notification_timer();
        let old = timer.arm_after(1000, 200).unwrap();
        timer.disarm().unwrap();
        assert_eq!(timer.observe_notification(old, 1200), Ok(Cancelled));
        let current = timer.arm_after(1100, 300).unwrap();
        let before = CALLS.with_borrow(Vec::len);
        assert_eq!(timer.observe_notification(old, 1400), Ok(Cancelled));
        assert_eq!(timer.arm_id(), Some(current));
        assert_eq!(CALLS.with_borrow(Vec::len), before);
    }

    #[test]
    fn late_kernel_wakeup_is_checked_against_current_deadline() {
        use crate::InGuestRcbDeadlineStatus::Cancelled;
        use crate::InGuestRcbDeadlineStatus::Reached;
        use crate::InGuestRcbDeadlineStatus::Remaining;
        let mut timer = notification_timer();
        timer.arm_after(1000, 200).unwrap();
        let current = timer.arm_after(1100, 300).unwrap();
        assert_eq!(
            timer.observe_notification(current, 1150),
            Ok(Remaining(250))
        );
        assert_eq!(timer.arm_id(), Some(current));
        assert_eq!(timer.observe_notification(current, 1399), Ok(Remaining(1)));
        assert_eq!(timer.observe_notification(current, 1400), Ok(Reached));
        assert_eq!(timer.observe_notification(current, 1400), Ok(Cancelled));
        assert_eq!(timer.arm_id(), None);
    }

    #[test]
    fn overshoot_is_not_precise_success_and_consumes_the_request() {
        use crate::InGuestRcbDeadlineStatus::Cancelled;
        use crate::InGuestRcbDeadlineStatus::Overshot;
        let mut timer = notification_timer();
        let arm = timer.arm_after(1000, 200).unwrap();
        assert_eq!(timer.observe_notification(arm, 1201), Ok(Overshot(1)));
        assert_eq!(timer.observe_notification(arm, 1201), Ok(Cancelled));
    }

    #[test]
    fn invalid_replacements_preserve_the_active_request() {
        let mut timer = notification_timer();
        let arm = timer.arm_after(1000, 200).unwrap();
        let before = CALLS.with_borrow(Vec::len);
        assert_eq!(timer.arm_after(1000, 0), Err(Errno::EINVAL));
        assert_eq!(timer.arm_after(u64::MAX, 1), Err(Errno::EOVERFLOW));
        assert_eq!(timer.arm_id(), Some(arm));
        assert_eq!(CALLS.with_borrow(Vec::len), before);
        timer.generation = u64::MAX;
        assert_eq!(timer.arm_after(1000, 200), Err(Errno::EOVERFLOW));
        assert_eq!(CALLS.with_borrow(Vec::len), before);
    }

    #[test]
    fn failure_to_rearm_early_wakeup_cancels_the_logical_request() {
        let mut timer = notification_timer();
        let arm = timer.arm_after(1000, 300).unwrap();
        FAIL_PERIOD.set(true);
        assert_eq!(timer.observe_notification(arm, 1001), Err(Errno::EINVAL));
        assert_eq!(timer.arm_id(), None);
    }

    #[test]
    fn all_programming_failures_cancel_and_propagate_without_accepting_callbacks() {
        use perf_event_open_sys::bindings as perf;
        for request in [perf::DISABLE, perf::RESET, perf::PERIOD, perf::ENABLE] {
            let mut timer = notification_timer();
            let arm = timer.arm_after(1000, 300).unwrap();
            FAIL_COMMAND.set(Some(request as u64));
            assert_eq!(timer.arm_after(1100, 300), Err(Errno::EIO));
            assert_eq!(timer.arm_id(), None);
            assert_eq!(
                timer.observe_notification(arm, 1300),
                Ok(crate::InGuestRcbDeadlineStatus::Cancelled)
            );
            FAIL_COMMAND.set(None);
            assert!(timer.arm_after(1200, 300).is_ok());
            timer.disarm().unwrap();
        }
    }

    #[test]
    fn hardware_notifications_rearm_and_cancel() {
        std::thread::spawn(|| {
            unsafe fn raw_gate(syscall: i64, arguments: [u64; 6]) -> i64 {
                let result = unsafe {
                    libc::syscall(syscall, arguments[0], arguments[1], arguments[2], arguments[3], arguments[4], arguments[5])
                };
                if result == -1 {
                    -i64::from(unsafe { *libc::__errno_location() })
                } else {
                    result
                }
            }

            let signal = reverie::Signal::SIGSTKFLT;
            let mut mask = unsafe { std::mem::zeroed::<libc::sigset_t>() };
            unsafe {
                assert_eq!(libc::sigemptyset(&mut mask), 0);
                assert_eq!(libc::sigaddset(&mut mask, signal as i32), 0);
                assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()), 0);
            }
            let mut timer = unsafe { InGuestRcbTimer::current_thread_with_syscall_gate(raw_gate, signal) }
                .expect("hardware PMU unavailable or notification setup failed; not a passing hardware test");
            let clock = InGuestRcbCounter::current_thread().expect("hardware clock setup failed");
            let sample = || {
                for _ in 0..100 {
                    let before = clock.counter.ctr_value().expect("independent perf read before sample");
                    match clock.sample_once() {
                        crate::InGuestRcbSample::Value(count) => {
                            let after = clock.counter.ctr_value().expect("independent perf read after sample");
                            assert!(before <= count && count <= after,
                                "sample_once {count} outside independent perf read bracket [{before}, {after}]");
                            return count;
                        }
                        crate::InGuestRcbSample::Retry => continue,
                        failure => panic!("live clock sampling unavailable: {failure:?}"),
                    }
                }
                panic!("live clock metadata remained contended");
            };
            let mut previous = sample();
            let descriptor = timer.raw_fd();
            for iteration in 0..3 {
                let before_arm = sample();
                timer.arm_after(0, 10000).unwrap();
                let after_arm = sample();
                assert!(after_arm > before_arm, "clock must advance immediately across notification reset");
                timer.arm_after(0, 10000).unwrap();
                let after_rearm = sample();
                assert!(after_rearm > after_arm, "clock must advance immediately across rearm");
                assert!(sample() > after_rearm, "back-to-back boundary reads must advance without bulk work");
                let mut received = false;
                for _ in 0..100 {
                    for branch in 0..10000 {
                        std::hint::black_box(branch);
                    }
                    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
                    let timeout = libc::timespec { tv_sec: 0, tv_nsec: 0 };
                    let result = unsafe { libc::sigtimedwait(&mask, &mut info, &timeout) };
                    if result == signal as i32 {
                        assert!(matches!(info.si_code, PERF_POLL_IN | PERF_POLL_HUP));
                        received = true;
                        break;
                    }
                    assert_eq!(result, -1);
                    assert_eq!(unsafe { *libc::__errno_location() }, libc::EAGAIN);
                }
                assert!(received, "no hardware notification on arm {iteration}");
                let before_cancel = sample();
                timer.disarm().unwrap();
                assert!(sample() > before_cancel, "clock must advance immediately across cancellation");
                assert_eq!(timer.arm_id(), None);
                assert_eq!(timer.raw_fd(), descriptor);
                let timeout = libc::timespec { tv_sec: 0, tv_nsec: 0 };
                let mut empty = false;
                for _ in 0..100 {
                    let result = unsafe { libc::sigtimedwait(&mask, std::ptr::null_mut(), &timeout) };
                    if result == -1 {
                        assert_eq!(unsafe { *libc::__errno_location() }, libc::EAGAIN);
                        empty = true;
                        break;
                    }
                    assert_eq!(result, signal as i32);
                }
                assert!(empty, "notification queue did not empty after cancellation");
                for branch in 0..100000 {
                    std::hint::black_box(branch);
                }
                assert_eq!(unsafe { libc::sigtimedwait(&mask, std::ptr::null_mut(), &timeout) }, -1);
                assert_eq!(unsafe { *libc::__errno_location() }, libc::EAGAIN);
                let current = sample();
                assert!(current > previous, "notification resets must not reset or freeze the clock");
                previous = current;
            }
        }).join().unwrap();
    }

    #[test]
    fn model_cf_is_refused_before_perf_event_open() {
        let config = PmuConfig::try_from_family_model(0x06, 0xcf);
        let error = InGuestRcbCounter::current_thread_with_config(config, None).unwrap_err();
        assert_eq!(error, Errno::ENODEV);
    }
}
