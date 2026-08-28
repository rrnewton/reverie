#!/usr/bin/env rust-script
//! Verify that worktree-state.json and ACTIVE.md describe the checked-out branches.
//!
//! ```cargo
//! [dependencies]
//! serde = { version = "1", features = ["derive"] }
//! serde_json = "1"
//! ```

#[path = "worktree_registry.rs"]
mod worktree_registry;

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

const BEGIN: &str =
    "<!-- BEGIN worktree-state (managed by scripts/allocate-worktree.rs; do not edit inside) -->";
const END: &str = "<!-- END worktree-state -->";

const USAGE: &str = r#"Usage: check-worktree-registry.rs [--root PATH] [SCOPE]
       check-worktree-registry.rs --validate-state-stdin
       check-worktree-registry.rs --assigned-slot AGENT

Scope (mutually exclusive):
  --slot SLOT          Verify exactly one named slot.
  --exclude-slot SLOT  Verify every slot except this explicitly named slot.
                       May be repeated; every exclusion is reported.

Fail unless every worktree-state.json branch agrees with the actual checkout and
the managed ACTIVE.md row agrees with worktree-state.json. Detached expectations
may be `detached` (any detached SHA) or `detached:<sha-prefix>` (exact prefix).

--validate-state-stdin validates one exact JSON document from standard input
against the shared schema without inspecting worktrees or ACTIVE.md.

--assigned-slot reads that document from standard input and prints the one slot
the allocator's existing ownership rule assigns to AGENT. No match is success
with no output; multiple matches are a refusal rather than an arbitrary choice.
"#;

#[derive(Debug)]
enum Actual {
    Absent,
    Branch(String),
    Detached(String),
    Unreadable(String),
}

fn die(message: &str) -> ! {
    eprintln!("worktree-registry: ERROR {message}");
    exit(2);
}

fn find_root() -> PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if dir.join(".gitmodules").is_file() && dir.join("worktrees").is_dir() {
            return dir;
        }
        if !dir.pop() {
            die("could not locate dev-hermit root; pass --root PATH");
        }
    }
}

fn git(path: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new("git").arg("-C").arg(path).args(args).output();
    match output {
        Ok(output) => (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ),
        Err(error) => (false, String::new(), error.to_string()),
    }
}

fn actual_checkout(path: &Path) -> Actual {
    if !path.exists() {
        return Actual::Absent;
    }
    let requested = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) => {
            return Actual::Unreadable(format!("canonicalize requested checkout: {error}"))
        }
    };
    let (has_top, top, error) = git(
        path,
        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
    );
    if !has_top || top.is_empty() {
        return Actual::Unreadable(if error.is_empty() {
            "not a git worktree".to_string()
        } else {
            error
        });
    }
    let observed = match std::fs::canonicalize(&top) {
        Ok(path) => path,
        Err(error) => return Actual::Unreadable(format!("canonicalize git top-level: {error}")),
    };
    if observed != requested {
        return Actual::Unreadable(format!(
            "git top-level {} does not equal requested checkout {}",
            observed.display(),
            requested.display()
        ));
    }
    let (on_branch, branch, _) = git(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    if on_branch && !branch.is_empty() {
        return Actual::Branch(branch);
    }
    let (has_head, head, error) = git(path, &["rev-parse", "HEAD"]);
    if has_head && !head.is_empty() {
        Actual::Detached(head)
    } else {
        Actual::Unreadable(error)
    }
}

fn actual_display(actual: &Actual) -> String {
    match actual {
        Actual::Absent => "-".to_string(),
        Actual::Branch(branch) => branch.clone(),
        Actual::Detached(head) => format!("detached:{}", &head[..head.len().min(12)]),
        Actual::Unreadable(reason) => format!("unreadable:{reason}"),
    }
}

fn matches_expected(expected: &str, actual: &Actual) -> bool {
    if expected == "-" {
        return matches!(actual, Actual::Absent);
    }
    if expected == "detached" {
        return matches!(actual, Actual::Detached(_));
    }
    if let Some(prefix) = expected.strip_prefix("detached:") {
        return matches!(actual, Actual::Detached(head) if head.starts_with(prefix));
    }
    matches!(actual, Actual::Branch(branch) if branch == expected)
}

fn validate_owners(slot: &str, state: &Value) -> Result<(), String> {
    let agents = state["agents"]
        .as_array()
        .ok_or_else(|| "agents is not an array".to_string())?;
    if agents.is_empty() {
        return Err("agents is empty".to_string());
    }
    let mut names = BTreeSet::new();
    let mut mutating = 0usize;
    for (index, agent) in agents.iter().enumerate() {
        let name = agent["name"]
            .as_str()
            .ok_or_else(|| format!("agent {index} has no string name"))?;
        if name.is_empty()
            || name.trim() != name
            || name == "-"
            || name.contains('|')
            || name.chars().any(char::is_control)
        {
            return Err(format!("agent {index} has invalid name '{name}'"));
        }
        if !names.insert(name.to_string()) {
            return Err(format!("duplicate agent '{name}'"));
        }
        let read_only = agent["read_only"]
            .as_bool()
            .ok_or_else(|| format!("agent '{name}' has no boolean read_only"))?;
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

fn active_rows(path: &Path) -> BTreeMap<String, Vec<String>> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| die(&format!("read {}: {error}", path.display())));
    let begin = text
        .find(BEGIN)
        .unwrap_or_else(|| die(&format!("{} lacks managed BEGIN marker", path.display())));
    let after_begin = begin + BEGIN.len();
    let end = text[after_begin..]
        .find(END)
        .map(|offset| after_begin + offset)
        .unwrap_or_else(|| die(&format!("{} lacks managed END marker", path.display())));

    let mut rows = BTreeMap::new();
    for line in text[after_begin..end].lines() {
        if !line.starts_with('|') || line.starts_with("| ---") || line.starts_with("| Slot ") {
            continue;
        }
        let cells: Vec<String> = line
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().to_string())
            .collect();
        if cells.len() != 8 {
            die(&format!("malformed ACTIVE.md managed row: {line}"));
        }
        let slot = cells[0].clone();
        if rows.insert(slot.clone(), cells).is_some() {
            die(&format!("duplicate ACTIVE.md managed row for slot {slot}"));
        }
    }
    rows
}

fn state_row(slot: &str, state: &Value) -> Vec<String> {
    let agents = state["agents"].as_array().cloned().unwrap_or_default();
    let owner = agents
        .iter()
        .find(|agent| !agent["read_only"].as_bool().unwrap_or(false))
        .and_then(|agent| agent["name"].as_str())
        .unwrap_or("-");
    let read_only: Vec<&str> = agents
        .iter()
        .filter(|agent| agent["read_only"].as_bool().unwrap_or(false))
        .filter_map(|agent| agent["name"].as_str())
        .collect();
    let agent_cell = if read_only.is_empty() {
        owner.to_string()
    } else {
        format!("{owner} (+ro: {})", read_only.join(", "))
    };
    vec![
        slot.to_string(),
        agent_cell,
        state["hermit_branch"].as_str().unwrap_or("-").to_string(),
        state["reverie_branch"].as_str().unwrap_or("-").to_string(),
        state["liteinst2_branch"]
            .as_str()
            .unwrap_or("-")
            .to_string(),
        state["task"].as_str().unwrap_or("-").to_string(),
        state["status"].as_str().unwrap_or("active").to_string(),
        if read_only.is_empty() { "no" } else { "shared" }.to_string(),
    ]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut root = None;
    let mut only_slot = None;
    let mut excluded_slots = BTreeSet::new();
    let mut validate_state_stdin = false;
    let mut assigned_slot_agent = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                if i >= args.len() {
                    die("--root requires a path");
                }
                root = Some(PathBuf::from(&args[i]));
            }
            "--slot" => {
                i += 1;
                if i >= args.len() {
                    die("--slot requires a value");
                }
                only_slot = Some(args[i].clone());
            }
            "--exclude-slot" => {
                i += 1;
                if i >= args.len() {
                    die("--exclude-slot requires a value");
                }
                excluded_slots.insert(args[i].clone());
            }
            "--validate-state-stdin" => validate_state_stdin = true,
            "--assigned-slot" => {
                i += 1;
                if i >= args.len() {
                    die("--assigned-slot requires an agent name");
                }
                assigned_slot_agent = Some(args[i].clone());
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            other => die(&format!("unknown argument {other}\n\n{USAGE}")),
        }
        i += 1;
    }
    if validate_state_stdin || assigned_slot_agent.is_some() {
        if root.is_some() || only_slot.is_some() || !excluded_slots.is_empty() {
            die("stdin registry modes are incompatible with root and scope arguments");
        }
        if validate_state_stdin && assigned_slot_agent.is_some() {
            die("--validate-state-stdin and --assigned-slot are mutually exclusive");
        }
        let mut bytes = Vec::new();
        io::stdin()
            .read_to_end(&mut bytes)
            .unwrap_or_else(|error| die(&format!("read stdin: {error}")));
        match worktree_registry::Registry::parse(&bytes) {
            Ok(registry) => {
                if let Some(agent) = assigned_slot_agent {
                    let slots = registry.assigned_slots(&agent);
                    match slots.as_slice() {
                        [] => {}
                        [slot] => println!("{slot}"),
                        _ => {
                            eprintln!(
                                "worktree-registry: REFUSE agent {agent:?} has multiple current slot assignments: {}",
                                slots.join(",")
                            );
                            exit(1);
                        }
                    }
                }
                return;
            }
            Err(error) => {
                eprintln!("worktree-registry: REFUSE {error}");
                exit(1);
            }
        }
    }
    if only_slot.is_some() && !excluded_slots.is_empty() {
        die("--slot and --exclude-slot are mutually exclusive");
    }
    if let Some(slot) = &only_slot {
        if !worktree_registry::valid_slot_name(slot) {
            die(&format!("invalid --slot value {slot:?}"));
        }
    }
    for slot in &excluded_slots {
        if !worktree_registry::valid_slot_name(slot) {
            die(&format!("invalid --exclude-slot value {slot:?}"));
        }
    }
    let root = root.unwrap_or_else(find_root);
    let state_path = root.join("worktree-state.json");
    let state_text = std::fs::read_to_string(&state_path)
        .unwrap_or_else(|error| die(&format!("read {}: {error}", state_path.display())));
    worktree_registry::Registry::parse(state_text.as_bytes())
        .unwrap_or_else(|error| die(&format!("validate {}: {error}", state_path.display())));
    let state: Value = serde_json::from_str(&state_text)
        .unwrap_or_else(|error| die(&format!("parse {}: {error}", state_path.display())));
    let slots = state["slots"]
        .as_object()
        .unwrap_or_else(|| die("worktree-state.json lacks object field 'slots'"));
    let active = active_rows(&root.join("worktrees/ACTIVE.md"));

    if let Some(slot) = &only_slot {
        if !slots.contains_key(slot) && !active.contains_key(slot) {
            die(&format!("scoped slot {slot:?} exists in neither registry"));
        }
        println!("worktree-registry: SCOPE slot={slot}");
    }
    for slot in &excluded_slots {
        if !slots.contains_key(slot) && !active.contains_key(slot) {
            die(&format!(
                "excluded slot {slot:?} exists in neither registry"
            ));
        }
    }
    if !excluded_slots.is_empty() {
        println!(
            "worktree-registry: SCOPE excluded={} (not inspected)",
            excluded_slots.iter().cloned().collect::<Vec<_>>().join(",")
        );
    }

    let in_scope = |slot: &str| {
        only_slot.as_deref().map_or(true, |only| only == slot) && !excluded_slots.contains(slot)
    };

    let mut issues: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut drift_cells = 0usize;
    for (slot, entry) in slots {
        if !in_scope(slot) {
            continue;
        }
        if let Err(error) = validate_owners(slot, entry) {
            issues
                .entry(slot.clone())
                .or_default()
                .push(format!("OWNERS {error}"));
        }
        match active.get(slot) {
            Some(row) if row == &state_row(slot, entry) => {}
            Some(row) => issues.entry(slot.clone()).or_default().push(format!(
                "ACTIVE_ROW recorded={row:?} state={:?}",
                state_row(slot, entry)
            )),
            None => issues
                .entry(slot.clone())
                .or_default()
                .push("ACTIVE_ROW missing".to_string()),
        }

        for product in ["hermit", "reverie", "liteinst2"] {
            let expected = entry[format!("{product}_branch")].as_str().unwrap_or("-");
            let expected_relative = format!("worktrees/slots/{slot}/{product}");
            let recorded_path = entry[format!("{product}_path")].as_str();
            if recorded_path != Some(expected_relative.as_str()) {
                drift_cells += 1;
                issues.entry(slot.clone()).or_default().push(format!(
                    "{product} path recorded={recorded_path:?} expected={expected_relative}"
                ));
                continue;
            }
            let actual = actual_checkout(&root.join(PathBuf::from(expected_relative)));
            if !matches_expected(expected, &actual) {
                drift_cells += 1;
                issues.entry(slot.clone()).or_default().push(format!(
                    "{product} recorded={expected} actual={}",
                    actual_display(&actual)
                ));
            }
        }
    }

    let state_names: BTreeSet<&String> = slots.keys().collect();
    for slot in active.keys() {
        if in_scope(slot) && !state_names.contains(slot) {
            issues
                .entry(slot.clone())
                .or_default()
                .push("ACTIVE_ROW has no worktree-state entry".to_string());
        }
    }

    for (slot, slot_issues) in &issues {
        for issue in slot_issues {
            eprintln!("DRIFT slot={slot} {issue}");
        }
    }
    let rows = state_names
        .iter()
        .filter(|name| in_scope(name.as_str()))
        .map(|name| name.as_str())
        .chain(
            active
                .keys()
                .filter(|name| in_scope(name.as_str()))
                .map(|name| name.as_str()),
        )
        .collect::<BTreeSet<_>>()
        .len();
    let drift_rows = issues.len();
    let correct_rows = rows.saturating_sub(drift_rows);
    let product_cells = rows * 3;
    if issues.is_empty() {
        println!(
            "worktree-registry: PASS rows={rows} correct_rows={correct_rows} drift_rows=0 product_cells={product_cells} drift_cells=0"
        );
    } else {
        eprintln!(
            "worktree-registry: FAIL rows={rows} correct_rows={correct_rows} drift_rows={drift_rows} product_cells={product_cells} drift_cells={drift_cells}"
        );
        exit(1);
    }
}
