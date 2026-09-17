#![cfg(target_os = "linux")]

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

struct OwnedFixture(Child);
impl Drop for OwnedFixture {
    fn drop(&mut self) {
        // The helper and every descendant inherit this independently owned group.
        unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
        let _ = self.0.wait();
    }
}

fn lifecycle(mode: &str) {
    let mut fixture = OwnedFixture(
        Command::new(env!("CARGO_BIN_EXE_guest_log_lifecycle_fixture"))
            .arg(mode)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(50);
    let status = loop {
        if let Some(status) = fixture.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "lifecycle {mode} exceeded 50 seconds"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    fixture
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    fixture
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(
        status.success(),
        "lifecycle {mode}: {status}\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains(&format!(
            "ordinary lifecycle case completed and descendants reaped: {mode}"
        )),
        "{stdout}"
    );
    print!("{stdout}");
}

#[test]
fn fork() {
    lifecycle("fork");
}
#[test]
fn wait_error() {
    lifecycle("wait-error");
}
#[test]
fn failed_fork() {
    lifecycle("failed-fork");
}
#[test]
fn death_before_attach() {
    lifecycle("death-before-attach");
}
#[test]
fn parent_first_exit() {
    lifecycle("parent-first-exit");
}
