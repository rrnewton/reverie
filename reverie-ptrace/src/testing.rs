/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Utilities that support constructing tests for Reverie Tools.

use futures::Future;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Tool;
use reverie::process::Command;
use reverie::process::Output;
use reverie::process::Stdio;

use crate::TracerBuilder;
pub use crate::perf::do_branches;
use crate::spawn_fn_with_config;

/// The number of late timer overflow signals this process has discarded at
/// injected syscalls. Concurrent tests in one process share the count.
pub fn late_timer_signals_discarded() -> u64 {
    crate::task::LATE_TIMER_SIGNALS_DISCARDED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The number of timer overflow signals this process has discarded while the
/// LiteInst patch helper ran. Concurrent tests in one process share the
/// count.
pub fn liteinst_helper_timer_signals_discarded() -> u64 {
    crate::task::LITEINST_HELPER_TIMER_SIGNALS_DISCARDED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The number of timer overflow signals this process has taken at injected
/// syscalls as the notification of a timer event that no stop had decided, to
/// deliver the event. Concurrent tests in one process share the count.
pub fn live_timer_signals_taken() -> u64 {
    crate::task::LIVE_TIMER_SIGNALS_TAKEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Makes the precise timers this process creates from now on map no overflow
/// records, as on a kernel that is or may be `PREEMPT_RT`. Timers already
/// created keep theirs. For tests of the behaviour without records; a test
/// binary that calls this should run nothing that expects records.
pub fn disable_timer_overflow_records() {
    crate::timer::OVERFLOW_RECORDS_DISABLED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The number of times this process forgot timer overflow records because
/// their notification had left the thread's pending queue. Concurrent tests
/// in one process share the count.
pub fn timer_overflow_records_expired() -> u64 {
    crate::timer::OVERFLOW_RECORDS_EXPIRED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The number of precise timer events this process has recorded as skid
/// overshoots because a stop other than their notification decided them
/// past their delivery point while the notification was already queued for
/// the thread: held back by the guest's signal mask, or, with the signal
/// unblocked, by an overflow interrupt so late that the stop's own signal
/// was queued too and dequeued first. A stop past the target whose event's
/// notification was lost, or cannot be told from a lost one (without
/// overflow records), is recorded as a skid overshoot but not counted here.
/// Concurrent tests in one process share the count.
pub fn precise_events_overtaken_with_notification_queued() -> u64 {
    crate::timer::OVERTAKEN_WITH_NOTIFICATION_QUEUED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Takes the events counted by
/// [`precise_events_overtaken_with_notification_queued`] since the last call,
/// as each one's clock target and the thread's clock at the stop that
/// overtook it, the first 1024 at most. Concurrent tests in one process
/// share the list.
pub fn take_precise_events_overtaken_with_notification_queued() -> Vec<(u64, u64)> {
    std::mem::take(
        &mut *crate::timer::OVERTAKEN_WITH_NOTIFICATION_QUEUED_EVENTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

/// The number of timer signals this process has discarded at their
/// signal-delivery stop because a stop before it had cancelled their event.
/// Concurrent tests in one process share the count.
pub fn cancelled_timer_signals_discarded() -> u64 {
    crate::timer::CANCELLED_TIMER_SIGNALS_DISCARDED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Checks that each of a run's precise timer events that a PMU notification
/// delivered fired at its target, `target` RCBs past its request, except
/// where Reverie witnessed a skid overshoot: `witnesses` is the change in
/// `reverie::take_skid_overshoot_count` over the run.
///
/// A precise event is delivered by single steps that start when its PMU
/// notification arrives, a skid margin before the target. The processor's
/// interrupt latency occasionally exceeds the margin, and then the guest has
/// passed the target when the notification arrives. Reverie delivers the
/// event late and counts it as a skid overshoot, and prints the
/// `HERMIT_SKID_OVERSHOOT` marker, by which Hermit refuses such a run as
/// nondeterministic.
///
/// So an event may fire past its target only if Reverie witnessed it, once:
/// the number of events past the target must equal `witnesses`. The caller
/// must also check how many events fired, since an event cancelled past its
/// target would be witnessed too. Never before the target. Prints the
/// overshoots of the late events, if any.
///
/// The count is process global, so the tests of a process that use this must
/// run one at a time (`--test-threads=1`).
pub fn assert_at_target_unless_witnessed(events: &[u64], target: u64, witnesses: u64) {
    assert!(
        events.iter().all(|&event| event >= target),
        "no event may fire before its target {target}: {events:?}"
    );
    let late = events.iter().filter(|&&event| event > target).count() as u64;
    assert_eq!(
        late, witnesses,
        "every event past its target {target}, and nothing else, must be a witnessed skid \
         overshoot: {events:?}"
    );
    if late > 0 {
        let overshoots: Vec<u64> = events
            .iter()
            .filter(|&&event| event > target)
            .map(|&event| event - target)
            .collect();
        eprintln!(
            "{late} of {} events fired past the target {target} with a witnessed skid \
             overshoot of {overshoots:?}",
            events.len()
        );
    }
}

/// The environment variable that tells a test binary re-run by
/// [`rerun_at_skid_margin`] which skid margin it was re-run at.
pub const RERUN_SKID_MARGIN_ENV: &str = "REVERIE_TEST_RERUN_SKID_MARGIN";

/// The skid margin at which [`rerun_at_skid_margin`] re-ran this test binary,
/// or `None` if it is not such a re-run. Checks that the processor's PMU
/// configuration uses that margin, so that a re-run whose override did not
/// take effect fails rather than repeating the parent's run.
pub fn rerun_skid_margin() -> Option<u64> {
    let margin: u64 = std::env::var(RERUN_SKID_MARGIN_ENV)
        .ok()?
        .parse()
        .expect("a re-run's skid margin must be a u64");
    assert_eq!(
        crate::PmuConfig::new().skid_margin(),
        margin,
        "the re-run's skid margin override must be in effect"
    );
    Some(margin)
}

/// Re-runs tests of the current test binary in a child process whose PMU uses
/// the skid margin `margin` (through `REVERIE_SKID_MARGIN_OVERRIDE`), one
/// test at a time, and returns the child's standard output and error. The
/// skid margin is fixed for a process when its first timer is created, so a
/// test of behaviour at another margin needs a process of its own.
///
/// `filter` is passed to the test harness as it is, to select the tests. The
/// child must pass exactly `expected_tests` tests, so that a filter that
/// selects nothing fails. A child still running after `timeout` is killed,
/// and the test fails.
pub fn rerun_at_skid_margin(
    filter: &[&str],
    margin: u64,
    expected_tests: usize,
    timeout: std::time::Duration,
) -> String {
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(filter)
        .args(["--test-threads=1", "--nocapture"])
        .env(crate::timer::SKID_MARGIN_OVERRIDE_ENV, margin.to_string())
        .env(RERUN_SKID_MARGIN_ENV, margin.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id() as libc::pid_t;
    let (sender, receiver) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    let output = match receiver.recv_timeout(timeout) {
        Ok(output) => output.unwrap(),
        Err(_) => {
            // The child is ours and not yet reaped, since the waiter has not
            // returned.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let _ = waiter.join();
            panic!("the re-run at skid margin {margin} did not finish within {timeout:?}");
        }
    };
    waiter.join().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "the re-run at skid margin {margin} failed ({}):\n{text}",
        output.status
    );
    let passed: Vec<usize> = text
        .lines()
        .filter_map(|line| line.strip_prefix("test result: ok. "))
        .filter_map(|rest| rest.split(' ').next()?.parse().ok())
        .collect();
    assert_eq!(
        passed,
        [expected_tests],
        "the re-run at skid margin {margin} must pass exactly {expected_tests} tests:\n{text}"
    );
    text
}

/// For some tests, its nice to show what was printed.
pub fn print_tracee_output(output: &Output) {
    println!(
        " >>> Tracee completed, {:?}, stdout len {}, stderr len {}",
        output.status,
        output.stdout.len(),
        output.stderr.len(),
    );
    if !output.stdout.is_empty() {
        println!(
            " >>> stdout:\n{}",
            std::str::from_utf8(&output.stdout)
                .expect("Reverie test helper operation should succeed")
        );
    }
    if !output.stderr.is_empty() {
        println!(
            " >>> stderr:\n{}",
            std::str::from_utf8(&output.stderr)
                .expect("Reverie test helper operation should succeed")
        );
    }
}

/// Configure tokio and tracing in the way that we like, and run the future.
pub fn run_tokio_test<F: Future>(fut: F) -> F::Output {
    let collector = tracing_subscriber::fmt()
        .with_env_filter("reverie=trace")
        .finish();

    // For reentrancy during testing we need to set up logging early because mio
    // will actually do some log chatter.

    // Here we ignore errors, because tests may be running in parallel, and we don't care who "wins".
    tracing::subscriber::set_global_default(collector).unwrap_or(());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_io()
        .enable_time()
        .worker_threads(2)
        .build()
        .expect("Reverie test helper operation should succeed");
    rt.block_on(async move {
        let local_set = tokio::task::LocalSet::new();
        local_set.run_until(fut).await
    })
}

/// Runs a command as a guest and returns its collected output and global state.
pub fn test_cmd_with_config<T>(
    program: &str,
    args: &[&str],
    config: <T::GlobalState as GlobalTool>::Config,
) -> Result<(Output, T::GlobalState), Error>
where
    T: Tool + 'static,
{
    let mut cmd = Command::new(program);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    run_tokio_test(async move {
        let tracer = TracerBuilder::<T>::new(cmd).config(config).spawn().await?;
        tracer.wait_with_output().await
    })
}

/// Runs a command as a guest and returns its collected output and global state.
pub fn test_cmd<T>(program: &str, args: &[&str]) -> Result<(Output, T::GlobalState), Error>
where
    T: Tool + 'static,
{
    test_cmd_with_config::<T>(program, args, Default::default())
}

/// Runs a function as a guest and returns its collected (stdout/err) output and global state.
pub fn test_fn_with_config<T, F>(
    f: F,
    config: <T::GlobalState as GlobalTool>::Config,
    capture_output: bool,
) -> Result<(Output, T::GlobalState), Error>
where
    T: Tool + 'static,
    F: FnOnce(),
{
    run_tokio_test(async move {
        let tracee = spawn_fn_with_config::<T, _>(f, config, capture_output).await?;
        tracee.wait_with_output().await
    })
}

/// Runs a function as a guest and returns its collected output and global state.
pub fn test_fn<T, F>(f: F) -> Result<(Output, T::GlobalState), Error>
where
    T: Tool + 'static,
    F: FnOnce(),
{
    test_fn_with_config::<T, F>(f, Default::default(), true)
}

/// Runs a function as a guest and returns its global state. Also checks that the
/// tracee exit code is 0.
pub fn check_fn_with_config<T, F>(
    f: F,
    config: <T::GlobalState as GlobalTool>::Config,
    capture_output: bool,
) -> T::GlobalState
where
    T: Tool + 'static,
    F: FnOnce(),
{
    let (output, state) = test_fn_with_config::<T, F>(f, config, capture_output)
        .expect("Reverie test helper operation should succeed");

    if output.status != ExitStatus::Exited(0) {
        print_tracee_output(&output);
        panic!("Got exit status {:?}", output.status);
    }

    state
}

/// Runs a function as a guest and returns its global state. Also checks that the
/// tracee exit code is 0.
pub fn check_fn<T, F>(f: F) -> T::GlobalState
where
    T: Tool + 'static,
    F: FnOnce(),
{
    check_fn_with_config::<T, F>(f, Default::default(), true)
}
