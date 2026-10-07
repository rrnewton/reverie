/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Guest::inject_requeuing_signals` on the ptrace backend: with caught
//! signals pending when a tool injects a syscall, the syscall runs and every
//! signal is delivered when the guest resumes, including one queued before a
//! later injection in the same callback. With a signal already held from an
//! earlier ordinary injection it declines without running. A blind retry of
//! `Guest::inject` holds a signal that stops the injection in a single slot,
//! so a second one replaces it and the first is lost.

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::ExitStatus;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Getpid;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_ptrace::testing::test_fn;

fn queue(guest_pid: i32, guest_tid: i32, signal: libc::c_int) {
    // SAFETY: tgkill has no memory-safety preconditions.
    let rc = unsafe { libc::syscall(libc::SYS_tgkill, guest_pid, guest_tid, signal) };
    assert_eq!(rc, 0, "queue signal {signal} for the stopped guest");
}

/// Answers `getppid` with an injected `getpid`: SIGUSR1 and SIGUSR2 are
/// queued, `inject_requeuing_signals` runs it, then SIGWINCH is queued and an
/// ordinary `inject` of `getpid` runs in the same callback. Answers `getuid`
/// after an ordinary injection held SIGUSR1: `inject_requeuing_signals` must
/// decline, and the call returns 0 if it did.
#[derive(Clone, Copy, Debug, Default)]
struct RequeueTool;

#[reverie::tool]
impl Tool for RequeueTool {
    type GlobalState = ();
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getppid, Sysno::getuid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let (pid, tid) = (guest.pid().as_raw(), guest.tid().as_raw());
        match syscall.number() {
            Sysno::getppid => {
                queue(pid, tid, libc::SIGUSR1);
                queue(pid, tid, libc::SIGUSR2);
                let first = guest
                    .inject_requeuing_signals(Getpid::new())
                    .await
                    .expect("ptrace requeues around the injection")?;
                queue(pid, tid, libc::SIGWINCH);
                // A later ordinary injection in the same callback requeues too,
                // so it runs instead of returning ERESTARTSYS with SIGWINCH
                // held.
                let second = guest.inject(Getpid::new()).await?;
                assert_eq!(first, second);
                Ok(first)
            }
            Sysno::getuid => {
                queue(pid, tid, libc::SIGUSR1);
                // Holds SIGUSR1 and returns ERESTARTSYS without running.
                assert_eq!(
                    guest.inject(Getpid::new()).await,
                    Err(reverie::Errno::ERESTARTSYS)
                );
                queue(pid, tid, libc::SIGUSR2);
                Ok(guest
                    .inject_requeuing_signals(Getpid::new())
                    .await
                    .map_or(0, |_| 1))
            }
            other => panic!("subscribed only to getppid and getuid, got {other:?}"),
        }
    }
}

static SIGUSR1_CALLS: AtomicUsize = AtomicUsize::new(0);
static SIGUSR2_CALLS: AtomicUsize = AtomicUsize::new(0);
static SIGWINCH_CALLS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_signal(signal: libc::c_int) {
    match signal {
        libc::SIGUSR1 => SIGUSR1_CALLS.fetch_add(1, Ordering::Relaxed),
        libc::SIGUSR2 => SIGUSR2_CALLS.fetch_add(1, Ordering::Relaxed),
        libc::SIGWINCH => SIGWINCH_CALLS.fetch_add(1, Ordering::Relaxed),
        _ => 0,
    };
}

/// Installs `count_signal` for SIGUSR1, SIGUSR2 and SIGWINCH, or exits 2.
unsafe fn install_counters() {
    for signal in [libc::SIGUSR1, libc::SIGUSR2, libc::SIGWINCH] {
        // SAFETY: the action is fully initialized and the signals are valid.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = count_signal as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                libc::_exit(2);
            }
        }
    }
}

#[test]
fn injection_with_pending_signals_delivers_every_one() {
    let (output, ()) = test_fn::<RequeueTool, _>(|| unsafe {
        install_counters();
        // The tool answers with the injected getpid.
        if libc::getppid() != libc::getpid() {
            libc::_exit(3);
        }
        // Every handler ran once when getppid returned.
        let usr1 = SIGUSR1_CALLS.load(Ordering::Relaxed);
        let usr2 = SIGUSR2_CALLS.load(Ordering::Relaxed);
        let winch = SIGWINCH_CALLS.load(Ordering::Relaxed);
        if usr1 != 1 || usr2 != 1 || winch != 1 {
            libc::_exit(10 + (usr1 as i32) * 9 + (usr2 as i32) * 3 + winch as i32);
        }
        libc::syscall(libc::SYS_exit_group, 0);
    })
    .unwrap();
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "exit 3: the injected getpid's result was not returned; exit \
         10+9*usr1+3*usr2+winch: a handler ran other than once"
    );
}

#[test]
fn injection_declines_while_a_signal_is_held() {
    let (output, ()) = test_fn::<RequeueTool, _>(|| unsafe {
        install_counters();
        // The tool returns 0 if inject_requeuing_signals declined.
        if libc::getuid() != 0 {
            libc::_exit(4);
        }
        let usr1 = SIGUSR1_CALLS.load(Ordering::Relaxed);
        let usr2 = SIGUSR2_CALLS.load(Ordering::Relaxed);
        if usr1 != 1 || usr2 != 1 {
            libc::_exit(10 + (usr1 as i32) * 3 + usr2 as i32);
        }
        libc::syscall(libc::SYS_exit_group, 0);
    })
    .unwrap();
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "exit 4: inject_requeuing_signals ran with a signal held; exit \
         10+3*usr1+usr2: a handler ran other than once"
    );
}
