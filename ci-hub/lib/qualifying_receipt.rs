//! The SINGLE qualifying-receipt predicate, shared by every consumer.
//!
//! # Why this module exists
//!
//! "Does this ledger row / receipt qualify as a full green?" was answered by
//! independent inline certifiers across three languages
//! (`validate_status.rs`, `history_queries.rs`, `query.py`,
//! `publish_receipt.py`, `anchor_select.py`, `finalize_receipt.py`, and
//! `verify_receipt.sh`). The original sweep found five; follow-up review found
//! the two Python consumers. Each was its own floor and each
//! drifted from the others: the count-schema constant was redefined three times,
//! one consumer gated `filtered_tests == 0` while the others did not, `result ==
//! pass` was belt-and-suspendered in two places and keyed alone in two others,
//! and the per-node coverage clause was present in three and absent in one. That
//! is the hardcoded-list-of-a-growing-set defect applied to a PREDICATE: a
//! duplicated rule drifts in N-1 places and each copy looks correct in isolation
//! (see task `one-shared-qualifying-receipt-predicate-five-consumers-bypass-the-registry`,
//! source sweep `ai_docs/2026-08-04-floor-consumer-sweep.md`).
//!
//! # The fix
//!
//! ONE data artifact — `ci-hub/validate/qualifying-receipt.json` — colocated
//! with `rebase-base-floors.json` / `gate_floors.py` so tightening the floor is
//! ONE edit. All consumers READ it rather than restating its values inline.
//! Cross-field evidence semantics are additionally a single Python
//! implementation: admission provenance and schema/coverage compatibility live
//! in `ci-hub/qualifying_receipt.py`. Rust delegates those clauses to the Python
//! authority and the shell verifier reaches it through Rust's receipt-digest
//! command. Canonical lock identity, owner ancestry, and the coverage schema
//! boundary therefore have one verifier, not parallel Rust/Python/Bash copies.
//!
//! # Resolution order (single source at run time)
//!
//! 1. `$QUALIFYING_RECEIPT_PREDICATE` — an explicit override path (the mutation
//!    test points this at a tightened copy; NEVER at the live file).
//! 2. the on-disk `ci-hub/validate/qualifying-receipt.json` resolved against the
//!    repo root — this is PRIMARY in production so an edit to the JSON takes
//!    effect with no recompile (the "one edit" guarantee).
//! 3. the compile-time [`EMBEDDED`] snapshot of that same file — a last-resort
//!    fallback for unit tests that run the compiled binary from a temp dir where
//!    the on-disk file is not reachable. It is `include_str!`-ed from the exact
//!    file, so it cannot silently diverge from the source at compile time.
//!
//! A malformed override or on-disk file is a deploy defect and PANICS (loud),
//! never silently falls through to the embed — silent fallback would mask the
//! very drift this module exists to prevent.

use crate::bounded_output::BoundedOutput;
use crate::records::{CoverageRow, HistoryRow};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Repo-relative path of the canonical predicate. The literal lives ONLY here.
pub const PREDICATE_REL: &str = "ci-hub/validate/qualifying-receipt.json";

/// Environment override consulted first, so the mutation test can point every
/// consumer at a tightened copy without touching the live file.
pub const PREDICATE_ENV: &str = "QUALIFYING_RECEIPT_PREDICATE";

/// Compile-time snapshot of the live file (fallback only; see module docs).
pub const EMBEDDED: &str = include_str!("../validate/qualifying-receipt.json");

/// The mandatory receipt/ledger-row fields and their required values. A `pass`
/// row must satisfy every one; sourcing them here means a consumer cannot omit
/// one and quietly become more lenient than its peers.
#[derive(Clone, Debug, Deserialize)]
pub struct RequireClause {
    pub commit_anchored: bool,
    pub tree_dirty: bool,
    pub profile: String,
    pub selection_mode: String,
    pub result: String,
    pub failures_max: u64,
    pub executed_tests_min: i64,
}

/// The per-node coverage obligation and the schema at/after which it is enforced.
#[derive(Clone, Debug, Deserialize)]
pub struct CoverageClause {
    pub applies_at_schema_min: u32,
    pub per_node: bool,
}

/// Receipt-carried base evidence. Semantics live in the same Python authority
/// as admission provenance; Rust only transports this versioned policy.
#[derive(Clone, Debug, Deserialize)]
pub struct BaseClause {
    pub applies_at_schema_min: u32,
    pub branch: String,
}

/// Data consumed by the one Python admission-provenance verifier.
///
/// Rust intentionally carries no semantic methods for this clause: duplicating
/// its exact comparisons here would recreate the split authority this task
/// removes.
#[derive(Clone, Debug, Deserialize)]
pub struct AdmissionClause {
    pub applies_at_schema_min: u32,
    pub required_admission: String,
    pub required_concurrent_validates: u64,
    pub required_concurrency_proof: String,
    #[serde(default)]
    pub maximum_concurrent_validates: Option<u64>,
    #[serde(default)]
    pub accepted_concurrency_proofs: Vec<String>,
    pub require_registered_producer: bool,
}

/// Receipt-bound per-cell evidence and the schema at which it becomes
/// mandatory. Semantics live in `qualifying_receipt.py`; Rust only transports
/// this versioned policy to that single verifier.
#[derive(Clone, Debug, Deserialize)]
pub struct CellResultsClause {
    pub applies_at_schema_min: u32,
    pub supported_schema_max: u32,
}

impl Default for CellResultsClause {
    fn default() -> Self {
        Self {
            applies_at_schema_min: u32::MAX,
            supported_schema_max: u32::MAX,
        }
    }
}

/// WHO WROTE THE ROW, and from when that must be declared.
///
/// The provenance twin of [`CoverageClause`]: same shape of obligation, but
/// keyed on `finished_at` rather than `schema_version`, because 0 of the 729
/// live rows carry `producer` while 729 of 729 carry `finished_at` -- and
/// because `ci-hub.rs` branches on `schema_version == 4` by exact equality, so
/// a writer version bump is independently unsafe. See the JSON's
/// `_producer_names_the_writer_so_provenance_is_not_forensics` note.
#[derive(Clone, Debug, Deserialize)]
pub struct ProducerClause {
    /// Whether provenance is demanded at all. False disarms the clause wholly.
    pub required: bool,
    /// RFC3339 UTC instant at/after which a row MUST name its writer. `None`
    /// means declared-but-inert: the column is recorded and credited, and
    /// nothing is refused for lacking it. Flipped only once every writer ships.
    #[serde(default)]
    pub applies_from_finished_at: Option<String>,
    /// The registered writer slugs. A row naming something outside this set is
    /// refused under enforcement -- presence alone is not the bar, because a
    /// typo'd or invented writer name is exactly as unattributable as none.
    #[serde(default)]
    pub known: Vec<String>,
}

impl Default for ProducerClause {
    /// Inert. Used only when a predicate omits the key (older mutation
    /// fixtures); the canonical file is separately asserted to DECLARE it, so
    /// this default can never silently disarm the live predicate.
    fn default() -> Self {
        Self {
            required: false,
            applies_from_finished_at: None,
            known: Vec::new(),
        }
    }
}

/// Typed outcome of the provenance obligation. `Grandfathered` deliberately
/// reuses the grandfathered schema-4 coverage wording: the row does not CLAIM a
/// writer and is not asked to, so its `producer_ok` is null, not false.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProducerVerdict {
    Ok,
    Grandfathered,
    Missing,
    UnknownWriter,
}

/// Whether the provenance obligation is LIVE for this particular row.
///
/// Needs all three: clause required, an epoch declared, and the row at/after
/// it. A row whose `finished_at` is absent or malformed while an epoch IS
/// declared is enforced (fail-closed) -- otherwise dropping the timestamp would
/// be a trivial way to opt out of the check.
pub fn producer_enforced_for(row: &HistoryRow, pred: &QualifyingPredicate) -> bool {
    if !pred.producer.required {
        return false;
    }
    let Some(epoch) = pred.producer.applies_from_finished_at.as_deref() else {
        return false;
    };
    if epoch.is_empty() {
        return false;
    }
    match row.finished_at.as_deref() {
        // Fixed-width RFC3339 UTC sorts lexicographically == chronologically,
        // so the epoch comparison needs no date parsing.
        Some(ts) if is_rfc3339_z(ts) => ts >= epoch,
        _ => true,
    }
}

/// `2026-08-07T20:15:31Z` exactly -- the shape every ledger row uses.
fn is_rfc3339_z(ts: &str) -> bool {
    let b = ts.as_bytes();
    b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z'
        && [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
            .iter()
            .all(|&i| b[i].is_ascii_digit())
}

/// Decide the provenance obligation. Mirrors the Python `producer_verdict`.
///
/// A row NAMING a registered writer is `Ok` whether or not the epoch has been
/// flipped, so rows from already-deployed writers stop being grandfathered the
/// moment they exist rather than at activation.
pub fn producer_verdict(row: &HistoryRow, pred: &QualifyingPredicate) -> ProducerVerdict {
    let named = row
        .extra
        .get("producer")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    if let Some(name) = named {
        if pred.producer.known.is_empty() || pred.producer.known.iter().any(|k| k == name) {
            return ProducerVerdict::Ok;
        }
        return ProducerVerdict::UnknownWriter;
    }
    if producer_enforced_for(row, pred) {
        ProducerVerdict::Missing
    } else {
        ProducerVerdict::Grandfathered
    }
}

/// The whole predicate, deserialized from the JSON. Unknown keys (e.g.
/// `_comment`) are ignored on purpose so the file can carry documentation.
#[derive(Clone, Debug, Deserialize)]
pub struct QualifyingPredicate {
    /// The `schema_version` at/after which a receipt's writer GUARANTEES it emits
    /// both count fields and per-node coverage, so an absent count/coverage on
    /// such a receipt is a writer DEFECT (reject), not a pre-count receipt.
    pub counts_schema: u32,
    pub require: RequireClause,
    /// When true, additionally require `filtered_tests == 0`. Default false: a
    /// full run legitimately filters other-lane/other-shard tests, so a positive
    /// aggregate filtered count says nothing about coverage (`executed>0` plus
    /// the per-node clause already guard emptiness/narrowing).
    #[serde(default)]
    pub gate_filtered_tests: bool,
    /// Green classes admitted by the landing predicate. The class is derived
    /// from receipt provenance; a row's optional `green_class` field is only a
    /// cache and must agree with that derivation.
    #[serde(default = "default_accepts_green_class")]
    pub accepts_green_class: Vec<String>,
    pub coverage: CoverageClause,
    /// Exact base/provenance evidence and its merge-boundary policy.
    pub base: BaseClause,
    /// Canonical admission/owner-ancestry obligation. Semantics live only in
    /// `qualifying_receipt.py::admission_verdict`.
    pub admission: AdmissionClause,
    /// Per-cell comparison evidence. Defaulted inert only for historical test
    /// predicates; the canonical JSON is mutation-tested to declare it.
    #[serde(default)]
    pub cell_results: CellResultsClause,
    /// Provenance obligation. Defaulted inert so an older override fixture that
    /// predates the key still parses; the live file declares it explicitly.
    #[serde(default)]
    pub producer: ProducerClause,
}

fn default_accepts_green_class() -> Vec<String> {
    vec!["hard".to_string()]
}

impl QualifyingPredicate {
    /// Parse from JSON text, mapping errors to a message that names the source.
    pub fn parse(text: &str, origin: &str) -> Result<Self, String> {
        serde_json::from_str(text)
            .map_err(|e| format!("{origin}: malformed qualifying predicate: {e}"))
    }
}

/// A per-node coverage obligation is SATISFIED iff the run planned at least one
/// test-bearing DAG node AND no planned test node was inert or absent. Carrying
/// the node NAMES in the receipt lets this be re-derived without re-reading a log
/// (Proxy Binding: the condition travels with the value).
/// A coverage obligation's outcome, carrying WHY rather than collapsing to a
/// bool. `Unavailable` is the case this type exists for: a receipt that did not
/// report enough to decide must be distinguishable from one that reported a
/// clean result, and must never be counted as the latter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoverageVerdict {
    /// Planned > 0 and both failure lists were REPORTED and empty.
    Satisfied,
    /// Reported, and something was inert or absent.
    Unsatisfied,
    /// Not decidable from this receipt. Refused, never a pass.
    Unavailable(&'static str),
}

/// Decide a per-node coverage obligation, fail-closed on anything unreported.
pub fn coverage_verdict(cov: &CoverageRow) -> CoverageVerdict {
    // Absence is UNKNOWN, not "none". Checked before the emptiness tests so a
    // missing list can never be read as a clean one.
    let (Some(zero), Some(absent)) = (&cov.zero_executed_nodes, &cov.absent_nodes) else {
        return CoverageVerdict::Unavailable(
            "coverage omitted zero_executed_nodes and/or absent_nodes; \
             an unreported list is unknown, not empty",
        );
    };
    if cov.planned_test_nodes == 0 {
        // The producer could not determine a planned set; never a satisfied
        // obligation (unchanged, and already fail-closed).
        return CoverageVerdict::Unavailable("planned_test_nodes == 0: no planned set to cover");
    }
    if zero.is_empty() && absent.is_empty() {
        CoverageVerdict::Satisfied
    } else {
        CoverageVerdict::Unsatisfied
    }
}

/// Bool view. Only [`CoverageVerdict::Satisfied`] counts; both `Unsatisfied`
/// and `Unavailable` are refusals.
///
/// Retained though the Rust callers now use [`coverage_verdict`] directly: this
/// is the name the Python verifier documents itself as mirroring
/// (`ci-hub/qualifying_receipt.py::coverage_satisfied`), so keeping it makes the
/// cross-language correspondence checkable by name.
#[allow(dead_code)]
pub fn coverage_satisfied(cov: &CoverageRow) -> bool {
    coverage_verdict(cov) == CoverageVerdict::Satisfied
}

/// How a row qualified — or why it did not. `row_qualifies` collapses this to a
/// bool for existing consumers, but the distinction is the point: a receipt
/// grandfathered through the count-only fallback carries NO coverage evidence
/// and is not the same fact as a full green, even though both are "true" today.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Qualification {
    /// Count-capable receipt whose per-node coverage obligation is satisfied.
    FullCoverage,
    /// Pre-coverage-schema receipt carrying counts. Proves nonzero execution and
    /// NOTHING about per-node completeness. Accepted for backward compatibility;
    /// a consumer that needs completeness must refuse this variant rather than
    /// read it as a full green.
    CountsOnlyGrandfathered { schema: u32 },
    /// Refused, with the clause that refused it. A statement ABOUT THE RECEIPT:
    /// it was read, and it does not qualify.
    Refused(&'static str),
    /// The receipt could not be READ, with the reader fault that stopped it.
    ///
    /// ⚠️ NOT A VERDICT ON THE RECEIPT, AND THAT IS THE WHOLE POINT. `Refused`
    /// says the receipt fails the contract; this says the contract was never
    /// evaluated, because the reader hit `python3` missing, a verifier that would
    /// not launch, a request never delivered, a child never reaped, unparseable
    /// output, or a verifier contradicting itself. Collapsing the two produces
    /// "this receipt is refused because python3 is not on PATH", which blames the
    /// evidence for the environment and sends the operator to the wrong place.
    ///
    /// Blocks exactly as `Refused` does. No admission decision moves.
    Indeterminate(&'static str),
}

impl Qualification {
    /// Today's boolean acceptance. Deliberately unchanged so this typing is not
    /// also a silent floor change.
    /// ⚠️ AN ALLOW-LIST, NOT A DENY-LIST, AND THE DIFFERENCE IS A FUTURE BUG.
    /// Written as `!matches!(self, Refused(_))` this admitted every variant that
    /// did not yet exist: adding `Indeterminate` would have made an UNREADABLE
    /// receipt accepted by omission, silently, with no line changed to review.
    /// Naming the accepting variants makes a new variant block until someone
    /// decides otherwise.
    pub fn accepted(&self) -> bool {
        // Expressed THROUGH `coverage_demonstrated` rather than repeating
        // `FullCoverage` here. Two spellings of the same predicate drift; and it
        // gives the coverage half a real caller, so the `#[allow(dead_code)]` that
        // was suppressing it -- and the warning the owner hit on every
        // validate-status -- both go away without deleting the predicate the
        // owner stopped an agent from deleting.
        self.coverage_demonstrated()
            || matches!(self, Qualification::CountsOnlyGrandfathered { .. })
    }

    /// Whether per-node completeness was actually DEMONSTRATED, as opposed to
    /// merely not contradicted.
    ///
    /// `accepted()` is now that caller, so this is no longer dead and the
    /// `#[allow(dead_code)]` that used to sit here is gone. It remains the half
    /// a consumer needing COMPLETENESS (rather than mere admission) must call,
    /// because `CountsOnlyGrandfathered` is accepted and demonstrates nothing.
    pub fn coverage_demonstrated(&self) -> bool {
        matches!(self, Qualification::FullCoverage)
    }
}

/// Derive the receipt's green class from the same provenance fields and rules
/// as `validate/green_class.py`. `None` is a refused, internally contradictory
/// provenance record; `not-green` remains a derived class so the policy datum,
/// rather than an inline Rust special case, decides whether it is accepted.
fn derived_green_class(row: &HistoryRow) -> Option<&'static str> {
    let commit = row.commit.as_deref()?;
    if commit == "unknown" {
        return None;
    }
    let validated = match row.extra.get("validated_head_sha") {
        None => commit,
        Some(value) => value.as_str()?,
    };
    let inherited = row.extra.get("inherited_from");
    let derived = if validated == commit {
        if inherited.is_some() {
            return None;
        }
        "hard"
    } else {
        let inherited = inherited?.as_object()?;
        let kind = inherited.get("delta_kind")?.as_str()?;
        if !matches!(
            kind,
            "rebase-only" | "rebase-plus-upstream" | "new-branch-commits"
        ) {
            return None;
        }
        let branch_commits = inherited.get("branch_commits")?.as_u64()?;
        let force_full = match inherited.get("force_full_paths") {
            None => &[][..],
            Some(value) => value.as_array()?.as_slice(),
        };
        if !force_full.iter().all(serde_json::Value::is_string) {
            return None;
        }
        let upstream = match inherited.get("upstream_commits") {
            None => 0,
            Some(value) => value.as_u64()?,
        };
        if kind == "new-branch-commits" || branch_commits > 0 {
            "not-green"
        } else {
            match kind {
                "rebase-only" => {
                    if upstream != 0 {
                        return None;
                    }
                    "soft-rebase-only"
                }
                "rebase-plus-upstream" => {
                    if upstream == 0 {
                        return None;
                    }
                    if force_full.is_empty() {
                        "soft-upstream-delta"
                    } else {
                        "soft-force-full-touched"
                    }
                }
                _ => unreachable!("delta kind was checked above"),
            }
        }
    };
    if let Some(label) = row.extra.get("green_class") {
        if label.as_str() != Some(derived) {
            return None;
        }
    }
    Some(derived)
}

#[derive(Debug, Deserialize)]
struct AdmissionReport {
    accepted: bool,
    admission_satisfied: Option<bool>,
    admission_status: String,
    base_satisfied: Option<bool>,
    base_status: String,
    coverage_schema_satisfied: Option<bool>,
    coverage_schema_status: String,
    cell_results_satisfied: Option<bool>,
    cell_results_status: String,
}

#[derive(Debug, Deserialize)]
struct BaseBoundaryReport {
    accepted: bool,
    base_satisfied: Option<bool>,
    base_status: String,
}

/// The three states a delegated boundary check can actually be in.
///
/// ⚠️ `bool` COULD ONLY EXPRESS TWO, so "I could not determine" and "I determined
/// it does not qualify" shared the value `false`. Those are opposite facts: one
/// is a verdict ABOUT THE RECEIPT, the other is a fault in the READER asking.
/// The Python authority already separates them and says why — rc 1 is "this
/// receipt does not qualify", rc 3 is "I could not judge it", and
/// `qualifying_receipt.py:1336` records the harm of merging them: an actor with
/// an incomplete checkout "disbelieve[s] every valid receipt in the fleet and
/// blame[s] the evidence for its own missing submodule." The information existed
/// and was discarded one layer up.
///
/// BOTH NON-ACCEPTING STATES STILL BLOCK, and that is why introducing this type
/// is a repair rather than a policy change: no admission decision moves. It is
/// already this subsystem's ruling that an unknown answer is not a permission —
/// see `landing_lock.rs` and `validate_lock.rs`, which refuse an UNKNOWN state
/// with the words "absence is not permission". What changes is only that a
/// reader fault stops being reported as a verdict against the evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryOutcome {
    /// The authority judged the row and accepted it.
    Accepted,
    /// The authority judged the row and refused it. A fact about the RECEIPT.
    Refused,
    /// The authority could not be consulted, or could not judge. A fact about
    /// the READER. Carries the specific failure so it cannot be reported as a
    /// generic refusal.
    Indeterminate(&'static str),
}

impl BoundaryOutcome {
    /// True only for `Accepted`. Both other states block, so a caller that only
    /// needs the gate keeps working — but it must not use this to BUILD A
    /// MESSAGE, because it cannot tell the two blocking states apart.
    ///
    /// ⚠️ DO NOT DELETE TO SILENCE A WARNING. This pair is the
    /// could-not-determine state that replaced a `bool` collapsing "could not
    /// determine" into "does not qualify". Removing them to make a lint pass
    /// would reward reintroducing that collapse. The `#[allow(dead_code)]` this
    /// carried until 2026-08-25 was the lint correctly reporting that the repair
    /// existed and was not installed; `shared_evidence_outcome` is now the caller
    /// the note used to predict, so the attribute is gone rather than suppressed.
    pub fn accepted(&self) -> bool {
        matches!(self, BoundaryOutcome::Accepted)
    }

    /// The reader-fault detail, when there is one. Paired with
    /// [`BoundaryOutcome::accepted`]: that answers *may I proceed*, this answers
    /// *why not, and is it my fault or the receipt's* — which are different
    /// questions and must not be answered by one boolean.
    pub fn indeterminate_reason(&self) -> Option<&'static str> {
        match self {
            BoundaryOutcome::Indeterminate(reason) => Some(reason),
            _ => None,
        }
    }
}

/// Delegate final mutable-tip currency to the same Python semantic authority.
/// Shell reaches this through `receipt-digest`; it never invokes or restates
/// the verifier itself.
///
/// Returns [`BoundaryOutcome`] so a reader fault is distinguishable from a
/// verdict; see that type for why both still block.
pub fn shared_base_boundary_outcome(
    row: &HistoryRow,
    current_base: &str,
    current_reverie_base: &str,
    repo_checkout: &Path,
    reverie_checkout: &Path,
) -> BoundaryOutcome {
    let verifier = resolve_root().join("ci-hub/qualifying_receipt.py");
    // Every arm below that cannot reach a verdict is INDETERMINATE, not refused.
    // None of these says anything about the row; each says the reader failed.
    let Ok(request) = serde_json::to_vec(row) else {
        return BoundaryOutcome::Indeterminate(
            "cannot send the row to the merge-boundary authority",
        );
    };
    let Ok(output) = Command::new("python3")
        .arg(verifier)
        .arg("--merge-boundary")
        .arg("--current-base")
        .arg(current_base)
        .arg("--current-reverie-base")
        .arg(current_reverie_base)
        .arg("--repo-checkout")
        .arg(repo_checkout)
        .arg("--reverie-checkout")
        .arg(reverie_checkout)
        .bounded_output_with_input(&request)
    else {
        return BoundaryOutcome::Indeterminate("merge-boundary authority did not complete");
    };
    // rc 3 is the authority SAYING it could not judge (an undereferenceable
    // producer definition, an incomplete checkout). rc 1 is a verdict. Any other
    // nonzero is a crash, which is likewise not a verdict.
    match output.status.code() {
        Some(0) => {}
        Some(1) => return BoundaryOutcome::Refused,
        Some(3) => {
            return BoundaryOutcome::Indeterminate(
                "merge-boundary authority could not dereference the producer definition (rc 3)",
            );
        }
        _ => {
            return BoundaryOutcome::Indeterminate("merge-boundary authority exited abnormally");
        }
    }
    let Ok(report) = serde_json::from_slice::<BaseBoundaryReport>(&output.stdout) else {
        return BoundaryOutcome::Indeterminate(
            "merge-boundary authority emitted no parsable report",
        );
    };
    let status_matches = match report.base_status.as_str() {
        "satisfied" => report.base_satisfied == Some(true),
        "grandfathered-unknown" => report.base_satisfied.is_none(),
        _ => false,
    };
    if report.accepted && status_matches {
        BoundaryOutcome::Accepted
    } else {
        BoundaryOutcome::Refused
    }
}

/// Delegate cross-field evidence consistency to its ONE semantic implementation.
///
/// Exit 0/1 is a typed accept/refuse verdict. Any launch, parse, or exit-code
/// inconsistency is a deploy defect and fails closed; a missing verifier must
/// never make a receipt more authoritative.
///
/// ⚠️ WHY THIS RETURNS `BoundaryOutcome` AND NOT `bool`. It used to return
/// `false` for six structurally different reasons, and the caller turned every
/// one of them into the single string "shared evidence contract not satisfied" —
/// a claim about the RECEIPT — when five of the six are facts about the READER
/// and one is a fact about the AUTHORITY. Failing closed is right and is
/// unchanged; reporting all six identically is not, because a caller that cannot
/// tell which fault it has cannot react to it.
///
/// EACH REASON EARNS ITS OWN VALUE BY HAVING A DIFFERENT REMEDY. That is the bar:
/// not that the causes differ, but that the response does.
///
///   `verifier-unlaunchable`      python3 could not be spawned. The interpreter
///                                is missing from PATH. Fix the environment; a
///                                retry on this box will fail identically.
///   `verifier-stdin-unavailable` the pipe was not created despite being
///                                requested — file-descriptor pressure, not
///                                configuration. RETRY; it is transient, which
///                                is exactly what the case above is not.
///   `verifier-request-undelivered` it launched, then died or closed the pipe
///                                before reading the request. Read the verifier's
///                                stderr; retrying re-runs a broken verifier.
///   `verifier-not-reaped`        `wait_with_output` failed. An OS-level fault
///                                with the verdict possibly already produced.
///                                RETRY, and unlike the pipe case this one may
///                                have had side effects.
///   `verifier-output-unparseable` it ran and answered, but the answer is not an
///                                `AdmissionReport`. This consumer and the Python
///                                authority have SKEWED SCHEMAS. Redeploy or fix
///                                the pin; no number of retries converges.
///   `verifier-self-inconsistent` it ran, answered, parsed — and its exit code
///                                contradicts its own report body, or a status
///                                string contradicts its boolean. A defect INSIDE
///                                the authority. File it against
///                                `ci-hub/qualifying_receipt.py`; the receipt is
///                                not implicated and must not be blamed.
///
/// Two of these share a remedy word ("retry") and are still kept apart:
/// `verifier-stdin-unavailable` is pure back-pressure with nothing executed,
/// while `verifier-not-reaped` may have run the verifier to completion. A caller
/// deciding whether a retry is idempotent needs that distinction, so sharing one
/// value would collapse the thing it most needs to know.
///
/// `Refused` is reserved for the ONE case that is a fact about the receipt: the
/// verifier ran, answered coherently, and said no.
fn shared_evidence_outcome(row: &HistoryRow, pred: &QualifyingPredicate) -> BoundaryOutcome {
    let root = resolve_root();
    let verifier = root.join("ci-hub/qualifying_receipt.py");
    let request = serde_json::json!({
        "row": row,
        "admission": {
            "applies_at_schema_min": pred.admission.applies_at_schema_min,
            "required_admission": pred.admission.required_admission,
            "required_concurrent_validates": pred.admission.required_concurrent_validates,
            "required_concurrency_proof": pred.admission.required_concurrency_proof,
            "maximum_concurrent_validates": pred.admission.maximum_concurrent_validates,
            "accepted_concurrency_proofs": pred.admission.accepted_concurrency_proofs,
            "require_registered_producer": pred.admission.require_registered_producer,
        },
        "base": {
            "applies_at_schema_min": pred.base.applies_at_schema_min,
            "branch": pred.base.branch,
        },
        "coverage": {
            "applies_at_schema_min": pred.coverage.applies_at_schema_min,
            "per_node": pred.coverage.per_node,
        },
        "cell_results": {
            "applies_at_schema_min": pred.cell_results.applies_at_schema_min,
            "supported_schema_max": pred.cell_results.supported_schema_max,
        },
        "producer": {
            "required": pred.producer.required,
            "applies_from_finished_at": pred.producer.applies_from_finished_at,
            "known": pred.producer.known,
        },
    });
    let Ok(request_bytes) = serde_json::to_vec(&request) else {
        return BoundaryOutcome::Indeterminate("verifier-request-undelivered");
    };
    let Ok(output) = Command::new("python3")
        .arg(verifier)
        .arg("--evidence-only")
        .bounded_output_with_input(&request_bytes)
    else {
        return BoundaryOutcome::Indeterminate("verifier-not-reaped");
    };
    let Ok(report) = serde_json::from_slice::<AdmissionReport>(&output.stdout) else {
        return BoundaryOutcome::Indeterminate("verifier-output-unparseable");
    };
    let exit_matches = match output.status.code() {
        Some(0) => report.accepted,
        Some(1) => !report.accepted,
        _ => false,
    };
    let status_matches = match report.admission_status.as_str() {
        "satisfied" => report.admission_satisfied == Some(true),
        "grandfathered-unknown" => report.admission_satisfied.is_none(),
        _ => report.admission_satisfied == Some(false),
    };
    let base_status_matches = match report.base_status.as_str() {
        "satisfied" => report.base_satisfied == Some(true),
        "grandfathered-unknown" => report.base_satisfied.is_none(),
        _ => report.base_satisfied == Some(false),
    };
    let coverage_schema_status_matches = match report.coverage_schema_status.as_str() {
        "satisfied" => report.coverage_schema_satisfied == Some(true),
        "grandfathered-unknown" => report.coverage_schema_satisfied.is_none(),
        _ => report.coverage_schema_satisfied == Some(false),
    };
    let cell_results_status_matches = match report.cell_results_status.as_str() {
        "satisfied" => report.cell_results_satisfied == Some(true),
        "grandfathered-unknown" => report.cell_results_satisfied.is_none(),
        _ => report.cell_results_satisfied == Some(false),
    };
    // SELF-CONSISTENCY FIRST, AND SEPARATELY FROM THE VERDICT. Each of these
    // compares the verifier's answer against its OWN other answer. A mismatch
    // says the authority is broken, which is not something the receipt can be
    // blamed for -- so it must not fall through into the `Refused` below.
    if !(exit_matches
        && status_matches
        && base_status_matches
        && coverage_schema_status_matches
        && cell_results_status_matches)
    {
        return BoundaryOutcome::Indeterminate("verifier-self-inconsistent");
    }
    // Only here is the answer both coherent and negative, which is the one
    // reading that is a fact about the RECEIPT.
    if report.accepted {
        BoundaryOutcome::Accepted
    } else {
        BoundaryOutcome::Refused
    }
}

/// THE qualifying-receipt predicate. Every Rust consumer routes here; the
/// control flow mirrors what the inline certifiers did, but every constant and
/// flag is sourced from `pred` so there is one place to tighten.
///
/// Over and above clean full coverage (the `require` string/bool fields) and
/// `result == require.result`:
///   * `failures` (defaulting to 0 when absent) must be `<= require.failures_max`
///     — belt-and-suspenders with `result == pass`; a `pass`/`failures>0` row is
///     malformed and must not qualify.
///   * a demonstrated zero-test run (`executed_tests == Some(0)`) is never a
///     green at any schema — never grandfathered.
///   * when a passing row carries `passed_tests`, it must equal
///     `executed_tests`; a partial pass count is contradictory green evidence.
///   * if `gate_filtered_tests`, additionally require `filtered_tests == 0`.
///   * COUNT-CAPABLE receipts (`schema_version >= counts_schema`) are held to the
///     FULL contract: every planned outer gate is accounted for,
///     `executed_tests >= require.executed_tests_min`, AND a `coverage` object
///     that satisfies its per-test-node obligation (when `coverage.per_node`
///     and `schema_version >= coverage.applies_at_schema_min`). Missing any of
///     these on a count-capable receipt is a writer defect -> reject.
///   * receipts carrying counts under an OLDER schema prove nonzero execution but
///     predate per-node coverage: held only to `executed_tests >=
///     require.executed_tests_min`.
///   * a receipt carrying NEITHER count proves nothing and is rejected
///     (NotValidated / re-dispatch), never treated as green.
pub fn row_qualifies(row: &HistoryRow, sha: &str, pred: &QualifyingPredicate) -> bool {
    row_qualification(row, sha, pred).accepted()
}

/// Planned-gate accounting, kept separate from the executed PASS list.
///
/// Only the statically proven empty-manifest case is safe to count toward a
/// complete plan. The node remains absent from `gates`/`gates_run`, so this
/// never manufactures a passing gate. Unknown reasons and every dependency or
/// unaccounted skip fail closed.
pub fn intentional_skip_count(row: &HistoryRow) -> Option<u64> {
    let Some(value) = row.extra.get("intentional_skipped_nodes") else {
        return Some(0);
    };
    let skips = value.as_array()?;
    if skips.is_empty() {
        return Some(0);
    }
    let mut names = BTreeSet::new();
    for skip in skips {
        let object = skip.as_object()?;
        let name = object.get("name")?.as_str()?.trim();
        if name.is_empty()
            || object.get("reason")?.as_str()? != "empty-manifest-bucket"
            || !names.insert(name)
        {
            return None;
        }
    }
    for field in ["dependency_skipped_nodes", "unaccounted_nodes"] {
        if !row
            .extra
            .get(field)
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            return None;
        }
    }
    let count = u64::try_from(skips.len()).ok()?;
    (row.extra.get("skipped_nodes")?.as_u64()? == count).then_some(count)
}

pub fn gate_accounting_complete(row: &HistoryRow) -> bool {
    let (Some(run), Some(expected), Some(intentional)) = (
        row.gates_run,
        row.gates_expected,
        intentional_skip_count(row),
    ) else {
        return false;
    };
    if expected == 0 {
        return false;
    }
    if intentional == 0 {
        run >= expected
    } else {
        run.checked_add(intentional) == Some(expected)
    }
}

/// The typed form of [`row_qualifies`]. Same decisions, but the outcome carries
/// its condition so a caller can tell a demonstrated full green from one
/// grandfathered through the count-only fallback with no coverage evidence.
pub fn row_qualification(row: &HistoryRow, sha: &str, pred: &QualifyingPredicate) -> Qualification {
    let req = &pred.require;
    if !(row.commit.as_deref() == Some(sha)
        && row.commit_anchored == Some(req.commit_anchored)
        && row.tree_dirty == Some(req.tree_dirty)
        && row.selection_mode.as_deref() == Some(req.selection_mode.as_str())
        && row.profile.as_deref() == Some(req.profile.as_str())
        && row.result.as_deref() == Some(req.result.as_str()))
    {
        return Qualification::Refused("identity/result clause");
    }
    // `pass` with a positive failure count is malformed; an absent count on a
    // pass row is treated as zero (old rows predate the field).
    if row.failures.unwrap_or(0) > req.failures_max {
        return Qualification::Refused("failures > failures_max");
    }
    // A demonstrated zero-test run is never a full green, at any schema.
    if row.executed_tests == Some(0) {
        return Qualification::Refused("executed_tests == 0");
    }
    if row
        .passed_tests
        .is_some_and(|passed| Some(passed) != row.executed_tests)
    {
        return Qualification::Refused("passed_tests != executed_tests for passing row");
    }
    if pred.gate_filtered_tests && row.filtered_tests != Some(0) {
        return Qualification::Refused("filtered_tests != 0");
    }
    let schema = row.schema_version.unwrap_or(0);
    let count_capable = schema >= pred.counts_schema;
    if count_capable && !gate_accounting_complete(row) {
        return Qualification::Refused("outer gate accounting incomplete");
    }
    let counts_present = row.executed_tests.is_some() && row.filtered_tests.is_some();
    let executed_ok = matches!(row.executed_tests, Some(n) if n >= req.executed_tests_min);
    let qualification = if count_capable {
        if !executed_ok {
            return Qualification::Refused("executed_tests below executed_tests_min");
        }
        // Coverage is not required when the predicate turns it off or the row
        // predates it; otherwise it must be present AND satisfied. `None` here
        // is a writer defect, and is refused rather than skipped.
        if !pred.coverage.per_node || schema < pred.coverage.applies_at_schema_min {
            return Qualification::FullCoverage;
        }
        match row.coverage.as_ref().map(coverage_verdict) {
            Some(CoverageVerdict::Satisfied) => Qualification::FullCoverage,
            Some(CoverageVerdict::Unsatisfied) => {
                Qualification::Refused("coverage reported inert or absent nodes")
            }
            Some(CoverageVerdict::Unavailable(why)) => Qualification::Refused(why),
            None => Qualification::Refused(
                "count-capable receipt carries no coverage object; unavailable coverage \
                 is refused, never accepted",
            ),
        }
    } else if counts_present {
        // Old-schema writer that carried counts but predates per-node coverage:
        // hold it to the strongest thing it can prove — nonzero execution.
        //
        // This is the count-based fallback. It stays accepted so the tightening
        // is not also a silent floor change, but it is now TYPED: coverage here
        // is UNAVAILABLE, not clean, and `coverage_demonstrated()` says so. A
        // consumer that needs completeness must not read this as a full green.
        if executed_ok {
            Qualification::CountsOnlyGrandfathered { schema }
        } else {
            Qualification::Refused("pre-coverage receipt below executed_tests_min")
        }
    } else {
        // Neither count present: an uncounted receipt is UNVERIFIED, not green.
        Qualification::Refused("no counts: receipt proves nothing")
    };
    if !qualification.accepted() {
        return qualification;
    }
    // PROVENANCE gate, ordered after the value clauses and before the class
    // gate exactly as in the Python twin. Like the class gate it can only
    // NARROW, and this position keeps every existing refusal reason
    // byte-identical, so activating it cannot silently re-attribute an
    // unrelated value failure to a missing writer.
    if producer_enforced_for(row, pred) {
        match producer_verdict(row, pred) {
            ProducerVerdict::Missing => {
                return Qualification::Refused("producer missing");
            }
            ProducerVerdict::UnknownWriter => {
                return Qualification::Refused("producer unknown-writer");
            }
            ProducerVerdict::Ok | ProducerVerdict::Grandfathered => {}
        }
    }
    // Cross-field evidence semantics are one Python authority, not parallel
    // Rust/Python comparisons. This includes the schema/coverage boundary:
    // schema-4 stays grandfathered only with absent/null coverage, while a
    // non-null coverage claim requires schema-5+. Admission and base evidence
    // remain in the same delegated check.
    // BOTH halves of the pair are load-bearing here, and they answer different
    // questions: `accepted()` is the gate, `indeterminate_reason()` is the
    // message. Using the gate alone is what produced one string for six faults.
    let evidence = shared_evidence_outcome(row, pred);
    if !evidence.accepted() {
        return match evidence.indeterminate_reason() {
            // A READER fault, carried out in its own variant. The string names
            // which of the six it was, so the operator learns whether to fix PATH,
            // retry, redeploy, or file a verifier bug -- and the TYPE says the
            // receipt was never judged, so no consumer can render this as a
            // verdict against it.
            Some(reason) => Qualification::Indeterminate(reason),
            // The only genuine statement about the receipt.
            None => Qualification::Refused("shared evidence contract not satisfied"),
        };
    }
    // Green-class gate (from origin/main): a receipt passing every value clause
    // must ALSO be of a green class the predicate accepts. Expressed as a
    // REFUSAL rather than a bare `false` so the reason survives into the typed
    // outcome; an underivable class is refused, never defaulted to accepted.
    let Some(green_class) = derived_green_class(row) else {
        return Qualification::Refused("no derivable green class");
    };
    if !pred
        .accepts_green_class
        .iter()
        .any(|accepted| accepted == green_class)
    {
        return Qualification::Refused("green class not accepted by predicate");
    }
    qualification
}

/// Resolve the on-disk predicate path against the repo root. The literal path
/// lives only in [`PREDICATE_REL`].
pub fn predicate_path(root: &Path) -> PathBuf {
    root.join(PREDICATE_REL)
}

/// Load the predicate, honoring the resolution order in the module docs.
/// `root` supplies the on-disk location; a malformed override/on-disk file is a
/// hard error (the caller decides whether to panic or fail closed).
pub fn load(root: &Path) -> Result<QualifyingPredicate, String> {
    if let Some(path) = std::env::var_os(PREDICATE_ENV).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(path);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("{}: cannot read {PREDICATE_ENV}: {e}", path.display()))?;
        return QualifyingPredicate::parse(&text, &path.display().to_string());
    }
    let path = predicate_path(root);
    match std::fs::read_to_string(&path) {
        Ok(text) => QualifyingPredicate::parse(&text, &path.display().to_string()),
        // Not reachable on disk (e.g. a unit-test temp dir): fall back to the
        // compile-time snapshot of the very same file. A malformed on-disk file,
        // by contrast, propagates as an error above and is never masked.
        Err(_) => QualifyingPredicate::parse(EMBEDDED, "embedded qualifying-receipt.json"),
    }
}

/// The process-wide active predicate for callers without a `root` in hand
/// (`is_clean_full_pass`, `assess`). Resolved once and cached. Resolution
/// mirrors `ci-hub.rs::workspace_root` (script/exe dir -> git toplevel) so a run
/// from the parent finds the live file; on any failure it falls back to the
/// embedded snapshot rather than stranding every green.
pub fn active() -> &'static QualifyingPredicate {
    static ACTIVE: OnceLock<QualifyingPredicate> = OnceLock::new();
    ACTIVE.get_or_init(|| {
        let root = resolve_root();
        match load(&root) {
            Ok(pred) => pred,
            // load() only errors on a malformed override/on-disk file — a real
            // deploy defect. Fail LOUD: a landing gate must not run on a guessed
            // predicate.
            Err(message) => panic!("qualifying-receipt predicate unusable: {message}"),
        }
    })
}

/// Best-effort repo root for [`active`], mirroring `ci-hub.rs::workspace_root`.
/// Falls back to the current directory (the embed then covers a missing file).
fn resolve_root() -> PathBuf {
    let source = std::env::var_os("RUST_SCRIPT_PATH")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok());
    let start = source
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&start)
        .args(["rev-parse", "--show-toplevel"])
        .bounded_output();
    match output {
        Ok(out) if out.status.success() => {
            PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
        }
        _ => start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Build a schema-5 full-green row whose `coverage` object is supplied
    /// verbatim, so a test can OMIT a field rather than set it empty.
    fn row_with_coverage(sha: &str, coverage: &str) -> HistoryRow {
        serde_json::from_str(&format!(
            r#"{{"schema_version":5,"profile":"full","selection_mode":"full","commit":"{sha}",
                "commit_anchored":true,"tree_dirty":false,"result":"pass","failures":0,
                "executed_tests":427,"filtered_tests":0,"coverage":{coverage},
                "checks":1,"gates_run":1,"gates_expected":1,
                "gates":[{{"name":"test","result":"pass","exit_code":0}}],
                "producer":"hermit-validate-sh","admission":"ci-hub-validate-lock",
                "concurrent_validates":0,
                "concurrency_proof":"validate_lock_owner_ancestry",
                "base_sha":"1111111111111111111111111111111111111111",
                "base_tree":"2222222222222222222222222222222222222222",
                "reverie_base_sha":"3333333333333333333333333333333333333333",
                "reverie_base_tree":"4444444444444444444444444444444444444444"}}"#
        ))
        .unwrap()
    }

    /// A schema-5 full green carrying `finished_at` and, optionally, a
    /// `producer`, so provenance is the only clause any assertion turns on.
    fn row_with_producer(sha: &str, finished_at: &str, producer: Option<&str>) -> HistoryRow {
        let prod = match producer {
            Some(p) => format!(r#","producer":"{p}""#),
            None => String::new(),
        };
        serde_json::from_str(&format!(
            r#"{{"schema_version":5,"profile":"full","selection_mode":"full","commit":"{sha}",
                "commit_anchored":true,"tree_dirty":false,"result":"pass","failures":0,
                "executed_tests":427,"filtered_tests":0,"finished_at":"{finished_at}",
                "checks":1,"gates_run":1,"gates_expected":1,
                "gates":[{{"name":"test","result":"pass","exit_code":0}}],
                "coverage":{{"planned_test_nodes":19,"executed_test_nodes":19,
                "zero_executed_nodes":[],"absent_nodes":[]}},
                "admission":"ci-hub-validate-lock","concurrent_validates":0,
                "concurrency_proof":"validate_lock_owner_ancestry",
                "base_sha":"1111111111111111111111111111111111111111",
                "base_tree":"2222222222222222222222222222222222222222",
                "reverie_base_sha":"3333333333333333333333333333333333333333",
                "reverie_base_tree":"4444444444444444444444444444444444444444"{prod}}}"#
        ))
        .unwrap()
    }

    /// The shipped predicate with the provenance epoch FLIPPED ON. The live file
    /// is deliberately inert until all four writers deploy, so every refusal
    /// assertion must mutate it -- otherwise the test proves only that the gate
    /// compiles, not that it fires.
    fn enforcing() -> QualifyingPredicate {
        let mut pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        pred.producer.applies_from_finished_at = Some("2026-08-08T00:00:00Z".to_string());
        pred
    }

    #[test]
    fn count_capable_receipt_requires_complete_outer_gate_accounting() {
        let sha = "b".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let mut row = row_with_producer(&sha, "2026-08-08T09:00:00Z", Some("hermit-validate-sh"));
        assert!(
            row_qualifies(&row, &sha, &pred),
            "1 of 1 control must qualify"
        );

        row.gates_expected = Some(2);
        assert_eq!(
            row_qualification(&row, &sha, &pred),
            Qualification::Refused("outer gate accounting incomplete")
        );
    }

    #[test]
    fn passing_receipt_requires_any_present_passed_count_to_match_execution() {
        let sha = "b".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let mut row = row_with_producer(&sha, "2026-08-08T09:00:00Z", Some("hermit-validate-sh"));

        assert!(row_qualifies(&row, &sha, &pred));
        row.passed_tests = row.executed_tests;
        assert!(row_qualifies(&row, &sha, &pred));

        for contradictory in [Some(-1), Some(0), Some(426), Some(428)] {
            row.passed_tests = contradictory;
            assert_eq!(
                row_qualification(&row, &sha, &pred),
                Qualification::Refused("passed_tests != executed_tests for passing row")
            );
        }
    }

    /// BOTH DIRECTIONS of the provenance gate, in the engine that must agree
    /// clause-for-clause with `qualifying_receipt.py`.
    #[test]
    fn producer_gate_refuses_unattributable_rows_and_accepts_registered_writers() {
        let sha = "b".repeat(40);
        let pred = enforcing();

        // CONTROL: without this, every refusal below could be an unrelated clause.
        assert!(
            row_qualifies(
                &row_with_producer(&sha, "2026-08-08T09:00:00Z", Some("hermit-validate-sh")),
                &sha,
                &pred
            ),
            "control row must qualify"
        );

        // NEGATIVE: no producer, and an unregistered one. Presence is not the
        // bar -- an invented name is as unattributable as none.
        for (label, producer) in [("absent", None), ("unregistered", Some("hermit-valrs"))] {
            let row = row_with_producer(&sha, "2026-08-08T09:00:00Z", producer);
            assert!(
                matches!(
                    row_qualification(&row, &sha, &pred),
                    Qualification::Refused(_)
                ),
                "{label} producer must be refused under enforcement"
            );
        }

        // POSITIVE: every registered slug, so a typo is caught here rather than
        // at activation, when it would refuse that writer's every row.
        //
        // Census the exact SET, not its length. A bare `len() == 4` says
        // nothing about WHICH slugs are registered -- a rename plus an
        // addition cancel out and it still passes -- and it read as a defect
        // the moment the registry legitimately grew a fifth entry. It did:
        // `validate.rs` is the legacy bare slug hermit/scripts/validate.rs
        // still emits, kept registered so rows already written under it keep
        // verifying (see the note in qualifying-receipt.json). Naming the set
        // makes any registry change fail HERE, where the diff states what
        // changed, instead of at activation.
        let census: BTreeSet<&str> = pred.producer.known.iter().map(String::as_str).collect();
        assert_eq!(
            census,
            BTreeSet::from([
                "hermit-validate-sh",
                "hermit-validate-rs",
                "ci-hub-finalize-receipt",
                "reverie-validate-sh",
                // Legacy; drop once no live ledger row carries it.
                "validate.rs",
            ]),
            "registered-writer census changed -- update the writers, the \
             registry, and this list together"
        );
        for slug in &pred.producer.known {
            let row = row_with_producer(&sha, "2026-08-08T09:00:00Z", Some(slug));
            assert!(row_qualifies(&row, &sha, &pred), "{slug} must be accepted");
        }

        // GRANDFATHER: schema-4 history keeps its authority even when it lacks
        // both producer and admission fields.
        let mut old = row_with_producer(&sha, "2026-08-07T20:15:31Z", None);
        old.schema_version = Some(4);
        old.coverage = None;
        let old_qualification = row_qualification(&old, &sha, &pred);
        assert!(
            old_qualification.accepted(),
            "schema-4 row must survive: {old_qualification:?}"
        );
        assert_eq!(
            producer_verdict(&old, &pred),
            ProducerVerdict::Grandfathered
        );

        // FAIL-CLOSED: dropping the epoch key must not opt a row out.
        let undated = row_with_producer(&sha, "not-a-timestamp", None);
        assert!(producer_enforced_for(&undated, &pred));
        assert!(!row_qualifies(&undated, &sha, &pred));
    }

    /// The legacy producer epoch remains inert, but schema-5 admission now
    /// requires a registered producer. Schema-4 remains grandfathered.
    #[test]
    fn shipped_predicate_requires_admission_and_grandfathers_schema4() {
        let sha = "b".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        assert!(
            pred.producer.required,
            "the live file must DECLARE the column"
        );
        assert!(
            pred.producer.applies_from_finished_at.is_none(),
            "flipping this epoch requires all four writers deployed first"
        );
        let row = row_with_producer(&sha, "2026-08-08T09:00:00Z", None);
        assert_eq!(
            producer_verdict(&row, &pred),
            ProducerVerdict::Grandfathered,
            "the old timestamp-based producer gate remains staged"
        );
        assert!(
            !row_qualifies(&row, &sha, &pred),
            "schema-5 admission must refuse a missing producer"
        );
        let mut invalid_schema4 = row_with_producer(&sha, "2026-08-08T09:00:00Z", None);
        invalid_schema4.schema_version = Some(4);
        assert!(
            !row_qualifies(&invalid_schema4, &sha, &pred),
            "schema-4 carrying non-null coverage must be refused"
        );
        let mut schema4 = row;
        schema4.schema_version = Some(4);
        schema4.coverage = None;
        assert!(
            row_qualifies(&schema4, &sha, &pred),
            "schema-4 with absent coverage must remain grandfathered"
        );
        assert_eq!(
            shared_base_boundary_outcome(
                &schema4,
                &sha,
                &sha,
                Path::new("/nonexistent-hermit"),
                Path::new("/nonexistent-reverie"),
            ),
            BoundaryOutcome::Accepted,
            "the real Rust-to-Python merge-boundary bridge must preserve schema-4"
        );
    }

    /// ⚠️ SIX READER FAULTS MUST REACH THE CALLER AS SIX, NOT AS ONE `Refused`.
    ///
    /// `shared_evidence_outcome` distinguishes six ways the verifier can fail to
    /// produce an answer. Until now `row_qualification` folded all six -- and the
    /// genuine receipt verdict -- into `Qualification::Refused`, so a consumer
    /// rendering the reason said "this receipt is refused because python3 is not
    /// on PATH": the environment blamed on the evidence, which is the sentence
    /// `BoundaryOutcome`'s own doc comment warns about.
    ///
    /// This pins that each of the six arrives as its own `Indeterminate`, that all
    /// six remain mutually distinguishable, and that none of them is a `Refused`.

    #[test]
    fn all_six_reader_faults_stay_distinguishable_from_each_other_and_from_a_refusal() {
        // The exact six, as spelled in `shared_evidence_outcome`.
        let faults = [
            "verifier-unlaunchable",
            "verifier-stdin-unavailable",
            "verifier-request-undelivered",
            "verifier-not-reaped",
            "verifier-output-unparseable",
            "verifier-self-inconsistent",
        ];

        let mut seen = BTreeSet::new();
        for fault in faults {
            let q = Qualification::Indeterminate(fault);
            assert!(!q.accepted(), "{fault} must block");
            assert!(
                matches!(q, Qualification::Indeterminate(r) if r == fault),
                "{fault} must arrive as itself, not as a shared string"
            );
            assert!(
                !matches!(q, Qualification::Refused(_)),
                "{fault} is a READER fault; typing it as Refused blames the receipt \
                 for the environment"
            );
            seen.insert(fault);
        }
        assert_eq!(seen.len(), 6, "six faults must remain six, not collapse");

        // And the receipt verdict is the one thing that IS a Refused.
        let refused = Qualification::Refused("shared evidence contract not satisfied");
        assert!(!refused.accepted());
        assert!(
            !matches!(refused, Qualification::Indeterminate(_)),
            "a genuine refusal must not masquerade as a reader fault either -- the \
             collapse has to be impossible in BOTH directions"
        );
    }

    /// ⚠️ THE CONTROL: `accepted()` must be an ALLOW-LIST.
    ///
    /// Written as `!matches!(self, Refused(_))`, adding `Indeterminate` would have
    /// made an UNREADABLE receipt accepted by omission -- silently, with no line
    /// changed for a reviewer to notice. This is the assertion that would have
    /// caught it.
    #[test]
    fn an_unreadable_receipt_is_not_accepted_by_omission() {
        assert!(!Qualification::Indeterminate("verifier-unlaunchable").accepted());
        assert!(!Qualification::Refused("anything").accepted());
        // ...and the accepting variants still accept, so this is not just "block
        // everything", which would pass the assertions above while breaking the gate.
        assert!(Qualification::FullCoverage.accepted());
        assert!(Qualification::CountsOnlyGrandfathered { schema: 4 }.accepted());
    }

    /// The three states are distinguishable through the accessors, not only by
    /// matching. A caller that only needs the gate must still be unable to
    /// mistake a reader fault for a receipt refusal.
    #[test]
    fn boundary_outcome_keeps_the_two_blocking_states_apart() {
        assert!(BoundaryOutcome::Accepted.accepted());
        assert!(!BoundaryOutcome::Refused.accepted());
        assert!(!BoundaryOutcome::Indeterminate("no python3").accepted());

        // Both non-accepting states BLOCK, and that is deliberate: an
        // unconsultable authority must never make a receipt more authoritative.
        // What must stay distinguishable is WHY.
        assert_eq!(BoundaryOutcome::Refused.indeterminate_reason(), None);
        assert_eq!(
            BoundaryOutcome::Indeterminate("no python3").indeterminate_reason(),
            Some("no python3")
        );
        assert_eq!(BoundaryOutcome::Accepted.indeterminate_reason(), None);
    }

    /// NEGATIVE (mutation): a coverage object that OMITS a failure list must be
    /// REFUSED. Before this was typed, `#[serde(default)]` turned the missing
    /// field into `[]` and the row read as fully covered — while the Python
    /// verifier, whose `cov.get("absent_nodes") == []` is False for a missing
    /// key, refused the very same input. Rust was the permissive half.
    #[test]
    fn coverage_missing_a_failure_list_is_refused_not_defaulted_to_clean() {
        let sha = "b".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();

        for (label, coverage) in [
            (
                "absent_nodes omitted",
                r#"{"planned_test_nodes":19,"zero_executed_nodes":[]}"#,
            ),
            (
                "zero_executed_nodes omitted",
                r#"{"planned_test_nodes":19,"absent_nodes":[]}"#,
            ),
            ("both omitted", r#"{"planned_test_nodes":19}"#),
        ] {
            let row = row_with_coverage(&sha, coverage);
            let verdict = row_qualification(&row, &sha, &pred);
            assert!(
                matches!(verdict, Qualification::Refused(_)),
                "{label}: unreported coverage must be refused, got {verdict:?}"
            );
            assert!(
                !row_qualifies(&row, &sha, &pred),
                "{label}: bool view must agree"
            );
        }
    }

    /// POSITIVE CONTROL: the same row with both lists REPORTED and empty still
    /// qualifies, and is typed as demonstrated full coverage. Without this, a
    /// verifier that refused everything would pass the negative test above.
    #[test]
    fn complete_coverage_still_qualifies_as_full() {
        let sha = "c".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let row = row_with_coverage(
            &sha,
            r#"{"planned_test_nodes":19,"executed_test_nodes":19,
                "zero_executed_nodes":[],"absent_nodes":[]}"#,
        );
        assert_eq!(
            row_qualification(&row, &sha, &pred),
            Qualification::FullCoverage
        );
        assert!(row_qualifies(&row, &sha, &pred));
    }

    /// A REPORTED non-empty list is Unsatisfied, distinct from Unavailable —
    /// the two refusals must not be collapsed, because only one of them is a
    /// producer defect.
    #[test]
    fn reported_inert_nodes_are_unsatisfied_not_unavailable() {
        let cov: CoverageRow = serde_json::from_str(
            r#"{"planned_test_nodes":19,"zero_executed_nodes":["test.cli"],"absent_nodes":[]}"#,
        )
        .unwrap();
        assert_eq!(coverage_verdict(&cov), CoverageVerdict::Unsatisfied);
        let missing: CoverageRow =
            serde_json::from_str(r#"{"planned_test_nodes":19,"absent_nodes":[]}"#).unwrap();
        assert!(matches!(
            coverage_verdict(&missing),
            CoverageVerdict::Unavailable(_)
        ));
    }

    /// The count-only fallback stays ACCEPTED (this typing is not a silent floor
    /// change) but is no longer indistinguishable from a full green: coverage is
    /// UNAVAILABLE on such a row and `coverage_demonstrated()` says so.
    /// Measured on the live ledger: 72 of 109 currently-qualifying rows are this
    /// class, with no coverage object at all.
    #[test]
    fn count_only_grandfather_is_accepted_but_typed_as_not_demonstrated() {
        let sha = "d".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let row: HistoryRow = serde_json::from_str(&format!(
            r#"{{"schema_version":3,"profile":"full","selection_mode":"full","commit":"{sha}",
                "commit_anchored":true,"tree_dirty":false,"result":"pass","failures":0,
                "executed_tests":427,"filtered_tests":0}}"#
        ))
        .unwrap();
        let verdict = row_qualification(&row, &sha, &pred);
        assert_eq!(
            verdict,
            Qualification::CountsOnlyGrandfathered { schema: 3 }
        );
        assert!(
            verdict.accepted(),
            "behaviour must be unchanged for existing rows"
        );
        assert!(
            !verdict.coverage_demonstrated(),
            "a receipt with no coverage evidence must not read as demonstrated coverage"
        );
    }

    /// A count-CAPABLE receipt with no coverage object at all is a writer defect
    /// and is refused — it must not fall through to the count-only fallback.
    #[test]
    fn count_capable_row_without_coverage_is_refused() {
        let sha = "e".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let row: HistoryRow = serde_json::from_str(&format!(
            r#"{{"schema_version":5,"profile":"full","selection_mode":"full","commit":"{sha}",
                "commit_anchored":true,"tree_dirty":false,"result":"pass","failures":0,
                "executed_tests":427,"filtered_tests":0}}"#
        ))
        .unwrap();
        assert!(matches!(
            row_qualification(&row, &sha, &pred),
            Qualification::Refused(_)
        ));
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    /// The embedded snapshot must always parse — it is the fallback of last
    /// resort and a parse failure would strand every green in a temp-dir run.
    #[test]
    fn embedded_snapshot_parses() {
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        assert!(pred.counts_schema >= 1);
        assert!(pred.require.executed_tests_min >= 1);
    }

    /// A tightened override MOVES the answer for the same row — the core of the
    /// mutation proof, exercised here at the Rust predicate boundary.
    #[test]
    fn tightening_executed_min_moves_the_answer() {
        let dir = std::env::temp_dir().join(format!(
            "qrp-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let live = write(&dir, "live.json", EMBEDDED);
        let tightened = write(
            &dir,
            "tight.json",
            &EMBEDDED.replace(
                "\"executed_tests_min\": 1",
                "\"executed_tests_min\": 999999",
            ),
        );
        let sha = "a".repeat(40);
        // Build the row the way the rest of the crate's tests do (HistoryRow has
        // no Default): a genuine schema-5 full green carrying counts + coverage.
        let row: HistoryRow = serde_json::from_str(&format!(
            r#"{{"schema_version":5,"profile":"full","selection_mode":"full","commit":"{sha}",
                "commit_anchored":true,"tree_dirty":false,"result":"pass","failures":0,
                "executed_tests":740,"filtered_tests":3,
                "checks":1,"gates_run":1,"gates_expected":1,
                "gates":[{{"name":"test","result":"pass","exit_code":0}}],
                "coverage":{{"planned_test_nodes":4,"executed_test_nodes":4,
                    "zero_executed_nodes":[],"absent_nodes":[]}},
                "producer":"hermit-validate-sh","admission":"ci-hub-validate-lock",
                "concurrent_validates":0,
                "concurrency_proof":"validate_lock_owner_ancestry",
                "base_sha":"1111111111111111111111111111111111111111",
                "base_tree":"2222222222222222222222222222222222222222",
                "reverie_base_sha":"3333333333333333333333333333333333333333",
                "reverie_base_tree":"4444444444444444444444444444444444444444"}}"#
        ))
        .unwrap();
        let live_pred =
            QualifyingPredicate::parse(&std::fs::read_to_string(&live).unwrap(), "live").unwrap();
        let tight_pred =
            QualifyingPredicate::parse(&std::fs::read_to_string(&tightened).unwrap(), "tight")
                .unwrap();
        assert!(
            row_qualifies(&row, &sha, &live_pred),
            "live must accept the genuine green"
        );
        assert!(
            !row_qualifies(&row, &sha, &tight_pred),
            "tightened must reject it"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hard_positive_and_soft_rebase_only_negative_share_the_policy() {
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let sha = "a".repeat(40);
        let validated = "c".repeat(40);
        let hard: HistoryRow = serde_json::from_value(serde_json::json!({
            "schema_version": 5,
            "profile": "full",
            "selection_mode": "full",
            "commit": sha,
            "commit_anchored": true,
            "tree_dirty": false,
            "result": "pass",
            "failures": 0,
            "executed_tests": 740,
            "filtered_tests": 3,
            "checks": 1,
            "gates_run": 1,
            "gates_expected": 1,
            "gates": [{"name": "test", "result": "pass", "exit_code": 0}],
            "coverage": {
                "planned_test_nodes": 4,
                "executed_test_nodes": 4,
                "zero_executed_nodes": [],
                "absent_nodes": []
            },
            "producer": "hermit-validate-sh",
            "admission": "ci-hub-validate-lock",
            "concurrent_validates": 0,
            "concurrency_proof": "validate_lock_owner_ancestry",
            "base_sha": "1111111111111111111111111111111111111111",
            "base_tree": "2222222222222222222222222222222222222222",
            "reverie_base_sha": "3333333333333333333333333333333333333333",
            "reverie_base_tree": "4444444444444444444444444444444444444444"
        }))
        .unwrap();
        assert!(row_qualifies(&hard, &sha, &pred));

        let mut soft_value = serde_json::to_value(&hard).unwrap();
        soft_value["validated_head_sha"] = serde_json::json!(validated);
        soft_value["inherited_from"] = serde_json::json!({
            "delta_kind": "rebase-only",
            "upstream_commits": 0,
            "branch_commits": 0,
            "patch_identical": true,
            "force_full_paths": []
        });
        soft_value["green_class"] = serde_json::json!("soft-rebase-only");
        let soft: HistoryRow = serde_json::from_value(soft_value).unwrap();
        assert!(
            !row_qualifies(&soft, &sha, &pred),
            "hard-only policy must not promote inherited soft evidence"
        );
    }

    #[test]
    fn soft_provenance_refuses_bool_counts_and_non_string_paths() {
        let sha = "a".repeat(40);
        let validated = "c".repeat(40);
        let base = serde_json::json!({
            "schema_version": 5,
            "profile": "full",
            "selection_mode": "full",
            "commit": sha,
            "commit_anchored": true,
            "tree_dirty": false,
            "result": "pass",
            "failures": 0,
            "executed_tests": 740,
            "coverage": {
                "planned_test_nodes": 4,
                "executed_test_nodes": 4,
                "zero_executed_nodes": [],
                "absent_nodes": []
            },
            "validated_head_sha": validated,
            "inherited_from": {
                "delta_kind": "rebase-only",
                "upstream_commits": 0,
                "branch_commits": 0,
                "force_full_paths": []
            },
            "green_class": "soft-rebase-only"
        });
        let valid: HistoryRow = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(derived_green_class(&valid), Some("soft-rebase-only"));

        let mut validated_number = base.clone();
        validated_number["validated_head_sha"] = serde_json::json!(7);
        let validated_number: HistoryRow = serde_json::from_value(validated_number).unwrap();
        assert_eq!(derived_green_class(&validated_number), None);

        let mut branch_bool = base.clone();
        branch_bool["inherited_from"]["branch_commits"] = serde_json::json!(false);
        let branch_bool: HistoryRow = serde_json::from_value(branch_bool).unwrap();
        assert_eq!(derived_green_class(&branch_bool), None);

        let mut upstream_bool = base.clone();
        upstream_bool["inherited_from"]["upstream_commits"] = serde_json::json!(false);
        let upstream_bool: HistoryRow = serde_json::from_value(upstream_bool).unwrap();
        assert_eq!(derived_green_class(&upstream_bool), None);

        let mut upstream_too_large = base.clone();
        upstream_too_large["inherited_from"]["delta_kind"] =
            serde_json::json!("rebase-plus-upstream");
        upstream_too_large["inherited_from"]["upstream_commits"] =
            serde_json::from_str("18446744073709551616").unwrap();
        upstream_too_large["green_class"] = serde_json::json!("soft-upstream-delta");
        let upstream_too_large: HistoryRow = serde_json::from_value(upstream_too_large).unwrap();
        assert_eq!(derived_green_class(&upstream_too_large), None);

        let mut null_path_list = base.clone();
        null_path_list["inherited_from"]["force_full_paths"] = serde_json::Value::Null;
        let null_path_list: HistoryRow = serde_json::from_value(null_path_list).unwrap();
        assert_eq!(derived_green_class(&null_path_list), None);

        let mut non_string_path = base;
        non_string_path["inherited_from"] = serde_json::json!({
            "delta_kind": "rebase-plus-upstream",
            "upstream_commits": 1,
            "branch_commits": 0,
            "force_full_paths": ["ci/run-node.sh", 7]
        });
        non_string_path["green_class"] = serde_json::json!("soft-force-full-touched");
        let non_string_path: HistoryRow = serde_json::from_value(non_string_path).unwrap();
        assert_eq!(derived_green_class(&non_string_path), None);
    }
}
