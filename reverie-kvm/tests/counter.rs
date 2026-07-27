/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! End-to-end counter1/counter2 example Tool coverage over KVM.

#![cfg(target_arch = "x86_64")]

use kvm_ioctls::Kvm;
use reverie_examples::counter1;
use reverie_examples::counter2;
use reverie_kvm::KvmBackend;

const GUEST_MEMORY_BYTES: usize = 256 * 1024 * 1024;
const ECHO_SYSCALL_BASELINE: u64 = 114;

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

fn echo_backend() -> KvmBackend {
    let image = std::fs::read("/bin/echo").expect("/bin/echo must be available");
    let mut backend = KvmBackend::new(GUEST_MEMORY_BYTES).unwrap();
    backend
        .install_static_elf_with_context(
            &image,
            &["/bin/echo", "hello"],
            &["PATH=/usr/bin:/bin", "LANG=C.UTF-8"],
            std::path::Path::new("/"),
        )
        .unwrap();
    backend
}

#[test]
fn counter1_matches_ptrace_echo_baseline() {
    if !kvm_available("counter1_matches_ptrace_echo_baseline") {
        return;
    }

    let (state, code, stdout, stderr) = futures::executor::block_on(
        echo_backend().run_static_elf_with_tool::<counter1::CounterLocal>((), true),
    )
    .unwrap();

    assert_eq!(code, 0);
    assert_eq!(stdout, b"hello\n");
    assert!(stderr.is_empty());
    assert_eq!(state.num_syscalls(), ECHO_SYSCALL_BASELINE);
}

#[test]
fn counter2_matches_ptrace_echo_baseline_and_exit_lifecycle() {
    if !kvm_available("counter2_matches_ptrace_echo_baseline_and_exit_lifecycle") {
        return;
    }

    let (state, code, stdout, stderr) = futures::executor::block_on(
        echo_backend().run_static_elf_with_tool::<counter2::CounterLocal>((), true),
    )
    .unwrap();
    let inner = state.inner.lock().unwrap();

    assert_eq!(code, 0);
    assert_eq!(stdout, b"hello\n");
    assert!(stderr.is_empty());
    assert_eq!(inner.total_syscalls, ECHO_SYSCALL_BASELINE);
    assert_eq!(inner.exited_procs, 1);
    assert_eq!(inner.exited_threads, 1);
}
