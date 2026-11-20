/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A guest thread that queues itself a SIGTRAP while a precise timer's steps
//! run it, through a call that lets it choose the code (`rt_tgsigqueueinfo`,
//! or `pidfd_send_signal` with `PIDFD_SIGNAL_THREAD`), does not lose it to
//! the step's own trap. The Tool observes sigreturns, serializes threads and
//! traces every syscall but those two, so the steps read nothing per step
//! but what the architecture needs, and neither call stops at a seccomp stop.
//! A SIGTRAP coded as a sender's reaches the guest's handler; one with a
//! trap's code ends the run with `QueuedSigtrapAtStep`, since nothing tells
//! it from the step's trap.

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use reverie::Errno;
use reverie::Error;
use reverie::Guest;
use reverie::Subscription;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::perf::Builder;
use reverie::perf::Event;
use reverie::perf::HardwareEvent;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_ptrace::QueuedSigtrapAtStep;
use reverie_ptrace::testing::do_branches;
use reverie_ptrace::testing::test_fn;
use test_case::test_case;

/// The first argument of the guest's `getppid` at whose stop the Tool
/// requests the precise timer.
const MARKER: usize = 0x5349_4754;

/// The timer's target, in branches after the marker returns: within the
/// skid margin, so that the timer steps from the marker's return with no
/// counter overflow first, and more than the guest retires before the call
/// that queues the SIGTRAP.
const TARGET_RCBS: u64 = 15;

/// `PIDFD_SIGNAL_THREAD` (include/uapi/linux/pidfd.h): the signal goes to
/// the thread's own queue.
const PIDFD_SIGNAL_THREAD: libc::c_long = 1;

/// `SI_QUEUE` (include/uapi/asm-generic/siginfo.h), `sigqueue`'s code.
const SI_QUEUE: i32 = -1;

#[derive(Debug, Default, Clone)]
struct SteppedSend;

#[reverie::tool]
impl Tool for SteppedSend {
    type GlobalState = ();
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        let mut subscription = Subscription::all_syscalls();
        subscription.disable_syscalls([Sysno::rt_tgsigqueueinfo, Sysno::pidfd_send_signal]);
        subscription
    }

    fn observes_sigreturns(_config: &()) -> bool {
        true
    }

    /// The guest has one thread.
    fn enforces_serialized_execution(_config: &()) -> bool {
        true
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let (nr, args) = syscall.into_parts();
        if nr == Sysno::getppid && args.arg0 == MARKER {
            guest.set_timer_precise(TimerSchedule::Rcbs(TARGET_RCBS))?;
        }
        guest.tail_inject(syscall).await
    }
}

/// How the guest queues itself the SIGTRAP.
#[derive(Clone, Copy, Debug)]
enum Send {
    /// `rt_tgsigqueueinfo` with `SI_QUEUE`, a sender's code.
    TgsigqueueinfoSender,
    /// `rt_tgsigqueueinfo` with `TRAP_BRKPT`, a trap's code.
    TgsigqueueinfoTrap,
    /// `pidfd_send_signal` with no siginfo, which Linux codes `SI_USER` with
    /// the sender's pid.
    PidfdSender,
    /// `pidfd_send_signal` with `TRAP_BRKPT`.
    PidfdTrap,
}

impl Send {
    fn trap_coded(self) -> bool {
        matches!(self, Send::TgsigqueueinfoTrap | Send::PidfdTrap)
    }
}

/// Runs of the guest's SIGTRAP handler.
static TRAPS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_sigtrap(_: libc::c_int) {
    TRAPS.fetch_add(1, Ordering::SeqCst);
}

/// The guest: SIGTRAP has a handler and is unblocked; the marker makes the
/// Tool request the timer, and the next call queues the SIGTRAP, with only
/// the libc wrapper's return between the two. The guest checks that the
/// call succeeded and its handler ran once.
fn guest(send: Send) {
    // SAFETY: the action is fully initialized, the handler increments an
    // atomic, and every call takes integers or pointers to live locals.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_sigtrap as *const () as usize;
        assert_eq!(
            libc::sigaction(libc::SIGTRAP, &action, std::ptr::null_mut()),
            0
        );
        let pid = libc::getpid();
        // The process's pidfd names the thread-group leader, which a
        // thread-directed signal reaches.
        assert_eq!(libc::syscall(libc::SYS_gettid), libc::c_long::from(pid));
        let pidfd = libc::syscall(libc::SYS_pidfd_open, pid, 0);
        assert!(pidfd >= 0, "pidfd_open");
        let mut info: libc::siginfo_t = std::mem::zeroed();
        info.si_signo = libc::SIGTRAP;
        info.si_code = if send.trap_coded() {
            libc::TRAP_BRKPT
        } else {
            SI_QUEUE
        };
        let info = &raw const info as libc::c_long;
        let sigtrap = libc::c_long::from(libc::SIGTRAP);
        let (nr, args) = match send {
            Send::TgsigqueueinfoSender | Send::TgsigqueueinfoTrap => (
                libc::SYS_rt_tgsigqueueinfo,
                [libc::c_long::from(pid), libc::c_long::from(pid), sigtrap, info],
            ),
            Send::PidfdSender => (
                libc::SYS_pidfd_send_signal,
                [pidfd, sigtrap, 0, PIDFD_SIGNAL_THREAD],
            ),
            Send::PidfdTrap => (
                libc::SYS_pidfd_send_signal,
                [pidfd, sigtrap, info, PIDFD_SIGNAL_THREAD],
            ),
        };
        libc::syscall(libc::SYS_getppid, MARKER);
        let sent = libc::syscall(nr, args[0], args[1], args[2], args[3]);
        do_branches(10 * TARGET_RCBS);
        assert_eq!(sent, 0, "{send:?}");
        assert_eq!(TRAPS.load(Ordering::SeqCst), 1, "{send:?}: handler runs");
    }
}

/// A host capability a case needs, as measured here: present, or measured
/// absent, with what showed it. A measurement that fails any other way
/// panics, so the case fails rather than being skipped.
enum Capability {
    Present,
    Absent(String),
}

/// Whether this host has the hardware performance counter the precise
/// timer needs: the same user-only hardware-instructions counter Reverie's
/// `is_perf_supported` opens. The errnos perf_event_open(2) returns where
/// no such counter can be had are absence; any other errno fails the case.
fn hardware_counter() -> Capability {
    match Builder::new(0, -1)
        .event(Event::Hardware(HardwareEvent::Instructions))
        .create()
    {
        Ok(_) => Capability::Present,
        Err(
            errno @ (Errno::ENOENT
            | Errno::ENODEV
            | Errno::EOPNOTSUPP
            | Errno::EACCES
            | Errno::EPERM
            | Errno::ENOSYS),
        ) => Capability::Absent(format!(
            "no hardware performance counter: perf_event_open of the instructions counter failed {errno}"
        )),
        Err(errno) => panic!("probe the hardware performance counter: {errno}"),
    }
}

/// Whether this kernel takes `PIDFD_SIGNAL_THREAD` (Linux 6.9), probed with
/// signal 0, which sends nothing. `ENOSYS` (no pidfd calls) and `EINVAL`
/// (a kernel that does not know the flag) are absence; any other failure
/// fails the case.
fn thread_directed_pidfd_signals() -> Capability {
    // SAFETY: pidfd_open, pidfd_send_signal and close take integers.
    unsafe {
        let pidfd = libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0);
        if pidfd < 0 {
            let errno = last_errno();
            assert_eq!(errno, Errno::ENOSYS, "pidfd_open");
            return Capability::Absent(format!("no pidfd_open: {errno}"));
        }
        let probed = libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd,
            0,
            0,
            PIDFD_SIGNAL_THREAD,
        );
        let errno = last_errno();
        assert_eq!(libc::close(pidfd as libc::c_int), 0, "close the pidfd");
        match probed {
            0 => Capability::Present,
            _ if errno == Errno::ENOSYS || errno == Errno::EINVAL => Capability::Absent(format!(
                "this kernel has no PIDFD_SIGNAL_THREAD: pidfd_send_signal failed {errno}"
            )),
            _ => panic!("probe pidfd_send_signal with PIDFD_SIGNAL_THREAD: {errno}"),
        }
    }
}

/// The calling thread's errno.
fn last_errno() -> Errno {
    Errno::new(
        std::io::Error::last_os_error()
            .raw_os_error()
            .expect("a failed call sets errno"),
    )
}

/// Names this case's skip for a measured absence past libtest's output
/// capture (fd 2 directly), as Reverie's other host-capability skips do, so
/// the counted runner and the gated runner report it as skipped, not passed.
/// The case is libtest's name for this thread.
fn report_skip(reason: &str) {
    let current = std::thread::current();
    let case = current
        .name()
        .filter(|name| *name != "main")
        .expect("libtest names a case's thread after the case");
    let line = format!("\nREVERIE_SKIP: {case}: {reason}\n");
    let mut rest = line.as_bytes();
    while !rest.is_empty() {
        // SAFETY: writes only the buffer above to this process's stderr.
        let written =
            unsafe { libc::write(libc::STDERR_FILENO, rest.as_ptr().cast(), rest.len()) };
        assert!(
            written > 0,
            "write the named skip for {case}: {}",
            std::io::Error::last_os_error()
        );
        rest = &rest[written as usize..];
    }
}

/// A SIGTRAP the guest queues itself with a sender's code during the timer's
/// steps reaches its handler once; one with a trap's code ends the run with
/// `QueuedSigtrapAtStep`. Neither is taken for the step's own trap. A host
/// without the hardware counter, or (for the pidfd cases) without
/// `PIDFD_SIGNAL_THREAD`, names the skip (`REVERIE_SKIP`).
#[test_case(Send::TgsigqueueinfoSender)]
#[test_case(Send::TgsigqueueinfoTrap)]
#[test_case(Send::PidfdSender)]
#[test_case(Send::PidfdTrap)]
fn a_sigtrap_queued_during_timer_steps_is_delivered_or_refused(send: Send) {
    if let Capability::Absent(reason) = hardware_counter() {
        report_skip(&reason);
        return;
    }
    if matches!(send, Send::PidfdSender | Send::PidfdTrap)
        && let Capability::Absent(reason) = thread_directed_pidfd_signals()
    {
        report_skip(&reason);
        return;
    }
    let result = test_fn::<SteppedSend, _>(move || guest(send));
    if send.trap_coded() {
        let error = match result {
            Ok((output, ())) => panic!("{send:?}: the run went on: {output:?}"),
            Err(error) => error,
        };
        let refusal = match &error {
            Error::Tool(inner) => inner.downcast_ref::<QueuedSigtrapAtStep>(),
            _ => None,
        };
        let refusal =
            refusal.unwrap_or_else(|| panic!("{send:?}: expected QueuedSigtrapAtStep: {error:?}"));
        assert_eq!(refusal.site, "precise timer", "{refusal}");
    } else {
        let (output, ()) = result.unwrap_or_else(|error| panic!("{send:?}: {error:?}"));
        assert!(output.status.success(), "{send:?}: {output:?}");
    }
}
