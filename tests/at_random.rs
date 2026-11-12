/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#![cfg(all(not(sanitized), test))]

#[path = "tool_fixtures/at_random.rs"]
mod at_random_tool;
#[path = "support/at_random_command.rs"]
mod command;

use at_random_tool::PRNG_SEED;
use at_random_tool::TestTool;

fn assert_command_output(label: &str, output: &reverie::process::Output) {
    eprintln!(
        "at-random command {label}: status={:?} stdout={:02x?} stderr={:02x?}",
        output.status, output.stdout, output.stderr,
    );
    assert_eq!(output.status, reverie::ExitStatus::Exited(0));
    assert_eq!(output.stdout.len(), PRNG_SEED.len());
    assert!(output.stderr.is_empty());
    assert_eq!(output.stdout, PRNG_SEED);
}

#[test]
fn ptrace_at_random_command() {
    let guest = command::GuestBinary::compile();
    guest.assert_native();
    guest.record_before_launch("ptrace");
    let (output, _state): (_, ()) =
        reverie_test_support::test_cmd_with_config::<TestTool>(guest.program(), &[], ()).unwrap();
    assert_command_output("ptrace", &output);
}

#[cfg(feature = "dbt")]
fn caller_manifest() -> std::path::PathBuf {
    let executable = std::env::current_exe().unwrap();
    executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("reverie-dbt-callers")
        .join("at-random")
        .join("manifest.json")
}

#[cfg(feature = "dbt")]
fn assert_rust_command_output(label: &str, output: &reverie::process::Output) {
    eprintln!(
        "at-random Rust command {label}: status={:?} stdout={:02x?} stderr={:02x?}",
        output.status, output.stdout, output.stderr,
    );
    assert_eq!(output.status, reverie::ExitStatus::Exited(0));
    assert_eq!(
        output.stdout,
        b"Running in guest mode (actual test).\nEntropy (intercepted) at at_random [12, 34, 56, 78, de, ad, be, ef, ca, fe, ca, fe, 87, 65, 43, 21]\n"
    );
    assert!(output.stderr.is_empty());
}

#[cfg(feature = "dbt")]
fn rust_command_test(manifest: &std::path::Path) {
    let program = env!("CARGO_BIN_EXE_at-random-rust-command-guest");
    eprintln!("at-random Rust guest: image={program}");
    let (output, ()): (_, ()) =
        reverie_test_support::test_cmd_with_config::<TestTool>(program, &[], ()).unwrap();
    assert_rust_command_output("ptrace", &output);
    let (output, ()): (_, ()) =
        reverie_test_support::test_dbt_cmd_with_config::<TestTool>(program, &[], (), manifest)
            .unwrap();
    assert_rust_command_output("dbt", &output);
}

#[cfg(feature = "dbt")]
#[test]
fn dbt_at_random_command() {
    let manifest = caller_manifest();
    reverie_dbt::CallerArtifact::load(&manifest).unwrap();
    let guest = command::GuestBinary::compile();
    guest.assert_native();
    guest.record_before_launch("dbt");
    let (output, _state): (_, ()) = reverie_test_support::test_dbt_cmd_with_config::<TestTool>(
        guest.program(),
        &[],
        (),
        &manifest,
    )
    .unwrap();
    assert_command_output("dbt", &output);
    rust_command_test(&manifest);
}

#[cfg(feature = "dbt")]
#[test]
fn dbt_at_random_missing_artifact_refuses() {
    let manifest = caller_manifest();
    let _valid_default = reverie_dbt::CallerArtifact::load(&manifest).unwrap();
    let missing = manifest.with_file_name("missing-at-random-manifest.json");
    assert!(!missing.exists());
    let error =
        reverie_test_support::test_dbt_cmd_with_config::<TestTool>("/bin/true", &[], (), &missing)
            .unwrap_err();
    eprintln!(
        "explicit missing at-random artifact {}: {error}",
        missing.display()
    );
    let reverie::Error::Io(error) = error else {
        panic!("expected missing manifest I/O error: {error}");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
