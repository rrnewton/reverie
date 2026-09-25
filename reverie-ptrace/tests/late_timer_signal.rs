/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A precise timer's overflow interrupt can arrive after the guest has already
//! reached its next syscall. That syscall cancels the timer, but the tracer's
//! own overflow signal is still in flight. It belongs to Reverie, not to the
//! guest: it must not interrupt a syscall the tool injects at that stop, and it
//! must never be delivered to the guest.
//!
//! Each iteration arms a precise timer whose target is beyond the syscall, so
//! no timer event is due, and places the counter overflow a few branches before
//! the syscall. Ordinary overflow-interrupt latency then lands the interrupt
//! after the syscall has been entered.
//!
//! A guest's own signal of the same number pending at such an injection is not
//! the timer's and must still reach the guest.

#![cfg(target_arch = "x86_64")]

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::ExitStatus;
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
use reverie_ptrace::testing::do_branches;
use reverie_ptrace::testing::print_tracee_output;
use reverie_ptrace::testing::test_fn_with_config;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
enum Report {
    TimerEvent,
    InjectedSyscall,
    InjectError(i32),
}

#[derive(Debug, Default)]
struct Log {
    timer_events: AtomicU64,
    injected_syscalls: AtomicU64,
    inject_errors: Mutex<Vec<i32>>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
struct Config {
    timeout_rcbs: u64,
    retry: bool,
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Request = Report;
    type Response = ();
    type Config = Config;

    async fn receive_rpc(&self, _from: Pid, report: Report) {
        match report {
            Report::TimerEvent => {
                self.timer_events.fetch_add(1, Ordering::SeqCst);
            }
            Report::InjectedSyscall => {
                self.injected_syscalls.fetch_add(1, Ordering::SeqCst);
            }
            Report::InjectError(errno) => self
                .inject_errors
                .lock()
                .expect("inject error log lock poisoned")
                .push(errno),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct LateTimerTool;

#[reverie::tool]
impl Tool for LateTimerTool {
    type GlobalState = Log;
    type ThreadState = ();

    fn subscriptions(_cfg: &Config) -> Subscription {
        Subscription::all()
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall.number() {
            Sysno::clock_getres => {
                let timeout = TimerSchedule::Rcbs(guest.config().timeout_rcbs);
                guest.set_timer_precise(timeout)?;
                Ok(0)
            }
            Sysno::getppid => {
                // The syscall event has cancelled the timer; an injected syscall
                // at this stop must complete without a Reverie-owned signal.
                let result = if guest.config().retry {
                    guest.inject_with_retry(Getpid::new()).await
                } else {
                    guest.inject(Getpid::new()).await
                };
                match result {
                    Ok(_) => guest.send_rpc(Report::InjectedSyscall).await,
                    Err(errno) => guest.send_rpc(Report::InjectError(errno.into_raw())).await,
                }
                Ok(0)
            }
            _ => guest.tail_inject(syscall).await,
        }
    }

    async fn handle_timer_event<T: Guest<Self>>(&self, guest: &mut T) {
        guest.send_rpc(Report::TimerEvent).await;
    }
}

const ITERS: u64 = 400;
/// Upper bound on the number of distances, in RCBs, by which the counter
/// overflow precedes the syscall.
const MAX_OVERFLOW_LEADS: u64 = 80;
/// RCBs from arming the timer to the counter overflow.
const ARM_RCBS: u64 = 10_000;

fn arm_precise_timer() {
    // SAFETY: the tool intercepts clock_getres and never runs it, so the
    // null result pointer is not dereferenced.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") libc::SYS_clock_getres => _,
            in("rdi") libc::CLOCK_MONOTONIC,
            in("rsi") 0usize,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
}

fn syscall_with_injection() {
    // SAFETY: getppid has no arguments and no memory effects.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") libc::SYS_getppid => _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
}

fn run(retry: bool) {
    // Precise timers need a PMU; GitHub-hosted runners have none. The
    // self-hosted hardware job runs this test.
    if !reverie_ptrace::is_perf_supported() {
        return;
    }
    let margin = reverie_ptrace::PmuConfig::new().skid_margin();
    // Keep the syscall well inside the margin, leaving room for the loop's own
    // branches, so the target is never reached. Supported margins range from
    // 100 to 10,000 RCBs.
    let overflow_leads = MAX_OVERFLOW_LEADS.min(margin / 4);
    assert!(overflow_leads > 0, "skid margin {margin} leaves no lead");
    // The counter overflows `margin` RCBs before the target, i.e. `ARM_RCBS`
    // after arming. Execute slightly more than that before the syscall.
    let timeout_rcbs = ARM_RCBS + margin;
    let (output, log) = test_fn_with_config::<LateTimerTool, _>(
        move || {
            for i in 0..ITERS {
                arm_precise_timer();
                do_branches(ARM_RCBS + i % overflow_leads);
                syscall_with_injection();
            }
        },
        Config {
            timeout_rcbs,
            retry,
        },
        true,
    )
    .expect("run late-timer-signal guest");

    let errors = log
        .inject_errors
        .lock()
        .expect("inject error log lock poisoned")
        .clone();
    if output.status != ExitStatus::Exited(0) || !errors.is_empty() {
        print_tracee_output(&output);
    }
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert_eq!(
        errors,
        Vec::<i32>::new(),
        "injected syscalls were interrupted"
    );
    assert_eq!(log.injected_syscalls.load(Ordering::SeqCst), ITERS);
    assert_eq!(log.timer_events.load(Ordering::SeqCst), 0);
}

#[test]
fn late_overflow_signal_does_not_interrupt_injected_syscall() {
    run(false);
}

#[test]
fn late_overflow_signal_is_not_delivered_to_guest_after_retry() {
    run(true);
}

#[derive(Debug, Default)]
struct ForeignLog {
    injected_syscalls: AtomicU64,
    inject_errors: Mutex<Vec<i32>>,
}

#[reverie::global_tool]
impl GlobalTool for ForeignLog {
    type Request = Report;
    type Response = ();
    type Config = Config;

    async fn receive_rpc(&self, _from: Pid, report: Report) {
        match report {
            Report::InjectedSyscall => {
                self.injected_syscalls.fetch_add(1, Ordering::SeqCst);
            }
            Report::InjectError(errno) => self
                .inject_errors
                .lock()
                .expect("inject error log lock poisoned")
                .push(errno),
            Report::TimerEvent => panic!("no timer was requested"),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct ForeignSignalTool;

#[reverie::tool]
impl Tool for ForeignSignalTool {
    type GlobalState = ForeignLog;
    type ThreadState = ();

    fn subscriptions(_cfg: &Config) -> Subscription {
        Subscription::all()
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall.number() {
            Sysno::getppid => {
                // Make the timer's signal number pending for the stopped guest, so its
                // delivery stop precedes the injected syscall instruction exactly as a
                // late overflow notification's does. Its siginfo is not an overflow
                // notification's.
                let sent = unsafe {
                    libc::syscall(
                        libc::SYS_tgkill,
                        guest.pid().as_raw(),
                        guest.tid().as_raw(),
                        libc::SIGSTKFLT,
                    )
                };
                assert_eq!(
                    sent,
                    0,
                    "tgkill failed: {}",
                    std::io::Error::last_os_error()
                );
                let result = if guest.config().retry {
                    guest.inject_with_retry(Getpid::new()).await
                } else {
                    guest.inject(Getpid::new()).await
                };
                match result {
                    Ok(_) => guest.send_rpc(Report::InjectedSyscall).await,
                    Err(errno) => guest.send_rpc(Report::InjectError(errno.into_raw())).await,
                }
                Ok(0)
            }
            _ => guest.tail_inject(syscall).await,
        }
    }
}

const FOREIGN_ITERS: u64 = 20;

static FOREIGN_SIGNALS_HANDLED: AtomicU64 = AtomicU64::new(0);
static FOREIGN_SIGNAL_OUTSIDE_HANDLER: AtomicBool = AtomicBool::new(false);

extern "C" fn count_foreign_signal(signo: libc::c_int) {
    if signo != libc::SIGSTKFLT {
        FOREIGN_SIGNAL_OUTSIDE_HANDLER.store(true, Ordering::SeqCst);
    }
    FOREIGN_SIGNALS_HANDLED.fetch_add(1, Ordering::SeqCst);
}

fn run_foreign(retry: bool) {
    let (output, log) = test_fn_with_config::<ForeignSignalTool, _>(
        || {
            // SAFETY: the handler only touches atomics.
            unsafe {
                let mut action: libc::sigaction = core::mem::zeroed();
                action.sa_sigaction = count_foreign_signal as extern "C" fn(libc::c_int) as usize;
                libc::sigemptyset(&mut action.sa_mask);
                assert_eq!(
                    libc::sigaction(libc::SIGSTKFLT, &action, core::ptr::null_mut()),
                    0
                );
            }
            for i in 0..FOREIGN_ITERS {
                syscall_with_injection();
                assert_eq!(
                    FOREIGN_SIGNALS_HANDLED.load(Ordering::SeqCst),
                    i + 1,
                    "the guest's signal was not delivered after injection {i}"
                );
            }
            assert!(!FOREIGN_SIGNAL_OUTSIDE_HANDLER.load(Ordering::SeqCst));
        },
        Config {
            timeout_rcbs: 0,
            retry,
        },
        true,
    )
    .expect("run foreign-signal guest");

    if output.status != ExitStatus::Exited(0) {
        print_tracee_output(&output);
    }
    assert_eq!(output.status, ExitStatus::Exited(0));
    let errors = log
        .inject_errors
        .lock()
        .expect("inject error log lock poisoned")
        .clone();
    if retry {
        assert_eq!(errors, Vec::<i32>::new());
        assert_eq!(log.injected_syscalls.load(Ordering::SeqCst), FOREIGN_ITERS);
    } else {
        // The pending signal interrupts the injection instead of being
        // discarded.
        assert_eq!(
            errors,
            vec![reverie::Errno::ERESTARTSYS.into_raw(); FOREIGN_ITERS as usize]
        );
        assert_eq!(log.injected_syscalls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn guest_signal_interrupts_injected_syscall() {
    run_foreign(false);
}

#[test]
fn guest_signal_is_delivered_after_retry() {
    run_foreign(true);
}
