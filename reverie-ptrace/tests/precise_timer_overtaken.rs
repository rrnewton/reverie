/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A precise timer event is decided by the first Tool-observable stop after it
//! is requested. When the overflow interrupt is late, that stop can be the
//! guest's next syscall even though the guest has already executed the target
//! branch. The syscall cancels the event, so the Tool sees the syscall where
//! the timer event was due. The event cannot be recovered at that point, but it
//! must be recorded as a skid overshoot so a supervisor can refuse the run.
//!
//! Interrupt latency cannot be forced in a test, so the guest blocks the timer
//! signal instead: no notification is ever handled, which is the limiting case
//! of a late interrupt. Blocking it at a boundary one branch either side of the
//! target shows exactly where the event becomes due.

#![cfg(target_arch = "x86_64")]

use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_ptrace::ret_without_perf;
use reverie_ptrace::testing::check_fn_with_config;
use reverie_ptrace::testing::do_branches;
use serde::Deserialize;
use serde::Serialize;
use test_case::test_case;

/// Far above any skid margin, so the request programs a real PMU notification.
const TIMEOUT_RCBS: u64 = 100_000;

/// Far more instructions than separate the target branch from `getpid` when
/// the guest takes a single further branch.
const OFFSET_INSTRS: u64 = 100;

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
enum Report {
    TimerEvent,
    OvertakingSyscall,
}

#[derive(Debug, Default)]
struct Log {
    timer_events: AtomicU64,
    overtaking_syscalls: AtomicU64,
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Request = Report;
    type Response = ();
    /// The precise request's instruction offset past the target branch.
    type Config = u64;

    async fn receive_rpc(&self, _from: Pid, report: Report) {
        let counter = match report {
            Report::TimerEvent => &self.timer_events,
            Report::OvertakingSyscall => &self.overtaking_syscalls,
        };
        counter.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Default, Clone)]
struct PreciseTimerTool;

#[reverie::tool]
impl Tool for PreciseTimerTool {
    type GlobalState = Log;
    type ThreadState = ();

    fn subscriptions(_cfg: &u64) -> Subscription {
        let mut s = Subscription::none();
        s.syscalls([Sysno::clock_getres, Sysno::getpid, Sysno::rt_sigprocmask]);
        s
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall.number() {
            Sysno::clock_getres => {
                let schedule = match *guest.config() {
                    0 => TimerSchedule::Rcbs(TIMEOUT_RCBS),
                    offset => TimerSchedule::RcbsAndInstructions(TIMEOUT_RCBS, offset),
                };
                guest.set_timer_precise(schedule).unwrap();
                Ok(0)
            }
            Sysno::getpid => {
                guest.send_rpc(Report::OvertakingSyscall).await;
                guest.tail_inject(syscall).await
            }
            _ => guest.tail_inject(syscall).await,
        }
    }

    async fn handle_timer_event<T: Guest<Self>>(&self, guest: &mut T) {
        guest.send_rpc(Report::TimerEvent).await;
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

fn block_timer_signal() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGSTKFLT);
        assert_eq!(
            libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()),
            0
        );
    }
}

/// The skid witness counter is process global; each case owns it while it
/// runs. No other test in this binary uses it.
static WITNESS: Mutex<()> = Mutex::new(());

// `do_branches(n)` executes `n + 1` conditional branches, so `TIMEOUT_RCBS - 1`
// lands exactly on the target when the guest reaches `getpid`, and one fewer
// stops a single branch short. Blocking the signal also blocks the artificial
// kick, so no case can deliver the event.
//
// With an instruction offset, the event is due `OFFSET_INSTRS` instructions
// after the target branch. One further branch leaves the guest well short of
// that, while `OFFSET_INSTRS` further branches prove it got there.
#[test_case(TIMEOUT_RCBS / 2, 0, 0; "well before the target")]
#[test_case(TIMEOUT_RCBS - 2, 0, 0; "one branch before the target")]
#[test_case(TIMEOUT_RCBS - 1, 0, 1; "exactly at the target")]
#[test_case(TIMEOUT_RCBS * 2, 0, 1; "well past the target")]
#[test_case(TIMEOUT_RCBS - 1, OFFSET_INSTRS, 0; "at the target with the offset outstanding")]
#[test_case(TIMEOUT_RCBS, OFFSET_INSTRS, 0; "one branch past the target with the offset outstanding")]
#[test_case(TIMEOUT_RCBS - 1 + OFFSET_INSTRS, OFFSET_INSTRS, 1; "branches past the target cover the offset")]
#[test_case(TIMEOUT_RCBS * 2, OFFSET_INSTRS, 1; "well past the target and the offset")]
fn overtaken_precise_event_is_witnessed(branches: u64, offset: u64, witnesses: u64) {
    ret_without_perf!();
    let _owner = WITNESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = reverie::take_skid_overshoot_count();

    let log = check_fn_with_config::<PreciseTimerTool, _>(
        move || {
            block_timer_signal();
            unsafe { syscall_no_branches(Sysno::clock_getres) };
            do_branches(branches);
            unsafe { syscall_no_branches(Sysno::getpid) };
        },
        offset,
        true,
    );

    assert_eq!(
        log.overtaking_syscalls.load(Ordering::SeqCst),
        1,
        "the guest must reach the syscall stop exactly once"
    );
    assert_eq!(
        log.timer_events.load(Ordering::SeqCst),
        0,
        "the syscall cancels the undelivered event"
    );
    assert_eq!(
        reverie::take_skid_overshoot_count(),
        witnesses,
        "a precise event overtaken after its target must be witnessed exactly once"
    );
}
