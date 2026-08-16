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
use std::path::Path;
use std::process::Command;

use reverie_dbt::Counter2Global;
use reverie_dbt::DbtRunner;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run with --ignored"]
async fn protected_evidence_survives_fork_pthread_lifecycle() {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = directory.path().join("fork-pthread-identity");
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fork_pthread_identity.c");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let compile_status = Command::new(compiler)
        .args(["-O2", "-g", "-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg("-pthread")
        .arg(source)
        .arg("-o")
        .arg(&fixture)
        .status()
        .expect("compile fork/pthread fixture");
    assert!(compile_status.success(), "fixture compilation failed");

    let mut evidence_file = tempfile::tempfile().expect("evidence tempfile");
    let runner = DbtRunner::from_env()
        .expect("DYNAMORIO_HOME (or DynamoRIO_DIR) and REVERIE_DBT_CLIENT must be set")
        .evidence_file(&evidence_file)
        .expect("configure protected evidence");
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
        !evidence.records().is_empty(),
        "fork/pthread run must publish protected evidence"
    );
}
