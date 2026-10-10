//! Shared, fail-closed diagnostic artifact qualification. No Cargo invocation is
//! implicit in consumer verification. Producer commands are bounded separately
//! from the enclosing validation DAG's CPU/memory/PID containment.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::fs::{self};
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use goblin::elf::Elf;
use goblin::elf::header;
use goblin::elf::program_header;
use goblin::elf::reloc;
use goblin::elf::section_header;
use goblin::elf::sym;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;

pub type Result<T> = std::result::Result<T, String>;
pub const SCHEMA: u32 = 2;
pub const FROZEN_RUST_ORACLE: &str =
    "d85c3094519855dde18571047b945aeca112b8e4df2138cff83fc33bfba0bce8";
pub const FROZEN_C_ORACLE: &str =
    "e21fd8768d61edc7d14f51b98e2c5f9087fd4ddcfa08a19a2ca646b6df912b2e";
pub const EXPORTS: [&str; 5] = [
    "m1_alloc",
    "m1_realloc",
    "m1_dealloc",
    "m1_query",
    "m1_probe_private",
];
pub const SHIMS: [&str; 4] = [
    "__rust_alloc",
    "__rust_alloc_zeroed",
    "__rust_realloc",
    "__rust_dealloc",
];
const ALLOWED_NEEDED: [&str; 7] = [
    "ld-linux-x86-64.so.2",
    "libc.so.6",
    "libm.so.6",
    "libdl.so.2",
    "libpthread.so.0",
    "librt.so.1",
    "libutil.so.1",
];

fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn file_sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| format!("hash {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn real_file(path: &Path) -> Result<PathBuf> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("metadata {}: {e}", path.display()))?;
    require(
        metadata.file_type().is_file(),
        format!("not a real regular file: {}", path.display()),
    )?;
    fs::canonicalize(path).map_err(|e| format!("canonicalize {}: {e}", path.display()))
}

pub fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("sync {}: {e}", path.display()))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub path: PathBuf,
    pub sha256: String,
}

impl FileIdentity {
    pub fn read(path: &Path) -> Result<Self> {
        let path = real_file(path)?;
        let sha256 = file_sha256(&path)?;
        Ok(Self { path, sha256 })
    }
    pub fn verify(&self) -> Result<()> {
        require(
            Self::read(&self.path)? == *self,
            format!("file identity changed: {}", self.path.display()),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEvidence {
    pub program: String,
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
    pub status: i32,
    pub elapsed_millis: u128,
    pub stdout: FileIdentity,
    pub stderr: FileIdentity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestReceipt {
    pub schema_version: u32,
    pub source: FileIdentity,
    pub binary: FileIdentity,
    pub compiler_version: String,
    pub command: CommandEvidence,
    pub executed_tests: u64,
}

pub fn verify_guest_receipt(
    receipt_path: &Path,
    binary_path: &Path,
    expected_source: &Path,
    expected_source_hash: &str,
) -> Result<GuestReceipt> {
    let bytes = fs::read(real_file(receipt_path)?).map_err(|e| e.to_string())?;
    let receipt: GuestReceipt =
        serde_json::from_slice(&bytes).map_err(|e| format!("C guest receipt: {e}"))?;
    require(
        receipt.schema_version == 1 && receipt.executed_tests == 0 && receipt.command.status == 0,
        "C guest is not a successful source-bound producer receipt",
    )?;
    require(
        receipt.source.path == real_file(expected_source)?
            && receipt.source.sha256 == expected_source_hash,
        "C guest frozen source identity differs",
    )?;
    require(
        receipt.binary.path == real_file(binary_path)?,
        "C guest selected path differs",
    )?;
    receipt.source.verify()?;
    receipt.binary.verify()?;
    receipt.command.stdout.verify()?;
    receipt.command.stderr.verify()?;
    let expected_arguments = vec![
        "-std=c11".to_owned(),
        "-O0".to_owned(),
        "-fno-builtin".to_owned(),
        "-fno-lto".to_owned(),
        "-Wl,--export-dynamic".to_owned(),
        "-Wl,-z,now".to_owned(),
        receipt
            .source
            .path
            .to_str()
            .ok_or("C source path is not UTF-8")?
            .to_owned(),
        "-o".to_owned(),
        receipt
            .binary
            .path
            .to_str()
            .ok_or("C binary path is not UTF-8")?
            .to_owned(),
        "-ldl".to_owned(),
    ];
    require(
        receipt.command.arguments == expected_arguments,
        "C guest compile recipe differs from fixed source/output/flags",
    )?;
    Ok(receipt)
}

/// One shared absolute deadline; each stream has its own strict byte ceiling.
/// Every child is placed in a fresh process group. Overflow/deadline kills that
/// group; the enclosing DAG cgroup remains the authoritative descendant guard.
pub struct Runner {
    pub logs: PathBuf,
    pub deadline: Instant,
    pub max_stream_bytes: usize,
    serial: u32,
    pub commands: Vec<CommandEvidence>,
}

impl Runner {
    pub fn new(logs: PathBuf, budget: Duration, max_stream_bytes: usize) -> Result<Self> {
        require(
            budget.as_secs() > 0 && max_stream_bytes > 0,
            "command bounds must be positive",
        )?;
        fs::create_dir(&logs).map_err(|e| format!("create unique log directory: {e}"))?;
        Ok(Self {
            logs,
            deadline: Instant::now() + budget,
            max_stream_bytes,
            serial: 0,
            commands: Vec::new(),
        })
    }

    pub fn run(&mut self, program: &str, args: &[String], cwd: &Path) -> Result<Vec<u8>> {
        self.run_with_combined_limit(program, args, cwd, None)
    }

    fn run_with_combined_limit(
        &mut self,
        program: &str,
        args: &[String],
        cwd: &Path,
        combined_limit: Option<usize>,
    ) -> Result<Vec<u8>> {
        require(
            combined_limit != Some(0),
            "command aggregate byte limit must be positive",
        )?;
        require(
            Instant::now() < self.deadline,
            "producer command deadline exhausted",
        )?;
        self.serial += 1;
        let prefix = self.logs.join(format!("{:04}", self.serial));
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Do not let a diagnostic producer inherit runtime selectors into Cargo,
        // linker/tools or readelf; these only belong on the guest test launch.
        for name in [
            "LD_PRELOAD",
            "HERMIT_LITEINST_STAGE",
            "HERMIT_LITEINST_TOOL_RUNTIME",
            "REVERIE_LITEINST_PRELOAD",
            "REVERIE_LITEINST_TEST_RUNTIME_MANIFEST",
        ] {
            command.env_remove(name);
        }
        command.env("LC_ALL", "C").process_group(0);
        let start = Instant::now();
        let mut child = command
            .spawn()
            .map_err(|e| format!("spawn {program}: {e}"))?;
        let overflow = Arc::new(AtomicBool::new(false));
        let combined_bytes = Arc::new(AtomicUsize::new(0));
        let byte_limit_description = if combined_limit.is_some() {
            "command stream/aggregate byte limit"
        } else {
            "command stream byte limit"
        };
        let stop_reading = Arc::new(AtomicBool::new(false));
        let mut readers = Vec::new();
        for fd in [
            child.stdout.as_ref().unwrap().as_raw_fd(),
            child.stderr.as_ref().unwrap().as_raw_fd(),
        ] {
            // SAFETY: this child owns these valid pipe descriptors.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                return Err("cannot make producer pipes nonblocking".to_owned());
            }
        }
        let pipes: Vec<Box<dyn Read + Send>> = vec![
            Box::new(child.stdout.take().unwrap()),
            Box::new(child.stderr.take().unwrap()),
        ];
        for mut pipe in pipes {
            let overflow = Arc::clone(&overflow);
            let combined_bytes = Arc::clone(&combined_bytes);
            let stop_reading = Arc::clone(&stop_reading);
            let cap = self.max_stream_bytes;
            readers.push(thread::spawn(move || -> Result<Vec<u8>> {
                let mut kept = Vec::new();
                let mut bytes = [0u8; 8192];
                let mut drain_started = None;
                loop {
                    if stop_reading.load(Ordering::Acquire) {
                        let at = drain_started.get_or_insert_with(Instant::now);
                        if at.elapsed() >= Duration::from_millis(250) {
                            overflow.store(true, Ordering::Release);
                            break;
                        }
                    }
                    let n = match pipe.read(&mut bytes) {
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            if stop_reading.load(Ordering::Acquire) {
                                break;
                            }
                            thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(format!("read child stream: {e}")),
                    };
                    if n == 0 {
                        break;
                    }
                    let combined_room = if let Some(limit) = combined_limit {
                        let mut before = combined_bytes.load(Ordering::Acquire);
                        loop {
                            match combined_bytes.compare_exchange_weak(
                                before,
                                before.saturating_add(n),
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            ) {
                                Ok(_) => break limit.saturating_sub(before),
                                Err(observed) => before = observed,
                            }
                        }
                    } else {
                        usize::MAX
                    };
                    let room = cap.saturating_sub(kept.len()).min(combined_room);
                    kept.extend_from_slice(&bytes[..n.min(room)]);
                    if n > room {
                        overflow.store(true, Ordering::Release);
                    }
                }
                Ok(kept)
            }));
        }
        let mut bounds_failure = None;
        let mut killed_at = None;
        let status = loop {
            if killed_at.is_none()
                && (overflow.load(Ordering::Acquire) || Instant::now() >= self.deadline)
            {
                bounds_failure = Some(if overflow.load(Ordering::Acquire) {
                    byte_limit_description
                } else {
                    "producer deadline"
                });
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                killed_at = Some(Instant::now());
            }
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // WNOWAIT reserves the leader PID until killpg; reaping first could
            // target another job if the group identity were reused.
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if rc < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                bounds_failure =
                    Some("producer waitid failed; enclosing cgroup must reclaim child");
                break std::process::ExitStatus::from_raw(libc::SIGKILL);
            }
            if unsafe { info.si_pid() } != 0 {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                break child
                    .wait()
                    .map_err(|e| format!("reap exited {program}: {e}"))?;
            }
            if killed_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(1)) {
                bounds_failure =
                    Some("producer cleanup deadline; enclosing cgroup must reclaim child");
                break std::process::ExitStatus::from_raw(libc::SIGKILL);
            }
            thread::sleep(Duration::from_millis(50));
        };
        stop_reading.store(true, Ordering::Release);
        let stdout = readers
            .remove(0)
            .join()
            .map_err(|_| "stdout reader panicked")??;
        let stderr = readers
            .remove(0)
            .join()
            .map_err(|_| "stderr reader panicked")??;
        if overflow.load(Ordering::Acquire) {
            bounds_failure = Some(byte_limit_description);
        }
        let stdout_path = prefix.with_extension("stdout");
        let stderr_path = prefix.with_extension("stderr");
        write_new(&stdout_path, &stdout)?;
        write_new(&stderr_path, &stderr)?;
        let evidence = CommandEvidence {
            program: program.to_owned(),
            arguments: args.to_vec(),
            cwd: fs::canonicalize(cwd).map_err(|e| e.to_string())?,
            status: status.code().unwrap_or(-1),
            elapsed_millis: start.elapsed().as_millis(),
            stdout: FileIdentity::read(&stdout_path)?,
            stderr: FileIdentity::read(&stderr_path)?,
        };
        write_new(
            &prefix.with_extension("command.json"),
            &serde_json::to_vec_pretty(&evidence).map_err(|e| e.to_string())?,
        )?;
        self.commands.push(evidence);
        require(
            bounds_failure.is_none(),
            format!(
                "{program} exceeded {}; logs {}",
                bounds_failure.unwrap_or(""),
                prefix.display()
            ),
        )?;
        require(
            status.success(),
            format!("{program} failed ({status}); logs {}", prefix.display()),
        )?;
        Ok(stdout)
    }

    pub fn text(&mut self, program: &str, args: &[&str], cwd: &Path) -> Result<String> {
        let args: Vec<_> = args.iter().map(|s| (*s).to_owned()).collect();
        String::from_utf8(self.run(program, &args, cwd)?)
            .map_err(|e| format!("{program} output is not UTF-8: {e}"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub root: PathBuf,
    pub head: String,
    pub tree: String,
    pub status: String,
    pub lock: FileIdentity,
    /// Includes both unchanged Rust export and fixed C workload sources.
    pub oracles: BTreeMap<String, FileIdentity>,
}

pub fn source_identity(
    root: &Path,
    lock: &Path,
    oracles: &BTreeMap<String, PathBuf>,
    runner: &mut Runner,
) -> Result<SourceIdentity> {
    let root = fs::canonicalize(root).map_err(|e| format!("source root: {e}"))?;
    let observed_root = runner.text("git", &["rev-parse", "--show-toplevel"], &root)?;
    require(
        Path::new(observed_root.trim()) == root,
        "source root must be the exact Git worktree root",
    )?;
    let head = runner
        .text("git", &["rev-parse", "HEAD"], &root)?
        .trim()
        .to_owned();
    let tree = runner
        .text("git", &["rev-parse", "HEAD^{tree}"], &root)?
        .trim()
        .to_owned();
    require(
        head.len() == 40
            && tree.len() == 40
            && head
                .bytes()
                .chain(tree.bytes())
                .all(|b| b.is_ascii_hexdigit()),
        "source is not a 40-hex commit/tree",
    )?;
    let status = runner.text(
        "git",
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
        &root,
    )?;
    require(
        status.is_empty(),
        format!("source must be clean before/after build: {status}"),
    )?;
    let lock = FileIdentity::read(lock)?;
    require(
        lock.path.starts_with(&root),
        "lock must be inside source worktree",
    )?;
    require(
        oracles.contains_key("rust_exports") && oracles.contains_key("c_workload"),
        "both fixed oracle source identities are required",
    )?;
    let oracles = oracles
        .iter()
        .map(|(label, path)| Ok((label.clone(), FileIdentity::read(path)?)))
        .collect::<Result<_>>()?;
    Ok(SourceIdentity {
        root,
        head,
        tree,
        status,
        lock,
        oracles,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIdentity {
    pub id: String,
    pub name: String,
    pub manifest: PathBuf,
    pub source: Option<String>,
    pub target: String,
    pub kind: Vec<String>,
    pub crate_types: Vec<String>,
    pub target_source: PathBuf,
}

/// Explicit expected manifest/source prevents selecting a lookalike workspace
/// package or wrong Cargo Git checkout with the same name. H passes the resolved
/// exact-pin Git package manifest and full metadata source string here.
pub fn select_package(
    metadata: &Value,
    name: &str,
    target_name: &str,
    expected_manifest: &Path,
    expected_source: Option<&str>,
) -> Result<PackageIdentity> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("metadata has no packages")?;
    let candidates: Vec<_> = packages.iter().filter(|p| p["name"] == name).collect();
    require(
        candidates.len() == 1,
        format!("expected exactly one metadata package {name}"),
    )?;
    let p = candidates[0];
    let manifest = real_file(Path::new(
        p["manifest_path"]
            .as_str()
            .ok_or("package manifest missing")?,
    ))?;
    require(
        manifest == real_file(expected_manifest)?,
        "selected package manifest differs from required checkout",
    )?;
    let source = p["source"].as_str().map(str::to_owned);
    require(
        source.as_deref() == expected_source,
        "selected Cargo package source differs from expected exact pin",
    )?;
    let targets = p["targets"]
        .as_array()
        .ok_or("metadata package has no targets")?;
    let targets: Vec<_> = targets
        .iter()
        .filter(|t| t["name"] == target_name)
        .collect();
    require(targets.len() == 1, "expected exactly one named leaf target")?;
    let t = targets[0];
    let kind = strings(&t["kind"])?;
    let crate_types = strings(&t["crate_types"])?;
    require(
        kind == ["cdylib"] && crate_types == ["cdylib"],
        "real preload target must be cdylib-only",
    )?;
    Ok(PackageIdentity {
        id: p["id"]
            .as_str()
            .ok_or("metadata package ID missing")?
            .to_owned(),
        name: name.to_owned(),
        manifest,
        source,
        target: target_name.to_owned(),
        kind,
        crate_types,
        target_source: real_file(Path::new(
            t["src_path"].as_str().ok_or("target src_path missing")?,
        ))?,
    })
}

fn strings(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| "expected string array".to_owned())?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "non-string array entry".to_owned())
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoArtifact {
    pub package: PackageIdentity,
    pub features: Vec<String>,
    /// Exact Cargo message profile. Custom profiles cannot be inferred from
    /// its fields alone; receipt separately records the explicit command name.
    pub profile: Value,
    pub fresh: bool,
    pub reported_path: PathBuf,
}

pub fn select_artifact(
    messages: &str,
    package: &PackageIdentity,
    expected_features: &[String],
    expected_profile: &Value,
    target_dir: &Path,
) -> Result<CargoArtifact> {
    let mut candidates = Vec::new();
    let mut finished = Vec::new();
    let mut expected_features = expected_features.to_vec();
    expected_features.sort();
    expected_features.dedup();
    for (index, line) in messages.lines().filter(|line| !line.is_empty()).enumerate() {
        let m: Value = serde_json::from_str(line)
            .map_err(|e| format!("Cargo JSON line {}: {e}", index + 1))?;
        if m["reason"] == "build-finished" {
            finished.push(m["success"].as_bool());
        }
        if m["reason"] != "compiler-artifact"
            || m["package_id"] != package.id
            || m["target"]["name"] != package.target
        {
            continue;
        }
        require(
            real_file(Path::new(
                m["manifest_path"]
                    .as_str()
                    .ok_or("artifact manifest_path missing")?,
            ))? == package.manifest,
            "artifact manifest mismatch",
        )?;
        require(
            strings(&m["target"]["kind"])? == package.kind
                && strings(&m["target"]["crate_types"])? == package.crate_types,
            "artifact target kind/crate-types mismatch",
        )?;
        require(
            real_file(Path::new(
                m["target"]["src_path"]
                    .as_str()
                    .ok_or("artifact target source missing")?,
            ))? == package.target_source,
            "artifact target source mismatch",
        )?;
        let mut features = strings(&m["features"])?;
        features.sort();
        require(
            features == expected_features,
            format!("artifact feature set differs: {features:?}, expected {expected_features:?}"),
        )?;
        require(
            &m["profile"] == expected_profile,
            format!("artifact profile differs: {}", m["profile"]),
        )?;
        let outputs: Vec<_> = strings(&m["filenames"])?
            .into_iter()
            .map(PathBuf::from)
            .filter(|p| p.extension().is_some_and(|e| e == "so"))
            .collect();
        require(
            outputs.len() == 1,
            "leaf compiler-artifact must report exactly one shared object",
        )?;
        let reported_path = real_file(&outputs[0])?;
        let target_dir = fs::canonicalize(target_dir).map_err(|e| e.to_string())?;
        require(
            reported_path.starts_with(target_dir),
            "artifact output is outside explicit separate target directory",
        )?;
        candidates.push(CargoArtifact {
            package: package.clone(),
            features,
            profile: m["profile"].clone(),
            fresh: m["fresh"].as_bool().ok_or("artifact freshness missing")?,
            reported_path,
        });
    }
    require(
        finished == [Some(true)],
        "Cargo must report exactly one successful build-finished message",
    )?;
    require(
        candidates.len() == 1,
        format!(
            "expected one current compiler-artifact; found {}",
            candidates.len()
        ),
    )?;
    Ok(candidates.remove(0))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolEvidence {
    pub name: String,
    pub address: u64,
    pub size: u64,
    pub binding: u8,
    pub visibility: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElfQualification {
    pub initializer: SymbolEvidence,
    pub init_array_slot: u64,
    pub init_array_resolution: String,
    pub exports: Vec<SymbolEvidence>,
    pub allocation_shims: BTreeMap<String, SymbolEvidence>,
    pub needed: Vec<String>,
    pub required_versions: Vec<String>,
    /// Each route lists direct locally resolved call/tail-call addresses from
    /// the actual exported entry point to its compiler-selected shim.
    pub call_paths: BTreeMap<String, Vec<u64>>,
    /// Inlined operations have an actual endpoint proof, never an invented
    /// export->unused-shim edge. Both actual entry and its respective LOCAL shim
    /// must reach the same LOCAL owned allocation endpoint.
    pub inline_routes: BTreeMap<String, InlineAllocationRoute>,
    /// Local, nonpreemptible RIP-relative GOT routes encountered in actual
    /// selected proof-source instructions. Each entry records the instruction, slot and RELATIVE
    /// target; a generic indirect instruction alone earns no qualification.
    pub local_got_edges: Vec<GotEdge>,
    /// Named methods can be optimized away. These optional emitted paths are
    /// observations; absence never waives the frozen runtime ownership oracle.
    pub named_private_allocator_paths: BTreeMap<String, Vec<u64>>,
    pub named_private_allocator_observation: String,
    pub readelf_versions: FileIdentity,
    pub readelf_symbols_relocations: FileIdentity,
    pub objdump_disassembly: FileIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GotEdge {
    pub source_function: u64,
    pub instruction: u64,
    pub slot: u64,
    pub target_function: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineAllocationRoute {
    pub entry: SymbolEvidence,
    pub shim: SymbolEvidence,
    pub target: SymbolEvidence,
    pub entry_to_endpoint: Vec<u64>,
    pub shim_to_endpoint: Vec<u64>,
    pub entry_got_edges: Vec<GotEdge>,
    pub shim_got_edges: Vec<GotEdge>,
    pub observation: String,
}

fn qualifies_owned_endpoint(name: &str) -> bool {
    let name = name.split(".llvm.").next().unwrap_or(name);
    ((name.contains("PrivatePatchAllocator") || name.contains("PrivateToolAllocator"))
        && name.contains("GlobalAlloc")
        && (name.ends_with("5alloc") || name.ends_with("12alloc_zeroed")))
        || (name.contains("ToolHeap") && name.ends_with("8allocate"))
}

fn allocation_wrapper(name: &str) -> bool {
    name.contains("allocator_fixture") && name.contains("m1_")
        || [
            "5alloc5alloc5alloc",
            "5alloc5alloc12alloc_zeroed",
            "5alloc5alloc7realloc",
            "5alloc5alloc7dealloc",
        ]
        .iter()
        .any(|pattern| name.contains(pattern))
}

fn called_shim_path(
    elf: &Elf<'_>,
    graph: &BTreeMap<u64, BTreeSet<u64>>,
    entry: u64,
    shim: u64,
) -> Result<Vec<u64>> {
    let wrappers: BTreeSet<_> = elf
        .syms
        .iter()
        .filter_map(|s| {
            elf.strtab
                .get_at(s.st_name)
                .filter(|name| allocation_wrapper(name))
                .map(|_| s.st_value)
        })
        .chain([entry, shim])
        .collect();
    // Panic/cleanup/container allocation is not evidence that the exported
    // operation's std::alloc call executes its compiler-selected shim.
    let restricted = graph
        .iter()
        .filter(|(source, _)| wrappers.contains(source))
        .map(|(source, targets)| {
            (
                *source,
                targets
                    .iter()
                    .filter(|target| wrappers.contains(target))
                    .copied()
                    .collect(),
            )
        })
        .collect();
    shortest_path(&restricted, entry, shim)
}

fn qualify_inline_route(
    key: &str,
    elf: &Elf<'_>,
    entry: &SymbolEvidence,
    shim: &SymbolEvidence,
    graph: &BTreeMap<u64, BTreeSet<u64>>,
    got_edges: &[GotEdge],
) -> Result<InlineAllocationRoute> {
    require(
        matches!(
            key,
            "m1_alloc->__rust_alloc" | "m1_alloc->__rust_alloc_zeroed"
        ),
        "unsupported optimized allocation route",
    )?;
    let names: BTreeMap<_, _> = elf
        .syms
        .iter()
        .filter(|s| executable_symbol(elf, s))
        .filter_map(|s| elf.strtab.get_at(s.st_name).map(|name| (s.st_value, name)))
        .collect();
    let route_nodes: BTreeSet<_> = names
        .iter()
        .filter(|(_, name)| allocation_wrapper(name) || qualifies_owned_endpoint(name))
        .map(|(address, _)| *address)
        .chain([entry.address, shim.address])
        .collect();
    let restricted = graph
        .iter()
        .filter(|(source, _)| route_nodes.contains(source))
        .map(|(source, targets)| {
            (
                *source,
                targets
                    .iter()
                    .filter(|target| route_nodes.contains(target))
                    .copied()
                    .collect(),
            )
        })
        .collect();
    let avoids_legacy = |path: &[u64]| {
        path.iter().all(|address| {
            names.get(address).is_none_or(|name| {
                !name.contains("GuestAllocator")
                    && !(name.contains("PatchAllocator") && !name.contains("PrivatePatchAllocator"))
            })
        })
    };
    let path_edges = |path: &[u64]| {
        got_edges
            .iter()
            .filter(|edge| {
                path.windows(2)
                    .any(|pair| edge.source_function == pair[0] && edge.target_function == pair[1])
            })
            .cloned()
            .collect()
    };
    let mut candidates = Vec::new();
    for symbol in elf
        .syms
        .iter()
        .filter(|s| executable_symbol(elf, s) && s.st_bind() == sym::STB_LOCAL)
    {
        let Some(name) = elf
            .strtab
            .get_at(symbol.st_name)
            .filter(|name| qualifies_owned_endpoint(name))
        else {
            continue;
        };
        let Ok(entry_path) = shortest_path(&restricted, entry.address, symbol.st_value) else {
            continue;
        };
        let Ok(shim_path) = shortest_path(&restricted, shim.address, symbol.st_value) else {
            continue;
        };
        if avoids_legacy(&entry_path) && avoids_legacy(&shim_path) {
            candidates.push((entry_path, shim_path, symbol_evidence(name, &symbol)));
        }
    }
    candidates.sort_by_key(|(entry, shim, target)| (entry.len() + shim.len(), target.address));
    let (entry_to_endpoint, shim_to_endpoint, target) = candidates.into_iter().next().ok_or_else(|| format!("{key}: no actual export and respective LOCAL shim routes to the same LOCAL owned allocation endpoint; emitted evidence incomplete"))?;
    Ok(InlineAllocationRoute {
        entry: entry.clone(), shim: shim.clone(), target,
        entry_got_edges: path_edges(&entry_to_endpoint), shim_got_edges: path_edges(&shim_to_endpoint),
        entry_to_endpoint, shim_to_endpoint,
        observation: "Actual export reaches the same LOCAL owned endpoint as its separately emitted LOCAL compiler shim; the operation does not execute that shim. Distinct alloc/zeroed semantics and zero foreign allocator entries remain runtime assertions.".to_owned(),
    })
}

fn executable_symbol(elf: &Elf<'_>, s: &sym::Sym) -> bool {
    s.st_type() == sym::STT_FUNC
        && s.st_shndx != section_header::SHN_UNDEF as usize
        && s.st_value != 0
        && s.st_size > 0
        && elf.program_headers.iter().any(|p| {
            p.p_type == program_header::PT_LOAD
                && p.p_flags & program_header::PF_X != 0
                && s.st_value >= p.p_vaddr
                && s.st_value
                    .checked_add(s.st_size)
                    .is_some_and(|end| end <= p.p_vaddr.saturating_add(p.p_memsz))
        })
}

fn symbol_evidence(name: &str, s: &sym::Sym) -> SymbolEvidence {
    SymbolEvidence {
        name: name.to_owned(),
        address: s.st_value,
        size: s.st_size,
        binding: s.st_bind(),
        visibility: s.st_visibility(),
    }
}

fn export(elf: &Elf<'_>, name: &str) -> Result<(usize, sym::Sym)> {
    let candidates: Vec<_> = elf
        .dynsyms
        .iter()
        .enumerate()
        .filter(|(_, s)| elf.dynstrtab.get_at(s.st_name) == Some(name) && executable_symbol(elf, s))
        .collect();
    require(
        candidates.len() == 1,
        format!("required unique defined dynamic function {name} is absent/ambiguous"),
    )?;
    let (index, s) = candidates[0];
    require(
        s.st_bind() == sym::STB_GLOBAL
            && matches!(s.st_visibility(), sym::STV_DEFAULT | sym::STV_PROTECTED),
        format!("required function {name} is not externally exported"),
    )?;
    Ok((index, s))
}

fn init_array_slot(
    elf: &Elf<'_>,
    bytes: &[u8],
    initializer_index: usize,
    initializer: &sym::Sym,
) -> Result<(u64, String)> {
    let sections: Vec<_> = elf
        .section_headers
        .iter()
        .filter(|s| {
            s.sh_type == section_header::SHT_INIT_ARRAY
                && elf.shdr_strtab.get_at(s.sh_name) == Some(".init_array")
        })
        .collect();
    require(
        sections.len() == 1,
        "required unique .init_array section absent",
    )?;
    let section = sections[0];
    require(
        section.sh_size > 0 && section.sh_size % 8 == 0 && section.sh_addr % 8 == 0,
        "invalid x86_64 init_array layout",
    )?;
    let start = usize::try_from(section.sh_offset).map_err(|e| e.to_string())?;
    let len = usize::try_from(section.sh_size).map_err(|e| e.to_string())?;
    let entries = bytes
        .get(
            start
                ..start
                    .checked_add(len)
                    .ok_or("init_array file range overflow")?,
        )
        .ok_or("init_array outside ELF file")?;
    require(
        elf.program_headers.iter().any(|p| {
            p.p_type == program_header::PT_LOAD
                && section.sh_addr >= p.p_vaddr
                && section
                    .sh_addr
                    .checked_add(section.sh_size)
                    .is_some_and(|end| end <= p.p_vaddr.saturating_add(p.p_memsz))
        }),
        ".init_array is not in a loaded segment",
    )?;
    let relocations: Vec<_> = elf.dynrelas.iter().chain(elf.dynrels.iter()).collect();
    for (i, entry) in entries.as_chunks::<8>().0.iter().enumerate() {
        let slot = section.sh_addr + (i as u64) * 8;
        let raw = u64::from_le_bytes(*entry);
        let at_slot: Vec<_> = relocations.iter().filter(|r| r.r_offset == slot).collect();
        require(at_slot.len() <= 1, "overlapping init_array relocations")?;
        if let Some(r) = at_slot.first() {
            let addend = r.r_addend.unwrap_or(raw as i64);
            if r.r_type == reloc::R_X86_64_RELATIVE
                && r.r_sym == 0
                && addend >= 0
                && addend as u64 == initializer.st_value
            {
                return Ok((
                    slot,
                    "R_X86_64_RELATIVE to exact initializer address".to_owned(),
                ));
            }
            if r.r_type == reloc::R_X86_64_64 && r.r_sym == initializer_index && addend == 0 {
                return Ok((
                    slot,
                    "R_X86_64_64 exact initializer symbol plus zero".to_owned(),
                ));
            }
        } else if raw == initializer.st_value {
            // A nonrelocated relative virtual address in ET_DYN is not a
            // callable pointer after ASLR. It cannot prove a real constructor.
            return Err("initializer address in init_array has no loader relocation".to_owned());
        }
    }
    Err(".init_array does not resolve an aligned slot to the required initializer".to_owned())
}

fn required_versions(text: &str) -> Result<Vec<String>> {
    let mut versions = BTreeSet::new();
    for line in text.lines() {
        let Some((_, rest)) = line.split_once("Name:") else {
            continue;
        };
        require(rest.contains("Flags:"), "malformed readelf version record")?;
        let name = rest
            .split_whitespace()
            .next()
            .ok_or("missing symbol version name")?;
        let components: Vec<_> = name
            .strip_prefix("GLIBC_2.")
            .ok_or_else(|| format!("guest preload requires unsupported version {name}"))?
            .split('.')
            .collect();
        require(
            (1..=2).contains(&components.len())
                && components
                    .iter()
                    .all(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit())),
            format!("unsupported guest symbol version {name}"),
        )?;
        let minor: u32 = components[0]
            .parse()
            .map_err(|e| format!("glibc version: {e}"))?;
        require(
            minor <= 34,
            format!("guest preload requires {name}, above GLIBC_2.34 floor"),
        )?;
        versions.insert(name.to_owned());
    }
    Ok(versions.into_iter().collect())
}

fn elf_required_versions(elf: &Elf<'_>) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    if let Some(verneed) = &elf.verneed {
        for need in verneed.iter() {
            let mut observed = 0usize;
            for auxiliary in need.iter() {
                let name = elf
                    .dynstrtab
                    .get_at(auxiliary.vna_name)
                    .ok_or("ELF version need has invalid string offset")?;
                // Use exactly the same version parser as retained readelf.
                names.extend(required_versions(&format!("Name: {name} Flags: none"))?);
                observed += 1;
            }
            require(
                observed == usize::from(need.vn_cnt),
                "truncated ELF symbol-version requirements",
            )?;
        }
    }
    Ok(names.into_iter().collect())
}

/// Resolve a GOT slot only when the loader's unique relocation computes an
/// exact locally bound function address without lookup in another DSO.
fn local_got_targets(elf: &Elf<'_>) -> Result<BTreeMap<u64, u64>> {
    let local_functions: BTreeSet<_> = elf
        .syms
        .iter()
        .filter(|s| executable_symbol(elf, s) && s.st_bind() == sym::STB_LOCAL)
        .map(|s| s.st_value)
        .collect();
    let mut slots = BTreeMap::<u64, Vec<_>>::new();
    for r in elf
        .dynrelas
        .iter()
        .chain(elf.dynrels.iter())
        .chain(elf.pltrelocs.iter())
    {
        slots.entry(r.r_offset).or_default().push(r);
    }
    let mut qualified = BTreeMap::new();
    for (slot, records) in slots {
        if records.len() != 1 {
            continue;
        }
        let r = &records[0];
        if r.r_type != reloc::R_X86_64_RELATIVE || r.r_sym != 0 {
            continue;
        }
        let Some(addend) = r.r_addend else {
            continue;
        };
        if addend < 0 || !local_functions.contains(&(addend as u64)) {
            continue;
        }
        // Slot must be loader-mapped and made read-only after relocations;
        // otherwise runtime writes could change the observed pointer route.
        let in_load = elf.program_headers.iter().any(|p| {
            p.p_type == program_header::PT_LOAD
                && slot >= p.p_vaddr
                && slot
                    .checked_add(8)
                    .is_some_and(|end| end <= p.p_vaddr.saturating_add(p.p_memsz))
        });
        let in_relro = elf.program_headers.iter().any(|p| {
            p.p_type == program_header::PT_GNU_RELRO
                && slot >= p.p_vaddr
                && slot
                    .checked_add(8)
                    .is_some_and(|end| end <= p.p_vaddr.saturating_add(p.p_memsz))
        });
        if in_load && in_relro {
            qualified.insert(slot, addend as u64);
        }
    }
    Ok(qualified)
}

/// Direct calls/tail calls and RIP-relative indirect calls with independently
/// qualified local RELATIVE GOT slots are admissible. Register-indirect calls
/// and other unsupported paths earn no edge, so required unresolved paths fail.
type CallGraph = BTreeMap<u64, BTreeSet<u64>>;

const MAX_OBJDUMP_BYTES: usize = 64 * 1024 * 1024;

/// The complete node union admitted by the existing required-route and named
/// allocator graphs. Scope is derived from the ELF, not a receipt or a successful
/// path: unchosen alternatives remain part of the required byte evidence.
fn proof_functions(elf: &Elf<'_>, initializer_name: &str) -> Result<BTreeMap<u64, u64>> {
    let mut addresses: BTreeSet<_> = elf
        .syms
        .iter()
        .filter(|s| executable_symbol(elf, s))
        .filter_map(|s| {
            elf.strtab
                .get_at(s.st_name)
                .filter(|name| {
                    allocation_wrapper(name)
                        || qualifies_owned_endpoint(name)
                        || (s.st_bind() == sym::STB_LOCAL
                            && (name.contains("PrivatePatchAllocator")
                                || name.contains("PrivateToolAllocator")))
                })
                .map(|_| s.st_value)
        })
        .collect();
    // Do not let a naming predicate prune a mandatory entry or compiler shim.
    for name in EXPORTS.into_iter().chain([initializer_name]) {
        addresses.insert(export(elf, name)?.1.st_value);
    }
    for shim in SHIMS {
        let candidates: Vec<_> = elf
            .syms
            .iter()
            .filter(|s| executable_symbol(elf, s))
            .filter(|s| {
                elf.strtab
                    .get_at(s.st_name)
                    .is_some_and(|name| name == shim || name.ends_with(shim))
            })
            .collect();
        require(
            candidates.len() == 1,
            format!("required unique compiler-selected shim {shim} absent/ambiguous"),
        )?;
        addresses.insert(candidates[0].st_value);
    }
    let mut functions = BTreeMap::new();
    for address in addresses {
        let sizes: BTreeSet<_> = elf
            .syms
            .iter()
            .chain(elf.dynsyms.iter())
            .filter(|s| {
                s.st_type() == sym::STT_FUNC
                    && s.st_shndx != section_header::SHN_UNDEF as usize
                    && s.st_value == address
            })
            .map(|s| s.st_size)
            .collect();
        require(
            sizes.len() == 1 && !sizes.contains(&0),
            format!("selected function {address:#x} has absent/conflicting alias sizes"),
        )?;
        let size = *sizes.iter().next().unwrap();
        require(
            address.checked_add(size).is_some(),
            format!("selected function {address:#x} overflows its address range"),
        )?;
        functions.insert(address, size);
    }
    Ok(functions)
}

/// Independently read every selected function's complete bytes from its
/// file-backed executable PT_LOAD range. Memory-only tails are not evidence.
fn proof_function_bytes(
    elf: &Elf<'_>,
    bytes: &[u8],
    functions: &BTreeMap<u64, u64>,
) -> Result<BTreeMap<u64, Vec<u8>>> {
    let mut expected = BTreeMap::new();
    let mut total = 0usize;
    for (&address, &size) in functions {
        let end = address.checked_add(size).ok_or("function range overflow")?;
        let mut offsets = BTreeSet::new();
        for p in elf.program_headers.iter().filter(|p| {
            p.p_type == program_header::PT_LOAD && p.p_flags & program_header::PF_X != 0
        }) {
            let file_end = p
                .p_vaddr
                .checked_add(p.p_filesz)
                .ok_or("executable segment range overflow")?;
            if address >= p.p_vaddr && end <= file_end {
                offsets.insert(
                    p.p_offset
                        .checked_add(address - p.p_vaddr)
                        .ok_or("function file offset overflow")?,
                );
            }
        }
        require(
            offsets.len() == 1,
            format!(
                "selected function {address:#x} lacks a unique complete file-backed executable range"
            ),
        )?;
        let offset = usize::try_from(*offsets.iter().next().unwrap())
            .map_err(|_| "function file offset exceeds usize")?;
        let size = usize::try_from(size).map_err(|_| "function size exceeds usize")?;
        let end = offset
            .checked_add(size)
            .ok_or("function file range overflow")?;
        let code = bytes
            .get(offset..end)
            .ok_or("function bytes exceed actual ELF file")?;
        total = total
            .checked_add(size)
            .ok_or("selected function byte total overflow")?;
        require(
            total <= MAX_OBJDUMP_BYTES,
            "selected function bytes exceed disassembly output cap",
        )?;
        expected.insert(address, code.to_vec());
    }
    Ok(expected)
}

fn raw_byte(word: &str) -> Option<u8> {
    (word.len() == 2 && word.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u8::from_str_radix(word, 16).unwrap())
}

/// A header is not completeness evidence. Require each selected interval once,
/// contiguous instruction addresses, and an exact match for every ELF byte.
fn verify_disassembly_bytes(text: &str, expected: &BTreeMap<u64, Vec<u8>>) -> Result<()> {
    require(!expected.is_empty(), "no selected disassembly functions")?;
    let mut consumed = BTreeMap::<u64, usize>::new();
    let mut current = None;
    for line in text.lines() {
        let trimmed = line.trim();
        let header = trimmed
            .split_once(" <")
            .filter(|(_, suffix)| suffix.ends_with(">:"))
            .and_then(|(addr, _)| u64::from_str_radix(addr, 16).ok());
        if let Some(address) = header {
            if let Some(source) = current {
                require(
                    consumed[&source] == expected[&source].len(),
                    format!("selected function {source:#x} disassembly is truncated"),
                )?;
            }
            require(
                expected.contains_key(&address),
                format!("unselected disassembly function {address:#x}"),
            )?;
            require(
                !consumed.contains_key(&address),
                format!("duplicate disassembly function {address:#x}"),
            )?;
            consumed.insert(address, 0);
            current = Some(address);
            continue;
        }
        let Some((address, instruction)) = trimmed.split_once(':') else {
            continue;
        };
        let Ok(address) = u64::from_str_radix(address.trim(), 16) else {
            continue;
        };
        let source = current.ok_or("instruction outside selected function header")?;
        let mut words = instruction.split_whitespace().peekable();
        let mut raw = Vec::new();
        while let Some(byte) = words.peek().and_then(|word| raw_byte(word)) {
            raw.push(byte);
            words.next();
        }
        require(
            !raw.is_empty() && raw.len() <= 15 && words.next().is_some(),
            format!("instruction {address:#x} lacks complete raw x86 bytes/mnemonic"),
        )?;
        let offset = consumed[&source];
        let next = source
            .checked_add(offset as u64)
            .ok_or("instruction address overflow")?;
        require(
            address == next,
            format!("function {source:#x} has a gap/duplicate at {address:#x}, expected {next:#x}"),
        )?;
        let end = offset
            .checked_add(raw.len())
            .ok_or("instruction byte range overflow")?;
        let code = expected[&source]
            .get(offset..end)
            .ok_or_else(|| format!("function {source:#x} disassembly overruns its ELF interval"))?;
        require(
            raw == code,
            format!("instruction {address:#x} raw bytes differ from actual ELF"),
        )?;
        consumed.insert(source, end);
    }
    require(
        consumed.len() == expected.len(),
        "selected function disassembly is missing a range",
    )?;
    for (address, bytes) in expected {
        require(
            consumed.get(address) == Some(&bytes.len()),
            format!("selected function {address:#x} disassembly is incomplete"),
        )?;
    }
    Ok(())
}

fn disassembly_edges(
    text: &str,
    functions: &BTreeMap<u64, u64>,
    got_targets: &BTreeMap<u64, u64>,
) -> Result<(CallGraph, Vec<GotEdge>)> {
    let mut graph = BTreeMap::<u64, BTreeSet<u64>>::new();
    let mut got_edges = Vec::new();
    let mut current = None;
    let mut headers = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        let header = trimmed
            .split_once(" <")
            .filter(|(_, suffix)| suffix.ends_with(">:"))
            .and_then(|(addr, _)| u64::from_str_radix(addr, 16).ok());
        if let Some(addr) = header {
            current = functions.contains_key(&addr).then_some(addr);
            headers += 1;
            continue;
        }
        let Some(source) = current else {
            continue;
        };
        let Some((addr, instruction)) = trimmed.split_once(':') else {
            continue;
        };
        let Ok(address) = u64::from_str_radix(addr.trim(), 16) else {
            continue;
        };
        // Objdump prints linker alignment padding between function ranges under
        // the preceding header. Padding contributes no call-path evidence.
        if address < source || address >= source.saturating_add(functions[&source]) {
            continue;
        }
        let mut words = instruction
            .split_whitespace()
            .skip_while(|word| raw_byte(word).is_some());
        let Some(opcode) = words.next() else {
            continue;
        };
        if !matches!(opcode, "call" | "callq" | "jmp" | "jmpq") {
            continue;
        }
        let Some(destination) = words.next() else {
            continue;
        };
        if destination.starts_with('*') {
            if !destination.ends_with("(%rip)") {
                continue;
            }
            let Some((_, comment)) = instruction.split_once('#') else {
                continue;
            };
            let Some(slot) = comment.split_whitespace().next() else {
                continue;
            };
            let Ok(slot) = u64::from_str_radix(slot, 16) else {
                continue;
            };
            let Some(target) = got_targets.get(&slot) else {
                continue;
            };
            graph.entry(source).or_default().insert(*target);
            got_edges.push(GotEdge {
                source_function: source,
                instruction: address,
                slot,
                target_function: *target,
            });
            continue;
        }
        let Ok(destination) = u64::from_str_radix(destination, 16) else {
            continue;
        };
        if functions.contains_key(&destination) {
            graph.entry(source).or_default().insert(destination);
        }
    }
    require(headers > 0, "objdump yielded no function disassembly")?;
    Ok((graph, got_edges))
}

fn shortest_path(graph: &BTreeMap<u64, BTreeSet<u64>>, from: u64, to: u64) -> Result<Vec<u64>> {
    let mut queue = VecDeque::from([from]);
    let mut predecessors = BTreeMap::from([(from, from)]);
    while let Some(current) = queue.pop_front() {
        if current == to {
            let mut path = vec![to];
            while *path.last().unwrap() != from {
                path.push(predecessors[path.last().unwrap()]);
            }
            path.reverse();
            return Ok(path);
        }
        if let Some(edges) = graph.get(&current) {
            for next in edges {
                if !predecessors.contains_key(next) {
                    predecessors.insert(*next, current);
                    queue.push_back(*next);
                }
            }
        }
    }
    Err(format!(
        "no direct locally resolved compiled call path from {from:#x} to {to:#x}; evidence incomplete"
    ))
}

pub fn qualify_elf(
    path: &Path,
    initializer_name: &str,
    runner: &mut Runner,
) -> Result<ElfQualification> {
    let artifact = FileIdentity::read(path)?;
    let cwd = artifact.path.parent().ok_or("artifact has no parent")?;
    let path_string = artifact.path.to_str().ok_or("artifact path is not UTF-8")?;
    runner.text("readelf", &["--version-info", "--wide", path_string], cwd)?;
    let readelf_versions = runner.commands.last().unwrap().stdout.clone();
    let bytes = fs::read(&artifact.path).map_err(|e| e.to_string())?;
    let elf = Elf::parse(&bytes).map_err(|e| format!("parse actual runtime ELF: {e}"))?;
    let scope = proof_functions(&elf, initializer_name)?;
    let expected_bytes = proof_function_bytes(&elf, &bytes, &scope)?;
    let mut disassembly = Vec::new();
    let mut emitted = 0usize;
    let limit = MAX_OBJDUMP_BYTES.min(runner.max_stream_bytes);
    for (&address, &size) in &scope {
        // One aggregate budget covers all stdout/stderr from all intervals.
        // Reserve the separator before spawning; neither a per-range nor a
        // concatenated-output cap can silently grow with the function count.
        let remaining = limit
            .checked_sub(emitted)
            .and_then(|remaining| remaining.checked_sub(1))
            .filter(|remaining| *remaining > 0)
            .ok_or("objdump aggregate byte limit exhausted")?;
        let end = address.checked_add(size).ok_or("function range overflow")?;
        let arguments = vec![
            "--disassemble".to_owned(),
            "--show-raw-insn".to_owned(),
            "--disassemble-zeroes".to_owned(),
            "--insn-width=16".to_owned(),
            "--wide".to_owned(),
            format!("--start-address={address:#x}"),
            format!("--stop-address={end:#x}"),
            path_string.to_owned(),
        ];
        let stdout = runner.run_with_combined_limit("objdump", &arguments, cwd, Some(remaining))?;
        let stderr_bytes = fs::metadata(&runner.commands.last().unwrap().stderr.path)
            .map_err(|e| e.to_string())?
            .len();
        emitted = emitted
            .checked_add(stdout.len())
            .and_then(|n| n.checked_add(usize::try_from(stderr_bytes).ok()?))
            .and_then(|n| n.checked_add(1))
            .ok_or("objdump aggregate byte count overflow")?;
        require(
            emitted <= limit,
            "objdump aggregate output exceeds unchanged byte cap",
        )?;
        disassembly.extend_from_slice(&stdout);
        disassembly.push(b'\n');
    }
    let text = std::str::from_utf8(&disassembly)
        .map_err(|e| format!("objdump output is not UTF-8: {e}"))?;
    verify_disassembly_bytes(text, &expected_bytes)?;
    let disassembly_path = runner
        .logs
        .join(format!("objdump-scope-{:04}.stdout", runner.serial));
    write_new(&disassembly_path, &disassembly)?;
    let objdump_disassembly = FileIdentity::read(&disassembly_path)?;
    runner.text(
        "readelf",
        &["--symbols", "--relocs", "--dynamic", "--wide", path_string],
        cwd,
    )?;
    let readelf_symbols_relocations = runner.commands.last().unwrap().stdout.clone();
    artifact.verify()?;
    qualify_elf_from_evidence(
        path,
        initializer_name,
        readelf_versions,
        readelf_symbols_relocations,
        objdump_disassembly,
    )
}

/// Consumer-side requalification uses the same ELF parser, call-path builder and
/// portability oracle with retained, hashed producer evidence; no tools/builds.
pub fn qualify_elf_from_evidence(
    path: &Path,
    initializer_name: &str,
    readelf_versions: FileIdentity,
    readelf_symbols_relocations: FileIdentity,
    objdump_disassembly: FileIdentity,
) -> Result<ElfQualification> {
    readelf_versions.verify()?;
    readelf_symbols_relocations.verify()?;
    objdump_disassembly.verify()?;
    let artifact = FileIdentity::read(path)?;
    let bytes = fs::read(&artifact.path).map_err(|e| e.to_string())?;
    let elf = Elf::parse(&bytes).map_err(|e| format!("parse actual runtime ELF: {e}"))?;
    require(
        elf.header.e_type == header::ET_DYN
            && elf.header.e_machine == header::EM_X86_64
            && elf.is_64
            && elf.little_endian,
        "runtime must be little-endian x86_64 ET_DYN",
    )?;
    require(elf.dynamic.is_some(), "runtime lacks a dynamic section")?;
    require(
        elf.rpaths.is_empty() && elf.runpaths.is_empty(),
        "runtime must have no RPATH/RUNPATH",
    )?;
    // Inspect tags as well: even an empty-string RPATH/RUNPATH is forbidden.
    require(
        !elf.dynamic.as_ref().unwrap().dyns.iter().any(|d| {
            matches!(
                d.d_tag,
                goblin::elf::dynamic::DT_RPATH | goblin::elf::dynamic::DT_RUNPATH
            )
        }),
        "runtime records a library search path",
    )?;
    require(
        elf.libraries.iter().all(|n| ALLOWED_NEEDED.contains(n)),
        format!("runtime needs non-glibc library: {:?}", elf.libraries),
    )?;
    let (initializer_index, initializer) = export(&elf, initializer_name)?;
    let (init_array_slot, init_array_resolution) =
        init_array_slot(&elf, &bytes, initializer_index, &initializer)?;
    let exports = EXPORTS
        .iter()
        .map(|name| export(&elf, name).map(|(_, s)| symbol_evidence(name, &s)))
        .collect::<Result<Vec<_>>>()?;
    let mut allocation_shims = BTreeMap::new();
    for shim in SHIMS {
        // rustc's v0 symbol names end in the allocation shim identifier; older
        // compilers emit the unmangled identifier. Require the exact suffix,
        // not a broad `alloc` substring that might name an unrelated helper.
        let candidates: Vec<_> = elf
            .syms
            .iter()
            .filter(|s| executable_symbol(&elf, s))
            .filter_map(|s| {
                elf.strtab
                    .get_at(s.st_name)
                    .filter(|name| *name == shim || name.ends_with(shim))
                    .map(|name| (name, s))
            })
            .collect();
        require(
            candidates.len() == 1,
            format!("required unique compiler-selected shim {shim} absent/ambiguous"),
        )?;
        let (name, s) = candidates[0];
        require(
            s.st_bind() == sym::STB_LOCAL,
            format!("allocation shim {name} is not LOCAL"),
        )?;
        require(
            !elf.dynsyms
                .iter()
                .any(|d| elf.dynstrtab.get_at(d.st_name) == Some(name)),
            format!("allocation shim {name} appears in dynamic symbol table"),
        )?;
        require(
            !elf.dynrelas
                .iter()
                .chain(elf.dynrels.iter())
                .chain(elf.pltrelocs.iter())
                .any(|r| {
                    elf.dynsyms
                        .get(r.r_sym)
                        .is_some_and(|s| elf.dynstrtab.get_at(s.st_name) == Some(name))
                }),
            format!("allocation shim {name} has a dynamic preemption route"),
        )?;
        allocation_shims.insert(shim.to_owned(), symbol_evidence(name, &s));
    }
    let version_text = fs::read_to_string(&readelf_versions.path)
        .map_err(|e| format!("read retained version evidence: {e}"))?;
    let required_versions = required_versions(&version_text)?;
    require(
        required_versions == elf_required_versions(&elf)?,
        "retained readelf and actual ELF symbol-version requirements differ",
    )?;
    require(
        fs::metadata(&objdump_disassembly.path)
            .map_err(|e| e.to_string())?
            .len()
            <= MAX_OBJDUMP_BYTES as u64,
        "retained disassembly exceeds unchanged byte cap",
    )?;
    let disassembly = fs::read_to_string(&objdump_disassembly.path)
        .map_err(|e| format!("read retained disassembly: {e}"))?;
    let scope = proof_functions(&elf, initializer_name)?;
    let expected_bytes = proof_function_bytes(&elf, &bytes, &scope)?;
    verify_disassembly_bytes(&disassembly, &expected_bytes)?;
    let functions = elf
        .syms
        .iter()
        .filter(|s| executable_symbol(&elf, s))
        .map(|s| (s.st_value, s.st_size))
        .collect();
    let got_targets = local_got_targets(&elf)?;
    let (graph, local_got_edges) = disassembly_edges(&disassembly, &functions, &got_targets)?;
    let mut call_paths = BTreeMap::new();
    let mut inline_routes = BTreeMap::new();
    for (entry, shim) in [
        ("m1_alloc", "__rust_alloc"),
        ("m1_alloc", "__rust_alloc_zeroed"),
        ("m1_realloc", "__rust_realloc"),
        ("m1_dealloc", "__rust_dealloc"),
    ] {
        let key = format!("{entry}->{shim}");
        let entry = exports.iter().find(|s| s.name == entry).unwrap();
        let shim = &allocation_shims[shim];
        match called_shim_path(&elf, &graph, entry.address, shim.address) {
            Ok(path) => {
                call_paths.insert(key, path);
            }
            Err(_) => {
                let route =
                    qualify_inline_route(&key, &elf, entry, shim, &graph, &local_got_edges)?;
                inline_routes.insert(key, route);
            }
        }
    }
    let private_methods: Vec<_> = elf
        .syms
        .iter()
        .filter(|s| executable_symbol(&elf, s) && s.st_bind() == sym::STB_LOCAL)
        .filter_map(|s| {
            elf.strtab
                .get_at(s.st_name)
                .filter(|name| {
                    name.contains("PrivatePatchAllocator") || name.contains("PrivateToolAllocator")
                })
                .map(|name| (name, s.st_value))
        })
        .collect();
    // Named implementation observations must not wander through panic/cleanup
    // paths to another unrelated allocation. They are additional emitted-code
    // evidence, independent of the required export-to-shim routes above.
    let private_nodes: BTreeSet<_> = private_methods
        .iter()
        .map(|(_, address)| *address)
        .chain(allocation_shims.values().map(|s| s.address))
        .collect();
    let private_graph = graph
        .iter()
        .filter(|(source, _)| private_nodes.contains(source))
        .map(|(source, targets)| {
            (
                *source,
                targets
                    .iter()
                    .filter(|target| private_nodes.contains(target))
                    .copied()
                    .collect(),
            )
        })
        .collect();
    let mut named_private_allocator_paths = BTreeMap::new();
    for (shim, symbol) in &allocation_shims {
        for (name, target) in &private_methods {
            if let Ok(path) = shortest_path(&private_graph, symbol.address, *target) {
                named_private_allocator_paths.insert(format!("{shim}->{name}"), path);
            }
        }
    }
    let named_private_allocator_observation = if named_private_allocator_paths.is_empty() {
        "No emitted named private allocator path qualified; methods may be inlined or routes require manual emitted-code review. This observation does not establish private ownership."
    } else {
        "Named private allocator methods are reachable through recorded locally resolved compiled paths. Runtime watcher/ownership assertions and frozen source audit remain required."
    }.to_owned();
    artifact.verify()?;
    Ok(ElfQualification {
        initializer: symbol_evidence(initializer_name, &initializer),
        init_array_slot,
        init_array_resolution,
        exports,
        allocation_shims,
        needed: elf.libraries.iter().map(|s| (*s).to_owned()).collect(),
        required_versions,
        call_paths,
        inline_routes,
        local_got_edges,
        named_private_allocator_paths,
        named_private_allocator_observation,
        readelf_versions,
        readelf_symbols_relocations,
        objdump_disassembly,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeReceipt {
    pub schema_version: u32,
    pub source_before: SourceIdentity,
    pub source_after: SourceIdentity,
    pub cargo_artifact: CargoArtifact,
    pub profile_name: String,
    pub artifact: FileIdentity,
    pub qualification: ElfQualification,
    pub rustc_verbose: String,
    pub cargo_version: String,
    pub readelf_version: String,
    pub objdump_version: String,
    pub build_environment: BTreeMap<String, Option<String>>,
    pub target_directory: PathBuf,
    pub commands: Vec<CommandEvidence>,
    /// Artifact qualification is never a runtime M1 passing result.
    pub executed_tests: u64,
    pub full_m1_pass_claimed: bool,
}

pub struct ConsumerExpectation<'a> {
    pub package: &'a PackageIdentity,
    pub features: &'a [String],
    pub profile_name: &'a str,
    pub profile: &'a Value,
    pub initializer: &'a str,
    pub source_head: &'a str,
    pub source_tree: &'a str,
    pub source_lock_sha256: &'a str,
    pub oracle_sha256: &'a BTreeMap<String, String>,
}

/// Requires caller-provided identities independent of this receipt. Does not
/// dlopen, invoke Cargo or silently return on absence. The test must separately
/// validate dladdr owner/base/hash and the genuine configured guest bootstrap.
pub fn verify_runtime_receipt(
    receipt_path: &Path,
    selected_path: &Path,
    expected: &ConsumerExpectation<'_>,
) -> Result<RuntimeReceipt> {
    real_file(receipt_path)?;
    let bytes =
        fs::read(receipt_path).map_err(|e| format!("read required runtime receipt: {e}"))?;
    let receipt: RuntimeReceipt = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parse required runtime receipt: {e}"))?;
    require(
        receipt.schema_version == SCHEMA,
        "unsupported runtime receipt schema",
    )?;
    require(
        receipt.source_before == receipt.source_after && receipt.source_before.status.is_empty(),
        "runtime source identity changed or dirty",
    )?;
    let source = &receipt.source_before;
    require(
        source.head == expected.source_head
            && source.tree == expected.source_tree
            && source.lock.sha256 == expected.source_lock_sha256,
        "runtime source HEAD/tree/lock identity differs",
    )?;
    let observed_oracles: BTreeMap<_, _> = source
        .oracles
        .iter()
        .map(|(label, file)| (label.clone(), file.sha256.clone()))
        .collect();
    require(
        &observed_oracles == expected.oracle_sha256,
        "runtime oracle identities differ",
    )?;
    source.lock.verify()?;
    for oracle in source.oracles.values() {
        oracle.verify()?;
    }
    require(
        &receipt.cargo_artifact.package == expected.package,
        "runtime package/manifest/target identity differs",
    )?;
    let mut features = expected.features.to_vec();
    features.sort();
    features.dedup();
    require(
        receipt.cargo_artifact.features == features
            && &receipt.cargo_artifact.profile == expected.profile
            && receipt.profile_name == expected.profile_name,
        "runtime features/profile identity differs",
    )?;
    for command in &receipt.commands {
        require(
            command.status == 0,
            "success receipt contains a failed producer command",
        )?;
        command.stdout.verify()?;
        command.stderr.verify()?;
    }
    let builds: Vec<_> = receipt
        .commands
        .iter()
        .filter(|c| c.program == "cargo" && c.arguments.first().is_some_and(|a| a == "build"))
        .collect();
    require(
        builds.len() == 1,
        "runtime receipt requires one explicit successful Cargo leaf build",
    )?;
    let arguments = &builds[0].arguments;
    for flag in [
        "--locked",
        "--offline",
        "--lib",
        "--message-format=json-render-diagnostics",
    ] {
        require(
            arguments.iter().any(|a| a == flag),
            format!("producer command lacks required {flag}"),
        )?;
    }
    for (option, value) in [
        ("-p", expected.package.name.as_str()),
        ("--features", "allocator-fixture"),
        ("--profile", expected.profile_name),
    ] {
        require(
            arguments
                .windows(2)
                .any(|w| w[0] == option && w[1] == value),
            format!("producer command has wrong/missing {option} {value}"),
        )?;
    }
    require(
        arguments
            .windows(2)
            .any(|w| w[0] == "--target-dir" && Path::new(&w[1]) == receipt.target_directory),
        "producer command target directory differs",
    )?;
    let build_messages = fs::read_to_string(&builds[0].stdout.path).map_err(|e| e.to_string())?;
    let selected_again = select_artifact(
        &build_messages,
        expected.package,
        expected.features,
        expected.profile,
        &receipt.target_directory,
    )?;
    require(
        selected_again == receipt.cargo_artifact,
        "producer current-Cargo artifact selection does not match receipt",
    )?;
    require(
        real_file(selected_path)? == receipt.artifact.path,
        "selected runtime path differs from qualified loaded-artifact path",
    )?;
    receipt.artifact.verify()?;
    require(
        receipt.qualification.initializer.name == expected.initializer
            && receipt.qualification.init_array_slot != 0
            && !receipt.qualification.init_array_resolution.is_empty(),
        "runtime constructor identity is unqualified",
    )?;
    let exports: BTreeSet<_> = receipt
        .qualification
        .exports
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    require(
        exports == BTreeSet::from(EXPORTS),
        "runtime required export identity differs",
    )?;
    require(
        receipt
            .qualification
            .allocation_shims
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == BTreeSet::from(SHIMS),
        "runtime lacks four qualified compiler allocation shims",
    )?;
    require(
        receipt
            .qualification
            .allocation_shims
            .values()
            .all(|s| s.binding == sym::STB_LOCAL),
        "runtime allocation shims are preemptible",
    )?;
    let routes: BTreeSet<_> = receipt
        .qualification
        .call_paths
        .keys()
        .chain(receipt.qualification.inline_routes.keys())
        .map(String::as_str)
        .collect();
    require(
        routes
            == BTreeSet::from([
                "m1_alloc->__rust_alloc",
                "m1_alloc->__rust_alloc_zeroed",
                "m1_realloc->__rust_realloc",
                "m1_dealloc->__rust_dealloc",
            ])
            && receipt.qualification.call_paths.len() + receipt.qualification.inline_routes.len()
                == 4
            && receipt
                .qualification
                .call_paths
                .values()
                .all(|p| p.len() >= 2),
        "runtime compiled allocation paths are incomplete/duplicated",
    )?;
    receipt.qualification.readelf_versions.verify()?;
    receipt.qualification.objdump_disassembly.verify()?;
    let independent = qualify_elf_from_evidence(
        selected_path,
        expected.initializer,
        receipt.qualification.readelf_versions.clone(),
        receipt.qualification.readelf_symbols_relocations.clone(),
        receipt.qualification.objdump_disassembly.clone(),
    )?;
    require(
        independent == receipt.qualification,
        "runtime ELF/evidence requalification differs from producer record",
    )?;
    require(
        receipt.executed_tests == 0 && !receipt.full_m1_pass_claimed,
        "artifact receipt mislabels build evidence as M1 runtime execution",
    )?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_endpoints_require_owned_allocator_allocation_methods() {
        assert!(qualifies_owned_endpoint(
            "_RNvPrivatePatchAllocatorGlobalAlloc5alloc"
        ));
        assert!(qualifies_owned_endpoint(
            "_RNvPrivateToolAllocatorGlobalAlloc12alloc_zeroed"
        ));
        assert!(qualifies_owned_endpoint("_RNvToolHeap8allocate"));
        for name in [
            "_RNvGuestAllocatorGlobalAlloc5alloc",
            "_RNvPatchAllocatorGlobalAlloc5alloc",
            "_RNvPrivatePatchAllocatorGlobalAlloc7dealloc",
            "_RNvToolHeap10deallocate",
            "__rust_alloc",
        ] {
            assert!(!qualifies_owned_endpoint(name), "{name}");
        }
        assert!(allocation_wrapper(
            "_RNvNtCs1ahl_5alloc5alloc5allocCsh_16reverie_liteinst"
        ));
        assert!(!allocation_wrapper("_RNvNtCscore9panicking_alloc"));
    }

    #[test]
    fn refuses_new_or_unrecognized_guest_symbol_versions() {
        for version in [
            "GLIBC_2.35",
            "GLIBC_ABI_DT_RELR",
            "GCC_3.0",
            "GLIBC_2.",
            "GLIBC_2.34.0.1",
        ] {
            assert!(
                required_versions(&format!("Name: {version} Flags: none")).is_err(),
                "{version}"
            );
        }
        assert_eq!(
            required_versions("Name: GLIBC_2.2.5 Flags: none\nName: GLIBC_2.34 Flags: none")
                .unwrap(),
            ["GLIBC_2.2.5", "GLIBC_2.34"]
        );
    }

    #[test]
    fn credits_exact_local_relative_got_route_and_direct_tailcall() {
        let functions = BTreeMap::from([(0x1000, 16), (0x2000, 16), (0x3000, 16)]);
        let got = BTreeMap::from([(0x4000, 0x2000)]);
        let text = "0000000000001000 <m1_alloc>:\n1000: call *0x2ffa(%rip) # 4000 <_DYNAMIC+0x10>\n0000000000002000 <wrapper>:\n2000: jmp 3000 <__rust_alloc>\n0000000000003000 <__rust_alloc>:\n3000: ret\n";
        let (graph, edges) = disassembly_edges(text, &functions, &got).unwrap();
        assert_eq!(
            shortest_path(&graph, 0x1000, 0x3000).unwrap(),
            [0x1000, 0x2000, 0x3000]
        );
        assert_eq!(
            edges,
            [GotEdge {
                source_function: 0x1000,
                instruction: 0x1000,
                slot: 0x4000,
                target_function: 0x2000
            }]
        );
        let raw = "0000000000001000 <m1_alloc>:\n1000: ff 15 fa 2f 00 00 call *0x2ffa(%rip) # 4000 <_DYNAMIC+0x10>\n1006: c3 ret\n0000000000002000 <wrapper>:\n2000: e9 fb 0f 00 00 jmp 3000 <__rust_alloc>\n0000000000003000 <__rust_alloc>:\n3000: c3 ret\n";
        let expected = BTreeMap::from([
            (0x1000, vec![0xff, 0x15, 0xfa, 0x2f, 0x00, 0x00, 0xc3]),
            (0x2000, vec![0xe9, 0xfb, 0x0f, 0x00, 0x00]),
            (0x3000, vec![0xc3]),
        ]);
        verify_disassembly_bytes(raw, &expected).unwrap();
        let (raw_graph, raw_edges) = disassembly_edges(raw, &functions, &got).unwrap();
        assert_eq!(raw_graph, graph);
        assert_eq!(raw_edges, edges);
        for incomplete in [
            raw.split("0000000000003000").next().unwrap().to_owned(),
            raw.replace("1006: c3 ret\n", ""),
            raw.replace("1006: c3", "1007: c3"),
            raw.replace("ff 15 fa 2f", "ff 15 fb 2f"),
            raw.replace("3000: c3 ret", "3000: c3 ret\n3001: c3 ret"),
            format!("{raw}0000000000003000 <__rust_alloc>:\n3000: c3 ret\n"),
            text.to_owned(),
        ] {
            assert!(verify_disassembly_bytes(&incomplete, &expected).is_err());
        }
    }

    #[test]
    fn unresolved_or_preemptible_indirect_calls_do_not_earn_a_path() {
        let functions = BTreeMap::from([(0x1000, 16), (0x2000, 16)]);
        for instruction in ["call *%rax", "call *0x2ffa(%rip) # 4000 <malloc@GOT>"] {
            let text = format!(
                "0000000000001000 <m1_alloc>:\n1000: {instruction}\n0000000000002000 <__rust_alloc>:\n2000: ret\n"
            );
            let (graph, edges) = disassembly_edges(&text, &functions, &BTreeMap::new()).unwrap();
            assert!(edges.is_empty());
            assert!(shortest_path(&graph, 0x1000, 0x2000).is_err());
        }
    }

    #[test]
    fn missing_build_finished_is_a_failed_current_artifact_selection() {
        let package = PackageIdentity {
            id: "exact-id".to_owned(),
            name: "reverie-liteinst-preload".to_owned(),
            manifest: "/source/leaf/Cargo.toml".into(),
            source: None,
            target: "reverie_liteinst_preload".to_owned(),
            kind: vec!["cdylib".to_owned()],
            crate_types: vec!["cdylib".to_owned()],
            target_source: "/source/leaf/src/lib.rs".into(),
        };
        assert!(
            select_artifact(
                "",
                &package,
                &["allocator-fixture".to_owned()],
                &Value::Null,
                Path::new("/separate-target")
            )
            .unwrap_err()
            .contains("build-finished")
        );
        assert!(
            select_artifact(
                "{\"reason\":\"build-finished\",\"success\":false}",
                &package,
                &[],
                &Value::Null,
                Path::new("/separate-target")
            )
            .is_err()
        );
    }
}
