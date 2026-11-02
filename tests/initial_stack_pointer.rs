/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#![cfg(all(not(sanitized), test, target_arch = "x86_64"))]

#[path = "tool_fixtures/initial_stack_pointer.rs"]
mod stack_tool;

use reverie::process::Output;
use stack_tool::Observation;
use stack_tool::Observations;
use stack_tool::StackTool;

fn observed(label: &str, args: &[&str], output: &Output, global: Observations) -> Observation {
    eprintln!(
        "initial-stack {label}: program=/bin/true args={args:?} status={:?} stdout={:02x?} stderr={:02x?}",
        output.status, output.stdout, output.stderr,
    );
    let seen = global.into_seen();
    assert_eq!(seen.len(), 1, "exactly one post-exec observation");
    let (sender, observation) = seen.into_iter().next().unwrap();
    eprintln!(
        "initial-stack {label}: sender={sender} pid={} tid={} reported={:?} rsp={} arguments={:?}",
        observation.pid,
        observation.tid,
        observation.reported,
        observation.rsp,
        observation.arguments,
    );
    assert!(sender > 0);
    assert_eq!(sender, observation.pid);
    assert_eq!(sender, observation.tid);
    observation
}

// Keep the four original success predicates together for both positive runs
// and the genuinely successful alternate-argv run's rejection.
fn assert_initial_stack(output: &Output, observation: &Observation) {
    let (reported, rsp, arguments) = (
        observation.reported,
        observation.rsp,
        &observation.arguments,
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(reported, Some(rsp));
    assert_eq!(&arguments[1..], ["first", "second"]);
    assert!(arguments[0].ends_with("true"), "{arguments:?}");
}

#[test]
fn ptrace_initial_stack_pointer_at_argc() {
    let args = ["first", "second"];
    let (output, global) = reverie_test_support::test_cmd::<StackTool>("/bin/true", &args).unwrap();
    let observation = observed("ptrace", &args, &output, global);
    assert_initial_stack(&output, &observation);
}
