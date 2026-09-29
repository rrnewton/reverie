//! Controller-driven activation of a constructor-disabled, real release runtime.
//!
//! Run through `tests/run_runtime_init.sh`. The guest has no loading or
//! initialization code; two calls at one site prove discovery followed by
//! exactly one installed-hook callback to the same host Tool as ptrace.

#![cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]

use std::collections::BTreeSet;
use std::ffi::CStr;
use std::ffi::CString;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::Mutex;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie::Backend;
use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::LiteinstDispatchPath;
use reverie::Subscription;
use reverie::Tid;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::process::Command;
use reverie::process::Output;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::LiteinstBackend;
use reverie_liteinst::LiteinstBackendStatsSource;
use reverie_ptrace::LiteinstRuntimeInit;
use reverie_ptrace::PtraceBackend;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

const PAIRS: usize = 10;
const CHILD_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RUNTIME_FILE: usize = 64 * 1024 * 1024;
const MEMFD_NAME: &str = "/memfd:reverie-liteinst-runtime (deleted)";
const R12_WITNESS: u64 = 0x0012_3456_789a_bcde;
const TIMER_RCBS: u64 = 1_000_000;
const PREINIT_R15_WITNESS: u64 = 0x0042_1357_9bdf_2468;
const MAX_ENTRY_TIMER_CALLBACKS: usize = 4096;
const CANCELLATION_MAGIC: u64 = 0x4c49_5445_4341_4e43;
const EXPECTED_STDOUT: &[u8] =
    b"calls=2 result=305419896 entry=restored env=unchanged preinit=clean simd=preserved errno=preserved\n";

struct Artifact {
    path: PathBuf,
    bytes: Vec<u8>,
    digest: String,
}

impl Artifact {
    fn bound() -> Self {
        let supplied = PathBuf::from(
            std::env::var_os("REVERIE_LITEINST_RUNTIME_INIT_DSO")
                .expect("run tests/run_runtime_init.sh: missing DSO path"),
        );
        assert!(supplied.is_absolute(), "bound DSO path must be absolute");
        let path = supplied.canonicalize().expect("canonicalize bound DSO");
        assert_eq!(supplied, path, "bound DSO path must already be canonical");
        let digest = std::env::var("REVERIE_LITEINST_RUNTIME_INIT_SHA256")
            .expect("run tests/run_runtime_init.sh: missing DSO SHA-256");
        assert_eq!(digest.len(), 64, "SHA-256 must contain 64 hex digits");
        assert!(
            digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "SHA-256 must be lowercase hexadecimal"
        );
        let bytes = fs::read(&path).expect("read bound DSO");
        assert!(!bytes.is_empty() && bytes.len() <= MAX_RUNTIME_FILE);
        assert_eq!(sha256(&bytes), digest, "bound DSO digest mismatch");
        Self {
            path,
            bytes,
            digest,
        }
    }

    fn assert_unchanged(&self) {
        let bytes = fs::read(&self.path).expect("re-read bound DSO");
        assert_eq!(sha256(&bytes), self.digest, "bound DSO changed");
        assert!(bytes == self.bytes, "bound DSO bytes changed");
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct Loader {
    bytes: Vec<u8>,
    version: String,
    interpreter: Vec<u8>,
    libgcc: Vec<u8>,
}

fn native_provider(address: *mut libc::c_void) -> PathBuf {
    assert!(!address.is_null(), "native provider address is absent");
    let mut info = MaybeUninit::<libc::Dl_info>::uninit();
    assert_ne!(unsafe { libc::dladdr(address, info.as_mut_ptr()) }, 0);
    let info = unsafe { info.assume_init() };
    assert!(!info.dli_fname.is_null());
    Path::new(OsStr::from_bytes(
        unsafe { CStr::from_ptr(info.dli_fname) }.to_bytes(),
    ))
    .canonicalize()
    .expect("canonicalize independently loaded native provider")
}

impl Loader {
    fn native() -> Self {
        // The native dynamic linker identifies the actual provider. readelf is
        // an independent oracle for its default symbol version, cross-checked
        // with dlvsym so an arbitrary similarly named version cannot qualify.
        let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"dlopen".as_ptr()) };
        assert!(!address.is_null(), "native dlopen is absent");
        let path = native_provider(address);
        let output = ProcessCommand::new("readelf")
            .args(["--dyn-syms", "--wide"])
            .arg(&path)
            .output()
            .expect("run readelf for native provider");
        assert!(output.status.success(), "readelf failed: {output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        let versions = text
            .lines()
            .filter_map(|line| {
                line.split_whitespace()
                    .find_map(|field| field.strip_prefix("dlopen@@"))
            })
            .collect::<Vec<_>>();
        assert_eq!(versions.len(), 1, "expected one default dlopen version");
        let version = versions[0].to_owned();
        let c_version = CString::new(version.as_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::dlvsym(libc::RTLD_DEFAULT, c"dlopen".as_ptr(), c_version.as_ptr()) },
            address,
            "default dlopen version does not match native symbol lookup"
        );
        Self {
            bytes: fs::read(path).expect("read native dlopen provider"),
            version,
            interpreter: fs::read(native_provider(
                unsafe { libc::getauxval(libc::AT_BASE) } as usize as *mut libc::c_void,
            ))
            .expect("read controller's native ELF interpreter"),
            libgcc: fs::read(native_provider(unsafe {
                libc::dlsym(libc::RTLD_DEFAULT, c"_Unwind_Resume".as_ptr())
            }))
            .expect("read controller's native libgcc provider"),
        }
    }

    fn descriptor(&self, runtime: &[u8]) -> LiteinstRuntimeInit {
        LiteinstRuntimeInit::new(
            self.bytes.clone(),
            self.version.clone(),
            self.interpreter.clone(),
            self.libgcc.clone(),
            runtime.to_vec(),
            0,
        )
        .expect("construct validated runtime-init descriptor")
    }
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
enum TimerCase {
    #[default]
    Disabled,
    Long,
    Entry {
        loop_start: u64,
        loop_end: u64,
    },
    Mapping {
        expected_rip: u64,
    },
}

impl TimerCase {
    fn has_preinit_marker(self) -> bool {
        matches!(self, Self::Entry { .. } | Self::Mapping { .. })
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct RunConfig {
    events: PathBuf,
    runtime: Option<PathBuf>,
    timer: TimerCase,
}

#[derive(Default)]
struct Observations {
    config: RunConfig,
    runtime: Option<Vec<u8>>,
    pid: AtomicU32,
    calls: Mutex<Vec<(u64, u64)>>,
    timers: Mutex<Vec<(u64, u64, u64)>>,
    markers: Mutex<Vec<(u64, u64)>>,
}

#[derive(Default)]
struct MatrixEvidence {
    pairs: usize,
    ptrace_runs: usize,
    initialized_runs: usize,
    refusals: usize,
    cancellations: usize,
    native_controls: usize,
    getpid_callbacks: usize,
    preinit_markers: usize,
    direct_hooks: u64,
    timer_pairs: usize,
    timer_callbacks: usize,
}

struct PairEvidence {
    reference_timer_callbacks: usize,
    initialized_timer_callbacks: usize,
    preinit_markers: usize,
}

impl PairEvidence {
    fn timer_callbacks(&self) -> usize {
        self.reference_timer_callbacks + self.initialized_timer_callbacks
    }
}

impl MatrixEvidence {
    // Call only after both runs and their exact parity assertions complete.
    // The runner checks the resulting totals against the required matrix, so
    // removing an iteration or case cannot keep advertising its old coverage.
    fn record_pair(
        &mut self,
        reference: &Observations,
        initialized: &Observations,
        stats: &LiteinstBackendStatsSource,
    ) -> PairEvidence {
        let pair = PairEvidence {
            reference_timer_callbacks: reference.timers.lock().unwrap().len(),
            initialized_timer_callbacks: initialized.timers.lock().unwrap().len(),
            preinit_markers: reference.markers.lock().unwrap().len()
                + initialized.markers.lock().unwrap().len(),
        };
        self.pairs += 1;
        self.ptrace_runs += usize::from(reference.pid.load(Ordering::SeqCst) != 0);
        self.initialized_runs += usize::from(initialized.pid.load(Ordering::SeqCst) != 0);
        self.getpid_callbacks +=
            reference.calls.lock().unwrap().len() + initialized.calls.lock().unwrap().len();
        self.preinit_markers += pair.preinit_markers;
        self.direct_hooks += stats
            .dispatch_path_counts()
            .count(&LiteinstDispatchPath::DirectHook);
        self.timer_pairs += usize::from(pair.timer_callbacks() != 0);
        self.timer_callbacks += pair.timer_callbacks();
        pair
    }
}

#[reverie::global_tool]
impl GlobalTool for Observations {
    type Request = (u8, u32, u64, u64, u64);
    type Response = ();
    type Config = RunConfig;

    async fn init_global_state(config: &RunConfig) -> Self {
        Self {
            config: config.clone(),
            runtime: config.runtime.as_ref().map(|path| fs::read(path).unwrap()),
            ..Self::default()
        }
    }

    async fn receive_rpc(&self, _from: Tid, (kind, pid, first, second, third): Self::Request) {
        match kind {
            0 => {
                assert_eq!(self.pid.swap(pid, Ordering::SeqCst), 0, "duplicate exec");
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&self.config.events)
                    .unwrap();
                writeln!(file, "exec {pid}").unwrap();
            }
            1 => {
                assert_eq!(self.pid.load(Ordering::SeqCst), pid);
                let mut calls = self.calls.lock().unwrap();
                if calls.is_empty() {
                    assert_runtime_mapping(pid, self.runtime.as_deref());
                }
                assert_eq!(third, 0);
                calls.push((first, second));
                let mut file = OpenOptions::new()
                    .append(true)
                    .open(&self.config.events)
                    .unwrap();
                writeln!(file, "getpid {pid} {first:x} {second:x}").unwrap();
            }
            2 => {
                assert!(
                    self.config.timer != TimerCase::Disabled,
                    "unrequested timer event"
                );
                assert_eq!(self.pid.load(Ordering::SeqCst), pid);
                self.timers.lock().unwrap().push((first, second, third));
                let mut file = OpenOptions::new()
                    .append(true)
                    .open(&self.config.events)
                    .unwrap();
                writeln!(file, "timer {pid} {first} {second:x} {third}").unwrap();
            }
            3 => {
                assert!(self.config.timer.has_preinit_marker());
                assert_eq!(self.pid.load(Ordering::SeqCst), pid);
                assert_eq!(third, 0);
                // This marker must arm its timer before controller loading.
                assert_runtime_mapping(pid, None);
                self.markers.lock().unwrap().push((first, second));
                let mut file = OpenOptions::new()
                    .append(true)
                    .open(&self.config.events)
                    .unwrap();
                writeln!(file, "preinit-gettid {pid} {first:x} {second:x}").unwrap();
            }
            _ => panic!("unexpected fixture RPC kind {kind}"),
        }
    }
}

#[derive(Default)]
struct FixedGetpid;

#[derive(Default, Deserialize, Serialize)]
struct ClockState {
    origin: Option<u64>,
    callbacks: usize,
    marker_seen: bool,
}

#[reverie::tool]
impl Tool for FixedGetpid {
    type GlobalState = Observations;
    type ThreadState = ClockState;

    fn subscriptions(config: &RunConfig) -> Subscription {
        let mut subscription: Subscription = [Sysno::getpid].into_iter().collect();
        if config.timer.has_preinit_marker() {
            subscription.syscall(Sysno::gettid);
        }
        subscription
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), reverie::Errno> {
        guest
            .send_rpc((0, guest.pid().as_raw() as u32, 0, 0, 0))
            .await;
        if guest.config().timer == TimerCase::Long {
            let clock = guest.read_clock().expect("read post-exec PMU clock");
            guest.thread_state_mut().origin = Some(clock);
            guest
                .set_timer_precise(TimerSchedule::Rcbs(TIMER_RCBS))
                .expect("arm precise timer across controller initialization");
        }
        Ok(())
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        let timer = guest.config().timer;
        assert!(timer != TimerCase::Disabled, "unrequested timer callback");
        let elapsed = guest
            .read_clock()
            .expect("read timer PMU clock")
            .checked_sub(
                guest
                    .thread_state()
                    .origin
                    .expect("timer lacks its clock origin"),
            )
            .expect("controller clock moved backwards");
        let regs = guest.regs().await;
        guest
            .send_rpc((2, guest.pid().as_raw() as u32, elapsed, regs.rip, regs.r15))
            .await;
        let state = guest.thread_state_mut();
        state.callbacks += 1;
        match timer {
            TimerCase::Disabled => unreachable!(),
            TimerCase::Long => {
                assert_eq!(state.callbacks, 1, "duplicate long timer");
                assert_eq!(elapsed, TIMER_RCBS);
            }
            TimerCase::Entry {
                loop_start,
                loop_end,
            } => {
                assert!(
                    state.callbacks <= MAX_ENTRY_TIMER_CALLBACKS,
                    "entry timer never reached entry"
                );
                assert_eq!(
                    elapsed, state.callbacks as u64,
                    "entry timer skipped a branch"
                );
                if !(loop_start..loop_end).contains(&regs.rip) {
                    guest
                        .set_timer_precise(TimerSchedule::Rcbs(1))
                        .expect("rearm precise timer through loader and executable entry");
                }
            }
            TimerCase::Mapping { expected_rip } => {
                assert_eq!(state.callbacks, 1, "duplicate mapping timer");
                assert_eq!(
                    (elapsed, regs.rip, regs.r15),
                    (1, expected_rip, PREINIT_R15_WITNESS)
                );
            }
        }
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let regs = guest.regs().await;
        if syscall.number() == Sysno::gettid {
            let timer = guest.config().timer;
            assert!(timer.has_preinit_marker(), "unexpected gettid subscription");
            assert!(
                !guest.thread_state().marker_seen,
                "duplicate preinit marker"
            );
            let clock = guest.read_clock().expect("read preinit marker clock");
            let state = guest.thread_state_mut();
            state.marker_seen = true;
            state.origin = Some(clock);
            guest
                .send_rpc((3, guest.pid().as_raw() as u32, regs.rip, regs.r15, 0))
                .await;
            let schedule = match timer {
                TimerCase::Entry { .. } => TimerSchedule::Rcbs(1),
                TimerCase::Mapping { .. } => TimerSchedule::RcbsAndInstructions(1, 12),
                _ => unreachable!(),
            };
            guest
                .set_timer_precise(schedule)
                .expect("arm precise preinit timer");
            return Ok(i64::from(guest.tid().as_raw()));
        }
        assert_eq!(syscall.number(), Sysno::getpid);
        guest
            .send_rpc((1, guest.pid().as_raw() as u32, regs.rip, regs.r12, 0))
            .await;
        Ok(0x1234_5678)
    }
}

#[derive(Clone, Copy)]
struct Load {
    flags: u32,
    offset: u64,
    address: u64,
    file_size: u64,
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn loads(bytes: &[u8]) -> Vec<Load> {
    assert_eq!(&bytes[..6], b"\x7fELF\x02\x01");
    assert_eq!(u16_at(bytes, 54), 56, "ELF64 program-header size");
    let start = usize::try_from(u64_at(bytes, 32)).unwrap();
    (0..usize::from(u16_at(bytes, 56)))
        .filter_map(|index| {
            let ph = start.checked_add(index.checked_mul(56).unwrap()).unwrap();
            (u32_at(bytes, ph) == 1).then(|| Load {
                flags: u32_at(bytes, ph + 4),
                offset: u64_at(bytes, ph + 8),
                address: u64_at(bytes, ph + 16),
                file_size: u64_at(bytes, ph + 32),
            })
        })
        .collect()
}

fn assert_runtime_mapping(pid: u32, expected: Option<&[u8]>) {
    let maps = fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
    let mapped = maps
        .lines()
        .filter(|line| line.ends_with(MEMFD_NAME))
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let Some(expected) = expected else {
        assert!(
            mapped.is_empty(),
            "ptrace baseline loaded a LiteInst runtime"
        );
        return;
    };
    assert!(
        !mapped.is_empty(),
        "controller did not load the sealed runtime"
    );
    let identities = mapped
        .iter()
        .map(|fields| (fields[3], fields[4].parse::<u64>().unwrap()))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        identities.len(),
        1,
        "runtime spans multiple file identities"
    );
    assert_ne!(identities.first().unwrap().1, 0, "runtime inode is absent");
    let zero_offset = mapped
        .iter()
        .filter(|fields| u64::from_str_radix(fields[2], 16).unwrap() == 0)
        .collect::<Vec<_>>();
    assert_eq!(
        zero_offset.len(),
        1,
        "runtime offset-zero mapping ambiguous"
    );
    let base = u64::from_str_radix(zero_offset[0][0].split('-').next().unwrap(), 16).unwrap();
    let memory = fs::File::open(format!("/proc/{pid}/mem")).unwrap();
    let mut compared = 0;
    for load in loads(expected)
        .into_iter()
        .filter(|load| load.flags & 2 == 0 && load.file_size != 0)
    {
        let offset = usize::try_from(load.offset).unwrap();
        let size = usize::try_from(load.file_size).unwrap();
        let end = offset.checked_add(size).unwrap();
        assert!(end <= expected.len());
        let mut actual = vec![0; size];
        memory
            .read_exact_at(&mut actual, base.checked_add(load.address).unwrap())
            .expect("read stopped runtime's non-writable PT_LOAD bytes");
        assert!(
            actual == expected[offset..end],
            "loaded runtime bytes changed at PT_LOAD offset {offset:#x}"
        );
        compared += 1;
    }
    assert!(
        compared >= 2,
        "real runtime must supply both RO and RX load bytes"
    );
    for entry in fs::read_dir(format!("/proc/{pid}/fd")).unwrap() {
        let target = fs::read_link(entry.unwrap().path()).unwrap();
        assert_ne!(
            target,
            Path::new(MEMFD_NAME),
            "private runtime fd leaked into guest"
        );
    }
}

fn compile(directory: &Path, source: &str, output: &str, flags: &[&str]) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(source);
    let output_path = directory.join(output);
    let compiler = std::env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
    let result = ProcessCommand::new(compiler)
        .args(["-std=gnu11", "-O0", "-Wall", "-Wextra", "-Werror"])
        .args(flags)
        .arg(&source)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("compile runtime-init fixture");
    assert!(
        result.status.success(),
        "compile {}: {result:?}",
        source.display()
    );
    output_path
}

fn entry_hex(binary: &Path) -> String {
    let bytes = fs::read(binary).unwrap();
    let entry = u64_at(&bytes, 24);
    let offsets = loads(&bytes)
        .into_iter()
        .filter_map(|load| {
            let within = entry.checked_sub(load.address)?;
            (within.checked_add(8)? <= load.file_size)
                .then(|| usize::try_from(load.offset.checked_add(within).unwrap()).unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(offsets.len(), 1, "ambiguous executable entry bytes");
    bytes[offsets[0]..offsets[0] + 8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn symbol_address(binary: &Path, symbol: &str) -> u64 {
    let output = ProcessCommand::new("nm")
        .arg("--defined-only")
        .arg(binary)
        .output()
        .unwrap();
    assert!(output.status.success(), "nm failed: {output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    let addresses = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let address = fields.next()?;
            let _kind = fields.next()?;
            (fields.next()? == symbol).then(|| u64::from_str_radix(address, 16).unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(addresses.len(), 1);
    addresses[0]
}

fn native_command(binary: &Path, entry: &str, marker: &Path) -> ProcessCommand {
    // Keep this limited to path, args, and per-variable environment changes:
    // these are preserved by Command::from_std_lossy for traced executions.
    let mut command = ProcessCommand::new(binary);
    command.arg(entry).arg(marker);
    for name in [
        "LD_PRELOAD",
        "LD_AUDIT",
        "LD_LIBRARY_PATH",
        "LD_PROFILE",
        "LD_DEBUG",
        "LD_DEBUG_OUTPUT",
        "LD_BIND_NOT",
        "LD_DYNAMIC_WEAK",
        "LD_ORIGIN_PATH",
        "GLIBC_TUNABLES",
    ] {
        command.env_remove(name);
    }
    for name in [
        "REVERIE_LITEINST_HOST_RUNTIME",
        "REVERIE_LITEINST_TOOL",
        "REVERIE_PRELOAD_TOOL",
        "REVERIE_LITEINST_STRADDLER_STALENESS_TICKS",
    ] {
        command.env(name, "runtime-init-poison");
    }
    command
}

fn read_pid(events: &Path) -> u32 {
    let text = fs::read_to_string(events).expect("controller did not record post-exec identity");
    let mut first = text.lines().next().unwrap().split_whitespace();
    assert_eq!(first.next(), Some("exec"));
    let pid = first.next().unwrap().parse().unwrap();
    assert!(first.next().is_none());
    pid
}

fn assert_reaped(events: &Path) {
    let pid = read_pid(events);
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "tracee {pid} remains alive"
    );
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

fn assert_positive(output: &Output, global: &Observations, marker: &Path, site: u64) {
    assert!(output.status.success(), "guest failed: {output:?}");
    assert_eq!(output.stdout, EXPECTED_STDOUT, "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(fs::read(marker).unwrap(), b"entered\n");
    assert_eq!(
        *global.calls.lock().unwrap(),
        [(site + 2, R12_WITNESS); 2],
        "must observe discovery and exactly one hot callback at the logical syscall RIP"
    );
}

fn assert_stats(stats: &LiteinstBackendStatsSource) {
    assert_eq!(stats.patch_candidates(), 1, "{stats}");
    assert_eq!(stats.distinct_rips(), 1, "{stats}");
    assert_eq!(stats.decision_counts(), [0, 1, 0, 0], "{stats}");
    assert_eq!(stats.classified_candidates(), 1, "{stats}");
    assert_eq!(stats.cacheline_straddlers(), 0, "{stats}");
    assert_eq!(stats.non_straddling(), 1, "{stats}");
    assert_eq!(
        stats.instruction_length_counts(),
        [0, 0, 0, 1, 0],
        "{stats}"
    );
    for path in LiteinstDispatchPath::ALL {
        let expected = u64::from(matches!(
            path,
            LiteinstDispatchPath::FirstSiteSeccomp
                | LiteinstDispatchPath::PtraceInstallation
                | LiteinstDispatchPath::DirectHook
        ));
        assert_eq!(
            stats.dispatch_path_counts().count(path),
            expected,
            "{path}: {stats}"
        );
    }
}

async fn refuse_before_entry(
    loader: &Loader,
    runtime: &Path,
    binary: &Path,
    entry: &str,
    directory: &Path,
    label: &str,
    evidence: &mut MatrixEvidence,
) {
    let marker = directory.join(format!("{label}.entered"));
    let events = directory.join(format!("{label}.events"));
    let callback = directory.join(format!("{label}.callback"));
    let mut command = native_command(binary, entry, &marker);
    if label == "executable-sysconf" {
        command.arg(&callback);
    }
    let runtime_bytes = fs::read(runtime).unwrap();
    let result = tokio::time::timeout(
        CHILD_TIMEOUT,
        LiteinstBackend::run_host_with_output_and_runtime_init_and_stats::<FixedGetpid>(
            Command::from_std_lossy(&command),
            RunConfig {
                events: events.clone(),
                runtime: Some(runtime.to_path_buf()),
                timer: TimerCase::Disabled,
            },
            loader.descriptor(&runtime_bytes),
        ),
    )
    .await;
    assert_reaped(&events);
    let error = match result.expect("activation refusal timed out") {
        Ok(_) => panic!("invalid activation succeeded"),
        Err(error) => error,
    };
    let description = error.to_string();
    let (_, detail) = description
        .split_once("LiteInst controller initialization failed for tracee ")
        .unwrap_or_else(|| panic!("{label}: refusal came from the wrong boundary: {error}"));
    let (reported_pid, reason) = detail
        .split_once(": ")
        .expect("controller refusal omitted tracee identity or reason");
    assert_eq!(
        reported_pid.parse::<u32>().unwrap(),
        read_pid(&events),
        "controller refusal names another tracee"
    );
    assert!(!reason.is_empty(), "controller refusal omitted its reason");
    if label == "initializer-signal" {
        assert!(
            reason.starts_with("unexpected controller event: Signal(SIGUSR1)"),
            "controller did not observe the initializer's exact signal: {error}"
        );
    }
    assert!(
        !marker.exists(),
        "{label}: failed activation reached application entry"
    );
    assert_eq!(
        fs::read_to_string(events).unwrap().lines().count(),
        1,
        "{label}: refused activation delivered a Tool callback"
    );
    assert!(
        !callback.exists(),
        "{label}: private initialization invoked executable code"
    );
    evidence.refusals += 1;
}

fn run_native_bounded(mut command: ProcessCommand) -> std::process::ExitStatus {
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("native control timed out");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

async fn refuse_loader_environment(
    loader: &Loader,
    artifact: &Artifact,
    fixture: &Path,
    entry: &str,
    directory: &Path,
    variable: &str,
    evidence: &mut MatrixEvidence,
) {
    let callback = directory.join(format!("{variable}.callback"));
    let marker_define = format!("-DCALLBACK_MARKER=\"{}\"", callback.display());
    let mut flags = vec![
        "-shared",
        "-fPIC",
        "-nostdlib",
        "-Wl,--hash-style=both",
        marker_define.as_str(),
    ];
    if variable == "LD_AUDIT" {
        flags.push("-DHOSTILE_AUDIT=1");
    }
    let hostile = compile(
        directory,
        "runtime_init_hostile.c",
        &format!("{variable}.so"),
        &flags,
    );

    // These callbacks execute in the normal loader before application entry.
    // Prove the hostile DSO really executes, then require the controller API to
    // reject its environment before spawning rather than hiding its effects.
    let mut native = ProcessCommand::new("/bin/true");
    native
        .env_remove("LD_PRELOAD")
        .env_remove("LD_AUDIT")
        .env(variable, &hostile);
    assert!(
        run_native_bounded(native).success(),
        "{variable}: native loader control failed"
    );
    assert_eq!(fs::read(&callback).unwrap(), b"callback\n");
    evidence.native_controls += 1;
    fs::remove_file(&callback).unwrap();

    let marker = directory.join(format!("{variable}.entered"));
    let events = directory.join(format!("{variable}.events"));
    let mut command = native_command(fixture, entry, &marker);
    command.env(variable, &hostile);
    let result = tokio::time::timeout(
        CHILD_TIMEOUT,
        LiteinstBackend::run_host_with_output_and_runtime_init_and_stats::<FixedGetpid>(
            Command::from_std_lossy(&command),
            RunConfig {
                events: events.clone(),
                runtime: Some(artifact.path.clone()),
                timer: TimerCase::Disabled,
            },
            loader.descriptor(&artifact.bytes),
        ),
    )
    .await
    .expect("pre-spawn environment refusal timed out");
    let error = match result {
        Ok(_) => panic!("hostile {variable} environment was accepted"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains(variable),
        "{variable}: unrelated refusal: {error}"
    );
    assert!(
        !events.exists(),
        "{variable}: refusal occurred after post-exec callback"
    );
    assert!(
        !callback.exists(),
        "{variable}: hostile loader callback executed before refusal"
    );
    assert!(
        !marker.exists(),
        "{variable}: application main ran before refusal"
    );
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG | libc::__WALL) },
        -1,
        "{variable}: refusal left a child"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    evidence.refusals += 1;
}

async fn cancel_initializer(
    loader: &Loader,
    fixture: &Path,
    entry: &str,
    directory: &Path,
    evidence: &mut MatrixEvidence,
) {
    let receipt = directory.join("initializer-cancel.receipt");
    let receipt_define = format!("-DCANCELLATION_MARKER=\"{}\"", receipt.display());
    let runtime = compile(
        directory,
        "runtime_init_negative.c",
        "initializer-cancel.so",
        &[
            "-shared",
            "-fPIC",
            "-nostdlib",
            "-Wl,--hash-style=both",
            "-Wl,-z,now",
            "-DINITIALIZER_CANCEL=1",
            &receipt_define,
        ],
    );
    let marker = directory.join("initializer-cancel.entered");
    let events = directory.join("initializer-cancel.events");
    let mut run = Box::pin(
        LiteinstBackend::run_host_with_output_and_runtime_init_and_stats::<FixedGetpid>(
            Command::from_std_lossy(&native_command(fixture, entry, &marker)),
            RunConfig {
                events: events.clone(),
                runtime: Some(runtime.clone()),
                timer: TimerCase::Disabled,
            },
            loader.descriptor(&fs::read(runtime).unwrap()),
        ),
    );
    let entered = async {
        loop {
            if let Ok(bytes) = fs::read(&receipt) {
                assert!(bytes.len() <= 16, "initializer receipt exceeds its ABI");
                if bytes.len() == 16 {
                    assert_eq!(u64_at(&bytes, 0), CANCELLATION_MAGIC);
                    let pid = u32::try_from(u64_at(&bytes, 8)).unwrap();
                    assert_eq!(pid, read_pid(&events), "initializer PID was virtualized");
                    assert!(Path::new(&format!("/proc/{pid}")).exists());
                    break pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    // The controller's deadline is ten seconds. Cancel only after the helper
    // proves it ran, and within two seconds so a deadline failure cannot pass
    // for cancellation cleanup. Polling the run also rejects early refusal.
    let pid = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            biased;
            result = &mut run => match result {
                Ok(_) => panic!("waiting initializer unexpectedly succeeded"),
                Err(error) => panic!("initializer failed before cancellation: {error}"),
            },
            pid = entered => pid,
        }
    })
    .await;
    drop(run);
    let pid = pid.expect("initializer did not publish its cancellation receipt");
    assert_eq!(pid, read_pid(&events));
    assert_reaped(&events);
    assert!(
        !marker.exists(),
        "cancelled initializer reached application entry"
    );
    let event_count = fs::read_to_string(events).unwrap().lines().count();
    assert_eq!(
        event_count, 1,
        "private initializer delivered a Tool callback before cancellation"
    );
    let cancellations_before = evidence.cancellations;
    evidence.cancellations += 1;
    println!(
        "runtime-init cancellation: initializer-receipts={} fully-reaped={} tool-callbacks={}",
        fs::read(receipt).unwrap().as_chunks::<16>().0.len(),
        evidence.cancellations - cancellations_before,
        event_count - 1,
    );
}

async fn timer_boundary_pair(
    loader: &Loader,
    artifact: &Artifact,
    directory: &Path,
    evidence: &mut MatrixEvidence,
) {
    let fixture = compile(
        directory,
        "runtime_init.c",
        "runtime-init-timer",
        &[
            "-fno-pie",
            "-no-pie",
            "-ldl",
            "-DTIMER_BOUNDARY=1",
            "-Wl,-e,runtime_init_entry",
        ],
    );
    let entry = entry_hex(&fixture);
    let site = symbol_address(&fixture, "runtime_init_getpid_site");
    assert_eq!(site % 64, 0);
    let loop_start = symbol_address(&fixture, "runtime_init_timer_loop");
    let loop_end = symbol_address(&fixture, "runtime_init_timer_end");
    let marker = directory.join("timer.entered");
    let events = directory.join("timer.events");

    // Reuse identical command bytes for both runs. The active timer begins in
    // the original loader, so even changing argv lengths could change its RCB
    // count before entry. The dedicated entry loop runs before libc or main.
    let reference = tokio::time::timeout(
        CHILD_TIMEOUT,
        PtraceBackend::run_with_output::<FixedGetpid>(
            Command::from_std_lossy(&native_command(&fixture, &entry, &marker)),
            RunConfig {
                events: events.clone(),
                runtime: None,
                timer: TimerCase::Long,
            },
        ),
    )
    .await;
    assert_reaped(&events);
    let (reference_output, reference_global, _) = reference
        .expect("ptrace timer reference timed out")
        .unwrap();
    assert_positive(&reference_output, &reference_global, &marker, site);
    let reference_timers = reference_global.timers.lock().unwrap().clone();
    assert_eq!(reference_timers.len(), 1, "ptrace timer callback count");
    let (clock, rip, remaining) = reference_timers[0];
    assert_eq!(clock, TIMER_RCBS, "ptrace precise timer clock");
    assert!(
        (loop_start..loop_end).contains(&rip),
        "timer outside entry loop"
    );
    assert!(
        (1..2_000_000).contains(&remaining),
        "entry loop witness absent"
    );
    fs::remove_file(&marker).unwrap();
    fs::remove_file(&events).unwrap();

    let initialized = tokio::time::timeout(
        CHILD_TIMEOUT,
        LiteinstBackend::run_host_with_output_and_runtime_init_and_stats::<FixedGetpid>(
            Command::from_std_lossy(&native_command(&fixture, &entry, &marker)),
            RunConfig {
                events: events.clone(),
                runtime: Some(artifact.path.clone()),
                timer: TimerCase::Long,
            },
            loader.descriptor(&artifact.bytes),
        ),
    )
    .await;
    assert_reaped(&events);
    let (output, global, stats) = initialized
        .expect("controller timer activation timed out")
        .unwrap();
    assert_positive(&output, &global, &marker, site);
    assert_stats(&stats);
    assert_eq!(output.status, reference_output.status);
    assert_eq!(output.stdout, reference_output.stdout);
    assert_eq!(output.stderr, reference_output.stderr);
    assert_eq!(
        *global.timers.lock().unwrap(),
        reference_timers,
        "initialization changed timer delivery, clock, RIP, or entry-loop iteration"
    );
    artifact.assert_unchanged();
    let pairs_before = evidence.pairs;
    let pair = evidence.record_pair(&reference_global, &global, &stats);
    println!(
        "runtime-init timer: pairs={} callbacks={} exact-rcbs={TIMER_RCBS} rip={rip:#x} remaining={remaining}",
        evidence.pairs - pairs_before,
        pair.timer_callbacks(),
    );
}

async fn precision_boundary_pair(
    loader: &Loader,
    artifact: &Artifact,
    directory: &Path,
    mapping: bool,
    evidence: &mut MatrixEvidence,
) -> PairEvidence {
    let label = if mapping {
        "mapping-precision"
    } else {
        "entry-precision"
    };
    let mut flags = vec!["-fno-pie", "-no-pie", "-ldl"];
    if mapping {
        flags.push("-DPREINIT_MAPPING_TIMER=1");
    } else {
        flags.extend([
            "-DPREINIT_ENTRY_TIMER=1",
            "-DTIMER_BOUNDARY=1",
            "-Wl,-e,runtime_init_entry",
        ]);
    }
    let fixture = compile(directory, "runtime_init.c", label, &flags);
    let entry = entry_hex(&fixture);
    let site = symbol_address(&fixture, "runtime_init_getpid_site");
    assert_eq!(site % 64, 0);
    let marker_rip = symbol_address(&fixture, "runtime_init_preinit_gettid_site") + 2;
    let timer = if mapping {
        TimerCase::Mapping {
            expected_rip: symbol_address(&fixture, "runtime_init_mapping_timer_expected"),
        }
    } else {
        TimerCase::Entry {
            loop_start: symbol_address(&fixture, "runtime_init_timer_loop"),
            loop_end: symbol_address(&fixture, "runtime_init_timer_end"),
        }
    };
    let marker = directory.join(format!("{label}.entered"));
    let events = directory.join(format!("{label}.events"));

    // Both backends receive byte-identical argv and environment. Marker RPCs
    // are counted separately from getpid and prove the runtime was absent at
    // the moment the precision request was armed.
    let reference = tokio::time::timeout(
        CHILD_TIMEOUT,
        PtraceBackend::run_with_output::<FixedGetpid>(
            Command::from_std_lossy(&native_command(&fixture, &entry, &marker)),
            RunConfig {
                events: events.clone(),
                runtime: None,
                timer,
            },
        ),
    )
    .await;
    assert_reaped(&events);
    let (reference_output, reference_global, _) = reference
        .expect("ptrace precision reference timed out")
        .unwrap();
    assert_positive(&reference_output, &reference_global, &marker, site);
    let reference_markers = reference_global.markers.lock().unwrap().clone();
    assert_eq!(reference_markers, [(marker_rip, PREINIT_R15_WITNESS)]);
    let reference_timers = reference_global.timers.lock().unwrap().clone();
    match timer {
        TimerCase::Entry {
            loop_start,
            loop_end,
        } => {
            assert!(!reference_timers.is_empty(), "entry timer never fired");
            assert!(reference_timers.len() <= MAX_ENTRY_TIMER_CALLBACKS);
            for (index, &(clock, rip, _)) in reference_timers.iter().enumerate() {
                assert_eq!(
                    clock,
                    index as u64 + 1,
                    "entry timer skipped or duplicated a branch"
                );
                assert_eq!(
                    (loop_start..loop_end).contains(&rip),
                    index + 1 == reference_timers.len(),
                    "entry precision trace stopped before entry or rearmed inside its loop"
                );
            }
            let (_, _, remaining) = *reference_timers.last().unwrap();
            assert!(
                (1..2_000_000).contains(&remaining),
                "entry-loop register witness absent"
            );
        }
        TimerCase::Mapping { expected_rip } => {
            assert_eq!(reference_timers, [(1, expected_rip, PREINIT_R15_WITNESS)]);
        }
        _ => unreachable!(),
    }
    fs::remove_file(&marker).unwrap();
    fs::remove_file(&events).unwrap();

    let initialized = tokio::time::timeout(
        CHILD_TIMEOUT,
        LiteinstBackend::run_host_with_output_and_runtime_init_and_stats::<FixedGetpid>(
            Command::from_std_lossy(&native_command(&fixture, &entry, &marker)),
            RunConfig {
                events: events.clone(),
                runtime: Some(artifact.path.clone()),
                timer,
            },
            loader.descriptor(&artifact.bytes),
        ),
    )
    .await;
    assert_reaped(&events);
    let (output, global, stats) = initialized
        .expect("controller precision activation timed out")
        .unwrap();
    assert_positive(&output, &global, &marker, site);
    assert_stats(&stats);
    assert_eq!(output.status, reference_output.status);
    assert_eq!(output.stdout, reference_output.stdout);
    assert_eq!(output.stderr, reference_output.stderr);
    assert_eq!(*global.markers.lock().unwrap(), reference_markers);
    assert_eq!(
        *global.timers.lock().unwrap(),
        reference_timers,
        "{label}: controller maintenance changed the complete precision trace"
    );
    artifact.assert_unchanged();
    let pair = evidence.record_pair(&reference_global, &global, &stats);
    println!(
        "runtime-init boundary: case={label} markers={} callbacks-per-run={} trace=exact",
        pair.preinit_markers, pair.reference_timer_callbacks,
    );
    pair
}

#[cfg(debug_assertions)]
fn require_release() {
    panic!("runtime-init conformance requires Cargo's release profile");
}

#[cfg(not(debug_assertions))]
fn require_release() {}

#[cfg(feature = "preload-constructor")]
fn require_constructor_disabled() {
    panic!("runtime-init conformance requires --no-default-features");
}

#[cfg(not(feature = "preload-constructor"))]
fn require_constructor_disabled() {}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an explicitly bound release/no-default-feature cdylib"]
async fn controller_loads_runtime_before_entry_and_dispatches_one_real_hook() {
    require_release();
    require_constructor_disabled();
    let artifact = Artifact::bound();
    let loader = Loader::native();
    let mut evidence = MatrixEvidence::default();
    let directory = tempfile::tempdir().unwrap();
    let fixture = compile(
        directory.path(),
        "runtime_init.c",
        "runtime-init",
        &["-fno-pie", "-no-pie", "-ldl"],
    );
    let entry = entry_hex(&fixture);
    let site = symbol_address(&fixture, "runtime_init_getpid_site");
    assert_eq!(site % 64, 0, "fixture site must not straddle a cache line");

    // A native run has the same environment and executable but lacks the Tool
    // that supplies a value above Linux's PID_MAX_LIMIT. It must fail at that
    // exact result assertion, independently of the PID allocated for this run.
    let native_marker = directory.path().join("native.entered");
    let native_status = run_native_bounded(native_command(&fixture, &entry, &native_marker));
    assert_eq!(
        native_status.code(),
        Some(77),
        "native control failed before getpid witness"
    );
    assert_eq!(fs::read(native_marker).unwrap(), b"entered\n");
    evidence.native_controls += 1;

    for pair in 0..PAIRS {
        let reference_marker = directory.path().join(format!("ptrace-{pair}.entered"));
        let reference_events = directory.path().join(format!("ptrace-{pair}.events"));
        let reference = tokio::time::timeout(
            CHILD_TIMEOUT,
            PtraceBackend::run_with_output::<FixedGetpid>(
                Command::from_std_lossy(&native_command(&fixture, &entry, &reference_marker)),
                RunConfig {
                    events: reference_events.clone(),
                    runtime: None,
                    timer: TimerCase::Disabled,
                },
            ),
        )
        .await;
        assert_reaped(&reference_events);
        let (reference_output, reference_global, _) =
            reference.expect("ptrace reference timed out").unwrap();
        assert_positive(
            &reference_output,
            &reference_global,
            &reference_marker,
            site,
        );

        let marker = directory.path().join(format!("runtime-{pair}.entered"));
        let events = directory.path().join(format!("runtime-{pair}.events"));
        let initialized = tokio::time::timeout(
            CHILD_TIMEOUT,
            LiteinstBackend::run_host_with_output_and_runtime_init_and_stats::<FixedGetpid>(
                Command::from_std_lossy(&native_command(&fixture, &entry, &marker)),
                RunConfig {
                    events: events.clone(),
                    runtime: Some(artifact.path.clone()),
                    timer: TimerCase::Disabled,
                },
                loader.descriptor(&artifact.bytes),
            ),
        )
        .await;
        assert_reaped(&events);
        let (output, global, stats) = initialized
            .expect("controller activation timed out")
            .unwrap();
        assert_positive(&output, &global, &marker, site);
        assert_stats(&stats);
        assert_eq!(output.status, reference_output.status, "pair {pair}");
        assert_eq!(output.stdout, reference_output.stdout, "pair {pair}");
        assert_eq!(output.stderr, reference_output.stderr, "pair {pair}");
        assert_eq!(
            *global.calls.lock().unwrap(),
            *reference_global.calls.lock().unwrap(),
            "pair {pair}"
        );
        artifact.assert_unchanged();
        evidence.record_pair(&reference_global, &global, &stats);
    }

    timer_boundary_pair(&loader, &artifact, directory.path(), &mut evidence).await;
    let entry_pair =
        precision_boundary_pair(&loader, &artifact, directory.path(), false, &mut evidence).await;
    let mapping_pair =
        precision_boundary_pair(&loader, &artifact, directory.path(), true, &mut evidence).await;
    assert_eq!(mapping_pair.reference_timer_callbacks, 1);
    println!(
        "runtime-init precision: entry-callbacks-per-run={} entry-callbacks={} mapping-callbacks={} preinit-markers={} timer-callbacks={}",
        entry_pair.reference_timer_callbacks,
        entry_pair.timer_callbacks(),
        mapping_pair.timer_callbacks(),
        entry_pair.preinit_markers + mapping_pair.preinit_markers,
        evidence.timer_callbacks,
    );

    for (label, result_flag) in [
        ("no-handshake", "-DINITIALIZER_RESULT=0"),
        ("initializer-error", "-DINITIALIZER_RESULT=-1"),
        ("initializer-signal", "-DINITIALIZER_SIGNAL=1"),
    ] {
        let negative = compile(
            directory.path(),
            "runtime_init_negative.c",
            &format!("{label}.so"),
            &[
                "-shared",
                "-fPIC",
                "-nostdlib",
                "-Wl,--hash-style=both",
                "-Wl,-z,now",
                result_flag,
            ],
        );
        refuse_before_entry(
            &loader,
            &negative,
            &fixture,
            &entry,
            directory.path(),
            label,
            &mut evidence,
        )
        .await;
    }
    cancel_initializer(&loader, &fixture, &entry, directory.path(), &mut evidence).await;
    let static_fixture = compile(
        directory.path(),
        "runtime_init_static.c",
        "runtime-init-static",
        &[
            "-nostdlib",
            "-static",
            "-fno-pie",
            "-no-pie",
            "-fno-stack-protector",
            "-Wl,--build-id=none",
        ],
    );
    refuse_before_entry(
        &loader,
        &artifact.path,
        &static_fixture,
        &entry_hex(&static_fixture),
        directory.path(),
        "static-image",
        &mut evidence,
    )
    .await;
    let interposed = compile(
        directory.path(),
        "runtime_init.c",
        "runtime-init-interposed",
        &[
            "-fno-pie",
            "-no-pie",
            "-rdynamic",
            "-DINTERPOSE_SYSCONF=1",
            "-ldl",
        ],
    );
    let interposed_entry = entry_hex(&interposed);
    let native_main = directory.path().join("native-interposed.entered");
    let native_callback = directory.path().join("native-interposed.callback");
    let mut native = native_command(&interposed, &interposed_entry, &native_main);
    native.arg(&native_callback);
    assert_eq!(run_native_bounded(native).code(), Some(98));
    assert_eq!(fs::read(native_main).unwrap(), b"entered\n");
    assert_eq!(fs::read(native_callback).unwrap(), b"sysconf\n");
    evidence.native_controls += 1;
    refuse_before_entry(
        &loader,
        &artifact.path,
        &interposed,
        &interposed_entry,
        directory.path(),
        "executable-sysconf",
        &mut evidence,
    )
    .await;
    for variable in ["LD_PRELOAD", "LD_AUDIT"] {
        refuse_loader_environment(
            &loader,
            &artifact,
            &fixture,
            &entry,
            directory.path(),
            variable,
            &mut evidence,
        )
        .await;
    }
    artifact.assert_unchanged();
    println!(
        "runtime-init evidence: pairs={} ptrace-runs={} initialized-runs={} refusals={} cancellations={} native-controls={} getpid-callbacks={} preinit-markers={} direct-hooks={} timer-pairs={}",
        evidence.pairs,
        evidence.ptrace_runs,
        evidence.initialized_runs,
        evidence.refusals,
        evidence.cancellations,
        evidence.native_controls,
        evidence.getpid_callbacks,
        evidence.preinit_markers,
        evidence.direct_hooks,
        evidence.timer_pairs,
    );
}
