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
//! - After a stepped `popf`, Linux treats TF as the guest's own for the rest
//!   of the stepping, even though `popf` loaded flags without TF, and leaves
//!   it set when the guest resumes.
//!
//! A stepped `syscall` also saves TF in r11, which the guest reads when the
//! syscall returns. A traced syscall ends the stepping at its seccomp stop
//! instead of a SIGTRAP, and TF must not leak there either. A load of SS holds
//! the step's trap back until the next instruction has run, so one step can
//! run a `pushf` or `popf` as well.
//!
//! The fix must not take TF from a guest that sets it itself: once a stepped
//! `popf`, `iret` or `rt_sigreturn` loads TF, the steps after it leave it
//! alone. `rt_sigreturn` also restores r11 from the signal frame, which the
//! fix must leave as the frame has it.
//!
//! Reverie resumes the guest after each SIGTRAP it cannot attribute, so the
//! extra traps a leaked TF causes do not kill the guest, which sees the leak
//! only by reading its flags. The `pushf` and `popf` guests run their loops
//! across the timer's target and push their flags in every round; the `pushf`
//! guest's loop stores them with a stepped `pushf`, and the `popf` guest's
//! loop reads them after a `popf` of flags without TF. The other guests run
//! the instruction under test once, before the target or just past it, and
//! then push their flags.

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

/// Low enough that the timer is delivered with an artificial signal. The
/// stepping then starts at the syscall that requested the timer, so every
/// instruction from there to the target is stepped, however late the
/// processor would have raised a PMU signal.
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
        s.syscalls([Sysno::clock_getres, Sysno::getppid]);
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

/// Makes the `clock_getres` at which the Tool requests the timer, then runs
/// `iterations` rounds of a loop with one conditional branch each, so the
/// last round's `jnz` is the timer's target. After the loop the guest sets
/// TF itself with `popfq`, pushes its flags, and clears TF again. Returns the
/// flags it pushed. There is no conditional branch between the syscall and
/// the loop, so the target is exact.
#[inline(always)]
fn own_trap_flag_after_loop(iterations: u64) -> u64 {
    let pushed: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            "2:",
            "dec {n}",
            "jnz 2b",
            "push {own}",
            "popfq",
            "pushfq",
            "pop {pushed}",
            "push {clean}",
            "popfq",
            inlateout("rax") Sysno::clock_getres as usize => _,
            in("rdi") 0usize,
            in("rsi") 0usize,
            // Not lateout: the syscall clobbers them before the inputs below
            // are read.
            out("rcx") _,
            out("r11") _,
            n = inout(reg) iterations => _,
            own = in(reg) CLEAN_FLAGS | TRAP_FLAG,
            clean = in(reg) CLEAN_FLAGS,
            pushed = out(reg) pushed,
        );
    }
    pushed
}

// The guest's own TF must survive the stepping. The steps end at the target
// or 1 to 8 instructions past it: offset 2 steps the `popfq` that sets TF,
// and offsets 3 to 8 step the `pushfq` after it as well. While its own TF is
// set the guest takes SIGTRAPs, which Reverie does not deliver.
//
// The artificial signal starts the stepping at the `clock_getres` return,
// before the guest sets TF. A PMU signal that Linux raises more than the skid
// margin late would arrive after the guest had set TF; the SIGTRAP stop of the
// guest's own TF would then reach Reverie first and cancel the timer, which
// loses the event. That loss is older than this test and is not what it
// checks.
#[test_case(None; "at the target")]
#[test_case(Some(1); "one instruction past the target")]
#[test_case(Some(2); "two instructions past the target")]
#[test_case(Some(3); "three instructions past the target")]
#[test_case(Some(4); "four instructions past the target")]
#[test_case(Some(5); "five instructions past the target")]
#[test_case(Some(6); "six instructions past the target")]
#[test_case(Some(7); "seven instructions past the target")]
#[test_case(Some(8); "eight instructions past the target")]
fn stepping_keeps_the_guests_own_trap_flag(instructions: Option<u64>) {
    ret_without_perf!();

    let log = check_fn_with_config::<PreciseTimerTool, _>(
        move || {
            let pushed = own_trap_flag_after_loop(LESS_RCBS);
            assert_eq!(
                pushed & TRAP_FLAG,
                TRAP_FLAG,
                "the guest set TF but pushf stored {pushed:#x}"
            );
        },
        Schedule {
            rcbs: LESS_RCBS,
            instructions,
        },
        true,
    );

    assert_eq!(
        log.timer_events.load(Ordering::SeqCst),
        1,
        "the timer must fire at the end of the loop"
    );
}

/// Makes the `clock_getres` at which the Tool requests the timer, runs
/// `iterations` rounds of a loop with one conditional branch each, loads flags
/// without TF with `popfq`, and makes a syscall with the number `no` in rax.
/// Then it runs `after` more rounds and pushes its flags. Returns the flags it
/// pushed and the value the kernel left in r11, which the `syscall`
/// instruction loads with RFLAGS.
#[inline(always)]
fn syscall_after_popf(iterations: u64, no: u64, after: u64) -> (u64, u64) {
    let pushed: u64;
    let r11: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            "2:",
            "dec {n}",
            "jnz 2b",
            "push {clean}",
            "popfq",
            "mov rax, {no}",
            "syscall",
            "mov {r11}, r11",
            "3:",
            "dec {after}",
            "jnz 3b",
            "pushfq",
            "pop {pushed}",
            inlateout("rax") Sysno::clock_getres as usize => _,
            in("rdi") 0usize,
            in("rsi") 0usize,
            out("rcx") _,
            out("r11") _,
            n = inout(reg) iterations => _,
            after = inout(reg) after => _,
            no = in(reg) no,
            clean = in(reg) CLEAN_FLAGS,
            r11 = out(reg) r11,
            pushed = out(reg) pushed,
        );
    }
    (pushed, r11)
}

// The steps run the `popfq` and then the syscall, a few branches before the
// target. `getppid` is traced, so its seccomp stop ends the stepping before
// the target. With the artificial signal the stepping starts at the
// `clock_getres` return, so the seccomp stop always ends a step.
//
// No timer event is expected because Reverie cancels a timer at any stop that
// reaches the Tool before the event fires, as `EventStatus` documents. That is
// the existing contract, recorded here, not the outcome this test wants: the
// review of https://github.com/rrnewton/reverie/pull/654 found that it loses
// the event.
#[test_case(MANY_RCBS; "perf signal")]
#[test_case(LESS_RCBS; "artificial signal")]
fn stepping_that_ends_at_a_syscall_stop_does_not_leak_the_trap_flag(rcbs: u64) {
    ret_without_perf!();

    let log = check_fn_with_config::<PreciseTimerTool, _>(
        move || {
            let (pushed, r11) = syscall_after_popf(rcbs - 4, Sysno::getppid as u64, 8);
            assert_eq!(
                pushed & TRAP_FLAG,
                0,
                "pushf stored flags with TF set: {pushed:#x}"
            );
            assert_eq!(r11 & TRAP_FLAG, 0, "the syscall saved TF in r11: {r11:#x}");
        },
        Schedule {
            rcbs,
            instructions: None,
        },
        true,
    );

    assert_eq!(
        log.timer_events.load(Ordering::SeqCst),
        0,
        "the syscall's stop must end the stepping before the target"
    );
}

/// `getpid` with bits set above the low 32, which Linux ignores when it reads
/// the syscall number. orig_rax keeps them, so it is negative.
const GETPID_WITH_HIGH_BITS: u64 = 0xffff_ffff_0000_0000 | Sysno::getpid as u64;

// The steps run the `popfq` and an untraced syscall, and the target is in the
// loop after it. rax = -1 is no syscall, and Linux fails it with ENOSYS; like
// the high bits above, it leaves orig_rax negative.
#[test_case(MANY_RCBS, Sysno::getpid as u64; "getpid, perf signal")]
#[test_case(LESS_RCBS, Sysno::getpid as u64; "getpid, artificial signal")]
#[test_case(MANY_RCBS, GETPID_WITH_HIGH_BITS; "getpid with high bits, perf signal")]
#[test_case(LESS_RCBS, GETPID_WITH_HIGH_BITS; "getpid with high bits, artificial signal")]
#[test_case(MANY_RCBS, u64::MAX; "invalid number, perf signal")]
#[test_case(LESS_RCBS, u64::MAX; "invalid number, artificial signal")]
fn stepped_syscall_does_not_leak_the_trap_flag(rcbs: u64, no: u64) {
    ret_without_perf!();

    let log = check_fn_with_config::<PreciseTimerTool, _>(
        move || {
            let (pushed, r11) = syscall_after_popf(rcbs - 4, no, 8);
            assert_eq!(
                pushed & TRAP_FLAG,
                0,
                "pushf stored flags with TF set: {pushed:#x}"
            );
            assert_eq!(r11 & TRAP_FLAG, 0, "the syscall saved TF in r11: {r11:#x}");
        },
        Schedule {
            rcbs,
            instructions: None,
        },
        true,
    );

    assert_eq!(
        log.timer_events.load(Ordering::SeqCst),
        1,
        "the timer must fire in the loop after the syscall"
    );
}

/// Makes the `clock_getres` at which the Tool requests the timer, pushes its
/// flags with the two-byte `pushfw`, and then runs `after` rounds of a loop
/// with one conditional branch each. Returns the flags it pushed.
#[inline(always)]
fn pushfw_before_loop(after: u64) -> u64 {
    let pushed: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            "pushfw",
            "pop {pushed:x}",
            "2:",
            "dec {n}",
            "jnz 2b",
            inlateout("rax") Sysno::clock_getres as usize => _,
            in("rdi") 0usize,
            in("rsi") 0usize,
            out("rcx") _,
            out("r11") _,
            n = inout(reg) after => _,
            pushed = out(reg) pushed,
        );
    }
    pushed & 0xffff
}

// The artificial signal starts the stepping at the `clock_getres` return, so
// the `pushfw` is stepped, and the target is in the loop after it.
#[test]
fn stepped_pushfw_does_not_leak_the_trap_flag() {
    ret_without_perf!();

    let log = check_fn_with_config::<PreciseTimerTool, _>(
        move || {
            let pushed = pushfw_before_loop(2 * LESS_RCBS);
            assert_eq!(
                pushed & TRAP_FLAG,
                0,
                "pushfw stored flags with TF set: {pushed:#x}"
            );
        },
        Schedule {
            rcbs: LESS_RCBS,
            instructions: None,
        },
        true,
    );

    assert_eq!(
        log.timer_events.load(Ordering::SeqCst),
        1,
        "the timer must fire inside the loop"
    );
}

/// Makes the `clock_getres` at which the Tool requests the timer, and then
/// loads SS with its own value just before `pushfq`. A load of SS holds the
/// debug trap back for one instruction, so a single step runs both. Then it
/// runs `after` rounds of a loop with one conditional branch each. Returns the
/// flags it pushed.
#[inline(always)]
fn mov_ss_pushf_before_loop(after: u64) -> u64 {
    let pushed: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            "mov {ss:e}, ss",
            "mov ss, {ss:e}",
            "pushfq",
            "pop {pushed}",
            "2:",
            "dec {n}",
            "jnz 2b",
            inlateout("rax") Sysno::clock_getres as usize => _,
            in("rdi") 0usize,
            in("rsi") 0usize,
            out("rcx") _,
            out("r11") _,
            n = inout(reg) after => _,
            ss = out(reg) _,
            pushed = out(reg) pushed,
        );
    }
    pushed
}

// One step runs the load of SS and the `pushfq` after it, and the target is in
// the loop after them.
#[test]
fn stepped_pushf_after_a_load_of_ss_does_not_leak_the_trap_flag() {
    ret_without_perf!();

    let log = check_fn_with_config::<PreciseTimerTool, _>(
        move || {
            let pushed = mov_ss_pushf_before_loop(2 * LESS_RCBS);
            assert_eq!(
                pushed & TRAP_FLAG,
                0,
                "pushf stored flags with TF set: {pushed:#x}"
            );
        },
        Schedule {
            rcbs: LESS_RCBS,
            instructions: None,
        },
        true,
    );

    assert_eq!(
        log.timer_events.load(Ordering::SeqCst),
        1,
        "the timer must fire inside the loop"
    );
}

/// Makes the `clock_getres` at which the Tool requests the timer, and then
/// loads `flags` with a `popfq` just after a load of SS, so a single step runs
/// both. It pushes the flags it now has, loads flags without TF, and runs
/// `after` rounds of a loop with one conditional branch each. Returns the
/// flags it pushed.
#[inline(always)]
fn mov_ss_popf_before_loop(flags: u64, after: u64) -> u64 {
    let pushed: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            "push {flags}",
            "mov {ss:e}, ss",
            "mov ss, {ss:e}",
            "popfq",
            "pushfq",
            "pop {pushed}",
            "push {clean}",
            "popfq",
            "2:",
            "dec {n}",
            "jnz 2b",
            inlateout("rax") Sysno::clock_getres as usize => _,
            in("rdi") 0usize,
            in("rsi") 0usize,
            out("rcx") _,
            out("r11") _,
            n = inout(reg) after => _,
            flags = in(reg) flags,
            clean = in(reg) CLEAN_FLAGS,
            ss = out(reg) _,
            pushed = out(reg) pushed,
        );
    }
    pushed
}

// One step runs the load of SS and the `popfq` after it. Linux looks only at
// the load of SS, and does not see that the step loads the guest's flags.
#[test_case(CLEAN_FLAGS | TRAP_FLAG; "own trap flag")]
#[test_case(CLEAN_FLAGS; "no trap flag")]
fn stepped_popf_after_a_load_of_ss_loads_the_guests_trap_flag(flags: u64) {
    ret_without_perf!();

    let log = check_fn_with_config::<PreciseTimerTool, _>(
        move || {
            let pushed = mov_ss_popf_before_loop(flags, 2 * LESS_RCBS);
            assert_eq!(
                pushed & TRAP_FLAG,
                flags & TRAP_FLAG,
                "the guest loaded {flags:#x} but pushf stored {pushed:#x}"
            );
        },
        Schedule {
            rcbs: LESS_RCBS,
            instructions: None,
        },
        true,
    );

    assert_eq!(
        log.timer_events.load(Ordering::SeqCst),
        1,
        "the timer must fire inside the loop"
    );
}

/// Makes the `clock_getres` at which the Tool requests the timer, and then
/// loads `flags` with an `iretq` to the next instruction. It pushes the flags
/// it now has, loads flags without TF, and runs `after` rounds of a loop with
/// one conditional branch each. Returns the flags it pushed.
#[inline(always)]
fn iret_before_loop(flags: u64, after: u64) -> u64 {
    let pushed: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            "mov {sp}, rsp",
            "mov {selector:e}, ss",
            "push {selector}",
            "push {sp}",
            "push {flags}",
            "mov {selector:e}, cs",
            "push {selector}",
            "lea {sp}, [rip + 3f]",
            "push {sp}",
            "iretq",
            "3:",
            "pushfq",
            "pop {pushed}",
            "push {clean}",
            "popfq",
            "2:",
            "dec {n}",
            "jnz 2b",
            inlateout("rax") Sysno::clock_getres as usize => _,
            in("rdi") 0usize,
            in("rsi") 0usize,
            out("rcx") _,
            out("r11") _,
            n = inout(reg) after => _,
            flags = in(reg) flags,
            clean = in(reg) CLEAN_FLAGS,
            sp = out(reg) _,
            selector = out(reg) _,
            pushed = out(reg) pushed,
        );
    }
    pushed
}

// The `iretq` is stepped, and the target is in the loop after it. Like
// `popf`, `iret` loads the guest's own flags.
#[test_case(CLEAN_FLAGS | TRAP_FLAG; "own trap flag")]
#[test_case(CLEAN_FLAGS; "no trap flag")]
fn stepped_iret_loads_the_guests_trap_flag(flags: u64) {
    ret_without_perf!();

    let log = check_fn_with_config::<PreciseTimerTool, _>(
        move || {
            let pushed = iret_before_loop(flags, 2 * LESS_RCBS);
            assert_eq!(
                pushed & TRAP_FLAG,
                flags & TRAP_FLAG,
                "the guest loaded {flags:#x} but pushf stored {pushed:#x}"
            );
        },
        Schedule {
            rcbs: LESS_RCBS,
            instructions: None,
        },
        true,
    );

    assert_eq!(
        log.timer_events.load(Ordering::SeqCst),
        1,
        "the timer must fire inside the loop"
    );
}

/// A signal frame as Linux builds it on the stack for a handler, which
/// `rt_sigreturn` reads back: the handler's return address, which its `ret`
/// pops, and then the context to restore.
#[repr(C)]
struct SignalFrame {
    return_address: u64,
    context: libc::ucontext_t,
    info: libc::siginfo_t,
}

/// Where the frame holds the general register `reg`.
const fn frame_register(reg: libc::c_int) -> usize {
    core::mem::offset_of!(SignalFrame, context)
        + core::mem::offset_of!(libc::ucontext_t, uc_mcontext)
        + core::mem::offset_of!(libc::mcontext_t, gregs)
        + reg as usize * core::mem::size_of::<libc::greg_t>()
}

/// The value the signal frame restores to r11. It has bit 8, where `syscall`
/// saves TF, set.
const R11_MARKER: u64 = 0xfeed_0346;

/// Makes the `clock_getres` at which the Tool requests the timer, runs
/// `iterations` rounds of a loop with one conditional branch each, and, if
/// `popf` is set, loads flags without TF with `popfq`. Then it makes
/// `rt_sigreturn` with a signal frame that restores `flags`, r11 and its other
/// registers, and continues at the next instruction. It stores r11 and pushes
/// its flags, loads flags without TF, and runs `after` rounds of the loop.
/// Returns the value of r11 and the flags it pushed.
///
/// The frame keeps the alternate signal stack and the blocked signals as they
/// are. It has no floating-point state, so Linux resets it.
#[inline(always)]
fn rt_sigreturn_after_loop(iterations: u64, popf: bool, flags: u64, after: u64) -> [u64; 2] {
    let mut saved = [0u64; 2];
    // SAFETY: all zeros is a valid value for these C structures.
    let mut frame: SignalFrame = unsafe { core::mem::zeroed() };
    let context = &mut frame.context;
    // SAFETY: both only store into the structures they are given.
    unsafe {
        assert_eq!(
            libc::sigaltstack(core::ptr::null(), &mut context.uc_stack),
            0
        );
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_BLOCK, core::ptr::null(), &mut context.uc_sigmask),
            0
        );
    }
    let (cs, ss): (u64, u64);
    // SAFETY: reads the segment selectors.
    unsafe {
        core::arch::asm!(
            "mov {cs:e}, cs",
            "mov {ss:e}, ss",
            cs = out(reg) cs,
            ss = out(reg) ss,
            options(nomem, nostack, preserves_flags),
        );
    }
    let gregs = &mut context.uc_mcontext.gregs;
    gregs[libc::REG_EFL as usize] = flags as i64;
    // cs is the low 16 bits, and ss the high 16.
    gregs[libc::REG_CSGSFS as usize] = (cs | ss << 48) as i64;
    gregs[libc::REG_R11 as usize] = R11_MARKER as i64;
    gregs[libc::REG_R13 as usize] = saved.as_mut_ptr() as i64;
    gregs[libc::REG_R14 as usize] = after as i64;
    // SAFETY: the frame restores rbx, rbp and rsp as they are at the
    // `rt_sigreturn`, and every register it changes is marked clobbered.
    unsafe {
        core::arch::asm!(
            "syscall",
            "2:",
            "dec r8",
            "jnz 2b",
            "test r9, r9",
            "jz 3f",
            "push r10",
            "popfq",
            "3:",
            "mov [r12 + {rbx}], rbx",
            "mov [r12 + {rbp}], rbp",
            "mov [r12 + {rsp}], rsp",
            "lea rax, [rip + 4f]",
            "mov [r12 + {rip}], rax",
            "lea rsp, [r12 + {context}]",
            "mov eax, {rt_sigreturn}",
            "syscall",
            "4:",
            "mov [r13], r11",
            "pushfq",
            "pop qword ptr [r13 + 8]",
            "push {clean}",
            "popfq",
            "5:",
            "dec r14",
            "jnz 5b",
            rbx = const frame_register(libc::REG_RBX),
            rbp = const frame_register(libc::REG_RBP),
            rsp = const frame_register(libc::REG_RSP),
            rip = const frame_register(libc::REG_RIP),
            context = const core::mem::offset_of!(SignalFrame, context),
            rt_sigreturn = const libc::SYS_rt_sigreturn,
            clean = const CLEAN_FLAGS,
            inlateout("rax") Sysno::clock_getres as usize => _,
            inlateout("rdi") 0usize => _,
            inlateout("rsi") 0usize => _,
            out("rcx") _,
            out("rdx") _,
            inout("r8") iterations => _,
            inout("r9") popf as u64 => _,
            inout("r10") CLEAN_FLAGS => _,
            out("r11") _,
            inout("r12") &mut frame as *mut SignalFrame => _,
            inout("r13") saved.as_mut_ptr() => _,
            inout("r14") after => _,
            out("r15") _,
            clobber_abi("C"),
        );
    }
    saved
}

// The steps run the loop, the `popfq` if there is one, and `rt_sigreturn`,
// and the target is in the loop after it. `rt_sigreturn` restores r11 and
// RFLAGS from the frame. TF in the frame is the guest's own, and a TF-like
// bit in r11 is only the frame's value.
#[test_case(false, CLEAN_FLAGS | TRAP_FLAG; "own trap flag")]
#[test_case(false, CLEAN_FLAGS; "no trap flag")]
#[test_case(true, CLEAN_FLAGS | TRAP_FLAG; "own trap flag after popf")]
#[test_case(true, CLEAN_FLAGS; "no trap flag after popf")]
fn stepped_rt_sigreturn_restores_the_frames_flags(popf: bool, flags: u64) {
    ret_without_perf!();

    let log = check_fn_with_config::<PreciseTimerTool, _>(
        move || {
            let [r11, pushed] = rt_sigreturn_after_loop(4, popf, flags, 2 * LESS_RCBS);
            assert_eq!(
                r11, R11_MARKER,
                "rt_sigreturn restored r11 as {r11:#x}, not the frame's"
            );
            assert_eq!(
                pushed & TRAP_FLAG,
                flags & TRAP_FLAG,
                "the frame held {flags:#x} but pushf stored {pushed:#x}"
            );
        },
        Schedule {
            rcbs: LESS_RCBS,
            instructions: None,
        },
        true,
    );

    assert_eq!(
        log.timer_events.load(Ordering::SeqCst),
        1,
        "the timer must fire in the loop after rt_sigreturn"
    );
}
