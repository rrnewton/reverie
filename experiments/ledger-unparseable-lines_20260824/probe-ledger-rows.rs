#!/usr/bin/env -S rust-script --force
//! DIAGNOSTIC ONLY. Read-only.
//!
//! `ci-hub validate-status` prints "skipped N unparseable ledger line(s)" and
//! throws the reason away: `validate_status::parse_ledger` matches
//! `Err(_) => skipped += 1`. So the operator is told a count and nothing else.
//!
//! This reproduces that parse against the SAME struct the real reader uses --
//! `records::HistoryRow`, included by `#[path]` from the live source, not a
//! copy -- and prints the serde error and the offending row for each line it
//! cannot read. It writes nothing and touches no ledger file.
//!
//! usage: probe-ledger-rows.rs < rows.jsonl
//!   where rows.jsonl is the output of `python3 ci-hub/ledger/validate_rows.py rows`
//!
//! ```cargo
//! [dependencies]
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! ```

#[path = "/home/newton/work/dev-hermit/ci-hub/lib/records.rs"]
mod records;

use std::io::Read;

fn main() {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .expect("read stdin");

    let mut total = 0usize;
    let mut skipped = 0usize;

    for (idx, line) in buf.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        total += 1;
        // Exactly what validate_status::parse_ledger does, except that the
        // error is kept instead of discarded.
        match serde_json::from_str::<records::HistoryRow>(line) {
            Ok(_) => {}
            Err(error) => {
                skipped += 1;
                println!("=== line {} UNPARSEABLE ===", idx + 1);
                println!("  serde error: {error}");
                // Surface the identity fields an operator would need to decide
                // whether this row would have changed a validation answer.
                match serde_json::from_str::<serde_json::Value>(line) {
                    Ok(v) => {
                        for key in [
                            "schema_version",
                            "run_id",
                            "commit",
                            "result",
                            "repo",
                            "host",
                            "started_at",
                            "finished_at",
                            "profile",
                            "selection_mode",
                            "source",
                            "exit_code",
                        ] {
                            if let Some(found) = v.get(key) {
                                println!("  {key}: {found}");
                            }
                        }
                        if let Some(obj) = v.as_object() {
                            let mut keys: Vec<&String> = obj.keys().collect();
                            keys.sort();
                            println!("  all keys: {keys:?}");
                        }
                    }
                    Err(_) => println!("  (line is not even valid JSON)"),
                }
                println!();
            }
        }
    }

    println!("total non-empty lines: {total}");
    println!("unparseable as HistoryRow: {skipped}");
}
