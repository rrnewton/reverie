#!/usr/bin/env -S rust-script --force
//! Typed front door for dev-hermit CI state and operations.
//!
//! `--force` makes rust-script ask Cargo to check the `#[path]` modules below.
//! Without it, rust-script keys only on this top-level file and can execute a
//! stale cached binary after `lib/*.rs` changes. Unchanged modules take Cargo's
//! no-op dependency-check path rather than being recompiled.
//!
//! ```cargo
//! [dependencies]
//! chrono = "0.4"
//! clap = { version = "4", features = ["derive"] }
//! fs2 = "0.4"
//! libc = "0.2"
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! sha2 = "0.10"
//! thiserror = "2"
//! ```
//!
//! `libc` is used by `lib/landing_lock.rs` (pidfd / kill process-domain
//! cleanup). A `#[path]` module cannot declare its own dependency, so anything
//! those modules use must be declared HERE. Combined with `--force` above,
//! omitting one does not degrade gracefully and does not fail only the landing
//! subcommand: Cargo fails to build the single binary, so EVERY ci-hub
//! subcommand stops working -- `pr-status`, `newest-green`, `validate-status`,
//! `validate-run`, `close-task`. That is a fleet-wide landing outage from one
//! missing line, so add the dependency in the same commit as the first use.

#![recursion_limit = "256"]

#[path = "lib/history_queries.rs"]
mod history_queries;
#[path = "lib/landing_lock.rs"]
mod landing_lock;
#[path = "lib/measured.rs"]
mod measured;
#[path = "lib/qualifying_receipt.rs"]
mod qualifying_receipt;
#[path = "lib/records.rs"]
mod records;
#[path = "lib/validate_lock.rs"]
mod validate_lock;
// The legacy assessment type remains exercised by its compatibility tests;
// production consumers below use the stricter canonical receipt assessment.
#[allow(dead_code)]
#[path = "lib/validate_status.rs"]
mod validate_status;

use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand, ValueEnum};
use fs2::FileExt;
use history_queries::{
    CellEvidenceCache, FirstBadOutcome, HistoryQueryEngine, NewestGreenCache, NewestGreenOutcome,
};
use records::{HistoryRow, ObligationRecord};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use thiserror::Error;

const DEFAULT_MAIN_RUN_LIMIT: usize = 100;
const DEFAULT_WARN_THRESHOLD: usize = 10;
const DEFAULT_GITHUB_WAIT_SECONDS: u64 = 120;
const DEFAULT_POLL_SECONDS: u64 = 15;
const DEFAULT_AGENT_SNAPSHOT_MAX_AGE_SECONDS: u64 = 10 * 60;
const GATE_FLOOR_POLICY: &str = "registry-effective-floor";
const CANONICAL_VALIDATE_REPO: &str = "rrnewton/hermit";
const ROOT_HELP: &str = r#"Typed front door for dev-hermit CI state and operations

Usage: ci-hub <COMMAND>

START HERE
  Begin with fleet-wide truth and ownership before narrowing the question.
  quickstart              Print the opinionated five-step agent workflow
  health                  Combine main CI, open PRs, and remediation obligations
  active-work             Reconcile tasks, owners, and live ORC agents

HISTORY & FORENSICS
  Ask what is healthy, which branch commit was last green, and where failure began.
  main-health             Query current main-branch GitHub workflow health
  newest-green            Find the newest gate-qualified green commit [default: --branch main]
  first-bad               Find a retained PASS -> FAIL transition for a cell or gate
  validate-status         Check whether one SHA has a clean full-validation receipt
  hosted-status           Check the registered exact-head GitHub job authority
  receipt-digest          Recompute a HistoryRow's canonical exact-row digest
  validate-run            Launch detached validation through ci-hub admission
  validate-stop           Stop detached validate-* user units and verify termination
  ledger                  Read canonical qualified views of the validate ledger
  local-history           Inspect local validate receipts across slots and worktrees
  history                 Query the retained GitHub Actions timeline
  green-time              Estimate carried-forward green/red time and densification gaps
  refresh-history         Ingest fresh GitHub and local history into the local store
  validate-worktrees      Show registered worktree validation runs

PR & RUNNER HEALTH
  Inspect live pull-request admission, runner capacity, and host pressure.
  pr-status               Pull fresh open-PR CI status [alias: fresh]
  runner-health           Summarize self-hosted runners and recent workflows
  load-probe              Measure whether the host can safely start sensitive work
  tick                    Run due scheduled operational-health gates

LANDING & BATCH CONTROL
  Serialize landings and manage the explicit policy state that gates them.
  land-lock               Acquire, renew, inspect, or release the fleet landing mutex
  validate-lock            Box-exclusive gate: one validate/bench at a time (GRANT/QUEUE/REFUSE)
  apply-local-label       Label PR heads backed by a clean full-validation receipt
  ci-mode                 Inspect or change constrained CI admission mode
  batch                   Inspect or edit the named current CI batch
  ci-timeout              Cancel hosted runs starved waiting to start; reroute to local

POST-LAND REMEDIATION
  Arm, discover, watch, and resolve durable exact-SHA verification obligations.
  arm-land                Arm concurrent local and GitHub verification after a land
  verify-landing          Verify a PR replay SHA or commit SHA on a fetched target
  obligations             List open or actionable speculative-land obligations
  inherit-obligations     Let a fresh lander claim obligations from a recycled agent
  watch-obligations       Poll verifier state and trigger failure remediation
  record-obligation-wake  Record an ORC wake-delivery attempt
  resolve-obligation      Record a completed fix-forward or revert

Run `ci-hub <COMMAND> --help` for exhaustive flags and `ci-hub quickstart` for
the short operational workflow.

Options:
  -h, --help     Print help
  -V, --version  Print version
"#;
const AGENT_QUICKSTART: &str = r#"ci-hub agent quickstart

FOR: dev-hermit operational CI truth, local validation receipts, landing locks,
and mandatory post-land remediation. NOT FOR: product test implementation or a
portable CI engine; those stay in Hermit and pinned agent-utils.

1. Start with the composite health signal; never translate pending into green:
     ./ci-hub/ci-hub health
   Nonzero means open remediation, red CI, or unavailable evidence. Read the
   named component before acting.

2. Inspect the narrower source of truth when triaging:
     ./ci-hub/ci-hub fresh
     ./ci-hub/ci-hub main-health
     ./ci-hub/ci-hub runner-health --all
     ./ci-hub/bin/load-probe

3. Recover local validation evidence after an agent or pane disappears:
     ./ci-hub/ci-hub local-history --since YYYY-MM-DD
     ./ci-hub/ci-hub validate-worktrees --runs 10
     ./ci-hub/ci-hub newest-green
     ./ci-hub/ci-hub first-bad CELL_OR_GATE
   Receipts live under ignored/ci-hub and identify slot, SHA, profile, dirty
   state, result, wall seconds, and CPU seconds. Run newest-green immediately
   before each rebase; it rejects green evidence below the current gate-schema
   floor. First-bad reports unobserved commit gaps.

4. Treat post-land obligations as durable work, not notification delivery:
     ./ci-hub/ci-hub obligations --actionable
     ./ci-hub/ci-hub inherit-obligations --agent AGENT --session "$HOSTNAME:$$"
   Do not raw-admin-merge; use the documented land-and-arm path so local and
   GitHub exact-SHA verification are armed together.

5. Serialize fleet landings through the evidence-based mutex:
     ./ci-hub/ci-hub land-lock status
     ./ci-hub/ci-hub land-lock run --agent AGENT --pr PR -- COMMAND...
   Serialize box-exclusive compute (one validate OR bench, so load cannot forge FAILEDs):
     ./ci-hub/ci-hub validate-run --checkout WORKTREE --agent A --target SHA --pr N -- full
   Never force-release another owner. Dead-owner reclamation requires process
   evidence and is built into the lock.

Run from the dev-hermit root. Networked subcommands apply with-proxy internally.
Meaningful work prints estimated and actual wall+CPU cost; quickstart itself is
pure and performs no workspace discovery, filesystem writes, or network calls.
"#;

#[derive(Parser, Debug)]
#[command(
    name = "ci-hub",
    about = "Typed front door for dev-hermit CI state and operations",
    version,
    propagate_version = true,
    override_help = ROOT_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: HubCommand,
}

#[derive(Subcommand, Debug)]
enum HubCommand {
    /// Print the opinionated agent workflow (pure: no files or network).
    Quickstart,
    /// Summarize current-main, open-PR, and speculative-land health.
    Health(HealthArgs),
    /// Reconcile TaskGraph state, task ownership, and live ORC agents.
    ActiveWork(ActiveWorkArgs),
    /// Query current-main GitHub workflow health.
    MainHealth(MainHealthArgs),
    /// Pull fresh open-PR CI status via pinned agent-utils.
    #[command(visible_alias = "fresh")]
    PrStatus(PrStatusArgs),
    /// Run due operational-health gates.
    Tick(PassthroughArgs),
    /// Arm local and GitHub verification for a just-landed SHA.
    ArmLand(ArmLandArgs),
    /// Verify a PR replay SHA or commit SHA against a freshly fetched target.
    #[command(visible_alias = "verify-landed-pr")]
    VerifyLanding(VerifyLandingArgs),
    /// Show speculative-land obligations from the typed JSONL store.
    Obligations(ObligationsArgs),
    /// Discover and acknowledge remediation inherited by this lander instance.
    InheritObligations(InheritObligationsArgs),
    /// Record that ORC sent a wake which has not yet been acknowledged.
    RecordObligationWake(RecordObligationWakeArgs),
    /// Poll open obligations and record verifier transitions.
    WatchObligations(WatchObligationsArgs),
    /// Record a completed fix-forward or revert.
    ResolveObligation(ResolveObligationArgs),
    /// Incrementally ingest GitHub and local CI history.
    RefreshHistory(RefreshHistoryArgs),
    /// Query the local commit/CI history store.
    History(PassthroughArgs),
    /// Green-time over a linear branch history: sparse signal carried forward,
    /// plus the densification plan that shrinks the estimate's error.
    GreenTime(PassthroughArgs),
    /// Query legacy and machine-wide validate-run records.
    LocalHistory(LocalHistoryArgs),
    /// Show validate runs currently registered by each worktree.
    ValidateWorktrees(ValidateWorktreesArgs),
    /// Summarize self-hosted runners and recent workflows.
    RunnerHealth(RunnerHealthArgs),
    /// Gate timing-sensitive work on measured CPU and memory utilization.
    LoadProbe(LoadProbeArgs),
    /// Query the local validate ledger for a commit and print the landing/cache verdict.
    ValidateStatus(ValidateStatusArgs),
    /// Query the registered exact-head hosted job authority.
    HostedStatus(HostedStatusArgs),
    /// Recompute the canonical digest of one HistoryRow read from stdin.
    ReceiptDigest(ReceiptDigestArgs),
    /// Launch a detached validation whose service enters through validate-lock.
    ValidateRun(PassthroughArgs),
    /// Stop detached validate-* user units and verify they terminated.
    ValidateStop(PassthroughArgs),
    /// Read canonical qualified views of the validate ledger.
    Ledger(LedgerArgs),
    /// Find the newest branch commit whose latest local validation passed.
    NewestGreen(NewestGreenArgs),
    /// Find the newest recorded PASS -> FAIL transition for a local cell or gate.
    FirstBad(FirstBadArgs),
    /// Apply `locally-validated` to PRs whose head has a clean full-validate record.
    ApplyLocalLabel(ApplyLocalLabelArgs),
    /// Operate the shared-file landing mutex.
    LandLock(landing_lock::LandLockArgs),
    /// Operate the box-exclusive-compute admission gate (one validate/bench at a time).
    ValidateLock(validate_lock::ValidateLockArgs),
    /// Inspect or switch the committed CI-constrained mode and its GitHub projection.
    CiMode(CiModeArgs),
    /// Inspect or edit the named current CI batch and its ci-batch PR labels.
    Batch(BatchArgs),
    /// Cancel PRs whose hosted CI starved waiting to start and reroute to local validation.
    CiTimeout(CiTimeoutArgs),
}

#[derive(Args, Clone, Debug)]
struct HealthArgs {
    #[arg(long = "repo")]
    repos: Vec<String>,
    #[arg(long, default_value_t = DEFAULT_MAIN_RUN_LIMIT)]
    limit: usize,
    #[arg(long, default_value_t = DEFAULT_WARN_THRESHOLD)]
    warn_threshold: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Clone, Debug)]
struct ActiveWorkArgs {
    /// Read a fresh orc.listAgents() JSON array from this file.
    #[arg(long)]
    agent_snapshot: Option<PathBuf>,
    /// Reject the cached ORC snapshot after this many seconds.
    #[arg(long, default_value_t = DEFAULT_AGENT_SNAPSHOT_MAX_AGE_SECONDS)]
    max_snapshot_age: u64,
    /// Emit the versioned machine-readable report.
    #[arg(long)]
    json: bool,
    /// Emit tick-hub key/value fields.
    #[arg(long)]
    gate: bool,
}

#[derive(Args, Clone, Debug)]
struct MainHealthArgs {
    #[arg(long = "repo")]
    repos: Vec<String>,
    #[arg(long, default_value_t = DEFAULT_MAIN_RUN_LIMIT)]
    limit: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Clone, Debug)]
struct PrStatusArgs {
    #[arg(long = "repo")]
    repos: Vec<String>,
    #[arg(long, default_value_t = DEFAULT_WARN_THRESHOLD)]
    warn_threshold: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Clone, Debug)]
struct PassthroughArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<OsString>,
}

#[derive(Clone, Debug, ValueEnum)]
enum LandMode {
    Admin,
    Speculative,
}

impl LandMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Speculative => "speculative",
        }
    }
}

#[derive(Args, Clone, Debug)]
struct ArmLandArgs {
    sha: String,
    #[arg(long, default_value = "rrnewton/hermit")]
    repo: String,
    /// PR number used to resolve a rebase-merged head to its replayed SHA.
    #[arg(long)]
    pr: Option<u64>,
    #[arg(long)]
    source: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = LandMode::Speculative)]
    land_mode: LandMode,
    #[arg(long)]
    actor: Option<String>,
    #[arg(long, default_value_t = DEFAULT_GITHUB_WAIT_SECONDS)]
    github_wait_seconds: u64,
    #[arg(long, default_value_t = DEFAULT_POLL_SECONDS)]
    poll_seconds: u64,
    #[arg(long)]
    no_dispatch: bool,
    #[arg(long)]
    store: Option<PathBuf>,
}

#[derive(Args, Clone, Debug)]
struct VerifyLandingArgs {
    /// Positive PR number or commit OID (abbreviations are expanded before reporting).
    reference: String,
    /// Human-readable item name included in claim-audit output.
    #[arg(long)]
    item: Option<String>,
    /// OID originally reported for a PR landing; compares it with the replayed merge commit.
    #[arg(long, requires = "item")]
    claimed_oid: Option<String>,
    #[arg(long, default_value = "rrnewton/hermit")]
    repo: String,
    #[arg(long)]
    source: Option<PathBuf>,
    #[arg(long, default_value = "main")]
    target: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Clone, Debug)]
struct ObligationsArgs {
    #[arg(long)]
    all: bool,
    #[arg(long)]
    actionable: bool,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    gate: bool,
    #[arg(long)]
    store: Option<PathBuf>,
}

#[derive(Args, Clone, Debug)]
struct InheritObligationsArgs {
    #[arg(long)]
    agent: String,
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    store: Option<PathBuf>,
}

#[derive(Args, Clone, Debug)]
struct RecordObligationWakeArgs {
    #[arg(long)]
    target: String,
    #[arg(long, default_value = "orc")]
    source: String,
    #[arg(long)]
    store: Option<PathBuf>,
}

#[derive(Args, Clone, Debug)]
struct WatchObligationsArgs {
    #[arg(long)]
    id: Option<String>,
    #[arg(long)]
    once: bool,
    #[arg(long)]
    gate: bool,
    #[arg(long, default_value_t = DEFAULT_POLL_SECONDS)]
    poll_seconds: u64,
    #[arg(long)]
    store: Option<PathBuf>,
}

#[derive(Clone, Debug, ValueEnum)]
enum RemediationKind {
    FixForward,
    Revert,
}

impl RemediationKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::FixForward => "fix-forward",
            Self::Revert => "revert",
        }
    }
}

#[derive(Args, Clone, Debug)]
struct ResolveObligationArgs {
    id: String,
    #[arg(long, value_enum)]
    kind: RemediationKind,
    #[arg(long = "ref")]
    reference: String,
    #[arg(long)]
    started_at: Option<String>,
    #[arg(long)]
    source: Option<PathBuf>,
    #[arg(long)]
    store: Option<PathBuf>,
}

#[derive(Args, Clone, Debug)]
struct RefreshHistoryArgs {
    #[arg(long)]
    full: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    extra: Vec<OsString>,
}

#[derive(Args, Clone, Debug)]
struct LocalHistoryArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    csv: Option<PathBuf>,
    #[arg(long)]
    write_global: bool,
    #[arg(long)]
    since: Option<String>,
    #[arg(long)]
    slot: Option<String>,
    #[arg(long)]
    profiling: bool,
}

#[derive(Args, Clone, Debug)]
struct ValidateWorktreesArgs {
    /// Include this many most-recent registered runs.
    #[arg(long, default_value_t = 0)]
    runs: usize,
    /// Mark a registered worktree stale after this many hours.
    #[arg(long, default_value_t = 24.0)]
    stale_hours: f64,
    /// Emit the machine-readable worktree and run report.
    #[arg(long)]
    json: bool,
    /// Override the canonical parent ignored/ci-hub store.
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

#[derive(Args, Clone, Debug)]
struct RunnerHealthArgs {
    #[arg(long, default_value = "rrnewton/hermit")]
    repo: String,
    #[arg(long)]
    all: bool,
    #[arg(long, default_value_t = DEFAULT_MAIN_RUN_LIMIT)]
    limit: usize,
    #[arg(long, default_value_t = 15)]
    sample: usize,
    #[arg(long)]
    gate: bool,
    #[arg(long)]
    gh: Option<String>,
}

#[derive(Args, Clone, Debug)]
struct LoadProbeArgs {
    #[arg(long, default_value_t = 1.0)]
    sample_seconds: f64,
    #[arg(long, default_value_t = 50.0)]
    max_executing_percent: f64,
    #[arg(long, default_value_t = 10.0)]
    min_memory_available_percent: f64,
    #[arg(long, default_value_t = 5)]
    top: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Clone, Debug)]
struct ValidateStatusArgs {
    /// The commit SHA given POSITIONALLY (equivalent to --sha). Accepts either
    /// `validate-status <SHA>` or `validate-status --sha <SHA>`; the bare form
    /// is the natural reflex, so rejecting it was a usability trap. Supplying
    /// both the positional and --sha, or combining with --pr, is a conflict.
    #[arg(value_name = "SHA", conflicts_with_all = ["sha", "pr"])]
    sha_positional: Option<String>,
    /// The commit SHA (full 40-hex or an unambiguous ledger prefix) to assess.
    #[arg(long, conflicts_with = "pr")]
    sha: Option<String>,
    /// A PR number; its head SHA is resolved via gh and assessed.
    #[arg(long, conflicts_with = "sha")]
    pr: Option<u64>,
    /// Repository used to resolve --pr head SHAs.
    #[arg(long, default_value = "rrnewton/hermit")]
    repo: String,
    /// Override the validate ledger path. Default: the parent-repo ledger named
    /// by validate_status::LEDGER_REL (resolved against the repo root by
    /// ledger_path); the literal lives only in that const, never restated here.
    #[arg(long)]
    ledger: Option<PathBuf>,
    /// Emit the machine-readable verdict report.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Clone, Debug)]
struct ReceiptDigestArgs {
    /// Exact commit the row must describe.
    #[arg(long)]
    sha: String,
    /// Emit a typed JSON report rather than the digest alone.
    #[arg(long)]
    json: bool,
    /// Emit the exact canonical HistoryRow bytes instead of their digest.
    #[arg(long, conflicts_with = "json")]
    canonical_row: bool,
    /// Refuse unless the shared qualifying-receipt predicate accepts the row.
    /// This lets shell consumers bind semantic qualification and the Rust
    /// canonical digest in one authoritative process.
    #[arg(long)]
    require_qualifying: bool,
    /// Refuse unless the row satisfies the complete canonical local-receipt
    /// predicate used by validate-status (including shared policy, typed gate
    /// completeness, raw result, tree, repository, and run identity).
    #[arg(long, conflicts_with = "require_qualifying")]
    require_canonical_qualifying: bool,
    /// Fresh Hermit main tip for the optional final merge-boundary assertion.
    #[arg(long, requires_all = ["current_reverie_base", "repo_checkout", "reverie_checkout"])]
    current_base: Option<String>,
    /// Fresh Reverie main tip at the same boundary.
    #[arg(long)]
    current_reverie_base: Option<String>,
    /// Hermit object store containing `current_base`.
    #[arg(long)]
    repo_checkout: Option<PathBuf>,
    /// Reverie object store containing `current_reverie_base`.
    #[arg(long)]
    reverie_checkout: Option<PathBuf>,
}

#[derive(Args, Clone, Debug)]
struct HostedStatusArgs {
    #[arg(long, default_value = "rrnewton/hermit")]
    repo: String,
    #[arg(long)]
    sha: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Clone, Debug)]
struct LedgerArgs {
    #[command(subcommand)]
    command: LedgerCommand,
}

#[derive(Subcommand, Clone, Debug)]
enum LedgerCommand {
    /// Emit complete, nonempty PASS rows sorted by finished_at.
    QualifiedRows(QualifiedRowsArgs),
    /// Attribute FAILED rows to their per-substep cause by dereferencing log_file.
    AttributeReds(AttributeRedsArgs),
}

#[derive(Args, Clone, Debug)]
struct QualifiedRowsArgs {
    /// Override ignored/validate-run-ledger.jsonl.
    #[arg(long)]
    ledger: Option<PathBuf>,
}

#[derive(Args, Clone, Debug)]
struct AttributeRedsArgs {
    /// Override ignored/validate-run-ledger.jsonl.
    #[arg(long)]
    ledger: Option<PathBuf>,
    /// Only rows whose commit starts with this prefix.
    #[arg(long)]
    commit: Option<String>,
    /// Attribute the most recent N failed rows (0 = all).
    #[arg(long)]
    last: Option<u64>,
    /// Emit JSON, not text.
    #[arg(long)]
    json: bool,
    /// Persist durable per-red-node attribution (verbatim first_error_line) from
    /// each surviving log into ignored/validate-red-attribution.jsonl
    /// (append-only, idempotent), so a red is attributable after its /tmp log
    /// is evicted. Safe to run on every landing.
    #[arg(long)]
    persist: bool,
    /// Maintenance: backfill first_error_line for records an older extractor
    /// persisted null whose log still survives (the idempotency key excludes
    /// first_error_line, so --persist alone never re-fires). Atomic in-place
    /// rewrite of ignored/validate-red-attribution.jsonl — do NOT run
    /// concurrently with --persist.
    #[arg(long)]
    refill: bool,
}

#[derive(Args, Clone, Debug)]
struct HistoryQueryArgs {
    /// Hermit checkout whose first-parent branch history is queried.
    #[arg(long, default_value = "hermit")]
    repo_dir: PathBuf,
    /// Remote branch to walk, newest first.
    #[arg(long, default_value = "main")]
    branch: String,
    /// Override the canonical local validate ledger.
    #[arg(long)]
    ledger: Option<PathBuf>,
    /// Do not refresh origin/<branch> before querying (offline/reproducible use).
    #[arg(long)]
    no_fetch: bool,
    /// Emit a versioned machine-readable report.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Clone, Debug)]
struct NewestGreenArgs {
    #[command(flatten)]
    query: HistoryQueryArgs,
    /// Override the cache path.
    #[arg(long)]
    cache: Option<PathBuf>,
    /// Ignore a valid cache and recompute from the same ledger and branch tip.
    #[arg(long)]
    no_cache: bool,
}

#[derive(Args, Clone, Debug)]
struct FirstBadArgs {
    /// Exact local validate gate, DAG node, or Rust test-function name.
    cell_or_gate: String,
    #[command(flatten)]
    query: HistoryQueryArgs,
}

#[derive(Args, Clone, Debug)]
struct ApplyLocalLabelArgs {
    /// A single PR number to consider.
    #[arg(long, conflicts_with = "all_open")]
    pr: Option<u64>,
    /// Sweep every open PR in the repo, labeling each validated head.
    #[arg(long, conflicts_with = "pr")]
    all_open: bool,
    /// Repository whose PRs are labeled.
    #[arg(long, default_value = "rrnewton/hermit")]
    repo: String,
    /// Override the validate ledger path. Default: the parent-repo ledger named
    /// by validate_status::LEDGER_REL (resolved against the repo root by
    /// ledger_path); the literal lives only in that const, never restated here.
    #[arg(long)]
    ledger: Option<PathBuf>,
    /// Report intended label actions without editing any label.
    #[arg(long)]
    dry_run: bool,
    /// Emit the machine-readable per-PR action report.
    #[arg(long)]
    json: bool,
}

/// Name of the committed source-of-truth mode file, relative to the workspace root.
const CI_MODE_STATE_PATH: &str = "ci-hub/health/ci-mode.json";
/// GitHub repository variable that projects the mode where workflows can read it.
const CI_MODE_VARIABLE: &str = "CI_MODE";
/// Repositories whose auto-fan-out is gated by the mode; both carry the projection.
const CI_MODE_REPOS: [&str; 2] = ["rrnewton/hermit", "rrnewton/reverie"];

/// Name of the committed source-of-truth batch file, relative to the workspace root.
const CI_BATCH_STATE_PATH: &str = "ci-hub/health/ci-batch.json";
/// PR label that projects batch membership where workflows can read it; a PR
/// carrying this label is exempt from the constrained-mode gate and gets CI.
const CI_BATCH_LABEL: &str = "ci-batch";
/// Repository assumed for `--pr N` when no `--repo` is given.
const CI_BATCH_DEFAULT_REPO: &str = "rrnewton/hermit";

/// PR label the ci-timeout reaper applies BEFORE cancelling a starved run, so the
/// `ci-portable-autoretry.yml` guard sees it on the workflow_run:cancelled event
/// and stands down instead of re-firing the hosted lane.
const CI_TIMEOUT_FALLBACK_LABEL: &str = "ci-local-fallback";
/// Append-only audit trail of every ci-timeout cancellation, relative to root.
const CI_TIMEOUT_AUDIT_PATH: &str = "ci-hub/health/ci-timeout-cancellations.jsonl";
/// Default hosted-start-latency threshold (minutes) beyond which a not-yet-started
/// portable run is treated as starved and rerouted to local validation.
const DEFAULT_CI_TIMEOUT_THRESHOLD_MINUTES: u64 = 60;
/// Default cap on open PRs inspected per ci-timeout scan/reap.
const DEFAULT_CI_TIMEOUT_LIMIT: usize = 100;
/// Repository assumed by ci-timeout when no `--repo` is given.
const CI_TIMEOUT_DEFAULT_REPO: &str = "rrnewton/hermit";

#[derive(Args, Clone, Debug)]
struct CiModeArgs {
    #[command(subcommand)]
    command: CiModeCommand,
}

#[derive(Subcommand, Clone, Debug)]
enum CiModeCommand {
    /// Print the committed mode and report drift against the GitHub projection.
    Status(CiModeStatusArgs),
    /// Write the mode, project it to GitHub, and commit the state file to main.
    Set(CiModeSetArgs),
    /// Dispatch targeted CI for one PR head without auto-arming any fan-out.
    Fire(CiModeFireArgs),
}

#[derive(Args, Clone, Debug)]
struct CiModeStatusArgs {
    /// Emit the machine-readable mode-and-drift report.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CiModeValue {
    /// Every PR push auto-fans-out the hosted CI workflows.
    Auto,
    /// Auto-fan-out is suppressed; hosted CI runs only when explicitly fired.
    Constrained,
}

impl CiModeValue {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Constrained => "constrained",
        }
    }
}

#[derive(Args, Clone, Debug)]
struct CiModeSetArgs {
    /// New mode to record and project.
    #[arg(value_enum)]
    mode: CiModeValue,
    /// Operator-supplied justification recorded in the state file.
    #[arg(long)]
    reason: String,
    /// Optional evidence string, e.g. "queued=6 max-age=2h03m".
    #[arg(long)]
    evidence: Option<String>,
    /// Compute and print the intended write without touching files, GitHub, or git.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CiModeLane {
    Portable,
    Privileged,
}

impl CiModeLane {
    fn as_str(self) -> &'static str {
        match self {
            Self::Portable => "portable",
            Self::Privileged => "privileged",
        }
    }
}

#[derive(Args, Clone, Debug)]
struct CiModeFireArgs {
    /// PR number whose head branch receives the targeted dispatch.
    #[arg(long)]
    pr: u64,
    /// Which validation lane to dispatch.
    #[arg(long, value_enum, default_value_t = CiModeLane::Portable)]
    lane: CiModeLane,
    /// Repository owning the PR and the dispatch-only DAG workflow.
    #[arg(long, default_value = "rrnewton/hermit")]
    repo: String,
}

/// Committed source of truth for the CI-constrained mode. Absent file == auto.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct CiModeState {
    mode: String,
    reason: String,
    since: String,
    actor: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    evidence: Option<String>,
}

impl CiModeState {
    fn default_auto() -> Self {
        Self {
            mode: "auto".into(),
            reason: "default: no committed CI-mode state on disk; treated as auto".into(),
            since: String::new(),
            actor: String::new(),
            evidence: None,
        }
    }
}

#[derive(Args, Clone, Debug)]
struct BatchArgs {
    #[command(subcommand)]
    command: BatchCommand,
}

#[derive(Subcommand, Clone, Debug)]
enum BatchCommand {
    /// Print the named current batch and its member PRs (reads the file only).
    Show(BatchShowArgs),
    /// Replace the current batch with a new named batch and label its PRs.
    Set(BatchSetArgs),
    /// Add PR(s) to the current batch and apply the ci-batch label.
    Add(BatchMemberArgs),
    /// Remove PR(s) from the current batch and drop the ci-batch label.
    Remove(BatchMemberArgs),
    /// Clear the current batch, dropping the ci-batch label from every member.
    Clear(BatchClearArgs),
}

#[derive(Args, Clone, Debug)]
struct BatchShowArgs {
    /// Emit the machine-readable batch report.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Clone, Debug)]
struct BatchSetArgs {
    /// Stable descriptive slug naming the batch, e.g. "cpu-timeout-landing".
    name: String,
    /// Operator-supplied justification recorded in the state file.
    #[arg(long)]
    reason: String,
    /// Repository owning every `--pr` in this invocation.
    #[arg(long, default_value = CI_BATCH_DEFAULT_REPO)]
    repo: String,
    /// Initial member PR number(s); each is labelled ci-batch. Repeatable.
    #[arg(long = "pr")]
    prs: Vec<u64>,
    /// Compute and print the intended change without touching files, GitHub, or git.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args, Clone, Debug)]
struct BatchMemberArgs {
    /// Repository owning every `--pr` in this invocation.
    #[arg(long, default_value = CI_BATCH_DEFAULT_REPO)]
    repo: String,
    /// PR number(s) to add or remove. Repeatable; at least one required.
    #[arg(long = "pr", required = true)]
    prs: Vec<u64>,
    /// Compute and print the intended change without touching files, GitHub, or git.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args, Clone, Debug)]
struct BatchClearArgs {
    /// Compute and print the intended change without touching files, GitHub, or git.
    #[arg(long)]
    dry_run: bool,
}

/// One member of a batch: a PR is identified by its owning repo and number, so a
/// batch may span both gated repositories.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct BatchPr {
    repo: String,
    number: u64,
}

/// Committed source of truth for the named current CI batch. Absent file == no
/// batch. Membership is projected to GitHub as the ci-batch label on each PR.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct CiBatchState {
    name: String,
    reason: String,
    since: String,
    actor: String,
    #[serde(default)]
    prs: Vec<BatchPr>,
}

impl CiBatchState {
    fn empty() -> Self {
        Self {
            name: String::new(),
            reason: String::new(),
            since: String::new(),
            actor: String::new(),
            prs: Vec::new(),
        }
    }

    fn is_active(&self) -> bool {
        !self.name.is_empty()
    }
}

#[derive(Args, Clone, Debug)]
struct CiTimeoutArgs {
    #[command(subcommand)]
    command: CiTimeoutCommand,
}

#[derive(Subcommand, Clone, Debug)]
enum CiTimeoutCommand {
    /// Report starved-vs-untouched PRs without mutating anything (read-only).
    Scan(CiTimeoutScanArgs),
    /// Cancel starved runs and reroute to local validation (DRY RUN unless --execute).
    Reap(CiTimeoutReapArgs),
}

#[derive(Args, Clone, Debug)]
struct CiTimeoutScanArgs {
    /// Repository whose open PRs are inspected.
    #[arg(long, default_value = CI_TIMEOUT_DEFAULT_REPO)]
    repo: String,
    /// Hosted-start-latency threshold (minutes) above which a not-started run is starved.
    #[arg(long, default_value_t = DEFAULT_CI_TIMEOUT_THRESHOLD_MINUTES)]
    threshold_minutes: u64,
    /// Emit the machine-readable scan report.
    #[arg(long)]
    json: bool,
    /// Maximum number of open PRs to inspect.
    #[arg(long, default_value_t = DEFAULT_CI_TIMEOUT_LIMIT)]
    limit: usize,
}

#[derive(Args, Clone, Debug)]
struct CiTimeoutReapArgs {
    /// Repository whose open PRs are inspected and, under --execute, mutated.
    #[arg(long, default_value = CI_TIMEOUT_DEFAULT_REPO)]
    repo: String,
    /// Hosted-start-latency threshold (minutes) above which a not-started run is starved.
    #[arg(long, default_value_t = DEFAULT_CI_TIMEOUT_THRESHOLD_MINUTES)]
    threshold_minutes: u64,
    /// Maximum number of open PRs to inspect.
    #[arg(long, default_value_t = DEFAULT_CI_TIMEOUT_LIMIT)]
    limit: usize,
    /// Perform the label/cancel/audit mutations; without it this is a dry run.
    #[arg(long)]
    execute: bool,
    /// Act on exactly this PR number if it qualifies; otherwise act on all starved PRs.
    #[arg(long)]
    pr: Option<u64>,
    /// Emit the machine-readable reap report.
    #[arg(long)]
    json: bool,
}

/// One open-PR row from `gh pr list --json number,headRefName,headRefOid,createdAt`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPrRow {
    number: u64,
    head_ref_name: String,
    head_ref_oid: String,
}

/// One workflow-run row from `gh run list --json databaseId,headSha,status,...`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRunRow {
    database_id: u64,
    head_sha: String,
    status: String,
    created_at: String,
    event: String,
}

/// The `jobs` array from `gh run view <id> --json jobs`.
#[derive(Deserialize)]
struct GhRunJobs {
    #[serde(default)]
    jobs: Vec<GhJobRow>,
}

/// One job from a run's `jobs` array; a job that never started is `queued`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhJobRow {
    status: Option<String>,
    started_at: Option<String>,
}

impl GhJobRow {
    /// A job has STARTED once its status leaves `queued` or it records a start time.
    fn started(&self) -> bool {
        self.status
            .as_deref()
            .map(|s| s != "queued")
            .unwrap_or(false)
            || self
                .started_at
                .as_deref()
                .map(|s| !s.is_empty())
                .unwrap_or(false)
    }
}

/// A qualified starved PR: its portable run is not completed, no job has started,
/// and it has waited past the threshold to begin.
struct StarvedPr {
    pr: u64,
    head_sha: String,
    run_id: u64,
    run_created_at: String,
    wait_seconds: i64,
    jobs_total: usize,
    jobs_started: usize,
}

/// Outcome of the shared qualifier: every open PR is either starved or normally
/// scheduled (started, under threshold, completed, or has no portable run).
struct CiTimeoutQualification {
    now: chrono::DateTime<chrono::Utc>,
    starved: Vec<StarvedPr>,
    scheduled_untouched: Vec<u64>,
}

#[derive(Clone, Debug)]
struct CostSpec {
    tool: &'static str,
    basis: String,
}

impl HubCommand {
    fn cost_spec(&self) -> Option<CostSpec> {
        let spec = match self {
            Self::Health(_) => CostSpec {
                tool: "ci-hub/health",
                basis: "not measured: composite repository/API query cost has no retained history"
                    .into(),
            },
            Self::ActiveWork(_) => CostSpec {
                tool: "ci-hub/active-work",
                basis: "not measured: one local TaskGraph scan plus ORC snapshot reconciliation"
                    .into(),
            },
            Self::MainHealth(args) => CostSpec {
                tool: "ci-hub/main-health",
                basis: format!(
                    "not measured: repo_count={}; no retained commit/workflow-query cost history",
                    if args.repos.is_empty() { 3 } else { args.repos.len() }
                ),
            },
            Self::HostedStatus(_) => CostSpec {
                tool: "ci-hub/hosted-status",
                basis: "not measured: exact-SHA workflow and complete job-set GitHub queries"
                    .into(),
            },
            Self::PrStatus(args) => CostSpec {
                tool: "ci-hub/pr-status",
                basis: format!(
                    "not measured: repo_count={}; no retained planner/GitHub-query cost history",
                    if args.repos.is_empty() { 2 } else { args.repos.len() }
                ),
            },
            Self::Tick(_) => CostSpec {
                tool: "ci-hub/tick",
                basis: "not measured: due health gates vary and tick cost history is not retained"
                    .into(),
            },
            Self::ArmLand(_) => CostSpec {
                tool: "ci-hub/arm-land",
                basis: "not measured: verifier launch and GitHub dispatch cost history is not retained"
                    .into(),
            },
            Self::VerifyLanding(args) => CostSpec {
                tool: "ci-hub/verify-landing",
                basis: if args.reference.len() == 40 {
                    "not measured: one target fetch plus local SHA resolution and ancestry check"
                        .into()
                } else {
                    "not measured: one GitHub PR query plus one target fetch and ancestry check"
                        .into()
                },
            },
            Self::ResolveObligation(_) => CostSpec {
                tool: "ci-hub/resolve-obligation",
                basis: "not measured: one repository-bound main fetch plus exact repair-object and ancestry checks"
                    .into(),
            },
            Self::WatchObligations(args) => CostSpec {
                tool: "ci-hub/watch-obligations",
                basis: if args.once {
                    "not measured: one exact-SHA obligation poll has no retained cost history"
                        .into()
                } else {
                    "not measured: verifier completion time is unknown; polling bounds are not estimates"
                        .into()
                },
            },
            Self::RefreshHistory(args) => CostSpec {
                tool: "ci-hub/refresh-history",
                basis: if args.full {
                    "not measured: full GitHub Actions backfill size and cost are unknown before query"
                        .into()
                } else {
                    "not measured: incremental history scan cost is not retained".into()
                },
            },
            Self::History(_) => CostSpec {
                tool: "ci-hub/history",
                basis: "not measured: history-store scan cost is not retained".into(),
            },
            Self::LocalHistory(_) => CostSpec {
                tool: "ci-hub/local-history",
                basis: "not measured: ledger/store scan cost history is not retained".into(),
            },
            Self::NewestGreen(_) => CostSpec {
                tool: "ci-hub/newest-green",
                basis: "not measured: one bounded branch fetch plus local first-parent/ledger query; cache may avoid the query but not freshness check".into(),
            },
            Self::FirstBad(_) => CostSpec {
                tool: "ci-hub/first-bad",
                basis: "not measured: one bounded main fetch plus local ledger/log/diff query; no tests are executed".into(),
            },
            Self::RunnerHealth(_) => CostSpec {
                tool: "ci-hub/runner-health",
                basis: "not measured: runner/workflow query cost history is not retained".into(),
            },
            Self::LoadProbe(args) => CostSpec {
                tool: "ci-hub/load-probe",
                basis: format!(
                    "not measured: requested sample={:.3}s plus /proc+cgroup scan; retained runtime history not established",
                    args.sample_seconds
                ),
            },
            Self::LandLock(args) if args.command.consumes_meaningful_time() => CostSpec {
                tool: "ci-hub/land-lock",
                basis: "not measured: queue wait and optional child command vary; wait/lease values are bounds, not estimates"
                    .into(),
            },
            Self::ValidateLock(args) if args.command.consumes_meaningful_time() => CostSpec {
                tool: "ci-hub/validate-lock",
                basis: "not measured: queue wait and optional child command vary; wait/lease/child-deadline values are bounds, not estimates"
                    .into(),
            },
            Self::CiTimeout(_) => CostSpec {
                tool: "ci-hub/ci-timeout",
                basis: "not measured: bounded gh queries per open PR plus optional label/cancel mutations; no tests executed here (local validate is enqueued separately under the validate-lock)".into(),
            },
            Self::Obligations(_)
            | Self::InheritObligations(_)
            | Self::RecordObligationWake(_)
            | Self::ValidateWorktrees(_)
            | Self::Quickstart
            | Self::GreenTime(_)
            | Self::Ledger(_)
            | Self::CiMode(_)
            | Self::Batch(_)
            | Self::ValidateStatus(_)
            | Self::ReceiptDigest(_)
            | Self::ValidateRun(_)
            | Self::ValidateStop(_)
            | Self::ApplyLocalLabel(_)
            | Self::LandLock(_)
            | Self::ValidateLock(_) => return None,
        };
        Some(spec)
    }
}

#[derive(Debug, Error)]
enum CiHubError {
    #[error("ci-hub: cannot locate workspace from {0}")]
    Workspace(PathBuf),
    #[error("ci-hub: failed to launch {tool}: {source}")]
    Launch {
        tool: String,
        #[source]
        source: io::Error,
    },
    #[error("ci-hub: cannot open obligation store {path}: {source}")]
    ObligationOpen {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("ci-hub: cannot read obligation store {path}: {source}")]
    ObligationRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("ci-hub: obligation store {path} line {line}: {source}")]
    ObligationJson {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("ci-hub: obligation store {path} line {line} uses schema {schema}, expected 1")]
    ObligationSchema {
        path: PathBuf,
        line: usize,
        schema: u32,
    },
    #[error("ci-hub: local-history --json returned an invalid typed history row: {0}")]
    HistoryJson(#[source] serde_json::Error),
    #[error("ci-hub: cannot read CI-mode state {path}: {source}")]
    CiModeRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("ci-hub: cannot write CI-mode state {path}: {source}")]
    CiModeWrite {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("ci-hub: CI-mode state {path} is not valid JSON: {source}")]
    CiModeJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("ci-hub: cannot read batch state {path}: {source}")]
    CiBatchRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("ci-hub: cannot write batch state {path}: {source}")]
    CiBatchWrite {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("ci-hub: batch state {path} is not valid JSON: {source}")]
    CiBatchJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("ci-hub: cannot read validate ledger {path}: {source}")]
    LedgerRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("ci-hub: validate-status: {0}")]
    ValidateStatus(String),
    #[error("ci-hub: history query: {0}")]
    HistoryQuery(String),
    #[error("ci-hub: gh {context}: {message}")]
    Gh { context: String, message: String },
    #[error(transparent)]
    LandingLock(#[from] landing_lock::LandLockError),
    #[error(transparent)]
    ValidateLock(#[from] validate_lock::ValidateLockError),
}

impl CiHubError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::LandingLock(error) => error.exit_code(),
            Self::ValidateLock(error) => error.exit_code(),
            _ => 2,
        }
    }
}

fn main() -> ExitCode {
    let mut raw: Vec<OsString> = env::args_os().collect();
    if raw.len() == 1 {
        raw.push("--help".into());
    }
    let cli = match Cli::try_parse_from(raw.clone()) {
        Ok(cli) => cli,
        Err(error) => {
            let code = match error.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
                _ => 2,
            };
            let _ = error.print();
            return to_exit_code(code);
        }
    };
    // Agent primers are exploratory documentation. Keep this before workspace
    // discovery, cost wrapping, and every subprocess so it is safe anywhere.
    if matches!(&cli.command, HubCommand::Quickstart) {
        print!("{AGENT_QUICKSTART}");
        return ExitCode::SUCCESS;
    }
    if env::var_os("CI_HUB_DOCS_PARSE_ONLY").is_some() {
        println!(
            "DOCS PARSE OK: {}",
            raw[1..]
                .iter()
                .map(|argument| argument.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        );
        return ExitCode::SUCCESS;
    }
    let root = match workspace_root() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("{error}");
            return to_exit_code(error.exit_code());
        }
    };

    if env::var_os("CI_HUB_TOOL_COST_ACTIVE").is_none() {
        if let Some(spec) = cli.command.cost_spec() {
            match run_costed(&root, &raw[1..], spec) {
                Ok(code) => return to_exit_code(code),
                Err(error) => {
                    eprintln!("{error}");
                    return to_exit_code(error.exit_code());
                }
            }
        }
    }

    match execute(&root, cli.command) {
        Ok(code) => to_exit_code(code),
        Err(error) => {
            eprintln!("{error}");
            to_exit_code(error.exit_code())
        }
    }
}

fn workspace_root() -> Result<PathBuf, CiHubError> {
    let source = env::var_os("RUST_SCRIPT_PATH")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::current_exe().ok())
        .ok_or_else(|| CiHubError::Workspace(PathBuf::from("ci-hub.rs")))?;
    let start = source
        .parent()
        .ok_or_else(|| CiHubError::Workspace(source.clone()))?;
    let output = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|source_error| CiHubError::Launch {
            tool: "git rev-parse".into(),
            source: source_error,
        })?;
    if !output.status.success() {
        return Err(CiHubError::Workspace(start.to_path_buf()));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

fn run_costed(root: &Path, original_args: &[OsString], spec: CostSpec) -> Result<i32, CiHubError> {
    let executable = env::current_exe().map_err(|source| CiHubError::Launch {
        tool: "current ci-hub executable".into(),
        source,
    })?;
    let mut command = Command::new(root.join("ci-hub/bin/tool-cost"));
    command
        .env("CI_HUB_TOOL_COST_ACTIVE", "1")
        .arg("--tool")
        .arg(spec.tool)
        .arg("--estimate-unknown")
        .arg("--basis")
        .arg(spec.basis)
        .arg("--")
        .arg(executable)
        .args(original_args);
    run_status(command, "tool-cost")
}

fn execute(root: &Path, command: HubCommand) -> Result<i32, CiHubError> {
    match command {
        HubCommand::Quickstart => unreachable!("quickstart returns before workspace discovery"),
        HubCommand::Health(args) => {
            let obligation_code = print_obligations(
                root,
                ObligationsArgs {
                    all: false,
                    actionable: false,
                    json: false,
                    gate: false,
                    store: None,
                },
            )?;
            let main_code = run_python(
                root,
                "ci-hub/health/github_main_health.py",
                main_health_arguments(&MainHealthArgs {
                    repos: args.repos.clone(),
                    limit: args.limit,
                    json: args.json,
                }),
            )?;
            let pr_code = run_python(
                root,
                "ci-hub/health/pr_status.py",
                pr_status_arguments(&PrStatusArgs {
                    repos: args.repos,
                    warn_threshold: args.warn_threshold,
                    json: args.json,
                }),
            )?;
            // Owner-existence cross-reference on EVERY health poll: surface any
            // non-terminal task whose owner is no longer a live fleet agent
            // (an agent recycles ~every 30-60 min; the owned task row survives,
            // the owner does not, and nothing else emits a signal for it). This
            // reuses the verified report-only detector; --gate makes it exit 1
            // when a real orphan exists and 3 on a fail-safe unreadable-fleet
            // abort. The detector prints a plain-text report, so it is skipped
            // in --json mode (a JSON poller can call the detector directly).
            let orphan_contrib = if args.json {
                0
            } else {
                match run_bash(
                    root,
                    "scripts/orphaned-task-detector.sh",
                    vec![OsString::from("--gate")],
                )? {
                    // A real orphan turns health non-green so the coordinator
                    // routes it (reassign or close). Exit 1 stays inside the
                    // range the live-health probe classifies as authoritative.
                    1 => 1,
                    // Fail-safe (exit 3, fleet unreadable) or clean (0): printed
                    // but never reddens health — an unreadable fleet must not
                    // masquerade as an orphan wave.
                    _ => 0,
                }
            };
            // Preserve the existing precedence (obligations > main > PR, keeping
            // main/PR's more-informative exit 2); orphans only redden a health
            // poll that would otherwise be green.
            let base = if obligation_code != 0 {
                obligation_code
            } else if main_code != 0 {
                main_code
            } else {
                pr_code
            };
            Ok(if base != 0 { base } else { orphan_contrib })
        }
        HubCommand::ActiveWork(args) => {
            let mut forwarded = vec![OsString::from("active-work")];
            if let Some(snapshot) = args.agent_snapshot {
                push_option(&mut forwarded, "--agent-snapshot", snapshot);
            }
            push_option(
                &mut forwarded,
                "--max-snapshot-age",
                args.max_snapshot_age.to_string(),
            );
            if args.json {
                forwarded.push("--json".into());
            }
            if args.gate {
                forwarded.push("--gate".into());
            }
            run_python(root, "ci-hub/health/operational_health.py", forwarded)
        }
        HubCommand::MainHealth(args) => run_python(
            root,
            "ci-hub/health/github_main_health.py",
            main_health_arguments(&args),
        ),
        HubCommand::PrStatus(args) => run_python(
            root,
            "ci-hub/health/pr_status.py",
            pr_status_arguments(&args),
        ),
        HubCommand::Tick(args) => {
            let mut command = Command::new(agent_tool(root));
            command
                .current_dir(root)
                .args(["tick-hub", "tick", "--config"])
                .arg(root.join("ci-hub/health/tick-hub.yaml"))
                .arg("--state")
                .arg(root.join("ci-hub/health/tick-hub-state.yaml"))
                .arg("--fired-state")
                .arg(root.join(".tick-hub/fired-state"))
                .args(["--current-tick-min", "5"])
                .args(args.args);
            run_status(command, "tick-hub")
        }
        HubCommand::ArmLand(args) => {
            let mut protocol_args = vec![OsString::from("arm"), OsString::from(args.sha)];
            push_option(&mut protocol_args, "--repo", args.repo);
            if let Some(pr) = args.pr {
                push_option(&mut protocol_args, "--pr", pr.to_string());
            }
            push_option(
                &mut protocol_args,
                "--source",
                args.source.unwrap_or_else(|| root.join("hermit")),
            );
            push_option(&mut protocol_args, "--land-mode", args.land_mode.as_str());
            push_option(
                &mut protocol_args,
                "--actor",
                args.actor.unwrap_or_else(default_actor),
            );
            push_option(
                &mut protocol_args,
                "--github-wait-seconds",
                args.github_wait_seconds.to_string(),
            );
            push_option(
                &mut protocol_args,
                "--poll-seconds",
                args.poll_seconds.to_string(),
            );
            if args.no_dispatch {
                protocol_args.push("--no-dispatch".into());
            }
            if let Some(store) = args.store {
                push_option(&mut protocol_args, "--store", store);
            }
            run_python(root, "ci-hub/remediation/protocol.py", protocol_args)
        }
        HubCommand::VerifyLanding(args) => {
            let mut protocol_args = vec![
                OsString::from("verify-landing"),
                OsString::from(args.reference),
            ];
            push_option(&mut protocol_args, "--repo", args.repo);
            push_option(
                &mut protocol_args,
                "--source",
                args.source.unwrap_or_else(|| root.join("hermit")),
            );
            push_option(&mut protocol_args, "--target", args.target);
            if let Some(item) = args.item {
                push_option(&mut protocol_args, "--item", item);
            }
            if let Some(claimed_oid) = args.claimed_oid {
                push_option(&mut protocol_args, "--claimed-oid", claimed_oid);
            }
            if args.json {
                protocol_args.push("--json".into());
            }
            run_python(root, "ci-hub/remediation/protocol.py", protocol_args)
        }
        HubCommand::Obligations(args) => print_obligations(root, args),
        HubCommand::InheritObligations(args) => {
            let mut protocol_args = vec![OsString::from("inherit")];
            push_option(&mut protocol_args, "--agent", args.agent);
            if let Some(session) = args.session {
                push_option(&mut protocol_args, "--session", session);
            }
            if let Some(store) = args.store {
                push_option(&mut protocol_args, "--store", store);
            }
            run_python(root, "ci-hub/remediation/protocol.py", protocol_args)
        }
        HubCommand::RecordObligationWake(args) => {
            let mut protocol_args = vec![OsString::from("wake-sent")];
            push_option(&mut protocol_args, "--target", args.target);
            push_option(&mut protocol_args, "--source", args.source);
            if let Some(store) = args.store {
                push_option(&mut protocol_args, "--store", store);
            }
            run_python(root, "ci-hub/remediation/protocol.py", protocol_args)
        }
        HubCommand::WatchObligations(args) => {
            let mut protocol_args = vec![OsString::from("watch")];
            if let Some(id) = args.id {
                push_option(&mut protocol_args, "--id", id);
            }
            if args.once {
                protocol_args.push("--once".into());
            }
            if args.gate {
                protocol_args.push("--gate".into());
            }
            push_option(
                &mut protocol_args,
                "--poll-seconds",
                args.poll_seconds.to_string(),
            );
            if let Some(store) = args.store {
                push_option(&mut protocol_args, "--store", store);
            }
            run_python(root, "ci-hub/remediation/protocol.py", protocol_args)
        }
        HubCommand::ResolveObligation(args) => {
            let mut protocol_args = vec![OsString::from("resolve"), OsString::from(args.id)];
            push_option(&mut protocol_args, "--kind", args.kind.as_str());
            push_option(&mut protocol_args, "--ref", args.reference);
            if let Some(started_at) = args.started_at {
                push_option(&mut protocol_args, "--started-at", started_at);
            }
            if let Some(source) = args.source {
                push_option(&mut protocol_args, "--source", source);
            }
            if let Some(store) = args.store {
                push_option(&mut protocol_args, "--store", store);
            }
            run_python(root, "ci-hub/remediation/protocol.py", protocol_args)
        }
        HubCommand::RefreshHistory(args) => {
            let ingester = root.join("ci-hub/history/ingest.py");
            let mut forwarded = args.extra;
            if args.full {
                forwarded.insert(0, "--full".into());
            }
            if ingester.is_file() {
                run_python_path(&ingester, forwarded)
            } else {
                eprintln!("ci-hub: unified ingester pending; refreshing local validate history");
                forwarded.insert(0, "--write-global".into());
                run_python(root, "ci-hub/validate/aggregate.py", forwarded)
            }
        }
        HubCommand::GreenTime(args) => {
            run_python_path(&root.join("ci-hub/greentime/timeline.py"), args.args)
        }
        HubCommand::History(args) => {
            let query = root.join("ci-hub/history/query.py");
            if query.is_file() {
                run_python_path(&query, args.args)
            } else {
                run_python(root, "ci-hub/validate/aggregate.py", args.args)
            }
        }
        HubCommand::LocalHistory(args) => run_local_history(root, args),
        HubCommand::ReceiptDigest(args) => run_receipt_digest(args),
        HubCommand::HostedStatus(args) => {
            let mut forwarded = vec![OsString::from("hosted-status")];
            push_option(&mut forwarded, "--repo", args.repo);
            push_option(&mut forwarded, "--sha", args.sha);
            if args.json {
                forwarded.push("--json".into());
            }
            run_python(root, "ci-hub/remediation/protocol.py", forwarded)
        }
        HubCommand::ValidateWorktrees(args) => {
            let mut forwarded = Vec::new();
            push_option(&mut forwarded, "--runs", args.runs.to_string());
            push_option(
                &mut forwarded,
                "--stale-hours",
                args.stale_hours.to_string(),
            );
            if args.json {
                forwarded.push("--json".into());
            }
            if let Some(data_dir) = args.data_dir {
                push_option(&mut forwarded, "--data-dir", data_dir);
            }
            run_python(root, "ci-hub/validate/worktrees.py", forwarded)
        }
        HubCommand::RunnerHealth(args) => {
            let mut forwarded = Vec::new();
            push_option(&mut forwarded, "--repo", args.repo);
            if args.all {
                forwarded.push("--all".into());
            }
            push_option(&mut forwarded, "--limit", args.limit.to_string());
            push_option(&mut forwarded, "--sample", args.sample.to_string());
            if args.gate {
                forwarded.push("--gate".into());
            }
            if let Some(gh) = args.gh {
                push_option(&mut forwarded, "--gh", gh);
            }
            run_python(root, "ci-hub/runners/ci-status.py", forwarded)
        }
        HubCommand::LoadProbe(args) => {
            let mut forwarded = Vec::new();
            push_option(
                &mut forwarded,
                "--sample-seconds",
                args.sample_seconds.to_string(),
            );
            push_option(
                &mut forwarded,
                "--max-executing-percent",
                args.max_executing_percent.to_string(),
            );
            push_option(
                &mut forwarded,
                "--min-memory-available-percent",
                args.min_memory_available_percent.to_string(),
            );
            push_option(&mut forwarded, "--top", args.top.to_string());
            if args.json {
                forwarded.push("--json".into());
            }
            run_python(root, "ci-hub/health/load_probe.py", forwarded)
        }
        HubCommand::ValidateStatus(args) => run_validate_status(root, args),
        HubCommand::ValidateRun(args) => {
            run_python(root, "ci-hub/validate/start_unit.py", args.args)
        }
        HubCommand::ValidateStop(args) => {
            run_python(root, "ci-hub/validate/stop_units.py", args.args)
        }
        HubCommand::Ledger(args) => match args.command {
            LedgerCommand::QualifiedRows(qualified_args) => {
                let mut forwarded = Vec::new();
                if let Some(ledger) = qualified_args.ledger {
                    push_option(&mut forwarded, "--ledger", ledger);
                }
                run_python(root, "ci-hub/validate/qualified_rows.py", forwarded)
            }
            LedgerCommand::AttributeReds(red_args) => {
                let mut forwarded = Vec::new();
                if let Some(ledger) = red_args.ledger {
                    push_option(&mut forwarded, "--ledger", ledger);
                }
                if let Some(commit) = red_args.commit {
                    push_option(&mut forwarded, "--commit", commit);
                }
                if let Some(last) = red_args.last {
                    push_option(&mut forwarded, "--last", last.to_string());
                }
                if red_args.json {
                    forwarded.push("--json".into());
                }
                if red_args.persist {
                    forwarded.push("--persist".into());
                }
                if red_args.refill {
                    forwarded.push("--refill".into());
                }
                run_python(root, "ci-hub/validate/attribute_reds.py", forwarded)
            }
        },
        HubCommand::NewestGreen(args) => run_newest_green(root, args),
        HubCommand::FirstBad(args) => run_first_bad(root, args),
        HubCommand::ApplyLocalLabel(args) => run_apply_local_label(root, args),
        HubCommand::LandLock(args) => landing_lock::execute(root, args).map_err(Into::into),
        HubCommand::ValidateLock(args) => validate_lock::execute(root, args).map_err(Into::into),
        HubCommand::CiMode(args) => match args.command {
            CiModeCommand::Status(status_args) => ci_mode_status(root, status_args),
            CiModeCommand::Set(set_args) => ci_mode_set(root, set_args),
            CiModeCommand::Fire(fire_args) => ci_mode_fire(root, fire_args),
        },
        HubCommand::Batch(args) => match args.command {
            BatchCommand::Show(show_args) => batch_show(root, show_args),
            BatchCommand::Set(set_args) => batch_set(root, set_args),
            BatchCommand::Add(member_args) => batch_add(root, member_args),
            BatchCommand::Remove(member_args) => batch_remove(root, member_args),
            BatchCommand::Clear(clear_args) => batch_clear(root, clear_args),
        },
        HubCommand::CiTimeout(args) => match args.command {
            CiTimeoutCommand::Scan(scan_args) => run_ci_timeout_scan(root, scan_args),
            CiTimeoutCommand::Reap(reap_args) => run_ci_timeout_reap(root, reap_args),
        },
    }
}

fn ci_mode_path(root: &Path) -> PathBuf {
    root.join(CI_MODE_STATE_PATH)
}

/// Load the committed mode. Returns `(state, present)`; an absent file is auto.
fn load_ci_mode(root: &Path) -> Result<(CiModeState, bool), CiHubError> {
    let path = ci_mode_path(root);
    if !path.exists() {
        return Ok((CiModeState::default_auto(), false));
    }
    let raw = std::fs::read_to_string(&path).map_err(|source| CiHubError::CiModeRead {
        path: path.clone(),
        source,
    })?;
    let state =
        serde_json::from_str(&raw).map_err(|source| CiHubError::CiModeJson { path, source })?;
    Ok((state, true))
}

fn ci_mode_actor() -> String {
    if let Ok(session) = env::var("ORC_AGENT_SESSION_ID") {
        if !session.is_empty() {
            return session;
        }
    }
    let host = env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            Command::new("hostname")
                .arg("-s")
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .filter(|value| !value.is_empty())
        });
    match host {
        Some(host) => format!("{host}:{}", std::process::id()),
        None => "unknown".into(),
    }
}

fn on_path(binary: &str) -> bool {
    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|dir| dir.join(binary).is_file()))
        .unwrap_or(false)
}

/// `validate.sh` intentionally points XDG_CONFIG_HOME at an isolated temporary
/// directory. Preserve that isolation, but let the control-plane `gh` child read
/// its dedicated credential directory when the caller did not set one.
fn gh_config_dir() -> Option<PathBuf> {
    env::var_os("GH_CONFIG_DIR").map(PathBuf::from).or_else(|| {
        let candidate = PathBuf::from(env::var_os("HOME")?).join(".config/gh");
        candidate.join("hosts.yml").is_file().then_some(candidate)
    })
}

/// Build a `gh` invocation, prefixing `with-proxy` for external egress when
/// available (mirrors the Python health probes' `with-proxy gh` default).
fn gh_command(root: &Path, args: &[&str]) -> Command {
    let mut command = if on_path("with-proxy") {
        let mut command = Command::new("with-proxy");
        command.arg("gh");
        command
    } else {
        Command::new("gh")
    };
    command.args(args).current_dir(root);
    if let Some(config_dir) = gh_config_dir() {
        command.env("GH_CONFIG_DIR", config_dir);
    }
    command
}

#[derive(Deserialize)]
struct GhVariableRow {
    name: String,
    value: String,
}

/// Read the projected mode variable. `Ok(None)` means the variable is not set
/// (treated as auto); `Err` means the query itself failed (network/auth).
fn read_ci_mode_variable(root: &Path, repo: &str) -> Result<Option<String>, String> {
    let output = gh_command(
        root,
        &["variable", "list", "--repo", repo, "--json", "name,value"],
    )
    .output()
    .map_err(|source| format!("launch gh: {source}"))?;
    if !output.status.success() {
        return Err(format!(
            "gh variable list exited {}: {}",
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let rows: Vec<GhVariableRow> = serde_json::from_slice(&output.stdout)
        .map_err(|source| format!("parse gh json: {source}"))?;
    Ok(rows
        .into_iter()
        .find(|row| row.name == CI_MODE_VARIABLE)
        .map(|row| row.value))
}

fn set_ci_mode_variable(root: &Path, repo: &str, value: &str) -> Result<(), String> {
    let output = gh_command(
        root,
        &[
            "variable",
            "set",
            CI_MODE_VARIABLE,
            "--repo",
            repo,
            "--body",
            value,
        ],
    )
    .output()
    .map_err(|source| format!("launch gh: {source}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "gh variable set exited {}: {}",
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn ci_mode_status(root: &Path, args: CiModeStatusArgs) -> Result<i32, CiHubError> {
    let (state, present) = load_ci_mode(root)?;

    #[derive(Serialize)]
    struct Projection {
        repo: String,
        value: Option<String>,
        projected_mode: String,
        drift: bool,
        error: Option<String>,
    }

    let mut projections = Vec::new();
    let mut any_drift = false;
    let mut any_error = false;
    for repo in CI_MODE_REPOS {
        match read_ci_mode_variable(root, repo) {
            Ok(value) => {
                let projected = value.clone().unwrap_or_else(|| "auto".into());
                let drift = projected != state.mode;
                any_drift |= drift;
                projections.push(Projection {
                    repo: repo.to_string(),
                    value,
                    projected_mode: projected,
                    drift,
                    error: None,
                });
            }
            Err(error) => {
                any_error = true;
                projections.push(Projection {
                    repo: repo.to_string(),
                    value: None,
                    projected_mode: "unknown".into(),
                    drift: false,
                    error: Some(error),
                });
            }
        }
    }

    if args.json {
        #[derive(Serialize)]
        struct Report<'a> {
            file_present: bool,
            mode: &'a str,
            reason: &'a str,
            since: &'a str,
            actor: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            evidence: &'a Option<String>,
            drift: bool,
            projection_error: bool,
            projections: &'a [Projection],
        }
        println!(
            "{}",
            serde_json::to_string(&Report {
                file_present: present,
                mode: &state.mode,
                reason: &state.reason,
                since: &state.since,
                actor: &state.actor,
                evidence: &state.evidence,
                drift: any_drift,
                projection_error: any_error,
                projections: &projections,
            })
            .expect("ci-mode status report is serializable")
        );
    } else {
        println!("CI mode: {}", state.mode.to_uppercase());
        println!(
            "  source: {}",
            if present {
                ci_mode_path(root).display().to_string()
            } else {
                format!("(no {CI_MODE_STATE_PATH}; default auto)")
            }
        );
        if present {
            println!("  reason: {}", state.reason);
            if !state.since.is_empty() {
                println!("  since:  {}", state.since);
            }
            if !state.actor.is_empty() {
                println!("  actor:  {}", state.actor);
            }
            if let Some(evidence) = &state.evidence {
                println!("  evidence: {evidence}");
            }
        }
        println!("GitHub projection ({CI_MODE_VARIABLE}):");
        for projection in &projections {
            match &projection.error {
                Some(error) => println!("  {}: QUERY FAILED: {error}", projection.repo),
                None => {
                    let displayed = projection
                        .value
                        .clone()
                        .unwrap_or_else(|| "<not set> (auto)".into());
                    let marker = if projection.drift { "  <-- DRIFT" } else { "" };
                    println!("  {}: {displayed}{marker}", projection.repo);
                }
            }
        }
        if any_drift {
            println!(
                "DRIFT: committed mode is {} but the GitHub projection disagrees; re-run `ci-hub ci-mode set {}` to reconcile.",
                state.mode, state.mode
            );
        }
        if any_error {
            println!("WARNING: at least one projection query failed; drift is undetermined for those repos.");
        }
    }

    Ok(if any_error {
        2
    } else if any_drift {
        1
    } else {
        0
    })
}

fn ci_mode_set(root: &Path, args: CiModeSetArgs) -> Result<i32, CiHubError> {
    let value = args.mode.as_str();
    let state = CiModeState {
        mode: value.to_string(),
        reason: args.reason,
        since: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        actor: ci_mode_actor(),
        evidence: args.evidence,
    };
    let json = serde_json::to_string_pretty(&state).expect("ci-mode state is serializable") + "\n";
    let path = ci_mode_path(root);

    if args.dry_run {
        println!("DRY RUN: no files, GitHub variables, or commits changed.");
        println!("Would write {}:", path.display());
        println!("{json}");
        println!(
            "Would project {CI_MODE_VARIABLE}={value} to: {}",
            CI_MODE_REPOS.join(", ")
        );
        println!("Would commit {CI_MODE_STATE_PATH} to parent main and push origin HEAD:main.");
        return Ok(0);
    }

    // 1. Write the source of truth first so a projection or git failure never
    //    loses the recorded decision.
    std::fs::write(&path, &json).map_err(|source| CiHubError::CiModeWrite {
        path: path.clone(),
        source,
    })?;
    println!("Wrote {} (mode={value}).", path.display());

    let mut failures: Vec<String> = Vec::new();

    // 2. Project to the GitHub variable on every gated repo.
    for repo in CI_MODE_REPOS {
        match set_ci_mode_variable(root, repo, value) {
            Ok(()) => println!("Projected {CI_MODE_VARIABLE}={value} to {repo}."),
            Err(error) => {
                eprintln!("PROJECTION FAILED for {repo}: {error}");
                failures.push(format!("{repo}: {error}"));
            }
        }
    }

    // 3. Commit and publish through the one serialized parent-main writer.
    //    It freshly fetches origin/main, enforces the CAS, and verifies fresh
    //    post-push ancestry without disturbing another agent's staged work.
    match commit_ci_mode_state(root, value) {
        Ok(true) => {
            println!("Committed and published {CI_MODE_STATE_PATH} to parent main.");
        }
        Ok(false) => println!("No state change to commit (file already matches)."),
        Err(error) => {
            eprintln!("COMMIT FAILED: {error}");
            failures.push(format!("commit: {error}"));
        }
    }

    if failures.is_empty() {
        println!("CI mode set to {value}.");
        Ok(0)
    } else {
        eprintln!(
            "ci-hub ci-mode set: committed decision is {value}, but {} projection/publish step(s) failed: {}",
            failures.len(),
            failures.join("; ")
        );
        Ok(2)
    }
}

/// Returns Ok(true) if a commit was created, Ok(false) if the file was unchanged.
fn commit_ci_mode_state(root: &Path, value: &str) -> Result<bool, String> {
    let dirty = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain", "--", CI_MODE_STATE_PATH])
        .output()
        .map_err(|source| format!("launch git status: {source}"))?;
    if !dirty.status.success() {
        return Err(format!(
            "git status exited {}: {}",
            exit_status_code(dirty.status),
            String::from_utf8_lossy(&dirty.stderr).trim()
        ));
    }
    if dirty.stdout.is_empty() {
        return Ok(false);
    }
    let message = format!("ci-hub: set CI mode to {value}");
    let output = Command::new(root.join("scripts/parent-main-write"))
        .current_dir(root)
        .args(["commit", "-m", &message, "--", CI_MODE_STATE_PATH])
        .output()
        .map_err(|source| format!("launch serialized parent-main writer: {source}"))?;
    if output.status.success() {
        Ok(true)
    } else {
        Err(format!(
            "serialized parent-main writer exited {}: {}",
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn ci_mode_fire(root: &Path, args: CiModeFireArgs) -> Result<i32, CiHubError> {
    let pr = args.pr.to_string();
    // 1. Resolve the PR head branch; the dispatch-only DAG runs at that ref.
    let view = gh_command(
        root,
        &[
            "pr",
            "view",
            &pr,
            "--repo",
            &args.repo,
            "--json",
            "headRefName",
            "-q",
            ".headRefName",
        ],
    )
    .output()
    .map_err(|source| CiHubError::Launch {
        tool: "gh pr view".into(),
        source,
    })?;
    if !view.status.success() {
        eprintln!(
            "ci-hub ci-mode fire: gh pr view #{pr} on {} failed (exit {}): {}",
            args.repo,
            exit_status_code(view.status),
            String::from_utf8_lossy(&view.stderr).trim()
        );
        return Ok(2);
    }
    let head_ref = String::from_utf8_lossy(&view.stdout).trim().to_string();
    if head_ref.is_empty() {
        eprintln!(
            "ci-hub ci-mode fire: could not resolve head branch for PR #{pr} on {}.",
            args.repo
        );
        return Ok(2);
    }

    // 2. Dispatch the workflow_dispatch-only DAG at that head; arm nothing else.
    let lane_arg = format!("lane={}", args.lane.as_str());
    let dispatch = gh_command(
        root,
        &[
            "workflow",
            "run",
            "ci-dag.yml",
            "--repo",
            &args.repo,
            "--ref",
            &head_ref,
            "-f",
            &lane_arg,
        ],
    )
    .status()
    .map_err(|source| CiHubError::Launch {
        tool: "gh workflow run".into(),
        source,
    })?;
    if dispatch.success() {
        println!(
            "Dispatched ci-dag.yml (lane={}) on {} at {} (PR #{pr}).",
            args.lane.as_str(),
            args.repo,
            head_ref
        );
        Ok(0)
    } else {
        eprintln!(
            "ci-hub ci-mode fire: gh workflow run failed (exit {}).",
            exit_status_code(dispatch)
        );
        Ok(2)
    }
}

fn batch_path(root: &Path) -> PathBuf {
    root.join(CI_BATCH_STATE_PATH)
}

/// Load the committed batch. Returns `(state, present)`; an absent file is an
/// empty (inactive) batch.
fn load_batch(root: &Path) -> Result<(CiBatchState, bool), CiHubError> {
    let path = batch_path(root);
    if !path.exists() {
        return Ok((CiBatchState::empty(), false));
    }
    let raw = std::fs::read_to_string(&path).map_err(|source| CiHubError::CiBatchRead {
        path: path.clone(),
        source,
    })?;
    let state =
        serde_json::from_str(&raw).map_err(|source| CiHubError::CiBatchJson { path, source })?;
    Ok((state, true))
}

/// Serialize the batch to its committed on-disk form (pretty + trailing newline,
/// matching ci-mode so a hand-authored seed and a tool write are byte-identical).
fn batch_json(state: &CiBatchState) -> String {
    serde_json::to_string_pretty(state).expect("batch state is serializable") + "\n"
}

/// Create the ci-batch label if it is missing; an "already exists" result is
/// success, so this is idempotent and safe to call before every label edit.
fn ensure_batch_label(root: &Path, repo: &str) -> Result<(), String> {
    let output = gh_command(
        root,
        &[
            "label",
            "create",
            CI_BATCH_LABEL,
            "--repo",
            repo,
            "--color",
            "1D76DB",
            "--description",
            "Current CI batch: exempt from constrained-mode gate; gets CI now.",
        ],
    )
    .output()
    .map_err(|source| format!("launch gh label create: {source}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("already exists") {
        return Ok(());
    }
    Err(format!(
        "gh label create exited {}: {}",
        exit_status_code(output.status),
        stderr.trim()
    ))
}

/// Apply (`--add-label`) or drop (`--remove-label`) the ci-batch label on one PR.
fn edit_batch_label(root: &Path, pr: &BatchPr, add: bool) -> Result<(), String> {
    let flag = if add { "--add-label" } else { "--remove-label" };
    let number = pr.number.to_string();
    let output = gh_command(
        root,
        &[
            "pr",
            "edit",
            &number,
            "--repo",
            &pr.repo,
            flag,
            CI_BATCH_LABEL,
        ],
    )
    .output()
    .map_err(|source| format!("launch gh pr edit: {source}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "gh pr edit #{} on {} exited {}: {}",
            pr.number,
            pr.repo,
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Returns Ok(true) if a commit was created, Ok(false) if the file was unchanged.
fn commit_batch_state(root: &Path, name: &str) -> Result<bool, String> {
    let dirty = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain", "--", CI_BATCH_STATE_PATH])
        .output()
        .map_err(|source| format!("launch git status: {source}"))?;
    if !dirty.status.success() {
        return Err(format!(
            "git status exited {}: {}",
            exit_status_code(dirty.status),
            String::from_utf8_lossy(&dirty.stderr).trim()
        ));
    }
    if dirty.stdout.is_empty() {
        return Ok(false);
    }
    let message = if name.is_empty() {
        "ci-hub: clear CI batch".to_string()
    } else {
        format!("ci-hub: set CI batch to {name}")
    };
    let output = Command::new(root.join("scripts/parent-main-write"))
        .current_dir(root)
        .args(["commit", "-m", &message, "--", CI_BATCH_STATE_PATH])
        .output()
        .map_err(|source| format!("launch serialized parent-main writer: {source}"))?;
    if output.status.success() {
        Ok(true)
    } else {
        Err(format!(
            "serialized parent-main writer exited {}: {}",
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Shared publish path for every mutating batch command: write the source of
/// truth first (a label/git failure never loses the decision), project the
/// ci-batch label additions and removals, then commit and push the state file.
/// `to_add`/`to_drop` are the label deltas; `state` is the already-updated batch.
fn publish_batch(
    root: &Path,
    state: &CiBatchState,
    to_add: &[BatchPr],
    to_drop: &[BatchPr],
) -> Result<i32, CiHubError> {
    let path = batch_path(root);
    std::fs::write(&path, batch_json(state)).map_err(|source| CiHubError::CiBatchWrite {
        path: path.clone(),
        source,
    })?;
    println!("Wrote {}.", path.display());

    let mut failures: Vec<String> = Vec::new();

    // Ensure the label exists once per repo we add it to (removals reference an
    // already-existing label, so only additions need the create).
    let mut repos: Vec<&str> = to_add.iter().map(|pr| pr.repo.as_str()).collect();
    repos.sort_unstable();
    repos.dedup();
    for repo in repos {
        if let Err(error) = ensure_batch_label(root, repo) {
            eprintln!("LABEL ENSURE FAILED for {repo}: {error}");
            failures.push(format!("ensure-label {repo}: {error}"));
        }
    }

    for pr in to_add {
        match edit_batch_label(root, pr, true) {
            Ok(()) => println!("Labelled {} #{} {CI_BATCH_LABEL}.", pr.repo, pr.number),
            Err(error) => {
                eprintln!("LABEL ADD FAILED for {} #{}: {error}", pr.repo, pr.number);
                failures.push(format!("add-label {} #{}: {error}", pr.repo, pr.number));
            }
        }
    }
    for pr in to_drop {
        match edit_batch_label(root, pr, false) {
            Ok(()) => println!("Unlabelled {} #{} {CI_BATCH_LABEL}.", pr.repo, pr.number),
            Err(error) => {
                eprintln!(
                    "LABEL REMOVE FAILED for {} #{}: {error}",
                    pr.repo, pr.number
                );
                failures.push(format!("remove-label {} #{}: {error}", pr.repo, pr.number));
            }
        }
    }

    match commit_batch_state(root, &state.name) {
        Ok(true) => {
            println!("Committed and published {CI_BATCH_STATE_PATH} to parent main.");
        }
        Ok(false) => println!("No state change to commit (file already matches)."),
        Err(error) => {
            eprintln!("COMMIT FAILED: {error}");
            failures.push(format!("commit: {error}"));
        }
    }

    if failures.is_empty() {
        Ok(0)
    } else {
        eprintln!(
            "ci-hub batch: state written, but {} projection/publish step(s) failed: {}",
            failures.len(),
            failures.join("; ")
        );
        Ok(2)
    }
}

fn batch_show(root: &Path, args: BatchShowArgs) -> Result<i32, CiHubError> {
    let (state, present) = load_batch(root)?;
    if args.json {
        #[derive(Serialize)]
        struct Report<'a> {
            file_present: bool,
            active: bool,
            name: &'a str,
            reason: &'a str,
            since: &'a str,
            actor: &'a str,
            prs: &'a [BatchPr],
        }
        println!(
            "{}",
            serde_json::to_string(&Report {
                file_present: present,
                active: state.is_active(),
                name: &state.name,
                reason: &state.reason,
                since: &state.since,
                actor: &state.actor,
                prs: &state.prs,
            })
            .expect("batch report is serializable")
        );
        return Ok(0);
    }
    if !state.is_active() {
        println!("Current batch: NONE");
        println!(
            "  source: {}",
            if present {
                batch_path(root).display().to_string()
            } else {
                format!("(no {CI_BATCH_STATE_PATH}; no batch)")
            }
        );
        return Ok(0);
    }
    println!("Current batch: {}", state.name);
    println!("  source: {}", batch_path(root).display());
    println!("  reason: {}", state.reason);
    if !state.since.is_empty() {
        println!("  since:  {}", state.since);
    }
    if !state.actor.is_empty() {
        println!("  actor:  {}", state.actor);
    }
    if state.prs.is_empty() {
        println!("  PRs:    (none)");
    } else {
        println!("  PRs:");
        for pr in &state.prs {
            println!("    {} #{}", pr.repo, pr.number);
        }
    }
    Ok(0)
}

/// Deduplicate a `--repo` + repeated `--pr` invocation into distinct members.
fn members_from(repo: &str, prs: &[u64]) -> Vec<BatchPr> {
    let mut out: Vec<BatchPr> = Vec::new();
    for &number in prs {
        let pr = BatchPr {
            repo: repo.to_string(),
            number,
        };
        if !out.contains(&pr) {
            out.push(pr);
        }
    }
    out
}

fn batch_set(root: &Path, args: BatchSetArgs) -> Result<i32, CiHubError> {
    let (old, _present) = load_batch(root)?;
    let new_members = members_from(&args.repo, &args.prs);
    let state = CiBatchState {
        name: args.name,
        reason: args.reason,
        since: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        actor: ci_mode_actor(),
        prs: new_members.clone(),
    };
    // Old members no longer present lose the label; new members gain it.
    let to_drop: Vec<BatchPr> = old
        .prs
        .iter()
        .filter(|pr| !new_members.contains(pr))
        .cloned()
        .collect();
    let to_add: Vec<BatchPr> = new_members
        .iter()
        .filter(|pr| !old.prs.contains(pr))
        .cloned()
        .collect();

    if args.dry_run {
        batch_dry_run(root, &state, &to_add, &to_drop);
        return Ok(0);
    }
    publish_batch(root, &state, &to_add, &to_drop)
}

fn batch_add(root: &Path, args: BatchMemberArgs) -> Result<i32, CiHubError> {
    let (mut state, _present) = load_batch(root)?;
    if !state.is_active() {
        eprintln!(
            "ci-hub batch add: no current batch; run `ci-hub batch set <name> --reason ...` first."
        );
        return Ok(2);
    }
    let requested = members_from(&args.repo, &args.prs);
    let to_add: Vec<BatchPr> = requested
        .iter()
        .filter(|pr| !state.prs.contains(pr))
        .cloned()
        .collect();
    if to_add.is_empty() {
        println!("All requested PRs are already in batch {}.", state.name);
        return Ok(0);
    }
    state.prs.extend(to_add.iter().cloned());

    if args.dry_run {
        batch_dry_run(root, &state, &to_add, &[]);
        return Ok(0);
    }
    publish_batch(root, &state, &to_add, &[])
}

fn batch_remove(root: &Path, args: BatchMemberArgs) -> Result<i32, CiHubError> {
    let (mut state, present) = load_batch(root)?;
    if !present || !state.is_active() {
        eprintln!("ci-hub batch remove: no current batch to remove PRs from.");
        return Ok(2);
    }
    let requested = members_from(&args.repo, &args.prs);
    let to_drop: Vec<BatchPr> = requested
        .iter()
        .filter(|pr| state.prs.contains(pr))
        .cloned()
        .collect();
    if to_drop.is_empty() {
        println!("None of the requested PRs are in batch {}.", state.name);
        return Ok(0);
    }
    state.prs.retain(|pr| !to_drop.contains(pr));

    if args.dry_run {
        batch_dry_run(root, &state, &[], &to_drop);
        return Ok(0);
    }
    publish_batch(root, &state, &[], &to_drop)
}

fn batch_clear(root: &Path, args: BatchClearArgs) -> Result<i32, CiHubError> {
    let (old, present) = load_batch(root)?;
    if !present && !old.is_active() {
        println!("No current batch to clear.");
        return Ok(0);
    }
    let to_drop = old.prs.clone();
    let state = CiBatchState::empty();

    if args.dry_run {
        batch_dry_run(root, &state, &[], &to_drop);
        return Ok(0);
    }
    publish_batch(root, &state, &[], &to_drop)
}

fn batch_dry_run(root: &Path, state: &CiBatchState, to_add: &[BatchPr], to_drop: &[BatchPr]) {
    println!("DRY RUN: no files, GitHub labels, or commits changed.");
    println!("Would write {}:", batch_path(root).display());
    println!("{}", batch_json(state));
    for pr in to_add {
        println!(
            "Would add label {CI_BATCH_LABEL} to {} #{}.",
            pr.repo, pr.number
        );
    }
    for pr in to_drop {
        println!(
            "Would remove label {CI_BATCH_LABEL} from {} #{}.",
            pr.repo, pr.number
        );
    }
    println!("Would commit {CI_BATCH_STATE_PATH} to parent main and push origin HEAD:main.");
}

fn agent_tool(root: &Path) -> PathBuf {
    env::var_os("CI_HUB_AGENT_TOOL")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("ci-hub/bin/agent-tool"))
}

/// Seconds elapsed since an RFC3339 timestamp, clamped to 0 (never negative when
/// a clock skew or future-dated run makes `now` precede the timestamp).
fn wait_seconds_since(now: chrono::DateTime<chrono::Utc>, rfc3339: &str) -> i64 {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(ts) => now
            .signed_duration_since(ts.with_timezone(&chrono::Utc))
            .num_seconds()
            .max(0),
        Err(_) => 0,
    }
}

/// Human wait rendering, e.g. `2h 03m` -> `2h 3m`.
fn format_wait_hm(seconds: i64) -> String {
    let secs = seconds.max(0);
    format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
}

/// Exact admission-gated local-validate enqueue command a follow-on runs; this
/// command never runs it here (the validate-lock serializes box-exclusive compute).
fn ci_timeout_enqueue_command(head_sha: &str, pr: u64) -> String {
    format!(
        "ci-hub validate-lock run --kind validate --agent ci-timeout --target {head_sha} -- env PR_NUMBER={pr} ./validate.sh"
    )
}

/// Shared qualifier for scan and reap: classify every open PR as starved (portable
/// run not completed, no job started, waited past threshold) or scheduled-untouched.
/// An `Err` means a gh query hard-failed and the qualification is incomplete.
fn qualify_ci_timeout(
    root: &Path,
    repo: &str,
    threshold_minutes: u64,
    limit: usize,
) -> Result<CiTimeoutQualification, String> {
    let now = chrono::Utc::now();
    let threshold_seconds = (threshold_minutes as i64).saturating_mul(60);
    let limit_str = limit.to_string();
    let output = gh_command(
        root,
        &[
            "pr",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--draft=false",
            "--json",
            "number,headRefName,headRefOid,createdAt",
            "-L",
            &limit_str,
        ],
    )
    .output()
    .map_err(|source| format!("launch gh pr list: {source}"))?;
    if !output.status.success() {
        return Err(format!(
            "gh pr list exited {}: {}",
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let prs: Vec<GhPrRow> = serde_json::from_slice(&output.stdout)
        .map_err(|source| format!("parse gh pr list json: {source}"))?;

    let mut starved = Vec::new();
    let mut scheduled_untouched = Vec::new();
    for pr in &prs {
        match qualify_one_pr(root, repo, pr, now, threshold_seconds)? {
            Some(entry) => starved.push(entry),
            None => scheduled_untouched.push(pr.number),
        }
    }
    Ok(CiTimeoutQualification {
        now,
        starved,
        scheduled_untouched,
    })
}

/// Qualify a single PR. Returns `Some(StarvedPr)` only when its newest exact-head
/// pull_request portable run is not completed, has NOT started, and has waited past
/// the threshold; every other case (no run, completed, started, under threshold) is
/// scheduled-untouched (`None`). An `Err` is a gh query failure for this PR.
fn qualify_one_pr(
    root: &Path,
    repo: &str,
    pr: &GhPrRow,
    now: chrono::DateTime<chrono::Utc>,
    threshold_seconds: i64,
) -> Result<Option<StarvedPr>, String> {
    let output = gh_command(
        root,
        &[
            "run",
            "list",
            "--repo",
            repo,
            "--workflow",
            "ci-portable.yml",
            "--branch",
            &pr.head_ref_name,
            "--json",
            "databaseId,headSha,status,conclusion,createdAt,event",
            "-L",
            "10",
        ],
    )
    .output()
    .map_err(|source| format!("launch gh run list (pr #{}): {source}", pr.number))?;
    if !output.status.success() {
        return Err(format!(
            "gh run list (pr #{}) exited {}: {}",
            pr.number,
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let runs: Vec<GhRunRow> = serde_json::from_slice(&output.stdout)
        .map_err(|source| format!("parse gh run list json (pr #{}): {source}", pr.number))?;
    // Runs arrive newest-first; take the newest whose head matches this exact PR
    // head and which came from a pull_request event. No match => no portable run.
    let Some(run) = runs
        .into_iter()
        .find(|run| run.head_sha == pr.head_ref_oid && run.event == "pull_request")
    else {
        return Ok(None);
    };
    if run.status == "completed" {
        return Ok(None);
    }
    // Threshold is time-WAITING-TO-START, not runtime: inspect jobs so a legitimately
    // long-running job is never killed. Not-started == every job still queued.
    let run_id_str = run.database_id.to_string();
    let jobs_output = gh_command(
        root,
        &["run", "view", &run_id_str, "--repo", repo, "--json", "jobs"],
    )
    .output()
    .map_err(|source| format!("launch gh run view (pr #{}): {source}", pr.number))?;
    if !jobs_output.status.success() {
        return Err(format!(
            "gh run view {} (pr #{}) exited {}: {}",
            run.database_id,
            pr.number,
            exit_status_code(jobs_output.status),
            String::from_utf8_lossy(&jobs_output.stderr).trim()
        ));
    }
    let view: GhRunJobs = serde_json::from_slice(&jobs_output.stdout)
        .map_err(|source| format!("parse gh run view json (pr #{}): {source}", pr.number))?;
    let jobs_total = view.jobs.len();
    let jobs_started = view.jobs.iter().filter(|job| job.started()).count();
    let not_started = if view.jobs.is_empty() {
        run.status == "queued"
    } else {
        jobs_started == 0
    };
    let wait_seconds = wait_seconds_since(now, &run.created_at);
    if not_started && wait_seconds > threshold_seconds {
        Ok(Some(StarvedPr {
            pr: pr.number,
            head_sha: run.head_sha,
            run_id: run.database_id,
            run_created_at: run.created_at,
            wait_seconds,
            jobs_total,
            jobs_started,
        }))
    } else {
        Ok(None)
    }
}

fn run_ci_timeout_scan(root: &Path, args: CiTimeoutScanArgs) -> Result<i32, CiHubError> {
    let qual = match qualify_ci_timeout(root, &args.repo, args.threshold_minutes, args.limit) {
        Ok(qual) => qual,
        Err(message) => {
            eprintln!("ci-hub ci-timeout scan: {message}");
            return Ok(2);
        }
    };
    let now_rfc = qual.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if args.json {
        #[derive(Serialize)]
        struct StarvedReport<'a> {
            pr: u64,
            head_sha: &'a str,
            run_id: u64,
            run_created_at: &'a str,
            wait_seconds: i64,
            jobs_total: usize,
            jobs_started: usize,
        }
        #[derive(Serialize)]
        struct ScanReport<'a> {
            schema_version: u32,
            repo: &'a str,
            threshold_minutes: u64,
            now: &'a str,
            starved: Vec<StarvedReport<'a>>,
            starved_count: usize,
            scheduled_untouched_count: usize,
            scheduled_untouched_prs: &'a [u64],
        }
        let starved: Vec<StarvedReport> = qual
            .starved
            .iter()
            .map(|entry| StarvedReport {
                pr: entry.pr,
                head_sha: &entry.head_sha,
                run_id: entry.run_id,
                run_created_at: &entry.run_created_at,
                wait_seconds: entry.wait_seconds,
                jobs_total: entry.jobs_total,
                jobs_started: entry.jobs_started,
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&ScanReport {
                schema_version: 1,
                repo: &args.repo,
                threshold_minutes: args.threshold_minutes,
                now: &now_rfc,
                starved_count: starved.len(),
                scheduled_untouched_count: qual.scheduled_untouched.len(),
                scheduled_untouched_prs: &qual.scheduled_untouched,
                starved,
            })
            .expect("ci-timeout scan report is serializable")
        );
        return Ok(0);
    }
    for entry in &qual.starved {
        println!(
            "STARVED pr #{} run {} waited {} (threshold {}m)",
            entry.pr,
            entry.run_id,
            format_wait_hm(entry.wait_seconds),
            args.threshold_minutes
        );
    }
    // Both-direction statement is required: never report only the cancellations.
    println!(
        "STARVED: {} PR(s) would be cancelled+rerouted. UNTOUCHED: {} normally-scheduled PR(s) not disturbed.",
        qual.starved.len(),
        qual.scheduled_untouched.len()
    );
    Ok(0)
}

/// Idempotent `gh label create` for the local-fallback label ("already exists" is
/// success), mirroring `ensure_batch_label`.
fn ensure_fallback_label(root: &Path, repo: &str) -> Result<(), String> {
    let output = gh_command(
        root,
        &[
            "label",
            "create",
            CI_TIMEOUT_FALLBACK_LABEL,
            "--repo",
            repo,
            "--color",
            "B60205",
            "--description",
            "ci-hub cancelled this run to reroute to local validation; autoretry stands down.",
        ],
    )
    .output()
    .map_err(|source| format!("launch gh label create: {source}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("already exists") {
        return Ok(());
    }
    Err(format!(
        "gh label create exited {}: {}",
        exit_status_code(output.status),
        stderr.trim()
    ))
}

/// Apply the local-fallback label to one PR. MUST run before the cancel so the
/// workflow_run:cancelled event carries the label and the guard stands down.
fn add_fallback_label(root: &Path, repo: &str, pr: u64) -> Result<(), String> {
    let number = pr.to_string();
    let output = gh_command(
        root,
        &[
            "pr",
            "edit",
            &number,
            "--repo",
            repo,
            "--add-label",
            CI_TIMEOUT_FALLBACK_LABEL,
        ],
    )
    .output()
    .map_err(|source| format!("launch gh pr edit: {source}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "gh pr edit #{pr} exited {}: {}",
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Cancel a hosted run. A cancelled run is classified NO_RESULT (never RED) by all
/// consumers, so cancelling is safe with respect to the health classifiers.
fn cancel_run(root: &Path, repo: &str, run_id: u64) -> Result<(), String> {
    let id = run_id.to_string();
    let output = gh_command(root, &["run", "cancel", &id, "--repo", repo])
        .output()
        .map_err(|source| format!("launch gh run cancel: {source}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "gh run cancel {run_id} exited {}: {}",
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Append one JSONL audit record explaining WHY a run was cancelled. `state` is
/// always "no_result" (never "red"/"fail"): this is the audit trail, and the
/// cancelled GitHub run is already NO_RESULT to the health classifiers.
fn append_ci_timeout_audit(
    root: &Path,
    repo: &str,
    entry: &StarvedPr,
    threshold_minutes: u64,
    at: &str,
    actor: &str,
) -> Result<(), String> {
    #[derive(Serialize)]
    struct AuditRecord<'a> {
        schema_version: u32,
        at: &'a str,
        actor: &'a str,
        repo: &'a str,
        pr: u64,
        head_sha: &'a str,
        run_id: u64,
        wait_seconds: i64,
        threshold_minutes: u64,
        state: &'a str,
        reason: &'a str,
    }
    let reason = format!(
        "portable CI waited {}s (> {}m) to start; cancelled and rerouted to local validation",
        entry.wait_seconds, threshold_minutes
    );
    let record = AuditRecord {
        schema_version: 1,
        at,
        actor,
        repo,
        pr: entry.pr,
        head_sha: &entry.head_sha,
        run_id: entry.run_id,
        wait_seconds: entry.wait_seconds,
        threshold_minutes,
        state: "no_result",
        reason: &reason,
    };
    let line =
        serde_json::to_string(&record).map_err(|source| format!("serialize audit: {source}"))?;
    let path = root.join(CI_TIMEOUT_AUDIT_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|source| format!("create {}: {source}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| format!("open {}: {source}", path.display()))?;
    writeln!(file, "{line}").map_err(|source| format!("write {}: {source}", path.display()))?;
    Ok(())
}

fn run_ci_timeout_reap(root: &Path, args: CiTimeoutReapArgs) -> Result<i32, CiHubError> {
    let qual = match qualify_ci_timeout(root, &args.repo, args.threshold_minutes, args.limit) {
        Ok(qual) => qual,
        Err(message) => {
            eprintln!("ci-hub ci-timeout reap: {message}");
            return Ok(2);
        }
    };
    // --pr narrows to exactly that PR if it qualifies; otherwise all starved PRs.
    let targets: Vec<&StarvedPr> = match args.pr {
        Some(number) => qual
            .starved
            .iter()
            .filter(|entry| entry.pr == number)
            .collect(),
        None => qual.starved.iter().collect(),
    };

    if !args.execute {
        if args.json {
            #[derive(Serialize)]
            struct WouldReap<'a> {
                pr: u64,
                head_sha: &'a str,
                run_id: u64,
                wait_seconds: i64,
                enqueue_command: String,
            }
            #[derive(Serialize)]
            struct DryRunReport<'a> {
                dry_run: bool,
                repo: &'a str,
                threshold_minutes: u64,
                would_reap: Vec<WouldReap<'a>>,
            }
            let would_reap: Vec<WouldReap> = targets
                .iter()
                .map(|entry| WouldReap {
                    pr: entry.pr,
                    head_sha: &entry.head_sha,
                    run_id: entry.run_id,
                    wait_seconds: entry.wait_seconds,
                    enqueue_command: ci_timeout_enqueue_command(&entry.head_sha, entry.pr),
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string(&DryRunReport {
                    dry_run: true,
                    repo: &args.repo,
                    threshold_minutes: args.threshold_minutes,
                    would_reap,
                })
                .expect("ci-timeout dry-run report is serializable")
            );
            return Ok(0);
        }
        println!(
            "DRY RUN (pass --execute to mutate). {} PR(s) would be reaped:",
            targets.len()
        );
        for entry in &targets {
            println!(
                "pr #{} run {} (waited {}):",
                entry.pr,
                entry.run_id,
                format_wait_hm(entry.wait_seconds)
            );
            println!("  1. ensure repo label {CI_TIMEOUT_FALLBACK_LABEL} exists");
            println!(
                "  2. gh pr edit {} --repo {} --add-label {CI_TIMEOUT_FALLBACK_LABEL}",
                entry.pr, args.repo
            );
            println!("  3. gh run cancel {} --repo {}", entry.run_id, args.repo);
            println!("  4. append audit record to {CI_TIMEOUT_AUDIT_PATH} (state=no_result)");
            println!(
                "  5. enqueue: {}",
                ci_timeout_enqueue_command(&entry.head_sha, entry.pr)
            );
        }
        return Ok(0);
    }

    let actor = ci_mode_actor();
    let now_rfc = qual.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut failures = 0usize;

    #[derive(Serialize)]
    struct ReapResult {
        pr: u64,
        head_sha: String,
        run_id: u64,
        wait_seconds: i64,
        label_applied: bool,
        cancelled: bool,
        audit_written: bool,
        enqueue_command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }
    let mut results: Vec<ReapResult> = Vec::new();

    for entry in &targets {
        let enqueue = ci_timeout_enqueue_command(&entry.head_sha, entry.pr);
        let mut result = ReapResult {
            pr: entry.pr,
            head_sha: entry.head_sha.clone(),
            run_id: entry.run_id,
            wait_seconds: entry.wait_seconds,
            label_applied: false,
            cancelled: false,
            audit_written: false,
            enqueue_command: enqueue.clone(),
            error: None,
        };
        // a. ensure the fallback label exists (idempotent).
        if let Err(message) = ensure_fallback_label(root, &args.repo) {
            if !args.json {
                eprintln!("pr #{}: FAILED to ensure label: {message}", entry.pr);
            }
            result.error = Some(format!("ensure label: {message}"));
            failures += 1;
            results.push(result);
            continue;
        }
        // b. apply the label BEFORE the cancel so the cancelled event carries it.
        if let Err(message) = add_fallback_label(root, &args.repo, entry.pr) {
            if !args.json {
                eprintln!("pr #{}: FAILED to apply label: {message}", entry.pr);
            }
            result.error = Some(format!("apply label: {message}"));
            failures += 1;
            results.push(result);
            continue;
        }
        result.label_applied = true;
        // c. cancel the starved run.
        if let Err(message) = cancel_run(root, &args.repo, entry.run_id) {
            // Label is already present, so the guard will already stand down; report,
            // do not try to remove the label.
            if !args.json {
                eprintln!(
                    "pr #{}: FAILED to cancel run {} (label {CI_TIMEOUT_FALLBACK_LABEL} already applied; guard will stand down): {message}",
                    entry.pr, entry.run_id
                );
            }
            result.error = Some(format!(
                "cancel run (label already applied; guard will stand down): {message}"
            ));
            failures += 1;
            results.push(result);
            continue;
        }
        result.cancelled = true;
        // d. append the audit record explaining WHY.
        if let Err(message) = append_ci_timeout_audit(
            root,
            &args.repo,
            entry,
            args.threshold_minutes,
            &now_rfc,
            &actor,
        ) {
            if !args.json {
                eprintln!(
                    "pr #{}: cancelled but FAILED to write audit record: {message}",
                    entry.pr
                );
            }
            result.error = Some(format!("audit record: {message}"));
            failures += 1;
            results.push(result);
            continue;
        }
        result.audit_written = true;
        if !args.json {
            println!(
                "pr #{}: labelled {CI_TIMEOUT_FALLBACK_LABEL}, cancelled run {}, audited.",
                entry.pr, entry.run_id
            );
            // e. emit the enqueue command; do NOT run it here.
            println!("  enqueue: {enqueue}");
        }
        results.push(result);
    }

    if args.json {
        #[derive(Serialize)]
        struct ExecReport<'a> {
            dry_run: bool,
            repo: &'a str,
            threshold_minutes: u64,
            reaped: &'a [ReapResult],
            failures: usize,
        }
        println!(
            "{}",
            serde_json::to_string(&ExecReport {
                dry_run: false,
                repo: &args.repo,
                threshold_minutes: args.threshold_minutes,
                reaped: &results,
                failures,
            })
            .expect("ci-timeout reap report is serializable")
        );
    } else {
        println!(
            "REAPED: {} succeeded, {} failed of {} starved PR(s).",
            results
                .iter()
                .filter(|result| result.error.is_none())
                .count(),
            failures,
            targets.len()
        );
    }
    Ok(if failures > 0 { 2 } else { 0 })
}

fn main_health_arguments(args: &MainHealthArgs) -> Vec<OsString> {
    let mut forwarded = Vec::new();
    for repo in &args.repos {
        push_option(&mut forwarded, "--repo", repo);
    }
    push_option(&mut forwarded, "--limit", args.limit.to_string());
    if args.json {
        forwarded.push("--json".into());
    }
    forwarded
}

fn pr_status_arguments(args: &PrStatusArgs) -> Vec<OsString> {
    let mut forwarded = Vec::new();
    for repo in &args.repos {
        push_option(&mut forwarded, "--repo", repo);
    }
    push_option(
        &mut forwarded,
        "--warn-threshold",
        args.warn_threshold.to_string(),
    );
    if args.json {
        forwarded.push("--json".into());
    }
    forwarded
}

fn run_local_history(root: &Path, args: LocalHistoryArgs) -> Result<i32, CiHubError> {
    let mut forwarded = Vec::new();
    if args.json {
        forwarded.push("--json".into());
    }
    if let Some(csv) = args.csv {
        push_option(&mut forwarded, "--csv", csv);
    }
    if args.write_global {
        forwarded.push("--write-global".into());
    }
    if let Some(since) = args.since {
        push_option(&mut forwarded, "--since", since);
    }
    if let Some(slot) = args.slot {
        push_option(&mut forwarded, "--slot", slot);
    }
    if args.profiling {
        forwarded.push("--profiling".into());
    }

    if args.json && !args.write_global && !args.profiling {
        let output = Command::new("python3")
            .arg(root.join("ci-hub/validate/aggregate.py"))
            .args(&forwarded)
            .output()
            .map_err(|source| CiHubError::Launch {
                tool: "local-history".into(),
                source,
            })?;
        io::stderr().write_all(&output.stderr).ok();
        if !output.status.success() {
            io::stdout().write_all(&output.stdout).ok();
            return Ok(exit_status_code(output.status));
        }
        let _: Vec<HistoryRow> =
            serde_json::from_slice(&output.stdout).map_err(CiHubError::HistoryJson)?;
        io::stdout().write_all(&output.stdout).ok();
        return Ok(0);
    }
    run_python(root, "ci-hub/validate/aggregate.py", forwarded)
}

/// Resolve the validate ledger path, honoring an explicit override.
fn ledger_path(root: &Path, override_path: &Option<PathBuf>) -> PathBuf {
    override_path
        .clone()
        .unwrap_or_else(|| root.join(validate_status::LEDGER_REL))
}

/// Load and parse the validate ledger. A missing ledger is an empty history (a
/// commit is simply NOT validated), never an error. Unparseable lines are
/// skipped with a warning so one bad append never blinds the whole query.
fn load_ledger_rows(path: &Path) -> Result<Vec<HistoryRow>, CiHubError> {
    let buf = match std::fs::read_to_string(path) {
        Ok(buf) => buf,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(CiHubError::LedgerRead {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let (rows, skipped) = validate_status::parse_ledger(&buf);
    if skipped > 0 {
        eprintln!(
            "ci-hub: validate-status: skipped {skipped} unparseable ledger line(s) in {}",
            path.display()
        );
    }
    Ok(rows)
}

/// Read a PR's current head commit via gh (`with-proxy` when available).
fn gh_pr_head(root: &Path, repo: &str, pr: u64) -> Result<String, CiHubError> {
    let pr_arg = pr.to_string();
    let output = gh_command(
        root,
        &[
            "pr",
            "view",
            &pr_arg,
            "--repo",
            repo,
            "--json",
            "headRefOid",
            "-q",
            ".headRefOid",
        ],
    )
    .output()
    .map_err(|source| CiHubError::Launch {
        tool: "gh pr view".into(),
        source,
    })?;
    if !output.status.success() {
        return Err(CiHubError::Gh {
            context: format!("pr view #{pr}"),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let sha = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_ascii_lowercase();
    if sha.is_empty() {
        return Err(CiHubError::Gh {
            context: format!("pr view #{pr}"),
            message: "empty headRefOid".into(),
        });
    }
    Ok(sha)
}

/// List open PR numbers in the repo.
fn gh_open_prs(root: &Path, repo: &str) -> Result<Vec<u64>, CiHubError> {
    let output = gh_command(
        root,
        &[
            "pr",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number",
            "-q",
            ".[].number",
        ],
    )
    .output()
    .map_err(|source| CiHubError::Launch {
        tool: "gh pr list".into(),
        source,
    })?;
    if !output.status.success() {
        return Err(CiHubError::Gh {
            context: "pr list".into(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .collect())
}

const LOCALLY_VALIDATED_LABEL: &str = "locally-validated";
const VALIDATION_RECEIPT_REPO: &str = "rrnewton/dev-hermit";
const VALIDATION_RECEIPT_BRANCH: &str = "validation-receipts";
const RECEIPT_CANONICALIZATION: &str = "serde_json::to_vec(HistoryRow)-v1";

#[derive(Clone, Debug)]
struct QualifyingReceipt {
    row: HistoryRow,
    canonical_row_json: String,
    selected_tests: i64,
    discovered_tests: i64,
    count_derivation: &'static str,
    coverage_basis: &'static str,
    canonical_sha256: String,
    producer_definition: Option<ProducerDefinitionEvidence>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProducerDefinitionEvidence {
    definition: BTreeMap<String, String>,
    coverage_status: String,
    paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    valid_commits: Option<Vec<String>>,
    resolved_from: String,
}

#[derive(Debug)]
struct CanonicalReceiptAssessment {
    sha: String,
    verdict: validate_status::Verdict,
    qualifying: Vec<QualifyingReceipt>,
    disqualified: Vec<HistoryRow>,
    failed_records: usize,
    withheld_nonpass_records: usize,
}

fn is_oid(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn extra_str<'a>(row: &'a HistoryRow, key: &str) -> Option<&'a str> {
    row.extra.get(key).and_then(|value| value.as_str())
}

fn canonical_row_json_and_sha256(row: &HistoryRow) -> Option<(String, String)> {
    // Canonicalization contract v1: serde's struct field order, BTreeMap order
    // for flattened extras, and original Vec order. Any parsed receipt field,
    // gate, count, or unknown extension therefore changes this digest.
    let canonical_row_json = serde_json::to_string(row).ok()?;
    let digest = format!("{:x}", Sha256::digest(canonical_row_json.as_bytes()));
    Some((canonical_row_json, digest))
}

fn run_receipt_digest(args: ReceiptDigestArgs) -> Result<i32, CiHubError> {
    if !is_oid(&args.sha) || args.sha.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(CiHubError::ValidateStatus(
            "receipt-digest --sha must be exactly 40 lowercase hex characters".into(),
        ));
    }
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| CiHubError::ValidateStatus(format!("cannot read HistoryRow: {error}")))?;
    let row: HistoryRow = serde_json::from_str(&input).map_err(|error| {
        CiHubError::ValidateStatus(format!("receipt-digest input is not a HistoryRow: {error}"))
    })?;
    if row.commit.as_deref() != Some(args.sha.as_str()) {
        return Err(CiHubError::ValidateStatus(
            "receipt-digest HistoryRow is not bound to --sha".into(),
        ));
    }
    if args.require_canonical_qualifying {
        if qualify_canonical_receipt(&row, &args.sha).is_none() {
            return Err(CiHubError::ValidateStatus(
                "receipt-digest HistoryRow does not satisfy the complete canonical qualifying predicate"
                    .into(),
            ));
        }
    } else if args.require_qualifying
        && !crate::qualifying_receipt::row_qualifies(
            &row,
            &args.sha,
            crate::qualifying_receipt::active(),
        )
    {
        return Err(CiHubError::ValidateStatus(
            "receipt-digest HistoryRow does not satisfy the shared qualifying predicate".into(),
        ));
    }
    let boundary_count = [
        args.current_base.is_some(),
        args.current_reverie_base.is_some(),
        args.repo_checkout.is_some(),
        args.reverie_checkout.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if boundary_count != 0 && boundary_count != 4 {
        return Err(CiHubError::ValidateStatus(
            "receipt-digest final merge-boundary verification requires all four base/checkouts arguments".into(),
        ));
    }
    if let (
        Some(current_base),
        Some(current_reverie_base),
        Some(repo_checkout),
        Some(reverie_checkout),
    ) = (
        args.current_base.as_deref(),
        args.current_reverie_base.as_deref(),
        args.repo_checkout.as_deref(),
        args.reverie_checkout.as_deref(),
    ) {
        if !crate::qualifying_receipt::shared_base_boundary_accepts(
            &row,
            current_base,
            current_reverie_base,
            repo_checkout,
            reverie_checkout,
        ) {
            return Err(CiHubError::ValidateStatus(
                "receipt-digest HistoryRow failed final merge-boundary base verification".into(),
            ));
        }
    }
    let (canonical_row, digest) = canonical_row_json_and_sha256(&row).ok_or_else(|| {
        CiHubError::ValidateStatus("cannot canonicalize receipt HistoryRow".into())
    })?;
    if args.canonical_row {
        print!("{canonical_row}");
    } else if args.json {
        println!(
            "{}",
            serde_json::json!({
                "schema_version": 1,
                "sha": args.sha,
                "digest_algorithm": "sha256",
                "canonicalization": RECEIPT_CANONICALIZATION,
                "digest": digest,
            })
        );
    } else {
        println!("{digest}");
    }
    Ok(0)
}

/// Return the complete evidence bundle iff this row alone proves a canonical
/// Hermit full-validation receipt. This is the sole positive receipt predicate
/// consumed by validate-status and label application.
fn qualify_canonical_receipt(row: &HistoryRow, sha: &str) -> Option<QualifyingReceipt> {
    let schema_version = row.schema_version?;
    let repo_bound = matches!(
        row.repo.as_deref(),
        Some("hermit") | Some(CANONICAL_VALIDATE_REPO)
    ) || (schema_version == 4 && row.repo.is_none());
    if !crate::qualifying_receipt::row_qualifies(row, sha, crate::qualifying_receipt::active())
        || schema_version < 4
        || row.commit.as_deref() != Some(sha)
        || !is_oid(sha)
        || !repo_bound
        || row.commit_anchored != Some(true)
        || row.tree_dirty != Some(false)
        || row.profile.as_deref() != Some("full")
        || row.selection_mode.as_deref() != Some("full")
        || row.result.as_deref() != Some("pass")
        || extra_str(row, "raw_result") != Some("pass")
        || row.exit_code != Some(0)
        || row.failures != Some(0)
    {
        return None;
    }

    let tree = extra_str(row, "tree")?;
    if !is_oid(tree) {
        return None;
    }
    let checks = row.checks?;
    let gates_run = row.gates_run?;
    let gates_expected = row.gates_expected?;
    if gates_expected == 0
        || gates_run < gates_expected
        || checks != gates_run
        || usize::try_from(gates_run).ok()? != row.gates.len()
        || row.gates.iter().any(|gate| {
            gate.name.trim().is_empty()
                || gate.result.as_deref() != Some("pass")
                || gate.exit_code != Some(0)
        })
    {
        return None;
    }

    let executed_tests = row.executed_tests?;
    let filtered_tests = row.filtered_tests?;
    if executed_tests <= 0 || filtered_tests < 0 {
        return None;
    }
    // Schema-4 has only libtest executed+filtered aggregates. Preserve the
    // genuine 630f receipt without pretending these are producer-native fields:
    // expose the arithmetic and its source explicitly in the report.
    let selected_tests = executed_tests;
    let discovered_tests = executed_tests.checked_add(filtered_tests)?;
    if selected_tests <= 0 || discovered_tests <= 0 {
        return None;
    }

    let coverage_basis = if row.coverage.is_some() {
        if !row
            .coverage
            .as_ref()
            .is_some_and(crate::qualifying_receipt::coverage_satisfied)
        {
            return None;
        }
        "declared-per-node"
    } else if schema_version == 4 {
        "legacy-schema4-full-gates-and-aggregate-counts"
    } else {
        // Schema-5+ declares per-node coverage capability. Missing coverage is
        // a malformed receipt, not permission to fall back to the legacy rule.
        return None;
    };

    for required in [
        row.finished_at.as_deref(),
        row.host.as_deref(),
        row.slot.as_deref(),
        row.log_file.as_deref(),
    ] {
        if required.is_none_or(|value| value.trim().is_empty()) {
            return None;
        }
    }
    let (canonical_row_json, canonical_sha256) = canonical_row_json_and_sha256(row)?;
    Some(QualifyingReceipt {
        row: row.clone(),
        canonical_row_json,
        selected_tests,
        discovered_tests,
        count_derivation:
            "selected_tests=executed_tests;discovered_tests=executed_tests+filtered_tests",
        coverage_basis,
        canonical_sha256,
        producer_definition: None,
    })
}

#[cfg(not(test))]
fn resolve_producer_definition_evidence(
    root: &Path,
    row: &HistoryRow,
    sha: &str,
) -> Result<Option<ProducerDefinitionEvidence>, String> {
    let verifier = root.join("ci-hub/qualifying_receipt.py");
    let checkout = root.join("hermit");
    let mut child = Command::new("python3")
        .arg(verifier)
        .args(["--producer-definition-row", "--sha", sha, "--repo-checkout"])
        .arg(checkout)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot launch producer-definition resolver: {error}"))?;
    serde_json::to_writer(
        child
            .stdin
            .take()
            .ok_or_else(|| "producer-definition resolver stdin unavailable".to_string())?,
        row,
    )
    .map_err(|error| format!("cannot send row to producer-definition resolver: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("cannot wait for producer-definition resolver: {error}"))?;
    match output.status.code() {
        Some(0) => {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout)
                .map_err(|error| format!("producer-definition resolver returned invalid JSON: {error}"))?;
            if report.get("accepted").and_then(|value| value.as_bool()) != Some(true)
                || report
                    .get("producer_definition_status")
                    .and_then(|value| value.as_str())
                    != Some("satisfied")
            {
                return Err("producer-definition resolver returned inconsistent success".into());
            }
            let evidence = serde_json::from_value::<ProducerDefinitionEvidence>(
                report
                    .get("producer")
                    .cloned()
                    .ok_or_else(|| "producer-definition resolver omitted evidence".to_string())?,
            )
            .map_err(|error| format!("producer-definition evidence is malformed: {error}"))?;
            let keys = evidence.definition.keys().cloned().collect::<Vec<_>>();
            if evidence.paths != keys
                || !matches!(
                    evidence.coverage_status.as_str(),
                    "legacy-selected-paths" | "complete"
                )
                || evidence.valid_commits.as_ref().is_some_and(|commits| {
                    commits.is_empty()
                        || !commits.iter().all(|commit| is_oid(commit))
                        || !commits.iter().any(|commit| commit == sha)
                })
            {
                return Err("producer-definition evidence carries inconsistent coverage".into());
            }
            Ok(Some(evidence))
        }
        Some(1) => Ok(None),
        _ => Err(format!(
            "producer-definition resolver failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

#[cfg(test)]
fn resolve_producer_definition_evidence(
    _root: &Path,
    _row: &HistoryRow,
    _sha: &str,
) -> Result<Option<ProducerDefinitionEvidence>, String> {
    // Pure receipt-shape unit tests use synthetic commits. Production call-path
    // brackets execute the non-test binary against real git fixture commits.
    Ok(Some(ProducerDefinitionEvidence {
        definition: BTreeMap::new(),
        coverage_status: "legacy-selected-paths".into(),
        paths: Vec::new(),
        valid_commits: None,
        resolved_from: "synthetic-unit-fixture".into(),
    }))
}

fn assess_canonical_receipts(
    root: &Path,
    rows: &[HistoryRow],
    sha: &str,
    repo: &str,
) -> Result<CanonicalReceiptAssessment, String> {
    if repo != CANONICAL_VALIDATE_REPO {
        return Err(format!(
            "canonical local receipt ledger is bound to {CANONICAL_VALIDATE_REPO}, not {repo}"
        ));
    }
    let mut qualifying = Vec::new();
    let mut disqualified = Vec::new();
    let mut saw_failed = false;
    let mut saw_needs_rerun = false;
    let mut saw_truncated = false;
    let mut saw_no_result = false;
    for row in rows {
        if row.commit.as_deref() != Some(sha) {
            continue;
        }
        if let Some(mut receipt) = qualify_canonical_receipt(row, sha) {
            match resolve_producer_definition_evidence(root, row, sha)? {
                Some(evidence) => {
                    receipt.producer_definition = Some(evidence);
                    qualifying.push(receipt);
                }
                None => disqualified.push(row.clone()),
            }
        } else {
            match validate_status::failure_disposition(row, sha) {
                validate_status::FailureDisposition::Failed => saw_failed = true,
                validate_status::FailureDisposition::NeedsRerun => saw_needs_rerun = true,
                validate_status::FailureDisposition::Truncated => saw_truncated = true,
                validate_status::FailureDisposition::NoResult => saw_no_result = true,
                validate_status::FailureDisposition::NotFailure => {}
            }
            disqualified.push(row.clone());
        }
    }
    let verdict = if !qualifying.is_empty() {
        validate_status::Verdict::Validated
    } else if saw_failed {
        validate_status::Verdict::FailedOnRecord
    } else if saw_needs_rerun {
        validate_status::Verdict::NeedsRerun
    } else if saw_truncated {
        validate_status::Verdict::Truncated
    } else if saw_no_result {
        validate_status::Verdict::NoResult
    } else {
        validate_status::Verdict::NotValidated
    };
    // Retain the current-main adverse-record accounting while the canonical
    // receipt predicate above remains the stricter positive authority.
    let status_assessment = validate_status::assess(rows, sha);
    Ok(CanonicalReceiptAssessment {
        sha: sha.to_string(),
        verdict,
        qualifying,
        disqualified,
        failed_records: status_assessment.failed_records,
        withheld_nonpass_records: status_assessment.withheld_nonpass_records,
    })
}

fn newest_canonical_receipt(rows: &[QualifyingReceipt]) -> Option<&QualifyingReceipt> {
    rows.iter().max_by(|a, b| {
        a.row
            .finished_at
            .as_deref()
            .unwrap_or("")
            .cmp(b.row.finished_at.as_deref().unwrap_or(""))
    })
}

/// One qualifying authority record rendered with every condition it verified.
fn describe_receipt(receipt: &QualifyingReceipt) -> serde_json::Value {
    let row = &receipt.row;
    let tree = extra_str(row, "tree").expect("qualified receipt has tree");
    serde_json::json!({
        "schema_version": row.schema_version,
        "repo": CANONICAL_VALIDATE_REPO,
        "sha": row.commit,
        "commit": row.commit,
        "tree": tree,
        "base_sha": extra_str(row, "base_sha"),
        "base_tree": extra_str(row, "base_tree"),
        "reverie_base_sha": extra_str(row, "reverie_base_sha"),
        "reverie_base_tree": extra_str(row, "reverie_base_tree"),
        "commit_anchored": row.commit_anchored,
        "tree_dirty": row.tree_dirty,
        "finished_at": row.finished_at,
        "host": row.host,
        "profile": row.profile,
        "selection_mode": row.selection_mode,
        "result": row.result,
        "raw_result": extra_str(row, "raw_result"),
        "exit_code": row.exit_code,
        "checks": row.checks,
        "failures": row.failures,
        "gates_run": row.gates_run,
        "gates_expected": row.gates_expected,
        "gates": row.gates,
        "executed_tests": row.executed_tests,
        "filtered_tests": row.filtered_tests,
        "selected_tests": receipt.selected_tests,
        "discovered_tests": receipt.discovered_tests,
        "count_derivation": receipt.count_derivation,
        "coverage": row.coverage,
        "coverage_satisfied": row.coverage.as_ref().map(|_| true),
        "coverage_status": if row.coverage.is_some() {
            "satisfied"
        } else {
            "grandfathered-unknown"
        },
        "coverage_basis": receipt.coverage_basis,
        "real_seconds": row.real_seconds,
        "user_seconds": row.user_seconds,
        "sys_seconds": row.sys_seconds,
        "slot": row.slot,
        "log_file": row.log_file,
        "receipt_identity": {
            "digest_algorithm": "sha256",
            "canonicalization": RECEIPT_CANONICALIZATION,
            "digest": receipt.canonical_sha256,
            "tuple": {
                "repo": CANONICAL_VALIDATE_REPO,
                "sha": row.commit,
                "tree": tree,
                "finished_at": row.finished_at,
                "host": row.host,
                "slot": row.slot,
                "log_file": row.log_file,
            },
        },
        "producer_definition": receipt.producer_definition,
    })
}

fn history_repo_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn fetch_history_branch(repo: &Path, branch: &str) -> Result<(), CiHubError> {
    if branch.is_empty() || branch.starts_with('-') {
        return Err(CiHubError::HistoryQuery(format!(
            "invalid branch name '{branch}'"
        )));
    }
    let remote = "origin";
    let refspec = format!("refs/heads/{branch}:refs/remotes/{remote}/{branch}");
    let mut command = if on_path("with-proxy") {
        let mut command = Command::new("with-proxy");
        command.arg("git");
        command
    } else {
        Command::new("git")
    };
    let output = command
        .arg("-C")
        .arg(repo)
        .args(["fetch", "--quiet", remote, &refspec])
        .output()
        .map_err(|source| CiHubError::Launch {
            tool: "git fetch main for history query".into(),
            source,
        })?;
    if !output.status.success() {
        return Err(CiHubError::HistoryQuery(format!(
            "git fetch {remote}/{branch} exited {}: {}",
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn branch_history(repo: &Path, branch_ref: &str) -> Result<Vec<String>, CiHubError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-list", "--first-parent", branch_ref])
        .output()
        .map_err(|source| CiHubError::Launch {
            tool: "git rev-list main for history query".into(),
            source,
        })?;
    if !output.status.success() {
        return Err(CiHubError::HistoryQuery(format!(
            "cannot walk {branch_ref}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let commits: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if commits.is_empty() {
        return Err(CiHubError::HistoryQuery(format!(
            "{branch_ref} has no commits"
        )));
    }
    Ok(commits)
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct EffectiveGateFloor {
    sha: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct GateFloorQueryOutput {
    ok: bool,
    effective_floor: Option<String>,
    effective_kind: Option<String>,
    verdict: Option<String>,
    history_depth: Option<usize>,
    required_history_depth: Option<String>,
    required_history_depth_min: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
enum GateFloorResolution {
    Effective(EffectiveGateFloor),
    UnverifiableShallow {
        visible_depth: usize,
        required_depth: String,
        required_depth_min: usize,
    },
}

fn parse_gate_floor_resolution(raw: &[u8]) -> Result<GateFloorResolution, CiHubError> {
    let output: GateFloorQueryOutput = serde_json::from_slice(raw).map_err(|error| {
        CiHubError::HistoryQuery(format!("gate_floors.py returned invalid JSON: {error}"))
    })?;
    if output.verdict.as_deref() == Some("UNVERIFIABLE-SHALLOW-HISTORY") {
        return Ok(GateFloorResolution::UnverifiableShallow {
            visible_depth: output.history_depth.ok_or_else(|| {
                CiHubError::HistoryQuery(
                    "shallow-history result omitted history_depth; refusing to guess".into(),
                )
            })?,
            required_depth: output.required_history_depth.ok_or_else(|| {
                CiHubError::HistoryQuery(
                    "shallow-history result omitted required_history_depth; refusing to guess"
                        .into(),
                )
            })?,
            required_depth_min: output.required_history_depth_min.ok_or_else(|| {
                CiHubError::HistoryQuery(
                    "shallow-history result omitted required_history_depth_min; refusing to guess"
                        .into(),
                )
            })?,
        });
    }
    if !output.ok {
        return Err(CiHubError::HistoryQuery(
            "gate_floors.py refused to derive an effective floor; refusing to guess a rebase base"
                .into(),
        ));
    }
    let sha = output.effective_floor.ok_or_else(|| {
        CiHubError::HistoryQuery(
            "gate_floors.py reported ok without effective_floor; refusing to guess".into(),
        )
    })?;
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CiHubError::HistoryQuery(format!(
            "gate_floors.py returned invalid effective_floor {sha:?}; refusing to guess"
        )));
    }
    Ok(GateFloorResolution::Effective(EffectiveGateFloor {
        sha: sha.to_ascii_lowercase(),
        kind: output.effective_kind.unwrap_or_else(|| "unknown".into()),
    }))
}

#[cfg(test)]
fn parse_effective_gate_floor(raw: &[u8]) -> Result<EffectiveGateFloor, CiHubError> {
    match parse_gate_floor_resolution(raw)? {
        GateFloorResolution::Effective(floor) => Ok(floor),
        GateFloorResolution::UnverifiableShallow { .. } => Err(CiHubError::HistoryQuery(
            "gate_floors.py reported UNVERIFIABLE-SHALLOW-HISTORY where a floor was required"
                .into(),
        )),
    }
}

fn query_effective_gate_floor(
    root: &Path,
    repo: &Path,
    branch: &str,
) -> Result<GateFloorResolution, CiHubError> {
    let script = root.join("ci-hub/validate/gate_floors.py");
    let registry = root.join("ci-hub/validate/rebase-base-floors.json");
    let output = Command::new("python3")
        .arg(&script)
        .args(["--branch", branch, "--repo-checkout"])
        .arg(repo)
        .arg("--registry")
        .arg(&registry)
        // run_newest_green already fetched the branch unless the caller chose
        // --no-fetch. Derive against that exact snapshot rather than fetching
        // a second, potentially different tip inside the registry helper.
        .args(["--no-fetch", "--json"])
        .output()
        .map_err(|source| CiHubError::Launch {
            tool: script.display().to_string(),
            source,
        })?;
    let parsed = parse_gate_floor_resolution(&output.stdout);
    if output.status.success() {
        return parsed;
    }
    if let Ok(resolution @ GateFloorResolution::UnverifiableShallow { .. }) = parsed {
        return Ok(resolution);
    }
    {
        let detail = if output.stdout.is_empty() {
            String::from_utf8_lossy(&output.stderr).trim().to_string()
        } else {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        return Err(CiHubError::HistoryQuery(format!(
            "gate_floors.py exited {}: {detail}",
            exit_status_code(output.status)
        )));
    }
}

fn history_at_or_after_gate_floor(
    mut commits: Vec<String>,
    branch_ref: &str,
    gate_floor: &str,
) -> Result<Vec<String>, CiHubError> {
    let Some(floor_index) = commits.iter().position(|commit| commit == gate_floor) else {
        return Err(CiHubError::HistoryQuery(format!(
            "{branch_ref} does not contain required registry-derived first-parent floor \
             {gate_floor}; refusing to guess a usable rebase base"
        )));
    };
    commits.truncate(floor_index + 1);
    Ok(commits)
}

fn ledger_stamp(path: &Path) -> Result<(u64, u128), CiHubError> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(source) => {
            return Err(CiHubError::LedgerRead {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok((metadata.len(), modified_ns))
}

fn producer_definition_authority_key(root: &Path) -> Result<String, CiHubError> {
    let verifier = root.join("ci-hub/validation/verify_receipt.sh");
    let output = Command::new(verifier)
        .arg("--producer-definition-allowed")
        .output()
        .map_err(|source| CiHubError::Launch {
            tool: "producer-definition authority".into(),
            source,
        })?;
    if !output.status.success() {
        return Err(CiHubError::HistoryQuery(format!(
            "producer-definition authority unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(format!("{:x}", Sha256::digest(&output.stdout)))
}

/// Remove only would-be green rows whose exact target commit does not resolve
/// to one currently registered producer map. Non-green rows remain available
/// for failure attribution. Accepted rows carry the exact map and coverage
/// status into newest-green's evidence record.
fn producer_authoritative_history_rows(
    root: &Path,
    rows: Vec<HistoryRow>,
) -> Result<Vec<HistoryRow>, CiHubError> {
    let mut retained = Vec::with_capacity(rows.len());
    for mut row in rows {
        let Some(sha) = row.commit.clone() else {
            retained.push(row);
            continue;
        };
        if !crate::qualifying_receipt::row_qualifies(
            &row,
            &sha,
            crate::qualifying_receipt::active(),
        ) {
            retained.push(row);
            continue;
        }
        match resolve_producer_definition_evidence(root, &row, &sha)
            .map_err(CiHubError::HistoryQuery)?
        {
            Some(evidence) => {
                row.extra.insert(
                    "producer_definition".into(),
                    serde_json::to_value(evidence).map_err(|error| {
                        CiHubError::HistoryQuery(format!(
                            "cannot serialize producer-definition evidence: {error}"
                        ))
                    })?,
                );
                retained.push(row);
            }
            None => {}
        }
    }
    Ok(retained)
}

fn read_newest_green_cache(path: &Path) -> Option<NewestGreenCache> {
    let raw = std::fs::read(path).ok()?;
    match serde_json::from_slice(&raw) {
        Ok(cache) => Some(cache),
        Err(error) => {
            eprintln!(
                "ci-hub: ignoring invalid newest-green cache {}: {error}",
                path.display()
            );
            None
        }
    }
}

fn write_newest_green_cache(path: &Path, cache: &NewestGreenCache) -> Result<(), CiHubError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| {
            CiHubError::HistoryQuery(format!(
                "cannot create cache directory {}: {source}",
                parent.display()
            ))
        })?;
    }
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut bytes = serde_json::to_vec_pretty(cache)
        .map_err(|error| CiHubError::HistoryQuery(format!("serialize cache: {error}")))?;
    bytes.push(b'\n');
    std::fs::write(&temporary, bytes).map_err(|source| {
        CiHubError::HistoryQuery(format!("write cache {}: {source}", temporary.display()))
    })?;
    std::fs::rename(&temporary, path).map_err(|source| {
        CiHubError::HistoryQuery(format!(
            "replace cache {} with {}: {source}",
            path.display(),
            temporary.display()
        ))
    })
}

fn retain_cell_evidence(root: &Path, rows: &mut [HistoryRow]) -> Result<(), CiHubError> {
    let path = history_queries::cell_evidence_cache_path(root);
    let cache: CellEvidenceCache = match std::fs::read(&path) {
        Ok(raw) => match serde_json::from_slice::<CellEvidenceCache>(&raw) {
            Ok(cache) if cache.schema_version == 2 => cache,
            Ok(cache) => {
                eprintln!(
                    "ci-hub: rebuilding cell-evidence cache {} (schema {} != 2)",
                    path.display(),
                    cache.schema_version
                );
                CellEvidenceCache::default()
            }
            Err(error) => {
                eprintln!(
                    "ci-hub: ignoring invalid cell-evidence cache {}: {error}",
                    path.display()
                );
                CellEvidenceCache::default()
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => CellEvidenceCache::default(),
        Err(source) => {
            return Err(CiHubError::HistoryQuery(format!(
                "read cell-evidence cache {}: {source}",
                path.display()
            )))
        }
    };
    let indexed: std::collections::BTreeSet<(&str, Option<&str>)> = cache
        .records
        .iter()
        .map(|record| (record.commit.as_str(), record.finished_at.as_deref()))
        .collect();
    history_queries::merge_retained_cell_evidence(rows, &cache);
    for row in rows.iter_mut() {
        let Some(commit) = row.commit.as_deref() else {
            continue;
        };
        if !indexed.contains(&(commit, row.finished_at.as_deref())) {
            history_queries::enrich_rows_from_logs(std::slice::from_mut(row));
        }
    }
    let updated = history_queries::retained_cell_evidence(rows);
    if updated.records.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| {
            CiHubError::HistoryQuery(format!(
                "create cell-evidence cache directory {}: {source}",
                parent.display()
            ))
        })?;
    }
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut bytes = serde_json::to_vec_pretty(&updated).map_err(|error| {
        CiHubError::HistoryQuery(format!("serialize cell-evidence cache: {error}"))
    })?;
    bytes.push(b'\n');
    std::fs::write(&temporary, bytes).map_err(|source| {
        CiHubError::HistoryQuery(format!(
            "write cell-evidence cache {}: {source}",
            temporary.display()
        ))
    })?;
    std::fs::rename(&temporary, &path).map_err(|source| {
        CiHubError::HistoryQuery(format!(
            "replace cell-evidence cache {}: {source}",
            path.display()
        ))
    })
}

fn print_newest_green(report: &history_queries::NewestGreenReport, cache_hit: bool, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "cache_hit": cache_hit,
                "report": report,
            }))
            .expect("serialize newest-green report")
        );
        return;
    }
    println!(
        "NEWEST-GREEN {} validated={} profile={} selection={} coverage={} coverage_satisfied={} coverage_status={}",
        report.green.sha,
        report.green.finished_at.as_deref().unwrap_or("unknown"),
        report.green.profile,
        report.green.selection_mode,
        report
            .green
            .coverage
            .as_ref()
            .map(history_queries::CoverageStrength::as_str)
            .unwrap_or("unknown"),
        report
            .green
            .coverage_satisfied
            .map(|satisfied| if satisfied { "true" } else { "false" })
            .unwrap_or("null"),
        report.green.coverage_status,
    );
    println!(
        "GATE-SCHEMA {} floor={} eligibility=at-or-after",
        report.gate_schema, report.gate_schema_floor,
    );
    if let Some(producer) = report.green.producer_definition.as_ref() {
        let status = producer
            .get("coverage_status")
            .and_then(|value| value.as_str())
            .unwrap_or("unavailable");
        let paths = producer
            .get("paths")
            .and_then(|value| value.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        println!("PRODUCER-DEFINITION status={status} paths={paths}");
    }
    println!(
        "BRANCH {} tip={} commits-after-green={} recorded={} no-record={} cache={}",
        report.branch,
        report.branch_tip,
        report.commits_after_green,
        report.commits_with_records,
        report.commits_without_any_record,
        if cache_hit { "hit" } else { "miss" },
    );
    println!(
        "EVIDENCE-WINDOW first-parent={}..{} commits={} trustworthy-recorded={} full-green={}",
        report.range_oldest_commit,
        report.branch_tip,
        report.branch_commits_in_range,
        report.trustworthy_recorded_commits_in_range,
        report.full_green_commits_in_range,
    );
}

fn run_newest_green(root: &Path, args: NewestGreenArgs) -> Result<i32, CiHubError> {
    let repo = history_repo_path(root, &args.query.repo_dir);
    let branch_ref = format!("origin/{}", args.query.branch);
    if !args.query.no_fetch {
        fetch_history_branch(&repo, &args.query.branch)?;
    }
    let effective_floor = match query_effective_gate_floor(root, &repo, &args.query.branch)? {
        GateFloorResolution::Effective(floor) => floor,
        GateFloorResolution::UnverifiableShallow {
            visible_depth,
            required_depth,
            required_depth_min,
        } => {
            if args.query.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 2,
                        "verdict": "UNVERIFIABLE-SHALLOW-HISTORY",
                        "exit_code": 2,
                        "branch": args.query.branch,
                        "visible_first_parent_depth": visible_depth,
                        "required_history_depth": required_depth,
                        "required_history_depth_min": required_depth_min,
                    })
                );
            } else {
                println!(
                    "NEWEST-GREEN UNVERIFIABLE-SHALLOW-HISTORY branch={} visible-first-parent-depth={} required-depth={} required-depth-min={} -- deepen or unshallow the repository before deciding whether a qualifying green exists",
                    args.query.branch, visible_depth, required_depth, required_depth_min
                );
            }
            return Ok(2);
        }
    };
    let floor_policy = format!("{GATE_FLOOR_POLICY}:{}", effective_floor.kind);
    let commits = history_at_or_after_gate_floor(
        branch_history(&repo, &branch_ref)?,
        &branch_ref,
        &effective_floor.sha,
    )?;
    let branch_tip = commits.first().expect("nonempty history");
    let ledger = ledger_path(root, &args.query.ledger);
    let (ledger_len, ledger_modified_ns) = ledger_stamp(&ledger)?;
    let producer_definition_authority = producer_definition_authority_key(root)?;
    let cache_path = history_queries::cache_path(root, &args.cache);
    if !args.no_cache {
        if let Some(cache) = read_newest_green_cache(&cache_path) {
            if history_queries::cache_matches(
                &cache,
                &args.query.branch,
                &branch_ref,
                branch_tip,
                &effective_floor.sha,
                &ledger,
                ledger_len,
                ledger_modified_ns,
                &producer_definition_authority,
            ) {
                print_newest_green(&cache.report, true, args.query.json);
                return Ok(0);
            }
        }
    }

    let mut rows = producer_authoritative_history_rows(root, load_ledger_rows(&ledger)?)?;
    retain_cell_evidence(root, &mut rows)?;
    match HistoryQueryEngine::new(commits, rows).newest_green(
        &args.query.branch,
        &branch_ref,
        &floor_policy,
        &effective_floor.sha,
    ) {
        NewestGreenOutcome::Found(report) => {
            let cache = NewestGreenCache {
                schema_version: 6,
                branch: report.branch.clone(),
                branch_ref: report.branch_ref.clone(),
                branch_tip: report.branch_tip.clone(),
                gate_schema_floor: report.gate_schema_floor.clone(),
                ledger_path: ledger.display().to_string(),
                ledger_len,
                ledger_modified_ns,
                producer_definition_authority,
                report: (*report).clone(),
            };
            write_newest_green_cache(&cache_path, &cache)?;
            print_newest_green(&report, false, args.query.json);
            Ok(0)
        }
        NewestGreenOutcome::FailedOnly {
            branch_tip,
            recorded,
            commits_in_range,
        } => {
            if args.query.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version": 2, "verdict": "FAILED", "exit_code": 3, "branch": args.query.branch, "branch_tip": branch_tip, "gate_schema": floor_policy, "gate_schema_floor": effective_floor.sha, "branch_commits_in_range": commits_in_range, "trustworthy_recorded_commits": recorded, "full_green_commits_in_range": 0})
                );
            } else {
                println!("NEWEST-GREEN FAILED branch={} tip={branch_tip} floor-policy={floor_policy} effective-floor={} window-commits={commits_in_range} full-green=0 -- {recorded} eligible branch commit(s) have clean anchored records, but none has a latest PASS", args.query.branch, effective_floor.sha);
            }
            Ok(3)
        }
        NewestGreenOutcome::NoEvidence {
            branch_tip,
            commits_in_range,
            recorded,
        } => {
            if args.query.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version": 2, "verdict": "NOT-VALIDATED", "exit_code": 4, "branch": args.query.branch, "branch_tip": branch_tip, "gate_schema": floor_policy, "gate_schema_floor": effective_floor.sha, "branch_commits_in_range": commits_in_range, "trustworthy_recorded_commits": recorded, "full_green_commits_in_range": 0})
                );
            } else {
                println!("NEWEST-GREEN NOT-VALIDATED branch={} tip={branch_tip} floor-policy={floor_policy} effective-floor={} window-commits={commits_in_range} trustworthy-recorded={recorded} full-green=0 -- no qualifying at-or-after-floor clean commit-anchored branch validation record exists", args.query.branch, effective_floor.sha);
            }
            Ok(4)
        }
    }
}

fn files_touched(repo: &Path, sha: &str) -> Result<Vec<String>, CiHubError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-only",
            "-r",
            sha,
        ])
        .output()
        .map_err(|source| CiHubError::Launch {
            tool: "git diff-tree first-bad".into(),
            source,
        })?;
    if !output.status.success() {
        return Err(CiHubError::HistoryQuery(format!(
            "cannot inspect files at {sha}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn assess_diff_plausibility(files: &[String], source_node: Option<&str>) -> String {
    if files.is_empty() {
        return "unknown: candidate commit has no retained file list".into();
    }
    if files.iter().all(|path| path.starts_with(".github/")) {
        return format!(
            "implausible: the commit changes only GitHub workflow files, which local validate{} does not execute",
            source_node.map(|node| format!(" cell {node}")) .unwrap_or_default()
        );
    }
    if files.iter().all(|path| {
        path.ends_with(".md")
            || path.starts_with("docs/")
            || path.starts_with("ai_docs/")
            || path.starts_with("experiments/")
    }) {
        return "implausible: the commit changes only documentation/evidence paths excluded by the test-footprint policy".into();
    }
    "not exonerated by the file-only check; inspect the listed paths against the cell's test footprint".into()
}

fn print_first_bad(report: &history_queries::FirstBadReport, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(report).expect("serialize first-bad report")
        );
        return;
    }
    println!(
        "FIRST-BAD {} cell={} observed={} profile={} selection={}",
        report.first_bad.sha,
        report.matched_name,
        report.first_bad.finished_at.as_deref().unwrap_or("unknown"),
        report.first_bad.profile,
        report.first_bad.selection_mode,
    );
    println!("BRANCH {} ref={}", report.branch, report.branch_ref);
    println!(
        "LAST-GOOD {} observed={} commits-between={} no-cell-record={} first-bad-mixed={}",
        report.last_good.sha,
        report.last_good.finished_at.as_deref().unwrap_or("unknown"),
        report.commits_between,
        report.commits_without_cell_record,
        report.first_bad_commit_has_mixed_outcomes,
    );
    if report.first_bad_commit_has_mixed_outcomes {
        println!("FLAKE-SIGNAL: the same first-bad SHA has both PASS and FAIL records for this cell; the failure is real evidence but not a deterministic commit regression");
    }
    println!(
        "FILES-TOUCHED {}",
        if report.files_touched.is_empty() {
            "(none)".into()
        } else {
            report.files_touched.join(" ")
        }
    );
    println!("PLAUSIBILITY {}", report.plausibility);
    println!(
        "LOAD-CONTEXT {}",
        report.load_context.as_deref().unwrap_or("not retained")
    );
    if report.error_excerpt.is_empty() {
        println!("ERROR-DETAIL not retained (ledger has the outcome but its referenced log is absent or has no canonical error line)");
    } else {
        println!("ERROR-DETAIL");
        for line in &report.error_excerpt {
            println!("  {line}");
        }
    }
}

fn run_first_bad(root: &Path, args: FirstBadArgs) -> Result<i32, CiHubError> {
    let repo = history_repo_path(root, &args.query.repo_dir);
    let branch_ref = format!("origin/{}", args.query.branch);
    if !args.query.no_fetch {
        fetch_history_branch(&repo, &args.query.branch)?;
    }
    let commits = branch_history(&repo, &branch_ref)?;
    let main_set: std::collections::BTreeSet<&str> = commits.iter().map(String::as_str).collect();
    let ledger = ledger_path(root, &args.query.ledger);
    let mut rows = load_ledger_rows(&ledger)?;
    rows.retain(|row| {
        row.commit
            .as_deref()
            .map(|commit| main_set.contains(commit))
            .unwrap_or(false)
    });
    retain_cell_evidence(root, &mut rows)?;
    match HistoryQueryEngine::new(commits, rows).first_bad(
        &args.cell_or_gate,
        &args.query.branch,
        &branch_ref,
    ) {
        FirstBadOutcome::Found(mut report) => {
            report.files_touched = files_touched(&repo, &report.first_bad.sha)?;
            report.plausibility =
                assess_diff_plausibility(&report.files_touched, report.source_node.as_deref());
            print_first_bad(&report, args.query.json);
            Ok(0)
        }
        FirstBadOutcome::FailureWithoutKnownGood {
            query,
            matched_name,
            failure,
        } => {
            if args.query.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version": 1, "verdict": "FAILED", "exit_code": 3, "branch": args.query.branch, "query": query, "matched_name": matched_name, "failure": failure, "reason": "failure exists but no earlier PASS is retained"})
                );
            } else {
                println!("FIRST-BAD FAILED cell={matched_name} sha={} -- failure exists but no earlier PASS is retained", failure.sha);
            }
            Ok(3)
        }
        FirstBadOutcome::NoEvidence {
            query,
            available_names,
        } => {
            if args.query.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version": 1, "verdict": "NOT-VALIDATED", "exit_code": 4, "branch": args.query.branch, "query": query, "suggestions": available_names, "reason": "no retained cell/gate record"})
                );
            } else {
                println!("FIRST-BAD NOT-VALIDATED cell={query} -- no retained cell/gate record; absence is not PASS");
                if !available_names.is_empty() {
                    println!("SUGGESTIONS {}", available_names.join(" | "));
                }
            }
            Ok(4)
        }
        FirstBadOutcome::NoTransition {
            query,
            matched_name,
            observations,
        } => {
            if args.query.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version": 1, "verdict": "NOT-VALIDATED", "exit_code": 4, "branch": args.query.branch, "query": query, "matched_name": matched_name, "observations": observations, "reason": "no retained PASS-to-FAIL transition"})
                );
            } else {
                println!("FIRST-BAD NOT-VALIDATED cell={matched_name} observations={observations} -- no retained PASS-to-FAIL transition");
            }
            Ok(4)
        }
    }
}

/// `ci-hub validate-status --sha <SHA> | --pr <N>` — the SHA-queryable local
/// receipt/cache authority (the landing combiner also queries hosted authority).
/// Exit 0 VALIDATED, 3 FAILED (known-bad), 4 for every
/// re-measurement state (TRUNCATED, NEEDS-RERUN, NO-RESULT, or NOT-VALIDATED).
fn run_validate_status(root: &Path, args: ValidateStatusArgs) -> Result<i32, CiHubError> {
    let path = ledger_path(root, &args.ledger);
    let rows = load_ledger_rows(&path)?;
    // A SHA may arrive as `--sha` or positionally; clap already forbids giving
    // both, or either together with `--pr`.
    let sha_input = args.sha.as_ref().or(args.sha_positional.as_ref());
    let sha = match (sha_input, args.pr) {
        (Some(input), None) => {
            validate_status::resolve_sha(&rows, input).map_err(CiHubError::ValidateStatus)?
        }
        (None, Some(pr)) => gh_pr_head(root, &args.repo, pr)?,
        _ => {
            return Err(CiHubError::ValidateStatus(
                "exactly one of a SHA (positional or --sha) or --pr is required".into(),
            ))
        }
    };
    let assessment = assess_canonical_receipts(root, &rows, &sha, &args.repo)
        .map_err(CiHubError::ValidateStatus)?;
    let newest = newest_canonical_receipt(&assessment.qualifying);

    if args.json {
        let qualifying_receipts: Vec<_> =
            assessment.qualifying.iter().map(describe_receipt).collect();
        let report = serde_json::json!({
            "schema_version": 1,
            "repo": args.repo,
            "sha": assessment.sha,
            "verdict": assessment.verdict.as_str(),
            "exit_code": assessment.verdict.exit_code(),
            "qualifying_count": assessment.qualifying.len(),
            "disqualified_count": assessment.disqualified.len(),
            // Genuine clean full-coverage FAILs on this same commit. When the
            // verdict is VALIDATED these are SUPERSEDED (the ledger does not
            // latch) -- a machine consumer must be able to see them rather than
            // read a bare green.
            "failed_record_count": assessment.failed_records,
            "withheld_nonpass_record_count": assessment.withheld_nonpass_records,
            "newest_qualifying": newest.map(describe_receipt),
            "qualifying_receipts": qualifying_receipts,
            "ledger": path.display().to_string(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serialize report")
        );
    } else {
        match assessment.verdict {
            validate_status::Verdict::Validated => {
                let row = newest.expect("validated implies a qualifying record");
                println!(
                    "# validate VALIDATED {} (passed {}, wall {}s, host {}, profile full/full) -- clean-tree commit-anchored full run",
                    assessment.sha,
                    row.row.finished_at.as_deref().unwrap_or("?"),
                    row.row.real_seconds.map(|s| s.round() as i64).unwrap_or(-1),
                    row.row.host.as_deref().unwrap_or("?"),
                );
                if let Some(producer) = row.producer_definition.as_ref() {
                    println!(
                        "# producer-definition status={} paths={}",
                        producer.coverage_status,
                        producer.paths.join(",")
                    );
                }
                // The ledger does not latch: this PASS supersedes any earlier
                // clean full FAIL on the same commit. Surface those loudly --
                // a green that silently buries a same-commit failure claims
                // more than it verified, and the pass/fail pair on identical
                // clean full conditions is a FLAKE SIGNAL, not noise.
                if assessment.failed_records > 0 {
                    println!(
                        "# validate WARNING {} -- {} SUPERSEDED clean full-coverage FAIL record(s) on this same commit; the green above did not disprove them. Same commit passing AND failing a clean full run is a FLAKE SIGNAL: investigate before trusting this green for landing.",
                        assessment.sha, assessment.failed_records,
                    );
                }
                // Withheld reds are not FAILURES, but neither are they nothing.
                // Measured on the live ledger: 20 of 105 VALIDATED commits carry
                // a same-commit `fail` record and EVERY one is withheld, so a
                // banner reporting only the failure count is silent on all of
                // them. Report the count and name it as withheld, so the reader
                // can tell "no adverse record" from "adverse record, not
                // attributable".
                if assessment.withheld_nonpass_records > 0 {
                    println!(
                        "# validate NOTE {} -- {} same-commit clean full-coverage NON-PASS record(s) exist but are WITHHELD from the failure count (contended, incomplete solo conditions, truncated, or environment fault); they carry no product verdict, but this green is not the only record of this commit.",
                        assessment.sha, assessment.withheld_nonpass_records,
                    );
                }
            }
            validate_status::Verdict::FailedOnRecord => {
                println!(
                    "# validate FAILED {} -- a clean full-coverage run exists but did NOT pass ({} record(s)); this commit is known-failing",
                    assessment.sha,
                    // Was `disqualified.len()`, which counts EVERY non-qualifying
                    // row -- subset, dirty, truncated, env-fault -- so the number
                    // overstated how many genuine clean full failures existed.
                    assessment.failed_records,
                );
            }
            validate_status::Verdict::Truncated => {
                println!(
                    "# validate TRUNCATED {} -- a run ended before producing a genuine failing gate; re-dispatch required",
                    assessment.sha,
                );
            }
            validate_status::Verdict::NeedsRerun => {
                println!(
                    "# validate NEEDS-RERUN {} -- red evidence lacks complete solo execution conditions or is known-flaky/contended; rerun solo at -j 4",
                    assessment.sha,
                );
            }
            validate_status::Verdict::NoResult => {
                println!(
                    "# validate NO-RESULT {} -- red gates could not run (command-not-found storm or sub-second collapse); an environment fault, not a product defect; re-dispatch required",
                    assessment.sha,
                );
            }
            validate_status::Verdict::NotValidated => {
                println!(
                    "# validate NOT-VALIDATED {} -- no clean full-coverage PASS record ({} non-qualifying record(s) for this commit)",
                    assessment.sha,
                    assessment.disqualified.len(),
                );
            }
        }
    }
    Ok(assessment.verdict.exit_code())
}

#[derive(Debug)]
struct VerifiedPublishedReceipt {
    receipt_commit: Option<String>,
    path: String,
    artifact_sha256: String,
    selected_digest: String,
    run_id: String,
    executed_tests: i64,
    log_sha256: String,
    producer_coverage_status: String,
    producer_paths: Vec<String>,
    producer_valid_commits: Option<Vec<String>>,
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Verify the mechanical publisher returned the exact artifact bytes for the
/// one row Rust selected. No publisher field can substitute for row equality.
fn verify_publisher_report(
    output: &[u8],
    repo: &str,
    sha: &str,
    selected: &QualifyingReceipt,
    dry_run: bool,
) -> Result<VerifiedPublishedReceipt, String> {
    let report: serde_json::Value = serde_json::from_slice(output)
        .map_err(|error| format!("publisher output is not JSON: {error}"))?;
    if report
        .get("schema_version")
        .and_then(|value| value.as_u64())
        != Some(1)
    {
        return Err("publisher output has unsupported schema".into());
    }
    let expected_action = if dry_run {
        "would-publish"
    } else {
        "published"
    };
    if report.get("action").and_then(|value| value.as_str()) != Some(expected_action) {
        return Err(format!("publisher action is not {expected_action}"));
    }
    if report
        .get("receipt_repository")
        .and_then(|value| value.as_str())
        != Some(VALIDATION_RECEIPT_REPO)
        || report
            .get("receipt_branch")
            .and_then(|value| value.as_str())
            != Some(VALIDATION_RECEIPT_BRANCH)
    {
        return Err("publisher output is not bound to the canonical receipt repository".into());
    }
    let selected_digest = report
        .get("receipt_identity_sha256")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "publisher omitted selected receipt digest".to_string())?;
    if selected_digest != selected.canonical_sha256 {
        return Err("publisher selected receipt digest does not match Rust selection".into());
    }
    let path = report
        .get("path")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "publisher omitted artifact path".to_string())?;
    let artifact_sha256 = report
        .get("artifact_sha256")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "publisher omitted artifact digest".to_string())?;
    let artifact_body = report
        .get("artifact_body")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "publisher omitted exact artifact body".to_string())?;
    let actual_artifact_sha256 = format!("{:x}", Sha256::digest(artifact_body.as_bytes()));
    if !is_sha256(artifact_sha256) || artifact_sha256 != actual_artifact_sha256 {
        return Err("publisher artifact bytes do not match artifact digest".into());
    }
    let expected_path = format!("validation-receipts/{repo}/{sha}/{artifact_sha256}.json");
    if path != expected_path {
        return Err("publisher artifact path is not artifact-digest-addressed".into());
    }
    let receipt: serde_json::Value = serde_json::from_str(artifact_body)
        .map_err(|error| format!("publisher artifact body is not JSON: {error}"))?;
    if receipt.get("repository").and_then(|value| value.as_str()) != Some(repo)
        || receipt.get("commit").and_then(|value| value.as_str()) != Some(sha)
    {
        return Err("publisher artifact is not bound to repository and SHA".into());
    }
    let identity = receipt
        .get("selected_receipt_identity")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "publisher artifact omitted selected receipt identity".to_string())?;
    if identity
        .get("digest_algorithm")
        .and_then(|value| value.as_str())
        != Some("sha256")
        || identity
            .get("canonicalization")
            .and_then(|value| value.as_str())
            != Some(RECEIPT_CANONICALIZATION)
        || identity.get("digest").and_then(|value| value.as_str())
            != Some(selected.canonical_sha256.as_str())
    {
        return Err("publisher artifact receipt identity does not match Rust selection".into());
    }
    let expected_row = serde_json::to_value(&selected.row)
        .map_err(|error| format!("cannot serialize selected row: {error}"))?;
    if receipt.get("ledger_record") != Some(&expected_row) {
        return Err("publisher artifact ledger row differs from Rust-selected row".into());
    }
    // Derive the identity only from the exact Rust-selected ledger row. The
    // Python publisher carries the same host field into the artifact so two
    // otherwise-identical runs from different machines cannot collide.
    let expected_run_id = format!(
        "{sha}@{}@{}",
        selected
            .row
            .started_at
            .as_deref()
            .ok_or_else(|| "selected row omitted started_at".to_string())?,
        selected
            .row
            .host
            .as_deref()
            .filter(|host| !host.trim().is_empty())
            .ok_or_else(|| "selected row omitted host".to_string())?
    );
    if receipt.get("run_id").and_then(|value| value.as_str()) != Some(&expected_run_id) {
        return Err("publisher artifact run identity differs from selected row".into());
    }
    let executed_tests = receipt
        .get("ledger_record")
        .and_then(|value| value.get("executed_tests"))
        .and_then(|value| value.as_i64())
        .ok_or_else(|| "publisher artifact omitted selected execution count".to_string())?;
    let log_sha256 = receipt
        .get("log_sha256")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "publisher artifact omitted log digest".to_string())?;
    if !is_sha256(log_sha256) {
        return Err("publisher artifact log digest is malformed".into());
    }
    let producer = receipt
        .get("producer")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "publisher artifact omitted producer evidence".to_string())?;
    let producer_coverage_status = producer
        .get("coverage_status")
        .and_then(|value| value.as_str())
        .or_else(|| {
            report
                .get("verified_producer_coverage_status")
                .and_then(|value| value.as_str())
                .filter(|status| *status == "legacy-selected-paths")
        })
        .ok_or_else(|| "publisher artifact omitted producer coverage status".to_string())?;
    if !matches!(
        producer_coverage_status,
        "legacy-selected-paths" | "complete"
    ) {
        return Err("publisher artifact producer coverage status is invalid".into());
    }
    let definition = producer
        .get("definition")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "publisher artifact omitted producer definition".to_string())?;
    let producer_paths_value = producer
        .get("paths")
        .and_then(|value| value.as_array())
        .or_else(|| {
            (producer.get("coverage_status").is_none()
                && producer_coverage_status == "legacy-selected-paths")
                .then(|| {
                    report
                        .get("verified_producer_paths")
                        .and_then(|value| value.as_array())
                })
                .flatten()
        })
        .ok_or_else(|| "publisher artifact omitted producer paths".to_string())?;
    let producer_paths = producer_paths_value
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "publisher artifact producer path is not a string".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let definition_paths = definition.keys().cloned().collect::<Vec<_>>();
    if producer_paths != definition_paths {
        return Err("publisher artifact producer paths differ from its definition".into());
    }
    let producer_valid_commits_value = match producer.get("valid_commits") {
        Some(serde_json::Value::Array(values)) => Some(values),
        Some(_) => return Err("publisher artifact producer valid_commits is not an array".into()),
        None => report
            .get("verified_producer_valid_commits")
            .and_then(|value| value.as_array()),
    };
    let producer_valid_commits = producer_valid_commits_value
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    value.as_str().filter(|commit| is_oid(commit)).map(str::to_string)
                        .ok_or_else(|| {
                            "publisher artifact producer commit bound is malformed".to_string()
                        })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    if producer_valid_commits.as_ref().is_some_and(|commits| {
        commits.is_empty()
            || !commits.iter().any(|commit| commit == sha)
            || commits.windows(2).any(|pair| pair[0] >= pair[1])
    }) {
        return Err("publisher artifact producer commit bound is invalid for target".into());
    }
    if let Some(expected) = selected.producer_definition.as_ref() {
        let artifact_definition = definition
            .iter()
            .map(|(path, value)| {
                value
                    .as_str()
                    .map(|blob| (path.clone(), blob.to_string()))
                    .ok_or_else(|| "publisher artifact producer blob is not a string".to_string())
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        if artifact_definition != expected.definition
            || producer_coverage_status != expected.coverage_status
            || producer_paths != expected.paths
            || producer_valid_commits != expected.valid_commits
        {
            return Err(
                "publisher artifact producer condition differs from exact target commit".into(),
            );
        }
    }
    let receipt_commit = match report.get("receipt_commit") {
        Some(serde_json::Value::String(value))
            if !dry_run
                && is_oid(value)
                && value.bytes().all(|byte| !byte.is_ascii_uppercase()) =>
        {
            Some(value.clone())
        }
        Some(serde_json::Value::Null) if dry_run => None,
        _ => return Err("publisher receipt commit does not match execution mode".into()),
    };
    Ok(VerifiedPublishedReceipt {
        receipt_commit,
        path: path.to_string(),
        artifact_sha256: artifact_sha256.to_string(),
        selected_digest: selected_digest.to_string(),
        run_id: expected_run_id,
        executed_tests,
        log_sha256: log_sha256.to_string(),
        producer_coverage_status: producer_coverage_status.to_string(),
        producer_paths,
        producer_valid_commits,
    })
}

fn publish_selected_receipt(
    root: &Path,
    publisher: &Path,
    ledger: &Path,
    repo: &str,
    sha: &str,
    selected: &QualifyingReceipt,
    dry_run: bool,
) -> Result<VerifiedPublishedReceipt, CiHubError> {
    let ledger_arg = ledger.display().to_string();
    let mut command = Command::new("python3");
    command
        .arg(publisher)
        .args([
            "--repo",
            repo,
            "--sha",
            sha,
            "--ledger",
            &ledger_arg,
            "--selected-receipt-sha256",
            &selected.canonical_sha256,
            "--canonicalization",
            RECEIPT_CANONICALIZATION,
            "--receipt-repo",
            VALIDATION_RECEIPT_REPO,
            "--receipt-branch",
            VALIDATION_RECEIPT_BRANCH,
        ])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if dry_run {
        command.arg("--dry-run");
    }
    if let Some(config_dir) = gh_config_dir() {
        command.env("GH_CONFIG_DIR", config_dir);
    }
    let mut child = command.spawn().map_err(|source| CiHubError::Launch {
        tool: publisher.display().to_string(),
        source,
    })?;
    child
        .stdin
        .take()
        .ok_or_else(|| CiHubError::ValidateStatus("publisher stdin is unavailable".into()))?
        .write_all(selected.canonical_row_json.as_bytes())
        .map_err(|source| CiHubError::Launch {
            tool: "write publisher selected row".into(),
            source,
        })?;
    let output = child
        .wait_with_output()
        .map_err(|source| CiHubError::Launch {
            tool: publisher.display().to_string(),
            source,
        })?;
    if !output.status.success() {
        return Err(CiHubError::ValidateStatus(format!(
            "mechanical receipt publisher exited {}: {}",
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    verify_publisher_report(&output.stdout, repo, sha, selected, dry_run)
        .map_err(CiHubError::ValidateStatus)
}

#[derive(Debug)]
struct ImmutableReceiptReference {
    receipt_commit: String,
    path: String,
    artifact_sha256: String,
    producer_coverage_status: String,
    producer_paths: Vec<String>,
    producer_valid_commits: Option<Vec<String>>,
}

fn parse_immutable_receipt_reference(
    output: &[u8],
    repo: &str,
    sha: &str,
) -> Result<ImmutableReceiptReference, String> {
    let output = std::str::from_utf8(output)
        .map_err(|error| format!("immutable receipt verifier output is not UTF-8: {error}"))?;
    let tokens = output.split_whitespace().collect::<Vec<_>>();
    if tokens.len() != 6 {
        return Err("immutable receipt verifier did not return exactly six fields".into());
    }
    let fields = tokens
        .into_iter()
        .map(|field| {
            field
                .split_once('=')
                .ok_or_else(|| format!("malformed immutable receipt verifier field: {field}"))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    if fields.len() != 6 {
        return Err("immutable receipt verifier returned duplicate fields".into());
    }
    let receipt_commit = fields
        .get("receipt_commit")
        .ok_or_else(|| "immutable receipt verifier omitted receipt_commit".to_string())?;
    if !is_oid(receipt_commit) || receipt_commit.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err("immutable receipt verifier returned a malformed receipt commit".into());
    }
    let artifact_sha256 = fields
        .get("receipt_sha256")
        .ok_or_else(|| "immutable receipt verifier omitted receipt_sha256".to_string())?;
    if !is_sha256(artifact_sha256) {
        return Err("immutable receipt verifier returned a malformed artifact digest".into());
    }
    let path = fields
        .get("receipt_path")
        .ok_or_else(|| "immutable receipt verifier omitted receipt_path".to_string())?;
    let expected_path = format!("validation-receipts/{repo}/{sha}/{artifact_sha256}.json");
    if *path != expected_path {
        return Err("immutable receipt verifier returned a noncanonical artifact path".into());
    }
    let producer_coverage_status = fields
        .get("producer_coverage_status")
        .filter(|value| matches!(**value, "legacy-selected-paths" | "complete"))
        .ok_or_else(|| "immutable receipt verifier omitted producer coverage status".to_string())?;
    let producer_paths = fields
        .get("producer_paths")
        .ok_or_else(|| "immutable receipt verifier omitted producer paths".to_string())?
        .split(',')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if producer_paths.is_empty() {
        return Err("immutable receipt verifier returned no producer paths".into());
    }
    let producer_valid_commits = match fields
        .get("producer_valid_commits")
        .ok_or_else(|| "immutable receipt verifier omitted producer commit bound".to_string())?
    {
        &"unbounded" => None,
        commits => {
            let values = commits
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>();
            if values.is_empty()
                || values.iter().any(|commit| !is_oid(commit))
                || !values.iter().any(|commit| commit == sha)
                || values.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err("immutable receipt verifier returned invalid producer commit bound".into());
            }
            Some(values)
        }
    };
    Ok(ImmutableReceiptReference {
        receipt_commit: (*receipt_commit).to_string(),
        path: (*path).to_string(),
        artifact_sha256: (*artifact_sha256).to_string(),
        producer_coverage_status: (*producer_coverage_status).to_string(),
        producer_paths,
        producer_valid_commits,
    })
}

fn fetch_issue_comments(root: &Path, repo: &str, pr: u64) -> Result<Vec<u8>, CiHubError> {
    let endpoint = format!("repos/{repo}/issues/{pr}/comments?per_page=100");
    let comments = gh_command(root, &["api", "--paginate", "--slurp", &endpoint])
        .output()
        .map_err(|source| CiHubError::Launch {
            tool: "gh issue comments".into(),
            source,
        })?;
    if !comments.status.success() {
        return Err(CiHubError::Gh {
            context: format!("issue comments #{pr}"),
            message: String::from_utf8_lossy(&comments.stderr).trim().to_string(),
        });
    }
    Ok(comments.stdout)
}

fn run_immutable_receipt_verifier(
    root: &Path,
    repo: &str,
    sha: &str,
    comments: &[u8],
) -> Result<Option<ImmutableReceiptReference>, String> {
    let verifier = root.join("ci-hub/validation/verify_receipt.sh");
    let producer_checkout = root.join("hermit");
    let mut command = Command::new(&verifier);
    command
        .args(["--repo", repo, "--sha", sha, "--comments", "/dev/stdin"])
        .arg("--producer-repo-checkout")
        .arg(producer_checkout)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(config_dir) = gh_config_dir() {
        command.env("GH_CONFIG_DIR", config_dir);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot launch immutable receipt verifier: {error}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "immutable receipt verifier stdin is unavailable".to_string())?
        .write_all(&comments)
        .map_err(|error| format!("cannot write immutable receipt comments: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("cannot wait for immutable receipt verifier: {error}"))?;
    if !output.status.success() {
        if exit_status_code(output.status) == 1 {
            return Ok(None);
        }
        return Err(format!(
            "immutable receipt verifier exited {}: {}",
            exit_status_code(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_immutable_receipt_reference(&output.stdout, repo, sha).map(Some)
}

fn remove_exact_comment_marker(value: &mut serde_json::Value, marker: &str) -> usize {
    match value {
        serde_json::Value::Array(values) => values
            .iter_mut()
            .map(|value| remove_exact_comment_marker(value, marker))
            .sum(),
        serde_json::Value::Object(fields) => {
            let mut removed = 0;
            if let Some(serde_json::Value::String(body)) = fields.get_mut("body") {
                let lines = body.split('\n').collect::<Vec<_>>();
                removed = lines.iter().filter(|line| **line == marker).count();
                if removed > 0 {
                    *body = lines
                        .into_iter()
                        .filter(|line| *line != marker)
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            }
            removed
                + fields
                    .values_mut()
                    .map(|value| remove_exact_comment_marker(value, marker))
                    .sum::<usize>()
        }
        _ => 0,
    }
}

fn fetch_and_verify_immutable_receipt(
    root: &Path,
    repo: &str,
    sha: &str,
    selected: &QualifyingReceipt,
    reference: ImmutableReceiptReference,
) -> Result<VerifiedPublishedReceipt, String> {
    let expected_producer_status = reference.producer_coverage_status.clone();
    let expected_producer_paths = reference.producer_paths.clone();
    let expected_producer_valid_commits = reference.producer_valid_commits.clone();
    let endpoint = format!(
        "repos/{VALIDATION_RECEIPT_REPO}/contents/{}?ref={}",
        reference.path, reference.receipt_commit
    );
    let artifact = gh_command(
        root,
        &[
            "api",
            "-H",
            "Accept: application/vnd.github.raw+json",
            &endpoint,
        ],
    )
    .output()
    .map_err(|error| format!("cannot fetch immutable receipt artifact: {error}"))?;
    if !artifact.status.success() {
        return Err(format!(
            "cannot fetch immutable receipt artifact: {}",
            String::from_utf8_lossy(&artifact.stderr).trim()
        ));
    }
    let artifact_body = std::str::from_utf8(&artifact.stdout)
        .map_err(|error| format!("immutable receipt artifact is not UTF-8 JSON: {error}"))?;
    // Reuse the same byte/row/run/log checks as a freshly published artifact.
    // The shell verifier above independently establishes marker authorship,
    // receipt-commit reachability, producer-definition equality, and the shared
    // qualifying predicate for these exact immutable bytes.
    let report = serde_json::json!({
        "schema_version": 1,
        "action": "published",
        "receipt_commit": reference.receipt_commit,
        "receipt_repository": VALIDATION_RECEIPT_REPO,
        "receipt_branch": VALIDATION_RECEIPT_BRANCH,
        "path": reference.path,
        "receipt_identity_sha256": selected.canonical_sha256,
        "artifact_sha256": reference.artifact_sha256,
        "artifact_body": artifact_body,
        "verified_producer_coverage_status": expected_producer_status.clone(),
        "verified_producer_paths": expected_producer_paths.clone(),
        "verified_producer_valid_commits": expected_producer_valid_commits.clone(),
    });
    let verified = verify_publisher_report(
        &serde_json::to_vec(&report)
            .map_err(|error| format!("cannot encode immutable receipt report: {error}"))?,
        repo,
        sha,
        selected,
        false,
    )?;
    if verified.producer_coverage_status != expected_producer_status
        || verified.producer_paths != expected_producer_paths
        || verified.producer_valid_commits != expected_producer_valid_commits
    {
        return Err(
            "immutable verifier producer condition differs from artifact evidence".into(),
        );
    }
    Ok(verified)
}

/// Reuse a receipt only after the immutable landing verifier accepts its
/// owner-authored marker and exact receipt commit. Then fetch those exact bytes
/// and pass them through Rust's publisher-report verifier, which additionally
/// requires equality with the one canonical ledger row selected above.
///
/// A head may have multiple genuine receipts. If the newest accepted marker is
/// for another qualifying run, remove only that exact marker from the in-memory
/// verifier input and continue until the Rust-selected canonical row is found.
/// `Ok(None)` means no marker passed the immutable verifier at all. Every other
/// failure is returned for diagnostics, but callers still take the existing
/// fail-closed publisher path rather than treating reuse as required.
fn reuse_existing_immutable_receipt(
    root: &Path,
    repo: &str,
    pr: u64,
    sha: &str,
    selected: &QualifyingReceipt,
) -> Result<Option<VerifiedPublishedReceipt>, String> {
    let comments = fetch_issue_comments(root, repo, pr).map_err(|error| error.to_string())?;
    let mut comments: serde_json::Value = serde_json::from_slice(&comments)
        .map_err(|error| format!("invalid issue comments JSON: {error}"))?;
    let mut last_rejection = None;
    loop {
        let verifier_input = serde_json::to_vec(&comments)
            .map_err(|error| format!("cannot encode issue comments: {error}"))?;
        let Some(reference) = run_immutable_receipt_verifier(root, repo, sha, &verifier_input)?
        else {
            return match last_rejection {
                Some(error) => Err(error),
                None => Ok(None),
            };
        };
        let marker = format!(
            "<!-- locally-validated-receipt commit={} path={} sha256={} -->",
            reference.receipt_commit, reference.path, reference.artifact_sha256
        );
        match fetch_and_verify_immutable_receipt(root, repo, sha, selected, reference) {
            Ok(artifact) => return Ok(Some(artifact)),
            Err(error) => last_rejection = Some(error),
        }
        if remove_exact_comment_marker(&mut comments, &marker) == 0 {
            return Err(
                "immutable receipt verifier returned a marker absent from its input".into(),
            );
        }
    }
}

fn comment_pages_contain_marker(value: &serde_json::Value, marker: &str) -> bool {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| comment_pages_contain_marker(value, marker)),
        serde_json::Value::Object(fields) => fields
            .get("body")
            .and_then(|value| value.as_str())
            .is_some_and(|body| body.contains(marker)),
        _ => false,
    }
}

fn bind_verified_receipt_to_pr(
    root: &Path,
    repo: &str,
    pr: u64,
    sha: &str,
    artifact: &VerifiedPublishedReceipt,
) -> Result<(), CiHubError> {
    let receipt_commit = artifact
        .receipt_commit
        .as_deref()
        .ok_or_else(|| CiHubError::ValidateStatus("published receipt has no commit".into()))?;
    let marker = format!(
        "<!-- locally-validated-receipt commit={receipt_commit} path={} sha256={} -->",
        artifact.path, artifact.artifact_sha256
    );
    let comments = fetch_issue_comments(root, repo, pr)?;
    let comments_json: serde_json::Value = serde_json::from_slice(&comments).map_err(|error| {
        CiHubError::ValidateStatus(format!("invalid issue comments JSON: {error}"))
    })?;
    if !comment_pages_contain_marker(&comments_json, &marker) {
        let pr_arg = pr.to_string();
        let body = format!(
            "[coordinator, gpt-5.6-sol]\n\nLocal validation receipt published before applying `{LOCALLY_VALIDATED_LABEL}`.\n\n- SHA: `{sha}`\n- Run ID: `{}`\n- Executed tests: `{}`\n- Producer coverage: `{}`\n- Producer paths: `{}`\n- Producer valid commits: `{}`\n- Receipt identity SHA-256: `{}`\n- Immutable artifact: `{VALIDATION_RECEIPT_REPO}@{receipt_commit}:{}`\n- Artifact SHA-256: `{}`\n- Log SHA-256: `{}`\n\n{marker}",
            artifact.run_id,
            artifact.executed_tests,
            artifact.producer_coverage_status,
            artifact.producer_paths.join(","),
            artifact
                .producer_valid_commits
                .as_ref()
                .map(|commits| commits.join(","))
                .unwrap_or_else(|| "unbounded".into()),
            artifact.selected_digest,
            artifact.path,
            artifact.artifact_sha256,
            artifact.log_sha256,
        );
        let comment = gh_command(
            root,
            &["pr", "comment", &pr_arg, "--repo", repo, "--body", &body],
        )
        .output()
        .map_err(|source| CiHubError::Launch {
            tool: "gh pr comment".into(),
            source,
        })?;
        if !comment.status.success() {
            return Err(CiHubError::Gh {
                context: format!("pr comment #{pr}"),
                message: String::from_utf8_lossy(&comment.stderr).trim().to_string(),
            });
        }
    }
    let pr_arg = pr.to_string();
    let label = gh_command(
        root,
        &[
            "pr",
            "edit",
            &pr_arg,
            "--repo",
            repo,
            "--add-label",
            LOCALLY_VALIDATED_LABEL,
        ],
    )
    .output()
    .map_err(|source| CiHubError::Launch {
        tool: "gh pr edit locally-validated".into(),
        source,
    })?;
    if !label.status.success() {
        return Err(CiHubError::Gh {
            context: format!("pr edit #{pr}"),
            message: String::from_utf8_lossy(&label.stderr).trim().to_string(),
        });
    }
    Ok(())
}

/// `ci-hub apply-local-label --pr <N> | --all-open` — close the retroactive gap:
/// when a commit was validated BEFORE its PR existed, validate.sh could not stamp
/// `locally-validated`. This applies it to any PR whose head has a clean
/// full-validate record and lacks the label. Never fabricates the label: a PR
/// whose head is not VALIDATED is left untouched.
fn run_apply_local_label(root: &Path, args: ApplyLocalLabelArgs) -> Result<i32, CiHubError> {
    let path = ledger_path(root, &args.ledger);
    let rows = load_ledger_rows(&path)?;
    let prs = match (args.pr, args.all_open) {
        (Some(pr), false) => vec![pr],
        (None, true) => gh_open_prs(root, &args.repo)?,
        _ => {
            return Err(CiHubError::ValidateStatus(
                "exactly one of --pr or --all-open is required".into(),
            ))
        }
    };

    let mut actions: Vec<serde_json::Value> = Vec::new();
    let mut applied = 0i32;
    let mut failed = 0i32;
    for pr in prs {
        // In sweep mode a single unreadable PR must not abort the whole run.
        let head = match gh_pr_head(root, &args.repo, pr) {
            Ok(head) => head,
            Err(error) => {
                eprintln!("ci-hub: apply-local-label: PR #{pr}: {error}");
                actions.push(
                    serde_json::json!({"pr": pr, "action": "error", "detail": error.to_string()}),
                );
                continue;
            }
        };
        let assessment = assess_canonical_receipts(root, &rows, &head, &args.repo)
            .map_err(CiHubError::ValidateStatus)?;
        if assessment.verdict != validate_status::Verdict::Validated {
            println!(
                "PR #{pr}: skip -- head {} is {}",
                &head[..12.min(head.len())],
                assessment.verdict.as_str()
            );
            actions.push(serde_json::json!({"pr": pr, "head": head, "action": "skip", "verdict": assessment.verdict.as_str()}));
            continue;
        }
        let selected = newest_canonical_receipt(&assessment.qualifying)
            .expect("validated implies one selected canonical receipt");
        // Prefer an already-published immutable artifact only when the existing
        // landing verifier accepts its exact marker/commit/content and Rust
        // confirms it embeds this exact selected row. Otherwise preserve the
        // existing fail-closed publisher path. Producer resolution may use a
        // durable checkout, but missing source evidence still refuses locally.
        let publisher = root.join("ci-hub/validation/publish_receipt.py");
        let reuse = reuse_existing_immutable_receipt(root, &args.repo, pr, &head, selected);
        let (artifact, receipt_source) = match reuse {
            Ok(Some(artifact)) => (artifact, "reused-immutable"),
            reuse_unavailable => {
                match publish_selected_receipt(
                    root,
                    &publisher,
                    &path,
                    &args.repo,
                    &head,
                    selected,
                    args.dry_run,
                ) {
                    Ok(artifact) => (artifact, "published"),
                    Err(error) => {
                        let detail = match reuse_unavailable {
                            Err(reuse_error) => {
                                format!("{error}; immutable receipt reuse refused: {reuse_error}")
                            }
                            Ok(None) => error.to_string(),
                            Ok(Some(_)) => unreachable!("handled reusable artifact above"),
                        };
                        eprintln!("ci-hub: apply-local-label: PR #{pr}: {detail}");
                        actions.push(serde_json::json!({"pr": pr, "head": head, "action": "receipt-failed", "detail": detail}));
                        failed += 1;
                        continue;
                    }
                }
            }
        };
        if !args.dry_run {
            if let Err(error) = bind_verified_receipt_to_pr(root, &args.repo, pr, &head, &artifact)
            {
                eprintln!("ci-hub: apply-local-label: PR #{pr}: {error}");
                actions.push(serde_json::json!({"pr": pr, "head": head, "action": "bind-failed", "detail": error.to_string()}));
                failed += 1;
                continue;
            }
        }
        let action = if args.dry_run { "would-bind" } else { "bound" };
        println!("PR #{pr}: {action} {LOCALLY_VALIDATED_LABEL} to counted exact-head receipt");
        actions.push(serde_json::json!({
            "pr": pr,
            "head": head,
            "action": action,
            "receipt_identity_sha256": artifact.selected_digest,
            "artifact_sha256": artifact.artifact_sha256,
            "path": artifact.path,
            "receipt_source": receipt_source,
            "producer_coverage_status": artifact.producer_coverage_status,
            "producer_paths": artifact.producer_paths,
            "producer_valid_commits": artifact.producer_valid_commits,
        }));
        applied += 1;
    }

    if args.json {
        let report =
            serde_json::json!({"schema_version": 1, "applied": applied, "actions": actions});
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serialize report")
        );
    }
    Ok(if failed == 0 { 0 } else { 1 })
}

fn print_obligations(root: &Path, args: ObligationsArgs) -> Result<i32, CiHubError> {
    let store = args
        .store
        .or_else(|| env::var_os("CI_HUB_OBLIGATIONS_STORE").map(PathBuf::from))
        .unwrap_or_else(|| root.join("ignored/ci-hub/obligations.jsonl"));
    let mut records = latest_obligations(&store)?;
    if args.actionable {
        records.retain(|record| record.remediation_required());
    }
    if !args.all {
        records.retain(|record| !record.is_closed());
    }
    records.sort_by(|left, right| {
        (&left.opened_at, &left.obligation_id).cmp(&(&right.opened_at, &right.obligation_id))
    });
    let unresolved: Vec<_> = records
        .iter()
        .filter(|record| !record.is_closed())
        .collect();
    let remediation: Vec<_> = unresolved
        .iter()
        .filter(|record| record.remediation_required())
        .collect();
    let sent_unacknowledged = remediation
        .iter()
        .filter(|record| record.dispatch_state() == "sent_unacknowledged")
        .count();
    let acknowledged = remediation
        .iter()
        .filter(|record| record.dispatch_state() == "acknowledged")
        .count();

    if args.json {
        #[derive(Serialize)]
        struct Output<'a> {
            obligations: &'a [ObligationRecord],
        }
        println!(
            "{}",
            serde_json::to_string(&Output {
                obligations: &records
            })
            .expect("typed obligation output is serializable")
        );
    } else if args.gate {
        let state = if !remediation.is_empty() {
            "remediation-required"
        } else if !unresolved.is_empty() {
            "open"
        } else {
            "clear"
        };
        println!("state={state}");
        println!("count={}", unresolved.len());
        println!("remediation_count={}", remediation.len());
        println!("sent_unacknowledged_count={sent_unacknowledged}");
        println!("acknowledged_count={acknowledged}");
        println!(
            "ids={}",
            if unresolved.is_empty() {
                "none".to_string()
            } else {
                unresolved
                    .iter()
                    .map(|record| record.obligation_id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            }
        );
        println!(
            "summary={}",
            if unresolved.is_empty() {
                "no-open-speculative-land-obligations".to_string()
            } else {
                unresolved
                    .iter()
                    .map(|record| obligation_summary(record))
                    .collect::<Vec<_>>()
                    .join(";")
            }
        );
    } else {
        let heading = if !remediation.is_empty() {
            "Speculative-land obligations: REMEDIATION REQUIRED"
        } else if !unresolved.is_empty() {
            "Speculative-land obligations: OPEN"
        } else {
            "Speculative-land obligations: CLEAR"
        };
        println!("{heading}");
        for record in &records {
            println!("  {}", obligation_summary(record));
            if let Some(failure) = &record.failure_summary {
                println!("    failure: {failure}");
            }
        }
    }
    Ok(if !remediation.is_empty() {
        2
    } else if !unresolved.is_empty() {
        1
    } else {
        0
    })
}

fn latest_obligations(path: &Path) -> Result<Vec<ObligationRecord>, CiHubError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path).map_err(|source| CiHubError::ObligationOpen {
        path: path.to_path_buf(),
        source,
    })?;
    file.lock_shared()
        .map_err(|source| CiHubError::ObligationOpen {
            path: path.to_path_buf(),
            source,
        })?;
    let mut latest = BTreeMap::new();
    for (index, line) in BufReader::new(&file).lines().enumerate() {
        let line_number = index + 1;
        let line = line.map_err(|source| CiHubError::ObligationRead {
            path: path.to_path_buf(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let record: ObligationRecord =
            serde_json::from_str(&line).map_err(|source| CiHubError::ObligationJson {
                path: path.to_path_buf(),
                line: line_number,
                source,
            })?;
        if record.schema_version != 1 {
            return Err(CiHubError::ObligationSchema {
                path: path.to_path_buf(),
                line: line_number,
                schema: record.schema_version,
            });
        }
        latest.insert(record.obligation_id.clone(), record);
    }
    FileExt::unlock(&file).map_err(|source| CiHubError::ObligationRead {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(latest.into_values().collect())
}

fn obligation_summary(record: &ObligationRecord) -> String {
    format!(
        "{} {}@{} overall={} local={} github={} recommendation={} dispatch={}",
        record.obligation_id,
        record.repo,
        &record.landed_sha[..record.landed_sha.len().min(12)],
        record.overall_state,
        record.local.state,
        record.github.state,
        record.recommendation_action(),
        record.dispatch_state()
    )
}

fn run_python(root: &Path, relative_script: &str, args: Vec<OsString>) -> Result<i32, CiHubError> {
    run_python_path(&root.join(relative_script), args)
}

fn run_bash(root: &Path, relative_script: &str, args: Vec<OsString>) -> Result<i32, CiHubError> {
    let script = root.join(relative_script);
    let mut command = Command::new("bash");
    command.current_dir(root).arg(&script).args(args);
    run_status(command, &script.display().to_string())
}

fn run_python_path(script: &Path, args: Vec<OsString>) -> Result<i32, CiHubError> {
    let mut command = Command::new("python3");
    command.arg(script).args(args);
    run_status(command, &script.display().to_string())
}

fn run_status(mut command: Command, tool: &str) -> Result<i32, CiHubError> {
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = command.status().map_err(|source| CiHubError::Launch {
        tool: tool.to_string(),
        source,
    })?;
    Ok(exit_status_code(status))
}

fn exit_status_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

fn push_option(arguments: &mut Vec<OsString>, name: &'static str, value: impl AsRef<OsStr>) {
    arguments.push(name.into());
    arguments.push(value.as_ref().to_os_string());
}

fn default_actor() -> String {
    env::var("AGENT")
        .or_else(|_| env::var("USER"))
        .unwrap_or_else(|_| "unknown".into())
}

fn to_exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECEIPT_SHA: &str = "630f44aab7fdf4ee52e572c38ae09818e92271b2";

    fn schema4_receipt_value() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 4,
            "started_at": "2026-08-01T12:00:00Z",
            "finished_at": "2026-08-01T12:01:00Z",
            "host": "validation-host",
            "slot": "lander",
            "profile": "full",
            "selection_mode": "full",
            "commit": RECEIPT_SHA,
            "tree": "f9294df9294df9294df9294df9294df9294df929",
            "commit_anchored": true,
            "tree_dirty": false,
            "result": "pass",
            "raw_result": "pass",
            "exit_code": 0,
            "executed_tests": 786,
            "filtered_tests": 693,
            "checks": 2,
            "gates_run": 2,
            "gates_expected": 2,
            "failures": 0,
            "real_seconds": 60.0,
            "log_file": "/tmp/validate-630f.log",
            "gates": [
                {"name": "fmt", "result": "pass", "exit_code": 0},
                {"name": "test", "result": "pass", "exit_code": 0}
            ]
        })
    }

    fn history_row(value: serde_json::Value) -> HistoryRow {
        serde_json::from_value(value).expect("valid history fixture")
    }

    #[test]
    fn canonical_receipt_accepts_genuine_schema4_and_carries_its_conditions() {
        let row = history_row(schema4_receipt_value());
        let assessment = assess_canonical_receipts(
            Path::new("."),
            &[row],
            RECEIPT_SHA,
            CANONICAL_VALIDATE_REPO,
        )
        .unwrap();
        assert_eq!(assessment.verdict, validate_status::Verdict::Validated);
        let receipt = newest_canonical_receipt(&assessment.qualifying).unwrap();
        assert_eq!(receipt.selected_tests, 786);
        assert_eq!(receipt.discovered_tests, 1479);
        assert_eq!(receipt.canonical_sha256.len(), 64);

        let report = describe_receipt(receipt);
        assert_eq!(report["repo"], CANONICAL_VALIDATE_REPO);
        assert_eq!(report["sha"], RECEIPT_SHA);
        assert_eq!(report["tree_dirty"], false);
        assert_eq!(report["checks"], 2);
        assert_eq!(report["gates_run"], 2);
        assert_eq!(report["gates_expected"], 2);
        assert_eq!(report["failures"], 0);
        assert_eq!(report["executed_tests"], 786);
        assert_eq!(report["selected_tests"], 786);
        assert_eq!(report["discovered_tests"], 1479);
        assert_eq!(
            report["count_derivation"],
            "selected_tests=executed_tests;discovered_tests=executed_tests+filtered_tests"
        );
        assert_eq!(
            report["coverage_basis"],
            "legacy-schema4-full-gates-and-aggregate-counts"
        );
        assert_eq!(report["coverage"], serde_json::Value::Null);
        assert_eq!(report["coverage_satisfied"], serde_json::Value::Null);
        assert_eq!(report["coverage_status"], "grandfathered-unknown");
        assert_eq!(report["receipt_identity"]["digest_algorithm"], "sha256");
        assert_eq!(report["receipt_identity"]["tuple"]["sha"], RECEIPT_SHA);
    }

    #[test]
    fn canonical_receipt_rejects_planted_pass_with_failures_and_no_gates() {
        let mut planted = schema4_receipt_value();
        planted["schema_version"] = serde_json::json!(5);
        planted["repo"] = serde_json::json!("hermit");
        planted["coverage"] = serde_json::json!({
            "planned_test_nodes": 1,
            "executed_test_nodes": 1,
            "zero_executed_nodes": [],
            "absent_nodes": []
        });
        planted["executed_tests"] = serde_json::json!(1);
        planted["filtered_tests"] = serde_json::json!(0);
        planted["failures"] = serde_json::json!(7);
        planted["checks"] = serde_json::json!(0);
        planted["gates_run"] = serde_json::json!(0);
        planted["gates_expected"] = serde_json::json!(5);
        planted["gates"] = serde_json::json!([]);

        let assessment = assess_canonical_receipts(
            Path::new("."),
            &[history_row(planted)],
            RECEIPT_SHA,
            CANONICAL_VALIDATE_REPO,
        )
        .unwrap();
        assert_ne!(assessment.verdict, validate_status::Verdict::Validated);
        assert!(assessment.qualifying.is_empty());
    }

    #[test]
    fn canonical_receipt_brackets_gate_coverage_and_repo_predicates() {
        for (name, mutate) in [
            ("nonzero failures", ("failures", serde_json::json!(1))),
            ("missing checks", ("checks", serde_json::Value::Null)),
            ("gate count mismatch", ("checks", serde_json::json!(1))),
            ("dirty tree", ("tree_dirty", serde_json::json!(true))),
            (
                "unanchored commit",
                ("commit_anchored", serde_json::json!(false)),
            ),
            (
                "wrong raw result",
                ("raw_result", serde_json::json!("fail")),
            ),
            ("missing tree", ("tree", serde_json::Value::Null)),
        ] {
            let mut value = schema4_receipt_value();
            value[mutate.0] = mutate.1;
            assert!(
                qualify_canonical_receipt(&history_row(value), RECEIPT_SHA).is_none(),
                "planted {name} row qualified"
            );
        }

        let mut red_gate = schema4_receipt_value();
        red_gate["gates"][0]["result"] = serde_json::json!("fail");
        red_gate["gates"][0]["exit_code"] = serde_json::json!(1);
        assert!(qualify_canonical_receipt(&history_row(red_gate), RECEIPT_SHA).is_none());

        let mut schema5_missing_coverage = schema4_receipt_value();
        schema5_missing_coverage["schema_version"] = serde_json::json!(5);
        schema5_missing_coverage["repo"] = serde_json::json!("hermit");
        assert!(
            qualify_canonical_receipt(&history_row(schema5_missing_coverage), RECEIPT_SHA)
                .is_none()
        );

        let mut schema5_zero_coverage = schema4_receipt_value();
        schema5_zero_coverage["schema_version"] = serde_json::json!(5);
        schema5_zero_coverage["repo"] = serde_json::json!("hermit");
        schema5_zero_coverage["coverage"] = serde_json::json!({
            "planned_test_nodes": 1,
            "executed_test_nodes": 0,
            "zero_executed_nodes": ["test.unit"],
            "absent_nodes": []
        });
        assert!(
            qualify_canonical_receipt(&history_row(schema5_zero_coverage), RECEIPT_SHA).is_none()
        );

        let mut schema5_missing_repo = schema4_receipt_value();
        schema5_missing_repo["schema_version"] = serde_json::json!(5);
        schema5_missing_repo["coverage"] = serde_json::json!({
            "planned_test_nodes": 1,
            "executed_test_nodes": 1,
            "zero_executed_nodes": [],
            "absent_nodes": []
        });
        schema5_missing_repo.as_object_mut().unwrap().remove("repo");
        assert!(
            qualify_canonical_receipt(&history_row(schema5_missing_repo), RECEIPT_SHA).is_none()
        );

        let error = assess_canonical_receipts(
            Path::new("."),
            &[history_row(schema4_receipt_value())],
            RECEIPT_SHA,
            "rrnewton/reverie",
        )
        .unwrap_err();
        assert!(error.contains("bound to rrnewton/hermit"));
    }

    #[test]
    fn canonical_receipt_digest_changes_with_receipt_content() {
        let first = history_row(schema4_receipt_value());
        let mut changed = schema4_receipt_value();
        changed["executed_tests"] = serde_json::json!(787);
        let second = history_row(changed);
        let first = qualify_canonical_receipt(&first, RECEIPT_SHA).unwrap();
        let second = qualify_canonical_receipt(&second, RECEIPT_SHA).unwrap();
        assert_ne!(first.canonical_sha256, second.canonical_sha256);
    }

    fn publisher_report_value(selected: &QualifyingReceipt, dry_run: bool) -> serde_json::Value {
        let producer_definition = serde_json::json!({
            ".github/workflows/ci-portable.yml": "1".repeat(40),
            "validate.sh": "2".repeat(40),
        });
        let receipt = serde_json::json!({
            "schema_version": 1,
            "repository": CANONICAL_VALIDATE_REPO,
            "commit": RECEIPT_SHA,
            "run_id": format!(
                "{RECEIPT_SHA}@{}@{}",
                selected.row.started_at.as_deref().unwrap(),
                selected.row.host.as_deref().unwrap()
            ),
            "source_log_file": selected.row.log_file,
            "durable_log_file": "/tmp/durable-validate.log",
            "log_sha256": "e".repeat(64),
            "producer": {
                "definition": producer_definition,
                "coverage_status": "legacy-selected-paths",
                "paths": [".github/workflows/ci-portable.yml", "validate.sh"],
                "resolved_from": "/fixture/producer-checkout",
            },
            "selected_receipt_identity": {
                "digest_algorithm": "sha256",
                "canonicalization": RECEIPT_CANONICALIZATION,
                "digest": selected.canonical_sha256,
            },
            "ledger_record": selected.row,
        });
        let artifact_body = serde_json::to_string(&receipt).unwrap();
        let artifact_sha256 = format!("{:x}", Sha256::digest(artifact_body.as_bytes()));
        serde_json::json!({
            "schema_version": 1,
            "action": if dry_run { "would-publish" } else { "published" },
            "receipt_commit": if dry_run { serde_json::Value::Null } else { serde_json::json!("e".repeat(40)) },
            "receipt_repository": VALIDATION_RECEIPT_REPO,
            "receipt_branch": VALIDATION_RECEIPT_BRANCH,
            "path": format!(
                "validation-receipts/{CANONICAL_VALIDATE_REPO}/{RECEIPT_SHA}/{}.json",
                artifact_sha256
            ),
            "receipt_identity_sha256": selected.canonical_sha256,
            "artifact_sha256": artifact_sha256,
            "artifact_body": artifact_body,
        })
    }

    fn rewrite_report_run_id(report: &mut serde_json::Value, run_id: &str, sha: &str) {
        let mut artifact: serde_json::Value =
            serde_json::from_str(report["artifact_body"].as_str().unwrap()).unwrap();
        artifact["run_id"] = serde_json::json!(run_id);
        let artifact_body = serde_json::to_string(&artifact).unwrap();
        let artifact_sha256 = format!("{:x}", Sha256::digest(artifact_body.as_bytes()));
        report["path"] = serde_json::json!(format!(
            "validation-receipts/{CANONICAL_VALIDATE_REPO}/{sha}/{artifact_sha256}.json"
        ));
        report["artifact_sha256"] = serde_json::json!(artifact_sha256);
        report["artifact_body"] = serde_json::json!(artifact_body);
    }

    /// A REAL one-commit git repo carrying the registered producer files, so the
    /// mint-side producer binding resolves against a genuine `git rev-parse
    /// <sha>:<path>` rather than a stub. Mirrors `make_producer_checkout` in
    /// ci-hub/validation/test_publish_receipt.py; deliberately no test-only
    /// override of the resolver itself, which would be a forgery path.
    fn make_producer_checkout(
        root: &std::path::Path,
    ) -> (std::path::PathBuf, String, std::path::PathBuf) {
        let repo = root.join("producer-checkout");
        std::fs::create_dir_all(repo.join(".github/workflows")).unwrap();
        std::fs::write(
            repo.join("validate.sh"),
            "#!/usr/bin/env bash\necho validate\n",
        )
        .unwrap();
        std::fs::write(repo.join(".github/workflows/ci-portable.yml"), "name: CI\n").unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "producer fixture"]);
        let sha = git(&["rev-parse", "HEAD"]);
        let definition = serde_json::json!({
            "validate.sh": git(&["rev-parse", &format!("{sha}:validate.sh")]),
            ".github/workflows/ci-portable.yml": git(&[
                "rev-parse",
                &format!("{sha}:.github/workflows/ci-portable.yml")
            ]),
        });
        let registry = root.join("producer-definition-fixture.json");
        std::fs::write(
            &registry,
            serde_json::to_vec(&serde_json::json!({
                "registered_at": sha,
                "registered_coverage_status": "legacy-selected-paths",
                "registered_valid_commits": [sha],
                "registered": definition,
            }))
            .unwrap(),
        )
        .unwrap();
        (repo, sha, registry)
    }

    #[test]
    fn python_publisher_and_rust_verifier_share_host_bound_run_identity() {
        let root = workspace_root().unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = env::temp_dir().join(format!(
            "ci-hub-publisher-contract-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let log = temp.join("validate.log");
        std::fs::write(&log, b"test result: ok. 786 passed; 0 failed\n").unwrap();
        let ledger = temp.join("ledger.jsonl");

        // MERGE NOTE (w9/reconcile-round2): publish_receipt.py now binds each
        // receipt to the producer definition it was minted by, resolved from the
        // VALIDATED COMMIT. So the row must name a real checkout at a real sha;
        // the previous hard-coded RECEIPT_SHA had no repository behind it.
        let (producer_repo, receipt_sha, producer_registry) = make_producer_checkout(&temp);
        let mut value = schema4_receipt_value();
        value["log_file"] = serde_json::json!(log.display().to_string());
        value["commit"] = serde_json::json!(receipt_sha);
        value["cwd"] = serde_json::json!(producer_repo.display().to_string());
        let selected = qualify_canonical_receipt(&history_row(value), &receipt_sha).unwrap();
        let publisher = root.join("ci-hub/validation/publish_receipt.py");
        let mut child = Command::new("python3")
            .arg(&publisher)
            .args([
                "--repo",
                CANONICAL_VALIDATE_REPO,
                "--sha",
                &receipt_sha,
                "--ledger",
                ledger.to_str().unwrap(),
                "--selected-receipt-sha256",
                &selected.canonical_sha256,
                "--canonicalization",
                RECEIPT_CANONICALIZATION,
                "--receipt-repo",
                VALIDATION_RECEIPT_REPO,
                "--receipt-branch",
                VALIDATION_RECEIPT_BRANCH,
                "--dry-run",
            ])
            .current_dir(&root)
            .env("PRODUCER_DEFINITION_REGISTRY", producer_registry)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(selected.canonical_row_json.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "Python publisher failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let verified = verify_publisher_report(
            &output.stdout,
            CANONICAL_VALIDATE_REPO,
            &receipt_sha,
            &selected,
            true,
        )
        .unwrap();
        let expected_run_id = format!(
            "{receipt_sha}@{}@{}",
            selected.row.started_at.as_deref().unwrap(),
            selected.row.host.as_deref().unwrap()
        );
        assert_eq!(verified.run_id, expected_run_id);

        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        for (name, run_id) in [
            (
                "obsolete hostless identity",
                format!(
                    "{receipt_sha}@{}",
                    selected.row.started_at.as_deref().unwrap()
                ),
            ),
            (
                "wrong-host identity",
                format!(
                    "{receipt_sha}@{}@other-host",
                    selected.row.started_at.as_deref().unwrap()
                ),
            ),
        ] {
            let mut tampered = report.clone();
            rewrite_report_run_id(&mut tampered, &run_id, &receipt_sha);
            let error = verify_publisher_report(
                serde_json::to_string(&tampered).unwrap().as_bytes(),
                CANONICAL_VALIDATE_REPO,
                &receipt_sha,
                &selected,
                true,
            )
            .unwrap_err();
            assert!(
                error.contains("run identity differs"),
                "{name} reached the wrong refusal: {error}"
            );
        }

        std::fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn mixed_strong_and_newer_weak_selects_one_strong_publisher_input() {
        let strong = history_row(schema4_receipt_value());
        let mut weak_value = schema4_receipt_value();
        weak_value["finished_at"] = serde_json::json!("2026-08-01T12:02:00Z");
        weak_value["checks"] = serde_json::json!(0);
        weak_value["gates_run"] = serde_json::json!(0);
        weak_value["gates_expected"] = serde_json::json!(6);
        weak_value["gates"] = serde_json::json!([]);
        let weak = history_row(weak_value);
        let assessment = assess_canonical_receipts(
            Path::new("."),
            &[strong.clone(), weak.clone()],
            RECEIPT_SHA,
            CANONICAL_VALIDATE_REPO,
        )
        .unwrap();
        assert_eq!(assessment.verdict, validate_status::Verdict::Validated);
        assert_eq!(assessment.qualifying.len(), 1);
        let selected = newest_canonical_receipt(&assessment.qualifying).unwrap();
        assert_eq!(selected.row.checks, strong.checks);
        assert_ne!(selected.row.finished_at, weak.finished_at);
        assert_eq!(
            selected.canonical_row_json.as_bytes(),
            serde_json::to_vec(&selected.row).unwrap()
        );
        let parsed: HistoryRow = serde_json::from_str(&selected.canonical_row_json).unwrap();
        assert_eq!(parsed.checks, Some(2));
        assert_eq!(parsed.gates.len(), 2);
    }

    #[test]
    fn publisher_report_must_match_selected_digest_and_exact_artifact_bytes() {
        let selected =
            qualify_canonical_receipt(&history_row(schema4_receipt_value()), RECEIPT_SHA).unwrap();
        let positive = publisher_report_value(&selected, true);
        let verified = verify_publisher_report(
            serde_json::to_string(&positive).unwrap().as_bytes(),
            CANONICAL_VALIDATE_REPO,
            RECEIPT_SHA,
            &selected,
            true,
        )
        .unwrap();
        assert_eq!(verified.selected_digest, selected.canonical_sha256);
        assert_eq!(verified.receipt_commit, None);

        let mut wrong_receipt_repository = positive.clone();
        wrong_receipt_repository["receipt_repository"] = serde_json::json!("attacker/fork");
        let error = verify_publisher_report(
            serde_json::to_string(&wrong_receipt_repository)
                .unwrap()
                .as_bytes(),
            CANONICAL_VALIDATE_REPO,
            RECEIPT_SHA,
            &selected,
            true,
        )
        .unwrap_err();
        assert!(error.contains("canonical receipt repository"));

        let mut wrong_selected_digest = positive.clone();
        wrong_selected_digest["receipt_identity_sha256"] = serde_json::json!("0".repeat(64));
        let error = verify_publisher_report(
            serde_json::to_string(&wrong_selected_digest)
                .unwrap()
                .as_bytes(),
            CANONICAL_VALIDATE_REPO,
            RECEIPT_SHA,
            &selected,
            true,
        )
        .unwrap_err();
        assert!(error.contains("does not match Rust selection"));

        let mut tampered_body = positive.clone();
        tampered_body["artifact_body"] = serde_json::json!("{}");
        let error = verify_publisher_report(
            serde_json::to_string(&tampered_body).unwrap().as_bytes(),
            CANONICAL_VALIDATE_REPO,
            RECEIPT_SHA,
            &selected,
            true,
        )
        .unwrap_err();
        assert!(error.contains("artifact bytes"));

        let mut wrong_row = positive;
        let mut artifact: serde_json::Value =
            serde_json::from_str(wrong_row["artifact_body"].as_str().unwrap()).unwrap();
        artifact["ledger_record"]["checks"] = serde_json::json!(0);
        let artifact_body = serde_json::to_string(&artifact).unwrap();
        let artifact_sha256 = format!("{:x}", Sha256::digest(artifact_body.as_bytes()));
        wrong_row["path"] = serde_json::json!(format!(
            "validation-receipts/{CANONICAL_VALIDATE_REPO}/{RECEIPT_SHA}/{artifact_sha256}.json"
        ));
        wrong_row["artifact_sha256"] = serde_json::json!(artifact_sha256);
        wrong_row["artifact_body"] = serde_json::json!(artifact_body);
        let error = verify_publisher_report(
            serde_json::to_string(&wrong_row).unwrap().as_bytes(),
            CANONICAL_VALIDATE_REPO,
            RECEIPT_SHA,
            &selected,
            true,
        )
        .unwrap_err();
        assert!(error.contains("differs from Rust-selected row"));
    }

    #[test]
    fn parses_typed_arm_command() {
        let cli = Cli::try_parse_from([
            "ci-hub",
            "arm-land",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--pr",
            "1219",
            "--land-mode",
            "admin",
            "--no-dispatch",
        ])
        .unwrap();
        let HubCommand::ArmLand(args) = cli.command else {
            panic!("wrong command variant")
        };
        assert!(matches!(args.land_mode, LandMode::Admin));
        assert_eq!(args.pr, Some(1219));
        assert!(args.no_dispatch);
    }

    #[test]
    fn parses_qualified_ledger_rows_command() {
        let cli = Cli::try_parse_from([
            "ci-hub",
            "ledger",
            "qualified-rows",
            "--ledger",
            "/tmp/ledger.jsonl",
        ])
        .unwrap();
        let HubCommand::Ledger(args) = cli.command else {
            panic!("wrong command variant")
        };
        let LedgerCommand::QualifiedRows(args) = args.command else {
            panic!("wrong ledger subcommand variant")
        };
        assert_eq!(args.ledger, Some(PathBuf::from("/tmp/ledger.jsonl")));
    }

    #[test]
    fn parses_attribute_reds_command() {
        let cli = Cli::try_parse_from([
            "ci-hub",
            "ledger",
            "attribute-reds",
            "--ledger",
            "/tmp/ledger.jsonl",
            "--commit",
            "fedc81ed",
            "--last",
            "0",
            "--json",
            "--persist",
        ])
        .unwrap();
        let HubCommand::Ledger(args) = cli.command else {
            panic!("wrong command variant")
        };
        let LedgerCommand::AttributeReds(args) = args.command else {
            panic!("wrong ledger subcommand variant")
        };
        assert_eq!(args.ledger, Some(PathBuf::from("/tmp/ledger.jsonl")));
        assert_eq!(args.commit.as_deref(), Some("fedc81ed"));
        assert_eq!(args.last, Some(0));
        assert!(args.json);
        assert!(args.persist);
    }

    #[test]
    fn parses_rebase_aware_landing_verifier() {
        let cli = Cli::try_parse_from([
            "ci-hub",
            "verify-landing",
            "1219",
            "--repo",
            "rrnewton/hermit",
            "--target",
            "main",
            "--json",
        ])
        .unwrap();
        let HubCommand::VerifyLanding(args) = cli.command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.reference, "1219");
        assert_eq!(args.target, "main");
        assert!(args.json);
    }

    #[test]
    fn parses_landing_claim_audit_with_full_identity_context() {
        let cli = Cli::try_parse_from([
            "ci-hub",
            "verify-landing",
            "1592",
            "--item",
            "PR #1592",
            "--claimed-oid",
            "abedbe29",
        ])
        .unwrap();
        let HubCommand::VerifyLanding(args) = cli.command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.item.as_deref(), Some("PR #1592"));
        assert_eq!(args.claimed_oid.as_deref(), Some("abedbe29"));
    }

    #[test]
    fn parses_landing_verifier_sha_and_legacy_alias() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let cli = Cli::try_parse_from(["ci-hub", "verify-landed-pr", sha]).unwrap();
        let HubCommand::VerifyLanding(args) = cli.command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.reference, sha);
    }

    #[test]
    fn validate_status_accepts_positional_or_flag_sha() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        // Positional form (the reflex that used to be rejected).
        let cli = Cli::try_parse_from(["ci-hub", "validate-status", sha]).unwrap();
        let HubCommand::ValidateStatus(args) = cli.command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.sha_positional.as_deref(), Some(sha));
        assert_eq!(args.sha, None);

        // Flag form still works.
        let cli = Cli::try_parse_from(["ci-hub", "validate-status", "--sha", sha]).unwrap();
        let HubCommand::ValidateStatus(args) = cli.command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.sha.as_deref(), Some(sha));
        assert_eq!(args.sha_positional, None);

        // Positional + --pr conflict, and positional + --sha conflict.
        assert!(Cli::try_parse_from(["ci-hub", "validate-status", sha, "--pr", "1"]).is_err());
        assert!(Cli::try_parse_from(["ci-hub", "validate-status", sha, "--sha", sha]).is_err());
    }

    #[test]
    fn passthrough_commands_accept_worker_flags() {
        let tick = Cli::try_parse_from(["ci-hub", "tick", "--flush", "--no-header"])
            .unwrap()
            .command;
        let HubCommand::Tick(args) = tick else {
            panic!("wrong command variant")
        };
        assert_eq!(args.args, ["--flush", "--no-header"]);

        let history = Cli::try_parse_from(["ci-hub", "history", "--since", "2026-08-03"])
            .unwrap()
            .command;
        assert!(matches!(history, HubCommand::History(_)));
    }

    #[test]
    fn parses_typed_active_work_command() {
        let command = Cli::try_parse_from([
            "ci-hub",
            "active-work",
            "--agent-snapshot",
            "/tmp/agents.json",
            "--max-snapshot-age",
            "300",
            "--json",
        ])
        .unwrap()
        .command;
        let HubCommand::ActiveWork(args) = command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.agent_snapshot, Some(PathBuf::from("/tmp/agents.json")));
        assert_eq!(args.max_snapshot_age, 300);
        assert!(args.json);
    }

    #[test]
    fn parses_typed_validate_worktrees_command() {
        let command = Cli::try_parse_from([
            "ci-hub",
            "validate-worktrees",
            "--runs",
            "10",
            "--stale-hours",
            "6",
            "--data-dir",
            "/tmp/ci-hub",
            "--json",
        ])
        .unwrap()
        .command;
        let HubCommand::ValidateWorktrees(args) = command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.runs, 10);
        assert_eq!(args.stale_hours, 6.0);
        assert_eq!(args.data_dir, Some(PathBuf::from("/tmp/ci-hub")));
        assert!(args.json);
    }

    #[test]
    fn parses_typed_load_probe_command() {
        let command = Cli::try_parse_from([
            "ci-hub",
            "load-probe",
            "--sample-seconds",
            "0.5",
            "--max-executing-percent",
            "40",
            "--top",
            "3",
            "--json",
        ])
        .unwrap()
        .command;
        let HubCommand::LoadProbe(args) = command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.sample_seconds, 0.5);
        assert_eq!(args.max_executing_percent, 40.0);
        assert_eq!(args.top, 3);
        assert!(args.json);
        assert!(HubCommand::LoadProbe(args).cost_spec().is_some());
    }

    #[test]
    fn parses_validate_stop_passthrough() {
        let command = Cli::try_parse_from(["ci-hub", "validate-stop", "--all"])
            .unwrap()
            .command;
        let HubCommand::ValidateStop(args) = command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.args, vec![OsString::from("--all")]);
        assert!(HubCommand::ValidateStop(args).cost_spec().is_none());
    }

    #[test]
    fn parses_validate_run_passthrough() {
        let command = Cli::try_parse_from([
            "ci-hub",
            "validate-run",
            "--checkout",
            "/tmp/hermit",
            "--agent",
            "hermit-test",
            "--target",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--",
            "full",
        ])
        .unwrap()
        .command;
        let HubCommand::ValidateRun(args) = command else {
            panic!("wrong command variant")
        };
        assert!(args.args.contains(&OsString::from("--checkout")));
        assert!(args.args.contains(&OsString::from("full")));
        assert!(HubCommand::ValidateRun(args).cost_spec().is_none());
    }

    #[test]
    fn parses_shared_history_query_commands() {
        let newest = Cli::try_parse_from([
            "ci-hub",
            "newest-green",
            "--branch",
            "release-frontier",
            "--no-fetch",
            "--no-cache",
            "--json",
        ])
        .unwrap()
        .command;
        let HubCommand::NewestGreen(args) = newest else {
            panic!("wrong command variant")
        };
        assert!(args.query.no_fetch);
        assert_eq!(args.query.branch, "release-frontier");
        assert!(args.no_cache);
        assert!(args.query.json);
        assert!(HubCommand::NewestGreen(args).cost_spec().is_some());

        let first_bad =
            Cli::try_parse_from(["ci-hub", "first-bad", "test.detcore_misc", "--no-fetch"])
                .unwrap()
                .command;
        let HubCommand::FirstBad(args) = first_bad else {
            panic!("wrong command variant")
        };
        assert_eq!(args.cell_or_gate, "test.detcore_misc");
        assert!(args.query.no_fetch);
        assert_eq!(args.query.branch, "main");
        assert!(HubCommand::FirstBad(args).cost_spec().is_some());
    }

    #[test]
    fn gate_floor_excludes_older_green_bases() {
        let commits = vec!["tip".into(), "floor".into(), "old-green".into()];
        assert_eq!(
            history_at_or_after_gate_floor(commits, "origin/main", "floor").unwrap(),
            vec!["tip", "floor"]
        );
    }

    #[test]
    fn effective_floor_follows_registry_output_without_a_rust_constant() {
        let old = parse_effective_gate_floor(
            br#"{"ok":true,"effective_floor":"1111111111111111111111111111111111111111","effective_kind":"merge-gate"}"#,
        )
        .unwrap();
        let added = parse_effective_gate_floor(
            br#"{"ok":true,"effective_floor":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","effective_kind":"producer-anchor"}"#,
        )
        .unwrap();

        assert_ne!(old.sha, added.sha);
        assert_eq!(added.sha, "a".repeat(40));
        assert_eq!(added.kind, "producer-anchor");
    }

    #[test]
    fn refused_registry_result_cannot_become_a_green_base() {
        let error = parse_effective_gate_floor(
            br#"{"ok":false,"effective_floor":null,"effective_kind":null}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("refused"));
        assert!(error.contains("refusing to guess"));
    }

    #[test]
    fn shallow_floor_result_is_an_explicit_third_state() {
        let result = parse_gate_floor_resolution(
            br#"{"ok":false,"effective_floor":null,"effective_kind":null,"verdict":"UNVERIFIABLE-SHALLOW-HISTORY","history_depth":11,"required_history_depth":"full","required_history_depth_min":12}"#,
        )
        .unwrap();
        assert_eq!(
            result,
            GateFloorResolution::UnverifiableShallow {
                visible_depth: 11,
                required_depth: "full".into(),
                required_depth_min: 12,
            }
        );
    }

    #[test]
    fn missing_gate_floor_refuses_instead_of_guessing() {
        let error = history_at_or_after_gate_floor(
            vec!["tip".into(), "old-green".into()],
            "origin/main",
            "floor",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("does not contain required"));
        assert!(error.contains("refusing to guess"));
    }

    #[test]
    fn root_help_groups_commands_for_first_time_users() {
        let error = Cli::try_parse_from(["ci-hub", "--help"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        let help = error.to_string();
        let headings = [
            "START HERE",
            "HISTORY & FORENSICS",
            "PR & RUNNER HEALTH",
            "LANDING & BATCH CONTROL",
            "POST-LAND REMEDIATION",
        ];
        let mut previous = 0;
        for heading in headings {
            let position = help.find(heading).expect("missing help group");
            assert!(position >= previous, "help groups are out of order");
            previous = position;
        }
        assert!(help.contains("newest-green"));
        assert!(help.contains("[default: --branch main]"));
        assert!(help.contains("history                 Query the retained GitHub Actions timeline"));
        assert!(help.contains("local-history           Inspect local validate receipts"));
        assert!(!help.contains(&["newest", "green", "main"].join("-")));

        let clap_commands: std::collections::BTreeSet<String> =
            <Cli as clap::CommandFactory>::command()
                .get_subcommands()
                .map(|command| command.get_name().to_string())
                .collect();
        let listed: Vec<String> = ROOT_HELP
            .lines()
            .filter_map(|line| line.strip_prefix("  "))
            .filter_map(|line| line.split_whitespace().next())
            .filter(|word| clap_commands.contains(*word))
            .map(str::to_string)
            .collect();
        let listed_commands: std::collections::BTreeSet<String> = listed.iter().cloned().collect();
        assert_eq!(
            listed.len(),
            listed_commands.len(),
            "duplicate help command"
        );
        assert_eq!(
            listed_commands, clap_commands,
            "root help must classify every public subcommand exactly once"
        );
    }

    #[test]
    fn quickstart_is_short_pure_agent_workflow() {
        let command = Cli::try_parse_from(["ci-hub", "quickstart"])
            .unwrap()
            .command;
        assert!(matches!(command, HubCommand::Quickstart));
        assert!(AGENT_QUICKSTART.starts_with("ci-hub agent quickstart\n"));
        assert!(AGENT_QUICKSTART.contains("ci-hub/ci-hub health"));
        assert!(AGENT_QUICKSTART.contains("validate-worktrees"));
        assert!(AGENT_QUICKSTART.contains("newest-green"));
        assert!(AGENT_QUICKSTART.contains("first-bad CELL_OR_GATE"));
        assert!(AGENT_QUICKSTART.contains("obligations --actionable"));
        assert!(AGENT_QUICKSTART.contains("land-lock run"));
        assert!(AGENT_QUICKSTART.lines().count() < 45);
    }

    #[test]
    fn trivial_reads_have_no_cost_wrapper() {
        let obligations = Cli::try_parse_from(["ci-hub", "obligations"])
            .unwrap()
            .command;
        assert!(obligations.cost_spec().is_none());
        let inherit = Cli::try_parse_from([
            "ci-hub",
            "inherit-obligations",
            "--agent",
            "hermit-lander",
            "--session",
            "session-1",
        ])
        .unwrap()
        .command;
        assert!(inherit.cost_spec().is_none());
        let wake = Cli::try_parse_from([
            "ci-hub",
            "record-obligation-wake",
            "--target",
            "hermit-lander",
        ])
        .unwrap()
        .command;
        assert!(wake.cost_spec().is_none());
        let status = Cli::try_parse_from(["ci-hub", "land-lock", "status"])
            .unwrap()
            .command;
        assert!(status.cost_spec().is_none());
        let worktrees = Cli::try_parse_from(["ci-hub", "validate-worktrees"])
            .unwrap()
            .command;
        assert!(worktrees.cost_spec().is_none());
    }

    #[test]
    fn parses_typed_ci_mode_commands() {
        let set = Cli::try_parse_from([
            "ci-hub",
            "ci-mode",
            "set",
            "constrained",
            "--reason",
            "queued=6 max-age=2h03m",
            "--dry-run",
        ])
        .unwrap()
        .command;
        let HubCommand::CiMode(args) = set else {
            panic!("wrong command variant")
        };
        let CiModeCommand::Set(set_args) = args.command else {
            panic!("wrong subcommand variant")
        };
        assert!(matches!(set_args.mode, CiModeValue::Constrained));
        assert_eq!(set_args.reason, "queued=6 max-age=2h03m");
        assert!(set_args.dry_run);

        let fire = Cli::try_parse_from([
            "ci-hub",
            "ci-mode",
            "fire",
            "--pr",
            "1563",
            "--lane",
            "privileged",
        ])
        .unwrap()
        .command;
        let HubCommand::CiMode(args) = fire else {
            panic!("wrong command variant")
        };
        let CiModeCommand::Fire(fire_args) = args.command.clone() else {
            panic!("wrong subcommand variant")
        };
        assert_eq!(fire_args.pr, 1563);
        assert!(matches!(fire_args.lane, CiModeLane::Privileged));
        assert_eq!(fire_args.repo, "rrnewton/hermit");

        // ci-mode is a trivial local read/write dispatcher: no cost wrapper.
        assert!(HubCommand::CiMode(args).cost_spec().is_none());
    }

    #[test]
    fn parses_typed_batch_commands() {
        let set = Cli::try_parse_from([
            "ci-hub",
            "batch",
            "set",
            "cpu-timeout-landing",
            "--reason",
            "priority: land cpu_timeout chain",
            "--pr",
            "1566",
            "--pr",
            "1568",
        ])
        .unwrap()
        .command;
        let HubCommand::Batch(args) = set else {
            panic!("wrong command variant")
        };
        let BatchCommand::Set(set_args) = args.command else {
            panic!("wrong subcommand variant")
        };
        assert_eq!(set_args.name, "cpu-timeout-landing");
        assert_eq!(set_args.repo, CI_BATCH_DEFAULT_REPO);
        assert_eq!(set_args.prs, vec![1566, 1568]);
        assert!(!set_args.dry_run);

        let add = Cli::try_parse_from([
            "ci-hub",
            "batch",
            "add",
            "--repo",
            "rrnewton/reverie",
            "--pr",
            "42",
        ])
        .unwrap()
        .command;
        let HubCommand::Batch(args) = add else {
            panic!("wrong command variant")
        };
        let BatchCommand::Add(member_args) = args.command.clone() else {
            panic!("wrong subcommand variant")
        };
        assert_eq!(member_args.repo, "rrnewton/reverie");
        assert_eq!(member_args.prs, vec![42]);

        // batch is a local read/write dispatcher: no cost wrapper, like ci-mode.
        assert!(HubCommand::Batch(args).cost_spec().is_none());

        // members_from deduplicates a repeated --pr within one repo.
        let members = members_from("rrnewton/hermit", &[7, 7, 9]);
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].number, 7);
        assert_eq!(members[1].number, 9);
    }

    #[test]
    fn substantive_network_work_is_costed() {
        let command = Cli::try_parse_from(["ci-hub", "main-health"])
            .unwrap()
            .command;
        assert!(command.cost_spec().is_some());
    }
}
