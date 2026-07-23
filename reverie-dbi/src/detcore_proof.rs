/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A minimal **Detcore-shaped** proof tool for the DynamoRIO backend.
//!
//! Detcore (in the separate `hermit` repo) cannot be dropped onto the DBI Guest
//! yet — see `ai_docs/dbi-detcore-integration.md` for the
//! architectural blockers (cross-repo dependency cycle, no cooperative executor
//! for the multi-thread scheduler, and hermit having no `--backend` selection).
//!
//! What *is* provable today, and what this module demonstrates, is that the
//! post-M2 [`crate::DbiGuest`] implements enough of the [`reverie::Guest`]
//! contract to host a tool with the **same interface shape as Detcore** for the
//! single-threaded / immediately-resolving case:
//!
//!  * a real, non-`()` [`GlobalTool`] state consulted through async RPC
//!    (via the [`reverie::GlobalRPC`] supertrait of [`Guest`]) — a miniature of Detcore's
//!    central logical clock / scheduler;
//!  * a real, non-`()` [`Tool::ThreadState`] seeded by
//!    [`Tool::init_thread_state`] (the lifecycle callback Detcore uses to assign
//!    deterministic thread ids and per-thread logical clocks);
//!  * a real, non-`()` [`GlobalTool::Config`] read via [`Guest::config`].
//!
//! The single-poll driver can drive this because the in-process RPC resolves
//! synchronously. Detcore's handlers additionally *suspend* mid-handler waiting
//! on cross-thread scheduling decisions, which is the remaining executor gap
//! documented in the analysis doc.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::sync::LazyLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Tid;
use reverie::Tool;
use reverie::syscalls::Errno;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use serde::Deserialize;
use serde::Serialize;

use crate::DbiGuest;
use crate::RegisterReader;
use crate::SyscallInvoker;

/// Deterministic configuration. Proves `Guest::config` plumbing for a non-`()`
/// config (Detcore reads a large `DetConfig` the same way).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ProofConfig {
    /// Seed the logical clock starts from, so runs are reproducible.
    pub seed: u64,
}

/// Central logical clock — a miniature of Detcore's global scheduler/clock.
/// Hands out monotonically increasing, deterministic ticks over RPC.
#[derive(Debug, Default)]
pub struct ProofGlobal {
    clock: AtomicU64,
}

/// RPC request: advance the logical clock by `n`, returning the pre-increment
/// tick. Modeled on Detcore consulting its global clock/scheduler per syscall.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Tick(pub u64);

#[reverie::global_tool]
impl GlobalTool for ProofGlobal {
    type Request = Tick;
    type Response = u64;
    type Config = ProofConfig;

    async fn init_global_state(cfg: &Self::Config) -> Self {
        ProofGlobal {
            clock: AtomicU64::new(cfg.seed),
        }
    }

    async fn receive_rpc(&self, _from: Tid, Tick(n): Tick) -> u64 {
        self.clock.fetch_add(n, Ordering::SeqCst)
    }
}

/// Per-thread state, seeded by [`Tool::init_thread_state`]. Detcore seeds an
/// analogous structure (deterministic thread id + per-thread logical clock).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ProofThread {
    /// Deterministic thread id assigned at thread start (tree position, not the
    /// host tid).
    pub dettid: u64,
    /// Last logical tick this thread observed from the global clock.
    pub last_tick: u64,
    /// Syscalls handled by this thread.
    pub syscalls: u64,
}

/// The Detcore-shaped proof tool: an async, stateful, RPC-driven, lifecycle-aware
/// [`Tool`] dispatched through [`crate::DbiGuest`].
#[derive(Clone, Copy, Debug, Default)]
pub struct DetProofTool;

#[reverie::tool]
impl Tool for DetProofTool {
    type GlobalState = ProofGlobal;
    type ThreadState = ProofThread;

    fn init_thread_state(
        &self,
        _child: Tid,
        parent: Option<(Tid, &Self::ThreadState)>,
    ) -> Self::ThreadState {
        // Deterministic thread id from tree position (mirrors Detcore's dettid),
        // independent of the host-assigned tid.
        let dettid = match parent {
            Some((_, parent_state)) => parent_state.dettid + 1,
            None => 1,
        };
        ProofThread {
            dettid,
            last_tick: 0,
            syscalls: 0,
        }
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        // Consult the central logical clock — the determinism decision, exactly
        // as Detcore consults its scheduler/clock before proceeding. In-process
        // this RPC resolves synchronously, so the single-poll DBI driver drives
        // it to completion. (Detcore additionally *suspends* here awaiting
        // cross-thread scheduling; see the analysis doc.)
        let tick = guest.send_rpc(Tick(1)).await;
        // Prove non-`()` config plumbing.
        let _seed = guest.config().seed;
        let state = guest.thread_state_mut();
        state.last_tick = tick;
        state.syscalls += 1;
        // Pass the syscall through to the kernel, observing its real result.
        Ok(guest.inject(call).await?)
    }
}

// ---------------------------------------------------------------------------
// Runtime dispatch: run the proof tool under the real DynamoRIO client, gated
// by `HERMIT_DBI_DETPROOF=1`. This is the closest achievable analog to
// `hermit run --backend dbi` — a Detcore-shaped Tool driven over the live DBI
// Guest — since real Detcore cannot yet be dropped in (see the analysis doc).
// ---------------------------------------------------------------------------

const DETPROOF_ENV: &str = "HERMIT_DBI_DETPROOF";
const DETPROOF_SEED_ENV: &str = "HERMIT_DBI_DETPROOF_SEED";

fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| {
        !value.is_empty() && value != OsStr::new("0") && value != OsStr::new("false")
    })
}

static ENABLED: LazyLock<bool> = LazyLock::new(|| env_flag(DETPROOF_ENV));

/// Process-global config, seeded from the environment for reproducibility.
static CONFIG: LazyLock<ProofConfig> = LazyLock::new(|| ProofConfig {
    seed: std::env::var(DETPROOF_SEED_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0),
});

/// The single process-global instance of the tool's [`GlobalTool`] state — the
/// owner the DBI backend otherwise lacks (it hardwires the global to `()`).
static GLOBAL: LazyLock<ProofGlobal> = LazyLock::new(|| ProofGlobal {
    clock: AtomicU64::new(CONFIG.seed),
});

thread_local! {
    /// Per-thread state, lazily seeded by [`Tool::init_thread_state`] on the
    /// thread's first observed syscall.
    static THREAD: RefCell<Option<ProofThread>> = const { RefCell::new(None) };
}

/// Whether the Detcore-shaped proof tool is selected.
pub(crate) fn enabled() -> bool {
    *ENABLED
}

/// Handles one syscall through the proof tool over a live [`DbiGuest`], returning
/// the result to install (suppressing the original syscall).
#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    context: usize,
    tid: i32,
    pid: i32,
    sysnum: i64,
    raw_args: &[u64],
    branches: u64,
    invoke_syscall: SyscallInvoker,
    read_registers: RegisterReader,
) -> i64 {
    let number = Sysno::from(sysnum as i32);
    let syscall = Syscall::from_raw(
        number,
        SyscallArgs::new(
            raw_args[0] as usize,
            raw_args[1] as usize,
            raw_args[2] as usize,
            raw_args[3] as usize,
            raw_args[4] as usize,
            raw_args[5] as usize,
        ),
    );
    let tool = DetProofTool;

    THREAD.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            // DBI instruments a single process, so the first thread observed is
            // the root of the tree (no parent state).
            *slot = Some(tool.init_thread_state(Pid::from_raw(tid), None));
        }
        let thread_state = slot.as_mut().expect("thread state seeded above");

        // Print a summary just before the process exits (the injected exit does
        // not return to us).
        if matches!(number, Sysno::exit | Sysno::exit_group) {
            crate::tools::emit_line(&format!(
                "reverie-dbi detproof: dettid {} handled {} syscalls; logical clock = {}",
                thread_state.dettid,
                thread_state.syscalls,
                GLOBAL.clock.load(Ordering::SeqCst),
            ));
        }

        let mut guest: DbiGuest<'_, DetProofTool> = DbiGuest::new(
            context,
            Pid::from_raw(tid),
            Pid::from_raw(pid),
            None,
            branches,
            thread_state,
            &*GLOBAL,
            &*CONFIG,
            invoke_syscall,
            read_registers,
        );
        match crate::run_ready(tool.handle_syscall_event(&mut guest, syscall)) {
            Some(Ok(value)) => value,
            Some(Err(Error::Errno(errno))) => -(errno.into_raw() as i64),
            Some(Err(_)) => -(Errno::EIO.into_raw() as i64),
            // The proof tool never suspends (its RPC is synchronous in-process);
            // a suspension would indicate a Detcore-style cross-thread await we
            // cannot yet drive here.
            None => -(Errno::EIO.into_raw() as i64),
        }
    })
}

#[cfg(test)]
mod tests {
    use reverie::Pid;
    use reverie::syscalls::SyscallArgs;
    use reverie::syscalls::Sysno;

    use super::*;
    use crate::DbiGuest;

    unsafe extern "C" fn invoke_write(_context: usize, sysnum: i64, args: *const u64) -> i64 {
        assert_eq!(sysnum, libc::SYS_write);
        // Echo back the count argument as the "kernel" result.
        unsafe { *args.add(2) as i64 }
    }

    unsafe extern "C" fn read_regs(_context: usize, regs: *mut libc::user_regs_struct) -> i32 {
        unsafe { (*regs).rip = 0x1000 };
        1
    }

    /// Polls a handler expected to resolve without suspending (the in-process
    /// RPC does), mirroring the driver's single poll.
    fn poll_once<F: std::future::Future>(future: F) -> Option<F::Output> {
        let mut future = std::pin::pin!(future);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => Some(value),
            std::task::Poll::Pending => None,
        }
    }

    fn write_syscall() -> Syscall {
        Syscall::from_raw(Sysno::write, SyscallArgs::new(1, 0x2000, 7, 0, 0, 0))
    }

    // This test's real value is that it *compiles*: it forces
    // `DbiGuest: Guest<DetProofTool>` where `DetProofTool` has non-`()`
    // GlobalState, ThreadState and Config — i.e. the DBI Guest satisfies the
    // interface a Detcore-shaped tool requires.
    #[test]
    fn dbi_guest_hosts_a_detcore_shaped_tool() {
        let tool = DetProofTool;
        let config = ProofConfig { seed: 100 };
        let global = ProofGlobal {
            clock: AtomicU64::new(config.seed),
        };

        // init_thread_state seeds deterministic per-thread state.
        let mut thread_state = tool.init_thread_state(Pid::from_raw(1), None);
        assert_eq!(thread_state.dettid, 1);

        let mut guest: DbiGuest<'_, DetProofTool> = DbiGuest::new(
            0,
            Pid::from_raw(10),
            Pid::from_raw(10),
            None,
            0,
            &mut thread_state,
            &global,
            &config,
            invoke_write,
            read_regs,
        );

        // Handler resolves via a single poll (in-process RPC is synchronous) and
        // returns the injected syscall's result.
        let result = poll_once(tool.handle_syscall_event(&mut guest, write_syscall()))
            .expect("handler must resolve without suspending")
            .expect("handler must succeed");
        assert_eq!(result, 7);

        // The global clock advanced deterministically and the thread observed it.
        assert_eq!(guest.thread_state().last_tick, 100);
        assert_eq!(guest.thread_state().syscalls, 1);
        assert_eq!(global.clock.load(Ordering::SeqCst), 101);
    }

    // Two independent runs from the same seed produce identical tick sequences —
    // the essence of determinism the tool proves through the DBI Guest.
    #[test]
    fn logical_clock_is_deterministic_across_runs() {
        fn run() -> Vec<u64> {
            let tool = DetProofTool;
            let config = ProofConfig { seed: 0 };
            let global = ProofGlobal::default();
            let mut ticks = Vec::new();
            for _ in 0..3 {
                let mut thread_state = tool.init_thread_state(Pid::from_raw(1), None);
                let mut guest: DbiGuest<'_, DetProofTool> = DbiGuest::new(
                    0,
                    Pid::from_raw(10),
                    Pid::from_raw(10),
                    None,
                    0,
                    &mut thread_state,
                    &global,
                    &config,
                    invoke_write,
                    read_regs,
                );
                poll_once(tool.handle_syscall_event(&mut guest, write_syscall()))
                    .unwrap()
                    .unwrap();
                ticks.push(guest.thread_state().last_tick);
            }
            ticks
        }
        assert_eq!(run(), run());
        assert_eq!(run(), vec![0, 1, 2]);
    }
}
