/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
*/

//! Exact native observations precede cancellable Tool continuation.
//! These tests create native ptrace tracees; they are not pure unit controls.

#![cfg(target_arch = "x86_64")]

use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::InjectedSyscallEvent;
use reverie::Pid;
use reverie::Signal;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Getpid;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use reverie_ptrace::testing::test_fn_with_config;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
enum Case {
    #[default]
    Original,
    Private,
    CancelBefore,
    CancelSuccess,
    CancelError,
    SurvivingChild,
    KilledAtChildEvent,
    BackendSetup,
    PrivateChild,
    SignalAtChildEvent,
    StopAtChildEvent,
    VforkCreatorDeath,
    ProcessParentRejection,
    ThreadParentRejection,
    LateParentRejection,
    Preparation,
    PreparationCancelBefore,
}

impl Case {
    fn rejects_parent(self) -> bool {
        matches!(
            self,
            Self::ProcessParentRejection | Self::ThreadParentRejection | Self::LateParentRejection
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum Request {
    Observed {
        tid: i32,
        nr: i64,
        raw: i64,
        child: bool,
    },
    ChildReturned {
        tid: i32,
        child: i32,
        raw: i64,
    },
    ContinueAfterChildReturn {
        parent: i32,
        child: i32,
    },
    WaitTerminal(i32),
    ToolExit(i32),
}

#[derive(Clone, Copy, Debug)]
struct Observation {
    tid: i32,
    nr: Sysno,
    args: SyscallArgs,
    event: InjectedSyscallEvent,
}

#[derive(Default)]
struct Log {
    observations: Mutex<Vec<Observation>>,
    terminal: Mutex<Vec<(i32, ExitStatus)>>,
    tool_exit: Mutex<Vec<i32>>,
    changed: Notify,
    killed: AtomicBool,
    signal_sent: AtomicBool,
    stop_sent: AtomicBool,
    continued: AtomicBool,
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Request = Request;
    type Response = bool;
    type Config = Case;

    async fn receive_rpc(&self, from: Pid, request: Request) -> bool {
        match request {
            Request::Observed {
                tid,
                nr,
                raw,
                child,
            } => {
                assert_eq!(tid, from.as_raw());
                self.observations.lock().unwrap().iter().any(|entry| {
                    entry.tid == tid
                        && entry.nr as i64 == nr
                        && if child {
                            entry.event
                                == InjectedSyscallEvent::ChildCreated(Pid::from_raw(raw as i32))
                        } else {
                            entry.event == InjectedSyscallEvent::Returned(raw)
                        }
                })
            }
            Request::ChildReturned { tid, child, raw } => {
                assert_eq!(tid, from.as_raw());
                self.observations.lock().unwrap().iter().any(|entry| {
                    entry.tid == tid
                        && entry.event
                            == InjectedSyscallEvent::ChildSyscallReturned {
                                child: Pid::from_raw(child),
                                raw,
                            }
                })
            }
            Request::ContinueAfterChildReturn { parent, child } => {
                assert_eq!(from.as_raw(), child);
                loop {
                    let changed = self.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if self.observations.lock().unwrap().iter().any(|entry| {
                        entry.tid == parent
                            && entry.event
                                == InjectedSyscallEvent::ChildSyscallReturned {
                                    child: Pid::from_raw(child),
                                    raw: i64::from(child),
                                }
                    }) {
                        break;
                    }
                    changed.await;
                }
                assert!(self.stop_sent.load(Ordering::SeqCst));
                assert!(!self.continued.swap(true, Ordering::SeqCst));
                assert_eq!(unsafe { libc::kill(parent, libc::SIGCONT) }, 0);
                true
            }
            Request::WaitTerminal(parent) => loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self
                    .terminal
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(tid, _)| *tid == parent)
                {
                    break true;
                }
                changed.await;
            },
            Request::ToolExit(tid) => {
                assert_eq!(tid, from.as_raw());
                assert!(
                    self.terminal
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|(owner, _)| *owner == tid)
                );
                self.tool_exit.lock().unwrap().push(tid);
                true
            }
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Thread {
    parent: Option<i32>,
    child_seen: Option<i32>,
    terminal: bool,
    prepared: usize,
    entered: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct Observer {
    case: Case,
}

#[reverie::tool]
impl Tool for Observer {
    type GlobalState = Log;
    type ThreadState = Thread;

    fn new(_pid: Pid, case: &Case) -> Self {
        Self { case: *case }
    }

    fn subscriptions(case: &Case) -> Subscription {
        let mut subscriptions = Subscription::none();
        match case {
            Case::Original => {
                subscriptions.syscalls([Sysno::getpid, Sysno::setpgid]);
            }
            Case::Private | Case::PrivateChild => {
                subscriptions.syscall(Sysno::getuid);
            }
            Case::BackendSetup => {
                // Exercise internal vDSO/CPUID setup while retaining an actual
                // Tool mprotect with the same syscall number as setup uses.
                subscriptions.syscalls([Sysno::mprotect, Sysno::clock_gettime]);
                subscriptions.cpuid();
            }
            Case::CancelBefore
            | Case::CancelSuccess
            | Case::Preparation
            | Case::PreparationCancelBefore => {
                subscriptions.syscall(Sysno::getpid);
            }
            Case::CancelError => {
                subscriptions.syscall(Sysno::setpgid);
            }
            Case::SurvivingChild
            | Case::KilledAtChildEvent
            | Case::SignalAtChildEvent
            | Case::StopAtChildEvent
            | Case::ProcessParentRejection
            | Case::LateParentRejection => {
                subscriptions.syscalls([Sysno::getpid, Sysno::fork]);
            }
            Case::ThreadParentRejection => {
                subscriptions.syscall(Sysno::clone);
            }
            Case::VforkCreatorDeath => {
                subscriptions.syscall(Sysno::vfork);
            }
        }
        subscriptions
    }

    fn observe_injected_syscalls(_case: &Case) -> bool {
        true
    }

    fn observe_injected_syscall_preparation(case: &Case) -> bool {
        matches!(case, Case::Preparation | Case::PreparationCancelBefore)
    }

    fn init_thread_state(&self, child: Pid, parent: Option<(Pid, &Thread)>) -> Thread {
        if let Some((_, state)) = parent {
            assert_eq!(
                state.child_seen,
                Some(child.as_raw()),
                "child event must precede actual Tool state inheritance"
            );
        }
        Thread {
            parent: parent.map(|(tid, _)| tid.as_raw()),
            ..Thread::default()
        }
    }

    fn on_injected_syscall_observed(
        &self,
        tid: Pid,
        global: &Log,
        state: &mut Thread,
        nr: Sysno,
        args: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        assert!(!state.terminal);
        if matches!(self.case, Case::Preparation | Case::PreparationCancelBefore) {
            assert_eq!(nr, Sysno::getpid);
            match event {
                InjectedSyscallEvent::Prepared => {
                    assert_eq!(state.prepared, 0);
                    state.prepared += 1;
                }
                InjectedSyscallEvent::Entered => {
                    assert_eq!(state.prepared, 1);
                    assert_eq!(state.entered, 0);
                    state.entered += 1;
                }
                InjectedSyscallEvent::Returned(raw) => {
                    assert_eq!(state.entered, 1);
                    assert_eq!(
                        state.prepared, 1,
                        "actual result must follow preparation in the same owned state"
                    );
                    assert_eq!(raw, i64::from(tid.as_raw()));
                }
                other => panic!("unexpected preparation-control event {other:?}"),
            }
        }
        if let InjectedSyscallEvent::ChildCreated(child) = event {
            state.child_seen = Some(child.as_raw());
        }
        global.observations.lock().unwrap().push(Observation {
            tid: tid.as_raw(),
            nr,
            args,
            event,
        });
        global.changed.notify_waiters();
        if self.case == Case::SignalAtChildEvent
            && matches!(event, InjectedSyscallEvent::ChildCreated(_))
        {
            assert!(!global.signal_sent.swap(true, Ordering::SeqCst));
            // Exact current creator is still held at the original child event.
            assert_eq!(unsafe { libc::kill(tid.as_raw(), libc::SIGUSR1) }, 0);
        }
        if self.case == Case::StopAtChildEvent
            && matches!(event, InjectedSyscallEvent::ChildCreated(_))
        {
            assert!(!global.stop_sent.swap(true, Ordering::SeqCst));
            assert_eq!(unsafe { libc::kill(tid.as_raw(), libc::SIGSTOP) }, 0);
        }
        if self.case.rejects_parent() {
            let mut evidence = REJECTION_EVIDENCE.lock().unwrap();
            let evidence = evidence.as_mut().expect("owned rejection fixture");
            evidence.observed.push((tid.as_raw(), event));
            if let InjectedSyscallEvent::ChildCreated(child) = event {
                assert_eq!(
                    nr,
                    if self.case == Case::ThreadParentRejection {
                        Sysno::clone
                    } else {
                        Sysno::fork
                    }
                );
                evidence.births.push((tid.as_raw(), child.as_raw()));
                for pid in [tid, child] {
                    if !evidence
                        .actors
                        .iter()
                        .any(|(saved, _)| *saved == pid.as_raw())
                    {
                        // Original NewChild stop still owns both identities;
                        // neither task has resumed or been reaped in this hook.
                        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.as_raw(), 128) }
                            as i32;
                        assert!(
                            fd >= 0,
                            "bind actual rejection actor: {}",
                            std::io::Error::last_os_error()
                        );
                        evidence
                            .actors
                            .push((pid.as_raw(), unsafe { OwnedFd::from_raw_fd(fd) }));
                    }
                }
                if (self.case != Case::LateParentRejection && evidence.births.len() == 2)
                    || (self.case == Case::LateParentRejection && state.parent.is_some())
                {
                    if let Some(root) = state.parent {
                        assert_eq!(self.case, Case::LateParentRejection);
                        assert_eq!(evidence.births.len(), 2);
                        assert!(evidence.process_consumed.contains(&root));
                        assert!(evidence.terminal.contains(&(root, ExitStatus::Exited(0))));
                    } else {
                        assert_eq!(evidence.births.len(), 2);
                        assert_eq!(evidence.pending_child, Some(evidence.births[0].1));
                    }
                    assert_eq!(evidence.rejected.replace(tid.as_raw()), None);
                    // The current ptracer owns this exact creator stop. Corrupt
                    // only the claimed syscall identity; do not fabricate a
                    // kernel wait, completion receipt, errno, or fatal signal.
                    let mut regs: libc::user_regs_struct = unsafe { std::mem::zeroed() };
                    assert_eq!(
                        unsafe {
                            libc::ptrace(libc::PTRACE_GETREGS, tid.as_raw(), 0usize, &mut regs)
                        },
                        0
                    );
                    assert_eq!(regs.orig_rax, nr as u64);
                    regs.orig_rax = libc::SYS_getpid as u64;
                    assert_eq!(
                        unsafe { libc::ptrace(libc::PTRACE_SETREGS, tid.as_raw(), 0usize, &regs) },
                        0
                    );
                }
            }
        }
        let cancel = matches!((self.case, nr, event),
            (Case::CancelSuccess, Sysno::getpid, InjectedSyscallEvent::Returned(raw)) if raw > 0)
            || matches!((self.case, nr, event),
                (Case::CancelError, Sysno::setpgid, InjectedSyscallEvent::Returned(raw)) if raw == -(libc::EINVAL as i64))
            || matches!(
                (self.case, nr, event),
                (
                    Case::KilledAtChildEvent,
                    Sysno::fork,
                    InjectedSyscallEvent::ChildCreated(_)
                )
            );
        if cancel {
            assert!(!global.killed.swap(true, Ordering::SeqCst));
            // This test's current backend-owned tracee is stopped in this hook,
            // has not been resumed or reaped, and has no other kill source.
            assert_eq!(unsafe { libc::kill(tid.as_raw(), libc::SIGKILL) }, 0);
        }
    }

    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        global: &Log,
        state: &mut Thread,
        status: ExitStatus,
    ) {
        assert!(
            !state.terminal,
            "forwarding a final wait must not notify twice"
        );
        state.terminal = true;
        if self.case.rejects_parent() {
            let mut evidence = REJECTION_EVIDENCE.lock().unwrap();
            evidence
                .as_mut()
                .unwrap()
                .terminal
                .push((tid.as_raw(), status));
        }
        let mut terminal = global.terminal.lock().unwrap();
        assert!(!terminal.iter().any(|(owner, _)| *owner == tid.as_raw()));
        terminal.push((tid.as_raw(), status));
        drop(terminal);
        global.changed.notify_waiters();
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let nr = match &syscall {
            Syscall::Getpid(_) => Sysno::getpid,
            Syscall::Getuid(_) => Sysno::getuid,
            Syscall::Setpgid(_) => Sysno::setpgid,
            Syscall::Mprotect(_) => Sysno::mprotect,
            Syscall::Clone(_) => Sysno::clone,
            Syscall::Fork(_) => Sysno::fork,
            Syscall::Vfork(_) => Sysno::vfork,
            _ => panic!("unexpected subscribed syscall"),
        };
        if matches!(
            self.case,
            Case::CancelBefore | Case::PreparationCancelBefore
        ) && nr == Sysno::getpid
        {
            // No Guest::inject has been called. This current tracee is stopped
            // at its subscribed entry, so cancellation must not invent a result.
            assert_eq!(
                unsafe { libc::kill(guest.tid().as_raw(), libc::SIGKILL) },
                0
            );
            return futures::future::pending().await;
        }
        if matches!(self.case, Case::SurvivingChild | Case::KilledAtChildEvent)
            && nr == Sysno::getpid
            && let Some(parent) = guest.thread_state().parent
        {
            assert!(guest.send_rpc(Request::WaitTerminal(parent)).await);
        }
        if self.case == Case::LateParentRejection
            && nr == Sysno::fork
            && let Some(root) = guest.thread_state().parent
        {
            loop {
                let notified = REJECTION_CHANGED.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if REJECTION_EVIDENCE
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .process_consumed
                    .contains(&root)
                {
                    break;
                }
                notified.await;
            }
        }
        if matches!(
            self.case,
            Case::ProcessParentRejection | Case::ThreadParentRejection
        ) && guest.thread_state().parent.is_none()
        {
            loop {
                let changed = REJECTION_CHANGED.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let ready = {
                    let evidence = REJECTION_EVIDENCE.lock().unwrap();
                    let evidence = evidence.as_ref().unwrap();
                    evidence.births.is_empty()
                        || evidence.pending_child == Some(evidence.births[0].1)
                };
                if ready {
                    break;
                }
                changed.await;
            }
        }
        if self.case == Case::StopAtChildEvent
            && nr == Sysno::getpid
            && let Some(parent) = guest.thread_state().parent
        {
            assert!(
                guest
                    .send_rpc(Request::ContinueAfterChildReturn {
                        parent,
                        child: guest.tid().as_raw(),
                    })
                    .await
            );
        }
        if self.case == Case::PrivateChild && nr == Sysno::getuid {
            // A different syscall is pending, so this real fork uses the same
            // private instruction/context path as arbitrary Tool injection.
            let raw = guest.inject(reverie::syscalls::Fork::new()).await?;
            assert!(raw > 0);
            assert!(
                guest
                    .send_rpc(Request::ChildReturned {
                        tid: guest.tid().as_raw(),
                        child: raw as i32,
                        raw
                    })
                    .await
            );
            return Ok(raw);
        }
        if self.case == Case::Private && nr == Sysno::getuid {
            // The pending syscall is getuid: this injection uses the private
            // syscall page rather than the original syscall-exit path.
            let raw = guest.inject(Getpid::default()).await?;
            assert!(
                guest
                    .send_rpc(Request::Observed {
                        tid: guest.tid().as_raw(),
                        nr: Sysno::getpid as i64,
                        raw,
                        child: false
                    })
                    .await
            );
        }
        let result = guest.inject(syscall).await;
        if matches!(
            (self.case, nr),
            (Case::CancelSuccess, Sysno::getpid) | (Case::CancelError, Sysno::setpgid)
        ) {
            // If the inject future returned before the fatal signal was
            // consumed, abandon the Tool continuation at this await instead.
            return futures::future::pending().await;
        }
        let raw = result.unwrap_or_else(|error| -(error.into_raw() as i64));
        assert!(
            guest
                .send_rpc(Request::Observed {
                    tid: guest.tid().as_raw(),
                    nr: nr as i64,
                    raw,
                    child: matches!(nr, Sysno::clone | Sysno::fork | Sysno::vfork) && raw > 0,
                })
                .await,
            "shared observation must precede the ordinary inject continuation"
        );
        if matches!(self.case, Case::SurvivingChild | Case::SignalAtChildEvent)
            && nr == Sysno::fork
            && raw > 0
        {
            assert!(
                guest
                    .send_rpc(Request::ChildReturned {
                        tid: guest.tid().as_raw(),
                        child: raw as i32,
                        raw
                    })
                    .await,
                "actual parent completion must precede Tool continuation"
            );
        }
        Ok(result?)
    }

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        if matches!(
            self.case,
            Case::ProcessParentRejection | Case::ThreadParentRejection
        ) && guest.thread_state().parent.is_some()
        {
            let first = {
                let mut evidence = REJECTION_EVIDENCE.lock().unwrap();
                let evidence = evidence.as_mut().unwrap();
                let first = evidence.births[0].1 == guest.tid().as_raw();
                if first {
                    assert_eq!(evidence.pending_child.replace(guest.tid().as_raw()), None);
                }
                first
            };
            if first {
                REJECTION_CHANGED.notify_waiters();
                // This is an actual borrowed Tool callback in the existing
                // actor. The common failure must cancel it and consume EXIT.
                return futures::future::pending().await;
            }
        }
        Ok(())
    }

    async fn on_exit_thread<G: GlobalRPC<Log>>(
        &self,
        tid: Pid,
        global: &G,
        state: Thread,
        _status: ExitStatus,
    ) -> Result<(), Error> {
        assert!(
            state.terminal,
            "this fixture requires an actual native final wait"
        );
        assert!(global.send_rpc(Request::ToolExit(tid.as_raw())).await);
        if self.case.rejects_parent() {
            REJECTION_EVIDENCE
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .consumed
                .push(tid.as_raw());
        }
        Ok(())
    }

    async fn on_exit_process<G: GlobalRPC<Log>>(
        self,
        pid: Pid,
        _global: &G,
        _status: ExitStatus,
    ) -> Result<(), Error> {
        if self.case.rejects_parent() {
            {
                let mut evidence = REJECTION_EVIDENCE.lock().unwrap();
                let evidence = evidence.as_mut().unwrap();
                assert!(!evidence.process_consumed.contains(&pid.as_raw()));
                evidence.process_consumed.push(pid.as_raw());
            }
            // Synchronous marker, then no await: the single LocalSet finishes
            // this root run before polling the newly awakened orphan actor.
            REJECTION_CHANGED.notify_waiters();
        }
        Ok(())
    }
}

fn assert_normal_terminal(log: &Log, expected: usize) {
    let terminal = log.terminal.lock().unwrap();
    assert_eq!(terminal.len(), expected);
    assert!(
        terminal
            .iter()
            .all(|(_, status)| *status == ExitStatus::Exited(0))
    );
    let mut terminal_ids: Vec<_> = terminal.iter().map(|(tid, _)| *tid).collect();
    let mut tool_ids = log.tool_exit.lock().unwrap().clone();
    terminal_ids.sort_unstable();
    tool_ids.sort_unstable();
    assert_eq!(terminal_ids, tool_ids);
}

#[test]
fn original_native_success_and_error_are_retained_before_continuation() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            assert!(libc::syscall(libc::SYS_getpid) > 0);
            assert_eq!(libc::syscall(libc::SYS_setpgid, -1i32, 0i32), -1);
            assert_eq!(*libc::__errno_location(), libc::EINVAL);
        },
        Case::Original,
        true,
    )
    .expect("ptrace original observations");
    assert_eq!(output.status, ExitStatus::Exited(0));
    let observed = log.observations.lock().unwrap();
    let errors: Vec<_> = observed
        .iter()
        .filter(|entry| entry.nr == Sysno::setpgid)
        .collect();
    assert_eq!(errors.len(), 2);
    assert_eq!(errors[0].event, InjectedSyscallEvent::Entered);
    assert_eq!(errors[0].tid, errors[1].tid);
    assert_eq!(errors[0].args, errors[1].args);
    assert_eq!(errors[0].args.arg0 as i32, -1);
    assert_eq!(errors[0].args.arg1, 0);
    assert_eq!(
        errors[1].event,
        InjectedSyscallEvent::Returned(-(libc::EINVAL as i64))
    );
    assert!(observed.iter().any(|entry| entry.nr == Sysno::getpid
        && entry.event == InjectedSyscallEvent::Returned(entry.tid as i64)));
    drop(observed);
    assert_normal_terminal(&log, 1);
}

#[test]
fn private_native_return_is_observed_before_tool_continuation() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            assert!(libc::syscall(libc::SYS_getuid) >= 0);
        },
        Case::Private,
        true,
    )
    .expect("ptrace private observation");
    assert_eq!(output.status, ExitStatus::Exited(0));
    let observed = log.observations.lock().unwrap();
    assert_eq!(
        observed
            .iter()
            .filter(|entry| entry.nr == Sysno::getpid)
            .count(),
        1
    );
    assert_eq!(
        observed
            .iter()
            .filter(|entry| entry.nr == Sysno::getuid)
            .count(),
        1
    );
    drop(observed);
    assert_normal_terminal(&log, 1);
}

fn canceled_continuation(case: Case) {
    let (output, log) = test_fn_with_config::<Observer, _>(
        move || unsafe {
            match case {
                Case::CancelSuccess => {
                    libc::syscall(libc::SYS_getpid);
                }
                Case::CancelError => {
                    libc::syscall(libc::SYS_setpgid, -1i32, 0i32);
                }
                _ => unreachable!(),
            }
            libc::_exit(99); // A resumed guest is a failure, not expected refusal.
        },
        case,
        true,
    )
    .expect("ptrace canceled continuation");
    assert!(matches!(
        output.status,
        ExitStatus::Signaled(Signal::SIGKILL, _)
    ));
    assert!(log.killed.load(Ordering::SeqCst));
    let observed = log.observations.lock().unwrap();
    assert_eq!(
        observed.len(),
        2,
        "unexpected native observations: {observed:?}"
    );
    assert_eq!(observed[0].event, InjectedSyscallEvent::Entered);
    assert_eq!(observed[0].tid, observed[1].tid);
    assert_eq!(observed[0].nr, observed[1].nr);
    assert_eq!(observed[0].args, observed[1].args);
    let entry = observed[1];
    assert_eq!(
        entry.event,
        InjectedSyscallEvent::Returned(if case == Case::CancelSuccess {
            entry.tid as i64
        } else {
            -(libc::EINVAL as i64)
        })
    );
    drop(observed);
    let terminal = log.terminal.lock().unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].1, output.status);
    assert_eq!(*log.tool_exit.lock().unwrap(), vec![terminal[0].0]);
}

#[test]
fn observed_success_survives_cancellation_before_tool_continuation() {
    canceled_continuation(Case::CancelSuccess);
}

#[test]
fn observed_error_survives_cancellation_before_tool_continuation() {
    canceled_continuation(Case::CancelError);
}

#[test]
fn native_child_event_precedes_inheritance_and_parent_final_wait_unblocks_survivor() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            let child = libc::syscall(libc::SYS_fork);
            assert!(child >= 0);
            if child == 0 {
                assert!(libc::syscall(libc::SYS_getpid) > 0);
            }
            libc::_exit(0);
        },
        Case::SurvivingChild,
        true,
    )
    .expect("ptrace surviving child");
    assert_eq!(output.status, ExitStatus::Exited(0));
    let observed = log.observations.lock().unwrap();
    let births: Vec<_> = observed
        .iter()
        .filter_map(|entry| match entry.event {
            InjectedSyscallEvent::ChildCreated(child) => Some((entry.tid, child.as_raw())),
            _ => None,
        })
        .collect();
    assert_eq!(births.len(), 1);
    assert_ne!(births[0].0, births[0].1);
    assert!(
        !observed.iter().any(|entry| entry.nr == Sysno::fork
            && matches!(entry.event, InjectedSyscallEvent::Returned(_))),
        "a NewChild event must retain its distinct native meaning"
    );
    let completions: Vec<_> = observed
        .iter()
        .filter_map(|entry| {
            if let InjectedSyscallEvent::ChildSyscallReturned { child, raw } = entry.event {
                Some((entry.tid, entry.nr, child.as_raw(), raw))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        completions,
        vec![(
            births[0].0,
            Sysno::fork,
            births[0].1,
            i64::from(births[0].1)
        )]
    );
    let birth_position = observed
        .iter()
        .position(|entry| matches!(entry.event, InjectedSyscallEvent::ChildCreated(_)))
        .unwrap();
    let completion_position = observed
        .iter()
        .position(|entry| {
            matches!(
                entry.event,
                InjectedSyscallEvent::ChildSyscallReturned { .. }
            )
        })
        .unwrap();
    assert!(
        birth_position < completion_position,
        "early child identity precedes actual parent return"
    );
    drop(observed);
    assert_normal_terminal(&log, 2);
}

#[test]
fn cancellation_before_injection_emits_terminal_but_no_native_result() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            libc::syscall(libc::SYS_getpid);
            libc::_exit(99);
        },
        Case::CancelBefore,
        true,
    )
    .expect("ptrace uninvoked cancellation");
    assert!(matches!(
        output.status,
        ExitStatus::Signaled(Signal::SIGKILL, _)
    ));
    assert!(log.observations.lock().unwrap().is_empty());
    assert!(
        !log.killed.load(Ordering::SeqCst),
        "no result observer performed the kill"
    );
    let terminal = log.terminal.lock().unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].1, output.status);
    assert_eq!(*log.tool_exit.lock().unwrap(), vec![terminal[0].0]);
}

#[test]
fn decoded_native_child_survives_parent_death_before_parent_restoration() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            let child = libc::syscall(libc::SYS_fork);
            if child == 0 {
                assert!(libc::syscall(libc::SYS_getpid) > 0);
                libc::_exit(0);
            }
            // The observation hook kills the actual stopped parent before
            // dispatch attempts its restoration. It must never resume here.
            libc::_exit(99);
        },
        Case::KilledAtChildEvent,
        true,
    )
    .expect("decoded native child must remain owned after creator death");
    assert!(matches!(
        output.status,
        ExitStatus::Signaled(Signal::SIGKILL, _)
    ));
    assert!(log.killed.load(Ordering::SeqCst));
    let observations = log.observations.lock().unwrap();
    let births: Vec<_> = observations
        .iter()
        .filter_map(|observation| {
            if let InjectedSyscallEvent::ChildCreated(child) = observation.event {
                Some((observation.tid, child.as_raw()))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(births.len(), 1);
    let (parent, child) = births[0];
    assert_ne!(parent, child);
    assert!(
        !observations.iter().any(|observation| {
            observation.tid == parent
                && matches!(
                    observation.event,
                    InjectedSyscallEvent::ChildSyscallReturned { .. }
                )
        }),
        "parent death at ChildCreated cannot fabricate a later syscall exit"
    );
    assert!(observations.iter().any(|observation| {
        observation.tid == child
            && observation.event == InjectedSyscallEvent::Returned(child as i64)
    }));
    drop(observations);
    let terminals = log.terminal.lock().unwrap();
    assert_eq!(terminals.len(), 2);
    assert!(terminals.contains(&(parent, output.status)));
    assert!(terminals.contains(&(child, ExitStatus::Exited(0))));
    let mut cleanup = log.tool_exit.lock().unwrap().clone();
    cleanup.sort_unstable();
    let mut expected = vec![parent, child];
    expected.sort_unstable();
    assert_eq!(cleanup, expected);
}

#[test]
fn backend_setup_does_not_publish_as_tool_mprotect() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            let page = libc::mmap(
                std::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert_ne!(page, libc::MAP_FAILED);
            assert_eq!(libc::mprotect(page, 4096, libc::PROT_READ), 0);
            assert_eq!(libc::munmap(page, 4096), 0);
        },
        Case::BackendSetup,
        true,
    )
    .expect("ptrace backend setup and Tool mprotect");
    assert_eq!(output.status, ExitStatus::Exited(0));
    let observed = log.observations.lock().unwrap();
    assert_eq!(
        observed.len(),
        2,
        "unexpected native observations: {observed:?}"
    );
    assert_eq!(observed[0].event, InjectedSyscallEvent::Entered);
    assert_eq!(observed[0].tid, observed[1].tid);
    assert_eq!(observed[0].nr, observed[1].nr);
    assert_eq!(observed[0].args, observed[1].args);
    let entry = observed[1];
    assert_eq!(entry.nr, Sysno::mprotect);
    assert_eq!(entry.args.arg1, 4096);
    assert_eq!(entry.args.arg2, libc::PROT_READ as usize);
    assert_eq!(entry.event, InjectedSyscallEvent::Returned(0));
    drop(observed);
    assert_normal_terminal(&log, 1);
}

fn assert_child_completion_pair(log: &Log, original: bool) {
    let observations = log.observations.lock().unwrap();
    let births: Vec<_> = observations
        .iter()
        .filter_map(|entry| {
            if let InjectedSyscallEvent::ChildCreated(child) = entry.event {
                Some((entry.tid, child.as_raw()))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(births.len(), 1);
    let completions: Vec<_> = observations
        .iter()
        .filter_map(|entry| {
            if let InjectedSyscallEvent::ChildSyscallReturned { child, raw } = entry.event {
                Some((entry.tid, child.as_raw(), raw))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        completions,
        vec![(births[0].0, births[0].1, i64::from(births[0].1))]
    );
    assert!(
        !observations.iter().any(|entry| entry.nr == Sysno::fork
            && matches!(entry.event, InjectedSyscallEvent::Returned(_)))
    );
    assert_eq!(observations.len(), if original { 3 } else { 2 });
    let observations = if original {
        assert_eq!(observations[0].event, InjectedSyscallEvent::Entered);
        assert_eq!(observations[0].tid, observations[1].tid);
        assert_eq!(observations[0].nr, observations[1].nr);
        assert_eq!(observations[0].args, observations[1].args);
        &observations[1..]
    } else {
        &observations[..]
    };
    assert!(matches!(
        observations[0].event,
        InjectedSyscallEvent::ChildCreated(_)
    ));
    assert!(matches!(
        observations[1].event,
        InjectedSyscallEvent::ChildSyscallReturned { .. }
    ));
}

#[test]
fn private_child_return_is_authenticated_before_frame_restoration() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            libc::alarm(5);
            let child = libc::syscall(libc::SYS_getuid);
            assert!(child >= 0);
            if child == 0 {
                libc::_exit(0);
            }
            let mut status = 0;
            assert_eq!(libc::waitpid(child as i32, &mut status, 0), child as i32);
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0);
        },
        Case::PrivateChild,
        true,
    )
    .expect("actual private fork parent return");
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert_child_completion_pair(&log, false);
    assert_normal_terminal(&log, 2);
}

static PARENT_SIGNAL_COUNT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
extern "C" fn parent_signal_handler(_: libc::c_int) {
    PARENT_SIGNAL_COUNT.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn queued_parent_signal_preserves_authentic_child_return_and_delivery() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            libc::alarm(5);
            PARENT_SIGNAL_COUNT.store(0, Ordering::SeqCst);
            assert_ne!(
                libc::signal(
                    libc::SIGUSR1,
                    parent_signal_handler as *const () as libc::sighandler_t
                ),
                libc::SIG_ERR
            );
            let child = libc::syscall(libc::SYS_fork);
            assert!(child >= 0);
            if child == 0 {
                libc::_exit(0);
            }
            assert_eq!(PARENT_SIGNAL_COUNT.load(Ordering::SeqCst), 1);
            let mut status = 0;
            loop {
                let result = libc::waitpid(child as i32, &mut status, 0);
                if result == child as i32 {
                    break;
                }
                assert_eq!(result, -1);
                assert_eq!(*libc::__errno_location(), libc::EINTR);
            }
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0);
            assert_eq!(PARENT_SIGNAL_COUNT.load(Ordering::SeqCst), 1);
        },
        Case::SignalAtChildEvent,
        true,
    )
    .expect("signal around actual parent completion");
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(log.signal_sent.load(Ordering::SeqCst));
    assert!(!log.killed.load(Ordering::SeqCst));
    assert_child_completion_pair(&log, true);
    assert_normal_terminal(&log, 2);
}

#[derive(Default)]
struct RejectionEvidence {
    actors: Vec<(i32, OwnedFd)>,
    births: Vec<(i32, i32)>,
    observed: Vec<(i32, InjectedSyscallEvent)>,
    rejected: Option<i32>,
    terminal: Vec<(i32, ExitStatus)>,
    consumed: Vec<i32>,
    process_consumed: Vec<i32>,
    pending_child: Option<i32>,
}
static REJECTION_EVIDENCE: Mutex<Option<RejectionEvidence>> = Mutex::new(None);
static REJECTION_FIXTURE_OWNER: Mutex<()> = Mutex::new(());
static REJECTION_CHANGED: Notify = Notify::const_new();

#[test]
fn late_orphan_parent_completion_rejection_drains_original_actors_and_stays_failed() {
    let _fixture_owner = REJECTION_FIXTURE_OWNER.lock().unwrap();
    assert!(
        REJECTION_EVIDENCE
            .lock()
            .unwrap()
            .replace(RejectionEvidence::default())
            .is_none()
    );
    let result = test_fn_with_config::<Observer, _>(
        || unsafe {
            libc::alarm(5);
            let child = libc::syscall(libc::SYS_fork);
            assert!(child >= 0);
            if child > 0 {
                libc::_exit(0);
            }
            libc::alarm(5);
            let grandchild = libc::syscall(libc::SYS_fork);
            assert!(grandchild >= 0);
            loop {
                libc::pause();
            }
        },
        Case::LateParentRejection,
        true,
    );
    let error = match result {
        Ok(_) => panic!("late orphan authentication failure must not expose cached root success"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains("native parent syscall completion failed"),
        "{message}"
    );
    assert!(message.contains("EPROTO"), "original error lost: {message}");
    let evidence = REJECTION_EVIDENCE.lock().unwrap().take().unwrap();
    assert_eq!(evidence.births.len(), 2);
    let (root, parent) = evidence.births[0];
    let (creator, child) = evidence.births[1];
    assert_eq!(creator, parent);
    assert_eq!(evidence.rejected, Some(parent));
    assert_eq!(evidence.actors.len(), 3);
    let mut actual = evidence.terminal.clone();
    actual.sort_by_key(|r| r.0);
    let mut expected = vec![
        (root, ExitStatus::Exited(0)),
        (parent, ExitStatus::Signaled(Signal::SIGKILL, false)),
        (child, ExitStatus::Signaled(Signal::SIGKILL, false)),
    ];
    expected.sort_by_key(|r| r.0);
    assert_eq!(actual, expected);
    let mut ids = vec![root, parent, child];
    ids.sort();
    let mut consumed = evidence.consumed;
    consumed.sort();
    assert_eq!(consumed, ids);
    let mut process_consumed = evidence.process_consumed;
    process_consumed.sort();
    assert_eq!(process_consumed, ids);
    let completions: Vec<_> = evidence
        .observed
        .iter()
        .filter_map(|(tid, event)| {
            if let InjectedSyscallEvent::ChildSyscallReturned { child, raw } = event {
                Some((*tid, child.as_raw(), *raw))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(completions, vec![(root, parent, i64::from(parent))]);
    assert!(!evidence.observed.iter().any(|(tid, event)| *tid == parent
        && matches!(
            event,
            InjectedSyscallEvent::Returned(_) | InjectedSyscallEvent::ChildSyscallReturned { .. }
        )));
    for (pid, fd) in &evidence.actors {
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
        assert_eq!(
            poll.revents & libc::POLLIN,
            libc::POLLIN,
            "original actor {pid} is not terminal"
        );
    }
    println!(
        "parent-completion-rejection root={root}:exit0 parent={parent}:SIGKILL child={child}:SIGKILL consuming=3 pidfd-terminal=3 source=actual-final original=EPROTO no-external-signal"
    );
}

#[test]
fn queued_stop_and_child_continue_follow_authentic_parent_return() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            libc::alarm(5);
            let child = libc::syscall(libc::SYS_fork);
            assert!(child >= 0);
            if child == 0 {
                // The Tool waits for the actual parent receipt before sending
                // SIGCONT. This does not claim that the preexisting backend's
                // generic SIGSTOP suppression implements Linux group stops.
                assert!(libc::syscall(libc::SYS_getpid) > 0);
                libc::_exit(0);
            }
            let mut status = 0;
            loop {
                let result = libc::waitpid(child as i32, &mut status, 0);
                if result == child as i32 {
                    break;
                }
                assert_eq!(result, -1);
                assert_eq!(*libc::__errno_location(), libc::EINTR);
            }
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0);
        },
        Case::StopAtChildEvent,
        true,
    )
    .expect("queued stop must not prevent authentic parent return");
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(log.stop_sent.load(Ordering::SeqCst));
    assert!(log.continued.load(Ordering::SeqCst));
    let events = log.observations.lock().unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(events[0].event, InjectedSyscallEvent::Entered);
    let (parent, child) = match events[1].event {
        InjectedSyscallEvent::ChildCreated(child) => (events[1].tid, child.as_raw()),
        event => panic!("expected actual birth: {event:?}"),
    };
    assert_eq!(events[0].tid, parent);
    assert_eq!(events[0].nr, events[1].nr);
    assert_eq!(events[0].args, events[1].args);
    assert_eq!(events[2].tid, parent);
    assert_eq!(
        events[2].event,
        InjectedSyscallEvent::ChildSyscallReturned {
            child: Pid::from_raw(child),
            raw: i64::from(child),
        }
    );
    assert_eq!(events[3].event, InjectedSyscallEvent::Entered);
    assert_eq!(events[3].tid, child);
    assert_eq!(events[3].nr, events[4].nr);
    assert_eq!(events[3].args, events[4].args);
    assert_eq!(events[4].tid, child);
    assert_eq!(
        events[4].event,
        InjectedSyscallEvent::Returned(i64::from(child))
    );
    drop(events);
    assert_normal_terminal(&log, 2);
}

#[test]
fn vfork_child_death_signal_cancels_parent_return_wait_without_receipt() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            libc::alarm(5);
            let child = libc::syscall(libc::SYS_vfork);
            if child == 0 {
                // The creator cannot complete vfork while this child retains
                // its address space. Real child execution supplies SIGKILL;
                // the original creator EXIT owns cancellation of the wait.
                let parent = libc::syscall(libc::SYS_getppid);
                assert!(parent > 0);
                assert_eq!(libc::syscall(libc::SYS_kill, parent, libc::SIGKILL), 0);
                libc::_exit(0);
            }
            panic!("killed vfork creator cannot continue: {child}");
        },
        Case::VforkCreatorDeath,
        true,
    )
    .expect("vfork creator actual terminal and surviving child cleanup");
    assert_eq!(output.status, ExitStatus::Signaled(Signal::SIGKILL, false));
    let events = log.observations.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].nr, Sysno::vfork);
    assert_eq!(events[0].event, InjectedSyscallEvent::Entered);
    assert_eq!(events[0].tid, events[1].tid);
    assert_eq!(events[0].nr, events[1].nr);
    assert_eq!(events[0].args, events[1].args);
    let parent = events[0].tid;
    let child = match events[1].event {
        InjectedSyscallEvent::ChildCreated(child) => child.as_raw(),
        event => panic!("only early child identity is authorized: {event:?}"),
    };
    drop(events);
    let mut actual = log.terminal.lock().unwrap().clone();
    actual.sort_by_key(|r| r.0);
    let mut expected = vec![(parent, output.status), (child, ExitStatus::Exited(0))];
    expected.sort_by_key(|r| r.0);
    assert_eq!(actual, expected);
    let mut consumed = log.tool_exit.lock().unwrap().clone();
    consumed.sort();
    let mut ids = vec![parent, child];
    ids.sort();
    assert_eq!(consumed, ids);
    println!(
        "vfork-parent-wait parent={parent}:SIGKILL child={child}:exit0 actual-final=2 consuming=2 receipt=0"
    );
}

fn assert_published_parent_rejection(result: Result<(), String>, threaded: bool) {
    let message = result.expect_err("rejected parent identity must fail the entire traced tree");
    assert!(
        message.contains("native parent syscall completion failed"),
        "{message}"
    );
    assert!(message.contains("EPROTO"), "original error lost: {message}");
    let evidence = REJECTION_EVIDENCE.lock().unwrap().take().unwrap();
    assert_eq!(evidence.births.len(), 2);
    let (root, first) = evidence.births[0];
    let (second_parent, second) = evidence.births[1];
    assert_eq!(second_parent, root);
    assert_eq!(evidence.pending_child, Some(first));
    assert_eq!(evidence.rejected, Some(root));
    assert_eq!(evidence.actors.len(), 3);
    let mut ids = vec![root, first, second];
    ids.sort();
    let mut actual = evidence.terminal;
    actual.sort_by_key(|r| r.0);
    let expected: Vec<_> = ids
        .iter()
        .map(|id| (*id, ExitStatus::Signaled(Signal::SIGKILL, false)))
        .collect();
    assert_eq!(actual, expected);
    let mut consumed = evidence.consumed;
    consumed.sort();
    assert_eq!(consumed, ids);
    let mut process_consumed = evidence.process_consumed;
    process_consumed.sort();
    assert_eq!(
        process_consumed,
        if threaded { vec![root] } else { ids.clone() }
    );
    let completions: Vec<_> = evidence
        .observed
        .iter()
        .filter_map(|(tid, event)| {
            if let InjectedSyscallEvent::ChildSyscallReturned { child, raw } = event {
                Some((*tid, child.as_raw(), *raw))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(completions, vec![(root, first, i64::from(first))]);
    assert!(
        !evidence
            .observed
            .iter()
            .any(|(_, event)| matches!(event, InjectedSyscallEvent::Returned(_)))
    );
    for (pid, fd) in &evidence.actors {
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
        assert_eq!(
            poll.revents & libc::POLLIN,
            libc::POLLIN,
            "original actor {pid} is not terminal"
        );
    }
    println!(
        "parent-rejection threaded={threaded} root={root} pending-callback={first} published={second} actual-final=3 consuming=3 pidfd-terminal=3 source=actual-final original=EPROTO no-external-signal"
    );
}

#[test]
fn published_process_rejection_cancels_pending_tool_and_drains_all_actors() {
    let _fixture_owner = REJECTION_FIXTURE_OWNER.lock().unwrap();
    assert!(
        REJECTION_EVIDENCE
            .lock()
            .unwrap()
            .replace(RejectionEvidence::default())
            .is_none()
    );
    let result = test_fn_with_config::<Observer, _>(
        || unsafe {
            libc::alarm(5);
            let first = libc::syscall(libc::SYS_fork);
            assert!(first >= 0);
            if first == 0 {
                loop {
                    libc::pause();
                }
            }
            let second = libc::syscall(libc::SYS_fork);
            panic!("rejected creator/second child cannot execute guest continuation: {second}");
        },
        Case::ProcessParentRejection,
        true,
    );
    assert_published_parent_rejection(result.map(|_| ()).map_err(|e| e.to_string()), false);
}

extern "C" fn rejection_thread_body(_: *mut libc::c_void) -> libc::c_int {
    panic!("pending/rejected thread cannot execute a guest instruction");
}

#[test]
fn published_thread_rejection_cancels_pending_tool_and_drains_same_group() {
    let _fixture_owner = REJECTION_FIXTURE_OWNER.lock().unwrap();
    assert!(
        REJECTION_EVIDENCE
            .lock()
            .unwrap()
            .replace(RejectionEvidence::default())
            .is_none()
    );
    let result = test_fn_with_config::<Observer, _>(
        || unsafe {
            libc::alarm(5);
            let mut stacks = [vec![0u128; 4096], vec![0u128; 4096]];
            let flags = libc::CLONE_VM | libc::CLONE_SIGHAND | libc::CLONE_THREAD;
            let first = libc::clone(
                rejection_thread_body,
                stacks[0].as_mut_ptr().add(stacks[0].len()).cast(),
                flags,
                std::ptr::null_mut(),
            );
            assert!(first > 0);
            let second = libc::clone(
                rejection_thread_body,
                stacks[1].as_mut_ptr().add(stacks[1].len()).cast(),
                flags,
                std::ptr::null_mut(),
            );
            panic!("rejected thread creator cannot execute guest continuation: {second}");
        },
        Case::ThreadParentRejection,
        true,
    );
    assert_published_parent_rejection(result.map(|_| ()).map_err(|e| e.to_string()), true);
}

#[test]
fn opted_in_preparation_binds_same_owned_state_before_real_return_and_continuation() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            assert!(libc::syscall(libc::SYS_getpid) > 0);
        },
        Case::Preparation,
        true,
    )
    .expect("actual opted-in preparation");
    assert_eq!(output.status, ExitStatus::Exited(0));
    let events = log.observations.lock().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].nr, Sysno::getpid);
    assert_eq!(events[0].event, InjectedSyscallEvent::Prepared);
    assert_eq!(events[1].event, InjectedSyscallEvent::Entered);
    assert!(events.iter().all(|event| event.tid == events[0].tid
        && event.nr == events[0].nr
        && event.args == events[0].args));
    assert_eq!(
        events[2].event,
        InjectedSyscallEvent::Returned(i64::from(events[0].tid))
    );
    drop(events);
    assert_normal_terminal(&log, 1);
}

#[test]
fn opted_in_cancellation_before_injection_issues_neither_preparation_nor_result() {
    let (output, log) = test_fn_with_config::<Observer, _>(
        || unsafe {
            libc::syscall(libc::SYS_getpid);
            libc::_exit(87);
        },
        Case::PreparationCancelBefore,
        true,
    )
    .expect("actual cancellation before injection");
    assert_eq!(output.status, ExitStatus::Signaled(Signal::SIGKILL, false));
    assert!(log.observations.lock().unwrap().is_empty());
    assert_eq!(log.terminal.lock().unwrap().len(), 1);
    assert_eq!(log.tool_exit.lock().unwrap().len(), 1);
    assert_eq!(
        log.terminal.lock().unwrap()[0].0,
        log.tool_exit.lock().unwrap()[0]
    );
}

#[path = "injected_observation/original_read.rs"]
mod original_read;

#[path = "injected_observation/original_context.rs"]
mod original_context;
