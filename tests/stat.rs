/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// reinject stat* as fstatat unittest

#[cfg(all(not(sanitized), test))]
#[path = "tool_fixtures/stat.rs"]
mod stat_tool;

#[cfg(all(not(sanitized), test))]
use stat_tool::LocalState;

#[cfg(all(not(sanitized), test))]
#[path = "support/stat_rust_guest.rs"]
mod rust_guest;

#[cfg(all(not(sanitized), test))]
mod tests {
    use reverie_ptrace::testing::check_fn;

    use super::*;

    #[test]
    fn stat_can_be_reinjected() {
        check_fn::<LocalState, _>(rust_guest::stat_can_be_reinjected);
        #[cfg(feature = "dbt")]
        assert_rust_command("stat");
    }

    #[test]
    fn statx_fstat_returns_same_ino() {
        check_fn::<LocalState, _>(rust_guest::statx_fstat_returns_same_ino);
        #[cfg(feature = "dbt")]
        assert_rust_command("statx");
    }
}

#[cfg(all(not(sanitized), test, feature = "dbt"))]
fn assert_rust_command(operation: &str) {
    let manifest = caller_manifest();
    reverie_dbt::CallerArtifact::load(&manifest).unwrap();
    let program = env!("CARGO_BIN_EXE_stat-rust-command-guest");
    eprintln!("stat Rust command {operation}: image={program}");
    let output = std::process::Command::new(program)
        .arg(operation)
        .output()
        .expect("run native Rust stat fixture");
    eprintln!(
        "stat Rust command {operation}/native: status={:?} stdout={:02x?} stderr={:02x?}",
        output.status, output.stdout, output.stderr,
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let (output, ()) = reverie_test_support::test_cmd::<LocalState>(program, &[operation]).unwrap();
    assert_command_output(&format!("rust/{operation}/ptrace"), &output);
    let (output, ()) = reverie_test_support::test_dbt_cmd_with_config::<LocalState>(
        program,
        &[operation],
        (),
        &manifest,
    )
    .unwrap();
    assert_command_output(&format!("rust/{operation}/dbt"), &output);
}

#[cfg(all(not(sanitized), test))]
#[path = "support/stat_command.rs"]
mod command;

#[cfg(all(not(sanitized), test))]
fn assert_command_output(label: &str, output: &reverie::process::Output) {
    eprintln!(
        "stat command {label}: status={:?} stdout={:02x?} stderr={:02x?}",
        output.status, output.stdout, output.stderr,
    );
    assert_eq!(output.status, reverie::ExitStatus::Exited(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[cfg(all(not(sanitized), test))]
#[test]
fn ptrace_stat_command() {
    let guest = command::GuestBinary::compile(command::Operation::Stat);
    guest.assert_native();
    guest.record_before_launch("ptrace");
    let (output, ()) = reverie_test_support::test_cmd::<LocalState>(guest.program(), &[]).unwrap();
    assert_command_output("stat/ptrace", &output);
}

#[cfg(all(not(sanitized), test))]
#[test]
fn ptrace_statx_command() {
    let guest = command::GuestBinary::compile(command::Operation::Statx);
    guest.assert_native();
    guest.record_before_launch("ptrace");
    let (output, ()) = reverie_test_support::test_cmd::<LocalState>(guest.program(), &[]).unwrap();
    assert_command_output("statx/ptrace", &output);
}

#[cfg(all(not(sanitized), test, feature = "dbt"))]
fn caller_manifest() -> std::path::PathBuf {
    let executable = std::env::current_exe().unwrap();
    executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("reverie-dbt-callers")
        .join("stat")
        .join("manifest.json")
}

#[cfg(all(not(sanitized), test, feature = "dbt"))]
#[test]
fn dbt_stat_command() {
    let manifest = caller_manifest();
    reverie_dbt::CallerArtifact::load(&manifest).unwrap();
    let guest = command::GuestBinary::compile(command::Operation::Stat);
    guest.assert_native();
    guest.record_before_launch("dbt");
    let (output, ()) = reverie_test_support::test_dbt_cmd_with_config::<LocalState>(
        guest.program(),
        &[],
        (),
        &manifest,
    )
    .unwrap();
    assert_command_output("stat/dbt", &output);
}

#[cfg(all(not(sanitized), test, feature = "dbt"))]
#[test]
fn dbt_statx_command() {
    let manifest = caller_manifest();
    reverie_dbt::CallerArtifact::load(&manifest).unwrap();
    let guest = command::GuestBinary::compile(command::Operation::Statx);
    guest.assert_native();
    guest.record_before_launch("dbt");
    let (output, ()) = reverie_test_support::test_dbt_cmd_with_config::<LocalState>(
        guest.program(),
        &[],
        (),
        &manifest,
    )
    .unwrap();
    assert_command_output("statx/dbt", &output);
}

#[cfg(all(not(sanitized), test, feature = "dbt"))]
#[test]
fn dbt_stat_missing_artifact_refuses() {
    let manifest = caller_manifest();
    let _valid_default = reverie_dbt::CallerArtifact::load(&manifest).unwrap();
    let missing = manifest.with_file_name("missing-stat-manifest.json");
    assert!(!missing.exists());
    let error = reverie_test_support::test_dbt_cmd_with_config::<LocalState>(
        "/bin/true",
        &[],
        (),
        &missing,
    )
    .unwrap_err();
    eprintln!(
        "explicit missing stat artifact {}: {error}",
        missing.display()
    );
    let reverie::Error::Io(error) = error else {
        panic!("expected missing manifest I/O error: {error}");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
