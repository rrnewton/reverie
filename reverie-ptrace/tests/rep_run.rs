/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! In the stretch the timer single steps toward a precise event's target, a
//! `rep` string instruction runs to its end at a hardware breakpoint instead
//! of one stop per iteration. Its iterations retire no conditional branch, so
//! the delivery point must be exactly the one that stepping each iteration
//! reaches: the same clock and the same rip.
//!
//! This file holds one test, because rep runs are switched off for the whole
//! process for its second half.

#![cfg(target_arch = "x86_64")]

use std::sync::Mutex;

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
use reverie_ptrace::testing::PreciseTimerSteps;
use reverie_ptrace::testing::check_fn_with_config;
use reverie_ptrace::testing::precise_timer_steps;
use reverie_ptrace::testing::set_rep_runs;

/// The precise timer targets the guest requests, one at a time. Each loop
/// iteration of `copy_and_branch` copies with one `rep movsb` and then
/// retires three conditional branches, so consecutive targets land on each
/// of them, and the stretch single stepped before each starts at a `rep`.
const TARGETS: [u64; 6] = [3_000, 3_001, 3_002, 3_003, 3_004, 3_005];

/// Bytes each `rep movsb` copies: its iteration count.
const COPY: usize = 4096;

/// (clock from the request, rip) at each timer event.
#[derive(Debug, Default)]
struct TimerEvents(Mutex<Vec<(u64, u64)>>);

#[reverie::global_tool]
impl GlobalTool for TimerEvents {
    type Request = (u64, u64);
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, event: (u64, u64)) {
        self.0.lock().unwrap().push(event);
    }
}

#[derive(Debug, Default, Clone)]
struct PreciseTimerTool;

#[reverie::tool]
impl Tool for PreciseTimerTool {
    type GlobalState = TimerEvents;
    /// The clock at the request.
    type ThreadState = u64;

    fn subscriptions(_cfg: &()) -> Subscription {
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
                *guest.thread_state_mut() = guest.read_clock().unwrap();
                let target = syscall.into_parts().1.arg0 as u64;
                guest
                    .set_timer_precise(TimerSchedule::Rcbs(target))
                    .unwrap();
                Ok(0)
            }
            _ => guest.tail_inject(syscall).await,
        }
    }

    async fn handle_timer_event<T: Guest<Self>>(&self, guest: &mut T) {
        let clock = guest.read_clock().unwrap() - *guest.thread_state();
        let rip = guest.regs().await.rip;
        guest.send_rpc((clock, rip)).await;
    }
}

/// The `clock_getres` at which the Tool requests a timer `target` RCBs
/// ahead, with no conditional branch between it and the caller.
fn request(target: u64) {
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") Sysno::clock_getres as usize => _,
            in("rdi") target as usize,
            in("rsi") 0usize,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
}

/// Copies `COPY` bytes from `source` to `destination` with one `rep movsb`
/// per iteration, then retires three conditional branches: a `je` never
/// taken, a `jz` taken on even counts, and the loop's `jnz`. It allocates
/// nothing: a guest is forked from the test process, whose heap differs
/// between the two halves of the test, so an allocation after the request
/// would retire a different number of branches in each.
#[inline(never)]
fn copy_and_branch(iterations: u64, source: &[u8], destination: &mut [u8]) {
    unsafe {
        core::arch::asm!(
            "2:",
            "mov rsi, {src}",
            "mov rdi, {dst}",
            "mov rcx, {len}",
            "rep movsb",
            "cmp {n}, 0",
            "je 3f",
            "test {n}, 1",
            "jz 4f",
            "nop",
            "4:",
            "sub {n}, 1",
            "jnz 2b",
            "3:",
            n = inout(reg) iterations => _,
            src = in(reg) source.as_ptr(),
            dst = in(reg) destination.as_mut_ptr(),
            len = in(reg) COPY,
            out("rsi") _,
            out("rdi") _,
            out("rcx") _,
            options(nostack),
        );
    }
}

fn run() -> (Vec<(u64, u64)>, PreciseTimerSteps, u64) {
    let _ = reverie::take_skid_overshoot_count();
    let before = precise_timer_steps();
    let events = check_fn_with_config::<PreciseTimerTool, _>(
        || {
            let source = vec![7u8; COPY];
            let mut destination = vec![0u8; COPY];
            for target in TARGETS {
                request(target);
                copy_and_branch(target, &source, &mut destination);
            }
            assert_eq!(destination, source);
        },
        (),
        true,
    );
    let steps = precise_timer_steps() - before;
    let witnesses = reverie::take_skid_overshoot_count();
    (events.0.into_inner().unwrap(), steps, witnesses)
}

#[test]
fn rep_runs_deliver_where_iteration_steps_do() {
    ret_without_perf!();
    set_rep_runs(true);
    let (blocked, block_steps, block_witnesses) = run();
    set_rep_runs(false);
    let (single, single_steps, single_witnesses) = run();
    set_rep_runs(true);
    eprintln!("with rep runs: {block_steps:?}, events {blocked:?}");
    eprintln!("each iteration stepped: {single_steps:?}, events {single:?}");
    assert_eq!(blocked.len(), TARGETS.len(), "each timer fires once");
    assert_eq!(single.len(), TARGETS.len(), "each timer fires once");
    assert_eq!(single_steps.rep, 0, "switched off, no step is a rep run");
    assert!(
        block_steps.rep > 0,
        "the stretch before a target runs a rep to its end: {block_steps:?}"
    );
    assert!(
        block_steps.instruction + COPY as u64 <= single_steps.instruction,
        "a rep run replaces a step per iteration: {block_steps:?} against {single_steps:?}"
    );
    if block_witnesses + single_witnesses > 0 {
        // A late PMU notification is delivered at the observed counter, so
        // that event's point is the host's, not the stepping's.
        eprintln!(
            "skipping the comparison: {block_witnesses} and {single_witnesses} witnessed skid overshoots"
        );
        return;
    }
    for (event, target) in blocked.iter().zip(TARGETS) {
        assert_eq!(
            event.0, target,
            "each event fires at its target: {blocked:?}"
        );
    }
    assert_eq!(
        blocked, single,
        "rep runs must deliver at the clock and rip that stepping each iteration reaches"
    );
    let rips: std::collections::BTreeSet<u64> = blocked.iter().map(|event| event.1).collect();
    assert!(
        rips.len() >= 3,
        "the targets land on each of the loop's three branches: {blocked:?}"
    );
}
