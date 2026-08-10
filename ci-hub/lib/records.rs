//! Typed schemas consumed by the ci-hub front door.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

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
}
