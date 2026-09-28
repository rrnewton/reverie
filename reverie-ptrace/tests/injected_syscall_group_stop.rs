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
use reverie::syscalls::AddrMut;
use reverie::syscalls::Errno;
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
}
