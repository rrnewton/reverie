#!/usr/bin/env rust-script
//! Collect the Hermit cross-backend **compatibility envelope** into the
//! per-run observation CSV, as a side effect of running the e2e regression/expansion
//! suite. This lives in the OUTER dev-hermit repo on purpose: the CSV is
//! coordination/measurement state and must not add noise to hermit/reverie.
//!
//! It does NOT reimplement the e2e runner. It *drives* the canonical harness
//! (`hermit/ci/test_harness.sh`) and *consumes* the machine-readable JSONL it
//! already emits (one record per cell: outcome, backend, mode, duration_ms,
//! hermit_sha, ...). Two modes:
//!
//!   * `--mode regression` — run only the known-green (manifest `backends_enabled`,
//!     `ci=true`) cells and assert they stay green. This is what `validate`/CI
//!     calls; the CSV is the byproduct recording exactly what was green.
//!   * `--mode expansion` — run the FULL superset including currently-disabled /
//!     failing cells, to catch failing->passing flips (compat growth). Every
//!     such cell is boxed with a per-cell wall-time + memory budget (see
//!     `expansion-dag.rs`, which turns this same envelope into a fine-grained
//!     safe-ci-dag-runner DAG). Here `--mode expansion` runs them serially with
//!     `timeout(1)`; use `expansion-dag.rs` for the cgroup-boxed parallel run.
//!
//! Determinism vs parity:
//!   * determinism = a TWO-RUN comparison observed run1==run2. Only `verify`
//!     executes a second run, so only a passing `verify` cell earns a value;
//!     `strict`/`chaos`/`custom`/`replay` are single runs and are recorded BLANK
//!     (unmeasured), never 1. `verify_compare` records what the two runs were
//!     compared BY -- `stripped` normalises addresses/tmp paths and does not
//!     compare the detlog, so it is weaker than bitwise and must stay legible;
//!   * parity = the backend's guest output is bitwise-identical to the ptrace
//!     reference. We compute it by capturing guest stdout under ptrace and under
//!     the backend and comparing SHA-256 (only for reconstructable guest commands
//!     — `.sh`(--run) fixtures and `direct` commands; enable with `--with-parity`).
//!
//! Usage:
//!   compat-envelope/collect-envelope.rs --mode regression|expansion
//!       [--lane portable|privileged] [--buckets b1,b2,...] [--backends ...]
//!       [--with-parity] [--csv PATH] [--repo PATH] [--run-id ID] [--dry-run]
//!
//! ```cargo
//! [dependencies]
//! detcore-model = { path = "../hermit/detcore-model" }
//! serde_json = "1"
//! sha2 = "0.10"
//! ```

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
#[path = "lib/scorecard.rs"]
mod scorecard;

use detcore_model::backend_engagement::{BackendEngagement, BackendEngagementReport};
use detcore_model::summary::PathEvidence;
use detcore_model::summary::RunSummary;
use scorecard::{
    parse_columns, split_csv_line, ScorecardColumn as Column, ScorecardRow, ScorecardVariant,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = r#"Usage: compat-envelope/collect-envelope.rs --mode regression|expansion [OPTIONS]

Drive hermit/ci/test_harness.sh and record the cross-backend compat envelope to CSV.

Options:
  --mode M          regression (known-green, assert) or expansion (full superset).
  --lane L          portable | privileged (default: portable).
  --buckets LIST    Comma-separated manifest buckets (default: all in the lane).
  --backends LIST   Comma-separated backends to measure (default: all in plan).
  --with-parity     Capture guest stdout per backend and compare to ptrace ref.
  --csv PATH        Exact per-run observation path (default: ignored/envelope/).
  --repo PATH       hermit checkout (default: ../hermit relative to this script,
                    else $DEV_HERMIT/hermit).
  --run-id ID       Override the run id (default: unix-seconds).
  --assert-green    Exit 1 if any enabled (known-green) cell no longer passes
                    (the `validate`/CI whole-envelope gate). Regression mode.
  --dry-run         Enumerate + print what would run; touch nothing.
  -h, --help        Show this help.
"#;

const DEFAULT_CSV_TEMPLATE: &str = "ignored/envelope/<run_id>.csv";

fn die(msg: &str) -> ! {
    eprintln!("collect-envelope: {msg}\n\n{USAGE}");
    exit(2);
}

struct Meta {
    run_id: String,
    run_utc: String,
    hermit_sha: String,
    reverie_sha: String,
    dirty: bool,
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn exact_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Resolve the Reverie revision that the measured Hermit binary actually uses.
/// A sibling checkout is only correlated state; Cargo.lock is the dependency
/// authority.  More than one locked Reverie revision is an ambiguous build and
/// therefore not qualifying provenance.
fn locked_reverie_sha(repo: &Path) -> Option<String> {
    let lock = fs::read_to_string(repo.join("Cargo.lock")).ok()?;
    let mut revisions = BTreeSet::new();
    for line in lock
        .lines()
        .filter(|line| line.contains("github.com/rrnewton/reverie.git"))
    {
        let Some((_, suffix)) = line.rsplit_once('#') else {
            continue;
        };
        let sha: String = suffix
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if sha.len() == 40 {
            revisions.insert(sha.to_ascii_lowercase());
        }
    }
    (revisions.len() == 1).then(|| revisions.into_iter().next().unwrap())
}

fn population_id(cells: &[PlanCell], mode: &str, lane: &str) -> String {
    let mut keys: Vec<String> = cells
        .iter()
        .map(|cell| {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                mode, lane, cell.bucket, cell.test, cell.mode, cell.backend, cell.cell_state
            )
        })
        .collect();
    keys.sort();
    let mut hash = Sha256::new();
    hash.update(b"scorecard-population-v2\n");
    for key in keys {
        hash.update(key.as_bytes());
        hash.update(b"\n");
    }
    format!("sha256:{:x}", hash.finalize())
}

fn profile_flags(lane: &str, bucket: &str, backend: &str, mode: &str) -> String {
    let harness_mode = if mode == "regression" {
        "--ci-only"
    } else {
        "--include-manual"
    };
    let mut comparison = vec![
        "run".to_string(),
        "--backend".to_string(),
        backend.to_string(),
        "--strict".to_string(),
        "--summary".to_string(),
        "--base-env".to_string(),
        "minimal".to_string(),
        "-e".to_string(),
        "LC_ALL=C".to_string(),
        "-e".to_string(),
        "TZ=UTC".to_string(),
    ];
    if lane == "portable" {
        comparison.extend([
            "--no-virtualize-cpuid".to_string(),
            "--max-timeslice=disabled".to_string(),
        ]);
    }
    comparison.push("--".to_string());
    serde_json::json!({
        "harness": ["run", "--lane", lane, "--category", bucket, "--backend", backend, harness_mode],
        "comparison": comparison,
    })
    .to_string()
}

fn main() {
    let mut mode: Option<String> = None;
    let mut lane = "portable".to_string();
    let mut buckets_arg: Option<String> = None;
    let mut backends_arg: Option<String> = None;
    let mut with_parity = false;
    let mut csv: Option<PathBuf> = None;
    let mut repo: Option<PathBuf> = None;
    let mut run_id_arg: Option<String> = None;
    let mut dry_run = false;
    let mut assert_green = false;

    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                exit(0);
            }
            "--mode" => mode = Some(it.next().unwrap_or_else(|| die("--mode needs a value"))),
            "--lane" => lane = it.next().unwrap_or_else(|| die("--lane needs a value")),
            "--buckets" => {
                buckets_arg = Some(it.next().unwrap_or_else(|| die("--buckets needs a list")))
            }
            "--backends" => {
                backends_arg = Some(it.next().unwrap_or_else(|| die("--backends needs a list")))
            }
            "--self-check" => std::process::exit(self_check()),
            // Score a REAL `--verify-json` file through the same parser the
            // collector uses. The self-check exercises that parser against
            // synthetic records; this binds it to actual hermit output, so a
            // shape drift in hermit's verdict is observable rather than
            // silently degrading every row to the uncounted fallback.
            "--verdict-file" => {
                let path = it
                    .next()
                    .unwrap_or_else(|| die("--verdict-file needs a path"));
                match fs::read_to_string(&path)
                    .ok()
                    .as_deref()
                    .and_then(verify_tier_from_json)
                {
                    Some(v) => {
                        println!(
                            "tier={} verify_compare={} bitwise_parity={} compared_log_messages={} infrastructure_error={}",
                            v.tier,
                            v.verify_compare,
                            v.bitwise_parity,
                            v.compared_log_messages,
                            v.infrastructure_error.as_deref().unwrap_or("")
                        );
                        exit(0);
                    }
                    None => {
                        println!("no-verdict (fallback to stripped-uncounted): {path}");
                        exit(1);
                    }
                }
            }
            "--with-parity" => with_parity = true,
            "--csv" => {
                csv = Some(PathBuf::from(
                    it.next().unwrap_or_else(|| die("--csv needs a path")),
                ))
            }
            "--repo" => {
                repo = Some(PathBuf::from(
                    it.next().unwrap_or_else(|| die("--repo needs a path")),
                ))
            }
            "--run-id" => {
                run_id_arg = Some(it.next().unwrap_or_else(|| die("--run-id needs a value")))
            }
            "--dry-run" => dry_run = true,
            "--assert-green" => assert_green = true,
            other => die(&format!("unknown argument {other}")),
        }
    }
    let mode = mode.unwrap_or_else(|| die("--mode is required"));
    if mode != "regression" && mode != "expansion" {
        die("--mode must be regression or expansion");
    }

    let here = script_dir();
    let repo = repo
        .or_else(|| {
            let candidate = here.join("..").join("hermit");
            candidate.is_dir().then(|| candidate)
        })
        .or_else(|| {
            env::var("DEV_HERMIT")
                .ok()
                .map(|d| PathBuf::from(d).join("hermit"))
        })
        .unwrap_or_else(|| die("could not locate hermit checkout; pass --repo"));
    let repo = fs::canonicalize(&repo).unwrap_or(repo);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let meta = Meta {
        run_id: run_id_arg.unwrap_or_else(|| now.to_string()),
        run_utc: format!("@{now}"),
        hermit_sha: git(&repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into()),
        reverie_sha: locked_reverie_sha(&repo).unwrap_or_else(|| "unknown".into()),
        dirty: git(&repo, &["status", "--porcelain"])
            .map(|s| !s.is_empty())
            .unwrap_or(false),
    };
    let explicit_csv = csv.is_some();
    let csv_path = csv.unwrap_or_else(|| {
        let _output_class = DEFAULT_CSV_TEMPLATE;
        here.join("ignored")
            .join("envelope")
            .join(format!("{}.csv", meta.run_id))
    });
    let canonical = here.join("scorecard.csv");
    let requested_resolved = fs::canonicalize(&csv_path).unwrap_or_else(|_| csv_path.clone());
    let canonical_resolved = fs::canonicalize(&canonical).unwrap_or_else(|_| canonical.clone());
    if requested_resolved == canonical_resolved {
        die("refusing to append the tracked current scorecard; publish a reviewed per-run artifact through publish-scorecard.py");
    }
    if with_parity && (!exact_sha(&meta.hermit_sha) || !exact_sha(&meta.reverie_sha) || meta.dirty)
    {
        die(&format!(
            "parity requires clean exact code provenance; hermit_sha={:?} reverie_sha={:?} dirty={}",
            meta.hermit_sha, meta.reverie_sha, meta.dirty
        ));
    }

    let backends_filter: Option<Vec<String>> =
        backends_arg.map(|l| l.split(',').map(|s| s.trim().to_string()).collect());
    let buckets_filter: Option<Vec<String>> =
        buckets_arg.map(|l| l.split(',').map(|s| s.trim().to_string()).collect());

    // Enumerate the authoritative cell plan from the harness.
    let plan = enumerate_plan(&repo, &lane, &mode);
    let cells: Vec<PlanCell> = plan
        .into_iter()
        .filter(|c| {
            buckets_filter
                .as_ref()
                .map_or(true, |b| b.contains(&c.bucket))
        })
        .filter(|c| {
            backends_filter
                .as_ref()
                .map_or(true, |b| b.contains(&c.backend))
        })
        .collect();
    let population_id = population_id(&cells, &mode, &lane);
    let planned_count = cells.len();

    eprintln!(
        "collect-envelope: mode={mode} lane={lane} cells={} hermit={} dirty={}",
        cells.len(),
        &meta.hermit_sha[..meta.hermit_sha.len().min(12)],
        meta.dirty
    );

    if dry_run {
        let mut by_backend: BTreeMap<String, usize> = BTreeMap::new();
        for c in &cells {
            *by_backend.entry(c.backend.clone()).or_default() += 1;
        }
        for (b, n) in &by_backend {
            eprintln!("  {b}: {n} cells");
        }
        eprintln!("(dry-run: nothing executed, CSV untouched)");
        return;
    }

    // Ensure the CSV exists with a header.
    if !csv_path.exists() {
        if let Some(parent) = csv_path.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|e| die(&format!("cannot create observation directory: {e}")));
        }
        fs::write(&csv_path, format!("{}\n", ScorecardVariant::Run.header()))
            .unwrap_or_else(|e| die(&format!("cannot create CSV: {e}")));
    }
    // Bind to the target's declared projection of the shared row. The per-run
    // artifact carries collector-only fields, while publication projects the
    // same ScorecardRow onto scorecard.csv without positional reconstruction.
    let target_header: Vec<String> = {
        let first = fs::read_to_string(&csv_path)
            .unwrap_or_else(|e| die(&format!("cannot read CSV header: {e}")));
        let line = first.lines().next().unwrap_or("").to_string();
        split_csv_line(&line).unwrap_or_else(|error| die(&format!("invalid CSV header: {error}")))
    };
    let target_columns = parse_columns(&target_header, Some(Column::StdoutParity))
        .unwrap_or_else(|error| die(&format!("invalid CSV header: {error}")));
    // A provenance column is not optional merely because a legacy target header
    // omitted it.  Dropping one would recreate the exact condition/value split
    // this producer is responsible for repairing.
    for required in [
        Column::RunId,
        Column::RunUtc,
        Column::HermitSha,
        Column::ReverieSha,
        Column::TestId,
        Column::Backend,
        Column::Outcome,
        Column::Deterministic,
        Column::RefOutputHash,
        Column::ParityComparator,
        Column::ParityTier,
        Column::ProfileFlags,
        Column::PopulationId,
        Column::SelectedCount,
        Column::ExecutedCount,
        Column::EvidenceCount,
        Column::ComparisonTier,
    ] {
        if !target_columns.contains(&required) {
            die(&format!(
                "scorecard {} is missing the required column {:?}; its header has {} \
                 column(s): {}",
                csv_path.display(),
                required.name(),
                target_header.len(),
                target_header.join(",")
            ));
        }
    }
    // Run each (bucket,backend) group through the harness and read its JSONL.
    let mut groups: BTreeMap<(String, String), Vec<PlanCell>> = BTreeMap::new();
    for c in cells {
        groups
            .entry((c.bucket.clone(), c.backend.clone()))
            .or_default()
            .push(c);
    }

    // Probe backend availability once so an absent backend binary (e.g. SaBRe
    // not built in this checkout) is recorded as UNAVAILABLE — an honest
    // "not measured", never a confirmed compat/determinism fail. Anti-fakery: a
    // missing backend must not masquerade as a red backend.
    let mut avail: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (_, backend) in groups.keys() {
        avail
            .entry(backend.clone())
            .or_insert_with(|| backend_unavailable_reason(&repo, backend));
    }
    for (b, why) in &avail {
        if let Some(reason) = why {
            eprintln!(
                "collect-envelope: backend `{b}` UNAVAILABLE here — cells recorded as unavailable (not fail): {reason}"
            );
        }
    }

    let mut pending_rows: Vec<(ScorecardRow, bool, bool)> = Vec::new();
    let mut regressions: Vec<String> = Vec::new();
    for ((bucket, backend), group) in &groups {
        let unavailable_reason = avail.get(backend).and_then(|r| r.clone());
        let available = unavailable_reason.is_none();
        let results = if available {
            run_group(&repo, &lane, bucket, backend, &mode)
        } else {
            Vec::new()
        };
        // Index harness outcomes by (test, mode).
        let mut outcomes: BTreeMap<(String, String), HarnessResult> = BTreeMap::new();
        for r in results {
            outcomes.insert((r.test.clone(), r.mode.clone()), r);
        }
        for c in group {
            let hr = outcomes.get(&(c.test.clone(), c.mode.clone()));
            let executed = hr.is_some();
            let (mut outcome, duration_ms, mut reason) = if !available {
                (
                    "unavailable".to_string(),
                    0i64,
                    unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "backend unavailable (reason not captured)".to_string()),
                )
            } else {
                match hr {
                    Some(r) => (r.outcome.clone(), r.duration_ms, r.reason.clone()),
                    None => ("skip".to_string(), 0i64, "no harness record".to_string()),
                }
            };
            let ran_two_run_comparison = c.mode == "verify";
            // Read the harness attempt's own receipt on non-passing rows. A
            // successful row is still re-run with --verify-strict below because
            // the ordinary harness comparison may be weaker; a failed row must
            // not be ignored merely because it is not eligible for that rerun.
            let verdict = if ran_two_run_comparison && available && outcome != "skip" {
                if outcome == "pass" {
                    capture_verify_verdict(&repo, &lane, backend, c)
                } else {
                    hr.and_then(harness_verify_verdict)
                }
            } else {
                None
            };
            if let Some(infrastructure_error) = verdict
                .as_ref()
                .and_then(|verdict| verdict.infrastructure_error.as_ref())
            {
                outcome = "error".to_string();
                reason = infrastructure_error.clone();
            }
            let pass = outcome == "pass";
            // A known-green (enabled) cell that no longer passes is a regression —
            // but an unavailable backend or a named infrastructure error is
            // environmental, never a product regression.
            if is_regression(&outcome, available, &c.cell_state) {
                regressions.push(format!(
                    "{bucket}/{}::{} [{backend}] -> {outcome} ({reason})",
                    c.test, c.mode
                ));
            }
            // DETERMINISM MUST BE EARNED BY A TWO-RUN COMPARISON, never inferred
            // from a single passing run.
            //
            // The previous rule was `pass => deterministic=1` for EVERY mode. Only
            // `verify` actually executes a second run and compares it; `strict`,
            // `chaos`, `custom` and `replay` are single runs that cannot observe
            // run1==run2 at all. On the live scorecard that rule minted
            // deterministic=1 for 105 single-run cells (strict 102, chaos 1,
            // custom 1, replay 1) against 63 that genuinely compared — i.e. most
            // of the determinism evidence was an artifact of the rule, not a
            // measurement. A single run passing says the program worked once; it
            // is silent about reproducibility.
            //
            // BLANK means UNMEASURED and is the honest answer for a single run.
            // It is deliberately NOT `0`: `0` asserts observed nondeterminism,
            // which a single run cannot establish either.
            let deterministic =
                deterministic_from_harness(&outcome, available, ran_two_run_comparison);
            // WHAT the two runs were compared BY, read from hermit's OWN typed
            // verdict instead of assumed from the flag or scraped from the banner.
            // `detcore::logdiff` is the one real comparator in the system and this
            // scorecard never called it: the banner
            // `":: Success: deterministic. Determinism verified."` is printed by a
            // run whose own `--verify-json` says `bitwise_parity:false`, so the
            // banner cannot tell a stripped match from a bitwise one. `--verify-json`
            // carries the comparison's CONDITIONS beside its result, so read that.
            // FALLBACK, retained deliberately: some backends produce no verdict at
            // all (DBI accepts `--verify-json` and writes nothing -- measured rc=0,
            // no file), so the absent-JSON path is a REAL case, not a defensive
            // branch. `stripped-uncounted` states the comparator that ran and ADMITS
            // the missing count in its name, because the wired verifier requires a
            // count from anything claiming `stripped`: a log comparison that cannot
            // say how much it compared could have compared nothing. Blank parity and
            // counts mean NOT RECORDED; a `0` would assert a measurement never taken.
            let (verify_compare, tier, bitwise_parity, compared_log_messages) = match &verdict {
                Some(v) => (
                    v.verify_compare.clone(),
                    v.tier.clone(),
                    v.bitwise_parity.clone(),
                    v.compared_log_messages.clone(),
                ),
                None if deterministic == Some(true) && ran_two_run_comparison => (
                    "stripped".to_string(),
                    "stripped-uncounted".to_string(),
                    String::new(),
                    String::new(),
                ),
                None => (String::new(), String::new(), String::new(), String::new()),
            };
            let (
                parity,
                parity_exercised,
                native_output_hash,
                output_hash,
                ref_output_hash,
                backend_engaged,
                parity_failure,
            ) = if with_parity && pass {
                capture_parity(&repo, &lane, c, backend)
            } else {
                (
                    None,
                    None,
                    String::new(),
                    String::new(),
                    String::new(),
                    None,
                    String::new(),
                )
            };
            // NOT-EXERCISED is a THIRD bucket, not a downgrade of pass/fail. A cell whose guest
            // produces the same bytes with and without hermit did not exercise the property the
            // cell claims to measure, so reporting it as parity asserts coverage that does not
            // exist. The verdict is RECLASSIFIED, never dropped: `parity` keeps its measured
            // value so the row stays auditable, and `outcome` names the vacuity.
            // The SECOND, orthogonal not-exercised condition: DID THIS BACKEND ENGAGE.
            // The check above asks whether the GUEST exercised anything hermit
            // virtualizes. It cannot see a backend that did nothing, because the shared
            // runtime underneath still determinizes the guest: e9patch reporting
            // mapped_sites=0 still differs from native, so it passes the guest test and
            // then scores byte-identical parity with ptrace -- a manufactured perfect
            // score for a backend that never ran. Zero is a THIRD BUCKET here too:
            // never a pass, never a failure, excluded from the parity number with its
            // reason, because a libc-only guest legitimately gives a main-ELF patcher
            // nothing to do.
            let engagement_vacuous = backend_engaged == Some(0);
            let outcome = if !parity_failure.is_empty() {
                "fail".to_string()
            } else if parity_exercised == Some(false) || engagement_vacuous {
                "not-exercised".to_string()
            } else {
                outcome
            };
            let reason = if !parity_failure.is_empty() {
                parity_failure.clone()
            } else if engagement_vacuous {
                format!(
                    "{backend} performed no work on this guest ({}=0); excluded from parity \
                     rather than scored, because a backend that does nothing agrees with the \
                     reference perfectly",
                    engagement_counter_name(backend)
                )
            } else {
                reason
            };
            if !parity_failure.is_empty() && c.cell_state == "enabled" {
                regressions.push(format!(
                    "{bucket}/{}::{} [{backend}] -> fail ({parity_failure})",
                    c.test, c.mode
                ));
            }
            let has_parity_evidence = parity.is_some();
            let parity_comparator = if has_parity_evidence {
                "stdout-sha256-exact-v1"
            } else {
                ""
            };
            let parity_tier = if has_parity_evidence {
                "stdout-exact"
            } else {
                ""
            };
            let profile_flags = profile_flags(&lane, bucket, backend, &mode);
            let row = ScorecardRow::from_pairs([
                (Column::RunId, meta.run_id.clone()),
                (Column::RunUtc, meta.run_utc.clone()),
                (Column::HermitSha, meta.hermit_sha.clone()),
                (Column::ReverieSha, meta.reverie_sha.clone()),
                (Column::Dirty, meta.dirty.to_string()),
                (Column::RunMode, mode.clone()),
                (Column::Lane, lane.clone()),
                (Column::Bucket, bucket.clone()),
                (Column::TestId, c.test.clone()),
                (Column::TestMode, c.mode.clone()),
                (Column::Backend, backend.clone()),
                (Column::CellState, c.cell_state.clone()),
                (Column::Outcome, outcome),
                (
                    Column::Deterministic,
                    deterministic
                        .map(|value| if value { "1" } else { "0" })
                        .unwrap_or("")
                        .to_string(),
                ),
                (
                    Column::StdoutParity,
                    parity
                        .map(|value| if value { "1" } else { "0" })
                        .unwrap_or("")
                        .to_string(),
                ),
                (
                    Column::ParityExercised,
                    parity_exercised
                        .map(|value| if value { "1" } else { "0" })
                        .unwrap_or("")
                        .to_string(),
                ),
                // THE COUNT TRAVELS WITH THE CELL. Blank means no counter was found
                // for this backend, which is not the same as zero and must not be read
                // as engagement.
                (
                    Column::BackendEngaged,
                    backend_engaged
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                (Column::NativeOutputHash, native_output_hash),
                (Column::OutputHash, output_hash),
                (Column::RefOutputHash, ref_output_hash),
                (Column::DurationMs, duration_ms.to_string()),
                (Column::Reason, reason),
                (Column::VerifyCompare, verify_compare.to_string()),
                (Column::BitwiseParity, bitwise_parity.to_string()),
                (
                    Column::ComparedLogMessages,
                    compared_log_messages.to_string(),
                ),
                (Column::Tier, tier.to_string()),
                (Column::ParityComparator, parity_comparator.to_string()),
                (Column::ParityTier, parity_tier.to_string()),
                (Column::ProfileFlags, profile_flags),
                (Column::PopulationId, population_id.clone()),
                (
                    Column::ComparisonTier,
                    "unqualified-stdout-only".to_string(),
                ),
                (Column::StackParity, "0".to_string()),
                (Column::HeapParity, "0".to_string()),
            ]);
            pending_rows.push((row, executed, has_parity_evidence));
        }
    }
    let executed_count = pending_rows
        .iter()
        .filter(|(_, executed, _)| *executed)
        .count();
    let evidence_count = pending_rows
        .iter()
        .filter(|(_, _, evidence)| *evidence)
        .count();
    if pending_rows.len() != planned_count {
        die(&format!(
            "selected population changed while running: planned={planned_count} rows={}",
            pending_rows.len()
        ));
    }
    let mut out = fs::OpenOptions::new()
        .append(true)
        .open(&csv_path)
        .unwrap_or_else(|e| die(&format!("cannot open CSV for append: {e}")));
    for (mut row, _, _) in pending_rows {
        row.selected_count = planned_count.to_string();
        row.executed_count = executed_count.to_string();
        row.evidence_count = evidence_count.to_string();
        let line = row.encode_columns(&target_columns);
        writeln!(out, "{line}").unwrap_or_else(|e| die(&format!("CSV write failed: {e}")));
    }
    eprintln!(
        "collect-envelope: wrote {} rows to {} (executed={executed_count} parity_evidence={evidence_count} population={population_id})",
        planned_count,
        csv_path.display()
    );
    if !explicit_csv {
        eprintln!(
            "collect-envelope: current scorecard unchanged; after review publish with: python3 {} --observation {} --current {} --history {}",
            here.join("publish-scorecard.py").display(),
            csv_path.display(),
            canonical.display(),
            here.join("history/scorecard-observations.csv").display(),
        );
    }

    // Whole-envelope green gate (owner: `validate`/CI asserts every known-green
    // cell stayed green as a side effect of writing the CSV).
    if assert_green && !regressions.is_empty() {
        eprintln!(
            "collect-envelope: REGRESSION — {} known-green cell(s) no longer pass:",
            regressions.len()
        );
        for r in &regressions {
            eprintln!("  FAIL {r}");
        }
        exit(1);
    }
    if assert_green {
        eprintln!(
            "collect-envelope: RAW EXECUTION PASS — all enabled cells passed, but every new row is comparison_tier=unqualified-stdout-only; emitted strict greens: 0/{}",
            planned_count
        );
    }
}

#[derive(Clone)]
struct PlanCell {
    bucket: String,
    test: String,
    mode: String,
    backend: String,
    cell_state: String, // enabled | disabled
}

struct HarnessResult {
    test: String,
    mode: String,
    outcome: String,
    duration_ms: i64,
    reason: String,
    verification_report: Option<String>,
}

fn is_regression(outcome: &str, available: bool, cell_state: &str) -> bool {
    outcome != "pass" && outcome != "error" && available && cell_state == "enabled"
}

fn deterministic_from_harness(
    outcome: &str,
    available: bool,
    ran_two_run_comparison: bool,
) -> Option<bool> {
    if !available || matches!(outcome, "skip" | "error") {
        None
    } else if outcome != "pass" {
        Some(false)
    } else if ran_two_run_comparison {
        Some(true)
    } else {
        None
    }
}

/// Enumerate the authoritative plan. Regression = enabled ci cells
/// (`test_harness.sh plan`); expansion = enabled + disabled (`plan` + `audit-gaps`).
fn enumerate_plan(repo: &Path, lane: &str, mode: &str) -> Vec<PlanCell> {
    let mut cells = Vec::new();
    let harness = repo.join("ci/test_harness.sh");
    let run = |args: &[&str]| -> Option<Value> {
        let out = Command::new("bash")
            .arg(&harness)
            .args(args)
            .current_dir(repo)
            .output()
            .ok()?;
        if !out.status.success() {
            eprintln!(
                "warn: harness {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    };
    if let Some(Value::Array(rows)) = run(&["plan", "--lane", lane, "--format", "json"]) {
        for r in rows {
            cells.push(PlanCell {
                bucket: r
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                test: r
                    .get("test")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                mode: r
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                backend: r
                    .get("backend")
                    .and_then(Value::as_str)
                    .unwrap_or("native")
                    .to_string(),
                cell_state: "enabled".to_string(),
            });
        }
    }
    if mode == "expansion" {
        if let Some(Value::Array(rows)) = run(&["audit-gaps", "--lane", lane, "--format", "json"]) {
            for r in rows {
                let backend = r
                    .get("backend")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if backend.is_empty() || backend == "native" {
                    continue;
                }
                cells.push(PlanCell {
                    bucket: r
                        .get("category")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    test: r
                        .get("test")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    mode: r
                        .get("mode")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    backend,
                    cell_state: "disabled".to_string(),
                });
            }
        }
    }
    cells.retain(|c| !c.test.is_empty() && c.backend != "native");
    cells
}

/// Run one (bucket,backend) group and parse the results JSONL.
fn run_group(
    repo: &Path,
    lane: &str,
    bucket: &str,
    backend: &str,
    mode: &str,
) -> Vec<HarnessResult> {
    let harness = repo.join("ci/test_harness.sh");
    let results_dir = repo
        .join("ignored/e2e/compat-envelope")
        .join(lane)
        .join(bucket);
    let _ = fs::create_dir_all(&results_dir);
    let results = results_dir.join(format!("{backend}.jsonl"));
    let mut args: Vec<String> = vec![
        "run".into(),
        "--lane".into(),
        lane.into(),
        "--category".into(),
        bucket.into(),
        "--backend".into(),
        backend.into(),
        "--results".into(),
        results.to_string_lossy().into_owned(),
    ];
    if mode == "regression" {
        args.push("--ci-only".into());
    } else {
        args.push("--include-manual".into());
    }
    let status = Command::new("bash")
        .arg(&harness)
        .args(&args)
        .current_dir(repo)
        .status();
    if let Err(e) = status {
        eprintln!("warn: harness run failed for {bucket}/{backend}: {e}");
    }
    let mut out = Vec::new();
    if let Ok(text) = fs::read_to_string(&results) {
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v): Result<Value, _> = serde_json::from_str(line) else {
                continue;
            };
            out.push(HarnessResult {
                test: v
                    .get("test")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                mode: v
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                outcome: v
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase(),
                duration_ms: v.get("duration_ms").and_then(Value::as_i64).unwrap_or(0),
                reason: v
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                verification_report: v
                    .get("attempts")
                    .and_then(Value::as_array)
                    .and_then(|attempts| attempts.last())
                    .and_then(|attempt| attempt.get("verification_report"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
        }
    }
    out
}

/// Capture guest stdout under ptrace and under `backend`; parity = hashes match.
/// Only reconstructable guest commands are attempted (`.sh` fixtures run with
/// `--run`, and `direct` commands); otherwise returns (None, "").
/// Returns `(parity, exercised, native_hash, output_hash, ref_output_hash)`.
///
/// `parity` alone is a POSITIVE-ONLY comparison: it asks "does the candidate match the ptrace
/// golden" and nothing else, so it passes whenever both sides agree -- INCLUDING when they agree
/// because the guest never exercised the determinization under test. A test whose output is the
/// same with and without hermit cannot distinguish a determinizing backend from an inert one, so
/// it scores parity on EVERY backend and the whole row reads green. That is the same shape as an
/// e9patch cell scoring byte-identical parity while reporting `candidate_sites=0`.
///
/// `exercised` supplies the missing negative side by running the guest with NO hermit at all and
/// comparing that host answer to the golden. If the bare host already produces the golden output,
/// hermit changed nothing observable and the cell is NOT-EXERCISED -- a distinct bucket from pass
/// and fail, because the cell is neither evidence of parity nor evidence of a defect.
///
/// The negative control is a BARE EXEC, not `--backend native`: hermit has no `native` backend
/// (`--backend` accepts only ptrace/dbt/liteinst/sabre/kvm/e9patch), so the do-nothing run has to
/// bypass hermit entirely.
///
/// The reference hash was always computed here and then DISCARDED, which left a `parity=1` row
/// unable to say what it matched. A comparison result that does not name both sides is not
/// reproducible evidence: a reader cannot tell a genuine match from a row where both sides were
/// empty, nor re-check the claim later. Both hashes are now recorded.
fn split_run(r: Option<(String, Option<i64>)>) -> (Option<String>, Option<i64>) {
    match r {
        Some((h, e)) => (Some(h), e),
        None => (None, None),
    }
}

/// The four comparison columns, carried together so a tier can never travel
/// without the comparator and counts that justify it.
struct VerifyVerdict {
    tier: String,
    verify_compare: String,
    bitwise_parity: String,
    compared_log_messages: String,
    infrastructure_error: Option<String>,
}

/// The tier a `--verify` run actually EARNED, parsed from the verdict hermit
/// itself wrote. Pure over the JSON text so both directions are testable in
/// `--self-check` without running a guest.
///
/// `bitwise` requires BOTH `bitwise_parity` AND a nonzero compared count on
/// EACH side. The count is not redundant: an empty-vs-empty log comparison
/// reports "no difference" under the strictest possible spec, so without it a
/// run that produced no DETLOG at all would certify as bitwise parity. That is
/// the ambiguous zero these columns exist to rule out.
///
/// Returns `None` when no usable record exists -- an absent file, unparseable
/// text, or a `no_result` verdict, which is a REFUSAL (nothing was compared)
/// and never a result.
fn verify_tier_from_json(text: &str) -> Option<VerifyVerdict> {
    let record: Value = serde_json::from_str(text.trim()).ok()?;
    let record = record.as_object()?;
    match record.get("verdict").and_then(Value::as_str) {
        None | Some("no_result") => return None,
        Some("matched" | "diverged" | "infrastructure_error") => {}
        Some(other) => {
            eprintln!("collect-envelope: REFUSED unknown verification verdict {other:?}");
            return None;
        }
    }
    let infrastructure_error =
        if record.get("verdict").and_then(Value::as_str) == Some("infrastructure_error") {
            let cause = record.get("infrastructure_error")?.as_object()?;
            match (
                cause.get("kind").and_then(Value::as_str),
                cause.get("count").and_then(Value::as_u64),
            ) {
                (Some("skid_overshoot"), Some(count)) if count > 0 => Some(format!(
                    "verification recorded {count} HERMIT_SKID_OVERSHOOT report(s)"
                )),
                _ => return None,
            }
        } else {
            None
        };
    let comparison = record.get("comparison").and_then(Value::as_object);
    let counts = record
        .get("compared_log_messages")
        .and_then(Value::as_object);
    let left = counts.and_then(|c| c.get("left")).and_then(Value::as_i64);
    let right = counts.and_then(|c| c.get("right")).and_then(Value::as_i64);
    let compared = match (left, right) {
        (Some(l), Some(r)) => format!("{l}|{r}"),
        _ => String::new(),
    };
    let strictness = comparison
        .and_then(|c| c.get("strictness"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let verified = record
        .get("verified")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let bitwise = record
        .get("bitwise_parity")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && left.unwrap_or(0) != 0
        && right.unwrap_or(0) != 0;
    let compare_logs = comparison
        .and_then(|c| c.get("compare_logs"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tier = if !verified {
        "gap"
    } else if bitwise {
        "bitwise"
    } else if compare_logs {
        "stripped"
    } else {
        // Verified without comparing the log stream at all: stdout + exit only.
        "guest"
    };
    Some(VerifyVerdict {
        tier: tier.to_string(),
        verify_compare: strictness,
        bitwise_parity: if bitwise {
            "1".to_string()
        } else {
            "0".to_string()
        },
        compared_log_messages: compared,
        infrastructure_error,
    })
}

fn harness_verify_verdict(result: &HarnessResult) -> Option<VerifyVerdict> {
    result
        .verification_report
        .as_deref()
        .and_then(verify_tier_from_json)
}

/// Run this cell's guest under `--verify` and read the verdict hermit wrote.
///
/// `--verify-strict` is what makes `canonical`/`bitwise` REACHABLE at all.
/// Measured on this box against the same guest: plain `--verify` reports
/// `strictness=stripped, bitwise_parity=false` over 236|236 messages, while
/// `--verify-strict` reports `strictness=canonical, bitwise_parity=true` over
/// 112|112. Strict is used HERE, in this measurement collector, and deliberately
/// NOT pushed into hermit's gating `ci/test_harness.sh`: strict rejects
/// divergences the stripped policy tolerates, so flipping the gate would change
/// e2e pass/fail for every cell. This harness only measures, so it can afford
/// the stronger comparison; the gate cannot, absent its own task.
fn capture_verify_verdict(
    repo: &Path,
    lane: &str,
    backend: &str,
    cell: &PlanCell,
) -> Option<VerifyVerdict> {
    let guest = guest_command(repo, &cell.test)?;
    let out_dir = repo.join("ignored/e2e/compat-envelope/verify-json");
    let _ = fs::create_dir_all(&out_dir);
    let slug: String = format!("{}-{}-{}", cell.bucket, cell.test, backend)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let json_path = out_dir.join(format!("{slug}.json"));
    // Remove any stale verdict first: reading a previous run's file would bind
    // this row's tier to a different execution.
    let _ = fs::remove_file(&json_path);
    let mut cmd = Command::new("timeout");
    cmd.arg("120s")
        .arg(hermit_bin(repo))
        .arg("run")
        .arg("--backend")
        .arg(backend)
        .arg("--strict")
        .arg("--verify")
        .arg("--verify-allow")
        .arg("both")
        .arg("--verify-strict")
        .arg(format!("--verify-json={}", json_path.display()))
        .arg("--summary")
        // Same pinned guest environment as `run_and_hash`; see its comment for
        // why an env delta would otherwise move every stack-derived observable.
        .arg("--base-env")
        .arg("minimal")
        .arg("-e")
        .arg("LC_ALL=C")
        .arg("-e")
        .arg("TZ=UTC");
    if lane == "portable" {
        cmd.arg("--no-virtualize-cpuid")
            .arg("--max-timeslice=disabled");
    }
    cmd.arg("--");
    for g in &guest {
        cmd.arg(g);
    }
    cmd.env("LC_ALL", "C").env("TZ", "UTC").current_dir(repo);
    // The run's EXIT STATUS is deliberately not consulted. A verify run that
    // observes a divergence exits non-zero, and that divergence is exactly the
    // measurement wanted here -- gating on rc would discard every negative
    // result and leave only agreements, which is how a column comes to record
    // nothing but greens. The verdict file is the authority.
    let _ = cmd.output().ok()?;
    verify_tier_from_json(&fs::read_to_string(&json_path).ok()?)
}

fn capture_parity(
    repo: &Path,
    lane: &str,
    cell: &PlanCell,
    backend: &str,
) -> (
    Option<bool>,
    Option<bool>,
    String,
    String,
    String,
    Option<i64>,
    String,
) {
    let Some(guest) = guest_command(repo, &cell.test) else {
        return (
            None,
            None,
            String::new(),
            String::new(),
            String::new(),
            None,
            "stdout comparison could not resolve the guest command".to_string(),
        );
    };
    let (ref_hash, _) = split_run(run_and_hash(repo, lane, "ptrace", &guest));
    let (this_hash, engaged) = split_run(run_and_hash(repo, lane, backend, &guest));
    let native_hash = run_native_and_hash(repo, &guest);
    // A cell is EXERCISED when the determinized answer differs from the bare host answer. If they
    // are equal the guest observed nothing hermit virtualizes, so no backend could ever fail it.
    let exercised = match (ref_hash.clone(), native_hash.clone()) {
        (Some(r), Some(n)) => Some(r != n),
        // The bare run failing is not evidence either way (the guest may need the container), so
        // exercise stays UNKNOWN rather than being silently asserted.
        _ => None,
    };
    let native = native_hash.unwrap_or_default();
    match (ref_hash.clone(), this_hash.clone()) {
        (Some(r), Some(t)) => (
            Some(r == t),
            exercised,
            native,
            t,
            r,
            engaged,
            String::new(),
        ),
        _ => (
            None,
            exercised,
            native,
            this_hash.clone().unwrap_or_default(),
            ref_hash.clone().unwrap_or_default(),
            engaged,
            match (ref_hash.is_some(), this_hash.is_some()) {
                (false, false) => {
                    "stdout comparison produced neither reference nor candidate operand"
                }
                (false, true) => "stdout comparison produced no reference operand",
                (true, false) => "stdout comparison produced no candidate operand",
                (true, true) => unreachable!(),
            }
            .to_string(),
        ),
    }
}

/// Run the guest with NO hermit -- the do-nothing control that gives parity its negative side.
/// Mirrors `run_and_hash`'s pinned guest environment so the only difference between the two runs
/// is hermit itself; otherwise an env delta, not determinization, would explain any divergence.
fn run_native_and_hash(repo: &Path, guest: &[String]) -> Option<String> {
    let (prog, args) = guest.split_first()?;
    let mut cmd = Command::new("timeout");
    cmd.arg("120s").arg(prog);
    for a in args {
        cmd.arg(a);
    }
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .current_dir(repo);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let mut h = Sha256::new();
    h.update(&out.stdout);
    Some(format!("{:x}", h.finalize()))
}

/// Resolve a test-id to a runnable guest argv from the harness manifest docs.
fn guest_command(repo: &Path, test_id: &str) -> Option<Vec<String>> {
    let harness = repo.join("ci/test_harness.sh");
    let out = Command::new("cargo")
        .args([
            "run",
            "--quiet",
            "-p",
            "hermit-manifest-plan",
            "--",
            "--format",
            "harness-json",
        ])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        let _ = &harness;
        return None;
    }
    let docs: Value = serde_json::from_slice(&out.stdout).ok()?;
    let arr = docs.as_array()?;
    for doc in arr {
        let tests = doc.get("test").and_then(Value::as_array)?;
        for t in tests {
            if t.get("id").and_then(Value::as_str) != Some(test_id) {
                continue;
            }
            if let Some(prog) = t.get("program").and_then(Value::as_str) {
                if prog.ends_with(".sh") {
                    return Some(vec![
                        repo.join(prog).to_string_lossy().into_owned(),
                        "--run".into(),
                    ]);
                }
                if prog.ends_with(".c") || prog.ends_with(".rs") {
                    // Compiled fixtures: replicate the harness build step
                    // (test_harness.sh cc/rustc invocation) into a stable temp
                    // binary so parity can run the SAME guest under ptrace and the
                    // backend. On any build failure return None so the cell records
                    // parity as UNMEASURED (honest `?`), never a false 0.
                    return build_compiled_fixture(repo, test_id, prog, t.get("build"));
                }
                return None; // unknown program kind
            }
            if let Some(direct) = t.get("direct").and_then(Value::as_str) {
                return Some(vec!["bash".into(), "-c".into(), direct.to_string()]);
            }
        }
    }
    None
}

/// Probe whether `backend` can actually launch a trivial guest in this checkout.
/// Returns false when the backend binary is missing (e.g. SaBRe not built) so
/// its cells are recorded UNAVAILABLE rather than as a compat/determinism fail.
/// ptrace/kvm are in-process; treat them as available (kvm's /dev/kvm gate is a
/// separate runtime concern surfaced by the actual cell run).
/// Returns `None` when the backend is usable, or `Some(reason)` naming why it is not.
///
/// This previously returned a bare bool and every unavailable cell got the same hardcoded string,
/// "backend binary not present in this checkout" -- a constant masquerading as a diagnosis. It was
/// also frequently WRONG: a backend that is built but refuses to load reported "not present". The
/// reason is now the observed cause, so an unavailable row says which check failed.
fn backend_unavailable_reason(repo: &Path, backend: &str) -> Option<String> {
    if backend == "ptrace" || backend == "kvm" || backend == "native" {
        return None;
    }
    let hermit = hermit_bin(repo);
    if !hermit.exists() {
        return Some(format!(
            "hermit binary not built at {} (backend `{backend}` never probed)",
            hermit.display()
        ));
    }
    let out = Command::new("timeout")
        .arg("60s")
        .arg(&hermit)
        .arg("run")
        .arg("--backend")
        .arg(backend)
        .arg("--")
        .arg("/bin/true")
        .current_dir(repo)
        .output();
    match out {
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            // Explicit unavailability markers from the backend loader. Report the marker that
            // matched, so the row records the observed evidence rather than a generic phrase.
            for marker in [
                "is unavailable",
                "was not found",
                "HERMIT_SABRE_BINARY",
                "not found in the Hermit installation",
            ] {
                if err.contains(marker) {
                    let line = err
                        .lines()
                        .find(|l| l.contains(marker))
                        .unwrap_or(marker)
                        .trim();
                    return Some(format!(
                        "backend `{backend}` probe reported unavailable: {}",
                        line.chars().take(160).collect::<String>()
                    ));
                }
            }
            None
        }
        Err(e) => Some(format!(
            "backend `{backend}` probe could not be executed: {e}"
        )),
    }
}

/// Compile a `.c`/`.rs` fixture the way `ci/test_harness.sh` does, into a stable
/// per-test temp binary, and return its argv. `build` is the manifest's optional
/// `build` object (cflags/rustflags). Returns None on any compile failure so the
/// caller records parity as unmeasured rather than a false 0.
fn build_compiled_fixture(
    repo: &Path,
    test_id: &str,
    prog: &str,
    build: Option<&Value>,
) -> Option<Vec<String>> {
    let src = repo.join(prog);
    if !src.exists() {
        return None;
    }
    // Build under the repo output tree, NOT /tmp: hermit replaces guest /tmp with
    // an isolated dir and refuses to launch a guest program that lives there.
    let dir = repo
        .join("ignored/compat-envelope-parity")
        .join(test_id.replace('/', "_"));
    fs::create_dir_all(&dir).ok()?;
    let bin = dir.join("program");
    let extra = |key: &str| -> Vec<String> {
        build
            .and_then(|b| b.get(key))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let ok = if prog.ends_with(".c") {
        let mut c = Command::new("cc");
        c.args(["-std=c11", "-O2", "-g", "-Wall", "-Wextra", "-Werror"]);
        for a in extra("cflags") {
            c.arg(a);
        }
        c.arg(&src).arg("-o").arg(&bin);
        c.status().map(|s| s.success()).unwrap_or(false)
    } else {
        let mut c = Command::new("rustc");
        c.arg("-O");
        for a in extra("rustflags") {
            c.arg(a);
        }
        c.arg(&src).arg("-o").arg(&bin);
        c.status().map(|s| s.success()).unwrap_or(false)
    };
    if !ok {
        return None;
    }
    Some(vec![bin.to_string_lossy().into_owned()])
}

/// Run a guest under one backend with the portable determinism profile, capture
/// stdout, and return its SHA-256 (or None on failure/timeout).
/// How much work THIS backend uniquely performed, read from its typed run record.
///
/// A backend that did nothing agrees with the reference perfectly. e9patch scored
/// byte-identical parity with ptrace while reporting `mapped_sites=0`: the AOT pass
/// rewrote nothing, so plain ptrace ran and the cell recorded a manufactured perfect
/// score. The count is therefore a CONDITION that must travel with the verdict --
/// a cell without it cannot be distinguished from an earned one.
///
/// Each field counts work only that backend does, so a nonzero value cannot be
/// produced by the shared runtime underneath:
///   e9patch  `mapped_sites`  -- main-ELF sites actually rewritten by the AOT pass
///   sabre    `guest_rpc_observed` -- whether the in-guest handler reached Detcore
///   dbt      `counted_branches` -- branches counted by DynamoRIO
///   ptrace   `turns`         -- scheduler turns, i.e. guest stops the tracer took
///
/// `None` means the typed record was missing, malformed, for another backend,
/// or carried no supported counter. That is deliberately not the same as zero.
fn backend_engagement(backend: &str, bytes: &[u8]) -> Option<i64> {
    match backend {
        "ptrace" => serde_json::from_slice::<RunSummary>(bytes)
            .ok()?
            .sched_turns
            .try_into()
            .ok(),
        "sabre" => {
            let evidence = serde_json::from_slice::<PathEvidence>(bytes).ok()?;
            if evidence.schema != PathEvidence::SCHEMA {
                return None;
            }
            Some(i64::from(evidence.guest_rpc_observed))
        }
        "e9patch" | "dbt" => match serde_json::from_slice::<BackendEngagementReport>(bytes)
            .ok()
            .filter(|report| report.validate().is_ok())?
            .engagement
        {
            BackendEngagement::E9patch { mapped_sites } if backend == "e9patch" => {
                mapped_sites.try_into().ok()
            }
            BackendEngagement::Dbt { counted_branches } if backend == "dbt" => {
                counted_branches.try_into().ok()
            }
            BackendEngagement::Ptrace { .. }
            | BackendEngagement::E9patch { .. }
            | BackendEngagement::Dbt { .. } => None,
        },
        // liteinst and kvm publish no engagement counter today.
        _ => None,
    }
}

/// PLANTED BOTH-WAYS CHECK for the engagement invariant, run with `--self-check`.
///
/// The fixtures use the producer-owned record shapes rather than presentation text.
/// Every backend is asserted in BOTH directions:
/// an exercising run must read nonzero, a non-exercising run must read zero, and a
/// backend with no counter must read "unknown" -- which is deliberately distinct from
/// zero, because treating "I could not tell" as "it engaged" is the failure this
/// whole invariant exists to prevent.
fn self_check() -> i32 {
    // (backend, typed record, expected)
    let cases: &[(&str, &str, Option<i64>)] = &[
        // e9patch, the motivating case. Measured: an ordinary dynamically linked guest
        // gives the AOT pass nothing to rewrite, while a guest with syscalls in its main
        // ELF gives it two.
        (
            "e9patch",
            r#"{"schema":1,"engagement":{"backend":"e9patch","mapped_sites":0}}"#,
            Some(0),
        ),
        (
            "e9patch",
            r#"{"schema":1,"engagement":{"backend":"e9patch","mapped_sites":2}}"#,
            Some(2),
        ),
        // dbt
        (
            "dbt",
            r#"{"schema":1,"engagement":{"backend":"dbt","counted_branches":134328}}"#,
            Some(134328),
        ),
        (
            "dbt",
            r#"{"schema":1,"engagement":{"backend":"dbt","counted_branches":0}}"#,
            Some(0),
        ),
        // sabre
        (
            "sabre",
            r#"{"schema":1,"guest_rpc_observed":true,"ptrace_fallback_sites":7,"trusted_shared_object_sites":0,"trusted_shared_objects":[]}"#,
            Some(1),
        ),
        (
            "sabre",
            r#"{"schema":1,"guest_rpc_observed":false,"ptrace_fallback_sites":0,"trusted_shared_object_sites":0,"trusted_shared_objects":[]}"#,
            Some(0),
        ),
        // No counter published yet. MUST be None, never Some(0) and never a pass.
        ("liteinst", r#"{"schema":1}"#, None),
        ("kvm", r#"{"schema":1}"#, None),
        // A backend that printed nothing at all cannot be scored either.
        ("e9patch", "", None),
        // A record for another backend must not be reinterpreted positionally.
        (
            "e9patch",
            r#"{"schema":1,"engagement":{"backend":"dbt","counted_branches":2}}"#,
            None,
        ),
        // The historical presentation banner is no longer an authority.
        ("e9patch", "mapped_sites=2", None),
    ];
    let mut failures = 0;
    for (backend, text, expected) in cases {
        let got = backend_engagement(backend, text.as_bytes());
        let verdict = match got {
            None => "unknown".to_string(),
            Some(0) => "NOT-EXERCISED".to_string(),
            Some(n) => format!("engaged({n})"),
        };
        if got != *expected {
            println!("SELF-CHECK FAIL {backend}: expected {expected:?} got {got:?}");
            failures += 1;
        } else {
            println!("self-check ok  {backend:<9} {verdict}");
        }
    }
    for (scheduler_turns, expected) in [(43, Some(43)), (0, Some(0))] {
        let record = serde_json::to_vec(&RunSummary {
            sched_turns: scheduler_turns,
            ..RunSummary::default()
        })
        .unwrap();
        let got = backend_engagement("ptrace", &record);
        if got != expected {
            println!("SELF-CHECK FAIL ptrace: expected {expected:?} got {got:?}");
            failures += 1;
        } else {
            println!("self-check ok  ptrace    {scheduler_turns}");
        }
    }
    // The discrimination itself, stated as a property rather than left implicit.
    if backend_engagement(
        "e9patch",
        br#"{"schema":1,"engagement":{"backend":"e9patch","mapped_sites":0}}"#,
    ) == backend_engagement(
        "e9patch",
        br#"{"schema":1,"engagement":{"backend":"e9patch","mapped_sites":2}}"#,
    ) {
        println!("SELF-CHECK FAIL: parser does not discriminate zero from nonzero");
        failures += 1;
    }
    // --verify-json -> tier mapping, BOTH DIRECTIONS. A gate that only ever
    // passes proves nothing, so every positive here is paired with the nearest
    // negative that must NOT earn the same tier.
    // (name, json, expected `tier|bitwise_parity|compared|verify_compare`)
    let verdict_cases: &[(&str, &str, Option<&str>)] = &[
        // POSITIVE: a real canonical comparison over nonzero messages.
        (
            "canonical-nonzero",
            r#"{"verified":true,"bitwise_parity":true,"verdict":"matched",
                "comparison":{"strictness":"canonical","compare_logs":true},
                "compared_log_messages":{"left":112,"right":112}}"#,
            Some("bitwise|1|112|112|canonical"),
        ),
        // NEGATIVE, the ambiguous zero: parity is TRUE but nothing was compared.
        // An empty-vs-empty comparison "agrees" under any spec, so this must NOT
        // reach `bitwise`.
        (
            "canonical-zero-counts",
            r#"{"verified":true,"bitwise_parity":true,"verdict":"matched",
                "comparison":{"strictness":"canonical","compare_logs":true},
                "compared_log_messages":{"left":0,"right":0}}"#,
            Some("stripped|0|0|0|canonical"),
        ),
        // NEGATIVE: one side empty is still not a comparison.
        (
            "canonical-one-side-zero",
            r#"{"verified":true,"bitwise_parity":true,"verdict":"matched",
                "comparison":{"strictness":"canonical","compare_logs":true},
                "compared_log_messages":{"left":112,"right":0}}"#,
            Some("stripped|0|112|0|canonical"),
        ),
        // The stripped policy's own verdict: counted, but never bitwise.
        (
            "stripped-nonzero",
            r#"{"verified":true,"bitwise_parity":false,"verdict":"matched",
                "comparison":{"strictness":"stripped","compare_logs":true},
                "compared_log_messages":{"left":236,"right":236}}"#,
            Some("stripped|0|236|236|stripped"),
        ),
        // A DIVERGENCE must be recorded, not dropped.
        (
            "diverged",
            r#"{"verified":false,"bitwise_parity":false,"verdict":"diverged",
                "comparison":{"strictness":"canonical","compare_logs":true},
                "compared_log_messages":{"left":112,"right":109}}"#,
            Some("gap|0|112|109|canonical"),
        ),
        // Verified without comparing the log stream at all: stdout + exit only.
        (
            "guest-only",
            r#"{"verified":true,"bitwise_parity":false,"verdict":"matched",
                "comparison":{"strictness":"stripped","compare_logs":false},
                "compared_log_messages":{"left":0,"right":0}}"#,
            Some("guest|0|0|0|stripped"),
        ),
        // REFUSALS, which must fall back rather than mint a tier.
        (
            "no-result",
            r#"{"verified":false,"verdict":"no_result"}"#,
            None,
        ),
        (
            "infrastructure-with-comparison",
            r#"{"verified":false,"bitwise_parity":false,"verdict":"infrastructure_error",
                "infrastructure_error":{"kind":"skid_overshoot","count":2},
                "comparison":{"strictness":"canonical","compare_logs":true},
                "compared_log_messages":{"left":112,"right":112}}"#,
            Some("gap|0|112|112|canonical"),
        ),
        (
            "infrastructure-before-comparison",
            r#"{"verified":false,"bitwise_parity":false,"verdict":"infrastructure_error",
                "infrastructure_error":{"kind":"skid_overshoot","count":1},
                "comparison":null,"compared_log_messages":null}"#,
            Some("gap|0||"),
        ),
        (
            "unknown-verdict",
            r#"{"verified":false,"verdict":"future_result"}"#,
            None,
        ),
        ("empty-file", "", None),
        ("not-json", "Determinism verified.", None),
    ];
    for (name, json, expected) in verdict_cases {
        let got = verify_tier_from_json(json).map(|v| {
            format!(
                "{}|{}|{}|{}",
                v.tier, v.bitwise_parity, v.compared_log_messages, v.verify_compare
            )
        });
        let got_ref = got.as_deref();
        if got_ref != *expected {
            println!("SELF-CHECK FAIL verdict/{name}: expected {expected:?} got {got_ref:?}");
            failures += 1;
        } else {
            println!(
                "self-check ok  verdict/{name:<22} {}",
                got_ref.unwrap_or("no-verdict")
            );
        }
    }
    // The discrimination that matters, stated as a property: identical parity
    // booleans must NOT produce the same tier when only the counts differ.
    let counted = verify_tier_from_json(
        r#"{"verified":true,"bitwise_parity":true,"verdict":"matched",
            "comparison":{"strictness":"canonical","compare_logs":true},
            "compared_log_messages":{"left":1,"right":1}}"#,
    )
    .map(|v| v.tier);
    let uncounted = verify_tier_from_json(
        r#"{"verified":true,"bitwise_parity":true,"verdict":"matched",
            "comparison":{"strictness":"canonical","compare_logs":true},
            "compared_log_messages":{"left":0,"right":0}}"#,
    )
    .map(|v| v.tier);
    if counted == uncounted {
        println!("SELF-CHECK FAIL: zero-count comparison earns the same tier as a real one");
        failures += 1;
    }
    let infrastructure = verify_tier_from_json(
        r#"{"verified":false,"bitwise_parity":false,"verdict":"infrastructure_error",
            "infrastructure_error":{"kind":"skid_overshoot","count":3},
            "comparison":null,"compared_log_messages":null}"#,
    )
    .and_then(|verdict| verdict.infrastructure_error);
    if infrastructure.as_deref() != Some("verification recorded 3 HERMIT_SKID_OVERSHOOT report(s)")
    {
        println!(
            "SELF-CHECK FAIL: infrastructure receipt did not retain its named cause: {infrastructure:?}"
        );
        failures += 1;
    }
    if is_regression("error", true, "enabled")
        || deterministic_from_harness("error", true, true).is_some()
        || !is_regression("fail", true, "enabled")
        || deterministic_from_harness("fail", true, true) != Some(false)
    {
        println!(
            "SELF-CHECK FAIL: infrastructure error and product failure no longer stay in distinct scorecard buckets"
        );
        failures += 1;
    }
    let failed_harness = HarnessResult {
        test: "fixture".into(),
        mode: "verify".into(),
        outcome: "error".into(),
        duration_ms: 1,
        reason: "fixture".into(),
        verification_report: Some(
            r#"{"verified":false,"bitwise_parity":false,"verdict":"infrastructure_error",
                "infrastructure_error":{"kind":"skid_overshoot","count":4},
                "comparison":null,"compared_log_messages":null}"#
                .into(),
        ),
    };
    let failed_cause =
        harness_verify_verdict(&failed_harness).and_then(|verdict| verdict.infrastructure_error);
    if failed_cause.as_deref() != Some("verification recorded 4 HERMIT_SKID_OVERSHOOT report(s)") {
        println!(
            "SELF-CHECK FAIL: a non-passing harness row did not read its own infrastructure receipt: {failed_cause:?}"
        );
        failures += 1;
    }
    let total = cases.len() + 2 + verdict_cases.len();
    if failures == 0 {
        println!("self-check: {total} cases, all discriminating");
        0
    } else {
        println!("self-check: {failures} FAILED of {total}");
        3
    }
}

/// The name of the counter that backs each backend's engagement invariant, so the
/// reason string names the exact quantity that was zero rather than gesturing at it.
fn engagement_counter_name(backend: &str) -> &'static str {
    match backend {
        "e9patch" => "mapped_sites",
        "sabre" => "guest RPC observed",
        "dbt" => "counted branches",
        "ptrace" => "scheduler turns",
        _ => "engagement",
    }
}

static EVIDENCE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn backend_evidence_path(repo: &Path, backend: &str) -> Option<PathBuf> {
    let directory = repo.join("target").join("compat-envelope-evidence");
    fs::create_dir_all(&directory).ok()?;
    let sequence = EVIDENCE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Some(directory.join(format!(
        "{}-{}-{sequence}.json",
        std::process::id(),
        backend
    )))
}

fn run_and_hash(
    repo: &Path,
    lane: &str,
    backend: &str,
    guest: &[String],
) -> Option<(String, Option<i64>)> {
    let hermit = hermit_bin(repo);
    let evidence_path = backend_evidence_path(repo, backend)?;
    let _ = fs::remove_file(&evidence_path);
    let mut cmd = Command::new("timeout");
    cmd.arg("120s")
        .arg(&hermit)
        .arg("run")
        .arg("--backend")
        .arg(backend)
        .arg("--strict");
    // Pin the GUEST environment. `--base-env` defaults to `host`, which passes every one of
    // hermit's own variables through to the guest. The kernel builds the initial stack from
    // argv/envp/auxv, so the size of that block decides where the stack sits: a collector run
    // from a shell with one extra (or longer) variable shifts every guest stack address.
    // Measured on ptrace/--strict with an otherwise identical guest, changing a single variable's
    // length moved the stack 16 bytes and changed 110 of 172 DETLOG lines — all 54 [stack] hashes
    // plus 56 INFO-log syscall arguments that carry stack addresses. Stdout parity does not see
    // this, but any stack-derived observable does, so pinning here (once, at the env) is what
    // keeps cells comparable across invocations instead of two downstream exclusions that drift.
    // `minimal` is hermit's own deterministic base: PATH, HOSTNAME, HOME and nothing else.
    // `--summary-json`, `--backend-engagement-json`, and SaBRe's path-evidence
    // file are producer-owned records. `--summary` remains presentation only.
    cmd.arg("--summary");
    match backend {
        "ptrace" => {
            cmd.arg("--summary-json").arg(&evidence_path);
        }
        "sabre" => {
            cmd.env("HERMIT_SABRE_PATH_EVIDENCE", &evidence_path);
        }
        "e9patch" | "dbt" => {
            cmd.arg("--backend-engagement-json").arg(&evidence_path);
        }
        _ => {}
    }
    cmd.arg("--base-env").arg("minimal");
    // Re-add the two variables this harness has always relied on for guest determinism.
    // `minimal` would otherwise drop them, and an UNSET TZ is worse than an inherited one:
    // glibc then falls back to /etc/localtime, i.e. host state. Pinned guest env is exactly:
    // PATH, HOSTNAME, HOME (hermit `minimal`) + LC_ALL=C + TZ=UTC. Nothing else reaches the guest.
    cmd.arg("-e").arg("LC_ALL=C").arg("-e").arg("TZ=UTC");
    if lane == "portable" {
        cmd.arg("--no-virtualize-cpuid")
            .arg("--max-timeslice=disabled");
    }
    cmd.arg("--");
    for g in guest {
        cmd.arg(g);
    }
    cmd.env("LC_ALL", "C").env("TZ", "UTC").current_dir(repo);
    let out = cmd.output().ok()?;
    let evidence = fs::read(&evidence_path).ok();
    let _ = fs::remove_file(&evidence_path);
    if !out.status.success() {
        return None;
    }
    let mut h = Sha256::new();
    h.update(&out.stdout);
    let engaged = evidence
        .as_deref()
        .and_then(|bytes| backend_engagement(backend, bytes));
    Some((format!("{:x}", h.finalize()), engaged))
}

/// Resolve the hermit binary the same way `ci/test_harness.sh` does:
/// `HERMIT_BIN` if set, else `<repo>/target/debug/hermit`. Keeping this
/// env-overridable lets CI point the gate at the exact binary it just built
/// (release, alternate target dir, etc.) instead of a hardcoded debug path.
fn hermit_bin(repo: &Path) -> PathBuf {
    if let Ok(p) = env::var("HERMIT_BIN") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    repo.join("target/debug/hermit")
}

fn script_dir() -> PathBuf {
    if let Ok(p) = env::var("RUST_SCRIPT_BASE_PATH") {
        return PathBuf::from(p);
    }
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| env::current_dir().expect("cwd"))
}
