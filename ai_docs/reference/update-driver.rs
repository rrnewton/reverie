#!/usr/bin/env rust-script
//! Reference-doc update driver: compute the "light cone" of change behind a
//! single `ai_docs/reference/*.md` doc and craft an agent prompt to update it.
//!
//! A reference doc declares, in machine-readable YAML front-matter, the commit
//! it is current as of (`tracks_sha` in `tracks_repo`) plus a `watch_files`
//! list of the source paths its content depends on. This driver:
//!
//!   1. parses that front-matter;
//!   2. finds the intervening commits that touched the watch-files since
//!      `tracks_sha` (the SEED set);
//!   3. EXPANDS to the full light cone of relevant causation — refactors add
//!      files and pull in new modules, so a watch-file audit alone under-counts.
//!      Expansion uses (a) partial Rust parsing of `mod` declarations (the
//!      precise "a new file entered the tree" signal, both at HEAD and as ADDED
//!      lines within the window) and (b) co-change coupling (files that changed
//!      in the same commits as a watch-file), producing a SUPERSET that is then
//!      winnowed by heuristic to same-crate Rust source;
//!   4. emits the relevant commit hashes; and
//!   5. crafts a readable, self-contained prompt instructing an agent to update
//!      the doc (and to further winnow the proposed watch-list with judgment —
//!      the "agent in the inner loop" step).
//!
//! Usage:
//!   ai_docs/reference/update-driver.rs <doc.md> [--json] [--quick]
//!                                      [--emit-prompt PATH] [--today YYYY-MM-DD]
//!
//!   --json          Emit the machine-readable analysis as JSON (no prompt).
//!   --quick         Staleness only: days-stale + seed-commit count, skip the
//!                   (more expensive) light-cone expansion. Used by
//!                   check-staleness.rs to scan the whole directory cheaply.
//!   --emit-prompt P Write the crafted agent prompt to file P (default: stdout).
//!   --today D       Override "today" (for deterministic testing).
//!
//! ```cargo
//! [dependencies]
//! serde_json = "1"
//! ```

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

const USAGE: &str = r#"Usage: ai_docs/reference/update-driver.rs <doc.md> [OPTIONS]

Compute the light cone of change behind one reference doc and craft an
agent prompt to update it.

Options:
  --json            Emit machine-readable JSON analysis instead of the report.
  --quick           Staleness only (days-stale + seed count); skip expansion.
  --emit-prompt P   Write the agent prompt to file P (default: stdout).
  --today D         Override today's date (YYYY-MM-DD) for deterministic runs.
  -h, --help        Show this help.
"#;

fn die(msg: &str) -> ! {
    eprintln!("update-driver: {msg}\n\n{USAGE}");
    exit(2);
}

// ---------------------------------------------------------------------------
// Front-matter
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct FrontMatter {
    last_updated: Option<String>,
    tracks_repo: Option<String>,
    tracks_sha: Option<String>,
    watch_files: Vec<String>,
    staleness_max_days: Option<i64>,
    title: Option<String>,
}

/// Parse the leading `---` ... `---` YAML block. Returns None when the file has
/// no front-matter (a plain reference doc that opted out of the machinery).
fn parse_front_matter(text: &str) -> Option<FrontMatter> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut fm = FrontMatter::default();
    let mut in_watch = false;
    for line in lines {
        let trimmed = line.trim_end();
        if trimmed.trim() == "---" {
            return Some(fm);
        }
        // List item under watch_files:
        if in_watch {
            let t = trimmed.trim_start();
            if let Some(item) = t.strip_prefix("- ") {
                fm.watch_files.push(strip_comment(item).trim().to_string());
                continue;
            }
            // A non-indented, non-list line ends the watch_files block.
            if !line.starts_with(char::is_whitespace) && !t.is_empty() {
                in_watch = false;
            } else if t.is_empty() {
                continue;
            } else {
                continue;
            }
        }
        if let Some((key, val)) = trimmed.split_once(':') {
            let key = key.trim();
            let val = strip_comment(val).trim().trim_matches('"').to_string();
            match key {
                "last_updated" => fm.last_updated = non_empty(val),
                "tracks_repo" => fm.tracks_repo = non_empty(val),
                "tracks_sha" => fm.tracks_sha = non_empty(val),
                "title" => fm.title = non_empty(val),
                "staleness_max_days" => fm.staleness_max_days = val.parse().ok(),
                "watch_files" => in_watch = true,
                _ => {}
            }
        }
    }
    // Reached EOF without a closing marker: not valid front-matter.
    None
}

fn strip_comment(s: &str) -> String {
    // Drop a trailing ` # comment`, but keep '#' that is inside a quoted value.
    match s.find(" #") {
        Some(i) if !s[..i].contains('"') || s[..i].matches('"').count() % 2 == 0 => {
            s[..i].to_string()
        }
        _ => s.to_string(),
    }
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// ---------------------------------------------------------------------------
// Git + filesystem helpers
// ---------------------------------------------------------------------------

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Walk up from `start` to find the workspace root that contains `repo_name`
/// as a git checkout, and return that checkout's path.
fn find_repo(start: &Path, repo_name: &str) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let candidate = dir.join(repo_name);
        if candidate.join(".git").exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}

/// Days between an ISO date (YYYY-MM-DD) and `today`, via coreutils `date`.
fn days_since(date: &str, today: &str) -> Result<i64, String> {
    let to_epoch = |d: &str| -> Result<i64, String> {
        let out = Command::new("date")
            .args(["-u", "-d", d, "+%s"])
            .output()
            .map_err(|e| format!("failed to spawn date: {e}"))?;
        if !out.status.success() {
            return Err(format!("unparseable date: {d}"));
        }
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<i64>()
            .map_err(|e| format!("bad epoch for {d}: {e}"))
    };
    Ok((to_epoch(today)? - to_epoch(date)?) / 86_400)
}

// ---------------------------------------------------------------------------
// Light-cone expansion
// ---------------------------------------------------------------------------

/// One intervening commit in the window.
#[derive(Clone)]
struct Commit {
    hash: String,
    subject: String,
    date: String,
}

fn commits_touching(repo: &Path, range: &str, paths: &[String]) -> Result<Vec<Commit>, String> {
    let mut args = vec![
        "log".to_string(),
        "--no-merges".to_string(),
        "--format=%H%x1f%ad%x1f%s".to_string(),
        "--date=short".to_string(),
        range.to_string(),
        "--".to_string(),
    ];
    args.extend(paths.iter().cloned());
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git(repo, &argrefs)?;
    let mut commits = Vec::new();
    for line in out.lines() {
        let mut f = line.splitn(3, '\u{1f}');
        if let (Some(h), Some(d), Some(s)) = (f.next(), f.next(), f.next()) {
            commits.push(Commit {
                hash: h.to_string(),
                date: d.to_string(),
                subject: s.to_string(),
            });
        }
    }
    Ok(commits)
}

/// Files touched by a set of commits, with how many of those commits touched
/// each file (co-change coupling strength).
fn co_changed(repo: &Path, commits: &[Commit]) -> Result<BTreeMap<String, usize>, String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for c in commits {
        let out = git(
            repo,
            &[
                "show",
                "--name-only",
                "--format=",
                "-m",
                "--first-parent",
                &c.hash,
            ],
        )?;
        let mut seen = BTreeSet::new();
        for p in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if seen.insert(p.to_string()) {
                *counts.entry(p.to_string()).or_default() += 1;
            }
        }
    }
    Ok(counts)
}

/// Extract `mod NAME;` declarations from Rust source text (partial parse:
/// ignores inline `mod X { ... }` blocks, which do not add a file).
fn mod_decls(text: &str) -> Vec<String> {
    let mut mods = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let line = line.strip_prefix("pub ").unwrap_or(line);
        let line = line.strip_prefix("pub(crate) ").unwrap_or(line);
        if let Some(rest) = line.strip_prefix("mod ") {
            if let Some(name) = rest.strip_suffix(';') {
                let name = name.trim();
                if is_ident(name) {
                    mods.push(name.to_string());
                }
            }
        }
    }
    mods
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.chars().next().unwrap().is_ascii_digit()
}

/// Resolve `mod NAME;` declared inside file `owner` to the sibling file path
/// (repo-relative) that it introduces, if it exists on disk at HEAD.
fn resolve_mod(repo: &Path, owner: &str, name: &str) -> Option<String> {
    let owner_path = Path::new(owner);
    let stem = owner_path.file_stem()?.to_str()?;
    let parent = owner_path.parent().unwrap_or(Path::new(""));
    // A `foo.rs` that is not a module root introduces submodules under `foo/`.
    let base = if matches!(stem, "mod" | "lib" | "main") {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    };
    for cand in [
        base.join(format!("{name}.rs")),
        base.join(name).join("mod.rs"),
    ] {
        if repo.join(&cand).is_file() {
            return Some(cand.to_string_lossy().replace('\\', "/"));
        }
    }
    None
}

/// Added `mod NAME;` lines within the window, attributed to the file (+++ b/)
/// they were added to. These are the strongest light-cone signal: a module that
/// entered the tree during the very window the doc missed.
fn added_mods_in_window(
    repo: &Path,
    range: &str,
    paths: &[String],
) -> Result<Vec<(String, String)>, String> {
    let mut args = vec![
        "log".to_string(),
        "-p".to_string(),
        "--no-merges".to_string(),
        "--format=".to_string(),
        "-U0".to_string(),
        range.to_string(),
        "--".to_string(),
    ];
    args.extend(paths.iter().cloned());
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git(repo, &argrefs)?;
    let mut cur = String::new();
    let mut found = Vec::new();
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("+++ b/") {
            cur = p.trim().to_string();
        } else if let Some(added) = line.strip_prefix('+') {
            if !added.starts_with("++") {
                for name in mod_decls(added) {
                    if !cur.is_empty() {
                        found.push((cur.clone(), name));
                    }
                }
            }
        }
    }
    Ok(found)
}

/// Expand a directory watch entry (ending in `/`) to the .rs files under it.
fn expand_watch_paths(repo: &Path, watch: &[String]) -> Vec<String> {
    let mut files = Vec::new();
    for w in watch {
        if w.ends_with('/') {
            collect_rs(&repo.join(w), repo, &mut files);
        } else {
            files.push(w.clone());
        }
    }
    files.sort();
    files.dedup();
    files
}

fn collect_rs(dir: &Path, repo: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_rs(&p, repo, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            if let Ok(rel) = p.strip_prefix(repo) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

fn crate_of(path: &str) -> Option<&str> {
    path.split('/').next()
}

fn is_source_candidate(path: &str, watch_crates: &BTreeSet<String>) -> bool {
    if !path.ends_with(".rs") {
        return false;
    }
    let Some(cr) = crate_of(path) else {
        return false;
    };
    if !watch_crates.contains(cr) {
        return false;
    }
    let bad = ["/tests/", "/test/", "/benches/", "/examples/"];
    if bad.iter().any(|b| path.contains(b)) {
        return false;
    }
    !path.ends_with("build.rs")
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

struct Args {
    doc: PathBuf,
    json: bool,
    quick: bool,
    emit_prompt: Option<PathBuf>,
    today: Option<String>,
}

fn parse_args() -> Args {
    let mut doc = None;
    let mut json = false;
    let mut quick = false;
    let mut emit_prompt = None;
    let mut today = None;
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                exit(0);
            }
            "--json" => json = true,
            "--quick" => quick = true,
            "--emit-prompt" => {
                emit_prompt = Some(PathBuf::from(
                    it.next()
                        .unwrap_or_else(|| die("--emit-prompt needs a path")),
                ))
            }
            "--today" => today = Some(it.next().unwrap_or_else(|| die("--today needs a date"))),
            other if other.starts_with("--") => die(&format!("unknown flag {other}")),
            other => {
                if doc.is_some() {
                    die("only one doc argument is allowed");
                }
                doc = Some(PathBuf::from(other));
            }
        }
    }
    Args {
        doc: doc.unwrap_or_else(|| die("a doc path is required")),
        json,
        quick,
        emit_prompt,
        today,
    }
}

fn today_str(explicit: &Option<String>) -> String {
    if let Some(t) = explicit {
        return t.clone();
    }
    let out = Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .expect("date");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn main() {
    let args = parse_args();
    let text = fs::read_to_string(&args.doc)
        .unwrap_or_else(|e| die(&format!("cannot read {}: {e}", args.doc.display())));

    let Some(fm) = parse_front_matter(&text) else {
        if args.json {
            println!(
                "{}",
                json!({"doc": args.doc.to_string_lossy(), "has_front_matter": false})
            );
        } else {
            eprintln!(
                "{}: no staleness front-matter (plain reference doc) — skipped",
                args.doc.display()
            );
        }
        exit(0);
    };

    let repo_name = fm
        .tracks_repo
        .clone()
        .unwrap_or_else(|| die("front-matter missing tracks_repo"));
    let old_sha = fm
        .tracks_sha
        .clone()
        .unwrap_or_else(|| die("front-matter missing tracks_sha"));
    let last_updated = fm
        .last_updated
        .clone()
        .unwrap_or_else(|| die("front-matter missing last_updated"));
    if fm.watch_files.is_empty() {
        die("front-matter has an empty watch_files list");
    }

    let doc_dir = args.doc.canonicalize().unwrap_or_else(|_| args.doc.clone());
    let doc_dir = doc_dir.parent().unwrap_or(Path::new(".")).to_path_buf();
    let repo = find_repo(&doc_dir, &repo_name).unwrap_or_else(|| {
        die(&format!(
            "could not locate '{repo_name}' checkout above {}",
            doc_dir.display()
        ))
    });

    let head = git(&repo, &["rev-parse", "HEAD"])
        .unwrap_or_else(|e| die(&e))
        .trim()
        .to_string();
    let today = today_str(&args.today);
    let days_stale = days_since(&last_updated, &today).unwrap_or_else(|e| die(&e));

    // The doc's tracks_sha must still exist as a commit object; a rebase, GC, or
    // typo can leave it unreachable. Report that cleanly instead of letting the
    // range query fail fatally.
    let sha_known = git(&repo, &["cat-file", "-e", &format!("{old_sha}^{{commit}}")]).is_ok();
    if !sha_known {
        let payload = json!({
            "doc": args.doc.to_string_lossy(),
            "has_front_matter": true,
            "title": fm.title,
            "tracks_repo": repo_name,
            "tracks_sha": old_sha,
            "head_sha": head,
            "last_updated": last_updated,
            "days_stale": days_stale,
            "staleness_max_days": fm.staleness_max_days.unwrap_or(30),
            "seed_commits": 0,
            "old_sha_is_ancestor": false,
            "verdict": "UNKNOWN-SHA",
        });
        if args.json {
            println!("{payload}");
        } else {
            eprintln!(
                "UNKNOWN-SHA: {} — tracks_sha {old_sha} is not a commit in {repo_name}; \
                 fix the front-matter or re-run after fetching.",
                args.doc.display()
            );
        }
        exit(0);
    }

    let ancestor = git(&repo, &["merge-base", "--is-ancestor", &old_sha, "HEAD"]).is_ok();
    let range = format!("{old_sha}..HEAD");

    let seed = commits_touching(&repo, &range, &fm.watch_files).unwrap_or_else(|e| die(&e));

    let max_days = fm.staleness_max_days.unwrap_or(30);
    let stale_by_age = days_stale > max_days;
    let stale_by_drift = !seed.is_empty();
    let verdict = if !ancestor {
        "DIVERGED" // old SHA not an ancestor of HEAD (history rewrite / wrong repo)
    } else if stale_by_drift {
        "STALE-DRIFT"
    } else if stale_by_age {
        "STALE-AGE"
    } else {
        "FRESH"
    };

    if args.quick {
        if args.json {
            println!(
                "{}",
                json!({
                    "doc": args.doc.to_string_lossy(),
                    "has_front_matter": true,
                    "title": fm.title,
                    "tracks_repo": repo_name,
                    "tracks_sha": old_sha,
                    "head_sha": head,
                    "last_updated": last_updated,
                    "days_stale": days_stale,
                    "staleness_max_days": max_days,
                    "seed_commits": seed.len(),
                    "old_sha_is_ancestor": ancestor,
                    "verdict": verdict,
                })
            );
        } else {
            println!(
                "{verdict:<12} {days_stale:>4}d  seed={:<3}  {}",
                seed.len(),
                args.doc.display()
            );
        }
        exit(0);
    }

    // ---- Light-cone expansion ------------------------------------------------
    let watch_crates: BTreeSet<String> = fm
        .watch_files
        .iter()
        .filter_map(|w| crate_of(w).map(str::to_string))
        .collect();
    let watch_expanded = expand_watch_paths(&repo, &fm.watch_files);
    let watch_set: BTreeSet<String> = watch_expanded.iter().cloned().collect();

    // (a) co-change coupling from the seed commits.
    let co = co_changed(&repo, &seed).unwrap_or_else(|e| die(&e));

    // (b) mod-declaration expansion: current mods in watched files + mods ADDED
    //     within the window (strongest signal).
    let mut reasons: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for f in &watch_expanded {
        if let Ok(src) = fs::read_to_string(repo.join(f)) {
            for name in mod_decls(&src) {
                if let Some(child) = resolve_mod(&repo, f, &name) {
                    if !watch_set.contains(&child) {
                        reasons
                            .entry(child)
                            .or_default()
                            .insert("current-mod".into());
                    }
                }
            }
        }
    }
    for (owner, name) in added_mods_in_window(&repo, &range, &fm.watch_files).unwrap_or_default() {
        if let Some(child) = resolve_mod(&repo, &owner, &name) {
            if !watch_set.contains(&child) {
                reasons.entry(child).or_default().insert("new-mod".into());
            }
        }
    }
    for (path, n) in &co {
        if !watch_set.contains(path) {
            reasons
                .entry(path.clone())
                .or_default()
                .insert(format!("co-change x{n}"));
        }
    }

    // Winnow the superset to same-crate source files.
    let mut expanded: Vec<(String, Vec<String>)> = reasons
        .into_iter()
        .filter(|(p, _)| is_source_candidate(p, &watch_crates))
        .map(|(p, r)| (p, r.into_iter().collect::<Vec<_>>()))
        .collect();
    // Rank: new-mod first, then higher co-change, then path.
    expanded.sort_by(|a, b| {
        let score = |r: &[String]| -> i64 {
            let mut s = 0;
            for tag in r {
                if tag == "new-mod" {
                    s += 1000;
                } else if tag == "current-mod" {
                    s += 100;
                } else if let Some(n) = tag.strip_prefix("co-change x") {
                    s += n.parse::<i64>().unwrap_or(0);
                }
            }
            s
        };
        score(&b.1).cmp(&score(&a.1)).then(a.0.cmp(&b.0))
    });

    // Proposed additions = winnowed expansion not already watched.
    let proposed_watch: Vec<String> = expanded.iter().map(|(p, _)| p.clone()).collect();

    // Relevant commits = the light cone: commits touching watch ∪ winnowed set.
    let mut lightcone_paths = fm.watch_files.clone();
    lightcone_paths.extend(proposed_watch.iter().cloned());
    lightcone_paths.sort();
    lightcone_paths.dedup();
    let relevant = commits_touching(&repo, &range, &lightcone_paths).unwrap_or_else(|e| die(&e));

    if args.json {
        let expanded_json: Vec<Value> = expanded
            .iter()
            .map(|(p, r)| json!({"path": p, "reasons": r}))
            .collect();
        let commits_json: Vec<Value> = relevant
            .iter()
            .map(|c| json!({"hash": c.hash, "date": c.date, "subject": c.subject}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "doc": args.doc.to_string_lossy(),
                "has_front_matter": true,
                "title": fm.title,
                "tracks_repo": repo_name,
                "tracks_sha": old_sha,
                "head_sha": head,
                "old_sha_is_ancestor": ancestor,
                "last_updated": last_updated,
                "days_stale": days_stale,
                "verdict": verdict,
                "seed_commit_count": seed.len(),
                "expanded_files": expanded_json,
                "proposed_watch_additions": proposed_watch,
                "relevant_commits": commits_json,
            }))
            .unwrap()
        );
        return;
    }

    // ---- Human report --------------------------------------------------------
    println!("# Update analysis: {}", args.doc.display());
    println!();
    println!("- repo:          {repo_name} ({})", repo.display());
    println!("- current as of: {old_sha}  ({last_updated}, {days_stale}d ago)");
    println!("- HEAD:          {head}");
    println!("- verdict:       {verdict}");
    if !ancestor {
        println!("  ! tracks_sha is NOT an ancestor of HEAD — history rewrite or wrong repo; results may be partial.");
    }
    println!("- seed commits (touch watch-files): {}", seed.len());
    println!(
        "- light-cone commits (watch ∪ expanded): {}",
        relevant.len()
    );
    println!();
    println!(
        "## Proposed watch-file additions ({})",
        proposed_watch.len()
    );
    if expanded.is_empty() {
        println!("(none — watch-list appears complete)");
    } else {
        for (p, r) in &expanded {
            println!("  {p}   [{}]", r.join(", "));
        }
    }
    println!();
    println!("## Relevant commits (most recent first)");
    for c in &relevant {
        println!(
            "  {} {}  {}",
            &c.hash[..c.hash.len().min(12)],
            c.date,
            c.subject
        );
    }
    println!();

    let prompt = craft_prompt(
        &args.doc, &fm, &repo_name, &old_sha, &head, days_stale, &today, &relevant, &expanded,
    );
    if let Some(path) = &args.emit_prompt {
        fs::write(path, &prompt).unwrap_or_else(|e| die(&format!("cannot write prompt: {e}")));
        println!("Agent prompt written to {}", path.display());
    } else {
        println!("## Agent update prompt");
        println!("----------------------------------------------------------------");
        print!("{prompt}");
        println!("----------------------------------------------------------------");
    }
}

#[allow(clippy::too_many_arguments)]
fn craft_prompt(
    doc: &Path,
    fm: &FrontMatter,
    repo: &str,
    old_sha: &str,
    head: &str,
    days_stale: i64,
    today: &str,
    commits: &[Commit],
    expanded: &[(String, Vec<String>)],
) -> String {
    let subject = fm
        .title
        .clone()
        .unwrap_or_else(|| doc.display().to_string());
    let mut s = String::new();
    s.push_str("You are updating a durable reference document for a NEW reader.\n\n");
    s.push_str(&format!("DOCUMENT: {}\n", doc.display()));
    s.push_str(&format!("SUBJECT:  {subject}\n"));
    s.push_str(&format!(
        "It was last made current as of {repo} commit {old_sha} ({}), now {days_stale} days old.\n",
        fm.last_updated.clone().unwrap_or_default()
    ));
    s.push_str(&format!(
        "Bring it up to date as of {repo} HEAD {head}.\n\n"
    ));
    s.push_str("Do this:\n\n");
    s.push_str("1. Read the whole document first.\n\n");
    s.push_str("2. Study these intervening commits (most recent first). Each may have changed\n   the design the doc describes; read the diff where the subject is unclear:\n");
    for c in commits {
        s.push_str(&format!(
            "     {} {}  {}\n",
            &c.hash[..c.hash.len().min(12)],
            c.date,
            c.subject
        ));
    }
    if commits.is_empty() {
        s.push_str(
            "     (none touched the watched code; this is an age refresh — re-verify links.)\n",
        );
    }
    s.push('\n');
    s.push_str("3. These source files entered or co-moved within the doc's light cone since the\n   last update. Verify which GENUINELY affect the subject and fold them in;\n   repoint any code links/permalinks to the new SHA:\n");
    if expanded.is_empty() {
        s.push_str("     (none — the existing watch-list looks complete.)\n");
    } else {
        for (p, r) in expanded {
            s.push_str(&format!("     {p}   [{}]\n", r.join(", ")));
        }
    }
    s.push('\n');
    s.push_str("4. Rewrite affected sections so a new reader understands the CURRENT design.\n   No jargon, no notes-to-self, no changelog-in-prose. Describe how it works\n   now; put dated changes in the History section only, preserving verified\n   history and marking interpretation as such.\n\n");
    s.push_str("5. WINNOW with judgment: add a file to watch_files only if the doc's content\n   actually depends on it. Drop false positives from the candidate list above\n   (this is the intelligent narrowing of the mechanical superset).\n\n");
    s.push_str("6. Update the YAML front-matter in place:\n");
    s.push_str(&format!("     last_updated: {today}\n"));
    s.push_str(&format!("     tracks_sha:   {head}\n"));
    s.push_str("     watch_files:  the winnowed set (existing + confirmed additions)\n\n");
    s.push_str("Keep the document's existing section structure and its readable tone.\n");
    s
}
