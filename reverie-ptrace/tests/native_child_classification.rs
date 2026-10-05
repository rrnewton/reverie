/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Actual TGID, not the ptrace notification opcode, chooses process ownership.
//! Native ptrace fixtures: run individually under the maintained bounded owner.
#![cfg(target_arch = "x86_64")]

use std::sync::Mutex;

use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::InjectedSyscallEvent;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use reverie_ptrace::testing::test_fn_with_config;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
enum Case {
    #[default]
    CloneProcess,
    Clone3Process,
    Thread,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Request {
    Started {
        tid: i32,
        pid: i32,
        constructed: i32,
    },
    ThreadExit(i32),
    ProcessExit(i32),
}

#[derive(Default)]
struct Log {
    births: Mutex<Vec<(i32, i32, Sysno)>>,
    starts: Mutex<Vec<(i32, i32)>>,
    terminal: Mutex<Vec<(i32, ExitStatus)>>,
    threads: Mutex<Vec<i32>>,
    processes: Mutex<Vec<i32>>,
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Config = Case;
    type Request = Request;
    type Response = ();

    async fn receive_rpc(&self, from: Pid, request: Request) {
        match request {
            Request::Started {
                tid,
                pid,
                constructed,
            } => {
                assert_eq!(tid, from.as_raw());
                assert_eq!(
                    pid, constructed,
                    "Tool constructor must match the actual process"
                );
                let mut starts = self.starts.lock().unwrap();
                assert!(!starts.iter().any(|(old, _)| *old == tid));
                starts.push((tid, pid));
            }
            Request::ThreadExit(tid) => {
                assert_eq!(tid, from.as_raw());
                assert!(
                    self.terminal
                        .lock()
                        .unwrap()
                        .contains(&(tid, ExitStatus::Exited(0)))
                );
                self.threads.lock().unwrap().push(tid);
            }
            Request::ProcessExit(pid) => {
                assert_eq!(pid, from.as_raw());
                self.processes.lock().unwrap().push(pid);
            }
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ThreadState {
    child: Option<i32>,
    terminal: bool,
}

#[derive(Default)]
struct Identity {
    constructed: i32,
}

#[reverie::tool]
impl Tool for Identity {
    type GlobalState = Log;
    type ThreadState = ThreadState;

    fn new(pid: Pid, _case: &Case) -> Self {
        Self {
            constructed: pid.as_raw(),
        }
    }

    fn subscriptions(_case: &Case) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscalls([Sysno::clone, Sysno::clone3, Sysno::getpid, Sysno::gettid]);
        subscriptions
    }

    fn observe_injected_syscalls(_case: &Case) -> bool {
        true
    }

    fn init_thread_state(&self, child: Pid, parent: Option<(Pid, &ThreadState)>) -> ThreadState {
        if let Some((_, state)) = parent {
            assert_eq!(
                state.child,
                Some(child.as_raw()),
                "inherit only from the actual child event"
            );
        }
        ThreadState::default()
    }

    fn on_injected_syscall_observed(
        &self,
        tid: Pid,
        global: &Log,
        state: &mut ThreadState,
        nr: Sysno,
        _args: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        if let InjectedSyscallEvent::ChildCreated(child) = event {
            assert!(matches!(nr, Sysno::clone | Sysno::clone3));
            assert!(state.child.replace(child.as_raw()).is_none());
            global
                .births
                .lock()
                .unwrap()
                .push((tid.as_raw(), child.as_raw(), nr));
        }
    }

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        guest
            .send_rpc(Request::Started {
                tid: guest.tid().as_raw(),
                pid: guest.pid().as_raw(),
                constructed: self.constructed,
            })
            .await;
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let is_pid = matches!(&syscall, Syscall::Getpid(_));
        let is_tid = matches!(&syscall, Syscall::Gettid(_));
        let result = guest.inject(syscall).await;
        if is_pid {
            assert_eq!(
                result,
                Ok(guest.pid().as_raw() as i64),
                "Guest::pid must equal native getpid"
            );
            assert_eq!(result, Ok(self.constructed as i64));
        }
        if is_tid {
            assert_eq!(result, Ok(guest.tid().as_raw() as i64));
        }
        Ok(result?)
    }

    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        global: &Log,
        state: &mut ThreadState,
        status: ExitStatus,
    ) {
        assert!(!state.terminal);
        state.terminal = true;
        let mut terminal = global.terminal.lock().unwrap();
        assert!(!terminal.iter().any(|(old, _)| *old == tid.as_raw()));
        terminal.push((tid.as_raw(), status));
    }

    async fn on_exit_thread<G: GlobalRPC<Log>>(
        &self,
        tid: Pid,
        global: &G,
        state: ThreadState,
        status: ExitStatus,
    ) -> Result<(), Error> {
        assert!(state.terminal);
        assert_eq!(status, ExitStatus::Exited(0));
        global.send_rpc(Request::ThreadExit(tid.as_raw())).await;
        Ok(())
    }

    async fn on_exit_process<G: GlobalRPC<Log>>(
        self,
        pid: Pid,
        global: &G,
        status: ExitStatus,
    ) -> Result<(), Error> {
        assert_eq!(pid.as_raw(), self.constructed);
        assert_eq!(status, ExitStatus::Exited(0));
        global.send_rpc(Request::ProcessExit(pid.as_raw())).await;
        Ok(())
    }
}

// Linux UAPI clone_args layout (88 bytes); no shared VM/files/thread flags and a non-SIGCHLD
// exit signal deliberately select PTRACE_EVENT_CLONE for a process.
#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

fn run_case(case: Case) {
    let (output, log) = test_fn_with_config::<Identity, _>(
        move || unsafe {
            let parent = libc::syscall(libc::SYS_getpid);
            assert_eq!(libc::syscall(libc::SYS_gettid), parent);
            if case == Case::Thread {
                std::thread::spawn(move || {
                    assert_eq!(libc::syscall(libc::SYS_getpid), parent);
                    assert_ne!(libc::syscall(libc::SYS_gettid), parent);
                })
                .join()
                .unwrap();
                return;
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_IGN;
            assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0);
            assert_eq!(
                libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()),
                0
            );
            let child = if case == Case::CloneProcess {
                libc::syscall(
                    libc::SYS_clone,
                    libc::SIGUSR1,
                    0usize,
                    0usize,
                    0usize,
                    0usize,
                )
            } else {
                let args = CloneArgs {
                    exit_signal: libc::SIGUSR1 as u64,
                    ..CloneArgs::default()
                };
                libc::syscall(libc::SYS_clone3, &args, std::mem::size_of::<CloneArgs>())
            };
            assert!(
                child >= 0,
                "actual native clone failed: {}",
                std::io::Error::last_os_error()
            );
            if child == 0 {
                let pid = libc::syscall(libc::SYS_getpid);
                assert_ne!(pid, parent);
                assert_eq!(libc::syscall(libc::SYS_gettid), pid);
                libc::_exit(0);
            }
            let mut status = 0;
            assert_eq!(
                libc::waitpid(child as i32, &mut status, libc::__WCLONE),
                child as i32
            );
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0);
        },
        case,
        true,
    )
    .expect("native child classification");
    assert_eq!(output.status, ExitStatus::Exited(0));
    let births = log.births.lock().unwrap();
    assert_eq!(births.len(), 1);
    let (parent, child, nr) = births[0];
    if case == Case::CloneProcess {
        assert_eq!(nr, Sysno::clone);
    }
    if case == Case::Clone3Process {
        assert_eq!(nr, Sysno::clone3);
    }
    let mut starts = log.starts.lock().unwrap().clone();
    starts.sort_unstable();
    let mut expected = vec![
        (parent, parent),
        (child, if case == Case::Thread { parent } else { child }),
    ];
    expected.sort_unstable();
    assert_eq!(starts, expected);
    let mut terminal = log.terminal.lock().unwrap().clone();
    terminal.sort_unstable_by_key(|(tid, _)| *tid);
    let mut task_ids = vec![parent, child];
    task_ids.sort_unstable();
    assert_eq!(
        terminal,
        task_ids
            .iter()
            .map(|tid| (*tid, ExitStatus::Exited(0)))
            .collect::<Vec<_>>()
    );
    let mut threads = log.threads.lock().unwrap().clone();
    threads.sort_unstable();
    assert_eq!(threads, task_ids);
    let mut processes = log.processes.lock().unwrap().clone();
    processes.sort_unstable();
    assert_eq!(
        processes,
        if case == Case::Thread {
            vec![parent]
        } else {
            task_ids
        }
    );
}

#[test]
fn native_non_sigchld_clone_is_a_process() {
    run_case(Case::CloneProcess);
}
#[test]
fn native_non_sigchld_clone3_is_a_process() {
    run_case(Case::Clone3Process);
}
#[test]
fn native_clone_thread_shares_only_its_actual_process_owner() {
    run_case(Case::Thread);
}
