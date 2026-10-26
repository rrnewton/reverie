/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Shared command tests. Each target supplies its command helper and the same Tools.

#[test]
fn noop_tool_test() {
    let (output, _) = test_cmd::<NoopTool>("/bin/pwd", &[]).unwrap();
    // pwd should succeed & print some characters:
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(!output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn counter_tool_test() {
    let (output, state) = test_cmd::<CounterLocal>("ls", &[]).unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    // ls should print some characters and perform some syscalls:
    assert!(!output.stdout.is_empty());
    assert!(AtomicU64::load(&state.num_syscalls, Ordering::SeqCst) > 30);
}

#[test]
fn error_exit_test() {
    let (output, state) = test_cmd::<CounterLocal>("/bin/bash", &["-c", "exit 42"]).unwrap();
    assert_eq!(output.status, ExitStatus::Exited(42));
    assert_eq!(output.stdout.len(), 0);
    assert!(AtomicU64::load(&state.num_syscalls, Ordering::SeqCst) > 0);
}

#[cfg(not(sanitized))]
#[test]
fn run_guest_command_test() {
    let (output, _state) = test_cmd::<CounterLocal>("/bin/echo", &["-n", "abcd"]).unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert_eq!(output.stdout.as_slice(), b"abcd");
}

#[cfg(not(sanitized))]
#[test]
fn run_guest_command_test_closure() {
    let msg = "abcd";
    let (output, _state) = test_cmd::<CounterLocal>("/bin/echo", &["-n", msg]).unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert_eq!(output.stdout.as_slice(), msg.as_bytes());
}
