/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A Tool that probes per-thread state, global RPC and non-tail injection.

// The `#[reverie::tool]` expansion boxes each handler future by the bare name
// `Box`, which is only in the prelude with `std`.
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use serde::Deserialize;
use serde::Serialize;

/// The value `Probe` adds to the result of every `getpid` it handles.
pub const GETPID_BIAS: i64 = 5;

/// Process-tree state for [`Probe`].
#[derive(Debug, Default, Clone)]
pub struct ProbeGlobal {
    events: Arc<AtomicU64>,
    thread_state_sum: Arc<AtomicU64>,
}

impl ProbeGlobal {
    /// Number of syscalls the Tool handled.
    pub fn events(&self) -> u64 {
        self.events.load(Ordering::SeqCst)
    }

    /// Sum, over handled syscalls, of the handling thread's per-thread event
    /// count after that syscall. One thread handling `n` syscalls sums to
    /// `n * (n + 1) / 2`, which is only reached if the thread's state
    /// persisted across every syscall.
    pub fn thread_state_sum(&self) -> u64 {
        self.thread_state_sum.load(Ordering::SeqCst)
    }
}

/// One handled syscall, carrying the handling thread's updated count.
#[derive(PartialEq, Debug, Eq, Clone, Copy, Serialize, Deserialize)]
pub struct ProbeMsg(pub u64);

#[reverie::global_tool]
impl GlobalTool for ProbeGlobal {
    type Request = ProbeMsg;
    type Response = ();
    type Config = ();

    async fn init_global_state(_: &Self::Config) -> Self {
        Self::default()
    }

    async fn receive_rpc(&self, _from: Pid, ProbeMsg(count): ProbeMsg) {
        self.events.fetch_add(1, Ordering::SeqCst);
        self.thread_state_sum.fetch_add(count, Ordering::SeqCst);
    }
}

/// Counts syscalls per thread, reports each count to the global state, and
/// returns `getpid() + GETPID_BIAS` for every `getpid`, which it runs with a
/// non-tail `inject`. Every other syscall is tail-injected unmodified.
#[derive(Debug, Default, Clone)]
pub struct Probe;

#[reverie::tool]
impl Tool for Probe {
    type GlobalState = ProbeGlobal;
    type ThreadState = u64;

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        *guest.thread_state_mut() += 1;
        let count = *guest.thread_state();
        let _ = guest.send_rpc(ProbeMsg(count)).await;
        if syscall.number() == Sysno::getpid {
            let pid = guest.inject(syscall).await?;
            return Ok(pid + GETPID_BIAS);
        }
        guest.tail_inject(syscall).await
    }
}
