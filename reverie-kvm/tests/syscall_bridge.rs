/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Phase 0 spike: a guest syscall is not merely *observed* but *serviced* by
//! host Rust code, and the guest observes the real result in `rax`.
//!
//! Each test forwards one syscall out of a KVM guest through the transport
//! hypercall to [`HostSyscallDispatch`] (the gVisor `SyscallTable.External`
//! analog), which runs the real host syscall and returns its result to the
//! guest. This is the concrete milestone the gVisor architecture doc calls the
//! "Phase 0 Sentry-bridge spike".

#![cfg(target_arch = "x86_64")]

use kvm_ioctls::Kvm;
use reverie_kvm::GuestMemory;
use reverie_kvm::HostSyscallDispatch;
use reverie_kvm::KvmBackend;
use reverie_kvm::SyscallRequest;

const MEMORY_SIZE: usize = 0x10_000;
const ENTRY_POINT: u64 = 0x1000;
const FRAME_ADDRESS: u64 = 0x2000;
const SCRATCH_ADDRESS: u64 = 0x3000;

fn kvm_is_unavailable(error: &kvm_ioctls::Error) -> bool {
    matches!(error.errno(), libc::ENOENT | libc::EACCES | libc::EPERM)
}

/// Reads the full 64-bit syscall result the backend marshalled back through the
/// frame's return slot (the width-faithful channel; see `run_with`).
fn frame_return(backend: &KvmBackend) -> i64 {
    let mut bytes = [0u8; 8];
    backend
        .memory()
        .read(
            FRAME_ADDRESS + SyscallRequest::RETURN_SLOT_OFFSET,
            &mut bytes,
        )
        .unwrap();
    i64::from_le_bytes(bytes)
}

/// Returns `Some(backend)` when KVM is usable, or `None` (with a skip message)
/// when the device is unavailable, mirroring the existing vmcall test.
fn backend_or_skip(name: &str) -> Option<KvmBackend> {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping {name}: cannot open /dev/kvm: {error}");
            return None;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }
    Some(KvmBackend::new(MEMORY_SIZE).unwrap())
}

/// Stateless syscall: the guest's `getpid` returns the *host* process id.
#[test]
fn getpid_is_serviced_by_the_host() {
    let Some(mut backend) = backend_or_skip("getpid bridge test") else {
        return;
    };
    backend
        .install_syscall(
            ENTRY_POINT,
            FRAME_ADDRESS,
            SyscallRequest::new(libc::SYS_getpid as u64, [0; 6]),
        )
        .unwrap();

    let mut dispatch = HostSyscallDispatch::new();
    let guest_rax = backend.run_with(&mut dispatch).unwrap();

    assert_eq!(guest_rax, std::process::id() as u64);
}

/// Pointer-in (gVisor `CopyIn`): the host reads the guest buffer and performs a
/// real `write`, and the guest sees the byte count returned.
#[test]
fn write_reads_guest_memory_and_returns_count() {
    let Some(mut backend) = backend_or_skip("write bridge test") else {
        return;
    };
    let message = b"reverie-kvm bridge spike\n";
    backend
        .memory_mut()
        .write(SCRATCH_ADDRESS, message)
        .unwrap();
    backend
        .install_syscall(
            ENTRY_POINT,
            FRAME_ADDRESS,
            SyscallRequest::new(
                libc::SYS_write as u64,
                [
                    libc::STDERR_FILENO as u64,
                    SCRATCH_ADDRESS,
                    message.len() as u64,
                    0,
                    0,
                    0,
                ],
            ),
        )
        .unwrap();

    let mut dispatch = HostSyscallDispatch::new();
    let guest_rax = backend.run_with(&mut dispatch).unwrap();

    assert_eq!(guest_rax, message.len() as u64);
}

/// A guest `write` to a disallowed descriptor is rejected with `-EBADF`,
/// showing the spike does not hand arbitrary host fds to the guest.
#[test]
fn write_to_unlisted_fd_is_rejected() {
    let Some(mut backend) = backend_or_skip("write-guard bridge test") else {
        return;
    };
    backend.memory_mut().write(SCRATCH_ADDRESS, b"x").unwrap();
    backend
        .install_syscall(
            ENTRY_POINT,
            FRAME_ADDRESS,
            // fd 7 is not stdout/stderr.
            SyscallRequest::new(libc::SYS_write as u64, [7, SCRATCH_ADDRESS, 1, 0, 0, 0]),
        )
        .unwrap();

    let mut dispatch = HostSyscallDispatch::new();
    let guest_rax = backend.run_with(&mut dispatch).unwrap();

    // The register channel is 32-bit; the frame channel is full width.
    assert_eq!(guest_rax as u32 as i32, -libc::EBADF);
    assert_eq!(frame_return(&backend), -(libc::EBADF as i64));
}

/// Pointer-out (gVisor `CopyOut`): the host runs `clock_gettime` and serializes
/// the result back into guest memory, which the guest can then read.
#[test]
fn clock_gettime_writes_result_into_guest_memory() {
    let Some(mut backend) = backend_or_skip("clock_gettime bridge test") else {
        return;
    };
    backend
        .install_syscall(
            ENTRY_POINT,
            FRAME_ADDRESS,
            SyscallRequest::new(
                libc::SYS_clock_gettime as u64,
                [libc::CLOCK_MONOTONIC as u64, SCRATCH_ADDRESS, 0, 0, 0, 0],
            ),
        )
        .unwrap();

    let mut dispatch = HostSyscallDispatch::new();
    let guest_rax = backend.run_with(&mut dispatch).unwrap();
    assert_eq!(guest_rax, 0);

    let mut frame = [0u8; 16];
    backend.memory().read(SCRATCH_ADDRESS, &mut frame).unwrap();
    let tv_sec = i64::from_le_bytes(frame[..8].try_into().unwrap());
    let tv_nsec = i64::from_le_bytes(frame[8..].try_into().unwrap());
    assert!(tv_sec > 0, "CLOCK_MONOTONIC tv_sec should be positive");
    assert!((0..1_000_000_000).contains(&tv_nsec));
}

/// Anything outside the allowlist returns `-ENOSYS`: the bridge is a hook, not
/// the missing Linux-ABI substrate.
#[test]
fn unsupported_syscall_returns_enosys() {
    let Some(mut backend) = backend_or_skip("enosys bridge test") else {
        return;
    };
    backend
        .install_syscall(
            ENTRY_POINT,
            FRAME_ADDRESS,
            // openat is not in the spike allowlist.
            SyscallRequest::new(libc::SYS_openat as u64, [0; 6]),
        )
        .unwrap();

    let mut dispatch = HostSyscallDispatch::new();
    let guest_rax = backend.run_with(&mut dispatch).unwrap();

    assert_eq!(guest_rax as u32 as i32, -libc::ENOSYS);
    assert_eq!(frame_return(&backend), -(libc::ENOSYS as i64));
}

/// A plain closure can drive the same loop via the `SyscallHandler` blanket
/// impl — the interception point is a generic Reverie-style hook.
#[test]
fn closure_handler_can_service_syscalls() {
    let Some(mut backend) = backend_or_skip("closure handler test") else {
        return;
    };
    backend
        .install_syscall(
            ENTRY_POINT,
            FRAME_ADDRESS,
            SyscallRequest::new(libc::SYS_getpid as u64, [0; 6]),
        )
        .unwrap();

    let mut seen = None;
    let mut handler = |request: &SyscallRequest, _memory: &mut GuestMemory| -> i64 {
        seen = Some(request.number());
        4242
    };
    let guest_rax = backend.run_with(&mut handler).unwrap();

    assert_eq!(seen, Some(libc::SYS_getpid as u64));
    assert_eq!(guest_rax, 4242);
}
