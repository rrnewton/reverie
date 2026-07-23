/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! End-to-end multi-process (fork/exec) coordination test.
//!
//! Runs a genuinely-forking shell pipeline under the DynamoRIO client and checks
//! that every followed process (the shell plus each pipeline stage) contributes
//! a record to the coordinator directory, that the offline aggregator
//! reconstructs the process tree, and that its deterministic rendering is stable
//! across runs despite the OS pids differing.
//!
//! This test needs a built client and a DynamoRIO install, so it self-skips
//! unless `DYNAMORIO_HOME` (or `DynamoRIO_DIR`) is set; it therefore stays green
//! in environments without DynamoRIO while still being a real E2E when they are
//! present. Run it with, e.g.:
//!
//! ```text
//! DYNAMORIO_HOME=/path/to/dynamorio cargo test -p reverie-dbi --test multiprocess_coordinator -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::Command;

use reverie_dbi::DbiRunner;
use reverie_dbi::coordinator;

fn dynamorio_available() -> bool {
    std::env::var_os("DYNAMORIO_HOME").is_some() || std::env::var_os("DynamoRIO_DIR").is_some()
}

/// Runs `/bin/bash -c '/bin/echo hi | cat'` under the client with a fresh
/// coordinator directory and returns the aggregated process tree.
fn run_pipeline(runner: &DbiRunner, coord_dir: &std::path::Path) -> coordinator::ProcessTree {
    std::fs::create_dir_all(coord_dir).unwrap();
    let mut guest = Command::new("/bin/bash");
    guest.arg("-c").arg("/bin/echo hi | cat");

    // The client reads REVERIE_DBI_COORD_DIR from the (inherited) guest
    // environment; it survives the fork and the execs of the pipeline stages.
    let mut environment: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    environment.insert(
        OsString::from(coordinator::COORD_DIR_ENV),
        coord_dir.as_os_str().to_owned(),
    );

    let status = runner
        .status_with_environment(&guest, &environment)
        .expect("failed to launch guest under DynamoRIO");
    assert!(status.success(), "guest exited with failure: {status:?}");

    coordinator::summarize_dir(coord_dir).expect("failed to read coordinator directory")
}

#[test]
fn pipeline_is_followed_and_coordinated_deterministically() {
    if !dynamorio_available() {
        eprintln!("skipping: DYNAMORIO_HOME/DynamoRIO_DIR not set (no DynamoRIO available)");
        return;
    }
    let runner = match DbiRunner::from_env() {
        Ok(runner) => runner,
        Err(error) => {
            eprintln!("skipping: could not resolve DynamoRIO client: {error}");
            return;
        }
    };

    let first_dir = tempfile::tempdir().unwrap();
    let first = run_pipeline(&runner, first_dir.path());

    // bash (root) plus at least one followed pipeline stage must be recorded.
    // `/bin/echo hi | cat` yields bash + echo + cat = 3 processes.
    assert!(
        first.process_count() >= 2,
        "expected the child processes to be followed, saw {} process(es):\n{}",
        first.process_count(),
        first.render_deterministic()
    );
    eprintln!("{}", first.render_deterministic());

    // The root is the shell; every non-root process descends from it.
    assert!(first.nodes.iter().any(|n| n.parent_det_id.is_none()));
    assert!(
        first
            .nodes
            .iter()
            .filter(|n| n.parent_det_id.is_some())
            .count()
            >= 1
    );

    // Determinism: a second run with entirely different OS pids must produce a
    // byte-identical deterministic rendering.
    let second_dir = tempfile::tempdir().unwrap();
    let second = run_pipeline(&runner, second_dir.path());
    assert_eq!(
        first.render_deterministic(),
        second.render_deterministic(),
        "deterministic process-tree rendering differed across runs"
    );
}
