/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Live copied-child protected-evidence regression coverage.
//!
//! Run after `scripts/build-client.sh` with:
//!
//! ```text
//! DYNAMORIO_HOME=<...> REVERIE_DBT_CLIENT=<...>/libreverie_dbt_client.so \
//!   cargo test -p reverie-dbt --test evidence_fork_live -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::io::Seek as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::Command;
use std::process::Output;

use reverie_dbt::Counter2Global;
use reverie_dbt::DbtRunner;

fn compile_fixture(source_name: &str, output: &Path, extra_arguments: &[&str]) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(source_name);
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let compile_status = Command::new(compiler)
        .args(["-O2", "-g", "-std=c11", "-Wall", "-Wextra", "-Werror"])
        .args(extra_arguments)
        .arg(source)
        .arg("-o")
        .arg(output)
        .status()
        .expect("compile evidence fixture");
    assert!(compile_status.success(), "fixture compilation failed");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
async fn protected_evidence_survives_fork_pthread_lifecycle() {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = directory.path().join("fork-pthread-identity");
    compile_fixture("fork_pthread_identity.c", &fixture, &["-pthread"]);

    let mut evidence_file = tempfile::tempfile().expect("evidence tempfile");
    let runner = DbtRunner::from_env()
        .expect("DYNAMORIO_HOME (or DynamoRIO_DIR) and REVERIE_DBT_CLIENT must be set")
        .evidence_file(&evidence_file)
        .expect("configure protected evidence")
        .client_argument("-test-wait-for-background");
    let mut guest = Command::new(fixture);
    guest.env("HERMIT_DBT_COUNTER2", "1");

    let (output, _global) = runner
        .output_with_global::<Counter2Global>(&guest, ())
        .await
        .expect("fork/pthread evidence run should complete");
    assert!(
        output.status.success(),
        "fork/pthread guest exited unsuccessfully: {output:?}"
    );
    assert_eq!(output.stdout, b"fork-pthread-race=64\n");

    evidence_file
        .seek(std::io::SeekFrom::Start(0))
        .expect("rewind evidence artifact");
    let mut evidence_bytes = Vec::new();
    evidence_file
        .read_to_end(&mut evidence_bytes)
        .expect("read evidence artifact");
    let evidence = reverie_dbt::decode_evidence(&evidence_bytes)
        .expect("fork/pthread evidence artifact must decode");
    assert!(
        evidence.initialization_records() > 0,
        "fork/pthread run must publish process image initialization evidence"
    );
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
fn protected_evidence_covers_vfork_open_and_exec() {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = directory.path().join("vfork-open-exec");
    compile_fixture("evidence_vfork_open_exec.c", &fixture, &[]);

    let mut evidence_file = tempfile::tempfile().expect("evidence tempfile");
    let runner = DbtRunner::from_env()
        .expect("DYNAMORIO_HOME (or DynamoRIO_DIR) and REVERIE_DBT_CLIENT must be set")
        .evidence_file(&evidence_file)
        .expect("configure protected evidence")
        .client_argument("-test-wait-for-background");
    let output = runner
        .output(&Command::new(fixture))
        .expect("vfork/open/exec evidence run should complete");
    assert!(
        output.status.success(),
        "vfork/open/exec guest exited unsuccessfully: {output:?}"
    );
    assert_eq!(output.stdout, b"vfork-open-exec-ok\n");

    evidence_file
        .seek(std::io::SeekFrom::Start(0))
        .expect("rewind evidence artifact");
    let mut evidence_bytes = Vec::new();
    evidence_file
        .read_to_end(&mut evidence_bytes)
        .expect("read evidence artifact");
    let evidence = reverie_dbt::decode_evidence(&evidence_bytes)
        .expect("vfork/open/exec evidence artifact must decode");
    let initialized_images = evidence.initialization_records();
    assert!(
        initialized_images >= 2,
        "parent and exec child must both contribute protected evidence; got {initialized_images} initialization records"
    );
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
fn protected_evidence_follows_last_thread_explicit_exit() {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = directory.path().join("explicit-exit");
    compile_fixture("evidence_explicit_exit.c", &fixture, &[]);

    let mut evidence_file = tempfile::tempfile().expect("evidence tempfile");
    let runner = DbtRunner::from_env()
        .expect("DYNAMORIO_HOME (or DynamoRIO_DIR) and REVERIE_DBT_CLIENT must be set")
        .evidence_file(&evidence_file)
        .expect("configure protected evidence")
        .client_argument("-test-thread-exit-evidence");
    let output = runner
        .output(&Command::new(fixture))
        .expect("explicit SYS_exit evidence run should complete");
    assert!(
        output.status.success(),
        "explicit SYS_exit guest exited unsuccessfully: {output:?}"
    );

    evidence_file
        .seek(std::io::SeekFrom::Start(0))
        .expect("rewind evidence artifact");
    let mut evidence_bytes = Vec::new();
    evidence_file
        .read_to_end(&mut evidence_bytes)
        .expect("read evidence artifact");
    let evidence = reverie_dbt::decode_evidence(&evidence_bytes)
        .expect("explicit SYS_exit evidence artifact must decode");
    assert!(
        evidence.records().iter().any(|record| {
            record
                .windows(b"explicit SYS_exit thread callback completed".len())
                .any(|window| window == b"explicit SYS_exit thread callback completed")
        }),
        "last-thread SYS_exit must publish thread-exit evidence before FINAL"
    );
}

/// DynamoRIO runs the client's background thread as a separate process, and
/// that process publishes its own image initialization record.
const CLIENT_BACKGROUND_IMAGES: usize = 1;

fn compile_self_sigkill_fixture(directory: &Path) -> std::path::PathBuf {
    let fixture = directory.join("self-sigkill");
    compile_fixture("evidence_self_sigkill.c", &fixture, &[]);
    fixture
}

/// Runs one mode of the self-SIGKILL fixture under protected evidence and
/// returns the runner's result with the evidence artifact's bytes.
fn try_run_self_sigkill_fixture(mode: &str) -> (std::io::Result<Output>, Vec<u8>) {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = compile_self_sigkill_fixture(directory.path());

    let mut evidence_file = tempfile::tempfile().expect("evidence tempfile");
    let runner = DbtRunner::from_env()
        .expect("DYNAMORIO_HOME (or DynamoRIO_DIR) and REVERIE_DBT_CLIENT must be set")
        .evidence_file(&evidence_file)
        .expect("configure protected evidence")
        .client_argument("-test-wait-for-background");
    let mut guest = Command::new(fixture);
    guest.arg(mode);
    let result = runner.output(&guest);

    evidence_file
        .seek(std::io::SeekFrom::Start(0))
        .expect("rewind evidence artifact");
    let mut evidence_bytes = Vec::new();
    evidence_file
        .read_to_end(&mut evidence_bytes)
        .expect("read evidence artifact");
    (result, evidence_bytes)
}

/// Runs one mode of the self-SIGKILL fixture under protected evidence.
///
/// DynamoRIO runs no exit callback for SIGKILL. An image that kills itself
/// without first ending its evidence stream leaves the collector without that
/// image's FINAL frame, so the collector refuses the whole artifact and
/// `output` returns the refusal as an error.
fn run_self_sigkill_fixture(mode: &str) -> (Output, reverie_dbt::DbtEvidence) {
    let (result, evidence_bytes) = try_run_self_sigkill_fixture(mode);
    let output =
        result.unwrap_or_else(|error| panic!("self-SIGKILL {mode} evidence run failed: {error}"));
    let evidence = reverie_dbt::decode_evidence(&evidence_bytes).unwrap_or_else(|error| {
        panic!("self-SIGKILL {mode} evidence artifact must decode: {error}")
    });
    (output, evidence)
}

fn assert_child_self_sigkill_evidence(mode: &str) {
    let (output, evidence) = run_self_sigkill_fixture(mode);
    assert!(
        output.status.success(),
        "self-SIGKILL {mode} guest exited unsuccessfully: {output:?}"
    );
    assert_eq!(
        output.stdout,
        format!("self-sigkill-{mode}-ok\n").into_bytes(),
        "the parent must observe its {mode} child killed by SIGKILL"
    );
    assert_eq!(
        evidence.initialization_records(),
        CLIENT_BACKGROUND_IMAGES + 2,
        "the root and its {mode} child must each publish one image initialization"
    );
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
fn protected_evidence_finishes_a_forked_child_that_sigkills_itself() {
    assert_child_self_sigkill_evidence("fork");
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
fn protected_evidence_finishes_a_vfork_child_that_sigkills_itself() {
    assert_child_self_sigkill_evidence("vfork");
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
fn protected_evidence_finishes_a_root_that_sigkills_itself() {
    let (output, evidence) = run_self_sigkill_fixture("root");
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGKILL),
        "the root must die from its own SIGKILL: {output:?}"
    );
    assert!(
        output.stdout.is_empty(),
        "the root wrote output after its own SIGKILL: {output:?}"
    );
    assert_eq!(
        evidence.initialization_records(),
        CLIENT_BACKGROUND_IMAGES + 1
    );
}

/// A SIGKILL to another process is not self-directed. Here the target is an
/// exited, unreaped child, so the kill succeeds and the root must run on, exit
/// normally and publish.
#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
fn protected_evidence_continues_after_a_sigkill_to_a_zombie_child() {
    let (output, evidence) = run_self_sigkill_fixture("zombie-child");
    assert!(
        output.status.success(),
        "zombie-child guest exited unsuccessfully: {output:?}"
    );
    assert_eq!(output.stdout, b"self-sigkill-zombie-child-ok\n");
    assert_eq!(
        evidence.initialization_records(),
        CLIENT_BACKGROUND_IMAGES + 2
    );
}

/// The client ends the root's evidence stream before executing its
/// self-directed SIGKILL. When a seccomp filter makes that kill return, the
/// root would run on after its FINAL frame, so the client ends the run as a
/// backend failure and the collector must refuse publication.
#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
fn protected_evidence_refuses_a_root_whose_self_sigkill_returns() {
    let (result, evidence_bytes) = try_run_self_sigkill_fixture("root-returned");
    let error = result.expect_err("a returned self-directed SIGKILL must refuse publication");
    let message = error.to_string();
    assert!(
        message.contains("exited with status Some(101)")
            && message.contains(
                "DBT evidence client reported an error: backend failure after this \
                 process image's FINAL frame"
            ),
        "unexpected refusal: {message}"
    );
    assert!(
        evidence_bytes.is_empty(),
        "a refused run must leave the evidence artifact empty"
    );
}
