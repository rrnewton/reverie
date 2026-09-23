/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Executable coverage for copied-process runtime ownership and exit state.

use std::path::Path;
use std::process::Command;

#[test]
fn copied_process_lineage_and_exit_state_machine() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = manifest.join("tests/fixtures/runtime_lineage_state_machine.c");
    let directory = tempfile::tempdir().expect("lineage fixture tempdir");
    let executable = directory.path().join("runtime-lineage-state-machine");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());

    let compile = Command::new(compiler)
        .args([
            "-O2",
            "-std=c11",
            "-D_GNU_SOURCE",
            "-Wall",
            "-Wextra",
            "-Werror",
        ])
        .arg("-I")
        .arg(manifest.join("native"))
        .arg(source)
        .arg("-o")
        .arg(&executable)
        .status()
        .expect("compile lineage state-machine fixture");
    assert!(compile.success(), "lineage fixture compilation failed");

    let run = Command::new(executable)
        .status()
        .expect("run lineage state-machine fixture");
    assert!(run.success(), "lineage state-machine fixture failed");
}
