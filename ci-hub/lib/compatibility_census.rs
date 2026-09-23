//! Public comparison operands derived by a writer that held every raw census
//! member. Public readers verify this derived object, not private raw preimages.

use crate::records::{
    CellBackendParity, CellBackendParitySummary, CellIdentity, CellVerdict, HistoryRow,
};
use hermit_manifest_plan::ledger::{
    cell_verdict_from_source, compact_cell_verdict, read_schema10_source_result,
    validate_ordinary_verdict, ComparisonObservationVerdictV10, RequiredNullable,
};
use hermit_manifest_plan::runner::{CellResult, FailureClass, ObservedResult};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const LIMIT: u64 = 256 * 1024 * 1024;
pub(crate) const AUTHORITY: &str = "writer_verified_raw_census_v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DerivedAttempt {
    pub input_path: String,
    pub input_line: u64,
    pub cell: CellIdentity,
    pub attempt: u64,
    pub source_row_sha256: String,
    pub outcome: String,
    pub result: Option<ObservedResult>,
    pub failure_class: Option<FailureClass>,
    pub ordinary_verdict: CellVerdict,
    pub backend_parity: Option<CellBackendParitySummary>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DerivedCensus {
    pub schema: u32,
    pub authority: String,
    pub run_id: String,
    pub record_id: String,
    pub hermit_sha: String,
    pub history_binding_sha256: String,
    pub raw_census_sha256: String,
    pub attempts: Vec<DerivedAttempt>,
}

fn canonical(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("JSON values serialize")
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn json_digest(value: &Value) -> String {
    digest(&canonical(value))
}

fn history_binding(raw: &Value) -> Value {
    // These original fields are not changed by the existing path portability
    // transform. The carrier separately authenticates the complete parent event.
    let mut binding = Map::new();
    for field in [
        "schema_version",
        "run_id",
        "record_id",
        "commit",
        "tree_dirty",
        "commit_anchored",
        "profile",
        "selection_mode",
        "started_at",
        "finished_at",
        "result",
        "exit_code",
        "validation_complete",
        "raw_result_input_census_v1",
    ] {
        binding.insert(field.into(), raw.get(field).cloned().unwrap_or(Value::Null));
    }
    Value::Object(binding)
}

fn original_census(
    row: &HistoryRow,
) -> Result<hermit_manifest_plan::ledger::RawResultInputCensusV1, String> {
    if row.schema_version != Some(5)
        || row.cell_results.is_some()
        || !matches!(row.repo.as_deref(), Some("hermit" | "rrnewton/hermit"))
        || row.profile.as_deref() != Some("full")
        || row.selection_mode.as_deref() != Some("full")
        || row.commit_anchored != Some(true)
        || row.tree_dirty != Some(false)
        || row.started_at.as_deref().is_none_or(str::is_empty)
        || row.finished_at.as_deref().is_none_or(str::is_empty)
        || !matches!(row.result.as_deref(), Some("pass" | "fail" | "no_result"))
    {
        return Err("derived census requires an original terminal clean schema-5 full run".into());
    }
    let census = row
        .raw_result_input_census_v1()?
        .ok_or("original row has no raw census")?;
    if census.files.iter().all(|file| file.rows.is_empty()) {
        return Err("an empty raw census cannot establish schema-5 selected-zero evidence".into());
    }
    Ok(census)
}

fn row_identity(raw: &Value, field: &str) -> Result<String, String> {
    raw.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("original row omits {field}"))
}

impl DerivedCensus {
    pub(crate) fn validate(&self, row: &HistoryRow, raw: &Value) -> Result<(), String> {
        let census = original_census(row)?;
        if self.schema != 1
            || self.authority != AUTHORITY
            || self.run_id != census.run_id
            || self.hermit_sha != census.hermit_sha
            || self.record_id != row_identity(raw, "record_id")?
            || self.history_binding_sha256 != json_digest(&history_binding(raw))
            || self.raw_census_sha256 != json_digest(&raw["raw_result_input_census_v1"])
        {
            return Err(
                "derived comparison object differs from its original terminal/census".into(),
            );
        }
        let expected = census
            .files
            .iter()
            .flat_map(|file| {
                file.rows
                    .iter()
                    .map(|r| ((file.path.clone(), r.line), (r.cell.clone(), r.attempt)))
            })
            .collect::<Vec<_>>();
        if expected.len() != self.attempts.len() {
            return Err("derived comparison object omits or adds raw attempts".into());
        }
        let mut seen = BTreeSet::new();
        for (attempt, expected) in self.attempts.iter().zip(expected) {
            let key = (attempt.input_path.clone(), attempt.input_line);
            if !seen.insert(key.clone())
                || expected != (key, (attempt.cell.clone(), attempt.attempt))
            {
                return Err("derived comparison attempt differs from the original census".into());
            }
            if attempt.source_row_sha256.len() != 64
                || !attempt
                    .source_row_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !matches!(attempt.outcome.as_str(), "PASS" | "FAIL" | "ERROR" | "SKIP")
            {
                return Err("derived comparison attempt has invalid source/result identity".into());
            }
            validate_ordinary_verdict(&attempt.cell, &attempt.ordinary_verdict)?;
            if attempt.ordinary_verdict != compact_cell_verdict(&attempt.ordinary_verdict) {
                return Err("derived ordinary verdict has a noncanonical public reason".into());
            }
            if let Some(parity) = &attempt.backend_parity {
                if parity.reference_backend != "ptrace"
                    || parity.attempts.len() != 1
                    || parity.attempts[0].attempt != attempt.attempt
                {
                    return Err("derived parity operand has a different outer attempt".into());
                }
                let operand = &parity.attempts[0];
                if operand.candidate != attempt.ordinary_verdict {
                    return Err("derived parity candidate differs from ordinary verdict".into());
                }
                validate_ordinary_verdict(&attempt.cell, &operand.candidate)?;
                if let RequiredNullable::Value(reference) = &operand.reference {
                    validate_ordinary_verdict(
                        &CellIdentity {
                            backend: "ptrace".into(),
                            ..attempt.cell.clone()
                        },
                        reference,
                    )?;
                    if *reference != compact_cell_verdict(reference) {
                        return Err("derived parity reference has noncanonical reason".into());
                    }
                }
                for value in [
                    &operand.candidate_verification_report_sha256,
                    &operand.reference_verification_report_sha256,
                ] {
                    if matches!(value, RequiredNullable::Value(d) if d.len() != 64 || !d.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                    {
                        return Err("derived parity operand has malformed report digest".into());
                    }
                }
                match &operand.cross {
                    ComparisonObservationVerdictV10::Matched | ComparisonObservationVerdictV10::Diverged => {
                        if !matches!(operand.candidate, CellVerdict::ComparedAndMatched { .. })
                            || !matches!(operand.reference, RequiredNullable::Value(CellVerdict::ComparedAndMatched { .. }))
                            || !matches!(operand.candidate_verification_report_sha256, RequiredNullable::Value(_))
                            || !matches!(operand.reference_verification_report_sha256, RequiredNullable::Value(_)) {
                            return Err("derived cross comparison has no canonical candidate/reference operands".into());
                        }
                    }
                    ComparisonObservationVerdictV10::UnavailableWithReason { reason } => {
                        if reason != "Cross-backend comparison unavailable; exact detail is retained in the cell artifact" {
                            return Err("derived cross comparison has noncanonical reason".into());
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Verify the original byte census before interpreting or emitting any public
/// operands. The retained source files are re-read through held descriptors and
/// their original names before returning, so source replacement fails closed.
pub(crate) fn derive(row_bytes: &[u8], root: &Path) -> Result<DerivedCensus, String> {
    let raw = read_schema10_source_result(row_bytes)?;
    let row: HistoryRow = serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
    let census = original_census(&row)?;
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(root)
        .map_err(|e| format!("cannot open raw result root: {e}"))?;
    let mut inputs = BTreeMap::new();
    let mut held = Vec::new();
    let mut remaining = LIMIT;
    for input in &census.files {
        if input.bytes > remaining {
            return Err("raw census exceeds existing 256MiB reader capacity".into());
        }
        let mut file = crate::validation_evidence::open_relative_path(
            &directory,
            Path::new(&input.path),
            false,
        )
        .map_err(|e| format!("cannot open raw census member: {e}"))?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("raw census member is not a regular file".into());
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(remaining + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > remaining {
            return Err("raw census grew beyond reader capacity".into());
        }
        remaining -= bytes.len() as u64;
        inputs.insert(input.path.clone(), bytes);
        held.push((input.path.clone(), file));
    }
    row.verify_finalized_raw_input_bytes(
        &census.hermit_sha,
        row.started_at.as_deref().expect("checked"),
        &inputs,
        None,
    )?;
    let mut raw_rows = BTreeMap::new();
    let mut parity_rows: BTreeMap<CellIdentity, Vec<(u64, Value)>> = BTreeMap::new();
    for input in &census.files {
        let text = std::str::from_utf8(&inputs[&input.path]).map_err(|e| e.to_string())?;
        for indexed in &input.rows {
            let line = text
                .lines()
                .nth(indexed.line as usize - 1)
                .ok_or("census line absent")?;
            let value = read_schema10_source_result(line.as_bytes())?;
            let parity = value.get("backend_parity").is_some_and(|v| !v.is_null())
                || value
                    .get("attempts")
                    .and_then(Value::as_array)
                    .is_some_and(|a| {
                        a.iter().any(|v| {
                            v.get("index").and_then(Value::as_str) == Some("parity-reference")
                        })
                    });
            if parity {
                parity_rows
                    .entry(indexed.cell.clone())
                    .or_default()
                    .push((indexed.attempt, value.clone()));
            }
            raw_rows.insert((input.path.clone(), indexed.line), value);
        }
    }
    let mut parity_summaries = BTreeMap::new();
    for (cell, mut rows) in parity_rows {
        rows.sort_by_key(|(attempt, _)| *attempt);
        let proof = CellBackendParity::from_result_rows(&cell, &rows)?;
        parity_summaries.insert(cell.clone(), proof.summary(&cell)?);
    }
    let mut attempts = Vec::new();
    for input in &census.files {
        for indexed in &input.rows {
            let value = &raw_rows[&(input.path.clone(), indexed.line)];
            let typed: CellResult =
                serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            typed.require_current_classification()?;
            typed.require_current_timeout_policy()?;
            typed.require_cpu_observations()?;
            let (ordinary, backend_parity) =
                if let Some(summary) = parity_summaries.get(&indexed.cell) {
                    let operand = summary
                        .attempts
                        .iter()
                        .find(|a| a.attempt == indexed.attempt)
                        .ok_or("parity history omits census attempt")?;
                    let mut singleton = summary.clone();
                    singleton.attempts = vec![operand.clone()];
                    (operand.candidate.clone(), Some(singleton))
                } else {
                    (cell_verdict_from_source(value)?, None)
                };
            attempts.push(DerivedAttempt {
                input_path: input.path.clone(),
                input_line: indexed.line,
                cell: indexed.cell.clone(),
                attempt: indexed.attempt,
                source_row_sha256: json_digest(value),
                outcome: typed.outcome,
                result: typed.result,
                failure_class: typed.failure_class,
                ordinary_verdict: compact_cell_verdict(&ordinary),
                backend_parity,
            });
        }
    }
    for (path, file) in &mut held {
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let mut named =
            crate::validation_evidence::open_relative_path(&directory, Path::new(path), false)
                .map_err(|e| e.to_string())?;
        let mut named_bytes = Vec::new();
        (&mut named)
            .take(LIMIT + 1)
            .read_to_end(&mut named_bytes)
            .map_err(|e| e.to_string())?;
        if bytes != inputs[path] || named_bytes != inputs[path] {
            return Err("raw census source changed during derivation".into());
        }
    }
    let output = DerivedCensus {
        schema: 1,
        authority: AUTHORITY.into(),
        run_id: census.run_id,
        record_id: row_identity(&raw, "record_id")?,
        hermit_sha: census.hermit_sha,
        history_binding_sha256: json_digest(&history_binding(&raw)),
        raw_census_sha256: json_digest(&raw["raw_result_input_census_v1"]),
        attempts,
    };
    output.validate(&row, &raw)?;
    Ok(output)
}

pub(crate) fn write(row_path: &Path, root: &Path, output: &Path) -> Result<(), String> {
    let original = fs::read(row_path).map_err(|e| e.to_string())?;
    let derived = derive(&original, root)?;
    if fs::read(row_path).map_err(|e| e.to_string())? != original {
        return Err("original finalized row changed during derivation".into());
    }
    let mut bytes = canonical(&serde_json::to_value(derived).map_err(|e| e.to_string())?);
    bytes.push(b'\n');
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(output)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())
}

pub(crate) fn load(row: &HistoryRow, raw: &Value) -> Result<Option<DerivedCensus>, String> {
    let root = std::env::var_os("DEV_HERMIT_PARENT").map(std::path::PathBuf::from);
    load_at(row, raw, root.as_deref())
}

pub(crate) fn load_at(
    row: &HistoryRow,
    raw: &Value,
    root: Option<&Path>,
) -> Result<Option<DerivedCensus>, String> {
    if row.schema_version != Some(5) || raw.get("raw_result_input_census_v1").is_none() {
        return Ok(None);
    }
    let census = original_census(row)?;
    let Some(root) = root else {
        return Ok(None);
    };
    let relative = format!(
        "ignored/validate/artifacts/{}/website-derived-census-{}-{}.json",
        census.run_id,
        json_digest(&raw["raw_result_input_census_v1"]),
        json_digest(&history_binding(raw))
    );
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(root)
        .map_err(|e| e.to_string())?;
    let file = match crate::validation_evidence::open_relative_path(
        &directory,
        Path::new(&relative),
        false,
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot open derived census: {error}")),
    };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("derived census is not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("derived census exceeds reader capacity".into());
    }
    let value = read_schema10_source_result(&bytes)?;
    let evidence: DerivedCensus =
        serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    let mut expected = canonical(&value);
    expected.push(b'\n');
    if bytes != expected {
        return Err("derived census is not one canonical JSON line".into());
    }
    evidence.validate(row, raw)?;
    Ok(Some(evidence))
}

impl DerivedCensus {
    /// A census proves the recorded population, never the full planned selection.
    pub(crate) fn population_metadata(&self) -> Value {
        let cells = self
            .attempts
            .iter()
            .map(|attempt| &attempt.cell)
            .collect::<std::collections::BTreeSet<_>>();
        serde_json::json!({
            "selected_by_full_cell_count": null,
            "recorded_cell_count": cells.len(),
            "population_scope": "recorded_only",
            "comparison_authority": AUTHORITY,
        })
    }

    pub(crate) fn selected_cells(&self) -> Result<BTreeSet<String>, String> {
        Ok(self
            .attempts
            .iter()
            .map(|a| format!("{}/{}/{}", a.cell.test, a.cell.mode, a.cell.backend))
            .collect())
    }

    pub(crate) fn public_view(&self) -> Result<Value, String> {
        let mut groups: BTreeMap<CellIdentity, Vec<&DerivedAttempt>> = BTreeMap::new();
        for attempt in &self.attempts {
            groups
                .entry(attempt.cell.clone())
                .or_default()
                .push(attempt);
        }
        let mut cells = Vec::new();
        let mut selected = Vec::new();
        for (identity, mut attempts) in groups {
            attempts.sort_by_key(|a| a.attempt);
            let outcome = hermit_manifest_plan::runner::outcome_after_retries(
                attempts.iter().map(|a| (a.attempt, a.outcome.as_str())),
            )?;
            let chosen = attempts
                .iter()
                .rev()
                .find(|a| a.outcome == outcome)
                .ok_or("derived cell has no selected terminal outcome")?;
            let mut public = serde_json::to_value(&identity).map_err(|e| e.to_string())?;
            public["cell_verdict"] =
                serde_json::to_value(&chosen.ordinary_verdict).map_err(|e| e.to_string())?;
            let ordinary = if attempts
                .iter()
                .any(|a| matches!(a.ordinary_verdict, CellVerdict::ComparedAndDiverged { .. }))
            {
                "diverged"
            } else if attempts
                .iter()
                .all(|a| matches!(a.ordinary_verdict, CellVerdict::ComparedAndMatched { .. }))
            {
                "matched"
            } else {
                "unavailable"
            };
            public["ordinary_comparison"] = ordinary.into();
            public["comparison_authority"] = AUTHORITY.into();
            public["selected_attempt"] = chosen.attempt.into();
            public["attempt_count"] = attempts.len().into();
            let parity = attempts
                .iter()
                .filter_map(|a| a.backend_parity.as_ref())
                .collect::<Vec<_>>();
            public["backend_parity"] = if parity.is_empty() {
                Value::Null
            } else {
                if parity.len() != attempts.len() {
                    return Err("derived cell mixes ordinary and parity attempts".into());
                }
                let mut summary = parity[0].clone();
                summary.attempts = parity.iter().flat_map(|p| p.attempts.clone()).collect();
                serde_json::json!({"derived_operand_verified": true, "clean_match": summary.is_clean_match(), "summary": summary})
            };
            selected.push(serde_json::to_value(&identity).map_err(|e| e.to_string())?);
            cells.push(public);
        }
        let bytes = canonical(&serde_json::to_value(self).map_err(|e| e.to_string())?);
        Ok(serde_json::json!({
            "authority": AUTHORITY,
            "history_binding_sha256": self.history_binding_sha256,
            "raw_census_sha256": self.raw_census_sha256,
            "run_id": self.run_id, "hermit_sha": self.hermit_sha,
            "source_tree_dirty": false,
            "population_sha256": self.raw_census_sha256,
            "population_scope": "recorded_only",
            "selected_by": {"recorded": {"cell_count": selected.len(), "cells": selected}},
            "recorded": {"artifact": {"sha256": digest(&[bytes, vec![b'\n']].concat()), "row_count": self.attempts.len()},
                "cell_count": cells.len(), "cells": cells},
            "attempts": self.attempts,
        }))
    }
}
