//! Read retained validation artifacts through the producer-owned verifier.
//!
//! This module does not load the ledger, run qualification, or invoke Python.
//! Native callers use it in process; the JSONL command serves one batch from a
//! standalone Python reader without re-entering receipt qualification.

use crate::records::{
    CellIdentity, ComparisonObservationVerdictV10, HistoryRow, RequiredNullable,
    VerifiedValidationEvidenceV10,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Component, Path};

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum EvidenceOutcome {
    Verified {
        evidence: Box<VerifiedValidationEvidenceV10>,
        full_validation_refusal: Option<String>,
        backend_parity_matches: Vec<CellIdentity>,
    },
    Refused {
        reason: String,
    },
    ReaderFault {
        reason: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    /// Exact input bytes are carried inside a string rather than reparsed by
    /// the transport before their digest is computed.
    row_json: String,
}

#[derive(Debug, Serialize)]
struct Response {
    row_sha256: String,
    #[serde(flatten)]
    outcome: EvidenceOutcome,
}

fn refused(reason: impl Into<String>) -> EvidenceOutcome {
    EvidenceOutcome::Refused {
        reason: reason.into(),
    }
}

fn reader_fault(reason: impl Into<String>) -> EvidenceOutcome {
    EvidenceOutcome::ReaderFault {
        reason: reason.into(),
    }
}

/// Qualification remains stronger than readable partial evidence. This checks
/// only the producer's verified coverage and observations, never the raw report
/// policy a second time.
pub fn full_validation_refusal(evidence: &VerifiedValidationEvidenceV10) -> Option<&'static str> {
    if evidence.cell_results.selected_count == 0 || !evidence.missing_cells.is_empty() {
        return Some("cell_results incomplete for full validation");
    }
    if !evidence.full_test_results {
        return Some("test_results incomplete for full validation");
    }
    if evidence.observations.iter().any(|observation| {
        matches!(
            observation.verdict,
            ComparisonObservationVerdictV10::Diverged
        )
    }) {
        return Some("cell_results comparison-diverged");
    }
    if evidence.observations.iter().any(|observation| {
        matches!(
            observation.verdict,
            ComparisonObservationVerdictV10::UnavailableWithReason { .. }
        )
    }) {
        return Some("cell_results comparison-unavailable");
    }
    if !evidence.full_backend_parity {
        return Some("backend_parity incomplete for full validation");
    }
    None
}

fn retained_bytes(root: &Path, relative: &str) -> Result<Vec<u8>, EvidenceOutcome> {
    let path = Path::new(relative);
    if path.is_absolute()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        || !path.starts_with("ignored/validate/artifacts")
    {
        return Err(refused(
            "artifact path is outside retained validation artifacts",
        ));
    }
    let retained = root
        .join("ignored/validate/artifacts")
        .canonicalize()
        .map_err(|error| reader_fault(format!("cannot open retained artifact store: {error}")))?;
    let actual = root.join(path).canonicalize().map_err(|error| {
        reader_fault(format!("cannot open retained artifact {relative}: {error}"))
    })?;
    if !actual.starts_with(&retained) {
        return Err(refused(
            "artifact resolves outside retained validation artifacts",
        ));
    }
    let metadata = fs::metadata(&actual)
        .map_err(|error| reader_fault(format!("cannot stat artifact {relative}: {error}")))?;
    if !metadata.is_file() {
        return Err(refused("retained artifact is not a regular file"));
    }
    fs::read(actual)
        .map_err(|error| reader_fault(format!("cannot read retained artifact {relative}: {error}")))
}

/// A missing local artifact is a reader fault. A readable artifact that fails
/// the producer's hash, identity, plan, or report contract is a receipt refusal.
pub fn verify(row: &HistoryRow, root: &Path) -> EvidenceOutcome {
    if row.schema_version != Some(10) {
        return refused("schema has no authenticated validation-plan artifact contract");
    }
    let plan = match row.constructed_plan_artifact() {
        Ok(Some(value)) => value,
        Ok(None) => return refused("constructed_plan is missing"),
        Err(error) => return refused(error),
    };
    let cells = match row.schema10_cell_results() {
        Ok(Some(value)) => value,
        Ok(None) => return refused("cell_results is missing"),
        Err(error) => return refused(error),
    };
    // The shared schema-10 verifier owns the complete test_results shape.
    // This lookup selects its file only; it grants no summary authority.
    let test_value = match serde_json::to_value(&row.test_results) {
        Ok(value) => value,
        Err(error) => {
            return reader_fault(format!("cannot inspect test_results reference: {error}"))
        }
    };
    let Some(test_path) = test_value
        .get("artifact")
        .and_then(|value| value.get("path"))
        .and_then(|value| value.as_str())
    else {
        return refused("test_results artifact path is missing");
    };
    let plan_bytes = match retained_bytes(root, &plan.path) {
        Ok(value) => value,
        Err(outcome) => return outcome,
    };
    let cell_bytes = match retained_bytes(root, &cells.artifact.path) {
        Ok(value) => value,
        Err(outcome) => return outcome,
    };
    let test_bytes = match retained_bytes(root, test_path) {
        Ok(value) => value,
        Err(outcome) => return outcome,
    };
    match row.verify_schema10_artifact_bytes(&plan_bytes, &cell_bytes, &test_bytes) {
        Ok(Some(evidence)) => {
            let full_validation_refusal = full_validation_refusal(&evidence).map(str::to_owned);
            let backend_parity_matches = evidence
                .cell_results
                .cells
                .iter()
                .filter_map(|cell| match &cell.backend_parity {
                    RequiredNullable::Value(parity) if parity.is_clean_match() => {
                        Some(cell.identity())
                    }
                    _ => None,
                })
                .collect();
            EvidenceOutcome::Verified {
                evidence: Box::new(evidence),
                full_validation_refusal,
                backend_parity_matches,
            }
        }
        Ok(None) => refused("schema has no authenticated validation-plan artifact contract"),
        Err(error) => refused(error),
    }
}

pub fn verify_current_store(row: &HistoryRow) -> EvidenceOutcome {
    let root = match crate::workspace_root().and_then(|root| crate::operational_state_root(&root)) {
        Ok(root) => root,
        Err(error) => {
            return reader_fault(format!("cannot locate validation artifact store: {error}"))
        }
    };
    verify(row, &root)
}

/// Exit zero means the batch transport completed, including any refused rows.
/// Only each response's typed outcome judges its row. A malformed request or
/// failed output stream terminates the transport without inventing a verdict.
pub fn run_batch(root: &Path) -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for (index, line) in stdin.lock().lines().enumerate() {
        let line = line.map_err(|error| format!("cannot read request {}: {error}", index + 1))?;
        let request: Request = serde_json::from_str(&line)
            .map_err(|error| format!("invalid request {}: {error}", index + 1))?;
        let row_sha256 = format!("{:x}", Sha256::digest(request.row_json.as_bytes()));
        let outcome = match serde_json::from_str::<HistoryRow>(&request.row_json) {
            Ok(row) => verify(&row, root),
            Err(error) => refused(format!("HistoryRow is malformed: {error}")),
        };
        serde_json::to_writer(
            &mut output,
            &Response {
                row_sha256,
                outcome,
            },
        )
        .map_err(|error| format!("cannot serialize response {}: {error}", index + 1))?;
        output
            .write_all(b"\n")
            .map_err(|error| format!("cannot write response {}: {error}", index + 1))?;
    }
    output
        .flush()
        .map_err(|error| format!("cannot flush responses: {error}"))
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use crate::records::TestResultArtifactRow;
    use serde_json::{json, Value};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub struct Store(pub PathBuf);

    impl Store {
        pub fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "ci-hub-schema10-fixtures-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&path).expect("create exclusive fixture store");
            Self(path)
        }

        pub fn exact(&self, name: &str) -> HistoryRow {
            let (row, plan, cells, tests) = bytes(name);
            let value: Value = serde_json::from_str(row).unwrap();
            for (reference, contents) in [
                (&value["constructed_plan"], plan),
                (&value["cell_results"]["artifact"], cells),
                (&value["test_results"]["artifact"], tests),
            ] {
                let path = self.0.join(reference["path"].as_str().unwrap());
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, contents).unwrap();
            }
            serde_json::from_value(value).unwrap()
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove only owned fixture store");
        }
    }

    fn bytes(name: &str) -> (&'static str, &'static str, &'static str, &'static str) {
        macro_rules! files {
            ($name:literal) => {
                (
                    include_str!(concat!(
                        "../validate/tests/fixtures/schema10/",
                        $name,
                        "-row.json"
                    )),
                    include_str!(concat!(
                        "../validate/tests/fixtures/schema10/",
                        $name,
                        "-plan.json"
                    )),
                    include_str!(concat!(
                        "../validate/tests/fixtures/schema10/",
                        $name,
                        "-cells.jsonl"
                    )),
                    include_str!(concat!(
                        "../validate/tests/fixtures/schema10/",
                        $name,
                        "-tests.jsonl"
                    )),
                )
            };
        }
        match name {
            "matched" => files!("matched"),
            "reference-diverged" => files!("reference-diverged"),
            "cross-diverged-then-matched" => files!("cross-diverged-then-matched"),
            _ => panic!("unknown producer fixture"),
        }
    }

    pub struct Fixture {
        pub row: Value,
        pub plan: Value,
        pub cells: Vec<Value>,
        tests: Vec<TestResultArtifactRow>,
    }

    impl Fixture {
        pub fn new(name: &str, run: &str, commit: &str, ordinal: u64) -> Self {
            let (row, plan, cells, tests) = bytes(name);
            let mut value = Self {
                row: serde_json::from_str(row).unwrap(),
                plan: serde_json::from_str(plan).unwrap(),
                cells: cells
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect(),
                tests: tests
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect(),
            };
            value.row["run_id"] = json!(run);
            value.row["commit"] = json!(commit);
            value.row["tree"] = json!(commit);
            value.row["toolchain"] = json!("synthetic-rustc");
            value.row["finished_at"] = json!(format!("2026-09-16T00:{ordinal:02}:00Z"));
            for key in ["cell_results", "test_results"] {
                value.row[key]["run_id"] = json!(run);
                value.row[key]["hermit_sha"] = json!(commit);
            }
            value.plan["run_id"] = json!(run);
            value.plan["hermit_sha"] = json!(commit);
            for cell in &mut value.cells {
                cell["run_id"] = json!(run);
                cell["hermit_sha"] = json!(commit);
            }
            for test in &mut value.tests {
                test.run_id = run.into();
                test.hermit_sha = commit.into();
            }
            value
        }

        pub fn write(&mut self, root: &Path) -> HistoryRow {
            let run = self.row["run_id"].as_str().unwrap().to_owned();
            let mut cell_bytes = Vec::new();
            for cell in &self.cells {
                serde_json::to_writer(&mut cell_bytes, cell).unwrap();
                cell_bytes.push(b'\n');
            }
            let mut test_bytes = Vec::new();
            for test in &self.tests {
                serde_json::to_writer(&mut test_bytes, test).unwrap();
                test_bytes.push(b'\n');
            }
            let plan_bytes = serde_json::to_vec(&self.plan).unwrap();
            publish(
                root,
                &run,
                &mut self.row["constructed_plan"],
                "constructed-plan.json",
                &plan_bytes,
                None,
            );
            publish(
                root,
                &run,
                &mut self.row["cell_results"]["artifact"],
                "cell-results.jsonl",
                &cell_bytes,
                Some(self.cells.len()),
            );
            publish(
                root,
                &run,
                &mut self.row["test_results"]["artifact"],
                "test-results.jsonl",
                &test_bytes,
                Some(self.tests.len()),
            );
            serde_json::from_value(self.row.clone()).unwrap()
        }
    }

    fn publish(
        root: &Path,
        run: &str,
        reference: &mut Value,
        name: &str,
        bytes: &[u8],
        count: Option<usize>,
    ) {
        let relative = format!("ignored/validate/artifacts/{run}/{name}");
        let path = root.join(&relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        reference["path"] = json!(relative);
        reference["sha256"] = json!(format!("{:x}", Sha256::digest(bytes)));
        if let Some(count) = count {
            reference["row_count"] = json!(count);
        } else {
            reference["bytes"] = json!(bytes.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::ComparisonRelationV10;
    use fixtures::{Fixture, Store};
    use serde_json::json;

    const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn verified(
        outcome: EvidenceOutcome,
    ) -> (
        Box<VerifiedValidationEvidenceV10>,
        Option<String>,
        Vec<CellIdentity>,
    ) {
        match outcome {
            EvidenceOutcome::Verified {
                evidence,
                full_validation_refusal,
                backend_parity_matches,
            } => (evidence, full_validation_refusal, backend_parity_matches),
            other => panic!("expected verified synthetic artifact: {other:?}"),
        }
    }

    #[test]
    fn schema10_exact_producer_artifacts_preserve_relation_results() {
        for (name, observations, full_match) in [
            ("matched", 3, true),
            ("reference-diverged", 3, false),
            ("cross-diverged-then-matched", 6, false),
        ] {
            let store = Store::new();
            let row = store.exact(name);
            let (evidence, refusal, matches) = verified(verify(&row, &store.0));
            assert_eq!(evidence.cell_results.selected_count, 1);
            assert_eq!(evidence.cell_results.selected_backend_parity.len(), 1);
            assert_eq!(evidence.observations.len(), observations);
            assert_eq!(evidence.full_backend_parity, full_match);
            assert_eq!(refusal.is_none(), full_match);
            assert_eq!(matches.len(), usize::from(full_match));
            if name == "reference-diverged" {
                assert_eq!(
                    evidence.observations[0].verdict,
                    ComparisonObservationVerdictV10::Matched
                );
                assert_eq!(evidence.observations[1].identity.backend, "ptrace");
                assert!(matches!(
                    evidence.observations[1].relation,
                    ComparisonRelationV10::Reference { .. }
                ));
                assert_eq!(
                    evidence.observations[1].verdict,
                    ComparisonObservationVerdictV10::Diverged
                );
                assert!(matches!(
                    evidence.observations[2].verdict,
                    ComparisonObservationVerdictV10::UnavailableWithReason { .. }
                ));
            }
            if name == "cross-diverged-then-matched" {
                assert_eq!(
                    evidence.observations[2].verdict,
                    ComparisonObservationVerdictV10::Diverged
                );
                assert_eq!(
                    evidence.observations[5].verdict,
                    ComparisonObservationVerdictV10::Matched
                );
                assert_eq!(refusal.as_deref(), Some("cell_results comparison-diverged"));
            }
        }
    }

    #[test]
    fn schema10_missing_artifacts_are_reader_faults_and_corruption_is_refused() {
        let store = Store::new();
        let row = store.exact("matched");
        let reference = row.constructed_plan_artifact().unwrap().unwrap();
        let path = store.0.join(reference.path);
        let original = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(matches!(
            verify(&row, &store.0),
            EvidenceOutcome::ReaderFault { .. }
        ));
        let mut corrupt = original;
        corrupt.push(b' ');
        fs::write(&path, corrupt).unwrap();
        assert!(matches!(
            verify(&row, &store.0),
            EvidenceOutcome::Refused { .. }
        ));
    }

    #[test]
    fn schema10_plan_and_artifact_substitutions_never_gain_authority() {
        for mutation in [
            "compatibility",
            "manifest",
            "cross-summary",
            "report-bytes",
            "attempt-field",
            "attempt-null",
            "plan-run",
        ] {
            let store = Store::new();
            let mut fixture = Fixture::new("matched", mutation, COMMIT, 1);
            match mutation {
                "compatibility" => fixture.plan["compatibility_selected"] = json!(true),
                "manifest" => {
                    fixture.plan["expected_e2e_plan_json"] = json!("{\"schema\":1,\"cells\":[]}")
                }
                "cross-summary" => {
                    fixture.row["cell_results"]["cells"][0]["backend_parity"]["attempts"][0]
                        ["cross"]["state"] = json!("diverged")
                }
                "report-bytes" => {
                    fixture.cells[0]["backend_parity"]["attempts"][0]["candidate_attempt"]
                        ["verification_report"] = json!("changed report bytes")
                }
                "attempt-field" => {
                    fixture.cells[0]["backend_parity"]["attempts"][0]["candidate_attempt"]
                        ["unknown"] = json!(true)
                }
                "attempt-null" => {
                    fixture.cells[0]["backend_parity"]["attempts"][0]["candidate_attempt"]
                        .as_object_mut()
                        .unwrap()
                        .remove("signal");
                }
                "plan-run" => fixture.plan["run_id"] = json!("another-run"),
                _ => unreachable!(),
            }
            let row = fixture.write(&store.0);
            assert!(
                matches!(verify(&row, &store.0), EvidenceOutcome::Refused { .. }),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn schema10_missing_recorded_population_is_readable_but_not_full_green() {
        let store = Store::new();
        let mut fixture = Fixture::new("matched", "partial", COMMIT, 1);
        fixture.cells.clear();
        fixture.row["cell_results"]["cells"] = json!([]);
        fixture.row["cell_results"]["recorded_count"] = json!(0);
        let row = fixture.write(&store.0);
        let (evidence, refusal, matches) = verified(verify(&row, &store.0));
        assert_eq!(evidence.cell_results.selected_count, 1);
        assert_eq!(evidence.missing_cells.len(), 1);
        assert_eq!(evidence.missing_backend_parity.len(), 1);
        assert!(evidence.observations.is_empty());
        assert!(matches.is_empty());
        assert_eq!(
            refusal.as_deref(),
            Some("cell_results incomplete for full validation")
        );
    }

    #[test]
    fn schema10_artifact_paths_cannot_escape_the_retained_store() {
        let store = Store::new();
        let mut row = serde_json::to_value(store.exact("matched")).unwrap();
        row["constructed_plan"]["path"] = json!("ignored/validate/artifacts/../outside.json");
        let row = serde_json::from_value(row).unwrap();
        assert!(matches!(
            verify(&row, &store.0),
            EvidenceOutcome::Refused { .. }
        ));
    }
}
