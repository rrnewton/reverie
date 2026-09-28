//! Bounded subprocesses for tests that change process-wide runtime state.

use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

pub(crate) fn run(name: &str, marker: &str) -> Output {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(marker, name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("{name} exceeded its subprocess deadline: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
        "{name} was not discovered: {output:?}"
    );
    output
}

pub(crate) fn isolated(name: &str, marker: &str) -> bool {
    if std::env::var(marker).as_deref() == Ok(name) {
        return true;
    }
    let output = run(name, marker);
    assert!(output.status.success(), "{name}: {output:?}");
    // Preserve explicit native-capability diagnostics from the isolated child.
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    false
}
