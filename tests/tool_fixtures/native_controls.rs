// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.

//! Caller-owned fixtures shared by the controller tests and their two clients.

use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::CpuIdResult;
use reverie::Errno;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Rdtsc;
use reverie::RdtscResult;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use serde::Deserialize;
use serde::Serialize;

// A deliberately unusual probe leaf keeps the Tool's test reply out of normal
// loader feature detection. Other CPUID requests retain the physical answer.
pub const PROBE_LEAF: u32 = 0x4fff_fffe;
pub const PROBE_SUBLEAF: u32 = 0x1357;
pub const PROBE_REPLY: [u32; 4] = [0x3141_5926, 0x5358_9793, 0x2384_6264, 0x3383_2795];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstructionErrorMode {
    #[default]
    None,
    Cpuid,
    Rdtsc,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadState {
    pub initialized_tid: i32,
    pub thread_starts: u64,
    pub post_execs: u64,
    pub injected_start_pid: i64,
    pub syscalls: u64,
    pub ordinary_getpids: u64,
    pub cpuid_callbacks: u64,
    pub rdtsc_callbacks: u64,
    pub rdtsc_error_armed: bool,
    pub cookie: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    ThreadStart {
        state: ThreadState,
        pid: i32,
    },
    PostExec {
        state: ThreadState,
    },
    Getpid {
        injected: i64,
        pid: i32,
    },
    Cpuid {
        leaf: u32,
        subleaf: u32,
        value: [u32; 4],
    },
    Rdtsc {
        request: Rdtsc,
        value: RdtscResult,
    },
    InstructionError {
        mode: InstructionErrorMode,
        errno: i32,
        state: ThreadState,
    },
    ThreadExit {
        status: i32,
        state: ThreadState,
    },
    ProcessExit {
        status: i32,
    },
}

#[derive(Default)]
pub struct ControlGlobal {
    pub requests: AtomicU64,
    pub events: Mutex<Vec<(i32, Request)>>,
}

#[reverie::global_tool]
impl GlobalTool for ControlGlobal {
    type Request = Request;
    type Response = u64;
    type Config = InstructionErrorMode;

    async fn receive_rpc(&self, from: Pid, request: Request) -> u64 {
        let mut events = self.events.lock().unwrap();
        let ordinal = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        events.push((from.as_raw(), request));
        ordinal
    }
}

#[derive(Default)]
pub struct ControlTool<const INSTRUCTIONS: bool> {
    thread_exits: AtomicU64,
}

fn exit_code(status: ExitStatus) -> i32 {
    match status {
        ExitStatus::Exited(code) => code,
        other => panic!("native control expected an ordinary exit, got {other:?}"),
    }
}

#[reverie::tool]
impl<const INSTRUCTIONS: bool> Tool for ControlTool<INSTRUCTIONS> {
    type GlobalState = ControlGlobal;
    type ThreadState = ThreadState;

    fn subscriptions(_: &InstructionErrorMode) -> Subscription {
        let mut subscriptions = Subscription::all_syscalls();
        if INSTRUCTIONS {
            subscriptions.cpuid().rdtsc();
        }
        subscriptions
    }

    fn init_thread_state(&self, tid: Pid, parent: Option<(Pid, &ThreadState)>) -> ThreadState {
        assert!(parent.is_none(), "these commands have no child task");
        ThreadState {
            initialized_tid: tid.as_raw(),
            cookie: 0x41,
            ..ThreadState::default()
        }
    }

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        assert_eq!(guest.thread_state().initialized_tid, guest.tid().as_raw());
        assert_eq!(guest.thread_state().thread_starts, 0);
        assert_eq!(guest.thread_state().cookie, 0x41);
        // This is the same ordinary Getpid injection contract exercised by
        // tests/thread_start.rs, now through an actual command's native hook.
        let pid = guest.inject(reverie::syscalls::Getpid::new()).await?;
        assert_eq!(pid, i64::from(guest.pid().as_raw()));
        let state = guest.thread_state_mut();
        state.thread_starts = 1;
        state.injected_start_pid = pid;
        state.cookie += 1;
        let request = Request::ThreadStart {
            state: guest.thread_state().clone(),
            pid: guest.pid().as_raw(),
        };
        guest.send_rpc(request).await;
        Ok(())
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        assert_eq!(guest.thread_state().thread_starts, 1);
        assert_eq!(guest.thread_state().post_execs, 0);
        let state = guest.thread_state_mut();
        state.post_execs = 1;
        state.cookie += 0x10;
        guest
            .send_rpc(Request::PostExec {
                state: guest.thread_state().clone(),
            })
            .await;
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(guest.thread_state().thread_starts, 1);
        assert_eq!(guest.thread_state().post_execs, 1);
        let state = guest.thread_state_mut();
        state.syscalls += 1;
        state.cookie += 1;
        if let Syscall::Getpid(call) = syscall {
            let injected = guest.inject(call).await?;
            assert_eq!(injected, i64::from(guest.pid().as_raw()));
            guest.thread_state_mut().ordinary_getpids += 1;
            guest
                .send_rpc(Request::Getpid {
                    injected,
                    pid: guest.pid().as_raw(),
                })
                .await;
            Ok(injected)
        } else {
            guest.tail_inject(syscall).await
        }
    }

    async fn handle_cpuid_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        leaf: u32,
        subleaf: u32,
    ) -> Result<CpuIdResult, Errno> {
        assert!(INSTRUCTIONS, "an unsubscribed CPUID reached the Tool");
        let probe = leaf == PROBE_LEAF && subleaf == PROBE_SUBLEAF;
        let error_mode = *guest.config();
        if probe && error_mode != InstructionErrorMode::None {
            // Only the explicit guest probe may select an error. Loader
            // instruction callbacks must retain their ordinary answers.
            assert_eq!(guest.thread_state().thread_starts, 1);
            assert_eq!(guest.thread_state().post_execs, 1);
            if error_mode == InstructionErrorMode::Cpuid {
                let state = guest.thread_state_mut();
                state.cpuid_callbacks += 1;
                state.cookie += 1;
                let errno = Errno::EINVAL;
                guest
                    .send_rpc(Request::InstructionError {
                        mode: error_mode,
                        errno: errno.into_raw(),
                        state: guest.thread_state().clone(),
                    })
                    .await;
                return Err(errno);
            }
            guest.thread_state_mut().rdtsc_error_armed = true;
        }
        let value = if leaf == PROBE_LEAF && subleaf == PROBE_SUBLEAF {
            PROBE_REPLY
        } else {
            let physical = std::arch::x86_64::__cpuid_count(leaf, subleaf);
            [physical.eax, physical.ebx, physical.ecx, physical.edx]
        };
        let state = guest.thread_state_mut();
        state.cpuid_callbacks += 1;
        state.cookie += 1;
        guest
            .send_rpc(Request::Cpuid {
                leaf,
                subleaf,
                value,
            })
            .await;
        Ok(CpuIdResult {
            eax: value[0],
            ebx: value[1],
            ecx: value[2],
            edx: value[3],
        })
    }

    async fn handle_rdtsc_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        request: Rdtsc,
    ) -> Result<RdtscResult, Errno> {
        assert!(INSTRUCTIONS, "an unsubscribed RDTSC reached the Tool");
        if guest.thread_state().rdtsc_error_armed {
            assert_eq!(*guest.config(), InstructionErrorMode::Rdtsc);
            assert_eq!(guest.thread_state().thread_starts, 1);
            assert_eq!(guest.thread_state().post_execs, 1);
            let state = guest.thread_state_mut();
            state.rdtsc_callbacks += 1;
            state.cookie += 1;
            let errno = Errno::ERANGE;
            guest
                .send_rpc(Request::InstructionError {
                    mode: InstructionErrorMode::Rdtsc,
                    errno: errno.into_raw(),
                    state: guest.thread_state().clone(),
                })
                .await;
            return Err(errno);
        }
        // Preserve physical time. This fixture tests event routing and return
        // propagation, not a determinization strategy or a predicted timestamp.
        let value = RdtscResult::new(request);
        let state = guest.thread_state_mut();
        state.rdtsc_callbacks += 1;
        state.cookie += 1;
        guest.send_rpc(Request::Rdtsc { request, value }).await;
        Ok(value)
    }

    async fn on_exit_thread<R: GlobalRPC<ControlGlobal>>(
        &self,
        tid: Pid,
        port: &R,
        state: ThreadState,
        status: ExitStatus,
    ) -> Result<(), Error> {
        assert_eq!(tid.as_raw(), state.initialized_tid);
        assert_eq!(self.thread_exits.fetch_add(1, Ordering::SeqCst), 0);
        port.send_rpc(Request::ThreadExit {
            status: exit_code(status),
            state,
        })
        .await;
        Ok(())
    }

    async fn on_exit_process<R: GlobalRPC<ControlGlobal>>(
        self,
        _: Pid,
        port: &R,
        status: ExitStatus,
    ) -> Result<(), Error> {
        assert_eq!(self.thread_exits.load(Ordering::SeqCst), 1);
        port.send_rpc(Request::ProcessExit {
            status: exit_code(status),
        })
        .await;
        Ok(())
    }
}
