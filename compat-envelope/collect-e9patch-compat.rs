#!/usr/bin/env rust-script
//! e9patch compat-envelope collector: **preprocessing-invariance** of the ptrace
//! backend under e9tool AOT rewriting, written into the SAME 19-column scorecard
//! CSV schema that `collect-envelope.rs` / `collect-reverie-compat.rs` /
//! `render-scorecard.rs` use, but into its OWN CSV (`e9patch-scorecard.csv`) and
//! bucket (`e9patch-corpus`), exactly as reverie tool-parity lives in its own
//! `reverie-scorecard.csv`.
//!
//! WHY A SEPARATE CSV, NOT A BACKEND COLUMN. `hermit/AGENTS.md` is explicit:
//! e9patch is NOT a Detcore backend — it is binary-rewriting preprocessing used
//! WITH the ptrace backend. The main `scorecard.csv` is backend-oriented and its
//! anti-fakery gate (#152) admits a cell only for a B1.5+ backend running the
//! shared Detcore tool. Cramming an "e9patch backend" column into `scorecard.csv`
//! would violate both. So this collector measures a DIFFERENT question and files
//! it separately:
//!
//!   For a freestanding raw-syscall guest, is the e9tool-rewritten ELF's output
//!   under the ptrace backend bitwise-identical (L2) to the same guest run under
//!   the ptrace backend WITHOUT rewriting (the "golden" reference)?
//!
//! The Detcore backend is `ptrace` in BOTH arms; only the AOT rewrite differs.
//! The two "backend" columns are therefore two ARMS of one preprocessing check —
//! `ptrace` (golden, un-rewritten reference) and `e9patch` (e9tool-rewritten
//! variant) — mirroring how reverie uses (ptrace, kvm) for its two guest
//! contracts. The renderer must label this section "e9patch preprocessing over
//! the ptrace backend, not a Detcore backend".
//!
//! HONEST L1 vs L2. `deterministic=1` means and ONLY means the arm reached L2:
//! `hermit run --strict --verify` printed "Determinism verified" AND exited
//! cleanly. An arm that ran under `--strict` (L1) but whose `--verify` leg did
//! not confirm bitwise repeat is recorded with `deterministic=0` and a `reason`
//! that says whether L2 was missed because the verify leg WEDGED (PMU env, not a
//! product defect) or genuinely DIVERGED (a real finding). An L1 result is never
//! reported as L2, and a wedged verify is never faked green or falsely blamed on
//! the guest.
//!
//! ```cargo
//! [dependencies]
//! libc = "0.2"
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
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::Write as _;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{exit, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// Shared comparison/provenance contract plus the e9patch reach tail. Keep the
// common prefix in sync with the other collectors and the renderer.
const HEADER: &str = "run_id,run_utc,hermit_sha,reverie_sha,dirty,run_mode,lane,bucket,test_id,test_mode,backend,cell_state,outcome,deterministic,stdout_parity,output_hash,duration_ms,max_rss_kb,reason,verify_compare,bitwise_parity,compared_log_messages,tier,legacy_parity_unqualified,ref_output_hash,parity_comparator,parity_tier,profile_flags,population_id,selected_count,executed_count,evidence_count,comparison_tier,candidate_sites,mapped_sites,reach_state,exit_code_parity,detlog_parity,oracle_verdict,stack_parity,heap_parity";
const RUN_MODE: &str = "e9patch";
const BUCKET: &str = "e9patch-corpus";
// Single mode token: "run the freestanding raw-syscall guest under strict verify".
const MODE: &str = "verify";
const FREESTANDING_FLAGS: &[&str] = &[
    "-nostdlib",
    "-static",
    "-ffreestanding",
    "-O0",
    "-fno-pie",
    "-no-pie",
];
const L2_NEEDLE: &str = "Determinism verified";
const PINNED_GUEST_ENV_ARGS: &[&str] = &["--base-env", "minimal", "-e", "LC_ALL=C", "-e", "TZ=UTC"];

fn die(msg: &str) -> ! {
    eprintln!("collect-e9patch-compat: {msg}");
    exit(2);
}

fn script_dir() -> PathBuf {
    if let Ok(p) = env::var("RUST_SCRIPT_BASE_PATH") {
        return PathBuf::from(p);
    }
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Result of one hermit invocation.
struct Run {
    /// Process exited (not wedged) with this code (127 if spawn/exit unknown).
    exit: i32,
    /// True if the run wedged and had to be process-group killed.
    wedged: bool,
    stdout: Vec<u8>,
    /// "Determinism verified" seen on stderr (only meaningful for --verify runs).
    l2_verified: bool,
    /// e9patch AOT reach, parsed from the backend banner. `None` for a run with
    /// no banner at all -- which is UNKNOWN, deliberately not zero: "the tool
    /// said nothing" and "the tool said it patched nothing" are different
    /// facts and collapsing them would rebuild the ambiguous zero this gate
    /// exists to remove.
    reach: Option<(u64, u64)>,
    duration_ms: i64,
}

/// Run one command with a timeout, its own process group, and a hard SIGKILL to
/// the whole group on wedge (mirrors the python driver's start_new_session +
/// killpg, which the leaky corpus `run()` lacks).
fn run_timed(cmd_argv: &[String], timeout: Duration) -> Run {
    let t0 = Instant::now();
    let out_tmp = env::temp_dir().join(format!("e9c-out-{}", std::process::id()));
    let err_tmp = env::temp_dir().join(format!("e9c-err-{}", std::process::id()));
    let out_file = File::create(&out_tmp).unwrap_or_else(|e| die(&format!("tmp out: {e}")));
    let err_file = File::create(&err_tmp).unwrap_or_else(|e| die(&format!("tmp err: {e}")));

    let mut cmd = Command::new(&cmd_argv[0]);
    cmd.args(&cmd_argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file));
    // New process group so a wedged supervisor tree can be killpg'd cleanly.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            return Run {
                exit: 127,
                wedged: false,
                stdout: Vec::new(),
                l2_verified: false,
                reach: None,
                duration_ms: 0,
            }
        }
    };
    let pid = child.id() as i32;
    let mut wedged = false;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(-1),
            Ok(None) => {
                if t0.elapsed() >= timeout {
                    // SIGKILL the whole group, then reap.
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                    let _ = child.wait();
                    wedged = true;
                    break 124;
                }
                sleep(Duration::from_millis(40));
            }
            Err(_) => break -1,
        }
    };
    let dur = t0.elapsed().as_millis() as i64;
    let stdout = fs::read(&out_tmp).unwrap_or_default();
    let stderr = fs::read(&err_tmp).unwrap_or_default();
    let _ = fs::remove_file(&out_tmp);
    let _ = fs::remove_file(&err_tmp);
    let err_text = String::from_utf8_lossy(&stderr);
    let l2_verified = err_text.contains(L2_NEEDLE);
    let reach = parse_reach(&err_text);
    Run {
        exit,
        wedged,
        stdout,
        l2_verified,
        reach,
        duration_ms: dur,
    }
}

/// Parse `candidate_sites=N; mapped_sites=M` out of the e9patch backend banner.
/// Returns None when there is no banner, which callers must treat as UNKNOWN
/// rather than as zero.
fn parse_reach(text: &str) -> Option<(u64, u64)> {
    let cand_at = text.find("candidate_sites=")? + "candidate_sites=".len();
    let cand: u64 = text[cand_at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    let map_at = text.find("mapped_sites=")? + "mapped_sites=".len();
    let mapped: u64 = text[map_at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((cand, mapped))
}

/// THE GATE. An e9patch cell that rewrote nothing ran under the plain ptrace
/// runtime, so its parity verdict compares the reference with itself. That is
/// not a pass and not a failure -- it is a MEASUREMENT THAT DID NOT HAPPEN, so
/// it gets its own bucket and can never be counted as a pass.
///
/// Applies only to e9patch; every other backend is returned untouched.
fn apply_reach_gate<'a>(
    backend: &str,
    reach: Option<(u64, u64)>,
    outcome: &'a str,
    reason: &'a str,
) -> (String, String) {
    if backend != "e9patch" {
        return (outcome.to_string(), reason.to_string());
    }
    match reach {
        Some((_, mapped)) if mapped > 0 => (outcome.to_string(), reason.to_string()),
        Some((cand, _)) => (
            "not-exercised".to_string(),
            format!(
                "guest has no main-ELF sites (candidate_sites={cand}, mapped_sites=0): \
                 the AOT pass rewrote nothing, so this ran as plain ptrace and any \
                 parity here compares the reference with itself"
            ),
        ),
        None => (
            "unknown".to_string(),
            "no e9patch banner: cannot tell whether the backend did anything, and an \
             unanswerable question is not a pass"
                .to_string(),
        ),
    }
}

/// sha256 of bytes via the sha256sum coreutil (dependency-free hashing).
fn sha256(bytes: &[u8]) -> String {
    let mut child = match Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(bytes);
    }
    match child.wait_with_output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string(),
        Err(_) => String::new(),
    }
}

fn hermit_argv(hermit: &Path, e9: bool, verify: bool, guest: &Path) -> Vec<String> {
    let mut v = vec![hermit.to_string_lossy().into_owned()];
    if e9 {
        v.push("--backend".into());
        v.push("e9patch".into());
    }
    v.push("run".into());
    v.push("--strict".into());
    // `--base-env host` (the default) makes the ambient collector environment
    // part of the guest's initial stack. That invalidates comparisons across
    // invocations as soon as they include a stack-derived observable. Use the
    // same fixed guest environment as the other scorecard collectors instead:
    // Hermit's minimal PATH/HOSTNAME/HOME plus explicit locale and timezone.
    v.extend(PINNED_GUEST_ENV_ARGS.iter().map(|arg| (*arg).to_string()));
    if verify {
        v.push("--verify".into());
    }
    // Keep hermit from replacing the guest's /tmp so the binary path resolves
    // (mirrors the corpus driver).
    v.push("--tmp=/tmp".into());
    v.push("--".into());
    v.push(guest.to_string_lossy().into_owned());
    v
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
            == Some(
                PINNED_GUEST_ENV_ARGS
                    .iter()
                    .map(|arg| (*arg).to_string())
                    .collect(),
            )
    }

    fn actual_argv() -> Vec<String> {
        hermit_argv(Path::new("/hermit"), true, true, Path::new("/guest"))
    }

    #[test]
    fn every_arm_and_level_carries_the_exact_pin_before_the_guest() {
        for e9 in [false, true] {
            for verify in [false, true] {
                let argv = hermit_argv(Path::new("/hermit"), e9, verify, Path::new("/guest"));
                assert!(has_exact_pin(&argv), "missing pin in {argv:?}");
            }
        }
    }

    #[test]
    fn a_mutated_value_is_refused() {
        let mut argv = actual_argv();
        *argv
            .iter_mut()
            .find(|arg| arg.as_str() == "TZ=UTC")
            .unwrap() = "TZ=localtime".into();
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

/// L1/L2 measurement for one arm (golden or e9) of one guest.
struct Arm {
    /// L1 reached: strict run exited 0 with output.
    l1: bool,
    /// L2 reached: --verify printed "Determinism verified" and exited 0.
    l2: bool,
    /// True when L2 was missed because the verify leg wedged (env, not a defect).
    verify_wedged: bool,
    /// True when even the strict (L1) leg wedged on every retry (env, not a
    /// defect). Distinguishes an unmeasurable env wedge from a genuine failure.
    strict_wedged: bool,
    strict_exit: i32,
    strict_stdout: Vec<u8>,
    duration_ms: i64,
    /// AOT reach observed on the strict leg; see `apply_reach_gate`.
    reach: Option<(u64, u64)>,
}

fn measure_arm(
    hermit: &Path,
    e9: bool,
    guest: &Path,
    strict_to: Duration,
    verify_to: Duration,
    verify_retries: u32,
) -> Arm {
    // Strict (L1) leg. Retry a wedged strict the same way as verify: under heavy
    // fleet PMU load the strict leg can wedge too, and an env wedge must not be
    // misread as a guest run failure.
    let mut s = run_timed(&hermit_argv(hermit, e9, false, guest), strict_to);
    let mut total = s.duration_ms;
    let mut strict_wedged = s.wedged;
    if s.wedged {
        for _ in 1..verify_retries.max(1) {
            sleep(Duration::from_secs(2));
            let r = run_timed(&hermit_argv(hermit, e9, false, guest), strict_to);
            total += r.duration_ms;
            let done = !r.wedged;
            s = r;
            if done {
                strict_wedged = false;
                break;
            }
        }
    }
    let l1 = !s.wedged && s.exit == 0;

    // Only attempt L2 if L1 held. Retry any verify leg that did not confirm L2 —
    // under heavy fleet load the verify leg can wedge (timeout-killed) OR
    // skid-panic (exits non-zero without "Determinism verified"); both are PMU
    // env, not guest defects, so give L2 up to `verify_retries` attempts. A
    // genuine non-determinism would never print the needle and honestly settles
    // as L1 after the retries are spent.
    let mut l2 = false;
    let mut verify_wedged = false;
    if l1 {
        for _ in 0..verify_retries.max(1) {
            let v = run_timed(&hermit_argv(hermit, e9, true, guest), verify_to);
            total += v.duration_ms;
            // In --verify mode hermit reports the bitwise-repeat result on stderr
            // ("Determinism verified") and does not re-echo the guest stdout, so
            // L2 is the verified flag + clean exit (matches the corpus driver's
            // l2_ok). Guest-output parity is measured separately from the strict
            // (L1) arms below.
            if v.l2_verified && v.exit == 0 {
                l2 = true;
                verify_wedged = false;
                break;
            }
            // Not confirmed this attempt: record whether it was a wedge (for the
            // honest reason string) and try again after a short PMU cooldown.
            verify_wedged = v.wedged;
            sleep(Duration::from_secs(2));
        }
    }
    Arm {
        l1,
        l2,
        verify_wedged,
        strict_wedged,
        strict_exit: s.exit,
        reach: s.reach,
        strict_stdout: s.stdout,
        duration_ms: total,
    }
}

fn main() {
    let mut lane = "portable".to_string();
    let mut csv: Option<PathBuf> = None;
    let mut hermit_arg: Option<PathBuf> = None;
    let mut corpus_arg: Option<PathBuf> = None;
    let mut run_id_arg: Option<String> = None;
    let mut only: Option<String> = None;
    let mut limit: Option<usize> = None;
    let mut strict_to = 45u64;
    let mut verify_to = 90u64;
    let mut verify_retries = 3u32;
    let mut dry_run = false;
    let mut assert_green = false;

    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--lane" => lane = it.next().unwrap_or_else(|| die("--lane needs a value")),
            "--csv" => {
                csv = Some(PathBuf::from(
                    it.next().unwrap_or_else(|| die("--csv needs a path")),
                ))
            }
            "--hermit" => {
                hermit_arg = Some(PathBuf::from(
                    it.next().unwrap_or_else(|| die("--hermit needs a path")),
                ))
            }
            "--corpus" => {
                corpus_arg = Some(PathBuf::from(
                    it.next().unwrap_or_else(|| die("--corpus needs a path")),
                ))
            }
            "--run-id" => {
                run_id_arg = Some(it.next().unwrap_or_else(|| die("--run-id needs a value")))
            }
            "--only" => only = Some(it.next().unwrap_or_else(|| die("--only needs a substring"))),
            "--limit" => limit = it.next().and_then(|s| s.parse().ok()),
            "--strict-timeout" => {
                strict_to = it.next().and_then(|s| s.parse().ok()).unwrap_or(strict_to)
            }
            "--verify-timeout" => {
                verify_to = it.next().and_then(|s| s.parse().ok()).unwrap_or(verify_to)
            }
            "--verify-retries" => {
                verify_retries = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(verify_retries)
            }
            "--dry-run" => dry_run = true,
            "--assert-green" => assert_green = true,
            "-h" | "--help" => {
                eprintln!(
                    "collect-e9patch-compat.rs [--lane L] [--csv F] [--hermit BIN] [--corpus DIR]"
                );
                eprintln!("    [--run-id ID] [--only SUBSTR] [--limit N] [--strict-timeout S]");
                eprintln!(
                    "    [--verify-timeout S] [--verify-retries N] [--dry-run] [--assert-green]"
                );
                eprintln!("Measures e9patch preprocessing-invariance (golden ptrace vs e9-rewritten ptrace),");
                eprintln!(
                    "honest L1 (strict) vs L2 (strict --verify), into e9patch-scorecard.csv."
                );
                exit(0);
            }
            other => die(&format!("unknown argument {other}")),
        }
    }

    let here = script_dir();
    let csv_path = csv.unwrap_or_else(|| here.join("e9patch-scorecard.csv"));

    // Locate the e9patch-feature hermit worktree + its reverie (for e9tool paths).
    let parent = here
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| here.clone());
    let e9_hermit = parent.join("worktrees/e9patch/hermit");
    let e9_reverie = parent.join("worktrees/e9patch/reverie");
    let hermit_bin = hermit_arg.unwrap_or_else(|| e9_hermit.join("target/debug/hermit"));
    if !hermit_bin.is_file() {
        die(&format!(
            "hermit binary not found: {} (build --features e9patch or pass --hermit)",
            hermit_bin.display()
        ));
    }
    let corpus_dir =
        corpus_arg.unwrap_or_else(|| e9_hermit.join("tests/backend-parity/e9patch_corpus"));
    if !corpus_dir.is_dir() {
        die(&format!(
            "corpus dir not found: {} (pass --corpus)",
            corpus_dir.display()
        ));
    }

    // e9tool / e9patch backend env (required for the e9 arm).
    let e9tool = env::var("HERMIT_E9TOOL").unwrap_or_else(|_| {
        e9_reverie
            .join("third-party/e9patch/e9tool")
            .to_string_lossy()
            .into_owned()
    });
    let e9patch = env::var("HERMIT_E9PATCH_BACKEND").unwrap_or_else(|_| {
        e9_reverie
            .join("third-party/e9patch/e9patch")
            .to_string_lossy()
            .into_owned()
    });
    if !Path::new(&e9tool).is_file() || !Path::new(&e9patch).is_file() {
        die(&format!("e9tool/e9patch not found ({e9tool} / {e9patch}); set HERMIT_E9TOOL + HERMIT_E9PATCH_BACKEND"));
    }
    env::set_var("HERMIT_E9TOOL", &e9tool);
    env::set_var("HERMIT_E9PATCH_BACKEND", &e9patch);

    // Enumerate guests (*.c), sorted for stable ordering.
    let mut guests: Vec<String> = fs::read_dir(&corpus_dir)
        .unwrap_or_else(|e| die(&format!("read corpus dir: {e}")))
        .filter_map(|d| d.ok())
        .filter_map(|d| d.file_name().to_str().map(str::to_string))
        .filter(|n| n.ends_with(".c"))
        .map(|n| n.trim_end_matches(".c").to_string())
        .collect();
    guests.sort();
    if let Some(sub) = &only {
        guests.retain(|g| g.contains(sub.as_str()));
    }
    if let Some(n) = limit {
        guests.truncate(n);
    }
    if guests.is_empty() {
        die("no guests matched");
    }

    let cc = env::var("CC").unwrap_or_else(|_| "cc".into());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let run_id = run_id_arg.unwrap_or_else(|| format!("e9patch-{now}"));
    let run_utc = format!("@{now}");
    let hermit_sha = git(&e9_hermit, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let reverie_sha = git(&e9_reverie, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&e9_hermit, &["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    eprintln!(
        "collect-e9patch-compat: hermit={} guests={} strict_to={}s verify_to={}s retries={}",
        &hermit_sha[..hermit_sha.len().min(12)],
        guests.len(),
        strict_to,
        verify_to,
        verify_retries
    );

    if dry_run {
        eprintln!(
            "(dry-run) would measure {} guests x 2 arms; CSV untouched",
            guests.len()
        );
        for g in &guests {
            eprintln!("  {g}");
        }
        return;
    }

    if !csv_path.exists() {
        fs::write(&csv_path, format!("{HEADER}\n"))
            .unwrap_or_else(|e| die(&format!("create CSV: {e}")));
    } else {
        let existing_header = fs::read_to_string(&csv_path)
            .ok()
            .and_then(|text| text.lines().next().map(str::to_string))
            .unwrap_or_default();
        if existing_header != HEADER {
            die(&format!(
                "scorecard header does not match this producer (have {} columns, need {}); migrate it before appending",
                existing_header.split(',').count(),
                HEADER.split(',').count()
            ));
        }
    }

    let strict_to = Duration::from_secs(strict_to);
    let verify_to = Duration::from_secs(verify_to);
    let tmp = env::temp_dir().join(format!("e9c-guests-{now}"));
    fs::create_dir_all(&tmp).ok();

    let mut rows: Vec<String> = Vec::new();
    let mut regressions: Vec<String> = Vec::new();
    // Tallies for the honest summary.
    let mut tally: BTreeMap<&str, u32> = BTreeMap::new();

    for (i, g) in guests.iter().enumerate() {
        // Compile the guest.
        let elf = tmp.join(g);
        let src = corpus_dir.join(format!("{g}.c"));
        let comp = Command::new(&cc)
            .args(FREESTANDING_FLAGS)
            .arg(&src)
            .arg("-o")
            .arg(&elf)
            .output();
        let compiled = comp.map(|o| o.status.success()).unwrap_or(false);
        if !compiled {
            for backend in ["ptrace", "e9patch"] {
                rows.push(row(
                    &run_id,
                    &run_utc,
                    &hermit_sha,
                    &reverie_sha,
                    dirty,
                    &lane,
                    g,
                    backend,
                    "enabled",
                    "fail",
                    None,
                    None,
                    "",
                    "",
                    0,
                    "compile failed",
                    None,
                ));
            }
            *tally.entry("compile-fail").or_default() += 1;
            eprintln!("[{}/{}] {g}: COMPILE-FAIL", i + 1, guests.len());
            continue;
        }

        // Golden reference arm (un-rewritten ptrace).
        let golden = measure_arm(
            &hermit_bin,
            false,
            &elf,
            strict_to,
            verify_to,
            verify_retries,
        );
        // e9-rewritten arm (ptrace + e9tool AOT rewrite).
        let e9 = measure_arm(
            &hermit_bin,
            true,
            &elf,
            strict_to,
            verify_to,
            verify_retries,
        );

        let gref_hash = sha256(&golden.strict_stdout);
        let e9_hash = sha256(&e9.strict_stdout);
        let parity = golden.l1
            && e9.l1
            && golden.strict_exit == e9.strict_exit
            && golden.strict_stdout == e9.strict_stdout;

        // ---- ptrace (golden reference) cell ----
        let (g_out, g_det, g_reason) = arm_cell(&golden, None);
        rows.push(row(
            &run_id,
            &run_utc,
            &hermit_sha,
            &reverie_sha,
            dirty,
            &lane,
            g,
            "ptrace",
            "enabled",
            g_out,
            g_det,
            None,
            &gref_hash,
            &gref_hash,
            golden.duration_ms,
            &g_reason,
            None,
        ));

        // ---- e9patch (rewritten variant) cell ----
        let (e_out_raw, e_det, e_reason_raw) = arm_cell(&e9, Some(parity));
        // THE GATE. A cell that rewrote nothing cannot be a pass, however
        // perfect its parity looks -- that parity is the reference compared
        // with itself.
        let (e_out, e_reason) = apply_reach_gate("e9patch", e9.reach, e_out_raw, &e_reason_raw);
        // Parity is withheld too: reporting stdout_parity=1 beside
        // outcome=not-exercised would re-manufacture the score the gate removes.
        let e_parity = if matches!(e9.reach, Some((_, m)) if m > 0) {
            Some(parity)
        } else {
            None
        };
        rows.push(row(
            &run_id,
            &run_utc,
            &hermit_sha,
            &reverie_sha,
            dirty,
            &lane,
            g,
            "e9patch",
            "enabled",
            &e_out,
            e_det,
            e_parity,
            &e9_hash,
            &gref_hash,
            e9.duration_ms,
            &e_reason,
            e9.reach,
        ));

        // Tally by the honest per-guest outcome (e9 arm is the interesting one).
        // Order matters: real defects are only reachable once BOTH arms cleared
        // L1, so an env wedge (strict leg killed on every retry) is classified as
        // env, never as a run failure.
        let bucket = if e9.l2 && parity {
            "L2-parity"
        } else if e9.l1 && parity && e9.verify_wedged {
            "L1-parity-verify-wedged"
        } else if e9.l1 && parity {
            "L1-parity"
        } else if golden.l1 && e9.l1 && !parity {
            "parity-diverge"
        } else if golden.strict_wedged || e9.strict_wedged {
            "strict-wedged"
        } else {
            "run-fail"
        };
        *tally.entry(bucket).or_default() += 1;

        // Regression = a real defect (diverge or run failure), NOT an env wedge
        // (a verify-wedge or a strict-wedge is environment, reported not failed).
        if bucket == "parity-diverge" {
            regressions.push(format!("{g} [e9patch] -> parity diverge ({e_reason})"));
        } else if bucket == "run-fail" {
            regressions.push(format!(
                "{g} -> run failure (golden:{g_reason} | e9:{e_reason})"
            ));
        }

        eprintln!(
            "[{}/{}] {g}: golden {} | e9 {} | parity={} => {bucket}",
            i + 1,
            guests.len(),
            level_str(&golden),
            level_str(&e9),
            parity
        );
    }

    // Append rows.
    let existing = fs::read_to_string(&csv_path).unwrap_or_default();
    let mut buf = existing;
    if !buf.ends_with('\n') && !buf.is_empty() {
        buf.push('\n');
    }
    for r in &rows {
        buf.push_str(r);
        buf.push('\n');
    }
    fs::write(&csv_path, buf).unwrap_or_else(|e| die(&format!("write CSV: {e}")));
    let _ = fs::remove_dir_all(&tmp);

    eprintln!(
        "\ncollect-e9patch-compat: wrote {} rows to {} (run_id={run_id})",
        rows.len(),
        csv_path.display()
    );
    eprintln!("Honest per-guest tally (of {} guests):", guests.len());
    for (k, v) in &tally {
        eprintln!("  {k}: {v}");
    }

    if assert_green && !regressions.is_empty() {
        eprintln!(
            "collect-e9patch-compat: REGRESSION — {} real defect(s) (env wedges excluded):",
            regressions.len()
        );
        for r in &regressions {
            eprintln!("  FAIL {r}");
        }
        exit(1);
    }
    if assert_green {
        eprintln!(
            "collect-e9patch-compat: RAW STDOUT PASS — no parity divergences or run failures, but comparison_tier=unqualified-stdout-only; emitted strict greens: 0/{}",
            rows.len()
        );
    }
}

/// Map an arm's L1/L2 state to (outcome, deterministic, reason). `deterministic`
/// is `Some(true)` only at L2, `Some(false)` at L1 (self-determinism unconfirmed),
/// and `None` (blank) when even L1 could not be measured because the strict leg
/// wedged (env) — an env wedge is never a confirmed red. `parity` is Some for the
/// e9 variant arm (affects outcome), None for the golden ref.
fn arm_cell(a: &Arm, parity: Option<bool>) -> (&'static str, Option<bool>, String) {
    if !a.l1 {
        // Strict leg never cleared. A wedge (all retries killed) is env and NOT a
        // confirmed red -> skip + blank determinism; a real non-124 exit is fail.
        if a.strict_wedged {
            return (
                "skip",
                None,
                "level=none; strict leg wedged on every retry (PMU env, not measured)".to_string(),
            );
        }
        return (
            "fail",
            Some(false),
            format!("strict failed: exit={}", a.strict_exit),
        );
    }
    // L1 established. L2?
    if a.l2 {
        match parity {
            Some(false) => (
                "diverge",
                Some(true),
                "level=L2; parity diverges from golden".to_string(),
            ),
            _ => (
                "pass",
                Some(true),
                "level=L2 (Determinism verified)".to_string(),
            ),
        }
    } else {
        // L1 only.
        let base = if a.verify_wedged {
            "level=L1; verify wedged (PMU env, not a defect)"
        } else {
            "level=L1; verify did not confirm bitwise repeat"
        };
        let outcome = match parity {
            Some(false) => "diverge",
            _ => "l1", // ran under strict (L1) but L2 unconfirmed
        };
        let reason = match parity {
            Some(false) => format!("{base}; parity diverges from golden"),
            _ => base.to_string(),
        };
        (outcome, Some(false), reason)
    }
}

fn level_str(a: &Arm) -> &'static str {
    if a.strict_wedged {
        "WEDGE(env)"
    } else if !a.l1 {
        "FAIL"
    } else if a.l2 {
        "L2"
    } else if a.verify_wedged {
        "L1(wedge)"
    } else {
        "L1"
    }
}

#[allow(clippy::too_many_arguments)]
fn row(
    run_id: &str,
    run_utc: &str,
    hermit_sha: &str,
    reverie_sha: &str,
    dirty: bool,
    lane: &str,
    test_id: &str,
    backend: &str,
    cell_state: &str,
    outcome: &str,
    deterministic: Option<bool>,
    parity: Option<bool>,
    output_hash: &str,
    ref_output_hash: &str,
    duration_ms: i64,
    reason: &str,
    reach: Option<(u64, u64)>,
) -> String {
    let det = deterministic
        .map(|b| if b { "1" } else { "0" })
        .unwrap_or("");
    // Historical e9 parity lacks the complete profile/population receipt.  Keep
    // the observation in the explicit legacy field; never publish it as a
    // qualified stdout comparison.
    let legacy_parity = parity
        .map(|b| format!("stdout_parity:{}", if b { "1" } else { "0" }))
        .unwrap_or_default();
    let (verify_compare, det_tier) = if deterministic == Some(true) {
        ("stripped", "stripped-uncounted")
    } else {
        ("", "")
    };
    let reason_q = format!("\"{}\"", reason.replace('"', "'"));
    [
        run_id.to_string(),
        run_utc.to_string(),
        hermit_sha.to_string(),
        reverie_sha.to_string(),
        dirty.to_string(),
        RUN_MODE.to_string(),
        lane.to_string(),
        BUCKET.to_string(),
        test_id.to_string(),
        MODE.to_string(),
        backend.to_string(),
        cell_state.to_string(),
        outcome.to_string(),
        det.to_string(),
        String::new(), // stdout_parity: no complete provenance receipt
        output_hash.to_string(),
        duration_ms.to_string(),
        String::new(), // max_rss_kb
        reason_q,
        verify_compare.to_string(),
        String::new(), // bitwise_parity
        String::new(), // compared_log_messages
        det_tier.to_string(),
        legacy_parity,
        ref_output_hash.to_string(),
        String::new(), // parity_comparator
        String::new(), // parity_tier
        String::new(), // profile_flags
        String::new(), // population_id
        String::new(), // selected_count
        String::new(), // executed_count
        String::new(), // evidence_count
        "unqualified-stdout-only".to_string(),
        reach.map(|(c, _)| c.to_string()).unwrap_or_default(),
        reach.map(|(_, m)| m.to_string()).unwrap_or_default(),
        match reach {
            Some((_, m)) if m > 0 => "e9patch-exercised",
            Some(_) => "not-exercised-no-main-elf-sites",
            None => "unknown-no-banner",
        }
        .to_string(),
        // exit_code_parity / detlog_parity / oracle_verdict, added to the core
        // schema 2026-08-07. This emitter builds a FIXED POSITIONAL row, so the
        // placeholders are mandatory: without them every row is three fields
        // narrower than its header and each trailing value is read from the
        // wrong column. Blank is also the correct VALUE -- no producer computes
        // any of the three, and scorecard-schema.json refuses a non-blank one.
        String::new(),   // exit_code_parity
        String::new(),   // detlog_parity
        String::new(),   // oracle_verdict
        "0".to_string(), // stack_parity: this new run did not request L3
        "0".to_string(), // heap_parity: this new run did not request L3
    ]
    .join(",")
}
