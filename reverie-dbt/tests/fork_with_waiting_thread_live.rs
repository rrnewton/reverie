/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Live regressions for a fork while another thread waits inside DynamoRIO
//! (<https://github.com/rrnewton/hermit/issues/4483>).
//!
//! The inject-waits test Tool runs the guest's `select` and `pselect6` through
//! `Guest::inject`, as Detcore does for a blocking external wait. DynamoRIO
//! cannot suspend a thread inside that call, so the fork's synch retries for
//! about 50 s and interrupts the wait with its suspend signal on every round.
//! Each case checks that the guest sees exactly what Linux shows it (the same
//! fixture runs natively first), that each fork stays within a 70 s bound on
//! the host clock, and that the re-issue path really ran.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

use reverie_dbt::DbtRunner;

const FIXTURE: &str = "fork_with_waiting_thread";
/// The step-1 bound on one fork: DynamoRIO's synch gives up after about 50 s.
const FORK_BOUND_US: u64 = 70_000_000;
/// Fewer re-issues than a full synch's rounds (10000 retries) means something
/// other than the end of the synch ended the wait.
const SYNCH_ROUNDS: u64 = 10_000;

fn compile_fixture(directory: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{FIXTURE}.c"));
    let output = directory.join(FIXTURE);
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = Command::new(compiler)
        .args(["-O2", "-g", "-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .arg("-pthread")
        .arg("-o")
        .arg(&output)
        .status()
        .unwrap_or_else(|error| panic!("compile {}: {error}", source.display()));
    assert!(status.success(), "fixture compilation failed");
    output
}

struct Run {
    native: Output,
    dbt: Output,
    dbt_elapsed: std::time::Duration,
}

impl Run {
    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.dbt.stderr).into_owned()
    }

    /// Each timed fork as (host microseconds, forking process id), in order.
    fn forks(&self) -> Vec<(u64, u32)> {
        self.stderr()
            .lines()
            .filter_map(|line| line.strip_prefix("reverie-dbt-test: fork host_us="))
            .map(|rest| {
                let (us, pid) = rest.split_once(" pid=").unwrap();
                (us.parse().unwrap(), pid.parse().unwrap())
            })
            .collect()
    }

    /// The tids the runtime reported as exiting in the traced root, in order.
    fn thread_exits(&self) -> Vec<String> {
        self.stderr()
            .lines()
            .filter_map(|line| line.strip_prefix("reverie-dbt-test: runtime thread exit tid="))
            .map(str::to_owned)
            .collect()
    }

    /// Asserts that `threads` distinct threads each exited exactly once.
    fn assert_each_thread_exited_once(&self, threads: usize) {
        let exits = self.thread_exits();
        let mut distinct = exits.clone();
        distinct.sort();
        distinct.dedup();
        assert!(
            exits.len() == threads && distinct.len() == threads,
            "expected {threads} threads to exit once each, got {exits:?}: {}",
            self.stderr()
        );
    }

    /// The Tool's single witness line for its wait (restarts, result, sticky),
    /// joined with the backend's line for the wait's final `pselect6` issue
    /// (whether a deferred signal was deliverable, whether SIGALRM stayed in the
    /// wait mask), if there is one.
    fn reissue(&self) -> Option<Reissue> {
        let stderr = self.stderr();
        let lines: Vec<_> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix("reverie-dbt-test: injected wait reissued "))
            .collect();
        assert!(lines.len() <= 1, "more than one re-issued wait: {lines:?}");
        let line = lines.into_iter().next()?;
        let backend = stderr
            .lines()
            .filter_map(|line| line.strip_prefix("reverie-dbt-test: injected pselect6 "))
            .next_back()
            .unwrap_or_else(|| panic!("no backend pselect6 line: {stderr}"));
        let field = |line: &str, name: &str| -> i64 {
            line.split(' ')
                .find_map(|word| word.strip_prefix(&format!("{name}=")))
                .unwrap_or_else(|| panic!("no {name} in {line}"))
                .parse()
                .unwrap()
        };
        Some(Reissue {
            count: field(line, "count") as u64,
            result: field(line, "result"),
            eligible: field(backend, "deliverable") != 0,
            sticky: field(line, "sticky") != 0,
            alarm_blocked: field(backend, "alarm_blocked") != 0,
        })
    }
}

#[derive(Debug)]
struct Reissue {
    count: u64,
    result: i64,
    eligible: bool,
    sticky: bool,
    alarm_blocked: bool,
}

fn run(mode: &str, dynamorio_options: &[&str]) -> Run {
    run_with(mode, dynamorio_options, &[])
}

fn run_with(mode: &str, dynamorio_options: &[&str], client_arguments: &[&str]) -> Run {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = compile_fixture(directory.path());
    let native = Command::new(&fixture)
        .arg(mode)
        .output()
        .expect("run the fixture natively");
    assert!(native.status.success(), "native run failed: {native:?}");

    // Bound the launcher outside DynamoRIO, so a regression that hangs the
    // fork fails the test instead of stalling the suite.
    let launcher = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bounded_drrun.sh");
    let client = std::env::var_os("REVERIE_DBT_CLIENT")
        .expect("REVERIE_DBT_CLIENT must point to the built native client");
    let mut runner = DbtRunner::new(launcher, client)
        .expect("create bounded native runner")
        .dynamorio_option("-reverie_test_hooks")
        .client_argument("-test-fork-timing");
    for option in dynamorio_options {
        runner = runner.dynamorio_option(*option);
    }
    for argument in client_arguments {
        runner = runner.client_argument(*argument);
    }
    let mut guest = Command::new(&fixture);
    guest
        .arg(mode)
        .env("REVERIE_DBT_TEST_DRRUN", reverie_dbt::bundled_drrun_path())
        .env("HERMIT_DBT_TEST_INJECT_WAITS", "1");
    let start = std::time::Instant::now();
    let dbt = runner
        .output(&guest)
        .unwrap_or_else(|error| panic!("run {mode}: {error}"));
    Run {
        native,
        dbt,
        dbt_elapsed: start.elapsed(),
    }
}

/// Runs `mode` and checks everything every case shares: the native and DBT
/// outputs both equal `expected`, nothing was refused, no synch count went
/// negative, and every fork stayed within the bound.
fn run_and_check(mode: &str, expected: &str) -> Run {
    check(mode, run(mode, &[]), expected)
}

fn check(mode: &str, run: Run, expected: &str) -> Run {
    assert_eq!(
        String::from_utf8_lossy(&run.native.stdout),
        expected,
        "the fixture's native output changed: {:?}",
        run.native
    );
    assert!(run.dbt.status.success(), "DBT run failed: {:?}", run.dbt);
    assert_eq!(
        String::from_utf8_lossy(&run.dbt.stdout),
        expected,
        "DBT output differs from Linux: {:?}",
        run.dbt
    );
    let stderr = run.stderr();
    assert!(!stderr.contains("refused"), "DBT refused: {stderr}");
    assert!(
        !stderr.contains("pending_synch_count below zero"),
        "unbalanced synch exit: {stderr}"
    );
    // Every fork the mode makes is timed, in order, and each stays within the
    // bound: a missing measurement cannot hide behind another fork's.
    let forks = run.forks();
    let expected = expected_forks(mode);
    assert_eq!(forks.len(), expected.len(), "fork measurements: {stderr}");
    for (index, ((us, pid), same_as_first)) in forks.iter().zip(expected).enumerate() {
        assert!(
            *us <= FORK_BOUND_US,
            "fork {index} took {us} us, over the {FORK_BOUND_US} us bound: {stderr}"
        );
        assert_eq!(
            *pid == forks[0].1,
            same_as_first,
            "fork {index} came from the wrong process: {stderr}"
        );
    }
    run
}

/// The forks `mode` makes, in order. Each entry says whether that fork comes
/// from the same process as the first one.
fn expected_forks(mode: &str) -> Vec<bool> {
    match mode {
        // A single-threaded signal sender is forked first, then the fork under
        // test, both from the root.
        "arrive" | "signal" | "sigchld" => vec![true, true],
        // The root forks a child, which forks from a new thread.
        "nested" => vec![true, false],
        _ => vec![true],
    }
}

fn waiter_line(mode: &str, rest: &str) -> String {
    format!("waiter mode={mode} {rest}\nchild code=3\n")
}

const QUIET: &str = "r=1 handled=0 usr1_pending=0 usr2_pending=0";

/// A wait that the synch interrupted on every round, and that ended only when
/// its data arrived after the fork.
fn assert_reissued_through_the_synch(run: &Run) {
    let reissue = run.reissue().expect("the wait was never re-issued");
    assert!(
        reissue.count >= SYNCH_ROUNDS && reissue.result == 1 && !reissue.eligible,
        "{reissue:?}"
    );
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn select_waits_through_another_threads_fork() {
    let run = run_and_check("select", &waiter_line("select", QUIET));
    assert_reissued_through_the_synch(&run);
    // The root's two threads; the fork child's copies are not reported.
    run.assert_each_thread_exited_once(2);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn masked_pselect6_waits_through_another_threads_fork() {
    let run = run_and_check("pselect", &waiter_line("pselect", QUIET));
    assert_reissued_through_the_synch(&run);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_blocked_signal_pending_before_the_wait_stays_pending() {
    let run = run_and_check(
        "blocked",
        &waiter_line("blocked", "r=1 handled=0 usr1_pending=0 usr2_pending=1"),
    );
    assert_reissued_through_the_synch(&run);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_blocked_signal_arriving_during_the_wait_stays_pending() {
    let run = run_and_check(
        "arrive",
        &waiter_line("arrive", "r=1 handled=0 usr1_pending=1 usr2_pending=0"),
    );
    assert_reissued_through_the_synch(&run);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_waiter_blocking_every_signal_still_lets_the_synch_end() {
    let run = run_and_check("blockall", &waiter_line("blockall", QUIET));
    assert_reissued_through_the_synch(&run);
    // With no DynamoRIO or client itimer armed, the wait mask is the
    // application's, SIGALRM included.
    assert!(run.reissue().unwrap().alarm_blocked);
}

/// With a client itimer armed, its alarm signal must stay deliverable during
/// the re-issued wait even though the application blocks it.
#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn an_armed_client_itimer_stays_deliverable_during_the_wait() {
    let run = check(
        "itimer",
        run_with("itimer", &[], &["-test-itimer"]),
        &waiter_line("itimer", QUIET),
    );
    assert_reissued_through_the_synch(&run);
    let reissue = run.reissue().unwrap();
    assert!(!reissue.alarm_blocked, "{reissue:?}");
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn an_unblocked_signal_to_the_waiter_ends_the_wait_with_eintr() {
    let run = run_and_check(
        "signal",
        &waiter_line(
            "signal",
            "r=-1 errno=Interrupted system call handled=1 usr1_pending=0 usr2_pending=0",
        ),
    );
    let reissue = run.reissue().expect("the wait was never re-issued");
    assert!(
        reissue.eligible && reissue.result == -4 && reissue.count < SYNCH_ROUNDS,
        "the signal, not the end of the synch, must end the wait: {reissue:?}"
    );
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_default_ignored_signal_does_not_end_the_wait() {
    let run = run_and_check("sigchld", &waiter_line("sigchld", QUIET));
    assert_reissued_through_the_synch(&run);
}

fn assert_timed_out(run: &Run, sticky: bool) {
    let reissue = run.reissue().expect("the wait was never re-issued");
    assert!(
        reissue.result == 0 && !reissue.eligible && reissue.sticky == sticky,
        "{reissue:?}"
    );
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_select_timeout_expires_on_time_during_the_synch() {
    let run = run_and_check(
        "timeout",
        &waiter_line(
            "timeout",
            "r=0 handled=0 usr1_pending=0 usr2_pending=0 timeout=0.000000 within_5s=1",
        ),
    );
    assert_timed_out(&run, false);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_pselect6_timeout_expires_on_time_during_the_synch() {
    let run = run_and_check(
        "ptimeout",
        &waiter_line(
            "ptimeout",
            "r=0 handled=0 usr1_pending=0 usr2_pending=0 timeout=0.000000 within_5s=1",
        ),
    );
    assert_timed_out(&run, false);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_sticky_select_timeout_expires_on_time_and_is_not_written() {
    let run = run_and_check(
        "sticky",
        &waiter_line(
            "sticky",
            "r=0 handled=0 usr1_pending=0 usr2_pending=0 timeout=2.000000 within_5s=1",
        ),
    );
    assert_timed_out(&run, true);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_sticky_pselect6_timeout_expires_on_time_and_is_not_written() {
    let run = run_and_check(
        "psticky",
        &waiter_line(
            "psticky",
            "r=0 handled=0 usr1_pending=0 usr2_pending=0 timeout=2.000000 within_5s=1",
        ),
    );
    assert_timed_out(&run, true);
}

/// A sticky timeout too long to add to the clock saturates, as Linux's
/// `poll_select_set_timeout` does; it must neither overflow into an immediate
/// timeout nor be shortened.
#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_sticky_timeout_of_long_max_seconds_waits_for_the_data() {
    let run = run_and_check("longtv", &waiter_line("longtv", QUIET));
    assert_reissued_through_the_synch(&run);
    assert!(run.reissue().unwrap().sticky);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_worker_forks_while_the_first_thread_joins_and_another_waits() {
    let run = run_and_check("three", &waiter_line("three", QUIET));
    assert_reissued_through_the_synch(&run);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_fork_child_forks_again_from_a_new_thread() {
    let run = run_and_check("nested", &waiter_line("nested", QUIET));
    assert_reissued_through_the_synch(&run);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_fork_from_a_signal_handler_while_another_thread_waits() {
    let run = run_and_check("handler", &waiter_line("handler", QUIET));
    assert_reissued_through_the_synch(&run);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn clone_without_clone_vm_while_another_thread_waits() {
    let run = run_and_check("clone", &waiter_line("clone", QUIET));
    assert_reissued_through_the_synch(&run);
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn clone3_without_clone_vm_while_another_thread_waits() {
    let run = run_and_check("clone3", &waiter_line("clone3", QUIET));
    assert_reissued_through_the_synch(&run);
}

/// A thread parked in an injected wait when another thread calls `exit` must
/// not hold the exit: the re-issue stops while DynamoRIO is terminating
/// threads, so the thread leaves the wait. DynamoRIO then terminates it and
/// runs its exit from the exiting thread, which the runtime accepts only in
/// this documented case. Before, the exit synch retried for about 50 s, and a
/// blanket refusal of non-current exits made the run fail with status 101.
#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn exit_with_a_waiter_parked_in_an_injected_wait_exits_each_thread_once() {
    let expected = "exiting with the waiter parked\n";
    let run = run("exitpark", &[]);
    assert_eq!(String::from_utf8_lossy(&run.native.stdout), expected);
    assert!(run.dbt.status.success(), "DBT run failed: {:?}", run.dbt);
    assert_eq!(String::from_utf8_lossy(&run.dbt.stdout), expected);
    let stderr = run.stderr();
    assert!(!stderr.contains("refused"), "{stderr}");
    assert!(
        run.reissue()
            .is_none_or(|reissue| reissue.result == -4 && !reissue.eligible),
        "{stderr}"
    );
    run.assert_each_thread_exited_once(2);
    // No synch gave up on the waiter: that takes about 50 s.
    assert!(
        run.dbt_elapsed < std::time::Duration::from_secs(20),
        "the exit took {:?}: {stderr}",
        run.dbt_elapsed
    );
}

/// A code-cache reset's synch gives up on the waiting thread and takes
/// DynamoRIO's abort path, which must release the thread exactly once.
#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_cache_reset_that_aborts_its_synch_releases_the_waiter_once() {
    let expected = format!("waiter mode=reset {QUIET}\n");
    let run = run("reset", &["-reset_at_nth_thread", "3"]);
    assert_eq!(String::from_utf8_lossy(&run.native.stdout), expected);
    assert!(run.dbt.status.success(), "DBT run failed: {:?}", run.dbt);
    assert_eq!(String::from_utf8_lossy(&run.dbt.stdout), expected);
    let stderr = run.stderr();
    assert!(
        stderr.contains("reverie-dbt-test: synch abort released a notified thread"),
        "the abort path was not reached: {stderr}"
    );
    assert!(
        !stderr.contains("pending_synch_count below zero"),
        "the abort path released the waiter twice: {stderr}"
    );
}
