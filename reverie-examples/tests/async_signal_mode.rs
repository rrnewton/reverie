/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Asynchronous signal mode (`F_SETFL` with `O_ASYNC`, and `FIOASYNC`) on
//! every backend, under the transparent `noop` tool, against native Linux.
//! Each backend's actual values are asserted, not only their agreement. KVM
//! refuses the mode by name where its Linux provider cannot keep the signal
//! inside the run, here on /dev/urandom and on an inherited standard input:
//! those rows are ledgered in `e2e/CONFORMANCE.md` ("Asynchronous signal
//! mode") and count as refusals, never as parity.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const ROWS: usize = 13;
const ENOTTY: i64 = -(libc::ENOTTY as i64);
const ENOSYS: i64 = -(libc::ENOSYS as i64);

/// The fixture's rows (tests/fixtures/async_signal_mode.c), as Linux answers
/// them: a pipe the program makes (`FIOASYNC` on, the flag, off; `F_SETFL`
/// with `O_ASYNC`, the flag), /dev/null (`FIOASYNC` on: a requested
/// transition with no fasync, ENOTTY; `F_SETFL`, which cannot set it; the
/// flag), /dev/urandom (`FIOASYNC` on; `F_SETFL`), and the inherited standard
/// input pipe (`FIOASYNC` on; `F_SETFL`; the flag).
const LINUX: [i64; ROWS] = [0, 1, 0, 0, 1, ENOTTY, 0, 0, 0, 0, 0, 0, 1];

/// KVM's: Linux's, but for the typed refusals' ENOSYS on /dev/urandom and on
/// the inherited standard input, which changed nothing.
const KVM: [i64; ROWS] = [0, 1, 0, 0, 1, ENOTTY, 0, 0, ENOSYS, ENOSYS, ENOSYS, ENOSYS, 0];

fn compile_fixture(directory: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/async_signal_mode.c");
    let guest = directory.join("async-signal-mode");
    let output = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&guest)
        .output()
        .expect("compile the asynchronous signal mode fixture");
    assert!(
        output.status.success(),
        "{}: {}",
        source.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    guest
}

/// The fixture's rows on `backend` (`native` runs it with no tool), with a
/// fresh pipe as its standard input.
fn rows(backend: &str) -> Vec<i64> {
    let directory = tempfile::tempdir().unwrap();
    let guest = compile_fixture(directory.path());
    let log = directory.path().join("tool.log");
    let mut command = if backend == "native" {
        Command::new(&guest)
    } else {
        let mut command = Command::new(env!("CARGO_BIN_EXE_noop"));
        command
            .arg("--log-file")
            .arg(&log)
            .args(["--backend", backend, "--"])
            .arg(&guest);
        command
    };
    let (reader, writer) = std::io::pipe().unwrap();
    drop(writer);
    let output = command
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
        .stdin(reader)
        .output()
        .expect("run the asynchronous signal mode fixture");
    assert!(
        output.status.success() && output.stdout.len() == ROWS * 8,
        "{backend}: {output:?}\ntool log:\n{}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    output
        .stdout
        .chunks_exact(8)
        .map(|word| i64::from_le_bytes(word.try_into().unwrap()))
        .collect()
}

/// Whether this host can run the KVM backend; when `REVERIE_REQUIRE_KVM` is
/// set its absence is a failure, otherwise a `REVERIE_SKIP:` line the
/// scorecard counts as skipped, never as passed (as in `conformance.rs`).
fn kvm_available() -> bool {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
    {
        Ok(_) => true,
        Err(error) if std::env::var_os("REVERIE_REQUIRE_KVM").is_some() => {
            panic!("this test requires usable /dev/kvm: {error}")
        }
        Err(error) => {
            eprintln!("\nREVERIE_SKIP: kvm asynchronous signal mode: cannot open /dev/kvm: {error}");
            false
        }
    }
}

#[test]
fn native_answers_are_the_reference() {
    assert_eq!(rows("native"), LINUX);
}

#[test]
fn ptrace_answers_as_linux() {
    assert_eq!(rows("ptrace"), LINUX);
}

#[test]
fn liteinst_answers_as_linux() {
    assert_eq!(rows("liteinst"), LINUX);
}

#[test]
fn in_guest_trap_answers_as_linux() {
    assert_eq!(rows("in-guest-trap"), LINUX);
}

#[test]
fn kvm_refuses_by_name_where_it_cannot_keep_the_signal() {
    if kvm_available() {
        assert_eq!(rows("kvm"), KVM);
    }
}
