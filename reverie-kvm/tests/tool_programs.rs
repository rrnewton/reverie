/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#![cfg(target_arch = "x86_64")]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::process::Output;

use kvm_ioctls::Kvm;

const KVM_TOOL: &str = env!("CARGO_BIN_EXE_kvm-tool");

struct Case {
    label: &'static str,
    program: &'static str,
    args: &'static [&'static str],
    expected_stdout: Vec<u8>,
}

#[test]
fn counter_and_strace_agree_for_real_programs() {
    if let Err(error) = Kvm::new() {
        eprintln!("skipping KVM real-program tool test: cannot open /dev/kvm: {error}");
        return;
    }

    let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let cases = vec![
        Case {
            label: "echo",
            program: "/bin/echo",
            args: &["hello"],
            expected_stdout: b"hello\n".to_vec(),
        },
        Case {
            label: "cat",
            program: "/bin/cat",
            args: &["/dev/null"],
            expected_stdout: Vec::new(),
        },
        Case {
            label: "ls",
            program: "/bin/ls",
            args: &["-d", "/"],
            expected_stdout: b"/\n".to_vec(),
        },
        Case {
            label: "true",
            program: "/bin/true",
            args: &[],
            expected_stdout: Vec::new(),
        },
        Case {
            label: "pwd",
            program: "/bin/pwd",
            args: &[],
            expected_stdout: format!("{}\n", cwd.display()).into_bytes(),
        },
        Case {
            label: "printf",
            program: "/usr/bin/printf",
            args: &["%s|%s\n", "alpha", "two words"],
            expected_stdout: b"alpha|two words\n".to_vec(),
        },
        Case {
            label: "seq",
            program: "/usr/bin/seq",
            args: &["1", "3"],
            expected_stdout: b"1\n2\n3\n".to_vec(),
        },
        Case {
            label: "head",
            program: "/usr/bin/head",
            args: &["-c", "0", "/dev/null"],
            expected_stdout: Vec::new(),
        },
        Case {
            label: "base64",
            program: "/usr/bin/base64",
            args: &["/dev/null"],
            expected_stdout: Vec::new(),
        },
        Case {
            label: "id",
            program: "/usr/bin/id",
            args: &["-u"],
            expected_stdout: b"0\n".to_vec(),
        },
        Case {
            label: "basename",
            program: "/usr/bin/basename",
            args: &["/tmp/item"],
            expected_stdout: b"item\n".to_vec(),
        },
        Case {
            label: "shell-exec",
            program: "/bin/sh",
            args: &["-c", "/bin/echo child"],
            expected_stdout: b"child\n".to_vec(),
        },
        Case {
            label: "shell-pipeline",
            program: "/bin/sh",
            args: &["-c", "/bin/echo child | /bin/cat"],
            expected_stdout: b"child\n".to_vec(),
        },
        Case {
            label: "xargs",
            program: "/bin/sh",
            args: &["-c", "echo hi | xargs echo"],
            expected_stdout: b"hi\n".to_vec(),
        },
    ];

    for case in cases {
        assert!(
            Path::new(case.program).is_file(),
            "{} fixture program is unavailable at {}",
            case.label,
            case.program,
        );

        let counter = run_tool("counter", &case);
        let strace = run_tool("strace", &case);

        assert_eq!(
            counter.stdout, case.expected_stdout,
            "{} counter guest stdout",
            case.label,
        );
        assert_eq!(
            strace.stdout, case.expected_stdout,
            "{} strace guest stdout",
            case.label,
        );

        let counter_counts = parse_counter(&counter);
        let strace_counts = parse_strace(&strace);
        assert_eq!(
            counter_counts, strace_counts,
            "{} counter/strace syscall histogram",
            case.label,
        );
        assert!(
            counter_counts.values().sum::<u64>() > 0,
            "{} produced no intercepted syscalls",
            case.label,
        );
        assert!(
            counter_counts.contains_key("exit_group"),
            "{} did not intercept exit_group: {counter_counts:?}",
            case.label,
        );
        if case.label == "shell-pipeline" {
            assert!(
                counter_counts.get("clone").copied().unwrap_or_default() >= 2,
                "pipeline children did not reach the tool: {counter_counts:?}",
            );
            assert!(
                counter_counts.get("execve").copied().unwrap_or_default() >= 2,
                "pipeline child execs did not reach the tool: {counter_counts:?}",
            );
        }
        if case.label == "shell-exec" {
            assert!(
                counter_counts.contains_key("execve"),
                "shell exec did not reach the tool: {counter_counts:?}",
            );
        }
        if case.label == "xargs" {
            for (mode, output) in [("counter", &counter), ("strace", &strace)] {
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    !stderr.contains("No child processes"),
                    "{mode} xargs wait failed:\n{stderr}",
                );
            }
            assert!(
                counter_counts.contains_key("wait4"),
                "xargs did not exercise wait4: {counter_counts:?}",
            );
        }
    }
}

fn run_tool(mode: &str, case: &Case) -> Output {
    let output = Command::new(KVM_TOOL)
        .arg(mode)
        .arg("--")
        .arg(case.program)
        .args(case.args)
        .output()
        .unwrap_or_else(|error| panic!("failed to run {mode} for {}: {error}", case.label));
    assert!(
        output.status.success(),
        "{mode} failed for {}: status={:?}\nstdout={}\nstderr={}",
        case.label,
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn parse_counter(output: &Output) -> BTreeMap<String, u64> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("[kvm-counter-summary] ")),
        "counter summary missing from stderr:\n{stderr}",
    );

    stderr
        .lines()
        .filter_map(|line| line.strip_prefix("[kvm-counter] syscall="))
        .map(|line| {
            let (name, count) = line
                .split_once(" count=")
                .unwrap_or_else(|| panic!("malformed counter line: {line}"));
            (
                name.to_owned(),
                count
                    .parse::<u64>()
                    .unwrap_or_else(|error| panic!("malformed counter count {count}: {error}")),
            )
        })
        .collect()
}

fn parse_strace(output: &Output) -> BTreeMap<String, u64> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("[kvm-strace-summary] ")),
        "strace summary missing from stderr:\n{stderr}",
    );

    let mut counts = BTreeMap::new();
    for line in stderr
        .lines()
        .filter_map(|line| line.strip_prefix("[kvm-strace] "))
    {
        let name = line
            .split_whitespace()
            .next()
            .unwrap_or_else(|| panic!("malformed strace line: {line}"));
        *counts.entry(name.to_owned()).or_default() += 1;
    }
    counts
}
