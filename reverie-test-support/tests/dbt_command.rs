/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

use std::io;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::process::Output;
use reverie_dbt::CallerArtifact;
use reverie_dbt::DbtRunner;

#[path = "../../tests/tool_fixtures/basics.rs"]
pub mod tools;

const INPUT: &[u8] = b"generic command stdin\n";
const STDIN_CHILD: &str = "REVERIE_TEST_SUPPORT_STDIN_CHILD";

fn manifest(label: &str) -> PathBuf {
    let executable = std::env::current_exe().unwrap();
    executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("reverie-dbt-callers")
        .join(label)
        .join("manifest.json")
}

fn artifact(label: &str) -> CallerArtifact {
    let path = manifest(label);
    CallerArtifact::load(&path).unwrap_or_else(|error| {
        panic!(
            "produce tests/fixtures/{label}-dbt-runtime/Cargo.toml with \
             reverie-dbt/scripts/build-client.sh before running this test: \
             {}: {error}",
            path.display()
        )
    })
}

fn run_null<G: GlobalTool + 'static>(
    artifact: &CallerArtifact,
    program: &str,
    args: &[&str],
) -> io::Result<(Output, G)> {
    let runner = DbtRunner::new(reverie_dbt::bundled_drrun_path(), artifact.native_client())?;
    let mut command = Command::new(program);
    command.args(args);
    let (output, global) = reverie_ptrace::testing::run_tokio_test(
        runner.output_with_caller_tool::<G>(&command, Default::default()),
    )?;
    // Caller signal termination remains a launch error, not a successful
    // signal-status result. Preserve the actual returned wait bits otherwise.
    Ok((
        Output {
            status: ExitStatus::from_raw(output.status.into_raw()),
            stdout: output.stdout,
            stderr: output.stderr,
        },
        global,
    ))
}

fn assert_counter_command(output: &Output, state: &tools::CounterGlobal) {
    // The original counter_tool_test oracle, including its strict threshold.
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(!output.stdout.is_empty());
    assert!(AtomicU64::load(&state.num_syscalls, Ordering::SeqCst) > 30);
}

fn assert_stdin(output: &Output, state: &tools::CounterGlobal) {
    let count = state.num_syscalls.load(Ordering::SeqCst);
    eprintln!(
        "stdin observation: status={:?}, syscalls={count}, stdout={:?}, stderr={:?}",
        output.status, output.stdout, output.stderr
    );
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(count > 0);
    assert_eq!(output.stdout.as_slice(), INPUT);
}

fn run_counter(
    label: &str,
    program: &str,
    args: &[&str],
) -> Result<(Output, tools::CounterGlobal), reverie::Error> {
    reverie_test_support::test_dbt_cmd_with_config::<tools::CounterLocal>(
        program,
        args,
        (),
        &manifest(label),
    )
}

#[test]
fn caller_command_preserves_inherited_stdin() {
    if let Some(mode) = std::env::var_os(STDIN_CHILD) {
        if mode == "inherit" {
            let (output, state) = run_counter("basics-counter", "/bin/cat", &[]).unwrap();
            assert_stdin(&output, &state);
        } else {
            assert_eq!(mode, "null");
            let (output, state) =
                run_null::<tools::CounterGlobal>(&artifact("basics-counter"), "/bin/cat", &[])
                    .unwrap();
            assert_eq!(output.status, ExitStatus::Exited(0));
            assert!(state.num_syscalls.load(Ordering::SeqCst) > 0);
            assert!(output.stdout.is_empty());
            assert!(std::panic::catch_unwind(|| assert_stdin(&output, &state)).is_err());
        }
        return;
    }

    // Each isolated test process gets its own known pipe on fd0. The null
    // control therefore still has unread input when it launches its guest.
    for mode in ["inherit", "null"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "caller_command_preserves_inherited_stdin",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(STDIN_CHILD, mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(INPUT).unwrap();
        let output = child.wait_with_output().unwrap();
        io::stderr().write_all(&output.stderr).unwrap();
        assert!(
            output.status.success(),
            "isolated {mode} stdin oracle: {output:?}"
        );
    }
}

#[test]
fn caller_command_counter_oracle_requires_actual_count() {
    let (output, state) = run_counter("basics-counter", "ls", &[]).unwrap();
    eprintln!(
        "actual CounterLocal count: {}",
        state.num_syscalls.load(Ordering::SeqCst)
    );
    assert_counter_command(&output, &state);

    // This is the actual fixture Noop Tool, with its ordinary syscall handler
    // and no application RPCs. Both concrete Tools have the same unit Config.
    // A setup refusal must fail this test, not count as an oracle witness.
    let (output, state) = run_counter("basics-noop", "ls", &[]).unwrap();
    eprintln!(
        "actual NoopTool / CounterGlobal count: {}",
        state.num_syscalls.load(Ordering::SeqCst)
    );
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(!output.stdout.is_empty());
    assert_eq!(state.num_syscalls.load(Ordering::SeqCst), 0);
    assert!(std::panic::catch_unwind(|| assert_counter_command(&output, &state)).is_err());
}

#[test]
fn caller_command_missing_artifact_refuses() {
    // A valid default pair exists, so refusal cannot be mistaken for missing
    // producer prerequisites and must not silently select that default pair.
    let _valid_default = artifact("basics-counter");
    let missing = manifest("basics-counter").with_file_name("missing-manifest.json");
    assert!(!missing.exists());
    let error = reverie_test_support::test_dbt_cmd_with_config::<tools::CounterLocal>(
        "ls",
        &[],
        (),
        &missing,
    )
    .unwrap_err();
    eprintln!("explicit missing artifact {}: {error}", missing.display());
    let reverie::Error::Io(error) = error else {
        panic!("expected missing manifest I/O error: {error}");
    };
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}
