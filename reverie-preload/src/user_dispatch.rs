//! Opt-in, per-thread syscall user dispatch. This is not an exec implementation.

use std::cell::Cell;
use std::io;
use std::ops::Range;
use std::ptr;

use crate::lifecycle::LifecycleController;
use crate::lifecycle::RuntimeConfig;
use crate::signal;
use crate::trap;

pub(crate) const SYS_USER_DISPATCH_CODE: i32 = 2;
pub(crate) const PR_SET_SYSCALL_USER_DISPATCH: u64 = 59;
const PR_SYS_DISPATCH_ON: u64 = 1;
pub(crate) static RUNTIME_SIGNAL_MASK: u64 = u64::MAX;

thread_local! {
    static DISPATCH_SCOPE: Cell<*const DispatchScope> = const { Cell::new(ptr::null()) };
}

struct DispatchScope {
    previous: *const DispatchScope,
    guest_mask: *mut libc::sigset_t,
    suspended: Cell<bool>,
}

impl Drop for DispatchScope {
    fn drop(&mut self) {
        DISPATCH_SCOPE.set(self.previous);
    }
}

pub(crate) fn may_enter_dispatch() -> bool {
    let scope = DISPATCH_SCOPE.get();
    scope.is_null() || unsafe { (*scope).suspended.get() }
}

pub(crate) fn with_dispatch_mask<Result>(
    guest_mask: &mut libc::sigset_t,
    dispatch: impl FnOnce() -> Result,
) -> Result {
    let scope = DispatchScope {
        previous: DISPATCH_SCOPE.get(),
        guest_mask,
        suspended: Cell::new(false),
    };
    DISPATCH_SCOPE.set(&raw const scope);
    dispatch()
}

pub(crate) unsafe fn forward_syscall(number: i64, args: [u64; 6]) -> i64 {
    let scope = DISPATCH_SCOPE.get();
    if scope.is_null() {
        return unsafe { trap::raw_syscall6(number, args) };
    }
    let scope = unsafe { &*scope };
    if scope.suspended.replace(true) {
        unsafe { trap::exit_now(125) };
    }
    let opened = unsafe {
        trap::raw_syscall6(
            libc::SYS_rt_sigprocmask,
            [
                libc::SIG_SETMASK as u64,
                scope.guest_mask as u64,
                0,
                8,
                0,
                0,
            ],
        )
    };
    if opened != 0 {
        unsafe { trap::exit_now(125) };
    }
    let result = unsafe { trap::raw_syscall6(number, args) };
    let mut guest_mask = 0u64;
    let closed = unsafe {
        trap::raw_syscall6(
            libc::SYS_rt_sigprocmask,
            [
                libc::SIG_SETMASK as u64,
                (&raw const RUNTIME_SIGNAL_MASK) as u64,
                (&raw mut guest_mask) as u64,
                8,
                0,
                0,
            ],
        )
    };
    if closed != 0 {
        unsafe { trap::exit_now(125) };
    }
    unsafe { scope.guest_mask.cast::<u64>().write(guest_mask) };
    scope.suspended.set(false);
    result
}

/// In-process SIGSYS interception without an inherited trapping seccomp filter.
///
/// Only the shared syscall gate is trusted. The signal restorer tail-jumps to
/// that gate without changing RSP; it has no separate syscall instruction.
/// A null selector keeps every outside-range syscall intercepted; there is no
/// selector allocation, mutable ALLOW state, or selector lifetime to migrate.
/// SUD is not a security boundary against code that can call the trusted gate.
///
/// Linux resets SUD in new fork/clone tasks and across successful exec. The
/// caller must re-arm before guest work in each such thread or image. This
/// component neither covers loader startup nor restores a Tool across exec.
/// Existing exec/static restrictions and default seccomp behavior are unchanged.
#[derive(Debug, Default, Clone, Copy)]
pub struct InProcessUserDispatch;

impl InProcessUserDispatch {
    /// The exclusive-end trusted instruction-pointer range, for inspection.
    pub fn trusted_range() -> Range<usize> {
        trap::user_dispatch_range()
    }

    /// Enable or re-arm SUD on the calling thread, including after fork/clone.
    ///
    /// Checks the installed handler/restorer and rejects a blocked SIGSYS.
    /// Prepares a thread-local alternate stack if the process handler needs one.
    /// Unsupported-kernel and policy errors retain the kernel's errno.
    ///
    /// # Safety
    /// The process handler and dispatcher must already be installed, with their
    /// code/state alive until every participating thread has disabled SUD. The
    /// caller owns the current thread's signal state and must run this before
    /// guest instructions. No other thread may replace the handler. Do not mix
    /// this controller with a trapping seccomp controller. Forked Tool/TLS state
    /// must be made valid before re-arming. This is not automatic thread support.
    pub unsafe fn enable_current_thread() -> io::Result<()> {
        unsafe { signal::prepare_user_dispatch_thread()? };
        let range = Self::trusted_range();
        let result = unsafe {
            trap::raw_syscall6(
                libc::SYS_prctl,
                [
                    PR_SET_SYSCALL_USER_DISPATCH,
                    PR_SYS_DISPATCH_ON,
                    range.start as u64,
                    (range.end - range.start) as u64,
                    0,
                    0,
                ],
            )
        };
        syscall_result(result)
    }

    /// Disable only this thread's SUD; leave handler/altstack/dispatcher alive.
    ///
    /// # Safety
    /// The caller must own this thread's interception and prevent guest work
    /// until interception is re-armed or the thread exits. Disabling does not
    /// disable inherited seccomp or any CPUID/TSC trap setting. It must not be
    /// used to let a guest or Tool callback bypass interception policy.
    pub unsafe fn disable_current_thread() -> io::Result<()> {
        let result = unsafe {
            trap::raw_syscall6(
                libc::SYS_prctl,
                [PR_SET_SYSCALL_USER_DISPATCH, 0, 0, 0, 0, 0],
            )
        };
        syscall_result(result)
    }
}

impl LifecycleController for InProcessUserDispatch {
    fn name(&self) -> &'static str {
        "in-process-user-dispatch"
    }

    /// Install the process handler then arm only the calling thread.
    ///
    /// On error the handler/alternate stack may remain installed, but the kernel
    /// error is never converted into success or a native fallback.
    unsafe fn install(&self, config: &RuntimeConfig) -> io::Result<()> {
        unsafe {
            signal::install_user_dispatch_handler(config.use_alt_stack)?;
            Self::enable_current_thread()
        }
    }
}

pub(crate) fn syscall_result(result: i64) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::from_raw_os_error((-result) as i32))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_ownership_is_stacked_only_while_forwarding() {
        let mut outer_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut inner_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        assert!(may_enter_dispatch());
        with_dispatch_mask(&mut outer_mask, || {
            assert!(!may_enter_dispatch());
            let outer = DISPATCH_SCOPE.get();
            unsafe { (*outer).suspended.set(true) };
            assert!(may_enter_dispatch());
            with_dispatch_mask(&mut inner_mask, || {
                assert!(!may_enter_dispatch());
                assert_ne!(DISPATCH_SCOPE.get(), outer);
            });
            assert_eq!(DISPATCH_SCOPE.get(), outer);
            assert!(may_enter_dispatch());
            unsafe { (*outer).suspended.set(false) };
            assert!(!may_enter_dispatch());
        });
        assert!(DISPATCH_SCOPE.get().is_null());
    }

    #[test]
    fn gate_range_is_narrow_and_contains_syscall_return() {
        let range = InProcessUserDispatch::trusted_range();
        let gate = trap::trusted_gate();
        assert_eq!(range.start, gate.syscall_ip as usize);
        assert!(range.contains(&(gate.return_ip as usize)));
        assert!(!range.contains(&(trap::trusted_sigreturn_restorer as *const () as usize)));
        assert!(range.end > range.start);
        assert_eq!(range.len(), 3);
        let instructions =
            unsafe { std::slice::from_raw_parts(range.start as *const u8, range.len()) };
        assert_eq!(instructions, &[0x0f, 0x05, 0xc3]);
        assert_eq!(gate.return_ip, gate.syscall_ip + 2);
    }

    #[test]
    fn kernel_errors_are_not_capability_success() {
        for errno in [libc::EINVAL, libc::ENOSYS, libc::EFAULT, libc::EPERM] {
            assert_eq!(
                syscall_result(-i64::from(errno))
                    .unwrap_err()
                    .raw_os_error(),
                Some(errno)
            );
        }
    }
}
