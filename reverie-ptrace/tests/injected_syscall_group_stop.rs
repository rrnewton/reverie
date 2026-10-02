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
use reverie::syscalls::Fork;
use reverie::syscalls::Getpid;
use reverie::syscalls::Getppid;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Ppoll;
use reverie::syscalls::RtSigprocmask;
use reverie::syscalls::RtTgsigqueueinfo;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use reverie::syscalls::Tgkill;
#[cfg(target_arch = "x86_64")]
use reverie::syscalls::Vfork;
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
/// Like `PPOLL_FD`, followed by an injected `getpid` whose result the tool
/// returns to the guest.
const PPOLL_THEN_GETPID_FD: i32 = 905;
/// A zero-length write to this descriptor is replaced with `getppid`.
const GETPPID_FD: i32 = 906;
/// Like `PPOLL_FD`, but the tool returns 0 to the guest instead of the
/// `ppoll` result, so the guest's syscall is not restarted.
const PPOLL_THEN_ZERO_FD: i32 = 907;
/// For an `UnblockThenTrapArgs` at `buf`: `rt_sigprocmask(SIG_UNBLOCK,
/// &buf.set, NULL)`, then `getpid`, then `rt_tgsigqueueinfo(self, self,
/// SIGTRAP, &buf.info)`, each reported; the tool returns the last result.
const UNBLOCK_GETPID_THEN_TRAP_FD: i32 = 908;
/// `rt_sigprocmask(SIG_UNBLOCK, buf, NULL)`, then `getpid`, then `fork`, each
/// reported; the tool returns 0 to the guest.
const UNBLOCK_GETPID_FORK_THEN_ZERO_FD: i32 = 909;
/// `ExecInSignalHook` keeps `buf`, an `ExecArgs`, for its signal hook.
const EXEC_ARGS_FD: i32 = 910;
/// `tgkill(self, self, SIGSTOP)`, then `getpid`, each reported; the tool
/// returns the `getpid` result. The `tgkill` is skipped, and the `i32` at
/// `buf` left alone, once that `i32` is nonzero; the tool sets it to 1 when
/// it injects the `tgkill`, so a restarted marker sends no second SIGSTOP.
const SIGSTOP_THEN_GETPID_FD: i32 = 911;

/// Guest memory for the injected `ppoll`.
#[repr(C)]
struct PpollArgs {
    mask: libc::sigset_t,
    timeout: libc::timespec,
}
/// Guest memory for `UNBLOCK_GETPID_THEN_TRAP_FD`.
#[repr(C)]
struct UnblockThenTrapArgs {
    set: libc::sigset_t,
    info: libc::siginfo_t,
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
        replace_marker(guest, syscall).await
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

/// The marker replacements shared by `ReplaceMarker` and
/// `InjectInFirstSignalHook`.
async fn replace_marker<T, G>(guest: &mut G, syscall: Syscall) -> Result<i64, Error>
where
    T: Tool<GlobalState = Log>,
    G: Guest<T>,
{
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
        Syscall::Write(write)
            if (write.fd() == PPOLL_FD
                || write.fd() == PPOLL_THEN_GETPID_FD
                || write.fd() == PPOLL_THEN_ZERO_FD)
                && write.len() == 0 =>
        {
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
            if write.fd() == PPOLL_FD {
                return Ok(result?);
            }
            if write.fd() == PPOLL_THEN_ZERO_FD {
                return Ok(0);
            }
            let result = guest.inject(Getpid::new()).await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
            Ok(result?)
        }
        Syscall::Write(write) if write.fd() == SIGSTOP_THEN_GETPID_FD && write.len() == 0 => {
            let sent = write
                .buf()
                .and_then(|buf| AddrMut::<i32>::from_raw(buf.as_raw()))
                .ok_or(Errno::EFAULT)?;
            if guest.memory().read_value(sent)? == 0 {
                guest.memory().write_value(sent, &1)?;
                let result = guest
                    .inject(
                        Tgkill::new()
                            .with_tgid(guest.pid().as_raw())
                            .with_tid(guest.tid().as_raw())
                            .with_sig(libc::SIGSTOP),
                    )
                    .await;
                guest
                    .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                    .await;
            }
            let result = guest.inject(Getpid::new()).await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
            Ok(result?)
        }
        Syscall::Write(write) if write.fd() == GETPPID_FD && write.len() == 0 => {
            let result = guest.inject(Getppid::new()).await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
            Ok(result?)
        }
        Syscall::Write(write)
            if write.fd() == UNBLOCK_GETPID_FORK_THEN_ZERO_FD && write.len() == 0 =>
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
            let result = guest.inject(Getpid::new()).await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
            let result = guest.inject(Fork::new()).await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
            Ok(0)
        }
        Syscall::Write(write) if write.fd() == UNBLOCK_GETPID_THEN_TRAP_FD && write.len() == 0 => {
            let base = write.buf().map_or(0, |buf| buf.as_raw());
            let result = guest
                .inject(
                    RtSigprocmask::new()
                        .with_how(libc::SIG_UNBLOCK)
                        .with_set(Addr::from_raw(base))
                        .with_oldset(None)
                        .with_sigsetsize(8),
                )
                .await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
            let result = guest.inject(Getpid::new()).await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
            let result = guest
                .inject(
                    RtTgsigqueueinfo::new()
                        .with_tgid(guest.pid().as_raw())
                        .with_tid(guest.tid().as_raw())
                        .with_sig(libc::SIGTRAP)
                        .with_siginfo(AddrMut::from_raw(
                            base + std::mem::offset_of!(UnblockThenTrapArgs, info),
                        )),
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

/// Like `ReplaceMarker`, but the first signal hook on each thread injects a
/// `getpid`, reported, before passing the signal through.
#[derive(Clone, Copy, Debug, Default)]
struct InjectInFirstSignalHook;

#[reverie::tool]
impl Tool for InjectInFirstSignalHook {
    type GlobalState = Log;
    /// Signal hooks run on this thread so far.
    type ThreadState = u64;

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
        replace_marker(guest, syscall).await
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        guest.send_rpc(Report::Signal(signal as i32)).await;
        *guest.thread_state_mut() += 1;
        if *guest.thread_state_mut() == 1 {
            let result = guest.inject(Getpid::new()).await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
        }
        Ok(Some(signal))
    }
}

/// Like `InjectInFirstSignalHook`, but the first signal hook injects `fork`.
#[derive(Clone, Copy, Debug, Default)]
struct ForkInFirstSignalHook;

#[reverie::tool]
impl Tool for ForkInFirstSignalHook {
    type GlobalState = Log;
    /// Signal hooks run on this thread so far.
    type ThreadState = u64;

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
        replace_marker(guest, syscall).await
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        guest.send_rpc(Report::Signal(signal as i32)).await;
        *guest.thread_state_mut() += 1;
        if *guest.thread_state_mut() == 1 {
            let result = guest.inject(Fork::new()).await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
        }
        Ok(Some(signal))
    }
}

/// Guest memory naming the program `ExecInSignalHook` executes.
#[repr(C)]
struct ExecArgs {
    path: *const libc::c_char,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
}

/// Keeps the `ExecArgs` a zero-length write to `EXEC_ARGS_FD` names, and
/// executes them from the signal hook of the next SIGUSR2. In any program,
/// `geteuid` is replaced with `getpid`.
#[derive(Clone, Copy, Debug, Default)]
struct ExecInSignalHook;

#[reverie::tool]
impl Tool for ExecInSignalHook {
    type GlobalState = Log;
    /// The address of the kept `ExecArgs`.
    type ThreadState = usize;

    fn subscriptions(_config: &()) -> Subscription {
        let mut subscription = Subscription::none();
        subscription.syscall(Sysno::write);
        subscription.syscall(Sysno::geteuid);
        subscription
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall {
            Syscall::Write(write) if write.fd() == EXEC_ARGS_FD && write.len() == 0 => {
                *guest.thread_state_mut() = write.buf().map_or(0, |buf| buf.as_raw());
                Ok(0)
            }
            Syscall::Geteuid(_) => guest.tail_inject(Getpid::new()).await,
            other => Ok(guest.inject(other).await?),
        }
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        guest.send_rpc(Report::Signal(signal as i32)).await;
        let base = *guest.thread_state_mut();
        if signal == Signal::SIGUSR2 && base != 0 {
            let field = |offset: usize| base + offset;
            let mut args = [0usize; 3];
            for (i, offset) in [
                std::mem::offset_of!(ExecArgs, path),
                std::mem::offset_of!(ExecArgs, argv),
                std::mem::offset_of!(ExecArgs, envp),
            ]
            .into_iter()
            .enumerate()
            {
                let addr = Addr::<usize>::from_raw(field(offset)).ok_or(Errno::EFAULT)?;
                args[i] = guest.memory().read_value(addr)?;
            }
            let execve = Syscall::from_raw(
                Sysno::execve,
                SyscallArgs::new(args[0], args[1], args[2], 0, 0, 0),
            );
            // Returns only if the exec fails.
            let result = guest.inject(execve).await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
        }
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

    extern "C" fn stop_repeatedly(_: *mut libc::c_void) -> *mut libc::c_void {
        while !STOP.load(Ordering::Relaxed) {
            // SAFETY: raise has no memory-safety preconditions. SIGTSTP keeps
            // its default (stop) disposition, so delivering it initiates a
            // group stop that reaches the marker thread.
            unsafe { libc::raise(libc::SIGTSTP) };
        }
        std::ptr::null_mut()
    }
    // The guest is a fork of the multithreaded test process, so a lock another
    // test thread held at the fork stays held here forever. `std::thread`
    // takes std's stack-overflow `thread_info` lock when the thread starts and
    // again when it exits, and a guest that inherited it held hangs in
    // `join`. A raw pthread takes no std lock.
    let mut stopper: libc::pthread_t = 0;
    // SAFETY: stop_repeatedly has the pthread start-routine signature and
    // captures nothing; stopper is written before it is joined below.
    assert_eq!(
        unsafe {
            libc::pthread_create(
                &mut stopper,
                std::ptr::null(),
                stop_repeatedly,
                std::ptr::null_mut(),
            )
        },
        0
    );

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
    // SAFETY: stopper is a joinable thread created above and joined once.
    assert_eq!(
        unsafe { libc::pthread_join(stopper, std::ptr::null_mut()) },
        0
    );
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
///
/// That stop takes the signal out of the kernel's queue into
/// `pending_signal`; the Tool must still be told about it before the resume
/// delivers it, or a Tool that acts on signals never sees this one
/// (https://github.com/rrnewton/hermit/issues/3468: Hermit's
/// `--sigint-instakill` missed a SIGINT that interrupted an injected stdin
/// read).
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
    let signals = log.signals.lock().unwrap();
    eprintln!(
        "PROBE interrupted guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
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
    assert_eq!(
        *signals,
        vec![libc::SIGUSR1],
        "the held signal is reported to the tool once before it is delivered"
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
    let signals = log.signals.lock().unwrap();
    eprintln!(
        "PROBE ppoll guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
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
    assert_eq!(
        *signals,
        vec![libc::SIGSYS],
        "the held SIGSYS is reported to the tool once"
    );
}

static SIGBUS_HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_sigbus(_signal: libc::c_int) {
    SIGBUS_HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
}

/// Returns whether `signal` is in the calling thread's signal mask.
///
/// # Safety
/// Reads the calling thread's signal mask.
unsafe fn is_blocked(signal: libc::c_int) -> libc::c_int {
    unsafe {
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
        libc::sigismember(&current, signal)
    }
}

/// Linux's `dequeue_synchronous_signal` returns the first queued
/// synchronous-class entry (positive `si_code`) without consulting the mask
/// whenever some unblocked synchronous signal is pending, and the step
/// SIGTRAP always is. So a signal the guest keeps blocked can stop the
/// injected step after its `syscall` completed. Returning it to the kernel
/// queue must leave the guest's mask alone: unmasking it at the end of the
/// step would unblock a signal the guest itself blocked.
///
/// Oracle: the same guest body under `strace -f` prints "0 0 1 1" (a tracer
/// resuming a blocked signal has `ptrace_signal` requeue it, so its handler
/// does not run, and SIGBUS stays blocked). Untraced Linux prints "0 1 1 1";
/// no ptrace tracer can match its SIGBUS handler count.
#[test]
fn guest_blocked_signal_dequeued_after_injected_syscall_stays_blocked() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        SIGBUS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        SIGSEGV_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGBUS, count_sigbus);
        install_counter(libc::SIGSEGV, count_sigsegv);
        block(&[libc::SIGBUS, libc::SIGSEGV]);
        queue_to_self(libc::SIGBUS, 1);
        queue_to_self(libc::SIGSEGV, 1);
        let mut unblock: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut unblock);
        libc::sigaddset(&mut unblock, libc::SIGSEGV);
        let ret = libc::syscall(
            libc::SYS_write,
            UNBLOCK_FD,
            &unblock as *const libc::sigset_t,
            0usize,
        );
        println!(
            "{ret} {} {} {}",
            SIGBUS_HANDLER_CALLS.load(Ordering::Relaxed),
            SIGSEGV_HANDLER_CALLS.load(Ordering::Relaxed),
            is_blocked(libc::SIGBUS)
        );
    })
    .expect("run guest-blocked signal guest");
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
        "PROBE guest-blocked guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    assert_eq!(*injected, vec![Ok(0)], "the unblock runs once and succeeds");
    assert_eq!(
        stdout.trim(),
        "0 0 1 1",
        "success, SIGBUS still blocked and not run, SIGSEGV run once (strace oracle)"
    );
    // The requeued SIGBUS sits ahead of SIGSEGV, so the same quirk dequeues
    // it once more at the next resume: the tool sees it (as `strace -f` does)
    // and its requeue leaves it blocked, then SIGSEGV is delivered.
    assert_eq!(
        *signals,
        vec![libc::SIGBUS, libc::SIGSEGV],
        "the still-blocked SIGBUS is reported and requeued, then SIGSEGV delivered"
    );
}

/// The mask-swapping variant: an injected `ppoll` whose temporary mask keeps
/// SIGBUS blocked but unblocks SIGSYS, both queued with a positive `si_code`.
/// SIGBUS is dequeued first, still blocked; it must be requeued rather than
/// held, or it occupies the single hold slot and SIGSYS then fails the step
/// closed.
///
/// Oracle: the same guest body under `strace -f` prints "-1 4 1 0 1 1"
/// (EINTR, SIGSYS handler once, SIGBUS handler never, both blocked again
/// once `ppoll` restores the saved mask). Untraced Linux prints
/// "-1 4 1 1 1 1".
#[test]
fn injected_mask_swapping_syscall_requeues_a_signal_its_mask_blocks() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        SIGSYS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        SIGBUS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGSYS, count_sigsys);
        install_counter(libc::SIGBUS, count_sigbus);
        block(&[libc::SIGBUS, libc::SIGSYS]);
        queue_to_self(libc::SIGBUS, 1);
        queue_to_self(libc::SIGSYS, 1);
        let mut args: PpollArgs = std::mem::zeroed();
        libc::sigemptyset(&mut args.mask);
        libc::sigaddset(&mut args.mask, libc::SIGBUS);
        args.timeout.tv_sec = 5;
        let ret = libc::syscall(
            libc::SYS_write,
            PPOLL_FD,
            &mut args as *mut PpollArgs,
            0usize,
        );
        let errno = *libc::__errno_location();
        println!(
            "{ret} {errno} {} {} {} {}",
            SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed),
            SIGBUS_HANDLER_CALLS.load(Ordering::Relaxed),
            is_blocked(libc::SIGBUS),
            is_blocked(libc::SIGSYS)
        );
    })
    .expect("run ppoll guest-blocked guest");
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
        "PROBE ppoll-blocked guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    assert_eq!(
        *injected,
        vec![Err(Errno::ERESTARTNOHAND.into_raw())],
        "ppoll is interrupted by the signal its mask unblocks"
    );
    assert_eq!(
        stdout.trim(),
        format!("-1 {} 1 0 1 1", libc::EINTR),
        "EINTR, SIGSYS run once, SIGBUS never run, saved mask restored (strace oracle)"
    );
    assert_eq!(
        *signals,
        vec![libc::SIGSYS],
        "the held SIGSYS is reported once; the still-blocked SIGBUS is never delivered"
    );
}

/// Two synchronous-class signals requeued after an injected `rt_sigprocmask`
/// completes are pending again before the callback's next injected
/// `syscall` (`getpid`), which is therefore reported as interrupted. Both
/// handlers still run exactly once and the guest sees EINTR.
///
/// This is not native Linux, where both handlers run between the two
/// syscalls and `getpid` succeeds; untraced the same guest body prints the
/// pid. The signal that stops `getpid` before its `syscall` (SIGSYS) is
/// held in the single `pending_signal` slot and reported to the tool before
/// the resume delivers it; SIGSEGV is then delivered from the kernel queue.
///
/// Known gap pinned here, tracked in TaskGraph: the interrupted `getpid`
/// and the signal parked in the single slot are `reverie_pending_signal_single_slot`.
#[test]
fn requeued_signals_interrupt_the_next_injected_syscall() {
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
            UNBLOCK_THEN_GETPID_FD,
            &set as *const libc::sigset_t,
            0usize,
        );
        let errno = *libc::__errno_location();
        println!(
            "{ret} {errno} {} {}",
            SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed),
            SIGSEGV_HANDLER_CALLS.load(Ordering::Relaxed)
        );
    })
    .expect("run requeue-then-interrupt guest");
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
        "PROBE requeue-interrupt guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    assert_eq!(
        *injected,
        vec![Ok(0), Err(Errno::ERESTARTSYS.into_raw())],
        "the unblock completes; a requeued signal interrupts getpid before it runs"
    );
    assert_eq!(
        stdout.trim(),
        format!("-1 {} 1 1", libc::EINTR),
        "guest sees EINTR and each handler run once"
    );
    assert_eq!(
        *signals,
        vec![libc::SIGSYS, libc::SIGSEGV],
        "the held SIGSYS is reported before SIGSEGV, each once"
    );
}

/// An injected `ppoll` whose temporary mask unblocks two pending
/// synchronous-class signals. Both are dequeued ahead of the step SIGTRAP, one
/// stop each. The first is held for delivery at the guest's syscall site and
/// the step stops there, leaving the second (and the step SIGTRAP) queued in
/// the kernel: holding both would need a second hold slot, and returning the
/// second to the queue by masking it would discard `ppoll`'s saved mask.
///
/// Oracle: untraced Linux and `strace -f` both print "-1 4 1 1 1 1" (EINTR,
/// each handler once, both blocked again once the saved mask is restored).
///
/// The held SIGSYS is reported to the tool when the resume delivers it, and
/// SIGSEGV is reported when the kernel then delivers it from its queue.
#[test]
fn injected_mask_swapping_syscall_holds_the_first_of_two_unblocked_signals() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        SIGSYS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        SIGSEGV_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGSYS, count_sigsys);
        install_counter(libc::SIGSEGV, count_sigsegv);
        block(&[libc::SIGSYS, libc::SIGSEGV]);
        queue_to_self(libc::SIGSYS, 1);
        queue_to_self(libc::SIGSEGV, 1);
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
        println!(
            "{ret} {errno} {} {} {} {}",
            SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed),
            SIGSEGV_HANDLER_CALLS.load(Ordering::Relaxed),
            is_blocked(libc::SIGSYS),
            is_blocked(libc::SIGSEGV)
        );
    })
    .expect("run ppoll two-signal guest");
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
        "PROBE ppoll-two guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    assert_eq!(
        *injected,
        vec![Err(Errno::ERESTARTNOHAND.into_raw())],
        "ppoll is interrupted by the signals its mask unblocks"
    );
    assert_eq!(
        stdout.trim(),
        format!("-1 {} 1 1 1 1", libc::EINTR),
        "EINTR, each handler run once, saved mask restored (native and strace oracle)"
    );
    assert_eq!(
        *signals,
        vec![libc::SIGSYS, libc::SIGSEGV],
        "the held SIGSYS and then SIGSEGV are each reported once"
    );
}

/// After a held signal stops the step of an injected `ppoll`, that step's
/// SIGTRAP is still queued. The same callback's next injection (`getpid`)
/// meets it before its own `syscall` executes; it must be discarded rather
/// than read as that step's completion, which would report RAX (the syscall
/// number, 39) for a `getpid` that never ran.
///
/// Known gaps pinned here, tracked in TaskGraph
/// `reverie_pending_signal_single_slot`: resuming the second step lets the
/// kernel restore `ppoll`'s saved mask before the held SIGSYS is delivered,
/// so it is requeued blocked and its handler never runs. Untraced, the same
/// callback would return the pid after one SIGSYS handler run.
///
/// Because the restored mask blocks SIGSYS at the final resume, the kernel
/// requeues it instead of delivering it, so it is not reported to the tool:
/// a report here would claim a delivery that did not happen, and the tool
/// would see the signal a second time once the guest unblocks it.
#[test]
fn injection_after_a_held_signal_discards_the_stale_step_trap() {
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
            PPOLL_THEN_GETPID_FD,
            &mut args as *mut PpollArgs,
            0usize,
        );
        println!(
            "{} {} {} {}",
            ret == libc::getpid() as i64,
            SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed),
            is_blocked(libc::SIGSYS),
            ret
        );
    })
    .expect("run ppoll-then-getpid guest");
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
        "PROBE ppoll-getpid guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    assert_eq!(injected.len(), 2, "{:?}", *injected);
    assert_eq!(
        injected[0],
        Err(Errno::ERESTARTNOHAND.into_raw()),
        "ppoll is interrupted by the signal its mask unblocks"
    );
    assert_ne!(
        injected[1],
        Ok(libc::SYS_getpid),
        "the stale step SIGTRAP was read as getpid's completion"
    );
    let fields: Vec<&str> = stdout.trim().split(' ').collect();
    assert_eq!(
        fields[..3],
        ["true", "0", "1"],
        "getpid ran and returned the pid; SIGSYS stays pending and blocked (known gap)"
    );
    assert_eq!(
        *signals,
        Vec::<i32>::new(),
        "the requeued, still-blocked SIGSYS is not reported as delivered"
    );
}

/// Whether `signal` is pending for the calling thread.
///
/// # Safety
/// Plain libc calls on a local set.
unsafe fn is_pending(signal: libc::c_int) -> libc::c_int {
    unsafe {
        let mut pending: libc::sigset_t = std::mem::zeroed();
        assert_eq!(
            libc::syscall(
                libc::SYS_rt_sigpending,
                &mut pending as *mut libc::sigset_t,
                8usize
            ),
            0
        );
        libc::sigismember(&pending, signal)
    }
}

/// Unblocks `signal` for the calling thread.
///
/// # Safety
/// Changes the calling thread's signal mask.
unsafe fn unblock(signal: libc::c_int) {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, signal);
        assert_eq!(
            libc::syscall(
                libc::SYS_rt_sigprocmask,
                libc::SIG_UNBLOCK,
                &set as *const libc::sigset_t,
                0usize,
                8usize
            ),
            0
        );
    }
}

/// The held SIGSYS of `injected_mask_swapping_syscall_keeps_the_saved_mask`,
/// reported to a signal hook that injects `getpid` before passing it
/// through. The injection's step lets the kernel restore `ppoll`'s saved
/// mask, which blocks SIGSYS, so Reverie delivers it under `ppoll`'s
/// temporary mask, with the saved one restored when its handler returns.
/// The guest's result is `ppoll`'s, not the hook's `getpid`, and nothing
/// restarts it. The outcome is the non-injecting `ReplaceMarker`'s: untraced
/// Linux and that test print "-1 4 1 1 0 1": EINTR, one handler run, SIGSYS
/// blocked again and no longer pending, and no second handler run once it
/// is unblocked.
#[test]
fn signal_hook_injecting_after_a_held_signal_keeps_the_guest_result() {
    let (output, log) = test_fn::<InjectInFirstSignalHook, _>(|| unsafe {
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
        let calls = SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed);
        let blocked = is_blocked(libc::SIGSYS);
        let pending = is_pending(libc::SIGSYS);
        unblock(libc::SIGSYS);
        println!(
            "{ret} {errno} {calls} {blocked} {pending} {} {}",
            SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed),
            libc::getpid()
        );
    })
    .expect("run signal-hook injection guest");
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
        "PROBE hook-inject-held guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    let (fields, pid) = stdout.trim().rsplit_once(' ').expect("guest pid");
    let pid: i64 = pid.parse().expect("guest pid");
    assert_eq!(
        fields,
        format!("-1 {} 1 1 0 1", libc::EINTR),
        "guest sees EINTR, one handler run, and SIGSYS blocked and not pending"
    );
    let restart = Err(Errno::ERESTARTNOHAND.into_raw());
    assert_eq!(
        *injected,
        vec![restart, Ok(pid)],
        "ppoll is interrupted and the hook's getpid runs"
    );
    assert_eq!(*signals, vec![libc::SIGSYS], "SIGSYS is reported once");
}

/// As `signal_hook_injecting_after_a_held_signal_keeps_the_guest_result`, but
/// the tool returns 0 for the guest's syscall, so nothing restarts it. SIGSYS
/// is still delivered under the injected `ppoll`'s temporary mask before the
/// guest's syscall returns, and is blocked again afterwards. The guest's
/// outcome is the one it has under the non-injecting `ReplaceMarker`, run
/// here too: "0 1 1 0 1".
#[test]
fn signal_hook_injecting_after_a_held_signal_delivers_it_as_without_the_injection() {
    fn guest() {
        // SAFETY: plain libc calls on this thread's signal state and on
        // buffers that outlive each call.
        unsafe {
            SIGSYS_HANDLER_CALLS.store(0, Ordering::Relaxed);
            install_counter(libc::SIGSYS, count_sigsys);
            block(&[libc::SIGSYS]);
            queue_to_self(libc::SIGSYS, 1);
            let mut args: PpollArgs = std::mem::zeroed();
            libc::sigemptyset(&mut args.mask);
            args.timeout.tv_sec = 5;
            let ret = libc::syscall(
                libc::SYS_write,
                PPOLL_THEN_ZERO_FD,
                &mut args as *mut PpollArgs,
                0usize,
            );
            let calls = SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed);
            let blocked = is_blocked(libc::SIGSYS);
            let pending = is_pending(libc::SIGSYS);
            unblock(libc::SIGSYS);
            println!(
                "{ret} {calls} {blocked} {pending} {} {}",
                SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed),
                libc::getpid()
            );
        }
    }
    let (plain, _) = test_fn::<ReplaceMarker, _>(guest).expect("run non-injecting guest");
    let (output, log) =
        test_fn::<InjectInFirstSignalHook, _>(guest).expect("run signal-hook injection guest");
    for output in [&plain, &output] {
        assert_eq!(
            output.status,
            ExitStatus::Exited(0),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let plain = String::from_utf8_lossy(&plain.stdout);
    let injected = log.injected.lock().unwrap();
    let signals = log.signals.lock().unwrap();
    eprintln!(
        "PROBE hook-inject-held-zero guest={} plain={} injected={:?} signals={:?}",
        stdout.trim(),
        plain.trim(),
        *injected,
        *signals
    );
    let (fields, pid) = stdout.trim().rsplit_once(' ').expect("guest pid");
    let pid: i64 = pid.parse().expect("guest pid");
    let (plain_fields, _) = plain.trim().rsplit_once(' ').expect("guest pid");
    assert_eq!(
        plain_fields, "0 1 1 0 1",
        "without the hook's injection SIGSYS runs its handler during the marker"
    );
    assert_eq!(
        fields, plain_fields,
        "the hook's injection does not change the guest's outcome"
    );
    assert_eq!(
        *injected,
        vec![Err(Errno::ERESTARTNOHAND.into_raw()), Ok(pid)],
        "ppoll is interrupted and the hook's getpid runs"
    );
    assert_eq!(*signals, vec![libc::SIGSYS], "SIGSYS is reported once");
}

/// The ordinary signal route with the same hook: the guest's own `ppoll`,
/// whose temporary mask lets a pending SIGSYS through, is not intercepted.
/// The hook's `getpid` lets the kernel restore the saved mask, which blocks
/// SIGSYS, so Reverie delivers it under `ppoll`'s temporary mask, with the
/// saved one restored when its handler returns. The guest's result is its
/// own, not the hook's `getpid`. Untraced Linux prints "-1 4 1 1 0 1".
#[test]
fn signal_hook_injecting_at_a_delivery_stop_keeps_the_guest_result() {
    let (output, log) = test_fn::<InjectInFirstSignalHook, _>(|| unsafe {
        SIGSYS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGSYS, count_sigsys);
        block(&[libc::SIGSYS]);
        queue_to_self(libc::SIGSYS, 1);
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        let timeout = libc::timespec {
            tv_sec: 5,
            tv_nsec: 0,
        };
        let ret = libc::syscall(
            libc::SYS_ppoll,
            0usize,
            0usize,
            &timeout as *const libc::timespec,
            &mask as *const libc::sigset_t,
            8usize,
        );
        let errno = *libc::__errno_location();
        let calls = SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed);
        let blocked = is_blocked(libc::SIGSYS);
        let pending = is_pending(libc::SIGSYS);
        unblock(libc::SIGSYS);
        println!(
            "{ret} {errno} {calls} {blocked} {pending} {} {}",
            SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed),
            libc::getpid()
        );
    })
    .expect("run signal-hook injection guest");
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
        "PROBE hook-inject-ordinary guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    let (fields, pid) = stdout.trim().rsplit_once(' ').expect("guest pid");
    let pid: i64 = pid.parse().expect("guest pid");
    assert_eq!(
        fields,
        format!("-1 {} 1 1 0 1", libc::EINTR),
        "guest sees EINTR, one handler run, and SIGSYS blocked and not pending"
    );
    assert_eq!(*injected, vec![Ok(pid)], "the hook's getpid runs once");
    assert_eq!(*signals, vec![libc::SIGSYS], "SIGSYS is reported once");
}

/// A held SIGTSTP with its default (stop) disposition, as in
/// `signal_pending_before_injected_syscall_interrupts_it`. Resuming with it
/// starts a group stop, which the tracer sees as a second SIGTSTP stop
/// without siginfo. That stop is a consequence of the one delivery the tool
/// was told about, so the tool is not told again. A restarted ptraced tracee
/// does not honor the stop; the interrupted marker restarts and its
/// injections run again, the `getpid` now returning the pid.
#[test]
fn held_default_action_stop_signal_is_reported_once() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        // As in `guest`: a stop signal is discarded in an orphaned group.
        assert_eq!(libc::setpgid(0, 0), 0);
        let set = block(&[libc::SIGTSTP]);
        assert_eq!(
            libc::syscall(
                libc::SYS_tgkill,
                libc::getpid(),
                libc::syscall(libc::SYS_gettid),
                libc::SIGTSTP
            ),
            0
        );
        let ret = libc::syscall(
            libc::SYS_write,
            UNBLOCK_THEN_GETPID_FD,
            &set as *const libc::sigset_t,
            0usize,
        );
        println!("{ret} {}", libc::getpid());
    })
    .expect("run held stop-signal guest");
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
        "PROBE held-sigtstp guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    let (ret, pid) = stdout.trim().split_once(' ').expect("guest output");
    let pid: i64 = pid.parse().expect("guest pid");
    assert_eq!(ret, pid.to_string(), "the restarted marker returns the pid");
    assert_eq!(
        *injected,
        vec![Ok(0), Err(Errno::ERESTARTSYS.into_raw()), Ok(0), Ok(pid)],
        "SIGTSTP interrupts getpid; the restarted marker runs both injections"
    );
    assert_eq!(
        *signals,
        vec![libc::SIGTSTP],
        "one SIGTSTP is reported once, not again at its group stop"
    );
}

/// SIGSTOP sent by an injected `tgkill` is not synchronous-class, so, as
/// SIGUSR1 in `signal_pending_before_injected_syscall_interrupts_it`, the
/// `tgkill`'s step SIGTRAP is dequeued first and SIGSTOP stops the following
/// `getpid` before its `syscall`, which holds it. It is resumed without a
/// report, as the tracer never reports SIGSTOP to the tool: the resume starts
/// a group stop, which a ptraced tracee does not honor once restarted. The
/// interrupted marker restarts, sends no second SIGSTOP, and its `getpid`
/// now returns the pid.
#[test]
fn held_sigstop_is_not_reported() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        let mut sent: i32 = 0;
        let ret = libc::syscall(
            libc::SYS_write,
            SIGSTOP_THEN_GETPID_FD,
            &mut sent as *mut i32,
            0usize,
        );
        println!("{ret} {}", libc::getpid());
    })
    .expect("run held-SIGSTOP guest");
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
        "PROBE held-sigstop guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    let (ret, pid) = stdout.trim().split_once(' ').expect("guest pid");
    assert_eq!(ret, pid, "the guest gets the getpid result");
    let pid: i64 = pid.parse().expect("guest pid");
    assert_eq!(
        *injected,
        vec![Ok(0), Err(Errno::ERESTARTSYS.into_raw()), Ok(pid)],
        "tgkill succeeds, SIGSTOP interrupts getpid before it runs, and the restarted marker's getpid runs"
    );
    assert_eq!(*signals, Vec::<i32>::new(), "SIGSTOP is not reported");
}

/// The ordinary route of a default-action SIGTSTP: its delivery stop is
/// reported, and the group stop that follows the delivery is not.
#[test]
fn default_action_stop_signal_is_reported_once() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        assert_eq!(libc::setpgid(0, 0), 0);
        assert_eq!(libc::raise(libc::SIGTSTP), 0);
        println!("resumed");
    })
    .expect("run stop-signal guest");
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let signals = log.signals.lock().unwrap();
    eprintln!(
        "PROBE sigtstp guest={} signals={:?}",
        stdout.trim(),
        *signals
    );
    assert_eq!(stdout.trim(), "resumed");
    assert_eq!(
        *signals,
        vec![libc::SIGTSTP],
        "one SIGTSTP is reported once, not again at its group stop"
    );
}

/// A guest may queue itself a SIGTRAP whose `si_code` is that of a syscall
/// stop (`SIGTRAP | 0x80`). Here SIGUSR1 is held as in
/// `signal_pending_before_injected_syscall_interrupts_it`, and the callback's
/// third injection queues such a SIGTRAP, which Linux dequeues ahead of the
/// step SIGTRAP: the injection ends at a genuine signal-delivery stop whose
/// siginfo reads like a syscall stop. Resuming from it delivers the held
/// SIGUSR1, so the tool must be told about SIGUSR1 first.
#[test]
fn held_signal_is_reported_at_a_delivery_stop_with_syscall_stop_siginfo() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        SIGUSR1_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGUSR1, count_sigusr1);
        let mut args: UnblockThenTrapArgs = std::mem::zeroed();
        args.set = block(&[libc::SIGUSR1]);
        args.info.si_signo = libc::SIGTRAP;
        args.info.si_code = libc::SIGTRAP | 0x80;
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
            UNBLOCK_GETPID_THEN_TRAP_FD,
            &mut args as *mut UnblockThenTrapArgs,
            0usize,
        );
        println!("{ret} {}", SIGUSR1_HANDLER_CALLS.load(Ordering::Relaxed));
    })
    .expect("run syscall-stop siginfo guest");
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
        "PROBE trap-siginfo guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    assert_eq!(
        *injected,
        vec![Ok(0), Err(Errno::ERESTARTSYS.into_raw()), Ok(0)],
        "SIGUSR1 interrupts getpid; the SIGTRAP is queued"
    );
    assert_eq!(
        stdout.trim(),
        "0 1",
        "the guest sees the last result and SIGUSR1's handler runs once"
    );
    assert_eq!(
        *signals,
        vec![libc::SIGUSR1],
        "the held SIGUSR1 is reported once before it is delivered"
    );
}

#[cfg(target_arch = "x86_64")]
static SECCOMP_TRAPS: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_arch = "x86_64")]
static SECCOMP_CODE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
#[cfg(target_arch = "x86_64")]
static SECCOMP_SYSCALL: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
#[cfg(target_arch = "x86_64")]
static SECCOMP_RAX_SEEN: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// A SIGSYS handler emulating a seccomp-trapped syscall: records `si_code`,
/// `si_syscall` and the RAX it finds, then makes the syscall return 4242.
#[cfg(target_arch = "x86_64")]
extern "C" fn emulate_trapped_syscall(
    _signal: libc::c_int,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    // SAFETY: the kernel passes a valid siginfo and ucontext to an
    // SA_SIGINFO handler. For SIGSYS the siginfo union holds `_sigsys`
    // (`void *_call_addr; int _syscall; unsigned _arch;`) at offset 16.
    unsafe {
        let syscall = *(info.cast::<u8>().add(24).cast::<i32>());
        let gregs = &mut (*context.cast::<libc::ucontext_t>()).uc_mcontext.gregs;
        SECCOMP_CODE.store((*info).si_code as i64, Ordering::Relaxed);
        SECCOMP_SYSCALL.store(syscall as i64, Ordering::Relaxed);
        SECCOMP_RAX_SEEN.store(gregs[libc::REG_RAX as usize], Ordering::Relaxed);
        gregs[libc::REG_RAX as usize] = 4242;
    }
    SECCOMP_TRAPS.fetch_add(1, Ordering::Relaxed);
}

/// Loads a seccomp filter that traps `getppid` (`SECCOMP_RET_TRAP`) and
/// allows everything else, with `emulate_trapped_syscall` as the SIGSYS
/// handler.
///
/// # Safety
/// Replaces the process-wide SIGSYS disposition and restricts the calling
/// thread's syscalls for the rest of its life.
#[cfg(target_arch = "x86_64")]
unsafe fn trap_getppid() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = emulate_trapped_syscall as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGSYS, &action, std::ptr::null_mut()),
            0
        );
        let statement = |code: u32, k: u32, jt: u8, jf: u8| libc::sock_filter {
            code: code as u16,
            jt,
            jf,
            k,
        };
        let filter = [
            // seccomp_data.nr is at offset 0.
            statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0, 0),
            statement(
                libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
                libc::SYS_getppid as u32,
                0,
                1,
            ),
            statement(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_TRAP, 0, 0),
            statement(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_ALLOW, 0, 0),
        ];
        let program = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_ptr() as *mut libc::sock_filter,
        };
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        assert_eq!(
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &program as *const libc::sock_fprog
            ),
            0
        );
    }
}

/// Prints the last trapped syscall's return value and what the handler saw.
#[cfg(target_arch = "x86_64")]
fn print_seccomp_trap(ret: i64) {
    println!(
        "{ret} {} {} {} {}",
        SECCOMP_TRAPS.load(Ordering::Relaxed),
        SECCOMP_CODE.load(Ordering::Relaxed),
        SECCOMP_SYSCALL.load(Ordering::Relaxed),
        SECCOMP_RAX_SEEN.load(Ordering::Relaxed)
    );
}

/// A seccomp `SECCOMP_RET_TRAP` filter the guest installed can trap a syscall
/// the tool injects. The kernel does not execute it: it rolls RAX back to the
/// syscall number and raises SIGSYS (`si_code` `SYS_SECCOMP`) with RIP already
/// past the private `syscall`, ahead of the step SIGTRAP. The tool must be
/// told the syscall did not run (`ENOSYS`), not handed the leftover syscall
/// number as a success, and the guest's SIGSYS handler must still run.
///
/// The first line is the in-place control, the same guest calling `getppid`
/// itself: the handler sees `si_code` 1, `si_syscall` 110 and RAX 110, and
/// the call returns the handler's 4242. In the injected case the handler
/// sees the `-ENOSYS` the tool returned instead of 110 (seccomp(2) leaves
/// that register architecture-dependent), and its 4242 is again the result.
#[cfg(target_arch = "x86_64")]
#[test]
fn injected_syscall_trapped_by_guest_seccomp_reports_enosys() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        trap_getppid();
        print_seccomp_trap(libc::syscall(libc::SYS_getppid));
        let ret = libc::syscall(libc::SYS_write, GETPPID_FD, std::ptr::null::<u8>(), 0usize);
        print_seccomp_trap(ret);
    })
    .expect("run seccomp-trap guest");
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
        "PROBE seccomp-trap guest={:?} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    assert_eq!(
        *injected,
        vec![Err(libc::ENOSYS)],
        "the trapped getppid did not run"
    );
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(
        lines,
        [
            format!("4242 1 1 {} {}", libc::SYS_getppid, libc::SYS_getppid),
            format!("4242 2 1 {} {}", libc::SYS_getppid, -libc::ENOSYS),
        ],
        "in place and injected, the guest's SIGSYS handler emulates getppid once"
    );
    assert_eq!(
        *signals,
        vec![libc::SIGSYS, libc::SIGSYS],
        "each SIGSYS reaches the tool"
    );
}

/// How a guest disposes of a requeued SIGBUS without its delivery.
#[derive(Clone, Copy)]
enum Consume {
    /// `rt_sigtimedwait`, which the tool does not intercept.
    Sigtimedwait,
    /// A switch to `SIG_IGN` and back, which discards the pending signal.
    Ignore,
}

/// Leaves a SIGBUS the tool was told about pending, consumes it as `consume`
/// says, and prints the `UNBLOCK_FD` marker's result, SIGSEGV's handler runs,
/// and whether SIGBUS was pending before and after.
///
/// This is `guest_blocked_signal_dequeued_after_injected_syscall_stays_blocked`:
/// SIGBUS and SIGSEGV are queued blocked with a positive `si_code`, and the
/// marker unblocks SIGSEGV only. Linux's synchronous dequeue takes the
/// still-blocked SIGBUS at a delivery stop, which is reported to the tool,
/// and resuming it requeues it because it is blocked. SIGSEGV is then
/// delivered.
///
/// # Safety
/// Changes the calling thread's signal state.
unsafe fn requeue_then_consume(consume: Consume) -> String {
    unsafe {
        SIGBUS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        SIGSEGV_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGBUS, count_sigbus);
        install_counter(libc::SIGSEGV, count_sigsegv);
        block(&[libc::SIGBUS, libc::SIGSEGV]);
        queue_to_self(libc::SIGBUS, 1);
        queue_to_self(libc::SIGSEGV, 1);
        let mut segv: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut segv);
        libc::sigaddset(&mut segv, libc::SIGSEGV);
        let ret = libc::syscall(
            libc::SYS_write,
            UNBLOCK_FD,
            &segv as *const libc::sigset_t,
            0usize,
        );
        let before = is_pending(libc::SIGBUS);
        match consume {
            Consume::Sigtimedwait => {
                let mut bus: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut bus);
                libc::sigaddset(&mut bus, libc::SIGBUS);
                let mut info: libc::siginfo_t = std::mem::zeroed();
                let timeout = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                assert_eq!(
                    libc::syscall(
                        libc::SYS_rt_sigtimedwait,
                        &bus as *const libc::sigset_t,
                        &mut info as *mut libc::siginfo_t,
                        &timeout as *const libc::timespec,
                        8usize,
                    ),
                    libc::SIGBUS as libc::c_long
                );
            }
            Consume::Ignore => {
                assert_ne!(libc::signal(libc::SIGBUS, libc::SIG_IGN), libc::SIG_ERR);
                install_counter(libc::SIGBUS, count_sigbus);
            }
        }
        format!(
            "{ret} {} {before} {}",
            SIGSEGV_HANDLER_CALLS.load(Ordering::Relaxed),
            is_pending(libc::SIGBUS)
        )
    }
}

/// How the guest sends itself the second SIGBUS after its requeued first one
/// was consumed.
#[derive(Clone, Copy)]
enum Resend {
    /// `kill`, after no intercepted syscall.
    Kill,
    /// `rt_tgsigqueueinfo` with the siginfo the first SIGBUS was queued with,
    /// which its requeue kept, after no intercepted syscall, so that the
    /// siginfo's contents cannot tell it from the requeued instance.
    Lookalike,
    /// As `Lookalike`, after an intercepted `getppid` marker.
    LookalikeAfterSyscall,
}

/// Sends the calling thread SIGBUS as `resend` says.
///
/// # Safety
/// Sends a signal to the calling thread, which must be the leader.
unsafe fn resend_sigbus(resend: Resend) {
    unsafe {
        if let Resend::Kill = resend {
            assert_eq!(libc::kill(libc::getpid(), libc::SIGBUS), 0);
            return;
        }
        if let Resend::LookalikeAfterSyscall = resend {
            libc::syscall(libc::SYS_write, GETPPID_FD, std::ptr::null::<u8>(), 0usize);
        }
        queue_to_self(libc::SIGBUS, 1);
    }
}

/// The SIGBUS requeued after the tool was told about it is consumed without
/// a delivery. A later SIGBUS that the guest sends itself as `resend` says is
/// a different instance, which the tool has not seen: it is reported on its
/// ordinary delivery when the guest unblocks it. Untraced Linux runs the
/// handler once, for the second SIGBUS.
fn consumed_requeue_then_resend_is_reported(consume: Consume, resend: Resend) {
    let (output, log) = test_fn::<ReplaceMarker, _>(move || unsafe {
        let first = requeue_then_consume(consume);
        resend_sigbus(resend);
        unblock(libc::SIGBUS);
        println!("{first} {}", SIGBUS_HANDLER_CALLS.load(Ordering::Relaxed));
    })
    .expect("run consumed-requeue guest");
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
        "PROBE consumed-requeue guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    assert_eq!(
        stdout.trim(),
        "0 1 1 0 1",
        "SIGSEGV runs once; the requeued SIGBUS is pending, then consumed; the second runs the handler once"
    );
    let mut expected = vec![Ok(0)];
    if let Resend::LookalikeAfterSyscall = resend {
        expected.push(Ok(std::process::id() as i64));
    }
    assert_eq!(*injected, expected, "the unblock runs once and succeeds");
    assert_eq!(
        *signals,
        vec![libc::SIGBUS, libc::SIGSEGV, libc::SIGBUS],
        "the tool sees each SIGBUS once, and SIGSEGV"
    );
}

#[test]
fn requeue_consumed_by_sigtimedwait_does_not_hide_a_later_signal() {
    consumed_requeue_then_resend_is_reported(Consume::Sigtimedwait, Resend::Kill);
}

#[test]
fn requeue_discarded_by_sig_ign_does_not_hide_a_later_signal() {
    consumed_requeue_then_resend_is_reported(Consume::Ignore, Resend::Kill);
}

/// The second SIGBUS's siginfo cannot tell it from the requeued instance (see
/// `Resend::Lookalike`), and no intercepted syscall passes between the
/// consumption and the lookalike; it is still a separate instance, which the
/// tool sees.
#[test]
fn requeue_consumed_by_sigtimedwait_does_not_hide_a_lookalike_signal() {
    consumed_requeue_then_resend_is_reported(Consume::Sigtimedwait, Resend::Lookalike);
}

/// As `consumed_requeue_then_resend_is_reported`, but the second SIGBUS is
/// held by the injected `ppoll` of another marker, whose temporary mask lets
/// it through, and reported before the resume that delivers it. Untraced
/// Linux interrupts that `ppoll` with EINTR and runs the handler once.
fn consumed_requeue_then_held(resend: Resend) {
    let (output, log) = test_fn::<ReplaceMarker, _>(move || unsafe {
        let first = requeue_then_consume(Consume::Sigtimedwait);
        resend_sigbus(resend);
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
        println!(
            "{first} {ret} {errno} {} {}",
            SIGBUS_HANDLER_CALLS.load(Ordering::Relaxed),
            is_pending(libc::SIGBUS),
        );
    })
    .expect("run consumed-requeue guest");
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
        "PROBE consumed-requeue-held guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    assert_eq!(
        stdout.trim(),
        format!("0 1 1 0 -1 {} 1 0", libc::EINTR),
        "the second SIGBUS interrupts the ppoll marker and runs the handler once"
    );
    let mut expected = vec![Ok(0)];
    if let Resend::LookalikeAfterSyscall = resend {
        expected.push(Ok(std::process::id() as i64));
    }
    expected.push(Err(Errno::ERESTARTNOHAND.into_raw()));
    assert_eq!(
        *injected, expected,
        "the unblock succeeds and the ppoll is interrupted"
    );
    assert_eq!(
        *signals,
        vec![libc::SIGBUS, libc::SIGSEGV, libc::SIGBUS],
        "the tool sees each SIGBUS once, and SIGSEGV"
    );
}

#[test]
fn consumed_requeue_does_not_hide_a_later_held_signal() {
    consumed_requeue_then_held(Resend::Kill);
}

#[test]
fn consumed_requeue_does_not_hide_a_lookalike_held_signal() {
    consumed_requeue_then_held(Resend::LookalikeAfterSyscall);
}

/// Writes `line` to stdout with `writev` and exits with `exit_group`,
/// neither of which the tools intercept, so no callback follows.
///
/// # Safety
/// Ends the process.
unsafe fn print_and_exit_unintercepted(line: &str) -> ! {
    unsafe {
        let iov = libc::iovec {
            iov_base: line.as_ptr() as *mut libc::c_void,
            iov_len: line.len(),
        };
        assert_eq!(libc::writev(1, &iov, 1), line.len() as isize);
        libc::syscall(libc::SYS_exit_group, 0);
        unreachable!("exit_group returned");
    }
}

/// One injected `rt_sigprocmask` unblocks two pending signals. SIGSYS,
/// queued with a positive `si_code`, is dequeued ahead of the step SIGTRAP
/// and held. SIGUSR1, sent by `tgkill`, is not synchronous-class and stays
/// queued. The hook for SIGSYS injects `getpid`, and SIGUSR1 stops that step
/// before its `syscall`, so SIGUSR1 is held while the callback for SIGSYS is
/// still running. The guest then exits without another intercepted syscall,
/// so no later callback could hand SIGUSR1 back. On untraced Linux the
/// unblock returns 0 and delivers both, each handler running once.
///
/// Known gap pinned here: SIGUSR1 stays in the tracer's single hold slot,
/// which nothing resumes from, so it is never reported or delivered
/// (https://github.com/rrnewton/reverie/issues/845). The unblock's own result is the guest's 0, not
/// the hook's `getpid`.
#[test]
fn signal_held_by_a_signal_hook_injection_is_not_delivered() {
    let (output, log) = test_fn::<InjectInFirstSignalHook, _>(|| unsafe {
        SIGSYS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        SIGUSR1_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGSYS, count_sigsys);
        install_counter(libc::SIGUSR1, count_sigusr1);
        let set = block(&[libc::SIGSYS, libc::SIGUSR1]);
        queue_to_self(libc::SIGSYS, 1);
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
            UNBLOCK_FD,
            &set as *const libc::sigset_t,
            0usize,
        );
        print_and_exit_unintercepted(&format!(
            "{ret} {} {} {}\n",
            SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed),
            SIGUSR1_HANDLER_CALLS.load(Ordering::Relaxed),
            libc::getpid()
        ));
    })
    .expect("run hook-held signal guest");
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
        "PROBE hook-held guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    let (fields, _pid) = stdout.trim().rsplit_once(' ').expect("guest pid");
    assert_eq!(
        fields, "0 1 0",
        "the unblock succeeds and SIGSYS runs its handler; SIGUSR1 never does (known gap)"
    );
    assert_eq!(
        *injected,
        vec![Ok(0), Err(Errno::ERESTARTSYS.into_raw())],
        "the unblock completes; SIGUSR1 interrupts the hook's getpid before it runs"
    );
    assert_eq!(
        *signals,
        vec![libc::SIGSYS],
        "SIGUSR1, left in the hold slot, never reaches the tool (known gap)"
    );
}

/// The program `requeued_signal_survives_a_nonleader_exec` executes. It
/// prints whether SIGBUS is blocked and pending, installs a handler,
/// unblocks SIGBUS, and prints the handler's run count and the PID.
const UNBLOCK_SIGBUS_PY: &std::ffi::CStr = c"import os, signal
runs = []
blocked = int(signal.SIGBUS in signal.pthread_sigmask(signal.SIG_BLOCK, []))
pending = int(signal.SIGBUS in signal.sigpending())
signal.signal(signal.SIGBUS, lambda *_: runs.append(1))
signal.pthread_sigmask(signal.SIG_UNBLOCK, [signal.SIGBUS])
print(blocked, pending, len(runs), os.getpid())
";

/// A worker thread's blocked SIGBUS is reported to the tool and requeued on
/// the worker's private queue, as in `requeue_then_consume`, and SIGSEGV is
/// delivered. The worker then executes another program, which takes the
/// leader's TID and keeps the worker's private queue, and that program
/// unblocks SIGBUS, whose handler then runs once.
///
/// The tool is told about SIGBUS three times. Once before the exec; once
/// after it, as the new program's initialization injects syscalls whose step
/// SIGTRAP lets Linux's synchronous dequeue take the still-blocked SIGBUS at
/// the next resume (it is requeued again, as `strace -f` sees it); and once
/// for its delivery. Nothing in the tracer remembers the earlier reports.
#[test]
fn requeued_signal_survives_a_nonleader_exec() {
    extern "C" fn worker(_: *mut libc::c_void) -> *mut libc::c_void {
        // SAFETY: plain libc calls on this thread's signal state and on
        // buffers that outlive each call.
        unsafe {
            block(&[libc::SIGBUS, libc::SIGSEGV]);
            queue_to_self(libc::SIGBUS, 1);
            queue_to_self(libc::SIGSEGV, 1);
            let mut segv: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut segv);
            libc::sigaddset(&mut segv, libc::SIGSEGV);
            let ret = libc::syscall(
                libc::SYS_write,
                UNBLOCK_FD,
                &segv as *const libc::sigset_t,
                0usize,
            );
            assert_eq!(ret, 0);
            assert_eq!(SIGSEGV_HANDLER_CALLS.load(Ordering::Relaxed), 1);
            let path = c"/usr/bin/python3";
            let argv = [
                c"python3".as_ptr(),
                c"-c".as_ptr(),
                UNBLOCK_SIGBUS_PY.as_ptr(),
                std::ptr::null(),
            ];
            let envp = [std::ptr::null()];
            libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());
            libc::_exit(3);
        }
    }
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        SIGSEGV_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGSEGV, count_sigsegv);
        let mut thread: libc::pthread_t = 0;
        assert_eq!(
            libc::pthread_create(&mut thread, std::ptr::null(), worker, std::ptr::null_mut()),
            0
        );
        // The worker's exec ends this thread.
        libc::pthread_join(thread, std::ptr::null_mut());
        libc::_exit(4);
    })
    .expect("run nonleader-exec guest");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let injected = log.injected.lock().unwrap();
    let signals = log.signals.lock().unwrap();
    eprintln!(
        "PROBE nonleader-exec guest={} injected={:?} signals={:?} stderr={}",
        stdout.trim(),
        *injected,
        *signals,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (fields, pid) = stdout.trim().rsplit_once(' ').expect("program pid");
    assert_eq!(
        fields, "1 1 1",
        "SIGBUS stays blocked and pending across the exec, then runs the handler once"
    );
    let _: i64 = pid.parse().expect("program pid");
    assert_eq!(*injected, vec![Ok(0)], "the unblock runs once and succeeds");
    assert_eq!(
        *signals,
        vec![libc::SIGBUS, libc::SIGSEGV, libc::SIGBUS, libc::SIGBUS],
        "SIGBUS is reported before and after the exec, each time requeued, and when it is delivered"
    );
}

/// A held SIGUSR1 (as in `signal_pending_before_injected_syscall_interrupts_it`)
/// stays held while the same callback injects `fork`, and the callback
/// returns 0. The child's dispatch steps the parent to a signal-delivery stop
/// (its step SIGTRAP), from which the final resume delivers SIGUSR1 without
/// another stop, so it is reported before that resume.
#[test]
fn held_signal_is_reported_after_a_fork_injection() {
    let (output, log) = test_fn::<ReplaceMarker, _>(|| unsafe {
        SIGUSR1_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGUSR1, count_sigusr1);
        // The child's exit SIGCHLD stays pending rather than stopping the
        // parent at a time that depends on the child.
        block(&[libc::SIGCHLD]);
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
        let pid = libc::getpid();
        let ret = libc::syscall(
            libc::SYS_write,
            UNBLOCK_GETPID_FORK_THEN_ZERO_FD,
            &set as *const libc::sigset_t,
            0usize,
        );
        if libc::getpid() != pid {
            libc::_exit(0);
        }
        let calls = SIGUSR1_HANDLER_CALLS.load(Ordering::Relaxed);
        let mut status = 0;
        let child = libc::waitpid(-1, &mut status, 0);
        println!("{ret} {calls} {child} {status}");
    })
    .expect("run held-fork guest");
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
        "PROBE held-fork guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    let fields: Vec<&str> = stdout.trim().split(' ').collect();
    assert_eq!(injected.len(), 3, "unblock, getpid and fork are injected");
    let Ok(child) = injected[2] else {
        panic!("fork failed: {:?}", injected[2]);
    };
    assert_eq!(
        injected[..2],
        [Ok(0), Err(Errno::ERESTARTSYS.into_raw())],
        "the unblock completes; SIGUSR1 interrupts getpid before it runs"
    );
    assert_eq!(
        fields,
        ["0", "1", &child.to_string(), "0"],
        "the guest sees 0, runs the handler once, and reaps the forked child"
    );
    assert_eq!(
        *signals,
        vec![libc::SIGUSR1],
        "the held signal is reported once before it is delivered"
    );
}

/// `signal_hook_injecting_after_a_held_signal_keeps_the_guest_result` with a hook
/// that injects `fork`: the guest's interrupted `ppoll` keeps its
/// `-ERESTARTNOHAND` rather than the child's PID, which SIGSYS's delivery
/// turns into EINTR. Untraced Linux prints "-1 4 1 1 0 1".
#[test]
fn signal_hook_forking_after_a_held_signal_keeps_the_guest_result() {
    let (output, log) = test_fn::<ForkInFirstSignalHook, _>(|| unsafe {
        SIGSYS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGSYS, count_sigsys);
        block(&[libc::SIGSYS, libc::SIGCHLD]);
        queue_to_self(libc::SIGSYS, 1);
        let mut args: PpollArgs = std::mem::zeroed();
        libc::sigemptyset(&mut args.mask);
        libc::sigaddset(&mut args.mask, libc::SIGCHLD);
        args.timeout.tv_sec = 5;
        let pid = libc::getpid();
        let ret = libc::syscall(
            libc::SYS_write,
            PPOLL_FD,
            &mut args as *mut PpollArgs,
            0usize,
        );
        if libc::getpid() != pid {
            libc::_exit(0);
        }
        let errno = *libc::__errno_location();
        print_fork_hook_outcome(ret, errno);
    })
    .expect("run signal-hook fork guest");
    let child = check_fork_hook_outcome(&output, &log, "hook-fork-held");
    let restart = Err(Errno::ERESTARTNOHAND.into_raw());
    assert_eq!(
        *log.injected.lock().unwrap(),
        vec![restart, Ok(child)],
        "ppoll is interrupted and the hook's fork runs"
    );
}

/// `signal_hook_injecting_at_a_delivery_stop_keeps_the_guest_result` with a hook
/// that injects `fork`. Untraced Linux prints "-1 4 1 1 0 1".
#[test]
fn signal_hook_forking_at_a_delivery_stop_keeps_the_guest_result() {
    let (output, log) = test_fn::<ForkInFirstSignalHook, _>(|| unsafe {
        SIGSYS_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGSYS, count_sigsys);
        block(&[libc::SIGSYS, libc::SIGCHLD]);
        queue_to_self(libc::SIGSYS, 1);
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, libc::SIGCHLD);
        let timeout = libc::timespec {
            tv_sec: 5,
            tv_nsec: 0,
        };
        let pid = libc::getpid();
        let ret = libc::syscall(
            libc::SYS_ppoll,
            0usize,
            0usize,
            &timeout as *const libc::timespec,
            &mask as *const libc::sigset_t,
            8usize,
        );
        if libc::getpid() != pid {
            libc::_exit(0);
        }
        let errno = *libc::__errno_location();
        print_fork_hook_outcome(ret, errno);
    })
    .expect("run signal-hook fork guest");
    let child = check_fork_hook_outcome(&output, &log, "hook-fork-ordinary");
    assert_eq!(
        *log.injected.lock().unwrap(),
        vec![Ok(child)],
        "the hook's fork runs once"
    );
}

/// Prints a fork-hook guest's syscall result, errno, SIGSYS handler runs,
/// SIGSYS blocked and pending, handler runs after unblocking, and the child
/// it reaped.
///
/// # Safety
/// Changes the calling thread's signal mask and reaps a child.
unsafe fn print_fork_hook_outcome(ret: libc::c_long, errno: libc::c_int) {
    unsafe {
        let calls = SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed);
        let blocked = is_blocked(libc::SIGSYS);
        let pending = is_pending(libc::SIGSYS);
        unblock(libc::SIGSYS);
        let mut status = 0;
        let child = libc::waitpid(-1, &mut status, 0);
        println!(
            "{ret} {errno} {calls} {blocked} {pending} {} {child}",
            SIGSYS_HANDLER_CALLS.load(Ordering::Relaxed),
        );
    }
}

/// Checks `print_fork_hook_outcome`'s line and the hook reports, and returns
/// the reaped child.
fn check_fork_hook_outcome(output: &reverie::process::Output, log: &Log, probe: &str) -> i64 {
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!(
        "PROBE {probe} guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *log.injected.lock().unwrap(),
        *log.signals.lock().unwrap()
    );
    let (fields, child) = stdout.trim().rsplit_once(' ').expect("reaped child");
    let child: i64 = child.parse().expect("reaped child");
    assert!(child > 0, "the guest reaps the hook's child");
    assert_eq!(
        fields,
        format!("-1 {} 1 1 0 1", libc::EINTR),
        "guest sees EINTR, one handler run, and SIGSYS blocked and not pending"
    );
    assert_eq!(
        *log.signals.lock().unwrap(),
        vec![libc::SIGSYS],
        "SIGSYS is reported once"
    );
    child
}

/// Like `InjectInFirstSignalHook`, but the first signal hook injects `vfork`.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Default)]
struct VforkInFirstSignalHook;

#[cfg(target_arch = "x86_64")]
#[reverie::tool]
impl Tool for VforkInFirstSignalHook {
    type GlobalState = Log;
    /// Signal hooks run on this thread so far.
    type ThreadState = u64;

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
        replace_marker(guest, syscall).await
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        guest.send_rpc(Report::Signal(signal as i32)).await;
        *guest.thread_state_mut() += 1;
        if *guest.thread_state_mut() == 1 {
            let result = guest.inject(Vfork::new()).await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
        }
        Ok(Some(signal))
    }
}

/// Makes the syscall `nr(a0, a1, a2)` and returns its raw result. A child that
/// a signal hook's `vfork` starts right after the `syscall` instruction shares
/// the stack of its parent, which stays suspended until the child exits; it
/// exits at once, writing no memory.
///
/// # Safety
/// Makes an arbitrary syscall.
#[cfg(target_arch = "x86_64")]
unsafe fn syscall_exiting_a_vfork_child(nr: libc::c_long, a0: i64, a1: i64, a2: i64) -> i64 {
    let pid = unsafe { libc::getpid() } as i64;
    let ret: i64;
    unsafe {
        std::arch::asm!(
            "syscall",
            "mov r12, rax",
            "mov eax, {getpid}",
            "syscall",
            "cmp rax, r13",
            "je 2f",
            "mov eax, {exit}",
            "xor edi, edi",
            "syscall",
            "2:",
            getpid = const libc::SYS_getpid,
            exit = const libc::SYS_exit,
            inout("rax") nr => _,
            inout("rdi") a0 => _,
            in("rsi") a1,
            in("rdx") a2,
            in("r13") pid,
            out("r12") ret,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    ret
}

/// Checks a vfork-hook guest's line, `{ret} {handler runs} {reaped child}
/// {status}`, and the injections after `before`, and that the hook saw
/// SIGUSR1 once.
#[cfg(target_arch = "x86_64")]
fn check_vfork_hook_outcome(
    output: &reverie::process::Output,
    log: &Log,
    probe: &str,
    ret: i64,
    before: &[Result<i64, i32>],
) {
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
        "PROBE {probe} guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *injected,
        *signals
    );
    assert_eq!(injected.len(), before.len() + 1, "{:?}", *injected);
    assert_eq!(injected[..before.len()], *before);
    let Ok(child) = injected[before.len()] else {
        panic!("vfork failed: {:?}", injected[before.len()]);
    };
    let fields: Vec<&str> = stdout.trim().split(' ').collect();
    assert_eq!(
        fields,
        [&ret.to_string(), "1", &child.to_string(), "0"],
        "the guest keeps its result, runs the SIGUSR1 handler once, and reaps the hook's child"
    );
    assert_eq!(*signals, vec![libc::SIGUSR1], "the hook sees SIGUSR1 once");
}

/// A held SIGUSR1 (as in `signal_pending_before_injected_syscall_interrupts_it`)
/// is reported to a hook that injects `vfork`. The parent's step after the
/// child's exit ends at the vfork-done event stop, which drops a signal passed
/// on resume; the injection steps on to the step trap, so the final resume
/// still delivers SIGUSR1.
#[cfg(target_arch = "x86_64")]
#[test]
fn held_signal_is_delivered_after_a_signal_hook_vfork() {
    let (output, log) = test_fn::<VforkInFirstSignalHook, _>(|| unsafe {
        SIGUSR1_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGUSR1, count_sigusr1);
        // The child's exit SIGCHLD stays pending rather than stopping the
        // parent at a time that depends on the child.
        block(&[libc::SIGCHLD]);
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
        let ret = syscall_exiting_a_vfork_child(
            libc::SYS_write,
            UNBLOCK_THEN_GETPID_FD as i64,
            &set as *const libc::sigset_t as i64,
            0,
        );
        let calls = SIGUSR1_HANDLER_CALLS.load(Ordering::Relaxed);
        let mut status = 0;
        let child = libc::waitpid(-1, &mut status, 0);
        println!("{ret} {calls} {child} {status}");
    })
    .expect("run held-vfork guest");
    check_vfork_hook_outcome(
        &output,
        &log,
        "held-vfork",
        -(libc::EINTR as i64),
        &[Ok(0), Err(Errno::ERESTARTSYS.into_raw())],
    );
}

/// As `held_signal_is_delivered_after_a_signal_hook_vfork`, but SIGUSR1 is
/// reported at its own signal-delivery stop, after the guest's `tgkill`.
#[cfg(target_arch = "x86_64")]
#[test]
fn signal_is_delivered_after_a_signal_hook_vfork_at_its_delivery_stop() {
    let (output, log) = test_fn::<VforkInFirstSignalHook, _>(|| unsafe {
        SIGUSR1_HANDLER_CALLS.store(0, Ordering::Relaxed);
        install_counter(libc::SIGUSR1, count_sigusr1);
        block(&[libc::SIGCHLD]);
        let ret = syscall_exiting_a_vfork_child(
            libc::SYS_tgkill,
            libc::getpid() as i64,
            libc::syscall(libc::SYS_gettid),
            libc::SIGUSR1 as i64,
        );
        let calls = SIGUSR1_HANDLER_CALLS.load(Ordering::Relaxed);
        let mut status = 0;
        let child = libc::waitpid(-1, &mut status, 0);
        println!("{ret} {calls} {child} {status}");
    })
    .expect("run delivery-stop vfork guest");
    check_vfork_hook_outcome(&output, &log, "delivery-vfork", 0, &[]);
}

/// A signal hook executes another program. The replacement program's
/// `geteuid`, which the tool replaces with `getpid`, gets the PID: the
/// callback that injected the exec never resumed, and nothing of it, notably
/// that a signal callback was running (whose injections keep the guest's
/// return register), outlives it.
#[test]
fn exec_from_a_signal_hook_ends_the_callback() {
    let (output, log) = test_fn::<ExecInSignalHook, _>(|| unsafe {
        let path = c"/usr/bin/id";
        let argv = [c"id".as_ptr(), c"-u".as_ptr(), std::ptr::null()];
        let envp = [std::ptr::null()];
        let args = ExecArgs {
            path: path.as_ptr(),
            argv: argv.as_ptr(),
            envp: envp.as_ptr(),
        };
        libc::syscall(
            libc::SYS_write,
            EXEC_ARGS_FD,
            &args as *const ExecArgs,
            0usize,
        );
        use std::io::Write;
        println!("{}", libc::getpid());
        std::io::stdout().flush().unwrap();
        libc::kill(libc::getpid(), libc::SIGUSR2);
        // Reached only if the hook did not execute the program.
        libc::_exit(3);
    })
    .expect("run exec-from-hook guest");
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!(
        "PROBE exec-hook guest={:?} injected={:?} signals={:?} stderr={}",
        stdout.trim(),
        *log.injected.lock().unwrap(),
        *log.signals.lock().unwrap(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 2, "the guest's PID, then id's output");
    assert_eq!(
        lines[1], lines[0],
        "id's geteuid, replaced with getpid, returns the PID"
    );
    assert_eq!(*log.signals.lock().unwrap(), vec![libc::SIGUSR2]);
    assert!(log.injected.lock().unwrap().is_empty(), "the exec succeeds");
}

/// How many callbacks `InjectInEverySignalHook` injects in, so that a guest
/// whose signal is never delivered still ends.
const HOOK_INJECTIONS: u64 = 100;

/// Like `ReplaceMarker`, but every signal hook on each thread, up to
/// `HOOK_INJECTIONS` of them, injects a `getpid`, reported, before passing
/// the signal through.
#[derive(Clone, Copy, Debug, Default)]
struct InjectInEverySignalHook;

#[reverie::tool]
impl Tool for InjectInEverySignalHook {
    type GlobalState = Log;
    /// Signal hooks run on this thread so far.
    type ThreadState = u64;

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
        replace_marker(guest, syscall).await
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        guest.send_rpc(Report::Signal(signal as i32)).await;
        *guest.thread_state_mut() += 1;
        if *guest.thread_state_mut() <= HOOK_INJECTIONS {
            let result = guest.inject(Getpid::new()).await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
        }
        Ok(Some(signal))
    }
}

/// The value `queue_value_to_self` sends.
const QUEUED_VALUE: usize = 77;

static RECORDED_CALLS: AtomicUsize = AtomicUsize::new(0);
static RECORDED_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static RECORDED_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static RECORDED_VALUE: AtomicUsize = AtomicUsize::new(0);

/// An `SA_SIGINFO` handler that counts its runs and records the last
/// siginfo's code, sender PID and value.
extern "C" fn record_siginfo(
    _signal: libc::c_int,
    info: *mut libc::siginfo_t,
    _context: *mut libc::c_void,
) {
    // SAFETY: the kernel passes a valid siginfo to an SA_SIGINFO handler.
    let (code, pid, value) = unsafe {
        (
            (*info).si_code,
            (*info).si_pid(),
            (*info).si_value().sival_ptr as usize,
        )
    };
    RECORDED_CODE.store(code, Ordering::Relaxed);
    RECORDED_PID.store(pid, Ordering::Relaxed);
    RECORDED_VALUE.store(value, Ordering::Relaxed);
    RECORDED_CALLS.fetch_add(1, Ordering::Relaxed);
}

/// Installs `record_siginfo` for `signal` without `SA_RESTART` and clears
/// what it recorded.
///
/// # Safety
/// Replaces the process-wide disposition of `signal`.
unsafe fn install_recorder(signal: libc::c_int) {
    RECORDED_CALLS.store(0, Ordering::Relaxed);
    RECORDED_CODE.store(0, Ordering::Relaxed);
    RECORDED_PID.store(0, Ordering::Relaxed);
    RECORDED_VALUE.store(0, Ordering::Relaxed);
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = record_siginfo as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(libc::sigaction(signal, &action, std::ptr::null_mut()), 0);
    }
}

/// Queues `signal` to the calling thread with `si_code`, this process as the
/// sender, and `QUEUED_VALUE`, as `sigqueue` does with `SI_QUEUE`.
///
/// # Safety
/// Sends a signal to the calling thread.
unsafe fn queue_value_to_self(signal: libc::c_int, si_code: libc::c_int) {
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        info.si_signo = signal;
        info.si_code = si_code;
        // `si_pid`, `si_uid` and `si_value` open the union after the three
        // ints.
        let fields = (&mut info as *mut libc::siginfo_t).cast::<u8>().add(16);
        fields.cast::<libc::pid_t>().write(libc::getpid());
        fields.add(4).cast::<libc::uid_t>().write(libc::getuid());
        fields.add(8).cast::<usize>().write(QUEUED_VALUE);
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

/// Prints a syscall's result and errno, what `record_siginfo` recorded
/// (runs, code, sender PID and value), whether `signal` is blocked and
/// pending, the runs after unblocking it, and the PID.
///
/// # Safety
/// Changes the calling thread's signal mask.
unsafe fn print_recorded(ret: libc::c_long, errno: libc::c_int, signal: libc::c_int) {
    unsafe {
        let calls = RECORDED_CALLS.load(Ordering::Relaxed);
        let blocked = is_blocked(signal);
        let pending = is_pending(signal);
        unblock(signal);
        println!(
            "{ret} {errno} {calls} {} {} {} {blocked} {pending} {} {}",
            RECORDED_CODE.load(Ordering::Relaxed),
            RECORDED_PID.load(Ordering::Relaxed),
            RECORDED_VALUE.load(Ordering::Relaxed),
            RECORDED_CALLS.load(Ordering::Relaxed),
            libc::getpid()
        );
    }
}

/// Checks the guest's `print_recorded` line: `ret`, `errno` and the handler
/// running once, with the queued siginfo (`code`, the guest's PID and
/// `QUEUED_VALUE`), the signal blocked again and not pending, and no second
/// run. Returns the guest's PID.
fn check_recorded(
    output: &reverie::process::Output,
    probe: &str,
    log: &Log,
    ret_errno: &str,
    code: i32,
) -> i64 {
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!(
        "PROBE {probe} guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *log.injected.lock().unwrap(),
        *log.signals.lock().unwrap()
    );
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (fields, pid) = stdout.trim().rsplit_once(' ').expect("guest pid");
    let pid: i64 = pid.parse().expect("guest pid");
    assert_eq!(
        fields,
        format!("{ret_errno} 1 {code} {pid} {QUEUED_VALUE} 1 0 1"),
        "one handler run, with the queued code, sender and value; blocked again and not pending"
    );
    pid
}

/// The guest's own `ppoll`, with a 5-second timeout, whose temporary mask
/// lets a pending `signal` through, and a hook that injects at every
/// callback. Each injection's step lets the kernel restore `ppoll`'s saved
/// mask, which blocks `signal`. Passing it through from there would requeue
/// it, and the restarted `ppoll` would take it again, at every callback.
/// Reverie instead delivers it under the temporary mask. Untraced Linux
/// prints EINTR at once, one handler run with the queued siginfo, and the
/// signal blocked again. A signal that is lost instead ends `ppoll` with 0
/// after its timeout.
///
/// `code` is the queued `si_code`: a positive one is synchronous-class, which
/// the kernel dequeues ahead of a step's SIGTRAP.
fn always_injecting_hook_delivers_at_a_delivery_stop(signal: libc::c_int, code: libc::c_int) {
    let (output, log) = test_fn::<InjectInEverySignalHook, _>(move || unsafe {
        install_recorder(signal);
        block(&[signal]);
        queue_value_to_self(signal, code);
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        let timeout = libc::timespec {
            tv_sec: 5,
            tv_nsec: 0,
        };
        let ret = libc::syscall(
            libc::SYS_ppoll,
            0usize,
            0usize,
            &timeout as *const libc::timespec,
            &mask as *const libc::sigset_t,
            8usize,
        );
        let errno = *libc::__errno_location();
        print_recorded(ret, errno, signal);
    })
    .expect("run always-injecting hook guest");
    let pid = check_recorded(
        &output,
        "always-inject-ordinary",
        &log,
        &format!("-1 {}", libc::EINTR),
        code,
    );
    assert_eq!(
        *log.injected.lock().unwrap(),
        vec![Ok(pid)],
        "the hook's getpid runs once"
    );
    assert_eq!(
        *log.signals.lock().unwrap(),
        vec![signal],
        "the signal is reported once"
    );
}

#[test]
fn always_injecting_hook_delivers_a_signal_at_its_delivery_stop() {
    always_injecting_hook_delivers_at_a_delivery_stop(libc::SIGUSR1, libc::SI_QUEUE);
}

#[test]
fn always_injecting_hook_delivers_a_synchronous_signal_at_its_delivery_stop() {
    always_injecting_hook_delivers_at_a_delivery_stop(libc::SIGSYS, 1);
}

/// Loads a seccomp filter that answers `ppoll` with `action` and allows
/// everything else.
///
/// # Safety
/// Restricts the calling thread's syscalls for the rest of its life.
unsafe fn filter_ppoll(action: u32) {
    unsafe {
        let statement = |code: u32, k: u32, jt: u8, jf: u8| libc::sock_filter {
            code: code as u16,
            jt,
            jf,
            k,
        };
        let filter = [
            // seccomp_data.nr is at offset 0.
            statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0, 0),
            statement(
                libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
                libc::SYS_ppoll as u32,
                0,
                1,
            ),
            statement(libc::BPF_RET | libc::BPF_K, action, 0, 0),
            statement(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_ALLOW, 0, 0),
        ];
        let program = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_ptr() as *mut libc::sock_filter,
        };
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        assert_eq!(
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &program as *const libc::sock_fprog
            ),
            0
        );
    }
}

/// As `always_injecting_hook_delivers_at_a_delivery_stop`, but the guest's
/// mask-swapping syscall is `pselect6`, and a seccomp filter the guest
/// installed answers `ppoll`, which neither the guest nor the hook makes,
/// with `action`. Delivering the signal must not make a syscall the guest
/// did not: untraced Linux prints EINTR, one handler run with the queued
/// siginfo, and the signal blocked again.
fn always_injecting_hook_delivers_under_a_guest_seccomp_filter(action: u32) {
    let (output, log) = test_fn::<InjectInEverySignalHook, _>(move || unsafe {
        install_recorder(libc::SIGUSR1);
        block(&[libc::SIGUSR1]);
        queue_value_to_self(libc::SIGUSR1, libc::SI_QUEUE);
        filter_ppoll(action);
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        let sigmask: [usize; 2] = [&mask as *const libc::sigset_t as usize, 8];
        let timeout = libc::timespec {
            tv_sec: 5,
            tv_nsec: 0,
        };
        let ret = libc::syscall(
            libc::SYS_pselect6,
            0usize,
            0usize,
            0usize,
            0usize,
            &timeout as *const libc::timespec,
            sigmask.as_ptr(),
        );
        let errno = *libc::__errno_location();
        print_recorded(ret, errno, libc::SIGUSR1);
    })
    .expect("run seccomp-filtered always-injecting hook guest");
    let pid = check_recorded(
        &output,
        "always-inject-seccomp",
        &log,
        &format!("-1 {}", libc::EINTR),
        libc::SI_QUEUE,
    );
    assert_eq!(
        *log.injected.lock().unwrap(),
        vec![Ok(pid)],
        "the hook's getpid runs once"
    );
    assert_eq!(
        *log.signals.lock().unwrap(),
        vec![libc::SIGUSR1],
        "the signal is reported once"
    );
}

#[test]
fn always_injecting_hook_delivers_when_the_guest_kills_on_ppoll() {
    always_injecting_hook_delivers_under_a_guest_seccomp_filter(libc::SECCOMP_RET_KILL_PROCESS);
}

#[test]
fn always_injecting_hook_delivers_when_the_guest_fails_ppoll() {
    always_injecting_hook_delivers_under_a_guest_seccomp_filter(
        libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
    );
}

#[test]
fn always_injecting_hook_delivers_when_the_guest_traps_ppoll() {
    always_injecting_hook_delivers_under_a_guest_seccomp_filter(libc::SECCOMP_RET_TRAP);
}

/// As `always_injecting_hook_delivers_at_a_delivery_stop`, but the guest
/// ignores SIGUSR1 (`SIG_IGN`) and `ppoll` times out after 100 ms. Linux
/// discards the signal when `ppoll` takes it, restarts `ppoll`, and it
/// returns 0 with the signal blocked again and not pending, and no handler.
#[test]
fn always_injecting_hook_discards_an_ignored_signal() {
    let (output, log) = test_fn::<InjectInEverySignalHook, _>(|| unsafe {
        libc::signal(libc::SIGUSR1, libc::SIG_IGN);
        block(&[libc::SIGUSR1]);
        queue_value_to_self(libc::SIGUSR1, libc::SI_QUEUE);
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        *libc::__errno_location() = 0;
        let ret = libc::syscall(
            libc::SYS_ppoll,
            0usize,
            0usize,
            &timeout as *const libc::timespec,
            &mask as *const libc::sigset_t,
            8usize,
        );
        let errno = *libc::__errno_location();
        println!(
            "{ret} {errno} {} {} {}",
            is_blocked(libc::SIGUSR1),
            is_pending(libc::SIGUSR1),
            libc::getpid()
        );
    })
    .expect("run ignored-signal always-injecting hook guest");
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!(
        "PROBE always-inject-ignored guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *log.injected.lock().unwrap(),
        *log.signals.lock().unwrap()
    );
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (fields, pid) = stdout.trim().rsplit_once(' ').expect("guest pid");
    let pid: i64 = pid.parse().expect("guest pid");
    assert_eq!(
        fields, "0 0 1 0",
        "ppoll times out; the signal is blocked again and not pending"
    );
    assert_eq!(
        *log.injected.lock().unwrap(),
        vec![Ok(pid)],
        "the hook's getpid runs once"
    );
    assert_eq!(
        *log.signals.lock().unwrap(),
        vec![libc::SIGUSR1],
        "the signal is reported once"
    );
}

/// The set `BlockInFirstSignalHook` blocks. A static, so the forked guest has
/// it at the same address.
static SIGUSR1_SET: u64 = 1 << (libc::SIGUSR1 - 1);

/// Like `ReplaceMarker`, but the first signal hook on each thread injects
/// `rt_sigprocmask(SIG_BLOCK, {SIGUSR1})`, reported, before passing the
/// signal through.
#[derive(Clone, Copy, Debug, Default)]
struct BlockInFirstSignalHook;

#[reverie::tool]
impl Tool for BlockInFirstSignalHook {
    type GlobalState = Log;
    /// Whether a signal hook has run on this thread.
    type ThreadState = bool;

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
        replace_marker(guest, syscall).await
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        guest.send_rpc(Report::Signal(signal as i32)).await;
        if !std::mem::replace(guest.thread_state_mut(), true) {
            let set = Addr::from_raw(&SIGUSR1_SET as *const u64 as usize);
            let result = guest
                .inject(
                    RtSigprocmask::new()
                        .with_how(libc::SIG_BLOCK)
                        .with_set(set)
                        .with_oldset(None)
                        .with_sigsetsize(8),
                )
                .await;
            guest
                .send_rpc(Report::Injected(result.map_err(Errno::into_raw)))
                .await;
        }
        Ok(Some(signal))
    }
}

/// A SIGUSR1 queued unblocked is delivered as `rt_tgsigqueueinfo` returns,
/// outside any mask-swapping syscall. The hook blocks it with an injected
/// `rt_sigprocmask` before passing it through, so Linux requeues it: the
/// handler does not run until the guest unblocks it, and then runs with the
/// queued siginfo. Only a mask-swapping syscall's restore makes Reverie
/// deliver a signal its injections left blocked.
#[test]
fn signal_hook_blocking_its_signal_keeps_it_pending() {
    let (output, log) = test_fn::<BlockInFirstSignalHook, _>(|| unsafe {
        install_recorder(libc::SIGUSR1);
        queue_value_to_self(libc::SIGUSR1, libc::SI_QUEUE);
        print_recorded(0, 0, libc::SIGUSR1);
    })
    .expect("run hook-blocking guest");
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!(
        "PROBE hook-blocks guest={} injected={:?} signals={:?}",
        stdout.trim(),
        *log.injected.lock().unwrap(),
        *log.signals.lock().unwrap()
    );
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (fields, pid) = stdout.trim().rsplit_once(' ').expect("guest pid");
    let pid: i64 = pid.parse().expect("guest pid");
    assert_eq!(
        fields,
        format!("0 0 0 {} {pid} {QUEUED_VALUE} 1 1 1", libc::SI_QUEUE),
        "no handler run while blocked and pending; one run with the queued siginfo once unblocked"
    );
    assert_eq!(
        *log.injected.lock().unwrap(),
        vec![Ok(0)],
        "the hook's rt_sigprocmask runs once"
    );
    assert_eq!(
        *log.signals.lock().unwrap(),
        vec![libc::SIGUSR1, libc::SIGUSR1],
        "the signal is reported when delivered, and again once unblocked"
    );
}

/// As `always_injecting_hook_delivers_at_a_delivery_stop`, but `signal` is
/// held by the `PPOLL_FD` marker's injected `ppoll`, and the guest's result
/// is that `ppoll`'s. Untraced Linux prints EINTR, one handler run with the
/// queued siginfo, and the signal blocked again.
fn always_injecting_hook_delivers_after_a_held_signal(signal: libc::c_int, code: libc::c_int) {
    let (output, log) = test_fn::<InjectInEverySignalHook, _>(move || unsafe {
        install_recorder(signal);
        block(&[signal]);
        queue_value_to_self(signal, code);
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
        print_recorded(ret, errno, signal);
    })
    .expect("run always-injecting hook guest");
    let pid = check_recorded(
        &output,
        "always-inject-held",
        &log,
        &format!("-1 {}", libc::EINTR),
        code,
    );
    assert_eq!(
        *log.injected.lock().unwrap(),
        vec![Err(Errno::ERESTARTNOHAND.into_raw()), Ok(pid)],
        "ppoll is interrupted and the hook's getpid runs once"
    );
    assert_eq!(
        *log.signals.lock().unwrap(),
        vec![signal],
        "the signal is reported once"
    );
}

#[test]
fn always_injecting_hook_delivers_a_held_signal() {
    always_injecting_hook_delivers_after_a_held_signal(libc::SIGUSR1, libc::SI_QUEUE);
}

#[test]
fn always_injecting_hook_delivers_a_held_synchronous_signal() {
    always_injecting_hook_delivers_after_a_held_signal(libc::SIGSYS, 1);
}

/// A SIGUSR1 queued blocked with `SI_QUEUE`, the guest as its sender and
/// `QUEUED_VALUE`, is unblocked by the `UNBLOCK_THEN_GETPID_FD` marker's
/// injected `rt_sigprocmask` and stops the following `getpid` before its
/// `syscall`, so it is held. Nothing blocks it again, so nothing requeues it.
/// The hook injects `getpid` before passing it through, so the final resume
/// is from that injection's stop, not from SIGUSR1's: the handler must still
/// see the queued siginfo, not one Linux makes up for a signal resumed from
/// another signal's stop (`SI_USER`, the tracer's PID, no value).
#[test]
fn held_signal_keeps_its_siginfo_after_a_signal_hook_injection() {
    let (output, log) = test_fn::<InjectInFirstSignalHook, _>(|| unsafe {
        install_recorder(libc::SIGUSR1);
        let set = block(&[libc::SIGUSR1]);
        queue_value_to_self(libc::SIGUSR1, libc::SI_QUEUE);
        let ret = libc::syscall(
            libc::SYS_write,
            UNBLOCK_THEN_GETPID_FD,
            &set as *const libc::sigset_t,
            0usize,
        );
        let errno = *libc::__errno_location();
        // Blocked again for `print_recorded`'s check.
        block(&[libc::SIGUSR1]);
        print_recorded(ret, errno, libc::SIGUSR1);
    })
    .expect("run held-siginfo guest");
    let pid = check_recorded(
        &output,
        "held-siginfo",
        &log,
        &format!("-1 {}", libc::EINTR),
        libc::SI_QUEUE,
    );
    assert_eq!(
        *log.injected.lock().unwrap(),
        vec![Ok(0), Err(Errno::ERESTARTSYS.into_raw()), Ok(pid)],
        "the unblock completes, SIGUSR1 interrupts getpid before it runs, and the hook's getpid runs"
    );
    assert_eq!(*log.signals.lock().unwrap(), vec![libc::SIGUSR1]);
}

/// The ordinary route: the guest unblocks the SIGUSR1 of
/// `held_signal_keeps_its_siginfo_after_a_signal_hook_injection` itself,
/// with an `rt_sigprocmask` the tool does not intercept, and it is delivered
/// as that syscall returns. The hook's `getpid` must not cost it its siginfo.
#[test]
fn signal_keeps_its_siginfo_after_a_signal_hook_injection() {
    let (output, log) = test_fn::<InjectInFirstSignalHook, _>(|| unsafe {
        install_recorder(libc::SIGUSR1);
        block(&[libc::SIGUSR1]);
        queue_value_to_self(libc::SIGUSR1, libc::SI_QUEUE);
        unblock(libc::SIGUSR1);
        block(&[libc::SIGUSR1]);
        print_recorded(0, 0, libc::SIGUSR1);
    })
    .expect("run siginfo guest");
    let pid = check_recorded(&output, "ordinary-siginfo", &log, "0 0", libc::SI_QUEUE);
    assert_eq!(
        *log.injected.lock().unwrap(),
        vec![Ok(pid)],
        "the hook's getpid runs"
    );
    assert_eq!(*log.signals.lock().unwrap(), vec![libc::SIGUSR1]);
}
