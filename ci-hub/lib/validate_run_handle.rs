//! Rust consumers of the producer-owned validation run-handle contract.
//!
//! Current handles retain wire schema 1 for compatibility with already-deployed
//! readers and add the required `kind` discriminator. Their complete contract is
//! defined once by `ci-hub/validate/run_registry.py`, which is also the Python
//! writer. Rust asks that authority for a small typed projection instead of
//! independently assigning meanings to `kind`, `state`, and `process_identity`.
//! Schema-1 records without `kind` remain on their historical compatibility paths
//! so a process started before deployment can still finish.

use crate::bounded_output::BoundedOutput;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn is_current(value: &serde_json::Value) -> bool {
    value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        == Some(1)
        && value.get("producer").and_then(serde_json::Value::as_str)
            == Some("ci-hub/validate/run_registry.py")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_ticks: u64,
    pub boot_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AdmissionState {
    Admitted,
    Refused,
}

impl AdmissionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Refused => "refused",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum AdmissionRefusalReason {
    StaleBase,
    ValidationLockRefusal,
}

impl AdmissionRefusalReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::StaleBase => "stale-base",
            Self::ValidationLockRefusal => "validation-lock-refusal",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct AdmissionResult {
    pub state: AdmissionState,
    pub recorded_at: String,
    pub run_number: Option<u64>,
    pub reason: Option<AdmissionRefusalReason>,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TerminationRequest {
    pub requested_at: String,
    pub agent: String,
    pub pid: u32,
    pub reason: String,
    pub confirmed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Projection {
    pub state: String,
    pub unit: Option<String>,
    pub target: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub agent: Option<String>,
    pub started_at: Option<String>,
    pub host: Option<String>,
    pub process_identity: Option<ProcessIdentity>,
    pub admission_result: Option<AdmissionResult>,
    pub result: Option<String>,
    pub detail: Option<String>,
    pub exit_code: Option<i32>,
    pub finished_at: Option<String>,
    #[serde(default)]
    pub termination_requests: Vec<TerminationRequest>,
    pub lock_admissible: bool,
    pub counts_as_validation: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct InspectedRecord {
    pub path: PathBuf,
    pub record: Projection,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnreadableRecord {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct InspectionReport {
    pub records: Vec<InspectedRecord>,
    pub unreadable: Vec<UnreadableRecord>,
}

fn authority(root: &Path) -> PathBuf {
    root.join("ci-hub/validate/run_registry.py")
}

/// Materialize the real Python run-handle authority under a temporary tool
/// root, for tests that call it through [`authority`].
///
/// Production runs `python3 <tool root>/ci-hub/validate/run_registry.py` from a
/// complete checkout, so every sibling import resolves. A fixture carries only
/// the files written here, so this list must be the authority's complete
/// top-level import closure: `run_registry` imports `service_result` and
/// `reverie_node`; both import `final_validate_status`; `reverie_node` adds
/// `ci-hub/lib` to `sys.path` for `bounded_capture`. A module missing here makes
/// every authority call fail with `ModuleNotFoundError`, which is how
/// `import reverie_node` (01991d70e603) broke twelve fixtures that each kept
/// their own three-file copy. Keep this the one list the fixtures share.
#[cfg(test)]
pub(crate) fn write_authority_fixture(tool_root: &Path) {
    let validate = tool_root.join("ci-hub/validate");
    let lib = tool_root.join("ci-hub/lib");
    std::fs::create_dir_all(&validate).unwrap();
    std::fs::create_dir_all(&lib).unwrap();
    for (path, contents) in [
        (
            validate.join("run_registry.py"),
            include_str!("../validate/run_registry.py"),
        ),
        (
            validate.join("service_result.py"),
            include_str!("../validate/service_result.py"),
        ),
        (
            validate.join("final_validate_status.py"),
            include_str!("../validate/final_validate_status.py"),
        ),
        (
            validate.join("reverie_node.py"),
            include_str!("../validate/reverie_node.py"),
        ),
        (
            lib.join("bounded_capture.py"),
            include_str!("bounded_capture.py"),
        ),
    ] {
        std::fs::write(path, contents).unwrap();
    }
}

fn run_authority<T: DeserializeOwned>(root: &Path, arguments: &[String]) -> Result<T, String> {
    let script = authority(root);
    let output = Command::new("python3")
        .arg(&script)
        .args(arguments)
        .bounded_output()
        .map_err(|error| {
            format!(
                "cannot execute validation run-handle authority {}: {error}",
                script.display()
            )
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "validation run-handle authority refused rc={:?}: {}",
            output.status.code(),
            if detail.is_empty() {
                "no diagnostic"
            } else {
                &detail
            }
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!("validation run-handle authority emitted invalid projection: {error}")
    })
}

pub fn inspect_current_records(
    root: &Path,
    records: &[PathBuf],
) -> Result<InspectionReport, String> {
    let mut arguments = vec!["inspect-current-records".into()];
    for record in records {
        arguments.push("--path".into());
        arguments.push(record.display().to_string());
    }
    run_authority(root, &arguments)
}

pub fn verify_attribution(
    root: &Path,
    record: &Path,
    agent: &str,
    target: &str,
    kind: &str,
) -> Result<Projection, String> {
    run_authority(
        root,
        &[
            "verify-attribution".into(),
            "--path".into(),
            record.display().to_string(),
            "--agent".into(),
            agent.into(),
            "--target".into(),
            target.into(),
            "--kind".into(),
            kind.into(),
        ],
    )
}

pub fn verify_holder(
    root: &Path,
    record: &Path,
    agent: &str,
    target: &str,
    kind: &str,
    unit: &str,
    identity: &ProcessIdentity,
) -> Result<Projection, String> {
    run_authority(
        root,
        &[
            "verify-holder".into(),
            "--path".into(),
            record.display().to_string(),
            "--agent".into(),
            agent.into(),
            "--target".into(),
            target.into(),
            "--kind".into(),
            kind.into(),
            "--unit".into(),
            unit.into(),
            "--pid".into(),
            identity.pid.to_string(),
            "--start-ticks".into(),
            identity.start_ticks.to_string(),
            "--boot-id".into(),
            identity.boot_id.clone(),
        ],
    )
}

pub fn bind_process_identity(
    root: &Path,
    record: &Path,
    identity: &ProcessIdentity,
) -> Result<Projection, String> {
    run_authority(
        root,
        &[
            "bind-process-identity".into(),
            "--path".into(),
            record.display().to_string(),
            "--pid".into(),
            identity.pid.to_string(),
            "--start-ticks".into(),
            identity.start_ticks.to_string(),
            "--boot-id".into(),
            identity.boot_id.clone(),
        ],
    )
}

pub fn publish_admission_result(
    root: &Path,
    record: &Path,
    state: AdmissionState,
    reason: Option<AdmissionRefusalReason>,
    exit_code: Option<i32>,
) -> Result<Projection, String> {
    let mut arguments = vec![
        "publish-admission-result".into(),
        "--path".into(),
        record.display().to_string(),
        "--state".into(),
        state.as_str().into(),
    ];
    if let Some(reason) = reason {
        arguments.push("--reason".into());
        arguments.push(reason.as_str().into());
    }
    if let Some(exit_code) = exit_code {
        arguments.push("--exit-code".into());
        arguments.push(exit_code.to_string());
    }
    run_authority(root, &arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn fixture_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ci-hub-run-handle-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_authority_fixture(&root);
        root
    }

    fn current_record(state: &str) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "kind": "validate",
            "state": state,
            "unit": "validate-a.service",
            "target": SHA,
            "repo": "rrnewton/hermit",
            "branch": "codex/validate-branch-provenance",
            "checkout": "/tmp/checkout",
            "source_checkout": "/tmp/source",
            "temporary_checkout": true,
            "cargo_home": "/tmp/cargo",
            "log": "/tmp/validate.log",
            "agent": "type-audit",
            "pr": null,
            "started_at": "2026-08-28T07:00:00Z",
            "parent_checkout_head": SHA,
            "producer": "ci-hub/validate/run_registry.py",
            "admission": "ci-hub validate-lock",
            "pane_role": "observer-only",
            "validate_lock_child_deadline_seconds": 3600,
            "e2e_result_root": "/tmp/e2e",
            "safe_ci_dag_runner_log_dir": "/tmp/dag",
            "hermit_run_timeout_seconds": 3300
        })
    }

    #[test]
    fn historical_schema_one_kind_is_not_the_current_discriminator() {
        let historical = serde_json::json!({
            "schema_version": 1,
            "kind": "bench",
            "producer": "systemd-user-v1",
            "state": "completed"
        });
        assert!(!is_current(&historical));
    }

    #[test]
    fn rust_consumers_use_the_python_owned_current_contract() {
        let root = fixture_root();
        let runs = root.join("ignored/validate/runs");
        fs::create_dir_all(&runs).unwrap();
        let record = runs.join("validate-a.json");
        fs::write(
            &record,
            serde_json::to_vec(&current_record("launching")).unwrap(),
        )
        .unwrap();

        let projected = verify_attribution(&root, &record, "type-audit", SHA, "validate")
            .expect("current handle must be accepted by its producer-owned authority");
        assert!(projected.lock_admissible);
        assert!(projected.counts_as_validation);
        assert_eq!(
            projected.branch.as_deref(),
            Some("codex/validate-branch-provenance")
        );

        let identity = ProcessIdentity {
            pid: 7,
            start_ticks: 11,
            boot_id: "boot-a".into(),
        };
        bind_process_identity(&root, &record, &identity).unwrap();
        verify_holder(
            &root,
            &record,
            "type-audit",
            SHA,
            "validate",
            "validate-a.service",
            &identity,
        )
        .unwrap();
        let projected =
            publish_admission_result(&root, &record, AdmissionState::Admitted, None, None).unwrap();
        assert_eq!(
            Some(AdmissionState::Admitted),
            projected
                .admission_result
                .as_ref()
                .map(|result| result.state)
        );
        assert_eq!(
            Some(1),
            projected
                .admission_result
                .as_ref()
                .and_then(|result| result.run_number)
        );
        let error = publish_admission_result(
            &root,
            &record,
            AdmissionState::Refused,
            Some(AdmissionRefusalReason::ValidationLockRefusal),
            Some(3),
        )
        .expect_err("a recorded admission must not be replaced by a refusal");
        assert!(error.contains("different admission result"), "{error}");

        fs::write(
            &record,
            serde_json::to_vec(&current_record("failed")).unwrap(),
        )
        .unwrap();
        let error = verify_attribution(&root, &record, "type-audit", SHA, "validate")
            .expect_err("a single-record identity check must still refuse malformed state");
        assert!(error.contains("validation-run-handle-state"), "{error}");

        let report = inspect_current_records(&root, &[record.clone()])
            .expect("bulk inspection must isolate a malformed record");
        assert!(report.records.is_empty());
        assert_eq!(report.unreadable.len(), 1);
        assert_eq!(report.unreadable[0].path, record);
        assert!(
            report.unreadable[0]
                .error
                .contains("validation-run-handle-state"),
            "{}",
            report.unreadable[0].error
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn multi_record_inspection_isolates_a_malformed_handle_between_valid_handles() {
        let root = fixture_root();
        let runs = root.join("ignored/validate/runs");
        fs::create_dir_all(&runs).unwrap();
        let first = runs.join("validate-first.json");
        let second = runs.join("validate-second.json");
        let mut first_value = current_record("running");
        first_value["unit"] = serde_json::json!("validate-first.service");
        let mut second_value = current_record("launching");
        second_value["unit"] = serde_json::json!("validate-second.service");
        fs::write(&first, serde_json::to_vec(&first_value).unwrap()).unwrap();
        let mut malformed = current_record("cancelled");
        malformed["unit"] = serde_json::json!("validate-malformed.service");
        let malformed_path = runs.join("validate-malformed.json");
        fs::write(&malformed_path, serde_json::to_vec(&malformed).unwrap()).unwrap();
        fs::write(&second, serde_json::to_vec(&second_value).unwrap()).unwrap();

        let report = inspect_current_records(
            &root,
            &[first.clone(), malformed_path.clone(), second.clone()],
        )
        .expect("one malformed record must not hide valid neighbors");
        assert_eq!(
            report
                .records
                .iter()
                .map(|record| record.path.as_path())
                .collect::<Vec<_>>(),
            [first.as_path(), second.as_path()]
        );
        assert_eq!(report.unreadable.len(), 1);
        assert_eq!(report.unreadable[0].path, malformed_path);
        assert!(
            report.unreadable[0]
                .error
                .contains("validation-run-handle-state"),
            "{}",
            report.unreadable[0].error
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn new_admission_state_is_refused_by_the_producer_owned_contract() {
        let root = fixture_root();
        let runs = root.join("ignored/validate/runs");
        fs::create_dir_all(&runs).unwrap();
        let mut malformed = current_record("running");
        malformed["admission_result"] = serde_json::json!({
            "state": "waiting",
            "recorded_at": "2026-08-28T07:00:01Z"
        });
        let record = runs.join("validate-malformed-admission.json");
        fs::write(&record, serde_json::to_vec(&malformed).unwrap()).unwrap();

        let report = inspect_current_records(&root, &[record])
            .expect("bulk inspection must isolate an unsupported admission state");
        assert!(report.records.is_empty());
        assert_eq!(report.unreadable.len(), 1);
        let error = &report.unreadable[0].error;
        assert!(
            error.contains("validation-run-handle-admission_result.state"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn admitted_validation_without_run_number_is_valid_and_renders_absent() {
        let root = fixture_root();
        let runs = root.join("ignored/validate/runs");
        fs::create_dir_all(&runs).unwrap();
        let mut historical = current_record("running");
        historical["admission_result"] = serde_json::json!({
            "state": "admitted",
            "recorded_at": "2026-08-28T07:00:01Z"
        });
        let record = runs.join("validate-historical.json");
        fs::write(&record, serde_json::to_vec(&historical).unwrap()).unwrap();

        let mut inspected = inspect_current_records(&root, &[record])
            .expect("the optional display counter must not define admission");
        let admission = inspected
            .records
            .pop()
            .expect("one projected record")
            .record
            .admission_result
            .expect("typed admission result");
        assert_eq!(admission.state, AdmissionState::Admitted);
        assert_eq!(admission.run_number, None);
        let _ = fs::remove_dir_all(root);
    }
}
