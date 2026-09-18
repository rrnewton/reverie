/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Signal multiplexing between the runtime and the guest.
//!
//! The runtime reserves `SIGSYS` for its own syscall trap. The guest must not
//! be able to change the disposition, mask, or alternate stack of a reserved
//! signal, or it could displace the trap and either crash (default `SIGSYS`
//! action) or blind the runtime. The dispatcher enforces this via
//! [`is_reserved`]; the runtime installs the handler here.
//!
//! All other signals belong to the guest and flow through untouched. When the
//! runtime later gains a hosted tool that itself wants signal events, this is
//! the single place that decides which signals are runtime-private.
//!
//! Runtime handlers use raw kernel `rt_sigaction`, rather than glibc's wrapper,
//! so the installed `SA_RESTORER` is exactly the assembly restorer authorized
//! by the seccomp filter.

use std::io;
use std::ptr;

/// Linux x86-64 `SA_RESTORER`; intentionally absent from glibc's public API.
const SA_RESTORER: libc::c_ulong = 0x0400_0000;
const KERNEL_SIGSET_SIZE: usize = core::mem::size_of::<u64>();
const SUPPORTED_RUNTIME_HANDLER_FLAGS: libc::c_int =
    libc::SA_SIGINFO | libc::SA_RESTART | libc::SA_ONSTACK;

/// The kernel's x86-64 `struct sigaction` layout. This is not glibc's larger
/// userspace `sigaction`, whose field order and 1024-bit mask are incompatible
/// with a raw `rt_sigaction` syscall.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct KernelSigaction {
    handler: usize,
    flags: libc::c_ulong,
    restorer: usize,
    mask: u64,
}

const _: () = {
    assert!(core::mem::size_of::<KernelSigaction>() == 32);
    assert!(core::mem::align_of::<KernelSigaction>() == 8);
    assert!(core::mem::offset_of!(KernelSigaction, handler) == 0);
    assert!(core::mem::offset_of!(KernelSigaction, flags) == 8);
    assert!(core::mem::offset_of!(KernelSigaction, restorer) == 16);
    assert!(core::mem::offset_of!(KernelSigaction, mask) == 24);
};

/// Signals the runtime reserves for itself. The guest may not reconfigure these.
pub const RESERVED_SIGNALS: &[i32] = &[libc::SIGSYS];

/// Whether `signal` is reserved by the runtime.
pub fn is_reserved(signal: i32) -> bool {
    RESERVED_SIGNALS.contains(&signal)
}

fn runtime_sigaction(
    handler: unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
    flags: libc::c_int,
) -> io::Result<KernelSigaction> {
    if flags & libc::SA_SIGINFO == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime signal handler must use SA_SIGINFO",
        ));
    }
    if flags & !SUPPORTED_RUNTIME_HANDLER_FLAGS != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported runtime signal-handler flags",
        ));
    }
    Ok(KernelSigaction {
        handler: handler as *const () as usize,
        flags: flags as libc::c_ulong | SA_RESTORER,
        restorer: crate::trap::rt_sigreturn_restorer_address(),
        mask: 0,
    })
}

/// Install a runtime-owned handler through raw `rt_sigaction` with exact flags.
///
/// `flags` must contain `SA_SIGINFO` and may additionally contain only
/// `SA_RESTART` and `SA_ONSTACK`. The installer always adds `SA_RESTORER` with
/// the runtime's exact assembly restorer; callers cannot substitute one.
/// LiteInst's SIGTRAP router uses `SA_SIGINFO | SA_RESTART` through this API.
///
/// # Safety
///
/// `handler` must be a valid `SA_SIGINFO` signal handler that is
/// async-signal-safe. Installs process-global disposition.
pub unsafe fn install_runtime_siginfo_handler_with_flags(
    signal: libc::c_int,
    handler: unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
    flags: libc::c_int,
) -> io::Result<()> {
    let action = runtime_sigaction(handler, flags)?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_rt_sigaction,
            signal,
            ptr::addr_of!(action),
            ptr::null_mut::<KernelSigaction>(),
            KERNEL_SIGSET_SIZE,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Install a runtime-owned `SA_SIGINFO` handler through raw `rt_sigaction`.
///
/// `on_alt_stack` requests `SA_ONSTACK`, so the handler runs on the alternate
/// signal stack configured with [`install_alt_stack`]. That keeps the trap
/// working even when the guest's own stack is nearly exhausted.
/// Every returning handler uses the runtime's exact assembly restorer and
/// therefore reaches `rt_sigreturn` only through the seccomp-authorized gate.
/// As with the ordinary hidden syscall gates, the trusted-guest support model
/// excludes a crafted direct transfer into that runtime text; exact-IP matching
/// is not a control-flow-integrity claim.
///
/// # Safety
///
/// `handler` must be a valid `SA_SIGINFO` signal handler that is
/// async-signal-safe. Installs process-global disposition.
pub unsafe fn install_runtime_siginfo_handler(
    signal: libc::c_int,
    handler: unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
    on_alt_stack: bool,
) -> io::Result<()> {
    let flags = libc::SA_SIGINFO | if on_alt_stack { libc::SA_ONSTACK } else { 0 };
    unsafe { install_runtime_siginfo_handler_with_flags(signal, handler, flags) }
}

/// Install `handler` for the runtime-reserved `SIGSYS` signal.
///
/// # Safety
///
/// See [`install_runtime_siginfo_handler`].
pub unsafe fn install_sigsys_handler(
    handler: unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
    on_alt_stack: bool,
) -> io::Result<()> {
    unsafe { install_runtime_siginfo_handler(libc::SIGSYS, handler, on_alt_stack) }
}

/// Allocate and register an alternate signal stack for the current thread.
///
/// Returns the leaked stack memory's base pointer (kept alive for process
/// lifetime). Using an alternate stack means a trapped syscall issued near the
/// guest's stack limit still has room for the handler frame.
///
/// # Safety
///
/// Registers process/thread signal-stack state; call before installing the
/// handler on a thread.
pub unsafe fn install_alt_stack() -> io::Result<*mut libc::c_void> {
    let size = (libc::SIGSTKSZ).max(64 * 1024);
    // Leak a Vec as the stack backing store; it lives for the process lifetime.
    let mut backing = vec![0_u8; size].into_boxed_slice();
    let base = backing.as_mut_ptr().cast::<libc::c_void>();
    core::mem::forget(backing);
    let stack = libc::stack_t {
        ss_sp: base,
        ss_flags: 0,
        ss_size: size,
    };
    if unsafe { libc::sigaltstack(&stack, ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn test_handler(
        _signal: libc::c_int,
        _info: *mut libc::siginfo_t,
        _context: *mut libc::c_void,
    ) {
    }

    #[test]
    fn sigsys_is_reserved_but_others_are_not() {
        assert!(is_reserved(libc::SIGSYS));
        assert!(!is_reserved(libc::SIGINT));
        assert!(!is_reserved(libc::SIGTERM));
        assert!(!is_reserved(libc::SIGCHLD));
    }

    #[test]
    fn raw_runtime_action_uses_exact_restorer_and_kernel_layout() {
        for flags in [
            libc::SA_SIGINFO,
            libc::SA_SIGINFO | libc::SA_ONSTACK,
            libc::SA_SIGINFO | libc::SA_RESTART,
            libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESTART,
        ] {
            let action = runtime_sigaction(test_handler, flags).unwrap();
            assert_eq!(action.handler, test_handler as *const () as usize);
            assert_eq!(
                action.restorer,
                crate::trap::rt_sigreturn_restorer_address()
            );
            assert_eq!(action.mask, 0);
            assert_ne!(action.flags & SA_RESTORER, 0);
            assert_ne!(action.flags & libc::SA_SIGINFO as libc::c_ulong, 0);
            assert_eq!(
                action.flags & libc::SA_ONSTACK as libc::c_ulong != 0,
                flags & libc::SA_ONSTACK != 0
            );
            assert_eq!(
                action.flags & libc::SA_RESTART as libc::c_ulong != 0,
                flags & libc::SA_RESTART != 0
            );
        }
        assert_eq!(KERNEL_SIGSET_SIZE, 8);
    }

    #[test]
    fn raw_runtime_action_rejects_missing_siginfo_or_extra_flags() {
        assert_eq!(
            runtime_sigaction(test_handler, libc::SA_RESTART)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            runtime_sigaction(test_handler, libc::SA_SIGINFO | libc::SA_NODEFER)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
