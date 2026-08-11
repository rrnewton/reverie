//! Shared, fail-closed lease transition policy.
//!
//! Resource adapters own their canonical records and their mechanical probes.
//! This module owns the decision that combines those facts.  Keeping I/O out of
//! the policy makes it impossible for a validate lock, landing lock, slot, or
//! future holder to quietly substitute a weaker liveness rule.

use std::fmt;

pub(crate) const LEASE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LeaseState {
    Active,
    Draining,
    Expired,
    Quarantined,
    Cleared,
}

impl LeaseState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Expired => "expired",
            Self::Quarantined => "quarantined",
            Self::Cleared => "cleared",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OwnerLiveness {
    Alive,
    Dead { evidence: String },
    Unknown { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DomainState {
    ProvenEmpty { evidence: String },
    Populated { evidence: String },
    Unproven { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HandoffState {
    pub(crate) complete: bool,
    pub(crate) digest: Option<String>,
    pub(crate) continuation: Option<String>,
    pub(crate) recovery_sha: Option<String>,
    pub(crate) dirty: bool,
    pub(crate) task_closed: bool,
}

impl HandoffState {
    pub(crate) fn complete_ephemeral(digest: impl Into<String>) -> Self {
        Self {
            complete: true,
            digest: Some(digest.into()),
            continuation: None,
            recovery_sha: None,
            dirty: false,
            task_closed: false,
        }
    }

    fn refusal_reason(&self) -> Option<String> {
        if !self.complete {
            return Some("automatic recovery handoff is incomplete".into());
        }
        if self.digest.as_deref().is_none_or(str::is_empty) {
            return Some("automatic recovery handoff has no digest".into());
        }
        if self.dirty && self.recovery_sha.as_deref().is_none_or(str::is_empty) {
            return Some(
                "holder left dirty work without a durable recovery SHA; quarantine is required"
                    .into(),
            );
        }
        if self.task_closed && self.dirty {
            return Some(
                "task is closed while its live ownership still carries dirty work; task closure is not release authority"
                    .into(),
            );
        }
        None
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LeaseRecord {
    pub(crate) schema_version: u32,
    pub(crate) resource: String,
    pub(crate) generation: String,
    pub(crate) owner_generation: String,
    pub(crate) issued_at: i64,
    pub(crate) renewed_at: i64,
    pub(crate) ttl_seconds: u64,
    pub(crate) expires_at: i64,
    pub(crate) state: LeaseState,
}

impl LeaseRecord {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != LEASE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported lease schema_version={}",
                self.schema_version
            ));
        }
        if self.resource.is_empty()
            || self.generation.is_empty()
            || self.owner_generation.is_empty()
        {
            return Err("lease omits resource, generation, or owner generation".into());
        }
        if self.ttl_seconds == 0 {
            return Err("lease TTL must be positive".into());
        }
        if self.renewed_at < self.issued_at {
            return Err("lease renewed_at precedes issued_at".into());
        }
        let ttl = i64::try_from(self.ttl_seconds)
            .map_err(|_| "lease TTL is outside the supported range".to_string())?;
        if self.expires_at != self.renewed_at.saturating_add(ttl) {
            return Err("lease expires_at is not renewed_at + ttl_seconds".into());
        }
        Ok(())
    }

    pub(crate) fn effective_state(&self, now: i64) -> LeaseState {
        if self.state == LeaseState::Active && now >= self.expires_at {
            LeaseState::Expired
        } else {
            self.state
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LeaseDecision {
    ReleaseRequested,
    Clear { evidence: String },
    Refuse { reason: String },
    Quarantine { reason: String },
    AlreadyCleared,
}

impl fmt::Display for LeaseDecision {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReleaseRequested => write!(output, "release-requested"),
            Self::Clear { evidence } => write!(output, "clear: {evidence}"),
            Self::Refuse { reason } => write!(output, "refused: {reason}"),
            Self::Quarantine { reason } => write!(output, "quarantine: {reason}"),
            Self::AlreadyCleared => write!(output, "already-cleared"),
        }
    }
}

/// Decide whether the exact holder may begin a two-phase release.
///
/// Ownership authorizes the request.  Liveness is intentionally absent: a
/// holder is expected to be alive when it records its handoff and stops
/// renewing.  Final clearing is a separate decision below.
pub(crate) fn release_request(
    lease: &LeaseRecord,
    expected_generation: &str,
    actor_owner_generation: &str,
    handoff: &HandoffState,
) -> LeaseDecision {
    if let Err(reason) = lease.validate() {
        return LeaseDecision::Refuse { reason };
    }
    if lease.generation != expected_generation {
        return LeaseDecision::Refuse {
            reason: "lease generation changed before release request".into(),
        };
    }
    if lease.owner_generation != actor_owner_generation {
        return LeaseDecision::Refuse {
            reason: "release requester is not the exact holder generation".into(),
        };
    }
    if lease.state == LeaseState::Cleared {
        return LeaseDecision::AlreadyCleared;
    }
    if let Some(reason) = handoff.refusal_reason() {
        return LeaseDecision::Quarantine { reason };
    }
    LeaseDecision::ReleaseRequested
}

/// Decide whether a draining or expired lease may be cleared.
///
/// TTL is eligibility, never death proof.  A live owner refuses even after
/// expiry; an unreadable owner is quarantined rather than reinterpreted as
/// dead.  Adapters must re-check generation and domain under their final CAS
/// fence immediately before applying `Clear`.
pub(crate) fn stale_clear(
    lease: &LeaseRecord,
    expected_generation: &str,
    now: i64,
    liveness: OwnerLiveness,
    domain: DomainState,
    handoff: &HandoffState,
) -> LeaseDecision {
    if let Err(reason) = lease.validate() {
        return LeaseDecision::Refuse { reason };
    }
    if lease.generation != expected_generation {
        return LeaseDecision::Refuse {
            reason: "lease generation changed before stale clear".into(),
        };
    }
    let state = lease.effective_state(now);
    if state == LeaseState::Cleared {
        return LeaseDecision::AlreadyCleared;
    }
    if !matches!(
        state,
        LeaseState::Draining | LeaseState::Expired | LeaseState::Quarantined
    ) {
        return LeaseDecision::Refuse {
            reason: format!(
                "lease state {} is not eligible for stale clear",
                state.as_str()
            ),
        };
    }
    match liveness {
        OwnerLiveness::Alive => {
            return LeaseDecision::Refuse {
                reason: "exact holder generation is alive".into(),
            }
        }
        OwnerLiveness::Unknown { reason } => {
            return LeaseDecision::Quarantine {
                reason: format!("owner liveness is indeterminate: {reason}"),
            }
        }
        OwnerLiveness::Dead { evidence } => match domain {
            DomainState::Populated { evidence: domain } => {
                return LeaseDecision::Refuse {
                    reason: format!(
                        "holder is dead ({evidence}) but the cleanup domain is populated: {domain}"
                    ),
                }
            }
            DomainState::Unproven { reason } => {
                return LeaseDecision::Quarantine {
                    reason: format!("cleanup domain is unproven: {reason}"),
                }
            }
            DomainState::ProvenEmpty { evidence: domain } => {
                if let Some(reason) = handoff.refusal_reason() {
                    return LeaseDecision::Quarantine { reason };
                }
                return LeaseDecision::Clear {
                    evidence: format!(
                        "owner dead: {evidence}; cleanup domain empty: {domain}; handoff={}",
                        handoff.digest.as_deref().unwrap_or("missing")
                    ),
                };
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(state: LeaseState) -> LeaseRecord {
        LeaseRecord {
            schema_version: LEASE_SCHEMA_VERSION,
            resource: "slot/slot04".into(),
            generation: "lease-generation-1".into(),
            owner_generation: "boot:42:100".into(),
            issued_at: 100,
            renewed_at: 100,
            ttl_seconds: 30,
            expires_at: 130,
            state,
        }
    }

    fn handoff() -> HandoffState {
        HandoffState {
            complete: true,
            digest: Some("sha256:handoff".into()),
            continuation: Some("release-worktree.rs --slot slot04 --clean".into()),
            recovery_sha: Some("0123456789abcdef0123456789abcdef01234567".into()),
            dirty: false,
            task_closed: false,
        }
    }

    fn empty() -> DomainState {
        DomainState::ProvenEmpty {
            evidence: "cgroup populated=0 and zero path users".into(),
        }
    }

    #[test]
    fn exact_live_holder_can_request_release() {
        assert_eq!(
            release_request(
                &lease(LeaseState::Active),
                "lease-generation-1",
                "boot:42:100",
                &handoff()
            ),
            LeaseDecision::ReleaseRequested
        );
    }

    #[test]
    fn foreign_or_restarted_holder_cannot_request_release() {
        let decision = release_request(
            &lease(LeaseState::Active),
            "lease-generation-1",
            "boot:42:101",
            &handoff(),
        );
        assert!(matches!(decision, LeaseDecision::Refuse { .. }));
    }

    #[test]
    fn expired_dead_empty_complete_clears() {
        assert!(matches!(
            stale_clear(
                &lease(LeaseState::Active),
                "lease-generation-1",
                131,
                OwnerLiveness::Dead {
                    evidence: "pid start_ticks absent".into()
                },
                empty(),
                &handoff(),
            ),
            LeaseDecision::Clear { .. }
        ));
    }

    #[test]
    fn live_holder_is_never_cleared_even_after_expiry() {
        let decision = stale_clear(
            &lease(LeaseState::Active),
            "lease-generation-1",
            10_000,
            OwnerLiveness::Alive,
            empty(),
            &handoff(),
        );
        assert_eq!(
            decision,
            LeaseDecision::Refuse {
                reason: "exact holder generation is alive".into()
            }
        );
    }

    #[test]
    fn unreadable_liveness_is_not_dead() {
        assert!(matches!(
            stale_clear(
                &lease(LeaseState::Draining),
                "lease-generation-1",
                110,
                OwnerLiveness::Unknown {
                    reason: "/proc unreadable".into()
                },
                empty(),
                &handoff(),
            ),
            LeaseDecision::Quarantine { .. }
        ));
    }

    #[test]
    fn changed_generation_refuses() {
        assert!(matches!(
            stale_clear(
                &lease(LeaseState::Expired),
                "replacement-generation",
                131,
                OwnerLiveness::Dead {
                    evidence: "dead".into()
                },
                empty(),
                &handoff(),
            ),
            LeaseDecision::Refuse { .. }
        ));
    }

    #[test]
    fn dead_holder_with_populated_domain_refuses() {
        assert!(matches!(
            stale_clear(
                &lease(LeaseState::Expired),
                "lease-generation-1",
                131,
                OwnerLiveness::Dead {
                    evidence: "dead".into()
                },
                DomainState::Populated {
                    evidence: "one process remains".into()
                },
                &handoff(),
            ),
            LeaseDecision::Refuse { .. }
        ));
    }

    #[test]
    fn missing_handoff_quarantines() {
        let mut missing = handoff();
        missing.complete = false;
        assert!(matches!(
            stale_clear(
                &lease(LeaseState::Expired),
                "lease-generation-1",
                131,
                OwnerLiveness::Dead {
                    evidence: "dead".into()
                },
                empty(),
                &missing,
            ),
            LeaseDecision::Quarantine { .. }
        ));
    }

    #[test]
    fn closed_task_does_not_discharge_live_dirty_ownership() {
        let mut orphaned = handoff();
        orphaned.dirty = true;
        orphaned.task_closed = true;
        orphaned.recovery_sha = None;
        let decision = release_request(
            &lease(LeaseState::Active),
            "lease-generation-1",
            "boot:42:100",
            &orphaned,
        );
        assert!(matches!(decision, LeaseDecision::Quarantine { .. }));
        assert!(decision.to_string().contains("dirty work"));
    }

    #[test]
    fn invalid_ttl_never_becomes_clearable() {
        let mut invalid = lease(LeaseState::Expired);
        invalid.ttl_seconds = 0;
        assert!(matches!(
            stale_clear(
                &invalid,
                "lease-generation-1",
                131,
                OwnerLiveness::Dead {
                    evidence: "dead".into()
                },
                empty(),
                &handoff(),
            ),
            LeaseDecision::Refuse { .. }
        ));
    }

    #[test]
    fn quarantined_state_still_requires_dead_empty_complete_evidence() {
        let mut quarantined = lease(LeaseState::Quarantined);
        quarantined.expires_at = 130;
        assert!(matches!(
            stale_clear(
                &quarantined,
                "lease-generation-1",
                131,
                OwnerLiveness::Alive,
                empty(),
                &handoff(),
            ),
            LeaseDecision::Refuse { .. }
        ));
    }
}
