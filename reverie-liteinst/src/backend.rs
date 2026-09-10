//! Coordinator-side implementation of Reverie's backend contract.

use std::future::Future;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::process::Output;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reverie::Backend;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Tool;
use reverie::process::Command;
use reverie::process::Output as ReverieOutput;
#[cfg(test)]
use reverie_rpc_transport::ConnectionMonitor;
use reverie_rpc_transport::RpcError;
use reverie_rpc_transport::RpcServer;

mod logged;
pub use logged::LoggedRunError;
pub use logged::owned::PreparedCommand;
pub use logged::run_evidence;
use reverie_liteinst_runtime::bootstrap::*;

const RPC_CONNECTION_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

// TODO-HUMAN-REVIEW(PR-127): Review LiteInst Backend lifecycle and preload contract.
/// Online LiteInst backend with a coordinator-owned `GlobalTool`.
pub struct LiteinstBackend;

impl LiteinstBackend {
    pub fn prepare_with_owned_configuration<T, O, F>(
        command: PreparedCommand<O>,
        config: <T::GlobalState as GlobalTool>::Config,
        tool_data: Vec<u8>,
        sink: reverie_rpc_transport::guest_log::LogSink,
        mode: run_evidence::StdioMode,
        configure: F,
    ) -> (
        run_evidence::RunObserver,
        impl Future<Output = Result<(Output, T::GlobalState), LoggedRunError>>,
    )
    where
        T: Tool + 'static,
        O: Send + 'static,
        F: FnOnce(
                &mut O,
                &std::process::Command,
                &mut <T::GlobalState as GlobalTool>::Config,
            ) -> Result<(), Error>
            + Send
            + 'static,
    {
        logged::owned::prepare_configured::<T, O, F>(
            command, config, tool_data, sink, mode, configure,
        )
    }

    pub fn prepare_with_owned_command_data_and_log_sink<T, O>(
        command: PreparedCommand<O>,
        config: <T::GlobalState as GlobalTool>::Config,
        tool_data: impl Into<Vec<u8>>,
        sink: reverie_rpc_transport::guest_log::LogSink,
        mode: run_evidence::StdioMode,
    ) -> (
        run_evidence::RunObserver,
        impl Future<Output = Result<(Output, T::GlobalState), LoggedRunError>>,
    )
    where
        T: Tool + 'static,
        O: Send + 'static,
    {
        logged::owned::prepare::<T, O>(command, config, tool_data.into(), sink, mode)
    }

    #[cfg(test)]
    fn prepare_test_command<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
        tool_data: impl Into<Vec<u8>>,
        sink: reverie_rpc_transport::guest_log::LogSink,
        mode: run_evidence::StdioMode,
    ) -> (
        run_evidence::RunObserver,
        impl Future<Output = Result<(Output, T::GlobalState), LoggedRunError>>,
    )
    where
        T: Tool + 'static,
    {
        let preload = preload.into();
        let command = command
            .try_into_std()
            .expect("test command must convert to std::process::Command");
        let prepared =
            PreparedCommand::new(command, preload).with_spawn_check(|preload, command| {
                let preload = preload.canonicalize()?;
                command.env("LD_PRELOAD", preload);
                Ok(())
            });
        Self::prepare_with_owned_command_data_and_log_sink::<T, _>(
            prepared, config, tool_data, sink, mode,
        )
    }

    #[cfg(test)]
    pub(crate) fn prepare_with_output_and_preload_data_and_log_sink<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
        tool_data: impl Into<Vec<u8>>,
        sink: reverie_rpc_transport::guest_log::LogSink,
    ) -> (
        run_evidence::RunObserver,
        impl Future<Output = Result<(Output, T::GlobalState), LoggedRunError>>,
    )
    where
        T: Tool + 'static,
    {
        Self::prepare_test_command::<T>(
            command,
            config,
            preload,
            tool_data,
            sink,
            run_evidence::StdioMode::Captured,
        )
    }

    #[cfg(test)]
    pub(crate) fn prepare_with_inherited_stdio_and_preload_data_and_log_sink<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
        tool_data: impl Into<Vec<u8>>,
        sink: reverie_rpc_transport::guest_log::LogSink,
    ) -> (
        run_evidence::RunObserver,
        impl Future<Output = Result<(Output, T::GlobalState), LoggedRunError>>,
    )
    where
        T: Tool + 'static,
    {
        Self::prepare_test_command::<T>(
            command,
            config,
            preload,
            tool_data,
            sink,
            run_evidence::StdioMode::Inherited,
        )
    }

    #[cfg(test)]
    pub(crate) fn run_with_output_and_preload_data_and_log_sink<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
        tool_data: impl Into<Vec<u8>>,
        sink: reverie_rpc_transport::guest_log::LogSink,
    ) -> impl Future<Output = Result<(Output, T::GlobalState), LoggedRunError>>
    where
        T: Tool + 'static,
    {
        Self::prepare_with_output_and_preload_data_and_log_sink::<T>(
            command, config, preload, tool_data, sink,
        )
        .1
    }

    #[cfg(test)]
    pub(crate) fn run_with_inherited_stdio_and_preload_data_and_log_sink<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
        tool_data: impl Into<Vec<u8>>,
        sink: reverie_rpc_transport::guest_log::LogSink,
    ) -> impl Future<Output = Result<(Output, T::GlobalState), LoggedRunError>>
    where
        T: Tool + 'static,
    {
        Self::prepare_with_inherited_stdio_and_preload_data_and_log_sink::<T>(
            command, config, preload, tool_data, sink,
        )
        .1
    }
}

#[cfg(test)]
fn configure_in_guest_address_space(command: &mut Command) {
    unsafe {
        command.pre_exec(set_in_guest_address_space);
    }
}

fn configure_owned_in_guest_address_space(command: &mut std::process::Command) {
    unsafe {
        command.pre_exec(|| set_in_guest_address_space().map_err(Into::into));
    }
}

fn set_in_guest_address_space() -> Result<(), reverie::syscalls::Errno> {
    unsafe {
        let current = libc::personality(0xffff_ffff);
        if current == -1 {
            return Err(reverie::syscalls::Errno::last());
        }
        let personality = current as libc::c_ulong | libc::ADDR_NO_RANDOMIZE as libc::c_ulong;
        if libc::personality(personality) == -1 {
            return Err(reverie::syscalls::Errno::last());
        }
        Ok(())
    }
}

#[cfg(test)]
fn inherit_stdio(command: &mut Command) {
    command.stdin(reverie::process::Stdio::inherit());
    command.stdout(reverie::process::Stdio::inherit());
    command.stderr(reverie::process::Stdio::inherit());
}

#[reverie::backend(?Send)]
impl Backend for LiteinstBackend {
    type Stats = crate::LiteinstBackendStatsSnapshot;

    async fn run<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
    ) -> Result<(ExitStatus, T::GlobalState), Error>
    where
        T: Tool + 'static,
    {
        let _ = (command, config);
        Err(unsupported_backend_run())
    }

    async fn run_with_stats<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
    ) -> Result<(ExitStatus, T::GlobalState, Self::Stats), Error>
    where
        T: Tool + 'static,
    {
        let _ = (command, config);
        Err(unsupported_backend_run())
    }

    async fn run_with_output<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
    ) -> Result<(ReverieOutput, T::GlobalState, Self::Stats), Error>
    where
        T: Tool + 'static,
    {
        let _ = (command, config);
        Err(unsupported_backend_run())
    }
}

fn unsupported_backend_run() -> Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "LiteInst Backend execution requires a caller-owned PreparedCommand",
    )
    .into()
}

#[cfg(test)]
async fn serve_rpc_until<G, F, T>(
    server: RpcServer<G>,
    stats_server: Option<RpcServer<crate::stats::LiteinstStatsGlobal>>,
    connection_monitors: Vec<ConnectionMonitor>,
    completion: F,
) -> io::Result<T>
where
    G: GlobalTool + 'static,
    F: Future<Output = io::Result<T>>,
{
    serve_rpc_until_with_timeout(
        server,
        stats_server,
        connection_monitors,
        completion,
        RPC_CONNECTION_DRAIN_TIMEOUT,
    )
    .await
}

#[cfg(test)]
async fn serve_rpc_until_with_timeout<G, F, T>(
    server: RpcServer<G>,
    stats_server: Option<RpcServer<crate::stats::LiteinstStatsGlobal>>,
    connection_monitors: Vec<ConnectionMonitor>,
    completion: F,
    drain_timeout: Duration,
) -> io::Result<T>
where
    G: GlobalTool + 'static,
    F: Future<Output = io::Result<T>>,
{
    // JoinSet aborts the server if this future is cancelled. RpcServer::serve
    // owns the per-connection JoinSet, so aborting it also releases every
    // outstanding connection and its GlobalTool Arc.
    let mut serving = tokio::task::JoinSet::new();
    serving.spawn(server.serve());
    if let Some(stats_server) = stats_server {
        serving.spawn(stats_server.serve());
    }

    serve_rpc_tasks_until_with_timeout(serving, connection_monitors, completion, drain_timeout)
        .await
}

#[cfg(test)]
fn rpc_server_stopped(
    result: Option<Result<Result<(), RpcError>, tokio::task::JoinError>>,
) -> io::Error {
    let message = match result {
        Some(Ok(Ok(()))) => "LiteInst coordinator stopped unexpectedly".to_owned(),
        Some(Ok(Err(error))) => error.to_string(),
        Some(Err(error)) => error.to_string(),
        None => "LiteInst coordinator task disappeared".to_owned(),
    };
    io::Error::other(message)
}

#[cfg(test)]
async fn serve_rpc_tasks_until_with_timeout<F, T>(
    mut serving: tokio::task::JoinSet<Result<(), RpcError>>,
    connection_monitors: Vec<ConnectionMonitor>,
    completion: F,
    drain_timeout: Duration,
) -> io::Result<T>
where
    F: Future<Output = io::Result<T>>,
{
    tokio::pin!(completion);

    let mut result = tokio::select! {
        biased;
        result = &mut completion => result,
        result = serving.join_next() => return Err(rpc_server_stopped(result)),
    };

    // A fork child inherits the parent's connected socket. If it later needs
    // its own identity, the client opens the replacement before dropping the
    // inherited descriptor. Consequently this count cannot transiently reach
    // zero while a supported descendant still owns coordinator state. Block
    // on last-close notifications instead of keeping this task runnable, but
    // fail closed after a finite interval if a descriptor is leaked.
    let drain = async {
        for monitor in &connection_monitors {
            monitor.wait_for_idle().await;
        }
    };
    tokio::pin!(drain);
    let drain_result = tokio::select! {
        biased;
        result = serving.join_next() => return Err(rpc_server_stopped(result)),
        result = tokio::time::timeout(drain_timeout, &mut drain) => result,
    };
    if drain_result.is_err() {
        let active_connections = connection_monitors
            .iter()
            .map(ConnectionMonitor::active_connections)
            .sum::<usize>();
        let timeout_message = format!(
            "LiteInst coordinator retained {active_connections} active RPC connection(s) for {}ms after guest exit",
            drain_timeout.as_millis()
        );
        if result.is_ok() {
            result = Err(io::Error::new(io::ErrorKind::TimedOut, timeout_message));
        } else {
            tracing::warn!("{timeout_message}; preserving guest completion error");
        }
    }

    serving.abort_all();
    while let Some(server_result) = serving.join_next().await {
        match server_result {
            Err(error) if error.is_cancelled() => {}
            Ok(Ok(())) => {
                return Err(io::Error::other(
                    "LiteInst coordinator stopped unexpectedly",
                ));
            }
            Ok(Err(error)) => return Err(io::Error::other(error.to_string())),
            Err(error) => return Err(io::Error::other(error.to_string())),
        }
    }
    result
}

async fn unwrap_global_after_connections<G>(mut global: Arc<G>) -> io::Result<G> {
    // Aborting RpcServer::serve drops its JoinSet, which aborts every connection
    // task. Those tasks release their GlobalTool Arc on their next scheduler
    // turn, so a fast guest can otherwise race Arc::try_unwrap here.
    for _ in 0..1024 {
        match Arc::try_unwrap(global) {
            Ok(global) => return Ok(global),
            Err(still_shared) => global = still_shared,
        }
        tokio::task::yield_now().await;
    }
    Err(io::Error::other(
        "LiteInst coordinator state still has owners after connection shutdown",
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn buffered_bootstrap_preserves_closed_stdin() {
        const CHILD: &str = "REVERIE_TEST_BUFFERED_CLOSED_STDIN";
        if std::env::var_os(CHILD).is_some() {
            let (_host, guest) = reverie_rpc_transport::guest_log::channel_pair(
                reverie_rpc_transport::guest_log::Options::bounded(1024),
            )
            .unwrap();
            assert_eq!(unsafe { libc::close(libc::STDIN_FILENO) }, 0);
            let packet = super::create_log_bootstrap(
                std::path::Path::new("/tmp/rpc"),
                b"tool",
                Some(guest.as_raw_fd()),
                true,
            )
            .unwrap();
            assert!(packet.as_raw_fd() > libc::STDERR_FILENO);
            assert_eq!(
                unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFD) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "backend::tests::buffered_bootstrap_preserves_closed_stdin",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{output:?}"
        );
    }
    #[test]
    fn buffered_guest_log_bootstrap_is_distinct_and_round_trips() {
        use std::os::fd::AsRawFd;
        use std::os::fd::IntoRawFd;
        let (host, guest) = reverie_rpc_transport::guest_log::channel_pair(
            reverie_rpc_transport::guest_log::Options::bounded(1024),
        )
        .unwrap();
        let fd = guest.into_raw_fd();
        let packet =
            super::create_log_bootstrap(std::path::Path::new("/tmp/rpc"), b"tool", Some(fd), true)
                .unwrap();
        let mut magic = [0u8; 16];
        assert_eq!(
            unsafe {
                libc::pread(
                    packet.as_raw_fd(),
                    magic.as_mut_ptr().cast(),
                    magic.len(),
                    0,
                )
            },
            16
        );
        assert_eq!(&magic, super::BUFFERED_LOG_BOOTSTRAP_MAGIC);
        assert_ne!(&magic, super::LOG_BOOTSTRAP_MAGIC);
        let decoded = super::read_preload_bootstrap(packet.as_raw_fd())
            .unwrap()
            .unwrap();
        assert_eq!(decoded.coordinator, std::path::Path::new("/tmp/rpc"));
        assert_eq!(decoded.tool_data, b"tool");
        assert!(decoded.log.is_some());
        drop(decoded);
        drop(host);
    }
    #[test]
    fn guest_log_bootstrap_round_trip() {
        use std::os::fd::AsRawFd;
        use std::os::fd::IntoRawFd;
        let (guest, _peer) = crate::guest_log::channel_pair().unwrap();
        let fd = guest.into_raw_fd();
        let packet = super::create_logged_preload_bootstrap(
            std::path::Path::new("/tmp/coordinator"),
            b"typed-tool",
            Some(fd),
        )
        .unwrap();
        let decoded = super::read_preload_bootstrap(packet.as_raw_fd())
            .unwrap()
            .unwrap();
        assert_eq!(
            decoded.coordinator,
            std::path::Path::new("/tmp/coordinator")
        );
        assert_eq!(decoded.tool_data, b"typed-tool");
        assert!(decoded.log.is_some());
    }

    use std::sync::Mutex;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    use reverie::Tid;
    use reverie_preload::rpc::CoordinatorClient;

    use super::*;

    fn short_socket_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("reverie-liteinst-test-")
            .tempdir_in("/tmp")
            .unwrap()
    }

    struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CapturedLogWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn coordinator_global_waits_for_cancelled_connection_owners() {
        let global = Arc::new(17_u64);
        let connection_owner = global.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            drop(connection_owner);
        });

        assert_eq!(unwrap_global_after_connections(global).await.unwrap(), 17);
    }

    #[derive(Default)]
    struct MultiClientGlobal {
        total: AtomicU64,
        senders: Mutex<Vec<i32>>,
    }

    #[reverie::global_tool]
    impl GlobalTool for MultiClientGlobal {
        type Request = u64;
        type Response = u64;
        type Config = u64;

        async fn receive_rpc(&self, from: Tid, amount: u64) -> u64 {
            self.senders.lock().unwrap().push(from.as_raw());
            self.total.fetch_add(amount, Ordering::Relaxed) + amount
        }
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn production_coordinator_drain_timeout_is_thirty_seconds() {
        assert_eq!(RPC_CONNECTION_DRAIN_TIMEOUT, Duration::from_secs(30));

        let directory = short_socket_tempdir();
        let socket = directory.path().join("coordinator.sock");
        let server = RpcServer::bind(&socket, Arc::new(MultiClientGlobal::default()), 41).unwrap();
        let monitor = server.connection_monitor();
        let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();
        let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();

        let client = tokio::spawn(async move {
            let _client = reverie_rpc_transport::RpcClient::<MultiClientGlobal>::connect(
                &socket,
                Tid::from_raw(707),
            )
            .await
            .unwrap();
            connected_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let completion = async move {
            complete_rx
                .await
                .map_err(|error| io::Error::other(error.to_string()))
        };
        let mut serving = Box::pin(serve_rpc_until(
            server,
            None,
            vec![monitor.clone()],
            completion,
        ));

        let initial_poll =
            std::future::poll_fn(|context| std::task::Poll::Ready(serving.as_mut().poll(context)))
                .await;
        assert!(initial_poll.is_pending());
        connected_rx.await.unwrap();
        assert_eq!(monitor.active_connections(), 1);

        complete_tx.send(()).unwrap();
        let drain_poll =
            std::future::poll_fn(|context| std::task::Poll::Ready(serving.as_mut().poll(context)))
                .await;
        assert!(drain_poll.is_pending());
        tokio::time::advance(Duration::from_secs(29)).await;
        let before_deadline =
            std::future::poll_fn(|context| std::task::Poll::Ready(serving.as_mut().poll(context)))
                .await;
        assert!(before_deadline.is_pending());

        tokio::time::advance(Duration::from_secs(1)).await;
        let error = tokio::time::timeout(Duration::from_secs(1), serving)
            .await
            .expect("the production coordinator drain did not expire at 30 seconds")
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            error.to_string(),
            "LiteInst coordinator retained 1 active RPC connection(s) for 30000ms after guest exit"
        );

        client.abort();
        let _ = client.await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coordinator_serves_multiple_local_rpc_connections() {
        let directory = short_socket_tempdir();
        let socket = directory.path().join("coordinator.sock");
        let global = Arc::new(MultiClientGlobal::default());
        let server = RpcServer::bind(&socket, global.clone(), 41).unwrap();
        let connection_monitors = vec![server.connection_monitor()];

        let clients = tokio::task::spawn_blocking(move || -> io::Result<()> {
            let mut first = CoordinatorClient::connect(&socket)?;
            let mut second = CoordinatorClient::connect(&socket)?;
            assert_eq!(first.config::<u64>()?, 41);
            assert_eq!(second.config::<u64>()?, 41);

            let first_total: u64 = first.send(Tid::from_raw(101), 2_u64)?;
            let second_total: u64 = second.send(Tid::from_raw(202), 3_u64)?;
            assert_eq!(first_total, 2);
            assert_eq!(second_total, 5);
            Ok(())
        });
        let completion = async move {
            clients
                .await
                .map_err(|error| io::Error::other(error.to_string()))?
        };

        tokio::time::timeout(
            Duration::from_secs(5),
            serve_rpc_until(server, None, connection_monitors, completion),
        )
        .await
        .expect("the second local RPC connection blocked at its config handshake")
        .unwrap();

        assert_eq!(global.total.load(Ordering::Relaxed), 5);
        assert_eq!(*global.senders.lock().unwrap(), [101, 202]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coordinator_two_monitor_drain_is_level_triggered() {
        let directory = short_socket_tempdir();
        let main_socket = directory.path().join("coordinator.sock");
        let stats_socket = directory.path().join("stats.sock");
        let main_server =
            RpcServer::bind(&main_socket, Arc::new(MultiClientGlobal::default()), 41).unwrap();
        let stats_server = RpcServer::bind(
            &stats_socket,
            Arc::new(crate::stats::LiteinstStatsGlobal::default()),
            (),
        )
        .unwrap();
        let main_monitor = main_server.connection_monitor();
        let stats_monitor = stats_server.connection_monitor();
        let connection_monitors = vec![main_monitor.clone(), stats_monitor.clone()];
        let (main_connected_tx, main_connected_rx) = tokio::sync::oneshot::channel();
        let (stats_connected_tx, stats_connected_rx) = tokio::sync::oneshot::channel();
        let (release_main_tx, release_main_rx) = tokio::sync::oneshot::channel();
        let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();

        let main_client = tokio::spawn(async move {
            let client = reverie_rpc_transport::RpcClient::<MultiClientGlobal>::connect(
                &main_socket,
                Tid::from_raw(404),
            )
            .await
            .unwrap();
            main_connected_tx.send(()).unwrap();
            release_main_rx.await.unwrap();
            drop(client);
        });
        let stats_client = tokio::spawn(async move {
            let client =
                reverie_rpc_transport::RpcClient::<crate::stats::LiteinstStatsGlobal>::connect(
                    &stats_socket,
                    Tid::from_raw(405),
                )
                .await
                .unwrap();
            stats_connected_tx.send(()).unwrap();
            drop(client);
        });
        let completion = async move {
            complete_rx
                .await
                .map_err(|error| io::Error::other(error.to_string()))
        };
        let mut serving = Box::pin(serve_rpc_until_with_timeout(
            main_server,
            Some(stats_server),
            connection_monitors,
            completion,
            Duration::from_secs(1),
        ));

        let initial_poll =
            std::future::poll_fn(|context| std::task::Poll::Ready(serving.as_mut().poll(context)))
                .await;
        assert!(initial_poll.is_pending());
        main_connected_rx.await.unwrap();
        stats_connected_rx.await.unwrap();
        stats_client.await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while stats_monitor.active_connections() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the stats connection did not close before the drain began");
        assert_eq!(main_monitor.active_connections(), 1);
        assert_eq!(stats_monitor.active_connections(), 0);

        complete_tx.send(()).unwrap();
        let drain_poll =
            std::future::poll_fn(|context| std::task::Poll::Ready(serving.as_mut().poll(context)))
                .await;
        assert!(drain_poll.is_pending());
        assert_eq!(main_monitor.active_connections(), 1);

        release_main_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), serving)
            .await
            .expect("the two-monitor coordinator drain hung")
            .unwrap();
        main_client.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coordinator_surfaces_server_failure_during_connection_drain() {
        let directory = short_socket_tempdir();
        let socket = directory.path().join("coordinator.sock");
        let server = RpcServer::bind(&socket, Arc::new(MultiClientGlobal::default()), 41).unwrap();
        let monitor = server.connection_monitor();
        let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();
        let (release_client_tx, release_client_rx) = tokio::sync::oneshot::channel();
        let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();
        let (fail_server_tx, fail_server_rx) = tokio::sync::oneshot::channel();

        let client = tokio::spawn(async move {
            let client = reverie_rpc_transport::RpcClient::<MultiClientGlobal>::connect(
                &socket,
                Tid::from_raw(505),
            )
            .await
            .unwrap();
            connected_tx.send(()).unwrap();
            release_client_rx.await.unwrap();
            drop(client);
        });
        let mut server_tasks = tokio::task::JoinSet::new();
        server_tasks.spawn(server.serve());
        server_tasks.spawn(async move {
            fail_server_rx.await.unwrap();
            Err(RpcError::Io(io::Error::other("drain-phase server failure")))
        });
        let completion = async move {
            complete_rx
                .await
                .map_err(|error| io::Error::other(error.to_string()))
        };
        let mut serving = Box::pin(serve_rpc_tasks_until_with_timeout(
            server_tasks,
            vec![monitor.clone()],
            completion,
            Duration::from_secs(1),
        ));

        connected_rx.await.unwrap();
        assert_eq!(monitor.active_connections(), 1);
        complete_tx.send(()).unwrap();
        let drain_poll =
            std::future::poll_fn(|context| std::task::Poll::Ready(serving.as_mut().poll(context)))
                .await;
        assert!(drain_poll.is_pending());
        assert_eq!(monitor.active_connections(), 1);

        fail_server_tx.send(()).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), serving)
            .await
            .expect("the drain did not observe the injected server failure")
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(
            error.to_string(),
            "rpc i/o error: drain-phase server failure"
        );

        release_client_tx.send(()).unwrap();
        client.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coordinator_connection_drain_is_bounded() {
        let directory = short_socket_tempdir();
        let socket = directory.path().join("coordinator.sock");
        let global = Arc::new(MultiClientGlobal::default());
        let server = RpcServer::bind(&socket, global, 41).unwrap();
        let connection_monitors = vec![server.connection_monitor()];
        let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();

        let client = tokio::spawn(async move {
            let _client = reverie_rpc_transport::RpcClient::<MultiClientGlobal>::connect(
                &socket,
                Tid::from_raw(303),
            )
            .await
            .unwrap();
            connected_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let completion = async move {
            connected_rx
                .await
                .map_err(|error| io::Error::other(error.to_string()))
        };

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            serve_rpc_until_with_timeout(
                server,
                None,
                connection_monitors,
                completion,
                Duration::from_millis(25),
            ),
        )
        .await
        .expect("a held connection left the coordinator drain unbounded")
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            error.to_string(),
            "LiteInst coordinator retained 1 active RPC connection(s) for 25ms after guest exit"
        );

        client.abort();
        let _ = client.await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coordinator_drain_timeout_preserves_completion_error() {
        let directory = short_socket_tempdir();
        let socket = directory.path().join("coordinator.sock");
        let server = RpcServer::bind(&socket, Arc::new(MultiClientGlobal::default()), 41).unwrap();
        let connection_monitors = vec![server.connection_monitor()];
        let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();
        let captured_logs = Arc::new(Mutex::new(Vec::new()));
        let writer_logs = captured_logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_writer(move || CapturedLogWriter(writer_logs.clone()))
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);

        let client = tokio::spawn(async move {
            let _client = reverie_rpc_transport::RpcClient::<MultiClientGlobal>::connect(
                &socket,
                Tid::from_raw(606),
            )
            .await
            .unwrap();
            connected_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let completion = async move {
            connected_rx
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            Err::<(), _>(io::Error::new(
                io::ErrorKind::InvalidData,
                "guest completion failed before RPC drain",
            ))
        };

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            serve_rpc_until_with_timeout(
                server,
                None,
                connection_monitors,
                completion,
                Duration::from_millis(25),
            ),
        )
        .await
        .expect("a held connection left the coordinator drain unbounded")
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "guest completion failed before RPC drain"
        );
        let logs = String::from_utf8(captured_logs.lock().unwrap().clone()).unwrap();
        assert!(logs.contains(
            "LiteInst coordinator retained 1 active RPC connection(s) for 25ms after guest exit; preserving guest completion error"
        ));

        client.abort();
        let _ = client.await;
    }

    /// Identifies live bootstrap objects by protocol payload, not descriptor
    /// number. Another parallel test may reuse an integer immediately after
    /// this test closes it, but cannot turn an unrelated descriptor into the
    /// uniquely identified bootstrap object.
    fn open_test_bootstraps() -> io::Result<Vec<(PathBuf, Vec<u8>)>> {
        let mut open = Vec::new();
        for entry in std::fs::read_dir("/proc/self/fd")? {
            let entry = entry?;
            let Some(fd) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<libc::c_int>().ok())
            else {
                continue;
            };
            if fd <= libc::STDERR_FILENO {
                continue;
            }
            if let Some(bootstrap) = read_preload_bootstrap(fd)? {
                open.push((bootstrap.coordinator, bootstrap.tool_data));
            }
        }
        Ok(open)
    }

    #[test]
    fn rejects_and_closes_multiple_matching_bootstraps() {
        let expected = [
            (
                PathBuf::from("/tmp/reverie-liteinst-fd-reuse-test-1.sock"),
                b"one".to_vec(),
            ),
            (
                PathBuf::from("/tmp/reverie-liteinst-fd-reuse-test-2.sock"),
                b"two".to_vec(),
            ),
            (
                PathBuf::from("/tmp/reverie-liteinst-fd-reuse-test-3.sock"),
                b"three".to_vec(),
            ),
        ];
        for (coordinator, tool_data) in &expected {
            let bootstrap = create_preload_bootstrap(coordinator, tool_data).unwrap();
            std::mem::forget(bootstrap);
        }
        let open = open_test_bootstraps().unwrap();
        assert!(
            expected.iter().all(|expected| open.contains(expected)),
            "created bootstrap descriptors are not all visible: {open:?}"
        );

        let error = match unsafe { take_preload_bootstrap() } {
            Err(error) => error,
            Ok(_) => panic!("multiple matching bootstraps must fail"),
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "multiple LiteInst preload bootstraps");
        let open = open_test_bootstraps().unwrap();
        assert!(
            expected.iter().all(|expected| !open.contains(expected)),
            "rejected bootstrap descriptors remain open: {open:?}"
        );
    }

    #[test]
    fn inherited_stdio_replaces_caller_pipes() {
        let mut command = Command::new("/bin/true");
        command
            .stdin(reverie::process::Stdio::piped())
            .stdout(reverie::process::Stdio::piped())
            .stderr(reverie::process::Stdio::piped());
        inherit_stdio(&mut command);
        let mut child = command.try_into_std().unwrap().spawn().unwrap();
        assert!(child.stdin.is_none());
        assert!(child.stdout.is_none());
        assert!(child.stderr.is_none());
        let status = child.wait().unwrap();
        assert!(status.success());
    }

    #[test]
    fn in_guest_address_space_preserves_caller_layout_and_parent() {
        const CHILD_ENV: &str = "REVERIE_LITEINST_ADDRESS_SPACE_TEST_CHILD";
        let mut stack = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut stack) },
            0
        );
        let original_personality = unsafe { libc::personality(0xffff_ffff) };
        assert_ne!(original_personality, -1);
        if let Ok(expected) = std::env::var(CHILD_ENV) {
            let expected: Vec<u64> = expected
                .split(',')
                .map(|value| value.parse().unwrap())
                .collect();
            assert_eq!(original_personality as u64, expected[0]);
            assert_eq!(stack.rlim_cur, expected[1]);
            assert_eq!(stack.rlim_max, expected[2]);
            return;
        }

        let caller_personality =
            (original_personality & !libc::ADDR_NO_RANDOMIZE) | libc::ADDR_COMPAT_LAYOUT;
        let expected_personality = caller_personality | libc::ADDR_NO_RANDOMIZE;
        let caller_stack = libc::rlimit {
            rlim_cur: stack.rlim_cur.min(4 * 1024 * 1024),
            rlim_max: stack.rlim_max,
        };
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "backend::tests::in_guest_address_space_preserves_caller_layout_and_parent",
            "--test-threads=1",
        ]);
        command.env(
            CHILD_ENV,
            format!(
                "{expected_personality},{},{}",
                caller_stack.rlim_cur, caller_stack.rlim_max
            ),
        );
        unsafe {
            command.pre_exec(move || {
                if libc::personality(caller_personality as libc::c_ulong) == -1
                    || libc::setrlimit(libc::RLIMIT_STACK, &caller_stack) == -1
                {
                    return Err(reverie::syscalls::Errno::last());
                }
                Ok(())
            });
        }
        configure_in_guest_address_space(&mut command);
        configure_in_guest_address_space(&mut command);
        let output = command.try_into_std().unwrap().output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            unsafe { libc::personality(0xffff_ffff) },
            original_personality
        );
        let mut after = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut after) },
            0
        );
        assert_eq!(after.rlim_cur, stack.rlim_cur);
        assert_eq!(after.rlim_max, stack.rlim_max);
    }

    #[test]
    fn in_guest_address_space_errors_abort_exec_without_diagnostics() {
        for deny_query in [true, false] {
            for initialize in [false, true] {
                let mut command = Command::new("/bin/true");
                command.stderr(reverie::process::Stdio::null());
                unsafe {
                    command.pre_exec(move || {
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
                                k: libc::SECCOMP_RET_ERRNO | libc::EACCES as u32,
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
                            return Err(reverie::syscalls::Errno::last());
                        }
                        Ok(())
                    });
                }
                if initialize {
                    configure_in_guest_address_space(&mut command);
                }
                let result = command.try_into_std().unwrap().spawn();
                if initialize {
                    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EACCES));
                } else {
                    assert!(result.unwrap().wait().unwrap().success());
                }
            }
        }
    }
}
