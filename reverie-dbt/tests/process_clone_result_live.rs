/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Live process-clone callback delivery regression coverage.

use std::path::Path;
use std::process::Command;

use reverie_dbt::DbtRunner;

fn compile_fixture(output: &Path) {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/process_clone_results.c");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = Command::new(compiler)
        .args(["-O2", "-g", "-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(output)
        .status()
        .expect("compile process-clone fixture");
    assert!(status.success(), "fixture compilation failed");
}

fn callback_results(stderr: &[u8], sysnum: i64) -> Vec<i64> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|line| {
            let fields = line.strip_prefix("reverie-dbt-test: process-clone-result ")?;
            let mut fields = fields.split_whitespace();
            let observed_sysnum: i64 = fields.next()?.strip_prefix("sysnum=")?.parse().ok()?;
            let result: i64 = fields.next()?.strip_prefix("result=")?.parse().ok()?;
            (observed_sysnum == sysnum).then_some(result)
        })
        .collect()
}

fn assert_parent_and_child(results: &[i64], name: &str) {
    assert_eq!(
        results.iter().filter(|result| **result == 0).count(),
        1,
        "{name} must deliver exactly one child-zero callback: {results:?}"
    );
    assert_eq!(
        results.iter().filter(|result| **result > 0).count(),
        1,
        "{name} must deliver exactly one parent-positive callback: {results:?}"
    );
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn process_clone_result_delivery_matches_the_public_contract() {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = directory.path().join("process-clone-results");
    compile_fixture(&fixture);

    let runner = DbtRunner::from_env()
        .expect("DYNAMORIO_HOME (or DynamoRIO_DIR) and REVERIE_DBT_CLIENT must be set")
        .client_argument("-test-wait-for-background");
    let mut guest = Command::new(fixture);
    guest.env("REVERIE_DBT_TEST_PROCESS_CLONE_RESULTS", "1");
    let output = runner
        .output(&guest)
        .expect("process-clone matrix must run");
    assert!(output.status.success(), "guest failed: {output:?}");
    assert_eq!(output.stdout, b"process-clone-results-ok\n");

    let clone_results = callback_results(&output.stderr, libc::SYS_clone);
    assert_eq!(
        clone_results
            .iter()
            .filter(|result| **result == -libc::EINVAL as i64)
            .count(),
        1,
        "invalid clone must deliver its raw -EINVAL once: {clone_results:?}"
    );
    assert_parent_and_child(
        &clone_results
            .into_iter()
            .filter(|result| *result >= 0)
            .collect::<Vec<_>>(),
        "clone",
    );
    assert_parent_and_child(&callback_results(&output.stderr, libc::SYS_fork), "fork");
    #[cfg(target_arch = "x86_64")]
    assert_parent_and_child(
        &callback_results(&output.stderr, libc::SYS_clone3),
        "clone3",
    );

    let vfork_results = callback_results(&output.stderr, libc::SYS_vfork);
    assert_eq!(
        vfork_results.len(),
        1,
        "vfork must deliver only its parent result: {vfork_results:?}"
    );
    assert!(vfork_results[0] > 0, "vfork result must be parent-positive");
}
