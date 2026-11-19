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

#[test]
fn sources_name_no_consumer_or_engine_policy() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut scanned = 0usize;
    for name in ["lib.rs", "frame.rs"] {
        let text = std::fs::read_to_string(root.join(name)).unwrap();
        scanned += text.len();
        assert_eq!(
            scan(&text),
            Vec::<String>::new(),
            "{name} names consumer/engine vocabulary"
        );
    }
    assert!(
        scanned > 10_000,
        "scan must cover real source bytes, got {scanned}"
    );
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
        xstate: XstateDesc {
            features: SUPPORTED_XFEATURES,
            size: 832,
            mxcsr_mask: 0xffbf,
        },
        stack_lower_bound: 0,
    };
    let mut values = Vec::new();
    while let Some(plan) = domain.next_delivery(leader, &env, &mut ledger) {
        values.push((plan.info.signo, plan.info.value));
    }
    // Synchronous/numeric order: 10, then SIGCHLD(17) discard, then RT FIFO.
    assert_eq!(observed.len(), 4);
    assert_eq!(values, vec![(10, 11), (17, 0), (40, 22), (40, 33)]);
    assert!(!values.is_empty(), "the walk must execute deliveries (N>0)");
}
