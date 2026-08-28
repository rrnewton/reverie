#!/usr/bin/env -S rust-script --force
//! Typed front door for dev-hermit validation, GH Actions, and runner operations.
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
//! hermit-manifest-plan = { path = "../hermit/ci/manifest-plan" }
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

// ZERO-WARNINGS, MADE INTRINSIC RATHER THAN DELEGATED TO AN EXTERNAL GATE.
// A rust-script is invisible to `cargo clippy --workspace`, so the project rule
// held here only for as long as something remembered to cover this file.
//
// ⚠️ A CLOSED, NAMED SET -- DELIBERATELY NOT `deny(warnings)`. These compile at
// INVOCATION, so a blanket deny would let a future toolchain introducing any new
// lint stop the script from RUNNING. That is a production break, not a build
// break, and it would be worse than the warnings it prevents. An enumerated set
// cannot be widened by a toolchain upgrade. New lints are caught instead by
// `scripts/lint-rust-scripts.sh`, which compiles this file under `-D warnings`
// externally, where a failure blocks a commit rather than an execution.
#![deny(
    dead_code,
    unused_imports,
    unused_mut,
    unused_variables,
    unused_must_use
)]
#![recursion_limit = "256"]

#[path = "lib/bounded_output.rs"]
mod bounded_output;
#[path = "lib/failure_obligations.rs"]
mod failure_obligations;
#[path = "lib/history_queries.rs"]
mod history_queries;
#[path = "lib/landing_lock.rs"]
mod landing_lock;
#[path = "lib/lease.rs"]
mod lease;
#[path = "lib/ledger_event.rs"]
mod ledger_event;
#[path = "lib/measured.rs"]
mod measured;
#[path = "lib/published_receipt.rs"]
mod published_receipt;
#[path = "lib/qualifying_receipt.rs"]
mod qualifying_receipt;
#[path = "lib/records.rs"]
mod records;
#[path = "lib/validate_lock.rs"]
mod validate_lock;
#[path = "lib/validate_run_handle.rs"]
mod validate_run_handle;
// The legacy assessment type remains exercised by its compatibility tests;
// production consumers below use the stricter canonical receipt assessment.
#[allow(dead_code)]
#[path = "lib/validate_status.rs"]
mod validate_status;

use crate::bounded_output::BoundedOutput;
use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand, ValueEnum};
use history_queries::{
    CellEvidenceCache, FirstBadOutcome, HistoryQueryEngine, NewestGreenCache, NewestGreenOutcome,
};
use ledger_event::LedgerEvent;
use published_receipt::{
    IdentityRequirement, ProducerDefinitionEvidence, ProducerDefinitionExpectation,
    PublishedValidationReceipt,
};
use records::HistoryRow;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use thiserror::Error;

const DEFAULT_MAIN_RUN_LIMIT: usize = 100;
const DEFAULT_VALIDATE_HISTORY_LIMIT: usize = 10;
const DEFAULT_WARN_THRESHOLD: usize = 10;
const DEFAULT_AGENT_SNAPSHOT_MAX_AGE_SECONDS: u64 = 10 * 60;
const GATE_FLOOR_POLICY: &str = "registry-effective-floor";
const CANONICAL_VALIDATE_REPO: &str = "rrnewton/hermit";
const CANONICAL_REVERIE_REPO: &str = "rrnewton/reverie";
const ROOT_HELP: &str = r#"Typed front door for dev-hermit validation, GH Actions, and runner operations

Usage: ci-hub <COMMAND>

START HERE
  Begin with fleet-wide truth and ownership before narrowing the question.
  quickstart              Print the opinionated five-step agent workflow
  health                  Combine current-main, open-PR, and ownership diagnostics
  active-work             Reconcile tasks, owners, and live ORC agents

READ-ONLY STATUS AND HISTORY
  Inspect current and retained state without changing product source, labels, or queues.
  main-health             Query current main-branch GitHub workflow health
  pr-status               Pull fresh open-PR GitHub status [alias: fresh]
  runner-health           Summarize self-hosted runners and recent workflows
  load-probe              Measure current CPU and memory pressure
  validate-status         List recent validation runs or inspect one SHA/PR
  local-history           List recent local validation runs [default: 10]
  newest-green            Find the newest ledger-qualified green commit [default: --branch main]
  first-bad               Find a retained PASS -> FAIL transition for a cell or gate
  hosted-status           Check the registered exact-head GH Actions result
  receipt-digest          Recompute a HistoryRow's canonical exact-row digest
  history                 Query the retained GitHub Actions timeline
  green-time              Estimate carried-forward green/red time and densification gaps
  verify-landing          Verify a PR replay SHA or commit SHA on a fetched target

VALIDATION ACTIONS
  Start, stop, admit, or persist validation work.
  validate-run            Launch detached validation through ci-hub admission
  validate-stop           Stop detached validate-* user units and verify termination
  validate-lock           Two validation slots; benchmarks reserve both (GRANT/QUEUE/REFUSE)
  ledger                  Read or update canonical validate-ledger views
  refresh-history         Ingest fresh GitHub and local history into the local store

LANDING AND COORDINATION ACTIONS
  Mutate landing, review, GitHub, batch, or scheduled operational state.
  tick                    Run due scheduled operational-health gates
  land-lock               Acquire, renew, inspect, or release the fleet landing mutex
  apply-local-label       Reconcile the derived label with exact-head validation truth
  review-attest           Record a coordinator's typed exact-head review outcome
  ci-mode                 Inspect or change constrained GH Actions admission mode
  batch                   Inspect or edit the named current GH Actions batch
  ci-timeout              Cancel hosted runs starved waiting to start; reroute to local

Run `ci-hub <COMMAND> --help` for exhaustive flags and `ci-hub quickstart` for
the short operational workflow.

Options:
  -h, --help     Print help
  -V, --version  Print version
"#;
const AGENT_QUICKSTART: &str = r#"ci-hub agent quickstart

FOR: the normal Hermit loop: work, validation, derived label, landing, ancestry
proof, repeat.

POLICY: Follow project skill `hermit-validation-authority` for CI mode, evidence
qualification, hard/soft green, and the local-versus-GitHub boundary. This help
lists commands; it does not define those rules independently.

1. Check current state, then work in the assigned slot and publish the PR:
     ./ci-hub/ci-hub health
     ./ci-hub/ci-hub fresh
2. Run validation through the sole production path, then ask the canonical
   reader whether the recorded row qualifies:
     ./ci-hub/ci-hub validate-run --checkout WORKTREE --agent AGENT --target SHA --pr PR -- full
     ./ci-hub/ci-hub validate-status --sha SHA

3. Reconcile the cache label with the exact-head row; never type it by hand:
     ./ci-hub/ci-hub apply-local-label --pr PR --repo rrnewton/hermit

4. Land your own reviewed PR, then prove publication and repeat:
     ./ci-hub/landing/land-pr.sh PR BRANCH --agent AGENT
   The tracked lander uses `land-lock run`, rechecks the LEDGER, and proves
   ancestry.

5. Recover and diagnose without changing the authority hierarchy:
     ./ci-hub/ci-hub local-history --since YYYY-MM-DD
     ./ci-hub/ci-hub newest-green
     ./ci-hub/ci-hub first-bad CELL_OR_GATE
   Query runner health for capacity only: ./ci-hub/ci-hub runner-health --all

Run from the dev-hermit root. Networked subcommands apply with-proxy internally.
Meaningful work prints estimated and actual wall+CPU cost; quickstart itself is
pure and performs no workspace discovery, filesystem writes, or network calls.
"#;

#[derive(Parser, Debug)]
#[command(
    name = "ci-hub",
    about = "Typed front door for dev-hermit validation, GH Actions, and runner operations",
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
    /// Summarize current-main, open-PR, and ownership health.
    Health(HealthArgs),
    /// Reconcile TaskGraph state, task ownership, and live ORC agents.
    ActiveWork(ActiveWorkArgs),
    /// Query current-main GitHub workflow health.
    MainHealth(MainHealthArgs),
    /// Pull fresh open-PR GH Actions status via pinned agent-utils.
    #[command(visible_alias = "fresh")]
    PrStatus(PrStatusArgs),
    /// Run due operational-health gates.
    Tick(PassthroughArgs),
    /// Verify a PR replay SHA or commit SHA against a freshly fetched target.
    #[command(visible_alias = "verify-landed-pr")]
    VerifyLanding(VerifyLandingArgs),
    /// Incrementally ingest GH Actions and local VALIDATE history.
    RefreshHistory(RefreshHistoryArgs),
    /// Query the local commit, VALIDATE, and GH Actions history store.
    History(PassthroughArgs),
    /// Green-time over a linear branch history: sparse signal carried forward,
    /// plus the densification plan that shrinks the estimate's error.
    GreenTime(PassthroughArgs),
    /// List recent machine-wide validation runs, or use --all for every record.
    LocalHistory(LocalHistoryArgs),
    /// Summarize self-hosted runners and recent workflows.
    RunnerHealth(RunnerHealthArgs),
    /// Gate timing-sensitive work on measured CPU and memory utilization.
    LoadProbe(LoadProbeArgs),
    /// List recent validation runs, or inspect one commit/PR verdict.
    ValidateStatus(ValidateStatusArgs),
    /// Query the registered exact-head GH Actions result.
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
    /// Reconcile `locally-validated` with each PR's exact-head validation record.
    ApplyLocalLabel(ApplyLocalLabelArgs),
    /// Record a coordinator's exact-head review outcome and cache labels.
    ReviewAttest(ReviewAttestArgs),
    /// Operate the shared-file landing mutex.
    LandLock(landing_lock::LandLockArgs),
    /// Operate validation admission (two validates, or one benchmark reserving both slots).
    ValidateLock(validate_lock::ValidateLockArgs),
    /// Inspect or switch the committed GH Actions-constrained mode and its GitHub projection.
    CiMode(CiModeArgs),
    /// Inspect or edit the named current GH Actions batch and its ci-batch PR labels.
    Batch(BatchArgs),
    /// Cancel PRs whose GH Actions starved waiting to start and reroute to local validation.
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
/// A subcommand whose real argument surface lives in the tool it forwards to.
///
/// `disable_help_flag` is deliberate. clap has nothing to print for these --
/// the arguments are a `trailing_var_arg` catch-all -- so with clap's own help
/// enabled, `-h` printed a stub reading only `[ARGS]...` while the actual
/// surface sat in the forwarded tool's parser. Measured 2026-08-20: the owner
/// could not find `--checkout`, `--agent` or `--target` for `validate-run`
/// because `-h` did not mention them, and the requirement is enforced only in
/// ci-hub/validate/start_unit.py. Disabling clap's flag lets `-h` and `--help`
/// travel through to the parser that actually knows the answer.
#[command(disable_help_flag = true)]
struct PassthroughArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<OsString>,
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
struct RefreshHistoryArgs {
    #[arg(long)]
    full: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    extra: Vec<OsString>,
}

#[derive(Args, Clone, Debug)]
struct LocalHistoryArgs {
    /// Show this many newest runs (newest first).
    #[arg(long, default_value_t = DEFAULT_VALIDATE_HISTORY_LIMIT, conflicts_with = "all")]
    limit: usize,
    /// Show every retained run (the slower, complete view).
    #[arg(long)]
    all: bool,
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
    /// Number of recent validation runs to show when no SHA or PR is supplied.
    #[arg(long, default_value_t = DEFAULT_VALIDATE_HISTORY_LIMIT)]
    limit: usize,
    /// Do not include process-verified validation runs that are still executing.
    #[arg(long, conflicts_with = "in_progress_only")]
    exclude_in_progress: bool,
    /// Show only process-verified validation runs that are still executing.
    #[arg(long, conflicts_with = "exclude_in_progress")]
    in_progress_only: bool,
    /// Optional identity columns for the current/recent table. Comma-separated
    /// values and repeated flags are both accepted; `none` selects no identity
    /// column. The default stays narrow with SHA alone.
    #[arg(long, value_enum, value_delimiter = ',', default_value = "sha")]
    columns: Vec<ValidateStatusColumn>,
    /// Render human-readable timestamps relative to now, in local time, or in UTC.
    /// Machine-readable JSON always retains the original RFC 3339 values.
    #[arg(long, value_enum, default_value_t = ValidateStatusTimes::Relative)]
    times: ValidateStatusTimes,
    /// Repository used to resolve --pr head SHAs.
    #[arg(long, default_value = "rrnewton/hermit")]
    repo: String,
    /// Emit the machine-readable verdict report.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ValidateStatusColumn {
    Sha,
    Branch,
    #[value(name = "gitdepth", alias = "git-depth")]
    GitDepth,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ValidateStatusTimes {
    Relative,
    Local,
    Utc,
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
    /// Read a complete published validation receipt from stdin rather than a
    /// bare HistoryRow. The embedded row is still the value canonicalized.
    #[arg(long)]
    published_receipt: bool,
    /// Repository the published receipt must name.
    #[arg(long, requires = "published_receipt")]
    repository: Option<String>,
    /// Accept the historical owner-service receipt shape that predates the
    /// selected-receipt identity. Current receipts must carry it.
    #[arg(long, requires = "published_receipt")]
    allow_legacy_missing_identity: bool,
    /// JSON file containing the exact producer definition expected for the
    /// validated commit.
    #[arg(long, requires = "published_receipt")]
    expected_producer_record: Option<PathBuf>,
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
struct QualifiedRowsArgs {}

#[derive(Args, Clone, Debug)]
struct AttributeRedsArgs {
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
    /// Sweep every open PR, adding backed labels and removing unbacked labels.
    #[arg(long, conflicts_with = "pr")]
    all_open: bool,
    /// Repository whose PR labels are reconciled.
    #[arg(long, default_value = "rrnewton/hermit")]
    repo: String,
    /// Report intended reconciliation actions without editing any label.
    #[arg(long)]
    dry_run: bool,
    /// Emit the machine-readable per-PR action report.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Debug, ValueEnum)]
enum ReviewFamily {
    Codex,
    Claude,
}

impl ReviewFamily {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum ReviewOutcome {
    Approval,
    Refusal,
}

impl ReviewOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Refusal => "refusal",
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum CriticalReviewClass {
    HermitSyscall,
    DetcoreScheduling,
    ReverieApi,
}

impl CriticalReviewClass {
    fn as_str(&self) -> &'static str {
        match self {
            Self::HermitSyscall => "hermit-syscall",
            Self::DetcoreScheduling => "detcore-scheduling",
            Self::ReverieApi => "reverie-api",
        }
    }
}

#[derive(Args, Clone, Debug)]
struct ReviewAttestArgs {
    /// Positive pull-request number whose live head is re-read before mutation.
    #[arg(long)]
    pr: u64,
    /// Supported fork repository carrying the PR and existing review labels.
    #[arg(long, default_value = "rrnewton/hermit")]
    repo: String,
    /// Exact lowercase 40-hex head reviewed by the dispatched reviewer.
    #[arg(long)]
    head: String,
    /// Coordinator-selected reviewer family; unknown values fail before mutation.
    #[arg(long, value_enum)]
    family: ReviewFamily,
    /// Review outcome; unknown values fail before mutation.
    #[arg(long, value_enum)]
    outcome: ReviewOutcome,
    /// Full URL of the reviewer's exact verdict comment.
    #[arg(long)]
    comment_url: String,
    /// Exact dispatched reviewer agent name.
    #[arg(long)]
    reviewer: String,
    /// Durable task id through which the review was dispatched and returned.
    #[arg(long)]
    task: String,
    /// Numbered review round whose existing activity label is applied.
    #[arg(long, default_value_t = 1)]
    round: u8,
    /// Existing critical-change class; also applies post-facto-human-review.
    #[arg(long, value_enum)]
    critical: Option<CriticalReviewClass>,
    /// Coordinator identity checked by the GitHub disclosure boundary.
    #[arg(long)]
    who: String,
    /// Coordinator team slug emitted by the GitHub disclosure boundary.
    #[arg(long)]
    team: String,
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
/// carrying this label is exempt from the constrained-mode gate and gets GH Actions.
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
    /// Dispatch targeted GH Actions for one PR head without auto-arming any fan-out.
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
    /// Every PR push auto-fans-out the GH Actions workflows.
    Auto,
    /// Auto-fan-out is suppressed; GH Actions runs only when explicitly fired.
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

/// Committed source of truth for the GH Actions-constrained mode. Absent file == auto.
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
            reason: "default: no committed GH Actions-mode state on disk; treated as auto".into(),
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

/// Committed source of truth for the named current GH Actions batch. Absent file == no
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
            Self::ReviewAttest(_) => CostSpec {
                tool: "ci-hub/review-attest",
                basis: "not measured: bounded live-head, exact-comment and repository-label GitHub reads plus one attestation comment and optional cache-label mutation".into(),
            },
            Self::Quickstart
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
    #[error("ci-hub: local-history --json returned an invalid typed history row: {0}")]
    HistoryJson(#[source] serde_json::Error),
    #[error("ci-hub: cannot read GH Actions-mode state {path}: {source}")]
    CiModeRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("ci-hub: cannot write GH Actions-mode state {path}: {source}")]
    CiModeWrite {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("ci-hub: GH Actions-mode state {path} is not valid JSON: {source}")]
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

    // This is deliberately contact-time visibility, not authority and not an
    // obligation. The exact-main classifier re-dereferences the canonical
    // receipt and prints only when the current tip is recorded soft-green but
    // is not yet hard-green. It is advisory: a broken warning probe must say
    // UNKNOWN without changing the command the caller actually requested.
    warn_if_main_is_soft_green(&root);

    match execute(&root, cli.command) {
        Ok(code) => to_exit_code(code),
        Err(error) => {
            eprintln!("{error}");
            to_exit_code(error.exit_code())
        }
    }
}

fn warn_if_main_is_soft_green(root: &Path) {
    if env::var_os("CI_HUB_SKIP_SOFT_MAIN_WARNING").is_some() {
        return;
    }
    let output = Command::new("python3")
        .arg(root.join("ci-hub/landing/main_soft_green.py"))
        .arg("status")
        .current_dir(root)
        .bounded_output();
    match output {
        Ok(output) if output.status.success() => {
            let _ = write_python_stderr(&output.stderr);
        }
        Ok(output) => eprintln!(
            "ci-hub: WARNING: current-main hard/soft state is UNKNOWN: {}",
            String::from_utf8_lossy(&python_stderr_without_allocator_warning(&output.stderr))
                .trim()
        ),
        Err(error) => {
            eprintln!("ci-hub: WARNING: current-main hard/soft state is UNKNOWN: {error}")
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
        .bounded_output()
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
            // Preserve main/PR's more-informative exit 2; orphans only redden a
            // health poll that would otherwise be green.
            let base = if main_code != 0 { main_code } else { pr_code };
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
                .arg(root.join(".ops-state.yaml"))
                .arg("--fired-state")
                .arg(root.join(".tick-hub/fired-state"))
                .args(["--current-tick-min", "60"])
                .args(args.args);
            run_status(command, "tick-hub")
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
            // `root` is the exact linked worktree whose ci-hub code is running,
            // while validate-lock deliberately stores authority under the
            // repository's main worktree. Pass that already-canonicalized state
            // root to the producer instead of making Python independently
            // rediscover it; two path resolvers are how the holder and its run
            // record ended up in different directories.
            run_python(
                root,
                "ci-hub/validate/start_unit.py",
                validate_run_forwarded_args(root, args.args),
            )
        }
        HubCommand::ValidateStop(args) => {
            run_python(root, "ci-hub/validate/stop_units.py", args.args)
        }
        HubCommand::Ledger(args) => match args.command {
            LedgerCommand::QualifiedRows(qualified_args) => {
                let _ = qualified_args;
                run_python(root, "ci-hub/validate/qualified_rows.py", Vec::new())
            }
            LedgerCommand::AttributeReds(red_args) => {
                let mut forwarded = Vec::new();
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
        HubCommand::ReviewAttest(args) => {
            let mut forwarded = Vec::new();
            push_option(&mut forwarded, "--pr", args.pr.to_string());
            push_option(&mut forwarded, "--repo", args.repo);
            push_option(&mut forwarded, "--head", args.head);
            push_option(&mut forwarded, "--family", args.family.as_str());
            push_option(&mut forwarded, "--outcome", args.outcome.as_str());
            push_option(&mut forwarded, "--comment-url", args.comment_url);
            push_option(&mut forwarded, "--reviewer", args.reviewer);
            push_option(&mut forwarded, "--task", args.task);
            push_option(&mut forwarded, "--round", args.round.to_string());
            if let Some(critical) = args.critical {
                push_option(&mut forwarded, "--critical", critical.as_str());
            }
            push_option(&mut forwarded, "--who", args.who);
            push_option(&mut forwarded, "--team", args.team);
            run_python(root, "ci-hub/bin/review-attest", forwarded)
        }
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
                .bounded_output()
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

/// The validation driver intentionally points XDG_CONFIG_HOME at an isolated temporary
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
    .bounded_output()
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
    .bounded_output()
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
        println!("GH Actions mode: {}", state.mode.to_uppercase());
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
        println!("GH Actions mode set to {value}.");
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
        .bounded_output()
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
    let message = format!("ci-hub: set GH Actions mode to {value}");
    let output = Command::new(root.join("scripts/parent-main-write"))
        .current_dir(root)
        .args(["commit", "-m", &message, "--", CI_MODE_STATE_PATH])
        .bounded_output()
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
    .bounded_output()
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
            "Current GH Actions batch: exempt from constrained-mode gate; gets GH Actions now.",
        ],
    )
    .bounded_output()
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
    .bounded_output()
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
        .bounded_output()
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
        "ci-hub: clear GH Actions batch".to_string()
    } else {
        format!("ci-hub: set GH Actions batch to {name}")
    };
    let output = Command::new(root.join("scripts/parent-main-write"))
        .current_dir(root)
        .args(["commit", "-m", &message, "--", CI_BATCH_STATE_PATH])
        .bounded_output()
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
/// command never runs it here (`validate-run` creates the durable identity before
/// the validate-lock grants one validation slot).
fn ci_timeout_enqueue_command(head_sha: &str, pr: u64) -> String {
    format!(
        "ci-hub validate-run --checkout \"$PWD\" --agent ci-timeout --target {head_sha} --pr {pr} -- full"
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
    .bounded_output()
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
    .bounded_output()
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
    .bounded_output()
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
    .bounded_output()
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
    .bounded_output()
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
        .bounded_output()
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
        "portable GH Actions waited {}s (> {}m) to start; cancelled and rerouted to local validation",
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
    if args.all {
        forwarded.push("--all".into());
    } else {
        push_option(&mut forwarded, "--limit", args.limit.to_string());
    }
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
            .bounded_output()
            .map_err(|source| CiHubError::Launch {
                tool: "local-history".into(),
                source,
            })?;
        write_python_stderr(&output.stderr).ok();
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

/// Resolve the one logical validate-ledger root.
fn ledger_path(root: &Path) -> PathBuf {
    root.join(validate_status::LEDGER_REL)
}

/// Load and parse the validate ledger. The default `ledger/` argument is a
/// LOGICAL root, not a file: the one canonical adapter unions per-machine live
/// journals and published shards. A file is accepted only as an explicit
/// diagnostic/test fixture; production call sites never manufacture one.
fn load_ledger_rows_reporting(
    path: &Path,
) -> Result<(Vec<HistoryRow>, Vec<validate_status::LedgerParseFailure>), CiHubError> {
    let canonical = path.file_name().is_some_and(|name| name == "ledger");
    let buf = if canonical {
        let root = path.parent().unwrap_or_else(|| Path::new("."));
        let output = Command::new("python3")
            .arg(root.join("ci-hub/ledger/validate_rows.py"))
            .arg("rows")
            .bounded_output()
            .map_err(|source| CiHubError::Launch {
                tool: "canonical validate-ledger union".into(),
                source,
            })?;
        if !output.status.success() {
            return Err(CiHubError::ValidateStatus(format!(
                "canonical validate-ledger union refused: {}",
                String::from_utf8_lossy(&python_stderr_without_allocator_warning(&output.stderr))
                    .trim()
            )));
        }
        String::from_utf8(output.stdout).map_err(|error| {
            CiHubError::ValidateStatus(format!("canonical ledger emitted non-UTF-8 data: {error}"))
        })?
    } else {
        match std::fs::read_to_string(path) {
            Ok(buf) => buf,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(source) => {
                return Err(CiHubError::LedgerRead {
                    path: path.to_path_buf(),
                    source,
                })
            }
        }
    };
    let (rows, failures) = validate_status::parse_ledger(&buf);
    if !failures.is_empty() {
        // BE LOUD, AND SAY WHAT. The previous version printed only a count.
        // Unreadable receipts in the authority ledger scrolled past behind that
        // count, and nobody could act on them because the
        // reason had already been discarded inside `parse_ledger`. A reader of
        // the file that decides landing authority owes the operator the reason.
        eprintln!(
            "ci-hub: LEDGER RECORDS UNREADABLE: {} of {} line(s) in {} could not be parsed as a validate receipt.",
            failures.len(),
            failures.len() + rows.len(),
            path.display()
        );
        // Name EVERY rejected record. Reporting only a prefix recreates the
        // original defect for every row past the cap: it is still invisible
        // and still has no identity an operator can act on.
        for failure in &failures {
            eprintln!("ci-hub:   {failure}");
        }
        eprintln!(
            "ci-hub: these records are INVISIBLE as verdict evidence. Each is named above so the \
             writer can be fixed and validation rerun. Do not widen the reader or rewrite the ledger: \
             a receipt that cannot be read does not exist as far as any verdict is concerned."
        );
    }
    Ok((rows, failures))
}

/// Rows only, for call sites that do not need to reason about what was
/// unreadable. The failures are still REPORTED by `load_ledger_rows_reporting`;
/// this discards only the caller's handle on them, never the operator's notice.
fn load_ledger_rows(path: &Path) -> Result<Vec<HistoryRow>, CiHubError> {
    Ok(load_ledger_rows_reporting(path)?.0)
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
    .bounded_output()
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

/// Read the current PR head and whether its derived validation cache label is
/// present in one GitHub snapshot. Reconciliation must not combine a head from
/// one API response with labels from another.
fn gh_pr_head_and_local_label(
    root: &Path,
    repo: &str,
    pr: u64,
) -> Result<(String, bool), CiHubError> {
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
            "headRefOid,labels",
        ],
    )
    .bounded_output()
    .map_err(|source| CiHubError::Launch {
        tool: "gh pr view head and labels".into(),
        source,
    })?;
    if !output.status.success() {
        return Err(CiHubError::Gh {
            context: format!("pr view head and labels #{pr}"),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        CiHubError::ValidateStatus(format!("invalid head/labels JSON for PR #{pr}: {error}"))
    })?;
    let head = value
        .get("headRefOid")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if head.is_empty() {
        return Err(CiHubError::Gh {
            context: format!("pr view head and labels #{pr}"),
            message: "empty headRefOid".into(),
        });
    }
    let has_label = value
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|labels| {
            labels.iter().any(|label| {
                label.get("name").and_then(serde_json::Value::as_str)
                    == Some(LOCALLY_VALIDATED_LABEL)
            })
        });
    Ok((head, has_label))
}

fn remove_local_validation_label(root: &Path, repo: &str, pr: u64) -> Result<(), CiHubError> {
    let pr_arg = pr.to_string();
    let output = gh_command(
        root,
        &[
            "pr",
            "edit",
            &pr_arg,
            "--repo",
            repo,
            "--remove-label",
            LOCALLY_VALIDATED_LABEL,
        ],
    )
    .bounded_output()
    .map_err(|source| CiHubError::Launch {
        tool: "gh pr edit remove locally-validated".into(),
        source,
    })?;
    if !output.status.success() {
        return Err(CiHubError::Gh {
            context: format!("pr edit remove label #{pr}"),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalLabelReconcileAction {
    Bind,
    Remove,
    LeaveAbsent,
}

fn local_label_reconcile_action(validated: bool, has_label: bool) -> LocalLabelReconcileAction {
    if validated {
        LocalLabelReconcileAction::Bind
    } else if has_label {
        LocalLabelReconcileAction::Remove
    } else {
        LocalLabelReconcileAction::LeaveAbsent
    }
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
    .bounded_output()
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
const RECEIPT_CANONICALIZATION: &str = published_receipt::CANONICALIZATION;

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

#[derive(Debug)]
struct CanonicalReceiptAssessment {
    sha: String,
    verdict: validate_status::Verdict,
    qualifying: Vec<QualifyingReceipt>,
    disqualified: Vec<HistoryRow>,
    failed_records: usize,
    withheld_nonpass_records: usize,
    unresolved_failure_obligations: Vec<failure_obligations::UnresolvedFailureObligation>,
}

fn is_oid(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn extra_str<'a>(row: &'a HistoryRow, key: &str) -> Option<&'a str> {
    row.extra.get(key).and_then(|value| value.as_str())
}

fn canonical_validation_repo(repo: &str) -> Option<&'static str> {
    match repo {
        "hermit" | CANONICAL_VALIDATE_REPO => Some(CANONICAL_VALIDATE_REPO),
        "reverie" | CANONICAL_REVERIE_REPO => Some(CANONICAL_REVERIE_REPO),
        _ => None,
    }
}

fn row_matches_validation_repo(row: &HistoryRow, repo: &str) -> bool {
    match canonical_validation_repo(repo) {
        Some(CANONICAL_VALIDATE_REPO) => matches!(
            row.repo.as_deref(),
            None | Some("hermit") | Some(CANONICAL_VALIDATE_REPO)
        ),
        Some(CANONICAL_REVERIE_REPO) => matches!(
            row.repo.as_deref(),
            Some("reverie") | Some(CANONICAL_REVERIE_REPO)
        ),
        _ => false,
    }
}

fn parse_failure_matches_validation_repo(
    failure: &validate_status::LedgerParseFailure,
    repo: &str,
) -> bool {
    match canonical_validation_repo(repo) {
        Some(CANONICAL_VALIDATE_REPO) => matches!(
            failure.repo.as_deref(),
            None | Some("hermit") | Some(CANONICAL_VALIDATE_REPO)
        ),
        Some(CANONICAL_REVERIE_REPO) => matches!(
            failure.repo.as_deref(),
            Some("reverie") | Some(CANONICAL_REVERIE_REPO)
        ),
        _ => false,
    }
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
    let published = args
        .published_receipt
        .then(|| PublishedValidationReceipt::parse(input.as_bytes()))
        .transpose()
        .map_err(CiHubError::ValidateStatus)?;
    let row: HistoryRow = match &published {
        Some(receipt) => receipt.ledger_record.clone(),
        None => serde_json::from_str(&input).map_err(|error| {
            CiHubError::ValidateStatus(format!("receipt-digest input is not a HistoryRow: {error}"))
        })?,
    };
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
        // Both non-accepting states block, exactly as the previous `bool` did.
        // They are reported differently because they are different facts: a
        // REFUSAL is about this receipt, an INDETERMINATE is about our ability
        // to read it. Blaming the evidence for a reader fault is what sends an
        // actor with an incomplete checkout to disbelieve valid receipts.
        match crate::qualifying_receipt::shared_base_boundary_outcome(
            &row,
            current_base,
            current_reverie_base,
            repo_checkout,
            reverie_checkout,
        ) {
            crate::qualifying_receipt::BoundaryOutcome::Accepted => {}
            crate::qualifying_receipt::BoundaryOutcome::Refused => {
                return Err(CiHubError::ValidateStatus(
                    "receipt-digest HistoryRow failed final merge-boundary base verification"
                        .into(),
                ));
            }
            crate::qualifying_receipt::BoundaryOutcome::Indeterminate(reason) => {
                return Err(CiHubError::ValidateStatus(format!(
                    "CANNOT DEREFERENCE PRODUCER DEFINITION: the merge-boundary authority could \
                     not judge this receipt ({reason}). This is a READER FAULT, not a verdict \
                     about the receipt: it still blocks, because absence is not permission, but \
                     do not treat it as evidence that the receipt is bad. Repair the reader \
                     (checkout completeness, python3 availability) and ask again."
                )));
            }
        }
    }
    let (canonical_row, digest) = canonical_row_json_and_sha256(&row).ok_or_else(|| {
        CiHubError::ValidateStatus("cannot canonicalize receipt HistoryRow".into())
    })?;
    let mut effective_producer = None;
    if let Some(receipt) = &published {
        let repository = args.repository.as_deref().ok_or_else(|| {
            CiHubError::ValidateStatus(
                "receipt-digest --published-receipt requires --repository".into(),
            )
        })?;
        let expected_producer = args
            .expected_producer_record
            .as_deref()
            .map(|path| {
                let bytes = std::fs::read(path).map_err(|error| {
                    CiHubError::ValidateStatus(format!(
                        "cannot read expected producer record {}: {error}",
                        path.display()
                    ))
                })?;
                serde_json::from_slice::<ProducerDefinitionExpectation>(&bytes).map_err(|error| {
                    CiHubError::ValidateStatus(format!(
                        "expected producer record {} is malformed: {error}",
                        path.display()
                    ))
                })
            })
            .transpose()?;
        receipt
            .validate(
                repository,
                &args.sha,
                &row,
                &digest,
                if args.allow_legacy_missing_identity {
                    IdentityRequirement::LegacyOptional
                } else {
                    IdentityRequirement::Required
                },
                expected_producer.as_ref(),
            )
            .map_err(CiHubError::ValidateStatus)?;
        let mut producer = receipt.producer.clone();
        if producer.valid_commits.is_none() {
            producer.valid_commits = expected_producer
                .as_ref()
                .and_then(|expected| expected.valid_commits.clone());
        }
        effective_producer = Some(producer);
    }
    if args.canonical_row {
        print!("{canonical_row}");
    } else if args.json {
        let mut report = serde_json::json!({
            "schema_version": 1,
            "sha": args.sha,
            "digest_algorithm": "sha256",
            "canonicalization": RECEIPT_CANONICALIZATION,
            "digest": digest,
        });
        if let Some(producer) = &effective_producer {
            report["producer_coverage_status"] = serde_json::json!(producer.coverage_status);
            report["producer_paths"] = serde_json::json!(producer.paths);
            report["producer_valid_commits"] = serde_json::json!(producer.valid_commits);
        }
        println!("{report}");
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

    // Receipt qualification deliberately maps both an absent and a malformed
    // tree to a non-qualifying row. Other consumers need to distinguish those
    // states, so the shared accessor retains that distinction.
    let _tree = row.tree().ok()??;
    let checks = row.checks?;
    let gates_run = row.gates_run?;
    let gates_expected = row.gates_expected?;
    if gates_expected == 0
        || !crate::qualifying_receipt::gate_accounting_complete(row)
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

/// Return the complete evidence bundle iff one row alone proves Reverie's
/// versioned local-validation contract. Direct `validate.sh` rows are useful
/// diagnostics but never qualify: only the central runner can carry both a
/// live validate-lock ancestry proof and observed safe-ci step containment.
fn qualify_reverie_receipt(row: &HistoryRow, sha: &str) -> Option<QualifyingReceipt> {
    // v1 preserves exact historical seven-gate receipts. v2 is the current
    // six-gate contract after hosted merge-gate policy enforcement retired.
    const V1_GATES: [&str; 7] = [
        "Cross-client skill discovery",
        "Merge-gate policy",
        "Build workspace",
        "Test regular workspace cases",
        "Documentation tests",
        "Clippy",
        "Rustfmt",
    ];
    const V2_GATES: [&str; 6] = [
        "Cross-client skill discovery",
        "Build workspace",
        "Test regular workspace cases",
        "Documentation tests",
        "Clippy",
        "Rustfmt",
    ];
    if row.schema_version? < 5
        || !row_matches_validation_repo(row, CANONICAL_REVERIE_REPO)
        || row.commit.as_deref() != Some(sha)
        || !is_oid(sha)
        || row.commit_anchored != Some(true)
        || row.tree_dirty != Some(false)
        || row.profile.as_deref() != Some("full")
        || row.selection_mode.as_deref() != Some("full")
        || row.full_coverage != Some(true)
        || row.result.as_deref() != Some("pass")
        || extra_str(row, "raw_result") != Some("pass")
        || row.exit_code != Some(0)
        || row.failures != Some(0)
        || extra_str(row, "producer") != Some("ci-hub-reverie-validate-run")
        || extra_str(row, "admission") != Some("ci-hub-validate-lock")
        || row.concurrent_validates != Some(0)
        || extra_str(row, "concurrency_proof") != Some("validate_lock_owner_ancestry")
    {
        return None;
    }
    let gates: &[&str] = match extra_str(row, "reverie_validation_policy")? {
        "reverie-local-validation/v1" => &V1_GATES,
        "reverie-local-validation/v2" => &V2_GATES,
        _ => return None,
    };
    // Match the Hermit receipt contract: both absent and malformed trees are
    // non-qualifying here, while the shared accessor keeps them distinguishable
    // for consumers that must report malformed rows.
    let _tree = row.tree().ok()??;
    let checks = row.checks?;
    let gates_run = row.gates_run?;
    let gates_expected = row.gates_expected?;
    let expected_gate_count = gates.len() as u64;
    if checks != expected_gate_count
        || gates_run != expected_gate_count
        || gates_expected != expected_gate_count
        || row.gates.len() != gates.len()
        || row
            .gates
            .iter()
            .zip(gates.iter().copied())
            .any(|(gate, expected)| {
                gate.name != expected
                    || gate.result.as_deref() != Some("pass")
                    || gate.exit_code != Some(0)
            })
    {
        return None;
    }
    let executed_tests = row.executed_tests?;
    let filtered_tests = row.filtered_tests?;
    let coverage = row.coverage.as_ref()?;
    if executed_tests <= 0
        || filtered_tests < 0
        || coverage.planned_test_nodes != 2
        || coverage.executed_test_nodes != 2
        || !crate::qualifying_receipt::coverage_satisfied(coverage)
    {
        return None;
    }
    let safe_ci = row.extra.get("safe_ci")?.as_object()?;
    let cgroup = safe_ci.get("cgroup")?.as_str()?;
    if safe_ci.get("required")?.as_bool()? != true
        || safe_ci.get("observed")?.as_bool()? != true
        || safe_ci.get("fallback_allowed")?.as_bool()? != false
        || safe_ci.get("step")?.as_str()? != "validate.reverie-full"
        || !cgroup.contains("safe-ci-")
        || !cgroup.contains("/step-")
    {
        return None;
    }
    let authority = row.extra.get("validate_lock_authority")?.as_object()?;
    if authority.get("schema_version")?.as_u64()? != 1
        || authority.get("admissible")?.as_bool()? != true
        || authority.get("canonical_anchor_held")?.as_bool()? != true
        || authority.get("holder")?.get("kind")?.as_str()? != "reverie-validate"
        || authority.get("holder")?.get("target")?.as_str()? != sha
        || authority.get("owner")?.get("liveness")?.as_str()? != "alive"
    {
        return None;
    }
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
        selected_tests: executed_tests,
        discovered_tests: executed_tests.checked_add(filtered_tests)?,
        count_derivation:
            "selected_tests=executed_tests;discovered_tests=executed_tests+filtered_tests",
        coverage_basis: "reverie-declared-two-test-gates",
        canonical_sha256,
        producer_definition: None,
    })
}

fn assess_canonical_receipts(
    root: &Path,
    rows: &[HistoryRow],
    sha: &str,
    repo: &str,
) -> Result<CanonicalReceiptAssessment, String> {
    let repo = canonical_validation_repo(repo).ok_or_else(|| {
        format!(
            "canonical local receipt ledger supports only {CANONICAL_VALIDATE_REPO} and {CANONICAL_REVERIE_REPO}, not {repo}"
        )
    })?;
    let mut qualifying = Vec::new();
    let mut disqualified = Vec::new();
    let mut saw_failed = false;
    let mut saw_needs_rerun = false;
    let mut saw_truncated = false;
    let mut saw_no_result = false;
    for row in rows {
        if !row_matches_validation_repo(row, repo) || row.commit.as_deref() != Some(sha) {
            continue;
        }
        let receipt = if repo == CANONICAL_VALIDATE_REPO {
            qualify_canonical_receipt(row, sha)
        } else {
            qualify_reverie_receipt(row, sha)
        };
        if let Some(receipt) = receipt {
            // A clean exact-commit full run is the validation authority. The
            // former six-file allowlist made validator improvements unable to
            // qualify until a second repository blessed their blob hashes.
            // Keep any producer map as optional diagnostic data, never as a
            // condition for accepting the run.
            qualifying.push(receipt);
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
    let repo_rows = rows
        .iter()
        .filter(|row| row_matches_validation_repo(row, repo))
        .cloned()
        .collect::<Vec<_>>();
    let qualifying_target_rows = qualifying
        .iter()
        .map(|receipt| receipt.row.clone())
        .collect::<Vec<_>>();
    let product_checkout = root.join(if repo == CANONICAL_REVERIE_REPO {
        "reverie"
    } else {
        "hermit"
    });
    let mut ancestry_cache: BTreeMap<(String, String), Result<bool, String>> = BTreeMap::new();
    let unresolved_failure_obligations = failure_obligations::unresolved(
        &repo_rows,
        sha,
        &qualifying_target_rows,
        qualifying_receipt::active()
            .cell_results
            .applies_at_schema_min,
        qualifying_receipt::active()
            .cell_results
            .supported_schema_max,
        |ancestor, target| {
            if ancestor == target {
                return Ok(true);
            }
            if let Some(result) = ancestry_cache.get(&(ancestor.to_owned(), target.to_owned())) {
                return result.clone();
            }
            let result = match Command::new("git")
                .arg("-C")
                .arg(&product_checkout)
                .args(["merge-base", "--is-ancestor", ancestor, target])
                .status()
            {
                Ok(status) if status.success() => Ok(true),
                Ok(status) if status.code() == Some(1) => Ok(false),
                Ok(status) => Err(format!(
                    "cannot establish commit ancestry {ancestor} -> {target}: git exited {status}"
                )),
                Err(error) => Err(format!(
                    "cannot establish commit ancestry {ancestor} -> {target}: {error}"
                )),
            };
            ancestry_cache.insert((ancestor.to_owned(), target.to_owned()), result.clone());
            result
        },
    )?;
    // A genuine same-commit failure and a durable per-cell obligation both win
    // over a lucky PASS. Withheld non-verdicts remain non-poisoning.
    let verdict = if saw_failed {
        validate_status::Verdict::FailedOnRecord
    } else if !unresolved_failure_obligations.is_empty() {
        validate_status::Verdict::NeedsRerun
    } else if !qualifying.is_empty() {
        validate_status::Verdict::Validated
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
    let status_assessment = validate_status::assess(&repo_rows, sha);
    Ok(CanonicalReceiptAssessment {
        sha: sha.to_string(),
        verdict,
        qualifying,
        disqualified,
        failed_records: status_assessment.failed_records,
        withheld_nonpass_records: status_assessment.withheld_nonpass_records,
        unresolved_failure_obligations,
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
    let tree = row
        .tree()
        .expect("qualified receipt has a well-formed tree")
        .expect("qualified receipt has tree");
    let repo = row
        .repo
        .as_deref()
        .and_then(canonical_validation_repo)
        .unwrap_or(CANONICAL_VALIDATE_REPO);
    serde_json::json!({
        "schema_version": row.schema_version,
        "repo": repo,
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
        // Intentional skips are PLANNED-NODE accounting only. They remain
        // outside the executed gate list and therefore can never become PASS.
        "skipped_nodes": row.extra.get("skipped_nodes").cloned().unwrap_or_else(|| serde_json::json!(0)),
        "intentional_skipped_nodes": row.extra.get("intentional_skipped_nodes").cloned().unwrap_or_else(|| serde_json::json!([])),
        "dependency_skipped_nodes": row.extra.get("dependency_skipped_nodes").cloned().unwrap_or_else(|| serde_json::json!([])),
        "unaccounted_nodes": row.extra.get("unaccounted_nodes").cloned().unwrap_or_else(|| serde_json::json!([])),
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
        .bounded_output()
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
        .bounded_output()
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

fn branch_history_with_trees(
    repo: &Path,
    branch_ref: &str,
) -> Result<(Vec<String>, BTreeMap<String, String>), CiHubError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["log", "--first-parent", "--format=%H%x09%T", branch_ref])
        .bounded_output()
        .map_err(|source| CiHubError::Launch {
            tool: "git log trees for history query".into(),
            source,
        })?;
    if !output.status.success() {
        return Err(CiHubError::HistoryQuery(format!(
            "cannot read Git trees for {branch_ref}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let mut commits = Vec::new();
    let mut trees = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((commit, tree)) = line.trim().split_once('\t') else {
            return Err(CiHubError::HistoryQuery(format!(
                "git log emitted a malformed commit/tree row for {branch_ref}: {line}"
            )));
        };
        if !is_oid(commit) || !is_oid(tree) {
            return Err(CiHubError::HistoryQuery(format!(
                "git log emitted a malformed commit/tree value for {branch_ref}: {line}"
            )));
        }
        commits.push(commit.to_string());
        trees.insert(commit.to_string(), tree.to_string());
    }
    if commits.is_empty() {
        return Err(CiHubError::HistoryQuery(format!(
            "{branch_ref} has no commits"
        )));
    }
    Ok((commits, trees))
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
        .bounded_output()
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
    if metadata.is_dir() {
        let mut total = 0_u64;
        let mut newest = 0_u128;
        let mut pending = vec![path.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).map_err(|source| CiHubError::LedgerRead {
                path: dir.clone(),
                source,
            })? {
                let entry = entry.map_err(|source| CiHubError::LedgerRead {
                    path: dir.clone(),
                    source,
                })?;
                let item = entry.path();
                let item_meta = entry.metadata().map_err(|source| CiHubError::LedgerRead {
                    path: item.clone(),
                    source,
                })?;
                if item_meta.is_dir() {
                    pending.push(item);
                } else if item.extension().is_some_and(|ext| ext == "jsonl") {
                    total = total.saturating_add(item_meta.len());
                    newest = newest.max(
                        item_meta
                            .modified()
                            .ok()
                            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|duration| duration.as_nanos())
                            .unwrap_or(0),
                    );
                }
            }
        }
        return Ok((total, newest));
    }
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok((metadata.len(), modified_ns))
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

fn newest_green_branch_line(
    report: &history_queries::NewestGreenReport,
    cache_hit: bool,
) -> String {
    format!(
        "BRANCH {} tip={} commits-after-green={} failed={} needs-rerun={} truncated={} \
         no-result={} not-validated={} recorded={} ledger-unreadable={} no-record={} cache={}",
        report.branch,
        report.branch_tip,
        report.commits_after_green,
        report.commits_failed_on_record,
        report.commits_needing_rerun,
        report.commits_truncated,
        report.commits_with_no_result,
        report.commits_not_validated,
        report.commits_with_records,
        report.commits_with_unreadable_record,
        report.commits_without_any_record,
        if cache_hit { "hit" } else { "miss" },
    )
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
    println!("{}", newest_green_branch_line(report, cache_hit));
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
    let ledger = ledger_path(root);
    let (ledger_len, ledger_modified_ns) = ledger_stamp(&ledger)?;
    // Preserve the cache field for schema compatibility, but validator file
    // hashes no longer decide whether a completed exact-commit run is green.
    let producer_definition_authority = String::new();
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

    let (rows, parse_failures) = load_ledger_rows_reporting(&ledger)?;
    let rows = rows
        .into_iter()
        .filter(|row| row_matches_validation_repo(row, CANONICAL_VALIDATE_REPO))
        .collect::<Vec<_>>();
    let unreadable_commits = parse_failures
        .into_iter()
        .filter(|failure| parse_failure_matches_validation_repo(failure, CANONICAL_VALIDATE_REPO))
        .filter_map(|failure| failure.commit)
        .collect();
    // Verdicts come from the ledger rows exactly as the exact-SHA
    // validate-status path reads them. Log/cache enrichment belongs to
    // first-bad's cell detail and must not change a commit verdict here.
    match HistoryQueryEngine::new(commits, rows)
        .with_unreadable_commits(unreadable_commits)
        .newest_green(
            &args.query.branch,
            &branch_ref,
            &floor_policy,
            &effective_floor.sha,
        ) {
        NewestGreenOutcome::Found(report) => {
            let cache = NewestGreenCache {
                schema_version: 7,
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
        .bounded_output()
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
        "FIRST-BAD {} matched-by={} cell={} observed={} profile={} selection={}",
        report.first_bad.sha,
        report.first_bad_matched_by,
        report.matched_name,
        report.first_bad.finished_at.as_deref().unwrap_or("unknown"),
        report.first_bad.profile,
        report.first_bad.selection_mode,
    );
    println!("BRANCH {} ref={}", report.branch, report.branch_ref);
    println!(
        "LAST-GOOD {} matched-by={} observed={} commits-between={} no-cell-record={} first-bad-mixed={}",
        report.last_good.sha,
        report.last_good_matched_by,
        report.last_good.finished_at.as_deref().unwrap_or("unknown"),
        report.commits_between,
        report.commits_without_cell_record,
        report.first_bad_commit_has_mixed_outcomes,
    );
    if report.first_bad_commit_has_mixed_outcomes {
        println!("FLAKE-SIGNAL: the same first-bad SHA has both PASS and FAIL records for this cell; the failure is real evidence but not a deterministic commit regression");
    }
    for refusal in &report.tree_match_refusals {
        println!("TREE-MATCH-REFUSED {refusal}");
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

/// COULD-NOT-DETERMINE for `first-bad`, distinct from every other code this
/// subcommand can return. It MUST NOT collide with:
///   0  FOUND            -- a localized PASS->FAIL transition
///   3  FAILED           -- a failure exists but no earlier PASS is retained
///   4  NOT-VALIDATED    -- no retained record, or no transition in the records
/// 75 is `EX_TEMPFAIL`, the same code `ci-hub/validate/start_unit.py` already
/// uses for its could-not-determine state, so one meaning has one number across
/// the two tools. It is deliberately NOT a failure code: an unlocalized window
/// is missing measurement, and collapsing it into FAILED is the same loss of
/// the third state in the opposite direction.
const FIRST_BAD_INDETERMINATE: i32 = 75;

fn run_first_bad(root: &Path, args: FirstBadArgs) -> Result<i32, CiHubError> {
    let repo = history_repo_path(root, &args.query.repo_dir);
    let branch_ref = format!("origin/{}", args.query.branch);
    if !args.query.no_fetch {
        fetch_history_branch(&repo, &args.query.branch)?;
    }
    let (commits, commit_trees) = branch_history_with_trees(&repo, &branch_ref)?;
    let ledger = ledger_path(root);
    let mut rows = load_ledger_rows(&ledger)?;
    rows.retain(|row| row_matches_validation_repo(row, CANONICAL_VALIDATE_REPO));
    retain_cell_evidence(root, &mut rows)?;
    let outcome = HistoryQueryEngine::new_for_first_bad(commits, rows).first_bad_with_trees(
        &args.cell_or_gate,
        &args.query.branch,
        &branch_ref,
        &commit_trees,
    );
    match outcome {
        FirstBadOutcome::Found(mut report) => {
            report.files_touched = files_touched(&repo, &report.first_bad.sha)?;
            report.plausibility =
                assess_diff_plausibility(&report.files_touched, report.source_node.as_deref());
            print_first_bad(&report, args.query.json);
            Ok(0)
        }
        FirstBadOutcome::Indeterminate {
            mut report,
            bisect_probe,
        } => {
            // Deliberately NOT `files_touched(first_bad.sha)`. Naming one
            // commit's diff inside a window this tool just said it cannot
            // localize is the exact false-confidence being removed here.
            report.plausibility = "not-assessed: window is unlocalized".into();
            if args.query.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 2,
                        "verdict": "INDETERMINATE",
                        "exit_code": FIRST_BAD_INDETERMINATE,
                        "branch": args.query.branch,
                        "query": report.query,
                        "matched_name": report.matched_name,
                        "newest_known_fail": report.first_bad,
                        "newest_known_fail_matched_by": report.first_bad_matched_by,
                        "oldest_known_pass": report.last_good,
                        "oldest_known_pass_matched_by": report.last_good_matched_by,
                        "unlocalized_window_commits": report.commits_between + 1,
                        "commits_between": report.commits_between,
                        "commits_without_cell_record": report.commits_without_cell_record,
                        "first_bad_commit_has_mixed_outcomes": report.first_bad_commit_has_mixed_outcomes,
                        "tree_match_refusals": report.tree_match_refusals,
                        "bisect_probe": bisect_probe,
                        "reason": "the two endpoints are adjacent RUNS of this cell, not adjacent commits; no commit in the window has a record for this cell, so the regression is not localized to any commit",
                        "remedy": bisect_probe.as_deref().map(|sha| format!(
                            "validate cell {} at {} to halve the window",
                            report.matched_name, sha
                        )).unwrap_or_else(|| format!(
                            "validate cell {} at the commits inside the window",
                            report.matched_name
                        )),
                    })
                );
                return Ok(FIRST_BAD_INDETERMINATE);
            }
            println!(
                "FIRST-BAD INDETERMINATE cell={} -- a PASS-to-FAIL transition exists but is NOT localized to a commit",
                report.matched_name
            );
            println!("BRANCH {} ref={}", report.branch, report.branch_ref);
            println!(
                "NEWEST-KNOWN-FAIL {} matched-by={} observed={}",
                report.first_bad.sha,
                report.first_bad_matched_by,
                report.first_bad.finished_at.as_deref().unwrap_or("unknown"),
            );
            println!(
                "OLDEST-KNOWN-PASS {} matched-by={} observed={}",
                report.last_good.sha,
                report.last_good_matched_by,
                report.last_good.finished_at.as_deref().unwrap_or("unknown"),
            );
            println!(
                "UNLOCALIZED-WINDOW {} commits ({} between the endpoints, {} of them with no record for this cell)",
                report.commits_between + 1,
                report.commits_between,
                report.commits_without_cell_record,
            );
            println!(
                "WHY: those two endpoints are adjacent RUNS of this cell, not adjacent commits. Any commit in the window can carry the regression; naming the newer endpoint would state a run-level fact under this cell's name."
            );
            match bisect_probe.as_deref() {
                Some(sha) => println!(
                    "REMEDY: validate cell {} at {} to halve the window",
                    report.matched_name, sha
                ),
                None => println!(
                    "REMEDY: validate cell {} at the commits inside the window",
                    report.matched_name
                ),
            }
            if report.first_bad_commit_has_mixed_outcomes {
                println!(
                    "FLAKE-SIGNAL: the newest-known-fail SHA also has a PASS record for this cell"
                );
            }
            for refusal in &report.tree_match_refusals {
                println!("TREE-MATCH-REFUSED {refusal}");
            }
            Ok(FIRST_BAD_INDETERMINATE)
        }
        FirstBadOutcome::FailureWithoutKnownGood {
            query,
            matched_name,
            failure,
            matched_by,
            tree_match_refusals,
        } => {
            if args.query.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version": 2, "verdict": "FAILED", "exit_code": 3, "branch": args.query.branch, "query": query, "matched_name": matched_name, "failure": failure, "matched_by": matched_by, "tree_match_refusals": tree_match_refusals, "reason": "failure exists but no earlier PASS is retained"})
                );
            } else {
                println!("FIRST-BAD FAILED cell={matched_name} sha={} matched-by={matched_by} -- failure exists but no earlier PASS is retained", failure.sha);
                for refusal in &tree_match_refusals {
                    println!("TREE-MATCH-REFUSED {refusal}");
                }
            }
            Ok(3)
        }
        FirstBadOutcome::NoEvidence {
            query,
            available_names,
            tree_match_refusals,
        } => {
            if args.query.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version": 2, "verdict": "NOT-VALIDATED", "exit_code": 4, "branch": args.query.branch, "query": query, "suggestions": available_names, "tree_match_refusals": tree_match_refusals, "reason": "no retained cell/gate record"})
                );
            } else {
                println!("FIRST-BAD NOT-VALIDATED cell={query} -- no retained cell/gate record; absence is not PASS");
                if !available_names.is_empty() {
                    println!("SUGGESTIONS {}", available_names.join(" | "));
                }
                for refusal in &tree_match_refusals {
                    println!("TREE-MATCH-REFUSED {refusal}");
                }
            }
            Ok(4)
        }
        FirstBadOutcome::NoTransition {
            query,
            matched_name,
            observations,
            tree_match_refusals,
        } => {
            if args.query.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version": 2, "verdict": "NOT-VALIDATED", "exit_code": 4, "branch": args.query.branch, "query": query, "matched_name": matched_name, "observations": observations, "tree_match_refusals": tree_match_refusals, "reason": "no retained PASS-to-FAIL transition"})
                );
            } else {
                println!("FIRST-BAD NOT-VALIDATED cell={matched_name} observations={observations} -- no retained PASS-to-FAIL transition");
                for refusal in &tree_match_refusals {
                    println!("TREE-MATCH-REFUSED {refusal}");
                }
            }
            Ok(4)
        }
    }
}

/// What a freshly fetched remote ledger knows about a commit that THIS checkout
/// cannot read.
///
/// WHY THIS EXISTS. `ledger/` is a TRACKED directory, so a checkout only learns
/// of a run when it PULLS. The spool's retention deliberately protects only the
/// PRODUCING root -- `publisher.py::reap_retained` keeps a batch until that
/// root's own shards carry it -- so a run produced by any OTHER parent root is
/// invisible here until this one pulls, and no local retention can ever cover
/// it. Measured 2026-08-11: hermit `647c7af5` was validated (full profile, FAIL,
/// 959 tests, 49 nodes, retained log) and published to `origin/main`, while this
/// reader printed `0 non-qualifying record(s)` -- byte-identical to a commit
/// nobody ever tested. "No record exists" and "I have not pulled" are different
/// facts about different subjects and must never print the same.
#[derive(Debug)]
enum LedgerFreshness {
    /// The remote ledger carries records for this commit that this checkout
    /// cannot see. The local absence is a STALENESS fact, not a verdict.
    UpstreamHasRecord { records: usize, git_ref: String },
    /// The remote ledger has no record either: absence is genuine.
    UpstreamAgrees { git_ref: String },
    /// Freshness could not be established. NEVER collapse this into
    /// `UpstreamAgrees`: failing to prove staleness is not proving currency,
    /// and hardening an unverifiable probe into an absence claim would rebuild
    /// the exact defect this type exists to remove.
    Unknown { reason: String },
    /// The record for this commit IS HERE and could not be PARSED. Nothing
    /// about the checkout's freshness is implied and pulling cannot help.
    ///
    /// This variant exists because the tool used to answer LEDGER-LAGGING in
    /// exactly this situation and tell the operator to `git pull` -- a remedy
    /// that can never terminate, because the upstream copy carries the same
    /// unreadable bytes. Measured 2026-08-24 on commits 99e378524b9a and
    /// 88d4d61bfa9b: both had a `result: pass` record present locally AND in
    /// origin/main, both unreadable in both places. "Absent here, present
    /// upstream" and "present here, unreadable" are different facts about
    /// different subjects and must never print the same, which is the same
    /// principle the rest of this enum already encodes.
    LocalRecordUnreadable { records: usize },
}

/// Count records for `sha` in `git_ref`'s tracked ledger shards.
///
/// Parses each row instead of substring-matching the 40-hex sha: `base_sha` (and
/// other commit-shaped fields) can equal this commit on a record that belongs to
/// a DIFFERENT commit, so a substring probe would manufacture a false lag report
/// and tell an agent to pull for a record that was never there.
fn remote_ledger_records_for(
    root: &Path,
    git_ref: &str,
    sha: &str,
    repo: &str,
) -> Result<usize, String> {
    let listing = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "-r", "--name-only", git_ref, "ledger/"])
        .bounded_output()
        .map_err(|source| format!("git ls-tree {git_ref} failed to launch: {source}"))?;
    if !listing.status.success() {
        return Err(format!(
            "git ls-tree {git_ref} exited {}",
            listing.status.code().unwrap_or(-1)
        ));
    }
    let mut records = 0usize;
    let team = if repo == CANONICAL_REVERIE_REPO {
        "reverie"
    } else {
        "hermit"
    };
    for shard in String::from_utf8_lossy(&listing.stdout).lines() {
        if !shard.ends_with(".jsonl") || !shard.starts_with(&format!("ledger/{team}/")) {
            continue;
        }
        let blob = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["show", &format!("{git_ref}:{shard}")])
            .bounded_output()
            .map_err(|source| format!("git show {git_ref}:{shard} failed to launch: {source}"))?;
        if !blob.status.success() {
            return Err(format!(
                "git show {git_ref}:{shard} exited {}: {}",
                blob.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&blob.stderr).trim()
            ));
        }
        let path = Path::new(shard);
        let shard_host = path
            .parent()
            .and_then(Path::file_name)
            .and_then(OsStr::to_str)
            .ok_or_else(|| format!("remote ledger shard has no host component: {shard}"))?;
        let shard_month = path
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or_else(|| format!("remote ledger shard has no month component: {shard}"))?;
        for (index, line) in String::from_utf8_lossy(&blob.stdout).lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let event = LedgerEvent::parse(line.as_bytes())
                .and_then(|event| {
                    event.validate()?;
                    Ok(event)
                })
                .map_err(|error| format!("{git_ref}:{shard}:{}: {error}", index + 1))?;
            if event.team != team {
                return Err(format!(
                    "{git_ref}:{shard}:{}: event team {:?} differs from shard team {team:?}",
                    index + 1,
                    event.team
                ));
            }
            if event.host != shard_host {
                return Err(format!(
                    "{git_ref}:{shard}:{}: event host {:?} differs from shard host {shard_host:?}",
                    index + 1,
                    event.host
                ));
            }
            if event.emitted_at.get(..7) != Some(shard_month) {
                return Err(format!(
                    "{git_ref}:{shard}:{}: event month differs from shard month {shard_month:?}",
                    index + 1
                ));
            }
            let commit = event
                .legacy_row
                .as_ref()
                .and_then(|row| row.get("commit"))
                .and_then(serde_json::Value::as_str)
                .or(event.commit.as_deref());
            if commit == Some(sha) {
                records += 1;
            }
        }
    }
    Ok(records)
}

/// Probe remote ledger freshness. Runs ONLY on the pure-absence path, so the
/// network cost is paid exactly when this reader is about to assert that
/// nothing exists -- never on a path that already has evidence.
fn probe_ledger_freshness(root: &Path, sha: &str, repo: &str) -> LedgerFreshness {
    let git_ref =
        std::env::var("CI_HUB_LEDGER_LAG_REF").unwrap_or_else(|_| "origin/main".to_string());
    // Seam for offline and hermetic tests: a planted local ref can stand in for
    // the remote so both brackets run without network.
    if std::env::var("CI_HUB_LEDGER_LAG_FETCH").as_deref() != Ok("0") {
        if let Err(error) = fetch_history_branch(root, "main") {
            return LedgerFreshness::Unknown {
                reason: format!("could not fetch origin/main: {error}"),
            };
        }
    }
    match remote_ledger_records_for(root, &git_ref, sha, repo) {
        Ok(0) => LedgerFreshness::UpstreamAgrees { git_ref },
        Ok(records) => LedgerFreshness::UpstreamHasRecord { records, git_ref },
        Err(reason) => LedgerFreshness::Unknown { reason },
    }
}

fn recent_row_verdict(row: &HistoryRow, repo: &str) -> &'static str {
    let Some(sha) = row.commit.as_deref() else {
        return "NO-COMMIT";
    };
    let qualifying = if repo == CANONICAL_VALIDATE_REPO {
        qualify_canonical_receipt(row, sha).is_some()
    } else {
        qualify_reverie_receipt(row, sha).is_some()
    };
    if qualifying {
        return "VALIDATED";
    }
    match validate_status::failure_disposition(row, sha) {
        validate_status::FailureDisposition::Failed => "FAILED",
        validate_status::FailureDisposition::NeedsRerun => "NEEDS-RERUN",
        validate_status::FailureDisposition::Truncated => "TRUNCATED",
        validate_status::FailureDisposition::NoResult => "NO-RESULT",
        validate_status::FailureDisposition::NotFailure => "NOT-VALIDATED",
    }
}

fn recent_row_summary(row: &HistoryRow, verdict: &str) -> String {
    // ⚠️ AN ABORTED NODE IS A CONSEQUENCE, A FAILED NODE IS A CAUSE, AND THIS
    // SUMMARY USED TO MERGE THEM. Both carry `result: "fail"`, so filtering on
    // `result` alone put ten eager-exit victims and the one real failure into
    // one list, then printed the FIRST TWO IN ARRAY ORDER. On main's
    // dee8cf49ce6a that printed "failed: lint.clippy, test.hermit_integration
    // (+9 more)" while the node that actually failed --
    // `e2e.manifest_applications`, the only one with `aborted: false` and
    // `reason: "exit 1"` -- was not among the names shown at all.
    //
    // The cost was not cosmetic: those names were quoted to the owner as main's
    // failure list, a P0 was filed titled after them, and an agent was sent to
    // investigate a build script on the strength of the clippy name, which was
    // a downstream abort. A summary that names consequences and hides the cause
    // sends every reader to the wrong place.
    // Reuses `validate_status::gate_is_red` -- the predicate this codebase
    // already uses for "failed on its own account" -- rather than a second copy
    // that could drift from it. The `aborted` flag lives in the row's flattened
    // `extra`, and moving it to a typed field would silently blind every
    // existing reader of that map; I tried it and broke five tests.
    let is_unsuccessful = |gate: &&records::GateHistoryRow| {
        matches!(gate.result.as_deref(), Some("fail" | "failed" | "timeout"))
            || matches!(gate.kind.as_deref(), Some("fail" | "failed" | "timeout"))
    };
    let (failed, aborted): (Vec<_>, Vec<_>) = row
        .gates
        .iter()
        .filter(is_unsuccessful)
        .partition(|gate| validate_status::gate_is_red(gate));
    let aborted_note = if aborted.is_empty() {
        String::new()
    } else {
        format!("; {} aborted", aborted.len())
    };
    if !failed.is_empty() {
        let shown = failed
            .iter()
            .take(2)
            .map(|gate| gate.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return if failed.len() > 2 {
            format!("failed: {shown} (+{} more){aborted_note}", failed.len() - 2)
        } else {
            format!("failed: {shown}{aborted_note}")
        };
    }
    // Every unsuccessful node was an abort and none of them is the cause. Say
    // that, rather than promoting an arbitrary victim to "failed" -- which is
    // exactly the wrong answer this function used to give.
    if !aborted.is_empty() {
        let shown = aborted
            .iter()
            .take(2)
            .map(|gate| gate.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let more = if aborted.len() > 2 {
            format!(" (+{} more)", aborted.len() - 2)
        } else {
            String::new()
        };
        return format!(
            "no failing node recorded; {} aborted: {shown}{more}",
            aborted.len()
        );
    }
    match verdict {
        "VALIDATED" => "passed".into(),
        "FAILED" => "full run failed".into(),
        "NEEDS-RERUN" => "failure needs an uncontended confirmation".into(),
        "TRUNCATED" => "run ended before all planned work completed".into(),
        "NO-RESULT" => "validation did not exercise the product".into(),
        "NO-COMMIT" => "receipt has no commit".into(),
        _ if row.concurrent_validates.is_some_and(|count| count > 0) => format!(
            "ran with {} other validation(s)",
            row.concurrent_validates.unwrap_or(0)
        ),
        _ if row.extra.get("admission").and_then(|value| value.as_str())
            != Some("ci-hub-validate-lock") =>
        {
            "not launched through ci-hub validation admission".into()
        }
        _ if row.tree_dirty == Some(true) => "dirty tree".into(),
        _ if row.commit_anchored != Some(true) => "not anchored to a commit".into(),
        _ if row.profile.as_deref() != Some("full")
            || row.selection_mode.as_deref() != Some("full") =>
        {
            format!(
                "profile={} selection={}",
                row.profile.as_deref().unwrap_or("?"),
                row.selection_mode.as_deref().unwrap_or("?")
            )
        }
        _ => "receipt is incomplete".into(),
    }
}

fn short_wall(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds else {
        return "-".into();
    };
    let seconds = seconds.max(0.0).round() as u64;
    if seconds >= 3600 {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn compact_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        if minutes == 0 {
            format!("{hours}hr{}", if hours == 1 { "" } else { "s" })
        } else {
            format!("{hours}h{minutes}m")
        }
    } else {
        let days = seconds / 86_400;
        let hours = (seconds % 86_400) / 3600;
        if hours == 0 {
            format!("{days}d")
        } else {
            format!("{days}d{hours}h")
        }
    }
}

fn format_validate_timestamp(
    value: &str,
    times: ValidateStatusTimes,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(value) else {
        return value.to_string();
    };
    match times {
        ValidateStatusTimes::Relative => {
            let age = now.signed_duration_since(timestamp.with_timezone(&chrono::Utc));
            let seconds = age.num_seconds();
            if seconds >= 0 {
                format!("{} ago", compact_age(seconds as u64))
            } else {
                format!("in {}", compact_age(seconds.unsigned_abs()))
            }
        }
        ValidateStatusTimes::Local => timestamp
            .with_timezone(&chrono::Local)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ValidateStatusTimes::Utc => timestamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    }
}

fn validate_status_time_width(times: ValidateStatusTimes) -> usize {
    match times {
        ValidateStatusTimes::Local => 25,
        ValidateStatusTimes::Relative | ValidateStatusTimes::Utc => 20,
    }
}

/// COMMITS SINCE GREEN, for `validate-status`' standard output.
///
/// ⚠️ WHY THIS LIVES IN THE STANDARD READER AND NOT ONLY IN `newest-green`.
/// `validate-status` is the union reader every agent and script already calls,
/// and until now it said NOT-VALIDATED and stopped. That answer is true and
/// carries no information: it reads identically when the last full green was one
/// commit back and when it was three hundred. Measured 2026-08-25 on hermit
/// `main`, the honest answer was 281 — a number nobody could see because it was
/// only ever printed by a subcommand nobody runs. The point is that the number
/// arrives without the reader having to know the concept exists.
///
/// ⚠️ DEPTH IS FIRST-PARENT DISTANCE, NEVER RECEIPT AGE. `commits_after_green`
/// is `commits[..index].len()` over the first-parent walk, so it is a distance by
/// construction. Ordering by `emitted_at` would be wrong in exactly the case this
/// exists for: a full validate takes ~25 minutes against a churning `main`, so a
/// receipt emitted LATER routinely names an OLDER commit.
///
/// ⚠️ AND AN ABSENCE MUST STATE ITS WINDOW. `NoneInWindow` always prints the
/// window it searched, because "no green within N" and "no green ever" render
/// identically once the bound is dropped — and that collapse is how a bounded
/// search gets read as a permanent verdict. `Unverifiable` and `Unavailable` are
/// kept distinct from both for the same reason: neither is evidence of absence,
/// and neither may render as a green.
///
/// COST. Offline by construction: no fetch, no network, local refs and the local
/// ledger only. It reuses `newest-green`'s cache, so the common case is a read.
/// A standard reader that paid for a fetch would be a standard reader people
/// stop calling.
enum GreenDepth {
    Found {
        depth: usize,
        sha: String,
        window: usize,
        failed_on_record: usize,
        needs_rerun: usize,
        truncated: usize,
        no_result: usize,
        not_validated: usize,
        ledger_unreadable: usize,
        no_record: usize,
    },
    NoneInWindow {
        window: usize,
        oldest: String,
        recorded: usize,
    },
    Unverifiable {
        visible_depth: usize,
        required_depth: String,
    },
    /// The query could not run at all. NOT an absence, and never a green.
    Unavailable { why: String },
}

impl GreenDepth {
    fn line(&self) -> String {
        match self {
            GreenDepth::Found {
                depth,
                sha,
                window,
                failed_on_record,
                needs_rerun,
                truncated,
                no_result,
                not_validated,
                ledger_unreadable,
                no_record,
            } => format!(
                "HERMIT COMMITS-SINCE-GREEN {depth} (last full green {} at first-parent depth {depth}; \
                 FAILED={failed_on_record} NEEDS-RERUN={needs_rerun} TRUNCATED={truncated} \
                 NO-RESULT={no_result} NOT-VALIDATED={not_validated} \
                 LEDGER-UNREADABLE={ledger_unreadable} NO-RECORD={no_record}; \
                 searched {window} commits)",
                sha.get(..12).unwrap_or(sha)
            ),
            GreenDepth::NoneInWindow {
                window,
                oldest,
                recorded,
            } => format!(
                "HERMIT COMMITS-SINCE-GREEN NONE-WITHIN-{window} -- no full green in the {window} \
                 first-parent commits back to {} ({recorded} carried any record). This is a \
                 BOUNDED search, not a claim that no green exists; widen it with \
                 `ci-hub newest-green`.",
                oldest.get(..12).unwrap_or(oldest)
            ),
            GreenDepth::Unverifiable {
                visible_depth,
                required_depth,
            } => format!(
                "HERMIT COMMITS-SINCE-GREEN UNVERIFIABLE-SHALLOW-HISTORY -- only {visible_depth} \
                 first-parent commits visible, {required_depth} required. Deepen the checkout; \
                 this is not an absence."
            ),
            GreenDepth::Unavailable { why } => format!(
                "HERMIT COMMITS-SINCE-GREEN UNAVAILABLE -- {why}. Not measured, and NOT evidence that \
                 main is ungreen."
            ),
        }
    }

    fn json(&self) -> serde_json::Value {
        match self {
            GreenDepth::Found {
                depth,
                sha,
                window,
                failed_on_record,
                needs_rerun,
                truncated,
                no_result,
                not_validated,
                ledger_unreadable,
                no_record,
            } => serde_json::json!({
                "state": "found", "depth": depth, "green_sha": sha,
                "searched_commits": window,
                "commits_failed_on_record": failed_on_record,
                "commits_needing_rerun": needs_rerun,
                "commits_truncated": truncated,
                "commits_with_no_result": no_result,
                "commits_not_validated": not_validated,
                "commits_with_unreadable_record": ledger_unreadable,
                "commits_with_records": failed_on_record
                    + needs_rerun
                    + truncated
                    + no_result
                    + not_validated,
                "commits_without_any_record": no_record,
                // Compatibility: this key historically meant "no readable
                // row", so an unreadable physical row remains part of it.
                "commits_without_record": no_record + ledger_unreadable,
                "basis": "first-parent distance",
            }),
            GreenDepth::NoneInWindow {
                window,
                oldest,
                recorded,
            } => serde_json::json!({
                "state": "none-within-window", "depth": serde_json::Value::Null,
                "searched_commits": window, "oldest_searched": oldest,
                "commits_with_records": recorded,
                "note": "bounded search; not a claim that no green exists",
            }),
            GreenDepth::Unverifiable {
                visible_depth,
                required_depth,
            } => serde_json::json!({
                "state": "unverifiable-shallow-history", "depth": serde_json::Value::Null,
                "visible_first_parent_depth": visible_depth,
                "required_history_depth": required_depth,
            }),
            GreenDepth::Unavailable { why } => serde_json::json!({
                "state": "unavailable", "depth": serde_json::Value::Null, "reason": why,
            }),
        }
    }
}

/// Compute the standard-output green depth. Never returns `Err`: a reader that
/// dies because a side metric could not be computed is worse than one that says
/// so, and every failure path lands in `Unavailable` with its reason.
fn standard_green_depth(root: &Path, branch: &str) -> GreenDepth {
    let repo = history_repo_path(root, Path::new("hermit"));
    if !repo.join(".git").exists() {
        return GreenDepth::Unavailable {
            why: format!("no hermit checkout at {}", repo.display()),
        };
    }
    let branch_ref = format!("origin/{branch}");
    // Deliberately no fetch: see the cost note above.
    let floor = match query_effective_gate_floor(root, &repo, branch) {
        Ok(GateFloorResolution::Effective(floor)) => floor,
        Ok(GateFloorResolution::UnverifiableShallow {
            visible_depth,
            required_depth,
            ..
        }) => {
            return GreenDepth::Unverifiable {
                visible_depth,
                required_depth,
            }
        }
        Err(error) => {
            return GreenDepth::Unavailable {
                why: format!("gate floor: {error}"),
            }
        }
    };
    let history = match branch_history(&repo, &branch_ref) {
        Ok(history) => history,
        Err(error) => {
            return GreenDepth::Unavailable {
                why: format!("{branch_ref}: {error}"),
            }
        }
    };
    let commits = match history_at_or_after_gate_floor(history, &branch_ref, &floor.sha) {
        Ok(commits) => commits,
        Err(error) => {
            return GreenDepth::Unavailable {
                why: format!("gate floor window: {error}"),
            }
        }
    };
    let ledger = ledger_path(root);
    let (rows, parse_failures) = match load_ledger_rows_reporting(&ledger) {
        Ok(parsed) => parsed,
        Err(error) => {
            return GreenDepth::Unavailable {
                why: format!("ledger: {error}"),
            }
        }
    };
    let rows = rows
        .into_iter()
        .filter(|row| row_matches_validation_repo(row, CANONICAL_VALIDATE_REPO))
        .collect::<Vec<_>>();
    let unreadable_commits = parse_failures
        .into_iter()
        .filter(|failure| parse_failure_matches_validation_repo(failure, CANONICAL_VALIDATE_REPO))
        .filter_map(|failure| failure.commit)
        .collect();
    // Keep this on the same unmodified parsed rows as exact-SHA
    // validate-status. `retain_cell_evidence` is for first-bad cell detail.
    let floor_policy = format!("{GATE_FLOOR_POLICY}:{}", floor.kind);
    match HistoryQueryEngine::new(commits, rows)
        .with_unreadable_commits(unreadable_commits)
        .newest_green(branch, &branch_ref, &floor_policy, &floor.sha)
    {
        NewestGreenOutcome::Found(report) => GreenDepth::Found {
            depth: report.commits_after_green,
            sha: report.green.sha.clone(),
            window: report.branch_commits_in_range,
            failed_on_record: report.commits_failed_on_record,
            needs_rerun: report.commits_needing_rerun,
            truncated: report.commits_truncated,
            no_result: report.commits_with_no_result,
            not_validated: report.commits_not_validated,
            ledger_unreadable: report.commits_with_unreadable_record,
            no_record: report.commits_without_any_record,
        },
        NewestGreenOutcome::FailedOnly {
            commits_in_range,
            recorded,
            ..
        }
        | NewestGreenOutcome::NoEvidence {
            commits_in_range,
            recorded,
            ..
        } => GreenDepth::NoneInWindow {
            window: commits_in_range,
            oldest: floor.sha.clone(),
            recorded,
        },
    }
}

fn parse_passed_tests(line: &str) -> Option<i64> {
    let result = line.split_once("test result: ")?.1;
    let before_passed = result.split_once(" passed")?.0;
    before_passed
        .split_whitespace()
        .next_back()?
        .parse::<i64>()
        .ok()
        .filter(|count| *count >= 0)
}

/// Recover an observed lower bound for historical rows whose producer retained
/// test summaries only in the durable log. Compact logs can omit summaries and
/// retry logs can retain more attempts than the terminal receipt, so this value
/// must never be presented as an exact count. No parseable summary is UNKNOWN,
/// never a fabricated zero.
fn observed_passed_tests_from_log(root: &Path, row: &HistoryRow) -> Option<i64> {
    let raw = row.log_file.as_deref()?;
    let path = if let Some(relative) = raw.strip_prefix("{{WORKSPACE_ROOT}}/") {
        root.join(relative)
    } else {
        history_repo_path(root, Path::new(raw))
    };
    let file = std::fs::File::open(path).ok()?;
    let mut passed = None;
    for line in BufReader::new(file).lines() {
        let line = line.ok()?;
        let Some(count) = parse_passed_tests(&line) else {
            continue;
        };
        passed = Some(passed.unwrap_or(0_i64).checked_add(count)?);
    }
    let passed = passed?;
    let total = row.executed_tests?.checked_add(row.filtered_tests?)?;
    (total >= 0 && passed <= total).then_some(passed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TestCounts {
    total: Option<i64>,
    passed: Option<i64>,
    passed_at_least: Option<i64>,
}

fn recent_row_test_counts(root: &Path, row: &HistoryRow) -> TestCounts {
    let total = match (row.executed_tests, row.filtered_tests) {
        (Some(executed), Some(filtered)) if executed >= 0 && filtered >= 0 => {
            executed.checked_add(filtered)
        }
        _ => None,
    };
    // `executed_tests` includes failed and ignored tests, so it cannot be
    // relabelled as passed. Prefer a producer-carried count; old rows fall back
    // to the final per-node summaries in their retained log.
    let passed = row
        .extra
        .get("passed_tests")
        .and_then(serde_json::Value::as_i64)
        .filter(|count| *count >= 0)
        .filter(|count| {
            row.executed_tests
                .is_some_and(|executed| executed >= 0 && *count <= executed)
        });
    let passed_at_least = passed
        .is_none()
        .then(|| observed_passed_tests_from_log(root, row))
        .flatten();
    TestCounts {
        total,
        passed,
        passed_at_least,
    }
}

type ValidateRunProcessIdentity = validate_run_handle::ProcessIdentity;

#[derive(Clone, Debug, Deserialize)]
struct ValidateRunRecord {
    schema_version: u32,
    unit: String,
    target: String,
    repo: String,
    #[serde(default)]
    agent: Option<String>,
    started_at: String,
    #[serde(default)]
    host: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ValidateStatusEntry {
    status: String,
    in_progress: bool,
    sha: Option<String>,
    branch: Option<String>,
    git_depth: Option<i64>,
    started_at: Option<String>,
    finished_at: Option<String>,
    elapsed_seconds: Option<f64>,
    tests_total: Option<i64>,
    tests_passed: Option<i64>,
    tests_passed_at_least: Option<i64>,
    host: Option<String>,
    slot: Option<String>,
    summary: String,
}

fn process_state_and_start_ticks(stat: &str) -> Option<(char, u64)> {
    // Everything after the final `) ` begins at proc stat field 3; starttime is
    // field 22, therefore index 19 in this suffix. Splitting at the final close
    // handles spaces and closing parentheses in the process name.
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let state = fields.first()?.chars().next()?;
    let start_ticks = fields.get(19)?.parse().ok()?;
    Some((state, start_ticks))
}

fn cgroup_contains_exact_unit(cgroup: &str, unit: &str) -> bool {
    cgroup.lines().any(|line| {
        line.splitn(3, ':')
            .nth(2)
            .is_some_and(|path| path.split('/').any(|component| component == unit))
    })
}

fn validate_run_process_is_live(
    proc_root: &Path,
    boot_id_path: &Path,
    unit: &str,
    identity: &ValidateRunProcessIdentity,
) -> Result<bool, String> {
    let current_boot = std::fs::read_to_string(boot_id_path)
        .map_err(|error| format!("cannot read {}: {error}", boot_id_path.display()))?;
    if current_boot.trim() != identity.boot_id {
        return Ok(false);
    }
    let process = proc_root.join(identity.pid.to_string());
    let stat_path = process.join("stat");
    let stat = match std::fs::read_to_string(&stat_path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("cannot read {}: {error}", stat_path.display())),
    };
    let Some((state, start_ticks)) = process_state_and_start_ticks(&stat) else {
        return Ok(false);
    };
    if matches!(state, 'Z' | 'X') || start_ticks != identity.start_ticks {
        return Ok(false);
    }
    let cgroup_path = process.join("cgroup");
    let cgroup = match std::fs::read_to_string(&cgroup_path) {
        Ok(cgroup) => cgroup,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("cannot read {}: {error}", cgroup_path.display())),
    };
    Ok(cgroup_contains_exact_unit(&cgroup, unit))
}

fn local_host() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty())
}

fn elapsed_since(started_at: &str, now: chrono::DateTime<chrono::Utc>) -> Option<f64> {
    let started = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    Some(
        now.signed_duration_since(started.with_timezone(&chrono::Utc))
            .num_milliseconds()
            .max(0) as f64
            / 1000.0,
    )
}

fn live_validate_runs_from(
    root: &Path,
    repo: &str,
    proc_root: &Path,
    boot_id_path: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<ValidateStatusEntry>, CiHubError> {
    let directory = root.join("ignored/validate/runs");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(CiHubError::ValidateStatus(format!(
                "cannot read validation run handles in {}: {error}",
                directory.display()
            )))
        }
    };
    let mut paths = entries
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|error| {
                CiHubError::ValidateStatus(format!(
                    "cannot enumerate validation run handles in {}: {error}",
                    directory.display()
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort();

    let records = paths
        .into_iter()
        .map(|path| {
            let content = std::fs::read_to_string(&path).map_err(|error| {
                CiHubError::ValidateStatus(format!(
                    "cannot read validation run handle {}: {error}",
                    path.display()
                ))
            })?;
            let value = serde_json::from_str(&content).map_err(|error| {
                CiHubError::ValidateStatus(format!(
                    "validation run handle {} is not JSON: {error}",
                    path.display()
                ))
            })?;
            Ok((path, value))
        })
        .collect::<Result<Vec<_>, CiHubError>>()?;
    let current_paths = records
        .iter()
        .filter(|(_, value)| validate_run_handle::is_current(value))
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let mut current = if current_paths.is_empty() {
        BTreeMap::new()
    } else {
        validate_run_handle::inspect_current_records(root, &current_paths)
            .map_err(CiHubError::ValidateStatus)?
            .into_iter()
            .map(|inspected| (inspected.path, inspected.record))
            .collect::<BTreeMap<_, _>>()
    };

    let mut live = Vec::new();
    for (path, value) in records {
        if validate_run_handle::is_current(&value) {
            let record = current.remove(&path).ok_or_else(|| {
                CiHubError::ValidateStatus(format!(
                    "current validation run handle {} was not returned by its shared authority",
                    path.display()
                ))
            })?;
            if !record.counts_as_validation || !record.lock_admissible {
                continue;
            }
            let Some(unit) = record.unit.as_deref() else {
                return Err(CiHubError::ValidateStatus(format!(
                    "live validation run handle {} has no unit after shared validation",
                    path.display()
                )));
            };
            let Some(identity) = record.process_identity.as_ref() else {
                continue;
            };
            if !validate_run_process_is_live(proc_root, boot_id_path, unit, identity)
                .map_err(CiHubError::ValidateStatus)?
            {
                continue;
            }
            let Some(record_repo) = record.repo.as_deref() else {
                return Err(CiHubError::ValidateStatus(format!(
                    "live validation run handle {} has no repo after shared validation",
                    path.display()
                )));
            };
            if canonical_validation_repo(record_repo) != Some(repo) {
                continue;
            }
            let expected_name = format!("{}.json", unit.trim_end_matches(".service"));
            if path.file_name().and_then(OsStr::to_str) != Some(expected_name.as_str()) {
                return Err(CiHubError::ValidateStatus(format!(
                    "live validation run handle {} does not bind its unit {}",
                    path.display(),
                    unit
                )));
            }
            let target = record.target.ok_or_else(|| {
                CiHubError::ValidateStatus(format!(
                    "live validation run handle {} has no target after shared validation",
                    path.display()
                ))
            })?;
            let started_at = record.started_at.ok_or_else(|| {
                CiHubError::ValidateStatus(format!(
                    "live validation run handle {} has no started_at after shared validation",
                    path.display()
                ))
            })?;
            let summary = format!(
                "agent={} unit={}",
                record.agent.as_deref().unwrap_or("?"),
                unit
            );
            live.push(ValidateStatusEntry {
                status: "IN-PROGRESS".into(),
                in_progress: true,
                sha: Some(target),
                branch: None,
                git_depth: None,
                started_at: Some(started_at.clone()),
                finished_at: None,
                elapsed_seconds: elapsed_since(&started_at, now),
                tests_total: None,
                tests_passed: None,
                tests_passed_at_least: None,
                host: record.host.or_else(local_host),
                slot: None,
                summary,
            });
            continue;
        }
        if !matches!(
            value.get("state").and_then(serde_json::Value::as_str),
            Some("launching" | "running")
        ) {
            continue;
        }
        let Some(unit) = value.get("unit").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !unit.starts_with("validate-") || !unit.ends_with(".service") {
            continue;
        }
        let Some(identity_value) = value.get("process_identity") else {
            // `launching` exists briefly before validate-lock publishes the
            // process identity. It is not yet enough evidence to call a run
            // executing, and old interrupted launch records prove why the state
            // string alone must not be trusted.
            continue;
        };
        let identity: ValidateRunProcessIdentity =
            match serde_json::from_value(identity_value.clone()) {
                Ok(identity) => identity,
                Err(_) => continue,
            };
        if !validate_run_process_is_live(proc_root, boot_id_path, unit, &identity)
            .map_err(CiHubError::ValidateStatus)?
        {
            continue;
        }
        // Only a process-bound live record can affect the answer. Historical
        // stale handles predate fields that are required now; rejecting those
        // before checking liveness would make an old file hide a current run.
        let record: ValidateRunRecord = serde_json::from_value(value).map_err(|error| {
            CiHubError::ValidateStatus(format!(
                "live validation run handle {} is malformed: {error}",
                path.display()
            ))
        })?;
        if record.schema_version != 1 {
            return Err(CiHubError::ValidateStatus(format!(
                "live validation run handle {} has unsupported schema {}",
                path.display(),
                record.schema_version
            )));
        }
        if canonical_validation_repo(&record.repo) != Some(repo) {
            continue;
        }
        let expected_name = format!("{}.json", record.unit.trim_end_matches(".service"));
        if path.file_name().and_then(OsStr::to_str) != Some(expected_name.as_str()) {
            return Err(CiHubError::ValidateStatus(format!(
                "live validation run handle {} does not bind its unit {}",
                path.display(),
                record.unit
            )));
        }
        let summary = format!(
            "agent={} unit={}",
            record.agent.as_deref().unwrap_or("?"),
            record.unit
        );
        live.push(ValidateStatusEntry {
            status: "IN-PROGRESS".into(),
            in_progress: true,
            sha: Some(record.target),
            branch: None,
            git_depth: None,
            started_at: Some(record.started_at.clone()),
            finished_at: None,
            elapsed_seconds: elapsed_since(&record.started_at, now),
            tests_total: None,
            tests_passed: None,
            tests_passed_at_least: None,
            host: record.host.or_else(local_host),
            slot: None,
            summary,
        });
    }
    live.sort_by(|left, right| right.started_at.cmp(&left.started_at));
    Ok(live)
}

fn live_validate_runs(
    root: &Path,
    repo: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<ValidateStatusEntry>, CiHubError> {
    live_validate_runs_from(
        root,
        repo,
        Path::new("/proc"),
        Path::new("/proc/sys/kernel/random/boot_id"),
        now,
    )
}

fn selected_validate_status_columns(
    requested: &[ValidateStatusColumn],
) -> Result<Vec<ValidateStatusColumn>, CiHubError> {
    if requested.contains(&ValidateStatusColumn::None) {
        if requested.len() != 1 {
            return Err(CiHubError::ValidateStatus(
                "--columns none cannot be combined with another column".into(),
            ));
        }
        return Ok(Vec::new());
    }
    let mut columns = Vec::new();
    for column in requested {
        if !columns.contains(column) {
            columns.push(*column);
        }
    }
    Ok(columns)
}

fn validation_repo_checkout(root: &Path, repo: &str) -> PathBuf {
    root.join(if repo == CANONICAL_REVERIE_REPO {
        "reverie"
    } else {
        "hermit"
    })
}

fn branch_for_validation_sha(root: &Path, repo: &str, sha: &str) -> Option<String> {
    let checkout = validation_repo_checkout(root, repo);
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args([
            "branch",
            "--remotes",
            "--contains",
            sha,
            "--format=%(refname:short)",
        ])
        .bounded_output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let branches = stdout
        .lines()
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .collect::<Vec<_>>();
    branches
        .iter()
        .find(|branch| **branch == "origin/main")
        .or_else(|| branches.first())
        .map(|branch| branch.strip_prefix("origin/").unwrap_or(branch).to_string())
}

fn git_depth_for_validation_sha(root: &Path, repo: &str, sha: &str) -> Option<i64> {
    let checkout = validation_repo_checkout(root, repo);
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-list", "--count", sha])
        .bounded_output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn fill_optional_validate_status_columns(
    root: &Path,
    repo: &str,
    columns: &[ValidateStatusColumn],
    entry: &mut ValidateStatusEntry,
) {
    let Some(sha) = entry.sha.as_deref() else {
        return;
    };
    if columns.contains(&ValidateStatusColumn::Branch) && entry.branch.is_none() {
        entry.branch = branch_for_validation_sha(root, repo, sha);
    }
    if columns.contains(&ValidateStatusColumn::GitDepth) && entry.git_depth.is_none() {
        entry.git_depth = git_depth_for_validation_sha(root, repo, sha);
    }
}

fn validate_status_header(columns: &[ValidateStatusColumn], times: ValidateStatusTimes) -> String {
    let mut fields = vec![format!("{:<13}", "STATUS")];
    for column in columns {
        fields.push(match column {
            ValidateStatusColumn::Sha => format!("{:<12}", "SHA"),
            ValidateStatusColumn::Branch => format!("{:<18}", "BRANCH"),
            ValidateStatusColumn::GitDepth => format!("{:>8}", "GITDEPTH"),
            ValidateStatusColumn::None => unreachable!("none is removed before rendering"),
        });
    }
    fields.push(format!(
        "{:<width$}",
        "STARTED/FINISHED",
        width = validate_status_time_width(times)
    ));
    fields.push(format!("{:>7}", "ELAPSED"));
    fields.push(format!("{:<16}", "HOST"));
    fields.push(format!("{:>11}", "TESTS-TOTAL"));
    fields.push(format!("{:>12}", "TESTS-PASSED"));
    fields.push("SUMMARY".into());
    fields.join(" ")
}

fn validate_status_line(
    entry: &ValidateStatusEntry,
    columns: &[ValidateStatusColumn],
    times: ValidateStatusTimes,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let mut fields = vec![format!("{:<13}", entry.status)];
    for column in columns {
        fields.push(match column {
            ValidateStatusColumn::Sha => {
                let sha = entry.sha.as_deref().unwrap_or("-");
                format!("{:<12}", sha.get(..12).unwrap_or(sha))
            }
            ValidateStatusColumn::Branch => {
                format!("{:<18}", entry.branch.as_deref().unwrap_or("-"))
            }
            ValidateStatusColumn::GitDepth => format!(
                "{:>8}",
                entry
                    .git_depth
                    .map(|depth| depth.to_string())
                    .unwrap_or_else(|| "-".into())
            ),
            ValidateStatusColumn::None => unreachable!("none is removed before rendering"),
        });
    }
    let when = entry
        .finished_at
        .as_deref()
        .or(entry.started_at.as_deref())
        .unwrap_or("-");
    fields.push(format!(
        "{:<width$}",
        format_validate_timestamp(when, times, now),
        width = validate_status_time_width(times)
    ));
    fields.push(format!("{:>7}", short_wall(entry.elapsed_seconds)));
    let host = entry
        .host
        .as_deref()
        .unwrap_or("-")
        .split('.')
        .next()
        .unwrap_or("-");
    fields.push(format!("{:<16}", host));
    fields.push(format!(
        "{:>11}",
        entry
            .tests_total
            .map(|count| count.to_string())
            .unwrap_or_else(|| "-".into())
    ));
    fields.push(format!(
        "{:>12}",
        match (entry.tests_passed, entry.tests_passed_at_least) {
            (Some(count), _) => count.to_string(),
            (None, Some(count)) => format!(">={count}"),
            (None, None) => "-".into(),
        }
    ));
    fields.push(entry.summary.clone());
    fields.join(" ")
}

fn finished_validate_status_entry(
    root: &Path,
    repo: &str,
    row: &HistoryRow,
) -> ValidateStatusEntry {
    let verdict = recent_row_verdict(row, repo);
    let tests = recent_row_test_counts(root, row);
    ValidateStatusEntry {
        status: verdict.into(),
        in_progress: false,
        sha: row.commit.clone(),
        branch: row
            .extra
            .get("branch")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        git_depth: row
            .extra
            .get("git_depth")
            .and_then(serde_json::Value::as_i64),
        started_at: row.started_at.clone(),
        finished_at: row.finished_at.clone(),
        elapsed_seconds: row.real_seconds,
        tests_total: tests.total,
        tests_passed: tests.passed,
        tests_passed_at_least: tests.passed_at_least,
        host: row.host.clone(),
        slot: row.slot.clone(),
        summary: recent_row_summary(row, verdict),
    }
}

fn print_recent_validate_history(
    root: &Path,
    rows: &[HistoryRow],
    repo: &str,
    ledger: &Path,
    args: &ValidateStatusArgs,
) -> Result<i32, CiHubError> {
    if args.limit == 0 {
        return Err(CiHubError::ValidateStatus(
            "--limit must be greater than zero".into(),
        ));
    }
    let columns = selected_validate_status_columns(&args.columns)?;
    let now = chrono::Utc::now();
    let mut current = if args.exclude_in_progress {
        Vec::new()
    } else {
        live_validate_runs(root, repo, now)?
    };
    let mut recent = rows
        .iter()
        .filter(|row| row.commit.is_some())
        .collect::<Vec<_>>();
    recent.sort_by(|left, right| {
        let left = left
            .finished_at
            .as_deref()
            .or(left.started_at.as_deref())
            .unwrap_or("");
        let right = right
            .finished_at
            .as_deref()
            .or(right.started_at.as_deref())
            .unwrap_or("");
        right.cmp(left)
    });
    recent.retain(|row| {
        !current.iter().any(|entry| {
            entry.sha.as_deref() == row.commit.as_deref()
                && entry.started_at.as_deref() == row.started_at.as_deref()
        })
    });
    recent.truncate(args.limit);

    let mut finished = if args.in_progress_only {
        Vec::new()
    } else {
        recent
            .iter()
            .map(|row| finished_validate_status_entry(root, repo, row))
            .collect::<Vec<_>>()
    };
    for entry in current.iter_mut().chain(finished.iter_mut()) {
        fill_optional_validate_status_columns(root, repo, &columns, entry);
    }
    let mut entries = current;
    entries.extend(finished);

    // Computed for BOTH output modes: a number that only appears under --json
    // is a number the humans reading the default output never see, which is the
    // condition this metric exists to end.
    let depth = standard_green_depth(root, "main");

    if args.json {
        let in_progress_count = entries.iter().filter(|entry| entry.in_progress).count();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 3,
                "repo": repo,
                "count": entries.len(),
                "in_progress_count": in_progress_count,
                "finished_count": entries.len() - in_progress_count,
                "limit": args.limit,
                "columns": columns.iter().map(|column| column.to_possible_value().expect("value enum").get_name().to_string()).collect::<Vec<_>>(),
                "runs": entries,
                "commits_since_green": depth.json(),
                "ledger": ledger.display().to_string(),
            }))
            .expect("serialize recent validation history")
        );
        return Ok(0);
    }

    // Printed even when there are no runs at all: "no runs" and "no green in the
    // last N commits" are different facts and the second is the actionable one.
    println!("{}", depth.line());
    if entries.is_empty() {
        if args.in_progress_only {
            println!("No in-progress validation runs found for {repo}.");
        } else {
            println!("No validation runs found for {repo}.");
        }
        return Ok(0);
    }
    println!("{}", validate_status_header(&columns, args.times));
    for entry in &entries {
        println!("{}", validate_status_line(entry, &columns, args.times, now));
    }
    Ok(0)
}

/// `ci-hub validate-status [<SHA> | --sha <SHA> | --pr <N>]` — list recent
/// local validation runs by default, or inspect one exact commit when supplied.
/// Exit 0 VALIDATED, 3 FAILED (known-bad), 4 for every
/// re-measurement state (TRUNCATED, NEEDS-RERUN, NO-RESULT, NOT-VALIDATED, or
/// LEDGER-LAGGING).
///
/// LEDGER-LAGGING deliberately KEEPS exit 4. It is a distinct OUTCOME, not a
/// distinct authorization: every existing consumer already refuses on 4, while a
/// new code could fall through an enumerated `case`/`if` and be read as success.
/// The distinction belongs in the verdict string and JSON, where a reader
/// consumes it; the exit code stays in the refusal class where it belongs.
fn run_validate_status(root: &Path, args: ValidateStatusArgs) -> Result<i32, CiHubError> {
    let path = ledger_path(root);
    // The failures are kept, not just printed: a record for THE COMMIT BEING
    // ASKED ABOUT that we could not read is the one case where answering
    // NOT-VALIDATED would be a confident wrong answer. See the
    // `LocalRecordUnreadable` arm below.
    let (rows, parse_failures) = load_ledger_rows_reporting(&path)?;
    let repo = canonical_validation_repo(&args.repo).ok_or_else(|| {
        CiHubError::ValidateStatus(format!(
            "--repo must be {CANONICAL_VALIDATE_REPO} or {CANONICAL_REVERIE_REPO}, got {}",
            args.repo
        ))
    })?;
    // Repository identity is part of the key. Filter BEFORE prefix resolution:
    // a same-SHA row from the other product must neither disambiguate nor bless
    // this query.
    let repo_rows = rows
        .iter()
        .filter(|row| row_matches_validation_repo(row, repo))
        .cloned()
        .collect::<Vec<_>>();
    // A SHA may arrive as `--sha` or positionally; clap already forbids giving
    // both, or either together with `--pr`.
    let sha_input = args.sha.as_ref().or(args.sha_positional.as_ref());
    if sha_input.is_none() && args.pr.is_none() {
        return print_recent_validate_history(root, &repo_rows, repo, &path, &args);
    }
    let sha = match (sha_input, args.pr) {
        (Some(input), None) => {
            validate_status::resolve_sha(&repo_rows, input).map_err(CiHubError::ValidateStatus)?
        }
        (None, Some(pr)) => gh_pr_head(root, &args.repo, pr)?,
        _ => unreachable!("clap rejects conflicting validate-status targets"),
    };
    let assessment = assess_canonical_receipts(root, &repo_rows, &sha, repo)
        .map_err(CiHubError::ValidateStatus)?;
    let newest = newest_canonical_receipt(&assessment.qualifying);

    // Only the PURE-ABSENCE case is confusable. If any record for this commit is
    // readable here -- qualifying or not -- the reader has evidence about the
    // commit and staleness cannot be masquerading as absence, so no probe runs
    // and no network cost is paid.
    let local_records = assessment.qualifying.len() + assessment.disqualified.len();
    // A record for this commit that is PRESENT BUT UNREADABLE is a strictly
    // better explanation than staleness, and it is cheaper: no fetch is needed
    // to know it. Check it FIRST, so the staleness probe never gets the chance
    // to blame the checkout for a schema defect.
    let unreadable_here = parse_failures
        .iter()
        .filter(|failure| {
            parse_failure_matches_validation_repo(failure, repo)
                && failure
                    .commit
                    .as_deref()
                    .is_some_and(|commit| commit == assessment.sha)
        })
        .count();
    let freshness = if matches!(assessment.verdict, validate_status::Verdict::NotValidated)
        && local_records == 0
    {
        if unreadable_here > 0 {
            Some(LedgerFreshness::LocalRecordUnreadable {
                records: unreadable_here,
            })
        } else {
            Some(probe_ledger_freshness(root, &assessment.sha, repo))
        }
    } else {
        None
    };
    let freshness_json = match &freshness {
        Some(LedgerFreshness::UpstreamHasRecord { records, git_ref }) => serde_json::json!({
            "state": "upstream-has-record",
            "ref": git_ref,
            "upstream_records": records,
            "local_absence_is_authoritative": false,
        }),
        Some(LedgerFreshness::UpstreamAgrees { git_ref }) => serde_json::json!({
            "state": "current",
            "ref": git_ref,
            "upstream_records": 0,
            "local_absence_is_authoritative": true,
        }),
        Some(LedgerFreshness::Unknown { reason }) => serde_json::json!({
            "state": "unknown",
            "reason": reason,
            "local_absence_is_authoritative": false,
        }),
        Some(LedgerFreshness::LocalRecordUnreadable { records }) => serde_json::json!({
            "state": "local-record-unreadable",
            "unreadable_local_records": records,
            "local_absence_is_authoritative": false,
            // Say it in the payload too. A machine consumer that retries after
            // a pull would loop forever, because the upstream bytes are the
            // same bytes.
            "pull_would_help": false,
        }),
        None => serde_json::Value::Null,
    };
    // A lagging read, and an unreadable local record, are each reported as
    // their own verdict so no consumer mistakes either for a statement about
    // the commit -- and so the two are never confused with each other.
    let reported_verdict = match &freshness {
        Some(LedgerFreshness::UpstreamHasRecord { .. }) => "LEDGER-LAGGING",
        Some(LedgerFreshness::LocalRecordUnreadable { .. }) => "LEDGER-UNREADABLE",
        _ => assessment.verdict.as_str(),
    };

    if args.json {
        let qualifying_receipts: Vec<_> =
            assessment.qualifying.iter().map(describe_receipt).collect();
        let report = serde_json::json!({
            "schema_version": 1,
            "repo": repo,
            "sha": assessment.sha,
            "verdict": reported_verdict,
            "exit_code": assessment.verdict.exit_code(),
            // Present only on the pure-absence path. `local_absence_is_authoritative`
            // is the field a consumer must read before treating a missing record
            // as "never tested".
            "ledger_freshness": freshness_json,
            "qualifying_count": assessment.qualifying.len(),
            "disqualified_count": assessment.disqualified.len(),
            // Genuine clean full-coverage FAILs on this same commit. Any
            // positive count prevents VALIDATED; the count explains the latch.
            "failed_record_count": assessment.failed_records,
            "withheld_nonpass_record_count": assessment.withheld_nonpass_records,
            "unresolved_failure_obligations": assessment.unresolved_failure_obligations,
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
                let passed = row
                    .row
                    .finished_at
                    .as_deref()
                    .map(|timestamp| {
                        format_validate_timestamp(timestamp, args.times, chrono::Utc::now())
                    })
                    .unwrap_or_else(|| "?".into());
                println!(
                    "# validate VALIDATED {} (passed {}, wall {}s, host {}, profile full/full) -- clean-tree commit-anchored full run",
                    assessment.sha,
                    passed,
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
                debug_assert_eq!(assessment.failed_records, 0);
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
                if assessment.unresolved_failure_obligations.is_empty() {
                    println!(
                        "# validate NEEDS-RERUN {} -- red evidence lacks complete solo execution conditions or is known-flaky/contended; rerun solo at -j 4",
                        assessment.sha,
                    );
                } else {
                    println!(
                        "# validate NEEDS-RERUN {} -- {} durable per-cell failure obligation(s) remain unresolved",
                        assessment.sha,
                        assessment.unresolved_failure_obligations.len(),
                    );
                    for obligation in &assessment.unresolved_failure_obligations {
                        println!(
                            "#   {}/{} {} {}: target has {} clean leg(s), {} failure(s); needs {} clean leg(s) after {}/{} baseline failures at {}",
                            obligation.category,
                            obligation.test,
                            obligation.mode,
                            obligation.backend,
                            obligation.observed_clean_legs_at_target,
                            obligation.target_failures,
                            obligation.required_clean_legs,
                            obligation.baseline_failures,
                            obligation.baseline_legs,
                            obligation.failing_commit,
                        );
                    }
                }
            }
            validate_status::Verdict::NoResult => {
                println!(
                    "# validate NO-RESULT {} -- red gates could not run (command-not-found storm or sub-second collapse); an environment fault, not a product defect; re-dispatch required",
                    assessment.sha,
                );
            }
            validate_status::Verdict::NotValidated => match &freshness {
                // The confusable case, now named. This says nothing about the
                // commit -- it reports that THIS READER cannot see records that
                // demonstrably exist.
                Some(LedgerFreshness::UpstreamHasRecord { records, git_ref }) => {
                    println!(
                        "# validate LEDGER-LAGGING {} -- THIS CHECKOUT IS STALE, NOT THIS COMMIT UNTESTED. {} ledger record(s) for this commit exist in {git_ref} and are ABSENT here; `ledger/` is tracked, so it advances only on pull. Pull, then re-run this query. No verdict about the commit is implied and nothing is authorized.",
                        assessment.sha, records,
                    );
                }
                // Present here and unreadable. Distinct from staleness, and the
                // remedy is the opposite: pulling cannot help, because the
                // upstream copy is the same bytes. Saying "pull" here sends an
                // operator round a loop that never terminates, which is what
                // this tool used to do.
                Some(LedgerFreshness::LocalRecordUnreadable { records }) => {
                    println!(
                        "# validate LEDGER-UNREADABLE {} -- THIS RECORD IS HERE AND CANNOT BE PARSED, NOT MISSING AND NOT UNTESTED. {} ledger record(s) for this commit are present in this checkout and failed to deserialize (reasons printed on stderr above). DO NOT PULL: the upstream copy carries the same bytes. Do not widen the reader or rewrite the ledger. Fix the writer and rerun validation to append a valid receipt. No verdict about the commit is implied and nothing is authorized.",
                        assessment.sha, records,
                    );
                }
                // Freshness unprovable: report the absence WITH its caveat rather
                // than letting an unverifiable probe harden into a clean claim.
                Some(LedgerFreshness::Unknown { reason }) => {
                    println!(
                        "# validate NOT-VALIDATED {} -- no clean full-coverage PASS record (0 non-qualifying record(s) for this commit); FRESHNESS UNVERIFIED ({reason}), so this absence is NOT authoritative -- a record may exist upstream that this checkout cannot see",
                        assessment.sha,
                    );
                }
                // Absence confirmed against the remote: genuinely never validated.
                Some(LedgerFreshness::UpstreamAgrees { git_ref }) => {
                    println!(
                        "# validate NOT-VALIDATED {} -- no clean full-coverage PASS record (0 non-qualifying record(s) for this commit); {git_ref} carries no record for it either, so this absence IS authoritative",
                        assessment.sha,
                    );
                }
                None => {
                    println!(
                        "# validate NOT-VALIDATED {} -- no clean full-coverage PASS record ({} non-qualifying record(s) for this commit)",
                        assessment.sha,
                        assessment.disqualified.len(),
                    );
                }
            },
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
    let typed_receipt = PublishedValidationReceipt::parse(artifact_body.as_bytes())?;
    let expected_producer = selected
        .producer_definition
        .as_ref()
        .map(ProducerDefinitionExpectation::from);
    typed_receipt.validate(
        repo,
        sha,
        &selected.row,
        &selected.canonical_sha256,
        IdentityRequirement::Required,
        expected_producer.as_ref(),
    )?;
    let executed_tests = typed_receipt
        .ledger_record
        .executed_tests
        .ok_or_else(|| "publisher artifact omitted selected execution count".to_string())?;
    let producer_valid_commits = typed_receipt.producer.valid_commits.clone().or_else(|| {
        selected
            .producer_definition
            .as_ref()
            .and_then(|producer| producer.valid_commits.clone())
    });
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
        run_id: typed_receipt.run_id,
        executed_tests,
        log_sha256: typed_receipt.log_sha256,
        producer_coverage_status: typed_receipt.producer.coverage_status,
        producer_paths: typed_receipt.producer.paths,
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
    let _ = ledger;
    let mut command = Command::new("python3");
    command
        .arg(publisher)
        .args([
            "--repo",
            repo,
            "--sha",
            sha,
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
    let output = command
        .bounded_output_with_input(selected.canonical_row_json.as_bytes())
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
            let values = commits.split(',').map(str::to_string).collect::<Vec<_>>();
            if values.is_empty()
                || values.iter().any(|commit| !is_oid(commit))
                || !values.iter().any(|commit| commit == sha)
                || values.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(
                    "immutable receipt verifier returned invalid producer commit bound".into(),
                );
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
        .bounded_output()
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
    let output = command
        .bounded_output_with_input(&comments)
        .map_err(|error| format!("cannot run the immutable receipt verifier: {error}"))?;
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
    .bounded_output()
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
        return Err("immutable verifier producer condition differs from artifact evidence".into());
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
            "Local validation receipt published before applying `{LOCALLY_VALIDATED_LABEL}`.\n\n- SHA: `{sha}`\n- Run ID: `{}`\n- Executed tests: `{}`\n- Producer coverage: `{}`\n- Producer paths: `{}`\n- Producer valid commits: `{}`\n- Receipt identity SHA-256: `{}`\n- Immutable artifact: `{VALIDATION_RECEIPT_REPO}@{receipt_commit}:{}`\n- Artifact SHA-256: `{}`\n- Log SHA-256: `{}`\n\n{marker}",
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
        let agent = env::var("DG_AGENT_NAME").unwrap_or_default();
        let comment = gh_command(
            root,
            &[
                "--who",
                &agent,
                "--team",
                "hermit2",
                "--role",
                "coordinator",
                "pr",
                "comment",
                &pr_arg,
                "--repo",
                repo,
                "--body",
                &body,
            ],
        )
        .bounded_output()
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
    .bounded_output()
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

/// `ci-hub apply-local-label --pr <N> | --all-open` reconciles the derived
/// `locally-validated` cache in both directions. A qualifying exact-head receipt
/// binds the label; every other exact-head verdict removes an existing label.
/// Receipt comments remain immutable history and are already exact-head bound.
fn run_apply_local_label(root: &Path, args: ApplyLocalLabelArgs) -> Result<i32, CiHubError> {
    let path = ledger_path(root);
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
    let mut removed = 0i32;
    let mut failed = 0i32;
    for pr in prs {
        // In sweep mode a single unreadable PR must not abort the whole run.
        let (head, has_label) = match gh_pr_head_and_local_label(root, &args.repo, pr) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("ci-hub: apply-local-label: PR #{pr}: {error}");
                actions.push(
                    serde_json::json!({"pr": pr, "action": "error", "detail": error.to_string()}),
                );
                failed += 1;
                continue;
            }
        };
        let assessment = match assess_canonical_receipts(root, &rows, &head, &args.repo) {
            Ok(assessment) => assessment,
            Err(detail) => {
                eprintln!("ci-hub: apply-local-label: PR #{pr}: {detail}");
                actions.push(serde_json::json!({
                    "pr": pr,
                    "head": head,
                    "action": "assessment-error",
                    "detail": detail,
                }));
                failed += 1;
                continue;
            }
        };
        match local_label_reconcile_action(
            assessment.verdict == validate_status::Verdict::Validated,
            has_label,
        ) {
            LocalLabelReconcileAction::Remove => {
                let action = if args.dry_run {
                    "would-remove"
                } else {
                    match remove_local_validation_label(root, &args.repo, pr) {
                        Ok(()) => "removed",
                        Err(error) => {
                            eprintln!("ci-hub: apply-local-label: PR #{pr}: {error}");
                            actions.push(serde_json::json!({
                                "pr": pr,
                                "head": head,
                                "action": "remove-failed",
                                "verdict": assessment.verdict.as_str(),
                                "detail": error.to_string(),
                            }));
                            failed += 1;
                            continue;
                        }
                    }
                };
                println!(
                    "PR #{pr}: {action} stale {LOCALLY_VALIDATED_LABEL} -- head {} is {}",
                    &head[..12.min(head.len())],
                    assessment.verdict.as_str()
                );
                actions.push(serde_json::json!({
                    "pr": pr,
                    "head": head,
                    "action": action,
                    "verdict": assessment.verdict.as_str(),
                }));
                removed += 1;
                continue;
            }
            LocalLabelReconcileAction::LeaveAbsent => {
                println!(
                    "PR #{pr}: skip -- head {} is {} and label is absent",
                    &head[..12.min(head.len())],
                    assessment.verdict.as_str()
                );
                actions.push(serde_json::json!({
                    "pr": pr,
                    "head": head,
                    "action": "skip",
                    "verdict": assessment.verdict.as_str(),
                }));
                continue;
            }
            LocalLabelReconcileAction::Bind => {}
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
        let report = serde_json::json!({
            "schema_version": 1,
            "applied": applied,
            "removed": removed,
            "actions": actions,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serialize report")
        );
    }
    Ok(if failed == 0 { 0 } else { 1 })
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
    command.stdin(Stdio::inherit()).stdout(Stdio::inherit());
    let (status, stderr) =
        command
            .bounded_status_with_stderr()
            .map_err(|source| CiHubError::Launch {
                tool: script.display().to_string(),
                source,
            })?;
    write_python_stderr(&stderr).map_err(|source| CiHubError::Launch {
        tool: format!("{} stderr", script.display()),
        source,
    })?;
    Ok(exit_status_code(status))
}

const PYTHON_JEMALLOC_WARNING: &[u8] =
    b"<jemalloc>: Invalid conf pair: experimental_infallible_new:true";

fn python_stderr_without_allocator_warning(stderr: &[u8]) -> Vec<u8> {
    let mut kept = Vec::with_capacity(stderr.len());
    for line in stderr.split_inclusive(|byte| *byte == b'\n') {
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        if content != PYTHON_JEMALLOC_WARNING {
            kept.extend_from_slice(line);
        }
    }
    kept
}

fn write_python_stderr(stderr: &[u8]) -> io::Result<()> {
    io::stderr().write_all(&python_stderr_without_allocator_warning(stderr))
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

fn validate_run_forwarded_args(root: &Path, mut arguments: Vec<OsString>) -> Vec<OsString> {
    let insertion = arguments
        .iter()
        .position(|argument| argument == "--")
        .unwrap_or(arguments.len());
    arguments.splice(
        insertion..insertion,
        [
            OsString::from("--state-root"),
            landing_lock::repository_lock_root(root).into_os_string(),
        ],
    );
    arguments
}

fn to_exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
}

#[cfg(test)]
mod tests {

    // ---- COMMITS-SINCE-GREEN rendering -------------------------------------
    // The rendering IS the feature: this number exists so a reader who has never
    // heard of green depth still gets it, so what the line SAYS is the contract.

    #[test]
    fn found_states_the_depth_and_the_commit() {
        let line = GreenDepth::Found {
            depth: 365,
            sha: "dee8cf49ce6a7e3e9e2c87c930f3aa55ae83da18".into(),
            window: 628,
            failed_on_record: 1,
            needs_rerun: 18,
            truncated: 2,
            no_result: 0,
            not_validated: 1,
            ledger_unreadable: 1,
            no_record: 342,
        }
        .line();
        assert_eq!(
            line,
            "HERMIT COMMITS-SINCE-GREEN 365 (last full green dee8cf49ce6a at first-parent depth 365; FAILED=1 NEEDS-RERUN=18 TRUNCATED=2 NO-RESULT=0 NOT-VALIDATED=1 LEDGER-UNREADABLE=1 NO-RECORD=342; searched 628 commits)"
        );
    }

    #[test]
    fn found_json_keeps_each_verdict_and_absence_separate() {
        let value = GreenDepth::Found {
            depth: 7,
            sha: "dee8cf49ce6a7e3e9e2c87c930f3aa55ae83da18".into(),
            window: 12,
            failed_on_record: 1,
            needs_rerun: 1,
            truncated: 1,
            no_result: 1,
            not_validated: 1,
            ledger_unreadable: 1,
            no_record: 1,
        }
        .json();

        assert_eq!(value["commits_failed_on_record"], 1);
        assert_eq!(value["commits_needing_rerun"], 1);
        assert_eq!(value["commits_truncated"], 1);
        assert_eq!(value["commits_with_no_result"], 1);
        assert_eq!(value["commits_not_validated"], 1);
        assert_eq!(value["commits_with_unreadable_record"], 1);
        assert_eq!(value["commits_with_records"], 5);
        assert_eq!(value["commits_without_any_record"], 1);
        assert_eq!(value["commits_without_record"], 2);
    }

    #[test]
    fn newest_green_branch_output_shows_the_full_partition() {
        let report = history_queries::NewestGreenReport {
            schema_version: 5,
            branch: "main".into(),
            branch_ref: "origin/main".into(),
            branch_tip: "f".repeat(40),
            gate_schema: "merge-gate-v2".into(),
            gate_schema_floor: "floor".into(),
            range_oldest_commit: "0".repeat(40),
            branch_commits_in_range: 628,
            trustworthy_recorded_commits_in_range: 23,
            full_green_commits_in_range: 1,
            green: history_queries::ValidationEvidence {
                sha: "d".repeat(40),
                finished_at: Some("2026-08-26T00:00:00Z".into()),
                profile: "full".into(),
                selection_mode: "full".into(),
                coverage: None,
                coverage_satisfied: None,
                coverage_status: "grandfathered-unknown".into(),
                result: "pass".into(),
                log_file: None,
                producer_definition: None,
            },
            commits_after_green: 365,
            commits_failed_on_record: 1,
            commits_needing_rerun: 18,
            commits_truncated: 2,
            commits_with_no_result: 0,
            commits_not_validated: 1,
            commits_with_unreadable_record: 1,
            commits_without_any_record: 342,
            commits_with_records: 22,
        };

        assert_eq!(
            newest_green_branch_line(&report, false),
            format!(
                "BRANCH main tip={} commits-after-green=365 failed=1 needs-rerun=18 truncated=2 no-result=0 not-validated=1 recorded=22 ledger-unreadable=1 no-record=342 cache=miss",
                "f".repeat(40)
            )
        );
        assert_eq!(
            report.commits_with_records
                + report.commits_with_unreadable_record
                + report.commits_without_any_record,
            report.commits_after_green
        );
    }

    #[test]
    fn a_bounded_absence_states_its_window_and_refuses_to_generalise() {
        // ⚠️ THE WHOLE POINT. Without the window, "no green within 200" and "no
        // green ever" render identically and the metric is a binary again.
        let line = GreenDepth::NoneInWindow {
            window: 200,
            oldest: "0123456789abcdef0123456789abcdef01234567".into(),
            recorded: 10,
        }
        .line();
        assert_eq!(
            line,
            "HERMIT COMMITS-SINCE-GREEN NONE-WITHIN-200 -- no full green in the 200 first-parent commits back to 0123456789ab (10 carried any record). This is a BOUNDED search, not a claim that no green exists; widen it with `ci-hub newest-green`."
        );
    }

    #[test]
    fn the_four_states_never_render_alike() {
        let states = [
            GreenDepth::Found {
                depth: 0,
                sha: "a".repeat(40),
                window: 1,
                failed_on_record: 0,
                needs_rerun: 0,
                truncated: 0,
                no_result: 0,
                not_validated: 0,
                ledger_unreadable: 0,
                no_record: 0,
            },
            GreenDepth::NoneInWindow {
                window: 5,
                oldest: "b".repeat(40),
                recorded: 1,
            },
            GreenDepth::Unverifiable {
                visible_depth: 3,
                required_depth: "50".into(),
            },
            GreenDepth::Unavailable {
                why: "no checkout".into(),
            },
        ];
        let lines: Vec<String> = states.iter().map(GreenDepth::line).collect();
        assert!(lines.iter().all(|line| line.starts_with("HERMIT ")));
        for i in 0..lines.len() {
            for j in (i + 1)..lines.len() {
                assert_ne!(lines[i], lines[j]);
            }
        }
        // And only the FOUND state may carry a non-null depth.
        for (idx, state) in states.iter().enumerate() {
            let depth = state.json()["depth"].clone();
            if idx == 0 {
                assert_eq!(depth, serde_json::json!(0));
            } else {
                assert!(depth.is_null(), "state {idx} must not report a depth");
            }
        }
    }

    #[test]
    fn a_failed_query_is_not_an_absence() {
        // Unavailable must never be readable as "main has no green".
        let line = GreenDepth::Unavailable {
            why: "no hermit checkout".into(),
        }
        .line();
        assert!(line.contains("UNAVAILABLE"), "{line}");
        assert!(line.contains("NOT evidence"), "{line}");
        assert_eq!(
            GreenDepth::Unavailable { why: "x".into() }.json()["state"],
            serde_json::json!("unavailable")
        );
    }

    #[test]
    fn recent_history_names_total_and_passed_tests_for_a_qualifying_row() {
        let mut value = schema4_receipt_value();
        value["passed_tests"] = serde_json::json!(785);
        let row = history_row(value);
        let mut entry = finished_validate_status_entry(
            Path::new("/no-retained-log"),
            CANONICAL_VALIDATE_REPO,
            &row,
        );
        entry.finished_at = Some("2026-08-03T00:00:00Z".into());

        assert_eq!(entry.status, "VALIDATED");
        assert_eq!(entry.tests_total, Some(1479));
        assert_eq!(entry.tests_passed, Some(785));
        assert_eq!(entry.tests_passed_at_least, None);
        assert_eq!(
            validate_status_header(&[ValidateStatusColumn::Sha], ValidateStatusTimes::Relative),
            "STATUS        SHA          STARTED/FINISHED     ELAPSED HOST             TESTS-TOTAL TESTS-PASSED SUMMARY"
        );
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-03T01:36:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let line = validate_status_line(
            &entry,
            &[ValidateStatusColumn::Sha],
            ValidateStatusTimes::Relative,
            now,
        );
        assert!(line.starts_with("VALIDATED     630f44aab7fd"), "{line}");
        assert!(line.contains("1h36m ago"), "{line}");
        assert!(line.contains("          785 passed"), "{line}");
    }

    #[test]
    fn validate_status_time_modes_render_the_same_timestamp() {
        let value = "2026-08-27T17:26:14Z";
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-27T19:02:14Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            format_validate_timestamp(value, ValidateStatusTimes::Relative, now),
            "1h36m ago"
        );
        assert_eq!(
            format_validate_timestamp(value, ValidateStatusTimes::Utc, now),
            "2026-08-27T17:26:14Z"
        );
        assert_eq!(
            format_validate_timestamp("2026-08-27T07:02:14Z", ValidateStatusTimes::Relative, now,),
            "12hrs ago"
        );
        let expected_local = chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&chrono::Local)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert_eq!(
            format_validate_timestamp(value, ValidateStatusTimes::Local, now),
            expected_local
        );
    }

    #[test]
    fn relative_validate_status_time_reports_future_clock_skew() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-27T17:21:14Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            format_validate_timestamp("2026-08-27T17:26:14Z", ValidateStatusTimes::Relative, now),
            "in 5m"
        );
    }

    #[test]
    fn recent_history_keeps_unknown_total_distinct_for_a_nonqualifying_row() {
        let row = history_row(serde_json::json!({
            "schema_version": 5,
            "repo": CANONICAL_VALIDATE_REPO,
            "commit": "0123456789abcdef0123456789abcdef01234567",
            "finished_at": "2026-08-02T00:00:00Z",
            "host": "runner",
            "slot": "slot",
            "profile": "full",
            "selection_mode": "full",
            "commit_anchored": true,
            "tree_dirty": true,
            "result": "pass",
            "exit_code": 0,
            "failures": 0,
            "executed_tests": 10,
            "filtered_tests": null,
            "passed_tests": 10,
            "gates_run": 1,
            "gates_expected": 2,
            "real_seconds": 10.0,
            "admission": "ci-hub-validate-lock"
        }));
        let entry = finished_validate_status_entry(
            Path::new("/no-retained-log"),
            CANONICAL_VALIDATE_REPO,
            &row,
        );
        assert_eq!(entry.status, "NOT-VALIDATED");
        assert_eq!(entry.tests_total, None);
        assert_eq!(entry.tests_passed, Some(10));
        assert_eq!(entry.tests_passed_at_least, None);
        assert_eq!(entry.summary, "dirty tree");
    }

    #[test]
    fn recent_history_names_a_zero_test_pass_as_no_result() {
        let mut value = schema4_receipt_value();
        value["executed_tests"] = serde_json::json!(0);
        value["filtered_tests"] = serde_json::json!(0);
        value["passed_tests"] = serde_json::json!(0);
        let row = history_row(value);
        let entry = finished_validate_status_entry(
            Path::new("/no-retained-log"),
            CANONICAL_VALIDATE_REPO,
            &row,
        );

        assert_eq!(entry.status, "NO-RESULT");
        assert_eq!(entry.tests_total, Some(0));
        assert_eq!(entry.tests_passed, Some(0));
        assert_eq!(entry.summary, "validation did not exercise the product");
    }

    #[test]
    fn recent_history_reads_an_explicit_passed_count_for_a_failed_row() {
        let row = history_row(serde_json::json!({
            "schema_version": 5,
            "repo": CANONICAL_VALIDATE_REPO,
            "commit": "fedcba9876543210fedcba9876543210fedcba98",
            "finished_at": "2026-08-03T00:00:00Z",
            "host": "runner",
            "slot": "slot",
            "profile": "full",
            "selection_mode": "full",
            "commit_anchored": true,
            "tree_dirty": false,
            "result": "fail",
            "exit_code": 1,
            "failures": 1,
            "executed_tests": 1685,
            "filtered_tests": 1960,
            "passed_tests": 1678,
            "gates_run": 64,
            "gates_expected": 64,
            "real_seconds": 1850.0,
            "admission": "ci-hub-validate-lock"
        }));
        let entry = finished_validate_status_entry(
            Path::new("/no-retained-log"),
            CANONICAL_VALIDATE_REPO,
            &row,
        );
        assert_eq!(entry.tests_total, Some(3645));
        assert_eq!(entry.tests_passed, Some(1678));
        assert_eq!(entry.tests_passed_at_least, None);
    }

    #[test]
    fn recent_history_does_not_relabel_executed_tests_as_passed() {
        let mut value = schema4_receipt_value();
        value["executed_tests"] = serde_json::json!(786);
        value["filtered_tests"] = serde_json::json!(693);
        let row = history_row(value);
        assert_eq!(
            recent_row_test_counts(Path::new("/no-retained-log"), &row),
            TestCounts {
                total: Some(1479),
                passed: None,
                passed_at_least: None,
            }
        );
    }

    #[test]
    fn recent_history_rejects_malformed_test_counts() {
        for passed in [
            serde_json::json!(-1),
            serde_json::json!(787),
            serde_json::json!(true),
            serde_json::json!("786"),
        ] {
            let mut value = schema4_receipt_value();
            value["passed_tests"] = passed;
            let row = history_row(value);
            assert_eq!(
                recent_row_test_counts(Path::new("/no-retained-log"), &row).passed,
                None
            );
        }

        let mut value = schema4_receipt_value();
        value["executed_tests"] = serde_json::json!(-1);
        value["filtered_tests"] = serde_json::json!(2);
        value["passed_tests"] = serde_json::json!(0);
        let row = history_row(value);
        assert_eq!(
            recent_row_test_counts(Path::new("/no-retained-log"), &row),
            TestCounts {
                total: None,
                passed: None,
                passed_at_least: None,
            }
        );

        let mut value = schema4_receipt_value();
        value["executed_tests"] = serde_json::Value::Null;
        value["passed_tests"] = serde_json::json!(0);
        let row = history_row(value);
        assert_eq!(
            recent_row_test_counts(Path::new("/no-retained-log"), &row).passed,
            None
        );
    }

    #[test]
    fn recent_history_marks_log_derived_passes_as_an_observed_lower_bound() {
        let log = std::env::temp_dir().join(format!(
            "ci-hub-passed-tests-{}-{}.log",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::write(
            &log,
            "[test.one] test result: ok. 1 passed; 0 failed\n\
             [test.one] ✓ PASS [test result: ok. 10 passed; 0 failed]\n\
             [test.two] test result: FAILED. 2 passed; 1 failed\n\
             [test.two] test result: FAILED. nextest: 8 passed, 1 failed\n",
        )
        .unwrap();
        let mut value = schema4_receipt_value();
        value["executed_tests"] = serde_json::json!(30);
        value["filtered_tests"] = serde_json::json!(0);
        value["log_file"] = serde_json::json!(log);
        let row = history_row(value);
        assert_eq!(
            recent_row_test_counts(Path::new("/"), &row),
            TestCounts {
                total: Some(30),
                passed: None,
                passed_at_least: Some(21),
            }
        );
        std::fs::remove_file(log).unwrap();
    }

    #[test]
    fn recent_history_does_not_turn_an_empty_log_into_zero_passed() {
        let log = std::env::temp_dir().join(format!(
            "ci-hub-no-passed-tests-{}-{}.log",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::write(&log, "validation started but emitted no test summary\n").unwrap();
        let mut value = schema4_receipt_value();
        value["executed_tests"] = serde_json::json!(1);
        value["log_file"] = serde_json::json!(log);
        let row = history_row(value);
        assert_eq!(
            recent_row_test_counts(Path::new("/"), &row).passed_at_least,
            None
        );
        std::fs::remove_file(log).unwrap();
    }

    #[test]
    fn live_validate_requires_boot_pid_generation_and_exact_unit() {
        let root = std::env::temp_dir().join(format!(
            "ci-hub-live-validate-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let runs = root.join("ignored/validate/runs");
        let proc_root = root.join("proc");
        let process = proc_root.join("123");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::create_dir_all(&process).unwrap();
        let boot_id = root.join("boot_id");
        std::fs::write(&boot_id, "boot-one\n").unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[19] = "777";
        std::fs::write(
            process.join("stat"),
            format!("123 (validate runner) {}\n", fields.join(" ")),
        )
        .unwrap();
        std::fs::write(
            process.join("cgroup"),
            "0::/user.slice/validate-hermit-test.service\n",
        )
        .unwrap();
        std::fs::write(
            runs.join("validate-hermit-test.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "state": "running",
                "unit": "validate-hermit-test.service",
                "target": "a".repeat(40),
                "repo": CANONICAL_VALIDATE_REPO,
                "agent": "hermit-test",
                "started_at": "2026-08-27T12:00:00Z",
                "host": "validation-host",
                "process_identity": {
                    "pid": 123,
                    "start_ticks": 777,
                    "boot_id": "boot-one"
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-27T12:05:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let live =
            live_validate_runs_from(&root, CANONICAL_VALIDATE_REPO, &proc_root, &boot_id, now)
                .unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].status, "IN-PROGRESS");
        assert_eq!(live[0].elapsed_seconds, Some(300.0));
        assert_eq!(live[0].host.as_deref(), Some("validation-host"));

        std::fs::write(
            process.join("cgroup"),
            "0::/user.slice/validate-hermit-test.service.old\n",
        )
        .unwrap();
        assert!(
            live_validate_runs_from(&root, CANONICAL_VALIDATE_REPO, &proc_root, &boot_id, now,)
                .unwrap()
                .is_empty()
        );
        fields[0] = "Z";
        std::fs::write(
            process.join("stat"),
            format!("123 (validate runner) {}\n", fields.join(" ")),
        )
        .unwrap();
        std::fs::write(
            process.join("cgroup"),
            "0::/user.slice/validate-hermit-test.service\n",
        )
        .unwrap();
        assert!(
            live_validate_runs_from(&root, CANONICAL_VALIDATE_REPO, &proc_root, &boot_id, now,)
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_validate_reads_current_handle_through_shared_contract() {
        let root = std::env::temp_dir().join(format!(
            "ci-hub-live-current-validate-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let runs = root.join("ignored/validate/runs");
        let process = root.join("proc/123");
        let authority_dir = root.join("ci-hub/validate");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::create_dir_all(&process).unwrap();
        std::fs::create_dir_all(&authority_dir).unwrap();
        std::fs::write(
            authority_dir.join("run_registry.py"),
            include_str!("validate/run_registry.py"),
        )
        .unwrap();
        let boot_id = root.join("boot_id");
        std::fs::write(&boot_id, "boot-one\n").unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[19] = "777";
        std::fs::write(
            process.join("stat"),
            format!("123 (validate runner) {}\n", fields.join(" ")),
        )
        .unwrap();
        std::fs::write(
            process.join("cgroup"),
            "0::/user.slice/validate-hermit-current.service\n",
        )
        .unwrap();
        let current = serde_json::json!({
            "schema_version": 1,
            "kind": "validate",
            "state": "running",
            "unit": "validate-hermit-current.service",
            "target": "a".repeat(40),
            "repo": CANONICAL_VALIDATE_REPO,
            "checkout": "/tmp/checkout",
            "source_checkout": "/tmp/source",
            "temporary_checkout": true,
            "cargo_home": "/tmp/cargo",
            "log": "/tmp/validate.log",
            "agent": "hermit-test",
            "pr": null,
            "started_at": "2026-08-27T12:00:00Z",
            "parent_checkout_head": "b".repeat(40),
            "producer": "ci-hub/validate/run_registry.py",
            "admission": "ci-hub validate-lock",
            "pane_role": "observer-only",
            "validate_lock_child_deadline_seconds": 3600,
            "e2e_result_root": "/tmp/e2e",
            "safe_ci_dag_runner_log_dir": "/tmp/dag",
            "hermit_run_timeout_seconds": 3300,
            "host": "validation-host",
            "process_identity": {
                "pid": 123,
                "start_ticks": 777,
                "boot_id": "boot-one"
            }
        });
        let legacy_shape: ValidateRunRecord = serde_json::from_value(current.clone())
            .expect("an already-deployed schema-1 reader must accept the additive fields");
        assert_eq!(legacy_shape.schema_version, 1);
        std::fs::write(
            runs.join("validate-hermit-current.json"),
            serde_json::to_vec(&current).unwrap(),
        )
        .unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-27T12:05:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let live = live_validate_runs_from(
            &root,
            CANONICAL_VALIDATE_REPO,
            &root.join("proc"),
            &boot_id,
            now,
        )
        .unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(
            live[0].sha.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(live[0].host.as_deref(), Some("validation-host"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_running_handle_without_process_identity_is_not_in_progress() {
        let root = std::env::temp_dir().join(format!(
            "ci-hub-stale-validate-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let runs = root.join("ignored/validate/runs");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::write(
            runs.join("stale.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "state": "running",
                "unit": "validate-stale.service",
                "target": "b".repeat(40),
                "repo": CANONICAL_VALIDATE_REPO,
                "started_at": "2026-08-01T00:00:00Z"
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(live_validate_runs_from(
            &root,
            CANONICAL_VALIDATE_REPO,
            &root.join("proc"),
            &root.join("boot_id"),
            chrono::Utc::now(),
        )
        .unwrap()
        .is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn python_allocator_warning_filter_removes_only_the_exact_line() {
        let input = b"before\n<jemalloc>: Invalid conf pair: experimental_infallible_new:true\nafter\n<jemalloc>: another warning\n";
        assert_eq!(
            python_stderr_without_allocator_warning(input),
            b"before\nafter\n<jemalloc>: another warning\n"
        );
    }
    use super::*;

    const RECEIPT_SHA: &str = "630f44aab7fdf4ee52e572c38ae09818e92271b2";

    /// THE SUMMARY MUST NAME THE CAUSE, NOT THE CONSEQUENCES.
    ///
    /// Reproduces the exact shape of main's dee8cf49ce6a receipt: one node that
    /// failed on its own account plus several stopped by the runner's eager
    /// exit, all carrying `result: "fail"`. Before this fix the summary printed
    /// the first two IN ARRAY ORDER -- both aborts -- and omitted the real
    /// failure entirely, which is how ten downstream victims came to be quoted
    /// as main's failure list.
    ///
    /// Deliberately ORDERED WITH THE ABORTS FIRST, because array order is what
    /// the old code keyed on: if the fix ever regresses to taking the head of
    /// the list, this test sees it.
    #[test]
    fn the_summary_names_the_failing_node_and_only_counts_the_aborted_ones() {
        let row: HistoryRow = serde_json::from_str(
            r#"{"gates":[
                {"name":"lint.clippy","result":"fail","aborted":true,
                 "reason":"ABORTED (eager-exit after another step failed)"},
                {"name":"test.hermit_integration","result":"fail","aborted":true,
                 "reason":"ABORTED (eager-exit after another step failed)"},
                {"name":"e2e.manifest_applications","result":"fail","aborted":false,
                 "reason":"exit 1"},
                {"name":"test.rr_suite_contract","result":"fail","aborted":true,
                 "reason":"ABORTED (eager-exit after another step failed)"},
                {"name":"build.workspace","result":"pass"}
            ]}"#,
        )
        .expect("fixture row parses");

        let summary = recent_row_summary(&row, "NEEDS-RERUN");
        assert_eq!(summary, "failed: e2e.manifest_applications; 3 aborted");
        assert!(
            !summary.contains("lint.clippy"),
            "an aborted node must not be presented as the failure: {summary}"
        );
    }

    /// A run where EVERYTHING unsuccessful was an abort has no cause among its
    /// gates, and must say so instead of promoting an arbitrary victim.
    #[test]
    fn a_run_with_only_aborted_nodes_reports_no_failing_node() {
        let row: HistoryRow = serde_json::from_str(
            r#"{"gates":[
                {"name":"lint.clippy","result":"fail","aborted":true,"reason":"ABORTED"},
                {"name":"test.cli","result":"fail","aborted":true,"reason":"ABORTED"}
            ]}"#,
        )
        .expect("fixture row parses");

        let summary = recent_row_summary(&row, "NEEDS-RERUN");
        assert!(
            summary.starts_with("no failing node recorded; 2 aborted:"),
            "an all-aborted run must not claim a failure: {summary}"
        );
    }

    /// An ordinary failure with no aborts keeps the original wording, so the
    /// fix does not churn the common case.
    #[test]
    fn a_plain_failure_is_unchanged_by_the_abort_split() {
        let row: HistoryRow = serde_json::from_str(
            r#"{"gates":[{"name":"e2e.manifest_backend_parity_c","result":"fail"}]}"#,
        )
        .expect("fixture row parses");
        assert_eq!(
            recent_row_summary(&row, "NEEDS-RERUN"),
            "failed: e2e.manifest_backend_parity_c"
        );
    }

    #[test]
    fn local_label_reconcile_preserves_or_adds_a_validated_head() {
        assert_eq!(
            local_label_reconcile_action(true, true),
            LocalLabelReconcileAction::Bind
        );
        assert_eq!(
            local_label_reconcile_action(true, false),
            LocalLabelReconcileAction::Bind
        );
    }

    #[test]
    fn local_label_reconcile_removes_only_an_unbacked_label() {
        assert_eq!(
            local_label_reconcile_action(false, true),
            LocalLabelReconcileAction::Remove
        );
        assert_eq!(
            local_label_reconcile_action(false, false),
            LocalLabelReconcileAction::LeaveAbsent
        );
    }

    /// The remote-ledger probe must count records BY THEIR COMMIT FIELD.
    ///
    /// A 40-hex sha appears in several commit-shaped fields of an unrelated
    /// record -- `base_sha` most commonly -- so a substring scan would report a
    /// lag for a record that is not about this commit at all, and send an agent
    /// to pull for evidence that was never there. Both directions are planted
    /// here: one genuine record for the target, and one decoy whose `base_sha`
    /// is the target while its own commit is something else.
    #[test]
    fn remote_ledger_probe_counts_by_commit_field_not_substring() {
        let target = "647c7af521ab5ab7c4e9ad5f8f2dd299345f3eda";
        let other = "d0dfef25c1552325426e98a4734b9f57abfb846f";
        let root = std::env::temp_dir().join(format!(
            "ci-hub-ledger-lag-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let shard = root.join("ledger/hermit/devbig014");
        std::fs::create_dir_all(&shard).unwrap();
        let genuine = serde_json::json!({
            "schema": "validate-ledger/v1",
            "event_id": "genuine",
            "event_type": "run.result",
            "emitted_at": "2026-08-28T03:00:00Z",
            "team": "hermit",
            "host": "devbig014",
            "run_id": "genuine",
            "producer": {
                "source": "observed",
                "tool": "fixture",
                "tool_version": "1"
            },
            "commit": target,
            "outcome": "fail",
            "legacy_row": {"commit": target, "result": "fail", "profile": "full"},
        });
        // Decoy: its OWN commit is `other`; the target only appears as base_sha.
        let decoy = serde_json::json!({
            "schema": "validate-ledger/v1",
            "event_id": "decoy",
            "event_type": "run.result",
            "emitted_at": "2026-08-28T03:01:00Z",
            "team": "hermit",
            "host": "devbig014",
            "run_id": "decoy",
            "producer": {
                "source": "observed",
                "tool": "fixture",
                "tool_version": "1"
            },
            "commit": other,
            "outcome": "pass",
            "legacy_row": {"commit": other, "base_sha": target, "result": "pass"},
        });
        std::fs::write(shard.join("2026-08.jsonl"), format!("{genuine}\n{decoy}\n")).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&root)
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
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "ledger fixture"]);

        assert_eq!(
            remote_ledger_records_for(&root, "HEAD", target, CANONICAL_VALIDATE_REPO).unwrap(),
            1,
            "the genuine record must count and the base_sha decoy must not"
        );
        // A commit with no record at all must read as a true zero, or every
        // absence would be reported as a lag and the outcome would become a
        // blanket excuse rather than a distinction.
        assert_eq!(
            remote_ledger_records_for(
                &root,
                "HEAD",
                "0123456789abcdef0123456789abcdef01234567",
                CANONICAL_VALIDATE_REPO,
            )
            .unwrap(),
            0,
        );
        // An unresolvable ref is an ERROR, never a silent zero: a zero here would
        // harden an unverifiable probe into an authoritative absence claim.
        assert!(remote_ledger_records_for(
            &root,
            "refs/heads/absent",
            target,
            CANONICAL_VALIDATE_REPO,
        )
        .is_err());

        // A malformed line is not absence. The old Value reader skipped it and
        // returned zero, which let a broken remote ledger confirm a local
        // NOT-VALIDATED claim.
        let mut malformed = genuine;
        malformed.as_object_mut().unwrap().remove("event_id");
        std::fs::write(shard.join("2026-08.jsonl"), format!("{malformed}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "malformed ledger fixture"]);
        let error =
            remote_ledger_records_for(&root, "HEAD", target, CANONICAL_VALIDATE_REPO).unwrap_err();
        assert!(error.contains("event_id"), "{error}");
        std::fs::remove_dir_all(&root).ok();
    }

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

    fn reverie_receipt_value() -> serde_json::Value {
        let gates = [
            "Cross-client skill discovery",
            "Build workspace",
            "Test regular workspace cases",
            "Documentation tests",
            "Clippy",
            "Rustfmt",
        ]
        .into_iter()
        .map(|name| serde_json::json!({"name": name, "result": "pass", "exit_code": 0}))
        .collect::<Vec<_>>();
        serde_json::json!({
            "schema_version": 5,
            "producer": "ci-hub-reverie-validate-run",
            "repo": CANONICAL_REVERIE_REPO,
            "started_at": "2026-08-11T12:00:00Z",
            "finished_at": "2026-08-11T12:01:00Z",
            "host": "validation-host",
            "slot": "reverie-slot",
            "cwd": "/tmp/reverie",
            "profile": "full",
            "selection_mode": "full",
            "full_coverage": true,
            "commit": RECEIPT_SHA,
            "tree": "f9294df9294df9294df9294df9294df9294df929",
            "commit_anchored": true,
            "tree_dirty": false,
            "result": "pass",
            "raw_result": "pass",
            "exit_code": 0,
            "executed_tests": 100,
            "filtered_tests": 20,
            "coverage": {
                "planned_test_nodes": 2,
                "executed_test_nodes": 2,
                "zero_executed_nodes": [],
                "absent_nodes": []
            },
            "checks": 6,
            "gates_run": 6,
            "gates_expected": 6,
            "failures": 0,
            "real_seconds": 60.0,
            "log_file": "/tmp/reverie-validate.log",
            "admission": "ci-hub-validate-lock",
            "concurrent_validates": 0,
            "concurrency_proof": "validate_lock_owner_ancestry",
            "safe_ci": {
                "required": true,
                "observed": true,
                "fallback_allowed": false,
                "step": "validate.reverie-full",
                "cgroup": "/user.slice/safe-ci-123.scope/step-validate.reverie-full"
            },
            "validate_lock_authority": {
                "schema_version": 1,
                "admissible": true,
                "canonical_anchor_held": true,
                "holder": {"kind": "reverie-validate", "target": RECEIPT_SHA},
                "owner": {"liveness": "alive"}
            },
            "reverie_validation_policy": "reverie-local-validation/v2",
            "gates": gates
        })
    }

    fn history_row(value: serde_json::Value) -> HistoryRow {
        serde_json::from_value(value).expect("valid history fixture")
    }

    #[test]
    fn canonical_receipt_accepts_genuine_schema4_and_carries_its_conditions() {
        let row = history_row(schema4_receipt_value());
        let assessment =
            assess_canonical_receipts(Path::new("."), &[row], RECEIPT_SHA, CANONICAL_VALIDATE_REPO)
                .unwrap();
        assert_eq!(assessment.verdict, validate_status::Verdict::Validated);
        let receipt = newest_canonical_receipt(&assessment.qualifying).unwrap();
        assert_eq!(receipt.selected_tests, 786);
        assert_eq!(receipt.discovered_tests, 1479);
        assert_eq!(receipt.canonical_sha256.len(), 64);

        let report = describe_receipt(receipt);
        assert_eq!(report["repo"], CANONICAL_VALIDATE_REPO);
        assert_eq!(report["sha"], RECEIPT_SHA);
        assert_eq!(report["tree"], "f9294df9294df9294df9294df9294df9294df929");
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
    fn canonical_assessment_latches_a_genuine_same_commit_failure() {
        let pass = history_row(schema4_receipt_value());
        let mut fail = schema4_receipt_value();
        fail["finished_at"] = serde_json::json!("2026-08-01T12:00:30Z");
        fail["result"] = serde_json::json!("fail");
        fail["raw_result"] = serde_json::json!("fail");
        fail["exit_code"] = serde_json::json!(1);
        fail["failures"] = serde_json::json!(1);
        fail["dag_jobs"] = serde_json::json!(4);
        fail["concurrent_validates"] = serde_json::json!(0);
        fail["known_flaky_failure"] = serde_json::json!(false);
        fail["solo_rerun_confirmation"] = serde_json::json!(false);
        fail["gates"] = serde_json::json!([
            {
                "name": "fmt",
                "result": "pass",
                "exit_code": 0,
                "real_seconds": 5.0
            },
            {
                "name": "test",
                "result": "fail",
                "exit_code": 1,
                "real_seconds": 30.0,
                "failure_origin": "outer_gate",
                "failed_substeps": []
            }
        ]);
        let assessment = assess_canonical_receipts(
            Path::new("."),
            &[history_row(fail), pass],
            RECEIPT_SHA,
            CANONICAL_VALIDATE_REPO,
        )
        .unwrap();
        assert_eq!(assessment.verdict, validate_status::Verdict::FailedOnRecord);
        assert_eq!(assessment.failed_records, 1);
    }

    #[test]
    fn canonical_receipt_accounts_typed_empty_bucket_without_passing_it() {
        let mut value = schema4_receipt_value();
        value["gates_expected"] = serde_json::json!(3);
        value["skipped_nodes"] = serde_json::json!(1);
        value["intentional_skipped_nodes"] = serde_json::json!([{
            "name": "privileged-e2e.manifest_applications",
            "reason": "empty-manifest-bucket"
        }]);
        value["dependency_skipped_nodes"] = serde_json::json!([]);
        value["unaccounted_nodes"] = serde_json::json!([]);
        let receipt = qualify_canonical_receipt(&history_row(value), RECEIPT_SHA)
            .expect("exact typed empty bucket should preserve completeness");
        let report = describe_receipt(&receipt);
        assert_eq!(report["gates_run"], 2);
        assert_eq!(report["gates"].as_array().unwrap().len(), 2);
        assert_eq!(
            report["intentional_skipped_nodes"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            report["intentional_skipped_nodes"][0]["reason"],
            "empty-manifest-bucket"
        );
    }

    #[test]
    fn canonical_receipt_refuses_malformed_or_nonintentional_skips() {
        let typed = || {
            let mut value = schema4_receipt_value();
            value["gates_expected"] = serde_json::json!(3);
            value["skipped_nodes"] = serde_json::json!(1);
            value["intentional_skipped_nodes"] = serde_json::json!([{
                "name": "privileged-e2e.manifest_applications",
                "reason": "empty-manifest-bucket"
            }]);
            value["dependency_skipped_nodes"] = serde_json::json!([]);
            value["unaccounted_nodes"] = serde_json::json!([]);
            value
        };
        let mut unknown = typed();
        unknown["intentional_skipped_nodes"][0]["reason"] = serde_json::json!("looks-empty");
        assert!(qualify_canonical_receipt(&history_row(unknown), RECEIPT_SHA).is_none());

        let mut dependency = typed();
        dependency["dependency_skipped_nodes"] = serde_json::json!(["lost.node"]);
        assert!(qualify_canonical_receipt(&history_row(dependency), RECEIPT_SHA).is_none());

        let mut mismatched = typed();
        mismatched["skipped_nodes"] = serde_json::json!(0);
        assert!(qualify_canonical_receipt(&history_row(mismatched), RECEIPT_SHA).is_none());
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
            ("malformed tree", ("tree", serde_json::json!("unknown"))),
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

        let cross_repo = assess_canonical_receipts(
            Path::new("."),
            &[history_row(schema4_receipt_value())],
            RECEIPT_SHA,
            CANONICAL_REVERIE_REPO,
        )
        .unwrap();
        assert!(cross_repo.qualifying.is_empty());
    }

    #[test]
    fn reverie_receipt_requires_central_safe_ci_and_repo_binding() {
        let positive = history_row(reverie_receipt_value());
        let assessment = assess_canonical_receipts(
            Path::new("."),
            &[positive],
            RECEIPT_SHA,
            CANONICAL_REVERIE_REPO,
        )
        .unwrap();
        assert_eq!(assessment.verdict, validate_status::Verdict::Validated);
        assert_eq!(assessment.qualifying.len(), 1);

        let mut retired_seventh_gate = reverie_receipt_value();
        retired_seventh_gate["checks"] = serde_json::json!(7);
        retired_seventh_gate["gates_run"] = serde_json::json!(7);
        retired_seventh_gate["gates_expected"] = serde_json::json!(7);
        retired_seventh_gate["gates"]
            .as_array_mut()
            .unwrap()
            .insert(
                1,
                serde_json::json!({
                    "name": "Merge-gate policy",
                    "result": "pass",
                    "exit_code": 0
                }),
            );
        let mut legacy_seven_gate = retired_seventh_gate.clone();
        legacy_seven_gate["reverie_validation_policy"] =
            serde_json::json!("reverie-local-validation/v1");
        assert!(
            qualify_reverie_receipt(&history_row(legacy_seven_gate), RECEIPT_SHA).is_some(),
            "historical v1 seven-gate receipt stopped qualifying"
        );
        assert!(
            qualify_reverie_receipt(&history_row(retired_seventh_gate), RECEIPT_SHA).is_none(),
            "v2 receipt accepted the retired seventh gate"
        );

        let mut mixed_version = reverie_receipt_value();
        mixed_version["reverie_validation_policy"] =
            serde_json::json!("reverie-local-validation/v1");
        assert!(
            qualify_reverie_receipt(&history_row(mixed_version), RECEIPT_SHA).is_none(),
            "v1 policy accepted the v2 six-gate sequence"
        );

        let mut missing_gate = reverie_receipt_value();
        missing_gate["checks"] = serde_json::json!(5);
        missing_gate["gates_run"] = serde_json::json!(5);
        missing_gate["gates_expected"] = serde_json::json!(5);
        missing_gate["gates"].as_array_mut().unwrap().pop();
        assert!(
            qualify_reverie_receipt(&history_row(missing_gate), RECEIPT_SHA).is_none(),
            "v2 policy accepted a missing gate"
        );

        let mut reordered_gates = reverie_receipt_value();
        reordered_gates["gates"].as_array_mut().unwrap().swap(0, 1);
        assert!(
            qualify_reverie_receipt(&history_row(reordered_gates), RECEIPT_SHA).is_none(),
            "v2 policy accepted reordered gates"
        );

        let mut failed_gate = reverie_receipt_value();
        failed_gate["gates"][0]["result"] = serde_json::json!("fail");
        failed_gate["gates"][0]["exit_code"] = serde_json::json!(1);
        assert!(
            qualify_reverie_receipt(&history_row(failed_gate), RECEIPT_SHA).is_none(),
            "v2 policy accepted a failed gate"
        );

        let mut unknown_policy = reverie_receipt_value();
        unknown_policy["reverie_validation_policy"] =
            serde_json::json!("reverie-local-validation/v999");
        assert!(
            qualify_reverie_receipt(&history_row(unknown_policy), RECEIPT_SHA).is_none(),
            "unknown Reverie validation policy qualified"
        );

        for (name, path, replacement) in [
            ("malformed tree", vec!["tree"], serde_json::json!("unknown")),
            (
                "direct producer",
                vec!["producer"],
                serde_json::json!("reverie-validate-sh"),
            ),
            (
                "unobserved safe-ci",
                vec!["safe_ci", "observed"],
                serde_json::json!(false),
            ),
            (
                "unboxed fallback",
                vec!["safe_ci", "fallback_allowed"],
                serde_json::json!(true),
            ),
            (
                "missing test node",
                vec!["coverage", "absent_nodes"],
                serde_json::json!(["Documentation tests"]),
            ),
        ] {
            let mut planted = reverie_receipt_value();
            let mut cursor = &mut planted;
            for key in &path[..path.len() - 1] {
                cursor = &mut cursor[*key];
            }
            cursor[path[path.len() - 1]] = replacement;
            assert!(
                qualify_reverie_receipt(&history_row(planted), RECEIPT_SHA).is_none(),
                "planted {name} row qualified"
            );
        }
    }

    #[test]
    fn repository_filter_prevents_same_sha_cross_product_authority() {
        let hermit = history_row(schema4_receipt_value());
        let reverie = history_row(reverie_receipt_value());
        let rows = vec![hermit, reverie];
        let hermit_assessment =
            assess_canonical_receipts(Path::new("."), &rows, RECEIPT_SHA, CANONICAL_VALIDATE_REPO)
                .unwrap();
        let reverie_assessment =
            assess_canonical_receipts(Path::new("."), &rows, RECEIPT_SHA, CANONICAL_REVERIE_REPO)
                .unwrap();
        assert_eq!(hermit_assessment.qualifying.len(), 1);
        assert_eq!(reverie_assessment.qualifying.len(), 1);
        assert_eq!(hermit_assessment.qualifying[0].row.repo, None);
        assert_eq!(
            reverie_assessment.qualifying[0].row.repo.as_deref(),
            Some(CANONICAL_REVERIE_REPO)
        );
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

    fn rewrite_report_artifact(
        report: &mut serde_json::Value,
        artifact: &serde_json::Value,
        sha: &str,
    ) {
        let artifact_body = serde_json::to_string(&artifact).unwrap();
        let artifact_sha256 = format!("{:x}", Sha256::digest(artifact_body.as_bytes()));
        report["path"] = serde_json::json!(format!(
            "validation-receipts/{CANONICAL_VALIDATE_REPO}/{sha}/{artifact_sha256}.json"
        ));
        report["artifact_sha256"] = serde_json::json!(artifact_sha256);
        report["artifact_body"] = serde_json::json!(artifact_body);
    }

    fn rewrite_report_run_id(report: &mut serde_json::Value, run_id: &str, sha: &str) {
        let mut artifact: serde_json::Value =
            serde_json::from_str(report["artifact_body"].as_str().unwrap()).unwrap();
        artifact["run_id"] = serde_json::json!(run_id);
        rewrite_report_artifact(report, &artifact, sha);
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

        let mut missing_schema = publisher_report_value(&selected, true);
        let mut artifact: serde_json::Value =
            serde_json::from_str(missing_schema["artifact_body"].as_str().unwrap()).unwrap();
        artifact.as_object_mut().unwrap().remove("schema_version");
        rewrite_report_artifact(&mut missing_schema, &artifact, RECEIPT_SHA);
        let error = verify_publisher_report(
            serde_json::to_string(&missing_schema).unwrap().as_bytes(),
            CANONICAL_VALIDATE_REPO,
            RECEIPT_SHA,
            &selected,
            true,
        )
        .unwrap_err();
        assert!(error.contains("missing field `schema_version`"), "{error}");

        let mut wrong_source_log = publisher_report_value(&selected, true);
        let mut artifact: serde_json::Value =
            serde_json::from_str(wrong_source_log["artifact_body"].as_str().unwrap()).unwrap();
        artifact["source_log_file"] = serde_json::json!("/tmp/other.log");
        rewrite_report_artifact(&mut wrong_source_log, &artifact, RECEIPT_SHA);
        let error = verify_publisher_report(
            serde_json::to_string(&wrong_source_log).unwrap().as_bytes(),
            CANONICAL_VALIDATE_REPO,
            RECEIPT_SHA,
            &selected,
            true,
        )
        .unwrap_err();
        assert!(error.contains("source_log_file differs"), "{error}");
    }

    #[test]
    fn parses_qualified_ledger_rows_command() {
        let cli = Cli::try_parse_from(["ci-hub", "ledger", "qualified-rows"]).unwrap();
        let HubCommand::Ledger(args) = cli.command else {
            panic!("wrong command variant")
        };
        let LedgerCommand::QualifiedRows(_) = args.command else {
            panic!("wrong ledger subcommand variant")
        };
    }

    #[test]
    fn parses_attribute_reds_command() {
        let cli = Cli::try_parse_from([
            "ci-hub",
            "ledger",
            "attribute-reds",
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
    fn validate_status_without_a_target_lists_recent_runs() {
        let cli = Cli::try_parse_from(["ci-hub", "validate-status"]).unwrap();
        let HubCommand::ValidateStatus(args) = cli.command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.sha_positional, None);
        assert_eq!(args.sha, None);
        assert_eq!(args.pr, None);
        assert_eq!(args.limit, DEFAULT_VALIDATE_HISTORY_LIMIT);
        assert_eq!(args.columns, [ValidateStatusColumn::Sha]);
        assert_eq!(args.times, ValidateStatusTimes::Relative);
        assert!(!args.exclude_in_progress);
        assert!(!args.in_progress_only);

        for (value, expected) in [
            ("relative", ValidateStatusTimes::Relative),
            ("local", ValidateStatusTimes::Local),
            ("utc", ValidateStatusTimes::Utc),
        ] {
            let cli = Cli::try_parse_from(["ci-hub", "validate-status", "--times", value]).unwrap();
            let HubCommand::ValidateStatus(args) = cli.command else {
                panic!("wrong command variant")
            };
            assert_eq!(args.times, expected);
        }
        assert!(Cli::try_parse_from(["ci-hub", "validate-status", "--times", "wall"]).is_err());

        let cli = Cli::try_parse_from([
            "ci-hub",
            "validate-status",
            "--limit",
            "3",
            "--columns",
            "branch,gitdepth",
            "--exclude-in-progress",
        ])
        .unwrap();
        let HubCommand::ValidateStatus(args) = cli.command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.limit, 3);
        assert_eq!(
            args.columns,
            [ValidateStatusColumn::Branch, ValidateStatusColumn::GitDepth]
        );
        assert!(args.exclude_in_progress);
        assert!(Cli::try_parse_from([
            "ci-hub",
            "validate-status",
            "--exclude-in-progress",
            "--in-progress-only",
        ])
        .is_err());
        assert!(selected_validate_status_columns(&[
            ValidateStatusColumn::None,
            ValidateStatusColumn::Sha,
        ])
        .is_err());
    }

    #[test]
    fn local_history_defaults_to_recent_and_requires_all_for_the_full_view() {
        let cli = Cli::try_parse_from(["ci-hub", "local-history"]).unwrap();
        let HubCommand::LocalHistory(args) = cli.command else {
            panic!("wrong command variant")
        };
        assert_eq!(args.limit, DEFAULT_VALIDATE_HISTORY_LIMIT);
        assert!(!args.all);

        let cli = Cli::try_parse_from(["ci-hub", "local-history", "--all"]).unwrap();
        let HubCommand::LocalHistory(args) = cli.command else {
            panic!("wrong command variant")
        };
        assert!(args.all);
        assert!(Cli::try_parse_from(["ci-hub", "local-history", "--all", "--limit", "5"]).is_err());
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
    fn validate_run_places_canonical_state_root_before_payload_arguments() {
        let root = Path::new("/not-a-repository/dev-hermit-worktree");
        let forwarded = validate_run_forwarded_args(
            root,
            vec![
                OsString::from("--checkout"),
                OsString::from("/tmp/hermit"),
                OsString::from("--state-root"),
                OsString::from("/caller/cannot/override"),
                OsString::from("--"),
                OsString::from("full"),
            ],
        );
        let separator = forwarded
            .iter()
            .position(|argument| argument == "--")
            .expect("payload separator");
        assert_eq!(
            &forwarded[separator - 2..separator],
            [
                OsString::from("--state-root"),
                root.as_os_str().to_os_string(),
            ]
        );
        assert_eq!(forwarded[separator + 1], OsString::from("full"));
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
            "READ-ONLY STATUS AND HISTORY",
            "VALIDATION ACTIONS",
            "LANDING AND COORDINATION ACTIONS",
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
        assert!(help.contains("local-history           List recent local validation runs"));
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
        assert!(AGENT_QUICKSTART.contains("newest-green"));
        assert!(AGENT_QUICKSTART.contains("first-bad CELL_OR_GATE"));
        assert!(AGENT_QUICKSTART.contains("land-lock run"));
        assert!(AGENT_QUICKSTART.contains("validate-run --checkout"));
        assert!(AGENT_QUICKSTART.contains("apply-local-label --pr"));
        assert!(AGENT_QUICKSTART.contains("landing/land-pr.sh"));
        assert!(AGENT_QUICKSTART.contains("hermit-validation-authority"));
        assert!(!AGENT_QUICKSTART.contains("modifying scripts/validate.rs"));
        assert!(AGENT_QUICKSTART.lines().count() < 45);
    }

    #[test]
    fn review_attest_family_outcome_and_critical_class_are_closed_enums() {
        let command = Cli::try_parse_from([
            "ci-hub",
            "review-attest",
            "--pr",
            "42",
            "--head",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--family",
            "codex",
            "--outcome",
            "refusal",
            "--comment-url",
            "https://github.com/rrnewton/hermit/pull/42#issuecomment-1234",
            "--reviewer",
            "codex-2",
            "--task",
            "review-pr-42",
            "--critical",
            "hermit-syscall",
            "--who",
            "hermit-coord",
            "--team",
            "hermit2",
        ])
        .unwrap()
        .command;
        assert!(matches!(&command, HubCommand::ReviewAttest(_)));
        assert!(command.cost_spec().is_some());

        let family_error = Cli::try_parse_from([
            "ci-hub",
            "review-attest",
            "--pr",
            "42",
            "--head",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--family",
            "codxe",
            "--outcome",
            "approval",
            "--comment-url",
            "https://github.com/rrnewton/hermit/pull/42#issuecomment-1234",
            "--reviewer",
            "codex-2",
            "--task",
            "review-pr-42",
            "--who",
            "hermit-coord",
            "--team",
            "hermit2",
        ])
        .unwrap_err();
        assert_eq!(family_error.kind(), ErrorKind::InvalidValue);

        let outcome_error = Cli::try_parse_from([
            "ci-hub",
            "review-attest",
            "--pr",
            "42",
            "--head",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--family",
            "codex",
            "--outcome",
            "unknown",
            "--comment-url",
            "https://github.com/rrnewton/hermit/pull/42#issuecomment-1234",
            "--reviewer",
            "codex-2",
            "--task",
            "review-pr-42",
            "--who",
            "hermit-coord",
            "--team",
            "hermit2",
        ])
        .unwrap_err();
        assert_eq!(outcome_error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn trivial_reads_have_no_cost_wrapper() {
        let status = Cli::try_parse_from(["ci-hub", "land-lock", "status"])
            .unwrap()
            .command;
        assert!(status.cost_spec().is_none());
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
