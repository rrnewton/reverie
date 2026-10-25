// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Host-boundary refusal controls. These never execute a compiler or establish
//! positive native compilation/qualification evidence.

use std::process::Command;

const CLI: &str = env!("CARGO_BIN_EXE_reverie-native-rust-action");

fn required() -> Vec<&'static str> {
    vec![
        "--expected-inputs",
        "independent.json",
        "--action-inputs",
        "recipe.json",
        "--output-directory",
        "outputs",
        "--invocation",
        "outputs/invocation.json",
        "--evidence",
        "outputs/evidence.json",
        "--finalization",
        "outputs/finalization.json",
        "--input-inventory",
        "outputs/input_inventory.json",
        "--product",
        "product.rlib",
    ]
}

#[test]
fn malformed_host_input_exits_nonzero_before_logs_or_wrapper() {
    for args in [
        vec![],
        vec!["--unknown", "value"],
        vec!["ambient.json"],
        vec!["--expected-inputs", "--"],
    ] {
        let result = Command::new(CLI)
            .args(args)
            .env_remove("TMPDIR")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.starts_with("reverie-native-rust-action:"));
        assert!(
            !error.contains("TMPDIR") && !error.contains("RUST_ACTION_INPUT_METADATA"),
            "malformed input reached action setup: {error}"
        );
    }
}

#[test]
fn valid_syntax_requires_owned_tmpdir_without_starting_wrapper() {
    let mut args = required();
    args.extend(["--", "/no-such-wrapper", "--product", "wrapper-value"]);
    let result = Command::new(CLI)
        .args(args)
        .env("RUST_ACTION_INPUT_METADATA", "executor/input.json")
        .env_remove("TMPDIR")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    let error = String::from_utf8(result.stderr).unwrap();
    assert!(
        error.contains("TMPDIR must name an owned temporary directory"),
        "{error}"
    );
    assert!(!error.contains("spawn"));
}

#[test]
fn missing_or_empty_executor_metadata_refuses_before_logs_or_wrapper() {
    let mut args = required();
    args.extend(["--", "/no-such-wrapper"]);
    for value in [None, Some("")] {
        let mut command = Command::new(CLI);
        command.args(&args).env(
            "TMPDIR",
            std::env::var_os("TMPDIR").expect("owned test TMPDIR"),
        );
        if let Some(value) = value {
            command.env("RUST_ACTION_INPUT_METADATA", value);
        } else {
            command.env_remove("RUST_ACTION_INPUT_METADATA");
        }
        let result = command.output().unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains("RUST_ACTION_INPUT_METADATA"), "{error}");
        assert!(!error.contains("create owned action logs"));
        assert!(!error.contains("spawn"));
    }
}

#[test]
fn empty_wrapper_and_duplicate_optional_output_refuse() {
    let mut empty = required();
    empty.push("--");
    let mut duplicate = required();
    duplicate.extend([
        "--qualified-product",
        "one",
        "--qualified-product",
        "two",
        "--",
        "/no-such-wrapper",
    ]);
    for (args, expected) in [
        (empty, "missing whole normal wrapper executable"),
        (duplicate, "duplicate host option: --qualified-product"),
    ] {
        let result = Command::new(CLI)
            .args(args)
            .env_remove("TMPDIR")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("TMPDIR"));
    }
}
