/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A guest that *sends itself* a `SIGTRAP` must observe it.
//!
//! Sibling of `guest_int3.rs`, which covers the `#BP` half of guest-owned
//! `SIGTRAP` provenance. This file covers the process-generated half: `raise`,
//! `kill`, `sigqueue`, and the asynchronous sources a guest can arm (here, a
//! POSIX timer). Together the two files bracket the whole `si_code` space that
//! `handle_sigtrap` now partitions.
//!
//! Coverage carried from rrnewton/reverie#388. That PR's committed tests
//! stopped at the pure predicate, which its adversarial review
//! (`[adversarial-reviewer agent, gpt-5.6-sol]`, exact head
//! `bf8ee2dd8ebcf61c18e742c943851823bc4904f4`, finding 2) blocked on: a pure
//! predicate cannot prove the delivery *branch* fires, that `PTRACE_GETSIGINFO`
//! reaches it, that reinjection preserves the guest's `si_code`, or that the
//! default disposition still terminates. Each of those is asserted below
//! against a live tracee.

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

/// `SI_TIMER`, which `libc` does not export. Linux stamps it on a signal
/// delivered by an expiring POSIX timer (`kernel/time/posix-timers.c`).
#[cfg(target_arch = "x86_64")]
const SI_TIMER: libc::c_int = -2;

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

/// Install `on_trap` as the guest's `SIGTRAP` handler.
///
/// # Safety
///
/// Must be called from a guest that is about to raise `SIGTRAP` and that does
/// not concurrently reinstall the disposition.
#[cfg(target_arch = "x86_64")]
unsafe fn install_trap_handler() {
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
}

/// POSITIVE side, arm 1: `raise(SIGTRAP)` -- glibc's `raise` is `tgkill`, so
/// `si_code == SI_TKILL`. This is the exact case the regression hit.
///
/// Asserting the *code* as well as "the handler ran" is deliberate: a path that
/// synthesized a `SIGTRAP` from some other source would satisfy the weaker
/// assertion while corrupting what the guest sees.
#[cfg(target_arch = "x86_64")]
#[test]
fn raised_sigtrap_reaches_the_guests_own_handler_with_its_code() {
    reverie_ptrace::testing::check_fn::<PassthroughTool, _>(|| {
        // SAFETY: installing a handler for a signal this thread raises next.
        unsafe { install_trap_handler() };

        assert_eq!(unsafe { libc::raise(libc::SIGTRAP) }, 0);

        assert_eq!(
            TRAP_HITS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the guest's own SIGTRAP handler must run exactly once for raise(SIGTRAP)"
        );
        assert_eq!(
            TRAP_CODE.load(std::sync::atomic::Ordering::SeqCst),
            libc::SI_TKILL,
            "the guest must see the native si_code for a raise()d signal"
        );
    });
}

/// POSITIVE side, arm 2: a source **outside** the `{SI_USER, SI_TKILL,
/// SI_QUEUE}` allowlist that rrnewton/reverie#388 shipped. An expiring POSIX
/// timer armed by the guest with `sigev_signo = SIGTRAP` produces `SI_TIMER`,
/// which that allowlist still swallowed -- finding 1 of #388's review, now
/// covered by the `SI_FROMUSER` (`si_code <= 0`) domain test.
#[cfg(target_arch = "x86_64")]
#[test]
fn guest_armed_posix_timer_sigtrap_reaches_the_guests_own_handler() {
    reverie_ptrace::testing::check_fn::<PassthroughTool, _>(|| {
        // SAFETY: installing a handler for a signal this thread arms next.
        unsafe { install_trap_handler() };

        // SAFETY: a one-shot CLOCK_MONOTONIC timer targeting this thread's
        // SIGTRAP. The handler installed above returns, so execution resumes.
        unsafe {
            let mut sev: libc::sigevent = std::mem::zeroed();
            sev.sigev_notify = libc::SIGEV_SIGNAL;
            sev.sigev_signo = libc::SIGTRAP;
            let mut timer: libc::timer_t = std::mem::zeroed();
            assert_eq!(
                libc::timer_create(libc::CLOCK_MONOTONIC, &mut sev, &mut timer),
                0,
                "timer_create failed: {}",
                std::io::Error::last_os_error()
            );

            let spec = libc::itimerspec {
                it_interval: libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                },
                it_value: libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 1_000_000, // 1 ms one-shot
                },
            };
            assert_eq!(
                libc::timer_settime(timer, 0, &spec, std::ptr::null_mut()),
                0,
                "timer_settime failed: {}",
                std::io::Error::last_os_error()
            );

            // Block until the timer fires. `pause` returns -1/EINTR once a
            // handler has run, so this cannot spin or hang on a fast host.
            while TRAP_HITS.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                libc::pause();
            }

            libc::timer_delete(timer);
        }

        assert_eq!(
            TRAP_CODE.load(std::sync::atomic::Ordering::SeqCst),
            SI_TIMER,
            "a guest-armed POSIX timer's SIGTRAP must reach the guest with SI_TIMER; \
             an {{SI_USER, SI_TKILL, SI_QUEUE}} allowlist swallows this one"
        );
    });
}

/// The other observable, and the one the bug was actually reported as: with no
/// handler installed, `SIGTRAP`'s default disposition is terminate-with-core,
/// so the guest must die rather than run on. The `Exited(99)` arm names the
/// regression explicitly so a reopened one reports itself instead of surfacing
/// as a generic mismatch.
#[cfg(target_arch = "x86_64")]
#[test]
fn unhandled_raised_sigtrap_terminates_the_guest() {
    let (output, _) = reverie_ptrace::testing::test_fn::<PassthroughTool, _>(|| {
        // SAFETY: raising SIGTRAP with the default disposition in place. This
        // is expected to terminate the guest and never return.
        unsafe { libc::raise(libc::SIGTRAP) };

        // Reached only if the signal was swallowed, which is the regression.
        std::process::exit(99);
    })
    .unwrap();

    match output.status {
        reverie::ExitStatus::Signaled(reverie::Signal::SIGTRAP, _) => {}
        reverie::ExitStatus::Exited(99) => panic!(
            "raise(SIGTRAP) was swallowed: the guest survived a signal whose default \
             disposition kills it, and exited normally (rrnewton/reverie#388)"
        ),
        other => panic!("expected termination by SIGTRAP, got {other:?}"),
    }
}
