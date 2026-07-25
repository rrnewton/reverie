/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Backend-agnostic syscall-counter Reverie tool.
//!
//! This crate defines the tool *once*, depending only on the `reverie` trait
//! crate, so the exact same [`SysCtr`] / [`SysCtrGlobal`] implementation can be
//! linked into a binary against any Reverie backend (ptrace, KVM, DBI). The
//! per-backend binaries live in `reverie-multibackend-tools`.
//!
//! # Aggregation model
//!
//! The tally is maintained *live*: [`SysCtr::handle_syscall_event`] sends one
//! [`IncrMsg`] to the shared global state per syscall. This is deliberately not
//! the "contribute totals in `on_exit_process`" model used by
//! `reverie-examples/counter2`, because not every backend drives the process /
//! thread exit hooks (the KVM static-ELF runner, for example, does not). Live
//! increment relies only on `handle_syscall_event`, which every backend drives,
//! so the counts are correct on all of them. When the global state is shared
//! across a `fork` tree (in-process for ptrace/KVM, or via the cross-process
//! RPC transport for DBI), the tally aggregates across the whole tree.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Tool;
use reverie::syscalls::Syscall;
use serde::Deserialize;
use serde::Serialize;

/// Process-tree-wide syscall tally shared by every guest process.
#[derive(Debug, Default)]
pub struct SysCtrGlobal {
    total_syscalls: AtomicU64,
    pids: Mutex<BTreeSet<i32>>,
}

impl SysCtrGlobal {
    /// Total number of syscalls observed across the whole process tree so far.
    pub fn total_syscalls(&self) -> u64 {
        self.total_syscalls.load(Ordering::SeqCst)
    }

    /// Number of distinct guest processes that have issued at least one syscall.
    pub fn process_count(&self) -> usize {
        self.pids.lock().unwrap().len()
    }
}

/// RPC message: "add this many syscalls to the global tally."
#[derive(PartialEq, Eq, Hash, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IncrMsg(pub u64);

#[reverie::global_tool]
impl GlobalTool for SysCtrGlobal {
    type Request = IncrMsg;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, from: Pid, IncrMsg(n): IncrMsg) -> Self::Response {
        self.total_syscalls.fetch_add(n, Ordering::SeqCst);
        self.pids.lock().unwrap().insert(from.as_raw());
    }
}

/// The per-guest tool. Stateless: all counting happens in the shared global.
#[derive(Debug, Default, Clone)]
pub struct SysCtr;

#[reverie::tool]
impl Tool for SysCtr {
    type GlobalState = SysCtrGlobal;
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        // Count first, then run the syscall for real via the backend.
        let _ = guest.send_rpc(IncrMsg(1)).await;
        guest.tail_inject(syscall).await
    }
}

/// Print the final tally to stderr. Called by each backend binary after the
/// guest tree exits.
pub fn report(global: &SysCtrGlobal) {
    eprintln!(
        " [reverie-sysctr] Total syscalls in process tree: {} across {} process(es).",
        global.total_syscalls(),
        global.process_count(),
    );
}
