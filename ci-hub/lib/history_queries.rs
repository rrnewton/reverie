//! Shared local-validate history query engine.
//!
//! Both owner-facing directions use this index: `newest-green` scans a branch
//! newest-to-oldest for the latest passing evidence, while `first-bad` scans
//! recorded observations for the newest PASS -> FAIL transition. Keeping the
//! commit ordering and evidence rules here prevents the two answers drifting.

use crate::qualifying_receipt::{self, CoverageVerdict};
use crate::records::{GateHistoryRow, HistoryRow};
use crate::validate_status::{assess, newest, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const NEWEST_GREEN_CACHE_REL: &str = "ignored/ci-hub/newest-green-cache.json";
pub const CELL_EVIDENCE_CACHE_REL: &str = "ignored/ci-hub/local-cell-evidence-cache.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CoverageStrength {
    Full,
    SmartSelection,
    NarrowerProfile,
}

impl CoverageStrength {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::SmartSelection => "smart-selection",
            Self::NarrowerProfile => "narrower-profile",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ValidationEvidence {
    pub sha: String,
    pub finished_at: Option<String>,
    pub profile: String,
    pub selection_mode: String,
    /// Scope strength is reported only when this receipt actually carries a
    /// satisfied per-node coverage obligation. `None` is serialized as JSON
    /// null so a grandfathered schema-4 receipt cannot masquerade as full.
    pub coverage: Option<CoverageStrength>,
    pub coverage_satisfied: Option<bool>,
    pub coverage_status: String,
    pub result: String,
    pub log_file: Option<String>,
    /// Exact map-derived producer condition carried with a green verdict.
    pub producer_definition: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct NewestGreenReport {
    pub schema_version: u32,
    pub branch: String,
    pub branch_ref: String,
    pub branch_tip: String,
    pub gate_schema: String,
    pub gate_schema_floor: String,
    pub range_oldest_commit: String,
    pub branch_commits_in_range: usize,
    pub trustworthy_recorded_commits_in_range: usize,
    pub full_green_commits_in_range: usize,
    pub green: ValidationEvidence,
    pub commits_after_green: usize,
    pub commits_without_any_record: usize,
    pub commits_with_records: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct FirstBadReport {
    pub schema_version: u32,
    pub branch: String,
    pub branch_ref: String,
    pub query: String,
    pub matched_name: String,
    pub first_bad: ValidationEvidence,
    pub last_good: ValidationEvidence,
    pub commits_between: usize,
    pub commits_without_cell_record: usize,
    pub first_bad_commit_has_mixed_outcomes: bool,
    pub files_touched: Vec<String>,
    pub plausibility: String,
    pub source_node: Option<String>,
    pub error_excerpt: Vec<String>,
    pub load_context: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NewestGreenCache {
    pub schema_version: u32,
    pub branch: String,
    pub branch_ref: String,
    pub branch_tip: String,
    pub gate_schema_floor: String,
    pub ledger_path: String,
    pub ledger_len: u64,
    pub ledger_modified_ns: u128,
    /// Hash of the canonical verifier's currently allowed producer records.
    /// This changes on registry edits and at transition expiry.
    #[serde(default)]
    pub producer_definition_authority: String,
    pub report: NewestGreenReport,
}

/// Durable derived detail for a row in the append-only validation ledger. This
/// is not a parallel verdict store: a cached record is used only when its
/// `(commit, finished_at)` ledger row still exists.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RetainedCellEvidence {
    pub commit: String,
    pub finished_at: Option<String>,
    pub gates: Vec<GateHistoryRow>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CellEvidenceCache {
    pub schema_version: u32,
    pub records: Vec<RetainedCellEvidence>,
}

#[derive(Clone, Debug)]
pub enum NewestGreenOutcome {
    Found(Box<NewestGreenReport>),
    FailedOnly {
        branch_tip: String,
        recorded: usize,
        commits_in_range: usize,
    },
    NoEvidence {
        branch_tip: String,
        commits_in_range: usize,
        recorded: usize,
    },
}

#[derive(Clone, Debug)]
pub enum FirstBadOutcome {
    Found(Box<FirstBadReport>),
    FailureWithoutKnownGood {
        query: String,
        matched_name: String,
        failure: ValidationEvidence,
    },
    NoEvidence {
        query: String,
        available_names: Vec<String>,
    },
    NoTransition {
        query: String,
        matched_name: String,
        observations: usize,
    },
}

pub struct HistoryQueryEngine {
    commits: Vec<String>,
    rows_by_commit: BTreeMap<String, Vec<HistoryRow>>,
}

impl HistoryQueryEngine {
    /// `commits` are first-parent branch commits in newest-to-oldest order.
    pub fn new(commits: Vec<String>, rows: Vec<HistoryRow>) -> Self {
        let main: BTreeSet<&str> = commits.iter().map(String::as_str).collect();
        let mut rows_by_commit: BTreeMap<String, Vec<HistoryRow>> = BTreeMap::new();
        for row in rows {
            let Some(commit) = row.commit.as_deref() else {
                continue;
            };
            if main.contains(commit) {
                rows_by_commit
                    .entry(commit.to_string())
                    .or_default()
                    .push(row);
            }
        }
        Self {
            commits,
            rows_by_commit,
        }
    }

    pub fn newest_green(
        &self,
        branch: &str,
        branch_ref: &str,
        gate_schema: &str,
        gate_schema_floor: &str,
    ) -> NewestGreenOutcome {
        let branch_tip = self.commits.first().cloned().unwrap_or_default();
        let range_oldest_commit = self.commits.last().cloned().unwrap_or_default();
        let mut trustworthy_recorded = 0usize;
        let mut failed_recorded = 0usize;
        let mut full_green_commits = 0usize;
        let mut newest_full_green = None;
        for (index, sha) in self.commits.iter().enumerate() {
            let Some(rows) = self.rows_by_commit.get(sha) else {
                continue;
            };
            let assessment = assess(rows, sha);
            match assessment.verdict {
                Verdict::Validated => {
                    trustworthy_recorded += 1;
                    full_green_commits += 1;
                    if newest_full_green.is_none() {
                        let row = newest(&assessment.qualifying)
                            .expect("validated assessment has qualifying evidence");
                        newest_full_green = Some((index, sha, row.clone()));
                    }
                }
                Verdict::FailedOnRecord => {
                    trustworthy_recorded += 1;
                    failed_recorded += 1;
                }
                Verdict::NotValidated => {
                    // A clean anchored partial PASS is trustworthy evidence of
                    // its narrower scope, even though it cannot be the branch's
                    // full green. Preserve that accounting while excluding
                    // ambiguous red states below.
                    if rows.iter().any(|row| {
                        row.commit_anchored == Some(true)
                            && row.tree_dirty == Some(false)
                            && row.result.as_deref() == Some("pass")
                    }) {
                        trustworthy_recorded += 1;
                    }
                }
                Verdict::Truncated | Verdict::NeedsRerun | Verdict::NoResult => {}
            }
        }

        if let Some((index, sha, row)) = newest_full_green {
            let newer = &self.commits[..index];
            let missing = newer
                .iter()
                .filter(|commit| !self.rows_by_commit.contains_key(*commit))
                .count();
            let report = NewestGreenReport {
                schema_version: 4,
                branch: branch.to_string(),
                branch_ref: branch_ref.to_string(),
                branch_tip,
                gate_schema: gate_schema.to_string(),
                gate_schema_floor: gate_schema_floor.to_string(),
                range_oldest_commit,
                branch_commits_in_range: self.commits.len(),
                trustworthy_recorded_commits_in_range: trustworthy_recorded,
                full_green_commits_in_range: full_green_commits,
                green: evidence(sha, &row),
                commits_after_green: newer.len(),
                commits_without_any_record: missing,
                commits_with_records: newer.len() - missing,
            };
            return NewestGreenOutcome::Found(Box::new(report));
        }
        if failed_recorded > 0 {
            NewestGreenOutcome::FailedOnly {
                branch_tip,
                recorded: trustworthy_recorded,
                commits_in_range: self.commits.len(),
            }
        } else {
            NewestGreenOutcome::NoEvidence {
                branch_tip,
                commits_in_range: self.commits.len(),
                recorded: trustworthy_recorded,
            }
        }
    }

    pub fn first_bad(&self, query: &str, branch: &str, branch_ref: &str) -> FirstBadOutcome {
        let normalized = normalize(query);
        let mut available = BTreeSet::new();
        let mut observations: Vec<(usize, String, GateHistoryRow, HistoryRow)> = Vec::new();

        for (index, sha) in self.commits.iter().enumerate() {
            let Some(rows) = self.rows_by_commit.get(sha) else {
                continue;
            };
            for row in rows
                .iter()
                .filter(|row| row.commit_anchored == Some(true) && row.tree_dirty == Some(false))
            {
                for gate in &row.gates {
                    available.insert(gate.name.clone());
                }
                let Some(gate) = row
                    .gates
                    .iter()
                    .find(|gate| normalize(&gate.name) == normalized)
                else {
                    continue;
                };
                observations.push((index, sha.clone(), gate.clone(), row.clone()));
            }
        }

        if observations.is_empty() {
            let suggestions = available
                .into_iter()
                .filter(|name| {
                    let candidate = normalize(name);
                    candidate.contains(&normalized) || normalized.contains(&candidate)
                })
                .take(12)
                .collect();
            return FirstBadOutcome::NoEvidence {
                query: query.to_string(),
                available_names: suggestions,
            };
        }

        // Commit chronology first, then run chronology within one commit. A
        // later PASS must not erase an earlier FAIL at the same SHA: that is
        // positive flake evidence, not a reason to rewrite history as green.
        observations.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| {
                a.3.finished_at
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.3.finished_at.as_deref().unwrap_or(""))
            })
        });
        let matched_name = observations[0].2.name.clone();
        let mut latest_transition = None;
        let mut open_failure_epoch = None;
        let mut last_good = None;
        for observation in &observations {
            if gate_passed(&observation.2) {
                if let Some(epoch) = open_failure_epoch.take() {
                    latest_transition = Some(epoch);
                }
                last_good = Some(observation.clone());
            } else if gate_failed(&observation.2) && open_failure_epoch.is_none() {
                if let Some(good) = &last_good {
                    open_failure_epoch = Some((good.clone(), observation.clone()));
                }
            }
        }
        let transition = open_failure_epoch.or(latest_transition);
        if let Some((good, bad)) = transition {
            let lower = bad.0.min(good.0);
            let upper = bad.0.max(good.0);
            let between = upper.saturating_sub(lower + 1);
            let observed_indices: BTreeSet<usize> =
                observations.iter().map(|item| item.0).collect();
            let missing = (lower + 1..upper)
                .filter(|index| !observed_indices.contains(index))
                .count();
            let bad_results: BTreeSet<&str> = observations
                .iter()
                .filter(|item| item.0 == bad.0)
                .filter_map(|item| item.2.result.as_deref().or(item.2.kind.as_deref()))
                .collect();
            return FirstBadOutcome::Found(Box::new(FirstBadReport {
                schema_version: 1,
                branch: branch.to_string(),
                branch_ref: branch_ref.to_string(),
                query: query.to_string(),
                matched_name: bad.2.name.clone(),
                first_bad: gate_evidence(&bad.1, &bad.3, &bad.2),
                last_good: gate_evidence(&good.1, &good.3, &good.2),
                commits_between: between,
                commits_without_cell_record: missing,
                first_bad_commit_has_mixed_outcomes: bad_results.contains("pass")
                    && (bad_results.contains("fail") || bad_results.contains("timeout")),
                files_touched: Vec::new(),
                plausibility: "not-assessed".into(),
                source_node: bad.2.source_node.clone(),
                error_excerpt: error_excerpt(&bad.3, &bad.2.name),
                load_context: load_context(&bad.3),
            }));
        }

        if let Some((_, sha, gate, row)) =
            observations.iter().rev().find(|item| gate_failed(&item.2))
        {
            return FirstBadOutcome::FailureWithoutKnownGood {
                query: query.to_string(),
                matched_name,
                failure: gate_evidence(sha, row, gate),
            };
        }
        FirstBadOutcome::NoTransition {
            query: query.to_string(),
            matched_name,
            observations: observations.len(),
        }
    }
}

fn scope_strength(row: &HistoryRow) -> CoverageStrength {
    match (row.profile.as_deref(), row.selection_mode.as_deref()) {
        (Some("full"), Some("full")) => CoverageStrength::Full,
        (Some("full"), _) => CoverageStrength::SmartSelection,
        _ => CoverageStrength::NarrowerProfile,
    }
}

/// Report only coverage the receipt itself carries. The schema boundary and
/// per-node verdict come from the one shared qualifying-receipt authority;
/// profile/selection labels alone never manufacture coverage.
fn coverage(row: &HistoryRow) -> (Option<CoverageStrength>, Option<bool>, &'static str) {
    let predicate = qualifying_receipt::active();
    let schema = row.schema_version.unwrap_or(0);
    if !predicate.coverage.per_node || schema < predicate.coverage.applies_at_schema_min {
        return (None, None, "grandfathered-unknown");
    }
    match row
        .coverage
        .as_ref()
        .map(qualifying_receipt::coverage_verdict)
    {
        Some(CoverageVerdict::Satisfied) => (Some(scope_strength(row)), Some(true), "satisfied"),
        Some(CoverageVerdict::Unsatisfied) => (None, Some(false), "unsatisfied"),
        Some(CoverageVerdict::Unavailable(_)) | None => (None, None, "unavailable"),
    }
}

fn evidence(sha: &str, row: &HistoryRow) -> ValidationEvidence {
    let (coverage, coverage_satisfied, coverage_status) = coverage(row);
    ValidationEvidence {
        sha: sha.to_string(),
        finished_at: row.finished_at.clone(),
        profile: row.profile.clone().unwrap_or_else(|| "unknown".into()),
        selection_mode: row
            .selection_mode
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        coverage,
        coverage_satisfied,
        coverage_status: coverage_status.into(),
        result: row.result.clone().unwrap_or_else(|| "unknown".into()),
        log_file: row.log_file.clone(),
        producer_definition: row.extra.get("producer_definition").cloned(),
    }
}

fn gate_evidence(sha: &str, row: &HistoryRow, gate: &GateHistoryRow) -> ValidationEvidence {
    let mut value = evidence(sha, row);
    value.result = gate
        .result
        .clone()
        .or_else(|| gate.kind.clone())
        .unwrap_or_else(|| "unknown".into());
    value
}

fn gate_passed(gate: &GateHistoryRow) -> bool {
    gate.result.as_deref() == Some("pass") || gate.kind.as_deref() == Some("pass")
}

fn gate_failed(gate: &GateHistoryRow) -> bool {
    matches!(gate.result.as_deref(), Some("fail") | Some("failed"))
        || matches!(gate.kind.as_deref(), Some("fail") | Some("timeout"))
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Add DAG-node and Rust-test observations from the retained log named by the
/// ledger. The ledger remains the index/source of truth; an absent log simply
/// means cell detail was not retained and is reported as such.
pub fn enrich_rows_from_logs(rows: &mut [HistoryRow]) {
    for row in rows {
        let Some(path) = row.log_file.as_deref() else {
            continue;
        };
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut seen: BTreeSet<(String, String)> = row
            .gates
            .iter()
            .map(|gate| {
                (
                    normalize(&gate.name),
                    gate.result.clone().unwrap_or_default(),
                )
            })
            .collect();
        for mut gate in parse_log_observations(&raw) {
            if gate_failed(&gate) {
                let excerpt = scoped_error_excerpt(&raw, &gate);
                if !excerpt.is_empty() {
                    gate.extra
                        .insert("error_excerpt".into(), serde_json::json!(excerpt));
                }
            }
            let key = (
                normalize(&gate.name),
                gate.result.clone().unwrap_or_default(),
            );
            if seen.insert(key) {
                row.gates.push(gate);
            } else if let Some(existing) = row.gates.iter_mut().find(|existing| {
                normalize(&existing.name) == normalize(&gate.name) && existing.result == gate.result
            }) {
                existing.extra.extend(gate.extra);
            }
        }
    }
}

pub fn merge_retained_cell_evidence(rows: &mut [HistoryRow], cache: &CellEvidenceCache) {
    let retained: BTreeMap<(&str, Option<&str>), &RetainedCellEvidence> = cache
        .records
        .iter()
        .map(|record| {
            (
                (record.commit.as_str(), record.finished_at.as_deref()),
                record,
            )
        })
        .collect();
    for row in rows {
        let Some(commit) = row.commit.as_deref() else {
            continue;
        };
        let Some(record) = retained.get(&(commit, row.finished_at.as_deref())) else {
            continue;
        };
        let mut names: BTreeSet<(String, String)> = row
            .gates
            .iter()
            .map(|gate| {
                (
                    normalize(&gate.name),
                    gate.result.clone().unwrap_or_default(),
                )
            })
            .collect();
        for gate in &record.gates {
            let key = (
                normalize(&gate.name),
                gate.result.clone().unwrap_or_default(),
            );
            if names.insert(key) {
                row.gates.push(gate.clone());
            }
        }
    }
}

pub fn retained_cell_evidence(rows: &[HistoryRow]) -> CellEvidenceCache {
    let records = rows
        .iter()
        .filter_map(|row| {
            let gates: Vec<GateHistoryRow> = row
                .gates
                .iter()
                .filter(|gate| gate.source_node.is_some())
                .cloned()
                .collect();
            Some(RetainedCellEvidence {
                commit: row.commit.clone()?,
                finished_at: row.finished_at.clone(),
                gates,
            })
        })
        .collect();
    CellEvidenceCache {
        schema_version: 2,
        records,
    }
}

pub fn parse_log_observations(raw: &str) -> Vec<GateHistoryRow> {
    let mut observations = Vec::new();
    for raw_line in raw.lines() {
        let line = strip_ansi(raw_line);
        let source_node = line
            .strip_prefix('[')
            .and_then(|rest| rest.split_once(']'))
            .map(|(node, _)| node.to_string());
        if let Some(node) = source_node.as_deref() {
            let result = if line.contains("\u{2713} PASS") {
                Some("pass")
            } else if line.contains("\u{2717} FAIL") {
                Some("fail")
            } else {
                None
            };
            if let Some(result) = result {
                observations.push(GateHistoryRow {
                    name: node.to_string(),
                    result: Some(result.into()),
                    kind: None,
                    exit_code: None,
                    real_seconds: None,
                    source_node: Some(node.to_string()),
                    failure_origin: None,
                    failed_substeps: Vec::new(),
                    extra: BTreeMap::new(),
                });
            }
        }
        let Some(test_at) = line.find("test ") else {
            continue;
        };
        let test = &line[test_at + 5..];
        let Some((name, suffix)) = test.split_once(" ... ") else {
            continue;
        };
        if name == "result:" || name.is_empty() {
            continue;
        }
        let result = if suffix.starts_with("ok") {
            "pass"
        } else if suffix.starts_with("FAILED") {
            "fail"
        } else {
            continue;
        };
        observations.push(GateHistoryRow {
            name: name.to_string(),
            result: Some(result.into()),
            kind: None,
            exit_code: None,
            real_seconds: None,
            source_node,
            failure_origin: None,
            failed_substeps: Vec::new(),
            extra: BTreeMap::new(),
        });
    }
    observations
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for code in chars.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn error_excerpt(row: &HistoryRow, query: &str) -> Vec<String> {
    if let Some(lines) = row
        .gates
        .iter()
        .find(|gate| normalize(&gate.name) == normalize(query))
        .and_then(|gate| gate.extra.get("error_excerpt"))
        .and_then(|value| value.as_array())
    {
        return lines
            .iter()
            .filter_map(|line| line.as_str().map(str::to_string))
            .collect();
    }
    let Some(path) = row.log_file.as_deref() else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let gate = row
        .gates
        .iter()
        .find(|gate| normalize(&gate.name) == normalize(query))
        .cloned()
        .unwrap_or(GateHistoryRow {
            name: query.to_string(),
            result: Some("fail".into()),
            kind: None,
            exit_code: None,
            real_seconds: None,
            source_node: None,
            failure_origin: None,
            failed_substeps: Vec::new(),
            extra: BTreeMap::new(),
        });
    scoped_error_excerpt(&raw, &gate)
}

fn scoped_error_excerpt(raw: &str, gate: &GateHistoryRow) -> Vec<String> {
    let source_prefix = gate.source_node.as_deref().map(|node| format!("[{node}]"));
    raw.lines()
        .map(strip_ansi)
        .filter(|line| {
            let belongs = line.contains(&gate.name)
                || source_prefix
                    .as_deref()
                    .map(|prefix| line.starts_with(prefix))
                    .unwrap_or(false);
            let diagnostic = line.contains(&gate.name)
                || line.contains("panicked at")
                || line.contains("error:")
                || line.contains("\u{2717} FAIL");
            belongs && diagnostic
        })
        .map(|line| line.chars().take(300).collect())
        .take(20)
        .collect()
}

fn load_context(row: &HistoryRow) -> Option<String> {
    row.extra
        .get("load_context")
        .map(|value| value.to_string())
        .or_else(|| {
            let cpu = row.user_seconds? + row.sys_seconds?;
            let wall = row.real_seconds?;
            (wall > 0.0).then(|| {
                format!(
                    "run CPU/wall={:.1}; host-load-at-run-time not retained",
                    cpu / wall
                )
            })
        })
}

pub fn cache_matches(
    cache: &NewestGreenCache,
    branch: &str,
    branch_ref: &str,
    branch_tip: &str,
    gate_schema_floor: &str,
    ledger_path: &Path,
    ledger_len: u64,
    ledger_modified_ns: u128,
    producer_definition_authority: &str,
) -> bool {
    cache.schema_version == 6
        && cache.branch == branch
        && cache.branch_ref == branch_ref
        && cache.branch_tip == branch_tip
        && cache.gate_schema_floor == gate_schema_floor
        && cache.ledger_path == ledger_path.display().to_string()
        && cache.ledger_len == ledger_len
        && cache.ledger_modified_ns == ledger_modified_ns
        && cache.producer_definition_authority == producer_definition_authority
}

pub fn cache_path(root: &Path, override_path: &Option<PathBuf>) -> PathBuf {
    override_path
        .clone()
        .unwrap_or_else(|| root.join(NEWEST_GREEN_CACHE_REL))
}

pub fn cell_evidence_cache_path(root: &Path) -> PathBuf {
    root.join(CELL_EVIDENCE_CACHE_REL)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(sha: &str, at: &str, profile: &str, selection: &str, result: &str) -> HistoryRow {
        let mut value = serde_json::json!({
            "schema_version": 4,
            "finished_at": at,
            "profile": profile,
            "selection_mode": selection,
            "commit": sha,
            "commit_anchored": true,
            "tree_dirty": false,
            "result": result,
            "executed_tests": 36,
            "filtered_tests": 0,
            "gates": []
        });
        if matches!(result, "fail" | "failed" | "timeout") {
            value["exit_code"] = serde_json::json!(1);
            value["checks"] = serde_json::json!(1);
            value["gates_run"] = serde_json::json!(1);
            value["gates_expected"] = serde_json::json!(1);
            // A genuine clean failure must have exercised the full suite; the
            // base 36-count above is a pass-side placeholder that would now trip
            // the executed-test plausibility floor and demote these fixtures to
            // NeedsRerun. These rows model DURABLE failures, so carry a
            // plausible-full executed count (peer of complete_failure in
            // validate_status).
            value["executed_tests"] = serde_json::json!(765);
            value["failures"] = serde_json::json!(1);
            value["dag_jobs"] = serde_json::json!(4);
            value["concurrent_validates"] = serde_json::json!(0);
            value["known_flaky_failure"] = serde_json::json!(false);
            value["solo_rerun_confirmation"] = serde_json::json!(false);
            value["gates"] = serde_json::json!([{
                "name": "outer failure",
                "result": "fail",
                "exit_code": 1,
                "failure_origin": "outer_gate"
            }]);
        }
        serde_json::from_value(value).unwrap()
    }

    fn gate(mut row: HistoryRow, name: &str, result: &str) -> HistoryRow {
        row.gates.push(GateHistoryRow {
            name: name.into(),
            result: Some(result.into()),
            kind: None,
            exit_code: None,
            real_seconds: None,
            source_node: None,
            failure_origin: None,
            failed_substeps: Vec::new(),
            extra: BTreeMap::new(),
        });
        row
    }

    fn with_satisfied_coverage(mut row: HistoryRow) -> HistoryRow {
        row.schema_version = Some(5);
        row.coverage = Some(crate::records::CoverageRow {
            planned_test_nodes: 2,
            executed_test_nodes: 2,
            zero_executed_nodes: Some(Vec::new()),
            absent_nodes: Some(Vec::new()),
        });
        row.extra
            .insert("producer".into(), serde_json::json!("hermit-validate-sh"));
        row.extra.insert(
            "admission".into(),
            serde_json::json!("ci-hub-validate-lock"),
        );
        row.concurrent_validates = Some(0);
        row.extra.insert(
            "concurrency_proof".into(),
            serde_json::json!("validate_lock_owner_ancestry"),
        );
        row.extra
            .insert("base_sha".into(), serde_json::json!("1".repeat(40)));
        row.extra
            .insert("base_tree".into(), serde_json::json!("2".repeat(40)));
        row.extra
            .insert("reverie_base_sha".into(), serde_json::json!("3".repeat(40)));
        row.extra.insert(
            "reverie_base_tree".into(),
            serde_json::json!("4".repeat(40)),
        );
        row
    }

    fn newest_green(engine: HistoryQueryEngine) -> NewestGreenOutcome {
        engine.newest_green("main", "origin/main", "merge-gate-v2", "floor")
    }

    #[test]
    fn newest_green_uses_full_evidence_and_reports_gaps_and_profile() {
        let commits = vec!["tip".into(), "gap".into(), "selected".into(), "full".into()];
        let rows = vec![
            row("tip", "2026-08-03T03:00:00Z", "full", "full", "fail"),
            row(
                "selected",
                "2026-08-03T02:00:00Z",
                "full",
                "selective",
                "pass",
            ),
            row("full", "2026-08-03T01:00:00Z", "full", "full", "pass"),
        ];
        let NewestGreenOutcome::Found(report) =
            newest_green(HistoryQueryEngine::new(commits, rows))
        else {
            panic!("expected green")
        };
        assert_eq!(report.green.sha, "full");
        assert_eq!(report.green.coverage, None);
        assert_eq!(report.green.coverage_satisfied, None);
        assert_eq!(report.green.coverage_status, "grandfathered-unknown");
        let rendered = serde_json::to_value(&report.green).unwrap();
        assert_eq!(rendered["coverage"], serde_json::Value::Null);
        assert_eq!(rendered["coverage_satisfied"], serde_json::Value::Null);
        assert_eq!(rendered["coverage_status"], "grandfathered-unknown");
        assert_eq!(report.gate_schema, "merge-gate-v2");
        assert_eq!(report.gate_schema_floor, "floor");
        assert_eq!(report.commits_after_green, 3);
        assert_eq!(report.commits_without_any_record, 1);
        assert_eq!(report.commits_with_records, 2);
        assert_eq!(report.branch_commits_in_range, 4);
        assert_eq!(report.trustworthy_recorded_commits_in_range, 3);
        assert_eq!(report.full_green_commits_in_range, 1);
    }

    #[test]
    fn newest_green_refuses_three_clean_failures() {
        let commits = vec!["tip".into(), "middle".into(), "oldest".into()];
        let rows = vec![
            row("tip", "2026-08-03T03:00:00Z", "full", "full", "fail"),
            row("middle", "2026-08-03T02:00:00Z", "full", "full", "fail"),
            row("oldest", "2026-08-03T01:00:00Z", "full", "full", "fail"),
        ];

        assert!(matches!(
            newest_green(HistoryQueryEngine::new(commits, rows)),
            NewestGreenOutcome::FailedOnly {
                branch_tip,
                recorded: 3,
                commits_in_range: 3,
            } if branch_tip == "tip"
        ));
    }

    #[test]
    fn newest_green_selects_newest_of_three_clean_full_passes() {
        let commits = vec!["tip".into(), "middle".into(), "oldest".into()];
        let rows = vec![
            with_satisfied_coverage(row("tip", "2026-08-03T03:00:00Z", "full", "full", "pass")),
            row("middle", "2026-08-03T02:00:00Z", "full", "full", "pass"),
            row("oldest", "2026-08-03T01:00:00Z", "full", "full", "pass"),
        ];
        let NewestGreenOutcome::Found(report) =
            newest_green(HistoryQueryEngine::new(commits, rows))
        else {
            panic!("expected newest green")
        };

        assert_eq!(report.green.sha, "tip");
        assert_eq!(report.green.coverage, Some(CoverageStrength::Full));
        assert_eq!(report.green.coverage_satisfied, Some(true));
        assert_eq!(report.green.coverage_status, "satisfied");
        let rendered = serde_json::to_value(&report.green).unwrap();
        assert_eq!(rendered["coverage"], "full");
        assert_eq!(rendered["coverage_satisfied"], true);
        assert_eq!(rendered["coverage_status"], "satisfied");
        assert_eq!(report.commits_after_green, 0);
        assert_eq!(report.branch_commits_in_range, 3);
        assert_eq!(report.trustworthy_recorded_commits_in_range, 3);
        assert_eq!(report.full_green_commits_in_range, 3);
    }

    #[test]
    fn newest_green_counts_distinct_commits_not_duplicate_receipts() {
        let commits = vec!["tip".into(), "oldest".into()];
        let rows = vec![
            row("tip", "2026-08-03T03:00:00Z", "full", "full", "pass"),
            row("tip", "2026-08-03T04:00:00Z", "full", "full", "pass"),
            row("oldest", "2026-08-03T01:00:00Z", "full", "full", "pass"),
        ];
        let NewestGreenOutcome::Found(report) =
            newest_green(HistoryQueryEngine::new(commits, rows))
        else {
            panic!("expected newest green")
        };

        assert_eq!(report.trustworthy_recorded_commits_in_range, 2);
        assert_eq!(report.full_green_commits_in_range, 2);
    }

    #[test]
    fn newest_green_uses_finished_at_not_ledger_append_order() {
        let commits = vec!["tip".into()];
        let rows = vec![
            row("tip", "2026-08-04T12:00:00Z", "full", "full", "pass"),
            // The append-only ledger can receive an older run after a newer
            // one. Its last row is not necessarily its most recent evidence.
            row("tip", "2026-08-04T11:00:00Z", "full", "full", "pass"),
        ];
        let NewestGreenOutcome::Found(report) =
            newest_green(HistoryQueryEngine::new(commits, rows))
        else {
            panic!("expected newest green")
        };

        assert_eq!(
            report.green.finished_at.as_deref(),
            Some("2026-08-04T12:00:00Z")
        );
    }

    #[test]
    fn newest_green_does_not_call_a_partial_pass_failed() {
        let commits = vec!["tip".into(), "floor".into()];
        let rows = vec![row(
            "tip",
            "2026-08-04T16:27:33Z",
            "portable-strict-compat-only",
            "full",
            "pass",
        )];
        assert!(matches!(
            newest_green(HistoryQueryEngine::new(commits, rows)),
            NewestGreenOutcome::NoEvidence {
                branch_tip,
                commits_in_range: 2,
                recorded: 1,
            } if branch_tip == "tip"
        ));
    }

    #[test]
    fn newest_green_does_not_count_a_two_of_five_red_as_failure() {
        let commits = vec!["tip".into(), "floor".into()];
        let mut truncated = row("tip", "2026-08-04T17:16:12Z", "full", "full", "fail");
        truncated.checks = Some(2);
        truncated.gates_run = Some(2);
        truncated.gates_expected = Some(5);

        assert!(matches!(
            newest_green(HistoryQueryEngine::new(commits, vec![truncated])),
            NewestGreenOutcome::NoEvidence {
                branch_tip,
                commits_in_range: 2,
                recorded: 0,
            } if branch_tip == "tip"
        ));
    }

    #[test]
    fn first_bad_finds_newest_transition_without_treating_gap_as_pass() {
        let commits = vec!["bad2".into(), "bad1".into(), "gap".into(), "good".into()];
        let rows = vec![
            gate(
                row("bad2", "2026-08-03T04:00:00Z", "full", "full", "fail"),
                "cell",
                "fail",
            ),
            gate(
                row("bad1", "2026-08-03T03:00:00Z", "full", "full", "fail"),
                "cell",
                "fail",
            ),
            gate(
                row("good", "2026-08-03T01:00:00Z", "full", "full", "pass"),
                "cell",
                "pass",
            ),
        ];
        let FirstBadOutcome::Found(report) =
            HistoryQueryEngine::new(commits, rows).first_bad("cell", "main", "origin/main")
        else {
            panic!("expected transition")
        };
        assert_eq!(report.first_bad.sha, "bad1");
        assert_eq!(report.last_good.sha, "good");
        assert_eq!(report.commits_without_cell_record, 1);
    }

    #[test]
    fn later_pass_on_same_sha_is_reported_as_mixed_not_erased() {
        let commits = vec!["tip".into(), "good".into()];
        let rows = vec![
            gate(
                row("good", "2026-08-03T01:00:00Z", "full", "full", "pass"),
                "cell",
                "pass",
            ),
            gate(
                row("tip", "2026-08-03T02:00:00Z", "full", "full", "fail"),
                "cell",
                "fail",
            ),
            gate(
                row("tip", "2026-08-03T03:00:00Z", "full", "full", "pass"),
                "cell",
                "pass",
            ),
        ];
        let FirstBadOutcome::Found(report) =
            HistoryQueryEngine::new(commits, rows).first_bad("cell", "main", "origin/main")
        else {
            panic!("expected retained historical transition")
        };
        assert_eq!(report.first_bad.sha, "tip");
        assert_eq!(report.last_good.sha, "good");
        assert!(report.first_bad_commit_has_mixed_outcomes);
    }

    #[test]
    fn parses_runner_nodes_and_rust_test_functions() {
        let rows = parse_log_observations(
            "[lint.clippy] \u{2717} FAIL Clippy\n[test.liteinst_strict] test liteinst_detcore_strict_verify_micro_suite ... FAILED\n",
        );
        assert!(rows
            .iter()
            .any(|row| row.name == "lint.clippy" && gate_failed(row)));
        let test = rows
            .iter()
            .find(|row| row.name == "liteinst_detcore_strict_verify_micro_suite")
            .unwrap();
        assert_eq!(test.source_node.as_deref(), Some("test.liteinst_strict"));
    }

    #[test]
    fn dirty_or_unanchored_pass_is_not_green() {
        let mut dirty = row("tip", "2026-08-03T01:00:00Z", "full", "full", "pass");
        dirty.tree_dirty = Some(true);
        assert!(matches!(
            newest_green(HistoryQueryEngine::new(vec!["tip".into()], vec![dirty])),
            NewestGreenOutcome::NoEvidence { .. }
        ));
    }

    #[test]
    fn cache_invalidates_on_branch_tip_or_ledger_change() {
        let report = NewestGreenReport {
            schema_version: 4,
            branch: "main".into(),
            branch_ref: "origin/main".into(),
            branch_tip: "tip-a".into(),
            gate_schema: "merge-gate-v2".into(),
            gate_schema_floor: "floor".into(),
            range_oldest_commit: "root".into(),
            branch_commits_in_range: 2,
            trustworthy_recorded_commits_in_range: 1,
            full_green_commits_in_range: 1,
            green: evidence(
                "tip-a",
                &row("tip-a", "2026-08-03T01:00:00Z", "full", "full", "pass"),
            ),
            commits_after_green: 0,
            commits_without_any_record: 0,
            commits_with_records: 0,
        };
        let cache = NewestGreenCache {
            schema_version: 6,
            branch: "main".into(),
            branch_ref: "origin/main".into(),
            branch_tip: "tip-a".into(),
            gate_schema_floor: "floor".into(),
            ledger_path: "/tmp/ledger".into(),
            ledger_len: 100,
            ledger_modified_ns: 200,
            producer_definition_authority: "producer-a".into(),
            report,
        };
        let path = Path::new("/tmp/ledger");
        assert!(cache_matches(
            &cache,
            "main",
            "origin/main",
            "tip-a",
            "floor",
            path,
            100,
            200,
            "producer-a"
        ));
        assert!(!cache_matches(
            &cache,
            "main",
            "origin/main",
            "tip-b",
            "floor",
            path,
            100,
            200,
            "producer-a"
        ));
        assert!(!cache_matches(
            &cache,
            "main",
            "origin/main",
            "tip-a",
            "floor",
            path,
            101,
            201,
            "producer-a"
        ));
        assert!(!cache_matches(
            &cache,
            "main",
            "origin/main",
            "tip-a",
            "new-floor",
            path,
            100,
            200,
            "producer-a"
        ));
        assert!(!cache_matches(
            &cache,
            "main",
            "origin/main",
            "tip-a",
            "floor",
            path,
            100,
            200,
            "producer-b"
        ));
    }

    #[test]
    fn retained_cell_evidence_is_used_only_for_a_matching_ledger_row() {
        let original = gate(
            row("sha", "2026-08-03T01:00:00Z", "full", "full", "fail"),
            "outer",
            "fail",
        );
        let mut cached_gate = GateHistoryRow {
            name: "inner.cell".into(),
            result: Some("fail".into()),
            kind: None,
            exit_code: None,
            real_seconds: None,
            source_node: Some("test.inner".into()),
            failure_origin: None,
            failed_substeps: Vec::new(),
            extra: BTreeMap::new(),
        };
        cached_gate.extra.insert(
            "error_excerpt".into(),
            serde_json::json!(["panicked at source.rs:1"]),
        );
        let cache = CellEvidenceCache {
            schema_version: 2,
            records: vec![RetainedCellEvidence {
                commit: "sha".into(),
                finished_at: Some("2026-08-03T01:00:00Z".into()),
                gates: vec![cached_gate],
            }],
        };
        let mut rows = vec![original];
        merge_retained_cell_evidence(&mut rows, &cache);
        assert!(rows[0].gates.iter().any(|gate| gate.name == "inner.cell"));

        rows[0].finished_at = Some("different-run".into());
        rows[0].gates.retain(|gate| gate.name != "inner.cell");
        merge_retained_cell_evidence(&mut rows, &cache);
        assert!(!rows[0].gates.iter().any(|gate| gate.name == "inner.cell"));
    }
}
