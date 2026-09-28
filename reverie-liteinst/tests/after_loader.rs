//! End-to-end coverage for the constructor-disabled after-loader host runner.
//!
//! This test consumes retained, independently reviewed staging artifacts named by
//! `REVERIE_LITEINST_AFTER_LOADER_FIXTURE`,
//! `REVERIE_LITEINST_AFTER_LOADER_RUNTIME`,
//! `REVERIE_LITEINST_AFTER_LOADER_MARKER`, and
//! three opaque manifest path/SHA-256 pairs. Run it in Cargo's release profile
//! with default features disabled and only `liteinst-after-loader-experiment`
//! enabled. A separate review must approve each complete manifest and supply
//! its exact digest. This harness neither constructs nor interprets manifest
//! bytes; it only checks their independently supplied identities before asking
//! the public binder to enforce their contract.
//!
//! This is runner and restoration coverage. The unchanged backend parity cells
//! remain the acceptance test for the original RDTSC divergence.

#![cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    feature = "liteinst-after-loader-experiment"
))]

#[cfg(feature = "preload-constructor")]
compile_error!(
    "after_loader requires --no-default-features --features liteinst-after-loader-experiment"
);

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use goblin::elf::Elf;
use goblin::elf::program_header;
use liteinst2::scanner::InstructionScanner;
use liteinst2::scanner::ScanError;
use reverie::BackendBootstrapEntropy;
use reverie::Errno;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tid;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::process::Command;
use reverie::process::Stdio;
use reverie::syscalls::Gettid;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::LiteinstBackend;
use reverie_liteinst::LiteinstBackendStatsSource;
use reverie_liteinst::LiteinstDispatchPath;
use reverie_ptrace::LiteinstAfterLoaderAuthenticationFailure;
use reverie_ptrace::LiteinstAfterLoaderAuthenticationStage;
use reverie_ptrace::LiteinstAfterLoaderConfig;
use reverie_ptrace::LiteinstAfterLoaderProfile;
use reverie_ptrace::LiteinstCallerDiagnostics;
use reverie_ptrace::LiteinstCallerObservation;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

const FIXTURE_ENV: &str = "REVERIE_LITEINST_AFTER_LOADER_FIXTURE";
const RUNTIME_ENV: &str = "REVERIE_LITEINST_AFTER_LOADER_RUNTIME";
const MARKER_ENV: &str = "REVERIE_LITEINST_AFTER_LOADER_MARKER";
const FOUR_CANONICAL_MANIFEST_ENV: &str = "REVERIE_LITEINST_AFTER_LOADER_MANIFEST_FOUR_CANONICAL";
const FOUR_CANONICAL_MANIFEST_SHA256_ENV: &str =
    "REVERIE_LITEINST_AFTER_LOADER_MANIFEST_FOUR_CANONICAL_SHA256";
const ONE_CALL_MANIFEST_ENV: &str = "REVERIE_LITEINST_AFTER_LOADER_MANIFEST_ONE_CALL";
const ONE_CALL_MANIFEST_SHA256_ENV: &str = "REVERIE_LITEINST_AFTER_LOADER_MANIFEST_ONE_CALL_SHA256";
const UNPATCHABLE_MANIFEST_ENV: &str = "REVERIE_LITEINST_AFTER_LOADER_MANIFEST_UNPATCHABLE";
const UNPATCHABLE_MANIFEST_SHA256_ENV: &str =
    "REVERIE_LITEINST_AFTER_LOADER_MANIFEST_UNPATCHABLE_SHA256";
const MAX_REVIEWED_MANIFEST_BYTES: usize = 64 * 1024;
const GETPID_SENTINEL: i64 = 0x4c49_5445;
// Mechanism-only input for the after-loader test Tools. This fixed value is
// not a ptrace-parity oracle and must not be used by a production Tool.
const TEST_PTMALLOC_TCACHE_KEY: [u8; 8] = *b"liteinst";
const CANONICAL_STDOUT: &[u8] = b"guest-entered-v1\n\
restoration=preinit-constructor-main-ok\n\
environment=sentinel-preserved-loader-selectors-absent\n\
getpid=4c495445 calls=4 stages=3\n";

fn test_backend_bootstrap_entropy(request: BackendBootstrapEntropy) -> Result<[u8; 8], Errno> {
    match request {
        BackendBootstrapEntropy::PtmallocTcacheKey => Ok(TEST_PTMALLOC_TCACHE_KEY),
        _ => Err(Errno::ENOSYS),
    }
}

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

    fn handle_backend_bootstrap_entropy(
        &self,
        _thread_state: &mut Self::ThreadState,
        request: BackendBootstrapEntropy,
    ) -> Result<[u8; 8], Errno> {
        test_backend_bootstrap_entropy(request)
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct TimerDeoptConfig {
    logical_syscall_rip: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum TimerDeoptObservation {
    Syscall {
        ordinal: u8,
        clock: u64,
        rip: u64,
        clock_after_timer_request: Option<u64>,
        clock_after_injection: Option<u64>,
    },
    Timer {
        ordinal: u8,
        clock: u64,
        rip: u64,
        clock_after_rearm: Option<u64>,
        clock_after_injection: Option<u64>,
    },
}

impl TimerDeoptObservation {
    fn clock(&self) -> u64 {
        match self {
            Self::Syscall { clock, .. } | Self::Timer { clock, .. } => *clock,
        }
    }

    fn rip(&self) -> u64 {
        match self {
            Self::Syscall { rip, .. } | Self::Timer { rip, .. } => *rip,
        }
    }
}

#[derive(Debug, Default)]
struct TimerDeoptLog(Mutex<Vec<TimerDeoptObservation>>);

#[reverie::global_tool]
impl GlobalTool for TimerDeoptLog {
    type Request = TimerDeoptObservation;
    type Response = ();
    type Config = TimerDeoptConfig;

    async fn receive_rpc(&self, _from: Tid, observation: TimerDeoptObservation) {
        self.0.lock().unwrap().push(observation);
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct TimerDeoptThreadState {
    syscall_callbacks: u8,
    timer_callbacks: u8,
}

#[derive(Default)]
struct TimerDeoptGetpid;

#[reverie::tool]
impl Tool for TimerDeoptGetpid {
    type GlobalState = TimerDeoptLog;
    type ThreadState = TimerDeoptThreadState;

    fn subscriptions(_config: &TimerDeoptConfig) -> Subscription {
        [Sysno::getpid].into_iter().collect()
    }

    fn handle_backend_bootstrap_entropy(
        &self,
        _thread_state: &mut Self::ThreadState,
        request: BackendBootstrapEntropy,
    ) -> Result<[u8; 8], Errno> {
        test_backend_bootstrap_entropy(request)
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(syscall.number(), Sysno::getpid);
        let ordinal = {
            let state = guest.thread_state_mut();
            state.syscall_callbacks += 1;
            state.syscall_callbacks
        };
        assert!(
            (1..=4).contains(&ordinal),
            "fixture produced an extra getpid callback"
        );
        let expected_rip = guest.config().logical_syscall_rip;
        let rip = guest.regs().await.rip;
        assert_eq!(
            rip, expected_rip,
            "getpid callback did not expose the logical post-syscall RIP"
        );
        let clock = guest.read_clock()?;
        let mut clock_after_timer_request = None;
        let mut clock_after_injection = None;

        match ordinal {
            2 => {
                guest.set_timer_precise(TimerSchedule::Rcbs(20_000_000))?;
                assert_eq!(
                    guest.regs().await.rip,
                    rip,
                    "arming the transport changed the installed callback RIP"
                );
                clock_after_timer_request = Some(guest.read_clock()?);
                assert_eq!(
                    clock_after_timer_request,
                    Some(clock),
                    "arming the transport retired guest work"
                );
                assert!(guest.inject(Gettid::default()).await? > 0);
                assert_eq!(
                    guest.regs().await.rip,
                    rip,
                    "injection changed the installed callback RIP"
                );
                clock_after_injection = Some(guest.read_clock()?);
                assert_eq!(
                    clock_after_injection,
                    Some(clock),
                    "installed-callback injection retired guest work"
                );
            }
            3 => {
                guest.set_timer_precise(TimerSchedule::Rcbs(1))?;
                assert_eq!(
                    guest.regs().await.rip,
                    rip,
                    "replacing the transport request changed the fallback callback RIP"
                );
                clock_after_timer_request = Some(guest.read_clock()?);
                assert_eq!(
                    clock_after_timer_request,
                    Some(clock),
                    "replacing the transport request retired guest work"
                );
            }
            1 | 4 => {}
            _ => unreachable!(),
        }

        guest
            .send_rpc(TimerDeoptObservation::Syscall {
                ordinal,
                clock,
                rip,
                clock_after_timer_request,
                clock_after_injection,
            })
            .await;
        Ok(GETPID_SENTINEL)
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        let (ordinal, syscall_callbacks) = {
            let state = guest.thread_state_mut();
            state.timer_callbacks += 1;
            (state.timer_callbacks, state.syscall_callbacks)
        };
        assert_eq!(
            syscall_callbacks, 3,
            "timer fired outside the interval between fixture calls three and four"
        );
        assert!(
            (1..=2).contains(&ordinal),
            "transport delivered an extra timer callback"
        );
        let rip = guest.regs().await.rip;
        let clock = guest.read_clock().expect("read timer callback clock");
        let mut clock_after_rearm = None;
        let mut clock_after_injection = None;

        if ordinal == 1 {
            guest
                .set_timer_precise(TimerSchedule::Rcbs(1))
                .expect("rearm one-RCB timer from its callback");
            assert_eq!(
                guest.regs().await.rip,
                rip,
                "timer rearm changed the interrupted guest RIP"
            );
            clock_after_rearm = Some(guest.read_clock().expect("read clock after timer rearm"));
            assert_eq!(
                clock_after_rearm,
                Some(clock),
                "timer rearm retired guest work"
            );
            assert!(
                guest
                    .inject(Gettid::default())
                    .await
                    .expect("inject gettid from timer callback")
                    > 0
            );
            assert_eq!(
                guest.regs().await.rip,
                rip,
                "timer-callback injection changed the interrupted guest RIP"
            );
            clock_after_injection = Some(
                guest
                    .read_clock()
                    .expect("read clock after timer-callback injection"),
            );
            assert_eq!(
                clock_after_injection,
                Some(clock),
                "timer-callback injection retired guest work"
            );
        }

        guest
            .send_rpc(TimerDeoptObservation::Timer {
                ordinal,
                clock,
                rip,
                clock_after_rearm,
                clock_after_injection,
            })
            .await;
    }
}

fn required_staged_file(variable: &str) -> PathBuf {
    let value = std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("{variable} must name one retained staging artifact"));
    let path = PathBuf::from(value);
    assert!(path.is_absolute(), "{variable} must be an absolute path");
    let metadata = std::fs::symlink_metadata(&path)
        .unwrap_or_else(|error| panic!("inspect {variable}: {error}"));
    assert!(
        metadata.file_type().is_file(),
        "{variable} must name a regular file without a terminal symlink"
    );
    let canonical = path
        .canonicalize()
        .unwrap_or_else(|error| panic!("canonicalize {variable}: {error}"));
    assert_eq!(
        canonical.as_os_str().as_bytes(),
        path.as_os_str().as_bytes(),
        "{variable} must use its exact canonical absolute path"
    );
    canonical
}

fn required_reviewed_sha256(variable: &str) -> String {
    let digest = std::env::var(variable)
        .unwrap_or_else(|_| panic!("{variable} must contain one independently reviewed SHA-256"));
    assert!(
        is_lower_hex(&digest, 64),
        "{variable} must be exactly 64 lowercase hexadecimal characters"
    );
    digest
}

#[derive(Debug)]
struct ReviewedManifestInput {
    path: PathBuf,
    digest: String,
}

impl ReviewedManifestInput {
    fn from_environment(path_variable: &str, digest_variable: &str) -> Self {
        Self {
            path: required_staged_file(path_variable),
            digest: required_reviewed_sha256(digest_variable),
        }
    }
}

#[derive(Debug)]
struct StagedInputs {
    fixture: PathBuf,
    runtime: PathBuf,
    marker: PathBuf,
    four_canonical: ReviewedManifestInput,
    one_call: ReviewedManifestInput,
    unpatchable: ReviewedManifestInput,
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
        fixture: required_staged_file(FIXTURE_ENV),
        runtime: required_staged_file(RUNTIME_ENV),
        marker: required_staged_file(MARKER_ENV),
        four_canonical: ReviewedManifestInput::from_environment(
            FOUR_CANONICAL_MANIFEST_ENV,
            FOUR_CANONICAL_MANIFEST_SHA256_ENV,
        ),
        one_call: ReviewedManifestInput::from_environment(
            ONE_CALL_MANIFEST_ENV,
            ONE_CALL_MANIFEST_SHA256_ENV,
        ),
        unpatchable: ReviewedManifestInput::from_environment(
            UNPATCHABLE_MANIFEST_ENV,
            UNPATCHABLE_MANIFEST_SHA256_ENV,
        ),
    };
    let distinct = [
        &inputs.fixture,
        &inputs.runtime,
        &inputs.marker,
        &inputs.four_canonical.path,
        &inputs.one_call.path,
        &inputs.unpatchable.path,
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(
        distinct.len(),
        6,
        "fixture, runtime, marker and three reviewed manifests must be distinct retained files"
    );
    inputs
}

fn elf_symbol_prefix(path: &Path, name: &str, len: usize) -> (u64, Vec<u8>) {
    let bytes = std::fs::read(path).expect("read fixed executable for symbol validation");
    let elf = Elf::parse(&bytes).expect("parse fixed executable for symbol validation");
    let symbol = elf
        .syms
        .iter()
        .find(|symbol| elf.strtab.get_at(symbol.st_name) == Some(name))
        .unwrap_or_else(|| panic!("fixed executable lacks symbol {name:?}"));
    let end = symbol
        .st_value
        .checked_add(u64::try_from(len).expect("symbol prefix length fits u64"))
        .expect("symbol prefix address overflows");
    let segment = elf
        .program_headers
        .iter()
        .find(|segment| {
            segment.p_type == program_header::PT_LOAD
                && segment.p_vaddr <= symbol.st_value
                && segment
                    .p_vaddr
                    .checked_add(segment.p_filesz)
                    .is_some_and(|segment_end| end <= segment_end)
        })
        .unwrap_or_else(|| panic!("symbol {name:?} prefix is outside a file-backed PT_LOAD"));
    let offset = segment
        .p_offset
        .checked_add(symbol.st_value - segment.p_vaddr)
        .and_then(|offset| usize::try_from(offset).ok())
        .expect("symbol file offset is not representable");
    let end = offset
        .checked_add(len)
        .expect("symbol prefix file range overflows");
    (
        symbol.st_value,
        bytes
            .get(offset..end)
            .unwrap_or_else(|| panic!("symbol {name:?} prefix exceeds executable bytes"))
            .to_vec(),
    )
}

fn assert_unpatchable_getpid_prefix(fixture: &Path) {
    let (site, prefix) = elf_symbol_prefix(fixture, "reverie_liteinst_unpatchable_getpid_site", 32);
    assert_eq!(
        &prefix[..6],
        &[0x0f, 0x05, 0xeb, 0x18, 0x0f, 0x04],
        "unpatchable getpid fixture lost its exact syscall/jump/invalid prefix"
    );
    assert!(
        matches!(
            InstructionScanner::default().scan_prefix(&prefix, site, 8),
            Err(ScanError::InvalidInstruction { address, offset })
                if address == site + 4 && offset == 4
        ),
        "production prefix scanner did not refuse the fixed invalid encoding"
    );
}

fn assert_stack_getpid_prefix(fixture: &Path) {
    let (site, prefix) = elf_symbol_prefix(fixture, "reverie_liteinst_stack_getpid_site", 8);
    assert_eq!(
        prefix,
        [0x0f, 0x05, 0x4c, 0x89, 0xe4, 0x41, 0x5c, 0xc3],
        "controlled-stack getpid fixture lost its exact syscall/restore/return prefix"
    );
    let scan = InstructionScanner::default()
        .scan_prefix(&prefix, site, 8)
        .expect("controlled-stack getpid site is no longer patchable");
    assert_eq!(scan.instructions()[0].address(), site);
    assert_eq!(scan.instructions()[0].len(), 2);
}

fn sha256_file(path: &Path) -> String {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .expect("open reviewed manifest input")
        .take(MAX_REVIEWED_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .expect("read reviewed manifest input");
    assert!(
        !bytes.is_empty() && bytes.len() <= MAX_REVIEWED_MANIFEST_BYTES,
        "reviewed manifest input is empty or exceeds its byte bound"
    );
    format!("{:x}", Sha256::digest(bytes))
}

fn bind_reviewed_profile(
    manifest: &ReviewedManifestInput,
    expected_marker: &Path,
) -> LiteinstAfterLoaderConfig {
    let observed = sha256_file(&manifest.path);
    assert_eq!(
        observed, manifest.digest,
        "retained manifest differs from its independently supplied reviewed SHA-256"
    );
    // SAFETY: `manifest.digest` is supplied independently by the reviewed
    // staging receipt. This harness neither creates the manifest nor derives
    // the approval value from its bytes; the hash above is only a refusal gate.
    let profile =
        unsafe { LiteinstAfterLoaderProfile::review_dynamic_x86_64_et_exec_v2(&manifest.digest) }
            .expect("approve exact reviewed after-loader manifest digest");
    let caller = profile
        .bind_reviewed_manifest(&manifest.path)
        .expect("bind exact reviewed after-loader manifest");
    assert_eq!(
        caller.runtime_marker_path().as_os_str().as_bytes(),
        expected_marker.as_os_str().as_bytes(),
        "authenticated manifest runtime marker differs from the separately retained marker"
    );
    caller
}

fn four_canonical_environment() -> BTreeMap<OsString, OsString> {
    BTreeMap::from([
        (
            OsString::from("LITEINST_CALLER_OUTPUT"),
            OsString::from("canonical-v1"),
        ),
        (
            OsString::from("LITEINST_CALLER_SENTINEL"),
            OsString::from("preserved"),
        ),
    ])
}

fn one_call_environment() -> BTreeMap<OsString, OsString> {
    BTreeMap::from([
        (OsString::from("LITEINST_CALLER_CALLS"), OsString::from("1")),
        (
            OsString::from("LITEINST_CALLER_SENTINEL"),
            OsString::from("preserved"),
        ),
    ])
}

fn unpatchable_environment() -> BTreeMap<OsString, OsString> {
    BTreeMap::from([
        (OsString::from("LITEINST_CALLER_CALLS"), OsString::from("1")),
        (
            OsString::from("LITEINST_CALLER_SENTINEL"),
            OsString::from("preserved"),
        ),
        (
            OsString::from("LITEINST_CALLER_SITE"),
            OsString::from("unpatchable"),
        ),
    ])
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

fn assert_fixture_stdout(stdout: &[u8], getpid_calls: usize, unpatchable_site: bool) {
    let stdout = std::str::from_utf8(stdout).expect("fixture stdout is not UTF-8");
    assert!(stdout.ends_with('\n'), "fixture stdout is unterminated");
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 9, "unexpected fixture stdout: {stdout:?}");
    assert_eq!(lines[0], "guest-entered-v1");
    let samples = ["preinit", "constructor", "main"]
        .into_iter()
        .enumerate()
        .map(|(index, label)| parse_sample(lines[index + 1], label))
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
    assert_eq!(lines[4], "env LITEINST_CALLER_SENTINEL=preserved");
    assert_eq!(lines[5], "env LD_PRELOAD=<absent>");
    assert_eq!(lines[6], "env REVERIE_LITEINST_HOST_RUNTIME=<absent>");
    assert_eq!(lines[7], "env REVERIE_LITEINST_TOOL=<absent>");
    match (getpid_calls, unpatchable_site) {
        (1, false) => assert_eq!(lines[8], "getpid=4c495445 calls=1 stages=3"),
        (1, true) => assert_eq!(
            lines[8],
            "getpid=4c495445 calls=1 site=unpatchable stages=3"
        ),
        (4, false) => assert_eq!(lines[8], "getpid=4c495445 stages=3"),
        _ => panic!("test requested an unsupported getpid call count"),
    }
}

fn assert_dispatch_stats(
    stats: &LiteinstBackendStatsSource,
    first_site_seccomp: u64,
    ptrace_installation: u64,
    direct_hook: u64,
    unpatchable_or_other_fallback: u64,
) {
    assert_eq!(
        stats.snapshot().process_reports(),
        0,
        "ptrace-host after-loader stats unexpectedly contain guest reports: {stats}"
    );
    let paths = stats.dispatch_path_counts();
    for path in LiteinstDispatchPath::ALL {
        let expected = match path {
            LiteinstDispatchPath::FirstSiteSeccomp => first_site_seccomp,
            LiteinstDispatchPath::PtraceInstallation => ptrace_installation,
            LiteinstDispatchPath::DirectHook => direct_hook,
            LiteinstDispatchPath::UnpatchableOrOtherFallback => unpatchable_or_other_fallback,
            _ => 0,
        };
        assert_eq!(
            paths.count(path),
            expected,
            "unexpected typed dispatch count for {path}: {stats}"
        );
    }
}

fn assert_one_installed_site(stats: &LiteinstBackendStatsSource) {
    assert_eq!(stats.decision_counts(), [0, 1, 0, 0]);
    assert_eq!(stats.patch_candidates(), 1);
    assert_eq!(stats.distinct_rips(), 1);
}

fn assert_tool_callback_counts(
    diagnostics: &LiteinstCallerDiagnostics,
    seccomp: usize,
    installed: usize,
) {
    let observations = diagnostics.observations();
    assert_eq!(
        observations
            .iter()
            .filter(|observation| {
                observation.operation == "Tool callback: Tool::handle_syscall_event(seccomp)"
            })
            .count(),
        seccomp,
        "unexpected seccomp Tool callback count: {}",
        bounded_diagnostics(diagnostics)
    );
    assert_eq!(
        observations
            .iter()
            .filter(|observation| {
                observation.operation
                    == "Tool callback: Tool::handle_syscall_event(installed completion)"
            })
            .count(),
        installed,
        "unexpected installed-completion Tool callback count: {}",
        bounded_diagnostics(diagnostics)
    );
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
        "guest machine state restored and helper/arena writers isolated",
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
    assert_ptmalloc_bootstrap_entropy_emulations(diagnostics, 1);
}

fn assert_ptmalloc_bootstrap_entropy_emulations(
    diagnostics: &LiteinstCallerDiagnostics,
    expected: usize,
) {
    let observations = diagnostics.observations();
    let matched = observations
        .iter()
        .filter(|observation| {
            observation.operation == "profiled ptmalloc bootstrap entropy emulated"
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matched.len(),
        expected,
        "profiled ptmalloc bootstrap entropy count changed: {}",
        bounded_diagnostics(diagnostics)
    );
    let expected_digest = Sha256::digest(TEST_PTMALLOC_TCACHE_KEY)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let expected_detail = format!("length=8 flags=1 tcache_key_sha256={expected_digest}");
    for observation in matched {
        assert_eq!(
            observation.detail, expected_detail,
            "profiled ptmalloc entropy receipt is not exact: {observation:?}"
        );
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ClockTrajectorySample {
    operation: String,
    raw_clock: Option<u64>,
}

const ENTRY_CLOCK_PREFIX: [&str; 6] = [
    "entry held",
    "entry registers and XSTATE",
    "entry original stack",
    "entry random and canary",
    "entry signals and descriptors",
    "entry maps",
];

const FOUR_CALL_CLOCK_MILESTONES: [&str; 8] = [
    "entry held",
    "guest machine state restored and helper/arena writers isolated",
    "first ordinary guest event after restoration",
    "Tool callback: Tool::handle_syscall_event(seccomp)",
    "Tool callback: Tool::handle_syscall_event(installed completion)",
    "Tool callback: Tool::handle_syscall_event(installed completion)",
    "Tool callback: Tool::handle_syscall_event(installed completion)",
    "physical event partition validated",
];

fn exact_four_call_clock_trajectory(
    diagnostics: &LiteinstCallerDiagnostics,
) -> Vec<ClockTrajectorySample> {
    let observations = diagnostics.observations();
    let (entry_index, entry) = one_observation(&observations, "entry held", diagnostics);
    let (restored_index, _) = one_observation(
        &observations,
        "guest machine state restored and helper/arena writers isolated",
        diagnostics,
    );
    assert!(entry_index < restored_index);
    let entry_clock = entry
        .raw_clock
        .expect("entry diagnostic lacks the persistent ptrace clock");
    let trajectory = observations[entry_index..]
        .iter()
        .map(|observation| ClockTrajectorySample {
            operation: observation.operation.clone(),
            raw_clock: observation.raw_clock,
        })
        .collect::<Vec<_>>();
    let (terminal, timed_trajectory) = trajectory
        .split_last()
        .expect("entry-to-terminal trajectory is empty");
    assert_eq!(
        terminal,
        &ClockTrajectorySample {
            operation: "physical event partition validated".to_owned(),
            raw_clock: None,
        },
        "only the final physical-partition receipt may omit the persistent ptrace clock: {trajectory:?}"
    );
    assert!(
        timed_trajectory
            .iter()
            .all(|sample| sample.raw_clock.is_some()),
        "a nonterminal retained trajectory observation lacks the persistent ptrace clock: {trajectory:?}"
    );
    let activation_len = restored_index - entry_index + 1;
    assert!(
        timed_trajectory.len() > activation_len,
        "clock trajectory ends at restoration: {}",
        bounded_diagnostics(diagnostics)
    );
    assert!(
        timed_trajectory[..activation_len]
            .iter()
            .all(|sample| sample.raw_clock == Some(entry_clock)),
        "private activation changed the persistent clock: {trajectory:?}"
    );
    assert_eq!(
        timed_trajectory
            .get(..ENTRY_CLOCK_PREFIX.len())
            .expect("entry clock trajectory is shorter than its fixed prefix")
            .iter()
            .map(|sample| sample.operation.as_str())
            .collect::<Vec<_>>(),
        ENTRY_CLOCK_PREFIX,
        "clock trajectory did not begin with the exact fixed-fixture entry snapshot: {trajectory:?}"
    );
    assert!(
        timed_trajectory
            .windows(2)
            .all(|pair| pair[0].raw_clock.unwrap() <= pair[1].raw_clock.unwrap()),
        "post-restoration clock trajectory moved backwards: {trajectory:?}"
    );
    assert!(
        timed_trajectory
            .last()
            .is_some_and(|sample| sample.raw_clock.is_some_and(|clock| clock > entry_clock)),
        "post-restoration trajectory never advanced: {trajectory:?}"
    );

    let required_operations = trajectory
        .iter()
        .filter(|sample| {
            matches!(
                sample.operation.as_str(),
                "entry held"
                    | "guest machine state restored and helper/arena writers isolated"
                    | "first ordinary guest event after restoration"
                    | "Tool callback: Tool::handle_syscall_event(seccomp)"
                    | "Tool callback: Tool::handle_syscall_event(installed completion)"
                    | "physical event partition validated"
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        required_operations
            .iter()
            .map(|sample| sample.operation.as_str())
            .collect::<Vec<_>>(),
        FOUR_CALL_CLOCK_MILESTONES,
        "clock trajectory did not contain the independently specified fixed-fixture milestones exactly once and in order: {trajectory:?}"
    );
    let callbacks = &required_operations[3..7];
    assert!(
        callbacks
            .windows(2)
            .all(|pair| pair[0].raw_clock.unwrap() < pair[1].raw_clock.unwrap()),
        "successive syscall callbacks did not make strict fine-grained clock progress: {callbacks:?}"
    );
    trajectory
}

fn exact_clock_trajectories_match(
    left: &[ClockTrajectorySample],
    right: &[ClockTrajectorySample],
) -> bool {
    left == right
}

const TIMER_DEOPT_CLOCK_MILESTONES: [&str; 11] = [
    "entry held",
    "guest machine state restored and helper/arena writers isolated",
    "first ordinary guest event after restoration",
    "Tool callback: Tool::handle_syscall_event(seccomp)",
    "Tool callback: Tool::handle_syscall_event(installed completion)",
    "timer transport deoptimized installed tail",
    "Tool callback: Tool::handle_syscall_event(seccomp)",
    "Tool callback: Tool::handle_timer_event",
    "Tool callback: Tool::handle_timer_event",
    "Tool callback: Tool::handle_syscall_event(seccomp)",
    "physical event partition validated",
];

const TIMER_DEOPT_CALLBACKS: [&str; 6] = [
    "Tool callback: Tool::handle_syscall_event(seccomp)",
    "Tool callback: Tool::handle_syscall_event(installed completion)",
    "Tool callback: Tool::handle_syscall_event(seccomp)",
    "Tool callback: Tool::handle_timer_event",
    "Tool callback: Tool::handle_timer_event",
    "Tool callback: Tool::handle_syscall_event(seccomp)",
];

fn exact_timer_deopt_clock_trajectory(
    diagnostics: &LiteinstCallerDiagnostics,
    expected_logical_rip: u64,
) -> (Vec<ClockTrajectorySample>, Vec<u64>) {
    let observations = diagnostics.observations();
    let (entry_index, entry) = one_observation(&observations, "entry held", diagnostics);
    let (restored_index, _) = one_observation(
        &observations,
        "guest machine state restored and helper/arena writers isolated",
        diagnostics,
    );
    assert!(entry_index < restored_index);
    let entry_clock = entry
        .raw_clock
        .expect("entry diagnostic lacks the persistent ptrace clock");
    let trajectory = observations[entry_index..]
        .iter()
        .map(|observation| ClockTrajectorySample {
            operation: observation.operation.clone(),
            raw_clock: observation.raw_clock,
        })
        .collect::<Vec<_>>();
    let (terminal, timed_trajectory) = trajectory
        .split_last()
        .expect("timer-deopt entry-to-terminal trajectory is empty");
    assert_eq!(
        terminal,
        &ClockTrajectorySample {
            operation: "physical event partition validated".to_owned(),
            raw_clock: None,
        },
        "only the final physical-partition receipt may omit the persistent ptrace clock: {trajectory:?}"
    );
    assert!(
        timed_trajectory
            .iter()
            .all(|sample| sample.raw_clock.is_some()),
        "a nonterminal timer-deopt observation lacks the persistent ptrace clock: {trajectory:?}"
    );
    let activation_len = restored_index - entry_index + 1;
    assert!(
        timed_trajectory[..activation_len]
            .iter()
            .all(|sample| sample.raw_clock == Some(entry_clock)),
        "private activation changed the persistent clock: {trajectory:?}"
    );
    assert_eq!(
        timed_trajectory
            .get(..ENTRY_CLOCK_PREFIX.len())
            .expect("timer-deopt trajectory is shorter than its fixed entry prefix")
            .iter()
            .map(|sample| sample.operation.as_str())
            .collect::<Vec<_>>(),
        ENTRY_CLOCK_PREFIX,
        "timer-deopt trajectory did not begin with the fixed entry snapshot: {trajectory:?}"
    );
    assert!(
        timed_trajectory
            .windows(2)
            .all(|pair| pair[0].raw_clock.unwrap() <= pair[1].raw_clock.unwrap()),
        "timer-deopt trajectory moved backwards: {trajectory:?}"
    );
    assert!(
        timed_trajectory
            .last()
            .is_some_and(|sample| sample.raw_clock.is_some_and(|clock| clock > entry_clock)),
        "timer-deopt trajectory never advanced: {trajectory:?}"
    );

    let required_operations = trajectory
        .iter()
        .filter(|sample| {
            TIMER_DEOPT_CLOCK_MILESTONES
                .iter()
                .any(|operation| sample.operation == *operation)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        required_operations
            .iter()
            .map(|sample| sample.operation.as_str())
            .collect::<Vec<_>>(),
        TIMER_DEOPT_CLOCK_MILESTONES,
        "timer-deopt milestones are missing, duplicated, or reordered: {trajectory:?}"
    );

    let callbacks = trajectory
        .iter()
        .filter(|sample| sample.operation.starts_with("Tool callback: "))
        .collect::<Vec<_>>();
    assert_eq!(
        callbacks
            .iter()
            .map(|sample| sample.operation.as_str())
            .collect::<Vec<_>>(),
        TIMER_DEOPT_CALLBACKS,
        "timer-deopt Tool callback sequence differs from the fixed six-event schedule: {trajectory:?}"
    );
    let callback_clocks = callbacks
        .iter()
        .map(|sample| {
            sample
                .raw_clock
                .expect("timer-deopt Tool callback lacks its persistent clock")
        })
        .collect::<Vec<_>>();
    assert!(
        callback_clocks.windows(2).all(|pair| pair[0] < pair[1]),
        "successive timer-deopt callbacks did not make strict clock progress: {callbacks:?}"
    );
    assert_eq!(
        callback_clocks[3],
        callback_clocks[2] + 1,
        "the first precise timer callback did not occur exactly one RCB after replacement"
    );
    assert_eq!(
        callback_clocks[4],
        callback_clocks[3] + 1,
        "the rearmed precise timer callback did not occur exactly one RCB later"
    );

    let (tail_index, tail) = one_observation(
        &observations,
        "timer transport deoptimized installed tail",
        diagnostics,
    );
    let installed_index = observations
        .iter()
        .position(|observation| {
            observation.operation
                == "Tool callback: Tool::handle_syscall_event(installed completion)"
        })
        .expect("installed callback diagnostic is absent");
    let third_syscall_index = observations
        .iter()
        .enumerate()
        .filter(|(_, observation)| {
            observation.operation == "Tool callback: Tool::handle_syscall_event(seccomp)"
        })
        .nth(1)
        .map(|(index, _)| index)
        .expect("third fixture syscall diagnostic is absent");
    assert!(
        installed_index < tail_index && tail_index < third_syscall_index,
        "installed-tail deoptimization receipt is outside its exact callback interval"
    );
    assert_eq!(
        tail.raw_clock,
        Some(callback_clocks[1]),
        "installed-tail deoptimization changed the logical callback clock"
    );
    let observed_rip = parse_hex_word(
        diagnostic_field(&tail.detail, "observed_rip="),
        "deoptimized installed-tail observed RIP",
    );
    let relocated_tail = parse_hex_word(
        diagnostic_field(&tail.detail, "relocated_tail="),
        "deoptimized installed-tail relocated RIP",
    );
    assert_eq!(
        observed_rip, expected_logical_rip,
        "installed-tail deoptimization did not preserve the logical post-syscall RIP"
    );
    assert_ne!(
        relocated_tail, observed_rip,
        "installed-tail deoptimization left the Tool-visible generated RIP in place"
    );

    (trajectory, callback_clocks)
}

fn assert_timer_deopt_observations(
    observations: &[TimerDeoptObservation],
    expected_logical_rip: u64,
) -> Vec<u64> {
    assert_eq!(
        observations.len(),
        6,
        "timer-deopt Tool did not report the fixed six-event schedule: {observations:?}"
    );
    let expected = [
        ("syscall", 1_u8),
        ("syscall", 2),
        ("syscall", 3),
        ("timer", 1),
        ("timer", 2),
        ("syscall", 4),
    ];
    for (observation, (kind, ordinal)) in observations.iter().zip(expected) {
        assert_ne!(
            observation.rip(),
            0,
            "callback did not retain a concrete Tool-visible program counter"
        );
        match (observation, kind) {
            (
                TimerDeoptObservation::Syscall {
                    ordinal: actual, ..
                },
                "syscall",
            )
            | (
                TimerDeoptObservation::Timer {
                    ordinal: actual, ..
                },
                "timer",
            ) => {
                assert_eq!(*actual, ordinal, "callback ordinal changed")
            }
            _ => panic!("callback kind changed in fixed schedule: {observations:?}"),
        }
    }
    for observation in [
        &observations[0],
        &observations[1],
        &observations[2],
        &observations[5],
    ] {
        let TimerDeoptObservation::Syscall { rip, .. } = observation else {
            unreachable!()
        };
        assert_eq!(
            *rip, expected_logical_rip,
            "a syscall callback exposed a generated or displaced RIP"
        );
    }

    match &observations[0] {
        TimerDeoptObservation::Syscall {
            clock_after_timer_request,
            clock_after_injection,
            ..
        } => {
            assert_eq!(*clock_after_timer_request, None);
            assert_eq!(*clock_after_injection, None);
        }
        _ => unreachable!(),
    }
    match &observations[1] {
        TimerDeoptObservation::Syscall {
            clock,
            clock_after_timer_request,
            clock_after_injection,
            ..
        } => {
            assert_eq!(*clock_after_timer_request, Some(*clock));
            assert_eq!(*clock_after_injection, Some(*clock));
        }
        _ => unreachable!(),
    }
    match &observations[2] {
        TimerDeoptObservation::Syscall {
            clock,
            clock_after_timer_request,
            clock_after_injection,
            ..
        } => {
            assert_eq!(*clock_after_timer_request, Some(*clock));
            assert_eq!(*clock_after_injection, None);
        }
        _ => unreachable!(),
    }
    match &observations[3] {
        TimerDeoptObservation::Timer {
            clock,
            clock_after_rearm,
            clock_after_injection,
            ..
        } => {
            assert_eq!(*clock_after_rearm, Some(*clock));
            assert_eq!(*clock_after_injection, Some(*clock));
        }
        _ => unreachable!(),
    }
    for observation in [&observations[4], &observations[5]] {
        match observation {
            TimerDeoptObservation::Timer {
                clock_after_rearm,
                clock_after_injection,
                ..
            } => {
                assert_eq!(*clock_after_rearm, None);
                assert_eq!(*clock_after_injection, None);
            }
            TimerDeoptObservation::Syscall {
                clock_after_timer_request,
                clock_after_injection,
                ..
            } => {
                assert_eq!(*clock_after_timer_request, None);
                assert_eq!(*clock_after_injection, None);
            }
        }
    }

    let clocks = observations
        .iter()
        .map(TimerDeoptObservation::clock)
        .collect::<Vec<_>>();
    assert!(
        clocks.windows(2).all(|pair| pair[0] < pair[1]),
        "Tool-observed timer-deopt callback clocks did not strictly advance: {observations:?}"
    );
    assert_eq!(
        clocks[3],
        clocks[2] + 1,
        "Tool did not observe the first replacement timer at exactly one RCB"
    );
    assert_eq!(
        clocks[4],
        clocks[3] + 1,
        "Tool did not observe the rearmed timer at exactly one RCB"
    );
    clocks
}

#[test]
fn exact_clock_trajectory_comparator_refuses_mutation_loss_duplication_and_reordering() {
    let expected = vec![
        ClockTrajectorySample {
            operation: "entry held".to_owned(),
            raw_clock: Some(19_807),
        },
        ClockTrajectorySample {
            operation: "first ordinary guest event after restoration".to_owned(),
            raw_clock: Some(19_824),
        },
        ClockTrajectorySample {
            operation: "Tool callback: installed completion".to_owned(),
            raw_clock: Some(19_836),
        },
        ClockTrajectorySample {
            operation: "physical event partition validated".to_owned(),
            raw_clock: None,
        },
    ];
    assert!(exact_clock_trajectories_match(&expected, &expected));

    let mut mutated = expected.clone();
    *mutated[1].raw_clock.as_mut().unwrap() += 1;
    assert!(!exact_clock_trajectories_match(&expected, &mutated));

    let mut shifted = expected.clone();
    for sample in &mut shifted {
        if let Some(clock) = &mut sample.raw_clock {
            *clock += 1;
        }
    }
    assert!(!exact_clock_trajectories_match(&expected, &shifted));

    let mut missing_clock = expected.clone();
    missing_clock[1].raw_clock = None;
    assert!(!exact_clock_trajectories_match(&expected, &missing_clock));

    let mut lost = expected.clone();
    lost.remove(1);
    assert!(!exact_clock_trajectories_match(&expected, &lost));

    let mut duplicated = expected.clone();
    duplicated.insert(1, duplicated[1].clone());
    assert!(!exact_clock_trajectories_match(&expected, &duplicated));

    let mut reordered = expected.clone();
    reordered.swap(1, 2);
    assert!(!exact_clock_trajectories_match(&expected, &reordered));
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
fn staged_reviewed_profiles_bind_exact_union_graph() {
    let staged = staged_inputs();
    assert_stack_getpid_prefix(&staged.fixture);
    for manifest in [
        &staged.four_canonical,
        &staged.one_call,
        &staged.unpatchable,
    ] {
        let caller = bind_reviewed_profile(manifest, &staged.marker);
        let diagnostics = caller.diagnostics();
        let observations = diagnostics.observations();
        let (_, reviewed_manifest) =
            one_observation(&observations, "reviewed manifest bound", &diagnostics);
        assert_eq!(
            diagnostic_field(&reviewed_manifest.detail, "profile="),
            "DynamicX86_64EtExecV2"
        );
        assert_eq!(
            diagnostic_field(&reviewed_manifest.detail, "manifest_sha256="),
            manifest.digest,
            "reviewed manifest diagnostic does not carry the independently supplied digest"
        );
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
}

#[tokio::test(flavor = "current_thread")]
async fn command_environment_mismatch_is_typed_and_never_enters_the_guest() {
    let staged = staged_inputs();
    let caller = bind_reviewed_profile(&staged.four_canonical, &staged.marker);
    let mut command = Command::new(&staged.fixture);
    let mut mismatched = four_canonical_environment();
    assert_eq!(
        mismatched.insert(
            OsString::from("LITEINST_CALLER_SENTINEL"),
            OsString::from("changed-after-review"),
        ),
        Some(OsString::from("preserved"))
    );
    let mut stdout = tempfile::tempfile().expect("create empty no-entry stdout receipt");
    command
        .env_clear()
        .envs(mismatched)
        .stdout(stdout.try_clone().expect("clone no-entry stdout receipt"))
        .stderr(Stdio::null());

    let error = tokio::time::timeout(
        Duration::from_secs(30),
        LiteinstBackend::run_host_after_loader::<CountRawGetpid>(
            command,
            (),
            &staged.runtime,
            caller,
        ),
    )
    .await
    .expect("command-environment refusal timed out")
    .expect_err("a command-environment mismatch reached the guest");
    let Error::Tool(error) = &error else {
        panic!("environment mismatch lacked a typed Tool refusal: {error}");
    };
    let refusal = error
        .downcast_ref::<LiteinstAfterLoaderAuthenticationFailure>()
        .unwrap_or_else(|| panic!("environment mismatch has the wrong Tool type: {error}"));
    assert_eq!(
        refusal.stage(),
        LiteinstAfterLoaderAuthenticationStage::CommandEnvironmentAuthentication
    );

    stdout
        .seek(SeekFrom::Start(0))
        .expect("rewind no-entry stdout receipt");
    let mut observed = Vec::new();
    stdout
        .read_to_end(&mut observed)
        .expect("read no-entry stdout receipt");
    assert_eq!(
        observed.as_slice(),
        b"",
        "the immediate preinit guest-entered-v1 canary ran before refusal"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stats_output_api_matches_old_api_raw_bytes_for_four_calls() {
    let staged = staged_inputs();
    let old_caller = bind_reviewed_profile(&staged.four_canonical, &staged.marker);
    let old_diagnostics = old_caller.diagnostics();
    let mut old_command = Command::new(&staged.fixture);
    old_command.env_clear().envs(four_canonical_environment());
    let old_result = tokio::time::timeout(
        Duration::from_secs(30),
        LiteinstBackend::run_host_with_output_after_loader::<CountRawGetpid>(
            old_command,
            (),
            &staged.runtime,
            old_caller,
        ),
    )
    .await;
    let (old_output, old_global) = match old_result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!(
            "old output after-loader API failed: {error}; {}",
            bounded_diagnostics(&old_diagnostics)
        ),
        Err(_) => panic!(
            "old output after-loader API timed out; {}",
            bounded_diagnostics(&old_diagnostics)
        ),
    };

    let stats_caller = bind_reviewed_profile(&staged.four_canonical, &staged.marker);
    let stats_diagnostics = stats_caller.diagnostics();
    let mut stats_command = Command::new(&staged.fixture);
    stats_command.env_clear().envs(four_canonical_environment());
    let stats_result = tokio::time::timeout(
        Duration::from_secs(30),
        LiteinstBackend::run_host_with_output_after_loader_and_stats::<CountRawGetpid>(
            stats_command,
            (),
            &staged.runtime,
            stats_caller,
        ),
    )
    .await;
    let (stats_output, stats_global, stats) = match stats_result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!(
            "stats output after-loader API failed: {error}; {}",
            bounded_diagnostics(&stats_diagnostics)
        ),
        Err(_) => panic!(
            "stats output after-loader API timed out; {}",
            bounded_diagnostics(&stats_diagnostics)
        ),
    };

    assert_eq!(old_output.status, ExitStatus::Exited(0), "{old_output:?}");
    assert_eq!(
        stats_output.status,
        ExitStatus::Exited(0),
        "{stats_output:?}"
    );
    assert_eq!(old_output.status, stats_output.status);
    assert_eq!(old_output.stdout.as_slice(), CANONICAL_STDOUT);
    assert_eq!(stats_output.stdout.as_slice(), CANONICAL_STDOUT);
    assert_eq!(old_output.stdout, stats_output.stdout);
    assert_eq!(old_output.stderr.as_slice(), b"");
    assert_eq!(stats_output.stderr.as_slice(), b"");
    assert_eq!(old_output.stderr, stats_output.stderr);
    assert_eq!(
        old_global.0.load(Ordering::SeqCst),
        4,
        "old output API did not receive exactly four raw getpid calls"
    );
    assert_eq!(
        stats_global.0.load(Ordering::SeqCst),
        4,
        "stats output API did not receive exactly four raw getpid calls"
    );
    assert_dispatch_stats(&stats, 1, 1, 3, 0);
    assert_one_installed_site(&stats);
    assert_restoration_diagnostics(&old_diagnostics);
    assert_restoration_diagnostics(&stats_diagnostics);
    let old_trajectory = exact_four_call_clock_trajectory(&old_diagnostics);
    let stats_trajectory = exact_four_call_clock_trajectory(&stats_diagnostics);
    assert!(
        exact_clock_trajectories_match(&old_trajectory, &stats_trajectory),
        "old and stats APIs produced different retained absolute PMU-clock observation trajectories:\nold={old_trajectory:?}\nstats={stats_trajectory:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn installed_hook_timer_transport_deopts_to_exact_ptrace_trajectory() {
    let staged = staged_inputs();
    assert_stack_getpid_prefix(&staged.fixture);
    let (syscall_site, _) =
        elf_symbol_prefix(&staged.fixture, "reverie_liteinst_stack_getpid_site", 8);
    let logical_syscall_rip = syscall_site
        .checked_add(2)
        .expect("fixed fixture syscall RIP overflows");
    let mut retained_trajectories = Vec::new();
    let mut tool_runs = Vec::new();

    for run in 0..2 {
        let caller = bind_reviewed_profile(&staged.four_canonical, &staged.marker);
        let diagnostics = caller.diagnostics();
        let mut command = Command::new(&staged.fixture);
        command.env_clear().envs(four_canonical_environment());
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            LiteinstBackend::run_host_with_output_after_loader_and_stats::<TimerDeoptGetpid>(
                command,
                TimerDeoptConfig {
                    logical_syscall_rip,
                },
                &staged.runtime,
                caller,
            ),
        )
        .await;
        let (output, global, stats) = match result {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => panic!(
                "timer-deopt after-loader run {run} failed: {error}; {}",
                bounded_diagnostics(&diagnostics)
            ),
            Err(_) => panic!(
                "timer-deopt after-loader run {run} timed out; {}",
                bounded_diagnostics(&diagnostics)
            ),
        };

        assert_eq!(
            output.status,
            ExitStatus::Exited(0),
            "run {run}: {output:?}"
        );
        assert_eq!(
            output.stdout.as_slice(),
            CANONICAL_STDOUT,
            "timer-deopt run {run} changed fixed fixture stdout"
        );
        assert_eq!(
            output.stderr.as_slice(),
            b"",
            "timer-deopt run {run} produced stderr"
        );
        assert_dispatch_stats(&stats, 1, 1, 1, 2);
        assert_one_installed_site(&stats);
        assert_eq!(
            stats.deoptimized_fallback_hits(),
            2,
            "both post-deoptimization fixture calls must use the typed fallback"
        );
        assert_tool_callback_counts(&diagnostics, 3, 1);
        assert_ptmalloc_bootstrap_entropy_emulations(&diagnostics, 1);
        assert_eq!(
            diagnostics
                .observations()
                .iter()
                .filter(|observation| {
                    observation.operation == "Tool callback: Tool::handle_timer_event"
                })
                .count(),
            2,
            "transport did not deliver exactly two timer callbacks: {}",
            bounded_diagnostics(&diagnostics)
        );

        let tool_observations = global.0.lock().unwrap().clone();
        let tool_clocks = assert_timer_deopt_observations(&tool_observations, logical_syscall_rip);
        let (trajectory, diagnostic_clocks) =
            exact_timer_deopt_clock_trajectory(&diagnostics, logical_syscall_rip);
        assert_eq!(
            diagnostic_clocks, tool_clocks,
            "retained callback diagnostics and Tool clock observations diverged"
        );
        retained_trajectories.push(trajectory);
        tool_runs.push(tool_observations);
    }

    assert!(
        exact_clock_trajectories_match(&retained_trajectories[0], &retained_trajectories[1]),
        "repeated timer-deopt runs produced different retained absolute PMU-clock trajectories:\nfirst={:?}\nsecond={:?}",
        retained_trajectories[0],
        retained_trajectories[1]
    );
    assert_eq!(
        tool_runs[0], tool_runs[1],
        "repeated timer-deopt runs produced different Tool-visible clocks or program counters"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn non_output_stats_api_reports_four_call_dispatch_exactly() {
    let staged = staged_inputs();
    let caller = bind_reviewed_profile(&staged.four_canonical, &staged.marker);
    let diagnostics = caller.diagnostics();
    let mut command = Command::new(&staged.fixture);
    command
        .env_clear()
        .envs(four_canonical_environment())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        LiteinstBackend::run_host_after_loader_and_stats::<CountRawGetpid>(
            command,
            (),
            &staged.runtime,
            caller,
        ),
    )
    .await;
    let (status, global, stats) = match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!(
            "non-output stats after-loader API failed: {error}; {}",
            bounded_diagnostics(&diagnostics)
        ),
        Err(_) => panic!(
            "non-output stats after-loader API timed out; {}",
            bounded_diagnostics(&diagnostics)
        ),
    };

    assert_eq!(status, ExitStatus::Exited(0));
    assert_eq!(
        global.0.load(Ordering::SeqCst),
        4,
        "non-output stats API did not receive exactly four raw getpid calls"
    );
    assert_dispatch_stats(&stats, 1, 1, 3, 0);
    assert_one_installed_site(&stats);
    assert_restoration_diagnostics(&diagnostics);
}

#[tokio::test(flavor = "current_thread")]
async fn one_getpid_call_has_no_direct_hook_or_fallback_dispatch() {
    let staged = staged_inputs();
    let caller = bind_reviewed_profile(&staged.one_call, &staged.marker);
    let diagnostics = caller.diagnostics();
    let mut command = Command::new(&staged.fixture);
    command.env_clear().envs(one_call_environment());

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        LiteinstBackend::run_host_with_output_after_loader_and_stats::<CountRawGetpid>(
            command,
            (),
            &staged.runtime,
            caller,
        ),
    )
    .await;
    let (output, global, stats) = match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!(
            "one-call after-loader runner failed: {error}; {}",
            bounded_diagnostics(&diagnostics)
        ),
        Err(_) => panic!(
            "one-call after-loader runner timed out; {}",
            bounded_diagnostics(&diagnostics)
        ),
    };

    assert_eq!(output.status, ExitStatus::Exited(0), "{output:?}");
    assert!(output.stderr.is_empty(), "unexpected stderr: {output:?}");
    assert_fixture_stdout(&output.stdout, 1, false);
    assert_eq!(
        global.0.load(Ordering::SeqCst),
        1,
        "Tool did not receive exactly the fixture's one raw getpid call"
    );
    assert_dispatch_stats(&stats, 1, 1, 0, 0);
    assert_one_installed_site(&stats);
    assert_tool_callback_counts(&diagnostics, 1, 0);
    assert_ptmalloc_bootstrap_entropy_emulations(&diagnostics, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn unpatchable_getpid_refuses_installation_and_uses_one_retained_fallback() {
    let staged = staged_inputs();
    assert_unpatchable_getpid_prefix(&staged.fixture);
    let caller = bind_reviewed_profile(&staged.unpatchable, &staged.marker);
    let diagnostics = caller.diagnostics();
    let mut command = Command::new(&staged.fixture);
    command.env_clear().envs(unpatchable_environment());

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        LiteinstBackend::run_host_with_output_after_loader_and_stats::<CountRawGetpid>(
            command,
            (),
            &staged.runtime,
            caller,
        ),
    )
    .await;
    let (output, global, stats) = match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!(
            "fallback after-loader runner failed: {error}; {}",
            bounded_diagnostics(&diagnostics)
        ),
        Err(_) => panic!(
            "fallback after-loader runner timed out; {}",
            bounded_diagnostics(&diagnostics)
        ),
    };

    assert_eq!(output.status, ExitStatus::Exited(0), "{output:?}");
    assert!(output.stderr.is_empty(), "unexpected stderr: {output:?}");
    assert_fixture_stdout(&output.stdout, 1, true);
    assert_eq!(
        global.0.load(Ordering::SeqCst),
        1,
        "Tool did not service exactly one retained-fallback getpid call"
    );
    assert_dispatch_stats(&stats, 1, 0, 0, 1);
    assert_eq!(stats.decision_counts(), [0, 0, 0, 1]);
    assert_eq!(stats.patch_candidates(), 1);
    assert_eq!(stats.distinct_rips(), 0);
    assert_eq!(stats.classified_candidates(), 0);
    assert_tool_callback_counts(&diagnostics, 1, 0);
    assert_ptmalloc_bootstrap_entropy_emulations(&diagnostics, 0);
}
