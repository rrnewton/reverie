use std::process::Command;

fn run_harness(mode: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-lifecycle-guest"))
        .arg(mode)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{mode}: status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn waits_for_descendants_after_the_root_exits() {
    run_harness("root-exit-harness");
}

#[test]
fn fails_closed_when_vdso_clock_calls_cannot_be_patched() {
    run_harness("clock-harness");
}

#[test]
fn reports_the_root_signal_status() {
    run_harness("signal-harness");
}
