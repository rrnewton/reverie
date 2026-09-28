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

const USAGE: &str = r#"Usage: ci-hub/series/check-series-rows.rs [--read | --projection-results | --write-results] < rows.jsonl

Deserialize every proposed row as Hermit's shared stress-series type. The
default applies write-boundary checks. With --read, retained v1 rows remain
readable while v2 and v3 rows must pass their complete schema-specific read
checks. With --projection-results, apply the read checks and emit one exact
validate_for_projection result per event_id, plus a SHA-256 digest of the
nonempty input lines with one LF terminator per line. With --write-results,
apply the write-boundary checks to every row without stopping at the first
refusal and emit one result per nonempty input line, in input order: null
when the row is admitted, or the exact refusal. The same digest is included.
Other successful modes print no output.
"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Write,
    Read,
    ProjectionResults,
    WriteResults,
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
        [argument] if argument == "--write-results" => Mode::WriteResults,
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
    let mut write_results: Vec<(usize, Option<String>)> = Vec::new();
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
        if matches!(mode, Mode::ProjectionResults | Mode::WriteResults) {
            input_sha256.update(line.as_bytes());
            input_sha256.update(b"\n");
        }
        if mode == Mode::WriteResults {
            // Every row gets its own verdict; one refusal must not decide the
            // fate of the independent rows beside it.
            let verdict = match serde_json::from_str::<SeriesRow>(&line) {
                Ok(row) => row.validate_for_write().err(),
                Err(error) => Some(format!("does not match the shared SeriesRow: {error}")),
            };
            write_results.push((index + 1, verdict));
            rows += 1;
            continue;
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
            Mode::WriteResults => unreachable!("write results are handled above"),
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
    let input_digest = format!("{:x}", input_sha256.finalize());
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
            "input_jsonl_sha256": input_digest,
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
    if mode == Mode::WriteResults {
        let results = write_results
            .into_iter()
            .map(|(line, result)| serde_json::json!({"line": line, "result": result}))
            .collect::<Vec<_>>();
        let output = serde_json::json!({
            "results": results,
            "input_row_count": rows,
            "input_jsonl_sha256": input_digest,
            "schema_version": 1,
        });
        let encoded = match serde_json::to_string(&output) {
            Ok(encoded) => encoded,
            Err(error) => {
                eprintln!("check-series-rows: cannot serialize write results: {error}");
                return ExitCode::from(2);
            }
        };
        println!("{encoded}");
    }
    ExitCode::SUCCESS
}
