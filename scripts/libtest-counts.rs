#!/usr/bin/env -S rust-script --force
//! Copyright (c) Meta Platforms, Inc. and affiliates.
//! All rights reserved.
//!
//! This source code is licensed under the BSD-style license found in the
//! LICENSE file in the root directory of this source tree.
//!
//! Run one Cargo test command with libtest JSON enabled and record its counts.
//!
//! ```cargo
//! [dependencies]
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! ```

use std::env;
use std::fs;
use std::io;
use std::io::BufRead;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitCode;
use std::process::Stdio;

use serde::Deserialize;
use serde::Serialize;

const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TestCounts {
    schema_version: u32,
    executed_tests: u64,
    filtered_tests: u64,
}

impl TestCounts {
    fn empty() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            executed_tests: 0,
            filtered_tests: 0,
        }
    }

    fn add_suite(
        &mut self,
        passed: u64,
        failed: u64,
        measured: u64,
        filtered_out: u64,
    ) -> Result<(), String> {
        self.executed_tests = self
            .executed_tests
            .checked_add(passed)
            .and_then(|count| count.checked_add(failed))
            .and_then(|count| count.checked_add(measured))
            .ok_or_else(|| "executed test count overflowed u64".to_string())?;
        self.filtered_tests = self
            .filtered_tests
            .checked_add(filtered_out)
            .ok_or_else(|| "filtered test count overflowed u64".to_string())?;
        Ok(())
    }
}

fn parse_events<R: BufRead>(reader: R, mut echo: impl Write) -> Result<TestCounts, String> {
    let mut counts = TestCounts::empty();
    let mut started = None;
    let mut completed = 0_u64;

    for line in reader.lines() {
        let line = line.map_err(|error| format!("cannot read libtest JSON: {error}"))?;
        writeln!(echo, "{line}").map_err(|error| format!("cannot copy test output: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&line)
            .map_err(|error| format!("libtest emitted a non-JSON line: {error}: {line:?}"))?;
        if value.get("type").and_then(serde_json::Value::as_str) != Some("suite") {
            continue;
        }
        match value.get("event").and_then(serde_json::Value::as_str) {
            Some("started") => {
                if started.is_some() {
                    return Err("a suite started before the preceding suite terminated".to_string());
                }
                let test_count = value
                    .get("test_count")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| {
                        "started suite event lacks nonnegative test_count".to_string()
                    })?;
                started = Some(test_count);
            }
            Some("ok" | "failed") => {
                let expected = started.take().ok_or_else(|| {
                    "terminal suite event appeared without a started suite".to_string()
                })?;
                let number = |field: &str| {
                    value
                        .get(field)
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| format!("terminal suite event lacks nonnegative {field}"))
                };
                let passed = number("passed")?;
                let failed = number("failed")?;
                let ignored = number("ignored")?;
                let measured = number("measured")?;
                let filtered_out = number("filtered_out")?;
                let selected = passed
                    .checked_add(failed)
                    .and_then(|count| count.checked_add(ignored))
                    .and_then(|count| count.checked_add(measured))
                    .ok_or_else(|| "selected test count overflowed u64".to_string())?;
                if selected != expected {
                    return Err(format!(
                        "suite selected {expected} tests but terminal counts total {selected}"
                    ));
                }
                counts.add_suite(passed, failed, measured, filtered_out)?;
                completed = completed
                    .checked_add(1)
                    .ok_or_else(|| "completed suite count overflowed u64".to_string())?;
            }
            Some(event) => return Err(format!("unknown suite event {event:?}")),
            None => return Err("suite event lacks string event".to_string()),
        }
    }

    if started.is_some() {
        return Err("a started suite lacks a terminal event".to_string());
    }
    if completed == 0 {
        return Err("libtest emitted no complete suite".to_string());
    }
    Ok(counts)
}

fn write_counts(path: &Path, counts: TestCounts) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("count path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("count path has no UTF-8 file name: {}", path.display()))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    let mut bytes = serde_json::to_vec(&counts)
        .map_err(|error| format!("cannot encode test counts: {error}"))?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes)
        .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!(
            "cannot publish {} as {}: {error}",
            temporary.display(),
            path.display()
        )
    })
}

fn read_counts(path: &Path) -> Result<TestCounts, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let counts: TestCounts = serde_json::from_slice(&bytes)
        .map_err(|error| format!("malformed {}: {error}", path.display()))?;
    if counts.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "unsupported test-count schema {} in {}",
            counts.schema_version,
            path.display()
        ));
    }
    Ok(counts)
}

fn cargo_test_args(mut args: Vec<String>) -> Result<Vec<String>, String> {
    if args.first().map(String::as_str) != Some("cargo")
        || args.get(1).map(String::as_str) != Some("test")
    {
        return Err("run requires a cargo test command".to_string());
    }
    if !args.iter().any(|arg| arg == "--") {
        args.push("--".to_string());
    }
    args.push("-Z".to_string());
    args.push("unstable-options".to_string());
    args.push("--format".to_string());
    args.push("json".to_string());
    Ok(args)
}

fn run(path: PathBuf, args: Vec<String>) -> Result<i32, String> {
    let args = cargo_test_args(args)?;
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("cannot start cargo test: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "cargo test stdout was not piped".to_string())?;
    let parsed = parse_events(io::BufReader::new(stdout), io::stdout().lock());
    let status = child
        .wait()
        .map_err(|error| format!("cannot wait for cargo test: {error}"))?;
    let counts = parsed?;
    write_counts(&path, counts)?;
    Ok(status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1))
}

fn self_test() -> Result<(), String> {
    let first = concat!(
        "{\"type\":\"suite\",\"event\":\"started\",\"test_count\":4}\n",
        "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"same rendered text\"}\n",
        "{\"type\":\"suite\",\"event\":\"ok\",\"passed\":2,\"failed\":1,",
        "\"ignored\":1,\"measured\":0,\"filtered_out\":3}\n"
    );
    let second = concat!(
        "{\"type\":\"suite\",\"event\":\"started\",\"test_count\":6}\n",
        "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"same rendered text\"}\n",
        "{\"type\":\"suite\",\"event\":\"ok\",\"passed\":5,\"failed\":0,",
        "\"ignored\":1,\"measured\":0,\"filtered_out\":1}\n"
    );
    let mut first_echo = Vec::new();
    let mut second_echo = Vec::new();
    let first_counts = parse_events(first.as_bytes(), &mut first_echo)?;
    let second_counts = parse_events(second.as_bytes(), &mut second_echo)?;
    if first_counts.executed_tests != 3 || first_counts.filtered_tests != 3 {
        return Err(format!("first mutation produced {first_counts:?}"));
    }
    if second_counts.executed_tests != 5 || second_counts.filtered_tests != 1 {
        return Err(format!("second mutation produced {second_counts:?}"));
    }
    if first_counts == second_counts {
        return Err("mutating typed suite counts did not change the record".to_string());
    }
    if parse_events("same rendered text\n".as_bytes(), io::sink()).is_ok() {
        return Err("human output was accepted as libtest JSON".to_string());
    }
    if parse_events(
        "{\"type\":\"suite\",\"event\":\"started\",\"test_count\":1}\n".as_bytes(),
        io::sink(),
    )
    .is_ok()
    {
        return Err("unterminated suite was accepted".to_string());
    }
    println!("PASS: libtest JSON counts are complete, typed, and mutation-sensitive");
    Ok(())
}

fn usage() {
    eprintln!(
        "usage: libtest-counts.rs run OUTPUT -- cargo test [ARGS...]\n       libtest-counts.rs read OUTPUT\n       libtest-counts.rs --self-test"
    );
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("run") => {
            let path = args.next().map(PathBuf::from);
            let separator = args.next();
            match (path, separator.as_deref()) {
                (Some(path), Some("--")) => run(path, args.collect()).map(|code| {
                    if code == 0 {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(u8::try_from(code).unwrap_or(1))
                    }
                }),
                _ => Err("run requires OUTPUT -- cargo test [ARGS...]".to_string()),
            }
        }
        Some("read") => match (args.next(), args.next()) {
            (Some(path), None) => read_counts(Path::new(&path)).map(|counts| {
                println!("{} {}", counts.executed_tests, counts.filtered_tests);
                ExitCode::SUCCESS
            }),
            _ => Err("read requires exactly one OUTPUT path".to_string()),
        },
        Some("--self-test") if args.next().is_none() => self_test().map(|()| ExitCode::SUCCESS),
        _ => {
            usage();
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("libtest-counts: {error}");
            ExitCode::from(2)
        }
    }
}
