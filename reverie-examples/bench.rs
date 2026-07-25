#!/usr/bin/env -S cargo +nightly -Zscript
---
[package]
edition = "2024"
---

//! Cross-backend syscall-counter harness.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt;
use std::fs::OpenOptions;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::time::Duration;
use std::time::Instant;

const USAGE: &str = "usage: bench.rs --backend ptrace|dbi|kvm|sabre|all -- PROGRAM [ARG ...]";

#[derive(Clone, Copy, Debug)]
enum Backend {
    Ptrace,
    Dbi,
    Kvm,
    Sabre,
}

impl Backend {
    const ALL: [Self; 4] = [Self::Ptrace, Self::Dbi, Self::Kvm, Self::Sabre];

    fn name(self) -> &'static str {
        match self {
            Self::Ptrace => "ptrace",
            Self::Dbi => "dbi",
            Self::Kvm => "kvm",
            Self::Sabre => "sabre",
        }
    }
}

enum RunError {
    Unavailable(String),
    Failed(String),
}

impl fmt::Display for RunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Failed(message) => formatter.write_str(message),
        }
    }
}

struct Measurement {
    elapsed: Duration,
    syscalls: u64,
}

struct Args {
    backends: Vec<Backend>,
    command: Vec<OsString>,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = std::env::args_os().skip(1).peekable();
    if args.peek().is_some_and(|arg| arg == "--self-test") {
        parser_self_test();
        return Ok(None);
    }
    if args
        .peek()
        .is_some_and(|arg| arg == "-h" || arg == "--help")
    {
        println!("{USAGE}");
        return Ok(None);
    }

    let mut selected = None;
    while let Some(argument) = args.next() {
        if argument == "--" {
            break;
        }
        if argument == "--backend" {
            selected = args.next();
            continue;
        }
        return Err(format!("unexpected argument {:?}\n{USAGE}", argument));
    }

    let selected = selected.ok_or_else(|| format!("missing --backend\n{USAGE}"))?;
    let backends = match selected.to_str() {
        Some("ptrace") => vec![Backend::Ptrace],
        Some("dbi") => vec![Backend::Dbi],
        Some("kvm") => vec![Backend::Kvm],
        Some("sabre") => vec![Backend::Sabre],
        Some("all") => Backend::ALL.to_vec(),
        _ => return Err(format!("unknown backend {:?}\n{USAGE}", selected)),
    };
    let command: Vec<OsString> = args.collect();
    if command.is_empty() {
        return Err(format!("missing program\n{USAGE}"));
    }
    Ok(Some(Args { backends, command }))
}

fn workspace_root() -> Result<PathBuf, RunError> {
    let script_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        script_directory.to_path_buf(),
        std::env::current_dir().map_err(|error| RunError::Failed(error.to_string()))?,
    ];
    for candidate in candidates {
        for ancestor in candidate.ancestors() {
            if ancestor.join("reverie-examples/Cargo.toml").is_file()
                && ancestor.join("reverie-kvm/Cargo.toml").is_file()
            {
                return Ok(ancestor.to_path_buf());
            }
        }
    }
    Err(RunError::Failed(
        "could not locate the Reverie workspace".into(),
    ))
}

fn target_directory(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || root.join("target"),
        |path| {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        },
    )
}

fn cargo(root: &Path) -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root);
    command
}

fn build(root: &Path, packages: &[&str], binary: Option<&str>) -> Result<(), RunError> {
    let mut command = cargo(root);
    command.arg("build");
    for package in packages {
        command.args(["-p", package]);
    }
    if let Some(binary) = binary {
        command.args(["--bin", binary]);
    }
    let status = command
        .status()
        .map_err(|error| RunError::Failed(format!("failed to start Cargo: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(RunError::Failed(format!("Cargo build failed: {status}")))
    }
}

fn timed_output(command: &mut Command) -> Result<(Duration, Output), RunError> {
    let start = Instant::now();
    let output = command
        .output()
        .map_err(|error| RunError::Failed(format!("failed to start backend: {error}")))?;
    let elapsed = start.elapsed();
    if !output.status.success() {
        return Err(RunError::Failed(format!(
            "guest exited with {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok((elapsed, output))
}

fn parse_last_count(output: &[u8], marker: &str) -> Option<u64> {
    String::from_utf8_lossy(output)
        .lines()
        .rev()
        .find_map(|line| {
            let start = line.find(marker)? + marker.len();
            let digits: String = line[start..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
        })
}

fn require_count(output: &[u8], marker: &str) -> Result<u64, RunError> {
    parse_last_count(output, marker).ok_or_else(|| {
        RunError::Failed(format!(
            "backend did not emit {marker:?}; stderr: {}",
            String::from_utf8_lossy(output).trim()
        ))
    })
}

fn append_guest(command: &mut Command, guest: &[OsString]) {
    command.arg("--").args(guest);
}

fn run_ptrace(root: &Path, guest: &[OsString]) -> Result<Measurement, RunError> {
    build(root, &["reverie-examples"], Some("counter2"))?;
    let mut command = Command::new(target_directory(root).join("debug/counter2"));
    append_guest(&mut command, guest);
    let (elapsed, output) = timed_output(&mut command)?;
    let syscalls = require_count(&output.stderr, "Total system calls in process tree: ")?;
    Ok(Measurement { elapsed, syscalls })
}

fn last_nonempty_line(output: &[u8]) -> Option<PathBuf> {
    String::from_utf8_lossy(output)
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| PathBuf::from(line.trim()))
}

fn run_dbi(root: &Path, guest: &[OsString]) -> Result<Measurement, RunError> {
    if !root.join("third-party/dynamorio/CMakeLists.txt").is_file() {
        return Err(RunError::Unavailable(
            "DynamoRIO source is not active; run `with-proxy scripts/backend-submodule.sh activate dynamorio`"
                .into(),
        ));
    }
    let build_output = Command::new(root.join("reverie-dbi/scripts/build-client.sh"))
        .current_dir(root)
        .env("PROFILE", "debug")
        .output()
        .map_err(|error| RunError::Failed(format!("failed to build DBI client: {error}")))?;
    if !build_output.status.success() {
        return Err(RunError::Failed(format!(
            "DBI client build failed: {}",
            String::from_utf8_lossy(&build_output.stderr).trim()
        )));
    }
    let client = last_nonempty_line(&build_output.stdout)
        .filter(|path| path.is_file())
        .ok_or_else(|| RunError::Failed("DBI build did not report a client library".into()))?;
    let helper = target_directory(root).join("debug/reverie-dbi-dynamorio-path");
    let helper_output = Command::new(&helper)
        .arg("drrun")
        .output()
        .map_err(|error| RunError::Failed(format!("failed to query drrun path: {error}")))?;
    let drrun = last_nonempty_line(&helper_output.stdout)
        .filter(|path| path.is_file())
        .ok_or_else(|| RunError::Failed("DBI path helper did not report drrun".into()))?;

    let mut command = Command::new(drrun);
    command
        .args(["-quiet", "-disable_rseq", "-stack_size", "2M", "-c"])
        .arg(client)
        .arg("-summary");
    append_guest(&mut command, guest);
    let (elapsed, output) = timed_output(&mut command)?;
    let syscalls = require_count(&output.stderr, " syscalls=")?;
    Ok(Measurement { elapsed, syscalls })
}

fn run_kvm(root: &Path, guest: &[OsString]) -> Result<Measurement, RunError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .map_err(|error| RunError::Unavailable(format!("/dev/kvm is unavailable: {error}")))?;
    build(root, &["reverie-kvm"], Some("reverie-kvm-counter"))?;
    let mut command = Command::new(target_directory(root).join("debug/reverie-kvm-counter"));
    append_guest(&mut command, guest);
    let (elapsed, output) = timed_output(&mut command)?;
    let syscalls = require_count(&output.stderr, "reverie-counter: syscalls=")?;
    Ok(Measurement { elapsed, syscalls })
}

fn sabre_binary(root: &Path) -> Result<PathBuf, RunError> {
    if let Some(path) = std::env::var_os("SABRE_BINARY").map(PathBuf::from) {
        return path
            .is_file()
            .then_some(path)
            .ok_or_else(|| RunError::Unavailable("SABRE_BINARY does not name a file".into()));
    }
    let binary = target_directory(root).join("sabre/sabre");
    if binary.is_file() {
        return Ok(binary);
    }
    if !root.join("third-party/sabre/CMakeLists.txt").is_file() {
        return Err(RunError::Unavailable(
            "SaBRe source is not active; run `with-proxy scripts/backend-submodule.sh activate sabre`"
                .into(),
        ));
    }
    let build_directory = target_directory(root).join("sabre");
    let configure = Command::new("cmake")
        .args(["-S", "third-party/sabre", "-B"])
        .arg(&build_directory)
        .current_dir(root)
        .output()
        .map_err(|error| RunError::Failed(format!("failed to configure SaBRe: {error}")))?;
    if !configure.status.success() {
        return Err(RunError::Failed(format!(
            "SaBRe configure failed: {}",
            String::from_utf8_lossy(&configure.stderr).trim()
        )));
    }
    let compile = Command::new("cmake")
        .args(["--build"])
        .arg(&build_directory)
        .current_dir(root)
        .output()
        .map_err(|error| RunError::Failed(format!("failed to build SaBRe: {error}")))?;
    if !compile.status.success() || !binary.is_file() {
        return Err(RunError::Failed(format!(
            "SaBRe build failed: {}",
            String::from_utf8_lossy(&compile.stderr).trim()
        )));
    }
    Ok(binary)
}

fn run_sabre(root: &Path, guest: &[OsString]) -> Result<Measurement, RunError> {
    let sabre = sabre_binary(root)?;
    build(root, &["riptrace", "riptrace-tool"], None)?;
    let target = target_directory(root).join("debug");
    let mut command = Command::new(target.join("riptrace"));
    command
        .arg("--sabre")
        .arg(sabre)
        .arg("--plugin")
        .arg(target.join("libriptrace_plugin.so"))
        .args(["--quiet", "--summary"]);
    append_guest(&mut command, guest);
    let (elapsed, output) = timed_output(&mut command)?;
    let syscalls = require_count(&output.stderr, "Saw ")?;
    Ok(Measurement { elapsed, syscalls })
}

fn measure(backend: Backend, root: &Path, guest: &[OsString]) -> Result<Measurement, RunError> {
    match backend {
        Backend::Ptrace => run_ptrace(root, guest),
        Backend::Dbi => run_dbi(root, guest),
        Backend::Kvm => run_kvm(root, guest),
        Backend::Sabre => run_sabre(root, guest),
    }
}

fn csv_field(value: &OsStr) -> String {
    format!("\"{}\"", value.to_string_lossy().replace('"', "\"\""))
}

fn parser_self_test() {
    assert_eq!(
        parse_last_count(
            b"Total system calls in process tree: 42, from 1 process\n",
            "Total system calls in process tree: "
        ),
        Some(42)
    );
    assert_eq!(
        parse_last_count(
            b"reverie-dbi: tool=PrototypeTool branches=9 syscalls=17 rewritten=2\n",
            " syscalls="
        ),
        Some(17)
    );
    assert_eq!(
        parse_last_count(b"Saw 5 syscalls\nSaw 11 syscalls\n", "Saw "),
        Some(11)
    );
    println!("counter parser self-test passed");
}

fn run() -> Result<i32, String> {
    let Some(args) = parse_args()? else {
        return Ok(0);
    };
    let root = workspace_root().map_err(|error| error.to_string())?;
    let all = args.backends.len() > 1;
    let program = &args.command[0];
    let mut failures = 0;
    let mut successes = 0;

    println!("program,backend,wall_time_ms,syscall_count");
    for backend in args.backends {
        match measure(backend, &root, &args.command) {
            Ok(measurement) => {
                successes += 1;
                println!(
                    "{},{},{:.3},{}",
                    csv_field(program),
                    backend.name(),
                    measurement.elapsed.as_secs_f64() * 1000.0,
                    measurement.syscalls
                );
            }
            Err(RunError::Unavailable(message)) if all => {
                eprintln!("reverie-counter: skipping {}: {message}", backend.name());
            }
            Err(error) => {
                failures += 1;
                eprintln!("reverie-counter: {}: {error}", backend.name());
            }
        }
    }

    Ok(i32::from(failures != 0 || successes == 0))
}

fn main() {
    match run() {
        Ok(status) => std::process::exit(status),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}
