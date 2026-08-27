/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Live regressions for DBT thread virtual-identity startup.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

use reverie_dbt::DbtRunner;

fn compile_fixture(directory: &Path, name: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.c"));
    let output = directory.join(name);
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = Command::new(compiler)
        .args(["-O2", "-g", "-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .arg("-pthread")
        .arg("-o")
        .arg(&output)
        .status()
        .unwrap_or_else(|error| panic!("compile {}: {error}", source.display()));
    assert!(status.success(), "fixture compilation failed");
    output
}

fn run_fixture(name: &str, client_argument: &str) -> Output {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = compile_fixture(directory.path(), name);
    let guest = Command::new(fixture);
    DbtRunner::from_env()
        .expect("DYNAMORIO_HOME (or DynamoRIO_DIR) and REVERIE_DBT_CLIENT must be set")
        .client_argument(client_argument)
        .output(&guest)
        .unwrap_or_else(|error| panic!("run {name}: {error}"))
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn reused_host_tid_waits_for_the_clone_parents_virtual_identity() {
    let output = run_fixture("reused_tid_start", "-test-reused-tid");
    assert!(output.status.success(), "guest failed: {output:?}");
    assert_eq!(output.stdout, b"reused-tid-start=ok tid=4\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("REUSED_TID_TEST exercised=1"),
        "the stale host-TID mapping was not exercised: {output:?}"
    );
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn an_exiting_process_does_not_block_another_process_thread_start() {
    let output = run_fixture(
        "thread_clone_process_exit",
        "-test-thread-clone-process-exit",
    );
    assert!(output.status.success(), "guest failed: {output:?}");
    assert_eq!(output.stdout, b"thread-clone-process-exit=ok tid=6\n");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("THREAD_CLONE_PROCESS_EXIT_TEST exercised=1"),
        "the exiting process path was not exercised: {output:?}"
    );
}
