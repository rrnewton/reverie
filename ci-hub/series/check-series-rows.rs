#!/usr/bin/env -S rust-script --force
//! Check proposed stress-series rows with Hermit's shared serialized type.
//!
//! ```cargo
//! [dependencies]
//! hermit-manifest-plan = { path = "../../hermit/ci/manifest-plan" }
//! serde_json = "1"
//! sha2 = "0.10"
//! ```

#![deny(
    dead_code,
    unused_imports,
    unused_mut,
    unused_variables,
    unused_must_use
)]

use hermit_manifest_plan::stress_series::SeriesRow;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::{self, BufRead};
use std::process::ExitCode;

const USAGE: &str = r#"Usage: ci-hub/series/check-series-rows.rs [--read | --projection-results] < rows.jsonl

Deserialize every proposed row as Hermit's shared stress-series type. The
default applies write-boundary checks. With --read, retained v1 rows remain
readable while v2 and v3 rows must pass their complete schema-specific read
checks. With --projection-results, apply the read checks and emit one exact
validate_for_projection result per event_id, plus a SHA-256 digest of the
nonempty input lines with one LF terminator per line. Other successful modes
print no output.
"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Write,
    Read,
    ProjectionResults,
}

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let mode = match arguments.as_slice() {
        [] => Mode::Write,
        [argument] if argument == "--read" => Mode::Read,
        [argument] if argument == "--projection-results" => Mode::ProjectionResults,
        _ => {
            eprintln!(
                "check-series-rows: unexpected argument {:?}\n{USAGE}",
                arguments[0]
            );
            return ExitCode::from(2);
        }
    };
    let stdin = io::stdin();
    let mut rows = 0usize;
    let mut input_sha256 = Sha256::new();
    let mut event_ids = BTreeSet::new();
    let mut projection_results: Vec<(String, Option<String>)> = Vec::new();
    for (index, line) in stdin.lock().lines().enumerate() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!(
                    "check-series-rows: cannot read stdin line {}: {error}",
                    index + 1
                );
                return ExitCode::from(2);
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if mode == Mode::ProjectionResults {
            input_sha256.update(line.as_bytes());
            input_sha256.update(b"\n");
        }
        let row = match serde_json::from_str::<SeriesRow>(&line) {
            Ok(row) => row,
            Err(error) => {
                eprintln!(
                    "check-series-rows: stdin line {} does not match the shared SeriesRow: {error}",
                    index + 1
                );
                return ExitCode::FAILURE;
            }
        };
        let validation = match mode {
            Mode::Write => row.validate_for_write(),
            Mode::Read | Mode::ProjectionResults => row.validate_for_read(),
        };
        if let Err(error) = validation {
            eprintln!("check-series-rows: stdin line {}: {error}", index + 1);
            return ExitCode::FAILURE;
        }
        if mode == Mode::ProjectionResults {
            if !event_ids.insert(row.event_id.clone()) {
                eprintln!(
                    "check-series-rows: stdin line {} repeats event_id {:?}",
                    index + 1,
                    row.event_id
                );
                return ExitCode::FAILURE;
            }
            let projection_result = row.validate_for_projection().err();
            projection_results.push((row.event_id, projection_result));
        }
        rows += 1;
    }
    if rows == 0 {
        eprintln!("check-series-rows: no rows on stdin");
        return ExitCode::from(2);
    }
    if mode == Mode::ProjectionResults {
        projection_results.sort_by(|left, right| left.0.cmp(&right.0));
        let projection_results = projection_results
            .into_iter()
            .map(|(event_id, result)| {
                serde_json::json!({
                    "event_id": event_id,
                    "result": result,
                })
            })
            .collect::<Vec<_>>();
        let output = serde_json::json!({
            "results": projection_results,
            "input_row_count": rows,
            "input_jsonl_sha256": format!("{:x}", input_sha256.finalize()),
            "schema_version": 1,
        });
        let encoded = match serde_json::to_string(&output) {
            Ok(encoded) => encoded,
            Err(error) => {
                eprintln!("check-series-rows: cannot serialize projection results: {error}");
                return ExitCode::from(2);
            }
        };
        println!("{encoded}");
    }
    ExitCode::SUCCESS
}
