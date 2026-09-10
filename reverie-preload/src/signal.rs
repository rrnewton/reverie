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

use std::io;
use std::ptr;

use crate::trap;
use crate::user_dispatch::syscall_result;

pub mod native_frame;
pub mod owned_trace;
mod runtime_owned;
pub use runtime_owned::RuntimeSignal;
pub use runtime_owned::configure_runtime_signals;
pub use runtime_owned::runtime_handler_mask;
pub use runtime_owned::runtime_ordinary_mask;
pub use runtime_owned::runtime_signal_descriptors;
pub use runtime_owned::runtime_signal_mask;
pub use runtime_owned::runtime_signals_configured;

/// Sources which must stay unblocked in guest masks. Unlike returning sources,
/// an owned synchronous trace remains blocked by runtime_handler_mask.
pub fn required_runtime_signal_mask() -> u64 {
    runtime_signal_mask()
        | if owned_trace::configured() {
            1 << (libc::SIGTRAP - 1)
        } else {
            0
        }
}

#[repr(C)]
#[derive(Default)]
struct KernelSignalAction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

fn current_sigsys_action() -> io::Result<KernelSignalAction> {
    let mut action = KernelSignalAction::default();
    syscall_result(unsafe {
        trap::raw_syscall6(
            libc::SYS_rt_sigaction,
            [libc::SIGSYS as u64, 0, (&raw mut action) as u64, 8, 0, 0],
        )
    })?;
    Ok(action)
}

pub(crate) unsafe fn install_user_dispatch_handler(on_alt_stack: bool) -> io::Result<()> {
    let current = current_sigsys_action()?;
    if current.handler != libc::SIG_DFL
        && current.handler != trap::user_dispatch_handler as *const () as usize
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "SIGSYS already has a different handler",
        ));
    }
    let action = KernelSignalAction {
        handler: trap::user_dispatch_handler as *const () as usize,
        flags: (libc::SA_SIGINFO | if on_alt_stack { libc::SA_ONSTACK } else { 0 }) as u64
            | 0x04000000,
        restorer: trap::trusted_sigreturn_restorer as *const () as usize,
        mask: runtime_handler_mask(),
    };
    unsafe { runtime_owned::install_runtime_signals()? };
    unsafe { owned_trace::install()? };
    syscall_result(unsafe {
        trap::raw_syscall6(
            libc::SYS_rt_sigaction,
            [libc::SIGSYS as u64, (&raw const action) as u64, 0, 8, 0, 0],
        )
    })
}

pub(crate) unsafe fn prepare_user_dispatch_thread() -> io::Result<()> {
    let action = current_sigsys_action()?;
    if action.handler != trap::user_dispatch_handler as *const () as usize
        || action.restorer != trap::trusted_sigreturn_restorer as *const () as usize
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SUD handler and trusted restorer must be installed before re-arming",
        ));
    }
    let mut mask = 0u64;
    syscall_result(unsafe {
        trap::raw_syscall6(
            libc::SYS_rt_sigprocmask,
            [0, 0, (&raw mut mask) as u64, 8, 0, 0],
        )
    })?;
    if mask & (1u64 << (libc::SIGSYS - 1)) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot enable SUD with SIGSYS blocked",
        ));
    }
    if action.flags & libc::SA_ONSTACK as u64 != 0 {
        let mut stack: libc::stack_t = unsafe { core::mem::zeroed() };
        syscall_result(unsafe {
            trap::raw_syscall6(
                libc::SYS_sigaltstack,
                [0, (&raw mut stack) as u64, 0, 0, 0, 0],
            )
        })?;
        if stack.ss_flags & libc::SS_DISABLE != 0 || stack.ss_size < alt_stack_size() {
            unsafe { install_alt_stack()? };
        }
    }
    Ok(())
}

/// Signals the runtime reserves for itself. The guest may not reconfigure these.
pub const RESERVED_SIGNALS: &[i32] = &[libc::SIGSYS];

/// Whether `signal` is reserved by the runtime.
pub fn is_reserved(signal: i32) -> bool {
    RESERVED_SIGNALS.contains(&signal)
        || (signal == libc::SIGTRAP && owned_trace::configured())
        || ((1..=64).contains(&signal) && runtime_signal_mask() & (1u64 << (signal - 1)) != 0)
}

/// Install `handler` for `SIGSYS` with `SA_SIGINFO`.
///
/// `on_alt_stack` requests `SA_ONSTACK`, so the handler runs on the alternate
/// signal stack configured with [`install_alt_stack`]. That keeps the trap
/// working even when the guest's own stack is nearly exhausted.
///
/// # Safety
///
/// `handler` must be a valid `SA_SIGINFO` signal handler that is
/// async-signal-safe. Installs process-global disposition.
pub unsafe fn install_sigsys_handler(
    handler: unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
    on_alt_stack: bool,
) -> io::Result<()> {
    let mut action: libc::sigaction = unsafe { core::mem::zeroed() };
    action.sa_flags = libc::SA_SIGINFO | if on_alt_stack { libc::SA_ONSTACK } else { 0 };
    action.sa_sigaction = handler as *const () as usize;
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::sigaction(libc::SIGSYS, &action, ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
    let size = alt_stack_size();
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

fn alt_stack_size() -> usize {
    (libc::SIGSTKSZ).max(64 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigsys_is_reserved_but_others_are_not() {
        assert!(is_reserved(libc::SIGSYS));
        assert!(!is_reserved(libc::SIGINT));
        assert!(!is_reserved(libc::SIGTERM));
        assert!(!is_reserved(libc::SIGCHLD));
    }
}
