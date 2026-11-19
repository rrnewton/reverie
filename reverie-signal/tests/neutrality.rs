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

/// Resolve the workspace graph with Cargo itself (`cargo tree -i`):
/// text matching cannot see valid TOML spellings (quoted headers,
/// table-form aliases), Cargo's resolver can. Returns (workspace member
/// count, incoming dependent lines). An unresolved graph is an edge:
/// the guard fails closed.
fn incoming_edges(root: &std::path::Path) -> (usize, Vec<String>) {
    incoming_edges_scoped(root, false)
}

/// `scratch` is explicit: only scratch fixture trees may have their
/// lockfiles reset per resolution; a real workspace never may.
fn incoming_edges_scoped(root: &std::path::Path, scratch: bool) -> (usize, Vec<String>) {
    let root_text = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let members = root_text
        .split("members = [")
        .nth(1)
        .map(|rest| {
            rest.split(']')
                .next()
                .unwrap()
                .split(',')
                .filter(|s| !s.trim().trim_matches(|c| c == '"' || c == '\'').is_empty())
                .count()
        })
        .unwrap_or(0);
    // One resolver pass across every configuration: all features, all
    // targets, all edge kinds — optional and target-specific incoming
    // dependencies are visible to Cargo even when disabled here.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    // Scratch fixture trees only: their lockfiles pin stale graphs.
    // The real workspace lock is tracked state and never touched.
    // (scratch supplied explicitly by the caller)
    if scratch {
        let _ = std::fs::remove_file(root.join("Cargo.lock"));
    }
    let mut cmd = std::process::Command::new(&cargo);
    if !scratch {
        cmd.arg("--locked");
    }
    let out = cmd
        .args([
            "tree",
            "-i",
            "reverie-signal",
            "--prefix",
            "none",
            "--offline",
            "--all-features",
            "--target",
            "all",
            "-e",
            "all",
            "--workspace",
        ])
        .current_dir(root)
        .output()
        .expect("run cargo tree");
    if !out.status.success() {
        return (members, vec!["unresolved workspace graph".to_owned()]);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut edges: Vec<String> = text
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty() && !l.contains(" feature "))
        .map(|l| format!("dependent: {}", l.trim()))
        .collect();
    // Declared-but-unresolvable configurations still count: any member
    // manifest naming the package is a declared incoming edge.
    if let Some(list) = root_text.split("members = [").nth(1) {
        for member in list
            .split(']')
            .next()
            .unwrap()
            .split(',')
            .map(|x| x.trim().trim_matches(|c| c == '"' || c == '\''))
            .filter(|x| !x.is_empty() && *x != "reverie-signal")
        {
            let manifest = root.join(member).join("Cargo.toml");
            match std::fs::read_to_string(&manifest) {
                Ok(text) if text.contains("reverie-signal") => {
                    edges.push(format!("declared: {member}"));
                }
                Ok(_) => {}
                Err(_) => edges.push(format!("unresolved member {member}")),
            }
        }
    }
    edges.sort();
    edges.dedup();
    (members, edges)
}

#[test]
fn no_incoming_dependency_edges() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    // Even with TMPDIR as an ancestor of the checkout, the real root
    // is never treated as scratch (explicit ownership, no path guess).
    // SAFETY: this test process owns its environment.
    unsafe {
        std::env::set_var("TMPDIR", root.parent().unwrap());
    }
    let lock_before = std::fs::read(root.join("Cargo.lock")).unwrap();
    let (checked, edges) = incoming_edges(&root);
    assert_eq!(
        std::fs::read(root.join("Cargo.lock")).unwrap(),
        lock_before,
        "the guard must not modify the workspace Cargo.lock"
    );
    assert!(
        checked > 15,
        "guard must resolve the real workspace, got {checked}"
    );
    assert_eq!(edges, Vec::<String>::new(), "incoming edges appeared");
    // Negative control: the same guard refuses a dependent tree.
    let tmp = std::env::temp_dir().join(format!("reverie-signal-deptree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    for stub in [
        "member-a/src",
        "experimental/member-b/src",
        "reverie-signal/src",
    ] {
        std::fs::create_dir_all(tmp.join(stub)).unwrap();
        std::fs::write(tmp.join(stub).join("lib.rs"), "").unwrap();
    }
    std::fs::write(
        tmp.join("reverie-signal/Cargo.toml"),
        "[package]\nname = \"reverie-signal\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"member-a\", \"experimental/member-b\", \"reverie-signal\"]\n[workspace.dependencies]\nsignals = { package = \"reverie-signal\", path = \"reverie-signal\" }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("member-a/Cargo.toml"),
        "[package]\nname = \"member-a\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nsignals.workspace = true\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("experimental/member-b/Cargo.toml"),
        "[package]\nname = \"member-b\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nreverie-signal = { path = \"../../reverie-signal\" }\n",
    )
    .unwrap();
    let (c_checked, c_edges) = incoming_edges_scoped(&tmp, true);
    assert_eq!(c_checked, 3);
    assert!(
        c_edges.len() >= 2,
        "dependent tree must be refused: {c_edges:?}"
    );
    // Optional (disabled feature) and foreign-target declarations too.
    std::fs::create_dir_all(tmp.join("member-c/src")).unwrap();
    std::fs::write(tmp.join("member-c/src/lib.rs"), "").unwrap();
    std::fs::write(
        tmp.join("member-c/Cargo.toml"),
        "[package]\nname = \"member-c\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[target.'cfg(windows)'.dependencies]\nreverie-signal = { path = \"../reverie-signal\", optional = true }\n",
    )
    .unwrap();
    // Valid workspace: the alias declaration stays, so refusal must
    // come from resolution, not a manifest load failure.
    std::fs::write(
        tmp.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"member-a\", \"member-c\", \"experimental/member-b\", \"reverie-signal\"]\n[workspace.dependencies]\nsignals = { package = \"reverie-signal\", path = \"reverie-signal\" }\n",
    )
    .unwrap();
    let (_w_checked, w_edges) = incoming_edges_scoped(&tmp, true);
    assert!(
        w_edges.iter().any(|e| e.contains("member-c")),
        "member-c must be named, not a vacuous graph failure: {w_edges:?}"
    );
    // Spaced table header form must not escape either.
    std::fs::write(
        tmp.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = ['member-a', 'experimental/member-b', 'reverie-signal']\n[ workspace.dependencies.signals ]\npackage = \"reverie-signal\"\npath = \"reverie-signal\"\n",
    )
    .unwrap();
    let (_s_checked, s_edges) = incoming_edges_scoped(&tmp, true);
    assert!(
        !s_edges.is_empty(),
        "spaced table alias tree must be refused: {s_edges:?}"
    );
    // Non-default member with an inherited alias: only a --workspace
    // resolution sees experimental/nostd-print-style members.
    std::fs::write(
        tmp.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"member-a\", \"member-c\", \"experimental/member-b\", \"reverie-signal\"]\ndefault-members = [\"reverie-signal\"]\n[workspace.dependencies]\nsignals = { package = \"reverie-signal\", path = \"reverie-signal\" }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("experimental/member-b/Cargo.toml"),
        "[package]\nname = \"member-b\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nsignals.workspace = true\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("member-c/Cargo.toml"),
        "[package]\nname = \"member-c\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let (_d_checked, d_edges) = incoming_edges_scoped(&tmp, true);
    assert!(
        d_edges.iter().any(|e| e.contains("member-b")),
        "non-default inherited-alias member must be caught: {d_edges:?}"
    );
    // Alias-only control: member-c reaches the package ONLY through
    // the workspace alias, so the resolver pass alone must catch it
    // (the declared-manifest scan cannot see an alias consumption).
    std::fs::write(
        tmp.join("experimental/member-b/Cargo.toml"),
        "[package]\nname = \"member-b\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("member-a/Cargo.toml"),
        "[package]\nname = \"member-a\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("member-c/Cargo.toml"),
        "[package]\nname = \"member-c\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[target.'cfg(windows)'.dependencies]\nsignals = { workspace = true, optional = true }\n",
    )
    .unwrap();
    let (_g_checked, g_edges) = incoming_edges_scoped(&tmp, true);
    assert!(
        g_edges.iter().any(|e| e.contains("member-c")),
        "alias-only windows edge must be caught by resolution alone: {g_edges:?}"
    );
    // Deliberate fail-closed control: a broken workspace refuses.
    std::fs::write(tmp.join("Cargo.toml"), "this is not toml = [").unwrap();
    let (_b_checked, b_edges) = incoming_edges_scoped(&tmp, true);
    assert!(
        b_edges.iter().any(|e| e.contains("unresolved")),
        "broken tree fails closed: {b_edges:?}"
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
