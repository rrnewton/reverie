#![cfg(target_os = "linux")]

use std::process::Command;
use std::time::Duration;

#[path = "fixtures/owned_lifecycle.rs"]
mod owned_lifecycle;

fn capture(mode: &str) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mapped_capture_fixture"));
    command.arg(mode);
    let (output, _) = owned_lifecycle::run(command, Duration::from_secs(20)).unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout,
        format!("mapped capture complete: {mode}\n").as_bytes()
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn independent_outputs_need_actual_process_reap() {
    capture("complete");
}

#[test]
fn missing_private_finish_does_not_change_public_completion() {
    capture("missing-private-finish");
}

#[test]
fn abandoned_process_owner_is_incomplete() {
    capture("abandoned-owner");
}
