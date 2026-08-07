/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A guest that executes its own `int3` must observe the resulting `SIGTRAP`.
//!
//! Regression coverage for
//! <https://github.com/rrnewton/hermit/issues/1715>: the ptrace backend used to
//! consume *every* `SIGTRAP` it could not attribute to one of its own
//! mechanisms, so a guest breakpoint silently did nothing. The guest ran past
//! the `int3`, no handler fired, and the process exited normally where native
//! Linux reports `WIFSIGNALED`/`WTERMSIG == SIGTRAP`.
//!
//! Both observables are pinned here, because the bug's defining property was
//! that it was *silent* -- either one alone can be satisfied by a partial fix:
//!
//!  * with a handler installed, the handler must run and must see the native
//!    `si_code` (`SI_KERNEL`);
//!  * with no handler installed, the default disposition must terminate the
//!    guest with `SIGTRAP`.

use reverie::Error;
use reverie::Guest;
use reverie::Tool;
use reverie::syscalls::Syscall;

/// A tool that does nothing but pass syscalls through, so the test observes the
/// backend's signal behavior rather than a tool's.
#[derive(Debug, Default, Clone)]
struct PassthroughTool;

#[reverie::tool]
impl Tool for PassthroughTool {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        guest.tail_inject(syscall).await
    }
}

/// `SI_KERNEL`, which `libc` does not export. Linux stamps this on the
/// `SIGTRAP` raised by a guest `int3`: `do_int3_user` in
/// `arch/x86/kernel/traps.c` passes `sicode == 0`, which makes `do_trap` take
/// the `force_sig` path.
#[cfg(target_arch = "x86_64")]
const SI_KERNEL: libc::c_int = 0x80;

#[cfg(target_arch = "x86_64")]
static TRAP_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(target_arch = "x86_64")]
static TRAP_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(i32::MIN);

#[cfg(target_arch = "x86_64")]
extern "C" fn on_trap(
    _signum: libc::c_int,
    info: *mut libc::siginfo_t,
    _context: *mut libc::c_void,
) {
    // SAFETY: the kernel hands a valid `siginfo_t` to an `SA_SIGINFO` handler.
    let code = unsafe { (*info).si_code };
    TRAP_CODE.store(code, std::sync::atomic::Ordering::SeqCst);
    TRAP_HITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// POSITIVE side of the bracket: the guest's own handler must run, and must see
/// what it would see natively. Asserting `si_code` as well as "the handler ran"
/// is deliberate -- a path that synthesized a `SIGTRAP` from some other source
/// would satisfy the weaker assertion.
#[cfg(target_arch = "x86_64")]
#[test]
fn guest_int3_reaches_the_guests_own_handler() {
    reverie_ptrace::testing::check_fn::<PassthroughTool, _>(|| {
        // SAFETY: installing a handler for a signal this thread is about to
        // raise synchronously.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_trap as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO;
            libc::sigemptyset(&mut action.sa_mask);
            assert_eq!(
                libc::sigaction(libc::SIGTRAP, &action, std::ptr::null_mut()),
                0
            );
        }

        // SAFETY: a one-byte `int3`. The handler installed above returns, so
        // execution resumes at the following instruction as it does natively.
        unsafe { std::arch::asm!("int3") };

        assert_eq!(
            TRAP_HITS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the guest's own SIGTRAP handler must run for the guest's own int3"
        );
        assert_eq!(
            TRAP_CODE.load(std::sync::atomic::Ordering::SeqCst),
            SI_KERNEL,
            "the guest must see the native si_code for a #BP"
        );
    });
}

/// The other observable: with no handler installed, `SIGTRAP`'s default
/// disposition is terminate-with-core, so the guest must die rather than run
/// on. This is the exact shape issue #1715 reported -- the guest previously
/// survived and exited normally. The `Exited(99)` arm names that case
/// explicitly so a reopened regression reports itself instead of surfacing as a
/// generic mismatch.
#[cfg(target_arch = "x86_64")]
#[test]
fn unhandled_guest_int3_terminates_the_guest() {
    let (output, _) = reverie_ptrace::testing::test_fn::<PassthroughTool, _>(|| {
        // SAFETY: raising `#BP` with the default disposition in place. This is
        // expected to terminate the guest and never return.
        unsafe { std::arch::asm!("int3") };

        // Reached only if the trap was swallowed, which is the regression.
        std::process::exit(99);
    })
    .unwrap();

    match output.status {
        reverie::ExitStatus::Signaled(reverie::Signal::SIGTRAP, _) => {}
        reverie::ExitStatus::Exited(99) => panic!(
            "guest int3 was swallowed: the guest ran past its own breakpoint and \
             exited normally (rrnewton/hermit#1715)"
        ),
        other => panic!("expected termination by SIGTRAP, got {other:?}"),
    }
}
