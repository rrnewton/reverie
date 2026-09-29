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
use reverie_ptrace::testing::KeptTimerProgrammingChecks;
use reverie_ptrace::testing::assert_at_target_unless_witnessed;

/// The skid witness count and the kept programming checks are process
/// global; each test owns them while it runs.
static COUNTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs `test` on a current-thread runtime of its own, owning `COUNTS`.
fn with_counts(test: impl std::future::Future<Output = ()>) {
    let _owner = COUNTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

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
// nothing fires. The notification is queued at the trap, held back by the
// blocked signal, but without records a queued notification cannot be told
// from a lost one, so no witness may be counted as overtaken with its
// notification queued (see
// `reverie_ptrace::testing::precise_events_overtaken_with_notification_queued`).
// No trap keeps an event, so none has its programming checked.
#[test]
fn an_rt_sigreturn_hook_trap_past_the_target_is_witnessed_once() {
    reverie_ptrace::ret_without_perf!();
    with_counts(rt_sigreturn_hook_trap_past_the_target_is_witnessed_once());
}

async fn rt_sigreturn_hook_trap_past_the_target_is_witnessed_once() {
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
    // The counts are process global; see `COUNTS`.
    let _ = reverie::take_skid_overshoot_count();
    let overtaken_before =
        reverie_ptrace::testing::precise_events_overtaken_with_notification_queued();
    let checks = KeptTimerProgrammingChecks::start();
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
    let keeps = checks.keeps();
    let witnesses = reverie::take_skid_overshoot_count();
    let overtaken = reverie_ptrace::testing::precise_events_overtaken_with_notification_queued()
        - overtaken_before;
    assert_eq!(keeps, 0, "no trap here keeps its event");
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
    assert_eq!(
        overtaken, 0,
        "without overflow records no witness may be counted as overtaken with its \
         notification queued"
    );
}

// The rt_sigreturn hook's trap before the keep point, as in
// reverie-liteinst/tests/hybrid.rs's
// `an_rt_sigreturn_hook_trap_keeps_the_timer_event`, without records: each
// handler returns 5000 RCBs after its request for an event 30000 RCBs past
// it, above the largest skid margin in Reverie's PMU table, and the trap
// must keep the event with its PMU programming as the request made it,
// checked at the trap and again at the thread's next stop (see
// `reverie_ptrace::testing::check_kept_timer_programming`). Without records
// the trap reads the guest's clock from the counter alone, so this is the
// check's only coverage of that path.
//
// Every event must fire at its target, or past it only as a witnessed skid
// overshoot. The one exception is the host timing that hybrid.rs's
// `overtaken_rounds` identifies with records: a notification serviced only
// at the next round's signal, which overtakes the kept event past its
// target and witnesses it. Without records a queued notification cannot be
// told from a lost one, so such a round is not listed as overtaken; it is
// missing, and witnessed. At most `MISSING_CAP` rounds may be missing, the
// cap hybrid.rs's `overtaken_cap` gives for the 15 rounds that could be
// overtaken (the last cannot be, since the guest makes no stop between its
// handler's return and its exit), and every witness must be a late event
// or a missing round. Since a lost notification is also a missing round,
// and witnessed, the cap admits as many witnessed losses too.
#[test]
fn an_rt_sigreturn_hook_trap_keeps_the_timer_event_without_records() {
    reverie_ptrace::ret_without_perf!();
    with_counts(rt_sigreturn_hook_trap_keeps_the_timer_event_without_records());
}

async fn rt_sigreturn_hook_trap_keeps_the_timer_event_without_records() {
    reverie_ptrace::testing::disable_timer_overflow_records();
    /// hybrid.rs's `overtaken_cap(15)`.
    const MISSING_CAP: u64 = 2;
    let rcbs: u64 = 30_000;
    let (lead, after) = (5_000, 2 * rcbs);
    let rounds: u64 = 16;
    let (_directory, guest) = compile_fixture("hybrid_sigreturn_hook_timer.c");
    let mut command = Command::new(guest);
    command.args([lead, rounds, after, 1, 0, 0].iter().map(u64::to_string));
    // The counts are process global; see `COUNTS`.
    let _ = reverie::take_skid_overshoot_count();
    let checks = KeptTimerProgrammingChecks::start();
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
    let keeps = checks.keeps();
    let witnesses = reverie::take_skid_overshoot_count();
    assert_eq!(output.status, ExitStatus::Exited(0), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("rounds={rounds} handled={rounds} wrong=0\n")
    );
    assert_eq!(global.requests.load(Ordering::SeqCst), rounds);
    assert_eq!(
        keeps, rounds,
        "each round's trap must keep its event, with its programming unchanged"
    );
    let events = global.timer_events.into_inner().unwrap();
    assert!(events.len() as u64 <= rounds, "{events:?}");
    let missing = rounds - events.len() as u64;
    assert!(
        missing <= MISSING_CAP,
        "{missing} of {rounds} kept events did not fire: {events:?}"
    );
    let late = events.iter().filter(|&&clock| clock > rcbs).count() as u64;
    eprintln!(
        "{missing} of {rounds} kept events missing, at most {MISSING_CAP}; {late} of {} fired \
         past the target {rcbs}; {witnesses} witnessed",
        events.len()
    );
    assert_eq!(
        witnesses,
        late + missing,
        "every witness must be a late event or a missing round: {events:?}"
    );
    assert_at_target_unless_witnessed(&events, rcbs, witnesses - missing);
}
