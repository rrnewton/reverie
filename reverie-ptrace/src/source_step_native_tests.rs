/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Native regression fixtures. These require real ptrace and finite PMU timers;
//! pure-test dispatches must never select this module. No perf-denial fallback.
use std::sync::atomic::AtomicUsize;

use reverie::Guest;
use reverie::TimerSchedule;
use reverie::syscalls::Getpid;
use reverie::syscalls::Gettid;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Tgkill;

use super::*;

const GUEST: &str = r#"
#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <ucontext.h>
#include <unistd.h>
static volatile sig_atomic_t traps, usr1;
static void usr(int sig) { if (sig != SIGUSR1) _exit(83); ++usr1; }
static void trap(int sig, siginfo_t *info, void *context) {
    ucontext_t *u = context;
    if (sig != SIGTRAP || info->si_code != TRAP_TRACE) _exit(70);
    if (!(u->uc_mcontext.gregs[REG_EFL] & 0x100)) _exit(71);
    ++traps;
    u->uc_mcontext.gregs[REG_EFL] &= ~0x100;
}
static void burst(int arm) {
    // No libc call/return between the subscribed marker and the native calls.
    // The taken branch supplies the unchanged finite one-RCB target.
    long result;
    __asm__ volatile(
        "syscall\n\txor %%r10d,%%r10d\n\tjz 1f\n1:\n"
        ".rept 96\n\tmov $39,%%eax\n\tsyscall\n\t.endr\n"
        : "=a"(result)
        : "0"(SYS_write), "D"(arm ? 780L : 783L), "S"(0L), "d"(0L)
        : "rcx", "r11", "r10", "memory", "cc");
    if (result <= 0) _exit(72);
}
static void subscribed_burst(long fd) {
    long result;
    long marker = 780;
    register long target __asm__("r8") = fd;
    __asm__ volatile(
        "syscall\n\txor %%r10d,%%r10d\n\tjz 1f\n1:\n"
        "mov %[fd],%%rdi\n\t"
        ".rept 96\n\tmov $1,%%eax\n\tsyscall\n\t.endr\n"
        : "=a"(result), "+D"(marker)
        : "0"(SYS_write), "S"(0L), "d"(0L), [fd] "r"(target)
        : "rcx", "r11", "r10", "memory", "cc");
    if (result <= 0 || syscall(SYS_write, 786, 0, 0) != 1) _exit(86);
}
int main(int argc, char **argv) {
    if (argc != 2) return 73;
    int mode = atoi(argv[1]);
    struct sigaction action = {0}, actual = {0};
    sigset_t mask, actual_mask;
    sigemptyset(&mask);
    if (mode == 0) {
        action.sa_handler = SIG_IGN;
        sigemptyset(&action.sa_mask);
        sigaddset(&mask, SIGTRAP);
        if (sigaction(SIGTRAP, &action, 0) || sigprocmask(SIG_BLOCK, &mask, 0)) return 74;
        burst(0);
        if (sigaction(SIGTRAP, 0, &actual) || sigprocmask(SIG_SETMASK, 0, &actual_mask)) return 75;
        if (actual.sa_handler != SIG_IGN || sigismember(&actual_mask, SIGTRAP) != 1) return 76;
    } else if (mode == 1) {
        action.sa_sigaction = trap; action.sa_flags = SA_SIGINFO;
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGTRAP, &action, 0)) return 77;
        __asm__ volatile("pushfq; orq $0x100,(%%rsp); popfq; nop" ::: "memory", "cc");
        if (traps != 1) return 78;
        long result = SYS_getpid;
        __asm__ volatile("pushfq; orq $0x100,(%%rsp); popfq; syscall; nop"
            : "+a"(result) : : "rcx", "r11", "memory", "cc");
        if (result <= 0 || traps != 2) return 82;
        burst(0);
    } else if (mode == 3) {
        action.sa_handler = usr;
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGUSR1, &action, 0)) return 84;
        // The Tool queues SIGUSR1 in a completed private tgkill, then injects
        // getpid. Delivery before that attempt's ENTRY must retain its owner.
        if (syscall(SYS_write, 784, 0, 0) != getpid() || usr1 != 1) return 85;
    } else if (mode == 4 || mode == 5) {
        subscribed_burst(mode == 4 ? 785 : 787);
    } else {
        burst(1);
        if (syscall(SYS_write, 781, 0, 0) != getpid()) return 79;
        if (syscall(SYS_write, 782, 0, 0) != 23) return 80;
    }
    char source[4] = {'A','B','C','D'};
    if (syscall(SYS_write, 778, source, 4) != 4) return 81;
    return 0;
}
"#;

#[derive(Default)]
struct StepLog {
    clocks: StdMutex<Vec<u64>>,
    timers: AtomicUsize,
    private_calls: AtomicUsize,
    emulated_calls: AtomicUsize,
    positives: AtomicUsize,
}
#[reverie::global_tool]
impl GlobalTool for StepLog {
    type Config = u8;
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
#[derive(Default)]
struct StepTool;
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct StepThreadState {
    // Set only after the same callback's actual tgkill has returned. The only
    // remaining operation is its last Getpid and returning that exact value.
    drain_last_identity: bool,
}
#[reverie::tool]
impl Tool for StepTool {
    type GlobalState = StepLog;
    type ThreadState = StepThreadState;
    async fn handle_private_interruption<G: Guest<Self>>(
        &self,
        guest: &mut G,
        event: &reverie::PrivateInterruption,
    ) -> Result<reverie::PrivateInterruptionAction, Error> {
        let (logical, args) = event.logical_call();
        if guest.thread_state().drain_last_identity
            && logical == Sysno::write
            && args.arg0 == 784
            && event.helper() == Getpid::new().into_parts()
            && event.signal() == Signal::SIGUSR1
        {
            guest.claim_private_interruption(event)?;
            guest.thread_state_mut().drain_last_identity = false;
            Ok(reverie::PrivateInterruptionAction::DrainHelperResult)
        } else {
            Ok(reverie::PrivateInterruptionAction::Unsupported)
        }
    }
    fn subscriptions(_: &u8) -> Subscription {
        [Sysno::write].into_iter().collect()
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        let (_, args) = call.into_parts();
        match args.arg0 {
            780 => {
                crate::timer::TOOL_STEP_WITNESS.with(|witness| witness.borrow_mut().clear());
                let clock = guest.read_clock()?;
                guest
                    .local_global_state()
                    .unwrap()
                    .clocks
                    .lock()
                    .unwrap()
                    .push(clock);
                guest.set_timer_precise(TimerSchedule::RcbsAndInstructions(1, 64))?;
                Ok(0)
            }
            783 => Ok(0),
            785 => {
                let before = crate::timer::TOOL_STEP_WITNESS.with(|witness| witness.borrow().len());
                let result = guest.inject(Getpid::new()).await?;
                guest.inject(Gettid::new()).await?;
                assert_eq!(
                    crate::timer::TOOL_STEP_WITNESS.with(|witness| witness.borrow().len()),
                    before,
                    "private EXIT cannot complete the original guest step before Tool return"
                );
                Ok(result)
            }
            787 => Ok(23),
            786 => {
                crate::timer::TOOL_STEP_WITNESS.with(|witness| {
                    let witness = witness.borrow();
                    assert_eq!(
                        witness.len(),
                        1,
                        "actual precise SECCOMP transfer must complete once"
                    );
                    let (before, after) = witness[0];
                    assert_eq!(
                        after,
                        (before.0, before.1 + 1),
                        "exact original instruction count"
                    );
                });
                Ok(1)
            }
            781 => {
                guest
                    .local_global_state()
                    .unwrap()
                    .private_calls
                    .fetch_add(1, Ordering::SeqCst);
                Ok(guest.inject(Getpid::new()).await?)
            }
            784 => {
                let pid = guest.inject(Getpid::new()).await?;
                let tid = guest.inject(Gettid::new()).await?;
                guest
                    .inject(
                        Tgkill::new()
                            .with_tgid(pid as _)
                            .with_tid(tid as _)
                            .with_sig(libc::SIGUSR1),
                    )
                    .await?;
                guest.thread_state_mut().drain_last_identity = true;
                Ok(guest.inject(Getpid::new()).await?)
            }
            782 => {
                guest
                    .local_global_state()
                    .unwrap()
                    .emulated_calls
                    .fetch_add(1, Ordering::SeqCst);
                Ok(23)
            }
            778 => {
                let bytes = guest
                    .read_native_source(args.arg1, args.arg2, Box::new(()))
                    .await;
                assert_eq!(
                    bytes,
                    Ok(b"ABCD".to_vec()),
                    "source-positive after actual observer/private/step paths"
                );
                guest
                    .local_global_state()
                    .unwrap()
                    .positives
                    .fetch_add(1, Ordering::SeqCst);
                Ok(4)
            }
            _ => Ok(guest.inject(call).await?),
        }
    }
    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        let clock = guest.read_clock().unwrap();
        let log = guest.local_global_state().unwrap();
        log.clocks.lock().unwrap().push(clock);
        log.timers.fetch_add(1, Ordering::SeqCst);
    }
}
async fn run(mode: u8) {
    static EXECUTABLE: LazyLock<PathBuf> = LazyLock::new(|| {
        let path = std::env::temp_dir().join(format!("reverie-source-step-{}", std::process::id()));
        let source = path.with_extension("c");
        std::fs::write(&source, GUEST).unwrap();
        let status = std::process::Command::new("cc")
            .args(["-O0", "-g"])
            .arg(&source)
            .arg("-o")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path
    });
    let mut command = Command::new(EXECUTABLE.as_path());
    command.arg(mode.to_string());
    let tracer = TracerBuilder::<StepTool>::new(command)
        .config(mode)
        .spawn()
        .await
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), tracer.wait_completion())
        .await
        .expect("source step Command deadline");
    let complete = match outcome {
        ToolRunOutcome::Complete(complete) => complete,
        ToolRunOutcome::CleanupPending(pending) => {
            panic!("source step cleanup pending: {:#}", pending.quarantine())
        }
        ToolRunOutcome::UnsupportedBackend(_) => panic!("ordinary Command unsupported"),
    };
    assert_eq!(complete.result.unwrap(), ExitStatus::Exited(0));
    let log = complete.global_state;
    assert_eq!(log.positives.load(Ordering::SeqCst), 1);
    if mode == 2 {
        assert_eq!(log.private_calls.load(Ordering::SeqCst), 1);
        assert_eq!(log.emulated_calls.load(Ordering::SeqCst), 1);
        assert_eq!(log.timers.load(Ordering::SeqCst), 1);
        let clocks = log.clocks.lock().unwrap();
        assert_eq!(clocks.len(), 2);
        assert!(clocks[1] > clocks[0], "finite counter must advance");
    }
}
#[tokio::test(flavor = "current_thread")]
async fn observed_native_syscalls_preserve_ignored_blocked_sigtrap() {
    run(0).await;
}
#[tokio::test(flavor = "current_thread")]
async fn actual_guest_tf_debug_reaches_the_original_signal_frame() {
    run(1).await;
}
#[tokio::test(flavor = "current_thread")]
async fn precise_native_syscalls_private_and_emulated_tool_calls_keep_source_positive() {
    run(2).await;
}

#[tokio::test(flavor = "current_thread")]
async fn private_attempt_preentry_signal_preserves_native_result_and_context() {
    run(3).await;
}

#[tokio::test(flavor = "current_thread")]
async fn precise_tool_transfer_survives_private_exits_and_completes_once() {
    run(4).await;
}

#[tokio::test(flavor = "current_thread")]
async fn precise_tool_emulation_completes_the_original_instruction_once() {
    run(5).await;
}

#[path = "source_private_continuation_tests.rs"]
mod private_continuation;

#[path = "source_private_replay_tests.rs"]
mod private_replay;
