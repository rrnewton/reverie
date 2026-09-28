//! Controlled native host and real installed Tool endpoint-set controls.

use std::future::Future;
use std::io::{self, Write};
use std::path::Path;
use std::pin::{Pin, pin};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use reverie::{Error, GlobalRPC, GlobalTool, Guest, Pid, Subscription, Tool};
use reverie::syscalls::{Syscall, Sysno};
use reverie_liteinst::rpc::{CoordinatorRpc, InstalledCoordinator, InstalledSetupListener};
use reverie_preload::tool_host::drive_ready;
use reverie_preload::trap::raw_syscall6;
use reverie_rpc_transport::guest_log as g;
use reverie_rpc_transport::mapped::{MappedAbort, MappedCompletion};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[path = "../../tests/fixtures/installed_host.rs"]
mod ownership;
#[path = "../../tests/fixtures/installed_io_uring.rs"]
mod installed_io_uring;
#[path = "../../tests/fixtures/installed_setup.rs"]
mod installed_setup;

static IN_GUEST: AtomicBool = AtomicBool::new(false);
static ROOT: AtomicI32 = AtomicI32::new(0);
static CURRENT: AtomicU64 = AtomicU64::new(0);
static CONFIG_DROPS: AtomicUsize = AtomicUsize::new(0);
static CAPTURE_DROPS: AtomicUsize = AtomicUsize::new(0);
static THREAD_STARTS: AtomicUsize = AtomicUsize::new(0);
static CALLBACK: OnceLock<Arc<Callback>> = OnceLock::new();
static MODE: OnceLock<String> = OnceLock::new();
static OUTPUT_FAILED: AtomicBool = AtomicBool::new(false);
static REQUEST_DROPS: AtomicUsize = AtomicUsize::new(0);
static REQUEST_POINTER: AtomicUsize = AtomicUsize::new(0);
static REQUEST_FREES: AtomicUsize = AtomicUsize::new(0);

// This isolated fixture observes the actual System deallocation, not only a
// Rust Drop counter. No production allocator behavior or old fixture changes.
unsafe extern "C" { fn __libc_free(pointer: *mut libc::c_void); }
#[unsafe(no_mangle)]
unsafe extern "C" fn free(pointer: *mut libc::c_void) {
    let observed = !pointer.is_null() && pointer as usize == REQUEST_POINTER.load(Ordering::Relaxed);
    unsafe { __libc_free(pointer) };
    if observed { REQUEST_FREES.fetch_add(1, Ordering::Relaxed); }
}

#[derive(Serialize, Deserialize)]
struct Request {
    operation: u8,
    process: i32,
    payload: Vec<u8>,
    #[serde(skip)]
    external_owner: bool,
}
impl Request {
    fn new(operation: u8) -> Self { Self { operation, process: pid(), payload: Vec::new(), external_owner: false } }
}
impl Drop for Request {
    fn drop(&mut self) {
        if self.external_owner {
            REQUEST_DROPS.fetch_add(1, Ordering::Relaxed);
            // This real recursive RPC must run after the outer request's lock
            // and runtime allocation scope have ended. Fields free afterward.
            let rpc = CALLBACK.get().unwrap().rpc.get().unwrap().upgrade().unwrap();
            check(drive_ready(rpc.send_rpc(Request::new(31))));
        }
    }
}

#[derive(Default)]
struct Callback { rpc: OnceLock<Weak<CoordinatorRpc<Global>>> }

#[derive(Default)]
struct Config { connection: u64, cached: bool, callback: Arc<Callback> }

impl Serialize for Config {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.connection.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let connection = u64::deserialize(deserializer)?;
        let callback = if IN_GUEST.load(Ordering::Relaxed) {
            audit();
            CURRENT.store(connection, Ordering::Relaxed);
            emit("config-decode");
            CALLBACK.get().unwrap().clone()
        } else { Arc::default() };
        Ok(Self { connection, cached: false, callback })
    }
}
impl Clone for Config {
    fn clone(&self) -> Self {
        if IN_GUEST.load(Ordering::Relaxed) { audit(); emit("config-clone"); }
        Self { connection: self.connection, cached: true, callback: self.callback.clone() }
    }
}
impl Drop for Config {
    fn drop(&mut self) {
        if IN_GUEST.load(Ordering::Relaxed) {
            audit();
            assert!(!self.cached, "cached Config was dropped before physical exit");
            assert_ne!(pid(), ROOT.load(Ordering::Relaxed));
            CONFIG_DROPS.fetch_add(1, Ordering::Relaxed);
            emit("retired-config-drop");
            let rpc = self.callback.rpc.get().unwrap().upgrade().unwrap();
            check(drive_ready(rpc.send_rpc(Request::new(10))));
        }
    }
}

#[derive(Default)]
struct Global {
    records: Mutex<Vec<(i32, u8, i32)>>,
    root: AtomicI32,
    destination_failure: Option<Arc<AtomicBool>>,
    capacity_prefix: Option<Arc<Mutex<Vec<u8>>>>,
}

#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = Config;
    type Request = Request;
    type Response = i32;
    async fn receive_rpc(&self, from: Pid, request: Self::Request) -> i32 {
        let (operation, process) = (request.operation, request.process);
        if operation == 1 {
            assert_eq!(request.payload, (0..257).map(|index| (index % 251) as u8).collect::<Vec<_>>());
        } else { assert!(request.payload.is_empty()); }
        if operation == 40 {
            let failed = self.destination_failure.as_ref().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !failed.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "actual destination EIO not observed");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
        if operation == 20 && from.as_raw() == self.root.load(Ordering::Relaxed)
            && let Some(bytes) = &self.capacity_prefix {
                let expected = format!("private thread-start {} 1\n", from.as_raw());
                let deadline = Instant::now() + Duration::from_secs(2);
                while !bytes.lock().unwrap().ends_with(expected.as_bytes()) {
                    assert!(Instant::now() < deadline, "pre-fork private publication not observed");
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
        }
        assert_eq!(from.as_raw(), process, "fresh process client identity");
        self.records.lock().unwrap().push((from.as_raw(), operation, process));
        from.as_raw()
    }
}

#[derive(Default)]
struct InstalledTool;
#[reverie::tool]
impl Tool for InstalledTool {
    type GlobalState = Global;
    type ThreadState = u64;
    fn subscriptions(_: &Config) -> Subscription {
        [Sysno::fork, Sysno::clone3, Sysno::exit_group].into_iter().collect()
    }
    fn new(_: Pid, config: &Config) -> Self {
        audit(); assert_eq!(config.connection, 1); emit("tool-new"); Self
    }
    fn init_thread_state(&self, _: Pid, parent: Option<(Pid, &u64)>) -> u64 {
        if let Some((_, value)) = parent { assert_eq!(*value, 42); }
        42
    }
    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        audit(); assert_eq!(guest.config().connection, 1);
        if pid() != ROOT.load(Ordering::Relaxed) {
            assert_eq!(CONFIG_DROPS.load(Ordering::Relaxed), 1);
            assert_eq!(CAPTURE_DROPS.load(Ordering::Relaxed), 1);
        }
        THREAD_STARTS.fetch_add(1, Ordering::Relaxed);
        emit("thread-start"); check(guest.send_rpc(Request::new(20)).await); Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(&self, guest: &mut G, syscall: Syscall) -> Result<i64, Error> {
        ForkFuture { guest, syscall: Some(syscall) }.await
    }
    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(self, _: Pid, rpc: &G, _: reverie::ExitStatus) -> Result<(), Error> {
        audit(); emit("tool-exit"); check(rpc.send_rpc(Request::new(80)).await); Ok(())
    }
}
impl Drop for InstalledTool {
    fn drop(&mut self) { if IN_GUEST.load(Ordering::Relaxed) { audit(); emit("tool-drop"); } }
}

struct ForkFuture<'a, G: Guest<InstalledTool>> { guest: &'a mut G, syscall: Option<Syscall> }
impl<G: Guest<InstalledTool>> Future for ForkFuture<'_, G> {
    type Output = Result<i64, Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let syscall = this.syscall.take().expect("one actual injected operation");
        pin!(this.guest.inject(syscall)).poll(cx).map(|result| result.map_err(Error::from))
    }
}
impl<G: Guest<InstalledTool>> Drop for ForkFuture<'_, G> {
    fn drop(&mut self) {
        audit(); assert_eq!(self.guest.config().connection, 1);
        if pid() != ROOT.load(Ordering::Relaxed) && THREAD_STARTS.load(Ordering::Relaxed) == 1 {
            // Root's one thread-start is inherited; the actual child start has
            // not run when the driver's safe borrowed future is destroyed.
            CAPTURE_DROPS.fetch_add(1, Ordering::Relaxed);
        }
        emit("future-drop"); check(drive_ready(self.guest.send_rpc(Request::new(30))));
    }
}

fn pid() -> i32 { unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) as i32 } }
fn check(response: i32) { assert_eq!(response, pid()); }
fn audit() {
    assert_eq!(unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) }, i64::from(pid()));
    for entry in std::fs::read_dir("/proc/self/fd").unwrap() {
        if let Ok(path) = std::fs::read_link(entry.unwrap().path()) {
            let name = path.to_string_lossy();
            assert!(!name.starts_with("socket:[") && !name.contains("memfd:reverie-rpc")
                && !name.contains("memfd:reverie-guest-log") && !name.ends_with("setup.identity"),
                "private setup descriptor survived callback: {name}");
        }
    }
}
fn emit(label: &str) {
    let (public, private) = reverie_liteinst::mapped_log_producers().unwrap();
    let mode = MODE.get().unwrap().as_str();
    for (role, writer) in [("public", public), ("private", private)] {
        let result = writer.write_record(format!("{role} {label} {} {}\n", pid(), CURRENT.load(Ordering::Relaxed)).as_bytes());
        let inactive = failed_role(mode) == Some(role) && (OUTPUT_FAILED.load(Ordering::Acquire)
            || (mode.contains("capacity") && (pid() != ROOT.load(Ordering::Relaxed)
                || matches!(label, "future-drop" | "parent-body" | "tool-exit" | "tool-drop"))));
        if inactive { assert!(result.is_err(), "failed capture cannot resume publication"); }
        else { result.unwrap(); }
    }
}

core::arch::global_asm!(r#"
.text
.p2align 4
.global installed_raw_fork
.hidden installed_raw_fork
.type installed_raw_fork,@function
installed_raw_fork:
    mov eax, 57
    syscall
    nop
    nop
    nop
    ret
.size installed_raw_fork, .-installed_raw_fork
"#);
unsafe extern "C" { fn installed_raw_fork() -> i64; }

fn kernel_error(mode: &str) -> bool { mode.ends_with("kernel-error") }
fn failed_role(mode: &str) -> Option<&'static str> {
    if mode.starts_with("public-failure") { Some("public") }
    else if mode.starts_with("private-failure") || mode.starts_with("private-capacity") { Some("private") }
    else { None }
}

fn guest(directory: &Path, mode: &str) {
    assert_eq!(pid(), unsafe { libc::gettid() });
    assert_eq!(std::fs::read_dir("/proc/self/task").unwrap().count(), 1);
    ROOT.store(pid(), Ordering::Relaxed);
    CALLBACK.set(Arc::default()).ok().unwrap();
    MODE.set(mode.to_owned()).unwrap();
    IN_GUEST.store(true, Ordering::Relaxed);
    let endpoint = InstalledCoordinator::from_bytes(&std::fs::read(directory.join("setup.identity")).unwrap()).unwrap();
    let rpc = Arc::new(unsafe { CoordinatorRpc::<Global>::connect_installed(endpoint, Duration::from_secs(5), Duration::from_secs(2)) }.unwrap());
    CALLBACK.get().unwrap().rpc.set(Arc::downgrade(&rpc)).ok().unwrap();
    let borrowed = rpc.config();
    let original = borrowed as *const Config as usize;
    let mut first_request = Request::new(1);
    first_request.payload = (0..257).map(|index| (index % 251) as u8).collect();
    first_request.external_owner = true;
    REQUEST_POINTER.store(first_request.payload.as_ptr() as usize, Ordering::Relaxed);
    unsafe { reverie_liteinst::install_tool_with_rpc::<InstalledTool>(rpc.clone()) }.unwrap();
    // No priming guest syscall precedes this first outer Arc RPC.
    check(drive_ready(rpc.send_rpc(first_request)));
    assert_eq!(REQUEST_DROPS.load(Ordering::Relaxed), 1);
    assert_eq!(REQUEST_FREES.load(Ordering::Relaxed), 1, "externally allocated request must be freed outside dispatch");
    REQUEST_POINTER.store(0, Ordering::Relaxed);
    if mode.contains("failure") {
        let (public, private) = reverie_liteinst::mapped_log_producers().unwrap();
        let writer = if failed_role(mode) == Some("public") { public } else { private };
        writer.write_record(b"destination-error\n").unwrap();
        check(drive_ready(rpc.send_rpc(Request::new(40))));
        // Only after the actual destination returned EIO is this capture marked
        // failed. The healthy capture and native fork still have all duties.
        writer.record_failed();
        OUTPUT_FAILED.store(true, Ordering::Release);
    }
    let mut args = [0_u64; 12]; args[4] = libc::SIGCHLD as u64;
    if kernel_error(mode) { args[11] = 1; }
    let child = if kernel_error(mode) || mode == "clone3" {
        unsafe { libc::syscall(libc::SYS_clone3, args.as_ptr(), if kernel_error(mode) { 96 } else { 88 }) }
    } else { unsafe { installed_raw_fork() } };
    if kernel_error(mode) {
        assert_eq!(child, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::E2BIG), "actual admitted clone3 extension failure required");
        check(drive_ready(rpc.send_rpc(Request::new(2))));
    } else {
        assert!(child >= 0, "actual fork: {}", io::Error::last_os_error());
        if child == 0 {
            assert_eq!(borrowed as *const Config as usize, original);
            assert_eq!(borrowed.connection, 1);
            assert_eq!(CURRENT.load(Ordering::Relaxed), 2);
            assert_eq!(CONFIG_DROPS.load(Ordering::Relaxed), 1);
            assert_eq!(CAPTURE_DROPS.load(Ordering::Relaxed), 1);
            check(drive_ready(rpc.send_rpc(Request::new(3))));
            emit("child-body");
            unsafe { libc::_exit(0); }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child as i32, &raw mut status, 0) }, child as i32);
        assert_eq!(status, 0, "actual child wait status");
        check(drive_ready(rpc.send_rpc(Request::new(4))));
    }
    assert_eq!(borrowed as *const Config as usize, original);
    assert_eq!(borrowed.connection, 1);
    assert_eq!(CURRENT.load(Ordering::Relaxed), 1);
    if mode == "io-uring" {
        audit();
        let report = installed_io_uring::run_observed(|case| {
            std::fs::write(directory.join(format!("io-case-{}.txt", case.name)), format!("{case:#?}\n")).unwrap();
        });
        std::fs::write(directory.join("installed-io-uring.txt"), format!("{report:#?}\n")).unwrap();
        assert!(report.all_passed(), "ordinary io_uring case failed or unmeasured: {report:#?}");
        audit();
    }
    emit("parent-body");
    std::io::stdout().write_all(b"configured stdout\0\xff\n").unwrap();
    std::io::stderr().write_all(b"configured stderr\0\xfe\n").unwrap();
    if mode == "root-signal" {
        assert_eq!(unsafe { libc::kill(pid(), libc::SIGKILL) }, 0);
        panic!("SIGKILL unexpectedly returned to live guest");
    }
    unsafe { libc::_exit(if mode == "root-nonzero" { 7 } else { 0 }); }
}

#[derive(Clone, Default)]
struct Destination {
    bytes: Arc<Mutex<Vec<u8>>>,
    fail_marker: bool,
    failed: Arc<AtomicBool>,
}
impl Write for Destination {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_marker && bytes.windows(b"destination-error".len()).any(|part| part == b"destination-error") {
            self.failed.store(true, Ordering::Release);
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        self.bytes.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}
impl g::CaptureDestination for Destination {
    fn progress(&self) -> g::DestinationProgress { g::DestinationProgress { acknowledged_data_bytes: self.bytes.lock().unwrap().len() as u64, ..Default::default() } }
}
fn options() -> g::CaptureOptions {
    g::CaptureOptions {
        limits: g::CaptureLimits { producers: 16, slots_per_producer: 8, max_record_bytes: 8192,
            host_pending_bytes: 32768, guest_pending_bytes: 32768, pending_records: 32, diagnostic_bytes: 65536 },
        timeouts: g::CaptureTimeouts { startup: Duration::from_secs(2), blocked_publication: Duration::from_secs(2), final_drain: Duration::from_millis(200) },
    }
}

struct Connection {
    pid: i32,
    role: &'static str,
    abort: MappedAbort,
    completion: MappedCompletion,
    task: tokio::task::JoinHandle<Result<(), reverie_rpc_transport::RpcError>>,
}

fn host(directory: &Path, mode: &str) {
    let mut owner = ownership::ControlledHost::enter().unwrap();
    let deadline = Instant::now() + Duration::from_secs(if mode == "io-uring" { 8 } else { 30 });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host_inner(&mut owner, directory, mode, deadline)));
    if let Err(payload) = result {
        owner.cancel();
        let cleanup = owner.drain_until(deadline);
        std::fs::write(directory.join("failed-host-cleanup.txt"), format!("{cleanup:?}\n{:?}\n", owner.snapshot())).unwrap();
        std::panic::resume_unwind(payload);
    }
}

fn host_inner(owner: &mut ownership::ControlledHost, directory: &Path, mode: &str, deadline: Instant) {
    let (mut processes, observer) = unsafe { g::MappedProcessOwner::new() };
    let public_bytes = Destination { fail_marker: mode.starts_with("public-failure"), ..Default::default() };
    let private_bytes = Destination { fail_marker: mode.starts_with("private-failure"), ..Default::default() };
    let mut private_options = options();
    private_options.limits.max_record_bytes = 6144;
    if mode.contains("capacity") { private_options.limits.producers = 2; }
    let (mut public, public_fd, public_writer) = unsafe { g::prepared_mapped_capture(options(), public_bytes.clone(), observer.clone()) }.unwrap();
    let (mut private, private_fd, private_writer) = unsafe { g::prepared_mapped_capture(private_options, private_bytes.clone(), observer.clone()) }.unwrap();
    public_writer.write_record(b"public host initialization\n").unwrap();
    private_writer.write_record(b"private host initialization\n").unwrap();
    let statistics = mode != "statistics-off";
    let socket_directory = tempfile::Builder::new().prefix("li-installed-").tempdir().unwrap();
    let mut listener = unsafe { InstalledSetupListener::bind(socket_directory.path().join("setup.sock"), public_fd, private_fd, statistics, libc::geteuid(), libc::getegid()) }.unwrap();
    std::fs::write(directory.join("setup.identity"), listener.coordinator().to_bytes()).unwrap();
    let count = if kernel_error(mode) { 1 } else { 2 };
    let (sender, receiver) = std::sync::mpsc::channel();
    let accept = std::thread::spawn(move || {
        for _ in 0..count {
            sender.send(unsafe { listener.accept(4096, deadline) }).unwrap();
        }
    });
    let global = Arc::new(Global {
        destination_failure: if mode.starts_with("public-failure") { Some(public_bytes.failed.clone()) }
            else if mode.starts_with("private-failure") { Some(private_bytes.failed.clone()) } else { None },
        capacity_prefix: mode.contains("capacity").then(|| private_bytes.bytes.clone()),
        ..Default::default()
    });
    let stats = reverie_liteinst::MappedStatsCollector::default();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.arg("guest").arg(directory).arg(mode).stdout(Stdio::piped()).stderr(Stdio::piped());
    let root = owner.spawn_root(&mut command).unwrap();
    global.root.store(root, Ordering::Relaxed);
    owner.close_admission(); processes.close_admission();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap();
    runtime.block_on(async {
        let mut connections = Vec::new(); let mut received = 0;
        loop {
            if Instant::now() >= deadline - Duration::from_secs(3) {
                std::fs::write(directory.join("timeout-public.bin"), &*public_bytes.bytes.lock().unwrap()).unwrap();
                std::fs::write(directory.join("timeout-private.bin"), &*private_bytes.bytes.lock().unwrap()).unwrap();
                std::fs::write(directory.join("timeout-rpc.txt"), format!("received={received}; records={:?}\n{:?}\n", global.records.lock().unwrap(), owner.snapshot())).unwrap();
                for file in ["stat", "wchan", "syscall", "status"] {
                    let bytes = std::fs::read(format!("/proc/{root}/{file}"));
                    std::fs::write(directory.join(format!("timeout-{file}.txt")), format!("{bytes:?}\n")).unwrap();
                }
            }
            assert!(Instant::now() < deadline - Duration::from_secs(3), "installed execution exceeded its owned interval");
            while let Ok(set) = receiver.try_recv() {
                let set = set.unwrap(); received += 1;
                assert!(set.pid.as_raw() > 0);
                let inactive = if received == 1 { None } else { failed_role(mode) };
                assert_eq!([set.public_incarnation, set.private_incarnation], [if inactive == Some("public") { 0 } else { received }, if inactive == Some("private") { 0 } else { received }]);
                assert_eq!(set.process_identity, received as u64);
                assert_eq!(set.parent_identity, if received == 1 { 0 } else { 1 });
                let abort = set.main.abort_handle(); let stream = set.main.into_async().unwrap();
                let completion = stream.completion();
                let config = Config { connection: received as u64, cached: false, callback: Arc::default() };
                let task = tokio::spawn(reverie_rpc_transport::serve_stream(global.clone(), config, stream));
                connections.push(Connection { pid: set.pid.as_raw(), role: "main", abort, completion, task });
                if let Some(stream) = set.statistics {
                    assert!(statistics);
                    let abort = stream.abort_handle(); let stream = stream.into_async().unwrap();
                    let completion = stream.completion(); let stats = stats.clone();
                    let task = tokio::spawn(async move { stats.serve(stream).await });
                    connections.push(Connection { pid: set.pid.as_raw(), role: "statistics", abort, completion, task });
                } else { assert!(!statistics); }
            }
            owner.poll(deadline).unwrap();
            assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) }, 0);
            if owner.snapshot().complete { break; }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(received, count);
        accept.join().unwrap();
        let waited = owner.snapshot();
        std::fs::write(directory.join("processes.txt"), format!("{waited:#?}\n")).unwrap();
        assert!(waited.echild && waited.ownership_known && waited.admission_closed && !waited.spawn_in_progress);
        assert!(waited.stdout_pipe && waited.stderr_pipe && waited.stdout_eof && waited.stderr_eof);
        assert_eq!(waited.stdout, b"configured stdout\0\xff\n");
        assert_eq!(waited.stderr, b"configured stderr\0\xfe\n");
        let status = waited.root_status.unwrap();
        if mode == "root-signal" {
            assert!(libc::WIFSIGNALED(status)); assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
        } else {
            assert!(libc::WIFEXITED(status)); assert_eq!(libc::WEXITSTATUS(status), if mode == "root-nonzero" { 7 } else { 0 });
        }
        unsafe { processes.root_reaped(root, status) }.unwrap();
        unsafe { processes.all_reaped() }.unwrap();
        let mut outcomes = Vec::new();
        for connection in connections {
            let killed = mode == "root-signal" && connection.pid == root;
            if killed {
                // Only the real consumed root SIGKILL status authorizes this
                // failure notification. It does not invent graceful closure.
                connection.abort.abort(reverie_rpc_transport::mapped::MappedFailure::PeerFailed);
            }
            // Proper runtime exit must close logically without owner abort.
            // Keep the abort owner through join so error cleanup can wake peers.
            let result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), connection.task).await.unwrap().unwrap();
            outcomes.push(format!("{} {} {result:?}", connection.pid, connection.role));
            if killed {
                assert!(matches!(&result, Err(reverie_rpc_transport::RpcError::Io(error)) if error.kind() == io::ErrorKind::ConnectionReset), "killed peer must remain a connection failure: {result:?}");
            } else {
                assert!(result.is_ok(), "{} stream failed: {result:?}", connection.role);
            }
            while connection.completion.result().is_none() || connection.completion.helper_result().is_none() {
                assert!(Instant::now() < deadline, "mapped worker cleanup incomplete");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            assert_eq!(connection.completion.result(), Some(Ok(())));
            assert_eq!(connection.completion.helper_result(), Some(Ok(())));
            drop(connection.abort);
        }
        std::fs::write(directory.join("connection-results.txt"), outcomes.join("\n") + "\n").unwrap();
    });
    let stats = stats.try_into_source().ok().expect("statistics server owner retained");
    assert_eq!(stats.snapshot().process_reports(), if statistics { (count - usize::from(mode == "root-signal")) as u64 } else { 0 });
    for (role, writer) in [("public", &public_writer), ("private", &private_writer)] {
        let result = writer.write_record(format!("{role} host cleanup\n").as_bytes());
        if mode.contains("capacity") && failed_role(mode) == Some(role) { assert!(result.is_err(), "exhausted shared buffer cannot accept cleanup"); }
        else { result.unwrap(); }
    }
    let raw_status = owner.snapshot().root_status.unwrap();
    let run = if libc::WIFEXITED(raw_status) && libc::WEXITSTATUS(raw_status) == 0 { g::RunState::Succeeded } else { g::RunState::Failed };
    public.handle().run_state(run); private.handle().run_state(run);
    public.request_close_until(deadline); private.request_close_until(deadline);
    let public_report = public.finish_until(deadline); let private_report = private.finish_until(deadline);
    std::fs::write(directory.join("capture-reports.txt"), format!("public={public_report:#?}\nprivate={private_report:#?}\nstatistics={}\n", stats.snapshot())).unwrap();
    assert_eq!(public_report.qualifies(), !matches!(mode, "root-nonzero" | "root-signal") && failed_role(mode) != Some("public"));
    assert_eq!(private_report.qualifies(), !matches!(mode, "root-nonzero" | "root-signal") && failed_role(mode) != Some("private"));
    assert!(!public_report.capture.guest.peer_closed && !private_report.capture.guest.peer_closed);
    if mode == "root-signal" {
        for report in [&public_report, &private_report] {
            assert_eq!(report.processes.root_status, Some(libc::SIGKILL));
            assert_eq!(report.capture.guest.run, g::RunState::Failed);
            assert_eq!(report.capture.guest.phase, g::Phase::Incomplete);
            let root_stream = report.capture.streams.iter().find(|stream| stream.pid == root as u64).unwrap();
            assert!(!root_stream.finished, "SIGKILL cannot manufacture root FINISH");
            assert_eq!(report.capture.streams.len(), 3);
            assert_eq!(report.capture.streams[0].incarnation, 1);
            assert_eq!(report.capture.streams[0].pid, std::process::id() as u64);
            assert!(!report.capture.streams[0].finished, "host close uses admission/publication, not guest FINISH");
            assert_eq!(report.capture.streams[2].incarnation, 3);
            assert_ne!(report.capture.streams[2].pid, root as u64);
            assert!(report.capture.streams[2].finished, "normally exited child still owes FINISH");
        }
    }
    let public = public_bytes.bytes.lock().unwrap(); let private = private_bytes.bytes.lock().unwrap();
    std::fs::write(directory.join("public.bin"), &*public).unwrap(); std::fs::write(directory.join("private.bin"), &*private).unwrap();
    if let Some(role) = failed_role(mode) {
        let (failed, healthy) = if role == "public" { (&public_report, &private_report) } else { (&private_report, &public_report) };
        assert_eq!(failed.capture.guest.run, g::RunState::Succeeded, "logging failure does not change native exit");
        assert_eq!(failed.capture.guest.phase, g::Phase::Incomplete);
        assert_eq!(failed.capture.streams.len(), 2, "failed role cannot invent a child reservation");
        assert!(!failed.capture.streams[1].finished);
        assert!(healthy.qualifies());
        assert_eq!(healthy.capture.streams.len(), 3, "healthy reservation is never erased");
        if kernel_error(mode) {
            let cancelled = &healthy.capture.streams[2];
            assert_eq!(cancelled.pid, 0); assert!(!cancelled.finished); assert_eq!(cancelled.complete_records, 0);
        } else { assert!(healthy.capture.streams[2].finished); }
        if mode.contains("failure") {
            let destination = if role == "public" { &public_bytes } else { &private_bytes };
            assert!(destination.failed.load(Ordering::Acquire));
            assert!(failed.capture.publication.error.is_some(), "actual destination failure retained");
        }
    }
    let records = global.records.lock().unwrap();
    let child = records.iter().find(|record| record.0 != root).map(|record| record.0);
    for (role, bytes) in [("public", &*public), ("private", &*private)] {
        let text = std::str::from_utf8(bytes).unwrap();
        if failed_role(mode) == Some(role) {
            let mut exact = format!("{role} host initialization\n");
            for label in ["config-decode", "config-clone", "tool-new"] {
                exact.push_str(&format!("{role} {label} {root} 1\n"));
            }
            if mode.contains("capacity") { exact.push_str(&format!("{role} thread-start {root} 1\n")); }
            assert_eq!(text, exact, "retained failed-output bytes");
            continue;
        }
        assert!(text.starts_with(&format!("{role} host initialization\n")) && text.ends_with(&format!("{role} host cleanup\n")));
        let mut per_process = std::collections::BTreeMap::<i32, Vec<String>>::new();
        for line in text.split_inclusive('\n').skip(1).take(text.lines().count() - 2) {
            assert!(line.starts_with(&format!("{role} ")) && line.ends_with('\n'));
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 4, "complete record framing");
            per_process.entry(fields[2].parse().unwrap()).or_default().push(line.to_owned());
        }
        let expected = |pid: i32, connection: u64, labels: &[&str]| labels.iter().map(|label| format!("{role} {label} {pid} {connection}\n")).collect::<Vec<_>>();
        let root_labels: &[&str] = if mode == "root-signal" {
            &["config-decode", "config-clone", "tool-new", "thread-start", "future-drop", "parent-body"]
        } else {
            &["config-decode", "config-clone", "tool-new", "thread-start", "future-drop", "parent-body", "future-drop", "tool-exit", "tool-drop"]
        };
        assert_eq!(per_process.remove(&root).unwrap(), expected(root, 1, root_labels));
        if let Some(child) = child {
            assert_eq!(per_process.remove(&child).unwrap(), expected(child, 2, &["config-decode", "retired-config-drop", "future-drop", "tool-new", "tool-drop", "thread-start", "child-body", "future-drop", "tool-exit", "tool-drop"]));
        }
        assert!(per_process.is_empty(), "unexpected producer records");
    }
    assert_eq!(records.iter().filter(|record| record.1 == 80).count(), count - usize::from(mode == "root-signal"));
    assert_eq!(records.iter().filter(|record| record.1 == 10).count(), count - 1);
    std::fs::write(directory.join("rpc-records.txt"), format!("{records:#?}\n")).unwrap();
    println!("installed mapping complete: {mode}; processes={count}; statistics={statistics}");
}

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let directory = Path::new(&args[2]); let mode = args[3].to_str().unwrap();
    match args[1].to_str().unwrap() { "host" => host(directory, mode), "guest" => guest(directory, mode), "setup-host" => installed_setup::host(directory, mode), "setup-client" => installed_setup::client(directory, mode), _ => panic!("unknown fixture operation") }
}
