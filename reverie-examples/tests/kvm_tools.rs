/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#![cfg(target_arch = "x86_64")]

use std::process::Command;

const KVM_EXAMPLES: &str = env!("CARGO_BIN_EXE_kvm_examples");

#[test]
fn all_six_example_tools_run_real_program_on_kvm() {
    if std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_err()
    {
        eprintln!("skipping KVM example-tool matrix: /dev/kvm is unavailable");
        return;
    }

    let cases = [
        ("counter1", "[kvm-counter1-summary]"),
        ("counter2", "[kvm-counter2-summary]"),
        ("strace", "[kvm-strace-summary]"),
        ("strace_minimal", "[kvm-strace-minimal-summary]"),
        ("chaos", "[kvm-chaos-summary]"),
        ("noop", "[kvm-noop-summary]"),
    ];

    for (mode, marker) in cases {
        let output = Command::new(KVM_EXAMPLES)
            .arg(mode)
            .args(["--", "/bin/echo", "hello"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .output()
            .unwrap_or_else(|error| panic!("failed to launch {mode}: {error}"));
        assert!(
            output.status.success(),
            "{mode} failed: status={:?}\nstdout={}\nstderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(output.stdout, b"hello\n", "{mode} guest stdout");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(marker),
            "{mode} omitted completion marker {marker}:\n{stderr}",
        );
        if mode.starts_with("counter") {
            assert!(
                !stderr.contains("total=0"),
                "{mode} did not observe any syscalls:\n{stderr}",
            );
        }
        if mode == "strace" {
            assert!(
                stderr.contains("write(1,"),
                "full strace omitted decoded write arguments:\n{stderr}",
            );
        }
        if mode == "chaos" {
            assert!(
                stderr.contains(" = -512") && stderr.contains(", 1) = 1"),
                "chaos did not exercise restart and short-read behavior:\n{stderr}",
            );
        }
    }
}
