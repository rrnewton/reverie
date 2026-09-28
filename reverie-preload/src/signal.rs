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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

fn query_kernel_sigaction(signal: libc::c_int) -> io::Result<KernelSigaction> {
    let mut action = core::mem::MaybeUninit::<KernelSigaction>::uninit();
    let result = unsafe {
        libc::syscall(
            libc::SYS_rt_sigaction,
            signal,
            ptr::null::<KernelSigaction>(),
            action.as_mut_ptr(),
            KERNEL_SIGSET_SIZE,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful raw query initialized the exact kernel structure.
    Ok(unsafe { action.assume_init() })
}

fn set_kernel_sigaction(signal: libc::c_int, action: &KernelSigaction) -> io::Result<()> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_rt_sigaction,
            signal,
            ptr::from_ref(action),
            ptr::null_mut::<KernelSigaction>(),
            KERNEL_SIGSET_SIZE,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_kernel_signal_mask(how: libc::c_int, set: &u64, old: Option<&mut u64>) -> io::Result<()> {
    let old = old.map_or(ptr::null_mut(), ptr::from_mut);
    let result = unsafe {
        libc::syscall(
            libc::SYS_rt_sigprocmask,
            how,
            ptr::from_ref(set),
            old,
            KERNEL_SIGSET_SIZE,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
fn query_kernel_signal_mask() -> io::Result<u64> {
    let mut mask = 0_u64;
    let result = unsafe {
        libc::syscall(
            libc::SYS_rt_sigprocmask,
            libc::SIG_SETMASK,
            ptr::null::<u64>(),
            ptr::from_mut(&mut mask),
            KERNEL_SIGSET_SIZE,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(mask)
}

/// Ask the process libc for its standard signal restorer without leaving any
/// probe disposition or mask behind. The runtime installation contract is
/// single-threaded, so blocking the probe signal closes its only delivery
/// window; every exit after the block attempts both restorations.
fn discover_libc_restorer_with_post_install<F>(
    handler: unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
    post_install: F,
) -> io::Result<usize>
where
    F: FnOnce() -> io::Result<()>,
{
    const PROBE_SIGNAL: libc::c_int = libc::SIGUSR2;
    let probe_mask = 1_u64 << (PROBE_SIGNAL - 1);
    let mut original_mask = 0_u64;
    set_kernel_signal_mask(libc::SIG_BLOCK, &probe_mask, Some(&mut original_mask))?;

    let mut original_action = None;
    let discovery = (|| {
        let action = query_kernel_sigaction(PROBE_SIGNAL)?;
        original_action = Some(action);

        let mut probe: libc::sigaction = unsafe { core::mem::zeroed() };
        probe.sa_flags = libc::SA_SIGINFO;
        probe.sa_sigaction = handler as *const () as usize;
        if unsafe { libc::sigemptyset(&mut probe.sa_mask) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::sigaction(PROBE_SIGNAL, &probe, ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        post_install()?;

        let installed = query_kernel_sigaction(PROBE_SIGNAL)?;
        if installed.handler != handler as *const () as usize
            || installed.flags & SA_RESTORER == 0
            || installed.restorer == 0
        {
            return Err(io::Error::other(
                "libc installed an invalid signal restorer",
            ));
        }
        Ok(installed.restorer)
    })();

    let action_restore = original_action
        .as_ref()
        .map_or(Ok(()), |action| set_kernel_sigaction(PROBE_SIGNAL, action));
    let mask_restore = set_kernel_signal_mask(libc::SIG_SETMASK, &original_mask, None);

    if action_restore.is_err() || mask_restore.is_err() {
        // Both restorations were attempted above. Continuing after either
        // failure could expose the temporary handler or mask to application
        // code, so this irreversible initialization boundary must fail closed.
        unsafe { libc::_exit(126) }
    }
    discovery
}

pub(crate) fn discover_libc_restorer(
    handler: unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
) -> io::Result<usize> {
    discover_libc_restorer_with_post_install(handler, || Ok(()))
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

/// Install `handler` for the runtime-reserved `SIGSYS` signal with the
/// runtime-owned exact restorer.
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

    #[test]
    fn libc_restorer_probe_restores_exact_action_and_mask_on_success_and_failure() {
        const CHILD: &str = "REVERIE_TEST_LIBC_RESTORER_PROBE";
        if let Some(mode) = std::env::var_os(CHILD) {
            let mut original_action =
                runtime_sigaction(test_handler, libc::SA_SIGINFO | libc::SA_RESTART).unwrap();
            original_action.mask = 1_u64 << (libc::SIGINT - 1);
            set_kernel_sigaction(libc::SIGUSR2, &original_action).unwrap();
            let original_mask = (1_u64 << (libc::SIGUSR1 - 1)) | (1_u64 << (libc::SIGTERM - 1));
            set_kernel_signal_mask(libc::SIG_SETMASK, &original_mask, None).unwrap();

            let before_action = query_kernel_sigaction(libc::SIGUSR2).unwrap();
            let before_mask = query_kernel_signal_mask().unwrap();
            let result = if mode == "success" {
                discover_libc_restorer_with_post_install(test_handler, || Ok(()))
            } else {
                discover_libc_restorer_with_post_install(test_handler, || {
                    Err(io::Error::other("injected post-install failure"))
                })
            };
            if mode == "success" {
                assert_ne!(result.unwrap(), 0);
            } else {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "injected post-install failure"
                );
            }
            assert_eq!(
                query_kernel_sigaction(libc::SIGUSR2).unwrap(),
                before_action
            );
            assert_eq!(query_kernel_signal_mask().unwrap(), before_mask);
            return;
        }

        for mode in ["success", "failure"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "signal::tests::libc_restorer_probe_restores_exact_action_and_mask_on_success_and_failure",
                    "--nocapture",
                ])
                .env(CHILD, mode)
                .output()
                .unwrap();
            assert!(output.status.success(), "{mode}: {output:?}");
            assert!(output.stderr.is_empty(), "{mode}: {output:?}");
        }
    }
}
