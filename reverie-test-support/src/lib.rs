/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Command helpers for generic Reverie conformance tests.
//!
//! Default helpers use ptrace. The optional DBT helper requires an explicit
//! artifact containing the caller's actual Tool and returns its real global
//! state. There is no backend selector or implicit fallback here.
//! Inherited function guests continue to use `reverie_ptrace::testing` directly.

use reverie::Error;
use reverie::GlobalTool;
use reverie::Tool;
use reverie::process::Output;

/// Runs a ptrace command with the existing output capture and runtime setup.
pub fn test_cmd_with_config<T>(
    program: &str,
    args: &[&str],
    config: <T::GlobalState as GlobalTool>::Config,
) -> Result<(Output, T::GlobalState), Error>
where
    T: Tool + 'static,
{
    reverie_ptrace::testing::test_cmd_with_config::<T>(program, args, config)
}

/// Runs a ptrace command with the caller's default Tool configuration.
pub fn test_cmd<T>(program: &str, args: &[&str]) -> Result<(Output, T::GlobalState), Error>
where
    T: Tool + 'static,
{
    test_cmd_with_config::<T>(program, args, Default::default())
}

/// Runs a DBT command with an explicit paired artifact and the real global state.
///
/// The selected manifest must contain the caller's `T`; this helper does not
/// construct or substitute a Tool. Stdin, cwd and the application environment
/// are inherited, while stdout and stderr are captured. The caller protocol's
/// initial-process restrictions still apply, and signal termination is a
/// launch error. Missing or invalid artifacts are errors without a fallback.
#[cfg(feature = "dbt")]
pub fn test_dbt_cmd_with_config<T>(
    program: &str,
    args: &[&str],
    config: <T::GlobalState as GlobalTool>::Config,
    caller_manifest: &std::path::Path,
) -> Result<(Output, T::GlobalState), Error>
where
    T: Tool + 'static,
{
    use std::os::unix::process::ExitStatusExt;

    let artifact = reverie_dbt::CallerArtifact::load(caller_manifest)?;
    let runner =
        reverie_dbt::DbtRunner::new(reverie_dbt::bundled_drrun_path(), artifact.native_client())?;
    let mut command = std::process::Command::new(program);
    command.args(args);
    let (output, global) = reverie_ptrace::testing::run_tokio_test(
        runner.output_with_inherited_stdin_and_caller_tool::<T::GlobalState>(&command, config),
    )?;
    Ok((
        Output {
            status: reverie::ExitStatus::from_raw(output.status.into_raw()),
            stdout: output.stdout,
            stderr: output.stderr,
        },
        global,
    ))
}
