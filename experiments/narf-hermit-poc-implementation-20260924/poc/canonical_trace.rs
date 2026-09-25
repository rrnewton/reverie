//! Fail-closed canonical trace helper for the bounded Hermit-on-Narf POC.
//!
//! Build with `rustc canonical_trace.rs -o canonical-trace`. The helper writes
//! canonical bytes only after validating the complete expected trace.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;

const PREFIX: &str = "INFO narf-hermit-canonical-v1 ";
const WRITE_ADDRESS: u64 = 0x0000_0080_0000_3000;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("canonical-trace: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or_else(usage)?;
    match command.as_str() {
        "canonicalize-hermit" | "canonicalize-narf" | "mutate" => {
            let input = args.next().ok_or_else(usage)?;
            if args.next().is_some() {
                return Err(usage());
            }
            let bytes = fs::read(&input).map_err(|error| format!("read {input}: {error}"))?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|error| format!("{input} is not UTF-8: {error}"))?;
            let canonical = match command.as_str() {
                "canonicalize-hermit" => canonicalize_hermit(text)?,
                "canonicalize-narf" => canonicalize_narf(text)?,
                "mutate" => mutate(text)?,
                _ => unreachable!(),
            };
            io::stdout()
                .write_all(canonical.as_bytes())
                .map_err(|error| format!("write canonical stdout: {error}"))?;
        }
        "compare" => {
            let left = args.next().ok_or_else(usage)?;
            let right = args.next().ok_or_else(usage)?;
            if args.next().is_some() {
                return Err(usage());
            }
            compare(&left, &right)?;
        }
        _ => return Err(usage()),
    }
    Ok(())
}

fn usage() -> String {
    "usage: canonical-trace canonicalize-{hermit|narf} INPUT | compare LEFT RIGHT | mutate INPUT"
        .to_owned()
}

fn canonicalize_hermit(input: &str) -> Result<String, String> {
    let mut records = Vec::new();
    for line in input.lines() {
        if let Some(event) = line.split_once("inbound syscall: ").map(|(_, event)| event) {
            let event = event
                .split_once(" DETLOG_RECORD=")
                .map_or(event, |(event, _)| event);
            if let Some(arguments) = event
                .strip_prefix("write(")
                .and_then(|rest| rest.strip_suffix(") = ?"))
            {
                let values = parse_csv(arguments, 3)?;
                let fd = parse_decimal(values[0], "write fd")?;
                let address = parse_hex(values[1], "write address")?;
                let length = parse_decimal(values[2], "write length")?;
                records.push(format!(
                    "{PREFIX}seq=0 phase=enter nr=1 a0={fd} a1=0x{address:016x} a2={length}"
                ));
            } else if let Some(argument) = event
                .strip_prefix("exit(")
                .and_then(|rest| rest.strip_suffix(") = ?"))
            {
                let status = parse_decimal(argument, "exit status")?;
                records.push(format!("{PREFIX}seq=1 phase=enter nr=60 a0={status}"));
            } else {
                return Err(format!("unsupported Hermit syscall record: {event}"));
            }
        }

        if line.contains("finish syscall") && line.contains(": write(") {
            let result = line
                .split_once(" = Ok(")
                .and_then(|(_, suffix)| suffix.split_once(')'))
                .map(|(value, _)| value)
                .ok_or_else(|| format!("malformed Hermit write completion: {line}"))?;
            let result = parse_decimal(result, "write result")?;
            records.push(format!("{PREFIX}seq=0 phase=return result={result}"));
        }
    }
    normalize_and_validate(records)
}

fn canonicalize_narf(input: &str) -> Result<String, String> {
    let records = input
        .lines()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            line.find(PREFIX).map(|offset| line[offset..].to_owned())
        })
        .collect();
    normalize_and_validate(records)
}

fn normalize_and_validate(records: Vec<String>) -> Result<String, String> {
    if records.len() != 3 {
        return Err(format!(
            "expected exactly 3 canonical records, observed {}",
            records.len()
        ));
    }
    let expected = [
        format!("{PREFIX}seq=0 phase=enter nr=1 a0=1 a1=0x{WRITE_ADDRESS:016x} a2=16"),
        format!("{PREFIX}seq=0 phase=return result=16"),
        format!("{PREFIX}seq=1 phase=enter nr=60 a0=0"),
    ];
    for (index, (actual, expected)) in records.iter().zip(expected.iter()).enumerate() {
        if actual != expected {
            return Err(format!(
                "record {index} mismatch\nexpected: {expected}\nactual:   {actual}"
            ));
        }
    }
    Ok(records.join("\n") + "\n")
}

fn parse_csv(input: &str, count: usize) -> Result<Vec<&str>, String> {
    let values: Vec<_> = input.split(',').map(str::trim).collect();
    if values.len() != count {
        return Err(format!(
            "expected {count} comma-separated values in {input:?}, observed {}",
            values.len()
        ));
    }
    Ok(values)
}

fn parse_decimal(input: &str, field: &str) -> Result<u64, String> {
    input
        .parse()
        .map_err(|error| format!("invalid {field} {input:?}: {error}"))
}

fn parse_hex(input: &str, field: &str) -> Result<u64, String> {
    let digits = input
        .strip_prefix("0x")
        .ok_or_else(|| format!("{field} lacks 0x prefix: {input:?}"))?;
    u64::from_str_radix(digits, 16).map_err(|error| format!("invalid {field} {input:?}: {error}"))
}

fn compare(left: &str, right: &str) -> Result<(), String> {
    let left_bytes = fs::read(left).map_err(|error| format!("read {left}: {error}"))?;
    let right_bytes = fs::read(right).map_err(|error| format!("read {right}: {error}"))?;
    if left_bytes == right_bytes {
        return Ok(());
    }
    let offset = left_bytes
        .iter()
        .zip(&right_bytes)
        .position(|(left, right)| left != right)
        .unwrap_or(left_bytes.len().min(right_bytes.len()));
    Err(format!(
        "canonical traces differ at byte {offset} (left {} bytes, right {} bytes)",
        left_bytes.len(),
        right_bytes.len()
    ))
}

fn mutate(input: &str) -> Result<String, String> {
    let valid = normalize_and_validate(input.lines().map(str::to_owned).collect())?;
    let needle = "phase=return result=16";
    if valid.matches(needle).count() != 1 {
        return Err("canonical input does not contain one mutable result".to_owned());
    }
    Ok(valid.replacen(needle, "phase=return result=15", 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_trace_validates_and_mutation_is_rejected() {
        let trace = normalize_and_validate(vec![
            format!("{PREFIX}seq=0 phase=enter nr=1 a0=1 a1=0x{WRITE_ADDRESS:016x} a2=16"),
            format!("{PREFIX}seq=0 phase=return result=16"),
            format!("{PREFIX}seq=1 phase=enter nr=60 a0=0"),
        ])
        .unwrap();
        let changed = mutate(&trace).unwrap();
        assert_ne!(trace.as_bytes(), changed.as_bytes());
        assert!(normalize_and_validate(changed.lines().map(str::to_owned).collect()).is_err());

        let mut extra: Vec<_> = trace.lines().map(str::to_owned).collect();
        extra.push(format!("{PREFIX}seq=2 phase=enter nr=39"));
        assert!(normalize_and_validate(extra).is_err());
    }
}
