/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A precise timer's PMU signal can be raised just before the guest enters a
//! syscall and still be pending when the tool handles that syscall. If the
//! tool injects a syscall, the single step that runs it is where the pending
//! signal stops the guest. That signal is the timer's and must reach the
//! timer, not the guest: `SIGSTKFLT`, the marker the timer uses, terminates a
//! guest that has no handler for it.
//!
//! Each round here sets a precise timer, runs a loop that ends a few
//! branches either side of the timer's PMU notification, and then makes a
//! syscall for which the tool injects another. The guest counts every
//! `SIGSTKFLT` it receives and requires none. The three cases put the timer
//! in each state it can be in when that signal stops the injected syscall.

#![cfg(target_arch = "x86_64")]

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Getpid;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_ptrace::PmuConfig;
use reverie_ptrace::ret_without_perf;
use reverie_ptrace::testing::check_fn_with_config;
use serde::Deserialize;
use serde::Serialize;
use test_case::test_case;

/// Far above any skid margin, so the request programs a real PMU notification.
const TIMER_RCBS: u64 = 10_000;

/// How many loop lengths the rounds cycle through around the notification.
const OFFSETS: u64 = 16;

/// Rounds per run, each loop length used this many times over.
const ROUNDS: u64 = OFFSETS * 8;

/// The timer's state when its signal stops the injected syscall.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Case {
    /// The injecting syscall is the first stop since the request.
    #[default]
    Armed,
    /// The tool requests another event before injecting.
    Rerequested,
    /// The guest made another traced syscall since the request.
    Cancelled,
}

#[derive(Debug, Default)]
struct Log {
    timer_events: AtomicU64,
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Request = ();
    type Response = ();
    type Config = Case;

    async fn receive_rpc(&self, _from: Pid, _: ()) {
        self.timer_events.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Default, Clone)]
struct InjectingTool;

#[reverie::tool]
impl Tool for InjectingTool {
    type GlobalState = Log;
    type ThreadState = ();

    fn subscriptions(_cfg: &Case) -> Subscription {
        let mut s = Subscription::none();
        s.syscalls([Sysno::clock_getres, Sysno::getegid, Sysno::getppid]);
        s
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall.number() {
            Sysno::clock_getres => {
                guest
                    .set_timer_precise(TimerSchedule::Rcbs(TIMER_RCBS))
                    .unwrap();
                Ok(0)
            }
            Sysno::getppid => {
                if *guest.config() == Case::Rerequested {
                    guest
                        .set_timer_precise(TimerSchedule::Rcbs(TIMER_RCBS))
                        .unwrap();
                }
                // A syscall other than the guest's, so Reverie skips the
                // guest's and runs this one from its private page.
                guest.inject(Getpid::new()).await?;
                guest.tail_inject(syscall).await
            }
            _ => guest.tail_inject(syscall).await,
        }
    }

    async fn handle_timer_event<T: Guest<Self>>(&self, guest: &mut T) {
        guest.send_rpc(()).await;
    }
}

/// A syscall with no conditional branch between its caller and the kernel
/// entry, so the guest's branch count at the stop is exact.
#[inline(always)]
unsafe fn syscall_no_branches(no: Sysno) {
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") no as usize => _,
            in("rdi") 0usize,
            in("rsi") 0usize,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
}

/// Retires exactly `iterations` conditional branches, and no other.
#[inline(always)]
fn branches(iterations: u64) {
    unsafe {
        core::arch::asm!(
            "2:",
            "dec {n}",
            "jnz 2b",
            n = inout(reg) iterations => _,
            options(nostack, nomem),
        );
    }
}

static TIMER_SIGNALS_DELIVERED: AtomicU64 = AtomicU64::new(0);

extern "C" fn count_timer_signal(_: libc::c_int) {
    TIMER_SIGNALS_DELIVERED.fetch_add(1, Ordering::SeqCst);
}

#[test_case(Case::Armed, 0; "armed")]
#[test_case(Case::Rerequested, ROUNDS; "rerequested")]
#[test_case(Case::Cancelled, 0; "cancelled")]
fn timer_signal_pending_at_an_injected_syscall_is_not_delivered_to_the_guest(
    case: Case,
    expected_events: u64,
) {
    ret_without_perf!();
    let skid = PmuConfig::new().skid_margin();
    assert!(skid < TIMER_RCBS, "skid margin {skid}");
    let notification = TIMER_RCBS - skid;
    // Past the target of a timer requested at getppid, so that event fires
    // before the next round's request replaces it.
    let after = match case {
        Case::Rerequested => 2 * TIMER_RCBS,
        Case::Armed | Case::Cancelled => 2 * skid + OFFSETS,
    };

    let log = check_fn_with_config::<InjectingTool, _>(
        move || {
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = count_timer_signal as extern "C" fn(libc::c_int) as usize;
                assert_eq!(
                    libc::sigaction(libc::SIGSTKFLT, &action, std::ptr::null_mut()),
                    0
                );
            }
            for round in 0..ROUNDS {
                // From four branches before the notification to eleven after.
                let before = (notification - 4).wrapping_add(round % OFFSETS);
                unsafe { syscall_no_branches(Sysno::clock_getres) };
                if case == Case::Cancelled {
                    unsafe { syscall_no_branches(Sysno::getegid) };
                }
                branches(before);
                unsafe { syscall_no_branches(Sysno::getppid) };
                branches(after);
            }
            assert_eq!(
                TIMER_SIGNALS_DELIVERED.load(Ordering::SeqCst),
                0,
                "the guest received the timer's SIGSTKFLT"
            );
        },
        case,
        true,
    );

    // The guest's own getppid comes before the target of the event requested
    // at clock_getres, so Reverie cancels that event whether or not its signal
    // is sent again. Only an event requested at getppid fires, once a round.
    assert_eq!(log.timer_events.load(Ordering::SeqCst), expected_events);
}
