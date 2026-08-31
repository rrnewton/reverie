//! Validation/benchmark admission controller: at most SIX validation jobs run
//! on this box at a time, while a benchmark takes every validation slot.
//!
//! Concurrent heavy load manufactures false test FAILEDs (detcore_misc residual
//! is monotonic in load; see `experiments/multisect_detcore_misc_20260803`), so a
//! seventh validation or a benchmark overlapping any validation is never silently
//! admitted. This is a SEPARATE lock
//! from the landing mutex (`landing_lock.rs`): a validate must never block a
//! lander and vice versa, so they key on different lockfiles. The lease engine
//! (guard flock + FIFO queue + evidence-based dead-owner reclaim + bounded
//! child-deadline heartbeat) mirrors `landing_lock.rs` exactly on its own files;
//! pure, stateless helpers are reused from that module rather than re-derived.

use crate::bounded_output::BoundedOutput;
use crate::lease::{
    self, DomainState as LeaseDomainState, HandoffState, LeaseDecision, LeaseRecord, LeaseState,
    OwnerLiveness as LeaseOwnerLiveness,
};
use crate::validate_run_handle;
use chrono::{Local, TimeZone};
use clap::{Args, Subcommand, ValueEnum};
use fs2::FileExt;
use serde::Serialize;
#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::VecDeque;
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
#[cfg(test)]
use crate::landing_lock::census_recorded_domain;
use crate::landing_lock::{
    box_exclusion_anchor_path as box_exclusion_anchor_at, capture_and_freeze_residuals,
    census_cleanup_domain, census_disposition, current_host, enable_child_subreaper,
    exact_process_liveness, exit_status_code, foreign_box_claim, foreign_box_quarantine,
    heartbeat_test_helper_delay, print_census_recovery, print_cleanup_record, process_group_exists,
    process_start_ticks, reap_exited_children, reap_while_supervising, register_box_claim,
    remove_cleanup_record_and_claim, repository_lock_root, signal_group, spawn_gated_child, suffix,
    verify_cleanup_record, write_cleanup_record, BoxAnchorObservation, BoxExclusionAnchor,
    CleanupDomain, CleanupPhase, CleanupRecord, CleanupVerification, DomainAttestation,
    DomainEvidence, GatedChild, ProcessIdentity, ResidualCapture,
};

const DEFAULT_WAIT_SECONDS: u64 = 1_800;
const DEFAULT_HOLD_SECONDS: u64 = 900;
const POLL_SECONDS: u64 = 3;
const GUARD_WAIT_SECONDS: u64 = 30;
const GLOBAL_QUEUE_DECLARATION_TTL_SECONDS: i64 = 86_400;
/// Six validation slots are available. Benchmarks still take every slot and
/// therefore remain box-exclusive against every validation and benchmark.
pub(crate) const VALIDATE_SLOT_COUNT: usize = 6;
const VALIDATE_CONCURRENT_CAP: u64 = VALIDATE_SLOT_COUNT as u64;
/// A measured warm full-profile validate reached 1.98 GiB RSS. Round upward to
/// a 2 GiB admission reservation; this is not a cgroup memory.max.
const VALIDATE_MEMORY_RESERVATION_BYTES: u64 = 2_147_483_648;
/// The measured pre-knee envelope is six live validates at 2 GiB each. Current
/// MemAvailable may reduce this budget, but can never enlarge it.
const VALIDATE_MEMORY_BUDGET_BYTES: u64 =
    VALIDATE_CONCURRENT_CAP * VALIDATE_MEMORY_RESERVATION_BYTES;
const LEGACY_ACQUIRE_CAP: u64 = 1;
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
/// A completed child can leave a transient systemd/helper process in its
/// numeric process group after the subreaper census is already complete. Give
/// that *empty* census a short, bounded stabilization window. A live,
/// incomplete, or unreadable domain still quarantines; this is never a signal
/// or preemption path.
const EMPTY_DOMAIN_SETTLE_SECONDS: u64 = CHILD_TERM_GRACE_SECONDS;
const EMPTY_DOMAIN_SETTLE_POLL_MILLIS: u64 = 20;
/// Exit code reported when admission REFUSES a validate whose head predates an
/// immutable fixed floor or freshly fetched `origin/main`. Same 3 as an
/// ownership refusal: a refusal, not a crash.
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
/// The owner's periodic acceptance watch gets the first free admission boundary
/// after background work releases the box. This is a yield protocol, never a
/// preemptor: it neither signals nor reclaims a holder.
const OWNER_WATCH_AGENT: &str = "owner-checkout-watch";
/// A background acquirer may proceed after this bounded gap even when the watch
/// has not produced a durable admission result. Until then it yields without
/// entering the FIFO or taking the box-global anchor.
const OWNER_WATCH_YIELD_SECONDS: i64 = 90;
const OWNER_WATCH_STATE_RELATIVE: &str = ".local/state/hermit-owner-validate";
const OWNER_WATCH_VERIFIER_RELATIVE: &str = "scripts/owner-checkout-validate-watch.py";
/// Bound the only network operation performed at the free admission boundary.
/// A failed or timed-out fetch refuses freshness; it must not turn a stale-head
/// optimization into an unbounded box-global anchor hold.
const CURRENT_MAIN_FETCH_TIMEOUT_SECONDS: u64 = 30;
/// Admission-owned source-branch provenance forwarded to the ledger adapter.
/// The child must not inherit an ambient value when an older or detached run
/// handle carries no branch.
const VALIDATE_BRANCH_ENV: &str = "CI_HUB_VALIDATE_BRANCH";

fn apply_admission_provenance(
    command: &mut Command,
    run_number: Option<u64>,
    branch: Option<&str>,
) {
    if let Some(run_number) = run_number {
        command.env("CI_HUB_VALIDATE_RUN_NUMBER", run_number.to_string());
    }
    command.env_remove(VALIDATE_BRANCH_ENV);
    if let Some(branch) = branch {
        command.env(VALIDATE_BRANCH_ENV, branch);
    }
}

/// How often a QUEUED waiter re-tests that its target still contains
/// `origin/main`.
///
/// ⚠️ NOT `POLL_SECONDS`. The freshness check performs a real `git fetch`, so
/// testing it on the 3-second queue poll would issue ~700 fetches across one
/// 35-minute wait. Measured 2026-08-26: main merged four times in 72 minutes
/// (11:51, 12:36, 12:47, 13:03), a ~24-minute interval, against a ~28-minute
/// run. At 60 seconds the slot is released within a minute of the merge that
/// doomed it, which is the whole benefit; a faster poll buys nothing because
/// the thing being detected moves on a timescale three orders of magnitude
/// slower.
const CURRENT_MAIN_RECHECK_SECONDS: u64 = 60;

/// Is this refusal the DEFINITE "your target is behind main", as opposed to
/// "I could not find out"?
///
/// ⚠️ THIS DISTINCTION IS THE DIFFERENCE BETWEEN AN EVICTION AND A BUG.
/// `current_main_freshness_check` returns `StaleBase` for FOUR conditions: the
/// fetch could not run, the fetch failed, the ancestry test was unresolved, and
/// the target genuinely does not contain main. Only the last is a property of
/// the TARGET; the other three are properties of the network or the moment. A
/// mid-wait eviction that fired on all four would throw a perfectly good waiter
/// out of the queue because a fetch timed out -- an over-eager eviction is
/// indistinguishable from a working one when you only look at the case that
/// should evict, which is why the control test exists.
///
/// UNRESOLVED deliberately keeps its position. Nothing is lost by waiting: the
/// front-of-queue check runs again with the anchor held and fails closed there,
/// where fail-closed costs one run rather than one slot.
fn stale_base_is_definite(error: &ValidateLockError) -> bool {
    match error {
        ValidateLockError::StaleBase(message) => {
            message.contains("does not contain freshly fetched origin/main")
        }
        _ => false,
    }
}

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
    /// Declare the priority for one future run. The declaration is inert until
    /// that exact agent/kind/target reaches the box-global queue.
    Prioritize(PrioritizeArgs),
    /// Retire one exact inactive declaration after its work reaches a terminal
    /// outcome. An active waiter is never retired by this command.
    RetirePriority(RetirePriorityArgs),
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
    /// Report whether a background acquirer must yield to the owner's watch.
    /// Exit 0=ALLOW, 1=YIELD, 2=UNKNOWN (fail closed).
    YieldStatus(YieldStatusArgs),
    /// Reclaim a lease only when its recorded validation supervisor process has exited.
    ReclaimDead,
    /// Standard fail-closed stale-clear path. `reclaim-dead` is retained as a
    /// compatibility alias; both call the same lease verifier.
    StaleClear,
    /// Census an UNCENSUSED payload domain post-hoc, after the supervisor died
    /// before it could take one, so `reclaim-dead` can finish.
    CensusOrphanedDomain(CensusOrphanedDomainArgs),
    /// Acquire and run with a heartbeat + child-deadline; release after complete
    /// cleanup proof, otherwise retain a quarantine.
    Run(RunArgs),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum QueuePriority {
    OwnerDirective,
    Expedited,
    Prepared,
    Routine,
}

impl QueuePriority {
    fn as_str(self) -> &'static str {
        match self {
            Self::OwnerDirective => "owner-directive",
            Self::Expedited => "expedited",
            Self::Prepared => "prepared",
            Self::Routine => "routine",
        }
    }

    fn parse(value: &str) -> Result<Self, ValidateLockError> {
        match value {
            "owner-directive" => Ok(Self::OwnerDirective),
            "expedited" => Ok(Self::Expedited),
            "prepared" => Ok(Self::Prepared),
            "routine" => Ok(Self::Routine),
            other => Err(ValidateLockError::InvalidState(format!(
                "unknown queue priority {other:?}"
            ))),
        }
    }

    fn rank(self, agent: &str) -> u8 {
        if agent == OWNER_WATCH_AGENT {
            0
        } else {
            match self {
                Self::OwnerDirective => 1,
                Self::Expedited => 2,
                Self::Prepared => 3,
                Self::Routine => 4,
            }
        }
    }
}

#[derive(Args, Clone, Debug)]
pub struct PrioritizeArgs {
    #[arg(long)]
    pub agent: String,
    #[arg(long, value_enum)]
    pub kind: Kind,
    #[arg(long)]
    pub target: String,
    #[arg(long, value_enum)]
    pub priority: QueuePriority,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum TerminalOutcome {
    Completed,
    Failed,
    Abandoned,
}

impl TerminalOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Args, Clone, Debug)]
pub struct RetirePriorityArgs {
    #[arg(long)]
    pub agent: String,
    #[arg(long, value_enum)]
    pub kind: Kind,
    #[arg(long)]
    pub target: String,
    /// Explicit terminal transition for this exact work identity. The queue
    /// cannot infer this uniformly: validate targets are SHAs, bench targets
    /// are arbitrary, and abandoned work has no shared receipt authority.
    #[arg(long, value_enum)]
    pub outcome: TerminalOutcome,
}

#[derive(Args, Clone, Debug)]
pub struct AuthorityStatusArgs {
    /// Emit the stable versioned JSON authority contract.
    #[arg(long, required = true)]
    pub json: bool,
}

#[derive(Args, Clone, Debug)]
pub struct YieldStatusArgs {
    /// Emit the stable versioned JSON yield-decision contract.
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
    ///
    /// Optional at the CLI so a mechanically provable census can be run exactly
    /// as `status` prints it, with no attestation flags at all; it is still
    /// enforced as non-empty on every path that actually consumes it.
    #[arg(long, default_value = "")]
    pub evidence: String,
}

/// A validation or benchmark job kind. Validation kinds share the validation
/// slots; a benchmark reserves all of them and therefore remains mutually
/// exclusive with every validation and benchmark.
#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum Kind {
    Validate,
    ReverieValidate,
    /// A validation run pinned to an EXACT sha that is deliberately NOT tracked
    /// to current `origin/main`.
    ///
    /// ⚠️ THIS EXISTS BECAUSE EVIDENCE-GATHERING AND LANDING WANT OPPOSITE
    /// THINGS FROM THE SAME BOX. A landing run must contain current main or its
    /// receipt cannot authorize a merge, so
    /// [`current_main_freshness_check`] evicts it the moment main advances --
    /// measured 2026-08-26, that eviction is what stops a doomed waiter holding
    /// a ~28-minute slot for 34m40s. An EVIDENCE run wants the opposite: freeze
    /// the sha, answer "was THIS tree green", and accept that the answer is
    /// about that sha and no other. With one box and main moving eight times a
    /// day, a run that must stay current can never complete, so no evidence
    /// about any tree could be gathered at all.
    ///
    /// ⚠️ IT SKIPS THE MOVING BOUNDARY, NOT THE PRODUCER FLOOR.
    /// [`Kind::is_validation`] returns TRUE for this kind, so
    /// [`base_admission_check`] still runs and a genuinely bad target -- a
    /// non-sha, a head below the immutable floors -- is refused exactly as it is
    /// for [`Kind::Validate`]. [`current_main_freshness_check`] then requires the
    /// opposite of the ordinary relationship: freshly fetched main must NOT be
    /// contained by the target. `Bench` skips both checks because it is not
    /// validation evidence; this kind changes only the moving-tip predicate.
    ///
    /// ⚠️ ITS RECEIPT IS NOT CANONICAL CURRENT-MAIN AUTHORITY. Authority status
    /// reports the fully checked holder as `lock_admissible`, but leaves
    /// `admissible` false, so a frozen receipt can never be read as evidence that
    /// main is green. That separation is the whole safety property: the run is
    /// admitted, and its answer is labelled for its own sha.
    FrozenValidate,
    Bench,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Validate => "validate",
            Kind::ReverieValidate => "reverie-validate",
            Kind::FrozenValidate => "frozen-validate",
            Kind::Bench => "bench",
        }
    }

    fn parse(value: &str) -> Result<Self, ValidateLockError> {
        match value {
            "validate" => Ok(Kind::Validate),
            "reverie-validate" => Ok(Kind::ReverieValidate),
            "frozen-validate" => Ok(Kind::FrozenValidate),
            "bench" => Ok(Kind::Bench),
            other => Err(ValidateLockError::InvalidState(format!(
                "unknown kind {other:?}, expected                  validate|reverie-validate|frozen-validate|bench"
            ))),
        }
    }

    /// Does this kind produce landing-grade evidence, and therefore have to
    /// clear the immutable producer floors?
    ///
    /// ⚠️ `FrozenValidate` IS INCLUDED ON PURPOSE. It skips the FRESHNESS
    /// boundary, not the floor. Excluding it here would make
    /// [`base_admission_check`] return early and a non-sha or below-floor target
    /// would be ADMITTED -- turning a narrow "accept a non-current sha" into
    /// "accept anything", which is the failure this split exists to avoid.
    fn is_validation(self) -> bool {
        matches!(
            self,
            Kind::Validate | Kind::ReverieValidate | Kind::FrozenValidate
        )
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
    #[arg(long, default_value_t = LEGACY_ACQUIRE_CAP)]
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
    /// Canonical ci-hub run record created before the systemd service starts.
    /// Production refuses before taking the box anchor unless this record binds
    /// the requested agent/target to the service unit that contains this process.
    #[arg(long)]
    pub run_record: Option<PathBuf>,
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
    /// Admission envelope. Supervised runs accept the exclusive value 1 or the
    /// measured value 6. Each live validate reserves 2 GiB from the 12 GiB
    /// validation budget, further bounded by current Linux MemAvailable.
    #[arg(long, default_value_t = VALIDATE_CONCURRENT_CAP)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ReleaseRecord {
    schema_version: u32,
    released_at: i64,
    released_human: String,
    released_by: String,
    kind: String,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    attested_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attestation_evidence: Option<String>,
}

impl ReleaseRecord {
    fn parse(content: &str) -> Result<Self, ValidateLockError> {
        let mut schema_version = None;
        let mut released_at = None;
        let mut released_human = None;
        let mut released_by = None;
        let mut kind = None;
        let mut target = None;
        let mut attested_by = None;
        let mut attestation_evidence = None;
        for (line_number, line) in content.lines().enumerate() {
            let (key, value) = line.split_once('=').ok_or_else(|| {
                ValidateLockError::InvalidState(format!(
                    "release line {} is not key=value",
                    line_number + 1
                ))
            })?;
            match key {
                "schema_version" => schema_version = Some(parse_integer(key, value)?),
                "released_at" => released_at = Some(parse_integer(key, value)?),
                "released_human" => released_human = Some(value.to_string()),
                "released_by" => released_by = Some(value.to_string()),
                "kind" => kind = Some(value.to_string()),
                "target" => target = Some(value.to_string()),
                "attested_by" => attested_by = Some(value.to_string()),
                "attestation_evidence" => attestation_evidence = Some(value.to_string()),
                unknown => {
                    return Err(ValidateLockError::InvalidState(format!(
                        "unknown release field {unknown:?}"
                    )));
                }
            }
        }
        let schema_version = required(schema_version, "release schema_version")?;
        let (attested_by, attestation_evidence) = match (
            schema_version,
            attested_by.as_deref(),
            attestation_evidence.as_deref(),
        ) {
            (1, None, None) => (None, None),
            (2, Some(attester), Some(evidence)) => {
                let attestation = DomainAttestation::new(attester, evidence)
                    .map_err(ValidateLockError::InvalidState)?;
                (Some(attestation.attester), Some(attestation.evidence))
            }
            (1, _, _) => {
                return Err(ValidateLockError::InvalidState(
                    "release schema_version=1 cannot carry an attestation".into(),
                ))
            }
            (2, _, _) => {
                return Err(ValidateLockError::InvalidState(
                    "release schema_version=2 requires attested_by and attestation_evidence".into(),
                ))
            }
            _ => {
                return Err(ValidateLockError::InvalidState(format!(
                    "unsupported release schema_version={schema_version}"
                )))
            }
        };
        Ok(Self {
            schema_version: schema_version as u32,
            released_at: required(released_at, "release released_at")?,
            released_human: required(released_human, "release released_human")?,
            released_by: required(released_by, "release released_by")?,
            kind: required(kind, "release kind")?,
            target: required(target, "release target")?,
            attested_by,
            attestation_evidence,
        })
    }

    fn render(&self) -> String {
        let mut output = format!(
            "schema_version={}\nreleased_at={}\nreleased_human={}\nreleased_by={}\nkind={}\ntarget={}\n",
            self.schema_version,
            self.released_at,
            self.released_human,
            self.released_by,
            self.kind,
            self.target
        );
        if let (Some(attested_by), Some(evidence)) = (&self.attested_by, &self.attestation_evidence)
        {
            output.push_str(&format!(
                "attested_by={attested_by}\nattestation_evidence={evidence}\n"
            ));
        }
        output
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct OwnerWatchAdmission {
    state: String,
    verifier_output: String,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum YieldState {
    Allow,
    Yield,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct YieldStatus {
    schema_version: u32,
    state: YieldState,
    admissible: bool,
    reason_code: String,
    detail: String,
    observed_at: i64,
    remaining_seconds: Option<i64>,
    release: Option<ReleaseRecord>,
    owner_watch_admission: Option<OwnerWatchAdmission>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct GlobalQueueEntry {
    enqueued_at: i64,
    priority: QueuePriority,
    agent: String,
    kind: Kind,
    target: String,
    owner: Option<ProcessOwner>,
}

impl GlobalQueueEntry {
    fn parse(line: &str, line_number: usize) -> Result<Self, ValidateLockError> {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 10 {
            return Err(ValidateLockError::InvalidState(format!(
                "global queue line {line_number} must have ten tab-separated fields"
            )));
        }
        let parse_u32 = |name: &str, value: &str| {
            let parsed = parse_integer(name, value)?;
            u32::try_from(parsed).map_err(|_| {
                ValidateLockError::InvalidState(format!("{name} is outside u32: {value:?}"))
            })
        };
        let parse_u64 = |name: &str, value: &str| {
            let parsed = parse_integer(name, value)?;
            u64::try_from(parsed).map_err(|_| {
                ValidateLockError::InvalidState(format!("{name} is negative: {value:?}"))
            })
        };
        let owner = match fields[5] {
            "declared" => {
                if fields[6..] != ["-", "-", "0", "0"] {
                    return Err(ValidateLockError::InvalidState(format!(
                        "global queue line {line_number} has identity fields on an inactive declaration"
                    )));
                }
                None
            }
            "active" => Some(ProcessOwner {
                host: fields[6].to_string(),
                boot_id: fields[7].to_string(),
                pid: parse_u32("global queue owner pid", fields[8])?,
                start_ticks: parse_u64("global queue owner start_ticks", fields[9])?,
            }),
            state => {
                return Err(ValidateLockError::InvalidState(format!(
                    "global queue line {line_number} has unknown state {state:?}"
                )))
            }
        };
        Ok(Self {
            enqueued_at: parse_integer("global queue timestamp", fields[0])?,
            priority: QueuePriority::parse(fields[1])?,
            agent: fields[2].to_string(),
            kind: Kind::parse(fields[3])?,
            target: fields[4].to_string(),
            owner,
        })
    }

    fn render(&self) -> String {
        let (state, host, boot_id, pid, start_ticks) = match &self.owner {
            Some(owner) => (
                "active",
                owner.host.as_str(),
                owner.boot_id.as_str(),
                owner.pid,
                owner.start_ticks,
            ),
            None => ("declared", "-", "-", 0, 0),
        };
        format!(
            "{}\t{}\t{}\t{}\t{}\t{state}\t{host}\t{boot_id}\t{pid}\t{start_ticks}\n",
            self.enqueued_at,
            self.priority.as_str(),
            self.agent,
            self.kind.as_str(),
            self.target,
        )
    }

    fn matches(&self, agent: &str, kind: Kind, target: &str) -> bool {
        self.agent == agent && self.kind == kind && self.target == target
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
    /// Whether the live holder is valid for executing the validation kind it
    /// acquired. This is deliberately separate from `admissible`: a frozen
    /// validation may run against its exact target, but it is never canonical
    /// current-main authority.
    lock_admissible: bool,
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
            lock_admissible: false,
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
            lock_admissible: true,
            admissible: true,
            state: AuthorityState::Held,
            reason_code: None,
            holder: Some(holder),
            owner: Some(owner),
            canonical_anchor_held: true,
            cleanup_state,
        }
    }

    fn admitted_noncanonical(
        reason_code: &str,
        holder: AuthorityHolder,
        owner: AuthorityOwner,
        cleanup_state: AuthorityCleanupState,
    ) -> Self {
        Self {
            schema_version: 1,
            lock_admissible: true,
            admissible: false,
            state: AuthorityState::Held,
            reason_code: Some(reason_code.to_string()),
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
    #[error("validate-lock: unsupported --max (bare acquire requires 1; supervised run requires 1 or the measured six-slot box policy)")]
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
    /// Durable record of the most recent transition from held to free. Unlike
    /// `.validate-lock.queue` mtime, this is an explicit admission contract.
    released: PathBuf,
    /// Public owner-watch admission verifier and its single durable cursor.
    /// validate-lock consumes this interface; it does not infer admission by
    /// re-parsing the watch's per-attempt result directory.
    owner_watch_verifier: Option<PathBuf>,
    owner_watch_state_dir: Option<PathBuf>,
    /// The box-global exclusion anchor (see [`BoxExclusionAnchor`]). Carried here
    /// rather than read from a global so it is CONSTRUCTOR-SCOPED: production
    /// builds it from the uid, and each test builds its own beside its own temp
    /// lease. That is deliberate — the first version of this used a `cfg(test)`
    /// env override and two tests raced it through the process-global
    /// environment, one of them taking the REAL box anchor and blocking on it.
    /// A test binary must never contend with the live box.
    anchor: PathBuf,
    global_anchor: PathBuf,
    /// Production reads Linux MemAvailable here. Tests use a private fixture so
    /// admission tests never depend on or mutate host-wide memory state.
    meminfo: PathBuf,
}

impl LockPaths {
    fn for_workspace(tool_root: &Path, state_root: &Path) -> Self {
        // The base is DISTINCT from `.landing-lock` so a validate never contends
        // with a lander. The path is NOT caller-selectable in a production build:
        // `test_only_override` returns None there (and says so), because an
        // ambient variable that redirects the box-exclusive lockfile escapes box
        // exclusivity outright (c6767e06).
        //
        // AND THE STATE ROOT IS CANONICALIZED TO THE REPOSITORY'S MAIN WORKTREE,
        // because one repository is not one checkout. EVERY linked worktree used
        // to derive its own private `.validate-lock` and admit independently. Measured
        // 2026-08-08 on this box: 46 worktrees of ~/work/dev-hermit, all 46 carrying
        // a runnable `ci-hub/ci-hub`, i.e. 46 mutually blind box-exclusive locks —
        // and two runs from two of them were admitted 2/2 concurrently. Only the
        // main worktree held any `.validate-lock*` state (5 files; the other 45 held
        // none), so collapsing them onto it orphans nothing.
        let lock = test_only_override(VALIDATE_LOCK_PATH_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| repository_lock_root(state_root).join(".validate-lock"));
        let mut paths = Self::at(lock);
        // Executable policy comes from the immutable tool root. Operational
        // locks and handles remain rooted in the mutable shared state checkout.
        paths.owner_watch_verifier = Some(tool_root.join(OWNER_WATCH_VERIFIER_RELATIVE));
        paths.owner_watch_state_dir =
            env::var_os("HOME").map(|home| PathBuf::from(home).join(OWNER_WATCH_STATE_RELATIVE));
        paths
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
        let global_anchor = box_exclusion_anchor_at(VALIDATE_BOX_ANCHOR);
        Self {
            guard: suffix(&lock, ".guard"),
            queue: suffix(&lock, ".queue"),
            owner: suffix(&lock, ".owner"),
            cleanup: suffix(&lock, ".cleanup-required"),
            released: suffix(&lock, ".released"),
            owner_watch_verifier: None,
            owner_watch_state_dir: None,
            anchor: global_anchor.clone(),
            global_anchor,
            meminfo: PathBuf::from("/proc/meminfo"),
            lock,
        }
    }

    fn for_slot(&self, slot: usize) -> Self {
        assert!(slot < VALIDATE_SLOT_COUNT);
        if slot == 0 {
            return self.clone();
        }
        let lock = suffix(&self.lock, &format!(".slot-{slot}"));
        Self {
            guard: suffix(&lock, ".guard"),
            queue: suffix(&lock, ".queue"),
            owner: suffix(&lock, ".owner"),
            cleanup: suffix(&lock, ".cleanup-required"),
            // The owner-watch boundary is global: either validate release must
            // give it the same opportunity to run.
            released: self.released.clone(),
            owner_watch_verifier: self.owner_watch_verifier.clone(),
            owner_watch_state_dir: self.owner_watch_state_dir.clone(),
            anchor: suffix(&self.global_anchor, &format!(".slot-{slot}")),
            global_anchor: self.global_anchor.clone(),
            meminfo: self.meminfo.clone(),
            lock,
        }
    }

    fn global_anchor(&self) -> PathBuf {
        self.global_anchor.clone()
    }

    fn slot_anchor(&self, slot: usize) -> PathBuf {
        assert!(slot < VALIDATE_SLOT_COUNT);
        if slot == 0 {
            self.global_anchor()
        } else {
            suffix(&self.global_anchor(), &format!(".slot-{slot}"))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VerifiedRunAttribution {
    unit: String,
    record: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GlobalValidateHolder {
    pid: u32,
    start_ticks: u64,
    boot_id: String,
    host: String,
    agent: String,
    kind: String,
    target: String,
    unit: String,
    record: PathBuf,
    cgroup: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum GlobalValidateObservation {
    Free,
    Held(GlobalValidateHolder),
    Unknown(String),
}

fn unique_anchor_field<'a>(body: &'a str, name: &str) -> Result<&'a str, String> {
    let mut values = body.split_whitespace().filter_map(|token| {
        let (key, value) = token.split_once('=')?;
        (key == name).then_some(value)
    });
    let value = values
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("held anchor has no nonempty {name}= field"))?;
    if values.next().is_some() {
        return Err(format!("held anchor has duplicate {name}= fields"));
    }
    Ok(value)
}

fn cgroup_contains_unit(cgroup: &str, unit: &str) -> bool {
    cgroup.lines().any(|line| {
        line.splitn(3, ':')
            .nth(2)
            .is_some_and(|path| path.split('/').any(|component| component == unit))
    })
}

fn verify_global_validate_holder(
    tool_root: &Path,
    body: &str,
) -> Result<GlobalValidateHolder, String> {
    if unique_anchor_field(body, "anchor")? != "box-exclusive-v1" {
        return Err("held anchor has an unsupported authority version".into());
    }
    let pid: u32 = unique_anchor_field(body, "pid")?
        .parse()
        .map_err(|_| "held anchor pid is not a u32".to_string())?;
    let start_ticks: u64 = unique_anchor_field(body, "start_ticks")?
        .parse()
        .map_err(|_| "held anchor start_ticks is not a u64".to_string())?;
    if pid == 0 || start_ticks == 0 {
        return Err("held anchor identity requires pid>0 and start_ticks>0".into());
    }
    let boot_id = unique_anchor_field(body, "boot_id")?.to_string();
    let host = unique_anchor_field(body, "host")?.to_string();
    let agent = unique_anchor_field(body, "agent")?.to_string();
    let kind = unique_anchor_field(body, "kind")?.to_string();
    let target = unique_anchor_field(body, "target")?.to_string();
    let unit = unique_anchor_field(body, "unit")?.to_string();
    let record = PathBuf::from(unique_anchor_field(body, "record")?);

    let current_host_name = current_host();
    if current_host_name == "unknown" || host != current_host_name {
        return Err(format!(
            "held anchor host {host} does not match observable host {current_host_name}"
        ));
    }
    let observed_boot = current_boot_id().map_err(|error| error.to_string())?;
    if boot_id != observed_boot {
        return Err(format!(
            "held anchor boot_id {boot_id} does not match observable boot {observed_boot}"
        ));
    }
    match process_start_ticks(pid) {
        Ok(observed) if observed == start_ticks => {}
        Ok(observed) => {
            return Err(format!(
                "held anchor pid {pid} start_ticks mismatch: recorded {start_ticks}, observed {observed}"
            ))
        }
        Err(error) => {
            return Err(format!(
                "held anchor pid {pid} liveness is unverifiable: {error}"
            ))
        }
    }

    let canonical_record = fs::canonicalize(&record).map_err(|error| {
        format!(
            "cannot canonicalize exact run record {}: {error}",
            record.display()
        )
    })?;
    if canonical_record != record {
        return Err(format!(
            "held anchor run record is not canonical: {} resolves to {}",
            record.display(),
            canonical_record.display()
        ));
    }
    let content = fs::read_to_string(&record)
        .map_err(|error| format!("cannot read exact run record {}: {error}", record.display()))?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|error| format!("exact run record {} is not JSON: {error}", record.display()))?;
    let schema = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    if validate_run_handle::is_current(&value) {
        validate_run_handle::verify_holder(
            tool_root,
            &record,
            &agent,
            &target,
            &kind,
            &unit,
            &validate_run_handle::ProcessIdentity {
                pid,
                start_ticks,
                boot_id: boot_id.clone(),
            },
        )?;
    } else {
        let field = |name: &str| value.get(name).and_then(serde_json::Value::as_str);
        if schema != Some(1)
            || field("agent") != Some(agent.as_str())
            || field("target") != Some(target.as_str())
            || field("unit") != Some(unit.as_str())
        {
            return Err(format!(
                "exact run record {} does not bind readable schema agent={agent} target={target} unit={unit}",
                record.display()
            ));
        }
        let identity = value
            .get("process_identity")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                format!(
                    "exact run record {} has no process_identity",
                    record.display()
                )
            })?;
        if identity.get("pid").and_then(serde_json::Value::as_u64) != Some(u64::from(pid))
            || identity
                .get("start_ticks")
                .and_then(serde_json::Value::as_u64)
                != Some(start_ticks)
            || identity.get("boot_id").and_then(serde_json::Value::as_str) != Some(boot_id.as_str())
        {
            return Err(format!(
                "exact run record {} carries a different process_identity",
                record.display()
            ));
        }
    }

    let cgroup_path = PathBuf::from(format!("/proc/{pid}/cgroup"));
    let cgroup = fs::read_to_string(&cgroup_path)
        .map_err(|error| format!("cannot read live cgroup {}: {error}", cgroup_path.display()))?;
    if !cgroup_contains_unit(&cgroup, &unit) {
        return Err(format!(
            "live cgroup for pid {pid} does not contain exact unit {unit}"
        ));
    }

    Ok(GlobalValidateHolder {
        pid,
        start_ticks,
        boot_id,
        host,
        agent,
        kind,
        target,
        unit,
        record,
        cgroup: cgroup.trim().to_string(),
    })
}

fn observe_global_validate_anchor(tool_root: &Path, path: &Path) -> GlobalValidateObservation {
    match BoxExclusionAnchor::observe(path) {
        BoxAnchorObservation::Free => GlobalValidateObservation::Free,
        BoxAnchorObservation::Held(body) => match verify_global_validate_holder(tool_root, &body) {
            Ok(holder) => GlobalValidateObservation::Held(holder),
            Err(reason) => GlobalValidateObservation::Unknown(reason),
        },
        BoxAnchorObservation::Unknown(reason) => GlobalValidateObservation::Unknown(reason),
    }
}

fn systemd_service_unit(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|line| {
        let path = line.splitn(3, ':').nth(2)?;
        path.rsplit('/')
            .find(|component| component.ends_with(".service") && !component.is_empty())
            .map(str::to_string)
    })
}

fn verify_run_attribution(
    tool_root: &Path,
    state_root: &Path,
    args: &RunArgs,
    cgroup: &str,
) -> Result<VerifiedRunAttribution, ValidateLockError> {
    let record = args.run_record.as_ref().ok_or_else(|| {
        ValidateLockError::InvalidState(
            "run acquisition has no --run-record; use ci-hub validate-run so attribution exists before the box anchor".into(),
        )
    })?;
    let record = fs::canonicalize(record)
        .map_err(|source| io_error("canonicalize run record", record, source))?;
    let expected_dir = repository_lock_root(state_root).join("ignored/validate/runs");
    let expected_dir = fs::canonicalize(&expected_dir)
        .map_err(|source| io_error("canonicalize run-record directory", &expected_dir, source))?;
    if record.parent() != Some(expected_dir.as_path()) {
        return Err(ValidateLockError::InvalidState(format!(
            "run record {} is outside canonical directory {}",
            record.display(),
            expected_dir.display()
        )));
    }
    let content = fs::read_to_string(&record)
        .map_err(|source| io_error("read run record", &record, source))?;
    let value: serde_json::Value = serde_json::from_str(&content).map_err(|error| {
        ValidateLockError::InvalidState(format!(
            "run record {} is not valid JSON: {error}",
            record.display()
        ))
    })?;
    let schema = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    let unit = if validate_run_handle::is_current(&value) {
        validate_run_handle::verify_attribution(
            tool_root,
            &record,
            &args.agent,
            &args.target,
            args.kind.as_str(),
        )
        .map_err(ValidateLockError::InvalidState)?
        .unit
        .ok_or_else(|| {
            ValidateLockError::InvalidState(format!(
                "run record {} has no unit after shared validation",
                record.display()
            ))
        })?
    } else {
        let field = |name: &str| value.get(name).and_then(serde_json::Value::as_str);
        if schema != Some(1)
            || field("agent") != Some(args.agent.as_str())
            || field("target") != Some(args.target.as_str())
            || !matches!(field("state"), Some("launching" | "running"))
        {
            return Err(ValidateLockError::InvalidState(format!(
                "run record {} does not bind readable schema state=launching|running agent={} target={}",
                record.display(),
                args.agent,
                args.target
            )));
        }
        field("unit")
            .ok_or_else(|| {
                ValidateLockError::InvalidState(format!(
                    "run record {} has no unit",
                    record.display()
                ))
            })?
            .to_string()
    };
    let expected_name = format!(
        "{}.json",
        unit.strip_suffix(".service").unwrap_or(unit.as_str())
    );
    if record.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
        return Err(ValidateLockError::InvalidState(format!(
            "run record filename does not bind unit {unit}: {}",
            record.display()
        )));
    }
    let live_unit = systemd_service_unit(cgroup).ok_or_else(|| {
        ValidateLockError::InvalidState(
            "run acquisition is not inside an attributable systemd service unit".into(),
        )
    })?;
    if live_unit != unit {
        return Err(ValidateLockError::InvalidState(format!(
            "live systemd unit {live_unit} differs from recorded unit {unit}"
        )));
    }
    Ok(VerifiedRunAttribution { unit, record })
}

fn require_run_attribution(
    tool_root: &Path,
    state_root: &Path,
    args: &RunArgs,
) -> Result<VerifiedRunAttribution, ValidateLockError> {
    #[cfg(test)]
    if args.run_record.is_none() {
        // Existing lock-mechanics unit tests do not execute inside a user
        // service. Dedicated attribution tests below exercise the production
        // predicate with fixture cgroup text and canonical records.
        return Ok(VerifiedRunAttribution {
            unit: "validate-test.service".into(),
            record: PathBuf::from("test-only-run-record"),
        });
    }
    let cgroup_path = Path::new("/proc/self/cgroup");
    let cgroup = fs::read_to_string(cgroup_path)
        .map_err(|source| io_error("read live cgroup identity", cgroup_path, source))?;
    verify_run_attribution(tool_root, state_root, args, &cgroup)
}

/// Publish the supervisor's complete, reuse-safe identity into the canonical
/// run record before it can take the box-global anchor.  Use the same lock and
/// atomic-replace protocol as `validate/run_registry.py`: the observer and the
/// launcher may update this record concurrently, and a plain read/write here
/// could erase either their state or this identity.
fn publish_run_process_identity(
    tool_root: &Path,
    record: &Path,
    owner: &ProcessOwner,
) -> Result<(), ValidateLockError> {
    if owner.pid == 0 || owner.start_ticks == 0 || owner.boot_id.trim().is_empty() {
        return Err(ValidateLockError::InvalidState(
            "run process_identity requires pid>0, start_ticks>0, and nonempty boot_id".into(),
        ));
    }
    let file_name = record
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ValidateLockError::InvalidState(format!(
                "run record has no UTF-8 filename: {}",
                record.display()
            ))
        })?;
    let parent = record.parent().ok_or_else(|| {
        ValidateLockError::InvalidState(format!(
            "run record has no parent directory: {}",
            record.display()
        ))
    })?;
    let initial = fs::read_to_string(record)
        .map_err(|source| io_error("read run record for identity", record, source))?;
    let initial_value: serde_json::Value = serde_json::from_str(&initial).map_err(|error| {
        ValidateLockError::InvalidState(format!(
            "run record {} is not valid JSON while publishing identity: {error}",
            record.display()
        ))
    })?;
    if validate_run_handle::is_current(&initial_value) {
        let identity = validate_run_handle::ProcessIdentity {
            pid: owner.pid,
            start_ticks: owner.start_ticks,
            boot_id: owner.boot_id.clone(),
        };
        validate_run_handle::bind_process_identity(tool_root, record, &identity)
            .map_err(ValidateLockError::InvalidState)?;
        return Ok(());
    }
    let lock_path = parent.join(format!(".{file_name}.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|source| io_error("open run-record lock", &lock_path, source))?;
    lock.lock_exclusive()
        .map_err(|source| io_error("lock run record", &lock_path, source))?;

    let result = (|| {
        let content = fs::read_to_string(record)
            .map_err(|source| io_error("read run record for identity", record, source))?;
        let mut value: serde_json::Value = serde_json::from_str(&content).map_err(|error| {
            ValidateLockError::InvalidState(format!(
                "run record {} is not valid JSON while publishing identity: {error}",
                record.display()
            ))
        })?;
        let object = value.as_object_mut().ok_or_else(|| {
            ValidateLockError::InvalidState(format!(
                "run record {} is not a JSON object",
                record.display()
            ))
        })?;
        let identity = serde_json::json!({
            "pid": owner.pid,
            "start_ticks": owner.start_ticks,
            "boot_id": owner.boot_id.as_str(),
        });
        if let Some(existing) = object.get("process_identity") {
            if existing != &identity {
                return Err(ValidateLockError::InvalidState(format!(
                    "run record {} already carries a different process_identity",
                    record.display()
                )));
            }
        }
        object.insert("process_identity".into(), identity);

        let mut encoded = serde_json::to_vec_pretty(&value).map_err(|error| {
            ValidateLockError::InvalidState(format!(
                "cannot encode run record {} with process_identity: {error}",
                record.display()
            ))
        })?;
        encoded.push(b'\n');
        let temporary = parent.join(format!(
            ".{file_name}.identity-{}-{}.tmp",
            owner.pid, owner.start_ticks
        ));
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|source| {
                    io_error("create run-record identity temp", &temporary, source)
                })?;
            file.write_all(&encoded)
                .map_err(|source| io_error("write run-record identity temp", &temporary, source))?;
            file.sync_all()
                .map_err(|source| io_error("sync run-record identity temp", &temporary, source))?;
            fs::rename(&temporary, record)
                .map_err(|source| io_error("publish run-record identity", record, source))?;
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|source| io_error("sync run-record directory", parent, source))
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result
    })();

    let unlock =
        FileExt::unlock(&lock).map_err(|source| io_error("unlock run record", &lock_path, source));
    result.and(unlock)
}

fn publish_required_run_process_identity(
    tool_root: &Path,
    _args: &RunArgs,
    attribution: &VerifiedRunAttribution,
    owner: &ProcessOwner,
) -> Result<(), ValidateLockError> {
    #[cfg(test)]
    if _args.run_record.is_none() {
        // Existing lock-mechanics tests execute the production `run` body but
        // are not systemd services and intentionally have no canonical
        // producer record.  Dedicated tests above exercise both the real
        // writer and its refusal paths; production cannot enter this branch.
        return Ok(());
    }
    publish_run_process_identity(tool_root, &attribution.record, owner)
}

fn anchor_token(name: &str, value: &str) -> Result<(), ValidateLockError> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(ValidateLockError::InvalidState(format!(
            "box-anchor attribution {name} must be one nonempty token"
        )));
    }
    Ok(())
}

fn publish_run_admission_result(
    tool_root: &Path,
    attribution: &VerifiedRunAttribution,
    state: validate_run_handle::AdmissionState,
    reason: Option<validate_run_handle::AdmissionRefusalReason>,
    exit_code: Option<i32>,
) -> Result<Option<validate_run_handle::Projection>, ValidateLockError> {
    #[cfg(test)]
    if attribution.record == Path::new("test-only-run-record") {
        return Ok(None);
    }
    let projected = validate_run_handle::publish_admission_result(
        tool_root,
        &attribution.record,
        state,
        reason,
        exit_code,
    )
    .map_err(ValidateLockError::InvalidState)?;
    Ok(Some(projected))
}

fn record_pre_admission_refusal(
    tool_root: &Path,
    attribution: &VerifiedRunAttribution,
    error: ValidateLockError,
) -> ValidateLockError {
    let exit_code = error.exit_code();
    match publish_run_admission_result(
        tool_root,
        attribution,
        validate_run_handle::AdmissionState::Refused,
        Some(validate_run_handle::AdmissionRefusalReason::ValidationLockRefusal),
        Some(exit_code),
    ) {
        Ok(_) => error,
        Err(record_error) => ValidateLockError::InvalidState(format!(
            "{error}; additionally could not publish typed pre-admission refusal: {record_error}"
        )),
    }
}

fn prepare_run_before_admission_with(
    tool_root: &Path,
    args: &RunArgs,
    attribution: &VerifiedRunAttribution,
    owner: Result<ProcessOwner, ValidateLockError>,
    enable_subreaper: impl FnOnce() -> io::Result<()>,
) -> Result<ProcessOwner, ValidateLockError> {
    let owner =
        owner.map_err(|error| record_pre_admission_refusal(tool_root, attribution, error))?;
    publish_required_run_process_identity(tool_root, args, attribution, &owner)?;

    let setup = (|| {
        if args.child.is_empty() {
            return Err(ValidateLockError::EmptyChild);
        }
        if args.child_deadline == 0 {
            return Err(ValidateLockError::UnboundedChildDeadline);
        }
        reject_bad_run_max(args.max, args.kind)?;
        enable_subreaper().map_err(|source| {
            io_error("enable child subreaper at", Path::new("/proc/self"), source)
        })
    })();
    setup.map_err(|error| record_pre_admission_refusal(tool_root, attribution, error))?;
    Ok(owner)
}

fn prepare_run_before_admission(
    tool_root: &Path,
    args: &RunArgs,
    attribution: &VerifiedRunAttribution,
) -> Result<ProcessOwner, ValidateLockError> {
    prepare_run_before_admission_with(
        tool_root,
        args,
        attribution,
        current_process_owner(),
        enable_child_subreaper,
    )
}

fn refusal_before_admission(
    tool_root: &Path,
    attribution: &VerifiedRunAttribution,
    code: i32,
) -> Result<i32, ValidateLockError> {
    let _ = publish_run_admission_result(
        tool_root,
        attribution,
        validate_run_handle::AdmissionState::Refused,
        Some(validate_run_handle::AdmissionRefusalReason::ValidationLockRefusal),
        Some(code),
    )?;
    Ok(code)
}

fn stale_base_refusal_with_record(
    tool_root: &Path,
    attribution: &VerifiedRunAttribution,
    error: ValidateLockError,
) -> Result<i32, ValidateLockError> {
    let code = stale_base_refusal(error)?;
    let _ = publish_run_admission_result(
        tool_root,
        attribution,
        validate_run_handle::AdmissionState::Refused,
        Some(validate_run_handle::AdmissionRefusalReason::StaleBase),
        Some(code),
    )?;
    Ok(code)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum OriginalProcessGroupState {
    /// The recorded payload group is absent. This also covers a numeric PGID
    /// that the kernel has already reused for a different leader generation:
    /// reuse is possible only after the original group ceased to exist.
    Absent(String),
    /// The recorded group is still populated by the original generation, or
    /// persists leaderless. Either case remains executable payload ownership.
    Active(String),
    /// The group probe or its generation binding could not be verified.
    Unknown(String),
}

fn classify_existing_process_group(
    pgid: u32,
    original_leader: &ProcessIdentity,
    current_leader_start: io::Result<u64>,
) -> OriginalProcessGroupState {
    match current_leader_start {
        Ok(start_ticks) if start_ticks == original_leader.start_ticks => {
            OriginalProcessGroupState::Active(format!(
                "payload process group {pgid} retains its exact leader generation {pgid}@{start_ticks}"
            ))
        }
        Ok(start_ticks) => OriginalProcessGroupState::Absent(format!(
            "numeric process group {pgid} was reused by leader {pgid}@{start_ticks}; original leader was {}@{}",
            original_leader.pid, original_leader.start_ticks
        )),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            // `kill(-pgid, 0)` succeeded but there is no PID equal to the PGID.
            // Linux permits an original group to persist after its leader exits
            // while other members remain. Never reinterpret that as reuse.
            OriginalProcessGroupState::Active(format!(
                "payload process group {pgid} remains populated without a readable group leader"
            ))
        }
        Err(source) => OriginalProcessGroupState::Unknown(format!(
            "payload process group {pgid} is present but leader generation is unreadable: {source}"
        )),
    }
}

#[cfg(test)]
thread_local! {
    /// Exact, per-test-thread group observations for the zero-residual race.
    /// Production never consults this queue. The process-domain test guard
    /// serializes every test that sets it.
    static ORIGINAL_GROUP_TEST_STATES: RefCell<VecDeque<OriginalProcessGroupState>> =
        const { RefCell::new(VecDeque::new()) };
}

fn original_process_group_state(
    pgid: u32,
    original_leader: &ProcessIdentity,
) -> OriginalProcessGroupState {
    #[cfg(test)]
    if let Some(state) = ORIGINAL_GROUP_TEST_STATES.with(|states| states.borrow_mut().pop_front()) {
        return state;
    }

    if !process_group_exists(pgid) {
        return OriginalProcessGroupState::Absent(format!(
            "payload process group {pgid} is absent"
        ));
    }
    classify_existing_process_group(pgid, original_leader, process_start_ticks(pgid))
}

#[cfg(test)]
struct OriginalGroupTestStatesGuard;

#[cfg(test)]
impl OriginalGroupTestStatesGuard {
    fn install(states: impl IntoIterator<Item = OriginalProcessGroupState>) -> Self {
        ORIGINAL_GROUP_TEST_STATES.with(|queue| {
            let mut queue = queue.borrow_mut();
            assert!(queue.is_empty(), "nested original-group test override");
            queue.extend(states);
        });
        Self
    }
}

#[cfg(test)]
impl Drop for OriginalGroupTestStatesGuard {
    fn drop(&mut self) {
        ORIGINAL_GROUP_TEST_STATES.with(|states| states.borrow_mut().clear());
    }
}

#[derive(Debug)]
enum RunCleanupFinalization {
    Released,
    Quarantined {
        residual_count: usize,
        domain_complete: bool,
        group: OriginalProcessGroupState,
    },
}

fn settle_empty_payload_domain(
    supervisor_pid: u32,
    pgid: u32,
    leader: &ProcessIdentity,
    domain_token: &str,
    mut capture: ResidualCapture,
) -> ResidualCapture {
    let deadline = Instant::now() + Duration::from_secs(EMPTY_DOMAIN_SETTLE_SECONDS);
    while capture.complete && capture.identities.is_empty() {
        if matches!(
            original_process_group_state(pgid, leader),
            OriginalProcessGroupState::Absent(_)
        ) {
            return capture;
        }
        if Instant::now() >= deadline {
            return capture;
        }
        thread::sleep(Duration::from_millis(EMPTY_DOMAIN_SETTLE_POLL_MILLIS));
        // Repeat the *whole* census, not merely the PGID probe. A helper that
        // becomes visible during the grace window must turn into an exact
        // residual and quarantine; waiting may never erase live ownership.
        capture = capture_and_freeze_residuals(supervisor_pid, leader.start_ticks, domain_token);
    }
    capture
}

struct ValidateLock {
    paths: LockPaths,
}

/// Result of one guarded acquisition attempt.
enum AcquireToken {
    Acquired,
    AcquiredReclaimed(String),
    /// A recent background release has not yet been followed by a durable
    /// owner-watch admission. The caller must stay out of both FIFO and anchor.
    Yield {
        reason: String,
        seconds_left: i64,
    },
    /// The yield predicate could not be evaluated. Absence is not permission.
    YieldUnknown {
        reason: String,
    },
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

enum GlobalTurn {
    Acquired {
        anchors: Vec<BoxExclusionAnchor>,
        slot: usize,
    },
    WaitPriority {
        head_agent: String,
        head_priority: QueuePriority,
        own_priority: QueuePriority,
        position: usize,
    },
    Held(String),
    WaitMemory(String),
    MemoryUnknown(String),
}

fn parse_mem_available_bytes(content: &str) -> Result<u64, String> {
    let mut available = None;
    for line in content.lines() {
        let Some(value) = line.strip_prefix("MemAvailable:") else {
            continue;
        };
        if available.is_some() {
            return Err("/proc/meminfo contains duplicate MemAvailable fields".into());
        }
        let fields = value.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 || fields[1] != "kB" {
            return Err(format!(
                "/proc/meminfo MemAvailable must be '<integer> kB', got {value:?}"
            ));
        }
        let kib = fields[0]
            .parse::<u64>()
            .map_err(|error| format!("/proc/meminfo MemAvailable is not an integer: {error}"))?;
        available = Some(
            kib.checked_mul(1024)
                .ok_or_else(|| "/proc/meminfo MemAvailable overflows bytes".to_string())?,
        );
    }
    available.ok_or_else(|| "/proc/meminfo has no MemAvailable field".into())
}

fn validate_memory_admission(
    live_validate_jobs: usize,
    available_bytes: u64,
    max_jobs: u64,
) -> Result<(), String> {
    let requested_jobs = u64::try_from(live_validate_jobs)
        .map_err(|_| "live validate count is outside u64".to_string())?
        .checked_add(1)
        .ok_or_else(|| "live validate count overflow".to_string())?;
    if requested_jobs > max_jobs {
        return Err(format!(
            "{requested_jobs} live validates would exceed the --max {max_jobs} admission limit"
        ));
    }
    let reserved_bytes = requested_jobs
        .checked_mul(VALIDATE_MEMORY_RESERVATION_BYTES)
        .ok_or_else(|| "validate memory reservation overflow".to_string())?;
    let budget_bytes = available_bytes.min(VALIDATE_MEMORY_BUDGET_BYTES);
    if reserved_bytes > budget_bytes {
        return Err(format!(
            "memory admission requires {reserved_bytes} bytes for {requested_jobs} live validate(s), but the budget is {budget_bytes} bytes (MemAvailable={available_bytes}, policy maximum={VALIDATE_MEMORY_BUDGET_BYTES})"
        ));
    }
    Ok(())
}

pub fn execute(
    tool_root: &Path,
    state_root: &Path,
    args: ValidateLockArgs,
) -> Result<i32, ValidateLockError> {
    let lock = ValidateLock {
        paths: LockPaths::for_workspace(tool_root, state_root),
    };
    match args.command {
        ValidateLockCommand::Prioritize(args) => lock.prioritize(args),
        ValidateLockCommand::RetirePriority(args) => lock.retire_priority(args),
        ValidateLockCommand::Acquire(args) => {
            reject_bad_acquire_max(args.max)?;
            base_admission_check(tool_root, &args.target, args.kind, args.skip_base_check)?;
            let yield_status =
                lock.wait_for_owner_watch_yield(&args.agent, args.wait, args.wait == 0)?;
            if yield_status != 0 {
                return Ok(yield_status);
            }
            // `acquire` cannot HOLD the box-global anchor,
            // but it must not hand out a lease while another repository's payload
            // owns the box. `run` is the sanctioned production path
            // (ci-hub/validate/start_unit.py invokes `validate-lock run`); this
            // keeps the bare-`acquire` path from being the way around it.
            for slot in 0..VALIDATE_SLOT_COUNT {
                match BoxExclusionAnchor::observe(&lock.paths.slot_anchor(slot)) {
                    BoxAnchorObservation::Free => {}
                    BoxAnchorObservation::Held(holder) => {
                        eprintln!(
                            "REFUSED: validation slot {slot} held by {holder}; bare acquire remains single-holder compatibility only"
                        );
                        return Ok(REFUSED_EXIT_CODE);
                    }
                    BoxAnchorObservation::Unknown(reason) => {
                        eprintln!(
                            "REFUSED: validation slot {slot} state UNKNOWN: {reason}; absence is not permission"
                        );
                        return Ok(REFUSED_EXIT_CODE);
                    }
                }
            }
            // The anchor is a live-process signal; this is the durable one. A
            // supervisor that died leaving its payload alive released the anchor,
            // but its quarantine record is still outstanding, and now visible from
            // here. ONE MORE necessary condition -- `require_no_cleanup` inside
            // `acquire` still runs for this repository's own record.
            if let Some(reason) =
                foreign_box_claim(&lock.paths.global_anchor(), &lock.paths.cleanup)
            {
                eprintln!("REFUSED: {reason}");
                return Ok(REFUSED_EXIT_CODE);
            }
            lock.acquire(&args.agent, args.kind, &args.target, args.wait, args.hold)
        }
        ValidateLockCommand::Renew(args) => {
            let selected = (0..VALIDATE_SLOT_COUNT)
                .map(|slot| ValidateLock {
                    paths: lock.paths.for_slot(slot),
                })
                .find(|candidate| {
                    candidate
                        .read_holder()
                        .ok()
                        .flatten()
                        .is_some_and(|holder| holder.agent == args.agent)
                })
                .unwrap_or_else(|| ValidateLock {
                    paths: lock.paths.clone(),
                });
            selected.renew(&args.agent, args.hold, true)?;
            Ok(0)
        }
        ValidateLockCommand::Release(args) => {
            let selected = (0..VALIDATE_SLOT_COUNT)
                .map(|slot| ValidateLock {
                    paths: lock.paths.for_slot(slot),
                })
                .find(|candidate| {
                    candidate
                        .read_holder()
                        .ok()
                        .flatten()
                        .is_some_and(|holder| holder.agent == args.agent)
                })
                .unwrap_or_else(|| ValidateLock {
                    paths: lock.paths.clone(),
                });
            selected.release(&args.agent, true)?;
            Ok(0)
        }
        ValidateLockCommand::Status => {
            for slot in 0..VALIDATE_SLOT_COUNT {
                println!("validation slot {slot}:");
                ValidateLock {
                    paths: lock.paths.for_slot(slot),
                }
                .status(tool_root)?;
            }
            Ok(0)
        }
        ValidateLockCommand::AuthorityStatus(args) => {
            debug_assert!(args.json, "clap requires --json");
            ValidateLock {
                paths: LockPaths::for_authority_query(state_root),
            }
            .print_authority_status()?;
            Ok(0)
        }
        ValidateLockCommand::YieldStatus(args) => {
            debug_assert!(args.json, "clap requires --json");
            lock.print_yield_status()
        }
        ValidateLockCommand::ReclaimDead | ValidateLockCommand::StaleClear => {
            let mut errors = Vec::new();
            let mut found = false;
            for slot in 0..VALIDATE_SLOT_COUNT {
                let candidate = ValidateLock {
                    paths: lock.paths.for_slot(slot),
                };
                if candidate.paths.lock.exists()
                    || candidate.paths.owner.exists()
                    || candidate.paths.cleanup.exists()
                {
                    found = true;
                    if let Err(error) = candidate.reclaim_dead() {
                        errors.push(format!("slot {slot}: {error}"));
                    }
                }
            }
            if !errors.is_empty() {
                return Err(ValidateLockError::ReclaimNotProven(errors.join("; ")));
            }
            if !found {
                eprintln!("validate-lock: reclaim-dead: no lock held");
            }
            Ok(0)
        }
        ValidateLockCommand::CensusOrphanedDomain(args) => {
            let operation = args.operation.clone();
            let selected = (0..VALIDATE_SLOT_COUNT)
                .map(|slot| ValidateLock {
                    paths: lock.paths.for_slot(slot),
                })
                .find(|candidate| {
                    candidate
                        .read_holder()
                        .ok()
                        .flatten()
                        .is_some_and(|holder| {
                            holder.agent == args.agent
                                && format!("{}:{}", holder.kind, holder.target) == operation
                        })
                })
                .unwrap_or_else(|| ValidateLock {
                    paths: lock.paths.clone(),
                });
            selected.census_orphaned_domain(args)
        }
        ValidateLockCommand::Run(args) => lock.run_with_roots(args, tool_root, state_root),
    }
}

fn reject_bad_acquire_max(max: u64) -> Result<(), ValidateLockError> {
    if max != LEGACY_ACQUIRE_CAP {
        return Err(ValidateLockError::BadMax);
    }
    Ok(())
}

fn reject_bad_run_max(max: u64, _kind: Kind) -> Result<(), ValidateLockError> {
    if !matches!(max, LEGACY_ACQUIRE_CAP | VALIDATE_CONCURRENT_CAP) {
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

/// Refuse to admit a validation unless its exact head satisfies its repository
/// boundary. Hermit must contain every immutable
/// producer and merge-gate floor. Mutable origin/main currency is intentionally
/// absent here: the receipt records its exact base and the canonical verifier
/// asserts currency once at the final merge boundary.
///
/// Reverie has no Hermit producer-floor registry, so its exact-SHA check returns
/// here and its moving-main boundary is enforced by
/// [`current_main_freshness_check`]. Bench kinds pass through. Validation
/// targets that are not exact SHAs and the legacy `--skip-base-check` flag are
/// refused: either would sever the
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
    if !kind.is_validation() {
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

    if kind == Kind::ReverieValidate {
        return Ok(());
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
    let output = match Command::new(program).args(rest).bounded_output() {
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

/// Fetch the product repository's current `origin/main`, then require it to be
/// an ancestor of the exact validation target.
///
/// This is deliberately separate from [`base_admission_check`]. That check is
/// the immutable producer-floor authority; this one is the moving boundary
/// whose answer can change while a caller waits. Production `run` invokes this
/// once before entering the box-global queue and again at the free boundary.
/// The inner product driver retains its local-ref check as a defensive
/// assertion, but it no longer discovers staleness only after consuming the
/// validation lease.
///
/// Fail-CLOSED: a missing checkout, failed fetch, unresolved target, or
/// indeterminate ancestry is a stale-base refusal. An unavailable freshness
/// authority cannot authorize scarce box time.
fn validation_repository(root: &Path, kind: Kind) -> Result<Option<PathBuf>, ValidateLockError> {
    // Bench work is not validation evidence and has no moving-main boundary.
    // FrozenValidate still needs this repository: it inverts the ordinary
    // relation below and is admitted only when the exact target does NOT contain
    // freshly fetched main. That makes the opt-in path specific to the old-SHA
    // measurement it claims rather than a second always-open admission route.
    if kind == Kind::Bench {
        return Ok(None);
    }

    // A production run is launched with systemd's WorkingDirectory set to the
    // exact source checkout. Use that repository rather than assuming every
    // caller is a linked worktree of the parent's primary submodule. The test
    // binary runs many lock-only cases from one process-wide cwd, so dedicated
    // fixtures live below their isolated root instead.
    #[cfg(not(test))]
    {
        let _ = root;
        env::current_dir().map(Some).map_err(|error| {
            ValidateLockError::StaleBase(format!(
                "validate-lock: validation source checkout is UNRESOLVED ({error}); current-main freshness cannot be established."
            ))
        })
    }
    #[cfg(test)]
    {
        Ok(Some(root.join(match kind {
            Kind::Validate | Kind::FrozenValidate => "hermit",
            Kind::ReverieValidate => "reverie",
            Kind::Bench => unreachable!("bench returned above"),
        })))
    }
}

fn current_main_freshness_check(
    root: &Path,
    target: &str,
    kind: Kind,
) -> Result<(), ValidateLockError> {
    let Some(repository) = validation_repository(root, kind)? else {
        return Ok(());
    };

    // Existing lock-only tests inject the immutable-floor command and do not
    // construct a product checkout. Production cannot take this arm: the
    // override is effective only in cfg(test), and the dedicated freshness
    // brackets below exercise real repositories and real fetches.
    #[cfg(test)]
    if test_only_override(ADMIT_PREFLIGHT_CMD_ENV).is_some() && !repository.join(".git").exists() {
        return Ok(());
    }

    // A frozen checkout deliberately rewrites its private origin/main to the
    // measured target after each freshness check. If current main has since
    // diverged, the next fetch is necessarily a non-fast-forward update of
    // that private remote-tracking ref. The Git metadata lock above makes the
    // forced update exclusive; without `+`, the frozen run becomes unusable
    // precisely for the divergent history it exists to measure.
    let refspec = "+refs/heads/main:refs/remotes/origin/main";
    let fetch_timeout = format!("{CURRENT_MAIN_FETCH_TIMEOUT_SECONDS}s");
    let common = Command::new("git")
        .arg("-C")
        .arg(&repository)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .bounded_output()
        .map_err(|error| {
            ValidateLockError::StaleBase(format!(
                "validate-lock: shared Git directory is UNRESOLVED in {} ({error})",
                repository.display()
            ))
        })?;
    if !common.status.success() {
        return Err(ValidateLockError::StaleBase(format!(
            "validate-lock: shared Git directory is UNRESOLVED in {} (git exit {:?}: {})",
            repository.display(),
            common.status.code(),
            String::from_utf8_lossy(&common.stderr).trim()
        )));
    }
    let common = PathBuf::from(String::from_utf8_lossy(&common.stdout).trim());
    let git_lock_path = common.join("ci-hub-validate.lock");
    let git_lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&git_lock_path)
        .map_err(|source| io_error("open shared Git mutation lock", &git_lock_path, source))?;
    git_lock
        .lock_exclusive()
        .map_err(|source| io_error("lock shared Git metadata", &git_lock_path, source))?;
    let mut fetch = Command::new("timeout");
    fetch.args(["--signal=TERM", "--kill-after=1s"]);
    fetch.arg(&fetch_timeout);
    if crate::on_path("with-proxy") {
        fetch.arg("with-proxy");
    }
    let output = fetch
        .arg("git")
        .arg("-C")
        .arg(&repository)
        .args(["fetch", "--quiet", "--no-tags", "origin", refspec])
        .bounded_output()
        .map_err(|error| {
            ValidateLockError::StaleBase(format!(
                "validate-lock: current origin/main fetch could not run in {} ({error}); freshness for {target} is UNRESOLVED.",
                repository.display()
            ))
        })?;
    FileExt::unlock(&git_lock)
        .map_err(|source| io_error("unlock shared Git metadata", &git_lock_path, source))?;
    if !output.status.success() {
        return Err(ValidateLockError::StaleBase(format!(
            "validate-lock: current origin/main fetch failed in {} (git exit {:?}: {}); freshness for {target} is UNRESOLVED.",
            repository.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(&repository)
        .args([
            "merge-base",
            "--is-ancestor",
            "refs/remotes/origin/main",
            target,
        ])
        .bounded_output()
        .map_err(|error| {
            ValidateLockError::StaleBase(format!(
                "validate-lock: current-main ancestry check could not run in {} ({error}); freshness for {target} is UNRESOLVED.",
                repository.display()
            ))
        })?;
    match (kind, output.status.code()) {
        (Kind::FrozenValidate, Some(1)) => {
            // The remote fetch above is the authority proving this target is
            // historical. The child then runs the target's own validation
            // gates, whose Reverie-pin monotonicity floor is `origin/main`.
            // Leaving the live ref in place makes that gate compare an old
            // target with today's pin and abort before the matrix starts.
            //
            // This mutation is safe only in the standalone shared clone that
            // start_unit creates for frozen validation. A linked worktree
            // shares refs with every sibling and must fail closed instead of
            // changing their view of main.
            if !repository.join(".git").is_dir() {
                return Err(ValidateLockError::StaleBase(format!(
                    "REFUSE: frozen validation checkout {} does not have independent Git refs; \
                     refusing to rewrite shared origin/main",
                    repository.display()
                )));
            }
            let reset = Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["update-ref", "refs/remotes/origin/main", target])
                .bounded_output()
                .map_err(|error| {
                    ValidateLockError::StaleBase(format!(
                        "validate-lock: could not set frozen origin/main to {target} in {} ({error})",
                        repository.display()
                    ))
                })?;
            if !reset.status.success() {
                return Err(ValidateLockError::StaleBase(format!(
                    "validate-lock: could not set frozen origin/main to {target} in {} \
                     (git exit {:?}: {})",
                    repository.display(),
                    reset.status.code(),
                    String::from_utf8_lossy(&reset.stderr).trim()
                )));
            }
            let readback = Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["rev-parse", "--verify", "refs/remotes/origin/main^{commit}"])
                .bounded_output()
                .map_err(|error| {
                    ValidateLockError::StaleBase(format!(
                        "validate-lock: frozen origin/main readback failed in {} ({error})",
                        repository.display()
                    ))
                })?;
            let observed = String::from_utf8_lossy(&readback.stdout).trim().to_string();
            if !readback.status.success() || observed != target {
                return Err(ValidateLockError::StaleBase(format!(
                    "validate-lock: frozen origin/main readback in {} was {:?}, expected {target}",
                    repository.display(),
                    observed
                )));
            }
            Ok(())
        }
        (Kind::FrozenValidate, Some(0)) => Err(ValidateLockError::StaleBase(format!(
            "REFUSE: frozen validation target {target} still contains freshly fetched origin/main. Use ordinary validate for a current target."
        ))),
        (_, Some(0)) => Ok(()),
        (_, Some(1)) => Err(ValidateLockError::StaleBase(format!(
            "REFUSE: validation target {target} does not contain freshly fetched origin/main. Rebase before validating."
        ))),
        (_, other) => Err(ValidateLockError::StaleBase(format!(
            "validate-lock: current-main ancestry for {target} is UNRESOLVED in {} (git exit {other:?}: {}).",
            repository.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

fn stale_base_refusal(error: ValidateLockError) -> Result<i32, ValidateLockError> {
    match error {
        ValidateLockError::StaleBase(message) => {
            eprintln!("{message}");
            Ok(STALE_BASE_EXIT_CODE)
        }
        other => Err(other),
    }
}

#[cfg(test)]
thread_local! {
    static CURRENT_MAIN_BETWEEN_CHECKS_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        RefCell::new(None);
}

#[cfg(test)]
fn run_current_main_between_checks_hook() {
    CURRENT_MAIN_BETWEEN_CHECKS_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_current_main_between_checks_hook() {}

impl ValidateLock {
    fn holder_lease_record(
        &self,
        holder: &ValidateLockState,
    ) -> Result<LeaseRecord, ValidateLockError> {
        let ttl = holder.expires_at.saturating_sub(holder.acquired_at);
        let ttl_seconds = u64::try_from(ttl).map_err(|_| {
            ValidateLockError::InvalidState("validate holder has a negative TTL".into())
        })?;
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
            resource: format!("validate/{}/{}", holder.kind, holder.target),
            generation: format!(
                "validate:{}:{}:{}:{}:{}",
                holder.agent, holder.kind, holder.target, holder.acquired_at, holder.expires_at
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
        holder: &ValidateLockState,
        now: i64,
        cleanup: CleanupVerification,
    ) -> Result<LeaseDecision, ValidateLockError> {
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
            "validate:{}:{}:{}:{}",
            holder.agent, holder.kind, holder.target, lease.generation
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

    fn global_queue_path(&self) -> PathBuf {
        suffix(&self.paths.global_anchor(), ".priority-queue")
    }

    fn global_queue_guard_path(&self) -> PathBuf {
        suffix(&self.paths.global_anchor(), ".priority-queue.guard")
    }

    fn with_global_queue_guard<T>(
        &self,
        action: impl FnOnce() -> Result<T, ValidateLockError>,
    ) -> Result<T, ValidateLockError> {
        let path = self.global_queue_guard_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|source| io_error("create global queue directory", parent, source))?;
        }
        let guard = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| io_error("open global queue guard", &path, source))?;
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
                Err(source) => {
                    return Err(io_error("lock global queue guard", &path, source));
                }
            }
        }
        let result = action();
        let unlock = FileExt::unlock(&guard)
            .map_err(|source| io_error("unlock global queue guard", &path, source));
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn read_global_queue(&self) -> Result<Vec<GlobalQueueEntry>, ValidateLockError> {
        let path = self.global_queue_path();
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(io_error("read global priority queue", &path, source)),
        };
        let queue: Vec<_> = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .enumerate()
            .map(|(index, line)| GlobalQueueEntry::parse(line, index + 1))
            .collect::<Result<_, _>>()?;
        for (index, entry) in queue.iter().enumerate() {
            if queue[..index].iter().any(|prior| {
                prior.matches(&entry.agent, entry.kind, &entry.target)
                    && (prior.owner.is_none()
                        || entry.owner.is_none()
                        || prior.owner == entry.owner)
            }) {
                return Err(ValidateLockError::InvalidState(format!(
                    "global priority queue has duplicate identity agent={} kind={} target={}; refusing ambiguous admission",
                    entry.agent,
                    entry.kind.as_str(),
                    entry.target
                )));
            }
        }
        Ok(queue)
    }

    fn write_global_queue(&self, queue: &[GlobalQueueEntry]) -> Result<(), ValidateLockError> {
        let path = self.global_queue_path();
        let temporary = suffix(&path, &format!(".tmp-{}", std::process::id()));
        let mut content = String::new();
        for entry in queue {
            content.push_str(&entry.render());
        }
        fs::write(&temporary, content)
            .map_err(|source| io_error("write global priority queue temp", &temporary, source))?;
        fs::rename(&temporary, &path)
            .map_err(|source| io_error("replace global priority queue", &path, source))
    }

    fn global_queue_owner_liveness(
        &self,
        owner: &ProcessOwner,
    ) -> Result<OwnerLiveness, ValidateLockError> {
        let host = current_host();
        if host == "unknown" || owner.host != host {
            return Ok(OwnerLiveness::Unknown(format!(
                "queue owner host {} does not match observable host {host}",
                owner.host
            )));
        }
        let boot_id = current_boot_id()?;
        if owner.boot_id != boot_id {
            return Ok(OwnerLiveness::Dead(format!(
                "queue owner boot {} differs from current boot {boot_id}",
                owner.boot_id
            )));
        }
        match process_start_ticks(owner.pid) {
            Ok(observed) if observed == owner.start_ticks => Ok(OwnerLiveness::Alive),
            Ok(observed) => Ok(OwnerLiveness::Dead(format!(
                "queue owner pid {} start_ticks changed from {} to {observed}",
                owner.pid, owner.start_ticks
            ))),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(OwnerLiveness::Dead(
                format!("queue owner pid {} no longer exists", owner.pid),
            )),
            Err(source) => Ok(OwnerLiveness::Unknown(format!(
                "queue owner pid {} cannot be verified: {source}",
                owner.pid
            ))),
        }
    }

    fn prune_and_sort_global_queue(
        &self,
        queue: &mut Vec<GlobalQueueEntry>,
        now: i64,
    ) -> Result<(), ValidateLockError> {
        let mut retained = Vec::with_capacity(queue.len());
        for entry in queue.drain(..) {
            match &entry.owner {
                None if entry.enqueued_at < now - GLOBAL_QUEUE_DECLARATION_TTL_SECONDS => {}
                None => retained.push(entry),
                Some(owner) => match self.global_queue_owner_liveness(owner)? {
                    OwnerLiveness::Alive => retained.push(entry),
                    OwnerLiveness::Dead(_) => {}
                    OwnerLiveness::Unknown(reason) => {
                        return Err(ValidateLockError::InvalidState(format!(
                            "global priority queue owner is UNKNOWN for agent={} target={}: {reason}; absence is not permission",
                            entry.agent, entry.target
                        )))
                    }
                },
            }
        }
        retained.sort_by(|left, right| {
            left.owner
                .is_none()
                .cmp(&right.owner.is_none())
                .then_with(|| {
                    left.priority
                        .rank(&left.agent)
                        .cmp(&right.priority.rank(&right.agent))
                })
                // `sort_by` is stable, so same-second declarations retain the
                // file's guard-serialized insertion order: equal priority is FIFO.
                .then_with(|| left.enqueued_at.cmp(&right.enqueued_at))
        });
        *queue = retained;
        Ok(())
    }

    fn prioritize(&self, args: PrioritizeArgs) -> Result<i32, ValidateLockError> {
        for (name, value) in [
            ("agent", args.agent.as_str()),
            ("kind", args.kind.as_str()),
            ("target", args.target.as_str()),
        ] {
            anchor_token(name, value)?;
        }
        let now = epoch_seconds()?;
        let position = self.with_global_queue_guard(|| {
            let mut queue = self.read_global_queue()?;
            self.prune_and_sort_global_queue(&mut queue, now)?;
            if let Some(entry) = queue
                .iter_mut()
                .find(|entry| entry.matches(&args.agent, args.kind, &args.target))
            {
                entry.priority = args.priority;
            } else {
                queue.push(GlobalQueueEntry {
                    enqueued_at: now,
                    priority: args.priority,
                    agent: args.agent.clone(),
                    kind: args.kind,
                    target: args.target.clone(),
                    owner: None,
                });
            }
            self.prune_and_sort_global_queue(&mut queue, now)?;
            let position = queue
                .iter()
                .filter(|entry| entry.owner.is_some())
                .position(|entry| entry.matches(&args.agent, args.kind, &args.target));
            self.write_global_queue(&queue)?;
            Ok(position)
        })?;
        match position {
            Some(position) => eprintln!(
                "validate-lock: reprioritized ACTIVE waiter agent={} kind={} target={} priority={} position={}",
                args.agent,
                args.kind.as_str(),
                args.target,
                args.priority.as_str(),
                position + 1
            ),
            None => eprintln!(
                "validate-lock: DECLARED agent={} kind={} target={} priority={}; inert until that exact run enters the global queue",
                args.agent,
                args.kind.as_str(),
                args.target,
                args.priority.as_str()
            ),
        }
        Ok(0)
    }

    fn retire_priority(&self, args: RetirePriorityArgs) -> Result<i32, ValidateLockError> {
        for (name, value) in [
            ("agent", args.agent.as_str()),
            ("kind", args.kind.as_str()),
            ("target", args.target.as_str()),
        ] {
            anchor_token(name, value)?;
        }
        let now = epoch_seconds()?;
        let retired = self.with_global_queue_guard(|| {
            let mut queue = self.read_global_queue()?;
            let exact = queue
                .iter()
                .position(|entry| entry.matches(&args.agent, args.kind, &args.target));
            match exact {
                Some(index) if queue[index].owner.is_some() => {
                    let owner = queue[index].owner.as_ref().expect("checked above");
                    return Err(ValidateLockError::InvalidState(format!(
                        "REFUSED: priority identity agent={} kind={} target={} is an ACTIVE waiter owned by pid {} start_ticks {}; terminal retirement may remove declarations only",
                        args.agent,
                        args.kind.as_str(),
                        args.target,
                        owner.pid,
                        owner.start_ticks
                    )));
                }
                Some(index) => {
                    queue.remove(index);
                    self.prune_and_sort_global_queue(&mut queue, now)?;
                    self.write_global_queue(&queue)?;
                    Ok(true)
                }
                None => {
                    // Idempotent terminal delivery: a retry after successful
                    // retirement, TTL expiry, or declaration consumption is a
                    // no-op. Still persist ordinary dead/expired pruning.
                    self.prune_and_sort_global_queue(&mut queue, now)?;
                    self.write_global_queue(&queue)?;
                    Ok(false)
                }
            }
        })?;
        if retired {
            eprintln!(
                "validate-lock: RETIRED declaration agent={} kind={} target={} terminal={}",
                args.agent,
                args.kind.as_str(),
                args.target,
                args.outcome.as_str()
            );
        } else {
            eprintln!(
                "validate-lock: declaration already absent agent={} kind={} target={} terminal={}",
                args.agent,
                args.kind.as_str(),
                args.target,
                args.outcome.as_str()
            );
        }
        Ok(0)
    }

    fn try_global_turn(
        &self,
        args: &RunArgs,
        owner: &ProcessOwner,
        attribution: &str,
        consume_on_acquire: bool,
    ) -> Result<GlobalTurn, ValidateLockError> {
        let now = epoch_seconds()?;
        self.with_global_queue_guard(|| {
            let mut queue = self.read_global_queue()?;
            self.prune_and_sort_global_queue(&mut queue, now)?;
            if let Some(entry) = queue.iter_mut().find(|entry| {
                entry.matches(&args.agent, args.kind, &args.target)
                    && (entry.owner.is_none() || entry.owner.as_ref() == Some(owner))
            })
            {
                match &entry.owner {
                    None => entry.owner = Some(owner.clone()),
                    Some(recorded) if recorded == owner => {}
                    Some(recorded) => {
                        return Err(ValidateLockError::InvalidState(format!(
                            "global priority queue identity agent={} kind={} target={} is already owned by live pid {} start_ticks {}; refusing replacement by pid {} start_ticks {}",
                            args.agent,
                            args.kind.as_str(),
                            args.target,
                            recorded.pid,
                            recorded.start_ticks,
                            owner.pid,
                            owner.start_ticks
                        )))
                    }
                }
            } else {
                queue.push(GlobalQueueEntry {
                    enqueued_at: now,
                    priority: QueuePriority::Routine,
                    agent: args.agent.clone(),
                    kind: args.kind,
                    target: args.target.clone(),
                    owner: Some(owner.clone()),
                });
            }
            self.prune_and_sort_global_queue(&mut queue, now)?;
            let my_index = queue
                .iter()
                .position(|entry| {
                    entry.matches(&args.agent, args.kind, &args.target)
                        && entry.owner.as_ref() == Some(owner)
                })
                .ok_or_else(|| {
                    ValidateLockError::InvalidState(
                        "global priority queue lost the current run".into(),
                    )
                })?;
            if my_index != 0 {
                let head = &queue[0];
                let result = GlobalTurn::WaitPriority {
                    head_agent: head.agent.clone(),
                    head_priority: head.priority,
                    own_priority: queue[my_index].priority,
                    position: my_index + 1,
                };
                self.write_global_queue(&queue)?;
                return Ok(result);
            }

            if args.kind.is_validation() {
                let mut live_validate_jobs = 0usize;
                for candidate in 0..VALIDATE_SLOT_COUNT {
                    match BoxExclusionAnchor::observe(&self.paths.slot_anchor(candidate)) {
                        BoxAnchorObservation::Free => {}
                        BoxAnchorObservation::Held(_) => live_validate_jobs += 1,
                        BoxAnchorObservation::Unknown(reason) => {
                            self.write_global_queue(&queue)?;
                            return Ok(GlobalTurn::MemoryUnknown(format!(
                                "cannot establish live validate reservations from slot {candidate}: {reason}"
                            )));
                        }
                    }
                }
                if live_validate_jobs < args.max as usize {
                    let content = match fs::read_to_string(&self.paths.meminfo) {
                        Ok(content) => content,
                        Err(error) => {
                            self.write_global_queue(&queue)?;
                            return Ok(GlobalTurn::MemoryUnknown(format!(
                                "cannot read {}: {error}",
                                self.paths.meminfo.display()
                            )));
                        }
                    };
                    let available_bytes = match parse_mem_available_bytes(&content) {
                        Ok(bytes) => bytes,
                        Err(reason) => {
                            self.write_global_queue(&queue)?;
                            return Ok(GlobalTurn::MemoryUnknown(reason));
                        }
                    };
                    if let Err(reason) =
                        validate_memory_admission(live_validate_jobs, available_bytes, args.max)
                    {
                        self.write_global_queue(&queue)?;
                        return Ok(GlobalTurn::WaitMemory(reason));
                    }
                }
            }

            let mut anchors = Vec::new();
            let mut slot = 0usize;
            let wanted = if args.kind == Kind::Bench || args.max == LEGACY_ACQUIRE_CAP {
                VALIDATE_SLOT_COUNT
            } else {
                1
            };
            for candidate in 0..VALIDATE_SLOT_COUNT {
                let path = self.paths.slot_anchor(candidate);
                let anchor = BoxExclusionAnchor::take_attributed(&path, 0, attribution)
                    .map_err(|source| io_error("take validation slot", &path, source))?;
                if let Some(anchor) = anchor {
                    if anchors.is_empty() {
                        slot = candidate;
                    }
                    anchors.push(anchor);
                    if anchors.len() == wanted {
                        break;
                    }
                } else if args.kind == Kind::Bench {
                    anchors.clear();
                    break;
                }
            }
            if anchors.len() == wanted {
                if consume_on_acquire {
                    queue.remove(my_index);
                }
                self.write_global_queue(&queue)?;
                Ok(GlobalTurn::Acquired { anchors, slot })
            } else {
                let holders = (0..VALIDATE_SLOT_COUNT)
                    .map(|index| {
                        let path = self.paths.slot_anchor(index);
                        format!("slot {index}: {}", BoxExclusionAnchor::describe_holder(&path))
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                self.write_global_queue(&queue)?;
                Ok(GlobalTurn::Held(holders))
            }
        })
    }

    fn remove_own_global_waiter(
        &self,
        args: &RunArgs,
        owner: &ProcessOwner,
    ) -> Result<(), ValidateLockError> {
        self.with_global_queue_guard(|| {
            let mut queue = self.read_global_queue()?;
            queue.retain(|entry| {
                !(entry.matches(&args.agent, args.kind, &args.target)
                    && entry.owner.as_ref() == Some(owner))
            });
            self.write_global_queue(&queue)
        })
    }

    fn wait_for_global_turn(
        &self,
        args: &RunArgs,
        owner: &ProcessOwner,
        attribution: &str,
        consume_on_acquire: bool,
        root: &Path,
    ) -> Result<Option<(Vec<BoxExclusionAnchor>, usize)>, ValidateLockError> {
        let started = Instant::now();
        let mut last = String::new();
        // The caller checked freshness immediately before entering the queue,
        // so the clock starts now rather than at zero.
        let mut last_freshness = Instant::now();
        loop {
            match self.try_global_turn(args, owner, attribution, consume_on_acquire)? {
                GlobalTurn::Acquired { anchors, slot } => return Ok(Some((anchors, slot))),
                GlobalTurn::WaitPriority {
                    head_agent,
                    head_priority,
                    own_priority,
                    position,
                } => {
                    let token = format!(
                        "priority:{head_agent}:{}:{position}",
                        head_priority.as_str()
                    );
                    if token != last {
                        eprintln!(
                            "validate-lock: YIELD priority={} position={position}; earlier-or-higher-priority queue head agent={head_agent} priority={}",
                            own_priority.as_str(),
                            head_priority.as_str()
                        );
                        last = token;
                    }
                }
                GlobalTurn::Held(holder) => {
                    let token = format!("held:{holder}");
                    if token != last {
                        eprintln!(
                            "validate-lock: waiting in declared-priority queue; live holder retained: {holder}"
                        );
                        last = token;
                    }
                }
                GlobalTurn::WaitMemory(reason) => {
                    let token = format!("memory:{reason}");
                    if token != last {
                        eprintln!("validate-lock: waiting for memory admission: {reason}");
                        last = token;
                    }
                }
                GlobalTurn::MemoryUnknown(reason) => {
                    self.remove_own_global_waiter(args, owner)?;
                    eprintln!(
                        "REFUSED: validate memory admission is UNKNOWN: {reason}; absence is not permission"
                    );
                    return Ok(None);
                }
            }
            if args.no_wait || started.elapsed() >= Duration::from_secs(args.wait) {
                self.remove_own_global_waiter(args, owner)?;
                eprintln!(
                    "REFUSED: declared-priority queue did not grant agent={} target={} within {}s",
                    args.agent,
                    args.target,
                    started.elapsed().as_secs()
                );
                return Ok(None);
            }
            // `run` enables subreaping before queue admission. Preflight and
            // queue helpers can therefore leave exited grandchildren adopted
            // here even though the payload has not been spawned yet. There is
            // no protected direct Child in this phase, so the shared full reap
            // is race-free.
            // ⚠️ A DOOMED WAITER MUST NOT HOLD A SLOT. Freshness was true when this
            // run entered the queue; `origin/main` can advance while it waits, and
            // until now nothing noticed until the waiter reached the FRONT -- where
            // the existing check refuses it having consumed the slot it was waiting
            // for. Measured 2026-08-26: agent(hermit-107) waited 34m40s for
            // e3dbcfcbcebf, acquired the box, and refused on freshness with ZERO
            // nodes run, delaying every landable head behind it by that whole time.
            //
            // ⚠️ EVICTING EARLY IS PROVABLY SAFE BECAUSE STALENESS IS MONOTONE.
            // `origin/main` only moves forward, so a target that has stopped
            // containing it can never contain it again without being rebased --
            // which produces a different target and a different run. There is no
            // state in which waiting longer would have made this target fresh, so
            // an early eviction cannot be premature. That property is what makes
            // this a check rather than a policy.
            if last_freshness.elapsed() >= Duration::from_secs(CURRENT_MAIN_RECHECK_SECONDS) {
                last_freshness = Instant::now();
                if let Err(error) = current_main_freshness_check(root, &args.target, args.kind) {
                    if stale_base_is_definite(&error) {
                        self.remove_own_global_waiter(args, owner)?;
                        if let ValidateLockError::StaleBase(message) = &error {
                            eprintln!("{message}");
                        }
                        eprintln!(
                            "validate-lock: EVICTED agent={} target={} from the queue after {}s: origin/main advanced while it waited, so this run could never have been admitted. Slot released.",
                            args.agent,
                            args.target,
                            started.elapsed().as_secs()
                        );
                        return Ok(None);
                    }
                    // UNRESOLVED keeps its position on purpose; see
                    // `stale_base_is_definite`.
                    eprintln!(
                        "validate-lock: mid-wait freshness UNRESOLVED for target={}, keeping queue position: {error}",
                        args.target
                    );
                }
            }
            reap_exited_children();
            thread::sleep(Duration::from_secs(POLL_SECONDS));
        }
    }

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
                ValidateLockError::CleanupQuarantined(format!(
                    "FLEET-WIDE validate quarantine: every validate-lock acquisition is refused. \
                     Discharge with `validate-lock census-orphaned-domain` using the exact command \
                     printed by `validate-lock status`, then run `validate-lock reclaim-dead`: \
                     {reason}"
                )),
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
                &self.paths.global_anchor(),
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
        // `Some` only at publish. Every other transition preserves the exact
        // payload-owned cleanup boundary already recorded there.
        domain: Option<CleanupDomain>,
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
            if let Some(domain) = domain {
                record.domain = domain;
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
            // The start-gated child has its own PGID. Record that payload-owned
            // boundary, never the inherited Codex/systemd cgroup that also
            // contains this supervisor and unrelated infrastructure.
            Some(CleanupDomain::ProcessGroup),
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
                    remove_cleanup_record_and_claim(
                        &self.paths.cleanup,
                        &self.paths.global_anchor(),
                    )
                    .map_err(|source| io_error("remove", &self.paths.cleanup, source))
                }
                other => Err(ValidateLockError::InvalidState(format!(
                    "unstarted cleanup requires armed authority, got {other:?}"
                ))),
            }
        })
    }

    /// Finalize the supervisor's census under the same guard that owns the
    /// durable cleanup authority. A complete empty descendant census is not by
    /// itself enough: the original process group must also be generation-bound
    /// absent. Conversely, a different process now using the same numeric PGID
    /// is proof that the original group ended, not ownership by this run.
    fn finalize_run_census(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        leader: &ProcessIdentity,
        pgid: u32,
        capture: ResidualCapture,
    ) -> Result<RunCleanupFinalization, ValidateLockError> {
        self.with_guard(|| {
            let holder = self.read_holder()?.ok_or_else(|| {
                ValidateLockError::InvalidState("cleanup finalization has no lock holder".into())
            })?;
            if holder.agent != agent || holder.kind != kind.as_str() || holder.target != target {
                return Err(ValidateLockError::InvalidState(
                    "cleanup finalization does not match the current operation".into(),
                ));
            }
            let mut record = match self.cleanup_verification(Some(&holder)) {
                CleanupVerification::Armed { record, .. }
                | CleanupVerification::Active { record, .. }
                | CleanupVerification::Uncensused { record, .. }
                | CleanupVerification::Recoverable { record, .. } => record,
                CleanupVerification::None => {
                    return Err(ValidateLockError::InvalidState(
                        "cleanup finalization has no durable authority".into(),
                    ))
                }
                CleanupVerification::Unknown { reason, .. } => {
                    return Err(ValidateLockError::CleanupQuarantined(format!(
                        "cannot finalize unverifiable authority: {reason}"
                    )))
                }
            };
            match &record.phase {
                CleanupPhase::CensusPending {
                    leader: recorded_leader,
                    pgid: recorded_pgid,
                } if recorded_leader == leader && *recorded_pgid == pgid => {}
                phase => {
                    return Err(ValidateLockError::InvalidState(format!(
                        "cleanup finalization rejected phase {phase:?}"
                    )))
                }
            }

            let group = original_process_group_state(pgid, leader);
            if capture.complete
                && capture.identities.is_empty()
                && matches!(group, OriginalProcessGroupState::Absent(_))
            {
                remove_cleanup_record_and_claim(&self.paths.cleanup, &self.paths.global_anchor())
                    .map_err(|source| io_error("remove", &self.paths.cleanup, source))?;
                return Ok(RunCleanupFinalization::Released);
            }

            let residual_count = capture.identities.len();
            let domain_complete = capture.complete;
            record.phase = CleanupPhase::Residual {
                pgid,
                domain_complete,
                residuals: capture.identities,
            };
            write_cleanup_record(&self.paths.cleanup, &record)
                .map_err(|source| io_error("write", &self.paths.cleanup, source))?;
            Ok(RunCleanupFinalization::Quarantined {
                residual_count,
                domain_complete,
                group,
            })
        })
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
        self.acquire_ordered(agent, kind, target, wait, hold, true)
    }

    fn acquire_after_global_turn(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        wait: u64,
        hold: u64,
    ) -> Result<i32, ValidateLockError> {
        self.acquire_ordered(agent, kind, target, wait, hold, false)
    }

    fn acquire_ordered(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        wait: u64,
        hold: u64,
        honor_repository_fifo: bool,
    ) -> Result<i32, ValidateLockError> {
        let started = Instant::now();
        let mut last = String::new();
        loop {
            let token = self.with_guard(|| {
                self.try_acquire_ordered(agent, kind, target, hold, honor_repository_fifo)
            })?;
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
                AcquireToken::Yield {
                    reason,
                    seconds_left,
                } => {
                    let token = format!("YIELD:{seconds_left}:{reason}");
                    if token != last {
                        eprintln!(
                            "validate-lock: YIELD for owner watch (~{seconds_left}s): {reason}"
                        );
                        last = token;
                    }
                    if wait == 0 {
                        eprintln!("REFUSED: owner watch has priority at this free boundary");
                        return Ok(REFUSED_EXIT_CODE);
                    }
                }
                AcquireToken::YieldUnknown { reason } => {
                    eprintln!(
                        "REFUSED: validate yield state UNKNOWN: {reason}; absence is not permission"
                    );
                    return Ok(REFUSED_EXIT_CODE);
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
            // No payload Child exists until this acquisition succeeds.
            reap_exited_children();
            thread::sleep(Duration::from_secs(POLL_SECONDS));
        }
    }

    #[cfg(test)]
    fn try_acquire(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        hold: u64,
    ) -> Result<AcquireToken, ValidateLockError> {
        self.try_acquire_ordered(agent, kind, target, hold, true)
    }

    fn try_acquire_after_global_turn(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        hold: u64,
    ) -> Result<AcquireToken, ValidateLockError> {
        self.try_acquire_ordered(agent, kind, target, hold, false)
    }

    fn try_acquire_ordered(
        &self,
        agent: &str,
        kind: Kind,
        target: &str,
        hold: u64,
        honor_repository_fifo: bool,
    ) -> Result<AcquireToken, ValidateLockError> {
        let now = epoch_seconds()?;
        let holder = self.read_holder()?;
        self.require_no_cleanup(holder.as_ref())?;
        let mut queue = self.read_queue()?;
        if honor_repository_fifo && !queue.iter().any(|entry| entry.agent == agent) {
            queue.push(QueueEntry {
                enqueued_at: now,
                agent: agent.to_string(),
                target: target.to_string(),
            });
        }
        let cutoff = now - (2 * DEFAULT_WAIT_SECONDS) as i64;
        queue.retain(|entry| entry.enqueued_at >= cutoff);
        let my_index = if honor_repository_fifo {
            queue
                .iter()
                .position(|entry| entry.agent == agent)
                .unwrap_or(0)
        } else {
            0
        };

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
                    kind: holder.kind.clone(),
                    target: holder.target.clone(),
                    seconds_left: holder.expires_at.saturating_sub(now).max(0),
                    ahead: my_index,
                });
            }
        }

        // Only an actually free boundary is subject to the owner-watch yield.
        // A dead holder follows the existing evidence-reclaim path instead of
        // being laundered into a normal release. Yielders remove themselves from
        // FIFO so they cannot remain ahead of the watch while claiming to wait.
        if holder.is_none() {
            let status = self.yield_status_for_agent(agent, now);
            match status.state {
                YieldState::Allow => {}
                YieldState::Yield => {
                    queue.retain(|entry| entry.agent != agent);
                    self.write_queue(&queue)?;
                    return Ok(AcquireToken::Yield {
                        reason: status.detail,
                        seconds_left: status.remaining_seconds.unwrap_or(0),
                    });
                }
                YieldState::Unknown => {
                    queue.retain(|entry| entry.agent != agent);
                    self.write_queue(&queue)?;
                    return Ok(AcquireToken::YieldUnknown {
                        reason: status.detail,
                    });
                }
            }
        }

        // The watch is the priority acquirer at a yielded free boundary. This
        // is queue ordering only — never signalling, reclaiming, or preemption.
        if honor_repository_fifo && agent != OWNER_WATCH_AGENT {
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
        }

        let reclaimed = holder.map(|holder| match clear_decision {
            Some(LeaseDecision::Clear { evidence }) => {
                format!(
                    "{} ({})",
                    holder.agent,
                    render_supervisor_clear_evidence(&evidence)
                )
            }
            _ => holder.agent,
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
            // Publish the transition before removing the lease, under the same
            // guard. If this write fails, the lock stays held: UNKNOWN never
            // becomes a silently free/admissible boundary.
            self.write_release_record(&holder.agent, &holder.kind, &holder.target, None)?;
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
        let mut statuses = Vec::new();
        for slot in 0..VALIDATE_SLOT_COUNT {
            let lock = ValidateLock {
                paths: self.paths.for_slot(slot),
            };
            let status = lock.with_guard(|| lock.authority_status_guarded())?;
            let mut value = serde_json::to_value(&status).map_err(|error| {
                ValidateLockError::InvalidState(format!(
                    "cannot serialize canonical authority slot {slot}: {error}"
                ))
            })?;
            value
                .as_object_mut()
                .expect("AuthorityStatus serializes as an object")
                .insert("slot".into(), serde_json::json!(slot));
            statuses.push((status, value));
        }
        let primary = statuses
            .iter()
            .find(|(status, _)| status.admissible)
            .or_else(|| statuses.first())
            .ok_or_else(|| ValidateLockError::InvalidState("no validation slots".into()))?;
        let mut output = primary.1.clone();
        output
            .as_object_mut()
            .expect("authority output serializes as an object")
            .insert(
                "authorities".into(),
                serde_json::Value::Array(statuses.into_iter().map(|(_, value)| value).collect()),
            );
        let json = serde_json::to_string(&output).map_err(|error| {
            ValidateLockError::InvalidState(format!(
                "cannot serialize canonical authority status: {error}"
            ))
        })?;
        println!("{json}");
        Ok(())
    }

    fn print_yield_status(&self) -> Result<i32, ValidateLockError> {
        let now = epoch_seconds()?;
        let status = self.with_guard(|| Ok(self.yield_status_guarded(now)))?;
        let exit = match status.state {
            YieldState::Allow => 0,
            YieldState::Yield => 1,
            YieldState::Unknown => 2,
        };
        let json = serde_json::to_string(&status).map_err(|error| {
            ValidateLockError::InvalidState(format!(
                "cannot serialize validate-yield status: {error}"
            ))
        })?;
        println!("{json}");
        Ok(exit)
    }

    fn yield_status_for_agent(&self, agent: &str, now: i64) -> YieldStatus {
        if agent == OWNER_WATCH_AGENT {
            return YieldStatus {
                schema_version: 1,
                state: YieldState::Allow,
                admissible: true,
                reason_code: "owner-watch-priority".into(),
                detail: "owner checkout watch is the priority acquirer".into(),
                observed_at: now,
                remaining_seconds: None,
                release: self.read_release_record().ok().flatten(),
                owner_watch_admission: None,
            };
        }
        self.yield_status_guarded(now)
    }

    fn yield_status_guarded(&self, now: i64) -> YieldStatus {
        let release = match self.read_release_record() {
            Ok(Some(record)) => record,
            Ok(None) => {
                return YieldStatus {
                    schema_version: 1,
                    state: YieldState::Allow,
                    admissible: true,
                    reason_code: "bootstrap-no-release-yet".into(),
                    detail: format!(
                        "{} is absent: no release boundary has occurred yet, so there is nothing for the owner watch to be starved behind",
                        self.paths.released.display()
                    ),
                    observed_at: now,
                    remaining_seconds: None,
                    release: None,
                    owner_watch_admission: None,
                };
            }
            Err(error) => {
                return YieldStatus {
                    schema_version: 1,
                    state: YieldState::Unknown,
                    admissible: false,
                    reason_code: "release-record-unreadable".into(),
                    detail: error.to_string(),
                    observed_at: now,
                    remaining_seconds: None,
                    release: None,
                    owner_watch_admission: None,
                };
            }
        };

        if release.released_at > now {
            return YieldStatus {
                schema_version: 1,
                state: YieldState::Unknown,
                admissible: false,
                reason_code: "release-clock-in-future".into(),
                detail: format!(
                    "release timestamp {} is later than observed time {now}",
                    release.released_at
                ),
                observed_at: now,
                remaining_seconds: None,
                release: Some(release),
                owner_watch_admission: None,
            };
        }

        // The lock itself is the strongest possible admission witness: a
        // release by the exact owner-watch identity means it held this guarded
        // lease. Do not manufacture a short blind gap while its result writer
        // finishes the atomic JSON publication immediately after release.
        if release.released_by == OWNER_WATCH_AGENT {
            return YieldStatus {
                schema_version: 1,
                state: YieldState::Allow,
                admissible: true,
                reason_code: "owner-watch-released-last".into(),
                detail: "the most recent holder was the owner checkout watch".into(),
                observed_at: now,
                remaining_seconds: None,
                release: Some(release),
                owner_watch_admission: None,
            };
        }

        let admission = match self.owner_watch_admission_after(release.released_at) {
            Ok(admission) => admission,
            Err(detail) => {
                return YieldStatus {
                    schema_version: 1,
                    state: YieldState::Unknown,
                    admissible: false,
                    reason_code: "owner-watch-results-unreadable".into(),
                    detail,
                    observed_at: now,
                    remaining_seconds: None,
                    release: Some(release),
                    owner_watch_admission: None,
                };
            }
        };
        if let Some(admission) = admission {
            return YieldStatus {
                schema_version: 1,
                state: YieldState::Allow,
                admissible: true,
                reason_code: "owner-watch-admitted-since-release".into(),
                detail: "public owner-watch verifier observed an exact validate-lock admission after the release boundary".into(),
                observed_at: now,
                remaining_seconds: None,
                release: Some(release),
                owner_watch_admission: Some(admission),
            };
        }

        let age = now - release.released_at;
        if age < OWNER_WATCH_YIELD_SECONDS {
            let remaining = OWNER_WATCH_YIELD_SECONDS - age;
            YieldStatus {
                schema_version: 1,
                state: YieldState::Yield,
                admissible: false,
                reason_code: "owner-watch-not-admitted-since-release".into(),
                detail: format!(
                    "no durable owner-watch admission since release by {}; yield {remaining}s",
                    release.released_by
                ),
                observed_at: now,
                remaining_seconds: Some(remaining),
                release: Some(release),
                owner_watch_admission: None,
            }
        } else {
            YieldStatus {
                schema_version: 1,
                state: YieldState::Allow,
                admissible: true,
                reason_code: "yield-window-elapsed".into(),
                detail: format!(
                    "no owner-watch admission was observed, but the bounded {}s yield window elapsed",
                    OWNER_WATCH_YIELD_SECONDS
                ),
                observed_at: now,
                remaining_seconds: Some(0),
                release: Some(release),
                owner_watch_admission: None,
            }
        }
    }

    fn owner_watch_admission_after(
        &self,
        released_at: i64,
    ) -> Result<Option<OwnerWatchAdmission>, String> {
        let verifier = self
            .paths
            .owner_watch_verifier
            .as_ref()
            .ok_or_else(|| "owner-watch admission verifier path is unavailable".to_string())?;
        let state_dir = self.paths.owner_watch_state_dir.as_ref().ok_or_else(|| {
            "HOME is unavailable, so the owner-watch state directory cannot be derived".to_string()
        })?;
        let output = Command::new("python3")
            .arg(verifier)
            .arg("--state-dir")
            .arg(state_dir)
            .arg("--admission-status-since")
            .arg(released_at.to_string())
            .bounded_output()
            .map_err(|error| {
                format!(
                    "cannot execute owner-watch admission verifier {}: {error}",
                    verifier.display()
                )
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let value: serde_json::Value = serde_json::from_str(&stdout).map_err(|error| {
            format!(
                "owner-watch admission verifier returned invalid JSON (rc={:?}): {error}; stdout={stdout:?}; stderr={stderr:?}",
                output.status.code()
            )
        })?;
        let state = value
            .get("state")
            .and_then(|field| field.as_str())
            .ok_or_else(|| "owner-watch admission verifier JSON has no state".to_string())?;
        match (output.status.code(), state) {
            (Some(0), "ADMITTED") => Ok(Some(OwnerWatchAdmission {
                state: state.to_string(),
                verifier_output: stdout,
            })),
            (Some(1), "NOT_ADMITTED") => Ok(None),
            (Some(2), "UNKNOWN") => Err(format!(
                "owner-watch admission verifier returned UNKNOWN: {stdout}"
            )),
            (code, _) => Err(format!(
                "owner-watch admission verifier contract mismatch rc={code:?} state={state:?}; stderr={stderr:?}"
            )),
        }
    }

    /// Wait outside the box anchor and outside FIFO. This is what makes the
    /// priority executable: a yielding background caller consumes no resource
    /// that the owner watch needs in order to take its natural free boundary.
    fn wait_for_owner_watch_yield(
        &self,
        agent: &str,
        wait: u64,
        no_wait: bool,
    ) -> Result<i32, ValidateLockError> {
        if agent == OWNER_WATCH_AGENT {
            return Ok(0);
        }
        let started = Instant::now();
        let mut last_reason = String::new();
        loop {
            let now = epoch_seconds()?;
            let status = self.with_guard(|| Ok(self.yield_status_for_agent(agent, now)))?;
            match status.state {
                YieldState::Allow => return Ok(0),
                YieldState::Unknown => {
                    eprintln!(
                        "REFUSED: validate yield state UNKNOWN ({}): {}; absence is not permission",
                        status.reason_code, status.detail
                    );
                    return Ok(REFUSED_EXIT_CODE);
                }
                YieldState::Yield => {
                    if status.reason_code != last_reason {
                        eprintln!("validate-lock: YIELD for owner watch: {}", status.detail);
                        last_reason = status.reason_code;
                    }
                    if no_wait || wait == 0 {
                        eprintln!("REFUSED: owner watch has priority at this free boundary");
                        return Ok(REFUSED_EXIT_CODE);
                    }
                    if started.elapsed() >= Duration::from_secs(wait) {
                        eprintln!(
                            "validate-lock: TIMEOUT after {wait}s while yielding to owner watch"
                        );
                        return Ok(1);
                    }
                    // No payload Child exists while yielding outside the box
                    // anchor, so adopted helper exits are safe to reap here.
                    reap_exited_children();
                    thread::sleep(Duration::from_secs(POLL_SECONDS));
                }
            }
        }
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
        // Only validation kinds may reach the common holder checks below. A
        // frozen holder is valid for executing its exact-target run, but is
        // classified as noncanonical only after its host and owner identity
        // have passed the same checks as ordinary validation.
        let holder_kind = Kind::parse(&holder_record.kind).ok();
        if !holder_kind.is_some_and(Kind::is_validation) {
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
        if holder_kind == Some(Kind::FrozenValidate) {
            return Ok(AuthorityStatus::admitted_noncanonical(
                "canonical-holder-kind-not-validate",
                holder,
                owner,
                cleanup_state,
            ));
        }
        Ok(AuthorityStatus::admitted(holder, owner, cleanup_state))
    }

    fn status(&self, tool_root: &Path) -> Result<(), ValidateLockError> {
        match observe_global_validate_anchor(tool_root, &self.paths.anchor) {
            GlobalValidateObservation::Held(holder) => {
                println!("HELD (box-global authority):");
                println!("  agent={}", holder.agent);
                println!("  kind={}", holder.kind);
                println!("  target={}", holder.target);
                println!("  host={}", holder.host);
                println!("  owner_pid={}", holder.pid);
                println!("  owner_boot_id={}", holder.boot_id);
                println!("  owner_start_ticks={}", holder.start_ticks);
                println!("  unit={}", holder.unit);
                println!("  record={}", holder.record.display());
                println!("  cgroup={}", holder.cgroup);
                self.print_queue()?;
                return Ok(());
            }
            GlobalValidateObservation::Unknown(reason) => {
                println!("UNKNOWN (box-global authority):");
                println!("  reason={reason}");
                println!("  acquirable=false");
                self.print_queue()?;
                return Ok(());
            }
            GlobalValidateObservation::Free => {}
        }

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
                println!("QUARANTINED (payload domain lacks final census):");
                print_cleanup_record(&record, &reason);
                print_census_recovery(&record, "ci-hub validate-lock");
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
            println!(
                "  validation_supervisor_process={}",
                render_supervisor_liveness(&liveness)
            );
        } else {
            match holder {
                Some(holder)
                    if holder.live_at(now) && matches!(liveness, OwnerLiveness::Dead(_)) =>
                {
                    println!("VALIDATION SUPERVISOR EXITED (reclaimable):");
                    for line in holder.render().lines() {
                        println!("  {line}");
                    }
                    print_owner_identity(process_owner.as_ref());
                    println!(
                        "  validation_supervisor_process={}",
                        render_supervisor_liveness(&liveness)
                    );
                    println!("  secs_left={}", holder.expires_at - now);
                }
                Some(holder) if holder.live_at(now) => {
                    println!("HELD:");
                    for line in holder.render().lines() {
                        println!("  {line}");
                    }
                    print_owner_identity(process_owner.as_ref());
                    println!(
                        "  validation_supervisor_process={}",
                        render_supervisor_liveness(&liveness)
                    );
                    println!("  secs_left={}", holder.expires_at - now);
                }
                Some(holder) => {
                    println!("LAPSED (reclaimable):");
                    for line in holder.render().lines() {
                        println!("  {line}");
                    }
                    print_owner_identity(process_owner.as_ref());
                    println!(
                        "  validation_supervisor_process={}",
                        render_supervisor_liveness(&liveness)
                    );
                }
                None => println!("FREE"),
            }
        }
        self.print_queue()
    }

    fn print_queue(&self) -> Result<(), ValidateLockError> {
        let now = epoch_seconds()?;
        let global = self.with_global_queue_guard(|| {
            let mut queue = self.read_global_queue()?;
            let original_len = queue.len();
            // Status is a queue consumer too. Apply the declaration TTL here
            // without probing active-owner liveness, so an expired declaration
            // cannot remain visible forever merely because no mutating caller
            // arrives, while an unverifiable active waiter stays represented.
            queue.retain(|entry| {
                entry.owner.is_some()
                    || entry.enqueued_at >= now - GLOBAL_QUEUE_DECLARATION_TTL_SECONDS
            });
            if queue.len() != original_len {
                self.write_global_queue(&queue)?;
            }
            Ok(queue)
        })?;
        if !global.is_empty() {
            println!("global priority queue:");
            let mut active_position = 0;
            for entry in &global {
                match &entry.owner {
                    Some(owner) => {
                        active_position += 1;
                        println!(
                            "  {:>6}\tpriority={} agent={} kind={} target={} owner={}:{}:{}",
                            active_position,
                            entry.priority.as_str(),
                            entry.agent,
                            entry.kind.as_str(),
                            entry.target,
                            owner.pid,
                            owner.boot_id,
                            owner.start_ticks
                        );
                    }
                    None => println!(
                        "  declared\tpriority={} agent={} kind={} target={}",
                        entry.priority.as_str(),
                        entry.agent,
                        entry.kind.as_str(),
                        entry.target
                    ),
                }
            }
        }
        let queue = self.read_queue()?;
        if !queue.is_empty() {
            println!("repository compatibility queue (FIFO):");
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
            let cleanup = self.cleanup_verification(Some(&holder));
            let attestation = match &cleanup {
                CleanupVerification::Recoverable { record, .. } => record.attestation.clone(),
                _ => None,
            };
            let now = epoch_seconds()?;
            match self.holder_stale_clear_decision(&holder, now, cleanup)? {
                LeaseDecision::Clear { evidence } => {
                    self.write_release_record(
                        &format!("reclaim-dead:{}", holder.agent),
                        &holder.kind,
                        &holder.target,
                        attestation.as_ref(),
                    )?;
                    remove_if_exists(&self.paths.lock)?;
                    remove_if_exists(&self.paths.owner)?;
                    remove_cleanup_record_and_claim(
                        &self.paths.cleanup,
                        &self.paths.global_anchor(),
                    )
                    .map_err(|source| io_error("remove", &self.paths.cleanup, source))?;
                    Ok(Some((holder.agent, holder.kind, holder.target, evidence)))
                }
                decision => Err(ValidateLockError::ReclaimNotProven(decision.to_string())),
            }
        })?;
        match reclaimed {
            Some((agent, kind, target, reason)) => eprintln!(
                "validate-lock: evidence-reclaimed holder_agent={agent} kind={kind} target={target}: {}",
                render_supervisor_clear_evidence(&reason)
            ),
            None => eprintln!("validate-lock: reclaim-dead: no lock held"),
        }
        Ok(0)
    }

    /// Take the residual census that a dead supervisor never got to take, so the
    /// existing `reclaim-dead` path can finish.
    ///
    /// WHY THIS EXISTS. `capture_and_freeze_residuals` enumerates the payload
    /// domain with `scan_descendants(supervisor_pid)`, floored at the payload
    /// leader's start time — a PPID-tree walk that is only exhaustive because
    /// `run` made the supervisor a `PR_SET_CHILD_SUBREAPER`, so even a `setsid`'d
    /// double-fork reparents back to it. That same subreaper status also adopts
    /// unrelated orphans, which is what the start-time floor excludes; see
    /// `capture_and_freeze_residuals` for the measured incident.
    /// When the supervisor itself dies (e.g. systemd TERMs the whole unit
    /// cgroup) that anchor is destroyed: survivors reparent to `systemd --user`
    /// or pid 1 and NO ppid walk can reconstruct the domain. So `Published` ->
    /// `Uncensused` became a one-way door — `reclaim_dead` refuses `Uncensused`
    /// before it ever checks owner liveness, and the only writer of a complete
    /// census lives inside `run`. The box stayed refused for every agent.
    ///
    /// WHAT IS AND IS NOT ATTESTABLE. Everything the kernel can still answer is
    /// checked mechanically here and REFUSES on failure — the restated identity,
    /// a proven-dead supervisor, the leader's exact pid+start_ticks absence, and
    /// the recorded cleanup domain. None may be attested away. Version-4 records
    /// publish the payload-owned process group, so an absent group is mechanical
    /// evidence and a reused/live group fails closed as populated. Version-3
    /// cgroups remain readable; only version-2 records lack a domain and retain
    /// the explicit, auditable attestation path.
    fn census_orphaned_domain(
        &self,
        args: CensusOrphanedDomainArgs,
    ) -> Result<i32, ValidateLockError> {
        let (subject, pgid, disposition) = self.with_guard(|| {
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

            let (leader, pgid, residuals, incomplete_residual) = match &record.phase {
                CleanupPhase::Published { leader, pgid }
                | CleanupPhase::CensusPending { leader, pgid } => {
                    let restated = args.leader.as_deref().ok_or_else(|| {
                        ValidateLockError::RecoveryNotProven(
                            "--leader is required for a published cleanup record".into(),
                        )
                    })?;
                    let restated = parse_leader_identity(restated)?;
                    if *leader != restated {
                        return Err(ValidateLockError::RecoveryNotProven(format!(
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
                        return Err(ValidateLockError::RecoveryNotProven(
                            "--leader must be omitted for an incomplete residual record, which \
                             has no recorded leader identity"
                                .into(),
                        ));
                    }
                    (None, *pgid, residuals.clone(), true)
                }
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
            if let Some(leader) = &leader {
                match exact_process_liveness(leader) {
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
            }
            for identity in &residuals {
                match exact_process_liveness(identity) {
                    Ok(false) => {}
                    Ok(true) => {
                        return Err(ValidateLockError::RecoveryNotProven(format!(
                            "recorded residual {}:{} is ALIVE",
                            identity.pid, identity.start_ticks
                        )))
                    }
                    Err(reason) => {
                        return Err(ValidateLockError::RecoveryNotProven(format!(
                            "cannot verify recorded residual absence: {reason}"
                        )))
                    }
                }
            }
            if process_group_exists(pgid) {
                return Err(ValidateLockError::RecoveryNotProven(format!(
                    "recorded process group {pgid} is still populated"
                )));
            }

            // (5) The domain itself. A recorded process group or readable legacy
            // cgroup is mechanical and consumes NO attestation; only a legacy
            // record without a domain uses operator evidence.
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
            .map_err(ValidateLockError::RecoveryNotProven)?;

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
                .map_err(ValidateLockError::RecoveryNotProven)?;
            write_cleanup_record(&self.paths.cleanup, &recovered)
                .map_err(|source| io_error("write", &self.paths.cleanup, source))?;
            let subject = match leader {
                Some(leader) => format!("leader={}:{}", leader.pid, leader.start_ticks),
                None => {
                    format!("incomplete-residual recorded_identities={recorded_residual_count}")
                }
            };
            Ok((subject, pgid, disposition))
        })?;
        // Truthful about WHICH evidence discharged this: a mechanically proven
        // census and an operator attestation are not the same claim, and a
        // reader must be able to tell them apart in the transcript.
        eprintln!(
            "validate-lock: CENSUSED orphaned domain agent={} operation={} {subject} pgid={pgid}; \
             supervisor and every recorded payload identity proven absent, {}",
            args.agent, args.operation, disposition
        );
        eprintln!(
            "validate-lock: quarantine is now RECOVERABLE; run `validate-lock reclaim-dead` to \
             release the box."
        );
        Ok(0)
    }

    #[cfg(test)]
    fn run(&self, args: RunArgs, root: &Path) -> Result<i32, ValidateLockError> {
        self.run_with_roots(args, root, root)
    }

    fn run_with_roots(
        &self,
        args: RunArgs,
        tool_root: &Path,
        state_root: &Path,
    ) -> Result<i32, ValidateLockError> {
        // The run record is the shared producer-consumer contract for
        // admission. Verify it and bind this service's process identity before
        // validating setup, so every attributable pre-admission exit can publish
        // a typed refusal instead of leaving an ambiguous historical handle.
        let attribution = require_run_attribution(tool_root, state_root, &args)?;
        let owner = prepare_run_before_admission(tool_root, &args, &attribution)?;

        // Refuse immutable-floor and current-main violations BEFORE this run
        // enters the box-global queue. A stale head must not wait for scarce
        // validation time it can never consume usefully.
        if let Err(error) =
            base_admission_check(tool_root, &args.target, args.kind, args.skip_base_check)
        {
            return stale_base_refusal_with_record(tool_root, &attribution, error);
        }
        if let Err(error) = current_main_freshness_check(state_root, &args.target, args.kind) {
            return stale_base_refusal_with_record(tool_root, &attribution, error);
        }

        let record_text = attribution.record.to_string_lossy();
        for (name, value) in [
            ("boot_id", owner.boot_id.as_str()),
            ("agent", args.agent.as_str()),
            ("kind", args.kind.as_str()),
            ("target", args.target.as_str()),
            ("unit", attribution.unit.as_str()),
            ("record", record_text.as_ref()),
        ] {
            anchor_token(name, value)?;
        }
        let anchor_attribution = format!(
            "boot_id={} start_ticks={} agent={} kind={} target={} unit={} record={}",
            owner.boot_id,
            owner.start_ticks,
            args.agent,
            args.kind.as_str(),
            args.target,
            attribution.unit,
            record_text,
        );

        // Yield BEFORE taking the box-global anchor. A background waiter that
        // holds the anchor while sleeping would block the very watch this rule
        // exists to prioritize.
        let yield_status = self.wait_for_owner_watch_yield(&args.agent, args.wait, args.no_wait)?;
        if yield_status != 0 {
            return refusal_before_admission(tool_root, &attribution, yield_status);
        }

        // BOX-GLOBAL PRIORITY QUEUE, THEN ANCHOR, BOTH BEFORE REPOSITORY STATE.
        //
        // The old order took the anchor first and reached a repository-local FIFO
        // second. Independent clones therefore raced for the flock and timing chose
        // the winner. The global queue guard now admits exactly its declared head
        // to try the anchor. `_box_anchor` still lives until `run` returns; queue
        // ordering never releases, signals, or preempts it.
        //
        // RESIDUE, stated rather than papered over: the flock spans the SUPERVISOR,
        // so an escaped payload that outlives its supervisor releases the anchor
        // early. Within a repository that case is still covered by the durable
        // cleanup quarantine, which is unchanged and still consulted below; ACROSS
        // repositories it is not. Closing that needs a box-global durable record,
        // which is a different change from this one.
        let (mut box_anchors, mut slot) = match self.wait_for_global_turn(
            &args,
            &owner,
            &anchor_attribution,
            false,
            state_root,
        )? {
            Some(reservation) => reservation,
            None => {
                return refusal_before_admission(tool_root, &attribution, REFUSED_EXIT_CODE);
            }
        };
        loop {
            let held = box_anchors
                .iter()
                .map(|anchor| anchor.path().display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!("validate-lock: validation slot anchor(s) held at {held}");

            // The anchor wait may have spanned a new background release. Keep
            // this exact waiter in the global queue until the boundary is
            // settled. On YIELD, drop only the anchor: the owner-watch agent
            // ranks ahead of every queued priority and can acquire naturally,
            // while this caller retains its position instead of being disposed
            // and sent to the tail. UNKNOWN still refuses fail-closed.
            let now = epoch_seconds()?;
            let boundary = self.with_guard(|| Ok(self.yield_status_for_agent(&args.agent, now)))?;
            match boundary.state {
                YieldState::Allow => {
                    // `origin/main` may have advanced during the queue wait.
                    // Re-fetch at the last boundary before this waiter is
                    // removed and the repository lease is acquired. The
                    // product driver's local-ref check remains a defensive
                    // assertion against any later race.
                    run_current_main_between_checks_hook();
                    if let Err(error) =
                        current_main_freshness_check(state_root, &args.target, args.kind)
                    {
                        drop(box_anchors);
                        self.remove_own_global_waiter(&args, &owner)?;
                        return stale_base_refusal_with_record(tool_root, &attribution, error);
                    }
                    self.remove_own_global_waiter(&args, &owner)?;
                    break;
                }
                YieldState::Unknown => {
                    drop(box_anchors);
                    self.remove_own_global_waiter(&args, &owner)?;
                    eprintln!(
                        "REFUSED: validate yield state UNKNOWN ({}): {}; absence is not permission",
                        boundary.reason_code, boundary.detail
                    );
                    return refusal_before_admission(tool_root, &attribution, REFUSED_EXIT_CODE);
                }
                YieldState::Yield => {
                    eprintln!("validate-lock: YIELD for owner watch: {}", boundary.detail);
                    drop(box_anchors);
                    let yield_status =
                        self.wait_for_owner_watch_yield(&args.agent, args.wait, args.no_wait)?;
                    if yield_status != 0 {
                        self.remove_own_global_waiter(&args, &owner)?;
                        return refusal_before_admission(tool_root, &attribution, yield_status);
                    }
                    (box_anchors, slot) = match self.wait_for_global_turn(
                        &args,
                        &owner,
                        &anchor_attribution,
                        false,
                        state_root,
                    )? {
                        Some(reservation) => reservation,
                        None => {
                            return refusal_before_admission(
                                tool_root,
                                &attribution,
                                REFUSED_EXIT_CODE,
                            )
                        }
                    };
                }
            }
        }
        let active = ValidateLock {
            paths: self.paths.for_slot(slot),
        };
        let _box_anchors = box_anchors;

        // Durable half of box exclusion, checked WITH the anchor held so the
        // answer cannot change under us. The anchor proves no LIVE supervisor
        // holds the box; this proves no DEAD supervisor left a live payload
        // behind in some other repository.
        let foreign = if args.kind == Kind::Bench {
            foreign_box_claim(&active.paths.global_anchor(), &active.paths.cleanup)
        } else {
            foreign_box_quarantine(&active.paths.global_anchor(), &active.paths.cleanup)
        };
        if let Some(reason) = foreign {
            eprintln!("REFUSED: {reason}");
            return refusal_before_admission(tool_root, &attribution, REFUSED_EXIT_CODE);
        }

        // Admission: block FIFO, or refuse immediately under --no-wait. Never
        // silently admit more holders than the selected validation slots allow.
        if args.no_wait {
            let token = active.with_guard(|| {
                active.try_acquire_after_global_turn(
                    &args.agent,
                    args.kind,
                    &args.target,
                    args.hold,
                )
            })?;
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
                AcquireToken::Yield {
                    reason,
                    seconds_left,
                } => {
                    eprintln!(
                        "REFUSED: owner watch has priority at this free boundary (~{seconds_left}s): {reason}"
                    );
                    return refusal_before_admission(tool_root, &attribution, REFUSED_EXIT_CODE);
                }
                AcquireToken::YieldUnknown { reason } => {
                    eprintln!(
                        "REFUSED: validate yield state UNKNOWN: {reason}; absence is not permission"
                    );
                    return refusal_before_admission(tool_root, &attribution, REFUSED_EXIT_CODE);
                }
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
                    return refusal_before_admission(tool_root, &attribution, REFUSED_EXIT_CODE);
                }
                AcquireToken::WaitTurn { head, ahead } => {
                    eprintln!(
                        "REFUSED: box-exclusive lock free but {head} is ahead of {} in the FIFO queue, ~0s left; {ahead} ahead in queue",
                        args.agent
                    );
                    return refusal_before_admission(tool_root, &attribution, REFUSED_EXIT_CODE);
                }
            }
        } else {
            let acquire_status = active.acquire_after_global_turn(
                &args.agent,
                args.kind,
                &args.target,
                args.wait,
                args.hold,
            )?;
            if acquire_status != 0 {
                return refusal_before_admission(tool_root, &attribution, acquire_status);
            }
        }

        if let Err(error) = active.with_guard(|| active.write_current_process_owner()) {
            let _ = active.release(&args.agent, true);
            return Err(error);
        }
        let admitted = match publish_run_admission_result(
            tool_root,
            &attribution,
            validate_run_handle::AdmissionState::Admitted,
            None,
            None,
        ) {
            Ok(admitted) => admitted,
            Err(error) => {
                let _ = active.release(&args.agent, true);
                return Err(error);
            }
        };
        let (run_number, branch) = match (args.kind.is_validation(), admitted.as_ref()) {
            (true, Some(projection)) => (
                projection
                    .admission_result
                    .as_ref()
                    .and_then(|result| result.run_number),
                projection.branch.as_deref(),
            ),
            _ => (None, None),
        };
        if let Some(run_number) = run_number {
            eprintln!(
                "validate-lock: RUN {run_number} assigned on {} at admission",
                current_host()
            );
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
        active.arm_run(&args.agent, args.kind, &args.target)?;
        #[cfg(test)]
        if crate::landing_lock::take_run_crash_hook(
            &format!("{}:{}", args.kind.as_str(), args.target),
            crate::landing_lock::RunCrashPoint::AfterArm,
        ) {
            return Err(ValidateLockError::CleanupQuarantined(
                "test-injected supervisor crash after arm".into(),
            ));
        }
        let owner_file = active.paths.owner.clone();
        let mut gated = match spawn_gated_child(&args.child[0], &args.child[1..], |command| {
            // The child may record `concurrent_validates=0` only when it can
            // prove it is descended from this live, process-bound lock owner.
            command
                .env(
                    "CI_HUB_VALIDATE_LOCK_OWNER_PID",
                    std::process::id().to_string(),
                )
                .env("CI_HUB_VALIDATE_LOCK_OWNER_FILE", &owner_file);
            apply_admission_provenance(command, run_number, branch);
        }) {
            Ok(gated) => gated,
            Err(source) => {
                active.clear_unstarted_run(&args.agent, args.kind, &args.target)?;
                active.release(&args.agent, true)?;
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
        active.publish_run(&args.agent, args.kind, &args.target, &gated)?;
        gated.release().map_err(|source| {
            io_error("release child start gate at", &active.paths.cleanup, source)
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
        let heartbeat_paths = active.paths.clone();
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
        active.begin_run_census(&args.agent, args.kind, &args.target, &gated)?;
        let _ = stop_tx.send(());
        let _ = heartbeat.join();
        reap_exited_children();
        let pgid = outcome.pgid();
        let capture = settle_empty_payload_domain(
            std::process::id(),
            pgid,
            &gated.leader,
            &gated.domain_token,
            capture_and_freeze_residuals(
                std::process::id(),
                gated.leader.start_ticks,
                &gated.domain_token,
            ),
        );
        match active.finalize_run_census(
            &args.agent,
            args.kind,
            &args.target,
            &gated.leader,
            pgid,
            capture,
        )? {
            RunCleanupFinalization::Released => {}
            RunCleanupFinalization::Quarantined {
                residual_count,
                domain_complete,
                group,
            } => {
                let group = match group {
                    OriginalProcessGroupState::Absent(reason) => {
                        format!("original group absent ({reason})")
                    }
                    OriginalProcessGroupState::Active(reason) => {
                        format!("original group active ({reason})")
                    }
                    OriginalProcessGroupState::Unknown(reason) => {
                        format!("original group UNKNOWN ({reason})")
                    }
                };
                return Err(ValidateLockError::CleanupQuarantined(format!(
                    "completed process group {pgid} did not yield a proven-empty payload domain after a bounded {}s settle; domain_complete={domain_complete}; {residual_count} exact residual identities recorded; {group}; lock retained",
                    EMPTY_DOMAIN_SETTLE_SECONDS
                )));
            }
        }
        active.release(&args.agent, true)?;
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

    fn read_release_record(&self) -> Result<Option<ReleaseRecord>, ValidateLockError> {
        if !self.paths.released.exists() {
            return Ok(None);
        }
        let content = read_to_string(&self.paths.released)?;
        Ok(Some(ReleaseRecord::parse(&content)?))
    }

    fn write_release_record(
        &self,
        released_by: &str,
        kind: &str,
        target: &str,
        attestation: Option<&DomainAttestation>,
    ) -> Result<ReleaseRecord, ValidateLockError> {
        let released_at = epoch_seconds()?;
        let released_human = Local
            .timestamp_opt(released_at, 0)
            .single()
            .ok_or_else(|| {
                ValidateLockError::InvalidState("release timestamp is out of range".into())
            })?
            .to_rfc3339();
        let record = ReleaseRecord {
            schema_version: if attestation.is_some() { 2 } else { 1 },
            released_at,
            released_human,
            released_by: released_by.to_string(),
            kind: kind.to_string(),
            target: target.to_string(),
            attested_by: attestation.map(|value| value.attester.clone()),
            attestation_evidence: attestation.map(|value| value.evidence.clone()),
        };
        self.with_global_queue_guard(|| {
            let temporary = suffix(
                &self.paths.released,
                &format!(".tmp-{}-{released_at}", std::process::id()),
            );
            write_truncated(&temporary, record.render().as_bytes())?;
            fs::rename(&temporary, &self.paths.released).map_err(|source| {
                io_error("replace release record", &self.paths.released, source)
            })?;
            Ok(())
        })?;
        Ok(record)
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

fn render_supervisor_liveness(liveness: &OwnerLiveness) -> String {
    match liveness {
        OwnerLiveness::Alive => "alive".into(),
        OwnerLiveness::Dead(reason) => format!("exited:{reason}"),
        OwnerLiveness::Unknown(reason) => format!("unknown:{reason}"),
    }
}

fn render_supervisor_clear_evidence(evidence: &str) -> String {
    match evidence.strip_prefix("owner dead: ") {
        Some(rest) => format!("validation supervisor exited: {rest}"),
        None => evidence.to_string(),
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

/// Wait for `child`, reaping adopted exits while it is live and killing its
/// process group if it runs longer than `deadline_secs`. Copied (not reused)
/// from landing_lock so the deadline-kill messages carry the `validate-lock:`
/// prefix; `signal_group` itself is shared.
fn supervise_child(child: &mut Child, deadline_secs: u64, target: &str) -> ChildOutcome {
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
                eprintln!("validate-lock: cannot wait on child: {source}");
                return ChildOutcome::Uncertain {
                    pgid: child.id(),
                    reason: source.to_string(),
                };
            }
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

    #[test]
    fn admission_forwards_recorded_branch_and_clears_an_unrecorded_one() {
        let mut attached = Command::new("/bin/true");
        apply_admission_provenance(
            &mut attached,
            Some(701),
            Some("codex/validate-branch-provenance"),
        );
        assert_eq!(
            attached
                .get_envs()
                .find(|(name, _)| *name == VALIDATE_BRANCH_ENV)
                .and_then(|(_, value)| value),
            Some(std::ffi::OsStr::new("codex/validate-branch-provenance"))
        );

        let mut detached = Command::new("/bin/true");
        detached.env(VALIDATE_BRANCH_ENV, "ambient-main");
        apply_admission_provenance(&mut detached, None, None);
        assert!(matches!(
            detached
                .get_envs()
                .find(|(name, _)| *name == VALIDATE_BRANCH_ENV),
            Some((_, None))
        ));
    }

    fn attribution_args(record: Option<PathBuf>) -> RunArgs {
        RunArgs {
            agent: "attributed-agent".into(),
            kind: Kind::Validate,
            target: HEX_SHA.into(),
            run_record: record,
            no_wait: true,
            wait: 0,
            hold: 30,
            child_deadline: 30,
            max: 1,
            skip_base_check: false,
            child: vec![OsString::from("/bin/true")],
        }
    }

    #[test]
    fn attributable_run_record_binds_agent_target_and_live_unit() {
        let root = env::temp_dir().join(format!(
            "ci-hub-attribution-positive-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        let runs = root.join("ignored/validate/runs");
        fs::create_dir_all(&runs).unwrap();
        let record = runs.join("validate-attributed.json");
        fs::write(
            &record,
            format!(
                "{{\"schema_version\":1,\"state\":\"launching\",\"agent\":\"attributed-agent\",\"target\":\"{HEX_SHA}\",\"unit\":\"validate-attributed.service\"}}\n"
            ),
        )
        .unwrap();
        let args = attribution_args(Some(record.clone()));
        let verified = verify_run_attribution(
            &root,
            &root,
            &args,
            "0::/user.slice/user-1.slice/validate-attributed.service\n",
        )
        .expect("exact pre-existing run identity must be attributable");
        assert_eq!(verified.unit, "validate-attributed.service");
        assert_eq!(verified.record, fs::canonicalize(record).unwrap());
        let owner = ProcessOwner {
            host: "fixture".into(),
            boot_id: "boot-fixture".into(),
            pid: 123,
            start_ticks: 456,
        };
        publish_run_process_identity(&root, &verified.record, &owner)
            .expect("complete identity must be published before anchor acquisition");
        let published: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&verified.record).unwrap()).unwrap();
        assert_eq!(
            published.get("process_identity"),
            Some(&serde_json::json!({
                "pid": 123,
                "start_ticks": 456,
                "boot_id": "boot-fixture",
            }))
        );
        assert_eq!(
            published.get("state").and_then(|v| v.as_str()),
            Some("launching")
        );
        let _ = fs::remove_dir_all(root);
    }

    fn write_run_handle_authority(root: &Path) {
        let authority_dir = root.join("ci-hub/validate");
        fs::create_dir_all(&authority_dir).unwrap();
        fs::write(
            authority_dir.join("run_registry.py"),
            include_str!("../validate/run_registry.py"),
        )
        .unwrap();
        fs::write(
            authority_dir.join("service_result.py"),
            include_str!("../validate/service_result.py"),
        )
        .unwrap();
        fs::write(
            authority_dir.join("final_validate_status.py"),
            include_str!("../validate/final_validate_status.py"),
        )
        .unwrap();
    }

    fn current_validation_record(name: &str) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "kind": "validate",
            "state": "launching",
            "agent": "attributed-agent",
            "target": HEX_SHA,
            "unit": format!("{name}.service"),
            "repo": "rrnewton/hermit",
            "branch": "codex/validate-branch-provenance",
            "checkout": "/tmp/checkout",
            "source_checkout": "/tmp/source",
            "temporary_checkout": true,
            "cargo_home": "/tmp/cargo",
            "log": "/tmp/log",
            "pr": null,
            "started_at": "2026-08-28T07:00:00Z",
            "parent_checkout_head": HEX_SHA,
            "producer": "ci-hub/validate/run_registry.py",
            "admission": "ci-hub validate-lock",
            "pane_role": "observer-only",
            "validate_lock_child_deadline_seconds": 3600,
            "e2e_result_root": "/tmp/e2e",
            "safe_ci_dag_runner_log_dir": "/tmp/dag",
            "hermit_run_timeout_seconds": 3300,
            "commit_status_publication": {
                "state": "not-attempted",
                "recorded_at": "2026-08-31T12:16:24Z",
                "repository": "rrnewton/hermit",
                "sha": HEX_SHA,
                "reason": "canonical verdict is not yet available"
            }
        })
    }

    fn current_validation_attribution_fixture(
        name: &str,
    ) -> (PathBuf, PathBuf, RunArgs, VerifiedRunAttribution) {
        let root = env::temp_dir().join(format!(
            "ci-hub-attribution-current-{name}-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        let runs = root.join("ignored/validate/runs");
        fs::create_dir_all(&runs).unwrap();
        write_run_handle_authority(&root);
        let record = runs.join(format!("{name}.json"));
        let current = current_validation_record(name);
        fs::write(&record, serde_json::to_vec(&current).unwrap()).unwrap();
        let args = attribution_args(Some(record.clone()));
        let cgroup = format!("0::/user.slice/user-1.slice/{name}.service\n");
        let verified = verify_run_attribution(&root, &root, &args, &cgroup)
            .expect("current run identity must be attributable through the shared contract");
        (root, record, args, verified)
    }

    #[test]
    fn validate_lock_uses_tool_authority_for_shared_current_handles() {
        let root = env::temp_dir().join(format!(
            "ci-hub-attribution-split-authority-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        let tool_root = root.join("tool");
        let state_root = root.join("state");
        write_run_handle_authority(&tool_root);
        let stale_authority_dir = state_root.join("ci-hub/validate");
        let runs = state_root.join("ignored/validate/runs");
        fs::create_dir_all(&stale_authority_dir).unwrap();
        fs::create_dir_all(&runs).unwrap();
        fs::write(
            stale_authority_dir.join("run_registry.py"),
            "import sys\nprint('run_registry: REFUSED: stale state-root authority executed', file=sys.stderr)\nraise SystemExit(2)\n",
        )
        .unwrap();

        let name = "validate-split-authority";
        let record = runs.join(format!("{name}.json"));
        let current = current_validation_record(name);
        fs::write(&record, serde_json::to_vec(&current).unwrap()).unwrap();
        let args = attribution_args(Some(record.clone()));
        let cgroup = format!("0::/user.slice/user-1.slice/{name}.service\n");
        let verified = verify_run_attribution(&tool_root, &state_root, &args, &cgroup)
            .expect("known fields must be read by the exact executing tool authority");
        let owner = ProcessOwner {
            host: "fixture".into(),
            boot_id: "boot-fixture".into(),
            pid: 123,
            start_ticks: 456,
        };
        publish_run_process_identity(&tool_root, &verified.record, &owner)
            .expect("the exact tool authority must bind process identity");
        let admitted = publish_run_admission_result(
            &tool_root,
            &verified,
            validate_run_handle::AdmissionState::Admitted,
            None,
            None,
        )
        .expect("the exact tool authority must publish admission")
        .expect("a current handle must return its projection");
        assert_eq!(
            admitted.branch.as_deref(),
            Some("codex/validate-branch-provenance")
        );

        let mut unknown = current;
        unknown["unexpected_contract_field"] = serde_json::json!(true);
        fs::write(&record, serde_json::to_vec(&unknown).unwrap()).unwrap();
        let error = verify_run_attribution(&tool_root, &state_root, &args, &cgroup)
            .expect_err("a truly unknown field must still refuse");
        assert!(
            error
                .to_string()
                .contains("unknown field(s) for kind 'validate': unexpected_contract_field"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn current_run_record_uses_shared_kind_and_state_contract() {
        let (root, record, args, verified) =
            current_validation_attribution_fixture("validate-attributed");
        let current: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&record).unwrap()).unwrap();
        let owner = ProcessOwner {
            host: "fixture".into(),
            boot_id: "boot-fixture".into(),
            pid: 123,
            start_ticks: 456,
        };
        publish_run_process_identity(&root, &verified.record, &owner).unwrap();
        let admitted = publish_run_admission_result(
            &root,
            &verified,
            validate_run_handle::AdmissionState::Admitted,
            None,
            None,
        )
        .unwrap()
        .expect("a real run record must return its admitted result");
        assert_eq!(
            Some(1),
            admitted
                .admission_result
                .as_ref()
                .and_then(|result| result.run_number)
        );
        assert_eq!(
            admitted.branch.as_deref(),
            Some("codex/validate-branch-provenance")
        );
        let published: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&record).unwrap()).unwrap();
        assert_eq!(published["process_identity"]["pid"], 123);
        assert_eq!(published["admission_result"]["run_number"], 1);

        let mut invalid = current;
        invalid["state"] = serde_json::json!("failed");
        fs::write(&record, serde_json::to_vec(&invalid).unwrap()).unwrap();
        let error = verify_run_attribution(
            &root,
            &root,
            &args,
            "0::/user.slice/user-1.slice/validate-attributed.service\n",
        )
        .expect_err("a bench-only state must not enter validation admission");
        assert!(
            error.to_string().contains("validation-run-handle-state"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pre_admission_setup_errors_publish_identity_and_typed_refusal() {
        for (name, bad_max, subreaper_error) in [
            ("validate-bad-max", true, false),
            ("validate-subreaper-error", false, true),
        ] {
            let (root, record, mut args, verified) = current_validation_attribution_fixture(name);
            if bad_max {
                args.max = 3;
            }
            let owner = ProcessOwner {
                host: "fixture".into(),
                boot_id: "boot-fixture".into(),
                pid: 123,
                start_ticks: 456,
            };
            let error =
                prepare_run_before_admission_with(&root, &args, &verified, Ok(owner), || {
                    if subreaper_error {
                        Err(io::Error::other("fixture subreaper failure"))
                    } else {
                        panic!("bad --max must refuse before subreaper setup")
                    }
                })
                .unwrap_err();
            if bad_max {
                assert!(matches!(error, ValidateLockError::BadMax));
            } else {
                assert!(matches!(error, ValidateLockError::Io { .. }));
            }
            let published: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&record).unwrap()).unwrap();
            assert_eq!(published["process_identity"]["pid"], 123);
            assert_eq!(published["admission_result"]["state"], "refused");
            assert_eq!(
                published["admission_result"]["reason"],
                "validation-lock-refusal"
            );
            assert_eq!(published["admission_result"]["exit_code"], 2);
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn unattributable_run_is_refused_before_anchor_inputs_exist() {
        let root = env::temp_dir().join(format!(
            "ci-hub-attribution-negative-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(root.join("ignored/validate/runs")).unwrap();
        let error = verify_run_attribution(
            &root,
            &root,
            &attribution_args(None),
            "0::/user.slice/validate-attributed.service\n",
        )
        .expect_err("missing run identity must refuse");
        assert!(error.to_string().contains("no --run-record"), "{error}");

        let record = root.join("ignored/validate/runs/validate-attributed.json");
        fs::write(
            &record,
            format!(
                "{{\"schema_version\":1,\"state\":\"launching\",\"agent\":\"wrong-agent\",\"target\":\"{HEX_SHA}\",\"unit\":\"validate-attributed.service\"}}\n"
            ),
        )
        .unwrap();
        let error = verify_run_attribution(
            &root,
            &root,
            &attribution_args(Some(record)),
            "0::/user.slice/validate-attributed.service\n",
        )
        .expect_err("mismatched run identity must refuse");
        assert!(error.to_string().contains("does not bind"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn conflicting_run_process_identity_is_refused_without_rewrite() {
        let root = env::temp_dir().join(format!(
            "ci-hub-attribution-conflict-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        let runs = root.join("ignored/validate/runs");
        fs::create_dir_all(&runs).unwrap();
        let record = runs.join("validate-attributed.json");
        let original = format!(
            "{{\"schema_version\":1,\"state\":\"launching\",\"agent\":\"attributed-agent\",\"target\":\"{HEX_SHA}\",\"unit\":\"validate-attributed.service\",\"process_identity\":{{\"pid\":1,\"start_ticks\":2,\"boot_id\":\"old-boot\"}}}}\n"
        );
        fs::write(&record, &original).unwrap();
        let owner = ProcessOwner {
            host: "fixture".into(),
            boot_id: "new-boot".into(),
            pid: 3,
            start_ticks: 4,
        };
        let error = publish_run_process_identity(&root, &record, &owner)
            .expect_err("a pre-existing different identity must fail closed");
        assert!(
            error.to_string().contains("different process_identity"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&record).unwrap(), original);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_run_process_identity_is_refused_without_rewrite() {
        let root = env::temp_dir().join(format!(
            "ci-hub-attribution-malformed-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        let runs = root.join("ignored/validate/runs");
        fs::create_dir_all(&runs).unwrap();
        let record = runs.join("validate-attributed.json");
        let original = format!(
            "{{\"schema_version\":1,\"state\":\"launching\",\"agent\":\"attributed-agent\",\"target\":\"{HEX_SHA}\",\"unit\":\"validate-attributed.service\"}}\n"
        );
        for (name, owner) in [
            (
                "zero-pid",
                ProcessOwner {
                    host: "fixture".into(),
                    boot_id: "boot-fixture".into(),
                    pid: 0,
                    start_ticks: 4,
                },
            ),
            (
                "zero-start-ticks",
                ProcessOwner {
                    host: "fixture".into(),
                    boot_id: "boot-fixture".into(),
                    pid: 3,
                    start_ticks: 0,
                },
            ),
            (
                "empty-boot-id",
                ProcessOwner {
                    host: "fixture".into(),
                    boot_id: "".into(),
                    pid: 3,
                    start_ticks: 4,
                },
            ),
        ] {
            fs::write(&record, &original).unwrap();
            let error = publish_run_process_identity(&root, &record, &owner)
                .expect_err("malformed identity must be refused");
            assert_eq!(
                error.to_string(),
                "validate-lock: invalid on-disk state: run process_identity requires pid>0, start_ticks>0, and nonempty boot_id",
                "{name} must fail for the identity predicate"
            );
            assert_eq!(
                fs::read_to_string(&record).unwrap(),
                original,
                "{name} must not rewrite the canonical run record"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

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
        let owner_watch_state_dir = root.join("owner-watch-state");
        fs::create_dir_all(&owner_watch_state_dir).unwrap();
        let owner_watch_verifier = Path::new(file!())
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .unwrap()
            .join(OWNER_WATCH_VERIFIER_RELATIVE);
        assert!(
            owner_watch_verifier.is_file(),
            "published owner-watch verifier missing"
        );
        let paths = LockPaths {
            guard: suffix(&lock, ".guard"),
            queue: suffix(&lock, ".queue"),
            owner: suffix(&lock, ".owner"),
            cleanup: suffix(&lock, ".cleanup-required"),
            released: suffix(&lock, ".released"),
            owner_watch_verifier: Some(owner_watch_verifier),
            owner_watch_state_dir: Some(owner_watch_state_dir),
            // PER-TEST anchor beside the per-test lease. Every `run`-path test
            // therefore exercises the real anchor CODE while touching neither the
            // live box anchor nor any sibling test's.
            anchor: suffix(&lock, ".box-anchor"),
            global_anchor: suffix(&lock, ".box-anchor"),
            meminfo: root.join("meminfo"),
            lock,
        };
        fs::write(
            &paths.meminfo,
            "MemTotal: 1073741824 kB\nMemAvailable: 1073741824 kB\n",
        )
        .unwrap();
        // Existing lock tests are about lease/cleanup behavior, not bootstrap
        // UNKNOWN. Seed them with the strongest ALLOW witness: the owner watch
        // was the previous holder. Dedicated yield tests remove/replace it.
        let now = epoch_seconds().unwrap();
        let release = ReleaseRecord {
            schema_version: 1,
            released_at: now.saturating_sub(1),
            released_human: "test-owner-watch-release".into(),
            released_by: OWNER_WATCH_AGENT.into(),
            kind: "validate".into(),
            target: "test-bootstrap".into(),
            attested_by: None,
            attestation_evidence: None,
        };
        write_truncated(&paths.released, release.render().as_bytes()).unwrap();
        seed_owner_watch_cursor(&paths, now - 60, None);
        paths
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

    fn seed_background_release(paths: &LockPaths, released_at: i64) {
        let release = ReleaseRecord {
            schema_version: 1,
            released_at,
            released_human: "test-background-release".into(),
            released_by: "background-agent".into(),
            kind: "validate".into(),
            target: "test-target".into(),
            attested_by: None,
            attestation_evidence: None,
        };
        write_truncated(&paths.released, release.render().as_bytes()).unwrap();
    }

    fn seed_owner_watch_cursor(
        paths: &LockPaths,
        observation_started_at: i64,
        admission_at: Option<i64>,
    ) {
        let admission = admission_at.map(|admitted_at| serde_json::json!({
            "state": "ADMITTED",
            "agent": OWNER_WATCH_AGENT,
            "kind": "validate",
            "target": "0123456789abcdef0123456789abcdef01234567",
            "attempt_id": "test-owner-watch-admission",
            "admitted_at": Local.timestamp_opt(admitted_at, 0).single().unwrap().to_rfc3339(),
            "admitted_at_epoch_ms": admitted_at * 1000,
            "evidence_line": "validate-lock: ACQUIRED by owner-checkout-watch running validate 0123456789abcdef0123456789abcdef01234567",
            "result_json": "/test/result.json"
        }));
        let cursor = serde_json::json!({
            "schema_version": 1,
            "observation_started_at": Local.timestamp_opt(observation_started_at, 0).single().unwrap().to_rfc3339(),
            "observation_started_at_epoch_ms": observation_started_at * 1000,
            "last_admission": admission,
        });
        let path = paths
            .owner_watch_state_dir
            .as_ref()
            .unwrap()
            .join("owner-watch-admission.json");
        write_truncated(&path, cursor.to_string().as_bytes()).unwrap();
    }

    fn seed_watch_after_latest_release(lock: &ValidateLock) {
        let released_at = lock
            .read_release_record()
            .unwrap()
            .expect("test operation must publish a release boundary")
            .released_at;
        seed_owner_watch_cursor(&lock.paths, released_at - 60, Some(released_at + 1));
    }

    fn incomplete_validate_residual(
        agent: &str,
        target: &str,
        pgid: u32,
        residuals: Vec<ProcessIdentity>,
    ) -> CleanupRecord {
        CleanupRecord::new(
            agent,
            format!("validate:{target}"),
            CleanupPhase::Residual {
                pgid,
                domain_complete: false,
                residuals,
            },
        )
        .unwrap()
    }

    #[test]
    fn incomplete_residual_discharge_restores_admission_without_stealing_live_lock() {
        let pgid = 0x7fff_fffe;
        let target = "residual-head";
        let paths = temp_paths("incomplete-residual-dead");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        assert_eq!(
            lock.acquire("residual-owner", Kind::Validate, target, 0, 30)
                .unwrap(),
            0
        );
        lock.write_current_process_owner().unwrap();
        write_cleanup_record(
            &paths.cleanup,
            &incomplete_validate_residual(
                "residual-owner",
                target,
                pgid,
                vec![ProcessIdentity {
                    pid: 0,
                    start_ticks: 1,
                }],
            ),
        )
        .unwrap();
        let mut owner = lock.read_process_owner().unwrap().unwrap();
        owner.pid = u32::MAX;
        write_truncated(&paths.owner, owner.render().as_bytes()).unwrap();
        let holder = lock.read_holder().unwrap();
        assert!(matches!(
            lock.cleanup_verification(holder.as_ref()),
            CleanupVerification::Uncensused { .. }
        ));
        let refusal = lock.require_no_cleanup(holder.as_ref()).unwrap_err();
        assert!(
            refusal
                .to_string()
                .contains("FLEET-WIDE validate quarantine")
                && refusal.to_string().contains("census-orphaned-domain"),
            "refusal must name fleet scope and discharge: {refusal}"
        );
        let cleanup_before_attestation = fs::read(&paths.cleanup).unwrap();
        let release_before_attestation = fs::read(&paths.released).ok();
        let anonymous = lock
            .census_orphaned_domain(CensusOrphanedDomainArgs {
                agent: "residual-owner".into(),
                operation: format!("validate:{target}"),
                leader: None,
                pgid,
                attest_domain_empty: true,
                attester: String::new(),
                evidence: "unit cgroup absent; migration targets empty".into(),
            })
            .unwrap_err();
        assert!(
            anonymous.to_string().contains("--attester"),
            "discharge without a named attester must refuse: {anonymous}"
        );
        assert_eq!(
            fs::read(&paths.cleanup).unwrap(),
            cleanup_before_attestation,
            "an anonymous attempt must not mutate the quarantine"
        );
        assert_eq!(
            fs::read(&paths.released).ok(),
            release_before_attestation,
            "an anonymous attempt must not publish or alter a release boundary"
        );
        assert_eq!(
            lock.census_orphaned_domain(CensusOrphanedDomainArgs {
                agent: "residual-owner".into(),
                operation: format!("validate:{target}"),
                leader: None,
                pgid,
                attest_domain_empty: true,
                attester: "codex-validatelock".into(),
                evidence: "unit cgroup absent; migration targets empty".into(),
            })
            .unwrap(),
            0
        );
        let recovered = CleanupRecord::parse(&fs::read_to_string(&paths.cleanup).unwrap()).unwrap();
        assert!(matches!(
            recovered.phase,
            CleanupPhase::Residual {
                domain_complete: true,
                ref residuals,
                ..
            } if residuals.len() == 1
        ));
        assert_eq!(
            recovered.attestation,
            Some(DomainAttestation {
                attester: "codex-validatelock".into(),
                evidence: "unit cgroup absent; migration targets empty".into(),
            })
        );
        assert_eq!(lock.reclaim_dead().unwrap(), 0);
        let release = lock.read_release_record().unwrap().unwrap();
        assert_eq!(release.schema_version, 2);
        assert_eq!(release.attested_by.as_deref(), Some("codex-validatelock"));
        assert_eq!(
            release.attestation_evidence.as_deref(),
            Some("unit cgroup absent; migration targets empty")
        );
        seed_watch_after_latest_release(&lock);
        assert_eq!(
            lock.acquire(
                "post-discharge",
                Kind::Validate,
                "post-discharge-head",
                0,
                30,
            )
            .unwrap(),
            0
        );

        let live_paths = temp_paths("incomplete-residual-live");
        let live = ValidateLock {
            paths: live_paths.clone(),
        };
        assert_eq!(
            live.acquire("live-owner", Kind::Validate, "live-head", 0, 30)
                .unwrap(),
            0
        );
        live.write_current_process_owner().unwrap();
        write_cleanup_record(
            &live_paths.cleanup,
            &incomplete_validate_residual("live-owner", "live-head", pgid, Vec::new()),
        )
        .unwrap();
        let live_holder_before = fs::read(&live_paths.lock).unwrap();
        let live_owner_before = fs::read(&live_paths.owner).unwrap();
        let live_cleanup_before = fs::read(&live_paths.cleanup).unwrap();
        let error = live
            .census_orphaned_domain(CensusOrphanedDomainArgs {
                agent: "live-owner".into(),
                operation: "validate:live-head".into(),
                leader: None,
                pgid,
                attest_domain_empty: true,
                attester: "codex-validatelock".into(),
                evidence: "inert isolated fixture".into(),
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("supervisor process is ALIVE"),
            "live owner must refuse the discharge verb: {error}"
        );
        assert!(matches!(
            live.acquire("would-steal", Kind::Validate, "other-head", 0, 30),
            Err(ValidateLockError::CleanupQuarantined(_))
        ));
        assert_eq!(fs::read(&live_paths.lock).unwrap(), live_holder_before);
        assert_eq!(fs::read(&live_paths.owner).unwrap(), live_owner_before);
        assert_eq!(fs::read(&live_paths.cleanup).unwrap(), live_cleanup_before);
        remove_if_exists(&live_paths.owner).unwrap();
        let unverifiable = live
            .census_orphaned_domain(CensusOrphanedDomainArgs {
                agent: "live-owner".into(),
                operation: "validate:live-head".into(),
                leader: None,
                pgid,
                attest_domain_empty: true,
                attester: "codex-validatelock".into(),
                evidence: "inert isolated fixture".into(),
            })
            .unwrap_err();
        assert!(
            unverifiable.to_string().contains("UNVERIFIABLE"),
            "missing guarded owner evidence must refuse: {unverifiable}"
        );
        assert!(live_paths.cleanup.exists());
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
        let _ = fs::remove_dir_all(live_paths.lock.parent().unwrap());
    }

    #[test]
    fn no_release_marker_bootstraps_to_allow_and_acquires() {
        let paths = temp_paths("yield-bootstrap");
        remove_if_exists(&paths.released).unwrap();
        let lock = ValidateLock { paths };
        let now = epoch_seconds().unwrap();

        let status = lock.yield_status_guarded(now);
        assert_eq!(status.state, YieldState::Allow);
        assert!(status.admissible);
        assert_eq!(status.reason_code, "bootstrap-no-release-yet");

        let token = lock
            .with_guard(|| lock.try_acquire("background-agent", Kind::Validate, "head", 60))
            .unwrap();
        assert!(matches!(token, AcquireToken::Acquired));
    }

    #[test]
    fn recent_background_release_without_watch_admission_yields() {
        let paths = temp_paths("yield-starved");
        let now = epoch_seconds().unwrap();
        seed_background_release(&paths, now - 1);
        seed_owner_watch_cursor(&paths, now - 60, None);
        let lock = ValidateLock { paths };

        let status = lock.yield_status_guarded(now);
        assert_eq!(status.state, YieldState::Yield);
        assert!(!status.admissible);
        assert_eq!(status.reason_code, "owner-watch-not-admitted-since-release");

        let token = lock
            .with_guard(|| lock.try_acquire("next-background-agent", Kind::Validate, "head", 60))
            .unwrap();
        assert!(matches!(token, AcquireToken::Yield { .. }));
    }

    #[test]
    fn owner_watch_admission_after_release_allows_next_acquirer() {
        let paths = temp_paths("yield-watch-admitted");
        let now = epoch_seconds().unwrap();
        seed_background_release(&paths, now - 10);
        seed_owner_watch_cursor(&paths, now - 60, Some(now - 5));
        let lock = ValidateLock { paths };

        let status = lock.yield_status_guarded(now);
        assert_eq!(status.state, YieldState::Allow);
        assert!(status.admissible);
        assert_eq!(status.reason_code, "owner-watch-admitted-since-release");

        let token = lock
            .with_guard(|| lock.try_acquire("next-background-agent", Kind::Validate, "head", 60))
            .unwrap();
        assert!(matches!(token, AcquireToken::Acquired));
    }

    #[test]
    fn unreadable_admission_observable_is_unknown_and_refuses_acquisition() {
        let paths = temp_paths("yield-unknown");
        let now = epoch_seconds().unwrap();
        seed_background_release(&paths, now - 1);
        let cursor = paths
            .owner_watch_state_dir
            .as_ref()
            .unwrap()
            .join("owner-watch-admission.json");
        write_truncated(&cursor, b"not-json\n").unwrap();
        let lock = ValidateLock { paths };

        let status = lock.yield_status_guarded(now);
        assert_eq!(status.state, YieldState::Unknown);
        assert!(!status.admissible);
        assert_eq!(status.reason_code, "owner-watch-results-unreadable");

        let token = lock
            .with_guard(|| lock.try_acquire("next-background-agent", Kind::Validate, "head", 60))
            .unwrap();
        assert!(matches!(token, AcquireToken::YieldUnknown { .. }));
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
        assert!(!status.lock_admissible);
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
        assert_eq!(json["lock_admissible"], false);
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
            assert!(
                !status.lock_admissible,
                "case {} became lock-admissible",
                case.name
            );
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
    fn frozen_authority_is_lock_admissible_but_not_canonical() {
        let paths = temp_paths("authority-frozen-validate");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        lock.write_holder(
            &new_holder("authority-test", Kind::FrozenValidate, HEX_SHA, 60, None).unwrap(),
        )
        .unwrap();
        write_truncated(
            &paths.owner,
            current_process_owner().unwrap().render().as_bytes(),
        )
        .unwrap();
        let _anchor = BoxExclusionAnchor::take(&paths.anchor, 0)
            .unwrap()
            .expect("frozen authority anchor must be available");

        let status = lock.with_guard(|| lock.authority_status_guarded()).unwrap();
        assert!(status.lock_admissible);
        assert!(!status.admissible);
        assert_eq!(status.state, AuthorityState::Held);
        assert_eq!(
            status.reason_code.as_deref(),
            Some("canonical-holder-kind-not-validate")
        );
        assert_eq!(
            status.holder.as_ref().map(|holder| holder.kind.as_str()),
            Some("frozen-validate")
        );
        assert_eq!(
            status.owner.as_ref().map(|owner| owner.liveness),
            Some(AuthorityOwnerLiveness::Alive)
        );

        let mut invalid_owner = current_process_owner().unwrap();
        invalid_owner.start_ticks = invalid_owner.start_ticks.saturating_add(1);
        write_truncated(&paths.owner, invalid_owner.render().as_bytes()).unwrap();
        let invalid = lock.with_guard(|| lock.authority_status_guarded()).unwrap();
        assert!(!invalid.lock_admissible);
        assert!(!invalid.admissible);
        assert_eq!(invalid.state, AuthorityState::Orphaned);
        assert_eq!(
            invalid.reason_code.as_deref(),
            Some("canonical-owner-start-ticks-mismatch")
        );
        let _ = fs::remove_dir_all(root);
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
            assert!(status.lock_admissible, "real run must have a valid holder");
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
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
                    run_record: None,
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

        let global = GlobalQueueEntry {
            enqueued_at: 1785770488,
            priority: QueuePriority::OwnerDirective,
            agent: "codex-timeouts".into(),
            kind: Kind::Validate,
            target: HEX_SHA.into(),
            owner: Some(ProcessOwner {
                host: "testhost".into(),
                boot_id: "boot-1".into(),
                pid: 42,
                start_ticks: 99,
            }),
        };
        assert_eq!(
            GlobalQueueEntry::parse(global.render().trim_end(), 1).unwrap(),
            global
        );
    }

    // 2. N=3 sequential jobs with the priority watch admitted at each free
    // boundary all proceed; lock ends FREE.
    #[test]
    fn positive_non_starvation_three_sequential() {
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
                        run_record: None,
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
            if i < 2 {
                seed_watch_after_latest_release(&lock);
            }
        }
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn run_child_receives_process_bound_exclusivity_proof() {
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
                    run_record: None,
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

    // 3. The legacy one-holder acquisition remains exclusive, and a benchmark
    // request is refused while that holder is live.
    #[test]
    fn negative_legacy_holder_refuses_a_second_request_and_a_benchmark() {
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
        let paths = temp_paths("second-holder");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        // agent1 holds the legacy exclusive lease as a validate.
        assert_eq!(
            lock.acquire("agent1", Kind::Validate, "sha1", 0, 60)
                .unwrap(),
            0
        );

        // (a) a second legacy acquisition returns Held (not Acquired), naming agent1.
        let token = lock
            .with_guard(|| lock.try_acquire("agent2", Kind::Validate, "sha2", 60))
            .unwrap();
        assert!(
            matches!(&token, AcquireToken::Held { agent, .. } if agent == "agent1"),
            "second legacy acquisition must see agent1 holding the box"
        );

        // (b) the --no-wait refuse path returns exit code 3.
        let code = lock
            .run(
                RunArgs {
                    agent: "agent2".into(),
                    kind: Kind::Bench,
                    target: "sha2".into(),
                    run_record: None,
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
        seed_watch_after_latest_release(&lock);
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

    #[test]
    fn expired_live_holder_is_not_replaced_during_acquire() {
        let paths = temp_paths("expired-live-holder");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let now = epoch_seconds().unwrap();
        let mut holder = new_holder("still-running", Kind::Validate, "sha1", 60, None).unwrap();
        holder.acquired_at = now - 61;
        holder.expires_at = now - 1;
        lock.write_holder(&holder).unwrap();
        lock.write_current_process_owner().unwrap();

        let token = lock
            .with_guard(|| lock.try_acquire("replacement", Kind::Validate, "sha2", 60))
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
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let now = epoch_seconds().unwrap();
        let mut holder = new_holder("dead-holder", Kind::Validate, "sha1", 60, None).unwrap();
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
            .with_guard(|| lock.try_acquire("replacement", Kind::Validate, "sha2", 60))
            .unwrap();
        assert!(matches!(token, AcquireToken::AcquiredReclaimed(_)));
        assert_eq!(lock.read_holder().unwrap().unwrap().agent, "replacement");
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    // 4. run child-deadline kills the child quickly and releases the box.
    #[test]
    fn run_child_deadline_kills_and_releases() {
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
                    run_record: None,
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
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
                    run_record: None,
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
            "redundant-attester",
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

    // A positive live-domain census is stronger than operator attestation. The
    // exact defect under test recorded the agent sandbox; allowing an override
    // here would let the cleaner bury live agent/infrastructure processes.
    #[test]
    fn a_populated_domain_cannot_be_overridden_by_attestation() {
        let refusal = census_disposition(
            DomainEvidence::Populated("payload cgroup /x still holds 2 process(es): 11,22".into()),
            true,
            "unsafe-attester",
            "operator checked: strays are unrelated",
            CTX,
        )
        .expect_err("attestation must not contradict a live-domain census");
        assert!(
            refusal.contains("cannot override a live domain"),
            "the refusal must state the safety rule, got: {refusal}"
        );
        assert!(
            refusal.contains("11,22"),
            "must carry the occupants, got: {refusal}"
        );
    }

    // ...and an anonymous override is refused for the same stronger reason.
    #[test]
    fn an_anonymous_override_of_a_populated_domain_is_refused() {
        let refusal = census_disposition(
            DomainEvidence::Populated("/x holds 1".into()),
            true,
            "",
            "  ",
            CTX,
        )
        .unwrap_err();
        assert!(
            refusal.contains("cannot override a live domain"),
            "got: {refusal}"
        );
    }

    // Unprovable and unattested: refused, and the refusal must name BOTH why it
    // could not be proven and the exact remedy.
    #[test]
    fn an_unprovable_domain_without_attestation_is_refused_with_the_remedy() {
        let refusal = census_disposition(
            DomainEvidence::Unproven("no cgroup anchor".into()),
            false,
            "",
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

    // Mutation guard: satisfying every other input may never produce an
    // attested discharge whose recorded claimant is empty.
    #[test]
    fn an_unnamed_attestation_is_still_refused() {
        let refusal = census_disposition(
            DomainEvidence::Unproven("no cgroup anchor".into()),
            true,
            "",
            "unit cgroup absent; migration targets empty",
            CTX,
        )
        .expect_err("an attestation with no recorded attester must not discharge");
        assert!(refusal.contains("--attester"), "got: {refusal}");
    }

    // The attestation must stay auditable; a named but unexplained one is still
    // refused.
    #[test]
    fn an_unexplained_attestation_is_still_refused() {
        let refusal = census_disposition(
            DomainEvidence::Unproven("no cgroup anchor".into()),
            true,
            "recovery-agent",
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
            "recovery-agent",
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

    // LEGACY FALLBACK PRESERVED: a version-2 record has no cleanup domain, so
    // the result is Unproven and operator attestation remains the only route.
    #[test]
    fn a_record_without_a_cleanup_domain_is_unprovable_and_keeps_attestation() {
        match census_recorded_domain(None) {
            DomainEvidence::Unproven(why) => assert!(
                why.contains("no cleanup domain"),
                "the reason must name the missing domain, got: {why}"
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
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
                run_record: None,
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
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
                run_record: None,
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
    fn process_group_generation_distinguishes_reuse_from_live_or_unknown() {
        let original = ProcessIdentity {
            pid: 4242,
            start_ticks: 100,
        };
        assert!(matches!(
            classify_existing_process_group(4242, &original, Ok(101)),
            OriginalProcessGroupState::Absent(reason) if reason.contains("reused")
        ));
        assert!(matches!(
            classify_existing_process_group(
                4242,
                &original,
                Err(io::Error::from(io::ErrorKind::NotFound)),
            ),
            OriginalProcessGroupState::Active(reason) if reason.contains("without a readable group leader")
        ));
        assert!(matches!(
            classify_existing_process_group(
                4242,
                &original,
                Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            ),
            OriginalProcessGroupState::Unknown(reason) if reason.contains("unreadable")
        ));
    }

    /// POSITIVE ZERO-RESIDUAL BRACKET. The completed child leaves no exact
    /// descendants, while the first PGID sample still reports a transient live
    /// group — the exact production state from #2127. The bounded re-census
    /// observes it drain, releases both authorities, and the next validate is
    /// admitted. Without the stabilization loop the first run quarantines.
    #[test]
    fn zero_residual_transient_group_releases_and_immediately_readmits() {
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let stub = write_stub("zero-residual-admit", "#!/bin/sh\necho OK\nexit 0\n");
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);

        let paths = temp_paths("zero-residual-readmit");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let run_args = |agent: &str| RunArgs {
            agent: agent.into(),
            kind: Kind::Validate,
            target: HEX_SHA.into(),
            run_record: None,
            no_wait: false,
            wait: 0,
            hold: 30,
            child_deadline: 30,
            max: 1,
            skip_base_check: false,
            child: vec![OsString::from("/bin/true")],
        };

        let transient = OriginalGroupTestStatesGuard::install([
            OriginalProcessGroupState::Active("fixture transient group".into()),
            OriginalProcessGroupState::Absent("fixture group drained".into()),
        ]);
        assert_eq!(lock.run(run_args("zero-residual-green"), &root).unwrap(), 0);
        drop(transient);
        assert!(lock.read_holder().unwrap().is_none());
        assert!(!paths.cleanup.exists());

        let released = ReleaseRecord::parse(&fs::read_to_string(&paths.released).unwrap()).unwrap();
        let admitted_at = loop {
            let now = epoch_seconds().unwrap();
            if now > released.released_at {
                break now;
            }
            thread::sleep(Duration::from_millis(10));
        };
        seed_owner_watch_cursor(&paths, released.released_at, Some(admitted_at));
        assert_eq!(
            lock.run(run_args("post-zero-residual-green"), &root)
                .unwrap(),
            0
        );
        assert!(lock.read_holder().unwrap().is_none());

        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let _ = fs::remove_file(&stub);
        let _ = fs::remove_dir_all(&root);
    }

    /// The adopted grandchild must disappear while the direct payload is still
    /// alive. An end-of-run reap cannot satisfy this bracket. Run it once with
    /// success and once with a nonzero direct exit so periodic `waitpid(-1)`
    /// cannot silently discard the payload's result.
    #[test]
    fn supervise_child_reaps_adopted_exits_during_run_and_preserves_direct_status() {
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
        enable_child_subreaper().unwrap();
        SUPERVISOR_TERMINATION_SIGNAL.store(0, Ordering::SeqCst);

        let script = r#"
import os
import pathlib
import sys
import time

grandchild_pid_path = pathlib.Path(sys.argv[1])
release_path = pathlib.Path(sys.argv[2])
exit_code = int(sys.argv[3])

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

        for expected_code in [0, 23] {
            let root = env::temp_dir().join(format!(
                "validate-lock-periodic-reap-{}-{expected_code}",
                std::process::id()
            ));
            fs::create_dir_all(&root).unwrap();
            let grandchild_pid_path = root.join("grandchild.pid");
            let release_path = root.join("release");
            let mut gated = spawn_gated_child(
                &OsString::from("/usr/bin/python3"),
                &[
                    OsString::from("-c"),
                    OsString::from(script),
                    grandchild_pid_path.clone().into_os_string(),
                    release_path.clone().into_os_string(),
                    OsString::from(expected_code.to_string()),
                ],
                |_| {},
            )
            .unwrap();
            let direct_pid = gated.child.id();
            gated.release().unwrap();

            let observer = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(4);
                let grandchild_pid = loop {
                    if let Ok(text) = fs::read_to_string(&grandchild_pid_path) {
                        if let Ok(pid) = text.trim().parse::<u32>() {
                            break pid;
                        }
                    }
                    assert!(
                        Instant::now() < deadline,
                        "direct child did not publish its grandchild PID"
                    );
                    thread::sleep(Duration::from_millis(10));
                };
                let grandchild_proc = PathBuf::from(format!("/proc/{grandchild_pid}"));
                let direct_proc = PathBuf::from(format!("/proc/{direct_pid}"));
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
                fs::write(&release_path, b"release\n").unwrap();
            });

            let outcome = supervise_child(&mut gated.child, 5, "periodic-reap-bracket");
            observer.join().unwrap();
            match outcome {
                ChildOutcome::Exited { status, pgid } => {
                    assert_eq!(pgid, direct_pid);
                    assert_eq!(status.code(), Some(expected_code));
                }
                _ => panic!("expected direct child exit"),
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    /// POSITIVE PROTOCOL BRACKET: the supervised leader deliberately exits
    /// while leaving one already-exited child adopted by the subreaper. That
    /// zombie is completion bookkeeping, not executable payload ownership;
    /// cleanup must release both authorities and admit the next run.
    #[test]
    fn completed_run_with_adopted_zombie_releases_and_readmits() {
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let stub = write_stub("zombie-readmit-admit", "#!/bin/sh\necho OK\nexit 0\n");
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);

        let paths = temp_paths("zombie-readmit");
        let root = paths.lock.parent().unwrap().to_path_buf();
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let zombie_script = r#"
import os
import time

child = os.fork()
if child == 0:
    os._exit(0)
while True:
    with open(f"/proc/{child}/stat", encoding="utf-8") as stat:
        if stat.read().split()[2] == "Z":
            break
    time.sleep(0.001)
os._exit(0)
"#;
        let run_args = |agent: &str, child: Vec<OsString>| RunArgs {
            agent: agent.into(),
            kind: Kind::Validate,
            target: HEX_SHA.into(),
            run_record: None,
            no_wait: false,
            wait: 0,
            hold: 30,
            child_deadline: 30,
            max: 1,
            skip_base_check: false,
            child,
        };

        let first = lock.run(
            run_args(
                "zombie-complete-validate",
                vec![
                    OsString::from("/usr/bin/python3"),
                    OsString::from("-c"),
                    OsString::from(zombie_script),
                ],
            ),
            &root,
        );
        assert_eq!(first.unwrap(), 0);
        assert!(lock.read_holder().unwrap().is_none());
        assert!(!paths.cleanup.exists());

        // Satisfy the independent owner-watch fairness rule, then exercise the
        // real box-anchor + durable-lease admission path a second time.
        let released = ReleaseRecord::parse(&fs::read_to_string(&paths.released).unwrap()).unwrap();
        let admitted_at = loop {
            let now = epoch_seconds().unwrap();
            if now > released.released_at {
                break now;
            }
            thread::sleep(Duration::from_millis(10));
        };
        seed_owner_watch_cursor(&paths, released.released_at, Some(admitted_at));
        let second = lock.run(
            run_args("post-zombie-validate", vec![OsString::from("/bin/true")]),
            &root,
        );
        assert_eq!(second.unwrap(), 0);
        assert!(lock.read_holder().unwrap().is_none());

        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let _ = fs::remove_file(&stub);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn escaped_payload_quarantine_refuses_consumers_until_exact_recovery() {
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
                    run_record: None,
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

        let held_before = fs::read(&paths.lock).unwrap();
        let owner_before = fs::read(&paths.owner).unwrap();
        let cleanup_before = fs::read(&paths.cleanup).unwrap();
        assert!(matches!(
            lock.acquire("replacement-validate", Kind::Validate, "sha-next", 0, 30),
            Err(ValidateLockError::CleanupQuarantined(_))
        ));
        assert_eq!(fs::read(&paths.lock).unwrap(), held_before);
        assert_eq!(fs::read(&paths.owner).unwrap(), owner_before);
        assert_eq!(fs::read(&paths.cleanup).unwrap(), cleanup_before);
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
        lock.status(paths.lock.parent().unwrap()).unwrap();

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
        seed_watch_after_latest_release(&lock);
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
                run_record: None,
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
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
                        run_record: None,
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
                ) => {
                    assert_eq!(
                        record.domain,
                        CleanupDomain::ProcessGroup,
                        "publish must record the payload-owned PGID, not the supervisor cgroup"
                    );
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
            lock.status(paths.lock.parent().unwrap()).unwrap();

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
                assert_eq!(
                    lock.census_orphaned_domain(CensusOrphanedDomainArgs {
                        agent: "crash-validate".into(),
                        operation: format!("bench:{target}"),
                        leader: Some(format!("{}:{}", leader.pid, leader.start_ticks)),
                        pgid,
                        attest_domain_empty: false,
                        attester: String::new(),
                        evidence: String::new(),
                    })
                    .unwrap(),
                    0
                );
                assert!(matches!(
                    lock.cleanup_verification(lock.read_holder().unwrap().as_ref()),
                    CleanupVerification::Recoverable { .. }
                ));
                assert_eq!(lock.reclaim_dead().unwrap(), 0);
            }
            let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
        }
    }

    #[test]
    fn normal_leader_exit_cannot_clear_an_escaped_payload_barrier() {
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
                    run_record: None,
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

    // 5. --max outside the fixed one/six policies is rejected before acquiring.
    #[test]
    fn unsupported_intermediate_max_is_rejected() {
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
                    run_record: None,
                    no_wait: false,
                    wait: 0,
                    hold: 30,
                    child_deadline: 30,
                    max: 3,
                    skip_base_check: false,
                    child: vec![OsString::from("/bin/true")],
                },
                paths.lock.parent().unwrap(),
            )
            .unwrap_err();
        assert!(matches!(error, ValidateLockError::BadMax));
        assert!(lock.read_holder().unwrap().is_none());
        // The Display carries the exact unproven-cap rationale.
        assert!(error.to_string().contains("unsupported --max"));
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn live_named_agent_and_exited_validation_supervisor_are_reported_separately() {
        let named_agent = "codex-lsm";
        let named_agent_process = current_process_owner().unwrap();
        assert_eq!(
            process_start_ticks(named_agent_process.pid).unwrap(),
            named_agent_process.start_ticks,
            "the named-agent stand-in must be live while the supervisor is reported exited"
        );

        let report = format!(
            "agent={named_agent}\nvalidation_supervisor_process={}",
            render_supervisor_liveness(&OwnerLiveness::Dead("pid 2884548 is absent".into()))
        );
        assert_eq!(
            report,
            "agent=codex-lsm\nvalidation_supervisor_process=exited:pid 2884548 is absent"
        );
        assert!(!report.contains("owner_process=dead"));
        assert!(!report.contains("agent=codex-lsm dead"));
    }

    // 6. An exited validation supervisor is retained until TTL expiry, then reclaimed.
    #[test]
    fn exited_validation_supervisor_waits_for_expiry_then_reclaims() {
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
        // Death alone does not erase an unexpired distributed lease.
        assert_eq!(
            lock.acquire("replacement-validate", Kind::Validate, "sha-new", 0, 60)
                .unwrap(),
            1
        );
        assert_eq!(lock.read_holder().unwrap().unwrap().agent, "dead-validate");
        let now = epoch_seconds().unwrap();
        let mut expired = lock.read_holder().unwrap().unwrap();
        expired.acquired_at = now - 61;
        expired.expires_at = now - 1;
        lock.write_holder(&expired).unwrap();
        assert_eq!(
            lock.acquire("replacement-validate", Kind::Validate, "sha-new", 0, 60)
                .unwrap(),
            0
        );
        let holder = lock.read_holder().unwrap().unwrap();
        assert_eq!(holder.agent, "replacement-validate");
        assert!(holder
            .reclaimed_from
            .unwrap()
            .contains("validation supervisor exited"));
        lock.release("replacement-validate", false).unwrap();
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    // --- composite admission gate (mechanical REBASE-BEFORE-VALIDATE) ---
    // ⚠️ HOLD ENV_LOCK TO *READ* THE OVERRIDE, NOT ONLY TO WRITE IT.
    //
    // This comment previously said only "these serialize because they mutate a
    // process-global env var", and that framing is what let a flake through: it
    // names writers and it names tests that never touch the env, and it silently
    // omits the third case — a test that READS the env without writing it. A
    // reader without the guard is just as racy, because the value it reads may be
    // a concurrent writer's.
    //
    // `LockPaths::for_workspace` reads `test_only_override(VALIDATE_LOCK_PATH_ENV)`,
    // which under `cfg(test)` is a live `env::var_os`. So any test that calls
    // `for_workspace` AND asserts on a field derived from `lock` (`lock`, `guard`,
    // `queue`, `owner`, `cleanup`, `released`) must hold this. `anchor` is exempt:
    // it is uid-derived in `LockPaths::at` and never a function of the override.
    //
    // `for_authority_query` is exempt by construction — it resolves the production
    // domain without consulting any ambient override.
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
        let default_paths = LockPaths::for_workspace(&root, &root);
        assert_eq!(default_paths.lock, root.join(".validate-lock"));
        assert_eq!(default_paths.guard, root.join(".validate-lock.guard"));
        // Override arm: present only because this is a `cfg(test)` build.
        env::set_var(VALIDATE_LOCK_PATH_ENV, &alt);
        let overridden = LockPaths::for_workspace(&root, &root);
        env::remove_var(VALIDATE_LOCK_PATH_ENV);
        assert_eq!(overridden.lock, alt);
        assert_eq!(overridden.queue, suffix(&alt, ".queue"));
        // An empty value is not an override -- it would otherwise select "" and
        // silently relocate every lock file to the process working directory.
        env::set_var(VALIDATE_LOCK_PATH_ENV, "");
        let empty = LockPaths::for_workspace(&root, &root);
        env::remove_var(VALIDATE_LOCK_PATH_ENV);
        assert_eq!(empty.lock, root.join(".validate-lock"));
    }

    #[test]
    fn split_roots_keep_lock_state_and_owner_watch_code_separate() {
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = env::temp_dir().join(format!(
            "ci-hub-vlock-split-roots-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        let tool_root = root.join("tool");
        let state_root = root.join("state");
        fs::create_dir_all(&tool_root).unwrap();
        fs::create_dir_all(&state_root).unwrap();

        let paths = LockPaths::for_workspace(&tool_root, &state_root);

        assert_eq!(paths.lock, state_root.join(".validate-lock"));
        assert_eq!(
            paths.owner_watch_verifier,
            Some(tool_root.join(OWNER_WATCH_VERIFIER_RELATIVE))
        );
        let _ = fs::remove_dir_all(root);
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
        // Held for the same reason as `a_non_repository_root_falls_back_to_itself`:
        // the two `for_workspace` calls below READ the process-global override, so
        // a concurrent writer makes both sides return the same planted path and the
        // linked-worktree assertion fails for a reason it does not name.
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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

        let from_main = LockPaths::for_workspace(&main, &main);
        let from_linked = LockPaths::for_workspace(&linked, &linked);
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
        //
        // ENV_LOCK IS HELD BECAUSE THIS TEST *READS* THE PROCESS-GLOBAL OVERRIDE,
        // not because it writes one. `for_workspace` consults
        // `test_only_override(VALIDATE_LOCK_PATH_ENV)`, which under `cfg(test)` is
        // a live `env::var_os`. Without the guard a concurrent writer's value is
        // read as this test's answer, and the assertion below reports the OTHER
        // test's directory.
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = env::temp_dir().join(format!(
            "ci-hub-vlock-norepo-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        assert_eq!(repository_lock_root(&root), root);
        assert_eq!(
            LockPaths::for_workspace(&root, &root).lock,
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
        assert_eq!(
            BoxExclusionAnchor::observe(&anchor),
            BoxAnchorObservation::Free
        );
        let first = BoxExclusionAnchor::take(&anchor, 0).unwrap();
        assert!(first.is_some(), "an unheld anchor must be granted");

        // A SECOND caller -- which in production is a different repository with its
        // own lease file -- is refused, and `acquire`'s non-holding probe sees it.
        let second = BoxExclusionAnchor::take(&anchor, 0).unwrap();
        assert!(
            second.is_none(),
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
    fn mem_available_parser_requires_one_kib_field() {
        assert_eq!(
            parse_mem_available_bytes(
                "MemTotal: 1048576 kB\nMemAvailable: 12582912 kB\nSwapFree: 0 kB\n"
            )
            .unwrap(),
            12_884_901_888
        );
        assert!(parse_mem_available_bytes("MemTotal: 1 kB\n")
            .unwrap_err()
            .contains("no MemAvailable"));
        assert!(
            parse_mem_available_bytes("MemAvailable: 1 kB\nMemAvailable: 2 kB\n")
                .unwrap_err()
                .contains("duplicate")
        );
        assert!(parse_mem_available_bytes("MemAvailable: unknown kB\n")
            .unwrap_err()
            .contains("not an integer"));
    }

    #[test]
    fn memory_admission_can_refuse_while_count_capacity_remains() {
        assert_eq!(VALIDATE_CONCURRENT_CAP, 6);
        assert!(validate_memory_admission(1, 4_294_967_296, VALIDATE_CONCURRENT_CAP).is_ok());
        let reason =
            validate_memory_admission(2, 5_368_709_120, VALIDATE_CONCURRENT_CAP).unwrap_err();
        assert!(reason.contains("memory admission requires 6442450944 bytes"));
        assert!(reason.contains("MemAvailable=5368709120"));
        assert!(
            validate_memory_admission(5, u64::MAX, VALIDATE_CONCURRENT_CAP).is_ok(),
            "the measured six-job, 12-GiB envelope must be admissible"
        );
        assert!(
            validate_memory_admission(6, u64::MAX, VALIDATE_CONCURRENT_CAP)
                .unwrap_err()
                .contains("exceed the --max 6")
        );
    }

    #[test]
    fn global_turn_rechecks_memory_under_the_queue_guard() {
        let paths = temp_paths("memory-admission");
        fs::write(
            &paths.meminfo,
            "MemTotal: 16777216 kB\nMemAvailable: 3145728 kB\n",
        )
        .unwrap();
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let owner = current_process_owner().unwrap();
        let mut first = global_queue_run_args("memory-first", HEX_SHA);
        first.max = VALIDATE_CONCURRENT_CAP;
        let first_anchors = match lock
            .try_global_turn(&first, &owner, "fixture=memory-first", true)
            .unwrap()
        {
            GlobalTurn::Acquired { anchors, slot: 0 } => anchors,
            _ => panic!("first validate must fit the memory budget"),
        };

        let mut second =
            global_queue_run_args("memory-second", "1111111111111111111111111111111111111111");
        second.max = VALIDATE_CONCURRENT_CAP;
        assert!(matches!(
            lock.try_global_turn(&second, &owner, "fixture=memory-second", true)
                .unwrap(),
            GlobalTurn::WaitMemory(reason)
                if reason.contains("memory admission requires 4294967296 bytes")
        ));
        assert_eq!(
            first_anchors.len(),
            1,
            "memory refusal must not disturb the live validate"
        );

        fs::write(
            &paths.meminfo,
            "MemTotal: 16777216 kB\nMemAvailable: 4194304 kB\n",
        )
        .unwrap();
        let second_anchors = match lock
            .try_global_turn(&second, &owner, "fixture=memory-second", true)
            .unwrap()
        {
            GlobalTurn::Acquired { anchors, slot: 1 } => anchors,
            _ => panic!("second validate must be admitted after memory recovers"),
        };
        drop(second_anchors);
        drop(first_anchors);
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn every_validation_kind_uses_memory_admission() {
        for kind in [Kind::Validate, Kind::ReverieValidate, Kind::FrozenValidate] {
            let paths = temp_paths(kind.as_str());
            fs::write(
                &paths.meminfo,
                "MemTotal: 16777216 kB\nMemAvailable: 1048576 kB\n",
            )
            .unwrap();
            let lock = ValidateLock {
                paths: paths.clone(),
            };
            let owner = current_process_owner().unwrap();
            let mut args = global_queue_run_args(kind.as_str(), HEX_SHA);
            args.kind = kind;
            args.max = VALIDATE_CONCURRENT_CAP;
            assert!(matches!(
                lock.try_global_turn(&args, &owner, "fixture=validation-kind", true)
                    .unwrap(),
                GlobalTurn::WaitMemory(_)
            ));
            assert_eq!(
                BoxExclusionAnchor::observe(&paths.slot_anchor(0)),
                BoxAnchorObservation::Free
            );
            let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
        }
    }

    #[test]
    fn malformed_memory_evidence_refuses_before_taking_a_slot() {
        let paths = temp_paths("memory-unknown");
        fs::write(&paths.meminfo, "MemTotal: 1048576 kB\n").unwrap();
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let owner = current_process_owner().unwrap();
        let mut args = global_queue_run_args("memory-unknown", HEX_SHA);
        args.max = VALIDATE_CONCURRENT_CAP;
        args.no_wait = true;
        assert!(lock
            .wait_for_global_turn(
                &args,
                &owner,
                "fixture=memory-unknown",
                true,
                paths.lock.parent().unwrap(),
            )
            .unwrap()
            .is_none());
        assert!(
            lock.read_global_queue().unwrap().is_empty(),
            "memory-evidence refusal must remove its queue entry"
        );
        assert_eq!(
            BoxExclusionAnchor::observe(&paths.slot_anchor(0)),
            BoxAnchorObservation::Free,
            "unknown memory evidence must not consume a validation slot"
        );
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn six_validation_slots_remove_the_old_single_anchor_collision() {
        let dir = env::temp_dir().join(format!(
            "ci-hub-vlock-six-slots-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let mut paths = LockPaths::at(dir.join("repository-validate-lock"));
        paths.anchor = dir.join("validate-box.lock");
        paths.global_anchor = paths.anchor.clone();

        // Removed behavior: both validates took the one global anchor, so the
        // second acquisition deterministically collided with the first.
        let old_first = BoxExclusionAnchor::take(&paths.global_anchor(), 0)
            .unwrap()
            .expect("old first validation must acquire the single anchor");
        assert!(
            BoxExclusionAnchor::take(&paths.global_anchor(), 0)
                .unwrap()
                .is_none(),
            "the old one-anchor path must reproduce the second-run collision"
        );
        drop(old_first);

        let mut slots = Vec::new();
        for slot in 0..VALIDATE_SLOT_COUNT {
            slots.push(
                BoxExclusionAnchor::take(&paths.slot_anchor(slot), 5)
                    .unwrap()
                    .unwrap_or_else(|| panic!("slot {slot} must be available")),
            );
        }
        assert!(
            BoxExclusionAnchor::take(&paths.slot_anchor(VALIDATE_SLOT_COUNT - 1), 0)
                .unwrap()
                .is_none(),
            "a seventh validation must not exceed the six-slot policy"
        );
        assert_eq!(
            paths.for_slot(VALIDATE_SLOT_COUNT - 1).global_anchor(),
            paths.global_anchor(),
            "cleanup from the highest slot must use the global anchor",
        );
        drop(slots);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn six_guarded_validate_admissions_get_distinct_cleanup_slots() {
        let paths = temp_paths("six-guarded-slots");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let owner = current_process_owner().unwrap();
        let mut admitted = Vec::new();
        for slot in 0..VALIDATE_SLOT_COUNT {
            let mut args =
                global_queue_run_args(&format!("six-slot-agent-{slot}"), &format!("{slot:040x}"));
            args.max = VALIDATE_CONCURRENT_CAP;
            match lock
                .try_global_turn(&args, &owner, &format!("fixture=six-slot-{slot}"), true)
                .unwrap()
            {
                GlobalTurn::Acquired {
                    anchors,
                    slot: admitted_slot,
                } => {
                    assert_eq!(admitted_slot, slot);
                    assert_eq!(anchors.len(), 1);
                    assert_eq!(
                        paths.for_slot(admitted_slot).global_anchor(),
                        paths.global_anchor(),
                        "every cleanup slot must retain the one global anchor",
                    );
                    admitted.push(anchors);
                }
                _ => panic!("validate {slot} must be admitted"),
            }
        }

        let mut seventh = global_queue_run_args(
            "six-slot-agent-refused",
            "ffffffffffffffffffffffffffffffffffffffff",
        );
        seventh.max = VALIDATE_CONCURRENT_CAP;
        assert!(matches!(
            lock.try_global_turn(&seventh, &owner, "fixture=six-slot-refused", true)
                .unwrap(),
            GlobalTurn::Held(_)
        ));
        drop(admitted);
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    fn attributed_status_fixture(
        name: &str,
    ) -> (PathBuf, PathBuf, BoxExclusionAnchor, GlobalValidateHolder) {
        let dir = env::temp_dir().join(format!(
            "ci-hub-vlock-global-status-{name}-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let anchor = dir.join("validate-box.lock");
        let record = dir.join("validate-status-fixture.json");
        let pid = std::process::id();
        let start_ticks = process_start_ticks(pid).unwrap();
        let boot_id = current_boot_id().unwrap();
        let host = current_host();
        let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
        let unit = cgroup
            .lines()
            .find_map(|line| line.splitn(3, ':').nth(2))
            .and_then(|path| path.rsplit('/').find(|component| !component.is_empty()))
            .unwrap()
            .to_string();
        fs::write(
            &record,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1,
                // Mutable observer state is not owner identity.
                "state": "completed",
                "agent": "global-status-agent",
                "target": HEX_SHA,
                "unit": unit,
                "process_identity": {
                    "pid": pid,
                    "start_ticks": start_ticks,
                    "boot_id": boot_id,
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let record = fs::canonicalize(record).unwrap();
        let attribution = format!(
            "boot_id={boot_id} start_ticks={start_ticks} agent=global-status-agent \
             kind=validate target={HEX_SHA} unit={unit} record={}",
            record.display()
        );
        let held = BoxExclusionAnchor::take_attributed(&anchor, 0, &attribution)
            .unwrap()
            .unwrap();
        let expected = GlobalValidateHolder {
            pid,
            start_ticks,
            boot_id,
            host,
            agent: "global-status-agent".into(),
            kind: "validate".into(),
            target: HEX_SHA.into(),
            unit,
            record: record.clone(),
            cgroup: cgroup.trim().to_string(),
        };
        (anchor, record, held, expected)
    }

    /// HELD bracket: global flock, exact record identity, and live cgroup agree
    /// even though the record's mutable observer state is terminal.
    #[test]
    fn global_status_names_the_exact_live_holder() {
        let (anchor, record, held, expected) = attributed_status_fixture("held");
        assert_eq!(
            observe_global_validate_anchor(record.parent().unwrap(), &anchor),
            GlobalValidateObservation::Held(expected)
        );
        drop(held);
        let _ = fs::remove_dir_all(record.parent().unwrap());
    }

    /// FREE bracket: an absent anchor is a positive, non-mutating free sample.
    #[test]
    fn global_status_reports_free_with_no_holder() {
        let dir = env::temp_dir().join(format!(
            "ci-hub-vlock-global-status-free-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            observe_global_validate_anchor(&dir, &dir.join("absent-anchor")),
            GlobalValidateObservation::Free
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// UNKNOWN bracket: a live flock whose exact record is unreadable is not
    /// FREE, and the kernel authority still refuses a second acquisition.
    #[test]
    fn unreadable_live_record_is_unknown_and_not_acquirable() {
        let (anchor, record, held, _) = attributed_status_fixture("unknown");
        fs::remove_file(&record).unwrap();
        assert!(matches!(
            observe_global_validate_anchor(record.parent().unwrap(), &anchor),
            GlobalValidateObservation::Unknown(reason)
                if reason.contains("exact run record")
        ));
        assert!(
            BoxExclusionAnchor::take(&anchor, 0).unwrap().is_none(),
            "UNKNOWN must not become an acquirable boundary"
        );
        drop(held);
        let _ = fs::remove_dir_all(record.parent().unwrap());
    }

    fn global_queue_run_args(agent: &str, target: &str) -> RunArgs {
        RunArgs {
            agent: agent.into(),
            kind: Kind::Validate,
            target: target.into(),
            run_record: None,
            no_wait: false,
            wait: 30,
            hold: 30,
            child_deadline: 30,
            max: 1,
            skip_base_check: false,
            child: vec![OsString::from("/bin/true")],
        }
    }

    /// PRIORITY BRACKET + NO-PREEMPTION BRACKET. Both waiters arrive while an
    /// unrelated live holder owns the anchor. The higher-priority waiter moves
    /// to the head, the lower waiter yields, and neither operation changes or
    /// releases the holder. Only after the holder drops does the head acquire.
    #[test]
    fn lower_priority_yields_without_preempting_live_holder() {
        let paths = temp_paths("global-priority-yield");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let live_holder = BoxExclusionAnchor::take(&paths.anchor, 0).unwrap().unwrap();
        let holder_before = fs::read_to_string(&paths.anchor).unwrap();
        let low = global_queue_run_args("prepared-agent", HEX_SHA);
        let high = global_queue_run_args(
            "owner-directive-agent",
            "1111111111111111111111111111111111111111",
        );
        lock.prioritize(PrioritizeArgs {
            agent: low.agent.clone(),
            kind: low.kind,
            target: low.target.clone(),
            priority: QueuePriority::Prepared,
        })
        .unwrap();
        lock.prioritize(PrioritizeArgs {
            agent: high.agent.clone(),
            kind: high.kind,
            target: high.target.clone(),
            priority: QueuePriority::OwnerDirective,
        })
        .unwrap();
        let owner = current_process_owner().unwrap();

        assert!(matches!(
            lock.try_global_turn(&low, &owner, "fixture=low", true)
                .unwrap(),
            GlobalTurn::Held(_)
        ));
        assert!(matches!(
            lock.try_global_turn(&high, &owner, "fixture=high", true)
                .unwrap(),
            GlobalTurn::Held(_)
        ));
        assert!(matches!(
            lock.try_global_turn(&low, &owner, "fixture=low", true).unwrap(),
            GlobalTurn::WaitPriority {
                head_agent,
                head_priority: QueuePriority::OwnerDirective,
                own_priority: QueuePriority::Prepared,
                position: 2,
            } if head_agent == high.agent
        ));
        assert_eq!(fs::read_to_string(&paths.anchor).unwrap(), holder_before);
        assert!(
            BoxExclusionAnchor::take(&paths.anchor, 0)
                .unwrap()
                .is_none(),
            "queue ordering must never preempt a live holder"
        );

        drop(live_holder);
        let deadline = Instant::now() + Duration::from_secs(5);
        let admitted = loop {
            match lock
                .try_global_turn(&high, &owner, "fixture=high", true)
                .unwrap()
            {
                GlobalTurn::Acquired { anchors, .. } => break anchors,
                GlobalTurn::Held(_) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                _ => panic!("higher-priority head did not acquire after holder release"),
            }
        };
        assert!(
            BoxExclusionAnchor::take(&paths.anchor, 0)
                .unwrap()
                .is_none(),
            "admitted head must retain the anchor"
        );
        drop(admitted);
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    /// SECOND-BOUNDARY YIELD BRACKET. A caller that has paid its queue wait can
    /// retain that exact queue identity while dropping the anchor. The owner
    /// watch still jumps ahead and acquires naturally; after it releases, the
    /// original caller reacquires without being disposed or preempting anyone.
    #[test]
    fn owner_watch_jumps_retained_waiter_without_disposing_it() {
        let paths = temp_paths("global-priority-owner-watch-jump");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let routine = global_queue_run_args("patient-pr-head", HEX_SHA);
        let watch = global_queue_run_args(
            OWNER_WATCH_AGENT,
            "1111111111111111111111111111111111111111",
        );
        let owner = current_process_owner().unwrap();

        let routine_anchor = match lock
            .try_global_turn(&routine, &owner, "fixture=routine", false)
            .unwrap()
        {
            GlobalTurn::Acquired { anchors, .. } => anchors,
            _ => panic!("routine waiter did not acquire the free boundary"),
        };
        assert!(lock
            .read_global_queue()
            .unwrap()
            .iter()
            .any(|entry| entry.matches(&routine.agent, routine.kind, &routine.target)));

        assert!(matches!(
            lock.try_global_turn(&watch, &owner, "fixture=watch", true)
                .unwrap(),
            GlobalTurn::Held(_)
        ));
        let queue = lock.read_global_queue().unwrap();
        assert_eq!(queue[0].agent, OWNER_WATCH_AGENT);
        assert_eq!(queue[1].agent, routine.agent);

        drop(routine_anchor);
        let watch_anchor = match lock
            .try_global_turn(&watch, &owner, "fixture=watch", true)
            .unwrap()
        {
            GlobalTurn::Acquired { anchors, .. } => anchors,
            _ => panic!("owner watch did not jump the retained waiter"),
        };
        assert!(lock
            .read_global_queue()
            .unwrap()
            .iter()
            .any(|entry| entry.matches(&routine.agent, routine.kind, &routine.target)));
        drop(watch_anchor);

        let resumed = match lock
            .try_global_turn(&routine, &owner, "fixture=routine", true)
            .unwrap()
        {
            GlobalTurn::Acquired { anchors, .. } => anchors,
            _ => panic!("retained waiter did not resume after owner watch release"),
        };
        assert!(lock.read_global_queue().unwrap().is_empty());
        drop(resumed);
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    /// Equal priorities preserve guard-serialized insertion order, including
    /// declarations made within the same one-second timestamp bucket.
    #[test]
    fn equal_priority_global_waiters_are_fifo() {
        let paths = temp_paths("global-priority-fifo");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let holder = BoxExclusionAnchor::take(&paths.anchor, 0).unwrap().unwrap();
        let first = global_queue_run_args("z-first", HEX_SHA);
        let second = global_queue_run_args("a-second", "2222222222222222222222222222222222222222");
        for args in [&first, &second] {
            lock.prioritize(PrioritizeArgs {
                agent: args.agent.clone(),
                kind: args.kind,
                target: args.target.clone(),
                priority: QueuePriority::Prepared,
            })
            .unwrap();
        }
        let owner = current_process_owner().unwrap();
        assert!(matches!(
            lock.try_global_turn(&first, &owner, "fixture=first", true)
                .unwrap(),
            GlobalTurn::Held(_)
        ));
        assert!(matches!(
            lock.try_global_turn(&second, &owner, "fixture=second", true)
                .unwrap(),
            GlobalTurn::WaitPriority { head_agent, .. } if head_agent == first.agent
        ));
        drop(holder);
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    /// A dead active waiter is mechanically pruned rather than wedging every
    /// lower-priority run. UNKNOWN remains an error in the production helper.
    #[test]
    fn dead_global_waiter_is_pruned() {
        let paths = temp_paths("global-priority-dead");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let now = epoch_seconds().unwrap();
        let mut dead_owner = current_process_owner().unwrap();
        dead_owner.pid = u32::MAX;
        let mut queue = vec![GlobalQueueEntry {
            enqueued_at: now,
            priority: QueuePriority::OwnerDirective,
            agent: "dead-priority".into(),
            kind: Kind::Validate,
            target: HEX_SHA.into(),
            owner: Some(dead_owner),
        }];
        lock.prune_and_sort_global_queue(&mut queue, now).unwrap();
        assert!(queue.is_empty());
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    /// TERMINAL/PENDING BRACKET. A terminal declaration disappears, while a
    /// genuinely pending owner directive still outranks a routine active
    /// waiter once the declared work reaches the queue.
    #[test]
    fn terminal_declaration_retires_without_erasing_pending_priority() {
        let paths = temp_paths("global-priority-terminal-retirement");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let completed = global_queue_run_args(
            "completed-owner-directive",
            "1111111111111111111111111111111111111111",
        );
        let pending = global_queue_run_args(
            "pending-owner-directive",
            "2222222222222222222222222222222222222222",
        );
        let routine =
            global_queue_run_args("routine-waiter", "3333333333333333333333333333333333333333");
        for args in [&completed, &pending] {
            lock.prioritize(PrioritizeArgs {
                agent: args.agent.clone(),
                kind: args.kind,
                target: args.target.clone(),
                priority: QueuePriority::OwnerDirective,
            })
            .unwrap();
        }

        lock.retire_priority(RetirePriorityArgs {
            agent: completed.agent.clone(),
            kind: completed.kind,
            target: completed.target.clone(),
            outcome: TerminalOutcome::Completed,
        })
        .unwrap();
        let queue = lock.read_global_queue().unwrap();
        assert!(!queue.iter().any(|entry| entry.matches(
            &completed.agent,
            completed.kind,
            &completed.target
        )));
        assert!(queue.iter().any(|entry| entry.matches(
            &pending.agent,
            pending.kind,
            &pending.target
        )));

        let holder = BoxExclusionAnchor::take(&paths.anchor, 0).unwrap().unwrap();
        let owner = current_process_owner().unwrap();
        assert!(matches!(
            lock.try_global_turn(&routine, &owner, "fixture=routine", true)
                .unwrap(),
            GlobalTurn::Held(_)
        ));
        assert!(matches!(
            lock.try_global_turn(&pending, &owner, "fixture=pending", true)
                .unwrap(),
            GlobalTurn::Held(_)
        ));
        assert!(matches!(
            lock.try_global_turn(&routine, &owner, "fixture=routine", true)
                .unwrap(),
            GlobalTurn::WaitPriority {
                head_agent,
                head_priority: QueuePriority::OwnerDirective,
                own_priority: QueuePriority::Routine,
                position: 2,
            } if head_agent == pending.agent
        ));
        drop(holder);
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    /// NON-PREEMPTION BRACKET. A terminal assertion against an active waiter
    /// refuses without changing a byte of queue authority.
    #[test]
    fn terminal_retirement_refuses_active_waiter_byte_identically() {
        let paths = temp_paths("global-priority-active-retirement");
        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let args = global_queue_run_args("active-owner-directive", HEX_SHA);
        let owner = current_process_owner().unwrap();
        lock.write_global_queue(&[GlobalQueueEntry {
            enqueued_at: epoch_seconds().unwrap(),
            priority: QueuePriority::OwnerDirective,
            agent: args.agent.clone(),
            kind: args.kind,
            target: args.target.clone(),
            owner: Some(owner),
        }])
        .unwrap();
        let before = fs::read(lock.global_queue_path()).unwrap();
        let error = lock
            .retire_priority(RetirePriorityArgs {
                agent: args.agent,
                kind: args.kind,
                target: args.target,
                outcome: TerminalOutcome::Abandoned,
            })
            .unwrap_err();
        assert!(error.to_string().contains("ACTIVE waiter"));
        assert_eq!(fs::read(lock.global_queue_path()).unwrap(), before);
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    #[test]
    fn attributed_anchor_publishes_identity_and_releases_on_owner_death() {
        let dir = env::temp_dir().join(format!(
            "ci-hub-vlock-attributed-anchor-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let anchor = dir.join("validate-box.lock");
        let owner = current_process_owner().unwrap();
        let identity = format!(
            "boot_id={} start_ticks={} agent=fixture kind=validate target={} unit=validate-fixture.service record=/fixture/validate-fixture.json",
            owner.boot_id, owner.start_ticks, HEX_SHA
        );
        let holder = BoxExclusionAnchor::take_attributed(&anchor, 0, &identity)
            .unwrap()
            .expect("attributable holder must acquire");
        let description = BoxExclusionAnchor::describe_holder(&anchor);
        assert!(description.contains(&format!("pid={}", std::process::id())));
        assert!(description.contains(&format!("start_ticks={}", owner.start_ticks)));
        assert!(description.contains("unit=validate-fixture.service"));
        assert!(
            BoxExclusionAnchor::take_attributed(&anchor, 0, &identity)
                .unwrap()
                .is_none(),
            "a live attributable holder must retain exclusion"
        );
        drop(holder);
        assert!(
            BoxExclusionAnchor::take_attributed(&anchor, 5, &identity)
                .unwrap()
                .is_some(),
            "kernel-confirmed owner death/drop must make the anchor reclaimable without a timeout reaper"
        );
        let _ = fs::remove_dir_all(dir);
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
        assert_eq!(
            LockPaths::for_workspace(&workspace, &workspace).anchor,
            path
        );
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

    #[test]
    fn reverie_freshness_brackets_fetched_current_main_ancestry() {
        let (paths, root, seed, target) = current_main_fixture("reverie-fresh-main", "reverie");
        assert!(
            current_main_freshness_check(&root, &target, Kind::ReverieValidate).is_ok(),
            "a Reverie target containing fetched main must be admitted"
        );
        git_success(Some(&seed), &["push", "--quiet", "origin", "main"]);
        assert!(matches!(
            current_main_freshness_check(&root, &target, Kind::ReverieValidate),
            Err(ValidateLockError::StaleBase(message))
                if message.contains("does not contain freshly fetched origin/main")
        ));
        assert!(matches!(
            base_admission_check(&root, "not-a-sha", Kind::ReverieValidate, false),
            Err(ValidateLockError::StaleBase(_))
        ));
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    /// ⚠️ THE THIRD BRACKET: A FROZEN TARGET IS ADMITTED WHILE STAYING BEHIND
    /// MAIN, AND A GENUINELY BAD TARGET IS STILL REFUSED. BOTH DIRECTIONS, OR
    /// THIS KIND IS A HOLE RATHER THAN A GATE.
    ///
    /// The first two brackets already exist:
    /// `reverie_freshness_brackets_fetched_current_main_ancestry` pins
    /// fresh-is-admitted and behind-is-refused, and
    /// `a_waiter_is_evicted_once_main_advances_past_its_target` pins that the
    /// refusal is DEFINITE and therefore evicting. Neither says anything about a
    /// kind that is supposed to survive the advance.
    ///
    /// Direction 1 -- REFUSED WHILE CURRENT, THEN ADMITTED AFTER MAIN MOVES. The
    /// same real repository and real remote advance the other brackets use.
    /// `Kind::FrozenValidate` must not become an always-open alias for ordinary
    /// validation; before the advance it refuses. Afterwards `Kind::Validate`
    /// must refuse (the preserved control) and `Kind::FrozenValidate` must admit
    /// the SAME target at the SAME moment.
    ///
    /// Direction 2 -- STILL REFUSED. `FrozenValidate` changes the moving-main
    /// predicate only. `is_validation()` is true for it, so
    /// `base_admission_check` still runs and a non-sha is refused exactly as it
    /// is for `Kind::Validate`. If someone later moves `FrozenValidate` out of
    /// `is_validation`, THIS assertion is what fails.
    #[test]
    fn a_frozen_target_is_admitted_behind_main_and_a_bad_target_is_still_refused() {
        let (paths, root, seed, target) = current_main_fixture("frozen-behind-main", "hermit");

        // The fixture must start fresh, or "still admitted after the advance"
        // would be indistinguishable from "admitted because nothing moved".
        assert!(
            current_main_freshness_check(&root, &target, Kind::Validate).is_ok(),
            "the fixture must start fresh or neither direction below proves anything"
        );
        assert!(matches!(
            current_main_freshness_check(&root, &target, Kind::FrozenValidate),
            Err(ValidateLockError::StaleBase(message))
                if message.contains("Use ordinary validate for a current target")
        ));

        // main advances underneath the target.
        git_success(Some(&seed), &["push", "--quiet", "origin", "main"]);

        // Match prepare_fresh_checkout's frozen state: its private origin/main
        // is pinned to the measured target. With main now on the sibling
        // history, the removed fetch refspec refuses the non-fast-forward
        // tracking-ref update before the frozen predicate can be evaluated.
        git_success(
            Some(&root.join("hermit")),
            &["update-ref", "refs/remotes/origin/main", &target],
        );
        let removed = Command::new("git")
            .arg("-C")
            .arg(root.join("hermit"))
            .args([
                "fetch",
                "--no-tags",
                "origin",
                "refs/heads/main:refs/remotes/origin/main",
            ])
            .output()
            .unwrap();
        assert!(
            !removed.status.success()
                && String::from_utf8_lossy(&removed.stderr).contains("non-fast-forward"),
            "the removed refspec must reproduce the divergent frozen-checkout refusal: {:?}",
            removed
        );

        // CONTROL: a landing run must now be refused, and DEFINITELY so.
        let landing = current_main_freshness_check(&root, &target, Kind::Validate);
        let error = landing.expect_err("Kind::Validate must refuse a target behind main");
        assert!(
            stale_base_is_definite(&error),
            "the control must be a DEFINITE stale base, or the eviction it guards is \
             not the thing being preserved, got {error:?}"
        );

        // DIRECTION 1: the frozen kind is admitted for the SAME target at the
        // SAME moment the landing kind is refused.
        assert!(
            current_main_freshness_check(&root, &target, Kind::FrozenValidate).is_ok(),
            "a frozen target must be ADMITTED while behind main -- that is the \
             whole reason this kind exists"
        );
        assert_eq!(
            git_success(
                Some(&root.join("hermit")),
                &["rev-parse", "refs/remotes/origin/main"]
            ),
            target,
            "the target's own gates must see the historical main ref after admission"
        );
        assert_ne!(
            git_success(
                None,
                &[
                    "--git-dir",
                    root.join("remote.git").to_str().unwrap(),
                    "rev-parse",
                    "refs/heads/main",
                ]
            ),
            target,
            "resetting the isolated checkout must not change the fetched remote authority"
        );

        // DIRECTION 2: the producer floor still applies. A non-sha is refused
        // for the frozen kind exactly as it is for the landing kind.
        assert!(
            matches!(
                base_admission_check(&root, "not-a-sha", Kind::FrozenValidate, false),
                Err(ValidateLockError::StaleBase(_))
            ),
            "a genuinely bad target must still be REFUSED for the frozen kind; \
             skipping freshness must not become skipping the floor"
        );
        assert!(
            Kind::FrozenValidate.is_validation(),
            "FrozenValidate must remain a validation kind, or base_admission_check \
             returns early and every bad target is admitted"
        );

        // The frozen receipt must never read as canonical current-main authority.
        assert!(
            !matches!(
                Kind::FrozenValidate.as_str(),
                "validate" | "reverie-validate"
            ),
            "frozen-validate must stay outside the canonical holder-kind allowlist"
        );

        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    /// ⚠️ THE EVICTION. A waiter admitted while fresh must be thrown out of the
    /// queue once `origin/main` advances past its target, WITHOUT waiting to reach
    /// the front.
    ///
    /// Measured 2026-08-26: agent(hermit-107) entered the queue fresh for
    /// e3dbcfcbcebf, waited 34m40s, acquired the box, and refused on freshness with
    /// ZERO nodes run. One box and ~28-minute runs means that is not waste, it is
    /// delay imposed on every landable head behind it.
    ///
    /// This drives the real predicate against a real repository and a real remote
    /// advance -- the same scaffolding as
    /// `reverie_freshness_brackets_fetched_current_main_ancestry` -- and then asserts
    /// the eviction DECISION, which is `stale_base_is_definite`.
    #[test]
    fn a_waiter_is_evicted_once_main_advances_past_its_target() {
        let (paths, root, seed, target) = current_main_fixture("evict-main-advanced", "hermit");

        // Before: fresh, so a waiter keeps its position.
        assert!(
            current_main_freshness_check(&root, &target, Kind::Validate).is_ok(),
            "the fixture must start fresh or the eviction below proves nothing"
        );

        // main advances underneath the waiter, exactly as it did four times on
        // 2026-08-26 (11:51, 12:36, 12:47, 13:03).
        git_success(Some(&seed), &["push", "--quiet", "origin", "main"]);

        let verdict = current_main_freshness_check(&root, &target, Kind::Validate);
        let error = verdict.expect_err("an advanced main must make the target stale");
        assert!(
            stale_base_is_definite(&error),
            "a target behind main must be a DEFINITE stale base and therefore evictable, got {error:?}"
        );

        // The eviction is bounded in time by the recheck interval, which is what
        // makes "released within one poll" a claim with a number behind it.
        assert!(
            CURRENT_MAIN_RECHECK_SECONDS <= 60,
            "the recheck interval bounds how long a doomed waiter holds a slot; \
             {CURRENT_MAIN_RECHECK_SECONDS}s is too long to call this an eviction"
        );
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    /// ⚠️ THE CONTROL, AND IT IS THE HALF THAT MAKES THE OTHER ONE MEAN ANYTHING.
    /// An eviction that fires on every recheck would pass the test above and be
    /// catastrophic: it would empty the queue. A test that only exercises the case
    /// which SHOULD evict cannot tell a working eviction from an over-eager one.
    ///
    /// So: main does NOT advance, the check runs repeatedly, and the waiter must
    /// survive every single recheck.
    #[test]
    fn a_still_fresh_waiter_keeps_its_position_across_repeated_rechecks() {
        let (paths, root, _seed, target) = current_main_fixture("evict-control-fresh", "hermit");

        for attempt in 1..=3 {
            let verdict = current_main_freshness_check(&root, &target, Kind::Validate);
            assert!(
                verdict.is_ok(),
                "recheck {attempt}: a waiter whose target still contains main must KEEP its \
                 queue position; evicting it would empty the queue of runs that can be used. \
                 got {verdict:?}"
            );
        }
        let _ = fs::remove_dir_all(paths.lock.parent().unwrap());
    }

    /// ⚠️ AN UNRESOLVED ANSWER IS NOT A STALE TARGET, AND EVICTING ON IT WOULD BE
    /// THE OVER-EAGER BUG WEARING THE RIGHT UNIFORM.
    ///
    /// `current_main_freshness_check` returns `StaleBase` for FOUR conditions. Three
    /// are properties of the network or the moment -- the fetch could not run, the
    /// fetch failed, the ancestry test was unresolved -- and only the fourth is a
    /// property of the TARGET. A mid-wait eviction keyed on the enum variant alone
    /// would throw a perfectly good waiter out of the queue because a fetch timed
    /// out. Those three keep their position and are caught by the existing
    /// front-of-queue check, where failing closed costs one run instead of one slot.
    #[test]
    fn only_a_definite_stale_base_evicts_a_waiter() {
        let definite = ValidateLockError::StaleBase(
            "REFUSE: validation target abc does not contain freshly fetched origin/main. \
             Rebase before validating."
                .to_string(),
        );
        assert!(
            stale_base_is_definite(&definite),
            "the one message that names a property of the TARGET must evict"
        );

        for unresolved in [
            "validate-lock: current origin/main fetch could not run in /x (boom); freshness for abc is UNRESOLVED.",
            "validate-lock: current origin/main fetch failed in /x (git exit Some(128): boom); freshness for abc is UNRESOLVED.",
            "validate-lock: current-main ancestry for abc is UNRESOLVED in /x (git exit Some(129): boom).",
        ] {
            let error = ValidateLockError::StaleBase(unresolved.to_string());
            assert!(
                !stale_base_is_definite(&error),
                "UNRESOLVED must NOT evict -- a network blip is not a stale target: {unresolved}"
            );
        }
    }

    fn git_success(repo: Option<&Path>, args: &[&str]) -> String {
        let mut command = Command::new("git");
        if let Some(repo) = repo {
            command.arg("-C").arg(repo);
        }
        let output = command.args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn current_main_fixture(
        name: &str,
        product_directory: &str,
    ) -> (LockPaths, PathBuf, PathBuf, String) {
        let paths = temp_paths(name);
        let root = paths.lock.parent().unwrap().to_path_buf();
        let remote = root.join("remote.git");
        let seed = root.join("seed");
        let checkout = root.join(product_directory);

        git_success(
            None,
            &[
                "init",
                "--bare",
                "--quiet",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        git_success(
            None,
            &[
                "init",
                "--quiet",
                "--initial-branch=main",
                seed.to_str().unwrap(),
            ],
        );
        git_success(Some(&seed), &["config", "user.name", "Test"]);
        git_success(
            Some(&seed),
            &["config", "user.email", "test@example.invalid"],
        );
        fs::write(seed.join("base"), "base\n").unwrap();
        git_success(Some(&seed), &["add", "base"]);
        git_success(Some(&seed), &["commit", "--quiet", "-m", "base"]);
        git_success(
            Some(&seed),
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git_success(Some(&seed), &["push", "--quiet", "-u", "origin", "main"]);
        git_success(
            None,
            &[
                "clone",
                "--quiet",
                remote.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        git_success(Some(&checkout), &["config", "user.name", "Test"]);
        git_success(
            Some(&checkout),
            &["config", "user.email", "test@example.invalid"],
        );

        fs::write(checkout.join("target"), "target\n").unwrap();
        git_success(Some(&checkout), &["add", "target"]);
        git_success(
            Some(&checkout),
            &["commit", "--quiet", "-m", "validation target"],
        );
        let target = git_success(Some(&checkout), &["rev-parse", "HEAD"]);

        // Prepare, but do not publish, a main commit that the target lacks.
        // The negative bracket publishes it between run's two checks.
        fs::write(seed.join("advanced"), "advanced\n").unwrap();
        git_success(Some(&seed), &["add", "advanced"]);
        git_success(Some(&seed), &["commit", "--quiet", "-m", "main advances"]);
        (paths, root, seed, target)
    }

    #[test]
    fn current_main_freshness_refuses_an_unavailable_checkout() {
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let prior_override = env::var_os(ADMIT_PREFLIGHT_CMD_ENV);
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let root = env::temp_dir().join(format!(
            "ci-hub-current-main-unavailable-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        let result = current_main_freshness_check(&root, HEX_SHA, Kind::Validate);
        if let Some(value) = prior_override {
            env::set_var(ADMIT_PREFLIGHT_CMD_ENV, value);
        }
        assert!(matches!(
            result,
            Err(ValidateLockError::StaleBase(message)) if message.contains("UNRESOLVED")
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_admits_target_containing_fetched_main_at_both_boundaries() {
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let stub = write_stub("fresh-main-admit", "#!/bin/sh\necho OK\nexit 0\n");
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);
        let (paths, root, _seed, target) = current_main_fixture("fresh-main-positive", "hermit");
        let marker = root.join("child-ran");
        let code = ValidateLock {
            paths: paths.clone(),
        }
        .run(
            RunArgs {
                agent: "fresh-main-positive".into(),
                kind: Kind::Validate,
                target,
                run_record: None,
                no_wait: false,
                wait: 0,
                hold: 30,
                child_deadline: 30,
                max: 1,
                skip_base_check: false,
                child: vec![
                    OsString::from("/bin/sh"),
                    OsString::from("-c"),
                    OsString::from("printf ran > \"$1\""),
                    OsString::from("fresh-main-child"),
                    marker.clone().into_os_string(),
                ],
            },
            &root,
        )
        .unwrap();
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let _ = fs::remove_file(&stub);
        assert_eq!(code, 0);
        assert_eq!(fs::read_to_string(marker).unwrap(), "ran");
        assert!(ValidateLock { paths }.read_holder().unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_refuses_when_main_advances_during_wait_before_consuming_lease() {
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let stub = write_stub("advanced-main-admit", "#!/bin/sh\necho OK\nexit 0\n");
        env::set_var(ADMIT_PREFLIGHT_CMD_ENV, &stub);
        let (paths, root, seed, target) = current_main_fixture("fresh-main-advanced", "hermit");
        let marker = root.join("child-must-not-run");
        CURRENT_MAIN_BETWEEN_CHECKS_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                git_success(Some(&seed), &["push", "--quiet", "origin", "main"]);
            }));
        });

        let lock = ValidateLock {
            paths: paths.clone(),
        };
        let code = lock
            .run(
                RunArgs {
                    agent: "fresh-main-advanced".into(),
                    kind: Kind::Validate,
                    target,
                    run_record: None,
                    no_wait: false,
                    wait: 0,
                    hold: 30,
                    child_deadline: 30,
                    max: 1,
                    skip_base_check: false,
                    child: vec![
                        OsString::from("/bin/sh"),
                        OsString::from("-c"),
                        OsString::from("printf forbidden > \"$1\""),
                        OsString::from("stale-main-child"),
                        marker.clone().into_os_string(),
                    ],
                },
                &root,
            )
            .unwrap();
        CURRENT_MAIN_BETWEEN_CHECKS_HOOK.with(|slot| *slot.borrow_mut() = None);
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let _ = fs::remove_file(&stub);

        assert_eq!(code, STALE_BASE_EXIT_CODE);
        assert!(!marker.exists(), "a stale target must not start its child");
        assert!(lock.read_holder().unwrap().is_none());
        assert!(
            matches!(
                BoxExclusionAnchor::observe(&paths.anchor),
                BoxAnchorObservation::Free
            ),
            "a stale target must release the box-global anchor"
        );
        let _ = fs::remove_dir_all(root);
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

    #[test]
    fn base_admission_loads_preflight_from_immutable_tool_root() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prior_override = env::var_os(ADMIT_PREFLIGHT_CMD_ENV);
        env::remove_var(ADMIT_PREFLIGHT_CMD_ENV);
        let root = env::temp_dir().join(format!(
            "ci-hub-vlock-split-preflight-{}-{}",
            std::process::id(),
            epoch_seconds().unwrap()
        ));
        let tool_root = root.join("tool");
        let state_root = root.join("state");
        let tool_preflight = tool_root.join("ci-hub/validate/preflight_validate.py");
        let state_preflight = state_root.join("ci-hub/validate/preflight_validate.py");
        fs::create_dir_all(tool_preflight.parent().unwrap()).unwrap();
        fs::create_dir_all(state_preflight.parent().unwrap()).unwrap();
        fs::write(
            &tool_preflight,
            "from pathlib import Path\nPath(__file__).parents[2].joinpath('tool-preflight-ran').write_text('ok')\n",
        )
        .unwrap();
        fs::write(&state_preflight, "raise SystemExit(2)\n").unwrap();

        let result = base_admission_check(&tool_root, HEX_SHA, Kind::Validate, false);

        if let Some(value) = prior_override {
            env::set_var(ADMIT_PREFLIGHT_CMD_ENV, value);
        }
        assert!(
            result.is_ok(),
            "immutable preflight should authorize: {result:?}"
        );
        assert!(tool_root.join("tool-preflight-ran").is_file());
        assert!(!state_root.join("tool-preflight-ran").exists());
        let _ = fs::remove_dir_all(root);
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
        let Some(_domain_guard) = crate::landing_lock::process_domain_test_guard() else {
            return;
        };
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
                    run_record: None,
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
