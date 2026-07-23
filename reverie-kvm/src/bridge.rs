/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A host-side syscall bridge for the KVM prototype.
//!
//! This is the reverie-kvm analog of gVisor's per-syscall interception hook
//! (`SyscallTable.External` and the `Task.doSyscall` site) and of Reverie's
//! [`Tool::handle_syscall_event`]. It sits *around* syscall dispatch: the guest
//! traps out via the syscall transport hypercall, and a [`SyscallHandler`]
//! decides what the syscall means and produces the value returned to the guest.
//!
//! What this bridge is **not** is the Linux ABI substrate — gVisor's Sentry
//! (`kernel/SyscallTable` implementations, `mm/`, `vfs2/`, `loader/`). That is
//! the large piece hermit's KVM path still lacks (see the crate README and
//! `ai_docs/kvm-syscall-bridge-spike.md`). A handler here
//! services one syscall at a time with no process, virtual-memory, signal, or
//! filesystem model behind it.
//!
//! [`Tool::handle_syscall_event`]: https://docs.rs/reverie/latest/reverie/trait.Tool.html

use crate::GuestMemory;
use crate::SyscallRequest;

/// A host-side handler for syscalls forwarded out of the guest.
///
/// This mirrors gVisor's `Context.Switch` returning `nil` (== "a syscall was
/// intercepted") feeding the per-syscall hook: the backend's run loop reports
/// each guest syscall and the handler returns the value to place in the guest's
/// result register. The Linux convention is a negative `errno` on failure and a
/// non-negative result on success.
///
/// `memory` is passed mutably so a handler can service pointer arguments in both
/// directions — reading an input buffer (gVisor `AddressSpaceIO::CopyIn`) or
/// writing an output buffer (`CopyOut`).
pub trait SyscallHandler {
    /// Handle one forwarded syscall and return the guest-visible result.
    fn handle(&mut self, request: &SyscallRequest, memory: &mut GuestMemory) -> i64;
}

/// A blanket impl so a plain closure can be used as a [`SyscallHandler`].
impl<F> SyscallHandler for F
where
    F: FnMut(&SyscallRequest, &mut GuestMemory) -> i64,
{
    fn handle(&mut self, request: &SyscallRequest, memory: &mut GuestMemory) -> i64 {
        self(request, memory)
    }
}

/// A minimal "Sentry bridge" spike: execute a small allowlist of forwarded
/// syscalls directly on the host and return the real result to the guest.
///
/// This is deliberately tiny. It demonstrates that a guest syscall can be
/// *serviced* (not merely observed) by host Rust code — the guest sees the
/// host's real pid, real clock, and real writes. Everything outside the
/// allowlist returns `-ENOSYS`, because servicing arbitrary Linux syscalls
/// requires the ABI substrate this crate does not have.
///
/// # Safety / scope
///
/// `write` is restricted to stdout/stderr so a guest cannot reach arbitrary
/// host descriptors through the spike. The allowlist is intentionally limited
/// to syscalls that are either stateless (`getpid`, `getuid`, `getgid`,
/// `gettid`) or that only touch a caller-provided buffer (`write`,
/// `clock_gettime`). This is a research vehicle, not a sandbox boundary.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostSyscallDispatch;

impl HostSyscallDispatch {
    /// Creates a dispatcher over the fixed spike allowlist.
    pub fn new() -> Self {
        Self
    }
}

impl SyscallHandler for HostSyscallDispatch {
    fn handle(&mut self, request: &SyscallRequest, memory: &mut GuestMemory) -> i64 {
        let args = request.args();
        match request.number() as i64 {
            // Stateless identity syscalls: forwarded verbatim to the host.
            libc::SYS_getpid => (unsafe { libc::getpid() }) as i64,
            libc::SYS_getuid => (unsafe { libc::getuid() }) as i64,
            libc::SYS_getgid => (unsafe { libc::getgid() }) as i64,
            libc::SYS_gettid => unsafe { libc::syscall(libc::SYS_gettid) },

            // Pointer-in (CopyIn): read the guest buffer, then write it out.
            // Restricted to stdout/stderr for the spike.
            libc::SYS_write => {
                let fd = args[0] as i32;
                if fd != libc::STDOUT_FILENO && fd != libc::STDERR_FILENO {
                    return -libc::EBADF as i64;
                }
                let count = args[2] as usize;
                let mut buffer = vec![0u8; count];
                if memory.read(args[1], &mut buffer).is_err() {
                    return -libc::EFAULT as i64;
                }
                // SAFETY: buffer is a valid host allocation of `count` bytes and
                // `fd` is a real host descriptor (stdout/stderr).
                let written = unsafe { libc::write(fd, buffer.as_ptr().cast(), count) };
                if written < 0 {
                    -errno() as i64
                } else {
                    written as i64
                }
            }

            // Pointer-out (CopyOut): run the host syscall into a local struct,
            // then serialize it back into guest memory.
            libc::SYS_clock_gettime => {
                let mut ts = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                // SAFETY: &mut ts is a valid, correctly-typed timespec pointer.
                let rc = unsafe { libc::clock_gettime(args[0] as libc::clockid_t, &mut ts) };
                if rc != 0 {
                    return -errno() as i64;
                }
                // tv_sec/tv_nsec are i64 on x86-64 (this crate is x86-64 only).
                let mut frame = [0u8; 16];
                frame[..8].copy_from_slice(&ts.tv_sec.to_le_bytes());
                frame[8..].copy_from_slice(&ts.tv_nsec.to_le_bytes());
                if memory.write(args[1], &frame).is_err() {
                    return -libc::EFAULT as i64;
                }
                0
            }

            // Everything else needs the missing Linux-ABI substrate.
            _ => -libc::ENOSYS as i64,
        }
    }
}

/// Reads the current thread's `errno` after a failed libc call.
fn errno() -> i32 {
    // SAFETY: __errno_location always returns a valid pointer to thread-local
    // errno storage.
    unsafe { *libc::__errno_location() }
}
