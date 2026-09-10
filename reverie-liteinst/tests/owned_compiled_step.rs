#![cfg(feature = "test-owned-cpuid")]

use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

const GUEST: &str = env!("CARGO_BIN_EXE_reverie-liteinst-owned-compiled-step");

fn release_guest() -> &'static Path {
    static RELEASE: OnceLock<std::path::PathBuf> = OnceLock::new();
    RELEASE.get_or_init(|| {
        let directory = directory("release-build");
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let target = Path::new(GUEST)
            .parent()
            .unwrap()
            .join("compiled-step-release");
        let compiler = Command::new("rustup")
            .args(["which", "--toolchain", "nightly-2026-07-29", "rustc"])
            .output()
            .unwrap();
        assert!(compiler.status.success(), "{compiler:?}");
        let compiler = String::from_utf8(compiler.stdout).unwrap();
        let version = Command::new(compiler.trim()).arg("-vV").output().unwrap();
        assert!(version.status.success(), "{version:?}");
        std::fs::write(directory.join("rustc.txt"), version.stdout).unwrap();
        let mut command = Command::new("timeout");
        command
            .args([
                "--kill-after=10s",
                "600s",
                "rustup",
                "run",
                "nightly-2026-07-29",
                "cargo",
                "build",
                "--offline",
                "--locked",
                "--release",
                "--all-features",
                "-p",
                "reverie-liteinst-runtime",
                "-p",
                "reverie-liteinst",
                "-p",
                "reverie-rpc-transport",
                "--bin",
                "reverie-liteinst-owned-compiled-step",
                "--target",
                "x86_64-unknown-linux-gnu",
                "--message-format=json",
                "--target-dir",
            ])
            .arg(&target)
            .current_dir(workspace)
            .env("RUSTC", compiler.trim())
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .stdout(Stdio::from(
                std::fs::File::create(directory.join("cargo.jsonl")).unwrap(),
            ))
            .stderr(Stdio::from(
                std::fs::File::create(directory.join("cargo.stderr")).unwrap(),
            ));
        let flags = std::env::vars_os()
            .filter(|(name, _)| {
                name == "RUSTFLAGS"
                    || name == "CARGO_ENCODED_RUSTFLAGS"
                    || name
                        .as_encoded_bytes()
                        .starts_with(b"CARGO_PROFILE_RELEASE_")
            })
            .collect::<Vec<_>>();
        std::fs::write(
            directory.join("command.txt"),
            format!("{command:?}\nprofile environment: {flags:?}\n"),
        )
        .unwrap();
        let status = command.status().unwrap();
        std::fs::write(directory.join("status"), status.to_string()).unwrap();
        assert!(
            status.success(),
            "release fixture build failed; evidence: {}",
            directory.display()
        );
        let built =
            target.join("x86_64-unknown-linux-gnu/release/reverie-liteinst-owned-compiled-step");
        let guest = directory.join("guest");
        std::fs::copy(built, &guest).unwrap();
        let hash = Command::new("sha256sum").arg(&guest).output().unwrap();
        assert!(hash.status.success(), "{hash:?}");
        std::fs::write(directory.join("guest.sha256"), hash.stdout).unwrap();
        guest
    })
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn configure_child(directory: &Path, role: &str, command: &mut Command) {
    let mut names = std::env::vars_os()
        .map(|(name, _)| name)
        .chain(command.get_envs().map(|(name, _)| name.to_owned()))
        .filter(|name| name.as_encoded_bytes().starts_with(b"LD_") || name == "GLIBC_TUNABLES")
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    for name in &names {
        command.env_remove(name);
    }
    std::fs::write(
        directory.join(format!("preflight-loader-{role}.txt")),
        format!("removed from child command: {names:?}\n"),
    )
    .unwrap();
}

fn run(directory: &Path, armed: bool, work: u64, budget: &str) -> Output {
    assert_eq!(
        std::fs::symlink_metadata("/etc/ld.so.preload")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    let socket = directory.join("s");
    let mut server_command = Command::new(release_guest());
    server_command
        .args(["server"])
        .arg(&socket)
        .stdout(Stdio::from(
            std::fs::File::create(directory.join("server.stdout")).unwrap(),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(directory.join("server.stderr")).unwrap(),
        ));
    configure_child(directory, "server", &mut server_command);
    let mut server = Server(server_command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(server.0.try_wait().unwrap().is_none(), "RPC server exited");
        assert!(Instant::now() < deadline, "RPC setup timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut guest_command = Command::new(release_guest());
    guest_command
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
        ));
    configure_child(directory, "guest", &mut guest_command);
    let mut guest = guest_command.spawn().unwrap();
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
    static DEFAULT_ROOT: OnceLock<std::path::PathBuf> = OnceLock::new();
    let root = std::env::var_os("REVERIE_COMPILED_STEP_ARTIFACTS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            DEFAULT_ROOT
                .get_or_init(|| {
                    tempfile::Builder::new()
                        .prefix("reverie-compiled-step-")
                        .tempdir()
                        .expect("create retained default artifact root")
                        .keep()
                })
                .clone()
        });
    let directory = Path::new(&root).join(name);
    std::fs::create_dir(&directory).unwrap();
    assert!(
        directory.join("s").as_os_str().as_encoded_bytes().len() < 100,
        "short private output path required"
    );
    directory
}

#[test]
fn child_loader_configuration_is_recorded_without_changing_parent() {
    let directory = directory("loader-control");
    let before = std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>();
    for role in ["server", "guest"] {
        let mut command = Command::new(GUEST);
        command.env("LD_PRELOAD", "never-loaded");
        command.env("GLIBC_TUNABLES", "never-applied");
        command.env("PATH", "preserved");
        configure_child(&directory, role, &mut command);
        let configured = command
            .get_envs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            configured.get(std::ffi::OsStr::new("LD_PRELOAD")),
            Some(&None)
        );
        assert_eq!(
            configured.get(std::ffi::OsStr::new("GLIBC_TUNABLES")),
            Some(&None)
        );
        assert_eq!(
            configured.get(std::ffi::OsStr::new("PATH")),
            Some(&Some(std::ffi::OsStr::new("preserved")))
        );
        let report =
            std::fs::read_to_string(directory.join(format!("preflight-loader-{role}.txt")))
                .unwrap();
        assert!(report.contains("LD_PRELOAD"));
        assert!(report.contains("GLIBC_TUNABLES"));
    }
    assert_eq!(
        std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>(),
        before
    );
}

#[test]
fn default_artifacts_remain_after_the_directory_handle_is_dropped() {
    let first = directory("retention-first");
    let saved = first.clone();
    std::fs::write(first.join("evidence"), b"retained").unwrap();
    drop(first);
    let second = directory("retention-second");
    assert_ne!(saved, second);
    assert_eq!(saved.parent(), second.parent());
    assert_eq!(std::fs::read(saved.join("evidence")).unwrap(), b"retained");
}

#[test]
fn release_fixture_has_retained_build_and_executable_identity() {
    let guest = release_guest();
    let directory = guest.parent().unwrap();
    let hash = Command::new("sha256sum").arg(guest).output().unwrap();
    assert!(hash.status.success(), "{hash:?}");
    assert_eq!(
        hash.stdout,
        std::fs::read(directory.join("guest.sha256")).unwrap()
    );
    let command = std::fs::read_to_string(directory.join("command.txt")).unwrap();
    for required in [
        "nightly-2026-07-29",
        "--release",
        "--all-features",
        "reverie-liteinst-runtime",
        "reverie-liteinst",
        "reverie-rpc-transport",
    ] {
        assert!(command.contains(required), "{command}");
    }
    let artifacts = std::fs::read_to_string(directory.join("cargo.jsonl")).unwrap();
    assert!(artifacts.contains("compiler-artifact"));
    assert!(artifacts.contains("reverie-liteinst-owned-compiled-step"));
    assert!(
        std::fs::read_to_string(directory.join("rustc.txt"))
            .unwrap()
            .contains("host: x86_64-unknown-linux-gnu")
    );
}

#[test]
fn compiled_unarmed_native_outputs_and_once_only_pipe() {
    let directory = directory("unarmed");
    let output = run(&directory, false, 0, "full");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(
        std::fs::metadata(directory.join("frames.bin"))
            .unwrap()
            .len()
            > 0
    );
    for suffix in ["before.xsave", "after.xsave"] {
        assert_eq!(
            std::fs::metadata(directory.join("frames.bin").with_extension(suffix))
                .unwrap()
                .len(),
            2440
        );
    }
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
    assert_eq!(output.stderr, b"owned-native: evidence budget exhausted\nliteinst terminal126: operation=owned-context detail=predicate value=none site=reverie-liteinst-runtime/src/owned_context.rs:1096:36\n");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("budget=64"));
    assert!(!text.contains("compiled-event:"));
    assert!(!text.contains("compiled-terminal:"));
}
