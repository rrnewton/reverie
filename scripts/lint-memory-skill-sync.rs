#!/usr/bin/env rust-script
//! Lint repository-owned coordinator skill structure.
//!
//! WHY: coordinator memories and the `.claude/skills/` files drift apart when a
//! policy/protocol/architecture decision is updated in one place but not the
//! other. This check makes the drift mechanical and loud instead of silent.
//!
//! MODEL:
//!   * Versioned `.claude/skills/` files are the source of truth.
//!   * A local memory may mirror a skill, but absence or drift is advisory and
//!     can never make local state overwrite repository policy.
//!   * Every mapping is `.claude/skills/<memory-slug>/SKILL.md`.
//!   * A directory symlink may bridge directly to a versioned canonical skill
//!     under `agent-utils/skills/<same-name>/`; the external skill is its own
//!     source of truth and deliberately has no duplicate memory body.
//!
//! NOTE ON "sqlite": the task brief assumed memories live in a sqlite DB. This
//! project's memory store is FILE-BASED markdown; this linter reads that store.
//! Override its location with HERMIT_MEMORY_DIR.
//!
//! Exit code: 0 when repository skill structure is valid; 1 only on a
//! repository-owned structural problem. Local-memory warnings do not gate.
//!
//! Usage:
//!   scripts/lint-memory-skill-sync.rs            # lint, human report
//!   scripts/lint-memory-skill-sync.rs --quiet    # only print problems + summary
//!   scripts/lint-memory-skill-sync.rs --gate     # stable key=value result
use std::path::{Path, PathBuf};

const SKILL_DIR: &str = ".claude/skills";

const USAGE: &str = "\
lint-memory-skill-sync.rs — verify repository-owned coordinator skill structure

USAGE:
  scripts/lint-memory-skill-sync.rs            lint and print a human report
  scripts/lint-memory-skill-sync.rs --quiet    print only problems and the summary
  scripts/lint-memory-skill-sync.rs --gate     print stable key=value fields for a consumer
  scripts/lint-memory-skill-sync.rs -h|--help  show this help and exit (no side effects)
  scripts/lint-memory-skill-sync.rs --version  print version and exit (no side effects)

Read-only: never edits memories or skills. Repository skills are authoritative;
local-memory absence/drift is advisory. Override the optional file-based memory
location with HERMIT_MEMORY_DIR.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    if args.iter().any(|a| a == "--version") {
        println!("lint-memory-skill-sync.rs 1.0");
        return;
    }
    let gate = args.iter().any(|a| a == "--gate");
    let quiet = gate || args.iter().any(|a| a == "--quiet");
    let root = find_root();
    let memory_dir = memory_dir(&root);
    let skill_dir = root.join(SKILL_DIR);

    let mut warnings: Vec<String> = Vec::new();
    if !memory_dir.is_dir() {
        warnings.push(format!(
            "LOCAL memory dir not found: {} (optional; set HERMIT_MEMORY_DIR)",
            memory_dir.display()
        ));
    }

    // Collect optional CORE-memory mirrors from the file-based store.
    let mut core: Vec<(String, PathBuf, Meta)> = Vec::new();
    for entry in read_md_files(&memory_dir) {
        let slug = stem(&entry);
        if slug == "MEMORY" {
            continue;
        }
        let content = std::fs::read_to_string(&entry).unwrap_or_default();
        let meta = parse_meta(&content);
        if meta.core_memory {
            core.push((slug, entry, meta));
        }
    }
    core.sort_by(|a, b| a.0.cmp(&b.0));

    let mut problems: Vec<String> = Vec::new();
    let active_skills = read_skill_files(&root.join(".claude/skills"));
    let mut mapped_skills: Vec<String> = Vec::new();
    let mut ok = 0usize;

    for (slug, path, meta) in &core {
        // 1. metadata self-consistency.
        let Some(want_skill_rel) = &meta.core_skill else {
            warnings.push(format!(
                "META  {slug}: core_memory:true but no core_skill declared"
            ));
            continue;
        };
        let expected = package_skill_rel(slug);
        if want_skill_rel != &expected {
            warnings.push(format!(
                "PATH  {slug}: core_skill must be '{expected}', got '{want_skill_rel}'"
            ));
            continue;
        }
        mapped_skills.push(want_skill_rel.clone());
        // 2. body must carry the visible CORE-MEMORY tag.
        let content = std::fs::read_to_string(path).unwrap_or_default();
        if !content.contains("**CORE-MEMORY**") {
            warnings.push(format!(
                "TAG   {slug}: memory body missing '**CORE-MEMORY**' marker"
            ));
        }
        // 3. mapped skill presence + content equality.
        let skill_path = root.join(want_skill_rel);
        if !skill_path.is_file() {
            warnings.push(format!(
                "LOCAL {slug}: mapped repository skill is absent at {want_skill_rel}"
            ));
            continue;
        }
        let skill = std::fs::read_to_string(&skill_path).unwrap_or_default();
        let skill_meta = parse_meta(&skill);
        if skill_meta.name != meta.name || skill_meta.description != meta.description {
            warnings.push(format!(
                "META  {slug}: mapped skill frontmatter differs from memory"
            ));
            continue;
        }
        let want = canonical_body(&content);
        let have = canonical_body(&skill);
        if have == want {
            ok += 1;
            if !quiet {
                println!("OK    {slug} -> {want_skill_rel}");
            }
        } else {
            warnings.push(format!(
                "LOCAL {slug}: memory mirror differs from authoritative repository skill"
            ));
        }
    }

    // 4. Canonical skills are real package directories. The sole exception is
    // an explicit bridge to a versioned canonical agent-utils package.
    if let Ok(entries) = std::fs::read_dir(&skill_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if path.file_name().and_then(|name| name.to_str()) != Some("README.md") {
                    let rel = path.strip_prefix(&root).unwrap_or(&path).display();
                    problems.push(format!(
                        "PACKAGE {rel}: flat canonical skills are unsupported; use <slug>/SKILL.md"
                    ));
                }
                continue;
            }
            if is_external_skill_bridge_dir(&root, &path) {
                continue;
            }
            if !path.is_dir() {
                let rel = path.strip_prefix(&root).unwrap_or(&path).display();
                // A bridge into Hermit's nested agent-utils submodule that has not
                // been materialised is STILL A FAILURE -- the check stays strict, because
                // accepting an unresolvable bridge would trade a false failure for a
                // weaker check. What changes is that the message names the remedy.
                // Measured 2026-08-11: a plain `git clone` of the parent fails this
                // lint, because clone does not initialise nested submodules, and the
                // old text ("unsupported canonical skill entry") named a SKILL PACKAGE and said
                // nothing about submodules -- so a reader goes hunting for a missing
                // skill instead of running one command. An agent searches for the
                // symptom it is seeing, not for the cause it has not found yet.
                if let Some(target) = absent_submodule_bridge_target(&path) {
                    problems.push(format!(
                        "PACKAGE {rel}: bridge target absent ({target}); \
                         run `git -C hermit submodule update --init --checkout -- agent-utils`"
                    ));
                } else {
                    problems.push(format!("PACKAGE {rel}: unsupported canonical skill entry"));
                }
                continue;
            }
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .display()
                .to_string();
            let is_symlink = std::fs::symlink_metadata(&path)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false);
            if is_symlink || !path.join("SKILL.md").is_file() {
                problems.push(format!(
                    "PACKAGE {rel}: canonical package must be a real directory containing SKILL.md"
                ));
            }
        }
    }

    // 5. Every discoverable repository skill must have valid matching metadata.
    for skill_path in &active_skills {
        let rel = skill_path
            .strip_prefix(&root)
            .unwrap_or(skill_path)
            .to_string_lossy()
            .to_string();
        if is_external_skill_bridge_file(&root, skill_path) {
            if !quiet {
                println!("EXT   {rel} -> canonical agent-utils skill");
            }
            continue;
        }
        let content = std::fs::read_to_string(skill_path).unwrap_or_default();
        let meta = parse_meta(&content);
        let slug = skill_slug(skill_path);
        if meta.name != slug || meta.description.is_empty() {
            problems.push(format!(
                "META  {rel}: frontmatter name must be '{slug}' and description must be nonempty"
            ));
        }
        let count = mapped_skills
            .iter()
            .filter(|mapped| *mapped == &rel)
            .count();
        match count {
            0 | 1 => {}
            n => warnings.push(format!(
                "LOCAL {rel}: {n} local memories claim this repository skill"
            )),
        }
    }

    if gate {
        println!(
            "state={}",
            if problems.is_empty() {
                "ok"
            } else {
                "problems"
            }
        );
        println!(
            "summary={} authoritative repository skill problem(s); {} optional warning(s)",
            problems.len(),
            warnings.len()
        );
        println!("active_skills={}", active_skills.len());
        println!("mapped_memories={}", core.len());
        println!("in_sync={ok}");
        println!("problems={}", problems.len());
        println!("warnings={}", warnings.len());
        if !problems.is_empty() {
            println!(
                "detail={}",
                problems
                    .iter()
                    .map(|problem| problem.lines().collect::<Vec<_>>().join(" "))
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        }
        std::process::exit(if problems.is_empty() { 0 } else { 1 });
    }

    println!();
    println!(
        "active skills: {}  mapped memories: {}  in-sync: {}  problems: {}  warnings: {}",
        active_skills.len(),
        core.len(),
        ok,
        problems.len(),
        warnings.len()
    );
    if !quiet {
        for warning in &warnings {
            println!("WARN  {warning}");
        }
    }
    if problems.is_empty() {
        println!("RESULT: PASS — repository skills are authoritative and structurally valid.");
        std::process::exit(0);
    }
    println!("RESULT: FAIL");
    for p in &problems {
        println!("  {p}");
    }
    std::process::exit(1);
}

// ---- shared extraction/normalization (kept identical in sync-memory-skill.rs) ----

struct Meta {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    description: String,
    core_memory: bool,
    core_skill: Option<String>,
}

fn parse_meta(content: &str) -> Meta {
    let mut m = Meta {
        name: String::new(),
        description: String::new(),
        core_memory: false,
        core_skill: None,
    };
    if !content.starts_with("---") {
        return m;
    }
    for line in content.lines().skip(1) {
        if line.trim() == "---" {
            break;
        }
        let t = line.trim();
        if let Some(v) = t.strip_prefix("name:") {
            if m.name.is_empty() {
                m.name = unquote(v.trim());
            }
        } else if let Some(v) = t.strip_prefix("description:") {
            if m.description.is_empty() {
                m.description = unquote(v.trim());
            }
        } else if let Some(v) = t.strip_prefix("core_memory:") {
            m.core_memory = v.trim() == "true";
        } else if let Some(v) = t.strip_prefix("core_skill:") {
            m.core_skill = Some(unquote(v.trim()));
        }
    }
    m
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    s.to_string()
}

/// Everything after the leading YAML frontmatter block.
fn strip_frontmatter(content: &str) -> &str {
    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(idx) = rest.find("\n---\n") {
            return &rest[idx + 5..];
        }
        if let Some(idx) = rest.find("\n---") {
            return &rest[idx + 4..];
        }
    }
    content
}

/// Canonical, comparable form of a memory body: frontmatter removed, the visible
/// CORE-MEMORY marker line dropped, full-line HTML comments dropped, whitespace
/// normalized.
fn canonical_body(content: &str) -> String {
    let body = strip_frontmatter(content);
    let mut lines: Vec<String> = Vec::new();
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with("> **CORE-MEMORY**") {
            continue;
        }
        let tl = line.trim();
        if tl.starts_with("<!--") && tl.ends_with("-->") {
            continue;
        }
        lines.push(line.trim_end().to_string());
    }
    normalize_lines(lines)
}

/// Collapse any run of blank lines to a single blank; strip leading/trailing blanks.
fn normalize_lines(lines: Vec<String>) -> String {
    let mut res: Vec<String> = Vec::new();
    let mut pending_blank = false;
    for l in lines {
        if l.trim().is_empty() {
            if !res.is_empty() {
                pending_blank = true;
            }
        } else {
            if pending_blank {
                res.push(String::new());
                pending_blank = false;
            }
            res.push(l);
        }
    }
    res.join("\n")
}

// ---- filesystem helpers ----

fn package_skill_rel(slug: &str) -> String {
    format!("{SKILL_DIR}/{slug}/SKILL.md")
}

fn memory_dir(root: &Path) -> PathBuf {
    match std::env::var("HERMIT_MEMORY_DIR") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    eprintln!("HOME is not set (set HERMIT_MEMORY_DIR)");
                    std::process::exit(2);
                });
            let project_key: String = root
                .to_string_lossy()
                .chars()
                .map(|ch| if ch == '/' || ch == '\\' { '-' } else { ch })
                .collect();
            home.join(".claude/projects")
                .join(project_key)
                .join("memory")
        }
    }
}

fn read_md_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("md") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn read_skill_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            continue;
        }
        if !path.is_dir() {
            continue;
        }

        let folder_skill = path.join("SKILL.md");
        if folder_skill.is_file() {
            out.push(folder_skill);
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("core-memory") {
            out.extend(read_md_files(&path).into_iter().filter(|entry| {
                entry.file_name().and_then(|name| name.to_str()) != Some("README.md")
            }));
        }
    }
    out.sort();
    out
}

/// The link text of a symlink that points into the agent-utils skills tree but does
/// not resolve, or `None` when this is not that case.
///
/// Reads the LINK TEXT rather than canonicalising, because canonicalize fails on a
/// dangling symlink -- which is exactly the state being diagnosed here, and is why
/// `is_external_skill_bridge_dir` cannot distinguish "absent submodule" from
/// "nonsense entry" on its own. This classifies the failure; it never excuses it.
fn absent_submodule_bridge_target(bridge: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(bridge).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    if std::fs::canonicalize(bridge).is_ok() {
        return None; // resolves; not the absent-submodule case
    }
    let target = std::fs::read_link(bridge).ok()?;
    let text = target.to_string_lossy().to_string();
    text.contains("agent-utils/skills").then_some(text)
}

fn is_external_skill_bridge_dir(root: &Path, bridge: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(bridge) else {
        return false;
    };
    if !meta.file_type().is_symlink() {
        return false;
    }
    let Some(name) = bridge.file_name() else {
        return false;
    };
    let Ok(target) = std::fs::canonicalize(bridge) else {
        return false;
    };
    let Ok(canonical_root) = std::fs::canonicalize(root.join("agent-utils/skills")) else {
        return false;
    };
    target == canonical_root.join(name) && target.join("SKILL.md").is_file()
}

fn is_external_skill_bridge_file(root: &Path, skill: &Path) -> bool {
    skill.file_name().and_then(|name| name.to_str()) == Some("SKILL.md")
        && skill
            .parent()
            .is_some_and(|parent| is_external_skill_bridge_dir(root, parent))
}
fn stem(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

fn skill_slug(path: &Path) -> String {
    if path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
        return path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
    }
    stem(path)
}

fn find_root() -> PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if dir.join(".gitmodules").is_file()
            && dir.join("hermit").is_dir()
            && dir.join("reverie").is_dir()
        {
            return dir;
        }
        if !dir.pop() {
            eprintln!("could not locate dev-hermit root (need .gitmodules + hermit/ + reverie/)");
            std::process::exit(2);
        }
    }
}
