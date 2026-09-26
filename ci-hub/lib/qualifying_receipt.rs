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
use crate::records::{CoverageRow, HistoryRow, ValidatePath};
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

/// Shared with the Python predicate so every current-schema passing row reports
/// the same failed condition for an absent, malformed, or contradictory count.
pub const PASSED_TESTS_REFUSAL_REASON: &str = "passed_tests != executed_tests for passing row";
pub const FILTERED_TESTS_I64_REFUSAL_REASON: &str =
    "filtered_tests does not fit signed 64-bit integer";
pub const COUNT_REPRESENTATION_SCHEMA_MIN: u32 = 4;
pub const COUNT_REPRESENTATION_FIELDS: [&str; 3] =
    ["executed_tests", "passed_tests", "filtered_tests"];
pub const COUNT_REPRESENTATION_SIGNED_BITS: u8 = 64;

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
    /// Builder whose E2E payload a qualifying green executed, as
    /// [`release_builder_of`] derives it. The predicate file itself must name
    /// the builder, as the Python loader requires.
    pub release_builder: String,
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

/// Framework-owned passing-test count and the schema at which it is mandatory.
#[derive(Clone, Debug, Deserialize)]
pub struct PassedTestsClause {
    pub applies_at_schema_min: u32,
}

/// Fixed cross-language representation of top-level receipt count fields.
#[derive(Clone, Debug, Deserialize)]
pub struct CountRepresentationClause {
    pub applies_at_schema_min: u32,
    pub fields: Vec<String>,
    pub signed_bits: u8,
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
    pub passed_tests: PassedTestsClause,
    pub count_representation: CountRepresentationClause,
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
    /// Exact `e2e_payload` identity for each `release_builder`, mirroring
    /// Hermit's `e2e_payload_identity`. Read through [`release_builder_of`].
    pub release_payloads: std::collections::BTreeMap<String, serde_json::Value>,
    /// First instant at which a Hermit row may come from a writer that records
    /// `release_builder` and `e2e_payload`. Required, with no default: an
    /// absent cutoff would silently readmit every row that omits the pair.
    pub release_builder_required_from_finished_at: String,
    /// Row schemas written before the pair existed; a row without the pair is
    /// Cargo only at one of these. Required, with no default: an absent list
    /// would leave missing and future schemas undecided.
    pub release_builder_pairless_schema_versions: Vec<u32>,
}

/// Rows whose `repo` names Reverie. Reverie has only a Cargo build and its
/// producers never wrote a builder, so its rows carry neither field at any date.
const REVERIE_REPOS: [&str; 2] = ["reverie", "rrnewton/reverie"];

/// Rows whose `repo` names Hermit; an absent `repo` is a Hermit row.
const HERMIT_REPOS: [&str; 2] = ["hermit", "rrnewton/hermit"];

/// Whether `ts` is a real instant in fixed-width RFC3339 UTC. The shape alone
/// would admit `2026-13-45T99:00:00Z`, which still sorts before a cutoff; a
/// date that does not exist is not ordered against one. `qualifying_receipt.py`
/// `is_utc_timestamp` mirrors this. Python's `datetime` refuses a leap second
/// and year 0, both of which chrono parses, so they are refused explicitly.
pub(crate) fn is_utc_timestamp(ts: &str) -> bool {
    is_rfc3339_z(ts)
        && &ts[17..19] < "60"
        && &ts[0..4] != "0000"
        && chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%SZ").is_ok()
}

/// The builder whose E2E payload a validate row executed, or `None`.
///
/// `release_builder` and `e2e_payload` are read together. When both are
/// present the row names a builder only when `release_builder` is a key of
/// `release_payloads` and `e2e_payload` is exactly that builder's identity
/// (`serde_json` equality keeps `true` distinct from `1`).
///
/// A row with neither field was a Cargo run only where the fields could not
/// yet exist, and only at a `schema_version` listed in
/// `release_builder_pairless_schema_versions`: a Reverie row, or a Hermit row
/// whose `finished_at` is a real UTC
/// instant before `release_builder_required_from_finished_at`, when the first
/// Hermit writer of the pair was authored. A Hermit row from that instant on,
/// or with an absent or malformed `finished_at`, may come from a writer that
/// records the pair, so omitting both is not evidence of Cargo.
///
/// Every other shape names no builder, so no consumer may treat it as Cargo
/// evidence. `qualifying_receipt.py` mirrors this.
pub fn release_builder_of<'p>(row: &HistoryRow, pred: &'p QualifyingPredicate) -> Option<&'p str> {
    match (row.extra.get("release_builder"), row.extra.get("e2e_payload")) {
        (None, None) => {
            let cargo = pred
                .release_payloads
                .get_key_value("cargo")
                .map(|(builder, _)| builder.as_str())?;
            if !row
                .schema_version
                .is_some_and(|schema| pred.release_builder_pairless_schema_versions.contains(&schema))
            {
                return None;
            }
            let repo = row.repo.as_deref();
            if repo.is_some_and(|repo| REVERIE_REPOS.contains(&repo)) {
                return Some(cargo);
            }
            if repo.is_some_and(|repo| !HERMIT_REPOS.contains(&repo)) {
                return None;
            }
            let cutoff = pred.release_builder_required_from_finished_at.as_str();
            let finished = row.finished_at.as_deref()?;
            (is_utc_timestamp(finished) && is_utc_timestamp(cutoff) && finished < cutoff)
                .then_some(cargo)
        }
        (Some(builder), Some(payload)) => pred
            .release_payloads
            .get_key_value(builder.as_str()?)
            .filter(|(_, identity)| *identity == payload)
            .map(|(builder, _)| builder.as_str()),
        _ => None,
    }
}

/// The builder/payload case table shared with the Python consumers' tests, so
/// every consumer in both languages is checked against the same shapes.
#[cfg(test)]
pub(crate) mod release_payload_cases {
    use serde_json::Value;
    use std::collections::BTreeMap;

    #[derive(serde::Deserialize)]
    pub(crate) struct Case {
        pub(crate) label: String,
        /// Keys removed from the row before `fields` is applied.
        #[serde(default)]
        absent: Vec<String>,
        fields: BTreeMap<String, Value>,
        /// What [`super::release_builder_of`] must return.
        pub(crate) expected: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct Table {
        cases: Vec<Case>,
    }

    impl Case {
        pub(crate) fn expected(&self) -> Option<&str> {
            self.expected.as_deref()
        }

        /// Applies the case to a typed row. `repo`, `finished_at` and
        /// `schema_version` are typed fields, so they are set there rather
        /// than in `extra`.
        pub(crate) fn apply(&self, row: &mut crate::records::HistoryRow) {
            fn text(key: &str, value: &Value) -> Option<String> {
                match value {
                    Value::Null => None,
                    Value::String(text) => Some(text.clone()),
                    other => panic!("fixture {key} must be a string or null, not {other}"),
                }
            }
            row.extra.remove("release_builder");
            row.extra.remove("e2e_payload");
            for key in &self.absent {
                match key.as_str() {
                    "repo" => row.repo = None,
                    "finished_at" => row.finished_at = None,
                    "schema_version" => row.schema_version = None,
                    other => {
                        row.extra.remove(other);
                    }
                }
            }
            for (key, value) in &self.fields {
                match key.as_str() {
                    "repo" => row.repo = text(key, value),
                    "finished_at" => row.finished_at = text(key, value),
                    "schema_version" => {
                        row.schema_version = match value {
                            Value::Null => None,
                            other => Some(
                                other
                                    .as_u64()
                                    .and_then(|schema| u32::try_from(schema).ok())
                                    .unwrap_or_else(|| {
                                        panic!("fixture schema_version must be a u32 or null, not {other}")
                                    }),
                            ),
                        }
                    }
                    _ => {
                        row.extra.insert(key.clone(), value.clone());
                    }
                }
            }
        }

        /// [`Case::apply`] for a raw JSON row object.
        pub(crate) fn apply_json(&self, raw: &mut Value) {
            let row = raw.as_object_mut().expect("a JSON row object");
            row.remove("release_builder");
            row.remove("e2e_payload");
            for key in &self.absent {
                row.remove(key);
            }
            row.extend(self.fields.clone());
        }
    }

    pub(crate) fn all() -> Vec<Case> {
        let table: Table = serde_json::from_str(include_str!(
            "../validate/tests/fixtures/release-payload-cases.json"
        ))
        .expect("release-payload-cases.json parses");
        table.cases
    }

    pub(crate) fn buck() -> Case {
        all()
            .into_iter()
            .find(|case| case.label == "consistent buck")
            .expect("the table names a consistent buck case")
    }
}

fn default_accepts_green_class() -> Vec<String> {
    vec!["hard".to_string()]
}

impl QualifyingPredicate {
    /// Parse from JSON text, mapping errors to a message that names the source.
    pub fn parse(text: &str, origin: &str) -> Result<Self, String> {
        let predicate: Self = serde_json::from_str(text)
            .map_err(|e| format!("{origin}: malformed qualifying predicate: {e}"))?;
        let representation = &predicate.count_representation;
        let fields_match = representation
            .fields
            .iter()
            .map(String::as_str)
            .eq(COUNT_REPRESENTATION_FIELDS);
        if representation.applies_at_schema_min != COUNT_REPRESENTATION_SCHEMA_MIN
            || !fields_match
            || representation.signed_bits != COUNT_REPRESENTATION_SIGNED_BITS
        {
            return Err(format!(
                "{origin}: qualifying predicate count_representation must declare schema {}, fields {:?}, and {} signed bits",
                COUNT_REPRESENTATION_SCHEMA_MIN,
                COUNT_REPRESENTATION_FIELDS,
                COUNT_REPRESENTATION_SIGNED_BITS,
            ));
        }
        let payloads = &predicate.release_payloads;
        if !payloads.contains_key(&predicate.require.release_builder)
            || !payloads
                .values()
                .all(|identity| identity.as_object().is_some_and(|fields| !fields.is_empty()))
        {
            return Err(format!(
                "{origin}: qualifying predicate release_payloads must map each builder, including require.release_builder, to a non-empty payload identity"
            ));
        }
        if !is_utc_timestamp(&predicate.release_builder_required_from_finished_at) {
            return Err(format!(
                "{origin}: qualifying predicate release_builder_required_from_finished_at must be a UTC RFC3339 timestamp such as 2026-09-25T20:42:29Z"
            ));
        }
        let schemas = &predicate.release_builder_pairless_schema_versions;
        if schemas.is_empty()
            || schemas.contains(&0)
            || schemas.iter().collect::<std::collections::BTreeSet<_>>().len() != schemas.len()
        {
            return Err(format!(
                "{origin}: qualifying predicate release_builder_pairless_schema_versions must be a non-empty list of distinct positive integers"
            ));
        }
        Ok(predicate)
    }
}

/// Classify malformed count types before deserializing a current-schema pass
/// row into [`HistoryRow`]. Missing, null, and representable signed values
/// deliberately remain on the normal typed predicate path, as do numeric
/// mismatches. Rows before the policy cutoff retain their historical
/// deserialization behavior.
pub fn raw_current_schema_count_type_refusal(
    row: &serde_json::Value,
    pred: &QualifyingPredicate,
) -> Option<&'static str> {
    if row.get("result").and_then(serde_json::Value::as_str) != Some(pred.require.result.as_str()) {
        return None;
    }
    let schema = row
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|schema| u32::try_from(schema).ok())?;
    if schema < pred.count_representation.applies_at_schema_min {
        return None;
    }
    for field in &pred.count_representation.fields {
        let malformed = row
            .get(field)
            .is_some_and(|value| !value.is_null() && value.as_i64().is_none());
        if malformed {
            return Some(if field == "filtered_tests" {
                FILTERED_TESTS_I64_REFUSAL_REASON
            } else {
                PASSED_TESTS_REFUSAL_REASON
            });
        }
    }
    None
}

/// Apply the versioned passed-test count contract to an already typed row.
/// Schema-4+ rows require both counts and exact equality; older rows retain
/// the historical rule that an absent count is allowed but a present mismatch
/// is contradictory evidence.
pub fn passed_tests_satisfy_schema(row: &HistoryRow, pred: &QualifyingPredicate) -> bool {
    let schema = row.schema_version.unwrap_or(0);
    let exact = row
        .passed_tests
        .is_some_and(|passed| Some(passed) == row.executed_tests);
    if schema >= pred.passed_tests.applies_at_schema_min {
        exact
    } else {
        row.passed_tests.is_none() || exact
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
    Refused(String),
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
    fn refused(reason: impl Into<String>) -> Self {
        Self::Refused(reason.into())
    }

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
struct SharedEvidenceReport {
    accepted: bool,
    evidence_reason: String,
    admission_satisfied: Option<bool>,
    admission_status: String,
    base_satisfied: Option<bool>,
    base_status: String,
    coverage_schema_satisfied: Option<bool>,
    coverage_schema_status: String,
    cell_results_satisfied: Option<bool>,
    cell_results_status: String,
}

/// This response judges exactly three clauses. It carries neither a cell
/// verdict nor permission to qualify the whole receipt.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverageBaseAdmissionReport {
    scope: String,
    refusal: Option<String>,
    admission_satisfied: Option<bool>,
    admission_status: String,
    base_satisfied: Option<bool>,
    base_status: String,
    coverage_schema_satisfied: Option<bool>,
    coverage_schema_status: String,
}

impl CoverageBaseAdmissionReport {
    fn outcome(self, exit_code: Option<i32>) -> BoundaryOutcome {
        let clauses = [
            (
                "coverage schema",
                self.coverage_schema_status.as_str(),
                self.coverage_schema_satisfied,
            ),
            ("base", self.base_status.as_str(), self.base_satisfied),
            (
                "admission",
                self.admission_status.as_str(),
                self.admission_satisfied,
            ),
        ];
        let expected = clauses.iter().find_map(|(prefix, status, _)| {
            (!evidence_status_accepted(status)).then(|| format!("{prefix} {status}"))
        });
        if self.scope != "coverage-base-admission/v1"
            || !clauses
                .iter()
                .all(|(_, status, satisfied)| evidence_status_matches(status, *satisfied))
            || self.refusal != expected
            || exit_code != Some(i32::from(expected.is_some()))
        {
            return BoundaryOutcome::Indeterminate("verifier-self-inconsistent");
        }
        match expected {
            Some(reason) => BoundaryOutcome::Refused(reason),
            None => BoundaryOutcome::Accepted,
        }
    }
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
    Refused(String),
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

    /// The authority's canonical refusal reason, when it reached a negative
    /// verdict about the receipt itself.
    pub fn refusal_reason(&self) -> Option<&str> {
        match self {
            BoundaryOutcome::Refused(reason) => Some(reason),
            _ => None,
        }
    }
}

fn evidence_status_accepted(status: &str) -> bool {
    matches!(status, "satisfied" | "grandfathered-unknown")
}

fn evidence_status_matches(status: &str, satisfied: Option<bool>) -> bool {
    match status {
        "satisfied" => satisfied == Some(true),
        "grandfathered-unknown" => satisfied.is_none(),
        _ => satisfied == Some(false),
    }
}

impl SharedEvidenceReport {
    fn statuses_accepted(&self) -> bool {
        evidence_status_accepted(&self.coverage_schema_status)
            && evidence_status_accepted(&self.base_status)
            && evidence_status_accepted(&self.admission_status)
            && evidence_status_accepted(&self.cell_results_status)
    }

    fn expected_reason(&self) -> String {
        for (prefix, status) in [
            ("coverage schema", self.coverage_schema_status.as_str()),
            ("base", self.base_status.as_str()),
            ("admission", self.admission_status.as_str()),
            ("cell_results", self.cell_results_status.as_str()),
        ] {
            if !evidence_status_accepted(status) {
                return format!("{prefix} {status}");
            }
        }
        "qualifies".into()
    }

    fn statuses_match(&self) -> bool {
        evidence_status_matches(&self.admission_status, self.admission_satisfied)
            && evidence_status_matches(&self.base_status, self.base_satisfied)
            && evidence_status_matches(&self.coverage_schema_status, self.coverage_schema_satisfied)
            && evidence_status_matches(&self.cell_results_status, self.cell_results_satisfied)
    }

    fn internally_consistent(&self, exit_code: Option<i32>) -> bool {
        let exit_matches = match exit_code {
            Some(0) => self.accepted,
            Some(1) => !self.accepted,
            _ => false,
        };
        exit_matches
            && self.statuses_match()
            && self.accepted == self.statuses_accepted()
            && self.evidence_reason == self.expected_reason()
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
    let exit_matches = match output.status.code() {
        Some(0) => true,
        Some(1) => false,
        Some(3) => {
            return BoundaryOutcome::Indeterminate(
                "merge-boundary authority could not dereference the producer definition (rc 3)",
            );
        }
        _ => {
            return BoundaryOutcome::Indeterminate("merge-boundary authority exited abnormally");
        }
    };
    let Ok(report) = serde_json::from_slice::<BaseBoundaryReport>(&output.stdout) else {
        return BoundaryOutcome::Indeterminate(
            "merge-boundary authority emitted no parsable report",
        );
    };
    if report.accepted != exit_matches
        || !evidence_status_matches(&report.base_status, report.base_satisfied)
    {
        return BoundaryOutcome::Indeterminate("merge-boundary authority contradicted itself");
    }
    if report.accepted {
        BoundaryOutcome::Accepted
    } else {
        BoundaryOutcome::Refused(format!("base {}", report.base_status))
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
///   `verifier-output-unparseable` it ran and answered, but the answer is not a
///                                `SharedEvidenceReport`. This consumer and the Python
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
    let mut request = serde_json::json!({
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
    // Decode-only raw IDs are omitted from historical canonical bytes.
    // Restore each actual value (including null and duplicate sentinels) only
    // in this request, so Python judges the original optional claim at its
    // existing coverage -> base -> admission -> cells boundary.
    if row.extra.contains_key("admission_floor_evidence") {
        if let Some(run_id) = row
            .coverage
            .as_ref()
            .and_then(|coverage| coverage.admission_run_id.as_ref())
        {
            request["row"]["coverage"]["run_id"] = run_id.clone();
        }
        if let Some(run_id) = row
            .cell_results
            .as_ref()
            .and_then(|cells| cells.admission_run_id())
        {
            request["row"]["cell_results"]["run_id"] = run_id;
        }
        if let Some(run_id) = row
            .test_results
            .as_ref()
            .and_then(|tests| tests.admission_run_id())
        {
            request["row"]["test_results"]["run_id"] = run_id;
        }
        if let Some(log_identity) = row.admission_log_identity() {
            request["row"]["log_identity"] = log_identity;
        }
    }
    let Ok(request_bytes) = serde_json::to_vec(&request) else {
        return BoundaryOutcome::Indeterminate("verifier-request-undelivered");
    };
    let Ok(output) = Command::new("python3")
        .arg(verifier)
        .arg(if row.schema_version == Some(10) {
            "--coverage-base-admission-only"
        } else {
            "--evidence-only"
        })
        .bounded_output_with_input(&request_bytes)
    else {
        return BoundaryOutcome::Indeterminate("verifier-not-reaped");
    };
    if row.schema_version == Some(10) {
        let Ok(report) = serde_json::from_slice::<CoverageBaseAdmissionReport>(&output.stdout)
        else {
            return BoundaryOutcome::Indeterminate("verifier-output-unparseable");
        };
        let common = report.outcome(output.status.code());
        if !common.accepted() {
            return common;
        }
        if 10 < pred.cell_results.applies_at_schema_min
            || 10 > pred.cell_results.supported_schema_max
        {
            return BoundaryOutcome::Refused("cell_results schema-unavailable".into());
        }
        return match crate::validation_evidence::verify_current_store(row) {
            crate::validation_evidence::EvidenceOutcome::Verified {
                full_validation_refusal,
                ..
            } => match full_validation_refusal {
                Some(reason) => BoundaryOutcome::Refused(reason),
                None => BoundaryOutcome::Accepted,
            },
            crate::validation_evidence::EvidenceOutcome::LegacyUnbound {
                full_validation_refusal,
                ..
            } => BoundaryOutcome::Refused(
                full_validation_refusal.unwrap_or_else(|| "cell_results legacy-unbound".into()),
            ),
            crate::validation_evidence::EvidenceOutcome::Refused { reason }
            | crate::validation_evidence::EvidenceOutcome::Quarantined { reason, .. } => {
                BoundaryOutcome::Refused(format!("cell_results {reason}"))
            }
            crate::validation_evidence::EvidenceOutcome::ReaderFault { reason, .. } => {
                eprintln!("validation evidence: {reason}");
                BoundaryOutcome::Indeterminate("validation-evidence-artifacts-unreadable")
            }
        };
    }
    let Ok(report) = serde_json::from_slice::<SharedEvidenceReport>(&output.stdout) else {
        return BoundaryOutcome::Indeterminate("verifier-output-unparseable");
    };
    // SELF-CONSISTENCY FIRST, AND SEPARATELY FROM THE VERDICT. Each of these
    // compares the verifier's answer against its OWN other answer. A mismatch
    // says the authority is broken, which is not something the receipt can be
    // blamed for -- so it must not fall through into the `Refused` below.
    if !report.internally_consistent(output.status.code()) {
        return BoundaryOutcome::Indeterminate("verifier-self-inconsistent");
    }
    // Only here is the answer both coherent and negative, which is the one
    // reading that is a fact about the RECEIPT.
    if report.accepted {
        BoundaryOutcome::Accepted
    } else {
        BoundaryOutcome::Refused(report.evidence_reason)
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
///   * schema-4+ passing rows must carry `passed_tests` and it must equal
///     `executed_tests`; older rows retain the historical optional-field rule,
///     under which a present partial pass count is contradictory green evidence.
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

/// Additional optional admission proof on positive paths that do not delegate
/// to Python. Absence retains the old path; valid frozen evidence remains
/// exportable history but cannot supply canonical admission.
pub(crate) fn admission_floor_canonical_refusal(row: &HistoryRow) -> Option<&'static str> {
    match row.admission_evidence() {
        Err(_) => Some("admission admission-floor-invalid"),
        Ok(Some(evidence)) if evidence.floor().kind == "frozen-target" => {
            Some("admission admission-noncanonical")
        }
        Ok(_) => None,
    }
}

/// The typed form of [`row_qualifies`]. Same decisions, but the outcome carries
/// its condition so a caller can tell a demonstrated full green from one
/// grandfathered through the count-only fallback with no coverage evidence.
pub fn row_qualification(row: &HistoryRow, sha: &str, pred: &QualifyingPredicate) -> Qualification {
    let req = &pred.require;
    let commit = row.commit.as_deref().unwrap_or_default();
    if commit.is_empty() || commit == "unknown" {
        return Qualification::refused("no-commit");
    }
    if commit != sha {
        return Qualification::refused("commit");
    }
    if row.commit_anchored != Some(req.commit_anchored) {
        return Qualification::refused("commit_anchored");
    }
    if row.tree_dirty != Some(req.tree_dirty) {
        return Qualification::refused("tree_dirty");
    }
    if row.profile.as_deref() != Some(req.profile.as_str()) {
        return Qualification::refused("profile");
    }
    if row.selection_mode.as_deref() != Some(req.selection_mode.as_str()) {
        return Qualification::refused("selection_mode");
    }
    if release_builder_of(row, pred) != Some(req.release_builder.as_str()) {
        return Qualification::refused("release_builder");
    }
    if row.result.as_deref() != Some(req.result.as_str()) {
        return Qualification::refused("result");
    }
    // `pass` with a positive failure count is malformed; an absent count on a
    // pass row is treated as zero (old rows predate the field).
    if row.failures.unwrap_or(0) > req.failures_max {
        return Qualification::refused("failures");
    }
    // A demonstrated zero-test run is never a full green, at any schema.
    if row.executed_tests == Some(0) {
        return Qualification::refused("executed_tests == 0");
    }
    let schema = row.schema_version.unwrap_or(0);
    if !passed_tests_satisfy_schema(row, pred) {
        return Qualification::refused(PASSED_TESTS_REFUSAL_REASON);
    }
    if pred.gate_filtered_tests && row.filtered_tests != Some(0) {
        return Qualification::refused("filtered_tests != 0");
    }
    // Match Python's full-population refusal before checking the supported
    // schema window; unsupported incomplete rows still retain this first cause.
    if schema == 8 && row.cell_results_validate_path() == Some(ValidatePath::Full) {
        if let Some(evidence) = row.cell_results_evidence() {
            if evidence.selected_count == 0 || evidence.recorded_count != evidence.selected_count {
                return Qualification::refused("cell_results incomplete for full validation");
            }
        }
    }
    let count_capable = schema >= pred.counts_schema;
    if count_capable && !gate_accounting_complete(row) {
        return Qualification::refused("outer gate accounting incomplete");
    }
    let counts_present = row.executed_tests.is_some() && row.filtered_tests.is_some();
    let executed_ok = matches!(row.executed_tests, Some(n) if n >= req.executed_tests_min);
    let qualification = if count_capable {
        if !executed_ok {
            return Qualification::refused("executed_tests below executed_tests_min");
        }
        // Coverage is not required when the predicate turns it off or the row
        // predates it; otherwise it must be present AND satisfied. `None` here
        // is a writer defect, and is refused rather than skipped.
        if !pred.coverage.per_node || schema < pred.coverage.applies_at_schema_min {
            if let Some(reason) = admission_floor_canonical_refusal(row) {
                return Qualification::refused(reason);
            }
            return Qualification::FullCoverage;
        }
        match row.coverage.as_ref().map(coverage_verdict) {
            Some(CoverageVerdict::Satisfied) => Qualification::FullCoverage,
            Some(CoverageVerdict::Unsatisfied) => {
                Qualification::refused("count-capable receipt coverage unsatisfied")
            }
            Some(CoverageVerdict::Unavailable(_)) | None => {
                Qualification::refused("count-capable receipt coverage unavailable")
            }
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
            Qualification::refused("pre-coverage receipt below executed_tests_min")
        }
    } else {
        // Neither count present: an uncounted receipt is UNVERIFIED, not green.
        Qualification::refused("no counts: receipt proves nothing")
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
                return Qualification::refused("producer missing");
            }
            ProducerVerdict::UnknownWriter => {
                return Qualification::refused("producer unknown-writer");
            }
            ProducerVerdict::Ok | ProducerVerdict::Grandfathered => {}
        }
    }
    // Cross-field evidence semantics are one Python authority, not parallel
    // Rust/Python comparisons. This includes the schema/coverage boundary:
    // schema-4 stays grandfathered only with absent/null coverage, while a
    // non-null coverage claim requires schema-5+. Admission and base evidence
    // remain in the same delegated check.
    // Consume all three typed facets: an accepted verdict proceeds, a reader
    // fault stays indeterminate, and a coherent refusal keeps the authority's
    // exact negative status instead of collapsing it to a generic clause.
    let evidence = shared_evidence_outcome(row, pred);
    if !evidence.accepted() {
        // A READER fault, carried out in its own variant. The string names which
        // fault it was, so the operator learns whether to fix PATH, retry,
        // redeploy, or file a verifier bug -- and the TYPE says the receipt was
        // never judged, so no consumer can render this as a verdict against it.
        if let Some(reason) = evidence.indeterminate_reason() {
            return Qualification::Indeterminate(reason);
        }
        // The authority's exact negative status is a fact about the receipt.
        if let Some(reason) = evidence.refusal_reason() {
            return Qualification::Refused(reason.to_string());
        }
        unreachable!("every non-accepted boundary outcome carries a reason");
    }
    // Green-class gate (from origin/main): a receipt passing every value clause
    // must ALSO be of a green class the predicate accepts. Expressed as a
    // REFUSAL rather than a bare `false` so the reason survives into the typed
    // outcome; an underivable class is refused, never defaulted to accepted.
    let Some(green_class) = derived_green_class(row) else {
        return Qualification::refused("no derivable green class");
    };
    if !pred
        .accepts_green_class
        .iter()
        .any(|accepted| accepted == green_class)
    {
        return Qualification::refused("green class not accepted by predicate");
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
                "executed_tests":427,"passed_tests":427,"filtered_tests":0,"coverage":{coverage},
                "finished_at":"2026-08-08T09:00:00Z",
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
                "executed_tests":427,"passed_tests":427,"filtered_tests":0,"finished_at":"{finished_at}",
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

    fn schema8_empty_cell_results_row(sha: &str, path: &str) -> HistoryRow {
        let mut value = serde_json::to_value(row_with_producer(
            sha,
            "2026-08-08T09:00:00Z",
            Some("hermit-validate-sh"),
        ))
        .unwrap();
        value["schema_version"] = serde_json::json!(8);
        value["run_id"] = serde_json::json!("schema8-order");
        value["cell_results"] = serde_json::json!({
            "path": path,
            "run_id": "schema8-order",
            "hermit_sha": sha,
            "source_tree_dirty": false,
            "selected_count": 0,
            "recorded_count": 0,
            "population_sha256": "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945",
            "artifact": {
                "path": "ignored/validate/artifacts/schema8-order/cell-results.jsonl",
                "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "row_count": 0,
            },
            "selected": [],
            "cells": [],
        });
        serde_json::from_value(value).unwrap()
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

    fn refusal_reason(qualification: Qualification) -> String {
        match qualification {
            Qualification::Refused(reason) => reason.to_string(),
            other => panic!("expected a receipt refusal, got {other:?}"),
        }
    }

    #[test]
    fn each_identity_clause_names_its_own_cause() {
        let sha = "b".repeat(40);
        let other = "c".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let good = row_with_producer(&sha, "2026-08-08T09:00:00Z", Some("hermit-validate-sh"));
        assert!(
            row_qualifies(&good, &sha, &pred),
            "the fixture must otherwise qualify"
        );

        let refusal = |row: &HistoryRow| match row_qualification(row, &sha, &pred) {
            Qualification::Refused(reason) => reason,
            other => panic!("expected a refusal, got {other:?}"),
        };

        // Absent, empty and the literal "unknown" are all "no commit", and the
        // spelling matches the Python authority exactly so the two explanations
        // agree rather than merely coincide in meaning.
        for absent in [None, Some(String::new()), Some("unknown".to_string())] {
            let mut row = good.clone();
            row.commit = absent.clone();
            assert_eq!("no-commit", refusal(&row), "commit={absent:?}");
        }

        let mut wrong_commit = good.clone();
        wrong_commit.commit = Some(other.clone());
        assert_eq!("commit", refusal(&wrong_commit));

        let mut wrong_profile = good.clone();
        wrong_profile.profile = Some("not-the-required-profile".into());
        assert_eq!("profile", refusal(&wrong_profile));

        let mut wrong_selection = good.clone();
        wrong_selection.selection_mode = Some("not-the-required-selection".into());
        assert_eq!("selection_mode", refusal(&wrong_selection));

        let mut wrong_result = good.clone();
        wrong_result.result = Some("failure".into());
        assert_eq!("result", refusal(&wrong_result));

        // A Buck green executed the release payload. Only a row with neither
        // field or the exact Cargo builder/payload pair names Cargo; every
        // other pairing refuses on the same clause.
        let with_payload_fields = |case: &release_payload_cases::Case| {
            let mut row = good.clone();
            case.apply(&mut row);
            row
        };
        for case in release_payload_cases::all() {
            let row = with_payload_fields(&case);
            if case.expected() == Some("cargo") {
                assert!(row_qualifies(&row, &sha, &pred), "{} must qualify", case.label);
            } else {
                assert_eq!("release_builder", refusal(&row), "{}", case.label);
            }
        }
        let buck = with_payload_fields(&release_payload_cases::buck());

        // THE POINT OF THE TEST, not an afterthought: the five causes must be
        // DISTINGUISHABLE. Asserting each string individually would still pass
        // if every clause returned the same one.
        let causes = [
            refusal(&wrong_commit),
            refusal(&wrong_profile),
            refusal(&wrong_selection),
            refusal(&wrong_result),
            refusal(&buck),
            "no-commit".to_string(),
        ];
        let distinct: std::collections::BTreeSet<_> = causes.iter().collect();
        assert_eq!(
            6,
            distinct.len(),
            "each clause needs its own cause: {causes:?}"
        );
    }

    #[test]
    fn release_builder_of_reads_the_builder_and_payload_together() {
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let mut accepted = 0;
        let mut refused = 0;
        for case in release_payload_cases::all() {
            let mut row: HistoryRow = serde_json::from_str("{}").unwrap();
            case.apply(&mut row);
            assert_eq!(case.expected(), release_builder_of(&row, &pred), "{}", case.label);
            match case.expected() {
                Some(_) => accepted += 1,
                None => refused += 1,
            }
        }
        assert_eq!((7, 30), (accepted, refused), "the case table lost a case");
    }

    #[test]
    fn a_predicate_must_name_the_required_builders_payload() {
        let mut base: serde_json::Value = serde_json::from_str(EMBEDDED).unwrap();
        base.as_object_mut().unwrap().retain(|key, _| !key.starts_with('_'));
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove("release_payloads");
        let mut no_cargo = base.clone();
        no_cargo["release_payloads"].as_object_mut().unwrap().remove("cargo");
        let mut empty_identity = base.clone();
        empty_identity["release_payloads"]["cargo"] = serde_json::json!({});
        let mut scalar_identity = base.clone();
        scalar_identity["release_payloads"]["cargo"] = serde_json::json!("target/debug/hermit");
        assert!(QualifyingPredicate::parse(&base.to_string(), "base").is_ok());
        for (label, value) in [
            ("missing release_payloads", missing),
            ("no cargo identity", no_cargo),
            ("empty cargo identity", empty_identity),
            ("scalar cargo identity", scalar_identity),
        ] {
            assert!(
                QualifyingPredicate::parse(&value.to_string(), label).is_err(),
                "{label} must be refused"
            );
        }
    }

    #[test]
    fn a_predicate_must_date_the_legacy_cargo_cutoff() {
        let mut base: serde_json::Value = serde_json::from_str(EMBEDDED).unwrap();
        base.as_object_mut().unwrap().retain(|key, _| !key.starts_with('_'));
        let pred = QualifyingPredicate::parse(&base.to_string(), "base").unwrap();
        assert_eq!("2026-09-25T20:42:29Z", pred.release_builder_required_from_finished_at);
        let mut missing = base.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("release_builder_required_from_finished_at");
        let mut cases = vec![("missing cutoff", missing)];
        for (label, value) in [
            ("null cutoff", serde_json::Value::Null),
            ("integer cutoff", serde_json::json!(1_790_000_000)),
            ("empty cutoff", serde_json::json!("")),
            ("offset cutoff", serde_json::json!("2026-09-25T13:42:29-07:00")),
            ("fractional cutoff", serde_json::json!("2026-09-25T20:42:29.0Z")),
            ("impossible cutoff", serde_json::json!("2026-02-30T00:00:00Z")),
            ("leap-second cutoff", serde_json::json!("2026-09-25T20:42:60Z")),
            ("year-zero cutoff", serde_json::json!("0000-09-25T20:42:29Z")),
        ] {
            let mut bad = base.clone();
            bad["release_builder_required_from_finished_at"] = value;
            cases.push((label, bad));
        }
        for (label, value) in cases {
            assert!(
                QualifyingPredicate::parse(&value.to_string(), label).is_err(),
                "{label} must be refused"
            );
        }
    }

    #[test]
    fn optional_admission_is_checked_on_both_early_full_coverage_paths() {
        let sha = "b".repeat(40);
        let legacy = serde_json::to_value(row_with_producer(
            &sha,
            "2026-08-08T09:00:00Z",
            Some("hermit-validate-sh"),
        ))
        .unwrap();
        for coverage_disabled in [true, false] {
            let mut pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
            pred.coverage.per_node = !coverage_disabled;
            if !coverage_disabled {
                pred.coverage.applies_at_schema_min = 6;
            }
            let absent: HistoryRow = serde_json::from_value(legacy.clone()).unwrap();
            assert_eq!(
                row_qualification(&absent, &sha, &pred),
                Qualification::FullCoverage
            );
            let good = crate::tests::with_admission_context(legacy.clone(), false);
            let good_row: HistoryRow = serde_json::from_value(good.clone()).unwrap();
            assert_eq!(
                row_qualification(&good_row, &sha, &pred),
                Qualification::FullCoverage
            );
            for value in [
                serde_json::json!("other-run"),
                serde_json::Value::Null,
                serde_json::json!(42),
                serde_json::json!({}),
                serde_json::json!([]),
            ] {
                let mut bad = good.clone();
                bad["coverage"]["run_id"] = value;
                let bad: HistoryRow = serde_json::from_value(bad).unwrap();
                assert_eq!(
                    refusal_reason(row_qualification(&bad, &sha, &pred)),
                    "admission admission-floor-invalid"
                );
            }
            let frozen: HistoryRow =
                serde_json::from_value(crate::tests::with_admission_context(legacy.clone(), true))
                    .unwrap();
            assert_eq!(
                refusal_reason(row_qualification(&frozen, &sha, &pred)),
                "admission admission-noncanonical"
            );
            let mut dirty = good_row;
            dirty.tree_dirty = Some(true);
            dirty
                .extra
                .insert("admission_floor_evidence".into(), serde_json::Value::Null);
            assert_eq!(
                refusal_reason(row_qualification(&dirty, &sha, &pred)),
                "tree_dirty"
            );
        }
    }

    #[test]
    fn delegated_raw_nested_duplicates_preserve_admission_and_prior_refusals() {
        use crate::validation_evidence::fixtures::{Fixture, Store};

        const CHILD_STORE: &str = "CI_HUB_TEST_DUPLICATE_ADMISSION_STORE";
        let Some(store) = std::env::var_os(CHILD_STORE) else {
            let store = Store::new();
            // Select the actual store only in this child, without changing the
            // environment seen by other concurrently running libtest cases.
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "qualifying_receipt::tests::delegated_raw_nested_duplicates_preserve_admission_and_prior_refusals",
                    "--nocapture",
                ])
                .env(CHILD_STORE, &store.0)
                .env("DEV_HERMIT_PARENT", &store.0)
                .bounded_output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let sha = "b".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let retained = Fixture::new(
            "selected-attempt-v1/matched",
            "admission-duplicates",
            &sha,
            1,
        )
        .write(Path::new(&store));
        let retained = serde_json::to_value(retained).unwrap();
        let mut original = serde_json::to_value(row_with_producer(
            &sha,
            "2026-08-08T09:00:00Z",
            Some("hermit-validate-sh"),
        ))
        .unwrap();
        for key in [
            "schema_version",
            "run_id",
            "tree",
            "constructed_plan",
            "cell_results",
            "test_results",
            "executed_tests",
            "passed_tests",
            "filtered_tests",
        ] {
            original[key] = retained[key].clone();
        }
        original["coverage"]["planned_test_nodes"] = serde_json::json!(1);
        original["coverage"]["executed_test_nodes"] = serde_json::json!(1);
        let original = crate::tests::with_admission_context(original, false);
        // Acceptance requires both the real Python coverage/base/admission
        // boundary and verify_current_store's authenticated schema-10 files.
        let run_id = original["run_id"].as_str().unwrap().to_string();
        for field in ["coverage", "cell_results", "test_results"] {
            let mut good = original.clone();
            if !good[field].is_object() {
                good[field] = serde_json::json!({});
            }
            good[field]["run_id"] = serde_json::json!(run_id);
            let good_row: HistoryRow = serde_json::from_value(good.clone()).unwrap();
            assert!(shared_evidence_outcome(&good_row, &pred).accepted());
            for (first, second) in [
                ("wrong", run_id.as_str()),
                (run_id.as_str(), "wrong"),
                (run_id.as_str(), run_id.as_str()),
            ] {
                let mut raw_value = good.clone();
                raw_value[field]["run_id"] = serde_json::json!(first);
                let nested = serde_json::to_string(&raw_value[field]).unwrap();
                let duplicate = format!(
                    "{},\"run_id\":{}}}",
                    nested.strip_suffix('}').unwrap(),
                    serde_json::to_string(second).unwrap()
                );
                let raw = serde_json::to_string(&raw_value).unwrap();
                assert_eq!(raw.matches(&nested).count(), 1);
                let bad: HistoryRow =
                    serde_json::from_str(&raw.replace(&nested, &duplicate)).unwrap();
                assert_eq!(
                    shared_evidence_outcome(&bad, &pred).refusal_reason(),
                    Some("admission admission-floor-invalid")
                );
                let mut earlier = bad;
                earlier.extra.remove("base_sha");
                assert_eq!(
                    shared_evidence_outcome(&earlier, &pred).refusal_reason(),
                    Some("base base-sha-missing")
                );
            }
        }
    }

    #[test]
    fn delegated_admission_preserves_hidden_ids_and_existing_refusal_precedence() {
        let sha = "b".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let original = crate::tests::with_admission_context(
            serde_json::to_value(row_with_producer(
                &sha,
                "2026-08-08T09:00:00Z",
                Some("hermit-validate-sh"),
            ))
            .unwrap(),
            false,
        );
        let valid: HistoryRow = serde_json::from_value(original.clone()).unwrap();
        assert!(shared_evidence_outcome(&valid, &pred).accepted());
        let mut invalid = original.clone();
        invalid["coverage"]["run_id"] = serde_json::Value::Null;
        let invalid_row: HistoryRow = serde_json::from_value(invalid.clone()).unwrap();
        assert_eq!(
            shared_evidence_outcome(&invalid_row, &pred).refusal_reason(),
            Some("admission admission-floor-invalid")
        );
        for (field, value) in [
            ("schema_version", serde_json::json!(4)),
            ("base_sha", serde_json::Value::Null),
        ] {
            let mut earlier = original.clone();
            earlier[field] = value.clone();
            let earlier: HistoryRow = serde_json::from_value(earlier).unwrap();
            let expected = shared_evidence_outcome(&earlier, &pred);
            assert!(expected.refusal_reason().is_some());
            let mut both = invalid.clone();
            both[field] = value;
            let both: HistoryRow = serde_json::from_value(both).unwrap();
            assert_eq!(
                shared_evidence_outcome(&both, &pred).refusal_reason(),
                expected.refusal_reason()
            );
        }
        for (field, value, expected) in [
            ("tree_dirty", serde_json::json!(true), "tree_dirty"),
            ("result", serde_json::json!("fail"), "result"),
            (
                "executed_tests",
                serde_json::json!(0),
                "executed_tests == 0",
            ),
        ] {
            let mut both = invalid.clone();
            both[field] = value;
            let both: HistoryRow = serde_json::from_value(both).unwrap();
            assert_eq!(
                refusal_reason(row_qualification(&both, &sha, &pred)),
                expected
            );
        }
        let mut missing_cells = original.clone();
        missing_cells["schema_version"] = serde_json::json!(6);
        let missing_row: HistoryRow = serde_json::from_value(missing_cells.clone()).unwrap();
        let cell_refusal = shared_evidence_outcome(&missing_row, &pred);
        assert!(cell_refusal
            .refusal_reason()
            .is_some_and(|reason| reason.starts_with("cell_results ")));
        missing_cells["coverage"]["run_id"] = serde_json::Value::Null;
        let both: HistoryRow = serde_json::from_value(missing_cells).unwrap();
        assert_eq!(
            shared_evidence_outcome(&both, &pred).refusal_reason(),
            Some("admission admission-floor-invalid")
        );
        let mut unknown = invalid;
        unknown["producer"] = serde_json::json!("unknown-writer");
        let unknown: HistoryRow = serde_json::from_value(unknown).unwrap();
        assert_eq!(
            refusal_reason(row_qualification(&unknown, &sha, &enforcing())),
            "producer unknown-writer"
        );
    }

    #[test]
    fn canonical_refusal_reasons_survive_the_rust_boundary() {
        let sha = "b".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let control = row_with_producer(&sha, "2026-08-08T09:00:00Z", Some("hermit-validate-sh"));
        assert_eq!(
            row_qualification(&control, &sha, &pred),
            Qualification::FullCoverage
        );

        let mut dirty = control.clone();
        dirty.tree_dirty = Some(true);
        assert_eq!(
            refusal_reason(row_qualification(&dirty, &sha, &pred)),
            "tree_dirty"
        );

        let mut unanchored = control.clone();
        unanchored.commit_anchored = Some(false);
        assert_eq!(
            refusal_reason(row_qualification(&unanchored, &sha, &pred)),
            "commit_anchored"
        );

        let mut uncovered = control.clone();
        uncovered
            .coverage
            .as_mut()
            .expect("control carries coverage")
            .zero_executed_nodes = Some(vec!["test.detcore".into()]);
        assert_eq!(
            refusal_reason(row_qualification(&uncovered, &sha, &pred)),
            "count-capable receipt coverage unsatisfied"
        );

        let mut legacy_claims_coverage = control.clone();
        legacy_claims_coverage.schema_version = Some(4);
        assert_eq!(
            refusal_reason(row_qualification(&legacy_claims_coverage, &sha, &pred)),
            "coverage schema legacy-schema-claims-coverage"
        );

        let mut noncanonical_admission = control.clone();
        noncanonical_admission
            .extra
            .insert("admission".into(), serde_json::json!("direct-shell-launch"));
        assert_eq!(
            refusal_reason(row_qualification(&noncanonical_admission, &sha, &pred)),
            "admission admission-noncanonical"
        );

        let mut at_limit = control.clone();
        at_limit.concurrent_validates = Some(5);
        assert_eq!(
            row_qualification(&at_limit, &sha, &pred),
            Qualification::FullCoverage
        );

        let mut above_limit = control;
        above_limit.concurrent_validates = Some(6);
        assert_eq!(
            refusal_reason(row_qualification(&above_limit, &sha, &pred)),
            "admission concurrent-validates-invalid"
        );
    }

    #[test]
    fn schema8_full_population_refusal_precedes_policy_window() {
        let sha = "b".repeat(40);
        let mut pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        pred.cell_results.supported_schema_max = 7;
        let unsupported = schema8_empty_cell_results_row(&sha, "full");
        assert_eq!(
            refusal_reason(row_qualification(&unsupported, &sha, &pred)),
            "cell_results incomplete for full validation"
        );
        pred.cell_results.supported_schema_max = 8;

        let mismatched = schema8_empty_cell_results_row(&sha, "quick");
        assert_eq!(
            refusal_reason(row_qualification(&mismatched, &sha, &pred)),
            "cell_results path-mismatch"
        );

        let matching_path = schema8_empty_cell_results_row(&sha, "full");
        assert_eq!(
            refusal_reason(row_qualification(&matching_path, &sha, &pred)),
            "cell_results incomplete for full validation"
        );
    }

    #[test]
    fn schema8_quick_path_preserves_unsupported_schema_refusal() {
        let sha = "b".repeat(40);
        let mut pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        pred.cell_results.supported_schema_max = 7;
        let row = schema8_empty_cell_results_row(&sha, "quick");
        assert_eq!(
            refusal_reason(row_qualification(&row, &sha, &pred)),
            "cell_results schema-unavailable"
        );
        pred.cell_results.supported_schema_max = 8;
        assert_eq!(
            refusal_reason(row_qualification(&row, &sha, &pred)),
            "cell_results path-mismatch"
        );
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
            Qualification::refused("outer gate accounting incomplete")
        );
    }

    #[test]
    fn raw_current_schema_count_type_errors_use_shared_reasons() {
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        for schema in [4, 5] {
            let current = serde_json::json!({
                "schema_version": schema,
                "result": "pass",
                "executed_tests": 427,
                "passed_tests": 427,
                "filtered_tests": 0,
            });

            for field in ["executed_tests", "passed_tests"] {
                for malformed in [
                    serde_json::json!("427"),
                    serde_json::json!(true),
                    serde_json::json!(427.0),
                    serde_json::Value::from(1_u64 << 63),
                ] {
                    let mut row = current.clone();
                    row[field] = malformed;
                    assert_eq!(
                        raw_current_schema_count_type_refusal(&row, &pred),
                        Some(PASSED_TESTS_REFUSAL_REASON),
                        "schema-{schema} {field} type must have the shared refusal reason"
                    );

                    row["schema_version"] = serde_json::json!(3);
                    assert_eq!(
                        raw_current_schema_count_type_refusal(&row, &pred),
                        None,
                        "pre-4 {field} must retain its prior typed-reader behavior"
                    );
                }
            }

            for malformed in [
                serde_json::json!("0"),
                serde_json::json!(true),
                serde_json::json!(0.0),
                serde_json::Value::from(1_u64 << 63),
            ] {
                let mut row = current.clone();
                row["filtered_tests"] = malformed;
                assert_eq!(
                    raw_current_schema_count_type_refusal(&row, &pred),
                    Some(FILTERED_TESTS_I64_REFUSAL_REASON),
                    "schema-{schema} filtered_tests type must use its shared refusal reason"
                );

                row["schema_version"] = serde_json::json!(3);
                assert_eq!(
                    raw_current_schema_count_type_refusal(&row, &pred),
                    None,
                    "pre-4 filtered_tests must retain its prior typed-reader behavior"
                );
            }

            for (field, value) in [
                ("passed_tests", serde_json::Value::Null),
                ("passed_tests", serde_json::json!(426)),
                ("executed_tests", serde_json::Value::Null),
                ("filtered_tests", serde_json::Value::Null),
            ] {
                let mut row = current.clone();
                row[field] = value;
                assert_eq!(
                    raw_current_schema_count_type_refusal(&row, &pred),
                    None,
                    "{field} missing/null/mismatch belongs to the typed predicate path"
                );
            }
            let mut missing = current;
            missing.as_object_mut().unwrap().remove("passed_tests");
            assert_eq!(
                raw_current_schema_count_type_refusal(&missing, &pred),
                None,
                "missing passed_tests belongs to the typed predicate path"
            );
        }
    }

    #[test]
    fn schema4_and_newer_passing_receipts_require_exact_passed_count() {
        let sha = "b".repeat(40);
        let pred = QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap();
        let current = row_with_producer(&sha, "2026-08-08T09:00:00Z", Some("hermit-validate-sh"));

        for schema in [4, 5] {
            let mut exact = current.clone();
            exact.schema_version = Some(schema);
            if schema == 4 {
                exact.coverage = None;
            }
            assert!(row_qualifies(&exact, &sha, &pred));
            let mut maximum = exact.clone();
            maximum.executed_tests = Some(i64::MAX);
            maximum.passed_tests = Some(i64::MAX);
            maximum.filtered_tests = Some(i64::MAX);
            assert!(
                row_qualifies(&maximum, &sha, &pred),
                "schema-{schema} i64::MAX count fields must remain readable"
            );

            for label in ["missing", "null"] {
                let mut value = serde_json::to_value(&exact).unwrap();
                if label == "missing" {
                    value.as_object_mut().unwrap().remove("passed_tests");
                } else {
                    value["passed_tests"] = serde_json::Value::Null;
                }
                let row: HistoryRow = serde_json::from_value(value).unwrap();
                assert_eq!(
                    row_qualification(&row, &sha, &pred),
                    Qualification::refused(PASSED_TESTS_REFUSAL_REASON),
                    "schema-{schema} {label} passed_tests must refuse"
                );
            }

            for contradictory in [-1, 0, 426, 428] {
                let mut row = exact.clone();
                row.passed_tests = Some(contradictory);
                assert_eq!(
                    row_qualification(&row, &sha, &pred),
                    Qualification::refused(PASSED_TESTS_REFUSAL_REASON),
                    "schema-{schema} passed_tests={contradictory} must refuse"
                );
            }
        }

        let mut old_value = serde_json::to_value(&current).unwrap();
        old_value["schema_version"] = serde_json::json!(3);
        old_value.as_object_mut().unwrap().remove("coverage");
        let exact_old: HistoryRow = serde_json::from_value(old_value.clone()).unwrap();
        assert_eq!(
            row_qualification(&exact_old, &sha, &pred),
            Qualification::CountsOnlyGrandfathered { schema: 3 },
            "pre-4 exact passed_tests retains its qualification semantics"
        );
        for label in ["missing", "null"] {
            let mut value = old_value.clone();
            if label == "missing" {
                value.as_object_mut().unwrap().remove("passed_tests");
            } else {
                value["passed_tests"] = serde_json::Value::Null;
            }
            let row: HistoryRow = serde_json::from_value(value).unwrap();
            assert_eq!(
                row_qualification(&row, &sha, &pred),
                Qualification::CountsOnlyGrandfathered { schema: 3 },
                "pre-4 {label} passed_tests retains its qualification semantics"
            );
        }

        let mut old: HistoryRow = serde_json::from_value(old_value).unwrap();
        old.passed_tests = Some(426);
        assert_eq!(
            row_qualification(&old, &sha, &pred),
            Qualification::refused(PASSED_TESTS_REFUSAL_REASON),
            "pre-4 present contradictory evidence remains refused"
        );

        for schema in [4, 5] {
            for malformed in [
                serde_json::json!("427"),
                serde_json::json!(true),
                serde_json::json!(427.0),
            ] {
                let mut value = serde_json::to_value(&row_with_producer(
                    &sha,
                    "2026-08-08T09:00:00Z",
                    Some("hermit-validate-sh"),
                ))
                .unwrap();
                value["schema_version"] = serde_json::json!(schema);
                if schema == 4 {
                    value.as_object_mut().unwrap().remove("coverage");
                }
                value["passed_tests"] = malformed;
                assert!(
                    serde_json::from_value::<HistoryRow>(value).is_err(),
                    "the typed Rust row must reject malformed schema-{schema} passed_tests"
                );
            }
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

        // And the receipt verdict is the one thing that IS a Refused, carrying
        // the authority's exact status rather than a generic collapse.
        let refused = Qualification::refused("admission concurrent-validates-invalid");
        assert!(!refused.accepted());
        assert!(
            !matches!(refused, Qualification::Indeterminate(_)),
            "a genuine refusal must not masquerade as a reader fault either -- the \
             collapse has to be impossible in BOTH directions"
        );
    }

    #[test]
    fn shared_evidence_reason_must_match_the_first_negative_typed_status() {
        let mut report = SharedEvidenceReport {
            accepted: false,
            evidence_reason: "made-up refusal".into(),
            admission_satisfied: Some(false),
            admission_status: "concurrent-validates-invalid".into(),
            base_satisfied: Some(false),
            base_status: "base-sha-missing".into(),
            coverage_schema_satisfied: Some(true),
            coverage_schema_status: "satisfied".into(),
            cell_results_satisfied: Some(true),
            cell_results_status: "satisfied".into(),
        };
        assert_eq!(report.expected_reason(), "base base-sha-missing");
        assert!(
            !report.internally_consistent(Some(1)),
            "an arbitrary plausible refusal string must be treated as reader skew"
        );

        report.evidence_reason = report.expected_reason();
        assert!(report.internally_consistent(Some(1)));

        report.base_satisfied = Some(true);
        report.base_status = "satisfied".into();
        report.admission_satisfied = Some(true);
        report.admission_status = "satisfied".into();
        report.evidence_reason = "qualifies".into();
        assert!(
            !report.internally_consistent(Some(1)),
            "a refusal must carry at least one non-accepted typed status"
        );

        report.accepted = true;
        assert!(report.internally_consistent(Some(0)));
        report.evidence_reason = "satisfied".into();
        assert!(
            !report.internally_consistent(Some(0)),
            "accepted output must carry the canonical success reason"
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
        assert!(!Qualification::refused("anything").accepted());
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
        assert!(!BoundaryOutcome::Refused("receipt-refusal".into()).accepted());
        assert!(!BoundaryOutcome::Indeterminate("no python3").accepted());

        // Both non-accepting states BLOCK, and that is deliberate: an
        // unconsultable authority must never make a receipt more authoritative.
        // What must stay distinguishable is WHY.
        assert_eq!(
            BoundaryOutcome::Refused("receipt-refusal".into()).indeterminate_reason(),
            None
        );
        assert_eq!(
            BoundaryOutcome::Refused("receipt-refusal".into()).refusal_reason(),
            Some("receipt-refusal")
        );
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
                "executed_tests":427,"filtered_tests":0,"finished_at":"2026-08-08T09:00:00Z"}}"#
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
                "executed_tests":427,"passed_tests":427,"filtered_tests":0}}"#
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
        assert_eq!(pred.passed_tests.applies_at_schema_min, 4);
        assert_eq!(
            pred.count_representation.applies_at_schema_min,
            COUNT_REPRESENTATION_SCHEMA_MIN
        );
        assert_eq!(
            pred.count_representation
                .fields
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            COUNT_REPRESENTATION_FIELDS
        );
        assert_eq!(
            pred.count_representation.signed_bits,
            COUNT_REPRESENTATION_SIGNED_BITS
        );
        assert!(pred.require.executed_tests_min >= 1);
    }

    /// NEGATIVE: a predicate file that omits require.release_builder is
    /// refused rather than defaulted to cargo, so Rust is not the permissive
    /// half beside the Python loader.
    #[test]
    fn predicate_without_a_release_builder_is_refused() {
        let mut value: serde_json::Value = serde_json::from_str(EMBEDDED).unwrap();
        assert_eq!(value["require"]["release_builder"], "cargo");
        value["require"]
            .as_object_mut()
            .unwrap()
            .remove("release_builder");
        let error = QualifyingPredicate::parse(&value.to_string(), "no-builder").unwrap_err();
        assert!(error.contains("release_builder"), "{error}");
    }

    #[test]
    fn fixed_count_representation_rejects_contract_drift() {
        let expected_suffix = format!(
            "qualifying predicate count_representation must declare schema {}, fields {:?}, and {} signed bits",
            COUNT_REPRESENTATION_SCHEMA_MIN,
            COUNT_REPRESENTATION_FIELDS,
            COUNT_REPRESENTATION_SIGNED_BITS,
        );
        for (label, mutation) in [
            (
                "wrong width",
                serde_json::json!({
                    "applies_at_schema_min": 4,
                    "fields": ["executed_tests", "passed_tests", "filtered_tests"],
                    "signed_bits": 32,
                }),
            ),
            (
                "missing filtered field",
                serde_json::json!({
                    "applies_at_schema_min": 4,
                    "fields": ["executed_tests", "passed_tests"],
                    "signed_bits": 64,
                }),
            ),
            (
                "late application",
                serde_json::json!({
                    "applies_at_schema_min": 5,
                    "fields": ["executed_tests", "passed_tests", "filtered_tests"],
                    "signed_bits": 64,
                }),
            ),
        ] {
            let mut value: serde_json::Value = serde_json::from_str(EMBEDDED).unwrap();
            value["count_representation"] = mutation;
            let error = QualifyingPredicate::parse(&value.to_string(), label).unwrap_err();
            assert_eq!(
                error,
                format!("{label}: {expected_suffix}"),
                "{label} returned the wrong diagnostic"
            );
        }
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
                "executed_tests":740,"passed_tests":740,"filtered_tests":3,
                "finished_at":"2026-08-08T09:00:00Z",
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
            "passed_tests": 740,
            "filtered_tests": 3,
            "finished_at": "2026-08-08T09:00:00Z",
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

// Real implementation comparison. Shared-authority accounting is reported only
// AFTER every verdict has been compared; acceptance is never an exemption.
#[cfg(test)]
pub(crate) mod differential_tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub(crate) enum Answer {
        Accepted,
        Refused { cause: String },
        ReaderFault { cause: String },
    }

    #[derive(Clone, serde::Serialize)]
    pub(crate) struct Case {
        pub id: String,
        pub sha: String,
        // Retain raw JSON until each real reader decodes it. In particular,
        // do not round-trip a historical admission claim through Value.
        pub row: String,
    }

    pub(crate) struct Scratch(pub PathBuf);
    impl Scratch {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "qualifier-history-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).expect("exclusive test directory");
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove this test's own temporary directory");
        }
    }

    pub(crate) fn source_root() -> PathBuf {
        let root = resolve_root();
        assert_eq!(
            std::fs::read_to_string(root.join(PREDICATE_REL)).unwrap(),
            EMBEDDED,
            "Python's actual predicate must equal Rust's embedded predicate"
        );
        root
    }

    pub(crate) fn rust_answer(case: &Case) -> Answer {
        let row: HistoryRow = match serde_json::from_str(&case.row) {
            Ok(row) => row,
            Err(error) => {
                return Answer::ReaderFault {
                    cause: format!("HistoryRow: {error}"),
                }
            }
        };
        match row_qualification(
            &row,
            &case.sha,
            &QualifyingPredicate::parse(EMBEDDED, "embedded").unwrap(),
        ) {
            Qualification::FullCoverage => Answer::Accepted,
            Qualification::Refused(cause) => Answer::Refused { cause },
            Qualification::Indeterminate(cause) => Answer::ReaderFault {
                cause: cause.into(),
            },
            Qualification::CountsOnlyGrandfathered { schema } => Answer::ReaderFault {
                cause: format!("unmodeled counts-only acceptance at schema {schema}"),
            },
        }
    }

    const PYTHON: &str = r#"
import json, sys
sys.path.insert(0, sys.argv[1] + '/ci-hub')
import qualifying_receipt as qr
pred = qr.active()
with open(sys.argv[2]) as source:
    cases = json.load(source)
with open(sys.argv[3], 'x') as target:
    for case in cases:
        try:
            row = json.loads(case['row'], object_pairs_hook=qr.AdmissionJsonObject)
            accepted, reason = qr.row_qualification(row, case['sha'], pred)
            if accepted:
                if reason != 'qualifies':
                    raise ValueError('unmodeled accepted explanation: ' + repr(reason))
                answer = {'kind': 'accepted'}
            else:
                answer = {'kind': 'refused', 'cause': reason}
        except qr.ValidationEvidenceReaderFault as error:
            answer = {'kind': 'reader_fault', 'cause': str(error)}
        except Exception as error:
            answer = {'kind': 'unexpected_exception', 'cause': type(error).__name__ + ': ' + str(error)}
        target.write(json.dumps({'id': case['id'], 'answer': answer}) + '\n')
"#;

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        id: String,
        answer: Answer,
    }

    fn decode_replies(cases: &[Case], raw: &str) -> Result<Vec<Answer>, String> {
        let mut replies = BTreeMap::new();
        for line in raw.lines() {
            let reply: Reply =
                serde_json::from_str(line).map_err(|error| format!("Python reply: {error}"))?;
            if replies.insert(reply.id.clone(), reply.answer).is_some() {
                return Err(format!("duplicate Python reply {}", reply.id));
            }
        }
        if replies.len() != cases.len() {
            return Err(format!(
                "Python reply count {} != {}",
                replies.len(),
                cases.len()
            ));
        }
        let mut out = Vec::new();
        for case in cases {
            out.push(
                replies
                    .remove(&case.id)
                    .ok_or_else(|| format!("missing Python reply {}", case.id))?,
            );
        }
        if !replies.is_empty() {
            return Err("unexpected Python replies".into());
        }
        Ok(out)
    }

    pub(crate) fn python_answers(cases: &[Case]) -> Vec<Answer> {
        let root = source_root();
        let scratch = Scratch::new();
        let input = scratch.0.join("input.json");
        let output = scratch.0.join("output.jsonl");
        std::fs::write(&input, serde_json::to_vec(cases).unwrap()).unwrap();
        let result = Command::new("python3")
            .args(["-B", "-c", PYTHON])
            .arg(root)
            .arg(input)
            .arg(&output)
            .env_remove(PREDICATE_ENV)
            .output()
            .expect("run the real Python authority");
        assert!(
            result.status.success(),
            "Python driver refused: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        decode_replies(cases, &std::fs::read_to_string(output).unwrap()).unwrap()
    }

    // Exactly the observed cause/detail differences. Unknown explanations are
    // compared literally; a generic '=' splitter could erase a new cause.
    pub(crate) fn cause(reason: &str) -> String {
        for field in ["commit", "profile", "selection_mode", "result", "failures"] {
            if let Some(detail) = reason.strip_prefix(&format!("{field}=")) {
                let detail = if field == "selection_mode" {
                    detail.strip_suffix("(1-hop)").unwrap_or(detail)
                } else {
                    detail
                };
                if !detail.is_empty() {
                    return field.into();
                }
            }
        }
        match reason {
            "executed_tests == 0" => "executed_tests==0".into(),
            "filtered_tests != 0" => "filtered_tests!=0".into(),
            other => other.into(),
        }
    }

    pub(crate) fn compare(id: &str, rust: &Answer, python: &Answer) -> Result<(), String> {
        let agree = match (rust, python) {
            (Answer::Accepted, Answer::Accepted) => true,
            (Answer::Refused { cause: left }, Answer::Refused { cause: right }) => {
                cause(left) == cause(right)
            }
            // Reader failures are evidence loss, never a successful comparison
            // merely because both implementations could not judge the input.
            _ => false,
        };
        if agree {
            Ok(())
        } else {
            Err(format!("{id}: Rust {rust:?}; Python {python:?}"))
        }
    }

    #[test]
    fn comparison_catches_both_verdict_directions_causes_and_reader_faults() {
        let pass = Answer::Accepted;
        let profile = Answer::Refused {
            cause: "profile".into(),
        };
        let detail = Answer::Refused {
            cause: "profile='quick'".into(),
        };
        let result = Answer::Refused {
            cause: "result".into(),
        };
        let fault = Answer::ReaderFault {
            cause: "reader unavailable".into(),
        };
        assert!(compare("genuine-positive", &pass, &pass).is_ok());
        assert!(compare("known-detail", &profile, &detail).is_ok());
        for (left, right) in [
            (&pass, &profile),
            (&profile, &pass),
            (&profile, &result),
            (&fault, &profile),
            (&profile, &fault),
            (&fault, &fault),
        ] {
            let refusal = compare("named-counterexample", left, right).unwrap_err();
            assert!(
                refusal.contains("named-counterexample")
                    && refusal.contains("Rust")
                    && refusal.contains("Python")
            );
        }
        assert_ne!(cause("identity/result clause"), cause("no-commit"));
        assert_eq!(cause("profile='a=b'"), "profile");
        assert_eq!(cause("selection_mode='x'(1-hop)"), "selection_mode");
        assert_eq!(cause("executed_tests==0"), cause("executed_tests == 0"));
        assert_eq!(cause("filtered_tests!=0"), cause("filtered_tests != 0"));
        for unknown in [
            "new-cause=pass",
            "profile",
            "profile=",
            "unrelated(1-hop)",
            "executed_tests >= 0",
        ] {
            assert_eq!(cause(unknown), unknown);
        }
    }

    #[test]
    fn missing_duplicate_wrong_id_and_malformed_python_replies_refuse() {
        let cases = vec![Case {
            id: "case-one".into(),
            sha: "a".repeat(40),
            row: "{}".into(),
        }];
        let good = r#"{"id":"case-one","answer":{"kind":"accepted"}}"#;
        assert_eq!(
            decode_replies(&cases, good).unwrap(),
            vec![Answer::Accepted]
        );
        for bad in [
            String::new(),
            format!("{good}\n{good}"),
            good.replace("case-one", "different"),
            good.replace("accepted", "unexpected_exception"),
            "{".into(),
        ] {
            assert!(
                decode_replies(&cases, &bad).is_err(),
                "accepted malformed replies: {bad}"
            );
        }
    }

    #[test]
    fn generated_rows_compare_every_verdict_before_shared_authority_accounting() {
        let sha = "a".repeat(40);
        let values = generated_rows(&sha);
        let cases: Vec<Case> = values
            .iter()
            .enumerate()
            .map(|(index, (row, label))| Case {
                id: format!("generated-{index:05}: {label}"),
                sha: sha.clone(),
                row: serde_json::to_string(row).unwrap(),
            })
            .collect();
        assert_eq!(cases.len(), 32_805);
        let python = python_answers(&cases);
        let mut accepted = Vec::new();
        let mut causes = std::collections::BTreeSet::new();
        let mut errors = Vec::new();
        for (index, (case, theirs)) in cases.iter().zip(&python).enumerate() {
            let ours = rust_answer(case);
            // Crucially before classification; even a new false acceptance is
            // compared rather than silently moved out of the tested population.
            if let Err(error) = compare(&case.id, &ours, theirs) {
                errors.push(error);
            }
            match ours {
                Answer::Accepted => accepted.push(index),
                Answer::Refused { cause: reason } => {
                    causes.insert(cause(&reason));
                }
                Answer::ReaderFault { .. } => {}
            }
        }
        assert!(
            errors.is_empty(),
            "{} disagreements; first:\n{}",
            errors.len(),
            errors
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
        // These exact six inputs differ only in absent/zero failures and the
        // three filtered-count values; the shipped predicate does not gate
        // filtered_tests. All have executed_tests=passed_tests=427.
        assert_eq!(
            accepted,
            [0, 1, 2, 18, 19, 20],
            "the explicit shared-authority input set changed"
        );
        let expected: std::collections::BTreeSet<String> = [
            "no-commit",
            "commit",
            "commit_anchored",
            "tree_dirty",
            "profile",
            "selection_mode",
            "result",
            "failures",
            "executed_tests==0",
            PASSED_TESTS_REFUSAL_REASON,
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(
            causes, expected,
            "the measured independent cause population changed"
        );
        eprintln!("32805 complete outcome comparisons; 32799 independently refused inputs; six explicit accepted inputs {:?} traverse shared Python evidence; ten distinct refusal causes", accepted);
    }
    fn generated_rows(sha: &str) -> Vec<(serde_json::Value, String)> {
        let other = "c".repeat(40);
        let commits: Vec<Option<String>> = vec![
            Some(sha.to_string()),
            Some(other),
            None,
            Some(String::new()),
            Some("unknown".to_string()),
        ];
        let mut out = Vec::new();
        for commit in &commits {
            for anchored in [Some(true), Some(false), None] {
                for dirty in [Some(false), Some(true), None] {
                    for profile in [Some("full"), Some("quick"), None] {
                        for selection in [Some("full"), Some("selected"), None] {
                            for result in [Some("pass"), Some("failure"), None] {
                                for failures in [Some(0), Some(5), None] {
                                    for executed in [Some(427), Some(0), None] {
                                        for filtered in [Some(0), Some(3), None] {
                                            let mut row = serde_json::json!({
                                                "schema_version": 5,
                                                "passed_tests": 427,
                                                "finished_at": "2026-08-08T09:00:00Z",
                                                "producer": "hermit-validate-sh",
                                                "checks": 1,
                                                "gates_run": 1,
                                                "gates_expected": 1,
                                                "gates": [{"name":"test","result":"pass","exit_code":0}],
                                                "coverage": {"planned_test_nodes":19,"executed_test_nodes":19,
                                                             "zero_executed_nodes":[],"absent_nodes":[]},
                                                "admission": "ci-hub-validate-lock",
                                                "concurrent_validates": 0,
                                                "concurrency_proof": "validate_lock_owner_ancestry",
                                                "base_sha": "1".repeat(40),
                                                "base_tree": "2".repeat(40),
                                                "reverie_base_sha": "3".repeat(40),
                                                "reverie_base_tree": "4".repeat(40),
                                            });
                                            let map = row.as_object_mut().unwrap();
                                            if let Some(value) = commit {
                                                map.insert(
                                                    "commit".into(),
                                                    serde_json::json!(value),
                                                );
                                            }
                                            if let Some(value) = anchored {
                                                map.insert(
                                                    "commit_anchored".into(),
                                                    serde_json::json!(value),
                                                );
                                            }
                                            if let Some(value) = dirty {
                                                map.insert(
                                                    "tree_dirty".into(),
                                                    serde_json::json!(value),
                                                );
                                            }
                                            if let Some(value) = profile {
                                                map.insert(
                                                    "profile".into(),
                                                    serde_json::json!(value),
                                                );
                                            }
                                            if let Some(value) = selection {
                                                map.insert(
                                                    "selection_mode".into(),
                                                    serde_json::json!(value),
                                                );
                                            }
                                            if let Some(value) = result {
                                                map.insert(
                                                    "result".into(),
                                                    serde_json::json!(value),
                                                );
                                            }
                                            if let Some(value) = failures {
                                                map.insert(
                                                    "failures".into(),
                                                    serde_json::json!(value),
                                                );
                                            }
                                            if let Some(value) = executed {
                                                map.insert(
                                                    "executed_tests".into(),
                                                    serde_json::json!(value),
                                                );
                                            }
                                            if let Some(value) = filtered {
                                                map.insert(
                                                    "filtered_tests".into(),
                                                    serde_json::json!(value),
                                                );
                                            }
                                            let label = format!(
                                                "commit={commit:?} anchored={anchored:?} dirty={dirty:?} \
                                                 profile={profile:?} selection={selection:?} result={result:?} \
                                                 failures={failures:?} executed={executed:?} filtered={filtered:?}"
                                            );
                                            out.push((row, label));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    }

    pub(crate) fn compare_expected(
        id: &str,
        expected: &Answer,
        rust: &Answer,
        python: &Answer,
    ) -> Result<(), String> {
        if compare(id, rust, python).is_ok() && compare(id, expected, rust).is_ok() {
            Ok(())
        } else {
            Err(format!(
                "{id}: recorded expectation {expected:?}; Rust {rust:?}; Python {python:?}"
            ))
        }
    }
}
