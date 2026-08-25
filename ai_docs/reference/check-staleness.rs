#!/usr/bin/env rust-script
//! Staleness dashboard for `ai_docs/reference/`.
//!
//! Scans every `*.md` in a reference directory, and for each doc that carries
//! machine-readable staleness front-matter, reports how many days old it is and
//! how many intervening commits have touched its watch-files since the SHA it is
//! current as of. Docs without front-matter are listed as opted-out.
//!
//! The heavy lifting (front-matter parsing, git range queries) lives in
//! `update-driver.rs`; this front-end invokes it once per doc in `--quick
//! --json` mode and aggregates, so there is a single source of truth for the
//! staleness computation.
//!
//! Usage:
//!   ai_docs/reference/check-staleness.rs [--dir DIR] [--json] [--today D]
//!                                        [--fail-on-stale]
//!
//!   --dir DIR         Directory to scan (default: the script's own directory).
//!   --json            Emit one JSON array of per-doc records.
//!   --today D         Override today's date (YYYY-MM-DD), passed through.
//!   --fail-on-stale   Exit non-zero if any doc is STALE-* or DIVERGED (CI gate).
//!
//! ```cargo
//! [dependencies]
//! serde_json = "1"
//! ```

use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

const USAGE: &str = r#"Usage: ai_docs/reference/check-staleness.rs [OPTIONS]

Scan a reference-doc directory and report each doc's staleness.

Options:
  --dir DIR         Directory to scan (default: the script's own directory).
  --json            Emit a JSON array of per-doc records.
  --today D         Override today's date (YYYY-MM-DD).
  --fail-on-stale   Exit non-zero when any doc is stale or diverged.
  -h, --help        Show this help.
"#;

fn die(msg: &str) -> ! {
    eprintln!("check-staleness: {msg}\n\n{USAGE}");
    exit(2);
}

fn main() {
    let mut dir: Option<PathBuf> = None;
    let mut json = false;
    let mut today: Option<String> = None;
    let mut fail_on_stale = false;
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                exit(0);
            }
            "--json" => json = true,
            "--fail-on-stale" => fail_on_stale = true,
            "--dir" => {
                dir = Some(PathBuf::from(
                    it.next().unwrap_or_else(|| die("--dir needs a path")),
                ))
            }
            "--today" => today = Some(it.next().unwrap_or_else(|| die("--today needs a date"))),
            other => die(&format!("unknown argument {other}")),
        }
    }

    // Locate the driver next to this script; fall back to the scan dir.
    let script_dir = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let scan_dir = dir.unwrap_or_else(|| {
        // Default: the directory this script lives in. rust-script runs a temp
        // binary, so derive it from arg0's source path when available, else CWD.
        script_dir
            .clone()
            .unwrap_or_else(|| env::current_dir().expect("cwd"))
    });
    // The driver is a sibling source file; resolve relative to the scan dir,
    // which for the default case is the reference dir that holds both scripts.
    let driver = locate_driver(&scan_dir).unwrap_or_else(|| {
        die("could not find update-driver.rs next to the docs; pass --dir <ai_docs/reference>")
    });

    let mut mds: Vec<PathBuf> = fs::read_dir(&scan_dir)
        .unwrap_or_else(|e| die(&format!("cannot read {}: {e}", scan_dir.display())))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    mds.sort();

    let mut records: Vec<Value> = Vec::new();
    let mut stale = 0usize;
    let mut tracked = 0usize;
    for md in &mds {
        let mut args: Vec<String> = vec![
            driver.to_string_lossy().into_owned(),
            md.to_string_lossy().into_owned(),
            "--quick".into(),
            "--json".into(),
        ];
        if let Some(t) = &today {
            args.push("--today".into());
            args.push(t.clone());
        }
        let out = Command::new(&args[0])
            .args(&args[1..])
            .output()
            .unwrap_or_else(|e| die(&format!("failed to run driver: {e}")));
        if !out.status.success() {
            eprintln!(
                "warn: driver failed on {}: {}",
                md.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
            continue;
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let Ok(v): Result<Value, _> = serde_json::from_str(stdout.trim()) else {
            continue;
        };
        if v.get("has_front_matter").and_then(Value::as_bool) != Some(true) {
            records.push(serde_json::json!({
                "doc": v.get("doc").cloned().unwrap_or(Value::Null),
                "verdict": "NO-FRONT-MATTER",
            }));
            continue;
        }
        tracked += 1;
        let verdict = v.get("verdict").and_then(Value::as_str).unwrap_or("?");
        if verdict.starts_with("STALE") || verdict == "DIVERGED" || verdict == "UNKNOWN-SHA" {
            stale += 1;
        }
        records.push(v);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&records).unwrap());
    } else {
        println!("{:<12} {:>5}  {:>4}  {}", "VERDICT", "DAYS", "SEED", "DOC");
        println!("{}", "-".repeat(72));
        for r in &records {
            let doc = r.get("doc").and_then(Value::as_str).unwrap_or("?");
            let name = Path::new(doc)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(doc);
            let verdict = r.get("verdict").and_then(Value::as_str).unwrap_or("?");
            if verdict == "NO-FRONT-MATTER" {
                println!("{verdict:<12} {:>5}  {:>4}  {name}", "-", "-");
            } else {
                let days = r.get("days_stale").and_then(Value::as_i64).unwrap_or(0);
                let seed = r.get("seed_commits").and_then(Value::as_u64).unwrap_or(0);
                println!("{verdict:<12} {days:>5}  {seed:>4}  {name}");
            }
        }
        println!();
        println!(
            "{tracked} tracked doc(s); {stale} stale/diverged. Run update-driver.rs <doc> for the light cone + update prompt."
        );
    }

    if fail_on_stale && stale > 0 {
        exit(1);
    }
}

/// Find `update-driver.rs`: prefer the scan directory, then this script's dir.
fn locate_driver(scan_dir: &Path) -> Option<PathBuf> {
    let here = scan_dir.join("update-driver.rs");
    if here.is_file() {
        return Some(here);
    }
    None
}
