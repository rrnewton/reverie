/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Small, ready-to-run Reverie tools for the KVM backend prototype.
//!
//! These are deliberately trivial: they exercise the [`crate::KvmBackend`]
//! `run_with_tool` path end to end without needing a Linux execution runtime.
//! [`StraceTool`] is an strace-style observer that, for each intercepted
//! syscall, records both its name and its *fully decoded* rendering (arguments
//! dereferenced from guest memory) and then forwards it to the backend's
//! `SyscallExecutor`, exactly as the ptrace `Strace` example tool does.

use std::sync::Mutex;

use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Tool;
use reverie::syscalls::Displayable;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

/// One intercepted syscall recorded by [`StraceTool`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StraceEntry {
    /// The bare syscall mnemonic, for example `"write"`.
    pub name: String,
    /// The fully decoded syscall rendering, with typed arguments and pointers
    /// resolved against guest memory, for example `write(1, 0x3000, 5)` or
    /// `mmap(NULL, 0, ProtFlags(0x0), MapFlags(0x0), 0, 0)`. This has the same
    /// fidelity as the ptrace `Strace` example tool's
    /// `Displayable::display_with_outputs`, rather than the raw, undecoded
    /// argument words the `Debug` form prints.
    pub formatted: String,
}

/// Global state for [`StraceTool`]: the ordered list of intercepted syscalls,
/// aggregated from every guest thread through Reverie's global RPC.
#[derive(Default)]
pub struct StraceLog {
    entries: Mutex<Vec<StraceEntry>>,
}

impl StraceLog {
    /// Returns the syscall names recorded so far, in interception order.
    pub fn syscalls(&self) -> Vec<String> {
        self.entries
            .lock()
            .expect("strace log lock poisoned")
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// Returns the fully decoded syscall renderings (name + real, dereferenced
    /// arguments) recorded so far, in interception order.
    pub fn formatted(&self) -> Vec<String> {
        self.entries
            .lock()
            .expect("strace log lock poisoned")
            .iter()
            .map(|entry| entry.formatted.clone())
            .collect()
    }

    /// Returns the recorded entries (name + decoded rendering), in interception
    /// order.
    pub fn entries(&self) -> Vec<StraceEntry> {
        self.entries
            .lock()
            .expect("strace log lock poisoned")
            .clone()
    }
}

#[reverie::global_tool]
impl GlobalTool for StraceLog {
    // `(name, formatted rendering)`. A tuple of `String`s already satisfies
    // `Serialize + DeserializeOwned`, so the RPC transport needs no extra
    // dependency (the generated manifests stay untouched).
    type Request = (String, String);
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, (name, formatted): (String, String)) {
        self.entries
            .lock()
            .expect("strace log lock poisoned")
            .push(StraceEntry { name, formatted });
    }
}

/// An strace-like Reverie tool: on every subscribed syscall it renders the
/// syscall with its decoded arguments (dereferenced from guest memory), prints
/// it to stderr, records it in [`StraceLog`], and injects the syscall so the
/// backend executor still performs it. Running this through
/// [`crate::KvmBackend::run_with_tool`] proves the KVM `Guest`/`Tool` interface
/// works: interception, typed decoding, argument dereferencing, global RPC, and
/// injection all flow through the same Reverie contracts the ptrace backend
/// uses.
#[derive(Clone, Copy, Debug, Default)]
pub struct StraceTool;

#[reverie::tool]
impl Tool for StraceTool {
    type GlobalState = StraceLog;
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        let name = syscall.name().to_owned();
        // Execute the syscall through the backend `SyscallExecutor` first (like
        // the ptrace `Strace` tool's `inject`), so output buffers are populated
        // before rendering. Then record the *fully decoded* syscall — arguments
        // dereferenced from guest memory via `display_with_outputs` — rather
        // than the `Debug` form, which prints only the raw, undecoded argument
        // words and made KVM strace appear to have "zeroed" arguments. This
        // gives KVM strace the same real-argument fidelity as the ptrace and
        // SaBRe backends.
        let result = guest.inject(syscall).await;
        let formatted = format!("{}", syscall.display_with_outputs(&guest.memory()));
        eprintln!(
            "[kvm-strace] {formatted} = {}",
            result.unwrap_or_else(|errno| -(errno.into_raw() as i64)),
        );
        guest.send_rpc((name, formatted)).await;
        Ok(result?)
    }

    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        _tid: Pid,
        _global: &G,
        _thread_state: Self::ThreadState,
        _status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        Ok(())
    }
}
