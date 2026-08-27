#!/usr/bin/env rust-script
//! Inventory executable Rust files and source files that invoke `rust-script`.
//!
//! The scan covers tracked plus nonignored-untracked files in the dev-hermit
//! parent and its four canonical inner repositories. Worktree replicas, build
//! output, and ignored runtime data are therefore excluded by construction.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

const REPOSITORIES: &[&str] = &[".", "hermit", "reverie", "liteinst2", "agent-utils"];
const USAGE: &str = r#"Usage: scripts/list-rust-scripts.rs

Inventory executable Rust files and source files that invoke rust-script across
the dev-hermit parent and its canonical inner repositories.

Options:
  -h, --help  Print this help and exit
"#;

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    executable_rs: bool,
    rust_script: bool,
    description: String,
}

fn main() {
    if std::env::args_os()
        .skip(1)
        .any(|argument| argument == "-h" || argument == "--help")
    {
        print!("{USAGE}");
        return;
    }
    if let Err(error) = run() {
        eprintln!("list-rust-scripts: {error}");
        exit(2);
    }
}

fn run() -> Result<(), String> {
    let root = workspace_root()?;
    let files = repository_files(&root)?;
    let mut entries = Vec::new();

    for relative in files {
        let absolute = root.join(&relative);
        let metadata = match fs::metadata(&absolute) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => continue,
            Err(error) => return Err(format!("cannot stat {}: {error}", relative.display())),
        };
        let executable_rs = relative.extension().is_some_and(|ext| ext == "rs")
            && metadata.permissions().mode() & 0o111 != 0;
        let bytes = fs::read(&absolute)
            .map_err(|error| format!("cannot read {}: {error}", relative.display()))?;
        let content = String::from_utf8_lossy(&bytes);
        let rust_script = invokes_rust_script(&relative, &content, metadata.permissions().mode());
        if executable_rs || rust_script {
            entries.push(Entry {
                description: header_description(&content),
                path: relative,
                executable_rs,
                rust_script,
            });
        }
    }

    entries.sort_by(|left, right| left.path.cmp(&right.path));
    print_inventory(&entries);
    Ok(())
}

fn workspace_root() -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| format!("cannot run git rev-parse: {error}"))?;
    if !output.status.success() {
        return Err("current directory is not inside dev-hermit".to_string());
    }
    let root = String::from_utf8(output.stdout)
        .map_err(|error| format!("git returned a non-UTF-8 workspace path: {error}"))?;
    Ok(PathBuf::from(root.trim()))
}

fn repository_files(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut files = BTreeSet::new();
    for repository in REPOSITORIES {
        let repository_root = root.join(repository);
        if !repository_root.join(".git").exists() {
            return Err(format!(
                "canonical repository {} is not initialized",
                repository_root.display()
            ));
        }
        let output = Command::new("git")
            .current_dir(&repository_root)
            .args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .output()
            .map_err(|error| format!("cannot enumerate {repository}: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "git ls-files failed in {repository}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        for raw in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path = String::from_utf8(raw.to_vec())
                .map_err(|error| format!("non-UTF-8 path in {repository}: {error}"))?;
            let relative = if *repository == "." {
                PathBuf::from(path)
            } else {
                Path::new(repository).join(path)
            };
            files.insert(relative);
        }
    }
    Ok(files)
}

fn invokes_rust_script(path: &Path, content: &str, mode: u32) -> bool {
    if content
        .lines()
        .next()
        .is_some_and(|line| line.starts_with("#!") && line.contains("rust-script"))
    {
        return true;
    }

    if !is_executable_source(path, mode) {
        return false;
    }
    content.lines().any(line_invokes_rust_script)
}

fn is_executable_source(path: &Path, mode: u32) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let extension = path.extension().and_then(|extension| extension.to_str());
    mode & 0o111 != 0
        || matches!(name, "Makefile" | "makefile" | "GNUmakefile")
        || matches!(extension, Some("mk" | "sh" | "bash" | "yml" | "yaml"))
}

fn line_invokes_rust_script(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with("//") {
        return false;
    }
    for segment in trimmed.split([';', '|']) {
        let mut words = segment
            .trim_start_matches([' ', '\t', '-', '@', '+'])
            .split_whitespace()
            .peekable();
        while words
            .peek()
            .is_some_and(|word| word.contains('=') && !word.starts_with('='))
        {
            words.next();
        }
        while matches!(
            words.peek().copied(),
            Some("env" | "exec" | "nohup" | "with-proxy")
        ) {
            words.next();
        }
        if words.next() == Some("rust-script") {
            return true;
        }
    }
    false
}

fn header_description(content: &str) -> String {
    let mut paragraph = Vec::new();
    let mut cargo_fence = false;
    let mut in_block_comment = false;

    for line in content.lines().skip(1).take(160) {
        let trimmed = line.trim();
        if trimmed.starts_with("/*") {
            in_block_comment = true;
        }
        let comment = if let Some(text) = trimmed.strip_prefix("//!") {
            Some(text.trim())
        } else if let Some(text) = trimmed.strip_prefix("///") {
            Some(text.trim())
        } else if in_block_comment && trimmed != "*/" {
            Some(
                trimmed
                    .trim_start_matches("/*")
                    .trim_start_matches('*')
                    .trim_end_matches("*/")
                    .trim(),
            )
        } else {
            None
        };
        if trimmed.ends_with("*/") {
            in_block_comment = false;
        }

        let Some(comment) = comment else {
            if !trimmed.is_empty() && !paragraph.is_empty() {
                break;
            }
            continue;
        };
        if comment.starts_with("```cargo") {
            cargo_fence = true;
            continue;
        }
        if cargo_fence {
            if comment == "```" {
                cargo_fence = false;
            }
            continue;
        }
        if is_boilerplate(comment) {
            continue;
        }
        if comment.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        paragraph.push(comment.to_string());
    }

    if paragraph.is_empty() {
        "DESCRIPTION MISSING: no descriptive header comment".to_string()
    } else {
        paragraph.join(" ")
    }
}

fn is_boilerplate(line: &str) -> bool {
    let lowercase = line.to_ascii_lowercase();
    lowercase.contains("copyright")
        || lowercase == "all rights reserved."
        || lowercase.starts_with("this source code is licensed")
        || lowercase.starts_with("license file in")
}

fn print_inventory(entries: &[Entry]) {
    println!("Rust-script inventory (tracked + nonignored-untracked source files)");
    println!("scope: dev-hermit parent, hermit, reverie, liteinst2, agent-utils");
    for entry in entries {
        let state = match (entry.executable_rs, entry.rust_script) {
            (true, true) => "OK",
            (true, false) => "EXECUTABLE-BUT-NOT-RUST-SCRIPT",
            (false, true) => "RUST-SCRIPT-BUT-NOT-EXECUTABLE",
            (false, false) => unreachable!(),
        };
        println!("{state}\t{}\t{}", entry.path.display(), entry.description);
    }

    let executable = entries.iter().filter(|entry| entry.executable_rs).count();
    let rust_script = entries.iter().filter(|entry| entry.rust_script).count();
    let executable_only: Vec<_> = entries
        .iter()
        .filter(|entry| entry.executable_rs && !entry.rust_script)
        .collect();
    let rust_script_only: Vec<_> = entries
        .iter()
        .filter(|entry| entry.rust_script && !entry.executable_rs)
        .collect();

    println!(
        "COUNTS\texecutable_rs={executable}\trust_script={rust_script}\tunion={}",
        entries.len()
    );
    println!(
        "MISMATCH_COUNTS\texecutable_but_not_rust_script={}\trust_script_but_not_executable={}",
        executable_only.len(),
        rust_script_only.len()
    );
}
