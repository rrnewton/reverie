/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A tool that answers the guest's `brk(2)` itself
//! ([`DbtRunner::tool_answers_brk`]) finds the guest's start of the break as
//! the kernel leaves it: no page mapped after the guest image, and the
//! kernel's break where the kernel put it.
//!
//! Without the option, DynamoRIO's early injection reserves for the break.
//! It maps an inaccessible page after the guest image (its separate-.bss
//! page), maps a region of its own to emulate `brk` in, and with emulation
//! off nudges the kernel's break by a byte. A tool that keeps its own model
//! of the break, starting after the image as Linux's does, would find that
//! page in the way.
//!
//! [`tests/fixtures/tool_answers_brk.c`] reports, before anything has
//! allocated, whether the page after its image is mapped, whether `brk(0)`
//! is the kernel's start_brk, and whether the break lies in a mapping. With
//! the option, its output under DynamoRIO equals its native output. Without
//! it, the page is mapped, which is the control.
//!
//! `#[ignore]`d like the other live tests; run it with:
//!
//! ```text
//! REVERIE_DBT_CLIENT=<...>/libreverie_dbt_client.so \
//!   cargo test -p reverie-dbt --test tool_answers_brk_live -- --ignored --nocapture
//! ```

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

use reverie_dbt::DbtRunner;

fn compile_fixture(directory: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tool_answers_brk.c");
    let binary = directory.join("tool_answers_brk");
    let compile = Command::new(std::env::var("CC").unwrap_or_else(|_| "cc".into()))
        .args(["-O2", "-g", "-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap_or_else(|error| panic!("failed to start the C compiler: {error}"));
    assert!(
        compile.status.success(),
        "failed to compile {}: {}",
        source.display(),
        String::from_utf8_lossy(&compile.stderr)
    );
    binary
}

fn under_dynamorio(fixture: &Path, tool_answers_brk: bool) -> Output {
    let client = std::env::var_os("REVERIE_DBT_CLIENT")
        .expect("REVERIE_DBT_CLIENT must point to the built native client");
    DbtRunner::new(reverie_dbt::bundled_drrun_path(), client)
        .expect("create the native runner")
        .tool_answers_brk(tool_answers_brk)
        .output(&Command::new(fixture))
        .unwrap_or_else(|error| panic!("run the fixture: {error}"))
}

fn stdout_of(label: &str, output: &Output) -> String {
    assert!(output.status.success(), "{label} failed: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn a_tool_that_answers_brk_finds_the_break_as_the_kernel_leaves_it() {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = compile_fixture(directory.path());
    let native = stdout_of(
        "native",
        &Command::new(&fixture)
            .output()
            .expect("run the fixture natively"),
    );
    assert_eq!(
        native,
        "page after the image mapped: no\nbrk(0) is the kernel's start_brk: yes\nbreak in a mapping: no\n"
    );
    assert_eq!(
        stdout_of(
            "DynamoRIO, tool answers brk",
            &under_dynamorio(&fixture, true)
        ),
        native
    );
    let reserved = stdout_of("DynamoRIO", &under_dynamorio(&fixture, false));
    assert!(
        reserved.contains("page after the image mapped: yes"),
        "without the option DynamoRIO reserves the page after the image: {reserved}"
    );
}
