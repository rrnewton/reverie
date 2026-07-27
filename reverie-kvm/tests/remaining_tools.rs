/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! KVM coverage for Reverie's noop, chaos, and chunky-print example tools.
//!
//! These tests run the original example Tool types through KVM's generic
//! `run_with_tool` path. They require a working `/dev/kvm`; unavailable KVM
//! hosts receive the same explicit skip treatment as the other backend tests.

#![cfg(target_arch = "x86_64")]

use std::sync::Arc;
use std::sync::Mutex;

use kvm_ioctls::Kvm;
use reverie_examples::chaos::ChaosOpts;
use reverie_examples::chaos::ChaosTool;
use reverie_examples::chunky_print::ChunkyPrintLocal;
use reverie_examples::noop::NoopTool;
use reverie_kvm::GuestMemory;
use reverie_kvm::KvmBackend;
use reverie_kvm::SyscallRequest;

const MEMORY_SIZE: usize = 0x10_000;
const ENTRY_POINT: u64 = 0x1000;
const FRAME_ADDRESS: u64 = 0x2000;
const MESSAGE_ADDRESS: u64 = 0x8000;

fn kvm_is_unavailable(error: &kvm_ioctls::Error) -> bool {
    matches!(error.errno(), libc::ENOENT | libc::EACCES | libc::EPERM)
}

fn kvm_available(test: &str) -> bool {
    match Kvm::new() {
        Ok(_) => true,
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping {test}: cannot open /dev/kvm: {error}");
            false
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }
}

#[test]
fn noop_forwards_unsubscribed_syscalls_unchanged() {
    if !kvm_available("noop_forwards_unsubscribed_syscalls_unchanged") {
        return;
    }

    let requests = [
        SyscallRequest::new(libc::SYS_getpid as u64, [0; 6]),
        SyscallRequest::new(libc::SYS_close as u64, [9, 0, 0, 0, 0, 0]),
    ];
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_syscalls(ENTRY_POINT, FRAME_ADDRESS, &requests)
        .unwrap();

    let executed = Arc::new(Mutex::new(Vec::new()));
    let executor_seen = executed.clone();
    futures::executor::block_on(backend.run_with_tool::<NoopTool, _>(
        (),
        move |request: &SyscallRequest, _memory: &GuestMemory| {
            executor_seen.lock().unwrap().push(*request);
            0
        },
    ))
    .unwrap();

    assert_eq!(*executed.lock().unwrap(), requests);
}

#[test]
fn chaos_restarts_and_shortens_reads() {
    if !kvm_available("chaos_restarts_and_shortens_reads") {
        return;
    }

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .memory_mut()
        .write(MESSAGE_ADDRESS, b"hello")
        .unwrap();
    backend
        .install_syscall(
            ENTRY_POINT,
            FRAME_ADDRESS,
            SyscallRequest::new(libc::SYS_read as u64, [0, MESSAGE_ADDRESS, 5, 0, 0, 0]),
        )
        .unwrap();

    let executed_lengths = Arc::new(Mutex::new(Vec::new()));
    let executor_seen = executed_lengths.clone();
    futures::executor::block_on(backend.run_with_tool::<ChaosTool, _>(
        ChaosOpts::default(),
        move |request: &SyscallRequest, _memory: &GuestMemory| {
            assert_eq!(request.number(), libc::SYS_read as u64);
            executor_seen.lock().unwrap().push(request.args()[2]);
            request.args()[2] as i64
        },
    ))
    .unwrap();

    assert_eq!(*executed_lengths.lock().unwrap(), vec![1]);
}

#[test]
fn chunky_print_buffers_stdout_and_stderr() {
    if !kvm_available("chunky_print_buffers_stdout_and_stderr") {
        return;
    }

    let requests = [
        SyscallRequest::new(libc::SYS_write as u64, [1, MESSAGE_ADDRESS, 5, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_write as u64, [2, MESSAGE_ADDRESS, 5, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_close as u64, [9, 0, 0, 0, 0, 0]),
    ];
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .memory_mut()
        .write(MESSAGE_ADDRESS, b"hello")
        .unwrap();
    backend
        .install_syscalls(ENTRY_POINT, FRAME_ADDRESS, &requests)
        .unwrap();

    let executed = Arc::new(Mutex::new(Vec::new()));
    let executor_seen = executed.clone();
    let global_state = futures::executor::block_on(backend.run_with_tool::<ChunkyPrintLocal, _>(
        (),
        move |request: &SyscallRequest, _memory: &GuestMemory| {
            executor_seen.lock().unwrap().push(*request);
            0
        },
    ))
    .unwrap();

    assert_eq!(
        *executed.lock().unwrap(),
        vec![SyscallRequest::new(
            libc::SYS_close as u64,
            [9, 0, 0, 0, 0, 0],
        )],
    );

    assert_eq!(global_state.buffered_bytes(), 10);
    global_state.flush().unwrap();
    assert_eq!(global_state.buffered_bytes(), 0);
}
