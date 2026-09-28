//! Native process controls for the actual mapped installer and fork seam.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::pin::pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use std::time::Instant;

use reverie::Error;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::Sysno;
use reverie_liteinst::rpc::CoordinatorRpc;
use reverie_liteinst::rpc::MappedCoordinator;
use reverie_liteinst::rpc::MappedSetupListener;
use reverie_preload::tool_host::drive_ready;
use reverie_preload::trap::raw_syscall6;
use reverie_rpc_transport::mapped::MappedFailure;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;

static IN_GUEST: AtomicBool = AtomicBool::new(false);
static ROOT_PID: AtomicI32 = AtomicI32::new(0);
static MODE: AtomicUsize = AtomicUsize::new(0);
static CURRENT_CONFIG: AtomicU64 = AtomicU64::new(0);
static ROOT_CONFIG_CLONES: AtomicUsize = AtomicUsize::new(0);
static OLD_MAPPING: AtomicUsize = AtomicUsize::new(0);
static OLD_MAPPING_LEN: AtomicUsize = AtomicUsize::new(0);
static OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);
static CALLBACK: OnceLock<Arc<Callback>> = OnceLock::new();
const PAYLOAD: usize = 8192;

#[derive(Default)]
struct Callback {
    rpc: OnceLock<Weak<CoordinatorRpc<MappedGlobal>>>,
}

#[derive(Default)]
struct Config {
    connection: u64,
    value: u64,
    cached: bool,
    callback: Arc<Callback>,
}

impl Clone for Config {
    fn clone(&self) -> Self {
        if IN_GUEST.load(Ordering::Relaxed) {
            audit_callback();
            ROOT_CONFIG_CLONES.fetch_add(1, Ordering::Relaxed);
        }
        Self {
            connection: self.connection,
            value: self.value,
            cached: true,
            callback: self.callback.clone(),
        }
    }
}

impl Serialize for Config {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (self.connection, self.value).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (connection, value) = <(u64, u64)>::deserialize(deserializer)?;
        let callback = if IN_GUEST.load(Ordering::Relaxed) {
            audit_callback();
            if physical_pid() != ROOT_PID.load(Ordering::Relaxed)
                && MODE.load(Ordering::Relaxed) == 7
            {
                panic!("deliberate fresh child config panic");
            }
            CURRENT_CONFIG.store(connection, Ordering::Relaxed);
            CALLBACK.get().unwrap().clone()
        } else {
            Arc::default()
        };
        Ok(Self {
            connection,
            value,
            cached: false,
            callback,
        })
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        if !IN_GUEST.load(Ordering::Relaxed) {
            return;
        }
        audit_callback();
        let child = physical_pid() != ROOT_PID.load(Ordering::Relaxed);
        if self.cached {
            if child {
                observations().cached_drops.fetch_add(1, Ordering::Relaxed);
            }
        } else if child {
            observations().config_drops.fetch_add(1, Ordering::Relaxed);
            let rpc = self.callback.rpc.get().unwrap().upgrade().unwrap();
            let response = drive_ready(rpc.send_rpc(request(10)));
            check_response(response);
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Request {
    operation: u8,
    process: i32,
    bytes: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
struct Response {
    sender: i32,
    connection: u64,
    bytes: Vec<u8>,
}

type RpcRecord = (i32, u8, i32, u64, usize);

#[derive(Default)]
struct MappedGlobal {
    connections: Mutex<BTreeMap<i32, u64>>,
    records: Mutex<Vec<RpcRecord>>,
}

#[reverie::global_tool]
impl GlobalTool for MappedGlobal {
    type Config = Config;
    type Request = Request;
    type Response = Response;
    async fn receive_rpc(&self, from: Pid, request: Request) -> Response {
        let connection = *self
            .connections
            .lock()
            .unwrap()
            .get(&from.as_raw())
            .unwrap();
        self.records.lock().unwrap().push((
            from.as_raw(),
            request.operation,
            request.process,
            connection,
            request.bytes.len(),
        ));
        Response {
            sender: from.as_raw(),
            connection,
            bytes: request.bytes,
        }
    }
}

fn request(operation: u8) -> Request {
    audit_callback();
    Request {
        operation,
        process: physical_pid(),
        bytes: (0..PAYLOAD).map(|index| (index % 251) as u8).collect(),
    }
}

fn check_response(response: Response) {
    assert_eq!(
        response.sender,
        physical_pid(),
        "RPC must use this physical child's TID"
    );
    assert_eq!(
        response.connection,
        CURRENT_CONFIG.load(Ordering::Relaxed),
        "fresh client connection identity"
    );
    assert_eq!(
        response.bytes,
        (0..PAYLOAD)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>()
    );
}

#[derive(Default)]
struct MappedTool;

#[reverie::tool]
impl Tool for MappedTool {
    type GlobalState = MappedGlobal;
    type ThreadState = u64;
    fn subscriptions(_: &Config) -> Subscription {
        if MODE.load(Ordering::Relaxed) == 6 {
            return Subscription::default();
        }
        [Sysno::fork, Sysno::vfork, Sysno::clone, Sysno::clone3]
            .into_iter()
            .collect()
    }
    fn new(_: Pid, config: &Config) -> Self {
        audit_callback();
        assert_eq!(config.connection, 1, "outer cached config was replaced");
        assert_eq!(config.value, 0x1234_5678_9abc_def0);
        Self
    }
    fn init_thread_state(&self, _: Pid, parent: Option<(Pid, &u64)>) -> u64 {
        if let Some((_, state)) = parent {
            assert_eq!(*state, 42);
        }
        42
    }
    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        audit_callback();
        assert_eq!(guest.config().connection, 1);
        if physical_pid() != ROOT_PID.load(Ordering::Relaxed) {
            assert_eq!(observations().config_drops.load(Ordering::Relaxed), 1);
            assert_eq!(observations().cached_drops.load(Ordering::Relaxed), 0);
            assert_eq!(
                observations().capture_drops.load(Ordering::Relaxed),
                usize::from(MODE.load(Ordering::Relaxed) != 6)
            );
            observations().thread_starts.fetch_add(1, Ordering::Relaxed);
        }
        check_response(guest.send_rpc(request(20)).await);
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        ForkFuture {
            guest,
            syscall: Some(syscall),
        }
        .await
    }
}

// The short-lived injection future is dropped at the end of poll. On child
// Pending the shared driver then drops this outer future before finish_fork_child.
// It still owns a genuine mutable Guest reference, with no raw lifetime tricks.
struct ForkFuture<'a, G: Guest<MappedTool>> {
    guest: &'a mut G,
    syscall: Option<Syscall>,
}
impl<G: Guest<MappedTool>> Future for ForkFuture<'_, G> {
    type Output = Result<i64, Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let syscall = this
            .syscall
            .take()
            .expect("Tool driver must poll the fork once");
        if MODE.load(Ordering::Relaxed) == 5 {
            match pin!(this.guest.tail_inject(syscall)).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(never) => match never {},
            }
        } else {
            pin!(this.guest.inject(syscall))
                .poll(cx)
                .map(|result| result.map_err(Error::from))
        }
    }
}
impl<G: Guest<MappedTool>> Drop for ForkFuture<'_, G> {
    fn drop(&mut self) {
        audit_callback();
        assert_eq!(
            self.guest.config().connection,
            1,
            "capture sees original cached config"
        );
        assert_eq!(ROOT_CONFIG_CLONES.load(Ordering::Relaxed), 1);
        if physical_pid() != ROOT_PID.load(Ordering::Relaxed) {
            assert_eq!(
                observations().thread_starts.load(Ordering::Relaxed),
                0,
                "capture must drop before finish_fork_child"
            );
            observations().capture_drops.fetch_add(1, Ordering::Relaxed);
        }
        check_response(drive_ready(self.guest.send_rpc(request(30))));
    }
}

#[derive(Default)]
#[repr(C)]
struct Observations {
    unmaps: AtomicUsize,
    absent: AtomicUsize,
    config_drops: AtomicUsize,
    cached_drops: AtomicUsize,
    capture_drops: AtomicUsize,
    thread_starts: AtomicUsize,
}

fn observations() -> &'static Observations {
    let address = OBSERVATIONS.load(Ordering::Relaxed);
    assert_ne!(address, 0);
    // The fixture's private owner keeps this shared page until its process
    // exits; its known child is reaped before the parent reads final counters.
    unsafe { &*(address as *const Observations) }
}

fn physical_pid() -> i32 {
    unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) as i32 }
}
fn audit_callback() {
    assert_eq!(
        unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) },
        i64::from(physical_pid()),
        "callback must stay on the actual single guest thread"
    );
    for entry in std::fs::read_dir("/proc/self/fd").unwrap() {
        let path = std::fs::read_link(entry.unwrap().path());
        if let Ok(path) = path {
            let text = path.to_string_lossy();
            assert!(
                !text.starts_with("socket:[")
                    && !text.contains("memfd:reverie-rpc")
                    && !text.ends_with("setup.identity"),
                "setup fd survived into callback: {text}"
            );
        }
    }
}

// The existing RPC primitive uses libc::munmap. Interpose only the known child's
// inherited mapped range; every operation forwards to the real raw kernel gate.
// Immediate mincore occurs before any fresh allocation can reuse that address.
#[unsafe(no_mangle)]
unsafe extern "C" fn munmap(address: *mut libc::c_void, length: usize) -> libc::c_int {
    let tracked = IN_GUEST.load(Ordering::Relaxed)
        && physical_pid() != ROOT_PID.load(Ordering::Relaxed)
        && address as usize == OLD_MAPPING.load(Ordering::Relaxed);
    let result = unsafe {
        raw_syscall6(
            libc::SYS_munmap,
            [address as u64, length as u64, 0, 0, 0, 0],
        )
    };
    if tracked {
        let state = unsafe { &*(OBSERVATIONS.load(Ordering::Relaxed) as *const Observations) };
        state.unmaps.fetch_add(1, Ordering::Relaxed);
        let mut resident = 0_u8;
        let absent = unsafe {
            raw_syscall6(
                libc::SYS_mincore,
                [
                    address as u64,
                    OLD_MAPPING_LEN.load(Ordering::Relaxed) as u64,
                    (&raw mut resident) as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        if result == 0 && absent == -i64::from(libc::ENOMEM) {
            state.absent.fetch_add(1, Ordering::Relaxed);
        }
    }
    if result < 0 {
        unsafe {
            *libc::__errno_location() = -result as i32;
        }
        -1
    } else {
        0
    }
}

pub(super) fn guest(directory: &Path, case: &str) {
    assert_eq!(physical_pid(), unsafe { libc::gettid() });
    assert_eq!(std::fs::read_dir("/proc/self/task").unwrap().count(), 1);
    let mode = match case {
        "raw" => 0,
        "libc" => 1,
        "clone3" => 2,
        "vfork" => 3,
        "kernel-error" => 4,
        "tail" => 5,
        "unsubscribed" => 6,
        "config-panic" => 7,
        "setup-error" => 8,
        _ => panic!("unknown mapped case"),
    };
    MODE.store(mode, Ordering::Relaxed);
    ROOT_PID.store(physical_pid(), Ordering::Relaxed);
    let page = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(page, libc::MAP_FAILED);
    unsafe {
        page.cast::<Observations>().write(Observations::default());
    }
    OBSERVATIONS.store(page as usize, Ordering::Relaxed);
    CALLBACK.set(Arc::default()).ok().unwrap();
    IN_GUEST.store(true, Ordering::Relaxed);
    let endpoint =
        MappedCoordinator::from_bytes(&std::fs::read(directory.join("setup.identity")).unwrap())
            .unwrap();
    // The independently owned fixture host is the trusted setup owner; no
    // application threads exist and all accepted child creation is intercepted.
    let rpc =
        Arc::new(unsafe { CoordinatorRpc::<MappedGlobal>::connect_mapped(endpoint) }.unwrap());
    CALLBACK
        .get()
        .unwrap()
        .rpc
        .set(Arc::downgrade(&rpc))
        .ok()
        .unwrap();
    let borrowed_config = rpc.config();
    let original_address = borrowed_config as *const Config as usize;
    unsafe { reverie_liteinst::install_tool_with_rpc::<MappedTool>(rpc.clone()) }.unwrap();
    check_response(drive_ready(rpc.send_rpc(request(1))));
    let mappings: Vec<_> = std::fs::read_to_string("/proc/self/maps")
        .unwrap()
        .lines()
        .filter(|line| line.contains("/memfd:reverie-rpc"))
        .map(str::to_owned)
        .collect();
    assert_eq!(mappings.len(), 1);
    let (start, end) = mappings[0]
        .split_whitespace()
        .next()
        .unwrap()
        .split_once('-')
        .unwrap();
    let start = usize::from_str_radix(start, 16).unwrap();
    let end = usize::from_str_radix(end, 16).unwrap();
    assert_eq!(end - start, 4096);
    OLD_MAPPING.store(start, Ordering::Relaxed);
    OLD_MAPPING_LEN.store(end - start, Ordering::Relaxed);
    let mut clone_args = [0_u64; 12];
    clone_args[4] = libc::SIGCHLD as u64;
    if mode == 4 {
        clone_args[11] = 1;
    }
    let child = match mode {
        1 => (unsafe { libc::fork() }) as i64,
        2 | 4 => unsafe {
            libc::syscall(
                libc::SYS_clone3,
                clone_args.as_ptr(),
                if mode == 4 { 96 } else { 88 },
            )
        },
        3 => unsafe { libc::syscall(libc::SYS_vfork) },
        _ => unsafe { super::reverie_liteinst_rpc_raw_fork() },
    };
    if mode == 4 {
        assert_eq!(child, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::E2BIG),
            "actual admitted clone3 extension failure required"
        );
        check_response(drive_ready(rpc.send_rpc(request(2))));
        assert_eq!(borrowed_config as *const Config as usize, original_address);
        assert_eq!(borrowed_config.connection, 1);
        assert_eq!(observations().unmaps.load(Ordering::Relaxed), 0);
        println!("mapped kernel-error: E2BIG parent client preserved borrowed config preserved");
        return;
    }
    assert!(
        child >= 0,
        "actual fork failed: {}",
        std::io::Error::last_os_error()
    );
    if child == 0 {
        assert_eq!(borrowed_config as *const Config as usize, original_address);
        assert_eq!(borrowed_config.connection, 1);
        assert_eq!(borrowed_config.value, 0x1234_5678_9abc_def0);
        assert_eq!(observations().cached_drops.load(Ordering::Relaxed), 0);
        assert_eq!(observations().unmaps.load(Ordering::Relaxed), 1);
        assert_eq!(observations().absent.load(Ordering::Relaxed), 1);
        check_response(drive_ready(rpc.send_rpc(request(3))));
        unsafe { libc::_exit(0) };
    }
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(child as i32, &raw mut status, 0) },
        child as i32
    );
    assert!(libc::WIFEXITED(status), "child status {status}");
    assert_eq!(
        libc::WEXITSTATUS(status),
        match mode {
            7 => 118,
            8 => 123,
            _ => 0,
        }
    );
    check_response(drive_ready(rpc.send_rpc(request(4))));
    assert_eq!(borrowed_config as *const Config as usize, original_address);
    assert_eq!(borrowed_config.connection, 1);
    assert_eq!(observations().unmaps.load(Ordering::Relaxed), 1);
    assert_eq!(observations().absent.load(Ordering::Relaxed), 1);
    assert_eq!(observations().cached_drops.load(Ordering::Relaxed), 0);
    assert_eq!(
        observations().config_drops.load(Ordering::Relaxed),
        usize::from(mode != 7 && mode != 8)
    );
    assert_eq!(
        observations().capture_drops.load(Ordering::Relaxed),
        usize::from(mode != 6 && mode != 7 && mode != 8)
    );
    assert_eq!(
        observations().thread_starts.load(Ordering::Relaxed),
        usize::from(mode != 7 && mode != 8)
    );
    println!(
        "mapped {case}: child={} parent client preserved borrowed config preserved unmap=1 absent=1 config-drop={} capture-drop={} thread-start={}",
        libc::WEXITSTATUS(status),
        observations().config_drops.load(Ordering::Relaxed),
        observations().capture_drops.load(Ordering::Relaxed),
        observations().thread_starts.load(Ordering::Relaxed)
    );
}

pub(super) fn host(directory: &Path, case: &str) {
    let setup_directory = tempfile::Builder::new()
        .prefix("liteinst-mapped-")
        .tempdir()
        .unwrap();
    let setup_path = setup_directory.path().join("setup.sock");
    std::fs::write(
        directory.join("setup.socket-path"),
        setup_path.as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    let listener = MappedSetupListener::bind(&setup_path).unwrap();
    std::fs::write(
        directory.join("setup.identity"),
        listener.coordinator().to_bytes(),
    )
    .unwrap();
    let count = if matches!(case, "kernel-error" | "setup-error") {
        1
    } else {
        2
    };
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let accept = std::thread::spawn(move || {
        for connection in 1..=count {
            // Only the independently exec-created fixture guest has this run
            // identity; it keeps its stream private and follows the fork API.
            let (pid, stream) = unsafe { listener.accept(257) }.unwrap();
            sender.send((pid, connection, stream)).unwrap();
        }
    });
    let global = Arc::new(MappedGlobal::default());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut connections = Vec::new();
        for _ in 0..count {
            let (pid, connection, stream) = receiver.recv().await.unwrap();
            assert!(
                global
                    .connections
                    .lock()
                    .unwrap()
                    .insert(pid.as_raw(), connection)
                    .is_none()
            );
            let abort = stream.abort_handle();
            let stream = stream.into_async().unwrap();
            let completion = stream.completion();
            let config = Config {
                connection,
                value: 0x1234_5678_9abc_def0,
                cached: false,
                callback: Arc::default(),
            };
            let task = tokio::spawn(reverie_rpc_transport::serve_stream(
                global.clone(),
                config,
                stream,
            ));
            connections.push((connection, abort, completion, task));
        }
        accept.join().unwrap();
        // The discovered test writes this only after actually waiting for its
        // guest. Logical connection completion never stands in for process exit.
        while !directory.join("guest-reaped").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        for (connection, abort, completion, task) in connections {
            if case == "config-panic" && connection == 2 {
                // Deserialize panic drops only the newly imported stream.
                // The canonical server treats EOF at a frame boundary as Ok.
                // Observe that actual completion before owner cancellation;
                // accepting either EOF or cancellation here would mask a leak.
                let result = tokio::time::timeout(Duration::from_secs(5), task)
                    .await.expect("fresh panic stream did not close")
                    .unwrap();
                assert!(result.is_ok(), "fresh panic stream must end at frame-boundary EOF: {result:?}");
                println!("mapped connection {connection}: frame-boundary EOF after config panic");
            } else {
                abort.abort(MappedFailure::Cancelled);
                let result = task.await.unwrap();
                assert!(
                    matches!(&result, Err(reverie_rpc_transport::RpcError::Io(error)) if error.kind() == std::io::ErrorKind::ConnectionAborted),
                    "owner cancellation must retain ConnectionAborted: {result:?}"
                );
                println!("mapped connection {connection}: owner cancellation ConnectionAborted");
            }
            drop(abort);
            let deadline = Instant::now() + Duration::from_secs(5);
            while completion.result().is_none() || completion.helper_result().is_none() {
                assert!(
                    Instant::now() < deadline,
                    "mapped worker/helper cleanup remains pending"
                );
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            assert_eq!(completion.result(), Some(Ok(())));
            assert_eq!(completion.helper_result(), Some(Ok(())));
        }
    });
    for record in global.records.lock().unwrap().iter() {
        println!("mapped request: {record:?}");
    }
    println!(
        "mapped host: connections={count} actual guest reap observed worker and helper joins complete"
    );
}

struct OwnedChild(Option<std::process::Child>);
impl OwnedChild {
    fn wait(&mut self, limit: Duration) -> std::process::ExitStatus {
        let child = self.0.as_mut().unwrap();
        let deadline = Instant::now() + limit;
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id(),
                    &raw mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            assert_eq!(result, 0);
            if unsafe { info.si_pid() } != 0 {
                // Keep the PID reserved until its process group is stopped.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                return self.0.take().unwrap().wait().unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "owned mapped fixture exceeded its {limit:?} deadline"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
        }
    }
}

fn reap_adopted() {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut status = 0;
        let result = unsafe { libc::waitpid(-1, &raw mut status, libc::WNOHANG) };
        if result > 0 {
            eprintln!("mapped fixture adopted child reaped: pid={result} status={status}");
            continue;
        }
        if result < 0 {
            let error = std::io::Error::last_os_error().raw_os_error();
            if error == Some(libc::EINTR) {
                continue;
            }
            assert_eq!(error, Some(libc::ECHILD));
            return;
        }
        assert!(
            Instant::now() < deadline,
            "owned mapped fixture children remain after cleanup"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn control(directory: &Path, case: &str) {
    use std::os::unix::process::CommandExt;
    assert_eq!(physical_pid(), unsafe { libc::gettid() });
    assert_eq!(std::fs::read_dir("/proc/self/task").unwrap().count(), 1);
    // This ordinary fixture process owns only its two exec-created peers and
    // their known fork controls. It is not a production launcher/supervisor.
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );
    let result = std::panic::catch_unwind(|| {
        let binary = std::env::current_exe().unwrap();
        let spawn = |mode: &str, prefix: &str| {
            let mut command = std::process::Command::new(&binary);
            command
                .args([mode])
                .arg(directory)
                .arg(case)
                .process_group(0)
                .stdout(std::fs::File::create(directory.join(format!("{prefix}.stdout"))).unwrap())
                .stderr(std::fs::File::create(directory.join(format!("{prefix}.stderr"))).unwrap());
            let child = command.spawn().unwrap();
            std::fs::write(
                directory.join(format!("{prefix}.pid")),
                child.id().to_string(),
            )
            .unwrap();
            OwnedChild(Some(child))
        };
        let mut host = spawn("mapped-fork-host", "host");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !directory.join("setup.identity").exists() {
            assert!(
                Instant::now() < deadline,
                "mapped host readiness exceeded five seconds"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut guest = spawn("mapped-fork-guest", "guest");
        let status = guest.wait(Duration::from_secs(20));
        std::fs::write(directory.join("guest.status"), status.to_string()).unwrap();
        assert!(
            status.success(),
            "mapped {case} guest failed: {status}; {}",
            directory.display()
        );
        std::fs::write(directory.join("guest-reaped"), b"actual wait complete\n").unwrap();
        let status = host.wait(Duration::from_secs(5));
        std::fs::write(directory.join("host.status"), status.to_string()).unwrap();
        assert!(status.success(), "mapped {case} host failed: {status}");
        let guest_output = std::fs::read_to_string(directory.join("guest.stdout")).unwrap();
        let expected = match case {
            "kernel-error" => "mapped kernel-error: E2BIG parent client preserved borrowed config preserved\n".to_owned(),
            "config-panic" => "mapped config-panic: child=118 parent client preserved borrowed config preserved unmap=1 absent=1 config-drop=0 capture-drop=0 thread-start=0\n".to_owned(),
            "setup-error" => "mapped setup-error: child=123 parent client preserved borrowed config preserved unmap=1 absent=1 config-drop=0 capture-drop=0 thread-start=0\n".to_owned(),
            _ => format!("mapped {case}: child=0 parent client preserved borrowed config preserved unmap=1 absent=1 config-drop=1 capture-drop={} thread-start=1\n", usize::from(case != "unsubscribed")),
        };
        assert_eq!(guest_output, expected);
        let guest_error = std::fs::read_to_string(directory.join("guest.stderr")).unwrap();
        if case == "config-panic" {
            assert!(guest_error.contains("deliberate fresh child config panic"));
        } else {
            assert_eq!(guest_error, "");
        }
        assert_eq!(std::fs::read(directory.join("host.stderr")).unwrap(), b"");
        print!("{guest_output}");
    });
    reap_adopted();
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}
