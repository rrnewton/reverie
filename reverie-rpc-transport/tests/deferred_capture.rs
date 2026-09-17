#![cfg(target_os = "linux")]
use std::process::Command;
use std::time::Duration;
#[path = "fixtures/owned_lifecycle.rs"]
mod owned_lifecycle;
#[test]
fn fresh_process_preparation_adds_no_tasks_and_real_guest_reap_qualifies() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_guest_log_lifecycle_fixture"));
    command.arg("--deferred-capture");
    let (output, _) = owned_lifecycle::run(command, Duration::from_secs(50)).unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("zero-task preparation"), "{stdout}");
    assert!(stdout.contains("guest actual FINISH"), "{stdout}");
    assert!(
        stdout.contains("deferred capture qualified after actual reap"),
        "{stdout}"
    );
    print!("{stdout}");
}
