#!/usr/bin/env -S rust-script --force
//! Refuse to let a validate receipt go unreadable again.
//!
//! Every line the canonical ledger adapter emits must deserialize into the SAME
//! `HistoryRow` the landing-authority reader uses. This asserts that, against
//! the live `../lib/records.rs` rather than a copy, so it cannot drift away
//! from the type it is guarding.
//!
//! WHY THIS EXISTS. `ci-hub validate-status` used to print
//! `skipped N unparseable ledger line(s)` and discard the reason. Receipts sat
//! unreadable behind that count — including `result: pass` receipts, i.e. work
//! that validated and no query could see. Finding out why required writing a
//! throwaway program to re-run the parse and print the error serde had already
//! produced and thrown away. That program should not have been a throwaway;
//! this is it, kept.
//!
//! Read-only: reads the ledger through the canonical adapter and writes
//! nothing.
//!
//!   ci-hub/ledger/check-ledger-parses.rs            # exit 1 if anything is unreadable
//!   ci-hub/ledger/check-ledger-parses.rs --quiet    # only report failures
//!
//! ```cargo
//! [dependencies]
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! ```

// ZERO-WARNINGS, MADE INTRINSIC RATHER THAN DELEGATED TO AN EXTERNAL GATE.
// A rust-script is invisible to `cargo clippy --workspace`, so the project rule
// held here only for as long as something remembered to cover this file.
//
// ⚠️ A CLOSED, NAMED SET -- DELIBERATELY NOT `deny(warnings)`. These compile at
// INVOCATION, so a blanket deny would let a future toolchain introducing any new
// lint stop the script from RUNNING. That is a production break, not a build
// break, and it would be worse than the warnings it prevents. An enumerated set
// cannot be widened by a toolchain upgrade. New lints are caught instead by
// `scripts/lint-rust-scripts.sh`, which compiles this file under `-D warnings`
// externally, where a failure blocks a commit rather than an execution.
#![deny(
    dead_code,
    unused_imports,
    unused_mut,
    unused_variables,
    unused_must_use
)]
// `records.rs` is a SHARED module. Liveness of a symbol in it is a property of
// the whole program, not of this script: this checker uses a handful of the row
// types, `ci-hub.rs` uses the rest, and each `#[path]`-including script sees
// everything it personally does not call as dead.
//
// ⚠️ THE ALLOW IS ON THE INCLUDE, NOT INSIDE THE MODULE, AND THAT DISTINCTION IS
// THE POINT. Scoped here it silences only "unused FROM THIS ENTRY POINT", while
// `ci-hub.rs` — which consumes the superset — still gates the same file under
// `-D warnings` and would still fail on genuinely dead code. Putting the allow
// inside `records.rs` would blind every consumer at once.
//
// Without this, a per-script zero-warnings gate demands deleting live shared
// types. Measured 2026-08-25: 8 findings here, all of them symbols with 2 to 9
// references elsewhere in ci-hub.
#[allow(dead_code)]
#[path = "../lib/records.rs"]
mod records;
// Same reasoning as `records` above: the ONE capture wrapper is shared by path
// rather than copied, so this script cannot drift away from the fix. Only one of
// its two entry points is used here, hence the allow.
#[allow(dead_code)]
#[path = "../lib/bounded_output.rs"]
mod bounded_output;
use bounded_output::BoundedOutput;

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    // This script lives at <root>/ci-hub/ledger/, so the root is two up. Derived
    // from the script's own location rather than the cwd, so it behaves the
    // same from a lint runner as from a shell.
    PathBuf::from(file!())
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn main() {
    let quiet = std::env::args().any(|arg| arg == "--quiet");
    let root = repo_root();

    let output = Command::new("python3")
        .arg(root.join("ci-hub/ledger/validate_rows.py"))
        .arg("rows")
        .bounded_output()
        .expect("run the canonical validate-ledger union");
    if !output.status.success() {
        eprintln!(
            "check-ledger-parses: the canonical union refused: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        std::process::exit(2);
    }
    let buf = String::from_utf8(output.stdout).expect("union emitted UTF-8");

    let mut total = 0usize;
    let mut failures = Vec::new();
    for (index, line) in buf.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        total += 1;
        if let Err(error) = serde_json::from_str::<records::HistoryRow>(line) {
            // Recover identity permissively: the typed parse just failed, but
            // the operator still needs to know WHICH receipt was lost.
            let salvaged = serde_json::from_str::<serde_json::Value>(line).ok();
            let field = |name: &str| {
                salvaged
                    .as_ref()
                    .and_then(|value| value.get(name))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("-")
                    .to_string()
            };
            failures.push(format!(
                "line {}: {error}\n    record_id={} commit={} run_id={} repo={} host={} producer={} started_at={} result={}",
                index + 1,
                field("record_id"),
                field("commit"),
                field("run_id"),
                field("repo"),
                field("host"),
                field("producer"),
                field("started_at"),
                field("result"),
            ));
        }
    }

    if failures.is_empty() {
        if !quiet {
            println!("check-ledger-parses: OK — {total}/{total} ledger rows parse as HistoryRow");
        }
        return;
    }

    eprintln!(
        "check-ledger-parses: FAIL — {} of {total} ledger row(s) are UNREADABLE and therefore \
         invisible to every ledger query, including the landing-authority one:",
        failures.len()
    );
    for failure in &failures {
        eprintln!("  {failure}");
    }
    eprintln!(
        "A receipt that cannot be parsed does not exist as far as any verdict is concerned. \
         Fix the writer or schema mismatch; do not widen the reader to assign meaning to malformed \
         evidence, and do not rewrite the ledger."
    );
    std::process::exit(1);
}
