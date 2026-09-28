/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A syscall that a tool injects through Reverie's private page must report
//! the kernel's result exactly once, even when a job-control group stop lands
//! on the injecting thread around the single step.
//!
//! Under `PTRACE_TRACEME` a group stop is reported with the stop signal and no
//! event, so it looks like a signal-delivery stop. When another thread
//! initiates a group stop while the injected `syscall` executes, Linux checks
//! `JOBCTL_STOP_PENDING` in `get_signal` before it dequeues the step SIGTRAP
//! queued by `syscall_exit_work`, so the tracer sees the stop signal with RIP
//! already past the private `syscall` instruction and the kernel's result in
//! RAX. The injection must neither overwrite that completed result with
//! `-ERESTARTSYS` (re-executing the syscall through a restart) nor leave the
//! guest a raw restart code.

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Signal;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::Errno;
use reverie::syscalls::Getpid;
use reverie::syscalls::Ppoll;
use reverie::syscalls::RtSigprocmask;
use reverie::syscalls::RtTgsigqueueinfo;
use reverie::syscalls::Syscall;
use reverie::syscalls::Sysno;
use reverie_ptrace::testing::test_fn;
use serde::Deserialize;
use serde::Serialize;

/// The guest dups its pipe's write end here; a zero-length write to this
/// descriptor is the marker the tool replaces with a one-byte write.
const PROBE_FD: i32 = 900;
/// A zero-length write to this descriptor is replaced with
/// `rt_tgsigqueueinfo(self, self, SIGSYS, buf)`, where `buf` is the marker's
/// buffer holding a guest-prepared siginfo.
const SIGQUEUE_FD: i32 = 901;
/// A zero-length write to this descriptor is replaced with
/// `rt_sigprocmask(SIG_UNBLOCK, buf, NULL)`.
const UNBLOCK_FD: i32 = 902;
/// Like `UNBLOCK_FD`, followed by an injected `getpid` whose result the tool
/// returns to the guest.
const UNBLOCK_THEN_GETPID_FD: i32 = 903;
/// A zero-length write to this descriptor is replaced with
/// `ppoll(NULL, 0, &buf.timeout, &buf.mask, 8)` for a `PpollArgs` at `buf`.
const PPOLL_FD: i32 = 904;

/// Guest memory for the injected `ppoll`.
#[repr(C)]
struct PpollArgs {
    mask: libc::sigset_t,
    timeout: libc::timespec,
}
const MARKERS: usize = 1500;

#[derive(Debug, Deserialize, Serialize)]
enum Report {
    Injected(Result<i64, i32>),
    Signal(i32),
}

#[derive(Default)]
struct Log {
    injected: Mutex<Vec<Result<i64, i32>>>,
    signals: Mutex<Vec<i32>>,
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Request = Report;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, report: Report) {
        match report {
            Report::Injected(result) => self.injected.lock().unwrap().push(result),
            Report::Signal(signal) => self.signals.lock().unwrap().push(signal),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ReplaceMarker;

#[reverie::tool]
impl Tool for ReplaceMarker {
    type GlobalState = Log;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        let mut subscription = Subscription::none();
        subscription.syscall(Sysno::write);
        subscription
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall {
            Syscall::Write(write) if write.fd() == PROBE_FD && write.len() == 0 => {
                // A different syscall from the pending one: this takes the
                // private_inject -> untraced_syscall path.
                let result = guest.inject(write.with_len(1)).await;
                guest
                    .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                    .await;
                Ok(result?)
            }
            Syscall::Write(write) if write.fd() == SIGQUEUE_FD && write.len() == 0 => {
                let siginfo = write.buf().and_then(|buf| AddrMut::from_raw(buf.as_raw()));
                let result = guest
                    .inject(
                        RtTgsigqueueinfo::new()
                            .with_tgid(guest.pid().as_raw())
                            .with_tid(guest.tid().as_raw())
                            .with_sig(libc::SIGSYS)
                            .with_siginfo(siginfo),
                    )
                    .await;
                guest
                    .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                    .await;
                Ok(result?)
            }
            Syscall::Write(write)
                if (write.fd() == UNBLOCK_FD || write.fd() == UNBLOCK_THEN_GETPID_FD)
                    && write.len() == 0 =>
            {
                let set = write.buf().and_then(|buf| Addr::from_raw(buf.as_raw()));
                let result = guest
                    .inject(
                        RtSigprocmask::new()
                            .with_how(libc::SIG_UNBLOCK)
                            .with_set(set)
                            .with_oldset(None)
                            .with_sigsetsize(8),
                    )
                    .await;
                guest
                    .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                    .await;
                if write.fd() == UNBLOCK_FD {
                    return Ok(result?);
                }
                let result = guest.inject(Getpid::new()).await;
                guest
                    .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                    .await;
                Ok(result?)
            }
            Syscall::Write(write) if write.fd() == PPOLL_FD && write.len() == 0 => {
                let base = write.buf().map_or(0, |buf| buf.as_raw());
                let result = guest
                    .inject(
                        Ppoll::new()
                            .with_fds(None)
                            .with_nfds(0)
                            .with_timeout(AddrMut::from_raw(
                                base + std::mem::offset_of!(PpollArgs, timeout),
                            ))
                            .with_sigmask(Addr::from_raw(base))
                            .with_sigsetsize(8),
                    )
                    .await;
                guest
                    .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                    .await;
                Ok(result?)
            }
            other => Ok(guest.inject(other).await?),
        }
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        guest.send_rpc(Report::Signal(signal as i32)).await;
        Ok(Some(signal))
    }
}

/// Guest outcome, printed on stdout as one whitespace-separated line.
#[derive(Debug, Default)]
struct Outcome {
    markers: usize,
    returned_one: usize,
    other_returns: Vec<i64>,
    bytes: usize,
}

fn drain(read_fd: i32) -> usize {
    let mut total = 0;
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: buf is a valid writable buffer of the given length.
        let n = unsafe { libc::read(read_fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return total;
        }
        total += n as usize;
    }
}

fn guest() {
    static STOP: AtomicBool = AtomicBool::new(false);

    // SAFETY: plain libc calls on owned descriptors.
    let read_fd = unsafe {
        // A job-control stop signal is discarded in an orphaned process group.
        // Leading a new group whose parent (the tracer) is in another group of
        // the same session keeps SIGTSTP's default action a real stop.
        assert_eq!(libc::setpgid(0, 0), 0);
        let mut fds = [0; 2];
        assert_eq!(libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK), 0);
        assert_eq!(libc::dup2(fds[1], PROBE_FD), PROBE_FD);
        libc::close(fds[1]);
        fds[0]
    };

    let stopper = std::thread::spawn(|| {
        while !STOP.load(Ordering::Relaxed) {
            // SAFETY: raise has no memory-safety preconditions. SIGTSTP keeps
            // its default (stop) disposition, so delivering it initiates a
            // group stop that reaches the marker thread.
            unsafe { libc::raise(libc::SIGTSTP) };
        }
    });

    let mut outcome = Outcome {
        markers: MARKERS,
        ..Default::default()
    };
    let byte = 0x5au8;
    for _ in 0..MARKERS {
        // SAFETY: a zero-length write from a valid buffer.
        let ret = unsafe { libc::syscall(libc::SYS_write, PROBE_FD, &byte as *const u8, 0usize) };
        if ret == 1 {
            outcome.returned_one += 1;
        } else {
            outcome.other_returns.push(ret);
        }
        outcome.bytes += drain(read_fd);
    }
    STOP.store(true, Ordering::Relaxed);
    stopper.join().unwrap();
    outcome.bytes += drain(read_fd);
    let others: Vec<String> = outcome.other_returns.iter().map(i64::to_string).collect();
    println!(
        "{} {} {} {}",
        outcome.markers,
        outcome.returned_one,
        outcome.bytes,
        others.join(",")
    );
}

fn parse_outcome(line: &str) -> Outcome {
    let mut fields = line.split(' ');
    let mut number = || {
        fields
            .next()
            .expect("outcome field")
            .parse()
            .expect("count")
    };
    let markers = number();
    let returned_one = number();
    let bytes = number();
    let other_returns = line
        .splitn(4, ' ')
        .nth(3)
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().expect("return value"))
        .collect();
    Outcome {
        markers,
        returned_one,
        other_returns,
        bytes,
    }
}

#[test]
fn injected_syscall_completed_before_group_stop_reports_its_result_once() {
    let (output, log) = test_fn::<ReplaceMarker, _>(guest).expect("run group-stop guest");
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let outcome = parse_outcome(stdout.trim());
    let injected = log.injected.lock().unwrap();
    let signals = log.signals.lock().unwrap();
    let tstp = signals.iter().filter(|&&s| s == libc::SIGTSTP).count();
    let restarted = injected
        .iter()
        .filter(|result| matches!(result, Err(errno) if *errno == Errno::ERESTARTSYS.into_raw()))
        .count();
    eprintln!(
        "PROBE markers={} returned_one={} bytes={} injections={} restarted={} sigtstp_reports={} other_returns={:?}",
        outcome.markers,
        outcome.returned_one,
        outcome.bytes,
        injected.len(),
        restarted,
        tstp,
        outcome.other_returns,
    );
    assert!(tstp > 0, "the stopper thread never raised SIGTSTP");
    // Only the stopper thread receives SIGTSTP itself; the marker thread sees
    // nothing but group stops, which a restarted ptraced tracee ignores. No
    // injection may therefore be reported as interrupted or be repeated.
    assert_eq!(restarted, 0, "a group stop is not a signal delivery");
    assert_eq!(injected.len(), MARKERS, "each marker is injected once");
    assert_eq!(outcome.returned_one, MARKERS, "{outcome:?}");
    assert_eq!(
        outcome.bytes, MARKERS,
        "each marker must write exactly once"
    );
}

static SIGSYS_HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_sigsys(_signal: libc::c_int) {
    SIGSYS_HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
}

/// A synchronous-class signal (positive `si_code`) that the injected syscall
/// queues to its own thread is dequeued ahead of the step SIGTRAP, so the
/// tracer sees a genuine signal-delivery stop with RIP past the private
/// `syscall` and the completed result in RAX. The kernel under plain
/// execution returns that result and then runs the handler once; it must not
/// turn the completed syscall into `-ERESTARTSYS` (which, without
/// `SA_RESTART`, surfaces as `EINTR`) or run it twice.
#[test]
fn injected_syscall_completed_before_signal_delivery_keeps_its_result() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        SIGSYS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = count_sigsys as *const () as usize;
        // No SA_RESTART: a restarted-by-signal syscall would report EINTR.
        action.sa_flags = 0;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGSYS, &action, std::ptr::null_mut()),
            0
        );
        let mut info: libc::siginfo_t = std::mem::zeroed();
        info.si_signo = libc::SIGSYS;
        // A positive si_code classifies the queued signal as synchronous,
        // which Linux dequeues ahead of other pending signals.
        info.si_code = 1;
        let ret = libc::syscall(
            libc::SYS_write,
            SIGQUEUE_FD,
            &mut info as *mut libc::siginfo_t,
            0usize,
        );
        let calls = SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed);
        println!("{ret} {calls}");
    })
    .expect("run sigqueue guest");
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let injected = log.injected.lock().unwrap();
    let signals = log.signals.lock().unwrap();
    eprintln!("PROBE sigqueue signals={:?}", *signals);
    eprintln!(
        "PROBE sigqueue guest={} injected={:?}",
        stdout.trim(),
        *injected
    );
    assert_eq!(
        *injected,
        vec![Ok(0)],
        "the injected syscall runs once and succeeds"
    );
    assert_eq!(
        stdout.trim(),
        "0 1",
        "guest sees success and one handler run"
    );
    assert_eq!(
        *signals,
        vec![libc::SIGSYS],
        "the signal reaches the tool like any other signal delivery"
    );
}

static SIGSEGV_HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_sigsegv(_signal: libc::c_int) {
    SIGSEGV_HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
}

static SIGUSR1_HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_sigusr1(_signal: libc::c_int) {
    SIGUSR1_HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
}

/// Installs `handler` for `signal` without `SA_RESTART`.
///
/// # Safety
/// Replaces the process-wide disposition of `signal`.
unsafe fn install_counter(signal: libc::c_int, handler: extern "C" fn(libc::c_int)) {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as *const () as usize;
        action.sa_flags = 0;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(libc::sigaction(signal, &action, std::ptr::null_mut()), 0);
    }
}

/// Blocks `signals` and returns the set.
///
/// # Safety
/// Changes the calling thread's signal mask.
unsafe fn block(signals: &[libc::c_int]) -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for &signal in signals {
            libc::sigaddset(&mut set, signal);
        }
        assert_eq!(
            libc::syscall(
                libc::SYS_rt_sigprocmask,
                libc::SIG_BLOCK,
                &set as *const libc::sigset_t,
                0usize,
                8usize
            ),
            0
        );
        set
    }
}

/// Queues `signal` to the calling thread with `si_code`.
///
/// # Safety
/// Sends a signal to the calling thread.
unsafe fn queue_to_self(signal: libc::c_int, si_code: libc::c_int) {
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        info.si_signo = signal;
        info.si_code = si_code;
        assert_eq!(
            libc::syscall(
                libc::SYS_rt_tgsigqueueinfo,
                libc::getpid(),
                libc::syscall(libc::SYS_gettid),
                signal,
                &mut info as *mut libc::siginfo_t,
            ),
            0
        );
    }
}

/// Two synchronous-class signals (positive `si_code`) queued while blocked
/// and unblocked by one injected `rt_sigprocmask` are both dequeued ahead of
/// the step SIGTRAP, one stop each, with RIP past the private `syscall`.
/// Linux under plain execution returns 0 and runs both handlers once (checked
/// with the same guest body run untraced: "0 1 1"). Holding them in the
/// single `pending_signal` slot delivered only the second one ("0 0 1").
#[test]
fn injected_syscall_completed_before_two_signal_deliveries_delivers_both() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        SIGSYS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        SIGSEGV_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGSYS, count_sigsys);
        install_counter(libc::SIGSEGV, count_sigsegv);
        let set = block(&[libc::SIGSYS, libc::SIGSEGV]);
        queue_to_self(libc::SIGSYS, 1);
        queue_to_self(libc::SIGSEGV, 1);
        let ret = libc::syscall(
            libc::SYS_write,
            UNBLOCK_FD,
            &set as *const libc::sigset_t,
            0usize,
        );
        println!(
            "{ret} {} {}",
            SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed),
            SIGSEGV_HANDLER_CALLS.load(Ordering::Relaxed)
        );
    })
    .expect("run two-signal guest");
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let injected = log.injected.lock().unwrap();
    let signals = log.signals.lock().unwrap();
    eprintln!(
        "PROBE two-signal guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    assert_eq!(*injected, vec![Ok(0)], "the unblock runs once and succeeds");
    assert_eq!(
        stdout.trim(),
        "0 1 1",
        "guest sees success and each handler runs once"
    );
    assert_eq!(
        *signals,
        vec![libc::SIGSYS, libc::SIGSEGV],
        "both signals reach the tool, in queue order"
    );
}

/// A signal that becomes deliverable before an injected `syscall` executes
/// interrupts it: Reverie reports `ERESTARTSYS` and delivers the signal at
/// the next resume, where the kernel turns the restart into `EINTR` because
/// the handler lacks `SA_RESTART`. SIGUSR1 queued by `tgkill` is not
/// synchronous-class, so the unblock's own step SIGTRAP is dequeued first and
/// the signal stops the following `getpid` before its `syscall`.
#[test]
fn signal_pending_before_injected_syscall_interrupts_it() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        SIGUSR1_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGUSR1, count_sigusr1);
        let set = block(&[libc::SIGUSR1]);
        assert_eq!(
            libc::syscall(
                libc::SYS_tgkill,
                libc::getpid(),
                libc::syscall(libc::SYS_gettid),
                libc::SIGUSR1
            ),
            0
        );
        let ret = libc::syscall(
            libc::SYS_write,
            UNBLOCK_THEN_GETPID_FD,
            &set as *const libc::sigset_t,
            0usize,
        );
        let errno = *libc::__errno_location();
        println!(
            "{ret} {errno} {}",
            SIGUSR1_HANDLER_CALLS.load(Ordering::Relaxed)
        );
    })
    .expect("run interrupted-injection guest");
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let injected = log.injected.lock().unwrap();
    eprintln!(
        "PROBE interrupted guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *log.signals.lock().unwrap()
    );
    assert_eq!(
        *injected,
        vec![Ok(0), Err(Errno::ERESTARTSYS.into_raw())],
        "the unblock completes; the signal interrupts getpid before it runs"
    );
    assert_eq!(
        stdout.trim(),
        format!("-1 {} 1", libc::EINTR),
        "guest sees EINTR and one handler run"
    );
}

/// `ppoll` swaps in a temporary signal mask and leaves the kernel to restore
/// the saved one after signal handling. When the temporary mask unblocks a
/// pending synchronous-class signal, `ppoll` returns `-ERESTARTNOHAND` and
/// the signal is dequeued ahead of the step SIGTRAP. Returning it to the
/// kernel queue by masking it through ptrace would discard the saved mask, so
/// SIGSYS would stay unblocked after the call. Untraced Linux prints
/// "-1 4 1 1": EINTR, one handler run, SIGSYS blocked again.
#[test]
fn injected_mask_swapping_syscall_keeps_the_saved_mask() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        SIGSYS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGSYS, count_sigsys);
        block(&[libc::SIGSYS]);
        queue_to_self(libc::SIGSYS, 1);
        let mut args: PpollArgs = std::mem::zeroed();
        libc::sigemptyset(&mut args.mask);
        args.timeout.tv_sec = 5;
        let ret = libc::syscall(
            libc::SYS_write,
            PPOLL_FD,
            &mut args as *mut PpollArgs,
            0usize,
        );
        let errno = *libc::__errno_location();
        let mut current: libc::sigset_t = std::mem::zeroed();
        assert_eq!(
            libc::syscall(
                libc::SYS_rt_sigprocmask,
                libc::SIG_BLOCK,
                0usize,
                &mut current as *mut libc::sigset_t,
                8usize
            ),
            0
        );
        println!(
            "{ret} {errno} {} {}",
            SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed),
            libc::sigismember(&current, libc::SIGSYS)
        );
    })
    .expect("run ppoll guest");
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let injected = log.injected.lock().unwrap();
    eprintln!(
        "PROBE ppoll guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *log.signals.lock().unwrap()
    );
    assert_eq!(
        *injected,
        vec![Err(Errno::ERESTARTNOHAND.into_raw())],
        "ppoll is interrupted by the signal its mask unblocks"
    );
    assert_eq!(
        stdout.trim(),
        format!("-1 {} 1 1", libc::EINTR),
        "guest sees EINTR, one handler run, and its saved mask restored"
    );
}
