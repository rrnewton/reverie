/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Executable neutrality proof: this crate is exercised stand-alone by a
//! strace-style observer that only records what the domain decides, and
//! its sources are scanned for consumer/engine vocabulary. The scan is
//! itself tested against a deliberately contaminated copy.

use reverie_signal::frame::{SUPPORTED_XFEATURES, XstateDesc};
use reverie_signal::*;

/// Consumer and engine-policy vocabulary that must never appear in this
/// crate's Rust sources. Assembled from fragments so this test file
/// itself names no consumer.
fn forbidden() -> Vec<String> {
    [
        ("De", "tcore"),
        ("det", "core"),
        ("Her", "mit"),
        ("herm", "it_"),
        ("determin", "istic"),
        ("determin", "ism"),
        ("virtual ", "time"),
        ("virtual-", "time"),
        ("turn ", "holder"),
        ("R", "CB"),
    ]
    .iter()
    .map(|(a, b)| format!("{a}{b}"))
    .collect()
}

fn scan(text: &str) -> Vec<String> {
    forbidden()
        .into_iter()
        .filter(|w| text.contains(w.as_str()))
        .collect()
}

fn scan_tree(dir: &std::path::Path) -> (usize, Vec<String>) {
    let mut scanned = 0usize;
    let mut hits = Vec::new();
    for name in ["lib.rs", "frame.rs"] {
        let text = std::fs::read_to_string(dir.join(name)).unwrap();
        scanned += text.len();
        hits.extend(scan(&text));
    }
    (scanned, hits)
}

/// Resolve the workspace graph from the root manifest's member list
/// (including nested members) and return incoming edges on this crate:
/// direct path/name dependencies and root workspace aliases consumed
/// via `alias.workspace = true`.
fn incoming_edges(root: &std::path::Path) -> (usize, Vec<String>) {
    let root_text = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    // Members: quoted strings inside the members array.
    let members: Vec<String> = root_text
        .split("members = [")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap()
        .split(',')
        .filter_map(|s| {
            let s = s.trim().trim_matches(|c| c == '"' || c == '\'');
            (!s.is_empty()).then(|| s.to_owned())
        })
        .collect();
    // Root aliases mapping to this package.
    let mut aliases: Vec<String> = Vec::new();
    if let Some(deps) = root_text.split("[workspace.dependencies]").nth(1) {
        for line in deps.lines() {
            if line.contains("reverie-signal")
                && let Some(name) = line.split('=').next()
            {
                aliases.push(name.trim().to_owned());
            }
        }
    }
    // Table-form aliases: [workspace.dependencies.<name>] ... package =
    {
        let mut current: Option<String> = None;
        for line in root_text.lines() {
            let t = line.trim();
            if let Some(h) = t.strip_prefix("[workspace.dependencies.") {
                current = Some(h.trim_end_matches(']').trim().to_owned());
                continue;
            }
            if t.starts_with('[') {
                current = None;
            }
            if let Some(name) = &current
                && t.contains("reverie-signal")
                && !aliases.contains(name)
            {
                aliases.push(name.clone());
            }
        }
    }
    let mut edges = Vec::new();
    for m in &aliases {
        edges.push(format!("root workspace alias {m}"));
    }
    let mut checked = 0usize;
    for member in &members {
        if member == "reverie-signal" {
            continue;
        }
        let manifest = root.join(member).join("Cargo.toml");
        // Fail closed: an unresolved member is reported, never skipped.
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            edges.push(format!("unresolved member {member}"));
            continue;
        };
        checked += 1;
        if text.contains("reverie-signal") {
            edges.push(format!("{member} names reverie-signal"));
        }
        for a in &aliases {
            if text.contains(&format!("{a}.workspace")) || text.contains(&format!("{a} =")) {
                edges.push(format!("{member} consumes alias {a}"));
            }
        }
    }
    (checked, edges)
}

#[test]
fn no_incoming_dependency_edges() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let (checked, edges) = incoming_edges(&root);
    assert!(
        checked > 15,
        "guard must resolve the real workspace, got {checked}"
    );
    assert_eq!(edges, Vec::<String>::new(), "incoming edges appeared");
    // Negative control: the same guard refuses a dependent tree.
    let tmp = std::env::temp_dir().join(format!("reverie-signal-deptree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("member-a")).unwrap();
    std::fs::create_dir_all(tmp.join("experimental/member-b")).unwrap();
    std::fs::write(
        tmp.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"member-a\", \"experimental/member-b\", \"reverie-signal\"]\n[workspace.dependencies]\nsignals = { package = \"reverie-signal\", path = \"reverie-signal\" }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("member-a/Cargo.toml"),
        "[package]\nname = \"member-a\"\n[dependencies]\nsignals.workspace = true\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("experimental/member-b/Cargo.toml"),
        "[package]\nname = \"member-b\"\n[dependencies]\nreverie-signal = { path = \"../../reverie-signal\" }\n",
    )
    .unwrap();
    let (c_checked, c_edges) = incoming_edges(&tmp);
    assert_eq!(c_checked, 2);
    assert!(
        c_edges.len() >= 3,
        "dependent tree must be refused: {c_edges:?}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn sources_name_no_consumer_or_engine_policy() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let (scanned, hits) = scan_tree(&root);
    assert_eq!(
        hits,
        Vec::<String>::new(),
        "crate sources name consumer/engine vocabulary"
    );
    assert!(
        scanned > 10_000,
        "scan must cover real source bytes, got {scanned}"
    );
    // The SAME wired scan runs on a deliberately contaminated copy of
    // the tree and must refuse it (counted receipts on both sides).
    let tmp =
        std::env::temp_dir().join(format!("reverie-signal-neutrality-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    for name in ["lib.rs", "frame.rs"] {
        std::fs::copy(root.join(name), tmp.join(name)).unwrap();
    }
    let mut contaminated = std::fs::read_to_string(tmp.join("lib.rs")).unwrap();
    contaminated.push_str(&format!(
        "\n// {} {}\n",
        "Det".to_owned() + "core",
        "virtual ".to_owned() + "time"
    ));
    std::fs::write(tmp.join("lib.rs"), contaminated).unwrap();
    let (c_scanned, c_hits) = scan_tree(&tmp);
    assert!(
        c_scanned > 10_000 && c_hits.len() >= 2,
        "contaminated copy must be refused: {c_hits:?}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
    // Negative control: the same scan refuses contaminated text.
    let dirty = format!(
        "let _x = /* {} */ 1; // {} {}",
        "virtual ".to_owned() + "time",
        "Det".to_owned() + "core",
        "turn ".to_owned() + "holder"
    );
    let dirty = dirty.as_str();
    assert_eq!(scan(dirty).len(), 3, "scanner must detect contamination");
    assert_eq!(
        scan(&("the ".to_owned() + "herm" + "it_" + " shell game")).len(),
        1
    );
}

#[test]
fn strace_style_observer_walks_the_domain_standalone() {
    // A Noop/strace-style consumer: feeds sends in, records decisions,
    // applies nothing. Exercises the public surface with no backend.
    let mut ledger = Sigpending::new();
    let leader = TaskGen {
        tid: 4242,
        generation: 1,
    };
    let mut domain = ProcessDomain::new(leader, 1000, 64);
    domain.actions[9] = SigAction {
        kind: DispositionKind::Handler,
        handler: 0x401000,
        mask: 0,
        flags: SA_RESTORER | SA_SIGINFO,
        restorer: 0x7f0000,
    };
    let mut observed = Vec::new();
    for (signo, value) in [(10, 11i64), (40, 22), (40, 33), (17, 0)] {
        let info = SigInfo {
            signo,
            code: -1,
            value,
            pid: 7,
            uid: 8,
            addr: 0,
            overrun: 0,
        };
        observed.push(
            domain
                .send_process(info, SendSource::SigQueue, &mut ledger)
                .queue,
        );
    }
    let facts = [SchedFact {
        task: leader,
        running: true,
        exiting: false,
        stopped: false,
    }];
    assert_eq!(domain.choose_receiver(10, leader, &facts), Some(leader));
    let env = FrameEnv {
        sp_now: 0x7fff_0000,
        xstate: XstateDesc::new(SUPPORTED_XFEATURES, 832, 0xffbf).unwrap(),
        stack_lower_bound: 0,
    };
    let mut values = Vec::new();
    while let Some(plan) = domain.next_delivery(leader, &env, &mut ledger) {
        values.push((plan.info.signo, plan.info.value));
    }
    // SIGCHLD is default-ignored and unblocked: discarded at generation.
    assert_eq!(observed.len(), 4);
    assert_eq!(observed[3], QueueResult::DiscardedIgnored);
    assert_eq!(values, vec![(10, 11), (40, 22), (40, 33)]);
    assert!(!values.is_empty(), "the walk must execute deliveries (N>0)");
}
