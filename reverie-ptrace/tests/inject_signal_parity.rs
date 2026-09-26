/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! What a Tool's non-tail injects return when a signal is raised against the
//! tracee inside the Tool's syscall callback, and when the tracee dies there.
//!
//! These are the reference answers for the in-kernel (Narf) backend. They are
//! measured, not predicted, and they record how reverie-ptrace reports a
//! catchable signal: the tracer sees the signal-delivery stop only while it
//! waits out an inject, and the inject in which it sees that stop returns
//! `-ERESTARTSYS` in place of its own result (`task.rs`, the
//! `sig != Signal::SIGTRAP` arm of the injected-syscall wait). The signal is
//! kept as the task's pending signal and delivered after the callback
//! returns; later injects of the same callback run normally.
//!
//! * `kill(self, SIGTERM)` itself returns 0. The stop is seen during the next
//!   inject (`poll`), which returns `-ERESTARTSYS`; the inject after that
//!   (`getpid`) returns the pid.
//! * A signal arriving while a blocking inject (`pause`) sleeps ends that
//!   syscall with its raw restart code (`-ERESTARTNOHAND`, -514): the syscall
//!   exit is seen before the delivery stop. The next inject (`getpid`)
//!   returns `-ERESTARTSYS`, and the one after that returns the pid.
//! * A `SIGKILL` ends the tracee inside the inject that raised it: that
//!   inject never returns to the Tool, and nothing after it runs.

use std::sync::Mutex;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Signal;
use reverie::Tool;
use reverie::syscalls;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_ptrace::testing::test_fn;

/// Which callback the Tool runs (see [`Parity::handle_syscall_event`]).
static CASE: AtomicU32 = AtomicU32::new(0);
const CASE_KILL_SIGTERM: u32 = 1;
const CASE_ALARM_DURING_PAUSE: u32 = 2;
const CASE_KILL_SIGKILL: u32 = 3;

/// Marks a step the Tool reached before issuing it.
const REACHED: i64 = i64::MIN;

#[derive(Default)]
struct Steps {
    steps: Mutex<Vec<(u32, i64)>>,
}

#[reverie::global_tool]
impl GlobalTool for Steps {
    type Request = (u32, i64);
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, step: (u32, i64)) {
        self.steps.lock().unwrap().push(step);
    }
}

fn value(result: Result<i64, reverie::Errno>) -> i64 {
    match result {
        Ok(value) => value,
        Err(errno) => -(errno.into_raw() as i64),
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Parity;

#[reverie::tool]
impl Tool for Parity {
    type GlobalState = Steps;
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() != Sysno::getppid {
            guest.tail_inject(syscall).await
        }
        let pid = guest.pid().as_raw();
        guest.send_rpc((0, i64::from(pid))).await;
        match CASE.load(Ordering::SeqCst) {
            CASE_KILL_SIGTERM => {
                let kill = syscalls::Kill::new()
                    .with_pid(pid)
                    .with_sig(Signal::SIGTERM as i32);
                let r = value(guest.inject(kill).await);
                guest.send_rpc((1, r)).await;
                let poll = syscalls::Poll::new()
                    .with_fds(None)
                    .with_nfds(0)
                    .with_timeout(200);
                let r = value(guest.inject(poll).await);
                guest.send_rpc((2, r)).await;
                let r = value(guest.inject(syscalls::Getpid::new()).await);
                guest.send_rpc((3, r)).await;
            }
            CASE_ALARM_DURING_PAUSE => {
                let r = value(guest.inject(syscalls::Alarm::new().with_seconds(1)).await);
                guest.send_rpc((1, r)).await;
                let r = value(guest.inject(syscalls::Pause::new()).await);
                guest.send_rpc((2, r)).await;
                let r = value(guest.inject(syscalls::Getpid::new()).await);
                guest.send_rpc((3, r)).await;
                let r = value(guest.inject(syscalls::Getpid::new()).await);
                guest.send_rpc((4, r)).await;
            }
            CASE_KILL_SIGKILL => {
                guest.send_rpc((1, REACHED)).await;
                let kill = syscalls::Kill::new()
                    .with_pid(pid)
                    .with_sig(Signal::SIGKILL as i32);
                let r = value(guest.inject(kill).await);
                guest.send_rpc((2, r)).await;
                let r = value(guest.inject(syscalls::Getpid::new()).await);
                guest.send_rpc((3, r)).await;
            }
            _ => {}
        }
        guest.tail_inject(syscall).await
    }
}

fn run(case: u32) -> (ExitStatus, Vec<(u32, i64)>, i64) {
    CASE.store(case, Ordering::SeqCst);
    let (output, steps) = test_fn::<Parity, _>(|| unsafe {
        libc::syscall(libc::SYS_getppid);
        libc::syscall(libc::SYS_exit_group, 0);
    })
    .expect("run the parity guest");
    let steps = steps.steps.into_inner().unwrap();
    // Step 0 carries the guest's pid, as the Tool saw it.
    let pid = steps.iter().find(|s| s.0 == 0).map_or(0, |s| s.1);
    (output.status, steps, pid)
}

#[test]
fn inject_kill_self_sigterm_is_reported_at_the_next_inject_and_later_injects_run() {
    let (status, steps, pid) = run(CASE_KILL_SIGTERM);
    eprintln!("kill(self, SIGTERM): status {status:?} steps {steps:?}");
    assert!(pid > 0);
    assert_eq!(steps, vec![(0, pid), (1, 0), (2, -512), (3, pid)]);
    assert_eq!(status, ExitStatus::Signaled(Signal::SIGTERM, false));
}

#[test]
fn signal_during_a_blocking_inject_is_reported_at_the_next_inject_and_later_injects_run() {
    let (status, steps, pid) = run(CASE_ALARM_DURING_PAUSE);
    eprintln!("alarm during pause: status {status:?} steps {steps:?}");
    assert!(pid > 0);
    assert_eq!(
        steps,
        vec![(0, pid), (1, 0), (2, -514), (3, -512), (4, pid)]
    );
    assert_eq!(status, ExitStatus::Signaled(Signal::SIGALRM, false));
}

#[test]
fn inject_kill_self_sigkill_never_returns() {
    let (status, steps, pid) = run(CASE_KILL_SIGKILL);
    eprintln!("kill(self, SIGKILL): status {status:?} steps {steps:?}");
    assert!(pid > 0);
    assert_eq!(steps, vec![(0, pid), (1, REACHED)]);
    assert_eq!(status, ExitStatus::Signaled(Signal::SIGKILL, false));
}
