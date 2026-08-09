//! Box-exclusive-compute admission controller: at most ONE validate OR benchmark
//! job runs on this box at a time.
//!
//! Concurrent heavy load manufactures false test FAILEDs (detcore_misc residual
//! is monotonic in load; see `experiments/multisect_detcore_misc_20260803`), so a
//! second box-exclusive job is never silently admitted. This is a SEPARATE lock
//! from the landing mutex (`landing_lock.rs`): a validate must never block a
//! lander and vice versa, so they key on different lockfiles. The lease engine
//! (guard flock + FIFO queue + evidence-based dead-owner reclaim + bounded
//! child-deadline heartbeat) mirrors `landing_lock.rs` exactly on its own files;
//! pure, stateless helpers are reused from that module rather than re-derived.

use chrono::{Local, TimeZone};
use clap::{Args, Subcommand, ValueEnum};
use fs2::FileExt;
use serde::Serialize;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

// Reuse the PURE, stateless primitives from landing_lock rather than copy tricky
// code. None of these carry a LandLockError or print a `landing-lock:` message,
// so they are safe to share verbatim across both locks.
use crate::landing_lock::{
    box_exclusion_anchor_path as box_exclusion_anchor_at, capture_and_freeze_residuals,
    census_disposition, census_recorded_domain, current_host, enable_child_subreaper,
    exact_process_liveness, exit_status_code, foreign_box_claim, heartbeat_test_helper_delay,
    payload_cgroup_anchor, print_cleanup_record, process_group_exists, process_start_ticks,
    reap_exited_children, register_box_claim, remove_cleanup_record_and_claim,
    repository_lock_root, signal_group, spawn_gated_child, suffix, verify_cleanup_record,
    write_cleanup_record, BoxExclusionAnchor, CleanupPhase, CleanupRecord, CleanupVerification,
    DomainEvidence, GatedChild, ProcessIdentity, ResidualCapture,
};

const DEFAULT_WAIT_SECONDS: u64 = 1_800;
const DEFAULT_HOLD_SECONDS: u64 = 900;
const POLL_SECONDS: u64 = 3;
const GUARD_WAIT_SECONDS: u64 = 30;
/// Box-exclusive cap. Raising this is UNPROVEN: detcore_misc false-FAILED
/// residual is monotonic in load (`experiments/multisect_detcore_misc_20260803`),
/// so >1 requires hermit-250 evidence, not a config pick.
const BOX_EXCLUSIVE_CAP: u64 = 1;
/// Hard ceiling on how long a `run` child may execute before it is killed and
/// cleanup is proved. A complete empty-domain proof releases the box; otherwise
/// it stays quarantined. A warm validate runs ~8-16 min; this default (3600s)
/// allows generous headroom while still bounding cleanup or quarantine — an
/// unbounded holder is unboxed compute and a head-of-line block.
const DEFAULT_CHILD_DEADLINE_SECONDS: u64 = 3_600;
/// Exit code reported when a `run` child is killed for exceeding its deadline.
const CHILD_DEADLINE_EXIT_CODE: i32 = 124;
/// Exit code reported when `--no-wait` refuses admission (lock not immediately
/// grantable). Distinct, non-zero, and mapped to 3 like an ownership refusal.
const REFUSED_EXIT_CODE: i32 = 3;
/// Grace period between SIGTERM and SIGKILL when terminating a timed-out child.
const CHILD_TERM_GRACE_SECONDS: u64 = 5;
/// Interval at which `run` polls a live child for completion or deadline breach.
const CHILD_POLL_MILLIS: u64 = 500;
/// Exit code reported when admission REFUSES a validate whose head predates an
/// immutable fixed floor. Same 3 as an ownership refusal: a refusal, not a
/// crash. Mutable-tip currency belongs only at the final merge boundary.
const STALE_BASE_EXIT_CODE: i32 = 3;
/// TEST-BUILD-ONLY override of the composite validation-admission command
/// (space-split argv). The target head is appended as `--head <sha>`.
/// Production always runs the in-tree `ci-hub/validate/preflight_validate.py`,
/// which checks the immutable producer and merge-gate floors. See
/// [`test_only_override`] for why the name is compiled into BOTH builds while
/// its effect exists only under `cfg(test)`.
const ADMIT_PREFLIGHT_CMD_ENV: &str = "CI_HUB_ADMIT_PREFLIGHT_CMD";
/// TEST-BUILD-ONLY override of the box-exclusive lockfile base path. Production
/// always uses `<repository main worktree>/.validate-lock`.
const VALIDATE_LOCK_PATH_ENV: &str = "CI_HUB_VALIDATE_LOCK";
/// Anchor filename for VALIDATE. Distinct from `landing_lock::LANDING_BOX_ANCHOR`
/// on purpose: a validate must never block a lander and vice versa, so the two
/// are box-global with respect to their own kind and invisible to each other.
const VALIDATE_BOX_ANCHOR: &str = "validate-box.lock";

/// Read a TEST-BUILD-ONLY environment override.
///
/// **AN ADMISSION GATE WHOSE VERDICT ITS CALLER CAN SET IS NOT A GATE.** Both
/// variables above began as convenience hooks so Rust tests would need neither
/// Python, egress, nor a Hermit checkout — and both were live in the production
/// admission path. Measured 2026-08-08 on the then-current `origin/main`
/// (`ci-hub validate-lock run --kind validate --target <nonexistent 40-hex>`):
/// `CI_HUB_ADMIT_PREFLIGHT_CMD=false` refused with exit 3 and 0 children, while
/// `=true` ADMITTED the same nonexistent SHA, exit 0, 1 child. Separately,
/// `CI_HUB_VALIDATE_LOCK=<tmp>` acquired the box with the workspace's real
/// `.validate-lock*` never created at all — box exclusivity escaped by an
/// ambient variable that any inherited environment could carry.
///
/// The names stay compiled into every build ON PURPOSE: a production binary
/// must be able to SAY that it saw the override and refused it. Silently
/// ignoring a variable an operator deliberately exported is its own trap — they
/// would believe the stub took effect. The NOTICE is the whole observable
/// difference; the admission verdict, the lock target, and the spawned-child
/// count are identical whether the variables are set or unset.
///
/// Ignoring (rather than refusing) is fail-SAFE in both directions: an
/// inert `CI_HUB_ADMIT_PREFLIGHT_CMD` means the real preflight runs, and an
/// inert `CI_HUB_VALIDATE_LOCK` means the real box lock is contended. A hard
/// refusal would instead let one stale exported variable wedge every validate
/// on the box.
#[cfg(test)]
fn test_only_override(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

#[cfg(not(test))]
fn test_only_override(name: &str) -> Option<OsString> {
    if env::var_os(name).is_some_and(|value| !value.is_empty()) {
        eprintln!(
            "validate-lock: NOTICE: {name} is set but INERT in this build — it is \
             a test-build-only override, compiled out of production. Admission \
             uses the in-tree preflight and the workspace box lock regardless. \
             To exercise the override, run the test binary \
             (`rust-script --test ci-hub/ci-hub.rs`)."
        );
    }
    None
}

#[derive(Args, Clone, Debug)]
pub struct ValidateLockArgs {
    #[command(subcommand)]
    pub command: ValidateLockCommand,
}

#[derive(Subcommand, Clone, Debug)]
pub enum ValidateLockCommand {
    /// Wait in FIFO order and acquire the box-exclusive lease.
    Acquire(AcquireArgs),
    /// Refresh a lease owned by this agent.
    Renew(RenewArgs),
    /// Release a lease owned by this agent.
    Release(ReleaseArgs),
    /// Print holder metadata and the FIFO queue (with 1-based positions).
    Status,
    /// Emit one guarded, machine-readable canonical lock-authority snapshot.
    AuthorityStatus(AuthorityStatusArgs),
    /// Reclaim a lease only when its recorded owner process is proven dead.
    ReclaimDead,
    /// Census an UNCENSUSED payload domain post-hoc, after the supervisor died
    /// before it could take one, so `reclaim-dead` can finish.
    CensusOrphanedDomain(CensusOrphanedDomainArgs),
    /// Acquire and run with a heartbeat + child-deadline; release after complete
    /// cleanup proof, otherwise retain a quarantine.
    Run(RunArgs),
}

#[derive(Args, Clone, Debug)]
pub struct AuthorityStatusArgs {
    /// Emit the stable versioned JSON authority contract.
    #[arg(long, required = true)]
    pub json: bool,
}

/// Restated identity plus the one operator attestation that discharges an
/// UNCENSUSED quarantine. Every field is re-checked against the durable record;
/// a mismatch refuses, so this cannot be run blind against whatever happens to
/// be quarantined at the time.
#[derive(Args, Clone, Debug)]
pub struct CensusOrphanedDomainArgs {
    /// Must equal the recorded `agent` of the quarantined operation.
    #[arg(long)]
    pub agent: String,
    /// Must equal the recorded `operation` (`<kind>:<target>`).
    #[arg(long)]
    pub operation: String,
    /// Must equal the recorded published `leader` as `<pid>:<start_ticks>`.
    #[arg(long)]
    pub leader: String,
    /// Must equal the recorded published `pgid`.
    #[arg(long)]
    pub pgid: u32,
    /// Affirms that the payload's process domain was observed empty by a
    /// supervisor-independent authority (unit cgroup absent/empty AND every
    /// cgroup the payload can migrate into empty).
    ///
    /// NOT required when the record carries a cgroup anchor and the kernel
    /// already says the domain is empty -- that census is mechanical. This is
    /// only for records with no anchor, or where the anchor cannot be read.
    #[arg(long)]
    pub attest_domain_empty: bool,
    /// The exact observations backing `--attest-domain-empty`. Recorded in the
    /// transcript so the attestation is auditable rather than anonymous. Must be
    /// non-empty whenever `--attest-domain-empty` is used.
    ///
    /// Optional at the CLI so a mechanically provable census can be run exactly
    /// as `status` prints it, with no attestation flags at all; it is still
    /// enforced as non-empty on every path that actually consumes it.
    #[arg(long, default_value = "")]
    pub evidence: String,
}

/// A box-exclusive job kind. BOTH kinds share the ONE lock, so a validate and a
/// bench are mutually exclusive — the point is total box exclusivity, not
/// per-kind exclusivity.
#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum Kind {
    Validate,
    Bench,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Validate => "validate",
            Kind::Bench => "bench",
        }
    }

    fn parse(value: &str) -> Result<Self, ValidateLockError> {
        match value {
            "validate" => Ok(Kind::Validate),
            "bench" => Ok(Kind::Bench),
            other => Err(ValidateLockError::InvalidState(format!(
                "unknown kind {other:?}, expected validate|bench"
            ))),
        }
    }
}

#[derive(Args, Clone, Debug)]
pub struct AcquireArgs {
    #[arg(long)]
    pub agent: String,
    #[arg(long, value_enum)]
    pub kind: Kind,
    #[arg(long)]
    pub target: String,
    #[arg(long, default_value_t = DEFAULT_WAIT_SECONDS)]
    pub wait: u64,
    #[arg(long, default_value_t = DEFAULT_HOLD_SECONDS)]
    pub hold: u64,
    #[arg(long, default_value_t = BOX_EXCLUSIVE_CAP)]
    pub max: u64,
    /// Retained only to give old callers a named refusal. Qualifying validation
    /// can no longer bypass base admission. Historical differential debugging
    /// must run outside the receipt-producing validate path.
    #[arg(long, default_value_t = false)]
    pub skip_base_check: bool,
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
    #[arg(long, value_enum)]
    pub kind: Kind,
    #[arg(long)]
    pub target: String,
    /// Do not block: if the box is not immediately grantable, print a REFUSED
    /// reason to stderr and exit 3 instead of queueing indefinitely.
    #[arg(long, default_value_t = false)]
    pub no_wait: bool,
    #[arg(long, default_value_t = DEFAULT_WAIT_SECONDS)]
    pub wait: u64,
    #[arg(long, default_value_t = DEFAULT_HOLD_SECONDS)]
    pub hold: u64,
    /// Kill the child if it runs longer than this many seconds. Release follows
    /// only after complete cleanup proof; otherwise the box remains quarantined.
    /// Must be positive: unbounded box-exclusive holders are forbidden.
    #[arg(long, default_value_t = DEFAULT_CHILD_DEADLINE_SECONDS)]
    pub child_deadline: u64,
    #[arg(long, default_value_t = BOX_EXCLUSIVE_CAP)]
    pub max: u64,
    /// Retained only to give old callers a named refusal. Qualifying validation
    /// can no longer bypass base admission. Historical differential debugging
    /// must run outside the receipt-producing validate path.
    #[arg(long, default_value_t = false)]
    pub skip_base_check: bool,
    #[arg(last = true, required = true)]
    pub child: Vec<OsString>,
}

impl ValidateLockCommand {
    pub fn consumes_meaningful_time(&self) -> bool {
        matches!(self, Self::Acquire(_) | Self::Run(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidateLockState {
    pub agent: String,
    pub kind: String,
    pub target: String,
    pub host: String,
    pub acquired_at: i64,
    pub acquired_human: String,
    pub expires_at: i64,
    pub reclaimed_from: Option<String>,
}

impl ValidateLockState {
    fn parse(content: &str) -> Result<Self, ValidateLockError> {
        let mut agent = None;
        let mut kind = None;
        let mut target = None;
        let mut host = None;
        let mut acquired_at = None;
        let mut acquired_human = None;
        let mut expires_at = None;
        let mut reclaimed_from = None;
        for (line_number, line) in content.lines().enumerate() {
            let (key, value) = line.split_once('=').ok_or_else(|| {
                ValidateLockError::InvalidState(format!(
                    "holder line {} is not key=value",
                    line_number + 1
                ))
            })?;
            match key {
                "agent" => agent = Some(value.to_string()),
                "kind" => kind = Some(value.to_string()),
                "target" => target = Some(value.to_string()),
                "host" => host = Some(value.to_string()),
                "acquired_at" => acquired_at = Some(parse_integer(key, value)?),
                "acquired_human" => acquired_human = Some(value.to_string()),
                "expires_at" => expires_at = Some(parse_integer(key, value)?),
                "reclaimed_from" => reclaimed_from = Some(value.to_string()),
                unknown => {
                    return Err(ValidateLockError::InvalidState(format!(
                        "unknown holder field {unknown:?}"
                    )));
                }
            }
        }
        Ok(Self {
            agent: required(agent, "agent")?,
            kind: required(kind, "kind")?,
            target: required(target, "target")?,
            host: required(host, "host")?,
            acquired_at: required(acquired_at, "acquired_at")?,
            acquired_human: required(acquired_human, "acquired_human")?,
            expires_at: required(expires_at, "expires_at")?,
            reclaimed_from,
        })
    }

    fn render(&self) -> String {
        let mut output = format!(
            "agent={}\nkind={}\ntarget={}\nhost={}\nacquired_at={}\nacquired_human={}\nexpires_at={}\n",
            self.agent,
            self.kind,
            self.target,
            self.host,
            self.acquired_at,
            self.acquired_human,
            self.expires_at
        );
        if let Some(reclaimed_from) = &self.reclaimed_from {
            output.push_str(&format!("reclaimed_from={reclaimed_from}\n"));
        }
        output
    }

    fn live_at(&self, now: i64) -> bool {
        now < self.expires_at
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueEntry {
    pub enqueued_at: i64,
    pub agent: String,
    pub target: String,
}

impl QueueEntry {
    fn parse(line: &str, line_number: usize) -> Result<Self, ValidateLockError> {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 3 {
            return Err(ValidateLockError::InvalidState(format!(
                "queue line {line_number} must have three tab-separated fields"
            )));
        }
        Ok(Self {
            enqueued_at: parse_integer("queue timestamp", fields[0])?,
            agent: fields[1].to_string(),
            target: fields[2].to_string(),
        })
    }

    fn render(&self) -> String {
        format!("{}\t{}\t{}\n", self.enqueued_at, self.agent, self.target)
    }
}

/// Process-identity sidecar: proves whether the recorded owner is still the same
/// live process (boot_id defeats reboots; start_ticks defeats pid reuse).
#[derive(Clone, Debug, Eq, PartialEq)]
struct ProcessOwner {
    host: String,
    boot_id: String,
    pid: u32,
    start_ticks: u64,
}

impl ProcessOwner {
    fn parse(content: &str) -> Result<Self, ValidateLockError> {
        let mut host = None;
        let mut boot_id = None;
        let mut pid = None;
        let mut start_ticks = None;
        for (line_number, line) in content.lines().enumerate() {
            let (key, value) = line.split_once('=').ok_or_else(|| {
                ValidateLockError::InvalidState(format!(
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
                    return Err(ValidateLockError::InvalidState(format!(
                        "unknown owner field {unknown:?}"
                    )));
                }
            }
        }
        Ok(Self {
            host: required(host, "owner host")?,
            boot_id: required(boot_id, "owner boot_id")?,
            pid: u32::try_from(required(pid, "owner pid")?)
                .map_err(|_| ValidateLockError::InvalidState("owner pid exceeds u32".into()))?,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum AuthorityState {
    Held,
    Free,
    Lapsed,
    Orphaned,
    Quarantined,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum AuthorityCleanupState {
    None,
    ActiveBound,
    Armed,
    Recoverable,
    Uncensused,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum AuthorityOwnerLiveness {
    Alive,
    Dead,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct AuthorityHolder {
    agent: String,
    kind: String,
    target: String,
    host: String,
    acquired_at: i64,
    expires_at: i64,
}

impl From<&ValidateLockState> for AuthorityHolder {
    fn from(holder: &ValidateLockState) -> Self {
        Self {
            agent: holder.agent.clone(),
            kind: holder.kind.clone(),
            target: holder.target.clone(),
            host: holder.host.clone(),
            acquired_at: holder.acquired_at,
            expires_at: holder.expires_at,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct AuthorityOwner {
    host: String,
    boot_id: String,
    pid: u32,
    start_ticks: u64,
    liveness: AuthorityOwnerLiveness,
}

impl AuthorityOwner {
    fn from_process(owner: &ProcessOwner, liveness: AuthorityOwnerLiveness) -> Self {
        Self {
            host: owner.host.clone(),
            boot_id: owner.boot_id.clone(),
            pid: owner.pid,
            start_ticks: owner.start_ticks,
            liveness,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct AuthorityStatus {
    schema_version: u32,
    admissible: bool,
    state: AuthorityState,
    reason_code: Option<String>,
    holder: Option<AuthorityHolder>,
    owner: Option<AuthorityOwner>,
    canonical_anchor_held: bool,
    cleanup_state: AuthorityCleanupState,
}

impl AuthorityStatus {
    fn refused(
        state: AuthorityState,
        reason_code: &str,
        holder: Option<AuthorityHolder>,
        owner: Option<AuthorityOwner>,
        canonical_anchor_held: bool,
        cleanup_state: AuthorityCleanupState,
    ) -> Self {
        Self {
            schema_version: 1,
            admissible: false,
            state,
            reason_code: Some(reason_code.to_string()),
            holder,
            owner,
            canonical_anchor_held,
            cleanup_state,
        }
    }

    fn admitted(
        holder: AuthorityHolder,
        owner: AuthorityOwner,
        cleanup_state: AuthorityCleanupState,
    ) -> Self {
        Self {
            schema_version: 1,
            admissible: true,
            state: AuthorityState::Held,
            reason_code: None,
            holder: Some(holder),
            owner: Some(owner),
            canonical_anchor_held: true,
            cleanup_state,
        }
    }
}

enum AuthorityRecord<T> {
    Missing,
    Present(T),
    Malformed,
}

#[derive(Debug, Error)]
pub enum ValidateLockError {
    #[error("validate-lock: {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("validate-lock: invalid on-disk state: {0}")]
    InvalidState(String),
    #[error("validate-lock: timed out taking internal guard after {GUARD_WAIT_SECONDS}s")]
    GuardTimeout,
    #[error("validate-lock: renew: {agent} does not hold the lock")]
    RenewNotOwner { agent: String },
    #[error("validate-lock: release: lock is held by {holder}, not {agent}; refusing")]
    ReleaseNotOwner { agent: String, holder: String },
    #[error("validate-lock: child command is empty")]
    EmptyChild,
    #[error(
        "validate-lock: --child-deadline must be positive; unbounded lock holders are forbidden"
    )]
    UnboundedChildDeadline,
    #[error("validate-lock: --max must be 1; box-exclusive cap >1 is unproven (detcore_misc residual is monotonic in load, experiments/multisect_detcore_misc_20260803); raising N requires hermit-250 evidence")]
    BadMax,
    /// Admission refused a validate whose head predates an immutable producer
    /// or merge-gate floor. Mutable-tip currency is enforced at merge time.
    #[error("{0}")]
    StaleBase(String),
    #[error(
        "validate-lock: {operation}: process {pid} owns the supervised lease, not this process"
    )]
    ProcessNotOwner { operation: &'static str, pid: u32 },
    #[error("validate-lock: cannot reclaim lease: {0}")]
    ReclaimNotProven(String),
    #[error("validate-lock: cleanup quarantine: {0}")]
    CleanupQuarantined(String),
    /// A post-hoc census was requested but at least one MECHANICALLY checkable
    /// precondition failed. These are never attestable — an operator may only
    /// attest the one fact the kernel cannot answer once the subreaper anchor is
    /// gone (see `census_orphaned_domain`), never that a live process is dead.
    #[error("validate-lock: cannot census orphaned domain: {0}")]
    RecoveryNotProven(String),
}

impl ValidateLockError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::RenewNotOwner { .. }
            | Self::ReleaseNotOwner { .. }
            | Self::ProcessNotOwner { .. }
            | Self::ReclaimNotProven(_)
            | Self::RecoveryNotProven(_)
            | Self::CleanupQuarantined(_)
            | Self::GuardTimeout
            | Self::StaleBase(_)
            | Self::InvalidState(_) => 3,
            Self::Io { .. } | Self::EmptyChild | Self::UnboundedChildDeadline | Self::BadMax => 2,
        }
    }
}

#[derive(Clone, Debug)]
struct LockPaths {
    lock: PathBuf,
    guard: PathBuf,
    queue: PathBuf,
    owner: PathBuf,
    cleanup: PathBuf,
    /// The box-global exclusion anchor (see [`BoxExclusionAnchor`]). Carried here
    /// rather than read from a global so it is CONSTRUCTOR-SCOPED: production
    /// builds it from the uid, and each test builds its own beside its own temp
    /// lease. That is deliberate — the first version of this used a `cfg(test)`
    /// env override and two tests raced it through the process-global
    /// environment, one of them taking the REAL box anchor and blocking on it.
    /// A test binary must never contend with the live box.
    anchor: PathBuf,
}

impl LockPaths {
    fn for_workspace(root: &Path) -> Self {
        // The base is DISTINCT from `.landing-lock` so a validate never contends
        // with a lander. The path is NOT caller-selectable in a production build:
        // `test_only_override` returns None there (and says so), because an
        // ambient variable that redirects the box-exclusive lockfile escapes box
        // exclusivity outright (c6767e06).
        //
        // AND THE ROOT IS CANONICALIZED TO THE REPOSITORY'S MAIN WORKTREE, because
        // `root` alone is not one place. `workspace_root()` (ci-hub.rs) returns the
        // git toplevel of the running `ci-hub.rs`, so EVERY linked worktree used to
        // derive its own private `.validate-lock` and admit independently. Measured
        // 2026-08-08 on this box: 46 worktrees of ~/work/dev-hermit, all 46 carrying
        // a runnable `ci-hub/ci-hub`, i.e. 46 mutually blind box-exclusive locks —
        // and two runs from two of them were admitted 2/2 concurrently. Only the
        // main worktree held any `.validate-lock*` state (5 files; the other 45 held
        // none), so collapsing them onto it orphans nothing.
        let lock = test_only_override(VALIDATE_LOCK_PATH_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| repository_lock_root(root).join(".validate-lock"));
        Self::at(lock)
    }

    /// Resolve the authority query's production lock domain without consulting
    /// any ambient override, including the test-build-only override retained by
    /// mutating lock commands. Tests isolate snapshots by constructing
    /// `ValidateLock { paths: paths_at(...) }` directly; the command surface is
    /// deliberately incapable of selecting a different authority.
    fn for_authority_query(root: &Path) -> Self {
        Self::at(repository_lock_root(root).join(".validate-lock"))
    }

    fn at(lock: PathBuf) -> Self {
        Self {
            guard: suffix(&lock, ".guard"),
            queue: suffix(&lock, ".queue"),
            owner: suffix(&lock, ".owner"),
            cleanup: suffix(&lock, ".cleanup-required"),
            anchor: box_exclusion_anchor_at(VALIDATE_BOX_ANCHOR),
            lock,
        }
    }
}

struct ValidateLock {
    paths: LockPaths,
}

/// Result of one guarded acquisition attempt.
enum AcquireToken {
    Acquired,
    AcquiredReclaimed(String),
    /// The box is held by a live owner. `ahead` is how many FIFO waiters precede
    /// this agent (0-based) so callers can print a 1-based position.
    Held {
        agent: String,
        kind: String,
        target: String,
        seconds_left: i64,
        ahead: usize,
    },
    /// The box is free but this agent is not at the head of the FIFO queue.
    WaitTurn {
        head: String,
        ahead: usize,
    },
}

pub fn execute(root: &Path, args: ValidateLockArgs) -> Result<i32, ValidateLockError> {
    let lock = ValidateLock {
        paths: LockPaths::for_workspace(root),
    };
    match args.command {
        ValidateLockCommand::Acquire(args) => {
            reject_bad_max(args.max)?;
            base_admission_check(root, &args.target, args.kind, args.skip_base_check)?;
            // `acquire` cannot HOLD the box-global anchor (see `held_by_another`),
            // but it must not hand out a lease while another repository's payload
            // owns the box. `run` is the sanctioned production path
            // (ci-hub/validate/start_unit.py invokes `validate-lock run`); this
            // keeps the bare-`acquire` path from being the way around it.
            if BoxExclusionAnchor::held_by_another(&lock.paths.anchor) {
                eprintln!(
                    "REFUSED: box-exclusive anchor held by {}; this box admits one \
                     validate or benchmark at a time across ALL checkouts and clones",
                    BoxExclusionAnchor::describe_holder(&lock.paths.anchor)
                );
                return Ok(REFUSED_EXIT_CODE);
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
            lock.acquire(&args.agent, args.kind, &args.target, args.wait, args.hold)
        }
        ValidateLockCommand::Renew(args) => {
            lock.renew(&args.agent, args.hold, true)?;
            Ok(0)
        }
        ValidateLockCommand::Release(args) => {
            lock.release(&args.agent, true)?;
            Ok(0)
        }
        ValidateLockCommand::Status => {
            lock.status()?;
            Ok(0)
        }
        ValidateLockCommand::AuthorityStatus(args) => {
            debug_assert!(args.json, "clap requires --json");
            ValidateLock {
                paths: LockPaths::for_authority_query(root),
            }
            .print_authority_status()?;
            Ok(0)
        }
        ValidateLockCommand::ReclaimDead => lock.reclaim_dead(),
        ValidateLockCommand::CensusOrphanedDomain(args) => lock.census_orphaned_domain(args),
        ValidateLockCommand::Run(args) => lock.run(args, root),
    }
}

/// "Derive don't pick": any cap other than 1 is refused before touching the lock.
fn reject_bad_max(max: u64) -> Result<(), ValidateLockError> {
    if max != BOX_EXCLUSIVE_CAP {
        return Err(ValidateLockError::BadMax);
    }
    Ok(())
}

/// A 40-char lowercase-hex commit SHA — the only target shape that can bind the
/// admission verdict to the receipt-producing child.
fn is_full_sha(target: &str) -> bool {
    target.len() == 40
        && target
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Refuse to admit a `Validate` unless its exact head contains every immutable
/// producer and merge-gate floor. Mutable origin/main currency is intentionally
/// absent here: the receipt records its exact base and the canonical verifier
/// asserts currency once at the final merge boundary.
///
/// Non-Validate kinds pass through. Validate targets that are not exact SHAs and
/// the legacy `--skip-base-check` flag are refused: either would sever the
/// observable binding between the admission verdict and the validated commit.
/// The predicate is shelled out to `preflight_validate.py`, the same composite
/// authority used by all other ci-hub validation producers. THAT IS THE ONLY
/// PREDICATE A PRODUCTION BUILD WILL RUN: `CI_HUB_ADMIT_PREFLIGHT_CMD` is
/// compiled out of it (see [`test_only_override`]), so the verdict is not
/// caller-settable. This function adds no second implementation of the
/// admission predicate in any language; it dereferences the one Python
/// authority and maps its exit code.
///
/// Fail-CLOSED: preflight exit 2 (REFUSED) and any other non-zero (ERROR: head
/// unresolved) both become `StaleBase`. An admission gate that admitted on "I
/// couldn't check" would be advisory, not mechanical.
fn base_admission_check(
    root: &Path,
    target: &str,
    kind: Kind,
    skip: bool,
) -> Result<(), ValidateLockError> {
    if kind != Kind::Validate {
        return Ok(()); // bench/diagnostic work is not landing evidence
    }
    if skip {
        return Err(ValidateLockError::StaleBase(format!(
            "REFUSE: --skip-base-check cannot authorize qualifying validation of \
             {target}. The immutable validation floors must be checked. \
             Historical differential debugging is non-qualifying and must run \
             outside validate-run so it cannot mint landing evidence."
        )));
    }
    if !is_full_sha(target) {
        return Err(ValidateLockError::StaleBase(format!(
            "REFUSE: validation target {target:?} is not an exact lowercase \
             40-hex commit SHA, so base ancestry cannot be bound to the child. \
             Resolve the rebased PR head exactly before validating."
        )));
    }

    let override_argv = test_only_override(ADMIT_PREFLIGHT_CMD_ENV)
        .map(|cmd| cmd.to_string_lossy().into_owned())
        .filter(|cmd| !cmd.trim().is_empty())
        .map(|cmd| {
            cmd.split_whitespace()
                .map(OsString::from)
                .collect::<Vec<OsString>>()
        });
    let mut argv: Vec<OsString> = override_argv.unwrap_or_else(|| {
        vec![
            OsString::from("python3"),
            root.join("ci-hub/validate/preflight_validate.py")
                .into_os_string(),
        ]
    });
    argv.push(OsString::from("--head"));
    argv.push(OsString::from(target));

    let (program, rest) = argv.split_first().expect("argv is never empty");
    let output = match Command::new(program).args(rest).output() {
        Ok(o) => o,
        Err(err) => {
            return Err(ValidateLockError::StaleBase(format!(
                "validate-lock: validation admission check could not run \
                 ({err}); fixed-floor ancestry for {target} is UNRESOLVED."
            )));
        }
    };
    let code = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match code {
        Some(0) => Ok(()),
        Some(2) => {
            // preflight prints its NAMED refuse line (with the rebase remedy) on
            // stdout; carry it verbatim so the operator sees the exact floor.
            let msg = if stdout.is_empty() {
                format!(
                    "REFUSE: base for {target} predates an immutable \
                     validation floor. Rebase onto the named floor before validating."
                )
            } else {
                stdout
            };
            Err(ValidateLockError::StaleBase(msg))
        }
        other => {
            // ERROR (3) or an unexpected code: fail closed — an unresolved base
            // is treated as stale, not waved through.
            let detail = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                format!("exit {other:?}")
            };
            Err(ValidateLockError::StaleBase(format!(
                "validate-lock: validation admission check for {target} did not \
                 resolve ({detail}); treating fixed-floor ancestry as STALE."
            )))
        }
    }
}

impl ValidateLock {
    fn cleanup_verification(&self, holder: Option<&ValidateLockState>) -> CleanupVerification {
        let operation = holder.map(|holder| format!("{}:{}", holder.kind, holder.target));
        let expected = holder
            .zip(operation.as_deref())
            .map(|(holder, operation)| (holder.agent.as_str(), operation));
        verify_cleanup_record(&self.paths.cleanup, expected)
    }

    fn require_no_cleanup(
        &self,
        holder: Option<&ValidateLockState>,
    ) -> Result<(), ValidateLockError> {
        match self.cleanup_verification(holder) {
            CleanupVerification::None => Ok(()),
            CleanupVerification::Armed { reason, .. } => Err(
                ValidateLockError::CleanupQuarantined(format!("ARMED: {reason}")),
            ),
            CleanupVerification::Active { reason, .. } => Err(
                ValidateLockError::CleanupQuarantined(format!("ACTIVE: {reason}")),
            ),
            CleanupVerification::Uncensused { reason, .. } => Err(
                ValidateLockError::CleanupQuarantined(format!("UNCENSUSED: {reason}")),
            ),
            CleanupVerification::Recoverable { reason, .. } => Err(
                ValidateLockError::CleanupQuarantined(format!(
                    "payload absence is proven but explicit reclaim-dead recovery is required: {reason}"
                )),
            ),
            CleanupVerification::Unknown { reason, .. } => Err(
                ValidateLockError::CleanupQuarantined(format!("UNVERIFIABLE: {reason}")),
            ),
        }
    }

    fn arm_run(&self, agent: &str, kind: Kind, target: &str) -> Result<(), ValidateLockError> {
        let record = CleanupRecord::new(
            agent,
            format!("{}:{target}", kind.as_str()),
            CleanupPhase::Armed,
        )
        .map_err(|source| io_error("construct", &self.paths.cleanup, source))?;
        self.with_guard(|| {
            let holder = self.read_holder()?.ok_or_else(|| {
                ValidateLockError::InvalidState("cannot arm cleanup without a lock holder".into())
            })?;
            if holder.agent != agent || holder.kind != kind.as_str() || holder.target != target {
                return Err(ValidateLockError::InvalidState(
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
                &format!("{}:{target}", kind.as_str()),
            );
            Ok(())
        })
    }

    fn transition_run_cleanup(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        expected: fn(&CleanupPhase) -> bool,
        next: CleanupPhase,
        // `Some` only at publish, the one transition that learns the payload's
        // cgroup. Every other transition preserves whatever is already there.
        cgroup: Option<String>,
    ) -> Result<(), ValidateLockError> {
        self.with_guard(|| {
            let holder = self.read_holder()?.ok_or_else(|| {
                ValidateLockError::InvalidState("cleanup transition has no lock holder".into())
            })?;
            if holder.agent != agent || holder.kind != kind.as_str() || holder.target != target {
                return Err(ValidateLockError::InvalidState(
                    "cleanup transition does not match the current operation".into(),
                ));
            }
            let mut record = match self.cleanup_verification(Some(&holder)) {
                CleanupVerification::Armed { record, .. }
                | CleanupVerification::Active { record, .. }
                | CleanupVerification::Uncensused { record, .. }
                | CleanupVerification::Recoverable { record, .. } => record,
                CleanupVerification::None => {
                    return Err(ValidateLockError::InvalidState(
                        "cleanup transition has no durable authority".into(),
                    ))
                }
                CleanupVerification::Unknown { reason, .. } => {
                    return Err(ValidateLockError::CleanupQuarantined(format!(
                        "cannot transition unverifiable authority: {reason}"
                    )))
                }
            };
            if !expected(&record.phase) {
                return Err(ValidateLockError::InvalidState(format!(
                    "cleanup transition rejected phase {:?}",
                    record.phase
                )));
            }
            record.phase = next;
            if cgroup.is_some() {
                record.cgroup = cgroup.clone();
            }
            write_cleanup_record(&self.paths.cleanup, &record)
                .map_err(|source| io_error("write", &self.paths.cleanup, source))
        })
    }

    fn publish_run(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        gated: &GatedChild,
    ) -> Result<(), ValidateLockError> {
        self.transition_run_cleanup(
            agent,
            kind,
            target,
            |phase| matches!(phase, CleanupPhase::Armed),
            CleanupPhase::Published {
                leader: gated.leader.clone(),
                pgid: gated.pgid,
            },
            // Read from the payload itself while it is demonstrably alive, and
            // ONLY when that cgroup is exclusive to this run. Best effort by
            // design: if it cannot be read, or is shared with unrelated work,
            // the record simply carries no anchor and the census keeps its
            // attestation path -- the status quo, never a new failure. Anchoring
            // to a shared cgroup would be worse than no anchor at all, since a
            // permanently populated domain can never be censused.
            match payload_cgroup_anchor(gated.leader.pid) {
                Ok(cgroup) => Some(cgroup),
                Err(why) => {
                    // Say so rather than silently omitting it: without an anchor
                    // an orphan census of THIS run will need a human attestation,
                    // and that is worth knowing at publish time, not at 3am.
                    eprintln!(
                        "validate-lock: no cgroup anchor recorded for this run ({why}); if the \
                         supervisor dies, censusing the orphaned domain will require \
                         --attest-domain-empty"
                    );
                    None
                }
            },
        )
    }

    fn clear_unstarted_run(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
    ) -> Result<(), ValidateLockError> {
        self.with_guard(|| {
            let holder = self.read_holder()?.ok_or_else(|| {
                ValidateLockError::InvalidState("unstarted cleanup has no lock holder".into())
            })?;
            if holder.agent != agent || holder.kind != kind.as_str() || holder.target != target {
                return Err(ValidateLockError::InvalidState(
                    "unstarted cleanup does not match current operation".into(),
                ));
            }
            match self.cleanup_verification(Some(&holder)) {
                CleanupVerification::Armed { .. } => {
                    self.assert_current_process_owner("clear unstarted run")?;
                    remove_cleanup_record_and_claim(&self.paths.cleanup, &self.paths.anchor)
                        .map_err(|source| io_error("remove", &self.paths.cleanup, source))
                }
                other => Err(ValidateLockError::InvalidState(format!(
                    "unstarted cleanup requires armed authority, got {other:?}"
                ))),
            }
        })
    }

    fn record_residuals(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        pgid: u32,
        capture: ResidualCapture,
    ) -> Result<(), ValidateLockError> {
        self.transition_run_cleanup(
            agent,
            kind,
            target,
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
        kind: Kind,
        target: &str,
        gated: &GatedChild,
    ) -> Result<(), ValidateLockError> {
        self.transition_run_cleanup(
            agent,
            kind,
            target,
            |phase| matches!(phase, CleanupPhase::Published { .. }),
            CleanupPhase::CensusPending {
                leader: gated.leader.clone(),
                pgid: gated.pgid,
            },
            None,
        )
    }

    fn clear_proven_run(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        pgid: u32,
    ) -> Result<(), ValidateLockError> {
        self.record_residuals(
            agent,
            kind,
            target,
            pgid,
            ResidualCapture {
                complete: true,
                identities: Vec::new(),
            },
        )?;
        self.with_guard(|| {
            let holder = self.read_holder()?.ok_or_else(|| {
                ValidateLockError::InvalidState("cleanup clear has no lock holder".into())
            })?;
            match self.cleanup_verification(Some(&holder)) {
                CleanupVerification::Recoverable { .. } => {
                    remove_cleanup_record_and_claim(&self.paths.cleanup, &self.paths.anchor)
                        .map_err(|source| io_error("remove", &self.paths.cleanup, source))
                }
                other => Err(ValidateLockError::InvalidState(format!(
                    "cleanup clear requires proven absence, got {other:?}"
                ))),
            }
        })
    }

    fn renew_run_lease(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        hold: u64,
    ) -> Result<(), ValidateLockError> {
        self.with_guard(|| {
            let holder = self
                .read_holder()?
                .ok_or_else(|| ValidateLockError::RenewNotOwner {
                    agent: agent.to_string(),
                })?;
            if holder.agent != agent || holder.kind != kind.as_str() || holder.target != target {
                return Err(ValidateLockError::RenewNotOwner {
                    agent: agent.to_string(),
                });
            }
            match self.cleanup_verification(Some(&holder)) {
                CleanupVerification::Active { record, .. }
                    if matches!(record.phase, CleanupPhase::Published { .. }) => {}
                other => {
                    return Err(ValidateLockError::CleanupQuarantined(format!(
                        "run heartbeat requires an active published domain, got {other:?}"
                    )))
                }
            }
            self.assert_current_process_owner("run heartbeat")?;
            heartbeat_test_helper_delay();
            self.write_holder(&new_holder(agent, kind, target, hold, None)?)
        })
    }

    fn acquire(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        wait: u64,
        hold: u64,
    ) -> Result<i32, ValidateLockError> {
        let started = Instant::now();
        let mut last = String::new();
        loop {
            let token = self.with_guard(|| self.try_acquire(agent, kind, target, hold))?;
            match token {
                AcquireToken::Acquired => {
                    eprintln!(
                        "validate-lock: ACQUIRED by {agent} running {} {target} (lease {hold}s)",
                        kind.as_str()
                    );
                    return Ok(0);
                }
                AcquireToken::AcquiredReclaimed(previous) => {
                    eprintln!(
                        "validate-lock: ACQUIRED by {agent} running {} {target}; evidence-reclaimed lease from {previous}",
                        kind.as_str()
                    );
                    return Ok(0);
                }
                AcquireToken::Held {
                    agent: holder,
                    kind: holder_kind,
                    ahead,
                    ..
                } => {
                    let position = ahead + 1;
                    let token = format!("HELD:{holder}:{holder_kind}:{position}");
                    if token != last {
                        eprintln!(
                            "validate-lock: waiting: held by {holder} running {holder_kind}; queued position {position}"
                        );
                        last = token;
                    }
                }
                AcquireToken::WaitTurn { head, ahead } => {
                    let position = ahead + 1;
                    let token = format!("WAIT:{head}:{position}");
                    if token != last {
                        eprintln!(
                            "validate-lock: waiting: box free; {head} is ahead; queued position {position}"
                        );
                        last = token;
                    }
                }
            }
            if started.elapsed() >= Duration::from_secs(wait) {
                self.with_guard(|| {
                    self.remove_from_queue(agent)?;
                    Ok(())
                })?;
                eprintln!("validate-lock: TIMEOUT after {wait}s");
                return Ok(1);
            }
            thread::sleep(Duration::from_secs(POLL_SECONDS));
        }
    }

    fn try_acquire(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        hold: u64,
    ) -> Result<AcquireToken, ValidateLockError> {
        let now = epoch_seconds()?;
        let holder = self.read_holder()?;
        self.require_no_cleanup(holder.as_ref())?;
        let mut queue = self.read_queue()?;
        if !queue.iter().any(|entry| entry.agent == agent) {
            queue.push(QueueEntry {
                enqueued_at: now,
                agent: agent.to_string(),
                target: target.to_string(),
            });
        }
        let cutoff = now - (2 * DEFAULT_WAIT_SECONDS) as i64;
        queue.retain(|entry| entry.enqueued_at >= cutoff);
        let my_index = queue
            .iter()
            .position(|entry| entry.agent == agent)
            .unwrap_or(0);

        let dead_owner = if holder.as_ref().is_some_and(|holder| holder.live_at(now)) {
            match self.owner_liveness()? {
                OwnerLiveness::Dead(reason) => Some(reason),
                OwnerLiveness::Alive | OwnerLiveness::Unknown(_) => None,
            }
        } else {
            None
        };
        if let Some(holder) = holder
            .as_ref()
            .filter(|holder| holder.live_at(now) && dead_owner.is_none())
        {
            self.write_queue(&queue)?;
            return Ok(AcquireToken::Held {
                agent: holder.agent.clone(),
                kind: holder.kind.clone(),
                target: holder.target.clone(),
                seconds_left: holder.expires_at - now,
                ahead: my_index,
            });
        }

        if let Some(head) = queue.first() {
            if head.agent != agent {
                let head_agent = head.agent.clone();
                self.write_queue(&queue)?;
                return Ok(AcquireToken::WaitTurn {
                    head: head_agent,
                    ahead: my_index,
                });
            }
        }

        let reclaimed = holder.map(|holder| {
            if let Some(reason) = &dead_owner {
                format!("{} (dead owner: {reason})", holder.agent)
            } else {
                holder.agent
            }
        });
        self.write_holder(&new_holder(agent, kind, target, hold, reclaimed.clone())?)?;
        remove_if_exists(&self.paths.owner)?;
        queue.retain(|entry| entry.agent != agent);
        self.write_queue(&queue)?;
        Ok(match reclaimed {
            Some(previous) if !previous.is_empty() => AcquireToken::AcquiredReclaimed(previous),
            _ => AcquireToken::Acquired,
        })
    }

    fn renew(&self, agent: &str, hold: u64, announce: bool) -> Result<(), ValidateLockError> {
        self.with_guard(|| {
            let holder = self
                .read_holder()?
                .ok_or_else(|| ValidateLockError::RenewNotOwner {
                    agent: agent.to_string(),
                })?;
            self.require_no_cleanup(Some(&holder))?;
            if holder.agent != agent {
                return Err(ValidateLockError::RenewNotOwner {
                    agent: agent.to_string(),
                });
            }
            self.assert_current_process_owner("renew")?;
            let kind = Kind::parse(&holder.kind)?;
            self.write_holder(&new_holder(agent, kind, &holder.target, hold, None)?)?;
            Ok(())
        })?;
        if announce {
            eprintln!("validate-lock: renewed {agent} lease {hold}s");
        }
        Ok(())
    }

    fn release(&self, agent: &str, announce: bool) -> Result<(), ValidateLockError> {
        let (released, next) = self.with_guard(|| {
            let holder = self.read_holder()?;
            self.require_no_cleanup(holder.as_ref())?;
            let Some(holder) = holder else {
                remove_if_exists(&self.paths.owner)?;
                return Ok((false, None));
            };
            if holder.agent != agent {
                return Err(ValidateLockError::ReleaseNotOwner {
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
                (false, _) => eprintln!("validate-lock: release: no lock held"),
                (true, None) => {
                    eprintln!("validate-lock: RELEASED by {agent}; box FREE (queue empty)")
                }
                (true, Some(next)) => {
                    eprintln!("validate-lock: RELEASED by {agent}; box FREE -> next: {next}")
                }
            }
        }
        Ok(())
    }

    fn print_authority_status(&self) -> Result<(), ValidateLockError> {
        let status = self.with_guard(|| self.authority_status_guarded())?;
        let json = serde_json::to_string(&status).map_err(|error| {
            ValidateLockError::InvalidState(format!(
                "cannot serialize canonical authority status: {error}"
            ))
        })?;
        println!("{json}");
        Ok(())
    }

    /// Return one coherent canonical authority snapshot while the lock-state
    /// files are protected by `.validate-lock.guard`.
    ///
    /// Caller-provided owner PID/file variables are deliberately absent. The
    /// holder and process identity are read exactly once from the canonical
    /// main-worktree lock domain, and the uid-derived kernel anchor is sampled
    /// on both sides so a changing anchor fails closed as indeterminate.
    fn authority_status_guarded(&self) -> Result<AuthorityStatus, ValidateLockError> {
        let anchor_before = canonical_anchor_held(&self.paths.anchor)?;
        let now = epoch_seconds()?;
        let current_host_name = current_host();
        let holder_record = self.read_authority_holder()?;
        let holder_ref = match &holder_record {
            AuthorityRecord::Present(holder) => Some(holder),
            AuthorityRecord::Missing | AuthorityRecord::Malformed => None,
        };
        let cleanup = self.cleanup_verification(holder_ref);
        let owner_record = self.read_authority_owner()?;

        let (cleanup_state, cleanup_refusal) = match cleanup {
            CleanupVerification::None => (AuthorityCleanupState::None, None),
            CleanupVerification::Active { record, .. } => match record.phase {
                // Only the published, still-live payload is the normal run
                // state. CensusPending means the receipt-producing child has
                // already exited, while a live Residual is an escaped payload;
                // neither may be laundered as current admission merely because
                // both classify as `Active` for recovery purposes.
                CleanupPhase::Published { .. } => (AuthorityCleanupState::ActiveBound, None),
                CleanupPhase::CensusPending { .. } => (
                    AuthorityCleanupState::ActiveBound,
                    Some("canonical-cleanup-census-pending"),
                ),
                CleanupPhase::Residual { .. } => (
                    AuthorityCleanupState::ActiveBound,
                    Some("canonical-cleanup-residual-active"),
                ),
                CleanupPhase::Armed => (
                    AuthorityCleanupState::Unknown,
                    Some("canonical-cleanup-phase-inconsistent"),
                ),
            },
            CleanupVerification::Armed { .. } => (
                AuthorityCleanupState::Armed,
                Some("canonical-cleanup-armed"),
            ),
            CleanupVerification::Recoverable { .. } => (
                AuthorityCleanupState::Recoverable,
                Some("canonical-cleanup-recoverable"),
            ),
            CleanupVerification::Uncensused { .. } => (
                AuthorityCleanupState::Uncensused,
                Some("canonical-cleanup-uncensused"),
            ),
            CleanupVerification::Unknown { .. } => (
                AuthorityCleanupState::Unknown,
                Some("canonical-cleanup-indeterminate"),
            ),
        };

        let (owner, owner_refusal) = match &owner_record {
            AuthorityRecord::Missing => (None, None),
            AuthorityRecord::Malformed => (
                None,
                Some((AuthorityState::Indeterminate, "canonical-owner-malformed")),
            ),
            AuthorityRecord::Present(process_owner) => {
                let (liveness, refusal) = if current_host_name == "unknown" {
                    (
                        AuthorityOwnerLiveness::Unknown,
                        Some((
                            AuthorityState::Indeterminate,
                            "canonical-current-host-unavailable",
                        )),
                    )
                } else if process_owner.host != current_host_name {
                    (
                        AuthorityOwnerLiveness::Unknown,
                        Some((
                            AuthorityState::Indeterminate,
                            "canonical-owner-host-mismatch",
                        )),
                    )
                } else {
                    let boot_id = current_boot_id()?;
                    if process_owner.boot_id != boot_id {
                        (
                            AuthorityOwnerLiveness::Dead,
                            Some((AuthorityState::Orphaned, "canonical-owner-boot-id-mismatch")),
                        )
                    } else {
                        match process_start_ticks(process_owner.pid) {
                            Ok(observed) if observed == process_owner.start_ticks => {
                                (AuthorityOwnerLiveness::Alive, None)
                            }
                            Ok(_) => (
                                AuthorityOwnerLiveness::Dead,
                                Some((
                                    AuthorityState::Orphaned,
                                    "canonical-owner-start-ticks-mismatch",
                                )),
                            ),
                            Err(error) if error.kind() == io::ErrorKind::NotFound => (
                                AuthorityOwnerLiveness::Dead,
                                Some((AuthorityState::Orphaned, "canonical-owner-pid-absent")),
                            ),
                            Err(_) => (
                                AuthorityOwnerLiveness::Unknown,
                                Some((
                                    AuthorityState::Indeterminate,
                                    "canonical-owner-liveness-indeterminate",
                                )),
                            ),
                        }
                    }
                };
                (
                    Some(AuthorityOwner::from_process(process_owner, liveness)),
                    refusal,
                )
            }
        };

        let anchor_after = canonical_anchor_held(&self.paths.anchor)?;
        let canonical_anchor_held = anchor_before && anchor_after;
        let holder = holder_ref.map(AuthorityHolder::from);

        if anchor_before != anchor_after {
            return Ok(AuthorityStatus::refused(
                AuthorityState::Indeterminate,
                "canonical-anchor-raced",
                holder,
                owner,
                false,
                cleanup_state,
            ));
        }
        if matches!(holder_record, AuthorityRecord::Malformed) {
            return Ok(AuthorityStatus::refused(
                AuthorityState::Indeterminate,
                "canonical-holder-malformed",
                None,
                owner,
                canonical_anchor_held,
                cleanup_state,
            ));
        }
        if let Some(reason) = cleanup_refusal {
            return Ok(AuthorityStatus::refused(
                AuthorityState::Quarantined,
                reason,
                holder,
                owner,
                canonical_anchor_held,
                cleanup_state,
            ));
        }

        let AuthorityRecord::Present(holder_record) = holder_record else {
            if !matches!(owner_record, AuthorityRecord::Missing) {
                return Ok(AuthorityStatus::refused(
                    AuthorityState::Indeterminate,
                    "canonical-owner-without-holder",
                    None,
                    owner,
                    canonical_anchor_held,
                    cleanup_state,
                ));
            }
            let (state, reason) = if canonical_anchor_held {
                (AuthorityState::Indeterminate, "canonical-holder-missing")
            } else {
                (AuthorityState::Free, "canonical-lock-not-held")
            };
            return Ok(AuthorityStatus::refused(
                state,
                reason,
                None,
                None,
                canonical_anchor_held,
                cleanup_state,
            ));
        };
        let holder = AuthorityHolder::from(&holder_record);

        if !holder_record.live_at(now) {
            return Ok(AuthorityStatus::refused(
                AuthorityState::Lapsed,
                "canonical-holder-lapsed",
                Some(holder),
                owner,
                canonical_anchor_held,
                cleanup_state,
            ));
        }
        if !canonical_anchor_held {
            return Ok(AuthorityStatus::refused(
                AuthorityState::Orphaned,
                "canonical-anchor-not-held",
                Some(holder),
                owner,
                false,
                cleanup_state,
            ));
        }
        if holder_record.kind != Kind::Validate.as_str() {
            return Ok(AuthorityStatus::refused(
                AuthorityState::Held,
                "canonical-holder-kind-not-validate",
                Some(holder),
                owner,
                true,
                cleanup_state,
            ));
        }
        if current_host_name == "unknown" {
            return Ok(AuthorityStatus::refused(
                AuthorityState::Indeterminate,
                "canonical-current-host-unavailable",
                Some(holder),
                owner,
                true,
                cleanup_state,
            ));
        }
        if holder_record.host != current_host_name {
            return Ok(AuthorityStatus::refused(
                AuthorityState::Indeterminate,
                "canonical-holder-host-mismatch",
                Some(holder),
                owner,
                true,
                cleanup_state,
            ));
        }
        if matches!(owner_record, AuthorityRecord::Missing) {
            return Ok(AuthorityStatus::refused(
                AuthorityState::Indeterminate,
                "canonical-owner-missing",
                Some(holder),
                None,
                true,
                cleanup_state,
            ));
        }
        if let Some((state, reason)) = owner_refusal {
            return Ok(AuthorityStatus::refused(
                state,
                reason,
                Some(holder),
                owner,
                true,
                cleanup_state,
            ));
        }
        let owner = owner.ok_or_else(|| {
            ValidateLockError::InvalidState(
                "authority owner disappeared after canonical classification".into(),
            )
        })?;
        Ok(AuthorityStatus::admitted(holder, owner, cleanup_state))
    }

    fn status(&self) -> Result<(), ValidateLockError> {
        let now = epoch_seconds()?;
        let holder = self.read_holder()?;
        let liveness = self.owner_liveness()?;
        // The authoritative owner PROCESS IDENTITY, read from the flock-guarded
        // owner sidecar (the ONLY writer is an actual `validate-lock run`/lease
        // acquisition). Consumers that must prove a run descends from the live
        // lease holder -- e.g. Hermit's Rust driver's admission guard -- MUST read
        // owner_pid/owner_boot_id/owner_start_ticks from HERE, never from an env
        // var or an agent-writable file, because only a real acquisition writes
        // this record. boot_id defeats reboots; start_ticks defeats PID reuse.
        let process_owner = self.read_process_owner()?;
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
                println!("QUARANTINED (published domain lacks final census):");
                print_cleanup_record(&record, &reason);
                // Uncensused used to name NO action while `reclaim-dead` refused
                // it, which read as "wait" when nothing was ever coming. Name the
                // exit, with the identity already restated for the operator.
                if let CleanupPhase::Published { leader, pgid }
                | CleanupPhase::CensusPending { leader, pgid } = &record.phase
                {
                    // Say up front whether the kernel can settle this, so the
                    // operator is not told to attest something already provable
                    // -- or told it is mechanical when the domain is still live.
                    let domain = census_recorded_domain(record.cgroup.as_deref());
                    let attestation = match &domain {
                        DomainEvidence::ProvenEmpty(_) => "",
                        _ => " --attest-domain-empty --evidence '<observations>'",
                    };
                    println!(
                        "  recovery=ci-hub validate-lock census-orphaned-domain \
                         --agent {} --operation {} --leader {}:{} --pgid {pgid}{attestation}",
                        record.agent, record.operation, leader.pid, leader.start_ticks
                    );
                    match domain {
                        DomainEvidence::ProvenEmpty(detail) => println!(
                            "  recovery_precondition=none; the domain is ALREADY proven empty \
                             from the recorded cgroup anchor ({detail}), so no attestation is \
                             required. reclaim-dead alone still CANNOT clear this state."
                        ),
                        DomainEvidence::Populated(detail) => println!(
                            "  recovery_precondition=BLOCKED: {detail}. No census can run and no \
                             attestation can override a live domain."
                        ),
                        DomainEvidence::Unproven(why) => println!(
                            "  recovery_precondition={why}, so confirm by hand that the payload's \
                             unit cgroup is absent/empty AND every cgroup it migrates into is \
                             empty; reclaim-dead alone CANNOT clear this state"
                        ),
                    }
                }
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
            print_owner_identity(process_owner.as_ref());
            println!("  owner_process={}", render_liveness(&liveness));
        } else {
            match holder {
                Some(holder)
                    if holder.live_at(now) && matches!(liveness, OwnerLiveness::Dead(_)) =>
                {
                    println!("ORPHANED (reclaimable):");
                    for line in holder.render().lines() {
                        println!("  {line}");
                    }
                    print_owner_identity(process_owner.as_ref());
                    println!("  owner_process={}", render_liveness(&liveness));
                    println!("  secs_left={}", holder.expires_at - now);
                }
                Some(holder) if holder.live_at(now) => {
                    println!("HELD:");
                    for line in holder.render().lines() {
                        println!("  {line}");
                    }
                    print_owner_identity(process_owner.as_ref());
                    println!("  owner_process={}", render_liveness(&liveness));
                    println!("  secs_left={}", holder.expires_at - now);
                }
                Some(holder) => {
                    println!("LAPSED (reclaimable):");
                    for line in holder.render().lines() {
                        println!("  {line}");
                    }
                    print_owner_identity(process_owner.as_ref());
                    println!("  owner_process={}", render_liveness(&liveness));
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

    fn reclaim_dead(&self) -> Result<i32, ValidateLockError> {
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
                        Err(ValidateLockError::ReclaimNotProven(reason))
                    }
                    CleanupVerification::Recoverable { .. } => {
                        Err(ValidateLockError::ReclaimNotProven(
                            "cleanup authority is not bound to a lock holder".into(),
                        ))
                    }
                };
            };
            match self.cleanup_verification(Some(&holder)) {
                CleanupVerification::Armed { reason, .. }
                | CleanupVerification::Active { reason, .. }
                | CleanupVerification::Uncensused { reason, .. }
                | CleanupVerification::Unknown { reason, .. } => {
                    return Err(ValidateLockError::ReclaimNotProven(reason));
                }
                CleanupVerification::Recoverable { .. } | CleanupVerification::None => {}
            }
            match self.owner_liveness()? {
                OwnerLiveness::Dead(reason) => {
                    remove_if_exists(&self.paths.lock)?;
                    remove_if_exists(&self.paths.owner)?;
                    remove_cleanup_record_and_claim(&self.paths.cleanup, &self.paths.anchor)
                        .map_err(|source| io_error("remove", &self.paths.cleanup, source))?;
                    Ok(Some((holder.agent, holder.kind, holder.target, reason)))
                }
                OwnerLiveness::Alive => Err(ValidateLockError::ReclaimNotProven(
                    "recorded owner process is alive".into(),
                )),
                OwnerLiveness::Unknown(reason) => Err(ValidateLockError::ReclaimNotProven(reason)),
            }
        })?;
        match reclaimed {
            Some((agent, kind, target, reason)) => eprintln!(
                "validate-lock: evidence-reclaimed dead owner agent={agent} kind={kind} target={target}: {reason}"
            ),
            None => eprintln!("validate-lock: reclaim-dead: no lock held"),
        }
        Ok(0)
    }

    /// Take the residual census that a dead supervisor never got to take, so the
    /// existing `reclaim-dead` path can finish.
    ///
    /// WHY THIS EXISTS. `capture_and_freeze_residuals` enumerates the payload
    /// domain with `scan_descendants(supervisor_pid)` — a PPID-tree walk that is
    /// only exhaustive because `run` made the supervisor a
    /// `PR_SET_CHILD_SUBREAPER`, so even a `setsid`'d double-fork reparents back
    /// to it. When the supervisor itself dies (e.g. systemd TERMs the whole unit
    /// cgroup) that anchor is destroyed: survivors reparent to `systemd --user`
    /// or pid 1 and NO ppid walk can reconstruct the domain. So `Published` ->
    /// `Uncensused` became a one-way door — `reclaim_dead` refuses `Uncensused`
    /// before it ever checks owner liveness, and the only writer of a complete
    /// census lives inside `run`. The box stayed refused for every agent.
    ///
    /// WHAT IS AND IS NOT ATTESTABLE. Everything the kernel can still answer is
    /// checked mechanically here and REFUSES on failure — the restated identity,
    /// a proven-dead supervisor, the leader's exact pid+start_ticks absence, and
    /// an absent process group. None of those may be attested away. The single
    /// residual unknown is whether a descendant ESCAPED the recorded process
    /// group before dying out of view: `pgid` membership does not survive
    /// `setsid()`, so an absent pgid is necessary but not sufficient. Cgroup
    /// membership DOES survive `setsid()` and reparenting, so a
    /// supervisor-independent census is possible — but the version-2 record
    /// carries no cgroup anchor to check it against. Until it does (see the
    /// follow-up that records the payload cgroup at publish time), that one fact
    /// is supplied by `--attest-domain-empty` with the observations in
    /// `--evidence`, which are echoed into the transcript.
    fn census_orphaned_domain(
        &self,
        args: CensusOrphanedDomainArgs,
    ) -> Result<i32, ValidateLockError> {
        let restated = parse_leader_identity(&args.leader)?;
        let (leader, pgid, disposition) = self.with_guard(|| {
            let holder = self.read_holder()?.ok_or_else(|| {
                ValidateLockError::RecoveryNotProven(
                    "no lock holder; a census only discharges a quarantine bound to one".into(),
                )
            })?;

            // (1) The quarantine must be exactly the one this path addresses.
            let record = match self.cleanup_verification(Some(&holder)) {
                CleanupVerification::Uncensused { record, .. } => record,
                CleanupVerification::None => {
                    return Err(ValidateLockError::RecoveryNotProven(
                        "no cleanup authority is quarantined; nothing to census".into(),
                    ))
                }
                CleanupVerification::Recoverable { reason, .. } => {
                    return Err(ValidateLockError::RecoveryNotProven(format!(
                        "payload absence is ALREADY proven; run reclaim-dead instead: {reason}"
                    )))
                }
                CleanupVerification::Active { reason, .. } => {
                    return Err(ValidateLockError::RecoveryNotProven(format!(
                        "payload identities are STILL ALIVE; a census cannot bury a live \
                         domain: {reason}"
                    )))
                }
                CleanupVerification::Armed { reason, .. } => {
                    return Err(ValidateLockError::RecoveryNotProven(format!(
                        "cleanup is only ARMED, so no payload was ever published and there is \
                         no domain to census: {reason}"
                    )))
                }
                CleanupVerification::Unknown { reason, .. } => {
                    return Err(ValidateLockError::RecoveryNotProven(format!(
                        "cleanup authority is UNVERIFIABLE; refusing to census it: {reason}"
                    )))
                }
            };

            let (leader, pgid) = match &record.phase {
                CleanupPhase::Published { leader, pgid }
                | CleanupPhase::CensusPending { leader, pgid } => (leader.clone(), *pgid),
                other => {
                    return Err(ValidateLockError::RecoveryNotProven(format!(
                        "uncensused authority has unexpected phase {other:?}"
                    )))
                }
            };

            // (2) The caller must restate the recorded identity exactly, so this
            // cannot be aimed blind at whatever is quarantined right now.
            if record.agent != args.agent {
                return Err(ValidateLockError::RecoveryNotProven(format!(
                    "restated agent {:?} does not match recorded agent {:?}",
                    args.agent, record.agent
                )));
            }
            if record.operation != args.operation {
                return Err(ValidateLockError::RecoveryNotProven(format!(
                    "restated operation {:?} does not match recorded operation {:?}",
                    args.operation, record.operation
                )));
            }
            if leader != restated {
                return Err(ValidateLockError::RecoveryNotProven(format!(
                    "restated leader {}:{} does not match recorded leader {}:{}",
                    restated.pid, restated.start_ticks, leader.pid, leader.start_ticks
                )));
            }
            if pgid != args.pgid {
                return Err(ValidateLockError::RecoveryNotProven(format!(
                    "restated pgid {} does not match recorded pgid {pgid}",
                    args.pgid
                )));
            }

            // (3) A LIVE supervisor owns its own census; never race it.
            match self.owner_liveness()? {
                OwnerLiveness::Dead(_) => {}
                OwnerLiveness::Alive => {
                    return Err(ValidateLockError::RecoveryNotProven(
                        "the recorded supervisor process is ALIVE and owns this census; \
                         refusing to take it out from under a running validate"
                            .into(),
                    ))
                }
                OwnerLiveness::Unknown(reason) => {
                    return Err(ValidateLockError::RecoveryNotProven(format!(
                        "supervisor liveness is UNVERIFIABLE, so its death is not proven: {reason}"
                    )))
                }
            }

            // (4) Re-assert the mechanically checkable half of absence at the
            // instant of the write, under the guard — not from the earlier
            // classification, which was computed before the caller was admitted.
            match exact_process_liveness(&leader) {
                Ok(false) => {}
                Ok(true) => {
                    return Err(ValidateLockError::RecoveryNotProven(format!(
                        "recorded leader {}:{} is ALIVE",
                        leader.pid, leader.start_ticks
                    )))
                }
                Err(reason) => {
                    return Err(ValidateLockError::RecoveryNotProven(format!(
                        "cannot verify recorded leader absence: {reason}"
                    )))
                }
            }
            if process_group_exists(pgid) {
                return Err(ValidateLockError::RecoveryNotProven(format!(
                    "recorded process group {pgid} is still populated"
                )));
            }

            // (5) The domain itself. With a recorded cgroup anchor the kernel
            // answers this directly and NO attestation is consumed; without one
            // the operator attestation remains the only available evidence.
            let disposition = census_disposition(
                census_recorded_domain(record.cgroup.as_deref()),
                args.attest_domain_empty,
                &args.evidence,
                &format!(
                    "supervisor dead, leader {}:{} absent, pgid {pgid} absent",
                    leader.pid, leader.start_ticks
                ),
            )
            .map_err(ValidateLockError::RecoveryNotProven)?;

            let mut recovered = record.clone();
            recovered.phase = CleanupPhase::Residual {
                pgid,
                domain_complete: true,
                residuals: Vec::new(),
            };
            write_cleanup_record(&self.paths.cleanup, &recovered)
                .map_err(|source| io_error("write", &self.paths.cleanup, source))?;
            Ok((leader, pgid, disposition))
        })?;
        // Truthful about WHICH evidence discharged this: a mechanically proven
        // census and an operator attestation are not the same claim, and a
        // reader must be able to tell them apart in the transcript.
        eprintln!(
            "validate-lock: CENSUSED orphaned domain agent={} operation={} leader={}:{} pgid={pgid}; \
             supervisor proven dead, leader and process group proven absent, {}",
            args.agent, args.operation, leader.pid, leader.start_ticks, disposition
        );
        eprintln!(
            "validate-lock: quarantine is now RECOVERABLE; run `validate-lock reclaim-dead` to \
             release the box."
        );
        Ok(0)
    }

    fn run(&self, args: RunArgs, root: &Path) -> Result<i32, ValidateLockError> {
        if args.child.is_empty() {
            return Err(ValidateLockError::EmptyChild);
        }
        if args.child_deadline == 0 {
            return Err(ValidateLockError::UnboundedChildDeadline);
        }
        reject_bad_max(args.max)?;
        enable_child_subreaper().map_err(|source| {
            io_error("enable child subreaper at", Path::new("/proc/self"), source)
        })?;

        // Immutable producer-floor admission at the chokepoint. Refuse cleanly
        // with STALE_BASE_EXIT_CODE (mirroring the --no-wait Held refusal) so a
        // wrapper reads a distinct, non-crashing exit and can surface the remedy.
        if let Err(err) = base_admission_check(root, &args.target, args.kind, args.skip_base_check)
        {
            match err {
                ValidateLockError::StaleBase(msg) => {
                    eprintln!("{msg}");
                    return Ok(STALE_BASE_EXIT_CODE);
                }
                other => return Err(other),
            }
        }

        // BOX-GLOBAL ANCHOR, TAKEN OUTERMOST AND HELD FOR THE WHOLE RUN.
        //
        // Before this, exclusion was per selected lease file, so two roots admitted
        // 2/2 concurrently. The anchor is taken BEFORE any lease work so a loser
        // waits without holding its own repository's lease, and AFTER
        // `base_admission_check` so a stale-base validate is refused without ever
        // occupying the box. `_box_anchor` lives until `run` returns; the kernel
        // drops the flock if this supervisor dies, which is why it can never wedge
        // the box the way a stale record can.
        //
        // RESIDUE, stated rather than papered over: the flock spans the SUPERVISOR,
        // so an escaped payload that outlives its supervisor releases the anchor
        // early. Within a repository that case is still covered by the durable
        // cleanup quarantine, which is unchanged and still consulted below; ACROSS
        // repositories it is not. Closing that needs a box-global durable record,
        // which is a different change from this one.
        let wait_budget = if args.no_wait { 0 } else { args.wait };
        let _box_anchor = match BoxExclusionAnchor::take(&self.paths.anchor, wait_budget)
            .map_err(|source| io_error("take box exclusion anchor", &self.paths.anchor, source))?
        {
            Some(anchor) => anchor,
            None => {
                eprintln!(
                    "REFUSED: box-exclusive anchor held by {}; this box admits one \
                     validate or benchmark at a time across ALL checkouts and \
                     clones, not one per lock file",
                    BoxExclusionAnchor::describe_holder(&self.paths.anchor)
                );
                return Ok(REFUSED_EXIT_CODE);
            }
        };
        eprintln!(
            "validate-lock: box-global anchor held at {}",
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

        // Admission: block FIFO, or refuse immediately under --no-wait. Never
        // silently admit a second box-exclusive holder.
        if args.no_wait {
            let token = self
                .with_guard(|| self.try_acquire(&args.agent, args.kind, &args.target, args.hold))?;
            match token {
                AcquireToken::Acquired => eprintln!(
                    "validate-lock: ACQUIRED by {} running {} {} (lease {}s)",
                    args.agent,
                    args.kind.as_str(),
                    args.target,
                    args.hold
                ),
                AcquireToken::AcquiredReclaimed(previous) => eprintln!(
                    "validate-lock: ACQUIRED by {} running {} {}; evidence-reclaimed lease from {previous}",
                    args.agent,
                    args.kind.as_str(),
                    args.target
                ),
                AcquireToken::Held {
                    agent,
                    kind,
                    target,
                    seconds_left,
                    ahead,
                } => {
                    eprintln!(
                        "REFUSED: box-exclusive lock held by {agent} running {kind} {target}, ~{seconds_left}s left; {ahead} ahead in queue"
                    );
                    return Ok(REFUSED_EXIT_CODE);
                }
                AcquireToken::WaitTurn { head, ahead } => {
                    eprintln!(
                        "REFUSED: box-exclusive lock free but {head} is ahead of {} in the FIFO queue, ~0s left; {ahead} ahead in queue",
                        args.agent
                    );
                    return Ok(REFUSED_EXIT_CODE);
                }
            }
        } else {
            let acquire_status =
                self.acquire(&args.agent, args.kind, &args.target, args.wait, args.hold)?;
            if acquire_status != 0 {
                return Ok(acquire_status);
            }
        }

        if let Err(error) = self.with_guard(|| self.write_current_process_owner()) {
            let _ = self.release(&args.agent, true);
            return Err(error);
        }

        // Install AFTER acquisition (so a stop during a long FIFO wait keeps its
        // default, immediate disposition — there is no cleanup record yet, and a
        // dead owner with no record is plain `reclaim-dead` territory) and BEFORE
        // `arm_run` writes the first durable cleanup authority, so every state
        // that `reclaim-dead` refuses is covered by a census on the way out.
        install_termination_handlers().map_err(|source| {
            io_error(
                "install termination handlers at",
                Path::new("/proc/self"),
                source,
            )
        })?;
        self.arm_run(&args.agent, args.kind, &args.target)?;
        #[cfg(test)]
        if crate::landing_lock::take_run_crash_hook(
            &format!("{}:{}", args.kind.as_str(), args.target),
            crate::landing_lock::RunCrashPoint::AfterArm,
        ) {
            return Err(ValidateLockError::CleanupQuarantined(
                "test-injected supervisor crash after arm".into(),
            ));
        }
        let owner_file = self.paths.owner.clone();
        let mut gated = match spawn_gated_child(&args.child[0], &args.child[1..], |command| {
            // The child may record `concurrent_validates=0` only when it can
            // prove it is descended from this live, process-bound lock owner.
            command
                .env(
                    "CI_HUB_VALIDATE_LOCK_OWNER_PID",
                    std::process::id().to_string(),
                )
                .env("CI_HUB_VALIDATE_LOCK_OWNER_FILE", &owner_file);
        }) {
            Ok(gated) => gated,
            Err(source) => {
                self.clear_unstarted_run(&args.agent, args.kind, &args.target)?;
                self.release(&args.agent, true)?;
                return Err(ValidateLockError::Io {
                    action: "launch start-gated child from",
                    path: PathBuf::from(&args.child[0]),
                    source,
                });
            }
        };
        #[cfg(test)]
        if crate::landing_lock::take_run_crash_hook(
            &format!("{}:{}", args.kind.as_str(), args.target),
            crate::landing_lock::RunCrashPoint::AfterSpawn,
        ) {
            return Err(ValidateLockError::CleanupQuarantined(
                "test-injected supervisor crash after spawn".into(),
            ));
        }
        self.publish_run(&args.agent, args.kind, &args.target, &gated)?;
        gated.release().map_err(|source| {
            io_error("release child start gate at", &self.paths.cleanup, source)
        })?;
        #[cfg(test)]
        if crate::landing_lock::take_run_crash_hook(
            &format!("{}:{}", args.kind.as_str(), args.target),
            crate::landing_lock::RunCrashPoint::AfterPublish,
        ) {
            return Err(ValidateLockError::CleanupQuarantined(
                "test-injected supervisor crash after publication".into(),
            ));
        }

        let (stop_tx, stop_rx) = mpsc::channel();
        let heartbeat_paths = self.paths.clone();
        let heartbeat_agent = args.agent.clone();
        let heartbeat_kind = args.kind;
        let heartbeat_target = args.target.clone();
        let heartbeat_hold = args.hold;
        let heartbeat = thread::spawn(move || {
            let heartbeat_lock = ValidateLock {
                paths: heartbeat_paths,
            };
            let interval = Duration::from_secs((heartbeat_hold / 3).max(1));
            while stop_rx.recv_timeout(interval).is_err() {
                if heartbeat_lock
                    .renew_run_lease(
                        &heartbeat_agent,
                        heartbeat_kind,
                        &heartbeat_target,
                        heartbeat_hold,
                    )
                    .is_err()
                {
                    break;
                }
            }
        });

        let outcome = supervise_child(&mut gated.child, args.child_deadline, &args.target);
        self.begin_run_census(&args.agent, args.kind, &args.target, &gated)?;
        let _ = stop_tx.send(());
        let _ = heartbeat.join();
        reap_exited_children();
        let pgid = outcome.pgid();
        let capture = capture_and_freeze_residuals(std::process::id());
        let domain_empty =
            !process_group_exists(pgid) && capture.complete && capture.identities.is_empty();
        if !domain_empty {
            let residual_count = capture.identities.len();
            self.record_residuals(&args.agent, args.kind, &args.target, pgid, capture)?;
            return Err(ValidateLockError::CleanupQuarantined(format!(
                "completed process group {pgid} did not yield an empty payload domain; {residual_count} exact residual identities recorded and lock retained"
            )));
        }
        self.clear_proven_run(&args.agent, args.kind, &args.target, pgid)?;
        self.release(&args.agent, true)?;
        match outcome {
            ChildOutcome::Exited { status, pgid } => {
                debug_assert_eq!(pgid, gated.pgid);
                Ok(exit_status_code(status))
            }
            ChildOutcome::TimedOut { pgid } => {
                debug_assert_eq!(pgid, gated.pgid);
                eprintln!(
                    "validate-lock: ABANDON {}: child exceeded --child-deadline {}s; \
                     killed the subtree and RELEASED the box so the FIFO can proceed.",
                    args.target, args.child_deadline
                );
                Ok(CHILD_DEADLINE_EXIT_CODE)
            }
            ChildOutcome::Signalled { pgid, signal } => {
                debug_assert_eq!(pgid, gated.pgid);
                eprintln!(
                    "validate-lock: ABANDON {}: supervisor was signalled ({signal}); \
                     killed the subtree, PROVED the payload domain empty, and RELEASED the box \
                     so the FIFO can proceed.",
                    args.target
                );
                // Same convention as `exit_status_code` for a signal death, so a
                // stopped supervisor is not mistaken for a payload exit code.
                Ok(128 + signal)
            }
            ChildOutcome::Uncertain { reason, .. } => Err(ValidateLockError::InvalidState(
                format!("child supervision failed after payload domain was proven empty: {reason}"),
            )),
        }
    }

    fn with_guard<T>(
        &self,
        operation: impl FnOnce() -> Result<T, ValidateLockError>,
    ) -> Result<T, ValidateLockError> {
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
                        return Err(ValidateLockError::GuardTimeout);
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

    fn read_holder(&self) -> Result<Option<ValidateLockState>, ValidateLockError> {
        if !self.paths.lock.exists() {
            return Ok(None);
        }
        let content = read_to_string(&self.paths.lock)?;
        Ok(Some(ValidateLockState::parse(&content)?))
    }

    fn read_authority_holder(
        &self,
    ) -> Result<AuthorityRecord<ValidateLockState>, ValidateLockError> {
        match self.read_holder() {
            Ok(Some(holder)) => Ok(AuthorityRecord::Present(holder)),
            Ok(None) => Ok(AuthorityRecord::Missing),
            Err(ValidateLockError::InvalidState(_)) => Ok(AuthorityRecord::Malformed),
            Err(ValidateLockError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                Ok(AuthorityRecord::Malformed)
            }
            Err(error) => Err(error),
        }
    }

    fn read_process_owner(&self) -> Result<Option<ProcessOwner>, ValidateLockError> {
        if !self.paths.owner.exists() {
            return Ok(None);
        }
        let content = read_to_string(&self.paths.owner)?;
        Ok(Some(ProcessOwner::parse(&content)?))
    }

    fn read_authority_owner(&self) -> Result<AuthorityRecord<ProcessOwner>, ValidateLockError> {
        match self.read_process_owner() {
            Ok(Some(owner)) => Ok(AuthorityRecord::Present(owner)),
            Ok(None) => Ok(AuthorityRecord::Missing),
            Err(ValidateLockError::InvalidState(_)) => Ok(AuthorityRecord::Malformed),
            Err(ValidateLockError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                Ok(AuthorityRecord::Malformed)
            }
            Err(error) => Err(error),
        }
    }

    fn write_current_process_owner(&self) -> Result<(), ValidateLockError> {
        let owner = current_process_owner()?;
        write_truncated(&self.paths.owner, owner.render().as_bytes())
    }

    fn owner_liveness(&self) -> Result<OwnerLiveness, ValidateLockError> {
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

    fn assert_current_process_owner(
        &self,
        operation: &'static str,
    ) -> Result<(), ValidateLockError> {
        let Some(owner) = self.read_process_owner()? else {
            return Ok(());
        };
        let current = current_process_owner()?;
        if owner == current {
            Ok(())
        } else {
            Err(ValidateLockError::ProcessNotOwner {
                operation,
                pid: owner.pid,
            })
        }
    }

    fn write_holder(&self, holder: &ValidateLockState) -> Result<(), ValidateLockError> {
        write_truncated(&self.paths.lock, holder.render().as_bytes())
    }

    fn read_queue(&self) -> Result<Vec<QueueEntry>, ValidateLockError> {
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

    fn write_queue(&self, queue: &[QueueEntry]) -> Result<(), ValidateLockError> {
        let mut content = String::new();
        for entry in queue {
            content.push_str(&entry.render());
        }
        let temporary = suffix(&self.paths.queue, ".tmp");
        write_truncated(&temporary, content.as_bytes())?;
        fs::rename(&temporary, &self.paths.queue)
            .map_err(|source| io_error("replace queue", &self.paths.queue, source))
    }

    fn remove_from_queue(&self, agent: &str) -> Result<(), ValidateLockError> {
        let mut queue = self.read_queue()?;
        queue.retain(|entry| entry.agent != agent);
        self.write_queue(&queue)
    }
}

fn new_holder(
    agent: &str,
    kind: Kind,
    target: &str,
    hold: u64,
    reclaimed_from: Option<String>,
) -> Result<ValidateLockState, ValidateLockError> {
    let acquired_at = epoch_seconds()?;
    let acquired_human = Local
        .timestamp_opt(acquired_at, 0)
        .single()
        .ok_or_else(|| ValidateLockError::InvalidState("current timestamp is out of range".into()))?
        .format("%Y-%m-%dT%H:%M:%S%z")
        .to_string();
    let host = current_host();
    Ok(ValidateLockState {
        agent: agent.to_string(),
        kind: kind.as_str().to_string(),
        target: target.to_string(),
        host,
        acquired_at,
        acquired_human,
        expires_at: acquired_at.saturating_add(hold as i64),
        reclaimed_from,
    })
}

// --- small helpers copied from landing_lock, retyped to ValidateLockError ---
// (Reusing landing_lock's would export its LandLockError into this module's
// error surface; these are small and stateless, so a faithful copy keeps the
// error type clean without touching any landing_lock logic.)

fn epoch_seconds() -> Result<i64, ValidateLockError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            ValidateLockError::InvalidState(format!("system clock before epoch: {error}"))
        })?
        .as_secs();
    i64::try_from(seconds)
        .map_err(|_| ValidateLockError::InvalidState("system time exceeds i64".into()))
}

fn parse_integer(name: &str, value: &str) -> Result<i64, ValidateLockError> {
    value.parse().map_err(|error| {
        ValidateLockError::InvalidState(format!(
            "{name} must be an integer, got {value:?}: {error}"
        ))
    })
}

fn parse_unsigned(name: &str, value: &str) -> Result<u64, ValidateLockError> {
    value.parse().map_err(|error| {
        ValidateLockError::InvalidState(format!(
            "{name} must be an integer, got {value:?}: {error}"
        ))
    })
}

/// Parse a restated `<pid>:<start_ticks>` payload-leader identity. `start_ticks`
/// is what defeats PID reuse, so a bare pid is refused rather than tolerated.
fn parse_leader_identity(value: &str) -> Result<ProcessIdentity, ValidateLockError> {
    let refuse = || {
        ValidateLockError::RecoveryNotProven(format!(
            "--leader must be <pid>:<start_ticks> exactly as `status` prints it, got {value:?}"
        ))
    };
    let (pid, start_ticks) = value.trim().split_once(':').ok_or_else(refuse)?;
    Ok(ProcessIdentity {
        pid: pid.parse().map_err(|_| refuse())?,
        start_ticks: start_ticks.parse().map_err(|_| refuse())?,
    })
}

fn current_boot_id() -> Result<String, ValidateLockError> {
    let path = Path::new("/proc/sys/kernel/random/boot_id");
    fs::read_to_string(path)
        .map(|value| value.trim().to_string())
        .map_err(|source| io_error("read", path, source))
}

/// Probe the uid-derived kernel anchor without creating it and without turning
/// an I/O error into a false "free" verdict. Authority consumers need the
/// fail-closed result; the older human/acquire probe intentionally remains best
/// effort because it must not wedge the box on a diagnostic filesystem error.
fn canonical_anchor_held(path: &Path) -> Result<bool, ValidateLockError> {
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(io_error("open canonical anchor", path, source)),
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(&file)
                .map_err(|source| io_error("unlock canonical anchor", path, source))?;
            Ok(false)
        }
        Err(source) if source.kind() == io::ErrorKind::WouldBlock => Ok(true),
        Err(source) => Err(io_error("probe canonical anchor", path, source)),
    }
}

fn current_process_owner() -> Result<ProcessOwner, ValidateLockError> {
    let pid = std::process::id();
    let start_ticks = process_start_ticks(pid).map_err(|source| ValidateLockError::Io {
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

fn render_liveness(liveness: &OwnerLiveness) -> String {
    match liveness {
        OwnerLiveness::Alive => "alive".into(),
        OwnerLiveness::Dead(reason) => format!("dead:{reason}"),
        OwnerLiveness::Unknown(reason) => format!("unknown:{reason}"),
    }
}

/// Emit the authoritative owner process identity in a stable, greppable form so
/// a consumer can bind admission to *actually descending from the live lease
/// owner*. Only a real lease acquisition writes the owner sidecar under the
/// flock guard, so these lines cannot be produced without holding the lease --
/// unlike an env var or a hand-written file, which any agent can forge. A legacy
/// or manual lease with no sidecar prints nothing here, so a consumer that
/// requires owner_pid will correctly refuse to treat it as admission.
fn print_owner_identity(owner: Option<&ProcessOwner>) {
    if let Some(owner) = owner {
        println!("  owner_pid={}", owner.pid);
        println!("  owner_boot_id={}", owner.boot_id);
        println!("  owner_start_ticks={}", owner.start_ticks);
    }
}

fn required<T>(value: Option<T>, name: &str) -> Result<T, ValidateLockError> {
    value.ok_or_else(|| ValidateLockError::InvalidState(format!("holder has no {name} field")))
}

fn read_to_string(path: &Path) -> Result<String, ValidateLockError> {
    let mut file = File::open(path).map_err(|source| io_error("open", path, source))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|source| io_error("read", path, source))?;
    Ok(content)
}

fn write_truncated(path: &Path, bytes: &[u8]) -> Result<(), ValidateLockError> {
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

fn remove_if_exists(path: &Path) -> Result<(), ValidateLockError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("remove", path, source)),
    }
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> ValidateLockError {
    ValidateLockError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// How a supervised `run` child ended.
enum ChildOutcome {
    Exited {
        status: ExitStatus,
        pgid: u32,
    },
    TimedOut {
        pgid: u32,
    },
    /// The SUPERVISOR was signalled (systemd stopping the unit TERMs the whole
    /// control group, so this is the common death path — not an exotic one).
    Signalled {
        pgid: u32,
        signal: i32,
    },
    Uncertain {
        pgid: u32,
        reason: String,
    },
}

impl ChildOutcome {
    fn pgid(&self) -> u32 {
        match self {
            Self::Exited { pgid, .. }
            | Self::TimedOut { pgid }
            | Self::Signalled { pgid, .. }
            | Self::Uncertain { pgid, .. } => *pgid,
        }
    }
}

/// First termination signal delivered to the supervisor, or 0. Written ONLY by
/// `record_termination_signal`.
static SUPERVISOR_TERMINATION_SIGNAL: AtomicI32 = AtomicI32::new(0);

/// Async-signal-safe handler: one lock-free atomic CAS, nothing else. No
/// allocation, no libc call, no I/O — everything real happens back in
/// `supervise_child`, which polls this flag.
extern "C" fn record_termination_signal(signal: libc::c_int) {
    let _ = SUPERVISOR_TERMINATION_SIGNAL.compare_exchange(
        0,
        signal as i32,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
}

/// Replace the DEFAULT disposition of the termination signals with a recorder,
/// so the supervisor survives long enough to census its payload domain.
///
/// This is the fix for the fleet-wide wedge of 2026-08-06: `validate-lock run`
/// had no handler, so when systemd stopped the transient unit (`KillMode`
/// defaults to `control-group`, TERMing every process in the cgroup at once) the
/// supervisor died instantly inside `supervise_child` and never reached
/// `begin_run_census`/`capture_and_freeze_residuals`. The record was left at
/// `phase=published`, which `verify_cleanup_record` classifies as `Uncensused` —
/// a state `reclaim_dead` refuses and, because the census can only be taken from
/// the live subreaper, nothing could ever discharge. Every agent's validate was
/// refused until it was cleared by hand.
///
/// SIGKILL and SIGSTOP remain uncatchable, so this narrows the window rather
/// than closing it; `census-orphaned-domain` covers the residue. systemd's
/// default `TimeoutStopSec` (90s) is far longer than a census (~11s worst case:
/// a 5s TERM grace, a 5s KILL settle, then four freeze iterations), so the
/// common path now completes.
fn install_termination_handlers() -> io::Result<()> {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: zeroed sigaction is a valid empty set; we install a plain
        // handler with no flags and read back nothing.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        // Via `*const ()` rather than a direct function-item-to-integer cast:
        // the direct form is what `function_casts_as_integer` warns about, and
        // it warned on every ci-hub invocation because cargo replays it from
        // cache. Same address, no behaviour change.
        action.sa_sigaction = record_termination_signal as *const () as usize;
        // SAFETY: sigemptyset initialises the mask field we just zeroed.
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        // SAFETY: `action` is fully initialised and outlives the call.
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Wait for `child`, killing its process group if it runs longer than
/// `deadline_secs`. Copied (not reused) from landing_lock so the deadline-kill
/// messages carry the `validate-lock:` prefix; `signal_group` itself is shared.
fn supervise_child(child: &mut Child, deadline_secs: u64, target: &str) -> ChildOutcome {
    let deadline = Instant::now() + Duration::from_secs(deadline_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return ChildOutcome::Exited {
                    status,
                    pgid: child.id(),
                }
            }
            Ok(None) => {}
            Err(source) => match child.wait() {
                Ok(status) => {
                    return ChildOutcome::Exited {
                        status,
                        pgid: child.id(),
                    }
                }
                Err(_) => {
                    eprintln!("validate-lock: cannot wait on child: {source}");
                    return ChildOutcome::Uncertain {
                        pgid: child.id(),
                        reason: source.to_string(),
                    };
                }
            },
        }
        // A termination signal aimed at the SUPERVISOR must still fall through to
        // the census below, never skip it. Check before the deadline so a stop
        // during the final poll is not misreported as a deadline breach.
        let signal = SUPERVISOR_TERMINATION_SIGNAL.load(Ordering::SeqCst);
        if signal != 0 {
            let pgid = child.id();
            eprintln!(
                "validate-lock: supervisor received signal {signal} while running {target}; \
                 terminating the payload subtree and censusing it before releasing the box."
            );
            // Name the SIGNAL as the cause. Reusing the deadline phrase here is
            // what made the durable log contradict itself mid-record.
            terminate_child_group(child, target, &signalled_cause(signal));
            return ChildOutcome::Signalled { pgid, signal };
        }
        if Instant::now() >= deadline {
            let pgid = child.id();
            terminate_child_group(child, target, TERMINATION_CAUSE_DEADLINE);
            return ChildOutcome::TimedOut { pgid };
        }
        thread::sleep(Duration::from_millis(CHILD_POLL_MILLIS));
    }
}

/// The reason the payload subtree is being torn down, so the durable log names
/// the cause it actually observed.
///
/// This function is shared by the deadline path and the supervisor-signal path,
/// and it used to hardcode "child-deadline reached" for both. A postmortem
/// reading that line after an ordinary `systemctl stop` (KillMode=control-group,
/// SIGTERM to the supervisor) concluded the payload had overrun its deadline when
/// nothing of the kind happened -- the surrounding lines said "received signal
/// 15" and "supervisor was signalled", so the record contradicted itself in the
/// middle. The cause is now passed in rather than assumed.
///
/// `DEADLINE`'s text is deliberately BYTE-IDENTICAL to the old message, so any
/// log reader keying on that phrase keeps working for the case where it was
/// always true.
const TERMINATION_CAUSE_DEADLINE: &str = "child-deadline reached";

/// SIGTERM then (after a grace period) SIGKILL the child's process group and reap
/// the direct child. `signal_group` is reused from landing_lock.
///
/// `cause` names why the teardown is happening; it does NOT affect what the
/// teardown does. Exit codes are unchanged and are decided by the caller's
/// `ChildOutcome` (deadline = 124, signal = 128 + signal).
/// The teardown notice, as a pure function so both phrasings are testable
/// without spawning or signalling anything.
fn termination_notice(cause: &str, target: &str, group: &str) -> String {
    format!("validate-lock: {cause} for {target}; SIGTERM process group {group}")
}

/// The cause phrase for a supervisor that was signalled, naming the signal.
fn signalled_cause(signal: i32) -> String {
    format!("supervisor signalled ({signal})")
}

fn terminate_child_group(child: &mut Child, target: &str, cause: &str) {
    let pgid = child.id();
    let group = format!("-{pgid}");
    eprintln!("{}", termination_notice(cause, target, &group));
    signal_group(libc::SIGTERM, pgid);
    let grace = Instant::now() + Duration::from_secs(CHILD_TERM_GRACE_SECONDS);
    while Instant::now() < grace {
        thread::sleep(Duration::from_millis(CHILD_POLL_MILLIS));
    }
    eprintln!("validate-lock: grace expired for {target}; SIGKILL process group {group}");
    let kill_sent = signal_group(libc::SIGKILL, pgid);
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

#[cfg(test)]
mod tests {

    // ---------------------------------------------------------------------
    // Teardown cause attribution. The bug was NOT that the log was missing --
    // it was that the log CONFIDENTLY NAMED THE WRONG CAUSE, so a postmortem
    // read a deadline breach off a record whose neighbouring lines said the
    // supervisor had been signalled. Both phrasings are asserted, because a
    // fix that made every line say "terminated" would erase the deadline
    // signal instead of correcting the signal one.
    #[test]
    fn signal_teardown_does_not_claim_a_deadline() {
        let notice = termination_notice(&signalled_cause(15), "4c70658e", "-3475200");
        assert!(
            notice.contains("supervisor signalled (15)"),
            "signal path must name the signal: {notice}"
        );
        assert!(
            !notice.contains("deadline"),
            "signal path must NOT claim a deadline: {notice}"
        );
    }

    #[test]
    fn deadline_teardown_still_says_deadline() {
        let notice = termination_notice(TERMINATION_CAUSE_DEADLINE, "4c70658e", "-3475200");
        assert!(notice.contains("child-deadline reached"), "{notice}");
        assert!(!notice.contains("signalled"), "{notice}");
    }

    #[test]
    fn deadline_wording_is_byte_identical_to_the_historical_message() {
        // Log readers key on this exact phrase for the case where it was always
        // true; changing it would break them while fixing nothing.
        assert_eq!(
            termination_notice(TERMINATION_CAUSE_DEADLINE, "abc123", "-42"),
            "validate-lock: child-deadline reached for abc123; SIGTERM process group -42"
        );
    }

    #[test]
    fn the_two_causes_are_distinguishable() {
        // The whole point: a reader must be able to tell them apart.
        let a = termination_notice(TERMINATION_CAUSE_DEADLINE, "t", "-1");
        let b = termination_notice(&signalled_cause(9), "t", "-1");
        assert_ne!(a, b);
    }

    #[test]
    fn the_signal_number_is_carried_not_generic() {
        assert!(signalled_cause(9).contains('9'));
        assert!(signalled_cause(15).contains("15"));
        assert_ne!(signalled_cause(9), signalled_cause(15));
    }
    use super::*;
    use crate::landing_lock::{exact_process_liveness, signal_exact_process, ProcessIdentity};
    use std::os::unix::process::ExitStatusExt;

    const HARD_DEATH_ROOT_ENV: &str = "CI_HUB_VALIDATE_HARD_DEATH_ROOT";
    const HARD_DEATH_POINT_ENV: &str = "CI_HUB_VALIDATE_HARD_DEATH_POINT";
    const AUTHORITY_STATUS_ROOT_ENV: &str = "CI_HUB_VALIDATE_AUTHORITY_STATUS_ROOT";

    fn paths_at(root: &Path) -> LockPaths {
        let lock = root.join(".validate-lock");
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
            "ci-hub-validate-lock-{name}-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        paths_at(&root)
    }

    #[test]
    fn forged_child_proof_cannot_manufacture_canonical_authority() {
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let paths = temp_paths("authority-forged-proof");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let lock = ValidateLock {
            paths: paths.clone(),
        };

        // This is the exact old trust boundary: a child could choose both the
        // claimed supervisor PID and the sidecar path. Make the forgery as
        // convincing as possible (a genuinely live identity), but keep it away
        // from the canonical lock domain. The authority query must not read
        // either environment variable or this caller-selected file.
        let forged_owner = root.join("caller-selected.owner");
        let owner = current_process_owner().unwrap();
        write_truncated(&forged_owner, owner.render().as_bytes()).unwrap();
        env::set_var(
            "CI_HUB_VALIDATE_LOCK_OWNER_PID",
            std::process::id().to_string(),
        );
        env::set_var("CI_HUB_VALIDATE_LOCK_OWNER_FILE", &forged_owner);

        let status = lock.with_guard(|| lock.authority_status_guarded()).unwrap();

        env::remove_var("CI_HUB_VALIDATE_LOCK_OWNER_PID");
        env::remove_var("CI_HUB_VALIDATE_LOCK_OWNER_FILE");
        assert_eq!(status.schema_version, 1);
        assert!(!status.admissible);
        assert_eq!(status.state, AuthorityState::Free);
        assert_eq!(
            status.reason_code.as_deref(),
            Some("canonical-lock-not-held")
        );
        assert!(status.holder.is_none());
        assert!(status.owner.is_none());
        assert!(!status.canonical_anchor_held);
        assert_eq!(status.cleanup_state, AuthorityCleanupState::None);

        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["state"], "free");
        assert_eq!(json["reason_code"], "canonical-lock-not-held");
        assert_eq!(json["holder"], serde_json::Value::Null);
        assert_eq!(json["owner"], serde_json::Value::Null);
        let _ = fs::remove_dir_all(root);
    }

    fn planted_live_authority(paths: &LockPaths) -> (ValidateLock, BoxExclusionAnchor) {
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let holder = new_holder("authority-test", Kind::Validate, HEX_SHA, 60, None).unwrap();
        lock.write_holder(&holder).unwrap();
        write_truncated(
            &paths.owner,
            current_process_owner().unwrap().render().as_bytes(),
        )
        .unwrap();
        let anchor = BoxExclusionAnchor::take(&paths.anchor, 0)
            .unwrap()
            .expect("per-test canonical anchor must be available");
        (lock, anchor)
    }

    #[test]
    fn authority_refuses_boot_identity_mismatch() {
        let paths = temp_paths("authority-boot-mismatch");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let (lock, _anchor) = planted_live_authority(&paths);
        let mut owner = current_process_owner().unwrap();
        owner.boot_id = "forged-boot-id".into();
        write_truncated(&paths.owner, owner.render().as_bytes()).unwrap();

        let status = lock.with_guard(|| lock.authority_status_guarded()).unwrap();
        assert!(!status.admissible);
        assert_eq!(status.state, AuthorityState::Orphaned);
        assert_eq!(
            status.reason_code.as_deref(),
            Some("canonical-owner-boot-id-mismatch")
        );
        assert_eq!(
            status.owner.as_ref().map(|owner| owner.liveness),
            Some(AuthorityOwnerLiveness::Dead)
        );
        assert!(status.canonical_anchor_held);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn authority_refuses_pid_start_identity_mismatch() {
        let paths = temp_paths("authority-start-mismatch");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let (lock, _anchor) = planted_live_authority(&paths);
        let mut owner = current_process_owner().unwrap();
        owner.start_ticks = owner.start_ticks.saturating_add(1);
        write_truncated(&paths.owner, owner.render().as_bytes()).unwrap();

        let status = lock.with_guard(|| lock.authority_status_guarded()).unwrap();
        assert!(!status.admissible);
        assert_eq!(status.state, AuthorityState::Orphaned);
        assert_eq!(
            status.reason_code.as_deref(),
            Some("canonical-owner-start-ticks-mismatch")
        );
        assert_eq!(
            status.owner.as_ref().map(|owner| owner.liveness),
            Some(AuthorityOwnerLiveness::Dead)
        );
        assert!(status.canonical_anchor_held);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn authority_requires_anchor_live_validate_holder_and_owner() {
        struct Case {
            name: &'static str,
            kind: Kind,
            hold: u64,
            take_anchor: bool,
            write_owner: bool,
            state: AuthorityState,
            reason: &'static str,
        }
        for case in [
            Case {
                name: "no-anchor",
                kind: Kind::Validate,
                hold: 60,
                take_anchor: false,
                write_owner: true,
                state: AuthorityState::Orphaned,
                reason: "canonical-anchor-not-held",
            },
            Case {
                name: "expired",
                kind: Kind::Validate,
                hold: 0,
                take_anchor: true,
                write_owner: true,
                state: AuthorityState::Lapsed,
                reason: "canonical-holder-lapsed",
            },
            Case {
                name: "bench",
                kind: Kind::Bench,
                hold: 60,
                take_anchor: true,
                write_owner: true,
                state: AuthorityState::Held,
                reason: "canonical-holder-kind-not-validate",
            },
            Case {
                name: "missing-owner",
                kind: Kind::Validate,
                hold: 60,
                take_anchor: true,
                write_owner: false,
                state: AuthorityState::Indeterminate,
                reason: "canonical-owner-missing",
            },
        ] {
            let paths = temp_paths(&format!("authority-{}", case.name));
            let root = paths.lock.parent().unwrap().to_path_buf();
            let lock = ValidateLock {
                paths: paths.clone(),
            };
            lock.write_holder(
                &new_holder("authority-test", case.kind, HEX_SHA, case.hold, None).unwrap(),
            )
            .unwrap();
            if case.write_owner {
                write_truncated(
                    &paths.owner,
                    current_process_owner().unwrap().render().as_bytes(),
                )
                .unwrap();
            }
            let _anchor = case
                .take_anchor
                .then(|| BoxExclusionAnchor::take(&paths.anchor, 0).unwrap().unwrap());

            let status = lock.with_guard(|| lock.authority_status_guarded()).unwrap();
            assert!(!status.admissible, "case {} admitted", case.name);
            assert_eq!(status.state, case.state, "case {} state", case.name);
            assert_eq!(
                status.reason_code.as_deref(),
                Some(case.reason),
                "case {} reason",
                case.name
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn malformed_canonical_records_are_json_refusals_not_query_errors() {
        for (name, malformed_holder, reason) in [
            ("holder", true, "canonical-holder-malformed"),
            ("owner", false, "canonical-owner-malformed"),
        ] {
            let paths = temp_paths(&format!("authority-malformed-{name}"));
            let root = paths.lock.parent().unwrap().to_path_buf();
            let lock = ValidateLock {
                paths: paths.clone(),
            };
            if malformed_holder {
                write_truncated(&paths.lock, b"not-a-holder\n").unwrap();
            } else {
                lock.write_holder(
                    &new_holder("authority-test", Kind::Validate, HEX_SHA, 60, None).unwrap(),
                )
                .unwrap();
                write_truncated(&paths.owner, b"not-an-owner\n").unwrap();
            }
            let _anchor = BoxExclusionAnchor::take(&paths.anchor, 0).unwrap().unwrap();
            let status = lock.with_guard(|| lock.authority_status_guarded()).unwrap();
            assert!(!status.admissible);
            assert_eq!(status.state, AuthorityState::Indeterminate);
            assert_eq!(status.reason_code.as_deref(), Some(reason));
            serde_json::to_string(&status).expect("refusal must remain valid JSON");
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn authority_internal_anchor_io_error_is_not_a_json_refusal() {
        let paths = temp_paths("authority-anchor-io-error");
        let root = paths.lock.parent().unwrap().to_path_buf();
        fs::create_dir(&paths.anchor).unwrap();
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let error = lock
            .with_guard(|| lock.authority_status_guarded())
            .expect_err("an unreadable kernel anchor is an internal query error");
        assert!(matches!(error, ValidateLockError::Io { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn active_cleanup_is_admissible_only_while_payload_is_published() {
        for (name, phase, reason) in [
            (
                "census-pending",
                CleanupPhase::CensusPending {
                    leader: ProcessIdentity {
                        pid: std::process::id(),
                        start_ticks: process_start_ticks(std::process::id()).unwrap(),
                    },
                    pgid: u32::MAX,
                },
                "canonical-cleanup-census-pending",
            ),
            (
                "residual",
                CleanupPhase::Residual {
                    pgid: u32::MAX,
                    domain_complete: true,
                    residuals: vec![ProcessIdentity {
                        pid: std::process::id(),
                        start_ticks: process_start_ticks(std::process::id()).unwrap(),
                    }],
                },
                "canonical-cleanup-residual-active",
            ),
        ] {
            let paths = temp_paths(&format!("authority-active-{name}"));
            let root = paths.lock.parent().unwrap().to_path_buf();
            let (lock, _anchor) = planted_live_authority(&paths);
            let record =
                CleanupRecord::new("authority-test", format!("validate:{HEX_SHA}"), phase).unwrap();
            write_cleanup_record(&paths.cleanup, &record).unwrap();

            let status = lock.with_guard(|| lock.authority_status_guarded()).unwrap();
            assert!(!status.admissible, "active {name} admitted");
            assert_eq!(status.state, AuthorityState::Quarantined);
            assert_eq!(status.reason_code.as_deref(), Some(reason));
            assert_eq!(status.cleanup_state, AuthorityCleanupState::ActiveBound);
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn authority_command_constructor_ignores_all_ambient_lock_proofs() {
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = env::temp_dir().join(format!(
            "ci-hub-authority-strict-constructor-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        let redirected = root.join("attacker-selected-lock");
        env::set_var(VALIDATE_LOCK_PATH_ENV, &redirected);
        env::set_var("CI_HUB_VALIDATE_LOCK_OWNER_PID", "1");
        env::set_var("CI_HUB_VALIDATE_LOCK_OWNER_FILE", &redirected);

        let strict = LockPaths::for_authority_query(&root);

        env::remove_var(VALIDATE_LOCK_PATH_ENV);
        env::remove_var("CI_HUB_VALIDATE_LOCK_OWNER_PID");
        env::remove_var("CI_HUB_VALIDATE_LOCK_OWNER_FILE");
        assert_eq!(strict.lock, root.join(".validate-lock"));
        assert_ne!(strict.lock, redirected);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn authority_status_child_helper() {
        let Some(root) = env::var_os(AUTHORITY_STATUS_ROOT_ENV).map(PathBuf::from) else {
            return;
        };
        let lock = ValidateLock {
            paths: paths_at(&root),
        };
        let first = lock.with_guard(|| lock.authority_status_guarded()).unwrap();
        let second = lock.with_guard(|| lock.authority_status_guarded()).unwrap();
        for status in [&first, &second] {
            assert!(
                status.admissible,
                "real run must be authoritative: {status:?}"
            );
            assert_eq!(status.state, AuthorityState::Held);
            assert_eq!(status.reason_code, None);
            assert_eq!(status.cleanup_state, AuthorityCleanupState::ActiveBound);
            assert!(status.canonical_anchor_held);
            assert_eq!(
                status.holder.as_ref().map(|holder| holder.target.as_str()),
                Some(HEX_SHA)
            );
            assert_eq!(
                status.owner.as_ref().map(|owner| owner.liveness),
                Some(AuthorityOwnerLiveness::Alive)
            );
            let json = serde_json::to_value(status).unwrap();
            assert!(json["holder"]["acquired_at"].is_i64());
            assert!(json["holder"]["expires_at"].is_i64());
            assert!(json["owner"]["pid"].is_u64());
            assert!(json["owner"]["start_ticks"].is_u64());
        }
        assert_eq!(first.holder, second.holder);
        assert_eq!(first.owner, second.owner);
    }

    #[test]
    fn real_validate_run_reports_same_live_authority_twice() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let stub = write_stub("authority-admit", "#!/bin/sh\necho OK\nexit 0\n");
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);

        let paths = temp_paths("authority-real-run");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        env::set_var(AUTHORITY_STATUS_ROOT_ENV, &root);
        let code = lock
            .run(
                RunArgs {
                    agent: "authority-real-run".into(),
                    kind: Kind::Validate,
                    target: HEX_SHA.into(),
                    no_wait: false,
                    wait: 0,
                    hold: 30,
                    child_deadline: 30,
                    max: 1,
                    skip_base_check: false,
                    child: vec![
                        env::current_exe().unwrap().into_os_string(),
                        OsString::from("--exact"),
                        OsString::from("validate_lock::tests::authority_status_child_helper"),
                        OsString::from("--nocapture"),
                        OsString::from("--test-threads=1"),
                    ],
                },
                &root,
            )
            .unwrap();

        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        env::remove_var(AUTHORITY_STATUS_ROOT_ENV);
        let _ = fs::remove_file(&stub);
        assert_eq!(code, 0);
        assert!(lock.read_holder().unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }

    // 1. Byte round-trip for the holder (kind+target) and a queue entry.
    #[test]
    fn holder_and_queue_round_trip() {
        let holder = ValidateLockState {
            agent: "hermit-247".into(),
            kind: "validate".into(),
            target: "0123456789abcdef0123456789abcdef01234567".into(),
            host: "testhost".into(),
            acquired_at: 100,
            acquired_human: "1970-01-01T00:01:40+0000".into(),
            expires_at: 1_000,
            reclaimed_from: Some("hermit-opt".into()),
        };
        let rendered = "agent=hermit-247\nkind=validate\ntarget=0123456789abcdef0123456789abcdef01234567\nhost=testhost\nacquired_at=100\nacquired_human=1970-01-01T00:01:40+0000\nexpires_at=1000\nreclaimed_from=hermit-opt\n";
        assert_eq!(holder.render(), rendered);
        assert_eq!(ValidateLockState::parse(rendered).unwrap(), holder);

        let entry = QueueEntry {
            enqueued_at: 1785770487,
            agent: "hermit-opt".into(),
            target: "bench:cpu-vs-syscall".into(),
        };
        assert_eq!(
            entry.render(),
            "1785770487\thermit-opt\tbench:cpu-vs-syscall\n"
        );
        assert_eq!(
            QueueEntry::parse(entry.render().trim_end(), 1).unwrap(),
            entry
        );
    }

    // 2. N=3 sequential box-exclusive jobs all admitted, lock ends FREE.
    #[test]
    fn positive_non_starvation_three_sequential() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        let paths = temp_paths("three-sequential");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        for i in 0..3 {
            let code = lock
                .run(
                    RunArgs {
                        agent: format!("validate-agent-{i}"),
                        kind: Kind::Bench,
                        target: format!("sha-{i}"),
                        no_wait: false,
                        wait: 0,
                        hold: 30,
                        child_deadline: 30,
                        max: 1,
                        skip_base_check: false,
                        child: vec![OsString::from("/bin/true")],
                    },
                    paths.lock.parent().unwrap(),
                )
                .unwrap();
            assert_eq!(code, 0, "sequential validate {i} should be admitted");
            assert!(
                lock.read_holder().unwrap().is_none(),
                "lock must be FREE after validate {i}"
            );
        }
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn run_child_receives_process_bound_exclusivity_proof() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        let paths = temp_paths("child-proof");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let command = "test \"$CI_HUB_VALIDATE_LOCK_OWNER_PID\" = \"$PPID\" \
            && test -r \"$CI_HUB_VALIDATE_LOCK_OWNER_FILE\" \
            && test \"$(sed -n 's/^pid=//p' \"$CI_HUB_VALIDATE_LOCK_OWNER_FILE\")\" = \"$PPID\"";
        let code = lock
            .run(
                RunArgs {
                    agent: "proof-agent".into(),
                    kind: Kind::Bench,
                    target: "proof-sha".into(),
                    no_wait: false,
                    wait: 0,
                    hold: 30,
                    child_deadline: 30,
                    max: 1,
                    skip_base_check: false,
                    child: vec![
                        OsString::from("/bin/sh"),
                        OsString::from("-c"),
                        OsString::from(command),
                    ],
                },
                paths.lock.parent().unwrap(),
            )
            .unwrap();
        assert_eq!(code, 0, "child must verify the live wrapper owner proof");
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    // 3. Second box-exclusive holder is refused/queued; both kinds share the lock.
    #[test]
    fn negative_second_holder_refused_and_queued() {
        let paths = temp_paths("second-holder");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        // agent1 holds the box as a validate.
        assert_eq!(
            lock.acquire("agent1", Kind::Validate, "sha1", 0, 60)
                .unwrap(),
            0
        );

        // (a) a second agent's try_acquire returns Held (not Acquired), naming agent1.
        let token = lock
            .with_guard(|| lock.try_acquire("agent2", Kind::Validate, "sha2", 60))
            .unwrap();
        assert!(
            matches!(&token, AcquireToken::Held { agent, .. } if agent == "agent1"),
            "second validate must see agent1 holding the box"
        );

        // (b) the --no-wait refuse path returns exit code 3.
        let code = lock
            .run(
                RunArgs {
                    agent: "agent2".into(),
                    kind: Kind::Bench,
                    target: "sha2".into(),
                    no_wait: true,
                    wait: 0,
                    hold: 60,
                    child_deadline: 30,
                    max: 1,
                    skip_base_check: false,
                    child: vec![OsString::from("/bin/true")],
                },
                paths.lock.parent().unwrap(),
            )
            .unwrap();
        assert_eq!(code, REFUSED_EXIT_CODE, "no-wait must refuse with exit 3");

        // (c) a BENCH-kind second request is ALSO blocked by the validate holder.
        let bench_token = lock
            .with_guard(|| lock.try_acquire("agent3", Kind::Bench, "bench:x", 60))
            .unwrap();
        assert!(
            matches!(&bench_token, AcquireToken::Held { agent, .. } if agent == "agent1"),
            "a bench must be blocked by a validate holder (shared lock)"
        );

        // Release agent1; the queued agent2 (head of FIFO) then acquires.
        lock.release("agent1", false).unwrap();
        assert_eq!(
            lock.acquire("agent2", Kind::Validate, "sha2", 0, 60)
                .unwrap(),
            0
        );
        let holder = lock.read_holder().unwrap().unwrap();
        assert_eq!(holder.agent, "agent2");
        lock.release("agent2", false).unwrap();
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    // 4. run child-deadline kills the child quickly and releases the box.
    #[test]
    fn run_child_deadline_kills_and_releases() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        let paths = temp_paths("child-deadline");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let started = Instant::now();
        let code = lock
            .run(
                RunArgs {
                    agent: "stuck-validate".into(),
                    kind: Kind::Bench,
                    target: "sha-stuck".into(),
                    no_wait: false,
                    wait: 0,
                    hold: 30,
                    child_deadline: 1,
                    max: 1,
                    skip_base_check: false,
                    child: vec![OsString::from("sleep"), OsString::from("120")],
                },
                paths.lock.parent().unwrap(),
            )
            .unwrap();
        assert_eq!(code, CHILD_DEADLINE_EXIT_CODE);
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "child-deadline run took {:?}",
            started.elapsed()
        );
        assert!(lock.read_holder().unwrap().is_none());
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn run_kills_stubborn_descendant_before_releasing_box() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        let paths = temp_paths("stubborn-descendant");
        let lock = ValidateLock {
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
            .run(
                RunArgs {
                    agent: "stubborn-validate".into(),
                    kind: Kind::Bench,
                    target: "sha-stubborn".into(),
                    no_wait: false,
                    wait: 0,
                    hold: 30,
                    child_deadline: 1,
                    max: 1,
                    skip_base_check: false,
                    child: vec![
                        OsString::from("/bin/bash"),
                        OsString::from("-c"),
                        OsString::from(script),
                        OsString::from("stubborn-descendant"),
                        identity_path.clone().into_os_string(),
                    ],
                },
                paths.lock.parent().unwrap(),
            )
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
            "exact descendant {pid}@{start_ticks} survived box release"
        );
        assert!(!process_group_exists(pgid));
        assert!(lock.read_holder().unwrap().is_none());
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    // THE POLICY BRACKET. Six cases, covering every way a census can and cannot
    // be discharged. `census_disposition` is the whole behavioural change of the
    // cgroup anchor, so each branch is pinned explicitly.
    const CTX: &str = "supervisor dead, leader 1:2 absent, pgid 3 absent";

    // Mechanical, no attestation offered: accepted, and the transcript says
    // PROVEN. This is the case that did not exist before.
    #[test]
    fn a_proven_empty_domain_is_discharged_without_any_attestation() {
        let ok = census_disposition(
            DomainEvidence::ProvenEmpty("payload cgroup /x is absent".into()),
            false,
            "",
            CTX,
        )
        .expect("a mechanically proven domain must discharge with no attestation");
        assert!(ok.contains("PROVEN"), "got: {ok}");
        assert!(!ok.contains("ATTESTED"), "must not claim attestation: {ok}");
    }

    // An attestation supplied alongside mechanical proof is redundant, not
    // corrupting: the transcript must still report PROVEN, not ATTESTED.
    #[test]
    fn mechanical_proof_outranks_a_supplied_attestation() {
        let ok = census_disposition(
            DomainEvidence::ProvenEmpty("payload cgroup /x is absent".into()),
            true,
            "I looked",
            CTX,
        )
        .unwrap();
        assert!(
            ok.contains("PROVEN") && !ok.contains("ATTESTED"),
            "got: {ok}"
        );
    }

    // A live domain is refused by DEFAULT: no attestation, no discharge.
    #[test]
    fn a_populated_domain_is_refused_without_an_explicit_override() {
        let refusal = census_disposition(
            DomainEvidence::Populated("payload cgroup /x still holds 2 process(es): 11,22".into()),
            false,
            "",
            CTX,
        )
        .expect_err("a populated domain must not discharge on its own");
        assert!(refusal.contains("NOT empty"), "got: {refusal}");
        assert!(
            refusal.contains("11,22"),
            "must name the occupants, got: {refusal}"
        );
    }

    // An override IS possible -- refusing outright would create a NEW
    // undischargeable state, the very thing this work exists to remove -- but it
    // can never be silent. The disposition must be unmistakably labelled and
    // must carry the pids the kernel reported, so it can never later be read as
    // a clean mechanical census.
    #[test]
    fn overriding_a_populated_domain_is_allowed_but_loudly_recorded() {
        let ok = census_disposition(
            DomainEvidence::Populated("payload cgroup /x still holds 2 process(es): 11,22".into()),
            true,
            "operator checked: strays are unrelated",
            CTX,
        )
        .expect("an explicit, evidenced override must be possible");
        assert!(
            ok.contains("ATTESTED OVER A POPULATED CGROUP"),
            "an override must be labelled as one, got: {ok}"
        );
        assert!(ok.contains("11,22"), "must carry the occupants, got: {ok}");
        assert!(
            !ok.contains("PROVEN"),
            "must never read as mechanical: {ok}"
        );
    }

    // ...and an anonymous override is still refused.
    #[test]
    fn an_anonymous_override_of_a_populated_domain_is_refused() {
        assert!(census_disposition(
            DomainEvidence::Populated("/x holds 1".into()),
            true,
            "  ",
            CTX,
        )
        .unwrap_err()
        .contains("not auditable"));
    }

    // Unprovable and unattested: refused, and the refusal must name BOTH why it
    // could not be proven and the exact remedy.
    #[test]
    fn an_unprovable_domain_without_attestation_is_refused_with_the_remedy() {
        let refusal = census_disposition(
            DomainEvidence::Unproven("no cgroup anchor".into()),
            false,
            "",
            CTX,
        )
        .unwrap_err();
        assert!(refusal.contains("no cgroup anchor"), "got: {refusal}");
        assert!(refusal.contains("--attest-domain-empty"), "got: {refusal}");
        assert!(
            refusal.contains(CTX),
            "must state what DID pass, got: {refusal}"
        );
    }

    // The attestation must stay auditable; an anonymous one is still refused.
    #[test]
    fn an_anonymous_attestation_is_still_refused() {
        let refusal = census_disposition(
            DomainEvidence::Unproven("no cgroup anchor".into()),
            true,
            "   ",
            CTX,
        )
        .unwrap_err();
        assert!(refusal.contains("not auditable"), "got: {refusal}");
    }

    // LEGACY PATH INTACT: unprovable + attested + real evidence still
    // discharges, exactly as before this change, and is labelled ATTESTED.
    #[test]
    fn an_attested_unprovable_domain_still_discharges_as_before() {
        let ok = census_disposition(
            DomainEvidence::Unproven("no cgroup anchor".into()),
            true,
            "unit gone; safe-ci.slice 0 procs",
            CTX,
        )
        .unwrap();
        assert!(ok.contains("ATTESTED"), "got: {ok}");
        assert!(
            ok.contains("safe-ci.slice 0 procs"),
            "evidence must be echoed: {ok}"
        );
        assert!(
            !ok.contains("PROVEN:"),
            "must not claim mechanical proof: {ok}"
        );
    }

    // LEGACY FALLBACK PRESERVED: a record written before cgroup anchoring has
    // no anchor, so the domain is Unproven and the operator attestation stays
    // the only route. This is the compatibility guarantee of the version bump.
    #[test]
    fn a_record_without_a_cgroup_anchor_is_unprovable_and_keeps_attestation() {
        match census_recorded_domain(None) {
            DomainEvidence::Unproven(why) => assert!(
                why.contains("no cgroup anchor"),
                "the reason must name the missing anchor, got: {why}"
            ),
            other => panic!("an anchorless record must be Unproven, got {other:?}"),
        }
    }

    // NEGATIVE CONTROL, and the safety property that matters most: pointed at a
    // domain that really does hold a live process -- this one -- the census must
    // report Populated. `census_orphaned_domain` refuses that unconditionally,
    // so --attest-domain-empty cannot bury a live domain.
    #[test]
    fn a_live_domain_is_reported_populated_and_never_provable_empty() {
        let Some(mine) = crate::landing_lock::process_cgroup(std::process::id()) else {
            eprintln!("skipping: no cgroup-v2 entry for self on this host");
            return;
        };
        match census_recorded_domain(Some(&mine)) {
            DomainEvidence::Populated(detail) => assert!(
                detail.contains(&std::process::id().to_string()),
                "must name the live pid, got: {detail}"
            ),
            other => panic!("a live domain must be Populated, got {other:?}"),
        }
    }

    // POSITIVE: an absent payload cgroup whose derived safe-ci.slice is also
    // absent is PROVEN empty with no attestation at all. This is the case that
    // was unrepresentable before -- during the 2026-08-06 wedge the domain was
    // provably empty and the tool still could not say so.
    #[test]
    fn an_absent_domain_is_proven_empty_with_no_attestation() {
        let absent = "/user.slice/user-999999.slice/user@999999.service/ci-hub-absent.scope";
        match census_recorded_domain(Some(absent)) {
            DomainEvidence::ProvenEmpty(detail) => {
                assert!(detail.contains("payload cgroup"), "got: {detail}");
                // Both domains must be named: proving only the payload cgroup
                // while silently skipping the migration target is exactly the
                // hole this check exists to close.
                assert!(
                    detail.contains("migration target"),
                    "the proof must cover safe-ci.slice too, got: {detail}"
                );
            }
            other => panic!("an absent domain must be ProvenEmpty, got {other:?}"),
        }
    }

    // A payload path that is not user-manager shaped cannot yield safe-ci.slice.
    // That must degrade to Unproven -- NOT to ProvenEmpty, which would be
    // "proof" that skipped a whole domain.
    #[test]
    fn an_underivable_migration_target_is_unproven_not_empty() {
        match census_recorded_domain(Some("/system.slice/ci-hub-absent.scope")) {
            DomainEvidence::Unproven(why) => assert!(
                why.contains("safe-ci.slice"),
                "the reason must name the uncensused domain, got: {why}"
            ),
            other => panic!("an underivable migration target must be Unproven, got {other:?}"),
        }
    }

    // The async-signal-safe recorder in isolation: the FIRST signal latches and
    // later ones must not overwrite it. That CAS is the whole handler — if a
    // second signal could clobber the first, the reported cause would be
    // whichever signal happened to arrive last.
    #[test]
    fn record_termination_signal_latches_only_the_first_signal() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        SUPERVISOR_TERMINATION_SIGNAL.store(0, Ordering::SeqCst);
        record_termination_signal(libc::SIGTERM);
        record_termination_signal(libc::SIGINT);
        let latched = SUPERVISOR_TERMINATION_SIGNAL.load(Ordering::SeqCst);
        // Restore before asserting: the static is process-global and a panic
        // here would make every later run() test take the Signalled branch.
        SUPERVISOR_TERMINATION_SIGNAL.store(0, Ordering::SeqCst);
        assert_eq!(
            latched,
            libc::SIGTERM,
            "the first signal must latch; a later one must not overwrite it"
        );
    }

    // POSITIVE CONTROL for `ChildOutcome::Signalled`. A supervisor that is
    // terminated mid-run must NOT die where it stands: it has to kill the
    // payload subtree, prove the domain empty, RELEASE the box, and report
    // 128+signal.
    //
    // This is the 2026-08-06 fleet wedge in miniature. With no handler the
    // supervisor died inside `supervise_child`, the cleanup record stayed at
    // `phase=published` (which `verify_cleanup_record` maps to Uncensused), and
    // every agent's validate was refused until it was censused by hand.
    //
    // The flag is pre-set rather than delivered as a real signal so the test
    // cannot race the kernel and never signals anything outside its own domain
    // (Invariant 15). The recorder that a real SIGTERM would run is covered
    // directly above.
    #[test]
    fn run_signalled_supervisor_censuses_releases_and_reports_128_plus_signal() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        // Lock order is domain-guard THEN env-lock. No other test takes both;
        // any future test that does must use this same order.
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let stub = write_stub("signalled-admit", "#!/bin/sh\necho OK\nexit 0\n");
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);

        let paths = temp_paths("signalled-supervisor");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let identity_path = root.join("payload.identity");
        // Ignores TERM so the identity write cannot be raced by the terminate,
        // and so emptying the domain REQUIRES the supervisor's SIGKILL
        // escalation rather than the payload politely exiting on its own.
        let script = r#"
trap '' TERM
printf '%s %s\n' "$$" "$(awk '{print $22}' "/proc/$$/stat")" > "$1"
while :; do sleep 60; done
"#;

        // Record the signal MID-RUN, once the payload has published its
        // identity. Pre-setting the flag instead would terminate the payload
        // before bash could install its trap or write the file, and it would
        // also be less faithful: the wedge happened to a run already in flight.
        // The bounded fallback keeps a payload that never publishes from
        // hanging the suite.
        let publish_probe = identity_path.clone();
        let flagger = thread::spawn(move || {
            for _ in 0..600 {
                if fs::metadata(&publish_probe)
                    .map(|m| m.len() > 0)
                    .unwrap_or(false)
                {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            SUPERVISOR_TERMINATION_SIGNAL.store(libc::SIGTERM, Ordering::SeqCst);
        });

        let result = lock.run(
            RunArgs {
                agent: "signalled-validate".into(),
                kind: Kind::Validate,
                target: HEX_SHA.into(),
                no_wait: false,
                wait: 0,
                hold: 30,
                // Deliberately far away: if this run ends, it must be because of
                // the signal, never because the deadline fired.
                child_deadline: 600,
                max: 1,
                skip_base_check: false,
                child: vec![
                    OsString::from("/bin/bash"),
                    OsString::from("-c"),
                    OsString::from(script),
                    OsString::from("signalled-payload"),
                    identity_path.clone().into_os_string(),
                ],
            },
            &root,
        );
        let _ = flagger.join();
        SUPERVISOR_TERMINATION_SIGNAL.store(0, Ordering::SeqCst);
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let _ = fs::remove_file(&stub);

        // Ok at all is already load-bearing: the alternative return is
        // CleanupQuarantined, which is the wedge.
        let code = result.expect("a signalled supervisor must still complete its cleanup");
        assert_eq!(
            code,
            128 + libc::SIGTERM,
            "a signalled run must report 128+signal"
        );
        assert_ne!(
            code, CHILD_DEADLINE_EXIT_CODE,
            "a signal must not be reported as a child-deadline breach"
        );
        assert!(
            lock.read_holder().unwrap().is_none(),
            "the box must be FREE after a signalled run, not left quarantined"
        );

        // Process-domain cleanup, by EXACT identity rather than bare pid.
        let identity = fs::read_to_string(&identity_path).unwrap();
        let fields: Vec<_> = identity.split_whitespace().collect();
        assert_eq!(fields.len(), 2, "unexpected identity record {identity:?}");
        let pid: u32 = fields[0].parse().unwrap();
        let start_ticks: u64 = fields[1].parse().unwrap();
        assert!(
            !matches!(process_start_ticks(pid), Ok(current) if current == start_ticks),
            "payload {pid}@{start_ticks} survived a signalled run"
        );
        assert!(!process_group_exists(pid));
        let _ = fs::remove_dir_all(&root);
    }

    // NEGATIVE CONTROL for the same arm. Identical harness with the termination
    // flag CLEAR must take the ordinary Exited path and return the child's own
    // status. Without this, a Signalled arm that fired unconditionally would
    // still satisfy the positive test above.
    #[test]
    fn run_without_a_termination_signal_reports_the_child_exit_code() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let stub = write_stub("unsignalled-admit", "#!/bin/sh\necho OK\nexit 0\n");
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);

        let paths = temp_paths("unsignalled-supervisor");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let lock = ValidateLock {
            paths: paths.clone(),
        };

        SUPERVISOR_TERMINATION_SIGNAL.store(0, Ordering::SeqCst);
        let result = lock.run(
            RunArgs {
                agent: "unsignalled-validate".into(),
                kind: Kind::Validate,
                target: HEX_SHA.into(),
                no_wait: false,
                wait: 0,
                hold: 30,
                child_deadline: 600,
                max: 1,
                skip_base_check: false,
                child: vec![
                    OsString::from("/bin/sh"),
                    OsString::from("-c"),
                    OsString::from("exit 7"),
                ],
            },
            &root,
        );
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let _ = fs::remove_file(&stub);

        let code = result.expect("an ordinary child exit must complete cleanup");
        assert_eq!(
            code, 7,
            "an unsignalled run must report the child's own code"
        );
        assert_ne!(
            code,
            128 + libc::SIGTERM,
            "the Signalled arm must not fire when no signal was recorded"
        );
        assert!(lock.read_holder().unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn escaped_payload_quarantine_refuses_consumers_until_exact_recovery() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        let paths = temp_paths("escaped-quarantine");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let identity_path = paths.lock.parent().unwrap().join("escaped.identity");
        let script = r#"
trap 'exit 0' TERM
setsid /bin/bash -c '
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
            .run(
                RunArgs {
                    agent: "escaped-validate".into(),
                    kind: Kind::Bench,
                    target: "sha-escaped".into(),
                    no_wait: false,
                    wait: 0,
                    hold: 30,
                    child_deadline: 1,
                    max: 1,
                    skip_base_check: false,
                    child: vec![
                        OsString::from("/bin/bash"),
                        OsString::from("-c"),
                        OsString::from(script),
                        OsString::from("escaped-leader"),
                        identity_path.clone().into_os_string(),
                    ],
                },
                paths.lock.parent().unwrap(),
            )
            .unwrap_err();
        assert!(matches!(
            run_error,
            ValidateLockError::CleanupQuarantined(_)
        ));

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

        assert!(matches!(
            lock.acquire("replacement-validate", Kind::Validate, "sha-next", 0, 30),
            Err(ValidateLockError::CleanupQuarantined(_))
        ));
        assert!(matches!(
            lock.renew("escaped-validate", 30, false),
            Err(ValidateLockError::CleanupQuarantined(_))
        ));
        assert!(matches!(
            lock.release("escaped-validate", false),
            Err(ValidateLockError::CleanupQuarantined(_))
        ));
        let mut owner = lock.read_process_owner().unwrap().unwrap();
        owner.pid = u32::MAX;
        write_truncated(&paths.owner, owner.render().as_bytes()).unwrap();
        assert!(matches!(
            lock.acquire("replacement-validate", Kind::Validate, "sha-next", 0, 30),
            Err(ValidateLockError::CleanupQuarantined(_))
        ));
        assert!(matches!(
            lock.reclaim_dead(),
            Err(ValidateLockError::ReclaimNotProven(_))
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
        assert!(matches!(
            lock.acquire("replacement-validate", Kind::Validate, "sha-next", 0, 30),
            Err(ValidateLockError::CleanupQuarantined(_))
        ));

        assert_eq!(lock.reclaim_dead().unwrap(), 0);
        assert!(!paths.cleanup.exists());
        assert_eq!(
            lock.acquire("replacement-validate", Kind::Validate, "sha-next", 0, 30)
                .unwrap(),
            0
        );
        lock.release("replacement-validate", false).unwrap();
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn hard_death_run_subprocess_entry() {
        let Some(root) = env::var_os(HARD_DEATH_ROOT_ENV).map(PathBuf::from) else {
            return;
        };
        let point = match env::var(HARD_DEATH_POINT_ENV).as_deref() {
            Ok("after-arm") => crate::landing_lock::RunCrashPoint::AfterArm,
            Ok("after-spawn") => crate::landing_lock::RunCrashPoint::AfterSpawn,
            other => panic!("invalid hard-death point {other:?}"),
        };
        let paths = paths_at(&root);
        let ready = root.join("supervisor-ready");
        let marker = root.join("payload-started");
        let target = "hard-death";
        crate::landing_lock::install_run_hard_death_hook(&format!("bench:{target}"), point, ready);
        let result = ValidateLock { paths }.run(
            RunArgs {
                agent: "hard-death-validate".into(),
                kind: Kind::Bench,
                target: target.into(),
                no_wait: false,
                wait: 0,
                hold: 30,
                child_deadline: 30,
                max: 1,
                skip_base_check: true,
                child: vec![
                    OsString::from("/bin/sh"),
                    OsString::from("-c"),
                    OsString::from("printf started > \"$1\"; while :; do sleep 60; done"),
                    OsString::from("payload"),
                    marker.into_os_string(),
                ],
            },
            &root,
        );
        panic!("hard-death subprocess resumed instead of being SIGKILLed: {result:?}");
    }

    #[test]
    fn hard_death_before_publication_retains_armed_barrier() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        for point_name in ["after-arm", "after-spawn"] {
            let paths = temp_paths(&format!("hard-death-{point_name}"));
            let root = paths.lock.parent().unwrap().to_path_buf();
            let ready = root.join("supervisor-ready");
            let marker = root.join("payload-started");
            let mut supervisor = Command::new(env::current_exe().unwrap())
                .arg("--exact")
                .arg("validate_lock::tests::hard_death_run_subprocess_entry")
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

            let lock = ValidateLock {
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

            assert!(matches!(
                lock.acquire("replacement-validate", Kind::Validate, "replacement", 0, 30),
                Err(ValidateLockError::CleanupQuarantined(_))
            ));
            assert!(matches!(
                lock.reclaim_dead(),
                Err(ValidateLockError::ReclaimNotProven(_))
            ));
            let _ = fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn crash_windows_retain_armed_or_uncensused_barrier_after_owner_death() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        for (index, point) in [
            crate::landing_lock::RunCrashPoint::AfterArm,
            crate::landing_lock::RunCrashPoint::AfterSpawn,
            crate::landing_lock::RunCrashPoint::AfterPublish,
        ]
        .into_iter()
        .enumerate()
        {
            let paths = temp_paths(&format!("crash-window-{index}"));
            let lock = ValidateLock {
                paths: paths.clone(),
            };
            let marker = paths.lock.parent().unwrap().join("payload-started");
            let target = format!("crash-{index}");
            crate::landing_lock::install_run_crash_hook(&format!("bench:{target}"), point);
            let error = lock
                .run(
                    RunArgs {
                        agent: "crash-validate".into(),
                        kind: Kind::Bench,
                        target: target.clone(),
                        no_wait: false,
                        wait: 0,
                        hold: 30,
                        child_deadline: 30,
                        max: 1,
                        skip_base_check: true,
                        child: vec![
                            OsString::from("/bin/sh"),
                            OsString::from("-c"),
                            OsString::from("printf started > \"$1\"; while :; do sleep 60; done"),
                            OsString::from("payload"),
                            marker.clone().into_os_string(),
                        ],
                    },
                    paths.lock.parent().unwrap(),
                )
                .unwrap_err();
            assert!(matches!(error, ValidateLockError::CleanupQuarantined(_)));

            let record =
                CleanupRecord::parse(&fs::read_to_string(&paths.cleanup).unwrap()).unwrap();
            let published = match (&point, &record.phase) {
                (
                    crate::landing_lock::RunCrashPoint::AfterArm
                    | crate::landing_lock::RunCrashPoint::AfterSpawn,
                    CleanupPhase::Armed,
                ) => None,
                (
                    crate::landing_lock::RunCrashPoint::AfterPublish,
                    CleanupPhase::Published { leader, pgid },
                ) => Some((leader.clone(), *pgid)),
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
            assert!(matches!(
                lock.acquire("replacement-validate", Kind::Validate, "replacement", 0, 30),
                Err(ValidateLockError::CleanupQuarantined(_))
            ));
            assert!(matches!(
                lock.renew("crash-validate", 30, false),
                Err(ValidateLockError::CleanupQuarantined(_))
            ));
            assert!(matches!(
                lock.release("crash-validate", false),
                Err(ValidateLockError::CleanupQuarantined(_))
            ));
            assert!(matches!(
                lock.reclaim_dead(),
                Err(ValidateLockError::ReclaimNotProven(_))
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
                    Err(ValidateLockError::ReclaimNotProven(_))
                ));
            }
            let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
        }
    }

    #[test]
    fn normal_leader_exit_cannot_clear_an_escaped_payload_barrier() {
        let _domain_guard = crate::landing_lock::process_domain_test_guard();
        let paths = temp_paths("normal-exit-escaped");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let identity_path = paths.lock.parent().unwrap().join("escaped.identity");
        let script = r#"
setsid /bin/sh -c '
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
            .run(
                RunArgs {
                    agent: "normal-exit-validate".into(),
                    kind: Kind::Bench,
                    target: "normal-exit".into(),
                    no_wait: false,
                    wait: 0,
                    hold: 30,
                    child_deadline: 30,
                    max: 1,
                    skip_base_check: true,
                    child: vec![
                        OsString::from("/bin/sh"),
                        OsString::from("-c"),
                        OsString::from(script),
                        OsString::from("normal-leader"),
                        identity_path.clone().into_os_string(),
                    ],
                },
                paths.lock.parent().unwrap(),
            )
            .unwrap_err();
        assert!(matches!(error, ValidateLockError::CleanupQuarantined(_)));
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

    // 5. --max != 1 is rejected before acquiring; the box stays FREE.
    #[test]
    fn max_greater_than_one_rejected() {
        let paths = temp_paths("bad-max");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let error = lock
            .run(
                RunArgs {
                    agent: "greedy".into(),
                    kind: Kind::Bench,
                    target: "sha-greedy".into(),
                    no_wait: false,
                    wait: 0,
                    hold: 30,
                    child_deadline: 30,
                    max: 2,
                    skip_base_check: false,
                    child: vec![OsString::from("/bin/true")],
                },
                paths.lock.parent().unwrap(),
            )
            .unwrap_err();
        assert!(matches!(error, ValidateLockError::BadMax));
        assert!(lock.read_holder().unwrap().is_none());
        // The Display carries the exact unproven-cap rationale.
        assert!(error
            .to_string()
            .contains("box-exclusive cap >1 is unproven"));
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    // 6. A proven-dead owner is reclaimed before lease expiry.
    #[test]
    fn dead_owner_reclaimed() {
        let paths = temp_paths("dead-owner");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        assert_eq!(
            lock.acquire("dead-validate", Kind::Validate, "sha-dead", 0, 3_600)
                .unwrap(),
            0
        );
        // Plant a process-owner sidecar whose pid cannot exist -> proven dead.
        let mut owner = current_process_owner().unwrap();
        owner.pid = u32::MAX;
        write_truncated(&paths.owner, owner.render().as_bytes()).unwrap();
        assert!(matches!(
            lock.owner_liveness().unwrap(),
            OwnerLiveness::Dead(_)
        ));
        // A replacement acquires despite the still-live lease, recording provenance.
        assert_eq!(
            lock.acquire("replacement-validate", Kind::Validate, "sha-new", 0, 60)
                .unwrap(),
            0
        );
        let holder = lock.read_holder().unwrap().unwrap();
        assert_eq!(holder.agent, "replacement-validate");
        assert!(holder.reclaimed_from.unwrap().contains("dead owner"));
        lock.release("replacement-validate", false).unwrap();
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    // --- composite admission gate (mechanical REBASE-BEFORE-VALIDATE) ---
    // These serialize on ENV_LOCK because they mutate a process-global env var.
    // Bench-kind tests above never read the env, so they are unaffected.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn write_stub(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = env::temp_dir().join(format!(
            "ci-hub-preflight-stub-{name}-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::write(&path, body).unwrap();
        let mut perm = fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&path, perm).unwrap();
        path
    }

    const HEX_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    // --- the two overrides are TEST-BUILD-ONLY ---
    //
    // These run in a `cfg(test)` binary, so they see the override arm of
    // `test_only_override`. They are the sanctioned mechanism that replaced
    // poking a production binary with an ambient env var: the capability is
    // retained, but only where `cfg(test)` holds.
    //
    // The complementary half — that a PRODUCTION build ignores both — cannot be
    // asserted from inside this crate, because a `cfg(test)` build is by
    // definition not that build. It is bracketed out-of-process against the real
    // `ci-hub` binary; see the task evidence for
    // `remove-production-admission-overrides-from-validate-lock`.

    #[test]
    fn preflight_override_is_honored_only_in_a_test_build() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let stub = write_stub("cfg-gate", "#!/bin/sh\necho OK\nexit 0\n");
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);
        let observed = test_only_override(ADMIT_PREFLIGHT_CMD_ENV);
        // `/nonexistent` has no preflight_validate.py, so an admitted verdict
        // proves the STUB ran, not the default authority.
        let res = base_admission_check(Path::new("/nonexistent"), HEX_SHA, Kind::Validate, false);
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let after = test_only_override(ADMIT_PREFLIGHT_CMD_ENV);
        let _ = fs::remove_file(&stub);
        assert_eq!(observed.as_deref(), Some(stub.as_os_str()));
        assert!(
            res.is_ok(),
            "the test-build override must fire, got {res:?}"
        );
        assert!(after.is_none(), "an unset override must read as absent");
    }

    #[test]
    fn lock_path_override_is_honored_only_in_a_test_build() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = env::temp_dir().join(format!(
            "ci-hub-vlock-cfg-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        let alt = root.join("alt-validate-lock");
        // Default: derived from the workspace root, never from the environment.
        env::remove_var(VALIDATE_LOCK_PATH_ENV);
        let default_paths = LockPaths::for_workspace(&root);
        assert_eq!(default_paths.lock, root.join(".validate-lock"));
        assert_eq!(default_paths.guard, root.join(".validate-lock.guard"));
        // Override arm: present only because this is a `cfg(test)` build.
        env::set_var(VALIDATE_LOCK_PATH_ENV, &alt);
        let overridden = LockPaths::for_workspace(&root);
        env::remove_var(VALIDATE_LOCK_PATH_ENV);
        assert_eq!(overridden.lock, alt);
        assert_eq!(overridden.queue, suffix(&alt, ".queue"));
        // An empty value is not an override -- it would otherwise select "" and
        // silently relocate every lock file to the process working directory.
        env::set_var(VALIDATE_LOCK_PATH_ENV, "");
        let empty = LockPaths::for_workspace(&root);
        env::remove_var(VALIDATE_LOCK_PATH_ENV);
        assert_eq!(empty.lock, root.join(".validate-lock"));
    }

    // --- box-global exclusion: one lease per REPOSITORY, one anchor per BOX ---

    fn git(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn every_worktree_of_one_repository_shares_one_lease_file() {
        // THE DEFECT, at unit scale. `workspace_root()` hands `for_workspace` the
        // git toplevel of the running ci-hub.rs, so before this each linked
        // worktree derived a private `.validate-lock` and admitted independently.
        let base = env::temp_dir().join(format!(
            "ci-hub-vlock-repo-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        let main = base.join("main");
        let linked = base.join("linked");
        fs::create_dir_all(&main).unwrap();
        if !git(&main, &["init", "--quiet"]) {
            return; // no usable git on this host; the live bracket covers it
        }
        let _ = git(&main, &["config", "user.email", "t@example.invalid"]);
        let _ = git(&main, &["config", "user.name", "t"]);
        fs::write(main.join("seed"), b"seed").unwrap();
        assert!(git(&main, &["add", "seed"]));
        assert!(git(&main, &["commit", "--quiet", "-m", "seed"]));
        assert!(git(
            &main,
            &[
                "worktree",
                "add",
                "--detach",
                "--quiet",
                linked.to_str().unwrap()
            ]
        ));

        let from_main = LockPaths::for_workspace(&main);
        let from_linked = LockPaths::for_workspace(&linked);
        assert_eq!(
            from_linked.lock,
            main.join(".validate-lock"),
            "a linked worktree must resolve to the main worktree's lease"
        );
        assert_eq!(from_main.lock, from_linked.lock);
        assert_eq!(from_main.guard, from_linked.guard);
        assert_eq!(from_main.cleanup, from_linked.cleanup);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_non_repository_root_falls_back_to_itself() {
        // Fail-SAFE direction: without a resolvable repository identity we keep the
        // previous behaviour rather than pointing a live lease somewhere invented.
        let root = env::temp_dir().join(format!(
            "ci-hub-vlock-norepo-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        assert_eq!(repository_lock_root(&root), root);
        assert_eq!(
            LockPaths::for_workspace(&root).lock,
            root.join(".validate-lock")
        );
        let _ = fs::remove_dir_all(&root);
    }

    // No ENV_LOCK and no environment: the anchor path is a PARAMETER, so this test
    // is isolated by construction and can never touch the live box anchor.
    #[test]
    fn box_anchor_admits_one_and_refuses_the_second() {
        let dir = env::temp_dir().join(format!(
            "ci-hub-vlock-anchor-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let anchor = dir.join("validate-box.lock");

        // Nothing held: the FIRST caller is admitted (the mechanism is not inert).
        assert!(!BoxExclusionAnchor::held_by_another(&anchor));
        let first = BoxExclusionAnchor::take(&anchor, 0).unwrap();
        assert!(first.is_some(), "an unheld anchor must be granted");

        // A SECOND caller -- which in production is a different repository with its
        // own lease file -- is refused, and `acquire`'s non-holding probe sees it.
        let second = BoxExclusionAnchor::take(&anchor, 0).unwrap();
        assert!(
            second.is_none(),
            "a held anchor must refuse the second caller"
        );
        assert!(BoxExclusionAnchor::held_by_another(&anchor));
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
        // holding the description. Measured: asserting release in the same
        // instruction failed 2 of 3 suite runs against a suite that spawns
        // children constantly. Production is unaffected -- the supervisor holds the
        // anchor across its whole run, so the fork window is inside the hold, never
        // after it -- but the test must not assert a stronger timing property than
        // the mechanism has.
        drop(first);
        let reacquired = BoxExclusionAnchor::take(&anchor, 5).unwrap();
        assert!(
            reacquired.is_some(),
            "dropping the holder must release the box within the fork/exec window"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn box_anchor_path_is_uid_derived_and_not_workspace_derived() {
        let path = box_exclusion_anchor_at(VALIDATE_BOX_ANCHOR);
        let uid = unsafe { libc::getuid() };
        assert!(
            path.is_absolute(),
            "the anchor must not be relative to a cwd"
        );
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("validate-box.lock")
        );
        let text = path.to_string_lossy().into_owned();
        assert!(
            text.contains(&uid.to_string()),
            "the anchor must be keyed on the uid, got {text}"
        );
        // Stable across calls, and never a function of the workspace.
        assert_eq!(box_exclusion_anchor_at(VALIDATE_BOX_ANCHOR), path);
        // The PRODUCTION constructor wires that uid-derived path in; only the
        // test constructor (`paths_at`) substitutes a per-test one.
        let workspace = env::temp_dir().join("ci-hub-vlock-anchor-prod-probe");
        assert_eq!(LockPaths::for_workspace(&workspace).anchor, path);
    }

    // NEGATIVE direction: a floor-blocked head is REFUSED, and the refusal
    // carries the NAMED remedy (not a bare "refused"). This is the mutation that
    // proves the gate fires — swap the stub to exit 0 and it must stop refusing.
    #[test]
    fn base_admission_refuses_fixed_floor_and_names_remedy() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let stub = write_stub(
            "refuse",
            "#!/bin/sh\necho 'REFUSE: head 01234567 predates merge-gate floor \
             c369be3f; Rebase onto the named floor (>= c369be3f) before \
             validating/landing.'\nexit 2\n",
        );
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);
        let res = base_admission_check(Path::new("/nonexistent"), HEX_SHA, Kind::Validate, false);
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let _ = fs::remove_file(&stub);
        match res {
            Err(ValidateLockError::StaleBase(m)) => assert!(
                m.contains("Rebase onto"),
                "refusal must NAME the remedy, got: {m}"
            ),
            other => panic!("expected StaleBase refusal, got {other:?}"),
        }
    }

    // POSITIVE direction: a floor-containing head is GRANTED. Required so the gate is not
    // one that "refuses everything" — such a gate gets disabled.
    #[test]
    fn base_admission_admits_floor_containing_head() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let stub = write_stub("ok", "#!/bin/sh\necho OK\nexit 0\n");
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);
        let res = base_admission_check(Path::new("/nonexistent"), HEX_SHA, Kind::Validate, false);
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let _ = fs::remove_file(&stub);
        assert!(
            res.is_ok(),
            "a floor-containing head must be GRANTED, got {res:?}"
        );
    }

    // FAIL-CLOSED: an ERROR (unresolvable base) is treated as stale, never waved
    // through — an admission gate that admits on "couldn't check" is advisory.
    #[test]
    fn base_admission_fails_closed_on_error() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let stub = write_stub("err", "#!/bin/sh\necho boom >&2\nexit 3\n");
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);
        let res = base_admission_check(Path::new("/nonexistent"), HEX_SHA, Kind::Validate, false);
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let _ = fs::remove_file(&stub);
        match res {
            Err(ValidateLockError::StaleBase(m)) => assert!(
                m.contains("STALE") || m.contains("Rebase"),
                "ERROR must fail closed to a stale-base refusal, got: {m}"
            ),
            other => panic!("ERROR must fail closed to StaleBase, got {other:?}"),
        }
    }

    // PASS-THROUGH: bench work is not landing evidence and does not invoke the
    // predicate (the stub would REFUSE if it ran).
    #[test]
    fn base_admission_passes_through_bench() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let stub = write_stub(
            "pass",
            "#!/bin/sh\necho 'REFUSE: would block if invoked'\nexit 2\n",
        );
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);
        let r = Path::new("/nonexistent");
        assert!(
            base_admission_check(r, HEX_SHA, Kind::Bench, false).is_ok(),
            "bench is never floor-gated"
        );
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let _ = fs::remove_file(&stub);
    }

    // BYPASS NEGATIVES: neither an ambiguous target nor the former skip flag can
    // mint qualifying evidence. Both refuse before invoking the predicate.
    #[test]
    fn base_admission_refuses_nonhex_and_skip_bypass() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let r = Path::new("/nonexistent");
        for result in [
            base_admission_check(r, "sha-3", Kind::Validate, false),
            base_admission_check(r, HEX_SHA, Kind::Validate, true),
        ] {
            assert!(
                matches!(result, Err(ValidateLockError::StaleBase(_))),
                "a validate admission bypass must be refused, got {result:?}"
            );
        }
    }

    // END-TO-END through run(): a fixed-floor violation is refused with
    // STALE_BASE_EXIT_CODE
    // and NEVER consumes the box — a doomed ~17-min validate never starts.
    #[test]
    fn run_refuses_fixed_floor_without_consuming_box() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let paths = temp_paths("run-stale-base");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let stub = write_stub(
            "run-refuse",
            "#!/bin/sh\necho 'REFUSE: predates immutable validation floor; \
             rebase onto the named floor before validating.'\nexit 2\n",
        );
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);
        let code = lock
            .run(
                RunArgs {
                    agent: "stale-runner".into(),
                    kind: Kind::Validate,
                    target: HEX_SHA.into(),
                    no_wait: false,
                    wait: 0,
                    hold: 30,
                    child_deadline: 30,
                    max: 1,
                    skip_base_check: false,
                    // If the gate DIDN'T fire, this child would run and exit 0.
                    child: vec![OsString::from("/bin/true")],
                },
                paths.lock.parent().unwrap(),
            )
            .unwrap();
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        assert_eq!(
            code, STALE_BASE_EXIT_CODE,
            "a fixed-floor violation must refuse (not run the child to a 0 exit)"
        );
        assert!(
            lock.read_holder().unwrap().is_none(),
            "a refused validate must not have held the box"
        );
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
        let _ = fs::remove_file(&stub);
    }
}
