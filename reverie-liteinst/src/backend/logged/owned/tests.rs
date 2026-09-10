use std::os::unix::process::ExitStatusExt;
use std::sync::mpsc;

use reverie_rpc_transport::guest_log::Options;
use reverie_rpc_transport::guest_log::retained_log;

use super::*;

#[test]
fn death_before_pidfd_transfer_reaps_child_and_releases_owner() {
    const SELECTOR: &str = "REVERIE_PRETRANSFER_DEATH";
    if std::env::var_os(SELECTOR).is_none() {
        for operation in [
            "exit",
            "signal",
            "capture",
            "capture-panic",
            "capture-error-panic",
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "backend::logged::owned::tests::death_before_pidfd_transfer_reaps_child_and_releases_owner",
                    "--exact",
                    "--test-threads=1",
                ])
                .env(SELECTOR, operation)
                .output()
                .unwrap();
            assert!(output.status.success(), "{operation}: {output:?}");
        }
        return;
    }
    unsafe { libc::alarm(8) };
    struct Owner(mpsc::Sender<()>);
    impl Drop for Owner {
        fn drop(&mut self) {
            self.0.send(()).unwrap();
        }
    }
    struct SpawnOnDrop {
        spawned: mpsc::Sender<std::process::Child>,
        panic: bool,
    }
    impl Drop for SpawnOnDrop {
        fn drop(&mut self) {
            let mut status = 0;
            unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG | libc::__WNOTHREAD) };
            self.spawned
                .send(std::process::Command::new("/bin/true").spawn().unwrap())
                .unwrap();
            assert!(!self.panic, "command capture destructor panic");
        }
    }
    let operation = std::env::var(SELECTOR).unwrap();
    let signal = operation == "signal";
    let spawn_error = operation == "capture-error-panic";
    let (spawned, created) = mpsc::channel();
    let capture = operation.starts_with("capture").then(|| SpawnOnDrop {
        spawned,
        panic: operation.ends_with("panic"),
    });
    let mut command = std::process::Command::new("/bin/true");
    unsafe {
        command.pre_exec(move || {
            let _ = &capture;
            if spawn_error {
                return Err(io::Error::from_raw_os_error(libc::EACCES));
            }
            if signal {
                libc::raise(libc::SIGKILL);
            }
            libc::_exit(23)
        });
    }
    let (released, dropped) = mpsc::channel();
    let (_sink, handle) = retained_log(Options::bounded(1024));
    let mut launch = Lifetime {
        owner: Some(Owner(released)),
        check: None,
        pending: false,
        handle: handle.clone(),
    };
    let observer = RunObserver::new(StdioMode::Inherited);
    let mut unrelated = std::process::Command::new("/bin/true").spawn().unwrap();
    let result = child::Child::spawn(command, &mut launch, observer, handle, child::Kernel);
    assert!(result.is_err());
    if spawn_error {
        assert_eq!(
            result.as_ref().err().unwrap().raw_os_error(),
            Some(libc::EACCES)
        );
    }
    drop(result);
    if operation.starts_with("capture") {
        let mut created = created.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(created.wait().unwrap().success());
    }
    assert!(unrelated.wait().unwrap().success());
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) }, -1);
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    drop(launch);
    dropped.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn successful_exec_capture_panic_preserves_actual_child_evidence() {
    const SELECTOR: &str = "REVERIE_EXEC_CAPTURE_PANIC";
    if std::env::var_os(SELECTOR).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "backend::logged::owned::tests::successful_exec_capture_panic_preserves_actual_child_evidence",
                "--exact",
                "--test-threads=1",
            ])
            .env(SELECTOR, "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        return;
    }
    unsafe { libc::alarm(8) };
    struct PanicAfterExit;
    impl Drop for PanicAfterExit {
        fn drop(&mut self) {
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            assert_eq!(
                unsafe {
                    libc::waitid(
                        libc::P_ALL,
                        0,
                        &mut info,
                        libc::WEXITED | libc::WNOWAIT | libc::__WNOTHREAD,
                    )
                },
                0
            );
            panic!("capture destruction after successful exec");
        }
    }
    let capture = PanicAfterExit;
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "exit 23"]);
    unsafe {
        command.pre_exec(move || {
            let _ = &capture;
            Ok(())
        });
    }
    let (owner, slot, dropped) = tracked();
    let (_sink, handle) = retained_log(Options::bounded(1024));
    let mut launch = Lifetime {
        owner: Some(owner),
        check: None,
        pending: false,
        handle: handle.clone(),
    };
    let observer = RunObserver::new(StdioMode::Inherited);
    *slot.lock().unwrap() = Some(observer.clone());
    let result = child::Child::spawn(
        command,
        &mut launch,
        observer.clone(),
        handle.clone(),
        child::Kernel,
    );
    assert!(result.is_err());
    drop(result);
    let state = snapshot(&observer);
    assert!(state.spawned);
    assert!(state.pid.is_some());
    assert!(state.reaped);
    assert_eq!(state.wait_status.unwrap().code(), Some(23));
    assert!(handle.snapshot().root_reaped);
    drop(launch);
    assert_eq!(
        dropped.recv_timeout(Duration::from_secs(1)).unwrap(),
        (true, true)
    );
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) }, -1);
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

fn address_space_state() -> (libc::c_int, libc::rlim_t, libc::rlim_t) {
    let personality = unsafe { libc::personality(0xffff_ffff) };
    assert_ne!(personality, -1);
    let mut stack = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut stack) },
        0
    );
    (personality, stack.rlim_cur, stack.rlim_max)
}

#[test]
fn owned_address_space_preserves_child_spec_parent_and_limits() {
    const CHILD: &str = "LITEINST_OWNED_ADDRESS_SPACE_CHILD";
    const DIRECTORY: &str = "LITEINST_OWNED_ADDRESS_SPACE_DIRECTORY";
    if let Ok(expected) = std::env::var(CHILD) {
        let actual = address_space_state();
        let directory = std::path::PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
        std::fs::write(directory.join("observed"), format!("{actual:?}\n")).unwrap();
        assert_eq!(format!("{actual:?}"), expected);
        assert_eq!(std::env::current_dir().unwrap(), directory);
        assert_eq!(
            std::env::args().next().unwrap(),
            "owned-address-space-child"
        );
        assert_eq!(std::env::var("OWNED_VALUE").unwrap(), "two words");
        return;
    }
    let parent = address_space_state();
    for already_set in [false, true] {
        for mode in [StdioMode::Captured, StdioMode::Inherited] {
            let directory = tempfile::tempdir().unwrap();
            let initial = (parent.0 & !libc::ADDR_NO_RANDOMIZE)
                | libc::ADDR_COMPAT_LAYOUT
                | if already_set {
                    libc::ADDR_NO_RANDOMIZE
                } else {
                    0
                };
            let stack = libc::rlimit {
                rlim_cur: parent.1.min(4 * 1024 * 1024),
                rlim_max: parent.2,
            };
            let expected = (
                initial | libc::ADDR_NO_RANDOMIZE,
                stack.rlim_cur,
                stack.rlim_max,
            );
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.env_clear().env(CHILD, format!("{expected:?}"))
                .env(DIRECTORY, directory.path()).env("OWNED_VALUE", "two words")
                .current_dir(directory.path()).arg0("owned-address-space-child")
                .args(["--exact", "backend::logged::owned::tests::owned_address_space_preserves_child_spec_parent_and_limits", "--test-threads=1"]);
            unsafe {
                command.pre_exec(move || {
                    if libc::personality(initial as libc::c_ulong) == -1
                        || libc::setrlimit(libc::RLIMIT_STACK, &stack) == -1
                    {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let (owner, slot, dropped) = tracked();
            let cleanup_slot = slot.clone();
            let (called, cleaned) = mpsc::channel();
            let prepared = PreparedCommand::new(command, owner).with_cleanup(move |_| {
                let observer = cleanup_slot.lock().unwrap().as_ref().unwrap().clone();
                called.send(snapshot(&observer).reaped).unwrap();
                Ok(())
            });
            let (sink, handle) = retained_log(Options::bounded(2048));
            let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<
                (),
                _,
            >(prepared, (), Vec::new(), sink, mode);
            *slot.lock().unwrap() = Some(observer.clone());
            let error = runtime().block_on(future).unwrap_err();
            let observed = std::fs::read_to_string(directory.path().join("observed")).unwrap();
            assert!(cleaned.recv_timeout(Duration::from_secs(2)).unwrap());
            assert!(cleaned.try_recv().is_err());
            assert_eq!(
                dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
                (true, true)
            );
            assert_eq!(address_space_state(), parent);
            assert!(handle.snapshot().root_reaped);
            assert!(!handle.snapshot().qualifies());
            assert_eq!(
                snapshot(&observer).wait_status.unwrap().code(),
                Some(0),
                "already_set={already_set} mode={mode:?} observed={observed} error={error:?}"
            );
            assert_eq!(observed, format!("{expected:?}\n"));
        }
    }
}

#[test]
fn owned_address_space_precedes_late_spawn_hooks() {
    let parent = address_space_state();
    for mode in [StdioMode::Captured, StdioMode::Inherited] {
        let initial = (parent.0 & !libc::ADDR_NO_RANDOMIZE) | libc::ADDR_COMPAT_LAYOUT;
        let mut command = std::process::Command::new("/bin/true");
        unsafe {
            command.pre_exec(move || {
                if libc::personality(initial as libc::c_ulong) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let (owner, slot, dropped) = tracked();
        let prepared = PreparedCommand::new(command, owner).with_spawn_check(move |_, command| {
            unsafe {
                command.pre_exec(move || {
                    let current = libc::personality(0xffff_ffff);
                    if current == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    if current != initial | libc::ADDR_NO_RANDOMIZE {
                        return Err(io::Error::from_raw_os_error(libc::EUCLEAN));
                    }
                    Ok(())
                });
            }
            Ok(())
        });
        let (sink, _) = retained_log(Options::bounded(2048));
        let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<
            (),
            _,
        >(prepared, (), Vec::new(), sink, mode);
        *slot.lock().unwrap() = Some(observer.clone());
        let error = runtime().block_on(future).unwrap_err();
        assert_eq!(
            dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
            (true, true),
            "{error:?}"
        );
        assert_eq!(snapshot(&observer).wait_status.unwrap().code(), Some(0));
        assert_eq!(address_space_state(), parent);
    }
}

#[test]
fn owned_address_space_query_and_set_errors_preserve_errno_and_cleanup() {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let parent = address_space_state();
    for deny_query in [true, false] {
        for mode in [StdioMode::Captured, StdioMode::Inherited] {
            let expected_errno = if deny_query {
                libc::EACCES
            } else {
                libc::EPERM
            };
            let (mut receiver, sender) = std::os::unix::net::UnixStream::pair().unwrap();
            let mut command = std::process::Command::new("/bin/true");
            unsafe {
                command.pre_exec(move || {
                    let pid = libc::getpid().to_ne_bytes();
                    if libc::write(sender.as_raw_fd(), pid.as_ptr().cast(), pid.len())
                        != pid.len() as isize
                    {
                        return Err(io::Error::last_os_error());
                    }
                    let mut filter = [
                        libc::sock_filter {
                            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                            jt: 0,
                            jf: 0,
                            k: 0,
                        },
                        libc::sock_filter {
                            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                            jt: 0,
                            jf: 3,
                            k: libc::SYS_personality as u32,
                        },
                        libc::sock_filter {
                            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                            jt: 0,
                            jf: 0,
                            k: 16,
                        },
                        libc::sock_filter {
                            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                            jt: u8::from(!deny_query),
                            jf: u8::from(deny_query),
                            k: 0xffff_ffff,
                        },
                        libc::sock_filter {
                            code: (libc::BPF_RET | libc::BPF_K) as u16,
                            jt: 0,
                            jf: 0,
                            k: libc::SECCOMP_RET_ERRNO | expected_errno as u32,
                        },
                        libc::sock_filter {
                            code: (libc::BPF_RET | libc::BPF_K) as u16,
                            jt: 0,
                            jf: 0,
                            k: libc::SECCOMP_RET_ALLOW,
                        },
                    ];
                    let program = libc::sock_fprog {
                        len: filter.len() as u16,
                        filter: filter.as_mut_ptr(),
                    };
                    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == -1
                        || libc::prctl(libc::PR_SET_SECCOMP, 2, &program) == -1
                    {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let (owner, slot, dropped) = tracked();
            let cleanup_slot = slot.clone();
            let (called, cleaned) = mpsc::channel();
            let prepared = PreparedCommand::new(command, owner)
                .with_spawn_check(|_, command| {
                    unsafe {
                        command.pre_exec(|| Err(io::Error::from_raw_os_error(libc::EUCLEAN)));
                    }
                    Ok(())
                })
                .with_cleanup(move |_| {
                    let observer = cleanup_slot.lock().unwrap().as_ref().unwrap().clone();
                    called.send(snapshot(&observer).spawned).unwrap();
                    Ok(())
                });
            let (sink, _) = retained_log(Options::bounded(2048));
            let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<
                (),
                _,
            >(prepared, (), Vec::new(), sink, mode);
            *slot.lock().unwrap() = Some(observer.clone());
            let error = runtime().block_on(future).unwrap_err();
            match error.cause {
                Error::Io(error) => assert_eq!(error.raw_os_error(), Some(expected_errno)),
                other => panic!("expected original personality errno: {other:?}"),
            }
            assert!(!cleaned.recv_timeout(Duration::from_secs(2)).unwrap());
            assert!(cleaned.try_recv().is_err());
            assert_eq!(
                dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
                (false, false)
            );
            receiver
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = [0; std::mem::size_of::<libc::pid_t>()];
            receiver.read_exact(&mut bytes).unwrap();
            let pid = libc::pid_t::from_ne_bytes(bytes);
            assert!(pid > 0);
            assert_eq!(
                unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
                -1
            );
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
            assert_eq!(address_space_state(), parent);
        }
    }
}

#[test]
fn callback_and_destructor_cleanup_failures_preserve_primary_separately() {
    let (sink, handle) = retained_log(Options::bounded(2048));
    let prepared = PreparedCommand::new(std::process::Command::new("/bin/true"), PanickingOwner)
        .with_spawn_check(|_, _| Err(io::Error::other("primary before spawn").into()))
        .with_cleanup(|_| Err(io::Error::other("callback cleanup failure").into()));
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        prepared,
        (),
        Vec::new(),
        sink,
        StdioMode::Captured,
    );
    let error = runtime().block_on(future).unwrap_err();
    assert!(error.to_string().contains("primary before spawn"));
    let evidence = snapshot(&observer);
    assert!(!evidence.spawned);
    let issue = evidence.cleanup_issue.unwrap();
    assert!(issue.message.contains("callback cleanup failure"));
    assert!(issue.message.contains("cleanup unwound"));
    assert!(
        handle
            .snapshot()
            .issues
            .iter()
            .filter(|issue| issue.kind == IssueKind::Cleanup)
            .count()
            >= 2
    );
}

#[test]
fn fallible_cleanup_callback_observes_reap_before_owner_release() {
    let (owner, slot, dropped) = tracked();
    let callback_slot = slot.clone();
    let (called, callback_result) = mpsc::channel();
    let (sink, _) = retained_log(Options::bounded(2048));
    let mut command = std::process::Command::new("/bin/true");
    command.env_clear();
    let prepared = PreparedCommand::new(command, owner).with_cleanup(move |_| {
        let observer = callback_slot.lock().unwrap().as_ref().unwrap().clone();
        assert!(snapshot(&observer).reaped);
        called.send(()).unwrap();
        Err(io::Error::other("post-reap cleanup failure").into())
    });
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        prepared,
        (),
        Vec::new(),
        sink,
        StdioMode::Captured,
    );
    *slot.lock().unwrap() = Some(observer.clone());
    let error = runtime().block_on(future).unwrap_err();
    callback_result
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    assert_eq!(
        dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
        (true, true)
    );
    assert!(
        snapshot(&observer)
            .cleanup_issue
            .unwrap()
            .message
            .contains("post-reap cleanup failure")
    );
    assert!(!error.to_string().contains("post-reap cleanup failure"));
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct PreparedConfig {
    revision: u64,
    #[serde(skip)]
    events: Arc<Mutex<Vec<u64>>>,
}

#[derive(Debug, Default)]
struct PreparedGlobal;

#[reverie::global_tool]
impl GlobalTool for PreparedGlobal {
    type Request = ();
    type Response = ();
    type Config = PreparedConfig;

    async fn init_global_state(config: &PreparedConfig) -> Self {
        config.events.lock().unwrap().push(config.revision);
        Self
    }

    async fn receive_rpc(&self, _: reverie::Tid, _: ()) {}
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct PreparedTool;

#[reverie::tool]
impl Tool for PreparedTool {
    type GlobalState = PreparedGlobal;
    type ThreadState = ();
}

#[test]
fn pre_global_configuration_runs_on_owned_worker_before_shared_global_and_spawn_check() {
    for mode in [StdioMode::Captured, StdioMode::Inherited] {
        let config = PreparedConfig::default();
        let events = config.events.clone();
        let caller = std::thread::current().id();
        let (owner, slot, dropped) = tracked();
        let (sink, _) = retained_log(Options::bounded(1024));
        let before_spawn = events.clone();
        let prepared = PreparedCommand::new(std::process::Command::new("/bin/true"), owner)
            .with_spawn_check(move |_, command| {
                assert_eq!(command.get_program(), "/bin/true");
                assert_eq!(*before_spawn.lock().unwrap(), [1, 42]);
                before_spawn.lock().unwrap().push(43);
                Err(io::Error::other("modeled stop before native spawn").into())
            });
        let (observer, future) =
            LiteinstBackend::prepare_with_owned_configuration::<PreparedTool, _, _>(
                prepared,
                config,
                Vec::new(),
                sink,
                mode,
                move |_, command, config| {
                    assert_ne!(std::thread::current().id(), caller);
                    assert!(tokio::runtime::Handle::try_current().is_err());
                    assert_eq!(command.get_program(), "/bin/true");
                    config.events.lock().unwrap().push(1);
                    config.revision = 42;
                    Ok(())
                },
            );
        *slot.lock().unwrap() = Some(observer.clone());
        assert!(runtime().block_on(future).is_err());
        assert_eq!(*events.lock().unwrap(), [1, 42, 43]);
        assert_eq!(dropped.recv().unwrap(), (false, false));
        assert!(!snapshot(&observer).spawned);
    }
}

#[test]
fn pre_global_failure_unwind_and_cancellation_prevent_global_and_release_owner() {
    for failure in 0..3 {
        let config = PreparedConfig::default();
        let events = config.events.clone();
        let (owner, slot, dropped) = tracked();
        let (sink, _) = retained_log(Options::bounded(1024));
        let handle = sink.handle();
        let prepared = PreparedCommand::new(std::process::Command::new("/bin/true"), owner);
        let (observer, future) =
            LiteinstBackend::prepare_with_owned_configuration::<PreparedTool, _, _>(
                prepared,
                config,
                Vec::new(),
                sink,
                StdioMode::Captured,
                move |_, _, _| match failure {
                    0 => Err(io::Error::other("configuration refusal").into()),
                    1 => panic!("configuration unwind"),
                    _ => {
                        handle.stop(IssueKind::Child, "cancelled during configuration");
                        Ok(())
                    }
                },
            );
        *slot.lock().unwrap() = Some(observer.clone());
        assert!(runtime().block_on(future).is_err());
        assert!(events.lock().unwrap().is_empty());
        assert_eq!(dropped.recv().unwrap(), (false, false));
        assert!(!snapshot(&observer).spawned);
    }
}

type ObservationSlot = Arc<Mutex<Option<RunObserver>>>;

#[test]
fn cancelled_pre_global_callback_keeps_owner_until_worker_leaves_preparation() {
    let config = PreparedConfig::default();
    let events = config.events.clone();
    let (owner, slot, dropped) = tracked();
    let (sink, _) = retained_log(Options::bounded(1024));
    let (entered, entering) = mpsc::channel();
    let (release, releasing) = mpsc::channel();
    let prepared = PreparedCommand::new(std::process::Command::new("/bin/true"), owner);
    let (observer, future) = LiteinstBackend::prepare_with_owned_configuration::<PreparedTool, _, _>(
        prepared,
        config,
        Vec::new(),
        sink,
        StdioMode::Captured,
        move |_, _, _| {
            entered.send(()).unwrap();
            releasing.recv().unwrap();
            Ok(())
        },
    );
    *slot.lock().unwrap() = Some(observer.clone());
    runtime().block_on(async {
        let task = tokio::spawn(future);
        tokio::task::yield_now().await;
        entering.recv_timeout(Duration::from_secs(3)).unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(dropped.try_recv().is_err());
        release.send(()).unwrap();
    });
    assert_eq!(
        dropped.recv_timeout(Duration::from_secs(3)).unwrap(),
        (false, false)
    );
    assert!(events.lock().unwrap().is_empty());
    assert!(!snapshot(&observer).spawned);
}

type DropObservation = (bool, bool);

#[derive(Clone)]
struct TraceWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for TraceWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn lifecycle_thread_retains_scoped_host_dispatch_and_span() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let writer = TraceWriter(bytes.clone());
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let (sink, _) = retained_log(Options::bounded(1024));
    let (_, future) = tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("owned-launch-scope", token = 42);
        let _entered = span.enter();
        let prepared = PreparedCommand::new(std::process::Command::new("/bin/true"), ())
            .with_spawn_check(|_, _| {
                tracing::info!("owned-launch-host-event");
                Err(io::Error::other("host trace check refuses before spawn").into())
            });
        LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
            prepared,
            (),
            Vec::new(),
            sink,
            StdioMode::Captured,
        )
    });
    assert!(runtime().block_on(future).is_err());
    let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(output.contains("owned-launch-host-event"), "{output}");
    assert!(output.contains("owned-launch-scope"), "{output}");
    assert!(output.contains("token=42"), "{output}");
}

struct Tracked {
    observer: ObservationSlot,
    dropped: mpsc::Sender<DropObservation>,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        let state = snapshot(self.observer.lock().unwrap().as_ref().unwrap());
        let _ = self.dropped.send((state.spawned, state.reaped));
    }
}

fn snapshot(observer: &RunObserver) -> run_evidence::RunEvidence {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match observer.try_snapshot() {
            Ok(state) => return state,
            Err(run_evidence::SnapshotUnavailable::Busy) => {
                assert!(std::time::Instant::now() < deadline, "observer stayed busy");
                std::thread::yield_now();
            }
            Err(error) => panic!("observer unavailable: {error:?}"),
        }
    }
}

fn tracked() -> (Tracked, ObservationSlot, mpsc::Receiver<DropObservation>) {
    let observer = Arc::new(Mutex::new(None));
    let (dropped, receiver) = mpsc::channel();
    (
        Tracked {
            observer: observer.clone(),
            dropped,
        },
        observer,
        receiver,
    )
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn unpolled_owner_is_released_without_launch() {
    let (owner, slot, dropped) = tracked();
    let (sink, _) = retained_log(Options::bounded(1024));
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        PreparedCommand::new(std::process::Command::new("/bin/true"), owner),
        (),
        Vec::new(),
        sink,
        StdioMode::Captured,
    );
    *slot.lock().unwrap() = Some(observer.clone());
    assert!(dropped.try_recv().is_err());
    drop(future);
    assert_eq!(
        dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
        (false, false)
    );
    assert!(observer.try_snapshot().unwrap().caller_cancelled);
}

#[test]
fn spawn_check_preserves_prepared_spec_and_refuses_without_exec() {
    let (owner, slot, dropped) = tracked();
    let (sink, handle) = retained_log(Options::bounded(1024));
    let mut command = std::process::Command::new("unresolved-on-purpose");
    command
        .env_clear()
        .env("LD_PRELOAD", "must-not-be-loaded")
        .env(STATS_COORDINATOR_ENV, "guest-owned");
    command
        .current_dir("/unentered-on-purpose")
        .arg0("original-zero")
        .args(["one", "two words"]);
    let prepared = PreparedCommand::new(command, owner).with_spawn_check(move |_owner, command| {
        assert_eq!(command.get_program(), "unresolved-on-purpose");
        assert_eq!(
            command.get_current_dir(),
            Some(std::path::Path::new("/unentered-on-purpose"))
        );
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["one", "two words"]);
        assert!(format!("{command:?}").contains("original-zero"));
        let env = command
            .get_envs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(env.len(), 2);
        assert_eq!(
            env[std::ffi::OsStr::new("LD_PRELOAD")],
            Some(std::ffi::OsStr::new("must-not-be-loaded"))
        );
        assert_eq!(
            env[std::ffi::OsStr::new(STATS_COORDINATOR_ENV)],
            Some(std::ffi::OsStr::new("guest-owned"))
        );
        Err(io::Error::new(io::ErrorKind::Unsupported, "check refuses before spawn").into())
    });
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        prepared,
        (),
        Vec::new(),
        sink,
        StdioMode::Captured,
    );
    *slot.lock().unwrap() = Some(observer.clone());
    let error = runtime().block_on(future).unwrap_err();
    assert!(error.to_string().contains("check refuses before spawn"));
    assert_eq!(
        dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
        (false, false)
    );
    assert!(!handle.snapshot().root_reaped);
}

#[test]
fn missing_program_keeps_owner_through_real_spawn_failure() {
    let (owner, slot, dropped) = tracked();
    let (sink, handle) = retained_log(Options::bounded(1024));
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        PreparedCommand::new(
            std::process::Command::new("/missing-owned-launch-program"),
            owner,
        ),
        (),
        Vec::new(),
        sink,
        StdioMode::Inherited,
    );
    *slot.lock().unwrap() = Some(observer.clone());
    assert!(runtime().block_on(future).is_err());
    assert_eq!(
        dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
        (false, false)
    );
    assert!(!handle.snapshot().root_reaped);
    assert!(matches!(
        observer.try_snapshot().unwrap().completion,
        run_evidence::RunCompletion::Failed(_)
    ));
}

#[test]
fn spawn_check_unwind_retains_failure_and_releases_unspawned_owner() {
    let (owner, slot, dropped) = tracked();
    let (sink, handle) = retained_log(Options::bounded(1024));
    let prepared = PreparedCommand::new(std::process::Command::new("/bin/true"), owner)
        .with_spawn_check(|_, _| panic!("ordinary host spawn-check unwind"));
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        prepared,
        (),
        Vec::new(),
        sink,
        StdioMode::Captured,
    );
    *slot.lock().unwrap() = Some(observer.clone());
    let error = runtime().block_on(future).unwrap_err();
    assert!(error.to_string().contains("owned logged launch unwound"));
    assert_eq!(
        dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
        (false, false)
    );
    assert!(!handle.snapshot().root_reaped);
}

#[test]
fn ordinary_exit_retains_exact_stdio_and_owner_until_reap_without_handshake_success() {
    let directory = tempfile::tempdir().unwrap();
    let (owner, slot, dropped) = tracked();
    let (sink, handle) = retained_log(Options::bounded(1024));
    let mut command = std::process::Command::new("/bin/sh");
    command
        .env_clear()
        .env("VALUE", "two words")
        .current_dir(directory.path())
        .arg0("retained-zero")
        .args([
            "-c",
            "printf '%s\\000%s' \"$0\" \"$VALUE\"; printf 'err\\377tail' >&2; exit 17",
        ]);
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        PreparedCommand::new(command, owner),
        (),
        Vec::new(),
        sink,
        StdioMode::Captured,
    );
    *slot.lock().unwrap() = Some(observer.clone());
    let error = runtime().block_on(future).unwrap_err();
    assert_eq!(error.stdout, b"retained-zero\0two words");
    assert_eq!(error.stderr, b"err\xfftail");
    assert_eq!(
        dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
        (true, true)
    );
    assert_eq!(
        observer.try_snapshot().unwrap().wait_status.unwrap().code(),
        Some(17)
    );
    assert!(handle.snapshot().root_reaped);
    assert!(!handle.snapshot().qualifies());
}

#[test]
fn caller_runtime_destruction_keeps_owner_until_independent_kill_and_reap() {
    for mode in [StdioMode::Captured, StdioMode::Inherited] {
        let (owner, slot, dropped) = tracked();
        let (sink, handle) = retained_log(Options::bounded(1024));
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("30");
        let (observer, future) =
            LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
                PreparedCommand::new(command, owner),
                (),
                Vec::new(),
                sink,
                mode,
            );
        *slot.lock().unwrap() = Some(observer.clone());
        let runtime = runtime();
        let mut future = Box::pin(future);
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), async {
                tokio::select! {
                    result = &mut future => panic!("sleep returned early: {result:?}"),
                    _ = async { while !observer.try_snapshot().is_ok_and(|state| state.spawned) { tokio::task::yield_now().await; } } => {},
                }
            }).await.unwrap();
        });
        assert!(dropped.try_recv().is_err());
        drop(future);
        drop(runtime);
        assert_eq!(
            dropped.recv_timeout(Duration::from_secs(3)).unwrap(),
            (true, true)
        );
        let state = snapshot(&observer);
        assert!(state.caller_cancelled);
        assert_eq!(state.wait_status.unwrap().signal(), Some(libc::SIGKILL));
        assert!(handle.snapshot().root_reaped);
    }
}

#[test]
fn cancelled_spawn_check_retains_owner_and_never_spawns_after_release() {
    let (owner, slot, dropped) = tracked();
    let (sink, _) = retained_log(Options::bounded(1024));
    let (entered, observed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let prepared = PreparedCommand::new(std::process::Command::new("/bin/true"), owner)
        .with_spawn_check(move |_, _| {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(3)).unwrap();
            Ok(())
        });
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        prepared,
        (),
        Vec::new(),
        sink,
        StdioMode::Captured,
    );
    *slot.lock().unwrap() = Some(observer.clone());
    let runtime = runtime();
    let mut future = Box::pin(future);
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut future => panic!("check returned early: {result:?}"),
                _ = async { while observed.try_recv().is_err() { tokio::task::yield_now().await; } } => {},
            }
        }).await.unwrap();
    });
    drop(future);
    drop(runtime);
    assert!(dropped.try_recv().is_err());
    release.send(()).unwrap();
    assert_eq!(
        dropped.recv_timeout(Duration::from_secs(3)).unwrap(),
        (false, false)
    );
}

#[test]
fn pidfd_fallback_reaps_before_releasing_owner() {
    for already_reaped in [false, true] {
        let (owner, slot, dropped) = tracked();
        let observer = RunObserver::new(StdioMode::Inherited);
        *slot.lock().unwrap() = Some(observer.clone());
        let (_sink, handle) = retained_log(Options::bounded(1024));
        let mut lifetime = Lifetime {
            owner: Some(owner),
            check: None,
            pending: false,
            handle: handle.clone(),
        };
        let mut command = std::process::Command::new("/bin/true");
        unsafe {
            command.pre_exec(|| Ok(()));
        }
        let child = child::Child::spawn(
            command,
            &mut lifetime,
            observer.clone(),
            handle.clone(),
            child::Kernel,
        )
        .unwrap();
        let pid = snapshot(&observer).pid.unwrap() as libc::pid_t;
        if already_reaped {
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            assert!(std::process::ExitStatus::from_raw(status).success());
            drop(child);
        } else {
            drop(child);
            assert_eq!(unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) }, -1);
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
        drop(lifetime);
        assert_eq!(
            dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
            (true, true)
        );
        assert!(handle.snapshot().root_reaped);
        assert_eq!(
            observer.try_snapshot().unwrap().wait_status.is_none(),
            already_reaped
        );
    }
}

#[test]
fn actual_post_native_spawn_adaptation_error_reaps_before_owner_release() {
    for mode in [StdioMode::Captured, StdioMode::Inherited] {
        let (owner, slot, dropped) = tracked();
        let observer = RunObserver::new(mode);
        *slot.lock().unwrap() = Some(observer.clone());
        let (_sink, handle) = retained_log(Options::bounded(1024));
        let mut lifetime = Lifetime {
            owner: Some(owner),
            check: None,
            pending: false,
            handle: handle.clone(),
        };
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("30");
        if mode == StdioMode::Captured {
            command.stdout(std::process::Stdio::piped());
        }
        unsafe {
            command.pre_exec(|| Ok(()));
        }
        let mut child = child::Child::spawn(
            command,
            &mut lifetime,
            observer.clone(),
            handle.clone(),
            child::Kernel,
        )
        .unwrap();
        let state = snapshot(&observer);
        assert!(state.spawned);
        assert!(!state.reaped);
        assert!(dropped.try_recv().is_err());
        let runtime = runtime();
        let runtime_handle = runtime.handle().clone();
        drop(runtime);
        let entered = runtime_handle.enter();
        let error = child.adapt().unwrap_err();
        assert_eq!(
            error.to_string(),
            "A Tokio 1.x context was found, but it is being shutdown."
        );
        drop(entered);
        assert!(dropped.try_recv().is_err());
        drop(child);
        drop(lifetime);
        assert_eq!(
            dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
            (true, true)
        );
        assert_eq!(
            snapshot(&observer).wait_status.unwrap().signal(),
            Some(libc::SIGKILL)
        );
        assert!(handle.snapshot().root_reaped);
        assert_eq!(
            unsafe {
                libc::waitpid(
                    state.pid.unwrap() as i32,
                    std::ptr::null_mut(),
                    libc::WNOHANG,
                )
            },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}

struct PanickingOwner;

#[derive(Debug, Default)]
struct PanickingGlobal;

impl Drop for PanickingGlobal {
    fn drop(&mut self) {
        panic!("global destructor panic");
    }
}

#[reverie::global_tool]
impl GlobalTool for PanickingGlobal {
    type Request = ();
    type Response = ();
    type Config = ();

    async fn init_global_state(_: &()) -> Self {
        Self
    }

    async fn receive_rpc(&self, _: reverie::Tid, _: ()) {}
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct PanickingGlobalTool;

#[reverie::tool]
impl Tool for PanickingGlobalTool {
    type GlobalState = PanickingGlobal;
    type ThreadState = ();
}

#[test]
fn primary_spawn_check_error_survives_global_destructor_panic() {
    const CHILD: &str = "LITEINST_GLOBAL_DESTRUCTOR_PANIC_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new("timeout")
            .args(["-k", "2s", "10s"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "backend::logged::owned::tests::primary_spawn_check_error_survives_global_destructor_panic", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success(), "isolated destructor regression: {status}");
        return;
    }
    let (sink, handle) = retained_log(Options::bounded(1024));
    let prepared = PreparedCommand::new(std::process::Command::new("/bin/true"), ())
        .with_spawn_check(|_, _| Err(io::Error::from_raw_os_error(libc::EACCES).into()));
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<
        PanickingGlobalTool,
        _,
    >(prepared, (), Vec::new(), sink, StdioMode::Captured);
    let error = runtime().block_on(future).unwrap_err();
    assert!(
        matches!(&error.cause, Error::Io(cause) if cause.raw_os_error() == Some(libc::EACCES)),
        "original typed cause was replaced: {error:?}"
    );
    let state = snapshot(&observer);
    assert!(!state.spawned);
    assert!(
        state
            .first_error
            .unwrap()
            .display
            .contains("Permission denied")
    );
    assert!(
        state
            .cleanup_issue
            .unwrap()
            .message
            .contains("global state cleanup unwound")
    );
    assert!(
        matches!(state.completion, run_evidence::RunCompletion::Failed(failure) if failure.display.contains("Permission denied"))
    );
    for retained in [handle.snapshot(), error.logs.snapshot()] {
        assert!(
            retained
                .issues
                .iter()
                .any(|issue| issue.kind == IssueKind::Cleanup
                    && issue.message.contains("global state cleanup unwound"))
        );
        assert!(!retained.qualifies());
        assert!(retained.streams.is_empty());
    }
}

impl Drop for PanickingOwner {
    fn drop(&mut self) {
        panic!("owner destructor panic");
    }
}

#[test]
fn primary_execution_error_survives_owner_destructor_panic_with_cleanup_issue() {
    let (sink, handle) = retained_log(Options::bounded(1024));
    let prepared = PreparedCommand::new(std::process::Command::new("/bin/true"), PanickingOwner)
        .with_spawn_check(|_, _| Err(io::Error::other("original primary failure").into()));
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        prepared,
        (),
        Vec::new(),
        sink,
        StdioMode::Captured,
    );
    let error = runtime().block_on(future).unwrap_err();
    assert!(error.to_string().contains("original primary failure"));
    assert!(!error.to_string().contains("cleanup unwound"));
    assert!(!snapshot(&observer).spawned);
    assert!(format!("{:?}", snapshot(&observer).first_error).contains("original primary failure"));
    for retained in [handle.snapshot(), error.logs.snapshot()] {
        assert!(
            retained
                .issues
                .iter()
                .any(|issue| issue.kind == IssueKind::Cleanup
                    && issue.message.contains("cleanup unwound"))
        );
        assert!(!retained.qualifies());
    }
}

#[test]
fn reaped_output_and_primary_are_retained_when_owner_destructor_panics() {
    let (sink, handle) = retained_log(Options::bounded(1024));
    let mut command = std::process::Command::new("/bin/sh");
    command.args([
        "-c",
        "printf 'out\\000tail'; printf 'err\\377tail' >&2; exit 17",
    ]);
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        PreparedCommand::new(command, PanickingOwner),
        (),
        Vec::new(),
        sink,
        StdioMode::Captured,
    );
    let error = runtime().block_on(future).unwrap_err();
    assert!(!error.to_string().contains("cleanup unwound"));
    assert_eq!(error.stdout, b"out\0tail");
    assert_eq!(error.stderr, b"err\xfftail");
    assert_eq!(snapshot(&observer).wait_status.unwrap().code(), Some(17));
    assert!(snapshot(&observer).reaped);
    assert!(handle.snapshot().root_reaped);
    assert!(
        error
            .logs
            .snapshot()
            .issues
            .iter()
            .any(|issue| issue.kind == IssueKind::Cleanup
                && issue.message.contains("cleanup unwound"))
    );
    assert!(!handle.snapshot().qualifies());
}

#[test]
fn native_pre_exec_error_has_already_reaped_the_forked_child() {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let (mut receiver, sender) = std::os::unix::net::UnixStream::pair().unwrap();
    let (owner, slot, dropped) = tracked();
    let (sink, _) = retained_log(Options::bounded(1024));
    let mut command = std::process::Command::new("/bin/true");
    unsafe {
        command.pre_exec(move || {
            let pid = libc::getpid().to_ne_bytes();
            if libc::write(sender.as_raw_fd(), pid.as_ptr().cast(), pid.len()) != pid.len() as isize
            {
                libc::_exit(99);
            }
            Err(io::Error::from_raw_os_error(libc::EPERM))
        });
    }
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        PreparedCommand::new(command, owner),
        (),
        Vec::new(),
        sink,
        StdioMode::Inherited,
    );
    *slot.lock().unwrap() = Some(observer.clone());
    let error = runtime().block_on(future).unwrap_err();
    assert!(error.to_string().contains("Operation not permitted"));
    receiver
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = [0; std::mem::size_of::<libc::pid_t>()];
    receiver.read_exact(&mut bytes).unwrap();
    let pid = libc::pid_t::from_ne_bytes(bytes);
    assert!(pid > 0);
    assert_eq!(
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert_eq!(
        dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
        (false, false)
    );
}

#[test]
fn unconfirmed_native_reap_retains_launch_owner() {
    use std::os::fd::AsFd;
    use std::os::fd::OwnedFd;
    use std::os::linux::process::PidFd;

    use child::Operations;

    struct Unconfirmed(Arc<Mutex<Option<OwnedFd>>>);
    impl Operations for Unconfirmed {
        fn signal(&mut self, pidfd: &PidFd) -> io::Result<()> {
            *self.0.lock().unwrap() = Some(pidfd.as_fd().try_clone_to_owned()?);
            child::Kernel.signal(pidfd)
        }
        fn wait(&mut self, _: &PidFd, _: bool) -> io::Result<Option<std::process::ExitStatus>> {
            Err(io::Error::from_raw_os_error(libc::EIO))
        }
    }
    let (owner, slot, dropped) = tracked();
    let observer = RunObserver::new(StdioMode::Inherited);
    *slot.lock().unwrap() = Some(observer.clone());
    let (_sink, handle) = retained_log(Options::bounded(1024));
    let mut lifetime = Lifetime {
        owner: Some(owner),
        check: None,
        pending: false,
        handle: handle.clone(),
    };
    let identity = Arc::new(Mutex::new(None));
    let mut command = std::process::Command::new("/bin/true");
    unsafe {
        command.pre_exec(|| Ok(()));
    }
    let child = child::Child::spawn(
        command,
        &mut lifetime,
        observer.clone(),
        handle.clone(),
        Unconfirmed(identity.clone()),
    )
    .unwrap();
    drop(child);
    assert!(lifetime.pending);
    drop(lifetime);
    assert!(matches!(dropped.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert!(!snapshot(&observer).reaped);
    assert!(!handle.snapshot().root_reaped);
    assert!(
        handle
            .snapshot()
            .issues
            .iter()
            .any(|issue| issue.kind == IssueKind::Cleanup
                && issue.message.contains("launch resources retained"))
    );
    let pidfd = PidFd::from(identity.lock().unwrap().take().unwrap());
    assert!(child::Kernel.wait(&pidfd, true).unwrap().is_some());
}
