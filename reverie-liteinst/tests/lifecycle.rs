use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reverie::Backend;
use reverie::BackendStatsSnapshot;
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
use reverie_liteinst::LiteinstDispatchPath;
use reverie_liteinst::SITE_PATCHING_ENV;
use reverie_liteinst::TOOL_PRELOAD_ENV;

const RPC_GETPID: u64 = 1;
const RPC_CLOCK_GETTIME: u64 = 2;
const RPC_GETTIMEOFDAY: u64 = 3;
const RPC_FORK: u64 = 4;
const RPC_CLOSE_RANGE: u64 = 5;
const RPC_BLOCKING: u64 = 6;
/// Tags a process's exit-time count of its Tool callbacks.
const RPC_CALLBACK_COUNT: u64 = 1 << 32;
const BACKEND_OUTPUT_CHILD_ENV: &str = "REVERIE_LITEINST_BACKEND_OUTPUT_TEST_CHILD";

#[derive(Debug, Default)]
struct LifecycleGlobal {
    getpid: AtomicU64,
    clock_gettime: AtomicU64,
    gettimeofday: AtomicU64,
    fork: AtomicU64,
    /// Guest `close_range` calls the in-guest Tool saw.
    close_range: AtomicU64,
    /// Requests sent with `blocking_global_rpc`.
    blocking: AtomicU64,
    /// Every in-guest Tool syscall callback, counted in guest memory
    /// independently of the backend's statistics and reported once per
    /// process at exit.
    callbacks: AtomicU64,
    /// Pids reported through `on_backend_process_exited`.
    exited: std::sync::Mutex<Vec<i32>>,
    /// Calls of `report_backend_failure`.
    backend_failures: AtomicU64,
}

/// What `LifecycleGlobal::backend_pending_process_exits` returns, so a test can
/// tell the global's answer from the reporter's empty one after the run.
const PENDING_EXIT_SENTINEL: i32 = 4242;

#[reverie::global_tool]
impl GlobalTool for LifecycleGlobal {
    type Request = u64;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Tid, event: u64) {
        if event & RPC_CALLBACK_COUNT != 0 {
            self.callbacks
                .fetch_add(event & !RPC_CALLBACK_COUNT, Ordering::Relaxed);
            return;
        }
        let counter = match event {
            RPC_GETPID => &self.getpid,
            RPC_CLOCK_GETTIME => &self.clock_gettime,
            RPC_GETTIMEOFDAY => &self.gettimeofday,
            RPC_FORK => &self.fork,
            RPC_CLOSE_RANGE => &self.close_range,
            RPC_BLOCKING => &self.blocking,
            _ => panic!("unknown lifecycle fixture RPC {event}"),
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn on_backend_process_exited(&self, pid: i32) {
        self.exited.lock().unwrap().push(pid);
    }

    fn backend_pending_process_exits(&self) -> Vec<i32> {
        vec![PENDING_EXIT_SENTINEL]
    }

    fn report_backend_failure(&self, _event: reverie::BackendFailure) {
        self.backend_failures.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct CoordinatorOnlyTool;

#[reverie::tool]
impl Tool for CoordinatorOnlyTool {
    type GlobalState = LifecycleGlobal;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getpid, Sysno::clock_gettime, Sysno::gettimeofday]
            .into_iter()
            .collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        _guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        panic!(
            "coordinator-only Tool reached host syscall dispatch for {:?}",
            syscall.number()
        );
    }
}

fn compile_noop_preload() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lifecycle_preload.c");
    let output = directory.path().join("lifecycle-preload.so");
    let result = std::process::Command::new("cc")
        .args(["-shared", "-fPIC", "-Wl,--build-id=none"])
        .arg(source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "failed to compile lifecycle preload:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    (directory, output)
}

fn guest_command(mode: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-lifecycle-guest"));
    command.arg(mode);
    command
}

/// A guest for a `*_preload_data` launcher: it fails unless its coordinator
/// came through the bootstrap descriptor rather than the environment.
fn bootstrap_guest_command(mode: &str) -> Command {
    let mut command = guest_command(mode);
    command.env("REVERIE_LITEINST_LIFECYCLE_EXPECT_BOOTSTRAP", "1");
    command
}

fn assert_reaped(pid: u32) {
    // Once the root exits this descendant is reparented to the host's subreaper.
    // Closing its inherited coordinator connection proves it has exited, but
    // procfs may expose the zombie briefly before that independent reaper gets
    // scheduled (notably on the GitHub-hosted runner). Bound the observation
    // instead of racing a single procfs lookup against reaping.
    let proc_entry = PathBuf::from(format!("/proc/{pid}"));
    for _ in 0..100 {
        if !proc_entry.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !proc_entry.exists(),
        "LiteInst descendant {pid} remains in procfs"
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

#[tokio::test(flavor = "current_thread")]
async fn supervisor_preserves_root_status_and_drains_outliving_child() {
    let (_preload_directory, preload) = compile_noop_preload();
    let marker_directory = tempfile::tempdir().unwrap();
    let marker = marker_directory.path().join("descendant.marker");
    let mut command = guest_command("root-exits-first");
    command.arg(&marker);

    let (status, global) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_preload::<CoordinatorOnlyTool>(command, (), preload),
    )
    .await
    .expect("lifecycle supervisor hung while draining an outliving child")
    .unwrap();

    assert_eq!(status, ExitStatus::Exited(23));
    assert_eq!(std::fs::read(&marker).unwrap(), b"descendant-finished\n");
    assert_eq!(global.fork.load(Ordering::Relaxed), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn supervisor_observes_and_reaps_signaled_descendant() {
    let (_preload_directory, preload) = compile_noop_preload();
    let pid_directory = tempfile::tempdir().unwrap();
    let pid_file = pid_directory.path().join("descendant.pid");
    let mut command = guest_command("signaled-descendant");
    command.arg(&pid_file);

    let (status, global) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_preload::<CoordinatorOnlyTool>(command, (), preload),
    )
    .await
    .expect("lifecycle supervisor hung after descendant signal death")
    .unwrap();

    assert_eq!(status, ExitStatus::Exited(29));
    let pid = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert_reaped(pid);
    assert_eq!(global.fork.load(Ordering::Relaxed), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn supervisor_keeps_patchable_syscalls_in_guest() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (output, global) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload::<CoordinatorOnlyTool>(
            guest_command("fast-path"),
            (),
            preload,
        ),
    )
    .await
    .expect("lifecycle supervisor hung on the in-guest fast path")
    .unwrap();

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"calls=8 traps=1 hooks=8\n", "{output:?}");
    assert_eq!(global.getpid.load(Ordering::Relaxed), 8);
}

#[tokio::test(flavor = "current_thread")]
async fn in_guest_run_reports_typed_instrumentation_stats() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (output, global, stats) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload_and_stats::<CoordinatorOnlyTool>(
            guest_command("fast-path"),
            (),
            preload,
        ),
    )
    .await
    .expect("stats-enabled in-guest run hung")
    .unwrap();

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"calls=8 traps=1 hooks=8\n", "{output:?}");
    assert_eq!(global.getpid.load(Ordering::Relaxed), 8);
    assert_fast_path_stats(&global, &stats);
}

/// A guest command that does not inherit `ADDR_NO_RANDOMIZE` from the test
/// runner (for example one started under `setarch -R`), so only the backend
/// can set it. Caller `pre_exec` hooks run before the backend's.
fn randomized_guest_command(mut command: Command) -> Command {
    // SAFETY: the hook makes only async-signal-safe `personality` calls.
    unsafe {
        command.pre_exec(|| {
            let current = libc::personality(0xffff_ffff);
            if current == -1 {
                return Err(reverie::syscalls::Errno::new(libc::EINVAL));
            }
            let randomized = current as libc::c_ulong & !(libc::ADDR_NO_RANDOMIZE as libc::c_ulong);
            if libc::personality(randomized) == -1 {
                return Err(reverie::syscalls::Errno::new(libc::EINVAL));
            }
            Ok(())
        });
    }
    command
}

/// Both launchers start the guest with address-space randomization disabled,
/// as Reverie's ptrace and DBT launchers do, so its stack, heap and mapping
/// addresses are the same in every run.
#[tokio::test(flavor = "current_thread")]
async fn in_guest_runs_disable_address_space_randomization() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (output, _) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload::<CoordinatorOnlyTool>(
            randomized_guest_command(guest_command("address-randomization")),
            (),
            preload.clone(),
        ),
    )
    .await
    .expect("in-guest run hung")
    .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        output.stdout, b"address randomization=disabled\n",
        "{output:?}"
    );

    let (output, _) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload_data::<CoordinatorOnlyTool>(
            randomized_guest_command(bootstrap_guest_command("address-randomization")),
            (),
            preload,
            b"lifecycle".to_vec(),
        ),
    )
    .await
    .expect("bootstrap run hung")
    .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        output.stdout, b"address randomization=disabled\n",
        "{output:?}"
    );
}

/// A socket the in-guest Tool reserves for its own output is kept from the guest
/// exactly as the coordinator connection is: the guest cannot close, write,
/// shut down, configure, query, truncate, map or reopen it; a guest dup2 or
/// dup3 onto its number succeeds and moves the socket instead; and the Tool's
/// own write reaches it (the fixture checks its peer end receives exactly the
/// Tool's message). With the descriptor table full, a dup onto it still
/// succeeds and the runtime retires the socket; its retirement message reaches
/// the reader even through a full queue and with a forked child holding the
/// socket. Other calls that name it in a register argument (epoll, sendfile's
/// input, timerfd, inotify, ...) fail with EBADF as for a closed number. A
/// regular file is refused.
#[tokio::test(flavor = "current_thread")]
async fn a_reserved_tool_output_fd_is_protected_from_the_guest() {
    let (_preload_directory, preload) = compile_noop_preload();
    let directory = tempfile::tempdir().unwrap();
    let mut command = guest_command("tool-output-fd");
    command.arg(directory.path().join("regular-file"));
    let (result, _) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload::<CoordinatorOnlyTool>(command, (), preload),
    )
    .await
    .expect("in-guest run hung")
    .unwrap();
    assert_eq!(result.status.code(), Some(0), "{result:?}");
    assert_eq!(result.stdout, b"tool output fd: protected\n", "{result:?}");
}

/// A guest `close_range` whose range covers the runtime's own descriptors
/// reaches a Tool that subscribes to it, so the Tool can account for the guest
/// descriptors it closes; the runtime still spares its own descriptors at
/// physical execution, so the guest-visible result is unchanged and the
/// coordinator connection survives.
#[tokio::test(flavor = "current_thread")]
async fn a_close_range_over_runtime_descriptors_reaches_the_tool() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (result, global) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload::<CoordinatorOnlyTool>(
            guest_command("close-range-through-tool"),
            (),
            preload,
        ),
    )
    .await
    .expect("in-guest run hung")
    .unwrap();
    assert_eq!(result.status.code(), Some(0), "{result:?}");
    assert_eq!(
        result.stdout, b"close range through tool: ok\n",
        "{result:?}"
    );
    assert_eq!(global.close_range.load(Ordering::Relaxed), 1);
    assert!(global.getpid.load(Ordering::Relaxed) >= 1);
}

/// Synchronous Tool code inside a callback, outside the callback's own
/// send_rpc, sends a request to the global state through the process's
/// existing coordinator connection with `blocking_global_rpc`, and a request
/// for another global state type is refused without sending.
#[tokio::test(flavor = "current_thread")]
async fn synchronous_tool_code_inside_a_callback_reaches_the_global_state() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (result, global) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload::<CoordinatorOnlyTool>(
            guest_command("blocking-global-rpc"),
            (),
            preload,
        ),
    )
    .await
    .expect("in-guest run hung")
    .unwrap();
    assert_eq!(result.status.code(), Some(0), "{result:?}");
    assert_eq!(result.stdout, b"blocking global rpc: ok\n", "{result:?}");
    assert_eq!(global.blocking.load(Ordering::Relaxed), 1);
}

/// Asserts the statistics of one `fast-path` guest run.
fn assert_fast_path_stats(
    global: &LifecycleGlobal,
    stats: &reverie_liteinst::LiteinstBackendStatsSource,
) {
    assert_eq!(stats.snapshot().process_reports(), 1, "{stats}");
    assert!(stats.distinct_rips() >= 1, "{stats}");
    assert!(stats.patch_candidates() >= 1, "{stats}");
    let paths = stats.dispatch_path_counts();
    assert!(
        paths.count(&LiteinstDispatchPath::InGuestSigsys) >= 1,
        "expected an in-guest SIGSYS: {stats}"
    );
    assert!(
        paths.count(&LiteinstDispatchPath::DirectHook) >= 8,
        "expected installed-hook dispatches: {stats}"
    );
    let record = stats
        .snapshot()
        .dispatch_stats()
        .expect("in-guest LiteInst reports a dispatch record");
    assert_eq!(record.inconsistencies(), Vec::<String>::new(), "{record}");
    assert_eq!(record.counters.ptrace_seccomp_stops, Some(0), "{record}");
    assert_eq!(record.counters.ptrace_sigtrap_stops, Some(0), "{record}");
    assert_eq!(
        record.counters.patched_direct_calls,
        Some(paths.count(&LiteinstDispatchPath::DirectHook)),
        "{record}"
    );
    // A site is patched only after its first call trapped, so there is at
    // least one SIGSYS per patched site.
    assert!(record.sites.patched >= Some(1), "{record}");
    assert!(
        record.counters.signal_traps >= record.sites.patched,
        "{record}"
    );
    assert_physical_signals_agree(stats, &record);
    // The getpid site's first call trapped once and then entered through the
    // hook it installed; every call after that was a hook entry.
    assert!(
        record.counters.patched_direct_calls >= Some(8),
        "{record}\n{stats}"
    );
    assert_tool_callbacks_were_delivered(global, stats);
}

/// The handler's count of every `SIGSYS` it received equals the dispatcher's
/// classified signals, counted at a different point, plus each fallback's
/// completion signal, less the entries a fork child re-attributed to itself.
fn assert_physical_signals_agree(
    stats: &reverie_liteinst::LiteinstBackendStatsSource,
    record: &reverie::DispatchStats,
) {
    let paths = stats.dispatch_path_counts();
    let classified = paths.count(&LiteinstDispatchPath::InGuestSigsys)
        + paths.count(&LiteinstDispatchPath::InGuestNestedSigsys)
        + paths.count(&LiteinstDispatchPath::FallbackCompletionSigsys)
        - stats.snapshot().fork_child_entries().sigsys;
    assert_eq!(
        record.counters.signal_traps,
        Some(classified),
        "{record}\n{stats}"
    );
}

/// Every Tool callback the guest counted was entered by a guest event: a
/// top-level `SIGSYS` or a patched-site hook entry the Tool's own syscalls did
/// not make. The guest counts one callback per entry, so a callback re-run for
/// an `ERESTARTSYS` restart is not counted twice. The Tool's own syscalls are
/// excluded, so its RPC traffic cannot pad the count. It is still a bound, not
/// an equality: a lazily patched site's first call counts
/// a signal and then a hook entry for one callback, and a signal for an
/// unsubscribed syscall makes no callback.
///
/// Both sides are raw per-process sums. A fork child counts the callback it
/// returns from as its own, and so does its re-attributed first event.
fn assert_tool_callbacks_were_delivered(
    global: &LifecycleGlobal,
    stats: &reverie_liteinst::LiteinstBackendStatsSource,
) {
    let callbacks = global.callbacks.load(Ordering::Relaxed);
    assert!(callbacks > 0, "the guest Tool reported no callbacks");
    let paths = stats.dispatch_path_counts();
    let guest_entries = paths.count(&LiteinstDispatchPath::InGuestSigsys)
        + paths.count(&LiteinstDispatchPath::DirectHook)
        - paths.count(&LiteinstDispatchPath::InGuestNestedHook);
    assert!(
        callbacks <= guest_entries,
        "{callbacks} callbacks, {guest_entries} guest entries\n{stats}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn in_guest_tool_refuses_an_unsupported_site_patching_value() {
    let (_preload_directory, preload) = compile_noop_preload();
    let mut command = guest_command("patching-off");
    command.env(SITE_PATCHING_ENV, "2");
    let (output, global) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload::<CoordinatorOnlyTool>(command, (), preload),
    )
    .await
    .expect("in-guest run with an unsupported site-patching value hung")
    .unwrap();

    // The fixture unwraps the installation result, so the refusal is a panic
    // before the fixture's workload: no output and no getpid RPC.
    assert_eq!(output.status.code(), Some(101), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("unsupported REVERIE_LITEINST_SITE_PATCHING value"),
        "{output:?}"
    );
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(global.getpid.load(Ordering::Relaxed), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn bootstrap_run_with_output_reports_typed_instrumentation_stats() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (output, global, stats) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload_data_and_stats::<CoordinatorOnlyTool>(
            bootstrap_guest_command("fast-path"),
            (),
            preload,
            b"lifecycle".to_vec(),
        ),
    )
    .await
    .expect("stats-enabled bootstrap run hung")
    .unwrap();

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"calls=8 traps=1 hooks=8\n", "{output:?}");
    assert_eq!(global.getpid.load(Ordering::Relaxed), 8);
    assert_fast_path_stats(&global, &stats);
}

#[tokio::test(flavor = "current_thread")]
async fn bootstrap_run_with_inherited_stdio_reports_typed_instrumentation_stats() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (output, global, stats) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_inherited_stdio_and_preload_data_and_stats::<CoordinatorOnlyTool>(
            bootstrap_guest_command("fast-path"),
            (),
            preload,
            b"lifecycle".to_vec(),
        ),
    )
    .await
    .expect("stats-enabled inherited-stdio bootstrap run hung")
    .unwrap();

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    // The guest wrote to the inherited stdout, not to a captured buffer.
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(global.getpid.load(Ordering::Relaxed), 8);
    assert_fast_path_stats(&global, &stats);
}

/// The status-only bootstrap launchers keep the caller's stdio: a guest whose
/// stdout the caller pointed at a file writes there.
#[tokio::test(flavor = "current_thread")]
async fn bootstrap_status_run_keeps_caller_stdio_and_reports_stats() {
    let (_preload_directory, preload) = compile_noop_preload();
    let stdout_file = tempfile::NamedTempFile::new().unwrap();
    let mut command = bootstrap_guest_command("fast-path");
    command.stdout(reverie::process::Stdio::from(stdout_file.reopen().unwrap()));
    let (status, global, stats) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_preload_data_and_stats::<CoordinatorOnlyTool>(
            command,
            (),
            preload.clone(),
            b"lifecycle".to_vec(),
        ),
    )
    .await
    .expect("stats-enabled bootstrap status run hung")
    .unwrap();

    assert_eq!(status, ExitStatus::Exited(0));
    assert_eq!(
        std::fs::read(stdout_file.path()).unwrap(),
        b"calls=8 traps=1 hooks=8\n"
    );
    assert_eq!(global.getpid.load(Ordering::Relaxed), 8);
    assert_fast_path_stats(&global, &stats);

    let stdout_file = tempfile::NamedTempFile::new().unwrap();
    let mut command = bootstrap_guest_command("fast-path");
    command.stdout(reverie::process::Stdio::from(stdout_file.reopen().unwrap()));
    let (status, global) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_preload_data::<CoordinatorOnlyTool>(
            command,
            (),
            preload,
            b"lifecycle".to_vec(),
        ),
    )
    .await
    .expect("bootstrap status run hung")
    .unwrap();

    assert_eq!(status, ExitStatus::Exited(0));
    assert_eq!(
        std::fs::read(stdout_file.path()).unwrap(),
        b"calls=8 traps=1 hooks=8\n"
    );
    assert_eq!(global.getpid.load(Ordering::Relaxed), 8);
}

#[tokio::test(flavor = "current_thread")]
async fn site_patching_off_runs_every_call_through_the_in_guest_fallback() {
    let (_preload_directory, preload) = compile_noop_preload();
    let mut command = guest_command("patching-off");
    command.env(SITE_PATCHING_ENV, "0");
    let (output, global, stats) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload_and_stats::<CoordinatorOnlyTool>(
            command,
            (),
            preload,
        ),
    )
    .await
    .expect("patching-off in-guest run hung")
    .unwrap();

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    // No site is claimed, so the site keeps no trap or hook count; every
    // getpid and every vDSO clock_gettime and gettimeofday reached the
    // fallback, and no tracer is attached to the guest.
    assert_eq!(
        output.stdout,
        b"calls=8 traps=0 hooks=0 fallback_getpid=8 fallback_clock_gettime=8 \
          fallback_gettimeofday=8 tracer_pid=0\n",
        "{output:?}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(global.getpid.load(Ordering::Relaxed), 8);
    assert_eq!(global.clock_gettime.load(Ordering::Relaxed), 8);
    assert_eq!(global.gettimeofday.load(Ordering::Relaxed), 8);
    assert_eq!(stats.snapshot().process_reports(), 1, "{stats}");
    assert_eq!(stats.snapshot().patch_shapes().patched_rips(), 0, "{stats}");
    let paths = stats.dispatch_path_counts();
    for (path, expected) in [
        (LiteinstDispatchPath::DirectHook, 0),
        (LiteinstDispatchPath::FirstSiteSeccomp, 0),
        (LiteinstDispatchPath::PtraceInstallation, 0),
        (LiteinstDispatchPath::CachelineStraddlerFallback, 0),
        (LiteinstDispatchPath::UnpatchableOrOtherFallback, 0),
        (LiteinstDispatchPath::FallbackRefusal, 0),
    ] {
        assert_eq!(paths.count(&path), expected, "{path}: {stats}");
    }
    let disabled = paths.count(&LiteinstDispatchPath::PatchingDisabledFallback);
    assert!(
        disabled >= 24,
        "expected every getpid, clock_gettime and gettimeofday on the fallback: {stats}"
    );
    // Every fallback completes except the guest's final exit_group: it is
    // dispatched through the fallback like any other call, but the Tool host
    // submits the process statistics before that call exits the process, so
    // its completion is never counted.
    assert_eq!(
        paths.count(&LiteinstDispatchPath::FallbackCompletionSigsys),
        disabled - 1,
        "every fallback but the final exit_group must complete: {stats}"
    );
    let record = stats
        .snapshot()
        .dispatch_stats()
        .expect("in-guest LiteInst reports a dispatch record");
    assert_eq!(record.inconsistencies(), Vec::<String>::new(), "{record}");
    assert_eq!(record.counters.ptrace_seccomp_stops, Some(0), "{record}");
    assert_eq!(record.counters.ptrace_sigtrap_stops, Some(0), "{record}");
    assert_eq!(record.counters.patched_direct_calls, Some(0), "{record}");
    assert_physical_signals_agree(&stats, &record);
    assert_tool_callbacks_were_delivered(&global, &stats);
    println!("{stats}");
}

#[tokio::test(flavor = "current_thread")]
async fn in_guest_timer_requests_fail_closed_with_enosys() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (output, global) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload::<CoordinatorOnlyTool>(
            guest_command("timer-refused"),
            (),
            preload,
        ),
    )
    .await
    .expect("timer-refused in-guest run hung")
    .unwrap();

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    // Nothing in the guest delivers a timer event, so both requests must be
    // refused rather than accepted and never fired.
    assert_eq!(
        output.stdout, b"set_timer=ENOSYS set_timer_precise=ENOSYS\n",
        "{output:?}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(global.getpid.load(Ordering::Relaxed), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn fallback_fork_reports_both_process_dispatch_paths() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (output, global, stats) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload_and_stats::<CoordinatorOnlyTool>(
            guest_command("fallback-fork-stats"),
            (),
            preload,
        ),
    )
    .await
    .expect("fallback fork statistics run hung")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"fallback fork stats: child=finished\n");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(global.fork.load(Ordering::Relaxed), 1);
    assert_eq!(stats.snapshot().process_reports(), 2, "{stats}");
    assert_eq!(
        stats
            .dispatch_path_counts()
            .count(&LiteinstDispatchPath::UnpatchableOrOtherFallback),
        2,
        "{stats}"
    );
    assert_eq!(
        stats
            .dispatch_path_counts()
            .count(&LiteinstDispatchPath::CachelineStraddlerFallback),
        0,
        "{stats}"
    );
    assert_eq!(
        stats
            .dispatch_path_counts()
            .count(&LiteinstDispatchPath::FallbackRefusal),
        0,
        "{stats}"
    );
    assert!(
        stats
            .dispatch_path_counts()
            .count(&LiteinstDispatchPath::InGuestSigsys)
            >= 2,
        "{stats}"
    );
    // The child reports the parent's fork entry as its own first event; the
    // shared record still counts that physical signal once.
    assert_eq!(
        stats.snapshot().fork_child_entries(),
        reverie_liteinst::InheritedEntries {
            sigsys: 1,
            hooks: 0
        },
        "{stats}"
    );
    let record = stats
        .snapshot()
        .dispatch_stats()
        .expect("in-guest LiteInst reports a dispatch record");
    assert_eq!(record.inconsistencies(), Vec::<String>::new(), "{record}");
    assert_physical_signals_agree(&stats, &record);
    assert_tool_callbacks_were_delivered(&global, &stats);
    println!("{stats}");
}

#[tokio::test(flavor = "current_thread")]
async fn tool_syscall_through_a_patched_site_is_a_nested_hook_entry() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (output, global, stats) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload_and_stats::<CoordinatorOnlyTool>(
            guest_command("nested-hook"),
            (),
            preload,
        ),
    )
    .await
    .expect("nested hook statistics run hung")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    // The guest's own counts of the getppid site: its own call trapped and
    // patched the site, then the Tool's call entered through the hook.
    assert_eq!(
        output.stdout, b"nested getppid traps=1 hooks=2\n",
        "{output:?}"
    );
    assert_eq!(global.getpid.load(Ordering::Relaxed), 1);
    let paths = stats.dispatch_path_counts();
    assert_eq!(
        paths.count(&LiteinstDispatchPath::InGuestNestedHook),
        1,
        "{stats}"
    );
    let record = stats
        .snapshot()
        .dispatch_stats()
        .expect("in-guest LiteInst reports a dispatch record");
    assert_eq!(record.inconsistencies(), Vec::<String>::new(), "{record}");
    // The nested entry is a physical patched call like any other.
    assert_eq!(
        record.counters.patched_direct_calls,
        Some(paths.count(&LiteinstDispatchPath::DirectHook)),
        "{record}"
    );
    assert_physical_signals_agree(&stats, &record);
    assert_tool_callbacks_were_delivered(&global, &stats);
}

#[tokio::test(flavor = "current_thread")]
async fn hooked_fork_counts_the_inherited_hook_entry_once() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (output, global, stats) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload_and_stats::<CoordinatorOnlyTool>(
            guest_command("hooked-fork-stats"),
            (),
            preload,
        ),
    )
    .await
    .expect("hooked fork statistics run hung")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"hooked fork stats: child=finished\n");
    assert_eq!(global.fork.load(Ordering::Relaxed), 1);
    assert_eq!(stats.snapshot().process_reports(), 2, "{stats}");
    // The fork entered through the hook its first call installed; the child
    // reports that entry again as its own first event.
    assert_eq!(
        stats.snapshot().fork_child_entries(),
        reverie_liteinst::InheritedEntries {
            sigsys: 0,
            hooks: 1
        },
        "{stats}"
    );
    let paths = stats.dispatch_path_counts();
    let record = stats
        .snapshot()
        .dispatch_stats()
        .expect("in-guest LiteInst reports a dispatch record");
    assert_eq!(record.inconsistencies(), Vec::<String>::new(), "{record}");
    assert_eq!(
        record.counters.patched_direct_calls,
        Some(paths.count(&LiteinstDispatchPath::DirectHook) - 1),
        "{record}\n{stats}"
    );
    assert_physical_signals_agree(&stats, &record);
    assert_tool_callbacks_were_delivered(&global, &stats);
}

#[tokio::test(flavor = "current_thread")]
async fn backend_trait_output_capture_reports_bytes_status_and_stats() {
    if std::env::var_os(BACKEND_OUTPUT_CHILD_ENV).is_none() {
        let (_preload_directory, preload) = compile_noop_preload();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "backend_trait_output_capture_reports_bytes_status_and_stats",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(BACKEND_OUTPUT_CHILD_ENV, "1")
            .env(TOOL_PRELOAD_ENV, preload)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "isolated Backend::run_with_output test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        return;
    }

    let (output, global, stats) = tokio::time::timeout(
        Duration::from_secs(10),
        <LiteinstBackend as Backend>::run_with_output::<CoordinatorOnlyTool>(
            guest_command("fast-path"),
            (),
        ),
    )
    .await
    .expect("Backend::run_with_output hung on the LiteInst path")
    .unwrap();

    assert_eq!(output.status, ExitStatus::Exited(0), "{output:?}");
    assert_eq!(output.stdout, b"calls=8 traps=1 hooks=8\n", "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(global.getpid.load(Ordering::Relaxed), 8);
    assert_eq!(stats.process_reports(), 1, "{stats}");
    assert!(stats.patch_shapes().patched_rips() >= 1, "{stats}");
    assert!(stats.patch_shapes().candidate_rips() >= 1, "{stats}");
}

#[tokio::test(flavor = "current_thread")]
async fn supervisor_restarts_wait4_without_leaking_private_errno() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (output, _) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload::<CoordinatorOnlyTool>(
            guest_command("restart-wait4"),
            (),
            preload,
        ),
    )
    .await
    .expect("supervised wait4 hung")
    .unwrap();

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"wait4-restart-ok\n", "{output:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn supervisor_restarts_read_without_leaking_private_errno() {
    let (_preload_directory, preload) = compile_noop_preload();
    let (output, _) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::run_with_output_and_preload::<CoordinatorOnlyTool>(
            guest_command("restart-read"),
            (),
            preload,
        ),
    )
    .await
    .expect("supervised read hung")
    .unwrap();

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"read-result=4243 calls=3\n", "{output:?}");
}

/// Admits every guest connection and records the pid each pidfd names, and
/// whether the launcher attached an exit reporter before the first admission.
/// At the first admission, mid-run, it uses each of the reporter's methods
/// once.
#[derive(Default)]
struct RecordingAdmission {
    seen: std::sync::Mutex<Vec<i32>>,
    reporter_before_first_admission: std::sync::atomic::AtomicBool,
    reporter: std::sync::Mutex<Option<std::sync::Arc<dyn reverie_rpc_transport::ExitReporter>>>,
    pending_at_first_admission: std::sync::Mutex<Option<Vec<i32>>>,
}

impl reverie_rpc_transport::ConnectionAdmission for RecordingAdmission {
    fn attach_exit_reporter(
        &self,
        reporter: std::sync::Arc<dyn reverie_rpc_transport::ExitReporter>,
    ) {
        if self.seen.lock().unwrap().is_empty() {
            self.reporter_before_first_admission
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        *self.reporter.lock().unwrap() = Some(reporter);
    }

    fn admit(
        &self,
        peer: std::os::fd::OwnedFd,
    ) -> std::io::Result<reverie_rpc_transport::Admitted> {
        use std::os::fd::AsRawFd;
        let fdinfo =
            std::fs::read_to_string(format!("/proc/self/fdinfo/{}", peer.as_raw_fd())).unwrap();
        let pid: i32 = fdinfo
            .lines()
            .find_map(|line| line.strip_prefix("Pid:"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut seen = self.seen.lock().unwrap();
        if seen.is_empty()
            && let Some(reporter) = self.reporter.lock().unwrap().as_ref()
        {
            *self.pending_at_first_admission.lock().unwrap() =
                Some(reporter.pending_process_exits());
            reporter.process_exited(pid);
            reporter.backend_failed(reverie::BackendFailure {
                pid: reverie::Pid::from_raw(pid),
                tid: reverie::Tid::from_raw(pid),
                phase: "lifecycle test",
            });
        }
        seen.push(pid);
        Ok(reverie_rpc_transport::Admitted { process_id: pid })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn connection_admission_sees_each_guest_process_before_it_is_served() {
    let (_preload_directory, preload) = compile_noop_preload();
    let marker_directory = tempfile::tempdir().unwrap();
    let marker = marker_directory.path().join("descendant.marker");
    let mut command = guest_command("root-exits-first");
    command.arg(&marker);
    let admission = std::sync::Arc::new(RecordingAdmission::default());

    let (status, global) = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::with_connection_admission(
            admission.clone(),
            LiteinstBackend::run_with_preload::<CoordinatorOnlyTool>(command, (), preload),
        ),
    )
    .await
    .expect("an admitted run hung")
    .unwrap();

    // Admission did not change the run, and every admitted connection was a
    // guest process, never the coordinator itself, each admitted once.
    assert_eq!(status, ExitStatus::Exited(23));
    assert_eq!(std::fs::read(&marker).unwrap(), b"descendant-finished\n");
    assert_eq!(global.fork.load(Ordering::Relaxed), 1);
    let seen = admission.seen.lock().unwrap().clone();
    assert!(!seen.is_empty(), "no guest connection was admitted");
    let me = std::process::id() as i32;
    assert!(seen.iter().all(|&pid| pid > 0 && pid != me), "{seen:?}");
    let mut unique = seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seen.len(),
        "a process was admitted twice: {seen:?}"
    );
    assert!(
        admission
            .reporter_before_first_admission
            .load(std::sync::atomic::Ordering::SeqCst),
        "the launcher must attach the exit reporter before admitting a connection"
    );
    // During the run each reporter method reached the global tool's hook.
    assert_eq!(
        *admission.pending_at_first_admission.lock().unwrap(),
        Some(vec![PENDING_EXIT_SENTINEL])
    );
    assert_eq!(*global.exited.lock().unwrap(), [seen[0]]);
    assert_eq!(global.backend_failures.load(Ordering::Relaxed), 1);
    // After the run the reporter holds no global, so it reaches nothing.
    let reporter = admission.reporter.lock().unwrap().clone().unwrap();
    assert!(reporter.pending_process_exits().is_empty());
    reporter.process_exited(seen[0]);
}

struct RefusingAdmission;

impl reverie_rpc_transport::ConnectionAdmission for RefusingAdmission {
    fn admit(
        &self,
        _peer: std::os::fd::OwnedFd,
    ) -> std::io::Result<reverie_rpc_transport::Admitted> {
        Err(std::io::Error::other("refused by the test"))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn refused_guest_connection_fails_the_run() {
    let (_preload_directory, preload) = compile_noop_preload();
    let marker_directory = tempfile::tempdir().unwrap();
    let marker = marker_directory.path().join("descendant.marker");
    let mut command = guest_command("root-exits-first");
    command.arg(&marker);

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        LiteinstBackend::with_connection_admission(
            std::sync::Arc::new(RefusingAdmission),
            LiteinstBackend::run_with_preload::<CoordinatorOnlyTool>(command, (), preload),
        ),
    )
    .await
    .expect("a refused run hung");

    // The guest never reaches its program: the run fails or the guest exits
    // with something other than the fixture's own status.
    match result {
        Err(_) => {}
        Ok((status, _global)) => assert_ne!(status, ExitStatus::Exited(23)),
    }
    assert!(!marker.exists(), "the refused guest ran its program");
}
