#!/usr/bin/env -S rust-script --force
//! Check validation-ledger event envelopes with the producer-owned shared type.
//!
//! Python owns the append-only store and its path/reference checks. The parent
//! repository owns the complete serialized envelope and closed values, so a
//! producer or reader cannot silently omit a required field or invent an event
//! type, producer source, or outcome.
//!
//! ```cargo
//! [dependencies]
//! chrono = "0.4"
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! ```

#![deny(
    dead_code,
    unused_imports,
    unused_mut,
    unused_variables,
    unused_must_use
)]

#[allow(dead_code)]
#[path = "../lib/ledger_event.rs"]
mod ledger_event;

use ledger_event::LedgerEvent;
use std::io::{self, BufRead};
use std::process::ExitCode;

const USAGE: &str = r#"Usage: ci-hub/ledger/check-ledger-events.rs < events.jsonl

Deserialize every nonempty input line as the shared validate-ledger/v1 event
and apply its intrinsic envelope checks. Prints no output on success.
"#;

fn main() -> ExitCode {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if let Some(argument) = arguments.first() {
        eprintln!(
            "check-ledger-events: unexpected argument {:?}\n{USAGE}",
            argument
        );
        return ExitCode::from(2);
    }

    let stdin = io::stdin();
    let mut events = 0usize;
    for (index, line) in stdin.lock().lines().enumerate() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!(
                    "check-ledger-events: cannot read stdin line {}: {error}",
                    index + 1
                );
                return ExitCode::from(2);
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let event = match LedgerEvent::parse(line.as_bytes()) {
            Ok(event) => event,
            Err(error) => {
                eprintln!("check-ledger-events: stdin line {}: {error}", index + 1);
                return ExitCode::FAILURE;
            }
        };
        if let Err(error) = event.validate() {
            eprintln!("check-ledger-events: stdin line {}: {error}", index + 1);
            return ExitCode::FAILURE;
        }
        events += 1;
    }
    if events == 0 {
        eprintln!("check-ledger-events: no events on stdin");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}
