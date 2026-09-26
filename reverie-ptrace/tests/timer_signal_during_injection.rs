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
//! `SIGSTKFLT` it receives and requires none. Three cases put the timer in
//! each state it can be in when that signal stops the injected syscall, and
//! each requires that Reverie took the signal in that state. A fourth case
//! makes the pending `SIGSTKFLT` one the timer did not send, which the guest
//! must receive.

#![cfg(target_arch = "x86_64")]

use std::sync::Mutex;
use std::sync::OnceLock;
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
use reverie_ptrace::testing::ConsumedTimerSignals;
use reverie_ptrace::testing::check_fn_with_config;
use reverie_ptrace::testing::take_consumed_timer_signals;
use serde::Deserialize;
use serde::Serialize;
use test_case::test_case;

/// Branches from a request to its PMU notification, far above the
/// single-step threshold so every request programs a real notification.
const NOTIFICATION_RCBS: u64 = 9_000;

/// How many loop lengths the rounds cycle through around the notification.
const OFFSETS: u64 = 16;

/// Rounds per run, each loop length used this many times over.
const ROUNDS: u64 = OFFSETS * 8;

/// The interval every request asks for. The skid margin differs between
/// processors, so the interval is set from it: the notification then comes
/// `NOTIFICATION_RCBS` branches after the request on every processor.
fn timer_rcbs() -> u64 {
    static RCBS: OnceLock<u64> = OnceLock::new();
    *RCBS.get_or_init(|| {
        PmuConfig::new()
            .skid_margin()
            .checked_add(NOTIFICATION_RCBS)
            .expect("skid margin")
    })
}

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
    /// The tool sends `SIGSTKFLT` itself before injecting, with no timer
    /// kick outstanding, so the pending signal is not the timer's.
    Foreign,
}

/// The counts of taken timer signals are process-wide, so the cases take
/// turns.
static SERIAL: Mutex<()> = Mutex::new(());

#[derive(Debug, Default)]
struct Log {
    timer_events: AtomicU64,
    /// Events that fired at a clock other than their target, unless Reverie
    /// recorded a skid overshoot for them, which reports the clock observed
    /// past the target.
    misplaced: Mutex<Vec<Fired>>,
}

/// A timer event, as the tool saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Fired {
    clock: u64,
    target: Option<u64>,
    overshoot: bool,
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Request = Fired;
    type Response = ();
    type Config = Case;

    async fn receive_rpc(&self, _from: Pid, fired: Fired) {
        self.timer_events.fetch_add(1, Ordering::SeqCst);
        let placed = match fired.target {
            Some(target) if fired.overshoot => fired.clock > target,
            Some(target) => fired.clock == target,
            None => false,
        };
        if !placed {
            self.misplaced.lock().unwrap().push(fired);
        }
    }
}

#[derive(Debug, Default, Clone)]
struct InjectingTool;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Rounds {
    /// The last round the tool sent a foreign `SIGSTKFLT` in.
    signalled: Option<u64>,
    /// The clock at which the latest request is due.
    target: Option<u64>,
}

/// Requests an event `timer_rcbs()` branches from now and notes its target.
fn request<T: Guest<InjectingTool>>(guest: &mut T) -> Result<(), Error> {
    let target = guest.read_clock()? + timer_rcbs();
    guest.set_timer_precise(TimerSchedule::Rcbs(timer_rcbs()))?;
    guest.thread_state_mut().target = Some(target);
    Ok(())
}

#[reverie::tool]
impl Tool for InjectingTool {
    type GlobalState = Log;
    type ThreadState = Rounds;

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
                request(guest)?;
                Ok(0)
            }
            Sysno::getppid => {
                match *guest.config() {
                    Case::Rerequested => request(guest)?,
                    Case::Foreign => {
                        // The guest passes its round number. Send once a
                        // round, even if the guest's syscall restarts.
                        let round = syscall.into_parts().1.arg0 as u64;
                        if guest.thread_state().signalled != Some(round) {
                            guest.thread_state_mut().signalled = Some(round);
                            let sent = unsafe {
                                libc::syscall(
                                    libc::SYS_tgkill,
                                    guest.pid().as_raw(),
                                    guest.tid().as_raw(),
                                    libc::SIGSTKFLT,
                                )
                            };
                            assert_eq!(sent, 0);
                        }
                    }
                    Case::Armed | Case::Cancelled => {}
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
        let fired = Fired {
            clock: guest.read_clock().unwrap(),
            target: guest.thread_state().target,
            // The cases take turns, so any overshoot recorded is this run's,
            // and each is taken as the event it belongs to fires.
            overshoot: reverie::take_skid_overshoot_count() > 0,
        };
        guest.send_rpc(fired).await;
    }
}

/// A syscall with no conditional branch between its caller and the kernel
/// entry, so the guest's branch count at the stop is exact.
#[inline(always)]
unsafe fn syscall_no_branches(no: Sysno, arg: u64) {
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") no as usize => _,
            in("rdi") arg,
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
#[test_case(Case::Foreign, 0; "foreign")]
fn timer_signal_pending_at_an_injected_syscall_is_not_delivered_to_the_guest(
    case: Case,
    expected_events: u64,
) {
    ret_without_perf!();
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    take_consumed_timer_signals();
    reverie::take_skid_overshoot_count();
    let skid = PmuConfig::new().skid_margin();
    let notification = NOTIFICATION_RCBS;
    // Past the target of a timer requested at getppid, so that event fires
    // before the next round's request replaces it.
    let after = match case {
        Case::Rerequested => 2 * timer_rcbs(),
        Case::Armed | Case::Cancelled | Case::Foreign => 2 * skid + OFFSETS,
    };
    // Every signal the tool sends, and none the timer sends.
    let expected_delivered = match case {
        Case::Foreign => ROUNDS,
        Case::Armed | Case::Rerequested | Case::Cancelled => 0,
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
                unsafe { syscall_no_branches(Sysno::clock_getres, 0) };
                if case == Case::Cancelled {
                    unsafe { syscall_no_branches(Sysno::getegid, 0) };
                }
                if case != Case::Foreign {
                    branches(before);
                }
                unsafe { syscall_no_branches(Sysno::getppid, round) };
                branches(after);
            }
            assert_eq!(
                TIMER_SIGNALS_DELIVERED.load(Ordering::SeqCst),
                expected_delivered,
                "SIGSTKFLTs the guest received"
            );
        },
        case,
        true,
    );

    // The guest's own getppid comes before the target of the event requested
    // at clock_getres, so Reverie cancels that event whether or not its signal
    // is sent again. Only an event requested at getppid fires, once a round.
    assert_eq!(log.timer_events.load(Ordering::SeqCst), expected_events);
    // Each fired at the clock it was requested for.
    assert_eq!(*log.misplaced.lock().unwrap(), []);

    // Whether the timer's signal is still pending at the injected syscall
    // depends on how late the processor raises it, so require that it was
    // for some rounds, and that Reverie took it in the state the case sets
    // up. The foreign signal must not be taken at all.
    let taken = take_consumed_timer_signals();
    eprintln!("{case:?}: timer signals taken {taken:?}");
    let only = |count: u64| match case {
        Case::Armed => ConsumedTimerSignals {
            armed: count,
            ..Default::default()
        },
        Case::Rerequested => ConsumedTimerSignals {
            scheduled: count,
            ..Default::default()
        },
        Case::Cancelled => ConsumedTimerSignals {
            cancelled: count,
            ..Default::default()
        },
        Case::Foreign => ConsumedTimerSignals::default(),
    };
    let count = taken.armed + taken.scheduled + taken.cancelled;
    assert_eq!(taken, only(count), "timer signals taken");
    if case != Case::Foreign {
        assert!(
            count > 0,
            "no round had the timer's signal pending: {taken:?}"
        );
    }
}
