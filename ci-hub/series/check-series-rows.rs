#!/usr/bin/env -S rust-script --force
//! Check proposed stress-series rows with Hermit's shared serialized type.
//!
//! ```cargo
//! [dependencies]
//! hermit-manifest-plan = { path = "../../hermit/ci/manifest-plan" }
//! serde_json = "1"
//! ```

#![deny(
    dead_code,
    unused_imports,
    unused_mut,
    unused_variables,
    unused_must_use
)]

use hermit_manifest_plan::stress_series::SeriesRow;
use std::io::{self, BufRead};
use std::process::ExitCode;

const USAGE: &str = r#"Usage: ci-hub/series/check-series-rows.rs [--read] < rows.jsonl

Deserialize every proposed row as Hermit's shared stress-series type and apply
its write-boundary checks. With --read, retained v1 rows are accepted while v2
rows must carry their complete current-schema facts. Prints no output on success.
"#;

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let read = arguments.as_slice() == ["--read"];
    if !arguments.is_empty() && !read {
        eprintln!(
            "check-series-rows: unexpected argument {:?}\n{USAGE}",
            arguments[0]
        );
        return ExitCode::from(2);
    }

    let stdin = io::stdin();
    let mut rows = 0usize;
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
        let validation = if read {
            row.validate_for_read()
        } else {
            row.validate_for_write()
        };
        if let Err(error) = validation {
            eprintln!("check-series-rows: stdin line {}: {error}", index + 1);
            return ExitCode::FAILURE;
        }
        rows += 1;
    }
    if rows == 0 {
        eprintln!("check-series-rows: no rows on stdin");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}
