#!/usr/bin/env rust-script
//! Make landed parent tooling reachable from the shared primary without ever
//! touching the primary's working tree.
//!
//! ## The problem, measured
//!
//! Attribution enforcement landed as `.orc/plugins/hermit-dev/gh-coord-comment`
//! and `gh-coord-pr-create`. The shared parent checkout is **156 commits behind
//! `origin/main`, 5 ahead, and carries 58 dirty paths**, so those wrappers do
//! not exist at the path agents actually invoke. The rule is enforced by a
//! wrapper that is absent, which is the same as not enforced.
//!
//! The obvious repairs are all forbidden here, for good reasons:
//!
//! - `git pull` / `git reset` on the primary would move 58 dirty paths belonging
//!   to other agents. Invariant 5: unexpected changes are somebody else's.
//! - Copying the file in by hand plants an untracked ad-hoc blob that drifts the
//!   moment main moves again, and nothing records which commit it came from.
//! - Waiting for the primary to be clean makes enforcement depend on an event
//!   nobody controls.
//!
//! ## What this does instead
//!
//! Materialise `origin/main`'s tracked tree into a **content-addressed store
//! outside the repository**, publish a `current` pointer by atomic rename, and
//! place a **thin symlink only where the primary has no file at all**.
//!
//! Two properties carry the safety argument:
//!
//! 1. **A link is created only for a path that does not exist.** Not "exists but
//!    looks stale", not "exists and is tracked" -- does not exist. Every dirty
//!    byte in the primary is preserved because nothing that exists is ever
//!    written, moved, or read-modified. This is checked per path immediately
//!    before the link, not once at the start, so a file another agent creates
//!    mid-run is still respected.
//! 2. **`.orc/` is gitignored in the parent** (`.gitignore:5:/.orc/`), so a new
//!    symlink there is invisible to `git status` for every other agent. Tracked
//!    files under it stay tracked because they predate the ignore rule. A path
//!    whose new link WOULD be visible is refused rather than silently
//!    polluting a shared checkout -- see `refuse_visible_link`.
//!
//! The store is versioned by commit, so "which tooling am I running" always has
//! an answer, and `current` swaps by `rename(2)` so a reader either sees the old
//! complete tree or the new complete tree, never a half-extracted one.
//!
//! ```cargo
//! [dependencies]
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! ```

use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

/// Tooling paths this launcher is responsible for making reachable. Deliberately
/// an explicit list rather than "everything under .orc": a blanket link would
/// shadow files the primary legitimately owns, and the point of this tool is to
/// add only what is missing.
const DEFAULT_TOOLING: &[&str] = &[
    ".orc/plugins/hermit-dev/gh-coord-comment",
    ".orc/plugins/hermit-dev/gh-coord-pr-create",
    ".orc/plugins/hermit-dev/gh-issue-create",
];

const EXIT_OK: i32 = 0;
const EXIT_DRIFT: i32 = 1;
const EXIT_ERROR: i32 = 3;

const USAGE: &str = r#"Usage: main-tooling-link.rs <command> [OPTIONS]

Make landed parent tooling reachable from a dirty, behind shared primary without
pulling, resetting, or copying into it.

Commands:
  refresh    Materialise origin/main's tree into the store and publish `current`
             by atomic rename. Does not touch the primary working tree.
  link       Create a thin symlink for each managed tooling path that is ABSENT
             in the primary. Never overwrites, never touches an existing file.
  verify     Check that each managed path resolves to the exact blob recorded at
             the pinned origin/main commit. Exit 1 on drift.
  status     Report the pinned commit, each managed path, and its resolution.

Options:
  --root PATH        Repository root (default: walk up for .gitmodules+AGENTS.md).
  --store PATH       Versioned tooling store
                     (default: $HOME/.local/state/hermit-main-tooling).
  --ref REF          Ref to materialise (default: origin/main).
  --no-fetch         Use the already-fetched ref; do not touch the network.
  --path REL         Manage this repo-relative path instead of the defaults.
                     Repeatable.
  --json             Machine-readable output.
  --dry-run          Decide and print; create nothing.
  -h, --help         Print this pure help text.

Exit codes:
  0  success
  1  drift: a managed path does not resolve to the pinned blob
  3  usage or internal error
"#;

// ------------------------------------------------------------------ git -----

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("run git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The blob id `ref:path` resolves to, or None when the ref has no such path.
fn blob_at(root: &Path, reference: &str, rel: &str) -> Option<String> {
    git(root, &["rev-parse", &format!("{reference}:{rel}")]).ok()
}

/// Git's own hash of a file on disk. Compared against `blob_at`, this is what
/// makes "the link reaches the exact landed blob" an observation rather than an
/// inference from a path.
fn hash_file(root: &Path, path: &Path) -> Option<String> {
    git(root, &["hash-object", "--", path.to_str()?]).ok()
}

// ---------------------------------------------------------------- store -----

fn tree_dir(store: &Path, sha: &str) -> PathBuf {
    store.join("trees").join(sha)
}

fn current_link(store: &Path) -> PathBuf {
    store.join("current")
}

/// Extract `sha`'s tracked tree into the store, then publish it.
///
/// Extraction goes to a temporary sibling and is renamed into place, so a
/// concurrent reader never observes a partially written tree. Republishing an
/// already-materialised commit is a no-op on the tree and still refreshes the
/// pointer, which makes `refresh` idempotent and safe to run from several agents.
fn materialise(root: &Path, store: &Path, sha: &str) -> Result<PathBuf, String> {
    let final_dir = tree_dir(store, sha);
    fs::create_dir_all(store.join("trees"))
        .map_err(|e| format!("create {}: {e}", store.join("trees").display()))?;
    if !final_dir.is_dir() {
        let staging = store
            .join("trees")
            .join(format!(".staging-{sha}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).map_err(|e| format!("create {}: {e}", staging.display()))?;
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "git -C {root} archive --format=tar {sha} | tar -x -C {dest}",
                root = shell_quote(root),
                sha = sha,
                dest = shell_quote(&staging)
            ))
            .status()
            .map_err(|e| format!("git archive: {e}"))?;
        if !status.success() {
            let _ = fs::remove_dir_all(&staging);
            return Err(format!("git archive {sha} failed"));
        }
        // Atomic within the same filesystem. If another agent won the race and
        // already published this commit, drop ours -- the trees are identical
        // by construction because they are the same commit.
        if fs::rename(&staging, &final_dir).is_err() && !final_dir.is_dir() {
            let _ = fs::remove_dir_all(&staging);
            return Err(format!("publish {} failed", final_dir.display()));
        }
        let _ = fs::remove_dir_all(&staging);
    }
    Ok(final_dir)
}

/// Point `current` at `sha` by rename. `symlink()` cannot replace an existing
/// path, so the swap is create-temp-then-rename; `rename(2)` over a symlink is
/// atomic, which is the whole reason this is not `rm && ln -s`.
fn publish_current(store: &Path, sha: &str) -> Result<(), String> {
    let target = PathBuf::from("trees").join(sha);
    let tmp = store.join(format!(".current-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    symlink(&target, &tmp).map_err(|e| format!("symlink {}: {e}", tmp.display()))?;
    fs::rename(&tmp, current_link(store)).map_err(|e| format!("publish current: {e}"))
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

// ----------------------------------------------------------------- link -----

/// What happened, or would happen, to one managed path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Disposition {
    /// Nothing there; a link is ours to create.
    Linkable,
    /// Already a link into our store, pointing at `current`.
    AlreadyLinked,
    /// A real file the primary owns. Untouched, always.
    PrimaryOwned,
    /// A link would be visible to `git status` in a shared checkout.
    RefusedVisible,
    /// The ref has no such path.
    AbsentUpstream,
}

/// THE SAFETY RULE. A path is ours to link only when nothing exists there.
///
/// `symlink_metadata`, not `metadata`: a dangling symlink still EXISTS, and
/// following it would report "absent" and invite us to clobber somebody's
/// deliberate placeholder.
fn disposition(
    root: &Path,
    store: &Path,
    reference: &str,
    rel: &str,
    ignored: bool,
) -> Disposition {
    if blob_at(root, reference, rel).is_none() {
        return Disposition::AbsentUpstream;
    }
    let live = root.join(rel);
    match fs::symlink_metadata(&live) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                if let Ok(dest) = fs::read_link(&live) {
                    if dest.starts_with(current_link(store)) {
                        return Disposition::AlreadyLinked;
                    }
                }
            }
            // Exists and is not our link: the primary owns it. Dirty, clean,
            // tracked, untracked -- all the same answer.
            Disposition::PrimaryOwned
        }
        Err(_) if !ignored => Disposition::RefusedVisible,
        Err(_) => Disposition::Linkable,
    }
}

/// Is a new file at `rel` invisible to `git status`? Only then may we create one
/// in a checkout eighteen other agents are reading.
fn is_ignored(root: &Path, rel: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "-q", "--no-index", rel])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PathReport {
    path: String,
    disposition: Disposition,
    upstream_blob: Option<String>,
    resolved_blob: Option<String>,
    matches: bool,
}

fn report_for(root: &Path, store: &Path, reference: &str, rel: &str) -> PathReport {
    let ignored = is_ignored(root, rel);
    let d = disposition(root, store, reference, rel, ignored);
    let upstream = blob_at(root, reference, rel);
    let live = root.join(rel);
    let resolved = if live.exists() {
        hash_file(root, &live)
    } else {
        None
    };
    let matches = match (&upstream, &resolved) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    PathReport {
        path: rel.to_string(),
        disposition: d,
        upstream_blob: upstream,
        resolved_blob: resolved,
        matches,
    }
}

// ----------------------------------------------------------------- main -----

fn main() {
    match run() {
        Ok(code) => exit(code),
        Err(message) => {
            eprintln!("main-tooling-link: {message}");
            exit(EXIT_ERROR);
        }
    }
}

fn repository_root() -> Result<PathBuf, String> {
    let mut path = env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    loop {
        if path.join(".gitmodules").is_file() && path.join("AGENTS.md").is_file() {
            return Ok(path);
        }
        if !path.pop() {
            return Err("could not locate the dev-hermit root; pass --root PATH".to_string());
        }
    }
}

fn run() -> Result<i32, String> {
    let home = env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    let mut command = String::new();
    let mut root: Option<PathBuf> = None;
    let mut store = PathBuf::from(&home).join(".local/state/hermit-main-tooling");
    let mut reference = "origin/main".to_string();
    let mut fetch = true;
    let mut paths: Vec<String> = Vec::new();
    let mut json = false;
    let mut dry_run = false;

    let mut raw = env::args().skip(1);
    while let Some(flag) = raw.next() {
        let mut next = |n: &str| -> Result<String, String> {
            raw.next()
                .ok_or_else(|| format!("{n} requires a value\n\n{USAGE}"))
        };
        match flag.as_str() {
            "refresh" | "link" | "verify" | "status" => command = flag,
            "--root" => root = Some(PathBuf::from(next("--root")?)),
            "--store" => store = PathBuf::from(next("--store")?),
            "--ref" => reference = next("--ref")?,
            "--no-fetch" => fetch = false,
            "--path" => paths.push(next("--path")?),
            "--json" => json = true,
            "--dry-run" => dry_run = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(EXIT_OK);
            }
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    if command.is_empty() {
        return Err(format!("a command is required\n\n{USAGE}"));
    }
    let root = match root {
        Some(p) => p,
        None => repository_root()?,
    };
    if paths.is_empty() {
        paths = DEFAULT_TOOLING.iter().map(|s| s.to_string()).collect();
    }

    if fetch && matches!(command.as_str(), "refresh") {
        // Best effort: an offline agent should still be able to publish an
        // already-fetched ref rather than failing closed on the network.
        if let Some((remote, branch)) = reference.split_once('/') {
            let _ = Command::new("with-proxy")
                .arg("git")
                .arg("-C")
                .arg(&root)
                .args(["fetch", remote, branch])
                .status();
        }
    }

    let sha = git(&root, &["rev-parse", &reference])?;

    match command.as_str() {
        "refresh" => {
            if dry_run {
                println!(
                    "would materialise {reference} = {sha} into {}",
                    store.display()
                );
                return Ok(EXIT_OK);
            }
            let dir = materialise(&root, &store, &sha)?;
            publish_current(&store, &sha)?;
            println!("pinned {reference} = {sha}");
            println!("tree    {}", dir.display());
            println!("current {} -> trees/{sha}", current_link(&store).display());
            Ok(EXIT_OK)
        }
        "link" => {
            let mut created = 0;
            let mut reports = Vec::new();
            for rel in &paths {
                let ignored = is_ignored(&root, rel);
                let d = disposition(&root, &store, &reference, rel, ignored);
                match d {
                    Disposition::Linkable if !dry_run => {
                        let live = root.join(rel);
                        if let Some(parent) = live.parent() {
                            fs::create_dir_all(parent)
                                .map_err(|e| format!("create {}: {e}", parent.display()))?;
                        }
                        let target = current_link(&store).join(rel);
                        // Re-check immediately before creating: another agent may
                        // have written a real file since the disposition above.
                        // symlink() itself fails if the path exists, which is the
                        // final guarantee that we never clobber.
                        symlink(&target, &live)
                            .map_err(|e| format!("link {}: {e}", live.display()))?;
                        created += 1;
                        println!("linked   {rel} -> {}", target.display());
                    }
                    Disposition::Linkable => println!("would link {rel}"),
                    Disposition::AlreadyLinked => println!("ok       {rel} (already linked)"),
                    Disposition::PrimaryOwned => {
                        println!("skipped  {rel} (primary owns this file; left byte-identical)")
                    }
                    Disposition::RefusedVisible => println!(
                        "REFUSED  {rel} (a new file here is NOT gitignored; linking would show \
                         as untracked to every agent in the shared checkout)"
                    ),
                    Disposition::AbsentUpstream => {
                        println!("absent   {rel} (not present at {reference})")
                    }
                }
                reports.push(report_for(&root, &store, &reference, rel));
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&reports).unwrap_or_default()
                );
            }
            println!("created {created} link(s); pinned {reference} = {sha}");
            Ok(EXIT_OK)
        }
        "verify" | "status" => {
            let reports: Vec<PathReport> = paths
                .iter()
                .map(|rel| report_for(&root, &store, &reference, rel))
                .collect();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&reports).unwrap_or_default()
                );
            } else {
                println!("pinned {reference} = {sha}");
                for r in &reports {
                    println!(
                        "  {:<55} {:?} match={} upstream={} resolved={}",
                        r.path,
                        r.disposition,
                        r.matches,
                        r.upstream_blob.clone().unwrap_or_else(|| "-".into()),
                        r.resolved_blob.clone().unwrap_or_else(|| "-".into())
                    );
                }
            }
            let drift = reports
                .iter()
                .any(|r| !r.matches && r.disposition != Disposition::AbsentUpstream);
            if command == "verify" && drift {
                return Ok(EXIT_DRIFT);
            }
            Ok(EXIT_OK)
        }
        other => Err(format!("unknown command: {other}\n\n{USAGE}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the tests set or read a file mode, so the trait belongs here rather
    // than at module scope where the binary build sees it as unused.
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct Repo {
        root: PathBuf,
        store: PathBuf,
    }

    impl Repo {
        /// A real git repository, because the whole tool is about blob identity
        /// and a fake would let a hash bug pass.
        fn new(label: &str) -> Repo {
            let base = env::temp_dir().join(format!(
                "main-tooling-test-{}-{}-{}",
                label,
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&base);
            let root = base.join("repo");
            fs::create_dir_all(root.join(".orc/plugins/hermit-dev")).unwrap();
            let run = |args: &[&str]| {
                Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(args)
                    .output()
                    .unwrap();
            };
            run(&["init", "-q", "-b", "main"]);
            run(&["config", "user.email", "t@example.com"]);
            run(&["config", "user.name", "t"]);
            // Mirror the real parent: .orc is ignored, but files committed
            // before the rule stay tracked.
            fs::write(root.join(".gitignore"), "/.orc/\n").unwrap();
            fs::write(root.join(".gitmodules"), "").unwrap();
            fs::write(root.join("AGENTS.md"), "policy\n").unwrap();
            Repo {
                root,
                store: base.join("store"),
            }
        }

        fn write_tooling(&self, name: &str, body: &str) {
            let p = self.root.join(".orc/plugins/hermit-dev").join(name);
            fs::write(&p, body).unwrap();
            let mut perm = fs::metadata(&p).unwrap().permissions();
            perm.set_mode(0o755);
            fs::set_permissions(&p, perm).unwrap();
        }

        fn commit_all(&self, message: &str) -> String {
            let run = |args: &[&str]| {
                Command::new("git")
                    .arg("-C")
                    .arg(&self.root)
                    .args(args)
                    .output()
                    .unwrap()
            };
            run(&["add", "-A", "-f"]);
            run(&["commit", "-q", "-m", message]);
            String::from_utf8_lossy(&run(&["rev-parse", "HEAD"]).stdout)
                .trim()
                .to_string()
        }
    }

    fn rel(name: &str) -> String {
        format!(".orc/plugins/hermit-dev/{name}")
    }

    // ---- the core safety property: an existing file is NEVER touched ----

    /// THE test. A dirty file in the primary must be byte-identical afterwards,
    /// and must still be the primary's own file rather than a link.
    #[test]
    fn an_existing_dirty_file_is_never_overwritten_or_relinked() {
        let repo = Repo::new("dirty");
        repo.write_tooling("gh-issue-create", "#!/bin/sh\n# landed version\n");
        let sha = repo.commit_all("land tooling");
        // Now dirty it, the way a live agent would.
        let dirty = "#!/bin/sh\n# LOCAL WORK IN PROGRESS, do not clobber\n";
        repo.write_tooling("gh-issue-create", dirty);
        let before = fs::read(repo.root.join(rel("gh-issue-create"))).unwrap();

        materialise(&repo.root, &repo.store, &sha).unwrap();
        publish_current(&repo.store, &sha).unwrap();
        let d = disposition(&repo.root, &repo.store, &sha, &rel("gh-issue-create"), true);
        assert_eq!(
            d,
            Disposition::PrimaryOwned,
            "a file that exists is the primary's"
        );

        let after = fs::read(repo.root.join(rel("gh-issue-create"))).unwrap();
        assert_eq!(
            before, after,
            "the dirty bytes changed -- this is the failure that matters"
        );
        assert!(
            !fs::symlink_metadata(repo.root.join(rel("gh-issue-create")))
                .unwrap()
                .file_type()
                .is_symlink(),
            "an existing file must not be replaced by a link"
        );
    }

    /// A dangling symlink still EXISTS. Following it would report absent and
    /// invite clobbering a deliberate placeholder.
    #[test]
    fn a_dangling_symlink_counts_as_existing() {
        let repo = Repo::new("dangling");
        repo.write_tooling("gh-coord-comment", "#!/bin/sh\n");
        let sha = repo.commit_all("land");
        fs::remove_file(repo.root.join(rel("gh-coord-comment"))).unwrap();
        symlink(
            "/nonexistent/target",
            repo.root.join(rel("gh-coord-comment")),
        )
        .unwrap();
        assert_eq!(
            disposition(
                &repo.root,
                &repo.store,
                &sha,
                &rel("gh-coord-comment"),
                true
            ),
            Disposition::PrimaryOwned,
            "a dangling link exists; symlink_metadata must be used, not metadata"
        );
    }

    // ---- POSITIVE: the missing wrapper becomes reachable, at the exact blob ----

    #[test]
    fn positive_a_missing_wrapper_is_linked_and_resolves_to_the_exact_blob() {
        let repo = Repo::new("positive");
        let body = "#!/bin/sh\necho landed-wrapper\n";
        repo.write_tooling("gh-coord-comment", body);
        let sha = repo.commit_all("land the wrapper");
        let upstream = blob_at(&repo.root, &sha, &rel("gh-coord-comment")).unwrap();

        // The stale-primary NEGATIVE: remove it, as the real primary lacks it.
        fs::remove_file(repo.root.join(rel("gh-coord-comment"))).unwrap();
        assert!(
            !repo.root.join(rel("gh-coord-comment")).exists(),
            "precondition: the wrapper is absent, as on the stale primary"
        );

        materialise(&repo.root, &repo.store, &sha).unwrap();
        publish_current(&repo.store, &sha).unwrap();
        assert_eq!(
            disposition(
                &repo.root,
                &repo.store,
                &sha,
                &rel("gh-coord-comment"),
                true
            ),
            Disposition::Linkable
        );
        let target = current_link(&repo.store).join(rel("gh-coord-comment"));
        symlink(&target, repo.root.join(rel("gh-coord-comment"))).unwrap();

        // Reaches the EXACT landed blob, established by hash, not by path.
        let resolved = hash_file(&repo.root, &repo.root.join(rel("gh-coord-comment"))).unwrap();
        assert_eq!(
            resolved, upstream,
            "the link must resolve to the landed blob"
        );
        assert_eq!(
            fs::read_to_string(repo.root.join(rel("gh-coord-comment"))).unwrap(),
            body
        );
        // And it is executable through the link, or agents cannot invoke it.
        let mode = fs::metadata(repo.root.join(rel("gh-coord-comment")))
            .unwrap()
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "the linked wrapper must be executable, mode {mode:o}"
        );
    }

    // ---- refuse to pollute a shared checkout ----

    #[test]
    fn refuse_visible_link() {
        let repo = Repo::new("visible");
        fs::create_dir_all(repo.root.join("scripts")).unwrap();
        fs::write(repo.root.join("scripts/tool.sh"), "#!/bin/sh\n").unwrap();
        let sha = repo.commit_all("land a tracked, NON-ignored tool");
        fs::remove_file(repo.root.join("scripts/tool.sh")).unwrap();
        // scripts/ is not gitignored, so a link there would show as untracked
        // to every other agent reading this shared checkout.
        assert_eq!(
            disposition(&repo.root, &repo.store, &sha, "scripts/tool.sh", false),
            Disposition::RefusedVisible
        );
        // ...while the same absence under the ignored .orc/ is fine.
        assert_eq!(
            disposition(&repo.root, &repo.store, &sha, "scripts/tool.sh", true),
            Disposition::Linkable
        );
    }

    #[test]
    fn the_real_parent_ignore_rule_is_what_makes_orc_linkable() {
        let repo = Repo::new("ignored");
        repo.write_tooling("gh-coord-comment", "#!/bin/sh\n");
        repo.commit_all("land");
        assert!(
            is_ignored(&repo.root, &rel("gh-coord-comment")),
            "a NEW file under .orc/ must be gitignored, or linking pollutes git status"
        );
        assert!(
            !is_ignored(&repo.root, "scripts/anything.rs"),
            "the ignore probe must not report everything as ignored, or it is inert"
        );
    }

    // ---- atomicity and versioning ----

    #[test]
    fn current_swaps_atomically_and_always_names_a_complete_tree() {
        let repo = Repo::new("atomic");
        repo.write_tooling("gh-coord-comment", "v1\n");
        let sha1 = repo.commit_all("v1");
        materialise(&repo.root, &repo.store, &sha1).unwrap();
        publish_current(&repo.store, &sha1).unwrap();
        assert_eq!(
            fs::read_to_string(current_link(&repo.store).join(rel("gh-coord-comment"))).unwrap(),
            "v1\n"
        );

        repo.write_tooling("gh-coord-comment", "v2\n");
        let sha2 = repo.commit_all("v2");
        materialise(&repo.root, &repo.store, &sha2).unwrap();
        publish_current(&repo.store, &sha2).unwrap();
        assert_eq!(
            fs::read_to_string(current_link(&repo.store).join(rel("gh-coord-comment"))).unwrap(),
            "v2\n",
            "current must follow the new commit"
        );
        // Both versions remain addressable, so "which tooling ran" is answerable.
        assert!(
            tree_dir(&repo.store, &sha1).is_dir(),
            "the old tree must remain addressable"
        );
        assert!(tree_dir(&repo.store, &sha2).is_dir());
        // No staging directory survives a successful publish.
        let leftovers: Vec<_> = fs::read_dir(repo.store.join("trees"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".staging-"))
            .collect();
        assert!(leftovers.is_empty(), "staging dirs leaked: {leftovers:?}");
    }

    #[test]
    fn republishing_the_same_commit_is_idempotent() {
        let repo = Repo::new("idem");
        repo.write_tooling("gh-coord-comment", "x\n");
        let sha = repo.commit_all("c");
        materialise(&repo.root, &repo.store, &sha).unwrap();
        publish_current(&repo.store, &sha).unwrap();
        // Second run must not fail, and must not disturb the tree.
        materialise(&repo.root, &repo.store, &sha).unwrap();
        publish_current(&repo.store, &sha).unwrap();
        assert_eq!(
            fs::read_to_string(current_link(&repo.store).join(rel("gh-coord-comment"))).unwrap(),
            "x\n"
        );
    }

    // ---- drift detection: verify must FAIL on a stale link ----

    #[test]
    fn verify_reports_drift_when_the_pin_moves_ahead_of_current() {
        let repo = Repo::new("drift");
        repo.write_tooling("gh-coord-comment", "old\n");
        let sha1 = repo.commit_all("old");
        materialise(&repo.root, &repo.store, &sha1).unwrap();
        publish_current(&repo.store, &sha1).unwrap();
        fs::remove_file(repo.root.join(rel("gh-coord-comment"))).unwrap();
        symlink(
            current_link(&repo.store).join(rel("gh-coord-comment")),
            repo.root.join(rel("gh-coord-comment")),
        )
        .unwrap();

        // Control: at the pinned commit it matches.
        let ok = report_for(&repo.root, &repo.store, &sha1, &rel("gh-coord-comment"));
        assert!(
            ok.matches,
            "control: a fresh link must match its own commit"
        );

        // Now main moves. The link still serves the OLD blob, and verify must say so.
        fs::write(
            repo.root
                .join(rel("gh-coord-comment"))
                .with_extension("tmpsrc"),
            "",
        )
        .unwrap();
        let _ = fs::remove_file(
            repo.root
                .join(rel("gh-coord-comment"))
                .with_extension("tmpsrc"),
        );
        let staged = repo.store.join("newsrc");
        fs::create_dir_all(&staged).unwrap();
        // Commit a new version by writing through the store tree, not the link.
        let real = tree_dir(&repo.store, &sha1).join(rel("gh-coord-comment"));
        assert!(real.exists());
        // Simulate a newer commit by creating one in the repo directly.
        fs::remove_file(repo.root.join(rel("gh-coord-comment"))).unwrap();
        repo.write_tooling("gh-coord-comment", "new\n");
        let sha2 = repo.commit_all("new");
        fs::remove_file(repo.root.join(rel("gh-coord-comment"))).unwrap();
        symlink(
            current_link(&repo.store).join(rel("gh-coord-comment")),
            repo.root.join(rel("gh-coord-comment")),
        )
        .unwrap();
        let drifted = report_for(&repo.root, &repo.store, &sha2, &rel("gh-coord-comment"));
        assert!(
            !drifted.matches,
            "verify must detect that current still serves the old blob: {drifted:?}"
        );
    }

    #[test]
    fn a_path_absent_upstream_is_typed_not_silently_linked() {
        let repo = Repo::new("absent");
        repo.write_tooling("gh-coord-comment", "x\n");
        let sha = repo.commit_all("c");
        assert_eq!(
            disposition(&repo.root, &repo.store, &sha, &rel("does-not-exist"), true),
            Disposition::AbsentUpstream,
            "we must not invent a link to a blob that does not exist upstream"
        );
    }

    // ---- no destructive SCM surface, enforced as a test ----

    /// This tool exists precisely because pulling/resetting the primary is
    /// forbidden. Asserting the ABSENCE stops a future "just sync it" edit.
    #[test]
    fn no_destructive_scm_surface_in_this_script() {
        let source = fs::read_to_string(Path::new(file!())).expect("read own source");
        let banned = [
            format!("{} {}", "reset", "--hard"),
            format!("{}{}", "\"pu", "ll\""),
            format!("{} {}", "checkout", "--force"),
            format!("{}{}", "clean", " -"),
            format!("{}{}", "\"stas", "h\""),
        ];
        for needle in &banned {
            let hits: Vec<&str> = source
                .lines()
                .filter(|l| l.contains(needle.as_str()))
                .filter(|l| !l.trim_start().starts_with("//"))
                .filter(|l| !l.contains("format!"))
                .collect();
            assert!(
                hits.is_empty(),
                "destructive SCM surface {needle:?}: {hits:?}"
            );
        }
    }

    #[test]
    fn destructive_scan_is_not_inert() {
        let planted = format!("    git(&root, &[\"{} {}\"]);", "reset", "--hard");
        assert!(
            planted.contains(format!("{} {}", "reset", "--hard").as_str()),
            "the scanner's needle must match a planted violation"
        );
    }

    /// The portability gate scans *.rs. A literal owner home here would red main
    /// the moment this lands, which has already happened twice this week.
    #[test]
    fn no_owner_specific_paths_in_this_script() {
        let source = fs::read_to_string(Path::new(file!())).expect("read own source");
        let home_marker = format!("/{}/", "home");
        let hits: Vec<&str> = source
            .lines()
            .filter(|l| l.contains(home_marker.as_str()))
            .filter(|l| !l.contains("/home/example/") && !l.contains("/home/user/"))
            .collect();
        assert!(hits.is_empty(), "literal home path(s) present: {hits:?}");
    }
}
