//! Read retained validation artifacts through the producer-owned verifier.
//!
//! This module does not load the ledger, run qualification, or invoke Python.
//! Native callers use it in process; the JSONL command serves one batch from a
//! standalone Python reader without re-entering receipt qualification.

use crate::records::{
    CellBindingContract, CellIdentity, ComparisonObservationVerdictV10, HistoryRow, RequiredNullable,
    VerifiedValidationEvidenceV10,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};

// Reader capacity limits, not a claim about all valid producer artifacts.
// Exhaustion is an operational reader fault and never a product verdict.
const ARTIFACT_BYTES_PER_ROW: u64 = 256 * 1024 * 1024;
const BATCH_LINE_BYTES: usize = 16 * 1024 * 1024;
const BATCH_TOTAL_BYTES: usize = 128 * 1024 * 1024;

struct ArtifactBudget {
    limit: u64,
    remaining: u64,
}

impl ArtifactBudget {
    fn new(limit: u64) -> Self {
        Self {
            limit,
            remaining: limit,
        }
    }

    fn exceeded(&self, relative: &str) -> EvidenceOutcome {
        reader_fault(format!(
            "retained artifact {relative} exceeds aggregate artifact limit {} bytes per row ({} bytes remaining)",
            self.limit, self.remaining
        ))
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum EvidenceOutcome {
    /// Artifacts were authenticated under the historical schema-10 shape.
    /// Keep their observations, but do not grant selected-attempt authority.
    LegacyUnbound {
        evidence: Box<VerifiedValidationEvidenceV10>,
        full_validation_refusal: Option<String>,
        backend_parity_matches: Vec<CellIdentity>,
    },
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
    // The shared V9 artifact verifier currently requires every planned test
    // producer before returning this DTO, so full_test_results is true and
    // missing_test_producers is empty on that path. This preserves the typed
    // qualification guard; it is not a second independent coverage check.
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
    // An empty selected relation set reports no parity coverage. Ordinary test
    // completion still uses its own checks above.
    if !evidence.cell_results.selected_backend_parity.is_empty() && !evidence.full_backend_parity {
        return Some("backend_parity incomplete for full validation");
    }
    None
}

fn read_artifact_bytes(
    file: impl Read,
    observed_bytes: u64,
    declared_bytes: Option<u64>,
    relative: &str,
    budget: &mut ArtifactBudget,
) -> Result<Vec<u8>, EvidenceOutcome> {
    if observed_bytes > budget.remaining {
        return Err(budget.exceeded(relative));
    }
    if declared_bytes.is_some_and(|expected| expected != observed_bytes) {
        return Err(refused(format!(
            "artifact {relative} length differs from declared constructed_plan bytes"
        )));
    }
    // A file can grow after metadata was read. Never allocate/read its entire
    // current length on trust; one extra byte distinguishes exact capacity.
    let mut bytes = Vec::new();
    file.take(budget.remaining + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            reader_fault(format!("cannot read retained artifact {relative}: {error}"))
        })?;
    if bytes.len() as u64 > budget.remaining {
        return Err(budget.exceeded(relative));
    }
    if declared_bytes.is_some_and(|expected| expected != bytes.len() as u64) {
        return Err(refused(format!(
            "artifact {relative} length differs from declared constructed_plan bytes"
        )));
    }
    budget.remaining -= bytes.len() as u64;
    Ok(bytes)
}

fn retained_bytes(
    root: &Path,
    relative: &str,
    declared_bytes: Option<u64>,
    budget: &mut ArtifactBudget,
) -> Result<Vec<u8>, EvidenceOutcome> {
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
    // The caller-owned state root and fixed store prefix may use ordinary
    // directory symlinks. Bind that directory before traversing any row-owned
    // suffix; every suffix component rejects symlinks through openat below.
    let retained = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NONBLOCK)
        .open(root.join("ignored/validate/artifacts"))
        .map_err(|error| reader_fault(format!("cannot open retained artifact store: {error}")))?;
    let suffix = path
        .strip_prefix("ignored/validate/artifacts")
        .expect("retained prefix checked above");
    let file = open_relative_path(&retained, suffix, false).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidInput {
            refused(format!("retained artifact path is invalid: {error}"))
        } else if matches!(
            error.raw_os_error(),
            Some(libc::ELOOP) | Some(libc::ENOTDIR)
        ) {
            refused("retained artifact path contains a symlink or non-directory component")
        } else {
            reader_fault(format!("cannot open retained artifact {relative}: {error}"))
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| reader_fault(format!("cannot stat artifact {relative}: {error}")))?;
    if !metadata.is_file() {
        return Err(refused("retained artifact is not a regular file"));
    }
    read_artifact_bytes(file, metadata.len(), declared_bytes, relative, budget)
}

/// Every directory is opened relative to the previously opened descriptor.
/// A rename after opening cannot redirect the next component to another tree.
fn open_relative_path(root: &fs::File, path: &Path, final_directory: bool) -> io::Result<fs::File> {
    let mut current = root.try_clone()?;
    let mut components = path.components().peekable();
    if components.peek().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty artifact path",
        ));
    }
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact path is not relative normal components",
            ));
        };
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "artifact path contains NUL")
        })?;
        let directory = components.peek().is_some() || final_directory;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 };
        // SAFETY: current owns its directory fd; name is a NUL-terminated
        // normal component. A successful fd is immediately owned by File.
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        current = unsafe { fs::File::from_raw_fd(fd) };
    }
    Ok(current)
}

/// A missing local artifact is a reader fault. A readable artifact that fails
/// the producer's hash, identity, plan, or report contract is a receipt refusal.
pub fn verify(row: &HistoryRow, root: &Path) -> EvidenceOutcome {
    verify_with_capacity(row, root, ARTIFACT_BYTES_PER_ROW)
}

fn verify_with_capacity(row: &HistoryRow, root: &Path, artifact_bytes: u64) -> EvidenceOutcome {
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
    let mut budget = ArtifactBudget::new(artifact_bytes);
    let plan_bytes = match retained_bytes(root, &plan.path, Some(plan.bytes), &mut budget) {
        Ok(value) => value,
        Err(outcome) => return outcome,
    };
    let cell_bytes = match retained_bytes(root, &cells.artifact.path, None, &mut budget) {
        Ok(value) => value,
        Err(outcome) => return outcome,
    };
    let test_bytes = match retained_bytes(root, test_path, None, &mut budget) {
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
            match evidence.cell_results.binding_contract {
                CellBindingContract::LegacyUnbound => EvidenceOutcome::LegacyUnbound {
                    evidence: Box::new(evidence),
                    full_validation_refusal,
                    backend_parity_matches,
                },
                CellBindingContract::SelectedAttemptV1 => EvidenceOutcome::Verified {
                    evidence: Box::new(evidence),
                    full_validation_refusal,
                    backend_parity_matches,
                },
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
    run_batch_io(
        root,
        stdin.lock(),
        stdout.lock(),
        BATCH_LINE_BYTES,
        BATCH_TOTAL_BYTES,
    )
}

/// Buffer a bounded batch so a later capacity fault cannot publish a prefix
/// of successful responses. The caller reports transport failure for all rows.
fn run_batch_io(
    root: &Path,
    mut input: impl BufRead,
    mut output: impl Write,
    line_limit: usize,
    total_limit: usize,
) -> Result<(), String> {
    let mut input_bytes = 0;
    let mut responses = Vec::new();
    let mut index = 0;
    loop {
        let line = read_request_line(&mut input, line_limit, total_limit, &mut input_bytes)?;
        if line.is_empty() {
            break;
        }
        index += 1;
        let request: Request = serde_json::from_slice(&line)
            .map_err(|error| format!("invalid request {index}: {error}"))?;
        let row_sha256 = format!("{:x}", Sha256::digest(request.row_json.as_bytes()));
        let outcome = match serde_json::from_str::<HistoryRow>(&request.row_json) {
            Ok(row) => verify(&row, root),
            Err(error) => refused(format!("HistoryRow is malformed: {error}")),
        };
        let response = Response {
            row_sha256,
            outcome,
        };
        append_response(&mut responses, &response, line_limit, total_limit)?;
    }
    output
        .write_all(&responses)
        .map_err(|error| format!("cannot write responses: {error}"))?;
    output
        .flush()
        .map_err(|error| format!("cannot flush responses: {error}"))
}

fn read_request_line(
    input: &mut impl BufRead,
    line_limit: usize,
    total_limit: usize,
    total: &mut usize,
) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    // This limit also bounds an unterminated line. The extra byte detects
    // overflow without allowing read_until to allocate an unbounded buffer.
    let remaining = total_limit - *total;
    input
        .take(line_limit.min(remaining) as u64 + 1)
        .read_until(b'\n', &mut line)
        .map_err(|error| format!("cannot read request: {error}"))?;
    if line.len() > line_limit {
        return Err(format!("request line exceeds {line_limit} bytes"));
    }
    if line.len() > remaining {
        return Err(format!("request batch exceeds {total_limit} bytes"));
    }
    *total += line.len();
    Ok(line)
}

struct ResponseWriter<'a> {
    bytes: &'a mut Vec<u8>,
    line_start: usize,
    line_limit: usize,
    total_limit: usize,
}

impl Write for ResponseWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.line_limit - (self.bytes.len() - self.line_start) {
            return Err(io::Error::other(format!(
                "response line exceeds {} bytes",
                self.line_limit
            )));
        }
        if bytes.len() > self.total_limit - self.bytes.len() {
            return Err(io::Error::other(format!(
                "response batch exceeds {} bytes",
                self.total_limit
            )));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn append_response(
    output: &mut Vec<u8>,
    response: &impl Serialize,
    line_limit: usize,
    total_limit: usize,
) -> Result<(), String> {
    let line_start = output.len();
    let mut writer = ResponseWriter {
        bytes: output,
        line_start,
        line_limit,
        total_limit,
    };
    serde_json::to_writer(&mut writer, response)
        .map_err(|error| format!("cannot serialize response: {error}"))?;
    writer
        .write_all(b"\n")
        .map_err(|error| format!("cannot finish response: {error}"))
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

    pub(super) fn bytes(name: &str) -> (&'static str, &'static str, &'static str, &'static str) {
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
            "ordinary-only" => files!("ordinary-only"),
            "selected-attempt-v1/matched" => files!("selected-attempt-v1/matched"),
            "selected-attempt-v1/reference-diverged" => files!("selected-attempt-v1/reference-diverged"),
            "selected-attempt-v1/cross-diverged-then-matched" => files!("selected-attempt-v1/cross-diverged-then-matched"),
            "selected-attempt-v1/ordinary-only" => files!("selected-attempt-v1/ordinary-only"),
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
            for cell in value.row["cell_results"]["cells"].as_array_mut().unwrap() {
                if let Some(binding) = cell.get_mut("evidence_binding") {
                    binding["run_id"] = json!(run);
                    binding["tree"] = json!(commit);
                }
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
            }
            | EvidenceOutcome::LegacyUnbound {
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
            let outcome = verify(&row, &store.0);
            assert!(matches!(outcome, EvidenceOutcome::LegacyUnbound { .. }));
            let (evidence, refusal, matches) = verified(outcome);
            assert_eq!(evidence.cell_results.binding_contract, CellBindingContract::LegacyUnbound);
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
    fn schema10_mixed_contract_batch_keeps_each_typed_outcome() {
        let store = Store::new();
        let legacy = Fixture::new("reference-diverged", "batch-legacy", COMMIT, 1)
            .write(&store.0);
        let bound = Fixture::new("selected-attempt-v1/matched", "batch-bound", COMMIT, 2)
            .write(&store.0);
        let missing = Fixture::new("matched", "batch-missing", COMMIT, 3)
            .write(&store.0);
        fs::remove_file(store.0.join(missing.constructed_plan_artifact().unwrap().unwrap().path))
            .unwrap();
        let bound_json = serde_json::to_string(&bound).unwrap();
        assert!(bound_json.contains("\"binding_contract\":1"));
        let duplicate = bound_json.replacen(
            "\"binding_contract\":1",
            "\"binding_contract\":1,\"binding_contract\":1",
            1,
        );
        let rows = [
            serde_json::to_string(&legacy).unwrap(),
            bound_json,
            duplicate,
            serde_json::to_string(&missing).unwrap(),
        ];
        let mut input = Vec::new();
        for row in &rows {
            serde_json::to_writer(&mut input, &json!({"row_json": row})).unwrap();
            input.push(b'\n');
        }
        let mut output = Vec::new();
        run_batch_io(&store.0, io::Cursor::new(input), &mut output, 1 << 20, 4 << 20)
            .unwrap();
        let reports: Vec<serde_json::Value> = String::from_utf8(output).unwrap().lines()
            .map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(reports.len(), rows.len());
        for ((row, report), status) in rows.iter().zip(&reports)
            .zip(["legacy-unbound", "verified", "refused", "reader-fault"])
        {
            assert_eq!(report["row_sha256"], format!("{:x}", Sha256::digest(row.as_bytes())));
            assert_eq!(report["status"], status);
        }
        assert!(reports[0]["evidence"]["cell_results"].get("binding_contract").is_none());
        assert_eq!(reports[0]["evidence"]["observations"].as_array().unwrap().len(), 3);
        assert_eq!(reports[0]["full_validation_refusal"], "cell_results comparison-diverged");
        assert_eq!(reports[1]["evidence"]["cell_results"]["binding_contract"], 1);
        assert!(reports[1]["full_validation_refusal"].is_null());
        assert!(reports[2]["reason"].as_str().unwrap().contains("binding_contract"));
    }

    #[test]
    fn schema10_ordinary_only_coverage_does_not_claim_backend_parity() {
        let store = Store::new();
        let row = store.exact("ordinary-only");
        let (evidence, refusal, matches) = verified(verify(&row, &store.0));
        assert_eq!(evidence.cell_results.selected_count, 1);
        assert_eq!(evidence.cell_results.recorded_count, 1);
        assert!(evidence.cell_results.selected_backend_parity.is_empty());
        assert!(!evidence.full_backend_parity);
        assert!(evidence.full_test_results);
        assert!(evidence.missing_cells.is_empty());
        assert!(evidence.missing_backend_parity.is_empty());
        assert_eq!(evidence.observations.len(), 1);
        assert_eq!(
            evidence.observations[0].relation,
            ComparisonRelationV10::Ordinary
        );
        assert_eq!(
            evidence.observations[0].verdict,
            ComparisonObservationVerdictV10::Matched
        );
        assert!(matches.is_empty());
        assert!(refusal.is_none());
    }

    #[test]
    fn schema10_missing_artifacts_are_reader_faults_and_corruption_is_refused() {
        let store = Store::new();
        let row = store.exact("matched");
        let reference = row.constructed_plan_artifact().unwrap().unwrap();
        let path = store.0.join(&reference.path);
        let original = fs::read(&path).unwrap();
        assert_eq!(original.len() as u64, reference.bytes);
        assert_eq!(format!("{:x}", Sha256::digest(&original)), reference.sha256);
        fs::remove_file(&path).unwrap();
        assert!(matches!(
            verify(&row, &store.0),
            EvidenceOutcome::ReaderFault { .. }
        ));
        let mut corrupt = original.clone();
        corrupt.push(b' ');
        fs::write(&path, corrupt).unwrap();
        assert!(matches!(
            verify(&row, &store.0),
            EvidenceOutcome::Refused { reason }
            if reason == format!("artifact {} length differs from declared constructed_plan bytes", reference.path)
        ));
        // Preserve the declared length and recorded digest. The parent length
        // guard must permit this through to the shared artifact identity check.
        let mut same_length = original.clone();
        assert_eq!(same_length[0], b'{');
        same_length[0] = b'[';
        assert_eq!(same_length.len(), original.len());
        assert_ne!(
            format!("{:x}", Sha256::digest(&same_length)),
            reference.sha256
        );
        fs::write(&path, same_length).unwrap();
        assert!(matches!(
            verify(&row, &store.0),
            EvidenceOutcome::Refused { reason }
            if reason == "schema 10 constructed plan bytes differ from their recorded identity"
        ));
    }

    #[test]
    fn schema10_plan_and_artifact_substitutions_never_gain_authority() {
        use crate::records::{BackendParityRelation, ConstructedValidationPlanV10};

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Populations {
            cells: Vec<CellIdentity>,
            backend_parity: Vec<BackendParityRelation>,
        }

        // These are retained actual generated selections, with synthetic run
        // identities. They demonstrate plan admission, not guest comparisons.
        let cases: [(&str, &[u8], &[u8], usize, usize); 4] = [
            (
                "full-ordinary",
                include_bytes!("../validate/tests/fixtures/schema10/generated-plans/full-ordinary-plan.json"),
                include_bytes!("../validate/tests/fixtures/schema10/generated-plans/full-ordinary-populations.json"),
                756,
                0,
            ),
            (
                "full-parity",
                include_bytes!("../validate/tests/fixtures/schema10/generated-plans/full-parity-plan.json"),
                include_bytes!("../validate/tests/fixtures/schema10/generated-plans/full-parity-populations.json"),
                756,
                97,
            ),
            (
                "hosted-portable-ordinary",
                include_bytes!("../validate/tests/fixtures/schema10/generated-plans/hosted-portable-ordinary-plan.json"),
                include_bytes!("../validate/tests/fixtures/schema10/generated-plans/hosted-portable-ordinary-populations.json"),
                753,
                0,
            ),
            (
                "hosted-portable-parity",
                include_bytes!("../validate/tests/fixtures/schema10/generated-plans/hosted-portable-parity-plan.json"),
                include_bytes!("../validate/tests/fixtures/schema10/generated-plans/hosted-portable-parity-populations.json"),
                753,
                97,
            ),
        ];
        let mut ordinary = std::collections::BTreeMap::new();
        for (name, plan_bytes, expected_bytes, cells, relations) in cases {
            let plan: ConstructedValidationPlanV10 = serde_json::from_slice(plan_bytes).unwrap();
            let expected: Populations = serde_json::from_slice(expected_bytes).unwrap();
            assert_eq!(expected.cells.len(), cells, "{name}");
            assert_eq!(expected.backend_parity.len(), relations, "{name}");
            assert_eq!(plan.planned_cells().unwrap(), expected.cells, "{name}");
            assert_eq!(
                plan.planned_backend_parity_relations().unwrap(),
                expected.backend_parity,
                "{name}"
            );
            if let Some(label) = name.strip_suffix("-ordinary") {
                ordinary.insert(label, expected.cells);
            } else {
                let label = name.strip_suffix("-parity").unwrap();
                assert_eq!(ordinary.get(label).unwrap(), &expected.cells, "{name}");
            }
        }

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

    #[test]
    fn schema10_artifact_capacity_counts_all_three_files() {
        let store = Store::new();
        let row = store.exact("matched");
        let (_, plan, cells, tests) = fixtures::bytes("matched");
        let total = (plan.len() + cells.len() + tests.len()) as u64;
        let (_, refusal, _) = verified(verify_with_capacity(&row, &store.0, total));
        assert!(refusal.is_none());
        match verify_with_capacity(&row, &store.0, total - 1) {
            EvidenceOutcome::ReaderFault { reason } => {
                assert!(reason.contains(&format!("aggregate artifact limit {} bytes", total - 1)));
                assert!(reason.contains("test-results.jsonl"));
            }
            other => panic!("capacity is a reader fault, not partial success: {other:?}"),
        }
    }

    #[test]
    fn schema10_artifact_symlinks_and_nonregular_files_are_refused() {
        use std::os::unix::fs::symlink;
        for kind in ["escape", "directory", "fifo"] {
            let store = Store::new();
            let row = store.exact("matched");
            let reference = row.constructed_plan_artifact().unwrap().unwrap();
            let path = store.0.join(reference.path);
            fs::remove_file(&path).unwrap();
            match kind {
                "escape" => {
                    let outside = store.0.join("outside-plan.json");
                    fs::write(&outside, b"synthetic outside bytes").unwrap();
                    symlink(outside, &path).unwrap();
                }
                "directory" => fs::create_dir(&path).unwrap(),
                "fifo" => {
                    use std::os::unix::ffi::OsStrExt;
                    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                }
                _ => unreachable!(),
            }
            let expected = if kind == "escape" {
                "retained artifact path contains a symlink or non-directory component"
            } else {
                "retained artifact is not a regular file"
            };
            assert!(
                matches!(verify(&row, &store.0), EvidenceOutcome::Refused { reason } if reason == expected)
            );
        }
    }

    #[test]
    fn schema10_operator_owned_artifact_prefix_symlinks_are_readable() {
        use std::os::unix::fs::symlink;
        for prefix in ["ignored", "ignored/validate", "ignored/validate/artifacts"] {
            let store = Store::new();
            let external = Store::new();
            let row = store.exact("matched");
            let target = external.0.join("retained");
            fs::rename(store.0.join(prefix), &target).unwrap();
            symlink(&target, store.0.join(prefix)).unwrap();
            let (_, refusal, matches) = verified(verify(&row, &store.0));
            assert!(refusal.is_none(), "operator-owned prefix {prefix}");
            assert_eq!(matches.len(), 1);
        }
    }

    #[test]
    fn schema10_malformed_test_artifact_suffixes_are_refusals() {
        for (path, reason) in [
            (
                "ignored/validate/artifacts",
                "retained artifact path is invalid: empty artifact path",
            ),
            (
                "ignored/validate/artifacts/fixture-v10/bad\0test-results.jsonl",
                "retained artifact path is invalid: artifact path contains NUL",
            ),
        ] {
            let store = Store::new();
            let mut row = serde_json::to_value(store.exact("matched")).unwrap();
            // The plan/cell references have already passed their shared shape
            // checks. This real row field reaches the retained-file boundary.
            row["test_results"]["artifact"]["path"] = json!(path);
            let row = serde_json::from_value(row).unwrap();
            assert!(matches!(
                verify(&row, &store.0),
                EvidenceOutcome::Refused { reason: actual } if actual == reason
            ));
        }
    }

    #[test]
    fn schema10_intermediate_directory_substitution_cannot_redirect_artifact_reads() {
        use std::os::unix::fs::symlink;
        let store = Store::new();
        let row = store.exact("matched");
        let reference = row.constructed_plan_artifact().unwrap().unwrap();
        let plan = store.0.join(&reference.path);
        let original = fs::read(&plan).unwrap();
        let run = plan.parent().unwrap();
        let saved = run.with_file_name("saved-fixture-v10");
        let outside = store.0.join("outside-run");
        fs::create_dir(&outside).unwrap();
        fs::write(
            outside.join("constructed-plan.json"),
            b"outside bytes must never be read",
        )
        .unwrap();
        let retained = fs::File::open(run.parent().unwrap()).unwrap();
        // Bind the actual directory through the same production traversal,
        // then replace its old pathname before opening the leaf.
        let bound_run = open_relative_path(&retained, Path::new("fixture-v10"), true).unwrap();
        fs::rename(run, &saved).unwrap();
        symlink(&outside, run).unwrap();
        let error = open_relative_path(
            &retained,
            Path::new("fixture-v10/constructed-plan.json"),
            false,
        )
        .unwrap_err();
        assert!(matches!(
            error.raw_os_error(),
            Some(libc::ELOOP) | Some(libc::ENOTDIR)
        ));
        assert!(
            matches!(verify(&row, &store.0), EvidenceOutcome::Refused { reason }
            if reason == "retained artifact path contains a symlink or non-directory component")
        );
        let mut file =
            open_relative_path(&bound_run, Path::new("constructed-plan.json"), false).unwrap();
        let mut observed = Vec::new();
        file.read_to_end(&mut observed).unwrap();
        assert_eq!(observed, original);
        assert_ne!(observed, b"outside bytes must never be read");
    }

    #[test]
    fn schema10_artifact_store_uses_main_worktree_or_explicit_parent() {
        use crate::bounded_output::BoundedOutput;
        use std::process::Command;
        let store = Store::new();
        let main = store.0.join("main");
        let worktree = store.0.join("worktree");
        let override_root = store.0.join("explicit-parent");
        let output = Command::new("git")
            .args(["init", "--quiet"])
            .arg(&main)
            .bounded_output()
            .unwrap();
        assert!(output.status.success());
        let git = |args: &[&str], input: &[u8]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&main)
                .args([
                    "-c",
                    "user.name=Synthetic",
                    "-c",
                    "user.email=synthetic@example.invalid",
                ])
                .args(args)
                .bounded_output_with_input(input)
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let tree = git(&["hash-object", "-w", "-t", "tree", "--stdin"], b"");
        let commit = git(&["commit-tree", &tree], b"synthetic fixture\n");
        git(
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                worktree.to_str().unwrap(),
                &commit,
            ],
            b"",
        );
        let root = crate::operational_state_root_with(&worktree, None).unwrap();
        assert_eq!(root, main);
        let mut fixture = Fixture::new("matched", "main-store", COMMIT, 1);
        let row = fixture.write(&root);
        assert!(verified(verify(&row, &root)).1.is_none());
        assert!(matches!(
            verify(&row, &worktree),
            EvidenceOutcome::ReaderFault { .. }
        ));

        fs::create_dir(&override_root).unwrap();
        let explicit = crate::operational_state_root_with(
            &worktree,
            Some(override_root.as_os_str().to_os_string()),
        )
        .unwrap();
        assert_eq!(explicit, override_root);
        let mut fixture = Fixture::new("ordinary-only", "explicit-store", COMMIT, 2);
        let row = fixture.write(&explicit);
        assert!(verified(verify(&row, &explicit)).1.is_none());
        assert!(matches!(
            verify(&row, &main),
            EvidenceOutcome::ReaderFault { .. }
        ));
    }

    #[test]
    fn schema10_opened_artifact_growth_cannot_exceed_capacity() {
        let store = Store::new();
        let path = store.0.join("growing-artifact");
        fs::write(&path, b"1234").unwrap();
        let file = fs::File::open(&path).unwrap();
        let before = file.metadata().unwrap().len();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"56789")
            .unwrap();
        let mut budget = ArtifactBudget::new(8);
        assert!(matches!(
            read_artifact_bytes(file, before, None, "growing-artifact", &mut budget),
            Err(EvidenceOutcome::ReaderFault { reason }) if reason.contains("limit 8 bytes")
        ));
        assert_eq!(budget.remaining, 8);
        let file = fs::File::open(&path).unwrap();
        let mut exact = ArtifactBudget::new(9);
        assert_eq!(
            read_artifact_bytes(file, 9, Some(9), "exact", &mut exact).unwrap(),
            b"123456789"
        );
        assert_eq!(exact.remaining, 0);
    }

    #[test]
    fn schema10_artifact_metadata_refuses_before_reading() {
        struct MustNotRead;
        impl Read for MustNotRead {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("metadata should reject before reading or allocating artifact bytes")
            }
        }
        let mut budget = ArtifactBudget::new(8);
        assert!(matches!(
            read_artifact_bytes(MustNotRead, 9, None, "oversize", &mut budget),
            Err(EvidenceOutcome::ReaderFault { reason }) if reason.contains("limit 8 bytes")
        ));
        assert!(matches!(
            read_artifact_bytes(MustNotRead, 4, Some(5), "plan", &mut budget),
            Err(EvidenceOutcome::Refused { reason }) if reason.contains("declared constructed_plan bytes")
        ));
    }

    #[test]
    fn schema10_request_line_and_batch_capacity_are_exact() {
        let mut input = io::Cursor::new(b"abc\ndef\n");
        let mut total = 0;
        assert_eq!(
            read_request_line(&mut input, 4, 8, &mut total).unwrap(),
            b"abc\n"
        );
        assert_eq!(
            read_request_line(&mut input, 4, 8, &mut total).unwrap(),
            b"def\n"
        );
        assert!(read_request_line(&mut input, 4, 8, &mut total)
            .unwrap()
            .is_empty());
        assert_eq!(total, 8);
        let mut input = io::Cursor::new(b"abcd\n");
        assert_eq!(
            read_request_line(&mut input, 4, 8, &mut 0).unwrap_err(),
            "request line exceeds 4 bytes"
        );
        let mut input = io::Cursor::new(b"abc\ndef\n");
        let mut total = 0;
        read_request_line(&mut input, 4, 7, &mut total).unwrap();
        assert_eq!(
            read_request_line(&mut input, 4, 7, &mut total).unwrap_err(),
            "request batch exceeds 7 bytes"
        );
    }

    #[test]
    fn schema10_response_line_and_batch_capacity_are_exact() {
        let value = json!({"result": "synthetic"});
        let mut encoded = serde_json::to_vec(&value).unwrap();
        encoded.push(b'\n');
        let length = encoded.len();
        let mut output = Vec::new();
        append_response(&mut output, &value, length, 2 * length).unwrap();
        append_response(&mut output, &value, length, 2 * length).unwrap();
        assert_eq!(output, encoded.repeat(2));
        assert!(
            append_response(&mut Vec::new(), &value, length - 1, 2 * length)
                .unwrap_err()
                .contains(&format!("response line exceeds {} bytes", length - 1))
        );
        let mut output = encoded;
        assert!(append_response(&mut output, &value, length, 2 * length - 1)
            .unwrap_err()
            .contains(&format!("response batch exceeds {} bytes", 2 * length - 1)));
        assert!(output.len() < 2 * length);
    }

    #[test]
    fn schema10_batch_capacity_fault_publishes_no_partial_responses() {
        let store = Store::new();
        let request = b"{\"row_json\":\"{}\"}\n";
        let mut input = request.to_vec();
        input.extend(std::iter::repeat_n(b'x', 1025));
        let mut output = Vec::new();
        assert_eq!(
            run_batch_io(&store.0, io::Cursor::new(input), &mut output, 1024, 4096).unwrap_err(),
            "request line exceeds 1024 bytes"
        );
        assert!(output.is_empty());

        let input = request.repeat(2);
        let mut complete = Vec::new();
        run_batch_io(&store.0, io::Cursor::new(&input), &mut complete, 4096, 8192).unwrap();
        assert_eq!(complete.iter().filter(|&&byte| byte == b'\n').count(), 2);
        let length = complete.len();
        let mut exact = Vec::new();
        run_batch_io(&store.0, io::Cursor::new(&input), &mut exact, 4096, length).unwrap();
        assert_eq!(exact, complete);
        let mut refused = Vec::new();
        assert!(run_batch_io(
            &store.0,
            io::Cursor::new(&input),
            &mut refused,
            4096,
            length - 1
        )
        .unwrap_err()
        .contains(&format!("response batch exceeds {} bytes", length - 1)));
        assert!(refused.is_empty());
    }
}
