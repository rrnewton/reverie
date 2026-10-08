/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The guest process's signal state under the in-guest runtime: which
//! signals must stay unblocked, the reset of inherited handlers at
//! installation, and which `rt_sigaction` calls the guest may make.

use std::io;

use crate::guest::instruction::InstructionSubscriptions;
use crate::guest::instruction::any_instruction_subscribed;
use crate::guest::support::KernelSigaction;
use crate::guest::support::SignalInstallGuard;
use crate::trap::raw_syscall6;

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-913): Review the reserved-signal set kept unblocked.
/// Signals the runtime receives as forced signals and so must never be
/// blocked: SIGSYS for every trapped system call, and SIGSEGV for CPUID or
/// RDTSC faulting while an instruction is subscribed. Linux resets a blocked
/// forced signal to its default action, which kills the process.
pub fn reserved_signal_mask() -> u64 {
    let mut reserved = 1_u64 << (libc::SIGSYS - 1);
    if any_instruction_subscribed() {
        reserved |= 1_u64 << (libc::SIGSEGV - 1);
    }
    reserved
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-133): Review atomic signal-state preparation.
/// Prepares this process's signal state for the runtime: blocks every signal
/// and returns a guard that restores the caller's mask with SIGSYS (and
/// SIGSEGV when an instruction is subscribed) removed, so those forced
/// signals stay deliverable. Every inherited handler other than `SIG_DFL` or
/// `SIG_IGN` is reset to `SIG_DFL` while the mask is held, so no handler
/// installed before the runtime can run guest code it would not observe.
pub fn prepare_guest_signal_state(
    instructions: InstructionSubscriptions,
) -> io::Result<SignalInstallGuard> {
    let sigsys = 1_u64 << (libc::SIGSYS - 1);
    let sigsegv = if instructions.cpuid || instructions.rdtsc {
        1_u64 << (libc::SIGSEGV - 1)
    } else {
        0
    };
    let install_mask = u64::MAX;
    let mut previous_mask = 0_u64;
    let result = unsafe {
        raw_syscall6(
            libc::SYS_rt_sigprocmask,
            [
                libc::SIG_SETMASK as u64,
                (&raw const install_mask) as u64,
                (&raw mut previous_mask) as u64,
                core::mem::size_of::<u64>() as u64,
                0,
                0,
            ],
        )
    };
    if result < 0 {
        return Err(io::Error::from_raw_os_error((-result) as i32));
    }
    let guard = SignalInstallGuard::restoring(previous_mask & !(sigsys | sigsegv));

    for signal in 1..=64 {
        if matches!(signal, libc::SIGKILL | libc::SIGSTOP) {
            continue;
        }
        let mut action = KernelSigaction::default();
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [
                    signal as u64,
                    0,
                    (&raw mut action) as u64,
                    core::mem::size_of::<u64>() as u64,
                    0,
                    0,
                ],
            )
        };
        if result < 0 {
            return Err(io::Error::from_raw_os_error((-result) as i32));
        }
        if action.handler != libc::SIG_DFL as u64 && action.handler != libc::SIG_IGN as u64 {
            let default_action = KernelSigaction::default();
            let result = unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigaction,
                    [
                        signal as u64,
                        (&raw const default_action) as u64,
                        0,
                        core::mem::size_of::<u64>() as u64,
                        0,
                        0,
                    ],
                )
            };
            if result < 0 {
                return Err(io::Error::from_raw_os_error((-result) as i32));
            }
        }
    }
    Ok(guard)
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-133): Review fault-safe guest signal-action decoding.
/// Whether the runtime can let the guest's `rt_sigaction(number, args)` run.
/// Any other syscall, and a query without a new action, is supported. A new
/// action for SIGSYS (or SIGSEGV while an instruction is subscribed) is not;
/// for any other signal only `SIG_DFL` and `SIG_IGN` are, because a guest
/// handler would run outside the Tool's view. The handler is read from the
/// guest's `struct sigaction` with `process_vm_readv`, so an unreadable
/// pointer is refused rather than faulting. A new action for SIGKILL or
/// SIGSTOP is supported whatever it holds: Linux refuses it (EINVAL, or
/// EFAULT for an unreadable action) without changing anything, and a program
/// may make that call to validate its signal handling.
pub fn signal_action_supported(number: i64, args: [u64; 6]) -> bool {
    if number != libc::SYS_rt_sigaction || args[1] == 0 {
        return true;
    }
    if args[0] as i32 == libc::SIGKILL || args[0] as i32 == libc::SIGSTOP {
        return true;
    }
    // Linux reads the signal argument as a C int.
    let signal = args[0] as i32;
    if signal == libc::SIGSYS || (signal == libc::SIGSEGV && any_instruction_subscribed()) {
        return false;
    }

    let mut handler = 0_u64;
    let local = libc::iovec {
        iov_base: (&raw mut handler).cast(),
        iov_len: core::mem::size_of::<u64>(),
    };
    let remote = libc::iovec {
        iov_base: args[1] as usize as *mut libc::c_void,
        iov_len: core::mem::size_of::<u64>(),
    };
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let read = unsafe {
        raw_syscall6(
            libc::SYS_process_vm_readv,
            [
                pid as u64,
                (&raw const local) as u64,
                1,
                (&raw const remote) as u64,
                1,
                0,
            ],
        )
    };
    read == core::mem::size_of::<u64>() as i64
        && matches!(handler, value if value == libc::SIG_DFL as u64 || value == libc::SIG_IGN as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_action_supported_allows_only_default_and_ignore_handlers() {
        let action = |handler: u64| [handler, 0_u64, 0, 0];
        let ignore = action(libc::SIG_IGN as u64);
        let default = action(libc::SIG_DFL as u64);
        let handler = action(0x1000);
        let args = |signal: i32, act: &[u64; 4]| [signal as u64, act.as_ptr() as u64, 0, 8, 0, 0];

        // Other syscalls and queries without a new action are always allowed.
        assert!(signal_action_supported(libc::SYS_getpid, [0; 6]));
        assert!(signal_action_supported(
            libc::SYS_rt_sigaction,
            [libc::SIGUSR1 as u64, 0, 0, 8, 0, 0]
        ));
        // SIGSYS is the runtime's own, whatever the action.
        assert!(!signal_action_supported(
            libc::SYS_rt_sigaction,
            args(libc::SIGSYS, &default)
        ));
        // Only SIG_DFL and SIG_IGN are allowed for another signal.
        assert!(signal_action_supported(
            libc::SYS_rt_sigaction,
            args(libc::SIGUSR1, &ignore)
        ));
        assert!(signal_action_supported(
            libc::SYS_rt_sigaction,
            args(libc::SIGUSR1, &default)
        ));
        assert!(!signal_action_supported(
            libc::SYS_rt_sigaction,
            args(libc::SIGUSR1, &handler)
        ));
        // An unreadable action is refused, not dereferenced.
        assert!(!signal_action_supported(
            libc::SYS_rt_sigaction,
            [libc::SIGUSR1 as u64, 8, 0, 8, 0, 0]
        ));
        // Linux refuses any new action for SIGKILL and SIGSTOP itself, so the
        // call runs and the guest sees the kernel's error.
        for signal in [libc::SIGKILL, libc::SIGSTOP] {
            assert!(signal_action_supported(
                libc::SYS_rt_sigaction,
                args(signal, &handler)
            ));
            assert!(signal_action_supported(
                libc::SYS_rt_sigaction,
                [signal as u64, 8, 0, 8, 0, 0]
            ));
        }
    }

    /// The signal argument is read as a C int, as Linux reads it, whether or
    /// not SIGALRM handlers are admitted: an alias with high bits set is that
    /// signal. (Before, a SIGKILL alias with a handler got EPERM where Linux
    /// returns EINVAL, and a SIGSYS alias passed the guard.)
    #[test]
    fn the_guard_reads_the_signal_as_a_c_int() {
        let handler = [0x1000_u64, 0, 0, 0];
        let args = |signal: u64| [signal, handler.as_ptr() as u64, 0, 8, 0, 0];
        let high = 0x1_0000_0000_u64;
        assert!(signal_action_supported(
            libc::SYS_rt_sigaction,
            args(high | libc::SIGKILL as u64)
        ));
        assert!(!signal_action_supported(
            libc::SYS_rt_sigaction,
            args(high | libc::SIGSYS as u64)
        ));
        assert!(!signal_action_supported(
            libc::SYS_rt_sigaction,
            args(high | libc::SIGUSR1 as u64)
        ));
    }

    /// The kernel's answer the guest sees for the SIGKILL and SIGSTOP calls
    /// the guard admits: EINVAL for a readable action, EFAULT for an
    /// unreadable one, and the action is unchanged.
    #[test]
    fn linux_refuses_a_new_sigkill_or_sigstop_action_itself() {
        let handler = KernelSigaction {
            handler: 0x1000,
            ..KernelSigaction::default()
        };
        for signal in [libc::SIGKILL, libc::SIGSTOP] {
            let readable = unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigaction,
                    [signal as u64, (&raw const handler) as u64, 0, 8, 0, 0],
                )
            };
            assert_eq!(readable, -i64::from(libc::EINVAL));
            let unreadable =
                unsafe { raw_syscall6(libc::SYS_rt_sigaction, [signal as u64, 8, 0, 8, 0, 0]) };
            assert_eq!(unreadable, -i64::from(libc::EFAULT));
            let mut current = KernelSigaction::default();
            let query = unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigaction,
                    [signal as u64, 0, (&raw mut current) as u64, 8, 0, 0],
                )
            };
            assert_eq!(query, 0);
            assert_eq!(current.handler, libc::SIG_DFL as u64);
        }
    }
}
