//! The SHA-queryable local-receipt/cache authority over the validate-run ledger.
//!
//! This is the ONE shared local artifact behind two owner P0s (2026-08-03):
//!   * `validate-result-cache-by-sha` CHECKS it (skip a validate whose clean
//!     HEAD already has a pass record), and
//!   * the landing authority combiner READS it as the counted-local positive.
//!     The independently dereferenced hosted job set is the peer positive; this
//!     module makes no GitHub calls and does not define the total landing rule.
//!
//! One store, one predicate: Hermit's `scripts/validate.rs` and Reverie's
//! `validate.sh` WRITE the JSONL ledger, this module READS it. There is
//! deliberately no second table.
//!
//! ## The predicate (VALIDATED) — version-aware over a schema transition
//!
//! The full rule is "a green must carry what it verified" (AGENTS.md Proxy
//! Binding, parent da98bdd): a clean, commit-anchored, FULL-profile,
//! FULL-selection PASS with a nonzero executed-test count AND zero filtered-out.
//! But the PRODUCER TRAVELS WITH THE BRANCH: a receipt's writer is whatever
//! validation producer shipped on that PR's branch, and older writers do not emit the
//! count fields at all. Enforcing the count clauses on EVERY receipt therefore
//! strands every pre-count receipt (measured 2026-08-04: 35/35 clean full passes
//! in the ledger flip to NOT-VALIDATED), identically to the bfb0a9ef anchor
//! transition. So this consumer is VERSION-AWARE: it applies the rule that
//! matches each receipt's declared capability.
//!
//! A record satisfies the predicate for a commit iff it is clean,
//! commit-anchored, FULL-profile, FULL-selection, `result == "pass"`, AND:
//!   * it passes ONE UNIVERSAL guard that holds at every schema — `executed_tests
//!     != Some(0)` (a demonstrated zero-test run is a no-result, never green).
//!     There is NO `filtered_tests` guard: a real full run legitimately filters
//!     hundreds of other-shard tests, so `filtered_tests == 0` rejected every real
//!     full green — it is DELETED and superseded by the per-node `coverage`
//!     obligation, which distinguishes legitimate full-run filtering from a
//!     narrowed-subset masquerade; AND
//!   * IF the receipt is count-capable (`schema_version >= COUNTS_SCHEMA`, i.e.
//!     written by a coverage-emitting writer) it is held to the FULL per-node
//!     contract: `executed_tests == Some(n>0)` AND a `coverage` object that
//!     satisfies its obligation (a planned test node set, none inert, none
//!     absent). Absent counts OR absent coverage from a count-capable writer is a
//!     writer DEFECT, refused.
//!   * ELSE IF it carries both counts under an old schema (`aggregate.py`) it
//!     predates per-node coverage but is held to `executed_tests == Some(n>0)`.
//!   * ELSE it carries NEITHER count — a genuinely pre-count receipt, or a
//!     producer that failed to emit counts. It proves neither nonzero execution
//!     nor bounded filtering, so it is NotValidated (an uncounted receipt is
//!     UNVERIFIED, not green). The former transition grandfather (accept an
//!     uncounted clean/full/full/pass) is REMOVED.
//!
//! Why the grandfather is gone: it was a bounded transition allowance that
//! un-broke the drain while no count-backed greens existed. That precondition is
//! now met — the backlog is backfilled to schema-5 by `finalize_receipt.py
//! --scan` and every landing consume-path re-mints count-backed rows from each
//! green's durable log before reading the ledger (see `ci-hub/validate/scan-finalize.sh`,
//! wired into `ci-hub/landing/{land-pr,parallel-prevalidate}.sh`). Measured on the
//! live ledger: post-backfill the removal strands 0 legitimate greens; the sole
//! refusal (ee303899) is a genuine per-node coverage fail it SHOULD refuse. See
//! `ai_docs/transition-design-executed-filtered-count-schema-tightening_20260804.md`.
//! Keying strictness on field PRESENCE (not the `schema_version` integer alone) is
//! deliberate: the live writers disagree about the version — `aggregate.py` emits
//! counts under schema 1 while schema 3 and 4 exist WITHOUT counts — so the
//! condition ("this receipt measured coverage") must travel with the value as the
//! fields themselves; the schema escalator exists only to catch a count-capable
//! writer that emits nothing.
//!
//! This is intentionally NEVER LOOSER than `hermit/scripts/validate.rs`'s own
//! `locally-validated` stamp guard, which fires only when
//! `VALIDATION_LEVEL == full && failures == 0` on a commit-anchored, non-dirty,
//! non-subset run. `profile` is the selected validation level unless a subset
//! mode overrode it, so requiring both `profile == "full"`
//! and `selection_mode == "full"` reproduces "level==full AND not a subset run".
//! A `quick`/`super`/`portable-only` level, or any `--only`/`--selective`/
//! `*-only` subset run, is fail-closed rejected: a subset must never masquerade
//! as full coverage. `commit_anchored`/`tree_dirty` guarantee the record
//! describes the actual commit, not a dirty tree (the tree is not the commit).

use crate::records::HistoryRow;
use std::collections::BTreeSet;

/// Logical canonical ledger root relative to the workspace. Consumers never
/// open a receipt file beneath it directly: `ci-hub/ledger/validate_rows.py`
/// unions every published team/machine/month shard plus the one deterministic
/// live machine journal and emits the compatibility HistoryRow view.
pub const LEDGER_REL: &str = "ledger";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// At least one clean full-coverage PASS record for the commit.
    Validated,
    /// No PASS, but a clean full-coverage record that FAILED/killed/timed out —
    /// a known-bad commit, distinct and more informative than "no record".
    FailedOnRecord,
    /// The run ended before all planned gates completed. Nothing observed
    /// failed, so this is an absence of a result and must be re-dispatched.
    Truncated,
    /// A red exists, but its execution conditions are missing, contended, or
    /// known-flaky and lack the required solo `-j 4` confirmation.
    NeedsRerun,
    /// A red whose gates could not run at all (an ENVIRONMENT fault, not a
    /// product defect): a command-not-found storm (all-or-most failing gates at
    /// exit 127) or a sub-second collapse where every gate failed. Nothing about
    /// the product was actually exercised, so it is re-runnable, never FAILED.
    NoResult,
    /// No qualifying record: none at all, or only dirty/subset/unanchored runs.
    NotValidated,
}

impl Verdict {
    /// Exit codes are stable so shell callers (`land-pr.sh`, the cache) can gate
    /// on them: 0 = land/skip-validate, 3 = known-failing, 4 = must validate.
    pub fn exit_code(self) -> i32 {
        match self {
            Verdict::Validated => 0,
            Verdict::FailedOnRecord => 3,
            Verdict::Truncated
            | Verdict::NeedsRerun
            | Verdict::NoResult
            | Verdict::NotValidated => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Validated => "VALIDATED",
            Verdict::FailedOnRecord => "FAILED",
            Verdict::Truncated => "TRUNCATED",
            Verdict::NeedsRerun => "NEEDS-RERUN",
            Verdict::NoResult => "NO-RESULT",
            Verdict::NotValidated => "NOT-VALIDATED",
        }
    }
}

fn field_bool(row: &HistoryRow, key: &str) -> Option<bool> {
    match key {
        "commit_anchored" => row.commit_anchored,
        "tree_dirty" => row.tree_dirty,
        _ => row.extra.get(key).and_then(|value| value.as_bool()),
    }
}

fn field_str<'a>(row: &'a HistoryRow, key: &str) -> Option<&'a str> {
    match key {
        "selection_mode" => row.selection_mode.as_deref(),
        _ => row.extra.get(key).and_then(|value| value.as_str()),
    }
}

/// A record that is clean, commit-anchored, full-profile, full-selection — the
/// coverage prerequisite shared by the PASS and the known-FAIL cases.
fn is_clean_full_coverage(row: &HistoryRow, sha: &str) -> bool {
    row.commit.as_deref() == Some(sha)
        && field_bool(row, "commit_anchored") == Some(true)
        && field_bool(row, "tree_dirty") == Some(false)
        && field_str(row, "selection_mode") == Some("full")
        && row.profile.as_deref() == Some("full")
}

/// The `schema_version` at which a receipt's WRITER guarantees it emits both
/// count fields. A receipt stamped `>= COUNTS_SCHEMA` is held to the STRICT rule
/// even when a count is absent, because for such a writer an absent count is a
/// DEFECT (it was contractually required to emit), not a pre-count receipt.
///
/// The PRODUCTION source of truth for this boundary is now
/// `qualifying-receipt.json` (`counts_schema`), which the shared predicate reads;
/// the production predicate no longer keys on this const. It is retained ONLY as
/// a test-fixture convenience and is PINNED to the JSON by
/// `counts_schema_const_matches_shared_predicate`, so it cannot silently drift.
///
/// Why 5, not 4: schema 1/2/3 are already in use and schema 4 is ALREADY in the
/// ledger from a branch writer that emits NO counts, so 4 cannot mean "counts
/// present". 5 is the first clean anchor.
///
/// `#[cfg(test)]`: production no longer reads this const (it reads the JSON via
/// the shared predicate), so it exists ONLY for the test fixtures below and the
/// pin `counts_schema_const_matches_shared_predicate`. Compiling it into the
/// binary would be dead code (the `-D warnings` clippy gate would reject it).
#[cfg(test)]
pub const COUNTS_SCHEMA: u32 = 5;

/// A schema-3+ full-profile run's known gate contract is five gates. When the
/// producer has not yet written `gates_expected` (it predates that field), a
/// full row of schema >= 3 still has this expected count. Mirrors the Python
/// `flake_class.gate_counts` fallback so both engines apply the SAME
/// completeness rule (the canonical guard for validate_ledger_qualified_rows).
pub const FULL_GATES_EXPECTED: u64 = 5;

/// The landing / cache predicate — version-aware over the count-schema
/// transition (see the module docs and the transition design note). Over and
/// above clean full coverage (`is_clean_full_coverage`) and `result == "pass"`:
///
///   * ONE UNIVERSAL GUARD, applied at EVERY schema (grandfather or strict):
///     `executed_tests != Some(0)` — a demonstrated zero-test run is a no-result
///     wearing a success badge (the `--features`-gated build that compiled the
///     tests out), never a green, and never grandfathered.
///   * There is NO `filtered_tests` guard. A real full run legitimately filters
///     hundreds of other-lane/other-shard tests (~693 measured), so a positive
///     aggregate filtered count says NOTHING about coverage; the blunt
///     `filtered_tests == 0` predicate rejected every real full green and is
///     DELETED. The narrowed-subset masquerade it tried to catch is caught
///     precisely by the PER-NODE `coverage` obligation below (a subset run leaves
///     required nodes absent or inert), and by the profile/selection full-coverage
///     gates upstream. `filtered_tests` is retained only as a diagnostic.
///   * THEN, if the receipt is COUNT-CAPABLE (`schema_version >= COUNTS_SCHEMA`),
///     it is held to the FULL per-node contract: `executed_tests == Some(n>0)`
///     AND a `coverage` object that SATISFIES its obligation. A count-capable
///     receipt that omits `coverage` (or the counts) is a writer DEFECT and is
///     rejected fail-closed — it was contractually required to emit them.
///   * ELSE if the receipt CARRIES both counts under an old schema
///     (`aggregate.py`, schema 1), it predates per-node coverage but can still
///     prove nonzero execution: held to `executed_tests == Some(n>0)`. (It cannot
///     be required to carry `coverage` it never emitted.)
///   * ELSE the receipt carries NEITHER count (a genuinely pre-count receipt, or
///     a producer that failed to emit them): it proves neither nonzero execution
///     nor bounded filtering and is NotValidated. The former transition
///     grandfather (accept-on-absence) is REMOVED now that the backlog is
///     backfilled to schema-5 and every landing consume-path re-mints count-backed
///     rows before reading the ledger, so no legitimate green rides this branch.
///
/// Everything short of the applicable rule is NotValidated (exit 4 = re-dispatch),
/// never FailedOnRecord — an uncounted or under-covered run is UNVERIFIED, not
/// known-bad. This AGREES with the AGENTS.md Proxy Binding truth table (parent
/// da98bdd): an evidence value must carry the condition it claims, so a receipt
/// that carries no count cannot certify execution and is refused.
pub fn is_clean_full_pass(row: &HistoryRow, sha: &str) -> bool {
    // The predicate itself is NOT restated here — it is the ONE shared
    // qualifying-receipt predicate loaded from `ci-hub/validate/qualifying-receipt.json`
    // (module `qualifying_receipt`), which every consumer across Rust/Python/jq
    // reads. Restating the clauses inline is exactly the drift this delegation
    // removes (task
    // `one-shared-qualifying-receipt-predicate-five-consumers-bypass-the-registry`).
    // The doc block above records the SEMANTICS the shared predicate implements.
    crate::qualifying_receipt::row_qualifies(row, sha, crate::qualifying_receipt::active())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureDisposition {
    NotFailure,
    Truncated,
    NeedsRerun,
    NoResult,
    Failed,
}

fn gate_is_red(gate: &crate::records::GateHistoryRow) -> bool {
    gate.extra
        .get("aborted")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
        && (matches!(gate.result.as_deref(), Some("fail" | "failed" | "timeout"))
            || matches!(gate.kind.as_deref(), Some("fail" | "failed" | "timeout")))
}

/// The shell's exit code for "command not found" — a gate at this code never ran
/// the tool it wraps, so it exercised nothing about the product.
const EXIT_COMMAND_NOT_FOUND: i32 = 127;

/// The observable signature of an ENVIRONMENT fault rather than a product defect:
/// the gate commands could not run at all, so a red carries no information about
/// the commit. Bound to values the row itself carries (Proxy Binding — classify
/// on the observed fault, not on the incidental absence of condition fields, so
/// the verdict survives the producer starting to emit `dag_jobs`/conditions).
/// Two independent, each-sufficient tells:
///
///   (A) COMMAND-NOT-FOUND STORM: at least one failing gate is exit 127 AND no
///       failing gate is a genuine product failure (a red gate that actually ran
///       for >0s at a non-127 exit). A real test/assertion red is exit 1/101
///       after real execution — never 127 — so a genuine defect can never be
///       laundered here. LIVE: a1493427 recorded fail at 1s with five gates at
///       exit 127; the SAME commit passed 6/6 at 58s.
///   (B) SUB-SECOND COLLAPSE: the whole run's wall is <= 1s AND every gate that
///       produced a result failed (no gate passed) — the run died before doing
///       any real work. A genuine build+test red cannot complete this fast.
fn is_env_fault_red(row: &HistoryRow, failed_gates: &[&crate::records::GateHistoryRow]) -> bool {
    if failed_gates.is_empty() {
        return false;
    }
    // (A) A gate that genuinely exercised the product and failed disqualifies the
    // env-fault reading: it ran for real time at a non-command-not-found exit.
    let any_genuine_red = failed_gates.iter().any(|gate| {
        gate.exit_code != Some(EXIT_COMMAND_NOT_FOUND) && gate.real_seconds.is_some_and(|s| s > 0.0)
    });
    let any_command_not_found = failed_gates
        .iter()
        .any(|gate| gate.exit_code == Some(EXIT_COMMAND_NOT_FOUND));
    let command_not_found_storm = any_command_not_found && !any_genuine_red;

    // (B) Sub-second wall with no passing gate. `real_seconds` is the whole run's
    // wall; a run this short cannot have built and tested the product.
    let subsecond_collapse = row.real_seconds.is_some_and(|s| s <= 1.0)
        && !row.gates.is_empty()
        && row.gates.iter().all(gate_is_red);

    command_not_found_storm || subsecond_collapse
}

/// Interpret one red ledger row using the conditions carried with it.
///
/// A bare `result=fail` is deliberately insufficient. Durable FAILED requires
/// a complete outer-gate run, a real failing gate with its origin recorded, and
/// the width/concurrency/flake conditions. A contended or known-flaky red is a
/// rerun request until a solo `-j 4` confirmation exists.
pub fn failure_disposition(row: &HistoryRow, sha: &str) -> FailureDisposition {
    // Schema-4+ producers carry non-verdict outcomes explicitly. Read them
    // before the legacy fail inference so first-class TRUNCATED/NEEDS-RERUN
    // records remain observable even though they intentionally cannot satisfy
    // the clean full-pass predicate.
    match row.result.as_deref() {
        Some("truncated") => return FailureDisposition::Truncated,
        Some("needs-rerun" | "needs_rerun") => return FailureDisposition::NeedsRerun,
        _ => {}
    }
    if !is_clean_full_coverage(row, sha)
        || !matches!(row.result.as_deref(), Some("fail" | "failed" | "timeout"))
    {
        return FailureDisposition::NotFailure;
    }

    let gates_run = row.gates_run.or(row.checks);
    // Apply the schema-aware full-profile fallback so an anchored full run whose
    // producer predates `gates_expected` still carries its five-gate contract —
    // identical to Python `gate_counts`, so the two engines never disagree on
    // completeness. `is_clean_full_coverage` above already guarantees
    // profile == "full" here; the schema guard keeps the count off legacy rows.
    let gates_expected = row.gates_expected.or_else(|| {
        (row.profile.as_deref() == Some("full") && row.schema_version.is_some_and(|v| v >= 3))
            .then_some(FULL_GATES_EXPECTED)
    });
    let failed_gates: Vec<_> = row.gates.iter().filter(|gate| gate_is_red(gate)).collect();

    // An environment fault (command-not-found storm, sub-second collapse) is
    // checked FIRST: its gates could not run, so the red carries no information
    // about the commit. This is the most specific reading and must win over the
    // completeness/condition logic below — otherwise, once the producer emits
    // conditions, a 127-storm with full profile + bound origin would launder
    // into a durable FAILED. It is re-runnable, never FAILED.
    if is_env_fault_red(row, &failed_gates) {
        return FailureDisposition::NoResult;
    }

    // Completeness is structural. Even when a fail-fast row carries a real red
    // gate, it did not execute the full validation contract and cannot become a
    // durable FAILED verdict. A genuine red control SATISFIES its gate contract
    // (`executed + exact intentional skips == expected`, matching the shared
    // accounting check below): only an under-accounted run is a truncation. An OVER-run (`ran > expected`, e.g. six
    // live gates against a hardcoded five-gate expectation) is complete, not short,
    // and must fall through to the failure logic — a stale expected count must not
    // launder a genuine over-run failure into a non-verdict.
    let has_real_failure = row.failures.is_some_and(|f| f >= 1) || !failed_gates.is_empty();
    let gates_accounted = if row.gates_expected.is_some() {
        crate::qualifying_receipt::gate_accounting_complete(row)
    } else {
        gates_expected
            .zip(gates_run)
            .is_some_and(|(expected, ran)| expected > 0 && ran >= expected)
    };
    if gates_expected.is_some_and(|expected| expected > 0)
        && !gates_accounted
        || (!has_real_failure
            && (row.exit_code == Some(130) || (row.failures == Some(0) && failed_gates.is_empty())))
    {
        return FailureDisposition::Truncated;
    }

    // No named failing gate means no observable defect to condemn. `executed_tests`
    // was refuted as a proxy in BOTH directions — a build/clippy red exercises zero
    // tests yet is a genuine defect, and a high-count run can still be a no-result —
    // so the verdict binds to the NAMED GATE authority (gates[] + exit_code), not a
    // count. This is checked AFTER the env-fault and truncation readings (each a
    // more specific "why nothing ran") and BEFORE the completeness/condition/flake
    // logic. When neither a named failing gate nor a failure count is present, the
    // red carries no durable evidence: NO-RESULT (exit 4, re-dispatch), never the
    // permanent FAILED that condemns a PR nobody re-runs. `executed_tests` remains
    // in the record for diagnostics only; it is no longer a classification key.
    if !has_real_failure {
        return FailureDisposition::NoResult;
    }

    // Missing completeness or execution conditions cannot prove a defect. Old
    // reds therefore become re-measurement requests instead of permanent
    // condemnations when the schema tightens.
    let complete = if row.gates_expected.is_some() {
        crate::qualifying_receipt::gate_accounting_complete(row)
    } else {
        gates_expected
            .zip(gates_run)
            .is_some_and(|(expected, ran)| expected > 0 && ran >= expected)
    };
    let conditions_present = row.dag_jobs.is_some()
        && row.concurrent_validates.is_some()
        && row.known_flaky_failure.is_some();
    let origin_bound = !failed_gates.is_empty()
        && failed_gates
            .iter()
            .all(|gate| match gate.failure_origin.as_deref() {
                Some("outer_gate") => true,
                Some("lane_substep") => !gate.failed_substeps.is_empty(),
                _ => false,
            });
    if !complete || !conditions_present || !origin_bound {
        return FailureDisposition::NeedsRerun;
    }

    // The measured #1592 false-red bracket was -j16 FAIL versus solo -j4 PASS.
    // Width itself creates intra-run Cargo package/build-directory contention;
    // it does not require a second validate process. Therefore either observed
    // peer overlap OR width above the measured safe rerun width needs remeasure.
    let contended = row.concurrent_validates.is_some_and(|count| count > 0)
        || row.dag_jobs.is_some_and(|jobs| jobs > 4);
    let flaky = row.known_flaky_failure == Some(true);
    if contended || flaky {
        let confirmed = row.solo_rerun_confirmation == Some(true)
            && row.dag_jobs == Some(4)
            && row.concurrent_validates == Some(0);
        if !confirmed {
            return FailureDisposition::NeedsRerun;
        }
    }
    FailureDisposition::Failed
}

/// The outcome of assessing a commit against the ledger.
pub struct Assessment {
    pub sha: String,
    pub verdict: Verdict,
    /// Records that satisfy the predicate (clean full-coverage PASS).
    pub qualifying: Vec<HistoryRow>,
    /// Every other record for the same commit (subset, dirty, failed, ...),
    /// retained so callers can explain WHY a commit is NOT validated.
    pub disqualified: Vec<HistoryRow>,
    /// How many records for this commit are GENUINE clean full-coverage
    /// FAILURES — i.e. [`FailureDisposition::Failed`], which already excludes
    /// subset/dirty runs, truncated and needs-rerun records, and environment
    /// faults (command-not-found storms, sub-second collapses).
    ///
    /// The ledger does NOT latch: a later clean full PASS supersedes an earlier
    /// clean full FAIL and the verdict flips to VALIDATED. That is correct — but
    /// a VALIDATED banner that says nothing about the superseded failure claims
    /// more than it verified. The same commit both passing and failing a clean
    /// full run is a FLAKE SIGNAL, and it is exactly the evidence a green would
    /// otherwise bury. Counted here so the print path can surface it rather than
    /// each caller re-deriving it (and re-deriving it differently).
    ///
    /// This is a raw count, not a judgement: whether these failures are
    /// "superseded" depends on the verdict, which is the caller's to read.
    pub failed_records: usize,
    /// Clean full-coverage NON-PASS records that the classifier deliberately
    /// WITHHELD from [`Self::failed_records`] — contended reds, reds whose
    /// producer predates the solo-execution condition fields, truncated runs,
    /// and environment faults. Each carries no product verdict on its own, so
    /// withholding them from the failure count is correct.
    ///
    /// They are counted separately because withholding them from the COUNT is
    /// not the same as withholding them from the OPERATOR. Measured on the live
    /// ledger: 20 of 105 VALIDATED commits carry at least one same-commit
    /// `result: "fail"` record, and every one of those failures is withheld —
    /// so a banner that reports only `failed_records` says nothing at all about
    /// any of them. "1 non-pass record (withheld: contended)" and silence are
    /// very different claims.
    pub withheld_nonpass_records: usize,
}

/// Assess a single commit against all ledger rows. `sha` must be a full 40-hex
/// commit (resolve prefixes with [`resolve_sha`] first).
pub fn assess(rows: &[HistoryRow], sha: &str) -> Assessment {
    let mut qualifying = Vec::new();
    let mut disqualified = Vec::new();
    let mut failed_records = 0usize;
    let mut withheld_nonpass_records = 0usize;
    let mut saw_failed = false;
    let mut saw_needs_rerun = false;
    let mut saw_truncated = false;
    let mut saw_no_result = false;
    for row in rows {
        if row.commit.as_deref() != Some(sha) {
            continue;
        }
        if is_clean_full_pass(row, sha) {
            qualifying.push(row.clone());
        } else {
            match failure_disposition(row, sha) {
                FailureDisposition::Failed => {
                    saw_failed = true;
                    failed_records += 1;
                }
                FailureDisposition::NeedsRerun => {
                    saw_needs_rerun = true;
                    if is_clean_full_coverage(row, sha) {
                        withheld_nonpass_records += 1;
                    }
                }
                FailureDisposition::Truncated => {
                    saw_truncated = true;
                    if is_clean_full_coverage(row, sha) {
                        withheld_nonpass_records += 1;
                    }
                }
                FailureDisposition::NoResult => {
                    saw_no_result = true;
                    if is_clean_full_coverage(row, sha) {
                        withheld_nonpass_records += 1;
                    }
                }
                FailureDisposition::NotFailure => {}
            }
            disqualified.push(row.clone());
        }
    }
    let verdict = if !qualifying.is_empty() {
        Verdict::Validated
    } else if saw_failed {
        Verdict::FailedOnRecord
    } else if saw_needs_rerun {
        Verdict::NeedsRerun
    } else if saw_truncated {
        Verdict::Truncated
    } else if saw_no_result {
        Verdict::NoResult
    } else {
        Verdict::NotValidated
    };
    Assessment {
        sha: sha.to_string(),
        verdict,
        qualifying,
        disqualified,
        failed_records,
        withheld_nonpass_records,
    }
}

/// Pick the most recent qualifying record (by `finished_at` string order, which
/// is ISO-8601-Z and therefore lexicographically chronological).
pub fn newest(rows: &[HistoryRow]) -> Option<&HistoryRow> {
    rows.iter().max_by(|a, b| {
        a.finished_at
            .as_deref()
            .unwrap_or("")
            .cmp(b.finished_at.as_deref().unwrap_or(""))
    })
}

/// Expand a commit input to a full 40-hex commit using the ledger's own
/// commits. A full hex input is returned lowercased unchanged (no ledger lookup
/// needed). A shorter prefix must resolve to exactly one distinct commit.
pub fn resolve_sha(rows: &[HistoryRow], input: &str) -> Result<String, String> {
    let lc = input.to_ascii_lowercase();
    let is_hex = !lc.is_empty() && lc.chars().all(|c| c.is_ascii_hexdigit());
    if !is_hex {
        return Err(format!("'{input}' is not a hex commit or prefix"));
    }
    if lc.len() == 40 {
        return Ok(lc);
    }
    if lc.len() > 40 {
        return Err(format!("'{input}' is longer than a 40-hex commit"));
    }
    let mut distinct: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        if let Some(commit) = row.commit.as_deref() {
            if commit.starts_with(&lc) {
                distinct.insert(commit.to_string());
            }
        }
    }
    match distinct.len() {
        1 => Ok(distinct.into_iter().next().expect("len==1")),
        0 => Err(format!("no ledger record has a commit matching '{input}'")),
        n => Err(format!(
            "commit prefix '{input}' is ambiguous across {n} distinct ledger commits"
        )),
    }
}

/// Parse an append-only validate-runs JSONL buffer into typed rows. Blank lines
/// are skipped. A line that fails to parse is skipped and COUNTED (returned as
/// the second element) rather than aborting: the ledger is appended by many
/// writers and one malformed line must never blind the query to every other
/// record. Fail-closed at the predicate, tolerant at the parser.
pub fn parse_ledger(buf: &str) -> (Vec<HistoryRow>, usize) {
    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for line in buf.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<HistoryRow>(line) {
            Ok(row) => rows.push(row),
            Err(_) => skipped += 1,
        }
    }
    (rows, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::CoverageRow;

    const PASS_SHA: &str = "cde3c1195eee4e2691bac64a4aec10a45aba853e";
    const OTHER_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn row(json: &str) -> HistoryRow {
        serde_json::from_str(json).expect("valid HistoryRow json")
    }

    /// The canonical LEGITIMATE full green: clean, commit-anchored, full/full,
    /// `result==pass`, and — as a green must — CARRYING its counts: a nonzero
    /// executed count and zero filtered-out. Negative tests override one field to
    /// prove that field alone is disqualifying.
    fn clean_full_pass(sha: &str) -> HistoryRow {
        row(&format!(
            r#"{{"schema_version":3,"finished_at":"2026-08-03T19:08:57Z","host":"test-runner.example",
                "profile":"full","selection_mode":"full","commit":"{sha}",
                "commit_anchored":true,"tree_dirty":false,"result":"pass",
                "executed_tests":36,"filtered_tests":0,
                "producer":"hermit-validate-sh","admission":"ci-hub-validate-lock",
                "concurrent_validates":0,
                "concurrency_proof":"validate_lock_owner_ancestry",
                "base_sha":"1111111111111111111111111111111111111111",
                "base_tree":"2222222222222222222222222222222222222222",
                "reverie_base_sha":"3333333333333333333333333333333333333333",
                "reverie_base_tree":"4444444444444444444444444444444444444444",
                "checks":36,"failures":0,"real_seconds":528,"user_seconds":1300,"sys_seconds":90}}"#
        ))
    }

    // ---- SUPERSEDED-FAIL SURFACING -------------------------------------
    // The ledger does not latch, so a later clean full PASS flips the verdict to
    // VALIDATED. `failed_records` is what stops that green from silently burying
    // the same commit's clean full FAIL. Bracketed BOTH ways: it must count a
    // genuine superseded failure, and must NOT fire on a green with no failure
    // or on a red that carries no information about the product.

    #[test]
    fn validated_commit_counts_its_superseded_clean_full_failures() {
        let rows = vec![complete_failure(PASS_SHA), clean_full_pass(PASS_SHA)];
        let assessment = assess(&rows, PASS_SHA);
        // The pass supersedes the fail -- the verdict is genuinely VALIDATED ...
        assert_eq!(assessment.verdict, Verdict::Validated);
        // ... and the failure is still SURFACED rather than buried.
        assert_eq!(assessment.failed_records, 1);
    }

    #[test]
    fn a_clean_green_reports_no_superseded_failures() {
        // POSITIVE CONTROL: without this, a counter that always returned >0
        // would pass the test above and warn on every green.
        let rows = vec![clean_full_pass(PASS_SHA)];
        let assessment = assess(&rows, PASS_SHA);
        assert_eq!(assessment.verdict, Verdict::Validated);
        assert_eq!(assessment.failed_records, 0);
    }

    #[test]
    fn an_environment_fault_is_not_counted_as_a_superseded_failure() {
        // A command-not-found storm exercised nothing about the product, so it is
        // not a flake signal and must not raise the alarm on a legitimate green.
        let rows = vec![command_not_found_row(PASS_SHA), clean_full_pass(PASS_SHA)];
        let assessment = assess(&rows, PASS_SHA);
        assert_eq!(assessment.verdict, Verdict::Validated);
        assert_eq!(assessment.failed_records, 0);
    }

    #[test]
    fn superseded_failures_are_counted_not_merely_flagged() {
        // The banner prints N; a boolean would under-report a repeatedly-flaking
        // commit as indistinguishable from a single blip.
        let rows = vec![
            complete_failure(PASS_SHA),
            complete_failure(PASS_SHA),
            complete_failure(PASS_SHA),
            clean_full_pass(PASS_SHA),
        ];
        assert_eq!(assess(&rows, PASS_SHA).failed_records, 3);
    }

    #[test]
    fn failures_on_a_different_commit_are_not_attributed_here() {
        let rows = vec![complete_failure(OTHER_SHA), clean_full_pass(PASS_SHA)];
        let assessment = assess(&rows, PASS_SHA);
        assert_eq!(assessment.verdict, Verdict::Validated);
        assert_eq!(assessment.failed_records, 0);
    }

    #[test]
    fn failed_on_record_counts_only_genuine_clean_full_failures() {
        // The FAILED banner used `disqualified.len()`, which also counts subset,
        // dirty, truncated and env-fault rows. With no qualifying pass the
        // verdict is FailedOnRecord; the reported number must be the genuine
        // failures only -- here 1, not the 2 rows sitting in `disqualified`.
        let rows = vec![complete_failure(PASS_SHA), command_not_found_row(PASS_SHA)];
        let assessment = assess(&rows, PASS_SHA);
        assert_eq!(assessment.verdict, Verdict::FailedOnRecord);
        assert_eq!(assessment.disqualified.len(), 2);
        assert_eq!(assessment.failed_records, 1);
    }

    #[test]
    fn a_contended_red_is_withheld_from_failures_but_still_counted() {
        // The live shape: a clean full red recorded while other validates ran.
        // It is NOT a product failure (so `failed_records` stays 0) but the
        // operator must still learn the record exists.
        let contended = row(&format!(
            r#"{{"schema_version":4,"profile":"full","selection_mode":"full",
                "commit":"{PASS_SHA}","commit_anchored":true,"tree_dirty":false,
                "result":"fail","exit_code":1,"checks":5,"gates_run":5,
                "gates_expected":5,"failures":1,"executed_tests":765,"dag_jobs":16,
                "concurrent_validates":9,"known_flaky_failure":false,
                "solo_rerun_confirmation":false,"real_seconds":507,
                "gates":[{{"name":"portable CI DAG lane","result":"fail",
                    "exit_code":1,"real_seconds":423}}]}}"#
        ));
        let assessment = assess(&[contended, clean_full_pass(PASS_SHA)], PASS_SHA);
        assert_eq!(assessment.verdict, Verdict::Validated);
        assert_eq!(
            assessment.failed_records, 0,
            "a contended red is not a product failure"
        );
        assert_eq!(
            assessment.withheld_nonpass_records, 1,
            "but it must not be silent"
        );
    }

    #[test]
    fn a_clean_green_has_nothing_withheld() {
        // POSITIVE CONTROL for the second banner line.
        let assessment = assess(&[clean_full_pass(PASS_SHA)], PASS_SHA);
        assert_eq!(assessment.failed_records, 0);
        assert_eq!(assessment.withheld_nonpass_records, 0);
    }

    #[test]
    fn a_genuine_failure_is_counted_as_a_failure_not_withheld() {
        // The two counters must not double-count the same record.
        let assessment = assess(
            &[complete_failure(PASS_SHA), clean_full_pass(PASS_SHA)],
            PASS_SHA,
        );
        assert_eq!(assessment.failed_records, 1);
        assert_eq!(assessment.withheld_nonpass_records, 0);
    }

    fn complete_failure(sha: &str) -> HistoryRow {
        row(&format!(
            r#"{{"schema_version":6,"profile":"full","selection_mode":"full",
                "commit":"{sha}","commit_anchored":true,"tree_dirty":false,
                "result":"fail","exit_code":1,"checks":5,"gates_run":5,
                "gates_expected":5,"failures":1,"executed_tests":765,"dag_jobs":4,
                "concurrent_validates":0,"known_flaky_failure":false,
                "solo_rerun_confirmation":false,
                "gates":[{{"name":"portable CI DAG lane","result":"fail",
                    "exit_code":1,"failure_origin":"lane_substep",
                    "failed_substeps":["test.detcore_misc"]}}]}}"#
        ))
    }

    /// The exact live shape of a1493427 (reverie): a clean full run that recorded
    /// `fail` at 1s with every FAILING gate at exit 127 (command not found) — the
    /// build/test binaries were missing. The SAME commit passed 6/6 at 58s. An
    /// environment fault exercised nothing about the product.
    fn command_not_found_row(sha: &str) -> HistoryRow {
        row(&format!(
            r#"{{"schema_version":3,"repo":"reverie","profile":"full","selection_mode":"full",
                "commit":"{sha}","commit_anchored":true,"tree_dirty":false,
                "result":"fail","exit_code":1,"checks":6,"failures":5,"real_seconds":1,
                "gates":[
                    {{"name":"Merge-gate policy","result":"pass","exit_code":0,"real_seconds":0}},
                    {{"name":"Build workspace","result":"fail","exit_code":127,"real_seconds":0}},
                    {{"name":"Test regular workspace cases","result":"fail","exit_code":127,"real_seconds":0}},
                    {{"name":"Documentation tests","result":"fail","exit_code":127,"real_seconds":0}},
                    {{"name":"Clippy","result":"fail","exit_code":127,"real_seconds":0}},
                    {{"name":"Rustfmt","result":"fail","exit_code":127,"real_seconds":0}}
                ]}}"#
        ))
    }

    /// A SATISFIED per-node coverage obligation: a nonempty planned set, every
    /// planned test node executed, none inert, none absent — the shape a real
    /// full run stamps. `sat_coverage(13)` matches the measured 13-node lane set.
    fn sat_coverage(planned: u64) -> CoverageRow {
        CoverageRow {
            planned_test_nodes: planned,
            executed_test_nodes: planned,
            zero_executed_nodes: Some(vec![]),
            absent_nodes: Some(vec![]),
        }
    }

    #[test]
    fn clean_full_pass_validates() {
        let rows = vec![clean_full_pass(PASS_SHA)];
        let a = assess(&rows, PASS_SHA);
        assert_eq!(a.verdict, Verdict::Validated);
        assert_eq!(a.verdict.exit_code(), 0);
        assert_eq!(a.qualifying.len(), 1);
    }

    #[test]
    fn dirty_tree_is_never_a_hit() {
        let mut r = clean_full_pass(PASS_SHA);
        r.tree_dirty = Some(true);
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::NotValidated);
        assert_eq!(a.verdict.exit_code(), 4);
    }

    #[test]
    fn unanchored_is_never_a_hit() {
        let mut r = clean_full_pass(PASS_SHA);
        r.commit_anchored = Some(false);
        assert_eq!(assess(&[r], PASS_SHA).verdict, Verdict::NotValidated);
    }

    #[test]
    fn subset_selection_is_never_a_hit() {
        let mut r = clean_full_pass(PASS_SHA);
        r.selection_mode = Some("selective".into());
        assert_eq!(assess(&[r], PASS_SHA).verdict, Verdict::NotValidated);
    }

    #[test]
    fn non_full_profile_is_never_a_hit() {
        // A `quick`-level run is real work but is NOT the full gate; the stamp
        // guard (VALIDATION_LEVEL==full) would not fire, so neither do we.
        let mut r = clean_full_pass(PASS_SHA);
        r.profile = Some("quick".into());
        assert_eq!(assess(&[r], PASS_SHA).verdict, Verdict::NotValidated);
    }

    #[test]
    fn clean_full_failure_is_failed_not_missing() {
        let r = complete_failure(PASS_SHA);
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::FailedOnRecord);
        assert_eq!(a.verdict.exit_code(), 3);
    }

    #[test]
    fn truncated_failure_record_is_not_failed() {
        // Exact live shape from 3a404879 run 2: the driver was terminated after
        // two passing gates. A bare `result=fail` must not condemn the commit.
        let r = row(&format!(
            r#"{{"schema_version":3,"profile":"full","selection_mode":"full",
                "commit":"{PASS_SHA}","commit_anchored":true,"tree_dirty":false,
                "result":"fail","exit_code":130,"checks":2,"failures":0,
                "gates":[
                    {{"name":"Initialize repository submodules","result":"pass","exit_code":0}},
                    {{"name":"Centralized test manifest and inventory","result":"pass","exit_code":0}}
                ]}}"#
        ));
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::Truncated);
        assert_eq!(a.verdict.exit_code(), 4);
    }

    #[test]
    fn exit_130_after_a_real_gate_failure_is_failed_not_truncated() {
        // A gate genuinely failed, then teardown was Ctrl-C'd (exit 130). The
        // SIGINT must NOT launder the recorded failure into a non-verdict — a
        // real red is worse to hide than a needless re-run. Live shape: d096c20c.
        let mut r = complete_failure(PASS_SHA);
        r.exit_code = Some(130);
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::FailedOnRecord);
        assert_eq!(a.verdict.exit_code(), 3);
    }

    #[test]
    fn explicit_truncated_record_stays_first_class() {
        let r = row(&format!(
            r#"{{"schema_version":4,"profile":"full","selection_mode":"full",
                "commit":"{PASS_SHA}","commit_anchored":true,"tree_dirty":false,
                "result":"truncated","raw_result":"fail","exit_code":130,
                "checks":2,"gates_run":2,"gates_expected":5,"failures":0}}"#
        ));
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::Truncated);
        assert_eq!(a.verdict.exit_code(), 4);
    }

    #[test]
    fn incomplete_real_gate_failure_is_truncated_not_failed() {
        let mut r = complete_failure(PASS_SHA);
        r.checks = Some(1);
        r.gates_run = Some(1);
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::Truncated);
        assert_eq!(a.verdict.exit_code(), 4);
    }

    #[test]
    fn schema3_full_incomplete_red_without_gates_expected_is_truncated() {
        // Live shape a4d20fa8: a schema-3 full run whose producer predates the
        // `gates_expected` field. It fail-fasted at gate 1 of the known five-gate
        // contract. The full-profile fallback (expected=5) must apply so ran(1)!=5
        // reads TRUNCATED, matching Python flake_class.gate_counts — before this
        // fix Rust read raw gates_expected=None and returned NEEDS-RERUN, diverging
        // from Python on the canonical guard.
        let r = row(&format!(
            r#"{{"schema_version":3,"profile":"full","selection_mode":"full",
                "commit":"{PASS_SHA}","commit_anchored":true,"tree_dirty":false,
                "result":"fail","exit_code":1,"checks":1,"failures":1,
                "gates":[
                    {{"name":"Initialize repository submodules","result":"fail","exit_code":1}}
                ]}}"#
        ));
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::Truncated);
        assert_eq!(a.verdict.exit_code(), 4);
    }

    #[test]
    fn over_run_red_is_failed_not_truncated() {
        // OVER-RUN bracket: a genuine red that executed MORE gates than the
        // expected count (six live gates against a stale five-gate expectation) is
        // COMPLETE, not truncated. Before the `ran != expected` -> `ran < expected`
        // fix this read TRUNCATED (exit 4, re-dispatch), laundering a real failure
        // into a non-verdict and inconsistent with the `ran >= expected` complete
        // check in the same function. Only an UNDER-run is a truncation.
        let mut r = complete_failure(PASS_SHA);
        r.checks = Some(6);
        r.gates_run = Some(6);
        r.gates_expected = Some(5);
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::FailedOnRecord);
        assert_eq!(a.verdict.exit_code(), 3);
    }

    #[test]
    fn typed_empty_bucket_does_not_make_complete_failure_look_truncated() {
        let mut r = complete_failure(PASS_SHA);
        r.gates_expected = Some(6);
        r.extra.insert("skipped_nodes".into(), serde_json::json!(1));
        r.extra.insert(
            "intentional_skipped_nodes".into(),
            serde_json::json!([{
                "name": "privileged-e2e.manifest_applications",
                "reason": "empty-manifest-bucket"
            }]),
        );
        r.extra
            .insert("dependency_skipped_nodes".into(), serde_json::json!([]));
        r.extra
            .insert("unaccounted_nodes".into(), serde_json::json!([]));
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::FailedOnRecord);
        assert_eq!(a.verdict.exit_code(), 3);
    }

    #[test]
    fn legacy_schema1_incomplete_red_gets_no_five_gate_fallback() {
        // The fallback is schema>=3 only: a schema-1 full run legitimately had a
        // different gate count, so an absent gates_expected must NOT be fabricated
        // as 5. Such a row is never given a completeness-derived TRUNCATED verdict.
        let r = row(&format!(
            r#"{{"schema_version":1,"profile":"full","selection_mode":"full",
                "commit":"{PASS_SHA}","commit_anchored":true,"tree_dirty":false,
                "result":"fail","exit_code":1,"checks":1,"failures":1,
                "gates":[
                    {{"name":"Initialize repository submodules","result":"fail","exit_code":1}}
                ]}}"#
        ));
        assert_ne!(assess(&[r], PASS_SHA).verdict, Verdict::Truncated);
    }

    #[test]
    fn contended_or_known_flaky_red_needs_rerun() {
        let mut contended = complete_failure(PASS_SHA);
        contended.concurrent_validates = Some(1);
        assert_eq!(assess(&[contended], PASS_SHA).verdict, Verdict::NeedsRerun);

        // The exact #1592 class: one validate, but a -j16 lane contends with its
        // own Cargo nodes. The same non-flaky complete failure at solo -j4 is a
        // genuine failure; width is not decorative metadata.
        let mut wide = complete_failure(PASS_SHA);
        wide.dag_jobs = Some(16);
        wide.concurrent_validates = Some(0);
        assert_eq!(assess(&[wide], PASS_SHA).verdict, Verdict::NeedsRerun);
        assert_eq!(
            assess(&[complete_failure(PASS_SHA)], PASS_SHA).verdict,
            Verdict::FailedOnRecord
        );

        let mut flaky = complete_failure(PASS_SHA);
        flaky.known_flaky_failure = Some(true);
        assert_eq!(
            assess(&[flaky.clone()], PASS_SHA).verdict,
            Verdict::NeedsRerun
        );

        flaky.solo_rerun_confirmation = Some(true);
        assert_eq!(assess(&[flaky], PASS_SHA).verdict, Verdict::FailedOnRecord);
    }

    #[test]
    fn aborted_siblings_do_not_block_a_durable_failure() {
        let mut durable = complete_failure(PASS_SHA);
        let mut aborted = row(
            r#"{"gates":[{"name":"test.aborted","result":"fail","exit_code":-15,
                "aborted":true,"reason":"eager-exit after another step failed"}]}"#,
        );
        durable.gates.push(aborted.gates.remove(0));
        durable.gates_run = Some(6);
        durable.gates_expected = Some(6);
        assert_eq!(
            assess(&[durable], PASS_SHA).verdict,
            Verdict::FailedOnRecord,
            "an aborted sibling is not another unbound product failure",
        );
    }

    #[test]
    fn non_aborted_sibling_still_requires_bound_failure_origin() {
        let mut unbound = complete_failure(PASS_SHA);
        let mut genuine = row(
            r#"{"gates":[{"name":"test.second","result":"fail","exit_code":1,
                "aborted":false,"reason":"a second completed failure"}]}"#,
        );
        unbound.gates.push(genuine.gates.remove(0));
        unbound.gates_run = Some(6);
        unbound.gates_expected = Some(6);
        assert_eq!(
            assess(&[unbound], PASS_SHA).verdict,
            Verdict::NeedsRerun,
            "excluding aborted work must not excuse a second genuine unbound failure",
        );
    }

    #[test]
    fn executed_count_does_not_move_a_named_gate_red_off_failed() {
        // UNWIRE bracket — the genuine-failure direction. `executed_tests` was
        // refuted as a proxy in BOTH directions, so it no longer keys the verdict:
        // a red carrying a NAMED failing gate (portable CI DAG lane, bound origin)
        // stays a durable FailedOnRecord across every executed-count value, absent
        // included. A build/clippy red that exercises zero tests is still a real
        // defect. Live shape: 71bc3856 (exec 765) — a REAL failure.
        for count in [None, Some(0), Some(1), Some(430), Some(765)] {
            let mut r = complete_failure(PASS_SHA);
            r.executed_tests = count;
            let a = assess(&[r], PASS_SHA);
            assert_eq!(
                a.verdict,
                Verdict::FailedOnRecord,
                "executed_tests={count:?} with a named failing gate must stay FAILED",
            );
            assert_eq!(a.verdict.exit_code(), 3);
        }
    }

    #[test]
    fn red_without_a_named_gate_or_failure_count_is_no_result() {
        // UNWIRE bracket — the no-evidence direction. When no named failing gate
        // and no failure count are present, the red carries no observable defect,
        // so it is NO-RESULT regardless of a high executed-test count: the count is
        // diagnostic only and cannot conjure a durable FAILED. `failures` is absent
        // (not 0, which the truncation reading above already claims), the gate list
        // is empty, and executed_tests=765 must still re-dispatch, never FAILED.
        let r = row(&format!(
            r#"{{"schema_version":6,"profile":"full","selection_mode":"full",
                "commit":"{PASS_SHA}","commit_anchored":true,"tree_dirty":false,
                "result":"fail","exit_code":1,"checks":5,"gates_run":5,
                "gates_expected":5,"executed_tests":765,"dag_jobs":4,
                "concurrent_validates":0,"known_flaky_failure":false,
                "solo_rerun_confirmation":false,"gates":[]}}"#
        ));
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::NoResult);
        assert_eq!(a.verdict.exit_code(), 4);
    }

    #[test]
    fn conditionless_red_needs_rerun_instead_of_failed() {
        let mut r = complete_failure(PASS_SHA);
        r.gates_expected = None;
        r.dag_jobs = None;
        r.concurrent_validates = None;
        r.known_flaky_failure = None;
        assert_eq!(assess(&[r], PASS_SHA).verdict, Verdict::NeedsRerun);
    }

    #[test]
    fn complete_red_without_bound_failure_origin_needs_rerun() {
        let mut r = complete_failure(PASS_SHA);
        r.gates[0].failure_origin = None;
        r.gates[0].failed_substeps.clear();
        assert_eq!(assess(&[r.clone()], PASS_SHA).verdict, Verdict::NeedsRerun);

        r.gates.clear();
        assert_eq!(assess(&[r], PASS_SHA).verdict, Verdict::NeedsRerun);
    }

    #[test]
    fn command_not_found_storm_is_no_result_not_failed() {
        // LIVE a1493427: five failing gates at exit 127 (command not found), 1s
        // wall; the same commit passed 6/6 at 58s. An env fault exercised nothing
        // about the product, so it must be re-runnable, never a permanent FAILED.
        // (Before this rule the reverie 6-gate row only escaped FAILED by accident
        // — schema-3 carried no conditions and the 5-gate fallback mismatched its
        // gate count; once the producer emits conditions that accident vanishes.)
        let a = assess(&[command_not_found_row(PASS_SHA)], PASS_SHA);
        assert_eq!(a.verdict, Verdict::NoResult);
        assert_eq!(a.verdict.exit_code(), 4);
    }

    #[test]
    fn command_not_found_storm_with_full_conditions_still_no_result() {
        // The forward hazard: once the producer emits dag_jobs / concurrency /
        // bound origin on these rows, a complete condition-bound 127-storm must
        // STILL read NO-RESULT — env-fault detection is keyed on the observed
        // fault, not on the absence of condition fields.
        let mut r = command_not_found_row(PASS_SHA);
        r.schema_version = Some(6);
        r.gates_run = Some(6);
        r.gates_expected = Some(6);
        r.dag_jobs = Some(4);
        r.concurrent_validates = Some(0);
        r.known_flaky_failure = Some(false);
        for gate in r.gates.iter_mut().filter(|g| gate_is_red(g)) {
            gate.failure_origin = Some("outer_gate".into());
        }
        assert_eq!(assess(&[r], PASS_SHA).verdict, Verdict::NoResult);
    }

    #[test]
    fn subsecond_collapse_all_gates_red_is_no_result() {
        // No 127 code, but the whole run collapsed in <=1s with every gate red —
        // the run died before doing any real work. Env fault, re-runnable.
        let r = row(&format!(
            r#"{{"schema_version":3,"profile":"full","selection_mode":"full",
                "commit":"{PASS_SHA}","commit_anchored":true,"tree_dirty":false,
                "result":"fail","exit_code":1,"checks":3,"failures":3,"real_seconds":1,
                "gates":[
                    {{"name":"Build workspace","result":"fail","exit_code":1,"real_seconds":0}},
                    {{"name":"Test regular workspace cases","result":"fail","exit_code":1,"real_seconds":0}},
                    {{"name":"Clippy","result":"fail","exit_code":1,"real_seconds":0}}
                ]}}"#
        ));
        assert_eq!(assess(&[r], PASS_SHA).verdict, Verdict::NoResult);
    }

    #[test]
    fn genuine_red_that_executed_is_not_laundered_as_env_fault() {
        // A real product red: a lane substep failed at exit 1 AFTER real execution
        // time, complete profile with bound conditions. Neither env-fault tell
        // matches (exit != 127; wall not sub-second), so it stays a durable FAILED.
        // This is the "GENUINE red still reads FAILED" bracket.
        let mut r = complete_failure(PASS_SHA);
        r.real_seconds = Some(58.0);
        r.gates[0].real_seconds = Some(45.0);
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::FailedOnRecord);
        assert_eq!(a.verdict.exit_code(), 3);
    }

    #[test]
    fn mixed_command_not_found_and_genuine_red_is_not_no_result() {
        // One gate genuinely failed after running (exit 1, 45s); a second was
        // command-not-found. A real defect co-occurring with env noise must NOT be
        // laundered to NO-RESULT: any genuine executed red disqualifies the
        // env-fault reading.
        let mut r = complete_failure(PASS_SHA);
        r.real_seconds = Some(60.0);
        r.gates[0].real_seconds = Some(45.0);
        r.gates.push(
            serde_json::from_str(
                r#"{"name":"Rustfmt","result":"fail","exit_code":127,"real_seconds":0}"#,
            )
            .expect("valid gate json"),
        );
        assert_ne!(assess(&[r], PASS_SHA).verdict, Verdict::NoResult);
    }

    #[test]
    fn genuine_failed_sibling_wins_over_env_fault_row() {
        // Two rows for one commit: an env-fault 127-storm AND a genuine complete
        // red. The genuine red must surface (FailedOnRecord), never be downgraded
        // by the env-fault sibling.
        let mut genuine = complete_failure(PASS_SHA);
        genuine.real_seconds = Some(58.0);
        genuine.gates[0].real_seconds = Some(45.0);
        let rows = vec![command_not_found_row(PASS_SHA), genuine];
        assert_eq!(assess(&rows, PASS_SHA).verdict, Verdict::FailedOnRecord);
    }

    #[test]
    fn killed_run_is_no_result_not_failed_on_record() {
        // The real observed record: a full/full run that was killed (Ctrl-C).
        let r = row(
            r#"{"schema_version":3,"host":"test-runner.example","profile":"full","selection_mode":"full",
                "commit":"cde3c1195eee4e2691bac64a4aec10a45aba853e","commit_anchored":true,
                "tree_dirty":false,"result":"killed","exit_code":130,"checks":0,"failures":0}"#,
        );
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::NotValidated);
        assert_eq!(a.verdict.exit_code(), 4);
    }

    #[test]
    fn zero_test_green_is_not_validated() {
        // NEGATIVE: a clean full-coverage "pass" whose own banners prove zero
        // tests ran is a no-result, not a landing-eligible green. It is neither a
        // pass nor a known failure, so it must fall through to NotValidated
        // (exit 4 = re-dispatch), never Validated and never FailedOnRecord.
        let mut r = clean_full_pass(PASS_SHA);
        r.executed_tests = Some(0);
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::NotValidated);
        assert_eq!(a.verdict.exit_code(), 4);
        assert_eq!(a.qualifying.len(), 0);
    }

    #[test]
    fn counted_pass_validates() {
        // POSITIVE: the same clean full-coverage pass with a NONZERO executed
        // count is validated. Proves the guard is not inert — it does not reject
        // every green, only the demonstrably empty one.
        let mut r = clean_full_pass(PASS_SHA);
        r.executed_tests = Some(47);
        assert_eq!(assess(&[r], PASS_SHA).verdict, Verdict::Validated);
    }

    #[test]
    fn uncounted_pre_count_pass_is_not_validated() {
        // STRICT (post-transition, the grandfather-removal case): a receipt that
        // carries NEITHER count — old-schema (3 < COUNTS_SCHEMA) writer that
        // emitted no executed/filtered — is clean/full/full/pass yet proves
        // nothing about execution. With the grandfather REMOVED it is NotValidated
        // (exit 4 = re-dispatch to mint counts), not a landing-eligible green.
        // This is the "uncounted receipt REFUSED" direction; the backlog no longer
        // strands because it is backfilled to schema-5 and the landing consume-path
        // re-mints count-backed rows before reading the ledger.
        let mut r = clean_full_pass(PASS_SHA);
        r.schema_version = Some(3);
        r.executed_tests = None;
        r.filtered_tests = None;
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::NotValidated);
        assert_eq!(a.verdict.exit_code(), 4);
        assert_eq!(a.qualifying.len(), 0);
    }

    #[test]
    fn grandfather_still_refuses_the_zero_test_guard() {
        // NEGATIVE: grandfathering is NOT a free pass. A pre-count receipt whose
        // OWN banners nonetheless prove a zero-test run is refused at every schema
        // — a demonstrated zero-test run is never a full green.
        let mut zero = clean_full_pass(PASS_SHA);
        zero.schema_version = Some(3);
        zero.filtered_tests = None;
        zero.executed_tests = Some(0);
        assert_eq!(assess(&[zero], PASS_SHA).verdict, Verdict::NotValidated);
    }

    #[test]
    fn filtered_out_no_longer_rejects_a_full_green() {
        // TRANSITION POSITIVE (criterion 2 — the one that matters): a positive
        // aggregate filtered count is legitimate on a real full run (each shard
        // filters out other shards' tests; ~693 measured). The old blunt
        // `filtered_tests == 0` predicate rejected every such green and recreated
        // the stall "with better vocabulary". It is DELETED: an old-schema counted
        // receipt with filtered>0 + nonzero executed still validates.
        //
        // (A receipt carrying ONLY a filtered count and no executed count no longer
        // validates — that is the grandfather branch, now removed; see
        // `uncounted_pre_count_pass_is_not_validated`. `filtered` alone was never
        // execution evidence.)
        let mut counted = clean_full_pass(PASS_SHA);
        counted.schema_version = Some(1); // aggregate.py, counts present
        counted.executed_tests = Some(749);
        counted.filtered_tests = Some(693);
        assert_eq!(assess(&[counted], PASS_SHA).verdict, Verdict::Validated);
    }

    #[test]
    fn old_schema_carrying_counts_validates_on_nonzero_execution() {
        // aggregate.py stamps schema_version 1 but DOES emit both counts. It
        // predates per-node coverage, so it is held to the strongest thing it can
        // prove: NONZERO execution. `filtered` no longer gates (a positive count
        // is filtered=0 or filtered>0 alike), and coverage cannot be required of a
        // writer that never emitted it. A positive count validates, a demonstrated
        // zero does not.
        let mut ok = clean_full_pass(PASS_SHA);
        ok.schema_version = Some(1);
        ok.executed_tests = Some(47);
        ok.filtered_tests = Some(0);
        assert_eq!(assess(&[ok], PASS_SHA).verdict, Verdict::Validated);

        let mut zero = clean_full_pass(PASS_SHA);
        zero.schema_version = Some(1);
        zero.executed_tests = Some(0);
        zero.filtered_tests = Some(0);
        assert_eq!(assess(&[zero], PASS_SHA).verdict, Verdict::NotValidated);
    }

    #[test]
    fn count_capable_absent_counts_is_a_defect() {
        // NEGATIVE (the new-writer bug catch): a COUNT-CAPABLE receipt
        // (schema_version >= COUNTS_SCHEMA) that emits NO counts is a writer
        // DEFECT, not a pre-count receipt — it was contractually required to emit.
        // Held strict; absent counts => NotValidated. This is the one case pure
        // presence-keying cannot catch and the schema escalator exists for.
        let mut r = clean_full_pass(PASS_SHA);
        r.schema_version = Some(COUNTS_SCHEMA);
        r.executed_tests = None;
        r.filtered_tests = None;
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::NotValidated);
        assert_eq!(a.verdict.exit_code(), 4);
        assert_eq!(a.qualifying.len(), 0);
    }

    #[test]
    fn count_capable_full_green_with_legit_filters_validates() {
        // CRITERION 2 (the one that matters), N=2 stated: a count-capable receipt
        // that carries a SATISFIED per-node coverage obligation validates EVEN WITH
        // a large positive aggregate filtered count. Both cases below are real full
        // greens; the second is the measured shape (749 executed / 693 filtered out
        // across the DAG's cross-shard nodes) that the deleted `filtered==0` guard
        // used to reject. A bind that rejected these would recreate the stall with
        // better vocabulary.
        let cases: [(i64, i64); 2] = [(36, 0), (749, 693)];
        for (executed, filtered) in cases {
            let mut r = clean_full_pass(PASS_SHA);
            r.schema_version = Some(COUNTS_SCHEMA);
            r.executed_tests = Some(executed);
            r.filtered_tests = Some(filtered);
            r.coverage = Some(sat_coverage(13));
            let a = assess(&[r], PASS_SHA);
            assert_eq!(
                a.verdict,
                Verdict::Validated,
                "executed={executed} filtered={filtered} must be Validated"
            );
            assert_eq!(a.verdict.exit_code(), 0);
        }
    }

    #[test]
    fn count_capable_inert_or_absent_node_is_not_validated() {
        // CRITERION 1 (skipped/inert required node fails), decided from the receipt
        // WITHOUT reading a log: the narrowed-subset masquerade is now caught here,
        // per-node, not by the aggregate `filtered` count. A count-capable receipt
        // whose coverage names an ABSENT planned node (never ran) or an INERT node
        // (ran but executed 0 countable tests) violates its obligation.
        // ABSENT: a required node produced no terminal result at all.
        let mut absent = clean_full_pass(PASS_SHA);
        absent.schema_version = Some(COUNTS_SCHEMA);
        absent.executed_tests = Some(749);
        absent.filtered_tests = Some(693);
        absent.coverage = Some(CoverageRow {
            planned_test_nodes: 13,
            executed_test_nodes: 12,
            zero_executed_nodes: Some(vec![]),
            absent_nodes: Some(vec!["test.detcore_unit".into()]),
        });
        let a = assess(&[absent], PASS_SHA);
        assert_eq!(a.verdict, Verdict::NotValidated);
        assert_eq!(a.verdict.exit_code(), 4);

        // INERT: a required node ran but every crate filtered-to-empty / compiled
        // out (passed-sum 0) — the true `1 passed; 154 filtered out`-style subset.
        let mut inert = clean_full_pass(PASS_SHA);
        inert.schema_version = Some(COUNTS_SCHEMA);
        inert.executed_tests = Some(749);
        inert.filtered_tests = Some(693);
        inert.coverage = Some(CoverageRow {
            planned_test_nodes: 13,
            executed_test_nodes: 12,
            zero_executed_nodes: Some(vec!["test.cli".into()]),
            absent_nodes: Some(vec![]),
        });
        assert_eq!(assess(&[inert], PASS_SHA).verdict, Verdict::NotValidated);

        // NO planned test node at all is not a satisfied obligation either.
        let mut empty = clean_full_pass(PASS_SHA);
        empty.schema_version = Some(COUNTS_SCHEMA);
        empty.executed_tests = Some(749);
        empty.filtered_tests = Some(693);
        empty.coverage = Some(sat_coverage(0));
        assert_eq!(assess(&[empty], PASS_SHA).verdict, Verdict::NotValidated);
    }

    #[test]
    fn count_capable_missing_coverage_is_a_defect() {
        // DEFECT (fail-closed): a count-capable receipt that carries counts but
        // OMITS the coverage object was contractually required to emit it. Nonzero
        // executed + positive filtered is NOT enough on schema >= COUNTS_SCHEMA;
        // without coverage the run cannot prove no required node was inert/absent.
        let mut r = clean_full_pass(PASS_SHA);
        r.schema_version = Some(COUNTS_SCHEMA);
        r.executed_tests = Some(749);
        r.filtered_tests = Some(693);
        r.coverage = None;
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::NotValidated);
        assert_eq!(a.verdict.exit_code(), 4);
        assert_eq!(a.qualifying.len(), 0);
    }

    #[test]
    fn positive_control_legitimate_passes_validate_across_all_branches() {
        // POSITIVE CONTROL, N=4 across the two surviving ACCEPT branches: the
        // version-aware consumer must not reject everything (a consumer that did
        // would pass every negative test and be disabled within a day). Each is a
        // legitimate green:
        //   - OLD-WITH-COUNTS: aggregate.py carries counts under schema 1.
        //   - COUNT-CAPABLE: new writer, schema >= COUNTS_SCHEMA, counts present.
        // A count-capable green must carry a SATISFIED coverage obligation; the
        // older branch never emitted one. The former grandfather branch (no counts)
        // is removed and covered by `uncounted_pre_count_pass_is_not_validated`.
        // (schema_version, executed, filtered, coverage)
        let legit: [(u32, Option<i64>, Option<i64>, Option<CoverageRow>); 4] = [
            (1, Some(1), Some(0), None),    // old schema carrying counts
            (1, Some(47), Some(693), None), // old schema carrying counts, legit filters
            (COUNTS_SCHEMA, Some(36), Some(0), Some(sat_coverage(13))), // count-capable green
            (COUNTS_SCHEMA, Some(332), Some(693), Some(sat_coverage(13))), // count-capable, legit filters
        ];
        for (sv, executed, filtered, coverage) in legit {
            let mut r = clean_full_pass(PASS_SHA);
            r.schema_version = Some(sv);
            r.executed_tests = executed;
            r.filtered_tests = filtered;
            r.coverage = coverage;
            let a = assess(&[r], PASS_SHA);
            assert_eq!(
                a.verdict,
                Verdict::Validated,
                "schema={sv} executed={executed:?} filtered={filtered:?} must be Validated"
            );
            assert_eq!(a.verdict.exit_code(), 0);
        }
    }

    #[test]
    fn counts_schema_const_matches_shared_predicate() {
        // The const is a TEST fixture only; production reads the JSON. Pin it so a
        // JSON tightening cannot leave this file's fixtures behind (the drift this
        // whole change removes). Also assert the fixture assumptions this module
        // relies on still hold in the shared predicate.
        let pred = crate::qualifying_receipt::active();
        assert_eq!(
            pred.counts_schema, COUNTS_SCHEMA,
            "qualifying-receipt.json counts_schema drifted from the test-fixture const"
        );
        assert_eq!(pred.require.executed_tests_min, 1);
        assert_eq!(pred.require.failures_max, 0);
        assert_eq!(pred.require.result, "pass");
        assert!(!pred.gate_filtered_tests);
        assert!(pred.coverage.per_node);
        assert_eq!(pred.coverage.applies_at_schema_min, COUNTS_SCHEMA);
    }

    #[test]
    fn no_result_verdict_is_not_failed_on_record() {
        // A `no_result` (the aggregator's downgrade of a zero-test green) is
        // unverified, not known-bad: NotValidated (re-dispatch), not
        // FailedOnRecord.
        let mut r = clean_full_pass(PASS_SHA);
        r.result = Some("no_result".into());
        let a = assess(&[r], PASS_SHA);
        assert_eq!(a.verdict, Verdict::NotValidated);
        assert_eq!(a.verdict.exit_code(), 4);
    }

    #[test]
    fn no_record_is_not_validated() {
        let rows = vec![clean_full_pass(OTHER_SHA)];
        assert_eq!(assess(&rows, PASS_SHA).verdict, Verdict::NotValidated);
    }

    #[test]
    fn a_pass_wins_over_a_sibling_failure() {
        // Two records for one commit: an earlier fail and a later clean pass.
        let mut fail = clean_full_pass(PASS_SHA);
        fail.result = Some("fail".into());
        fail.finished_at = Some("2026-08-03T10:00:00Z".into());
        let rows = vec![fail, clean_full_pass(PASS_SHA)];
        assert_eq!(assess(&rows, PASS_SHA).verdict, Verdict::Validated);
    }

    #[test]
    fn parse_ledger_skips_blank_and_bad_lines() {
        let buf = format!(
            "{}\n\n   \nnot json at all\n{}\n",
            serde_json::to_string(&clean_full_pass(PASS_SHA)).unwrap(),
            serde_json::to_string(&clean_full_pass(OTHER_SHA)).unwrap(),
        );
        let (rows, skipped) = parse_ledger(&buf);
        assert_eq!(rows.len(), 2);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn resolve_full_sha_needs_no_ledger() {
        assert_eq!(resolve_sha(&[], PASS_SHA).unwrap(), PASS_SHA);
        assert_eq!(
            resolve_sha(&[], &PASS_SHA.to_ascii_uppercase()).unwrap(),
            PASS_SHA
        );
    }

    #[test]
    fn resolve_prefix_is_unambiguous_or_errors() {
        let rows = vec![clean_full_pass(PASS_SHA), clean_full_pass(OTHER_SHA)];
        assert_eq!(resolve_sha(&rows, "cde3c11").unwrap(), PASS_SHA);
        assert!(resolve_sha(&rows, "deadbeef").is_err()); // no match
                                                          // "a" prefixes only OTHER_SHA here, so it is unambiguous; a shared
                                                          // prefix across two distinct commits must error.
        let mut third = clean_full_pass("cdefffffffffffffffffffffffffffffffffffff");
        third.finished_at = Some("2026-08-03T20:00:00Z".into());
        let rows2 = vec![clean_full_pass(PASS_SHA), third];
        assert!(resolve_sha(&rows2, "cde").is_err()); // ambiguous
    }

    #[test]
    fn newest_picks_latest_finished_at() {
        let mut early = clean_full_pass(PASS_SHA);
        early.finished_at = Some("2026-08-03T01:00:00Z".into());
        let mut late = clean_full_pass(PASS_SHA);
        late.finished_at = Some("2026-08-03T23:00:00Z".into());
        let rows = vec![early, late.clone()];
        assert_eq!(
            newest(&rows).unwrap().finished_at.as_deref(),
            Some("2026-08-03T23:00:00Z")
        );
    }
}
