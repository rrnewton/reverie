/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The pass-through recorder (`reverie-examples/recorder.rs`) gives a program
//! the syscall results native Linux gives it, on ptrace, for programs whose
//! record does not depend on the environment. The full corpus, with its
//! expected-mismatch ledger, runs in the environment the ledger was measured
//! in (`reverie-examples/e2e/`).

use std::path::Path;
use std::process::Command;

/// Every run gets the sanitizer settings reverie-ptrace adds to its guest's
/// environment (tracer.rs), so that ptrace's guest has the native one: the
/// number of variables decides when `sh -c` grows its heap.
fn record(backend: &str, program: &[&str], output: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_recorder"))
        .env("ASAN_OPTIONS", "detect_leaks=0")
        .env("LSAN_OPTIONS", "detect_leaks=0")
        .args(["--backend", backend, "--output"])
        .arg(output)
        .args(program)
        .output()
        .expect("run the recorder")
}

#[test]
fn ptrace_records_what_native_linux_returns() {
    let directory = tempfile::tempdir().unwrap();
    for program in [
        &["/bin/true"][..],
        &["/bin/false"],
        &["/bin/echo", "hello"],
        &["/bin/sh", "-c", "echo a; exit 3"],
    ] {
        let native = directory.path().join("native");
        let ptrace = directory.path().join("ptrace");
        for (backend, path) in [("native", &native), ("ptrace", &ptrace)] {
            let output = record(backend, program, path);
            assert!(
                output.status.success(),
                "{backend} {program:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let native = std::fs::read_to_string(&native).unwrap();
        let ptrace = std::fs::read_to_string(&ptrace).unwrap();
        assert!(
            native.starts_with("exit ") && native.contains("\nthread 0\nexecve=ok\n"),
            "{program:?}: {native}"
        );
        assert_eq!(ptrace, native, "{program:?}");
    }
}

/// The in-guest backends host the recorder in the example preload; a program
/// whose calls all come after the loader records as natively.
#[test]
fn in_guest_backends_host_the_recorder_in_the_example_preload() {
    let directory = tempfile::tempdir().unwrap();
    for program in [&["/bin/true"][..], &["/bin/false"]] {
        let native = directory.path().join("native");
        let output = record("native", program, &native);
        assert!(output.status.success(), "native {program:?}");
        let native = std::fs::read_to_string(&native).unwrap();
        for backend in ["liteinst", "in-guest-trap"] {
            let path = directory.path().join(backend);
            let output = record(backend, program, &path);
            assert!(
                output.status.success(),
                "{backend} {program:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let got = std::fs::read_to_string(&path).unwrap();
            // The loader's calls come before the preload's constructor, so
            // the backend's record is the end of the native one.
            let (exit, calls) = got.split_once("\nthread 0\n").unwrap();
            assert_eq!(
                exit,
                native.lines().next().unwrap(),
                "{backend} {program:?}"
            );
            assert!(
                native.ends_with(calls) && !calls.is_empty(),
                "{backend} {program:?}:\n{got}\nnative:\n{native}"
            );
        }
    }
}
