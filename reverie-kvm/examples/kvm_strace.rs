/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A minimal strace-style Reverie tool driven by the KVM backend.
//!
//! This is the KVM analogue of `reverie-examples/strace_minimal.rs`. Instead of
//! `reverie_ptrace::TracerBuilder`, it installs a short guest program of
//! `vmcall`-based syscalls into a `KvmBackend` and dispatches each intercepted
//! syscall through a normal `reverie::Tool`. The tool prints the typed syscall
//! (with decoded outputs) and tail-injects it; the backend routes the injected
//! syscall to the provided `SyscallExecutor`, which stands in for the Linux
//! kernel this prototype does not yet have.
//!
//! Run with `/dev/kvm` accessible:
//!   cargo run -p reverie-kvm --example kvm_strace
//! Expected trace (one line per intercepted syscall), e.g.:
//!   [kvm-strace tid 0] write(1, "hello", 5) = 5
//!   [kvm-strace tid 0] close(1) = 0

#![cfg(target_arch = "x86_64")]

use kvm_ioctls::Kvm;
use reverie::Error;
use reverie::Guest;
use reverie::Tool;
use reverie::syscalls::Displayable;
use reverie::syscalls::Syscall;
use reverie_kvm::GuestMemory;
use reverie_kvm::KvmBackend;
use reverie_kvm::SyscallRequest;

const MEMORY_SIZE: usize = 0x10_000;
const ENTRY_POINT: u64 = 0x1000;
const FRAME_ADDRESS: u64 = 0x2000;
const MESSAGE_ADDRESS: u64 = 0x3000;

/// A stateless tool that prints every intercepted syscall and lets it run.
#[derive(Default)]
struct KvmStraceTool {}

#[reverie::tool]
impl Tool for KvmStraceTool {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        // `display_with_outputs` decodes typed arguments and reads guest memory
        // (e.g. the write buffer) so the trace shows real values, exactly like
        // the ptrace-backed strace example.
        eprintln!(
            "[kvm-strace tid {}] {}",
            guest.tid(),
            syscall.display_with_outputs(&guest.memory()),
        );
        // Forward the syscall unchanged; the backend routes it to the executor.
        guest.tail_inject(syscall).await
    }
}

fn main() {
    // `/dev/kvm` is a host capability; skip cleanly when it is unavailable
    // (containers, CI without the device, missing permissions).
    match Kvm::new() {
        Ok(_) => {}
        Err(error) => {
            eprintln!("kvm-strace: /dev/kvm unavailable ({error}); nothing to trace.");
            return;
        }
    }

    let mut backend = KvmBackend::new(MEMORY_SIZE).expect("create KvmBackend");
    backend
        .memory_mut()
        .write(MESSAGE_ADDRESS, b"hello")
        .expect("stage guest message");

    // Install a short program of vmcall syscalls for the tool to trace.
    let requests = [
        SyscallRequest::new(libc::SYS_write as u64, [1, MESSAGE_ADDRESS, 5, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_close as u64, [1, 0, 0, 0, 0, 0]),
    ];
    backend
        .install_syscalls(ENTRY_POINT, FRAME_ADDRESS, &requests)
        .expect("install guest syscalls");

    eprintln!("kvm-strace: running guest; tracing syscalls via vmcall...");
    futures::executor::block_on(backend.run_with_tool::<KvmStraceTool, _>(
        (),
        // Stand-in "kernel" for tail-injected syscalls: return a plausible
        // result so the trace shows a return value.
        |request: &SyscallRequest, _memory: &GuestMemory| -> i64 {
            if request.number() == libc::SYS_write as u64 {
                request.args()[2] as i64 // bytes written
            } else {
                0
            }
        },
    ))
    .expect("run KVM guest under strace tool");
    eprintln!("kvm-strace: done.");
}
