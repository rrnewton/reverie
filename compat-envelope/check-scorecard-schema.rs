#!/usr/bin/env rust-script
//! Check the shared compatibility scorecard row type against its JSON projection
//! and every published CSV.
//!
//! ```cargo
//! [dependencies]
//! serde_json = "1"
//! ```

#![deny(
    dead_code,
    unused_imports,
    unused_mut,
    unused_variables,
    unused_must_use
)]

#[path = "lib/scorecard.rs"]
mod scorecard;

use scorecard::{
    column_names, core_column_names, extra_column_names, split_csv_line, ScorecardRow,
    ScorecardVariant,
};
use serde_json::Value;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;

const USAGE: &str = r#"Usage: compat-envelope/check-scorecard-schema.rs [--root PATH]
       compat-envelope/check-scorecard-schema.rs --header FILE
       compat-envelope/check-scorecard-schema.rs --file PATH --variant FILE
       compat-envelope/check-scorecard-schema.rs --self-check

Check that scorecard-schema.json is the exact projection of the shared Rust
ScorecardRow and that every published scorecard has its declared row shape.
"#;

fn die(message: &str) -> ! {
    eprintln!("check-scorecard-schema: {message}");
    exit(2);
}

fn strings(value: Option<&Value>, what: &str) -> Result<Vec<String>, String> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{what} is not an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{what} contains a non-string"))
        })
        .collect()
}

fn check_csv(path: &Path, variant: ScorecardVariant) -> Result<usize, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut lines = text.lines();
    let header_line = lines
        .next()
        .ok_or_else(|| format!("{} is empty", path.display()))?;
    let header = split_csv_line(header_line)
        .map_err(|error| format!("{} header: {error}", path.display()))?;
    let expected = column_names(&variant.columns());
    if header != expected {
        return Err(format!(
            "{} header differs from shared {:?} row: got {} columns, expected {}",
            path.display(),
            variant,
            header.len(),
            expected.len()
        ));
    }
    let mut rows = 0usize;
    for (index, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_csv_line(line)
            .map_err(|error| format!("{}:{}: {error}", path.display(), index + 2))?;
        ScorecardRow::parse(&header, &fields, None)
            .map_err(|error| format!("{}:{}: {error}", path.display(), index + 2))?;
        rows += 1;
    }
    Ok(rows)
}

fn check(root: &Path) -> Result<(), Vec<String>> {
    let envelope = root.join("compat-envelope");
    let schema_path = envelope.join("scorecard-schema.json");
    let schema: Value = fs::read_to_string(&schema_path)
        .map_err(|error| vec![format!("cannot read {}: {error}", schema_path.display())])
        .and_then(|text| {
            serde_json::from_str(&text)
                .map_err(|error| vec![format!("{} is malformed: {error}", schema_path.display())])
        })?;
    let mut problems = Vec::new();
    let core = match strings(schema.get("core"), "core") {
        Ok(core) => core,
        Err(error) => return Err(vec![error]),
    };
    let expected_core = core_column_names();
    if core != expected_core {
        problems.push(format!(
            "scorecard-schema.json core differs from ScorecardRow: got {} columns, expected {}",
            core.len(),
            expected_core.len()
        ));
    }
    let variants = match schema.get("variants").and_then(Value::as_object) {
        Some(variants) => variants,
        None => {
            return Err(vec![
                "scorecard-schema.json variants is not an object".to_string()
            ])
        }
    };

    let expected_names: BTreeSet<&str> = ScorecardVariant::PUBLISHED
        .iter()
        .map(|variant| variant.file_name())
        .collect();
    let declared_names: BTreeSet<&str> = variants.keys().map(String::as_str).collect();
    if expected_names != declared_names {
        problems.push(format!(
            "scorecard-schema.json variant set differs from ScorecardVariant: declared={declared_names:?} expected={expected_names:?}"
        ));
    }

    for variant in ScorecardVariant::PUBLISHED {
        let name = variant.file_name();
        let Some(definition) = variants.get(name) else {
            continue;
        };
        let expected_columns = column_names(&variant.columns());
        match strings(
            definition.get("columns"),
            &format!("variants.{name}.columns"),
        ) {
            Ok(columns) if columns == expected_columns => {}
            Ok(columns) => problems.push(format!(
                "{name}: JSON columns differ from shared {:?} row: got {}, expected {}",
                variant,
                columns.len(),
                expected_columns.len()
            )),
            Err(error) => problems.push(error),
        }
        let expected_extras = extra_column_names(*variant);
        match strings(
            definition.get("extra_columns"),
            &format!("variants.{name}.extra_columns"),
        ) {
            Ok(extras) if extras == expected_extras => {}
            Ok(extras) => problems.push(format!(
                "{name}: extra_columns differ from shared {:?} row: got {extras:?}, expected {expected_extras:?}",
                variant
            )),
            Err(error) => problems.push(error),
        }
        if definition
            .get("omits_core")
            .and_then(Value::as_array)
            .is_none_or(|v| !v.is_empty())
        {
            problems.push(format!(
                "{name}: every shared row variant must carry the complete core"
            ));
        }
        if definition.get("core_prefix_safe").and_then(Value::as_bool) != Some(true) {
            problems.push(format!("{name}: core_prefix_safe must be true"));
        }
        if definition.get("emitter").and_then(Value::as_str) != Some(variant.emitter()) {
            problems.push(format!(
                "{name}: emitter differs from shared {:?} definition",
                variant
            ));
        }

        let path = envelope.join(name);
        if path.exists() {
            match check_csv(&path, *variant) {
                Ok(rows) => println!(
                    "  {:<30} -> {name} ({rows} rows, {} columns) OK",
                    variant.emitter(),
                    expected_columns.len()
                ),
                Err(error) => problems.push(error),
            }
        } else {
            println!(
                "  {:<30} -> {name} (target absent; {} columns) NOT YET WRITTEN",
                variant.emitter(),
                expected_columns.len()
            );
        }
    }

    let expected_emitters: BTreeSet<&str> = ScorecardVariant::PUBLISHED
        .iter()
        .map(|variant| variant.emitter())
        .collect();
    let discovered_emitters: BTreeSet<String> = fs::read_dir(&envelope)
        .map_err(|error| vec![format!("cannot enumerate {}: {error}", envelope.display())])?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            (name.starts_with("collect-")
                && matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("rs" | "sh")
                ))
            .then(|| name.to_string())
        })
        .collect();
    let expected_collectors: BTreeSet<String> = expected_emitters
        .iter()
        .filter(|name| name.starts_with("collect-"))
        .map(|name| (*name).to_string())
        .collect();
    if discovered_emitters != expected_collectors {
        problems.push(format!(
            "scorecard emitter population differs from ScorecardVariant: discovered={discovered_emitters:?} expected={expected_collectors:?}"
        ));
    }
    for emitter in expected_emitters {
        let path = envelope.join(emitter);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                problems.push(format!(
                    "cannot read declared emitter {}: {error}",
                    path.display()
                ));
                continue;
            }
        };
        let derives = if emitter.ends_with(".rs") {
            text.contains("#[path = \"lib/scorecard.rs\"]") && !text.contains("const HEADER: &str")
        } else {
            text.contains("scorecard-schema.json")
        };
        if !derives {
            problems.push(format!(
                "{emitter}: does not derive its row shape from the shared ScorecardRow projection"
            ));
        }
    }
    let renderer = fs::read_to_string(envelope.join("render-scorecard.rs")).unwrap_or_default();
    if !renderer.contains("#[path = \"lib/scorecard.rs\"]") {
        problems.push("render-scorecard.rs does not import the shared ScorecardRow".to_string());
    }

    let history = envelope.join("history/scorecard-observations.csv");
    if history.exists() {
        match check_csv(&history, ScorecardVariant::Scorecard) {
            Ok(rows) => println!(
                "  {:<30} -> history/scorecard-observations.csv ({rows} rows, {} columns) OK",
                "publish-scorecard.py",
                ScorecardVariant::Scorecard.columns().len()
            ),
            Err(error) => problems.push(error),
        }
    }

    let mut on_disk = BTreeSet::new();
    let entries = fs::read_dir(&envelope)
        .map_err(|error| vec![format!("cannot enumerate {}: {error}", envelope.display())])?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.contains("scorecard") && name.ends_with(".csv") {
            on_disk.insert(name.to_string());
        }
    }
    for name in on_disk {
        if ScorecardVariant::for_file_name(&name).is_none() {
            problems.push(format!(
                "{name}: published scorecard has no ScorecardVariant; an untyped file cannot pass"
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn self_check() -> Result<(), String> {
    let variant = ScorecardVariant::FullCorpus;
    let header = column_names(&variant.columns());
    let row = ScorecardRow::from_pairs([
        (scorecard::ScorecardColumn::RunId, "fixture".to_string()),
        (
            scorecard::ScorecardColumn::Reason,
            "comma, quote \" and newline\n".to_string(),
        ),
    ]);
    let encoded = row.encode(variant);
    let fields = split_csv_line(&encoded)?;
    let parsed = ScorecardRow::parse(&header, &fields, None)?;
    if parsed != row {
        return Err("shared row did not survive CSV round trip".to_string());
    }
    if ScorecardRow::parse(&header, &fields[..fields.len() - 1], None).is_ok() {
        return Err("short row was accepted".to_string());
    }
    let mut duplicate = header.clone();
    duplicate[1] = duplicate[0].clone();
    if ScorecardRow::parse(&duplicate, &fields, None).is_ok() {
        return Err("duplicate logical column was accepted".to_string());
    }
    let mut unknown = header;
    unknown[0] = "unknown_column".to_string();
    if ScorecardRow::parse(&unknown, &fields, None).is_ok() {
        return Err("unknown column was accepted".to_string());
    }
    Ok(())
}

fn main() {
    let mut root = PathBuf::from(".");
    let mut header = None;
    let mut file = None;
    let mut file_variant = None;
    let mut run_self_check = false;
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            "--root" => {
                root = PathBuf::from(args.next().unwrap_or_else(|| die("--root needs a path")))
            }
            "--header" => {
                header = Some(
                    args.next()
                        .unwrap_or_else(|| die("--header needs a file name")),
                )
            }
            "--file" => {
                file = Some(PathBuf::from(
                    args.next().unwrap_or_else(|| die("--file needs a path")),
                ))
            }
            "--variant" => {
                file_variant = Some(
                    args.next()
                        .unwrap_or_else(|| die("--variant needs a scorecard file name")),
                )
            }
            "--self-check" => run_self_check = true,
            other => die(&format!("unknown argument {other:?}")),
        }
    }

    if let Some(name) = header {
        let variant = ScorecardVariant::for_file_name(&name)
            .unwrap_or_else(|| die(&format!("unknown scorecard file {name:?}")));
        println!("{}", variant.header());
        return;
    }
    if file.is_some() || file_variant.is_some() {
        let path = file.unwrap_or_else(|| die("--file and --variant must be used together"));
        let name =
            file_variant.unwrap_or_else(|| die("--file and --variant must be used together"));
        let variant = ScorecardVariant::for_file_name(&name)
            .unwrap_or_else(|| die(&format!("unknown scorecard file {name:?}")));
        let rows = check_csv(&path, variant)
            .unwrap_or_else(|error| die(&format!("shared row check failed: {error}")));
        println!(
            "check-scorecard-schema: {} rows match the shared {:?} row",
            rows, variant
        );
        return;
    }
    if run_self_check {
        self_check().unwrap_or_else(|error| die(&format!("self-check failed: {error}")));
        println!("check-scorecard-schema self-check: PASS");
        return;
    }

    match check(&root) {
        Ok(()) => println!(
            "OK: scorecard-schema.json and every published CSV derive from the shared ScorecardRow"
        ),
        Err(problems) => {
            eprintln!("REFUSED:");
            for problem in problems {
                eprintln!("  {problem}");
            }
            exit(1);
        }
    }
}
