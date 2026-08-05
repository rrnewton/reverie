#!/usr/bin/env rust-script
/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */
//! Verify that Claude and stock Codex discover the same Reverie product skills.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const ROOT_SKILLS: &[&str] = &[
    "adding-a-backend",
    "repo-cleanliness",
    "reverie-architecture",
    "syscall-interception",
    "testing-tools",
];

const LITEINST_SKILLS: &[&str] = &[
    "liteinst-binary-instrumentation",
    "liteinst-testing",
    "liteinst-tool-lifecycle",
];

fn git_root() -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| format!("could not run git rev-parse: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

fn require_symlink(path: &Path, expected: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.file_type().is_symlink() {
        return Err(format!("{} must be a symlink", path.display()));
    }
    let actual =
        fs::read_link(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if actual != expected {
        return Err(format!(
            "{} points to {:?}, expected {:?}",
            path.display(),
            actual,
            expected
        ));
    }
    Ok(())
}

fn frontmatter<'a>(contents: &'a str, path: &Path) -> Result<&'a str, String> {
    let rest = contents
        .strip_prefix("---\n")
        .ok_or_else(|| format!("{} lacks YAML frontmatter", path.display()))?;
    let closing = rest
        .find("\n---\n")
        .ok_or_else(|| format!("{} has unterminated YAML frontmatter", path.display()))?;
    Ok(&contents[..4 + closing + 5])
}

fn checked_frontmatter<'a>(
    contents: &'a str,
    path: &Path,
    expected_name: &str,
) -> Result<&'a str, String> {
    let metadata = frontmatter(contents, path)?;
    let name = metadata
        .lines()
        .find_map(|line| line.strip_prefix("name:"))
        .map(str::trim)
        .ok_or_else(|| format!("{} frontmatter lacks name", path.display()))?;
    if name != expected_name {
        return Err(format!(
            "{} declares name {:?}, expected {:?}",
            path.display(),
            name,
            expected_name
        ));
    }
    let description = metadata
        .lines()
        .find_map(|line| line.strip_prefix("description:"))
        .map(str::trim)
        .ok_or_else(|| format!("{} frontmatter lacks description", path.display()))?;
    if description.is_empty() || description == "\"\"" || description == "''" {
        return Err(format!(
            "{} frontmatter has an empty description",
            path.display()
        ));
    }
    Ok(metadata)
}

fn expected_wrapper(
    name: &str,
    canonical: &str,
    canonical_path: &Path,
    relative_target: &str,
) -> Result<String, String> {
    let metadata = checked_frontmatter(canonical, canonical_path, name)?;
    Ok(format!(
        "{metadata}\n# Codex discovery entrypoint\n\n\
         Read and follow [the canonical `{name}` skill]({relative_target}) completely. \
         Resolve further relative links from the canonical file's directory.\n"
    ))
}

fn entry_names(path: &Path) -> Result<BTreeSet<String>, String> {
    fs::read_dir(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?
        .map(|entry| {
            let entry = entry.map_err(|error| format!("cannot read directory entry: {error}"))?;
            entry
                .file_name()
                .into_string()
                .map_err(|name| format!("non-UTF-8 skill entry: {name:?}"))
        })
        .collect()
}

fn expected_names(skills: &[&str], suffix: &str, readme: bool) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = skills
        .iter()
        .map(|name| format!("{name}{suffix}"))
        .collect();
    if readme {
        names.insert("README.md".to_owned());
    }
    names
}

fn check_group(
    canonical_root: &Path,
    codex_root: &Path,
    skills: &[&str],
    target_root: &str,
) -> Result<(), String> {
    let canonical_metadata = fs::symlink_metadata(canonical_root)
        .map_err(|error| format!("cannot inspect {}: {error}", canonical_root.display()))?;
    if !canonical_metadata.is_dir() || canonical_metadata.file_type().is_symlink() {
        return Err(format!(
            "{} must be a real canonical directory",
            canonical_root.display()
        ));
    }
    let codex_metadata = fs::symlink_metadata(codex_root)
        .map_err(|error| format!("cannot inspect {}: {error}", codex_root.display()))?;
    if !codex_metadata.is_dir() || codex_metadata.file_type().is_symlink() {
        return Err(format!("{} must be a real directory", codex_root.display()));
    }

    let actual_canonical = entry_names(canonical_root)?;
    let expected_canonical = expected_names(skills, ".md", false);
    if actual_canonical != expected_canonical {
        return Err(format!(
            "canonical entries differ in {}:\n  actual: {actual_canonical:?}\n  expected: {expected_canonical:?}",
            canonical_root.display()
        ));
    }

    let actual_codex = entry_names(codex_root)?;
    let expected_codex = expected_names(skills, "", true);
    if actual_codex != expected_codex {
        return Err(format!(
            "Codex entries differ in {}:\n  actual: {actual_codex:?}\n  expected: {expected_codex:?}",
            codex_root.display()
        ));
    }

    for name in skills {
        let canonical_path = canonical_root.join(format!("{name}.md"));
        let canonical_file_metadata = fs::symlink_metadata(&canonical_path)
            .map_err(|error| format!("cannot inspect {}: {error}", canonical_path.display()))?;
        if !canonical_file_metadata.is_file() || canonical_file_metadata.file_type().is_symlink() {
            return Err(format!(
                "{} must be a regular canonical file",
                canonical_path.display()
            ));
        }
        let canonical = fs::read_to_string(&canonical_path)
            .map_err(|error| format!("cannot read {}: {error}", canonical_path.display()))?;
        let wrapper_dir = codex_root.join(name);
        let wrapper_metadata = fs::symlink_metadata(&wrapper_dir)
            .map_err(|error| format!("cannot inspect {}: {error}", wrapper_dir.display()))?;
        if !wrapper_metadata.is_dir() || wrapper_metadata.file_type().is_symlink() {
            return Err(format!(
                "{} must be a real directory",
                wrapper_dir.display()
            ));
        }
        if entry_names(&wrapper_dir)? != BTreeSet::from(["SKILL.md".to_owned()]) {
            return Err(format!(
                "{} must contain only SKILL.md",
                wrapper_dir.display()
            ));
        }
        let wrapper_path = wrapper_dir.join("SKILL.md");
        let wrapper_file_metadata = fs::symlink_metadata(&wrapper_path)
            .map_err(|error| format!("cannot inspect {}: {error}", wrapper_path.display()))?;
        if !wrapper_file_metadata.is_file() || wrapper_file_metadata.file_type().is_symlink() {
            return Err(format!("{} must be a regular file", wrapper_path.display()));
        }
        let wrapper = fs::read_to_string(&wrapper_path)
            .map_err(|error| format!("cannot read {}: {error}", wrapper_path.display()))?;
        let relative_target = format!("{target_root}/{name}.md");
        let expected = expected_wrapper(name, &canonical, &canonical_path, &relative_target)?;
        if wrapper != expected {
            return Err(format!(
                "{} is stale; regenerate it from {}",
                wrapper_path.display(),
                canonical_path.display()
            ));
        }
    }

    Ok(())
}

fn check(root: &Path) -> Result<(), String> {
    require_symlink(&root.join("CLAUDE.md"), Path::new("AGENTS.md"))?;
    require_symlink(&root.join(".llms/skills"), Path::new("../.claude/skills"))?;
    check_group(
        &root.join(".claude/skills"),
        &root.join(".agents/skills"),
        ROOT_SKILLS,
        "../../../.claude/skills",
    )?;

    let liteinst = root.join("reverie-liteinst");
    require_symlink(&liteinst.join("CLAUDE.md"), Path::new("AGENTS.md"))?;
    require_symlink(
        &liteinst.join(".claude/skills"),
        Path::new("../.llms/skills"),
    )?;
    check_group(
        &liteinst.join(".llms/skills"),
        &liteinst.join(".agents/skills"),
        LITEINST_SKILLS,
        "../../../.llms/skills",
    )?;
    Ok(())
}

fn main() {
    let root = match env::args().nth(1) {
        Some(path) => PathBuf::from(path),
        None => match git_root() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("check-skill-discovery: ERROR: {error}");
                std::process::exit(1);
            }
        },
    };
    if let Err(error) = check(&root) {
        eprintln!("check-skill-discovery: ERROR: {error}");
        std::process::exit(1);
    }
    println!(
        "check-skill-discovery: PASS ({} root adapters, {} LiteInst adapters)",
        ROOT_SKILLS.len(),
        LITEINST_SKILLS.len()
    );
}
