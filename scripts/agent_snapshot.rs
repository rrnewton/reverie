//! Shared contract for `ignored/ci-hub/agent-snapshot.json` schema 1.
//!
//! The stored snapshot is produced from identity-checked coordinator state
//! (ORC or Herdr) by both Python and Rust. It is consumed by slot allocation,
//! release, container reconciliation, active work reporting, and slot/agent
//! parity. Those consumers ask different questions, but they must not maintain
//! different answers to whether one serialized agent status is live, busy, or
//! broken.
//!
//! Coordinator producers own fields that this repository does not interpret.
//! They remain in `extra`; every field used by a repository consumer is typed
//! here.

#![allow(dead_code)]

use serde::{Deserialize, Deserializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

pub const SCHEMA_VERSION: u64 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentStatus {
    Active,
    Busy,
    Closed,
    Crashed,
    Dead,
    Disconnected,
    Error,
    Exited,
    Failed,
    Free,
    Idle,
    InProgress,
    Managed,
    NeedsInput,
    NeedsPermission,
    Retired,
    Running,
    Spawning,
    Stuck,
    Terminated,
    Unreachable,
    Unresponsive,
    Working,
}

impl AgentStatus {
    pub fn parse(value: &str) -> Result<Self, String> {
        Ok(match value {
            "active" => Self::Active,
            "busy" => Self::Busy,
            "closed" => Self::Closed,
            "crashed" => Self::Crashed,
            "dead" => Self::Dead,
            "disconnected" => Self::Disconnected,
            "error" => Self::Error,
            "exited" => Self::Exited,
            "failed" => Self::Failed,
            "free" => Self::Free,
            "idle" => Self::Idle,
            "in_progress" => Self::InProgress,
            "managed" => Self::Managed,
            "needs-input" => Self::NeedsInput,
            "needs-permission" => Self::NeedsPermission,
            "retired" => Self::Retired,
            "running" => Self::Running,
            "spawning" => Self::Spawning,
            "stuck" => Self::Stuck,
            "terminated" => Self::Terminated,
            "unreachable" => Self::Unreachable,
            "unresponsive" => Self::Unresponsive,
            "working" => Self::Working,
            other => return Err(format!("unsupported agent status {other:?}")),
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Busy => "busy",
            Self::Closed => "closed",
            Self::Crashed => "crashed",
            Self::Dead => "dead",
            Self::Disconnected => "disconnected",
            Self::Error => "error",
            Self::Exited => "exited",
            Self::Failed => "failed",
            Self::Free => "free",
            Self::Idle => "idle",
            Self::InProgress => "in_progress",
            Self::Managed => "managed",
            Self::NeedsInput => "needs-input",
            Self::NeedsPermission => "needs-permission",
            Self::Retired => "retired",
            Self::Running => "running",
            Self::Spawning => "spawning",
            Self::Stuck => "stuck",
            Self::Terminated => "terminated",
            Self::Unreachable => "unreachable",
            Self::Unresponsive => "unresponsive",
            Self::Working => "working",
        }
    }

    pub fn is_live(&self) -> bool {
        match self {
            Self::Active
            | Self::Busy
            | Self::Free
            | Self::Idle
            | Self::InProgress
            | Self::Managed
            | Self::NeedsInput
            | Self::NeedsPermission
            | Self::Running
            | Self::Spawning
            | Self::Working => true,
            Self::Closed
            | Self::Crashed
            | Self::Dead
            | Self::Disconnected
            | Self::Error
            | Self::Exited
            | Self::Failed
            | Self::Retired
            | Self::Stuck
            | Self::Terminated
            | Self::Unreachable
            | Self::Unresponsive => false,
        }
    }

    pub fn is_busy(&self) -> bool {
        match self {
            Self::Active
            | Self::Busy
            | Self::InProgress
            | Self::Running
            | Self::Spawning
            | Self::Working => true,
            Self::Closed
            | Self::Crashed
            | Self::Dead
            | Self::Disconnected
            | Self::Error
            | Self::Exited
            | Self::Failed
            | Self::Free
            | Self::Idle
            | Self::Managed
            | Self::NeedsInput
            | Self::NeedsPermission
            | Self::Retired
            | Self::Stuck
            | Self::Terminated
            | Self::Unreachable
            | Self::Unresponsive => false,
        }
    }

    pub fn is_broken(&self) -> bool {
        match self {
            Self::Crashed
            | Self::Disconnected
            | Self::Error
            | Self::Failed
            | Self::Stuck
            | Self::Unreachable
            | Self::Unresponsive => true,
            Self::Active
            | Self::Busy
            | Self::Closed
            | Self::Dead
            | Self::Exited
            | Self::Free
            | Self::Idle
            | Self::InProgress
            | Self::Managed
            | Self::NeedsInput
            | Self::NeedsPermission
            | Self::Retired
            | Self::Running
            | Self::Spawning
            | Self::Terminated
            | Self::Working => false,
        }
    }
}

impl<'de> Deserialize<'de> for AgentStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    Coordinator,
    Worker,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Agent {
    pub name: String,
    pub status: AgentStatus,
    #[serde(default)]
    pub current_task: Option<String>,
    #[serde(default)]
    pub tmux_pane_id: Option<String>,
    #[serde(default)]
    pub herdr_pane_id: Option<String>,
    #[serde(default)]
    pub last_activity: Option<f64>,
    #[serde(default)]
    pub role: Option<AgentRole>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedTask {
    pub local_id: String,
    pub owner: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotAuthority {
    pub source: String,
    pub taskgraph_db: String,
    pub binding_sha256: String,
    pub owned_in_progress: Vec<OwnedTask>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSnapshot {
    pub schema_version: u64,
    pub captured_at: f64,
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub authority: Option<SnapshotAuthority>,
}

impl AgentSnapshot {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|error| format!("invalid agent snapshot JSON: {error}"))?;
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "agent snapshot root is not an object".to_string())?;
        let version = object
            .get("schema_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                "agent snapshot schema_version is missing or not an unsigned integer".to_string()
            })?;
        if version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported agent snapshot schema {version}; expected {SCHEMA_VERSION}"
            ));
        }
        let snapshot: Self = serde_json::from_value(value.clone())
            .map_err(|error| format!("agent snapshot schema {SCHEMA_VERSION}: {error}"))?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.captured_at.is_finite() || self.captured_at < 0.0 {
            return Err(
                "agent snapshot captured_at is not a nonnegative finite number".to_string(),
            );
        }
        let mut names = BTreeSet::new();
        for (index, agent) in self.agents.iter().enumerate() {
            agent.validate(index)?;
            if !names.insert(agent.name.as_str()) {
                return Err(format!(
                    "agent snapshot has duplicate agent name {:?}",
                    agent.name
                ));
            }
        }
        if let Some(authority) = &self.authority {
            authority.validate()?;
        }
        Ok(())
    }

    pub fn ensure_fresh(&self, now: f64, max_age_secs: f64) -> Result<(), String> {
        let age = now - self.captured_at;
        if !(0.0..=max_age_secs).contains(&age) {
            return Err(format!(
                "agent snapshot is not fresh: age={}s max={}s",
                age.max(0.0) as u64,
                max_age_secs as u64
            ));
        }
        Ok(())
    }

    pub fn ensure_nonempty(&self) -> Result<(), String> {
        if self.agents.is_empty() {
            Err("agent snapshot lists no agents; refusing an empty census".to_string())
        } else {
            Ok(())
        }
    }

    pub fn ensure_canonical_authority(&self, snapshot_path: &Path) -> Result<(), String> {
        if !snapshot_path.ends_with(Path::new("ignored/ci-hub/agent-snapshot.json")) {
            return Ok(());
        }
        let Some(root) = snapshot_path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
        else {
            return Ok(());
        };
        let binding_path = root.join(".tick-hub/herdr-agent-bindings.json");
        let binding_bytes = match fs::read(&binding_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "cannot inspect Herdr authority binding {}: {error}",
                    binding_path.display()
                ));
            }
        };
        let binding: Value = serde_json::from_slice(&binding_bytes).map_err(|error| {
            format!(
                "invalid Herdr authority binding {}: {error}",
                binding_path.display()
            )
        })?;
        if binding.get("schema_version").and_then(Value::as_u64) != Some(2) {
            return Err(
                "unsupported Herdr authority binding; legacy snapshot remains disabled".into(),
            );
        }
        let expected_db = binding
            .get("taskgraph_db")
            .and_then(Value::as_str)
            .filter(|value| Path::new(value).is_absolute())
            .ok_or_else(|| "Herdr authority binding has no absolute taskgraph_db".to_string())?;
        let expected_db = fs::canonicalize(expected_db).map_err(|error| {
            format!("cannot canonicalize Herdr authority taskgraph_db: {error}")
        })?;
        let authority = self
            .authority
            .as_ref()
            .ok_or_else(|| "canonical Herdr snapshot lacks composite authority".to_string())?;
        let binding_sha256 = format!("{:x}", Sha256::digest(&binding_bytes));
        if authority.source != "herdr-agentctl-snapshot-v2"
            || authority.binding_sha256 != binding_sha256
            || authority.taskgraph_db != expected_db.to_string_lossy()
        {
            return Err(
                "canonical Herdr snapshot composite authority does not match its binding".into(),
            );
        }
        Ok(())
    }

    pub fn agent(&self, name: &str) -> Option<&Agent> {
        self.agents.iter().find(|agent| agent.name == name)
    }
}

impl SnapshotAuthority {
    fn validate(&self) -> Result<(), String> {
        if self.source != "herdr-agentctl-snapshot-v2" {
            return Err(format!(
                "unsupported agent snapshot authority source {:?}",
                self.source
            ));
        }
        if !self.taskgraph_db.starts_with('/') || self.taskgraph_db.contains('\0') {
            return Err("agent snapshot authority taskgraph_db is not an absolute path".into());
        }
        if self.binding_sha256.len() != 64
            || !self
                .binding_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("agent snapshot authority binding_sha256 is not lowercase SHA-256".into());
        }
        let mut previous: Option<(&str, &str)> = None;
        for (index, task) in self.owned_in_progress.iter().enumerate() {
            if task.local_id.is_empty()
                || task.local_id.trim() != task.local_id
                || task.owner.is_empty()
                || task.owner.trim() != task.owner
            {
                return Err(format!(
                    "agent snapshot authority owned task {index} is empty or untrimmed"
                ));
            }
            let current = (task.local_id.as_str(), task.owner.as_str());
            if previous.is_some_and(|value| value >= current) {
                return Err(
                    "agent snapshot authority owned tasks are not strictly sorted and unique"
                        .into(),
                );
            }
            previous = Some(current);
        }
        Ok(())
    }
}

impl Agent {
    fn validate(&self, index: usize) -> Result<(), String> {
        if self.name.is_empty() || self.name.trim() != self.name {
            return Err(format!(
                "agent snapshot entry {index} has an empty or untrimmed name"
            ));
        }
        if let Some(task) = &self.current_task {
            if task.trim() != task {
                return Err(format!(
                    "agent snapshot entry {index} current_task is not trimmed"
                ));
            }
        }
        if let Some(pane) = &self.tmux_pane_id {
            let digits = pane.strip_prefix('%').unwrap_or_default();
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(format!(
                    "agent snapshot entry {index} has invalid tmux_pane_id {pane:?}"
                ));
            }
        }
        if let Some(pane) = &self.herdr_pane_id {
            let valid = pane
                .strip_prefix('w')
                .and_then(|value| value.split_once(":p"))
                .is_some_and(|(workspace, pane)| {
                    !workspace.is_empty()
                        && !pane.is_empty()
                        && workspace.bytes().all(|byte| byte.is_ascii_alphanumeric())
                        && pane.bytes().all(|byte| byte.is_ascii_alphanumeric())
                });
            if !valid {
                return Err(format!(
                    "agent snapshot entry {index} has invalid herdr_pane_id {pane:?}"
                ));
            }
        }
        if self.tmux_pane_id.is_some() && self.herdr_pane_id.is_some() {
            return Err(format!(
                "agent snapshot entry {index} carries both tmux and Herdr pane identities"
            ));
        }
        if let Some(last_activity) = self.last_activity {
            if !last_activity.is_finite() || last_activity < 0.0 {
                return Err(format!(
                    "agent snapshot entry {index} has invalid last_activity"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_snapshot() -> Value {
        json!({
            "schema_version": 1,
            "captured_at": 1_787_893_230.0,
            "agents": [{
                "name": "type-audit",
                "status": "busy",
                "current_task": "twenty-two-remaining-type-families-from-the-published-census",
                "tmux_pane_id": "%20",
                "last_activity": 1_787_893_200,
                "backend": "cloud-agent"
            }]
        })
    }

    #[test]
    fn accepts_current_shape_and_preserves_uninterpreted_fields() {
        let snapshot = AgentSnapshot::from_value(&valid_snapshot()).unwrap();
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].extra["backend"], "cloud-agent");
    }

    #[test]
    fn accepts_one_exact_herdr_pane_identity() {
        let mut value = valid_snapshot();
        let agent = value["agents"][0].as_object_mut().unwrap();
        agent.remove("tmux_pane_id");
        agent.insert("herdr_pane_id".to_string(), json!("wK:p2"));
        let snapshot = AgentSnapshot::from_value(&value).unwrap();
        assert_eq!(snapshot.agents[0].herdr_pane_id.as_deref(), Some("wK:p2"));
    }

    #[test]
    fn accepts_only_schema_checked_coordinator_or_worker_roles() {
        let mut coordinator = valid_snapshot();
        coordinator["agents"][0]["role"] = json!("coordinator");
        let snapshot = AgentSnapshot::from_value(&coordinator).unwrap();
        assert_eq!(snapshot.agents[0].role, Some(AgentRole::Coordinator));

        let mut malformed = valid_snapshot();
        malformed["agents"][0]["role"] = json!("observer");
        assert!(AgentSnapshot::from_value(&malformed)
            .unwrap_err()
            .contains("unknown variant"));
    }

    #[test]
    fn accepts_only_bound_sorted_composite_authority() {
        let mut value = valid_snapshot();
        value["authority"] = json!({
            "source": "herdr-agentctl-snapshot-v2",
            "taskgraph_db": "/state/tasks.db",
            "binding_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "owned_in_progress": [
                {"local_id": "a", "owner": "worker-a"},
                {"local_id": "b", "owner": "worker-b"}
            ]
        });
        assert!(AgentSnapshot::from_value(&value)
            .unwrap()
            .authority
            .is_some());

        value["authority"]["owned_in_progress"] = json!([
            {"local_id": "b", "owner": "worker-b"},
            {"local_id": "a", "owner": "worker-a"}
        ]);
        assert!(AgentSnapshot::from_value(&value)
            .unwrap_err()
            .contains("strictly sorted"));
    }

    #[test]
    fn canonical_binding_requires_matching_composite_generation() {
        let root = std::env::temp_dir().join(format!(
            "agent-snapshot-authority-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("ignored/ci-hub/agent-snapshot.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::create_dir_all(root.join(".tick-hub")).unwrap();
        fs::create_dir_all(root.join("state")).unwrap();
        fs::write(root.join("state/tasks.db"), b"fixture").unwrap();
        let binding = format!(
            "{{\"schema_version\":2,\"taskgraph_db\":\"{}/state/../state/tasks.db\"}}",
            root.display()
        );
        fs::write(
            root.join(".tick-hub/herdr-agent-bindings.json"),
            binding.as_bytes(),
        )
        .unwrap();

        let legacy = AgentSnapshot::from_value(&valid_snapshot()).unwrap();
        assert!(legacy
            .ensure_canonical_authority(&path)
            .unwrap_err()
            .contains("lacks composite authority"));

        let mut composite = valid_snapshot();
        composite["authority"] = json!({
            "source": "herdr-agentctl-snapshot-v2",
            "taskgraph_db": root.join("state/tasks.db").to_string_lossy(),
            "binding_sha256": format!("{:x}", Sha256::digest(binding.as_bytes())),
            "owned_in_progress": []
        });
        AgentSnapshot::from_value(&composite)
            .unwrap()
            .ensure_canonical_authority(&path)
            .unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_ambiguous_or_malformed_herdr_pane_identity() {
        let mut both = valid_snapshot();
        both["agents"][0]["herdr_pane_id"] = json!("wK:p2");
        assert!(AgentSnapshot::from_value(&both)
            .unwrap_err()
            .contains("both tmux and Herdr"));

        let mut malformed = valid_snapshot();
        let agent = malformed["agents"][0].as_object_mut().unwrap();
        agent.remove("tmux_pane_id");
        agent.insert("herdr_pane_id".to_string(), json!("not-a-pane"));
        assert!(AgentSnapshot::from_value(&malformed)
            .unwrap_err()
            .contains("invalid herdr_pane_id"));
    }

    #[test]
    fn version_is_read_before_the_shape() {
        let value = json!({"schema_version": 2, "agents": "not-an-array"});
        assert_eq!(
            AgentSnapshot::from_value(&value).unwrap_err(),
            "unsupported agent snapshot schema 2; expected 1"
        );
    }

    #[test]
    fn missing_and_unknown_statuses_fail_by_name() {
        let mut missing = valid_snapshot();
        missing["agents"][0]
            .as_object_mut()
            .unwrap()
            .remove("status");
        assert!(AgentSnapshot::from_value(&missing)
            .unwrap_err()
            .contains("status"));

        let mut unknown = valid_snapshot();
        unknown["agents"][0]["status"] = json!("future-state");
        assert!(AgentSnapshot::from_value(&unknown)
            .unwrap_err()
            .contains("future-state"));
    }

    #[test]
    fn duplicate_names_fail_by_name_and_empty_is_a_consumer_policy() {
        let mut duplicate = valid_snapshot();
        let second = duplicate["agents"][0].clone();
        duplicate["agents"].as_array_mut().unwrap().push(second);
        assert!(AgentSnapshot::from_value(&duplicate)
            .unwrap_err()
            .contains("duplicate agent name"));

        let empty = json!({"schema_version": 1, "captured_at": 1, "agents": []});
        let empty = AgentSnapshot::from_value(&empty).unwrap();
        assert!(empty
            .ensure_nonempty()
            .unwrap_err()
            .contains("empty census"));
    }

    #[test]
    fn shared_status_semantics_cover_current_disagreement() {
        assert!(AgentStatus::Busy.is_live());
        assert!(AgentStatus::Busy.is_busy());
        assert!(AgentStatus::Spawning.is_live());
        assert!(AgentStatus::Spawning.is_busy());
        assert!(!AgentStatus::Spawning.is_broken());
        assert!(AgentStatus::NeedsPermission.is_live());
        assert!(!AgentStatus::NeedsPermission.is_busy());
        assert!(!AgentStatus::NeedsPermission.is_broken());
        assert!(!AgentStatus::Unreachable.is_live());
        assert!(AgentStatus::Unreachable.is_broken());
        assert!(!AgentStatus::Stuck.is_live());
    }
}
