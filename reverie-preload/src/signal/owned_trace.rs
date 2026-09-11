//! Separate, synchronous single-step ownership; not a returning notification.

use std::io;
use std::sync::OnceLock;

use super::KernelSignalAction;
use crate::trap;
use crate::user_dispatch::syscall_result;

type Entry = unsafe extern "C" fn(i32, *mut libc::siginfo_t, *mut libc::c_void);
static ENTRY: OnceLock<Entry> = OnceLock::new();

pub fn configured() -> bool {
    ENTRY.get().is_some()
}

fn action() -> Option<KernelSignalAction> {
    Some(KernelSignalAction {
        handler: *ENTRY.get()? as *const () as usize,
        flags: (libc::SA_SIGINFO | libc::SA_ONSTACK) as u64 | 0x04000000,
        restorer: trap::trusted_sigreturn_restorer as *const () as usize,
        mask: u64::MAX,
    })
}

/// Reserve the single synchronous #DB source before SUD installation.
/// No action is installed and no debug source is activated here.
///
/// # Safety
/// Single installing thread, no live TF or competing debug facility, no pending
/// signals, and default/unblocked SIGTRAP. The entry must capture/authenticate
/// one expected TRAP_TRACE on the owned altstack, with process-life code/storage.
/// It must run clock-paused and without TF, allocation, locks, unwind or Tool
/// calls in signal context. Only an independently owned direct-return protocol
/// may retain a relocated frame; raw signal-frame references must not survive.
/// Runtime and first-return masks block SIGTRAP; only an admitted final guest
/// frame may restore owned TF. No other returning/async source is admitted.
pub unsafe fn configure(entry: Entry) -> io::Result<()> {
    let flags: u64;
    unsafe {
        core::arch::asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags));
    }
    if flags & 0x100 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pre-existing TF",
        ));
    }
    if configured() || super::runtime_signals_configured() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "runtime source conflict",
        ));
    }
    let mut mask = 0u64;
    let mut pending = 0u64;
    let mut current = KernelSignalAction::default();
    for (number, args) in [
        (
            libc::SYS_rt_sigprocmask,
            [0, 0, (&raw mut mask) as u64, 8, 0, 0],
        ),
        (
            libc::SYS_rt_sigpending,
            [(&raw mut pending) as u64, 8, 0, 0, 0, 0],
        ),
        (
            libc::SYS_rt_sigaction,
            [libc::SIGTRAP as u64, 0, (&raw mut current) as u64, 8, 0, 0],
        ),
    ] {
        syscall_result(unsafe { trap::raw_syscall6(number, args) })?;
    }
    if mask & (1 << (libc::SIGTRAP - 1)) != 0 || pending != 0 || current.handler != libc::SIG_DFL {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unavailable owned trace source",
        ));
    }
    ENTRY
        .set(entry)
        .map_err(|_| io::Error::other("owned trace already configured"))
}

pub(super) unsafe fn install() -> io::Result<()> {
    let Some(action) = action() else {
        return Ok(());
    };
    syscall_result(unsafe {
        trap::raw_syscall6(
            libc::SYS_rt_sigaction,
            [libc::SIGTRAP as u64, (&raw const action) as u64, 0, 8, 0, 0],
        )
    })?;
    if !installed() {
        return Err(io::Error::other("owned trace action readback mismatch"));
    }
    Ok(())
}

/// Bounded action readback through the raw gate; no allocation or retry.
pub fn installed() -> bool {
    let Some(expected) = action() else {
        return false;
    };
    let mut actual = KernelSignalAction::default();
    let result = unsafe {
        trap::raw_syscall6(
            libc::SYS_rt_sigaction,
            [libc::SIGTRAP as u64, 0, (&raw mut actual) as u64, 8, 0, 0],
        )
    };
    result == 0
        && actual.handler == expected.handler
        && actual.flags == expected.flags
        && actual.restorer == expected.restorer
        && actual.mask == expected.mask & !((1 << (libc::SIGKILL - 1)) | (1 << (libc::SIGSTOP - 1)))
}
