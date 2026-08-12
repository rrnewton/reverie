#!/usr/bin/env rust-script
//! Prepare or execute one-variable submodule bumps with A/B verification.
//!
//! ```cargo
//! [dependencies]
//! chrono = "0.4"
//! clap = { version = "4", features = ["derive"] }
//! fs2 = "0.4"
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! thiserror = "2"
//! ```

use chrono::Utc;
use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand, ValueEnum};
use fs2::FileExt;
use serde::Serialize;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use thiserror::Error;

const SCHEMA_VERSION: u32 = 1;

#[derive(Parser, Debug)]
#[command(
    name = "single-submodule-bump",
    about = "Isolate one submodule bump between known-green A and verified B",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: BumpCommand,
}

#[derive(Subcommand, Debug)]
enum BumpCommand {
    /// Check A and show the exact fetched A→target transition without mutation.
    Plan(CommonArgs),
    /// Create B, run verification, and append the result to ci-hub history.
    Run(RunArgs),
}

#[derive(Args, Clone, Debug)]
struct CommonArgs {
    /// The only gitlink allowed to change.
    #[arg(long, value_enum)]
    submodule: ManagedSubmodule,
    /// Full known-green parent commit; current parent HEAD must equal it.
    #[arg(long)]
    base: String,
    /// Durable evidence file containing the full base SHA and its green result.
    #[arg(long)]
    base_evidence: PathBuf,
    /// Verification class; determinism requires matched-load calibration evidence.
    #[arg(long, value_enum, default_value = "standard")]
    verification_kind: VerificationKind,
    /// Calibration/evidence for a powered matched-load determinism probe.
    #[arg(long)]
    matched_load_calibration: Option<PathBuf>,
    /// Label for A in matched.sh output.
    #[arg(long, default_value = "a")]
    matched_load_baseline_label: String,
    /// Label for B in matched.sh output.
    #[arg(long, default_value = "b")]
    matched_load_subject_label: String,
    /// Known-bad validity-calibrator label in matched.sh output.
    #[arg(long, default_value = "z_HEAD")]
    matched_load_calibrator_label: String,
}

#[derive(Args, Clone, Debug)]
struct RunArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Override the append-only result store.
    #[arg(long)]
    history_store: Option<PathBuf>,
    /// Override the isolated bump commit subject.
    #[arg(long)]
    commit_message: Option<String>,
    /// Verification argv. Use `bash -lc` explicitly when shell syntax is needed.
    #[arg(last = true, required = true)]
    verification_command: Vec<OsString>,
}

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum ManagedSubmodule {
    AgentUtils,
    Hermit,
    Reverie,
    Liteinst2,
}

impl ManagedSubmodule {
    fn path(self) -> &'static str {
        match self {
            Self::AgentUtils => "agent-utils",
            Self::Hermit => "hermit",
            Self::Reverie => "reverie",
            Self::Liteinst2 => "liteinst2",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum VerificationKind {
    Standard,
    Determinism,
}

#[derive(Debug)]
struct Transition {
    parent_a: String,
    submodule_a: String,
    submodule_b: String,
    checkout_branch: String,
}

#[derive(Serialize)]
struct BumpRecord {
    schema_version: u32,
    record_type: &'static str,
    recorded_at: String,
    run_id: String,
    submodule: ManagedSubmodule,
    parent_a: String,
    parent_b: String,
    submodule_a: String,
    submodule_b: String,
    submodule_remote_main: String,
    base_evidence: String,
    verification_kind: VerificationKind,
    verification_command: Vec<String>,
    verification_command_exit: i32,
    verification_exit: i32,
    verdict: String,
    log_path: String,
    matched_load_calibration: Option<String>,
    matched_load_summary: Option<MatchedLoadSummary>,
    cost_record_path: String,
}

#[derive(Clone, Debug, Serialize)]
struct MatchedLoadSummary {
    total_waves: u32,
    valid_waves: u32,
    baseline_red_waves: u32,
    subject_red_waves: u32,
    baseline_label: String,
    subject_label: String,
    calibrator_label: String,
}

#[derive(Debug, Error)]
enum BumpError {
    #[error("{0}")]
    Precondition(String),
    #[error("cannot launch {tool}: {source}")]
    Launch {
        tool: String,
        #[source]
        source: io::Error,
    },
    #[error("{command} failed: {detail}")]
    Command { command: String, detail: String },
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot serialize bump result: {0}")]
    Json(#[from] serde_json::Error),
}

impl BumpCommand {
    fn common(&self) -> &CommonArgs {
        match self {
            Self::Plan(args) => args,
            Self::Run(args) => &args.common,
        }
    }

    fn is_run(&self) -> bool {
        matches!(self, Self::Run(_))
    }
}

fn main() -> ExitCode {
    let raw: Vec<OsString> = env::args_os().collect();
    let cli = match Cli::try_parse_from(raw.clone()) {
        Ok(cli) => cli,
        Err(error) => {
            let code = match error.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
                _ => 2,
            };
            let _ = error.print();
            return exit_code(code);
        }
    };
    let root = match workspace_root() {
        Ok(root) => root,
        Err(error) => return report_error(error),
    };

    if env::var_os("CI_HUB_TOOL_COST_ACTIVE").is_none() {
        match run_costed(&root, &raw[1..], &cli.command) {
            Ok(code) => return exit_code(code),
            Err(error) => return report_error(error),
        }
    }

    match execute(&root, cli.command) {
        Ok(code) => exit_code(code),
        Err(error) => report_error(error),
    }
}

fn workspace_root() -> Result<PathBuf, BumpError> {
    let source = env::var_os("RUST_SCRIPT_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::current_exe().ok())
        .ok_or_else(|| BumpError::Precondition("cannot locate script source".into()))?;
    let start = source
        .parent()
        .ok_or_else(|| BumpError::Precondition("script has no parent directory".into()))?;
    git(start, ["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

fn run_costed(
    root: &Path,
    original_args: &[OsString],
    command: &BumpCommand,
) -> Result<i32, BumpError> {
    let executable = env::current_exe().map_err(|source| BumpError::Launch {
        tool: "current single-submodule-bump executable".into(),
        source,
    })?;
    let run_id = env::var("SUBMODULE_BUMP_RUN_ID").unwrap_or_else(|_| {
        format!(
            "{}-{}-{}",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            command.common().submodule.path(),
            std::process::id()
        )
    });
    let cost_path = root
        .join("ignored/ci-hub/submodule-bumps")
        .join(&run_id)
        .join("operation-cost.json");
    let basis = match command {
        BumpCommand::Plan(_) => {
            "not measured: one submodule fetch plus local ancestry/precondition checks"
        }
        BumpCommand::Run(_) => {
            "not measured: submodule fetch, isolated commit, and caller-supplied verification have no retained combined runtime history"
        }
    };
    let mut process = Command::new(root.join("ci-hub/bin/tool-cost"));
    process
        .env("CI_HUB_TOOL_COST_ACTIVE", "1")
        .env("SUBMODULE_BUMP_RUN_ID", &run_id)
        .arg("--tool")
        .arg(format!(
            "single-submodule-bump/{}/{}",
            if command.is_run() { "run" } else { "plan" },
            command.common().submodule.path()
        ))
        .arg("--estimate-unknown")
        .arg("--basis")
        .arg(basis)
        .arg("--actual-json")
        .arg(cost_path)
        .arg("--")
        .arg(executable)
        .args(original_args);
    let status = process.status().map_err(|source| BumpError::Launch {
        tool: "ci-hub/bin/tool-cost".into(),
        source,
    })?;
    Ok(status_code(status))
}

fn execute(root: &Path, command: BumpCommand) -> Result<i32, BumpError> {
    let common = command.common().clone();
    let transition = inspect_transition(root, &common)?;
    print_transition(&transition, &common);
    match command {
        BumpCommand::Plan(_) => {
            println!("state=ready-to-bump");
            Ok(0)
        }
        BumpCommand::Run(args) => run_bump(root, args, transition),
    }
}

fn inspect_transition(root: &Path, args: &CommonArgs) -> Result<Transition, BumpError> {
    require_sha("--base", &args.base)?;
    let head = git(root, ["rev-parse", "HEAD"])?;
    if head != args.base {
        return Err(BumpError::Precondition(format!(
            "current parent HEAD {head} is not declared known-green A {}",
            args.base
        )));
    }
    require_bound_evidence(root, &args.base_evidence, &args.base, "base evidence")?;
    if matches!(args.verification_kind, VerificationKind::Determinism) {
        let calibration = args.matched_load_calibration.as_ref().ok_or_else(|| {
            BumpError::Precondition(
                "determinism verification requires --matched-load-calibration; one green run is insufficient"
                    .into(),
            )
        })?;
        require_existing(root, calibration, "matched-load calibration")?;
    }

    let parent_status = git(root, ["status", "--porcelain=v1", "--untracked-files=no"])?;
    if !parent_status.is_empty() {
        return Err(BumpError::Precondition(format!(
            "parent A is not clean; tracked changes exist:\n{parent_status}"
        )));
    }
    let staged = git(root, ["diff", "--cached", "--name-only"])?;
    if !staged.is_empty() {
        return Err(BumpError::Precondition(format!(
            "parent index is not empty before bump:\n{staged}"
        )));
    }

    let submodule_path = args.submodule.path();
    let checkout = root.join(submodule_path);
    if !checkout.join(".git").exists() {
        return Err(BumpError::Precondition(format!(
            "{} is not initialized",
            checkout.display()
        )));
    }
    let submodule_status = git(
        &checkout,
        ["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !submodule_status.is_empty() {
        return Err(BumpError::Precondition(format!(
            "{submodule_path} checkout is dirty:\n{submodule_status}"
        )));
    }

    fetch_origin_main(root, &checkout)?;
    let parent_pin = git(root, ["rev-parse", &format!("HEAD:{submodule_path}")])?;
    let checkout_head = git(&checkout, ["rev-parse", "HEAD"])?;
    if parent_pin != checkout_head {
        return Err(BumpError::Precondition(format!(
            "A gitlink {parent_pin} does not match {submodule_path} checkout {checkout_head}"
        )));
    }
    let target = git(&checkout, ["rev-parse", "origin/main"])?;
    if target == parent_pin {
        return Err(BumpError::Precondition(format!(
            "{submodule_path} has nothing to bump: A pin and origin/main are both {target}"
        )));
    }
    let ancestor = run_status(Command::new("git").arg("-C").arg(&checkout).args([
        "merge-base",
        "--is-ancestor",
        &parent_pin,
        &target,
    ]))?;
    if ancestor != 0 {
        return Err(BumpError::Precondition(format!(
            "{submodule_path} origin/main {target} is not a fast-forward from A pin {parent_pin}"
        )));
    }
    let checkout_branch = git(&checkout, ["branch", "--show-current"])?;
    if !checkout_branch.is_empty() && checkout_branch != "main" {
        return Err(BumpError::Precondition(format!(
            "{submodule_path} checkout is on feature branch {checkout_branch:?}; expected main or detached"
        )));
    }
    Ok(Transition {
        parent_a: head,
        submodule_a: parent_pin,
        submodule_b: target,
        checkout_branch,
    })
}

fn run_bump(root: &Path, args: RunArgs, transition: Transition) -> Result<i32, BumpError> {
    let parent_branch = git(root, ["branch", "--show-current"])?;
    if parent_branch.is_empty() || parent_branch == "main" {
        return Err(BumpError::Precondition(
            "run requires a dedicated non-main parent bump branch/worktree; plan is safe on main"
                .into(),
        ));
    }
    if matches!(args.common.verification_kind, VerificationKind::Determinism) {
        require_matched_load_command(&args.verification_command)?;
    }
    require_parent_head(root, &transition.parent_a)?;
    let run_id = env::var("SUBMODULE_BUMP_RUN_ID").unwrap_or_else(|_| {
        format!(
            "{}-{}-{}",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            args.common.submodule.path(),
            std::process::id()
        )
    });
    let run_dir = root.join("ignored/ci-hub/submodule-bumps").join(&run_id);
    fs::create_dir_all(&run_dir).map_err(|source| BumpError::Io {
        path: run_dir.clone(),
        source,
    })?;
    let checkout = root.join(args.common.submodule.path());
    if transition.checkout_branch == "main" {
        run_checked(Command::new("git").arg("-C").arg(&checkout).args([
            "merge",
            "--ff-only",
            &transition.submodule_b,
        ]))?;
    } else {
        run_checked(Command::new("git").arg("-C").arg(&checkout).args([
            "switch",
            "--detach",
            &transition.submodule_b,
        ]))?;
    }
    require_parent_head(root, &transition.parent_a)?;

    run_checked(Command::new("git").arg("-C").arg(root).args([
        "add",
        "--",
        args.common.submodule.path(),
    ]))?;
    require_only_path(
        &git(root, ["diff", "--cached", "--name-only"])?,
        args.common.submodule.path(),
        "staged bump",
    )?;
    let message = args.commit_message.unwrap_or_else(|| {
        format!(
            "Bump {} {} to {}",
            args.common.submodule.path(),
            short(&transition.submodule_a),
            short(&transition.submodule_b)
        )
    });
    run_checked(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["commit", "--only", "-m"])
            .arg(&message)
            .args(["--", args.common.submodule.path()]),
    )?;
    let parent_b = git(root, ["rev-parse", "HEAD"])?;
    let changed = git(
        root,
        [
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            &parent_b,
        ],
    )?;
    require_only_path(&changed, args.common.submodule.path(), "commit B")?;

    let log_path = run_dir.join("verification.log");
    let mut log = File::create(&log_path).map_err(|source| BumpError::Io {
        path: log_path.clone(),
        source,
    })?;
    writeln!(
        log,
        "run_id={run_id}\nparent_a={}\nparent_b={parent_b}\nverification_command={:?}",
        transition.parent_a, args.verification_command
    )
    .map_err(|source| BumpError::Io {
        path: log_path.clone(),
        source,
    })?;
    log.flush().map_err(|source| BumpError::Io {
        path: log_path.clone(),
        source,
    })?;
    let stderr = log.try_clone().map_err(|source| BumpError::Io {
        path: log_path.clone(),
        source,
    })?;
    let mut verification = Command::new(&args.verification_command[0]);
    verification
        .current_dir(root)
        .args(&args.verification_command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    let (verification_command_exit, spawn_failed) = match verification.status() {
        Ok(status) => (status_code(status), false),
        Err(source) => {
            let mut diagnostic =
                OpenOptions::new()
                    .append(true)
                    .open(&log_path)
                    .map_err(|open_error| BumpError::Io {
                        path: log_path.clone(),
                        source: open_error,
                    })?;
            writeln!(diagnostic, "verification launch failed: {source}").map_err(
                |write_error| BumpError::Io {
                    path: log_path.clone(),
                    source: write_error,
                },
            )?;
            (127, true)
        }
    };
    let mut matched_load_summary = None;
    let (verification_exit, verdict) = if spawn_failed {
        (127, "infra-error".to_string())
    } else if verification_command_exit != 0 {
        (verification_command_exit, "fail".to_string())
    } else if matches!(args.common.verification_kind, VerificationKind::Determinism) {
        let output = fs::read_to_string(&log_path).map_err(|source| BumpError::Io {
            path: log_path.clone(),
            source,
        })?;
        let (code, state, summary) = evaluate_matched_load(
            &output,
            &args.common.matched_load_baseline_label,
            &args.common.matched_load_subject_label,
            &args.common.matched_load_calibrator_label,
        );
        matched_load_summary = Some(summary);
        (code, state.to_string())
    } else {
        (0, "pass".to_string())
    };
    let cost_path = run_dir.join("operation-cost.json");
    let record = BumpRecord {
        schema_version: SCHEMA_VERSION,
        record_type: "single-submodule-bump",
        recorded_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        run_id: run_id.clone(),
        submodule: args.common.submodule,
        parent_a: transition.parent_a,
        parent_b: parent_b.clone(),
        submodule_a: transition.submodule_a,
        submodule_b: transition.submodule_b.clone(),
        submodule_remote_main: transition.submodule_b,
        base_evidence: absolute(root, &args.common.base_evidence)
            .display()
            .to_string(),
        verification_kind: args.common.verification_kind,
        verification_command: args
            .verification_command
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect(),
        verification_command_exit,
        verification_exit,
        verdict: verdict.clone(),
        log_path: log_path.display().to_string(),
        matched_load_calibration: args
            .common
            .matched_load_calibration
            .as_ref()
            .map(|path| absolute(root, path).display().to_string()),
        matched_load_summary,
        cost_record_path: cost_path.display().to_string(),
    };
    let store = args
        .history_store
        .map(|path| absolute(root, &path))
        .unwrap_or_else(|| root.join("ignored/ci-hub/submodule-bumps.jsonl"));
    append_record(&store, &record)?;
    println!("state={verdict}");
    println!("run_id={run_id}");
    println!("parent_a={}", record.parent_a);
    println!("parent_b={parent_b}");
    println!("log_path={}", log_path.display());
    println!("history_store={}", store.display());
    Ok(verification_exit)
}

fn fetch_origin_main(root: &Path, checkout: &Path) -> Result<(), BumpError> {
    run_checked(
        Command::new("python3")
            .arg(root.join("ci-hub/lib/fetch_ref.py"))
            .args(["--repo"])
            .arg(checkout)
            .args(["--remote", "origin", "--branch", "main"]),
    )
}

fn print_transition(transition: &Transition, args: &CommonArgs) {
    println!("single-variable submodule bump");
    println!("  submodule={}", args.submodule.path());
    println!("  parent_a={}", transition.parent_a);
    println!("  submodule_a={}", transition.submodule_a);
    println!("  submodule_b={}", transition.submodule_b);
    println!("  verification_kind={:?}", args.verification_kind);
    println!("  changed_paths_if_run={}", args.submodule.path());
}

fn require_bound_evidence(
    root: &Path,
    path: &Path,
    sha: &str,
    name: &str,
) -> Result<(), BumpError> {
    let path = absolute(root, path);
    let content = fs::read_to_string(&path).map_err(|source| BumpError::Io {
        path: path.clone(),
        source,
    })?;
    if !content.contains(sha) {
        return Err(BumpError::Precondition(format!(
            "{name} {} does not contain full A SHA {sha}",
            path.display()
        )));
    }
    Ok(())
}

fn require_existing(root: &Path, path: &Path, name: &str) -> Result<(), BumpError> {
    let path = absolute(root, path);
    if !path.is_file() {
        return Err(BumpError::Precondition(format!(
            "{name} is not a file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_only_path(output: &str, expected: &str, context: &str) -> Result<(), BumpError> {
    let paths: Vec<_> = output.lines().filter(|line| !line.is_empty()).collect();
    if paths != [expected] {
        return Err(BumpError::Precondition(format!(
            "{context} must change exactly {expected:?}, got {paths:?}"
        )));
    }
    Ok(())
}

fn require_sha(name: &str, value: &str) -> Result<(), BumpError> {
    if value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(BumpError::Precondition(format!(
            "{name} must be a full 40-character hexadecimal SHA"
        )))
    }
}

fn require_parent_head(root: &Path, expected: &str) -> Result<(), BumpError> {
    let actual = git(root, ["rev-parse", "HEAD"])?;
    if actual == expected {
        Ok(())
    } else {
        Err(BumpError::Precondition(format!(
            "parent HEAD moved during bump preparation: expected A {expected}, found {actual}"
        )))
    }
}

fn require_matched_load_command(command: &[OsString]) -> Result<(), BumpError> {
    let rendered = command
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    if rendered.contains("experiments/multisect_detcore_misc_20260803/matched.sh")
        || rendered.contains("multisect_detcore_misc_20260803/matched.sh")
    {
        Ok(())
    } else {
        Err(BumpError::Precondition(
            "determinism verification command must invoke the calibrated experiments/multisect_detcore_misc_20260803/matched.sh probe"
                .into(),
        ))
    }
}

fn evaluate_matched_load(
    output: &str,
    baseline_label: &str,
    subject_label: &str,
    calibrator_label: &str,
) -> (i32, &'static str, MatchedLoadSummary) {
    let mut summary = MatchedLoadSummary {
        total_waves: 0,
        valid_waves: 0,
        baseline_red_waves: 0,
        subject_red_waves: 0,
        baseline_label: baseline_label.to_string(),
        subject_label: subject_label.to_string(),
        calibrator_label: calibrator_label.to_string(),
    };
    for line in output.lines().filter(|line| line.starts_with("wave")) {
        summary.total_waves += 1;
        let mut classes = std::collections::BTreeMap::new();
        for token in line.split_whitespace() {
            let Some((label, classification)) = token.split_once(':') else {
                continue;
            };
            let classification = classification.split('(').next().unwrap_or(classification);
            if matches!(classification, "PASS" | "FAIL" | "FLAKY" | "NORUN") {
                classes.insert(label, classification);
            }
        }
        let calibrator = classes.get(calibrator_label).copied();
        if !matches!(calibrator, Some("FAIL" | "FLAKY")) {
            continue;
        }
        summary.valid_waves += 1;
        if classes.get(baseline_label).copied() != Some("PASS") {
            summary.baseline_red_waves += 1;
        }
        if classes.get(subject_label).copied() != Some("PASS") {
            summary.subject_red_waves += 1;
        }
    }
    let result = if summary.valid_waves == 0 {
        (4, "underpowered")
    } else if summary.baseline_red_waves > 0 {
        (4, "invalid-baseline")
    } else if summary.subject_red_waves > 0 {
        (1, "fail")
    } else {
        (0, "pass")
    };
    (result.0, result.1, summary)
}

fn append_record(path: &Path, record: &BumpRecord) -> Result<(), BumpError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| BumpError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .map_err(|source| BumpError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.lock_exclusive().map_err(|source| BumpError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut json = serde_json::to_vec(record)?;
    json.push(b'\n');
    file.write_all(&json).map_err(|source| BumpError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| BumpError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    FileExt::unlock(&file).map_err(|source| BumpError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn absolute(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn git<I, S>(repo: &Path, args: I) -> Result<String, BumpError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(args);
    let output = command.output().map_err(|source| BumpError::Launch {
        tool: format!("{command:?}"),
        source,
    })?;
    if !output.status.success() {
        return Err(BumpError::Command {
            command: format!("{command:?}"),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

fn run_checked(command: &mut Command) -> Result<(), BumpError> {
    let display = format!("{command:?}");
    let output = command.output().map_err(|source| BumpError::Launch {
        tool: display.clone(),
        source,
    })?;
    if output.status.success() {
        return Ok(());
    }
    Err(BumpError::Command {
        command: display,
        detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

fn run_status(command: &mut Command) -> Result<i32, BumpError> {
    let display = format!("{command:?}");
    let status = command.status().map_err(|source| BumpError::Launch {
        tool: display,
        source,
    })?;
    Ok(status_code(status))
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

fn status_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

fn report_error(error: BumpError) -> ExitCode {
    eprintln!("single-submodule-bump: {error}");
    ExitCode::from(2)
}

fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_one_expected_path() {
        assert!(require_only_path("agent-utils\n", "agent-utils", "test").is_ok());
        assert!(require_only_path("agent-utils\nhermit\n", "agent-utils", "test").is_err());
        assert!(require_only_path("", "agent-utils", "test").is_err());
    }

    #[test]
    fn requires_full_sha() {
        assert!(require_sha("base", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_ok());
        assert!(require_sha("base", "aaaaaaaa").is_err());
        assert!(require_sha("base", "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_err());
    }

    #[test]
    fn determinism_mode_is_typed() {
        let cli = Cli::try_parse_from([
            "single-submodule-bump",
            "plan",
            "--submodule",
            "reverie",
            "--base",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--base-evidence",
            "baseline.json",
            "--verification-kind",
            "determinism",
            "--matched-load-calibration",
            "experiments/multisect_detcore_misc_20260803/metadata.json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command.common().verification_kind,
            VerificationKind::Determinism
        ));
    }

    #[test]
    fn matched_load_requires_powered_clean_a_and_b() {
        let clean = "wave1 load=400 | a:PASS(0/32) b:PASS(0/32) z_HEAD:FLAKY(4/32)\n";
        let (code, verdict, summary) = evaluate_matched_load(clean, "a", "b", "z_HEAD");
        assert_eq!((code, verdict), (0, "pass"));
        assert_eq!(summary.valid_waves, 1);

        let flaky_b = "wave1 load=400 | a:PASS(0/32) b:FLAKY(4/32) z_HEAD:FLAKY(4/32)\n";
        assert_eq!(evaluate_matched_load(flaky_b, "a", "b", "z_HEAD").0, 1);

        let underpowered = "wave1 load=100 | a:PASS(0/32) b:PASS(0/32) z_HEAD:PASS(0/32)\n";
        assert_eq!(
            evaluate_matched_load(underpowered, "a", "b", "z_HEAD").1,
            "underpowered"
        );

        let bad_a = "wave1 load=400 | a:FLAKY(2/32) b:PASS(0/32) z_HEAD:FLAKY(4/32)\n";
        assert_eq!(
            evaluate_matched_load(bad_a, "a", "b", "z_HEAD").1,
            "invalid-baseline"
        );
    }

    #[test]
    fn determinism_command_must_name_calibrated_probe() {
        assert!(require_matched_load_command(&[OsString::from(
            "experiments/multisect_detcore_misc_20260803/matched.sh"
        )])
        .is_ok());
        assert!(require_matched_load_command(&[OsString::from("/bin/true")]).is_err());
    }

    #[test]
    fn appends_typed_history_record() {
        let path = env::temp_dir().join(format!(
            "submodule-bump-history-test-{}.jsonl",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let record = BumpRecord {
            schema_version: 1,
            record_type: "single-submodule-bump",
            recorded_at: "2026-08-03T00:00:00Z".into(),
            run_id: "test-run".into(),
            submodule: ManagedSubmodule::AgentUtils,
            parent_a: "a".repeat(40),
            parent_b: "b".repeat(40),
            submodule_a: "c".repeat(40),
            submodule_b: "d".repeat(40),
            submodule_remote_main: "d".repeat(40),
            base_evidence: "/tmp/base.json".into(),
            verification_kind: VerificationKind::Standard,
            verification_command: vec!["/bin/true".into()],
            verification_command_exit: 0,
            verification_exit: 0,
            verdict: "pass".into(),
            log_path: "/tmp/verify.log".into(),
            matched_load_calibration: None,
            matched_load_summary: None,
            cost_record_path: "/tmp/cost.json".into(),
        };
        append_record(&path, &record).unwrap();
        let line = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["parent_a"], "a".repeat(40));
        assert_eq!(value["verdict"], "pass");
        let _ = fs::remove_file(path);
    }
}
