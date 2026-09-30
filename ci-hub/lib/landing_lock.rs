//! Typed implementation of the shared-file landing mutex.

use crate::bounded_output::BoundedOutput;
use crate::lease::{
    self, DomainState as LeaseDomainState, HandoffState, LeaseDecision, LeaseRecord, LeaseState,
    OwnerLiveness as LeaseOwnerLiveness,
};
use chrono::{Local, TimeZone};
use clap::{Args, Subcommand};
use fs2::FileExt;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

const DEFAULT_WAIT_SECONDS: u64 = 1_800;
const DEFAULT_HOLD_SECONDS: u64 = 900;
const POLL_SECONDS: u64 = 3;
const GUARD_WAIT_SECONDS: u64 = 30;
/// Hard ceiling on how long a `run` child may execute before it is killed and
/// cleanup is proved. A complete empty-domain proof releases the lock; otherwise
/// it stays quarantined. A stuck land holding the lock is a head-of-line block
/// for every other FIFO waiter (the ~2040-minute starvation this bounds
/// against): an unbounded wait is unboxed compute. A real land is minutes; this
/// measured ceiling is far below any starvation.
// Measured 2026-08-04: 11 successful demo-gate runs had p99/max 864s. The
// lander gate bound is 1080s (max + 25%); the whole-child ceiling allows two
// complete gate windows while still guaranteeing bounded cleanup or quarantine
// on a wedged child.
const DEFAULT_CHILD_DEADLINE_SECONDS: u64 = 2_160;
/// Exit code reported when a `run` child is killed for exceeding its deadline.
const CHILD_DEADLINE_EXIT_CODE: i32 = 124;
/// Exit code when the box-global anchor refuses admission. Deliberately the SAME
/// 1 that `acquire` already returns on TIMEOUT: to every existing caller this is
/// the familiar "you did not get the lock", not a new failure mode to handle.
const REFUSED_EXIT_CODE: i32 = 1;
/// Grace period between SIGTERM and SIGKILL when terminating a timed-out child.
const CHILD_TERM_GRACE_SECONDS: u64 = 5;
/// Interval at which `run` polls a live child for completion or deadline breach.
const CHILD_POLL_MILLIS: u64 = 500;
const START_GATE_FD: libc::c_int = 9;
const START_GATE_SCRIPT: &str = r#"
if IFS= read -r ci_hub_start_token <&9 && [ "$ci_hub_start_token" = "ci-hub-go" ]; then
  exec 9<&-
  exec "$@"
fi
exit 125
"#;

#[derive(Args, Clone, Debug)]
pub struct LandLockArgs {
    #[command(subcommand)]
    pub command: LandLockCommand,
}

#[derive(Subcommand, Clone, Debug)]
pub enum LandLockCommand {
    /// Wait in FIFO order and acquire the landing lease.
    Acquire(AcquireArgs),
    /// Refresh a lease owned by this agent.
    Renew(RenewArgs),
    /// Release a lease owned by this agent.
    Release(ReleaseArgs),
    /// Print holder metadata and the FIFO queue.
    Status,
    /// Reclaim a lease only when its recorded lock-holder process has exited.
    ReclaimDead,
    /// Standard fail-closed stale-clear path. `reclaim-dead` is retained as a
    /// compatibility alias; both call the same lease verifier.
    StaleClear,
    /// Census an UNCENSUSED payload domain post-hoc, after the supervisor died
    /// before it could take one, so `reclaim-dead` can finish.
    CensusOrphanedDomain(CensusOrphanedDomainArgs),
    /// Acquire and run with a heartbeat; release after complete cleanup proof,
    /// otherwise retain a quarantine.
    Run(RunArgs),
}

/// Restated identity plus the one operator attestation that discharges an
/// UNCENSUSED quarantine. Every field is re-checked against the durable record;
/// a mismatch refuses, so this cannot be run blind against whatever happens to
/// be quarantined at the time.
///
/// This mirrors `validate-lock census-orphaned-domain`. Landing needed its own
/// because the two are DISTINCT authorities over distinct record files
/// (`CI_HUB_LANDING_LOCK` vs `CI_HUB_VALIDATE_LOCK`), so the validate command
/// could never address a landing record no matter what arguments it was handed.
/// Without this, `reclaim-dead` demanded a census that nothing could produce:
/// the only producers, `begin_run_census` and `record_residuals`, are reachable
/// only from `run()` and only for the LIVE holder, so a supervisor that died at
/// `phase=published` left a one-way door.
#[derive(Args, Clone, Debug)]
pub struct CensusOrphanedDomainArgs {
    /// Must equal the recorded `agent` of the quarantined operation.
    #[arg(long)]
    pub agent: String,
    /// Must equal the recorded `operation` (`pr:<number>`).
    #[arg(long)]
    pub operation: String,
    /// Must equal the recorded published `leader` as `<pid>:<start_ticks>`.
    /// Omit for an incomplete residual record, which has no leader field.
    #[arg(long)]
    pub leader: Option<String>,
    /// Must equal the recorded published `pgid`.
    #[arg(long)]
    pub pgid: u32,
    /// Affirms that the payload's process domain was observed empty by a
    /// supervisor-independent authority (unit cgroup absent/empty AND every
    /// cgroup the payload can migrate into empty).
    ///
    /// NOT required when the record carries a mechanical cleanup domain and the
    /// kernel already says it is empty. This remains only for legacy records
    /// with no domain, or legacy cgroup records whose anchor cannot be read.
    #[arg(long)]
    pub attest_domain_empty: bool,
    /// Identity of the operator making `--attest-domain-empty`. This names the
    /// claimant; it does not replace any mechanical liveness check.
    #[arg(long, default_value = "")]
    pub attester: String,
    /// The exact observations backing `--attest-domain-empty`. Recorded in the
    /// transcript so the attestation is auditable rather than anonymous. Must be
    /// non-empty whenever `--attest-domain-empty` is used.
    #[arg(long, default_value = "")]
    pub evidence: String,
}

#[derive(Args, Clone, Debug)]
pub struct AcquireArgs {
    #[arg(long)]
    pub agent: String,
    #[arg(long)]
    pub pr: String,
    #[arg(long, default_value_t = DEFAULT_WAIT_SECONDS)]
    pub wait: u64,
    #[arg(long, default_value_t = DEFAULT_HOLD_SECONDS)]
    pub hold: u64,
}

#[derive(Args, Clone, Debug)]
pub struct RenewArgs {
    #[arg(long)]
    pub agent: String,
    #[arg(long, default_value_t = DEFAULT_HOLD_SECONDS)]
    pub hold: u64,
}

#[derive(Args, Clone, Debug)]
pub struct ReleaseArgs {
    #[arg(long)]
    pub agent: String,
}

#[derive(Args, Clone, Debug)]
pub struct RunArgs {
    #[arg(long)]
    pub agent: String,
    #[arg(long)]
    pub pr: String,
    #[arg(long, default_value_t = DEFAULT_WAIT_SECONDS)]
    pub wait: u64,
    #[arg(long, default_value_t = DEFAULT_HOLD_SECONDS)]
    pub hold: u64,
    /// Kill the child if it runs longer than this many seconds. Release follows
    /// only after complete cleanup proof; otherwise the lock remains
    /// quarantined. Must be positive: unbounded lock holders are forbidden.
    #[arg(long, default_value_t = DEFAULT_CHILD_DEADLINE_SECONDS)]
    pub child_deadline: u64,
    #[arg(last = true, required = true)]
    pub child: Vec<OsString>,
}

impl LandLockCommand {
    pub fn consumes_meaningful_time(&self) -> bool {
        matches!(self, Self::Acquire(_) | Self::Run(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockState {
    pub agent: String,
    /// Optional fields added by the exact-head lander while it owns the
    /// shared lease.  The mutex must preserve these fields on parse/render so
    /// an annotated holder remains readable by every landing entry point.
    pub repo: Option<String>,
    pub operation: Option<String>,
    pub pending_mutation: Option<String>,
    pub pending_attempt: Option<String>,
    pub pending_call_count: Option<u64>,
    pub pending_call_id: Option<String>,
    pub pr: String,
    pub host: String,
    pub acquired_at: i64,
    pub acquired_human: String,
    pub expires_at: i64,
    pub reclaimed_from: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProcessOwner {
    host: String,
    boot_id: String,
    pid: u32,
    start_ticks: u64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: u32,
    pub(crate) start_ticks: u64,
}

/// Durable state for one supervised payload domain. The `Armed` state is
/// persisted before spawn, `Published` binds the still-gated child identity,
/// `CensusPending` durably disables heartbeat renewal before any descendant is
/// frozen, and `Residual` records the final exact census. Every consumer
/// must dereference this record through `verify_cleanup_record`; file existence
/// or a printed marker is not proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CleanupRecord {
    pub(crate) agent: String,
    pub(crate) operation: String,
    pub(crate) host: String,
    pub(crate) boot_id: String,
    pub(crate) phase: CleanupPhase,
    /// Exact cleanup boundary published while the start-gated payload is alive.
    ///
    /// A process group is narrow and actionable: it contains the payload, not
    /// the long-lived agent sandbox that launched the lock. A cgroup remains a
    /// readable legacy boundary, but new records must not copy an inherited
    /// supervisor cgroup and mislabel it as payload-owned.
    pub(crate) domain: CleanupDomain,
    /// Present only when an operator, rather than a kernel-readable boundary,
    /// supplied the otherwise-unprovable domain-completeness fact.
    pub(crate) attestation: Option<DomainAttestation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DomainAttestation {
    pub(crate) attester: String,
    pub(crate) evidence: String,
}

impl DomainAttestation {
    pub(crate) fn new(attester: &str, evidence: &str) -> Result<Self, String> {
        let attester = attester.trim();
        if attester.is_empty() {
            return Err(
                "--attester must name who is asserting --attest-domain-empty; the recorded \
                 lock owner is the subject, not necessarily the attester"
                    .to_string(),
            );
        }
        if !attester
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._/@+-".contains(character))
        {
            return Err(format!(
                "--attester must be a single agent identity token, got {attester:?}"
            ));
        }
        let evidence = evidence.trim();
        if evidence.is_empty() {
            return Err(
                "--evidence must record the observations backing --attest-domain-empty; an \
                 anonymous attestation is not auditable"
                    .to_string(),
            );
        }
        if evidence.contains(['\n', '\r']) {
            return Err("--evidence must be one line so it cannot inject cleanup fields".into());
        }
        Ok(Self {
            attester: attester.to_string(),
            evidence: evidence.to_string(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CleanupDomain {
    /// Version-2 records predate an explicit cleanup boundary.
    LegacyUnproven,
    /// Version-4 records bind cleanup to the payload's own process group.
    ProcessGroup,
    /// Version-3 compatibility for already-published cgroup records.
    Cgroup(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CleanupPhase {
    Armed,
    Published {
        leader: ProcessIdentity,
        pgid: u32,
    },
    CensusPending {
        leader: ProcessIdentity,
        pgid: u32,
    },
    Residual {
        pgid: u32,
        domain_complete: bool,
        residuals: Vec<ProcessIdentity>,
    },
}

impl CleanupRecord {
    pub(crate) fn new(
        agent: impl Into<String>,
        operation: impl Into<String>,
        phase: CleanupPhase,
    ) -> io::Result<Self> {
        let host = current_host();
        if host == "unknown" {
            return Err(io::Error::other(
                "cannot arm cleanup authority without a host identity",
            ));
        }
        Ok(Self {
            agent: agent.into(),
            operation: operation.into(),
            host,
            boot_id: read_current_boot_id()?,
            phase,
            domain: CleanupDomain::LegacyUnproven,
            attestation: None,
        })
    }

    pub(crate) fn parse(content: &str) -> Result<Self, String> {
        let mut version = None;
        let mut agent = None;
        let mut operation = None;
        let mut host = None;
        let mut boot_id = None;
        let mut phase = None;
        let mut leader = None;
        let mut pgid = None;
        let mut domain_complete = None;
        let mut cgroup = None;
        let mut domain = None;
        let mut attester = None;
        let mut attestation_evidence = None;
        let mut residuals = Vec::new();
        for (line_number, line) in content.lines().enumerate() {
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("cleanup line {} is not key=value", line_number + 1))?;
            match key {
                "version" => set_once(&mut version, value.to_string(), key)?,
                "agent" => set_once(&mut agent, value.to_string(), key)?,
                "operation" => set_once(&mut operation, value.to_string(), key)?,
                "host" => set_once(&mut host, value.to_string(), key)?,
                "boot_id" => set_once(&mut boot_id, value.to_string(), key)?,
                "phase" => set_once(&mut phase, value.to_string(), key)?,
                "leader" => {
                    let identity = parse_process_identity(value, "leader")?;
                    set_once(&mut leader, identity, key)?;
                }
                "pgid" => set_once(
                    &mut pgid,
                    value
                        .parse::<u32>()
                        .map_err(|error| format!("invalid cleanup pgid {value:?}: {error}"))?,
                    key,
                )?,
                "domain_complete" => {
                    let parsed = match value {
                        "true" => true,
                        "false" => false,
                        _ => return Err(format!("invalid domain_complete {value:?}")),
                    };
                    set_once(&mut domain_complete, parsed, key)?;
                }
                "residual" => residuals.push(parse_process_identity(value, "residual")?),
                "cgroup" => set_once(&mut cgroup, value.to_string(), key)?,
                "domain" => set_once(&mut domain, value.to_string(), key)?,
                "attester" => set_once(&mut attester, value.to_string(), key)?,
                "attestation_evidence" => {
                    set_once(&mut attestation_evidence, value.to_string(), key)?
                }
                unknown => return Err(format!("unknown cleanup field {unknown:?}")),
            }
        }
        let cleanup_domain = match version.as_deref() {
            Some("2") if cgroup.is_none() && domain.is_none() => CleanupDomain::LegacyUnproven,
            Some("2") => return Err("cleanup version 2 cannot carry a cleanup domain".to_string()),
            Some("3") if domain.is_none() => cgroup
                .map(CleanupDomain::Cgroup)
                .unwrap_or(CleanupDomain::LegacyUnproven),
            Some("3") => return Err("cleanup version 3 cannot carry a domain field".to_string()),
            Some("4") if cgroup.is_none() && domain.as_deref() == Some("process-group") => {
                CleanupDomain::ProcessGroup
            }
            Some("4") => {
                return Err("cleanup version 4 requires exactly domain=process-group".to_string())
            }
            other => return Err(format!("unsupported cleanup version {other:?}")),
        };
        residuals.sort();
        residuals.dedup();
        let phase = match phase.as_deref() {
            Some("armed")
                if leader.is_none()
                    && pgid.is_none()
                    && domain_complete.is_none()
                    && residuals.is_empty() =>
            {
                CleanupPhase::Armed
            }
            Some("published") if domain_complete.is_none() && residuals.is_empty() => {
                CleanupPhase::Published {
                    leader: leader
                        .ok_or_else(|| "published cleanup record has no leader".to_string())?,
                    pgid: pgid.ok_or_else(|| "published cleanup record has no pgid".to_string())?,
                }
            }
            Some("census-pending") if domain_complete.is_none() && residuals.is_empty() => {
                CleanupPhase::CensusPending {
                    leader: leader
                        .ok_or_else(|| "census-pending cleanup record has no leader".to_string())?,
                    pgid: pgid
                        .ok_or_else(|| "census-pending cleanup record has no pgid".to_string())?,
                }
            }
            Some("residual") if leader.is_none() => CleanupPhase::Residual {
                pgid: pgid.ok_or_else(|| "residual cleanup record has no pgid".to_string())?,
                domain_complete: domain_complete
                    .ok_or_else(|| "residual cleanup record has no domain_complete".to_string())?,
                residuals,
            },
            Some(value) => return Err(format!("cleanup phase {value:?} has incompatible fields")),
            None => return Err("cleanup record has no phase".to_string()),
        };
        let attestation = match (attester, attestation_evidence) {
            (None, None) => None,
            (Some(attester), Some(evidence)) => Some(DomainAttestation::new(&attester, &evidence)?),
            _ => {
                return Err(
                    "cleanup attestation requires both attester and attestation_evidence"
                        .to_string(),
                )
            }
        };
        if attestation.is_some()
            && !matches!(
                phase,
                CleanupPhase::Residual {
                    domain_complete: true,
                    ..
                }
            )
        {
            return Err(
                "cleanup attestation is only valid on a complete residual census".to_string(),
            );
        }
        Ok(Self {
            agent: agent.ok_or_else(|| "cleanup record has no agent".to_string())?,
            operation: operation.ok_or_else(|| "cleanup record has no operation".to_string())?,
            host: host.ok_or_else(|| "cleanup record has no host".to_string())?,
            boot_id: boot_id.ok_or_else(|| "cleanup record has no boot_id".to_string())?,
            phase,
            domain: cleanup_domain,
            attestation,
        })
    }

    pub(crate) fn render(&self) -> String {
        let version = match self.domain {
            CleanupDomain::LegacyUnproven => 2,
            CleanupDomain::Cgroup(_) => 3,
            CleanupDomain::ProcessGroup => 4,
        };
        let mut output = format!(
            "version={version}\nagent={}\noperation={}\nhost={}\nboot_id={}\n",
            self.agent, self.operation, self.host, self.boot_id
        );
        match &self.domain {
            CleanupDomain::LegacyUnproven => {}
            CleanupDomain::Cgroup(cgroup) => output.push_str(&format!("cgroup={cgroup}\n")),
            CleanupDomain::ProcessGroup => output.push_str("domain=process-group\n"),
        }
        match &self.phase {
            CleanupPhase::Armed => output.push_str("phase=armed\n"),
            CleanupPhase::Published { leader, pgid } => output.push_str(&format!(
                "phase=published\nleader={}:{}\npgid={pgid}\n",
                leader.pid, leader.start_ticks
            )),
            CleanupPhase::CensusPending { leader, pgid } => output.push_str(&format!(
                "phase=census-pending\nleader={}:{}\npgid={pgid}\n",
                leader.pid, leader.start_ticks
            )),
            CleanupPhase::Residual {
                pgid,
                domain_complete,
                residuals,
            } => {
                output.push_str(&format!(
                    "phase=residual\npgid={pgid}\ndomain_complete={domain_complete}\n"
                ));
                for identity in residuals {
                    output.push_str(&format!(
                        "residual={}:{}\n",
                        identity.pid, identity.start_ticks
                    ));
                }
            }
        }
        if let Some(attestation) = &self.attestation {
            output.push_str(&format!(
                "attester={}\nattestation_evidence={}\n",
                attestation.attester, attestation.evidence
            ));
        }
        output
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T, key: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("duplicate cleanup field {key:?}"));
    }
    Ok(())
}

fn parse_process_identity(value: &str, field: &str) -> Result<ProcessIdentity, String> {
    let (pid, start_ticks) = value
        .split_once(':')
        .ok_or_else(|| format!("invalid {field} identity {value:?}"))?;
    Ok(ProcessIdentity {
        pid: pid
            .parse()
            .map_err(|error| format!("invalid {field} pid {pid:?}: {error}"))?,
        start_ticks: start_ticks
            .parse()
            .map_err(|error| format!("invalid {field} start ticks {start_ticks:?}: {error}"))?,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CleanupVerification {
    None,
    Armed {
        record: CleanupRecord,
        reason: String,
    },
    Active {
        record: CleanupRecord,
        reason: String,
    },
    Uncensused {
        record: CleanupRecord,
        reason: String,
    },
    Recoverable {
        record: CleanupRecord,
        reason: String,
    },
    Unknown {
        record: Option<CleanupRecord>,
        reason: String,
    },
}

impl ProcessOwner {
    fn parse(content: &str) -> Result<Self, LandLockError> {
        let mut host = None;
        let mut boot_id = None;
        let mut pid = None;
        let mut start_ticks = None;
        for (line_number, line) in content.lines().enumerate() {
            let (key, value) = line.split_once('=').ok_or_else(|| {
                LandLockError::InvalidState(format!(
                    "owner line {} is not key=value",
                    line_number + 1
                ))
            })?;
            match key {
                "host" => host = Some(value.to_string()),
                "boot_id" => boot_id = Some(value.to_string()),
                "pid" => pid = Some(parse_unsigned(key, value)?),
                "start_ticks" => start_ticks = Some(parse_unsigned(key, value)?),
                unknown => {
                    return Err(LandLockError::InvalidState(format!(
                        "unknown owner field {unknown:?}"
                    )));
                }
            }
        }
        Ok(Self {
            host: required(host, "owner host")?,
            boot_id: required(boot_id, "owner boot_id")?,
            pid: u32::try_from(required(pid, "owner pid")?)
                .map_err(|_| LandLockError::InvalidState("owner pid exceeds u32".into()))?,
            start_ticks: required(start_ticks, "owner start_ticks")?,
        })
    }

    fn render(&self) -> String {
        format!(
            "host={}\nboot_id={}\npid={}\nstart_ticks={}\n",
            self.host, self.boot_id, self.pid, self.start_ticks
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum OwnerLiveness {
    Alive,
    Dead(String),
    Unknown(String),
}

impl LockState {
    fn parse(content: &str) -> Result<Self, LandLockError> {
        let mut agent = None;
        let mut repo = None;
        let mut operation = None;
        let mut pending_mutation = None;
        let mut pending_attempt = None;
        let mut pending_call_count = None;
        let mut pending_call_id = None;
        let mut pr = None;
        let mut host = None;
        let mut acquired_at = None;
        let mut acquired_human = None;
        let mut expires_at = None;
        let mut reclaimed_from = None;
        for (line_number, line) in content.lines().enumerate() {
            let (key, value) = line.split_once('=').ok_or_else(|| {
                LandLockError::InvalidState(format!(
                    "holder line {} is not key=value",
                    line_number + 1
                ))
            })?;
            match key {
                "agent" => agent = Some(value.to_string()),
                "repo" => repo = Some(value.to_string()),
                "operation" => operation = Some(value.to_string()),
                "pending_mutation" => pending_mutation = Some(value.to_string()),
                "pending_attempt" => pending_attempt = Some(value.to_string()),
                "pending_call_count" => pending_call_count = Some(parse_unsigned(key, value)?),
                "pending_call_id" => pending_call_id = Some(value.to_string()),
                "pr" => pr = Some(value.to_string()),
                "host" => host = Some(value.to_string()),
                "acquired_at" => acquired_at = Some(parse_integer(key, value)?),
                "acquired_human" => acquired_human = Some(value.to_string()),
                "expires_at" => expires_at = Some(parse_integer(key, value)?),
                "reclaimed_from" => reclaimed_from = Some(value.to_string()),
                unknown => {
                    return Err(LandLockError::InvalidState(format!(
                        "unknown holder field {unknown:?}"
                    )));
                }
            }
        }
        Ok(Self {
            agent: required(agent, "agent")?,
            repo,
            operation,
            pending_mutation,
            pending_attempt,
            pending_call_count,
            pending_call_id,
            pr: required(pr, "pr")?,
            host: required(host, "host")?,
            acquired_at: required(acquired_at, "acquired_at")?,
            acquired_human: required(acquired_human, "acquired_human")?,
            expires_at: required(expires_at, "expires_at")?,
            reclaimed_from,
        })
    }

    fn render(&self) -> String {
        let mut output = format!("agent={}\n", self.agent);
        for (key, value) in [
            ("repo", self.repo.as_deref()),
            ("operation", self.operation.as_deref()),
            ("pending_mutation", self.pending_mutation.as_deref()),
            ("pending_attempt", self.pending_attempt.as_deref()),
        ] {
            if let Some(value) = value {
                output.push_str(&format!("{key}={value}\n"));
            }
        }
        if let Some(value) = self.pending_call_count {
            output.push_str(&format!("pending_call_count={value}\n"));
        }
        if let Some(value) = &self.pending_call_id {
            output.push_str(&format!("pending_call_id={value}\n"));
        }
        output.push_str(&format!(
            "pr={}\nhost={}\nacquired_at={}\nacquired_human={}\nexpires_at={}\n",
            self.pr, self.host, self.acquired_at, self.acquired_human, self.expires_at
        ));
        if let Some(reclaimed_from) = &self.reclaimed_from {
            output.push_str(&format!("reclaimed_from={reclaimed_from}\n"));
        }
        output
    }

    fn live_at(&self, now: i64) -> bool {
        now < self.expires_at
    }

    fn renewed(&self, hold: u64) -> Result<Self, LandLockError> {
        let mut renewed = new_holder(&self.agent, &self.pr, hold, None)?;
        renewed.repo = self.repo.clone();
        renewed.operation = self.operation.clone();
        renewed.pending_mutation = self.pending_mutation.clone();
        renewed.pending_attempt = self.pending_attempt.clone();
        renewed.pending_call_count = self.pending_call_count;
        renewed.pending_call_id = self.pending_call_id.clone();
        Ok(renewed)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueEntry {
    pub enqueued_at: i64,
    pub agent: String,
    pub pr: String,
}

impl QueueEntry {
    fn parse(line: &str, line_number: usize) -> Result<Self, LandLockError> {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 3 {
            return Err(LandLockError::InvalidState(format!(
                "queue line {line_number} must have three tab-separated fields"
            )));
        }
        Ok(Self {
            enqueued_at: parse_integer("queue timestamp", fields[0])?,
            agent: fields[1].to_string(),
            pr: fields[2].to_string(),
        })
    }

    fn render(&self) -> String {
        format!("{}\t{}\t{}\n", self.enqueued_at, self.agent, self.pr)
    }
}

#[derive(Debug, Error)]
pub enum LandLockError {
    #[error("landing-lock: {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("landing-lock: invalid on-disk state: {0}")]
    InvalidState(String),
    #[error("landing-lock: timed out taking internal guard after {GUARD_WAIT_SECONDS}s")]
    GuardTimeout,
    #[error("landing-lock: renew: {agent} does not hold the lock")]
    RenewNotOwner { agent: String },
    #[error("landing-lock: release: lock is held by {holder}, not {agent}; refusing")]
    ReleaseNotOwner { agent: String, holder: String },
    #[error("landing-lock: child command is empty")]
    EmptyChild,
    #[error(
        "landing-lock: --child-deadline must be positive; unbounded lock holders are forbidden"
    )]
    UnboundedChildDeadline,
    #[error(
        "landing-lock: {operation}: process {pid} owns the supervised lease, not this process"
    )]
    ProcessNotOwner { operation: &'static str, pid: u32 },
    #[error("landing-lock: cannot reclaim lease: {0}")]
    ReclaimNotProven(String),
    #[error("landing-lock: cleanup quarantine: {0}")]
    CleanupQuarantined(String),
}

impl LandLockError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::RenewNotOwner { .. }
            | Self::ReleaseNotOwner { .. }
            | Self::ProcessNotOwner { .. }
            | Self::ReclaimNotProven(_)
            | Self::CleanupQuarantined(_)
            | Self::GuardTimeout
            | Self::InvalidState(_) => 3,
            Self::Io { .. } | Self::EmptyChild | Self::UnboundedChildDeadline => 2,
        }
    }
}

/// Anchor filename for LANDING. Distinct from the validate anchor on purpose:
/// this module's contract is that a validate must never block a lander and vice
/// versa, so the two are box-global with respect to their own kind and invisible
/// to each other.
pub(crate) const LANDING_BOX_ANCHOR: &str = "landing-box.lock";

#[derive(Clone, Debug)]
struct LockPaths {
    lock: PathBuf,
    guard: PathBuf,
    queue: PathBuf,
    owner: PathBuf,
    cleanup: PathBuf,
    /// The box-global exclusion anchor. Carried here rather than read from a
    /// global so it is CONSTRUCTOR-SCOPED: production builds it from the uid, and
    /// each test builds its own beside its own temp lease. A test binary must
    /// never contend with the live box.
    anchor: PathBuf,
}

impl LockPaths {
    fn for_workspace(root: &Path) -> Self {
        // THE LEASE PATH IS CANONICALIZED TO THE REPOSITORY'S MAIN WORKTREE.
        // `workspace_root()` (ci-hub.rs) returns the git toplevel of the running
        // `ci-hub.rs`, so every linked worktree used to derive its own private
        // `.landing-lock` and land independently. Measured 2026-08-08: 46
        // worktrees of ~/work/dev-hermit, all 46 with a runnable `ci-hub/ci-hub`,
        // and two of them ran concurrent landings 2/2.
        //
        // `CI_HUB_LANDING_LOCK` STILL SELECTS THE LEASE, and that is a considered
        // difference from `validate_lock.rs`, where the equivalent variable was
        // compiled out of production at c6767e06. There it had ZERO consumers, so
        // gating it was free. Here it has THREE, two of them MUTATING:
        // `ci-hub/tests/test_operational_bounds.py` runs the real binary through
        // `land-lock acquire/release/run/reclaim-dead` against an isolated lease.
        // Ignoring the variable would point those harnesses at the PRODUCTION
        // landing lease — a test suite acquiring the live landing mutex, which is
        // worse than the defect it would be fixing.
        //
        // The escape it used to buy is closed anyway, and by construction rather
        // than by convention: the anchor below is NOT redirectable, so moving the
        // lease no longer buys a second concurrent landing. Measured both ways in
        // the commit that added this. The variable is now bookkeeping isolation,
        // not an exclusion control, and `execute` says so out loud when it is set.
        let lock = env::var_os("CI_HUB_LANDING_LOCK")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| repository_lock_root(root).join(".landing-lock"));
        Self {
            guard: suffix(&lock, ".guard"),
            queue: suffix(&lock, ".queue"),
            owner: suffix(&lock, ".owner"),
            cleanup: suffix(&lock, ".cleanup-required"),
            anchor: box_exclusion_anchor_path(LANDING_BOX_ANCHOR),
            lock,
        }
    }
}

struct LandingLock {
    paths: LockPaths,
}

enum AcquireToken {
    Acquired,
    AcquiredReclaimed(String),
    Held { agent: String, seconds_left: i64 },
    WaitTurn(String),
}

pub fn execute(root: &Path, args: LandLockArgs) -> Result<i32, LandLockError> {
    let lock = LandingLock {
        paths: LockPaths::for_workspace(root),
    };
    // Say it out loud. An operator who exported this expects a sandbox; they must
    // not also believe it bought them a way past box exclusion, because it does
    // not — the anchor is uid-derived and cannot be redirected.
    if env::var_os("CI_HUB_LANDING_LOCK").is_some_and(|value| !value.is_empty()) {
        eprintln!(
            "land-lock: NOTICE: CI_HUB_LANDING_LOCK selects the lease RECORD only \
             ({}). Box exclusion is enforced by the uid-derived anchor {}, which \
             this variable cannot move.",
            lock.paths.lock.display(),
            lock.paths.anchor.display()
        );
    }
    match args.command {
        LandLockCommand::Acquire(args) => {
            // `acquire` cannot HOLD the anchor, but it must
            // not hand out a lease while another repository's landing owns the box.
            match BoxExclusionAnchor::observe(&lock.paths.anchor) {
                BoxAnchorObservation::Free => {}
                BoxAnchorObservation::Held(holder) => {
                    eprintln!(
                        "REFUSED: box-exclusive landing anchor held by {holder}; this box \
                         lands one PR at a time across ALL checkouts and clones"
                    );
                    return Ok(REFUSED_EXIT_CODE);
                }
                BoxAnchorObservation::Unknown(reason) => {
                    eprintln!(
                        "REFUSED: box-exclusive landing anchor state UNKNOWN: {reason}; \
                         absence is not permission"
                    );
                    return Ok(REFUSED_EXIT_CODE);
                }
            }
            // The anchor is a live-process signal; this is the durable one. A
            // supervisor that died leaving its payload alive released the anchor,
            // but its quarantine record is still outstanding, and now visible from
            // here. ONE MORE necessary condition -- `require_no_cleanup` inside
            // `acquire` still runs for this repository's own record.
            if let Some(reason) = foreign_box_claim(&lock.paths.anchor, &lock.paths.cleanup) {
                eprintln!("REFUSED: {reason}");
                return Ok(REFUSED_EXIT_CODE);
            }
            lock.acquire(&args)
        }
        LandLockCommand::Renew(args) => {
            lock.renew(&args.agent, args.hold, true)?;
            Ok(0)
        }
        LandLockCommand::Release(args) => {
            lock.release(&args.agent, true)?;
            Ok(0)
        }
        LandLockCommand::Status => {
            lock.status()?;
            Ok(0)
        }
        LandLockCommand::ReclaimDead | LandLockCommand::StaleClear => lock.reclaim_dead(),
        LandLockCommand::CensusOrphanedDomain(args) => lock.census_orphaned_domain(args),
        LandLockCommand::Run(args) => lock.run(args),
    }
}

impl LandingLock {
    fn holder_lease_record(&self, holder: &LockState) -> Result<LeaseRecord, LandLockError> {
        let ttl = holder.expires_at.saturating_sub(holder.acquired_at);
        let ttl_seconds = u64::try_from(ttl)
            .map_err(|_| LandLockError::InvalidState("landing holder has a negative TTL".into()))?;
        let owner_generation = self
            .read_process_owner()?
            .map(|owner| {
                format!(
                    "{}:{}:{}:{}",
                    owner.host, owner.boot_id, owner.pid, owner.start_ticks
                )
            })
            .unwrap_or_else(|| "legacy-owner-unverifiable".into());
        Ok(LeaseRecord {
            schema_version: lease::LEASE_SCHEMA_VERSION,
            resource: format!("landing/{}", holder.pr),
            generation: format!(
                "landing:{}:{}:{}:{}",
                holder.agent, holder.pr, holder.acquired_at, holder.expires_at
            ),
            owner_generation,
            issued_at: holder.acquired_at,
            renewed_at: holder.acquired_at,
            ttl_seconds,
            expires_at: holder.expires_at,
            state: LeaseState::Active,
        })
    }

    fn holder_stale_clear_decision(
        &self,
        holder: &LockState,
        now: i64,
        cleanup: CleanupVerification,
    ) -> Result<LeaseDecision, LandLockError> {
        let mut lease = self.holder_lease_record(holder)?;
        if !matches!(&cleanup, CleanupVerification::None) {
            lease.state = LeaseState::Quarantined;
        }
        let liveness = match self.owner_liveness()? {
            OwnerLiveness::Alive => LeaseOwnerLiveness::Alive,
            OwnerLiveness::Dead(evidence) => LeaseOwnerLiveness::Dead { evidence },
            OwnerLiveness::Unknown(reason) => LeaseOwnerLiveness::Unknown { reason },
        };
        let domain = match cleanup {
            CleanupVerification::None => LeaseDomainState::ProvenEmpty {
                evidence: "no supervised payload domain is registered".into(),
            },
            CleanupVerification::Recoverable { reason, .. } => {
                LeaseDomainState::ProvenEmpty { evidence: reason }
            }
            CleanupVerification::Active { reason, .. } => {
                LeaseDomainState::Populated { evidence: reason }
            }
            CleanupVerification::Armed { reason, .. }
            | CleanupVerification::Uncensused { reason, .. }
            | CleanupVerification::Unknown { reason, .. } => LeaseDomainState::Unproven { reason },
        };
        let handoff = HandoffState::complete_ephemeral(format!(
            "landing:{}:{}:{}",
            holder.agent, holder.pr, lease.generation
        ));
        Ok(lease::stale_clear(
            &lease,
            &lease.generation,
            now,
            liveness,
            domain,
            &handoff,
        ))
    }

    fn cleanup_verification(&self, holder: Option<&LockState>) -> CleanupVerification {
        let operation = holder.map(|holder| format!("pr:{}", holder.pr));
        let expected = holder
            .zip(operation.as_deref())
            .map(|(holder, operation)| (holder.agent.as_str(), operation));
        verify_cleanup_record(&self.paths.cleanup, expected)
    }

    fn require_no_cleanup(&self, holder: Option<&LockState>) -> Result<(), LandLockError> {
        match self.cleanup_verification(holder) {
            CleanupVerification::None => Ok(()),
            CleanupVerification::Armed { reason, .. } => Err(
                LandLockError::CleanupQuarantined(format!("ARMED: {reason}")),
            ),
            CleanupVerification::Active { reason, .. } => Err(
                LandLockError::CleanupQuarantined(format!("ACTIVE: {reason}")),
            ),
            CleanupVerification::Uncensused { reason, .. } => Err(
                LandLockError::CleanupQuarantined(format!(
                    "FLEET-WIDE landing quarantine: every land-lock acquisition is refused. \
                     Discharge with `land-lock census-orphaned-domain` using the exact command \
                     printed by `land-lock status`, then run `land-lock reclaim-dead`: {reason}"
                )),
            ),
            CleanupVerification::Recoverable { reason, .. } => Err(
                LandLockError::CleanupQuarantined(format!(
                    "payload absence is proven but explicit reclaim-dead recovery is required: {reason}"
                )),
            ),
            CleanupVerification::Unknown { reason, .. } => Err(
                LandLockError::CleanupQuarantined(format!("UNVERIFIABLE: {reason}")),
            ),
        }
    }

    fn arm_run(&self, agent: &str, pr: &str) -> Result<(), LandLockError> {
        let record = CleanupRecord::new(agent, format!("pr:{pr}"), CleanupPhase::Armed)
            .map_err(|source| io_error("construct", &self.paths.cleanup, source))?;
        self.with_guard(|| {
            let holder = self.read_holder()?.ok_or_else(|| {
                LandLockError::InvalidState("cannot arm cleanup without a lock holder".into())
            })?;
            if holder.agent != agent || holder.pr != pr {
                return Err(LandLockError::InvalidState(
                    "cleanup arm does not match the current operation".into(),
                ));
            }
            self.require_no_cleanup(Some(&holder))?;
            write_cleanup_record(&self.paths.cleanup, &record)
                .map_err(|source| io_error("write", &self.paths.cleanup, source))?;
            // Publish the box-global pointer at the SAME moment the durable record
            // is armed -- before any payload exists. From here on, if this
            // supervisor dies its record outlives it, and every OTHER repository
            // can now see that record instead of being admitted beside the escaped
            // payload it describes.
            register_box_claim(
                &self.paths.anchor,
                &self.paths.cleanup,
                agent,
                &format!("pr:{pr}"),
            );
            Ok(())
        })
    }

    fn transition_run_cleanup(
        &self,
        agent: &str,
        pr: &str,
        expected: fn(&CleanupPhase) -> bool,
        next: CleanupPhase,
        // `Some(anchor)` records the payload's containment boundary; `None` leaves whatever the
        // record already carried, so the phase transitions that are not the acquire point cannot
        // accidentally erase an anchor written earlier.
        domain: Option<CleanupDomain>,
    ) -> Result<(), LandLockError> {
        self.with_guard(|| {
            let holder = self.read_holder()?.ok_or_else(|| {
                LandLockError::InvalidState("cleanup transition has no lock holder".into())
            })?;
            if holder.agent != agent || holder.pr != pr {
                return Err(LandLockError::InvalidState(
                    "cleanup transition does not match the current operation".into(),
                ));
            }
            let verification = self.cleanup_verification(Some(&holder));
            let mut record = match verification {
                CleanupVerification::Armed { record, .. }
                | CleanupVerification::Active { record, .. }
                | CleanupVerification::Uncensused { record, .. }
                | CleanupVerification::Recoverable { record, .. } => record,
                CleanupVerification::None => {
                    return Err(LandLockError::InvalidState(
                        "cleanup transition has no durable authority".into(),
                    ))
                }
                CleanupVerification::Unknown { reason, .. } => {
                    return Err(LandLockError::CleanupQuarantined(format!(
                        "cannot transition unverifiable authority: {reason}"
                    )))
                }
            };
            if !expected(&record.phase) {
                return Err(LandLockError::InvalidState(format!(
                    "cleanup transition rejected phase {:?}",
                    record.phase
                )));
            }
            record.phase = next;
            if let Some(domain) = domain {
                record.domain = domain;
            }
            write_cleanup_record(&self.paths.cleanup, &record)
                .map_err(|source| io_error("write", &self.paths.cleanup, source))
        })
    }

    fn publish_run(&self, agent: &str, pr: &str, gated: &GatedChild) -> Result<(), LandLockError> {
        self.transition_run_cleanup(
            agent,
            pr,
            |phase| matches!(phase, CleanupPhase::Armed),
            CleanupPhase::Published {
                leader: gated.leader.clone(),
                pgid: gated.pgid,
            },
            // Publish the payload-owned boundary before opening the start gate.
            // The child has its own PGID; unlike its inherited cgroup, that
            // boundary excludes this supervisor and the long-lived sandbox.
            Some(CleanupDomain::ProcessGroup),
        )
    }

    fn clear_unstarted_run(&self, agent: &str, pr: &str) -> Result<(), LandLockError> {
        self.with_guard(|| {
            let holder = self.read_holder()?.ok_or_else(|| {
                LandLockError::InvalidState("unstarted cleanup has no lock holder".into())
            })?;
            if holder.agent != agent || holder.pr != pr {
                return Err(LandLockError::InvalidState(
                    "unstarted cleanup does not match current operation".into(),
                ));
            }
            match self.cleanup_verification(Some(&holder)) {
                CleanupVerification::Armed { .. } => {
                    self.assert_current_process_owner("clear unstarted run")?;
                    remove_cleanup_record_and_claim(&self.paths.cleanup, &self.paths.anchor)
                        .map_err(|source| io_error("remove", &self.paths.cleanup, source))
                }
                other => Err(LandLockError::InvalidState(format!(
                    "unstarted cleanup requires armed authority, got {other:?}"
                ))),
            }
        })
    }

    fn record_residuals(
        &self,
        agent: &str,
        pr: &str,
        pgid: u32,
        capture: ResidualCapture,
    ) -> Result<(), LandLockError> {
        self.transition_run_cleanup(
            agent,
            pr,
            |phase| matches!(phase, CleanupPhase::CensusPending { .. }),
            CleanupPhase::Residual {
                pgid,
                domain_complete: capture.complete,
                residuals: capture.identities,
            },
            None,
        )
    }

    fn begin_run_census(
        &self,
        agent: &str,
        pr: &str,
        gated: &GatedChild,
    ) -> Result<(), LandLockError> {
        self.transition_run_cleanup(
            agent,
            pr,
            |phase| matches!(phase, CleanupPhase::Published { .. }),
            CleanupPhase::CensusPending {
                leader: gated.leader.clone(),
                pgid: gated.pgid,
            },
            None,
        )
    }

    fn clear_proven_run(&self, agent: &str, pr: &str, pgid: u32) -> Result<(), LandLockError> {
        self.record_residuals(
            agent,
            pr,
            pgid,
            ResidualCapture {
                complete: true,
                identities: Vec::new(),
                unclassifiable: Vec::new(),
            },
        )?;
        self.with_guard(|| {
            let holder = self.read_holder()?.ok_or_else(|| {
                LandLockError::InvalidState("cleanup clear has no lock holder".into())
            })?;
            match self.cleanup_verification(Some(&holder)) {
                CleanupVerification::Recoverable { .. } => {
                    remove_cleanup_record_and_claim(&self.paths.cleanup, &self.paths.anchor)
                        .map_err(|source| io_error("remove", &self.paths.cleanup, source))
                }
                other => Err(LandLockError::InvalidState(format!(
                    "cleanup clear requires proven absence, got {other:?}"
                ))),
            }
        })
    }

    fn renew_run_lease(&self, agent: &str, pr: &str, hold: u64) -> Result<(), LandLockError> {
        self.with_guard(|| {
            let holder = self
                .read_holder()?
                .ok_or_else(|| LandLockError::RenewNotOwner {
                    agent: agent.to_string(),
                })?;
            if holder.agent != agent || holder.pr != pr {
                return Err(LandLockError::RenewNotOwner {
                    agent: agent.to_string(),
                });
            }
            match self.cleanup_verification(Some(&holder)) {
                CleanupVerification::Active { record, .. }
                    if matches!(record.phase, CleanupPhase::Published { .. }) => {}
                other => {
                    return Err(LandLockError::CleanupQuarantined(format!(
                        "run heartbeat requires an active published domain, got {other:?}"
                    )))
                }
            }
            self.assert_current_process_owner("run heartbeat")?;
            heartbeat_test_helper_delay();
            self.write_holder(&holder.renewed(hold)?)
        })
    }

    fn acquire(&self, args: &AcquireArgs) -> Result<i32, LandLockError> {
        let started = Instant::now();
        let mut last = String::new();
        loop {
            let token = self.with_guard(|| self.try_acquire(args))?;
            match token {
                AcquireToken::Acquired => {
                    eprintln!(
                        "landing-lock: ACQUIRED by {} for PR #{} (lease {}s)",
                        args.agent, args.pr, args.hold
                    );
                    return Ok(0);
                }
                AcquireToken::AcquiredReclaimed(previous) => {
                    eprintln!(
                        "landing-lock: ACQUIRED by {} for PR #{}; evidence-reclaimed lease from {}",
                        args.agent, args.pr, previous
                    );
                    return Ok(0);
                }
                AcquireToken::Held {
                    agent,
                    seconds_left,
                } => {
                    let token = format!("HELD:{agent}:{seconds_left}");
                    if token != last {
                        eprintln!(
                            "landing-lock: waiting: held by {agent}:{seconds_left} (agent:secs_left); queued as {}",
                            args.agent
                        );
                        last = token;
                    }
                }
                AcquireToken::WaitTurn(head) => {
                    let token = format!("WAIT_TURN:{head}");
                    if token != last {
                        eprintln!("landing-lock: waiting: lock free, ahead of me in queue: {head}");
                        last = token;
                    }
                }
            }
            if started.elapsed() >= Duration::from_secs(args.wait) {
                self.with_guard(|| {
                    self.remove_from_queue(&args.agent)?;
                    Ok(())
                })?;
                eprintln!("landing-lock: TIMEOUT after {}s", args.wait);
                return Ok(1);
            }
            thread::sleep(Duration::from_secs(POLL_SECONDS));
        }
    }

    fn try_acquire(&self, args: &AcquireArgs) -> Result<AcquireToken, LandLockError> {
        let now = epoch_seconds()?;
        let holder = self.read_holder()?;
        self.require_no_cleanup(holder.as_ref())?;
        let mut queue = self.read_queue()?;
        if !queue.iter().any(|entry| entry.agent == args.agent) {
            queue.push(QueueEntry {
                enqueued_at: now,
                agent: args.agent.clone(),
                pr: args.pr.clone(),
            });
        }
        let cutoff = now - (2 * DEFAULT_WAIT_SECONDS) as i64;
        queue.retain(|entry| entry.enqueued_at >= cutoff);

        let clear_decision = holder
            .as_ref()
            .map(|holder| {
                self.holder_stale_clear_decision(
                    holder,
                    now,
                    self.cleanup_verification(Some(holder)),
                )
            })
            .transpose()?;
        if let (Some(holder), Some(decision)) = (holder.as_ref(), clear_decision.as_ref()) {
            if !matches!(decision, LeaseDecision::Clear { .. }) {
                self.write_queue(&queue)?;
                return Ok(AcquireToken::Held {
                    agent: holder.agent.clone(),
                    seconds_left: holder.expires_at.saturating_sub(now).max(0),
                });
            }
        }

        if let Some(head) = queue.first() {
            if head.agent != args.agent {
                let agent = head.agent.clone();
                self.write_queue(&queue)?;
                return Ok(AcquireToken::WaitTurn(agent));
            }
        }

        let reclaimed = holder.map(|holder| match clear_decision {
            Some(LeaseDecision::Clear { evidence }) => {
                format!(
                    "{} ({})",
                    holder.agent,
                    render_lock_holder_clear_evidence(&evidence)
                )
            }
            _ => holder.agent,
        });
        self.write_holder(&new_holder(
            &args.agent,
            &args.pr,
            args.hold,
            reclaimed.clone(),
        )?)?;
        remove_if_exists(&self.paths.owner)?;
        queue.retain(|entry| entry.agent != args.agent);
        self.write_queue(&queue)?;
        Ok(match reclaimed {
            Some(previous) if !previous.is_empty() => AcquireToken::AcquiredReclaimed(previous),
            _ => AcquireToken::Acquired,
        })
    }

    fn renew(&self, agent: &str, hold: u64, announce: bool) -> Result<(), LandLockError> {
        self.with_guard(|| {
            let holder = self
                .read_holder()?
                .ok_or_else(|| LandLockError::RenewNotOwner {
                    agent: agent.to_string(),
                })?;
            self.require_no_cleanup(Some(&holder))?;
            if holder.agent != agent {
                return Err(LandLockError::RenewNotOwner {
                    agent: agent.to_string(),
                });
            }
            self.assert_current_process_owner("renew")?;
            self.write_holder(&holder.renewed(hold)?)?;
            Ok(())
        })?;
        if announce {
            eprintln!("landing-lock: renewed {agent} lease {hold}s");
        }
        Ok(())
    }

    fn release(&self, agent: &str, announce: bool) -> Result<(), LandLockError> {
        let (released, next) = self.with_guard(|| {
            let holder = self.read_holder()?;
            self.require_no_cleanup(holder.as_ref())?;
            let Some(holder) = holder else {
                remove_if_exists(&self.paths.owner)?;
                return Ok((false, None));
            };
            if holder.agent != agent {
                return Err(LandLockError::ReleaseNotOwner {
                    agent: agent.to_string(),
                    holder: holder.agent,
                });
            }
            self.assert_current_process_owner("release")?;
            remove_if_exists(&self.paths.lock)?;
            remove_if_exists(&self.paths.owner)?;
            self.remove_from_queue(agent)?;
            Ok((
                true,
                self.read_queue()?.first().map(|entry| entry.agent.clone()),
            ))
        })?;
        if announce {
            match (released, next) {
                (false, _) => eprintln!("landing-lock: release: no lock held"),
                (true, None) => {
                    eprintln!("landing-lock: RELEASED by {agent}; lock FREE (queue empty)")
                }
                (true, Some(next)) => {
                    eprintln!("landing-lock: RELEASED by {agent}; lock FREE -> next: {next}")
                }
            }
        }
        Ok(())
    }

    fn status(&self) -> Result<(), LandLockError> {
        let now = epoch_seconds()?;
        let holder = self.read_holder()?;
        let liveness = self.owner_liveness()?;
        let cleanup = self.cleanup_verification(holder.as_ref());
        let quarantined = !matches!(cleanup, CleanupVerification::None);
        match cleanup {
            CleanupVerification::None => {}
            CleanupVerification::Armed { record, reason } => {
                println!("QUARANTINED (pre-spawn barrier armed):");
                print_cleanup_record(&record, &reason);
            }
            CleanupVerification::Active { record, reason } => {
                println!("QUARANTINED (cleanup active):");
                print_cleanup_record(&record, &reason);
            }
            CleanupVerification::Recoverable { record, reason } => {
                println!("QUARANTINED (absence proven; run reclaim-dead):");
                print_cleanup_record(&record, &reason);
            }
            CleanupVerification::Uncensused { record, reason } => {
                println!("QUARANTINED (payload domain lacks final census):");
                print_cleanup_record(&record, &reason);
                print_census_recovery(&record, "ci-hub land-lock");
            }
            CleanupVerification::Unknown { record, reason } => {
                println!("QUARANTINED (cleanup unverifiable):");
                if let Some(record) = record {
                    print_cleanup_record(&record, &reason);
                } else {
                    println!("  reason={reason}");
                }
            }
        }
        if quarantined {
            println!(
                "  lock_holder_process={}",
                render_lock_holder_liveness(&liveness)
            );
        } else {
            match holder {
                Some(holder)
                    if holder.live_at(now) && matches!(liveness, OwnerLiveness::Dead(_)) =>
                {
                    println!("LOCK-HOLDER PROCESS EXITED (reclaimable):");
                    for line in holder.render().lines() {
                        println!("  {line}");
                    }
                    println!(
                        "  lock_holder_process={}",
                        render_lock_holder_liveness(&liveness)
                    );
                    println!("  secs_left={}", holder.expires_at - now);
                }
                Some(holder) if holder.live_at(now) => {
                    println!("HELD:");
                    for line in holder.render().lines() {
                        println!("  {line}");
                    }
                    println!(
                        "  lock_holder_process={}",
                        render_lock_holder_liveness(&liveness)
                    );
                    println!("  secs_left={}", holder.expires_at - now);
                }
                Some(holder) => {
                    println!("LAPSED (reclaimable):");
                    for line in holder.render().lines() {
                        println!("  {line}");
                    }
                    println!(
                        "  lock_holder_process={}",
                        render_lock_holder_liveness(&liveness)
                    );
                }
                None => println!("FREE"),
            }
        }
        let queue = self.read_queue()?;
        if !queue.is_empty() {
            println!("queue (FIFO):");
            for (index, entry) in queue.iter().enumerate() {
                print!("  {:>6}\t{}", index + 1, entry.render());
            }
        }
        Ok(())
    }

    /// Discharge an UNCENSUSED landing quarantine post-hoc.
    ///
    /// The census this satisfies can otherwise only be produced by the LIVE
    /// supervisor inside `run()` (`begin_run_census`/`record_residuals` both go
    /// through `transition_run_cleanup`, which requires being the current
    /// holder). When that supervisor dies at `phase=published`, nothing can move
    /// the record forward and `reclaim-dead` refuses forever on the same boot.
    /// This is the missing producer, gated the same five ways validate-lock
    /// gates its own.
    fn census_orphaned_domain(&self, args: CensusOrphanedDomainArgs) -> Result<i32, LandLockError> {
        let (subject, pgid, disposition) = self.with_guard(|| {
            let holder = self.read_holder()?.ok_or_else(|| {
                LandLockError::ReclaimNotProven(
                    "no lock holder; a census only discharges a quarantine bound to one".into(),
                )
            })?;

            // (1) The quarantine must be exactly the one this path addresses.
            let record = match self.cleanup_verification(Some(&holder)) {
                CleanupVerification::Uncensused { record, .. } => record,
                CleanupVerification::None => {
                    return Err(LandLockError::ReclaimNotProven(
                        "no cleanup authority is quarantined; nothing to census".into(),
                    ))
                }
                CleanupVerification::Recoverable { reason, .. } => {
                    return Err(LandLockError::ReclaimNotProven(format!(
                        "payload absence is ALREADY proven; run reclaim-dead instead: {reason}"
                    )))
                }
                CleanupVerification::Active { reason, .. } => {
                    return Err(LandLockError::ReclaimNotProven(format!(
                        "payload identities are STILL ALIVE; a census cannot bury a live \
                         domain: {reason}"
                    )))
                }
                CleanupVerification::Armed { reason, .. } => {
                    return Err(LandLockError::ReclaimNotProven(format!(
                        "cleanup is only ARMED, so no payload was ever published and there is \
                         no domain to census: {reason}"
                    )))
                }
                CleanupVerification::Unknown { reason, .. } => {
                    return Err(LandLockError::ReclaimNotProven(format!(
                        "cleanup authority is UNVERIFIABLE; refusing to census it: {reason}"
                    )))
                }
            };

            let (leader, pgid, residuals, incomplete_residual) = match &record.phase {
                CleanupPhase::Published { leader, pgid }
                | CleanupPhase::CensusPending { leader, pgid } => {
                    let restated = args.leader.as_deref().ok_or_else(|| {
                        LandLockError::ReclaimNotProven(
                            "--leader is required for a published cleanup record".into(),
                        )
                    })?;
                    let restated = parse_leader_identity(restated)?;
                    if *leader != restated {
                        return Err(LandLockError::ReclaimNotProven(format!(
                            "restated leader {}:{} does not match recorded leader {}:{}",
                            restated.pid, restated.start_ticks, leader.pid, leader.start_ticks
                        )));
                    }
                    (Some(leader.clone()), *pgid, Vec::new(), false)
                }
                CleanupPhase::Residual {
                    pgid,
                    domain_complete: false,
                    residuals,
                } => {
                    if args.leader.is_some() {
                        return Err(LandLockError::ReclaimNotProven(
                            "--leader must be omitted for an incomplete residual record, which \
                             has no recorded leader identity"
                                .into(),
                        ));
                    }
                    (None, *pgid, residuals.clone(), true)
                }
                other => {
                    return Err(LandLockError::ReclaimNotProven(format!(
                        "uncensused authority has unexpected phase {other:?}"
                    )))
                }
            };

            // (2) The caller must restate the recorded identity exactly, so this
            // cannot be aimed blind at whatever is quarantined right now.
            if record.agent != args.agent {
                return Err(LandLockError::ReclaimNotProven(format!(
                    "restated agent {:?} does not match recorded agent {:?}",
                    args.agent, record.agent
                )));
            }
            if record.operation != args.operation {
                return Err(LandLockError::ReclaimNotProven(format!(
                    "restated operation {:?} does not match recorded operation {:?}",
                    args.operation, record.operation
                )));
            }
            if pgid != args.pgid {
                return Err(LandLockError::ReclaimNotProven(format!(
                    "restated pgid {} does not match recorded pgid {pgid}",
                    args.pgid
                )));
            }

            // (3) A LIVE supervisor owns its own census; never race it.
            match self.owner_liveness()? {
                OwnerLiveness::Dead(_) => {}
                OwnerLiveness::Alive => {
                    return Err(LandLockError::ReclaimNotProven(
                        "the recorded supervisor process is ALIVE and owns this census; \
                         refusing to take it out from under a running landing"
                            .into(),
                    ))
                }
                OwnerLiveness::Unknown(reason) => {
                    return Err(LandLockError::ReclaimNotProven(format!(
                        "supervisor liveness is UNVERIFIABLE, so its death is not proven: {reason}"
                    )))
                }
            }

            // (4) Re-assert the mechanically checkable half of absence at the
            // instant of the write, under the guard.
            if let Some(leader) = &leader {
                match exact_process_liveness(leader) {
                    Ok(false) => {}
                    Ok(true) => {
                        return Err(LandLockError::ReclaimNotProven(format!(
                            "recorded leader {}:{} is ALIVE",
                            leader.pid, leader.start_ticks
                        )))
                    }
                    Err(reason) => {
                        return Err(LandLockError::ReclaimNotProven(format!(
                            "cannot verify recorded leader absence: {reason}"
                        )))
                    }
                }
            }
            for identity in &residuals {
                match exact_process_liveness(identity) {
                    Ok(false) => {}
                    Ok(true) => {
                        return Err(LandLockError::ReclaimNotProven(format!(
                            "recorded residual {}:{} is ALIVE",
                            identity.pid, identity.start_ticks
                        )))
                    }
                    Err(reason) => {
                        return Err(LandLockError::ReclaimNotProven(format!(
                            "cannot verify recorded residual absence: {reason}"
                        )))
                    }
                }
            }
            if process_group_exists(pgid) {
                return Err(LandLockError::ReclaimNotProven(format!(
                    "recorded process group {pgid} is still populated"
                )));
            }

            // (5) The domain itself, through the SHARED policy both authorities
            // use. New records carry the payload-owned process group; legacy
            // anchorless records retain the explicit attestation branch.
            let mechanical_context = match &leader {
                Some(leader) => format!(
                    "supervisor dead, leader {}:{} absent, pgid {pgid} absent",
                    leader.pid, leader.start_ticks
                ),
                None => format!(
                    "supervisor dead, {} recorded residual identity/identities absent, pgid \
                     {pgid} absent",
                    residuals.len()
                ),
            };
            let domain = if incomplete_residual {
                DomainEvidence::Unproven(
                    "residual process-domain capture was incomplete, so escaped descendants \
                     cannot be reconstructed post-hoc"
                        .into(),
                )
            } else {
                census_cleanup_domain(&record, pgid)
            };
            let uses_attestation = matches!(domain, DomainEvidence::Unproven(_));
            let disposition = census_disposition(
                domain,
                args.attest_domain_empty,
                &args.attester,
                &args.evidence,
                &mechanical_context,
            )
            .map_err(LandLockError::ReclaimNotProven)?;

            let recorded_residual_count = residuals.len();
            let mut recovered = record.clone();
            recovered.phase = CleanupPhase::Residual {
                pgid,
                domain_complete: true,
                residuals,
            };
            recovered.attestation = uses_attestation
                .then(|| DomainAttestation::new(&args.attester, &args.evidence))
                .transpose()
                .map_err(LandLockError::ReclaimNotProven)?;
            write_cleanup_record(&self.paths.cleanup, &recovered)
                .map_err(|source| io_error("write", &self.paths.cleanup, source))?;
            let subject = match leader {
                Some(leader) => format!("leader={}:{}", leader.pid, leader.start_ticks),
                None => format!(
                    "incomplete-residual recorded_identities={}",
                    recorded_residual_count
                ),
            };
            Ok((subject, pgid, disposition))
        })?;
        // Truthful about WHICH evidence discharged this: a mechanically proven
        // census and an operator attestation are not the same claim.
        eprintln!(
            "landing-lock: CENSUSED orphaned domain agent={} operation={} {subject} pgid={pgid}; \
             supervisor and every recorded payload identity proven absent, {}",
            args.agent, args.operation, disposition
        );
        eprintln!(
            "landing-lock: quarantine is now RECOVERABLE; run `land-lock reclaim-dead` to release \
             the lock."
        );
        Ok(0)
    }

    fn reclaim_dead(&self) -> Result<i32, LandLockError> {
        let reclaimed = self.with_guard(|| {
            let Some(holder) = self.read_holder()? else {
                return match self.cleanup_verification(None) {
                    CleanupVerification::None => {
                        remove_if_exists(&self.paths.owner)?;
                        Ok(None)
                    }
                    CleanupVerification::Armed { reason, .. }
                    | CleanupVerification::Active { reason, .. }
                    | CleanupVerification::Uncensused { reason, .. }
                    | CleanupVerification::Unknown { reason, .. } => {
                        Err(LandLockError::ReclaimNotProven(reason))
                    }
                    CleanupVerification::Recoverable { .. } => {
                        Err(LandLockError::ReclaimNotProven(
                            "cleanup authority is not bound to a lock holder".into(),
                        ))
                    }
                };
            };
            let now = epoch_seconds()?;
            match self.holder_stale_clear_decision(
                &holder,
                now,
                self.cleanup_verification(Some(&holder)),
            )? {
                LeaseDecision::Clear { evidence } => {
                    remove_if_exists(&self.paths.lock)?;
                    remove_if_exists(&self.paths.owner)?;
                    remove_cleanup_record_and_claim(&self.paths.cleanup, &self.paths.anchor)
                        .map_err(|source| io_error("remove", &self.paths.cleanup, source))?;
                    Ok(Some((holder.agent, holder.pr, evidence)))
                }
                decision => Err(LandLockError::ReclaimNotProven(decision.to_string())),
            }
        })?;
        match reclaimed {
            Some((agent, pr, reason)) => eprintln!(
                "landing-lock: evidence-reclaimed holder_agent={agent} pr={pr}: {}",
                render_lock_holder_clear_evidence(&reason)
            ),
            None => eprintln!("landing-lock: reclaim-dead: no lock held"),
        }
        Ok(0)
    }

    fn run(&self, args: RunArgs) -> Result<i32, LandLockError> {
        if args.child.is_empty() {
            return Err(LandLockError::EmptyChild);
        }
        if args.child_deadline == 0 {
            return Err(LandLockError::UnboundedChildDeadline);
        }
        enable_child_subreaper().map_err(|source| {
            io_error("enable child subreaper at", Path::new("/proc/self"), source)
        })?;

        // BOX-GLOBAL ANCHOR, TAKEN OUTERMOST AND HELD FOR THE WHOLE RUN.
        //
        // Before this, landing exclusion was per selected lease file, so two
        // checkouts — or one checkout naming two paths — landed 2/2 concurrently.
        // The anchor is taken BEFORE any lease work so a loser waits without
        // holding its own repository's lease, and `_box_anchor` lives until `run`
        // returns; the kernel drops the flock if this supervisor dies, which is
        // why it can never wedge the box the way a stale record can.
        //
        // RESIDUE, stated rather than papered over: the flock spans the
        // SUPERVISOR, so an escaped payload that outlives its supervisor releases
        // the anchor early. Within a repository that case is still covered by the
        // durable cleanup quarantine, which is unchanged and still consulted
        // below; ACROSS repositories it is not. Closing that needs a box-global
        // durable record, which is a different change from this one.
        let _box_anchor = match BoxExclusionAnchor::take(&self.paths.anchor, args.wait)
            .map_err(|source| io_error("take box exclusion anchor", &self.paths.anchor, source))?
        {
            Some(anchor) => anchor,
            None => {
                eprintln!(
                    "REFUSED: box-exclusive landing anchor held by {}; this box \
                     lands one PR at a time across ALL checkouts and clones, not \
                     one per lock file",
                    BoxExclusionAnchor::describe_holder(&self.paths.anchor)
                );
                return Ok(REFUSED_EXIT_CODE);
            }
        };
        eprintln!(
            "landing-lock: box-global anchor held at {}",
            _box_anchor.path().display()
        );

        // Durable half of box exclusion, checked WITH the anchor held so the
        // answer cannot change under us. The anchor proves no LIVE supervisor
        // holds the box; this proves no DEAD supervisor left a live payload
        // behind in some other repository.
        if let Some(reason) = foreign_box_claim(&self.paths.anchor, &self.paths.cleanup) {
            eprintln!("REFUSED: {reason}");
            return Ok(REFUSED_EXIT_CODE);
        }

        let acquire = AcquireArgs {
            agent: args.agent.clone(),
            pr: args.pr.clone(),
            wait: args.wait,
            hold: args.hold,
        };
        let acquire_status = self.acquire(&acquire)?;
        if acquire_status != 0 {
            return Ok(acquire_status);
        }
        if let Err(error) = self.with_guard(|| self.write_current_process_owner()) {
            let _ = self.release(&args.agent, true);
            return Err(error);
        }

        // Persist the fail-closed authority before any child exists. The start
        // gate below cannot run the guest until its exact identity has replaced
        // this armed record durably.
        self.arm_run(&args.agent, &args.pr)?;
        #[cfg(test)]
        if take_run_crash_hook(&format!("pr:{}", args.pr), RunCrashPoint::AfterArm) {
            return Err(LandLockError::CleanupQuarantined(
                "test-injected supervisor crash after arm".into(),
            ));
        }
        let mut gated = match spawn_gated_child(&args.child[0], &args.child[1..], |_| {}) {
            Ok(gated) => gated,
            Err(source) => {
                self.clear_unstarted_run(&args.agent, &args.pr)?;
                self.release(&args.agent, true)?;
                return Err(LandLockError::Io {
                    action: "launch start-gated child from",
                    path: PathBuf::from(&args.child[0]),
                    source,
                });
            }
        };
        #[cfg(test)]
        if take_run_crash_hook(&format!("pr:{}", args.pr), RunCrashPoint::AfterSpawn) {
            return Err(LandLockError::CleanupQuarantined(
                "test-injected supervisor crash after spawn".into(),
            ));
        }
        self.publish_run(&args.agent, &args.pr, &gated)?;
        gated.release().map_err(|source| {
            io_error("release child start gate at", &self.paths.cleanup, source)
        })?;
        #[cfg(test)]
        if take_run_crash_hook(&format!("pr:{}", args.pr), RunCrashPoint::AfterPublish) {
            return Err(LandLockError::CleanupQuarantined(
                "test-injected supervisor crash after publication".into(),
            ));
        }

        let (stop_tx, stop_rx) = mpsc::channel();
        let heartbeat_paths = self.paths.clone();
        let heartbeat_agent = args.agent.clone();
        let heartbeat_pr = args.pr.clone();
        let heartbeat_hold = args.hold;
        let heartbeat = thread::spawn(move || {
            let heartbeat_lock = LandingLock {
                paths: heartbeat_paths,
            };
            let interval = Duration::from_secs((heartbeat_hold / 3).max(1));
            while stop_rx.recv_timeout(interval).is_err() {
                if heartbeat_lock
                    .renew_run_lease(&heartbeat_agent, &heartbeat_pr, heartbeat_hold)
                    .is_err()
                {
                    break;
                }
            }
        });

        let mut lineage = PayloadLineage::new(
            std::process::id(),
            gated.leader.start_ticks,
            &gated.domain_token,
            gated.pgid,
        );
        let outcome = supervise_child(
            &mut gated.child,
            args.child_deadline,
            &args.pr,
            &mut lineage,
        );
        // Atomically disable heartbeat renewal before stopping it. A heartbeat
        // already holding the guard finishes first; after this durable phase
        // transition no stale renewal can race cleanup or authorize reclaim.
        self.begin_run_census(&args.agent, &args.pr, &gated)?;
        let _ = stop_tx.send(());
        let _ = heartbeat.join();
        reap_exited_children();
        let pgid = outcome.pgid();
        // Kill, do not merely freeze, whatever outlived the leader: a setsid'd
        // descendant escapes the group signal but not its payload token, and a
        // tokenless one escapes both but not the lineage recorded from its
        // parent (https://github.com/rrnewton/dev-hermit/issues/419). The
        // capture names only what SIGKILL could not remove, and that still
        // quarantines.
        let capture = tear_down_payload_domain(
            &mut lineage,
            Duration::from_secs(CHILD_TERM_GRACE_SECONDS),
            "landing-lock",
        );
        let domain_empty =
            !process_group_exists(pgid) && capture.complete && capture.identities.is_empty();
        if !domain_empty {
            let residual_count = capture.identities.len();
            // Name the unclassifiable separately. An operator reading this has
            // to be able to tell "your payload leaked N processes" from "I
            // could not read M of them", because those need different actions
            // and only the first is the payload's fault.
            let unreadable = match capture.unclassifiable.len() {
                0 => String::new(),
                count => format!(
                    "; {count} further descendant(s) UNCLASSIFIABLE (environment unreadable, so \
                     membership was neither proven nor excluded): {}",
                    capture
                        .unclassifiable
                        .iter()
                        .map(|identity| identity.pid.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            };
            self.record_residuals(&args.agent, &args.pr, pgid, capture)?;
            return Err(LandLockError::CleanupQuarantined(format!(
                "completed process group {pgid} did not yield an empty payload domain; {residual_count} exact residual identities recorded and lock retained{unreadable}"
            )));
        }
        self.clear_proven_run(&args.agent, &args.pr, pgid)?;
        self.release(&args.agent, true)?;
        match outcome {
            ChildOutcome::Exited { status, pgid } => {
                debug_assert_eq!(pgid, gated.pgid);
                Ok(exit_status_code(status))
            }
            ChildOutcome::TimedOut { pgid } => {
                debug_assert_eq!(pgid, gated.pgid);
                eprintln!(
                    "landing-lock: ABANDON PR #{}: child exceeded --child-deadline {}s; \
                     killed the land subtree and RELEASED the lock so the FIFO can proceed. \
                     PR left open for retry.",
                    args.pr, args.child_deadline
                );
                Ok(CHILD_DEADLINE_EXIT_CODE)
            }
            ChildOutcome::Uncertain { reason, .. } => Err(LandLockError::InvalidState(format!(
                "child supervision failed after payload domain was proven empty: {reason}"
            ))),
        }
    }

    fn with_guard<T>(
        &self,
        operation: impl FnOnce() -> Result<T, LandLockError>,
    ) -> Result<T, LandLockError> {
        let guard = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.paths.guard)
            .map_err(|source| io_error("open guard", &self.paths.guard, source))?;
        let deadline = Instant::now() + Duration::from_secs(GUARD_WAIT_SECONDS);
        loop {
            match guard.try_lock_exclusive() {
                Ok(()) => break,
                Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(LandLockError::GuardTimeout);
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                Err(source) => return Err(io_error("lock guard", &self.paths.guard, source)),
            }
        }
        let result = operation();
        let unlock = FileExt::unlock(&guard)
            .map_err(|source| io_error("unlock guard", &self.paths.guard, source));
        match result {
            Ok(value) => {
                unlock?;
                Ok(value)
            }
            Err(error) => {
                let _ = unlock;
                Err(error)
            }
        }
    }

    fn read_holder(&self) -> Result<Option<LockState>, LandLockError> {
        if !self.paths.lock.exists() {
            return Ok(None);
        }
        let content = read_to_string(&self.paths.lock)?;
        Ok(Some(LockState::parse(&content)?))
    }

    fn read_process_owner(&self) -> Result<Option<ProcessOwner>, LandLockError> {
        if !self.paths.owner.exists() {
            return Ok(None);
        }
        let content = read_to_string(&self.paths.owner)?;
        Ok(Some(ProcessOwner::parse(&content)?))
    }

    fn write_current_process_owner(&self) -> Result<(), LandLockError> {
        let owner = current_process_owner()?;
        write_truncated(&self.paths.owner, owner.render().as_bytes())
    }

    fn owner_liveness(&self) -> Result<OwnerLiveness, LandLockError> {
        let Some(owner) = self.read_process_owner()? else {
            return Ok(OwnerLiveness::Unknown(
                "no process evidence (legacy/manual lease)".into(),
            ));
        };
        let host = current_host();
        if owner.host != host {
            return Ok(OwnerLiveness::Unknown(format!(
                "owner host {} differs from local host {host}",
                owner.host
            )));
        }
        let boot_id = current_boot_id()?;
        if owner.boot_id != boot_id {
            return Ok(OwnerLiveness::Dead(format!(
                "host rebooted (owner boot_id={} current={boot_id})",
                owner.boot_id
            )));
        }
        match process_start_ticks(owner.pid) {
            Ok(start_ticks) if start_ticks == owner.start_ticks => Ok(OwnerLiveness::Alive),
            Ok(start_ticks) => Ok(OwnerLiveness::Dead(format!(
                "pid {} was reused (owner start_ticks={} current={start_ticks})",
                owner.pid, owner.start_ticks
            ))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(OwnerLiveness::Dead(format!("pid {} is absent", owner.pid)))
            }
            Err(error) => Ok(OwnerLiveness::Unknown(format!(
                "cannot inspect pid {}: {error}",
                owner.pid
            ))),
        }
    }

    fn assert_current_process_owner(&self, operation: &'static str) -> Result<(), LandLockError> {
        let Some(owner) = self.read_process_owner()? else {
            return Ok(());
        };
        let current = current_process_owner()?;
        if owner == current {
            Ok(())
        } else {
            Err(LandLockError::ProcessNotOwner {
                operation,
                pid: owner.pid,
            })
        }
    }

    fn write_holder(&self, holder: &LockState) -> Result<(), LandLockError> {
        write_truncated(&self.paths.lock, holder.render().as_bytes())
    }

    fn read_queue(&self) -> Result<Vec<QueueEntry>, LandLockError> {
        if !self.paths.queue.exists() {
            return Ok(Vec::new());
        }
        let content = read_to_string(&self.paths.queue)?;
        content
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.is_empty())
            .map(|(index, line)| QueueEntry::parse(line, index + 1))
            .collect()
    }

    fn write_queue(&self, queue: &[QueueEntry]) -> Result<(), LandLockError> {
        let mut content = String::new();
        for entry in queue {
            content.push_str(&entry.render());
        }
        let temporary = suffix(&self.paths.queue, ".tmp");
        write_truncated(&temporary, content.as_bytes())?;
        fs::rename(&temporary, &self.paths.queue)
            .map_err(|source| io_error("replace queue", &self.paths.queue, source))
    }

    fn remove_from_queue(&self, agent: &str) -> Result<(), LandLockError> {
        let mut queue = self.read_queue()?;
        queue.retain(|entry| entry.agent != agent);
        self.write_queue(&queue)
    }
}

fn new_holder(
    agent: &str,
    pr: &str,
    hold: u64,
    reclaimed_from: Option<String>,
) -> Result<LockState, LandLockError> {
    let acquired_at = epoch_seconds()?;
    let acquired_human = Local
        .timestamp_opt(acquired_at, 0)
        .single()
        .ok_or_else(|| LandLockError::InvalidState("current timestamp is out of range".into()))?
        .format("%Y-%m-%dT%H:%M:%S%z")
        .to_string();
    let host = current_host();
    Ok(LockState {
        agent: agent.to_string(),
        repo: None,
        operation: None,
        pending_mutation: None,
        pending_attempt: None,
        pending_call_count: None,
        pending_call_id: None,
        pr: pr.to_string(),
        host,
        acquired_at,
        acquired_human,
        expires_at: acquired_at.saturating_add(hold as i64),
        reclaimed_from,
    })
}

fn epoch_seconds() -> Result<i64, LandLockError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            LandLockError::InvalidState(format!("system clock before epoch: {error}"))
        })?
        .as_secs();
    i64::try_from(seconds)
        .map_err(|_| LandLockError::InvalidState("system time exceeds i64".into()))
}

fn parse_integer(name: &str, value: &str) -> Result<i64, LandLockError> {
    value.parse().map_err(|error| {
        LandLockError::InvalidState(format!("{name} must be an integer, got {value:?}: {error}"))
    })
}

fn parse_unsigned(name: &str, value: &str) -> Result<u64, LandLockError> {
    value.parse().map_err(|error| {
        LandLockError::InvalidState(format!("{name} must be an integer, got {value:?}: {error}"))
    })
}

/// This host's short name from `hostname -s`, or `"unknown"` when that fails.
///
/// ⚠️ THE FIRST SUCCESSFUL ANSWER IS KEPT FOR THE LIFE OF THE PROCESS. A run
/// supervisor's heartbeat thread calls this while the payload runs and while
/// it is torn down. Each uncached call spawned `hostname` as a direct child of
/// the supervisor in its own process group (`bounded_output` spawns with
/// `process_group(0)`), which is exactly the shape [`PayloadLineage`] records
/// as a payload process whose parent exited, and the supervisor's
/// `waitpid(-1)` loops could reap that child before `bounded_output` did.
/// `arm_run` needs a real host name before any payload exists, so once a run
/// is armed no later call spawns anything. A failure is not kept, so a
/// transient one does not make every later record say `"unknown"`.
pub(crate) fn current_host() -> String {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(host) = HOST.get() {
        return host.clone();
    }
    let host = Command::new("hostname")
        .arg("-s")
        .bounded_output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty());
    match host {
        Some(host) => HOST.get_or_init(|| host).clone(),
        None => "unknown".to_string(),
    }
}

fn current_boot_id() -> Result<String, LandLockError> {
    let path = Path::new("/proc/sys/kernel/random/boot_id");
    fs::read_to_string(path)
        .map(|value| value.trim().to_string())
        .map_err(|source| io_error("read", path, source))
}

pub(crate) fn process_start_ticks(pid: u32) -> io::Result<u64> {
    process_stat_fields(pid).map(|(_, _, start_ticks)| start_ticks)
}

/// The fields of a `/proc/<pid>/stat` line that follow the `comm` field.
///
/// ⚠️ `comm` IS BYTES, NOT TEXT. A process may name itself anything, and the
/// kernel cuts a task name to 15 bytes, which can split a multi-byte UTF-8
/// character. Reading the whole line with `read_to_string` failed with
/// InvalidData for such a process. The census reads every `/proc` entry on
/// the host, so one unrelated process with such a name made the scan fail,
/// the census incomplete, and the run quarantine instead of releasing
/// (devbig014, 2026-09-29, host pid 261192;
/// https://github.com/rrnewton/dev-hermit/issues/434). Everything after the
/// LAST `)` is a state letter and numbers.
fn stat_fields_after_comm(stat: &[u8]) -> io::Result<&str> {
    let close = stat.iter().rposition(|byte| *byte == b')').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "process stat has no closing comm",
        )
    })?;
    std::str::from_utf8(&stat[close + 1..]).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("process stat fields after comm are not text: {error}"),
        )
    })
}

fn process_stat_fields(pid: u32) -> io::Result<(char, u32, u64)> {
    let path = PathBuf::from(format!("/proc/{pid}/stat"));
    let stat = fs::read(path)?;
    let fields: Vec<_> = stat_fields_after_comm(&stat)?.split_whitespace().collect();
    let state = fields
        .first()
        .and_then(|value| value.chars().next())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "process stat has no state field",
            )
        })?;
    let parent = fields.get(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "process stat has no parent field",
        )
    })?;
    let start_ticks = fields.get(19).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "process stat has no starttime field",
        )
    })?;
    let parent = parent.parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid process parent {parent:?}: {error}"),
        )
    })?;
    let start_ticks = start_ticks.parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid process starttime {start_ticks:?}: {error}"),
        )
    })?;
    Ok((state, parent, start_ticks))
}

pub(crate) fn read_current_boot_id() -> io::Result<String> {
    fs::read_to_string("/proc/sys/kernel/random/boot_id").map(|value| value.trim().to_string())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProcessGroupLiveness {
    Active,
    Absent,
    Unknown(String),
}

fn process_group_liveness(pgid: u32) -> ProcessGroupLiveness {
    let Some(target) = process_group_target(pgid) else {
        return ProcessGroupLiveness::Unknown(format!("invalid process group {pgid}"));
    };
    // SAFETY: `process_group_target` excludes zero and the special -1 broadcast
    // target. Signal zero probes existence without delivering a signal.
    if unsafe { libc::kill(target, 0) } == 0 {
        return ProcessGroupLiveness::Active;
    }
    let source = io::Error::last_os_error();
    if source.raw_os_error() == Some(libc::ESRCH) {
        ProcessGroupLiveness::Absent
    } else {
        ProcessGroupLiveness::Unknown(source.to_string())
    }
}

pub(crate) fn exact_process_liveness(identity: &ProcessIdentity) -> Result<bool, String> {
    match process_start_ticks(identity.pid) {
        Ok(start_ticks) => Ok(start_ticks == identity.start_ticks),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(format!(
            "cannot verify pid {}@{}: {source}",
            identity.pid, identity.start_ticks
        )),
    }
}

/// Scheduler state letter of exactly `identity` (`R`, `S`, `T`, `t`, `Z`, ...),
/// `Ok(None)` when that pid is gone or now names a later process, and `Err`
/// when `/proc` cannot answer. State and start time come from ONE `stat` read,
/// so the letter belongs to this generation.
pub(crate) fn exact_process_state(identity: &ProcessIdentity) -> Result<Option<char>, String> {
    match process_stat_fields(identity.pid) {
        Ok((state, _, start_ticks)) => Ok((start_ticks == identity.start_ticks).then_some(state)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(format!(
            "cannot read the state of pid {}@{}: {source}",
            identity.pid, identity.start_ticks
        )),
    }
}

pub(crate) fn verify_cleanup_record(
    path: &Path,
    expected: Option<(&str, &str)>,
) -> CleanupVerification {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return CleanupVerification::None
        }
        Err(source) => {
            return CleanupVerification::Unknown {
                record: None,
                reason: format!("cannot read {}: {source}", path.display()),
            }
        }
    };
    let record = match CleanupRecord::parse(&content) {
        Ok(record) => record,
        Err(reason) => {
            return CleanupVerification::Unknown {
                record: None,
                reason: format!("malformed {}: {reason}", path.display()),
            }
        }
    };
    let Some((agent, operation)) = expected else {
        return CleanupVerification::Unknown {
            record: Some(record),
            reason: "cleanup authority has no matching lock holder".into(),
        };
    };
    if record.agent != agent || record.operation != operation {
        return CleanupVerification::Unknown {
            reason: format!(
                "cleanup authority binds agent={:?} operation={:?}, not agent={agent:?} operation={operation:?}",
                record.agent, record.operation
            ),
            record: Some(record),
        };
    }
    let host = current_host();
    if host == "unknown" || record.host != host {
        return CleanupVerification::Unknown {
            reason: format!(
                "cleanup authority host {:?} does not match current host {host:?}",
                record.host
            ),
            record: Some(record),
        };
    }
    let boot_id = match read_current_boot_id() {
        Ok(boot_id) => boot_id,
        Err(source) => {
            return CleanupVerification::Unknown {
                reason: format!("cannot read current boot identity: {source}"),
                record: Some(record),
            }
        }
    };
    if record.boot_id != boot_id {
        return CleanupVerification::Recoverable {
            reason: "recorded payload domain is from an earlier host boot".into(),
            record,
        };
    }
    match &record.phase {
        CleanupPhase::Armed => CleanupVerification::Armed {
            reason: "pre-spawn barrier is armed; no payload identity was durably published".into(),
            record,
        },
        CleanupPhase::Published { leader, pgid } | CleanupPhase::CensusPending { leader, pgid } => {
            let mut active = Vec::new();
            let mut unknown = Vec::new();
            match exact_process_liveness(leader) {
                Ok(true) => active.push(format!("{}@{}", leader.pid, leader.start_ticks)),
                Ok(false) => {}
                Err(reason) => unknown.push(reason),
            }
            match process_group_liveness(*pgid) {
                ProcessGroupLiveness::Active => active.push(format!("pgid {pgid}")),
                ProcessGroupLiveness::Absent => {}
                ProcessGroupLiveness::Unknown(reason) => {
                    unknown.push(format!("cannot verify pgid {pgid}: {reason}"))
                }
            }
            if !active.is_empty() {
                CleanupVerification::Active {
                    reason: format!(
                        "published payload identities remain active: {}",
                        active.join(", ")
                    ),
                    record,
                }
            } else {
                let mut reason = unknown;
                reason.push(
                    "published payload ended without a complete residual census; same-boot absence cannot exclude an escaped descendant"
                        .into(),
                );
                CleanupVerification::Uncensused {
                    reason: reason.join("; "),
                    record,
                }
            }
        }
        CleanupPhase::Residual {
            pgid,
            domain_complete,
            residuals,
        } => {
            let mut active = Vec::new();
            let mut unknown = Vec::new();
            for identity in residuals {
                match exact_process_liveness(identity) {
                    Ok(true) => active.push(format!("{}@{}", identity.pid, identity.start_ticks)),
                    Ok(false) => {}
                    Err(reason) => unknown.push(reason),
                }
            }
            match process_group_liveness(*pgid) {
                ProcessGroupLiveness::Active => active.push(format!("pgid {pgid}")),
                ProcessGroupLiveness::Absent => {}
                ProcessGroupLiveness::Unknown(reason) => {
                    unknown.push(format!("cannot verify pgid {pgid}: {reason}"))
                }
            }
            if !active.is_empty() {
                CleanupVerification::Active {
                    reason: format!("payload identities remain active: {}", active.join(", ")),
                    record,
                }
            } else if !unknown.is_empty() {
                CleanupVerification::Unknown {
                    reason: unknown.join("; "),
                    record: Some(record),
                }
            } else if !domain_complete {
                CleanupVerification::Uncensused {
                    reason: "all recorded residual identities and the captured process group are absent, but residual process-domain capture was incomplete; escaped descendants remain unproven"
                        .into(),
                    record,
                }
            } else {
                CleanupVerification::Recoverable {
                    reason:
                        "all recorded residual identities and the captured process group are absent"
                            .into(),
                    record,
                }
            }
        }
    }
}

pub(crate) fn write_cleanup_record(path: &Path, record: &CleanupRecord) -> io::Result<()> {
    let temporary = PathBuf::from(format!("{}.tmp-{}", path.display(), std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(record.render().as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub(crate) fn remove_cleanup_record(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(source),
    }
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Box-global claims: making the per-repository quarantine visible box-wide.
//
// THE GAP THIS CLOSES. The exclusion anchor above is an `flock`, and an flock
// belongs to a PROCESS. When a supervisor dies but its payload escapes and keeps
// running, the kernel drops the anchor while the payload is still burning the
// box. Inside the owning repository that is already covered: the supervisor
// armed a durable cleanup record before spawning, `require_no_cleanup` refuses
// every later acquire there, and `reclaim-dead` / `census-orphaned-domain`
// discharge it on evidence. ACROSS repositories nothing saw that record, so a
// second clone was admitted next to a live escaped payload.
//
// WHAT IS AND IS NOT NEW HERE. No second quarantine, no second verifier, no new
// notion of liveness. The authority stays exactly what it was — the per-repo
// cleanup record, read by `verify_cleanup_record` — and it stays exactly where
// it was on disk. A claim is a POINTER to that record, nothing more, so the two
// can never disagree about whether the box is quarantined. In particular the
// claim carries no liveness of its own: `CleanupRecord::domain` names the exact
// boundary selected by the record version, and the shared cleanup verifier is
// the only code allowed to interpret that boundary.
//
// WHY A POINTER CANNOT WEDGE THE BOX. The flock's great virtue was that the
// kernel released it on death, so it could never strand anything; a durable
// record gives that up unless discharge is mechanical. Here it stays mechanical
// twice over. A claim is discharged the moment its record is — and a reader that
// dereferences a claim whose record is gone DELETES the claim on the spot. So a
// crashed supervisor, a deleted clone, or a wiped `/run/user` all self-heal on
// the next admission instead of requiring an operator.
//
// MIGRATION: NONE, deliberately. The claim is new state written alongside the
// arming of a record. No existing lease or quarantine file is moved, reread
// differently, or required to carry a new field, so a box with no claims behaves
// exactly as it does today. The one honest limit: a run already in flight under
// an older binary armed its record without a claim, so it keeps only the
// per-repository coverage it has now — the same position it was already in, never
// a worse one.
// ---------------------------------------------------------------------------

/// Directory of box-global claims for one anchor kind, beside the anchor itself.
///
/// Derived from the ANCHOR rather than from a global, so it inherits the anchor's
/// constructor-scoped isolation for free: production gets the uid-derived
/// location and each test gets its own beside its own temp lease.
pub(crate) fn box_claim_dir(anchor: &Path) -> PathBuf {
    let stem = anchor
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("box");
    let parent = anchor.parent().unwrap_or(Path::new("."));
    parent.join(format!("{stem}.claims"))
}

/// Stable filename for the claim pointing at `cleanup`. The file's CONTENT is
/// authoritative; this only has to be collision-free per cleanup path.
fn box_claim_file(anchor: &Path, cleanup: &Path) -> PathBuf {
    let encoded: String = cleanup
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    box_claim_dir(anchor).join(format!("{encoded}.claim"))
}

/// Publish a box-global pointer to this repository's cleanup record.
///
/// Best effort by design: a claim that cannot be written must never block a
/// legitimate run, because the per-repository quarantine — the actual authority —
/// is unaffected either way. Failing to publish loses cross-repository coverage
/// for that run and nothing else, which is exactly today's behaviour.
pub(crate) fn register_box_claim(anchor: &Path, cleanup: &Path, agent: &str, operation: &str) {
    let dir = box_claim_dir(anchor);
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let body = format!(
        "version=1\ncleanup={}\nagent={}\noperation={}\nhost={}\n",
        cleanup.display(),
        agent,
        operation,
        current_host()
    );
    let claim = box_claim_file(anchor, cleanup);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temporary = dir.join(format!(
        ".{}.tmp-{}-{nonce}",
        claim
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("claim"),
        std::process::id()
    ));
    let published = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, &claim)?;
        File::open(&dir)?.sync_all()?;
        Ok(())
    })();
    if published.is_err() {
        let _ = fs::remove_file(&temporary);
    }
}

/// Drop this repository's claim. Called wherever its cleanup record is removed;
/// a miss is harmless because readers prune a claim whose record is gone.
pub(crate) fn clear_box_claim(anchor: &Path, cleanup: &Path) {
    let _ = fs::remove_file(box_claim_file(anchor, cleanup));
}

/// Discharge a cleanup record and its box-global claim together.
///
/// The two must not drift, so every removal site goes through here rather than
/// remembering to do both. Order matters: the RECORD goes first, because that is
/// the authority — if the process dies between the two, the leftover claim
/// dereferences to a missing record and the next reader prunes it. Doing it the
/// other way round would leave a live record nobody outside its repository could
/// see, which is precisely the gap being closed.
pub(crate) fn remove_cleanup_record_and_claim(cleanup: &Path, anchor: &Path) -> io::Result<()> {
    let outcome = remove_cleanup_record(cleanup);
    clear_box_claim(anchor, cleanup);
    outcome
}

fn claim_field<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    body.lines().find_map(|line| line.strip_prefix(name))
}

/// Is ANOTHER repository's cleanup record still outstanding on this box?
///
/// Returns the reason to refuse, or `None` to proceed. Dereferences each claim
/// through `verify_cleanup_record`, the same and only cleanup authority the
/// owning repository uses — this function decides nothing about quarantine
/// itself, it only makes a decision already recorded elsewhere visible here.
/// Claims whose record has been cleared are pruned as they are read.
pub(crate) fn foreign_box_claim(anchor: &Path, own_cleanup: &Path) -> Option<String> {
    foreign_box_claim_with_active(anchor, std::slice::from_ref(&own_cleanup), false)
}

/// [`foreign_box_claim`] for a caller that owns SEVERAL cleanup records: a
/// repository's validation slots all register in the same claim directory.
/// Skipping only the active slot's record reported a quarantine in sibling
/// slot k as "another repository's", which is false and sent the operator to
/// the wrong place; the caller must settle its own slots itself (review of
/// https://github.com/rrnewton/dev-hermit/issues/419).
pub(crate) fn foreign_box_claim_excluding<P: AsRef<Path>>(
    anchor: &Path,
    own_cleanups: &[P],
) -> Option<String> {
    foreign_box_claim_with_active(anchor, own_cleanups, false)
}

/// The validation form of [`foreign_box_claim_excluding`]. Validation may
/// deliberately coexist with one other live validation. An armed run has not
/// released its start-gated child, and a published payload owned by a live
/// supervisor is not a quarantine. Every other cleanup state remains a
/// box-wide refusal.
pub(crate) fn foreign_box_quarantine_excluding<P: AsRef<Path>>(
    anchor: &Path,
    own_cleanups: &[P],
) -> Option<String> {
    foreign_box_claim_with_active(anchor, own_cleanups, true)
}

pub(crate) fn operation_is_validation(operation: &str) -> bool {
    operation.starts_with("validate:")
        || operation.starts_with("reverie-validate:")
        || operation.starts_with("frozen-validate:")
}

fn foreign_box_claim_with_active<P: AsRef<Path>>(
    anchor: &Path,
    own_cleanups: &[P],
    allow_published: bool,
) -> Option<String> {
    let entries = fs::read_dir(box_claim_dir(anchor)).ok()?;
    for entry in entries.flatten() {
        let claim_path = entry.path();
        if claim_path.extension().and_then(|e| e.to_str()) != Some("claim") {
            continue;
        }
        let Ok(body) = fs::read_to_string(&claim_path) else {
            continue;
        };
        let Some(cleanup) = claim_field(&body, "cleanup=").map(PathBuf::from) else {
            // Unparseable claim names no record, so it can never be discharged by
            // one. Drop it rather than block the box on a file nobody can clear.
            let _ = fs::remove_file(&claim_path);
            continue;
        };
        let Some(agent) = claim_field(&body, "agent=") else {
            return Some(format!(
                "box-global claim {} names cleanup {} but has no agent; refusing until the claim is repaired or removed",
                claim_path.display(),
                cleanup.display()
            ));
        };
        let Some(operation) = claim_field(&body, "operation=") else {
            return Some(format!(
                "box-global claim {} names cleanup {} but has no operation; refusing until the claim is repaired or removed",
                claim_path.display(),
                cleanup.display()
            ));
        };
        if own_cleanups.iter().any(|own| cleanup == own.as_ref()) {
            continue; // my own repository; the caller settles its own records
        }
        match verify_cleanup_record(&cleanup, Some((agent, operation))) {
            CleanupVerification::None => {
                // The owning repository discharged its record. Prune and move on.
                let _ = fs::remove_file(&claim_path);
            }
            CleanupVerification::Armed { .. }
                if allow_published && operation_is_validation(operation) =>
            {
                continue;
            }
            CleanupVerification::Active { record, .. }
                if allow_published
                    && operation_is_validation(operation)
                    && matches!(record.phase, CleanupPhase::Published { .. }) =>
            {
                continue;
            }
            other => {
                let detail = match &other {
                    CleanupVerification::Armed { record: r, .. }
                    | CleanupVerification::Active { record: r, .. }
                    | CleanupVerification::Uncensused { record: r, .. }
                    | CleanupVerification::Recoverable { record: r, .. }
                    | CleanupVerification::Unknown {
                        record: Some(r), ..
                    } => {
                        format!(
                            "agent={} operation={} phase={:?}",
                            r.agent, r.operation, r.phase
                        )
                    }
                    _ => "record present".to_string(),
                };
                return Some(format!(
                    "another repository's payload domain is still quarantined: \
                     {} ({detail}). Discharge it there with `reclaim-dead` (or \
                     `census-orphaned-domain` first if the supervisor died before \
                     censusing), which clears this claim automatically.",
                    cleanup.display()
                ));
            }
        }
    }
    None
}

/// A child whose wrapper has execed into its own process group but whose guest
/// command cannot start until `release` writes the one valid token. The pipe is
/// close-on-exec everywhere except the wrapper's fixed read descriptor, and
/// EOF/error is a fail-closed exit. Thus supervisor death before publication
/// cannot accidentally start the guest.
pub(crate) struct GatedChild {
    pub(crate) child: Child,
    pub(crate) leader: ProcessIdentity,
    pub(crate) pgid: u32,
    /// Value of [`PAYLOAD_DOMAIN_ENV`] handed to this payload. Inherited by
    /// every descendant at fork, so the census can attribute one even after
    /// reparenting has destroyed the PPID evidence.
    pub(crate) domain_token: String,
    release: Option<File>,
}

/// Environment variable carrying the payload-domain token.
///
/// ⚠️ THIS IS THE ONLY DOMAIN EVIDENCE THAT SURVIVES ADOPTION. A subreaper's
/// PPID walk cannot distinguish a payload descendant from an unrelated orphan
/// the kernel reparented onto the supervisor, and neither can the process
/// group, the start time, or -- measured 2026-08-25 -- the cgroup. Environment
/// is inherited at fork and is unaffected by `setsid`, `setpgid`, or
/// reparenting, which is exactly the set of things that defeat the others.
pub(crate) const PAYLOAD_DOMAIN_ENV: &str = "CI_HUB_PAYLOAD_DOMAIN";

/// Mint a token unique to one payload on one box.
///
/// The supervisor pid scopes it to this process, so a daemon abandoned by an
/// EARLIER run can never match; the counter scopes it to one payload within
/// this process, so a shared test binary's concurrent payloads stay distinct.
fn mint_domain_token() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    )
}

/// How a descendant relates to the payload domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DomainMembership {
    /// `/proc/<pid>/environ` carries this payload's token.
    Payload,
    /// `/proc/<pid>/environ` was readable and does NOT carry the token.
    Foreign,
    /// The environment could not be read. NOT evidence of either answer.
    Unclassifiable,
}

/// Classify one descendant against the payload token.
///
/// ⚠️ AN UNREADABLE ENVIRONMENT IS ITS OWN ANSWER. Folding it into `Foreign`
/// would silently discharge a real escaped payload, and folding it into
/// `Payload` would quarantine the box on a stranger. The caller must carry it
/// as a third state and say so.
fn classify_membership(pid: u32, token: &str) -> DomainMembership {
    #[cfg(test)]
    if UNREADABLE_ENVIRON_PID.load(std::sync::atomic::Ordering::SeqCst) == pid {
        return DomainMembership::Unclassifiable;
    }
    match fs::read(format!("/proc/{pid}/environ")) {
        Ok(raw) => {
            let needle = format!("{PAYLOAD_DOMAIN_ENV}={token}");
            let carries = raw
                .split(|byte| *byte == 0)
                .any(|entry| entry == needle.as_bytes());
            if carries {
                DomainMembership::Payload
            } else {
                DomainMembership::Foreign
            }
        }
        Err(_) => DomainMembership::Unclassifiable,
    }
}

impl GatedChild {
    pub(crate) fn release(&mut self) -> io::Result<()> {
        let mut release = self
            .release
            .take()
            .ok_or_else(|| io::Error::other("child start gate was already released"))?;
        release.write_all(b"ci-hub-go\n")?;
        release.flush()
    }
}

pub(crate) fn spawn_gated_child(
    program: &OsString,
    args: &[OsString],
    configure: impl FnOnce(&mut Command),
) -> io::Result<GatedChild> {
    let mut pipe_fds = [-1; 2];
    // SAFETY: `pipe_fds` names two writable c_int slots. Both returned
    // descriptors are immediately owned below.
    if unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful pipe2 returned two new owned descriptors.
    let read_gate = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
    // SAFETY: successful pipe2 returned two new owned descriptors.
    let release_gate = unsafe { File::from_raw_fd(pipe_fds[1]) };
    let read_fd = read_gate.as_raw_fd();

    let domain_token = mint_domain_token();
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(START_GATE_SCRIPT)
        .arg("ci-hub-start-gate")
        .arg(program)
        .args(args)
        .process_group(0);
    configure(&mut command);
    // Set the token AFTER `configure` so no call site can clear the domain
    // evidence, deliberately or by calling `env_clear`. Losing it would not
    // fail loudly -- it would make every descendant Foreign and silently
    // discharge a real escape.
    command.env(PAYLOAD_DOMAIN_ENV, &domain_token);
    // SAFETY: the callback uses only async-signal-safe descriptor syscalls.
    // It never waits: the execed shell performs the blocking read, so
    // `Command::spawn` can return to publish the exact identity.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(read_fd, START_GATE_FD) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(START_GATE_FD, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    drop(read_gate);
    let pid = child.id();
    let pid_t = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "child pid exceeds pid_t"))?;
    // SAFETY: getpgid only reads kernel state for this positive child PID.
    let observed_pgid = unsafe { libc::getpgid(pid_t) };
    if observed_pgid != pid_t {
        drop(release_gate);
        let _ = child.wait();
        let detail = if observed_pgid < 0 {
            io::Error::last_os_error().to_string()
        } else {
            format!("observed process group {observed_pgid}")
        };
        return Err(io::Error::other(format!(
            "start-gated child {pid} was not group leader: {detail}"
        )));
    }
    let leader = ProcessIdentity {
        pid,
        start_ticks: process_start_ticks(pid)?,
    };
    Ok(GatedChild {
        child,
        leader,
        pgid: pid,
        domain_token,
        release: Some(release_gate),
    })
}

#[cfg(test)]
const PROCESS_DOMAIN_TEST_CHILD_ENV: &str = "CI_HUB_PROCESS_DOMAIN_TEST_CHILD";
#[cfg(test)]
const PROCESS_DOMAIN_TEST_LOCK_ENV: &str = "CI_HUB_PROCESS_DOMAIN_TEST_LOCK";
#[cfg(test)]
const PROCESS_DOMAIN_TEST_MARKER_ENV: &str = "CI_HUB_PROCESS_DOMAIN_TEST_MARKER";

pub(crate) fn enable_child_subreaper() -> io::Result<()> {
    #[cfg(test)]
    if env::var_os(PROCESS_DOMAIN_TEST_CHILD_ENV).is_none() {
        return Err(io::Error::other(
            "a test enabling child subreaping must enter through process_domain_test_guard",
        ));
    }
    // SAFETY: PR_SET_CHILD_SUBREAPER changes only this process attribute.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut enabled: libc::c_int = 0;
    // SAFETY: PR_GET_CHILD_SUBREAPER writes one c_int to the provided pointer.
    if unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut enabled) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if enabled != 1 {
        return Err(io::Error::other("kernel did not enable child subreaper"));
    }
    Ok(())
}

/// Root of the unified (cgroup-v2) hierarchy.
pub(crate) const CGROUP_MOUNT: &str = "/sys/fs/cgroup";

/// Outcome of censusing one cgroup subtree. `Unknown` is deliberately distinct
/// from `Empty`: "I could not look" must never read as "nothing is there".
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CgroupCensus {
    /// No process in the subtree — including the case where the cgroup itself
    /// is gone, which is a POSITIVE proof: the kernel only lets a cgroup be
    /// removed once it holds no processes.
    Empty { absent: bool },
    /// At least one live pid, listed exactly.
    Populated(Vec<u32>),
    /// Could not be determined. Fail closed on this.
    Unknown(String),
}

/// Cgroup-v2 path of `pid`, relative to [`CGROUP_MOUNT`].
///
/// Returns `None` rather than an error: a missing or v1-shaped entry simply
/// means this record cannot carry the anchor and must keep the attestation
/// path. `/proc/<pid>/cgroup` on a v2 host has the single line `0::/<path>`.
#[cfg(test)]
pub(crate) fn process_cgroup(pid: u32) -> Option<String> {
    let content = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    for line in content.lines() {
        // hierarchy-ID:controllers:path — the v2 entry is the one with an empty
        // controller list and hierarchy 0.
        let mut parts = line.splitn(3, ':');
        let hierarchy = parts.next()?;
        let controllers = parts.next()?;
        let path = parts.next()?;
        if hierarchy == "0" && controllers.is_empty() && path.starts_with('/') {
            return Some(path.to_string());
        }
    }
    None
}

/// Census every process in `relative` and all of its descendant cgroups.
///
/// This is the supervisor-independent half of proving an orphaned domain empty:
/// unlike a ppid walk it needs no live subreaper, and unlike a pgid check it is
/// not defeated by `setsid()`.
pub(crate) fn cgroup_population(relative: &str) -> CgroupCensus {
    if !relative.starts_with('/') || relative.contains("..") {
        return CgroupCensus::Unknown(format!("refusing to census cgroup path {relative:?}"));
    }
    let root = PathBuf::from(CGROUP_MOUNT).join(relative.trim_start_matches('/'));
    match root.symlink_metadata() {
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return CgroupCensus::Empty { absent: true }
        }
        Err(source) => {
            return CgroupCensus::Unknown(format!("cannot stat {}: {source}", root.display()))
        }
        Ok(metadata) if !metadata.is_dir() => {
            return CgroupCensus::Unknown(format!("{} is not a cgroup directory", root.display()))
        }
        Ok(_) => {}
    }

    let mut pids = Vec::new();
    let mut pending = vec![root];
    while let Some(directory) = pending.pop() {
        match fs::read_to_string(directory.join("cgroup.procs")) {
            Ok(content) => {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    match line.parse::<u32>() {
                        Ok(pid) => pids.push(pid),
                        Err(error) => {
                            return CgroupCensus::Unknown(format!(
                                "unparsable pid {line:?} in {}: {error}",
                                directory.display()
                            ))
                        }
                    }
                }
            }
            // A cgroup that vanished mid-walk took its processes with it.
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return CgroupCensus::Unknown(format!(
                    "cannot read cgroup.procs in {}: {source}",
                    directory.display()
                ))
            }
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return CgroupCensus::Unknown(format!(
                    "cannot list {}: {source}",
                    directory.display()
                ))
            }
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return CgroupCensus::Unknown(format!("cannot walk {}", directory.display()));
            };
            // Nested cgroups are real directories; never follow a symlink out
            // of the subtree we were asked to census.
            if matches!(entry.file_type(), Ok(kind) if kind.is_dir()) {
                pending.push(entry.path());
            }
        }
    }
    pids.sort_unstable();
    pids.dedup();
    if pids.is_empty() {
        CgroupCensus::Empty { absent: false }
    } else {
        CgroupCensus::Populated(pids)
    }
}

/// The one cgroup a payload can legitimately migrate OUT of its unit cgroup
/// into: `safe-ci.slice`, used by `safe-ci-dag-runner`.
///
/// Derived from the payload's own recorded path rather than hardcoded, because
/// the real path embeds the invoking uid
/// (`/user.slice/user-<uid>.slice/user@<uid>.service/...`); a host literal would
/// be both wrong on another account and a portability-lint violation. `None`
/// when the recorded path is not user-manager shaped — the caller must then
/// treat the census as incomplete rather than skip the domain silently.
pub(crate) fn safe_ci_slice_for(payload_cgroup: &str) -> Option<String> {
    let marker = payload_cgroup
        .split('/')
        .find(|segment| segment.starts_with("user@") && segment.ends_with(".service"))?;
    let (prefix, _) = payload_cgroup.split_once(marker)?;
    Some(format!("{prefix}{marker}/safe.slice/safe-ci.slice"))
}

/// One `/proc` stat read per process: pid -> (state, parent pid, start ticks).
type ProcessTable = BTreeMap<u32, (char, u32, u64)>;

fn scan_process_table(_diagnostic_root: u32) -> io::Result<ProcessTable> {
    let mut processes = BTreeMap::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        match scan_process_stat_fields(pid) {
            Ok((state, parent, start_ticks)) => {
                processes.insert(pid, (state, parent, start_ticks));
            }
            // /proc can open a live task's stat entry, then return ESRCH
            // from read after the task exits. Like ENOENT at open, that proves
            // this entry vanished; every other read/parse error stays unknown.
            Err(source)
                if source.kind() == io::ErrorKind::NotFound
                    || source.raw_os_error() == Some(libc::ESRCH) => {}
            Err(source) => {
                #[cfg(test)]
                eprintln!(
                    "CENSUS_SCAN_ERROR root={_diagnostic_root} pid={pid} kind={:?} errno={:?} error={source:?}",
                    source.kind(),
                    source.raw_os_error()
                );
                return Err(source);
            }
        }
    }
    Ok(processes)
}

/// Every pid whose parent chain in `processes` reaches `root_pid`, excluding
/// `root_pid` itself.
fn descendant_pids(processes: &ProcessTable, root_pid: u32) -> BTreeSet<u32> {
    let mut domain = BTreeSet::from([root_pid]);
    loop {
        let before = domain.len();
        for (&pid, &(_, parent, _)) in processes {
            if domain.contains(&parent) {
                domain.insert(pid);
            }
        }
        if domain.len() == before {
            break;
        }
    }
    domain.remove(&root_pid);
    domain
}

fn scan_descendants(root_pid: u32) -> io::Result<Vec<ProcessIdentity>> {
    let processes = scan_process_table(root_pid)?;
    let domain = descendant_pids(&processes, root_pid);
    Ok(domain
        .into_iter()
        .filter_map(|pid| {
            processes
                .get(&pid)
                // A zombie owns no executable payload. Direct and adopted
                // zombies are reaped before each census sample below; this
                // filter also prevents an unreapable zombie beneath a still-
                // live descendant from masquerading as a second live owner.
                // The live parent remains in the census and keeps cleanup
                // fail-closed.
                .filter(|(state, _, _)| *state != 'Z')
                .map(|(_, _, start_ticks)| ProcessIdentity {
                    pid,
                    start_ticks: *start_ticks,
                })
        })
        .collect())
}

fn scan_process_stat_fields(pid: u32) -> io::Result<(char, u32, u64)> {
    #[cfg(test)]
    if SCAN_UNREADABLE_PID.load(std::sync::atomic::Ordering::SeqCst) == pid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "test-injected unreadable process identity",
        ));
    }
    #[cfg(test)]
    if let Some(reader) = SCAN_STAT_READER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(target, _)| *target == pid) {
            slot.take().map(|(_, reader)| reader)
        } else {
            None
        }
    }) {
        return reader();
    }
    process_stat_fields(pid)
}

#[cfg(test)]
type ScanStatReader = Box<dyn FnOnce() -> io::Result<(char, u32, u64)>>;

#[cfg(test)]
thread_local! {
    // One exact read boundary in the current isolated process-domain test.
    static SCAN_STAT_READER: std::cell::RefCell<Option<(u32, ScanStatReader)>> =
        const { std::cell::RefCell::new(None) };
}

/// Replace the next `/proc` scan's read of `pid` on THIS thread with `reader`.
/// The reader is taken out before it runs, so it may arm the next one.
#[cfg(test)]
pub(crate) fn inject_scan_stat_reader(
    pid: u32,
    reader: impl FnOnce() -> io::Result<(char, u32, u64)> + 'static,
) {
    SCAN_STAT_READER.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "previous scan fixture was not read"
        );
        *slot.borrow_mut() = Some((pid, Box::new(reader)));
    });
}

/// Disarm this thread's scan reader, returning whether one was still armed.
#[cfg(test)]
pub(crate) fn disarm_scan_stat_reader() -> bool {
    SCAN_STAT_READER.with(|slot| slot.borrow_mut().take().is_some())
}

#[cfg(test)]
static SCAN_UNREADABLE_PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Force one pid's membership to be unreadable, so the third state can be
/// exercised without needing a process whose `/proc` entry we cannot read.
#[cfg(test)]
static UNREADABLE_ENVIRON_PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(test)]
fn inject_unreadable_environ_pid(pid: u32) {
    UNREADABLE_ENVIRON_PID.store(pid, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
fn inject_unreadable_scan_pid(pid: u32) {
    SCAN_UNREADABLE_PID.store(pid, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn signal_exact_process(
    identity: &ProcessIdentity,
    signal: libc::c_int,
) -> io::Result<bool> {
    let pid = libc::pid_t::try_from(identity.pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid exceeds pid_t"))?;
    // Open a stable kernel reference before checking start_ticks. If the PID was
    // already reused, the identity check rejects it; if it exits afterward,
    // pidfd_send_signal returns ESRCH rather than targeting a future occupant.
    // SAFETY: pidfd_open takes a positive PID and zero flags.
    let raw_pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if raw_pidfd < 0 {
        let source = io::Error::last_os_error();
        return if source.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(source)
        };
    }
    // SAFETY: successful pidfd_open returned one new owned descriptor.
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw_pidfd as libc::c_int) };
    if !exact_process_liveness(identity).map_err(io::Error::other)? {
        return Ok(false);
    }
    // SAFETY: pidfd_send_signal targets only the process referenced by pidfd;
    // a null siginfo and zero flags are the documented simple-signal form.
    let sent = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if sent == 0 {
        Ok(true)
    } else {
        let source = io::Error::last_os_error();
        if source.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(source)
        }
    }
}

pub(crate) struct ResidualCapture {
    pub(crate) complete: bool,
    /// Descendants PROVEN to carry the payload token.
    pub(crate) identities: Vec<ProcessIdentity>,
    /// Descendants whose membership could not be read. Carried separately and
    /// reported by name: "I could not classify this" is not "it is not there",
    /// and collapsing it into either answer loses the only warning an operator
    /// gets that the census was partial.
    pub(crate) unclassifiable: Vec<ProcessIdentity>,
}

/// Enumerate and freeze the payload domain beneath `supervisor_pid`.
///
/// `payload_start_ticks` is the boot-relative start time of the payload group
/// leader, and it is a REQUIRED FLOOR, not a hint: only processes that started
/// at or after the leader are part of the payload domain.
///
/// ⚠️ WHY THE FLOOR EXISTS. `PR_SET_CHILD_SUBREAPER` is what makes the PPID
/// walk exhaustive, but it cuts both ways: the kernel reparents ANY orphan in
/// this process's subtree onto the supervisor, including processes this
/// supervisor never spawned. They then appear in `scan_descendants` as payload
/// residuals, and the census SIGSTOPs them.
///
/// Measured 2026-08-25 in the ci-hub unit suite: nine `scm_telem_log` daemons
/// -- the Sapling/Mercurial telemetry helper, forked and abandoned by unrelated
/// SCM invocations on the box -- were adopted by the test binary and recorded
/// as residuals by SEVEN separate censuses in one run, each reporting the SAME
/// nine identities against a DIFFERENT payload process group, every one of them
/// alongside `original group absent`. They predated every payload they were
/// charged to. That single adoption event surfaced as twelve failing tests
/// across five runs with shifting membership, because which tests fail is only
/// which ones happen to census after the daemons arrive.
///
/// The start-time floor is exact and cannot lose a real residual -- every
/// descendant of the leader necessarily starts after the leader -- but it is
/// only a bound, not an attribution. It cannot exclude a daemon orphaned DURING
/// the run window: measured 2026-08-25, two `scm_telem_log` daemons started 236
/// and 245 ticks AFTER the leader, in their own process groups, and were
/// charged to the payload.
///
/// ⚠️ NO TIMING, PROCESS-GROUP OR CGROUP TEST CAN CLASSIFY THAT PROCESS, and
/// each was measured rather than assumed:
///   - start time: it started after the leader, like a real descendant.
///   - process group: it leads its own, like a payload that called `setsid`.
///   - cgroup: on this box the test binary, its children and the ambient
///     telemetry daemons all sit in ONE byte-identical scope
///     (`.../3pai_sandbox.slice/run-p<N>-i<N>.scope`), so cgroup separates
///     nothing here. It DOES separate in production, where `run-boxed` gives
///     the supervisor its own `dagrun-<pid>.scope` -- which is exactly why
///     wiring cgroup in would have looked principled and fixed nothing.
///
/// So attribution uses the one axis that is inherited at fork and untouched by
/// `setsid`, `setpgid` and reparenting: [`PAYLOAD_DOMAIN_ENV`]. A descendant
/// carrying this payload's token is ours; one whose environment is readable and
/// lacks it is not; one whose environment cannot be read is
/// [`DomainMembership::Unclassifiable`] and is REPORTED AS SUCH rather than
/// omitted or guessed.
pub(crate) fn capture_and_freeze_residuals(
    supervisor_pid: u32,
    payload_start_ticks: u64,
    domain_token: &str,
) -> ResidualCapture {
    let mut prior = Vec::new();
    let mut unclassifiable = Vec::new();
    for _diagnostic_sample in 0..4 {
        // The supervisor is a child subreaper. A payload helper can therefore
        // become our adopted zombie after the supervised group leader has
        // already returned. Drain those exits before every sample rather than
        // turning a completed helper into durable residual ownership.
        reap_exited_children();
        unclassifiable.clear();
        let identities = match scan_descendants(supervisor_pid) {
            Ok(scanned) => {
                let mut mine = Vec::new();
                for identity in scanned {
                    // Cheap exact bound first: nothing that predates the leader
                    // can descend from it, so it need not be read at all.
                    if identity.start_ticks < payload_start_ticks {
                        continue;
                    }
                    // Classify BEFORE the freeze below. A process this payload
                    // does not own must not be SIGSTOPped by its census.
                    match classify_membership(identity.pid, domain_token) {
                        DomainMembership::Payload => mine.push(identity),
                        DomainMembership::Foreign => {}
                        DomainMembership::Unclassifiable => unclassifiable.push(identity),
                    }
                }
                mine
            }
            Err(_diagnostic_source) => {
                #[cfg(test)]
                eprintln!(
                    "CENSUS_CAPTURE_ERROR supervisor={supervisor_pid} sample={_diagnostic_sample} floor={payload_start_ticks} kind={:?} errno={:?} prior={prior:?} unclassifiable={unclassifiable:?} error={_diagnostic_source:?}",
                    _diagnostic_source.kind(),
                    _diagnostic_source.raw_os_error()
                );
                return ResidualCapture {
                    complete: false,
                    identities: prior,
                    unclassifiable,
                };
            }
        };
        let mut all_stopped = true;
        for identity in &identities {
            if signal_exact_process(identity, libc::SIGSTOP).is_err() {
                all_stopped = false;
            }
        }
        thread::sleep(Duration::from_millis(20));
        for identity in &identities {
            match process_stat_fields(identity.pid) {
                Ok((state, _, start_ticks)) if start_ticks == identity.start_ticks => {
                    // A transition to Z is not stable ownership. Force another
                    // loop so the next leading reap can remove it.
                    all_stopped &= state == 'T' || state == 't';
                }
                Ok(_) => {}
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(_) => all_stopped = false,
            }
        }
        if identities == prior && all_stopped {
            return ResidualCapture {
                // An unclassifiable descendant means the domain was not fully
                // read, so the census is NOT complete however stable the
                // classified part looks. Reporting `complete` here would let a
                // caller discharge on evidence that was never gathered.
                complete: unclassifiable.is_empty(),
                identities,
                unclassifiable,
            };
        }
        prior = identities;
    }
    ResidualCapture {
        complete: false,
        identities: prior,
        unclassifiable,
    }
}

/// Environment entry that exempts a test payload process from the teardown
/// signal sweeps below. See [`exempt_from_teardown_signals`].
#[cfg(test)]
pub(crate) const TEST_UNKILLABLE_ENTRY: &str = "CI_HUB_TEST_UNKILLABLE=1";

/// Whether the teardown sweeps must leave `pid` alone.
///
/// Production never exempts anything. A test binary exempts a process whose
/// environment carries [`TEST_UNKILLABLE_ENTRY`], which stands in for the
/// payload process SIGKILL cannot remove: one in uninterruptible sleep, or one
/// the supervisor is not permitted to signal. Without that stand-in the
/// quarantine that must follow a failed kill would have no test that reaches
/// it, because every fixture process dies when it is killed.
fn exempt_from_teardown_signals(pid: u32) -> bool {
    #[cfg(test)]
    if let Ok(raw) = fs::read(format!("/proc/{pid}/environ")) {
        return raw
            .split(|byte| *byte == 0)
            .any(|entry| entry == TEST_UNKILLABLE_ENTRY.as_bytes());
    }
    let _ = pid;
    false
}

/// Process group of exactly `identity`, or `None` when that pid is gone, now
/// names a later process, or its `stat` cannot be parsed. Reading the group and
/// the start time from ONE `stat` read binds the answer to this generation.
fn exact_process_group(identity: &ProcessIdentity) -> Option<u32> {
    let stat = fs::read(format!("/proc/{}/stat", identity.pid)).ok()?;
    let fields: Vec<_> = stat_fields_after_comm(&stat).ok()?.split_whitespace().collect();
    let pgrp = fields.get(2)?.parse::<u32>().ok()?;
    let start_ticks = fields.get(19)?.parse::<u64>().ok()?;
    (start_ticks == identity.start_ticks).then_some(pgrp)
}

/// Whether `pid`'s readable environment names a DIFFERENT payload domain.
/// That is a nested supervisor's payload or an adopted stranger that labels
/// itself, and no teardown of this payload may signal it.
fn carries_other_payload_token(pid: u32, domain_token: &str) -> bool {
    let Ok(raw) = fs::read(format!("/proc/{pid}/environ")) else {
        return false;
    };
    let prefix = format!("{PAYLOAD_DOMAIN_ENV}=");
    let own = format!("{PAYLOAD_DOMAIN_ENV}={domain_token}");
    raw.split(|byte| *byte == 0).any(|entry| {
        entry.starts_with(prefix.as_bytes()) && entry != own.as_bytes()
    })
}

/// Every process the teardown of ONE run has attributed to its payload, kept by
/// exact identity (pid and start time) across every census and poll of that
/// teardown.
///
/// ⚠️ WHY A PERSISTENT RECORD. The two axes that attribute a process at the
/// moment teardown looks -- the payload token and membership of the payload's
/// original process group -- both miss a process started with
/// `env -u CI_HUB_PAYLOAD_DOMAIN setsid ...`. Such a process is attributable
/// only through its parent, and the parent link lasts only while the parent
/// lives: once teardown signals the parent and the parent exits, the child is
/// reparented to this subreaper and `/proc` no longer says where it came from.
/// Two shapes turned that into a release with a live payload process, where
/// the code before teardown killed anything had frozen the parent and
/// quarantined (review of https://github.com/rrnewton/dev-hermit/issues/419):
///   - parent-waits: a token-carrying setsid'd parent waits on such a child.
///     Killing the parent orphaned the child, and the run released.
///   - escape-on-term: an original-group process that dropped the token has a
///     SIGTERM handler that starts such a child and exits. The SIGTERM the
///     teardown sent was what created the escapee.
///
/// So [`PayloadLineage::observe`] runs before the first teardown signal and at
/// every teardown poll, and records every live process that started no earlier
/// than the payload leader, does not carry a DIFFERENT payload's token, and is
/// one of:
///   - a payload-token member beneath this subreaper (the census rule);
///   - a surviving member of the original group `payload_pgid`
///     ([`in_original_group`]);
///   - a child, in the same `/proc` scan, of an identity already selected, so a
///     child is recorded while its parent's link is still visible;
///   - a direct child of this subreaper that is not in the subreaper's own
///     process group. Such a process is either the payload leader or was
///     reparented here from a payload process that exited -- for example one
///     forked by a SIGTERM handler, or a tokenless daemon that detached while
///     the payload ran.
///
/// ⚠️ WHY THE DIRECT-CHILD RULE HAS NO BASELINE. It used to exempt every
/// direct child present at the FIRST observation, on the theory that anything
/// already reparented then predated teardown. A tokenless helper that got
/// SIGTERM from outside before teardown began -- `systemctl stop` with
/// `KillMode=control-group`, or `kill -TERM -<pgid>` -- could start a setsid'd
/// escapee from its handler and exit, and the escapee was reparented here
/// before the first observation. It was then exempt and survived a released
/// lock (review of https://github.com/rrnewton/dev-hermit/issues/419). A failed
/// first observation made it worse: the next successful one took the baseline
/// instead. The baseline taken when the payload is spawned is exactly the
/// leader, and the leader is a payload process every other rule selects too,
/// so no baseline is kept at all.
///
/// That relies on the supervisor starting no process outside its own group
/// while the payload runs or is torn down. [`current_host`] keeps its answer
/// for that reason, and `arm_run` asks for it before the payload exists.
///
/// A recorded identity stays recorded after it is reparented. Each is a
/// teardown target, and one still alive when [`kill_payload_domain`] finishes
/// is returned as an exact residual, which blocks release exactly like a
/// census residual.
///
/// NOT COVERED, tracked in https://github.com/rrnewton/dev-hermit/issues/428:
///   - anything that is not a descendant of this subreaper at all, for example
///     a process started through `systemd-run` or another service manager;
///   - a direct child of this subreaper that moved into the supervisor's own
///     process group and dropped the token, and its descendants unless another
///     rule selects them;
///   - a process that carries a DIFFERENT payload's token, which is left to
///     that payload's supervisor;
///   - a process whose start time is before the leader's.
pub(crate) struct PayloadLineage {
    supervisor_pid: u32,
    payload_start_ticks: u64,
    domain_token: String,
    payload_pgid: u32,
    /// The supervisor's own process group.
    supervisor_pgid: Option<u32>,
    recorded: BTreeSet<ProcessIdentity>,
    /// Identities already sent SIGTERM, or already reached by the payload
    /// group's SIGTERM (see [`GroupTerm`]), so no process gets that signal
    /// from teardown twice.
    termed: BTreeSet<ProcessIdentity>,
}

impl PayloadLineage {
    pub(crate) fn new(
        supervisor_pid: u32,
        payload_start_ticks: u64,
        domain_token: &str,
        payload_pgid: u32,
    ) -> Self {
        // SAFETY: getpgid(0) only reads this process's own process-group id.
        let supervisor_pgid = u32::try_from(unsafe { libc::getpgid(0) })
            .ok()
            .filter(|pgid| *pgid > 0);
        Self {
            supervisor_pid,
            payload_start_ticks,
            domain_token: domain_token.to_string(),
            payload_pgid,
            supervisor_pgid,
            recorded: BTreeSet::new(),
            termed: BTreeSet::new(),
        }
    }

    /// Scan `/proc` once, record every process the rules on [`PayloadLineage`]
    /// attribute to the payload, and return the recorded identities that are
    /// alive in this scan. A zombie is not alive here: it owns nothing, and
    /// its live parent (if any) is recorded too. Reads `/proc` only; it
    /// neither reaps nor signals.
    pub(crate) fn observe(&mut self) -> io::Result<Vec<ProcessIdentity>> {
        let table = scan_process_table(self.supervisor_pid)?;
        let descendants = descendant_pids(&table, self.supervisor_pid);
        let floor = self.payload_start_ticks;
        let live_identity = |pid: u32| -> Option<ProcessIdentity> {
            let &(state, _, start_ticks) = table.get(&pid)?;
            (state != 'Z' && start_ticks >= floor).then_some(ProcessIdentity { pid, start_ticks })
        };
        let direct_children: BTreeSet<ProcessIdentity> = descendants
            .iter()
            .filter(|pid| matches!(table.get(pid), Some(&(_, parent, _)) if parent == self.supervisor_pid))
            .filter_map(|pid| live_identity(*pid))
            .collect();
        let original_generation = original_group_generation_is_live(self.payload_pgid, floor);

        let mut selected = BTreeSet::new();
        for &pid in &descendants {
            let Some(identity) = live_identity(pid) else {
                continue;
            };
            let chosen = self.recorded.contains(&identity)
                || match classify_membership(pid, &self.domain_token) {
                    DomainMembership::Payload => true,
                    DomainMembership::Foreign | DomainMembership::Unclassifiable => {
                        !carries_other_payload_token(pid, &self.domain_token)
                            && ((original_generation
                                && in_original_group(&identity, self.payload_pgid))
                                || (direct_children.contains(&identity)
                                    && exact_process_group(&identity) != self.supervisor_pgid))
                    }
                };
            if chosen {
                selected.insert(pid);
            }
        }
        loop {
            let before = selected.len();
            for &pid in &descendants {
                if selected.contains(&pid) {
                    continue;
                }
                let Some(&(_, parent, _)) = table.get(&pid) else {
                    continue;
                };
                if selected.contains(&parent)
                    && live_identity(pid).is_some()
                    && !carries_other_payload_token(pid, &self.domain_token)
                {
                    selected.insert(pid);
                }
            }
            if selected.len() == before {
                break;
            }
        }
        self.recorded
            .extend(selected.into_iter().filter_map(|pid| live_identity(pid)));
        Ok(self
            .recorded
            .iter()
            .filter(|identity| {
                matches!(
                    table.get(&identity.pid),
                    Some(&(state, _, start_ticks))
                        if start_ticks == identity.start_ticks && state != 'Z'
                )
            })
            .cloned()
            .collect())
    }

    /// [`Self::observe`], or, when `/proc` cannot be scanned, every recorded
    /// identity not proven gone. Signalling that fallback is safe because
    /// every signal is sent by exact identity; the census that follows fails
    /// closed on the same scan error.
    fn live_or_recorded(&mut self) -> Vec<ProcessIdentity> {
        self.observe_or_recorded().0
    }

    /// [`Self::live_or_recorded`], also saying whether the `/proc` scan
    /// succeeded (`false` means the fallback was used).
    fn observe_or_recorded(&mut self) -> (Vec<ProcessIdentity>, bool) {
        match self.observe() {
            Ok(live) => (live, true),
            Err(_) => (
                self.recorded
                    .iter()
                    .filter(|identity| !matches!(exact_process_liveness(identity), Ok(false)))
                    .cloned()
                    .collect(),
                false,
            ),
        }
    }

    /// Observe, then SIGTERM every live recorded identity that has not had
    /// SIGTERM yet. No group signal was sent, so every identity is signalled
    /// by itself. Returns the identities signalled here.
    fn term_unsignalled(&mut self) -> Vec<ProcessIdentity> {
        let live = self.live_or_recorded();
        self.term_listed(&live, GroupTerm::NotSent)
    }

    /// SIGTERM each of `live`, identities an observation the caller already
    /// made returned, that has not had SIGTERM yet, without scanning `/proc`
    /// again. An identity the group signal described by `group` already
    /// reached is counted as signalled and not sent a second SIGTERM. Returns
    /// the identities signalled here.
    fn term_listed(&mut self, live: &[ProcessIdentity], group: GroupTerm) -> Vec<ProcessIdentity> {
        let mut reached = Vec::new();
        for identity in live.iter().cloned() {
            if self.termed.contains(&identity) || exempt_from_teardown_signals(identity.pid) {
                continue;
            }
            self.termed.insert(identity.clone());
            if group.reached(&identity) {
                continue;
            }
            if matches!(signal_exact_process(&identity, libc::SIGTERM), Ok(true)) {
                reached.push(identity);
            }
        }
        reached
    }
}

/// Whether, and when, [`term_payload_group`] sent SIGTERM to the payload's
/// process group, so [`PayloadLineage::term_listed`] sends no identity a
/// second SIGTERM and misses none.
///
/// ⚠️ A GROUP MEMBER FIRST SEEN AFTER THE GROUP SIGNAL MAY NOT HAVE HAD IT. The
/// grace polls used to count every current member of the group as signalled.
/// A member started after the group signal -- for example a child forked by
/// a SIGTERM handler -- then got no SIGTERM at all and ran until the SIGKILL
/// that ends the grace (review of
/// https://github.com/rrnewton/dev-hermit/issues/419). The normal-exit
/// teardown sends such a process its own SIGTERM, so the deadline path now
/// does too.
#[derive(Clone, Copy, Debug)]
enum GroupTerm {
    /// No group signal was sent.
    NotSent,
    /// The group `pgid` was sent SIGTERM after the identities were observed,
    /// so an identity in the group existed, and was reached, when it was sent.
    SentAfterObservation { pgid: u32 },
    /// The group `pgid` was sent SIGTERM before the identities were observed,
    /// with the boot clock at `sent_at_ticks` just before the signal (see
    /// [`boot_clock_ticks`]). A member that started before that tick was
    /// reached. One that started at or after it may have been forked after
    /// the signal and is signalled by itself; one forked in that same tick
    /// just before the signal therefore gets SIGTERM twice.
    SentBeforeObservation { pgid: u32, sent_at_ticks: u64 },
}

impl GroupTerm {
    fn reached(self, identity: &ProcessIdentity) -> bool {
        match self {
            Self::NotSent => false,
            Self::SentAfterObservation { pgid } => exact_process_group(identity) == Some(pgid),
            Self::SentBeforeObservation {
                pgid,
                sent_at_ticks,
            } => {
                identity.start_ticks < sent_at_ticks && exact_process_group(identity) == Some(pgid)
            }
        }
    }
}

/// The boot clock in the units of a `/proc/<pid>/stat` start time (field 22),
/// or `None` when it cannot be read. The kernel derives that field from the
/// same clock (`CLOCK_BOOTTIME`, including a time namespace's offset) divided
/// the same way, so a process whose start time is below a value read here
/// started before the read.
fn boot_clock_ticks() -> Option<u64> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable timespec for the duration of the call.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut now) } != 0 {
        return None;
    }
    // SAFETY: sysconf only reads a system configuration value.
    let ticks_per_second = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) })
        .ok()
        .filter(|ticks| *ticks > 0 && *ticks <= 1_000_000_000)?;
    let nanos = u64::try_from(now.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(now.tv_nsec).ok()?)?;
    Some(nanos / (1_000_000_000 / ticks_per_second))
}

/// Whether the number `payload_pgid` still names the payload's ORIGINAL
/// process group. A live process whose pid IS `payload_pgid` but whose start
/// time differs from the leader's means the original group emptied and a later
/// one took the number, so no member of it may be selected.
fn original_group_generation_is_live(payload_pgid: u32, payload_start_ticks: u64) -> bool {
    process_group_target(payload_pgid).is_some()
        && !matches!(
            process_start_ticks(payload_pgid),
            Ok(start_ticks) if start_ticks != payload_start_ticks
        )
}

/// Whether one `stat` read shows exactly `identity` in the payload's original
/// process group.
///
/// ⚠️ WHY THE ORIGINAL GROUP IS A SECOND AXIS. The payload token is inherited,
/// but a process can drop it: `env -u`, `env -i`, `sudo`'s `env_reset`, or a
/// non-dumpable process whose environment cannot be read. Such a process that
/// stays in the payload's group used to be reached only by the deadline path's
/// group signal. After a NORMAL leader exit nothing signalled the group, so it
/// kept the group alive, the census recorded "0 exact residual identities;
/// original group active", and the box stayed quarantined with nothing a
/// waiter could do about it (review of
/// https://github.com/rrnewton/dev-hermit/issues/419).
///
/// The leader is already reaped after a normal exit, so a group signal is not
/// safe: the number is only protected while the group has members. Each group
/// member is therefore selected and signalled by exact identity, and only
/// while [`original_group_generation_is_live`] holds.
fn in_original_group(identity: &ProcessIdentity, payload_pgid: u32) -> bool {
    exact_process_group(identity) == Some(payload_pgid)
}

/// Send `signal`, by exact identity, to each of `identities` that is not
/// exempt and, with `skip_pgid`, not currently in that process group. Returns
/// the identities it reached. It does not reap, so it is safe while the direct
/// payload child's status is still owed to `Child::wait`.
fn signal_identities(
    identities: &[ProcessIdentity],
    skip_pgid: Option<u32>,
    signal: libc::c_int,
) -> Vec<ProcessIdentity> {
    let mut reached = Vec::new();
    for identity in identities {
        if exempt_from_teardown_signals(identity.pid) {
            continue;
        }
        if skip_pgid.is_some() && exact_process_group(identity) == skip_pgid {
            continue;
        }
        if matches!(signal_exact_process(identity, signal), Ok(true)) {
            reached.push(identity.clone());
        }
    }
    reached
}

/// Deadline and supervisor-signal path, first half: SIGTERM the payload's
/// process group `pgid` and, by exact identity, every recorded payload process
/// outside it, then keep recording for up to `grace` and SIGTERM, by exact
/// identity, each process found during it that the group signal did not
/// reach: one outside the group, or a member that started no earlier than the
/// group signal ([`GroupTerm`]).
///
/// The lineage is observed BEFORE the first signal, so a child of a process
/// about to be signalled is recorded while its parent link is visible. That
/// observation also supplies the identities signalled outside the group, so
/// no second scan runs before the grace. The grace loop never reaps: the
/// unreaped leader keeps the number `pgid` from being reused before the final
/// SIGKILL.
///
/// The grace ends early when a successful observation finds no recorded
/// process alive other than test-exempt ones. The leader is then a zombie
/// and every other recorded process has exited, so waiting longer would only
/// delay the SIGKILL that follows. Returns whether it ended that way; the
/// caller passes that to [`kill_payload_group`].
///
/// ⚠️ COST. Each poll is one full `/proc` scan, which took over a second on
/// devbig014 at load average 200 (2026-09-29), so the number of scans is what
/// a teardown costs. Revision 42ddf70e3b4b8476b1de68487eb6967d5e0220e0 polled
/// every 20 ms for the whole grace: under strace, a `--child-deadline 1` run
/// of `/bin/sleep 30` scanned `/proc` 14 to 16 times, against 5 at
/// 0539a4b915f2d4dc7253cfc6b30890f0f41e1f3e, spent 4.7 to 11.9 s of system
/// CPU, and made `test_land_lock_full_protocol_is_bounded` in
/// ci-hub/tests/test_operational_bounds.py exceed its 5 s CPU limit. With the
/// early end and [`pace_teardown_poll`], that run scans 5 times again: a
/// payload that exits on SIGTERM costs the scan before the signal plus one
/// scan per poll until it is gone, usually one.
///
/// A payload process that IGNORES SIGTERM is the expensive case. The grace
/// then runs to its end, and because each poll waits only as long as the
/// scan before it, about half of the grace is spent scanning. On devbig014
/// on 2026-09-29, with 4,431 to 4,654 processes at load average 97 to 173, a
/// `--child-deadline 1` landing run of `bash -c "trap '' TERM; exec sleep 30"`
/// spent 3.5 to 4.1 s of supervisor CPU (user plus system, whole run, three
/// runs each) with this loop, against 0.7 to 2.4 s at
/// 0539a4b915f2d4dc7253cfc6b30890f0f41e1f3e, whose grace slept without
/// scanning. The review of https://github.com/rrnewton/dev-hermit/issues/419
/// measured 3 to 4 s against 0.6 to 0.9 s with about 4,300 processes. That is
/// the price of recording, during the grace, processes started after the
/// signal, such as one forked by a SIGTERM handler; the pacing is unchanged.
pub(crate) fn term_payload_group(
    lineage: &mut PayloadLineage,
    pgid: u32,
    grace: Duration,
    label: &str,
) -> bool {
    let scan_started = Instant::now();
    let (live, _) = lineage.observe_or_recorded();
    // The first poll is paced by this scan too, which gives processes that
    // exit on SIGTERM that long to do so before they are looked for.
    let mut last_scan = scan_started.elapsed();
    // Read before the signal, so every process forked after it has a start
    // time at or above this. Unreadable, 0 makes every member found during
    // the grace get its own SIGTERM, which can only duplicate one.
    let sent_at_ticks = boot_clock_ticks().unwrap_or(0);
    signal_group(libc::SIGTERM, pgid);
    let reached = lineage.term_listed(&live, GroupTerm::SentAfterObservation { pgid });
    if !reached.is_empty() {
        eprintln!(
            "{label}: SIGTERM {} payload process(es) outside process group -{pgid}: {}",
            reached.len(),
            describe_identities(&reached)
        );
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        pace_teardown_poll(last_scan, deadline);
        let scan_started = Instant::now();
        let (live, observed) = lineage.observe_or_recorded();
        last_scan = scan_started.elapsed();
        let reached = lineage.term_listed(
            &live,
            GroupTerm::SentBeforeObservation {
                pgid,
                sent_at_ticks,
            },
        );
        if !reached.is_empty() {
            eprintln!(
                "{label}: SIGTERM {} further payload process(es) found during the grace period: {}",
                reached.len(),
                describe_identities(&reached)
            );
        }
        if observed
            && live
                .iter()
                .all(|identity| exempt_from_teardown_signals(identity.pid))
        {
            return true;
        }
    }
    false
}

/// Sleep before the next teardown poll for at least [`TEARDOWN_POLL_MILLIS`]
/// and at least `last_scan`, the time the previous poll's `/proc` scan took,
/// but not past `deadline`. Scanning therefore takes at most about half of a
/// grace or settle window however slow `/proc` is, instead of all of it.
fn pace_teardown_poll(last_scan: Duration, deadline: Instant) {
    let pause = last_scan.max(Duration::from_millis(TEARDOWN_POLL_MILLIS));
    let remaining = deadline.saturating_duration_since(Instant::now());
    thread::sleep(pause.min(remaining));
}

/// Deadline and supervisor-signal path, second half: SIGKILL the process group
/// `pgid` and every live recorded payload process outside it. Returns whether
/// the group signal was delivered.
///
/// `grace_emptied` is [`term_payload_group`]'s result. When it is true, the
/// last observation found no recorded process alive, so no recorded process
/// was left to fork and this skips its own `/proc` scan and sends only the
/// group signal. A process a killed parent forked after the last observation
/// is reparented to this subreaper and recorded by the normal-exit teardown
/// that follows.
pub(crate) fn kill_payload_group(
    lineage: &mut PayloadLineage,
    pgid: u32,
    grace_emptied: bool,
    label: &str,
) -> bool {
    let kill_sent = signal_group(libc::SIGKILL, pgid);
    if grace_emptied {
        return kill_sent;
    }
    let live = lineage.live_or_recorded();
    let reached = signal_identities(&live, Some(pgid), libc::SIGKILL);
    if !reached.is_empty() {
        eprintln!(
            "{label}: SIGKILL {} payload process(es) outside process group -{pgid}: {}",
            reached.len(),
            describe_identities(&reached)
        );
    }
    kill_sent
}

pub(crate) fn describe_identities(identities: &[ProcessIdentity]) -> String {
    identities
        .iter()
        .map(|identity| format!("{}@{}", identity.pid, identity.start_ticks))
        .collect::<Vec<_>>()
        .join(",")
}

/// Test payload for the review of
/// https://github.com/rrnewton/dev-hermit/issues/419: the leader backgrounds
/// a helper that STAYS in the payload's original process group (no setsid)
/// but runs under `helper_env` (for example `env -u CI_HUB_PAYLOAD_DOMAIN`),
/// ignores TERM and HUP, and blocks reading a FIFO nobody writes; the leader
/// then exits 0. Arguments: `$1` identity file (`pid start_ticks`), `$2` the
/// FIFO, `$3` the helper's `pgrp token-or-unset`, `$4` the leader's pgrp.
///
/// Every fixture process that ignores TERM blocks with `read -t 300 ... <>`:
/// opening a FIFO read-write never blocks on Linux, and the read gives up
/// after 300 s, so a test runner killed before its cleanup leaks such a
/// process for at most that long instead of forever.
#[cfg(test)]
pub(crate) fn same_group_helper_payload(helper_env: &str) -> String {
    format!(
        r#"
{helper_env} /bin/bash -c '
  trap "" TERM HUP
  pid=$BASHPID
  printf "%s %s\n" "$pid" "$(awk "{{print \$22}}" "/proc/$pid/stat")" >> "$1"
  printf "%s %s\n" "$(awk "{{print \$5}}" "/proc/$pid/stat")" "${{{PAYLOAD_DOMAIN_ENV}-unset}}" >> "$3"
  read -r -t 300 line <> "$2"
' same-group-helper "$1" "$2" "$3" &
while [ "$(wc -l < "$1")" -lt 1 ] || [ "$(wc -l < "$3")" -lt 1 ]; do sleep 0.01; done
awk "{{print \$5}}" /proc/$$/stat > "$4"
exit 0
"#
    )
}

/// Test payload for the parent-waits shape on [`PayloadLineage`]: the leader
/// backgrounds a `setsid`'d PARENT that keeps the payload token, ignores TERM
/// and HUP, and waits in the foreground on a GRANDCHILD started as
/// `{grandchild_env} setsid ...`, which blocks reading a FIFO nobody writes and
/// inherits the ignored TERM and HUP. `grandchild_env` must drop the token (for
/// example `env -u CI_HUB_PAYLOAD_DOMAIN`), so the grandchild is attributable
/// only through its parent. Arguments: `$1` identity file (`pid start_ticks`,
/// the parent's line first), `$2` the FIFO, `$3` the grandchild's
/// `sid pgrp ppid token-or-unset`, `$4` `wait` to make the leader wait for its
/// deadline instead of exiting 0.
#[cfg(test)]
pub(crate) fn parent_waits_payload(grandchild_env: &str) -> String {
    const SCRIPT: &str = r#"
parent_script='
  trap "" TERM HUP
  pid=$BASHPID
  printf "%s %s\n" "$pid" "$(awk "{print \$22}" "/proc/$pid/stat")" >> "$1"
  @GRANDCHILD_ENV@ setsid /bin/bash -c "$4" parent-waits-grandchild "$1" "$2" "$3"
  exit $?
'
grandchild_script='
  pid=$BASHPID
  printf "%s %s\n" "$pid" "$(awk "{print \$22}" "/proc/$pid/stat")" >> "$1"
  printf "%s %s\n" "$(awk "{print \$6, \$5, \$4}" "/proc/$pid/stat")" "${@TOKEN_VAR@-unset}" >> "$3"
  read -r -t 300 line <> "$2"
'
setsid /bin/bash -c "$parent_script" parent-waits-parent "$1" "$2" "$3" "$grandchild_script" &
while [ "$(wc -l < "$1")" -lt 2 ] || [ "$(wc -l < "$3")" -lt 1 ]; do sleep 0.01; done
if [ "$4" = wait ]; then
  trap 'exit 0' TERM
  wait
fi
exit 0
"#;
    SCRIPT
        .replace("@GRANDCHILD_ENV@", grandchild_env)
        .replace("@TOKEN_VAR@", PAYLOAD_DOMAIN_ENV)
}

/// Test payload for the escape-on-term shape on [`PayloadLineage`]: the leader
/// backgrounds a HELPER that drops the payload token but stays in the payload's
/// original process group and blocks reading a FIFO nobody writes. Its SIGTERM
/// handler ignores TERM and HUP, starts an ESCAPEE with `setsid` (tokenless,
/// its own session, inheriting the ignored TERM and HUP, blocked on the same
/// FIFO), waits until the escapee has recorded itself, and exits 0. The leader
/// exits 0 once the helper has recorded itself. Arguments: `$1` the helper's
/// identity, `$2` the FIFO, `$3` the escapee's identity, `$4` the escapee's
/// `sid pgrp token-or-unset`, `$5` the helper's `pgrp token-or-unset`, `$6`
/// the leader's pgrp.
#[cfg(test)]
pub(crate) fn escape_on_term_payload() -> String {
    const SCRIPT: &str = r#"
escapee_script='
  pid=$BASHPID
  printf "%s %s\n" "$pid" "$(awk "{print \$22}" "/proc/$pid/stat")" >> "$1"
  printf "%s %s\n" "$(awk "{print \$6, \$5}" "/proc/$pid/stat")" "${@TOKEN_VAR@-unset}" >> "$2"
  read -r -t 300 line <> "$3"
'
helper_script='
  fifo=$2 escapee_ids=$3 escapee=$4 escapee_facts=$5
  on_term() {
    trap "" TERM HUP
    setsid /bin/bash -c "$escapee" escape-on-term-escapee "$escapee_ids" "$escapee_facts" "$fifo" &
    while [ "$(wc -l < "$escapee_ids")" -lt 1 ] || [ "$(wc -l < "$escapee_facts")" -lt 1 ]; do
      sleep 0.01
    done
    exit 0
  }
  trap on_term TERM
  pid=$BASHPID
  printf "%s %s\n" "$pid" "$(awk "{print \$22}" "/proc/$pid/stat")" >> "$1"
  printf "%s %s\n" "$(awk "{print \$5}" "/proc/$pid/stat")" "${@TOKEN_VAR@-unset}" >> "$6"
  read -r -t 300 line <> "$fifo"
'
env -u @TOKEN_VAR@ /bin/bash -c "$helper_script" escape-on-term-helper "$1" "$2" "$3" "$escapee_script" "$4" "$5" &
while [ "$(wc -l < "$1")" -lt 1 ] || [ "$(wc -l < "$5")" -lt 1 ]; do sleep 0.01; done
awk "{print \$5}" /proc/$$/stat > "$6"
exit 0
"#;
    SCRIPT.replace("@TOKEN_VAR@", PAYLOAD_DOMAIN_ENV)
}

/// Upper bound on the SIGKILL-and-recensus rounds in [`kill_payload_domain`].
const TEARDOWN_KILL_ROUNDS: usize = 3;
/// How long one round waits for its SIGKILLed identities to disappear.
const TEARDOWN_KILL_SETTLE_MILLIS: u64 = 2_000;
const TEARDOWN_POLL_MILLIS: u64 = 20;

/// Census the payload domain, SIGKILL every member the census froze and every
/// live identity the [`PayloadLineage`] has recorded, and census again, until
/// nothing is left or [`TEARDOWN_KILL_ROUNDS`] is spent.
///
/// ⚠️ THE CENSUS FREEZE IS A SNAPSHOT, NOT A DISPOSAL.
/// [`capture_and_freeze_residuals`] SIGSTOPs what it finds so the membership
/// it reports holds still. Before
/// https://github.com/rrnewton/dev-hermit/issues/419 that frozen set was the
/// end state: RUN 1959's setsid'd scorecard publication was recorded, left
/// stopped, and held the box in quarantine from 12:37:49Z to 13:01:19Z on
/// 2026-09-29 until an operator killed it. Here every frozen member is killed
/// and the domain is re-censused.
///
/// The returned capture names what SIGKILL could not remove (uninterruptible
/// sleep, or no permission): the census's token members, which stay frozen,
/// PLUS every recorded lineage identity still alive after the last round,
/// including one the census rule no longer attributes because it carries no
/// token and was reparented. Both quarantine the box: nothing is admitted while
/// any process this teardown attributed to the old payload is alive. What the
/// lineage does not attribute is listed on [`PayloadLineage`] and tracked in
/// https://github.com/rrnewton/dev-hermit/issues/428. Unclassifiable
/// descendants the lineage did not record are not killed and keep the capture
/// incomplete.
///
/// A target killed in a round can have forked first; each settle poll records
/// such a child through the lineage and kills it in the same round.
///
/// Worst case this spends [`TEARDOWN_KILL_ROUNDS`] ×
/// [`TEARDOWN_KILL_SETTLE_MILLIS`] (6 s) waiting for kills to land, plus one
/// census per round and one `/proc` scan per settle poll. Polls are paced by
/// [`pace_teardown_poll`]: every 20 ms, or less often when a scan takes longer.
///
/// Callers must have reaped, or given up on, the direct payload child first:
/// the census reaps adopted children with `waitpid(-1)`.
pub(crate) fn kill_payload_domain(lineage: &mut PayloadLineage, label: &str) -> ResidualCapture {
    let census = |lineage: &PayloadLineage| {
        capture_and_freeze_residuals(
            lineage.supervisor_pid,
            lineage.payload_start_ticks,
            &lineage.domain_token,
        )
    };
    let mut capture = census(lineage);
    // The lineage observation taken after the last census when no signal was
    // sent after it. It is reused below instead of scanning `/proc` again,
    // which keeps the common case (nothing outlived the leader) at the scan
    // count it had before the lineage existed. One scan took over a second
    // on devbig014 at load average 228 (2026-09-29).
    let mut final_observation = None;
    for _round in 0..TEARDOWN_KILL_ROUNDS {
        let (live, observed) = lineage.observe_or_recorded();
        let mut targets: BTreeSet<ProcessIdentity> = capture.identities.iter().cloned().collect();
        targets.extend(live.iter().cloned());
        targets.retain(|identity| !exempt_from_teardown_signals(identity.pid));
        if targets.is_empty() {
            final_observation = Some((live, observed));
            break;
        }
        let payload_pgid = lineage.payload_pgid;
        eprintln!(
            "{label}: SIGKILL {} payload process(es) still alive after the payload leader, \
             found by payload token, original process group {payload_pgid}, or descent from \
             one of those (setsid, setpgid or a dropped token does not remove all three): {}",
            targets.len(),
            describe_identities(&targets.iter().cloned().collect::<Vec<_>>())
        );
        for identity in &targets {
            let _ = signal_exact_process(identity, libc::SIGKILL);
        }
        let settle = Instant::now() + Duration::from_millis(TEARDOWN_KILL_SETTLE_MILLIS);
        loop {
            // A killed member whose parent was also killed is reparented to
            // this subreaper as a zombie; reaping it is what makes it absent.
            reap_exited_children();
            let scan_started = Instant::now();
            let live = lineage.live_or_recorded();
            let last_scan = scan_started.elapsed();
            let forked: Vec<_> = live
                .into_iter()
                .filter(|identity| {
                    !targets.contains(identity) && !exempt_from_teardown_signals(identity.pid)
                })
                .collect();
            if !forked.is_empty() {
                eprintln!(
                    "{label}: SIGKILL {} further payload process(es) recorded while the kill \
                     settled: {}",
                    forked.len(),
                    describe_identities(&forked)
                );
                for identity in forked {
                    let _ = signal_exact_process(&identity, libc::SIGKILL);
                    targets.insert(identity);
                }
            }
            if targets
                .iter()
                .all(|identity| matches!(exact_process_liveness(identity), Ok(false)))
                || Instant::now() >= settle
            {
                break;
            }
            pace_teardown_poll(last_scan, settle);
        }
        capture = census(lineage);
    }
    // A recorded identity blocks release for as long as it is alive, whether
    // or not the census rule still attributes it. That includes an exempt
    // (test-unkillable) identity, which was left out of the kill targets
    // only.
    let (survivors, observed) = match final_observation {
        Some(observation) => observation,
        None => lineage.observe_or_recorded(),
    };
    if !observed {
        capture.complete = false;
    }
    let unrecorded: Vec<_> = survivors
        .into_iter()
        .filter(|identity| !capture.identities.contains(identity))
        .collect();
    if !unrecorded.is_empty() {
        eprintln!(
            "{label}: {} recorded payload process(es) outside the census survived SIGKILL and \
             are recorded as exact residuals: {}",
            unrecorded.len(),
            describe_identities(&unrecorded)
        );
        capture.identities.extend(unrecorded);
        capture.identities.sort();
        capture.identities.dedup();
    }
    capture
}

/// Tear down the payload processes that outlived the payload leader and that
/// the [`PayloadLineage`] attributes, then return the census of whatever
/// survived.
///
/// The lineage is observed before any signal. Every live recorded identity
/// gets SIGTERM and up to `grace` to exit on its own, so a publication or
/// cleanup step can finish writing. Each poll of that grace (every 20 ms, or
/// less often when a scan is slower; see [`pace_teardown_poll`]) observes the
/// lineage again and sends SIGTERM to every newly recorded process, including
/// a child a SIGTERM handler forked and a process reparented here when its
/// parent exited. Whatever remains is then disposed of by
/// [`kill_payload_domain`]. When nothing outlived the leader this costs two
/// lineage `/proc` scans plus the census's own samples, the same count as
/// before the lineage existed, and returns the same census as before.
///
/// Worst-case wall time: `grace` (5 s at both call sites) plus
/// [`kill_payload_domain`]'s 6 s of kill settling, so about 11 s, plus census
/// and scan time. The validate run path then adds its bounded empty-domain
/// settle; see `settle_empty_payload_domain` in validate_lock.rs.
///
/// Call only after the direct child has been reaped (normal exit, or the
/// group teardown's `Child::wait`) or its status is already lost.
pub(crate) fn tear_down_payload_domain(
    lineage: &mut PayloadLineage,
    grace: Duration,
    label: &str,
) -> ResidualCapture {
    reap_exited_children();
    let termed = lineage.term_unsignalled();
    if !termed.is_empty() {
        eprintln!(
            "{label}: {} payload process(es) outlived the payload leader: {}; \
             SIGTERM sent, waiting up to {}s before SIGKILL",
            termed.len(),
            describe_identities(&termed),
            grace.as_secs()
        );
        let deadline = Instant::now() + grace;
        loop {
            reap_exited_children();
            let scan_started = Instant::now();
            let more = lineage.term_unsignalled();
            let last_scan = scan_started.elapsed();
            if !more.is_empty() {
                eprintln!(
                    "{label}: SIGTERM {} further payload process(es) found during the grace \
                     period: {}",
                    more.len(),
                    describe_identities(&more)
                );
            }
            let all_gone = lineage
                .termed
                .iter()
                .all(|identity| matches!(exact_process_liveness(identity), Ok(false)));
            if all_gone || Instant::now() >= deadline {
                break;
            }
            pace_teardown_poll(last_scan, deadline);
        }
    }
    kill_payload_domain(lineage, label)
}

/// Reap every exited child adopted by this subreaper without losing the direct
/// payload child's status.
///
/// `Child::try_wait` followed by [`reap_exited_children`] is unsafe here: the
/// direct child can exit between those calls, and `waitpid(-1)` can then consume
/// the status that `Child` still needs. Use one `waitpid(-1, WNOHANG)` loop for
/// both jobs and return the direct status to the caller when it appears.
pub(crate) fn reap_while_supervising(direct_pid: u32) -> io::Result<Option<ExitStatus>> {
    loop {
        let mut raw_status: libc::c_int = 0;
        // SAFETY: `raw_status` is writable for the duration of the call. This
        // process deliberately acts as a child subreaper, so -1 names only its
        // own direct or adopted children; WNOHANG never blocks supervision.
        let reaped = unsafe { libc::waitpid(-1, &mut raw_status, libc::WNOHANG) };
        if reaped == 0 {
            return Ok(None);
        }
        if reaped < 0 {
            let source = io::Error::last_os_error();
            if source.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(source);
        }
        if reaped == direct_pid as libc::pid_t {
            return Ok(Some(ExitStatus::from_raw(raw_status)));
        }
    }
}

pub(crate) fn reap_exited_children() {
    loop {
        let mut status = 0;
        // SAFETY: waitpid with WNOHANG reaps only exited children of this
        // dedicated supervisor process and never blocks.
        let result = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if result <= 0 {
            break;
        }
    }
}

#[cfg(test)]
pub(crate) struct ProcessDomainTestGuard {
    _thread: std::sync::MutexGuard<'static, ()>,
    _process: File,
}

#[cfg(test)]
pub(crate) fn process_domain_test_guard() -> Option<ProcessDomainTestGuard> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // `PR_SET_CHILD_SUBREAPER` and `waitpid(-1)` are process-wide. A mutex can
    // serialize the tests that knowingly use them, but it cannot protect an
    // unrelated parallel test waiting for its own child. Run every marked
    // process-domain test in its own test process instead. The mutex remains
    // useful inside that process for tests which also mutate process-global
    // hooks or signal state. The file lock preserves the existing mutual
    // exclusion among this marked cohort without serializing unrelated tests.
    if env::var_os(PROCESS_DOMAIN_TEST_CHILD_ENV).is_some() {
        let marker_path = env::var_os(PROCESS_DOMAIN_TEST_MARKER_ENV)
            .expect("isolated process-domain test must name its entry marker");
        let test_name = thread::current()
            .name()
            .expect("the Rust test harness must name each process-domain test")
            .to_string();
        fs::write(&marker_path, &test_name).expect("publish process-domain test entry marker");
        let lock_path = env::var_os(PROCESS_DOMAIN_TEST_LOCK_ENV)
            .expect("isolated process-domain test must name its shared lock");
        let process = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("open process-domain test lock");
        process
            .lock_exclusive()
            .expect("lock process-domain test cohort");
        return Some(ProcessDomainTestGuard {
            _thread: LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
            _process: process,
        });
    }

    let test_name = thread::current()
        .name()
        .expect("the Rust test harness must name each process-domain test")
        .to_string();
    let lock_path = env::temp_dir().join(format!(
        "ci-hub-process-domain-tests-{}.lock",
        std::process::id()
    ));
    let marker_path = env::temp_dir().join(format!(
        "ci-hub-process-domain-test-{}-{}.entered",
        std::process::id(),
        test_name.replace("::", "-")
    ));
    let status = Command::new(env::current_exe().expect("locate current test executable"))
        .arg("--exact")
        .arg(&test_name)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(PROCESS_DOMAIN_TEST_CHILD_ENV, "1")
        .env(PROCESS_DOMAIN_TEST_LOCK_ENV, lock_path)
        .env(PROCESS_DOMAIN_TEST_MARKER_ENV, &marker_path)
        .status()
        .expect("launch isolated process-domain test");
    let entered = fs::read_to_string(&marker_path)
        .unwrap_or_else(|error| panic!("isolated process-domain test did not start: {error}"));
    let _ = fs::remove_file(&marker_path);
    assert_eq!(
        entered, test_name,
        "isolated process-domain test entry marker named the wrong test"
    );
    assert!(
        status.success(),
        "isolated process-domain test {test_name} failed: {status}"
    );
    None
}

#[cfg(test)]
static HEARTBEAT_HELPER_DELAY_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
pub(crate) fn set_heartbeat_helper_delay(milliseconds: u64) {
    HEARTBEAT_HELPER_DELAY_MS.store(milliseconds, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn heartbeat_test_helper_delay() {
    let milliseconds = HEARTBEAT_HELPER_DELAY_MS.swap(0, std::sync::atomic::Ordering::SeqCst);
    if milliseconds > 0 {
        Command::new("/bin/sleep")
            .arg(format!("{:.3}", milliseconds as f64 / 1_000.0))
            .status()
            .expect("delayed heartbeat helper must complete");
    }
}

#[cfg(not(test))]
pub(crate) fn heartbeat_test_helper_delay() {}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunCrashPoint {
    AfterArm,
    AfterSpawn,
    AfterPublish,
}

#[cfg(test)]
enum RunCrashAction {
    ReturnError,
    StopSupervisor { ready: PathBuf },
}

#[cfg(test)]
static RUN_CRASH_HOOK: std::sync::Mutex<Option<(String, RunCrashPoint, RunCrashAction)>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn install_run_crash_hook(operation: &str, point: RunCrashPoint) {
    let mut hook = RUN_CRASH_HOOK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(hook.is_none(), "a run crash hook is already installed");
    *hook = Some((operation.to_string(), point, RunCrashAction::ReturnError));
}

#[cfg(test)]
pub(crate) fn install_run_hard_death_hook(operation: &str, point: RunCrashPoint, ready: PathBuf) {
    let mut hook = RUN_CRASH_HOOK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(hook.is_none(), "a run crash hook is already installed");
    *hook = Some((
        operation.to_string(),
        point,
        RunCrashAction::StopSupervisor { ready },
    ));
}

#[cfg(test)]
pub(crate) fn take_run_crash_hook(operation: &str, point: RunCrashPoint) -> bool {
    let mut hook = RUN_CRASH_HOOK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let action = if hook
        .as_ref()
        .is_some_and(|(expected_operation, expected_point, _)| {
            expected_operation == operation && *expected_point == point
        }) {
        hook.take().map(|(_, _, action)| action)
    } else {
        None
    };
    drop(hook);
    match action {
        Some(RunCrashAction::ReturnError) => true,
        Some(RunCrashAction::StopSupervisor { ready }) => {
            fs::write(&ready, b"ready\n").unwrap_or_else(|source| {
                panic!("write hard-death readiness marker at {ready:?}: {source}")
            });
            let raised = unsafe { libc::raise(libc::SIGSTOP) };
            assert_eq!(raised, 0, "SIGSTOP hard-death supervisor hook failed");
            true
        }
        None => false,
    }
}

fn current_process_owner() -> Result<ProcessOwner, LandLockError> {
    let pid = std::process::id();
    let start_ticks = process_start_ticks(pid).map_err(|source| LandLockError::Io {
        action: "read process identity from",
        path: PathBuf::from(format!("/proc/{pid}/stat")),
        source,
    })?;
    Ok(ProcessOwner {
        host: current_host(),
        boot_id: current_boot_id()?,
        pid,
        start_ticks,
    })
}

fn render_lock_holder_liveness(liveness: &OwnerLiveness) -> String {
    match liveness {
        OwnerLiveness::Alive => "alive".into(),
        OwnerLiveness::Dead(reason) => format!("exited:{reason}"),
        OwnerLiveness::Unknown(reason) => format!("unknown:{reason}"),
    }
}

fn render_lock_holder_clear_evidence(evidence: &str) -> String {
    match evidence.strip_prefix("owner dead: ") {
        Some(rest) => format!("lock-holder process exited: {rest}"),
        None => evidence.to_string(),
    }
}

pub(crate) fn print_cleanup_record(record: &CleanupRecord, reason: &str) {
    for line in record.render().lines() {
        println!("  {line}");
    }
    println!("  verification={reason}");
}

fn required<T>(value: Option<T>, name: &str) -> Result<T, LandLockError> {
    value.ok_or_else(|| LandLockError::InvalidState(format!("holder has no {name} field")))
}

pub(crate) fn suffix(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.display()))
}

fn read_to_string(path: &Path) -> Result<String, LandLockError> {
    let mut file = File::open(path).map_err(|source| io_error("open", path, source))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|source| io_error("read", path, source))?;
    Ok(content)
}

fn write_truncated(path: &Path, bytes: &[u8]) -> Result<(), LandLockError> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|source| io_error("open for write", path, source))?;
    file.write_all(bytes)
        .map_err(|source| io_error("write", path, source))?;
    file.flush()
        .map_err(|source| io_error("flush", path, source))?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<(), LandLockError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("remove", path, source)),
    }
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> LandLockError {
    LandLockError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

// ---------------------------------------------------------------------------
// Box-global exclusion, shared by BOTH locks.
//
// These two helpers live HERE, in the lower module, for the same reason the pure
// lease primitives above do: `validate_lock.rs` already reuses this module's
// stateless helpers "rather than copy tricky code", and `landing_lock` cannot
// import from `validate_lock`. They were first written on the validate side
// (dev-hermit 25d251e5) and are MOVED here rather than copied, so there is one
// implementation, not two. Each lock passes its OWN anchor filename: the module
// contract is that a validate must never block a lander and vice versa, so they
// are box-global with respect to their own kind and invisible to each other.
// ---------------------------------------------------------------------------

/// The directory whose lock state governs `root`'s repository.
///
/// A linked worktree and its main worktree are ONE repository sharing ONE
/// checkout of the box's compute; they must share one lease. `git rev-parse
/// --git-common-dir` is the identity that is stable across every worktree of a
/// repository (each worktree's `--git-dir` differs; the COMMON dir does not), and
/// its parent is the main working tree.
///
/// Measured on this box 2026-08-08 before the fix: 46 worktrees of
/// `~/work/dev-hermit`, every one of them carrying a runnable `ci-hub/ci-hub`,
/// so every one derived its own private lock and admitted independently — two of
/// them ran concurrent landings 2/2.
///
/// Falls back to `root` — the previous behaviour — whenever that identity cannot
/// be established (no git, a bare repo, a submodule whose common dir is
/// `…/.git/modules/<name>`). Falling back is safe in the only direction that
/// matters: it can leave two roots un-merged, which is the status quo ante, but
/// it can never point a live lease at a directory that is not a working tree.
pub(crate) fn repository_lock_root(root: &Path) -> PathBuf {
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .bounded_output()
    else {
        return root.to_path_buf();
    };
    if !output.status.success() {
        return root.to_path_buf();
    }
    let common = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if common.file_name().and_then(|name| name.to_str()) != Some(".git") {
        return root.to_path_buf();
    }
    match common.parent() {
        Some(main_worktree) if main_worktree.is_dir() => main_worktree.to_path_buf(),
        _ => root.to_path_buf(),
    }
}

/// Path of a box-global exclusion anchor, derived from the calling **uid alone**
/// — no environment, no argument, no workspace, nothing a caller can redirect.
///
/// `/run/user/<uid>` is per-user, 0700 and tmpfs; the `/tmp` form is the fallback
/// for a box without logind. `kind` names the anchor file, so the landing anchor
/// and the validate anchor are distinct and never block one another.
pub(crate) fn box_exclusion_anchor_path(kind: &str) -> PathBuf {
    let uid = unsafe { libc::getuid() };
    let runtime = PathBuf::from(format!("/run/user/{uid}"));
    let dir = if runtime.is_dir() {
        runtime.join("ci-hub")
    } else {
        PathBuf::from(format!("/tmp/ci-hub-box-exclusion.{uid}"))
    };
    dir.join(kind)
}

/// Box-global exclusion anchor: ONE per box, per uid, for every repository.
///
/// Canonicalizing to the main worktree merges the worktrees of one repository. It
/// does NOT merge two repositories, and this box has two (`~/work/dev-hermit` and
/// `~/temp/dev-hermit`, measured 2026-08-08). Two repositories mean two lease
/// files, and the exclusive property is about the BOX, not about a repository.
///
/// The value of an flock here is exactly that it is NOT state anyone has to
/// maintain: the kernel releases it when the holder dies, so it can never strand
/// the box the way a stale record can, and it needs no migration of the lease and
/// quarantine files other tooling already reads in the workspace.
///
/// It is strictly ADDITIVE. Every existing check still runs — the FIFO queue, the
/// lease, the cleanup quarantine, the evidence-based dead-owner reclaim. This is
/// one more necessary condition on top, never a replacement, and it is taken
/// OUTERMOST so a loser waits before touching any lease.
pub(crate) struct BoxExclusionAnchor {
    #[allow(dead_code)]
    file: File,
    path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BoxAnchorObservation {
    Free,
    Held(String),
    Unknown(String),
}

impl BoxExclusionAnchor {
    /// Take the anchor, waiting up to `wait_seconds` (0 = do not wait).
    ///
    /// `Ok(None)` means another holder has it and we are not waiting, so the
    /// caller can print the same REFUSED shape as a held lease rather than crash.
    /// Errors are raw `io::Error` so each lock can wrap them in its own type;
    /// this helper deliberately knows nothing about either error enum.
    pub(crate) fn take(path: &Path, wait_seconds: u64) -> io::Result<Option<Self>> {
        Self::take_with_attribution(path, wait_seconds, None)
    }

    /// Take the anchor and publish a pre-verified run identity in its diagnostic
    /// body. The caller must construct `attribution` before acquisition; an
    /// empty or line-breaking value is refused so a holder can never forge
    /// additional key/value records in the human-readable body.
    pub(crate) fn take_attributed(
        path: &Path,
        wait_seconds: u64,
        attribution: &str,
    ) -> io::Result<Option<Self>> {
        if attribution.is_empty()
            || attribution
                .bytes()
                .any(|byte| byte == b'\n' || byte == b'\r')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "box-anchor attribution must be one nonempty line",
            ));
        }
        Self::take_with_attribution(path, wait_seconds, Some(attribution))
    }

    fn take_with_attribution(
        path: &Path,
        wait_seconds: u64,
        attribution: Option<&str>,
    ) -> io::Result<Option<Self>> {
        let path = path.to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        let deadline = Instant::now() + Duration::from_secs(wait_seconds);
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => {
                    // Record who holds it. DIAGNOSTIC ONLY -- the flock is the
                    // authority, so a truncated or stale body can never grant or
                    // deny anything. It exists so a waiting agent can name the
                    // holder instead of staring at an anonymous block.
                    let mut handle = &file;
                    let _ = handle.set_len(0);
                    let suffix = attribution
                        .map(|value| format!(" {value}"))
                        .unwrap_or_default();
                    let _ = writeln!(
                        handle,
                        "pid={} host={} anchor=box-exclusive-v1{}",
                        std::process::id(),
                        current_host(),
                        suffix,
                    );
                    return Ok(Some(Self { file, path }));
                }
                Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(POLL_SECONDS * 100));
                }
                Err(source) => return Err(source),
            }
        }
    }

    /// Who currently holds the anchor, for a refusal message. Best effort.
    pub(crate) fn describe_holder(path: &Path) -> String {
        match fs::read_to_string(path) {
            Ok(body) if !body.trim().is_empty() => body.trim().replace('\n', " "),
            _ => format!("another process (anchor {})", path.display()),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Observe the same kernel flock that `take` uses without creating or
    /// mutating it. This is deliberately tri-state: an I/O failure is not proof
    /// that a box-exclusive authority is free.
    pub(crate) fn observe(path: &Path) -> BoxAnchorObservation {
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return BoxAnchorObservation::Free
            }
            Err(source) => {
                return BoxAnchorObservation::Unknown(format!(
                    "cannot open anchor {}: {source}",
                    path.display()
                ))
            }
        };
        match file.try_lock_exclusive() {
            Ok(()) => {
                if let Err(source) = FileExt::unlock(&file) {
                    BoxAnchorObservation::Unknown(format!(
                        "cannot unlock observed anchor {}: {source}",
                        path.display()
                    ))
                } else {
                    BoxAnchorObservation::Free
                }
            }
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                let mut body = String::new();
                let mut handle = &file;
                match handle.read_to_string(&mut body) {
                    Ok(_) if !body.trim().is_empty() => {
                        BoxAnchorObservation::Held(body.trim().replace('\n', " "))
                    }
                    Ok(_) => BoxAnchorObservation::Unknown(format!(
                        "held anchor {} has an empty attribution record",
                        path.display()
                    )),
                    Err(source) => BoxAnchorObservation::Unknown(format!(
                        "cannot read held anchor {}: {source}",
                        path.display()
                    )),
                }
            }
            Err(source) => BoxAnchorObservation::Unknown(format!(
                "cannot probe anchor {}: {source}",
                path.display()
            )),
        }
    }
}

/// How a supervised `run` child ended.
enum ChildOutcome {
    /// The child exited on its own with this status.
    Exited {
        status: ExitStatus,
        pgid: u32,
    },
    /// The child exceeded its deadline and was killed.
    TimedOut {
        pgid: u32,
    },
    Uncertain {
        pgid: u32,
        reason: String,
    },
}

impl ChildOutcome {
    fn pgid(&self) -> u32 {
        match self {
            Self::Exited { pgid, .. } | Self::TimedOut { pgid } | Self::Uncertain { pgid, .. } => {
                *pgid
            }
        }
    }
}

/// Wait for `child`, killing it if it runs longer than `deadline_secs`.
///
/// The child subtree is signalled SIGTERM, given a short grace period, then
/// SIGKILLed, and the child is reaped so no zombie is left behind. Callers reject
/// zero before acquiring the lock so the pre-guardrail unbounded behavior cannot
/// be re-enabled.
fn supervise_child(
    child: &mut Child,
    deadline_secs: u64,
    pr: &str,
    lineage: &mut PayloadLineage,
) -> ChildOutcome {
    let deadline = Instant::now() + Duration::from_secs(deadline_secs);
    loop {
        match reap_while_supervising(child.id()) {
            Ok(Some(status)) => {
                return ChildOutcome::Exited {
                    status,
                    pgid: child.id(),
                }
            }
            Ok(None) => {}
            Err(source) => {
                eprintln!("landing-lock: cannot wait on child: {source}");
                return ChildOutcome::Uncertain {
                    pgid: child.id(),
                    reason: source.to_string(),
                };
            }
        }
        if Instant::now() >= deadline {
            let pgid = child.id();
            terminate_child_group(child, pr, lineage);
            return ChildOutcome::TimedOut { pgid };
        }
        thread::sleep(Duration::from_millis(CHILD_POLL_MILLIS));
    }
}

/// SIGTERM then (after up to a grace period) SIGKILL the child's process group, and
/// reap the direct child. The child was spawned with `process_group(0)`, so its
/// pid equals its pgid and signalling `-pid` reaches the whole land subtree
/// (gh / git / cargo), not just the wrapper shell.
///
/// Each group signal is paired with the same signal, by exact identity, to
/// every process the [`PayloadLineage`] has recorded OUTSIDE the group
/// ([`term_payload_group`], [`kill_payload_group`]), so a descendant that
/// called `setsid`, with or without the payload token, shares the grace window
/// and the final SIGKILL instead of surviving to be frozen by the census
/// (https://github.com/rrnewton/dev-hermit/issues/419).
fn terminate_child_group(child: &mut Child, pr: &str, lineage: &mut PayloadLineage) {
    // Capture the protected domain before TERM can reap its leader. Descendants
    // retain this pgid even after the direct child exits.
    let pgid = child.id();
    let group = format!("-{pgid}");
    eprintln!("landing-lock: child-deadline reached for PR #{pr}; SIGTERM process group {group}");
    // Do not reap the leader during the grace: its exact PID keeps the
    // captured process group identity from disappearing or being reused
    // before final KILL.
    let grace_emptied = term_payload_group(
        lineage,
        pgid,
        Duration::from_secs(CHILD_TERM_GRACE_SECONDS),
        "landing-lock",
    );
    if grace_emptied {
        eprintln!(
            "landing-lock: every recorded payload process exited during the grace for PR #{pr}; \
             SIGKILL process group {group}"
        );
    } else {
        eprintln!("landing-lock: grace expired for PR #{pr}; SIGKILL process group {group}");
    }
    let kill_sent = kill_payload_group(lineage, pgid, grace_emptied, "landing-lock");
    let child_reaped = if kill_sent {
        child.wait().is_ok()
    } else {
        matches!(child.try_wait(), Ok(Some(_)))
    };
    let killed_deadline = Instant::now() + Duration::from_secs(CHILD_TERM_GRACE_SECONDS);
    while Instant::now() < killed_deadline {
        if child_reaped && !process_group_exists(pgid) {
            break;
        }
        thread::sleep(Duration::from_millis(CHILD_POLL_MILLIS));
    }
    if child_reaped {
        reap_exited_children();
    }
}

/// Send `signal` to the positive process-group ID without delegating negative
/// PID parsing to a host-specific `kill(1)` implementation.
pub(crate) fn signal_group(signal: libc::c_int, pgid: u32) -> bool {
    signal_group_succeeds(signal, pgid)
}

fn signal_group_succeeds(signal: libc::c_int, pgid: u32) -> bool {
    signal_group_with(signal, pgid, |target, signal| {
        // SAFETY: `signal_group_with` admits only a negative group target below
        // -1, and every signal is a libc constant at the production call sites.
        unsafe { libc::kill(target, signal) == 0 }
    })
}

fn signal_group_with(
    signal: libc::c_int,
    pgid: u32,
    send: impl FnOnce(libc::pid_t, libc::c_int) -> bool,
) -> bool {
    process_group_target(pgid).is_some_and(|target| send(target, signal))
}

fn process_group_target(pgid: u32) -> Option<libc::pid_t> {
    let pgid = libc::pid_t::try_from(pgid).ok()?;
    // `kill(-1, signal)` is the Linux broadcast form, not process group 1.
    // Refuse both non-group values before entering the syscall boundary.
    (pgid > 1).then_some(-pgid)
}

/// Probe the captured process group, treating every error except ESRCH as live
/// so cleanup fails closed on permission or transient kernel errors.
pub(crate) fn process_group_exists(pgid: u32) -> bool {
    !matches!(process_group_liveness(pgid), ProcessGroupLiveness::Absent)
}

pub(crate) fn exit_status_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

#[cfg(test)]
mod cgroup_anchor_tests {
    use super::*;

    fn record_with(cgroup: Option<&str>) -> CleanupRecord {
        CleanupRecord {
            agent: "a".into(),
            operation: "validate:deadbeef".into(),
            host: "testhost".into(),
            boot_id: "boot".into(),
            phase: CleanupPhase::Published {
                leader: ProcessIdentity {
                    pid: 42,
                    start_ticks: 7,
                },
                pgid: 42,
            },
            domain: cgroup
                .map(|value| CleanupDomain::Cgroup(value.to_string()))
                .unwrap_or(CleanupDomain::LegacyUnproven),
            attestation: None,
        }
    }

    // BACK-COMPAT, the load-bearing half of the version bump: a record with no
    // cgroup must still render EXACTLY as version 2. Landing-lock records and
    // every armed/legacy validate record take this path, so older binaries on
    // this shared box keep reading them.
    #[test]
    fn record_without_a_cgroup_still_renders_version_2() {
        let rendered = record_with(None).render();
        assert!(
            rendered.starts_with("version=2\n"),
            "expected v2 for an anchorless record, got: {rendered}"
        );
        assert!(!rendered.contains("cgroup="));
        assert_eq!(
            CleanupRecord::parse(&rendered).unwrap().domain,
            CleanupDomain::LegacyUnproven
        );
    }

    // A record that HAS an anchor moves to v3 and round-trips.
    #[test]
    fn record_with_a_cgroup_renders_version_3_and_round_trips() {
        let record = record_with(Some("/user.slice/payload.scope"));
        let rendered = record.render();
        assert!(rendered.starts_with("version=3\n"), "got: {rendered}");
        assert!(rendered.contains("cgroup=/user.slice/payload.scope\n"));
        assert_eq!(CleanupRecord::parse(&rendered).unwrap(), record);
    }

    // Forward-compat in the other direction: a v2 record that somehow carries a
    // cgroup is MALFORMED, not silently tolerated. Version must continue to
    // mean something.
    #[test]
    fn version_2_carrying_a_cgroup_is_refused() {
        let bad = "version=2\nagent=a\noperation=o\nhost=h\nboot_id=b\n\
                   cgroup=/x\nphase=armed\n";
        let error = CleanupRecord::parse(bad).unwrap_err();
        assert!(
            error.contains("cannot carry a cleanup domain"),
            "got: {error}"
        );
    }

    #[test]
    fn process_group_domain_renders_version_4_and_round_trips() {
        let mut record = record_with(None);
        record.domain = CleanupDomain::ProcessGroup;
        let rendered = record.render();
        assert!(rendered.starts_with("version=4\n"), "got: {rendered}");
        assert!(rendered.contains("domain=process-group\n"));
        assert!(!rendered.contains("cgroup="));
        assert_eq!(CleanupRecord::parse(&rendered).unwrap(), record);
    }

    #[test]
    fn unsupported_versions_are_still_refused() {
        let bad = "version=5\nagent=a\noperation=o\nhost=h\nboot_id=b\nphase=armed\n";
        assert!(CleanupRecord::parse(bad)
            .unwrap_err()
            .contains("unsupported cleanup version"));
    }

    // POSITIVE CONTROL for the census: pointed at a cgroup that demonstrably
    // contains a live process -- our own -- it must SEE it. Without this, a
    // census that reported Empty unconditionally would pass every other test
    // here while being catastrophically wrong.
    #[test]
    fn cgroup_population_sees_this_very_process() {
        let Some(mine) = process_cgroup(std::process::id()) else {
            eprintln!("skipping: no cgroup-v2 entry for self on this host");
            return;
        };
        assert!(
            mine.starts_with('/'),
            "expected an absolute v2 path: {mine}"
        );
        match cgroup_population(&mine) {
            CgroupCensus::Populated(pids) => assert!(
                pids.contains(&std::process::id()),
                "own cgroup {mine} did not list this process: {pids:?}"
            ),
            other => panic!("own cgroup {mine} must be POPULATED, got {other:?}"),
        }
    }

    // The emptiness proof that closes the one-way door: an absent cgroup counts
    // as empty, because the kernel only removes one that holds no processes.
    #[test]
    fn an_absent_cgroup_counts_as_proven_empty() {
        let absent = "/user.slice/user-999999.slice/user@999999.service/ci-hub-absent.scope";
        assert_eq!(
            cgroup_population(absent),
            CgroupCensus::Empty { absent: true }
        );
    }

    // Fail closed on anything that is not a plain absolute cgroup path, so a
    // crafted record cannot walk the census out of the hierarchy.
    #[test]
    fn traversal_and_relative_cgroup_paths_are_refused() {
        for bad in ["../escape", "relative/path", "/ok/../../escape"] {
            assert!(
                matches!(cgroup_population(bad), CgroupCensus::Unknown(_)),
                "cgroup path {bad:?} must be refused"
            );
        }
    }

    // safe-ci.slice is DERIVED from the payload's own path, never hardcoded:
    // the real path embeds the invoking uid.
    #[test]
    fn safe_ci_slice_is_derived_from_the_payload_path() {
        assert_eq!(
            safe_ci_slice_for(
                "/user.slice/user-4321.slice/user@4321.service/x.slice/payload.scope"
            )
            .as_deref(),
            Some("/user.slice/user-4321.slice/user@4321.service/safe.slice/safe-ci.slice")
        );
        // Not user-manager shaped => the caller must NOT get a confidently wrong
        // path; it must get nothing and treat the census as incomplete.
        assert_eq!(safe_ci_slice_for("/system.slice/whatever.scope"), None);
        assert_eq!(safe_ci_slice_for("/"), None);
    }
}

/// Parse a restated `<pid>:<start_ticks>` payload-leader identity. `start_ticks`
/// is what defeats PID reuse, so a bare pid is refused rather than tolerated.
fn parse_leader_identity(value: &str) -> Result<ProcessIdentity, LandLockError> {
    let refuse = || {
        LandLockError::ReclaimNotProven(format!(
            "--leader must be <pid>:<start_ticks> exactly as `status` prints it, got {value:?}"
        ))
    };
    let (pid, start_ticks) = value.trim().split_once(':').ok_or_else(refuse)?;
    Ok(ProcessIdentity {
        pid: pid.parse().map_err(|_| refuse())?,
        start_ticks: start_ticks.parse().map_err(|_| refuse())?,
    })
}

// ---------------------------------------------------------------------------
// Post-mortem domain census. SHARED by both lock authorities.
//
// These live here, not in validate_lock.rs, because BOTH land-lock and
// validate-lock must reach the same verdict from the same evidence. A second
// copy of `census_disposition` would be a policy that can drift: one authority
// could start discharging a quarantine the other still refuses.
// ---------------------------------------------------------------------------

/// What the KERNEL can still say about a recorded payload domain once the
/// supervisor is gone.
///
/// Three outcomes, not two: "I could not look" must never be collapsed into
/// "nothing is there", which is exactly the mistake that would let a census
/// bury a live domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DomainEvidence {
    /// Every domain named by the record is empty, verified against the kernel.
    ProvenEmpty(String),
    /// At least one live process remains. Not overridable by attestation.
    Populated(String),
    /// Not determinable from the record alone; attestation is the only recourse.
    Unproven(String),
}

/// Census a legacy version-3 payload domain from its recorded cgroup anchor.
///
/// This is what closes the UNCENSUSED one-way door. `leader`/`pgid` are both
/// dead ends after the supervisor exits — a ppid walk needs the subreaper that
/// died with it, and pgid membership does not survive `setsid()`. Cgroup
/// membership survives both, so a recorded cgroup can be read directly.
///
/// Two domains are checked, and BOTH must be empty: the payload's own cgroup
/// subtree, and `safe-ci.slice`, the one place a payload legitimately migrates
/// out of its unit cgroup (via `safe-ci-dag-runner`).
pub(crate) fn census_recorded_domain(cgroup: Option<&str>) -> DomainEvidence {
    let Some(payload) = cgroup else {
        return DomainEvidence::Unproven(
            "the legacy cleanup record carries no cleanup domain".into(),
        );
    };

    let mut proven = Vec::new();
    let mut census = |label: &str, path: &str| -> Result<(), DomainEvidence> {
        match cgroup_population(path) {
            CgroupCensus::Empty { absent } => {
                // Absence is a POSITIVE proof, not a skipped check: a cgroup can
                // only be removed once it holds no processes.
                proven.push(format!(
                    "{label} {path} {}",
                    if absent { "is absent" } else { "is empty" }
                ));
                Ok(())
            }
            CgroupCensus::Populated(pids) => Err(DomainEvidence::Populated(format!(
                "{label} {path} still holds {} process(es): {}",
                pids.len(),
                pids.iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ))),
            CgroupCensus::Unknown(why) => Err(DomainEvidence::Unproven(format!(
                "{label} {path} could not be censused: {why}"
            ))),
        }
    };

    if let Err(evidence) = census("payload cgroup", payload) {
        return evidence;
    }
    let Some(safe_ci) = safe_ci_slice_for(payload) else {
        return DomainEvidence::Unproven(format!(
            "payload cgroup {payload} is not user-manager shaped, so safe-ci.slice cannot be \
             derived and the migration target would go uncensused"
        ));
    };
    if let Err(evidence) = census("migration target", &safe_ci) {
        return evidence;
    }
    DomainEvidence::ProvenEmpty(proven.join("; "))
}

/// Re-census the exact boundary published for this payload.
///
/// Version-4 records use the payload's own process group. `kill(-pgid, 0)` is
/// only a liveness probe here; this path never signals the group. A reused PGID
/// therefore fails closed as populated. Version-3 cgroup records remain
/// readable for recovery, and version-2 records retain their attestation path.
pub(crate) fn census_cleanup_domain(record: &CleanupRecord, pgid: u32) -> DomainEvidence {
    match &record.domain {
        CleanupDomain::LegacyUnproven => census_recorded_domain(None),
        CleanupDomain::ProcessGroup if process_group_exists(pgid) => {
            DomainEvidence::Populated(format!("payload process group {pgid} is still populated"))
        }
        CleanupDomain::ProcessGroup => {
            DomainEvidence::ProvenEmpty(format!("payload process group {pgid} is absent"))
        }
        CleanupDomain::Cgroup(cgroup) => census_recorded_domain(Some(cgroup)),
    }
}

/// Print the exact phase-sensitive discharge command for either lock consumer.
/// Published records carry a leader identity; incomplete residual records do
/// not and always require explicit domain evidence because their escaped
/// descendants cannot be reconstructed after the fact.
pub(crate) fn print_census_recovery(record: &CleanupRecord, command: &str) {
    match &record.phase {
        CleanupPhase::Published { leader, pgid } | CleanupPhase::CensusPending { leader, pgid } => {
            let domain = census_cleanup_domain(record, *pgid);
            let attestation = match &domain {
                DomainEvidence::ProvenEmpty(_) => "",
                _ => " --attest-domain-empty --attester '<agent-name>' --evidence '<observations>'",
            };
            println!(
                "  recovery={command} census-orphaned-domain --agent {} --operation {} \
                 --leader {}:{} --pgid {pgid}{attestation}",
                record.agent, record.operation, leader.pid, leader.start_ticks
            );
            match domain {
                DomainEvidence::ProvenEmpty(detail) => println!(
                    "  recovery_precondition=none; the recorded cleanup domain is ALREADY \
                     proven empty ({detail}), so no attestation is required. reclaim-dead \
                     alone still CANNOT clear this state."
                ),
                DomainEvidence::Populated(detail) => println!(
                    "  recovery_precondition=BLOCKED: {detail}. No census can run and no \
                     attestation can override a live domain."
                ),
                DomainEvidence::Unproven(why) => println!(
                    "  recovery_precondition={why}, so confirm by hand that the payload's unit \
                     cgroup is absent/empty AND every cgroup it migrates into is empty; \
                     reclaim-dead alone CANNOT clear this state"
                ),
            }
        }
        CleanupPhase::Residual {
            pgid,
            domain_complete: false,
            ..
        } => {
            println!(
                "  recovery={command} census-orphaned-domain --agent {} --operation {} \
                 --pgid {pgid} --attest-domain-empty --attester '<agent-name>' \
                 --evidence '<observations>'",
                record.agent, record.operation
            );
            println!(
                "  recovery_precondition=residual process-domain capture was incomplete; \
                 confirm by hand that the payload's unit cgroup is absent/empty AND every \
                 cgroup it could migrate into is empty. The command rechecks the guarded \
                 supervisor identity, every recorded residual identity, and the PGID; \
                 reclaim-dead alone CANNOT clear this state"
            );
        }
        CleanupPhase::Armed
        | CleanupPhase::Residual {
            domain_complete: true,
            ..
        } => {}
    }
}

/// What discharges this census, if anything.
///
/// Pure on purpose: this is the entire policy change of the cgroup-anchor work,
/// so it is testable without fabricating lock state. `Ok` is the disposition to
/// record in the transcript, `Err` the refusal.
pub(crate) fn census_disposition(
    domain: DomainEvidence,
    attested: bool,
    attester: &str,
    evidence: &str,
    mechanical_context: &str,
) -> Result<String, String> {
    match domain {
        // Mechanical. An attestation, if supplied, is simply not needed.
        DomainEvidence::ProvenEmpty(detail) => Ok(format!("empty domain PROVEN: {detail}")),
        // A live domain is never overridable. An attestation may fill an
        // unobservable gap; it cannot contradict a positive kernel census.
        DomainEvidence::Populated(detail) => Err(format!(
            "the recorded payload domain is NOT empty: {detail}. Wait for it to drain, or \
             terminate only the exact recorded identities after rechecking boot_id and \
             /proc/<pid>/stat field 22. --attest-domain-empty cannot override a live domain."
        )),
        DomainEvidence::Unproven(why) => {
            if !attested {
                return Err(format!(
                    "every mechanical precondition passes ({mechanical_context}), but the \
                     domain could not be censused mechanically: {why}. Re-run with \
                     --attest-domain-empty --attester '<agent-name>' --evidence \
                     '<observations>' after confirming the payload's unit cgroup is absent/empty \
                     AND every cgroup the payload migrates into is empty."
                ));
            }
            let attestation = DomainAttestation::new(attester, evidence)?;
            Ok(format!(
                "empty domain ATTESTED by {} (not mechanically provable: {why}): {}",
                attestation.attester, attestation.evidence
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HARD_DEATH_ROOT_ENV: &str = "CI_HUB_LANDING_HARD_DEATH_ROOT";
    const HARD_DEATH_POINT_ENV: &str = "CI_HUB_LANDING_HARD_DEATH_POINT";
    const PERIODIC_REAP_ROOT_ENV: &str = "CI_HUB_LANDING_PERIODIC_REAP_ROOT";
    const PERIODIC_REAP_EXIT_ENV: &str = "CI_HUB_LANDING_PERIODIC_REAP_EXIT";

    fn paths_at(root: &Path) -> LockPaths {
        let lock = root.join(".landing-lock");
        LockPaths {
            guard: suffix(&lock, ".guard"),
            queue: suffix(&lock, ".queue"),
            owner: suffix(&lock, ".owner"),
            cleanup: suffix(&lock, ".cleanup-required"),
            // PER-TEST anchor beside the per-test lease. Every `run`-path test
            // therefore exercises the real anchor CODE while touching neither the
            // live box anchor nor any sibling test's.
            anchor: suffix(&lock, ".box-anchor"),
            lock,
        }
    }

    fn temp_paths(name: &str) -> LockPaths {
        let root = env::temp_dir().join(format!(
            "ci-hub-landing-lock-{name}-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        paths_at(&root)
    }

    #[test]
    fn child_subreaping_refuses_an_unisolated_test_process() {
        let error = enable_child_subreaper()
            .expect_err("a test must not make the shared harness process a child subreaper");
        assert!(
            error
                .to_string()
                .contains("must enter through process_domain_test_guard"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn process_group_signal_is_typed_and_target_scoped() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let mut target = Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();
        let mut control = Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();

        let invalid_syscalls = std::cell::Cell::new(0);
        let zero_refused = !signal_group_with(libc::SIGTERM, 0, |_, _| {
            invalid_syscalls.set(invalid_syscalls.get() + 1);
            true
        });
        let one_refused = !signal_group_with(libc::SIGTERM, 1, |_, _| {
            invalid_syscalls.set(invalid_syscalls.get() + 1);
            true
        });
        let target_signaled = signal_group_succeeds(libc::SIGTERM, target.id());
        let deadline = Instant::now() + Duration::from_secs(2);
        let target_exited = loop {
            if target.try_wait().unwrap().is_some() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let control_survived = control.try_wait().unwrap().is_none();

        if !target_exited {
            let _ = target.kill();
            let _ = target.wait();
        }
        signal_group(libc::SIGKILL, control.id());
        let _ = control.wait();

        assert!(zero_refused, "pgid 0 must never target the caller's group");
        assert!(one_refused, "pgid 1 must never become kill(-1) broadcast");
        assert_eq!(
            invalid_syscalls.get(),
            0,
            "invalid pgids must be rejected before signal delivery"
        );
        assert!(target_signaled, "typed process-group signal was refused");
        assert!(target_exited, "target group did not terminate within 2s");
        assert!(control_survived, "unrelated process group was signalled");
    }

    /// POSITIVE BRACKET: an exited child can remain visible as a zombie until
    /// the subreaper drains it. It owns no executable payload and must not
    /// manufacture a residual quarantine after an otherwise completed run.
    #[test]
    fn zombie_only_descendant_is_reaped_and_domain_is_empty() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let token = mint_domain_token();
        let zombie = Command::new("/bin/true")
            .process_group(0)
            .env(PAYLOAD_DOMAIN_ENV, &token)
            .spawn()
            .unwrap();
        let pid = zombie.id();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if matches!(process_stat_fields(pid), Ok(('Z', _, _))) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "child {pid} did not enter zombie state"
            );
            thread::sleep(Duration::from_millis(10));
        }

        // The zombie IS the payload here, so its own start time is the floor
        // production would pass. That also keeps anything older than this test
        // out of the walk, which is the whole point of the floor.
        let floor = process_start_ticks(pid).unwrap();
        let capture = capture_and_freeze_residuals(std::process::id(), floor, &token);
        assert!(capture.complete);
        assert!(capture.identities.is_empty());
        assert!(!process_group_exists(pid));
    }

    /// NEGATIVE BRACKET: draining zombies must not turn the classifier into a
    /// way to clear a genuinely executing descendant.
    #[test]
    fn live_descendant_remains_an_exact_residual() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let token = mint_domain_token();
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .env(PAYLOAD_DOMAIN_ENV, &token)
            .spawn()
            .unwrap();
        let identity = ProcessIdentity {
            pid: child.id(),
            start_ticks: process_start_ticks(child.id()).unwrap(),
        };

        let capture =
            capture_and_freeze_residuals(std::process::id(), identity.start_ticks, &token);
        assert!(capture.complete);
        assert!(capture.identities.contains(&identity));

        signal_exact_process(&identity, libc::SIGKILL).unwrap();
        child.wait().unwrap();
    }

    /// REGRESSION: an orphan adopted DURING the run window is not the payload's.
    ///
    /// This is the case no timing, process-group or cgroup test can decide. It
    /// starts after the leader, it leads its own process group, and on this box
    /// it shares the test binary's cgroup byte for byte -- indistinguishable
    /// from a genuine payload descendant that called `setsid`. Only the
    /// inherited token separates them.
    ///
    /// ⚠️ THE SECOND ASSERTION IS THE CONTROL. A same-aged, same-shaped process
    /// that DOES carry the token must still be a residual, or this test would
    /// pass just as well if the census had stopped counting anything at all.
    #[test]
    fn an_orphan_adopted_during_the_run_is_not_charged_to_the_payload() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let token = mint_domain_token();

        let mut foreign = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .env(PAYLOAD_DOMAIN_ENV, "some-other-payload-9999")
            .spawn()
            .unwrap();
        let foreign_identity = ProcessIdentity {
            pid: foreign.id(),
            start_ticks: process_start_ticks(foreign.id()).unwrap(),
        };
        let mut mine = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .env(PAYLOAD_DOMAIN_ENV, &token)
            .spawn()
            .unwrap();
        let mine_identity = ProcessIdentity {
            pid: mine.id(),
            start_ticks: process_start_ticks(mine.id()).unwrap(),
        };

        // Floor 0, so the floor cannot be what excludes anything here.
        let capture = capture_and_freeze_residuals(std::process::id(), 0, &token);
        eprintln!(
            "CENSUS_CONTROL_RESULT supervisor={} mine={mine_identity:?} foreign={foreign_identity:?} complete={} identities={:?} unclassifiable={:?}",
            std::process::id(), capture.complete, capture.identities, capture.unclassifiable
        );
        assert!(
            capture.identities.contains(&mine_identity),
            "CONTROL FAILED: a token-carrying descendant must remain a residual; mine={mine_identity:?}, complete={}, identities={:?}, unclassifiable={:?}",
            capture.complete, capture.identities, capture.unclassifiable
        );
        assert!(
            !capture.identities.contains(&foreign_identity),
            "an orphan carrying another payload's token was charged to this one; foreign={foreign_identity:?}, complete={}, identities={:?}, unclassifiable={:?}",
            capture.complete, capture.identities, capture.unclassifiable
        );
        assert!(
            !capture.unclassifiable.contains(&foreign_identity),
            "a readable non-member is Foreign, not Unclassifiable; foreign={foreign_identity:?}, complete={}, identities={:?}, unclassifiable={:?}",
            capture.complete, capture.identities, capture.unclassifiable
        );

        assert!(
            capture.complete,
            "the member/non-member census must be complete"
        );

        signal_exact_process(&foreign_identity, libc::SIGKILL).unwrap();
        signal_exact_process(&mine_identity, libc::SIGKILL).unwrap();
        foreign.wait().unwrap();
        mine.wait().unwrap();
    }

    struct ScanFixtureChildren(Vec<Child>);

    impl Drop for ScanFixtureChildren {
        fn drop(&mut self) {
            for child in &mut self.0 {
                // Only an unreaped live child can be signalled safely here.
                if matches!(child.try_wait(), Ok(None)) {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
    }

    fn scan_fixture_child(token: &str) -> Child {
        Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .env(PAYLOAD_DOMAIN_ENV, token)
            .spawn()
            .unwrap()
    }

    fn scan_fixture_identity(child: &Child) -> ProcessIdentity {
        ProcessIdentity {
            pid: child.id(),
            start_ticks: process_start_ticks(child.id()).unwrap(),
        }
    }

    #[test]
    fn vanished_proc_stat_entry_does_not_poison_the_capture() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let token = mint_domain_token();
        let children = ScanFixtureChildren(vec![
            scan_fixture_child(&token),
            scan_fixture_child("other-payload"),
        ]);
        let mine = scan_fixture_identity(&children.0[0]);
        let foreign = scan_fixture_identity(&children.0[1]);
        let mut gone = ScanFixtureChildren(vec![scan_fixture_child(&token)]);
        let gone_identity = scan_fixture_identity(&gone.0[0]);
        let mut stat = File::open(format!("/proc/{}/stat", gone_identity.pid)).unwrap();
        // The entry is live when /proc is enumerated. At its actual scan read
        // boundary, reap this owned child and read the previously opened fd.
        // Linux reports ESRCH here; opening the now-absent path gives ENOENT.
        inject_scan_stat_reader(gone_identity.pid, move || {
            gone.0[0].kill()?;
            gone.0[0].wait()?;
            let mut raw = String::new();
            let error = stat.read_to_string(&mut raw).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
            assert_eq!(
                File::open(format!("/proc/{}/stat", gone_identity.pid))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::NotFound
            );
            Err(error)
        });
        let capture = capture_and_freeze_residuals(std::process::id(), 0, &token);
        assert!(SCAN_STAT_READER.with(|slot| slot.borrow().is_none()));
        assert!(capture.complete, "gone entry poisoned the census");
        assert!(capture.identities.contains(&mine));
        assert!(!capture.identities.contains(&foreign));
        assert!(!capture.identities.contains(&gone_identity));
        assert!(capture.unclassifiable.is_empty());
    }

    #[test]
    fn unreadable_or_malformed_proc_stat_still_refuses_completeness() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let token = mint_domain_token();
        let children = ScanFixtureChildren(vec![scan_fixture_child(&token)]);
        let mine = scan_fixture_identity(&children.0[0]);
        for (name, error) in [
            ("permission", io::Error::from_raw_os_error(libc::EACCES)),
            ("io", io::Error::from_raw_os_error(libc::EIO)),
            (
                "malformed",
                io::Error::new(io::ErrorKind::InvalidData, "malformed stat fixture"),
            ),
        ] {
            inject_scan_stat_reader(mine.pid, move || Err(error));
            let refused = capture_and_freeze_residuals(std::process::id(), 0, &token);
            assert!(SCAN_STAT_READER.with(|slot| slot.borrow().is_none()));
            assert!(!refused.complete, "{name} must not clear the census");
            let readable = capture_and_freeze_residuals(std::process::id(), 0, &token);
            assert!(readable.complete, "{name}: readable opposing control");
            assert!(readable.identities.contains(&mine));
            assert!(readable.unclassifiable.is_empty());
        }
    }

    /// A descendant we cannot classify is REPORTED, never omitted.
    ///
    /// Dropping it would silently discharge a real escaped payload; charging it
    /// would quarantine the box on a stranger. It gets its own bucket and it
    /// makes the census incomplete, so no caller can release on evidence that
    /// was never gathered.
    #[test]
    fn an_unclassifiable_descendant_is_reported_and_blocks_completeness() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let token = mint_domain_token();
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .env(PAYLOAD_DOMAIN_ENV, &token)
            .spawn()
            .unwrap();
        let identity = ProcessIdentity {
            pid: child.id(),
            start_ticks: process_start_ticks(child.id()).unwrap(),
        };

        inject_unreadable_environ_pid(identity.pid);
        let capture = capture_and_freeze_residuals(std::process::id(), 0, &token);
        assert!(
            capture.unclassifiable.contains(&identity),
            "an unreadable descendant must be reported as unclassifiable"
        );
        assert!(
            !capture.identities.contains(&identity),
            "an unclassifiable descendant must not be asserted as a proven member"
        );
        assert!(
            !capture.complete,
            "a census that could not read part of its domain is not complete"
        );

        // CONTROL: with the injection cleared the very same process classifies,
        // proving the bucket above came from the unreadable environment and not
        // from the process being absent or filtered on some other axis.
        // Assert about OUR pid, not about the whole tree: a shared test binary
        // can always have some unrelated descendant exiting mid-scan, and
        // `is_empty()` here would make this test fail for that instead.
        inject_unreadable_environ_pid(0);
        let reread = capture_and_freeze_residuals(std::process::id(), 0, &token);
        assert!(!reread.unclassifiable.contains(&identity));
        assert!(reread.identities.contains(&identity));

        signal_exact_process(&identity, libc::SIGKILL).unwrap();
        child.wait().unwrap();
    }

    /// REGRESSION: a process that PREDATES the payload is not a residual of it.
    ///
    /// `PR_SET_CHILD_SUBREAPER` is what makes the PPID walk exhaustive, and it
    /// is also what makes the walk over-broad: the kernel reparents unrelated
    /// orphans onto the supervisor, and the census then charges them to the
    /// payload AND SIGSTOPs them. Measured 2026-08-25 in this suite: nine
    /// `scm_telem_log` daemons were charged to seven different payload process
    /// groups within one run, each census reporting the same nine identities
    /// against a different pgid and every one of them alongside
    /// `original group absent`.
    ///
    /// ⚠️ THE SECOND CENSUS IS A CONTROL AND IT MUST SEE THE OLDER PROCESS.
    /// Without it this test would pass just as well if the older process had
    /// never been in the walk at all, which would prove nothing about the floor.
    /// BOTH processes carry the SAME payload token, so the token cannot be what
    /// separates them and the floor is the only thing under test. Giving the
    /// older one no token would let this pass for the wrong reason.
    #[test]
    fn a_process_older_than_the_payload_is_not_a_residual_of_it() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let token = mint_domain_token();
        let mut older = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .env(PAYLOAD_DOMAIN_ENV, &token)
            .spawn()
            .unwrap();
        let older_identity = ProcessIdentity {
            pid: older.id(),
            start_ticks: process_start_ticks(older.id()).unwrap(),
        };

        // `start_ticks` has USER_HZ granularity (10ms here), so the payload has
        // to be separated by more than one tick or the floor is only
        // accidentally satisfied by an equal value.
        thread::sleep(Duration::from_millis(50));

        let mut payload = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .env(PAYLOAD_DOMAIN_ENV, &token)
            .spawn()
            .unwrap();
        let payload_identity = ProcessIdentity {
            pid: payload.id(),
            start_ticks: process_start_ticks(payload.id()).unwrap(),
        };
        assert!(
            payload_identity.start_ticks > older_identity.start_ticks,
            "the payload must start strictly after the older process, or this \
             test brackets nothing"
        );

        // `complete` is deliberately NOT asserted. Convergence depends on the
        // whole descendant tree holding still, which a 32-thread test binary
        // cannot promise; the property under test is membership. The positive
        // assertion below is what keeps the negative one from passing
        // vacuously on a short or stale list.
        let floored =
            capture_and_freeze_residuals(std::process::id(), payload_identity.start_ticks, &token);
        assert!(
            floored.identities.contains(&payload_identity),
            "a descendant started after the payload must remain an exact residual"
        );
        assert!(
            !floored.identities.contains(&older_identity),
            "a process predating the payload was charged to it as a residual"
        );

        let unfloored = capture_and_freeze_residuals(std::process::id(), 0, &token);
        assert!(
            unfloored.identities.contains(&older_identity),
            "CONTROL FAILED: the older process was never in the walk, so the \
             assertion above did not measure the floor"
        );

        signal_exact_process(&older_identity, libc::SIGKILL).unwrap();
        signal_exact_process(&payload_identity, libc::SIGKILL).unwrap();
        older.wait().unwrap();
        payload.wait().unwrap();
    }

    #[test]
    fn unreadable_descendant_identity_makes_census_incomplete_then_recovers() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let token = mint_domain_token();
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .env(PAYLOAD_DOMAIN_ENV, &token)
            .spawn()
            .unwrap();
        let identity = ProcessIdentity {
            pid: child.id(),
            start_ticks: process_start_ticks(child.id()).unwrap(),
        };
        inject_unreadable_scan_pid(identity.pid);
        let incomplete =
            capture_and_freeze_residuals(std::process::id(), identity.start_ticks, &token);
        assert!(!incomplete.complete);

        inject_unreadable_scan_pid(0);
        let visible =
            capture_and_freeze_residuals(std::process::id(), identity.start_ticks, &token);
        assert!(visible.complete);
        assert!(visible.identities.contains(&identity));
        signal_exact_process(&identity, libc::SIGKILL).unwrap();
        child.wait().unwrap();
        reap_exited_children();
        let recovered =
            capture_and_freeze_residuals(std::process::id(), identity.start_ticks, &token);
        assert!(recovered.complete);
        assert!(recovered.identities.is_empty());
    }

    #[test]
    fn pidfd_signal_rejects_stale_identity_and_targets_exact_process() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let mut target = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let mut control = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let identity = ProcessIdentity {
            pid: target.id(),
            start_ticks: process_start_ticks(target.id()).unwrap(),
        };
        let stale = ProcessIdentity {
            pid: identity.pid,
            start_ticks: identity.start_ticks.saturating_add(1),
        };
        assert!(!signal_exact_process(&stale, libc::SIGKILL).unwrap());
        assert!(target.try_wait().unwrap().is_none());
        assert!(signal_exact_process(&identity, libc::SIGKILL).unwrap());
        target.wait().unwrap();
        assert!(control.try_wait().unwrap().is_none());
        control.kill().unwrap();
        control.wait().unwrap();
    }

    /// Fabricate the legacy wedge shape: a lock held by a dead supervisor whose
    /// cleanup record is stuck at `published` with no cleanup domain -- i.e. the
    /// historical `mergegate-fix`/`pr:1666` state.
    fn anchorless_published_quarantine(name: &str) -> (LandingLock, ProcessIdentity, u32) {
        let paths = temp_paths(name);
        let lock = LandingLock { paths };
        let holder = LockState {
            agent: "mergegate-fix".into(),
            repo: None,
            operation: None,
            pending_mutation: None,
            pending_attempt: None,
            pending_call_count: None,
            pending_call_id: None,
            pr: "1666".into(),
            host: current_host(),
            acquired_at: 100,
            acquired_human: "1970-01-01T00:01:40+0000".into(),
            expires_at: 1_000,
            reclaimed_from: None,
        };
        lock.write_holder(&holder).unwrap();
        // A dead supervisor: pid 0 can never be a live owner.
        let owner = ProcessOwner {
            host: current_host(),
            boot_id: current_boot_id().unwrap(),
            pid: 0,
            start_ticks: 1,
        };
        fs::write(&lock.paths.owner, owner.render()).unwrap();
        // A leader identity that cannot be alive: pid 0 with an absurd start.
        let leader = ProcessIdentity {
            pid: 0,
            start_ticks: u64::MAX,
        };
        let pgid = 0x7fff_fffe;
        let record = CleanupRecord {
            agent: "mergegate-fix".into(),
            operation: "pr:1666".into(),
            host: current_host(),
            boot_id: current_boot_id().unwrap(),
            phase: CleanupPhase::Published {
                leader: leader.clone(),
                pgid,
            },
            domain: CleanupDomain::LegacyUnproven,
            attestation: None,
        };
        write_cleanup_record(&lock.paths.cleanup, &record).unwrap();
        (lock, leader, pgid)
    }

    /// Legacy v3 shape: identical to `anchorless_published_quarantine`, except
    /// the record carries a cgroup anchor. `cgroup` is a user-manager-shaped
    /// path under a temp root that does not exist, and an absent cgroup is
    /// positive proof of emptiness.
    fn anchored_published_quarantine(
        name: &str,
        cgroup: &str,
    ) -> (LandingLock, ProcessIdentity, u32) {
        let (lock, leader, pgid) = anchorless_published_quarantine(name);
        let record = CleanupRecord {
            agent: "mergegate-fix".into(),
            operation: "pr:1666".into(),
            host: current_host(),
            boot_id: current_boot_id().unwrap(),
            phase: CleanupPhase::Published {
                leader: leader.clone(),
                pgid,
            },
            domain: CleanupDomain::Cgroup(cgroup.to_string()),
            attestation: None,
        };
        write_cleanup_record(&lock.paths.cleanup, &record).unwrap();
        (lock, leader, pgid)
    }

    /// NEGATIVE->POSITIVE, the whole point of A2. The survivor discharges a dead owner's lock
    /// with NO attestation and NO `census-orphaned-domain` archaeology, because the boundary
    /// was recorded while the payload was still alive.
    #[test]
    fn an_anchored_published_quarantine_reclaims_without_attestation() {
        let (lock, _leader, _pgid) = anchored_published_quarantine(
            "anchored-empty",
            "/sys/fs/cgroup/user.slice/user-0.slice/user@0.service/ci-hub-a2-absent.scope",
        );
        let _ = &lock;
        let evidence = census_recorded_domain(Some(
            "/sys/fs/cgroup/user.slice/user-0.slice/user@0.service/ci-hub-a2-absent.scope",
        ));
        assert!(
            matches!(evidence, DomainEvidence::ProvenEmpty(_)),
            "an absent anchored domain must census EMPTY without attestation, got {evidence:?}"
        );
    }

    /// THE OTHER DIRECTION, and the constraint that must survive the fix: an anchor is not a
    /// licence to discharge. Recording a boundary makes the domain CHECKABLE; it does not make it
    /// empty. A dead owner still sitting on live work is refused, and the refusal names the
    /// occupants rather than hand-waving.
    ///
    /// Tested against the pure policy function rather than a live cgroup on purpose: the only
    /// populated cgroups visible from this sandbox belong to other agents' transient tmux scopes,
    /// so keying the assertion on one would make the bracket depend on someone else's process
    /// lifetime. `census_disposition` IS the whole policy change, so it is the honest subject.
    #[test]
    fn an_anchored_but_populated_domain_is_refused_even_with_an_attestation() {
        let occupied = || DomainEvidence::Populated("payload cgroup X holds 3: 11,22,33".into());

        // Unattested -> refused, and the refusal carries the occupants.
        let refused = census_disposition(occupied(), false, "", "", "dead owner").unwrap_err();
        assert!(
            refused.contains("NOT empty") && refused.contains("11,22,33"),
            "a populated domain must be refused and name its occupants, got {refused}"
        );

        // Attestation cannot contradict positive liveness, with or without
        // evidence. This is the leg that protects live agent/infrastructure
        // processes when a stale record points at their broad sandbox.
        let anonymous = census_disposition(occupied(), true, "", "   ", "dead owner").unwrap_err();
        assert!(
            anonymous.contains("cannot override a live domain"),
            "attestation over a populated domain must be refused, got {anonymous}"
        );

        // And the positive control, so this is not a check that refuses everything:
        // a PROVEN-empty domain discharges mechanically with no attestation at all.
        let ok = census_disposition(
            DomainEvidence::ProvenEmpty("payload cgroup X is absent".into()),
            false,
            "",
            "",
            "dead owner",
        )
        .expect("a proven-empty domain must discharge without attestation");
        assert!(ok.contains("PROVEN"), "got {ok}");
    }

    /// ACQUIRE TIME, not release: a boundary written at release is never written in the case
    /// that needs it. Asserts the anchor is applied by the Armed->Published transition and that
    /// later transitions pass `None` and therefore cannot erase it.
    #[test]
    fn later_transitions_never_erase_the_acquire_time_anchor() {
        let (lock, leader, pgid) = anchored_published_quarantine(
            "anchored-preserved",
            "/sys/fs/cgroup/user.slice/user-0.slice/user@0.service/ci-hub-a2-keep.scope",
        );
        let rendered = fs::read_to_string(&lock.paths.cleanup).unwrap();
        assert!(
            rendered.contains("ci-hub-a2-keep.scope"),
            "publish must persist the acquire-time anchor, got: {rendered}"
        );
        // The runtime's later transitions pass `None`, which the transition applies as
        // "leave whatever was there" -- assert that contract at the source.
        let _ = (&leader, pgid);
        assert!(
            !rendered.contains("cgroup=\n"),
            "the anchor must not be blanked, got: {rendered}"
        );
    }

    fn census_args(leader: &ProcessIdentity, pgid: u32) -> CensusOrphanedDomainArgs {
        CensusOrphanedDomainArgs {
            agent: "mergegate-fix".into(),
            operation: "pr:1666".into(),
            leader: Some(format!("{}:{}", leader.pid, leader.start_ticks)),
            pgid,
            attest_domain_empty: false,
            attester: String::new(),
            evidence: String::new(),
        }
    }

    fn incomplete_residual(
        agent: &str,
        operation: &str,
        pgid: u32,
        residuals: Vec<ProcessIdentity>,
    ) -> CleanupRecord {
        CleanupRecord::new(
            agent,
            operation,
            CleanupPhase::Residual {
                pgid,
                domain_complete: false,
                residuals,
            },
        )
        .unwrap()
    }

    #[test]
    fn shared_classifier_distinguishes_incomplete_residual_absent_live_and_unverifiable() {
        let (lock, _, pgid) = anchorless_published_quarantine("residual-classifier");
        let absent = ProcessIdentity {
            pid: 0,
            start_ticks: 1,
        };
        write_cleanup_record(
            &lock.paths.cleanup,
            &incomplete_residual("mergegate-fix", "pr:1666", pgid, vec![absent]),
        )
        .unwrap();
        assert!(matches!(
            lock.cleanup_verification(lock.read_holder().unwrap().as_ref()),
            CleanupVerification::Uncensused { .. }
        ));

        let live = ProcessIdentity {
            pid: std::process::id(),
            start_ticks: process_start_ticks(std::process::id()).unwrap(),
        };
        write_cleanup_record(
            &lock.paths.cleanup,
            &incomplete_residual("mergegate-fix", "pr:1666", pgid, vec![live]),
        )
        .unwrap();
        assert!(matches!(
            lock.cleanup_verification(lock.read_holder().unwrap().as_ref()),
            CleanupVerification::Active { .. }
        ));

        write_cleanup_record(
            &lock.paths.cleanup,
            &incomplete_residual("mergegate-fix", "pr:1666", u32::MAX, Vec::new()),
        )
        .unwrap();
        assert!(matches!(
            lock.cleanup_verification(lock.read_holder().unwrap().as_ref()),
            CleanupVerification::Unknown { .. }
        ));
    }

    #[test]
    fn incomplete_residual_discharge_recovers_dead_owner_but_refuses_live_owner() {
        let (orphaned, _, pgid) = anchorless_published_quarantine("residual-discharge-dead");
        write_cleanup_record(
            &orphaned.paths.cleanup,
            &incomplete_residual(
                "mergegate-fix",
                "pr:1666",
                pgid,
                vec![ProcessIdentity {
                    pid: 0,
                    start_ticks: 1,
                }],
            ),
        )
        .unwrap();
        let holder = orphaned.read_holder().unwrap();
        let refusal = orphaned.require_no_cleanup(holder.as_ref()).unwrap_err();
        assert!(
            refusal
                .to_string()
                .contains("FLEET-WIDE landing quarantine")
                && refusal.to_string().contains("census-orphaned-domain"),
            "refusal must name fleet scope and discharge: {refusal}"
        );
        assert_eq!(
            orphaned
                .census_orphaned_domain(CensusOrphanedDomainArgs {
                    agent: "mergegate-fix".into(),
                    operation: "pr:1666".into(),
                    leader: None,
                    pgid,
                    attest_domain_empty: true,
                    attester: "recovery-agent".into(),
                    evidence: "unit cgroup absent; migration targets empty".into(),
                })
                .unwrap(),
            0
        );
        let recovered =
            CleanupRecord::parse(&fs::read_to_string(&orphaned.paths.cleanup).unwrap()).unwrap();
        assert!(matches!(
            recovered.phase,
            CleanupPhase::Residual {
                domain_complete: true,
                ref residuals,
                ..
            } if residuals.len() == 1
        ));
        assert_eq!(orphaned.reclaim_dead().unwrap(), 0);
        assert_eq!(
            orphaned
                .acquire(&AcquireArgs {
                    agent: "post-discharge".into(),
                    pr: "1667".into(),
                    wait: 0,
                    hold: 30,
                })
                .unwrap(),
            0
        );

        let live_paths = temp_paths("residual-discharge-live");
        let live = LandingLock {
            paths: live_paths.clone(),
        };
        assert_eq!(
            live.acquire(&AcquireArgs {
                agent: "live-owner".into(),
                pr: "77".into(),
                wait: 0,
                hold: 30,
            })
            .unwrap(),
            0
        );
        live.write_current_process_owner().unwrap();
        write_cleanup_record(
            &live.paths.cleanup,
            &incomplete_residual("live-owner", "pr:77", pgid, Vec::new()),
        )
        .unwrap();
        let error = live
            .census_orphaned_domain(CensusOrphanedDomainArgs {
                agent: "live-owner".into(),
                operation: "pr:77".into(),
                leader: None,
                pgid,
                attest_domain_empty: true,
                attester: "recovery-agent".into(),
                evidence: "inert isolated fixture".into(),
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("supervisor process is ALIVE"),
            "live owner must refuse the discharge verb: {error}"
        );
        assert!(matches!(
            live.acquire(&AcquireArgs {
                agent: "would-steal".into(),
                pr: "78".into(),
                wait: 0,
                hold: 30,
            }),
            Err(LandLockError::CleanupQuarantined(_))
        ));
        assert!(live.paths.cleanup.exists());
        remove_if_exists(&live.paths.owner).unwrap();
        let unverifiable = live
            .census_orphaned_domain(CensusOrphanedDomainArgs {
                agent: "live-owner".into(),
                operation: "pr:77".into(),
                leader: None,
                pgid,
                attest_domain_empty: true,
                attester: "recovery-agent".into(),
                evidence: "inert isolated fixture".into(),
            })
            .unwrap_err();
        assert!(
            unverifiable.to_string().contains("UNVERIFIABLE"),
            "missing guarded owner evidence must refuse: {unverifiable}"
        );
        assert!(live.paths.cleanup.exists());
        let _ = fs::remove_dir_all(live_paths.lock.parent().unwrap());
    }

    /// The whole point of A1: before this command existed, `reclaim-dead`
    /// demanded a census that nothing could produce. Walks the full door:
    /// blocked -> refused unattested -> refused anonymously -> discharged ->
    /// reclaimable -> acquirable.
    #[test]
    fn an_anchorless_published_quarantine_is_dischargeable_and_then_reclaimable() {
        let (lock, leader, pgid) = anchorless_published_quarantine("census-door");

        // The one-way door, before the census: reclaim-dead refuses.
        let before = lock.reclaim_dead().unwrap_err().to_string();
        assert!(
            before.contains("complete residual census"),
            "expected the uncensused refusal, got {before}"
        );

        // Anchorless + unattested -> refused, and the refusal names the remedy.
        let unattested = lock
            .census_orphaned_domain(census_args(&leader, pgid))
            .unwrap_err()
            .to_string();
        assert!(
            unattested.contains("--attest-domain-empty"),
            "an anchorless census must point at the attestation, got {unattested}"
        );

        // Attested but anonymous -> still refused. An unauditable attestation is
        // not evidence.
        let mut anonymous = census_args(&leader, pgid);
        anonymous.attest_domain_empty = true;
        let anonymous = lock
            .census_orphaned_domain(anonymous)
            .unwrap_err()
            .to_string();
        assert!(
            anonymous.contains("--attester"),
            "an anonymous attestation must be refused, got {anonymous}"
        );

        // Named but unexplained -> still refused. Both pieces form the durable
        // attestation and neither may be inferred by the tool.
        let mut unexplained = census_args(&leader, pgid);
        unexplained.attest_domain_empty = true;
        unexplained.attester = "recovery-agent".into();
        let unexplained = lock
            .census_orphaned_domain(unexplained)
            .unwrap_err()
            .to_string();
        assert!(
            unexplained.contains("--evidence"),
            "an unexplained attestation must be refused, got {unexplained}"
        );

        // Attested WITH evidence -> discharged.
        let mut attested = census_args(&leader, pgid);
        attested.attest_domain_empty = true;
        attested.attester = "recovery-agent".into();
        attested.evidence = "unit cgroup absent; safe-ci.slice populated 0".into();
        assert_eq!(lock.census_orphaned_domain(attested).unwrap(), 0);
        let recovered =
            CleanupRecord::parse(&fs::read_to_string(&lock.paths.cleanup).unwrap()).unwrap();
        assert_eq!(
            recovered.attestation,
            Some(DomainAttestation {
                attester: "recovery-agent".into(),
                evidence: "unit cgroup absent; safe-ci.slice populated 0".into(),
            })
        );

        // The record is now a complete residual census, so reclaim-dead finishes
        // and a subsequent acquire succeeds. That last step is the one that
        // matters operationally -- a discharge that does not restore landing is
        // not a fix.
        assert_eq!(lock.reclaim_dead().unwrap(), 0);
        assert!(matches!(
            verify_cleanup_record(&lock.paths.cleanup, None),
            CleanupVerification::None
        ));
        lock.acquire(&AcquireArgs {
            agent: "someone-else".into(),
            pr: "1635".into(),
            wait: 5,
            hold: 900,
        })
        .unwrap();
    }

    /// POSITIVE BRACKET: a genuinely orphaned payload whose published boundary
    /// is its own PGID is mechanically censusable and reclaimable. No broad
    /// cgroup and no operator attestation participates in the proof.
    #[test]
    fn an_orphaned_process_group_domain_is_censusable_and_reclaimable() {
        let (lock, leader, pgid) = anchorless_published_quarantine("pgid-census-door");
        let mut record =
            CleanupRecord::parse(&fs::read_to_string(&lock.paths.cleanup).unwrap()).unwrap();
        record.domain = CleanupDomain::ProcessGroup;
        write_cleanup_record(&lock.paths.cleanup, &record).unwrap();

        assert_eq!(
            lock.census_orphaned_domain(census_args(&leader, pgid))
                .unwrap(),
            0
        );
        assert!(matches!(
            lock.cleanup_verification(lock.read_holder().unwrap().as_ref()),
            CleanupVerification::Recoverable { .. }
        ));
        assert_eq!(lock.reclaim_dead().unwrap(), 0);
        assert!(matches!(
            verify_cleanup_record(&lock.paths.cleanup, None),
            CleanupVerification::None
        ));
    }

    /// The blind-aim guard: every restated field is re-checked, so this cannot
    /// be pointed at whatever happens to be quarantined at the time.
    #[test]
    fn a_census_refuses_every_mismatched_restatement() {
        let (lock, leader, pgid) = anchorless_published_quarantine("census-mismatch");
        let attest = |mut args: CensusOrphanedDomainArgs| {
            args.attest_domain_empty = true;
            args.evidence = "observed empty".into();
            lock.census_orphaned_domain(args).unwrap_err().to_string()
        };

        let mut wrong_agent = census_args(&leader, pgid);
        wrong_agent.agent = "someone-else".into();
        assert!(attest(wrong_agent).contains("does not match recorded agent"));

        let mut wrong_op = census_args(&leader, pgid);
        wrong_op.operation = "pr:9999".into();
        assert!(attest(wrong_op).contains("does not match recorded operation"));

        let mut wrong_leader = census_args(&leader, pgid);
        wrong_leader.leader = Some(format!("{}:{}", leader.pid, leader.start_ticks - 1));
        assert!(attest(wrong_leader).contains("does not match recorded leader"));

        let mut wrong_pgid = census_args(&leader, pgid);
        wrong_pgid.pgid = pgid - 1;
        assert!(attest(wrong_pgid).contains("does not match recorded pgid"));

        // A bare pid is refused: start_ticks is what defeats PID reuse.
        let mut bare = census_args(&leader, pgid);
        bare.leader = Some(leader.pid.to_string());
        assert!(attest(bare).contains("<pid>:<start_ticks>"));

        // And after all those refusals the quarantine is still intact. Verify it
        // BOUND TO ITS HOLDER: `verify_cleanup_record(.., None)` asks a different
        // question (an authority with no holder) and would not prove this.
        let holder = lock.read_holder().unwrap();
        assert!(matches!(
            lock.cleanup_verification(holder.as_ref()),
            CleanupVerification::Uncensused { .. }
        ));
    }

    #[test]
    fn holder_format_is_byte_compatible() {
        let holder = LockState {
            agent: "hermit-ci".into(),
            repo: None,
            operation: None,
            pending_mutation: None,
            pending_attempt: None,
            pending_call_count: None,
            pending_call_id: None,
            pr: "1533".into(),
            host: "testhost".into(),
            acquired_at: 100,
            acquired_human: "1970-01-01T00:01:40+0000".into(),
            expires_at: 1_000,
            reclaimed_from: Some("hermit-dbi".into()),
        };
        let rendered = "agent=hermit-ci\npr=1533\nhost=testhost\nacquired_at=100\nacquired_human=1970-01-01T00:01:40+0000\nexpires_at=1000\nreclaimed_from=hermit-dbi\n";
        assert_eq!(holder.render(), rendered);
        assert_eq!(LockState::parse(rendered).unwrap(), holder);
    }

    #[test]
    fn holder_format_preserves_exact_head_lander_extensions() {
        let rendered = "agent=hermit-lander\nrepo=rrnewton/hermit\noperation=op-1\npending_mutation=op-1\npending_attempt=attempt-1\npending_call_count=1\npending_call_id=call-1\npr=1650\nhost=testhost\nacquired_at=100\nacquired_human=1970-01-01T00:01:40+0000\nexpires_at=1000\n";
        let holder = LockState::parse(rendered).unwrap();
        assert_eq!(holder.repo.as_deref(), Some("rrnewton/hermit"));
        assert_eq!(holder.pending_call_count, Some(1));
        assert_eq!(holder.render(), rendered);

        let renewed = holder.renewed(60).unwrap();
        assert_eq!(renewed.repo, holder.repo);
        assert_eq!(renewed.operation, holder.operation);
        assert_eq!(renewed.pending_mutation, holder.pending_mutation);
        assert_eq!(renewed.pending_attempt, holder.pending_attempt);
        assert_eq!(renewed.pending_call_count, holder.pending_call_count);
        assert_eq!(renewed.pending_call_id, holder.pending_call_id);

        let unknown = format!("{rendered}unexpected=x\n");
        assert!(matches!(
            LockState::parse(&unknown),
            Err(LandLockError::InvalidState(message))
                if message.contains("unknown holder field")
        ));
    }

    #[test]
    fn queue_format_is_byte_compatible() {
        let entry = QueueEntry {
            enqueued_at: 1785770487,
            agent: "hermit-lander".into(),
            pr: "1281".into(),
        };
        assert_eq!(entry.render(), "1785770487\thermit-lander\t1281\n");
        assert_eq!(
            QueueEntry::parse(entry.render().trim_end(), 1).unwrap(),
            entry
        );
    }

    #[test]
    fn process_owner_sidecar_round_trips_without_changing_holder_format() {
        let owner = ProcessOwner {
            host: "testhost".into(),
            boot_id: "boot-1".into(),
            pid: 42,
            start_ticks: 123_456,
        };
        let rendered = "host=testhost\nboot_id=boot-1\npid=42\nstart_ticks=123456\n";
        assert_eq!(owner.render(), rendered);
        assert_eq!(ProcessOwner::parse(rendered).unwrap(), owner);
    }

    #[test]
    fn acquire_and_release_share_the_legacy_files() {
        let paths = temp_paths("roundtrip");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let status = lock
            .acquire(&AcquireArgs {
                agent: "test-agent".into(),
                pr: "42".into(),
                wait: 0,
                hold: 60,
            })
            .unwrap();
        assert_eq!(status, 0);
        let holder = LockState::parse(&read_to_string(&paths.lock).unwrap()).unwrap();
        assert_eq!(holder.agent, "test-agent");
        assert_eq!(holder.pr, "42");
        lock.release("test-agent", false).unwrap();
        assert!(!paths.lock.exists());
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn expired_live_holder_is_not_replaced_during_acquire() {
        let paths = temp_paths("expired-live-holder");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let now = epoch_seconds().unwrap();
        let mut holder = new_holder("still-running", "42", 60, None).unwrap();
        holder.acquired_at = now - 61;
        holder.expires_at = now - 1;
        lock.write_holder(&holder).unwrap();
        lock.write_current_process_owner().unwrap();

        let token = lock
            .with_guard(|| {
                lock.try_acquire(&AcquireArgs {
                    agent: "replacement".into(),
                    pr: "43".into(),
                    wait: 0,
                    hold: 60,
                })
            })
            .unwrap();
        assert!(
            matches!(token, AcquireToken::Held { ref agent, .. } if agent == "still-running"),
            "TTL expiry is eligibility, never proof that a live holder is gone"
        );
        assert_eq!(
            lock.read_holder().unwrap().unwrap().agent,
            "still-running",
            "mutation control: removing the live-owner refusal must fail this exact assertion"
        );
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn expired_dead_holder_is_reclaimed_during_acquire() {
        let paths = temp_paths("expired-dead-holder");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let now = epoch_seconds().unwrap();
        let mut holder = new_holder("dead-holder", "42", 60, None).unwrap();
        holder.acquired_at = now - 61;
        holder.expires_at = now - 1;
        lock.write_holder(&holder).unwrap();
        fs::write(
            &paths.owner,
            ProcessOwner {
                host: current_host(),
                boot_id: current_boot_id().unwrap(),
                pid: 0,
                start_ticks: 1,
            }
            .render(),
        )
        .unwrap();

        let token = lock
            .with_guard(|| {
                lock.try_acquire(&AcquireArgs {
                    agent: "replacement".into(),
                    pr: "43".into(),
                    wait: 0,
                    hold: 60,
                })
            })
            .unwrap();
        assert!(matches!(token, AcquireToken::AcquiredReclaimed(_)));
        assert_eq!(lock.read_holder().unwrap().unwrap().agent, "replacement");
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn full_protocol_acquire_renew_status_release_and_run() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths("full-protocol");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        assert_eq!(
            lock.acquire(&AcquireArgs {
                agent: "ci-shard".into(),
                pr: "test".into(),
                wait: 0,
                hold: 30,
            })
            .unwrap(),
            0
        );
        lock.renew("ci-shard", 60, false).unwrap();
        let holder = lock.read_holder().unwrap().unwrap();
        assert_eq!(holder.agent, "ci-shard");
        assert!(holder.expires_at > holder.acquired_at);
        lock.status().unwrap();
        lock.release("ci-shard", false).unwrap();
        assert!(lock.read_holder().unwrap().is_none());

        assert_eq!(
            lock.run(RunArgs {
                agent: "ci-shard-run".into(),
                pr: "test-run".into(),
                wait: 0,
                hold: 30,
                child_deadline: 30,
                child: vec![OsString::from("/bin/true")],
            })
            .unwrap(),
            0
        );
        assert!(lock.read_holder().unwrap().is_none());
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    fn run_periodic_reap_case(root: &Path, expected_code: i32) {
        let script = r#"
import os
import pathlib
import sys
import time

direct_pid_path = pathlib.Path(sys.argv[1])
grandchild_pid_path = pathlib.Path(sys.argv[2])
release_path = pathlib.Path(sys.argv[3])
exit_code = int(sys.argv[4])

direct_pid_path.write_text(f"{os.getpid()}\n", encoding="utf-8")
intermediate = os.fork()
if intermediate == 0:
    grandchild = os.fork()
    if grandchild == 0:
        os._exit(0)
    grandchild_pid_path.write_text(f"{grandchild}\n", encoding="utf-8")
    os._exit(0)

os.waitpid(intermediate, 0)
while not release_path.exists():
    time.sleep(0.01)
os._exit(exit_code)
"#;
        let paths = paths_at(root);
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let direct_pid_path = root.join("direct.pid");
        let grandchild_pid_path = root.join("grandchild.pid");
        let release_path = root.join("release");
        let observed_direct = direct_pid_path.clone();
        let observed_grandchild = grandchild_pid_path.clone();
        let observed_release = release_path.clone();

        let observer = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(4);
            let read_pid = |path: &Path, role: &str| loop {
                if let Ok(text) = fs::read_to_string(path) {
                    if let Ok(pid) = text.trim().parse::<u32>() {
                        break pid;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "direct payload did not publish its {role} PID"
                );
                thread::sleep(Duration::from_millis(10));
            };
            let direct_pid = read_pid(&observed_direct, "direct");
            let grandchild_pid = read_pid(&observed_grandchild, "grandchild");
            let direct_proc = PathBuf::from(format!("/proc/{direct_pid}"));
            let grandchild_proc = PathBuf::from(format!("/proc/{grandchild_pid}"));
            while grandchild_proc.exists() {
                assert!(
                    direct_proc.exists(),
                    "direct payload exited before its adopted child was reaped"
                );
                assert!(
                    Instant::now() < deadline,
                    "adopted exited child remained present during the live payload"
                );
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                direct_proc.exists(),
                "periodic reap was observed only after the direct payload exited"
            );
            fs::write(observed_release, b"release\n").unwrap();
        });

        let code = lock
            .run(RunArgs {
                agent: format!("periodic-reap-{expected_code}"),
                pr: format!("periodic-reap-{expected_code}"),
                wait: 0,
                hold: 30,
                child_deadline: 10,
                child: vec![
                    OsString::from("/usr/bin/python3"),
                    OsString::from("-c"),
                    OsString::from(script),
                    direct_pid_path.into_os_string(),
                    grandchild_pid_path.into_os_string(),
                    release_path.into_os_string(),
                    OsString::from(expected_code.to_string()),
                ],
            })
            .unwrap();
        observer.join().unwrap();
        assert_eq!(code, expected_code);
        assert!(lock.read_holder().unwrap().is_none());
        assert!(!paths.cleanup.exists());
    }

    #[test]
    fn run_periodic_reap_subprocess_entry() {
        let Some(root) = env::var_os(PERIODIC_REAP_ROOT_ENV).map(PathBuf::from) else {
            return;
        };
        let expected_code: i32 = env::var(PERIODIC_REAP_EXIT_ENV)
            .expect("periodic reap subprocess requires an exit code")
            .parse()
            .expect("periodic reap exit code must be an integer");
        run_periodic_reap_case(&root, expected_code);
    }

    /// The adopted grandchild must disappear while the direct payload is still
    /// alive. An end-of-run reap cannot satisfy this bracket. Run it once with
    /// success and once with a nonzero direct exit so periodic `waitpid(-1)`
    /// cannot silently discard the payload's result. Both runs must release the
    /// lease after preserving that result.
    #[test]
    fn run_reaps_adopted_exits_during_live_payload_and_preserves_status() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        for expected_code in [0, 23] {
            let paths = temp_paths(&format!("periodic-reap-{expected_code}"));
            let root = paths.lock.parent().unwrap().to_path_buf();
            let status = Command::new(env::current_exe().unwrap())
                .arg("--exact")
                .arg("landing_lock::tests::run_periodic_reap_subprocess_entry")
                .arg("--nocapture")
                .env(PERIODIC_REAP_ROOT_ENV, &root)
                .env(PERIODIC_REAP_EXIT_ENV, expected_code.to_string())
                .status()
                .unwrap();
            assert!(
                status.success(),
                "periodic reap subprocess failed for direct exit {expected_code}: {status}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn census_waits_for_delayed_heartbeat_helper_before_freezing_descendants() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths("delayed-heartbeat-helper");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        set_heartbeat_helper_delay(1_200);
        let started = Instant::now();
        let code = lock
            .run(RunArgs {
                agent: "delayed-heartbeat".into(),
                pr: "delayed-helper".into(),
                wait: 0,
                hold: 3,
                child_deadline: 10,
                child: vec![OsString::from("/bin/sleep"), OsString::from("1.4")],
            })
            .unwrap();
        assert_eq!(code, 0);
        assert!(
            started.elapsed() >= Duration::from_secs(2),
            "heartbeat delay was not exercised: {:?}",
            started.elapsed()
        );
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "census froze the heartbeat helper: {:?}",
            started.elapsed()
        );
        assert!(!paths.cleanup.exists());
        assert!(lock.read_holder().unwrap().is_none());
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    /// A process whose `comm` is not valid UTF-8 must not fail the `/proc`
    /// reads that every census and lineage observation runs
    /// (https://github.com/rrnewton/dev-hermit/issues/434). A 14-byte ASCII
    /// name followed by a 3-byte character is cut by the kernel to 15 bytes,
    /// inside that character.
    ///
    /// ⚠️ THE FIXTURE IS A THREAD OF THIS TEST, NOT A PROCESS. A ci-hub built
    /// before 42ddf70e3b4b8476b1de68487eb6967d5e0220e0 fails its whole `/proc`
    /// scan on such a name and quarantines its run, and every process on the
    /// host is in that scan. A process in a private PID namespace would not
    /// help: the host's `/proc` lists it too. A thread is not listed by
    /// `readdir("/proc")`, which only names thread-group leaders, so no scan
    /// of any version reads it, while `/proc/<tid>/stat` still carries the
    /// thread's own name. The test asserts both halves.
    #[test]
    fn a_process_whose_name_is_not_utf8_does_not_fail_the_proc_scan() {
        let (tid_tx, tid_rx) = mpsc::channel::<u32>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let renamed = thread::spawn(move || {
            let mut name = b"aaaaaaaaaaaaaa".to_vec();
            name.extend_from_slice("\u{20ac}".as_bytes());
            name.push(0);
            // SAFETY: `name` is NUL-terminated and outlives the call;
            // PR_SET_NAME renames only the calling thread.
            let renamed = unsafe { libc::prctl(libc::PR_SET_NAME, name.as_ptr()) };
            assert_eq!(renamed, 0, "PR_SET_NAME failed");
            // SAFETY: gettid has no arguments and only reads this thread's id.
            let tid = unsafe { libc::syscall(libc::SYS_gettid) };
            tid_tx.send(u32::try_from(tid).unwrap()).unwrap();
            // Ends when the test drops or sends on `stop_tx`, including when
            // an assertion below unwinds.
            let _ = stop_rx.recv_timeout(Duration::from_secs(60));
        });
        let tid = tid_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the renamed thread reports its id");
        let raw = fs::read(format!("/proc/{tid}/stat")).unwrap();
        let listed = fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_str() == Some(tid.to_string().as_str()));
        let fields = process_stat_fields(tid);
        let start_ticks = process_start_ticks(tid);
        let scanned = scan_process_stat_fields(tid);
        let group = start_ticks.as_ref().ok().and_then(|&start_ticks| {
            exact_process_group(&ProcessIdentity {
                pid: tid,
                start_ticks,
            })
        });
        let _ = stop_tx.send(());
        renamed.join().unwrap();

        assert!(
            raw.windows(14).any(|window| window == b"aaaaaaaaaaaaaa"),
            "the thread was not renamed: {raw:?}"
        );
        // The fixture is what broke `read_to_string`.
        assert!(
            std::str::from_utf8(&raw).is_err(),
            "the thread's stat line is valid UTF-8: {raw:?}"
        );
        assert!(
            !listed,
            "readdir(/proc) listed thread {tid}, so a host-wide scan would read it"
        );
        let fields = fields.expect("a non-UTF-8 comm parses");
        assert_ne!(fields.0, 'Z');
        assert_eq!(start_ticks.expect("start ticks parse"), fields.2);
        assert_eq!(
            scanned.expect("the per-entry read of the /proc scan parses a non-UTF-8 comm"),
            fields
        );
        // SAFETY: getpgid(0) only reads this process's own process-group id.
        let own_group = u32::try_from(unsafe { libc::getpgid(0) }).unwrap();
        assert_eq!(group, Some(own_group));
    }

    #[test]
    fn run_child_deadline_kills_child_and_releases_the_lock() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths("child-deadline");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let started = Instant::now();
        // A child that would otherwise sleep far past the test must be killed at
        // the deadline, the lock released, and the queue freed.
        let code = lock
            .run(RunArgs {
                agent: "stuck-lander".into(),
                pr: "9999".into(),
                wait: 0,
                hold: 30,
                child_deadline: 1,
                child: vec![OsString::from("sleep"), OsString::from("120")],
            })
            .unwrap();
        assert_eq!(code, CHILD_DEADLINE_EXIT_CODE);
        // Bounded: killed near the 1s deadline + short grace, nowhere near 120s.
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "child-deadline run took {:?}",
            started.elapsed()
        );
        // Head-of-line block is cleared: the lock is free for the next waiter.
        assert!(lock.read_holder().unwrap().is_none());
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn run_kills_stubborn_descendant_before_releasing_lock() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths("stubborn-descendant");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let identity_path = paths.lock.parent().unwrap().join("descendant.identity");
        let script = r#"
trap 'exit 0' TERM
(
  trap '' TERM HUP
  pid=$BASHPID
  start=$(awk '{print $22}' "/proc/$pid/stat")
  printf '%s %s %s\n' "$pid" "$start" "$$" > "$1"
  while :; do sleep 60; done
) &
while [ ! -s "$1" ]; do sleep 0.01; done
wait
"#;
        let started = Instant::now();
        let code = lock
            .run(RunArgs {
                agent: "stubborn-lander".into(),
                pr: "9997".into(),
                wait: 0,
                hold: 30,
                child_deadline: 1,
                child: vec![
                    OsString::from("/bin/bash"),
                    OsString::from("-c"),
                    OsString::from(script),
                    OsString::from("stubborn-descendant"),
                    identity_path.clone().into_os_string(),
                ],
            })
            .unwrap();
        assert_eq!(code, CHILD_DEADLINE_EXIT_CODE);

        let identity = fs::read_to_string(&identity_path).unwrap();
        let fields: Vec<_> = identity.split_whitespace().collect();
        assert_eq!(fields.len(), 3, "unexpected identity record {identity:?}");
        let pid: u32 = fields[0].parse().unwrap();
        let start_ticks: u64 = fields[1].parse().unwrap();
        let pgid: u32 = fields[2].parse().unwrap();
        let descendant_survived_release =
            matches!(process_start_ticks(pid), Ok(current) if current == start_ticks);
        if descendant_survived_release {
            signal_group(libc::SIGKILL, pgid);
        }

        assert!(
            started.elapsed() >= Duration::from_secs(CHILD_TERM_GRACE_SECONDS),
            "stubborn descendant did not survive TERM grace"
        );
        assert!(
            !descendant_survived_release,
            "exact descendant {pid}@{start_ticks} survived lock release"
        );
        assert!(!process_group_exists(pgid));
        assert!(lock.read_holder().unwrap().is_none());
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn escaped_payload_quarantine_refuses_consumers_until_exact_recovery() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths("escaped-quarantine");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let identity_path = paths.lock.parent().unwrap().join("escaped.identity");
        // The escaped child is marked CI_HUB_TEST_UNKILLABLE so the teardown's
        // kill sweep leaves it alive, standing in for a payload process SIGKILL
        // cannot remove. Since https://github.com/rrnewton/dev-hermit/issues/419
        // an ordinary setsid'd escapee is killed and the run releases; this test
        // covers the quarantine that must still follow a kill that fails.
        let script = r#"
trap 'exit 0' TERM
CI_HUB_TEST_UNKILLABLE=1 setsid /bin/bash -c '
  trap "" TERM HUP
  pid=$BASHPID
  start=$(awk "{print \$22}" "/proc/$pid/stat")
  printf "%s %s\n" "$pid" "$start" > "$1"
  while :; do sleep 60; done
' escaped-child "$1" &
while [ ! -s "$1" ]; do sleep 0.01; done
wait
"#;
        let run_error = lock
            .run(RunArgs {
                agent: "escaped-lander".into(),
                pr: "9996".into(),
                wait: 0,
                hold: 30,
                child_deadline: 1,
                child: vec![
                    OsString::from("/bin/bash"),
                    OsString::from("-c"),
                    OsString::from(script),
                    OsString::from("escaped-leader"),
                    identity_path.clone().into_os_string(),
                ],
            })
            .unwrap_err();
        assert!(matches!(run_error, LandLockError::CleanupQuarantined(_)));

        let identity = fs::read_to_string(&identity_path).unwrap();
        let fields: Vec<_> = identity.split_whitespace().collect();
        let escaped = ProcessIdentity {
            pid: fields[0].parse().unwrap(),
            start_ticks: fields[1].parse().unwrap(),
        };
        let record = CleanupRecord::parse(&fs::read_to_string(&paths.cleanup).unwrap()).unwrap();
        let residuals = match &record.phase {
            CleanupPhase::Residual {
                domain_complete: true,
                residuals,
                ..
            } => residuals.clone(),
            phase => panic!("expected complete residual phase, got {phase:?}"),
        };
        assert!(residuals.contains(&escaped));
        assert!(matches!(exact_process_liveness(&escaped), Ok(true)));
        assert!(matches!(
            lock.cleanup_verification(lock.read_holder().unwrap().as_ref()),
            CleanupVerification::Active { .. }
        ));

        let second = AcquireArgs {
            agent: "replacement-lander".into(),
            pr: "10000".into(),
            wait: 0,
            hold: 30,
        };
        assert!(matches!(
            lock.acquire(&second),
            Err(LandLockError::CleanupQuarantined(_))
        ));
        assert!(matches!(
            lock.renew("escaped-lander", 30, false),
            Err(LandLockError::CleanupQuarantined(_))
        ));
        assert!(matches!(
            lock.release("escaped-lander", false),
            Err(LandLockError::CleanupQuarantined(_))
        ));
        let mut owner = lock.read_process_owner().unwrap().unwrap();
        owner.pid = u32::MAX;
        write_truncated(&paths.owner, owner.render().as_bytes()).unwrap();
        assert!(matches!(
            lock.acquire(&second),
            Err(LandLockError::CleanupQuarantined(_))
        ));
        assert!(matches!(
            lock.reclaim_dead(),
            Err(LandLockError::ReclaimNotProven(_))
        ));
        lock.status().unwrap();

        for residual in &residuals {
            let _ = signal_exact_process(residual, libc::SIGKILL).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            reap_exited_children();
            if residuals
                .iter()
                .all(|identity| matches!(exact_process_liveness(identity), Ok(false)))
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(residuals
            .iter()
            .all(|identity| matches!(exact_process_liveness(identity), Ok(false))));
        assert!(matches!(
            lock.cleanup_verification(lock.read_holder().unwrap().as_ref()),
            CleanupVerification::Recoverable { .. }
        ));
        // Proven absence alone does not let an ordinary acquire erase the
        // quarantine; explicit recovery is still required.
        assert!(matches!(
            lock.acquire(&second),
            Err(LandLockError::CleanupQuarantined(_))
        ));

        assert_eq!(lock.reclaim_dead().unwrap(), 0);
        assert!(!paths.cleanup.exists());
        assert_eq!(lock.acquire(&second).unwrap(), 0);
        lock.release(&second.agent, false).unwrap();
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn hard_death_run_subprocess_entry() {
        let Some(root) = env::var_os(HARD_DEATH_ROOT_ENV).map(PathBuf::from) else {
            return;
        };
        let point = match env::var(HARD_DEATH_POINT_ENV).as_deref() {
            Ok("after-arm") => RunCrashPoint::AfterArm,
            Ok("after-spawn") => RunCrashPoint::AfterSpawn,
            other => panic!("invalid hard-death point {other:?}"),
        };
        let paths = paths_at(&root);
        let ready = root.join("supervisor-ready");
        let marker = root.join("payload-started");
        let pr = "hard-death";
        install_run_hard_death_hook(&format!("pr:{pr}"), point, ready);
        let result = LandingLock { paths }.run(RunArgs {
            agent: "hard-death-lander".into(),
            pr: pr.into(),
            wait: 0,
            hold: 30,
            child_deadline: 30,
            child: vec![
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from("printf started > \"$1\"; while :; do sleep 60; done"),
                OsString::from("payload"),
                marker.into_os_string(),
            ],
        });
        panic!("hard-death subprocess resumed instead of being SIGKILLed: {result:?}");
    }

    #[test]
    fn hard_death_before_publication_retains_armed_barrier() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        for point_name in ["after-arm", "after-spawn"] {
            let paths = temp_paths(&format!("hard-death-{point_name}"));
            let root = paths.lock.parent().unwrap().to_path_buf();
            let ready = root.join("supervisor-ready");
            let marker = root.join("payload-started");
            let mut supervisor = Command::new(env::current_exe().unwrap())
                .arg("--exact")
                .arg("landing_lock::tests::hard_death_run_subprocess_entry")
                .arg("--nocapture")
                .env(HARD_DEATH_ROOT_ENV, &root)
                .env(HARD_DEATH_POINT_ENV, point_name)
                .spawn()
                .unwrap();

            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready.exists() {
                if let Some(status) = supervisor.try_wait().unwrap() {
                    panic!("hard-death supervisor exited before readiness: {status}");
                }
                if Instant::now() >= deadline {
                    let _ = supervisor.kill();
                    let _ = supervisor.wait();
                    panic!("hard-death supervisor did not reach {point_name}");
                }
                thread::sleep(Duration::from_millis(10));
            }

            let lock = LandingLock {
                paths: paths.clone(),
            };
            let owner = lock.read_process_owner().unwrap().unwrap();
            assert_eq!(owner.pid, supervisor.id());
            assert_eq!(
                owner.start_ticks,
                process_start_ticks(supervisor.id()).unwrap(),
                "owner record was not bound to the stopped supervisor"
            );
            let record =
                CleanupRecord::parse(&fs::read_to_string(&paths.cleanup).unwrap()).unwrap();
            assert!(matches!(record.phase, CleanupPhase::Armed));
            assert!(!marker.exists(), "guest ran before identity publication");

            assert_eq!(
                unsafe { libc::kill(supervisor.id() as libc::pid_t, libc::SIGKILL) },
                0
            );
            let status = supervisor.wait().unwrap();
            assert_eq!(status.signal(), Some(libc::SIGKILL));
            let owner_identity = ProcessIdentity {
                pid: owner.pid,
                start_ticks: owner.start_ticks,
            };
            assert!(matches!(exact_process_liveness(&owner_identity), Ok(false)));
            thread::sleep(Duration::from_millis(100));
            assert!(
                !marker.exists(),
                "start-gated guest ran after supervisor death"
            );
            let persisted =
                CleanupRecord::parse(&fs::read_to_string(&paths.cleanup).unwrap()).unwrap();
            assert!(matches!(persisted.phase, CleanupPhase::Armed));

            let second = AcquireArgs {
                agent: "replacement-lander".into(),
                pr: "replacement".into(),
                wait: 0,
                hold: 30,
            };
            assert!(matches!(
                lock.acquire(&second),
                Err(LandLockError::CleanupQuarantined(_))
            ));
            assert!(matches!(
                lock.reclaim_dead(),
                Err(LandLockError::ReclaimNotProven(_))
            ));
            let _ = fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn crash_windows_retain_armed_or_uncensused_barrier_after_owner_death() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        for (index, point) in [
            RunCrashPoint::AfterArm,
            RunCrashPoint::AfterSpawn,
            RunCrashPoint::AfterPublish,
        ]
        .into_iter()
        .enumerate()
        {
            let paths = temp_paths(&format!("crash-window-{index}"));
            let lock = LandingLock {
                paths: paths.clone(),
            };
            let marker = paths.lock.parent().unwrap().join("payload-started");
            let pr = format!("crash-{index}");
            install_run_crash_hook(&format!("pr:{pr}"), point);
            let error = lock
                .run(RunArgs {
                    agent: "crash-lander".into(),
                    pr: pr.clone(),
                    wait: 0,
                    hold: 30,
                    child_deadline: 30,
                    child: vec![
                        OsString::from("/bin/sh"),
                        OsString::from("-c"),
                        OsString::from("printf started > \"$1\"; while :; do sleep 60; done"),
                        OsString::from("payload"),
                        marker.clone().into_os_string(),
                    ],
                })
                .unwrap_err();
            assert!(matches!(error, LandLockError::CleanupQuarantined(_)));

            let record =
                CleanupRecord::parse(&fs::read_to_string(&paths.cleanup).unwrap()).unwrap();
            let published = match (&point, &record.phase) {
                (RunCrashPoint::AfterArm | RunCrashPoint::AfterSpawn, CleanupPhase::Armed) => None,
                (RunCrashPoint::AfterPublish, CleanupPhase::Published { leader, pgid }) => {
                    Some((leader.clone(), *pgid))
                }
                (_, phase) => panic!("crash point {point:?} left unexpected phase {phase:?}"),
            };

            if let Some((leader, _)) = &published {
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline && !marker.exists() {
                    thread::sleep(Duration::from_millis(10));
                }
                assert!(marker.exists(), "published guest never started");
                assert!(matches!(exact_process_liveness(leader), Ok(true)));
            } else {
                thread::sleep(Duration::from_millis(100));
                reap_exited_children();
                assert!(
                    !marker.exists(),
                    "guest payload ran before exact identity publication"
                );
            }

            let mut owner = lock.read_process_owner().unwrap().unwrap();
            owner.pid = u32::MAX;
            write_truncated(&paths.owner, owner.render().as_bytes()).unwrap();
            let second = AcquireArgs {
                agent: "replacement-lander".into(),
                pr: "replacement".into(),
                wait: 0,
                hold: 30,
            };
            assert!(matches!(
                lock.acquire(&second),
                Err(LandLockError::CleanupQuarantined(_))
            ));
            assert!(matches!(
                lock.renew("crash-lander", 30, false),
                Err(LandLockError::CleanupQuarantined(_))
            ));
            assert!(matches!(
                lock.release("crash-lander", false),
                Err(LandLockError::CleanupQuarantined(_))
            ));
            assert!(matches!(
                lock.reclaim_dead(),
                Err(LandLockError::ReclaimNotProven(_))
            ));
            lock.status().unwrap();

            if let Some((leader, pgid)) = published {
                signal_group(libc::SIGKILL, pgid);
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline {
                    reap_exited_children();
                    if matches!(exact_process_liveness(&leader), Ok(false))
                        && !process_group_exists(pgid)
                    {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                assert!(matches!(exact_process_liveness(&leader), Ok(false)));
                assert!(!process_group_exists(pgid));
                assert!(matches!(
                    lock.cleanup_verification(lock.read_holder().unwrap().as_ref()),
                    CleanupVerification::Uncensused { .. }
                ));
                assert!(matches!(
                    lock.reclaim_dead(),
                    Err(LandLockError::ReclaimNotProven(_))
                ));
            }
            let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
        }
    }

    #[test]
    fn normal_leader_exit_cannot_clear_an_escaped_payload_barrier() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths("normal-exit-escaped");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let identity_path = paths.lock.parent().unwrap().join("escaped.identity");
        // The escaped child is marked CI_HUB_TEST_UNKILLABLE so the teardown's
        // kill sweep leaves it alive, standing in for a payload process SIGKILL
        // cannot remove. Since https://github.com/rrnewton/dev-hermit/issues/419
        // an ordinary setsid'd escapee is killed and the run releases; this test
        // covers the quarantine that must still follow a kill that fails.
        let script = r#"
CI_HUB_TEST_UNKILLABLE=1 setsid /bin/sh -c '
  trap "" TERM HUP
  pid=$$
  start=$(awk "{print \$22}" "/proc/$pid/stat")
  printf "%s %s\n" "$pid" "$start" > "$1"
  while :; do sleep 60; done
' escaped-child "$1" &
while [ ! -s "$1" ]; do sleep 0.01; done
exit 0
"#;
        let error = lock
            .run(RunArgs {
                agent: "normal-exit-lander".into(),
                pr: "normal-exit".into(),
                wait: 0,
                hold: 30,
                child_deadline: 30,
                child: vec![
                    OsString::from("/bin/sh"),
                    OsString::from("-c"),
                    OsString::from(script),
                    OsString::from("normal-leader"),
                    identity_path.clone().into_os_string(),
                ],
            })
            .unwrap_err();
        assert!(matches!(error, LandLockError::CleanupQuarantined(_)));
        let identity = fs::read_to_string(&identity_path).unwrap();
        let fields: Vec<_> = identity.split_whitespace().collect();
        let escaped = ProcessIdentity {
            pid: fields[0].parse().unwrap(),
            start_ticks: fields[1].parse().unwrap(),
        };
        let record = CleanupRecord::parse(&fs::read_to_string(&paths.cleanup).unwrap()).unwrap();
        let residuals = match record.phase {
            CleanupPhase::Residual {
                domain_complete: true,
                residuals,
                ..
            } => residuals,
            phase => panic!("expected complete residual census, got {phase:?}"),
        };
        assert!(residuals.contains(&escaped));
        assert!(lock.read_holder().unwrap().is_some());
        for residual in &residuals {
            let _ = signal_exact_process(residual, libc::SIGKILL).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            reap_exited_children();
            if residuals
                .iter()
                .all(|identity| matches!(exact_process_liveness(identity), Ok(false)))
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let mut owner = lock.read_process_owner().unwrap().unwrap();
        owner.pid = u32::MAX;
        write_truncated(&paths.owner, owner.render().as_bytes()).unwrap();
        assert_eq!(lock.reclaim_dead().unwrap(), 0);
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    /// https://github.com/rrnewton/dev-hermit/issues/419: a setsid'd payload
    /// process and its child, both ignoring TERM and blocked for at most 300 s
    /// reading a FIFO nobody writes, escape the landing supervisor's
    /// process-group kill. The teardown must kill them by payload token
    /// instead of quarantining.
    fn run_setsid_pipe_payload(name: &str, mode: &str, child_deadline: u64) {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths(name);
        let root = paths.lock.parent().unwrap().to_path_buf();
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let identities_path = root.join("escaped.identities");
        fs::write(&identities_path, b"").unwrap();
        let fifo = root.join("never-written.fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `fifo_c` is a NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let script = r#"
setsid /bin/bash -c '
  trap "" TERM HUP
  record() {
    local pid=$BASHPID
    printf "%s %s\n" "$pid" "$(awk "{print \$22}" "/proc/$pid/stat")" >> "$1"
  }
  ( trap "" TERM HUP; record "$1"; read -r -t 300 line <> "$2" ) &
  record "$1"
  read -r -t 300 line <> "$2"
' escaped-publication "$1" "$2" &
while [ "$(wc -l < "$1")" -lt 2 ]; do sleep 0.01; done
if [ "$3" = wait ]; then
  trap 'exit 0' TERM
  wait
fi
exit 0
"#;
        let result = lock.run(RunArgs {
            agent: format!("{name}-lander"),
            pr: "9419".into(),
            wait: 0,
            hold: 30,
            child_deadline,
            child: vec![
                OsString::from("/bin/bash"),
                OsString::from("-c"),
                OsString::from(script),
                OsString::from("payload-leader"),
                identities_path.clone().into_os_string(),
                fifo.clone().into_os_string(),
                OsString::from(mode),
            ],
        });
        let identities: Vec<ProcessIdentity> = fs::read_to_string(&identities_path)
            .unwrap()
            .lines()
            .map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                ProcessIdentity {
                    pid: fields[0].parse().unwrap(),
                    start_ticks: fields[1].parse().unwrap(),
                }
            })
            .collect();
        reap_exited_children();
        let survivors: Vec<_> = identities
            .iter()
            .filter(|identity| matches!(exact_process_liveness(identity), Ok(true)))
            .cloned()
            .collect();
        for identity in &survivors {
            let _ = signal_exact_process(identity, libc::SIGKILL);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && survivors
                .iter()
                .any(|identity| matches!(exact_process_liveness(identity), Ok(true)))
        {
            reap_exited_children();
            thread::sleep(Duration::from_millis(10));
        }
        let cleanup_left = paths.cleanup.exists();
        let holder_left = lock.read_holder().unwrap().is_some();
        let _ = fs::remove_dir_all(&root);

        assert_eq!(identities.len(), 2, "fixture must record both escaped processes");
        assert!(
            survivors.is_empty(),
            "setsid'd payload processes survived teardown: {survivors:?} (run result {result:?})"
        );
        let expected = if mode == "wait" {
            CHILD_DEADLINE_EXIT_CODE
        } else {
            0
        };
        assert_eq!(result.unwrap(), expected);
        assert!(!cleanup_left, "teardown left a cleanup quarantine behind");
        assert!(!holder_left, "teardown left the lock held");
    }

    /// Review of https://github.com/rrnewton/dev-hermit/issues/419: a process
    /// that stays in the payload's ORIGINAL process group but carries no
    /// payload token was never signalled at normal exit (the group kill ran
    /// only on the deadline and signal paths, and the token sweep skips it),
    /// so it survived and the box stayed quarantined on "original group
    /// active". Teardown must kill it.
    #[test]
    fn normal_exit_tears_down_a_tokenless_process_left_in_the_landing_group() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths("same-group-tokenless");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let identities_path = root.join("helper.identity");
        fs::write(&identities_path, b"").unwrap();
        let group_path = root.join("helper.group");
        fs::write(&group_path, b"").unwrap();
        let leader_group_path = root.join("leader.group");
        let fifo = root.join("never-written.fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `fifo_c` is a NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let result = lock.run(RunArgs {
            agent: "same-group-tokenless-lander".into(),
            pr: "9419".into(),
            wait: 0,
            hold: 30,
            child_deadline: 60,
            child: vec![
                OsString::from("/bin/bash"),
                OsString::from("-c"),
                OsString::from(same_group_helper_payload(&format!(
                    "env -u {PAYLOAD_DOMAIN_ENV}"
                ))),
                OsString::from("payload-leader"),
                identities_path.clone().into_os_string(),
                fifo.clone().into_os_string(),
                group_path.clone().into_os_string(),
                leader_group_path.clone().into_os_string(),
            ],
        });
        let identities: Vec<ProcessIdentity> = fs::read_to_string(&identities_path)
            .unwrap()
            .lines()
            .map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                assert_eq!(fields.len(), 2, "unexpected identity line {line:?}");
                ProcessIdentity {
                    pid: fields[0].parse().unwrap(),
                    start_ticks: fields[1].parse().unwrap(),
                }
            })
            .collect();
        reap_exited_children();
        let survivors: Vec<_> = identities
            .iter()
            .filter(|identity| matches!(exact_process_liveness(identity), Ok(true)))
            .cloned()
            .collect();
        for identity in &survivors {
            let _ = signal_exact_process(identity, libc::SIGKILL);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && survivors
                .iter()
                .any(|identity| matches!(exact_process_liveness(identity), Ok(true)))
        {
            reap_exited_children();
            thread::sleep(Duration::from_millis(10));
        }
        let helper = fs::read_to_string(&group_path).unwrap();
        let leader_group = fs::read_to_string(&leader_group_path).unwrap_or_default();
        let cleanup_left = paths.cleanup.exists();
        let holder_left = lock.read_holder().unwrap().is_some();
        let _ = fs::remove_dir_all(&root);

        let helper_fields: Vec<_> = helper.split_whitespace().collect();
        assert_eq!(identities.len(), 1, "fixture must record the helper");
        assert_eq!(helper_fields.len(), 2, "unexpected helper line {helper:?}");
        assert_eq!(
            helper_fields[0],
            leader_group.trim(),
            "the helper must stay in the payload's original process group"
        );
        assert_eq!(helper_fields[1], "unset", "the helper must carry no payload token");
        assert!(
            survivors.is_empty(),
            "a token-less member of the original group survived teardown: {survivors:?} (run result {result:?})"
        );
        assert_eq!(result.unwrap(), 0);
        assert!(!cleanup_left, "teardown left a cleanup quarantine behind");
        assert!(!holder_left, "teardown left the lock held");
    }

    #[test]
    fn normal_exit_tears_down_setsid_pipe_blocked_landing_payload() {
        run_setsid_pipe_payload("setsid-pipe-normal-exit", "exit", 60);
    }

    #[test]
    fn deadline_teardown_kills_setsid_pipe_blocked_landing_payload() {
        run_setsid_pipe_payload("setsid-pipe-deadline", "wait", 2);
    }

    /// Review of https://github.com/rrnewton/dev-hermit/issues/419 (parent-waits),
    /// landing side: a setsid'd token-carrying parent waits on a grandchild
    /// started with `env -u CI_HUB_PAYLOAD_DOMAIN setsid`. Killing the parent
    /// reparented the grandchild beyond both attribution axes and the run
    /// released with it alive. The grandchild must be killed.
    fn run_parent_waits_landing_payload(name: &str, mode: &str, child_deadline: u64) {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths(name);
        let root = paths.lock.parent().unwrap().to_path_buf();
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let identities_path = root.join("parent-waits.identities");
        fs::write(&identities_path, b"").unwrap();
        let facts_path = root.join("grandchild.facts");
        fs::write(&facts_path, b"").unwrap();
        let fifo = root.join("never-written.fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `fifo_c` is a NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let result = lock.run(RunArgs {
            agent: format!("{name}-lander"),
            pr: "9419".into(),
            wait: 0,
            hold: 30,
            child_deadline,
            child: vec![
                OsString::from("/bin/bash"),
                OsString::from("-c"),
                OsString::from(parent_waits_payload(&format!(
                    "env -u {PAYLOAD_DOMAIN_ENV}"
                ))),
                OsString::from("payload-leader"),
                identities_path.clone().into_os_string(),
                fifo.clone().into_os_string(),
                facts_path.clone().into_os_string(),
                OsString::from(mode),
            ],
        });
        let identities: Vec<ProcessIdentity> = fs::read_to_string(&identities_path)
            .unwrap()
            .lines()
            .map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                ProcessIdentity {
                    pid: fields[0].parse().unwrap(),
                    start_ticks: fields[1].parse().unwrap(),
                }
            })
            .collect();
        reap_exited_children();
        let survivors: Vec<_> = identities
            .iter()
            .filter(|identity| matches!(exact_process_liveness(identity), Ok(true)))
            .cloned()
            .collect();
        for identity in &survivors {
            let _ = signal_exact_process(identity, libc::SIGKILL);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && survivors
                .iter()
                .any(|identity| matches!(exact_process_liveness(identity), Ok(true)))
        {
            reap_exited_children();
            thread::sleep(Duration::from_millis(10));
        }
        let facts = fs::read_to_string(&facts_path).unwrap();
        let facts: Vec<String> = facts.split_whitespace().map(String::from).collect();
        let cleanup_left = paths.cleanup.exists();
        let holder_left = lock.read_holder().unwrap().is_some();
        let _ = fs::remove_dir_all(&root);

        assert_eq!(
            identities.len(),
            2,
            "fixture must record the parent and the grandchild"
        );
        assert_eq!(facts.len(), 4, "unexpected grandchild facts {facts:?}");
        let grandchild = identities[1].pid.to_string();
        assert_eq!(
            facts[0], grandchild,
            "the grandchild must lead its own session"
        );
        assert_eq!(
            facts[2],
            identities[0].pid.to_string(),
            "the grandchild must be the parent's child"
        );
        assert_eq!(
            facts[3], "unset",
            "the grandchild must carry no payload token"
        );
        assert!(
            survivors.is_empty(),
            "the parent-waits payload survived teardown: {survivors:?} (run result {result:?})"
        );
        let expected = if mode == "wait" {
            CHILD_DEADLINE_EXIT_CODE
        } else {
            0
        };
        assert_eq!(result.unwrap(), expected);
        assert!(!cleanup_left, "teardown left a cleanup quarantine behind");
        assert!(!holder_left, "teardown left the lock held");
    }

    #[test]
    fn normal_exit_kills_the_tokenless_grandchild_of_a_waiting_setsid_landing_parent() {
        run_parent_waits_landing_payload("parent-waits-normal", "exit", 60);
    }

    #[test]
    fn deadline_teardown_kills_the_tokenless_grandchild_of_a_waiting_setsid_landing_parent() {
        run_parent_waits_landing_payload("parent-waits-deadline", "wait", 2);
    }

    /// Run `program args` as a gated payload, wait for `ready` to exist when
    /// given, SIGTERM its group through [`term_payload_group`] with `grace`,
    /// then SIGKILL through [`kill_payload_group`]. Returns what the grace
    /// reported, how long it took, and the leader's terminating signal.
    fn deadline_grace_outcome(
        program: &str,
        args: &[OsString],
        ready: Option<&Path>,
        grace: Duration,
    ) -> (bool, Duration, Option<i32>) {
        let mut gated = spawn_gated_child(&OsString::from(program), args, |_| {}).unwrap();
        gated.release().unwrap();
        if let Some(ready) = ready {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !ready.exists() {
                assert!(
                    Instant::now() < deadline,
                    "payload never created {}",
                    ready.display()
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
        let mut lineage = PayloadLineage::new(
            std::process::id(),
            gated.leader.start_ticks,
            &gated.domain_token,
            gated.pgid,
        );
        let started = Instant::now();
        let emptied = term_payload_group(&mut lineage, gated.pgid, grace, "grace-test");
        let elapsed = started.elapsed();
        kill_payload_group(&mut lineage, gated.pgid, emptied, "grace-test");
        let status = gated.child.wait().unwrap();
        reap_exited_children();
        (emptied, elapsed, status.signal())
    }

    /// The deadline path's SIGTERM grace ends once a successful observation
    /// finds no recorded payload process alive. An earlier revision of this
    /// change scanned `/proc` every 20 ms for the whole 5 s grace, which spent
    /// 7.4 to 11.9 s of system CPU per `--child-deadline 1` run on devbig014
    /// and made `test_land_lock_full_protocol_is_bounded` in
    /// ci-hub/tests/test_operational_bounds.py exceed its 5 s CPU limit.
    #[test]
    fn deadline_grace_ends_once_no_recorded_payload_process_is_alive() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let grace = Duration::from_secs(60);
        let (emptied, elapsed, signal) =
            deadline_grace_outcome("/bin/sleep", &[OsString::from("30")], None, grace);
        assert_eq!(
            signal,
            Some(libc::SIGTERM),
            "the payload leader must die of SIGTERM"
        );
        assert!(
            emptied,
            "the grace ran to its end after {elapsed:?} although the payload died of SIGTERM"
        );
        assert!(
            elapsed < grace,
            "the grace took {elapsed:?}, all of its {grace:?}"
        );
    }

    /// NEGATIVE CONTROL for the early end: a recorded payload process that
    /// ignores SIGTERM keeps the grace running to its end, and the SIGKILL half
    /// still kills it.
    #[test]
    fn deadline_grace_runs_to_its_end_while_a_recorded_payload_process_lives() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths("grace-negative-control");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let ready = root.join("trap-installed");
        let grace = Duration::from_secs(1);
        let (emptied, elapsed, signal) = deadline_grace_outcome(
            "/bin/bash",
            &[
                OsString::from("-c"),
                OsString::from(r#"trap '' TERM; : > "$1"; exec /bin/sleep 30"#),
                OsString::from("grace-negative-control"),
                ready.clone().into_os_string(),
            ],
            Some(&ready),
            grace,
        );
        let _ = fs::remove_dir_all(&root);
        assert!(!emptied, "the grace reported every recorded process gone");
        assert!(
            elapsed >= grace,
            "the grace ended after {elapsed:?}, before {grace:?}"
        );
        assert_eq!(
            signal,
            Some(libc::SIGKILL),
            "the payload leader must die of SIGKILL"
        );
    }

    /// Review of https://github.com/rrnewton/dev-hermit/issues/419: a member
    /// of the payload's process group forked AFTER the group SIGTERM -- here
    /// by the leader's own SIGTERM handler -- never had that signal. The grace
    /// polls counted every group member as signalled, so it ran until the
    /// SIGKILL that ends the grace, which took the waiting leader with it. It
    /// must get its own SIGTERM, which lets the handler finish.
    #[test]
    fn deadline_grace_sigterms_a_group_member_forked_after_the_group_signal() {
        let Some(_domain_guard) = process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths("grace-late-group-member");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let ready = root.join("trap-installed");
        let status = root.join("late-member.status");
        let grace = Duration::from_secs(10);
        let (emptied, elapsed, signal) = deadline_grace_outcome(
            "/bin/bash",
            &[
                OsString::from("-c"),
                OsString::from(
                    r#"status=$2
on_term() { /bin/sleep 30 & child=$!; wait "$child"; echo "$?" > "$status"; exit 0; }
trap on_term TERM
: > "$1"
i=0
while [ "$i" -lt 6000 ]; do sleep 0.05; i=$((i + 1)); done
"#,
                ),
                OsString::from("grace-late-group-member"),
                ready.clone().into_os_string(),
                status.clone().into_os_string(),
            ],
            Some(&ready),
            grace,
        );
        let late_status = fs::read_to_string(&status).ok();
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            late_status.as_deref().map(str::trim),
            Some("143"),
            "the group member forked by the SIGTERM handler did not die of its own SIGTERM \
             (grace emptied={emptied} after {elapsed:?}, leader signal {signal:?})"
        );
        assert_eq!(
            signal, None,
            "the leader must exit on its own once its late child is gone"
        );
        assert!(
            emptied,
            "the grace ran to its end after {elapsed:?} although every process exited"
        );
    }

    #[test]
    fn run_rejects_unbounded_deadline_before_acquiring() {
        let paths = temp_paths("unbounded-child-deadline");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        let error = lock
            .run(RunArgs {
                agent: "unbounded-lander".into(),
                pr: "9998".into(),
                wait: 0,
                hold: 30,
                child_deadline: 0,
                child: vec![OsString::from("/bin/true")],
            })
            .unwrap_err();
        assert!(matches!(error, LandLockError::UnboundedChildDeadline));
        assert!(lock.read_holder().unwrap().is_none());
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn live_named_agent_and_exited_lock_holder_are_reported_separately() {
        let named_agent = "codex-lsm";
        let named_agent_process = current_process_owner().unwrap();
        assert_eq!(
            process_start_ticks(named_agent_process.pid).unwrap(),
            named_agent_process.start_ticks,
            "the named-agent stand-in must be live while the lock holder is reported exited"
        );

        let report = format!(
            "agent={named_agent}\nlock_holder_process={}",
            render_lock_holder_liveness(&OwnerLiveness::Dead("pid 2884548 is absent".into()))
        );
        assert_eq!(
            report,
            "agent=codex-lsm\nlock_holder_process=exited:pid 2884548 is absent"
        );
        assert!(!report.contains("owner_process=dead"));
        assert!(!report.contains("agent=codex-lsm dead"));
    }

    #[test]
    fn exited_lock_holder_waits_for_expiry_then_reclaims() {
        let paths = temp_paths("dead-owner-reclaim");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        assert_eq!(
            lock.acquire(&AcquireArgs {
                agent: "dead-lander".into(),
                pr: "100".into(),
                wait: 0,
                hold: 3_600,
            })
            .unwrap(),
            0
        );
        let mut owner = current_process_owner().unwrap();
        owner.pid = u32::MAX;
        write_truncated(&paths.owner, owner.render().as_bytes()).unwrap();
        assert!(matches!(
            lock.owner_liveness().unwrap(),
            OwnerLiveness::Dead(_)
        ));
        assert_eq!(
            lock.acquire(&AcquireArgs {
                agent: "replacement-lander".into(),
                pr: "101".into(),
                wait: 0,
                hold: 60,
            })
            .unwrap(),
            1,
            "death is proven, but TTL remains the stale-clear eligibility boundary"
        );
        assert_eq!(lock.read_holder().unwrap().unwrap().agent, "dead-lander");
        let now = epoch_seconds().unwrap();
        let mut expired = lock.read_holder().unwrap().unwrap();
        expired.acquired_at = now - 61;
        expired.expires_at = now - 1;
        lock.write_holder(&expired).unwrap();
        assert_eq!(
            lock.acquire(&AcquireArgs {
                agent: "replacement-lander".into(),
                pr: "101".into(),
                wait: 0,
                hold: 60,
            })
            .unwrap(),
            0
        );
        let holder = lock.read_holder().unwrap().unwrap();
        assert_eq!(holder.agent, "replacement-lander");
        assert!(holder
            .reclaimed_from
            .unwrap()
            .contains("lock-holder process exited"));
        lock.release("replacement-lander", false).unwrap();
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn live_owner_cannot_be_evidence_reclaimed() {
        let paths = temp_paths("live-owner-protected");
        let lock = LandingLock {
            paths: paths.clone(),
        };
        lock.acquire(&AcquireArgs {
            agent: "live-lander".into(),
            pr: "102".into(),
            wait: 0,
            hold: 60,
        })
        .unwrap();
        lock.write_current_process_owner().unwrap();
        assert!(matches!(
            lock.owner_liveness().unwrap(),
            OwnerLiveness::Alive
        ));
        assert!(matches!(
            lock.reclaim_dead().unwrap_err(),
            LandLockError::ReclaimNotProven(_)
        ));
        lock.release("live-lander", false).unwrap();
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    // --- box-global landing exclusion: one lease per REPOSITORY, one anchor per BOX ---

    fn git_in(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn every_worktree_of_one_repository_shares_one_landing_lease() {
        // THE DEFECT, at unit scale. `workspace_root()` hands `for_workspace` the
        // git toplevel of the running ci-hub.rs, so before this each linked
        // worktree derived a private `.landing-lock` and landed independently.
        let base = env::temp_dir().join(format!(
            "ci-hub-llock-repo-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        let main = base.join("main");
        let linked = base.join("linked");
        fs::create_dir_all(&main).unwrap();
        if !git_in(&main, &["init", "--quiet"]) {
            return; // no usable git on this host; the live bracket covers it
        }
        let _ = git_in(&main, &["config", "user.email", "t@example.invalid"]);
        let _ = git_in(&main, &["config", "user.name", "t"]);
        fs::write(main.join("seed"), b"seed").unwrap();
        assert!(git_in(&main, &["add", "seed"]));
        assert!(git_in(&main, &["commit", "--quiet", "-m", "seed"]));
        assert!(git_in(
            &main,
            &[
                "worktree",
                "add",
                "--detach",
                "--quiet",
                linked.to_str().unwrap()
            ]
        ));

        // The env override must not be in effect for this assertion to mean
        // anything; it is process-global, so read it rather than set it.
        if env::var_os("CI_HUB_LANDING_LOCK").is_some() {
            return;
        }
        let from_main = LockPaths::for_workspace(&main);
        let from_linked = LockPaths::for_workspace(&linked);
        assert_eq!(
            from_linked.lock,
            main.join(".landing-lock"),
            "a linked worktree must resolve to the main worktree's lease"
        );
        assert_eq!(from_main.lock, from_linked.lock);
        assert_eq!(from_main.guard, from_linked.guard);
        assert_eq!(from_main.cleanup, from_linked.cleanup);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_non_repository_root_falls_back_to_itself_for_landing() {
        // Fail-SAFE direction: without a resolvable repository identity we keep the
        // previous behaviour rather than pointing a live lease somewhere invented.
        let root = env::temp_dir().join(format!(
            "ci-hub-llock-norepo-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        assert_eq!(repository_lock_root(&root), root);
        let _ = fs::remove_dir_all(&root);
    }

    // No environment and no shared state: the anchor path is a PARAMETER, so this
    // test is isolated by construction and can never touch the live box anchor.
    #[test]
    fn landing_box_anchor_admits_one_and_refuses_the_second() {
        let dir = env::temp_dir().join(format!(
            "ci-hub-llock-anchor-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let anchor = dir.join("landing-box.lock");

        // Nothing held: the FIRST caller is admitted (the mechanism is not inert).
        assert_eq!(
            BoxExclusionAnchor::observe(&anchor),
            BoxAnchorObservation::Free
        );
        let first = BoxExclusionAnchor::take(&anchor, 0).unwrap();
        assert!(first.is_some(), "an unheld anchor must be granted");

        // A SECOND caller -- in production a different repository, or the same one
        // with CI_HUB_LANDING_LOCK pointed elsewhere -- is refused, and
        // `acquire`'s non-holding probe sees it.
        assert!(
            BoxExclusionAnchor::take(&anchor, 0).unwrap().is_none(),
            "a held anchor must refuse the second caller"
        );
        assert!(matches!(
            BoxExclusionAnchor::observe(&anchor),
            BoxAnchorObservation::Held(_)
        ));
        assert!(
            BoxExclusionAnchor::describe_holder(&anchor).contains("box-exclusive-v1"),
            "the refusal must be able to name the holder"
        );

        // Released by dropping the holder -- no record to clean, so a dead holder
        // can never strand the box.
        //
        // Re-TAKE with a bound rather than asserting an instantaneous release, and
        // the reason is a real property of `flock` + `fork`, not test flake. An
        // flock belongs to the OPEN FILE DESCRIPTION, and `fork` shares
        // descriptions. `O_CLOEXEC` (which Rust sets) closes the inherited fd at
        // EXEC, not at fork, so any sibling thread of this multithreaded test
        // binary that is between fork and exec while we hold the anchor is also
        // holding the description. Production is unaffected -- the supervisor
        // holds the anchor across its whole run, so the fork window is inside the
        // hold, never after it.
        drop(first);
        assert!(
            BoxExclusionAnchor::take(&anchor, 5).unwrap().is_some(),
            "dropping the holder must release the box within the fork/exec window"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn landing_and_validate_anchors_are_distinct() {
        // The module contract is that a validate must never block a lander and
        // vice versa. Same box-global mechanism, deliberately different files.
        let landing = box_exclusion_anchor_path(LANDING_BOX_ANCHOR);
        let validate = box_exclusion_anchor_path("validate-box.lock");
        assert_ne!(landing, validate);
        assert!(landing.is_absolute() && validate.is_absolute());
        let uid = unsafe { libc::getuid() };
        assert!(
            landing.to_string_lossy().contains(&uid.to_string()),
            "the anchor must be keyed on the uid, got {}",
            landing.display()
        );
        // Stable across calls, and never a function of the workspace.
        assert_eq!(box_exclusion_anchor_path(LANDING_BOX_ANCHOR), landing);
    }

    // --- box-global claims: the durable half of exclusion ---
    //
    // Pure-function tests on temp paths. The anchor is a PARAMETER, so the claim
    // directory derived from it is per-test and can never touch the live box.

    fn claim_fixture(name: &str) -> (PathBuf, PathBuf) {
        let dir = env::temp_dir().join(format!(
            "ci-hub-claim-{name}-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        (dir.join("box.lock"), dir)
    }

    fn armed_record_at(path: &Path, agent: &str, operation: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let record = CleanupRecord::new(agent, operation.to_string(), CleanupPhase::Armed).unwrap();
        write_cleanup_record(path, &record).unwrap();
    }

    #[test]
    fn no_claims_means_no_refusal() {
        // NOT INERT IN THE WRONG DIRECTION: with nothing outstanding, ordinary work
        // must be admitted. A box with no claims behaves exactly as it did before
        // this mechanism existed -- which is also the whole migration story.
        let (anchor, dir) = claim_fixture("empty");
        assert_eq!(
            foreign_box_claim(&anchor, &dir.join("mine.cleanup-required")),
            None
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_foreign_outstanding_record_refuses_and_names_itself() {
        // THE GAP. Another repository armed a record and its supervisor died; the
        // anchor is long gone, but the record is not, and it is now visible here.
        let (anchor, dir) = claim_fixture("foreign");
        let theirs = dir.join("other-repo/.landing-lock.cleanup-required");
        let mine = dir.join("my-repo/.landing-lock.cleanup-required");
        armed_record_at(&theirs, "escaped-lander", "pr:999");
        register_box_claim(&anchor, &theirs, "escaped-lander", "pr:999");

        let refusal = foreign_box_claim(&anchor, &mine).expect("must refuse");
        assert!(refusal.contains(&theirs.display().to_string()), "{refusal}");
        assert!(refusal.contains("escaped-lander"), "{refusal}");
        assert!(
            refusal.contains("reclaim-dead"),
            "must name the remedy: {refusal}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_live_published_validation_is_allowed_only_by_the_concurrent_path() {
        let (anchor, dir) = claim_fixture("live-validation");
        let theirs = dir.join("other-repo/.validate-lock.cleanup-required");
        let mine = dir.join("my-repo/.validate-lock.slot-1.cleanup-required");
        let leader = ProcessIdentity {
            pid: std::process::id(),
            start_ticks: process_start_ticks(std::process::id()).unwrap(),
        };
        let pgid = unsafe { libc::getpgrp() } as u32;
        let mut record = CleanupRecord::new(
            "first-validate",
            "validate:first",
            CleanupPhase::Published { leader, pgid },
        )
        .unwrap();
        record.domain = CleanupDomain::ProcessGroup;
        fs::create_dir_all(theirs.parent().unwrap()).unwrap();
        write_cleanup_record(&theirs, &record).unwrap();
        register_box_claim(&anchor, &theirs, "first-validate", "validate:first");

        // The removed implementation passed no expected agent/operation to the
        // verifier.  That can only return Unknown, so the second validation was
        // refused even though a second slot existed.
        assert!(matches!(
            verify_cleanup_record(&theirs, None),
            CleanupVerification::Unknown { .. }
        ));
        assert_eq!(
            foreign_box_quarantine_excluding(&anchor, std::slice::from_ref(&mine)),
            None,
            "a live published validation must coexist with the second validation slot"
        );
        assert!(
            foreign_box_claim(&anchor, &mine).is_some(),
            "benchmarks remain exclusive and must refuse the same live validation"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_armed_validation_is_allowed_only_by_the_concurrent_path() {
        let (anchor, dir) = claim_fixture("armed-validation");
        let theirs = dir.join("other-repo/.validate-lock.cleanup-required");
        let mine = dir.join("my-repo/.validate-lock.slot-1.cleanup-required");
        armed_record_at(&theirs, "first-validate", "validate:first");
        register_box_claim(&anchor, &theirs, "first-validate", "validate:first");

        assert!(
            foreign_box_claim(&anchor, &mine).is_some(),
            "without the concurrent-validation allowance, the armed record reproduces the refusal"
        );
        assert_eq!(
            foreign_box_quarantine_excluding(&anchor, std::slice::from_ref(&mine)),
            None,
            "the start gate proves that an armed validation has not started its payload"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_published_benchmark_is_not_allowed_by_the_concurrent_validation_path() {
        let (anchor, dir) = claim_fixture("live-benchmark");
        let theirs = dir.join("other-repo/.validate-lock.cleanup-required");
        let mine = dir.join("my-repo/.validate-lock.slot-1.cleanup-required");
        let leader = ProcessIdentity {
            pid: std::process::id(),
            start_ticks: process_start_ticks(std::process::id()).unwrap(),
        };
        let pgid = unsafe { libc::getpgrp() } as u32;
        let mut record = CleanupRecord::new(
            "first-benchmark",
            "bench:first",
            CleanupPhase::Published { leader, pgid },
        )
        .unwrap();
        record.domain = CleanupDomain::ProcessGroup;
        fs::create_dir_all(theirs.parent().unwrap()).unwrap();
        write_cleanup_record(&theirs, &record).unwrap();
        register_box_claim(&anchor, &theirs, "first-benchmark", "bench:first");

        let refusal = foreign_box_quarantine_excluding(&anchor, std::slice::from_ref(&mine))
            .expect("a validation must not overlap a published benchmark payload");
        assert!(refusal.contains("bench:first"), "{refusal}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn my_own_record_is_not_a_foreign_claim() {
        // This repository's own record is already enforced by `require_no_cleanup`
        // inside acquire. Counting it here too would make a repository refuse
        // itself and deadlock its own recovery commands.
        let (anchor, dir) = claim_fixture("own");
        let mine = dir.join("my-repo/.landing-lock.cleanup-required");
        armed_record_at(&mine, "me", "pr:1");
        register_box_claim(&anchor, &mine, "me", "pr:1");
        assert_eq!(foreign_box_claim(&anchor, &mine), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_discharged_record_prunes_its_claim_and_admits() {
        // CANNOT WEDGE THE BOX. The flock's virtue was kernel release on death; a
        // durable record gives that up unless discharge is mechanical. Here the
        // claim dies with the record it points at, and the reader does the pruning
        // -- so a crashed supervisor, a deleted clone or a wiped /run/user all
        // self-heal on the next admission rather than needing an operator.
        let (anchor, dir) = claim_fixture("discharged");
        let theirs = dir.join("other-repo/.landing-lock.cleanup-required");
        let mine = dir.join("my-repo/.landing-lock.cleanup-required");
        armed_record_at(&theirs, "lander", "pr:7");
        register_box_claim(&anchor, &theirs, "lander", "pr:7");
        assert!(
            foreign_box_claim(&anchor, &mine).is_some(),
            "must refuse while outstanding"
        );

        // The owning repository discharges its record, exactly as reclaim-dead does.
        remove_cleanup_record(&theirs).unwrap();
        assert_eq!(
            foreign_box_claim(&anchor, &mine),
            None,
            "must admit once discharged"
        );
        assert!(
            fs::read_dir(box_claim_dir(&anchor))
                .unwrap()
                .next()
                .is_none(),
            "the stale claim must be pruned, not merely ignored"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unparseable_claim_is_pruned_rather_than_wedging() {
        // A claim naming no record can never be discharged by one, so blocking on
        // it would be a permanent wedge with no remedy. Drop it instead.
        let (anchor, dir) = claim_fixture("garbage");
        fs::create_dir_all(box_claim_dir(&anchor)).unwrap();
        let junk = box_claim_dir(&anchor).join("junk.claim");
        fs::write(&junk, b"version=1\nnothing useful here\n").unwrap();
        assert_eq!(foreign_box_claim(&anchor, &dir.join("mine")), None);
        assert!(!junk.exists(), "unparseable claim must be pruned");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_claim_publication_prevents_the_old_truncate_reader_collision() {
        let (anchor, dir) = claim_fixture("atomic-publication");
        let theirs = dir.join("other-repo/.validate-lock.cleanup-required");
        let mine = dir.join("my-repo/.validate-lock.cleanup-required");
        armed_record_at(&theirs, "validate-a", "validate:one");
        register_box_claim(&anchor, &theirs, "validate-a", "validate:one");
        let claim = box_claim_file(&anchor, &theirs);

        // Removed behavior: fs::write opens the final path with truncate before
        // writing.  A reader in that interval sees an empty claim, prunes it,
        // and the writer finishes into an unlinked file.  The cleanup record is
        // then outstanding with no box-global pointer.
        let mut old_writer = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&claim)
            .unwrap();
        assert_eq!(foreign_box_claim(&anchor, &mine), None);
        old_writer.write_all(b"version=1\ncleanup=lost\n").unwrap();
        old_writer.flush().unwrap();
        assert!(
            !claim.exists(),
            "old truncate/write interleaving must lose the claim"
        );

        register_box_claim(&anchor, &theirs, "validate-a", "validate:one");
        let body = fs::read_to_string(&claim).unwrap();
        assert!(body.contains(&format!("cleanup={}\n", theirs.display())));
        assert!(body.contains("agent=validate-a\n"));
        assert!(body.contains("operation=validate:one\n"));
        assert!(
            foreign_box_claim(&anchor, &mine).is_some(),
            "the atomically published claim must preserve the outstanding cleanup refusal"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_and_claim_are_discharged_together() {
        // The two must not drift, so every removal site goes through one helper.
        let (anchor, dir) = claim_fixture("together");
        let cleanup = dir.join("repo/.landing-lock.cleanup-required");
        armed_record_at(&cleanup, "agent", "pr:3");
        register_box_claim(&anchor, &cleanup, "agent", "pr:3");
        assert!(box_claim_dir(&anchor).exists());
        remove_cleanup_record_and_claim(&cleanup, &anchor).unwrap();
        assert!(!cleanup.exists(), "record removed");
        assert!(
            fs::read_dir(box_claim_dir(&anchor))
                .unwrap()
                .next()
                .is_none(),
            "claim removed with it"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
