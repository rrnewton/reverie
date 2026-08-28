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
//! families. `OwnerSidecar` is the complete shared contract for the first; its
//! slot field remains a JSON value so one malformed historical sidecar can be
//! reported for that slot without making every other liveness result vanish.
//! `coordinator_lease` remains opaque until its own contract is shared. The
//! registry's own fields and closed status values are complete here.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u64 = 3;
pub const OWNER_SIDECAR_SCHEMA_VERSION: u64 = 1;
pub const OWNER_LEASE_SCHEMA_VERSION: u64 = 1;
#[allow(dead_code)]
pub const OWNER_LEASE_TTL_SECONDS: u64 = 86_400;

const OWNER_SIDECAR_FIELDS: &[&str] = &[
    "schema_version",
    "source",
    "slot",
    "agent",
    "task",
    "recorded_at",
    "generation",
    "supervisor_pid",
    "boot_id",
    "start_ticks",
    "tmux_pane_id",
    "cgroup_path",
];
const OWNER_LEASE_FIELDS: &[&str] = &[
    "schema_version",
    "resource",
    "generation",
    "issued_at",
    "renewed_at",
    "ttl_seconds",
    "expires_at",
    "state",
    "last_transition_actor",
    "automatic_handoff",
];

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerSidecarSource {
    OrcTmuxPaneSupervisorV1,
    CodexSystemdSentinelV1,
}

impl OwnerSidecarSource {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "orc-tmux-pane-supervisor-v1" => Ok(Self::OrcTmuxPaneSupervisorV1),
            "codex-systemd-sentinel-v1" => Ok(Self::CodexSystemdSentinelV1),
            other => Err(format!("owner_sidecar source is unsupported: {other:?}")),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OrcTmuxPaneSupervisorV1 => "orc-tmux-pane-supervisor-v1",
            Self::CodexSystemdSentinelV1 => "codex-systemd-sentinel-v1",
        }
    }
}

impl<'de> Deserialize<'de> for OwnerSidecarSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl Serialize for OwnerSidecarSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerLeaseState {
    Active,
    Draining,
    Quarantined,
}

impl OwnerLeaseState {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "active" => Ok(Self::Active),
            "draining" => Ok(Self::Draining),
            "quarantined" => Ok(Self::Quarantined),
            other => Err(format!(
                "owner_sidecar lease state is unsupported: {other:?}"
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Quarantined => "quarantined",
        }
    }
}

impl<'de> Deserialize<'de> for OwnerLeaseState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl Serialize for OwnerLeaseState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerLease {
    pub schema_version: u64,
    pub resource: String,
    pub generation: String,
    pub issued_at: u64,
    pub renewed_at: u64,
    pub ttl_seconds: u64,
    pub expires_at: u64,
    pub state: OwnerLeaseState,
    pub last_transition_actor: String,
    pub automatic_handoff: Option<Value>,
}

#[allow(dead_code)]
impl OwnerLease {
    fn active(slot: &str, generation: &str, issued_at: u64, actor: &str) -> Self {
        Self {
            schema_version: OWNER_LEASE_SCHEMA_VERSION,
            resource: format!("slot/{slot}"),
            generation: generation.to_string(),
            issued_at,
            renewed_at: issued_at,
            ttl_seconds: OWNER_LEASE_TTL_SECONDS,
            expires_at: issued_at.saturating_add(OWNER_LEASE_TTL_SECONDS),
            state: OwnerLeaseState::Active,
            last_transition_actor: actor.to_string(),
            automatic_handoff: None,
        }
    }

    fn validate(&self, slot: &str, generation: &str) -> Result<(), String> {
        if self.schema_version != OWNER_LEASE_SCHEMA_VERSION {
            return Err(format!(
                "owner_sidecar lease schema_version is unsupported: {}",
                self.schema_version
            ));
        }
        if self.resource != format!("slot/{slot}") {
            return Err("owner_sidecar lease resource does not match slot".into());
        }
        if self.generation != generation {
            return Err("owner_sidecar lease generation does not bind its exact identity".into());
        }
        if self.ttl_seconds == 0 {
            return Err("owner_sidecar lease ttl_seconds is not positive".into());
        }
        if self.renewed_at < self.issued_at {
            return Err("owner_sidecar lease renewed_at precedes issued_at".into());
        }
        if self.renewed_at.checked_add(self.ttl_seconds) != Some(self.expires_at) {
            return Err(
                "owner_sidecar lease expires_at does not equal renewed_at + ttl_seconds".into(),
            );
        }
        require_text(
            "owner_sidecar lease last_transition_actor",
            &self.last_transition_actor,
        )
    }

    fn from_value(value: &Value, slot: &str, generation: &str) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "owner_sidecar lease is not an object".to_string())?;
        let version = object
            .get("schema_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                "owner_sidecar lease schema_version is missing or not an integer".to_string()
            })?;
        if version != OWNER_LEASE_SCHEMA_VERSION {
            return Err(format!(
                "owner_sidecar lease schema_version is unsupported: {version}"
            ));
        }
        require_exact_fields("owner_sidecar lease", object, OWNER_LEASE_FIELDS, &[])?;
        let lease: Self = serde_json::from_value(value.clone())
            .map_err(|error| format!("owner_sidecar lease schema {version}: {error}"))?;
        lease.validate(slot, generation)?;
        Ok(lease)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerSidecar {
    pub schema_version: u64,
    pub source: OwnerSidecarSource,
    pub slot: String,
    pub agent: String,
    pub task: String,
    pub recorded_at: String,
    pub generation: String,
    pub supervisor_pid: u32,
    pub boot_id: String,
    pub start_ticks: u64,
    pub tmux_pane_id: Option<String>,
    pub cgroup_path: String,
    #[serde(default)]
    pub lease: Option<OwnerLease>,
}

#[allow(dead_code)]
impl OwnerSidecar {
    #[allow(clippy::too_many_arguments)]
    pub fn current(
        slot: &str,
        agent: &str,
        task: &str,
        recorded_at: &str,
        source: OwnerSidecarSource,
        supervisor_pid: u32,
        boot_id: &str,
        start_ticks: u64,
        tmux_pane_id: Option<&str>,
        cgroup_path: &str,
        issued_at: u64,
    ) -> Result<Self, String> {
        let generation = format!("{boot_id}:{supervisor_pid}:{start_ticks}");
        let result = Self {
            schema_version: OWNER_SIDECAR_SCHEMA_VERSION,
            source,
            slot: slot.to_string(),
            agent: agent.to_string(),
            task: task.to_string(),
            recorded_at: recorded_at.to_string(),
            generation: generation.clone(),
            supervisor_pid,
            boot_id: boot_id.to_string(),
            start_ticks,
            tmux_pane_id: tmux_pane_id.map(str::to_string),
            cgroup_path: cgroup_path.to_string(),
            lease: Some(OwnerLease::active(slot, &generation, issued_at, agent)),
        };
        result.validate(slot)?;
        match (result.source, result.tmux_pane_id.is_some()) {
            (OwnerSidecarSource::OrcTmuxPaneSupervisorV1, false) => {
                return Err("ORC owner_sidecar has no tmux pane binding".into());
            }
            (OwnerSidecarSource::OrcTmuxPaneSupervisorV1, true) => {}
            (OwnerSidecarSource::CodexSystemdSentinelV1, true) => {
                return Err("Codex sentinel owner_sidecar must not claim a tmux pane".into());
            }
            (OwnerSidecarSource::CodexSystemdSentinelV1, false) => {}
        }
        Ok(result)
    }

    pub fn from_value(value: &Value, expected_slot: &str) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "owner_sidecar is not an object".to_string())?;
        let version = object
            .get("schema_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                "owner_sidecar schema_version is missing or not an integer".to_string()
            })?;
        if version != OWNER_SIDECAR_SCHEMA_VERSION {
            return Err(format!(
                "owner_sidecar schema_version is unsupported: {version}"
            ));
        }
        require_exact_fields("owner_sidecar", object, OWNER_SIDECAR_FIELDS, &["lease"])?;
        if let Some(lease) = object.get("lease").filter(|value| !value.is_null()) {
            let generation = object
                .get("generation")
                .and_then(Value::as_str)
                .ok_or_else(|| "owner_sidecar generation is missing or not text".to_string())?;
            OwnerLease::from_value(lease, expected_slot, generation)?;
        }
        let sidecar: Self = serde_json::from_value(value.clone())
            .map_err(|error| format!("owner_sidecar schema {version}: {error}"))?;
        sidecar.validate(expected_slot)?;
        Ok(sidecar)
    }

    pub fn from_slot_value(value: &Value, expected_slot: &str) -> Result<Self, String> {
        let slot: Slot = serde_json::from_value(value.clone())
            .map_err(|error| format!("registry slot {expected_slot}: {error}"))?;
        slot.validate(expected_slot)?;
        let sidecar_value = slot
            .owner_sidecar
            .as_ref()
            .ok_or_else(|| format!("slot {expected_slot} has no versioned owner_sidecar"))?;
        let sidecar = Self::from_value(sidecar_value, expected_slot)?;
        sidecar.validate_binding(&slot, expected_slot)?;
        Ok(sidecar)
    }

    pub fn to_value(&self) -> Result<Value, String> {
        serde_json::to_value(self).map_err(|error| format!("serialize owner_sidecar: {error}"))
    }

    fn validate(&self, expected_slot: &str) -> Result<(), String> {
        if self.schema_version != OWNER_SIDECAR_SCHEMA_VERSION {
            return Err(format!(
                "owner_sidecar schema_version is unsupported: {}",
                self.schema_version
            ));
        }
        if self.slot != expected_slot {
            return Err("owner_sidecar slot binding does not match registry key".into());
        }
        require_text("owner_sidecar agent", &self.agent)?;
        require_text("owner_sidecar recorded_at", &self.recorded_at)?;
        if self.supervisor_pid == 0 || self.start_ticks == 0 || self.boot_id.is_empty() {
            return Err("owner_sidecar supervisor identity is incomplete".into());
        }
        if self.generation
            != format!(
                "{}:{}:{}",
                self.boot_id, self.supervisor_pid, self.start_ticks
            )
        {
            return Err("owner_sidecar generation does not bind its exact identity".into());
        }
        if self.cgroup_path == "/" || !self.cgroup_path.starts_with('/') {
            return Err("owner_sidecar cgroup path is not normalized".into());
        }
        if let Some(pane) = &self.tmux_pane_id {
            if !pane.starts_with('%') {
                return Err("owner_sidecar tmux_pane_id is not a pane id".into());
            }
        }
        if let Some(lease) = &self.lease {
            lease.validate(expected_slot, &self.generation)?;
        }
        Ok(())
    }

    pub fn validate_binding(&self, slot: &Slot, expected_slot: &str) -> Result<(), String> {
        let mut owners = slot
            .agents
            .iter()
            .filter(|owner| owner.read_only != Some(true));
        let owner = owners
            .next()
            .ok_or_else(|| "slot has no mutating owner".to_string())?;
        if owners.next().is_some() {
            return Err("slot has multiple mutating owners".into());
        }
        let owner_name = owner
            .name
            .as_deref()
            .ok_or_else(|| "mutating owner has no name".to_string())?;
        if self.agent != owner_name {
            return Err("owner_sidecar agent binding does not match mutating owner".into());
        }
        let owner_task = owner.task.as_deref().or(slot.task.as_deref()).unwrap_or("");
        if self.task != owner_task {
            return Err("owner_sidecar task binding does not match mutating owner".into());
        }
        match self.source {
            OwnerSidecarSource::OrcTmuxPaneSupervisorV1 => {
                let pane = self
                    .tmux_pane_id
                    .as_deref()
                    .ok_or_else(|| "ORC owner_sidecar has no tmux pane binding".to_string())?;
                if owner.tmux_pane_id.as_deref() != Some(pane)
                    || owner.cgroup_path.as_deref() != Some(self.cgroup_path.as_str())
                {
                    return Err(
                        "ORC owner_sidecar disagrees with the registered pane/cgroup lease".into(),
                    );
                }
            }
            OwnerSidecarSource::CodexSystemdSentinelV1 => {
                if self.tmux_pane_id.is_some() {
                    return Err("Codex sentinel owner_sidecar must not claim a tmux pane".into());
                }
                let lease = slot
                    .coordinator_lease
                    .as_ref()
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        "Codex owner_sidecar disagrees with the registered coordinator lease"
                            .to_string()
                    })?;
                if lease.get("schema_version").and_then(Value::as_u64) != Some(1)
                    || lease.get("source").and_then(Value::as_str)
                        != Some("codex-systemd-sentinel-v1")
                    || lease.get("main_pid").and_then(Value::as_u64)
                        != Some(u64::from(self.supervisor_pid))
                    || lease.get("main_pid_starttime").and_then(Value::as_u64)
                        != Some(self.start_ticks)
                    || lease.get("cgroup_path").and_then(Value::as_str)
                        != Some(self.cgroup_path.as_str())
                {
                    return Err(
                        "Codex owner_sidecar disagrees with the registered coordinator lease"
                            .into(),
                    );
                }
            }
        }
        self.validate(expected_slot)
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

/// Validate each sidecar independently so one malformed historical slot does
/// not erase every other slot's liveness result. `None` means the shared type
/// and the registry binding both accepted the row; `Some` carries the named
/// refusal for that slot.
pub fn owner_sidecar_errors(registry: &Registry) -> BTreeMap<String, Option<String>> {
    registry
        .slots
        .iter()
        .map(|(slot_name, slot)| {
            let result = slot
                .owner_sidecar
                .as_ref()
                .ok_or_else(|| "slot has no versioned owner_sidecar".to_string())
                .and_then(|value| OwnerSidecar::from_value(value, slot_name))
                .and_then(|sidecar| sidecar.validate_binding(slot, slot_name));
            (slot_name.clone(), result.err())
        })
        .collect()
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

fn require_exact_fields(
    artifact: &str,
    object: &Map<String, Value>,
    required: &[&str],
    optional: &[&str],
) -> Result<(), String> {
    if let Some(field) = required.iter().find(|field| !object.contains_key(**field)) {
        return Err(format!("{artifact} field {field} is missing"));
    }
    if let Some(field) = object
        .keys()
        .find(|field| !required.contains(&field.as_str()) && !optional.contains(&field.as_str()))
    {
        return Err(format!("{artifact} field {field} is unsupported"));
    }
    Ok(())
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

    fn valid_orc_sidecar(slot: &str) -> Value {
        OwnerSidecar::current(
            slot,
            "hermit-01",
            "",
            "2026-08-28T00:00:00Z",
            OwnerSidecarSource::OrcTmuxPaneSupervisorV1,
            123,
            "boot-a",
            456,
            Some("%7"),
            "/agent.slice/a.scope",
            100,
        )
        .unwrap()
        .to_value()
        .unwrap()
    }

    fn registry_with_valid_sidecar() -> Value {
        let mut value = valid_registry();
        value["slots"]["slot01"]["agents"][0]["tmux_pane_id"] = json!("%7");
        value["slots"]["slot01"]["agents"][0]["cgroup_path"] = json!("/agent.slice/a.scope");
        value["slots"]["slot01"]["owner_sidecar"] = valid_orc_sidecar("slot01");
        value
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

    #[test]
    fn current_owner_sidecar_round_trips_with_exact_binding() {
        let registry = Registry::from_value(&registry_with_valid_sidecar()).unwrap();
        assert_eq!(owner_sidecar_errors(&registry).get("slot01"), Some(&None));
        let sidecar = OwnerSidecar::from_value(
            registry.slots["slot01"].owner_sidecar.as_ref().unwrap(),
            "slot01",
        )
        .unwrap();
        assert_eq!(sidecar.source, OwnerSidecarSource::OrcTmuxPaneSupervisorV1);
        assert_eq!(sidecar.lease.unwrap().state, OwnerLeaseState::Active);
        OwnerSidecar::from_slot_value(&registry_with_valid_sidecar()["slots"]["slot01"], "slot01")
            .unwrap();
    }

    #[test]
    fn missing_owner_sidecar_field_fails_by_name() {
        let mut sidecar = valid_orc_sidecar("slot01");
        sidecar.as_object_mut().unwrap().remove("task");
        let error = OwnerSidecar::from_value(&sidecar, "slot01").unwrap_err();
        assert!(error.contains("field task is missing"), "{error}");
    }

    #[test]
    fn unknown_owner_sidecar_field_fails_by_name() {
        let mut sidecar = valid_orc_sidecar("slot01");
        sidecar["future"] = json!(true);
        let error = OwnerSidecar::from_value(&sidecar, "slot01").unwrap_err();
        assert!(error.contains("field future is unsupported"), "{error}");
    }

    #[test]
    fn owner_sidecar_version_is_read_before_shape() {
        let sidecar = json!({"schema_version": 2, "future": true});
        assert_eq!(
            OwnerSidecar::from_value(&sidecar, "slot01").unwrap_err(),
            "owner_sidecar schema_version is unsupported: 2"
        );
    }

    #[test]
    fn owner_lease_version_is_read_before_shape() {
        let mut sidecar = valid_orc_sidecar("slot01");
        sidecar["lease"] = json!({"schema_version": 2, "future": true});
        assert_eq!(
            OwnerSidecar::from_value(&sidecar, "slot01").unwrap_err(),
            "owner_sidecar lease schema_version is unsupported: 2"
        );
    }

    #[test]
    fn unknown_owner_source_and_lease_state_fail_by_value() {
        let mut sidecar = valid_orc_sidecar("slot01");
        sidecar["source"] = json!("future-owner");
        let source_error = OwnerSidecar::from_value(&sidecar, "slot01").unwrap_err();
        assert!(source_error.contains("future-owner"), "{source_error}");

        let mut sidecar = valid_orc_sidecar("slot01");
        sidecar["lease"]["state"] = json!("future-state");
        let state_error = OwnerSidecar::from_value(&sidecar, "slot01").unwrap_err();
        assert!(state_error.contains("future-state"), "{state_error}");
    }

    #[test]
    fn owner_sidecar_binding_mismatch_fails_by_name() {
        let mut value = registry_with_valid_sidecar();
        value["slots"]["slot01"]["owner_sidecar"]["agent"] = json!("somebody-else");
        let registry = Registry::from_value(&value).unwrap();
        let errors = owner_sidecar_errors(&registry);
        let error = errors["slot01"]
            .as_ref()
            .expect("mismatched binding must be refused");
        assert!(error.contains("agent binding"), "{error}");
    }

    #[test]
    fn one_invalid_sidecar_does_not_erase_another_slot_result() {
        let mut value = registry_with_valid_sidecar();
        let mut second = value["slots"]["slot01"].clone();
        second["hermit_path"] = json!("worktrees/slots/slot02/hermit");
        second["reverie_path"] = json!("worktrees/slots/slot02/reverie");
        second["liteinst2_path"] = json!("worktrees/slots/slot02/liteinst2");
        second["owner_sidecar"] = valid_orc_sidecar("slot02");
        second["owner_sidecar"]
            .as_object_mut()
            .unwrap()
            .remove("task");
        value["slots"]["slot02"] = second;
        let registry = Registry::from_value(&value).unwrap();
        let errors = owner_sidecar_errors(&registry);
        assert_eq!(errors["slot01"], None);
        assert!(errors["slot02"]
            .as_ref()
            .is_some_and(|error| error.contains("field task is missing")));
    }
}
