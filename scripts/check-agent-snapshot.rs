#!/usr/bin/env rust-script
//! Validate and describe the shared agent-snapshot schema.
//!
//! ```cargo
//! [dependencies]
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! sha2 = "0.10"
//! ```

// rust-script keys its compiled cache on this entrypoint, but does not notice
// changes in the #[path] dependency below. Keep this marker synchronized with
// status-contract changes so a deployed contract cannot keep using old code.
const CONTRACT_SOURCE_REVISION: &str = "schema-1-pane-identities-roles-authority-v6";

#[path = "agent_snapshot.rs"]
mod agent_snapshot;

use serde_json::json;
use std::env;
use std::io::{self, Read};
use std::process::exit;

const USAGE: &str = r#"Usage: check-agent-snapshot.rs --validate-stdin
       check-agent-snapshot.rs --classify-status STATUS

--validate-stdin validates one complete stored agent-snapshot document from
standard input. --classify-status prints the shared live/busy/broken meaning of
one accepted status for language-boundary consumers.
"#;

fn die(message: &str) -> ! {
    eprintln!("agent-snapshot: ERROR {message}");
    exit(2);
}

fn main() {
    let _contract_source_revision = CONTRACT_SOURCE_REVISION;
    let arguments: Vec<String> = env::args().skip(1).collect();
    match arguments.as_slice() {
        [argument] if argument == "-h" || argument == "--help" => print!("{USAGE}"),
        [argument] if argument == "--validate-stdin" => {
            let mut bytes = Vec::new();
            io::stdin()
                .read_to_end(&mut bytes)
                .unwrap_or_else(|error| die(&format!("read standard input: {error}")));
            agent_snapshot::AgentSnapshot::parse(&bytes).unwrap_or_else(|error| die(&error));
        }
        [argument, value] if argument == "--classify-status" => {
            let status =
                agent_snapshot::AgentStatus::parse(value).unwrap_or_else(|error| die(&error));
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "schema_version": agent_snapshot::SCHEMA_VERSION,
                    "status": status.as_str(),
                    "live": status.is_live(),
                    "busy": status.is_busy(),
                    "broken": status.is_broken(),
                }))
                .unwrap_or_else(|error| die(&format!("serialize status classification: {error}")))
            );
        }
        _ => die(USAGE.trim()),
    }
}
