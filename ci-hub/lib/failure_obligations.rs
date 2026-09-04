//! Durable per-cell requalification after a canonical divergence.
//!
//! Supported validation rows bind typed per-cell results to retained artifacts.
//! Those append-only rows are the obligation store: a canonical
//! divergence opens an obligation for that exact cell, and a later commit
//! closes it only after enough clean canonical legs have been retained to make
//! a repeat of the measured failure rate sufficiently unlikely.
//!
//! Cells with no recorded canonical divergence do not enter this path.  Their
//! ordinary full validation is neither repeated nor slowed down.

use crate::records::{
    CellIdentity, CellResult, CellResultsEvidence, CellVerdict, ComparisonObservationVerdictV10,
    ComparisonRelationV10, ComparisonStrictness, ComparisonTier, HistoryRow, RequiredNullable,
    ValidatePath, VerifiedValidationEvidenceV10,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

/// A clean candidate must make recurrence of the measured failure rate less
/// likely than 1 in 400 (99.75% confidence).  This is deliberately a policy
/// constant rather than a fixed repetition count: a PASS/FAIL/FAIL cell and a
/// 1-in-270 startup race require radically different budgets.
const REQUIRED_CONFIDENCE: f64 = 0.9975;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CellKey {
    lane: String,
    category: String,
    test: String,
    mode: String,
    backend: String,
    relation: ComparisonRelationV10,
}

impl From<&CellIdentity> for CellKey {
    fn from(cell: &CellIdentity) -> Self {
        Self {
            lane: cell.lane.clone(),
            category: cell.category.clone(),
            test: cell.test.clone(),
            mode: cell.mode.clone(),
            backend: cell.backend.clone(),
            relation: ComparisonRelationV10::Ordinary,
        }
    }
}

impl From<&CellResult> for CellKey {
    fn from(cell: &CellResult) -> Self {
        Self {
            lane: cell.lane.clone(),
            category: cell.category.clone(),
            test: cell.test.clone(),
            mode: cell.mode.clone(),
            backend: cell.backend.clone(),
            relation: ComparisonRelationV10::Ordinary,
        }
    }
}

impl CellKey {
    fn selected_candidate(&self) -> Self {
        match &self.relation {
            ComparisonRelationV10::Reference { candidate } => CellKey::from(candidate),
            _ => Self {
                relation: ComparisonRelationV10::Ordinary,
                ..self.clone()
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Observation {
    legs: u64,
    failures: u64,
}

#[derive(Clone, Debug)]
struct CommitObservation {
    first_seen: String,
    commit: String,
    tree: String,
    toolchain: String,
    observation: Observation,
    complete: bool,
    unavailable_comparisons: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RunFingerprint {
    commit: String,
    tree: String,
    toolchain: String,
    // `cell_results_evidence` normalizes schema 8 to the legacy evidence
    // fields, so retain its path separately as part of physical-run identity.
    validate_path: Option<ValidatePath>,
    evidence: RunEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RunEvidence {
    Legacy(CellResultsEvidence),
    Schema10(serde_json::Value),
}

fn ordinary_relation(relation: &ComparisonRelationV10) -> bool {
    matches!(relation, ComparisonRelationV10::Ordinary)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnresolvedFailureObligation {
    pub lane: String,
    pub category: String,
    pub test: String,
    pub mode: String,
    pub backend: String,
    #[serde(skip_serializing_if = "ordinary_relation")]
    pub relation: ComparisonRelationV10,
    pub failing_commit: String,
    pub baseline_failures: u64,
    pub baseline_legs: u64,
    pub required_clean_legs: u64,
    pub observed_clean_legs_at_target: u64,
    pub target_failures: u64,
    pub target_comparisons_complete: bool,
    pub target_unavailable_comparisons: u64,
}

impl UnresolvedFailureObligation {
    pub fn target_comparison_status(&self) -> String {
        if self.target_comparisons_complete {
            "comparisons complete".into()
        } else if self.target_unavailable_comparisons > 0 {
            format!(
                "comparisons incomplete: {} unavailable comparison(s)",
                self.target_unavailable_comparisons
            )
        } else {
            "no complete comparison evidence at target".into()
        }
    }

    pub fn comparison_description(&self) -> Option<String> {
        match &self.relation {
            ComparisonRelationV10::Ordinary => None,
            ComparisonRelationV10::Reference { candidate } => Some(format!(
                "reference comparison for {} {}/{}",
                candidate.backend, candidate.test, candidate.mode,
            )),
            ComparisonRelationV10::BackendParity {
                reference_backend,
                record_envelope,
            } => Some(format!(
                "parity comparison with {reference_backend}, {record_envelope:?}"
            )),
        }
    }
}

/// Return the number of consecutive clean legs needed to observe no recurrence
/// with [`REQUIRED_CONFIDENCE`] at the measured failure probability.
///
/// The result is fail-closed for malformed or certain-failure observations.
fn required_clean_legs(failures: u64, legs: u64) -> Option<u64> {
    if failures == 0 || legs == 0 || failures > legs {
        return None;
    }
    if failures == legs {
        return Some(u64::MAX);
    }
    let failure_probability = failures as f64 / legs as f64;
    let miss_probability = 1.0 - REQUIRED_CONFIDENCE;
    let required = (miss_probability.ln() / (1.0 - failure_probability).ln()).ceil();
    if !required.is_finite() || required <= 0.0 || required > u64::MAX as f64 {
        None
    } else {
        // Verification produces pairs of legs, and the measured startup-race
        // budget was operated in ten-leg batches. Round up rather than letting
        // a floating-point boundary admit a smaller, unmeasured budget.
        let required = required as u64;
        required.checked_add(9).map(|value| value / 10 * 10)
    }
}

/// Validate the common canonical, non-stripped, nonempty INFO shape against an
/// exact expected set of the four fields added after the first schema-6 rows.
fn observation_with_comparison_fields(
    cell: &CellResult,
    display_name: Option<&str>,
    compare_io_buffers: Option<bool>,
    record_envelope: Option<&str>,
    virtualize_time: Option<bool>,
) -> Option<Observation> {
    let (matched, tier, comparison, bitwise_parity, counts) = match &cell.cell_verdict {
        CellVerdict::ComparedAndMatched {
            comparison_tier,
            comparison,
            bitwise_parity,
            compared_log_messages,
        } => (
            true,
            comparison_tier,
            comparison,
            *bitwise_parity,
            compared_log_messages,
        ),
        CellVerdict::ComparedAndDiverged {
            comparison_tier,
            comparison,
            bitwise_parity,
            compared_log_messages,
        } => (
            false,
            comparison_tier,
            comparison,
            *bitwise_parity,
            compared_log_messages,
        ),
        CellVerdict::PerformsNoComparisonByDesign { .. }
        | CellVerdict::UnavailableWithReason { .. } => return None,
    };
    let RequiredNullable::Value(counts) = counts else {
        return None;
    };
    if *tier != ComparisonTier::CanonicalBitwise
        || comparison.strictness != ComparisonStrictness::Canonical
        || comparison.display_name.as_deref() != display_name
        || !comparison.compare_logs
        || comparison.compare_io_buffers != compare_io_buffers
        || comparison.record_envelope.as_deref() != record_envelope
        || comparison.virtualize_time != virtualize_time
        || comparison.log_scope != crate::records::ComparedLogScope::Info
        || comparison.strip_lines
        || !comparison.canonicalize_addresses
        || !comparison.full_trace
        || !comparison.exact_remainder
        || comparison.stripped_prefixes != ["real-wall-clock-prefix/v1"]
        || comparison.canonicalizations != ["host-address-to-first-appearance-ordinal/v1"]
        || comparison.ignore_lines
        || comparison.skip_commit
        || comparison.skip_detlog
        || counts.left == 0
        || counts.right == 0
        || bitwise_parity != matched
    {
        return None;
    }
    Some(Observation {
        legs: 2,
        failures: u64::from(!matched),
    })
}

/// A comparison contributes to requalification only when it carries the
/// complete current evidence. Output-only matches, stripped comparisons, 0/0
/// reports and unavailable cells never become clean legs.
fn canonical_observation(cell: &CellResult) -> Option<Observation> {
    let virtualize_time = match cell.mode.as_str() {
        "replay" => Some(false),
        "verify" | "chaos" => Some(true),
        _ => return None,
    };
    observation_with_comparison_fields(
        cell,
        Some("BitwiseInfoV1"),
        Some(true),
        Some("all_records_v1"),
        virtualize_time,
    )
}

fn known_cell_mode(mode: &str) -> bool {
    matches!(mode, "verify" | "replay" | "chaos" | "naked" | "custom")
}

/// The first schema-6 rows predate the four comparison fields that now make a
/// canonical observation complete.  Their exact older shape is readable, but
/// it contributes neither a clean leg nor a failure: the absent fields carry
/// no claim.  A partial current shape or any other weakened comparison remains
/// invalid rather than being mistaken for this history.
fn comparison_without_later_fields_is_well_formed(cell: &CellResult) -> bool {
    observation_with_comparison_fields(cell, None, None, None, None).is_some()
}

fn nonblank(value: &str) -> bool {
    !value.trim().is_empty()
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn population_sha256(selected: &[CellIdentity]) -> String {
    let mut selected = selected.to_vec();
    selected.sort_by(|left, right| CellKey::from(left).cmp(&CellKey::from(right)));
    // Match qualifying_receipt.py exactly: sorted identities, sorted object
    // keys, compact JSON. Serializing the Rust struct directly would preserve
    // declaration order and disagree with the canonical cross-language digest.
    let canonical = selected
        .iter()
        .map(|cell| {
            serde_json::json!({
                "backend": cell.backend,
                "category": cell.category,
                "lane": cell.lane,
                "mode": cell.mode,
                "test": cell.test,
            })
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&canonical).expect("cell identities serialize");
    format!("{:x}", Sha256::digest(bytes))
}

fn retained_artifact_path(value: &str) -> bool {
    let path = Path::new(value);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && path.starts_with("ignored/validate/artifacts")
}

fn cell_key_nonblank(cell: &CellKey) -> bool {
    nonblank(&cell.lane)
        && nonblank(&cell.category)
        && nonblank(&cell.test)
        && nonblank(&cell.mode)
        && nonblank(&cell.backend)
}

/// Validate a supported typed cell-results summary before using it as
/// repetition evidence.
/// The canonical ledger adapter additionally binds the retained artifact bytes;
/// this check prevents any direct caller from bypassing the summary contract.
fn validated_evidence<'a>(
    row: &'a HistoryRow,
    schema_min: u32,
    schema_max: u32,
) -> Result<Option<Cow<'a, CellResultsEvidence>>, String> {
    let schema = row.schema_version.unwrap_or(0);
    let evidence = row.cell_results.as_ref();
    if schema < schema_min {
        return if evidence.is_none() {
            Ok(None)
        } else {
            Err("legacy row illegally claims cell_results".into())
        };
    }
    if schema > schema_max || !matches!(schema, 6 | 7 | 8) {
        return Ok(None);
    }
    evidence.ok_or_else(|| format!("schema-{schema} row omits cell_results"))?;
    let evidence = row
        .cell_results_evidence()
        .ok_or_else(|| format!("schema-{schema} row has malformed cell_results"))?;
    let run_id = row
        .run_id
        .as_deref()
        .filter(|value| nonblank(value))
        .ok_or_else(|| format!("schema-{schema} row omits stable run_id"))?;
    if evidence.run_id != run_id {
        return Err("cell_results run_id differs from row run_id".into());
    }
    if row.commit.as_deref() != Some(evidence.hermit_sha.as_str()) {
        return Err("cell_results hermit_sha differs from row commit".into());
    }
    if evidence.source_tree_dirty {
        return Err("cell_results came from a dirty source tree".into());
    }
    let selected = &evidence.selected;
    let cells = &evidence.cells;
    let artifact = &evidence.artifact;
    let selected_count = evidence.selected_count;
    let recorded_count = evidence.recorded_count;
    if selected_count != selected.len() as u64
        || recorded_count != cells.len() as u64
        || artifact.row_count != recorded_count
        || !retained_artifact_path(&artifact.path)
        || !sha256(&artifact.sha256)
        || !sha256(&evidence.population_sha256)
    {
        return Err("cell_results counts or artifact identity are malformed".into());
    }
    if schema < 8 && (selected_count == 0 || recorded_count == 0) {
        return Err("cell_results counts or artifact identity are malformed".into());
    }
    if schema == 8
        && (row.cell_results_validate_path().map(|path| path.as_str()) != row.profile.as_deref()
            || row.selection_mode.as_deref() != Some("full"))
    {
        return Err("cell_results path differs from enclosing validation row".into());
    }
    let selected_keys: Vec<CellKey> = selected.iter().map(CellKey::from).collect();
    let cell_keys: Vec<CellKey> = cells.iter().map(CellKey::from).collect();
    if selected_keys.iter().any(|cell| !cell_key_nonblank(cell))
        || cell_keys.iter().any(|cell| !cell_key_nonblank(cell))
        || selected_keys.iter().collect::<BTreeSet<_>>().len() != selected_keys.len()
        || cell_keys.iter().collect::<BTreeSet<_>>().len() != cell_keys.len()
    {
        return Err("cell_results contain blank or duplicate identities".into());
    }
    let selected_set = selected_keys.iter().collect::<BTreeSet<_>>();
    let cell_set = cell_keys.iter().collect::<BTreeSet<_>>();
    if (schema < 8 && selected_set != cell_set)
        || (schema == 8 && !cell_set.is_subset(&selected_set))
    {
        return Err("cell_results selected population differs from recorded cells".into());
    }
    if population_sha256(selected) != evidence.population_sha256 {
        return Err("cell_results population_sha256 does not bind selected identities".into());
    }
    if cells.iter().any(|cell| {
        !known_cell_mode(&cell.mode)
            || match &cell.cell_verdict {
                CellVerdict::ComparedAndMatched { .. }
                | CellVerdict::ComparedAndDiverged { .. } => {
                    canonical_observation(cell).is_none()
                        && !(schema == schema_min
                            && comparison_without_later_fields_is_well_formed(cell))
                        && matches!(
                            &cell.cell_verdict,
                            CellVerdict::ComparedAndMatched {
                                comparison_tier: ComparisonTier::CanonicalBitwise,
                                ..
                            } | CellVerdict::ComparedAndDiverged {
                                comparison_tier: ComparisonTier::CanonicalBitwise,
                                ..
                            }
                        )
                }
                CellVerdict::PerformsNoComparisonByDesign { reason, .. }
                | CellVerdict::UnavailableWithReason { reason, .. } => !nonblank(reason),
            }
    }) {
        return Err("cell_results contain an invalid typed verdict".into());
    }
    Ok(Some(evidence))
}

enum ValidatedEvidence<'a> {
    Legacy(Cow<'a, CellResultsEvidence>),
    Schema10(Box<VerifiedValidationEvidenceV10>),
}

impl ValidatedEvidence<'_> {
    fn selected(&self) -> &[CellIdentity] {
        match self {
            Self::Legacy(evidence) => &evidence.selected,
            Self::Schema10(evidence) => &evidence.cell_results.selected,
        }
    }

    fn fingerprint(&self, row: &HistoryRow) -> RunEvidence {
        match self {
            Self::Legacy(evidence) => RunEvidence::Legacy(evidence.as_ref().clone()),
            Self::Schema10(evidence) => RunEvidence::Schema10(serde_json::json!({
                "cell_results": evidence.cell_results,
                "test_results": row.test_results,
                "constructed_plan": row.extra.get("constructed_plan"),
            })),
        }
    }

    fn validate_path(&self, row: &HistoryRow) -> Option<ValidatePath> {
        match self {
            Self::Legacy(_) => row.cell_results_validate_path(),
            Self::Schema10(evidence) => Some(evidence.cell_results.path),
        }
    }

    /// None is an observed unavailable attempt, not a clean comparison. Old
    /// summaries retain their established canonical-observation rules.
    fn observations(&self) -> Vec<(CellKey, Option<Observation>)> {
        match self {
            Self::Legacy(evidence) => evidence
                .cells
                .iter()
                .filter_map(|cell| {
                    canonical_observation(cell)
                        .map(|observation| (CellKey::from(cell), Some(observation)))
                })
                .collect(),
            Self::Schema10(evidence) => evidence
                .observations
                .iter()
                .map(|observation| {
                    let mut key = CellKey::from(&observation.identity);
                    key.relation = observation.relation.clone();
                    let value = match observation.verdict {
                        ComparisonObservationVerdictV10::Matched => Some(Observation {
                            legs: 2,
                            failures: 0,
                        }),
                        ComparisonObservationVerdictV10::Diverged => Some(Observation {
                            legs: 2,
                            failures: 1,
                        }),
                        ComparisonObservationVerdictV10::UnavailableWithReason { .. } => None,
                    };
                    (key, value)
                })
                .collect(),
        }
    }
}

fn validated_observation_evidence<'a>(
    row: &'a HistoryRow,
    schema_min: u32,
    schema_max: u32,
    artifact_root: Option<&Path>,
) -> Result<Option<ValidatedEvidence<'a>>, String> {
    if row.schema_version != Some(10) {
        return validated_evidence(row, schema_min, schema_max)
            .map(|value| value.map(ValidatedEvidence::Legacy));
    }
    if 10 < schema_min || 10 > schema_max {
        return Ok(None);
    }
    let outcome = match artifact_root {
        Some(root) => crate::validation_evidence::verify(row, root),
        None => crate::validation_evidence::verify_current_store(row),
    };
    match outcome {
        crate::validation_evidence::EvidenceOutcome::Verified { evidence, .. } => {
            Ok(Some(ValidatedEvidence::Schema10(evidence)))
        }
        crate::validation_evidence::EvidenceOutcome::Refused { reason } => Err(reason),
        crate::validation_evidence::EvidenceOutcome::ReaderFault { reason } => {
            Err(format!("validation evidence reader fault: {reason}"))
        }
    }
}

/// The website's permitted view of one row's typed per-cell evidence.
///
/// The exact first schema-6 shape remains readable but unavailable for public
/// cell-result claims: the four later comparison fields were not recorded, so
/// absence must not be filled in or treated as a canonical observation.
pub(crate) enum CellResultsForWebsite<'a> {
    Absent,
    Complete(Cow<'a, CellResultsEvidence>),
    Schema6WithoutLaterComparisonFields,
    UnsupportedSchema(u32),
}

pub(crate) fn cell_results_for_website(
    row: &HistoryRow,
) -> Result<CellResultsForWebsite<'_>, String> {
    const SCHEMA_MIN: u32 = 6;
    const SCHEMA_MAX: u32 = 8;
    let schema_min = SCHEMA_MIN;
    let schema_max = SCHEMA_MAX;
    let schema = row.schema_version.unwrap_or(0);
    if schema > schema_max && row.cell_results.is_some() {
        return Ok(CellResultsForWebsite::UnsupportedSchema(schema));
    }
    let Some(evidence) = validated_evidence(row, schema_min, schema_max)? else {
        return Ok(CellResultsForWebsite::Absent);
    };
    let lacks_later_comparison_fields = row.schema_version == Some(schema_min)
        && evidence.cells.iter().any(|cell| {
            canonical_observation(cell).is_none()
                && comparison_without_later_fields_is_well_formed(cell)
        });
    if lacks_later_comparison_fields {
        Ok(CellResultsForWebsite::Schema6WithoutLaterComparisonFields)
    } else {
        Ok(CellResultsForWebsite::Complete(evidence))
    }
}

/// Reviewed calibration policy for cells whose observed failure is rarer than
/// one ordinary verify pair can measure. Ledger rows cannot self-declare this:
/// changing the budget is a code-reviewed policy change bound to an exact cell.
fn trusted_calibration(cell: &CellKey) -> Option<Observation> {
    (ordinary_relation(&cell.relation)
        && cell.lane == "portable"
        && cell.category == "c-programs"
        && cell.test == "c-programs/perf-event-open-enosys"
        && cell.mode == "verify"
        && cell.backend == "ptrace")
        .then_some(Observation {
            failures: 1,
            legs: 270,
        })
}

fn target_cutoff(rows: &[HistoryRow], target_sha: &str) -> Option<String> {
    rows.iter()
        .filter(|row| row.commit.as_deref() == Some(target_sha))
        .filter_map(|row| row.finished_at.as_deref())
        .max()
        .map(str::to_owned)
}

/// Evaluate every per-cell failure obligation visible no later than the target
/// run.  A divergence on the target commit is never cleared by a lucky sibling
/// pass on that same commit.  An older obligation is cleared only by a later
/// commit carrying the calibrated number of clean canonical legs.
pub fn unresolved<F>(
    rows: &[HistoryRow],
    target_sha: &str,
    qualifying_target_rows: &[HistoryRow],
    cell_results_schema_min: u32,
    cell_results_schema_max: u32,
    is_ancestor: F,
) -> Result<Vec<UnresolvedFailureObligation>, String>
where
    F: FnMut(&str, &str) -> Result<bool, String>,
{
    unresolved_in_store(
        rows,
        target_sha,
        qualifying_target_rows,
        cell_results_schema_min,
        cell_results_schema_max,
        None,
        is_ancestor,
    )
}

fn unresolved_in_store<F>(
    rows: &[HistoryRow],
    target_sha: &str,
    qualifying_target_rows: &[HistoryRow],
    cell_results_schema_min: u32,
    cell_results_schema_max: u32,
    artifact_root: Option<&Path>,
    mut is_ancestor: F,
) -> Result<Vec<UnresolvedFailureObligation>, String>
where
    F: FnMut(&str, &str) -> Result<bool, String>,
{
    let Some(cutoff) = target_cutoff(rows, target_sha) else {
        return Ok(Vec::new());
    };
    // Only obligations for cells selected by this candidate can block it. A
    // disabled/red cell remains visible in the scorecard but must not multiply
    // unrelated validation work. `None` means no supported evidence was
    // available and therefore cannot narrow the obligations; `Some(empty)` is
    // schema-8's explicit empty selection and must remain empty.
    let mut target_selected: Option<BTreeSet<CellKey>> = None;
    for row in qualifying_target_rows
        .iter()
        .filter(|row| row.commit.as_deref() == Some(target_sha))
    {
        if let Some(evidence) = validated_observation_evidence(
            row,
            cell_results_schema_min,
            cell_results_schema_max,
            artifact_root,
        )? {
            target_selected
                .get_or_insert_default()
                .extend(evidence.selected().iter().map(CellKey::from));
        }
    }
    let mut by_cell_commit: BTreeMap<(CellKey, String, String, String), CommitObservation> =
        BTreeMap::new();
    let mut runs: BTreeMap<String, RunFingerprint> = BTreeMap::new();

    for row in rows {
        let Some(at) = row.finished_at.as_deref() else {
            continue;
        };
        if at > cutoff.as_str() {
            continue;
        }
        let Some(commit) = row.commit.as_deref() else {
            continue;
        };
        let Some(evidence) = validated_observation_evidence(
            row,
            cell_results_schema_min,
            cell_results_schema_max,
            artifact_root,
        )?
        else {
            continue;
        };
        let Some(tree) = row.tree().map_err(str::to_owned)? else {
            continue;
        };
        let Some(toolchain) = row.extra.get("toolchain").and_then(|value| value.as_str()) else {
            continue;
        };
        let run_id = row
            .run_id
            .as_deref()
            .expect("validated cell-results run_id");
        let fingerprint = RunFingerprint {
            commit: commit.to_owned(),
            tree: tree.to_owned(),
            toolchain: toolchain.to_owned(),
            validate_path: evidence.validate_path(row),
            evidence: evidence.fingerprint(row),
        };
        if let Some(existing) = runs.get(run_id) {
            if existing != &fingerprint {
                return Err(format!("conflicting correction rows for run_id {run_id}"));
            }
            continue;
        }
        runs.insert(run_id.to_owned(), fingerprint);
        for (cell, observation) in evidence.observations() {
            let key = (
                cell,
                commit.to_owned(),
                tree.to_owned(),
                toolchain.to_owned(),
            );
            let aggregate = by_cell_commit
                .entry(key)
                .or_insert_with(|| CommitObservation {
                    first_seen: at.to_owned(),
                    commit: commit.to_owned(),
                    tree: tree.to_owned(),
                    toolchain: toolchain.to_owned(),
                    observation: Observation::default(),
                    complete: true,
                    unavailable_comparisons: 0,
                });
            if at < aggregate.first_seen.as_str() {
                aggregate.first_seen = at.to_owned();
            }
            match observation {
                Some(observation) => {
                    aggregate.observation.legs += observation.legs;
                    aggregate.observation.failures += observation.failures;
                }
                None => {
                    aggregate.complete = false;
                    aggregate.unavailable_comparisons += 1;
                }
            }
        }
    }

    let mut by_cell: BTreeMap<CellKey, Vec<CommitObservation>> = BTreeMap::new();
    for ((cell, _, _, _), observation) in by_cell_commit {
        by_cell.entry(cell).or_default().push(observation);
    }

    let mut unresolved = Vec::new();
    for (cell, observations) in &mut by_cell {
        // With a supported selected set, only selected cells carry forward. If
        // the candidate has no typed selected set at all, fail closed: absence
        // cannot prove that a previously failing cell is no longer selected.
        if let Some(selected) = &target_selected {
            if !selected.contains(&cell.selected_candidate()) {
                continue;
            }
        }
        observations.sort_by(|left, right| {
            left.first_seen
                .cmp(&right.first_seen)
                .then_with(|| left.commit.cmp(&right.commit))
                .then_with(|| left.tree.cmp(&right.tree))
                .then_with(|| left.toolchain.cmp(&right.toolchain))
        });
        let mut failing = None;
        for observation in observations.iter().rev() {
            if observation.observation.failures > 0 && is_ancestor(&observation.commit, target_sha)?
            {
                failing = Some(observation);
                break;
            }
        }
        let Some(failing) = failing else {
            continue;
        };
        let rate = trusted_calibration(cell).unwrap_or(failing.observation);
        let required = required_clean_legs(rate.failures, rate.legs).unwrap_or(u64::MAX);
        let mut resolved = false;
        for observation in observations.iter() {
            if observation.first_seen >= failing.first_seen
                && observation.commit != failing.commit
                && observation.observation.failures == 0
                && observation.complete
                && observation.observation.legs >= required
                && is_ancestor(&failing.commit, &observation.commit)?
                && is_ancestor(&observation.commit, target_sha)?
            {
                resolved = true;
                break;
            }
        }
        if resolved {
            continue;
        }
        let target = observations
            .iter()
            .filter(|observation| observation.commit == target_sha)
            .max_by_key(|observation| observation.observation.legs);
        unresolved.push(UnresolvedFailureObligation {
            lane: cell.lane.clone(),
            category: cell.category.clone(),
            test: cell.test.clone(),
            mode: cell.mode.clone(),
            backend: cell.backend.clone(),
            relation: cell.relation.clone(),
            failing_commit: failing.commit.clone(),
            baseline_failures: rate.failures,
            baseline_legs: rate.legs,
            required_clean_legs: required,
            observed_clean_legs_at_target: target.map_or(0, |value| {
                value
                    .observation
                    .legs
                    .saturating_sub(value.observation.failures)
            }),
            target_failures: target.map_or(0, |value| value.observation.failures),
            target_comparisons_complete: target.is_some_and(|value| value.complete),
            target_unavailable_comparisons: target.map_or(0, |value| value.unavailable_comparisons),
        });
    }
    Ok(unresolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::{
        CellResultsArtifact, CellResultsEvidence, CellResultsValue, ComparedLogCounts,
        ComparedLogScope, ComparisonSpec,
    };

    const BAD: &str = "1111111111111111111111111111111111111111";
    const FIX: &str = "2222222222222222222222222222222222222222";
    const OTHER: &str = "3333333333333333333333333333333333333333";
    const OLD: &str = "0000000000000000000000000000000000000000";
    const CELL_RESULTS_SCHEMA_MIN: u32 = 6;
    const CELL_RESULTS_SCHEMA_MAX: u32 = 8;

    fn comparison() -> ComparisonSpec {
        ComparisonSpec {
            strictness: ComparisonStrictness::Canonical,
            display_name: Some("BitwiseInfoV1".into()),
            compare_logs: true,
            compare_io_buffers: Some(true),
            record_envelope: Some("all_records_v1".into()),
            virtualize_time: Some(true),
            log_scope: ComparedLogScope::Info,
            strip_lines: false,
            canonicalize_addresses: true,
            full_trace: true,
            exact_remainder: true,
            stripped_prefixes: vec!["real-wall-clock-prefix/v1".into()],
            canonicalizations: vec!["host-address-to-first-appearance-ordinal/v1".into()],
            ignore_lines: false,
            skip_commit: false,
            skip_detlog: false,
        }
    }

    fn row(commit: &str, ordinal: u64, matched: bool) -> HistoryRow {
        let verdict = if matched {
            CellVerdict::ComparedAndMatched {
                comparison_tier: ComparisonTier::CanonicalBitwise,
                comparison: comparison(),
                bitwise_parity: true,
                compared_log_messages: RequiredNullable::Value(ComparedLogCounts {
                    left: 120,
                    right: 120,
                }),
            }
        } else {
            CellVerdict::ComparedAndDiverged {
                comparison_tier: ComparisonTier::CanonicalBitwise,
                comparison: comparison(),
                bitwise_parity: false,
                compared_log_messages: RequiredNullable::Value(ComparedLogCounts {
                    left: 120,
                    right: 121,
                }),
            }
        };
        let identity = CellIdentity {
            lane: "portable".into(),
            category: "c-programs".into(),
            test: "c-programs/perf-event-open-enosys".into(),
            mode: "verify".into(),
            backend: "ptrace".into(),
        };
        let population_sha256 = population_sha256(std::slice::from_ref(&identity));
        serde_json::from_value(serde_json::json!({
            "schema_version": 6,
            "run_id": format!("run-{commit}-{ordinal}"),
            "finished_at": format!("2026-08-17T{:02}:{:02}:00Z", ordinal / 60, ordinal % 60),
            "commit": commit,
            "tree": commit,
            "toolchain": "rustc-test",
            "cell_results": CellResultsEvidence {
                run_id: format!("run-{commit}-{ordinal}"),
                hermit_sha: commit.into(),
                source_tree_dirty: false,
                selected_count: 1,
                recorded_count: 1,
                population_sha256,
                artifact: CellResultsArtifact {
                    path: "ignored/validate/artifacts/test/results.jsonl".into(),
                    sha256: "b".repeat(64),
                    row_count: 1,
                },
                selected: vec![identity.clone()],
                cells: vec![CellResult {
                    lane: identity.lane,
                    category: identity.category,
                    test: identity.test,
                    mode: identity.mode,
                    backend: identity.backend,
                    cell_verdict: verdict,
                }],
            }
        }))
        .unwrap()
    }

    fn row_comparison(row: &mut HistoryRow) -> &mut ComparisonSpec {
        let evidence = row
            .cell_results
            .as_mut()
            .expect("test row has cell evidence")
            .typed_mut()
            .expect("test row has typed cell evidence");
        match &mut evidence.cells[0].cell_verdict {
            CellVerdict::ComparedAndMatched { comparison, .. }
            | CellVerdict::ComparedAndDiverged { comparison, .. } => comparison,
            CellVerdict::PerformsNoComparisonByDesign { .. }
            | CellVerdict::UnavailableWithReason { .. } => {
                panic!("test row has a compared verdict")
            }
        }
    }

    fn schema8_row(commit: &str, ordinal: u64, matched: bool, path: &str) -> HistoryRow {
        let mut value = serde_json::to_value(row(commit, ordinal, matched)).unwrap();
        value["schema_version"] = serde_json::json!(8);
        value["profile"] = serde_json::json!(path);
        value["selection_mode"] = serde_json::json!("full");
        value["cell_results"]["path"] = serde_json::json!(path);
        serde_json::from_value(value).unwrap()
    }

    fn target_rows(rows: &[HistoryRow], target: &str) -> Vec<HistoryRow> {
        rows.iter()
            .filter(|row| row.commit.as_deref() == Some(target))
            .cloned()
            .collect()
    }

    fn uncalibrated_row(commit: &str, ordinal: u64, matched: bool) -> HistoryRow {
        let mut value = row(commit, ordinal, matched);
        let evidence = value.cell_results.as_mut().unwrap().typed_mut().unwrap();
        evidence.selected[0].test = "c-programs/ordinary-cell".into();
        evidence.cells[0].test = "c-programs/ordinary-cell".into();
        evidence.population_sha256 = population_sha256(&evidence.selected);
        value
    }

    fn targeted_producer_row(commit: &str, ordinal: u64, matched: bool) -> HistoryRow {
        let mut value = row(commit, ordinal, matched);
        let run_id = format!(
            "validate-cell-requalification-{}-20260817T{:06}Z-pid{}--portable-c-programs-c-programs-perf-event-open-enosys-verify-ptrace-repetition-0001",
            &commit[..commit.len().min(12)],
            ordinal,
            10_000 + ordinal,
        );
        value.run_id = Some(run_id.clone());
        value
            .extra
            .insert("selection_mode".into(), serde_json::json!("targeted"));
        let evidence = value.cell_results.as_mut().unwrap().typed_mut().unwrap();
        evidence.run_id = run_id.clone();
        evidence.artifact.path = format!("ignored/validate/artifacts/{run_id}/cell-results.jsonl");
        value
    }

    fn evaluate(
        rows: &[HistoryRow],
        target: &str,
    ) -> Result<Vec<UnresolvedFailureObligation>, String> {
        let target_receipts = target_rows(rows, target);
        unresolved(
            rows,
            target,
            &target_receipts,
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
            |ancestor, descendant| {
                Ok(ancestor == descendant || (ancestor == BAD && descendant == FIX))
            },
        )
    }

    #[test]
    fn unaffected_cell_needs_no_extra_repetitions() {
        let rows = vec![row(FIX, 1, true)];
        assert!(evaluate(&rows, FIX).unwrap().is_empty());
    }

    #[test]
    fn canonical_observation_requires_the_current_verifier_evidence() {
        let current = row(FIX, 1, true);
        let cell = &current
            .cell_results
            .as_ref()
            .unwrap()
            .typed()
            .unwrap()
            .cells[0];
        assert_eq!(
            canonical_observation(cell),
            Some(Observation {
                legs: 2,
                failures: 0
            })
        );

        let mut missing_display_name = current.clone();
        row_comparison(&mut missing_display_name).display_name = None;
        let cell = &missing_display_name
            .cell_results
            .as_ref()
            .unwrap()
            .typed()
            .unwrap()
            .cells[0];
        assert_eq!(canonical_observation(cell), None);

        let mut missing_io_buffers = current.clone();
        row_comparison(&mut missing_io_buffers).compare_io_buffers = None;
        let cell = &missing_io_buffers
            .cell_results
            .as_ref()
            .unwrap()
            .typed()
            .unwrap()
            .cells[0];
        assert_eq!(canonical_observation(cell), None);

        let mut caller_defined = current.clone();
        row_comparison(&mut caller_defined).record_envelope = Some("caller_defined".into());
        let cell = &caller_defined
            .cell_results
            .as_ref()
            .unwrap()
            .typed()
            .unwrap()
            .cells[0];
        assert_eq!(canonical_observation(cell), None);

        let mut host_time = current.clone();
        row_comparison(&mut host_time).virtualize_time = Some(false);
        let cell = &host_time
            .cell_results
            .as_ref()
            .unwrap()
            .typed()
            .unwrap()
            .cells[0];
        assert_eq!(canonical_observation(cell), None);

        let mut replay = current.clone();
        let evidence = replay.cell_results.as_mut().unwrap().typed_mut().unwrap();
        evidence.selected[0].mode = "replay".into();
        evidence.cells[0].mode = "replay".into();
        row_comparison(&mut replay).virtualize_time = Some(false);
        let cell = &replay.cell_results.as_ref().unwrap().typed().unwrap().cells[0];
        assert_eq!(
            canonical_observation(cell),
            Some(Observation {
                legs: 2,
                failures: 0
            })
        );

        let mut chaos_without_virtual_time = current.clone();
        let evidence = chaos_without_virtual_time
            .cell_results
            .as_mut()
            .unwrap()
            .typed_mut()
            .unwrap();
        evidence.selected[0].mode = "chaos".into();
        evidence.cells[0].mode = "chaos".into();
        row_comparison(&mut chaos_without_virtual_time).virtualize_time = Some(false);
        let cell = &chaos_without_virtual_time
            .cell_results
            .as_ref()
            .unwrap()
            .typed()
            .unwrap()
            .cells[0];
        assert_eq!(canonical_observation(cell), None);

        let mut unknown_mode = current.clone();
        let evidence = unknown_mode
            .cell_results
            .as_mut()
            .unwrap()
            .typed_mut()
            .unwrap();
        evidence.selected[0].mode = "unknown".into();
        evidence.cells[0].mode = "unknown".into();
        let cell = &unknown_mode
            .cell_results
            .as_ref()
            .unwrap()
            .typed()
            .unwrap()
            .cells[0];
        assert_eq!(canonical_observation(cell), None);

        let mut unknown_reasoned_mode = current.clone();
        let evidence = unknown_reasoned_mode
            .cell_results
            .as_mut()
            .unwrap()
            .typed_mut()
            .unwrap();
        evidence.selected[0].mode = "unknown".into();
        evidence.cells[0].mode = "unknown".into();
        evidence.cells[0].cell_verdict = CellVerdict::UnavailableWithReason {
            comparison_tier: ComparisonTier::DeclaredButUnverifiable,
            reason: "fixture unavailable".into(),
        };
        assert!(validated_evidence(
            &unknown_reasoned_mode,
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
        )
        .is_err());

        let mut extra_stripping = current.clone();
        row_comparison(&mut extra_stripping)
            .stripped_prefixes
            .push("unreviewed-prefix".into());
        let cell = &extra_stripping
            .cell_results
            .as_ref()
            .unwrap()
            .typed()
            .unwrap()
            .cells[0];
        assert_eq!(canonical_observation(cell), None);
    }

    #[test]
    fn comparison_without_later_fields_is_readable_but_adds_no_observation() {
        let mut before_fields = row(BAD, 1, false);
        let comparison = row_comparison(&mut before_fields);
        comparison.display_name = None;
        comparison.compare_io_buffers = None;
        comparison.record_envelope = None;
        comparison.virtualize_time = None;

        let cell = &before_fields
            .cell_results
            .as_ref()
            .unwrap()
            .typed()
            .unwrap()
            .cells[0];
        assert!(comparison_without_later_fields_is_well_formed(cell));
        assert_eq!(canonical_observation(cell), None);

        let target = row(FIX, 2, true);
        assert!(evaluate(&[before_fields, target], FIX).unwrap().is_empty());
    }

    #[test]
    fn partially_missing_later_fields_are_still_refused() {
        let mut malformed = row(BAD, 1, false);
        row_comparison(&mut malformed).display_name = None;
        let target = row(FIX, 2, true);
        let error = evaluate(&[malformed, target], FIX).unwrap_err();
        assert_eq!(error, "cell_results contain an invalid typed verdict");
    }

    #[test]
    fn schema7_requires_the_current_exact_shape() {
        let mut current = row(FIX, 1, true);
        current.schema_version = Some(7);
        assert!(
            validated_evidence(&current, CELL_RESULTS_SCHEMA_MIN, CELL_RESULTS_SCHEMA_MAX,)
                .unwrap()
                .is_some()
        );

        let mut missing_current_field = current;
        row_comparison(&mut missing_current_field).display_name = None;
        assert!(validated_evidence(
            &missing_current_field,
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
        )
        .is_err());
    }

    #[test]
    fn schema8_keeps_selected_population_distinct_from_recorded_cells() {
        let mut current = schema8_row(FIX, 1, true, "full");
        let mut value = serde_json::to_value(&current).unwrap();
        let mut second = value["cell_results"]["selected"][0].clone();
        second["test"] = serde_json::json!("second-cell");
        value["cell_results"]["selected"]
            .as_array_mut()
            .unwrap()
            .push(second);
        value["cell_results"]["selected_count"] = serde_json::json!(2);
        let selected: Vec<CellIdentity> =
            serde_json::from_value(value["cell_results"]["selected"].clone()).unwrap();
        value["cell_results"]["population_sha256"] =
            serde_json::json!(population_sha256(&selected));
        current = serde_json::from_value(value).unwrap();

        let evidence =
            validated_evidence(&current, CELL_RESULTS_SCHEMA_MIN, CELL_RESULTS_SCHEMA_MAX)
                .unwrap()
                .unwrap();
        assert_eq!(evidence.selected_count, 2);
        assert_eq!(evidence.recorded_count, 1);

        let mut outside = serde_json::to_value(&current).unwrap();
        outside["cell_results"]["cells"][0]["test"] = serde_json::json!("outside");
        let outside: HistoryRow = serde_json::from_value(outside).unwrap();
        assert_eq!(
            validated_evidence(&outside, CELL_RESULTS_SCHEMA_MIN, CELL_RESULTS_SCHEMA_MAX,)
                .unwrap_err(),
            "cell_results selected population differs from recorded cells"
        );
    }

    #[test]
    fn schema8_requires_the_recorded_path_to_match_the_row() {
        for path in ["quick", "full", "super"] {
            let current = schema8_row(FIX, 1, true, path);
            assert!(
                validated_evidence(&current, CELL_RESULTS_SCHEMA_MIN, CELL_RESULTS_SCHEMA_MAX,)
                    .unwrap()
                    .is_some()
            );
        }

        let mut substituted = schema8_row(FIX, 1, true, "quick");
        substituted.profile = Some("super".into());
        assert_eq!(
            validated_evidence(
                &substituted,
                CELL_RESULTS_SCHEMA_MIN,
                CELL_RESULTS_SCHEMA_MAX,
            )
            .unwrap_err(),
            "cell_results path differs from enclosing validation row"
        );
    }

    #[test]
    fn schema8_explicit_empty_population_is_readable() {
        let current = schema8_row(FIX, 1, true, "super");
        let mut value = serde_json::to_value(current).unwrap();
        value["cell_results"]["selected_count"] = serde_json::json!(0);
        value["cell_results"]["recorded_count"] = serde_json::json!(0);
        value["cell_results"]["selected"] = serde_json::json!([]);
        value["cell_results"]["cells"] = serde_json::json!([]);
        value["cell_results"]["artifact"]["row_count"] = serde_json::json!(0);
        value["cell_results"]["population_sha256"] = serde_json::json!(population_sha256(&[]));
        let current: HistoryRow = serde_json::from_value(value).unwrap();
        assert!(
            validated_evidence(&current, CELL_RESULTS_SCHEMA_MIN, CELL_RESULTS_SCHEMA_MAX,)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn schema8_same_run_cannot_change_validate_path() {
        let quick = schema8_row(FIX, 2, true, "quick");
        let full = schema8_row(FIX, 2, true, "full");
        let rows = [quick, full.clone()];

        let error = unresolved(
            &rows,
            FIX,
            &[full],
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
            |ancestor, descendant| Ok(ancestor == descendant),
        )
        .unwrap_err();

        assert_eq!(
            error,
            "conflicting correction rows for run_id run-2222222222222222222222222222222222222222-2"
        );
    }

    #[test]
    fn newer_cell_results_are_readable_but_have_no_observation_authority() {
        let mut newer = row(OTHER, 1, false);
        newer.schema_version = Some(CELL_RESULTS_SCHEMA_MAX + 1);
        newer.cell_results = Some(CellResultsValue::Other(serde_json::json!({
            "future_shape": true,
        })));
        assert!(
            validated_evidence(&newer, CELL_RESULTS_SCHEMA_MIN, CELL_RESULTS_SCHEMA_MAX,)
                .unwrap()
                .is_none()
        );

        let target = row(FIX, 2, true);
        assert!(evaluate(&[newer.clone(), target], FIX).unwrap().is_empty());

        let mut known_shape_at_future_schema = schema8_row(OTHER, 1, false, "full");
        known_shape_at_future_schema.schema_version = Some(CELL_RESULTS_SCHEMA_MAX + 1);
        assert!(matches!(
            known_shape_at_future_schema.cell_results,
            Some(crate::records::CellResultsValue::Other(_))
        ));
        assert!(validated_evidence(
            &known_shape_at_future_schema,
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
        )
        .unwrap()
        .is_none());
        let target = row(FIX, 2, true);
        assert!(evaluate(&[known_shape_at_future_schema, target], FIX)
            .unwrap()
            .is_empty());

        newer.schema_version = Some(CELL_RESULTS_SCHEMA_MAX);
        assert!(
            validated_evidence(&newer, CELL_RESULTS_SCHEMA_MIN, CELL_RESULTS_SCHEMA_MAX,).is_err()
        );
    }

    #[test]
    fn schema8_policy_maximum_alone_cannot_grant_schema9_observation_authority() {
        let mut row = schema8_row(OTHER, 1, false, "full");
        row.schema_version = Some(9);
        // The shared dependency already decodes the V9 summary. This positive
        // control makes the parent's explicit schema membership guard necessary.
        assert!(row.cell_results_evidence().is_some());
        assert!(validated_evidence(&row, 6, 9).unwrap().is_none());
    }

    #[test]
    fn unselected_failed_cell_does_not_slow_an_unrelated_candidate() {
        let failed = row(BAD, 1, false);
        let mut unrelated = row(FIX, 2, true);
        let evidence = unrelated
            .cell_results
            .as_mut()
            .unwrap()
            .typed_mut()
            .unwrap();
        evidence.selected[0].test = "unrelated-cell".into();
        evidence.cells[0].test = "unrelated-cell".into();
        evidence.population_sha256 = population_sha256(&evidence.selected);
        let rows = [failed, unrelated];
        assert!(evaluate(&rows, FIX).unwrap().is_empty());
    }

    #[test]
    fn missing_target_cell_evidence_cannot_bypass_an_open_obligation() {
        let failed = row(BAD, 1, false);
        let mut untyped_target = row(FIX, 2, true);
        untyped_target.schema_version = Some(5);
        untyped_target.run_id = None;
        untyped_target.cell_results = None;
        let rows = [failed, untyped_target.clone()];
        let obligations = unresolved(
            &rows,
            FIX,
            &[untyped_target],
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
            |ancestor, descendant| {
                Ok(ancestor == descendant || (ancestor == BAD && descendant == FIX))
            },
        )
        .unwrap();
        assert_eq!(obligations.len(), 1);
        assert_eq!(obligations[0].observed_clean_legs_at_target, 0);
    }

    #[test]
    fn schema8_explicit_empty_selection_does_not_borrow_an_older_population() {
        let failed = row(BAD, 1, false);
        let empty = schema8_row(FIX, 2, true, "super");
        let mut value = serde_json::to_value(empty).unwrap();
        value["cell_results"]["selected_count"] = serde_json::json!(0);
        value["cell_results"]["recorded_count"] = serde_json::json!(0);
        value["cell_results"]["selected"] = serde_json::json!([]);
        value["cell_results"]["cells"] = serde_json::json!([]);
        value["cell_results"]["artifact"]["row_count"] = serde_json::json!(0);
        value["cell_results"]["population_sha256"] = serde_json::json!(population_sha256(&[]));
        let empty: HistoryRow = serde_json::from_value(value).unwrap();
        let rows = [failed, empty.clone()];

        let obligations = unresolved(
            &rows,
            FIX,
            &[empty],
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
            |ancestor, descendant| {
                Ok(ancestor == descendant || (ancestor == BAD && descendant == FIX))
            },
        )
        .unwrap();

        assert!(obligations.is_empty());
    }

    #[test]
    fn same_commit_failure_cannot_be_overridden_by_a_lucky_pass() {
        let rows = vec![row(BAD, 1, false), row(BAD, 2, true)];
        let obligations = evaluate(&rows, BAD).unwrap();
        assert_eq!(obligations.len(), 1);
        assert_eq!(obligations[0].target_failures, 1);
        assert_eq!(obligations[0].observed_clean_legs_at_target, 3);
    }

    #[test]
    fn historical_startup_race_cannot_qualify_on_one_pass() {
        // The measured unfixed fixture: 5 reordered legs in 1,080.  Each typed
        // verify observation represents two legs, so 540 comparisons carry five
        // divergences.  A subsequent one-comparison pass is still far below the
        // calibrated budget.
        let mut rows = Vec::new();
        for ordinal in 0..540 {
            rows.push(row(BAD, ordinal, ordinal >= 5));
        }
        rows.push(row(FIX, 600, true));
        let obligations = evaluate(&rows, FIX).unwrap();
        assert_eq!(obligations.len(), 1);
        assert_eq!(obligations[0].baseline_failures, 1);
        assert_eq!(obligations[0].baseline_legs, 270);
        assert_eq!(obligations[0].required_clean_legs, 1620);
        assert_eq!(obligations[0].observed_clean_legs_at_target, 2);
    }

    #[test]
    fn measured_fixed_budget_clears_the_startup_race_obligation() {
        let mut rows = Vec::new();
        for ordinal in 0..540 {
            rows.push(row(BAD, ordinal, ordinal >= 5));
        }
        let required = required_clean_legs(1, 270).unwrap();
        assert_eq!(required, 1620);
        for ordinal in 0..809 {
            rows.push(targeted_producer_row(FIX, 600 + ordinal, true));
        }
        assert_eq!(evaluate(&rows, FIX).unwrap().len(), 1);
        rows.clear();
        for ordinal in 0..540 {
            rows.push(row(BAD, ordinal, ordinal >= 5));
        }
        for ordinal in 0..810 {
            rows.push(targeted_producer_row(FIX, 600 + ordinal, true));
        }
        assert!(evaluate(&rows, FIX).unwrap().is_empty());
    }

    #[test]
    fn repeated_physical_run_id_cannot_fake_the_1620_leg_budget() {
        let failed = row(BAD, 1, false);
        let one_physical_run = targeted_producer_row(FIX, 600, true);
        let mut rows = vec![failed];
        rows.extend(std::iter::repeat_n(one_physical_run, 810));
        let obligations = evaluate(&rows, FIX).unwrap();
        assert_eq!(obligations.len(), 1);
        assert_eq!(obligations[0].required_clean_legs, 1620);
        assert_eq!(obligations[0].observed_clean_legs_at_target, 2);
    }

    #[test]
    fn calibration_scales_with_the_observed_rate() {
        let frequent = required_clean_legs(2, 6).unwrap();
        let rare = required_clean_legs(1, 270).unwrap();
        assert!(
            frequent <= 20,
            "PASS/FAIL/FAIL must take a small targeted budget"
        );
        assert_eq!(rare, 1620);
    }

    #[test]
    fn correction_rows_do_not_double_count_one_physical_run() {
        let failed = uncalibrated_row(BAD, 1, false);
        let mut correction = failed.clone();
        correction.finished_at = Some("2026-08-17T00:02:00Z".into());
        let target = uncalibrated_row(FIX, 3, true);
        let rows = [failed, correction, target];
        let obligations = evaluate(&rows, FIX).unwrap();
        assert_eq!(obligations.len(), 1);
        assert_eq!(obligations[0].baseline_failures, 1);
        assert_eq!(obligations[0].baseline_legs, 2);
    }

    #[test]
    fn conflicting_correction_rows_are_refused() {
        let failed = uncalibrated_row(BAD, 1, false);
        let mut conflict = uncalibrated_row(BAD, 1, true);
        conflict.finished_at = Some("2026-08-17T00:02:00Z".into());
        let target = uncalibrated_row(FIX, 3, true);
        assert!(evaluate(&[failed, conflict, target], FIX).is_err());
    }

    #[test]
    fn unrelated_branch_repetitions_cannot_clear_an_obligation() {
        let mut rows = vec![row(BAD, 1, false)];
        for ordinal in 0..810 {
            rows.push(row(OTHER, 10 + ordinal, true));
        }
        rows.push(row(FIX, 900, true));
        let target_receipts = target_rows(&rows, FIX);
        let obligations = unresolved(
            &rows,
            FIX,
            &target_receipts,
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
            |ancestor, descendant| {
                Ok(ancestor == descendant || (ancestor == BAD && descendant == FIX))
            },
        )
        .unwrap();
        assert_eq!(obligations.len(), 1);
        assert_eq!(obligations[0].observed_clean_legs_at_target, 2);
    }

    #[test]
    fn selective_sibling_cannot_remove_a_full_receipts_selected_obligation() {
        let failed = row(BAD, 1, false);
        let full_target = row(FIX, 2, true);
        let mut selective = row(FIX, 3, true);
        let evidence = selective
            .cell_results
            .as_mut()
            .unwrap()
            .typed_mut()
            .unwrap();
        evidence.selected[0].test = "unrelated-cell".into();
        evidence.cells[0].test = "unrelated-cell".into();
        evidence.population_sha256 = population_sha256(&evidence.selected);
        let rows = [failed, full_target.clone(), selective];
        let obligations = unresolved(
            &rows,
            FIX,
            &[full_target],
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
            |ancestor, descendant| {
                Ok(ancestor == descendant || (ancestor == BAD && descendant == FIX))
            },
        )
        .unwrap();
        assert_eq!(obligations.len(), 1);
    }

    #[test]
    fn malformed_schema6_summary_is_refused() {
        let mut malformed = row(BAD, 1, false);
        malformed
            .cell_results
            .as_mut()
            .unwrap()
            .typed_mut()
            .unwrap()
            .run_id = "other-run".into();
        let target = row(FIX, 2, true);
        assert!(evaluate(&[malformed, target], FIX).is_err());
    }

    #[test]
    fn targeted_schema6_repetitions_clear_only_the_selected_obligation() {
        let mut rows = vec![row(BAD, 1, false)];
        let mut full_target = row(FIX, 2, true);
        full_target
            .extra
            .insert("selection_mode".into(), serde_json::json!("full"));
        rows.push(full_target.clone());
        for ordinal in 0..809 {
            rows.push(targeted_producer_row(FIX, 10 + ordinal, true));
        }
        assert!(unresolved(
            &rows,
            FIX,
            &[full_target],
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
            |ancestor, descendant| {
                Ok(ancestor == descendant || (ancestor == BAD && descendant == FIX))
            },
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn later_timestamp_on_an_older_ancestor_cannot_clear_a_descendant_failure() {
        let mut rows = vec![row(BAD, 1, false)];
        for ordinal in 0..810 {
            rows.push(row(OLD, 10 + ordinal, true));
        }
        let target = row(FIX, 900, true);
        rows.push(target.clone());
        let obligations = unresolved(
            &rows,
            FIX,
            &[target],
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
            |ancestor, descendant| {
                Ok(ancestor == descendant
                    || (ancestor == OLD && (descendant == BAD || descendant == FIX))
                    || (ancestor == BAD && descendant == FIX))
            },
        )
        .unwrap();
        assert_eq!(obligations.len(), 1);
    }

    #[test]
    fn ancestry_lookup_error_fails_closed() {
        let rows = [row(BAD, 1, false), row(FIX, 2, true)];
        let error = unresolved(
            &rows,
            FIX,
            &[rows[1].clone()],
            CELL_RESULTS_SCHEMA_MIN,
            CELL_RESULTS_SCHEMA_MAX,
            |ancestor, descendant| {
                if ancestor == BAD && descendant == FIX {
                    Err("missing commit object".into())
                } else {
                    Ok(ancestor == descendant)
                }
            },
        )
        .unwrap_err();
        assert!(error.contains("missing commit object"));
    }

    #[test]
    fn malformed_tree_in_supported_cell_results_fails_by_name() {
        let mut malformed = row(FIX, 1, true);
        malformed
            .extra
            .insert("tree".into(), serde_json::json!("unknown"));
        let error = evaluate(&[malformed], FIX).unwrap_err();
        assert_eq!(
            error,
            "malformed HistoryRow tree: expected exactly 40 hexadecimal characters"
        );
    }

    fn schema10_obligations(rows: &[HistoryRow], store: &Path) -> Vec<UnresolvedFailureObligation> {
        unresolved_in_store(
            rows,
            FIX,
            &target_rows(rows, FIX),
            6,
            10,
            Some(store),
            |ancestor, descendant| {
                Ok(ancestor == descendant || (ancestor == BAD && descendant == FIX))
            },
        )
        .unwrap()
    }

    fn schema8_candidate_target(source: &HistoryRow, ordinal: u64) -> HistoryRow {
        let evidence = source.schema10_cell_results().unwrap().unwrap();
        let mut target = serde_json::to_value(schema8_row(FIX, ordinal, true, "full")).unwrap();
        target["cell_results"]["selected"] = serde_json::to_value(&evidence.selected).unwrap();
        target["cell_results"]["cells"] = serde_json::json!([evidence.cells[0].ordinary()]);
        target["cell_results"]["population_sha256"] =
            serde_json::json!(population_sha256(&evidence.selected));
        target["finished_at"] = serde_json::json!(format!("2026-09-16T00:{ordinal:02}:00Z"));
        serde_json::from_value(target).unwrap()
    }

    #[test]
    fn schema10_ordinary_candidate_pass_cannot_clear_parity_or_reference_failure() {
        use crate::validation_evidence::fixtures::{Fixture, Store};
        for (name, reference) in [
            ("cross-diverged-then-matched", false),
            ("reference-diverged", true),
        ] {
            let store = Store::new();
            let failed = Fixture::new(name, "failed", BAD, 1).write(&store.0);
            let target = schema8_candidate_target(&failed, 2);
            let obligations = schema10_obligations(&[failed, target], &store.0);
            assert_eq!(obligations.len(), 1);
            let obligation = &obligations[0];
            assert_eq!(obligation.observed_clean_legs_at_target, 0);
            assert_eq!(obligation.target_failures, 0);
            assert_eq!(obligation.backend, if reference { "ptrace" } else { "kvm" });
            assert_eq!(
                matches!(obligation.relation, ComparisonRelationV10::Reference { .. }),
                reference
            );
            assert!(!matches!(
                obligation.relation,
                ComparisonRelationV10::Ordinary
            ));
            let description = obligation.comparison_description().unwrap();
            assert!(description.contains(if reference {
                "reference comparison"
            } else {
                "parity comparison with ptrace"
            }));
            assert!(serde_json::to_value(obligation)
                .unwrap()
                .get("relation")
                .is_some());
        }
    }

    #[test]
    fn schema10_only_enough_clean_evidence_for_the_same_relation_clears_failure() {
        use crate::validation_evidence::fixtures::{Fixture, Store};
        let store = Store::new();
        let failed = Fixture::new("cross-diverged-then-matched", "failed", BAD, 1).write(&store.0);
        let mut rows = vec![failed];
        for ordinal in 0..10 {
            rows.push(
                Fixture::new("matched", &format!("clean-{ordinal}"), FIX, ordinal + 2)
                    .write(&store.0),
            );
        }
        let obligations = schema10_obligations(&rows, &store.0);
        assert_eq!(obligations.len(), 1);
        assert_eq!(obligations[0].baseline_legs, 4);
        assert_eq!(obligations[0].baseline_failures, 1);
        assert_eq!(obligations[0].required_clean_legs, 30);
        assert_eq!(obligations[0].observed_clean_legs_at_target, 20);
        rows.push(Fixture::new("matched", "clean-10", FIX, 12).write(&store.0));
        let obligations = schema10_obligations(&rows, &store.0);
        assert_eq!(obligations.len(), 1);
        assert_eq!(obligations[0].required_clean_legs, 30);
        assert_eq!(obligations[0].observed_clean_legs_at_target, 22);
        for ordinal in 11..14 {
            rows.push(
                Fixture::new("matched", &format!("clean-{ordinal}"), FIX, ordinal + 2)
                    .write(&store.0),
            );
        }
        let obligations = schema10_obligations(&rows, &store.0);
        assert_eq!(obligations.len(), 1);
        assert_eq!(obligations[0].required_clean_legs, 30);
        assert_eq!(obligations[0].observed_clean_legs_at_target, 28);
        rows.push(Fixture::new("matched", "clean-14", FIX, 16).write(&store.0));
        assert!(schema10_obligations(&rows, &store.0).is_empty());

        // A target's earlier unavailable cross comparison prevents its later
        // matches from constituting a clean commit, even with enough legs.
        rows.push(Fixture::new("reference-diverged", "unavailable", FIX, 17).write(&store.0));
        let obligations = schema10_obligations(&rows, &store.0);
        let cross = obligations
            .iter()
            .find(|obligation| {
                matches!(
                    obligation.relation,
                    ComparisonRelationV10::BackendParity { .. }
                )
            })
            .unwrap();
        assert_eq!(cross.observed_clean_legs_at_target, 30);
        assert_eq!(cross.target_failures, 0);
        assert!(!cross.target_comparisons_complete);
        assert_eq!(cross.target_unavailable_comparisons, 1);
        assert_eq!(
            cross.target_comparison_status(),
            "comparisons incomplete: 1 unavailable comparison(s)"
        );
        let serialized = serde_json::to_value(cross).unwrap();
        assert_eq!(serialized["target_comparisons_complete"], false);
        assert_eq!(serialized["target_unavailable_comparisons"], 1);
    }

    #[test]
    fn schema10_corrections_cannot_change_a_physical_runs_bound_identity() {
        use crate::validation_evidence::fixtures::{Fixture, Store};
        let store = Store::new();
        let failed = Fixture::new("cross-diverged-then-matched", "failed", BAD, 1).write(&store.0);
        let mut correction = failed.clone();
        correction
            .extra
            .insert("toolchain".into(), serde_json::json!("changed-toolchain"));
        let target = schema8_candidate_target(&failed, 2);
        let error = unresolved_in_store(
            &[failed, correction, target.clone()],
            FIX,
            &[target],
            6,
            10,
            Some(&store.0),
            |_, _| Ok(true),
        )
        .unwrap_err();
        assert_eq!(error, "conflicting correction rows for run_id failed");
    }
}
