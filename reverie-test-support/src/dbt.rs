// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.

//! Runs the same caller-owned Tool using an explicitly supplied native client.

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;

use reverie::Error;
use reverie::ExitStatus;
use reverie::Tool;
use reverie::process::Output;

/// The test caller binds its Tool type to the native client that exports it.
/// This is artifact selection, not a runtime Tool registry or backend fallback.
pub trait DbtTestTool: Tool + 'static {
    /// Environment key containing the matching native client path.
    const CLIENT_ENV: &'static str;
    /// Whether this fixture intentionally sends no application request.
    const ZERO_APPLICATION_RPC: bool = false;
}

/// Run a real DBT command with the selected Tool and its default configuration.
/// Missing or incompatible artifacts are errors; no alternate backend runs.
pub fn test_cmd<T: DbtTestTool>(
    program: &str,
    args: &[&str],
) -> Result<(Output, T::GlobalState), Error> {
    test_cmd_with_config::<T>(program, args, Default::default())
}

/// Run a real DBT command using the caller's actual GlobalTool configuration.
pub fn test_cmd_with_config<T: DbtTestTool>(
    program: &str,
    args: &[&str],
    config: <T::GlobalState as reverie::GlobalTool>::Config,
) -> Result<(Output, T::GlobalState), Error> {
    let client = std::env::var_os(T::CLIENT_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| {
            std::io::Error::other(format!(
                "{} must name the native client linked to this Tool",
                T::CLIENT_ENV
            ))
        })?;
    let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let runner = reverie_dbt::DbtRunner::new(reverie_dbt::bundled_drrun_path(), client)?
        .require_initial_root_ready()
        .observe_application_requests(std::sync::Arc::clone(&observed));
    let mut command = std::process::Command::new(program);
    command.args(args);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (output, global) =
        runtime.block_on(runner.output_with_global::<T::GlobalState>(&command, config))?;
    let requested = observed.load(std::sync::atomic::Ordering::Acquire);
    eprintln!("DBT caller command {program:?} {args:?}: application_request_observed={requested}");
    if T::ZERO_APPLICATION_RPC {
        assert!(!requested, "zero-RPC Tool sent an application request");
    }
    Ok((
        Output {
            status: ExitStatus::from_raw(output.status.into_raw()),
            stdout: output.stdout,
            stderr: output.stderr,
        },
        global,
    ))
}
