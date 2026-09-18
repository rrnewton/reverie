//! End-to-end coverage for the constructor-disabled after-loader host runner.
//!
//! This test consumes three retained staging artifacts named by
//! `REVERIE_LITEINST_AFTER_LOADER_RUNTIME`,
//! `REVERIE_LITEINST_AFTER_LOADER_MARKER`, and
//! `REVERIE_LITEINST_AFTER_LOADER_GRAPH`. Run it in Cargo's release profile
//! with default features disabled and only `liteinst-after-loader-experiment`
//! enabled. The graph file is canonical UTF-8 with a final newline:
//!
//! ```text
//! schema=1
//! executable_sha256=<lowercase SHA-256>
//! executable_pt_interp=/lib64/ld-linux-x86-64.so.2
//! provider=libc.so.6
//! image=ld-linux-x86-64.so.2<TAB>/canonical/path<TAB><lowercase SHA-256>
//! image=libc.so.6<TAB>/canonical/path<TAB><lowercase SHA-256>
//! ```
//!
//! `image` lines must be strictly sorted by SONAME and must describe the exact
//! dependency closure of both the fixed executable and staged runtime. The
//! staging producer builds the runtime with the same Cargo flags, emits its
//! marker with `LiteinstCallerImage::runtime_stage_marker`, compiles this
//! fixture with the flags below, obtains PT_INTERP with `readelf -lW`, resolves
//! both dependency closures with `LC_ALL=C ldd`, canonicalizes every path with
//! `realpath`, and records `sha256sum` for every image. The retained graph must
//! be reviewed before it is supplied here. This test verifies all of those
//! claims independently before execution.
//!
//! This is runner and restoration coverage. The unchanged backend parity cells
//! remain the acceptance test for the original RDTSC divergence.

#![cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    feature = "liteinst-after-loader-experiment"
))]

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use goblin::elf::Elf;
use goblin::elf::header;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tid;
use reverie::Tool;
use reverie::process::Command;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::LiteinstBackend;
use reverie_ptrace::LiteinstAfterLoaderConfig;
use reverie_ptrace::LiteinstCallerDiagnostics;
use reverie_ptrace::LiteinstCallerImage;
use reverie_ptrace::LiteinstCallerObservation;
use sha2::Digest;
use sha2::Sha256;

const RUNTIME_ENV: &str = "REVERIE_LITEINST_AFTER_LOADER_RUNTIME";
const MARKER_ENV: &str = "REVERIE_LITEINST_AFTER_LOADER_MARKER";
const GRAPH_ENV: &str = "REVERIE_LITEINST_AFTER_LOADER_GRAPH";
const MAX_GRAPH_BYTES: usize = 64 * 1024;
const GETPID_SENTINEL: i64 = 0x4c49_5445;

#[derive(Debug, Default)]
struct GetpidCallbacks(AtomicU64);

#[reverie::global_tool]
impl GlobalTool for GetpidCallbacks {
    type Request = ();
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Tid, (): ()) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct CountRawGetpid;

#[reverie::tool]
impl Tool for CountRawGetpid {
    type GlobalState = GetpidCallbacks;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getpid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(syscall.number(), Sysno::getpid);
        guest.send_rpc(()).await;
        Ok(GETPID_SENTINEL)
    }
}

fn compile_fixture(directory: &Path) -> PathBuf {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/after_loader.c");
    let fixture = directory.join("after-loader");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let output = ProcessCommand::new(compiler)
        .args([
            "-std=gnu11",
            "-O0",
            "-fno-pie",
            "-no-pie",
            "-Wl,--build-id=none",
        ])
        .arg(&source)
        .arg("-o")
        .arg(&fixture)
        .output()
        .expect("run C compiler for after-loader fixture");
    assert!(
        output.status.success(),
        "failed to compile {}: status={:?} stdout={:?} stderr={:?}",
        source.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        output.stdout.is_empty() && output.stderr.is_empty(),
        "compiler emitted output for reviewed fixture: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fixture
        .canonicalize()
        .expect("canonicalize compiled after-loader fixture")
}

fn required_staged_file(variable: &str) -> PathBuf {
    let value = std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("{variable} must name one retained staging artifact"));
    let path = PathBuf::from(value);
    assert!(path.is_absolute(), "{variable} must be an absolute path");
    let path = path
        .canonicalize()
        .unwrap_or_else(|error| panic!("canonicalize {variable}: {error}"));
    assert!(path.is_file(), "{variable} does not name a regular file");
    path
}

struct StagedInputs {
    runtime: PathBuf,
    marker: PathBuf,
    graph: PathBuf,
}

fn staged_inputs() -> StagedInputs {
    assert!(
        !cfg!(debug_assertions),
        "after-loader staging evidence must run in Cargo's release profile"
    );
    assert!(
        !cfg!(feature = "preload-constructor"),
        "after-loader staging evidence requires the preload constructor feature to be disabled"
    );
    let inputs = StagedInputs {
        runtime: required_staged_file(RUNTIME_ENV),
        marker: required_staged_file(MARKER_ENV),
        graph: required_staged_file(GRAPH_ENV),
    };
    let distinct = [&inputs.runtime, &inputs.marker, &inputs.graph]
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        distinct.len(),
        3,
        "runtime, marker and graph must be three distinct retained files"
    );
    inputs
}

fn valid_soname(soname: &str) -> bool {
    !soname.is_empty()
        && soname
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

#[derive(Debug)]
struct ManifestImage {
    path: PathBuf,
    digest: String,
}

#[derive(Debug)]
struct GraphManifest {
    executable_digest: String,
    executable_pt_interp: PathBuf,
    provider: String,
    images: BTreeMap<String, ManifestImage>,
}

fn read_graph_manifest(path: &Path) -> GraphManifest {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .expect("open retained after-loader graph")
        .take(MAX_GRAPH_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .expect("read retained after-loader graph");
    assert!(
        !bytes.is_empty() && bytes.len() <= MAX_GRAPH_BYTES,
        "retained after-loader graph is empty or exceeds its byte bound"
    );
    let text = std::str::from_utf8(&bytes).expect("after-loader graph is not canonical UTF-8");
    let body = text
        .strip_suffix('\n')
        .expect("after-loader graph lacks its final newline");
    assert!(
        !body.contains(['\r', '\0']),
        "after-loader graph has noncanonical control bytes"
    );
    let fields = body.split('\n').collect::<Vec<_>>();
    assert!(fields.len() >= 5, "after-loader graph is incomplete");
    assert_eq!(fields[0], "schema=1", "unsupported graph schema");
    let executable_digest = fields[1]
        .strip_prefix("executable_sha256=")
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .expect("graph has no canonical executable SHA-256")
        .to_owned();
    let executable_pt_interp = fields[2]
        .strip_prefix("executable_pt_interp=")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .expect("graph has no absolute executable PT_INTERP");
    let provider = fields[3]
        .strip_prefix("provider=")
        .filter(|provider| valid_soname(provider))
        .expect("graph has no canonical provider SONAME")
        .to_owned();
    let mut images = BTreeMap::new();
    let mut previous = None::<String>;
    for field in &fields[4..] {
        let value = field
            .strip_prefix("image=")
            .expect("graph contains an unknown or misplaced field");
        let parts = value.split('\t').collect::<Vec<_>>();
        assert_eq!(
            parts.len(),
            3,
            "graph image must contain SONAME, canonical path and SHA-256"
        );
        let soname = parts[0];
        assert!(valid_soname(soname), "graph image has malformed SONAME");
        if let Some(previous) = previous.as_deref() {
            assert!(
                previous < soname,
                "graph image lines are duplicated or not strictly sorted"
            );
        }
        previous = Some(soname.to_owned());
        let image_path = PathBuf::from(parts[1]);
        assert!(
            image_path.is_absolute() && image_path.is_file(),
            "graph image does not name one existing absolute file: {image_path:?}"
        );
        assert_eq!(
            image_path
                .canonicalize()
                .expect("canonicalize graph image path"),
            image_path,
            "graph image path is not canonical"
        );
        let digest = parts[2];
        assert!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "graph image has malformed lowercase SHA-256"
        );
        assert!(
            images
                .insert(
                    soname.to_owned(),
                    ManifestImage {
                        path: image_path,
                        digest: digest.to_owned(),
                    },
                )
                .is_none(),
            "graph image SONAME is duplicated"
        );
    }
    assert!(
        !images.is_empty() && images.len() <= 32,
        "graph image count is outside the fixed fixture bound"
    );
    assert!(
        images.contains_key(&provider),
        "graph provider has no bound image"
    );
    GraphManifest {
        executable_digest,
        executable_pt_interp,
        provider,
        images,
    }
}

#[derive(Debug)]
struct ElfContract {
    interpreter: Option<PathBuf>,
    soname: Option<String>,
    needed: BTreeSet<String>,
}

fn elf_contract(path: &Path, expected_type: u16) -> ElfContract {
    let bytes = std::fs::read(path).expect("read ELF for graph validation");
    let elf = Elf::parse(&bytes).expect("parse ELF for graph validation");
    assert!(
        elf.is_64
            && elf.little_endian
            && elf.header.e_machine == header::EM_X86_64
            && elf.header.e_type == expected_type,
        "graph contains an ELF outside the fixed x86-64 contract: {}",
        path.display()
    );
    let mut needed = BTreeSet::new();
    for dependency in elf.libraries {
        assert!(
            valid_soname(dependency) && needed.insert(dependency.to_owned()),
            "ELF has malformed or duplicate DT_NEEDED: {}",
            path.display()
        );
    }
    let soname = elf.soname.map(str::to_owned);
    assert!(
        soname.as_deref().is_none_or(valid_soname),
        "ELF has a malformed DT_SONAME: {}",
        path.display()
    );
    ElfContract {
        interpreter: elf.interpreter.map(PathBuf::from),
        soname,
        needed,
    }
}

fn parse_load_address(text: &str) -> &str {
    let (prefix, address) = text
        .rsplit_once(" (")
        .unwrap_or_else(|| panic!("ldd line lacks a load address: {text:?}"));
    let address = address
        .strip_suffix(')')
        .unwrap_or_else(|| panic!("ldd line has malformed load address: {text:?}"));
    let digits = address
        .strip_prefix("0x")
        .unwrap_or_else(|| panic!("ldd line has non-hex load address: {text:?}"));
    assert!(
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "ldd line has malformed load address: {text:?}"
    );
    prefix
}

fn dynamic_dependencies(image: &Path) -> BTreeMap<String, PathBuf> {
    let output = ProcessCommand::new("ldd")
        .arg(image)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .output()
        .expect("run ldd for fixed after-loader graph");
    assert!(
        output.status.success(),
        "ldd failed for {}: status={:?} stdout={:?} stderr={:?}",
        image.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        output.stderr.is_empty(),
        "ldd emitted diagnostics for {}: {:?}",
        image.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("ldd output is not UTF-8");
    assert!(
        !stdout.is_empty() && stdout.ends_with('\n'),
        "ldd returned an empty or unterminated dependency graph"
    );
    let mut dependencies = BTreeMap::new();
    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        assert!(!line.is_empty(), "ldd emitted an empty graph line");
        let binding = parse_load_address(line);
        if binding == "linux-vdso.so.1" {
            continue;
        }
        let (soname, path) = match binding.split_once(" => ") {
            Some((soname, path)) => {
                assert!(
                    valid_soname(soname),
                    "ldd emitted a malformed soname: {line:?}"
                );
                (soname.to_owned(), PathBuf::from(path))
            }
            None => {
                let path = PathBuf::from(binding);
                let soname = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| valid_soname(name))
                    .expect("ldd interpreter has no canonical SONAME")
                    .to_owned();
                (soname, path)
            }
        };
        assert!(
            path.is_absolute() && path.is_file(),
            "ldd dependency is not one existing absolute file: {line:?}"
        );
        let path = path
            .canonicalize()
            .expect("canonicalize fixed after-loader dependency");
        assert!(
            dependencies.insert(soname.clone(), path).is_none(),
            "ldd emitted duplicate dependency {soname}"
        );
    }
    assert!(
        !dependencies.is_empty(),
        "ldd graph contained only the virtual DSO"
    );
    dependencies
}

fn bind_loader_graph(
    fixture: &Path,
    runtime: &Path,
    graph: &Path,
) -> (LiteinstCallerImage, Vec<LiteinstCallerImage>) {
    let manifest = read_graph_manifest(graph);
    let fixture_bytes = std::fs::read(fixture).expect("read compiled fixed executable");
    assert_eq!(
        format!("{:x}", Sha256::digest(&fixture_bytes)),
        manifest.executable_digest,
        "compiled fixed executable differs from the reviewed graph"
    );
    assert_eq!(
        manifest.provider, "libc.so.6",
        "fixed runner provider must be libc.so.6"
    );
    let fixture_contract = elf_contract(fixture, header::ET_EXEC);
    let runtime_contract = elf_contract(runtime, header::ET_DYN);
    assert!(
        runtime_contract.interpreter.is_none(),
        "staged runtime unexpectedly has PT_INTERP"
    );
    assert_eq!(
        fixture_contract.interpreter.as_ref(),
        Some(&manifest.executable_pt_interp),
        "compiled fixture PT_INTERP differs from the retained graph"
    );
    let interpreter_soname = manifest
        .executable_pt_interp
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| valid_soname(name))
        .expect("retained PT_INTERP has no canonical SONAME")
        .to_owned();
    let interpreter_path = manifest
        .executable_pt_interp
        .canonicalize()
        .expect("canonicalize retained executable PT_INTERP");
    assert_eq!(
        manifest
            .images
            .get(&interpreter_soname)
            .map(|image| &image.path),
        Some(&interpreter_path),
        "retained PT_INTERP is absent from or differs in the bound graph"
    );

    let mut observed = dynamic_dependencies(fixture);
    for (soname, path) in dynamic_dependencies(runtime) {
        if let Some(previous) = observed.insert(soname.clone(), path.clone()) {
            assert_eq!(
                previous, path,
                "the fixed graphs resolve {soname} to different files"
            );
        }
    }
    assert_eq!(
        observed.len(),
        manifest.images.len(),
        "resolved dependency count differs from the retained graph: observed={observed:?} manifest={manifest:?}"
    );
    for (soname, image) in &manifest.images {
        assert_eq!(
            observed.get(soname),
            Some(&image.path),
            "resolved path for {soname} differs from the retained graph"
        );
    }

    let mut pending = fixture_contract.needed.clone();
    pending.extend(runtime_contract.needed);
    pending.insert(interpreter_soname);
    let mut reachable = BTreeSet::new();
    while let Some(soname) = pending.pop_first() {
        if !reachable.insert(soname.clone()) {
            continue;
        }
        let image = manifest
            .images
            .get(&soname)
            .unwrap_or_else(|| panic!("dependency closure lacks {soname}"));
        let contract = elf_contract(&image.path, header::ET_DYN);
        if let Some(interpreter) = contract.interpreter {
            assert_eq!(
                interpreter, manifest.executable_pt_interp,
                "dependency image {soname} names a different PT_INTERP"
            );
        }
        assert_eq!(
            contract.soname.as_deref(),
            Some(soname.as_str()),
            "dependency image DT_SONAME differs from graph key {soname}"
        );
        pending.extend(contract.needed);
    }
    assert_eq!(
        reachable,
        manifest.images.keys().cloned().collect(),
        "retained graph contains missing or unreachable dependency images"
    );

    let mut bound = BTreeMap::new();
    for (soname, image) in manifest.images {
        let before = std::fs::read(&image.path).expect("read graph image before binding");
        assert_eq!(
            format!("{:x}", Sha256::digest(&before)),
            image.digest,
            "SHA-256 differs for graph image {soname}"
        );
        let caller_image = LiteinstCallerImage::read(&image.path)
            .unwrap_or_else(|error| panic!("bind graph image {soname}: {error}"));
        assert_eq!(
            std::fs::read(&image.path).expect("read graph image after binding"),
            before,
            "graph image {soname} changed while it was bound"
        );
        bound.insert(soname, caller_image);
    }
    let provider = bound.remove(&manifest.provider).unwrap();
    let dependencies = bound.into_values().collect();
    (provider, dependencies)
}

fn is_lower_hex(text: &str, width: usize) -> bool {
    text.len() == width
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_sample(line: &str, label: &str) -> ((i64, u32), String, String) {
    let rest = line
        .strip_prefix(&format!("{label} time="))
        .unwrap_or_else(|| panic!("missing {label} sample prefix: {line:?}"));
    let (time, rest) = rest
        .split_once(" canary=")
        .unwrap_or_else(|| panic!("missing {label} canary: {line:?}"));
    let (seconds, nanoseconds) = time
        .split_once('.')
        .unwrap_or_else(|| panic!("malformed {label} timestamp: {line:?}"));
    assert_eq!(
        nanoseconds.len(),
        9,
        "noncanonical {label} nanoseconds: {line:?}"
    );
    let seconds = seconds
        .parse::<i64>()
        .unwrap_or_else(|_| panic!("malformed {label} seconds: {line:?}"));
    let nanoseconds = nanoseconds
        .parse::<u32>()
        .unwrap_or_else(|_| panic!("malformed {label} nanoseconds: {line:?}"));
    assert!(nanoseconds < 1_000_000_000, "invalid timestamp: {line:?}");
    let (canary, random) = rest
        .split_once(" random=")
        .unwrap_or_else(|| panic!("missing {label} random bytes: {line:?}"));
    assert!(is_lower_hex(canary, 16), "malformed canary: {line:?}");
    assert!(is_lower_hex(random, 32), "malformed random bytes: {line:?}");
    ((seconds, nanoseconds), canary.to_owned(), random.to_owned())
}

fn assert_fixture_stdout(stdout: &[u8]) {
    let stdout = std::str::from_utf8(stdout).expect("fixture stdout is not UTF-8");
    assert!(stdout.ends_with('\n'), "fixture stdout is unterminated");
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 8, "unexpected fixture stdout: {stdout:?}");
    let samples = ["preinit", "constructor", "main"]
        .into_iter()
        .enumerate()
        .map(|(index, label)| parse_sample(lines[index], label))
        .collect::<Vec<_>>();
    assert!(
        samples.windows(2).all(|pair| pair[0].0 <= pair[1].0),
        "CLOCK_MONOTONIC went backwards in fixture output: {samples:?}"
    );
    assert!(
        samples.windows(2).all(|pair| pair[0].1 == pair[1].1),
        "stack canary changed in fixture output: {samples:?}"
    );
    assert!(
        samples.windows(2).all(|pair| pair[0].2 == pair[1].2),
        "AT_RANDOM changed in fixture output: {samples:?}"
    );
    assert_eq!(lines[3], "env LITEINST_CALLER_SENTINEL=preserved");
    assert_eq!(lines[4], "env LD_PRELOAD=<absent>");
    assert_eq!(lines[5], "env REVERIE_LITEINST_HOST_RUNTIME=<absent>");
    assert_eq!(lines[6], "env REVERIE_LITEINST_TOOL=<absent>");
    assert_eq!(lines[7], "getpid=4c495445 stages=3");
}

fn one_observation<'a>(
    observations: &'a [LiteinstCallerObservation],
    operation: &str,
    diagnostics: &LiteinstCallerDiagnostics,
) -> (usize, &'a LiteinstCallerObservation) {
    let matches = observations
        .iter()
        .enumerate()
        .filter(|(_, observation)| observation.operation == operation)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "missing or duplicate {operation:?} evidence: {}",
        bounded_diagnostics(diagnostics)
    );
    matches[0]
}

fn register_snapshot(detail: &str) -> (Vec<u64>, &str) {
    let detail = detail
        .strip_prefix("regs=[")
        .expect("register snapshot lacks register prefix");
    let (registers, xstate) = detail
        .split_once("] xstate=")
        .expect("register snapshot lacks XSTATE");
    let registers = registers
        .split(", ")
        .map(|word| word.parse::<u64>().expect("malformed register word"))
        .collect::<Vec<_>>();
    assert_eq!(registers.len(), 27, "register snapshot is incomplete");
    assert!(
        xstate.starts_with("XState([") && xstate.ends_with("])"),
        "XSTATE snapshot is malformed"
    );
    (registers, xstate)
}

fn diagnostic_field<'a>(detail: &'a str, prefix: &str) -> &'a str {
    let matches = detail
        .split_ascii_whitespace()
        .filter_map(|field| field.strip_prefix(prefix))
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "diagnostic field {prefix:?} is missing or duplicated: {detail:?}"
    );
    matches[0]
}

fn loader_phase_counts(
    observations: &[LiteinstCallerObservation],
    diagnostics: &LiteinstCallerDiagnostics,
) -> (usize, usize) {
    let (_, phases) = one_observation(observations, "loader dependency phases bound", diagnostics);
    let initial = diagnostic_field(&phases.detail, "initial_count=")
        .parse::<usize>()
        .expect("loader initial_count is not canonical decimal");
    let deferred = diagnostic_field(&phases.detail, "deferred_count=")
        .parse::<usize>()
        .expect("loader deferred_count is not canonical decimal");
    assert_eq!(
        phases.detail.matches("initial_paths=").count(),
        1,
        "loader initial path list is missing or duplicated: {phases:?}"
    );
    assert_eq!(
        phases.detail.matches("deferred_paths=").count(),
        1,
        "loader deferred path list is missing or duplicated: {phases:?}"
    );
    (initial, deferred)
}

fn parse_hex_word(value: &str, label: &str) -> u64 {
    let digits = value
        .strip_prefix("0x")
        .unwrap_or_else(|| panic!("{label} lacks its hexadecimal prefix: {value:?}"));
    assert!(!digits.is_empty(), "{label} is empty: {value:?}");
    u64::from_str_radix(digits, 16)
        .unwrap_or_else(|_| panic!("{label} is not canonical hexadecimal: {value:?}"))
}

fn image_geometry_identity(detail: &str) -> (&str, (u64, u64)) {
    let phase = diagnostic_field(detail, "phase=");
    assert!(
        matches!(phase, "initial-entry" | "post-dlopen-deferred"),
        "bound image diagnostic has an unknown phase: {detail:?}"
    );
    let device = diagnostic_field(detail, "file_device=")
        .parse::<u64>()
        .expect("bound image file device is not canonical decimal");
    let inode = diagnostic_field(detail, "file_inode=")
        .parse::<u64>()
        .expect("bound image file inode is not canonical decimal");
    let mapping_device = diagnostic_field(detail, "mapping_device=");
    let (mapping_major, mapping_minor) = mapping_device
        .split_once(':')
        .expect("bound image mapping device lacks its separator");
    assert!(
        !mapping_major.is_empty()
            && !mapping_minor.is_empty()
            && mapping_major.bytes().all(|byte| byte.is_ascii_hexdigit())
            && mapping_minor.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "bound image mapping device is not hexadecimal: {mapping_device:?}"
    );
    u64::from_str_radix(mapping_major, 16).expect("mapping device major overflows u64");
    u64::from_str_radix(mapping_minor, 16).expect("mapping device minor overflows u64");
    diagnostic_field(detail, "mapping_inode=")
        .parse::<u64>()
        .expect("bound image mapping inode is not canonical decimal");
    parse_hex_word(
        diagnostic_field(detail, "load_bias="),
        "bound image load bias",
    );
    let span = diagnostic_field(detail, "span=");
    let (span_start, span_end) = span
        .split_once('-')
        .expect("bound image span lacks its separator");
    let span_start = parse_hex_word(span_start, "bound image span start");
    let span_end = parse_hex_word(span_end, "bound image span end");
    assert!(
        span_start < span_end,
        "bound image span is empty or reversed: {detail:?}"
    );
    (phase, (device, inode))
}

fn physical_status_id(detail: &str) -> u64 {
    let value = diagnostic_field(detail, "physical_status=");
    let id = value
        .strip_prefix("Some(PhysicalStatusId(")
        .and_then(|value| value.strip_suffix("))"))
        .unwrap_or_else(|| panic!("ordinary event lacks a physical status ID: {detail:?}"))
        .parse::<u64>()
        .expect("ordinary event physical status ID is not canonical decimal");
    assert_ne!(id, 0, "ordinary event physical status ID is zero");
    id
}

fn assert_restoration_diagnostics(diagnostics: &LiteinstCallerDiagnostics) {
    let observations = diagnostics.observations();
    let (initial_dependencies, deferred_dependencies) =
        loader_phase_counts(&observations, diagnostics);
    let image_geometries = observations
        .iter()
        .filter(|observation| observation.operation == "bound target image geometry")
        .collect::<Vec<_>>();
    assert_eq!(
        image_geometries.len(),
        initial_dependencies + deferred_dependencies + 2,
        "target image geometry count differs from executable, provider and exact dependency phases: {}",
        bounded_diagnostics(diagnostics)
    );
    assert_eq!(
        image_geometries
            .iter()
            .filter(|observation| image_geometry_identity(&observation.detail).0 == "initial-entry")
            .count(),
        initial_dependencies + 2,
        "initial image geometry count differs from the exact loader phase: {}",
        bounded_diagnostics(diagnostics)
    );
    assert_eq!(
        image_geometries
            .iter()
            .filter(|observation| {
                image_geometry_identity(&observation.detail).0 == "post-dlopen-deferred"
            })
            .count(),
        deferred_dependencies,
        "deferred image geometry count differs from the exact loader phase: {}",
        bounded_diagnostics(diagnostics)
    );
    let mut image_identities = BTreeSet::new();
    for observation in &image_geometries {
        let (_, identity) = image_geometry_identity(&observation.detail);
        assert!(
            image_identities.insert(identity),
            "target image geometry is duplicated by file identity: {observation:?}"
        );
    }
    let (deferred_absent_index, deferred_absent) = one_observation(
        &observations,
        "deferred target image absent before dlopen",
        diagnostics,
    );
    assert!(
        deferred_absent.detail.contains("libgcc_s.so.1")
            && deferred_absent.detail.contains("mapping_device=")
            && deferred_absent.detail.contains("mapping_inode="),
        "deferred absence evidence is incomplete: {deferred_absent:?}"
    );
    let deferred_bound_index = observations
        .iter()
        .position(|observation| {
            observation.operation == "bound target image geometry"
                && image_geometry_identity(&observation.detail).0 == "post-dlopen-deferred"
        })
        .expect("deferred dependency was never bound");
    assert!(
        deferred_absent_index < deferred_bound_index,
        "deferred dependency was not proven absent before it was bound"
    );
    let (_, runtime_geometry) =
        one_observation(&observations, "bound runtime image geometry", diagnostics);
    for field in [
        "file_device=",
        "file_inode=",
        "mapping_device=",
        "mapping_inode=",
        "load_bias=",
        "span=",
    ] {
        assert!(
            runtime_geometry.detail.contains(field),
            "bound runtime diagnostic lacks {field:?}: {runtime_geometry:?}"
        );
    }
    let (entry_index, entry) = one_observation(&observations, "entry held", diagnostics);
    let (_, entry_machine) =
        one_observation(&observations, "entry registers and XSTATE", diagnostics);
    let (_, restored_machine) = one_observation(
        &observations,
        "restored entry registers and XSTATE",
        diagnostics,
    );
    let (restored_index, restored) = one_observation(
        &observations,
        "guest machine state restored and helper isolated",
        diagnostics,
    );
    let (ordinary_index, ordinary) = one_observation(
        &observations,
        "first ordinary guest event after restoration",
        diagnostics,
    );
    assert!(
        entry_index < restored_index && restored_index < ordinary_index,
        "restoration diagnostics are out of order: {}",
        bounded_diagnostics(diagnostics)
    );
    assert!(
        restored.detail.starts_with("retained dlopen handle=0x"),
        "state-restoration evidence lacks the retained runtime: {restored:?}"
    );
    diagnostic_field(&ordinary.detail, "tid=")
        .parse::<i32>()
        .expect("ordinary event TID is not canonical decimal");
    diagnostic_field(&ordinary.detail, "generation=")
        .parse::<u64>()
        .expect("ordinary event generation is not canonical decimal");
    physical_status_id(&ordinary.detail);
    assert_eq!(
        diagnostic_field(&ordinary.detail, "event="),
        "Seccomp",
        "first ordinary guest event is not the expected seccomp stop"
    );

    let frozen_clock = entry
        .raw_clock
        .expect("entry diagnostic lacks the persistent ptrace clock");
    for observation in &observations[entry_index..=restored_index] {
        assert_eq!(
            observation.raw_clock,
            Some(frozen_clock),
            "persistent ptrace clock changed during private execution at {:?}",
            observation.operation
        );
    }
    assert!(
        ordinary
            .raw_clock
            .is_some_and(|clock| clock >= frozen_clock),
        "first ordinary event has no clock at or after restoration"
    );

    let (mut entry_registers, entry_xstate) = register_snapshot(&entry_machine.detail);
    let (restored_registers, restored_xstate) = register_snapshot(&restored_machine.detail);
    assert_eq!(
        entry_xstate, restored_xstate,
        "XSTATE changed across activation"
    );
    assert_eq!(
        entry_registers[21], restored_registers[21],
        "FS base changed across activation"
    );
    assert_eq!(
        entry_registers[16],
        restored_registers[16] + 1,
        "restored instruction pointer does not precede the entry INT3 stop"
    );
    entry_registers[16] = restored_registers[16];
    assert_eq!(
        entry_registers, restored_registers,
        "register state changed across activation"
    );
    for suffix in [
        "original stack",
        "random and canary",
        "signals and descriptors",
    ] {
        let (_, before) = one_observation(&observations, &format!("entry {suffix}"), diagnostics);
        let (_, after) = one_observation(
            &observations,
            &format!("restored entry {suffix}"),
            diagnostics,
        );
        assert_eq!(
            before.detail, after.detail,
            "{suffix} changed across activation"
        );
    }

    let seccomp_callbacks = observations
        .iter()
        .filter(|observation| {
            observation.operation == "Tool callback: Tool::handle_syscall_event(seccomp)"
        })
        .count();
    let installed_callbacks = observations
        .iter()
        .filter(|observation| {
            observation.operation
                == "Tool callback: Tool::handle_syscall_event(installed completion)"
        })
        .count();
    assert_eq!(
        seccomp_callbacks,
        1,
        "the aligned getpid site did not install exactly once: {}",
        bounded_diagnostics(diagnostics)
    );
    assert_eq!(
        installed_callbacks,
        3,
        "the installed getpid site did not take exactly three direct-hook callbacks: {}",
        bounded_diagnostics(diagnostics)
    );
    assert_eq!(seccomp_callbacks + installed_callbacks, 4);
}

fn bounded_diagnostics(diagnostics: &LiteinstCallerDiagnostics) -> String {
    let observations = diagnostics.observations();
    let first = observations.len().saturating_sub(32);
    let mut summary = format!(
        "{} retained observations; showing [{}..{}]",
        observations.len(),
        first,
        observations.len()
    );
    for (index, observation) in observations.iter().enumerate().skip(first) {
        let detail_limit = if observation.operation == "physical event partition failure tail" {
            20 * 1024
        } else {
            256
        };
        let detail = observation
            .detail
            .chars()
            .take(detail_limit)
            .collect::<String>();
        summary.push_str(&format!(
            "\n{index}: operation={:?} raw_clock={:?} detail_bytes={} detail_prefix={detail:?}",
            observation.operation,
            observation.raw_clock,
            observation.detail.len(),
        ));
    }
    summary
}

#[test]
fn staged_union_graph_partitions_initial_and_dlopen_dependencies() {
    let staged = staged_inputs();
    let directory = tempfile::tempdir().expect("create after-loader graph test directory");
    let fixture = compile_fixture(directory.path());
    let executable = LiteinstCallerImage::read(&fixture).expect("bind exact fixture executable");
    let runtime_image = LiteinstCallerImage::read_runtime(&staged.runtime, &staged.marker)
        .expect("bind constructor-disabled runtime and marker");
    let (provider, dependencies) = bind_loader_graph(&fixture, &staged.runtime, &staged.graph);
    let environment = BTreeMap::from([(
        OsString::from("LITEINST_CALLER_SENTINEL"),
        OsString::from("preserved"),
    )]);
    // SAFETY: identical fixed staging contract to the runner below; this test
    // only constructs and inspects the configuration and never starts a guest.
    let caller = unsafe {
        LiteinstAfterLoaderConfig::new(
            executable,
            provider,
            runtime_image,
            dependencies,
            environment,
        )
    }
    .expect("partition fixed after-loader dependency graph");
    let diagnostics = caller.diagnostics();
    let observations = diagnostics.observations();
    assert_eq!(
        loader_phase_counts(&observations, &diagnostics),
        (1, 1),
        "fixed loader graph has a different initial/deferred partition"
    );
    let (_, phases) = one_observation(
        &observations,
        "loader dependency phases bound",
        &diagnostics,
    );
    assert_eq!(
        phases
            .detail
            .matches("(Some(\"ld-linux-x86-64.so.2\"),")
            .count(),
        1,
        "initial loader identity is missing or duplicated: {phases:?}"
    );
    assert_eq!(
        phases.detail.matches("(Some(\"libgcc_s.so.1\"),").count(),
        1,
        "deferred loader identity is missing or duplicated: {phases:?}"
    );
    assert_eq!(
        phases.detail.matches("(Some(\"").count(),
        2,
        "loader phase lists contain an unexpected image: {phases:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn constructor_disabled_runner_restores_guest_before_raw_getpid_callbacks() {
    let staged = staged_inputs();
    let directory = tempfile::tempdir().expect("create after-loader test directory");
    let fixture = compile_fixture(directory.path());

    let executable = LiteinstCallerImage::read(&fixture).expect("bind exact fixture executable");
    let runtime_image = LiteinstCallerImage::read_runtime(&staged.runtime, &staged.marker)
        .expect("bind constructor-disabled runtime and marker");
    let (provider, dependencies) = bind_loader_graph(&fixture, &staged.runtime, &staged.graph);
    let environment = BTreeMap::from([(
        OsString::from("LITEINST_CALLER_SENTINEL"),
        OsString::from("preserved"),
    )]);
    // SAFETY: this test owns the compiled fixed executable, constructor-disabled
    // runtime, exact ldd-resolved loader graph and complete one-entry environment.
    // It supplies no preload, audit module or guest interposer.
    let caller = unsafe {
        LiteinstAfterLoaderConfig::new(
            executable,
            provider,
            runtime_image,
            dependencies,
            environment.clone(),
        )
    }
    .expect("bind fixed after-loader caller configuration");
    let diagnostics = caller.diagnostics();
    let mut command = Command::new(&fixture);
    command.env_clear().envs(environment);

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        LiteinstBackend::run_host_with_output_after_loader::<CountRawGetpid>(
            command,
            (),
            &staged.runtime,
            caller,
        ),
    )
    .await;
    let (output, global) = match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!(
            "after-loader runner failed: {error}; {}",
            bounded_diagnostics(&diagnostics)
        ),
        Err(_) => panic!(
            "after-loader runner timed out; {}",
            bounded_diagnostics(&diagnostics)
        ),
    };

    assert_eq!(output.status, ExitStatus::Exited(0), "{output:?}");
    assert!(output.stderr.is_empty(), "unexpected stderr: {output:?}");
    assert_fixture_stdout(&output.stdout);
    assert_eq!(
        global.0.load(Ordering::SeqCst),
        4,
        "Tool did not receive exactly the fixture's four raw getpid calls"
    );
    assert_restoration_diagnostics(&diagnostics);
}
