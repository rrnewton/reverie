/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A preemption timer for one guest thread: a second guest-only perf event
//! whose overflow only kicks the vCPU out of `KVM_RUN`, and the state of the
//! one event a timer request names, decided from the thread's clock.
//!
//! The clock (see `clock.rs`) is a counting event for the retired conditional
//! branches (RCBs) of one guest thread in guest user mode. The timer event has
//! the same encoding and the same filter, so it counts exactly what the clock
//! counts, but it samples: at its overflow Linux sends [`KICK_SIGNAL`] to the
//! vCPU's host thread, which ends `KVM_RUN` with `EINTR`. Ending `KVM_RUN` is
//! the signal's only effect. Every decision is made from the clock value read
//! at the exit, never from the signal or the timer's own count, so a late,
//! spurious or lost kick can change when the backend looks, never what it
//! decides at a given count.
//!
//! Two events, not one. Linux throttles a sampling event that overflows more
//! often than `perf_event_max_sample_rate` allows, and a throttled event stops
//! counting. A sampling clock would then lose counts silently: a stopped but
//! scheduled event still accrues `time_running`, so the clock's residency
//! check would pass. A counting event is never throttled. The timer also stays
//! out of the clock's perf group, because throttling stops a whole group. A
//! throttled timer is harmless: the overflow that throttles it is still
//! notified, and the next `PERF_EVENT_IOC_PERIOD` un-throttles it.
//!
//! Programming is lazy. A thread that has never armed a timer has no timer
//! event, and its entries issue no syscall for one. While a request is armed,
//! an entry reprograms the event (one `PERF_EVENT_IOC_PERIOD`) only when it has
//! no pending overflow, when the clock has reached that overflow, or when the
//! overflow would come later than the request now needs. A request whose
//! target is unchanged costs nothing per entry. The pending overflow is a
//! clock value, so it is kept only while the event counts exactly what the
//! clock counts. An entry that runs the clock with the event disabled (an
//! unarmed entry on pinned placement, or one after another binding disabled
//! both events) forgets it, and the next armed entry programs the event
//! afresh. A distance longer than [`MAX_PERIOD`] is programmed in pieces.
//!
//! Between the overflow and the exit that the kick causes, the guest keeps
//! running for the interrupt's latency (the skid), so a precise request is
//! first programmed one skid margin short of its target. From that close to
//! the target, the event is programmed at the target itself: this backend
//! cannot yet run the guest to the exact count, so the next kick lands at or
//! after the target.
//!
//! An exit that finds a precise request's target reached is late, even at
//! exactly the target: an asynchronous exit does not stop the guest at the
//! instruction boundary right after the target branch, and non-branch
//! instructions may have run past it. Such an exit is recorded as every
//! backend records a late timer event: the canonical
//! [`reverie::SKID_OVERSHOOT_MARKER`] line and the process-global witness
//! count.
//!
//! Nothing outside this crate's tests requests a timer yet. `KvmGuest`'s
//! `set_timer` and `set_timer_precise` still arm nothing, and
//! `BackendCapabilities::KVM` keeps `delivers_preemption_timer_events` false,
//! so no Tool can rely on a timer event from this backend.
//!
//! Limits, for the work that wires requests to this timer and delivers its
//! events:
//! - A late kick reaches host code. The interrupt behind a kick can be held
//!   past the exit it would have caused, even across several further exits,
//!   and the signal then lands while the vCPU thread runs host code. The
//!   empty handler interrupts the host call in progress, and LinuxEmu passes
//!   the result to the guest: its host `nanosleep` and a relative host
//!   `clock_nanosleep` return `EINTR` with the remainder, an absolute one
//!   returns `EINTR`, its blocking host `ppoll` returns `EINTR`, a
//!   zero-timeout host `poll` returns `EINTR` when nothing is ready, and a
//!   blocking host `read` or `write` that has moved some bytes returns a short
//!   count. On a PREEMPT_RT kernel perf sends the signal from a thread, so
//!   late kicks are routine there. The planned fix keeps the kick signal
//!   blocked in host code, unblocks it only inside `KVM_RUN` through
//!   `KVM_SET_SIGNAL_MASK`, and drains a kick left pending at an `EINTR` exit.
//! - Nothing checks the timer event's residency. A pinned event that the PMU
//!   cannot schedule never kicks: a guest that makes no exit runs on, and its
//!   next exit records the request late.
//! - Only counted entries displace another binding's events. An uncounted
//!   vCPU on the same host task leaves an enabled timer counting its branches:
//!   the timer overflows early, inside that vCPU's run, reloads a whole period
//!   and kicks the armed thread late, by up to that period, so the request
//!   comes due at a later exit and is recorded late.
//! - An exec keeps the vCPU and its clock, and so an armed request; wiring
//!   requests to this timer must cancel it there.
//! - Nothing refuses a PREEMPT_RT kernel or nested virtualization, where the
//!   kick's latency has not been measured.
//! - The kick signal is unblocked when a timer event is opened and is not
//!   checked again; code that blocks it later on that thread stops the kicks.

use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::OnceLock;

use perf_event_open_sys::bindings as perf;
use perf_event_open_sys::ioctls;
use reverie::pmu_config::SINGLESTEP_TIMEOUT_RCBS;

use crate::Error;
use crate::Result;

/// The host signal a timer overflow sends to its vCPU thread: Reverie's
/// [`reverie::PERF_EVENT_SIGNAL`], `SIGSTKFLT`. It is separate from the entry
/// signal (real-time signal 64), whose handler refuses any instance that no
/// closer recorded.
pub(crate) const KICK_SIGNAL: libc::c_int = libc::SIGSTKFLT;

const _: () = assert!(reverie::PERF_EVENT_SIGNAL as libc::c_int == KICK_SIGNAL);

/// The longest period programmed at once. Linux refuses a period with bit 63
/// set, and x86 programs at most its counter's maximum period at a time and
/// reloads the rest silently. A longer distance is programmed in pieces of
/// this size, each forgotten and reprogrammed when the clock reaches it.
pub(crate) const MAX_PERIOD: u64 = 1 << 60;

/// A period long enough that the event never overflows: at this length the
/// silent reloads take days of guest execution to run out.
const QUIET_PERIOD: u64 = MAX_PERIOD;

/// Linux programs a sampling period below 2 as 2: a period of 1 cannot
/// overflow at the next branch.
const MIN_PERIOD: u64 = 2;

// Not available in the libc crate.
const F_SETSIG: libc::c_int = 10;
const F_SETOWN_EX: libc::c_int = 15;
const F_OWNER_TID: libc::c_int = 0;

#[repr(C)]
struct FOwnerEx {
    type_: libc::c_int,
    pid: libc::pid_t,
}

fn failure(message: impl Into<String>) -> Error {
    Error::GuestClock(message.into())
}

fn host_failure(operation: &str) -> Error {
    failure(format!(
        "preemption timer: {operation}: {}",
        std::io::Error::last_os_error()
    ))
}

/// Serializes the tests that read or drain the process-global witness count
/// (`reverie::take_skid_overshoot_count`), which a plain `cargo test` shares
/// among the threads of one process.
#[cfg(test)]
pub(crate) static WITNESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Takes the witness count's turn, drained to zero.
#[cfg(test)]
pub(crate) fn witness_turn() -> std::sync::MutexGuard<'static, ()> {
    let guard = WITNESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = reverie::take_skid_overshoot_count();
    guard
}

#[cfg(test)]
thread_local! {
    /// Kicks this thread's handler has taken.
    pub(crate) static KICKS_TAKEN: std::sync::atomic::AtomicU64 =
        const { std::sync::atomic::AtomicU64::new(0) };
    /// Host syscalls this thread issued for a timer: signal reservation, event
    /// creation and setup, and every programming ioctl.
    pub(crate) static TIMER_SYSCALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn count_syscall() {
    #[cfg(test)]
    TIMER_SYSCALLS.with(|count| count.set(count.get() + 1));
}

extern "C" fn kick(_signal: libc::c_int) {
    // Nothing to do: the delivery itself ends KVM_RUN with EINTR, and every
    // decision is made from the clock. A const thread local without a
    // destructor is a plain TLS access, safe in a signal handler.
    #[cfg(test)]
    KICKS_TAKEN.with(|kicks| kicks.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
}

fn query_kick_disposition() -> std::result::Result<usize, i32> {
    count_syscall();
    // SAFETY: the action storage is initialized and outlives the call.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(KICK_SIGNAL, std::ptr::null(), &mut current) != 0 {
            return Err(std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO));
        }
        Ok(current.sa_sigaction)
    }
}

/// Reserves [`KICK_SIGNAL`] for timer kicks and unblocks it on the calling
/// thread, which is about to own a timer event.
///
/// The first call in a process installs a handler that does nothing, with
/// `SA_RESTART`, and refuses with `EBUSY` if the signal already has a handler
/// or is ignored: it has another owner. The handler is never restored. Every
/// call checks that the installed handler is still in place, so a replacement
/// is refused at the next timer this process opens. A blocked kick would not
/// end `KVM_RUN`, so the calling thread unblocks the signal. A kick that
/// arrives outside `KVM_RUN` runs the empty handler. `SA_RESTART` restarts a
/// host call that has moved no data, but Linux never restarts a sleep or a
/// poll, which returns `EINTR`, and a blocking read or write that has moved
/// some bytes returns a short count: the late-kick limit in the module
/// documentation.
pub(crate) fn reserve_kick_signal() -> Result<()> {
    static INSTALL: OnceLock<std::result::Result<usize, i32>> = OnceLock::new();
    let installed = *INSTALL.get_or_init(|| {
        if query_kick_disposition()? != libc::SIG_DFL {
            return Err(libc::EBUSY);
        }
        count_syscall();
        // SAFETY: the action storage is initialized and outlives the call;
        // the handler performs no allocation or locking.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = kick as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(KICK_SIGNAL, &action, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO));
            }
            // The address passed to the kernel, which is what a later query
            // returns. In optimized builds the linker may fold this handler
            // with an identical one; re-deriving `kick as usize` could then
            // name a different symbol.
            Ok(action.sa_sigaction)
        }
    });
    let installed = installed.map_err(|errno| {
        failure(format!(
            "preemption timer: reserve host signal SIGSTKFLT: {}",
            std::io::Error::from_raw_os_error(errno)
        ))
    })?;
    match query_kick_disposition() {
        Ok(current) if current == installed => {}
        Ok(_) => {
            return Err(failure(
                "preemption timer: host signal SIGSTKFLT disposition changed",
            ));
        }
        Err(errno) => {
            return Err(failure(format!(
                "preemption timer: query host signal SIGSTKFLT: {}",
                std::io::Error::from_raw_os_error(errno)
            )));
        }
    }
    count_syscall();
    // SAFETY: both sets are valid storage; the change affects only this thread.
    let error = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, KICK_SIGNAL);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut())
    };
    if error != 0 {
        return Err(failure(format!(
            "preemption timer: unblock host signal SIGSTKFLT: {}",
            std::io::Error::from_raw_os_error(error)
        )));
    }
    Ok(())
}

/// The sampling event that kicks one guest thread's vCPU out of `KVM_RUN`
/// when it overflows. It counts what that thread's clock counts. Nothing
/// reads its count to decide anything.
pub(crate) struct TimerEvent {
    fd: Arc<OwnedFd>,
    enabled: bool,
    /// The clock value at which the current programming next overflows, while
    /// the clock has not reached it and the event has counted every branch
    /// the clock counted since the programming.
    overflow_at: Option<u64>,
    /// A finite period is programmed. The event reloads it after each
    /// overflow, so it keeps overflowing until it is reprogrammed.
    live: bool,
    /// The clock value at the entry that opened the event, from which its
    /// own count started.
    #[cfg(test)]
    pub(crate) opened_clock: u64,
}

/// What an entry must do to the timer event for the overflow it wants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Programming {
    /// Leave the event as it is.
    Keep,
    /// Program this period, counted from the entry's clock.
    Period(u64),
    /// Program [`QUIET_PERIOD`].
    Quiet,
}

/// The programming an entry at `clock` needs for an event that next
/// overflows at `overflow_at` and that counts and keeps overflowing when
/// `active` (enabled with a finite period), so that it overflows at `wanted`,
/// or never when `wanted` is None.
///
/// A pending overflow that comes no later than `wanted` is kept: the overflow
/// it causes only makes the backend look earlier than needed, while
/// reprogramming would cost a syscall now. A new period is never shorter than
/// [`MIN_PERIOD`] and never longer than [`MAX_PERIOD`]. A disabled event
/// cannot overflow, so it need not be quieted.
fn programming(
    clock: u64,
    wanted: Option<u64>,
    overflow_at: Option<u64>,
    active: bool,
) -> Programming {
    match wanted {
        Some(wanted) if overflow_at.is_some_and(|at| clock < at && at <= wanted) => {
            Programming::Keep
        }
        Some(wanted) => {
            Programming::Period(wanted.saturating_sub(clock).clamp(MIN_PERIOD, MAX_PERIOD))
        }
        None if active => Programming::Quiet,
        None => Programming::Keep,
    }
}

impl TimerEvent {
    /// Opens a disabled, quiet timer event for host task `tid` on `cpu` (-1
    /// for any CPU) with the clock's raw RCB encoding, owned by that task:
    /// each overflow sends it [`KICK_SIGNAL`]. Reserves the signal and
    /// unblocks it on the calling thread, which must be `tid`.
    pub(crate) fn open(tid: libc::pid_t, cpu: libc::c_int, raw_event: u64) -> Result<Self> {
        reserve_kick_signal()?;
        let mut attr = perf::perf_event_attr {
            type_: perf::PERF_TYPE_RAW,
            size: std::mem::size_of::<perf::perf_event_attr>() as u32,
            config: raw_event,
            ..Default::default()
        };
        attr.__bindgen_anon_1.sample_period = QUIET_PERIOD;
        attr.__bindgen_anon_2.wakeup_events = 1;
        // The clock's filter (CounterBinding::open): guest user mode only.
        attr.set_disabled(1);
        attr.set_pinned(1);
        attr.set_exclude_host(1);
        attr.set_exclude_guest(0);
        attr.set_exclude_kernel(1);
        attr.set_exclude_user(0);
        attr.set_exclude_hv(1);
        attr.set_inherit(0);
        count_syscall();
        // SAFETY: attr describes a sampling event for this actual host task;
        // the returned descriptor is newly owned. No group: see the module
        // documentation.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &attr,
                tid,
                cpu,
                -1,
                perf::PERF_FLAG_FD_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(host_failure(&format!(
                "perf_event_open tid {tid} cpu {cpu} event {raw_event:#x}"
            )));
        }
        // SAFETY: a newly returned descriptor that nothing else owns.
        let event = Self {
            fd: Arc::new(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) }),
            enabled: false,
            overflow_at: None,
            live: false,
            #[cfg(test)]
            opened_clock: 0,
        };
        // The event is disabled and quiet, so it cannot overflow before its
        // signal and owner are set. Each overflow then queues KICK_SIGNAL for
        // this task (si_code POLL_IN), not SIGIO for the process.
        let owner = FOwnerEx {
            type_: F_OWNER_TID,
            pid: tid,
        };
        let raw = event.fd.as_raw_fd();
        count_syscall();
        // SAFETY: fcntl on a live descriptor with an integer argument.
        if unsafe { libc::fcntl(raw, F_SETSIG, KICK_SIGNAL) } < 0 {
            return Err(host_failure("set overflow signal"));
        }
        count_syscall();
        // SAFETY: fcntl reads the initialized owner, which outlives the call.
        if unsafe { libc::fcntl(raw, F_SETOWN_EX, &owner as *const FOwnerEx) } < 0 {
            return Err(host_failure("set overflow signal owner"));
        }
        count_syscall();
        // SAFETY: fcntl on a live descriptor with an integer argument.
        if unsafe { libc::fcntl(raw, libc::F_SETFL, libc::O_ASYNC) } < 0 {
            return Err(host_failure("enable overflow signal"));
        }
        Ok(event)
    }

    pub(crate) fn fd(&self) -> &Arc<OwnedFd> {
        &self.fd
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Another binding on this host task disabled the event, together with
    /// its clock, so that neither counts while another guest runs. The
    /// pending overflow stays valid while both stay disabled; an entry that
    /// runs the clock without enabling the event forgets it (see
    /// [`Self::prepare`]).
    pub(crate) fn mark_disabled(&mut self) {
        self.enabled = false;
    }

    fn set_period(&mut self, period: u64) -> Result<()> {
        let mut period = period;
        count_syscall();
        // The bindings take the period by value; the ioctl reads it through a
        // pointer.
        // SAFETY: the ioctl reads one u64 from initialized storage.
        if unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                perf::PERIOD as _,
                std::ptr::from_mut(&mut period),
            )
        } < 0
        {
            return Err(host_failure("program period"));
        }
        Ok(())
    }

    pub(crate) fn enable(&mut self) -> Result<()> {
        // Mark before the ioctl so even an ambiguous failure is cleaned up.
        self.enabled = true;
        count_syscall();
        // SAFETY: the descriptor is a live perf event.
        if unsafe { ioctls::ENABLE(self.fd.as_raw_fd(), 0) } < 0 {
            return Err(host_failure("enable"));
        }
        Ok(())
    }

    pub(crate) fn disable(&mut self) -> Result<()> {
        if self.enabled {
            count_syscall();
            // SAFETY: the descriptor is a live perf event.
            if unsafe { ioctls::DISABLE(self.fd.as_raw_fd(), 0) } < 0 {
                return Err(host_failure("disable"));
            }
            self.enabled = false;
        }
        Ok(())
    }

    /// Makes the event overflow at `wanted` (never, for None) for an entry at
    /// `clock`, with as few syscalls as [`programming`] allows, and enables it
    /// when it is wanted. With nothing wanted and nothing pending, issues none.
    ///
    /// An event left disabled for the entry misses every branch the entry
    /// retires, while the clock counts them, so its pending overflow no
    /// longer names a clock value. It is forgotten, and the next entry that
    /// wants an overflow programs one afresh.
    pub(crate) fn prepare(&mut self, clock: u64, wanted: Option<u64>) -> Result<()> {
        match programming(clock, wanted, self.overflow_at, self.enabled && self.live) {
            Programming::Keep => {}
            Programming::Period(period) => {
                self.set_period(period)?;
                self.overflow_at = Some(clock.saturating_add(period));
                self.live = true;
            }
            Programming::Quiet => {
                self.set_period(QUIET_PERIOD)?;
                self.overflow_at = None;
                self.live = false;
            }
        }
        if wanted.is_some() && !self.enabled {
            self.enable()?;
        }
        if !self.enabled {
            self.overflow_at = None;
        }
        Ok(())
    }

    /// Forgets a pending overflow that the clock read at an exit has reached.
    /// The event has overflowed and reloaded its period, so its next overflow
    /// is not one an entry relies on.
    pub(crate) fn observe_exit(&mut self, clock: u64) {
        if self.overflow_at.is_some_and(|at| clock >= at) {
            self.overflow_at = None;
        }
    }

    /// The pending overflow, for tests.
    #[cfg(test)]
    pub(crate) fn overflow_at(&self) -> Option<u64> {
        self.overflow_at
    }

    /// Records the clock value at the entry that opened the event, for tests.
    #[cfg(test)]
    pub(crate) fn opened_at(mut self, clock: u64) -> Self {
        self.opened_clock = clock;
        self
    }
}

/// Whether a request names an exact count, or any count from its target on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only tests request a timer until KvmGuest's set_timer and set_timer_precise use it"
    )
)]
pub(crate) enum TimerKind {
    /// Due exactly when the clock reaches the target, as for
    /// `Guest::set_timer_precise`.
    Precise,
    /// Due at the target or at any later count, as for `Guest::set_timer`.
    Imprecise,
}

/// A request whose target an exit reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TimerDue {
    /// The clock value the request named.
    pub(crate) target: u64,
    /// The clock value read at the exit that reached it.
    pub(crate) clock: u64,
    /// The exit came too late for the request: always for a precise request,
    /// even at exactly its target (see the module documentation), and never
    /// for an imprecise one.
    pub(crate) late: bool,
}

/// The one timer request of a guest thread, decided from its clock alone.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TimerState {
    /// No request.
    #[default]
    Idle,
    /// A request whose target the clock has not reached.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only tests request a timer until KvmGuest's set_timer and set_timer_precise use it"
        )
    )]
    Armed {
        target: u64,
        kind: TimerKind,
        /// How far short of the target a precise request is first programmed:
        /// the PMU skid margin when it was requested.
        margin: u64,
    },
    /// A request whose target an exit reached. It stays here until it is
    /// replaced or cancelled, and is recorded only once.
    Due(TimerDue),
}

impl TimerState {
    /// An armed request due when the clock, now `clock`, has advanced `ticks`
    /// RCBs. `margin` is the PMU skid margin. A request for 0 RCBs names the
    /// current count, which no exit can reach, and is refused, as is one past
    /// the clock's range.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only tests request a timer until KvmGuest's set_timer and set_timer_precise use it"
        )
    )]
    pub(crate) fn request(clock: u64, ticks: u64, kind: TimerKind, margin: u64) -> Result<Self> {
        if ticks == 0 {
            return Err(failure(
                "preemption timer: a request for 0 RCBs names the current count",
            ));
        }
        let target = clock.checked_add(ticks).ok_or_else(|| {
            failure(format!(
                "preemption timer: {ticks} RCBs from {clock} overflows the clock"
            ))
        })?;
        Ok(Self::Armed {
            target,
            kind,
            margin,
        })
    }

    /// The clock value at which the timer event should next overflow for an
    /// entry at `clock`, or None when nothing should kick the entry.
    ///
    /// A precise request more than one skid margin plus
    /// [`SINGLESTEP_TIMEOUT_RCBS`] short of its target is first programmed one
    /// margin short of it. Closer, and for an imprecise request, it is
    /// programmed at the target. The result is always at least
    /// [`MIN_PERIOD`] past `clock`, because Linux programs no shorter period.
    pub(crate) fn wanted_overflow(&self, clock: u64) -> Option<u64> {
        let Self::Armed {
            target,
            kind,
            margin,
        } = *self
        else {
            return None;
        };
        let point = match kind {
            TimerKind::Precise
                if target.saturating_sub(clock)
                    > margin.saturating_add(SINGLESTEP_TIMEOUT_RCBS) =>
            {
                target - margin
            }
            TimerKind::Precise | TimerKind::Imprecise => target,
        };
        Some(point.max(clock.saturating_add(MIN_PERIOD)))
    }

    /// Decides the exit at which the clock read `clock`: an armed request
    /// whose target it reached becomes due, and a precise one is recorded as
    /// late. Returns the request this exit made due, at most once.
    pub(crate) fn observe_exit(&mut self, clock: u64) -> Option<TimerDue> {
        let Self::Armed { target, kind, .. } = *self else {
            return None;
        };
        if clock < target {
            return None;
        }
        let due = TimerDue {
            target,
            clock,
            late: matches!(kind, TimerKind::Precise),
        };
        if due.late {
            record_late(&due);
        }
        *self = Self::Due(due);
        Some(due)
    }

    /// The target of an armed or due request.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only tests read the target until a backend delivers timer events"
        )
    )]
    pub(crate) fn target(&self) -> Option<u64> {
        match *self {
            Self::Idle => None,
            Self::Armed { target, .. } => Some(target),
            Self::Due(due) => Some(due.target),
        }
    }

    /// The request an exit made due, while it is not replaced or cancelled.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only tests read a due request until a backend delivers timer events"
        )
    )]
    pub(crate) fn due(&self) -> Option<TimerDue> {
        match *self {
            Self::Due(due) => Some(due),
            Self::Idle | Self::Armed { .. } => None,
        }
    }
}

/// Records a late precise request with the canonical marker and the
/// process-global witness count. It uses the recorder that counts an arrival
/// at exactly the target, `record_missed_if_target_reached`: an asynchronous
/// exit at the target may already be past its delivery point, the boundary
/// right after the target branch.
fn record_late(due: &TimerDue) {
    match reverie::pmu_config::try_get_pmu_config() {
        Some(config) => {
            config.record_missed_if_target_reached(due.clock, due.target, 0);
        }
        // A request takes its margin from this configuration, so it exists
        // whenever a request was armed. Should it not, keep the witness.
        None => reverie::record_skid_overshoot(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARGIN: u64 = 2_000;

    fn armed(target: u64, kind: TimerKind) -> TimerState {
        TimerState::Armed {
            target,
            kind,
            margin: MARGIN,
        }
    }

    #[test]
    fn requests_name_a_count_ahead_of_the_clock() {
        assert_eq!(
            TimerState::request(10, 5, TimerKind::Precise, MARGIN).unwrap(),
            armed(15, TimerKind::Precise)
        );
        assert!(
            TimerState::request(10, 0, TimerKind::Imprecise, MARGIN)
                .unwrap_err()
                .to_string()
                .contains("0 RCBs")
        );
        assert!(
            TimerState::request(u64::MAX - 1, 2, TimerKind::Precise, MARGIN)
                .unwrap_err()
                .to_string()
                .contains("overflows")
        );
    }

    #[test]
    fn precise_requests_are_programmed_one_margin_short_until_close() {
        let target = 100_000;
        let state = armed(target, TimerKind::Precise);
        // Far from the target: one margin short of it.
        assert_eq!(state.wanted_overflow(0), Some(target - MARGIN));
        let edge = target - MARGIN - SINGLESTEP_TIMEOUT_RCBS;
        assert_eq!(state.wanted_overflow(edge - 1), Some(target - MARGIN));
        // Within the margin plus the single-step allowance: at the target.
        assert_eq!(state.wanted_overflow(edge), Some(target));
        assert_eq!(state.wanted_overflow(target - 2), Some(target));
        // One short: Linux's shortest period lands one past the target.
        assert_eq!(state.wanted_overflow(target - 1), Some(target + 1));
        // Imprecise requests are always programmed at the target.
        let state = armed(target, TimerKind::Imprecise);
        assert_eq!(state.wanted_overflow(0), Some(target));
        assert_eq!(state.wanted_overflow(target - 1), Some(target + 1));
        // Nothing is wanted without an armed request.
        assert_eq!(TimerState::Idle.wanted_overflow(0), None);
        let due = TimerState::Due(TimerDue {
            target,
            clock: target,
            late: true,
        });
        assert_eq!(due.wanted_overflow(target), None);
    }

    #[test]
    fn exits_decide_from_the_clock_and_record_a_late_precise_request_once() {
        // The witness count is process-global: take the turn every test that
        // reads it takes, so that a parallel `cargo test` cannot interleave.
        let _turn = witness_turn();
        let config = reverie::pmu_config::try_get_pmu_config();
        let mut state = armed(1_000, TimerKind::Precise);
        // Short of the target: nothing changes and nothing is recorded.
        assert_eq!(state.observe_exit(999), None);
        assert_eq!(state, armed(1_000, TimerKind::Precise));
        // Exactly at the target: late, because an exit is asynchronous.
        let due = TimerDue {
            target: 1_000,
            clock: 1_000,
            late: true,
        };
        assert_eq!(state.observe_exit(1_000), Some(due));
        assert_eq!(state.due(), Some(due));
        assert_eq!(state.target(), Some(1_000));
        // Later exits find it already due and record nothing more.
        assert_eq!(state.observe_exit(5_000), None);
        assert_eq!(state.due(), Some(due));
        // Past the target: late, with the overshoot the clock shows.
        let mut past = armed(1_000, TimerKind::Precise);
        assert_eq!(
            past.observe_exit(1_007),
            Some(TimerDue {
                target: 1_000,
                clock: 1_007,
                late: true,
            })
        );
        // An imprecise request reached at or after its target is on time.
        let mut imprecise = armed(1_000, TimerKind::Imprecise);
        assert_eq!(
            imprecise.observe_exit(1_000),
            Some(TimerDue {
                target: 1_000,
                clock: 1_000,
                late: false,
            })
        );
        let witnessed = reverie::take_skid_overshoot_count();
        assert_eq!(witnessed, 2, "one witness per late precise request");
        if let Some(config) = config {
            assert_eq!(
                config.format_skid_overshoot_marker(1_000, 1_000),
                format!(
                    "{} rcb_actual=1000 rcb_target=1000 skid_margin={} overshoot=0",
                    reverie::SKID_OVERSHOOT_MARKER,
                    config.skid_margin()
                )
            );
        }
    }

    #[test]
    fn programming_is_lazy() {
        // Nothing wanted and nothing pending: no syscall.
        assert_eq!(programming(5, None, None, false), Programming::Keep);
        // Nothing wanted, but an active finite period: quiet it once.
        assert_eq!(programming(5, None, Some(9), true), Programming::Quiet);
        assert_eq!(programming(5, None, None, true), Programming::Quiet);
        // A disabled event cannot overflow, so it is left alone.
        assert_eq!(programming(5, None, Some(9), false), Programming::Keep);
        // No pending overflow: program one.
        assert_eq!(
            programming(5, Some(105), None, true),
            Programming::Period(100)
        );
        // A pending overflow at or before the wanted point is kept, so an
        // unchanged target costs no syscall.
        assert_eq!(
            programming(5, Some(105), Some(105), true),
            Programming::Keep
        );
        assert_eq!(
            programming(5, Some(205), Some(105), true),
            Programming::Keep
        );
        // A pending overflow later than wanted, or already reached, is
        // reprogrammed from the entry's clock.
        assert_eq!(
            programming(5, Some(105), Some(106), true),
            Programming::Period(100)
        );
        assert_eq!(
            programming(105, Some(205), Some(105), true),
            Programming::Period(100)
        );
        // Never shorter than Linux's shortest period.
        assert_eq!(
            programming(5, Some(6), None, true),
            Programming::Period(MIN_PERIOD)
        );
        // Never longer than the longest period programmed at once: a farther
        // overflow is programmed in pieces. Linux refuses bit 63 outright.
        assert_eq!(
            programming(5, Some(5 + MAX_PERIOD), None, true),
            Programming::Period(MAX_PERIOD)
        );
        assert_eq!(
            programming(5, Some(5 + MAX_PERIOD + 1), None, true),
            Programming::Period(MAX_PERIOD)
        );
        assert_eq!(
            programming(0, Some(1 << 63), None, true),
            Programming::Period(MAX_PERIOD)
        );
        assert_eq!(
            programming(0, Some(u64::MAX), None, true),
            Programming::Period(MAX_PERIOD)
        );
        const { assert!(MAX_PERIOD & (1 << 63) == 0) };
        // A piece is kept until the clock reaches it, then the next follows.
        assert_eq!(
            programming(5, Some(u64::MAX), Some(5 + MAX_PERIOD), true),
            Programming::Keep
        );
        assert_eq!(
            programming(5 + MAX_PERIOD, Some(u64::MAX), Some(5 + MAX_PERIOD), true),
            Programming::Period(MAX_PERIOD)
        );
    }

    fn current_mask() -> libc::sigset_t {
        // SAFETY: initialized writable storage; this query changes no mask.
        unsafe {
            let mut mask = std::mem::zeroed();
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut mask),
                0
            );
            mask
        }
    }

    fn blocked(signal: libc::c_int) -> bool {
        // SAFETY: a valid initialized set.
        unsafe { libc::sigismember(&current_mask(), signal) == 1 }
    }

    fn block_kick_signal() {
        // SAFETY: valid set storage; affects only this thread.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, KICK_SIGNAL);
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()),
                0
            );
        }
    }

    extern "C" fn foreign_handler(_: libc::c_int) {}

    fn set_disposition(handler: usize) {
        // SAFETY: initialized action storage.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handler;
            libc::sigemptyset(&mut action.sa_mask);
            assert_eq!(
                libc::sigaction(KICK_SIGNAL, &action, std::ptr::null_mut()),
                0
            );
        }
    }

    #[test]
    fn kick_signal_reservation_is_idempotent_and_refuses_other_owners() {
        const CHILD: &str = "REVERIE_KVM_KICK_SIGNAL_CHILD";
        const TEST: &str = "preemption_timer::tests::kick_signal_reservation_is_idempotent_and_refuses_other_owners";
        let Ok(case) = std::env::var(CHILD) else {
            // Dispositions are process-wide: each case runs in a fresh
            // process so it cannot alter the parallel test runner.
            for case in ["repeated", "foreign", "ignored", "replaced"] {
                let output = std::process::Command::new("timeout")
                    .args(["--kill-after=2s", "20s"])
                    .arg(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                    .env(CHILD, case)
                    .output()
                    .unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    output.status.success(),
                    "{case}: {}{}",
                    stdout,
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    stdout.contains(&format!("kick signal case {case}: passed")),
                    "{case} must execute in the child: {stdout}"
                );
            }
            return;
        };
        match case.as_str() {
            "repeated" => {
                block_kick_signal();
                assert!(blocked(KICK_SIGNAL));
                reserve_kick_signal().unwrap();
                assert!(!blocked(KICK_SIGNAL), "reservation unblocks the caller");
                // Not compared with `kick as usize`: the linker may fold the
                // handler with an identical function (see reserve_kick_signal).
                let installed = query_kick_disposition().unwrap();
                assert_ne!(installed, libc::SIG_DFL);
                assert_ne!(installed, libc::SIG_IGN);
                block_kick_signal();
                reserve_kick_signal().unwrap();
                assert!(!blocked(KICK_SIGNAL));
                assert_eq!(query_kick_disposition().unwrap(), installed);
                // Another thread reserves too, and is unblocked itself.
                std::thread::spawn(move || {
                    block_kick_signal();
                    reserve_kick_signal().unwrap();
                    assert!(!blocked(KICK_SIGNAL));
                    assert_eq!(query_kick_disposition().unwrap(), installed);
                })
                .join()
                .unwrap();
                // The empty handler takes a kick without effect.
                let before =
                    KICKS_TAKEN.with(|kicks| kicks.load(std::sync::atomic::Ordering::Relaxed));
                // SAFETY: a thread-directed signal to this thread.
                assert_eq!(
                    unsafe { libc::pthread_kill(libc::pthread_self(), KICK_SIGNAL) },
                    0
                );
                let after =
                    KICKS_TAKEN.with(|kicks| kicks.load(std::sync::atomic::Ordering::Relaxed));
                assert_eq!(after, before + 1);
            }
            "foreign" | "ignored" => {
                let handler = if case == "ignored" {
                    libc::SIG_IGN
                } else {
                    foreign_handler as *const () as usize
                };
                set_disposition(handler);
                let error = reserve_kick_signal().unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains(&std::io::Error::from_raw_os_error(libc::EBUSY).to_string()),
                    "{error}"
                );
                assert_eq!(query_kick_disposition().unwrap(), handler);
                // The refusal is remembered: a second call refuses as well.
                assert!(reserve_kick_signal().is_err());
            }
            "replaced" => {
                reserve_kick_signal().unwrap();
                set_disposition(foreign_handler as *const () as usize);
                let error = reserve_kick_signal().unwrap_err();
                assert!(error.to_string().contains("disposition changed"), "{error}");
                assert_eq!(
                    query_kick_disposition().unwrap(),
                    foreign_handler as *const () as usize
                );
            }
            other => panic!("unknown case {other}"),
        }
        println!("kick signal case {case}: passed");
    }
}
