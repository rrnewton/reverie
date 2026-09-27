/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Only Tool-observable events count against a pending precise timer (see the
//! `reverie_ptrace::timer` module header). A LiteInst run stops the guest for
//! reasons of its own that ordinary ptrace does not have: the runtime's
//! handshake traps, controller-only mapping observations, and unsubscribed
//! syscalls that reach a patched site. None of them may use up the grace tick
//! the timer's own signal stop needs, or the timer is silently cancelled and
//! the slice boundary moves.
//!
//! Each test requests a precise timer at the guest's last Tool-observable
//! syscall before a syscall-free spin, with internal stops in between, and
//! requires the timer to fire at exactly its target.

#![cfg(target_arch = "x86_64")]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::Mutex;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::process::Command;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::LiteinstBackend;
use serde::Deserialize;
use serde::Serialize;

/// Far below the fixture's spin of 40 million iterations, each with at least
/// one conditional branch.
const TIMEOUT_RCBS: u64 = 1_000_000;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
enum Report {
    /// A precise timer was requested at this clock, at a stop for `nr`.
    Requested { nr: i32, clock: u64 },
    /// The timer event fired at this clock.
    Fired { clock: u64 },
}

#[derive(Debug, Default)]
struct Log {
    reports: Mutex<Vec<Report>>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
struct Config {
    /// Subscribe to every syscall rather than to getpid alone.
    all_syscalls: bool,
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Request = Report;
    type Response = ();
    type Config = Config;

    async fn receive_rpc(&self, _from: Pid, report: Report) {
        self.reports.lock().unwrap().push(report);
    }
}

#[derive(Debug, Default, Clone)]
struct TimerAtEverySyscall;

#[reverie::tool]
impl Tool for TimerAtEverySyscall {
    type GlobalState = Log;
    type ThreadState = ();

    fn subscriptions(config: &Config) -> Subscription {
        if config.all_syscalls {
            Subscription::all_syscalls()
        } else {
            [Sysno::getpid].into_iter().collect()
        }
    }

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let nr = syscall.number();
        if matches!(nr, Sysno::exit | Sysno::exit_group) {
            guest.tail_inject(syscall).await;
        }
        let result = guest.inject(syscall).await;
        // The posthook: each request replaces the previous one, so the request
        // at the last syscall before the spin is the one still pending there.
        let clock = guest.read_clock()?;
        guest.set_timer_precise(TimerSchedule::Rcbs(TIMEOUT_RCBS))?;
        guest
            .send_rpc(Report::Requested { nr: nr.id(), clock })
            .await;
        Ok(result?)
    }

    async fn handle_timer_event<T: Guest<Self>>(&self, guest: &mut T) {
        let clock = guest.read_clock().expect("read clock at timer event");
        guest.send_rpc(Report::Fired { clock }).await;
    }
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

/// Run the fixture in `mode` and return the request whose timer fired in the
/// spin, after checking every timer event.
async fn run_mode(mode: &str, all_syscalls: bool) -> (i32, u64) {
    let (_directory, guest) = compile_fixture("internal_stop_timer.c");
    let mut command = Command::new(guest);
    command.arg(mode);
    // The tracer runs in this process, and tests run one at a time.
    let _ = reverie::take_skid_overshoot_count();
    let (output, log) = LiteinstBackend::run_host_with_output_and_preload::<TimerAtEverySyscall>(
        command,
        Config { all_syscalls },
        preload_path(),
    )
    .await
    .unwrap();
    let overshoots = reverie::take_skid_overshoot_count();
    assert!(output.status.success(), "{mode}: {output:?}");
    let reports = log.reports.lock().unwrap().clone();

    // Every event, including any that fired before the spin, is at its target.
    // The one exception is the timer's documented degradation: a heavy-tailed
    // PMU skid can carry the guest past the target before the overflow
    // interrupt is taken, and single stepping cannot move it back. That event
    // fires late and the backend records the overshoot, so a late event is
    // accepted only against a recorded overshoot. An early event never is.
    let mut late = 0;
    for (index, report) in reports.iter().enumerate() {
        if let Report::Fired { clock } = report {
            let target = match index.checked_sub(1).map(|previous| reports[previous]) {
                Some(Report::Requested {
                    clock: requested, ..
                }) => requested + TIMEOUT_RCBS,
                _ => panic!("{mode}: timer event {index} without a request: {reports:?}"),
            };
            if *clock > target {
                late += 1;
            } else {
                assert_eq!(
                    *clock, target,
                    "{mode}: timer event {index} missed its target: {reports:?}"
                );
            }
        }
    }
    assert!(
        late <= overshoots,
        "{mode}: {late} late timer events but {overshoots} recorded skid overshoots: {reports:?}"
    );
    // The spin is the last thing the guest does before exit_group, which
    // makes no request. So the final report must be the spin's event.
    match reports.as_slice() {
        [.., Report::Requested { nr, clock }, Report::Fired { .. }] => (*nr, *clock),
        _ => panic!(
            "{mode}: the timer requested before the spin never fired; a tracer-internal stop cancelled it. Last reports: {:?}",
            &reports[reports.len().saturating_sub(4)..]
        ),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn handshake_ready_trap_does_not_cancel_a_pending_timer() {
    // The runtime's syscalls between its Begin and Ready traps reach the Tool;
    // Ready itself does not, and main spins right after it.
    let (nr, _) = run_mode("handshake", true).await;
    assert_ne!(
        Sysno::from(nr),
        Sysno::getpid,
        "the fixture's handshake mode must not issue its own syscall"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn handshake_begin_trap_does_not_cancel_a_pending_timer() {
    // The Tool sees only the getpid issued before the runtime initializes.
    // Every later stop until the timer fires is LiteInst-internal: Begin,
    // the runtime's controller-only mapping syscalls, and Ready.
    let (nr, _) = run_mode("begin", false).await;
    assert_eq!(Sysno::from(nr), Sysno::getpid);
}

#[tokio::test(flavor = "current_thread")]
async fn controller_mapping_stop_does_not_cancel_a_pending_timer() {
    let (nr, _) = run_mode("mapping", false).await;
    assert_eq!(Sysno::from(nr), Sysno::getpid);
}

#[tokio::test(flavor = "current_thread")]
async fn unsubscribed_patched_site_syscall_does_not_cancel_a_pending_timer() {
    let (nr, _) = run_mode("unsubscribed", false).await;
    assert_eq!(Sysno::from(nr), Sysno::getpid);
}
