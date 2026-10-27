/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The preemption timer against real KVM and the real PMU: the kick lands,
//! every decision follows the clock, a late arrival is recorded (exactly at
//! the target too), the clock is the same with and without the timer, an
//! unarmed thread's entries issue no syscall for the timer, and a request
//! re-armed after the timer stopped counting is programmed afresh.
//!
//! Placement: tests whose names say pinned force pinned placement and run on
//! every host. Tests that need following placement print NOT EXECUTED and
//! return on a host whose default placement pins each entry. The rest use the
//! host's default placement and hold on either placement.

use std::os::fd::AsRawFd;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use super::*;
use crate::preemption_timer::KICK_SIGNAL;
use crate::preemption_timer::KICKS_TAKEN;
use crate::preemption_timer::MAX_PERIOD;
use crate::preemption_timer::TIMER_SYSCALLS;
use crate::preemption_timer::reserve_kick_signal;
// The witness count is process-global: every test that reads it, here and in
// the timer's own unit tests, takes this one turn.
use crate::preemption_timer::witness_turn;

const MEMORY: usize = 16 * 1024 * 1024;

/// Timer syscalls that open the timer event at an entry, once this process
/// has installed the kick handler: query and unblock the kick signal,
/// `perf_event_open`, three `fcntl`s, one `PERF_EVENT_IOC_PERIOD` and one
/// `PERF_EVENT_IOC_ENABLE`. A pinned entry also disables its CPU-bound event
/// when it ends, one more.
const OPEN_SYSCALLS: u64 = 8;

fn margin() -> u64 {
    reverie::pmu_config::try_get_pmu_config()
        .expect("the preemption timer requires this CPU's PMU profile")
        .skid_margin()
}

fn timer_syscalls() -> u64 {
    TIMER_SYSCALLS.with(|count| count.get())
}

fn kicks_taken() -> u64 {
    KICKS_TAKEN.with(|kicks| kicks.load(Ordering::Relaxed))
}

/// A CPL3 loop, forever: exactly `branches` conditional branches (`dec ecx;
/// jnz`), then a getpid transport exit. The k-th exit is at clock k *
/// `branches`, and no branch retires between a block's last branch and its
/// exit, so an overflow at a block's end is followed by an exit at the same
/// count whether the kick or the syscall comes first.
fn block_loop(branches: u32) -> Vec<u8> {
    assert!(branches > 0);
    let mut code = vec![0xb9]; // start: mov ecx, branches
    code.extend_from_slice(&branches.to_le_bytes());
    code.extend_from_slice(&[
        0xff, 0xc9, 0x75, 0xfc, // loop: dec ecx; jnz loop
        0xb8, 39, 0, 0, 0, // mov eax, SYS_getpid
        0x0f, 0x05, // syscall
        0xeb, 0xee, // jmp start
    ]);
    code
}

/// A CPL3 loop with no exit at all: one taken JZ per iteration, forever.
const SPINNER: [u8; 6] = [
    0x31, 0xc0, // xor eax, eax
    0x85, 0xc0, // loop: test eax, eax
    0x74, 0xfc, // jz loop
];

/// Pins this thread to the CPU it is on until dropped, so that pinned entries
/// keep one counter identity and never reopen their events.
struct PinThread(libc::cpu_set_t);

impl PinThread {
    fn here() -> Self {
        let saved = affinity().unwrap();
        let cpu = current_cpu().expect("sched_getcpu");
        // SAFETY: an all-zero cpu_set_t is the empty set; `cpu` is below
        // CPU_SETSIZE because the kernel reported it.
        let mut one: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        unsafe { libc::CPU_SET(cpu, &mut one) };
        set_affinity(&one).unwrap();
        Self(saved)
    }
}

impl Drop for PinThread {
    fn drop(&mut self) {
        let _ = set_affinity(&self.0);
    }
}

fn counted_backend(code: &[u8], placement: CounterPlacement) -> crate::KvmBackend {
    let mut backend =
        crate::KvmBackend::new(MEMORY).expect("the preemption timer controls require /dev/kvm");
    backend
        .install_static_elf(&crate::vm::minimal_test_elf(code), "/bin/preemption-timer")
        .unwrap();
    assert_eq!(backend.vcpu.get_sregs().unwrap().cs.dpl, 3);
    backend.vcpu.clock.placement = placement;
    backend.vcpu.track_clock().unwrap();
    backend
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Exit {
    Syscall,
    Kick,
}

fn next_exit(vcpu: &mut CountedVcpu) -> Exit {
    match vcpu.run() {
        Ok(Some(VcpuExit::Hypercall(exit))) => {
            assert_eq!(exit.nr, crate::vm::VMCALL_SYSCALL_TRANSPORT);
            Exit::Syscall
        }
        Ok(Some(exit)) => panic!("unexpected exit {exit:?}"),
        Ok(None) => panic!("entry admission was closed"),
        Err(Error::Kvm(error)) if error.errno() == libc::EINTR => Exit::Kick,
        Err(error) => panic!("entry failed: {error}"),
    }
}

fn timer_event(vcpu: &CountedVcpu) -> Option<&TimerEvent> {
    vcpu.clock.binding.as_ref().and_then(|b| b.timer.as_ref())
}

/// The timer event's own count and the clock's advance since the entry that
/// opened that event, read once at the end of a test. The two are equal when
/// the timer counted exactly the branches its clock counted. An event
/// reopened with its binding (another host thread or CPU) counts from zero.
fn timer_and_clock_advance(vcpu: &CountedVcpu) -> (u64, u64) {
    let timer = timer_event(vcpu).expect("an armed thread has a timer event");
    let advance = vcpu.read_clock().unwrap() - timer.opened_clock;
    (timer_count(vcpu), advance)
}

/// The timer event's own count since it was opened.
fn timer_count(vcpu: &CountedVcpu) -> u64 {
    let timer = timer_event(vcpu).expect("an armed thread has a timer event");
    let mut value = 0_u64;
    // SAFETY: a sampling event with no read_format reads one native u64.
    let read = unsafe {
        libc::read(
            timer.fd().as_raw_fd(),
            std::ptr::from_mut(&mut value).cast(),
            std::mem::size_of::<u64>(),
        )
    };
    assert_eq!(read, std::mem::size_of::<u64>() as isize);
    value
}

fn due(target: u64, clock: u64, late: bool) -> Option<TimerDue> {
    Some(TimerDue {
        target,
        clock,
        late,
    })
}

/// A request's phases at exits that land on exact counts, under `placement`:
/// the first programming one skid margin short of the target, the second at
/// the target, and the exit at exactly the target recorded as late.
fn exits_follow_the_clock(placement: CounterPlacement) {
    let _turn = witness_turn();
    let margin = margin();
    if margin < 2 || margin > u64::from(u32::MAX) / 8 {
        println!("timer phases: NOT EXECUTED; skid margin {margin} is overridden out of range");
        return;
    }
    // One block per skid margin puts every programmed overflow at a block's
    // end, so each exit's clock is exact however late its kick is.
    let block = margin;
    let mut backend = counted_backend(&block_loop(block as u32), placement);
    backend
        .vcpu
        .request_timer(3 * block, TimerKind::Precise)
        .unwrap();
    assert_eq!(backend.vcpu.timer_target(), Some(3 * block));
    let mut exits = Vec::new();
    while backend.vcpu.read_clock().unwrap() < 3 * block {
        let exit = next_exit(&mut backend.vcpu);
        let clock = backend.vcpu.read_clock().unwrap();
        let overflow = timer_event(&backend.vcpu).unwrap().overflow_at();
        exits.push((exit, clock, overflow));
        if clock < 3 * block {
            assert_eq!(backend.vcpu.timer_due(), None, "{exits:?}");
        }
        // A kick whose interrupt came very late can land anywhere; a
        // syscall exit is always at a block's end.
        if exit == Exit::Syscall {
            assert_eq!(clock % block, 0, "{exits:?}");
            match clock / block {
                // The first programming overflows one margin short of the
                // target.
                1 => assert_eq!(overflow, Some(2 * block), "{exits:?}"),
                // Its overflow is reached and forgotten; an entry after a
                // kick at the block's end programs the target before the
                // block's exit.
                2 => assert!(overflow.is_none_or(|at| at == 3 * block), "{exits:?}"),
                _ => {}
            }
        }
    }
    // The first exit at or past the target is at exactly the target: no
    // branch retires between the block's last branch and its syscall exit.
    assert_eq!(backend.vcpu.read_clock().unwrap(), 3 * block, "{exits:?}");
    // Exactly at the target, and still late: the exit is asynchronous.
    assert_eq!(
        backend.vcpu.timer_due(),
        due(3 * block, 3 * block, true),
        "{exits:?}"
    );
    assert_eq!(reverie::take_skid_overshoot_count(), 1);
    // Later exits find it already due and record nothing more.
    while backend.vcpu.read_clock().unwrap() < 4 * block {
        next_exit(&mut backend.vcpu);
        assert_eq!(backend.vcpu.timer_due(), due(3 * block, 3 * block, true));
    }
    assert_eq!(reverie::take_skid_overshoot_count(), 0);

    // Past the target: the kick or the block's exit, whichever comes first,
    // finds the clock at or past it, but never past the block's end.
    backend
        .vcpu
        .request_timer(block / 2, TimerKind::Precise)
        .unwrap();
    let target = 4 * block + block / 2;
    while backend.vcpu.timer_due().is_none() {
        next_exit(&mut backend.vcpu);
    }
    let late = backend.vcpu.timer_due().unwrap();
    assert_eq!(late.target, target);
    assert!(late.late);
    assert!((target..=5 * block).contains(&late.clock), "{late:?}");
    assert_eq!(reverie::take_skid_overshoot_count(), 1);
    while backend.vcpu.read_clock().unwrap() < 5 * block {
        next_exit(&mut backend.vcpu);
    }

    // An imprecise request reached at its target is on time: nothing recorded.
    backend
        .vcpu
        .request_timer(block, TimerKind::Imprecise)
        .unwrap();
    while backend.vcpu.timer_due().is_none() {
        next_exit(&mut backend.vcpu);
    }
    assert_eq!(backend.vcpu.timer_due(), due(6 * block, 6 * block, false));
    assert_eq!(reverie::take_skid_overshoot_count(), 0);

    // A replaced request is forgotten; a cancelled one never comes due.
    backend
        .vcpu
        .request_timer(9 * block, TimerKind::Precise)
        .unwrap();
    backend
        .vcpu
        .request_timer(2 * block, TimerKind::Precise)
        .unwrap();
    assert_eq!(backend.vcpu.timer_target(), Some(8 * block));
    while backend.vcpu.timer_due().is_none() {
        next_exit(&mut backend.vcpu);
    }
    assert_eq!(backend.vcpu.timer_due(), due(8 * block, 8 * block, true));
    assert_eq!(reverie::take_skid_overshoot_count(), 1);
    backend
        .vcpu
        .request_timer(2 * block, TimerKind::Precise)
        .unwrap();
    backend.vcpu.cancel_timer();
    assert_eq!(backend.vcpu.timer_target(), None);
    while backend.vcpu.read_clock().unwrap() < 12 * block {
        next_exit(&mut backend.vcpu);
        assert_eq!(backend.vcpu.timer_due(), None);
    }
    assert_eq!(reverie::take_skid_overshoot_count(), 0);
    println!(
        "timer phases ({placement:?}, margin {margin}): {} exits to the first target: {exits:?}",
        exits.len()
    );
}

#[test]
fn exits_follow_the_clock_with_following_events() {
    if CounterPlacement::default() != CounterPlacement::FollowTask {
        println!("timer phases: FollowTask NOT EXECUTED; this host pins each entry");
        return;
    }
    exits_follow_the_clock(CounterPlacement::FollowTask);
}

#[test]
fn exits_follow_the_clock_with_pinned_entries() {
    let _pin = PinThread::here();
    exits_follow_the_clock(CounterPlacement::PinEachEntry);
}

/// Runs `rounds` requests on a guest that never exits by itself, so every
/// exit is a kick: a lost kick would leave `KVM_RUN` running until the
/// watchdog sends one, which fails the test.
fn every_kick_lands(placement: CounterPlacement, rounds: u64) {
    let _turn = witness_turn();
    let margin = margin();
    let mut backend = counted_backend(&SPINNER, placement);
    let steps = [
        1,
        2,
        3,
        5,
        6,
        margin.saturating_sub(1).max(1),
        margin.max(1),
        margin + 5,
        margin + 6,
        2 * margin + 1,
        10_005,
        10_006,
        100_000,
        1_000_000,
    ];
    let progress = AtomicU64::new(0);
    let done = AtomicBool::new(false);
    let rescued = AtomicU64::new(0);
    // SAFETY: the identity of this thread, which outlives the watchdog.
    let vcpu_thread = unsafe { libc::pthread_self() };
    let kicks_before = kicks_taken();
    let mut exits = 0_u64;
    let mut precise = 0_u64;
    let mut most_late = 0_u64;
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut last = 0;
            let mut since = Instant::now();
            while !done.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
                let now = progress.load(Ordering::SeqCst);
                if now != last {
                    last = now;
                    since = Instant::now();
                } else if since.elapsed() > Duration::from_secs(10) {
                    rescued.fetch_add(1, Ordering::SeqCst);
                    // SAFETY: the vCPU thread is alive until `done` is set.
                    unsafe { libc::pthread_kill(vcpu_thread, KICK_SIGNAL) };
                    since = Instant::now();
                }
            }
        });
        struct Done<'a>(&'a AtomicBool);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _done = Done(&done);
        for round in 0..rounds {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let ticks = if round % 2 == 0 {
                steps[(round / 2) as usize % steps.len()]
            } else {
                1 + (seed >> 33) % 200_000
            };
            let kind = if round % 3 == 0 {
                TimerKind::Imprecise
            } else {
                TimerKind::Precise
            };
            let start = backend.vcpu.read_clock().unwrap();
            backend.vcpu.request_timer(ticks, kind).unwrap();
            while backend.vcpu.timer_due().is_none() {
                assert_eq!(next_exit(&mut backend.vcpu), Exit::Kick);
                exits += 1;
            }
            let reached = backend.vcpu.timer_due().unwrap();
            assert_eq!(reached.target, start + ticks, "round {round}");
            assert!(
                reached.clock >= reached.target,
                "round {round}: {reached:?}"
            );
            assert_eq!(reached.late, kind == TimerKind::Precise, "round {round}");
            precise += u64::from(reached.late);
            most_late = most_late.max(reached.clock - reached.target);
            progress.fetch_add(1, Ordering::SeqCst);
        }
    });
    assert_eq!(rescued.load(Ordering::SeqCst), 0, "a kick was lost");
    assert_eq!(reverie::take_skid_overshoot_count(), precise);
    let kicks = kicks_taken() - kicks_before;
    assert!(exits >= rounds, "every round needs a kick");
    // The timer counted exactly what the clock counted from the entry that
    // opened it: neither lost a branch to the other's kicks.
    let clock = backend.vcpu.read_clock().unwrap();
    let (timer, advance) = timer_and_clock_advance(&backend.vcpu);
    assert_eq!(timer, advance);
    println!(
        "timer kicks ({placement:?}, margin {margin}): {rounds} requests, {exits} EINTR exits, \
         {kicks} kicks taken, 0 lost; clock {clock}; timer count {timer} = clock advance since it \
         opened; largest overshoot {most_late} RCBs"
    );
}

#[test]
fn every_kick_lands_with_following_events() {
    if CounterPlacement::default() != CounterPlacement::FollowTask {
        println!("timer kicks: FollowTask NOT EXECUTED; this host pins each entry");
        return;
    }
    every_kick_lands(CounterPlacement::FollowTask, 2_000);
}

#[test]
fn every_kick_lands_with_pinned_entries() {
    // One CPU for the whole test: a pinned event opened on another CPU would
    // count only from its own opening.
    let _pin = PinThread::here();
    every_kick_lands(CounterPlacement::PinEachEntry, 500);
}

/// The clocks at a deterministic guest's syscall exits, with no timer or
/// with a precise request renewed at every exit `ticks` RCBs ahead, and how
/// many kicks the timer caused. After `moved_at` syscall exits the guest
/// moves to another host thread, which reopens its binding: its clock and,
/// when armed, its timer. With a timer, also returns the clock value at the
/// entry that opened the current timer event.
fn clocks_at_syscalls(
    blocks: usize,
    branches: u32,
    ticks: Option<u64>,
    moved_at: usize,
) -> (Vec<u64>, u64, Option<u64>) {
    let mut backend = counted_backend(&block_loop(branches), CounterPlacement::default());
    let mut clocks = Vec::new();
    let mut kicks = 0;
    let mut run = |backend: &mut crate::KvmBackend, until: usize| {
        while clocks.len() < until {
            if let Some(ticks) = ticks {
                backend
                    .vcpu
                    .request_timer(ticks, TimerKind::Precise)
                    .unwrap();
            }
            match next_exit(&mut backend.vcpu) {
                Exit::Syscall => clocks.push(backend.vcpu.read_clock().unwrap()),
                Exit::Kick => kicks += 1,
            }
        }
    };
    run(&mut backend, moved_at);
    let first_tid = backend.vcpu.clock.binding.as_ref().unwrap().identity.tid;
    std::thread::scope(|scope| scope.spawn(|| run(&mut backend, blocks)).join().unwrap());
    assert_ne!(
        backend.vcpu.clock.binding.as_ref().unwrap().identity.tid,
        first_tid,
        "the guest must really move to another host task"
    );
    let opened = ticks.map(|_| {
        let (timer, advance) = timer_and_clock_advance(&backend.vcpu);
        assert_eq!(timer, advance, "the timer and its clock disagree");
        timer_event(&backend.vcpu).unwrap().opened_clock
    });
    if ticks.is_none() {
        assert!(timer_event(&backend.vcpu).is_none());
    }
    (clocks, kicks, opened)
}

#[test]
fn the_clock_is_bit_exact_with_and_without_the_timer() {
    let _turn = witness_turn();
    const BLOCKS: usize = 30;
    const MOVED_AT: usize = 10;
    const BRANCHES: u32 = 100_003;
    let (without, no_kicks, _) = clocks_at_syscalls(BLOCKS, BRANCHES, None, MOVED_AT);
    let (with, kicks, opened) = clocks_at_syscalls(BLOCKS, BRANCHES, Some(997), MOVED_AT);
    let expected: Vec<u64> = (1..=BLOCKS as u64)
        .map(|block| block * u64::from(BRANCHES))
        .collect();
    assert_eq!(without, expected);
    assert_eq!(with, without, "the timer changed the clock");
    assert_eq!(no_kicks, 0);
    assert!(
        kicks > BLOCKS as u64,
        "the timer must kick within blocks: {kicks}"
    );
    // The move reopened the timer partway, so its own count restarted there:
    // compared with the whole clock, it would fall short by `opened`.
    let opened = opened.unwrap();
    assert!(opened >= MOVED_AT as u64 * u64::from(BRANCHES), "{opened}");
    let _ = reverie::take_skid_overshoot_count();
    println!(
        "timer clock parity: {BLOCKS} syscall clocks equal with and without the timer; \
         {kicks} kicks with a request renewed 997 RCBs ahead; the guest moved host threads \
         after {MOVED_AT} exits and its timer reopened at clock {opened}"
    );
}

#[test]
fn unarmed_entries_issue_no_timer_syscalls_and_armed_ones_only_on_change() {
    let _turn = witness_turn();
    if CounterPlacement::default() != CounterPlacement::FollowTask {
        println!("timer syscalls: FollowTask NOT EXECUTED; this host pins each entry");
        return;
    }
    const BRANCHES: u32 = 10;
    const EXITS: u64 = 1_000;
    let mut backend = counted_backend(&block_loop(BRANCHES), CounterPlacement::FollowTask);
    let run = |backend: &mut crate::KvmBackend, exits: u64| -> u64 {
        let before = timer_syscalls();
        for _ in 0..exits {
            assert_eq!(next_exit(&mut backend.vcpu), Exit::Syscall);
        }
        timer_syscalls() - before
    };
    // Never armed: no event, no syscall.
    assert_eq!(run(&mut backend, EXITS), 0);
    assert!(timer_event(&backend.vcpu).is_none());
    // Armed far ahead: the first entry opens and programs the event, and no
    // later entry touches it while the target is unchanged. The kick handler
    // is installed first, so that the count does not depend on whether
    // another test in this process installed it already.
    reserve_kick_signal().unwrap();
    backend
        .vcpu
        .request_timer(1 << 40, TimerKind::Precise)
        .unwrap();
    let target = backend.vcpu.timer_target().unwrap();
    let opened = run(&mut backend, 1);
    assert_eq!(opened, OPEN_SYSCALLS);
    assert_eq!(run(&mut backend, EXITS), 0);
    // The same target requested again: nothing to change.
    let clock = backend.vcpu.read_clock().unwrap();
    backend
        .vcpu
        .request_timer(target - clock, TimerKind::Precise)
        .unwrap();
    assert_eq!(run(&mut backend, EXITS), 0);
    // An earlier target: one reprogramming.
    backend
        .vcpu
        .request_timer(1 << 39, TimerKind::Precise)
        .unwrap();
    assert_eq!(run(&mut backend, EXITS), 1);
    // A later target keeps the earlier overflow, which only kicks sooner.
    backend
        .vcpu
        .request_timer(1 << 41, TimerKind::Precise)
        .unwrap();
    assert_eq!(run(&mut backend, EXITS), 0);
    // Cancelled: one quieting programming, then nothing.
    backend.vcpu.cancel_timer();
    assert_eq!(run(&mut backend, 1), 1);
    assert_eq!(run(&mut backend, EXITS), 0);
    assert_eq!(reverie::take_skid_overshoot_count(), 0);
    println!(
        "timer syscalls: 0 per entry unarmed and while a target is unchanged ({EXITS} entries each); \
         {opened} to open, 1 per earlier target, 1 to quiet"
    );
}

#[test]
fn pinned_entries_enable_the_timer_only_while_armed() {
    let _turn = witness_turn();
    // One CPU, so that every entry keeps the same counter identity.
    let _pin = PinThread::here();
    const EXITS: u64 = 200;
    let mut backend = counted_backend(&block_loop(10), CounterPlacement::PinEachEntry);
    let run = |backend: &mut crate::KvmBackend, exits: u64| -> u64 {
        let before = timer_syscalls();
        for _ in 0..exits {
            assert_eq!(next_exit(&mut backend.vcpu), Exit::Syscall);
        }
        timer_syscalls() - before
    };
    assert_eq!(run(&mut backend, EXITS), 0);
    assert!(timer_event(&backend.vcpu).is_none());
    backend
        .vcpu
        .request_timer(1 << 40, TimerKind::Precise)
        .unwrap();
    run(&mut backend, 1);
    // A CPU-bound event is enabled for each armed entry and disabled after.
    assert_eq!(run(&mut backend, EXITS), 2 * EXITS);
    assert!(!timer_event(&backend.vcpu).unwrap().enabled());
    // Cancelled: a disabled event cannot overflow, so it is left alone.
    backend.vcpu.cancel_timer();
    assert_eq!(run(&mut backend, EXITS), 0);
    assert_eq!(reverie::take_skid_overshoot_count(), 0);
}

#[test]
fn another_guest_on_the_host_task_does_not_advance_the_timer() {
    let _turn = witness_turn();
    if CounterPlacement::default() != CounterPlacement::FollowTask {
        println!("timer displacement: NOT EXECUTED; this host pins each entry");
        return;
    }
    const BRANCHES: u32 = 1_000;
    let mut armed = counted_backend(&block_loop(BRANCHES), CounterPlacement::FollowTask);
    armed
        .vcpu
        .request_timer(1 << 40, TimerKind::Precise)
        .unwrap();
    for _ in 0..3 {
        assert_eq!(next_exit(&mut armed.vcpu), Exit::Syscall);
    }
    assert!(timer_event(&armed.vcpu).unwrap().enabled());
    // Another counted guest on this host task enables its own clock, which
    // disables the armed thread's clock and timer while it runs.
    let mut other = counted_backend(&block_loop(100_000), CounterPlacement::FollowTask);
    for _ in 0..5 {
        assert_eq!(next_exit(&mut other.vcpu), Exit::Syscall);
    }
    assert_eq!(other.vcpu.read_clock().unwrap(), 500_000);
    assert!(
        armed
            .vcpu
            .clock
            .binding
            .as_ref()
            .unwrap()
            .displaced
            .load(Ordering::Relaxed)
    );
    for _ in 0..3 {
        assert_eq!(next_exit(&mut armed.vcpu), Exit::Syscall);
    }
    assert!(timer_event(&armed.vcpu).unwrap().enabled());
    assert_eq!(armed.vcpu.read_clock().unwrap(), 6 * u64::from(BRANCHES));
    let (timer, advance) = timer_and_clock_advance(&armed.vcpu);
    assert_eq!(
        (timer, advance),
        (6 * u64::from(BRANCHES), 6 * u64::from(BRANCHES)),
        "the armed timer counted the other guest's branches"
    );
    assert_eq!(reverie::take_skid_overshoot_count(), 0);
}

#[test]
fn the_timer_follows_a_guest_to_another_host_thread() {
    let _turn = witness_turn();
    let margin = margin();
    let mut backend = counted_backend(&SPINNER, CounterPlacement::default());
    let placement = backend.vcpu.clock.placement;
    let ticks = 100 * margin.max(1_000);
    backend
        .vcpu
        .request_timer(ticks, TimerKind::Precise)
        .unwrap();
    // The first kick, normally one margin short of the target, comes on this
    // thread and leaves a timer event bound to it.
    assert_eq!(next_exit(&mut backend.vcpu), Exit::Kick);
    let mut late = u64::from(backend.vcpu.timer_due().is_some());
    let first_tid = backend.vcpu.clock.binding.as_ref().unwrap().identity.tid;
    let (opened, tid, start) = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let start = backend.vcpu.read_clock().unwrap();
                backend
                    .vcpu
                    .request_timer(ticks, TimerKind::Precise)
                    .unwrap();
                // The first entry on this thread opens its own timer event.
                let before = timer_syscalls();
                assert_eq!(next_exit(&mut backend.vcpu), Exit::Kick);
                let opened = timer_syscalls() - before;
                while backend.vcpu.timer_due().is_none() {
                    assert_eq!(next_exit(&mut backend.vcpu), Exit::Kick);
                }
                (opened, host_tid(), start)
            })
            .join()
            .unwrap()
    });
    late += 1;
    assert_ne!(tid, first_tid, "the test must really change host tasks");
    assert_eq!(
        backend.vcpu.clock.binding.as_ref().unwrap().identity.tid,
        tid
    );
    // The first thread's timer installed the kick handler, so the new
    // thread pays only the query and the unblock. On pinned placement the
    // measured entry also ends with the disable of its CPU-bound timer.
    let expected = OPEN_SYSCALLS + u64::from(placement == CounterPlacement::PinEachEntry);
    assert_eq!(
        opened, expected,
        "the new host task opens its own timer ({placement:?})"
    );
    let reached = backend.vcpu.timer_due().unwrap();
    assert_eq!(reached.target, start + ticks);
    assert!(
        reached.late && reached.clock >= reached.target,
        "{reached:?}"
    );
    assert_eq!(reverie::take_skid_overshoot_count(), late);
}

/// The re-arm tests' guest: blocks of `RE_ARM_BLOCK` branches, each ended by a
/// syscall exit, and a target in the middle of the third block. A request
/// programmed correctly comes due through a kick inside the third block; one
/// whose programming went stale is caught only by that block's syscall exit,
/// 2,000,000 RCBs late. That slack is about 0.7 ms of guest time, far above
/// the longest interrupt latency measured for these kicks.
const RE_ARM_BLOCK: u32 = 4_000_000;
const RE_ARM_TARGET: u64 = 10_000_000;

/// A precise request armed at clock 0 for [`RE_ARM_TARGET`], one block, then
/// `interlude` (which may cancel the request or run another guest on this
/// host task), then one block with no request, then the same absolute target
/// requested again at clock 8,000,000. Returns the timer event's pending
/// overflow just before that request, and the exits until it came due.
fn re_arm_after(
    placement: CounterPlacement,
    interlude: impl FnOnce(&mut crate::KvmBackend),
) -> (Option<u64>, Vec<(Exit, u64)>, TimerDue) {
    let block = u64::from(RE_ARM_BLOCK);
    let margin = margin();
    let mut backend = counted_backend(&block_loop(RE_ARM_BLOCK), placement);
    backend
        .vcpu
        .request_timer(RE_ARM_TARGET, TimerKind::Precise)
        .unwrap();
    assert_eq!(next_exit(&mut backend.vcpu), Exit::Syscall);
    assert_eq!(backend.vcpu.read_clock().unwrap(), block);
    assert_eq!(
        timer_event(&backend.vcpu).unwrap().overflow_at(),
        Some(RE_ARM_TARGET - margin)
    );
    interlude(&mut backend);
    backend.vcpu.cancel_timer();
    while backend.vcpu.read_clock().unwrap() < 2 * block {
        next_exit(&mut backend.vcpu);
    }
    assert_eq!(backend.vcpu.read_clock().unwrap(), 2 * block);
    let before = timer_event(&backend.vcpu).unwrap().overflow_at();
    backend
        .vcpu
        .request_timer(RE_ARM_TARGET - 2 * block, TimerKind::Precise)
        .unwrap();
    let mut exits = Vec::new();
    while backend.vcpu.timer_due().is_none() {
        let exit = next_exit(&mut backend.vcpu);
        exits.push((exit, backend.vcpu.read_clock().unwrap()));
    }
    (before, exits, backend.vcpu.timer_due().unwrap())
}

/// The request came due through a kick inside the third block, at or past
/// its target, rather than at that block's syscall exit.
fn assert_due_by_kick(due: TimerDue, exits: &[(Exit, u64)]) {
    let third_exit = 3 * u64::from(RE_ARM_BLOCK);
    assert_eq!(due.target, RE_ARM_TARGET, "{exits:?}");
    assert!(due.late, "{exits:?}");
    assert!(
        (RE_ARM_TARGET..third_exit).contains(&due.clock),
        "the re-armed request came due at {}, not through a kick before the \
         block's syscall exit at {third_exit}: {exits:?}",
        due.clock
    );
    assert_eq!(
        exits.last().map(|exit| exit.0),
        Some(Exit::Kick),
        "{exits:?}"
    );
}

#[test]
fn a_request_re_armed_after_a_cancel_is_programmed_afresh_with_pinned_entries() {
    let _turn = witness_turn();
    // One CPU, so that every entry keeps the same counter identity: the event
    // that missed the unarmed block is the one re-armed.
    let _pin = PinThread::here();
    let (before, exits, due) = re_arm_after(CounterPlacement::PinEachEntry, |_| {});
    // The unarmed block ran the clock with the event disabled, so its old
    // overflow named no clock value any more and was forgotten.
    assert_eq!(before, None, "{exits:?}");
    assert_due_by_kick(due, &exits);
    assert_eq!(reverie::take_skid_overshoot_count(), 1);
    println!("timer re-arm after cancel (PinEachEntry): exits {exits:?}, due {due:?}");
}

#[test]
fn a_request_re_armed_after_a_cancel_is_programmed_afresh_with_following_events() {
    let _turn = witness_turn();
    if CounterPlacement::default() != CounterPlacement::FollowTask {
        println!("timer re-arm after cancel: FollowTask NOT EXECUTED; this host pins each entry");
        return;
    }
    let (before, exits, due) = re_arm_after(CounterPlacement::FollowTask, |_| {});
    // A following event stays enabled: the cancel quieted it.
    assert_eq!(before, None, "{exits:?}");
    assert_due_by_kick(due, &exits);
    assert_eq!(reverie::take_skid_overshoot_count(), 1);
    println!("timer re-arm after cancel (FollowTask): exits {exits:?}, due {due:?}");
}

#[test]
fn a_request_re_armed_after_a_displacement_is_programmed_afresh() {
    let _turn = witness_turn();
    if CounterPlacement::default() != CounterPlacement::FollowTask {
        println!("timer re-arm after displacement: NOT EXECUTED; this host pins each entry");
        return;
    }
    let mut other = None;
    let (before, exits, due) = re_arm_after(CounterPlacement::FollowTask, |armed| {
        // Another counted guest on this host task disables the armed
        // thread's clock and timer while it runs.
        let mut guest = counted_backend(&block_loop(1_000), CounterPlacement::FollowTask);
        assert_eq!(next_exit(&mut guest.vcpu), Exit::Syscall);
        let binding = armed.vcpu.clock.binding.as_ref().unwrap();
        assert!(binding.displaced.load(Ordering::Relaxed));
        other = Some(guest);
    });
    assert_eq!(before, None, "{exits:?}");
    assert_due_by_kick(due, &exits);
    assert_eq!(reverie::take_skid_overshoot_count(), 1);
    drop(other);
    println!("timer re-arm after displacement: exits {exits:?}, due {due:?}");
}

#[test]
fn a_timer_that_cannot_be_displaced_refuses_the_displacing_entry() {
    let _turn = witness_turn();
    if CounterPlacement::default() != CounterPlacement::FollowTask {
        println!("timer displacement refusal: NOT EXECUTED; this host pins each entry");
        return;
    }
    let mut backend = counted_backend(&block_loop(1_000), CounterPlacement::FollowTask);
    // Another binding's timer that cannot be disabled: a pipe, on which the
    // perf disable ioctl fails with ENOTTY, as a denied ioctl would.
    let mut fds = [-1; 2];
    // SAFETY: pipe2 writes two new descriptors into the array.
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    // SAFETY: both descriptors are new and owned only here.
    let (timer, _writer) = unsafe {
        (
            std::sync::Arc::new(OwnedFd::from_raw_fd(fds[0])),
            OwnedFd::from_raw_fd(fds[1]),
        )
    };
    let other_displaced = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    ENABLED_FOLLOWING_EVENT.with(|slot| {
        *slot.borrow_mut() = Some(FollowingEvents {
            clock: std::sync::Weak::new(),
            timer: Some(std::sync::Arc::downgrade(&timer)),
            displaced: other_displaced.clone(),
        });
    });
    // That binding displaced this one, which must displace it back to run.
    let binding = backend.vcpu.clock.binding.as_ref().unwrap();
    binding.displaced.store(true, Ordering::Relaxed);
    let rip = backend.vcpu.get_regs().unwrap().rip;
    let error = match backend.vcpu.run() {
        Err(error) => error,
        Ok(exit) => panic!(
            "the entry ran although another guest's timer stayed enabled: {:?}",
            exit.map(|exit| format!("{exit:?}"))
        ),
    };
    assert!(
        matches!(error.primary(), Error::GuestClock(message)
            if message.contains("disable another guest's preemption timer")),
        "{error}"
    );
    // No guest instruction ran, and the refusal is final for this clock.
    assert_eq!(backend.vcpu.get_regs().unwrap().rip, rip);
    assert!(backend.vcpu.read_clock().is_err());
    // The other binding keeps both of its events, still recorded as enabled.
    assert!(!other_displaced.load(Ordering::Relaxed));
    let recorded = ENABLED_FOLLOWING_EVENT.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|events| std::sync::Arc::ptr_eq(&events.displaced, &other_displaced))
    });
    assert!(recorded);
    ENABLED_FOLLOWING_EVENT.with(|slot| slot.borrow_mut().take());
    assert_eq!(reverie::take_skid_overshoot_count(), 0);
    println!("timer displacement refusal: {error}");
}

/// A named limit, not a guarantee: only counted entries displace another
/// binding's events. An uncounted vCPU on the armed thread's host task
/// leaves its timer enabled, the timer counts that vCPU's branches,
/// overflows inside its run, reloads a whole period, and kicks the armed
/// thread late. This pins down that consequence: the request comes due at
/// the block's syscall exit, recorded late.
#[test]
fn an_uncounted_vcpu_on_the_host_task_makes_the_armed_kick_late() {
    let _turn = witness_turn();
    if CounterPlacement::default() != CounterPlacement::FollowTask {
        println!("timer uncounted vCPU: NOT EXECUTED; this host pins each entry");
        return;
    }
    let block = u64::from(RE_ARM_BLOCK);
    let mut armed = counted_backend(&block_loop(RE_ARM_BLOCK), CounterPlacement::FollowTask);
    armed
        .vcpu
        .request_timer(RE_ARM_TARGET, TimerKind::Precise)
        .unwrap();
    assert_eq!(next_exit(&mut armed.vcpu), Exit::Syscall);
    assert_eq!(armed.vcpu.read_clock().unwrap(), block);
    // An uncounted guest (its clock never tracked) runs 7,000,000 branches
    // on this host task: past the armed timer's remaining 5,998,000.
    let mut uncounted =
        crate::KvmBackend::new(MEMORY).expect("the preemption timer controls require /dev/kvm");
    uncounted
        .install_static_elf(
            &crate::vm::minimal_test_elf(&block_loop(7_000_000)),
            "/bin/uncounted",
        )
        .unwrap();
    let mut uncounted_exits = Vec::new();
    while uncounted_exits.last() != Some(&Exit::Syscall) {
        uncounted_exits.push(next_exit(&mut uncounted.vcpu));
    }
    assert!(
        uncounted_exits.contains(&Exit::Kick),
        "the armed timer did not count the uncounted guest: {uncounted_exits:?}"
    );
    let mut exits = Vec::new();
    while armed.vcpu.timer_due().is_none() {
        let exit = next_exit(&mut armed.vcpu);
        exits.push((exit, armed.vcpu.read_clock().unwrap()));
    }
    assert_eq!(
        armed.vcpu.timer_due(),
        due(RE_ARM_TARGET, 3 * block, true),
        "{exits:?}"
    );
    assert_eq!(reverie::take_skid_overshoot_count(), 1);
    println!(
        "timer uncounted vCPU (named limit): uncounted exits {uncounted_exits:?}; armed exits \
         {exits:?}; due {:?}",
        armed.vcpu.timer_due()
    );
}

#[test]
fn requests_beyond_the_longest_period_are_programmed_in_pieces() {
    let _turn = witness_turn();
    // One CPU, so that a pinned binding never reopens its timer at a later
    // clock and every overflow point below is counted from clock 0.
    let _pin = PinThread::here();
    let mut backend = counted_backend(&block_loop(1_000), CounterPlacement::default());
    // Linux refuses a period with bit 63 set. The request is accepted, and
    // the event is programmed with the longest period instead.
    backend
        .vcpu
        .request_timer(1 << 63, TimerKind::Imprecise)
        .unwrap();
    for _ in 0..3 {
        assert_eq!(next_exit(&mut backend.vcpu), Exit::Syscall);
    }
    assert_eq!(backend.vcpu.read_clock().unwrap(), 3_000);
    assert_eq!(
        timer_event(&backend.vcpu).unwrap().overflow_at(),
        Some(MAX_PERIOD)
    );
    // The farthest target the clock can name is accepted; one more RCB, or
    // none at all, is refused and leaves the request in place.
    let clock = backend.vcpu.read_clock().unwrap();
    backend
        .vcpu
        .request_timer(u64::MAX - clock, TimerKind::Precise)
        .unwrap();
    assert_eq!(backend.vcpu.timer_target(), Some(u64::MAX));
    for refused in [u64::MAX - clock + 1, 0] {
        let error = backend
            .vcpu
            .request_timer(refused, TimerKind::Precise)
            .unwrap_err();
        assert!(matches!(error, Error::GuestClock(_)), "{error}");
        assert_eq!(backend.vcpu.timer_target(), Some(u64::MAX));
    }
    assert_eq!(next_exit(&mut backend.vcpu), Exit::Syscall);
    assert_eq!(
        timer_event(&backend.vcpu).unwrap().overflow_at(),
        Some(MAX_PERIOD)
    );
    assert!(backend.vcpu.read_clock().is_ok());
    assert_eq!(reverie::take_skid_overshoot_count(), 0);
}
