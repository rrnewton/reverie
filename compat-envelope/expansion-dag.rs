#!/usr/bin/env rust-script
//! Generate the compat-envelope EXPANSION sweep as a fine-grained
//! safe-ci-dag-runner DAG — one boxed step per cell (test × mode × backend) —
//! plus an aggressive per-cell evidence tree.
//!
//! This is the EXPANSION half of the two-mode compat-envelope system (the other
//! half, REGRESSION, is `collect-envelope.rs`). Where regression asserts the
//! known-green cells stay green and writes the scorecard CSV, expansion runs the
//! FULL SUPERSET of cells — including currently-failing/disabled ones — to catch
//! failing→passing flips (compat growth). Because broken cells infinite-loop or
//! OOM, every cell runs inside its own cgroup+timeout box via 229's
//! safe-ci-dag-runner; this script only *generates* that DAG, it does not run it.
//!
//! Per-cell budgets (owner spec): for each backend, take the GEO-MEAN wall-time
//! and max-RSS ratio vs ptrace across the GREEN envelope (the scorecard CSV).
//! For a RED frontier cell, budget = ptrace-baseline(test) × backend-geomean ×
//! headroom (default 1.5), for BOTH wall-time (step `timeout`) and max-mem
//! (`hint.hard_mem_max_bytes`) → a tight box per cell.
//!
//! Evidence retention (owner spec): a DATED run dir with a SUBDIR PER CELL
//! holding INFO logs (per-backend + a ptrace reference), machine-readable exec
//! stats, and stdout+stderr — pre-generated evidence so agents/owner can
//! investigate without re-running. Everything lives under an `ignored/`
//! (gitignored) dated dir; the last K runs are retained (rotate). This is the
//! evidence source the `debug/` framework reads from.
//!
//! Usage:
//!   compat-envelope/expansion-dag.rs [OPTIONS]
//!
//!   --csv PATH         Scorecard CSV = the green envelope + ptrace baselines
//!                      (default: scorecard.csv beside this script).
//!   --repo PATH        Hermit checkout to enumerate + run cells in
//!                      (default: $PWD or the workspace hermit primary).
//!   --lane LANE        portable | privileged (default: portable).
//!   --buckets LIST     Comma-separated bucket allow-list (default: all).
//!   --backends LIST    Comma-separated backend cols (default: dbi,kvm,sabre,liteinst).
//!   --headroom F       Budget multiplier over geomean estimate (default: 1.5).
//!   --frontier-only    Only emit cells NOT currently green in the CSV
//!                      (default: full superset of every non-ptrace cell).
//!   --evidence-root D  Root for dated evidence dirs
//!                      (default: <repo-parent>/ignored/compat-envelope).
//!   --keep K           Retain the last K dated run dirs (default: 5).
//!   --run-id ID        Dated run-dir name (default: `date -u +%Y%m%d-%H%M%S`).
//!   --dag-out PATH     Write the DAG JSON here (default: <run-dir>/dag.json).
//!   --min-timeout S    Floor on per-cell wall-time budget (default: 20).
//!   --min-mem-mb M     Floor on per-cell mem budget (default: 256).
//!   --dry-run          Enumerate + compute budgets, print a summary, write
//!                      nothing.
//!
//! ```cargo
//! [dependencies]
//! serde_json = "1"
//! ```

use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

const USAGE: &str = r#"Usage: compat-envelope/expansion-dag.rs [OPTIONS]

Generate the compat-envelope EXPANSION sweep as a boxed safe-ci-dag-runner DAG
(one cell per step) plus a dated per-cell evidence tree.

Options:
  --csv PATH         Scorecard CSV (green envelope + ptrace baselines).
  --repo PATH        Hermit checkout to enumerate + run cells in.
  --lane LANE        portable | privileged (default: portable).
  --buckets LIST     Comma-separated bucket allow-list (default: all).
  --backends LIST    Comma-separated backend cols (default: dbi,kvm,sabre,liteinst).
  --headroom F       Budget multiplier over geomean estimate (default: 1.5).
  --frontier-only    Only cells NOT currently green (default: full superset).
  --evidence-root D  Root for dated evidence dirs.
  --keep K           Retain the last K dated run dirs (default: 5).
  --run-id ID        Dated run-dir name (default: date -u +%Y%m%d-%H%M%S).
  --dag-out PATH     Write the DAG JSON here (default: <run-dir>/dag.json).
  --min-timeout S    Floor on per-cell wall-time budget seconds (default: 20).
  --min-mem-mb M     Floor on per-cell mem budget MB (default: 256).
  --dry-run          Print a summary, write nothing.
  -h, --help         Show this help.
"#;

fn die(msg: &str) -> ! {
    eprintln!("expansion-dag: {msg}\n\n{USAGE}");
    exit(2);
}

/// Conservative fallback ratios vs ptrace, used ONLY when the green envelope has
/// no overlapping cell for a backend (so the geomean is undefined). Documented
/// as estimates; they tighten automatically as more backends get collected.
fn fallback_time_ratio(backend: &str) -> f64 {
    match backend {
        "ptrace" => 1.0,
        "kvm" => 2.5,
        "dbi" => 3.0,
        "sabre" => 4.0,
        "liteinst" => 3.0,
        _ => 3.0,
    }
}
fn fallback_mem_ratio(backend: &str) -> f64 {
    match backend {
        "ptrace" => 1.0,
        "kvm" => 3.0, // out-of-process VMM
        "dbi" => 1.6, // in-process JIT code cache
        "sabre" => 1.5,
        "liteinst" => 1.5,
        _ => 2.0,
    }
}

#[derive(Clone, Default)]
struct Baseline {
    dur_ms: Option<f64>,
    rss_kb: Option<f64>,
}

struct GreenCell {
    test: String,
    backend: String,
    dur_ms: Option<f64>,
    rss_kb: Option<f64>,
}

fn main() {
    let mut csv: Option<PathBuf> = None;
    let mut repo: Option<PathBuf> = None;
    let mut lane = "portable".to_string();
    let mut buckets: Vec<String> = Vec::new();
    let mut backends: Vec<String> = ["dbi", "kvm", "sabre", "liteinst"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut headroom = 1.5_f64;
    let mut frontier_only = false;
    let mut evidence_root: Option<PathBuf> = None;
    let mut keep = 5_usize;
    let mut run_id: Option<String> = None;
    let mut dag_out: Option<PathBuf> = None;
    let mut min_timeout = 20_i64;
    let mut min_mem_mb = 256_i64;
    let mut dry_run = false;

    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                exit(0);
            }
            "--csv" => csv = Some(PathBuf::from(next(&mut it, "--csv"))),
            "--repo" => repo = Some(PathBuf::from(next(&mut it, "--repo"))),
            "--lane" => lane = next(&mut it, "--lane"),
            "--buckets" => buckets = split_list(&next(&mut it, "--buckets")),
            "--backends" => backends = split_list(&next(&mut it, "--backends")),
            "--headroom" => {
                headroom = next(&mut it, "--headroom")
                    .parse()
                    .unwrap_or_else(|_| die("--headroom must be a number"))
            }
            "--frontier-only" => frontier_only = true,
            "--evidence-root" => {
                evidence_root = Some(PathBuf::from(next(&mut it, "--evidence-root")))
            }
            "--keep" => {
                keep = next(&mut it, "--keep")
                    .parse()
                    .unwrap_or_else(|_| die("--keep must be an integer"))
            }
            "--run-id" => run_id = Some(next(&mut it, "--run-id")),
            "--dag-out" => dag_out = Some(PathBuf::from(next(&mut it, "--dag-out"))),
            "--min-timeout" => {
                min_timeout = next(&mut it, "--min-timeout")
                    .parse()
                    .unwrap_or_else(|_| die("--min-timeout must be an integer"))
            }
            "--min-mem-mb" => {
                min_mem_mb = next(&mut it, "--min-mem-mb")
                    .parse()
                    .unwrap_or_else(|_| die("--min-mem-mb must be an integer"))
            }
            "--dry-run" => dry_run = true,
            other => die(&format!("unknown argument {other}")),
        }
    }

    let csv_path = csv.unwrap_or_else(|| script_dir().join("scorecard.csv"));
    let repo = repo.unwrap_or_else(default_repo);
    if !repo.join("ci/test_harness.sh").is_file() {
        die(&format!(
            "--repo {} has no ci/test_harness.sh",
            repo.display()
        ));
    }

    // --- 1. Parse the scorecard CSV: ptrace baselines + per-backend green cells.
    let (baselines, greens, green_keys) = parse_csv(&csv_path, &lane);

    // --- 2. Per-backend geomean ratios vs ptrace across the green envelope.
    let ratios = geomean_ratios(&baselines, &greens, &backends);
    eprintln!("expansion-dag: geomean ratios vs ptrace across green envelope:");
    for b in &backends {
        let (rt, rm, n) =
            ratios
                .get(b)
                .copied()
                .unwrap_or((fallback_time_ratio(b), fallback_mem_ratio(b), 0));
        let src = if n > 0 {
            format!("measured (n={n})")
        } else {
            "FALLBACK (no green overlap)".to_string()
        };
        eprintln!("  {b:<10} time×{rt:.2}  mem×{rm:.2}  [{src}]");
    }

    // --- 3. Enumerate the full cell superset (enabled plan ∪ disabled gaps).
    let mut cells = enumerate_cells(&repo, &lane);
    if !buckets.is_empty() {
        cells.retain(|c| buckets.iter().any(|b| b == &c.category));
    }
    // Only backends we score, and never a ptrace baseline row (ptrace IS the ref).
    cells.retain(|c| backends.iter().any(|b| b == &c.backend));
    if frontier_only {
        cells.retain(|c| {
            let k = cell_key(&c.category, &c.test, &c.mode, &c.backend);
            !green_keys.contains(&k)
        });
    }
    cells.sort_by(|a, b| {
        (&a.category, &a.test, &a.mode, &a.backend).cmp(&(
            &b.category,
            &b.test,
            &b.mode,
            &b.backend,
        ))
    });

    if cells.is_empty() {
        die("no cells enumerated for the given lane/buckets/backends");
    }

    // --- 4. Per-cell budgets.
    struct Budget<'a> {
        c: &'a Cell,
        timeout_s: i64,
        mem_bytes: i64,
        est_dur_s: f64,
        base_est: bool, // ptrace baseline was defaulted (no CSV data)
    }
    let default_base_time_s = 5.0_f64;
    let default_base_mem_b = 512.0 * 1024.0 * 1024.0;
    let budgets: Vec<Budget> = cells
        .iter()
        .map(|c| {
            let base = baselines.get(&c.test).cloned().unwrap_or_default();
            let base_est = base.dur_ms.is_none();
            let base_t = base
                .dur_ms
                .map(|ms| ms / 1000.0)
                .unwrap_or(default_base_time_s);
            let base_m = base
                .rss_kb
                .map(|kb| kb * 1024.0)
                .unwrap_or(default_base_mem_b);
            let (rt, rm, _) = ratios.get(&c.backend).copied().unwrap_or((
                fallback_time_ratio(&c.backend),
                fallback_mem_ratio(&c.backend),
                0,
            ));
            let est_dur_s = base_t * rt;
            let timeout_s = ((est_dur_s * headroom).ceil() as i64).max(min_timeout);
            let mem_bytes = ((base_m * rm * headroom) as i64).max(min_mem_mb * 1024 * 1024);
            Budget {
                c,
                timeout_s,
                mem_bytes,
                est_dur_s,
                base_est,
            }
        })
        .collect();

    let est_cnt = budgets.iter().filter(|b| b.base_est).count();
    eprintln!(
        "expansion-dag: {} cells (lane={lane}, {} with defaulted ptrace baseline)",
        budgets.len(),
        est_cnt
    );

    if dry_run {
        println!("# expansion sweep dry-run — {} cells", budgets.len());
        println!(
            "# {:<48} {:<8} {:>8} {:>9}",
            "cell", "backend", "timeout", "mem_mb"
        );
        for b in budgets.iter().take(40) {
            println!(
                "  {:<48} {:<8} {:>7}s {:>8}M{}",
                format!("{}/{}::{}", b.c.category, short(&b.c.test), b.c.mode),
                b.c.backend,
                b.timeout_s,
                b.mem_bytes / (1024 * 1024),
                if b.base_est { " (est)" } else { "" }
            );
        }
        if budgets.len() > 40 {
            println!("  ... {} more", budgets.len() - 40);
        }
        return;
    }

    // --- 5. Evidence run dir (dated, gitignored) + rotation.
    let ev_root = evidence_root.unwrap_or_else(|| default_evidence_root(&repo));
    let run_id = run_id.unwrap_or_else(|| now_stamp());
    let run_dir = ev_root.join(&run_id);
    fs::create_dir_all(&run_dir)
        .unwrap_or_else(|e| die(&format!("cannot create run dir {}: {e}", run_dir.display())));
    rotate(&ev_root, keep);

    // Emit the per-cell runner helper into the run dir.
    let helper = run_dir.join("run-expansion-cell.sh");
    fs::write(&helper, CELL_RUNNER).unwrap_or_else(|e| die(&format!("cannot write helper: {e}")));
    make_executable(&helper);

    // --- 6. Emit the DAG JSON: one boxed step per cell.
    let mut steps: Vec<Value> = Vec::with_capacity(budgets.len());
    let mut global_timeout = min_timeout;
    for b in &budgets {
        let cell_slug = cell_slug(&b.c.category, &b.c.test, &b.c.mode, &b.c.backend);
        let cell_dir = run_dir.join(&cell_slug);
        let cmd = format!(
            "bash {helper} {repo} {cell_dir} {lane} {bucket} {test} {mode} {backend}",
            helper = shell_quote(&helper.to_string_lossy()),
            repo = shell_quote(&repo.to_string_lossy()),
            cell_dir = shell_quote(&cell_dir.to_string_lossy()),
            lane = shell_quote(&lane),
            bucket = shell_quote(&b.c.category),
            test = shell_quote(&b.c.test),
            mode = shell_quote(&b.c.mode),
            backend = shell_quote(&b.c.backend),
        );
        global_timeout = global_timeout.max(b.timeout_s);
        steps.push(serde_json::json!({
            "group": b.c.category,
            "job": format!("{}__{}__{}", short(&b.c.test), b.c.mode, b.c.backend),
            "desc": format!("expansion cell {}/{} [{}] on {}", b.c.category, short(&b.c.test), b.c.mode, b.c.backend),
            "cmd": cmd,
            "deps": [],
            "timeout": b.timeout_s,
            "hint": {
                "est_duration_s": b.est_dur_s,
                "rss_baseline_bytes": (b.mem_bytes as f64 / headroom) as i64,
                "hard_mem_max_bytes": b.mem_bytes,
                "classification": "cpu-bound"
            }
        }));
    }

    let dag = serde_json::json!({
        "default_step_timeout": global_timeout,
        "resource_caps": {},
        "steps": steps,
    });
    let dag_json = serde_json::to_string_pretty(&dag).unwrap();
    let dag_path = dag_out.unwrap_or_else(|| run_dir.join("dag.json"));
    fs::write(&dag_path, &dag_json)
        .unwrap_or_else(|e| die(&format!("cannot write DAG {}: {e}", dag_path.display())));

    eprintln!(
        "expansion-dag: wrote DAG ({} steps) -> {}",
        budgets.len(),
        dag_path.display()
    );
    eprintln!("expansion-dag: evidence run dir -> {}", run_dir.display());
    eprintln!("expansion-dag: run it with 229's safe-ci-dag-runner, e.g.:");
    eprintln!(
        "  {}/agent-utils/rs/safe-ci-dag-runner/target/release/safe-ci-dag-runner run \\\n    --dag {} --max-mem <BUDGET>",
        repo.display(),
        dag_path.display()
    );
    // stdout = the DAG path, for scripting.
    println!("{}", dag_path.display());
}

// ---- helpers ---------------------------------------------------------------

fn next(it: &mut impl Iterator<Item = String>, flag: &str) -> String {
    it.next()
        .unwrap_or_else(|| die(&format!("{flag} needs a value")))
}
fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}
fn short(test: &str) -> &str {
    test.rsplit('/').next().unwrap_or(test)
}
fn cell_key(bucket: &str, test: &str, mode: &str, backend: &str) -> String {
    format!("{bucket}|{test}|{mode}|{backend}")
}
fn cell_slug(bucket: &str, test: &str, mode: &str, backend: &str) -> String {
    let t = test.replace('/', "-");
    sanitize(&format!("{bucket}__{t}__{mode}__{backend}"))
}
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r#"'\''"#))
}

/// Parse the scorecard CSV. Returns (ptrace baselines by test, all green cells,
/// set of green cell-keys). "green" = outcome==pass.
fn parse_csv(
    path: &Path,
    lane: &str,
) -> (
    BTreeMap<String, Baseline>,
    Vec<GreenCell>,
    std::collections::HashSet<String>,
) {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|e| die(&format!("cannot read {}: {e}", path.display())));
    let mut lines = text.lines();
    let header = lines.next().unwrap_or_else(|| die("empty CSV"));
    let cols: Vec<&str> = header.split(',').collect();
    let idx = |name: &str| {
        cols.iter()
            .position(|c| *c == name)
            .unwrap_or_else(|| die(&format!("CSV missing column '{name}'")))
    };
    let (i_lane, i_bucket, i_test, i_mode, i_backend, i_outcome, i_dur, i_rss) = (
        idx("lane"),
        idx("bucket"),
        idx("test_id"),
        idx("test_mode"),
        idx("backend"),
        idx("outcome"),
        idx("duration_ms"),
        idx("max_rss_kb"),
    );

    let mut baselines: BTreeMap<String, Baseline> = BTreeMap::new();
    let mut greens: Vec<GreenCell> = Vec::new();
    let mut green_keys = std::collections::HashSet::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = split_csv_line(line);
        let get = |i: usize| f.get(i).map(|s| s.as_str()).unwrap_or("");
        if get(i_lane) != lane || get(i_outcome) != "pass" {
            continue;
        }
        let test = get(i_test).to_string();
        let backend = get(i_backend).to_string();
        let dur = get(i_dur).parse::<f64>().ok();
        let rss = get(i_rss).parse::<f64>().ok();
        green_keys.insert(cell_key(get(i_bucket), &test, get(i_mode), &backend));
        if backend == "ptrace" {
            // Keep the fastest ptrace observation per test as the baseline.
            let e = baselines.entry(test.clone()).or_default();
            if let Some(d) = dur {
                e.dur_ms = Some(e.dur_ms.map_or(d, |o| o.min(d)));
            }
            if let Some(r) = rss {
                e.rss_kb = Some(e.rss_kb.map_or(r, |o| o.max(r)));
            }
        }
        greens.push(GreenCell {
            test,
            backend,
            dur_ms: dur,
            rss_kb: rss,
        });
    }
    (baselines, greens, green_keys)
}

/// Geo-mean wall-time + max-mem ratio vs ptrace across tests green in BOTH the
/// backend and ptrace. Returns backend -> (time_ratio, mem_ratio, n_samples).
fn geomean_ratios(
    baselines: &BTreeMap<String, Baseline>,
    greens: &[GreenCell],
    backends: &[String],
) -> BTreeMap<String, (f64, f64, usize)> {
    let mut out = BTreeMap::new();
    for backend in backends {
        let mut log_t = 0.0_f64;
        let mut nt = 0usize;
        let mut log_m = 0.0_f64;
        let mut nm = 0usize;
        for g in greens.iter().filter(|g| &g.backend == backend) {
            let base = match baselines.get(&g.test) {
                Some(b) => b,
                None => continue,
            };
            if let (Some(gt), Some(bt)) = (g.dur_ms, base.dur_ms) {
                if gt > 0.0 && bt > 0.0 {
                    log_t += (gt / bt).ln();
                    nt += 1;
                }
            }
            if let (Some(gm), Some(bm)) = (g.rss_kb, base.rss_kb) {
                if gm > 0.0 && bm > 0.0 {
                    log_m += (gm / bm).ln();
                    nm += 1;
                }
            }
        }
        let rt = if nt > 0 {
            (log_t / nt as f64).exp()
        } else {
            fallback_time_ratio(backend)
        };
        let rm = if nm > 0 {
            (log_m / nm as f64).exp()
        } else {
            fallback_mem_ratio(backend)
        };
        out.insert(
            backend.clone(),
            (
                rt,
                rm,
                nt.min(nm)
                    .max(if nt > 0 || nm > 0 { nt.max(nm) } else { 0 }),
            ),
        );
    }
    out
}

struct Cell {
    test: String,
    category: String,
    mode: String,
    backend: String,
}

/// Full cell superset = enabled plan ∪ disabled gaps (dedup on identity).
fn enumerate_cells(repo: &Path, lane: &str) -> Vec<Cell> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for sub in ["plan", "audit-gaps"] {
        let json = harness_json(repo, sub, lane);
        let arr = json.as_array().cloned().unwrap_or_default();
        for row in arr {
            let g = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let c = Cell {
                test: g("test"),
                category: g("category"),
                mode: g("mode"),
                backend: g("backend"),
            };
            let k = cell_key(&c.category, &c.test, &c.mode, &c.backend);
            if c.backend.is_empty() || !seen.insert(k) {
                continue;
            }
            out.push(c);
        }
    }
    out
}

fn harness_json(repo: &Path, sub: &str, lane: &str) -> Value {
    let out = Command::new("./ci/test_harness.sh")
        .current_dir(repo)
        .args([sub, "--lane", lane, "--format", "json"])
        .output()
        .unwrap_or_else(|e| die(&format!("failed to run test_harness.sh {sub}: {e}")));
    if !out.status.success() {
        die(&format!(
            "test_harness.sh {sub} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| die(&format!("test_harness.sh {sub} emitted invalid JSON: {e}")))
}

/// Minimal RFC-4180-ish CSV field split (handles quoted fields + `""` escape).
fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_q {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_q = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_q = true;
        } else if c == ',' {
            fields.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    fields.push(cur);
    fields
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

fn default_repo() -> PathBuf {
    // Prefer $PWD if it is a hermit checkout, else the workspace primary.
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if cwd.join("ci/test_harness.sh").is_file() {
        return cwd;
    }
    // script lives in <workspace>/compat-envelope/ ; sibling hermit is the primary
    let ws = script_dir().parent().map(Path::to_path_buf).unwrap_or(cwd);
    ws.join("hermit")
}

fn default_evidence_root(repo: &Path) -> PathBuf {
    // The scorecard + evidence live in the OUTER dev-hermit repo, gitignored.
    let ws = script_dir()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            repo.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        });
    ws.join("ignored/compat-envelope")
}

fn now_stamp() -> String {
    let out = Command::new("date").args(["-u", "+%Y%m%d-%H%M%S"]).output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "run".to_string(),
    }
}

/// Keep the last K dated run dirs under `root`, remove older ones.
fn rotate(root: &Path, keep: usize) {
    let mut dirs: Vec<PathBuf> = match fs::read_dir(root) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(_) => return,
    };
    if dirs.len() <= keep {
        return;
    }
    dirs.sort(); // dated names sort chronologically
    let remove = dirs.len() - keep;
    for d in dirs.into_iter().take(remove) {
        let _ = fs::remove_dir_all(&d);
        eprintln!("expansion-dag: rotated out old run dir {}", d.display());
    }
}

fn make_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(md) = fs::metadata(p) {
        let mut perm = md.permissions();
        perm.set_mode(0o755);
        let _ = fs::set_permissions(p, perm);
    }
}

/// Per-cell runner: runs one cell (and, for non-ptrace, a ptrace reference)
/// through the REAL harness so mode semantics match regression exactly, and
/// captures stdout/stderr/INFO-log/stats into the cell evidence dir. The
/// cgroup+timeout box is supplied by safe-ci-dag-runner, not here.
const CELL_RUNNER: &str = r#"#!/usr/bin/env bash
# run-expansion-cell.sh <repo> <cell_dir> <lane> <bucket> <test_id> <mode> <backend>
# Captures per-cell evidence; exits with the target backend's outcome.
set -u
repo="$1"; cell="$2"; lane="$3"; bucket="$4"; test="$5"; mode="$6"; backend="$7"
mkdir -p "$cell"

run_one() { # <backend> <outdir>
  local be="$1" out="$2"
  mkdir -p "$out"
  ( cd "$repo" && ./ci/test_harness.sh run \
      --lane "$lane" --category "$bucket" --test "$test" --mode "$mode" \
      --backend "$be" --include-manual \
      --results "$out/results.jsonl" ) >"$out/stdout" 2>"$out/stderr"
  local rc=$?
  # INFO log = the --log=info stream the harness routes to stderr.
  grep -aE ' (INFO|WARN|ERROR) ' "$out/stderr" > "$out/info.log" 2>/dev/null || true
  # Machine-readable exec stats = the harness's own per-cell JSONL.
  if [ -s "$out/results.jsonl" ]; then
    cp "$out/results.jsonl" "$out/stats.json"
  else
    printf '{"backend":"%s","exit":%d,"note":"no harness JSONL emitted"}\n' "$be" "$rc" > "$out/stats.json"
  fi
  return $rc
}

run_one "$backend" "$cell"
rc=$?
if [ "$backend" != "ptrace" ]; then
  run_one ptrace "$cell/ptrace-ref" || true
fi
exit $rc
"#;
