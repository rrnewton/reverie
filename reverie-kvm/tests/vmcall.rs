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
use reverie_kvm::SyscallRequest;

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
        .run(|request, memory| {
            let mut message = vec![0; request.args()[2] as usize];
            memory.read(request.args()[1], &mut message).unwrap();
            intercepted = Some((request.number(), request.args()[0], message));
            request.args()[2] as i64
        })
        .unwrap();

    assert_eq!(
        intercepted,
        Some((libc::SYS_write as u64, 1, b"hello".to_vec()))
    );
}

// ---------------------------------------------------------------------------
// Minimal Guest-trait milestone: a real reverie::Tool (a syscall counter) runs
// over KvmGuest via KvmBackend::run_tool, and its `inject` actually executes the
// intercepted syscall.
// ---------------------------------------------------------------------------

use reverie::Error;
use reverie::Guest;
use reverie::Pid;
use reverie::Tool;
use reverie::syscalls::Syscall;

/// A tool that counts intercepted syscalls (per-thread) and forwards them.
#[derive(Debug, Default, Clone)]
struct CounterTool;

#[reverie::tool]
impl Tool for CounterTool {
    type GlobalState = ();
    type ThreadState = u64;

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        *guest.thread_state_mut() += 1;
        Ok(guest.inject(syscall).await?)
    }
}

#[test]
fn counter_tool_runs_on_kvm_guest() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM counter-tool test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    // A pipe lets us prove `inject` really executed the guest's write(2).
    let mut fds = [0 as libc::c_int; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe failed");
    let (read_fd, write_fd) = (fds[0], fds[1]);

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .memory_mut()
        .write(MESSAGE_ADDRESS, b"hello")
        .unwrap();
    backend
        .install_syscall(
            ENTRY_POINT,
            FRAME_ADDRESS,
            SyscallRequest::new(
                libc::SYS_write as u64,
                [write_fd as u64, MESSAGE_ADDRESS, 5, 0, 0, 0],
            ),
        )
        .unwrap();

    let tool = CounterTool;
    let mut count: u64 = 0;
    backend
        .run_tool(&tool, &(), &(), &mut count, Pid::from_raw(1))
        .unwrap();

    // The tool observed exactly one syscall on the KvmGuest.
    assert_eq!(count, 1, "counter tool did not observe the guest syscall");

    // inject() actually executed the write into the pipe.
    let mut buf = [0u8; 5];
    let n = unsafe { libc::read(read_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    assert_eq!(n, 5, "injected write did not deliver 5 bytes");
    assert_eq!(&buf, b"hello", "injected write delivered the wrong bytes");

    unsafe {
        libc::close(read_fd);
        libc::close(write_fd);
    }
}
