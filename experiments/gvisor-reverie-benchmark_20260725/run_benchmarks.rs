#!/usr/bin/env rust-script

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

const WARMUPS: usize = 2;
const SAMPLES: usize = 9;
const MARGINAL_COUNTS: &[u64] = &[1_000, 10_000, 100_000, 1_000_000];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Backend {
    Native,
    GvisorSystrap,
    GvisorKvm,
    ReveriePtrace,
    ReverieDbi,
    ReverieKvm,
    ReverieSabre,
}

impl Backend {
    const ALL: [Self; 7] = [
        Self::Native,
        Self::GvisorSystrap,
        Self::GvisorKvm,
        Self::ReveriePtrace,
        Self::ReverieDbi,
        Self::ReverieKvm,
        Self::ReverieSabre,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::GvisorSystrap => "gvisor-systrap",
            Self::GvisorKvm => "gvisor-kvm",
            Self::ReveriePtrace => "reverie-ptrace",
            Self::ReverieDbi => "reverie-dbi",
            Self::ReverieKvm => "reverie-kvm",
            Self::ReverieSabre => "reverie-sabre",
        }
    }
}

#[derive(Clone)]
struct Workload {
    name: String,
    command: Vec<OsString>,
    expected_status: i32,
    operations: Option<u64>,
}

struct Paths {
    runsc: PathBuf,
    counter2: PathBuf,
    drrun: PathBuf,
    dbi_client: PathBuf,
    kvm_counter: PathBuf,
    riptrace: PathBuf,
    riptrace_plugin: PathBuf,
    sabre: PathBuf,
}

struct Outcome {
    wall_ns: u128,
    status: Option<i32>,
    observed_syscalls: Option<u64>,
    stderr_tail: String,
}

fn required_path(name: &str) -> io::Result<PathBuf> {
    let value = env::var_os(name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("missing {name}")))?;
    let path = PathBuf::from(value);
    if !path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{name} is not a file: {}", path.display()),
        ));
    }
    Ok(path)
}

fn paths() -> io::Result<Paths> {
    Ok(Paths {
        runsc: required_path("RUNSC")?,
        counter2: required_path("COUNTER2")?,
        drrun: required_path("DRRUN")?,
        dbi_client: required_path("DBI_CLIENT")?,
        kvm_counter: required_path("KVM_COUNTER")?,
        riptrace: required_path("RIPTRACE")?,
        riptrace_plugin: required_path("RIPTRACE_PLUGIN")?,
        sabre: required_path("SABRE")?,
    })
}

fn append_guest(command: &mut Command, workload: &Workload) {
    command.arg("--").args(&workload.command);
}

fn backend_command(backend: Backend, workload: &Workload, paths: &Paths) -> Command {
    let mut command = match backend {
        Backend::Native => {
            let mut command = Command::new(&workload.command[0]);
            command.args(&workload.command[1..]);
            command
        }
        Backend::GvisorSystrap | Backend::GvisorKvm => {
            let platform = if backend == Backend::GvisorSystrap {
                "systrap"
            } else {
                "kvm"
            };
            let mut command = Command::new(&paths.runsc);
            command
                .arg(format!("--platform={platform}"))
                .args([
                    "--network=none",
                    "--TESTONLY-unsafe-nonroot=true",
                    "--rootless",
                    "do",
                    "--quiet",
                    "--",
                ])
                .args(&workload.command);
            command
        }
        Backend::ReveriePtrace => {
            let mut command = Command::new(&paths.counter2);
            append_guest(&mut command, workload);
            command
        }
        Backend::ReverieDbi => {
            let mut command = Command::new(&paths.drrun);
            command
                .args(["-quiet", "-disable_rseq", "-stack_size", "2M", "-c"])
                .arg(&paths.dbi_client)
                .arg("-summary");
            append_guest(&mut command, workload);
            command
        }
        Backend::ReverieKvm => {
            let mut command = Command::new(&paths.kvm_counter);
            append_guest(&mut command, workload);
            command
        }
        Backend::ReverieSabre => {
            let mut command = Command::new(&paths.riptrace);
            command
                .arg("--sabre")
                .arg(&paths.sabre)
                .arg("--plugin")
                .arg(&paths.riptrace_plugin)
                .args(["--quiet", "--summary"]);
            append_guest(&mut command, workload);
            command
        }
    };
    command
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("RUST_LOG", "off")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

fn parse_last_count(output: &str, marker: &str) -> Option<u64> {
    output.lines().rev().find_map(|line| {
        let start = line.find(marker)? + marker.len();
        let digits: String = line[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
    })
}

fn observed_count(backend: Backend, stderr: &str) -> Option<u64> {
    match backend {
        Backend::ReveriePtrace => {
            parse_last_count(stderr, "Total system calls in process tree: ")
        }
        Backend::ReverieDbi => parse_last_count(stderr, " syscalls="),
        Backend::ReverieKvm => parse_last_count(stderr, "reverie-counter: syscalls="),
        Backend::ReverieSabre => parse_last_count(stderr, "Saw "),
        Backend::Native | Backend::GvisorSystrap | Backend::GvisorKvm => None,
    }
}

fn tail(value: &str, limit: usize) -> String {
    let mut chars: Vec<char> = value.chars().rev().take(limit).collect();
    chars.reverse();
    chars.into_iter().collect::<String>().replace('\n', "\\n")
}

fn run_once(backend: Backend, workload: &Workload, paths: &Paths) -> io::Result<Outcome> {
    let start = Instant::now();
    let output = backend_command(backend, workload, paths).output()?;
    let wall_ns = start.elapsed().as_nanos();
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(Outcome {
        wall_ns,
        status: output.status.code(),
        observed_syscalls: observed_count(backend, &stderr),
        stderr_tail: tail(&stderr, 500),
    })
}

fn median(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn append_header(path: &Path, header: &str) -> io::Result<File> {
    let is_new = !path.exists();
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    if is_new {
        writeln!(file, "{header}")?;
    }
    Ok(file)
}

fn run_matrix(
    suite: &str,
    workloads: &[Workload],
    output_dir: &Path,
    paths: &Paths,
) -> io::Result<()> {
    let raw_path = output_dir.join(format!("{suite}-raw.tsv"));
    let summary_path = output_dir.join(format!("{suite}-summary.tsv"));
    if raw_path.exists() || summary_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("refusing to overwrite {} results", suite),
        ));
    }
    let mut raw = append_header(
        &raw_path,
        "suite\tworkload\tbackend\tphase\tsample\twall_ns\tstatus\tobserved_syscalls\toperations\tstderr_tail",
    )?;
    let mut measurements: BTreeMap<(String, Backend), Vec<u128>> = BTreeMap::new();
    let mut counts: BTreeMap<(String, Backend), Vec<u64>> = BTreeMap::new();

    for workload in workloads {
        for round in 0..(WARMUPS + SAMPLES) {
            let phase = if round < WARMUPS { "warmup" } else { "measure" };
            let sample = if round < WARMUPS { round } else { round - WARMUPS };
            for offset in 0..Backend::ALL.len() {
                let backend = Backend::ALL[(offset + round) % Backend::ALL.len()];
                eprintln!(
                    "suite={suite} workload={} phase={phase} sample={sample} backend={}",
                    workload.name,
                    backend.name()
                );
                let outcome = run_once(backend, workload, paths)?;
                let status = outcome.status.unwrap_or(-1);
                writeln!(
                    raw,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    suite,
                    workload.name,
                    backend.name(),
                    phase,
                    sample,
                    outcome.wall_ns,
                    status,
                    outcome
                        .observed_syscalls
                        .map_or_else(|| "NA".into(), |value| value.to_string()),
                    workload
                        .operations
                        .map_or_else(|| "NA".into(), |value| value.to_string()),
                    outcome.stderr_tail,
                )?;
                raw.flush()?;
                let semantically_valid = status == workload.expected_status
                    && !outcome.stderr_tail.contains("Function not implemented");
                if !semantically_valid {
                    eprintln!(
                        "unsupported workload={} backend={} status={} expected={} stderr={}",
                        workload.name, backend.name(), status, workload.expected_status, outcome.stderr_tail
                    );
                    continue;
                }
                if phase == "measure" {
                    measurements
                        .entry((workload.name.clone(), backend))
                        .or_default()
                        .push(outcome.wall_ns);
                    if let Some(count) = outcome.observed_syscalls {
                        counts
                            .entry((workload.name.clone(), backend))
                            .or_default()
                            .push(count);
                    }
                }
            }
        }
    }

    let mut summary = append_header(
        &summary_path,
        "suite\tworkload\tbackend\tsamples\tmedian_wall_ns\tmin_wall_ns\tmax_wall_ns\tmedian_observed_syscalls",
    )?;
    for ((workload, backend), mut values) in measurements {
        let minimum = *values.iter().min().unwrap();
        let maximum = *values.iter().max().unwrap();
        let sample_count = values.len();
        let wall_median = median(&mut values);
        let count_median = counts
            .remove(&(workload.clone(), backend))
            .map(|values| median(&mut values.iter().map(|value| *value as u128).collect::<Vec<_>>()))
            .map_or_else(|| "NA".into(), |value| value.to_string());
        writeln!(
            summary,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            suite,
            workload,
            backend.name(),
            sample_count,
            wall_median,
            minimum,
            maximum,
            count_median
        )?;
    }
    Ok(())
}

fn os(value: impl Into<OsString>) -> OsString {
    value.into()
}

fn real_workloads(root: &Path) -> Vec<Workload> {
    let micro_iterations = env::var("MICRO_ITERATIONS")
        .expect("MICRO_ITERATIONS is required")
        .parse::<u64>()
        .expect("MICRO_ITERATIONS must be an integer");
    let getpid = root.join("workloads/getpid_loop");
    let sqlite = root.join("workloads/sqlite_workload.sql");
    let tar_repeats = env::var("TAR_REPEATS")
        .expect("TAR_REPEATS is required")
        .parse::<usize>()
        .expect("TAR_REPEATS must be an integer");
    let dd_count = env::var("DD_COUNT")
        .expect("DD_COUNT is required")
        .parse::<u64>()
        .expect("DD_COUNT must be an integer");
    let mut tar = vec![os("/usr/bin/tar"), os("cf"), os("/dev/null")];
    tar.extend((0..tar_repeats).map(|_| os("/usr/share/doc")));
    vec![
        Workload {
            name: "getpid-3s".into(),
            command: vec![os(getpid), os(micro_iterations.to_string())],
            expected_status: 0,
            operations: Some(micro_iterations),
        },
        Workload {
            name: "find-usr".into(),
            command: vec![os("/usr/bin/find"), os("/usr"), os("-type"), os("f")],
            expected_status: 1,
            operations: None,
        },
        Workload {
            name: "dd-byte-io".into(),
            command: vec![
                os("/usr/bin/dd"),
                os("if=/dev/zero"),
                os("of=/dev/null"),
                os("bs=1"),
                os(format!("count={dd_count}")),
                os("status=none"),
            ],
            expected_status: 0,
            operations: Some(dd_count * 2),
        },
        Workload {
            name: "tar-doc".into(),
            command: tar,
            expected_status: 0,
            operations: None,
        },
        Workload {
            name: "sqlite-100k".into(),
            command: vec![
                os("/usr/bin/sqlite3"),
                os(":memory:"),
                os(format!(".read {}", sqlite.display())),
            ],
            expected_status: 0,
            operations: Some(100_000),
        },
    ]
}

fn marginal_workloads(root: &Path) -> Vec<Workload> {
    let getpid = root.join("workloads/getpid_loop");
    MARGINAL_COUNTS
        .iter()
        .map(|count| Workload {
            name: format!("getpid-{count}"),
            command: vec![os(getpid.clone()), os(count.to_string())],
            expected_status: 0,
            operations: Some(*count),
        })
        .collect()
}

fn main() -> io::Result<()> {
    let mut args = env::args_os().skip(1);
    let suite = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "usage: run_benchmarks.rs real|marginal OUTPUT_DIR"))?;
    let output_dir = PathBuf::from(args.next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "missing output directory")
    })?);
    fs::create_dir_all(&output_dir)?;
    let root = PathBuf::from(env::var_os("BENCH_ROOT").unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into()));
    let paths = paths()?;
    match suite.as_str() {
        "real" => run_matrix("real", &real_workloads(&root), &output_dir, &paths),
        "marginal" => run_matrix("marginal", &marginal_workloads(&root), &output_dir, &paths),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown suite: {suite}"),
        )),
    }
}
