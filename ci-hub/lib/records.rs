//! Typed schemas consumed by the ci-hub front door.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ObligationRecord {
    pub schema_version: u32,
    pub obligation_id: String,
    pub repo: String,
    pub landed_sha: String,
    pub opened_at: String,
    pub overall_state: String,
    pub local: VerificationState,
    pub github: VerificationState,
    #[serde(default)]
    pub failure_summary: Option<String>,
    #[serde(default)]
    pub recommendation: Option<Recommendation>,
    #[serde(default)]
    pub remediation: Option<RemediationState>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ObligationRecord {
    pub fn is_closed(&self) -> bool {
        matches!(self.overall_state.as_str(), "satisfied" | "remediated")
    }

    pub fn remediation_required(&self) -> bool {
        self.overall_state == "remediation_required"
    }

    pub fn recommendation_action(&self) -> &str {
        self.recommendation
            .as_ref()
            .and_then(|recommendation| recommendation.action.as_deref())
            .unwrap_or("-")
    }

    pub fn dispatch_state(&self) -> &str {
        self.remediation
            .as_ref()
            .and_then(|remediation| remediation.dispatch.as_ref())
            .and_then(|dispatch| dispatch.state.as_deref())
            .unwrap_or("-")
    }
}

impl VerificationState {
    pub fn receipt_disposition(&self, redispatch_limit: u64) -> Value {
        let state = self.state.as_str();
        if matches!(state, "pending" | "starting" | "running") {
            return json!({
                "state": "producer_active",
                "will_consume_later_receipt": true,
                "reason": "an obligation-owned local producer is active",
            });
        }
        if matches!(state, "green" | "red") {
            return json!({
                "state": "receipt_observed",
                "will_consume_later_receipt": false,
                "reason": "the local leg already carries a terminal receipt verdict",
            });
        }
        if state != "no_result" {
            return json!({
                "state": "unavailable",
                "will_consume_later_receipt": false,
                "reason": format!("local state {state:?} has no receipt-consumption contract"),
            });
        }
        let spent = match self.extra.get("redispatch_count") {
            None => 0,
            Some(value) => match value.as_u64() {
                Some(spent) => spent,
                None => {
                    return json!({
                        "state": "unavailable",
                        "will_consume_later_receipt": false,
                        "reason": "local redispatch count is malformed",
                    });
                }
            },
        };
        let remaining = redispatch_limit.saturating_sub(spent);
        if remaining == 0 {
            return json!({
                "state": "receipt_window_exhausted",
                "will_consume_later_receipt": false,
                "redispatch_count": spent,
                "redispatch_limit": redispatch_limit,
                "automatic_rechecks_remaining": remaining,
                "reason": "bounded local redispatches are exhausted; use the explicit fix-forward/revert resolution path",
            });
        }
        let source_available = self
            .extra
            .get("source")
            .and_then(Value::as_str)
            .is_some_and(|source| Path::new(source).is_dir());
        if !source_available {
            return json!({
                "state": "receipt_window_unavailable",
                "will_consume_later_receipt": false,
                "redispatch_count": spent,
                "redispatch_limit": redispatch_limit,
                "automatic_rechecks_remaining": remaining,
                "reason": "no readable donor checkout exists for the remaining redispatch",
            });
        }
        json!({
            "state": "awaiting_receipt",
            "will_consume_later_receipt": true,
            "redispatch_count": spent,
            "redispatch_limit": redispatch_limit,
            "automatic_rechecks_remaining": remaining,
            "reason": "an obligation-owned bounded local redispatch remains available",
        })
    }

    pub fn receipt_disposition_state(&self) -> &str {
        self.extra
            .get("receipt_disposition")
            .and_then(Value::as_object)
            .and_then(|disposition| disposition.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("unclassified")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerificationState {
    pub state: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Recommendation {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemediationState {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub dispatch: Option<DispatchState>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DispatchState {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub acknowledged_by: Option<String>,
    #[serde(default)]
    pub acknowledged_session: Option<String>,
    #[serde(default)]
    pub acknowledgement: Option<DispatchAcknowledgement>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DispatchAcknowledgement {
    pub outcome: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub recorded_at: Option<String>,
    #[serde(default)]
    pub recorded_by: Option<String>,
    #[serde(default)]
    pub recorded_session: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// The stable fields emitted by `validate/aggregate.py --json` and JSONL stores.
/// Optional fields reflect honest reconstructed rows where a measurement was not
/// available; unrecognized fields are retained for forward compatibility.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HistoryRow {
    #[serde(default)]
    pub schema_version: Option<u32>,
    /// Stable validation-run identity. Schema-6 rows require this separately
    /// from `record_id`, because finalization mints a correction record while
    /// preserving the run and its per-cell evidence.
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub slot: Option<String>,
    /// Product the run validated: "hermit" or "reverie". Absent on pre-`repo`
    /// hermit ledger rows (aggregate.py defaults those to "hermit").
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub selection_mode: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub commit_anchored: Option<bool>,
    #[serde(default)]
    pub tree_dirty: Option<bool>,
    #[serde(default)]
    pub result: Option<String>,
    /// Process exit from the validation driver. A signal/cancellation exit can
    /// truncate a run after only passing gates; it is not a product failure.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Tests EXECUTED per the run's own test-runner banners. `None` = unknown (no
    /// banner in the log), distinct from `Some(0)` = a demonstrably inert green
    /// (a `--features`-gated build that compiled the tests out). A green carries
    /// this so a reader — and the landing predicate — can tell a real pass from a
    /// no-result wearing a success badge.
    #[serde(default)]
    pub executed_tests: Option<i64>,
    /// Tests FILTERED OUT by the run's selection. `Some(0)` alongside
    /// `executed_tests == Some(0)` is an empty target; `Some(n>0)` is a filtered
    /// subset (the `1 passed; 154 filtered out` narrowed-scope trap).
    #[serde(default)]
    pub filtered_tests: Option<i64>,
    /// Per-DAG-node test-coverage obligation outcome, written by the producer
    /// (Hermit's Rust driver) from the run's own per-node banners/terminal lines. Carries
    /// the CONDITION with the value (Proxy Binding): the NAMES of any inert or
    /// absent nodes travel in the receipt, so the landing predicate decides from
    /// receipt fields alone and never re-reads a log. `None` on a pre-coverage
    /// receipt; a count-capable receipt (schema >= COUNTS_SCHEMA) that omits it is
    /// a writer defect. Supersedes the blunt `filtered_tests == 0` predicate.
    #[serde(default)]
    pub coverage: Option<CoverageRow>,
    /// Whether the run covered the FULL profile (`level == "full"`), not a
    /// partial `*-only` profile whose pass reads identically to a full green.
    #[serde(default)]
    pub full_coverage: Option<bool>,
    #[serde(default)]
    pub checks: Option<u64>,
    /// Explicit completed/expected outer-gate counts. `checks` is retained for
    /// old rows; new failure evidence carries both names so completeness is
    /// observable rather than inferred from a profile name.
    #[serde(default)]
    pub gates_run: Option<u64>,
    #[serde(default)]
    pub gates_expected: Option<u64>,
    #[serde(default)]
    pub failures: Option<u64>,
    /// `-j` passed to the CI DAG lane, and the number of other validates
    /// observed concurrently. A failure without these conditions cannot be
    /// promoted to a durable known-bad verdict.
    #[serde(default)]
    pub dag_jobs: Option<u64>,
    #[serde(default)]
    pub concurrent_validates: Option<u64>,
    /// The failed cell was in the measured-flaky registry at run time. Such a
    /// red needs a solo `-j 4` confirmation before it is durable.
    #[serde(default)]
    pub known_flaky_failure: Option<bool>,
    /// This row is the required solo `-j 4` confirmation of an earlier
    /// contended or known-flaky red.
    #[serde(default)]
    pub solo_rerun_confirmation: Option<bool>,
    #[serde(default)]
    pub real_seconds: Option<f64>,
    #[serde(default)]
    pub user_seconds: Option<f64>,
    #[serde(default)]
    pub sys_seconds: Option<f64>,
    #[serde(default)]
    pub log_file: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    /// Receipt-bound per-cell comparison evidence. Historical rows predate this
    /// field and remain readable as `None`; schema-6 rows are required to carry
    /// a complete value by the shared qualifying-receipt predicate.
    #[serde(default)]
    pub cell_results: Option<CellResultsEvidence>,
    #[serde(default)]
    pub gates: Vec<GateHistoryRow>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Per-DAG-node test-coverage obligation outcome. The producer computes this from
/// the run's per-node `[node] test result:` banners (executed/filtered sums, and
/// whether the node emitted any banner) and `[node] ✓ PASS/✗ FAIL` terminal lines
/// (which nodes actually RAN), crossed against the PLANNED set of `test.*` DAG
/// nodes in the lane manifests. The obligation is SATISFIED iff
/// `planned_test_nodes > 0` and both failure lists were reported and empty.
/// This is the per-node replacement for the blunt aggregate `filtered_tests == 0`
/// predicate, which could not distinguish a full run's legitimate cross-shard
/// filtering (~693 tests) from a narrowed-subset masquerade.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct CoverageRow {
    /// Test-bearing DAG nodes the run PLANNED (manifest `test.*` steps for the
    /// lanes actually run). `0` = the producer could not determine a planned set;
    /// never treated as a satisfied obligation.
    #[serde(default)]
    pub planned_test_nodes: u64,
    /// Planned test nodes that actually executed tests (ran, and — where they emit
    /// libtest banners — with a positive passed-sum). Diagnostic.
    #[serde(default)]
    pub executed_test_nodes: u64,
    /// NAMES of planned test nodes that RAN and emitted libtest banner(s) whose
    /// passed-sum was 0 — an inert green (every crate filtered-to-empty or
    /// compiled-out). A banner-less node (legit shell/e2e) is NOT listed here.
    ///
    /// `None` = THE PRODUCER DID NOT REPORT THIS LIST, which is not the same as
    /// reporting an empty one. It must never satisfy the obligation: an absent
    /// list is unknown, and unknown is refused. This was `Vec<String>` with
    /// `#[serde(default)]`, so a receipt that simply omitted the field
    /// deserialized to `[]` and read as "no inert nodes" — a pass. Python's
    /// `cov.get("zero_executed_nodes") == []` is `False` for a missing key and
    /// therefore already refused it, so the two verifiers disagreed on exactly
    /// this input, with Rust the permissive one.
    #[serde(default)]
    pub zero_executed_nodes: Option<Vec<String>>,
    /// NAMES of planned test nodes that produced NO terminal PASS/FAIL line at all
    /// — never ran / skipped / absent from the run.
    ///
    /// `None` = not reported; see [`Self::zero_executed_nodes`]. Refused, never
    /// treated as "no absent nodes".
    #[serde(default)]
    pub absent_nodes: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct GateHistoryRow {
    pub name: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub real_seconds: Option<f64>,
    /// For a test function extracted from a retained DAG log, the manifest node
    /// which emitted it. Ledger-native outer gates leave this unset.
    #[serde(default)]
    pub source_node: Option<String>,
    /// `outer_gate` when the named gate itself failed; `lane_substep` when the
    /// outer gate merely carried a failing DAG node.
    #[serde(default)]
    pub failure_origin: Option<String>,
    /// Canonical DAG node names for a lane-carried failure. Empty is not valid
    /// evidence when `failure_origin == lane_substep`.
    #[serde(default)]
    pub failed_substeps: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonStrictness {
    Stripped,
    Canonical,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComparedLogScope {
    Deterministic,
    Info,
    FullTrace,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ComparisonTier {
    CanonicalBitwise,
    ExitAndStreamEquality,
    ExecutionOnlySelfConsistent,
    DeclaredButUnverifiable,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ComparisonSpec {
    pub strictness: ComparisonStrictness,
    pub compare_logs: bool,
    pub log_scope: ComparedLogScope,
    pub strip_lines: bool,
    pub canonicalize_addresses: bool,
    pub full_trace: bool,
    pub exact_remainder: bool,
    pub stripped_prefixes: Vec<String>,
    pub canonicalizations: Vec<String>,
    pub ignore_lines: bool,
    pub skip_commit: bool,
    pub skip_detlog: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ComparedLogCounts {
    pub left: u64,
    pub right: u64,
}

/// A required key whose JSON value may be null. A plain `Option<T>` would make
/// an omitted key deserialize exactly like an explicit null, recreating the
/// evidence collapse this schema exists to prevent.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RequiredNullable<T> {
    Null,
    Value(T),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CellVerdict {
    ComparedAndMatched {
        comparison_tier: ComparisonTier,
        comparison: ComparisonSpec,
        bitwise_parity: bool,
        compared_log_messages: RequiredNullable<ComparedLogCounts>,
    },
    ComparedAndDiverged {
        comparison_tier: ComparisonTier,
        comparison: ComparisonSpec,
        bitwise_parity: bool,
        compared_log_messages: RequiredNullable<ComparedLogCounts>,
    },
    PerformsNoComparisonByDesign {
        comparison_tier: ComparisonTier,
        reason: String,
    },
    UnavailableWithReason {
        comparison_tier: ComparisonTier,
        reason: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct CellIdentity {
    pub lane: String,
    pub category: String,
    pub test: String,
    pub mode: String,
    pub backend: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CellResult {
    pub lane: String,
    pub category: String,
    pub test: String,
    pub mode: String,
    pub backend: String,
    pub cell_verdict: CellVerdict,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CellResultsArtifact {
    /// Parent-root-relative JSONL path below `ignored/validate/artifacts/`.
    /// The validation checkout is disposable; binding there would leave a
    /// receipt whose evidence vanishes when the unit is removed.
    pub path: String,
    pub sha256: String,
    pub row_count: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CellResultsEvidence {
    pub run_id: String,
    pub hermit_sha: String,
    pub source_tree_dirty: bool,
    pub selected_count: u64,
    pub recorded_count: u64,
    /// SHA-256 over canonical JSON of the sorted `selected` identities.
    pub population_sha256: String,
    pub artifact: CellResultsArtifact,
    pub selected: Vec<CellIdentity>,
    pub cells: Vec<CellResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obligation_schema_retains_unknown_fields() {
        let record: ObligationRecord = serde_json::from_str(
            r#"{
                "schema_version":1,
                "obligation_id":"o1",
                "repo":"rrnewton/hermit",
                "landed_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "opened_at":"2026-08-03T00:00:00Z",
                "overall_state":"open",
                "local":{"state":"running","pid":42},
                "github":{"state":"pending","run_ids":[]},
                "remediation":{"state":"triggered","dispatch":{"state":"sent_unacknowledged","wake_id":"w1"}},
                "future_field":"preserved"
            }"#,
        )
        .unwrap();
        assert!(!record.is_closed());
        assert_eq!(record.local.extra["pid"], 42);
        assert_eq!(record.extra["future_field"], "preserved");
        assert_eq!(record.dispatch_state(), "sent_unacknowledged");
        assert_eq!(
            record.remediation.unwrap().dispatch.unwrap().extra["wake_id"],
            "w1"
        );
    }

    #[test]
    fn obligation_schema_reports_typed_receipt_disposition() {
        let state: VerificationState = serde_json::from_str(
            r#"{
                "state":"no_result",
                "receipt_disposition":{
                    "state":"receipt_window_exhausted",
                    "will_consume_later_receipt":false
                }
            }"#,
        )
        .unwrap();
        assert_eq!(
            state.receipt_disposition_state(),
            "receipt_window_exhausted"
        );

        let mut legacy: VerificationState =
            serde_json::from_str(r#"{"state":"no_result","redispatch_count":2}"#).unwrap();
        legacy
            .extra
            .insert("receipt_disposition".into(), legacy.receipt_disposition(2));
        assert_eq!(
            legacy.receipt_disposition_state(),
            "receipt_window_exhausted"
        );
    }

    #[test]
    fn history_schema_accepts_unmeasured_cpu() {
        let row: HistoryRow = serde_json::from_str(
            r#"{
                "schema_version":1,
                "commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "result":"pass",
                "checks":36,
                "real_seconds":123,
                "user_seconds":null,
                "sys_seconds":null
            }"#,
        )
        .unwrap();
        assert_eq!(row.checks, Some(36));
        assert_eq!(row.real_seconds, Some(123.0));
        assert_eq!(row.user_seconds, None);
    }

    #[test]
    fn cell_verdict_schema_keeps_all_four_states_distinct() {
        let compared = r#"{
            "state":"compared-and-matched",
            "comparison_tier":"canonical-bitwise",
            "comparison":{
                "strictness":"canonical","compare_logs":true,"log_scope":"info",
                "strip_lines":false,"canonicalize_addresses":true,"full_trace":true,
                "exact_remainder":true,"stripped_prefixes":["real-wall-clock-prefix/v1"],
                "canonicalizations":["host-address-to-first-appearance-ordinal/v1"],
                "ignore_lines":false,"skip_commit":false,"skip_detlog":false
            },
            "bitwise_parity":true,
            "compared_log_messages":{"left":7,"right":7}
        }"#;
        assert!(matches!(
            serde_json::from_str::<CellVerdict>(compared).unwrap(),
            CellVerdict::ComparedAndMatched { .. }
        ));
        for (state, expected) in [
            ("compared-and-diverged", "diverged"),
            ("performs-no-comparison-by-design", "by-design"),
            ("unavailable-with-reason", "unavailable"),
        ] {
            let value = if state == "compared-and-diverged" {
                compared
                    .replace("compared-and-matched", state)
                    .replace("\"bitwise_parity\":true", "\"bitwise_parity\":false")
            } else {
                format!(
                    r#"{{"state":"{state}","comparison_tier":"declared-but-unverifiable","reason":"fixture reason"}}"#
                )
            };
            let verdict: CellVerdict = serde_json::from_str(&value).unwrap();
            assert_eq!(
                match verdict {
                    CellVerdict::ComparedAndDiverged { .. } => "diverged",
                    CellVerdict::PerformsNoComparisonByDesign { .. } => "by-design",
                    CellVerdict::UnavailableWithReason { .. } => "unavailable",
                    CellVerdict::ComparedAndMatched { .. } => "matched",
                },
                expected
            );
        }
    }

    #[test]
    fn compared_log_messages_key_is_required_but_explicit_null_is_retained() {
        let base = r#"{
            "state":"compared-and-matched",
            "comparison_tier":"execution-only-self-consistent",
            "comparison":{
                "strictness":"stripped","compare_logs":false,"log_scope":"deterministic",
                "strip_lines":true,"canonicalize_addresses":false,"full_trace":false,
                "exact_remainder":false,"stripped_prefixes":[],"canonicalizations":[],
                "ignore_lines":false,"skip_commit":false,"skip_detlog":false
            },
            "bitwise_parity":false,
            "compared_log_messages":null
        }"#;
        assert!(serde_json::from_str::<CellVerdict>(base).is_ok());
        let mut missing: serde_json::Value = serde_json::from_str(base).unwrap();
        missing
            .as_object_mut()
            .unwrap()
            .remove("compared_log_messages");
        assert!(serde_json::from_value::<CellVerdict>(missing).is_err());

        for retired in [
            "full-stdout-info-stack-heap",
            "stdout-info-stack-heap-spot-check",
            "legacy-unqualified",
            "unqualified-no-comparison",
            "unqualified-stdout-only",
            "unqualified-self-verify-only",
            "unqualified-tool-count-only",
        ] {
            let old_tier = base.replace("execution-only-self-consistent", retired);
            assert!(serde_json::from_str::<CellVerdict>(&old_tier).is_err());
        }
    }
}
