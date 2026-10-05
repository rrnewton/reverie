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

/// The detail of the ERROR frame that the client sends when an image's backend
/// fails after that image's FINAL frame, as a returned self-directed SIGKILL
/// does.
const ERROR_AFTER_FINAL_DETAIL: &str = "backend failure after this process image's FINAL frame";

/// The record that `-test-thread-exit-evidence` adds when a thread exits.
const THREAD_EXIT_RECORD: &[u8] = b"explicit SYS_exit thread callback completed";

fn compile_self_sigkill_fixture(directory: &Path) -> std::path::PathBuf {
    let fixture = directory.join("self-sigkill");
    compile_fixture("evidence_self_sigkill.c", &fixture, &["-pthread"]);
    fixture
}

/// What one run of the self-SIGKILL fixture produced.
struct SelfSigkillRun {
    result: std::io::Result<Output>,
    evidence_bytes: Vec<u8>,
    diagnostics: String,
}

fn read_from_start(file: &mut std::fs::File, what: &str) -> Vec<u8> {
    file.seek(std::io::SeekFrom::Start(0))
        .unwrap_or_else(|error| panic!("rewind {what}: {error}"));
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .unwrap_or_else(|error| panic!("read {what}: {error}"));
    bytes
}

/// Runs the self-SIGKILL fixture with `guest_arguments` under protected
/// evidence, passing `client_arguments` to the native client as well.
fn try_run_self_sigkill_fixture(
    guest_arguments: &[&str],
    client_arguments: &[&str],
) -> SelfSigkillRun {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = compile_self_sigkill_fixture(directory.path());

    let mut evidence_file = tempfile::tempfile().expect("evidence tempfile");
    let mut diagnostic_file = tempfile::tempfile().expect("diagnostic tempfile");
    let mut runner = DbtRunner::from_env()
        .expect("DYNAMORIO_HOME (or DynamoRIO_DIR) and REVERIE_DBT_CLIENT must be set")
        .evidence_file(&evidence_file)
        .expect("configure protected evidence")
        .diagnostic_file(&diagnostic_file)
        .expect("configure client diagnostics")
        .client_argument("-test-wait-for-background");
    for argument in client_arguments {
        runner = runner.client_argument(argument);
    }
    let mut guest = Command::new(fixture);
    guest.args(guest_arguments);
    let result = runner.output(&guest);

    SelfSigkillRun {
        result,
        evidence_bytes: read_from_start(&mut evidence_file, "evidence artifact"),
        diagnostics: String::from_utf8_lossy(&read_from_start(
            &mut diagnostic_file,
            "client diagnostics",
        ))
        .into_owned(),
    }
}

/// Runs the self-SIGKILL fixture under protected evidence and requires the run
/// to publish.
///
/// DynamoRIO runs no exit callback for SIGKILL. An image that kills itself
/// without first ending its evidence stream leaves the collector without that
/// image's FINAL frame, so the collector refuses the whole artifact and
/// `output` returns the refusal as an error.
fn run_self_sigkill_fixture_with(
    guest_arguments: &[&str],
    client_arguments: &[&str],
) -> (Output, reverie_dbt::DbtEvidence) {
    let run = try_run_self_sigkill_fixture(guest_arguments, client_arguments);
    let output = run.result.unwrap_or_else(|error| {
        panic!(
            "self-SIGKILL {guest_arguments:?} evidence run failed: {error}\n\
             client diagnostics:\n{}",
            run.diagnostics
        )
    });
    let evidence = reverie_dbt::decode_evidence(&run.evidence_bytes).unwrap_or_else(|error| {
        panic!("self-SIGKILL {guest_arguments:?} evidence artifact must decode: {error}")
    });
    (output, evidence)
}

fn run_self_sigkill_fixture(mode: &str) -> (Output, reverie_dbt::DbtEvidence) {
    run_self_sigkill_fixture_with(&[mode], &[])
}

/// Requires the run to be refused, with nothing published, and returns the
/// refusal's message.
fn refusal_message(run: &SelfSigkillRun) -> String {
    let message = match &run.result {
        Ok(output) => panic!(
            "the run published although it must be refused: {output:?}\n\
             client diagnostics:\n{}",
            run.diagnostics
        ),
        Err(error) => error.to_string(),
    };
    assert!(
        run.evidence_bytes.is_empty(),
        "a refused run must leave the evidence artifact empty: {message}"
    );
    message
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

/// The root first creates and joins a thread, whose exit record must still be
/// published before the root's FINAL frame. The kill then waits until the
/// kernel no longer counts that thread, and ends the stream as in "root".
#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
fn protected_evidence_finishes_a_root_that_sigkills_itself_after_joining_a_thread() {
    let (output, evidence) =
        run_self_sigkill_fixture_with(&["root-after-thread"], &["-test-thread-exit-evidence"]);
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGKILL),
        "the root must die from its own SIGKILL: {output:?}"
    );
    assert_eq!(
        evidence.initialization_records(),
        CLIENT_BACKGROUND_IMAGES + 1
    );
    assert!(
        evidence.records().iter().any(|record| record
            .windows(THREAD_EXIT_RECORD.len())
            .any(|window| window == THREAD_EXIT_RECORD)),
        "the joined thread's exit record must be published"
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
    let run = try_run_self_sigkill_fixture(&["root-returned"], &[]);
    let message = refusal_message(&run);
    assert!(
        message.contains("exited with status Some(101)")
            && message.contains(&format!(
                "DBT evidence client reported an error: {ERROR_AFTER_FINAL_DETAIL}"
            )),
        "unexpected refusal: {message}"
    );
}

/// Requires a returned self-directed SIGKILL to be refused by the launcher's
/// check of how the process ended, and not by the client's ERROR frame, which
/// `run` must not have delivered.
fn assert_refused_by_exit_confirmation(run: &SelfSigkillRun) {
    let message = refusal_message(run);
    assert!(
        message.contains("exited with status Some(101)")
            && message.contains(
                "ended its stream for a self-directed SIGKILL, but it exited with status 101"
            ),
        "unexpected refusal: {message}"
    );
    assert!(
        !message.contains("reported an error"),
        "the ERROR frame was delivered, so this run does not test its loss: {message}"
    );
    assert!(
        run.diagnostics
            .contains("self-directed SIGKILL returned -1 after protected evidence was finalized"),
        "the client did not report the returned kill: {}",
        run.diagnostics
    );
}

/// As in "root-returned", but a second seccomp filter fails every write of the
/// ERROR frame payload's length, so the payload never reaches the collector.
#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
fn protected_evidence_refuses_a_returned_self_sigkill_whose_error_frame_is_lost() {
    let length = ERROR_AFTER_FINAL_DETAIL.len().to_string();
    let run = try_run_self_sigkill_fixture(&["root-returned-error-denied", &length], &[]);
    assert_refused_by_exit_confirmation(&run);
}

/// As in "root-returned", but the client finds no sender for its ERROR frame
/// and so sends none.
#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
fn protected_evidence_refuses_a_returned_self_sigkill_without_an_error_sender() {
    let run = try_run_self_sigkill_fixture(
        &["root-returned"],
        &["-test-backend-failure-sender-lookup-miss"],
    );
    assert_refused_by_exit_confirmation(&run);
    assert!(
        run.diagnostics
            .contains("skipped backend failure sender lookup for testing"),
        "the client looked up its sender: {}",
        run.diagnostics
    );
}

/// While a sibling thread keeps creating threads whose exits add records, the
/// main thread sends the process SIGKILL. The client cannot stop the other
/// threads before a FINAL frame, so it sends none and the run must be refused.
#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
fn protected_evidence_refuses_a_self_sigkill_while_other_threads_run() {
    let run = try_run_self_sigkill_fixture(&["threaded"], &["-test-thread-exit-evidence"]);
    let message = refusal_message(&run);
    assert!(
        message.contains(
            "DBT guest exited with status None while protected evidence failed: \
             DBT evidence is missing FINAL frames for process images"
        ),
        "unexpected refusal: {message}"
    );
    assert!(
        run.diagnostics
            .contains("self-directed SIGKILL with other threads live"),
        "the client did not report the other threads: {}",
        run.diagnostics
    );
}
