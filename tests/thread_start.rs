/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#![cfg(not(sanitized))]

#[path = "support/thread_start_guest.rs"]
mod thread_start_guest;
#[path = "tool_fixtures/thread_start.rs"]
mod thread_start_tool;

use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::Guest;
use reverie::Tool;
use reverie::syscalls;
use thread_start_tool::TestTool;

#[test]
fn thread_start_inject() {
    let state = reverie_ptrace::testing::check_fn::<TestTool, _>(thread_start_guest::guest);
    let observed = state.observed.load(Ordering::SeqCst);
    eprintln!("thread-start function: observed={observed}");
    assert!(observed);

    rust_command_test();
}

#[test]
fn thread_start_tail_inject() {
    #[derive(Debug, Default)]
    struct TestTool;

    #[reverie::tool]
    impl Tool for TestTool {
        type GlobalState = ();
        type ThreadState = ();

        async fn handle_thread_start<T: Guest<Self>>(&self, guest: &mut T) -> Result<(), Error> {
            guest.tail_inject(syscalls::Getpid::new()).await
        }
    }

    reverie_ptrace::testing::check_fn::<TestTool, _>(|| {});
}

fn assert_command_output(
    label: &str,
    output: &reverie::process::Output,
    state: &thread_start_tool::StartupObserved,
) {
    let observed = state.observed.load(Ordering::SeqCst);
    eprintln!(
        "thread-start Rust command {label}: status={:?} stdout={:02x?} stderr={:02x?} observed={observed}",
        output.status, output.stdout, output.stderr,
    );
    assert_eq!(output.status, reverie::ExitStatus::Exited(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(observed);
}

fn rust_command_test() {
    let program = env!("CARGO_BIN_EXE_thread-start-rust-command-guest");
    eprintln!("thread-start Rust command image={program}");

    let native = std::process::Command::new(program).output().unwrap();
    eprintln!(
        "thread-start Rust command native: status={:?} stdout={:02x?} stderr={:02x?}",
        native.status, native.stdout, native.stderr,
    );
    assert_eq!(native.status.code(), Some(0));
    assert!(native.stdout.is_empty());
    assert!(native.stderr.is_empty());

    let (output, state) =
        reverie_test_support::test_cmd_with_config::<TestTool>(program, &[], ()).unwrap();
    assert_command_output("ptrace", &output, &state);
}

