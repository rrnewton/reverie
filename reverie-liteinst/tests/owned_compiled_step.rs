#![cfg(feature = "test-owned-cpuid")]

use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

const GUEST: &str = env!("CARGO_BIN_EXE_reverie-liteinst-owned-compiled-step");

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(directory: &Path, armed: bool, work: u64, budget: &str) -> Output {
    assert!(
        std::env::vars_os()
            .all(|(key, _)| !key.as_encoded_bytes().starts_with(b"LD_") && key != "GLIBC_TUNABLES"),
        "invoke frozen test directly; do not silently remove loader variables"
    );
    assert_eq!(
        std::fs::symlink_metadata("/etc/ld.so.preload")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    let socket = directory.join("s");
    let mut server = Server(
        Command::new(GUEST)
            .args(["server"])
            .arg(&socket)
            .stdout(Stdio::from(
                std::fs::File::create(directory.join("server.stdout")).unwrap(),
            ))
            .stderr(Stdio::from(
                std::fs::File::create(directory.join("server.stderr")).unwrap(),
            ))
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(server.0.try_wait().unwrap().is_none(), "RPC server exited");
        assert!(Instant::now() < deadline, "RPC setup timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut guest = Command::new(GUEST)
        .arg("guest")
        .arg(&socket)
        .arg(directory.join("frames.bin"))
        .arg(work.to_string())
        .arg(if armed { "armed" } else { "unarmed" })
        .arg(budget)
        .stdout(Stdio::from(
            std::fs::File::create(directory.join("guest.stdout")).unwrap(),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(directory.join("guest.stderr")).unwrap(),
        ))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = guest.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            guest.kill().unwrap();
            let status = guest.wait().unwrap();
            std::fs::write(directory.join("timeout.status"), status.to_string()).unwrap();
            panic!(
                "guest timed out; streams retained at {}",
                directory.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    std::fs::write(directory.join("status"), status.to_string()).unwrap();
    Output {
        status,
        stdout: std::fs::read(directory.join("guest.stdout")).unwrap(),
        stderr: std::fs::read(directory.join("guest.stderr")).unwrap(),
    }
}

fn directory(name: &str) -> std::path::PathBuf {
    let root = std::env::var_os("REVERIE_COMPILED_STEP_ARTIFACTS")
        .expect("external immutable evidence directory required");
    let directory = Path::new(&root).join(name);
    std::fs::create_dir(&directory).unwrap();
    assert!(
        directory.join("s").as_os_str().as_encoded_bytes().len() < 100,
        "short private output path required"
    );
    directory
}

#[test]
fn compiled_unarmed_native_outputs_and_once_only_pipe() {
    let directory = directory("unarmed");
    let output = run(&directory, false, 0, "full");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    instruction_results(&text);
    print!("{text}");
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("compiled-event:"))
            .count(),
        6
    );
    assert!(text.contains("clocks=[0, 33, 33, 33, 33, 33, 33]"));
}

#[test]
fn compiled_precise_timers_preserve_whole_trajectory_across_runtime_work() {
    let unarmed = directory("armed-prequalification");
    let output = run(&unarmed, false, 0, "full");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let control = String::from_utf8(output.stdout).unwrap();
    let expected_instructions = instruction_results(&control);
    for work in [0, 20000] {
        for repeat in 0..3 {
            let directory = directory(&format!("w{work}-r{repeat}"));
            let output = run(&directory, true, work, "full");
            assert!(output.status.success(), "{output:?}");
            assert!(output.stderr.is_empty(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            assert_eq!(instruction_results(&text), expected_instructions);
            print!("{text}");
            assert_eq!(
                text.lines()
                    .filter(|line| line.starts_with("compiled-event:"))
                    .count(),
                22
            );
            assert_eq!(
                text.lines()
                    .filter(|line| line.starts_with("compiled-position:"))
                    .count(),
                16
            );
            assert!(text.contains("clocks=[0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30, 32, 33, 33, 33, 33, 33, 33]"));
            assert!(
                std::fs::metadata(directory.join("frames.bin"))
                    .unwrap()
                    .len()
                    > 133120
            );
        }
    }
}

fn instruction_results(text: &str) -> &str {
    let records: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("compiled-instructions:"))
        .collect();
    assert_eq!(
        records.len(),
        1,
        "missing or duplicated instruction result vector"
    );
    records[0]
}

#[test]
fn compiled_insufficient_evidence_budget_is_terminal_before_first_tool_event() {
    let directory = directory("budget");
    let output = run(&directory, true, 0, "small");
    assert_eq!(output.status.code(), Some(126), "{output:?}");
    assert_eq!(output.stderr, b"owned-native: evidence budget exhausted\n");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("budget=64"));
    assert!(!text.contains("compiled-event:"));
    assert!(!text.contains("compiled-terminal:"));
}
