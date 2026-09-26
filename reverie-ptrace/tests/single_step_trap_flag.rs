/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A precise timer event single-steps the guest to its target. Each step runs
//! one instruction with the trap flag (TF) set, and that TF must not reach the
//! guest. It could leak in two ways:
//!
//! - A stepped `pushf` stores flags with TF=1 although the guest never set it.
//!   If the guest later restores that image with `popf` while it is not being
//!   stepped, TF stays set and every instruction after it traps. LiteInst's
//!   trampolines save and restore flags exactly that way.
//! - After a stepped `popf`, Linux treats TF as the guest's own and leaves it
//!   set when the guest resumes, even though `popf` loaded flags without TF.
//!
//! Reverie resumes the guest after each SIGTRAP it cannot attribute, so the
//! extra traps a leaked TF causes do not kill the guest, which sees the leak
//! only by reading its flags. Each guest here runs its loop across the timer's target
//! and pushes its flags in every round; the `pushf` guest's loop stores them
//! with a stepped `pushf`, and the `popf` guest's loop reads them after a
//! `popf` of flags without TF.

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
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_ptrace::ret_without_perf;
use reverie_ptrace::testing::check_fn_with_config;
use serde::Deserialize;
use serde::Serialize;
use test_case::test_case;

/// The x86 trap flag in RFLAGS.
const TRAP_FLAG: u64 = 0x100;

/// Flags without TF for the guest to load: IF and the reserved bit 1.
const CLEAN_FLAGS: u64 = 0x202;

/// Far above any skid margin, so the request programs a real PMU notification.
const MANY_RCBS: u64 = 10_000;

/// Low enough that the timer is delivered with an artificial signal.
const LESS_RCBS: u64 = 15;

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
struct Schedule {
    rcbs: u64,
    /// Instructions past the target branch, if any.
    instructions: Option<u64>,
}

#[derive(Debug, Default)]
struct Log {
    timer_events: AtomicU64,
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Request = ();
    type Response = ();
    type Config = Schedule;

    async fn receive_rpc(&self, _from: Pid, _: ()) {
        self.timer_events.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Default, Clone)]
struct PreciseTimerTool;

#[reverie::tool]
impl Tool for PreciseTimerTool {
    type GlobalState = Log;
    type ThreadState = ();

    fn subscriptions(_cfg: &Schedule) -> Subscription {
        let mut s = Subscription::none();
        s.syscalls([Sysno::clock_getres]);
        s
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall.number() {
            Sysno::clock_getres => {
                let config = *guest.config();
                let schedule = match config.instructions {
                    None => TimerSchedule::Rcbs(config.rcbs),
                    Some(instructions) => {
                        TimerSchedule::RcbsAndInstructions(config.rcbs, instructions)
                    }
                };
                guest.set_timer_precise(schedule).unwrap();
                Ok(0)
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

/// Runs `iterations` rounds of `pushfq` and `popfq`, one conditional branch
/// each, and returns the OR of every flags image the guest pushed.
#[inline(always)]
fn pushf_loop(iterations: u64) -> u64 {
    let mut pushed: u64 = 0;
    unsafe {
        core::arch::asm!(
            "2:",
            "pushfq",
            "pop {image}",
            "or {pushed}, {image}",
            "pushfq",
            "popfq",
            "dec {n}",
            "jnz 2b",
            n = inout(reg) iterations => _,
            pushed = inout(reg) pushed,
            image = out(reg) _,
        );
    }
    pushed
}

/// Runs `iterations` rounds of `popfq` with flags that do not set TF, one
/// conditional branch each, and returns the OR of the flags after every
/// `popfq`.
#[inline(always)]
fn popf_loop(iterations: u64) -> u64 {
    let mut flags: u64 = 0;
    unsafe {
        core::arch::asm!(
            "2:",
            "push {image}",
            "popfq",
            "pushfq",
            "pop {loaded}",
            "or {flags}, {loaded}",
            "dec {n}",
            "jnz 2b",
            n = inout(reg) iterations => _,
            image = in(reg) CLEAN_FLAGS,
            flags = inout(reg) flags,
            loaded = out(reg) _,
        );
    }
    flags
}

// The loop runs twice as many branches as the timer's target, so the target
// and every step before it fall inside the loop. The loop body has seven
// instructions, so the instruction offsets 1 to 7 end the steps on each of
// them, including each `pushfq`.
#[test_case(MANY_RCBS, None; "perf signal")]
#[test_case(LESS_RCBS, None; "artificial signal")]
#[test_case(MANY_RCBS, Some(1); "one instruction past the target")]
#[test_case(MANY_RCBS, Some(2); "two instructions past the target")]
#[test_case(MANY_RCBS, Some(3); "three instructions past the target")]
#[test_case(MANY_RCBS, Some(4); "four instructions past the target")]
#[test_case(MANY_RCBS, Some(5); "five instructions past the target")]
#[test_case(MANY_RCBS, Some(6); "six instructions past the target")]
#[test_case(MANY_RCBS, Some(7); "seven instructions past the target")]
fn stepped_pushf_does_not_leak_the_trap_flag(rcbs: u64, instructions: Option<u64>) {
    ret_without_perf!();
    let iterations = 2 * rcbs;

    let log = check_fn_with_config::<PreciseTimerTool, _>(
        move || {
            unsafe { syscall_no_branches(Sysno::clock_getres) };
            let pushed = pushf_loop(iterations);
            assert_eq!(
                pushed & TRAP_FLAG,
                0,
                "pushf stored flags with TF set: {pushed:#x}"
            );
        },
        Schedule { rcbs, instructions },
        true,
    );

    assert_eq!(
        log.timer_events.load(Ordering::SeqCst),
        1,
        "the timer must fire inside the loop"
    );
}

// The loop body has seven instructions; offsets 1 to 7 end the steps on each.
#[test_case(MANY_RCBS, None; "perf signal")]
#[test_case(LESS_RCBS, None; "artificial signal")]
#[test_case(MANY_RCBS, Some(1); "one instruction past the target")]
#[test_case(MANY_RCBS, Some(2); "two instructions past the target")]
#[test_case(MANY_RCBS, Some(3); "three instructions past the target")]
#[test_case(MANY_RCBS, Some(4); "four instructions past the target")]
#[test_case(MANY_RCBS, Some(5); "five instructions past the target")]
#[test_case(MANY_RCBS, Some(6); "six instructions past the target")]
#[test_case(MANY_RCBS, Some(7); "seven instructions past the target")]
fn stepped_popf_does_not_leak_the_trap_flag(rcbs: u64, instructions: Option<u64>) {
    ret_without_perf!();
    let iterations = 2 * rcbs;

    let log = check_fn_with_config::<PreciseTimerTool, _>(
        move || {
            unsafe { syscall_no_branches(Sysno::clock_getres) };
            let flags = popf_loop(iterations);
            assert_eq!(
                flags & TRAP_FLAG,
                0,
                "popf loaded flags with TF set: {flags:#x}"
            );
        },
        Schedule { rcbs, instructions },
        true,
    );

    assert_eq!(
        log.timer_events.load(Ordering::SeqCst),
        1,
        "the timer must fire inside the loop"
    );
}
