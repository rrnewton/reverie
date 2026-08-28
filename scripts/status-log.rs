#!/usr/bin/env rust-script
//! Append one structured coordinator status update to the durable JSONL log.
//!
//! ```cargo
//! [dependencies]
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! ```

use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

const USAGE: &str = r#"Usage: scripts/status-log.rs [OPTIONS]

EVERY COUNT CARRIES ITS DENOMINATOR. Two adjacent entries once recorded
open_prs=105 and open_prs=10 nineteen minutes apart because one counted total
open across repositories and the other counted a ready/non-draft subset. The
same field name made a denominator change look like a 10x real drop. A count
whose counted set is not recorded beside it is not a measurement, so --repos is
required and `open_prs` has exactly one permitted meaning.

THE MAPPING IS AN ACTIVITY REPORT, NOT AN OWNERSHIP LEDGER. The 2026-08-07T01:00
entry listed 19 workstreams for 7 agents -- hermit-w3 seven times -- and not one
of the 19 was active work: 16 were tagged `implemented` (one already closed) and
3 were still OPEN/BACKLOG. Every task a name still owns is not every task it is
doing, so --mapping-json carries exactly ONE genuinely in-flight task per agent,
each task is dereferenced against live TaskGraph rather than believed, and work
that is implemented-awaiting-landing goes in --awaiting-landing-json where it
stays visible without being counted as activity.

Required:
  --mapping-json JSON    ACTIVE work only. Object mapping each workstream slug
                         to {"agent":"NAME","task":"TASK-ID","cwd":"PATH"}.
                         At most one entry per agent. Slugs are stable
                         descriptive major-goal/sub-goal, e.g.
                         backend-parity/dbi-stack-hash-determinism.
  --repos LIST           Comma-separated owner/name repositories that every
                         count below was taken over, e.g.
                         rrnewton/hermit,rrnewton/reverie. This is the
                         denominator; it is recorded with the counts.
  --open-prs N|auto      TOTAL open PRs INCLUDING DRAFTS across --repos. This
                         is the only permitted meaning of `open_prs`.
                         `auto` DEREFERENCES the declared repo set once and
                         adopts that total, recording the exact per-repo
                         breakdown and the window it was taken over. Prefer it:
                         measuring in the caller and re-checking in the writer
                         is a TOCTOU, and a real append failed when the
                         population moved 104 -> 105 between the two.
                         A NUMBER is still an assertion and is still CHECKED
                         against the population; only `auto` is adopted.
  --genuine-reds N       Current number of genuine-red PRs.
  --fleet-count N        Current active fleet size.
  --covers-hour HOUR     The UTC hour bucket YYYY-MM-DDTHH that the narrative and
                         the counts DESCRIBE. Required: it is not the write time,
                         and one recovered entry once carried 9 p.m. counts, a
                         10:28 p.m. mapping and a 10:28 p.m. timestamp with
                         nothing in the row to tell them apart. For an ordinary
                         report this is the hour you are reporting on; a write
                         from a LATER hour is retroactive and must also pass
                         --recovery-for.

Status input:
  --status-file PATH     Read the full status text from PATH.
                         When omitted, read the full text from stdin.
                         EITHER WAY at most one trailing line terminator is
                         stripped, so a heredoc, an editor and a pipe all yield
                         the SAME canonical bytes. A status logged from a file
                         once carried one extra 0x0A that the sent message did
                         not, which made a correct delivery hash as tampered.
  --expect-sha256 HEX    Assert the canonical status text hashes to HEX, which
                         is what the GChat API reported for the sent message.
                         A mismatch REFUSES the append instead of recording a
                         status that is not the one delivered.
  --expect-bytes N       Assert the canonical status text is N bytes, the byte
                         count the API reported. Refuses on mismatch.

Other:
  --awaiting-landing-json JSON
                         OPTIONAL, and a DISTINCT field. Same shape as
                         --mapping-json, for tasks tagged `implemented` that
                         are waiting to land. Recorded as
                         `awaiting_landing_to_worker`; never counted as active
                         work. Its tasks MUST be implemented-awaiting-landing,
                         which is the mirror image of the active check.
  --task-states-json PATH
                         Read task states from a JSON file instead of querying
                         TaskGraph: {"task-id":{"status":"IN_PROGRESS",
                         "tags":["implemented"]}}. For tests and offline use.
  --recovery-for HOUR    Mark this entry as the retroactive recovery of a missed
                         scheduled hour, naming that hour. Must equal
                         --covers-hour. Required when the write happens in a
                         later hour than the one covered, so a late entry
                         declares itself instead of being inferred by comparing
                         two timestamps.
  --ready-prs N          OPTIONAL, and a DISTINCT field. The ready/non-draft
                         subset. Recorded as `ready_prs`, never as `open_prs`.
  --open-prs-observed N  Supply the dereferenced open-PR total instead of
                         querying GitHub. The offline/test path, mirroring
                         --task-states-json. Still COMPARED against --open-prs;
                         it replaces the lookup, not the check.
  --no-verify-counts     Skip the count dereference entirely. Recorded in the
                         entry as open_prs_verified=false, because an unverified
                         count that does not say so is the defect this guard
                         exists to close.
  --open-prs-basis B     Assert the basis of --open-prs. The only accepted
                         value is total-open-including-drafts (the default).
                         A ready/non-draft basis is REFUSED here: pass it as
                         --ready-prs instead.
  --describe-log         Read the log and print one line per entry describing
                         each entry's count denominator, then exit. Entries
                         written before schema 2 are labelled
                         denominator=unknown. Use --log-file to select another
                         path. Read-only: never rewrites.
  --check-freshness      Compare this copy of the log against the landed one on
                         origin/main and exit. Nonzero when this copy is missing
                         landed rows, i.e. when publishing it would delete
                         delivered statuses. Writes nothing. Use as a
                         pre-commit/pre-publication guard.
  --print-reconciled     Write the union of the landed and local rows, in
                         timestamp order, to stdout and exit. Loses nothing and
                         duplicates nothing. Writes no files.
  --allow-stale-log      Append even though this copy is missing landed rows.
                         For an isolated copy that will never be published.
  --log-file PATH        Override the repository-relative log path.
                         Default: ai_docs/status-log/status-log.jsonl
  -h, --help             Show this help.

Example:
  printf 'Fleet healthy; 3 PRs open.\n' | scripts/status-log.rs \
    --mapping-json '{"pr-landing/open-pr-drain":{"agent":"hermit-coord",
      "task":"drain-open-prs-older-than-12h","cwd":"worktrees/lander/hermit"}}' \
    --repos rrnewton/hermit,rrnewton/reverie \
    --open-prs 3 --genuine-reds 0 --fleet-count 8
"#;

/// The ONE permitted meaning of `open_prs`.
const BASIS_TOTAL_OPEN: &str = "total-open-including-drafts";
/// The basis recorded for the distinct `ready_prs` field.
const BASIS_READY: &str = "ready-non-draft";
/// Entries from this schema on carry `count_semantics`; anything lower does not
/// and is therefore of unknown denominator.
const COUNT_SEMANTICS_SCHEMA: u64 = 2;
/// Schema 3 adds the per-worker `cwd` field, one-workstream-per-agent, and the
/// separate `awaiting_landing_to_worker` field.
/// Schema 4 adds the coverage fields below: `covers_hour`, `written_at`,
/// `mapping_observed_at`, and the optional `recovery_for`.
/// Schema 5 makes the complete row one producer-owned Rust type. New rows are
/// decoded by version before their shape is accepted, while schemas 1-4 remain
/// readable as the append-only historical formats they were written in.
const SCHEMA_VERSION: u64 = 5;

/// Entries from this schema on state WHICH HOUR they describe. Below it, the
/// only time in the record is the write time, so the covered hour is unknowable
/// from the row -- see `covers_hour` below for the row that proved it.
const COVERAGE_SCHEMA: u64 = 4;

/// The tag that marks a task complete-and-awaiting-landing. Such a task is a
/// LANDING obligation, not activity, and must never be reported as in-flight
/// work; that conflation is what turned one agent into seven bullets.
const IMPLEMENTED_TAG: &str = "implemented";

/// Values that look like an answer but assert nothing. `"none"` reached the log
/// as an agent name because the old check only required a non-empty string.
const PLACEHOLDER_VALUES: &[&str] = &[
    "",
    "-",
    "--",
    "?",
    "n/a",
    "na",
    "none",
    "null",
    "nil",
    "tbd",
    "todo",
    "unknown",
    "unassigned",
    "someone",
    "agent",
    "various",
    "multiple",
    "misc",
    "other",
    "pending",
];

/// Slug segments that number a thing instead of naming it. AGENTS.md bans these
/// outright: a workstream slug must survive the next update unchanged, and
/// `phase-1` cannot.
const ORDINAL_PREFIXES: &[&str] = &[
    "phase",
    "step",
    "round",
    "wave",
    "option",
    "batch",
    "part",
    "stage",
    "iteration",
    "attempt",
    "pass",
    "try",
    "run",
    "week",
    "day",
    "sprint",
    "milestone",
    "item",
    "task",
    "slot",
    "group",
];

#[derive(Default)]
struct Args {
    mapping_json: Option<String>,
    awaiting_landing_json: Option<String>,
    task_states_json: Option<PathBuf>,
    repos: Option<String>,
    open_prs: Option<u64>,
    open_prs_basis: Option<String>,
    /// `--open-prs auto`: adopt the dereferenced total instead of asserting one.
    open_prs_auto: bool,
    open_prs_observed: Option<u64>,
    no_verify_counts: bool,
    ready_prs: Option<u64>,
    genuine_reds: Option<u64>,
    fleet_count: Option<u64>,
    status_file: Option<PathBuf>,
    covers_hour: Option<String>,
    recovery_for: Option<String>,
    expect_sha256: Option<String>,
    expect_bytes: Option<usize>,
    log_file: Option<PathBuf>,
    describe_log: bool,
    allow_stale_log: bool,
    print_reconciled: bool,
    check_freshness: bool,
}

/// One validated workstream row: a slug, and the agent/task/workspace triple it
/// resolves to.
#[derive(Clone, Debug, PartialEq)]
struct Workstream {
    slug: String,
    agent: String,
    task: String,
    cwd: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StatusWorker {
    agent: String,
    task: String,
    cwd: String,
}

impl From<&Workstream> for StatusWorker {
    fn from(value: &Workstream) -> Self {
        Self {
            agent: value.agent.clone(),
            task: value.task.clone(),
            cwd: value.cwd.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ObservedWindow {
    start: String,
    end: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct OpenPrCountSemantics {
    basis: String,
    includes_drafts: bool,
    repos: Vec<String>,
    verified: bool,
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_window: Option<ObservedWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_per_repo: Option<BTreeMap<String, u64>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ReadyPrCountSemantics {
    basis: String,
    includes_drafts: bool,
    repos: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CountSemantics {
    open_prs: OpenPrCountSemantics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ready_prs: Option<ReadyPrCountSemantics>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LogFreshness {
    checked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    landed_rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    missing_landed_rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    overridden: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// The complete row written by `status-log.rs` at the current schema. The same
/// type is used before append, by `--describe-log`, and by reconciliation, so a
/// field cannot be added to the producer without making every current reader
/// account for it. Historical schemas remain on their original bytes.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StatusLogEntry {
    schema_version: u64,
    timestamp: String,
    written_at: String,
    covers_hour: String,
    mapping_observed_at: String,
    workstream_to_worker: BTreeMap<String, StatusWorker>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    awaiting_landing_to_worker: BTreeMap<String, StatusWorker>,
    open_prs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ready_prs: Option<u64>,
    genuine_reds: u64,
    fleet_count: u64,
    status_text: String,
    status_text_bytes: u64,
    status_text_sha256: String,
    count_semantics: CountSemantics,
    log_freshness: LogFreshness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery_for: Option<String>,
}

/// What TaskGraph actually says about a task, as opposed to what the caller
/// remembered about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskState {
    /// Non-terminal and NOT tagged `implemented`: genuinely being worked now.
    Active,
    /// Non-terminal but tagged `implemented`: finished, waiting to land.
    AwaitingLanding,
    /// Terminal.
    Closed,
    /// No such task in TaskGraph.
    Missing,
}

impl TaskState {
    fn describe(self) -> &'static str {
        match self {
            TaskState::Active => "active (in flight)",
            TaskState::AwaitingLanding => "IMPLEMENTED, awaiting landing -- not active work",
            TaskState::Closed => "CLOSED",
            TaskState::Missing => "NOT PRESENT in TaskGraph",
        }
    }
}

/// Classify a task from its live TaskGraph row. `implemented` wins over the
/// status because the status of implemented-awaiting-landing work is still
/// `in_progress` by policy -- which is exactly why reading status alone
/// produced seven simultaneous activities for an agent doing none.
fn classify_task(status: &str, tags: &[String]) -> TaskState {
    if tags
        .iter()
        .any(|t| t.trim().eq_ignore_ascii_case(IMPLEMENTED_TAG))
    {
        return TaskState::AwaitingLanding;
    }
    match status.trim().to_ascii_lowercase().as_str() {
        "closed" | "resolved" | "done" | "complete" | "completed" => TaskState::Closed,
        _ => TaskState::Active,
    }
}

fn is_placeholder(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    PLACEHOLDER_VALUES.contains(&normalized.as_str())
}

/// Normalize a slug or task id for comparison: case, and `_`/`/` both folded to
/// `-`, so `foo/bar_baz` and `Foo-Bar-Baz` are recognised as the same string.
fn normalize_identifier(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace(['_', '/', ' '], "-")
}

/// Is this segment a bare ordinal -- `phase-1`, `round-2`, `wave-x`, `v3`?
fn is_ordinal_segment(segment: &str) -> bool {
    if segment.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // `v3`, `v10`
    if let Some(rest) = segment.strip_prefix('v') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    for prefix in ORDINAL_PREFIXES {
        if let Some(rest) = segment.strip_prefix(prefix) {
            let rest = rest.trim_start_matches('-');
            // `phase`, `phase-1`, `phase-n`, `wave-x`, `option-a`
            if rest.is_empty()
                || rest.chars().all(|c| c.is_ascii_digit())
                || (rest.len() == 1 && rest.chars().all(|c| c.is_ascii_alphabetic()))
            {
                return true;
            }
        }
    }
    false
}

/// A workstream slug must be a STABLE DESCRIPTIVE `major-goal/sub-goal`: it
/// names the outcome, it reads the same next hour, and it tells the owner what
/// the work is without them having to look the task id up.
fn validate_slug(slug: &str, task: &str) -> Result<(), String> {
    if slug.trim().is_empty() {
        return Err("workstream slugs must not be empty".to_string());
    }
    if slug != slug.trim() {
        return Err(format!(
            "workstream slug {slug:?} has leading or trailing whitespace"
        ));
    }
    let segments: Vec<&str> = slug.split('/').collect();
    if segments.len() < 2 {
        return Err(format!(
            "workstream slug {slug:?} is flat; use a stable descriptive major-goal/sub-goal, \
             e.g. backend-parity/dbi-stack-hash-determinism. A bare id does not tell the owner \
             what the work is."
        ));
    }
    for segment in &segments {
        if segment.is_empty() {
            return Err(format!(
                "workstream slug {slug:?} has an empty path segment"
            ));
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!(
                "workstream slug segment {segment:?} in {slug:?} must be lowercase-hyphenated \
                 ([a-z0-9-])"
            ));
        }
        if segment.starts_with('-') || segment.ends_with('-') {
            return Err(format!(
                "workstream slug segment {segment:?} in {slug:?} must not start or end with '-'"
            ));
        }
        if is_placeholder(segment) {
            return Err(format!(
                "workstream slug segment {segment:?} in {slug:?} is a placeholder, not a name"
            ));
        }
        if is_ordinal_segment(segment) {
            return Err(format!(
                "workstream slug segment {segment:?} in {slug:?} is a bare ordinal/placeholder. \
                 Name the work or outcome (btrfs-flood-fix), never its position in a sequence; \
                 enumerate variants by suffix instead."
            ));
        }
        if segment.len() < 3 {
            return Err(format!(
                "workstream slug segment {segment:?} in {slug:?} is too short to be descriptive"
            ));
        }
    }
    if normalize_identifier(slug) == normalize_identifier(task) {
        return Err(format!(
            "workstream slug {slug:?} is just a restatement of task id {task:?}. The slug names the \
             workstream the owner cares about; the task id is recorded separately in the same row."
        ));
    }
    Ok(())
}

/// Validate one mapping row into a `Workstream`. Every field the owner needs to
/// act -- which agent, which task, which workspace -- is mandatory, because a
/// bullet missing any of them cannot be followed up on.
fn validate_workstream(slug: &str, worker: &Value) -> Result<Workstream, String> {
    let worker = worker
        .as_object()
        .ok_or_else(|| format!("mapping value for {slug:?} must be an object"))?;
    let mut fields = Vec::new();
    for field in ["agent", "task", "cwd"] {
        let value = worker
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!("mapping value for {slug:?} must contain a string {field:?} field")
            })?
            .trim();
        if value.is_empty() {
            return Err(format!("mapping {field:?} for {slug:?} must not be empty"));
        }
        if is_placeholder(value) {
            return Err(format!(
                "mapping {field:?} for {slug:?} is the placeholder {value:?}; a bullet that names \
                 no real {field} cannot be acted on"
            ));
        }
        fields.push(value.to_string());
    }
    let (agent, task, cwd) = (fields[0].clone(), fields[1].clone(), fields[2].clone());
    if !cwd.contains('/') {
        return Err(format!(
            "mapping \"cwd\" for {slug:?} is {cwd:?}, which is not a slot or working directory \
             path; give the agent's actual workspace, e.g. worktrees/slots/<slot>/hermit"
        ));
    }
    validate_slug(slug, &task)?;
    Ok(Workstream {
        slug: slug.to_string(),
        agent,
        task,
        cwd,
    })
}

/// Parse a mapping object into validated rows, ordered by slug for stable
/// messages.
fn parse_mapping(value: &Value) -> Result<Vec<Workstream>, String> {
    let mapping = value
        .as_object()
        .ok_or_else(|| "mapping must be a JSON object".to_string())?;
    let mut rows = Vec::new();
    for (slug, worker) in mapping {
        rows.push(validate_workstream(slug, worker)?);
    }
    rows.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(rows)
}

/// ONE WORKSTREAM PER AGENT. This is the rule the 01:00 report broke seven ways
/// at once: an agent is a worker, not a portfolio, so it gets exactly one
/// current bullet.
fn validate_one_workstream_per_agent(rows: &[Workstream]) -> Result<(), String> {
    let mut seen: Vec<(String, Vec<String>)> = Vec::new();
    for row in rows {
        match seen.iter_mut().find(|(agent, _)| agent == &row.agent) {
            Some((_, slugs)) => slugs.push(row.slug.clone()),
            None => seen.push((row.agent.clone(), vec![row.slug.clone()])),
        }
    }
    let mut offenders: Vec<String> = seen
        .into_iter()
        .filter(|(_, slugs)| slugs.len() > 1)
        .map(|(agent, slugs)| {
            format!(
                "{agent} claims {} workstreams: {}",
                slugs.len(),
                slugs.join(", ")
            )
        })
        .collect();
    if offenders.is_empty() {
        return Ok(());
    }
    offenders.sort();
    Err(format!(
        "an agent reports exactly ONE current workstream, but {}. Report the single task each \
         agent is working on right now; everything else it still owns is either awaiting landing \
         (--awaiting-landing-json) or not started, and neither is activity.",
        offenders.join("; ")
    ))
}

fn status_workers(rows: &[Workstream]) -> BTreeMap<String, StatusWorker> {
    rows.iter()
        .map(|row| (row.slug.clone(), StatusWorker::from(row)))
        .collect()
}

fn validate_status_workers(
    field: &str,
    rows: &BTreeMap<String, StatusWorker>,
) -> Result<Vec<Workstream>, String> {
    let mut workstreams = Vec::new();
    for (slug, worker) in rows {
        let value = serde_json::to_value(worker)
            .map_err(|error| format!("status-log-row-{field}: serialize worker: {error}"))?;
        workstreams.push(
            validate_workstream(slug, &value)
                .map_err(|error| format!("status-log-row-{field}: {error}"))?,
        );
    }
    validate_one_workstream_per_agent(&workstreams)
        .map_err(|error| format!("status-log-row-{field}: {error}"))?;
    Ok(workstreams)
}

impl LogFreshness {
    fn validate(&self) -> Result<(), String> {
        let checked_complete = self.checked
            && self.overridden.is_none()
            && self.reason.is_none()
            && self.landed_rows.is_some()
            && self.local_rows.is_some()
            && self.missing_landed_rows == Some(0);
        let checked_override = self.checked
            && self.overridden == Some(true)
            && self.reason.is_none()
            && self.landed_rows.is_none()
            && self.local_rows.is_none()
            && self.missing_landed_rows.is_some_and(|count| count > 0);
        let unchecked = !self.checked
            && self.overridden.is_none()
            && self.landed_rows.is_none()
            && self.local_rows.is_none()
            && self.missing_landed_rows.is_none()
            && self
                .reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty());
        if checked_complete || checked_override || unchecked {
            Ok(())
        } else {
            Err("status-log-row-log_freshness: fields do not describe checked, overridden, or unavailable freshness".into())
        }
    }
}

impl StatusLogEntry {
    fn from_value(value: Value) -> Result<Self, String> {
        let schema = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                "status-log-row-schema_version: missing or not an integer".to_string()
            })?;
        if schema != SCHEMA_VERSION {
            return Err(format!(
                "status-log-row-schema_version: expected {SCHEMA_VERSION}, got {schema}"
            ));
        }
        let entry: Self = serde_json::from_value(value)
            .map_err(|error| format!("status-log-row-field: {error}"))?;
        entry.validate()?;
        Ok(entry)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "status-log-row-schema_version: expected {SCHEMA_VERSION}, got {}",
                self.schema_version
            ));
        }
        if self.timestamp != self.written_at {
            return Err("status-log-row-written_at: must equal timestamp".into());
        }
        if self.mapping_observed_at != self.written_at {
            return Err("status-log-row-mapping_observed_at: must equal written_at".into());
        }
        if self.written_at.len() < 13 {
            return Err("status-log-row-written_at: must contain a UTC hour".into());
        }
        let write_hour = checked_hour_bucket("status-log-row-written_at", &self.written_at[..13])?;
        checked_hour_bucket("status-log-row-covers_hour", &self.covers_hour)?;
        match &self.recovery_for {
            Some(hour) if hour != &self.covers_hour => {
                return Err("status-log-row-recovery_for: must equal covers_hour".into())
            }
            None if self.covers_hour != write_hour => return Err(
                "status-log-row-recovery_for: required when covers_hour differs from written_at"
                    .into(),
            ),
            _ => {}
        }
        if self.status_text.is_empty() {
            return Err("status-log-row-status_text: must not be empty".into());
        }
        if self.status_text_bytes != self.status_text.len() as u64 {
            return Err("status-log-row-status_text_bytes: does not match status_text".into());
        }
        if self.status_text_sha256 != sha256_hex(self.status_text.as_bytes()) {
            return Err("status-log-row-status_text_sha256: does not match status_text".into());
        }

        let active = validate_status_workers("workstream_to_worker", &self.workstream_to_worker)?;
        let awaiting = validate_status_workers(
            "awaiting_landing_to_worker",
            &self.awaiting_landing_to_worker,
        )?;
        for row in &awaiting {
            if active.iter().any(|candidate| candidate.slug == row.slug) {
                return Err(format!(
                    "status-log-row-awaiting_landing_to_worker: workstream {:?} also appears in workstream_to_worker",
                    row.slug
                ));
            }
        }

        let open = &self.count_semantics.open_prs;
        if open.basis != BASIS_TOTAL_OPEN {
            return Err(format!(
                "status-log-row-count_semantics.open_prs.basis: expected {BASIS_TOTAL_OPEN}"
            ));
        }
        if !open.includes_drafts {
            return Err(
                "status-log-row-count_semantics.open_prs.includes_drafts: must be true".into(),
            );
        }
        if open.repos.is_empty() {
            return Err("status-log-row-count_semantics.open_prs.repos: must not be empty".into());
        }
        let mut unique_repos = std::collections::BTreeSet::new();
        for repo in &open.repos {
            let parsed = parse_repos(repo).map_err(|error| {
                format!("status-log-row-count_semantics.open_prs.repos: {error}")
            })?;
            if parsed.len() != 1 || parsed[0] != *repo {
                return Err(
                    "status-log-row-count_semantics.open_prs.repos: each array element must name exactly one owner/name repository"
                        .into(),
                );
            }
            if !unique_repos.insert(repo) {
                return Err(
                    "status-log-row-count_semantics.open_prs.repos: duplicate repository".into(),
                );
            }
        }
        let source_verified = match open.source.as_str() {
            "dereferenced-auto"
            | "declared-and-verified"
            | "declared-and-verified-supplied-observation" => true,
            "declared-unverified" => false,
            other => {
                return Err(format!(
                    "status-log-row-count_semantics.open_prs.source: unsupported value {other:?}"
                ))
            }
        };
        if open.verified != source_verified {
            return Err(
                "status-log-row-count_semantics.open_prs.verified: disagrees with source".into(),
            );
        }
        if let Some(per_repo) = &open.observed_per_repo {
            let expected = open
                .repos
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            let observed = per_repo
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            if observed != expected {
                return Err(
                    "status-log-row-count_semantics.open_prs.observed_per_repo: keys differ from repos"
                        .into(),
                );
            }
            if per_repo.values().sum::<u64>() != self.open_prs {
                return Err(
                    "status-log-row-open_prs: does not equal observed_per_repo total".into(),
                );
            }
        }
        if let Some(window) = &open.observed_window {
            if window.start.is_empty() || window.end.is_empty() {
                return Err(
                    "status-log-row-count_semantics.open_prs.observed_window: timestamps must not be empty"
                    .into(),
                );
            }
            if window.start > window.end {
                return Err(
                    "status-log-row-count_semantics.open_prs.observed_window: start is after end"
                        .into(),
                );
            }
        }
        let has_observation = open.observed_window.is_some() && open.observed_per_repo.is_some();
        let has_no_observation = open.observed_window.is_none() && open.observed_per_repo.is_none();
        match open.source.as_str() {
            "dereferenced-auto" | "declared-and-verified" if !has_observation => {
                return Err(
                    "status-log-row-count_semantics.open_prs.source: live observations require observed_window and observed_per_repo"
                        .into(),
                )
            }
            "declared-and-verified-supplied-observation" | "declared-unverified"
                if !has_no_observation =>
            {
                return Err(
                    "status-log-row-count_semantics.open_prs.source: supplied or unverified counts must not claim a live observation"
                        .into(),
                )
            }
            _ => {}
        }
        match (self.ready_prs, &self.count_semantics.ready_prs) {
            (Some(_), Some(ready))
                if ready.basis == BASIS_READY
                    && !ready.includes_drafts
                    && ready.repos == open.repos => {}
            (None, None) => {}
            _ => {
                return Err(
                    "status-log-row-ready_prs: value and count semantics must appear together and use the declared repository set"
                        .into(),
                )
            }
        }
        if self.ready_prs.is_some_and(|ready| ready > self.open_prs) {
            return Err("status-log-row-ready_prs: cannot exceed open_prs".into());
        }
        self.log_freshness.validate()
    }
}

/// Dereference every task against live TaskGraph state and refuse rows whose
/// real state contradicts the field they were placed in. `expect_active` picks
/// which side of the mirror we are checking.
fn validate_task_states(
    rows: &[Workstream],
    states: &BTreeMap<String, TaskState>,
    expect_active: bool,
    field: &str,
) -> Result<(), String> {
    let mut problems = Vec::new();
    for row in rows {
        let state = states.get(&row.task).copied().unwrap_or(TaskState::Missing);
        let ok = if expect_active {
            state == TaskState::Active
        } else {
            state == TaskState::AwaitingLanding
        };
        if !ok {
            problems.push(format!(
                "{} -> {} (agent {}) is {}",
                row.slug,
                row.task,
                row.agent,
                state.describe()
            ));
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    problems.sort();
    let remedy = if expect_active {
        "--mapping-json is ACTIVE work only. Move implemented-awaiting-landing tasks to \
         --awaiting-landing-json, drop closed and not-started ones, and report what the agent is \
         actually doing now."
    } else {
        "--awaiting-landing-json holds ONLY tasks tagged `implemented` that have not landed."
    };
    Err(format!(
        "{field} disagrees with live TaskGraph: {}. {remedy}",
        problems.join("; ")
    ))
}

/// Read task states from TaskGraph itself. A state copied from the caller would
/// be a cache of the authority, not the authority, and this whole defect is a
/// stale cache; so the states are dereferenced here on every run.
fn load_task_states_from_taskgraph(ids: &[String]) -> Result<BTreeMap<String, TaskState>, String> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let quoted: Vec<String> = ids
        .iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect();
    let query = format!(
        "SELECT local_id, status, tags FROM tasks WHERE local_id IN ({})",
        quoted.join(",")
    );
    let output = Command::new("tg")
        .args(["sql", &query])
        .output()
        .map_err(|e| format!("run `tg sql` to dereference task states: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`tg sql` failed while dereferencing task states: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(parse_tg_sql_rows(&text))
}

/// Parse `tg sql`'s pipe-separated table. Rows that do not parse are simply
/// absent, which classifies as `Missing` and therefore refuses -- the safe
/// direction.
fn parse_tg_sql_rows(text: &str) -> BTreeMap<String, TaskState> {
    let mut states = BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("local_id")
            || trimmed.starts_with('-')
            || trimmed.starts_with('(')
        {
            continue;
        }
        let parts: Vec<&str> = trimmed.split('|').map(str::trim).collect();
        if parts.len() < 3 {
            continue;
        }
        let tags: Vec<String> = serde_json::from_str::<Vec<String>>(parts[2]).unwrap_or_default();
        states.insert(parts[0].to_string(), classify_task(parts[1], &tags));
    }
    states
}

/// Read task states from a JSON fixture instead of TaskGraph, for tests and for
/// running without a TaskGraph on the box.
fn parse_task_states_json(text: &str) -> Result<BTreeMap<String, TaskState>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("parse task-states JSON: {e}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "task-states JSON must be an object keyed by task id".to_string())?;
    let mut states = BTreeMap::new();
    for (id, row) in object {
        let status = row.get("status").and_then(Value::as_str).unwrap_or("open");
        let tags: Vec<String> = row
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        states.insert(id.clone(), classify_task(status, &tags));
    }
    Ok(states)
}

/// Render the validated rows back to the JSON recorded in the log.
#[cfg(test)]
fn rows_to_json(rows: &[Workstream]) -> Value {
    let mut object = serde_json::Map::new();
    for row in rows {
        object.insert(
            row.slug.clone(),
            json!({"agent": row.agent, "task": row.task, "cwd": row.cwd}),
        );
    }
    Value::Object(object)
}

/// Parse and validate the repository scope. This is the denominator, so an
/// empty or malformed set is refused rather than recorded as a guess.
fn parse_repos(raw: &str) -> Result<Vec<String>, String> {
    let mut repos: Vec<String> = Vec::new();
    for part in raw.split(',') {
        let name = part.trim();
        if name.is_empty() {
            return Err("--repos contains an empty entry".to_string());
        }
        let mut halves = name.split('/');
        let owner = halves.next().unwrap_or_default();
        let repo = halves.next().unwrap_or_default();
        if owner.is_empty() || repo.is_empty() || halves.next().is_some() {
            return Err(format!(
                "--repos entry {name:?} must be exactly owner/name, e.g. rrnewton/hermit"
            ));
        }
        if !repos.iter().any(|existing| existing == name) {
            repos.push(name.to_string());
        }
    }
    if repos.is_empty() {
        return Err("--repos must name at least one owner/name repository".to_string());
    }
    Ok(repos)
}

/// Refuse a subset basis under `open_prs`. This is THE negative case: a
/// ready/non-draft count silently accepted here is exactly the defect that made
/// a denominator change look like a 10x drop.
/// DEREFERENCE THE COUNT, the way task ids are already dereferenced.
///
/// `--repos` was validated for SHAPE only and `--open-prs-basis` against an
/// allow-list, so nothing connected the NUMBER to the SET it claimed to have
/// counted. Entry 2026-08-07T02:55:39Z recorded `open_prs=7` under
/// `total-open-including-drafts` over [rrnewton/hermit, rrnewton/reverie]; the
/// true total across exactly those repositories was 102 when it was written and
/// 105 when this guard was added. The 7 was ci-hub's monitored-ROLLUP subset
/// (3 reds + 4 pending), a different population wearing the total's label.
///
/// That is the `open_prs=105` vs `open_prs=10` defect the denominator work was
/// built to prevent, one layer up: the earlier fix made the basis explicit, and
/// an explicit basis nobody checks is still just a label.
///
/// So when the basis claims the total, go and count it.
fn dereference_open_prs(repo: &str) -> Result<u64, String> {
    // `--limit` must exceed any plausible open-PR population: gh silently
    // truncates at the limit, and a truncated list would UNDERCOUNT and so
    // manufacture agreement with a subset -- the exact failure being closed.
    let output = Command::new("with-proxy")
        .args([
            "gh",
            "pr",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--limit",
            GH_PR_LIST_LIMIT_ARG,
            "--json",
            "number",
        ])
        .output()
        .map_err(|e| format!("run `gh pr list` to dereference {repo}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`gh pr list --repo {repo} --state open` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(text.trim())
        .map_err(|e| format!("parse `gh pr list --repo {repo}` output as JSON: {e}"))?;
    let array = parsed
        .as_array()
        .ok_or_else(|| format!("`gh pr list --repo {repo}` did not return a JSON array"))?;
    check_not_truncated(array.len(), GH_PR_LIST_LIMIT, repo)?;
    Ok(array.len() as u64)
}

/// Sum the open-PR population over exactly the declared repository set.
///
/// A multi-repository total CANNOT be atomic: each repository is a separate
/// query, so the sum spans a window rather than an instant. Pretending
/// otherwise is how a count acquires false precision, so the window is measured
/// and returned to be recorded with the value. Callers get ONE observation --
/// there is no second lookup to disagree with the first.
fn observed_open_prs(repos: &[String]) -> Result<Observation, String> {
    let started = timestamp_from_date();
    let mut per_repo = Vec::new();
    let mut total = 0u64;
    for repo in repos {
        let count = dereference_open_prs(repo)?;
        total += count;
        per_repo.push((repo.clone(), count));
    }
    Ok(Observation {
        total,
        per_repo,
        window_start: started,
        window_end: timestamp_from_date(),
    })
}

/// One dereference of the declared repository set, with the window it spans.
struct Observation {
    total: u64,
    per_repo: Vec<(String, u64)>,
    window_start: String,
    window_end: String,
}

/// Compare the declared count against the observed population. Pure so both
/// directions are testable without a network or a filesystem.
fn check_open_prs(
    declared: u64,
    observed: u64,
    basis: &str,
    repos: &[String],
    per_repo: Option<&[(String, u64)]>,
) -> Result<(), String> {
    if declared == observed {
        return Ok(());
    }
    let breakdown = per_repo
        .map(|rows| {
            rows.iter()
                .map(|(repo, n)| format!("{repo}={n}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| "supplied via --open-prs-observed".to_string());
    Err(format!(
        "--open-prs {declared} does not match the {basis} population over --repos {}: \
         observed {observed} ({breakdown}).\n\nA count that disagrees with the set it claims \
         to have counted is not a measurement of that set. If {declared} is a monitored \
         SUBSET -- ci-hub's rollup, for instance -- it needs its own field and basis the way \
         --ready-prs does; it may not borrow open_prs. Entry 2026-08-07T02:55:39Z recorded 7 \
         here while the true total was 102.",
        repos.join(",")
    ))
}

/// gh truncates SILENTLY at --limit, and a truncated list undercounts. An
/// undercount drifts toward a subset, i.e. it would manufacture agreement with
/// exactly the wrong number, so the bound refuses rather than trusting length.
const GH_PR_LIST_LIMIT: usize = 1000;
const GH_PR_LIST_LIMIT_ARG: &str = "1000";

fn check_not_truncated(returned: usize, limit: usize, repo: &str) -> Result<(), String> {
    if returned >= limit {
        return Err(format!(
            "`gh pr list --repo {repo}` returned {returned} entries, at or above the --limit \
             of {limit}; the list may be truncated and a truncated count would UNDERSTATE the \
             total"
        ));
    }
    Ok(())
}

/// `auto` and every way of supplying the number are mutually exclusive. Each
/// combination below would otherwise record a value whose provenance is a lie:
/// adopted-and-asserted, adopted-without-looking, or adopted-from-a-basis with
/// no population behind it.
fn validate_auto_mode(
    auto: bool,
    declared: Option<u64>,
    no_verify: bool,
    basis: &str,
    supplied_observation: Option<u64>,
) -> Result<(), String> {
    if !auto {
        return Ok(());
    }
    if declared.is_some() {
        return Err(
            "--open-prs auto adopts the dereferenced total; do not also assert a number".into(),
        );
    }
    if no_verify {
        return Err(
            "--open-prs auto IS the dereference; --no-verify-counts would leave nothing to adopt"
                .into(),
        );
    }
    if basis != BASIS_TOTAL_OPEN {
        return Err(format!(
            "--open-prs auto only defines a value for {BASIS_TOTAL_OPEN}; basis {basis} has no \
             population to dereference"
        ));
    }
    if supplied_observation.is_some() {
        return Err(
            "--open-prs auto performs the lookup; --open-prs-observed would replace it \
                    with a number nothing checked"
                .into(),
        );
    }
    Ok(())
}

fn validate_open_prs_basis(basis: &str) -> Result<(), String> {
    if basis == BASIS_TOTAL_OPEN {
        return Ok(());
    }
    let normalized = basis.trim().to_ascii_lowercase().replace('_', "-");
    let subset_like = [
        "ready",
        "ready-only",
        "ready-non-draft",
        "non-draft",
        "nondraft",
        "open-non-draft",
        "ready-prs",
        "excluding-drafts",
        "no-drafts",
    ];
    if subset_like.contains(&normalized.as_str()) {
        return Err(format!(
            "--open-prs-basis {basis:?} is a READY/NON-DRAFT SUBSET and is refused under \
             `open_prs`. `open_prs` means {BASIS_TOTAL_OPEN} and nothing else. Pass the subset \
             as --ready-prs, which is recorded in its own `ready_prs` field with basis \
             {BASIS_READY}."
        ));
    }
    Err(format!(
        "--open-prs-basis {basis:?} is not a recognised basis; the only accepted value is \
         {BASIS_TOTAL_OPEN}"
    ))
}

/// Describe one logged entry's count denominator. Entries written before
/// schema 2 carry no semantics, so they are labelled unknown rather than being
/// assumed to mean whatever the current field name means.
/// Which hour a row describes, for `--describe-log`. Rows below
/// `COVERAGE_SCHEMA` carry only a write time, so their covered hour is genuinely
/// unknown -- reported as such rather than guessed from `timestamp`, because
/// guessing is what filed a 9 p.m. report under 10 p.m. in the first place.
fn describe_coverage(entry: &Value) -> String {
    let schema = entry
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if schema < COVERAGE_SCHEMA {
        return format!("covers=unknown pre-schema-{COVERAGE_SCHEMA}-entry");
    }
    let covers = entry
        .get("covers_hour")
        .and_then(Value::as_str)
        .unwrap_or("<absent>");
    match entry.get("recovery_for").and_then(Value::as_str) {
        Some(hour) => format!("covers={covers} kind=recovery recovery_for={hour}"),
        None => format!("covers={covers} kind=current"),
    }
}

fn describe_entry(index: usize, entry: &Value) -> Result<String, String> {
    match entry.get("schema_version").and_then(Value::as_u64) {
        Some(SCHEMA_VERSION) => {
            let current = StatusLogEntry::from_value(entry.clone())?;
            return Ok(format!(
                "{index} {} open_prs={} basis={} repos={} {}",
                current.timestamp,
                current.open_prs,
                current.count_semantics.open_prs.basis,
                current.count_semantics.open_prs.repos.join("+"),
                match current.recovery_for {
                    Some(hour) => format!(
                        "covers={} kind=recovery recovery_for={hour}",
                        current.covers_hour
                    ),
                    None => format!("covers={} kind=current", current.covers_hour),
                }
            ));
        }
        Some(schema) if schema > SCHEMA_VERSION => {
            return Err(format!(
                "status-log-row-schema_version: unsupported newer schema {schema}; current reader supports through {SCHEMA_VERSION}"
            ));
        }
        _ => {}
    }
    let timestamp = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("<no-timestamp>");
    let open_prs = entry
        .get("open_prs")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "<absent>".to_string());
    let semantics = entry.get("count_semantics").and_then(|s| s.get("open_prs"));
    match semantics {
        Some(open) => {
            let basis = open
                .get("basis")
                .and_then(Value::as_str)
                .unwrap_or("<no-basis>");
            let repos = open
                .get("repos")
                .and_then(Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("+")
                })
                .unwrap_or_else(|| "<no-repos>".to_string());
            Ok(format!(
                "{index} {timestamp} open_prs={open_prs} basis={basis} repos={repos} {}",
                describe_coverage(entry)
            ))
        }
        None => Ok(format!(
            "{index} {timestamp} open_prs={open_prs} basis=unknown repos=unknown \
             denominator=unknown pre-schema-{COUNT_SEMANTICS_SCHEMA}-entry {}",
            describe_coverage(entry)
        )),
    }
}

// ---------------------------------------------------------------------------
// STATUS-LOG FRESHNESS
//
// The log is append-only and shared. A worktree whose copy is BEHIND the landed
// one is not merely out of date: committing it DELETES landed rows, because a
// checkout of a file is a whole-file replacement, not a merge. Measured on this
// repository 2026-08-07T03:10Z: the shared parent copy stood at +5/-51 against
// origin/main -- 53 local rows versus 99 landed -- so publishing it would have
// destroyed 51 delivered status records while adding 5.
//
// The predicate is IDENTITY-BASED, not base-SHA-based, and that distinction is
// load-bearing. "Is my HEAD behind origin/main?" is the wrong question: this
// parent checkout is routinely dozens of commits behind while the log file
// itself is identical, so a HEAD-based guard would refuse every legitimate
// append. The harmful condition is narrower and exactly checkable: DOES THE
// LANDED LOG CONTAIN A ROW MY COPY DOES NOT? If so, my copy can only reach
// origin/main by reverting that row.
// ---------------------------------------------------------------------------

/// Split a JSONL log into its non-empty rows. Row text is the identity: these
/// records are immutable once written, so byte equality is the right key and
/// needs no parsing (a row that fails to parse still compares).
fn log_rows(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Rows that are LANDED but absent locally -- i.e. exactly what publishing this
/// copy would delete. Order follows the landed file so the report reads
/// oldest-first.
fn missing_landed_rows(local: &str, landed: &str) -> Vec<String> {
    let mine: std::collections::HashSet<String> = log_rows(local).into_iter().collect();
    log_rows(landed)
        .into_iter()
        .filter(|row| !mine.contains(row))
        .collect()
}

/// Union of landed and local rows, in timestamp order, each row kept once.
/// This is the reconciliation an append-only record admits: rows have identity,
/// so merging is well defined and neither side loses anything.
fn status_log_row_timestamp(row: &str) -> Result<String, String> {
    let value: Value = match serde_json::from_str(row) {
        Ok(value) => value,
        Err(_) => return Ok(String::new()),
    };
    match value.get("schema_version").and_then(Value::as_u64) {
        Some(SCHEMA_VERSION) => return Ok(StatusLogEntry::from_value(value)?.timestamp),
        Some(schema) if schema > SCHEMA_VERSION => {
            return Err(format!(
                "status-log-row-schema_version: unsupported newer schema {schema}; current reader supports through {SCHEMA_VERSION}"
            ));
        }
        _ => {}
    }
    Ok(value
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

fn reconcile_rows(local: &str, landed: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in log_rows(landed).into_iter().chain(log_rows(local)) {
        if seen.insert(row.clone()) {
            out.push(row);
        }
    }
    let mut keyed = out
        .into_iter()
        .map(|row| status_log_row_timestamp(&row).map(|timestamp| (timestamp, row)))
        .collect::<Result<Vec<_>, _>>()?;
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(keyed.into_iter().map(|(_, row)| row).collect())
}

/// The landed copy of the log, read from the freshly-known remote ref. Returns
/// `None` when there is nothing to compare against (untracked log, no
/// origin/main, git unavailable) -- an absent comparison is reported, never
/// silently treated as a pass.
fn landed_log(root: &Path, log_path: &Path) -> Option<String> {
    let relative = log_path.strip_prefix(root).ok()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("show")
        .arg(format!("origin/main:{}", relative.display()))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Refuse an append onto a copy that would revert landed rows.
fn check_log_freshness(local: &str, landed: &str) -> Result<usize, String> {
    let missing = missing_landed_rows(local, landed);
    if missing.is_empty() {
        return Ok(log_rows(landed).len());
    }
    let stamp = |row: &String| -> String {
        serde_json::from_str::<Value>(row)
            .ok()
            .and_then(|v| {
                v.get("timestamp")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "<unparseable row>".to_string())
    };
    let first = missing.first().map(stamp).unwrap_or_default();
    let last = missing.last().map(stamp).unwrap_or_default();
    Err(format!(
        "STALE STATUS LOG: this copy is missing {} row(s) that are already landed on origin/main \
         (oldest {first}, newest {last}). Appending here deepens a divergence that can only be \
         resolved by reverting those rows, which is data loss -- the log is append-only and every \
         row is a delivered status. Reconcile first: `--print-reconciled` writes the union of the \
         landed and local rows, in timestamp order, losing and duplicating nothing. \
         `--allow-stale-log` overrides this for an isolated copy that will never be published.",
        missing.len()
    ))
}

fn die(message: &str) -> ! {
    eprintln!("status-log: {message}");
    exit(2);
}

fn take_value<I>(flag: &str, args: &mut I) -> String
where
    I: Iterator<Item = String>,
{
    args.next()
        .unwrap_or_else(|| die(&format!("{flag} requires a value")))
}

fn parse_count(flag: &str, value: String) -> u64 {
    value
        .parse::<u64>()
        .unwrap_or_else(|_| die(&format!("{flag} must be a non-negative integer")))
}

fn parse_args() -> Args {
    let mut parsed = Args::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mapping-json" => parsed.mapping_json = Some(take_value(&arg, &mut args)),
            "--awaiting-landing-json" => {
                parsed.awaiting_landing_json = Some(take_value(&arg, &mut args))
            }
            "--task-states-json" => {
                parsed.task_states_json = Some(take_value(&arg, &mut args).into())
            }
            "--repos" => parsed.repos = Some(take_value(&arg, &mut args)),
            "--open-prs" => {
                let raw = take_value(&arg, &mut args);
                // `auto` ADOPTS the dereferenced total. Anything else is an
                // assertion the caller must stand behind, and is still checked.
                if raw.eq_ignore_ascii_case("auto") {
                    parsed.open_prs_auto = true;
                } else {
                    parsed.open_prs = Some(parse_count(&arg, raw));
                }
            }
            "--open-prs-basis" => parsed.open_prs_basis = Some(take_value(&arg, &mut args)),
            "--open-prs-observed" => {
                parsed.open_prs_observed = Some(parse_count(&arg, take_value(&arg, &mut args)))
            }
            "--no-verify-counts" => parsed.no_verify_counts = true,
            "--ready-prs" => {
                parsed.ready_prs = Some(parse_count(&arg, take_value(&arg, &mut args)))
            }
            "--describe-log" => parsed.describe_log = true,
            "--genuine-reds" => {
                parsed.genuine_reds = Some(parse_count(&arg, take_value(&arg, &mut args)))
            }
            "--fleet-count" => {
                parsed.fleet_count = Some(parse_count(&arg, take_value(&arg, &mut args)))
            }
            "--status-file" => parsed.status_file = Some(take_value(&arg, &mut args).into()),
            "--covers-hour" => {
                let raw = take_value(&arg, &mut args);
                parsed.covers_hour = Some(parse_hour_bucket("--covers-hour", &raw));
            }
            "--recovery-for" => {
                let raw = take_value(&arg, &mut args);
                parsed.recovery_for = Some(parse_hour_bucket("--recovery-for", &raw));
            }
            "--expect-sha256" => parsed.expect_sha256 = Some(take_value(&arg, &mut args)),
            "--expect-bytes" => {
                let raw = take_value(&arg, &mut args);
                parsed.expect_bytes = Some(
                    raw.parse()
                        .unwrap_or_else(|e| die(&format!("--expect-bytes {raw}: {e}"))),
                );
            }
            "--log-file" => parsed.log_file = Some(take_value(&arg, &mut args).into()),
            "--allow-stale-log" => parsed.allow_stale_log = true,
            "--print-reconciled" => parsed.print_reconciled = true,
            "--check-freshness" => parsed.check_freshness = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                exit(0);
            }
            _ => die(&format!("unknown argument: {arg}\n\n{USAGE}")),
        }
    }

    parsed
}

fn repository_root() -> PathBuf {
    let mut path = env::current_dir().unwrap_or_else(|e| die(&format!("current directory: {e}")));
    loop {
        if path.join(".gitmodules").is_file() && path.join("AGENTS.md").is_file() {
            return path;
        }
        if !path.pop() {
            die("could not locate the dev-hermit repository root");
        }
    }
}

/// WHICH HOUR DOES THIS ROW DESCRIBE?
///
/// Measured ambiguity, entry `2026-08-07T02:28:13Z`: its narrative opens
/// "## 9 p.m. hourly status — recovered delivery", its counts are the 9 p.m.
/// (T01) counts, its `workstream_to_worker` holds the 15 workstreams that were
/// live at 02:28, and its only timestamp is the 02:28 WRITE time. One row, three
/// time bases, and nothing in the schema said so. A reader bucketing by
/// `timestamp` files a 9 p.m. report under 10 p.m.; a reader trusting the
/// mapping believes those 15 workstreams were live at 9 p.m. Neither is
/// recoverable from the row.
///
/// So schema 4 states all three explicitly:
///   `covers_hour`         the UTC hour the narrative and counts describe.
///   `written_at`          when the row was appended.
///   `mapping_observed_at` when the mapping was sampled. Always the write time:
///                         a past mapping cannot be reconstructed, and pretending
///                         otherwise is the exact conflation above.
///   `recovery_for`        present only on a retroactive entry, naming the hour
///                         whose scheduled delivery was missed.
fn checked_hour_bucket(flag: &str, value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let shaped = bytes.len() == 13
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
        && bytes[10] == b'T'
        && bytes[11..13].iter().all(u8::is_ascii_digit);
    if !shaped {
        return Err(format!(
            "{flag} must be a UTC hour bucket YYYY-MM-DDTHH, e.g. 2026-08-07T01; got {value:?}"
        ));
    }
    let month: u32 = value[5..7].parse().unwrap_or(0);
    let day: u32 = value[8..10].parse().unwrap_or(0);
    let hour: u32 = value[11..13].parse().unwrap_or(99);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 {
        return Err(format!("{flag}: {value:?} is not a real UTC hour"));
    }
    Ok(value.to_string())
}

fn parse_hour_bucket(flag: &str, value: &str) -> String {
    checked_hour_bucket(flag, value).unwrap_or_else(|error| die(&error))
}

/// The hour bucket of an ISO-8601 `YYYY-MM-DDTHH:MM:SSZ` write timestamp.
fn hour_of_timestamp(timestamp: &str) -> String {
    if timestamp.len() < 13 {
        die(&format!("unparseable write timestamp {timestamp:?}"));
    }
    timestamp[..13].to_string()
}

fn timestamp_from_date() -> String {
    let output = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap_or_else(|e| die(&format!("run date: {e}")));
    if !output.status.success() {
        die("date failed while generating the timestamp");
    }
    String::from_utf8(output.stdout)
        .unwrap_or_else(|e| die(&format!("date returned non-UTF-8 output: {e}")))
        .trim_end()
        .to_owned()
}

/// Parse one mapping flag into validated rows, applying the structural rules
/// that do not need TaskGraph.
fn parse_mapping_flag(flag: &str, raw: &str) -> Vec<Workstream> {
    let value: Value =
        serde_json::from_str(raw).unwrap_or_else(|e| die(&format!("parse {flag}: {e}")));
    let rows = parse_mapping(&value).unwrap_or_else(|e| die(&format!("{flag}: {e}")));
    validate_one_workstream_per_agent(&rows).unwrap_or_else(|e| die(&format!("{flag}: {e}")));
    rows
}

/// THE CANONICAL STATUS TEXT: exactly one byte sequence, whatever transport
/// carried it.
///
/// Measured defect, 2026-08-07T02. The GChat API reported the delivered text as
/// 1843 bytes / sha256 `fd702682…`; the JSONL entry for the same status recorded
/// 1844 bytes / `1ccab376…`. The difference was exactly one appended `0x0A` in
/// the 1843-byte common prefix — zero differing bytes otherwise. The extra byte
/// came from the `--status-file` round trip, because every ordinary way of
/// producing a text file (an editor, a heredoc, `echo`, a shell redirect)
/// terminates the last line. The sent message had no such byte.
///
/// That one byte is not cosmetic. The claim's digest then hashes the LOGGED text
/// while `gchat_message_name` dereferences the SENT text, so an auditor who
/// fetches the message and hashes what comes back gets a MISMATCH on a correct
/// delivery — the record accuses itself of tampering.
///
/// It was also inconsistent, which is worse than uniformly wrong: entries at
/// 01:00:36Z and 02:28:13Z (stdin) carried no trailing newline while 01:19:42Z
/// and 02:14:31Z (file) did, so "the canonical bytes" depended on how the caller
/// happened to pass the text.
///
/// So: strip AT MOST ONE trailing line terminator, identically for both
/// transports. At most one, never `trim_end`, because trailing blank lines
/// inside a status are author content and deleting them would silently rewrite
/// the message. `hourly-status-relay.rs` applies the same rule to
/// `--ack-text-file`, so the log digest and the claim digest agree by
/// construction rather than by luck.
// ------------------------------------------------------------- sha256 -----
// Same in-file implementation as scripts/hourly-status-relay.rs, deliberately.
// These two tools must produce the SAME digest for the same canonical text --
// that identity is the whole point of this file's canonical_status_text -- so
// they must not be able to drift onto different algorithms or crate versions.
// FIPS 180-4; bracketed against the published vectors below.
fn sha256_hex(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = bytes.to_vec();
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(v);
        }
    }
    h.iter().map(|w| format!("{w:08x}")).collect()
}

fn canonical_status_text(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

fn read_status_text(status_file: Option<&Path>) -> String {
    let mut text = String::new();
    match status_file {
        Some(path) => {
            text = fs::read_to_string(path)
                .unwrap_or_else(|e| die(&format!("read status file {}: {e}", path.display())))
        }
        None => {
            io::stdin()
                .read_to_string(&mut text)
                .unwrap_or_else(|e| die(&format!("read status text from stdin: {e}")));
        }
    }
    let text = canonical_status_text(text);
    if text.is_empty() {
        die("status text must not be empty");
    }
    text
}

fn main() {
    let args = parse_args();

    let root = repository_root();
    let log_path = match &args.log_file {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => root.join(path),
        None => root.join("ai_docs/status-log/status-log.jsonl"),
    };

    // Read-only freshness modes. Neither writes anything.
    if args.check_freshness || args.print_reconciled {
        let local = fs::read_to_string(&log_path).unwrap_or_default();
        let landed = landed_log(&root, &log_path)
            .unwrap_or_else(|| die("no origin/main copy of the log to compare against"));
        if args.print_reconciled {
            for row in reconcile_rows(&local, &landed).unwrap_or_else(|error| die(&error)) {
                println!("{row}");
            }
            exit(0);
        }
        match check_log_freshness(&local, &landed) {
            Ok(landed_rows) => {
                println!(
                    "status log is publishable: {} local row(s), {} landed row(s), 0 landed rows missing",
                    log_rows(&local).len(),
                    landed_rows
                );
                exit(0);
            }
            Err(message) => die(&message),
        }
    }

    // Read-only. Never rewrites history; old rows are labelled, not migrated.
    if args.describe_log {
        let text = fs::read_to_string(&log_path)
            .unwrap_or_else(|e| die(&format!("read log {}: {e}", log_path.display())));
        for (index, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
            match serde_json::from_str::<Value>(line) {
                Ok(entry) => println!(
                    "{}",
                    describe_entry(index, &entry).unwrap_or_else(|error| die(&error))
                ),
                Err(e) => println!("{index} <unparseable entry> denominator=unknown error={e}"),
            }
        }
        exit(0);
    }

    let mapping_json = args
        .mapping_json
        .as_deref()
        .unwrap_or_else(|| die("missing --mapping-json"));
    let active = parse_mapping_flag("--mapping-json", mapping_json);
    let awaiting = args
        .awaiting_landing_json
        .as_deref()
        .map(|raw| parse_mapping_flag("--awaiting-landing-json", raw))
        .unwrap_or_default();

    // A slug names one workstream; the same slug on both sides would say the
    // work is simultaneously in flight and finished.
    for row in &awaiting {
        if active.iter().any(|a| a.slug == row.slug) {
            die(&format!(
                "workstream {:?} appears in BOTH --mapping-json and --awaiting-landing-json; \
                 it is either in flight or waiting to land, not both",
                row.slug
            ));
        }
    }

    // Dereference the authority. A task state copied from the caller is a cache,
    // and a stale cache is exactly the defect this guard exists for.
    let mut ids: Vec<String> = active
        .iter()
        .chain(awaiting.iter())
        .map(|r| r.task.clone())
        .collect();
    ids.sort();
    ids.dedup();
    let states = match &args.task_states_json {
        Some(path) => {
            let text = fs::read_to_string(path)
                .unwrap_or_else(|e| die(&format!("read task states {}: {e}", path.display())));
            parse_task_states_json(&text).unwrap_or_else(|e| die(&e))
        }
        // Fails closed: no verification means no entry, rather than an entry
        // that silently asserts states nobody checked.
        None => load_task_states_from_taskgraph(&ids).unwrap_or_else(|e| {
            die(&format!(
                "{e}\n\nTask states could not be dereferenced, so the mapping cannot be \
                 verified. Pass --task-states-json PATH to supply them explicitly."
            ))
        }),
    };
    validate_task_states(&active, &states, true, "--mapping-json").unwrap_or_else(|e| die(&e));
    validate_task_states(&awaiting, &states, false, "--awaiting-landing-json")
        .unwrap_or_else(|e| die(&e));

    let repos_raw = args.repos.as_deref().unwrap_or_else(|| {
        die("missing --repos: a count without its counted set is not a measurement")
    });
    let repos = parse_repos(repos_raw).unwrap_or_else(|e| die(&e));
    validate_open_prs_basis(args.open_prs_basis.as_deref().unwrap_or(BASIS_TOTAL_OPEN))
        .unwrap_or_else(|e| die(&e));

    let basis = args.open_prs_basis.as_deref().unwrap_or(BASIS_TOTAL_OPEN);
    validate_auto_mode(
        args.open_prs_auto,
        args.open_prs,
        args.no_verify_counts,
        basis,
        args.open_prs_observed,
    )
    .unwrap_or_else(|e| die(&e));

    // ---- ONE observation, adopted or checked, never re-derived ----
    //
    // The caller-measures-then-writer-remeasures shape has a TOCTOU: a real
    // append failed because the population moved 104 -> 105 between the
    // caller's `gh pr list` and the writer's. Both numbers were honest; the
    // window between them was the defect. `auto` closes it by making the
    // writer's dereference the ONLY measurement, so there is no second reading
    // to disagree with the first.
    //
    // An explicitly supplied count is still CHECKED, not adopted: a caller who
    // states a number is making a claim, and a claim that disagrees with the
    // population is exactly the subset-wearing-the-total defect this guard
    // exists to refuse.
    let mut open_prs_verified = false;
    let mut observed_detail: Option<Vec<(String, u64)>> = None;
    let mut count_window: Option<(String, String)> = None;
    let mut count_source = if args.no_verify_counts {
        "declared-unverified"
    } else {
        "declared-and-verified"
    };

    let open_prs = if args.open_prs_auto {
        let obs = observed_open_prs(&repos).unwrap_or_else(|e| {
            die(&format!(
                "{e}\n\nCannot dereference --open-prs auto against --repos. Supply the total \
                 explicitly, or pass --no-verify-counts to record it as unverified."
            ))
        });
        open_prs_verified = true;
        count_source = "dereferenced-auto";
        count_window = Some((obs.window_start.clone(), obs.window_end.clone()));
        observed_detail = Some(obs.per_repo);
        obs.total
    } else {
        let declared = args.open_prs.unwrap_or_else(|| {
            die("missing --open-prs (pass a number, or `auto` to adopt the dereferenced total)")
        });
        if basis == BASIS_TOTAL_OPEN && !args.no_verify_counts {
            match args.open_prs_observed {
                // Offline/test injection, mirroring --task-states-json. It
                // replaces the LOOKUP, never the COMPARISON.
                Some(injected) => {
                    check_open_prs(declared, injected, basis, &repos, None)
                        .unwrap_or_else(|e| die(&e));
                    count_source = "declared-and-verified-supplied-observation";
                }
                None => {
                    let obs = observed_open_prs(&repos).unwrap_or_else(|e| {
                        die(&format!(
                            "{e}\n\nCannot dereference --open-prs against --repos. Supply the \
                             observed total with --open-prs-observed, pass --open-prs auto to \
                             adopt the dereferenced total, or pass --no-verify-counts to record \
                             the count as unverified."
                        ))
                    });
                    check_open_prs(declared, obs.total, basis, &repos, Some(&obs.per_repo))
                        .unwrap_or_else(|e| die(&e));
                    count_window = Some((obs.window_start.clone(), obs.window_end.clone()));
                    observed_detail = Some(obs.per_repo);
                }
            }
            open_prs_verified = true;
        }
        declared
    };
    let genuine_reds = args
        .genuine_reds
        .unwrap_or_else(|| die("missing --genuine-reds"));
    let fleet_count = args
        .fleet_count
        .unwrap_or_else(|| die("missing --fleet-count"));
    let status_text = read_status_text(args.status_file.as_deref());
    // BIND THE LOG TO THE SEND. The caller passes what the GChat API reported;
    // if the canonical text disagrees the append is REFUSED rather than written
    // and reconciled later. A logged status that does not hash to the delivered
    // message is not a record of that delivery.
    let status_digest = sha256_hex(status_text.as_bytes());
    if let Some(expected) = &args.expect_sha256 {
        if !expected.eq_ignore_ascii_case(&status_digest) {
            die(&format!(
                "--expect-sha256 {expected} does not match the canonical status text \
                 (sha256 {status_digest}, {} bytes). Refusing to append: the logged text \
                 is not the text that was sent. If the difference is a trailing newline, \
                 the sender added one the API did not receive.",
                status_text.len()
            ));
        }
    }
    if let Some(expected) = args.expect_bytes {
        if expected != status_text.len() {
            die(&format!(
                "--expect-bytes {expected} does not match the canonical status text \
                 ({} bytes, sha256 {status_digest}). Refusing to append.",
                status_text.len()
            ));
        }
    }

    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .unwrap_or_else(|e| die(&format!("create log directory {}: {e}", parent.display())));
    }

    // ---- coverage: which hour does this row describe, and when was it written ----
    let written_at = timestamp_from_date();
    let write_hour = hour_of_timestamp(&written_at);
    let covers_hour = args
        .covers_hour
        .clone()
        .unwrap_or_else(|| die("missing --covers-hour (the UTC hour this report DESCRIBES, which is not necessarily the hour it is written in)"));
    // An hour that has not happened cannot have been observed.
    if covers_hour.as_str() > write_hour.as_str() {
        die(&format!(
            "--covers-hour {covers_hour} is in the future relative to the write time \
             {written_at} (hour {write_hour}); an hour that has not elapsed cannot be reported"
        ));
    }
    if let Some(recovery_for) = &args.recovery_for {
        // One entry recovers one hour, and it is the hour it covers. Allowing
        // them to differ would reintroduce the two-time-bases ambiguity inside
        // a single field pair.
        if recovery_for != &covers_hour {
            die(&format!(
                "--recovery-for {recovery_for} must equal --covers-hour {covers_hour}: an \
                 entry recovers the hour it covers"
            ));
        }
    } else if write_hour != covers_hour {
        // THE REGRESSION GUARD. This is exactly the 2026-08-07T02:28 row: 9 p.m.
        // content written in the 10 p.m. hour with nothing marking it late.
        die(&format!(
            "writing in hour {write_hour} an entry that covers {covers_hour} is a RETROACTIVE \
             entry and must say so: pass --recovery-for {covers_hour}. A late report that does \
             not declare itself is indistinguishable from a current one."
        ));
    }

    // Refuse to deepen a divergence that can only be undone by deleting landed
    // rows. Recorded on success so the entry carries the condition it was
    // written under, rather than leaving "was this copy fresh?" unanswerable.
    let local_now = fs::read_to_string(&log_path).unwrap_or_default();
    let log_freshness = match landed_log(&root, &log_path) {
        Some(landed) => match check_log_freshness(&local_now, &landed) {
            Ok(landed_rows) => LogFreshness {
                checked: true,
                landed_rows: Some(landed_rows as u64),
                local_rows: Some(log_rows(&local_now).len() as u64),
                missing_landed_rows: Some(0),
                overridden: None,
                reason: None,
            },
            Err(message) => {
                if !args.allow_stale_log {
                    die(&message);
                }
                LogFreshness {
                    checked: true,
                    landed_rows: None,
                    local_rows: None,
                    missing_landed_rows: Some(missing_landed_rows(&local_now, &landed).len() as u64),
                    overridden: Some(true),
                    reason: None,
                }
            }
        },
        // No landed copy to compare against. Say so in the record instead of
        // letting an unchecked append look identical to a checked one.
        None => LogFreshness {
            checked: false,
            landed_rows: None,
            local_rows: None,
            missing_landed_rows: None,
            overridden: None,
            reason: Some("no origin/main copy of the log to compare against".into()),
        },
    };

    let observed_window = count_window.map(|(start, end)| ObservedWindow { start, end });
    let observed_per_repo = observed_detail.map(|rows| rows.into_iter().collect());
    let ready_prs = args.ready_prs;
    let entry = StatusLogEntry {
        schema_version: SCHEMA_VERSION,
        timestamp: written_at.clone(),
        written_at: written_at.clone(),
        covers_hour,
        // ALWAYS the write time. A mapping is sampled live and cannot be
        // reconstructed for a past hour; saying so is the difference between a
        // known limitation and a silent falsehood.
        mapping_observed_at: written_at,
        workstream_to_worker: status_workers(&active),
        // Recorded, but in its OWN field so it can never be mistaken for activity.
        awaiting_landing_to_worker: status_workers(&awaiting),
        open_prs,
        ready_prs,
        genuine_reds,
        fleet_count,
        status_text_bytes: status_text.len() as u64,
        status_text_sha256: status_digest,
        status_text,
        count_semantics: CountSemantics {
            open_prs: OpenPrCountSemantics {
                basis: BASIS_TOTAL_OPEN.into(),
                includes_drafts: true,
                repos: repos.clone(),
                // Carry the condition with the value: a count that does not say
                // whether it was checked is indistinguishable from one that was,
                // and one that does not say HOW cannot be audited.
                verified: open_prs_verified,
                source: count_source.into(),
                observed_window,
                observed_per_repo,
            },
            // The ready/non-draft subset is a DIFFERENT field with a DIFFERENT
            // basis. It is never allowed to occupy `open_prs`.
            ready_prs: ready_prs.map(|_| ReadyPrCountSemantics {
                basis: BASIS_READY.into(),
                includes_drafts: false,
                repos,
            }),
        },
        log_freshness,
        recovery_for: args.recovery_for.clone(),
    };
    entry.validate().unwrap_or_else(|error| die(&error));
    let line = serde_json::to_string(
        &serde_json::to_value(&entry)
            .unwrap_or_else(|error| die(&format!("serialize status entry: {error}"))),
    )
    .unwrap_or_else(|e| die(&format!("serialize status entry: {e}")));

    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap_or_else(|e| die(&format!("open log {}: {e}", log_path.display())));
    log.write_all(line.as_bytes())
        .and_then(|_| log.write_all(b"\n"))
        .and_then(|_| log.flush())
        .unwrap_or_else(|e| die(&format!("append log {}: {e}", log_path.display())));

    println!("appended {}", log_path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_status_log_entry() -> StatusLogEntry {
        let status_text = "Status is green.".to_string();
        StatusLogEntry {
            schema_version: SCHEMA_VERSION,
            timestamp: "2026-08-28T07:30:00Z".into(),
            written_at: "2026-08-28T07:30:00Z".into(),
            covers_hour: "2026-08-28T07".into(),
            mapping_observed_at: "2026-08-28T07:30:00Z".into(),
            workstream_to_worker: [(
                "fleet-observability/status-log".into(),
                StatusWorker {
                    agent: "type-audit".into(),
                    task: "status-log-row-contract".into(),
                    cwd: "worktrees/slot01/hermit".into(),
                },
            )]
            .into_iter()
            .collect(),
            awaiting_landing_to_worker: BTreeMap::new(),
            open_prs: 3,
            ready_prs: Some(2),
            genuine_reds: 0,
            fleet_count: 12,
            status_text_bytes: status_text.len() as u64,
            status_text_sha256: sha256_hex(status_text.as_bytes()),
            status_text,
            count_semantics: CountSemantics {
                open_prs: OpenPrCountSemantics {
                    basis: BASIS_TOTAL_OPEN.into(),
                    includes_drafts: true,
                    repos: vec!["rrnewton/hermit".into(), "rrnewton/reverie".into()],
                    verified: true,
                    source: "declared-and-verified-supplied-observation".into(),
                    observed_window: None,
                    observed_per_repo: None,
                },
                ready_prs: Some(ReadyPrCountSemantics {
                    basis: BASIS_READY.into(),
                    includes_drafts: false,
                    repos: vec!["rrnewton/hermit".into(), "rrnewton/reverie".into()],
                }),
            },
            log_freshness: LogFreshness {
                checked: true,
                landed_rows: Some(137),
                local_rows: Some(137),
                missing_landed_rows: Some(0),
                overridden: None,
                reason: None,
            },
            recovery_for: None,
        }
    }

    #[test]
    fn current_status_log_row_round_trips_through_the_complete_type() {
        let entry = current_status_log_entry();
        entry.validate().unwrap();
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(StatusLogEntry::from_value(value.clone()).unwrap(), entry);
        let described = describe_entry(137, &value).unwrap();
        assert!(described.contains("open_prs=3"), "{described}");
        assert!(
            described.contains("rrnewton/hermit+rrnewton/reverie"),
            "{described}"
        );
    }

    #[test]
    fn current_status_log_row_missing_field_fails_by_name() {
        let mut value = serde_json::to_value(current_status_log_entry()).unwrap();
        value.as_object_mut().unwrap().remove("status_text_sha256");
        let error = StatusLogEntry::from_value(value).unwrap_err();
        assert!(error.contains("status_text_sha256"), "{error}");
    }

    #[test]
    fn current_status_log_row_unknown_field_is_refused() {
        let mut value = serde_json::to_value(current_status_log_entry()).unwrap();
        value["unreviewed_count"] = json!(0);
        let error = StatusLogEntry::from_value(value).unwrap_err();
        assert!(error.contains("unreviewed_count"), "{error}");
    }

    #[test]
    fn newer_status_log_schema_is_refused_after_reading_the_version() {
        let mut value = serde_json::to_value(current_status_log_entry()).unwrap();
        value["schema_version"] = json!(SCHEMA_VERSION + 1);
        let describe_error = describe_entry(0, &value).unwrap_err();
        assert!(
            describe_error.contains("schema_version"),
            "{describe_error}"
        );
        assert!(describe_error.contains("newer schema"), "{describe_error}");

        let row = serde_json::to_string(&value).unwrap();
        let reconcile_error = reconcile_rows(&row, "").unwrap_err();
        assert!(
            reconcile_error.contains("schema_version"),
            "{reconcile_error}"
        );
        assert!(
            reconcile_error.contains("newer schema"),
            "{reconcile_error}"
        );
    }

    #[test]
    fn current_status_log_row_changed_text_fails_by_digest_field_name() {
        let mut value = serde_json::to_value(current_status_log_entry()).unwrap();
        value["status_text"] = json!("Status is not green.");
        value["status_text_bytes"] = json!("Status is not green.".len());
        let error = StatusLogEntry::from_value(value).unwrap_err();
        assert!(error.contains("status_text_sha256"), "{error}");
    }

    #[test]
    fn live_count_source_without_observation_fails_by_name() {
        let mut value = serde_json::to_value(current_status_log_entry()).unwrap();
        value["count_semantics"]["open_prs"]["source"] = json!("declared-and-verified");
        let error = StatusLogEntry::from_value(value).unwrap_err();
        assert!(error.contains("open_prs.source"), "{error}");
        assert!(error.contains("observed_window"), "{error}");
        assert!(error.contains("observed_per_repo"), "{error}");
    }

    #[test]
    fn reconciliation_refuses_a_malformed_current_row_by_name() {
        let mut value = serde_json::to_value(current_status_log_entry()).unwrap();
        value["ready_prs"] = json!(4);
        let row = serde_json::to_string(&value).unwrap();
        let error = reconcile_rows(&row, "").unwrap_err();
        assert!(error.contains("ready_prs"), "{error}");
    }

    #[test]
    fn every_retained_status_log_row_remains_readable() {
        let retained = include_str!("../ai_docs/status-log/status-log.jsonl");
        let mut rows = 0;
        for line in retained.lines().filter(|line| !line.trim().is_empty()) {
            let value: Value = serde_json::from_str(line).unwrap();
            describe_entry(rows, &value).unwrap();
            rows += 1;
        }
        assert!(
            rows > 0,
            "the compatibility test must exercise retained rows"
        );
    }

    // =====================================================================
    // THE MAPPING IS AN ACTIVITY REPORT, NOT AN OWNERSHIP LEDGER
    //
    // Regression source: ai_docs/status-log/status-log.jsonl entry
    // 2026-08-07T01:00:36Z. 19 workstreams, 7 agents, hermit-w3 seven times,
    // and zero of the 19 tasks genuinely active.
    // =====================================================================

    fn worker(agent: &str, task: &str, cwd: &str) -> Value {
        json!({"agent": agent, "task": task, "cwd": cwd})
    }

    /// The seven tasks the real report attributed to hermit-w3 simultaneously.
    /// Every one of them is tagged `implemented` in live TaskGraph.
    const W3_IMPLEMENTED_TASKS: &[&str] = &[
        "adversarial_review_claude_pr",
        "correct_pr_1147_claims",
        "demo-presentation-cycle",
        "enforce_typed_fail_closed",
        "mutation-test-the-fixtures-can-they-fail",
        "resolve-parent-main-divergence-7-ahead-4-behind",
        "spawn-conflates-cli-type-with-model-selection-claude-got-gpt-model",
    ];

    fn implemented_states() -> BTreeMap<String, TaskState> {
        W3_IMPLEMENTED_TASKS
            .iter()
            .map(|t| (t.to_string(), TaskState::AwaitingLanding))
            .collect()
    }

    // ---- POSITIVE: a well-formed one-per-agent mapping is accepted ----

    #[test]
    fn a_descriptive_one_per_agent_mapping_is_accepted() {
        let mapping = json!({
            "backend-parity/dbi-stack-hash-determinism":
                worker("hermit-w6", "dbi_detlog_stack_hashes", "worktrees/w6dbistack/reverie"),
            "fleet-observability/hourly-status-synthesis":
                worker("hermit-w3", "hourly-status-workstreams", "worktrees/w3/hermit"),
        });
        let rows = parse_mapping(&mapping).expect("well-formed mapping must parse");
        assert_eq!(rows.len(), 2);
        validate_one_workstream_per_agent(&rows).expect("distinct agents must be accepted");

        // Every bullet carries agent + task + workspace, which is what the
        // owner needs to follow one up.
        for row in &rows {
            assert!(!row.agent.is_empty() && !row.task.is_empty() && !row.cwd.is_empty());
            assert!(
                row.cwd.contains('/'),
                "cwd must be a workspace path: {row:?}"
            );
            assert!(
                row.slug.contains('/'),
                "slug must be major-goal/sub-goal: {row:?}"
            );
        }

        let states: BTreeMap<String, TaskState> = rows
            .iter()
            .map(|r| (r.task.clone(), TaskState::Active))
            .collect();
        validate_task_states(&rows, &states, true, "--mapping-json")
            .expect("genuinely active tasks must be accepted");
    }

    #[test]
    fn distinct_agents_stay_distinct() {
        let mapping = json!({
            "aaa-goal/one": worker("hermit-w1", "task-one", "worktrees/w1/hermit"),
            "bbb-goal/two": worker("hermit-w2", "task-two", "worktrees/w2/hermit"),
            "ccc-goal/three": worker("hermit-w3", "task-three", "worktrees/w3/hermit"),
        });
        let rows = parse_mapping(&mapping).unwrap();
        validate_one_workstream_per_agent(&rows).unwrap();
        let agents: Vec<&str> = rows.iter().map(|r| r.agent.as_str()).collect();
        assert_eq!(agents, vec!["hermit-w1", "hermit-w2", "hermit-w3"]);
    }

    // ---- NEGATIVE: THE regression -- one agent, many stale owned tasks ----

    /// The exact shape that produced seven hermit-w3 bullets. Rendering the
    /// agent's whole ownership set is refused, and the refusal names the agent
    /// and every slug it claimed so the coordinator can see what to collapse.
    #[test]
    fn one_agent_owning_seven_tasks_cannot_emit_seven_active_bullets() {
        let mut mapping = serde_json::Map::new();
        for (i, task) in W3_IMPLEMENTED_TASKS.iter().enumerate() {
            mapping.insert(
                format!(
                    "fleet-work/stale-claim-{}",
                    ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf"][i]
                ),
                worker("hermit-w3", task, "worktrees/w3/hermit"),
            );
        }
        let rows = parse_mapping(&Value::Object(mapping)).unwrap();
        assert_eq!(rows.len(), 7, "fixture must reproduce the 7-row shape");

        let err = validate_one_workstream_per_agent(&rows)
            .expect_err("seven bullets for one agent must be refused");
        assert!(err.contains("hermit-w3"), "must name the agent: {err}");
        assert!(err.contains("7 workstreams"), "must give the count: {err}");
        assert!(
            err.contains("--awaiting-landing-json"),
            "must say where the implemented work belongs: {err}"
        );

        // ...and independently, every one of those tasks is implemented, so
        // even ONE of them is not active work.
        let states = implemented_states();
        let single = vec![rows[0].clone()];
        let err = validate_task_states(&single, &states, true, "--mapping-json")
            .expect_err("an implemented task is not activity");
        assert!(err.contains("IMPLEMENTED"), "{err}");
    }

    /// After the fix, the same agent emits EXACTLY ONE active bullet, and its
    /// stale ownership is still recorded -- just not as activity.
    #[test]
    fn the_corrected_report_emits_exactly_one_active_bullet_per_agent() {
        let active = json!({
            "fleet-observability/hourly-status-synthesis":
                worker("hermit-w3", "hourly-status-workstreams", "worktrees/w3/hermit"),
        });
        let awaiting = json!({
            "review-drain/claude-pr-adversarial-review":
                worker("hermit-w3", "adversarial_review_claude_pr", "worktrees/w3/hermit"),
            "review-drain/pr-1147-claim-correction":
                worker("hermit-w3", "correct_pr_1147_claims", "worktrees/w3/hermit"),
        });
        let active_rows = parse_mapping(&active).unwrap();
        let awaiting_rows = parse_mapping(&awaiting).unwrap();

        // EXACTLY ONE active bullet for hermit-w3.
        assert_eq!(active_rows.len(), 1);
        validate_one_workstream_per_agent(&active_rows).unwrap();

        let mut states = implemented_states();
        states.insert("hourly-status-workstreams".to_string(), TaskState::Active);
        validate_task_states(&active_rows, &states, true, "--mapping-json").unwrap();
        // The awaiting side accepts precisely what the active side refuses.
        validate_task_states(&awaiting_rows, &states, false, "--awaiting-landing-json").unwrap();

        // The recorded JSON keeps agent + task + cwd on every row.
        let recorded = rows_to_json(&active_rows);
        let row = &recorded["fleet-observability/hourly-status-synthesis"];
        assert_eq!(row["agent"], "hermit-w3");
        assert_eq!(row["task"], "hourly-status-workstreams");
        assert_eq!(row["cwd"], "worktrees/w3/hermit");
    }

    // ---- NEGATIVE: the awaiting-landing field is not a dumping ground ----

    #[test]
    fn an_active_task_is_refused_in_the_awaiting_landing_field() {
        let rows = parse_mapping(&json!({
            "fleet-observability/hourly-status-synthesis":
                worker("hermit-w3", "still-working", "worktrees/w3/hermit"),
        }))
        .unwrap();
        let states: BTreeMap<String, TaskState> =
            [("still-working".to_string(), TaskState::Active)]
                .into_iter()
                .collect();
        let err = validate_task_states(&rows, &states, false, "--awaiting-landing-json")
            .expect_err("active work is not awaiting landing");
        assert!(err.contains("--awaiting-landing-json"), "{err}");
    }

    #[test]
    fn closed_and_missing_tasks_are_refused_as_active() {
        // fix-post-1.0-codex-invalid-sandbox-flag really was CLOSED when the
        // 01:00 report listed it as a live workstream.
        let rows = parse_mapping(&json!({
            "codex-startup/invalid-sandbox-flag":
                worker("hermit-w4", "fix-post-1.0-codex-invalid-sandbox-flag", "worktrees/w4/hermit"),
            "backend-rename/dbi-to-dbt":
                worker("hermit-w1", "rename-dbi-to-dbt-backend", "worktrees/w1/hermit"),
        }))
        .unwrap();
        let states: BTreeMap<String, TaskState> = [(
            "fix-post-1.0-codex-invalid-sandbox-flag".to_string(),
            TaskState::Closed,
        )]
        .into_iter()
        .collect();
        let err = validate_task_states(&rows, &states, true, "--mapping-json").unwrap_err();
        assert!(
            err.contains("CLOSED"),
            "a closed task is not activity: {err}"
        );
        assert!(
            err.contains("NOT PRESENT in TaskGraph"),
            "an unknown task id must not pass silently: {err}"
        );
    }

    // ---- SLUGS: descriptive and stable, positive and negative ----

    #[test]
    fn descriptive_major_goal_sub_goal_slugs_are_accepted() {
        for slug in [
            "backend-parity/dbi-stack-hash-determinism",
            "pr-landing/open-pr-drain",
            "fleet-observability/hourly-status-synthesis",
            "btrfs-flood-fix/claude-agent",
            "validate/box-quarantine-recovery",
        ] {
            validate_slug(slug, "some-unrelated-task-id")
                .unwrap_or_else(|e| panic!("{slug:?} should be accepted: {e}"));
        }
    }

    #[test]
    fn a_flat_slug_is_refused_because_it_is_not_a_workstream() {
        // Every slug in all 50 logged entries is flat today.
        let err = validate_slug(
            "dbi-detlog-stack-hash-nondeterminism",
            "dbi_detlog_stack_hashes",
        )
        .unwrap_err();
        assert!(err.contains("major-goal/sub-goal"), "{err}");
    }

    #[test]
    fn bare_ordinal_and_placeholder_slugs_are_refused() {
        for slug in [
            "phase-1/do-the-thing",
            "rollout/wave-x",
            "options/option-a",
            "drain/round-2",
            "drain/batch",
            "cleanup/v2",
            "cleanup/3",
            "tbd/something-real",
            "unknown/whatever-goes-here",
        ] {
            validate_slug(slug, "unrelated-task").expect_err(&format!("{slug:?} must be refused"));
        }
    }

    #[test]
    fn a_slug_that_merely_restates_the_task_id_is_refused() {
        // 107 rows in the existing log have slug == task id.
        let err = validate_slug(
            "demo5-prefix-parity/depth-ratchet",
            "demo5_prefix_parity_depth_ratchet",
        )
        .unwrap_err();
        assert!(err.contains("restatement"), "{err}");
        assert!(
            err.contains("demo5"),
            "must quote the offending pair: {err}"
        );
    }

    #[test]
    fn malformed_slug_segments_are_refused() {
        for slug in [
            "Backend-Parity/dbi",       // uppercase
            "backend parity/dbi-stack", // space
            "backend_parity/dbi-stack", // underscore
            "backend-parity/",          // empty segment
            "/dbi-stack",               // empty segment
            "backend-parity/-leading",  // leading hyphen
            "backend-parity/trailing-", // trailing hyphen
            "backend-parity/ab",        // too short to be descriptive
        ] {
            assert!(
                validate_slug(slug, "unrelated-task").is_err(),
                "{slug:?} must be refused"
            );
        }
    }

    // ---- WORKER FIELDS: agent + slot/cwd are mandatory and real ----

    #[test]
    fn a_missing_cwd_is_refused_so_every_bullet_names_a_workspace() {
        let err = validate_workstream(
            "backend-parity/dbi-stack-hash-determinism",
            &json!({"agent": "hermit-w6", "task": "dbi_detlog_stack_hashes"}),
        )
        .unwrap_err();
        assert!(err.contains("cwd"), "{err}");
    }

    #[test]
    fn a_cwd_that_is_not_a_path_is_refused() {
        let err = validate_workstream(
            "backend-parity/dbi-stack-hash-determinism",
            &json!({"agent": "hermit-w6", "task": "dbi_detlog_stack_hashes", "cwd": "w6dbistack"}),
        )
        .unwrap_err();
        assert!(err.contains("slot or working directory"), "{err}");
    }

    /// `{"agent":"none"}` is in the real log at 2026-08-07T01:19:42Z: it passed
    /// because the old check only asked for a non-empty string.
    #[test]
    fn placeholder_agents_and_tasks_are_refused() {
        for bad in ["none", "unknown", "n/a", "-", "TBD", "various", "pending"] {
            validate_workstream(
                "ci-hub/compile-repair",
                &json!({"agent": bad, "task": "ci_hub_is_fleet", "cwd": "worktrees/ci/hermit"}),
            )
            .expect_err(&format!("agent {bad:?} must be refused"));
            assert!(
                validate_workstream(
                    "ci-hub/compile-repair",
                    &json!({"agent": "hermit-w5", "task": bad, "cwd": "worktrees/ci/hermit"}),
                )
                .is_err(),
                "task {bad:?} must be refused"
            );
        }
    }

    // ---- TASK STATE CLASSIFICATION ----

    #[test]
    fn implemented_beats_in_progress_because_that_is_the_policy_shape() {
        // Policy: implemented work KEEPS status in_progress until it lands, so
        // reading status alone is exactly how ownership became "activity".
        assert_eq!(
            classify_task("IN_PROGRESS", &["determinism".into(), "implemented".into()]),
            TaskState::AwaitingLanding
        );
        assert_eq!(
            classify_task("IN_PROGRESS", &["determinism".into()]),
            TaskState::Active
        );
        assert_eq!(classify_task("OPEN", &[]), TaskState::Active);
        assert_eq!(classify_task("CLOSED", &[]), TaskState::Closed);
        // Terminal wins over nothing else; an implemented+closed row is still
        // not active, which is the property that matters.
        assert_ne!(
            classify_task("CLOSED", &["implemented".into()]),
            TaskState::Active
        );
    }

    #[test]
    fn task_states_are_read_from_the_authority_not_from_the_caller() {
        // Exactly the `tg sql` table shape.
        let table = "\
local_id | status | tags
---------------------------------------------
adversarial_review_claude_pr | IN_PROGRESS | [\"review\",\"implemented\"]
liteinst_in_guest_backend | OPEN | []
fix-post-1.0-codex-invalid-sandbox-flag | CLOSED | [\"implemented\"]

(3 rows)";
        let states = parse_tg_sql_rows(table);
        assert_eq!(
            states["adversarial_review_claude_pr"],
            TaskState::AwaitingLanding
        );
        assert_eq!(states["liteinst_in_guest_backend"], TaskState::Active);
        assert_ne!(
            states["fix-post-1.0-codex-invalid-sandbox-flag"],
            TaskState::Active
        );
        assert_eq!(
            states.len(),
            3,
            "header/rule/footer lines must not become rows"
        );
    }

    #[test]
    fn a_task_states_fixture_round_trips() {
        let states = parse_task_states_json(
            r#"{"a":{"status":"IN_PROGRESS","tags":["implemented"]},
                "b":{"status":"IN_PROGRESS","tags":[]}}"#,
        )
        .unwrap();
        assert_eq!(states["a"], TaskState::AwaitingLanding);
        assert_eq!(states["b"], TaskState::Active);
    }

    // =====================================================================
    // STATUS-LOG FRESHNESS
    //
    // Measured hazard, 2026-08-07T03:10Z: the shared parent copy of
    // ai_docs/status-log/status-log.jsonl stood at +5/-51 against origin/main
    // (53 local rows vs 99 landed). Committing it would have deleted 51
    // delivered status records.
    // =====================================================================

    fn row(ts: &str) -> String {
        format!(r#"{{"timestamp":"{ts}","open_prs":1}}"#)
    }
    fn log_of(stamps: &[&str]) -> String {
        stamps.iter().map(|t| row(t)).collect::<Vec<_>>().join("\n") + "\n"
    }

    // ---- NEGATIVE: a copy missing landed rows is refused ----

    #[test]
    fn a_copy_missing_landed_rows_is_refused_and_says_how_many() {
        let landed = log_of(&["t1", "t2", "t3", "t4"]);
        let local = log_of(&["t1", "t4"]); // t2, t3 landed while this copy sat
        let err = check_log_freshness(&local, &landed)
            .expect_err("a copy that would delete landed rows must be refused");
        assert!(
            err.contains("missing 2 row(s)"),
            "must count the loss: {err}"
        );
        assert!(
            err.contains("oldest t2") && err.contains("newest t3"),
            "must bracket it: {err}"
        );
        assert!(
            err.contains("--print-reconciled"),
            "must name the remedy: {err}"
        );
        assert!(
            err.contains("append-only"),
            "must say why it matters: {err}"
        );
    }

    /// The exact measured shape: 99 landed, 53 local of which 5 are local-only,
    /// so 51 landed rows are absent.
    #[test]
    fn the_measured_plus5_minus51_shared_copy_is_refused() {
        let landed_stamps: Vec<String> = (0..99).map(|i| format!("L{i:03}")).collect();
        let landed = log_of(&landed_stamps.iter().map(String::as_str).collect::<Vec<_>>());
        let mut local_stamps: Vec<String> = landed_stamps[..48].to_vec(); // kept 48 landed
        local_stamps.extend((0..5).map(|i| format!("W{i}"))); // plus 5 local-only
        let local = log_of(&local_stamps.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(log_rows(&local).len(), 53);
        assert_eq!(log_rows(&landed).len(), 99);
        assert_eq!(missing_landed_rows(&local, &landed).len(), 51);
        assert!(check_log_freshness(&local, &landed).is_err());
    }

    // ---- POSITIVE: a fresh copy, and an appended-to fresh copy, are accepted ----

    #[test]
    fn an_identical_copy_is_accepted() {
        let landed = log_of(&["t1", "t2", "t3"]);
        assert_eq!(check_log_freshness(&landed, &landed).unwrap(), 3);
    }

    #[test]
    fn a_superset_copy_with_local_appends_is_accepted() {
        let landed = log_of(&["t1", "t2"]);
        let local = log_of(&["t1", "t2", "t3"]); // ahead, nothing lost
        assert_eq!(check_log_freshness(&local, &landed).unwrap(), 2);
        assert!(missing_landed_rows(&local, &landed).is_empty());
    }

    /// The guard must key on ROW IDENTITY, not on being behind. This parent
    /// checkout runs dozens of commits behind while the log matches, and a
    /// HEAD-based guard would refuse every legitimate append.
    #[test]
    fn being_behind_on_commits_is_not_by_itself_staleness() {
        let landed = log_of(&["t1", "t2"]);
        let local = log_of(&["t1", "t2"]); // same rows, arbitrarily older HEAD
        assert!(check_log_freshness(&local, &landed).is_ok());
    }

    // ---- RECONCILIATION: no lost rows, no duplicates, timestamp order ----

    #[test]
    fn reconciliation_loses_nothing_and_duplicates_nothing() {
        let landed = log_of(&["t1", "t3", "t5"]);
        let local = log_of(&["t1", "t2", "t9"]);
        let merged = reconcile_rows(&local, &landed).unwrap();
        assert_eq!(merged.len(), 5, "union of 3 landed + 3 local sharing 1 = 5");
        for src in [&landed, &local] {
            for r in log_rows(src) {
                assert!(merged.contains(&r), "reconciliation must lose nothing: {r}");
            }
        }
        let unique: std::collections::HashSet<&String> = merged.iter().collect();
        assert_eq!(unique.len(), merged.len(), "no duplicates");
        let stamps: Vec<String> = merged
            .iter()
            .map(|r| {
                serde_json::from_str::<Value>(r).unwrap()["timestamp"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            stamps,
            vec!["t1", "t2", "t3", "t5", "t9"],
            "timestamp order"
        );
    }

    #[test]
    fn a_reconciled_copy_then_passes_the_guard() {
        // Close the loop: the remedy the refusal names actually clears it.
        let landed = log_of(&["t1", "t2", "t3", "t4"]);
        let local = log_of(&["t1", "t4"]);
        assert!(check_log_freshness(&local, &landed).is_err());
        let merged = reconcile_rows(&local, &landed).unwrap().join("\n") + "\n";
        assert!(
            check_log_freshness(&merged, &landed).is_ok(),
            "reconciling must be sufficient, not merely advisory"
        );
        assert_eq!(log_rows(&merged).len(), 4);
    }

    #[test]
    fn an_unparseable_row_still_participates_by_identity() {
        // Rows are compared as bytes, so a malformed landed row is still
        // protected from deletion rather than being silently dropped.
        let landed = "not json\n".to_string() + &row("t2") + "\n";
        let local = row("t2") + "\n";
        let missing = missing_landed_rows(&local, &landed);
        assert_eq!(missing, vec!["not json".to_string()]);
        let err = check_log_freshness(&local, &landed).unwrap_err();
        assert!(err.contains("<unparseable row>"), "{err}");
    }

    #[test]
    fn an_empty_local_copy_is_maximally_stale() {
        let landed = log_of(&["t1", "t2"]);
        assert_eq!(missing_landed_rows("", &landed).len(), 2);
        assert!(check_log_freshness("", &landed).is_err());
    }

    // ---- POSITIVE: total-open with an explicit repo scope is accepted ----

    #[test]
    fn explicit_repo_scope_is_accepted_and_deduped() {
        assert_eq!(
            parse_repos("rrnewton/hermit,rrnewton/reverie").unwrap(),
            vec!["rrnewton/hermit", "rrnewton/reverie"]
        );
        // whitespace tolerated, duplicates collapsed -- the denominator is a SET
        assert_eq!(
            parse_repos(" rrnewton/hermit , rrnewton/hermit ").unwrap(),
            vec!["rrnewton/hermit"]
        );
    }

    #[test]
    fn default_basis_is_total_open_including_drafts() {
        assert!(validate_open_prs_basis(BASIS_TOTAL_OPEN).is_ok());
    }

    // ---- NEGATIVE: a ready-only count under `open_prs` is refused ----

    #[test]
    fn ready_only_basis_is_refused_under_open_prs_and_names_the_right_field() {
        for basis in [
            "ready",
            "ready-only",
            "ready_non_draft",
            "non-draft",
            "nondraft",
            "no-drafts",
            "excluding-drafts",
            "READY",
        ] {
            let err = validate_open_prs_basis(basis)
                .expect_err(&format!("{basis:?} is a subset and must be refused"));
            assert!(
                err.contains("--ready-prs"),
                "refusal must redirect to the distinct field, got: {err}"
            );
            assert!(err.contains("SUBSET"), "refusal must say why: {err}");
        }
    }

    #[test]
    fn an_unrecognised_basis_is_refused_rather_than_silently_recorded() {
        let err = validate_open_prs_basis("whatever-i-counted").unwrap_err();
        assert!(err.contains(BASIS_TOTAL_OPEN));
    }

    #[test]
    fn a_missing_or_malformed_repo_scope_is_refused() {
        for bad in [
            "",
            "   ",
            "hermit",
            "rrnewton/",
            "/hermit",
            "a/b/c",
            "rrnewton/hermit,",
        ] {
            assert!(
                parse_repos(bad).is_err(),
                "{bad:?} is not a usable denominator and must be refused"
            );
        }
    }

    // ---- OLD ROWS: labelled unknown, never rewritten or assumed ----

    #[test]
    fn pre_schema2_rows_are_labelled_unknown_denominator() {
        // Exactly the shape of all 50 pre-existing rows: no schema_version,
        // no count_semantics.
        let old = serde_json::json!({
            "timestamp": "2026-08-06T15:05:12Z",
            "open_prs": 105,
            "genuine_reds": 0,
            "fleet_count": 15,
            "status_text": "…",
        });
        let line = describe_entry(7, &old).unwrap();
        assert!(line.contains("denominator=unknown"), "{line}");
        assert!(line.contains("basis=unknown"), "{line}");
        assert!(line.contains("open_prs=105"), "{line}");
        assert!(line.contains("2026-08-06T15:05:12Z"), "{line}");
    }

    #[test]
    fn schema2_rows_report_their_actual_basis_and_scope() {
        let new = serde_json::json!({
            "schema_version": 2,
            "timestamp": "2026-08-07T02:00:00Z",
            "open_prs": 10,
            "count_semantics": {
                "open_prs": {
                    "basis": BASIS_TOTAL_OPEN,
                    "includes_drafts": true,
                    "repos": ["rrnewton/hermit", "rrnewton/reverie"],
                },
            },
        });
        let line = describe_entry(8, &new).unwrap();
        assert!(line.contains(BASIS_TOTAL_OPEN), "{line}");
        assert!(line.contains("rrnewton/hermit+rrnewton/reverie"), "{line}");
        // Assert the DENOMINATOR specifically, not the bare word: this row is a
        // schema-2 entry, so its COVERED HOUR is legitimately unknown and saying
        // so is correct. A substring check for "unknown" conflates two different
        // unknowns and would have to be relaxed every time a field is added.
        assert!(
            !line.contains("denominator=unknown"),
            "a scoped row must not read denominator=unknown: {line}"
        );
        assert!(
            line.contains("covers=unknown pre-schema-4-entry"),
            "a pre-coverage row must say its covered hour is unknown: {line}"
        );
    }

    /// THE REGRESSION THIS EXISTS FOR: 105 and 10 nineteen minutes apart looked
    /// like a 10x drop. Once each row carries its denominator, the two are
    /// visibly incomparable rather than falsely comparable.
    #[test]
    fn the_105_vs_10_pair_is_no_longer_silently_comparable() {
        let total = serde_json::json!({
            "schema_version": 2, "timestamp": "t1", "open_prs": 105,
            "count_semantics": {"open_prs": {"basis": BASIS_TOTAL_OPEN, "repos": ["rrnewton/hermit"]}},
        });
        // The ready subset can no longer be written as open_prs at all...
        assert!(validate_open_prs_basis("ready").is_err());
        // ...so it lands in its own field, and the describe view shows the bases differ.
        let ready_as_its_own_field = serde_json::json!({
            "schema_version": 2, "timestamp": "t2", "open_prs": 105, "ready_prs": 10,
            "count_semantics": {
                "open_prs": {"basis": BASIS_TOTAL_OPEN, "repos": ["rrnewton/hermit"]},
                "ready_prs": {"basis": BASIS_READY, "repos": ["rrnewton/hermit"]},
            },
        });
        let a = describe_entry(0, &total).unwrap();
        let b = describe_entry(1, &ready_as_its_own_field).unwrap();
        assert!(
            a.contains("open_prs=105") && b.contains("open_prs=105"),
            "the comparable quantity now agrees: {a} / {b}"
        );
    }
    // ============ one canonical byte sequence, log and claim ============

    #[test]
    fn sha256_matches_published_vectors() {
        // status-log and hourly-status-relay must agree digit for digit, so both
        // are pinned to the same published vectors rather than to each other.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 1000]),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn canonical_text_strips_at_most_one_terminator() {
        // At most ONE: trailing blank lines inside a status are author content,
        // and a trim_end would silently rewrite the delivered message.
        assert_eq!(canonical_status_text("x\n".into()), "x");
        assert_eq!(canonical_status_text("x\r\n".into()), "x");
        assert_eq!(canonical_status_text("x".into()), "x");
        assert_eq!(canonical_status_text("x\n\n".into()), "x\n");
        assert_eq!(canonical_status_text("x\n\n\n".into()), "x\n\n");
        assert_eq!(canonical_status_text("\n".into()), "");
        assert_eq!(canonical_status_text("".into()), "");
        // Interior newlines are untouched.
        assert_eq!(canonical_status_text("a\nb\nc\n".into()), "a\nb\nc");
    }

    #[test]
    fn both_transports_yield_the_same_canonical_bytes() {
        // The skew was INCONSISTENT, which is worse than uniformly wrong: file
        // entries carried the extra byte and stdin entries did not, so which
        // bytes were canonical depended on how the caller passed the text.
        let sent = "## Hourly status\nbody";
        let from_file = canonical_status_text(format!("{sent}\n"));
        let from_stdin = canonical_status_text(sent.to_string());
        assert_eq!(from_file, from_stdin);
        assert_eq!(
            sha256_hex(from_file.as_bytes()),
            sha256_hex(from_stdin.as_bytes())
        );
        assert_eq!(from_file.len(), sent.len());
    }

    #[test]
    fn reproduces_the_measured_t02_skew_and_closes_it() {
        // The exact observed case, message QF7vBsq-DXc.QF7vBsq-DXc: the API
        // reported N bytes and the JSONL recorded N+1, differing by one appended
        // 0x0A in the common prefix. Reconstructed at small scale: the raw file
        // form and the canonical form must NOT hash alike, and canonicalizing
        // must land on the API form.
        let api_text = "## Hourly status — recovery delivery for 10:00 p.m. EDT";
        let file_form = format!("{api_text}\n");
        assert_eq!(file_form.len(), api_text.len() + 1);
        assert_ne!(
            sha256_hex(file_form.as_bytes()),
            sha256_hex(api_text.as_bytes()),
            "one byte must change the digest, else the skew would have been invisible"
        );
        let canonical = canonical_status_text(file_form);
        assert_eq!(canonical, api_text);
        assert_eq!(
            sha256_hex(canonical.as_bytes()),
            sha256_hex(api_text.as_bytes())
        );
    }

    // ================ covers_hour / recovery_for semantics ================
    // The row that forced this: 2026-08-07T02:28:13Z carried 9 p.m. narrative and
    // 9 p.m. counts, a mapping sampled at 10:28 p.m., and a 10:28 p.m. timestamp.
    // Three time bases, one row, nothing in the schema distinguishing them.

    #[test]
    fn hour_bucket_parsing_accepts_real_hours_and_refuses_the_rest() {
        assert_eq!(
            parse_hour_bucket("--covers-hour", "2026-08-07T01"),
            "2026-08-07T01"
        );
        assert_eq!(
            parse_hour_bucket("--covers-hour", "2026-12-31T23"),
            "2026-12-31T23"
        );
        assert_eq!(
            parse_hour_bucket("--covers-hour", "2026-01-01T00"),
            "2026-01-01T00"
        );
    }

    #[test]
    fn hour_of_timestamp_truncates_to_the_bucket() {
        assert_eq!(hour_of_timestamp("2026-08-07T02:28:13Z"), "2026-08-07T02");
        assert_eq!(hour_of_timestamp("2026-08-07T01:00:00Z"), "2026-08-07T01");
        // The exact pair from the defect: written in T02, covering T01.
        assert_ne!(
            hour_of_timestamp("2026-08-07T02:28:13Z"),
            "2026-08-07T01",
            "if these compared equal the retroactive guard could never fire"
        );
    }

    #[test]
    fn describe_reports_current_recovery_and_legacy_coverage_distinctly() {
        // CURRENT: covered hour equals the write hour, no recovery marker.
        let current = json!({
            "schema_version": COVERAGE_SCHEMA,
            "timestamp": "2026-08-07T03:00:11Z",
            "written_at": "2026-08-07T03:00:11Z",
            "covers_hour": "2026-08-07T03",
            "open_prs": 4,
            "count_semantics": {"open_prs": {"basis": BASIS_TOTAL_OPEN, "repos": ["rrnewton/hermit"]}},
        });
        let line = describe_entry(1, &current).unwrap();
        assert!(line.contains("covers=2026-08-07T03"), "{line}");
        assert!(line.contains("kind=current"), "{line}");
        assert!(!line.contains("recovery_for"), "{line}");

        // RECOVERY: the real shape of the 02:28 row, now self-describing.
        let recovery = json!({
            "schema_version": COVERAGE_SCHEMA,
            "timestamp": "2026-08-07T02:28:13Z",
            "written_at": "2026-08-07T02:28:13Z",
            "covers_hour": "2026-08-07T01",
            "recovery_for": "2026-08-07T01",
            "mapping_observed_at": "2026-08-07T02:28:13Z",
            "open_prs": 10,
            "count_semantics": {"open_prs": {"basis": BASIS_TOTAL_OPEN, "repos": ["rrnewton/hermit"]}},
        });
        let line = describe_entry(2, &recovery).unwrap();
        assert!(line.contains("covers=2026-08-07T01"), "{line}");
        assert!(line.contains("kind=recovery"), "{line}");
        assert!(line.contains("recovery_for=2026-08-07T01"), "{line}");
        // And it is NOT filed under its write hour, which was the whole defect.
        assert!(!line.contains("covers=2026-08-07T02"), "{line}");

        // LEGACY: a pre-coverage row must read unknown, never be back-inferred
        // from `timestamp` -- that inference is what mis-filed the 9 p.m. report.
        let legacy = json!({
            "timestamp": "2026-08-07T02:28:13Z",
            "open_prs": 10,
        });
        let line = describe_entry(3, &legacy).unwrap();
        assert!(line.contains("covers=unknown"), "{line}");
        assert!(
            !line.contains("covers=2026-08-07T02"),
            "must not guess: {line}"
        );
    }

    #[test]
    fn legacy_rows_still_parse_and_are_not_rewritten() {
        // Every historical shape must survive describe_entry without panicking:
        // the log is append-only and these rows are already on disk.
        for legacy in [
            json!({"timestamp": "2026-08-06T15:05:12Z", "open_prs": 3}),
            json!({"schema_version": 2, "timestamp": "2026-08-07T01:00:36Z", "open_prs": 5,
                   "count_semantics": {"open_prs": {"basis": BASIS_TOTAL_OPEN, "repos": ["rrnewton/hermit"]}}}),
            json!({"schema_version": 3, "timestamp": "2026-08-07T02:14:31Z", "open_prs": 8,
                   "count_semantics": {"open_prs": {"basis": BASIS_TOTAL_OPEN, "repos": ["rrnewton/hermit"]}}}),
            json!({"timestamp": "2026-08-07T02:28:13Z"}),
        ] {
            let line = describe_entry(0, &legacy).unwrap();
            assert!(
                line.contains("covers=unknown pre-schema-4-entry"),
                "every pre-schema-{COVERAGE_SCHEMA} row reads unknown: {line}"
            );
        }
        // Schema 3 is BELOW the coverage schema, so it is legacy for coverage
        // purposes even though it is current for the mapping rules.
        assert!(COUNT_SEMANTICS_SCHEMA < COVERAGE_SCHEMA);
        assert!(SCHEMA_VERSION > COVERAGE_SCHEMA);
    }
    // ============ the count must be bound to the set it names ============

    fn repos2() -> Vec<String> {
        vec![
            "rrnewton/hermit".to_string(),
            "rrnewton/reverie".to_string(),
        ]
    }

    #[test]
    fn a_subset_may_not_wear_the_total_open_basis() {
        // THE MEASURED DEFECT. Entry 2026-08-07T02:55:39Z recorded open_prs=7
        // under total-open-including-drafts over these two repositories while
        // the true population was 102 (105 when this guard was written). The 7
        // was ci-hub's monitored rollup: a different population in the total's
        // clothing, and ~15x smaller.
        let err = check_open_prs(7, 105, BASIS_TOTAL_OPEN, &repos2(), None)
            .expect_err("a subset under the total basis must be refused");
        assert!(err.contains("does not match"), "{err}");
        assert!(err.contains("observed 105"), "{err}");
        // The remedy must be named, not merely the refusal.
        assert!(
            err.contains("--ready-prs"),
            "the message must point at the subset field: {err}"
        );
        assert!(err.contains("may not borrow open_prs"), "{err}");
    }

    #[test]
    fn the_true_total_records() {
        // Positive control: the guard is not simply refusing everything.
        assert!(check_open_prs(105, 105, BASIS_TOTAL_OPEN, &repos2(), None).is_ok());
        assert!(check_open_prs(0, 0, BASIS_TOTAL_OPEN, &repos2(), None).is_ok());
    }

    #[test]
    fn off_by_one_in_either_direction_is_refused() {
        // The check is equality, not a tolerance. A count "about right" is the
        // same class of claim as a subset wearing the total's label.
        for (declared, observed) in [(104u64, 105u64), (106, 105), (1, 0), (0, 1)] {
            assert!(
                check_open_prs(declared, observed, BASIS_TOTAL_OPEN, &repos2(), None).is_err(),
                "declared {declared} vs observed {observed} must refuse"
            );
        }
    }

    #[test]
    fn the_refusal_names_the_repositories_and_their_per_repo_counts() {
        // A bare total tells the reader nothing about WHICH repository drifted.
        let per_repo = vec![
            ("rrnewton/hermit".to_string(), 96u64),
            ("rrnewton/reverie".to_string(), 9u64),
        ];
        let err = check_open_prs(7, 105, BASIS_TOTAL_OPEN, &repos2(), Some(&per_repo))
            .expect_err("must refuse");
        assert!(err.contains("rrnewton/hermit=96"), "{err}");
        assert!(err.contains("rrnewton/reverie=9"), "{err}");
        assert!(
            err.contains("rrnewton/hermit,rrnewton/reverie"),
            "the declared set: {err}"
        );

        // Without a breakdown it must say where the number came from rather
        // than implying a lookup that did not happen.
        let injected = check_open_prs(7, 105, BASIS_TOTAL_OPEN, &repos2(), None).unwrap_err();
        assert!(injected.contains("--open-prs-observed"), "{injected}");
    }

    // ============ auto mode: one observation, adopted not re-derived ============

    #[test]
    fn auto_is_exclusive_with_every_way_of_supplying_the_number() {
        // Each of these would record a value whose provenance is a lie.
        let cases: Vec<(&str, bool, Option<u64>, bool, &str, Option<u64>)> = vec![
            (
                "auto plus an asserted number",
                true,
                Some(99),
                false,
                BASIS_TOTAL_OPEN,
                None,
            ),
            (
                "auto plus --no-verify-counts",
                true,
                None,
                true,
                BASIS_TOTAL_OPEN,
                None,
            ),
            (
                "auto on a non-total basis",
                true,
                None,
                false,
                BASIS_READY,
                None,
            ),
            (
                "auto plus a supplied observation",
                true,
                None,
                false,
                BASIS_TOTAL_OPEN,
                Some(50),
            ),
        ];
        for (label, auto, declared, no_verify, basis, supplied) in cases {
            assert!(
                validate_auto_mode(auto, declared, no_verify, basis, supplied).is_err(),
                "{label} must be refused"
            );
        }
        // Positive controls: plain auto, and every non-auto shape, are fine.
        assert!(validate_auto_mode(true, None, false, BASIS_TOTAL_OPEN, None).is_ok());
        assert!(validate_auto_mode(false, Some(105), false, BASIS_TOTAL_OPEN, None).is_ok());
        assert!(validate_auto_mode(false, Some(7), true, BASIS_TOTAL_OPEN, None).is_ok());
        assert!(validate_auto_mode(false, Some(7), false, BASIS_READY, Some(7)).is_ok());
    }

    #[test]
    fn an_explicit_count_is_still_checked_after_auto_exists() {
        // Adding an adopt path must not soften the assert path: a subset under
        // the total basis is still refused, which is the whole point of 9fdb9ad.
        let repos = vec![
            "rrnewton/hermit".to_string(),
            "rrnewton/reverie".to_string(),
        ];
        assert!(check_open_prs(7, 106, BASIS_TOTAL_OPEN, &repos, None).is_err());
        assert!(check_open_prs(106, 106, BASIS_TOTAL_OPEN, &repos, None).is_ok());
    }

    #[test]
    fn the_truncation_bound_refuses_at_or_above_the_limit() {
        // A truncated list UNDERCOUNTS, and an undercount drifts toward a
        // subset -- so truncation would manufacture agreement with exactly the
        // wrong number. Refuse rather than trust the length.
        assert!(check_not_truncated(999, 1000, "rrnewton/hermit").is_ok());
        assert!(check_not_truncated(1000, 1000, "rrnewton/hermit").is_err());
        assert!(check_not_truncated(1001, 1000, "rrnewton/hermit").is_err());
        let err = check_not_truncated(1000, 1000, "rrnewton/hermit").unwrap_err();
        assert!(err.contains("UNDERSTATE"), "{err}");
        assert!(
            err.contains("rrnewton/hermit"),
            "the message must name the repo: {err}"
        );
        // The argument passed to gh and the bound compared against must agree,
        // or the check is off by whatever they differ by.
        assert_eq!(GH_PR_LIST_LIMIT_ARG, GH_PR_LIST_LIMIT.to_string());
    }

    #[test]
    fn an_observation_carries_the_window_it_spans() {
        // A multi-repository total is a sum over separate queries, so it spans
        // a window rather than an instant. The row must not claim precision the
        // measurement does not have.
        let obs = Observation {
            total: 106,
            per_repo: vec![
                ("rrnewton/hermit".to_string(), 97),
                ("rrnewton/reverie".to_string(), 9),
            ],
            window_start: "2026-08-07T03:50:41Z".to_string(),
            window_end: "2026-08-07T03:50:42Z".to_string(),
        };
        assert_eq!(obs.total, obs.per_repo.iter().map(|(_, n)| n).sum::<u64>());
        assert!(obs.window_start <= obs.window_end);
    }
}
