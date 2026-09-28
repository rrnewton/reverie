/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! LiteInst hook traps with precise timers that map no overflow records, as
//! on a kernel that is or may be `PREEMPT_RT`. Turning the records off is
//! process global and permanent, so these tests have a binary of their own;
//! reverie-liteinst/tests/hybrid.rs has the same traps with records.

#![cfg(target_arch = "x86_64")]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tid;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::process::Command;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::LiteinstBackend;

fn preload_path() -> PathBuf {
    let launcher = PathBuf::from(env!("CARGO_BIN_EXE_reverie-liteinst-strace"));
    let target = launcher.parent().unwrap();
    [
        target.join("libreverie_liteinst.so"),
        target.join("deps/libreverie_liteinst.so"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .expect("cargo did not build the LiteInst preload cdylib")
}

fn compile_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let output = directory.path().join(name.trim_end_matches(".c"));
    let compiler = std::env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
    let result = ProcessCommand::new(compiler)
        .args(["-std=gnu11", "-O0", "-fno-pie", "-no-pie"])
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "failed to compile {}:\n{}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    (directory, output)
}

#[derive(Debug, Default)]
struct SigreturnHookTimerEvents {
    requests: AtomicU64,
    /// The guest's RCBs from the request to each timer event.
    timer_events: std::sync::Mutex<Vec<u64>>,
}

#[reverie::global_tool]
impl GlobalTool for SigreturnHookTimerEvents {
    /// A timer event's RCBs from its request, or `None` for a request.
    type Request = Option<u64>;
    type Response = ();
    /// The timer's RCBs from each request to its target.
    type Config = u64;

    async fn receive_rpc(&self, _from: Tid, note: Option<u64>) {
        match note {
            None => {
                self.requests.fetch_add(1, Ordering::SeqCst);
            }
            Some(rcbs) => self.timer_events.lock().unwrap().push(rcbs),
        }
    }
}

/// Answers every getpid itself, with 0x4242, and requests a precise timer at
/// a getpid whose first argument is 1. It subscribes nothing else, so the
/// rt_sigreturn of `hybrid_sigreturn_hook_timer.c`'s restorer is a hook trap
/// that makes no Tool callback.
#[derive(Default)]
struct SigreturnHookTimerTool;

#[reverie::tool]
impl Tool for SigreturnHookTimerTool {
    type GlobalState = SigreturnHookTimerEvents;
    /// The guest's RCB clock at the latest request.
    type ThreadState = u64;

    fn subscriptions(_config: &u64) -> Subscription {
        [Sysno::getpid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(syscall.number(), Sysno::getpid);
        let (_, args) = syscall.into_parts();
        if args.arg0 == 1 {
            *guest.thread_state_mut() = guest.read_clock()?;
            guest.send_rpc(None).await;
            guest.set_timer_precise(TimerSchedule::Rcbs(*guest.config()))?;
        }
        Ok(0x4242)
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        let rcbs = guest.read_clock().unwrap() - *guest.thread_state();
        guest.send_rpc(Some(rcbs)).await;
    }
}

// The rt_sigreturn hook's trap past the target, with the timer's signal
// blocked, so that no notification can deliver the event before the trap.
// Without records the trap, a stop past the period, cancels the event as a
// stop the Tool observes does, and records its delivery point as a skid
// overshoot; the rt_sigreturn trap then retires the event, which must not
// record it a second time. So each round is witnessed exactly once, and
// nothing fires.
#[tokio::test(flavor = "current_thread")]
async fn an_rt_sigreturn_hook_trap_past_the_target_is_witnessed_once() {
    reverie_ptrace::ret_without_perf!();
    reverie_ptrace::testing::disable_timer_overflow_records();
    let margin = reverie_ptrace::PmuConfig::new().skid_margin();
    let rcbs = 10_000 + margin;
    let rounds: u64 = 16;
    let (_directory, guest) = compile_fixture("hybrid_sigreturn_hook_timer.c");
    let mut command = Command::new(guest);
    // The handler returns twice the timer's RCBs after its request, with the
    // timer's signal blocked throughout.
    command.args([
        (2 * rcbs).to_string(),
        rounds.to_string(),
        1_000.to_string(),
        1.to_string(),
        0.to_string(),
        1.to_string(),
    ]);
    // The count is process global, and this binary runs this test alone.
    let _ = reverie::take_skid_overshoot_count();
    let (output, global) = tokio::time::timeout(
        Duration::from_secs(120),
        LiteinstBackend::run_host_with_output_and_preload::<SigreturnHookTimerTool>(
            command,
            rcbs,
            preload_path(),
        ),
    )
    .await
    .expect("the rt_sigreturn hook guest did not complete")
    .unwrap();
    let witnesses = reverie::take_skid_overshoot_count();
    assert_eq!(output.status, ExitStatus::Exited(0), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("rounds={rounds} handled={rounds} wrong=0\n")
    );
    assert_eq!(global.requests.load(Ordering::SeqCst), rounds);
    assert_eq!(
        global.timer_events.into_inner().unwrap(),
        Vec::<u64>::new(),
        "the cancelled timer must not fire"
    );
    assert_eq!(
        witnesses, rounds,
        "each round's trap overtook its due event, and must be witnessed once"
    );
}
