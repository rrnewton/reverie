/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Utilities that support constructing tests for Reverie Tools.

#[cfg(test)]
mod fixtures;
#[cfg(test)]
pub(crate) use fixtures::fixture_path;
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

/// Retains the notifier generation used by a native newborn EXIT-stop fixture.
/// It cannot construct a wait result or resume/consume a ptrace stop.
#[must_use]
pub struct NewbornExitStop {
    child: reverie::Pid,
    cleanup: safeptrace::TerminalCleanup,
}

impl NewbornExitStop {
    /// The original child from the caller's still-owned NewChild event.
    pub fn child(&self) -> reverie::Pid {
        self.child
    }

    /// Waits for this original notifier worker's terminal acknowledgment.
    /// The test must independently require the actual backend final-wait callback;
    /// worker completion alone is not proof of a consumed native terminal status.
    pub fn worker_drained(&self, timeout: std::time::Duration) -> bool {
        self.cleanup.wait(timeout)
    }
}

/// Kill an actual newborn process and observe its notifier's EXIT-stop
/// publication without consuming or resuming that stop.
///
/// This test helper does not wait for pidfd terminal readiness: the tracer
/// still needs to resume the EXIT stop before the kernel can exit. It retains
/// the same notifier identity which ordinary dispatch will adopt, so no second
/// wait owner is introduced. Errors are failed fixture setup, never a native
/// child result. The outside owner must still prove the complete actor drain.
///
/// # Safety
///
/// The caller must be inside the synchronous Tool ChildCreated observation for
/// this exact child, with the original NewChild ptrace event still owned and
/// neither child nor creator resumed or reaped. This is the identity fence for
/// initial capture: a numeric PID obtained elsewhere is not sufficient. The
/// caller must fail the tracer if this helper errors, preserving its ordinary
/// EXITKILL and outside command-owner cleanup; it must not resume the fixture.
pub unsafe fn kill_newborn_process_at_exit_stop(
    child: reverie::Pid,
    timeout: std::time::Duration,
) -> Result<NewbornExitStop, Error> {
    let deadline = std::time::Instant::now() + timeout;
    let running = safeptrace::Running::new(child);
    let cleanup = running.terminal_cleanup();
    loop {
        match cleanup.ensure_registered() {
            Err(reverie::Errno::EINTR) if std::time::Instant::now() < deadline => continue,
            result => {
                result?;
                break;
            }
        }
    }
    if cleanup.thread_group_id()? != child || cleanup.exit_stop_observed() {
        return Err(anyhow::anyhow!("fixture child is not an original pre-exit process").into());
    }
    if std::time::Instant::now() >= deadline {
        return Err(anyhow::anyhow!("newborn binding exceeded original fixture deadline").into());
    }
    cleanup.terminate_bound_task()?;
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(
                anyhow::anyhow!("actual newborn EXIT-stop was not published in time").into(),
            );
        }
        if cleanup.exit_stop_observed() {
            return Ok(NewbornExitStop { child, cleanup });
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
