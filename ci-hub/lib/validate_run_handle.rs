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
use serde::Deserialize;
use std::env;
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
pub struct Projection {
    pub unit: Option<String>,
    pub target: Option<String>,
    pub repo: Option<String>,
    pub agent: Option<String>,
    pub started_at: Option<String>,
    pub host: Option<String>,
    pub process_identity: Option<ProcessIdentity>,
    pub admission_result: Option<AdmissionResult>,
    pub lock_admissible: bool,
    pub counts_as_validation: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct InspectedRecord {
    pub path: PathBuf,
    pub record: Projection,
}

pub fn workspace_root_for_record(record: &Path) -> Result<PathBuf, String> {
    let runs = record
        .parent()
        .ok_or_else(|| format!("run record has no parent: {}", record.display()))?;
    let validate = runs
        .parent()
        .ok_or_else(|| format!("run record has no validate directory: {}", record.display()))?;
    let ignored = validate
        .parent()
        .ok_or_else(|| format!("run record has no ignored directory: {}", record.display()))?;
    ignored
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("run record has no workspace root: {}", record.display()))
}

/// Prefer the running ci-hub source tree over the shared state tree.
///
/// `validate-run --state-root` deliberately writes handles in the primary
/// checkout while executing tooling from a feature worktree. During that
/// coordinated rollout the state tree may not contain the current authority
/// yet, so deriving the checker only from the record would make the new writer
/// unable to start. `RUST_SCRIPT_PATH` is the same source identity used by the
/// ci-hub front door; direct binaries fall back to the record's workspace.
pub fn authority_root_for_record(record: &Path) -> Result<PathBuf, String> {
    if let Some(source) = env::var_os("RUST_SCRIPT_PATH").filter(|value| !value.is_empty()) {
        let source = PathBuf::from(source);
        if source.file_name().and_then(|name| name.to_str()) == Some("ci-hub.rs") {
            if let Some(root) = source.parent().and_then(Path::parent) {
                return Ok(root.to_path_buf());
            }
        }
    }
    workspace_root_for_record(record)
}

fn authority(root: &Path) -> PathBuf {
    root.join("ci-hub/validate/run_registry.py")
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
) -> Result<Vec<InspectedRecord>, String> {
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
        let validate = root.join("ci-hub/validate");
        fs::create_dir_all(&validate).unwrap();
        fs::write(
            validate.join("run_registry.py"),
            include_str!("../validate/run_registry.py"),
        )
        .unwrap();
        fs::write(
            validate.join("service_result.py"),
            include_str!("../validate/service_result.py"),
        )
        .unwrap();
        fs::write(
            validate.join("final_validate_status.py"),
            include_str!("../validate/final_validate_status.py"),
        )
        .unwrap();
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
    fn run_record_path_recovers_the_parent_workspace() {
        let record = Path::new("/parent/ignored/validate/runs/validate-a.json");
        assert_eq!(
            workspace_root_for_record(record).unwrap(),
            Path::new("/parent")
        );
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
        let error = inspect_current_records(&root, &[record.clone()])
            .expect_err("a bench-only state must be refused");
        assert!(error.contains("validation-run-handle-state"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn multi_record_inspection_refuses_a_malformed_current_handle_by_name() {
        let root = fixture_root();
        let runs = root.join("ignored/validate/runs");
        fs::create_dir_all(&runs).unwrap();
        fs::write(
            runs.join("historical.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "state": "cancelled"
            }))
            .unwrap(),
        )
        .unwrap();
        let mut malformed = current_record("cancelled");
        malformed["unit"] = serde_json::json!("validate-malformed.service");
        fs::write(
            runs.join("validate-malformed.json"),
            serde_json::to_vec(&malformed).unwrap(),
        )
        .unwrap();

        let error = inspect_current_records(&root, &[runs.join("validate-malformed.json")])
            .expect_err("a current state outside the contract must not disappear as terminal");
        assert!(error.contains("validation-run-handle-state"), "{error}");
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

        let error = inspect_current_records(&root, &[record])
            .expect_err("a new admission state must not acquire implicit semantics");
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
