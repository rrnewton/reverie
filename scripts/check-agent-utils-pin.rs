#!/usr/bin/env rust-script
//! Verify that agent-utils is cleanly pegged to its fetched origin/main commit.
//!
//! The check follows the parent-recorded Hermit commit to its agent-utils
//! gitlink, then compares that pin with the initialized checkout and
//! `origin/main`. The parent compatibility path must be the tracked
//! `agent-utils -> hermit/agent-utils` symlink.
//! By default it fetches and prunes origin through `with-proxy` before deciding.
//!
//! Unpushed commits are SPLIT rather than counted together, because one number cannot
//! carry two conditions that want opposite responses:
//!
//! * IN FLIGHT — on a branch some worktree has checked out. Someone is working on it, and
//!   during an egress outage it cannot be pushed at all. Reported, never a failure.
//! * STRANDED — on a branch no worktree holds. Nothing is keeping this work; one
//!   `git branch -D` and it is unrecoverable. This is the condition worth failing on.
//! * UNATTRIBUTED — reachable from no local branch (detached HEAD only), so no branch name
//!   can be offered as the place to fix it.
//!
//! Failing on the union made the checker permanently red for the benign case, which trains
//! readers to ignore it — and then a real stranded commit looks exactly like the noise.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

const AGENT_UTILS_LINK: &str = "agent-utils";
const AGENT_UTILS_LINK_TARGET: &str = "hermit/agent-utils";
const HERMIT_PATH: &str = "hermit";

#[derive(Debug)]
struct Options {
    root: Option<PathBuf>,
    fetch: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckoutMode {
    CanonicalShared,
    IsolatedClone,
}

impl fmt::Display for CheckoutMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CanonicalShared => formatter.write_str("canonical-shared"),
            Self::IsolatedClone => formatter.write_str("isolated-clone"),
        }
    }
}

#[derive(Debug)]
enum CheckError {
    Usage(String),
    Io { tool: String, source: io::Error },
    Command { command: String, detail: String },
    InvalidOutput(String),
}

impl fmt::Display for CheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) | Self::InvalidOutput(message) => formatter.write_str(message),
            Self::Io { tool, source } => write!(formatter, "could not launch {tool}: {source}"),
            Self::Command { command, detail } => write!(formatter, "{command} failed: {detail}"),
        }
    }
}

fn main() -> ExitCode {
    let raw: Vec<OsString> = env::args_os().collect();
    let options = match parse_args(&raw[1..]) {
        Ok(Some(options)) => options,
        Ok(None) => {
            print_help();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("check-agent-utils-pin: {error}");
            eprintln!("Try --help for usage.");
            return ExitCode::from(2);
        }
    };
    let root = match options.root.clone().map(Ok).unwrap_or_else(workspace_root) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("check-agent-utils-pin: {error}");
            return ExitCode::from(2);
        }
    };

    if options.fetch && env::var_os("CI_HUB_TOOL_COST_ACTIVE").is_none() {
        match run_costed(&root, &raw[1..]) {
            Ok(status) => return exit_code(status),
            Err(error) => {
                eprintln!("check-agent-utils-pin: {error}");
                return ExitCode::from(2);
            }
        }
    }

    match check(&root, options.fetch) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("check-agent-utils-pin: {error}");
            ExitCode::from(2)
        }
    }
}

fn parse_args(args: &[OsString]) -> Result<Option<Options>, CheckError> {
    let mut root = None;
    let mut fetch = true;
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("-h" | "--help") => return Ok(None),
            Some("--no-fetch") => {
                fetch = false;
                index += 1;
            }
            Some("--root") => {
                let value = args.get(index + 1).ok_or_else(|| {
                    CheckError::Usage("--root requires a workspace path".to_string())
                })?;
                root = Some(PathBuf::from(value));
                index += 2;
            }
            Some(argument) => {
                return Err(CheckError::Usage(format!("unknown argument {argument:?}")));
            }
            None => {
                return Err(CheckError::Usage(
                    "arguments must be valid UTF-8".to_string(),
                ));
            }
        }
    }
    Ok(Some(Options { root, fetch }))
}

fn print_help() {
    println!(
        "Usage: scripts/check-agent-utils-pin.rs [--no-fetch] [--root PATH]\n\
         \n\
         Fetch agent-utils origin, then require:\n\
           * agent-utils is the exact symlink hermit/agent-utils\n\
           * parent-recorded Hermit pin's agent-utils gitlink == checkout HEAD == origin/main\n\
           * canonical ~/work/dev-hermit checkout branch is main\n\
           * any other (isolated-clone) checkout is clean; detached HEAD is allowed\n\
           * no local commit is STRANDED: unreachable from every origin/* ref\n\
         \n\
         An unpushed commit on a branch some worktree has checked out is IN FLIGHT:\n\
         reported, not failed. Only work no worktree is holding is stranded.\n\
         \n\
         --no-fetch  compare existing refs only (offline/debug use)\n\
         --root PATH override the dev-hermit parent path"
    );
}

fn workspace_root() -> Result<PathBuf, CheckError> {
    let mut command = Command::new("git");
    if let Some(script) = env::var_os("RUST_SCRIPT_PATH").filter(|path| !path.is_empty()) {
        let script = PathBuf::from(script);
        if let Some(parent) = script.parent() {
            command.arg("-C").arg(parent);
        }
    }
    let output = run(command.args(["rev-parse", "--show-toplevel"]))?;
    Ok(PathBuf::from(single_line("git workspace root", &output)?))
}

fn checkout_mode(root: &Path) -> Result<CheckoutMode, CheckError> {
    let home = env::var_os("HOME").ok_or_else(|| {
        CheckError::InvalidOutput(
            "HOME is unset; cannot distinguish the canonical shared checkout from an isolated clone"
                .to_string(),
        )
    })?;
    let canonical = PathBuf::from(home).join("work/dev-hermit");
    let resolved_root = fs::canonicalize(root).map_err(|source| CheckError::Io {
        tool: format!("canonicalize workspace root {}", root.display()),
        source,
    })?;
    let resolved_canonical = match fs::canonicalize(&canonical) {
        Ok(path) => path,
        Err(source) if source.kind() == io::ErrorKind::NotFound => canonical,
        Err(source) => {
            return Err(CheckError::Io {
                tool: format!("canonicalize canonical workspace {}", canonical.display()),
                source,
            });
        }
    };
    Ok(classify_checkout_mode(&resolved_root, &resolved_canonical))
}

fn classify_checkout_mode(root: &Path, canonical: &Path) -> CheckoutMode {
    if root == canonical {
        CheckoutMode::CanonicalShared
    } else {
        CheckoutMode::IsolatedClone
    }
}

fn run_costed(root: &Path, original_args: &[OsString]) -> Result<ExitStatus, CheckError> {
    let executable = env::current_exe().map_err(|source| CheckError::Io {
        tool: "current executable".into(),
        source,
    })?;
    let mut command = Command::new(root.join("ci-hub/bin/tool-cost"));
    command
        .env("CI_HUB_TOOL_COST_ACTIVE", "1")
        .arg("--tool")
        .arg("check-agent-utils-pin")
        .arg("--estimate-unknown")
        .arg("--basis")
        .arg("not measured: one agent-utils fetch/prune plus local ancestry checks")
        .arg("--")
        .arg(executable)
        .arg("--root")
        .arg(root)
        .args(original_args);
    command.status().map_err(|source| CheckError::Io {
        tool: "ci-hub/bin/tool-cost".into(),
        source,
    })
}

fn check(root: &Path, fetch: bool) -> Result<bool, CheckError> {
    let checkout = validate_compat_link(root)?;
    let hermit = root.join(HERMIT_PATH);
    if !hermit.join(".git").exists() {
        return Err(CheckError::InvalidOutput(format!(
            "{} is not initialized; run `git -C {} submodule update --init --checkout -- hermit`",
            hermit.display(),
            root.display()
        )));
    }
    let (parent_hermit, expected_pin) = expected_agent_utils_pin(root, &hermit)?;
    if !checkout.join(".git").exists() {
        return Err(CheckError::InvalidOutput(format!(
            "{} is not initialized through {}; run `git -C {} submodule update --init --checkout -- agent-utils`",
            checkout.display(),
            AGENT_UTILS_LINK_TARGET,
            hermit.display()
        )));
    }

    if fetch {
        let mut fetch_command = Command::new("with-proxy");
        fetch_command
            .args(["git", "-C"])
            .arg(&checkout)
            .args(["fetch", "--prune", "origin"]);
        run(&mut fetch_command)?;
    }

    let mode = checkout_mode(root)?;
    let checkout_head = git(&checkout, ["rev-parse", "HEAD"])?;
    let origin_main = git(&checkout, ["rev-parse", "origin/main"])?;
    let branch = git(&checkout, ["branch", "--show-current"])?;
    let (checkout_ahead, checkout_behind) = divergence(&checkout, "HEAD", "origin/main")?;
    let (pin_ahead, pin_behind) = divergence(&checkout, &expected_pin, "origin/main")?;
    let unpushed = git_lines(
        &checkout,
        [
            "rev-list",
            "HEAD",
            "--branches",
            "--not",
            "--remotes=origin",
        ],
    )?;
    // Attribute every unpushed commit to the branches carrying it, and note which of those
    // branches a worktree is holding. A bare count cannot separate "someone is working on this
    // right now" from "this is abandoned and one delete away from gone", and those want
    // opposite responses.
    let local_branches = git_lines(
        &checkout,
        ["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?;
    let mut branch_commits = Vec::with_capacity(local_branches.len());
    for name in &local_branches {
        let commits = git_lines(
            &checkout,
            ["rev-list", name.as_str(), "--not", "--remotes=origin"],
        )?;
        branch_commits.push((name.clone(), commits));
    }
    let worktree_branches =
        parse_worktree_branches(&git(&checkout, ["worktree", "list", "--porcelain"])?);
    let split = classify_unpushed(&unpushed, &branch_commits, &worktree_branches);

    let dirty = !git(
        &checkout,
        ["status", "--porcelain=v1", "--untracked-files=all"],
    )?
    .is_empty();

    println!("agent-utils drift check");
    println!("  checkout_mode={mode}");
    println!("  parent_hermit_gitlink={parent_hermit}");
    println!("  hermit_agent_utils_gitlink={expected_pin}");
    println!("  checkout_head={checkout_head}");
    println!("  origin_main={origin_main}");
    println!("  checkout_branch={branch}");
    println!("  checkout_ahead={checkout_ahead}");
    println!("  checkout_behind={checkout_behind}");
    println!("  pin_ahead={pin_ahead}");
    println!("  pin_behind={pin_behind}");
    println!("  local_unpushed_commits={}", unpushed.len());
    println!("  unpushed_in_flight_commits={}", split.in_flight_commits());
    println!("  unpushed_stranded_commits={}", split.stranded_commits());
    println!(
        "  unpushed_unattributed_commits={}",
        split.unattributed.len()
    );
    println!("  checkout_dirty={dirty}");
    if !split.in_flight.is_empty() {
        println!("  in_flight_branches={}", render_branches(&split.in_flight));
    }
    if !split.stranded.is_empty() {
        println!("  stranded_branches={}", render_branches(&split.stranded));
    }

    let mut failures = pin_state_failures(
        mode,
        &branch,
        dirty,
        &expected_pin,
        &checkout_head,
        &origin_main,
        checkout_ahead,
        checkout_behind,
        pin_ahead,
        pin_behind,
    );
    // Fail on STRANDED work only. Commits on a branch a worktree still holds are in flight —
    // a normal state, and unavoidable while egress is down — so failing on them makes the
    // checker permanently red for a benign condition, which trains every reader to ignore it.
    // Then a genuinely stranded commit looks identical to the noise. That is how a signal dies:
    // not by breaking, but by being permanently slightly wrong.
    if !split.stranded.is_empty() {
        let shas: Vec<String> = split
            .stranded
            .iter()
            .flat_map(|branch| branch.commits.iter().cloned())
            .take(20)
            .collect();
        failures.push(format!(
            "{} local commit(s) are unreachable from every origin/* ref AND from every worktree \
             — nothing is holding this work, so a `git branch -D` loses it. Publish or \
             deliberately retire: {} [{}]",
            split.stranded_commits(),
            render_branches(&split.stranded),
            shas.join(","),
        ));
    }
    if !split.unattributed.is_empty() {
        failures.push(format!(
            "{} local commit(s) are unreachable from every origin/* ref and from every local \
             branch (detached HEAD only): {}",
            split.unattributed.len(),
            split
                .unattributed
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
        ));
    }
    if split.in_flight_commits() > 0 {
        println!(
            "  note={} unpushed commit(s) are IN FLIGHT on branches with a live worktree; not \
             drift, but they are unpublished — push them when egress allows: {}",
            split.in_flight_commits(),
            render_branches(&split.in_flight),
        );
    }

    if dirty {
        println!(
            "  warning=agent-utils worktree has uncommitted changes; active work must be committed and pushed through a feature branch"
        );
    }
    if failures.is_empty() {
        println!("state=ok");
        return Ok(true);
    }
    for failure in failures {
        eprintln!("ERROR: {failure}");
    }
    println!("state=drift");
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
fn pin_state_failures(
    mode: CheckoutMode,
    branch: &str,
    dirty: bool,
    expected_pin: &str,
    checkout_head: &str,
    origin_main: &str,
    checkout_ahead: u64,
    checkout_behind: u64,
    pin_ahead: u64,
    pin_behind: u64,
) -> Vec<String> {
    let mut failures = Vec::new();
    if mode == CheckoutMode::CanonicalShared && branch != "main" {
        failures.push(format!(
            "canonical checkout branch is {branch:?}, expected main"
        ));
    }
    if mode == CheckoutMode::IsolatedClone && dirty {
        failures.push(
            "isolated-clone agent-utils checkout is dirty; clean exact-pin equality is required"
                .to_string(),
        );
    }
    if checkout_head != origin_main || checkout_ahead != 0 || checkout_behind != 0 {
        failures.push(format!(
            "checkout is not exactly origin/main (ahead={checkout_ahead}, behind={checkout_behind})"
        ));
    }
    if expected_pin != checkout_head {
        failures.push(format!(
            "parent-recorded Hermit agent-utils gitlink {expected_pin} does not match checkout {checkout_head}"
        ));
    }
    if expected_pin != origin_main || pin_ahead != 0 || pin_behind != 0 {
        failures.push(format!(
            "parent-recorded Hermit agent-utils gitlink is not exactly origin/main (ahead={pin_ahead}, behind={pin_behind})"
        ));
    }
    failures
}

fn validate_compat_link(root: &Path) -> Result<PathBuf, CheckError> {
    let link = root.join(AGENT_UTILS_LINK);
    let metadata = fs::symlink_metadata(&link).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            CheckError::InvalidOutput(format!(
                "{} is missing; restore the tracked `{} -> {}` compatibility symlink",
                link.display(),
                AGENT_UTILS_LINK,
                AGENT_UTILS_LINK_TARGET
            ))
        } else {
            CheckError::Io {
                tool: format!("inspect compatibility path {}", link.display()),
                source,
            }
        }
    })?;
    if !metadata.file_type().is_symlink() {
        return Err(CheckError::InvalidOutput(format!(
            "{} is not a symlink; expected `{} -> {}`",
            link.display(),
            AGENT_UTILS_LINK,
            AGENT_UTILS_LINK_TARGET
        )));
    }
    let target = fs::read_link(&link).map_err(|source| CheckError::Io {
        tool: format!("read compatibility symlink {}", link.display()),
        source,
    })?;
    if target != Path::new(AGENT_UTILS_LINK_TARGET) {
        return Err(CheckError::InvalidOutput(format!(
            "{} points to {:?}; expected exact relative target {:?}",
            link.display(),
            target,
            Path::new(AGENT_UTILS_LINK_TARGET)
        )));
    }
    Ok(link)
}

fn expected_agent_utils_pin(root: &Path, hermit: &Path) -> Result<(String, String), CheckError> {
    let parent_hermit_entry = git(root, ["ls-tree", "HEAD", "--", HERMIT_PATH])?;
    let parent_hermit = parse_gitlink_entry(&parent_hermit_entry, HERMIT_PATH)?;
    let commit = format!("{parent_hermit}^{{commit}}");
    if !git_object_exists(hermit, &commit)? {
        return Err(CheckError::InvalidOutput(format!(
            "{} does not contain parent-recorded Hermit commit {}; fetch it with `git -C {} fetch origin main`, then rerun",
            hermit.display(),
            parent_hermit,
            hermit.display()
        )));
    }
    let nested_entry = git(hermit, ["ls-tree", &parent_hermit, "--", AGENT_UTILS_LINK])?;
    let expected_pin = parse_gitlink_entry(&nested_entry, AGENT_UTILS_LINK).map_err(|_| {
        CheckError::InvalidOutput(format!(
            "parent-recorded Hermit commit {} has no agent-utils gitlink at {}; update the Hermit pin or repair that commit before rerunning",
            parent_hermit,
            AGENT_UTILS_LINK
        ))
    })?;
    Ok((parent_hermit, expected_pin))
}

fn parse_gitlink_entry(output: &str, expected_path: &str) -> Result<String, CheckError> {
    let mut fields = output.split_whitespace();
    let mode = fields.next();
    let kind = fields.next();
    let object = fields.next();
    let path = fields.next();
    if fields.next().is_some()
        || mode != Some("160000")
        || kind != Some("commit")
        || path != Some(expected_path)
        || !object.is_some_and(|value| {
            value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(CheckError::InvalidOutput(format!(
            "expected one gitlink entry for {expected_path}, got {output:?}"
        )));
    }
    Ok(object.expect("validated above").to_string())
}

fn git_object_exists(repo: &Path, object: &str) -> Result<bool, CheckError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(["cat-file", "-e", object]);
    command
        .output()
        .map(|output| output.status.success())
        .map_err(|source| CheckError::Io {
            tool: format!("git -C {} cat-file -e {object}", repo.display()),
            source,
        })
}

fn divergence(repo: &Path, left: &str, right: &str) -> Result<(u64, u64), CheckError> {
    let range = format!("{left}...{right}");
    let output = git(repo, ["rev-list", "--left-right", "--count", &range])?;
    parse_divergence(&output)
}

fn parse_divergence(output: &str) -> Result<(u64, u64), CheckError> {
    let values: Vec<_> = output.split_whitespace().collect();
    if values.len() != 2 {
        return Err(CheckError::InvalidOutput(format!(
            "expected two divergence counts, got {output:?}"
        )));
    }
    let ahead = values[0].parse().map_err(|error| {
        CheckError::InvalidOutput(format!("invalid ahead count {:?}: {error}", values[0]))
    })?;
    let behind = values[1].parse().map_err(|error| {
        CheckError::InvalidOutput(format!("invalid behind count {:?}: {error}", values[1]))
    })?;
    Ok((ahead, behind))
}

fn git<I, S>(repo: &Path, args: I) -> Result<String, CheckError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(args);
    let output = run(&mut command)?;
    Ok(output.trim_end().to_string())
}

fn git_lines<I, S>(repo: &Path, args: I) -> Result<Vec<String>, CheckError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Ok(git(repo, args)?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn run(command: &mut Command) -> Result<String, CheckError> {
    let display = format!("{command:?}");
    let output = command.output().map_err(|source| CheckError::Io {
        tool: display.clone(),
        source,
    })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CheckError::Command {
            command: display,
            detail: if detail.is_empty() {
                format!("exit {}", exit_number(output.status))
            } else {
                detail
            },
        });
    }
    String::from_utf8(output.stdout)
        .map_err(|error| CheckError::InvalidOutput(format!("non-UTF-8 output: {error}")))
}

fn single_line<'a>(name: &str, output: &'a str) -> Result<&'a str, CheckError> {
    let mut lines = output.lines();
    let value = lines
        .next()
        .filter(|line| !line.is_empty())
        .ok_or_else(|| CheckError::InvalidOutput(format!("{name} was empty")))?;
    if lines.next().is_some() {
        return Err(CheckError::InvalidOutput(format!(
            "{name} returned multiple lines"
        )));
    }
    Ok(value)
}

/// One local branch that carries commits no `origin/*` ref can reach.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnpushedBranch {
    name: String,
    /// Path of the git worktree that has this branch checked out, when one does.
    worktree: Option<String>,
    /// Commits on this branch unreachable from every `origin/*` ref.
    commits: Vec<String>,
}

/// Unpushed commits split by whether anything is still holding them.
///
/// `local_unpushed_commits` alone cannot distinguish these, and they demand opposite
/// responses, so collapsing them is what makes the number ignorable.
#[derive(Debug, Default, PartialEq, Eq)]
struct UnpushedSplit {
    /// On a branch with a live worktree: work in progress. Expected, and routine during an
    /// egress outage when a branch cannot be pushed yet. Reported, never a failure.
    in_flight: Vec<UnpushedBranch>,
    /// On a branch nobody has checked out: nothing is holding this work, so it is one
    /// `git branch -D` away from being unrecoverable. This is the condition worth failing on.
    stranded: Vec<UnpushedBranch>,
    /// Unpushed commits reachable from no local branch at all (e.g. only from a detached
    /// HEAD). Reported separately because no branch name can be offered as the fix location.
    unattributed: Vec<String>,
}

impl UnpushedSplit {
    fn in_flight_commits(&self) -> usize {
        self.in_flight.iter().map(|b| b.commits.len()).sum()
    }

    fn stranded_commits(&self) -> usize {
        self.stranded.iter().map(|b| b.commits.len()).sum()
    }
}

fn render_branches(branches: &[UnpushedBranch]) -> String {
    branches
        .iter()
        .map(|branch| match &branch.worktree {
            Some(path) => format!(
                "{} ({} commit(s), {path})",
                branch.name,
                branch.commits.len()
            ),
            None => format!("{} ({} commit(s))", branch.name, branch.commits.len()),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Split every unpushed commit by the branches that carry it.
///
/// A commit held by BOTH an in-flight and a stranded branch counts as in-flight: something is
/// still holding it, so it is not at risk. Classifying it as stranded would raise an alarm that
/// no action can clear, which is the same signal-killing pattern in miniature.
fn classify_unpushed(
    all_unpushed: &[String],
    branch_commits: &[(String, Vec<String>)],
    worktree_branches: &[(String, String)],
) -> UnpushedSplit {
    let worktree_of = |name: &str| -> Option<String> {
        worktree_branches
            .iter()
            .find(|(branch, _)| branch == name)
            .map(|(_, path)| path.clone())
    };

    let mut split = UnpushedSplit::default();
    let mut held: Vec<&str> = Vec::new();
    for (name, commits) in branch_commits {
        if commits.is_empty() {
            continue;
        }
        let branch = UnpushedBranch {
            name: name.clone(),
            worktree: worktree_of(name),
            commits: commits.clone(),
        };
        if branch.worktree.is_some() {
            held.extend(commits.iter().map(String::as_str));
            split.in_flight.push(branch);
        } else {
            split.stranded.push(branch);
        }
    }

    // A commit held by a worktree branch is not stranded even if a parked branch also has it.
    for branch in &mut split.stranded {
        branch.commits.retain(|sha| !held.contains(&sha.as_str()));
    }
    split.stranded.retain(|branch| !branch.commits.is_empty());

    let attributed: Vec<&str> = branch_commits
        .iter()
        .flat_map(|(_, commits)| commits.iter().map(String::as_str))
        .collect();
    split.unattributed = all_unpushed
        .iter()
        .filter(|sha| !attributed.contains(&sha.as_str()))
        .cloned()
        .collect();
    split
}

/// Branch -> worktree path, from `git worktree list --porcelain`.
fn parse_worktree_branches(output: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut current: Option<String> = None;
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(path.to_string());
        } else if let Some(reference) = line.strip_prefix("branch ") {
            if let Some(path) = current.clone() {
                let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
                pairs.push((name.to_string(), path));
            }
        }
    }
    pairs
}

fn exit_number(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

fn exit_code(status: ExitStatus) -> ExitCode {
    ExitCode::from(u8::try_from(exit_number(status).clamp(0, 255)).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before epoch")
                .as_nanos();
            let path = env::temp_dir().join(format!(
                "check-agent-utils-pin-{label}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn exact_pin_failures(mode: CheckoutMode, branch: &str, dirty: bool) -> Vec<String> {
        pin_state_failures(mode, branch, dirty, "same", "same", "same", 0, 0, 0, 0)
    }

    fn branch(name: &str, worktree: Option<&str>, commits: &[&str]) -> UnpushedBranch {
        UnpushedBranch {
            name: name.to_string(),
            worktree: worktree.map(str::to_string),
            commits: commits.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn owned(pairs: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        pairs
            .iter()
            .map(|(name, commits)| {
                (
                    name.to_string(),
                    commits.iter().map(|s| s.to_string()).collect(),
                )
            })
            .collect()
    }

    fn worktrees(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(b, w)| (b.to_string(), w.to_string()))
            .collect()
    }

    /// The observed 2026-08-06 shape: five unpushed commits, four of them held by live
    /// worktrees and one on a parked recovery ref. Only the parked one is stranded, and the
    /// old union rule failed on all five.
    #[test]
    fn splits_in_flight_worktree_branches_from_stranded_ones() {
        let all = vec![
            "0cb9576".to_string(),
            "7dc20de".to_string(),
            "bcb82a6".to_string(),
            "3c64150".to_string(),
            "d9d9437".to_string(),
        ];
        let split = classify_unpushed(
            &all,
            &owned(&[
                ("worker-thread-exception-fails-loudly", &["0cb9576"]),
                ("codex/cpu-timeout-platform-multiplier", &["7dc20de"]),
                ("recovery/primary-mode-flip-20260804", &["bcb82a6"]),
                ("codex/cgroups-cap-land", &["3c64150", "d9d9437"]),
                ("main", &[]),
            ]),
            &worktrees(&[
                ("worker-thread-exception-fails-loudly", "/s/au-worker-exc"),
                ("codex/cpu-timeout-platform-multiplier", "/s/au-cputo-mult"),
                ("codex/cgroups-cap-land", "/s/au-cap-land"),
            ]),
        );
        assert_eq!(split.in_flight_commits(), 4);
        assert_eq!(split.stranded_commits(), 1);
        assert!(split.unattributed.is_empty());
        assert_eq!(
            split.stranded,
            vec![branch(
                "recovery/primary-mode-flip-20260804",
                None,
                &["bcb82a6"]
            )]
        );
    }

    /// The whole point: a fleet mid-egress-outage, every branch held by a worktree, must be
    /// CLEAN. Under the old union rule this was a permanent failure.
    #[test]
    fn work_in_flight_on_worktree_branches_is_not_stranded() {
        let split = classify_unpushed(
            &["aaa".to_string(), "bbb".to_string()],
            &owned(&[("feature-a", &["aaa"]), ("feature-b", &["bbb"])]),
            &worktrees(&[("feature-a", "/s/a"), ("feature-b", "/s/b")]),
        );
        assert_eq!(split.stranded_commits(), 0);
        assert_eq!(split.in_flight_commits(), 2);
    }

    /// The negative half of the bracket: the checker must still catch genuinely abandoned
    /// work, or the split would just be a way of never failing.
    #[test]
    fn an_abandoned_branch_with_no_worktree_is_stranded() {
        let split = classify_unpushed(
            &["dead1".to_string()],
            &owned(&[("backup-enum-redesign-9bef79f", &["dead1"])]),
            &worktrees(&[]),
        );
        assert_eq!(split.stranded_commits(), 1);
        assert_eq!(split.in_flight_commits(), 0);
        assert!(render_branches(&split.stranded).contains("backup-enum-redesign-9bef79f"));
    }

    /// A commit on both a held and a parked branch is NOT at risk: something still holds it.
    /// Reporting it as stranded would raise an alarm no action can clear.
    #[test]
    fn a_commit_held_by_any_worktree_branch_is_not_stranded() {
        let split = classify_unpushed(
            &["shared".to_string()],
            &owned(&[("parked-copy", &["shared"]), ("live", &["shared"])]),
            &worktrees(&[("live", "/s/live")]),
        );
        assert_eq!(split.stranded_commits(), 0);
        assert_eq!(split.in_flight_commits(), 1);
        assert!(split.stranded.is_empty());
    }

    /// A commit on no local branch at all (detached HEAD) has no branch to name as the fix
    /// location, so it gets its own class rather than being silently dropped.
    #[test]
    fn commits_on_no_local_branch_are_unattributed_not_dropped() {
        let split = classify_unpushed(
            &["detached".to_string()],
            &owned(&[("main", &[])]),
            &worktrees(&[]),
        );
        assert_eq!(split.unattributed, vec!["detached".to_string()]);
        assert_eq!(split.stranded_commits(), 0);
        assert_eq!(split.in_flight_commits(), 0);
    }

    #[test]
    fn renders_branches_with_counts_and_worktrees() {
        let rendered = render_branches(&[
            branch("held", Some("/s/wt"), &["a", "b"]),
            branch("parked", None, &["c"]),
        ]);
        assert_eq!(rendered, "held (2 commit(s), /s/wt), parked (1 commit(s))");
    }

    #[test]
    fn parses_worktree_porcelain_branch_mapping() {
        let output = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                      worktree /s/wt\nHEAD def\nbranch refs/heads/codex/feature\n\n\
                      worktree /s/detached\nHEAD 123\ndetached\n";
        assert_eq!(
            parse_worktree_branches(output),
            worktrees(&[("main", "/repo"), ("codex/feature", "/s/wt")]),
            "a detached worktree holds no branch and must not appear"
        );
    }

    #[test]
    fn parses_git_divergence_counts() {
        assert_eq!(parse_divergence("3\t7\n").unwrap(), (3, 7));
        assert!(parse_divergence("3").is_err());
    }

    #[test]
    fn parses_only_the_expected_gitlink_entry() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            parse_gitlink_entry(
                &format!("160000 commit {sha}\tagent-utils\n"),
                "agent-utils"
            )
            .unwrap(),
            sha
        );
        assert!(
            parse_gitlink_entry(&format!("120000 blob {sha}\tagent-utils\n"), "agent-utils")
                .is_err()
        );
        assert!(
            parse_gitlink_entry(&format!("160000 commit {sha}\tother\n"), "agent-utils").is_err()
        );
    }

    #[test]
    fn compatibility_path_requires_the_exact_relative_symlink() {
        let temp = TestDir::new("compat-link");
        fs::create_dir_all(temp.path().join(AGENT_UTILS_LINK_TARGET))
            .expect("create symlink target");
        symlink(AGENT_UTILS_LINK_TARGET, temp.path().join(AGENT_UTILS_LINK))
            .expect("create compatibility symlink");

        assert_eq!(
            validate_compat_link(temp.path()).unwrap(),
            temp.path().join(AGENT_UTILS_LINK)
        );
    }

    #[test]
    fn missing_or_wrong_compatibility_path_is_actionable() {
        let missing = TestDir::new("missing-link");
        let error = validate_compat_link(missing.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("restore the tracked `agent-utils -> hermit/agent-utils`"));

        let wrong = TestDir::new("wrong-link");
        symlink("elsewhere", wrong.path().join(AGENT_UTILS_LINK))
            .expect("create wrong compatibility symlink");
        let error = validate_compat_link(wrong.path()).unwrap_err().to_string();
        assert!(error.contains("expected exact relative target"));
        assert!(error.contains("hermit/agent-utils"));
    }

    #[test]
    fn canonical_path_and_other_clone_select_different_modes() {
        let canonical = Path::new("/home/test/work/dev-hermit");
        assert_eq!(
            classify_checkout_mode(canonical, canonical),
            CheckoutMode::CanonicalShared
        );
        assert_eq!(
            classify_checkout_mode(Path::new("/tmp/dev-hermit-clone/repo"), canonical),
            CheckoutMode::IsolatedClone
        );
    }

    #[test]
    fn clean_detached_isolated_clone_is_accepted_at_exact_pin() {
        assert!(exact_pin_failures(CheckoutMode::IsolatedClone, "", false).is_empty());
    }

    #[test]
    fn canonical_shared_checkout_still_requires_main() {
        let detached = exact_pin_failures(CheckoutMode::CanonicalShared, "", false);
        assert_eq!(detached.len(), 1);
        assert!(detached[0].contains("expected main"));
        assert!(exact_pin_failures(CheckoutMode::CanonicalShared, "main", false).is_empty());
    }

    #[test]
    fn dirty_isolated_clone_is_refused_even_at_exact_pin() {
        let failures = exact_pin_failures(CheckoutMode::IsolatedClone, "", true);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("clean exact-pin equality is required"));
    }

    #[test]
    fn isolated_clone_still_refuses_any_pin_inequality() {
        let failures = pin_state_failures(
            CheckoutMode::IsolatedClone,
            "",
            false,
            "parent",
            "checkout",
            "origin",
            1,
            2,
            3,
            4,
        );
        assert_eq!(failures.len(), 3);
        assert!(failures
            .iter()
            .any(|failure| failure.contains("checkout is not exactly origin/main")));
        assert!(failures
            .iter()
            .any(|failure| failure.contains("does not match checkout")));
        assert!(failures.iter().any(|failure| failure
            .contains("parent-recorded Hermit agent-utils gitlink is not exactly origin/main")));
    }

    #[test]
    fn help_is_trivial_and_does_not_arm_work() {
        assert!(parse_args(&[OsString::from("--help")]).unwrap().is_none());
    }

    #[test]
    fn no_fetch_is_explicit() {
        let options = parse_args(&[OsString::from("--no-fetch")])
            .unwrap()
            .unwrap();
        assert!(!options.fetch);
    }
}
