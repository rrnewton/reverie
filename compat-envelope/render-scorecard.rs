#!/usr/bin/env rust-script
//! Render the Hermit cross-backend **compatibility-envelope scorecard** from the
//! machine-readable CSV that `collect-envelope.rs` writes as a side effect of
//! every regression / expansion run.
//!
//! The scorecard is the owner's exact format:
//!
//!   * one row per e2e manifest **bucket**, plus a `TOTAL` row;
//!   * the leftmost data column is **ptrace** as an *integer count* — the number
//!     of tests in that bucket that pass the golden strict+replay determinism
//!     check (`verify` mode). This is the B4 denominator;
//!   * the canonical Hermit backend columns are
//!     `"stdout-parity%, determinism%"` where
//!       - **stdout-parity%** = fraction of the ptrace denominator whose piped
//!                              stdout SHA-256 matches the ptrace reference;
//!       - **determinism%**  = fraction of the ptrace denominator that is itself
//!                             deterministic under that backend (run1 == run2).
//!     stdout-parity% is an upper bound on full cross-backend parity: it does not
//!     compare INFO logs, stack detlogs, or heap detlogs. TTY behavior is also
//!     outside this scorecard.
//!     Determinism and stdout parity are independent signals; neither implies
//!     the other. A cell the backend never ran is NOT counted as 0: `ran_count`
//!     and `<parity>_measured_count` travel with every cell, an unmeasured
//!     observable renders `?` (or `~` for partial) and a backend that could not
//!     run renders `n/a` — none of which is a confirmed zero. A small envelope
//!     therefore reads as a small MEASURED population, not as a low percentage
//!     over an imagined one.
//!     Correspondingly, an EMPTY DENOMINATOR is refused rather than rendered:
//!     if no ptrace row passes the requested `--denominator` mode there are no
//!     cells at all, so the tool prints `NO DATA:` with the modes and backends
//!     the run does contain and exits 3 (distinct from 2 = usage, 0 = rendered).
//!     Without that, a dbi/strict-only run rendered a confident `TOTAL 0`.
//!     Reverie counter CSVs select `--observable tool-count` instead and are
//!     labeled `tool-count-parity%`; the two observables are never conflated.
//!
//! The machine-readable projection (`--json` / `--tsv`) is printed underneath /
//! instead of the human table, so downstream tooling never scrapes the ASCII.
//!
//! Usage:
//!   compat-envelope/render-scorecard.rs --csv PATH [--run-id ID|--latest]
//!                                       [--denominator MODE] [--json|--tsv]
//!                                       [--backends b1,b2,...]
//!                                       [--observable stdout|tool-count]
//!
//!   --csv PATH        Scorecard CSV. Required so the population is explicit;
//!                     use fullcorpus-scorecard.csv for the full corpus.
//!   --run-id ID       Render only rows from this run_id.
//!   --latest          Render only the most recent run_id (default when neither
//!                     --run-id nor --all is given).
//!   --all             Render only when the CSV contains one run identity.
//!                     Multiple runs are refused; select one with --run-id.
//!   --denominator M   Which passing ptrace test_mode defines the denominator
//!                     (default: verify).
//!   --backends LIST   Comma-separated backend columns, in order
//!                     (default: dbi,kvm,sabre,liteinst — whichever appear).
//!   --observable O    Observable-specific parity column to read: `stdout_parity`
//!                     by default, or `tool_count_parity` for Reverie counters.
//!                     The legacy `parity` spelling remains readable.
//!   --json | --tsv    Machine-readable output instead of the table.
//!
//! ```cargo
//! [dependencies]
//! serde_json = "1"
//! ```

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{exit, Command, Stdio};

const USAGE: &str = r#"Usage: compat-envelope/render-scorecard.rs --csv PATH [OPTIONS]

Render a cross-backend compatibility-envelope scorecard from an explicit CSV.

Options:
  --csv PATH        Scorecard CSV (required; population must be explicit).
  --run-id ID       Render only rows from this run_id.
  --latest          Render only the most recent run_id (default).
  --all             Require one run identity; refuse mixed-run aggregation.
  --denominator M   Passing ptrace test_mode defining the count (def: verify).
  --backends LIST   Comma-separated backend columns (def: dbi,kvm,sabre,liteinst).
  --observable O    stdout (default) or tool-count; labels parity honestly.
  --json | --tsv    Machine-readable output instead of the table.
  -h, --help        Show this help.
"#;

fn die(msg: &str) -> ! {
    eprintln!("render-scorecard: {msg}\n\n{USAGE}");
    exit(2);
}

fn script_dir() -> PathBuf {
    if let Ok(path) = env::var("RUST_SCRIPT_BASE_PATH") {
        return PathBuf::from(path);
    }
    PathBuf::from(file!())
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("compat-envelope"))
}

/// One CSV row, only the fields the scorecard needs.
#[derive(Clone, Debug)]
struct Cell {
    /// File-append order (0-based). The collector only appends, so a higher seq
    /// is a strictly later write. "Newest" is defined by seq, NOT by run_id
    /// string order — run_ids may carry non-numeric prefixes (e.g.
    /// `canonical-<ts>`) that would sort above later numeric timestamps and
    /// silently mask a newer run.
    seq: usize,
    run_id: String,
    bucket: String,
    test_id: String,
    test_mode: String, // verify | replay | chaos | custom | naked
    backend: String,   // ptrace | dbi | kvm | sabre | liteinst | native
    outcome: String,   // pass | fail | error | timeout | oom | skip
    deterministic: Option<bool>,
    /// Selected observable parity. The schema column is chosen from
    /// `--observable`, with `parity` accepted only as a legacy fallback.
    observable_parity: Option<bool>,
    /// Cross-backend standard actually met. This is separate from `tier`,
    /// which describes the run1/run2 self-determinism comparator.
    comparison_tier: String,
    /// Optional stronger invocation modes. None is historical/unknown, false
    /// means the flag was not passed, and true means it was passed.
    stack_parity: Option<bool>,
    heap_parity: Option<bool>,
    /// A nonempty INFO-log comparison. Tier labels do not certify this; the
    /// row must carry a comparison that actually examined at least one record.
    info_comparison_evidenced: bool,
    /// Whether the row carries EVIDENCE that a determinism comparison actually
    /// happened: a non-blank `parity_comparator` or a non-blank
    /// `compared_log_messages`. Hermit refuses a determinism positive whose
    /// evidence fields are blank (4da445156, "backend-parity: refuse a
    /// determinism positive whose evidence fields are blank"); this is the
    /// parent half of that refusal, which never got inherited across the repo
    /// boundary. `deterministic=1` beside a blank comparator and blank counts is
    /// a green with nothing behind it.
    determinism_evidenced: bool,
}

const FULL_COMPARISON_TIER: &str = "full-stdout-info-stack-heap";
const SPOT_CHECK_COMPARISON_TIER: &str = "stdout-info-stack-heap-spot-check";
const UNQUALIFIED_COMPARISON_TIERS: &[&str] = &[
    "legacy-unqualified",
    "unqualified-no-comparison",
    "unqualified-stdout-only",
    "unqualified-self-verify-only",
    "unqualified-tool-count-only",
];

fn qualifies_as_green(tier: &str) -> bool {
    matches!(tier, FULL_COMPARISON_TIER | SPOT_CHECK_COMPARISON_TIER)
}

fn nonempty_comparison(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "-" | "n/a" | "none" | "null" | "0" | "0|0" | "0/0"
    )
}

fn cell_is_green(cell: &Cell) -> bool {
    cell.outcome == "pass"
        && qualifies_as_green(&cell.comparison_tier)
        && cell.observable_parity == Some(true)
        && cell.info_comparison_evidenced
}

fn known_comparison_tier(tier: &str) -> bool {
    qualifies_as_green(tier) || UNQUALIFIED_COMPARISON_TIERS.contains(&tier)
}

/// Columns common to every supported scorecard schema. Observable-specific
/// parity is resolved separately so stdout equality and Tool-count equality
/// can never be silently conflated.
const REQUIRED_COLUMNS: &[&str] = &[
    "run_id",
    "run_utc",
    "hermit_sha",
    "reverie_sha",
    "dirty",
    "run_mode",
    "lane",
    "bucket",
    "test_id",
    "test_mode",
    "backend",
    "cell_state",
    "outcome",
    "deterministic",
    "output_hash",
    "duration_ms",
    "max_rss_kb",
    "reason",
    "comparison_tier",
];

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim() {
        "1" | "true" | "TRUE" | "True" => Some(true),
        "0" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

/// Minimal RFC-4180-ish CSV split: handles double-quoted fields with commas and
/// escaped `""`. Good enough for our own writer's output.
fn split_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            out.push(std::mem::take(&mut field));
        } else {
            field.push(c);
        }
    }
    out.push(field);
    out
}

fn main() {
    let mut csv: Option<PathBuf> = None;
    let mut run_id: Option<String> = None;
    let mut latest = false;
    let mut all = false;
    let mut denom_mode = "verify".to_string();
    let mut backends_arg: Option<String> = None;
    let mut observable = "stdout".to_string();
    let mut fmt = "table";

    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                exit(0);
            }
            "--csv" => {
                csv = Some(PathBuf::from(
                    it.next().unwrap_or_else(|| die("--csv needs a path")),
                ))
            }
            "--run-id" => run_id = Some(it.next().unwrap_or_else(|| die("--run-id needs a value"))),
            "--latest" => latest = true,
            "--all" => all = true,
            "--denominator" => {
                denom_mode = it
                    .next()
                    .unwrap_or_else(|| die("--denominator needs a mode"))
            }
            "--backends" => {
                backends_arg = Some(it.next().unwrap_or_else(|| die("--backends needs a list")))
            }
            "--observable" => {
                observable = it
                    .next()
                    .unwrap_or_else(|| die("--observable needs a value"))
            }
            "--json" => fmt = "json",
            "--tsv" => fmt = "tsv",
            other => die(&format!("unknown argument {other}")),
        }
    }

    let csv_path = csv.unwrap_or_else(|| {
        die("--csv is required: choose `compat-envelope/fullcorpus-scorecard.csv` for the full corpus or `compat-envelope/scorecard.csv` for the CI/regression subset")
    });
    // One semantic verifier owns parity provenance. For the tracked current
    // view, first prove it is exactly the newest-per-(test_id,backend)
    // projection of the append-only history, then verify the complete history
    // run-by-run. The current view is a cache, never its own authority.
    let checker = script_dir().join("check-scorecard-provenance.py");
    let history = script_dir().join("history/scorecard-observations.csv");
    let is_current_view = csv_path
        .file_name()
        .is_some_and(|name| name == "scorecard.csv")
        && history.exists();
    let provenance_input = if is_current_view {
        let projection = Command::new("python3")
            .arg(script_dir().join("publish-scorecard.py"))
            .arg("--check")
            .arg("--current")
            .arg(&csv_path)
            .arg("--history")
            .arg(&history)
            .stdout(Stdio::null())
            .status()
            .unwrap_or_else(|error| die(&format!("cannot verify current projection: {error}")));
        if !projection.success() {
            die("current scorecard is not bound to its observation history");
        }
        &history
    } else {
        &csv_path
    };
    let mut check = Command::new(&checker);
    check
        .arg(provenance_input)
        .arg("--observable")
        .arg(&observable)
        .stdout(Stdio::null());
    if all && !is_current_view {
        check.arg("--aggregate");
    }
    if let Some(ref selected_run) = run_id {
        check.arg("--run-id").arg(selected_run);
    }
    let status = check.status().unwrap_or_else(|error| {
        die(&format!(
            "cannot execute provenance verifier {}: {error}",
            checker.display()
        ))
    });
    if !status.success() {
        die("parity provenance verifier refused this scorecard/scope");
    }
    let (parity_label, parity_key, parity_meaning, full_parity_not_measured) =
        match observable.as_str() {
            "stdout" => (
                "stdout-parity",
                "stdout_parity",
                "piped guest stdout SHA-256 equality with ptrace; upper bound on four-signal cross-backend parity",
                vec!["INFO log", "stack detlog", "heap detlog"],
            ),
            "tool-count" => (
                "tool-count-parity",
                "tool_count_parity",
                "shared Tool callback-count equality with ptrace; not cross-backend execution parity",
                vec!["stdout", "INFO log", "stack detlog", "heap detlog"],
            ),
            _ => die("--observable must be `stdout` or `tool-count`"),
        };
    let denominator_meaning = if denom_mode == "verify" {
        "tests passing golden ptrace strict+replay (verify)".to_string()
    } else {
        format!("ptrace rows passing test_mode `{denom_mode}`")
    };
    let text = fs::read_to_string(&csv_path)
        .unwrap_or_else(|e| die(&format!("cannot read {}: {e}", csv_path.display())));

    let mut lines = text.lines();
    let header_line = lines.next().unwrap_or_else(|| die("empty CSV (no header)"));
    let header = split_csv_line(header_line);
    let idx = |name: &str| -> usize {
        header
            .iter()
            .position(|h| h == name)
            .unwrap_or_else(|| die(&format!("CSV missing required column `{name}`")))
    };
    // Validate the header carries every common field we rely on.
    for h in REQUIRED_COLUMNS {
        let _ = idx(h);
    }
    let modern_parity_columns = ["stdout_parity", "tool_count_parity"];
    let selected_modern = parity_key;
    let selected_count = header
        .iter()
        .filter(|h| h.as_str() == selected_modern)
        .count();
    let legacy_count = header.iter().filter(|h| h.as_str() == "parity").count();
    let wrong_modern: Vec<&str> = modern_parity_columns
        .iter()
        .copied()
        .filter(|name| *name != selected_modern && header.iter().any(|h| h == name))
        .collect();
    if selected_count > 1 || legacy_count > 1 {
        die(&format!(
            "CSV has duplicate observable columns (`{selected_modern}` count={selected_count}, legacy `parity` count={legacy_count})"
        ));
    }
    if !wrong_modern.is_empty() {
        die(&format!(
            "CSV observable does not match --observable `{observable}`: found `{}`, expected `{selected_modern}` (or legacy `parity`)",
            wrong_modern.join("`, `")
        ));
    }
    if selected_count == 1 && legacy_count == 1 {
        die(&format!(
            "CSV has ambiguous observable columns `{selected_modern}` and legacy `parity`; keep exactly one"
        ));
    }
    let i_par = if selected_count == 1 {
        idx(selected_modern)
    } else if legacy_count == 1 {
        idx("parity")
    } else {
        die(&format!(
            "CSV missing observable column `{selected_modern}` (legacy `parity` is also accepted)"
        ));
    };
    let (i_run, i_bucket, i_tid, i_tmode, i_backend, i_outcome, i_det, i_comparison_tier) = (
        idx("run_id"),
        idx("bucket"),
        idx("test_id"),
        idx("test_mode"),
        idx("backend"),
        idx("outcome"),
        idx("deterministic"),
        idx("comparison_tier"),
    );
    // Evidence that a determinism comparison actually happened. OPTIONAL by
    // design: pre-schema-5 rows do not carry these columns, and such a row must
    // read as UNEVIDENCED rather than fail the whole render -- an old row cannot
    // retroactively acquire evidence it never recorded.
    let i_comparator = header.iter().position(|h| h == "parity_comparator");
    let i_compared = header.iter().position(|h| h == "compared_log_messages");
    let i_stack_parity = header.iter().position(|h| h == "stack_parity");
    let i_heap_parity = header.iter().position(|h| h == "heap_parity");

    let mut cells: Vec<Cell> = Vec::new();
    for (n, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let get = |i: usize| f.get(i).cloned().unwrap_or_default();
        if f.len() < header.len() {
            eprintln!(
                "warn: row {} has {} fields (< {}); skipping",
                n + 2,
                f.len(),
                header.len()
            );
            continue;
        }
        let comparison_tier = get(i_comparison_tier).trim().to_string();
        if comparison_tier.is_empty() {
            die(&format!(
                "REFUSED row {}: outcome {:?} has no comparison_tier; a raw pass may never default to a strict green",
                n + 2,
                get(i_outcome)
            ));
        }
        if !known_comparison_tier(&comparison_tier) {
            die(&format!(
                "REFUSED row {}: unknown comparison_tier {:?}",
                n + 2,
                comparison_tier
            ));
        }
        cells.push(Cell {
            seq: n,
            run_id: get(i_run),
            bucket: get(i_bucket),
            test_id: get(i_tid),
            test_mode: get(i_tmode),
            backend: get(i_backend),
            outcome: get(i_outcome),
            deterministic: parse_bool(&get(i_det)),
            observable_parity: parse_bool(&get(i_par)),
            comparison_tier,
            stack_parity: i_stack_parity.and_then(|index| parse_bool(&get(index))),
            heap_parity: i_heap_parity.and_then(|index| parse_bool(&get(index))),
            info_comparison_evidenced: i_compared
                .map(&get)
                .is_some_and(|value| nonempty_comparison(&value)),
            // Absent columns (pre-schema-5 rows) read as blank, i.e. NOT
            // evidenced -- an older row cannot retroactively acquire evidence it
            // never recorded.
            determinism_evidenced: i_comparator.map(&get).is_some_and(|v| !v.trim().is_empty())
                || i_compared
                    .map(&get)
                    .is_some_and(|v| nonempty_comparison(&v)),
        });
    }
    if cells.is_empty() {
        die("CSV has a header but no data rows");
    }

    // Select the run scope.
    let scope_run: Option<String> = if let Some(r) = run_id {
        Some(r)
    } else if all {
        None
    } else {
        // default / --latest: the run_id of the LAST-APPENDED row (highest seq),
        // not the lexicographic max — run_ids can carry non-numeric prefixes
        // (e.g. `canonical-<ts>`) that would sort above a newer numeric run.
        let _ = latest; // --latest is the default; the flag is accepted for clarity
        cells.iter().max_by_key(|c| c.seq).map(|c| c.run_id.clone())
    };
    if let Some(r) = &scope_run {
        cells.retain(|c| &c.run_id == r);
    } else {
        // The owner-defined current cell is (program, backend), represented as
        // (test_id, backend). Last physical observation wins.
        let mut newest: BTreeMap<(String, String), Cell> = BTreeMap::new();
        for c in cells.drain(..) {
            let key = (c.test_id.clone(), c.backend.clone());
            newest
                .entry(key)
                .and_modify(|e| {
                    // last-writer-wins by file-append order, not run_id string.
                    if c.seq > e.seq {
                        *e = c.clone();
                    }
                })
                .or_insert(c);
        }
        cells = newest.into_values().collect();
    }

    // Report exactly the selected logical population. Raw execution success is
    // not scorecard green until the row names one qualifying comparison tier.
    let mut tier_distribution: BTreeMap<String, usize> = BTreeMap::new();
    let mut raw_passes = 0usize;
    let mut qualified_passes = 0usize;
    let mut highest_grade_passes = 0usize;
    for cell in &cells {
        *tier_distribution
            .entry(cell.comparison_tier.clone())
            .or_default() += 1;
        if cell.outcome == "pass" {
            raw_passes += 1;
            if cell_is_green(cell) {
                qualified_passes += 1;
                if cell.stack_parity == Some(true) && cell.heap_parity == Some(true) {
                    highest_grade_passes += 1;
                }
            }
        }
    }
    eprintln!(
        "comparison-tier distribution: {:?} ({} rows); qualified green={}/{} raw passes",
        tier_distribution,
        cells.len(),
        qualified_passes,
        raw_passes,
    );
    eprintln!(
        "highest-grade green={}/{} qualified green in selected scope",
        highest_grade_passes, qualified_passes,
    );

    // Restrict the determinism denominator to the requested mode.
    let denom_cells: Vec<&Cell> = cells.iter().filter(|c| c.test_mode == denom_mode).collect();

    // Discover buckets and backend columns actually present.
    let buckets: BTreeSet<String> = denom_cells.iter().map(|c| c.bucket.clone()).collect();
    let present_backends: BTreeSet<String> =
        denom_cells.iter().map(|c| c.backend.clone()).collect();

    let default_order = ["dbi", "kvm", "sabre", "liteinst"];
    let backend_cols: Vec<String> = if let Some(list) = backends_arg {
        list.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        default_order
            .iter()
            .filter(|b| present_backends.contains(**b))
            .map(|s| s.to_string())
            .collect()
    };

    // ptrace denominator set per bucket = distinct test_ids where ptrace passed
    // the denominator mode.
    let mut ptrace_pass: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for c in &denom_cells {
        if c.backend == "ptrace" && cell_is_green(c) {
            ptrace_pass
                .entry(c.bucket.clone())
                .or_default()
                .insert(c.test_id.clone());
        }
    }

    // For each backend, index (bucket,test_id) -> (deterministic, parity, pass).
    // A backend "passes" a cell if outcome==pass. parity requires parity==true;
    // determinism requires deterministic==true OR (verify pass, since hermit's
    // internal double-run already proved run1==run2 for a passing verify cell).
    struct BCell {
        det: bool,
        /// Rows carrying determinism EVIDENCE. `det_measured_count` must count
        /// these, not rows -- counting rows reports the population size while
        /// the evidenced count may be zero, which is the same proxy defect one
        /// level up and would make the refusal invisible in the summary.
        det_measured: bool,
        par: bool,
        /// Whether parity was actually measured (CSV field non-blank). A blank
        /// parity is "unknown", NOT a confirmed 0 — kept distinct so the
        /// scorecard can report real comparison coverage (phase-2 needs every
        /// red to be a CONFIRMED red, never an unmeasured one).
        par_measured: bool,
        /// Whether the cell was actually RUN on this backend. An `unavailable`
        /// (backend binary absent) or `skip` cell was NOT run — its 0s are
        /// "not measured", never a confirmed compat/determinism fail.
        ran: bool,
    }
    let mut by_backend: BTreeMap<String, BTreeMap<(String, String), BCell>> = BTreeMap::new();
    for c in &denom_cells {
        if c.backend == "ptrace" || c.backend == "native" {
            continue;
        }
        let pass = cell_is_green(c);
        let ran = c.outcome != "unavailable" && c.outcome != "skip";
        // Determinism (run1 == run2) is independent of parity: a backend can be
        // self-deterministic yet diverge from ptrace. The CSV `deterministic`
        // field is authoritative, and the strict comparison tier is required.
        // Never infer determinism from a raw execution pass.
        // BLANK-EVIDENCE REFUSAL (parent half of hermit 4da445156). A
        // determinism positive requires evidence that a comparison verdict
        // exists. Without it the row is UNMEASURED, not deterministic-by-
        // default -- and not a negative either: refusing is zero qualifying
        // evidence, never a confirmed fail.
        let det = pass && c.deterministic.unwrap_or(false) && c.determinism_evidenced;
        let det_measured = pass && c.determinism_evidenced;
        // Parity is true only when the collector recorded a bitwise match.
        let par = pass && c.observable_parity.unwrap_or(false);
        let par_measured = pass && c.observable_parity.is_some();
        by_backend.entry(c.backend.clone()).or_default().insert(
            (c.bucket.clone(), c.test_id.clone()),
            BCell {
                det,
                det_measured,
                par,
                par_measured,
                ran,
            },
        );
    }

    // Build per-bucket rows.
    #[derive(Default, Clone)]
    struct Row {
        ptrace: usize,
        // backend -> (parity_count, det_count, parity_measured_count, ran_count, det_measured_count)
        back: BTreeMap<String, (usize, usize, usize, usize, usize)>,
    }
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut total = Row::default();
    for bucket in &buckets {
        let denom = ptrace_pass.get(bucket).cloned().unwrap_or_default();
        let mut row = Row {
            ptrace: denom.len(),
            back: BTreeMap::new(),
        };
        for b in &backend_cols {
            let mut par = 0usize;
            let mut det = 0usize;
            let mut par_meas = 0usize;
            let mut ran = 0usize;
            let mut det_meas = 0usize;
            if let Some(map) = by_backend.get(b) {
                for tid in &denom {
                    if let Some(bc) = map.get(&(bucket.clone(), tid.clone())) {
                        if bc.par {
                            par += 1;
                        }
                        if bc.det {
                            det += 1;
                        }
                        if bc.par_measured {
                            par_meas += 1;
                        }
                        if bc.det_measured {
                            det_meas += 1;
                        }
                        if bc.ran {
                            ran += 1;
                        }
                    }
                }
            }
            row.back
                .insert(b.clone(), (par, det, par_meas, ran, det_meas));
        }
        total.ptrace += row.ptrace;
        for b in &backend_cols {
            let e = total.back.entry(b.clone()).or_insert((0, 0, 0, 0, 0));
            let (p, d, m, r, dm) = row.back[b];
            e.0 += p;
            e.1 += d;
            e.2 += m;
            e.3 += r;
            e.4 += dm;
        }
        rows.insert(bucket.clone(), row);
    }

    // A1 -- AN EMPTY DENOMINATOR IS NOT A ZERO RESULT.
    //
    // The per-cell vocabulary below (`?` / `~` / `n/a`, plus ran_count and
    // measured_count) is careful about unmeasured CELLS, but it cannot speak for
    // the denominator one level up: when no ptrace row passes the requested mode
    // there are no cells at all, and every format renders a confident `TOTAL 0`
    // (exit 0) that is indistinguishable from "we measured, and nothing passed".
    // That happens for real -- a run that is dbi/strict only has a legitimately
    // empty verify/ptrace denominator.
    //
    // So refuse to render, and say what IS present so the caller can pick a mode
    // or backend that exists. Distinct exit status (3) keeps "could not measure"
    // separable from usage errors (2) and from a rendered result (0).
    //
    // The remedy line reports the modes that would ACTUALLY yield a denominator --
    // i.e. modes with a *passing ptrace* row -- not the modes the run merely
    // contains. Those two differ exactly when the run has no ptrace rows at all
    // (a dbi-only run "contains" strict, yet `--denominator strict` refuses too),
    // and naming the wrong set would repeat this very defect one level up:
    // a remedy is only actionable if it travels with the population it is drawn from.
    let denom_total: usize = ptrace_pass.values().map(|s| s.len()).sum();
    if denom_total == 0 {
        let modes_present: BTreeSet<&str> = cells.iter().map(|c| c.test_mode.as_str()).collect();
        let backends_present: BTreeSet<&str> = cells.iter().map(|c| c.backend.as_str()).collect();
        let ptrace_rows = cells.iter().filter(|c| c.backend == "ptrace").count();
        // Exactly the `--denominator` values that would produce a non-empty denominator.
        let usable: BTreeSet<&str> = cells
            .iter()
            .filter(|c| c.backend == "ptrace" && cell_is_green(c))
            .map(|c| c.test_mode.as_str())
            .collect();
        let run_label = scope_run
            .clone()
            .unwrap_or_else(|| "ALL (single run required)".into());
        let remedy = if !usable.is_empty() {
            format!(
                "Retry with --denominator <{}> -- those are the modes this run has passing \
                 ptrace rows in.",
                usable.iter().copied().collect::<Vec<_>>().join("|")
            )
        } else if ptrace_rows == 0 {
            "This run has NO ptrace rows in any mode, so no denominator can be formed from it \
             at all; changing --denominator will not help. Use a run that includes the ptrace \
             reference backend."
                .to_string()
        } else {
            format!(
                "This run has {ptrace_rows} ptrace rows but none passing in any mode, so no \
                 --denominator choice yields a population; the reference backend itself failed \
                 here."
            )
        };
        eprintln!(
            "NO DATA: run {run} has 0 ptrace/{mode} qualifying passing cells, so the denominator is empty \
             and no percentage is defined (this is NOT a measured zero).\n\
             \x20 rows considered:  {n}\n\
             \x20 raw passes:       {raw_passes} (qualified: {qualified_passes})\n\
             \x20 tier distribution:{tier_distribution:?}\n\
             \x20 ptrace rows:      {ptrace_rows} (passing in modes: {usable})\n\
             \x20 modes present:    {modes}\n\
             \x20 backends present: {backends}\n\
             \x20 csv:              {csv}\n\
             {remedy}",
            run = run_label,
            mode = denom_mode,
            n = cells.len(),
            raw_passes = raw_passes,
            qualified_passes = qualified_passes,
            tier_distribution = tier_distribution,
            ptrace_rows = ptrace_rows,
            usable = if usable.is_empty() {
                "none".to_string()
            } else {
                usable.iter().copied().collect::<Vec<_>>().join(",")
            },
            modes = modes_present.iter().copied().collect::<Vec<_>>().join(","),
            backends = backends_present.iter().copied().collect::<Vec<_>>().join(","),
            csv = csv_path.display(),
            remedy = remedy,
        );
        exit(3);
    }

    let pct = |num: usize, den: usize| -> f64 {
        if den == 0 {
            0.0
        } else {
            100.0 * num as f64 / den as f64
        }
    };

    match fmt {
        "json" => {
            let mut out_rows = Vec::new();
            let mut emit = |name: &str, row: &Row| {
                let mut backs = serde_json::Map::new();
                for b in &backend_cols {
                    let (p, d, m, r, dm) = row.back.get(b).copied().unwrap_or((0, 0, 0, 0, 0));
                    let mut metrics = serde_json::Map::new();
                    metrics.insert(format!("{parity_key}_count"), json!(p));
                    metrics.insert(format!("{parity_key}_measured_count"), json!(m));
                    // Rows carrying determinism EVIDENCE, not rows. With a blank
                    // comparator this is 0 while the population is large, which
                    // is exactly the distinction the refusal exists to surface.
                    metrics.insert("determinism_measured_count".to_string(), json!(dm));
                    metrics.insert(
                        format!("{parity_key}_pct"),
                        json!((pct(p, row.ptrace) * 10.0).round() / 10.0),
                    );
                    metrics.insert("determinism_count".into(), json!(d));
                    metrics.insert(
                        "determinism_pct".into(),
                        json!((pct(d, row.ptrace) * 10.0).round() / 10.0),
                    );
                    metrics.insert("ran_count".into(), json!(r));
                    backs.insert(b.clone(), Value::Object(metrics));
                }
                out_rows.push(json!({
                    "bucket": name,
                    "ptrace_count": row.ptrace,
                    "backends": backs,
                }));
            };
            for (name, row) in &rows {
                emit(name, row);
            }
            emit("TOTAL", &total);
            let doc = json!({
                "schema": 2,
                "kind": "compat-envelope-scorecard",
                "source_csv": csv_path.display().to_string(),
                "run_scope": scope_run.clone().unwrap_or_else(|| "all".into()),
                "denominator_mode": denom_mode,
                "denominator_meaning": denominator_meaning,
                "comparison_tier_distribution": tier_distribution,
                "raw_pass_count": raw_passes,
                "qualified_green_count": qualified_passes,
                "highest_grade_green_count": highest_grade_passes,
                "parity_metric": {
                    "label": parity_label,
                    "observable": observable,
                    "meaning": parity_meaning,
                    "is_full_parity": false,
                    "full_parity_not_measured": full_parity_not_measured,
                    "additional_unmeasured_context": ["TTY behavior"],
                },
                "backend_columns": backend_cols,
                "rows": out_rows,
            });
            println!("{}", serde_json::to_string_pretty(&doc).unwrap());
        }
        "tsv" => {
            let mut cols = vec!["bucket".to_string(), "ptrace".to_string()];
            for b in &backend_cols {
                cols.push(format!("{b}_{parity_key}_pct"));
                cols.push(format!("{b}_det_pct"));
                cols.push(format!("{b}_{parity_key}_measured"));
                cols.push(format!("{b}_ran"));
            }
            println!("{}", cols.join("\t"));
            let emit = |name: &str, row: &Row| {
                let mut f = vec![name.to_string(), row.ptrace.to_string()];
                for b in &backend_cols {
                    let (p, d, m, r, _dm) = row.back.get(b).copied().unwrap_or((0, 0, 0, 0, 0));
                    f.push(format!("{:.1}", pct(p, row.ptrace)));
                    f.push(format!("{:.1}", pct(d, row.ptrace)));
                    f.push(format!("{m}/{}", row.ptrace));
                    f.push(format!("{r}/{}", row.ptrace));
                }
                println!("{}", f.join("\t"));
            };
            for (name, row) in &rows {
                emit(name, row);
            }
            emit("TOTAL", &total);
        }
        _ => {
            // Human table in the owner's exact shape.
            println!(
                "Compat-envelope scorecard  (run: {}, denominator: {} = {})",
                scope_run
                    .clone()
                    .unwrap_or_else(|| "ALL (single run required)".into()),
                denom_mode,
                denominator_meaning,
            );
            println!("Input CSV: {}", csv_path.display());
            println!(
                "Comparison-tier distribution: {:?} ({} rows); qualified green={}/{} raw passes.",
                tier_distribution,
                cells.len(),
                qualified_passes,
                raw_passes
            );
            println!("Highest-grade green={}/{} qualified green in selected scope (requires base green plus --detlog-stack and --detlog-heap modes).", highest_grade_passes, qualified_passes);
            println!("Qualifying tiers: `{FULL_COMPARISON_TIER}` or `{SPOT_CHECK_COMPARISON_TIER}`. Explicit unqualified tiers remain non-green history.");
            println!("Each backend cell is `{parity_label}%, determinism%` of the ptrace count. The two measurements are independent.");
            if observable == "stdout" {
                println!("CAVEAT: stdout-parity% compares piped guest stdout SHA-256 only. It is an upper bound on four-signal cross-backend parity; INFO logs, stack detlogs, and heap detlogs are not measured. TTY behavior is also outside this scorecard.");
            } else {
                println!("CAVEAT: tool-count-parity% compares only the shared Tool callback total. It does not measure stdout, INFO logs, stack detlogs, or heap detlogs, and is not full cross-backend parity. TTY behavior is also outside this scorecard.");
            }
            println!("{parity_label} suffix: `?` = the observable was never compared for that bucket (UNKNOWN, not confirmed 0); `~` = partial coverage (some cells unmeasured).");
            println!("`n/a` = backend not runnable here (binary absent / not enabled) — 0 cells run, NOT a confirmed fail.");
            println!();
            let mut header = format!("{:<22} {:>7}", "bucket", "ptrace");
            for b in &backend_cols {
                header.push_str(&format!("  {:>16}", b));
            }
            println!("{header}");
            println!("{}", "-".repeat(header.len()));
            let emit = |name: &str, row: &Row| {
                let mut line = format!("{:<22} {:>7}", name, row.ptrace);
                for b in &backend_cols {
                    let (p, d, m, r, _dm) = row.back.get(b).copied().unwrap_or((0, 0, 0, 0, 0));
                    // A backend that ran ZERO denom cells here is not measurable
                    // (binary absent / not enabled) — show n/a, never a 0% red.
                    let cell = if row.ptrace > 0 && r == 0 {
                        "n/a".to_string()
                    } else {
                        // Distinguish "parity confirmed 0" from "parity never
                        // measured". m = denom cells whose parity was compared.
                        let mark = if row.ptrace == 0 {
                            ""
                        } else if m == 0 {
                            "?"
                        } else if m < row.ptrace {
                            "~"
                        } else {
                            ""
                        };
                        format!(
                            "{:.0}%{}, {:.0}%",
                            pct(p, row.ptrace),
                            mark,
                            pct(d, row.ptrace)
                        )
                    };
                    line.push_str(&format!("  {:>16}", cell));
                }
                println!("{line}");
            };
            for (name, row) in &rows {
                emit(name, row);
            }
            println!("{}", "-".repeat(header.len()));
            emit("TOTAL", &total);
        }
    }
}
