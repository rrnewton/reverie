#!/usr/bin/env rust-script
//! Allocate a worktree slot for a dev-hermit agent, enforcing worktree discipline.
//!
//! CANONICAL LAYOUT (nested, one slot per agent):
//!
//!   worktrees/slots/<slot>/hermit    Hermit worktree  (from the hermit/ primary)
//!   worktrees/slots/<slot>/reverie   Reverie worktree (from the reverie/ primary)
//!   worktrees/slots/<slot>/liteinst2 LiteInst2 worktree (from the liteinst2/ primary)
//!
//! `<slot>` is either a NAMED agent slot (e.g. worktrees/slots/kvm,
//! worktrees/slots/dbi for
//! the purpose-fixed agents hermit-kvm, hermit-dbi, ...) or a generic slotNN
//! (worktrees/slots/slot01, ...). Each agent owns exactly ONE slot (1-1 mapping). A
//! New slots contain all three product worktrees by default. A specialized
//! allocation may request an individual product or the legacy Hermit+Reverie
//! pair (`both`).
//!
//! This is the ONLY sanctioned way to create an agent worktree. It refuses to
//! let two mutating agents share a slot, records ownership in the machine-
//! readable `worktree-state.json`, and regenerates the machine-parseable table
//! block in `worktrees/ACTIVE.md`.
//!
//! ```cargo
//! [dependencies]
//! fs2 = "0.4"
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! ```
#[allow(dead_code)]
#[path = "agent_snapshot.rs"]
mod agent_snapshot;
#[allow(dead_code)]
#[path = "worktree_registry.rs"]
mod worktree_registry;

use fs2::FileExt;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{exit, Command};
use std::time::{SystemTime, UNIX_EPOCH};

/// Commits behind origin/main past which the base-distance notice fires. Small
/// on purpose: main moves fast here and the damage from a stale base is silent,
/// so the notice should appear well before the tree is badly out of date.
const BASE_DISTANCE_NOTICE: u64 = 20;

/// Workspace homeostasis caps. Disk is the primary advisory (expressed in GB);
/// the slot count is a secondary advisory against the active-worktree policy
/// limit. Override the disk cap with HERMIT_WORKTREE_GB_CAP.
///
/// UNIT: this cap is compared against `du -sb` (see `dir_size_bytes`), i.e. the
/// APPARENT / pre-compression / reflink-referenced logical size of worktrees/.
/// btrfs zstd is transparent to `du`, so `du -sb` overstates the real on-disk
/// footprint. Measured 2026-08-03 on this box (compsize over worktrees/):
///   du -sb (apparent, referenced) : actual disk = 639.7 GB : 163 GB = 3.92 : 1
/// so this apparent cap corresponds to ~1/3.9 as much real disk.
///
/// DERIVATION (do not "round" — recompute from measurements when the box or the
/// slot budget changes). Three recorded inputs, all in the apparent/`du -sb`
/// unit the check uses:
///   measured_per_tree = 95 GB  (mean of the 5 heaviest fully-built trees
///                               measured 2026-08-03: 54/66/87/110/152 GB ≈ 94,
///                               taken as 95 for margin; PRE-compression apparent)
///   target_count      = 12     (active-worktree policy limit, Hard Invariant 13)
///   headroom          = 1.25   (parked slots + build growth)
///   cap = 95 * 12 * 1.25 = 1425 GB apparent
/// Real-disk sanity: 1425 GB apparent / 3.92 ≈ 363 GB actual (≈475 GB even at a
/// conservative 3:1), vs 2158 GB free here — ~17-22% of free at the cap.
const DEFAULT_DISK_CAP_GB: u64 = 1425;
const SLOT_COUNT_ADVISORY: usize = 12;
const LANGUISH_HOURS: i64 = 24;
const OWNER_SIDECAR_SCHEMA_VERSION: u64 = 1;
const OWNER_LEASE_SCHEMA_VERSION: u64 = 1;
const OWNER_LEASE_TTL_SECONDS: i64 = 86_400;
const ORC_OWNER_SIDECAR_SOURCE: &str = "orc-tmux-pane-supervisor-v1";
const CODEX_OWNER_SIDECAR_SOURCE: &str = "codex-systemd-sentinel-v1";

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedOwnerLease {
    pane: String,
    cgroup: String,
    supervisor_pid: u32,
    boot_id: String,
    start_ticks: u64,
}

const USAGE: &str = r#"Usage: allocate-worktree.rs --agent NAME [OPTIONS]

Allocate a worktree slot (nested layout
worktrees/slots/<slot>/{hermit,reverie,liteinst2}) and register its ownership. Each
agent owns exactly one slot.

Required:
  --agent NAME        Owning agent (e.g. hermit-kvm). Mutating owner of the slot.

Options:
  --slot SLOT         Slot name. Default: the agent name with a leading
                      'hermit-' stripped (hermit-kvm -> worktrees/slots/kvm), or a
                      generic slotNN if the agent name is not a clean token.
                      Accepts a named token ([a-z0-9-]+) or slotNN.
  --task TASK-ID      Task this slot serves (recorded in state + ACTIVE.md).
  --product P         hermit | reverie | liteinst2 | both | all
                      (default: all; both means Hermit + Reverie).
  --hermit-branch B   Create this feature branch in the hermit worktree.
  --reverie-branch B  Create this feature branch in the reverie worktree.
  --liteinst2-branch B
                      Create this feature branch in the liteinst2 worktree.
  --start-point REF   Base for detached/new worktrees (default: primary HEAD).
  --purpose TEXT      One-line purpose recorded in state + ACTIVE.md.
  --i-promise-this-agent-is-read-mostly
                      Join an already-owned slot as an ADDITIONAL read-only
                      agent instead of failing the collision check.
  --codex-systemd-sentinel
                      Create or verify the coordinator-held per-slot Codex
                      sentinel lease. This is slot coordination authority, not
                      evidence that a Codex thread is live.
  --recover-legacy-unbound-owner
                      With --codex-systemd-sentinel, preserve an existing
                      lease-less slot's historical owner evidence and bind a
                      new coordinator recovery generation. Never rebinds the
                      current thread as the historical owner.
  --recovery-note TEXT
                      Required with --recover-legacy-unbound-owner; durable
                      coordinator reason for the binding-only migration.
  --check-only        Run registry branch/content consistency plus workspace
                      homeostasis, then exit WITHOUT allocating anything.
  --migrate-layout    Atomically rewrite exact legacy worktrees/<slot>/<product>
                      registry paths to worktrees/slots/<slot>/<product> and
                      regenerate the managed ACTIVE.md block. No worktree moves.
  --repair            Reconcile recorded branch cells from physical checkouts.
  --dry-run           With --repair or --migrate-layout, report changes only.
  --exclude-slot SLOT With --repair, explicitly exclude a named slot. May be
                      repeated; exclusions are printed and never inspected.
                      Alternatively, --repair --slot SLOT repairs one slot.
  -h, --help          Show this help.

Homeostasis (ADVISORY, never blocks — allocation always completes): every
allocation also prints an informational banner if worktrees/ apparent size
(du -sb) exceeds the GB soft cap (env HERMIT_WORKTREE_GB_CAP, default 1425 GB
apparent; note du -sb overstates real btrfs on-disk use ~3.9x here), if any
physical slot has had no file edits in >24h (registered or orphaned), or if slot
dirs exceed a soft count. These are housekeeping heads-ups, not stop signs; your
slot is allocated and you should proceed.

Policy:
  * One mutating owner per slot; a second mutating agent is refused.
  * One slot per agent; if the agent already owns a different slot, refused.
  * Read-mostly agents may share and are recorded as such.
  * After an owner starts or is replaced, re-run the same allocation to adopt
    its exact pane/cgroup lease; release refuses legacy or unbound ownership.
  * Codex coordination uses an explicit per-slot systemd sentinel instead of
    shared thread/pane identity. Re-adoption verifies the same generation and
    never silently replaces it.

Examples:
  ./scripts/allocate-worktree.rs --agent hermit-kvm --task impl-kvm-ratchet
      -> worktrees/slots/kvm/{hermit,reverie,liteinst2}
  ./scripts/allocate-worktree.rs --agent hermit-201 --slot slot01 \
      --product hermit --hermit-branch debug-batch1 --task impl-debug
  ./scripts/allocate-worktree.rs --agent hermit-ci --slot kvm \
      --i-promise-this-agent-is-read-mostly --task research-ci
"#;

fn die(msg: &str) -> ! {
    eprintln!("allocate-worktree: {msg}");
    exit(1);
}

/// Walk up from CWD to the dev-hermit parent (all three primary submodules).
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

/// Hold the single registry-writer authority across physical worktree and
/// state/ACTIVE mutations. The releaser takes this same lock.
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

/// Reap only leases whose canonical release adapter can prove eligible.
/// Refusals are expected (for example, an expired TTL with a live holder) and
/// never weaken allocation's own capacity checks.
fn reap_expired_slot_leases(root: &Path, agent: &str) {
    let actor = format!("allocator:{agent}");
    let release = root.join("scripts/release-worktree.rs");
    match Command::new(&release)
        .args(["--stale-clear-all", "--actor", actor.as_str()])
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!(
            "WARN: expired slot-lease reaper exited {:?}; allocation remains fail-closed on the recorded census",
            status.code()
        ),
        Err(error) => eprintln!(
            "WARN: expired slot-lease reaper unavailable at {}: {error}; allocation remains fail-closed on the recorded census",
            release.display()
        ),
    }
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

/// Run a git command in `dir`; return (success, stdout, stderr).
fn git(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| die(&format!("failed to spawn git: {e}")));
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// True when any of the slot's recorded product checkouts has uncommitted work.
/// FAILS CLOSED: an unreadable checkout, or a `git status` that errors, counts
/// as dirty. Absent children (`-`) are skipped, not treated as clean evidence.
fn slot_has_uncommitted_work(root: &Path, record: &Value) -> bool {
    for key in ["hermit_path", "reverie_path", "liteinst2_path"] {
        let Some(rel) = record.get(key).and_then(|v| v.as_str()) else {
            continue;
        };
        if rel.is_empty() || rel == "-" {
            continue;
        }
        let path = root.join(rel);
        if !path.join(".git").exists() {
            continue;
        }
        let (ok, out, _) = git(&path, &["status", "--porcelain"]);
        if !ok || !out.is_empty() {
            return true;
        }
    }
    false
}

/// A scoped repair may change only registry strings, but it must not normalize
/// away the only pointer to unpublished checkout state. Prove the exact current
/// checkout is clean and its HEAD is reachable from at least one remote ref
/// before changing that product's recorded branch cell.
fn verify_repair_checkout_published(
    root: &Path,
    slot: &str,
    product: &str,
    actual: &str,
) -> Result<(), String> {
    if actual == "-" {
        return Err(format!(
            "{slot}/{product}: checkout is absent; unpublished state cannot be proven"
        ));
    }
    let path = root.join(format!("worktrees/slots/{slot}/{product}"));
    exact_checkout_root(&path)
        .map_err(|error| format!("{slot}/{product}: exact checkout unavailable: {error}"))?;
    let (status_ok, status, status_error) = git(
        &path,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    if !status_ok {
        return Err(format!(
            "{slot}/{product}: could not inspect uncommitted work: {status_error}"
        ));
    }
    if !status.is_empty() {
        return Err(format!(
            "{slot}/{product}: uncommitted work is present ({})",
            status.lines().count()
        ));
    }
    let (rev_ok, count, rev_error) = git(
        &path,
        &["rev-list", "--count", "HEAD", "--not", "--remotes"],
    );
    if !rev_ok {
        return Err(format!(
            "{slot}/{product}: could not prove remote containment: {rev_error}"
        ));
    }
    let unpushed = count
        .parse::<u64>()
        .map_err(|_| format!("{slot}/{product}: invalid unpushed-commit count {count:?}"))?;
    if unpushed != 0 {
        return Err(format!(
            "{slot}/{product}: {unpushed} commit(s) are reachable from HEAD but no remote ref"
        ));
    }
    Ok(())
}

/// Whether a recorded branch cell is an intent that was asked for but never created.
///
/// `--repair` reconciles the RECORD to the CHECKOUT. That is right when the record holds a stale
/// branch name and the checkout is the current truth. It is WRONG in one specific case, and the
/// two are the same observation (`recorded != actual`) to everything upstream of here:
///
///   * STALE RECORD. The slot was repurposed, the recorded branch is a real prior branch, and the
///     checkout has already moved on. Reconciling registry->disk is correct, and the `drift70`
///     fixture brackets it.
///   * UNFULFILLED INTENT. Re-allocating an already-owned slot writes the NEW branch into the
///     registry but does not create it in the checkout, which is still on the OLD branch. Here
///     reconciling registry->disk records the previous task's branch as correct, so the divergence
///     becomes self-consistent and the verifier goes quiet. The record is fixed; the intent is lost.
///
/// The test is whether the recorded name resolves as a ref in that product's checkout. A branch
/// that was asked for and never created does not.
///
/// BE HONEST ABOUT WHAT THIS TEST IS. It is a SUPERSET, not a separator. From disk state alone the
/// two cases above are indistinguishable, because a stale record can also name a branch that was
/// deleted after the slot moved on, and that does not resolve either. This refuses that case too,
/// deliberately: when the recorded name resolves nowhere, the recorded string is the LAST REMAINING
/// POINTER to it, and overwriting it destroys the name. That is the same thing the "unpushed local
/// refs survive" guarantee above protects, one level up.
///
/// IT IS ALSO NOT COMPLETE. If the branch the re-allocation asked for happens to already resolve --
/// created by an earlier allocation, or by hand -- the hazard is real and this does not catch it.
/// The narrower shape where a re-allocation keeps the same branch NAME and only the head is stale
/// is not drift at all (recorded == actual) and is outside repair's reach entirely.
///
/// HOW MUCH THAT INCOMPLETENESS COSTS, MEASURED RATHER THAN ESTIMATED. Against the live registry on
/// 2026-08-15 the four slots then showing `recorded != actual` -- cadence2/hermit, ci/hermit,
/// claude-fixtureassert/hermit, fix-futex/hermit -- ALL FOUR had a recorded branch that RESOLVES, so
/// this refusal fires on NONE of them. Read that in both directions: it is the evidence that the
/// change does not disable repair in practice, AND it is the evidence that on the population
/// actually present, this catches nothing. If a real re-allocation hazard existed in those four, the
/// ref-existence test would not have found it.
///
/// This deliberately does NOT try to correct toward the intent: doing so would need either a
/// `git branch`/`checkout` (forbidden above) or a rewrite of ownership (also forbidden above,
/// because repurposing intent lives with a human). So the answer is to refuse and leave the cell
/// alone, which keeps the drift VISIBLE to `check-worktree-registry.rs` rather than teaching
/// anything to tolerate it.
fn recorded_branch_is_unfulfilled_intent(
    root: &Path,
    slot: &str,
    product: &str,
    recorded: &str,
) -> bool {
    // "-" is the absent-product placeholder and "detached"/"detached:<sha>" is a state, not a
    // branch that could have been asked for. Neither can be an unfulfilled intent.
    if recorded == "-" || recorded.is_empty() || recorded.starts_with("detached") {
        return false;
    }
    let path = root.join(format!("worktrees/slots/{slot}/{product}"));
    // An unreadable checkout is already skipped upstream; be conservative here and do not claim
    // an intent we cannot check for.
    if exact_checkout_root(&path).is_err() {
        return false;
    }
    let (resolved, _, _) = git(
        &path,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{recorded}"),
        ],
    );
    !resolved
}

/// Whether a `coordinator_lease_revocation` journal should still block `--repair`.
///
/// The refusal list this feeds is an IN-FLIGHT TRANSACTION guard, not an audit
/// registry: its siblings `release_journal` and `orphan_residue_journal` delete
/// themselves on completion, and the durable `coordinator_lease` is deliberately
/// excluded. The hazard it protects against is a checkout fenced aside mid-release,
/// which repair must not reconcile into `branch="-"` (see the RELEASE-JOURNAL
/// ISOLATION fixture in scripts/allocate-worktree-repair-test.sh).
///
/// A revocation cannot create that hazard at all — `arm_coordinator_lease_revocation`
/// stops a systemd unit and moves no file. Once `complete_coordinator_lease_revocation`
/// records `phase="revoked"` with its proof of death, the transaction is over.
/// Blocking on the mere PRESENCE of the key made every completed revocation a
/// permanent, undischargeable veto over repair for the whole registry.
///
/// NOTHING IS ERASED HERE. The journal, its lease identity and its proof all
/// remain in the record; repair merely stops mistaking a finished transaction for
/// a running one. The clean-tree condition is deliberately stricter than the
/// mechanism requires — repair only rewrites recorded branch cells and never
/// touches a file — because a completed revocation sitting on top of uncommitted
/// work is an anomaly that should stay visible rather than be reconciled away.
fn revocation_journal_blocks_repair(root: &Path, record: &Value) -> bool {
    let Some(journal) = record.get("coordinator_lease_revocation") else {
        return false;
    };
    if journal["phase"].as_str() != Some("revoked") {
        return true;
    }
    slot_has_uncommitted_work(root, record)
}

fn epoch_now() -> i64 {
    Command::new("date")
        .args(["+%s"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0)
}

/// Convert an ISO-8601 timestamp to epoch seconds via `date -d`.
fn epoch_of(iso: &str) -> Option<i64> {
    let out = Command::new("date")
        .args(["-d", iso, "+%s"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Total size of a directory in bytes via `du -sb`.
fn dir_size_bytes(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    Command::new("du")
        .arg("-sb")
        .arg(path)
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(0)
}

/// Newest modification time (epoch seconds) of a *source* file anywhere under
/// `dir`, ignoring build/VCS churn (target/, .git/, node_modules/). This is the
/// real "when was this slot last edited" signal — unlike the state timestamp,
/// which only moves when allocate-worktree runs. Returns 0 when nothing is found.
fn newest_source_mtime(dir: &Path) -> i64 {
    if !dir.exists() {
        return 0;
    }
    let out = Command::new("find")
        .arg(dir)
        .args([
            "(",
            "-name",
            "target",
            "-o",
            "-name",
            ".git",
            "-o",
            "-name",
            "node_modules",
            ")",
            "-prune",
            "-o",
            "-type",
            "f",
            "-printf",
            "%T@\n",
        ])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            // find prints float epochs like "1690000000.1234567"; take the seconds.
            .filter_map(|l| l.split('.').next().and_then(|s| s.parse::<i64>().ok()))
            .max()
            .unwrap_or(0),
        Err(_) => 0,
    }
}

/// Count physical slot directories under worktrees/slots/.
fn physical_slot_count(root: &Path) -> usize {
    let wt = root.join("worktrees/slots");
    std::fs::read_dir(&wt)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0)
}

/// Advisory workspace-health check: disk soft cap (GB, apparent/du -sb),
/// languishing slots (>24h), and a slot-count advisory. Never fails the run and
/// never blocks allocation — it prints an informational banner to stderr so
/// agents notice and can do housekeeping, NOT a stop sign. Wording is
/// deliberately advisory: a firing banner must not read as a reason to refuse
/// work.
fn homeostasis_check(root: &Path) {
    let cap_gb: u64 = std::env::var("HERMIT_WORKTREE_GB_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_DISK_CAP_GB);

    let wt = root.join("worktrees");
    let bytes = dir_size_bytes(&wt);
    let gb = bytes as f64 / 1e9;

    let mut warnings: Vec<String> = Vec::new();

    // BASE DISTANCE. A new slot is cut from the primary's HEAD (see
    // --start-point, whose default is primary HEAD), so it inherits the
    // primary's staleness exactly. That failure mode is QUIET: a stale edit
    // applies cleanly and reverts landed work while looking like ordinary
    // cleanup, and an ancestry check against a diverged local main answers the
    // "did it land?" question WRONGLY. Surfacing the number here is what lets an
    // agent see its base distance without having to think to ask.
    //
    // This deliberately does NOT fetch: allocation must not block on the network
    // (the proxy is intermittent here). So the distance is measured against the
    // origin/main ref AS LAST FETCHED, and the ref's own identity is printed --
    // otherwise a stale ref would report a reassuring 0 and be indistinguishable
    // from an up-to-date tree, which is the same false-zero this warning exists
    // to prevent.
    if let (true, tip, _) = git(root, &["rev-parse", "--short", "origin/main"]) {
        let behind = git(root, &["rev-list", "--count", "HEAD..origin/main"])
            .1
            .trim()
            .parse::<u64>()
            .unwrap_or(0);
        let ahead = git(root, &["rev-list", "--count", "origin/main..HEAD"])
            .1
            .trim()
            .parse::<u64>()
            .unwrap_or(0);
        if behind > BASE_DISTANCE_NOTICE {
            let diverged = if ahead > 0 {
                format!(
                    " and {ahead} AHEAD, i.e. DIVERGED and not fast-forwardable -- \
                     reconciling that is a coordinator/owner decision, not yours"
                )
            } else {
                String::new()
            };
            warnings.push(format!(
                "STALE BASE: this primary is {behind} commit(s) behind \
                 origin/main ({tip}, as last fetched -- this check does not \
                 fetch){diverged}.\n     \
                 A slot cut from it starts {behind} behind too, and an ancestry \
                 check against local main will report a landed commit as NOT \
                 landed.\n     \
                 Base on fresh origin instead:\n     \
                 with-proxy git -C {root} fetch origin main && \
                 scripts/allocate-worktree.rs --start-point origin/main ...",
                root = root.display()
            ));
        }
    }

    if gb > cap_gb as f64 {
        warnings.push(format!(
            "disk heads-up: worktrees/ = {gb:.1} GB apparent (du -sb) > {cap_gb} GB soft cap.\n     \
             This is advisory only — btrfs compression means real on-disk use is far\n     \
             lower, and nothing is blocked. When convenient (or leave for the\n     \
             coordinator), reclaim build dirs / idle slots:\n     \
             find worktrees -name target -type d -maxdepth 3 -exec rm -rf {{}} +\n     \
             scripts/release-worktree.rs --slot <slot> --clean"
        ));
    } else {
        eprintln!("homeostasis: worktrees/ = {gb:.1} GB apparent / {cap_gb} GB soft cap (ok)");
    }

    let languish_hours: i64 = std::env::var("HERMIT_WORKTREE_LANGUISH_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(LANGUISH_HOURS);

    // Languishing slots: no *file edits* in >24h. We walk the PHYSICAL slot dirs
    // (not just registered ones) so abandoned/orphaned slots are caught, and use
    // the real newest source-file mtime — max'd with the registered state
    // timestamp so a freshly-allocated slot that hasn't been touched yet isn't
    // falsely flagged. Re-read state fresh so --check-only reflects current truth.
    let now = epoch_now();
    let state = load_state(root);
    let slots_obj = state["slots"].as_object();
    // (name, idle_hours, registered) sorted most-idle-first.
    let mut languishing: Vec<(String, i64, bool)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.join("worktrees/slots")) {
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        for name in names {
            let dir = root.join("worktrees/slots").join(&name);
            let registered = slots_obj.map(|s| s.contains_key(&name)).unwrap_or(false);
            let state_ts = slots_obj
                .and_then(|s| s.get(&name))
                .and_then(|s| s["updated"].as_str().or_else(|| s["allocated"].as_str()))
                .and_then(epoch_of)
                .unwrap_or(0);
            // Last activity = most recent of {real edit, registration bump}.
            let last = newest_source_mtime(&dir).max(state_ts);
            if last == 0 {
                continue; // can't determine an age; don't guess.
            }
            let hours = (now - last) / 3600;
            if hours >= languish_hours {
                languishing.push((name, hours, registered));
            }
        }
    }
    if !languishing.is_empty() {
        languishing.sort_by(|a, b| b.1.cmp(&a.1));
        let total = languishing.len();
        const SHOW: usize = 10;
        let shown: Vec<String> = languishing
            .iter()
            .take(SHOW)
            .map(|(n, h, reg)| {
                let tag = if *reg { "" } else { ", UNREGISTERED" };
                format!("{n} ({h}h no edits{tag})")
            })
            .collect();
        let more = if total > SHOW {
            format!(" (+{} more)", total - SHOW)
        } else {
            String::new()
        };
        warnings.push(format!(
            "LANGUISHING SLOTS: {total} slot(s) with no file edits in >{languish_hours}h: {}{more}.\n     \
             Land their work as a branch/draft PR, then release: scripts/release-worktree.rs --slot <slot> --clean",
            shown.join(", ")
        ));
    }

    // Slot-count advisory (disk is the real cap; this flags sprawl).
    let phys = physical_slot_count(root);
    if phys > SLOT_COUNT_ADVISORY {
        warnings.push(format!(
            "SLOT SPRAWL: {phys} physical slot dirs under worktrees/ (advisory soft \
             limit {SLOT_COUNT_ADVISORY}).\n     \
             The real cap is disk ({cap_gb} GB); consolidate/clean idle slots."
        ));
    }

    if !warnings.is_empty() {
        eprintln!();
        eprintln!("┌────────────────────────────────────────────────────────────────────┐");
        eprintln!("│  ℹ  WORKTREE HOMEOSTASIS — ADVISORY (nothing blocked; keep working) │");
        eprintln!("└────────────────────────────────────────────────────────────────────┘");
        eprintln!("  Your slot IS allocated. The item(s) below are housekeeping heads-ups,");
        eprintln!("  not errors and not a reason to stop or refuse work. Address them when");
        eprintln!("  convenient, or leave them for the coordinator.");
        for (n, w) in warnings.iter().enumerate() {
            eprintln!("  {}. {w}", n + 1);
        }
        eprintln!(
            "  See ai_docs/transient/2026-07-27-worktree-management-map.md §5 (disk hygiene)."
        );
        eprintln!("────────────────────────────────────────────────────────────────────────");
        eprintln!();
    }
}

fn state_path(root: &Path) -> PathBuf {
    root.join("worktree-state.json")
}

fn load_state(root: &Path) -> Value {
    let p = state_path(root);
    if !p.exists() {
        return json!({ "version": 3, "updated": Value::Null, "slots": {} });
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
        ".{name}.allocate-{}-{nonce}.tmp",
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

const ACTIVE_BEGIN: &str =
    "<!-- BEGIN worktree-state (managed by scripts/allocate-worktree.rs; do not edit inside) -->";
const ACTIVE_END: &str = "<!-- END worktree-state -->";

fn active_row(name: &str, slot: &Value) -> String {
    let agents = slot["agents"].as_array().cloned().unwrap_or_default();
    let owner = agents
        .iter()
        .find(|agent| !agent["read_only"].as_bool().unwrap_or(false))
        .and_then(|agent| agent["name"].as_str())
        .unwrap_or("-");
    let read_only_agents: Vec<String> = agents
        .iter()
        .filter(|agent| agent["read_only"].as_bool().unwrap_or(false))
        .filter_map(|agent| agent["name"].as_str().map(str::to_string))
        .collect();
    let agent_cell = if read_only_agents.is_empty() {
        owner.to_string()
    } else {
        format!("{owner} (+ro: {})", read_only_agents.join(", "))
    };
    let hermit = slot["hermit_branch"].as_str().unwrap_or("-");
    let reverie = slot["reverie_branch"].as_str().unwrap_or("-");
    let liteinst2 = slot["liteinst2_branch"].as_str().unwrap_or("-");
    let task = slot["task"].as_str().unwrap_or("-");
    let status = slot["status"].as_str().unwrap_or("active");
    let shared = if read_only_agents.is_empty() {
        "no"
    } else {
        "shared"
    };
    format!(
        "| {name} | {agent_cell} | {hermit} | {reverie} | {liteinst2} | {task} | {status} | {shared} |"
    )
}

/// Rewrite the managed table block inside worktrees/ACTIVE.md from state.
/// Human-authored content outside the markers is preserved verbatim.
fn regen_active_md(root: &Path, state: &Value) -> bool {
    let mut rows = String::new();
    rows.push_str("| Slot | Agent | HermitBranch | ReverieBranch | LiteInst2Branch | Task | Status | ReadOnly |\n");
    rows.push_str("| --- | --- | --- | --- | --- | --- | --- | --- |\n");
    if let Some(slots) = state["slots"].as_object() {
        let mut names: Vec<&String> = slots.keys().collect();
        names.sort();
        for name in names {
            rows.push_str(&active_row(name, &slots[name]));
            rows.push('\n');
        }
    }
    let block = format!("{ACTIVE_BEGIN}\n{rows}{ACTIVE_END}\n");

    let path = root.join("worktrees").join("ACTIVE.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let new_content =
        if let (Some(b), Some(e)) = (existing.find(ACTIVE_BEGIN), existing.find(ACTIVE_END)) {
            let e_end = e + ACTIVE_END.len();
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
            let lead = if existing.is_empty() {
                "# Active Hermit Worktrees\n\n## Machine-managed slot table\n\n".to_string()
            } else {
                format!("{sep}\n## Machine-managed slot table\n\n")
            };
            format!("{existing}{lead}{block}")
        };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let changed = new_content != existing;
    if changed {
        durable_replace(&path, new_content.as_bytes())
            .unwrap_or_else(|e| die(&format!("write ACTIVE.md: {e}")));
    }
    changed
}

/// Update only named managed rows. Rows outside `selected` are preserved
/// byte-for-byte, so an explicit exclusion is a real mutation boundary rather
/// than merely a verifier filter.
fn regen_active_md_scoped(
    root: &Path,
    state: &Value,
    selected: &BTreeSet<String>,
) -> Result<bool, String> {
    let path = root.join("worktrees/ACTIVE.md");
    let existing =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let begin = existing
        .find(ACTIVE_BEGIN)
        .ok_or_else(|| "ACTIVE.md lacks managed BEGIN marker".to_string())?;
    let content_start = begin + ACTIVE_BEGIN.len();
    let relative_end = existing[content_start..]
        .find(ACTIVE_END)
        .ok_or_else(|| "ACTIVE.md lacks managed END marker".to_string())?;
    let end = content_start + relative_end;
    let managed = &existing[content_start..end];
    let slots = state["slots"]
        .as_object()
        .ok_or_else(|| "worktree-state.json lacks object field 'slots'".to_string())?;
    let mut seen = BTreeSet::new();
    let mut rewritten = String::new();
    for line in managed.lines() {
        let name = line
            .strip_prefix('|')
            .and_then(|row| row.split('|').next())
            .map(str::trim);
        if let Some(name) = name.filter(|name| selected.contains(*name)) {
            if !seen.insert(name.to_string()) {
                return Err(format!(
                    "ACTIVE.md has duplicate managed row for scoped slot {name}"
                ));
            }
            let slot = slots
                .get(name)
                .ok_or_else(|| format!("scoped slot {name} is absent from state"))?;
            rewritten.push_str(&active_row(name, slot));
        } else {
            rewritten.push_str(line);
        }
        rewritten.push('\n');
    }
    for name in selected {
        if seen.contains(name) {
            continue;
        }
        let slot = slots
            .get(name)
            .ok_or_else(|| format!("scoped slot {name} is absent from state"))?;
        rewritten.push_str(&active_row(name, slot));
        rewritten.push('\n');
    }
    let new_content = format!(
        "{}{}{}",
        &existing[..content_start],
        rewritten,
        &existing[end..]
    );
    if new_content == existing {
        return Ok(false);
    }
    durable_replace(&path, new_content.as_bytes())
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(true)
}

/// Physical checkout of a product worktree, read from git porcelain. Mirrors the
/// canonical verifier scripts/check-worktree-registry.rs::actual_checkout so the
/// repair mode and the verifier agree on what "the actual branch" means.
fn exact_checkout_root(path: &Path) -> Result<PathBuf, String> {
    let requested = fs::canonicalize(path)
        .map_err(|error| format!("canonicalize requested checkout: {error}"))?;
    let (ok, top, error) = git(
        path,
        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
    );
    if !ok || top.is_empty() {
        return Err(if error.is_empty() {
            "not a git worktree".to_string()
        } else {
            error
        });
    }
    let observed =
        fs::canonicalize(&top).map_err(|error| format!("canonicalize git top-level: {error}"))?;
    if observed != requested {
        return Err(format!(
            "git top-level {} does not equal requested checkout {}",
            observed.display(),
            requested.display()
        ));
    }
    Ok(requested)
}

fn actual_checkout(path: &Path) -> Option<String> {
    if !path.exists() {
        return Some("-".to_string()); // Absent -> recorded as "-"
    }
    if exact_checkout_root(path).is_err() {
        return None; // Unreadable: do NOT rewrite the recorded value; skip + warn.
    }
    let (on_branch, branch, _) = git(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    if on_branch && !branch.is_empty() {
        return Some(branch);
    }
    let (has_head, _, _) = git(path, &["rev-parse", "HEAD"]);
    if has_head {
        // Detached: verifier accepts the bare token "detached" for any detached SHA.
        Some("detached".to_string())
    } else {
        None
    }
}

fn validate_canonical_slot_paths(slot: &str, record: &Value) -> Result<(), String> {
    for product in ["hermit", "reverie", "liteinst2"] {
        let key = format!("{product}_path");
        let expected = format!("worktrees/slots/{slot}/{product}");
        if record[key.as_str()].as_str() != Some(expected.as_str()) {
            return Err(format!(
                "slot {slot} records noncanonical {key}; expected exact path '{expected}'"
            ));
        }
    }
    Ok(())
}

/// Rewrite only the exact path spelling replaced by the slots/ layout.
/// Anything else is left untouched and refuses the whole migration.
fn migrate_layout(root: &Path, dry_run: bool) -> ! {
    let mut state = load_state(root);
    let slot_names: Vec<String> = state["slots"]
        .as_object()
        .map(|slots| slots.keys().cloned().collect())
        .unwrap_or_default();
    let mut changes: Vec<(String, String, String)> = Vec::new();
    let mut invalid: Vec<String> = Vec::new();
    for slot in &slot_names {
        for product in ["hermit", "reverie", "liteinst2"] {
            let key = format!("{product}_path");
            let legacy = format!("worktrees/{slot}/{product}");
            let canonical = format!("worktrees/slots/{slot}/{product}");
            match state["slots"][slot][key.as_str()].as_str() {
                Some(value) if value == canonical => {}
                Some(value) if value == legacy => {
                    changes.push((slot.clone(), key, canonical));
                }
                other => invalid.push(format!(
                    "{slot}/{product}: expected {legacy:?} or {canonical:?}, got {other:?}"
                )),
            }
        }
    }
    if !invalid.is_empty() {
        for finding in invalid {
            eprintln!("REFUSING layout migration: {finding}");
        }
        die("registry contains a path that is neither the exact legacy nor canonical spelling");
    }
    for (slot, key, canonical) in &changes {
        println!(
            "  migrate {slot}/{key}: '{}' -> '{canonical}'",
            state["slots"][slot][key.as_str()].as_str().unwrap_or("")
        );
    }
    if dry_run {
        println!(
            "layout migration --dry-run: {} path cell(s) would change. No files written.",
            changes.len()
        );
        exit(if changes.is_empty() { 0 } else { 1 });
    }
    for (slot, key, canonical) in &changes {
        state["slots"][slot][key.as_str()] = json!(canonical);
    }
    if !changes.is_empty() {
        save_state(root, &mut state);
        regen_active_md(root, &state);
    }
    println!(
        "layout migration: rewrote {} path cell(s); no worktree was moved.",
        changes.len()
    );
    exit(0);
}

/// SINGLE-WRITER REPAIR/SYNC: reconcile the recorded {product}_branch cells in
/// worktree-state.json (and, via regen_active_md, the managed ACTIVE.md block)
/// FROM the physical submodule porcelain. This is the reconciler that closes the
/// three-registries-no-reconciler gap (Proxy-Binding worked-example #9).
///
/// SAFETY CONTRACT:
///   * Only {product}_branch cells are touched. Ownership (agents/task/status/
///     purpose) is NEVER rewritten from physical state — repurposing intent lives
///     with a human, not on disk.
///   * NO git branch/checkout/delete is ever run. Unpushed local refs (e.g. a
///     slot's codex/* branch parked at a local-only SHA) survive inherently
///     because repair only rewrites recorded strings, never git objects.
///   * An Unreadable child leaves its recorded value untouched (skip + warn),
///     so a transient fault cannot erase a legitimate record.
///   * A recorded branch that DOES NOT EXIST in its checkout is an intent that was asked for and
///     never created, not stale drift, so the whole scope REFUSES and nothing is written. See
///     recorded_branch_is_unfulfilled_intent. Reconciling it would record the previous task's
///     branch as correct and make the divergence self-consistent, which is strictly worse than
///     leaving it visible: refusing keeps check-worktree-registry.rs reporting the drift.
/// Runs the verifier before and after so the drift delta is observable, not
/// inferred.
fn repair_registry(
    root: &Path,
    dry_run: bool,
    only_slot: Option<&str>,
    excluded_slots: &BTreeSet<String>,
) -> ! {
    let checker = root.join("scripts/check-worktree-registry.rs");
    let root_arg = root.to_string_lossy().into_owned();
    let run_checker = |label: &str| -> bool {
        eprintln!("── verifier {label} repair ──");
        let mut command = Command::new(&checker);
        command.args(["--root", root_arg.as_str()]);
        if let Some(slot) = only_slot {
            command.args(["--slot", slot]);
        }
        for slot in excluded_slots {
            command.args(["--exclude-slot", slot]);
        }
        command.status().is_ok_and(|status| status.success())
    };

    let mut state = load_state(root);
    let all_slot_names: Vec<String> = state["slots"]
        .as_object()
        .map(|s| s.keys().cloned().collect())
        .unwrap_or_default();
    if let Some(slot) = only_slot {
        if !all_slot_names.iter().any(|name| name == slot) {
            die(&format!("scoped repair slot {slot:?} is not registered"));
        }
    }
    for slot in excluded_slots {
        if !all_slot_names.iter().any(|name| name == slot) {
            die(&format!("excluded repair slot {slot:?} is not registered"));
        }
    }
    let slot_names: Vec<String> = all_slot_names
        .into_iter()
        .filter(|slot| only_slot.map_or(true, |only| only == slot))
        .filter(|slot| !excluded_slots.contains(slot))
        .collect();
    if slot_names.is_empty() {
        die("repair scope selects no registered slots");
    }
    let selected: BTreeSet<String> = slot_names.iter().cloned().collect();
    if let Some(slot) = only_slot {
        println!("repair scope: only slot={slot}");
    } else if !excluded_slots.is_empty() {
        println!(
            "repair scope: excluded={} (not inspected or modified)",
            excluded_slots.iter().cloned().collect::<Vec<_>>().join(",")
        );
    } else {
        println!("repair scope: all registered slots");
    }
    run_checker("BEFORE");

    let releasing: Vec<&String> = slot_names
        .iter()
        .filter(|slot| {
            let record = &state["slots"][*slot];
            record["status"].as_str() == Some("releasing")
                || record.get("release_journal").is_some()
                || record.get("orphan_residue_journal").is_some()
                || record.get("coordinator_lease_intent").is_some()
                || revocation_journal_blocks_repair(root, record)
        })
        .collect();
    if !releasing.is_empty() {
        eprintln!(
            "REFUSING repair while slot release transaction(s) require guarded recovery: {}",
            releasing
                .iter()
                .map(|slot| slot.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        exit(1);
    }

    let mut changed = 0usize;
    let mut skipped = 0usize;
    let mut changes: Vec<(String, String, String)> = Vec::new();
    let mut observed: Vec<(String, String, String)> = Vec::new();
    let mut unfulfilled: Vec<String> = Vec::new();
    for slot in &slot_names {
        if let Err(error) = validate_canonical_slot_paths(slot, &state["slots"][slot]) {
            skipped += 3;
            eprintln!("  skip {slot}: {error}; all branch cells left as-is");
            continue;
        }
        for product in ["hermit", "reverie", "liteinst2"] {
            let recorded = state["slots"][slot][format!("{product}_branch")]
                .as_str()
                .unwrap_or("-")
                .to_string();
            let rel = PathBuf::from(format!("worktrees/slots/{slot}/{product}"));
            match actual_checkout(&root.join(&rel)) {
                None => {
                    skipped += 1;
                    eprintln!(
                        "  skip {slot}/{product}: worktree unreadable; recorded='{recorded}' left as-is"
                    );
                }
                Some(actual) => {
                    observed.push((slot.clone(), product.to_string(), actual.clone()));
                    // "detached" recorded value already matches any detached SHA in
                    // the verifier; don't churn a detached:<sha> recording into bare
                    // "detached" if it already agrees.
                    let already_ok = recorded == actual
                        || (actual == "detached" && recorded.starts_with("detached"));
                    if !already_ok {
                        if recorded_branch_is_unfulfilled_intent(root, slot, product, &recorded) {
                            // Do NOT count this as a change: the cell is left exactly as recorded,
                            // so the verifier keeps reporting the drift.
                            unfulfilled.push(format!(
                                "{slot}/{product}: recorded branch '{recorded}' does not exist in \
                                 the checkout, which is on '{actual}'. This is a branch that was \
                                 asked for but never created, so reconciling the record to the \
                                 checkout would cement '{actual}' and discard the intent. Create \
                                 '{recorded}' in worktrees/slots/{slot}/{product} and switch to it, or \
                                 have the owner correct the recorded cell; repair will not choose \
                                 between them."
                            ));
                            continue;
                        }
                        changed += 1;
                        println!("  reconcile {slot}/{product}: '{recorded}' -> '{actual}'");
                        changes.push((slot.clone(), product.to_string(), actual));
                    }
                }
            }
        }
    }

    let mut unpublished = Vec::new();
    let scoped = only_slot.is_some() || !excluded_slots.is_empty();
    let publication_checks = if scoped { &observed } else { &changes };
    for (slot, product, actual) in publication_checks {
        if actual == "-" {
            if changes
                .iter()
                .any(|(changed_slot, changed_product, changed_actual)| {
                    changed_slot == slot && changed_product == product && changed_actual == actual
                })
            {
                unpublished.push(format!(
                    "{slot}/{product}: checkout is absent; unpublished state cannot be proven"
                ));
            }
            continue;
        }
        if let Err(error) = verify_repair_checkout_published(root, slot, product, actual) {
            unpublished.push(error);
        }
    }
    if skipped != 0 && scoped {
        die(&format!(
            "scoped repair could not verify {skipped} selected branch cell(s); registry left unchanged"
        ));
    }
    if !unpublished.is_empty() {
        for finding in &unpublished {
            eprintln!("REFUSING repair: unpublished work check failed: {finding}");
        }
        die("repair scope contains unpublished or unverifiable work; registry left unchanged");
    }

    // Refuse the whole scope, not just the offending cell. A partial repair that fixed the other
    // cells would report success and leave the operator believing the slot was reconciled.
    if !unfulfilled.is_empty() {
        for finding in &unfulfilled {
            eprintln!("REFUSING repair: recorded branch was never created: {finding}");
        }
        die(
            "repair scope contains a recorded branch that does not exist in its checkout; \
             registry left unchanged. Reconciling it would record the previous task's branch as \
             correct and hide the divergence from check-worktree-registry.rs.",
        );
    }

    if dry_run {
        println!("repair --dry-run: {changed} branch cell(s) would change, {skipped} skipped (unreadable), unpublished=0. No files written.");
        exit(if changed == 0 { 0 } else { 1 });
    }

    for (slot, product, actual) in &changes {
        state["slots"][slot][format!("{product}_branch")] = json!(actual);
    }
    if changed > 0 {
        save_state(root, &mut state);
        if scoped {
            regen_active_md_scoped(root, &state, &selected)
                .unwrap_or_else(|error| die(&format!("scoped ACTIVE.md repair failed: {error}")));
        } else {
            regen_active_md(root, &state);
        }
        println!("repair: reconciled {changed} branch cell(s), {skipped} skipped (unreadable), unpublished=0.");
        println!("  state:  {}", state_path(root).display());
        println!("  active: {}", root.join("worktrees/ACTIVE.md").display());
    } else {
        // ACTIVE is derived independently from branch-cell drift. The
        // preflight regeneration above must still run when changed == 0 so a
        // crash between the two durable files cannot remain self-deadlocking.
        let active_reconciled = if scoped {
            regen_active_md_scoped(root, &state, &selected)
                .unwrap_or_else(|error| die(&format!("scoped ACTIVE.md repair failed: {error}")))
        } else {
            regen_active_md(root, &state)
        };
        println!("repair: 0 branch cells needed reconciliation ({skipped} skipped unreadable, unpublished=0).");
        if active_reconciled {
            println!("repair: regenerated the selected managed ACTIVE.md row(s).");
        }
    }

    if !run_checker("AFTER") && scoped {
        die("scoped post-repair verifier refused; selected scope is not consistent");
    }
    exit(0);
}

/// Slot name is a named token [a-z0-9-]+ (e.g. kvm) or slotNN.
fn valid_slot(name: &str) -> bool {
    worktree_registry::valid_slot_name(name)
}

/// Tmux pane ids that exist on this box right now, best effort. `None` means
/// "could not cross-check" and must never be read as "no live panes" — it is
/// used only to CLASSIFY a refusal that has already been decided.
fn live_tmux_pane_ids() -> Option<Vec<String>> {
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
    )
}

/// Decide whether an owner snapshot describes a real fleet AT ALL, so a refusal
/// can name the snapshot instead of blaming the agent.
///
/// A snapshot that is FRESH is not thereby TRUE: `captured_at` is stamped on
/// every write regardless of payload, so a stub launders itself as maximally
/// fresh. On 2026-08-08 a 52-second-old snapshot holding exactly one entry,
/// `{name:"worker", tmux_pane_id:null}`, made every real agent resolve zero
/// times — and the resulting "resolves agent X 0 times" was read fleet-wide as
/// "X is not a real agent". Four slots were recorded as permanently lease-less,
/// adoption was re-run repeatedly against a remedy that could not work, and a
/// release-then-reallocate left the `validate-rust` slot in a collision state.
///
/// The predicate is the identity requirement `observe_owner_lease` already
/// enforces per-agent — a `%NN` pane present in `tmux list-panes -a` — applied
/// to the payload as a whole. Returns `Some(reason)` when no entry could name a
/// real agent, which is the `implausible` member of the {missing, stale,
/// implausible} refusal set that `operational_health.load_agent_snapshot` is
/// adopting for the same file. Classification only: it decides WHAT TO SAY
/// about a refusal, never WHETHER to refuse.
fn owner_snapshot_implausibility(agents: &[agent_snapshot::Agent]) -> Option<String> {
    if agents.is_empty() {
        return Some("it lists no agents at all".to_string());
    }
    let panes: Vec<&str> = agents
        .iter()
        .filter_map(|entry| entry.tmux_pane_id.as_deref())
        .collect();
    if panes.is_empty() {
        let mut names: Vec<&str> = agents.iter().map(|entry| entry.name.as_str()).collect();
        names.truncate(5);
        return Some(format!(
            "not one of its {} entries carries a usable tmux pane id (names: {}{})",
            agents.len(),
            names.join(", "),
            if agents.len() > names.len() {
                ", ..."
            } else {
                ""
            },
        ));
    }
    let live = live_tmux_pane_ids()?;
    if !panes
        .iter()
        .any(|pane| live.iter().any(|candidate| candidate == pane))
    {
        return Some(format!(
            "none of its {} pane id(s) exists in `tmux list-panes -a` ({} live pane(s) on this box), \
             so it describes a fleet that is not running here",
            panes.len(),
            live.len(),
        ));
    }
    None
}

/// Capture the exact live pane and unified cgroup that own an agent name.  A
/// newly provisioned slot may precede agent startup; in that case allocation
/// succeeds but release remains fail-closed until the coordinator re-runs this
/// allocator adoption after the owner is live.
/// Resolve the process tree to read identity from.  Real `/proc` in production;
/// a disposable fixture's copy when one is explicitly provided.
///
/// WHY THIS IS HERE AT ALL.  This allocator validates a recorded supervisor
/// identity against the kernel, and it is RIGHT to do so -- that check is what
/// makes a sidecar unforgeable.  But it also made the check untestable: the
/// sentinel fixture in ci-hub/tests writes a complete fake process entry (stat,
/// cgroup, cwd) under its own root and points HERMIT_RELEASE_TEST_PROC_ROOT at
/// it, and this file was reading absolute `/proc` regardless.  So the fixture's
/// fabricated pid was checked against the real kernel, which of course does not
/// have it, and `test_allocator_preserves_legacy_authority_and_recovers_failed_launch`
/// -- the ONLY test of the `--recover-legacy-unbound-owner` path -- failed with
/// `read /proc/41000/stat: No such file or directory`.  That path is the named
/// remedy for a slot whose owner evidence is missing, and it could not be
/// exercised at all.
///
/// release-worktree.rs already reads process identity exactly this way, through
/// the same variable and the same guard; this file was the outlier.  The guard is
/// copied, not invented: the override is accepted ONLY when the workspace is a
/// canonicalized path under the temp directory whose ancestry contains a
/// `release-worktree-test.` fixture directory AND the override points inside that
/// workspace.  Anything else is an ERROR, not a silent fall-back to real `/proc`,
/// so a stray variable in a production environment cannot quietly redirect the
/// one check that makes ownership evidence unforgeable.
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

fn process_start_ticks(root: &Path, pid: u32) -> Result<u64, String> {
    let path = process_root(root)?.join(pid.to_string()).join("stat");
    let raw =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let close = raw
        .rfind(") ")
        .ok_or_else(|| format!("{} has no closing process name", path.display()))?;
    raw[close + 2..]
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| format!("{} omits field 22 start_ticks", path.display()))?
        .parse()
        .map_err(|error| format!("parse {} field 22 start_ticks: {error}", path.display()))
}

fn current_boot_id() -> Result<String, String> {
    let path = Path::new("/proc/sys/kernel/random/boot_id");
    let value =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{} is empty", path.display()));
    }
    Ok(value.to_string())
}

fn owner_sidecar(
    slot: &str,
    agent: &str,
    task: &str,
    recorded_at: &str,
    source: &str,
    supervisor_pid: u32,
    boot_id: &str,
    start_ticks: u64,
    tmux_pane_id: Option<&str>,
    cgroup_path: &str,
) -> Result<Value, String> {
    if supervisor_pid == 0 || start_ticks == 0 || boot_id.is_empty() {
        return Err("slot-owner sidecar has an incomplete supervisor identity".to_string());
    }
    if cgroup_path == "/" || !cgroup_path.starts_with('/') {
        return Err("slot-owner sidecar has no normalized non-root cgroup".to_string());
    }
    let generation = format!("{boot_id}:{supervisor_pid}:{start_ticks}");
    let issued_at = epoch_now();
    Ok(json!({
        "schema_version": OWNER_SIDECAR_SCHEMA_VERSION,
        "source": source,
        "slot": slot,
        "agent": agent,
        "task": task,
        "recorded_at": recorded_at,
        "generation": generation,
        "supervisor_pid": supervisor_pid,
        "boot_id": boot_id,
        "start_ticks": start_ticks,
        "tmux_pane_id": tmux_pane_id,
        "cgroup_path": cgroup_path,
        "lease": {
            "schema_version": OWNER_LEASE_SCHEMA_VERSION,
            "resource": format!("slot/{slot}"),
            "generation": generation,
            "issued_at": issued_at,
            "renewed_at": issued_at,
            "ttl_seconds": OWNER_LEASE_TTL_SECONDS,
            "expires_at": issued_at.saturating_add(OWNER_LEASE_TTL_SECONDS),
            "state": "active",
            "last_transition_actor": agent,
            "automatic_handoff": null,
        },
    }))
}

fn observed_owner_sidecar(
    slot: &str,
    agent: &str,
    task: &str,
    recorded_at: &str,
    lease: &ObservedOwnerLease,
) -> Result<Value, String> {
    owner_sidecar(
        slot,
        agent,
        task,
        recorded_at,
        ORC_OWNER_SIDECAR_SOURCE,
        lease.supervisor_pid,
        &lease.boot_id,
        lease.start_ticks,
        Some(&lease.pane),
        &lease.cgroup,
    )
}

fn codex_owner_sidecar(
    root: &Path,
    slot: &str,
    agent: &str,
    task: &str,
    recorded_at: &str,
    lease: &Value,
) -> Result<Value, String> {
    if lease["schema_version"].as_u64() != Some(1)
        || lease["source"].as_str() != Some(CODEX_OWNER_SIDECAR_SOURCE)
    {
        return Err("Codex coordinator lease has an unsupported identity schema".to_string());
    }
    let pid = lease["main_pid"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| "Codex coordinator lease has no supervisor pid".to_string())?;
    let recorded_ticks = lease["main_pid_starttime"]
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or_else(|| "Codex coordinator lease has no supervisor start_ticks".to_string())?;
    let observed_ticks = process_start_ticks(root, pid)?;
    if observed_ticks != recorded_ticks {
        return Err(format!(
            "Codex coordinator supervisor {pid} was reused: recorded start_ticks={recorded_ticks}, observed={observed_ticks}"
        ));
    }
    let cgroup = lease["cgroup_path"]
        .as_str()
        .ok_or_else(|| "Codex coordinator lease has no cgroup path".to_string())?;
    owner_sidecar(
        slot,
        agent,
        task,
        recorded_at,
        CODEX_OWNER_SIDECAR_SOURCE,
        pid,
        &current_boot_id()?,
        recorded_ticks,
        None,
        cgroup,
    )
}

fn observe_owner_lease(root: &Path, agent: &str) -> Result<ObservedOwnerLease, String> {
    let snapshot_path = root.join("ignored/ci-hub/agent-snapshot.json");
    let raw = fs::read(&snapshot_path)
        .map_err(|error| format!("read {}: {error}", snapshot_path.display()))?;
    let snapshot = agent_snapshot::AgentSnapshot::parse(&raw)
        .map_err(|error| format!("owner snapshot is invalid: {error}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?
        .as_secs_f64();
    snapshot
        .ensure_fresh(now, 600.0)
        .map_err(|error| error.replace("agent snapshot", "owner snapshot"))?;
    let agents = &snapshot.agents;
    let matches: Vec<&agent_snapshot::Agent> =
        agents.iter().filter(|entry| entry.name == agent).collect();
    // Same refusal as ever, in every case. Only the WORDING is split, because
    // one message for two causes sent the fleet after the wrong remedy: an
    // implausible snapshot has to be fixed at the producer, whereas a genuinely
    // absent agent is fixed by re-running adoption from that agent's live pane.
    if matches.len() > 1 {
        return Err(format!(
            "agent '{agent}' is listed {} times in the owner snapshot at {} — \
             ownership is ambiguous, so no lease can bind. Fix the duplicate entry; \
             the snapshot itself is otherwise usable.",
            matches.len(),
            snapshot_path.display(),
        ));
    }
    if matches.is_empty() {
        return Err(match owner_snapshot_implausibility(agents) {
            // Cause 1: the snapshot is the problem. Say so, and say plainly
            // that this is NOT evidence about the agent.
            Some(reason) => format!(
                "the owner snapshot at {} IS NOT USABLE: {reason}. This says nothing about whether \
                 '{agent}' exists — the lookup could not be performed at all, so do NOT conclude \
                 '{agent}' is absent, and do NOT release or reallocate the slot on the strength of \
                 this message. Re-running adoption cannot help until the snapshot's producer (a live \
                 ORC session) emits the real fleet. Note a fresh snapshot can still be false: \
                 captured_at is stamped on every write regardless of payload.",
                snapshot_path.display(),
            ),
            // Cause 2: the snapshot is fine and the agent really is not in it.
            None => format!(
                "agent '{agent}' is absent from the owner snapshot at {}, which does carry a real \
                 fleet ({} entr{} with live pane identities) — so this is a fact about '{agent}', \
                 not about the snapshot. Re-run this exact allocation/adoption from the live pane \
                 of '{agent}'.",
                snapshot_path.display(),
                agents.len(),
                if agents.len() == 1 { "y" } else { "ies" },
            ),
        });
    }
    let entry = matches[0];
    if !entry.status.is_live() {
        return Err(format!("owner '{agent}' is terminal in the fresh snapshot"));
    }
    let pane = entry
        .tmux_pane_id
        .as_deref()
        .ok_or_else(|| format!("owner '{agent}' has no valid tmux pane identity"))?;
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}\t#{pane_pid}"])
        .output()
        .map_err(|error| format!("tmux pane query unavailable: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "tmux pane query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(candidate, _)| *candidate == pane)
        .map(|(_, pid)| {
            pid.parse::<u32>()
                .map_err(|_| format!("tmux pane {pane} has invalid pid '{pid}'"))
        })
        .collect::<Result<_, _>>()?;
    if pids.len() != 1 {
        return Err(format!("tmux pane {pane} resolves {} times", pids.len()));
    }
    bind_pane_supervisor(root, pane, pids[0])
}

/// Read the exact identity of one tmux pane supervisor from `/proc`.
///
/// Shared by both owner-resolution paths so that the identity a sidecar binds
/// is captured the same way regardless of how the pane was FOUND.  The
/// start-ticks sample is deliberately taken twice, around the cgroup read, so a
/// pid recycled mid-binding is caught rather than recorded.
fn bind_pane_supervisor(
    root: &Path,
    pane: &str,
    supervisor_pid: u32,
) -> Result<ObservedOwnerLease, String> {
    let start_ticks_before = process_start_ticks(root, supervisor_pid)?;
    let boot_id = current_boot_id()?;
    let cgroup_text = fs::read_to_string(
        process_root(root)?
            .join(supervisor_pid.to_string())
            .join("cgroup"),
    )
    .map_err(|error| format!("read pane {pane} cgroup: {error}"))?;
    let groups: Vec<&str> = cgroup_text
        .lines()
        .filter_map(|line| line.strip_prefix("0::"))
        .collect();
    if groups.len() != 1 || groups[0] == "/" || !groups[0].starts_with('/') {
        return Err(format!("pane {pane} has no non-root unified cgroup"));
    }
    let relative = Path::new(groups[0].trim_start_matches('/'));
    if !relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!("pane {pane} cgroup is not normalized"));
    }
    let start_ticks_after = process_start_ticks(root, supervisor_pid)?;
    if start_ticks_before != start_ticks_after {
        return Err(format!(
            "pane {pane} supervisor pid {supervisor_pid} changed identity while binding"
        ));
    }
    Ok(ObservedOwnerLease {
        pane: pane.to_string(),
        cgroup: groups[0].to_string(),
        supervisor_pid,
        boot_id,
        start_ticks: start_ticks_before,
    })
}

/// Read one `/proc/<pid>/status` field.
fn process_status_field(root: &Path, pid: u32, field: &str) -> Result<String, String> {
    let path = process_root(root)?.join(pid.to_string()).join("status");
    let raw =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let prefix = format!("{field}:");
    raw.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().to_string())
        .ok_or_else(|| format!("{} omits {field}", path.display()))
}

/// Every pid from the calling process up to (but not through) pid 1.
fn caller_process_ancestry(root: &Path) -> Result<Vec<u32>, String> {
    let mut chain = Vec::new();
    let mut pid = std::process::id();
    // The kernel's pid space is bounded and a chain cannot legitimately be
    // deep; the cap only stops a fabricated fixture cycle from spinning.
    while pid > 1 && chain.len() < 64 {
        chain.push(pid);
        pid = process_status_field(root, pid, "PPid")?
            .parse()
            .map_err(|error| format!("parse PPid of {pid}: {error}"))?;
    }
    Ok(chain)
}

/// Bind the owner sidecar to the CALLING agent's own tmux pane supervisor.
///
/// WHY THIS PATH EXISTS.  `observe_owner_lease` resolves an agent name through
/// `ignored/ci-hub/agent-snapshot.json`, a cache written by one ORC session.
/// When that producer stalls, every allocation loses its owner evidence and the
/// row is UNASSESSABLE FOREVER -- the evidence has to be captured at binding
/// time and, per the sidecar migration contract, cannot be reconstructed later.
/// Measured 2026-08-27: the snapshot's `captured_at` was 8h00m old against a
/// 600s bound, and every mutating allocation after 2026-08-26T18:13:03Z
/// (`coordinator`, `lseekcmd`, `133`) was recorded with no sidecar, joining 18
/// earlier rows that `slot-owner-authority.py` answers `indeterminate` for and
/// `release-worktree.rs` therefore refuses.  Neither usable nor recoverable.
///
/// But an agent allocating its OWN slot never needed the cache.  Its pane
/// supervisor is an ANCESTOR OF THE CALLING PROCESS, which is a kernel fact
/// readable right now.  That is strictly stronger evidence than a name looked
/// up in a file written by somebody else: it cannot be stale, and it cannot
/// name the wrong subject.
///
/// TWO INDEPENDENT BINDINGS MUST AGREE, because the hazard here is binding the
/// WRONG subject -- a coordinator recording its own supervisor as a subagent's
/// owner would make a live agent's slot read dead:
///
///   1. the calling process's own `DG_AGENT_NAME` must equal the requested
///      agent, so the caller is claiming a slot for itself; and
///   2. the pane found on the caller's ancestry must not be NAMED after some
///      other agent.  ORC names each pane's window after the agent it runs
///      (measured 2026-08-27: 21 of 23 live panes matched exactly, one was the
///      ORC coordinator's own unnamed-for-an-agent window, one carried a stray
///      descendant that had reset the variable).  A window name that
///      CONTRADICTS the request refuses; an absent one merely does not
///      corroborate, and the ancestry plus (1) still hold.
///
/// Failing any of these returns Err and the caller falls back to the snapshot
/// path unchanged.  This path can only ADMIT an owner it has proven is the
/// caller's own; it never widens what the snapshot path would accept.
fn observe_caller_owner_lease(root: &Path, agent: &str) -> Result<ObservedOwnerLease, String> {
    caller_is_the_requested_owner(&std::env::var("DG_AGENT_NAME").unwrap_or_default(), agent)?;
    let ancestry = caller_process_ancestry(root)?;
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{pane_pid}\t#{window_name}",
        ])
        .output()
        .map_err(|error| format!("tmux pane query unavailable: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "tmux pane query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut found: Vec<(&str, u32, &str)> = Vec::new();
    for line in stdout.lines() {
        let mut fields = line.split('\t');
        let (Some(pane), Some(pid), window) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        if pane.starts_with('%') && ancestry.contains(&pid) {
            found.push((pane, pid, window.unwrap_or("")));
        }
    }
    if found.len() != 1 {
        return Err(format!(
            "the calling process's ancestry contains {} tmux pane supervisor(s), expected exactly one",
            found.len()
        ));
    }
    let (pane, supervisor_pid, window) = found[0];
    pane_window_does_not_contradict(pane, window, agent)?;
    bind_pane_supervisor(root, pane, supervisor_pid)
}

/// Guard 1: the caller must be claiming a slot for ITSELF.
///
/// The whole hazard of binding from the caller's own process tree is naming the
/// wrong subject. A coordinator that recorded its own supervisor as a
/// subagent's owner would make the subagent's slot read DEAD while the subagent
/// was working, and release-worktree.rs would then reclaim a live slot. So the
/// caller's own environment has to agree with what it asked for; anything else
/// falls back to the snapshot path, which resolves the named agent instead.
fn caller_is_the_requested_owner(claimed: &str, agent: &str) -> Result<(), String> {
    let claimed = claimed.trim();
    if claimed.is_empty() {
        return Err("the calling process carries no DG_AGENT_NAME".to_string());
    }
    if claimed != agent {
        return Err(format!(
            "the calling process is '{claimed}', not the requested owner '{agent}', so its own \
             supervisor is the wrong subject to bind"
        ));
    }
    Ok(())
}

/// Guard 2: tmux's own record of the pane must not name a DIFFERENT agent.
///
/// This is corroboration from a source the caller does not write. It is
/// deliberately asymmetric: a name that CONTRADICTS the request refuses, an
/// ABSENT name merely fails to corroborate and leaves guard 1 plus the ancestry
/// standing. Requiring a match outright would refuse a legitimate
/// self-allocation from any pane ORC had not named, which is a live-agent
/// failure mode, and the ancestry is already a kernel fact.
fn pane_window_does_not_contradict(pane: &str, window: &str, agent: &str) -> Result<(), String> {
    if !window.is_empty() && window != agent {
        return Err(format!(
            "the pane {pane} owning this process is named '{window}', which contradicts the \
             requested owner '{agent}'"
        ));
    }
    Ok(())
}

fn apply_observed_owner_lease(owner: &mut Value, observed: &Result<ObservedOwnerLease, String>) {
    match observed {
        Ok(lease) => {
            owner["tmux_pane_id"] = json!(lease.pane);
            owner["cgroup_path"] = json!(lease.cgroup);
        }
        Err(_) => {
            // An adoption attempt that cannot bind the current exact owner
            // invalidates any prior incarnation's lease.
            if let Some(owner) = owner.as_object_mut() {
                owner.remove("tmux_pane_id");
                owner.remove("cgroup_path");
            }
        }
    }
}

fn sentinel_tool(root: &Path) -> Result<PathBuf, String> {
    let tool = root.join("scripts/codex-slot-sentinel.rs");
    if !tool.is_file() {
        return Err(format!(
            "canonical Codex slot sentinel authority is missing: {}",
            tool.display()
        ));
    }
    Ok(tool)
}

fn sentinel_result(output: std::process::Output, expected_state: &str) -> Result<Value, String> {
    if !output.status.success() {
        return Err(format!(
            "Codex slot sentinel refused: {}{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse Codex slot sentinel result: {error}"))?;
    if value["state"].as_str() != Some(expected_state)
        || value["lease"]["schema_version"].as_u64() != Some(1)
        || value["lease"]["source"].as_str() != Some("codex-systemd-sentinel-v1")
    {
        return Err("Codex slot sentinel returned an unbound result".to_string());
    }
    Ok(value["lease"].clone())
}

fn plan_codex_sentinel(root: &Path, slot: &str, slot_dir: &Path) -> Result<Value, String> {
    let output = Command::new(sentinel_tool(root)?)
        .args(["plan", "--slot", slot, "--working-directory"])
        .arg(slot_dir)
        .output()
        .map_err(|error| format!("plan Codex slot sentinel: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Codex slot sentinel planning refused: {}{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse Codex sentinel plan: {error}"))?;
    let plan = value["plan"].clone();
    if value["state"].as_str() != Some("planned")
        || plan["schema_version"].as_u64() != Some(1)
        || plan["source"].as_str() != Some("codex-systemd-sentinel-v1")
        || plan["slot"].as_str() != Some(slot)
        || plan["working_directory"].as_str() != slot_dir.to_str()
    {
        return Err("planned Codex sentinel does not bind the exact slot".to_string());
    }
    Ok(plan)
}

fn launch_codex_sentinel(root: &Path, plan: &Value) -> Result<Value, String> {
    let plan_json = serde_json::to_string(plan)
        .map_err(|error| format!("serialize Codex sentinel plan: {error}"))?;
    let output = Command::new(sentinel_tool(root)?)
        .args(["launch", "--plan-json", &plan_json])
        .output()
        .map_err(|error| format!("launch Codex slot sentinel: {error}"))?;
    let lease = sentinel_result(output, "live")?;
    for field in [
        "schema_version",
        "source",
        "slot",
        "generation",
        "nonce",
        "unit",
        "working_directory",
    ] {
        if lease.get(field) != plan.get(field) {
            return Err(format!(
                "launched Codex sentinel changed planned field {field}"
            ));
        }
    }
    Ok(lease)
}

fn verify_codex_sentinel(root: &Path, lease: &Value) -> Result<Value, String> {
    let lease_json = serde_json::to_string(lease)
        .map_err(|error| format!("serialize Codex sentinel lease: {error}"))?;
    let output = Command::new(sentinel_tool(root)?)
        .args(["verify", "--lease-json", &lease_json])
        .output()
        .map_err(|error| format!("verify Codex slot sentinel: {error}"))?;
    let verified = sentinel_result(output, "live")?;
    if &verified != lease {
        return Err("Codex sentinel verification changed the exact recorded lease".to_string());
    }
    Ok(verified)
}

fn validate_codex_slot_binding(
    slot: &str,
    canonical_slot: &Path,
    value: &Value,
) -> Result<(), String> {
    if value["schema_version"].as_u64() != Some(1)
        || value["source"].as_str() != Some("codex-systemd-sentinel-v1")
        || value["slot"].as_str() != Some(slot)
        || value["working_directory"].as_str() != canonical_slot.to_str()
    {
        return Err(format!(
            "Codex sentinel identity does not bind exact canonical slot {}",
            canonical_slot.display()
        ));
    }
    Ok(())
}

fn validate_codex_intent_replay(slot_record: &Value, intent: &Value) -> Result<(), String> {
    let object = intent
        .as_object()
        .ok_or_else(|| "Codex sentinel launch intent is not an object".to_string())?;
    let expected_fields: std::collections::BTreeSet<&str> = [
        "schema_version",
        "source",
        "phase",
        "plan",
        "legacy_recovery",
        "recovery_note",
        "requested_agent",
        "recorded_at",
        "historical_slot",
        "slot_snapshot",
    ]
    .into_iter()
    .collect();
    if object
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>()
        != expected_fields
    {
        return Err("Codex sentinel launch intent has an inexact field set".to_string());
    }
    let legacy = intent["legacy_recovery"]
        .as_bool()
        .ok_or_else(|| "Codex sentinel launch intent has no legacy mode".to_string())?;
    let snapshot = intent["slot_snapshot"]
        .as_object()
        .map(|_| intent["slot_snapshot"].clone())
        .ok_or_else(|| "Codex sentinel launch intent has no exact slot snapshot".to_string())?;
    if snapshot.get("coordinator_lease").is_some()
        || snapshot.get("coordinator_lease_intent").is_some()
        || snapshot.get("coordinator_lease_revocation").is_some()
        || snapshot.get("release_journal").is_some()
        || snapshot.get("orphan_residue_journal").is_some()
    {
        return Err(
            "Codex sentinel slot snapshot already contains transaction authority".to_string(),
        );
    }
    if legacy && intent.get("historical_slot") != Some(&snapshot) {
        return Err(
            "legacy Codex sentinel intent does not preserve the exact historical slot".to_string(),
        );
    }
    if intent["recorded_at"]
        .as_str()
        .is_none_or(|recorded| recorded.is_empty())
    {
        return Err("Codex sentinel launch intent has no transaction timestamp".to_string());
    }
    let mut expected_inflight = snapshot;
    expected_inflight["status"] = json!("binding-coordinator-lease");
    expected_inflight["updated"] = intent["recorded_at"].clone();
    expected_inflight["coordinator_lease_intent"] = intent.clone();
    if &expected_inflight != slot_record {
        return Err(
            "current slot record drifted from the durable Codex launch snapshot".to_string(),
        );
    }
    Ok(())
}

fn assert_unique_sentinel_identity(
    state: &Value,
    candidate_slot: &str,
    candidate: &Value,
) -> Result<(), String> {
    let candidate_object = candidate
        .as_object()
        .ok_or_else(|| "Codex sentinel identity is not an object".to_string())?;
    let identity_fields = [
        "generation",
        "nonce",
        "unit",
        "invocation_id",
        "main_pid",
        "cgroup_path",
        "working_directory",
    ];
    for (other_slot, record) in state["slots"].as_object().into_iter().flatten() {
        if other_slot == candidate_slot {
            continue;
        }
        let mut identities = Vec::new();
        if let Some(lease) = record.get("coordinator_lease") {
            identities.push(("lease", lease));
        }
        if let Some(plan) = record
            .get("coordinator_lease_intent")
            .and_then(|intent| intent.get("plan"))
        {
            identities.push(("launch intent", plan));
        }
        for (kind, other) in identities {
            for field in identity_fields {
                let Some(value) = candidate_object.get(field) else {
                    continue;
                };
                if !value.is_null() && other.get(field) == Some(value) {
                    return Err(format!(
                        "Codex sentinel {field} collides with {kind} for slot {other_slot}"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn next_free_slot(root: &Path, state: &Value) -> String {
    let slots = state["slots"].as_object();
    for n in 1..=999 {
        let name = format!("slot{n:02}");
        let in_state = slots.map(|s| s.contains_key(&name)).unwrap_or(false);
        if !in_state && !root.join("worktrees/slots").join(&name).exists() {
            return name;
        }
    }
    die("no free slot found in slot01..slot999");
}

fn add_worktree(primary: &Path, dst: &Path, branch: Option<&str>, start: &str) {
    if dst.exists() {
        let canonical = exact_checkout_root(dst).unwrap_or_else(|error| {
            die(&format!(
                "path exists but is not the exact requested git worktree {}: {error}",
                dst.display()
            ))
        });
        let (listed, porcelain, error) = git(primary, &["worktree", "list", "--porcelain"]);
        if !listed {
            die(&format!(
                "could not inspect physical worktree registry for {}: {error}",
                primary.display()
            ));
        }
        let registrations = porcelain
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .filter(|candidate| Path::new(candidate) == canonical)
            .count();
        if registrations == 1 {
            println!("  adopt existing worktree {}", dst.display());
            return;
        }
        die(&format!(
            "path exists but has {registrations} matching registrations in {}: {}",
            primary.display(),
            dst.display()
        ));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let dst_s = dst.to_string_lossy().to_string();
    let (ok, _, err) = match branch {
        Some(b) => git(primary, &["worktree", "add", "-b", b, &dst_s, start]),
        None => git(primary, &["worktree", "add", "--detach", &dst_s, start]),
    };
    if !ok {
        die(&format!(
            "git worktree add failed for {}: {err}",
            dst.display()
        ));
    }
    let canonical = exact_checkout_root(dst).unwrap_or_else(|error| {
        die(&format!(
            "new worktree {} failed exact-root verification: {error}",
            dst.display()
        ))
    });
    let (listed, porcelain, error) = git(primary, &["worktree", "list", "--porcelain"]);
    let registrations = if listed {
        porcelain
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .filter(|candidate| Path::new(candidate) == canonical)
            .count()
    } else {
        die(&format!(
            "could not verify physical worktree registry for {}: {error}",
            primary.display()
        ));
    };
    if registrations != 1 {
        die(&format!(
            "new worktree {} has {registrations} matching physical registrations",
            dst.display()
        ));
    }
    println!("  created {}", dst.display());
    initialize_required_submodules(dst);
}

/// Submodules a new product worktree needs in order to BUILD.
///
/// `git worktree add` does not populate submodules, and this allocator never
/// asked it to, so a slot arrived with an EMPTY `agent-utils/` and self-tests
/// died in under a second on a dependency error that reads like a product
/// failure. Measured 2026-08-27 across the 61 slot hermit worktrees on disk:
/// 27 (44%) had an empty `agent-utils`. The 34 that were populated were
/// populated BY HAND, by whichever agent happened to need it and worked out why.
///
/// A slot that looks allocated and cannot build is worse than no slot, so this
/// belongs in the allocator rather than in a step somebody has to remember.
///
/// `third-party/rr` is deliberately NOT here. It is a large clone, it was empty
/// in 50 of the same 61 worktrees, and almost nothing needs it; initializing it
/// unconditionally would cost every allocation for a handful of callers.
const REQUIRED_SUBMODULES: &[(&str, &str)] = &[("hermit", "agent-utils")];

/// Populate the build-required submodules of a freshly created worktree.
///
/// ADVISORY, never fatal, matching this allocator's existing convention that
/// housekeeping does not block an allocation. A failure here leaves exactly the
/// state that existed before this function did — an empty submodule directory —
/// so refusing the whole allocation would be a strictly worse outcome than the
/// bug being fixed. It is printed loudly instead, because the failure it
/// replaces was silent.
///
/// Costs nothing on a normal box: the objects come from the parent's own
/// `.git/modules/...` through a git alternates file, so no network is involved.
/// Measured 2026-08-27: 5 seconds, populating 19 entries, with the proxy unset.
fn initialize_required_submodules(dst: &Path) {
    let Some(product) = dst.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    for (owner, submodule) in REQUIRED_SUBMODULES {
        if *owner != product {
            continue;
        }
        // Only when it is actually empty: re-running allocation to adopt a slot
        // must not disturb a submodule the owner has already checked out, and
        // may have moved to a different commit on purpose.
        let path = dst.join(submodule);
        let populated = std::fs::read_dir(&path)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false);
        if populated {
            continue;
        }
        let (ok, _, err) = git(dst, &["submodule", "update", "--init", submodule]);
        if ok {
            println!("  initialized {product} submodule {submodule}");
        } else {
            eprintln!(
                "⚠  could not initialize {product} submodule {submodule} in {}: {}\n   \
                 The slot IS allocated. Until this is populated, builds and self-tests in it \
                 fail immediately on a missing dependency, which reads like a product failure \
                 rather than an incomplete checkout. Re-run:\n     \
                 git -C {} submodule update --init {submodule}",
                dst.display(),
                err.trim(),
                dst.display(),
            );
        }
    }
}

fn includes_product(selection: &str, product: &str) -> bool {
    selection == "all"
        || selection == product
        || (selection == "both" && matches!(product, "hermit" | "reverie"))
}

fn primary_start(root: &Path, product: &str, override_start: Option<&str>) -> String {
    if let Some(start) = override_start {
        return start.to_string();
    }
    let (ok, out, _) = git(&root.join(product), &["rev-parse", "--abbrev-ref", "HEAD"]);
    if ok && !out.is_empty() {
        out
    } else {
        "HEAD".to_string()
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut agent = String::new();
    let mut slot = String::new();
    let mut task = String::new();
    let mut product = "all".to_string();
    let mut hermit_branch: Option<String> = None;
    let mut reverie_branch: Option<String> = None;
    let mut liteinst2_branch: Option<String> = None;
    let mut start_point: Option<String> = None;
    let mut purpose = String::new();
    let mut read_mostly = false;
    let mut check_only = false;
    let mut migrate_layout_requested = false;
    let mut repair = false;
    let mut dry_run = false;
    let mut excluded_slots = BTreeSet::new();
    let mut codex_systemd_sentinel = false;
    let mut recover_legacy_unbound_owner = false;
    let mut recovery_note = String::new();

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
            "--agent" => agent = take(&mut i, &argv, "--agent"),
            "--slot" => slot = take(&mut i, &argv, "--slot"),
            "--task" => task = take(&mut i, &argv, "--task"),
            "--product" => product = take(&mut i, &argv, "--product"),
            "--hermit-branch" => hermit_branch = Some(take(&mut i, &argv, "--hermit-branch")),
            "--reverie-branch" => reverie_branch = Some(take(&mut i, &argv, "--reverie-branch")),
            "--liteinst2-branch" => {
                liteinst2_branch = Some(take(&mut i, &argv, "--liteinst2-branch"))
            }
            "--start-point" => start_point = Some(take(&mut i, &argv, "--start-point")),
            "--purpose" => purpose = take(&mut i, &argv, "--purpose"),
            "--i-promise-this-agent-is-read-mostly" => read_mostly = true,
            "--codex-systemd-sentinel" => codex_systemd_sentinel = true,
            "--recover-legacy-unbound-owner" => recover_legacy_unbound_owner = true,
            "--recovery-note" => recovery_note = take(&mut i, &argv, "--recovery-note"),
            "--check-only" => check_only = true,
            "--migrate-layout" => migrate_layout_requested = true,
            "--repair" | "--sync" => repair = true,
            "--dry-run" => dry_run = true,
            "--exclude-slot" => {
                excluded_slots.insert(take(&mut i, &argv, "--exclude-slot"));
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            other => die(&format!("unknown argument: {other}\n\n{USAGE}")),
        }
        i += 1;
    }

    if migrate_layout_requested {
        if repair
            || check_only
            || !agent.is_empty()
            || !slot.is_empty()
            || !task.is_empty()
            || product != "all"
            || hermit_branch.is_some()
            || reverie_branch.is_some()
            || liteinst2_branch.is_some()
            || start_point.is_some()
            || !purpose.is_empty()
            || read_mostly
            || codex_systemd_sentinel
            || recover_legacy_unbound_owner
            || !recovery_note.is_empty()
            || !excluded_slots.is_empty()
        {
            die("--migrate-layout must be run by itself, optionally with --dry-run");
        }
        let root = find_root();
        let _registry_lock = lock_registry(&root);
        migrate_layout(&root, dry_run);
    }

    // Single-writer REPAIR/SYNC: reconcile recorded branch cells from physical
    // porcelain (never touches git objects or ownership). Runs before the normal
    // allocation path so it cannot be confused with a slot request.
    if repair {
        if !slot.is_empty() && !excluded_slots.is_empty() {
            die("--repair --slot and --exclude-slot are mutually exclusive");
        }
        if !slot.is_empty() && !valid_slot(&slot) {
            die(&format!("invalid repair --slot value {slot:?}"));
        }
        for excluded in &excluded_slots {
            if !valid_slot(excluded) {
                die(&format!("invalid --exclude-slot value {excluded:?}"));
            }
        }
        let root = find_root();
        let _registry_lock = lock_registry(&root);
        repair_registry(
            &root,
            dry_run,
            (!slot.is_empty()).then_some(slot.as_str()),
            &excluded_slots,
        );
    }
    if dry_run {
        die("--dry-run is valid only with --repair");
    }
    if !excluded_slots.is_empty() {
        die("--exclude-slot is valid only with --repair");
    }

    // Health check only: report homeostasis and exit without allocating.
    if check_only {
        let root = find_root();
        let _registry_lock = lock_registry(&root);
        homeostasis_check(&root);
        let checker = root.join("scripts/check-worktree-registry.rs");
        let root_arg = root.to_string_lossy().into_owned();
        let status = Command::new(&checker)
            .args(["--root", root_arg.as_str()])
            .status()
            .unwrap_or_else(|error| die(&format!("run {}: {error}", checker.display())));
        if !status.success() {
            exit(status.code().unwrap_or(1));
        }
        return;
    }

    if agent.is_empty() {
        die(&format!("--agent is required\n\n{USAGE}"));
    }
    if recover_legacy_unbound_owner && !codex_systemd_sentinel {
        die("--recover-legacy-unbound-owner requires --codex-systemd-sentinel");
    }
    if recover_legacy_unbound_owner && recovery_note.trim().is_empty() {
        die("--recover-legacy-unbound-owner requires a non-empty --recovery-note");
    }
    if !recover_legacy_unbound_owner && !recovery_note.is_empty() {
        die("--recovery-note is valid only with --recover-legacy-unbound-owner");
    }
    if !matches!(
        product.as_str(),
        "hermit" | "reverie" | "liteinst2" | "both" | "all"
    ) {
        die("--product must be hermit, reverie, liteinst2, both, or all");
    }

    let include_hermit = includes_product(&product, "hermit");
    let include_reverie = includes_product(&product, "reverie");
    let include_liteinst2 = includes_product(&product, "liteinst2");
    if hermit_branch.is_some() && !include_hermit {
        die("--hermit-branch requires --product hermit, both, or all");
    }
    if reverie_branch.is_some() && !include_reverie {
        die("--reverie-branch requires --product reverie, both, or all");
    }
    if liteinst2_branch.is_some() && !include_liteinst2 {
        die("--liteinst2-branch requires --product liteinst2 or all");
    }

    let root = find_root();
    // Event-driven cleanup mirrors the herdr pane lifecycle: every new create
    // first offers expired leases to the canonical fail-closed reaper. The
    // reaper acquires the registry lock per exact generation, so invoke it
    // before this allocator takes its own single-writer lock.
    reap_expired_slot_leases(&root, &agent);
    let _registry_lock = lock_registry(&root);
    let mut state = load_state(&root);

    // Default slot name: agent name with a leading 'hermit-' stripped.
    if slot.is_empty() {
        let candidate = agent.strip_prefix("hermit-").unwrap_or(&agent).to_string();
        slot = if valid_slot(&candidate) {
            candidate
        } else {
            next_free_slot(&root, &state)
        };
        println!("selected slot: {slot}");
    }
    if !valid_slot(&slot) {
        die(&format!(
            "invalid slot name: '{slot}' (expected [a-z0-9-]+ or slotNN)"
        ));
    }
    if let Some(existing) = state["slots"].get(&slot) {
        validate_canonical_slot_paths(&slot, existing).unwrap_or_else(|error| die(&error));
        if existing["status"].as_str() == Some("releasing")
            || existing.get("release_journal").is_some()
            || existing.get("orphan_residue_journal").is_some()
            || existing.get("coordinator_lease_revocation").is_some()
        {
            die(&format!(
                "slot {slot} has an unfinished release transaction; run release-worktree.rs --clean --recover-submodule-cleanup before allocation or re-adoption"
            ));
        }
    }

    let existing_slot = state["slots"].get(&slot).cloned();
    let existing_sentinel = existing_slot
        .as_ref()
        .and_then(|record| record.get("coordinator_lease"))
        .cloned();
    let existing_sentinel_intent = existing_slot
        .as_ref()
        .and_then(|record| record.get("coordinator_lease_intent"))
        .cloned();
    let existing_agents = existing_slot
        .as_ref()
        .and_then(|record| record["agents"].as_array());
    let existing_has_orc_lease = existing_agents.is_some_and(|agents| {
        agents
            .iter()
            .any(|owner| owner.get("tmux_pane_id").is_some() || owner.get("cgroup_path").is_some())
    });
    let existing_needs_binding = existing_slot.is_some() && existing_sentinel.is_none();
    if codex_systemd_sentinel {
        if existing_sentinel.is_some() && existing_sentinel_intent.is_some() {
            die("slot mixes a finalized Codex coordinator lease with an unfinished launch intent");
        }
        if existing_sentinel.is_some() && existing_has_orc_lease {
            die("slot mixes a finalized Codex coordinator lease with ORC pane/cgroup authority");
        }
        if recover_legacy_unbound_owner && !existing_needs_binding {
            die("legacy-unbound recovery requires an existing slot with no coordinator lease");
        }
        if existing_needs_binding
            && existing_sentinel_intent.is_none()
            && !recover_legacy_unbound_owner
        {
            die("existing slot without a coordinator lease requires explicit --recover-legacy-unbound-owner; refusing to imply that the current Codex thread is its historical owner");
        }
        if recover_legacy_unbound_owner {
            if read_mostly
                || !task.is_empty()
                || !purpose.is_empty()
                || hermit_branch.is_some()
                || reverie_branch.is_some()
                || liteinst2_branch.is_some()
                || start_point.is_some()
            {
                die("legacy coordinator binding is identity-only; do not combine it with task/purpose/branch/start/read-mostly changes");
            }
            let recorded = existing_agents
                .into_iter()
                .flatten()
                .filter(|owner| owner["name"].as_str() == Some(&agent))
                .count();
            if recorded != 1 {
                die("legacy coordinator binding requires --agent to name exactly one recorded historical owner");
            }
        }
        if let Some(intent) = &existing_sentinel_intent {
            let intent_recovery = intent["legacy_recovery"].as_bool();
            if intent["schema_version"].as_u64() != Some(1)
                || intent["source"].as_str() != Some("codex-systemd-sentinel-v1")
                || intent["phase"].as_str() != Some("launch-planned")
                || existing_slot
                    .as_ref()
                    .and_then(|slot| slot["status"].as_str())
                    != Some("binding-coordinator-lease")
                || intent["requested_agent"].as_str() != Some(&agent)
                || intent_recovery.is_none()
                || intent_recovery != Some(recover_legacy_unbound_owner)
                || (recover_legacy_unbound_owner
                    && intent["recovery_note"].as_str() != Some(recovery_note.as_str()))
                || (recover_legacy_unbound_owner && !intent["historical_slot"].is_object())
                || (!recover_legacy_unbound_owner
                    && (!intent["recovery_note"].is_null() || !intent["historical_slot"].is_null()))
            {
                die("existing Codex sentinel launch intent does not match this exact binding request");
            }
            validate_codex_intent_replay(
                existing_slot
                    .as_ref()
                    .expect("a launch intent has an existing slot"),
                intent,
            )
            .unwrap_or_else(|error| die(&error));
        }
    } else if existing_sentinel.is_some() || existing_sentinel_intent.is_some() {
        die("slot has a coordinator-held Codex sentinel; re-run with --codex-systemd-sentinel to verify the same generation");
    }

    // 1-1 mapping: refuse if this mutating agent already owns a different slot.
    //
    // A RELEASED slot does not count, for exactly the reason spelled out at the
    // collision check below: release-worktree.rs retains `agents` as the
    // historical record and records the disposition in `status`, so reading
    // `agents` alone reports ownership of a slot that was given up. Here that
    // mismatch made the refusal UNSATISFIABLE: it told you to
    // "Release it first (scripts/release-worktree.rs --slot X)" when you already
    // had -- and releasing again can never help, because release deliberately
    // keeps the row. An agent that released its slot could then never allocate
    // another one. Hit live 2026-08-08 after releasing rev1864.
    //
    // `owner-lease-revoked` IS ALSO DISCOUNTED HERE, AND ONLY HERE. This check and
    // the collision check below answer DIFFERENT questions, so matching them
    // "exactly" was the bug:
    //
    //   collision check   "may agent B take slot X?"      revoked must still REFUSE
    //                     -- a revoked slot's disposition is unresolved and it may
    //                     still hold work; that safety property is unchanged and is
    //                     bracketed by allocate-worktree-released-rebind-test.sh.
    //   this check        "may agent A, who owns revoked slot X, take a NEW slot Y?"
    //                     -- refusing here imprisons A. It cannot use X (revoked),
    //                     cannot release X (revoked), and the reaper skips X because
    //                     it predates TTL leases. There is no path out from inside.
    //
    // That is the SAME UNSATISFIABLE-REFUSAL defect already fixed for `released`
    // directly above: the error names a remedy the agent cannot perform. Measured
    // 2026-08-12: six slots were owner-lease-revoked -- codex-rescue, codex-tickgate,
    // codex-validatelock, slot04, slot74, w7 -- each imprisoning exactly its own
    // owner, and one of them blocked the owner's stated priority task.
    //
    // NOTE WHAT IS NOT WIDENED. `active` and `releasing` still count as owning, so a
    // live slot still blocks a second allocation. This removes an impossible demand;
    // it does not make the allocator permissive.
    if !read_mostly {
        if let Some(slots) = state["slots"].as_object() {
            for (other, s) in slots {
                if other == &slot {
                    continue;
                }
                let status = s["status"]
                    .as_str()
                    .and_then(|value| worktree_registry::SlotStatus::parse(value).ok())
                    .unwrap_or_else(|| {
                        die(&format!("validated slot '{other}' lost its typed status"))
                    });
                if !status.blocks_new_allocation() {
                    continue;
                }
                let owns = s["agents"]
                    .as_array()
                    .map(|a| {
                        a.iter().any(|x| {
                            x["name"].as_str() == Some(&agent)
                                && !x["read_only"].as_bool().unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);
                if owns {
                    die(&format!(
                        "agent '{agent}' already owns slot '{other}' (1 slot per agent).\n\
                         Release it first (scripts/release-worktree.rs --slot {other}) \
                         or reuse it."
                    ));
                }
            }
        }
    }

    // Collision check against existing owner of the target slot.
    //
    // A RELEASED slot has no owner, even though its `agents` array is still
    // populated. release-worktree.rs records the disposition in `status` and
    // deliberately retains `agents` as the historical record of who held it --
    // so reading `agents` alone reports an owner for a slot that was given up.
    // That mismatch is not theoretical: it is why the `validate-rust` slot could
    // not be rebound to a live agent after a release, and why 14 dead-owner
    // slots stayed pinned against the 12-active cap with no way to reclaim them
    // (quartermaster, 2026-08-08 -- "the tooling refused every attempt").
    //
    // Only `released` is treated as unowned. `owner-lease-revoked`,
    // `releasing`, and anything else keep refusing: a revoked or half-finished
    // release is NOT a clean handover, and guessing otherwise is exactly the
    // failure this collision check exists to prevent.
    // Set when a NEW mutating agent takes over a RELEASED slot. The takeover
    // must REPLACE the released predecessor, not join it -- see the mutation
    // site below, where this is applied.
    let mut replace_released_predecessor = false;
    if let Some(existing) = state["slots"].get(&slot) {
        let released = existing["status"].as_str() == Some("released");
        let owner = existing["agents"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|x| !x["read_only"].as_bool().unwrap_or(false))
            })
            .and_then(|x| x["name"].as_str())
            .map(|s| s.to_string());
        if let Some(owner) = owner {
            if owner != agent && !read_mostly && !released {
                die(&format!(
                    "slot {slot} is owned by '{owner}'. Refusing collision.\n\
                     Use a different --slot, or pass --i-promise-this-agent-is-read-mostly \
                     to share read-only."
                ));
            }
            if owner != agent && released {
                println!(
                    "slot {slot} was released by '{owner}'; re-allocating to '{agent}' \
                     (status=released means the slot was given up, not that it is in use)."
                );
                // Only a MUTATING arrival replaces. A read-mostly sharer is not
                // taking ownership, so it has nothing to replace and must leave
                // the historical record exactly as found.
                replace_released_predecessor = !read_mostly;
            }
        }
    }

    let slot_dir = root.join("worktrees/slots").join(&slot);

    // An existing finalized sentinel is the authority for every later
    // allocation mutation. Verify its exact live identity and cross-slot
    // uniqueness before creating or adopting even one product worktree, so a
    // dead/restarted/colliding lease cannot leave physical registry drift.
    let verified_codex_lease = if codex_systemd_sentinel {
        existing_sentinel.as_ref().map(|lease| {
            let canonical_slot = fs::canonicalize(&slot_dir).unwrap_or_else(|error| {
                die(&format!(
                    "canonicalize existing Codex sentinel slot before allocation: {error}"
                ))
            });
            validate_codex_slot_binding(&slot, &canonical_slot, lease)
                .unwrap_or_else(|error| die(&error));
            verify_codex_sentinel(&root, lease)
                .unwrap_or_else(|error| die(&format!("existing Codex sentinel refused: {error}")))
        })
    } else {
        None
    };
    if let Some(lease) = &verified_codex_lease {
        assert_unique_sentinel_identity(&state, &slot, lease).unwrap_or_else(|error| die(&error));
    }

    println!(
        "Allocating worktrees/slots/{slot} for agent '{agent}' (product={product}, start={})",
        start_point.as_deref().unwrap_or("each primary HEAD")
    );

    if include_hermit && !recover_legacy_unbound_owner && existing_sentinel_intent.is_none() {
        let start = primary_start(&root, "hermit", start_point.as_deref());
        add_worktree(
            &root.join("hermit"),
            &slot_dir.join("hermit"),
            hermit_branch.as_deref(),
            &start,
        );
    }
    if include_reverie && !recover_legacy_unbound_owner && existing_sentinel_intent.is_none() {
        let rstart = primary_start(&root, "reverie", start_point.as_deref());
        add_worktree(
            &root.join("reverie"),
            &slot_dir.join("reverie"),
            reverie_branch.as_deref(),
            &rstart,
        );
    }
    if include_liteinst2 && !recover_legacy_unbound_owner && existing_sentinel_intent.is_none() {
        let lstart = primary_start(&root, "liteinst2", start_point.as_deref());
        add_worktree(
            &root.join("liteinst2"),
            &slot_dir.join("liteinst2"),
            liteinst2_branch.as_deref(),
            &lstart,
        );
    }

    // ORC agents retain the existing exact pane/cgroup adoption. Codex uses a
    // separate slot-level coordinator sentinel because logical Codex threads
    // share process/pane/cgroup identity. A new sentinel is always preceded by
    // a durable exact launch intent, so a crash cannot leave an unowned unit.
    let codex_plan = if codex_systemd_sentinel && verified_codex_lease.is_none() {
        let canonical_slot = fs::canonicalize(&slot_dir)
            .unwrap_or_else(|error| die(&format!("canonicalize Codex sentinel slot: {error}")));
        let plan = existing_sentinel_intent
            .as_ref()
            .and_then(|intent| intent.get("plan"))
            .cloned()
            .unwrap_or_else(|| {
                plan_codex_sentinel(&root, &slot, &canonical_slot)
                    .unwrap_or_else(|error| die(&format!("plan Codex slot sentinel: {error}")))
            });
        validate_codex_slot_binding(&slot, &canonical_slot, &plan)
            .unwrap_or_else(|error| die(&error));
        assert_unique_sentinel_identity(&state, &slot, &plan).unwrap_or_else(|error| die(&error));
        Some(plan)
    } else {
        None
    };
    // Try the caller's own process ancestry FIRST. It needs no external
    // producer, so it still binds when the agent snapshot is stale or broken --
    // which is the condition under which slots were being recorded permanently
    // unassessable. The snapshot path stays as the fallback for allocations
    // performed on behalf of another agent, and its refusal is what gets
    // reported when neither can bind.
    let observed_owner_lease = (!codex_systemd_sentinel).then(|| {
        observe_caller_owner_lease(&root, &agent).or_else(|caller_error| {
            observe_owner_lease(&root, &agent).map_err(|snapshot_error| {
                format!("{snapshot_error}\n   Nor could this be bound from the caller's own process ancestry: {caller_error}")
            })
        })
    });
    if let Some(Err(error)) = &observed_owner_lease {
        // The remedy is carried by {error}, which now names the actual cause.
        // A blanket "re-run adoption after the owner is live" used to be
        // appended here unconditionally, and it is wrong advice whenever the
        // snapshot is the broken party — that is the sentence agents chased.
        eprintln!(
            "⚠  owner lease for '{agent}' was not refreshed: {error}\n   The slot is allocated but the lease is UNBOUND; release will refuse legacy/unbound ownership until it binds."
        );
    }

    // Merge/append this agent into the slot record. Resuming a durable launch
    // intent is deliberately non-mutating: the exact pre-crash allocation or
    // binding-only migration is completed before accepting any later changes.
    let now = now_iso();
    if existing_sentinel_intent.is_none() {
        let entry = state["slots"]
            .as_object_mut()
            .unwrap()
            .entry(slot.clone())
            .or_insert_with(|| {
                json!({
                    "agents": [],
                    "allocated": now.clone(),
                    "hermit_path": format!("worktrees/slots/{slot}/hermit"),
                    "reverie_path": format!("worktrees/slots/{slot}/reverie"),
                    "liteinst2_path": format!("worktrees/slots/{slot}/liteinst2"),
                })
            });
        if !recover_legacy_unbound_owner {
            entry["hermit_path"] = json!(format!("worktrees/slots/{slot}/hermit"));
            entry["reverie_path"] = json!(format!("worktrees/slots/{slot}/reverie"));
            entry["liteinst2_path"] = json!(format!("worktrees/slots/{slot}/liteinst2"));
            entry["status"] = json!("active");
            entry["updated"] = json!(now.clone());
            if let Some(lease) = &verified_codex_lease {
                entry["coordinator_lease"] = lease.clone();
            }
            // Slot-level task/purpose/branches describe the mutating OWNER. A
            // read-mostly sharer records its own task but cannot clobber these.
            if !read_mostly {
                if !task.is_empty() {
                    entry["task"] = json!(task.clone());
                }
                if !purpose.is_empty() {
                    entry["purpose"] = json!(purpose.clone());
                }
                if let Some(b) = &hermit_branch {
                    entry["hermit_branch"] = json!(b);
                }
                if let Some(b) = &reverie_branch {
                    entry["reverie_branch"] = json!(b);
                }
                if let Some(b) = &liteinst2_branch {
                    entry["liteinst2_branch"] = json!(b);
                }
            }
            if entry.get("hermit_branch").is_none()
                || (include_hermit && entry["hermit_branch"] == "-")
            {
                entry["hermit_branch"] = json!(if include_hermit { "detached" } else { "-" });
            }
            if entry.get("reverie_branch").is_none()
                || (include_reverie && entry["reverie_branch"] == "-")
            {
                entry["reverie_branch"] = json!(if include_reverie { "detached" } else { "-" });
            }
            if entry.get("liteinst2_branch").is_none()
                || (include_liteinst2 && entry["liteinst2_branch"] == "-")
            {
                entry["liteinst2_branch"] = json!(if include_liteinst2 { "detached" } else { "-" });
            }
            let agents = entry["agents"].as_array_mut().unwrap();
            // A mutating takeover of a RELEASED slot REPLACES its predecessor.
            //
            // The collision check above deliberately treats `released` as
            // unowned, so it lets this allocation through. Appending anyway
            // produced TWO mutating owners -- and release-worktree.rs then
            // refuses to touch the slot at all ("must have exactly one mutating
            // owner, found 2"), so it wedges for BOTH agents: the arrival cannot
            // drop the predecessor's row without violating Hard Invariant 5, and
            // the predecessor is gone and cannot drop its own. Only a coordinator
            // could unstick it, via a non-obvious demote-then-drop. Observed on
            // slot rev1864, 2026-08-08.
            //
            // Scoped narrowly on purpose. This fires ONLY for a mutating arrival
            // at a slot whose owner already gave it up; an ACTIVE owner still
            // refuses at the collision check and is never touched here. A replace
            // that fired unconditionally would let one agent silently evict a
            // live one, which is strictly worse than the wedge it repairs. Both
            // directions are bracketed in allocate-worktree-released-rebind-test.sh.
            //
            // Read-only entries are retained: they do not violate the
            // one-mutating-owner invariant and are not what wedges release.
            // ARCHIVED.md, not this array, is the durable record of who held a
            // slot -- release-worktree.rs writes it before the status flips.
            if replace_released_predecessor {
                agents.retain(|a| {
                    a["read_only"].as_bool().unwrap_or(false) || a["name"].as_str() == Some(&agent)
                });
            }
            if let Some(a) = agents
                .iter_mut()
                .find(|a| a["name"].as_str() == Some(&agent))
            {
                a["read_only"] = json!(read_mostly);
                if !task.is_empty() {
                    a["task"] = json!(task.clone());
                }
                if let Some(observed) = &observed_owner_lease {
                    apply_observed_owner_lease(a, observed);
                }
            } else {
                let mut owner = json!({ "name": agent, "read_only": read_mostly, "task": task });
                if let Some(observed) = &observed_owner_lease {
                    apply_observed_owner_lease(&mut owner, observed);
                }
                agents.push(owner);
            }
            if !read_mostly && !codex_systemd_sentinel {
                match observed_owner_lease
                    .as_ref()
                    .expect("non-Codex allocation observes owner")
                {
                    Ok(lease) => {
                        let bound_task = entry["task"].as_str().unwrap_or("");
                        entry["owner_sidecar"] =
                            observed_owner_sidecar(&slot, &agent, bound_task, &now, lease)
                                .unwrap_or_else(|error| {
                                    die(&format!("publish slot-owner sidecar: {error}"))
                                });
                        // A later successful binding clears the record of the
                        // earlier failure: the row is provable again.
                        entry
                            .as_object_mut()
                            .expect("slot record is an object")
                            .remove("owner_binding_failure");
                    }
                    Err(error) => {
                        // A sidecar is authority for one exact supervisor
                        // incarnation. Failed adoption invalidates the old
                        // incarnation instead of leaving stale proof behind.
                        entry
                            .as_object_mut()
                            .expect("slot record is an object")
                            .remove("owner_sidecar");
                        // ...but removing it silently left the row BYTE-FOR-BYTE
                        // indistinguishable from a 2026-08-07 legacy row that
                        // predates the sidecar entirely. Those are opposite
                        // situations: a legacy row is permanently unprovable,
                        // whereas this one failed for a named, often transient
                        // reason and becomes provable the moment adoption is
                        // re-run. The authority answered the same
                        // "slot has no versioned owner_sidecar" for both, the
                        // only other trace was a line on stderr nobody keeps,
                        // and 21 rows accumulated without anything counting
                        // them. Record WHY and WHEN so a reader can tell the two
                        // apart and act. This does not make the row provable and
                        // is not evidence of ownership -- the authority still
                        // answers indeterminate and release still refuses.
                        entry["owner_binding_failure"] = json!({
                            "schema_version": 1,
                            "slot": slot,
                            "agent": agent,
                            "recorded_at": now.clone(),
                            "reason": error,
                        });
                    }
                }
            }
        }

        if let Some(plan) = &codex_plan {
            let slot_snapshot = entry.clone();
            entry["status"] = json!("binding-coordinator-lease");
            entry["updated"] = json!(now.clone());
            entry["coordinator_lease_intent"] = json!({
                "schema_version": 1,
                "source": "codex-systemd-sentinel-v1",
                "phase": "launch-planned",
                "plan": plan,
                "legacy_recovery": recover_legacy_unbound_owner,
                "recovery_note": if recover_legacy_unbound_owner { Value::String(recovery_note.clone()) } else { Value::Null },
                "requested_agent": agent,
                "recorded_at": now.clone(),
                "historical_slot": if recover_legacy_unbound_owner { existing_slot.clone().unwrap_or(Value::Null) } else { Value::Null },
                "slot_snapshot": slot_snapshot,
            });
        }
    }

    if let Some(plan) = &codex_plan {
        // First durable boundary: the exact generation/nonce/unit plan and, for
        // legacy migration, the untouched historical slot record are stored
        // before systemd-run can create anything.
        save_state(&root, &mut state);
        regen_active_md(&root, &state);
        let lease = launch_codex_sentinel(&root, plan)
            .unwrap_or_else(|error| die(&format!("create/recover Codex slot sentinel: {error}")));
        assert_unique_sentinel_identity(&state, &slot, &lease).unwrap_or_else(|error| die(&error));
        let entry = &mut state["slots"][&slot];
        let intent = entry["coordinator_lease_intent"].clone();
        entry["coordinator_lease"] = lease;
        entry["status"] = json!("active");
        entry["updated"] = json!(now_iso());
        entry
            .as_object_mut()
            .expect("slot record is an object")
            .remove("coordinator_lease_intent");
        if intent["legacy_recovery"].as_bool() == Some(true) {
            let history = entry
                .as_object_mut()
                .expect("slot record is an object")
                .entry("coordinator_lease_history")
                .or_insert_with(|| json!([]));
            let history = history
                .as_array_mut()
                .unwrap_or_else(|| die("coordinator_lease_history is not an array"));
            history.push(json!({
                "schema_version": 1,
                "mode": "binding-only-legacy-recovery",
                "recorded_at": now_iso(),
                "recovery_note": intent["recovery_note"].clone(),
                "historical_slot": intent["historical_slot"].clone(),
            }));
            // ORC fields remain preserved in historical_slot, but no second
            // active authority may coexist with the coordinator sentinel.
            for owner in entry["agents"].as_array_mut().into_iter().flatten() {
                if let Some(owner) = owner.as_object_mut() {
                    owner.remove("tmux_pane_id");
                    owner.remove("cgroup_path");
                }
            }
        }
    }

    if codex_systemd_sentinel {
        let entry = &mut state["slots"][&slot];
        let lease = entry["coordinator_lease"].clone();
        let mutating: Vec<&Value> = entry["agents"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|owner| !owner["read_only"].as_bool().unwrap_or(false))
            .collect();
        if mutating.len() != 1 {
            die(&format!(
                "slot-owner sidecar requires exactly one mutating owner, found {}",
                mutating.len()
            ));
        }
        let owner_agent = mutating[0]["name"]
            .as_str()
            .unwrap_or_else(|| die("mutating owner has no name"))
            .to_string();
        let owner_task = mutating[0]["task"].as_str().unwrap_or("").to_string();
        entry["owner_sidecar"] =
            codex_owner_sidecar(&root, &slot, &owner_agent, &owner_task, &now_iso(), &lease)
                .unwrap_or_else(|error| die(&format!("publish Codex slot-owner sidecar: {error}")));
        // Same reason as the ORC path: a row that has just bound owner evidence
        // must not keep an earlier attempt's failure record, which would
        // describe it as unbindable while it is provable.
        if let Some(object) = entry.as_object_mut() {
            object.remove("owner_binding_failure");
        }
    }

    save_state(&root, &mut state);
    regen_active_md(&root, &state);

    println!("\n✓ allocated worktrees/slots/{slot} to {agent}");
    println!("  state:   {}", state_path(&root).display());
    println!("  active:  {}", root.join("worktrees/ACTIVE.md").display());

    // Advisory workspace-health banner (never blocks allocation).
    homeostasis_check(&root);
    if hermit_branch.is_none() && include_hermit {
        println!(
            "  note: hermit worktree is DETACHED. Create a feature branch before editing:\n\
             \r        git -C worktrees/slots/{slot}/hermit switch -c <branch> origin/main"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_adoption_invalidates_prior_incarnation_lease() {
        let mut owner = json!({
            "name": "hermit-kvm",
            "read_only": false,
            "tmux_pane_id": "%old",
            "cgroup_path": "/agent.slice/old.scope"
        });
        apply_observed_owner_lease(&mut owner, &Err("snapshot unavailable".to_string()));
        assert!(owner.get("tmux_pane_id").is_none());
        assert!(owner.get("cgroup_path").is_none());
    }

    #[test]
    fn successful_adoption_replaces_prior_incarnation_lease() {
        let mut owner = json!({
            "name": "hermit-kvm",
            "read_only": false,
            "tmux_pane_id": "%old",
            "cgroup_path": "/agent.slice/old.scope"
        });
        apply_observed_owner_lease(
            &mut owner,
            &Ok(ObservedOwnerLease {
                pane: "%new".to_string(),
                cgroup: "/agent.slice/new.scope".to_string(),
                supervisor_pid: 42,
                boot_id: "boot-a".to_string(),
                start_ticks: 99,
            }),
        );
        assert_eq!(owner["tmux_pane_id"], "%new");
        assert_eq!(owner["cgroup_path"], "/agent.slice/new.scope");
    }

    #[test]
    fn owner_sidecar_binds_exact_supervisor_generation() {
        let lease = ObservedOwnerLease {
            pane: "%7".to_string(),
            cgroup: "/agent.slice/live.scope".to_string(),
            supervisor_pid: 123,
            boot_id: "boot-a".to_string(),
            start_ticks: 456,
        };
        let sidecar = observed_owner_sidecar(
            "slot07",
            "codex-7",
            "task-7",
            "2026-08-11T00:00:00Z",
            &lease,
        )
        .unwrap();
        assert_eq!(sidecar["schema_version"], 1);
        assert_eq!(sidecar["source"], ORC_OWNER_SIDECAR_SOURCE);
        assert_eq!(sidecar["slot"], "slot07");
        assert_eq!(sidecar["agent"], "codex-7");
        assert_eq!(sidecar["supervisor_pid"], 123);
        assert_eq!(sidecar["start_ticks"], 456);
        assert_eq!(sidecar["generation"], "boot-a:123:456");
        assert_eq!(sidecar["lease"]["schema_version"], 1);
        assert_eq!(sidecar["lease"]["resource"], "slot/slot07");
        assert_eq!(sidecar["lease"]["generation"], "boot-a:123:456");
        assert_eq!(sidecar["lease"]["ttl_seconds"], OWNER_LEASE_TTL_SECONDS);
        assert_eq!(sidecar["lease"]["state"], "active");
        assert_eq!(
            sidecar["lease"]["expires_at"].as_i64().unwrap(),
            sidecar["lease"]["renewed_at"].as_i64().unwrap() + OWNER_LEASE_TTL_SECONDS
        );
    }

    // The caller-ancestry path exists so a stalled agent snapshot cannot make a
    // slot permanently unassessable. Its ONE dangerous failure mode is binding
    // the wrong subject, so both guards are bracketed in both directions: the
    // admit case is asserted alongside each refusal, because guards that only
    // ever refuse would also pass on a path that never binds anything.

    #[test]
    fn caller_binding_admits_only_an_agent_claiming_its_own_slot() {
        assert!(caller_is_the_requested_owner("hermit-136", "hermit-136").is_ok());
        // Trailing whitespace from an exported variable is not a mismatch.
        assert!(caller_is_the_requested_owner(" hermit-136\n", "hermit-136").is_ok());
    }

    #[test]
    fn caller_binding_refuses_an_allocation_made_for_someone_else() {
        // The coordinator case: binding here would record the COORDINATOR's
        // supervisor as hermit-007's owner, and hermit-007's slot would read
        // dead the moment the coordinator exited.
        let error = caller_is_the_requested_owner("coordinator", "hermit-007")
            .expect_err("a caller must not bind a slot for a different agent");
        assert!(error.contains("coordinator"), "{error}");
        assert!(error.contains("hermit-007"), "{error}");
        assert!(error.contains("wrong subject"), "{error}");
    }

    #[test]
    fn caller_binding_refuses_when_the_caller_has_no_agent_identity() {
        for claimed in ["", "   ", "\n"] {
            let error = caller_is_the_requested_owner(claimed, "hermit-007")
                .expect_err("an unidentified caller must not bind an owner");
            assert!(error.contains("no DG_AGENT_NAME"), "{error}");
        }
    }

    #[test]
    fn pane_window_refuses_a_contradiction_but_tolerates_an_absent_name() {
        // Corroborates.
        assert!(pane_window_does_not_contradict("%30", "hermit-136", "hermit-136").is_ok());
        // Does not corroborate, does not contradict: guard 1 and the ancestry
        // still stand, so a pane ORC never named must not block its own agent.
        assert!(pane_window_does_not_contradict("%30", "", "hermit-136").is_ok());
        // Contradicts: tmux says this pane belongs to somebody else.
        let error = pane_window_does_not_contradict("%30", "hermit-104", "hermit-136")
            .expect_err("a pane named for another agent must refuse");
        assert!(error.contains("hermit-104"), "{error}");
        assert!(error.contains("hermit-136"), "{error}");
        assert!(error.contains("contradicts"), "{error}");
    }
}
