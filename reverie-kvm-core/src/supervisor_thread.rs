/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The host threads that drive guest processes and guest threads.
//!
//! A supervisor thread runs one guest process's or one guest thread's whole
//! loop: vCPU entries, LinuxEmu's syscall emulation and the Tool's callbacks,
//! all under one `futures::executor::block_on`. Its stack therefore holds the
//! backend's process-loop future and every frame a Tool callback needs.
//! The supervisor threads are the forked-process thread
//! (`reverie-kvm-process-<pid>`) and the guest-thread workers
//! (`reverie-kvm-guest-<tid>`, Tool-owned or host-owned).
//!
//! Without an explicit size such a thread would get whatever
//! `RUST_MIN_STACK` says, or Rust's 2 MiB default, which a debug build
//! overflows.

use std::sync::OnceLock;

/// The stack every supervisor thread gets, unless `RUST_MIN_STACK` asks for
/// more (see [`stack_bytes`]).
pub const KVM_SUPERVISOR_THREAD_STACK_BYTES: usize = 16 << 20;

/// The stack size a supervisor thread is spawned with: the larger of
/// [`KVM_SUPERVISOR_THREAD_STACK_BYTES`] and `RUST_MIN_STACK`, read once, as
/// Rust's standard library reads it. An explicit size replaces
/// `RUST_MIN_STACK` for the thread it is given to, so taking the larger keeps
/// that variable able to raise the stack for a Tool that needs more, and
/// never lets it shrink the stack below what the backend needs.
pub fn stack_bytes() -> usize {
    if let Some(bytes) = crate::stack_probe::override_bytes() {
        return bytes;
    }
    static RUST_MIN_STACK: OnceLock<Option<usize>> = OnceLock::new();
    let requested = *RUST_MIN_STACK.get_or_init(|| {
        std::env::var_os("RUST_MIN_STACK").and_then(|value| value.to_str()?.parse().ok())
    });
    requested.map_or(KVM_SUPERVISOR_THREAD_STACK_BYTES, |bytes| {
        bytes.max(KVM_SUPERVISOR_THREAD_STACK_BYTES)
    })
}

/// A builder for the supervisor thread named `name`, with [`stack_bytes`] of
/// stack.
pub fn builder(name: String) -> std::thread::Builder {
    std::thread::Builder::new()
        .name(name)
        .stack_size(stack_bytes())
}
