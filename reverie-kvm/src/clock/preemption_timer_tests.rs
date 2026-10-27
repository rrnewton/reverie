/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The preemption timer against real KVM and the real PMU: the kick lands,
//! every decision follows the clock, a late arrival is recorded (exactly at
//! the target too), the clock is the same with and without the timer, and
//! an unarmed thread's entries issue no syscall for the timer.

use std::os::fd::AsRawFd;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use super::*;
use crate::preemption_timer::KICK_SIGNAL;
use crate::preemption_timer::KICKS_TAKEN;
use crate::preemption_timer::TIMER_SYSCALLS;

const MEMORY: usize = 16 * 1024 * 1024;

/// The witness count is process-global; tests that measure it take turns.
static WITNESS: Mutex<()> = Mutex::new(());

fn witness_turn() -> std::sync::MutexGuard<'static, ()> {
    let guard = WITNESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = reverie::take_skid_overshoot_count();
    guard
}

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

/// The timer event's own count, read once at the end of a test: it counts
/// exactly the branches its clock counts from the entry that opened it.
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
    // opened it, at clock 0: neither lost a branch to the other's kicks.
    let clock = backend.vcpu.read_clock().unwrap();
    assert_eq!(timer_count(&backend.vcpu), clock);
    println!(
        "timer kicks ({placement:?}, margin {margin}): {rounds} requests, {exits} EINTR exits, \
         {kicks} kicks taken, 0 lost; clock {clock} = timer count; largest overshoot {most_late} RCBs"
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
/// many kicks the timer caused.
fn clocks_at_syscalls(blocks: usize, branches: u32, ticks: Option<u64>) -> (Vec<u64>, u64) {
    let mut backend = counted_backend(&block_loop(branches), CounterPlacement::default());
    let mut clocks = Vec::new();
    let mut kicks = 0;
    while clocks.len() < blocks {
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
    if ticks.is_some() {
        assert_eq!(
            timer_count(&backend.vcpu),
            backend.vcpu.read_clock().unwrap()
        );
    } else {
        assert!(timer_event(&backend.vcpu).is_none());
    }
    (clocks, kicks)
}

#[test]
fn the_clock_is_bit_exact_with_and_without_the_timer() {
    let _turn = witness_turn();
    const BLOCKS: usize = 30;
    const BRANCHES: u32 = 100_003;
    let (without, no_kicks) = clocks_at_syscalls(BLOCKS, BRANCHES, None);
    let (with, kicks) = clocks_at_syscalls(BLOCKS, BRANCHES, Some(997));
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
    let _ = reverie::take_skid_overshoot_count();
    println!(
        "timer clock parity: {BLOCKS} syscall clocks equal with and without the timer; \
         {kicks} kicks with a request renewed 997 RCBs ahead"
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
    // later entry touches it while the target is unchanged.
    backend
        .vcpu
        .request_timer(1 << 40, TimerKind::Precise)
        .unwrap();
    let target = backend.vcpu.timer_target().unwrap();
    let opened = run(&mut backend, 1);
    assert!(opened >= 6, "open, owner, signal and programming: {opened}");
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
    assert_eq!(
        timer_count(&armed.vcpu),
        6 * u64::from(BRANCHES),
        "the armed timer counted the other guest's branches"
    );
    assert_eq!(reverie::take_skid_overshoot_count(), 0);
}

#[test]
fn the_timer_follows_a_guest_to_another_host_thread() {
    let _turn = witness_turn();
    let margin = margin();
    let mut backend = counted_backend(&SPINNER, CounterPlacement::default());
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
                let before = timer_syscalls();
                let start = backend.vcpu.read_clock().unwrap();
                backend
                    .vcpu
                    .request_timer(ticks, TimerKind::Precise)
                    .unwrap();
                while backend.vcpu.timer_due().is_none() {
                    assert_eq!(next_exit(&mut backend.vcpu), Exit::Kick);
                }
                (timer_syscalls() - before, host_tid(), start)
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
    assert!(
        opened >= 6,
        "the new host task opens its own timer: {opened}"
    );
    let reached = backend.vcpu.timer_due().unwrap();
    assert_eq!(reached.target, start + ticks);
    assert!(
        reached.late && reached.clock >= reached.target,
        "{reached:?}"
    );
    assert_eq!(reverie::take_skid_overshoot_count(), late);
}
