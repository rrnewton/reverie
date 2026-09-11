#[cfg(test)]
use std::sync::Mutex;

use reverie_rpc_transport::RpcIssueMonitor;
use reverie_rpc_transport::guest_log::IssueKind;
use reverie_rpc_transport::guest_log::LogHandle;
use reverie_rpc_transport::guest_log::LogSink;
use reverie_rpc_transport::guest_log::Phase;
use reverie_rpc_transport::guest_log::RunState;
use reverie_rpc_transport::guest_log::channel_pair;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;

use super::*;

mod child;
pub(super) mod owned;

#[cfg(test)]
mod capture_tests;

pub mod run_evidence;
use run_evidence::Reading;
use run_evidence::RunObserver;
use run_evidence::StdioMode;
use run_evidence::Stream;
use run_evidence::StreamState;

/// Retains the first run error and access to all collected log and RPC evidence.
pub struct LoggedRunError {
    pub cause: Error,
    pub logs: LogHandle,
    pub rpc: Option<RpcIssueMonitor>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl std::fmt::Debug for LoggedRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoggedRunError")
            .field("cause", &self.cause)
            .field("logs", &self.logs.snapshot())
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}
impl std::fmt::Display for LoggedRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cause.fmt(formatter)
    }
}
impl std::error::Error for LoggedRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

struct Owner {
    handle: LogHandle,
    evidence: RunObserver,
    completed: bool,
    caller: bool,
}
impl Owner {
    fn complete(&mut self) {
        self.completed = true;
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if !self.completed {
            self.evidence.dropped(self.caller);
            self.handle.run_state(if self.caller {
                RunState::Cancelled
            } else {
                RunState::Interrupted
            });
            self.handle.stop(
                if self.caller {
                    IssueKind::Cancelled
                } else {
                    IssueKind::Interrupted
                },
                "logged run owner dropped",
            );
        }
    }
}

struct Evidence {
    handle: LogHandle,
    rpc: Option<RpcIssueMonitor>,
    run: RunObserver,
}

impl Evidence {
    fn error(&self, cause: Error) -> LoggedRunError {
        let (stdout, stderr) = self.run.output();
        LoggedRunError {
            cause,
            logs: self.handle.clone(),
            rpc: self.rpc.clone(),
            stdout,
            stderr,
        }
    }
}

pub(super) fn prepare_run<T: Tool + 'static>(
    command: Command,
    config: <T::GlobalState as GlobalTool>::Config,
    preload: PathBuf,
    tool_data: Vec<u8>,
    sink: LogSink,
    mode: StdioMode,
) -> (
    RunObserver,
    impl Future<Output = Result<(Output, T::GlobalState), LoggedRunError>>,
) {
    prepare_command::<T>(
        move || prepare(command, preload),
        config,
        tool_data,
        sink,
        mode,
    )
}

fn prepare_command<T: Tool + 'static>(
    command: impl FnOnce() -> Result<std::process::Command, Error>,
    config: <T::GlobalState as GlobalTool>::Config,
    tool_data: Vec<u8>,
    sink: LogSink,
    mode: StdioMode,
) -> (
    RunObserver,
    impl Future<Output = Result<(Output, T::GlobalState), LoggedRunError>>,
) {
    let handle = sink.handle();
    let observer = RunObserver::new(mode);
    let retained = observer.clone();
    let mut caller = Owner {
        handle: handle.clone(),
        evidence: observer.clone(),
        completed: false,
        caller: true,
    };
    let future = async move {
        observer.polled();
        let command = match command() {
            Ok(command) => command,
            Err(error) => {
                observer.finished(Some(&error));
                handle.run_state(RunState::Failed);
                handle.stop(IssueKind::Startup, &error);
                caller.complete();
                return Err(LoggedRunError {
                    cause: error,
                    logs: handle,
                    rpc: None,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                });
            }
        };
        let mut worker = Owner {
            handle: handle.clone(),
            evidence: observer.clone(),
            completed: false,
            caller: false,
        };
        let task = tokio::spawn(async move {
            let mut evidence = Evidence {
                handle: handle.clone(),
                rpc: None,
                run: observer,
            };
            let result = execute::<T>(command, config, tool_data, sink, &mut evidence).await;
            evidence.run.finished(result.as_ref().err());
            if let Err(error) = &result {
                handle.run_state(RunState::Failed);
                handle.stop(IssueKind::Child, error);
            } else {
                handle.run_state(RunState::Succeeded);
            }
            worker.complete();
            result.map_err(|error| evidence.error(error))
        });
        caller.evidence.worker_submitted();
        let result = task.await;
        caller.complete();
        result.unwrap_or_else(|error| {
            let error = Error::from(io::Error::other(error));
            caller.evidence.finished(Some(&error));
            caller.handle.run_state(RunState::Interrupted);
            caller.handle.stop(IssueKind::Interrupted, &error);
            Err(Evidence {
                handle: caller.handle.clone(),
                rpc: None,
                run: caller.evidence.clone(),
            }
            .error(error))
        })
    };
    (retained, future)
}

fn cancelled() -> Error {
    io::Error::new(io::ErrorKind::Interrupted, "logged run cancelled").into()
}

fn prepare(mut command: Command, preload: PathBuf) -> Result<std::process::Command, Error> {
    let preload = preload.canonicalize()?;
    let arg0 = command.get_arg0().to_owned();
    let program = command.find_program()?;
    command.program(program).arg0(arg0);
    configure_in_guest_address_space(&mut command);
    configure_in_guest_command_preload(&mut command, preload);
    Ok(command.try_into_std()?)
}

struct ServerTask(tokio::task::JoinHandle<Result<(), RpcError>>);
impl Drop for ServerTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn execute<T: Tool + 'static>(
    mut child_command: std::process::Command,
    config: <T::GlobalState as GlobalTool>::Config,
    tool_data: Vec<u8>,
    sink: LogSink,
    evidence: &mut Evidence,
) -> Result<(Output, T::GlobalState), Error> {
    child_command.env_remove(STATS_COORDINATOR_ENV);
    execute_with_launch::<T>(child_command, config, tool_data, sink, evidence, None).await
}

async fn execute_with_launch<T: Tool + 'static>(
    mut child_command: std::process::Command,
    config: <T::GlobalState as GlobalTool>::Config,
    tool_data: Vec<u8>,
    mut sink: LogSink,
    evidence: &mut Evidence,
    mut launch: Option<&mut dyn owned::LaunchLifetime>,
) -> Result<(Output, T::GlobalState), Error> {
    #[cfg(feature = "test-guest-log")]
    let fixture_control = sink.fixture_control();
    let handle = evidence.handle.clone();
    if handle.stopped() {
        return Err(cancelled());
    }
    let directory = tempfile::Builder::new().prefix("rll-").tempdir_in("/tmp")?;
    let socket = directory.path().join("rpc");
    let global = tokio::select! {
        global = T::GlobalState::init_global_state(&config) => Arc::new(global),
        _ = handle.stopping() => return Err(cancelled()),
    };
    let connected = Arc::new(AtomicBool::new(false));
    let mut server = RpcServer::bind_with_connection_readiness(
        &socket,
        global.clone(),
        config,
        connected.clone(),
    )
    .map_err(|error| io::Error::other(error.to_string()))?;
    let issues = server.retain_connection_issues();
    handle.retain_rpc(issues.clone())?;
    evidence.rpc = Some(issues.clone());
    let connections = server.connection_monitor();
    let (guest, reader, ordered) = if let Some(guest) = sink.take_prepared_endpoint()? {
        (guest, None, true)
    } else {
        let (host, guest) = channel_pair(handle.options())?;
        let collector = sink.reader(host)?;
        (
            guest,
            Some(tokio::task::spawn_blocking(move || collector.run())),
            false,
        )
    };
    let bootstrap = create_versioned_log_bootstrap(
        &socket,
        &tool_data,
        Some(guest.as_raw_fd()),
        true,
        ordered,
    )?;
    let bootstrap_fd = bootstrap.as_raw_fd();
    let guest_fd = guest.as_raw_fd();
    unsafe {
        child_command.pre_exec(move || inherit_log_fds(bootstrap_fd, guest_fd));
    }
    if let Err(error) = handle.ready().await {
        if let Some(reader) = &reader {
            reader.abort();
        }
        return Err(error.into());
    }
    if handle.stopped() {
        return Err(cancelled());
    }
    if let Some(launch) = launch.as_deref_mut() {
        launch
            .before_spawn(&mut child_command)
            .inspect_err(|error| {
                evidence.run.failed(error);
                handle.stop(IssueKind::Startup, error);
            })?;
    }
    if handle.stopped() {
        return Err(cancelled());
    }
    let mut server_task = ServerTask(tokio::spawn(server.serve()));
    let mut child = match child::LoggedChild::spawn(
        child_command,
        launch,
        evidence.run.clone(),
        handle.clone(),
    ) {
        Ok(child) => child,
        Err(error) => {
            evidence.run.failed(&error);
            handle.stop(IssueKind::Startup, &error);
            issues.planned_shutdown();
            server_task.0.abort();
            let _ = (&mut server_task.0).await;
            drop(guest);
            drop(bootstrap);
            let _ = handle.guest_finished().await;
            if let Some(reader) = &reader {
                reader.abort();
            }
            return Err(error.into());
        }
    };
    let mut first_error = child.adapt().err().map(Error::from);
    drop(guest);
    drop(bootstrap);
    handle.run_state(RunState::Running);
    let mut output_readers = tokio::task::JoinSet::new();
    if let Some(stdout) = child.take_stdout() {
        output_readers.spawn(read_output(stdout, evidence.run.clone(), Stream::Stdout));
    } else {
        evidence.run.unavailable(Stream::Stdout);
    }
    if let Some(stderr) = child.take_stderr() {
        output_readers.spawn(read_output(stderr, evidence.run.clone(), Stream::Stderr));
    } else {
        evidence.run.unavailable(Stream::Stderr);
    }
    let mut server_finished = false;
    let status = if first_error.is_some() {
        None
    } else {
        tokio::select! {
            result = child.wait() => match result { Ok(status) => { evidence.run.reaped(status); Some(status) }, Err(error) => { evidence.run.wait_failed(&error); first_error = Some(error.into()); None } },
            _ = handle.stopping() => { first_error = Some(cancelled()); None },
            _ = issues.failed() => { first_error = Some(io::Error::other("Tool RPC connection failed; see retained connection issue").into()); None },
            result = &mut server_task.0 => {
                server_finished = true;
                first_error = Some(io::Error::other(format!("RPC listener ended: {result:?}")).into()); None
            },
        }
    };
    if let Some(error) = &first_error {
        evidence.run.failed(error);
        handle.stop(IssueKind::Child, error);
        if let Err(error) = child.start_kill() {
            handle.issue(IssueKind::Cleanup, error);
        }
        match tokio::time::timeout(RPC_CONNECTION_DRAIN_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => {
                evidence.run.reaped(status);
                #[cfg(feature = "test-guest-log")]
                if let Some(control) = &fixture_control {
                    control.record_guest_status(status);
                }
                #[cfg(not(feature = "test-guest-log"))]
                let _ = status;
                handle.root_reaped();
            }
            result => {
                if let Ok(Err(error)) = &result {
                    evidence.run.wait_failed(error);
                }
                handle.issue(IssueKind::Cleanup, format!("child reap: {result:?}"));
            }
        }
    } else {
        #[cfg(feature = "test-guest-log")]
        if let (Some(control), Some(status)) = (&fixture_control, status) {
            control.record_guest_status(status);
        }
        handle.root_reaped();
    }
    let drain = async {
        while let Some(result) = output_readers.join_next().await {
            result.map_err(io::Error::other)??;
        }
        connections.wait_for_idle().await;
        Ok::<_, io::Error>(())
    };
    let drained = tokio::select! {
        result = tokio::time::timeout(RPC_CONNECTION_DRAIN_TIMEOUT, drain) => result,
        _ = handle.stopping() => Ok(Err(io::Error::new(io::ErrorKind::Interrupted, "stdio/RPC drain cancelled"))),
    };
    match drained {
        Ok(Ok(())) => {}
        result => {
            let error = io::Error::other(format!("stdio/RPC drain: {result:?}"));
            handle.stop(IssueKind::Cleanup, &error);
            if first_error.is_none() {
                evidence.run.failed(&error);
                first_error = Some(error.into());
            }
        }
    }
    output_readers.abort_all();
    while output_readers.join_next().await.is_some() {}
    issues.planned_shutdown();
    if !server_finished {
        server_task.0.abort();
        let _ = (&mut server_task.0).await;
    }
    for issue in issues.snapshot() {
        handle.issue(IssueKind::Rpc, format!("{issue:?}"));
    }
    select_rpc_error(
        &mut first_error,
        !issues.snapshot().is_empty(),
        &evidence.run,
    );
    if tokio::time::timeout(RPC_CONNECTION_DRAIN_TIMEOUT, handle.guest_finished())
        .await
        .is_err()
    {
        handle.stop(
            IssueKind::Cutoff,
            "root ended but logging lifetime did not close",
        );
    }
    let report = handle.guest_finished().await;
    if let Some(reader) = reader {
        if reader.is_finished() {
            reader.await.map_err(io::Error::other)?;
        } else {
            reader.abort();
        }
    }
    if report.phase != Phase::Complete && first_error.is_none() {
        first_error = Some(io::Error::other("incomplete guest log; see retained report").into());
    }
    if !connected.load(Ordering::Acquire) && first_error.is_none() {
        first_error =
            Some(io::Error::other("guest exited before RPC configuration handshake").into());
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    let global = unwrap_global_after_connections(global).await?;
    let (stdout, stderr) = evidence.run.output();
    Ok((
        Output {
            status: status.expect("successful child wait"),
            stdout,
            stderr,
        },
        global,
    ))
}

fn select_rpc_error(first_error: &mut Option<Error>, has_issues: bool, observer: &RunObserver) {
    if has_issues && first_error.is_none() {
        let error = io::Error::other("retained RPC connection failure").into();
        observer.failed(&error);
        *first_error = Some(error);
    }
}

fn read_output(
    mut reader: impl AsyncRead + Unpin,
    observer: RunObserver,
    stream: Stream,
) -> impl Future<Output = io::Result<()>> {
    let reading = Reading::new(observer.clone(), stream);
    async move {
        let _reading = reading;
        let mut buffer = [0u8; 8192];
        loop {
            let length = match reader.read(&mut buffer).await {
                Ok(length) => length,
                Err(error) => {
                    observer.failed(&error);
                    observer.stream_state(
                        stream,
                        StreamState::ReadFailed(run_evidence::FailureEvidence::new(&error)),
                    );
                    return Err(error);
                }
            };
            if length == 0 {
                observer.stream_state(stream, StreamState::Eof);
                return Ok(());
            }
            observer.append(stream, &buffer[..length]);
        }
    }
}

unsafe fn inherit_log_fds(bootstrap_fd: i32, guest_fd: i32) -> io::Result<()> {
    for (fd, message) in [
        (bootstrap_fd, b"liteinst pre_exec log: fcntl F_SETFD bootstrap failed\n".as_slice()),
        (guest_fd, b"liteinst pre_exec log: fcntl F_SETFD guest log failed\n".as_slice()),
    ] {
        unsafe {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                let error = io::Error::last_os_error();
                reverie::process::report_pre_exec_failure(libc::STDERR_FILENO, message);
                return Err(error);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use reverie_rpc_transport::guest_log::Options;
    use reverie_rpc_transport::guest_log::retained_log;

    use super::*;

    #[test]
    fn pre_exec_log_fd_diagnostics_preserve_errors_and_success_is_silent() {
        use std::io::{Read, Seek};
        for bad_bootstrap in [false, true] {
            for closed_stderr in [false, true] {
                let mut output = tempfile::tempfile().unwrap();
                let valid = tempfile::tempfile().unwrap();
                let valid_fd = valid.as_raw_fd();
                let mut command = std::process::Command::new("/bin/true");
                command.stderr(output.try_clone().unwrap());
                unsafe {
                    command.pre_exec(move || {
                        if closed_stderr {
                            libc::close(libc::STDERR_FILENO);
                        }
                        if bad_bootstrap {
                            inherit_log_fds(-1, valid_fd)
                        } else {
                            inherit_log_fds(valid_fd, -1)
                        }
                    });
                }
                let error = command.spawn().unwrap_err();
                assert_eq!(error.raw_os_error(), Some(libc::EBADF));
                output.rewind().unwrap();
                let mut message = String::new();
                output.read_to_string(&mut message).unwrap();
                let expected = if closed_stderr { "" }
                    else if bad_bootstrap { "liteinst pre_exec log: fcntl F_SETFD bootstrap failed\n" }
                    else { "liteinst pre_exec log: fcntl F_SETFD guest log failed\n" };
                assert_eq!(message, expected);
            }
        }
        let retained = tempfile::tempfile().unwrap();
        let retained_fd = retained.as_raw_fd();
        let mut command = std::process::Command::new("/bin/true");
        unsafe {
            command.pre_exec(move || inherit_log_fds(retained_fd, retained_fd));
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }

    #[cfg(feature = "test-guest-log")]
    #[test]
    fn fixture_retains_failed_child_status_from_both_adapters() {
        use reverie_rpc_transport::guest_log::fixture::Control;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for inherited in [false, true] {
            let control = Arc::new(Control::new().unwrap());
            let (sink, handle) = retained_log(Options::bounded(1024));
            let sink = sink.with_fixture_control(control.clone());
            let command = Command::new("/bin/false");
            let result = runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(10), async {
                    if inherited {
                        LiteinstBackend::run_with_inherited_stdio_and_preload_data_and_log_sink::<()>(command, (), "/bin/true", Vec::new(), sink).await
                    } else {
                        LiteinstBackend::run_with_output_and_preload_data_and_log_sink::<()>(command, (), "/bin/true", Vec::new(), sink).await
                    }
                }).await.unwrap()
            });
            assert!(result.is_err());
            assert!(handle.snapshot().terminal());
            assert!(!handle.snapshot().qualifies());
            assert_eq!(
                control.observations().guest_reaped.load(Ordering::Acquire),
                1
            );
            assert_eq!(
                control
                    .observations()
                    .guest_wait_status
                    .load(Ordering::Acquire),
                1 << 8
            );
            assert_eq!(
                control
                    .observations()
                    .install_result
                    .load(Ordering::Acquire),
                0
            );
        }
    }

    fn saturated_launcher(inherited: bool, cancel: bool) {
        use std::io::Read;
        use std::sync::mpsc;

        use reverie_rpc_transport::guest_log::ReaderState;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (release, gate) = mpsc::channel();
        let (entered, entry) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        runtime.spawn_blocking(move || {
            entered.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(10)).unwrap();
            finished.send(()).unwrap();
        });
        entry.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut marker = tempfile::tempfile().unwrap();
        let marker_fd = marker.as_raw_fd();
        let mut command = Command::new("/bin/true");
        unsafe {
            command.pre_exec(move || {
                libc::write(marker_fd, b"P".as_ptr().cast(), 1);
                Err(reverie::syscalls::Errno::EINVAL)
            });
        }
        let (sink, handle) = retained_log(Options::bounded(1024));
        runtime.block_on(async {
            let mut future: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> = if inherited {
                Box::pin(LiteinstBackend::run_with_inherited_stdio_and_preload_data_and_log_sink::<()>(command, (), "/bin/true", Vec::new(), sink))
            } else {
                Box::pin(LiteinstBackend::run_with_output_and_preload_data_and_log_sink::<()>(command, (), "/bin/true", Vec::new(), sink))
            };
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::select! {
                    result = &mut future => panic!("launcher returned before readiness: {result:?}"),
                    _ = async { while handle.snapshot().reader != ReaderState::Queued { tokio::task::yield_now().await; } } => {},
                }
            }).await.unwrap();
            assert_eq!(marker.metadata().unwrap().len(), 0);
            assert_eq!(handle.snapshot().run, RunState::Pending);
            if cancel {
                drop(future);
                let report = tokio::time::timeout(Duration::from_secs(2), handle.finished()).await.unwrap();
                assert_eq!(report.phase, Phase::Incomplete);
                assert_eq!(report.run, RunState::Cancelled);
                assert_eq!(report.reader, ReaderState::Queued);
                assert!(report.streams.is_empty());
                assert!(handle.ready().await.is_err());
            } else {
                release.send(()).unwrap();
                let result = tokio::time::timeout(Duration::from_secs(2), future).await.unwrap();
                assert!(result.is_err());
                assert_eq!(handle.snapshot().reader, ReaderState::Ready);
                assert_eq!(marker.metadata().unwrap().len(), 1);
            }
        });
        runtime.shutdown_timeout(Duration::from_millis(20));
        if cancel {
            assert_eq!(handle.snapshot().run, RunState::Cancelled);
            assert_eq!(marker.metadata().unwrap().len(), 0);
            release.send(()).unwrap();
        }
        completion.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(handle.snapshot().terminal());
        let mut bytes = Vec::new();
        use std::io::Seek;
        marker.rewind().unwrap();
        marker.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, if cancel { b"".as_slice() } else { b"P" });
    }

    #[test]
    fn captured_launcher_waits_for_collector_under_pool_saturation() {
        for cancel in [true, false] {
            saturated_launcher(false, cancel);
        }
    }

    #[test]
    fn inherited_launcher_waits_for_collector_under_pool_saturation() {
        for cancel in [true, false] {
            saturated_launcher(true, cancel);
        }
    }

    #[test]
    fn both_unpolled_adapters_revoke_startup_ownership() {
        for inherited in [false, true] {
            let (sink, handle) = retained_log(Options::bounded(1024));
            if inherited {
                drop(
                    LiteinstBackend::run_with_inherited_stdio_and_preload_data_and_log_sink::<()>(
                        Command::new("/bin/true"),
                        (),
                        "/not-polled",
                        Vec::new(),
                        sink,
                    ),
                );
            } else {
                drop(
                    LiteinstBackend::run_with_output_and_preload_data_and_log_sink::<()>(
                        Command::new("/bin/true"),
                        (),
                        "/not-polled",
                        Vec::new(),
                        sink,
                    ),
                );
            }
            let report = handle.snapshot();
            assert_eq!(report.phase, Phase::Incomplete);
            assert_eq!(report.run, RunState::Cancelled);
            assert!(report.streams.is_empty());
        }
    }

    #[tokio::test]
    async fn launch_error_keeps_first_cause_and_caller_handle() {
        let (sink, handle) = retained_log(Options::bounded(1024));
        let error = LiteinstBackend::run_with_output_and_preload_data_and_log_sink::<()>(
            Command::new("/bin/true"),
            (),
            "/missing-retained-log-preload",
            Vec::new(),
            sink,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("No such file"), "{error:?}");
        let report = handle.finished().await;
        assert_eq!(report.run, RunState::Failed);
        assert_eq!(report.issues[0].kind, IssueKind::Startup);
        assert!(!report.root_reaped);
    }

    static INITIALIZING: AtomicBool = AtomicBool::new(false);
    #[derive(Default)]
    struct PendingGlobal;
    #[reverie::global_tool]
    impl GlobalTool for PendingGlobal {
        type Request = ();
        type Response = ();
        type Config = ();
        async fn init_global_state(_: &()) -> Self {
            INITIALIZING.store(true, Ordering::Release);
            std::future::pending().await
        }
        async fn receive_rpc(&self, _: reverie::Tid, _: ()) {}
    }
    #[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
    struct PendingTool;
    #[reverie::tool]
    impl Tool for PendingTool {
        type GlobalState = PendingGlobal;
        type ThreadState = ();
    }

    #[test]
    fn cancelled_initializer_retained_outside_tokio_runtime() {
        let (sink, handle) = retained_log(Options::bounded(1024));
        {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut future = Box::pin(LiteinstBackend::run_with_output_and_preload_data_and_log_sink::<PendingTool>(Command::new("/bin/true"), (), "/bin/true", Vec::new(), sink));
                tokio::time::timeout(Duration::from_secs(2), async {
                    tokio::select! {
                        _ = &mut future => panic!("pending initializer returned"),
                        _ = async { while !INITIALIZING.load(Ordering::Acquire) { tokio::task::yield_now().await; } } => {},
                    }
                }).await.unwrap();
                drop(future);
                assert_eq!(handle.finished().await.phase, Phase::Incomplete);
            });
        }
        assert_eq!(handle.snapshot().run, RunState::Cancelled);
        assert!(!handle.snapshot().root_reaped);
    }

    #[tokio::test]
    async fn ordinary_child_failure_retains_exact_separate_output() {
        let (sink, handle) = retained_log(Options::bounded(1024));
        let mut evidence = Evidence {
            handle: handle.clone(),
            rpc: None,
            run: RunObserver::new(StdioMode::Captured),
        };
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", "printf 'out\\000tail'; printf 'err\\377tail' >&2"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let result = execute::<()>(command, (), Vec::new(), sink, &mut evidence).await;
        assert!(result.is_err());
        assert_eq!(evidence.run.output().0, b"out\0tail");
        assert_eq!(evidence.run.output().1, b"err\xfftail");
        let report = handle.snapshot();
        assert!(report.root_reaped);
        assert_eq!(report.phase, Phase::Incomplete);
        assert!(report.streams.is_empty());
    }
}
