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
