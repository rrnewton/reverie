//! Typed public projection for compatibility-website inputs.
//!
//! Source records are deserialized through Hermit's shared Rust types.  This
//! module owns only the deliberately lossy website view; it never stores cell
//! state or a measured/not-measured flag.

use crate::compatibility_census::DerivedCensus;
use crate::failure_obligations::{self, CellResultsForWebsite};
use crate::records::{
    CellIdentity, CellResult, CellResultsArtifact, CellResultsEvidence, CellVerdict,
    ComparedLogCounts, ComparedLogScope, ComparisonSpec, ComparisonStrictness, ComparisonTier,
    GateHistoryRow, HistoryRow, RequiredNullable,
};
use hermit_manifest_plan::ci_selection::{CiDisabledReasonData, CiDisabledResult};
use hermit_manifest_plan::manifest_metadata::{
    BuildMetadata, CellMetadata, CurrentReproducer, DirectMetadata, ManifestMetadata,
    ObservationMetadata, TestMetadata,
};
use hermit_manifest_plan::runner::{FailureClass, ObservedResult};
use hermit_manifest_plan::stress_series::{
    HostCapability, HostCapabilityVerdict, SeriesAttemptDisposition, SeriesCoordinates,
    SeriesNoVerdictEvidence, SeriesNoVerdictKind, SeriesOutcome, SeriesPayload, SeriesProducer,
    SeriesRow, SeriesRuntime, SeriesRuntimeMeasurement, SeriesSchema, SourceDepth,
};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
struct Parsed<T> {
    typed: T,
    raw: Value,
}

#[derive(Clone, Debug)]
struct ValidationSelection {
    commit: String,
    path: String,
    selected_cells: BTreeSet<String>,
}

#[derive(Debug)]
struct ProjectedCellMetadata {
    identity: String,
    test: String,
    category: String,
    lane: String,
    mode: String,
    backend: String,
    public: Value,
}

#[derive(Debug)]
struct HistoryProjection {
    history: Vec<Value>,
    current: Vec<Value>,
    selections: BTreeMap<String, ValidationSelection>,
    exclusion_counts: BTreeMap<(String, String), (u64, u64)>,
    historical: Option<Value>,
    derived: BTreeMap<String, DerivedCensus>,
}

#[derive(Debug)]
struct PreparedSeries {
    public: Value,
    event_id: String,
    schema: String,
    producer: SeriesProducer,
    run_id: String,
    cell: String,
    tree: String,
    represented_runs: u64,
    projection_reason: Option<String>,
    product_result: Option<String>,
}

#[derive(Debug)]
struct SeriesProjection {
    rows: Vec<Value>,
    ancestry: BTreeMap<String, Option<bool>>,
    projection_results: BTreeMap<String, Option<String>>,
    product_results: BTreeMap<String, Option<String>>,
    validate_run_selection: BTreeMap<String, Value>,
    schema_counts: BTreeMap<String, u64>,
    physical_row_count: usize,
    represented_run_count: u64,
    admitted_physical_row_count: usize,
    admitted_represented_run_count: u64,
    exclusion_counts: BTreeMap<(String, String), (u64, u64)>,
}

#[derive(Debug, Default)]
struct SeriesAccumulation {
    prepared: Vec<PreparedSeries>,
    event_ids: BTreeSet<String>,
    selection_by_event: BTreeMap<String, Value>,
    projection_results: BTreeMap<String, Option<String>>,
    product_results: BTreeMap<String, Option<String>>,
    schema_counts: BTreeMap<String, u64>,
    represented_run_count: u64,
    admitted_physical_row_count: usize,
    admitted_represented_run_count: u64,
    excluded: BTreeMap<(String, String), (u64, u64)>,
}

pub(crate) fn project<F>(
    manifest_json: &[u8],
    history_jsonl: &str,
    series_jsonl: &str,
    historical_identity: Option<(&str, &str)>,
    mut current_main_ancestry: F,
) -> Result<Value, String>
where
    F: FnMut(&str) -> Result<Option<bool>, String>,
{
    let manifest: ManifestMetadata = serde_json::from_slice(manifest_json)
        .map_err(|error| format!("manifest metadata is invalid: {error}"))?;
    let history = parse_jsonl::<HistoryRow>(history_jsonl, "HistoryRow")?;
    let series = parse_jsonl::<SeriesRow>(series_jsonl, "SeriesRow")?;
    let history_input_row_count = history.len();
    let series_row_count = series.len();
    let manifest = project_manifest(manifest)?;
    let mut history = project_history(&history, historical_identity)?;
    let series = project_series(&series, &manifest, &history, &mut current_main_ancestry)?;
    if let Some(historical) = history.historical.as_mut() {
        bind_historical_measurement_environment(historical, &series.rows)?;
    }
    let current_validation_run_row_count = history.current.len();
    let validation_history_row_count = history.history.len();
    let HistoryProjection {
        history: validation_history_rows,
        current: current_validation_run_rows,
        selections: _,
        exclusion_counts: history_exclusion_counts,
        historical,
        derived: _,
    } = history;
    let SeriesProjection {
        rows: series_rows,
        ancestry,
        projection_results,
        product_results,
        validate_run_selection,
        schema_counts,
        physical_row_count,
        represented_run_count,
        admitted_physical_row_count,
        admitted_represented_run_count,
        mut exclusion_counts,
    } = series;
    for (key, counts) in history_exclusion_counts {
        let target = exclusion_counts.entry(key).or_default();
        target.0 = target
            .0
            .checked_add(counts.0)
            .ok_or("excluded physical-row count overflow")?;
        target.1 = target
            .1
            .checked_add(counts.1)
            .ok_or("excluded represented-run count overflow")?;
    }
    let exclusion_counts = projection_exclusion_counts(exclusion_counts);
    let mut output = serde_json::json!({
        "schema_version": 2,
        "input": {
            "manifest_metadata": input_binding(manifest_json, None),
            "validation_history_rows": input_binding(history_jsonl.as_bytes(), Some(history_input_row_count)),
            "series_rows": input_binding(series_jsonl.as_bytes(), Some(series_row_count)),
        },
        "manifest": manifest,
        "validation_history_rows": validation_history_rows,
        "current_validation_run_rows": current_validation_run_rows,
        "series_rows": series_rows,
        "current_main_ancestry": ancestry,
        "projection_results": projection_results,
        "product_results": product_results,
        "validate_run_selection": validate_run_selection,
        "counts": {
            "current_validation_run_row_count": current_validation_run_row_count,
            "projection_admitted_physical_row_count": admitted_physical_row_count,
            "projection_admitted_represented_run_count": admitted_represented_run_count,
            "projection_excluded_physical_row_count": physical_row_count - admitted_physical_row_count,
            "projection_excluded_represented_run_count": represented_run_count - admitted_represented_run_count,
            "series_physical_row_count": admitted_physical_row_count,
            "series_represented_run_count": admitted_represented_run_count,
            "series_schema_counts": schema_counts,
            "validation_history_row_count": validation_history_row_count,
        },
        "projection_exclusion_counts": exclusion_counts,
    });
    if let Some(historical) = historical {
        output["historical"] = historical;
    }
    let digest = sha256(&canonical_bytes(&output));
    output["projection_sha256"] = Value::String(digest);
    Ok(output)
}

fn parse_jsonl<T>(input: &str, label: &str) -> Result<Vec<Parsed<T>>, String>
where
    T: serde::de::DeserializeOwned,
{
    let mut values = Vec::new();
    for (index, line) in input.split_inclusive('\n').enumerate() {
        let line = line.strip_suffix('\n').unwrap_or(line);
        if line.trim().is_empty() {
            return Err(format!("{label} input line {} is blank", index + 1));
        }
        let raw = serde_json::from_str::<Value>(line)
            .map_err(|error| format!("{label} input line {} is invalid: {error}", index + 1))?;
        if !raw.is_object() {
            return Err(format!("{label} input line {} is not an object", index + 1));
        }
        let typed = serde_json::from_value(raw.clone())
            .map_err(|error| format!("{label} input line {} is invalid: {error}", index + 1))?;
        values.push(Parsed { typed, raw });
    }
    Ok(values)
}

fn input_binding(bytes: &[u8], row_count: Option<usize>) -> Value {
    let mut value = serde_json::json!({
        "bytes": bytes.len(),
        "sha256": sha256(bytes),
    });
    if let Some(row_count) = row_count {
        value["row_count"] = Value::from(row_count);
    }
    value
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn require_nonblank(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{label} must be nonempty"))
    } else {
        Ok(())
    }
}

fn require_optional_nonblank(value: Option<&str>, label: &str) -> Result<(), String> {
    if let Some(value) = value {
        require_nonblank(value, label)?;
    }
    Ok(())
}

fn require_nonblank_items(values: &[String], label: &str) -> Result<(), String> {
    for (index, value) in values.iter().enumerate() {
        require_nonblank(value, &format!("{label}[{index}]"))?;
    }
    Ok(())
}

fn require_nonempty_nonblank_items(values: &[String], label: &str) -> Result<(), String> {
    if values.is_empty() {
        return Err(format!("{label} must be nonempty"));
    }
    require_nonblank_items(values, label)
}

fn require_nonnegative_i64(value: Option<i64>, label: &str) -> Result<(), String> {
    if value.is_some_and(|value| value < 0) {
        Err(format!("{label} must be nonnegative"))
    } else {
        Ok(())
    }
}

fn require_i32_at_least(value: Option<i32>, minimum: i32, label: &str) -> Result<(), String> {
    if value.is_some_and(|value| value < minimum) {
        Err(format!("{label} must be at least {minimum}"))
    } else {
        Ok(())
    }
}

fn require_nonnegative_f64(value: Option<f64>, label: &str) -> Result<(), String> {
    if value.is_some_and(|value| value < 0.0) {
        Err(format!("{label} must be nonnegative"))
    } else {
        Ok(())
    }
}

fn require_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!("{label} must be lowercase 64-hex"))
    }
}

fn serialized<T: Serialize>(value: &T, label: &str) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| format!("cannot serialize {label}: {error}"))
}

fn project_manifest(manifest: ManifestMetadata) -> Result<Value, String> {
    let ManifestMetadata {
        schema,
        manifest_schema,
        manifest_sha256,
        tests,
        cells,
        selected_by_full_custom_commands,
    } = manifest;
    // Schema 2 binds independently resolved CPU and wall limits. The typed
    // CellMetadata decoder requires both; project_cell also refuses zero.
    if schema != 2 {
        return Err(format!("manifest metadata schema must be 2, got {schema}"));
    }
    if manifest_schema == 0 {
        return Err("manifest_schema must be positive".into());
    }
    require_sha256(&manifest_sha256, "manifest_sha256")?;

    let mut projected_tests = BTreeMap::new();
    for test in tests {
        let (id, value) = project_test(test)?;
        if projected_tests.insert(id.clone(), value).is_some() {
            return Err(format!("manifest metadata repeats test id {id:?}"));
        }
    }
    let mut cells_by_test: BTreeMap<String, Vec<String>> = projected_tests
        .keys()
        .cloned()
        .map(|test| (test, Vec::new()))
        .collect();
    let mut projected_cells = BTreeMap::new();
    for cell in cells {
        let projected = project_cell(cell, false)?;
        let test_value = projected_tests.get(&projected.test).ok_or_else(|| {
            format!(
                "manifest cell {:?} names unknown test {:?}",
                projected.identity, projected.test
            )
        })?;
        if test_value["category"].as_str() != Some(projected.category.as_str())
            || test_value["lane"].as_str() != Some(projected.lane.as_str())
        {
            return Err(format!(
                "manifest cell {:?} disagrees with its test",
                projected.identity
            ));
        }
        if projected_cells
            .insert(projected.identity.clone(), projected.public)
            .is_some()
        {
            return Err(format!(
                "manifest metadata repeats cell {:?}",
                projected.identity
            ));
        }
        cells_by_test
            .get_mut(&projected.test)
            .expect("known test has cell list")
            .push(projected.identity);
    }

    let mut projected_custom = Vec::new();
    let mut custom_ids = BTreeSet::new();
    for cell in selected_by_full_custom_commands {
        let projected = project_cell(cell, true)?;
        if projected_cells.contains_key(&projected.identity)
            || !custom_ids.insert(projected.identity.clone())
        {
            return Err(format!(
                "manifest metadata repeats custom command {:?}",
                projected.identity
            ));
        }
        let test_value = projected_tests.get(&projected.test).ok_or_else(|| {
            format!(
                "custom command {:?} names unknown test {:?}",
                projected.identity, projected.test
            )
        })?;
        if test_value["category"].as_str() != Some(projected.category.as_str())
            || test_value["lane"].as_str() != Some(projected.lane.as_str())
        {
            return Err(format!(
                "custom command {:?} disagrees with its test",
                projected.identity
            ));
        }
        projected_custom.push(projected);
    }
    projected_custom.sort_by(|left, right| {
        (&left.test, &left.mode, &left.backend).cmp(&(&right.test, &right.mode, &right.backend))
    });

    let mut tests = Vec::new();
    for (test, mut value) in projected_tests {
        let cells = cells_by_test.remove(&test).expect("test has cell list");
        let has_custom = projected_custom.iter().any(|cell| cell.test == test);
        if cells.is_empty() && !has_custom {
            return Err(format!("manifest test {test:?} has no cells"));
        }
        value["cells"] = serialized(&cells, "manifest test cells")?;
        tests.push(value);
    }
    let projected_custom = projected_custom
        .into_iter()
        .map(|cell| cell.public)
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "cells": projected_cells,
        "manifest_schema": manifest_schema,
        "manifest_sha256": manifest_sha256,
        "selected_by_full_custom_commands": projected_custom,
        "tests": tests,
    }))
}

fn project_test(test: TestMetadata) -> Result<(String, Value), String> {
    let TestMetadata {
        id,
        description,
        category,
        lane,
        requires,
        occasional,
        program,
        direct,
        build,
        observation,
        preprocessors,
    } = test;
    for (label, value) in [
        ("test id", id.as_str()),
        ("test description", description.as_str()),
        ("test category", category.as_str()),
        ("test lane", lane.as_str()),
    ] {
        require_nonblank(value, label)?;
    }
    require_optional_nonblank(program.as_deref(), "test program")?;
    require_nonblank_items(&requires, "test requires")?;
    require_nonblank_items(&preprocessors, "test preprocessors")?;
    if program.is_some() == direct.is_some() {
        return Err(format!(
            "manifest test {id:?} must contain exactly one of program or direct"
        ));
    }
    let direct = match direct {
        Some(DirectMetadata::Shell { command }) => {
            require_nonblank(&command, "direct shell command")?;
            Some(serde_json::json!({"kind": "shell", "command": command}))
        }
        Some(DirectMetadata::Argv { argv }) => {
            require_nonempty_nonblank_items(&argv, "direct argv")?;
            Some(serde_json::json!({"kind": "argv", "argv": argv}))
        }
        None => None,
    };
    let build = match build {
        Some(build) => {
            let BuildMetadata { cflags, rustflags } = build;
            require_nonblank_items(&cflags, "build cflags")?;
            require_nonblank_items(&rustflags, "build rustflags")?;
            Some(serde_json::json!({"cflags": cflags, "rustflags": rustflags}))
        }
        None => None,
    };
    let ObservationMetadata {
        status,
        stdout,
        stderr,
        artifacts,
    } = observation;
    require_nonblank_items(&artifacts, "observation artifacts")?;
    let value = serde_json::json!({
        "build": build,
        "category": category,
        "description": description,
        "direct": direct,
        "id": id,
        "lane": lane,
        "observation": {
            "artifacts": artifacts,
            "status": status,
            "stderr": stderr,
            "stdout": stdout,
        },
        "occasional": occasional,
        "preprocessors": preprocessors,
        "program": program,
        "requires": requires,
    });
    Ok((
        value["id"]
            .as_str()
            .expect("test id is a string")
            .to_owned(),
        value,
    ))
}

fn project_cell(cell: CellMetadata, custom: bool) -> Result<ProjectedCellMetadata, String> {
    let CellMetadata {
        test,
        category,
        lane,
        mode,
        backend,
        selected_by_full,
        not_selected_by_full_reason,
        not_applicable_reason,
        timeout_seconds,
        cpu_timeout_seconds,
        guest_args,
        workdir,
        current_reproducer,
        current_reproducer_unavailable_reason,
    } = cell;
    for (label, value) in [
        ("cell test", test.as_str()),
        ("cell category", category.as_str()),
        ("cell lane", lane.as_str()),
        ("cell mode", mode.as_str()),
        ("cell backend", backend.as_str()),
    ] {
        require_nonblank(value, label)?;
    }
    require_nonblank_items(&guest_args, "cell guest_args")?;
    require_optional_nonblank(workdir.as_deref(), "cell workdir")?;
    require_optional_nonblank(
        current_reproducer_unavailable_reason.as_deref(),
        "current reproducer unavailable reason",
    )?;
    if custom != (mode == "custom") || (custom && !selected_by_full) {
        return Err(format!(
            "invalid manifest cell mode/selection for {test}/{mode}/{backend}"
        ));
    }
    if timeout_seconds == 0 {
        return Err(format!(
            "manifest cell {test}/{mode}/{backend} has zero timeout"
        ));
    }
    if cpu_timeout_seconds == 0 {
        return Err(format!(
            "manifest cell {test}/{mode}/{backend} has zero CPU timeout"
        ));
    }
    match (
        selected_by_full,
        not_selected_by_full_reason.as_ref(),
        not_applicable_reason.as_ref(),
    ) {
        (true, None, None) | (false, Some(_), None) | (false, None, Some(_)) => {}
        _ => {
            return Err(format!(
                "manifest cell {test}/{mode}/{backend} has contradictory selection reasons"
            ));
        }
    }
    let not_selected_by_full_reason = not_selected_by_full_reason
        .map(project_not_selected_reason)
        .transpose()?;
    if not_applicable_reason
        .as_ref()
        .is_some_and(|reason| reason.trim().is_empty())
    {
        return Err(format!(
            "manifest cell {test}/{mode}/{backend} has a blank reason"
        ));
    }
    let current_reproducer = current_reproducer
        .map(project_current_reproducer)
        .transpose()?;
    match (
        current_reproducer.as_ref(),
        current_reproducer_unavailable_reason.as_deref(),
    ) {
        (Some(_), None) => {}
        (None, Some(reason)) if !reason.trim().is_empty() => {}
        _ => {
            return Err(format!(
                "manifest cell {test}/{mode}/{backend} has contradictory reproducer fields"
            ));
        }
    }
    let identity = format!("{test}/{mode}/{backend}");
    let value = serde_json::json!({
        "backend": backend,
        "category": category,
        "cell": identity,
        "current_reproducer": current_reproducer,
        "current_reproducer_unavailable_reason": current_reproducer_unavailable_reason,
        "guest_args": guest_args,
        "lane": lane,
        "mode": mode,
        "not_applicable_reason": not_applicable_reason,
        "not_selected_by_full_reason": not_selected_by_full_reason,
        "selected_by_full": selected_by_full,
        "test": test,
        "timeout_seconds": timeout_seconds,
        "cpu_timeout_seconds": cpu_timeout_seconds,
        "workdir": workdir,
    });
    Ok(ProjectedCellMetadata {
        identity,
        test,
        category,
        lane,
        mode,
        backend,
        public: value,
    })
}

fn project_not_selected_reason(reason: CiDisabledReasonData) -> Result<Value, String> {
    let CiDisabledReasonData {
        result,
        evidence,
        reason,
    } = reason;
    require_nonblank(&reason, "not selected by full reason")?;
    if result.is_some() != evidence.is_some() {
        return Err("not selected by full result and evidence must be present together".into());
    }
    let result = result.map(ci_disabled_result);
    let mut value = Map::new();
    if let Some(evidence) = evidence {
        require_nonblank(&evidence, "not selected by full evidence")?;
        value.insert("evidence".into(), Value::String(evidence));
    }
    value.insert("reason".into(), Value::String(reason));
    if let Some(result) = result {
        value.insert("result".into(), Value::String(result.into()));
    }
    Ok(Value::Object(value))
}

fn ci_disabled_result(result: CiDisabledResult) -> &'static str {
    match result {
        CiDisabledResult::NotMeasured => "not-measured",
        CiDisabledResult::DeterminismFailure => "determinism-failure",
        CiDisabledResult::ParityFailure => "parity-failure",
        CiDisabledResult::ReplayFailure => "replay-failure",
        CiDisabledResult::CrashError => "crash-error",
        CiDisabledResult::Timeout => "timeout",
        CiDisabledResult::Oom => "oom",
        CiDisabledResult::InfrastructureError => "infrastructure-error",
        CiDisabledResult::Unavailable => "unavailable",
    }
}

fn project_current_reproducer(reproducer: CurrentReproducer) -> Result<Value, String> {
    let CurrentReproducer {
        argv,
        shell_command,
    } = reproducer;
    require_nonempty_nonblank_items(&argv, "current reproducer argv")?;
    require_nonblank(&shell_command, "current reproducer shell_command")?;
    Ok(serde_json::json!({"argv": argv, "shell_command": shell_command}))
}

fn canonical_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(&canonical_sorted_value(value)).expect("JSON value serializes")
}

fn canonical_sorted_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical_sorted_value).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut sorted = Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonical_sorted_value(&object[key]));
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

fn project_history(
    rows: &[Parsed<HistoryRow>],
    historical_identity: Option<(&str, &str)>,
) -> Result<HistoryProjection, String> {
    project_history_with(rows, historical_identity, crate::compatibility_census::load)
}

fn project_history_with(
    rows: &[Parsed<HistoryRow>],
    historical_identity: Option<(&str, &str)>,
    load: impl Fn(&HistoryRow, &Value) -> Result<Option<DerivedCensus>, String>,
) -> Result<HistoryProjection, String> {
    let typed_rows = rows
        .iter()
        .map(|parsed| parsed.typed.clone())
        .collect::<Vec<_>>();
    let terminal = crate::current_validation_run_indices(&typed_rows)?;
    let terminal_set = terminal.iter().copied().collect::<BTreeSet<_>>();
    let mut history = Vec::new();
    let mut derived = BTreeMap::new();
    let mut admitted_by_source_ordinal = BTreeMap::new();
    let mut exclusion_counts: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
    for (index, parsed) in rows.iter().enumerate() {
        let operand = load(&parsed.typed, &parsed.raw)?;
        let reasons = if operand.is_some() {
            Vec::new()
        } else {
            history_exclusion_reasons(&parsed.typed)?
        };
        if !reasons.is_empty() {
            let schema = parsed
                .typed
                .schema_version
                .map(|value| value.to_string())
                .unwrap_or_else(|| "not recorded".into());
            for reason in reasons {
                let counts = exclusion_counts
                    .entry((schema.clone(), reason))
                    .or_default();
                counts.0 += 1;
                counts.1 += 1;
            }
            continue;
        }
        let public = if let Some(operand) = operand {
            let mut public =
                project_history_row_with_derived(&parsed.typed, &parsed.raw, Some(&operand))?;
            public["validation_complete"] = parsed
                .raw
                .get("validation_complete")
                .cloned()
                .unwrap_or(Value::Null);
            if derived.insert(operand.run_id.clone(), operand).is_some() {
                return Err("derived history repeats its run identity".into());
            }
            public
        } else {
            project_history_row(&parsed.typed, &parsed.raw)?
        };
        admitted_by_source_ordinal.insert(index, public.clone());
        history.push(public);
    }
    let (current, selections) =
        project_terminal_history(rows, &admitted_by_source_ordinal, &terminal_set, &derived)?;
    let historical = historical_identity
        .map(|(record_id, run_id)| {
            historical_source(
                rows,
                &terminal,
                &admitted_by_source_ordinal,
                &derived,
                record_id,
                run_id,
            )
        })
        .transpose()?;
    Ok(HistoryProjection {
        history,
        current,
        selections,
        exclusion_counts,
        historical,
        derived,
    })
}

fn history_exclusion_reasons(row: &HistoryRow) -> Result<Vec<String>, String> {
    let mut reasons = BTreeSet::new();
    if !matches!(row.schema_version, Some(7..=10)) {
        reasons.insert("validation HistoryRow schema is not 7, 8, 9 or 10".to_owned());
    }
    if !row_is_hermit(row) {
        reasons.insert("validation HistoryRow is not a Hermit row".to_owned());
    }
    for (field, value) in [
        ("run_id", row.run_id.as_deref()),
        ("started_at", row.started_at.as_deref()),
        ("finished_at", row.finished_at.as_deref()),
        ("profile", row.profile.as_deref()),
        ("selection_mode", row.selection_mode.as_deref()),
        ("result", row.result.as_deref()),
    ] {
        if value.is_none_or(|value| value.trim().is_empty()) {
            reasons.insert(format!("validation HistoryRow omits {field}"));
        }
    }
    if extra_string(row, "record_id")?.is_none() {
        reasons.insert("validation HistoryRow omits record_id".to_owned());
    }
    match failure_obligations::cell_results_for_website(row)? {
        CellResultsForWebsite::Complete(_) => {}
        CellResultsForWebsite::Absent => {
            reasons.insert("validation HistoryRow omits cell_results".to_owned());
        }
        CellResultsForWebsite::Schema6WithoutLaterComparisonFields => {
            reasons.insert(
                "validation HistoryRow cell_results lack the later comparison fields".to_owned(),
            );
        }
        CellResultsForWebsite::UnsupportedSchema(schema) => {
            reasons.insert(format!(
                "validation HistoryRow schema {schema} has no supported cell_results"
            ));
        }
        CellResultsForWebsite::Unavailable(reason) => {
            reasons.insert(format!("validation cell evidence unavailable: {reason}"));
        }
    }
    Ok(reasons.into_iter().collect())
}

fn project_terminal_history(
    rows: &[Parsed<HistoryRow>],
    public_rows: &BTreeMap<usize, Value>,
    terminal: &BTreeSet<usize>,
    derived: &BTreeMap<String, DerivedCensus>,
) -> Result<(Vec<Value>, BTreeMap<String, ValidationSelection>), String> {
    let mut current = Vec::new();
    let mut selections = BTreeMap::new();
    let mut current_run_ids = BTreeSet::new();
    for &index in terminal {
        let row = rows
            .get(index)
            .ok_or_else(|| format!("terminal HistoryRow ordinal {index} is out of range"))?;
        let Some(public) = public_rows.get(&index) else {
            continue;
        };
        current.push(public.clone());
        let run_id = row
            .typed
            .run_id
            .as_deref()
            .expect("admitted HistoryRow has run_id");
        require_nonblank(run_id, "terminal HistoryRow run_id")?;
        if !current_run_ids.insert(run_id.to_owned()) {
            return Err(format!("terminal validation rows repeat run_id {run_id:?}"));
        }
        let selection = if let Some(operand) = derived.get(run_id) {
            ValidationSelection {
                commit: operand.hermit_sha.clone(),
                path: "recorded".into(),
                selected_cells: operand.selected_cells()?,
            }
        } else {
            let CellResultsForWebsite::Complete(evidence) =
                failure_obligations::cell_results_for_website(&row.typed)?
            else {
                unreachable!("admitted HistoryRow has complete cell_results")
            };
            validation_selection(&row.typed, evidence.as_ref())?
        };
        selections.insert(run_id.to_owned(), selection);
    }
    Ok((current, selections))
}

fn historical_source(
    rows: &[Parsed<HistoryRow>],
    terminal: &[usize],
    admitted_by_source_ordinal: &BTreeMap<usize, Value>,
    derived: &BTreeMap<String, DerivedCensus>,
    record_id: &str,
    run_id: &str,
) -> Result<Value, String> {
    require_nonblank(record_id, "historical record_id")?;
    require_nonblank(run_id, "historical run_id")?;
    let matches = terminal
        .iter()
        .filter_map(|&index| rows.get(index).map(|row| (index, row)))
        .filter(|(_, row)| {
            row.typed.extra.get("record_id").and_then(Value::as_str) == Some(record_id)
                && row.typed.run_id.as_deref() == Some(run_id)
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "historical record_id {record_id:?} and run_id {run_id:?} matched {} terminal rows; expected one",
            matches.len()
        ));
    }
    let (index, parsed) = matches[0];
    if !admitted_by_source_ordinal.contains_key(&index) {
        return Err(
            "historical selected row does not satisfy the website history predicate".into(),
        );
    }
    let row = &parsed.typed;
    if !row_is_hermit(row) {
        return Err("historical selected row is not a Hermit validation row".into());
    }
    if row.profile.as_deref() != Some("full") || row.selection_mode.as_deref() != Some("full") {
        return Err(
            "historical selected row must record validate path full and selection_mode full".into(),
        );
    }
    let commit = row
        .commit
        .as_deref()
        .ok_or("historical selected row omits commit")?;
    require_object_id(commit, "historical measured Hermit commit")?;
    let started_at = row
        .started_at
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or("historical selected row omits started_at")?;
    let finished_at = row
        .finished_at
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or("historical selected row omits finished_at")?;
    let measurement_host = row
        .host
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or("historical selected row omits measurement host")?;
    let measured_reverie_commit = extra_string(row, "reverie_base_sha")?
        .ok_or("historical selected row omits measured Reverie commit")?;
    require_object_id(
        measured_reverie_commit,
        "historical measured Reverie commit",
    )?;
    let population = if let Some(evidence) = derived.get(run_id) {
        evidence.population_metadata()
    } else {
        let evidence = match failure_obligations::cell_results_for_website(row)? {
            CellResultsForWebsite::Complete(evidence) => evidence,
            CellResultsForWebsite::Absent
            | CellResultsForWebsite::Schema6WithoutLaterComparisonFields
            | CellResultsForWebsite::UnsupportedSchema(_)
            | CellResultsForWebsite::Unavailable(_) => {
                return Err("historical selected row has no complete cell_results evidence".into());
            }
        };
        serde_json::json!({"selected_by_full_cell_count": evidence.selected_count})
    };
    let mut source = serde_json::json!({
        "finished_at": finished_at,
        "history_row_sha256": history_row_digest(&rows[index].raw)?,
        "measurement_host": measurement_host,
        "measured_hermit_commit": commit,
        "measured_reverie_commit": measured_reverie_commit,
        "record_id": record_id,
        "run_id": run_id,
        "started_at": started_at,
        "validate_path": "full",
    });
    source.as_object_mut().expect("constructed source").extend(
        population
            .as_object()
            .expect("constructed population")
            .clone(),
    );
    Ok(source)
}

fn bind_historical_measurement_environment(
    historical: &mut Value,
    series_rows: &[Value],
) -> Result<(), String> {
    let source = historical
        .as_object_mut()
        .ok_or("historical source is not an object")?;
    let run_id = source
        .get("run_id")
        .and_then(Value::as_str)
        .ok_or("historical source omits run_id")?;
    let measurement_host = source
        .get("measurement_host")
        .and_then(Value::as_str)
        .ok_or("historical source omits measurement host")?;
    let mut environment: Option<Value> = None;
    for raw in series_rows {
        let row = raw
            .as_object()
            .ok_or("projected historical series row is not an object")?;
        if row.get("run_id").and_then(Value::as_str) != Some(run_id) {
            continue;
        }
        let host = row
            .get("host")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or("historical series row omits measurement host")?;
        if host != measurement_host {
            return Err(
                "historical series row measurement host disagrees with its validation row".into(),
            );
        }
        let payload = row
            .get("series")
            .and_then(Value::as_object)
            .ok_or("historical series row omits its payload")?;
        let machine = payload
            .get("machine_shortname")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or("historical series row omits machine_shortname")?;
        if machine != measurement_host {
            return Err(
                "historical series machine_shortname disagrees with its validation row host".into(),
            );
        }
        let kernel = payload
            .get("kernel_version")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or("historical series row omits kernel_version")?;
        let capabilities = payload
            .get("host_capabilities")
            .and_then(Value::as_object)
            .filter(|value| !value.is_empty())
            .ok_or("historical series row omits host_capabilities")?;
        let current = serde_json::json!({
            "host_capabilities": capabilities,
            "kernel_version": kernel,
        });
        match &environment {
            None => environment = Some(current),
            Some(expected) if expected == &current => {}
            Some(_) => {
                return Err(
                    "historical series rows disagree about their measurement environment".into(),
                );
            }
        }
    }
    let environment = environment
        .ok_or("historical selected run has no admitted series measurement environment")?;
    let environment = environment
        .as_object()
        .expect("constructed historical measurement environment is an object");
    source.insert(
        "host_capabilities".into(),
        environment["host_capabilities"].clone(),
    );
    source.insert(
        "kernel_version".into(),
        environment["kernel_version"].clone(),
    );
    Ok(())
}

fn project_history_row(row: &HistoryRow, raw: &Value) -> Result<Value, String> {
    project_history_row_with_derived(row, raw, None)
}

fn project_history_row_with_derived(
    row: &HistoryRow,
    raw: &Value,
    derived: Option<&DerivedCensus>,
) -> Result<Value, String> {
    let digest = history_row_digest(raw)?;
    let raw = raw
        .as_object()
        .ok_or("HistoryRow source value is not an object")?;
    let (schema, mut public) = project_history_fields(row, raw)?;
    project_history_tree_and_nodes(row, raw, schema, &mut public)?;
    project_history_extensions(row, &mut public)?;
    project_history_gates(row, raw, &mut public)?;
    if let Some(derived) = derived {
        public.insert("cell_results".into(), Value::Null);
        public.insert("derived_cell_results".into(), derived.public_view()?);
    } else {
        project_history_cell_results(row, &mut public)?;
    }
    public.insert("history_row_sha256".into(), Value::String(digest));
    Ok(Value::Object(public))
}

fn project_history_fields(
    row: &HistoryRow,
    raw: &Map<String, Value>,
) -> Result<(u32, Map<String, Value>), String> {
    let HistoryRow {
        schema_version,
        run_id,
        started_at,
        finished_at,
        host,
        slot,
        repo,
        cwd,
        profile,
        selection_mode,
        commit,
        commit_anchored,
        tree_dirty,
        result,
        exit_code,
        executed_tests,
        passed_tests,
        filtered_tests,
        coverage,
        full_coverage,
        checks,
        gates_run,
        gates_expected,
        failures,
        dag_jobs,
        concurrent_validates,
        known_flaky_failure,
        solo_rerun_confirmation,
        real_seconds,
        user_seconds,
        sys_seconds,
        log_file,
        source,
        cell_results,
        test_results,
        gates,
        extra,
    } = row;
    let _deliberately_omitted = (
        slot,
        cwd,
        commit_anchored,
        passed_tests,
        coverage,
        full_coverage,
        dag_jobs,
        concurrent_validates,
        known_flaky_failure,
        solo_rerun_confirmation,
        log_file,
        source,
    );
    let _projected_by_typed_helpers = (cell_results, test_results, gates, extra);
    let schema = schema_version.ok_or("validation HistoryRow omits schema_version")?;
    if schema == 0 {
        return Err("validation HistoryRow schema_version must be positive".into());
    }
    for (label, value) in [
        ("HistoryRow run_id", run_id.as_deref()),
        ("HistoryRow started_at", started_at.as_deref()),
        ("HistoryRow finished_at", finished_at.as_deref()),
        ("HistoryRow host", host.as_deref()),
        ("HistoryRow slot", slot.as_deref()),
        ("HistoryRow repo", repo.as_deref()),
        ("HistoryRow cwd", cwd.as_deref()),
        ("HistoryRow profile", profile.as_deref()),
        ("HistoryRow selection_mode", selection_mode.as_deref()),
        ("HistoryRow result", result.as_deref()),
        ("HistoryRow log_file", log_file.as_deref()),
        ("HistoryRow source", source.as_deref()),
    ] {
        require_optional_nonblank(value, label)?;
    }
    require_i32_at_least(*exit_code, 0, "HistoryRow exit_code")?;
    require_nonnegative_i64(*executed_tests, "HistoryRow executed_tests")?;
    require_nonnegative_i64(*filtered_tests, "HistoryRow filtered_tests")?;
    require_nonnegative_f64(*real_seconds, "HistoryRow real_seconds")?;
    require_nonnegative_f64(*user_seconds, "HistoryRow user_seconds")?;
    require_nonnegative_f64(*sys_seconds, "HistoryRow sys_seconds")?;
    let commit = commit
        .as_deref()
        .ok_or("validation HistoryRow omits commit")?;
    require_object_id(commit, "HistoryRow commit")?;

    let mut public = Map::new();
    public.insert("schema_version".into(), Value::from(schema));
    public.insert("commit".into(), Value::String(commit.to_owned()));
    for (name, value) in [
        ("run_id", run_id),
        ("started_at", started_at),
        ("finished_at", finished_at),
        ("host", host),
        ("repo", repo),
        ("profile", profile),
        ("selection_mode", selection_mode),
        ("result", result),
    ] {
        insert_option_if_present(&mut public, raw, name, value)?;
    }
    insert_option_if_present(&mut public, raw, "tree_dirty", tree_dirty)?;
    insert_option_if_present(&mut public, raw, "exit_code", exit_code)?;
    for (name, value) in [
        ("executed_tests", executed_tests),
        ("filtered_tests", filtered_tests),
    ] {
        insert_option_if_present(&mut public, raw, name, value)?;
    }
    for (name, value) in [
        ("checks", checks),
        ("gates_run", gates_run),
        ("gates_expected", gates_expected),
        ("failures", failures),
    ] {
        insert_option_if_present(&mut public, raw, name, value)?;
    }
    for (name, value) in [
        ("real_seconds", real_seconds),
        ("user_seconds", user_seconds),
        ("sys_seconds", sys_seconds),
    ] {
        insert_option_if_present(&mut public, raw, name, value)?;
    }
    Ok((schema, public))
}

fn project_history_tree_and_nodes(
    row: &HistoryRow,
    raw: &Map<String, Value>,
    schema: u32,
    public: &mut Map<String, Value>,
) -> Result<(), String> {
    if raw.contains_key("tree") {
        match raw.get("tree") {
            Some(Value::String(value)) if schema == 5 && value == "unknown" => {
                return Err(format!(
                    "admitted validation HistoryRow has legacy tree value {value:?}"
                ));
            }
            _ => {
                let tree = row.tree().map_err(str::to_owned)?.ok_or_else(|| {
                    "HistoryRow source has tree but the typed accessor returned absence".to_string()
                })?;
                require_object_id(tree, "HistoryRow tree")?;
                public.insert("tree".into(), Value::String(tree.to_owned()));
            }
        }
    }
    if raw.contains_key("executed_nodes") {
        let executed_nodes = row
            .executed_nodes()
            .map_err(str::to_owned)?
            .ok_or_else(|| {
                "HistoryRow source has executed_nodes but the typed accessor returned absence"
                    .to_string()
            })?;
        public.insert("executed_nodes".into(), Value::from(executed_nodes));
    }
    Ok(())
}

fn project_history_gates(
    row: &HistoryRow,
    raw: &Map<String, Value>,
    public: &mut Map<String, Value>,
) -> Result<(), String> {
    if !raw.contains_key("gates") {
        return Ok(());
    }
    let raw_gates = raw
        .get("gates")
        .and_then(Value::as_array)
        .ok_or("HistoryRow gates must be an array")?;
    if raw_gates.len() != row.gates.len() {
        return Err("typed HistoryRow gate count disagrees with source".into());
    }
    let projected = row
        .gates
        .iter()
        .zip(raw_gates)
        .enumerate()
        .map(|(index, (gate, raw))| project_gate(gate, raw, index))
        .collect::<Result<Vec<_>, _>>()?;
    public.insert("gates".into(), Value::Array(projected));
    Ok(())
}

fn project_history_cell_results(
    row: &HistoryRow,
    public: &mut Map<String, Value>,
) -> Result<(), String> {
    match failure_obligations::cell_results_for_website(row)? {
        CellResultsForWebsite::Complete(evidence) => {
            public.insert(
                "cell_results".into(),
                project_cell_results(row, evidence.as_ref(), evidence.modern.as_deref())?,
            );
        }
        CellResultsForWebsite::Absent
        | CellResultsForWebsite::Schema6WithoutLaterComparisonFields
        | CellResultsForWebsite::UnsupportedSchema(_)
        | CellResultsForWebsite::Unavailable(_) => {
            return Err("admitted validation HistoryRow has incomplete cell_results".into());
        }
    }
    Ok(())
}

fn insert_option_if_present<T: Serialize>(
    output: &mut Map<String, Value>,
    source: &Map<String, Value>,
    name: &str,
    value: &Option<T>,
) -> Result<(), String> {
    if source.contains_key(name) {
        output.insert(name.to_owned(), serialized(value, name)?);
    }
    Ok(())
}

fn insert_nullable_extension<F>(
    output: &mut Map<String, Value>,
    source: &BTreeMap<String, Value>,
    name: &str,
    project_value: F,
) -> Result<(), String>
where
    F: FnOnce(&Value) -> Result<Value, String>,
{
    match source.get(name) {
        None => Ok(()),
        Some(Value::Null) => {
            output.insert(name.to_owned(), Value::Null);
            Ok(())
        }
        Some(value) => {
            output.insert(name.to_owned(), project_value(value)?);
            Ok(())
        }
    }
}

fn project_nonblank_string(value: &Value, label: &str) -> Result<Value, String> {
    let value = value
        .as_str()
        .ok_or_else(|| format!("{label} must be a string or null"))?;
    require_nonblank(value, label)?;
    Ok(Value::String(value.to_owned()))
}

fn project_nonnegative_integer(value: &Value, label: &str) -> Result<Value, String> {
    value
        .as_u64()
        .map(Value::from)
        .ok_or_else(|| format!("{label} must be a nonnegative integer or null"))
}

fn project_boolean(value: &Value, label: &str) -> Result<Value, String> {
    value
        .as_bool()
        .map(Value::from)
        .ok_or_else(|| format!("{label} must be a boolean or null"))
}

fn project_history_extensions(
    row: &HistoryRow,
    public: &mut Map<String, Value>,
) -> Result<(), String> {
    match crate::correction_record_id(row)? {
        Some(record_id) => {
            public.insert("corrects".into(), Value::String(record_id.to_owned()));
        }
        None if matches!(row.extra.get("corrects"), Some(Value::Object(_))) => {
            return Err("admitted validation HistoryRow has legacy corrects metadata".into());
        }
        None => {}
    }
    for name in ["record_id", "producer"] {
        if let Some(value) = row.extra.get(name) {
            let value = value
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| format!("HistoryRow {name} must be a nonempty string"))?;
            public.insert(name.into(), Value::String(value.to_owned()));
        }
    }
    if let Some(run_number) = row
        .extra
        .get("run_number")
        .and_then(Value::as_u64)
        .filter(|run_number| *run_number > 0)
    {
        public.insert("run_number".into(), Value::from(run_number));
    }
    if let Some(value) = row.extra.get("skipped_nodes") {
        let value = value
            .as_u64()
            .ok_or_else(|| "HistoryRow skipped_nodes must be a nonnegative integer".to_string())?;
        public.insert("skipped_nodes".into(), Value::from(value));
    }
    Ok(())
}

fn project_gate(gate: &GateHistoryRow, raw: &Value, index: usize) -> Result<Value, String> {
    let raw = raw
        .as_object()
        .ok_or_else(|| format!("HistoryRow gate {index} is not an object"))?;
    let GateHistoryRow {
        name,
        result,
        kind,
        exit_code,
        real_seconds,
        source_node,
        failure_origin,
        failed_substeps,
        extra,
    } = gate;
    require_nonblank(name, "gate name")?;
    for (label, value) in [
        ("gate result", result.as_deref()),
        ("gate kind", kind.as_deref()),
        ("gate source_node", source_node.as_deref()),
        ("gate failure_origin", failure_origin.as_deref()),
    ] {
        require_optional_nonblank(value, label)?;
    }
    if let Some(failed_substeps) = failed_substeps {
        require_nonblank_items(failed_substeps, "gate failed_substeps")?;
    }
    require_i32_at_least(*exit_code, -255, "gate exit_code")?;
    require_nonnegative_f64(*real_seconds, "gate real_seconds")?;
    let _deliberately_omitted = (kind, source_node, failed_substeps);
    let mut public = Map::new();
    public.insert("name".into(), Value::String(name.clone()));
    for (field, value) in [("result", result), ("failure_origin", failure_origin)] {
        insert_option_if_present(&mut public, raw, field, value)?;
    }
    insert_option_if_present(&mut public, raw, "exit_code", exit_code)?;
    insert_option_if_present(&mut public, raw, "real_seconds", real_seconds)?;
    if let Some(failure_class) = gate.failure_class()? {
        public.insert(
            "failure_class".into(),
            Value::String(failure_class.as_str().into()),
        );
    }
    insert_nullable_extension(&mut public, extra, "first_attempt_result", |value| {
        project_nonblank_string(value, "gate first_attempt_result")
    })?;
    for field in ["oom_kills", "retries"] {
        insert_nullable_extension(&mut public, extra, field, |value| {
            project_nonnegative_integer(value, &format!("gate {field}"))
        })?;
    }
    for field in ["aborted", "cpu_timed_out", "oomed", "reported", "timed_out"] {
        insert_nullable_extension(&mut public, extra, field, |value| {
            project_boolean(value, &format!("gate {field}"))
        })?;
    }
    let _unknown_extensions_are_deliberately_omitted = extra;
    Ok(Value::Object(public))
}

fn project_cell_results(
    row: &HistoryRow,
    evidence: &CellResultsEvidence,
    modern: Option<&crate::records::VerifiedValidationEvidenceV10>,
) -> Result<Value, String> {
    if (row.schema_version == Some(10)) != modern.is_some() {
        return Err(
            "website schema-10 projection requires its retained artifact verification".into(),
        );
    }
    let CellResultsEvidence {
        run_id,
        hermit_sha,
        source_tree_dirty,
        selected_count,
        recorded_count,
        population_sha256,
        artifact,
        selected,
        cells,
    } = evidence;
    let CellResultsArtifact {
        path,
        sha256: artifact_sha256,
        row_count,
    } = artifact;
    let _retained_artifact_path_is_deliberately_omitted = path;
    let path = validation_path(row)?;
    let selected = selected
        .iter()
        .map(project_identity)
        .collect::<Result<Vec<_>, _>>()?;
    let mut cells = cells
        .iter()
        .map(project_cell_result)
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(evidence) = modern {
        // Preserve the exact verifier result used for this projection. Never
        // reparse the row summary as authority or reopen a second artifact set.
        for (public, cell) in cells.iter_mut().zip(&evidence.cell_results.cells) {
            let observations = evidence
                .observations
                .iter()
                .filter(|observation| {
                    observation.identity == cell.identity()
                        && observation.relation == crate::records::ComparisonRelationV10::Ordinary
                })
                .map(|observation| &observation.verdict)
                .collect::<Vec<_>>();
            let ordinary = if observations.iter().any(|verdict| {
                matches!(
                    verdict,
                    crate::records::ComparisonObservationVerdictV10::Diverged
                )
            }) {
                "diverged"
            } else if !observations.is_empty()
                && observations.iter().all(|verdict| {
                    matches!(
                        verdict,
                        crate::records::ComparisonObservationVerdictV10::Matched
                    )
                })
            {
                "matched"
            } else {
                "unavailable"
            };
            public["comparison_authority"] = Value::String("selected-attempt-v1".into());
            public["ordinary_comparison"] = Value::String(ordinary.into());
            public["backend_parity"] = match &cell.backend_parity {
                RequiredNullable::Null => Value::Null,
                RequiredNullable::Value(parity) => serde_json::json!({
                    "artifact_verified": true,
                    "clean_match": parity.is_clean_match(),
                    "summary": parity,
                }),
            };
        }
    }
    debug_assert_eq!(*selected_count as usize, selected.len());
    debug_assert_eq!(*recorded_count as usize, cells.len());
    Ok(serde_json::json!({
        "hermit_sha": hermit_sha,
        "population_sha256": population_sha256,
        "recorded": {
            "artifact": {"row_count": row_count, "sha256": artifact_sha256},
            "cell_count": cells.len(),
            "cells": cells,
        },
        "run_id": run_id,
        "selected_by": {
            path: {"cell_count": selected.len(), "cells": selected},
        },
        "source_tree_dirty": source_tree_dirty,
    }))
}

fn project_identity(identity: &CellIdentity) -> Result<Value, String> {
    let CellIdentity {
        lane,
        category,
        test,
        mode,
        backend,
    } = identity;
    for (name, value) in [
        ("lane", lane),
        ("category", category),
        ("test", test),
        ("mode", mode),
        ("backend", backend),
    ] {
        require_nonblank(value, &format!("CellIdentity {name}"))?;
    }
    Ok(serde_json::json!({
        "backend": backend,
        "category": category,
        "lane": lane,
        "mode": mode,
        "test": test,
    }))
}

fn project_cell_result(cell: &CellResult) -> Result<Value, String> {
    let CellResult {
        lane,
        category,
        test,
        mode,
        backend,
        cell_verdict,
        // The schema-6–8 projection does not publish binding authority.
        evidence_binding: _,
    } = cell;
    let identity = CellIdentity {
        lane: lane.clone(),
        category: category.clone(),
        test: test.clone(),
        mode: mode.clone(),
        backend: backend.clone(),
    };
    let mut value = project_identity(&identity)?;
    value["cell_verdict"] = project_verdict(cell_verdict)?;
    Ok(value)
}

fn project_verdict(verdict: &CellVerdict) -> Result<Value, String> {
    let mut value = Map::new();
    match verdict {
        CellVerdict::ComparedAndMatched {
            comparison_tier,
            comparison,
            bitwise_parity,
            compared_log_messages,
        } => {
            value.insert("state".into(), Value::String("compared-and-matched".into()));
            project_compared_verdict(
                &mut value,
                comparison_tier,
                comparison,
                *bitwise_parity,
                compared_log_messages,
            )?;
        }
        CellVerdict::ComparedAndDiverged {
            comparison_tier,
            comparison,
            bitwise_parity,
            compared_log_messages,
        } => {
            value.insert(
                "state".into(),
                Value::String("compared-and-diverged".into()),
            );
            project_compared_verdict(
                &mut value,
                comparison_tier,
                comparison,
                *bitwise_parity,
                compared_log_messages,
            )?;
        }
        CellVerdict::PerformsNoComparisonByDesign {
            comparison_tier,
            reason,
        } => {
            require_nonblank(reason, "cell verdict reason")?;
            value.insert(
                "state".into(),
                Value::String("performs-no-comparison-by-design".into()),
            );
            value.insert(
                "comparison_tier".into(),
                Value::String(comparison_tier_name(comparison_tier).into()),
            );
            value.insert("reason".into(), Value::String(reason.clone()));
        }
        CellVerdict::UnavailableWithReason {
            comparison_tier,
            reason,
        } => {
            require_nonblank(reason, "cell verdict reason")?;
            value.insert(
                "state".into(),
                Value::String("unavailable-with-reason".into()),
            );
            value.insert(
                "comparison_tier".into(),
                Value::String(comparison_tier_name(comparison_tier).into()),
            );
            value.insert("reason".into(), Value::String(reason.clone()));
        }
    }
    Ok(Value::Object(value))
}

fn project_compared_verdict(
    value: &mut Map<String, Value>,
    comparison_tier: &ComparisonTier,
    comparison: &ComparisonSpec,
    bitwise_parity: bool,
    compared_log_messages: &RequiredNullable<ComparedLogCounts>,
) -> Result<(), String> {
    value.insert(
        "comparison_tier".into(),
        Value::String(comparison_tier_name(comparison_tier).into()),
    );
    value.insert("comparison".into(), project_comparison(comparison)?);
    value.insert("bitwise_parity".into(), Value::Bool(bitwise_parity));
    value.insert(
        "compared_log_messages".into(),
        match compared_log_messages {
            RequiredNullable::Null => Value::Null,
            RequiredNullable::Value(counts) => {
                let ComparedLogCounts { left, right } = counts;
                serde_json::json!({"left": left, "right": right})
            }
        },
    );
    Ok(())
}

fn project_comparison(comparison: &ComparisonSpec) -> Result<Value, String> {
    let ComparisonSpec {
        strictness,
        display_name,
        compare_logs,
        compare_io_buffers,
        record_envelope,
        virtualize_time,
        log_scope,
        strip_lines,
        canonicalize_addresses,
        full_trace,
        exact_remainder,
        stripped_prefixes,
        canonicalizations,
        ignore_lines,
        skip_commit,
        skip_detlog,
    } = comparison;
    Ok(serde_json::json!({
        "canonicalizations": canonicalizations,
        "canonicalize_addresses": canonicalize_addresses,
        "compare_io_buffers": compare_io_buffers,
        "compare_logs": compare_logs,
        "display_name": display_name,
        "exact_remainder": exact_remainder,
        "full_trace": full_trace,
        "ignore_lines": ignore_lines,
        "log_scope": compared_log_scope_name(log_scope),
        "record_envelope": record_envelope,
        "skip_commit": skip_commit,
        "skip_detlog": skip_detlog,
        "strictness": comparison_strictness_name(strictness),
        "strip_lines": strip_lines,
        "stripped_prefixes": stripped_prefixes,
        "virtualize_time": virtualize_time,
    }))
}

fn comparison_tier_name(value: &ComparisonTier) -> &'static str {
    match value {
        ComparisonTier::CanonicalBitwise => "canonical-bitwise",
        ComparisonTier::ExitAndStreamEquality => "exit-and-stream-equality",
        ComparisonTier::ExecutionOnlySelfConsistent => "execution-only-self-consistent",
        ComparisonTier::DeclaredButUnverifiable => "declared-but-unverifiable",
    }
}

fn comparison_strictness_name(value: &ComparisonStrictness) -> &'static str {
    match value {
        ComparisonStrictness::Stripped => "stripped",
        ComparisonStrictness::Canonical => "canonical",
    }
}

fn compared_log_scope_name(value: &ComparedLogScope) -> &'static str {
    match value {
        ComparedLogScope::Deterministic => "deterministic",
        ComparedLogScope::Info => "info",
        ComparedLogScope::FullTrace => "full_trace",
    }
}

fn validation_path(row: &HistoryRow) -> Result<String, String> {
    let profile = row
        .profile
        .as_deref()
        .ok_or("HistoryRow with cell_results omits its recorded validate path")?;
    require_nonblank(profile, "HistoryRow validate path")?;
    if let Some(path) = row.cell_results_validate_path() {
        let path = path.as_str();
        if path != profile {
            return Err("schema-8 cell_results path differs from HistoryRow profile".into());
        }
    }
    Ok(profile.to_owned())
}

fn validation_selection(
    row: &HistoryRow,
    evidence: &CellResultsEvidence,
) -> Result<ValidationSelection, String> {
    let commit = row
        .commit
        .as_deref()
        .ok_or("HistoryRow with cell_results omits commit")?;
    require_object_id(commit, "HistoryRow commit")?;
    let path = validation_path(row)?;
    let selected_cells = evidence
        .selected
        .iter()
        .map(cell_identity_string)
        .collect::<Result<BTreeSet<_>, _>>()?;
    if selected_cells.len() != evidence.selected.len() {
        return Err(format!(
            "cell_results for run {:?} repeat a selected identity",
            evidence.run_id
        ));
    }
    Ok(ValidationSelection {
        commit: commit.to_owned(),
        path,
        selected_cells,
    })
}

fn cell_identity_string(identity: &CellIdentity) -> Result<String, String> {
    project_identity(identity)?;
    Ok(format!(
        "{}/{}/{}",
        identity.test, identity.mode, identity.backend
    ))
}

fn history_row_digest(raw: &Value) -> Result<String, String> {
    if !raw.is_object() {
        return Err("cannot canonicalize a non-object HistoryRow".into());
    }
    Ok(sha256(&canonical_bytes(raw)))
}

fn row_is_hermit(row: &HistoryRow) -> bool {
    matches!(
        row.repo.as_deref(),
        None | Some("hermit") | Some("rrnewton/hermit")
    )
}

fn extra_string<'a>(row: &'a HistoryRow, name: &str) -> Result<Option<&'a str>, String> {
    let Some(value) = row.extra.get(name) else {
        return Ok(None);
    };
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(Some)
        .ok_or_else(|| format!("HistoryRow {name} must be a nonempty string"))
}

fn project_series<F>(
    rows: &[Parsed<SeriesRow>],
    manifest: &Value,
    history: &HistoryProjection,
    current_main_ancestry: &mut F,
) -> Result<SeriesProjection, String>
where
    F: FnMut(&str) -> Result<Option<bool>, String>,
{
    let allowed_cells = manifest_cell_identities(manifest)?;
    validate_history_selections(&history.selections, &allowed_cells)?;
    validate_derived_series(rows, &history.derived)?;
    let physical_row_count = rows.len();
    let accumulation = accumulate_series(rows, history)?;
    let ancestry = series_ancestry(&accumulation.prepared, current_main_ancestry)?;
    let SeriesAccumulation {
        prepared,
        selection_by_event,
        projection_results,
        product_results,
        schema_counts,
        represented_run_count,
        admitted_physical_row_count,
        admitted_represented_run_count,
        excluded,
        event_ids: _,
    } = accumulation;
    let rows = decorate_series_rows(prepared, &ancestry)?;
    Ok(SeriesProjection {
        physical_row_count,
        rows,
        ancestry,
        projection_results,
        product_results,
        validate_run_selection: selection_by_event,
        schema_counts,
        represented_run_count,
        admitted_physical_row_count,
        admitted_represented_run_count,
        exclusion_counts: excluded,
    })
}

/// Join every public event span to the already authenticated original census.
/// Interval membership alone is not coverage: sparse, overlapping, omitted or
/// contradictory attempts refuse, including losslessly compressed retries.
fn validate_derived_series(
    rows: &[Parsed<SeriesRow>],
    derived: &BTreeMap<String, DerivedCensus>,
) -> Result<(), String> {
    for (run_id, evidence) in derived {
        let mut remaining = evidence
            .attempts
            .iter()
            .map(|a| {
                (
                    (
                        format!("{}/{}/{}", a.cell.test, a.cell.mode, a.cell.backend),
                        a.attempt,
                    ),
                    a,
                )
            })
            .collect::<BTreeMap<_, _>>();
        if remaining.len() != evidence.attempts.len() {
            return Err("derived census repeats a cell attempt".into());
        }
        for parsed in rows
            .iter()
            .filter(|r| r.typed.producer == SeriesProducer::Validate && &r.typed.run_id == run_id)
        {
            let row = &parsed.typed;
            row.validate_for_read()?;
            row.validate_for_projection()?;
            let span = &row.series;
            if row.schema != SeriesSchema::V3
                || span.tree != evidence.hermit_sha
                || span.source_tree_dirty
            {
                return Err("derived current series has a different source identity".into());
            }
            let last = span.last_run_index.map(Ok).unwrap_or_else(|| {
                span.run_index
                    .checked_add(span.num_runs)
                    .and_then(|v| v.checked_sub(1))
                    .ok_or("derived series span overflow")
            })?;
            if last
                .checked_sub(span.run_index)
                .and_then(|n| n.checked_add(1))
                != Some(span.num_runs)
                || span
                    .attempt
                    .is_some_and(|a| span.num_runs != 1 || a != span.run_index)
            {
                return Err(
                    "derived current series has sparse or contradictory attempt span".into(),
                );
            }
            let matched = remaining
                .range((span.cell.clone(), span.run_index)..=(span.cell.clone(), last))
                .map(|(key, value)| (key.clone(), *value))
                .collect::<Vec<_>>();
            if matched.len() as u64 != span.num_runs {
                return Err("derived current series omits or overlaps original attempts".into());
            }
            for (key, attempt) in matched {
                if span.result != attempt.result || span.failure_class != attempt.failure_class {
                    return Err(
                        "derived current series changes original attempt outcome/classification"
                            .into(),
                    );
                }
                remaining.remove(&key);
            }
        }
        if !remaining.is_empty() {
            return Err("public series omits derived original attempts".into());
        }
    }
    Ok(())
}

fn manifest_cell_identities(manifest: &Value) -> Result<BTreeSet<String>, String> {
    let manifest_cells = manifest
        .get("cells")
        .and_then(Value::as_object)
        .ok_or("projected manifest cells are not an object")?;
    let custom = manifest
        .get("selected_by_full_custom_commands")
        .and_then(Value::as_array)
        .ok_or("projected manifest custom commands are not an array")?;
    let mut allowed_cells = manifest_cells.keys().cloned().collect::<BTreeSet<_>>();
    for cell in custom {
        let identity = cell
            .get("cell")
            .and_then(Value::as_str)
            .ok_or("projected custom command omits cell")?;
        allowed_cells.insert(identity.to_owned());
    }
    Ok(allowed_cells)
}

fn validate_history_selections(
    selections: &BTreeMap<String, ValidationSelection>,
    allowed_cells: &BTreeSet<String>,
) -> Result<(), String> {
    for (run_id, selection) in selections {
        let unknown = selection
            .selected_cells
            .difference(allowed_cells)
            .cloned()
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Err(format!(
                "current validation run {run_id:?} selects unknown cells: {}",
                unknown.join(", ")
            ));
        }
    }
    Ok(())
}

fn accumulate_series(
    rows: &[Parsed<SeriesRow>],
    history: &HistoryProjection,
) -> Result<SeriesAccumulation, String> {
    let mut accumulation = SeriesAccumulation::default();
    for parsed in rows {
        parsed.typed.validate_for_read().map_err(|error| {
            format!(
                "series event {:?} is invalid: {error}",
                parsed.typed.event_id
            )
        })?;
        let row = prepare_series_row(&parsed.typed, &parsed.raw)?;
        if !accumulation.event_ids.insert(row.event_id.clone()) {
            return Err(format!("series rows repeat event_id {:?}", row.event_id));
        }
        record_raw_series_counts(&mut accumulation, &row)?;
        let reasons = series_exclusion_reasons(&row, history);
        if !reasons.is_empty() {
            for reason in reasons {
                let counts = accumulation
                    .excluded
                    .entry((row.schema.clone(), reason))
                    .or_default();
                counts.0 += 1;
                counts.1 = counts
                    .1
                    .checked_add(row.represented_runs)
                    .ok_or("excluded represented-run count overflow")?;
            }
            continue;
        }
        accumulation.admitted_physical_row_count += 1;
        accumulation.admitted_represented_run_count = accumulation
            .admitted_represented_run_count
            .checked_add(row.represented_runs)
            .ok_or("admitted represented-run count overflow")?;
        accumulation
            .projection_results
            .insert(row.event_id.clone(), None);
        accumulation
            .product_results
            .insert(row.event_id.clone(), row.product_result.clone());
        record_validate_selection(&mut accumulation, &row, history)?;
        accumulation.prepared.push(row);
    }
    Ok(accumulation)
}

fn record_raw_series_counts(
    accumulation: &mut SeriesAccumulation,
    row: &PreparedSeries,
) -> Result<(), String> {
    *accumulation
        .schema_counts
        .entry(row.schema.clone())
        .or_insert(0) += 1;
    accumulation.represented_run_count = accumulation
        .represented_run_count
        .checked_add(row.represented_runs)
        .ok_or("series represented-run count overflow")?;
    Ok(())
}

fn series_exclusion_reasons(row: &PreparedSeries, history: &HistoryProjection) -> BTreeSet<String> {
    let mut reasons = BTreeSet::new();
    if row.schema != series_schema_name(&SeriesSchema::V3) {
        reasons.insert("series schema is not stress-series/v3".to_owned());
    }
    if let Some(reason) = &row.projection_reason {
        reasons.insert(reason.clone());
    }
    match row.producer {
        SeriesProducer::PressureTest | SeriesProducer::HermitRepeat => {}
        SeriesProducer::Validate => match history.selections.get(&row.run_id) {
            None => {
                reasons.insert(
                    "validate series row has no admitted terminal validation row".to_owned(),
                );
            }
            Some(selection) => {
                if row.tree != selection.commit {
                    reasons.insert(
                        "validate series row and terminal validation row record different Hermit commits"
                            .to_owned(),
                    );
                }
                if !selection.selected_cells.contains(&row.cell) {
                    reasons.insert(
                        "validate series row names a cell not selected by its validate path"
                            .to_owned(),
                    );
                }
            }
        },
    }
    reasons
}

fn record_validate_selection(
    accumulation: &mut SeriesAccumulation,
    row: &PreparedSeries,
    history: &HistoryProjection,
) -> Result<(), String> {
    if row.producer != SeriesProducer::Validate {
        return Ok(());
    }
    let selection = history
        .selections
        .get(&row.run_id)
        .expect("admitted validate series row has an admitted terminal validation row");
    debug_assert_eq!(row.tree, selection.commit);
    debug_assert!(selection.selected_cells.contains(&row.cell));
    accumulation.selection_by_event.insert(
        row.event_id.clone(),
        serde_json::json!({"path": selection.path, "selected": true}),
    );
    Ok(())
}

fn series_ancestry<F>(
    rows: &[PreparedSeries],
    current_main_ancestry: &mut F,
) -> Result<BTreeMap<String, Option<bool>>, String>
where
    F: FnMut(&str) -> Result<Option<bool>, String>,
{
    let trees = rows
        .iter()
        .map(|row| row.tree.clone())
        .collect::<BTreeSet<_>>();
    let mut ancestry = BTreeMap::new();
    for tree in trees {
        require_object_id(&tree, "series Hermit tree")?;
        ancestry.insert(tree.clone(), current_main_ancestry(&tree)?);
    }
    Ok(ancestry)
}

fn decorate_series_rows(
    rows: Vec<PreparedSeries>,
    ancestry: &BTreeMap<String, Option<bool>>,
) -> Result<Vec<Value>, String> {
    rows.into_iter()
        .map(|mut row| {
            let public = row
                .public
                .as_object_mut()
                .ok_or("prepared series row is not an object")?;
            public.insert(
                "current_main_ancestry".into(),
                ancestry
                    .get(&row.tree)
                    .cloned()
                    .flatten()
                    .map(Value::Bool)
                    .unwrap_or(Value::Null),
            );
            public.insert(
                "product_result".into(),
                row.product_result.map(Value::String).unwrap_or(Value::Null),
            );
            Ok(row.public)
        })
        .collect()
}

fn projection_exclusion_counts(excluded: BTreeMap<(String, String), (u64, u64)>) -> Vec<Value> {
    excluded
        .into_iter()
        .map(
            |((schema, reason), (physical_row_count, represented_run_count))| {
                serde_json::json!({
                    "physical_row_count": physical_row_count,
                    "reason": reason,
                    "represented_run_count": represented_run_count,
                    "schema": schema,
                })
            },
        )
        .collect()
}

fn prepare_series_row(row: &SeriesRow, raw: &Value) -> Result<PreparedSeries, String> {
    let raw = raw
        .as_object()
        .ok_or("SeriesRow source value is not an object")?;
    let SeriesRow {
        source,
        schema,
        event_id,
        event_type,
        emitted_at,
        team,
        host,
        producer,
        run_id,
        series,
    } = row;
    let _reader_only_source_is_deliberately_omitted = source;
    let schema_name = series_schema_name(schema).to_owned();
    let producer_name = series_producer_name(producer);
    let payload_source = raw.get("series").ok_or("SeriesRow omits series")?;
    let (payload, cell, tree, represented_runs) =
        project_series_payload(schema, series, payload_source)?;
    let projection_reason = row.validate_for_projection().err();
    let (_product_result_available, product_result) = product_result(schema, series)?;
    Ok(PreparedSeries {
        public: serde_json::json!({
            "emitted_at": emitted_at,
            "event_id": event_id,
            "event_type": event_type,
            "host": host,
            "producer": producer_name,
            "run_id": run_id,
            "schema": schema_name,
            "series": payload,
            "team": team,
        }),
        event_id: event_id.clone(),
        schema: schema_name,
        producer: *producer,
        run_id: run_id.clone(),
        cell,
        tree,
        represented_runs,
        projection_reason,
        product_result,
    })
}

fn project_series_payload(
    schema: &SeriesSchema,
    payload: &SeriesPayload,
    raw: &Value,
) -> Result<(Value, String, String, u64), String> {
    let raw = raw
        .as_object()
        .ok_or("SeriesPayload source value is not an object")?;
    let SeriesPayload {
        cell,
        tree,
        detcore_tree,
        outcome,
        result,
        failure_class,
        no_verdict_evidence,
        pressure_evidence,
        run_index,
        attempt,
        num_runs,
        last_run_index,
        main_ancestry,
        runtime,
        source_tree_dirty,
        depth,
        coordinates,
        first_divergent_messages,
        machine_shortname,
        kernel_version,
        host_capabilities,
    } = payload;
    let _deliberately_omitted = (main_ancestry, first_divergent_messages);
    let _projected_by_typed_helpers = (
        detcore_tree,
        result,
        failure_class,
        no_verdict_evidence,
        pressure_evidence,
        attempt,
        last_run_index,
        runtime,
        source_tree_dirty,
        depth,
        coordinates,
        machine_shortname,
        kernel_version,
        host_capabilities,
    );
    let mut public = Map::new();
    public.insert("cell".into(), Value::String(cell.clone()));
    public.insert("tree".into(), Value::String(tree.clone()));
    public.insert(
        "outcome".into(),
        Value::String(series_outcome_name(outcome).into()),
    );
    public.insert("run_index".into(), Value::from(*run_index));
    public.insert("num_runs".into(), Value::from(*num_runs));
    project_series_execution_fields(&mut public, raw, payload)?;
    project_series_environment_fields(&mut public, raw, schema, payload)?;
    Ok((Value::Object(public), cell.clone(), tree.clone(), *num_runs))
}

fn project_series_execution_fields(
    public: &mut Map<String, Value>,
    raw: &Map<String, Value>,
    payload: &SeriesPayload,
) -> Result<(), String> {
    if raw.contains_key("detcore_tree") {
        public.insert(
            "detcore_tree".into(),
            payload
                .detcore_tree
                .as_ref()
                .map(|value| Value::String(value.clone()))
                .unwrap_or(Value::Null),
        );
    }
    if raw.contains_key("result") {
        public.insert(
            "result".into(),
            payload
                .result
                .map(|value| Value::String(observed_result_name(value).into()))
                .unwrap_or(Value::Null),
        );
    }
    if raw.contains_key("failure_class") {
        public.insert(
            "failure_class".into(),
            payload
                .failure_class
                .map(|value| Value::String(failure_class_name(value).into()))
                .unwrap_or(Value::Null),
        );
    }
    if raw.contains_key("attempt") {
        public.insert(
            "attempt".into(),
            payload.attempt.map(Value::from).unwrap_or(Value::Null),
        );
    }
    if raw.contains_key("last_run_index") {
        public.insert(
            "last_run_index".into(),
            payload
                .last_run_index
                .map(Value::from)
                .unwrap_or(Value::Null),
        );
    }
    if raw.contains_key("runtime") {
        public.insert(
            "runtime".into(),
            payload
                .runtime
                .as_ref()
                .map(project_runtime)
                .unwrap_or(Value::Null),
        );
    }
    if raw.contains_key("source_tree_dirty") {
        public.insert(
            "source_tree_dirty".into(),
            Value::Bool(payload.source_tree_dirty),
        );
    }
    if raw.contains_key("depth") {
        public.insert("depth".into(), project_depth(&payload.depth)?);
    }
    Ok(())
}

fn project_series_environment_fields(
    public: &mut Map<String, Value>,
    raw: &Map<String, Value>,
    schema: &SeriesSchema,
    payload: &SeriesPayload,
) -> Result<(), String> {
    require_optional_nonblank(
        payload.machine_shortname.as_deref(),
        "series machine_shortname",
    )?;
    require_optional_nonblank(payload.kernel_version.as_deref(), "series kernel_version")?;
    if raw.contains_key("coordinates") {
        public.insert(
            "coordinates".into(),
            payload
                .coordinates
                .as_ref()
                .map(project_coordinates)
                .unwrap_or(Value::Null),
        );
    }
    if !matches!(schema, SeriesSchema::V1) && raw.contains_key("machine_shortname") {
        public.insert(
            "machine_shortname".into(),
            payload
                .machine_shortname
                .as_ref()
                .map(|value| Value::String(value.clone()))
                .unwrap_or(Value::Null),
        );
    }
    if !matches!(schema, SeriesSchema::V1) && raw.contains_key("kernel_version") {
        public.insert(
            "kernel_version".into(),
            payload
                .kernel_version
                .as_ref()
                .map(|value| Value::String(value.clone()))
                .unwrap_or(Value::Null),
        );
    }
    if !matches!(schema, SeriesSchema::V1) && raw.contains_key("host_capabilities") {
        public.insert(
            "host_capabilities".into(),
            payload
                .host_capabilities
                .as_ref()
                .map(project_host_capabilities)
                .transpose()?
                .unwrap_or(Value::Null),
        );
    }
    if raw.contains_key("no_verdict_evidence") {
        public.insert(
            "no_verdict_evidence".into(),
            payload
                .no_verdict_evidence
                .as_ref()
                .map(project_no_verdict_evidence)
                .transpose()?
                .unwrap_or(Value::Null),
        );
    }
    if raw.contains_key("pressure_evidence") {
        public.insert(
            "pressure_evidence".into(),
            serde_json::to_value(&payload.pressure_evidence)
                .map_err(|error| format!("cannot project pressure_evidence: {error}"))?,
        );
    }
    Ok(())
}

fn project_depth(depth: &BTreeMap<String, SourceDepth>) -> Result<Value, String> {
    let mut public = Map::new();
    for (repository, depth) in depth {
        require_nonblank(repository, "series depth repository")?;
        let SourceDepth {
            commits,
            first_parent,
        } = depth;
        public.insert(
            repository.clone(),
            serde_json::json!({"commits": commits, "first_parent": first_parent}),
        );
    }
    Ok(Value::Object(public))
}

fn project_coordinates(coordinates: &SeriesCoordinates) -> Value {
    let SeriesCoordinates {
        first_divergent_scheduler_turn,
        first_divergent_virtual_nanoseconds,
        first_divergent_record,
        first_divergent_syscall,
    } = coordinates;
    serde_json::json!({
        "first_divergent_record": first_divergent_record,
        "first_divergent_scheduler_turn": first_divergent_scheduler_turn,
        "first_divergent_syscall": first_divergent_syscall,
        "first_divergent_virtual_nanoseconds": first_divergent_virtual_nanoseconds,
    })
}

fn project_runtime(runtime: &SeriesRuntime) -> Value {
    let SeriesRuntime {
        run1,
        run2,
        wall_time_min_ms,
        wall_time_max_ms,
    } = runtime;
    serde_json::json!({
        "run1": run1.as_ref().map(project_runtime_measurement),
        "run2": run2.as_ref().map(project_runtime_measurement),
        "wall_time_max_ms": wall_time_max_ms,
        "wall_time_min_ms": wall_time_min_ms,
    })
}

fn project_runtime_measurement(measurement: &SeriesRuntimeMeasurement) -> Value {
    let SeriesRuntimeMeasurement {
        scheduler_turns,
        virtual_nanoseconds,
        syscalls,
    } = measurement;
    serde_json::json!({
        "scheduler_turns": scheduler_turns,
        "syscalls": syscalls,
        "virtual_nanoseconds": virtual_nanoseconds,
    })
}

fn project_host_capabilities(
    capabilities: &BTreeMap<HostCapability, HostCapabilityVerdict>,
) -> Result<Value, String> {
    let mut public = Map::new();
    for (capability, verdict) in capabilities {
        let name = match capability {
            HostCapability::CpuidFaulting => "cpuid-faulting",
            HostCapability::Kvm => "kvm",
        };
        let HostCapabilityVerdict { present, evidence } = verdict;
        require_nonblank(evidence, "host capability evidence")?;
        public.insert(
            name.into(),
            serde_json::json!({"evidence": evidence, "present": present}),
        );
    }
    Ok(Value::Object(public))
}

fn project_no_verdict_evidence(evidence: &SeriesNoVerdictEvidence) -> Result<Value, String> {
    let SeriesNoVerdictEvidence {
        evidence_sha256,
        attempts,
    } = evidence;
    require_sha256(evidence_sha256, "no_verdict_evidence.evidence_sha256")?;
    Ok(serde_json::json!({
        "attempts": attempts.iter().map(project_attempt_disposition).collect::<Result<Vec<_>, _>>()?,
        "evidence_sha256": evidence_sha256,
    }))
}

fn project_attempt_disposition(attempt: &SeriesAttemptDisposition) -> Result<Value, String> {
    let SeriesAttemptDisposition {
        index,
        kind,
        detail,
        attempt_outcome,
        disposition,
        error_kind,
        status,
        signal,
        timed_out,
        verification_report_sha256,
    } = attempt;
    Ok(serde_json::json!({
        "attempt_outcome": attempt_outcome,
        "disposition": series_outcome_name(disposition),
        "detail": detail,
        "error_kind": error_kind,
        "index": index,
        "kind": no_verdict_kind_name(kind),
        "signal": signal,
        "status": status,
        "timed_out": timed_out,
        "verification_report_sha256": verification_report_sha256,
    }))
}

fn product_result(
    schema: &SeriesSchema,
    payload: &SeriesPayload,
) -> Result<(bool, Option<String>), String> {
    match schema {
        SeriesSchema::V1 | SeriesSchema::V2 => Ok((false, None)),
        SeriesSchema::V3 => {
            let result = match payload.outcome {
                SeriesOutcome::Passed => match (payload.result, payload.failure_class) {
                    (Some(ObservedResult::Pass), None) => Some("pass"),
                    _ => {
                        return Err("stress-series/v3 passed classification is contradictory".into())
                    }
                },
                SeriesOutcome::Diverged => match (payload.result, payload.failure_class) {
                    (
                        Some(
                            ObservedResult::DeterminismFailure
                            | ObservedResult::ParityFailure
                            | ObservedResult::ReplayFailure,
                        ),
                        Some(FailureClass::ProductFailure),
                    ) => Some("product_failure"),
                    _ => {
                        return Err(
                            "stress-series/v3 diverged classification is contradictory".into()
                        )
                    }
                },
                SeriesOutcome::Errored => match (payload.result, payload.failure_class) {
                    (Some(ObservedResult::CrashError), Some(FailureClass::ProductFailure)) => {
                        Some("product_failure")
                    }
                    (None, Some(FailureClass::UnderstoodInfrastructureFailure)) => None,
                    _ => {
                        return Err(
                            "stress-series/v3 errored classification is contradictory".into()
                        )
                    }
                },
                SeriesOutcome::Timeout => match (payload.result, payload.failure_class) {
                    (Some(ObservedResult::Timeout), Some(FailureClass::NoResult)) => None,
                    _ => {
                        return Err(
                            "stress-series/v3 timeout classification is contradictory".into()
                        )
                    }
                },
                SeriesOutcome::NoResult => match (payload.result, payload.failure_class) {
                    (Some(ObservedResult::Oom), Some(FailureClass::NoResult))
                    | (None, Some(FailureClass::NoResult)) => None,
                    _ => {
                        return Err(
                            "stress-series/v3 no_result classification is contradictory".into()
                        )
                    }
                },
                SeriesOutcome::Skipped => match (payload.result, payload.failure_class) {
                    (None, Some(FailureClass::UnderstoodPrerequisiteFailure)) => None,
                    _ => {
                        return Err(
                            "stress-series/v3 skipped classification is contradictory".into()
                        )
                    }
                },
            };
            Ok((true, result.map(str::to_owned)))
        }
    }
}

fn series_schema_name(value: &SeriesSchema) -> &'static str {
    match value {
        SeriesSchema::V1 => "stress-series/v1",
        SeriesSchema::V2 => "stress-series/v2",
        SeriesSchema::V3 => "stress-series/v3",
    }
}

fn series_producer_name(value: &SeriesProducer) -> &'static str {
    match value {
        SeriesProducer::Validate => "validate",
        SeriesProducer::PressureTest => "pressure-test",
        SeriesProducer::HermitRepeat => "hermit-repeat",
    }
}

fn series_outcome_name(value: &SeriesOutcome) -> &'static str {
    match value {
        SeriesOutcome::Passed => "passed",
        SeriesOutcome::Diverged => "diverged",
        SeriesOutcome::NoResult => "no_result",
        SeriesOutcome::Timeout => "timeout",
        SeriesOutcome::Errored => "errored",
        SeriesOutcome::Skipped => "skipped",
    }
}

fn observed_result_name(value: ObservedResult) -> &'static str {
    match value {
        ObservedResult::Pass => "pass",
        ObservedResult::DeterminismFailure => "determinism-failure",
        ObservedResult::ParityFailure => "parity-failure",
        ObservedResult::ReplayFailure => "replay-failure",
        ObservedResult::CrashError => "crash-error",
        ObservedResult::Timeout => "timeout",
        ObservedResult::Oom => "oom",
        ObservedResult::SandboxDenied => "sandbox-denied",
        ObservedResult::InfrastructureError => "infrastructure-error",
    }
}

fn failure_class_name(value: FailureClass) -> &'static str {
    match value {
        FailureClass::ProductFailure => "product_failure",
        FailureClass::UnderstoodInfrastructureFailure => "understood_infrastructure_failure",
        FailureClass::UnderstoodPrerequisiteFailure => "understood_prerequisite_failure",
        FailureClass::NoResult => "no_result",
    }
}

fn no_verdict_kind_name(value: &SeriesNoVerdictKind) -> &'static str {
    match value {
        SeriesNoVerdictKind::Unspecified => "unspecified",
        SeriesNoVerdictKind::ComparisonRefused => "comparison_refused",
        SeriesNoVerdictKind::NotRun => "not_run",
        SeriesNoVerdictKind::FirstRunRejected => "first_run_rejected",
        SeriesNoVerdictKind::ContainerFailed => "container_failed",
        SeriesNoVerdictKind::InfrastructureError => "infrastructure_error",
        SeriesNoVerdictKind::MissingReportTimeout => "missing_report_timeout",
        SeriesNoVerdictKind::NoncanonicalMatch => "noncanonical_match",
        SeriesNoVerdictKind::NoncanonicalDivergence => "noncanonical_divergence",
    }
}

fn require_object_id(value: &str, label: &str) -> Result<(), String> {
    if value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!("{label} must be lowercase 40-hex"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_identity() -> Value {
        serde_json::json!({
            "lane": "portable",
            "category": "fixture",
            "test": "fixture/test",
            "mode": "verify",
            "backend": "ptrace",
        })
    }

    fn comparison() -> Value {
        serde_json::json!({
            "strictness": "canonical",
            "display_name": "BitwiseInfoV1",
            "compare_logs": true,
            "compare_io_buffers": true,
            "record_envelope": "all_records_v1",
            "virtualize_time": true,
            "log_scope": "info",
            "strip_lines": false,
            "canonicalize_addresses": true,
            "full_trace": true,
            "exact_remainder": true,
            "stripped_prefixes": ["real-wall-clock-prefix/v1"],
            "canonicalizations": ["host-address-to-first-appearance-ordinal/v1"],
            "ignore_lines": false,
            "skip_commit": false,
            "skip_detlog": false,
        })
    }

    fn cell_results(run_id: &str, commit: &str, schema: u64) -> Value {
        let identity = cell_identity();
        let population_sha256 = sha256(&canonical_bytes(&Value::Array(vec![identity.clone()])));
        let mut evidence = serde_json::json!({
            "run_id": run_id,
            "hermit_sha": commit,
            "source_tree_dirty": false,
            "selected_count": 1,
            "recorded_count": 1,
            "population_sha256": population_sha256,
            "artifact": {
                "path": format!("ignored/validate/artifacts/{run_id}.jsonl"),
                "sha256": "b".repeat(64),
                "row_count": 1,
            },
            "selected": [identity],
            "cells": [{
                "lane": "portable",
                "category": "fixture",
                "test": "fixture/test",
                "mode": "verify",
                "backend": "ptrace",
                "cell_verdict": {
                    "state": "compared-and-matched",
                    "comparison_tier": "canonical-bitwise",
                    "comparison": comparison(),
                    "bitwise_parity": true,
                    "compared_log_messages": {"left": 3, "right": 3},
                },
            }],
        });
        if schema == 8 {
            evidence["path"] = Value::String("full".into());
        }
        evidence
    }

    fn history_row(schema: u64, record_id: &str, run_id: &str, commit: &str) -> Value {
        serde_json::json!({
            "schema_version": schema,
            "record_id": record_id,
            "run_id": run_id,
            "started_at": "2026-09-02T00:00:00Z",
            "finished_at": "2026-09-02T00:01:00Z",
            "host": "fixture-host",
            "repo": "hermit",
            "profile": "full",
            "selection_mode": "full",
            "commit": commit,
            "reverie_base_sha": "d".repeat(40),
            "reverie_base_tree": "e".repeat(40),
            "reverie_pin_current": true,
            "tree_dirty": false,
            "result": "pass",
            "exit_code": 0,
            "executed_tests": 1,
            "filtered_tests": 0,
            "checks": 1,
            "gates_run": 1,
            "gates_expected": 1,
            "failures": 0,
            "gates": [{
                "name": "fixture gate",
                "result": "pass",
                "kind": "test",
                "exit_code": 0,
                "real_seconds": 1.25,
                "source_node": "test.fixture",
                "failed_substeps": [],
                "reported": true,
                "retries": 0,
            }],
            "cell_results": cell_results(run_id, commit, schema),
        })
    }

    fn manifest() -> Value {
        serde_json::json!({
            "schema": 2,
            "manifest_schema": 3,
            "manifest_sha256": "a".repeat(64),
            "tests": [{
                "id": "fixture/test",
                "description": "fixture description",
                "category": "fixture",
                "lane": "portable",
                "requires": [],
                "occasional": false,
                "program": null,
                "direct": {"kind": "shell", "command": "/bin/true"},
                "build": null,
                "observation": {
                    "status": true,
                    "stdout": true,
                    "stderr": false,
                    "artifacts": [],
                },
                "preprocessors": [],
            }],
            "cells": [{
                "test": "fixture/test",
                "category": "fixture",
                "lane": "portable",
                "mode": "verify",
                "backend": "ptrace",
                "selected_by_full": true,
                "not_selected_by_full_reason": null,
                "not_applicable_reason": null,
                "timeout_seconds": 30,
                "cpu_timeout_seconds": 30,
                "guest_args": [],
                "workdir": null,
                "current_reproducer": {
                    "argv": ["test-harness", "run"],
                    "shell_command": "test-harness run",
                },
                "current_reproducer_unavailable_reason": null,
            }],
            "selected_by_full_custom_commands": [],
        })
    }

    #[test]
    fn actual_manifest_metadata_preserves_separate_cpu_and_wall_limits() {
        let hermit = std::path::Path::new(file!())
            .parent()
            .unwrap()
            .join("../../hermit")
            .canonicalize()
            .unwrap();
        let metadata = hermit_manifest_plan::manifest_metadata::build_export(&hermit).unwrap();
        assert_eq!(metadata.schema, 2);
        let encoded = serde_json::to_vec(&metadata).unwrap();
        let result = project(&encoded, "", "", None, |_| {
            panic!("empty history must not query ancestry")
        })
        .unwrap();
        assert_eq!(
            result["manifest"]["cells"].as_object().unwrap().len(),
            metadata.cells.len()
        );
        assert!(!metadata.cells.is_empty());
        for cell in &metadata.cells {
            let identity = format!("{}/{}/{}", cell.test, cell.mode, cell.backend);
            let projected = &result["manifest"]["cells"][&identity];
            assert_eq!(projected["timeout_seconds"], cell.timeout_seconds);
            assert_eq!(projected["cpu_timeout_seconds"], cell.cpu_timeout_seconds);
        }

        let mut distinct = manifest();
        distinct["cells"][0]["timeout_seconds"] = Value::from(30);
        distinct["cells"][0]["cpu_timeout_seconds"] = Value::from(7);
        let result = project(&canonical_bytes(&distinct), "", "", None, |_| Ok(None)).unwrap();
        let cell = &result["manifest"]["cells"]["fixture/test/verify/ptrace"];
        assert_eq!(cell["timeout_seconds"], 30);
        assert_eq!(cell["cpu_timeout_seconds"], 7);
        for schema in [0, 1, 3] {
            let mut invalid = distinct.clone();
            invalid["schema"] = Value::from(schema);
            assert!(project(&canonical_bytes(&invalid), "", "", None, |_| Ok(None)).is_err());
        }
        for field in ["timeout_seconds", "cpu_timeout_seconds"] {
            let mut missing = distinct.clone();
            missing["cells"][0].as_object_mut().unwrap().remove(field);
            assert!(project(&canonical_bytes(&missing), "", "", None, |_| Ok(None)).is_err());
            let mut zero = distinct.clone();
            zero["cells"][0][field] = Value::from(0);
            assert!(project(&canonical_bytes(&zero), "", "", None, |_| Ok(None)).is_err());
        }
    }

    fn series(run_id: &str, tree: &str, event_id: &str) -> Value {
        serde_json::json!({
            "schema": "stress-series/v3",
            "event_id": event_id,
            "event_type": "series.observation",
            "emitted_at": "2026-09-02T00:02:00Z",
            "team": "hermit",
            "host": "fixture-host",
            "producer": "validate",
            "run_id": run_id,
            "series": {
                "cell": "fixture/test/verify/ptrace",
                "tree": tree,
                "detcore_tree": "d".repeat(40),
                "outcome": "passed",
                "result": "pass",
                "failure_class": null,
                "run_index": 1,
                "attempt": 1,
                "num_runs": 1,
                "last_run_index": 1,
                "main_ancestry": true,
                "runtime": {
                    "run1": {"scheduler_turns": 4, "virtual_nanoseconds": 5, "syscalls": 6},
                    "run2": {"scheduler_turns": 7, "virtual_nanoseconds": 8, "syscalls": 9},
                    "wall_time_min_ms": 1.0,
                    "wall_time_max_ms": 2.0,
                },
                "source_tree_dirty": false,
                "depth": {"hermit": {"commits": 3, "first_parent": 2}},
                "coordinates": null,
                "machine_shortname": "fixture-host",
                "kernel_version": "6.1-fixture",
                "host_capabilities": {
                    "cpuid-faulting": {"present": true, "evidence": "cpuid fixture"},
                    "kvm": {"present": false, "evidence": "kvm fixture"},
                },
            },
        })
    }

    fn jsonl(values: &[Value]) -> String {
        values
            .iter()
            .map(|value| String::from_utf8(canonical_bytes(value)).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + if values.is_empty() { "" } else { "\n" }
    }

    fn assert_no_stored_measurement_flag(value: &Value) {
        match value {
            Value::Array(values) => {
                for value in values {
                    assert_no_stored_measurement_flag(value);
                }
            }
            Value::Object(object) => {
                for (key, value) in object {
                    assert_ne!(key, "measured");
                    assert_ne!(key, "never_measured");
                    assert_no_stored_measurement_flag(value);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn blank_jsonl_lines_refuse() {
        let error = project(&canonical_bytes(&manifest()), "\n", "", None, |_| {
            Ok(Some(true))
        })
        .unwrap_err();
        assert!(
            error.contains("HistoryRow input line 1 is blank"),
            "{error}"
        );
    }

    #[test]
    fn zero_inputs_emit_the_v2_empty_projection_without_gap_inventory() {
        let value = project(&canonical_bytes(&manifest()), "", "", None, |_| {
            panic!("empty series must not request ancestry")
        })
        .unwrap();
        assert_eq!(value["schema_version"], 2);
        for field in [
            "validation_history_rows",
            "current_validation_run_rows",
            "series_rows",
            "projection_exclusion_counts",
        ] {
            assert_eq!(value[field], serde_json::json!([]), "{field}");
        }
        for field in [
            "current_main_ancestry",
            "projection_results",
            "product_results",
            "validate_run_selection",
        ] {
            assert_eq!(value[field], serde_json::json!({}), "{field}");
        }
        assert!(value.get("validation_incomplete_data").is_none());
        assert!(value.get("missing_executions").is_none());
        assert_no_stored_measurement_flag(&value);
    }

    #[test]
    fn website_schema10_projects_verified_operands_and_refuses_changed_artifacts() {
        use crate::validation_evidence::{fixtures::Store, verify};
        let store = Store::new();
        let row = store.exact("selected-attempt-v1/matched");
        let CellResultsForWebsite::Complete(evidence) =
            failure_obligations::cell_results_for_website_with(&row, |row| verify(row, &store.0))
                .unwrap()
        else {
            panic!("exact selected-attempt fixture must be admitted")
        };
        let public =
            project_cell_results(&row, evidence.as_ref(), evidence.modern.as_deref()).unwrap();
        let cell = &public["recorded"]["cells"][0];
        assert_eq!(cell["comparison_authority"], "selected-attempt-v1");
        assert_eq!(cell["ordinary_comparison"], "matched");
        assert_eq!(cell["backend_parity"]["artifact_verified"], true);
        assert_eq!(cell["backend_parity"]["clean_match"], true);
        assert!(project_cell_results(&row, evidence.as_ref(), None).is_err());

        let raw = serde_json::to_value(&row).unwrap();
        let path = store
            .0
            .join(raw["cell_results"]["artifact"]["path"].as_str().unwrap());
        std::fs::write(&path, b"changed operand bytes\n").unwrap();
        assert!(matches!(
            failure_obligations::cell_results_for_website_with(&row, |row| verify(row, &store.0))
                .unwrap(),
            CellResultsForWebsite::Unavailable(_)
        ));
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            failure_obligations::cell_results_for_website_with(&row, |row| verify(row, &store.0))
                .unwrap(),
            CellResultsForWebsite::Unavailable(_)
        ));
        let legacy = store.exact("matched");
        assert!(matches!(
            failure_obligations::cell_results_for_website_with(&legacy, |row| verify(
                row, &store.0
            ))
            .unwrap(),
            CellResultsForWebsite::Unavailable(_)
        ));
    }

    #[test]
    fn website_schema9_retains_ordinary_evidence_without_inventing_backend_parity() {
        let mut raw = history_row(8, "record-nine", "run-nine", &"c".repeat(40));
        raw["schema_version"] = Value::from(9);
        let row: HistoryRow = serde_json::from_value(raw).unwrap();
        assert!(history_exclusion_reasons(&row).unwrap().is_empty());
        let CellResultsForWebsite::Complete(evidence) =
            failure_obligations::cell_results_for_website(&row).unwrap()
        else {
            panic!("schema nine ordinary evidence must remain readable")
        };
        let public =
            project_cell_results(&row, evidence.as_ref(), evidence.modern.as_deref()).unwrap();
        assert_eq!(
            public["recorded"]["cells"][0]["cell_verdict"]["state"],
            "compared-and-matched"
        );
        assert!(public["recorded"]["cells"][0]
            .get("backend_parity")
            .is_none());
        assert!(public["recorded"]["cells"][0]
            .get("comparison_authority")
            .is_none());
    }

    #[test]
    fn history_admission_is_strict_and_exclusion_reasons_can_overlap() {
        let commit = "c".repeat(40);
        let admitted = history_row(7, "record-good", "run-good", &commit);
        let legacy = history_row(6, "record-legacy", "run-legacy", &commit);
        let mut incomplete = history_row(7, "record-incomplete", "run-incomplete", &commit);
        incomplete.as_object_mut().unwrap().remove("profile");
        incomplete.as_object_mut().unwrap().remove("result");
        let values = [admitted, legacy, incomplete];
        let parsed = parse_jsonl::<HistoryRow>(&jsonl(&values), "HistoryRow").unwrap();
        let projected = project_history(&parsed, None).unwrap();

        assert_eq!(projected.history.len(), 1);
        assert_eq!(projected.current.len(), 1);
        assert_eq!(projected.selections.len(), 1);
        assert_eq!(projected.history[0]["run_id"], "run-good");
        assert_eq!(values.len() - projected.history.len(), 2);
        assert!(projected
            .exclusion_counts
            .iter()
            .any(|((schema, reason), counts)| {
                schema == "6"
                    && reason == "validation HistoryRow schema is not 7, 8, 9 or 10"
                    && *counts == (1, 1)
            }));
        assert!(projected
            .exclusion_counts
            .iter()
            .any(|((schema, reason), counts)| {
                schema == "7"
                    && reason == "validation HistoryRow omits profile"
                    && *counts == (1, 1)
            }));
        assert!(projected
            .exclusion_counts
            .iter()
            .any(|((schema, reason), counts)| {
                schema == "7" && reason == "validation HistoryRow omits result" && *counts == (1, 1)
            }));
        let audited_physical: u64 = projected
            .exclusion_counts
            .values()
            .map(|counts| counts.0)
            .sum();
        assert!(audited_physical > (values.len() - projected.history.len()) as u64);
    }

    #[test]
    fn run_number_is_optional_positive_u64_display_metadata() {
        let commit = "c".repeat(40);
        let cases = [
            (None, None),
            (Some(Value::Null), None),
            (Some(Value::String("17".into())), None),
            (Some(Value::Bool(true)), None),
            (Some(serde_json::json!(17.5)), None),
            (Some(Value::from(0_u64)), None),
            (Some(Value::from(-1_i64)), None),
            (Some(Value::from(17_u64)), Some(17_u64)),
            (Some(Value::from(u64::MAX)), Some(u64::MAX)),
        ];

        for (run_number, expected) in cases {
            let mut row = history_row(7, "record-good", "run-good", &commit);
            if let Some(run_number) = run_number {
                row["run_number"] = run_number;
            }
            let parsed = parse_jsonl::<HistoryRow>(&jsonl(&[row]), "HistoryRow").unwrap();
            let projected = project_history(&parsed, None).unwrap();
            assert_eq!(
                projected.history[0]
                    .get("run_number")
                    .and_then(Value::as_u64),
                expected
            );
            assert_eq!(projected.history.len(), 1);
            assert_eq!(projected.current.len(), 1);
        }
    }

    #[test]
    fn schema7_validate_and_v3_series_exact_join_are_admitted() {
        let commit = "c".repeat(40);
        let good_history = history_row(7, "record-good", "run-good", &commit);
        let mut legacy_series = series("missing-run", &commit, "event-legacy");
        legacy_series["schema"] = Value::String("stress-series/v1".into());
        let good_series = series("run-good", &commit, "event-good");
        let mut dirty_series = series("run-good", &commit, "event-dirty");
        dirty_series["series"]["source_tree_dirty"] = Value::Bool(true);
        let value = project(
            &canonical_bytes(&manifest()),
            &jsonl(&[good_history]),
            &jsonl(&[good_series, legacy_series, dirty_series]),
            None,
            |tree| {
                assert_eq!(tree, commit);
                Ok(Some(true))
            },
        )
        .unwrap();

        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["input"]["series_rows"]["row_count"], 3);
        assert_eq!(value["counts"]["series_physical_row_count"], 1);
        assert_eq!(value["counts"]["projection_admitted_physical_row_count"], 1);
        assert_eq!(value["counts"]["projection_excluded_physical_row_count"], 2);
        assert_eq!(value["series_rows"].as_array().unwrap().len(), 1);
        assert_eq!(value["series_rows"][0]["event_id"], "event-good");
        assert_eq!(value["projection_results"]["event-good"], Value::Null);
        assert_eq!(value["product_results"]["event-good"], "pass");
        assert_eq!(
            value["validate_run_selection"]["event-good"]["path"],
            "full"
        );
        assert_eq!(value["series_rows"][0]["current_main_ancestry"], true);
        assert!(value["series_rows"][0]
            .get("projection_validation_reason")
            .is_none());
        assert!(value["series_rows"][0]
            .get("product_result_available")
            .is_none());
        let audit = value["projection_exclusion_counts"].as_array().unwrap();
        assert!(audit.iter().any(|row| {
            row["schema"] == "stress-series/v1"
                && row["reason"] == "series schema is not stress-series/v3"
                && row["physical_row_count"] == 1
        }));
        assert!(audit.iter().any(|row| {
            row["schema"] == "stress-series/v1"
                && row["reason"] == "validate series row has no admitted terminal validation row"
                && row["physical_row_count"] == 1
        }));
        let audit_physical: u64 = audit
            .iter()
            .map(|row| row["physical_row_count"].as_u64().unwrap())
            .sum();
        assert!(
            audit_physical
                > value["counts"]["projection_excluded_physical_row_count"]
                    .as_u64()
                    .unwrap(),
            "overlapping reason counts must not be treated as a disjoint excluded total"
        );
        assert_eq!(
            value["input"]["series_rows"]["row_count"].as_u64().unwrap(),
            value["counts"]["projection_admitted_physical_row_count"]
                .as_u64()
                .unwrap()
                + value["counts"]["projection_excluded_physical_row_count"]
                    .as_u64()
                    .unwrap()
        );
        assert_no_stored_measurement_flag(&value);
    }

    #[test]
    fn pressure_v3_is_admitted_without_a_validation_row() {
        let commit = "d".repeat(40);
        let mut pressure = series("pressure-run", &commit, "event-pressure");
        pressure["producer"] = Value::String("pressure-test".into());
        let value = project(
            &canonical_bytes(&manifest()),
            "",
            &jsonl(&[pressure]),
            None,
            |_| Ok(Some(true)),
        )
        .unwrap();
        assert_eq!(value["series_rows"].as_array().unwrap().len(), 1);
        assert_eq!(value["series_rows"][0]["producer"], "pressure-test");
        assert_eq!(value["validate_run_selection"], serde_json::json!({}));
    }

    #[test]
    fn hermit_repeat_v3_is_admitted_without_a_validation_row() {
        let commit = "d".repeat(40);
        let mut repeated = series("repeat-run", &commit, "event-repeat");
        repeated["producer"] = Value::String("hermit-repeat".into());
        let value = project(
            &canonical_bytes(&manifest()),
            "",
            &jsonl(&[repeated]),
            None,
            |_| Ok(Some(true)),
        )
        .unwrap();
        assert_eq!(value["series_rows"].as_array().unwrap().len(), 1);
        assert_eq!(value["series_rows"][0]["producer"], "hermit-repeat");
        assert_eq!(value["validate_run_selection"], serde_json::json!({}));
    }

    #[test]
    fn container_failure_projection_retains_crash_and_no_comparison() {
        let mut row: SeriesRow = serde_json::from_value(series(
            "container-run",
            &"d".repeat(40),
            "event-container-failed",
        ))
        .unwrap();
        row.producer = SeriesProducer::PressureTest;
        row.series.outcome = SeriesOutcome::Errored;
        row.series.result = Some(ObservedResult::CrashError);
        row.series.failure_class = Some(FailureClass::ProductFailure);
        row.series.last_run_index = None;
        row.series.no_verdict_evidence = Some(SeriesNoVerdictEvidence {
            evidence_sha256: "b".repeat(64),
            attempts: vec![SeriesAttemptDisposition {
                index: "1".into(),
                kind: SeriesNoVerdictKind::ContainerFailed,
                detail: None,
                attempt_outcome: "FAIL".into(),
                disposition: SeriesOutcome::NoResult,
                error_kind: None,
                status: Some(125),
                signal: None,
                timed_out: false,
                verification_report_sha256: Some("c".repeat(64)),
            }],
        });
        row.validate_for_write().unwrap();
        let encoded = serde_json::to_value(&row).unwrap();
        let value = project(
            &canonical_bytes(&manifest()),
            "",
            &jsonl(std::slice::from_ref(&encoded)),
            None,
            |_| Ok(Some(true)),
        )
        .unwrap();
        assert_eq!(value["series_rows"].as_array().unwrap().len(), 1);
        let projected = &value["series_rows"][0]["series"];
        assert_eq!(projected["outcome"], "errored");
        assert_eq!(projected["result"], "crash-error");
        assert_eq!(projected["failure_class"], "product_failure");
        assert_eq!(
            serde_json::from_value::<SeriesNoVerdictEvidence>(
                projected["no_verdict_evidence"].clone()
            )
            .unwrap(),
            row.series.no_verdict_evidence.unwrap()
        );
        let attempt = &projected["no_verdict_evidence"]["attempts"][0];
        assert_eq!(attempt["kind"], "container_failed");
        assert_eq!(attempt["attempt_outcome"], "FAIL");
        assert_eq!(attempt["disposition"], "no_result");
        assert_eq!(attempt["status"], 125);
        assert_eq!(
            value["product_results"]["event-container-failed"],
            "product_failure"
        );
    }

    #[test]
    fn typed_no_comparison_verdict_keeps_reason_and_omits_comparison_fields() {
        let commit = "e".repeat(40);
        let mut row = history_row(8, "record-eight", "run-eight", &commit);
        row["cell_results"]["cells"][0]["cell_verdict"] = serde_json::json!({
            "state": "performs-no-comparison-by-design",
            "comparison_tier": "execution-only-self-consistent",
            "reason": "fixture does not compare",
        });
        let parsed = parse_jsonl::<HistoryRow>(&jsonl(&[row]), "HistoryRow").unwrap();
        let projected = project_history(&parsed, None).unwrap();
        let verdict = &projected.history[0]["cell_results"]["recorded"]["cells"][0]["cell_verdict"];
        assert_eq!(verdict["reason"], "fixture does not compare");
        assert!(verdict.get("bitwise_parity").is_none());
        assert!(verdict.get("compared_log_messages").is_none());
        assert!(projected.exclusion_counts.is_empty());
    }

    #[test]
    fn writer_derived_historical_selectors_preserve_failed_record_and_authority() {
        // Synthetic authenticated transport operands, not a product execution.
        // The same real derived-file reader serves both selectors and normal history.
        let commit = "f".repeat(40);
        let mut row = history_row(8, "record-five", "run-five", &commit);
        row["schema_version"] = 5.into();
        row["commit_anchored"] = true.into();
        row["result"] = "fail".into();
        row["exit_code"] = 1.into();
        row["validation_complete"] = false.into();
        row["cell_results"] = Value::Null;
        let cell = cell_identity();
        let census = serde_json::json!({
            "schema": 1, "run_id": "run-five", "hermit_sha": commit,
            "files": [{"path": "portable/results.jsonl", "bytes": 10, "sha256": "a".repeat(64),
                "rows": [{"line": 1, "cell": cell, "attempt": 1},
                         {"line": 2, "cell": cell, "attempt": 2}]}],
        });
        row["raw_result_input_census_v1"] = census.clone();
        let binding_fields = [
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
        ];
        let binding = binding_fields
            .into_iter()
            .map(|key| (key.to_owned(), row[key].clone()))
            .collect::<Map<_, _>>();
        let attempt = |number: u64, matched: bool| {
            serde_json::json!({
                "input_path": "portable/results.jsonl", "input_line": number, "cell": cell,
                "attempt": number, "source_row_sha256": "b".repeat(64),
                "outcome": if matched { "PASS" } else { "FAIL" },
                "result": if matched { "pass" } else { "determinism-failure" },
                "failure_class": if matched { Value::Null } else { "product_failure".into() },
                "ordinary_verdict": {"state": if matched { "compared-and-matched" } else { "compared-and-diverged" },
                    "comparison_tier": "canonical-bitwise", "comparison": comparison(),
                    "bitwise_parity": matched, "compared_log_messages": {"left": 3, "right": 3}},
                "backend_parity": null,
            })
        };
        let operand = serde_json::json!({
            "schema": 1, "authority": "writer_verified_raw_census_v1", "run_id": "run-five",
            "record_id": "record-five", "hermit_sha": commit,
            "raw_census_sha256": sha256(&canonical_bytes(&census)),
            "history_binding_sha256": sha256(&canonical_bytes(&Value::Object(binding))),
            "attempts": [attempt(1, false), attempt(2, true)],
        });
        let root = std::env::temp_dir().join(format!(
            "ci-hub-derived-selectors-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let path = root.join(format!(
            "ignored/validate/artifacts/run-five/website-derived-census-{}-{}.json",
            operand["raw_census_sha256"].as_str().unwrap(),
            operand["history_binding_sha256"].as_str().unwrap()
        ));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let write = |value: &Value| {
            let mut bytes = canonical_bytes(value);
            bytes.push(b'\n');
            std::fs::write(&path, bytes).unwrap();
        };
        let load = |row: &HistoryRow, raw: &Value| {
            crate::compatibility_census::load_at(row, raw, Some(&root))
        };
        let input = jsonl(&[row.clone()]);
        let parsed = parse_jsonl::<HistoryRow>(&input, "HistoryRow").unwrap();
        write(&operand);
        let normal = project_history_with(&parsed, None, load).unwrap();
        let historical =
            project_history_with(&parsed, Some(("record-five", "run-five")), load).unwrap();
        let selected =
            crate::validation_row_report_with(&input, "record-five", "run-five", load).unwrap();
        assert_eq!(normal.history, historical.history);
        assert_eq!(normal.current, historical.current);
        for source in [historical.historical.as_ref().unwrap(), &selected["source"]] {
            assert!(source["selected_by_full_cell_count"].is_null());
            assert_eq!(source["recorded_cell_count"], 1);
            assert_eq!(source["population_scope"], "recorded_only");
            assert_eq!(
                source["comparison_authority"],
                "writer_verified_raw_census_v1"
            );
        }
        for preserved in [&normal.history[0], &selected["row"]] {
            assert_eq!(preserved["schema_version"], 5);
            assert_eq!(preserved["result"], "fail");
            assert_eq!(preserved["exit_code"], 1);
            assert_eq!(preserved["validation_complete"], false);
            assert!(preserved["cell_results"].is_null());
        }
        let evidence = &normal.history[0]["derived_cell_results"];
        assert_eq!(
            evidence["recorded"]["cells"][0]["ordinary_comparison"],
            "diverged"
        );
        assert_eq!(evidence["attempts"].as_array().unwrap().len(), 2);
        for field in ["record_id", "run_id"] {
            let mut bad = operand.clone();
            bad[field] = "foreign".into();
            write(&bad);
            assert!(project_history_with(&parsed, None, load)
                .unwrap_err()
                .contains("differs from its original terminal/census"));
            assert!(
                project_history_with(&parsed, Some(("record-five", "run-five")), load)
                    .unwrap_err()
                    .contains("differs from its original terminal/census")
            );
            assert!(
                crate::validation_row_report_with(&input, "record-five", "run-five", load)
                    .unwrap_err()
                    .contains("differs from its original terminal/census")
            );
        }
        write(&operand);
        assert!(
            project_history_with(&parsed, Some(("wrong-record", "run-five")), load)
                .unwrap_err()
                .contains("matched 0 terminal rows")
        );
        assert!(
            crate::validation_row_report_with(&input, "record-five", "wrong-run", load)
                .unwrap_err()
                .contains("matched 0 terminal rows")
        );
        std::fs::remove_file(&path).unwrap();
        assert!(project_history_with(&parsed, None, load)
            .unwrap()
            .history
            .is_empty());
        assert!(
            project_history_with(&parsed, Some(("record-five", "run-five")), load)
                .unwrap_err()
                .contains("does not satisfy")
        );
        assert!(
            crate::validation_row_report_with(&input, "record-five", "run-five", load)
                .unwrap_err()
                .contains("no supported typed cell_results")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn historical_source_requires_an_admitted_terminal_row() {
        let commit = "f".repeat(40);
        let row = history_row(8, "record-eight", "run-eight", &commit);
        let complete_series = series("run-eight", &commit, "event-eight");
        let value = project(
            &canonical_bytes(&manifest()),
            &jsonl(&[row]),
            &jsonl(&[complete_series]),
            Some(("record-eight", "run-eight")),
            |_| Ok(Some(true)),
        )
        .unwrap();
        assert_eq!(value["historical"]["run_id"], "run-eight");
        assert_eq!(value["historical"]["measured_hermit_commit"], commit);
        assert_eq!(value["historical"]["measurement_host"], "fixture-host");
        assert_eq!(
            value["historical"]["measured_reverie_commit"],
            "d".repeat(40)
        );
        assert_eq!(value["historical"]["kernel_version"], "6.1-fixture");
        assert_eq!(
            value["historical"]["host_capabilities"]["kvm"]["present"],
            false
        );
    }

    #[test]
    fn historical_source_refuses_missing_or_inconsistent_measurement_provenance() {
        let commit = "f".repeat(40);
        let complete = history_row(8, "record-eight", "run-eight", &commit);
        let complete_series = series("run-eight", &commit, "event-eight");

        let mut missing_reverie = complete.clone();
        missing_reverie
            .as_object_mut()
            .unwrap()
            .remove("reverie_base_sha");
        let error = project(
            &canonical_bytes(&manifest()),
            &jsonl(&[missing_reverie]),
            &jsonl(std::slice::from_ref(&complete_series)),
            Some(("record-eight", "run-eight")),
            |_| Ok(Some(true)),
        )
        .unwrap_err();
        assert!(error.contains("omits measured Reverie commit"), "{error}");

        let mut inconsistent = complete_series;
        inconsistent["series"]["kernel_version"] = Value::String("other-kernel".into());
        let second = series("run-eight", &commit, "event-nine");
        let error = project(
            &canonical_bytes(&manifest()),
            &jsonl(&[complete]),
            &jsonl(&[inconsistent, second]),
            Some(("record-eight", "run-eight")),
            |_| Ok(Some(true)),
        )
        .unwrap_err();
        assert!(
            error.contains("disagree about their measurement environment"),
            "{error}"
        );
    }

    #[test]
    fn projection_is_stable_and_digest_covers_the_unsigned_envelope() {
        let commit = "a".repeat(40);
        let history = jsonl(&[history_row(7, "record-one", "run-one", &commit)]);
        let series = jsonl(&[series("run-one", &commit, "event-one")]);
        let first = project(
            &canonical_bytes(&manifest()),
            &history,
            &series,
            None,
            |_| Ok(Some(true)),
        )
        .unwrap();
        let second = project(
            &canonical_bytes(&manifest()),
            &history,
            &series,
            None,
            |_| Ok(Some(true)),
        )
        .unwrap();
        assert_eq!(first, second);
        let mut unsigned = first.clone();
        let digest = unsigned
            .as_object_mut()
            .unwrap()
            .remove("projection_sha256")
            .unwrap();
        assert_eq!(digest, sha256(&canonical_bytes(&unsigned)));
    }
}
