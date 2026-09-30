/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A precise timer event single-steps the guest to its target, and each step
//! ends in a SIGTRAP stop. An instruction that raises a SIGTRAP of its own,
//! such as `int3`, ends in a SIGTRAP stop too, and the stepping must not take
//! that stop for its step's. If it did, the trap would never reach the code
//! that handles SIGTRAPs, and the timer event would fire as if no trap had
//! come before it.
//!
//! When the timer is not stepping, a trap's stop cancels a timer event that
//! has not fired yet, like any other stop. The steps start wherever the
//! processor happens to raise the PMU signal, so a trap that the steps run
//! must cancel the event too, or whether the event fires would depend on how
//! late the signal came.
//!
//! Each guest makes the syscall at which the Tool requests the timer, then
//! runs two loops with one conditional branch per round and the instruction
//! under test between them.

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
use test_case::test_case;

/// Above the largest skid margin in Reverie's PMU table, 10,000 RCBs, so the
/// request programs a real PMU notification on every host in the table. The
/// steps then start wherever the processor raises its signal, up to the skid
/// margin before the target.
const PERF_RCBS: u64 = 30_000;

/// Low enough that the timer is delivered with an artificial signal. The
/// stepping then starts at the syscall that requested the timer, so every
/// instruction from there to the target is stepped.
const LESS_RCBS: u64 = 15;

#[derive(Debug, Default)]
struct TimerEvents(AtomicU64);

#[reverie::global_tool]
impl GlobalTool for TimerEvents {
    type Request = ();
    type Response = ();
    /// The timer's RCBs from the request to the target.
    type Config = u64;

    async fn receive_rpc(&self, _from: Pid, _request: ()) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Default, Clone)]
struct PreciseTimerTool;

#[reverie::tool]
impl Tool for PreciseTimerTool {
    type GlobalState = TimerEvents;
    type ThreadState = ();

    fn subscriptions(_cfg: &u64) -> Subscription {
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
                let rcbs = *guest.config();
                guest.set_timer_precise(TimerSchedule::Rcbs(rcbs)).unwrap();
                Ok(0)
            }
            _ => guest.tail_inject(syscall).await,
        }
    }

    async fn handle_timer_event<T: Guest<Self>>(&self, guest: &mut T) {
        guest.send_rpc(()).await;
    }
}

/// The instruction between the guest's two loops.
#[derive(Debug, Clone, Copy)]
enum Between {
    /// A `nop`, which does not trap.
    Nop,
    /// `int3`, which Linux reports as a SIGTRAP with SI_KERNEL.
    Int3,
    /// `icebp`, which Linux reports as a SIGTRAP with TRAP_BRKPT when the
    /// processor sets no DR6 status bit for it. An AMD EPYC 9D85 sets none
    /// even when it single-steps the instruction.
    Icebp,
    /// A `tgkill` of SIGTRAP to the guest's own thread, which Linux reports
    /// with SI_TKILL. A step of its `syscall` would report TRAP_BRKPT as the
    /// syscall returns, but the guest's SIGTRAP is already pending then, and
    /// Linux does not queue a second.
    Tgkill,
}

/// Makes the `clock_getres` at which the Tool requests the timer, then runs
/// `$before` rounds of a loop with one conditional branch each, then the
/// given instructions, then `$after` more rounds. There is no conditional
/// branch between the syscall and the loop, so the loop's branches are the
/// first after the request.
macro_rules! between_loops {
    ($before:expr, $after:expr, [$($instruction:literal),+] $(, $($operand:tt)+)?) => {
        unsafe {
            core::arch::asm!(
                "syscall",
                "2:",
                "dec {before}",
                "jnz 2b",
                $($instruction,)+
                "3:",
                "dec {after}",
                "jnz 3b",
                before = inout(reg) $before => _,
                after = inout(reg) $after => _,
                $($($operand)+,)?
                inlateout("rax") Sysno::clock_getres as usize => _,
                inlateout("rdi") 0usize => _,
                inlateout("rsi") 0usize => _,
                out("rdx") _,
                out("rcx") _,
                out("r11") _,
            )
        }
    };
}

impl Between {
    /// Runs the guest with `before` rounds before the instruction and `after`
    /// rounds after it.
    fn run(self, before: u64, after: u64) {
        match self {
            Between::Nop => between_loops!(before, after, ["nop"]),
            Between::Int3 => between_loops!(before, after, ["int3"]),
            // `icebp`, also known as `int1`.
            Between::Icebp => between_loops!(before, after, [".byte 0xf1"]),
            Between::Tgkill => {
                let pid = unsafe { libc::getpid() } as usize;
                let tid = unsafe { libc::gettid() } as usize;
                between_loops!(
                    before,
                    after,
                    [
                        "mov eax, {tgkill}",
                        "mov rdi, {pid}",
                        "mov rsi, {tid}",
                        "mov edx, {sigtrap}",
                        "syscall"
                    ],
                    pid = in(reg) pid,
                    tid = in(reg) tid,
                    tgkill = const libc::SYS_tgkill,
                    sigtrap = const libc::SIGTRAP
                )
            }
        }
    }
}

fn timer_events(between: Between, rcbs: u64, before: u64) -> u64 {
    let events = check_fn_with_config::<PreciseTimerTool, _>(
        move || between.run(before, 2 * rcbs - before),
        rcbs,
        true,
    );
    events.0.load(Ordering::SeqCst)
}

// Each guest traps one conditional branch before the target. With an
// artificial signal the steps start at the request, so they run the trap.
// With a PMU signal they run it as long as the processor's skid stays below
// the margin. A later signal comes after the trap's stop, and that stop
// cancels the event before any step.
#[test_case(Between::Int3, LESS_RCBS; "int3, artificial signal")]
#[test_case(Between::Int3, PERF_RCBS; "int3, perf signal")]
#[test_case(Between::Icebp, LESS_RCBS; "icebp, artificial signal")]
#[test_case(Between::Icebp, PERF_RCBS; "icebp, perf signal")]
#[test_case(Between::Tgkill, LESS_RCBS; "tgkill, artificial signal")]
#[test_case(Between::Tgkill, PERF_RCBS; "tgkill, perf signal")]
fn a_trap_before_the_target_cancels_the_timer(between: Between, rcbs: u64) {
    ret_without_perf!();
    assert_eq!(
        timer_events(between, rcbs, rcbs - 1),
        0,
        "the trap's stop must cancel the timer"
    );
}

// The same guests without a trap, where the timer fires one branch after the
// `nop`.
#[test_case(LESS_RCBS; "artificial signal")]
#[test_case(PERF_RCBS; "perf signal")]
fn the_timer_fires_without_a_trap(rcbs: u64) {
    ret_without_perf!();
    assert_eq!(
        timer_events(Between::Nop, rcbs, rcbs - 1),
        1,
        "the timer must fire inside the second loop"
    );
}

// The trap is the instruction just past the target, so the timer fires before
// the guest runs it. Only the artificial signal starts the steps at a known
// point: a PMU signal more than the skid margin late would come after the
// trap's stop.
#[test_case(Between::Int3; "int3")]
#[test_case(Between::Icebp; "icebp")]
#[test_case(Between::Tgkill; "tgkill")]
fn a_trap_past_the_target_does_not_cancel_the_timer(between: Between) {
    ret_without_perf!();
    assert_eq!(
        timer_events(between, LESS_RCBS, LESS_RCBS),
        1,
        "the timer must fire before the trap"
    );
}
