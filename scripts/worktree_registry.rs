//! Producer-owned contract for `worktree-state.json` schema 3.
//!
//! The allocator and releaser write this file. The registry checker and the
//! Python ownership/health readers consume it. Those readers intentionally ask
//! different questions of a slot status, but they must first agree that the
//! status and the rest of the row are well-formed. In particular, a missing
//! status must never mean `active` to one reader, `released` to another, and
//! still-owning to a third.
//!
//! `owner_sidecar` and `coordinator_lease` are separately versioned artifact
//! families, so this type preserves them as JSON values. The registry's own
//! fields and closed status values are complete here.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u64 = 3;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlotStatus {
    Active,
    Held,
    BindingCoordinatorLease,
    RevokingCoordinatorLease,
    OwnerLeaseRevoked,
    Releasing,
    FinalizingOrphanResidue,
    LeaseQuarantined,
    QuarantinedRetired,
    ReleaseRequested,
    Released,
}

impl SlotStatus {
    pub fn parse(value: &str) -> Result<Self, String> {
        Ok(match value {
            "active" => Self::Active,
            "held" => Self::Held,
            "binding-coordinator-lease" => Self::BindingCoordinatorLease,
            "revoking-coordinator-lease" => Self::RevokingCoordinatorLease,
            "owner-lease-revoked" => Self::OwnerLeaseRevoked,
            "releasing" => Self::Releasing,
            "finalizing-orphan-residue" => Self::FinalizingOrphanResidue,
            "lease-quarantined" => Self::LeaseQuarantined,
            "quarantined-retired" => Self::QuarantinedRetired,
            "release-requested" => Self::ReleaseRequested,
            "released" => Self::Released,
            other => return Err(format!("unsupported slot status {other:?}")),
        })
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Active => "active",
            Self::Held => "held",
            Self::BindingCoordinatorLease => "binding-coordinator-lease",
            Self::RevokingCoordinatorLease => "revoking-coordinator-lease",
            Self::OwnerLeaseRevoked => "owner-lease-revoked",
            Self::Releasing => "releasing",
            Self::FinalizingOrphanResidue => "finalizing-orphan-residue",
            Self::LeaseQuarantined => "lease-quarantined",
            Self::QuarantinedRetired => "quarantined-retired",
            Self::ReleaseRequested => "release-requested",
            Self::Released => "released",
        }
    }

    /// The allocator's existing one-slot-per-agent rule treats these two
    /// historical states as no longer owning a slot. Every other state still
    /// blocks a second allocation until its lifecycle is resolved.
    pub fn blocks_new_allocation(&self) -> bool {
        !matches!(self, Self::Released | Self::OwnerLeaseRevoked)
    }
}

impl<'de> Deserialize<'de> for SlotStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl Serialize for SlotStatus {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(dead_code)]
pub struct Agent {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub read_only: Option<bool>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub cgroup_path: Option<String>,
    #[serde(default)]
    pub tmux_pane_id: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(dead_code)]
pub struct Slot {
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub allocated: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    pub status: SlotStatus,
    pub hermit_path: String,
    pub hermit_branch: String,
    pub reverie_path: String,
    pub reverie_branch: String,
    pub liteinst2_path: String,
    pub liteinst2_branch: String,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub owner_sidecar: Option<Value>,
    #[serde(default)]
    pub owner_binding_failure: Option<Value>,
    #[serde(default)]
    pub coordinator_lease: Option<Value>,
    #[serde(default)]
    pub coordinator_lease_history: Option<Value>,
    #[serde(default)]
    pub coordinator_lease_revocation: Option<Value>,
    #[serde(default)]
    pub release_journal: Option<Value>,
    #[serde(default)]
    pub orphan_residue_journal: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(dead_code)]
pub struct Registry {
    pub version: u64,
    #[serde(default)]
    pub updated: Option<String>,
    pub slots: BTreeMap<String, Slot>,
    #[serde(default)]
    pub lease_history: Vec<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Registry {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|error| format!("invalid registry JSON: {error}"))?;
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "registry root is not an object".to_string())?;
        let version = object
            .get("version")
            .and_then(Value::as_u64)
            .ok_or_else(|| "registry version is missing or not an unsigned integer".to_string())?;
        if version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported registry version {version}; expected {SCHEMA_VERSION}"
            ));
        }
        let registry: Self = serde_json::from_value(value.clone())
            .map_err(|error| format!("registry schema {SCHEMA_VERSION}: {error}"))?;
        registry.validate()?;
        Ok(registry)
    }

    pub fn validate(&self) -> Result<(), String> {
        for (slot_name, slot) in &self.slots {
            if !valid_slot_name(slot_name) {
                return Err(format!("slot name {slot_name:?} is invalid"));
            }
            slot.validate(slot_name)?;
        }
        Ok(())
    }

    pub fn assigned_slots(&self, agent_name: &str) -> Vec<&str> {
        self.slots
            .iter()
            .filter(|(_, slot)| slot.status.blocks_new_allocation())
            .filter(|(_, slot)| {
                slot.agents
                    .iter()
                    .any(|agent| agent.name.as_deref() == Some(agent_name))
            })
            .map(|(slot_name, _)| slot_name.as_str())
            .collect()
    }
}

impl Slot {
    fn validate(&self, slot_name: &str) -> Result<(), String> {
        require_text(
            &format!("slot {slot_name} hermit_branch"),
            &self.hermit_branch,
        )?;
        require_text(
            &format!("slot {slot_name} reverie_branch"),
            &self.reverie_branch,
        )?;
        require_text(
            &format!("slot {slot_name} liteinst2_branch"),
            &self.liteinst2_branch,
        )?;
        require_text(&format!("slot {slot_name} hermit_path"), &self.hermit_path)?;
        require_text(
            &format!("slot {slot_name} reverie_path"),
            &self.reverie_path,
        )?;
        require_text(
            &format!("slot {slot_name} liteinst2_path"),
            &self.liteinst2_path,
        )?;

        Ok(())
    }
}

pub fn valid_slot_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        && value
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric())
}

fn require_text(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        Err(format!("{field} is empty"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_registry() -> Value {
        json!({
            "version": 3,
            "updated": "2026-08-28T00:00:00Z",
            "slots": {
                "slot01": {
                    "agents": [{"name": "hermit-01", "read_only": false, "task": ""}],
                    "allocated": "2026-08-28T00:00:00Z",
                    "updated": "2026-08-28T00:00:00Z",
                    "status": "active",
                    "hermit_path": "worktrees/slots/slot01/hermit",
                    "hermit_branch": "detached",
                    "reverie_path": "worktrees/slots/slot01/reverie",
                    "reverie_branch": "-",
                    "liteinst2_path": "worktrees/slots/slot01/liteinst2",
                    "liteinst2_branch": "-"
                }
            }
        })
    }

    #[test]
    fn accepts_current_core_with_optional_task_absent() {
        let registry = Registry::from_value(&valid_registry()).unwrap();
        assert_eq!(registry.slots.len(), 1);
    }

    #[test]
    fn missing_status_fails_by_name() {
        let mut value = valid_registry();
        value["slots"]["slot01"]
            .as_object_mut()
            .unwrap()
            .remove("status");
        let error = Registry::from_value(&value).unwrap_err();
        assert!(error.contains("status"), "{error}");
    }

    #[test]
    fn unknown_status_fails_by_value() {
        let mut value = valid_registry();
        value["slots"]["slot01"]["status"] = json!("future-state");
        let error = Registry::from_value(&value).unwrap_err();
        assert!(error.contains("future-state"), "{error}");
    }

    #[test]
    fn missing_path_fails_by_name() {
        let mut value = valid_registry();
        value["slots"]["slot01"]
            .as_object_mut()
            .unwrap()
            .remove("hermit_path");
        let error = Registry::from_value(&value).unwrap_err();
        assert!(error.contains("hermit_path"), "{error}");
    }

    #[test]
    fn version_is_read_before_the_shape() {
        let value = json!({"version": 4, "slots": []});
        assert_eq!(
            Registry::from_value(&value).unwrap_err(),
            "unsupported registry version 4; expected 3"
        );
    }

    #[test]
    fn assignment_ignores_released_and_revoked_rows() {
        let mut value = valid_registry();
        let active = value["slots"]["slot01"].clone();
        let mut released = active.clone();
        released["status"] = json!("released");
        released["hermit_path"] = json!("worktrees/slots/old/hermit");
        released["reverie_path"] = json!("worktrees/slots/old/reverie");
        released["liteinst2_path"] = json!("worktrees/slots/old/liteinst2");
        value["slots"]["old"] = released;
        let mut revoked = active;
        revoked["status"] = json!("owner-lease-revoked");
        revoked["hermit_path"] = json!("worktrees/slots/revoked/hermit");
        revoked["reverie_path"] = json!("worktrees/slots/revoked/reverie");
        revoked["liteinst2_path"] = json!("worktrees/slots/revoked/liteinst2");
        value["slots"]["revoked"] = revoked;

        let registry = Registry::from_value(&value).unwrap();
        assert_eq!(registry.assigned_slots("hermit-01"), vec!["slot01"]);
    }
}
