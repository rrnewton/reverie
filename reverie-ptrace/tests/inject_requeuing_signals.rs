/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Guest::inject_requeuing_signals` on the ptrace backend: with two caught
//! signals pending when a tool injects a syscall, the syscall runs and both
//! signals are delivered when the guest resumes. A blind retry of
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

/// Answers each `getppid` with an injected `getpid`, after queueing SIGUSR1
/// and SIGUSR2 for the stopped thread.
#[derive(Clone, Copy, Debug, Default)]
struct RequeueTool;

#[reverie::tool]
impl Tool for RequeueTool {
    type GlobalState = ();
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getppid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(syscall.number(), Sysno::getppid);
        for signal in [libc::SIGUSR1, libc::SIGUSR2] {
            // SAFETY: tgkill has no memory-safety preconditions.
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_tgkill,
                    guest.pid().as_raw(),
                    guest.tid().as_raw(),
                    signal,
                )
            };
            assert_eq!(rc, 0, "queue signal {signal} for the stopped guest");
        }
        Ok(guest.inject_requeuing_signals(Getpid::new()).await?)
    }
}

static SIGUSR1_CALLS: AtomicUsize = AtomicUsize::new(0);
static SIGUSR2_CALLS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_signal(signal: libc::c_int) {
    match signal {
        libc::SIGUSR1 => SIGUSR1_CALLS.fetch_add(1, Ordering::Relaxed),
        libc::SIGUSR2 => SIGUSR2_CALLS.fetch_add(1, Ordering::Relaxed),
        _ => 0,
    };
}

#[test]
fn injection_with_two_pending_signals_delivers_both() {
    let (output, ()) = test_fn::<RequeueTool, _>(|| unsafe {
        for signal in [libc::SIGUSR1, libc::SIGUSR2] {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = count_signal as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                libc::_exit(2);
            }
        }
        // The tool answers with the injected getpid.
        if libc::getppid() != libc::getpid() {
            libc::_exit(3);
        }
        // Both handlers ran when getppid returned.
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
        "exit 3: the injected getpid's result was not returned; exit 10+3*usr1+usr2: \
         a handler ran other than once"
    );
}
