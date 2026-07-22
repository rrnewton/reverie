/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::ffi::CStr;

use syscalls::Errno;
use syscalls::Sysno;
use syscalls::syscall3;

use super::paths;
use super::signal;
use crate::callbacks::CONTROLLED_EXIT_SIGNAL;

/// `readlink` needs to be handled in a special way. If we're trying to read
/// `/proc/self/exe`, then we can't return the path to the sabre executable. We
/// need to replace it with the path to the real binary.
///
/// NOTE: This doesn't handle numerous other cases such as:
///  1. Using `readlinkat(-100, "/proc/self/exe", ...)`
///  2. Using `readlinkat(dir_fd, "exe", ...)`
///  3. Using `readlink("/proc/{pid}/exe", ...)`
pub fn sys_readlink(
    path: *const libc::c_char,
    buf: *mut libc::c_char,
    bufsize: usize,
) -> Result<usize, Errno> {
    if unsafe { CStr::from_ptr(path) }.to_bytes() == b"/proc/self/exe" {
        if buf.is_null() {
            return Err(Errno::EFAULT);
        }

        let client_path = paths::client_path();
        let len = client_path.to_bytes().len().min(bufsize);

        unsafe { core::ptr::copy_nonoverlapping(client_path.as_ptr(), buf, len) };

        Ok(len)
    } else {
        unsafe {
            syscall3(
                Sysno::readlink,
                path as usize,
                buf as usize,
                bufsize as usize,
            )
        }
    }
}

/// SaBRe cannot atomically replace an image and keep its plugin injected. The
/// previous loader rewrite destroyed the guest image before malformed targets,
/// bad interpreters, and permission errors were known. Reject exec before
/// dereferencing guest pointers until the loader can preserve kernel semantics.
pub fn sys_execve(
    _filename: *const libc::c_char,
    _argv: *const *const libc::c_char,
    _envp: *const *const libc::c_char,
) -> Result<usize, Errno> {
    Err(Errno::ENOSYS)
}
pub fn sys_execveat() -> Result<usize, Errno> {
    Err(Errno::ENOSYS)
}

/// glibc defines this to be much larger than what the kernel accepts. Since we
/// have to make a direct syscall to `rt_sigaction`, we must use the same sigset
/// as the kernel does.
///
/// The kernel currently uses 64 bits for the sigset. See:
/// https://elixir.bootlin.com/linux/v5.17.5/source/arch/x86/include/uapi/asm/signal.h#L17
#[derive(Copy, Clone, Default)]
#[repr(C)]
pub struct KernelSigset(u64);

impl KernelSigset {
    /// Check if the sigset contains a signal.
    #[allow(unused)]
    pub fn contains(&self, sig: libc::c_int) -> bool {
        let mask = sigmask(sig);
        (self.0 & mask) == mask
    }

    /// Removes the given signal from the sigset.
    pub fn remove(&mut self, sig: libc::c_int) {
        let mask = sigmask(sig);
        self.0 &= !(mask as u64)
    }
}

/// The x86_64 kernel ABI representation of `struct sigaction`. libc's
/// representation embeds its much larger userspace `sigset_t`, so it cannot be
/// passed directly to the raw syscall.
#[derive(Copy, Clone)]
#[repr(C)]
pub struct KernelSigaction {
    handler: libc::sighandler_t,
    flags: libc::c_ulong,
    restorer: usize,
    mask: KernelSigset,
}

const _: () = assert!(core::mem::size_of::<KernelSigaction>() == 32);

impl KernelSigaction {
    fn from_libc(action: libc::sigaction) -> Self {
        let mask = unsafe { *(&action.sa_mask as *const libc::sigset_t as *const KernelSigset) };
        let restorer = action
            .sa_restorer
            .map(|function| function as usize)
            .unwrap_or(0);

        Self {
            handler: action.sa_sigaction,
            flags: action.sa_flags as libc::c_ulong,
            restorer,
            mask,
        }
    }

    fn into_libc(self) -> libc::sigaction {
        let mut action: libc::sigaction = unsafe { core::mem::zeroed() };
        action.sa_sigaction = self.handler;
        action.sa_flags = self.flags as libc::c_int;
        action.sa_restorer = if self.restorer == 0 {
            None
        } else {
            Some(unsafe { core::mem::transmute::<usize, extern "C" fn()>(self.restorer) })
        };
        unsafe {
            *(&mut action.sa_mask as *mut libc::sigset_t as *mut KernelSigset) = self.mask;
        }
        action
    }
}

#[inline]
fn sigmask(sig: libc::c_int) -> u64 {
    // wrapping_sub is safe because signal numbers start at 1.
    1 << (sig as u64).wrapping_sub(1)
}

/// Preserve Reverie's central handler while exposing the guest-facing action
/// through the normal `rt_sigaction` ABI. Signals not mediated by Reverie are
/// passed directly to the kernel.
pub fn sys_rt_sigaction(
    signal_value: libc::c_int,
    new_action_ptr: *const KernelSigaction,
    old_action_ptr: *mut KernelSigaction,
    sigset_size: usize,
) -> Result<usize, Errno> {
    if sigset_size != core::mem::size_of::<KernelSigset>() {
        return Err(Errno::EINVAL);
    }

    if !signal::handles_signal(signal_value) {
        return unsafe {
            syscalls::syscall4(
                Sysno::rt_sigaction,
                signal_value as usize,
                new_action_ptr as usize,
                old_action_ptr as usize,
                sigset_size,
            )
        };
    }

    // Read the new action first because Linux permits the old and new pointers
    // to alias.
    let new_action = if new_action_ptr.is_null() {
        None
    } else {
        Some(unsafe { *new_action_ptr })
    };

    let old_action = match new_action {
        Some(action) => signal::register_guest_handler(signal_value, action.into_libc())?,
        None => signal::registered_guest_handler(signal_value)?,
    };

    if !old_action_ptr.is_null() {
        unsafe {
            *old_action_ptr = KernelSigaction::from_libc(old_action);
        }
    }

    Ok(0)
}

/// rt_sigprocmask needs special handling because if the guest tries to set a
/// signal mask that prevents our control signal from being received by a
/// thread, we are going to create and pass our own sigset that only differs
/// from the client's in that it does not suppress our control signal
pub fn sys_rt_sigprocmask(
    operation: libc::c_int,
    sigset_ptr: *const KernelSigset,
    prev_sigset_ptr: *mut KernelSigset,
    // Should always 8 for x86_64
    sigset_size: usize,
) -> Result<usize, Errno> {
    if sigset_ptr.is_null() {
        return unsafe {
            syscalls::syscall4(
                Sysno::rt_sigprocmask,
                operation as usize,
                sigset_ptr as usize,
                prev_sigset_ptr as usize,
                sigset_size as usize,
            )
        };
    }

    let mut new_sigset = unsafe { *sigset_ptr };

    if matches!(operation, libc::SIG_SETMASK | libc::SIG_BLOCK) {
        new_sigset.remove(CONTROLLED_EXIT_SIGNAL);
    }

    unsafe {
        syscalls::syscall4(
            Sysno::rt_sigprocmask,
            operation as usize,
            &new_sigset as *const _ as usize,
            prev_sigset_ptr as usize,
            sigset_size as usize,
        )
    }
}

#[inline]
pub fn is_vfork(sys_no: Sysno, arg1: usize) -> bool {
    const VFORK_FLAGS: usize = (libc::CLONE_VM | libc::CLONE_VFORK | libc::SIGCHLD) as usize;
    sys_no == Sysno::vfork || (sys_no == Sysno::clone && (arg1 & VFORK_FLAGS == VFORK_FLAGS))
}

#[cfg(test)]
mod exec_tests {
    use super::*;

    #[test]
    fn execve_rejects_all_forms_without_dereferencing_guest_pointers() {
        assert_eq!(
            sys_execve(core::ptr::null(), core::ptr::null(), core::ptr::null()),
            Err(Errno::ENOSYS)
        );
        assert_eq!(
            sys_execve(
                usize::MAX as *const libc::c_char,
                usize::MAX as *const *const libc::c_char,
                usize::MAX as *const *const libc::c_char,
            ),
            Err(Errno::ENOSYS)
        );
        let relative = b"relative-program\0";
        let custom_arg0 = b"custom-argv-zero\0";
        let argv = [
            custom_arg0.as_ptr() as *const libc::c_char,
            core::ptr::null(),
        ];
        assert_eq!(
            sys_execve(
                relative.as_ptr() as *const libc::c_char,
                argv.as_ptr(),
                core::ptr::null(),
            ),
            Err(Errno::ENOSYS)
        );

        for unsupported in [
            b"non-executable\0".as_slice(),
            b"malformed-elf\0".as_slice(),
        ] {
            assert_eq!(
                sys_execve(
                    unsupported.as_ptr() as *const libc::c_char,
                    argv.as_ptr(),
                    core::ptr::null(),
                ),
                Err(Errno::ENOSYS)
            );
        }

        assert_eq!(sys_execveat(), Err(Errno::ENOSYS));
    }
}
