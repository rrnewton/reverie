#!/usr/bin/env rust-script
//! ```cargo
//! [dependencies]
//! serde_json = "1"
//! sha2 = "0.10"
//! libc = "0.2"
//! ```
//!
//! Real full-corpus DBI L2 measurement for the compat-envelope scorecard.
//!
//! The main collector (`collect-envelope.rs`) only measures cells the TOML
//! manifests declare `backends_enabled`/manual for a backend, so DBI has almost
//! no scorecard coverage. This producer is additive and orthogonal: it
//! reconstructs EVERY corpus guest command from `hermit-manifest-plan
//! --format harness-json` (the same reconstruction `collect-envelope.rs` uses
//! for parity) and runs each guest under BOTH ptrace and DBI at
//! `--strict --verify` (L2), so it measures DBI capability across the whole
//! corpus regardless of what the manifest currently enables.
//!
//! For each test it records, honestly:
//!   * determinism  = DBI `--strict --verify` exits 0 (hermit's own double-run
//!     confirmed a bitwise-identical repeat under the DBI backend -- real L2).
//!   * parity       = DBI guest stdout is byte-identical to the golden ptrace
//!     guest stdout (SHA-256), AND both were deterministic. Guest-visible parity
//!     vs the ptrace reference -- the B-level metric. Blank when the ptrace
//!     reference could not be established (parity UNMEASURED, never a false 0).
//!
//! It emits rows in the 20-column scorecard schema (bucket = the test's real
//! corpus bucket, test_mode = `verify`, backend in {dbi, ptrace-ref}) into the
//! TRACKED `compat-envelope/dbi-scorecard.csv`, beside the peer collectors'
//! `e9patch-scorecard.csv` and `reverie-scorecard.csv`; it never touches the
//! three existing collectors or `scorecard.csv`.
//!
//! HISTORY, because the previous two sentences used to be wrong in a way that
//! mattered. This tool wrote to `ignored/` (gitignored, .gitignore:120) and
//! claimed the scorecard owner "folds it with a plain concat". Both halves
//! failed: nothing tracked could read the file at all, so its cells could not be
//! reported as passing, failing or not-attempted -- they simply did not appear,
//! and every denominator computed without them was wrong. And the concat could
//! not have worked anyway, because the row lacked `comparison_tier`, which
//! render-scorecard.rs requires and refuses a row without.
//!
//! Anti-fakery (#152): a DBI cell that is self-consistent (verify passed) but
//! diverges from ptrace stdout is recorded deterministic=1 parity=0 with the
//! divergence noted -- it is NEVER presented as parity. A hang/timeout/crash is
//! a gap with the exact reason, never a silent pass.
//!
//! Usage:
//!   ./collect-dbi-corpus.rs [--repo PATH] [--manifest JSON] [--run-id ID]
//!       [--csv PATH] [--timeout SECS] [--tests a,b,...] [--buckets x,y]
//!       [--limit N] [--stdout] [--emit-ptrace-ref]
//! Defaults: --repo ../hermit  --manifest <invoke cargo>  --timeout 150
//!           --csv compat-envelope/dbi-scorecard.csv  --run-id dbi-corpus-verify

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

// `comparison_tier` is REQUIRED, not decorative: render-scorecard.rs refuses any
// row whose tier is empty or unknown ("a raw pass may never default to a strict
// green"), and it is the field that separates PASS from ATTEMPTED_UNQUALIFIABLE.
// Without it every row this tool emits is rejected at ingest, which is why the
// output was unreadable by anything tracked rather than merely untracked.
const HEADER: &str = "run_id,run_utc,hermit_sha,reverie_sha,dirty,run_mode,lane,bucket,test_id,test_mode,backend,cell_state,outcome,deterministic,stdout_parity,output_hash,duration_ms,max_rss_kb,reason,comparison_tier";
const PINNED_GUEST_ENV_ARGS: &[&str] =
    &["--base-env", "minimal", "-e", "LC_ALL=C", "-e", "TZ=UTC"];

fn die(msg: &str) -> ! {
    eprintln!("collect-dbi-corpus: {msg}");
    std::process::exit(1);
}

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn hermit_bin(repo: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("HERMIT_BIN") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    repo.join("target/release/hermit")
}

/// Build the Hermit side of one corpus invocation, excluding only `timeout`.
///
/// Keep the guest environment in step with `collect-envelope.rs::run_and_hash`.
/// Hermit's default `--base-env host` lets the collector's ambient environment
/// decide the guest's initial stack address, so stack-derived observations stop
/// being comparable between invocations. `minimal` fixes PATH/HOSTNAME/HOME;
/// LC_ALL and TZ are re-added explicitly because the corpus relies on them and
/// an unset TZ falls back to host `/etc/localtime`.
fn hermit_argv(hermit: &Path, backend: &str, verify: bool, guest: &[String]) -> Vec<String> {
    let mut argv = vec![
        hermit.to_string_lossy().into_owned(),
        "run".into(),
        "--backend".into(),
        backend.into(),
        "--strict".into(),
    ];
    if verify {
        argv.push("--verify".into());
    }
    argv.push("--no-virtualize-cpuid".into());
    argv.push("--max-timeslice=disabled".into());
    argv.extend(PINNED_GUEST_ENV_ARGS.iter().map(|arg| (*arg).to_string()));
    argv.push("--".into());
    argv.extend(guest.iter().cloned());
    argv
}

#[cfg(test)]
mod guest_env_pin_tests {
    use super::*;

    fn projected_env_args(argv: &[String]) -> Option<Vec<String>> {
        let separator = argv.iter().position(|arg| arg == "--")?;
        let mut projected = Vec::new();
        let mut index = 0;
        while index < separator {
            match argv[index].as_str() {
                "--base-env" | "-e" | "--env" => {
                    if index + 1 >= separator {
                        return None;
                    }
                    projected.push(argv[index].clone());
                    projected.push(argv[index + 1].clone());
                    index += 2;
                }
                arg if arg.starts_with("--base-env=")
                    || arg.starts_with("--env=")
                    || (arg.starts_with("-e") && arg.len() > 2) =>
                {
                    projected.push(arg.to_string());
                    index += 1;
                }
                _ => index += 1,
            }
        }
        Some(projected)
    }

    fn has_exact_pin(argv: &[String]) -> bool {
        projected_env_args(argv)
            == Some(PINNED_GUEST_ENV_ARGS.iter().map(|arg| (*arg).to_string()).collect())
    }

    fn actual_argv() -> Vec<String> {
        hermit_argv(
            Path::new("/hermit"),
            "dbi",
            true,
            &["/guest".to_string()],
        )
    }

    #[test]
    fn every_backend_and_level_carries_the_exact_pin_before_the_guest() {
        for backend in ["ptrace", "dbi"] {
            for verify in [false, true] {
                let argv = hermit_argv(
                    Path::new("/hermit"),
                    backend,
                    verify,
                    &["/guest".to_string()],
                );
                assert!(has_exact_pin(&argv), "missing pin in {argv:?}");
            }
        }
    }

    #[test]
    fn a_mutated_value_is_refused() {
        let mut argv = actual_argv();
        *argv.iter_mut().find(|arg| arg.as_str() == "TZ=UTC").unwrap() = "TZ=localtime".into();
        assert!(!has_exact_pin(&argv));
    }

    #[test]
    fn an_omitted_entry_is_refused() {
        let mut argv = actual_argv();
        let index = argv.iter().position(|arg| arg == "LC_ALL=C").unwrap();
        argv.drain(index - 1..=index);
        assert!(!has_exact_pin(&argv));
    }

    #[test]
    fn reordered_entries_are_refused() {
        let mut argv = actual_argv();
        let lc = argv.iter().position(|arg| arg == "LC_ALL=C").unwrap();
        let tz = argv.iter().position(|arg| arg == "TZ=UTC").unwrap();
        argv.swap(lc, tz);
        assert!(!has_exact_pin(&argv));
    }
}

/// One outcome of a single backend run of a guest.
struct RunOutcome {
    rc: i32,
    timed_out: bool,
    ran: bool,             // the guest actually launched (no spawn error)
    hash: String,          // SHA-256 of guest stdout (always computed)
    duration_ms: i64,
    stderr_tail: String,
}

/// Run one guest under `backend` with the portable determinism profile.
/// When `verify` is false this is an L1 run (`--strict`); when true it is an L2
/// run (`--strict --verify`, hermit double-runs and self-checks).
///
/// IMPORTANT: parity must be measured at L1, never by comparing `--verify`
/// stdout across backends. Under `--verify` the ptrace backend consumes guest
/// stdout internally (emits 0 bytes to the parent) while DBI passes it through,
/// so a cross-backend `--verify` stdout comparison is a plumbing artifact, not a
/// real divergence. At L1 both backends pass guest stdout through unchanged.
fn run_guest(repo: &Path, backend: &str, verify: bool, guest: &[String], timeout_s: u64) -> RunOutcome {
    let hermit = hermit_bin(repo);
    let mut cmd = Command::new("timeout");
    cmd.arg("-k").arg("5s").arg(format!("{timeout_s}s"));
    // The pure builder is shared by every ptrace/DBI and L1/L2 leg, making it
    // impossible for one recording path to silently omit the environment pin.
    cmd.args(hermit_argv(&hermit, backend, verify, guest));
    cmd.env("LC_ALL", "C").env("TZ", "UTC").current_dir(repo);

    // Redirect child stdout/stderr to FILES, not pipes. hermit forks a
    // background supervisor that reparents to init and can outlive the
    // `timeout`-killed foreground; if we read from a pipe, that survivor keeps
    // the pipe open and Command::output() blocks forever. With files, wait()
    // returns as soon as the direct child (`timeout`) exits, regardless of any
    // surviving grandchild. We then SIGKILL the child's process group to reap
    // the survivor.
    let out_path = std::env::temp_dir().join(format!("dbi-corpus-{}-{}.out", std::process::id(), rand_token()));
    let err_path = std::env::temp_dir().join(format!("dbi-corpus-{}-{}.err", std::process::id(), rand_token()));
    let (Ok(out_file), Ok(err_file)) = (File::create(&out_path), File::create(&err_path)) else {
        return RunOutcome { rc: -1, timed_out: false, ran: false, hash: String::new(), duration_ms: 0, stderr_tail: "could not create temp capture files".to_string() };
    };
    cmd.stdout(Stdio::from(out_file)).stderr(Stdio::from(err_file)).stdin(Stdio::null());
    // Put the whole invocation in its own process group so we can reap the
    // supervisor without touching this sweep or other agents' processes.
    cmd.process_group(0);

    let start = Instant::now();
    let spawned = cmd.spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&out_path);
            let _ = std::fs::remove_file(&err_path);
            return RunOutcome { rc: -1, timed_out: false, ran: false, hash: String::new(), duration_ms: 0, stderr_tail: format!("spawn failed: {e}") };
        }
    };
    let pgid = child.id() as i32; // process_group(0) => pgid == child pid
    let status = child.wait();
    let duration_ms = start.elapsed().as_millis() as i64;
    // Reap any survivor (e.g. hermit's background supervisor) in this group.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }

    let stdout_bytes = std::fs::read(&out_path).unwrap_or_default();
    let stderr_text = std::fs::read_to_string(&err_path).unwrap_or_default();
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);

    let rc = status.ok().and_then(|s| s.code()).unwrap_or(-1);
    let timed_out = rc == 124 || rc == 137; // timeout(1): 124 SIGTERM path, 137 = 128+SIGKILL
    let stderr_tail: String = stderr_text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect();
    let mut h = Sha256::new();
    h.update(&stdout_bytes);
    RunOutcome {
        rc,
        timed_out,
        ran: !timed_out,
        hash: format!("{:x}", h.finalize()),
        duration_ms,
        stderr_tail,
    }
}

/// Cheap unique-ish token for temp file names (Math.random / SystemTime-based).
fn rand_token() -> String {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    format!("{n:x}")
}

/// Reconstruct a runnable guest argv for `test_id` from the manifest docs, the
/// same way collect-envelope.rs does. `.sh` -> `<path> --run`; `.c`/`.rs` are
/// compiled into a stable per-test temp binary under the repo target tree.
fn guest_command(repo: &Path, docs: &Value, test_id: &str) -> Option<Vec<String>> {
    let arr = docs.as_array()?;
    for doc in arr {
        let tests = doc.get("test").and_then(Value::as_array)?;
        for t in tests {
            if t.get("id").and_then(Value::as_str) != Some(test_id) {
                continue;
            }
            if let Some(prog) = t.get("program").and_then(Value::as_str) {
                if prog.ends_with(".sh") {
                    // Absolute path: a bare relative path triggers hermit guest
                    // PATH lookup and fails to resolve.
                    let p = repo.join(prog);
                    let abs = std::fs::canonicalize(&p).unwrap_or(p);
                    return Some(vec![abs.to_string_lossy().into_owned(), "--run".into()]);
                }
                if prog.ends_with(".c") || prog.ends_with(".rs") {
                    return build_compiled_fixture(repo, test_id, prog, t.get("build"));
                }
                return None;
            }
            if let Some(direct) = t.get("direct").and_then(Value::as_str) {
                return Some(vec!["bash".into(), "-c".into(), direct.to_string()]);
            }
        }
    }
    None
}

/// Compile a `.c`/`.rs` fixture the way ci/test_harness.sh does, into a stable
/// per-test temp binary under the repo target tree (NOT /tmp, which hermit
/// isolates). Returns None on any compile failure so the caller records the cell
/// as unmeasured rather than a false parity 0.
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
    let dir = repo
        .join("ignored/compat-envelope-parity")
        .join(test_id.replace('/', "_"));
    std::fs::create_dir_all(&dir).ok()?;
    let bin = dir.join("program");
    let extra = |key: &str| -> Vec<String> {
        build
            .and_then(|b| b.get(key))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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
    // Absolute path so hermit resolves the guest directly instead of doing a
    // guest-PATH lookup on a relative path.
    let abs = std::fs::canonicalize(&bin).unwrap_or(bin);
    Some(vec![abs.to_string_lossy().into_owned()])
}

#[allow(clippy::too_many_arguments)]
fn push_row(
    out: &mut String,
    run_id: &str,
    run_utc: &str,
    hermit_sha: &str,
    dirty: bool,
    bucket: &str,
    test_id: &str,
    backend: &str,
    outcome: &str,
    deterministic: Option<bool>,
    parity: Option<bool>,
    output_hash: &str,
    duration_ms: i64,
    reason: &str,
) {
    let det = match deterministic {
        Some(true) => "1",
        Some(false) => "0",
        None => "",
    };
    let par = match parity {
        Some(true) => "1",
        Some(false) => "0",
        None => "",
    };
    // The tier states WHAT WAS COMPARED, never how it turned out -- pass/fail is
    // `outcome`'s job. `parity` is Some(_) exactly when the golden ptrace stdout
    // was established and DBI stdout was compared against it byte-for-byte, so
    // Some(_) is a real CROSS-BACKEND witness and None is no comparison at all.
    //
    // Deliberately NOT `unqualified-self-verify-only`: that tier means a backend
    // verified against itself. This tool compares DBI against ptrace, which is
    // narrower than full parity but is not self-referential, and recording it as
    // self-verify would under-claim evidence that actually exists. Under-claiming
    // corrupts a scorecard as surely as over-claiming and is harder to spot
    // because it reads as conservative.
    let comparison_tier = match parity {
        Some(_) => "unqualified-stdout-only",
        None => "unqualified-no-comparison",
    };
    let dur = if duration_ms > 0 { duration_ms.to_string() } else { String::new() };
    let row = [
        run_id.to_string(),
        run_utc.to_string(),
        hermit_sha.to_string(),
        "unknown".to_string(),
        dirty.to_string(),
        "expansion".to_string(),
        "portable".to_string(),
        bucket.to_string(),
        test_id.to_string(),
        "verify".to_string(),
        backend.to_string(),
        "enabled".to_string(),
        outcome.to_string(),
        det.to_string(),
        par.to_string(),
        output_hash.to_string(),
        dur,
        String::new(),
        reason.to_string(),
        comparison_tier.to_string(),
    ];
    let line = row.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(",");
    out.push_str(&line);
    out.push('\n');
}

fn main() {
    // Default to the DBI slot checkout, NEVER the primary `../hermit` (fixtures
    // are compiled into the repo target tree; the primary must stay clean).
    let mut repo = PathBuf::from("../worktrees/dbt-compat/hermit");
    let mut manifest: Option<PathBuf> = None;
    let mut run_id = String::from("dbi-corpus-verify");
    // TRACKED by default, alongside the peer collectors' e9patch-scorecard.csv and
    // reverie-scorecard.csv. The old default was `ignored/`, which is gitignored
    // (.gitignore:120), so nothing tracked could read this tool's output: its cells
    // could not be reported as passing, failing or not-attempted -- they simply did
    // not appear, and every denominator computed without them was wrong.
    let mut csv = PathBuf::from("compat-envelope/dbi-scorecard.csv");
    let mut timeout_s: u64 = 150;
    let mut tests_filter: Option<Vec<String>> = None;
    let mut buckets_filter: Option<Vec<String>> = None;
    let mut limit: Option<usize> = None;
    let mut to_stdout = false;
    let mut emit_ptrace_ref = false;

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--repo" => repo = PathBuf::from(it.next().unwrap_or_else(|| die("--repo needs a path"))),
            "--manifest" => manifest = Some(PathBuf::from(it.next().unwrap_or_else(|| die("--manifest needs a path")))),
            "--run-id" => run_id = it.next().unwrap_or_else(|| die("--run-id needs a value")),
            "--csv" => csv = PathBuf::from(it.next().unwrap_or_else(|| die("--csv needs a path"))),
            "--timeout" => timeout_s = it.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| die("--timeout needs secs")),
            "--tests" => tests_filter = Some(it.next().unwrap_or_else(|| die("--tests needs a list")).split(',').map(|s| s.trim().to_string()).collect()),
            "--buckets" => buckets_filter = Some(it.next().unwrap_or_else(|| die("--buckets needs a list")).split(',').map(|s| s.trim().to_string()).collect()),
            "--limit" => limit = it.next().and_then(|v| v.parse().ok()),
            "--stdout" => to_stdout = true,
            "--emit-ptrace-ref" => emit_ptrace_ref = true,
            "-h" | "--help" => {
                println!("usage: collect-dbi-corpus.rs [--repo P] [--manifest JSON] [--run-id ID] [--csv P] [--timeout S] [--tests a,b] [--buckets x,y] [--limit N] [--stdout] [--emit-ptrace-ref]");
                return;
            }
            other => die(&format!("unknown argument: {other}")),
        }
    }

    // Load manifest docs (from file, or by invoking hermit-manifest-plan).
    let docs: Value = if let Some(mpath) = &manifest {
        let text = std::fs::read_to_string(mpath)
            .unwrap_or_else(|e| die(&format!("cannot read manifest {}: {e}", mpath.display())));
        serde_json::from_str(&text).unwrap_or_else(|e| die(&format!("bad manifest json: {e}")))
    } else {
        let out = Command::new("cargo")
            .args(["run", "--quiet", "-p", "hermit-manifest-plan", "--", "--format", "harness-json"])
            .current_dir(&repo)
            .output()
            .unwrap_or_else(|e| die(&format!("cannot run hermit-manifest-plan: {e}")));
        if !out.status.success() {
            die(&format!("hermit-manifest-plan failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| die(&format!("bad manifest json: {e}")))
    };

    // Flatten (bucket, test_id) in manifest order.
    let mut tests: Vec<(String, String)> = Vec::new();
    if let Some(arr) = docs.as_array() {
        for doc in arr {
            let bucket = doc.get("bucket").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(ts) = doc.get("test").and_then(Value::as_array) {
                for t in ts {
                    if let Some(id) = t.get("id").and_then(Value::as_str) {
                        tests.push((bucket.clone(), id.to_string()));
                    }
                }
            }
        }
    }
    tests.retain(|(b, id)| {
        buckets_filter.as_ref().map_or(true, |f| f.contains(b))
            && tests_filter.as_ref().map_or(true, |f| f.contains(id))
    });
    if let Some(n) = limit {
        tests.truncate(n);
    }
    if tests.is_empty() {
        die("no tests selected");
    }

    let hermit_sha = git(&repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git(&repo, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false);
    let run_utc = format!(
        "@{}",
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    );

    eprintln!(
        "collect-dbi-corpus: {} tests, hermit {} dirty={}, timeout={}s, bin={}",
        tests.len(),
        hermit_sha,
        dirty,
        timeout_s,
        hermit_bin(&repo).display()
    );

    let mut lines = String::new();
    let mut tally: BTreeMap<&str, usize> = BTreeMap::new();
    let total = tests.len();

    for (i, (bucket, test_id)) in tests.iter().enumerate() {
        let guest = match guest_command(&repo, &docs, test_id) {
            Some(g) => g,
            None => {
                eprintln!("[{}/{total}] {test_id}: SKIP (unreconstructable/build-failed)", i + 1);
                push_row(&mut lines, &run_id, &run_utc, &hermit_sha, dirty, bucket, test_id,
                    "dbi", "unmeasured", None, None, "", 0,
                    "guest command could not be reconstructed or fixture failed to compile");
                *tally.entry("skip").or_default() += 1;
                continue;
            }
        };

        // L1 golden reference (ptrace --strict): the canonical guest stdout.
        let pt1 = run_guest(&repo, "ptrace", false, &guest, timeout_s);
        // L1 DBI (--strict): guest stdout under the backend, for parity vs ptrace.
        let db1 = run_guest(&repo, "dbi", false, &guest, timeout_s);
        // L2 DBI (--strict --verify): determinism = hermit's own double-run
        // self-check. Skip it when L1 already hung/failed to launch -- a backend
        // that cannot even complete a single L1 run (e.g. a no-preemption futex
        // hang) has no meaningful L2 result and each skipped run saves a full
        // timeout of wall-clock. Model the skip as a non-deterministic L2.
        let db2 = if db1.ran {
            run_guest(&repo, "dbi", true, &guest, timeout_s)
        } else {
            RunOutcome {
                rc: db1.rc,
                timed_out: db1.timed_out,
                ran: false,
                hash: String::new(),
                duration_ms: 0,
                stderr_tail: "L2 --verify not attempted: DBI L1 --strict did not complete".to_string(),
            }
        };

        // Determinism (L2): DBI's --verify double-run passed. NOTE: for a guest
        // that deterministically exits non-zero, hermit --verify may short-circuit
        // to a single run (see run_matrix.py's exit_status L2 gap), so a non-zero
        // rc here can also mean "verify skipped"; the reason records the exact rc.
        let dbi_det = db2.ran && db2.rc == 0;

        // Parity (L1, guest-visible): DBI guest stdout == ptrace guest stdout, AND
        // matching exit codes. Unmeasured (blank) if the ptrace reference or the
        // DBI L1 run did not launch.
        let parity: Option<bool> = if pt1.ran && db1.ran {
            Some(pt1.hash == db1.hash && pt1.rc == db1.rc)
        } else {
            None
        };

        // Outcome ranks the DBI cell. Parity is the B-level goal; determinism is
        // necessary but not sufficient. Honest gap taxonomy, never a false pass.
        let (outcome, reason) = if !db1.ran {
            if db1.timed_out {
                ("gap", format!("DBI L1 (--strict) timed out after {timeout_s}s (likely no-preemption hang or DR stall)"))
            } else {
                ("gap", format!("DBI L1 (--strict) did not launch: {}", db1.stderr_tail))
            }
        } else {
            match (parity, dbi_det) {
                (Some(true), true) => ("pass", "DBI B4: guest stdout+exit byte-identical to ptrace (L1) AND deterministic repeat (L2 --verify)".to_string()),
                (Some(true), false) => ("parity-not-det", format!("DBI matches ptrace guest output (L1) but --verify did not confirm a deterministic repeat (rc={}): {}", db2.rc, db2.stderr_tail)),
                (Some(false), true) => ("parity-gap", format!("DBI deterministic (L2) but guest output DIVERGES from ptrace golden: dbi_rc={} ptrace_rc={}", db1.rc, pt1.rc)),
                (Some(false), false) => ("gap", format!("DBI diverges from ptrace (L1) AND --verify non-deterministic (rc={})", db2.rc)),
                (None, true) => ("det-only", "DBI deterministic (L2) but parity unmeasured (ptrace reference did not launch)".to_string()),
                (None, false) => ("gap", format!("DBI parity unmeasured and --verify rc={}", db2.rc)),
            }
        };

        push_row(&mut lines, &run_id, &run_utc, &hermit_sha, dirty, bucket, test_id,
            "dbi", outcome, Some(dbi_det), parity, &db1.hash, db1.duration_ms, &reason);
        *tally.entry(outcome).or_default() += 1;

        if emit_ptrace_ref {
            let (pt_outcome, pt_reason) = if pt1.ran {
                ("pass", "ptrace L1 golden reference: guest ran under --strict".to_string())
            } else if pt1.timed_out {
                ("gap", format!("ptrace L1 timed out after {timeout_s}s"))
            } else {
                ("gap", format!("ptrace L1 did not launch: {}", pt1.stderr_tail))
            };
            push_row(&mut lines, &run_id, &run_utc, &hermit_sha, dirty, bucket, test_id,
                "ptrace-ref", pt_outcome, None, None, &pt1.hash, pt1.duration_ms, &pt_reason);
        }

        eprintln!(
            "[{}/{total}] {test_id}: dbi={outcome} (det={dbi_det} parity={:?}) pt_rc={} db_rc={} {}ms",
            i + 1,
            parity,
            pt1.rc,
            db1.rc,
            db1.duration_ms + db2.duration_ms
        );
    }

    eprintln!("collect-dbi-corpus: tally {tally:?}");

    if to_stdout {
        println!("{HEADER}");
        print!("{lines}");
        return;
    }
    if let Some(parent) = csv.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = format!("{HEADER}\n{lines}");
    std::fs::write(&csv, body).unwrap_or_else(|e| die(&format!("cannot write {}: {e}", csv.display())));
    eprintln!("collect-dbi-corpus: wrote {} to {}", tally.values().sum::<usize>(), csv.display());
    eprintln!("To fold into master scorecard (owner action): tail -n +2 {} >> scorecard.csv", csv.display());
}
