/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#![cfg(target_arch = "x86_64")]

use kvm_ioctls::Kvm;
use reverie_kvm::KvmBackend;
use reverie_kvm::SyscallInfo;
use reverie_kvm::SyscallRequest;
use reverie_kvm::Sysno;

const MEMORY_SIZE: usize = 0x10_000;
const ENTRY_POINT: u64 = 0x1000;
const FRAME_ADDRESS: u64 = 0x2000;
const MESSAGE_ADDRESS: u64 = 0x3000;

fn kvm_is_unavailable(error: &kvm_ioctls::Error) -> bool {
    matches!(error.errno(), libc::ENOENT | libc::EACCES | libc::EPERM)
}

#[test]
fn identifies_unavailable_kvm_errors() {
    for errno in [libc::ENOENT, libc::EACCES, libc::EPERM] {
        let error = kvm_ioctls::Error::new(errno);
        assert!(kvm_is_unavailable(&error));
    }

    let error = kvm_ioctls::Error::new(libc::EINVAL);
    assert!(!kvm_is_unavailable(&error));
}

#[test]
fn guest_write_syscall_is_intercepted_via_vmcall() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM vmcall test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
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
            SyscallRequest::new(libc::SYS_write as u64, [1, MESSAGE_ADDRESS, 5, 0, 0, 0]),
        )
        .unwrap();

    let mut intercepted = None;
    backend
        .run(|syscall, memory| {
            let (number, args) = syscall.into_parts();
            let mut message = vec![0; args.arg2];
            memory.read(args.arg1 as u64, &mut message).unwrap();
            intercepted = Some((number, args.arg0, message));
            args.arg2 as i64
        })
        .unwrap();

    assert_eq!(intercepted, Some((Sysno::write, 1, b"hello".to_vec())));
}

#[test]
fn guest_program_routes_required_syscalls_via_vmcall() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM vmcall test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let expected = [
        Sysno::read,
        Sysno::write,
        Sysno::open,
        Sysno::close,
        Sysno::mmap,
        Sysno::munmap,
        Sysno::brk,
        Sysno::ioctl,
    ];
    let requests = expected.map(|number| SyscallRequest::new(number.id() as u64, [0; 6]));

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_syscalls(ENTRY_POINT, FRAME_ADDRESS, &requests)
        .unwrap();

    let mut intercepted = Vec::new();
    backend
        .run(|syscall, _memory| {
            intercepted.push(syscall.number());
            0
        })
        .unwrap();

    assert_eq!(intercepted, expected);
}
