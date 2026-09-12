#!/usr/bin/env rust-script
//! Copyright (c) Meta Platforms, Inc. and affiliates.
//! All rights reserved.
//!
//! Anchored, one-hop test selection for the Hermit CI DAG.
//!
//! This is the owner's test-selection policy, made executable:
//!
//!   1. ANCHOR on a previous FULL GREEN result.
//!   2. DIFF the paths changed since that anchor.
//!   3. MAP changed paths -> the DAG STEP(S) they hit.
//!   4. RUN ONLY those steps through validate's constructed-plan selector.
//!
//! Soundness rests entirely on the anchor: everything untouched since a full
//! green was already proven green AT that anchor, so it need not run again.
//!
//! HARD CONSTRAINTS (enforced here, not by convention):
//!   * ONE HOP ONLY. The anchor MUST be a FULL green (`selection_mode == full`),
//!     never another incremental/selective green. We take the anchor from
//!     `ci-hub newest-green`, which already returns only clean full greens
//!     at/above the merge-gate floor, and we re-assert `selection_mode == full`
//!     before trusting it. No full-green anchor => conservative FULL run.
//!   * A NARROWED RUN STATES its anchor SHA, the changed paths, the DAG steps
//!     SELECTED, and the DAG steps SKIPPED. A selection that silently
//!     under-runs is a fake green; enumerating the skipped set makes any
//!     under-run visible.
//!   * CONSERVATIVE MAPPING. Any path not in the map => RUN EVERYTHING. A
//!     missing entry is a full run, never a skip. Over-running only costs time;
//!     under-running produces a false green.
//!
//! THE MAP: changed path -> DAG step(s), keyed on the node names that already
//! exist in `ci/dag/{portable,privileged}.json` (no parallel taxonomy). The
//! load-bearing distinction, matched MOST-SPECIFIC-FIRST:
//!
//!   * `reverie/` CORE (reverie, reverie-ptrace, reverie-syscalls, ...) lights
//!     up ALL of Hermit that depends on it => FULL. No narrowing; we say so.
//!   * `reverie/reverie-kvm/` -> only the KVM (privileged-lane) steps.
//!     `reverie/reverie-dbi/`, `reverie/reverie-liteinst/`,
//!     `reverie/reverie-e9patch/` likewise narrow to one backend.
//!   * Hermit backend dirs (`detcore-dbi/`, `detcore-sabre/`) -> that backend.
//!
//! A naive `reverie/` prefix match would collapse core-vs-backend and destroy
//! the whole benefit, so backend-crate rules are matched BEFORE the general
//! `reverie/**` -> FULL rule (first-match-wins, most-specific-first).
//!
//! Usage:
//!   scripts/anchored-test-selection.rs                      # anchor auto, diff HEAD
//!   scripts/anchored-test-selection.rs --files a.rs b.rs    # explicit path list
//!   git diff --name-only A..B | scripts/anchored-test-selection.rs --files -
//!   scripts/anchored-test-selection.rs --anchor <sha> --files ...   # fixed anchor
//!   scripts/anchored-test-selection.rs --format json
//!   scripts/anchored-test-selection.rs --files detcore-dbi/src/lib.rs --run
//!   scripts/anchored-test-selection.rs --self-test
//!
//! ```cargo
//! [dependencies]
//! serde_json = "1"
//! ```

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

// ---------------------------------------------------------------------------
// Glob matching (dependency-free, git-pathspec-like). Same semantics as
// hermit/ci/select-tests.rs so the two tools agree on what a pattern means.
// A pattern with no `/` (e.g. `Cargo.toml`) matches only that exact path, so a
// root-level `Cargo.toml` is distinct from `reverie/reverie-kvm/Cargo.toml`.
// ---------------------------------------------------------------------------

fn glob_match(pattern: &str, text: &str) -> bool {
    glob_inner(pattern.as_bytes(), text.as_bytes())
}

fn glob_inner(p: &[u8], t: &[u8]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    if p[0] == b'*' {
        if p.len() >= 2 && p[1] == b'*' {
            let mut rest = &p[2..];
            if rest.first() == Some(&b'/') {
                rest = &rest[1..];
            }
            let mut i = 0;
            loop {
                if glob_inner(rest, &t[i..]) {
                    return true;
                }
                if i >= t.len() {
                    return false;
                }
                i += 1;
            }
        } else {
            let mut i = 0;
            loop {
                if glob_inner(&p[1..], &t[i..]) {
                    return true;
                }
                if i >= t.len() || t[i] == b'/' {
                    return false;
                }
                i += 1;
            }
        }
    } else if p[0] == b'?' {
        !t.is_empty() && t[0] != b'/' && glob_inner(&p[1..], &t[1..])
    } else {
        !t.is_empty() && p[0] == t[0] && glob_inner(&p[1..], &t[1..])
    }
}

fn matches_any(globs: &[&str], file: &str) -> bool {
    globs.iter().any(|g| glob_match(g, file))
}

// ---------------------------------------------------------------------------
// DAG universe: read the REAL node names + deps + duration hints from the CI
// graph. Node identity is lane-qualified ("portable:test.dbt_parity") because
// several job names (e2e.metadata, e2e.manifest_backend_parity_c, ...) exist in
// BOTH lanes and mean different things.
// ---------------------------------------------------------------------------

#[allow(dead_code)] // id/lane retained for readability; derivable from the map key
struct Node {
    id: String, // "lane:group.job"
    lane: String,
    deps: Vec<String>, // lane-qualified
    est_s: u64,
}

struct Dag {
    nodes: BTreeMap<String, Node>,
    lane_order: BTreeMap<String, Vec<String>>,
}

impl Dag {
    fn load(hermit: &Path) -> Dag {
        let mut nodes = BTreeMap::new();
        let mut lane_order = BTreeMap::new();
        for lane in ["portable", "privileged"] {
            let path = hermit.join(format!("ci/dag/{lane}.json"));
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| fail(&format!("cannot read {}: {e}", path.display())));
            let v: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|e| fail(&format!("invalid JSON in {}: {e}", path.display())));
            for s in v["steps"].as_array().unwrap_or(&vec![]) {
                let job = format!(
                    "{}.{}",
                    s["group"].as_str().unwrap_or(""),
                    s["job"].as_str().unwrap_or("")
                );
                let id = format!("{lane}:{job}");
                lane_order
                    .entry(lane.to_string())
                    .or_insert_with(Vec::new)
                    .push(job.clone());
                let deps = s["deps"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(|d| format!("{lane}:{d}")))
                            .collect()
                    })
                    .unwrap_or_default();
                let est_s = s["hint"]["est_duration_s"].as_u64().unwrap_or(0);
                nodes.insert(
                    id.clone(),
                    Node {
                        id,
                        lane: lane.to_string(),
                        deps,
                        est_s,
                    },
                );
            }
        }
        if nodes.is_empty() {
            fail("no DAG nodes loaded (is --hermit pointing at a Hermit checkout?)");
        }
        Dag { nodes, lane_order }
    }

    fn all_ids(&self) -> BTreeSet<String> {
        self.nodes.keys().cloned().collect()
    }

    /// Add every transitive dep predecessor of the given nodes (so the subset
    /// DAG is runnable: a selected test pulls in the builds it needs).
    fn close_over_deps(&self, seed: &BTreeSet<String>) -> BTreeSet<String> {
        let mut out = seed.clone();
        let mut stack: Vec<String> = seed.iter().cloned().collect();
        while let Some(n) = stack.pop() {
            if let Some(node) = self.nodes.get(&n) {
                for d in &node.deps {
                    if out.insert(d.clone()) {
                        stack.push(d.clone());
                    }
                }
            }
        }
        out
    }

    fn est_of(&self, ids: &BTreeSet<String>) -> u64 {
        ids.iter()
            .filter_map(|i| self.nodes.get(i))
            .map(|n| n.est_s)
            .sum()
    }

    /// The "manifest gate": the guest build + every e2e manifest execution node,
    /// across both lanes. This is the dominant wall-time cost the owner tracks.
    fn manifest_gate(&self, ids: &BTreeSet<String>) -> BTreeSet<String> {
        ids.iter()
            .filter(|id| {
                let job = id.split(':').nth(1).unwrap_or("");
                job.starts_with("e2e.manifest_") || job == "build.manifest_guests"
            })
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The path -> DAG-step map. An ordered, most-specific-first rule list;
// FIRST MATCH WINS per changed path. Step sets reference REAL lane-qualified
// node names; `load()` asserts every one exists in the live DAG (stale-map
// guard) so the map can never silently select a node that no longer runs.
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Action {
    Full,               // this path forces the whole suite
    Irrelevant,         // provably inert (docs); contributes no steps
    Steps(Vec<String>), // this path narrows to these lane-qualified steps
}

struct Rule {
    globs: Vec<&'static str>,
    action: Action,
    why: &'static str,
}

// Backend step sets, keyed on the exact nodes in ci/dag/{portable,privileged}.json.
// deps (builds) are added later by dependency closure; here we name the leaf work.
fn kvm_steps() -> Vec<String> {
    // KVM has no node literally named "kvm": KVM IS the privileged lane
    // (resource_caps {kvm:1}; the parity + applications cells carry resources
    // {kvm:1}). These are "the KVM steps".
    vec![
        "privileged:e2e.manifest_backend_parity_c".into(),
        "privileged:e2e.manifest_applications".into(),
    ]
}
fn dbi_steps() -> Vec<String> {
    vec![
        "portable:test.dbt_parity".into(),
        "portable:e2e.manifest_backend_parity_c".into(),
    ]
}
fn sabre_steps() -> Vec<String> {
    vec![
        "portable:test.sabre_examples".into(),
        "portable:e2e.manifest_backend_parity_c".into(),
    ]
}
fn liteinst_steps() -> Vec<String> {
    vec![
        "portable:test.liteinst_strict".into(),
        "portable:build.liteinst_runtime_release".into(),
        "portable:e2e.manifest_backend_parity_c".into(),
    ]
}
fn e9patch_steps() -> Vec<String> {
    // e9patch is binary-rewriting PREPROCESSING used with the ptrace backend
    // (hermit/AGENTS.md), not its own backend, so it has no dedicated node. It
    // is exercised in backend parity; narrow to those cells in both lanes.
    vec![
        "portable:e2e.manifest_backend_parity_c".into(),
        "privileged:e2e.manifest_backend_parity_c".into(),
    ]
}

// Always-on cheap safety gates added to any selective (non-full) run.
const PREFLIGHT: &[&str] = &[
    "portable:lint.rustfmt",
    "portable:check.backend_abstraction",
    "portable:check.portability_paths",
];

fn rules() -> Vec<Rule> {
    // ORDER IS SIGNIFICANT — first match wins, most-specific-first.
    vec![
        // (1) Build config / CI machinery => FULL (a change here can invalidate
        //     any node's result). Root Cargo.toml is exact (no `/`), so a
        //     backend crate's own Cargo.toml does NOT match here.
        Rule {
            globs: vec![
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain.toml",
                ".cargo/**",
                "ci/**",
                "scripts/validate.rs",
                "scripts/lib/**",
                ".github/workflows/ci-portable.yml",
                ".github/workflows/ci-privileged.yml",
                ".github/workflows/merge-gate.yml",
            ],
            action: Action::Full,
            why: "build/CI machinery — can invalidate any node",
        },
        // (2) Provably inert => no steps. If EVERY changed path is inert the
        //     whole run is a skip; otherwise these paths simply add nothing.
        Rule {
            globs: vec![
                "**/*.md",
                "docs/**",
                "ai_docs/**",
                "experiments/**",
                "LICENSE",
                "CODEOWNERS",
                ".gitignore",
                "**/.gitignore",
                ".gitattributes",
                ".editorconfig",
                "**/*.png",
                "**/*.svg",
                "**/*.jpg",
            ],
            action: Action::Irrelevant,
            why: "docs/notes — CI-irrelevant",
        },
        // (3) BACKEND-SPECIFIC crates/dirs => that one backend. These MUST come
        //     before the general `reverie/**` rule below: this is the asymmetry.
        Rule {
            globs: vec!["reverie/reverie-kvm/**"],
            action: Action::Steps(kvm_steps()),
            why: "reverie-kvm backend crate -> KVM (privileged) steps only",
        },
        Rule {
            globs: vec!["reverie/reverie-dbi/**", "detcore-dbi/**"],
            action: Action::Steps(dbi_steps()),
            why: "DBI backend crate/dir -> DBI steps only",
        },
        Rule {
            globs: vec!["reverie/reverie-liteinst/**"],
            action: Action::Steps(liteinst_steps()),
            why: "reverie-liteinst backend crate -> LiteInst steps only",
        },
        Rule {
            globs: vec!["reverie/reverie-e9patch/**"],
            action: Action::Steps(e9patch_steps()),
            why: "reverie-e9patch preprocessing -> backend-parity steps only",
        },
        Rule {
            globs: vec!["detcore-sabre/**"],
            action: Action::Steps(sabre_steps()),
            why: "detcore-sabre backend dir -> SaBRe steps only",
        },
        // (4) REVERIE CORE (everything else under reverie/) => FULL. Core Reverie
        //     is a dependency of all of Hermit; a change here touches everything.
        Rule {
            globs: vec!["reverie/**"],
            action: Action::Full,
            why: "reverie CORE — depended on by all of Hermit",
        },
    ]
    // NO trailing catch-all: an unmatched path falls through to conservative
    // FULL in classify(), so a NEW/unmapped area can never be silently skipped.
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Clone, Copy)]
enum Decision {
    Skip,
    Selective,
    Full,
}

fn decision_str(d: Decision) -> &'static str {
    match d {
        Decision::Skip => "skip",
        Decision::Selective => "selective",
        Decision::Full => "full",
    }
}

struct Selection {
    decision: Decision,
    selected: BTreeSet<String>, // lane-qualified, dependency-closed
    reasons: Vec<String>,
}

/// Classify ONE path against the ordered rule list (first match wins). A path
/// that matches nothing is FULL (conservative).
fn classify(path: &str) -> (Action, &'static str) {
    for r in rules() {
        if matches_any(&r.globs, path) {
            return (r.action.clone(), r.why);
        }
    }
    (Action::Full, "unmapped path — conservative full")
}

fn select(dag: &Dag, files: &[String]) -> Selection {
    let mut reasons = Vec::new();

    if files.is_empty() {
        return Selection {
            decision: Decision::Full,
            selected: dag.all_ids(),
            reasons: vec!["no changed-path information -> full suite".into()],
        };
    }

    let mut steps: BTreeSet<String> = BTreeSet::new();
    let mut any_full = false;
    let mut any_narrowing = false; // at least one path mapped to concrete steps
    let mut all_inert = true;
    let mut full_reasons: Vec<String> = Vec::new();

    for f in files {
        let (action, why) = classify(f);
        match action {
            Action::Full => {
                any_full = true;
                all_inert = false;
                full_reasons.push(format!("{f} -> FULL ({why})"));
            }
            Action::Irrelevant => {
                reasons.push(format!("{f} -> inert ({why})"));
            }
            Action::Steps(s) => {
                all_inert = false;
                any_narrowing = true;
                for id in &s {
                    steps.insert(id.clone());
                }
                reasons.push(format!("{f} -> {} ({why})", s.join(", ")));
            }
        }
    }

    if any_full {
        for r in full_reasons {
            reasons.push(r);
        }
        reasons.push("at least one path forces the whole suite".into());
        return Selection {
            decision: Decision::Full,
            selected: dag.all_ids(),
            reasons,
        };
    }
    if !any_narrowing && all_inert {
        reasons.push("every changed path is CI-irrelevant -> nothing to run".into());
        return Selection {
            decision: Decision::Skip,
            selected: BTreeSet::new(),
            reasons,
        };
    }
    if steps.is_empty() {
        // Defensive: not full, not all-inert, yet no steps. Fail safe to full.
        reasons.push("no steps mapped but paths not proven inert -> full suite".into());
        return Selection {
            decision: Decision::Full,
            selected: dag.all_ids(),
            reasons,
        };
    }

    // Selective: add always-on preflight, verify every step exists, close deps.
    for pf in PREFLIGHT {
        if dag.nodes.contains_key(*pf) {
            steps.insert((*pf).to_string());
        }
    }
    let missing: Vec<String> = steps
        .iter()
        .filter(|s| !dag.nodes.contains_key(*s))
        .cloned()
        .collect();
    if !missing.is_empty() {
        reasons.push(format!(
            "map referenced {} node(s) absent from the live DAG ({}) -> full suite (stale map)",
            missing.len(),
            missing.join(", ")
        ));
        return Selection {
            decision: Decision::Full,
            selected: dag.all_ids(),
            reasons,
        };
    }
    let closed = dag.close_over_deps(&steps);
    reasons.push(format!(
        "{} leaf step(s) + preflight + build deps -> {} of {} DAG steps",
        steps.len(),
        closed.len(),
        dag.nodes.len()
    ));
    Selection {
        decision: Decision::Selective,
        selected: closed,
        reasons,
    }
}

// ---------------------------------------------------------------------------
// Anchor resolution (ONE HOP: full green only)
// ---------------------------------------------------------------------------

struct Anchor {
    sha: String,
    source: String,
}

/// Resolve the newest FULL-green anchor via `ci-hub newest-green --json`.
/// Returns None (=> conservative full) if there is no such anchor, or if the
/// returned record is not a full-coverage full-selection green. ONE HOP is
/// enforced here: we refuse anything whose `selection_mode` is not `full`.
fn resolve_anchor(ci_hub: &Path, branch: &str, no_fetch: bool) -> Option<Anchor> {
    let mut args = vec![
        "newest-green".to_string(),
        "--branch".to_string(),
        branch.to_string(),
        "--json".to_string(),
    ];
    if no_fetch {
        args.push("--no-fetch".to_string());
    }
    let out = Command::new(ci_hub).args(&args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    // newest-green wraps its JSON in `# COST` comment lines; take the JSON body.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout.find('{')?;
    let json_end = stdout.rfind('}')?;
    let body = &stdout[json_start..=json_end];
    let v: Value = serde_json::from_str(body).ok()?;
    let green = &v["report"]["green"];
    let sha = green["sha"].as_str()?.to_string();
    // Belt-and-suspenders one-hop assertion: newest-green already restricts to
    // clean full greens, but we re-check the carried conditions rather than
    // trust the tool's name.
    let mode = green["selection_mode"].as_str().unwrap_or("");
    let coverage = green["coverage"].as_str().unwrap_or("");
    let result = green["result"].as_str().unwrap_or("");
    if mode != "full" || coverage != "full" || result != "pass" {
        return None;
    }
    Some(Anchor {
        sha,
        source: format!("ci-hub newest-green --branch {branch}"),
    })
}

/// Pure interpretation of `ci-hub validate-status --sha <sha> --json` output —
/// the ONE canonical receipt verifier for an explicit anchor. It DEREFERENCES
/// the ledger record behind the SHA and returns the resolved full SHA ONLY when
/// that SHA carries a clean, commit-anchored, FULL-profile, FULL-selection,
/// passing receipt (verdict == VALIDATED). Otherwise it returns Err naming
/// exactly what was missing. Kept pure (string in, Result out) so the self-test
/// can plant the three receipt shapes without shelling out.
///
/// `verdict == VALIDATED` is STRICTLY STRONGER than the "result=pass,
/// profile=full, nonzero tests" checklist: the ci-hub verifier already requires
/// a clean tree, commit anchoring, nonzero executed tests, and satisfied
/// coverage before it will say VALIDATED (a 2-check `portable-strict-compat-only`
/// record disqualifies and yields NOT-VALIDATED). We additionally re-assert the
/// carried result/profile/selection fields — carry the condition with the value
/// — so a future loosening of VALIDATED cannot silently widen what this gate
/// accepts. A raw check count is deliberately NOT gated on: validate-status
/// exposes the versioned gate contract through VALIDATED, which already
/// subsumes the exact expected sequence and count.
fn interpret_validate_status(sha: &str, json: &str) -> Result<String, String> {
    // Tolerate `# COST`/log noise around the JSON body (newest-green wraps its
    // JSON; validate-status currently does not, but be robust to either).
    let body = match (json.find('{'), json.rfind('}')) {
        (Some(a), Some(b)) if b >= a => &json[a..=b],
        _ => {
            return Err(format!(
                "no JSON object in validate-status output for {sha}"
            ))
        }
    };
    let v: Value = serde_json::from_str(body)
        .map_err(|e| format!("unparseable validate-status JSON for {sha}: {e}"))?;
    let verdict = v["verdict"].as_str().unwrap_or("<absent>");
    // validate-status resolves a prefix to the full 40-hex in `sha`; prefer it.
    let resolved = v["sha"].as_str().unwrap_or(sha).to_string();
    if verdict != "VALIDATED" {
        let q = v["qualifying_count"].as_i64().unwrap_or(-1);
        let d = v["disqualified_count"].as_i64().unwrap_or(-1);
        return Err(format!(
            "verdict={verdict} (qualifying={q}, disqualified={d}): NO full-green receipt \
             dereferences this SHA — the DAG steps a narrowed run would SKIP were proven \
             green NOWHERE"
        ));
    }
    // Defense in depth: re-assert the conditions the accepted value must carry.
    let rec = &v["newest_qualifying"];
    let result = rec["result"].as_str().unwrap_or("<absent>");
    let profile = rec["profile"].as_str().unwrap_or("<absent>");
    let mode = rec["selection_mode"].as_str().unwrap_or("<absent>");
    let mut missing = Vec::new();
    if result != "pass" {
        missing.push(format!("result={result} (want pass)"));
    }
    if profile != "full" {
        missing.push(format!("profile={profile} (want full)"));
    }
    if mode != "full" {
        missing.push(format!("selection_mode={mode} (want full)"));
    }
    if !missing.is_empty() {
        return Err(format!(
            "verdict=VALIDATED but carried conditions incomplete: {} — refusing to trust it",
            missing.join(", ")
        ));
    }
    Ok(resolved)
}

/// Dereference an explicit `--anchor <sha>` through the canonical receipt
/// verifier. An override is NEVER trusted on the bare SHA: an unverified anchor
/// means the steps a selection would skip were proven green NOWHERE, which
/// manufactures a fake green. Shells out to `ci-hub validate-status` (stdout is
/// emitted with `--json` even on the NOT-VALIDATED exit-4 path, so we read it
/// regardless of exit status) and REFUSES LOUDLY (exit 2) naming what was
/// missing. This is the single writer/reader of the anchor authority — the
/// script never re-parses the ledger itself (one verifier per authority).
fn verify_anchor_override(ci_hub: &Path, sha: &str) -> Anchor {
    let out = Command::new(ci_hub)
        .args(["validate-status", "--sha", sha, "--json"])
        .output()
        .unwrap_or_else(|e| {
            fail(&format!(
                "--anchor {sha}: cannot run {} validate-status: {e}",
                ci_hub.display()
            ))
        });
    let stdout = String::from_utf8_lossy(&out.stdout);
    match interpret_validate_status(sha, &stdout) {
        Ok(resolved) => Anchor {
            sha: resolved,
            source: format!(
                "--anchor {sha} (dereferenced full-green receipt via ci-hub validate-status)"
            ),
        },
        Err(why) => fail(&format!(
            "--anchor {sha} REFUSED: {why}. An unverified anchor override manufactures a fake \
             green — one-hop narrowing is sound ONLY against a dereferenced full-green receipt. \
             Re-run WITHOUT --anchor to auto-resolve the newest full green, or pass a SHA that \
             carries one."
        )),
    }
}

// ---------------------------------------------------------------------------
// Changed-path discovery
// ---------------------------------------------------------------------------

fn git_lines(repo: &Path, args: &[&str]) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap_or_else(|e| fail(&format!("git {:?} failed: {e}", args)));
    if !out.status.success() {
        fail(&format!(
            "git -C {} {:?} failed: {}",
            repo.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// True if `repo` is a checked-out work tree (not a bare / no-worktree repo).
/// A no-worktree repo still has a reachable object DB, so committed history
/// (anchor..HEAD) is available even when the uncommitted/untracked scan is not.
fn is_work_tree(repo: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

/// Paths changed since the anchor: committed(anchor..HEAD) U staged/unstaged U
/// untracked. This is exactly the set whose green status the anchor does NOT
/// vouch for. The committed delta comes from the object DB and is always
/// available; the staged/unstaged/untracked scan requires a work tree, so it is
/// skipped for a no-worktree repo (there is nothing uncommitted to find there).
fn changed_since(repo: &Path, anchor: &str) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for f in git_lines(repo, &["diff", "--name-only", &format!("{anchor}..HEAD")]) {
        set.insert(f);
    }
    if is_work_tree(repo) {
        for f in git_lines(repo, &["diff", "--name-only", "HEAD"]) {
            set.insert(f);
        }
        for f in git_lines(repo, &["ls-files", "--others", "--exclude-standard"]) {
            set.insert(f);
        }
    }
    set.into_iter().collect()
}

fn read_stdin_lines() -> Vec<String> {
    use std::io::Read;
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s).ok();
    s.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The reverie git rev PINNED by a hermit commit, read from that commit's
/// Cargo.lock (`source = "git+…/reverie.git?rev=<hex>#…"`). This is the reverie
/// baseline the anchor's full green actually validated against. None if the pin
/// cannot be derived (=> caller falls back to conservative FULL for reverie).
fn reverie_pin_at(hermit: &Path, anchor: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(hermit)
        .args(["show", &format!("{anchor}:Cargo.lock")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_reverie_rev(&String::from_utf8_lossy(&out.stdout))
}

/// Pure parse: pull the reverie git rev out of a Cargo.lock body. Looks for the
/// `reverie.git?rev=<hex>` marker Cargo writes into a `source = "git+…"` line and
/// returns the leading hex run (>=7 chars). Kept separate from the git shell-out
/// so it can be exercised directly by the self-test.
fn parse_reverie_rev(text: &str) -> Option<String> {
    let marker = "reverie.git?rev=";
    for line in text.lines() {
        if let Some(i) = line.find(marker) {
            let rest = &line[i + marker.len()..];
            let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if hex.len() >= 7 {
                return Some(hex);
            }
        }
    }
    None
}

/// Changed paths across BOTH product repos, expressed PARENT-relative so they
/// match the backend-crate rules. Hermit paths are used verbatim (the anchor is
/// a hermit SHA); reverie paths are prefixed `reverie/`. The reverie baseline is
/// the rev PINNED at the hermit anchor (its Cargo.lock) — the reverie tree the
/// green was actually taken against. If that pin cannot be derived, or the
/// commit is not present in the local reverie checkout, we inject a sentinel
/// `reverie/…` path that classifies to FULL: a reverie change we cannot bound is
/// never silently omitted. This is what lets a real reverie-kvm-only change be
/// SEEN here at all — a hermit-only diff can never contain a `reverie/` path.
fn changed_cross_repo(
    hermit: &Path,
    reverie: &Path,
    anchor: &str,
    notes: &mut Vec<String>,
) -> Vec<String> {
    let mut set: BTreeSet<String> = changed_since(hermit, anchor).into_iter().collect();
    match reverie_pin_at(hermit, anchor) {
        Some(rev) => {
            let present = Command::new("git")
                .arg("-C")
                .arg(reverie)
                .args(["cat-file", "-e", &format!("{rev}^{{commit}}")])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            let short = &rev[..rev.len().min(12)];
            if present {
                let rfiles = changed_since(reverie, &rev);
                for f in &rfiles {
                    set.insert(format!("reverie/{f}"));
                }
                notes.push(format!(
                    "reverie: {} path(s) changed vs pinned baseline {} (from hermit anchor Cargo.lock)",
                    rfiles.len(),
                    short
                ));
            } else {
                set.insert("reverie/__UNRESOLVED_BASELINE__/marker".into());
                notes.push(format!(
                    "reverie: baseline {short} not present in local checkout -> conservative FULL for reverie"
                ));
            }
        }
        None => {
            set.insert("reverie/__UNRESOLVED_BASELINE__/marker".into());
            notes.push(
                "reverie: could not derive pin from hermit anchor Cargo.lock -> conservative FULL for reverie"
                    .into(),
            );
        }
    }
    set.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

fn by_lane(ids: &BTreeSet<String>) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in ids {
        let (lane, job) = id.split_once(':').unwrap_or(("?", id.as_str()));
        m.entry(lane.to_string()).or_default().push(job.to_string());
    }
    m
}

fn emit_human(dag: &Dag, anchor: &Option<Anchor>, files: &[String], sel: &Selection) {
    println!("=== anchored, one-hop test selection ===");
    match anchor {
        Some(a) => println!("anchor (FULL green): {}  [{}]", a.sha, a.source),
        None => println!("anchor (FULL green): NONE FOUND -> conservative FULL run"),
    }
    println!("decision: {}", decision_str(sel.decision));
    println!();
    println!("changed paths ({}):", files.len());
    for f in files {
        println!("  {f}");
    }
    if files.is_empty() {
        println!("  (none)");
    }
    println!();
    println!("reasoning:");
    for r in &sel.reasons {
        println!("  - {r}");
    }
    println!();

    let universe = dag.all_ids();
    let skipped: BTreeSet<String> = universe.difference(&sel.selected).cloned().collect();

    println!(
        "DAG steps SELECTED ({}/{}):",
        sel.selected.len(),
        universe.len()
    );
    for (lane, jobs) in by_lane(&sel.selected) {
        println!("  [{lane}] {}", jobs.join(", "));
    }
    if sel.selected.is_empty() {
        println!("  (none)");
    }
    println!();
    println!("DAG steps SKIPPED ({}/{}):", skipped.len(), universe.len());
    for (lane, jobs) in by_lane(&skipped) {
        println!("  [{lane}] {}", jobs.join(", "));
    }
    if skipped.is_empty() {
        println!("  (none)");
    }
    println!();

    // Wall-time model (est_duration_s hints; a proxy, not a measured run).
    let total_est = dag.est_of(&universe);
    let sel_est = dag.est_of(&sel.selected);
    let gate_all = dag.manifest_gate(&universe);
    let gate_sel = dag.manifest_gate(&sel.selected);
    let gate_all_est = dag.est_of(&gate_all);
    let gate_sel_est = dag.est_of(&gate_sel);
    println!("est-model work (sum of est_duration_s hints; NOT a measured wall):");
    println!(
        "  all steps:      {total_est}s   selected: {sel_est}s   ({:.0}% of full)",
        if total_est > 0 {
            100.0 * sel_est as f64 / total_est as f64
        } else {
            0.0
        }
    );
    println!(
        "  manifest gate:  {gate_all_est}s over {} nodes  ->  selected {gate_sel_est}s over {} nodes",
        gate_all.len(),
        gate_sel.len()
    );
    if sel.decision == Decision::Selective && gate_sel.len() < gate_all.len() {
        println!(
            "  => selection PRUNES the manifest gate: {} of {} manifest nodes skipped.",
            gate_all.len() - gate_sel.len(),
            gate_all.len()
        );
    } else if sel.decision == Decision::Full {
        println!("  => FULL run: the manifest gate runs in its entirety (no narrowing).");
    } else if sel.decision == Decision::Skip {
        println!("  => SKIP: the manifest gate does not run at all.");
    }
}

fn emit_json(dag: &Dag, anchor: &Option<Anchor>, files: &[String], sel: &Selection) {
    let universe = dag.all_ids();
    let skipped: BTreeSet<String> = universe.difference(&sel.selected).cloned().collect();
    let obj = serde_json::json!({
        "anchor_sha": anchor.as_ref().map(|a| a.sha.clone()),
        "anchor_source": anchor.as_ref().map(|a| a.source.clone()),
        "decision": decision_str(sel.decision),
        "changed_paths": files,
        "selected_steps": sel.selected.iter().cloned().collect::<Vec<_>>(),
        "skipped_steps": skipped.iter().cloned().collect::<Vec<_>>(),
        "selected_count": sel.selected.len(),
        "skipped_count": skipped.len(),
        "total_count": universe.len(),
        "est_total_s": dag.est_of(&universe),
        "est_selected_s": dag.est_of(&sel.selected),
        "manifest_gate_total_nodes": dag.manifest_gate(&universe).len(),
        "manifest_gate_selected_nodes": dag.manifest_gate(&sel.selected).len(),
        "reasons": sel.reasons,
    });
    println!("{}", serde_json::to_string_pretty(&obj).unwrap());
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

fn fail(msg: &str) -> ! {
    eprintln!("anchored-test-selection: {msg}");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if parent_flag_before_passthrough(&args, "-h")
        || parent_flag_before_passthrough(&args, "--help")
    {
        print_help();
        return;
    }
    if parent_flag_before_passthrough(&args, "--self-test") {
        self_test();
        return;
    }

    let mut hermit = PathBuf::from("hermit");
    let mut ci_hub = PathBuf::from("ci-hub/ci-hub");
    let mut branch = "main".to_string();
    let mut format = "human".to_string();
    let mut explicit_files: Option<Vec<String>> = None;
    let mut anchor_override: Option<String> = None;
    let mut no_fetch = false;
    let mut reverie = PathBuf::from("reverie");
    let mut run = false;
    let mut validate: Option<PathBuf> = None;
    let mut passthrough: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--hermit" => {
                hermit = PathBuf::from(need(&args, i));
                i += 2;
            }
            "--ci-hub" => {
                ci_hub = PathBuf::from(need(&args, i));
                i += 2;
            }
            "--branch" => {
                branch = need(&args, i);
                i += 2;
            }
            "--anchor" => {
                anchor_override = Some(need(&args, i));
                i += 2;
            }
            "--no-fetch" => {
                no_fetch = true;
                i += 1;
            }
            "--format" => {
                format = need(&args, i);
                i += 2;
            }
            "--reverie" => {
                reverie = PathBuf::from(need(&args, i));
                i += 2;
            }
            "--run" => {
                run = true;
                i += 1;
            }
            "--validate" => {
                validate = Some(PathBuf::from(need(&args, i)));
                i += 2;
            }
            "--" => {
                passthrough = args[i + 1..].to_vec();
                break;
            }
            "--files" => {
                if args.get(i + 1).map(|s| s.as_str()) == Some("-") {
                    explicit_files = Some(read_stdin_lines());
                    i += 2;
                } else {
                    let mut files = Vec::new();
                    let mut j = i + 1;
                    while j < args.len() && !args[j].starts_with("--") {
                        files.push(args[j].clone());
                        j += 1;
                    }
                    explicit_files = Some(files);
                    i = j;
                }
            }
            other => fail(&format!("unknown argument: {other} (see --help)")),
        }
    }

    let dag = Dag::load(&hermit);

    // Resolve the ONE-HOP full-green anchor. An explicit --anchor is NOT trusted
    // on the bare SHA: it is DEREFERENCED to a clean full-green receipt exactly
    // like the auto path, and REFUSED LOUDLY (exit 2) if the SHA is not one. See
    // verify_anchor_override for why an unverified override manufactures a fake
    // green.
    let anchor = match &anchor_override {
        Some(sha) => Some(verify_anchor_override(&ci_hub, sha)),
        None => resolve_anchor(&ci_hub, &branch, no_fetch),
    };

    // Changed paths: explicit list wins; else CROSS-REPO diff since the anchor
    // (hermit paths verbatim + reverie paths prefixed `reverie/`, baselined at
    // the reverie rev PINNED by the anchor's Cargo.lock). A hermit-only diff can
    // never contain a `reverie/` path, so this cross-repo diff is what makes the
    // owner's reverie-kvm-only case observable — and thus narrowable — at all.
    let mut diff_notes: Vec<String> = Vec::new();
    let (files, mut sel) = match (&explicit_files, &anchor) {
        (Some(f), _) => (f.clone(), select(&dag, f)),
        (None, Some(a)) => {
            let f = changed_cross_repo(&hermit, &reverie, &a.sha, &mut diff_notes);
            let s = select(&dag, &f);
            (f, s)
        }
        (None, None) => {
            // No anchor and no explicit files: cannot establish a delta.
            // Conservative FULL with an explicit reason.
            let s = Selection {
                decision: Decision::Full,
                selected: dag.all_ids(),
                reasons: vec![
                    "no full-green anchor available and no --files given -> full suite \
                     (one hop cannot be established)"
                        .into(),
                ],
            };
            (Vec::new(), s)
        }
    };

    if anchor.is_none() && sel.decision != Decision::Full {
        // Without a proven full-green anchor the delta is meaningless: nothing
        // vouches for the un-run steps. Fail safe to FULL.
        sel = Selection {
            decision: Decision::Full,
            selected: dag.all_ids(),
            reasons: vec![
                "no full-green anchor -> cannot trust any skip; conservative full suite".into(),
            ],
        };
    }

    for n in &diff_notes {
        eprintln!("anchored-test-selection: {n}");
    }

    finish(&dag, &anchor, &files, &sel, &format);

    // --run: execute through validate's selected constructed-plan interface. A
    // FULL decision selects every tag from the complete constructed lane,
    // including generated downstream checks; a SELECTIVE decision uses the
    // exact narrowed tags. The driver attaches explicit result ownership before
    // either plan verification or execution; no raw lane JSON crosses this
    // boundary.
    if run {
        let validate = validate.unwrap_or_else(|| hermit.join("scripts/validate.rs"));
        if !validate.is_file() {
            fail(&format!(
                "--run: validate.rs not found at {}; pass --validate <path> or point --hermit \
                 at a checkout containing scripts/validate.rs",
                validate.display()
            ));
        }
        let hermit = hermit
            .canonicalize()
            .unwrap_or_else(|e| fail(&format!("cannot resolve {}: {e}", hermit.display())));
        let validate = validate
            .canonicalize()
            .unwrap_or_else(|e| fail(&format!("cannot resolve {}: {e}", validate.display())));
        let rc = run_selection(&dag, &hermit, &validate, &sel, &passthrough);
        std::process::exit(rc);
    }
}

/// Execute one validate-owned constructed plan per nonempty lane.
///
/// FULL selects every tag from the complete constructed lane, including
/// generated downstream checks that are not named in the raw DAG, and requires
/// the selected output to equal that complete plan. This uses validate's
/// supported off-record admission path; an unselected portable full-cost run is
/// correctly reserved for ci-hub. For SELECTIVE, the selected tags are already
/// dependency-closed over the shipped lane; the validator repeats closure over
/// its constructed plan, adding its canonical preflight nodes. It attaches an
/// explicit `result_manifests` list to every step before execution. We first
/// export and inspect the exact plan; a missing/added selected tag, changed
/// canonical order, dangling dependency, or absent declaration refuses the run.
/// No raw lane JSON and no `RUN_DAG_FILE_OVERRIDE` cross this boundary.
fn run_selection(
    dag: &Dag,
    hermit: &Path,
    validate: &Path,
    sel: &Selection,
    passthrough: &[String],
) -> i32 {
    const LANES: [&str; 2] = ["portable", "privileged"];
    if let Err(error) = validate_execution_passthrough(passthrough) {
        eprintln!("anchored-test-selection: cannot build validate command: {error}");
        return 2;
    }
    if sel.decision == Decision::Skip {
        eprintln!(
            "anchored-test-selection: [SKIP] all changes inert -> 0 lanes invoked (nothing to run)"
        );
        return 0;
    }

    let mut overall_rc = 0;
    for lane in LANES {
        let selected = selected_lane_jobs(dag, sel, lane);
        if selected.is_empty() {
            eprintln!(
                "anchored-test-selection: [{}] lane '{lane}': 0 selected nodes -> WHOLE LANE SKIPPED (not invoked)",
                decision_str(sel.decision).to_ascii_uppercase()
            );
            continue;
        }
        let verified = match sel.decision {
            Decision::Full => construct_and_verify_full(hermit, validate, lane),
            Decision::Selective => construct_and_verify_subset(hermit, validate, lane, &selected),
            Decision::Skip => unreachable!("Skip returned before lane execution"),
        };
        let verified = match verified {
            Ok(verified) => verified,
            Err(error) => {
                eprintln!(
                    "anchored-test-selection: refusing lane '{lane}' before execution: {error}"
                );
                return 2;
            }
        };
        let plan_kind = if sel.decision == Decision::Full {
            "complete"
        } else {
            "selected"
        };
        eprintln!(
            "anchored-test-selection: [{}] lane '{lane}': {} raw lane node(s), {constructed} {plan_kind} constructed step(s), all with explicit result manifests",
            decision_str(sel.decision).to_ascii_uppercase(),
            selected.len(),
            constructed = verified.constructed_steps,
        );
        let start = std::time::Instant::now();
        let rc = exec_validate(hermit, validate, lane, &verified.validate_tags, passthrough);
        let secs = start.elapsed().as_secs_f64();
        let execution = if sel.decision == Decision::Full {
            "--selected (complete constructed plan)"
        } else {
            "--selected"
        };
        eprintln!(
            "anchored-test-selection: lane '{lane}' validate {execution} -> rc={rc} wall={secs:.1}s"
        );
        if rc != 0 {
            overall_rc = rc;
        }
    }
    overall_rc
}

fn selected_lane_jobs(dag: &Dag, sel: &Selection, lane: &str) -> Vec<String> {
    dag.lane_order
        .get(lane)
        .into_iter()
        .flatten()
        .filter(|job| sel.selected.contains(&format!("{lane}:{job}")))
        .cloned()
        .collect()
}

fn lane_validate_arg(lane: &str) -> Result<&'static str, String> {
    match lane {
        "portable" => Ok("portable-only"),
        "privileged" => Ok("--privileged-only"),
        other => Err(format!("unsupported validation lane {other:?}")),
    }
}

fn parent_flag_before_passthrough(args: &[String], flag: &str) -> bool {
    args.iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| argument == flag)
}

fn validate_execution_passthrough(passthrough: &[String]) -> Result<(), String> {
    let mut index = 0;
    while index < passthrough.len() {
        let argument = passthrough[index].as_str();
        let takes_value = matches!(argument, "-j" | "--run-timeout" | "--verbosity");
        let is_flag = matches!(
            argument,
            "-k" | "--keep-going" | "--verbose" | "--ignore-cache" | "--allow-cgroup-failure"
        );
        if !takes_value && !is_flag {
            return Err(format!(
                "passthrough argument {argument:?} is not an execution-only validate option; \
                 use only -j, --run-timeout, --verbosity, -k/--keep-going, --verbose, \
                 --ignore-cache, or --allow-cgroup-failure. Run validate query, plan, help, \
                 and self-test modes separately"
            ));
        }
        if takes_value {
            let value = passthrough.get(index + 1).ok_or_else(|| {
                format!(
                    "passthrough argument {argument:?} needs a value; provide it after the option"
                )
            })?;
            if value.starts_with('-') {
                return Err(format!(
                    "passthrough argument {argument:?} needs a value, got option {value:?}"
                ));
            }
            index += 1;
        }
        index += 1;
    }
    Ok(())
}

fn selected_validate_args(
    lane: &str,
    selected: &[String],
    constructed_out: Option<&Path>,
    passthrough: &[String],
) -> Result<Vec<String>, String> {
    if selected.is_empty() {
        return Err(format!("lane {lane:?} has no selected tags"));
    }
    validate_execution_passthrough(passthrough)?;
    let mut args = vec![
        lane_validate_arg(lane)?.to_string(),
        "--selected".into(),
        selected.join(","),
    ];
    if let Some(out) = constructed_out {
        args.push("--write-constructed-dag".into());
        args.push(out.to_string_lossy().into_owned());
    } else {
        args.push("--allow-local-off-the-record-run".into());
        args.extend(passthrough.iter().cloned());
    }
    Ok(args)
}

fn execution_validate_args(
    lane: &str,
    selected: &[String],
    passthrough: &[String],
) -> Result<Vec<String>, String> {
    validate_execution_passthrough(passthrough)?;
    if selected.is_empty() {
        return Err(format!("lane {lane:?} has no selected tags"));
    }
    let mut args = vec![
        lane_validate_arg(lane)?.to_string(),
        "--selected".into(),
        selected.join(","),
    ];
    args.push("--allow-local-off-the-record-run".into());
    args.extend(passthrough.iter().cloned());
    Ok(args)
}

fn configured_validate_command(validate: &Path, hermit: &Path, args: &[String]) -> Command {
    let mut cmd = Command::new(validate);
    cmd.current_dir(hermit)
        .args(args)
        .env_remove("RUN_DAG_FILE_OVERRIDE")
        .env_remove("VALIDATE_LEVEL")
        .env_remove("VALIDATE_FORCE_FULL");
    cmd
}

fn exec_validate(
    hermit: &Path,
    validate: &Path,
    lane: &str,
    selected: &[String],
    passthrough: &[String],
) -> i32 {
    let args = match execution_validate_args(lane, selected, passthrough) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("anchored-test-selection: cannot build validate command: {error}");
            return 2;
        }
    };
    let mut cmd = configured_validate_command(validate, hermit, &args);
    match cmd.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!(
                "anchored-test-selection: cannot exec {}: {e}",
                validate.display()
            );
            2
        }
    }
}

fn constructed_step_tag(step: &Value) -> Option<String> {
    Some(format!(
        "{}.{}",
        step.get("group")?.as_str()?,
        step.get("job")?.as_str()?
    ))
}

#[derive(Debug)]
struct ConstructedStep {
    tag: String,
    deps: Vec<String>,
    result_manifests: Vec<Value>,
}

fn inspect_constructed_plan(json: &str) -> Result<Vec<ConstructedStep>, String> {
    let value: Value = serde_json::from_str(json)
        .map_err(|error| format!("constructed plan is not valid JSON: {error}"))?;
    let steps = value
        .get("steps")
        .and_then(Value::as_array)
        .ok_or("constructed plan has no steps array")?;
    if steps.is_empty() {
        return Err("constructed plan has no steps".into());
    }

    let mut inspected = Vec::with_capacity(steps.len());
    let mut seen = BTreeSet::new();
    for (index, step) in steps.iter().enumerate() {
        let tag = constructed_step_tag(step)
            .ok_or_else(|| format!("constructed step {index} has no string group/job identity"))?;
        if !seen.insert(tag.clone()) {
            return Err(format!("constructed plan repeats step {tag}"));
        }
        let result_manifests = step
            .get("result_manifests")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| {
                format!("constructed step {tag} lacks an explicit result_manifests array")
            })?;
        let deps = step
            .get("deps")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("constructed step {tag} has no deps array"))?;
        let deps =
            deps.iter()
                .map(|dependency| {
                    dependency.as_str().map(str::to_string).ok_or_else(|| {
                        format!("constructed step {tag} has a non-string dependency")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
        inspected.push(ConstructedStep {
            tag,
            deps,
            result_manifests,
        });
    }
    for step in &inspected {
        for dependency in &step.deps {
            if !seen.contains(dependency) {
                return Err(format!(
                    "constructed step {} has dangling dependency {dependency}",
                    step.tag
                ));
            }
        }
    }
    Ok(inspected)
}

const STRICT_COMPAT_PLACEHOLDER: &str = "test.strict_compat";

fn normalize_selected_tags(
    selected: &[String],
    full: &[ConstructedStep],
) -> Result<Vec<String>, String> {
    let available: BTreeSet<&str> = full.iter().map(|step| step.tag.as_str()).collect();
    let mut normalized = Vec::new();
    for raw in selected {
        let candidate = if available.contains(raw.as_str()) {
            raw.as_str()
        } else {
            // Plan construction deduplicates these raw lane nodes onto the
            // earlier canonical preflight nodes. Carry the selected work to the
            // survivor instead of asking --selected for a tag that no longer
            // exists. Every other missing tag is a drift refusal.
            match raw.as_str() {
                "check.reverie_pin" if available.contains("pre.reverie_pin") => "pre.reverie_pin",
                "e2e.metadata" if available.contains("gate.manifest") => "gate.manifest",
                STRICT_COMPAT_PLACEHOLDER
                    if full.iter().any(|step| step.tag.starts_with("compat.")) =>
                {
                    STRICT_COMPAT_PLACEHOLDER
                }
                _ => {
                    return Err(format!(
                        "selected raw lane tag {raw} has no constructed-plan identity"
                    ));
                }
            }
        };
        if !normalized.iter().any(|tag| tag == candidate) {
            normalized.push(candidate.to_string());
        }
    }
    Ok(normalized)
}

fn expected_constructed_tags(
    full: &[ConstructedStep],
    selected: &[String],
) -> Result<BTreeSet<String>, String> {
    let dependencies: BTreeMap<&str, &[String]> = full
        .iter()
        .map(|step| (step.tag.as_str(), step.deps.as_slice()))
        .collect();
    let mut keep = BTreeSet::new();
    for tag in selected {
        if tag == STRICT_COMPAT_PLACEHOLDER {
            keep.extend(
                full.iter()
                    .filter(|step| {
                        step.tag.starts_with("compat.") || step.tag == "compatprep.fixtures"
                    })
                    .map(|step| step.tag.clone()),
            );
        } else if dependencies.contains_key(tag.as_str()) {
            keep.insert(tag.clone());
        } else {
            return Err(format!(
                "normalized selected tag {tag} is absent from the constructed plan"
            ));
        }
    }
    let mut pending: Vec<String> = keep.iter().cloned().collect();
    while let Some(tag) = pending.pop() {
        for dependency in dependencies.get(tag.as_str()).into_iter().flat_map(|v| *v) {
            if keep.insert(dependency.clone()) {
                pending.push(dependency.clone());
            }
        }
    }
    Ok(keep)
}

fn verify_constructed_subset(
    json: &str,
    full: &[ConstructedStep],
    selected: &[String],
) -> Result<usize, String> {
    let inspected = inspect_constructed_plan(json)?;
    let expected = expected_constructed_tags(full, selected)?;
    let expected_order: Vec<&str> = full
        .iter()
        .filter(|step| expected.contains(&step.tag))
        .map(|step| step.tag.as_str())
        .collect();
    let actual_order: Vec<&str> = inspected.iter().map(|step| step.tag.as_str()).collect();
    if actual_order != expected_order {
        return Err(format!(
            "constructed selection changed cells/order: expected={expected_order:?} actual={actual_order:?}"
        ));
    }
    for (actual, expected) in inspected
        .iter()
        .zip(full.iter().filter(|step| expected.contains(&step.tag)))
    {
        if actual.deps != expected.deps {
            return Err(format!(
                "constructed step {} changed dependencies: expected={:?} actual={:?}",
                actual.tag, expected.deps, actual.deps
            ));
        }
        if actual.result_manifests != expected.result_manifests {
            return Err(format!(
                "constructed step {} changed result_manifests: expected={:?} actual={:?}",
                actual.tag, expected.result_manifests, actual.result_manifests
            ));
        }
    }

    let strict_selected = selected
        .iter()
        .any(|tag| tag == STRICT_COMPAT_PLACEHOLDER || tag.starts_with("compat."));
    let strict_present = inspected
        .iter()
        .any(|step| step.tag == STRICT_COMPAT_PLACEHOLDER);
    let compat_count = inspected
        .iter()
        .filter(|step| step.tag.starts_with("compat."))
        .count();
    if strict_selected && (strict_present || compat_count == 0) {
        return Err(format!(
            "constructed strict-compat selection was not expanded: placeholder_present={strict_present} compat_nodes={compat_count}"
        ));
    }
    if !strict_selected && (strict_present || compat_count != 0) {
        return Err(format!(
            "constructed plan added unselected strict-compat work: placeholder_present={strict_present} compat_nodes={compat_count}"
        ));
    }
    Ok(inspected.len())
}

struct VerifiedSelection {
    validate_tags: Vec<String>,
    constructed_steps: usize,
}

fn write_constructed_plan(
    hermit: &Path,
    validate: &Path,
    lane: &str,
    selected: Option<&[String]>,
    out: &Path,
) -> Result<String, String> {
    match std::fs::remove_file(out) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot clear prior constructed plan {}: {error}",
                out.display()
            ));
        }
    }
    let args = match selected {
        Some(selected) => selected_validate_args(lane, selected, Some(out), &[])?,
        None => vec![
            lane_validate_arg(lane)?.to_string(),
            "--write-constructed-dag".into(),
            out.to_string_lossy().into_owned(),
        ],
    };
    let output = configured_validate_command(validate, hermit, &args)
        .output()
        .map_err(|error| format!("cannot construct selected plan: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "validate could not construct {lane} plan ({}): {}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    std::fs::read_to_string(out).map_err(|error| {
        format!(
            "validate reported success but {} is unreadable: {error}",
            out.display()
        )
    })
}

fn construct_and_verify_subset(
    hermit: &Path,
    validate: &Path,
    lane: &str,
    raw_selected: &[String],
) -> Result<VerifiedSelection, String> {
    let stem = format!("anchored-constructed-{}-{lane}", std::process::id());
    let full_out = std::env::temp_dir().join(format!("{stem}-full.json"));
    let selected_out = std::env::temp_dir().join(format!("{stem}-selected.json"));
    let result = (|| {
        let full_json = write_constructed_plan(hermit, validate, lane, None, &full_out)?;
        let full = inspect_constructed_plan(&full_json)?;
        let validate_tags = normalize_selected_tags(raw_selected, &full)?;
        let selected_json =
            write_constructed_plan(hermit, validate, lane, Some(&validate_tags), &selected_out)?;
        let constructed_steps = verify_constructed_subset(&selected_json, &full, &validate_tags)?;
        Ok(VerifiedSelection {
            validate_tags,
            constructed_steps,
        })
    })();
    let _ = std::fs::remove_file(&full_out);
    let _ = std::fs::remove_file(&selected_out);
    result
}

fn construct_and_verify_full(
    hermit: &Path,
    validate: &Path,
    lane: &str,
) -> Result<VerifiedSelection, String> {
    let stem = format!("anchored-constructed-{}-{lane}-full", std::process::id());
    let full_out = std::env::temp_dir().join(format!("{stem}.json"));
    let selected_out = std::env::temp_dir().join(format!("{stem}-selected.json"));
    let result = (|| {
        let full_json = write_constructed_plan(hermit, validate, lane, None, &full_out)?;
        let full = inspect_constructed_plan(&full_json)?;
        let validate_tags = full.iter().map(|step| step.tag.clone()).collect::<Vec<_>>();
        let selected_json =
            write_constructed_plan(hermit, validate, lane, Some(&validate_tags), &selected_out)?;
        let constructed_steps = verify_constructed_subset(&selected_json, &full, &validate_tags)?;
        let full_value: Value = serde_json::from_str(&full_json)
            .map_err(|error| format!("complete constructed plan is not valid JSON: {error}"))?;
        let selected_value: Value = serde_json::from_str(&selected_json)
            .map_err(|error| format!("selected complete plan is not valid JSON: {error}"))?;
        if selected_value != full_value {
            return Err(
                "selecting every complete constructed tag changed the complete plan".into(),
            );
        }
        Ok(VerifiedSelection {
            validate_tags,
            constructed_steps,
        })
    })();
    let _ = std::fs::remove_file(&full_out);
    let _ = std::fs::remove_file(&selected_out);
    result
}

fn finish(dag: &Dag, anchor: &Option<Anchor>, files: &[String], sel: &Selection, format: &str) {
    match format {
        "human" => emit_human(dag, anchor, files, sel),
        "json" => emit_json(dag, anchor, files, sel),
        other => fail(&format!("unknown --format {other} (human|json)")),
    }
}

fn need(args: &[String], i: usize) -> String {
    args.get(i + 1)
        .cloned()
        .unwrap_or_else(|| fail(&format!("{} needs a value", args[i])))
}

fn print_help() {
    print!(
        "\
Usage: scripts/anchored-test-selection.rs [OPTIONS]

Anchored, one-hop test selection for the Hermit CI DAG. Anchor on the newest
FULL green, diff paths changed since it, map paths -> DAG steps, report the
steps SELECTED and SKIPPED. Conservative: any unmapped path -> full suite.

  --hermit <dir>     Hermit checkout (default: hermit)
  --ci-hub <path>    ci-hub binary (default: ci-hub/ci-hub)
  --branch <name>    branch for newest-green anchor (default: main)
  --anchor <sha>     use a fixed anchor instead of newest-green (testing/repro).
                     NOT trusted on the bare SHA: it is DEREFERENCED through
                     `ci-hub validate-status` and REFUSED (exit 2), naming what
                     was missing, unless the SHA carries a clean FULL-profile,
                     FULL-selection, passing receipt (verdict==VALIDATED). An
                     unverified anchor would manufacture a fake green.
  --no-fetch         pass --no-fetch to newest-green (offline)
  --files <paths…>   classify an explicit path list (space-separated)
  --files -          read the path list from stdin (one per line)
  --format <fmt>     human (default) | json
  --reverie <dir>    reverie checkout for cross-repo diff (default: reverie)
  --run              EXECUTE through validate's constructed-plan --selected
                     interface. FULL selects every complete constructed tag,
                     including generated downstream checks; SELECTIVE uses the
                     narrowed tags. A lane with 0 selected nodes is not invoked.
                     Before execution, the constructed plan is checked for exact
                     selected order and explicit result manifests on every step.
  --validate <path>  validate.rs to invoke (default: <hermit>/scripts/validate.rs)
  --                 forward execution-only validate options: -j N,
                     --run-timeout SEC, --verbosity N, -k/--keep-going,
                     --verbose, --ignore-cache, and --allow-cgroup-failure
                     (explicit UNBOXED relaxation). Query, plan, help, self-test,
                     and selection-changing options are refused.
  --self-test        run built-in unit tests and exit non-zero on failure

CROSS-REPO DIFF: with an anchor and no explicit --files, paths changed since the
anchor are collected across BOTH hermit (verbatim) and reverie (prefixed
`reverie/`, baselined at the reverie rev pinned in the anchor's Cargo.lock). A
reverie baseline that cannot be derived or fetched => conservative FULL.

ONE HOP: the anchor must be a FULL green (selection_mode==full); newest-green
already guarantees this and we re-assert it. No full-green anchor => full run.
"
    );
}

// ---------------------------------------------------------------------------
// Built-in tests
// ---------------------------------------------------------------------------

fn self_test() {
    let mut failures = 0;
    let mut total = 0;
    let mut check = |name: &str, cond: bool| {
        total += 1;
        if cond {
            println!("ok   - {name}");
        } else {
            println!("FAIL - {name}");
            failures += 1;
        }
    };

    // --- glob matcher ---
    check(
        "glob exact root Cargo.toml",
        glob_match("Cargo.toml", "Cargo.toml"),
    );
    check(
        "glob root Cargo.toml is not backend Cargo.toml",
        !glob_match("Cargo.toml", "reverie/reverie-kvm/Cargo.toml"),
    );
    check(
        "glob prefix dir",
        glob_match("reverie/reverie-kvm/**", "reverie/reverie-kvm/src/lib.rs"),
    );
    check(
        "glob **/*.md",
        glob_match("**/*.md", "reverie/reverie-kvm/README.md"),
    );

    // --- classify: THE ASYMMETRY (owner's load-bearing case) ---
    let (a, _) = classify("reverie/reverie-kvm/src/lib.rs");
    check(
        "reverie-kvm -> Steps (not Full)",
        matches!(a, Action::Steps(_)),
    );
    if let (Action::Steps(s), _) = classify("reverie/reverie-kvm/src/lib.rs") {
        check(
            "reverie-kvm -> privileged parity step",
            s.contains(&"privileged:e2e.manifest_backend_parity_c".to_string()),
        );
        check(
            "reverie-kvm -> NO portable dbt step",
            !s.contains(&"portable:test.dbt_parity".to_string()),
        );
    }
    check(
        "reverie CORE -> Full",
        matches!(classify("reverie/reverie/src/lib.rs").0, Action::Full),
    );
    check(
        "reverie-ptrace (core default backend) -> Full",
        matches!(
            classify("reverie/reverie-ptrace/src/lib.rs").0,
            Action::Full
        ),
    );
    check(
        "reverie-syscalls (core) -> Full",
        matches!(
            classify("reverie/reverie-syscalls/src/lib.rs").0,
            Action::Full
        ),
    );

    // --- classify: backends narrow to their own backend ---
    check(
        "detcore-dbi -> Steps",
        matches!(classify("detcore-dbi/src/lib.rs").0, Action::Steps(_)),
    );
    check(
        "detcore-sabre -> Steps",
        matches!(classify("detcore-sabre/src/lib.rs").0, Action::Steps(_)),
    );
    check(
        "reverie-liteinst -> Steps",
        matches!(
            classify("reverie/reverie-liteinst/src/x.rs").0,
            Action::Steps(_)
        ),
    );
    check(
        "reverie-e9patch -> Steps",
        matches!(
            classify("reverie/reverie-e9patch/src/x.rs").0,
            Action::Steps(_)
        ),
    );

    // --- classify: conservative + inert ---
    check(
        "root Cargo.lock -> Full",
        matches!(classify("Cargo.lock").0, Action::Full),
    );
    check(
        "ci/** -> Full",
        matches!(classify("ci/dag/portable.json").0, Action::Full),
    );
    check(
        "scripts/validate.rs -> Full",
        matches!(classify("scripts/validate.rs").0, Action::Full),
    );
    check(
        "core hermit detcore/ -> Full (unmapped)",
        matches!(classify("detcore/src/scheduler.rs").0, Action::Full),
    );
    check(
        "hermit-cli/ -> Full (unmapped)",
        matches!(classify("hermit-cli/src/x.rs").0, Action::Full),
    );
    check(
        "brand new area -> Full (unmapped)",
        matches!(classify("some/new/area/x.py").0, Action::Full),
    );
    check(
        "docs -> Irrelevant",
        matches!(classify("docs/Users.md").0, Action::Irrelevant),
    );
    check(
        "top-level md -> Irrelevant",
        matches!(classify("README.md").0, Action::Irrelevant),
    );

    // --- selection over the LIVE DAG ---
    let dag = Dag::load(Path::new("hermit"));
    let n_all = dag.all_ids().len();
    check(
        "DAG has both lanes' nodes",
        dag.nodes.keys().any(|k| k.starts_with("portable:"))
            && dag.nodes.keys().any(|k| k.starts_with("privileged:")),
    );

    // Every step named in the map exists in the live DAG (stale-map guard).
    let mut mapped: BTreeSet<String> = BTreeSet::new();
    for r in rules() {
        if let Action::Steps(s) = r.action {
            for id in s {
                mapped.insert(id);
            }
        }
    }
    for pf in PREFLIGHT {
        mapped.insert((*pf).to_string());
    }
    let stale: Vec<String> = mapped
        .iter()
        .filter(|s| !dag.nodes.contains_key(*s))
        .cloned()
        .collect();
    check(
        &format!("all mapped steps exist in live DAG (stale: {stale:?})"),
        stale.is_empty(),
    );

    // reverie-kvm-only diff => selective, includes privileged parity + deps,
    // excludes portable backend/core work.
    let kvm = select(&dag, &vec!["reverie/reverie-kvm/src/lib.rs".into()]);
    check(
        "reverie-kvm diff => selective",
        kvm.decision == Decision::Selective,
    );
    check(
        "reverie-kvm selects privileged parity",
        kvm.selected
            .contains("privileged:e2e.manifest_backend_parity_c"),
    );
    check(
        "reverie-kvm pulls privileged build dep",
        kvm.selected.contains("privileged:build.privileged_tests"),
    );
    check(
        "reverie-kvm pulls privileged manifest_guests dep",
        kvm.selected.contains("privileged:build.manifest_guests"),
    );
    check(
        "reverie-kvm does NOT run portable strict_compat",
        !kvm.selected.contains("portable:test.strict_compat"),
    );
    let excludes_portable_dbt =
        |selected: &BTreeSet<String>| !selected.contains("portable:test.dbt_parity");
    check(
        "reverie-kvm does NOT run portable dbt_parity",
        excludes_portable_dbt(&kvm.selected),
    );
    let mut kvm_with_dbt = kvm.selected.clone();
    kvm_with_dbt.insert("portable:test.dbt_parity".into());
    check(
        "reverie-kvm DBT exclusion catches insertion of the live dbt_parity tag",
        !excludes_portable_dbt(&kvm_with_dbt),
    );
    check("reverie-kvm is a strict subset", kvm.selected.len() < n_all);
    check(
        "reverie-kvm prunes the manifest gate",
        dag.manifest_gate(&kvm.selected).len() < dag.manifest_gate(&dag.all_ids()).len(),
    );

    // reverie CORE diff => FULL (honest: no narrowing).
    let core = select(&dag, &vec!["reverie/reverie/src/lib.rs".into()]);
    check("reverie core diff => full", core.decision == Decision::Full);
    check(
        "reverie core selects everything",
        core.selected.len() == n_all,
    );

    // dbi-only => selective, dbi steps, not sabre/kvm.
    let dbi = select(&dag, &vec!["detcore-dbi/src/lib.rs".into()]);
    check("dbi diff => selective", dbi.decision == Decision::Selective);
    check(
        "dbi selects portable dbt_parity",
        dbi.selected.contains("portable:test.dbt_parity"),
    );
    check(
        "dbi does NOT select privileged kvm parity",
        !dbi.selected
            .contains("privileged:e2e.manifest_backend_parity_c"),
    );
    check(
        "dbi does NOT select sabre_examples",
        !dbi.selected.contains("portable:test.sabre_examples"),
    );

    // mixed backend + docs => selective (docs inert); backend + core => full.
    let mixed = select(
        &dag,
        &vec!["reverie/reverie-kvm/src/x.rs".into(), "README.md".into()],
    );
    check(
        "kvm + docs => selective",
        mixed.decision == Decision::Selective,
    );
    let mixed2 = select(
        &dag,
        &vec!["reverie/reverie-kvm/src/x.rs".into(), "Cargo.lock".into()],
    );
    check(
        "kvm + Cargo.lock => full (force wins)",
        mixed2.decision == Decision::Full,
    );
    let mixed3 = select(
        &dag,
        &vec![
            "reverie/reverie-kvm/src/x.rs".into(),
            "detcore/src/scheduler.rs".into(),
        ],
    );
    check(
        "kvm + core hermit => full (unmapped core wins)",
        mixed3.decision == Decision::Full,
    );

    // pure docs => skip; empty => full.
    let docs = select(&dag, &vec!["docs/a.md".into(), "README.md".into()]);
    check(
        "pure docs => skip",
        docs.decision == Decision::Skip && docs.selected.is_empty(),
    );
    let empty = select(&dag, &vec![]);
    check("empty change => full", empty.decision == Decision::Full);

    // dependency closure is transitive (parity -> build.manifest_guests -> e2e.metadata).
    check(
        "closure pulls e2e.metadata for kvm",
        kvm.selected.contains("privileged:e2e.metadata"),
    );

    // --- reverie pin parser (cross-repo baseline derivation) ---
    let lock = "\
name = \"reverie\"\n\
version = \"0.1.0\"\n\
source = \"git+https://github.com/rrnewton/reverie.git?rev=79517704abc1234def567890abcdef1234567890#79517704\"\n";
    check(
        "parse_reverie_rev finds the pinned rev",
        parse_reverie_rev(lock).as_deref() == Some("79517704abc1234def567890abcdef1234567890"),
    );
    check(
        "parse_reverie_rev None when no reverie git source",
        parse_reverie_rev("name = \"serde\"\nsource = \"registry+https://crates.io\"\n").is_none(),
    );
    check(
        "parse_reverie_rev stops at non-hex delimiter",
        parse_reverie_rev("...reverie.git?rev=deadbeef#deadbeef").as_deref() == Some("deadbeef"),
    );

    // --- anchor-override receipt verifier (interpret_validate_status) ---
    // The THREE planted receipt shapes the owner named, exercised against the
    // pure interpreter (the actual `ci-hub validate-status --json` output shapes).
    //
    // case1 NO RECORD: a nonexistent SHA has no ledger record => NOT-VALIDATED
    //                  => REFUSE (its skipped steps were proven green nowhere).
    let case1_no_record = r#"{"disqualified_count":0,"exit_code":4,
        "newest_qualifying":null,"qualifying_count":0,"schema_version":1,
        "sha":"deadbeefdeadbeefdeadbeefdeadbeefdeadbeef","verdict":"NOT-VALIDATED"}"#;
    check(
        "override case1 (no record) => REFUSE",
        interpret_validate_status("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef", case1_no_record)
            .is_err(),
    );

    // case2 COMPAT-ONLY: only a 2-check portable-strict-compat-only record
    //                    exists; it is disqualified => NOT-VALIDATED => REFUSE.
    let case2_compat_only = r#"{"disqualified_count":2,"exit_code":4,
        "newest_qualifying":null,"qualifying_count":0,"schema_version":1,
        "sha":"d8e95058b1fc7db5bb6b9cd41d0a5a0a4170148f","verdict":"NOT-VALIDATED"}"#;
    check(
        "override case2 (compat-only disqualified) => REFUSE",
        interpret_validate_status(
            "d8e95058b1fc7db5bb6b9cd41d0a5a0a4170148f",
            case2_compat_only,
        )
        .is_err(),
    );

    // case3 REAL FULL GREEN: THE POSITIVE CONTROL. A guard that refuses
    //                        everything passes case1+case2 and is USELESS; this
    //                        asserts it ACCEPTS a genuine full green and returns
    //                        the RESOLVED full SHA.
    let case3_full_green = r#"{"disqualified_count":1,"exit_code":0,
        "newest_qualifying":{"finished_at":"2026-08-05T03:34:08Z","host":"test-host",
        "profile":"full","real_seconds":556.0,"result":"pass","selection_mode":"full",
        "slot":"standalone"},"qualifying_count":1,"schema_version":1,
        "sha":"65ee45596ad3f9a16507e2fdd60ebbc5e23e5630","verdict":"VALIDATED"}"#;
    check(
        "override case3 (real full green) => ACCEPT [POSITIVE CONTROL]",
        interpret_validate_status("65ee4559", case3_full_green).as_deref()
            == Ok("65ee45596ad3f9a16507e2fdd60ebbc5e23e5630"),
    );

    // case4 LOOSENED-VALIDATED guard: verdict says VALIDATED but the carried
    //       record is compat-only. Defense in depth must still REFUSE — a future
    //       widening of VALIDATED cannot silently widen this gate.
    let case4_loosened = r#"{"newest_qualifying":{"profile":"portable-strict-compat-only",
        "result":"pass","selection_mode":"full"},"qualifying_count":1,
        "sha":"abc1234","verdict":"VALIDATED"}"#;
    check(
        "override case4 (VALIDATED but compat-only profile) => REFUSE (defense in depth)",
        interpret_validate_status("abc1234", case4_loosened).is_err(),
    );

    // Malformed output must fail closed, not accept.
    check(
        "override malformed output => REFUSE",
        interpret_validate_status("whatever", "not json at all").is_err(),
    );

    // --- constructed-plan execution boundary ---
    let kvm_privileged = selected_lane_jobs(&dag, &kvm, "privileged");
    let expected_kvm_privileged: Vec<String> = dag
        .lane_order
        .get("privileged")
        .unwrap()
        .iter()
        .filter(|tag| kvm.selected.contains(&format!("privileged:{tag}")))
        .cloned()
        .collect();
    check(
        "selected lane tags preserve canonical DAG order",
        kvm_privileged == expected_kvm_privileged,
    );

    let command = execution_validate_args(
        "portable",
        &["build.a".into(), "test.b".into()],
        &["-j".into(), "4".into()],
    );
    check(
        "selected execution uses validate's exact off-record tag interface",
        command.as_deref()
            == Ok(&[
                "portable-only".to_string(),
                "--selected".to_string(),
                "build.a,test.b".to_string(),
                "--allow-local-off-the-record-run".to_string(),
                "-j".to_string(),
                "4".to_string(),
            ][..]),
    );
    let complete_tags = [
        "pre.submodules".to_string(),
        "test.b".to_string(),
        "scorecard.compatibility".to_string(),
    ];
    let full_command = execution_validate_args("portable", &complete_tags, &[]);
    check(
        "full execution selects every complete constructed tag including downstream checks",
        full_command.as_deref()
            == Ok(&[
                "portable-only".to_string(),
                "--selected".to_string(),
                "pre.submodules,test.b,scorecard.compatibility".to_string(),
                "--allow-local-off-the-record-run".to_string(),
            ][..]),
    );
    check(
        "cgroup-failure relaxation is appended without changing anchored selection",
        execution_validate_args(
            "portable",
            &["build.a".into(), "test.b".into()],
            &["--allow-cgroup-failure".into()],
        )
        .as_deref()
            == Ok(&[
                "portable-only".to_string(),
                "--selected".to_string(),
                "build.a,test.b".to_string(),
                "--allow-local-off-the-record-run".to_string(),
                "--allow-cgroup-failure".to_string(),
            ][..]),
    );
    check(
        "passthrough cannot replace the anchored selection",
        execution_validate_args(
            "portable",
            &["test.b".into()],
            &["--selected".into(), "test.other".into()],
        )
        .is_err(),
    );
    for argument in [
        "--probe-host-capability",
        "--probe-host-capability=cpuid-faulting",
        "--show-plan",
        "--show-plan-json",
        "--write-constructed-dag",
        "--self-test",
        "--help",
        "-h",
    ] {
        check(
            &format!("passthrough {argument} cannot return success without selected work"),
            execution_validate_args("portable", &["test.b".into()], &[argument.into()]).is_err(),
        );
    }
    check(
        "execution passthrough refuses a missing option value",
        execution_validate_args("portable", &["test.b".into()], &["--run-timeout".into()]).is_err(),
    );
    let before_delimiter = vec!["--self-test".into(), "--".into(), "--help".into()];
    check(
        "parent self-test is recognized before the passthrough delimiter",
        parent_flag_before_passthrough(&before_delimiter, "--self-test"),
    );
    check(
        "passthrough help is not consumed as parent help",
        !parent_flag_before_passthrough(&before_delimiter, "--help"),
    );
    let after_delimiter = vec!["--".into(), "--self-test".into(), "-h".into()];
    check(
        "passthrough self-test is not consumed as parent self-test",
        !parent_flag_before_passthrough(&after_delimiter, "--self-test"),
    );
    check(
        "passthrough short help is not consumed as parent help",
        !parent_flag_before_passthrough(&after_delimiter, "-h"),
    );

    let selected = vec!["build.a".to_string(), "test.b".to_string()];
    let declared = r#"{"steps":[
        {"group":"pre","job":"submodules","deps":[],"result_manifests":[]},
        {"group":"build","job":"a","deps":["pre.submodules"],"result_manifests":[]},
        {"group":"test","job":"b","deps":["build.a"],"result_manifests":[{"kind":"structured-test-results","schema":2,"path_env":"DAGRUN_TEST_COUNTS_PATH","owner":"test.b"}]}
    ]}"#;
    let full = inspect_constructed_plan(declared).unwrap();
    check(
        "constructed subset keeps selected cells/order and explicit result manifests",
        verify_constructed_subset(declared, &full, &selected) == Ok(3),
    );
    let generated_downstream_json = r#"{"steps":[
            {"group":"pre","job":"submodules","deps":[],"result_manifests":[]},
            {"group":"test","job":"b","deps":["pre.submodules"],"result_manifests":[]},
            {"group":"scorecard","job":"compatibility","deps":["test.b"],"result_manifests":[]}
        ]}"#;
    let generated_downstream = inspect_constructed_plan(generated_downstream_json).unwrap();
    let predecessor_only =
        expected_constructed_tags(&generated_downstream, &["test.b".to_string()]).unwrap();
    check(
        "full execution cannot use predecessor-only selection that omits generated downstream checks",
        generated_downstream
            .iter()
            .any(|step| step.tag == "scorecard.compatibility")
            && !predecessor_only.contains("scorecard.compatibility"),
    );
    check(
        "selecting every constructed tag reproduces the complete plan including downstream checks",
        verify_constructed_subset(
            generated_downstream_json,
            &generated_downstream,
            &complete_tags,
        ) == Ok(3),
    );
    check(
        "constructed subset refuses an unselected lane cell",
        verify_constructed_subset(declared, &full, &["build.a".to_string()]).is_err(),
    );
    let legacy = r#"{"steps":[
        {"group":"build","job":"a","deps":[]},
        {"group":"test","job":"b","deps":["build.a"],"result_manifests":null}
    ]}"#;
    check(
        "constructed subset refuses legacy absent/null result ownership",
        verify_constructed_subset(legacy, &full, &selected).is_err(),
    );
    let declared_value: Value = serde_json::from_str(declared).unwrap();
    let mut dropped_dependency = declared_value.clone();
    dropped_dependency["steps"][2]["deps"] = serde_json::json!([]);
    check(
        "constructed subset refuses a dropped dependency edge",
        verify_constructed_subset(&dropped_dependency.to_string(), &full, &selected)
            .is_err_and(|error| error.contains("changed dependencies")),
    );
    let mut rewired_dependency = declared_value.clone();
    rewired_dependency["steps"][2]["deps"] = serde_json::json!(["pre.submodules"]);
    check(
        "constructed subset refuses a rewired valid dependency edge",
        verify_constructed_subset(&rewired_dependency.to_string(), &full, &selected)
            .is_err_and(|error| error.contains("changed dependencies")),
    );
    let mut removed_result_manifest = declared_value.clone();
    removed_result_manifest["steps"][2]["result_manifests"] = serde_json::json!([]);
    check(
        "constructed subset refuses removal of a producer result manifest",
        verify_constructed_subset(&removed_result_manifest.to_string(), &full, &selected)
            .is_err_and(|error| error.contains("changed result_manifests")),
    );
    let mut changed_result_kind = declared_value.clone();
    changed_result_kind["steps"][2]["result_manifests"][0]["kind"] = serde_json::json!("future");
    check(
        "constructed subset refuses a changed result-manifest kind",
        verify_constructed_subset(&changed_result_kind.to_string(), &full, &selected)
            .is_err_and(|error| error.contains("changed result_manifests")),
    );
    let mut changed_result_owner = declared_value;
    changed_result_owner["steps"][2]["result_manifests"][0]["owner"] =
        serde_json::json!("test.other");
    check(
        "constructed subset refuses a changed result-manifest owner",
        verify_constructed_subset(&changed_result_owner.to_string(), &full, &selected)
            .is_err_and(|error| error.contains("changed result_manifests")),
    );
    let dangling = r#"{"steps":[
        {"group":"build","job":"a","deps":["pre.missing"],"result_manifests":[]}
    ]}"#;
    check(
        "constructed subset refuses dangling dependencies",
        inspect_constructed_plan(dangling).is_err(),
    );
    let strict = r#"{"steps":[
        {"group":"pre","job":"submodules","deps":[],"result_manifests":[]},
        {"group":"compatprep","job":"fixtures","deps":["pre.submodules"],"result_manifests":[]},
        {"group":"compat","job":"smoke","deps":["compatprep.fixtures"],"result_manifests":[]}
    ]}"#;
    let strict_full = inspect_constructed_plan(strict).unwrap();
    check(
        "constructed subset accepts canonical strict-compat expansion",
        verify_constructed_subset(strict, &strict_full, &["test.strict_compat".to_string()])
            .is_ok(),
    );
    let fused = inspect_constructed_plan(
        r#"{"steps":[
            {"group":"pre","job":"reverie_pin","deps":[],"result_manifests":[]},
            {"group":"gate","job":"manifest","deps":["pre.reverie_pin"],"result_manifests":[]}
        ]}"#,
    )
    .unwrap();
    check(
        "raw duplicate tags map to their constructed-plan survivors",
        normalize_selected_tags(
            &["check.reverie_pin".to_string(), "e2e.metadata".to_string()],
            &fused,
        )
        .as_deref()
            == Ok(&["pre.reverie_pin".to_string(), "gate.manifest".to_string()][..]),
    );

    drop(check);
    println!("\n{total} check(s), {failures} failure(s)");
    if failures > 0 {
        std::process::exit(1);
    }
}
