#!/usr/bin/env rust-script
//! Release a worktree slot allocated by `allocate-worktree.rs`.
//!
//! CANONICAL LAYOUT (nested):
//! worktrees/slots/<slot>/{hermit,reverie,liteinst2}.
//!
//! Refuses to silently discard work: it inspects each product worktree for
//! uncommitted changes and warns loudly. `--clean` physically removes the
//! worktrees (and the now-empty slot dir), but only when they are clean or
//! `--force` is given. Ownership is updated in `worktree-state.json` and the
//! machine-parseable table block in `worktrees/ACTIVE.md` is regenerated.
//!
//! ```cargo
//! [dependencies]
//! fs2 = "0.4"
//! libc = "0.2"
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! sha2 = "0.10"
//! ```
// rust-script does not track #[path] dependencies in its cache key. Keep this
// marker synchronized with agent_snapshot.rs status-contract changes.
// agent-snapshot-schema-1-pane-identities-roles-authority-v7
#[allow(dead_code)]
#[path = "agent_snapshot.rs"]
mod agent_snapshot;
#[allow(dead_code)]
#[path = "worktree_registry.rs"]
mod worktree_registry;

use fs2::FileExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, OsStr};
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::{exit, Command, Output};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const AGENT_SNAPSHOT_MAX_AGE_SECS: u64 = 10 * 60;

const USAGE: &str = r#"Usage: release-worktree.rs --slot SLOT [OPTIONS]

LEGACY INTERFACE. In a project configured with .wrkslots.yml this command
refuses before salvaging, moving, or removing anything. Use
ci-hub/bin/wrkslots remove for a reclaimable slot, or ci-hub/bin/wrkslots
recover for an interrupted operation.

The options below remain only for unconfigured migration fixtures that still
exercise the previous nested-slot registry implementation.

Required:
  --slot SLOT         Slot to release (named token or slotNN).

Options:
  --agent NAME        If the slot is shared, drop only this agent. When other
                      agents remain, the slot stays active.
  --clean             Physically remove the git worktree(s) and the slot dir.
                      Refused if a worktree has uncommitted work OR unpushed
                      commits, unless --push (safe) or --force (dangerous).
  --push              Before removal, push any unpushed slot feature branches to
                      their upstream (with-proxy) so removal is fully recoverable
                      (the "push-then-remove" safe reclaim protocol).
  --request-release   Exact live owner records a draining lease and an automatic
                      recovery handoff before attempting ordinary clean release.
                      Requires --clean and --actor. Owner identity authorizes the
                      action; actual process/path use remains a separate veto.
  --stale-clear       Finalize one expired lease after exact owner-death and the
                      ordinary process/domain/cleanliness/durability proofs.
                      Requires --clean, --expected-generation, and --actor.
  --recover-quarantined-handoff
                      Retry one quarantined automatic handoff after its recorded
                      work has been made clean and remotely durable. Requires
                      --stale-clear; all ordinary stale-clear proofs still apply.
  --stale-clear-all   Event-driven reaper used before allocation. Attempts every
                      expired, non-quarantined slot lease through --stale-clear.
  --expected-generation ID
                      CAS identity required by --stale-clear.
  --actor ID          Attributed requester/reclaimer identity.
  --recover-submodule-cleanup
                      Recover an interrupted normal cleanup: restore any exact
                      quarantined admin, reinitialize recursively, and repeat all
                      proofs. Requires --clean and is incompatible with --force.
  --coordinator-finalize-orphan-residue
                      Finalize a released legacy row whose product path is only
                      generated residue and has zero Git worktree registrations.
                      Requires --clean and --orphan-recovery-note. This explicit
                      coordinator recovery is journaled and preserves hashed
                      textual evidence before removing generated residue.
  --orphan-recovery-note TEXT
                      Required durable reason for orphan-residue finalization.
  --force             Allow --clean despite uncommitted/unpushed work (dangerous).
  -h, --help          Show this help.

In those legacy fixtures, without --clean the physical worktree is left in place and the
slot is marked released in state. With --clean the worktree is removed and the
slot entry is dropped from state. Feature branches are NEVER deleted.

Examples:
  ./scripts/release-worktree.rs --slot kvm
  ./scripts/release-worktree.rs --slot kvm --clean --request-release --actor hermit-kvm
  ./scripts/release-worktree.rs --slot kvm --clean --stale-clear \
    --recover-quarantined-handoff --expected-generation ID --actor coordinator
  ./scripts/release-worktree.rs --stale-clear-all --actor allocator:hermit-kvm
  ./scripts/release-worktree.rs --slot slot01 --clean
  ./scripts/release-worktree.rs --slot kvm --agent hermit-ci   # drop one sharer
"#;

fn die(msg: &str) -> ! {
    eprintln!("release-worktree: {msg}");
    exit(1);
}

fn find_root() -> PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if dir.join(".gitmodules").is_file()
            && dir.join("hermit").is_dir()
            && dir.join("reverie").is_dir()
            && dir.join("liteinst2").is_dir()
        {
            return dir;
        }
        if !dir.pop() {
            die("could not locate dev-hermit root (need .gitmodules + hermit/ + reverie/ + liteinst2/)");
        }
    }
}

/// Hold the single registry-writer authority across every read, physical
/// mutation, and state/ACTIVE update. The allocator takes this same lock.
fn lock_registry(root: &Path) -> fs::File {
    let path = root.join("worktree-state.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap_or_else(|error| die(&format!("open registry lock {}: {error}", path.display())));
    FileExt::lock_exclusive(&file)
        .unwrap_or_else(|error| die(&format!("lock registry {}: {error}", path.display())));
    file
}

/// Exclude agent-podman container creation from the release proof.  The
/// wrapper takes this exact lock before spawning Podman and retains it until a
/// cidfile has been durably registered (or the child exits without one).
fn lock_container_lifecycle(root: &Path) -> Result<(fs::File, String), String> {
    let path = root.join("ignored/ci-hub/agent-container-lifecycle.lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create container authority {}: {error}", parent.display()))?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("open container lifecycle lock {}: {error}", path.display()))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => break,
            Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                return Err(format!(
                    "container lifecycle remained busy for 30s at {}",
                    path.display()
                ))
            }
            Err(error) => {
                return Err(format!(
                    "lock container lifecycle {}: {error}",
                    path.display()
                ))
            }
        }
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?
        .as_nanos();
    let token = format!("release-worktree:{}:{nonce}", std::process::id());
    file.set_len(0)
        .and_then(|()| file.write_all(format!("{token}\n").as_bytes()))
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            format!(
                "persist container lifecycle token {}: {error}",
                path.display()
            )
        })?;
    Ok((file, token))
}

fn now_iso() -> String {
    match Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "unknown".to_string(),
    }
}

fn git_output(dir: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| {
            format!(
                "could not run git -C {} {}: {e}",
                dir.display(),
                args.join(" ")
            )
        })
}

/// Run a Git inspection that must succeed before cleanup may continue.
fn git_inspect(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_output(dir, args)?;
    if !out.status.success() {
        return Err(format!(
            "git -C {} {} failed (exit {}): {}",
            dir.display(),
            args.join(" "),
            out.status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Slot name is a lowercase named token (for example `kvm`) or `slotNN`.
fn valid_slot(name: &str) -> bool {
    worktree_registry::valid_slot_name(name)
}

#[derive(Clone, Debug)]
struct CleanTarget {
    label: &'static str,
    branch_key: &'static str,
    primary: PathBuf,
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct OrphanTarget {
    label: &'static str,
    original: PathBuf,
    fenced: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResidueFile {
    relative: String,
    bytes: u64,
    sha256: String,
    copied: bool,
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResidueInventory {
    directories: Vec<String>,
    files: Vec<ResidueFile>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RepoSnapshot {
    path: PathBuf,
    head: String,
    origin_url: String,
}

#[derive(Clone, Debug)]
struct RepoInspection {
    snapshot: RepoSnapshot,
    status: String,
}

fn validate_owners(slot: &str, slot_state: &Value) -> Result<(), String> {
    let agents = slot_state["agents"]
        .as_array()
        .ok_or_else(|| format!("slot {slot} has no readable agents array"))?;
    if agents.is_empty() {
        return Err(format!("slot {slot} has no recorded owner"));
    }

    let mut names = BTreeSet::new();
    let mut mutating = 0usize;
    for (index, agent) in agents.iter().enumerate() {
        let name = agent["name"]
            .as_str()
            .ok_or_else(|| format!("slot {slot} agent {index} has no string name"))?;
        if name.is_empty()
            || name.trim() != name
            || name == "-"
            || name.contains('|')
            || name.chars().any(char::is_control)
        {
            return Err(format!(
                "slot {slot} agent {index} has invalid name '{name}'"
            ));
        }
        if !names.insert(name.to_string()) {
            return Err(format!("slot {slot} records duplicate agent '{name}'"));
        }
        let read_only = agent["read_only"]
            .as_bool()
            .ok_or_else(|| format!("slot {slot} agent '{name}' has no boolean read_only"))?;
        if !read_only {
            mutating += 1;
        }
    }
    if mutating != 1 {
        return Err(format!(
            "slot {slot} must have exactly one mutating owner, found {mutating}"
        ));
    }
    Ok(())
}

fn validate_canonical_slot_paths(slot: &str, slot_state: &Value) -> Result<(), String> {
    for product in ["hermit", "reverie", "liteinst2"] {
        let key = format!("{product}_path");
        let expected = format!("worktrees/slots/{slot}/{product}");
        if slot_state[key.as_str()].as_str() != Some(expected.as_str()) {
            return Err(format!(
                "slot {slot} records noncanonical {key}; expected exact path '{expected}'"
            ));
        }
    }
    Ok(())
}

fn canonical_exact(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "target path '{}' is not a normalized relative path",
            relative.display()
        ));
    }

    let canonical_root = fs::canonicalize(root)
        .map_err(|e| format!("could not canonicalize workspace root: {e}"))?;
    let target = root.join(relative);
    let canonical = fs::canonicalize(&target)
        .map_err(|e| format!("could not canonicalize target {}: {e}", target.display()))?;
    let expected = canonical_root.join(relative);
    if canonical != expected {
        return Err(format!(
            "target {} resolves to {}, not exact path {}; refusing symlink/path alias",
            target.display(),
            canonical.display(),
            expected.display()
        ));
    }
    Ok(canonical)
}

/// Reuse the repository's canonical state/ACTIVE/branch reconciler. Exact
/// physical-path binding remains local below because that verifier deliberately
/// checks checkout state, not destructive-target identity.
fn verify_registry(root: &Path) -> Result<(), String> {
    verify_registry_scope(root, None)
}

/// Cleanup is already bound to one state-recorded canonical slot. Verifying
/// that same slot prevents unrelated drift from deadlocking a guarded release;
/// it does not relax any target identity, ownership, cleanliness, publication,
/// or process proof inside the selected slot.
fn verify_registry_slot(root: &Path, slot: &str) -> Result<(), String> {
    verify_registry_scope(root, Some(slot))
}

fn verify_registry_scope(root: &Path, slot: Option<&str>) -> Result<(), String> {
    let checker = root.join("scripts/check-worktree-registry.rs");
    if !checker.is_file() {
        return Err(format!(
            "canonical registry verifier is missing: {}",
            checker.display()
        ));
    }
    let root_arg = root.to_string_lossy().into_owned();
    let mut command = Command::new(&checker);
    command.args(["--root", &root_arg]);
    if let Some(slot) = slot {
        command.args(["--slot", slot]);
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run {}: {error}", checker.display()))?;
    if !output.status.success() {
        return Err(format!(
            "canonical registry verifier refused cleanup: {}{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Bind cleanup to each exact state-recorded canonical product path and the
/// corresponding product repository's physical worktree registration.
fn clean_targets_from_state(
    root: &Path,
    state: &Value,
    slot: &str,
) -> Result<Vec<CleanTarget>, String> {
    let slot_state = state["slots"]
        .get(slot)
        .ok_or_else(|| format!("slot {slot} is not registered"))?;
    validate_owners(slot, slot_state)?;
    validate_canonical_slot_paths(slot, slot_state)?;

    let mut targets = Vec::new();
    for (label, branch_key) in [
        ("hermit", "hermit_branch"),
        ("reverie", "reverie_branch"),
        ("liteinst2", "liteinst2_branch"),
    ] {
        let expected_checkout = slot_state
            .get(branch_key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("slot {slot} has missing/non-string {branch_key}"))?;
        if expected_checkout == "-" {
            continue;
        }

        let expected_recorded = format!("worktrees/slots/{slot}/{label}");
        let expected_relative = PathBuf::from(&expected_recorded);
        let canonical = canonical_exact(root, &expected_relative)?;
        let primary = root.join(label);

        let listing = git_inspect(&primary, &["worktree", "list", "--porcelain"])?;
        let registered_matches = listing
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .filter(|candidate| Path::new(candidate) == canonical)
            .count();
        if registered_matches != 1 {
            return Err(format!(
                "slot {slot} {label} target {} has {registered_matches} matching physical worktree registrations; expected exactly 1",
                canonical.display()
            ));
        }

        targets.push(CleanTarget {
            label,
            branch_key,
            primary,
            path: canonical,
        });
    }

    let slot_relative = PathBuf::from("worktrees/slots").join(slot);
    let slot_dir = root.join(&slot_relative);
    if slot_dir.exists() {
        canonical_exact(root, &slot_relative)?;
        let allowed: BTreeSet<&str> = targets.iter().map(|target| target.label).collect();
        for entry in fs::read_dir(&slot_dir).map_err(|e| {
            format!(
                "could not inspect slot directory {}: {e}",
                slot_dir.display()
            )
        })? {
            let entry = entry.map_err(|e| format!("could not inspect slot entry: {e}"))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| format!("slot {slot} contains a non-UTF8 entry"))?;
            if !allowed.contains(name.as_str()) {
                return Err(format!(
                    "slot {slot} contains unexpected entry {}; refusing before mutation",
                    entry.path().display()
                ));
            }
        }
    }
    Ok(targets)
}

/// Drop the pane lease from a slot that has just been released.
///
/// ⚠️ A RELEASED SLOT MUST ASSERT NOTHING. `tmux_pane_id` was written at
/// allocation and never reset, so every non-clean release left a permanent claim
/// on a pane that would later be handed to somebody else. Measured 2026-08-25:
/// all 16 pane-carrying agent records in the live registry sat on released
/// slots, 16 of 16, and panes %131 and %135 were each claimed by two different
/// released slots. One of them -- released slot `codex-cpuid2258` still recording
/// agent `lane1` on pane `%3`, while `%3` was a live session -- made the identity
/// resolver report DISAGREEMENT and blocked every guarded write for ~33 hours.
///
/// ORDER MATTERS AND THIS IS DELIBERATELY LAST. Release READS `tmux_pane_id` to
/// know whose lease it is ending -- see the `recorded owner '{owner}' lacks
/// tmux_pane_id lease data` refusal earlier in this file. Clearing it any sooner
/// would break the very identification that makes release safe. Read, release,
/// then clear.
///
/// BOTH COPIES OR NEITHER. `slot-owner-authority.py` requires
/// `owner_sidecar.tmux_pane_id == agents[].tmux_pane_id` for an ORC sidecar and
/// raises "ORC owner_sidecar disagrees with the registered pane/cgroup lease"
/// when they differ. Clearing one and leaving the other would manufacture that
/// error. (Released slots do not currently reach that check -- it refuses them
/// with "expected an owner-lease lifecycle state" -- but the invariant is cheap
/// to honour and the check may widen.)
///
/// `--clean` does not need this: it removes the slot from the registry outright.
fn clear_pane_leases(slot_row: &mut Value) {
    if let Some(agents) = slot_row.get_mut("agents").and_then(Value::as_array_mut) {
        for agent in agents.iter_mut() {
            if agent.get("tmux_pane_id").is_some() {
                agent["tmux_pane_id"] = Value::Null;
            }
        }
    }
    if let Some(sidecar) = slot_row.get_mut("owner_sidecar") {
        if sidecar.get("tmux_pane_id").is_some() {
            sidecar["tmux_pane_id"] = Value::Null;
        }
    }
}

fn state_path(root: &Path) -> PathBuf {
    root.join("worktree-state.json")
}

/// Replace one small registry authority atomically and durably.  A release
/// journal must survive a process or host crash without leaving a truncated
/// JSON document that makes the fenced worktree unrecoverable.
fn durable_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "path has no parent"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "path has no UTF-8 name"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?
        .as_nanos();
    let temporary = parent.join(format!(
        ".{name}.release-{}-{nonce}.tmp",
        std::process::id()
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn load_state(root: &Path) -> Value {
    let p = state_path(root);
    if !p.exists() {
        die("worktree-state.json not found; nothing to release");
    }
    let txt = std::fs::read_to_string(&p).unwrap_or_else(|e| die(&format!("read state: {e}")));
    let v: Value = serde_json::from_str(&txt)
        .unwrap_or_else(|e| die(&format!("parse worktree-state.json: {e}")));
    worktree_registry::Registry::from_value(&v)
        .unwrap_or_else(|e| die(&format!("validate worktree-state.json: {e}")));
    v
}

fn save_state(root: &Path, state: &mut Value) {
    state["updated"] = json!(now_iso());
    state["version"] = json!(3);
    worktree_registry::Registry::from_value(state)
        .unwrap_or_else(|e| die(&format!("refuse invalid worktree-state.json write: {e}")));
    let txt = serde_json::to_string_pretty(state).unwrap();
    durable_replace(&state_path(root), (txt + "\n").as_bytes())
        .unwrap_or_else(|e| die(&format!("write state: {e}")));
}

fn regen_active_md(root: &Path, state: &Value) -> bool {
    const BEGIN: &str = "<!-- BEGIN worktree-state (managed by scripts/allocate-worktree.rs; do not edit inside) -->";
    const END: &str = "<!-- END worktree-state -->";

    let mut rows = String::new();
    rows.push_str("| Slot | Agent | HermitBranch | ReverieBranch | LiteInst2Branch | Task | Status | ReadOnly |\n");
    rows.push_str("| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    if let Some(slots) = state["slots"].as_object() {
        let mut names: Vec<&String> = slots.keys().collect();
        names.sort();
        for name in names {
            let s = &slots[name];
            let agents = s["agents"].as_array().cloned().unwrap_or_default();
            let owner = agents
                .iter()
                .find(|a| !a["read_only"].as_bool().unwrap_or(false))
                .and_then(|a| a["name"].as_str())
                .unwrap_or("-");
            let ro_agents: Vec<String> = agents
                .iter()
                .filter(|a| a["read_only"].as_bool().unwrap_or(false))
                .filter_map(|a| a["name"].as_str().map(|n| n.to_string()))
                .collect();
            let agent_cell = if ro_agents.is_empty() {
                owner.to_string()
            } else {
                format!("{owner} (+ro: {})", ro_agents.join(", "))
            };
            let hb = s["hermit_branch"].as_str().unwrap_or("-");
            let rb = s["reverie_branch"].as_str().unwrap_or("-");
            let lb = s["liteinst2_branch"].as_str().unwrap_or("-");
            let task = s["task"].as_str().unwrap_or("-");
            let status = s["status"].as_str().unwrap_or("active");
            let read_only = if ro_agents.is_empty() { "no" } else { "shared" };
            rows.push_str(&format!(
                "| {name} | {agent_cell} | {hb} | {rb} | {lb} | {task} | {status} | {read_only} |\n"
            ));
        }
    }
    let block = format!("{BEGIN}\n{rows}{END}\n");

    let path = root.join("worktrees").join("ACTIVE.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let new_content = if let (Some(b), Some(e)) = (existing.find(BEGIN), existing.find(END)) {
        let e_end = e + END.len();
        let after = existing[e_end..]
            .strip_prefix('\n')
            .unwrap_or(&existing[e_end..]);
        format!("{}{}{}", &existing[..b], block, after)
    } else {
        let sep = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        format!("{existing}{sep}\n## Machine-managed slot table\n\n{block}")
    };
    let changed = new_content != existing;
    if changed {
        durable_replace(&path, new_content.as_bytes())
            .unwrap_or_else(|e| die(&format!("write ACTIVE.md: {e}")));
    }
    changed
}

/// Cargo's own output directories inside a `target` directory.
///
/// ⚠️ AN ALLOWLIST, NOT `rm -rf target`, AND THAT IS THE WHOLE POINT. A directory
/// named `target` is not automatically regenerable. Measured across 162 slots on
/// 2026-08-27: 65G of 1.1T sat in entries Cargo never wrote, and
/// `cloudlint/hermit/target/relaxed-flag-matrix/` held `final.tsv` plus four
/// `results-*.tsv` -- somebody's measurement data. Removing the directory would
/// have destroyed it. Naming what Cargo writes means this cannot reach anything a
/// person put there, so it needs no preservation step for anyone to forget.
const REGENERABLE_BUILD_DIRS: &[&str] = &[
    "debug",
    "release",
    "doc",
    "nextest",
    "package",
    ".fingerprint",
];

/// Is any live process holding a path inside `dir`?
///
/// ⚠️ DELETING A TARGET MID-BUILD DESTROYS WORK RATHER THAN RECLAIMING WASTE, so
/// this refuses rather than guesses, and an unreadable `/proc` counts as busy.
/// One pass over `/proc` rather than `lsof`, which costs about 9.9s per call on
/// this box because it scans every descriptor the fleet holds; at 200+
/// directories that is half an hour to answer one question.
fn build_output_in_use(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return true;
    };
    for entry in entries.flatten() {
        let pid_dir = entry.path();
        let is_pid = pid_dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !is_pid {
            continue;
        }
        let mut probes: Vec<PathBuf> = vec![pid_dir.join("cwd"), pid_dir.join("exe")];
        if let Ok(fds) = fs::read_dir(pid_dir.join("fd")) {
            probes.extend(fds.flatten().map(|fd| fd.path()));
        }
        for probe in probes {
            if let Ok(resolved) = fs::read_link(&probe) {
                if resolved.starts_with(dir) {
                    return true;
                }
            }
        }
    }
    false
}

/// Remove Cargo's output from every `target` directory beneath `slot_dir`.
///
/// Returns the directories removed. This never fails the caller: reclaiming
/// build output is opportunistic and must not block a slot release, which is
/// also why it runs BEFORE the ownership and cleanliness checks that refuse. A
/// slot whose record must survive still gives its disk back.
fn scrub_build_output(slot_dir: &Path) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let mut queue = vec![slot_dir.to_path_buf()];
    // Slot layouts put `target` at `<slot>/target` or `<slot>/<repo>/target`.
    for _ in 0..3 {
        let mut next = Vec::new();
        for dir in queue.drain(..) {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !entry.file_type().is_ok_and(|t| t.is_dir()) || path.is_symlink() {
                    continue;
                }
                if entry.file_name() != "target" {
                    next.push(path);
                    continue;
                }
                if build_output_in_use(&path) {
                    eprintln!(
                        "release-worktree: leaving {} alone: a live process holds a path inside it",
                        path.display()
                    );
                    continue;
                }
                for name in REGENERABLE_BUILD_DIRS {
                    let victim = path.join(name);
                    if victim.is_dir() && !victim.is_symlink() {
                        match fs::remove_dir_all(&victim) {
                            Ok(()) => removed.push(victim),
                            Err(error) => eprintln!(
                                "release-worktree: could not remove {}: {error}",
                                victim.display()
                            ),
                        }
                    }
                }
            }
        }
        queue = next;
    }
    removed
}

/// Enumerate the target plus every recursively initialized submodule. Git owns
/// the recursion semantics; a malformed `.gitmodules` or any other inspection
/// error is a hard refusal rather than an empty/clean result.
fn initialized_repositories(target: &Path) -> Result<Vec<PathBuf>, String> {
    let canonical_target = fs::canonicalize(target)
        .map_err(|e| format!("could not canonicalize target {}: {e}", target.display()))?;
    let out = git_output(
        &canonical_target,
        &[
            "submodule",
            "foreach",
            "--quiet",
            "--recursive",
            r#"printf '%s\0' "$displaypath""#,
        ],
    )?;
    if !out.status.success() {
        return Err(format!(
            "could not enumerate initialized submodules below {} (exit {}): {}",
            canonical_target.display(),
            out.status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let mut repositories = vec![canonical_target.clone()];
    for raw in out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let relative = std::str::from_utf8(raw).map_err(|_| {
            format!(
                "initialized submodule below {} has a non-UTF8 display path",
                canonical_target.display()
            )
        })?;
        let canonical = canonical_exact(&canonical_target, Path::new(relative))?;
        if !repositories.contains(&canonical) {
            repositories.push(canonical);
        }
    }
    repositories.sort();
    Ok(repositories)
}

fn inspect_repository(path: &Path) -> Result<RepoInspection, String> {
    let status = git_inspect(path, &["status", "--porcelain=v1", "--untracked-files=all"])?;
    let head = git_inspect(path, &["rev-parse", "--verify", "HEAD"])?;
    if head.is_empty() {
        return Err(format!(
            "repository {} has no readable HEAD",
            path.display()
        ));
    }
    let origin_url = git_inspect(path, &["remote", "get-url", "origin"])?;
    if origin_url.is_empty() {
        return Err(format!(
            "repository {} has an empty origin URL",
            path.display()
        ));
    }
    Ok(RepoInspection {
        snapshot: RepoSnapshot {
            path: path.to_path_buf(),
            head,
            origin_url,
        },
        status,
    })
}

fn inspect_target_repositories(target: &Path) -> Result<Vec<RepoInspection>, String> {
    initialized_repositories(target)?
        .iter()
        .map(|path| inspect_repository(path))
        .collect()
}

/// True when the `with-proxy` egress wrapper is executable on `PATH`.
///
/// `with-proxy` is a Meta-dev-box wrapper; it does NOT exist on GitHub-hosted
/// runners, where the network is reachable directly. Spawning it
/// unconditionally made every hosted run of the pre-recycle durability
/// guardrail die with ENOENT ("could not run with-proxy git: No such file or
/// directory") BEFORE inspecting a single repository — a portability failure
/// reported as a durability failure. Probing `PATH` mirrors the convention
/// already used across this repo (`command -v with-proxy` in
/// `ci-hub/bin/agent-tool` and the workflows, `SUBMODULE_PROXY` in the
/// Makefile). Resolved once: `PATH` does not change mid-run, and a stable
/// answer keeps the guardrail's behaviour deterministic.
fn with_proxy_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join("with-proxy");
            // Must be a regular executable file, not merely present.
            fs::metadata(&candidate)
                .map(|meta| meta.is_file() && meta.mode() & 0o111 != 0)
                .unwrap_or(false)
        })
    })
}

/// `git` for network-touching inspection, behind `with-proxy` when that wrapper
/// exists. Falls back to invoking `git` directly rather than refusing: the
/// inspection itself is unchanged, only the unavailable egress wrapper is
/// dropped.
fn proxied_git() -> Command {
    if with_proxy_available() {
        let mut command = Command::new("with-proxy");
        command.arg("git");
        command
    } else {
        Command::new("git")
    }
}

/// How the current process actually invokes git, for honest error messages.
fn proxied_git_label() -> &'static str {
    if with_proxy_available() {
        "with-proxy git"
    } else {
        "git"
    }
}

fn proxy_git_inspect(path: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let label = proxied_git_label();
    let output = proxied_git()
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| format!("could not run {label}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{label} -C {} {} failed: {}",
            path.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

/// Prove HEAD is reachable from a current branch advertised by this
/// repository's own `origin`. Tags, remote HEAD, and custom refs are not
/// recoverable feature-branch authorities. Detached submodule HEADs receive
/// the same branch-ancestry proof as branch checkouts.
fn remote_durable(snapshot: &RepoSnapshot) -> Result<bool, String> {
    let stdout = String::from_utf8(proxy_git_inspect(&snapshot.path, &["ls-remote", "origin"])?)
        .map_err(|_| {
            format!(
                "git ls-remote origin returned non-UTF8 output for {}",
                snapshot.path.display()
            )
        })?;
    let mut branch_tips = BTreeSet::new();
    for (index, line) in stdout.lines().enumerate() {
        let mut fields = line.split_whitespace();
        let sha = fields.next().ok_or_else(|| {
            format!(
                "malformed ls-remote output line {} for {}",
                index + 1,
                snapshot.path.display()
            )
        })?;
        let remote_ref = fields.next().ok_or_else(|| {
            format!(
                "malformed ls-remote output line {} for {}",
                index + 1,
                snapshot.path.display()
            )
        })?;
        if fields.next().is_some()
            || !matches!(sha.len(), 40 | 64)
            || !sha.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !(remote_ref == "HEAD" || remote_ref.starts_with("refs/"))
        {
            return Err(format!(
                "malformed ls-remote output line {} for {}: {line}",
                index + 1,
                snapshot.path.display()
            ));
        }
        if remote_ref.starts_with("refs/heads/") {
            branch_tips.insert(sha.to_string());
        }
    }

    if branch_tips.contains(&snapshot.head) {
        return Ok(true);
    }
    if branch_tips.is_empty() {
        return Ok(false);
    }

    // Make the advertised branch-tip objects available for an observed
    // ancestry proof. A fetch or later Git error refuses cleanup; it cannot be
    // converted into a user-force bypass.
    proxy_git_inspect(
        &snapshot.path,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    )?;
    for remote_tip in branch_tips {
        let ancestry = git_output(
            &snapshot.path,
            &["merge-base", "--is-ancestor", &snapshot.head, &remote_tip],
        )?;
        match ancestry.status.code() {
            Some(0) => return Ok(true),
            Some(1) => {}
            _ => {
                return Err(format!(
                    "could not inspect ancestry {}..{} in {}: {}",
                    snapshot.head,
                    remote_tip,
                    snapshot.path.display(),
                    String::from_utf8_lossy(&ancestry.stderr).trim()
                ))
            }
        }
    }
    Ok(false)
}

/// Push the current branch only. Detached nested repositories cannot be given
/// an invented publication ref by cleanup; they remain at risk and are refused.
fn push_branch(path: &Path) -> Result<bool, String> {
    let branch = git_inspect(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "HEAD" {
        eprintln!("  cannot push {}: detached HEAD", path.display());
        return Ok(false);
    }
    let refspec = format!("HEAD:refs/heads/{branch}");
    let out = proxied_git()
        .arg("-C")
        .arg(path)
        .args(["push", "origin", &refspec])
        .output()
        .map_err(|e| format!("could not spawn git push for {}: {e}", path.display()))?;
    if out.status.success() {
        println!("  pushed {} -> origin/{branch}", path.display());
        Ok(true)
    } else {
        eprintln!(
            "  push failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(false)
    }
}

fn link_inside_target(link: &Path, targets: &[PathBuf]) -> bool {
    let bytes = link.as_os_str().as_bytes();
    let bytes = bytes.strip_suffix(b" (deleted)").unwrap_or(bytes);
    let link = Path::new(OsStr::from_bytes(bytes));
    targets
        .iter()
        .any(|target| link == target || link.starts_with(target))
}

fn restricted_fixture_path(root: &Path, variable: &str) -> Result<Option<PathBuf>, String> {
    let Some(test_path) = std::env::var_os(variable) else {
        return Ok(None);
    };
    let workspace = fs::canonicalize(root)
        .map_err(|error| format!("could not canonicalize fixture workspace: {error}"))?;
    let temp = fs::canonicalize(std::env::temp_dir())
        .map_err(|error| format!("could not canonicalize temporary directory: {error}"))?;
    let fixture_workspace = workspace.starts_with(&temp)
        && workspace.ancestors().any(|ancestor| {
            ancestor
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("release-worktree-test."))
                .unwrap_or(false)
        });
    let test_path = fs::canonicalize(PathBuf::from(test_path))
        .map_err(|error| format!("could not canonicalize {variable}: {error}"))?;
    if !fixture_workspace || !test_path.starts_with(&workspace) {
        return Err(format!(
            "{variable} is restricted to disposable release fixtures"
        ));
    }
    Ok(Some(test_path))
}

fn process_root(root: &Path) -> Result<PathBuf, String> {
    Ok(
        restricted_fixture_path(root, "HERMIT_RELEASE_TEST_PROC_ROOT")?
            .unwrap_or_else(|| PathBuf::from("/proc")),
    )
}

fn cgroup_root(root: &Path) -> Result<PathBuf, String> {
    Ok(
        restricted_fixture_path(root, "HERMIT_RELEASE_TEST_CGROUP_ROOT")?
            .unwrap_or_else(|| PathBuf::from("/sys/fs/cgroup")),
    )
}

#[derive(Clone, Debug)]
struct AgentPresence {
    live: bool,
    pane: Option<String>,
}

#[derive(Clone, Debug)]
struct TmuxPane {
    window: String,
    pane: String,
    pid: u32,
}

#[derive(Clone, Debug)]
struct OrcOwnerLease {
    pane: String,
    cgroup: String,
}

#[derive(Clone, Debug)]
struct SentinelAuthority {
    lease: worktree_registry::SentinelLease,
    revoked: bool,
    proof: Option<Value>,
}

#[derive(Clone, Debug)]
struct OwnerAuthorities {
    names: BTreeSet<String>,
    orc: BTreeMap<String, OrcOwnerLease>,
    sentinel: Option<SentinelAuthority>,
}

fn normalized_cgroup_path(path: &str) -> bool {
    if path == "/" || !path.starts_with('/') {
        return false;
    }
    Path::new(path.trim_start_matches('/'))
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
}

fn validate_sentinel_slot_binding(
    root: &Path,
    slot: &str,
    lease: &worktree_registry::SentinelLease,
) -> Result<(), String> {
    // Do not canonicalize the slot directory itself: a legitimate recovery
    // can begin after the physical slot directory was removed but before its
    // final registry update was made durable. The canonical workspace root
    // plus the already-validated slot token is still an exact, normalized
    // identity for the directory this lease must have guarded.
    let expected = fs::canonicalize(root)
        .map_err(|error| format!("canonicalize workspace for sentinel binding: {error}"))?
        .join("worktrees/slots")
        .join(slot);
    lease.validate_slot_binding(slot, &expected)
}

fn recorded_owner_authorities(
    root: &Path,
    slot: &str,
    slot_state: &Value,
) -> Result<OwnerAuthorities, String> {
    let mut names = BTreeSet::new();
    let mut leases = BTreeMap::new();
    for agent in slot_state["agents"].as_array().into_iter().flatten() {
        let owner = agent["name"]
            .as_str()
            .expect("validated owner must have a name");
        names.insert(owner.to_string());
    }
    if names.is_empty() {
        return Err(format!("slot {slot} has no recorded owners"));
    }
    if let Some(sentinel) = slot_state.get("coordinator_lease") {
        let sentinel = worktree_registry::SentinelLease::from_value(sentinel).map_err(|error| {
            format!("slot {slot} has malformed Codex coordinator lease authority: {error}")
        })?;
        validate_sentinel_slot_binding(root, slot, &sentinel)?;
        for agent in slot_state["agents"].as_array().into_iter().flatten() {
            if agent.get("tmux_pane_id").is_some() || agent.get("cgroup_path").is_some() {
                return Err(format!(
                    "slot {slot} mixes Codex sentinel and ORC pane/cgroup authority"
                ));
            }
        }
        let sentinel_value = sentinel.to_value()?;
        let journal = slot_state.get("coordinator_lease_revocation");
        if let Some(journal) = journal {
            let proof = journal.get("proof");
            if journal["schema_version"].as_u64() != Some(1)
                || journal["source"].as_str() != Some("codex-systemd-sentinel-v1")
                || journal.get("lease") != Some(&sentinel_value)
                || !matches!(
                    journal["phase"].as_str(),
                    Some("stop-authorized" | "revoked")
                )
                || journal["transaction_nonce"].as_str().is_none_or(|nonce| {
                    nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit())
                })
                || (journal["phase"].as_str() == Some("revoked") && !journal["proof"].is_object())
                || (journal["phase"].as_str() == Some("stop-authorized")
                    && journal.get("proof").is_some())
            {
                return Err(format!(
                    "slot {slot} has a malformed or mismatched sentinel revocation journal"
                ));
            }
            if let Some(proof) = proof {
                if proof["unit_id"].as_str() != Some(sentinel.unit.as_str())
                    || proof["recorded_main_pid"].as_u64() != Some(u64::from(sentinel.main_pid))
                    || proof["recorded_main_pid_starttime"].as_u64()
                        != Some(sentinel.main_pid_starttime)
                    || proof["cgroup_path"].as_str() != Some(sentinel.cgroup_path.as_str())
                    || proof["observed_main_pid_starttime"].as_u64()
                        == Some(sentinel.main_pid_starttime)
                    || proof["cgroup_members"]
                        .as_array()
                        .is_none_or(|members| !members.is_empty())
                    || !matches!(proof["cgroup_populated"].as_bool(), None | Some(false))
                {
                    return Err(format!(
                        "slot {slot} has a sentinel revocation proof that does not prove the recorded identity absent"
                    ));
                }
            }
        }
        return Ok(OwnerAuthorities {
            names,
            orc: leases,
            sentinel: Some(SentinelAuthority {
                lease: sentinel,
                revoked: journal.is_some_and(|value| value["phase"].as_str() == Some("revoked")),
                proof: journal.and_then(|value| value.get("proof")).cloned(),
            }),
        });
    }
    if slot_state.get("coordinator_lease_revocation").is_some() {
        return Err(format!(
            "slot {slot} has a sentinel revocation journal without a coordinator lease"
        ));
    }
    for agent in slot_state["agents"].as_array().into_iter().flatten() {
        let owner = agent["name"]
            .as_str()
            .expect("validated owner must have a name");
        let pane = agent["tmux_pane_id"].as_str().ok_or_else(|| {
            format!(
                "recorded owner '{owner}' lacks tmux_pane_id lease data; re-run allocate-worktree.rs for this owner while its exact pane is live"
            )
        })?;
        let cgroup = agent["cgroup_path"].as_str().ok_or_else(|| {
            format!(
                "recorded owner '{owner}' lacks cgroup_path lease data; re-run allocate-worktree.rs for this owner while its exact pane is live"
            )
        })?;
        if pane.is_empty() || !pane.starts_with('%') || pane.chars().any(char::is_whitespace) {
            return Err(format!(
                "recorded owner '{owner}' has invalid tmux_pane_id lease '{pane}'"
            ));
        }
        if !normalized_cgroup_path(cgroup) {
            return Err(format!(
                "recorded owner '{owner}' has invalid cgroup_path lease '{cgroup}'"
            ));
        }
        leases.insert(
            owner.to_string(),
            OrcOwnerLease {
                pane: pane.to_string(),
                cgroup: cgroup.to_string(),
            },
        );
    }
    Ok(OwnerAuthorities {
        names,
        orc: leases,
        sentinel: None,
    })
}

fn codex_sentinel_tool(root: &Path) -> Result<PathBuf, String> {
    let tool = root.join("scripts/codex-slot-sentinel.rs");
    if !tool.is_file() {
        return Err(format!(
            "canonical Codex slot sentinel authority is missing: {}",
            tool.display()
        ));
    }
    Ok(tool)
}

fn codex_sentinel_command(
    root: &Path,
    operation: &str,
    lease: &worktree_registry::SentinelLease,
    recover: bool,
) -> Result<Value, String> {
    let lease_json = serde_json::to_string(lease)
        .map_err(|error| format!("serialize Codex coordinator lease: {error}"))?;
    let mut command = Command::new(codex_sentinel_tool(root)?);
    command.args([operation, "--lease-json", &lease_json]);
    if recover {
        command.arg("--recover");
    }
    let output = command
        .output()
        .map_err(|error| format!("Codex slot sentinel {operation} unavailable: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Codex slot sentinel {operation} refused: {}{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let report: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse Codex sentinel {operation} result: {error}"))?;
    let expected_state = if operation == "verify" {
        "live"
    } else {
        "revoked"
    };
    let reported_lease = worktree_registry::SentinelLease::from_value(&report["lease"])?;
    if report["state"].as_str() != Some(expected_state) || &reported_lease != lease {
        return Err(format!(
            "Codex slot sentinel {operation} result is not bound to the exact lease"
        ));
    }
    if expected_state == "revoked" && !report["proof"].is_object() {
        return Err(format!(
            "Codex slot sentinel {operation} omitted its revocation proof"
        ));
    }
    Ok(report)
}

fn verify_codex_sentinel_live(
    root: &Path,
    lease: &worktree_registry::SentinelLease,
) -> Result<(), String> {
    codex_sentinel_command(root, "verify", lease, false).map(|_| ())
}

fn prove_codex_sentinel_revoked(
    root: &Path,
    lease: &worktree_registry::SentinelLease,
) -> Result<Value, String> {
    Ok(codex_sentinel_command(root, "prove-revoked", lease, false)?["proof"].clone())
}

fn revoke_codex_sentinel(
    root: &Path,
    lease: &worktree_registry::SentinelLease,
    recover: bool,
) -> Result<Value, String> {
    Ok(codex_sentinel_command(root, "revoke", lease, recover)?["proof"].clone())
}

fn agent_snapshot(root: &Path) -> Result<BTreeMap<String, AgentPresence>, String> {
    let path = root.join("ignored/ci-hub/agent-snapshot.json");
    let bytes = fs::read(&path).map_err(|error| {
        format!(
            "canonical ORC owner snapshot unavailable at {}: {error}",
            path.display()
        )
    })?;
    let snapshot = agent_snapshot::AgentSnapshot::parse(&bytes)
        .map_err(|error| format!("parse canonical ORC owner snapshot: {error}"))?;
    snapshot
        .ensure_canonical_authority(&path)
        .map_err(|error| format!("canonical owner snapshot authority: {error}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?
        .as_secs_f64();
    snapshot
        .ensure_fresh(now, AGENT_SNAPSHOT_MAX_AGE_SECS as f64)
        .map_err(|error| error.replace("agent snapshot", "canonical ORC owner snapshot"))?;
    let mut result = BTreeMap::new();
    for agent in &snapshot.agents {
        let name = agent.name.as_str();
        let pane = agent.tmux_pane_id.clone();
        let presence = AgentPresence {
            live: agent.status.is_live(),
            pane,
        };
        result.insert(name.to_string(), presence);
    }
    Ok(result)
}

fn tmux_panes(root: &Path) -> Result<Vec<TmuxPane>, String> {
    let bytes = if let Some(path) = restricted_fixture_path(root, "HERMIT_RELEASE_TEST_TMUX_PANES")?
    {
        fs::read(&path)
            .map_err(|error| format!("read test tmux pane authority {}: {error}", path.display()))?
    } else {
        let output = Command::new("tmux")
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{window_name}\t#{pane_id}\t#{pane_pid}",
            ])
            .output()
            .map_err(|error| format!("canonical tmux pane query unavailable: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "canonical tmux pane query failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        output.stdout
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| "canonical tmux pane query returned non-UTF8 data".to_string())?;
    let mut panes = Vec::new();
    let mut pane_ids = BTreeSet::new();
    for (index, line) in text.lines().enumerate() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 3 || fields.iter().any(|field| field.is_empty()) {
            return Err(format!(
                "malformed canonical tmux pane row {}: {line}",
                index + 1
            ));
        }
        let pid = fields[2]
            .parse::<u32>()
            .map_err(|_| format!("tmux pane {} has invalid pid", fields[1]))?;
        if !pane_ids.insert(fields[1].to_string()) {
            return Err(format!(
                "canonical tmux pane query duplicated {}",
                fields[1]
            ));
        }
        panes.push(TmuxPane {
            window: fields[0].to_string(),
            pane: fields[1].to_string(),
            pid,
        });
    }
    Ok(panes)
}

fn unified_cgroup(proc_path: &Path) -> Result<String, String> {
    let text = fs::read_to_string(proc_path.join("cgroup")).map_err(|error| {
        format!(
            "could not inspect owner cgroup {}: {error}",
            proc_path.join("cgroup").display()
        )
    })?;
    let groups: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("0::"))
        .collect();
    if groups.len() != 1 || !groups[0].starts_with('/') {
        return Err(format!(
            "owner process {} has no unique unified cgroup",
            proc_path.display()
        ));
    }
    let relative = Path::new(groups[0].trim_start_matches('/'));
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
        && !relative.as_os_str().is_empty()
    {
        return Err(format!(
            "owner cgroup path '{}' is not normalized",
            groups[0]
        ));
    }
    Ok(groups[0].to_string())
}

fn cgroup_members(cgroup_root: &Path, cgroup: &str) -> Result<Vec<u32>, String> {
    let path = cgroup_root
        .join(cgroup.trim_start_matches('/'))
        .join("cgroup.procs");
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("could not inspect owner lease {}: {error}", path.display()))?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.trim().parse::<u32>().map_err(|_| {
                format!(
                    "owner lease {} contains invalid pid '{line}'",
                    path.display()
                )
            })
        })
        .collect()
}

fn cgroup_populated(cgroup_root: &Path, cgroup: &str) -> Result<Option<bool>, String> {
    let directory = cgroup_root.join(cgroup.trim_start_matches('/'));
    let metadata = match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Ok(metadata) => metadata,
        Err(error) => {
            return Err(format!(
                "could not inspect owner cgroup lease {}: {error}",
                directory.display()
            ))
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "owner cgroup lease {} is not an exact directory",
            directory.display()
        ));
    }
    let events_path = directory.join("cgroup.events");
    let text = fs::read_to_string(&events_path).map_err(|error| {
        format!(
            "could not inspect owner subtree population {}: {error}",
            events_path.display()
        )
    })?;
    let values: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter_map(|(key, value)| (key == "populated").then_some(value.trim()))
        .collect();
    match values.as_slice() {
        ["0"] => Ok(Some(false)),
        ["1"] => Ok(Some(true)),
        _ => Err(format!(
            "owner cgroup lease {} has no unique populated 0/1 event",
            events_path.display()
        )),
    }
}

/// Resolve every registry owner through the freshness-bounded ORC snapshot and
/// exact tmux identity used by agent-podman, then bind a live pane pid to its
/// unified cgroup lease. Cleanup proceeds only after authoritative absence.
fn verify_recorded_owners_absent(
    root: &Path,
    proc_root: &Path,
    cgroup_root: &Path,
    owners: &BTreeMap<String, OrcOwnerLease>,
) -> Result<(), String> {
    let agents = agent_snapshot(root)?;
    let panes = tmux_panes(root)?;
    for (owner, lease) in owners {
        let snapshot = agents.get(owner);
        let named_panes: Vec<&TmuxPane> =
            panes.iter().filter(|pane| pane.window == *owner).collect();
        match snapshot {
            None | Some(AgentPresence { live: false, .. }) => {
                if !named_panes.is_empty() {
                    return Err(format!(
                        "recorded owner '{owner}' is absent/terminal in ORC but retains {} exact-name tmux pane(s)",
                        named_panes.len()
                    ));
                }
                if panes.iter().any(|candidate| candidate.pane == lease.pane) {
                    return Err(format!(
                        "recorded owner '{owner}' is absent/terminal but leased tmux pane {} remains under another window identity",
                        lease.pane
                    ));
                }
                if let Some(pane) = snapshot.and_then(|entry| entry.pane.as_deref()) {
                    if panes.iter().any(|candidate| candidate.pane == pane) {
                        return Err(format!(
                            "recorded owner '{owner}' is terminal but tmux pane {pane} remains"
                        ));
                    }
                }
                if cgroup_populated(cgroup_root, &lease.cgroup)? == Some(true) {
                    return Err(format!(
                        "recorded owner '{owner}' retains populated cgroup subtree {}",
                        lease.cgroup
                    ));
                }
            }
            Some(AgentPresence {
                live: true,
                pane: Some(pane_id),
            }) => {
                if pane_id != &lease.pane {
                    return Err(format!(
                        "recorded live owner '{owner}' moved from leased pane {} to {pane_id}; re-run allocator adoption before release",
                        lease.pane
                    ));
                }
                let matches: Vec<&TmuxPane> =
                    panes.iter().filter(|pane| pane.pane == *pane_id).collect();
                if matches.len() != 1 {
                    return Err(format!(
                        "recorded live owner '{owner}' pane {pane_id} resolved {} times",
                        matches.len()
                    ));
                }
                let pane = matches[0];
                let proc_path = proc_root.join(pane.pid.to_string());
                let cgroup = unified_cgroup(&proc_path)?;
                if cgroup != lease.cgroup {
                    return Err(format!(
                        "recorded live owner '{owner}' pane {pane_id} moved from leased cgroup {} to {cgroup}; re-run allocator adoption before release",
                        lease.cgroup
                    ));
                }
                let members = cgroup_members(cgroup_root, &cgroup)?;
                if !members.contains(&pane.pid) {
                    return Err(format!(
                        "recorded live owner '{owner}' pane pid {} is not bound to cgroup lease {cgroup}",
                        pane.pid
                    ));
                }
                if cgroup_populated(cgroup_root, &cgroup)? != Some(true) {
                    return Err(format!(
                        "recorded live owner '{owner}' cgroup lease {cgroup} is not subtree-populated"
                    ));
                }
                return Err(format!(
                    "recorded owner '{owner}' remains live in pane {pane_id}, cgroup {cgroup}, members={}",
                    members.len()
                ));
            }
            Some(AgentPresence {
                live: true,
                pane: None,
            }) => {
                return Err(format!(
                    "recorded live owner '{owner}' has no canonical tmux pane identity"
                ))
            }
        }
    }
    Ok(())
}

fn verify_release_authority_absent(
    root: &Path,
    proc_root: &Path,
    cgroup_root: &Path,
    authorities: &OwnerAuthorities,
) -> Result<(), String> {
    if let Some(sentinel) = &authorities.sentinel {
        if !sentinel.revoked {
            return Err(
                "Codex coordinator sentinel remains live; durable revocation is required before global release scans"
                    .to_string(),
            );
        }
        let current = prove_codex_sentinel_revoked(root, &sentinel.lease)?;
        let recorded = sentinel
            .proof
            .as_ref()
            .ok_or_else(|| "revoked Codex sentinel has no durable proof".to_string())?;
        for field in [
            "unit_id",
            "recorded_main_pid",
            "recorded_main_pid_starttime",
            "cgroup_path",
        ] {
            if current.get(field) != recorded.get(field) {
                return Err(format!(
                    "Codex sentinel revocation proof changed identity field {field}"
                ));
            }
        }
        Ok(())
    } else {
        verify_recorded_owners_absent(root, proc_root, cgroup_root, &authorities.orc)
    }
}

fn process_agent_name(proc_path: &Path) -> Option<String> {
    let raw = fs::read(proc_path.join("environ")).ok()?;
    raw.split(|byte| *byte == 0).find_map(|field| {
        field
            .strip_prefix(b"DG_AGENT_NAME=")
            .and_then(|value| String::from_utf8(value.to_vec()).ok())
            .filter(|value| !value.is_empty())
    })
}

fn inspect_process_link(
    pid: u32,
    label: &str,
    path: &Path,
    targets: &[PathBuf],
    users: &mut Vec<String>,
) {
    if let Ok(link) = fs::read_link(path) {
        if link_inside_target(&link, targets) {
            users.push(format!("pid {pid} {label}={}", link.display()));
        }
    }
}

fn inspect_process_directory_links(
    pid: u32,
    label: &str,
    path: &Path,
    targets: &[PathBuf],
    users: &mut Vec<String>,
) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        inspect_process_link(
            pid,
            &format!("{label}/{}", entry.file_name().to_string_lossy()),
            &entry.path(),
            targets,
            users,
        );
    }
}

const NAMED_READ_ONLY_WATCHERS: &[(&str, &str)] = &[("watchman", "/usr/local/bin/watchman")];

fn process_start_ticks(proc_path: &Path) -> Result<u64, String> {
    let text = fs::read_to_string(proc_path.join("stat"))
        .map_err(|error| format!("read process stat: {error}"))?;
    let (_, fields) = text
        .rsplit_once(") ")
        .ok_or_else(|| "process stat has no closing command delimiter".to_string())?;
    fields
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| "process stat omits field 22 start_ticks".to_string())?
        .parse::<u64>()
        .map_err(|error| format!("parse process field 22 start_ticks: {error}"))
}

fn named_read_only_watcher(
    proc_path: &Path,
) -> Result<Option<(&'static str, &'static str)>, String> {
    let executable = match fs::read_link(proc_path.join("exe")) {
        Ok(executable) => executable,
        Err(_) => return Ok(None),
    };
    let Some((name, expected_executable)) = NAMED_READ_ONLY_WATCHERS
        .iter()
        .copied()
        .find(|(_, expected)| executable == Path::new(expected))
    else {
        return Ok(None);
    };
    let comm = fs::read_to_string(proc_path.join("comm"))
        .map_err(|error| format!("read {name} comm: {error}"))?;
    if comm.trim_end() != name {
        return Ok(None);
    }
    let cmdline = fs::read(proc_path.join("cmdline"))
        .map_err(|error| format!("read {name} cmdline: {error}"))?;
    let fields: Vec<&[u8]> = cmdline
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect();
    if fields != [expected_executable.as_bytes(), b"--foreground"] {
        return Ok(None);
    }
    let environment = fs::read(proc_path.join("environ"))
        .map_err(|error| format!("read {name} environment: {error}"))?;
    if environment
        .split(|byte| *byte == 0)
        .any(|field| field.starts_with(b"DG_AGENT_NAME="))
    {
        return Ok(None);
    }
    Ok(Some((name, expected_executable)))
}

fn fd_flags(proc_path: &Path, fd: &OsStr) -> Result<(u64, String), String> {
    let text = fs::read_to_string(proc_path.join("fdinfo").join(fd))
        .map_err(|error| format!("read fdinfo: {error}"))?;
    let raw = text
        .lines()
        .find_map(|line| line.strip_prefix("flags:").map(str::trim))
        .ok_or_else(|| "fdinfo omits flags".to_string())?;
    let flags = u64::from_str_radix(raw, 8)
        .map_err(|error| format!("parse octal fd flags {raw:?}: {error}"))?;
    Ok((flags, raw.to_string()))
}

fn prove_named_watcher_fd(
    pid: u32,
    proc_path: &Path,
    fd: &OsStr,
    targets: &[PathBuf],
    watcher_name: &str,
    executable: &str,
) -> Result<Option<String>, String> {
    let fd_path = proc_path.join("fd").join(fd);
    let mut last_error = "descriptor evidence changed during proof".to_string();
    for _ in 0..4 {
        let first_link = match fs::read_link(&fd_path) {
            Ok(link) if link_inside_target(&link, targets) => link,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                last_error = format!("read descriptor target: {error}");
                continue;
            }
        };
        let start_ticks_before = match process_start_ticks(proc_path) {
            Ok(start_ticks) => start_ticks,
            Err(error) => {
                last_error = format!("process identity unverifiable: {error}");
                continue;
            }
        };
        match named_read_only_watcher(proc_path) {
            Ok(Some((current_name, current_executable)))
                if current_name == watcher_name && current_executable == executable => {}
            Ok(_) => {
                return Err(format!(
                    "pid {pid}@{start_ticks_before} fd/{}={} no longer has exact named-watcher identity",
                    fd.to_string_lossy(),
                    first_link.display()
                ));
            }
            Err(error) => {
                last_error = format!("named-watcher identity unverifiable: {error}");
                continue;
            }
        }
        let (flags, raw_flags) = match fd_flags(proc_path, fd) {
            Ok(flags) => flags,
            Err(error) => {
                last_error = format!("descriptor access unverifiable: {error}");
                continue;
            }
        };
        let second_link = fs::read_link(&fd_path);
        let start_ticks_after = process_start_ticks(proc_path);
        if second_link.ok().as_ref() != Some(&first_link)
            || start_ticks_after.ok().as_ref() != Some(&start_ticks_before)
        {
            last_error = "descriptor or process identity changed during proof".to_string();
            continue;
        }
        let access_mode = flags & libc::O_ACCMODE as u64;
        let append = flags & libc::O_APPEND as u64 != 0;
        if access_mode != libc::O_RDONLY as u64 || append {
            return Err(format!(
                "pid {pid}@{start_ticks_before} fd/{}={} named watcher {watcher_name} has writable flags=0{raw_flags}",
                fd.to_string_lossy(),
                first_link.display()
            ));
        }
        return Ok(Some(format!(
            "pid {pid}@{start_ticks_before} watcher={watcher_name} exe={executable} fd/{}={} flags=0{raw_flags}; reason=exact named watcher with stable non-writable descriptor",
            fd.to_string_lossy(),
            first_link.display()
        )));
    }

    match fs::read_link(&fd_path) {
        Ok(link) if link_inside_target(&link, targets) => Err(format!(
            "pid {pid} fd/{}={} {watcher_name} remained live but unverifiable after retries: {last_error}",
            fd.to_string_lossy(),
            link.display()
        )),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "pid {pid} fd/{} {watcher_name} final descriptor state unreadable: {error}; prior evidence: {last_error}",
            fd.to_string_lossy()
        )),
    }
}

fn inspect_process_fd_links(
    pid: u32,
    proc_path: &Path,
    targets: &[PathBuf],
    users: &mut Vec<String>,
    tolerated: &mut Vec<String>,
) {
    let Ok(entries) = fs::read_dir(proc_path.join("fd")) else {
        return;
    };
    for entry in entries.flatten() {
        let fd = entry.file_name();
        let first_link = match fs::read_link(entry.path()) {
            Ok(link) if link_inside_target(&link, targets) => link,
            _ => continue,
        };
        let watcher = match named_read_only_watcher(proc_path) {
            Ok(watcher) => watcher,
            Err(error) => {
                users.push(format!(
                    "pid {pid} fd/{}={} named-watcher identity unverifiable: {error}",
                    fd.to_string_lossy(),
                    first_link.display()
                ));
                continue;
            }
        };
        let Some((watcher_name, executable)) = watcher else {
            users.push(format!(
                "pid {pid} fd/{}={}",
                fd.to_string_lossy(),
                first_link.display()
            ));
            continue;
        };
        match prove_named_watcher_fd(pid, proc_path, &fd, targets, watcher_name, executable) {
            Ok(Some(reference)) => tolerated.push(reference),
            Ok(None) => {}
            Err(error) => users.push(error),
        }
    }
}

fn inspect_process_maps(pid: u32, proc_path: &Path, targets: &[PathBuf], users: &mut Vec<String>) {
    if let Ok(text) = fs::read_to_string(proc_path.join("maps")) {
        for line in text.lines() {
            let mapped = line
                .split_whitespace()
                .skip(5)
                .collect::<Vec<_>>()
                .join(" ");
            if mapped.starts_with('/') && link_inside_target(Path::new(&mapped), targets) {
                users.push(format!("pid {pid} map={mapped}"));
            }
        }
    }
}

/// Invoke the single semantic verifier for the Podman ownership authority.
/// Its release-only command independently confirms that this process still
/// holds the lifecycle fence before it enumerates every container and mount.
fn container_release_audit(
    root: &Path,
    fence_token: &str,
    owners: &BTreeSet<String>,
    targets: &[PathBuf],
) -> Result<Value, String> {
    let tool = root.join("scripts/agent-podman.rs");
    if !tool.is_file() {
        return Err(format!(
            "canonical container authority is missing: {}",
            tool.display()
        ));
    }
    let mut command = Command::new(&tool);
    command
        .arg("release-audit")
        .args(["--fence-token", fence_token]);
    for owner in owners {
        command.args(["--owner", owner]);
    }
    for target in targets {
        command.arg("--target").arg(target);
    }
    command.arg("--json").env(
        "DEV_HERMIT_CONTAINER_STATE",
        root.join("ignored/ci-hub/agent-containers.json"),
    );
    if let Some(test_podman) = restricted_fixture_path(root, "HERMIT_RELEASE_TEST_PODMAN_BIN")? {
        command.env("AGENT_PODMAN_BIN", test_podman);
    } else {
        // A caller's ambient test override is not production engine authority.
        command.env_remove("AGENT_PODMAN_BIN");
    }
    let output = command
        .output()
        .map_err(|error| format!("canonical container release-audit unavailable: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "canonical container release-audit refused: {}{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let report: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!("canonical container release-audit returned invalid JSON: {error}")
    })?;
    if report["state"].as_str() != Some("ok") {
        return Err(
            "canonical container release-audit exited successfully without state=ok".to_string(),
        );
    }
    let observation = report["container_observation"].as_array().ok_or_else(|| {
        "canonical container release-audit omitted its complete container observation".to_string()
    })?;
    if report["inspected"].as_u64() != Some(observation.len() as u64) {
        return Err(format!(
            "canonical container release-audit observation count does not bind inspected={}",
            report["inspected"]
        ));
    }
    Ok(report["container_observation"].clone())
}

/// Refuse every readable same-UID reference into the target except an explicitly
/// named watcher whose exact executable/process identity and stable descriptor
/// flags prove this particular reference non-writable. Every tolerance is
/// printed; missing or changing evidence remains a refusal.
fn live_process_users(
    root: &Path,
    proc_root: &Path,
    cgroup_root: &Path,
    container_fence_token: &str,
    owners: &OwnerAuthorities,
    targets: &[PathBuf],
    allow_live_owner_identity: bool,
) -> Result<(Vec<String>, Value), String> {
    if !allow_live_owner_identity {
        verify_release_authority_absent(root, proc_root, cgroup_root, owners)?;
    }
    let container_observation =
        container_release_audit(root, container_fence_token, &owners.names, targets)?;
    let own_uid = fs::metadata("/proc/self")
        .map_err(|error| format!("could not inspect /proc/self: {error}"))?
        .uid();
    let mut users = Vec::new();
    let mut tolerated = Vec::new();
    for entry in fs::read_dir(proc_root)
        .map_err(|error| format!("could not enumerate {}: {error}", proc_root.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("could not enumerate proc entry: {error}")),
        };
        let name = entry.file_name();
        let Some(pid_text) = name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "could not inspect proc entry for pid {pid}: {error}"
                ))
            }
        };
        if metadata.uid() != own_uid {
            continue;
        }
        let proc_path = entry.path();
        if !allow_live_owner_identity
            && process_agent_name(&proc_path).is_some_and(|name| owners.names.contains(&name))
        {
            return Err(format!(
                "recorded owner process pid {pid} remains despite authoritative release-lease absence"
            ));
        }
        for kind in ["cwd", "exe", "root"] {
            inspect_process_link(pid, kind, &proc_path.join(kind), targets, &mut users);
        }
        inspect_process_fd_links(pid, &proc_path, targets, &mut users, &mut tolerated);
        inspect_process_maps(pid, &proc_path, targets, &mut users);
        inspect_process_directory_links(
            pid,
            "map_files",
            &proc_path.join("map_files"),
            targets,
            &mut users,
        );
    }
    users.sort();
    users.dedup();
    tolerated.sort();
    tolerated.dedup();
    for reference in tolerated {
        eprintln!("TOLERATED_READ_ONLY_WATCHER: {reference}");
    }
    Ok((users, container_observation))
}

fn snapshots(inspections: &[RepoInspection]) -> Vec<RepoSnapshot> {
    inspections
        .iter()
        .map(|inspection| inspection.snapshot.clone())
        .collect()
}

/// Rebind the full authorization and recursive repository identity after every
/// network call. Ordinary cleanup then delegates the last write race to Git's
/// own non-force removal boundary.
fn final_removal_boundary(
    root: &Path,
    proc_root: &Path,
    cgroup_root: &Path,
    container_fence_token: &str,
    owners: &OwnerAuthorities,
    slot: &str,
    expected_slot: &Value,
    expected_target: &CleanTarget,
    expected_repositories: &[RepoSnapshot],
    allow_dirty: bool,
    allow_live_owner_identity: bool,
) -> Result<(), String> {
    verify_registry_slot(root, slot)?;
    let current_state = load_state(root);
    let current_slot = current_state["slots"]
        .get(slot)
        .ok_or_else(|| format!("slot {slot} disappeared from state"))?;
    if current_slot != expected_slot {
        return Err(format!(
            "slot {slot} owner/task/status/branch/path authorization changed before removal"
        ));
    }
    let rebound = clean_targets_from_state(root, &current_state, slot)?
        .into_iter()
        .find(|target| target.label == expected_target.label)
        .ok_or_else(|| format!("{} target disappeared from state", expected_target.label))?;
    if rebound.path != expected_target.path || rebound.primary != expected_target.primary {
        return Err(format!(
            "{} target identity changed before removal",
            expected_target.label
        ));
    }

    let final_inspections = inspect_target_repositories(&rebound.path)?;
    if snapshots(&final_inspections) != expected_repositories {
        return Err(format!(
            "{} recursive repository identity/HEAD/origin changed before removal",
            expected_target.label
        ));
    }
    if !allow_dirty {
        for inspection in &final_inspections {
            if !inspection.status.is_empty() {
                return Err(format!(
                    "final cleanliness check found work in {}: {}",
                    inspection.snapshot.path.display(),
                    inspection.status.replace('\n', "; ")
                ));
            }
        }
    }

    let (users, _) = live_process_users(
        root,
        proc_root,
        cgroup_root,
        container_fence_token,
        owners,
        &[rebound.path.clone()],
        allow_live_owner_identity,
    )?;
    if !users.is_empty() {
        return Err(format!(
            "live process ownership below {}: {}",
            rebound.path.display(),
            users.join(", ")
        ));
    }

    Ok(())
}

fn linked_worktree_admin(target: &CleanTarget) -> Result<PathBuf, String> {
    let git_dir = git_inspect(
        &target.path,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    )?;
    let common_dir = git_inspect(
        &target.path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let git_dir = fs::canonicalize(&git_dir)
        .map_err(|error| format!("could not canonicalize worktree git-dir: {error}"))?;
    let common_dir = fs::canonicalize(&common_dir)
        .map_err(|error| format!("could not canonicalize common git-dir: {error}"))?;
    let primary_common_dir = git_inspect(
        &target.primary,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let primary_common_dir = fs::canonicalize(&primary_common_dir)
        .map_err(|error| format!("could not canonicalize primary common git-dir: {error}"))?;
    if common_dir != primary_common_dir {
        return Err(format!(
            "{} child common-dir {} does not match primary {}",
            target.path.display(),
            common_dir.display(),
            primary_common_dir.display()
        ));
    }
    let worktrees_dir = fs::canonicalize(common_dir.join("worktrees"))
        .map_err(|error| format!("could not canonicalize worktree registry: {error}"))?;
    if git_dir.parent() != Some(worktrees_dir.as_path()) {
        return Err(format!(
            "{} git-dir {} is not an exact linked-worktree admin entry",
            target.path.display(),
            git_dir.display()
        ));
    }
    Ok(git_dir)
}

#[derive(Clone, Debug)]
struct SubmoduleAdminPaths {
    modules: PathBuf,
    quarantine: PathBuf,
    force_fence_quarantine: PathBuf,
    in_progress: PathBuf,
    path_fence: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PathFenceMethod {
    GitWorktreeMove,
    AtomicRenameRepair,
}

#[derive(Debug)]
struct PathFenceResult {
    paths: SubmoduleAdminPaths,
    deinitialized_submodule_fallback: bool,
}

fn submodule_admin_paths(target: &CleanTarget) -> Result<SubmoduleAdminPaths, String> {
    let git_dir = linked_worktree_admin(target)?;
    Ok(SubmoduleAdminPaths {
        modules: git_dir.join("modules"),
        quarantine: git_dir.join("modules.release-worktree"),
        force_fence_quarantine: git_dir.join("modules.release-path-fence"),
        in_progress: git_dir.join("release-worktree.in-progress"),
        path_fence: git_dir.join("release-worktree.path-fence.json"),
    })
}

fn tracked_gitlink_paths(target: &Path) -> Result<Vec<PathBuf>, String> {
    let output = git_output(target, &["ls-files", "--stage", "-z"])?;
    if !output.status.success() {
        return Err(format!(
            "could not enumerate tracked gitlinks below {} (exit {}): {}",
            target.display(),
            output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut paths = Vec::new();
    for record in output.stdout.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            return Err(format!(
                "malformed index record while enumerating gitlinks below {}",
                target.display()
            ));
        };
        let header = std::str::from_utf8(&record[..tab]).map_err(|_| {
            format!(
                "non-UTF8 index metadata while enumerating gitlinks below {}",
                target.display()
            )
        })?;
        let fields: Vec<&str> = header.split_ascii_whitespace().collect();
        if fields.len() != 3 {
            return Err(format!(
                "malformed index metadata while enumerating gitlinks below {}",
                target.display()
            ));
        }
        if fields[0] != "160000" {
            continue;
        }
        if fields[2] != "0" {
            return Err(format!(
                "unmerged gitlink stage {} below {}; refusing fallback",
                fields[2],
                target.display()
            ));
        }
        let path = std::str::from_utf8(&record[tab + 1..]).map_err(|_| {
            format!(
                "tracked gitlink below {} has a non-UTF8 path",
                target.display()
            )
        })?;
        let relative = PathBuf::from(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(format!(
                "tracked gitlink path {path:?} below {} is not normalized",
                target.display()
            ));
        }
        paths.push(relative);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Prove the narrow state for which Git's linked-worktree move restriction is
/// an implementation obstacle rather than evidence of live nested work.
fn verify_fully_deinitialized_submodules(
    target: &CleanTarget,
    paths: &SubmoduleAdminPaths,
) -> Result<(), String> {
    let repositories = initialized_repositories(&target.path)?;
    if repositories.len() != 1 || repositories[0] != target.path {
        return Err(format!(
            "submodule move fallback requires every nested repository to be deinitialized; found {} initialized repository path(s)",
            repositories.len().saturating_sub(1)
        ));
    }
    let status = git_inspect(
        &target.path,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !status.is_empty() {
        return Err(format!(
            "submodule move fallback requires a clean outer worktree: {}",
            status.replace('\n', "; ")
        ));
    }
    let gitlinks = tracked_gitlink_paths(&target.path)?;
    if gitlinks.is_empty() {
        return Err(format!(
            "Git reported a submodule move refusal for {}, but its index has no gitlinks",
            target.path.display()
        ));
    }
    let submodule_status = git_inspect(&target.path, &["submodule", "status", "--recursive"])?;
    let status_lines: Vec<&str> = submodule_status.lines().collect();
    if status_lines.len() != gitlinks.len()
        || status_lines
            .iter()
            .any(|line| !line.as_bytes().first().is_some_and(|byte| *byte == b'-'))
    {
        return Err(format!(
            "submodule move fallback requires {} fully deinitialized gitlink(s); observed status {:?}",
            gitlinks.len(),
            status_lines
        ));
    }
    for relative in gitlinks {
        let path = target.path.join(&relative);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                let mut entries = fs::read_dir(&path).map_err(|error| {
                    format!(
                        "could not inspect deinitialized submodule path {}: {error}",
                        path.display()
                    )
                })?;
                if entries
                    .next()
                    .transpose()
                    .map_err(|error| {
                        format!(
                            "could not inspect deinitialized submodule path {}: {error}",
                            path.display()
                        )
                    })?
                    .is_some()
                {
                    return Err(format!(
                        "deinitialized submodule path {} is not empty; refusing fallback",
                        path.display()
                    ));
                }
            }
            Ok(_) => {
                return Err(format!(
                    "deinitialized submodule path {} is not an exact empty directory",
                    path.display()
                ))
            }
            Err(error) => {
                return Err(format!(
                    "could not inspect deinitialized submodule path {}: {error}",
                    path.display()
                ))
            }
        }
    }
    match fs::symlink_metadata(&paths.modules) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "deinitialized submodule admin {} is not an exact directory",
                paths.modules.display()
            ))
        }
        Err(error) => {
            return Err(format!(
                "deinitialized submodule admin {} is unavailable: {error}",
                paths.modules.display()
            ))
        }
    }
    for path in [
        &paths.in_progress,
        &paths.quarantine,
        &paths.force_fence_quarantine,
    ] {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(format!(
                    "deinitialized submodule admin conflicts with transaction artifact {}",
                    path.display()
                ))
            }
            Err(error) => {
                return Err(format!(
                    "could not inspect deinitialized submodule admin artifact {}: {error}",
                    path.display()
                ))
            }
        }
    }
    Ok(())
}

fn worktree_registration_counts(
    primary: &Path,
    original: &Path,
    fenced: &Path,
) -> Result<(usize, usize), String> {
    let listing = git_inspect(primary, &["worktree", "list", "--porcelain"])?;
    let mut original_count = 0;
    let mut fenced_count = 0;
    for candidate in listing
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
    {
        let candidate = Path::new(candidate);
        if candidate == original {
            original_count += 1;
        }
        if candidate == fenced {
            fenced_count += 1;
        }
    }
    Ok((original_count, fenced_count))
}

fn require_worktree_registrations(
    target: &CleanTarget,
    fenced: &Path,
    expected: (usize, usize),
    boundary: &str,
) -> Result<(), String> {
    let observed = worktree_registration_counts(&target.primary, &target.path, fenced)?;
    if observed != expected {
        return Err(format!(
            "{boundary} worktree registration mismatch for {}: original={} fenced={} expected={}/{}",
            target.label, observed.0, observed.1, expected.0, expected.1
        ));
    }
    Ok(())
}

fn require_same_device_for_rename(from: &Path, to: &Path) -> Result<(), String> {
    let source = fs::symlink_metadata(from)
        .map_err(|error| format!("inspect rename source {}: {error}", from.display()))?;
    if !source.is_dir() || source.file_type().is_symlink() {
        return Err(format!(
            "atomic path fence source {} is not an exact directory",
            from.display()
        ));
    }
    let parent = to.parent().ok_or_else(|| {
        format!(
            "atomic path fence destination {} has no parent",
            to.display()
        )
    })?;
    let destination_parent = fs::symlink_metadata(parent).map_err(|error| {
        format!(
            "inspect atomic path fence destination parent {}: {error}",
            parent.display()
        )
    })?;
    if source.dev() != destination_parent.dev() {
        return Err(format!(
            "atomic path fence refuses cross-device rename {} -> {} (device {} != {})",
            from.display(),
            to.display(),
            source.dev(),
            destination_parent.dev()
        ));
    }
    Ok(())
}

fn verify_fenced_path(path: &Path) -> Result<(), String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("canonicalize fenced target {}: {error}", path.display()))?;
    if canonical != path {
        return Err(format!(
            "fenced target {} resolves to unexpected {}",
            path.display(),
            canonical.display()
        ));
    }
    Ok(())
}

/// Atomically rename without replacing any concurrently created destination.
/// Unsupported kernels/filesystems fail closed and retain the worktree.
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "source path contains NUL"))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "target path contains NUL"))?;
    // SAFETY: both arguments are live NUL-terminated C strings for this call;
    // AT_FDCWD makes each absolute path independent of mutable directory fds.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A crash after recursive proof, deinitialization, or the atomic admin rename
/// must never turn the previous run's proof into implicit authorization for a
/// retry. Recovery is an explicit coordinator action; ordinary cleanup and
/// `--force` both retain the slot while either transaction artifact exists.
fn ensure_no_submodule_cleanup_artifacts(target: &CleanTarget) -> Result<(), String> {
    let paths = submodule_admin_paths(target)?;
    let mut unfinished = Vec::new();
    for path in [
        &paths.in_progress,
        &paths.quarantine,
        &paths.force_fence_quarantine,
        &paths.path_fence,
    ] {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Ok(_) => unfinished.push(path.display().to_string()),
            Err(error) => {
                return Err(format!(
                    "could not inspect possible submodule-cleanup artifact {}: {error}",
                    path.display()
                ))
            }
        }
    }
    if unfinished.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unfinished submodule cleanup artifact(s) {}; retain slot for recovery",
            unfinished.join(", ")
        ))
    }
}

/// Restore an interrupted normal transaction to a fully initialized state.
/// The marker remains authoritative until recursive initialization succeeds;
/// the caller then repeats cleanliness, branch durability, and process proofs.
fn recover_submodule_cleanup(target: &CleanTarget) -> Result<bool, String> {
    let paths = submodule_admin_paths(target)?;
    match fs::symlink_metadata(&paths.in_progress) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return match fs::symlink_metadata(&paths.quarantine) {
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
                Ok(_) => Err(format!(
                    "submodule-admin quarantine {} has no recovery marker",
                    paths.quarantine.display()
                )),
                Err(error) => Err(format!(
                    "could not inspect submodule-admin quarantine {}: {error}",
                    paths.quarantine.display()
                )),
            }
        }
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "submodule-cleanup marker {} is not an exact file",
                paths.in_progress.display()
            ))
        }
        Err(error) => {
            return Err(format!(
                "could not inspect submodule-cleanup marker {}: {error}",
                paths.in_progress.display()
            ))
        }
    }

    match fs::symlink_metadata(&paths.quarantine) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            match fs::symlink_metadata(&paths.modules) {
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(format!(
                        "both submodule admin {} and quarantine {} exist",
                        paths.modules.display(),
                        paths.quarantine.display()
                    ))
                }
                Err(error) => {
                    return Err(format!(
                        "could not inspect submodule admin {}: {error}",
                        paths.modules.display()
                    ))
                }
            }
            restore_submodule_admin(&paths)?;
        }
        Ok(_) => {
            return Err(format!(
                "submodule-admin quarantine {} is not an exact directory",
                paths.quarantine.display()
            ))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "could not inspect submodule-admin quarantine {}: {error}",
                paths.quarantine.display()
            ))
        }
    }
    match fs::symlink_metadata(&paths.modules) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "recoverable submodule admin {} is not an exact directory",
                paths.modules.display()
            ))
        }
        Err(error) => {
            return Err(format!(
                "recoverable submodule admin {} is unavailable: {error}",
                paths.modules.display()
            ))
        }
    }

    proxy_git_inspect(
        &target.path,
        &["submodule", "update", "--init", "--recursive"],
    )
    .map_err(|error| format!("submodule cleanup recovery reinitialization failed: {error}"))?;
    let repositories = initialized_repositories(&target.path)?;
    if repositories.len() <= 1 {
        return Err(format!(
            "submodule cleanup recovery initialized no nested repositories below {}",
            target.path.display()
        ));
    }
    fs::remove_file(&paths.in_progress).map_err(|error| {
        format!(
            "could not clear recovered submodule-cleanup marker {}: {error}",
            paths.in_progress.display()
        )
    })?;
    eprintln!(
        "✓ recovered interrupted submodule cleanup for {}; recursive proofs will be repeated",
        target.path.display()
    );
    Ok(true)
}

/// Mark the transaction before deinitialization changes what the next process
/// can enumerate. Any later error intentionally leaves this marker in place.
fn create_submodule_cleanup_marker(paths: &SubmoduleAdminPaths) -> Result<(), String> {
    let mut marker = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&paths.in_progress)
        .map_err(|error| {
            format!(
                "could not create submodule-cleanup marker {}: {error}",
                paths.in_progress.display()
            )
        })?;
    marker
        .write_all(b"release-worktree submodule cleanup in progress\n")
        .and_then(|()| marker.sync_all())
        .map_err(|error| {
            format!(
                "could not persist submodule-cleanup marker {}: {error}; retain slot for recovery",
                paths.in_progress.display()
            )
        })?;
    fs::File::open(
        paths
            .in_progress
            .parent()
            .expect("submodule-cleanup marker has an admin parent"),
    )
    .and_then(|directory| directory.sync_all())
    .map_err(|error| {
        format!(
            "could not persist submodule-cleanup directory entry {}: {error}; retain slot for recovery",
            paths.in_progress.display()
        )
    })?;
    Ok(())
}

fn begin_submodule_cleanup(target: &CleanTarget) -> Result<SubmoduleAdminPaths, String> {
    ensure_no_submodule_cleanup_artifacts(target)?;
    let paths = submodule_admin_paths(target)?;
    create_submodule_cleanup_marker(&paths)?;
    Ok(paths)
}

/// The atomic path-fence fallback is entered only after Git itself reports its
/// exact "contains submodules" move refusal. At that point the target has
/// already been proved fully deinitialized and repaired at the fenced path.
/// Create the same crash-recoverable admin transaction as the initialized
/// path, while permitting the path-fence marker that causally authorized it.
fn begin_deinitialized_submodule_cleanup(
    paths: &SubmoduleAdminPaths,
    original: &Path,
    fenced: &Path,
) -> Result<(), String> {
    for path in [
        &paths.in_progress,
        &paths.quarantine,
        &paths.force_fence_quarantine,
    ] {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(format!(
                    "deinitialized-submodule fallback found conflicting admin artifact {}",
                    path.display()
                ))
            }
            Err(error) => {
                return Err(format!(
                    "could not inspect deinitialized-submodule admin artifact {}: {error}",
                    path.display()
                ))
            }
        }
    }
    validate_path_fence_marker(paths, original, fenced)?;
    create_submodule_cleanup_marker(paths)
}

fn maybe_inject_fixture_crash(marker: Option<&Path>, evidence: &str) -> Result<(), String> {
    let Some(marker) = marker else {
        return Ok(());
    };
    fs::write(marker, format!("{evidence}\n")).map_err(|error| {
        format!(
            "could not write injected crash evidence {}: {error}",
            marker.display()
        )
    })?;
    exit(86);
}

fn quarantine_submodule_admin(paths: &SubmoduleAdminPaths) -> Result<(), String> {
    let metadata = fs::symlink_metadata(&paths.modules).map_err(|error| {
        format!(
            "initialized submodule admin {} is unavailable: {error}",
            paths.modules.display()
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "submodule admin {} is not an exact directory",
            paths.modules.display()
        ));
    }
    rename_no_replace(&paths.modules, &paths.quarantine).map_err(|error| {
        format!(
            "could not quarantine submodule admin {}: {error}",
            paths.modules.display()
        )
    })
}

fn restore_submodule_admin(paths: &SubmoduleAdminPaths) -> Result<(), String> {
    rename_no_replace(&paths.quarantine, &paths.modules).map_err(|error| {
        format!(
            "could not restore submodule admin {} from {}: {error}",
            paths.modules.display(),
            paths.quarantine.display()
        )
    })
}

fn quarantine_force_fence_admin(paths: &SubmoduleAdminPaths) -> Result<(), String> {
    rename_no_replace(&paths.modules, &paths.force_fence_quarantine).map_err(|error| {
        format!(
            "could not quarantine force-only path-fence admin {}: {error}",
            paths.modules.display()
        )
    })
}

fn restore_force_fence_admin(paths: &SubmoduleAdminPaths) -> Result<bool, String> {
    match fs::symlink_metadata(&paths.force_fence_quarantine) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if paths.modules.exists() {
                return Err(format!(
                    "both submodule admin {} and force-fence quarantine {} exist",
                    paths.modules.display(),
                    paths.force_fence_quarantine.display()
                ));
            }
            rename_no_replace(&paths.force_fence_quarantine, &paths.modules).map_err(|error| {
                format!(
                    "could not restore force-fence admin {}: {error}",
                    paths.force_fence_quarantine.display()
                )
            })?;
            Ok(true)
        }
        Ok(_) => Err(format!(
            "force-fence quarantine {} is not an exact directory",
            paths.force_fence_quarantine.display()
        )),
        Err(error) => Err(format!(
            "inspect force-fence quarantine {}: {error}",
            paths.force_fence_quarantine.display()
        )),
    }
}

fn secure_transaction_nonce() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("read coordinator-revocation randomness: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn arm_coordinator_lease_revocation(
    root: &Path,
    state: &mut Value,
    slot: &str,
    crash_marker: Option<&Path>,
) -> Result<(), String> {
    let slot_state = &state["slots"][slot];
    if slot_state.get("coordinator_lease_revocation").is_some() {
        return Err("coordinator lease revocation is already journaled".to_string());
    }
    let lease_value = slot_state
        .get("coordinator_lease")
        .ok_or_else(|| "slot has no Codex coordinator lease to revoke".to_string())?;
    let lease = worktree_registry::SentinelLease::from_value(lease_value)?;
    validate_sentinel_slot_binding(root, slot, &lease)?;
    // This first exact live check prevents an already-dead/restarted unit from
    // becoming release authority. The helper repeats the check immediately
    // before its exact stop operation after the durable journal is written.
    verify_codex_sentinel_live(root, &lease)?;
    state["slots"][slot]["status"] = json!("revoking-coordinator-lease");
    state["slots"][slot]["coordinator_lease_revocation"] = json!({
        "schema_version": 1,
        "source": "codex-systemd-sentinel-v1",
        "phase": "stop-authorized",
        "transaction_nonce": secure_transaction_nonce()?,
        "lease": lease.to_value()?,
        "authorized_at": now_iso(),
    });
    state["slots"][slot]["updated"] = json!(now_iso());
    save_state(root, state);
    maybe_inject_fixture_crash(
        crash_marker,
        "post-coordinator-revocation-journal crash injected",
    )?;
    regen_active_md(root, state);
    Ok(())
}

fn complete_coordinator_lease_revocation(
    root: &Path,
    state: &mut Value,
    slot: &str,
    recover: bool,
    post_stop_crash_marker: Option<&Path>,
) -> Result<bool, String> {
    let journal = state["slots"][slot]
        .get("coordinator_lease_revocation")
        .cloned()
        .ok_or_else(|| "slot has no coordinator lease revocation journal".to_string())?;
    let lease = worktree_registry::SentinelLease::from_value(&journal["lease"])?;
    validate_sentinel_slot_binding(root, slot, &lease)?;
    match journal["phase"].as_str() {
        Some("stop-authorized") => {
            let proof = revoke_codex_sentinel(root, &lease, recover)?;
            maybe_inject_fixture_crash(
                post_stop_crash_marker,
                "post-coordinator-sentinel-stop crash injected",
            )?;
            state["slots"][slot]["coordinator_lease_revocation"]["phase"] = json!("revoked");
            state["slots"][slot]["coordinator_lease_revocation"]["proof"] = proof;
            state["slots"][slot]["coordinator_lease_revocation"]["revoked_at"] = json!(now_iso());
            state["slots"][slot]["status"] = json!("owner-lease-revoked");
            state["slots"][slot]["updated"] = json!(now_iso());
            save_state(root, state);
            regen_active_md(root, state);
            Ok(true)
        }
        Some("revoked") => {
            prove_codex_sentinel_revoked(root, &lease)?;
            Ok(false)
        }
        other => Err(format!(
            "coordinator lease revocation has invalid phase {other:?}"
        )),
    }
}

fn arm_release_journal(
    root: &Path,
    state: &mut Value,
    slot: &str,
    target: &CleanTarget,
    crash_marker: Option<&Path>,
) -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let fenced = target
        .path
        .parent()
        .expect("target has slot parent")
        .join(format!(
            ".{}.release-worktree-{}-{nonce}",
            target.label,
            std::process::id()
        ));
    state["slots"][slot]["status"] = json!("releasing");
    state["slots"][slot]["release_journal"] = json!({
        "schema_version": 1,
        "label": target.label,
        "original": target.path.to_string_lossy(),
        "fenced": fenced.to_string_lossy(),
        "phase": "armed",
    });
    state["slots"][slot]["updated"] = json!(now_iso());
    save_state(root, state);
    maybe_inject_fixture_crash(crash_marker, "post-journal-arm state/ACTIVE split injected")?;
    regen_active_md(root, state);
    Ok(fenced)
}

fn mark_git_removal_complete(root: &Path, state: &mut Value, slot: &str) {
    state["slots"][slot]["release_journal"]["phase"] = json!("git-removal-complete");
    state["slots"][slot]["updated"] = json!(now_iso());
    save_state(root, state);
    regen_active_md(root, state);
}

fn clear_release_journal(
    root: &Path,
    state: &mut Value,
    slot: &str,
    crash_marker: Option<&Path>,
) -> Result<(), String> {
    let has_lease = state["slots"][slot]
        .get("owner_sidecar")
        .and_then(|sidecar| sidecar.get("lease"))
        .is_some_and(|lease| !lease.is_null());
    let lease_state = if has_lease {
        Some(slot_lease_view(slot, &state["slots"][slot])?.state)
    } else {
        None
    };
    state["slots"][slot]["status"] = json!(match lease_state {
        Some(worktree_registry::OwnerLeaseState::Quarantined) => "lease-quarantined",
        Some(worktree_registry::OwnerLeaseState::Draining) => "release-requested",
        _ if state["slots"][slot]
            .get("coordinator_lease_revocation")
            .is_some() =>
        {
            "owner-lease-revoked"
        }
        _ => "active",
    });
    state["slots"][slot]["updated"] = json!(now_iso());
    state["slots"][slot]
        .as_object_mut()
        .expect("slot record is an object")
        .remove("release_journal");
    save_state(root, state);
    maybe_inject_fixture_crash(
        crash_marker,
        "post-journal-clear state/ACTIVE split injected",
    )?;
    regen_active_md(root, state);
    Ok(())
}

fn validate_path_fence_marker(
    paths: &SubmoduleAdminPaths,
    original: &Path,
    fenced: &Path,
) -> Result<PathFenceMethod, String> {
    let raw = fs::read_to_string(&paths.path_fence).map_err(|error| {
        format!(
            "read path-fence marker {}: {error}",
            paths.path_fence.display()
        )
    })?;
    let marker: Value =
        serde_json::from_str(&raw).map_err(|error| format!("parse path-fence marker: {error}"))?;
    if marker["original"].as_str() != original.to_str()
        || marker["fenced"].as_str() != fenced.to_str()
    {
        return Err(format!(
            "path-fence marker {} does not bind the exact original/fenced paths",
            paths.path_fence.display()
        ));
    }
    match marker["schema_version"].as_u64() {
        Some(1) => Ok(PathFenceMethod::GitWorktreeMove),
        Some(2)
            if marker["method"].as_str() == Some("atomic-rename-repair")
                && matches!(
                    marker["phase"].as_str(),
                    Some("prepared" | "renamed" | "repaired")
                )
                && marker["admin"].as_str() == paths.path_fence.parent().and_then(Path::to_str) =>
        {
            Ok(PathFenceMethod::AtomicRenameRepair)
        }
        _ => Err(format!(
            "path-fence marker {} has unsupported schema/method/phase/admin binding",
            paths.path_fence.display()
        )),
    }
}

fn persist_atomic_path_fence_marker(
    paths: &SubmoduleAdminPaths,
    original: &Path,
    fenced: &Path,
    phase: &str,
) -> Result<(), String> {
    let marker = json!({
        "schema_version": 2,
        "original": original.to_string_lossy(),
        "fenced": fenced.to_string_lossy(),
        "method": "atomic-rename-repair",
        "phase": phase,
        "admin": paths
            .path_fence
            .parent()
            .expect("path-fence marker has an admin parent")
            .to_string_lossy(),
    });
    let mut bytes = serde_json::to_vec(&marker)
        .map_err(|error| format!("serialize atomic path-fence marker: {error}"))?;
    bytes.push(b'\n');
    durable_replace(&paths.path_fence, &bytes)
        .map_err(|error| format!("persist atomic path-fence marker: {error}"))
}

fn git_failure_message(dir: &Path, args: &[&str], output: &Output) -> String {
    format!(
        "git -C {} {} failed (exit {}): {}",
        dir.display(),
        args.join(" "),
        output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".to_string()),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn is_exact_submodule_move_refusal(output: &Output) -> bool {
    !output.status.success()
        && String::from_utf8_lossy(&output.stderr).lines().any(|line| {
            line.trim() == "fatal: working trees containing submodules cannot be moved or removed"
        })
}

fn begin_atomic_path_fence(
    target: &CleanTarget,
    fenced: &Path,
    paths: &SubmoduleAdminPaths,
    crash_after_rename: Option<&Path>,
) -> Result<(), String> {
    verify_fully_deinitialized_submodules(target, paths)?;
    require_worktree_registrations(target, fenced, (1, 0), "pre-fallback")?;
    let expected_admin = linked_worktree_admin(target)?;
    if expected_admin != paths.path_fence.parent().unwrap() {
        return Err(format!(
            "atomic path fence admin mismatch for {}: {} != {}",
            target.path.display(),
            expected_admin.display(),
            paths.path_fence.parent().unwrap().display()
        ));
    }
    require_same_device_for_rename(&target.path, fenced)?;
    persist_atomic_path_fence_marker(paths, &target.path, fenced, "prepared")?;
    rename_no_replace(&target.path, fenced).map_err(|error| {
        format!(
            "atomic path fence rename {} -> {} failed: {error}",
            target.path.display(),
            fenced.display()
        )
    })?;
    persist_atomic_path_fence_marker(paths, &target.path, fenced, "renamed")?;
    if exact_path_present(&target.path)? || !exact_path_present(fenced)? {
        return Err(format!(
            "atomic path fence did not establish exact original/fenced presence for {}",
            target.label
        ));
    }
    verify_fenced_path(fenced)?;
    require_worktree_registrations(target, fenced, (1, 0), "post-rename/pre-repair")?;
    let moved_target = CleanTarget {
        path: fenced.to_path_buf(),
        ..target.clone()
    };
    let observed_admin = linked_worktree_admin(&moved_target)?;
    if observed_admin != expected_admin {
        return Err(format!(
            "atomic path fence changed linked-worktree admin identity: {} != {}",
            observed_admin.display(),
            expected_admin.display()
        ));
    }
    maybe_inject_fixture_crash(
        crash_after_rename,
        "post-atomic-path-fence-rename crash injected",
    )?;
    let fenced_text = fenced.to_string_lossy().into_owned();
    git_inspect(&target.primary, &["worktree", "repair", &fenced_text])?;
    require_worktree_registrations(target, fenced, (0, 1), "post-repair")?;
    let repaired_admin = linked_worktree_admin(&moved_target)?;
    if repaired_admin != expected_admin {
        return Err(format!(
            "git worktree repair rebound unexpected admin: {} != {}",
            repaired_admin.display(),
            expected_admin.display()
        ));
    }
    persist_atomic_path_fence_marker(paths, &target.path, fenced, "repaired")?;
    eprintln!(
        "✓ fenced {} through exact submodule-refusal atomic rename + verified git worktree repair",
        target.path.display()
    );
    Ok(())
}

fn begin_path_fence(
    target: &CleanTarget,
    fenced: &Path,
    allow_deinitialized_fallback: bool,
    crash_after_fallback_rename: Option<&Path>,
) -> Result<PathFenceResult, String> {
    match fs::symlink_metadata(fenced) {
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(format!(
                "path-fence destination already exists: {}",
                fenced.display()
            ))
        }
        Err(error) => {
            return Err(format!(
                "inspect path-fence destination {}: {error}",
                fenced.display()
            ))
        }
    }
    let paths = submodule_admin_paths(target)?;
    match fs::symlink_metadata(&paths.path_fence) {
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(format!(
                "unfinished path-fence marker {}; use --recover-submodule-cleanup",
                paths.path_fence.display()
            ))
        }
        Err(error) => return Err(format!("inspect path-fence marker: {error}")),
    }
    let marker = json!({
        "schema_version": 1,
        "original": target.path.to_string_lossy(),
        "fenced": fenced.to_string_lossy(),
    });
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&paths.path_fence)
        .map_err(|error| format!("create path-fence marker: {error}"))?;
    file.write_all(format!("{}\n", serde_json::to_string(&marker).unwrap()).as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("persist path-fence marker: {error}"))?;
    fs::File::open(
        paths
            .path_fence
            .parent()
            .expect("path-fence marker has an admin parent"),
    )
    .and_then(|directory| directory.sync_all())
    .map_err(|error| format!("persist path-fence directory entry: {error}"))?;

    let original = target.path.to_string_lossy().into_owned();
    let fenced_text = fenced.to_string_lossy().into_owned();
    let args = ["worktree", "move", &original, &fenced_text];
    let output = git_output(&target.primary, &args)?;
    let deinitialized_submodule_fallback = if output.status.success() {
        false
    } else if allow_deinitialized_fallback && is_exact_submodule_move_refusal(&output) {
        begin_atomic_path_fence(target, fenced, &paths, crash_after_fallback_rename)?;
        true
    } else {
        return Err(git_failure_message(&target.primary, &args, &output));
    };
    if target.path.exists() {
        return Err(format!(
            "canonical target {} still exists after path fence",
            target.path.display()
        ));
    }
    verify_fenced_path(fenced)?;
    validate_path_fence_marker(&paths, &target.path, fenced)?;
    Ok(PathFenceResult {
        paths,
        deinitialized_submodule_fallback,
    })
}

fn restore_atomic_path_fence(
    target: &CleanTarget,
    fenced: &Path,
    paths: &SubmoduleAdminPaths,
) -> Result<(), String> {
    if validate_path_fence_marker(paths, &target.path, fenced)?
        != PathFenceMethod::AtomicRenameRepair
    {
        return Err("atomic path-fence recovery received a non-atomic marker".to_string());
    }
    let expected_admin = paths
        .path_fence
        .parent()
        .expect("path-fence marker has an admin parent")
        .to_path_buf();
    let original_present = exact_path_present(&target.path)?;
    let fenced_present = exact_path_present(fenced)?;
    let registrations = worktree_registration_counts(&target.primary, &target.path, fenced)?;
    match (original_present, fenced_present) {
        (false, true) => {
            if registrations != (1, 0) && registrations != (0, 1) {
                return Err(format!(
                    "atomic path-fence recovery registration mismatch before restore: original={} fenced={}",
                    registrations.0, registrations.1
                ));
            }
            let fenced_target = CleanTarget {
                path: fenced.to_path_buf(),
                ..target.clone()
            };
            let observed_admin = linked_worktree_admin(&fenced_target)?;
            if observed_admin != expected_admin {
                return Err(format!(
                    "atomic path-fence recovery admin mismatch: {} != {}",
                    observed_admin.display(),
                    expected_admin.display()
                ));
            }
            require_same_device_for_rename(fenced, &target.path)?;
            rename_no_replace(fenced, &target.path).map_err(|error| {
                format!(
                    "atomic path-fence restore {} -> {} failed: {error}",
                    fenced.display(),
                    target.path.display()
                )
            })?;
            if registrations == (0, 1) {
                let original = target.path.to_string_lossy().into_owned();
                git_inspect(&target.primary, &["worktree", "repair", &original])?;
            }
        }
        (true, false) => {
            if registrations == (0, 1) {
                let original = target.path.to_string_lossy().into_owned();
                git_inspect(&target.primary, &["worktree", "repair", &original])?;
            } else if registrations != (1, 0) {
                return Err(format!(
                    "atomic path-fence recovery registration mismatch at canonical path: original={} fenced={}",
                    registrations.0, registrations.1
                ));
            }
        }
        (true, true) => {
            return Err(format!(
                "both canonical and fenced targets exist during atomic recovery: {}, {}",
                target.path.display(),
                fenced.display()
            ))
        }
        (false, false) => {
            return Err(format!(
                "both canonical and fenced targets are absent during atomic recovery: {}, {}",
                target.path.display(),
                fenced.display()
            ))
        }
    }
    if !exact_path_present(&target.path)? || exact_path_present(fenced)? {
        return Err("atomic path-fence recovery did not restore exact path presence".to_string());
    }
    verify_fenced_path(&target.path)?;
    require_worktree_registrations(target, fenced, (1, 0), "post-restore")?;
    let observed_admin = linked_worktree_admin(target)?;
    if observed_admin != expected_admin {
        return Err(format!(
            "atomic path-fence recovery rebound unexpected admin: {} != {}",
            observed_admin.display(),
            expected_admin.display()
        ));
    }
    fs::remove_file(&paths.path_fence).map_err(|error| {
        format!(
            "clear rolled-back path-fence marker {}: {error}",
            paths.path_fence.display()
        )
    })?;
    fs::File::open(&expected_admin)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("persist atomic path-fence marker removal: {error}"))?;
    Ok(())
}

fn rollback_path_fence(
    target: &CleanTarget,
    fenced: &Path,
    paths: &SubmoduleAdminPaths,
) -> Result<(), String> {
    match validate_path_fence_marker(paths, &target.path, fenced)? {
        PathFenceMethod::GitWorktreeMove => {
            let fenced_text = fenced.to_string_lossy().into_owned();
            let original = target.path.to_string_lossy().into_owned();
            git_inspect(
                &target.primary,
                &["worktree", "move", &fenced_text, &original],
            )?;
            fs::remove_file(&paths.path_fence).map_err(|error| {
                format!(
                    "clear rolled-back path-fence marker {}: {error}",
                    paths.path_fence.display()
                )
            })?;
            Ok(())
        }
        PathFenceMethod::AtomicRenameRepair => restore_atomic_path_fence(target, fenced, paths),
    }
}

fn journal_target(
    root: &Path,
    state: &Value,
    slot: &str,
) -> Result<Option<(CleanTarget, PathBuf)>, String> {
    let slot_state = state["slots"]
        .get(slot)
        .ok_or_else(|| format!("slot {slot} is not registered"))?;
    if slot_state.get("release_journal").is_none() {
        if slot_state["status"].as_str() == Some("releasing") {
            return Err(format!(
                "slot {slot} is releasing without a release_journal"
            ));
        }
        return Ok(None);
    }
    let journal = slot_state["release_journal"]
        .as_object()
        .ok_or_else(|| format!("slot {slot} is releasing without a readable release_journal"))?;
    if journal.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(format!(
            "slot {slot} release_journal has unsupported schema"
        ));
    }
    let label = match journal.get("label").and_then(Value::as_str) {
        Some("hermit") => "hermit",
        Some("reverie") => "reverie",
        Some("liteinst2") => "liteinst2",
        other => {
            return Err(format!(
                "slot {slot} release_journal has invalid label {other:?}"
            ))
        }
    };
    let branch_key = match label {
        "hermit" => "hermit_branch",
        "reverie" => "reverie_branch",
        "liteinst2" => "liteinst2_branch",
        _ => unreachable!(),
    };
    let expected_original = root.join("worktrees/slots").join(slot).join(label);
    let original = PathBuf::from(
        journal
            .get("original")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("slot {slot} release_journal has no original path"))?,
    );
    let fenced = PathBuf::from(
        journal
            .get("fenced")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("slot {slot} release_journal has no fenced path"))?,
    );
    let expected_prefix = format!(".{label}.release-worktree-");
    if original != expected_original
        || fenced.parent() != expected_original.parent()
        || !fenced
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&expected_prefix))
    {
        return Err(format!(
            "slot {slot} release_journal does not bind its exact canonical target"
        ));
    }
    Ok(Some((
        CleanTarget {
            label,
            branch_key,
            primary: root.join(label),
            path: original,
        },
        fenced,
    )))
}

enum PathFenceRecovery {
    None,
    Restored,
    RemovalCompleted(CleanTarget),
}

fn exact_path_present(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "inspect journal target {}: {error}",
            path.display()
        )),
    }
}

fn verify_absent_slot_recovery(root: &Path, slot: &str) -> Result<(), String> {
    let slot_dir = root.join("worktrees/slots").join(slot);
    if exact_path_present(&slot_dir)? {
        return Err(format!(
            "slot {slot} is absent from state but physical path {} remains",
            slot_dir.display()
        ));
    }
    for product in ["hermit", "reverie", "liteinst2"] {
        let primary = root.join(product);
        let listing = git_inspect(&primary, &["worktree", "list", "--porcelain"])?;
        if let Some(candidate) = listing
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(Path::new)
            .find(|candidate| candidate.starts_with(&slot_dir))
        {
            return Err(format!(
                "slot {slot} is absent from state but {product} still registers {}",
                candidate.display()
            ));
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("open {} for SHA-256: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read {} for SHA-256: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_runner_profile_dir(name: &OsStr) -> bool {
    name == OsStr::new(".dagrun") || name == OsStr::new(".safe-ci-dag-runner")
}

fn textual_evidence(relative: &Path) -> bool {
    let under_profiles = relative
        .components()
        .next()
        .is_some_and(|component| is_runner_profile_dir(component.as_os_str()));
    let extension = relative.extension().and_then(OsStr::to_str);
    relative == Path::new("target/validate-results.txt")
        || ((under_profiles
            || relative
                .components()
                .next()
                .is_some_and(|component| component.as_os_str() == "target"))
            && matches!(extension, Some("csv" | "json" | "jsonl" | "log" | "txt")))
}

fn recognized_generated_file(relative: &Path, metadata: &fs::Metadata) -> bool {
    let mut components = relative.components();
    let Some(Component::Normal(top)) = components.next() else {
        return false;
    };
    if is_runner_profile_dir(top) {
        return textual_evidence(relative);
    }
    if top != OsStr::new("target") {
        return false;
    }
    if textual_evidence(relative) {
        return true;
    }

    let Some(name) = relative.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    if matches!(
        name,
        "CACHEDIR.TAG"
            | ".cargo-lock"
            | ".cargo-artifact-lock"
            | ".rustc_info.json"
            | "cargo-timings.html"
            | "invoked.timestamp"
            | "root-output"
            | "stderr"
    ) {
        return true;
    }
    if [
        "bin-",
        "build-script-",
        "dep-",
        "lib-",
        "output-",
        "run-build-script-",
        "test-",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
    {
        return true;
    }
    if matches!(
        relative.extension().and_then(OsStr::to_str),
        Some(
            "a" | "bc"
                | "bin"
                | "crate"
                | "css"
                | "d"
                | "dll"
                | "dylib"
                | "exe"
                | "gcda"
                | "gcno"
                | "html"
                | "js"
                | "json"
                | "jsonl"
                | "lib"
                | "ll"
                | "lock"
                | "log"
                | "map"
                | "o"
                | "pdb"
                | "profdata"
                | "profraw"
                | "rlib"
                | "rmeta"
                | "so"
                | "stamp"
                | "svg"
                | "timestamp"
                | "txt"
                | "wasm"
                | "woff"
                | "woff2"
        )
    ) {
        return true;
    }

    let profile = components.next().and_then(|component| match component {
        Component::Normal(value) => value.to_str(),
        _ => None,
    });
    matches!(profile, Some("debug" | "release")) && metadata.mode() & 0o111 != 0
}

fn decode_mountinfo_path(field: &str, line_number: usize) -> Result<PathBuf, String> {
    let bytes = field.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 3 >= bytes.len()
            || !bytes[index + 1..=index + 3]
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'7'))
        {
            return Err(format!(
                "mountinfo line {line_number} has malformed escaped mount path"
            ));
        }
        let value = (bytes[index + 1] - b'0') * 64
            + (bytes[index + 2] - b'0') * 8
            + (bytes[index + 3] - b'0');
        decoded.push(value);
        index += 4;
    }
    let decoded = String::from_utf8(decoded)
        .map_err(|_| format!("mountinfo line {line_number} has non-UTF8 mount path"))?;
    Ok(PathBuf::from(decoded))
}

fn verify_no_nested_mounts(root: &Path, residue: &Path) -> Result<(), String> {
    let authority = restricted_fixture_path(root, "HERMIT_RELEASE_TEST_MOUNTINFO")?
        .unwrap_or_else(|| PathBuf::from("/proc/self/mountinfo"));
    let text = fs::read_to_string(&authority)
        .map_err(|error| format!("read mount authority {}: {error}", authority.display()))?;
    for (offset, line) in text.lines().enumerate() {
        let line_number = offset + 1;
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 7 || !fields.iter().any(|field| *field == "-") {
            return Err(format!("mountinfo line {line_number} is malformed"));
        }
        let mount = decode_mountinfo_path(fields[4], line_number)?;
        if mount == residue || mount.starts_with(residue) {
            return Err(format!(
                "nested mount {} remains inside orphan residue {}; refusing disposal",
                mount.display(),
                residue.display()
            ));
        }
    }
    Ok(())
}

fn walk_residue(
    base: &Path,
    directory: &Path,
    directories: &mut Vec<String>,
    files: &mut Vec<ResidueFile>,
) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .map_err(|error| format!("enumerate residue {}: {error}", directory.display()))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("enumerate residue entry: {error}"))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect residue {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "orphan residue contains symlink {}; refusing generated-residue disposal",
                path.display()
            ));
        }
        let relative = path
            .strip_prefix(base)
            .map_err(|_| format!("residue path escaped base: {}", path.display()))?;
        let relative_text = relative
            .to_str()
            .ok_or_else(|| format!("orphan residue has non-UTF8 path: {}", path.display()))?
            .to_string();
        if metadata.is_dir() {
            directories.push(relative_text);
            walk_residue(base, &path, directories, files)?;
        } else if metadata.is_file() {
            if !recognized_generated_file(relative, &metadata) {
                return Err(format!(
                    "orphan residue contains unrecognized generated file {}; preserve or classify it before retry",
                    path.display()
                ));
            }
            let copied = textual_evidence(relative);
            if copied && metadata.len() > 2 * 1024 * 1024 {
                return Err(format!(
                    "textual evidence {} exceeds 2 MiB; preserve it explicitly before retry",
                    path.display()
                ));
            }
            files.push(ResidueFile {
                relative: relative_text,
                bytes: metadata.len(),
                sha256: sha256_file(&path)?,
                copied,
                device: metadata.dev(),
                inode: metadata.ino(),
            });
        } else {
            return Err(format!(
                "orphan residue contains non-file/non-directory {}; refusing",
                path.display()
            ));
        }
    }
    Ok(())
}

fn residue_inventory(root: &Path, path: &Path) -> Result<ResidueInventory, String> {
    verify_no_nested_mounts(root, path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect orphan residue {}: {error}", path.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "orphan residue {} is not an exact directory",
            path.display()
        ));
    }
    let allowed: BTreeSet<&str> = [".dagrun", ".safe-ci-dag-runner", "target"]
        .into_iter()
        .collect();
    let mut top_entries: Vec<_> = fs::read_dir(path)
        .map_err(|error| format!("enumerate orphan residue {}: {error}", path.display()))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("enumerate orphan residue entry: {error}"))?;
    top_entries.sort_by_key(|entry| entry.file_name());
    if top_entries.is_empty() {
        return Err(format!(
            "orphan residue {} is empty; absence alone is not cleanup authority",
            path.display()
        ));
    }
    for entry in &top_entries {
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| format!("orphan residue has non-UTF8 top-level entry"))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("inspect orphan residue entry: {error}"))?;
        if !allowed.contains(name) || !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "orphan residue contains non-generated top-level entry {}; allowed entries are target/, .dagrun/, and .safe-ci-dag-runner/",
                entry.path().display()
            ));
        }
    }
    let mut directories = Vec::new();
    let mut files = Vec::new();
    walk_residue(path, path, &mut directories, &mut files)?;
    directories.sort();
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(ResidueInventory { directories, files })
}

fn inventory_json(inventory: &ResidueInventory) -> Value {
    json!({
        "directories": inventory.directories,
        "files": inventory.files.iter().map(|file| json!({
            "path": file.relative,
            "bytes": file.bytes,
            "sha256": file.sha256,
            "copied": file.copied,
        })).collect::<Vec<_>>(),
    })
}

fn copy_evidence_file(source: &Path, destination: &Path, expected: &str) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create evidence directory {}: {error}", parent.display()))?;
    }
    match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(format!(
                    "existing evidence destination {} is not an exact file",
                    destination.display()
                ));
            }
            if sha256_file(destination)? != expected {
                return Err(format!(
                    "existing evidence copy {} has the wrong SHA-256",
                    destination.display()
                ));
            }
            return Ok(());
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "inspect evidence destination {}: {error}",
                destination.display()
            ))
        }
    }
    let mut input = fs::File::open(source)
        .map_err(|error| format!("open evidence source {}: {error}", source.display()))?;
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(|error| format!("create evidence copy {}: {error}", destination.display()))?;
    io::copy(&mut input, &mut output)
        .and_then(|_| output.sync_all())
        .map_err(|error| format!("persist evidence copy {}: {error}", destination.display()))?;
    if sha256_file(destination)? != expected {
        return Err(format!(
            "evidence copy {} failed post-copy SHA-256 verification",
            destination.display()
        ));
    }
    Ok(())
}

fn evidence_directory(root: &Path, slot: &str, nonce: &str) -> Result<PathBuf, String> {
    let relative = PathBuf::from("ignored/worktree-recovery")
        .join(slot)
        .join(nonce);
    let directory = root.join(&relative);
    fs::create_dir_all(&directory).map_err(|error| {
        format!(
            "create orphan evidence directory {}: {error}",
            directory.display()
        )
    })?;
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("canonicalize workspace root: {error}"))?;
    let canonical = fs::canonicalize(&directory)
        .map_err(|error| format!("canonicalize orphan evidence directory: {error}"))?;
    let expected = canonical_root.join(relative);
    if canonical != expected {
        return Err(format!(
            "orphan evidence directory {} resolves to {}; refusing alias",
            expected.display(),
            canonical.display()
        ));
    }
    Ok(canonical)
}

fn build_or_verify_evidence(
    root: &Path,
    slot: &str,
    nonce: &str,
    snapshot: &Value,
    targets: &[OrphanTarget],
    sources: &[(PathBuf, ResidueInventory)],
) -> Result<(PathBuf, String, Value), String> {
    let directory = evidence_directory(root, slot, nonce)?;
    let mut target_rows = Vec::new();
    for (target, (source, inventory)) in targets.iter().zip(sources.iter()) {
        for file in &inventory.files {
            if file.copied {
                copy_evidence_file(
                    &source.join(&file.relative),
                    &directory.join(target.label).join(&file.relative),
                    &file.sha256,
                )?;
            }
        }
        target_rows.push(json!({
            "label": target.label,
            "original": target.original,
            "fenced": target.fenced,
            "inventory": inventory_json(inventory),
        }));
    }
    let manifest = json!({
        "schema_version": 1,
        "source": "coordinator-orphan-residue-v1",
        "slot": slot,
        "transaction_nonce": nonce,
        "slot_snapshot": snapshot,
        "targets": target_rows,
    });
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("serialize orphan evidence manifest: {error}"))?;
    let mut persisted = bytes.clone();
    persisted.push(b'\n');
    let path = directory.join("manifest.json");
    match fs::read(&path) {
        Ok(existing) if existing != persisted => {
            return Err(format!(
                "existing orphan evidence manifest {} does not match current residue",
                path.display()
            ))
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            durable_replace(&path, &persisted)
                .map_err(|error| format!("persist orphan evidence manifest: {error}"))?;
        }
        Err(error) => return Err(format!("read orphan evidence manifest: {error}")),
    }
    Ok((directory, sha256_bytes(&persisted), manifest))
}

fn verify_inventory_row(
    manifest: &Value,
    label: &str,
    inventory: &ResidueInventory,
) -> Result<(), String> {
    let rows = manifest["targets"]
        .as_array()
        .ok_or_else(|| "orphan evidence manifest has no targets".to_string())?;
    let row = rows
        .iter()
        .find(|row| row["label"].as_str() == Some(label))
        .ok_or_else(|| format!("orphan evidence manifest has no {label} target"))?;
    if row["inventory"] != inventory_json(inventory) {
        return Err(format!(
            "{label} residue changed after evidence capture; retaining fenced residue"
        ));
    }
    Ok(())
}

fn normalized_identity(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn orphan_owner_names(slot_state: &Value) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for owner in slot_state["agents"]
        .as_array()
        .ok_or_else(|| "orphan slot has no agents array".to_string())?
    {
        let name = owner["name"]
            .as_str()
            .ok_or_else(|| "orphan slot owner has no name".to_string())?;
        names.insert(name.to_string());
    }
    Ok(names)
}

fn verify_orphan_owners_absent(
    root: &Path,
    slot_state: &Value,
) -> Result<BTreeSet<String>, String> {
    let names = orphan_owner_names(slot_state)?;
    let agents = agent_snapshot(root)?;
    let panes = tmux_panes(root)?;
    for name in &names {
        if agents.get(name).is_some_and(|presence| presence.live) {
            return Err(format!(
                "recorded orphan owner '{name}' is still live in ORC"
            ));
        }
        if panes.iter().any(|pane| pane.window == *name) {
            return Err(format!(
                "recorded orphan owner '{name}' still has a tmux pane"
            ));
        }
        if agents
            .get(name)
            .and_then(|presence| presence.pane.as_deref())
            .is_some_and(|pane| panes.iter().any(|candidate| candidate.pane == pane))
        {
            return Err(format!(
                "recorded orphan owner '{name}' retains its recorded tmux pane"
            ));
        }
    }
    Ok(names)
}

fn verify_orphan_units_absent(
    root: &Path,
    owners: &BTreeSet<String>,
    targets: &[PathBuf],
) -> Result<(), String> {
    let rows: Vec<Value> =
        if let Some(path) = restricted_fixture_path(root, "HERMIT_RELEASE_TEST_SYSTEMD_UNITS")? {
            serde_json::from_slice(&fs::read(&path).map_err(|error| {
                format!("read test systemd authority {}: {error}", path.display())
            })?)
            .map_err(|error| format!("parse test systemd authority: {error}"))?
        } else {
            let mut rows = Vec::new();
            for user in [true, false] {
                let mut command = Command::new("systemctl");
                if user {
                    command.arg("--user");
                }
                let output = command
                    .args(["list-units", "--all", "--output=json", "--no-pager"])
                    .output()
                    .map_err(|error| format!("query systemd unit authority: {error}"))?;
                if !output.status.success() {
                    return Err(format!(
                        "systemd unit authority failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
                let listed: Vec<Value> = serde_json::from_slice(&output.stdout)
                    .map_err(|error| format!("parse systemd unit authority: {error}"))?;
                for mut row in listed {
                    row["scope"] = json!(if user { "user" } else { "system" });
                    rows.push(row);
                }
            }
            rows
        };
    let owner_tokens: Vec<String> = owners
        .iter()
        .map(|name| normalized_identity(name))
        .collect();
    for row in rows {
        let active = row["active"]
            .as_str()
            .ok_or_else(|| format!("systemd authority row has no active state: {row}"))?;
        if matches!(active, "inactive" | "failed") {
            continue;
        }
        let unit = row["unit"]
            .as_str()
            .filter(|unit| !unit.is_empty())
            .ok_or_else(|| format!("systemd authority row has no unit identity: {row}"))?;
        let mut details = row["details"].as_str().unwrap_or("").to_string();
        let unit_token = normalized_identity(unit);
        let name_match = owner_tokens
            .iter()
            .any(|owner| !owner.is_empty() && unit_token.contains(owner));
        if details.is_empty() && (unit.ends_with(".service") || unit.ends_with(".scope")) {
            let mut command = Command::new("systemctl");
            if row["scope"].as_str() == Some("user") {
                command.arg("--user");
            }
            let output = command
                .args([
                    "show",
                    unit,
                    "--no-pager",
                    "--property=Id,ActiveState,SubState,WorkingDirectory,ExecStart",
                ])
                .output()
                .map_err(|error| format!("inspect live systemd unit {unit}: {error}"))?;
            if !output.status.success() {
                return Err(format!("could not inspect live systemd unit {unit}"));
            }
            details = String::from_utf8_lossy(&output.stdout).to_string();
        }
        let target_match = targets
            .iter()
            .any(|target| details.contains(target.to_string_lossy().as_ref()));
        if name_match || target_match {
            return Err(format!(
                "live systemd unit {unit} remains associated with orphan owner/path"
            ));
        }
    }
    Ok(())
}

fn orphan_process_users(
    proc_root: &Path,
    owners: &BTreeSet<String>,
    targets: &[PathBuf],
) -> Result<Vec<String>, String> {
    let own_uid = fs::metadata("/proc/self")
        .map_err(|error| format!("could not inspect /proc/self: {error}"))?
        .uid();
    let mut users = Vec::new();
    for entry in fs::read_dir(proc_root)
        .map_err(|error| format!("could not enumerate {}: {error}", proc_root.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("could not enumerate proc entry: {error}")),
        };
        let Some(pid_text) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "could not inspect proc entry for pid {pid}: {error}"
                ))
            }
        };
        if metadata.uid() != own_uid {
            continue;
        }
        let proc_path = entry.path();
        if process_agent_name(&proc_path).is_some_and(|name| owners.contains(&name)) {
            return Err(format!(
                "recorded orphan owner process pid {pid} remains live"
            ));
        }
        for kind in ["cwd", "exe", "root"] {
            inspect_process_link(pid, kind, &proc_path.join(kind), targets, &mut users);
        }
        inspect_process_directory_links(pid, "fd", &proc_path.join("fd"), targets, &mut users);
        inspect_process_maps(pid, &proc_path, targets, &mut users);
        inspect_process_directory_links(
            pid,
            "map_files",
            &proc_path.join("map_files"),
            targets,
            &mut users,
        );
    }
    users.sort();
    users.dedup();
    Ok(users)
}

fn verify_orphan_locks_absent(root: &Path, inventories: &[ResidueInventory]) -> Result<(), String> {
    let path = restricted_fixture_path(root, "HERMIT_RELEASE_TEST_LOCKS")?
        .unwrap_or_else(|| PathBuf::from("/proc/locks"));
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("read kernel lock authority {}: {error}", path.display()))?;
    let identities: BTreeSet<(u64, u64, u64)> = inventories
        .iter()
        .flat_map(|inventory| inventory.files.iter())
        .map(|file| {
            (
                libc::major(file.device) as u64,
                libc::minor(file.device) as u64,
                file.inode,
            )
        })
        .collect();
    for line in text.lines() {
        for token in line.split_whitespace() {
            let fields: Vec<&str> = token.split(':').collect();
            if fields.len() != 3 {
                continue;
            }
            let parsed = (
                u64::from_str_radix(fields[0], 16),
                u64::from_str_radix(fields[1], 16),
                fields[2].parse::<u64>(),
            );
            if let (Ok(major), Ok(minor), Ok(inode)) = parsed {
                if identities.contains(&(major, minor, inode)) {
                    return Err(format!(
                        "live kernel lock references orphan residue: {line}"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn orphan_consumer_observation(
    root: &Path,
    slot_state: &Value,
    proc_root: &Path,
    container_token: &str,
    targets: &[PathBuf],
    inventories: &[ResidueInventory],
) -> Result<Value, String> {
    let owners = verify_orphan_owners_absent(root, slot_state)?;
    verify_orphan_units_absent(root, &owners, targets)?;
    verify_orphan_locks_absent(root, inventories)?;
    let users = orphan_process_users(proc_root, &owners, targets)?;
    if !users.is_empty() {
        return Err(format!(
            "live process references orphan residue: {}",
            users.join(", ")
        ));
    }
    container_release_audit(root, container_token, &owners, targets)
}

fn verify_no_slot_registrations(root: &Path, slot: &str) -> Result<(), String> {
    let slot_dir = root.join("worktrees/slots").join(slot);
    for product in ["hermit", "reverie", "liteinst2"] {
        let listing = git_inspect(&root.join(product), &["worktree", "list", "--porcelain"])?;
        if let Some(candidate) = listing
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(Path::new)
            .find(|candidate| candidate.starts_with(&slot_dir))
        {
            return Err(format!(
                "orphan finalizer found registered {product} worktree {}; expected zero registrations below {}",
                candidate.display(),
                slot_dir.display()
            ));
        }
    }
    Ok(())
}

fn initial_orphan_targets(
    root: &Path,
    slot: &str,
    slot_state: &Value,
    nonce: &str,
) -> Result<Vec<OrphanTarget>, String> {
    if slot_state["status"].as_str() != Some("released") {
        return Err(format!(
            "orphan finalizer requires status=released, found {:?}",
            slot_state["status"].as_str()
        ));
    }
    for forbidden in [
        "coordinator_lease",
        "coordinator_lease_intent",
        "coordinator_lease_revocation",
        "release_journal",
        "orphan_residue_journal",
    ] {
        if slot_state.get(forbidden).is_some() {
            return Err(format!(
                "orphan finalizer refuses slot with existing {forbidden} authority"
            ));
        }
    }
    for owner in slot_state["agents"].as_array().into_iter().flatten() {
        if owner.get("tmux_pane_id").is_some() || owner.get("cgroup_path").is_some() {
            return Err(
                "orphan finalizer is only for legacy lease-less rows; use normal release for leased owners"
                    .to_string(),
            );
        }
    }
    validate_canonical_slot_paths(slot, slot_state)?;
    verify_no_slot_registrations(root, slot)?;
    let mut targets = Vec::new();
    for (label, branch_key) in [
        ("hermit", "hermit_branch"),
        ("reverie", "reverie_branch"),
        ("liteinst2", "liteinst2_branch"),
    ] {
        let branch = slot_state[branch_key]
            .as_str()
            .ok_or_else(|| format!("orphan slot has no string {branch_key}"))?;
        if branch == "-" {
            continue;
        }
        let expected_relative = PathBuf::from("worktrees/slots").join(slot).join(label);
        let original = canonical_exact(root, &expected_relative)?;
        let top = git_output(
            &original,
            &["rev-parse", "--path-format=absolute", "--show-toplevel"],
        )?;
        if top.status.success() {
            let observed_text = String::from_utf8_lossy(&top.stdout).trim().to_string();
            let observed = fs::canonicalize(&observed_text).map_err(|error| {
                format!("canonicalize observed Git top-level {observed_text}: {error}")
            })?;
            if observed == original {
                return Err(format!(
                    "orphan finalizer refuses actual {label} Git worktree {}",
                    original.display()
                ));
            }
        }
        residue_inventory(root, &original)?;
        let fenced = original
            .parent()
            .expect("orphan target has slot parent")
            .join(format!(".{label}.orphan-residue-{nonce}"));
        if exact_path_present(&fenced)? {
            return Err(format!(
                "orphan fence destination already exists: {}",
                fenced.display()
            ));
        }
        targets.push(OrphanTarget {
            label,
            original,
            fenced,
        });
    }
    if targets.is_empty() {
        return Err("orphan finalizer found no recorded product residue".to_string());
    }
    let slot_dir = root.join("worktrees/slots").join(slot);
    let allowed: BTreeSet<&str> = targets.iter().map(|target| target.label).collect();
    for entry in fs::read_dir(&slot_dir)
        .map_err(|error| format!("enumerate orphan slot {}: {error}", slot_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("enumerate orphan slot entry: {error}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "orphan slot has non-UTF8 entry".to_string())?;
        if !allowed.contains(name.as_str()) {
            return Err(format!(
                "orphan slot contains unexpected entry {}; refusing",
                entry.path().display()
            ));
        }
    }
    Ok(targets)
}

fn parse_orphan_targets(
    root: &Path,
    slot: &str,
    journal: &Value,
) -> Result<Vec<OrphanTarget>, String> {
    let nonce = journal["transaction_nonce"]
        .as_str()
        .ok_or_else(|| "orphan journal has no transaction nonce".to_string())?;
    let rows = journal["targets"]
        .as_array()
        .ok_or_else(|| "orphan journal has no targets".to_string())?;
    let mut targets = Vec::new();
    for row in rows {
        let label = match row["label"].as_str() {
            Some("hermit") => "hermit",
            Some("reverie") => "reverie",
            Some("liteinst2") => "liteinst2",
            other => return Err(format!("orphan journal has invalid target {other:?}")),
        };
        let original = root.join("worktrees/slots").join(slot).join(label);
        let fenced = original
            .parent()
            .expect("orphan target has slot parent")
            .join(format!(".{label}.orphan-residue-{nonce}"));
        if row["original"].as_str() != original.to_str()
            || row["fenced"].as_str() != fenced.to_str()
        {
            return Err(format!(
                "orphan journal does not bind exact {label} original/fenced paths"
            ));
        }
        targets.push(OrphanTarget {
            label,
            original,
            fenced,
        });
    }
    if targets.is_empty() {
        return Err("orphan journal has no targets".to_string());
    }
    Ok(targets)
}

fn validate_orphan_journal(
    root: &Path,
    slot: &str,
    slot_state: &Value,
    note: &str,
) -> Result<(Value, Value, Vec<OrphanTarget>), String> {
    let journal = slot_state["orphan_residue_journal"]
        .as_object()
        .map(|_| slot_state["orphan_residue_journal"].clone())
        .ok_or_else(|| "slot has no readable orphan residue journal".to_string())?;
    if journal["schema_version"].as_u64() != Some(1)
        || journal["source"].as_str() != Some("coordinator-orphan-residue-v1")
        || journal["slot"].as_str() != Some(slot)
        || journal["recovery_note"].as_str() != Some(note)
        || journal["transaction_nonce"].as_str().is_none_or(|nonce| {
            nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(
            "orphan residue journal does not match this exact recovery request".to_string(),
        );
    }
    if !matches!(
        journal["phase"].as_str(),
        Some(
            "armed"
                | "evidence-complete"
                | "fence-authorized"
                | "fenced"
                | "residue-removed"
                | "physical-removal-complete"
        )
    ) {
        return Err("orphan residue journal has invalid phase".to_string());
    }
    let snapshot = journal["slot_snapshot"]
        .as_object()
        .map(|_| journal["slot_snapshot"].clone())
        .ok_or_else(|| "orphan residue journal has no slot snapshot".to_string())?;
    if snapshot["status"].as_str() != Some("released") {
        return Err("orphan journal snapshot was not released".to_string());
    }
    validate_canonical_slot_paths(slot, &snapshot)?;
    let mut current_base = slot_state.clone();
    let mut snapshot_base = snapshot.clone();
    for value in [&mut current_base, &mut snapshot_base] {
        let object = value
            .as_object_mut()
            .ok_or_else(|| "orphan slot record is not an object".to_string())?;
        object.remove("status");
        object.remove("updated");
        object.remove("orphan_residue_journal");
    }
    if current_base != snapshot_base {
        return Err("orphan slot authorization changed after journal arm".to_string());
    }
    let targets = parse_orphan_targets(root, slot, &journal)?;
    Ok((journal, snapshot, targets))
}

fn load_orphan_manifest(
    root: &Path,
    slot: &str,
    journal: &Value,
) -> Result<(PathBuf, String, Value), String> {
    let nonce = journal["transaction_nonce"]
        .as_str()
        .ok_or_else(|| "orphan journal has no nonce".to_string())?;
    let directory = evidence_directory(root, slot, nonce)?;
    let bytes = fs::read(directory.join("manifest.json"))
        .map_err(|error| format!("read orphan evidence manifest: {error}"))?;
    let digest = sha256_bytes(&bytes);
    if journal["manifest_sha256"].as_str() != Some(&digest) {
        return Err("orphan evidence manifest SHA-256 does not match journal".to_string());
    }
    let manifest: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse orphan evidence manifest: {error}"))?;
    if manifest["source"].as_str() != Some("coordinator-orphan-residue-v1")
        || manifest["slot"].as_str() != Some(slot)
        || manifest["transaction_nonce"] != journal["transaction_nonce"]
        || manifest["slot_snapshot"] != journal["slot_snapshot"]
    {
        return Err("orphan evidence manifest is not bound to the journal".to_string());
    }
    for target in manifest["targets"]
        .as_array()
        .ok_or_else(|| "orphan evidence manifest has no targets".to_string())?
    {
        let label = target["label"]
            .as_str()
            .ok_or_else(|| "orphan evidence target has no label".to_string())?;
        for file in target["inventory"]["files"]
            .as_array()
            .ok_or_else(|| format!("orphan evidence target {label} has no file inventory"))?
        {
            if file["copied"].as_bool() != Some(true) {
                continue;
            }
            let relative = file["path"]
                .as_str()
                .ok_or_else(|| "orphan evidence file has no path".to_string())?;
            let copy = directory.join(label).join(relative);
            let metadata = fs::symlink_metadata(&copy).map_err(|error| {
                format!("inspect orphan evidence copy {}: {error}", copy.display())
            })?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != file["bytes"].as_u64().unwrap_or(u64::MAX)
                || sha256_file(&copy)? != file["sha256"].as_str().unwrap_or("")
            {
                return Err(format!(
                    "orphan evidence copy {} does not match its manifest",
                    copy.display()
                ));
            }
        }
    }
    Ok((directory, digest, manifest))
}

fn current_orphan_sources(
    targets: &[OrphanTarget],
    allow_absent: bool,
) -> Result<Vec<Option<PathBuf>>, String> {
    let mut sources = Vec::new();
    for target in targets {
        let original = exact_path_present(&target.original)?;
        let fenced = exact_path_present(&target.fenced)?;
        match (original, fenced) {
            (true, false) => sources.push(Some(target.original.clone())),
            (false, true) => sources.push(Some(target.fenced.clone())),
            (true, true) => {
                return Err(format!(
                    "both orphan paths exist for {}: {}, {}",
                    target.label,
                    target.original.display(),
                    target.fenced.display()
                ))
            }
            (false, false) if allow_absent => sources.push(None),
            (false, false) => {
                return Err(format!(
                    "orphan {} paths are absent without journaled removal authority",
                    target.label
                ))
            }
        }
    }
    Ok(sources)
}

fn save_orphan_phase(root: &Path, state: &mut Value, slot: &str, phase: &str) {
    state["slots"][slot]["status"] = json!("finalizing-orphan-residue");
    state["slots"][slot]["updated"] = json!(now_iso());
    state["slots"][slot]["orphan_residue_journal"]["phase"] = json!(phase);
    save_state(root, state);
    regen_active_md(root, state);
}

fn verify_completed_orphan(root: &Path, slot: &str) -> Result<(), String> {
    verify_absent_slot_recovery(root, slot)?;
    let directory = root.join("ignored/worktree-recovery").join(slot);
    let mut completions = 0usize;
    for entry in fs::read_dir(&directory).map_err(|error| {
        format!(
            "read orphan recovery directory {}: {error}",
            directory.display()
        )
    })? {
        let entry = entry.map_err(|error| format!("read orphan recovery entry: {error}"))?;
        let completion = entry.path().join("completion.json");
        let Ok(bytes) = fs::read(&completion) else {
            continue;
        };
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse {}: {error}", completion.display()))?;
        if value["schema_version"].as_u64() != Some(1)
            || value["source"].as_str() != Some("coordinator-orphan-residue-v1")
            || value["slot"].as_str() != Some(slot)
            || value["phase"].as_str() != Some("physical-removal-complete")
        {
            continue;
        }
        let manifest = entry.path().join("manifest.json");
        if sha256_file(&manifest)? != value["manifest_sha256"].as_str().unwrap_or("") {
            return Err(format!(
                "completed orphan evidence {} has wrong manifest hash",
                entry.path().display()
            ));
        }
        completions += 1;
    }
    if completions == 0 {
        return Err("slot is absent without a completed orphan-residue transaction".to_string());
    }
    Ok(())
}

fn coordinator_finalize_orphan(
    root: &Path,
    state: &mut Value,
    slot: &str,
    note: &str,
) -> Result<(), String> {
    let crash_after_journal =
        restricted_fixture_path(root, "HERMIT_RELEASE_TEST_CRASH_AFTER_ORPHAN_JOURNAL")?;
    let crash_after_evidence =
        restricted_fixture_path(root, "HERMIT_RELEASE_TEST_CRASH_AFTER_ORPHAN_EVIDENCE")?;
    let crash_after_fence =
        restricted_fixture_path(root, "HERMIT_RELEASE_TEST_CRASH_AFTER_ORPHAN_FENCE")?;
    let crash_after_remove =
        restricted_fixture_path(root, "HERMIT_RELEASE_TEST_CRASH_AFTER_ORPHAN_REMOVE")?;
    let crash_after_final_state =
        restricted_fixture_path(root, "HERMIT_RELEASE_TEST_CRASH_AFTER_ORPHAN_FINAL_STATE")?;

    if state["slots"].get(slot).is_none() {
        verify_completed_orphan(root, slot)?;
        regen_active_md(root, state);
        println!("✓ orphan residue for {slot} was already finalized");
        return Ok(());
    }
    validate_owners(slot, &state["slots"][slot])?;
    let proc_root = process_root(root)?;
    let (_container_lock, container_token) = lock_container_lifecycle(root)?;

    if state["slots"][slot].get("orphan_residue_journal").is_none() {
        let nonce = secure_transaction_nonce()?;
        let snapshot = state["slots"][slot].clone();
        let targets = initial_orphan_targets(root, slot, &snapshot, &nonce)?;
        let paths: Vec<PathBuf> = targets
            .iter()
            .map(|target| target.original.clone())
            .collect();
        let inventories: Vec<ResidueInventory> = paths
            .iter()
            .map(|path| residue_inventory(root, path))
            .collect::<Result<_, _>>()?;
        orphan_consumer_observation(
            root,
            &snapshot,
            &proc_root,
            &container_token,
            &paths,
            &inventories,
        )?;
        state["slots"][slot]["status"] = json!("finalizing-orphan-residue");
        state["slots"][slot]["updated"] = json!(now_iso());
        state["slots"][slot]["orphan_residue_journal"] = json!({
            "schema_version": 1,
            "source": "coordinator-orphan-residue-v1",
            "slot": slot,
            "phase": "armed",
            "transaction_nonce": nonce,
            "recovery_note": note,
            "authorized_at": now_iso(),
            "slot_snapshot": snapshot,
            "targets": targets.iter().map(|target| json!({
                "label": target.label,
                "original": target.original,
                "fenced": target.fenced,
            })).collect::<Vec<_>>(),
        });
        save_state(root, state);
        maybe_inject_fixture_crash(
            crash_after_journal.as_deref(),
            "post-orphan-journal crash injected",
        )?;
        regen_active_md(root, state);
    }

    let (mut journal, snapshot, targets) =
        validate_orphan_journal(root, slot, &state["slots"][slot], note)?;
    verify_no_slot_registrations(root, slot)?;
    let phase = journal["phase"].as_str().unwrap_or("").to_string();
    if phase == "armed" {
        let source_paths = current_orphan_sources(&targets, false)?
            .into_iter()
            .map(|path| path.expect("non-absent orphan source"))
            .collect::<Vec<_>>();
        let inventories = source_paths
            .iter()
            .map(|path| residue_inventory(root, path))
            .collect::<Result<Vec<_>, _>>()?;
        let paired: Vec<(PathBuf, ResidueInventory)> = source_paths
            .iter()
            .cloned()
            .zip(inventories.iter().cloned())
            .collect();
        let (_, manifest_sha256, _) = build_or_verify_evidence(
            root,
            slot,
            journal["transaction_nonce"].as_str().unwrap(),
            &snapshot,
            &targets,
            &paired,
        )?;
        state["slots"][slot]["orphan_residue_journal"]["manifest_sha256"] = json!(manifest_sha256);
        save_orphan_phase(root, state, slot, "evidence-complete");
        maybe_inject_fixture_crash(
            crash_after_evidence.as_deref(),
            "post-orphan-evidence crash injected",
        )?;
        journal = state["slots"][slot]["orphan_residue_journal"].clone();
    }

    let (evidence_dir, manifest_sha256, manifest) = load_orphan_manifest(root, slot, &journal)?;
    let phase = journal["phase"].as_str().unwrap_or("");
    if !matches!(phase, "residue-removed" | "physical-removal-complete") {
        let allow_absent = phase == "fenced";
        let sources = current_orphan_sources(&targets, allow_absent)?;
        let mut current_paths = Vec::new();
        let mut inventories = Vec::new();
        for (target, source) in targets.iter().zip(sources.iter()) {
            if let Some(source) = source {
                let inventory = residue_inventory(root, source)?;
                verify_inventory_row(&manifest, target.label, &inventory)?;
                current_paths.push(source.clone());
                inventories.push(inventory);
            }
        }
        let pre_containers = orphan_consumer_observation(
            root,
            &state["slots"][slot],
            &proc_root,
            &container_token,
            &current_paths,
            &inventories,
        )?;
        state["slots"][slot]["orphan_residue_journal"]["container_observation"] =
            pre_containers.clone();
        save_orphan_phase(root, state, slot, "fence-authorized");
        for target in &targets {
            if exact_path_present(&target.original)? {
                rename_no_replace(&target.original, &target.fenced).map_err(|error| {
                    format!(
                        "could not fence orphan residue {} -> {}: {error}",
                        target.original.display(),
                        target.fenced.display()
                    )
                })?;
            }
        }
        let fenced_paths: Vec<PathBuf> = targets
            .iter()
            .filter(|target| target.fenced.exists())
            .map(|target| target.fenced.clone())
            .collect();
        let fenced_inventories: Vec<ResidueInventory> = fenced_paths
            .iter()
            .map(|path| residue_inventory(root, path))
            .collect::<Result<_, _>>()?;
        for target in &targets {
            if target.fenced.exists() {
                verify_inventory_row(
                    &manifest,
                    target.label,
                    &residue_inventory(root, &target.fenced)?,
                )?;
            }
        }
        let post_containers = orphan_consumer_observation(
            root,
            &state["slots"][slot],
            &proc_root,
            &container_token,
            &fenced_paths,
            &fenced_inventories,
        )?;
        if post_containers != pre_containers {
            return Err(
                "container census/config/mount observation changed across orphan path fence"
                    .to_string(),
            );
        }
        save_orphan_phase(root, state, slot, "fenced");
        maybe_inject_fixture_crash(
            crash_after_fence.as_deref(),
            "post-orphan-fence crash injected",
        )?;
        for target in &targets {
            if target.fenced.exists() {
                let inventory = residue_inventory(root, &target.fenced)?;
                verify_inventory_row(&manifest, target.label, &inventory)?;
                orphan_consumer_observation(
                    root,
                    &state["slots"][slot],
                    &proc_root,
                    &container_token,
                    &[target.fenced.clone()],
                    &[inventory],
                )?;
                fs::remove_dir_all(&target.fenced).map_err(|error| {
                    format!(
                        "remove fenced generated residue {}: {error}",
                        target.fenced.display()
                    )
                })?;
            }
        }
        save_orphan_phase(root, state, slot, "residue-removed");
        maybe_inject_fixture_crash(
            crash_after_remove.as_deref(),
            "post-orphan-remove crash injected",
        )?;
    }

    for target in &targets {
        if exact_path_present(&target.original)? || exact_path_present(&target.fenced)? {
            return Err(format!(
                "orphan {} residue remains after removal",
                target.label
            ));
        }
    }
    verify_no_slot_registrations(root, slot)?;
    let slot_dir = root.join("worktrees/slots").join(slot);
    if slot_dir.exists() {
        fs::remove_dir(&slot_dir).map_err(|error| {
            format!(
                "remove now-empty orphan slot directory {}: {error}",
                slot_dir.display()
            )
        })?;
    }
    let completion = json!({
        "schema_version": 1,
        "source": "coordinator-orphan-residue-v1",
        "phase": "physical-removal-complete",
        "slot": slot,
        "transaction_nonce": journal["transaction_nonce"],
        "manifest_sha256": manifest_sha256,
        "completed_at": now_iso(),
    });
    let mut completion_bytes = serde_json::to_vec_pretty(&completion)
        .map_err(|error| format!("serialize orphan completion: {error}"))?;
    completion_bytes.push(b'\n');
    durable_replace(&evidence_dir.join("completion.json"), &completion_bytes)
        .map_err(|error| format!("persist orphan completion evidence: {error}"))?;
    save_orphan_phase(root, state, slot, "physical-removal-complete");
    state["slots"].as_object_mut().unwrap().remove(slot);
    save_state(root, state);
    maybe_inject_fixture_crash(
        crash_after_final_state.as_deref(),
        "post-orphan-final-state crash injected",
    )?;
    regen_active_md(root, state);
    println!(
        "✓ finalized orphan residue for {slot}; evidence: {}",
        evidence_dir.display()
    );
    Ok(())
}

fn verify_completed_git_removal(target: &CleanTarget, fenced: &Path) -> Result<(), String> {
    let original_present = exact_path_present(&target.path)?;
    let fenced_present = exact_path_present(fenced)?;
    let listing = git_inspect(&target.primary, &["worktree", "list", "--porcelain"])?;
    let registrations = listing
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .filter(|candidate| {
            let candidate = Path::new(candidate);
            candidate == target.path.as_path() || candidate == fenced
        })
        .count();
    if original_present || fenced_present || registrations != 0 {
        return Err(format!(
            "Git removal postcondition failed for {}: original_present={original_present} fenced_present={fenced_present} registrations={registrations}",
            target.label
        ));
    }
    Ok(())
}

fn recover_path_fence(root: &Path, state: &Value, slot: &str) -> Result<PathFenceRecovery, String> {
    let Some((target, fenced)) = journal_target(root, state, slot)? else {
        return Ok(PathFenceRecovery::None);
    };
    let original_exists = exact_path_present(&target.path)?;
    let fenced_exists = exact_path_present(&fenced)?;
    let paths = match (original_exists, fenced_exists) {
        (true, false) => {
            let paths = submodule_admin_paths(&target)?;
            match fs::symlink_metadata(&paths.path_fence) {
                Ok(_) => match validate_path_fence_marker(&paths, &target.path, &fenced)? {
                    PathFenceMethod::GitWorktreeMove => {
                        fs::remove_file(&paths.path_fence).map_err(|error| {
                            format!("clear recovered path-fence marker: {error}")
                        })?;
                    }
                    PathFenceMethod::AtomicRenameRepair => {
                        restore_atomic_path_fence(&target, &fenced, &paths)?;
                    }
                },
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(format!("inspect path-fence marker: {error}")),
            }
            paths
        }
        (false, true) => {
            let fenced_target = CleanTarget {
                path: fenced.clone(),
                ..target.clone()
            };
            let paths = submodule_admin_paths(&fenced_target)?;
            match validate_path_fence_marker(&paths, &target.path, &fenced)? {
                PathFenceMethod::GitWorktreeMove => {
                    let fenced_text = fenced.to_string_lossy().into_owned();
                    let original = target.path.to_string_lossy().into_owned();
                    git_inspect(
                        &target.primary,
                        &["worktree", "move", &fenced_text, &original],
                    )?;
                    fs::remove_file(&paths.path_fence)
                        .map_err(|error| format!("clear recovered path-fence marker: {error}"))?;
                }
                PathFenceMethod::AtomicRenameRepair => {
                    restore_atomic_path_fence(&target, &fenced, &paths)?;
                }
            }
            paths
        }
        (true, true) => {
            return Err(format!(
                "both canonical and fenced targets exist: {}, {}",
                target.path.display(),
                fenced.display()
            ))
        }
        (false, false) => {
            // Git may have completed `worktree remove` immediately before the
            // process crashed. Accept that terminal transaction state only
            // when this exact release process durably recorded successful Git
            // removal and its postcondition. Mere absence is not causal proof:
            // an out-of-band deletion can produce the same proxy state.
            if state["slots"][slot]["release_journal"]["phase"].as_str()
                != Some("git-removal-complete")
            {
                return Err(format!(
                    "both release paths are absent without durable git-removal-complete evidence for {}",
                    target.label
                ));
            }
            verify_completed_git_removal(&target, &fenced)?;
            eprintln!(
                "✓ recovered completed Git removal for {}; registry state will be advanced",
                target.label
            );
            return Ok(PathFenceRecovery::RemovalCompleted(target));
        }
    };
    restore_force_fence_admin(&paths)?;
    eprintln!(
        "✓ recovered path-acquisition fence for {}; recursive proofs will be repeated",
        target.path.display()
    );
    Ok(PathFenceRecovery::Restored)
}

fn rollback_fenced_removal(
    target: &CleanTarget,
    fenced: &Path,
    fence_paths: &SubmoduleAdminPaths,
    submodule_cleanup: Option<&SubmoduleAdminPaths>,
    force_fence_admin: bool,
) -> Result<(), String> {
    rollback_path_fence(target, fenced, fence_paths)?;
    if let Some(paths) = submodule_cleanup {
        restore_submodule_admin(paths)?;
    }
    if force_fence_admin {
        restore_force_fence_admin(fence_paths)?;
    }
    Ok(())
}

/// Remove through Git's own non-force cleanliness boundary. Initialized
/// submodules are first deinitialized without force after recursive proof; an
/// explicit user `--force` is the only path that reaches Git-level force.
fn remove_target(
    root: &Path,
    target: &CleanTarget,
    fenced: &Path,
    repositories: &[RepoSnapshot],
    proc_root: &Path,
    cgroup_root: &Path,
    container_fence_token: &str,
    owners: &OwnerAuthorities,
    allow_dirty: bool,
    allow_live_owner_identity: bool,
) -> Result<(), String> {
    // Validate the test-only crash hook before creating a transaction marker or
    // deinitializing anything. A leaked test variable in production therefore
    // refuses without changing the target.
    let crash_marker = restricted_fixture_path(root, "HERMIT_RELEASE_TEST_CRASH_AFTER_DEINIT")?;
    let fence_crash_marker =
        restricted_fixture_path(root, "HERMIT_RELEASE_TEST_CRASH_AFTER_PATH_FENCE")?;
    let fallback_rename_crash_marker =
        restricted_fixture_path(root, "HERMIT_RELEASE_TEST_CRASH_AFTER_FALLBACK_RENAME")?;
    ensure_no_submodule_cleanup_artifacts(target)?;
    let mut submodule_cleanup = if !allow_dirty && repositories.len() > 1 {
        let paths = begin_submodule_cleanup(target)?;
        git_inspect(&target.path, &["submodule", "deinit", "--all"])?;
        Some(paths)
    } else {
        None
    };
    let force_fence_admin = allow_dirty && repositories.len() > 1;

    let status = git_inspect(
        &target.path,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !allow_dirty && !status.is_empty() {
        return Err(format!(
            "last-moment work appeared in {}: {}",
            target.path.display(),
            status.replace('\n', "; ")
        ));
    }

    // Git conflates "contains clean initialized submodules" with dirty-force.
    // Hide only this exact linked worktree's already-proven submodule admin so
    // ordinary removal can retain Git's own non-force dirty boundary.
    if let Some(paths) = &submodule_cleanup {
        quarantine_submodule_admin(paths)?;
        maybe_inject_fixture_crash(crash_marker.as_deref(), "post-deinit crash injected")?;
    }
    if force_fence_admin {
        let paths = submodule_admin_paths(target)?;
        quarantine_force_fence_admin(&paths)?;
    }

    // Bind the post-fence proof to the complete container/config/mount
    // observation immediately before the pathname moves. The lifecycle lock
    // excludes supported mutations; an out-of-protocol Podman change must be
    // observed as a changed authority rather than merely reclassified against
    // a different pathname.
    let pre_fence_containers = container_release_audit(
        root,
        container_fence_token,
        &owners.names,
        &[target.path.clone()],
    )?;

    // Atomically remove the published canonical pathname before the last
    // ownership proof. An acquisition that won the earlier race follows the
    // moved inode and is visible below; a later canonical acquisition receives
    // ENOENT. The lifecycle lock separately excludes supported container
    // creation across this interval.
    let fence_result = begin_path_fence(
        target,
        fenced,
        !allow_dirty,
        fallback_rename_crash_marker.as_deref(),
    )?;
    let fence_paths = fence_result.paths;
    if fence_result.deinitialized_submodule_fallback {
        if submodule_cleanup.is_some() || force_fence_admin {
            return Err(
                "deinitialized-submodule path fallback conflicted with initialized/force admin state"
                    .to_string(),
            );
        }
        begin_deinitialized_submodule_cleanup(&fence_paths, &target.path, fenced)?;
        quarantine_submodule_admin(&fence_paths)?;
        maybe_inject_fixture_crash(crash_marker.as_deref(), "post-deinit crash injected")?;
        submodule_cleanup = Some(fence_paths.clone());
    }
    maybe_inject_fixture_crash(
        fence_crash_marker.as_deref(),
        "post-path-fence crash injected",
    )?;
    let proof_paths = [target.path.clone(), fenced.to_path_buf()];
    let (users, post_fence_containers) = match live_process_users(
        root,
        proc_root,
        cgroup_root,
        container_fence_token,
        owners,
        &proof_paths,
        allow_live_owner_identity,
    ) {
        Ok(users) => users,
        Err(error) => {
            return match rollback_fenced_removal(
                target,
                fenced,
                &fence_paths,
                submodule_cleanup.as_ref(),
                force_fence_admin,
            ) {
                Ok(()) => Err(error),
                Err(rollback) => Err(format!(
                "{error}; path-fence rollback failed: {rollback}; use --recover-submodule-cleanup"
            )),
            }
        }
    };
    if post_fence_containers != pre_fence_containers {
        let error =
            "container census/config/mount observation changed across the path fence".to_string();
        return match rollback_fenced_removal(
            target,
            fenced,
            &fence_paths,
            submodule_cleanup.as_ref(),
            force_fence_admin,
        ) {
            Ok(()) => Err(error),
            Err(rollback) => Err(format!(
                "{error}; path-fence rollback failed: {rollback}; use --recover-submodule-cleanup"
            )),
        };
    }
    if !users.is_empty() {
        let error = format!(
            "post-fence live process ownership below {}: {}",
            fenced.display(),
            users.join(", ")
        );
        return match rollback_fenced_removal(
            target,
            fenced,
            &fence_paths,
            submodule_cleanup.as_ref(),
            force_fence_admin,
        ) {
            Ok(()) => Err(error),
            Err(rollback) => Err(format!(
                "{error}; path-fence rollback failed: {rollback}; use --recover-submodule-cleanup"
            )),
        };
    }

    let path = fenced.to_string_lossy().into_owned();
    let args = if allow_dirty {
        vec!["worktree", "remove", "--force", &path]
    } else {
        vec!["worktree", "remove", &path]
    };
    let output = match git_output(&target.primary, &args) {
        Ok(output) => output,
        Err(error) => {
            return match rollback_fenced_removal(
                target,
                fenced,
                &fence_paths,
                submodule_cleanup.as_ref(),
                force_fence_admin,
            ) {
                Ok(()) => Err(error),
                Err(rollback) => Err(format!(
                    "{error}; path-fence rollback failed: {rollback}; use --recover-submodule-cleanup"
                )),
            };
        }
    };
    if !output.status.success() {
        let error = format!(
            "could not remove exact {} target {}: {}",
            target.label,
            fenced.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return match rollback_fenced_removal(
            target,
            fenced,
            &fence_paths,
            submodule_cleanup.as_ref(),
            force_fence_admin,
        ) {
            Ok(()) => Err(error),
            Err(rollback) => Err(format!(
                "{error}; path-fence rollback failed: {rollback}; use --recover-submodule-cleanup"
            )),
        };
    }
    if let Err(error) = verify_completed_git_removal(target, fenced) {
        return match rollback_fenced_removal(
            target,
            fenced,
            &fence_paths,
            submodule_cleanup.as_ref(),
            force_fence_admin,
        ) {
            Ok(()) => Err(error),
            Err(rollback) => Err(format!(
                "{error}; postcondition rollback failed: {rollback}; use --recover-submodule-cleanup"
            )),
        };
    }
    Ok(())
}

const RELEASE_REQUEST_TTL_SECONDS: u64 = 60;

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Debug)]
struct SlotLeaseView {
    generation: String,
    state: worktree_registry::OwnerLeaseState,
    expires_at: u64,
    source: worktree_registry::OwnerSidecarSource,
    automatic_handoff: Option<Value>,
}

fn slot_lease_view(slot: &str, slot_state: &Value) -> Result<SlotLeaseView, String> {
    let sidecar = worktree_registry::OwnerSidecar::from_slot_value(slot_state, slot)?;
    let lease = sidecar
        .lease
        .as_ref()
        .ok_or_else(|| format!("slot {slot} has no TTL owner lease"))?;
    Ok(SlotLeaseView {
        generation: sidecar.generation,
        state: lease.state,
        expires_at: lease.expires_at,
        source: sidecar.source,
        automatic_handoff: lease.automatic_handoff.clone(),
    })
}

fn update_slot_owner_lease<F>(slot: &str, slot_state: &mut Value, update: F) -> Result<(), String>
where
    F: FnOnce(&mut worktree_registry::OwnerLease) -> Result<(), String>,
{
    let mut sidecar = worktree_registry::OwnerSidecar::from_slot_value(slot_state, slot)?;
    let lease = sidecar
        .lease
        .as_mut()
        .ok_or_else(|| format!("slot {slot} has no TTL owner lease"))?;
    update(lease)?;
    let serialized = sidecar.to_value()?;
    worktree_registry::OwnerSidecar::from_value(&serialized, slot)?;
    slot_state["owner_sidecar"] = serialized;
    Ok(())
}

fn valid_actor(actor: &str) -> bool {
    !actor.is_empty()
        && actor.len() <= 128
        && actor.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/' | b'+')
        })
}

fn owner_authority(root: &Path, slot: &str) -> Result<Value, String> {
    let output = Command::new(root.join("scripts/slot-owner-authority.py"))
        .args(["--slot", slot, "--json"])
        .output()
        .map_err(|error| format!("run slot-owner authority: {error}"))?;
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "slot-owner authority returned invalid JSON (exit {:?}): {error}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })
}

fn bind_owner_authority_to_locked_state(
    state: &Value,
    slot: &str,
    authority: &Value,
) -> Result<(), String> {
    let slot_state = state["slots"]
        .get(slot)
        .ok_or_else(|| format!("slot {slot} disappeared before stale-clear lock acquisition"))?;
    let sidecar = worktree_registry::OwnerSidecar::from_slot_value(slot_state, slot)?;
    let owner = slot_state["agents"]
        .as_array()
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent["read_only"].as_bool() == Some(false))
        })
        .ok_or_else(|| format!("slot {slot} has no mutating owner"))?;
    let expected_identity = json!({
        "source": sidecar.source.as_str(),
        "generation": sidecar.generation,
        "supervisor_pid": sidecar.supervisor_pid,
        "boot_id": sidecar.boot_id,
        "start_ticks": sidecar.start_ticks,
        "lease": sidecar.lease,
    });
    if authority["schema_version"].as_u64() != Some(1)
        || authority["slot"].as_str() != Some(slot)
        || authority["agent"] != owner["name"]
        || authority["owner_identity"] != expected_identity
    {
        return Err(format!(
            "slot {slot} owner authority changed before stale-clear lock acquisition"
        ));
    }
    Ok(())
}

fn automatic_handoff(
    root: &Path,
    state: &Value,
    slot: &str,
    actor: &str,
) -> Result<(Value, bool), String> {
    let lease = slot_lease_view(slot, &state["slots"][slot])?;
    let targets = clean_targets_from_state(root, state, slot)?;
    let mut repositories = Vec::new();
    let mut complete = true;
    for target in targets {
        for inspection in inspect_target_repositories(&target.path)? {
            let clean = inspection.status.is_empty();
            let durable = if clean {
                remote_durable(&inspection.snapshot)?
            } else {
                false
            };
            complete &= clean && durable;
            let branch = git_inspect(&inspection.snapshot.path, &["branch", "--show-current"])?;
            repositories.push(json!({
                "path": inspection.snapshot.path.to_string_lossy(),
                "head": inspection.snapshot.head,
                "origin_url": inspection.snapshot.origin_url,
                "branch": branch,
                "status": inspection.status,
                "clean": clean,
                "origin_durable": durable,
            }));
        }
    }
    repositories.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap_or("")
            .cmp(right["path"].as_str().unwrap_or(""))
    });
    let mut handoff = json!({
        "schema_version": 1,
        "resource": format!("slot/{slot}"),
        "generation": lease.generation,
        "recorded_at": now_iso(),
        "actor": actor,
        "complete": complete,
        "repositories": repositories,
        "continuation": stale_clear_continuation(
            slot,
            &lease.generation,
            actor,
            false,
        ),
    });
    let digest = sha256_bytes(
        &serde_json::to_vec(&handoff).map_err(|error| format!("serialize handoff: {error}"))?,
    );
    handoff["sha256"] = json!(digest);
    Ok((handoff, complete))
}

fn stale_clear_continuation(
    slot: &str,
    generation: &str,
    actor: &str,
    recover_quarantined_handoff: bool,
) -> String {
    let recovery = if recover_quarantined_handoff {
        " --recover-quarantined-handoff"
    } else {
        ""
    };
    format!(
        "./scripts/release-worktree.rs --slot {slot} --clean --stale-clear{recovery} --expected-generation {generation} --actor {actor}"
    )
}

fn set_handoff_continuation(
    handoff: &mut Value,
    slot: &str,
    generation: &str,
    actor: &str,
    recover_quarantined_handoff: bool,
) -> Result<(), String> {
    {
        let object = handoff
            .as_object_mut()
            .ok_or_else(|| "automatic handoff is not an object".to_string())?;
        object.remove("sha256");
        object.insert(
            "continuation".to_string(),
            json!(stale_clear_continuation(
                slot,
                generation,
                actor,
                recover_quarantined_handoff,
            )),
        );
    }
    let digest = sha256_bytes(
        &serde_json::to_vec(handoff).map_err(|error| format!("serialize handoff: {error}"))?,
    );
    handoff["sha256"] = json!(digest);
    Ok(())
}

fn validate_recorded_handoff(slot: &str, generation: &str, handoff: &Value) -> Result<(), String> {
    let object = handoff.as_object().ok_or_else(|| {
        format!("slot {slot} quarantined lease has no readable automatic handoff")
    })?;
    if object.get("schema_version").and_then(Value::as_u64) != Some(1)
        || object.get("resource").and_then(Value::as_str) != Some(&format!("slot/{slot}"))
        || object.get("generation").and_then(Value::as_str) != Some(generation)
        || object.get("complete").and_then(Value::as_bool).is_none()
        || object
            .get("repositories")
            .and_then(Value::as_array)
            .is_none()
        || object.get("continuation").and_then(Value::as_str).is_none()
    {
        return Err(format!(
            "slot {slot} quarantined automatic handoff is not identity-bound and complete enough to resume"
        ));
    }
    let recorded_digest = object
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("slot {slot} quarantined automatic handoff has no sha256"))?;
    let mut unsigned = handoff.clone();
    unsigned
        .as_object_mut()
        .expect("validated automatic handoff object")
        .remove("sha256");
    let actual_digest = sha256_bytes(
        &serde_json::to_vec(&unsigned)
            .map_err(|error| format!("serialize recorded handoff: {error}"))?,
    );
    if recorded_digest != actual_digest {
        return Err(format!(
            "slot {slot} quarantined automatic handoff sha256 does not match its recorded content"
        ));
    }
    Ok(())
}

fn mutating_owner(slot_state: &Value) -> Result<&str, String> {
    let mut owners = slot_state["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|owner| owner["read_only"].as_bool() == Some(false));
    let owner = owners
        .next()
        .and_then(|owner| owner["name"].as_str())
        .ok_or_else(|| "slot has no mutating owner".to_string())?;
    if owners.next().is_some() {
        return Err("slot has multiple mutating owners".to_string());
    }
    Ok(owner)
}

fn handoff_state_matches(recorded: &Value, current: &Value) -> bool {
    recorded["generation"] == current["generation"]
        && recorded["complete"] == current["complete"]
        && recorded["repositories"] == current["repositories"]
}

fn request_release(
    root: &Path,
    state: &mut Value,
    slot: &str,
    actor: &str,
) -> Result<Value, String> {
    let owner = mutating_owner(&state["slots"][slot])?;
    let caller = std::env::var("DG_AGENT_NAME")
        .map_err(|_| "release request requires DG_AGENT_NAME caller identity".to_string())?;
    if actor != owner || caller != owner {
        return Err(format!(
            "release request actor/caller {actor:?}/{caller:?} does not equal exact owner {owner:?}"
        ));
    }
    let lease = slot_lease_view(slot, &state["slots"][slot])?;
    if !matches!(
        lease.state,
        worktree_registry::OwnerLeaseState::Active | worktree_registry::OwnerLeaseState::Draining
    ) {
        return Err(format!(
            "slot {slot} lease is {}, expected active or draining",
            lease.state.as_str()
        ));
    }
    let (current_handoff, complete) = automatic_handoff(root, state, slot, actor)?;
    let mut handoff = if lease.state == worktree_registry::OwnerLeaseState::Draining {
        let recorded = lease
            .automatic_handoff
            .as_ref()
            .filter(|value| !value.is_null())
            .cloned()
            .ok_or_else(|| "draining lease has no automatic handoff".to_string())?;
        if !handoff_state_matches(&recorded, &current_handoff) {
            let mut quarantined = recorded;
            set_handoff_continuation(&mut quarantined, slot, &lease.generation, actor, true)?;
            update_slot_owner_lease(slot, &mut state["slots"][slot], |owner_lease| {
                owner_lease.state = worktree_registry::OwnerLeaseState::Quarantined;
                owner_lease.automatic_handoff = Some(quarantined);
                Ok(())
            })?;
            state["slots"][slot]["status"] = json!("lease-quarantined");
            save_state(root, state);
            regen_active_md(root, state);
            return Err(
                "automatic handoff changed after release request; lease quarantined".into(),
            );
        }
        recorded
    } else {
        current_handoff
    };
    if !complete {
        set_handoff_continuation(&mut handoff, slot, &lease.generation, actor, true)?;
    }
    let now = epoch_now();
    let slot_state = &mut state["slots"][slot];
    update_slot_owner_lease(slot, slot_state, |owner_lease| {
        owner_lease.renewed_at = now;
        owner_lease.ttl_seconds = RELEASE_REQUEST_TTL_SECONDS;
        owner_lease.expires_at = now
            .checked_add(RELEASE_REQUEST_TTL_SECONDS)
            .ok_or_else(|| "release request lease expiration overflows".to_string())?;
        owner_lease.state = if complete {
            worktree_registry::OwnerLeaseState::Draining
        } else {
            worktree_registry::OwnerLeaseState::Quarantined
        };
        owner_lease.last_transition_actor = actor.to_string();
        owner_lease.automatic_handoff = Some(handoff);
        Ok(())
    })?;
    slot_state["status"] = json!(if complete {
        "release-requested"
    } else {
        "lease-quarantined"
    });
    slot_state["updated"] = json!(now_iso());
    save_state(root, state);
    regen_active_md(root, state);
    if !complete {
        return Err("automatic handoff found dirty or non-durable work; lease quarantined".into());
    }
    println!(
        "✓ release requested for {slot}; generation={} expires_at={}",
        lease.generation,
        now + RELEASE_REQUEST_TTL_SECONDS
    );
    Ok(json!({
        "schema_version": 1,
        "resource": format!("slot/{slot}"),
        "generation": lease.generation,
        "actor": actor,
        "authority": "exact-live-owner-self-release",
        "handoff_sha256": state["slots"][slot]["owner_sidecar"]["lease"]
            ["automatic_handoff"]["sha256"],
    }))
}

fn prepare_stale_clear(
    root: &Path,
    state: &mut Value,
    slot: &str,
    expected_generation: &str,
    actor: &str,
    authority: &Value,
    recover_quarantined_handoff: bool,
) -> Result<Value, String> {
    let lease = slot_lease_view(slot, &state["slots"][slot])?;
    if lease.generation != expected_generation {
        return Err(format!(
            "slot {slot} generation changed: expected {expected_generation}, found {}",
            lease.generation
        ));
    }
    let quarantined = lease.state == worktree_registry::OwnerLeaseState::Quarantined;
    if quarantined && !recover_quarantined_handoff {
        return Err(format!(
            "slot {slot} lease is quarantined; after resolving the recorded handoff, run {}",
            stale_clear_continuation(slot, &lease.generation, actor, true)
        ));
    }
    if recover_quarantined_handoff
        && (!quarantined || state["slots"][slot]["status"].as_str() != Some("lease-quarantined"))
    {
        return Err(format!(
            "--recover-quarantined-handoff requires slot {slot} state lease-quarantined with a quarantined lease; found status={:?} lease={}",
            state["slots"][slot]["status"].as_str(),
            lease.state.as_str(),
        ));
    }
    if epoch_now() < lease.expires_at {
        return Err(format!(
            "slot {slot} lease is not expired (expires_at={})",
            lease.expires_at
        ));
    }
    let authority_state = authority["state"].as_str().unwrap_or("indeterminate");
    let codex_sentinel =
        lease.source == worktree_registry::OwnerSidecarSource::CodexSystemdSentinelV1;
    if authority_state == "indeterminate" {
        return Err(format!(
            "slot {slot} owner authority is INDETERMINATE; stale-clear is fail-closed"
        ));
    }
    bind_owner_authority_to_locked_state(state, slot, authority)?;
    if authority_state != "dead" && !codex_sentinel {
        return Err(format!(
            "slot {slot} owner authority is {authority_state}; live ORC owner refuses stale-clear"
        ));
    }
    let recorded = lease
        .automatic_handoff
        .as_ref()
        .filter(|value| !value.is_null())
        .cloned();
    if quarantined {
        validate_recorded_handoff(
            slot,
            &lease.generation,
            recorded.as_ref().ok_or_else(|| {
                format!("slot {slot} quarantined lease has no automatic handoff to resume")
            })?,
        )?;
    }
    let (mut current_handoff, complete) = automatic_handoff(root, state, slot, actor)?;
    if !complete {
        set_handoff_continuation(&mut current_handoff, slot, &lease.generation, actor, true)?;
        update_slot_owner_lease(slot, &mut state["slots"][slot], |owner_lease| {
            owner_lease.state = worktree_registry::OwnerLeaseState::Quarantined;
            owner_lease.last_transition_actor = actor.to_string();
            owner_lease.automatic_handoff = Some(current_handoff);
            Ok(())
        })?;
        state["slots"][slot]["status"] = json!("lease-quarantined");
        save_state(root, state);
        regen_active_md(root, state);
        return Err(
            "automatic handoff found dirty or non-durable work; lease remains quarantined".into(),
        );
    }
    if let Some(recorded) = recorded.as_ref().filter(|_| !quarantined) {
        if !handoff_state_matches(&recorded, &current_handoff) {
            let mut quarantined_handoff = recorded.clone();
            set_handoff_continuation(
                &mut quarantined_handoff,
                slot,
                &lease.generation,
                actor,
                true,
            )?;
            update_slot_owner_lease(slot, &mut state["slots"][slot], |owner_lease| {
                owner_lease.state = worktree_registry::OwnerLeaseState::Quarantined;
                owner_lease.automatic_handoff = Some(quarantined_handoff);
                Ok(())
            })?;
            state["slots"][slot]["status"] = json!("lease-quarantined");
            save_state(root, state);
            regen_active_md(root, state);
            return Err(
                "automatic handoff changed after release request; lease quarantined".into(),
            );
        }
    } else {
        update_slot_owner_lease(slot, &mut state["slots"][slot], |owner_lease| {
            owner_lease.automatic_handoff = Some(current_handoff);
            Ok(())
        })?;
    }
    update_slot_owner_lease(slot, &mut state["slots"][slot], |owner_lease| {
        owner_lease.state = worktree_registry::OwnerLeaseState::Draining;
        owner_lease.last_transition_actor = actor.to_string();
        Ok(())
    })?;
    state["slots"][slot]["status"] = json!("release-requested");
    save_state(root, state);
    regen_active_md(root, state);
    let mut release_record = json!({
        "schema_version": 1,
        "resource": format!("slot/{slot}"),
        "generation": lease.generation,
        "actor": actor,
        "authority": authority,
        "handoff_sha256": state["slots"][slot]["owner_sidecar"]["lease"]
            ["automatic_handoff"]["sha256"],
    });
    if quarantined {
        release_record["recovered_quarantined_handoff"] = json!(true);
        release_record["previous_handoff_sha256"] =
            recorded.expect("validated quarantined handoff")["sha256"].clone();
    }
    Ok(release_record)
}

fn stale_clear_all(root: &Path, actor: &str) -> Result<(), String> {
    let candidates = {
        let _lock = lock_registry(root);
        let state = load_state(root);
        state["slots"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(slot, row)| {
                let lease = slot_lease_view(slot, row).ok()?;
                (lease.state != worktree_registry::OwnerLeaseState::Quarantined
                    && lease.expires_at <= epoch_now())
                .then_some((slot.clone(), lease.generation))
            })
            .collect::<Vec<_>>()
    };
    let executable = std::env::current_exe()
        .map_err(|error| format!("resolve release-worktree executable: {error}"))?;
    let mut cleared = 0usize;
    let mut refused = 0usize;
    for (slot, generation) in &candidates {
        let status = Command::new(&executable)
            .args([
                "--slot",
                slot,
                "--clean",
                "--stale-clear",
                "--expected-generation",
                generation,
                "--actor",
                actor,
            ])
            .status()
            .map_err(|error| format!("stale-clear slot {slot}: {error}"))?;
        if status.success() {
            cleared += 1;
        } else {
            refused += 1;
        }
    }
    println!(
        "slot stale-clear census candidates={} cleared={} refused={}",
        candidates.len(),
        cleared,
        refused
    );
    Ok(())
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut slot = String::new();
    let mut agent: Option<String> = None;
    let mut clean = false;
    let mut force = false;
    let mut push = false;
    let mut recover_submodules = false;
    let mut finalize_orphan_residue = false;
    let mut orphan_recovery_note = String::new();
    let mut request_release_flag = false;
    let mut stale_clear = false;
    let mut recover_quarantined_handoff = false;
    let mut stale_clear_all_flag = false;
    let mut expected_generation = String::new();
    let mut actor = String::new();

    let mut i = 0;
    let take = |i: &mut usize, argv: &[String], flag: &str| -> String {
        *i += 1;
        if *i >= argv.len() {
            die(&format!("{flag} requires a value"));
        }
        argv[*i].clone()
    };
    while i < argv.len() {
        match argv[i].as_str() {
            "--slot" => slot = take(&mut i, &argv, "--slot"),
            "--agent" => agent = Some(take(&mut i, &argv, "--agent")),
            "--clean" => clean = true,
            "--force" => force = true,
            "--push" => push = true,
            "--request-release" => request_release_flag = true,
            "--stale-clear" => stale_clear = true,
            "--recover-quarantined-handoff" => recover_quarantined_handoff = true,
            "--stale-clear-all" => stale_clear_all_flag = true,
            "--expected-generation" => {
                expected_generation = take(&mut i, &argv, "--expected-generation")
            }
            "--actor" => actor = take(&mut i, &argv, "--actor"),
            "--recover-submodule-cleanup" => recover_submodules = true,
            "--coordinator-finalize-orphan-residue" => finalize_orphan_residue = true,
            "--orphan-recovery-note" => {
                orphan_recovery_note = take(&mut i, &argv, "--orphan-recovery-note")
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            other => die(&format!("unknown argument: {other}\n\n{USAGE}")),
        }
        i += 1;
    }

    if stale_clear_all_flag {
        if !slot.is_empty()
            || clean
            || force
            || push
            || recover_submodules
            || request_release_flag
            || stale_clear
            || recover_quarantined_handoff
            || !valid_actor(&actor)
        {
            die("--stale-clear-all requires only a non-empty --actor");
        }
        let root = find_root();
        stale_clear_all(&root, &actor)
            .unwrap_or_else(|error| die(&format!("stale-clear-all refused: {error}")));
        return;
    }
    if slot.is_empty() {
        die(&format!("--slot is required\n\n{USAGE}"));
    }
    if !valid_slot(&slot) {
        die(&format!(
            "invalid slot name: '{slot}' (expected a lowercase [a-z0-9-]+ token)"
        ));
    }
    if recover_submodules && (!clean || force) {
        die("--recover-submodule-cleanup requires --clean and is incompatible with --force");
    }
    if request_release_flag
        && (!clean || force || push || recover_submodules || stale_clear || !valid_actor(&actor))
    {
        die("--request-release requires --slot, --clean, and a valid --actor; it is incompatible with --force/--push/--recover-submodule-cleanup/--stale-clear");
    }
    if stale_clear
        && (!clean
            || force
            || push
            || request_release_flag
            || expected_generation.trim().is_empty()
            || !valid_actor(&actor))
    {
        die("--stale-clear requires --clean, --expected-generation, and --actor; it is incompatible with --force/--push/--request-release");
    }
    if recover_quarantined_handoff && !stale_clear {
        die("--recover-quarantined-handoff requires --stale-clear");
    }
    if recover_quarantined_handoff
        && (recover_submodules || finalize_orphan_residue || agent.is_some())
    {
        die("--recover-quarantined-handoff is incompatible with --agent, --recover-submodule-cleanup, and --coordinator-finalize-orphan-residue");
    }
    if finalize_orphan_residue && (!clean || force || push || recover_submodules || agent.is_some())
    {
        die("--coordinator-finalize-orphan-residue requires --clean and is incompatible with --agent, --push, --force, and --recover-submodule-cleanup");
    }
    if finalize_orphan_residue && orphan_recovery_note.trim().is_empty() {
        die("--coordinator-finalize-orphan-residue requires a non-empty --orphan-recovery-note");
    }
    if !finalize_orphan_residue && !orphan_recovery_note.is_empty() {
        die("--orphan-recovery-note is valid only with --coordinator-finalize-orphan-residue");
    }

    let root = find_root();
    if root.join(".wrkslots.yml").is_file() {
        eprintln!(
            "release-worktree: REFUSED -- this project records worktree lifecycle through wrkslots.\n\
             state: REFUSED -- no checkout was salvaged, moved, or removed.\n\
             remedy: run ci-hub/bin/wrkslots remove for a reclaimable slot, or \
             ci-hub/bin/wrkslots recover when an interrupted operation is recorded"
        );
        exit(3);
    }

    // slot-owner-authority takes the registry's shared lock while reading the
    // exact owner row. Run it before taking the writer lock below, then bind
    // its identity and complete lease value to the freshly reloaded state
    // under that writer lock. Calling it while holding the writer lock makes
    // the parent wait on a child that can never acquire its shared lock.
    let stale_clear_authority = if stale_clear {
        Some(
            owner_authority(&root, &slot)
                .unwrap_or_else(|error| die(&format!("stale-clear refused: {error}"))),
        )
    } else {
        None
    };
    let _registry_lock = lock_registry(&root);
    let mut state = load_state(&root);

    if finalize_orphan_residue {
        coordinator_finalize_orphan(&root, &mut state, &slot, &orphan_recovery_note)
            .unwrap_or_else(|error| die(&format!("orphan finalization refused: {error}")));
        return;
    }

    // ⚠️ BEFORE EVERY ORDINARY-RELEASE REFUSAL BELOW, DELIBERATELY. The owner's
    // ruling on 2026-08-27: build output is regenerable by construction, so it
    // can always be reclaimed even from a slot whose record must stay untouched.
    // Those are exactly the slots that leaked -- a release refused for unpushed
    // work or a dead record left its build output on disk forever.
    //
    // An unfinished orphan-residue transaction is the exception. Its manifest
    // binds the fenced bytes, so scrubbing before recovery would change evidence
    // that has already been captured and make the exact retry impossible.
    if state["slots"][&slot]
        .get("orphan_residue_journal")
        .is_none()
    {
        let candidate = root.join("worktrees/slots").join(&slot);
        if candidate.is_dir() {
            let removed = scrub_build_output(&candidate);
            if !removed.is_empty() {
                println!(
                    "release-worktree: reclaimed {} build output director{} under slot {slot}",
                    removed.len(),
                    if removed.len() == 1 { "y" } else { "ies" }
                );
            }
        }
    }

    // ACTIVE.md is a durable, human-augmented projection of the JSON
    // authority, but no filesystem offers an atomic rename spanning both
    // files. Explicit recovery therefore repairs only its managed block under
    // the same exclusive registry lock before invoking any verifier.
    let recovered_active_drift = recover_submodules && regen_active_md(&root, &state);

    if state["slots"].get(&slot).is_none() {
        if recover_submodules && recovered_active_drift {
            verify_absent_slot_recovery(&root, &slot).unwrap_or_else(|error| {
                die(&format!("terminal registry recovery failed: {error}"))
            });
            verify_registry(&root).unwrap_or_else(|error| {
                die(&format!(
                    "terminal registry recovery verification failed: {error}"
                ))
            });
            println!(
                "✓ recovered completed release metadata for {slot}; state and ACTIVE.md now agree"
            );
            return;
        }
        die(&format!(
            "slot {slot} is not registered in worktree-state.json"
        ));
    }
    validate_owners(&slot, &state["slots"][&slot])
        .unwrap_or_else(|error| die(&format!("invalid slot ownership: {error}")));
    let self_release = request_release_flag;
    let self_release_record = if self_release {
        Some(
            request_release(&root, &mut state, &slot, &actor)
                .unwrap_or_else(|error| die(&format!("release request refused: {error}"))),
        )
    } else {
        None
    };
    let stale_clear_record = if stale_clear {
        let record = prepare_stale_clear(
            &root,
            &mut state,
            &slot,
            &expected_generation,
            &actor,
            stale_clear_authority
                .as_ref()
                .expect("stale-clear authority is prepared before the writer lock"),
            recover_quarantined_handoff,
        )
        .unwrap_or_else(|error| die(&format!("stale-clear refused: {error}")));
        if state["slots"][&slot]
            .get("coordinator_lease_revocation")
            .is_some()
        {
            recover_submodules = true;
        }
        Some(record)
    } else {
        None
    };
    let release_record = self_release_record.or(stale_clear_record);
    if state["slots"][&slot]
        .get("coordinator_lease_intent")
        .is_some()
    {
        die("slot has an unfinished coordinator-sentinel launch intent; complete the exact allocator invocation before release");
    }
    if let Some(orphan_journal) = state["slots"][&slot].get("orphan_residue_journal") {
        let recorded_note = orphan_journal["recovery_note"]
            .as_str()
            .unwrap_or("<unreadable recorded note>");
        die(&format!(
            "slot {slot} has an unfinished orphan-residue journal; rerun ./scripts/release-worktree.rs --slot {slot} --clean --coordinator-finalize-orphan-residue with the exact original --orphan-recovery-note {recorded_note:?}"
        ));
    }
    if (state["slots"][&slot]["status"].as_str() == Some("releasing")
        || state["slots"][&slot].get("release_journal").is_some()
        || state["slots"][&slot]
            .get("coordinator_lease_revocation")
            .is_some())
        && !recover_submodules
    {
        die(&format!(
            "slot {slot} has an unfinished release journal or coordinator revocation journal; retain it and run --clean --recover-submodule-cleanup"
        ));
    }
    if recover_submodules && agent.is_some() {
        die("--recover-submodule-cleanup cannot be combined with --agent");
    }

    // Resolve every test-only crash hook before the first release mutation.
    // A leaked or malformed test path must never surface only after Git has
    // already removed a worktree.
    let journal_arm_crash_marker =
        restricted_fixture_path(&root, "HERMIT_RELEASE_TEST_CRASH_AFTER_JOURNAL_ARM_STATE")
            .unwrap_or_else(|error| die(&format!("journal-arm crash hook refused: {error}")));
    let journal_clear_crash_marker =
        restricted_fixture_path(&root, "HERMIT_RELEASE_TEST_CRASH_AFTER_JOURNAL_CLEAR_STATE")
            .unwrap_or_else(|error| die(&format!("journal-clear crash hook refused: {error}")));
    let final_state_crash_marker =
        restricted_fixture_path(&root, "HERMIT_RELEASE_TEST_CRASH_AFTER_FINAL_STATE")
            .unwrap_or_else(|error| die(&format!("final-state crash hook refused: {error}")));
    let git_remove_crash_marker =
        restricted_fixture_path(&root, "HERMIT_RELEASE_TEST_CRASH_AFTER_GIT_REMOVE")
            .unwrap_or_else(|error| die(&format!("Git-remove crash hook refused: {error}")));
    let sentinel_arm_crash_marker = restricted_fixture_path(
        &root,
        "HERMIT_RELEASE_TEST_CRASH_AFTER_SENTINEL_REVOCATION_ARM",
    )
    .unwrap_or_else(|error| die(&format!("sentinel-arm crash hook refused: {error}")));
    let sentinel_stop_crash_marker =
        restricted_fixture_path(&root, "HERMIT_RELEASE_TEST_CRASH_AFTER_SENTINEL_STOP")
            .unwrap_or_else(|error| die(&format!("sentinel-stop crash hook refused: {error}")));

    if state["slots"][&slot].get("coordinator_lease").is_some() && !clean {
        die("Codex coordinator leases can only be released with --clean so exact revocation and global absence proofs remain coupled");
    }
    if state["slots"][&slot].get("coordinator_lease").is_some() && agent.is_some() {
        die("cannot drop one recorded agent from a coordinator-leased slot; release the whole slot through exact sentinel revocation");
    }

    // If dropping one sharer and others remain, do not tear down the slot.
    if let Some(name) = &agent {
        let agents = state["slots"][&slot]["agents"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let remaining: Vec<Value> = agents
            .into_iter()
            .filter(|a| a["name"].as_str() != Some(name.as_str()))
            .collect();
        if !remaining.is_empty() {
            let mut candidate = state["slots"][&slot].clone();
            candidate["agents"] = json!(remaining.clone());
            validate_owners(&slot, &candidate).unwrap_or_else(|error| {
                die(&format!("refusing invalid remaining ownership: {error}"))
            });
            let n = remaining.len();
            state["slots"][&slot]["agents"] = json!(remaining);
            state["slots"][&slot]["updated"] = json!(now_iso());
            save_state(&root, &mut state);
            regen_active_md(&root, &state);
            println!(
                "✓ dropped agent '{name}' from {slot}; {n} agent(s) remain, slot still active"
            );
            return;
        }
        println!("agent '{name}' was the last owner of {slot}; releasing whole slot");
    }

    let slot_dir = root.join("worktrees/slots").join(&slot);
    if !clean {
        for label in ["hermit", "reverie", "liteinst2"] {
            let path = slot_dir.join(label);
            if !path.exists() {
                continue;
            }
            match git_inspect(
                &path,
                &["status", "--porcelain=v1", "--untracked-files=all"],
            ) {
                Ok(status) if !status.is_empty() => {
                    eprintln!("⚠  retained {label} worktree has uncommitted work:\n{status}")
                }
                Err(error) => eprintln!("⚠  could not inspect retained {label} worktree: {error}"),
                _ => {}
            }
        }
    }
    if clean {
        let proc_root = process_root(&root)
            .unwrap_or_else(|error| die(&format!("process inspection setup failed: {error}")));
        let cgroup_root = cgroup_root(&root)
            .unwrap_or_else(|error| die(&format!("cgroup inspection setup failed: {error}")));
        let (_container_lifecycle_lock, container_fence_token) = lock_container_lifecycle(&root)
            .unwrap_or_else(|error| {
                die(&format!("container lifecycle fence unavailable: {error}"))
            });
        let mut owners = recorded_owner_authorities(&root, &slot, &state["slots"][&slot])
            .unwrap_or_else(|error| die(&format!("owner lease refused cleanup: {error}")));
        let mut recovered_revocation = false;
        if recover_submodules
            && state["slots"][&slot]
                .get("coordinator_lease_revocation")
                .is_some()
        {
            complete_coordinator_lease_revocation(
                &root,
                &mut state,
                &slot,
                true,
                sentinel_stop_crash_marker.as_deref(),
            )
            .unwrap_or_else(|error| {
                die(&format!(
                    "coordinator lease revocation recovery failed: {error}"
                ))
            });
            recovered_revocation = true;
            owners = recorded_owner_authorities(&root, &slot, &state["slots"][&slot])
                .unwrap_or_else(|error| die(&format!("revoked owner lease invalid: {error}")));
        }
        if recover_submodules
            && state["slots"][&slot].get("release_journal").is_some()
            && owners
                .sentinel
                .as_ref()
                .is_some_and(|sentinel| !sentinel.revoked)
        {
            die("release journal exists for a live Codex sentinel without its required prior revocation journal");
        }
        let mut recovered_path_fence = false;
        if recover_submodules {
            if let Some((journaled, fenced)) = journal_target(&root, &state, &slot)
                .unwrap_or_else(|error| die(&format!("release journal recovery failed: {error}")))
            {
                let recovery_paths = [journaled.path.clone(), fenced];
                let (users, _) = live_process_users(
                    &root,
                    &proc_root,
                    &cgroup_root,
                    &container_fence_token,
                    &owners,
                    &recovery_paths,
                    self_release,
                )
                .unwrap_or_else(|error| {
                    die(&format!("release journal owner proof failed: {error}"))
                });
                if !users.is_empty() {
                    die(&format!(
                        "release journal remains in use; refusing recovery: {}",
                        users.join(", ")
                    ));
                }
                match recover_path_fence(&root, &state, &slot)
                    .unwrap_or_else(|error| die(&format!("path-fence recovery failed: {error}")))
                {
                    PathFenceRecovery::None => {}
                    PathFenceRecovery::Restored => recovered_path_fence = true,
                    PathFenceRecovery::RemovalCompleted(target) => {
                        state["slots"][&slot][target.branch_key] = json!("-");
                        clear_release_journal(
                            &root,
                            &mut state,
                            &slot,
                            journal_clear_crash_marker.as_deref(),
                        )
                        .unwrap_or_else(|error| {
                            die(&format!(
                                "completed-removal registry recovery interrupted: {error}"
                            ))
                        });
                        recovered_path_fence = true;
                    }
                }
            }
        }
        println!("release cleanup scope: slot={slot}; unrelated registry rows are not inspected");
        verify_registry_slot(&root, &slot)
            .unwrap_or_else(|error| die(&format!("scoped clean preflight failed: {error}")));
        let clean_targets = clean_targets_from_state(&root, &state, &slot)
            .unwrap_or_else(|error| die(&format!("clean preflight failed: {error}")));
        let preflight_target_paths: Vec<PathBuf> = clean_targets
            .iter()
            .map(|target| target.path.clone())
            .collect();
        if let Some(sentinel) = &owners.sentinel {
            if sentinel.revoked {
                let (preflight_users, _) = live_process_users(
                    &root,
                    &proc_root,
                    &cgroup_root,
                    &container_fence_token,
                    &owners,
                    &preflight_target_paths,
                    self_release,
                )
                .unwrap_or_else(|error| die(&format!("owner authority refused cleanup: {error}")));
                if !preflight_users.is_empty() {
                    die(&format!(
                        "refusing --clean while live processes use the slot: {}",
                        preflight_users.join(", ")
                    ));
                }
            } else {
                verify_codex_sentinel_live(&root, &sentinel.lease).unwrap_or_else(|error| {
                    die(&format!(
                        "live Codex coordinator lease refused cleanup: {error}"
                    ))
                });
            }
        } else {
            let (preflight_users, _) = live_process_users(
                &root,
                &proc_root,
                &cgroup_root,
                &container_fence_token,
                &owners,
                &preflight_target_paths,
                self_release,
            )
            .unwrap_or_else(|error| die(&format!("owner authority refused cleanup: {error}")));
            if !preflight_users.is_empty() {
                die(&format!(
                    "refusing --clean while live processes use the slot: {}",
                    preflight_users.join(", ")
                ));
            }
        }
        let mut proofs: Vec<(&'static str, Vec<RepoSnapshot>)> = Vec::new();
        let mut any_dirty = false;
        let mut recovered_any =
            recovered_path_fence || recovered_active_drift || recovered_revocation;

        for target in &clean_targets {
            if recover_submodules {
                recovered_any |= recover_submodule_cleanup(target).unwrap_or_else(|error| {
                    die(&format!(
                        "submodule cleanup recovery failed for {}: {error}",
                        target.label
                    ))
                });
            } else {
                ensure_no_submodule_cleanup_artifacts(target).unwrap_or_else(|error| {
                    die(&format!(
                        "clean preflight failed for {}: {error}",
                        target.label
                    ))
                });
            }
            let inspections = inspect_target_repositories(&target.path).unwrap_or_else(|error| {
                die(&format!(
                    "could not inspect {} target recursively: {error}",
                    target.label
                ))
            });
            for inspection in &inspections {
                if !inspection.status.is_empty() {
                    any_dirty = true;
                    eprintln!(
                        "⚠  repository {} has uncommitted work:",
                        inspection.snapshot.path.display()
                    );
                    for line in inspection.status.lines() {
                        eprintln!("     {line}");
                    }
                }
            }
            proofs.push((target.label, snapshots(&inspections)));
        }
        if recover_submodules && !recovered_any {
            die("--recover-submodule-cleanup found no interrupted transaction marker");
        }
        if any_dirty {
            eprintln!(
                "⚠  {slot} has uncommitted changes. Commit and push to a feature branch first."
            );
            if !force {
                die("refusing --clean with uncommitted work; pass --force to override");
            }
        }

        eprintln!(
            "pre-recycle guardrail: verifying every outer and initialized nested HEAD is reachable from its own origin..."
        );
        let mut at_risk = Vec::new();
        for (_, repositories) in &proofs {
            for repository in repositories {
                match remote_durable(repository) {
                    Ok(true) => {}
                    Ok(false) => {
                        eprintln!(
                            "⚠  {}: HEAD {} is not reachable from any current origin branch",
                            repository.path.display(),
                            &repository.head[..repository.head.len().min(12)]
                        );
                        at_risk.push(repository.path.clone());
                    }
                    Err(error) => die(&format!("origin durability inspection failed: {error}")),
                }
            }
        }

        if !at_risk.is_empty() && push {
            println!("--push: pushing at-risk repositories that have branches (with-proxy)...");
            for repository in &at_risk {
                push_branch(repository).unwrap_or_else(|error| {
                    die(&format!(
                        "could not inspect/push {}: {error}",
                        repository.display()
                    ))
                });
            }
            at_risk.retain(|path| {
                let proof = proofs
                    .iter()
                    .flat_map(|(_, repositories)| repositories)
                    .find(|repository| repository.path == *path)
                    .expect("at-risk repository must have a proof");
                match remote_durable(proof) {
                    Ok(durable) => !durable,
                    Err(error) => die(&format!("post-push durability inspection failed: {error}")),
                }
            });
        }
        if !at_risk.is_empty() {
            let paths = at_risk
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            if !force {
                die(&format!(
                    "REFUSING to release {slot}: committed work not on origin ({paths}). \
                     Pass --push to push branch checkouts, or --force to discard the durability guarantee."
                ));
            }
            eprintln!("⚠  --force: releasing {slot} despite work not on origin ({paths}).");
        } else {
            let count: usize = proofs
                .iter()
                .map(|(_, repositories)| repositories.len())
                .sum();
            eprintln!("✓ verified {count} outer/nested repository HEAD(s) on origin");
        }

        // Retain the armed journal until the exact scoped target has passed
        // both uncommitted-work and remote-durability checks. A refusal above
        // therefore cannot turn ambiguous/local-only work into an unjournaled
        // ordinary slot merely because path restoration succeeded.
        if recover_submodules && state["slots"][&slot].get("release_journal").is_some() {
            clear_release_journal(
                &root,
                &mut state,
                &slot,
                journal_clear_crash_marker.as_deref(),
            )
            .unwrap_or_else(|error| die(&format!("release journal recovery interrupted: {error}")));
            verify_registry_slot(&root, &slot).unwrap_or_else(|error| {
                die(&format!(
                    "scoped post-recovery registry verification failed: {error}"
                ))
            });
        }

        let target_paths: Vec<PathBuf> = clean_targets
            .iter()
            .map(|target| target.path.clone())
            .collect();
        if owners
            .sentinel
            .as_ref()
            .is_some_and(|sentinel| !sentinel.revoked)
        {
            arm_coordinator_lease_revocation(
                &root,
                &mut state,
                &slot,
                sentinel_arm_crash_marker.as_deref(),
            )
            .unwrap_or_else(|error| {
                die(&format!("coordinator lease revocation arm failed: {error}"))
            });
            complete_coordinator_lease_revocation(
                &root,
                &mut state,
                &slot,
                false,
                sentinel_stop_crash_marker.as_deref(),
            )
            .unwrap_or_else(|error| die(&format!("coordinator lease revocation failed: {error}")));
            owners = recorded_owner_authorities(&root, &slot, &state["slots"][&slot])
                .unwrap_or_else(|error| die(&format!("revoked owner lease invalid: {error}")));
        }
        let (users, _) = live_process_users(
            &root,
            &proc_root,
            &cgroup_root,
            &container_fence_token,
            &owners,
            &target_paths,
            self_release,
        )
        .unwrap_or_else(|error| die(&format!("live-process inspection failed: {error}")));
        if !users.is_empty() {
            die(&format!(
                "refusing --clean while live processes use the slot: {}",
                users.join(", ")
            ));
        }

        for target in &clean_targets {
            let expected_repositories = proofs
                .iter()
                .find(|(label, _)| *label == target.label)
                .map(|(_, repositories)| repositories.as_slice())
                .expect("clean target must have recursive repository proofs");
            final_removal_boundary(
                &root,
                &proc_root,
                &cgroup_root,
                &container_fence_token,
                &owners,
                &slot,
                &state["slots"][&slot],
                target,
                expected_repositories,
                force,
                self_release,
            )
            .unwrap_or_else(|error| {
                die(&format!(
                    "final removal boundary refused {}: {error}",
                    target.label
                ))
            });

            let fenced = arm_release_journal(
                &root,
                &mut state,
                &slot,
                target,
                journal_arm_crash_marker.as_deref(),
            )
            .unwrap_or_else(|error| die(&format!("release journal arm interrupted: {error}")));

            remove_target(
                &root,
                target,
                &fenced,
                expected_repositories,
                &proc_root,
                &cgroup_root,
                &container_fence_token,
                &owners,
                force,
                self_release,
            )
            .unwrap_or_else(|error| die(&format!("{error}; registry state retained")));
            mark_git_removal_complete(&root, &mut state, &slot);
            maybe_inject_fixture_crash(
                git_remove_crash_marker.as_deref(),
                "post-git-remove crash injected",
            )
            .unwrap_or_else(|error| die(&format!("Git removal completion interrupted: {error}")));
            println!(
                "  removed {} worktree {}",
                target.label,
                target.path.display()
            );

            // Record each exact product removal before attempting the next one,
            // so a later failure is retryable without lying about physical state.
            state["slots"][&slot][target.branch_key] = json!("-");
            clear_release_journal(
                &root,
                &mut state,
                &slot,
                journal_clear_crash_marker.as_deref(),
            )
            .unwrap_or_else(|error| die(&format!("release journal clear interrupted: {error}")));
        }

        if slot_dir.exists() {
            fs::remove_dir(&slot_dir).unwrap_or_else(|error| {
                die(&format!(
                    "could not remove now-empty exact slot directory {}: {error}; slot state retained",
                    slot_dir.display()
                ))
            });
        }
        if let Some(mut record) = release_record.clone() {
            record["cleared_at"] = json!(now_iso());
            let history = state
                .as_object_mut()
                .expect("registry state is an object")
                .entry("lease_history")
                .or_insert_with(|| json!([]));
            history
                .as_array_mut()
                .unwrap_or_else(|| die("lease_history is not an array"))
                .push(record);
        }
        state["slots"].as_object_mut().unwrap().remove(&slot);
        println!("✓ released and cleaned {slot} (removed from state)");
    } else {
        state["slots"][&slot]["status"] = json!("released");
        state["slots"][&slot]["updated"] = json!(now_iso());
        clear_pane_leases(&mut state["slots"][&slot]);
        println!("✓ released {slot} (worktree retained; marked 'released' in state)");
        println!("  run again with --clean to remove the physical worktree");
    }

    save_state(&root, &mut state);
    if clean {
        maybe_inject_fixture_crash(
            final_state_crash_marker.as_deref(),
            "post-final-state state/ACTIVE split injected",
        )
        .unwrap_or_else(|error| die(&format!("final registry update interrupted: {error}")));
    }
    regen_active_md(&root, &state);
    println!("  state:  {}", state_path(&root).display());
    println!("  active: {}", root.join("worktrees/ACTIVE.md").display());

    // Advisory post-mutation reconciliation check: call the canonical verifier
    // rather than trusting our own write. Endemic fleet drift means this is
    // advisory (never blocks the release); a FAIL points at
    // `allocate-worktree.rs --repair` to reconcile recorded branch cells.
    verify_registry_advisory(&root);
}

/// Run the canonical registry verifier and report its verdict without failing.
/// Every registry mutator routes through this one predicate instead of
/// re-parsing ACTIVE.md / worktree-state.json itself.
fn verify_registry_advisory(root: &Path) {
    let checker = root.join("scripts/check-worktree-registry.rs");
    if !checker.exists() {
        return;
    }
    if let Err(error) = verify_registry(root) {
        eprintln!(
            "note: worktree registry has drift after this release ({error}); run \
             `scripts/allocate-worktree.rs --repair` to reconcile (advisory, not a failure)."
        );
    }
}
