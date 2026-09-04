//! Coordinator-side implementation of Reverie's backend contract.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::process::Output;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reverie::Backend;
use reverie::BackendStatsRequest;
use reverie::BackendStatsSource;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Tool;
use reverie::process::Command;
use reverie::process::Output as ReverieOutput;
use reverie::process::Stdio as ReverieStdio;
use reverie_ptrace::TracerBuilder;
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

    /// Buffered logging with caller-owned evidence outside this future's lifetime.
    /// Application stdin is unchanged; stdout and stderr are captured separately.
    pub fn run_with_output_and_preload_data_and_log_sink<T>(
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

    pub fn prepare_with_output_and_preload_data_and_log_sink<T>(
        mut command: Command,
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
        command.stdout(ReverieStdio::piped());
        command.stderr(ReverieStdio::piped());
        logged::prepare_run::<T>(
            command,
            config,
            preload.into(),
            tool_data.into(),
            sink,
            run_evidence::StdioMode::Captured,
        )
    }

    /// Buffered logs without redirecting or re-emitting any inherited stdio.
    pub fn run_with_inherited_stdio_and_preload_data_and_log_sink<T>(
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

    pub fn prepare_with_inherited_stdio_and_preload_data_and_log_sink<T>(
        mut command: Command,
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
        inherit_stdio(&mut command);
        logged::prepare_run::<T>(
            command,
            config,
            preload.into(),
            tool_data.into(),
            sink,
            run_evidence::StdioMode::Inherited,
        )
    }

    /// Runs a typed preload with a bounded, separately retained guest log.
    ///
    /// The constructor must install the bootstrap log before its Tool. A missing
    /// completion frame, transport failure, or truncation is returned in `log.error`,
    /// even when the application exits successfully. Such logs cannot qualify parity.
    pub async fn run_with_output_and_preload_data_and_log<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
        tool_data: impl Into<Vec<u8>>,
        log_limit: usize,
    ) -> Result<(Output, T::GlobalState, crate::CapturedGuestLog), Error>
    where
        T: Tool + 'static,
    {
        let (sink, handle) = reverie_rpc_transport::guest_log::retained_log(
            reverie_rpc_transport::guest_log::Options::bounded(log_limit),
        );
        let (output, global) = Self::run_with_output_and_preload_data_and_log_sink::<T>(
            command, config, preload, tool_data, sink,
        )
        .await
        .map_err(|error| Error::from(io::Error::other(error)))?;
        let report = handle.snapshot();
        let error = if report.streams.len() > 1 {
            Some("multi-producer diagnostic concatenation is not canonical order; use the retained sink API".to_owned())
        } else if !report.qualifies() {
            Some(format!("{report:?}"))
        } else {
            None
        };
        let bytes = report
            .streams
            .into_iter()
            .flat_map(|stream| stream.bytes)
            .collect();
        Ok((output, global, crate::CapturedGuestLog { bytes, error }))
    }

    /// Runs a Tool under the ptrace-owned LiteInst hybrid runtime.
    ///
    /// Ptrace owns the sole Tool and GlobalTool from exec onward; the preload
    /// contributes only dynamic site installation and injected hot-site traps.
    /// The hybrid may follow threads and child processes through `clone` and
    /// `fork`. `vfork` remains fail-closed before either side is resumed because
    /// exec cannot preserve the preload runtime in its shared address space.
    ///
    /// Trap markers, exact DSO addresses, mapping state, and an inner runtime
    /// call site provide strong accidental-collision resistance. They are not a
    /// security boundary against arbitrary code already executing in the guest.
    // TODO-HUMAN-REVIEW(PR-270): Review public host-hybrid launch API.
    pub async fn run_host_with_preload<T>(
        mut command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
    ) -> Result<(ExitStatus, T::GlobalState), Error>
    where
        T: Tool + 'static,
    {
        let preload = configure_host_command(&mut command, preload.into())?;
        TracerBuilder::<T>::new(command)
            .config(config)
            .liteinst_runtime(
                preload,
                crate::runtime::HOST_BEGIN_MARKER,
                crate::runtime::HOST_READY_MARKER,
                crate::runtime::HOST_HELPER_RETURN_MARKER,
                crate::runtime::HOST_SYSCALL_MARKER,
            )
            .spawn()
            .await?
            .wait()
            .await
    }

    /// Runs the ptrace-owned LiteInst hybrid and returns typed backend statistics.
    ///
    /// This source fully accounts for the current hybrid because every installed
    /// hook returns through the ptrace-host SIGTRAP path. The in-guest Tool path
    /// keeps direct-hook counters in each guest process; exposing those after
    /// exit requires RPC aggregation and is deliberately not inferred here.
    pub async fn run_host_with_preload_and_stats<T>(
        mut command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
    ) -> Result<
        (
            ExitStatus,
            T::GlobalState,
            crate::LiteinstBackendStatsSource,
        ),
        Error,
    >
    where
        T: Tool + 'static,
    {
        let preload = configure_host_command(&mut command, preload.into())?;
        let tracer = TracerBuilder::<T>::new(command)
            .config(config)
            .liteinst_runtime_with_stats(
                preload,
                crate::runtime::HOST_BEGIN_MARKER,
                crate::runtime::HOST_READY_MARKER,
                crate::runtime::HOST_HELPER_RETURN_MARKER,
                crate::runtime::HOST_SYSCALL_MARKER,
                BackendStatsRequest::ENABLED,
            )
            .spawn()
            .await?;
        let stats = tracer
            .liteinst_instrumentation_stats()
            .expect("LiteInst runtime tracer must expose instrumentation statistics");
        let (status, global) = tracer.wait().await?;
        Ok((
            status,
            global,
            crate::LiteinstBackendStatsSource::from_ptrace_host_hybrid(stats.snapshot()),
        ))
    }

    /// Runs a Tool under the ptrace-owned LiteInst hybrid and captures output.
    ///
    /// The same single-process/single-thread and non-security-boundary contract
    /// as [`Self::run_host_with_preload`] applies.
    // TODO-HUMAN-REVIEW(PR-270): Review public host-hybrid output API.
    pub async fn run_host_with_output_and_preload<T>(
        mut command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
    ) -> Result<(ReverieOutput, T::GlobalState), Error>
    where
        T: Tool + 'static,
    {
        command
            .stdout(ReverieStdio::piped())
            .stderr(ReverieStdio::piped());
        let preload = configure_host_command(&mut command, preload.into())?;
        TracerBuilder::<T>::new(command)
            .config(config)
            .liteinst_runtime(
                preload,
                crate::runtime::HOST_BEGIN_MARKER,
                crate::runtime::HOST_READY_MARKER,
                crate::runtime::HOST_HELPER_RETURN_MARKER,
                crate::runtime::HOST_SYSCALL_MARKER,
            )
            .spawn()
            .await?
            .wait_with_output()
            .await
    }

    /// Runs the ptrace-owned LiteInst hybrid, captures output, and returns typed statistics.
    pub async fn run_host_with_output_and_preload_and_stats<T>(
        mut command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
    ) -> Result<
        (
            ReverieOutput,
            T::GlobalState,
            crate::LiteinstBackendStatsSource,
        ),
        Error,
    >
    where
        T: Tool + 'static,
    {
        command
            .stdout(ReverieStdio::piped())
            .stderr(ReverieStdio::piped());
        let preload = configure_host_command(&mut command, preload.into())?;
        let tracer = TracerBuilder::<T>::new(command)
            .config(config)
            .liteinst_runtime_with_stats(
                preload,
                crate::runtime::HOST_BEGIN_MARKER,
                crate::runtime::HOST_READY_MARKER,
                crate::runtime::HOST_HELPER_RETURN_MARKER,
                crate::runtime::HOST_SYSCALL_MARKER,
                BackendStatsRequest::ENABLED,
            )
            .spawn()
            .await?;
        let stats = tracer
            .liteinst_instrumentation_stats()
            .expect("LiteInst runtime tracer must expose instrumentation statistics");
        let (output, global) = tracer.wait_with_output().await?;
        Ok((
            output,
            global,
            crate::LiteinstBackendStatsSource::from_ptrace_host_hybrid(stats.snapshot()),
        ))
    }

    /// Runs a tool using an explicit tool-specific preload library.
    ///
    /// This path dispatches patchable syscalls in the guest and keeps the
    /// `GlobalTool` in this coordinator. The coordinator drains inherited RPC
    /// connections to follow process-like fork/clone3 descendants without
    /// attaching ptrace. Vfork is translated to a COW child and supports the
    /// child-exit completion boundary. Thread clone, exec rebootstrap, and
    /// unpatchable-site fallback remain unsupported.
    pub async fn run_with_preload<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
    ) -> Result<(ExitStatus, T::GlobalState), Error>
    where
        T: Tool + 'static,
    {
        let (wait, global, stats) = launch::<T>(
            command,
            config,
            preload.into(),
            false,
            None,
            BackendStatsRequest::DISABLED,
        )
        .await?;
        debug_assert!(stats.is_none());
        match wait {
            ChildWait::Status(status) => Ok((status.into(), global)),
            ChildWait::Output(_) => unreachable!("status run returned captured output"),
        }
    }

    /// Runs an in-guest Tool and aggregates one typed statistics snapshot per process.
    pub async fn run_with_preload_and_stats<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
    ) -> Result<
        (
            ExitStatus,
            T::GlobalState,
            crate::LiteinstBackendStatsSource,
        ),
        Error,
    >
    where
        T: Tool + 'static,
    {
        let (wait, global, stats) = launch::<T>(
            command,
            config,
            preload.into(),
            false,
            None,
            BackendStatsRequest::ENABLED,
        )
        .await?;
        let stats = stats.expect("enabled LiteInst run must return statistics");
        match wait {
            ChildWait::Status(status) => Ok((status.into(), global, stats)),
            ChildWait::Output(_) => unreachable!("status run returned captured output"),
        }
    }

    /// Runs a tool and captures the guest's stdout and stderr.
    pub async fn run_with_output_and_preload<T>(
        mut command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
    ) -> Result<(Output, T::GlobalState), Error>
    where
        T: Tool + 'static,
    {
        command.stdout(reverie::process::Stdio::piped());
        command.stderr(reverie::process::Stdio::piped());
        let (wait, global, stats) = launch::<T>(
            command,
            config,
            preload.into(),
            true,
            None,
            BackendStatsRequest::DISABLED,
        )
        .await?;
        debug_assert!(stats.is_none());
        match wait {
            ChildWait::Output(output) => Ok((output, global)),
            ChildWait::Status(_) => unreachable!("output run returned only a status"),
        }
    }

    /// Runs an in-guest Tool with captured output and per-process statistics.
    pub async fn run_with_output_and_preload_and_stats<T>(
        mut command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
    ) -> Result<(Output, T::GlobalState, crate::LiteinstBackendStatsSource), Error>
    where
        T: Tool + 'static,
    {
        command.stdout(reverie::process::Stdio::piped());
        command.stderr(reverie::process::Stdio::piped());
        let (wait, global, stats) = launch::<T>(
            command,
            config,
            preload.into(),
            true,
            None,
            BackendStatsRequest::ENABLED,
        )
        .await?;
        let stats = stats.expect("enabled LiteInst run must return statistics");
        match wait {
            ChildWait::Output(output) => Ok((output, global, stats)),
            ChildWait::Status(_) => unreachable!("output run returned only a status"),
        }
    }

    // TODO-HUMAN-REVIEW(PR-139): Review the public tool-specific bootstrap API.
    /// Runs a tool with captured output and opaque constructor bootstrap bytes.
    pub async fn run_with_output_and_preload_data<T>(
        mut command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
        tool_data: impl Into<Vec<u8>>,
    ) -> Result<(Output, T::GlobalState), Error>
    where
        T: Tool + 'static,
    {
        command.stdout(reverie::process::Stdio::piped());
        command.stderr(reverie::process::Stdio::piped());
        let (wait, global, stats) = launch::<T>(
            command,
            config,
            preload.into(),
            true,
            Some(tool_data.into()),
            BackendStatsRequest::DISABLED,
        )
        .await?;
        debug_assert!(stats.is_none());
        match wait {
            ChildWait::Output(output) => Ok((output, global)),
            ChildWait::Status(_) => unreachable!("output run returned only a status"),
        }
    }

    // TODO-HUMAN-REVIEW(PR-152): Review inherited-stdio tool bootstrap support.
    /// Runs a tool with inherited guest stdio and opaque constructor bootstrap bytes.
    ///
    /// The returned [`Output`] contains the guest status and empty byte buffers.
    /// This is useful for tools that share the launcher's output sink and must
    /// preserve ordering between intercepted and pass-through guest writes.
    pub async fn run_with_inherited_stdio_and_preload_data<T>(
        mut command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
        preload: impl Into<PathBuf>,
        tool_data: impl Into<Vec<u8>>,
    ) -> Result<(Output, T::GlobalState), Error>
    where
        T: Tool + 'static,
    {
        inherit_stdio(&mut command);
        let (wait, global, stats) = launch::<T>(
            command,
            config,
            preload.into(),
            true,
            Some(tool_data.into()),
            BackendStatsRequest::DISABLED,
        )
        .await?;
        debug_assert!(stats.is_none());
        match wait {
            ChildWait::Output(output) => {
                debug_assert!(output.stdout.is_empty());
                debug_assert!(output.stderr.is_empty());
                Ok((output, global))
            }
            ChildWait::Status(_) => unreachable!("output run returned only a status"),
        }
    }
}

fn configure_host_command(command: &mut Command, preload: PathBuf) -> io::Result<PathBuf> {
    if !preload.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("LiteInst host runtime {} is not a file", preload.display()),
        ));
    }
    let preload = preload.canonicalize()?;
    let mut ld_preload = preload.clone().into_os_string();
    if let Some(existing) = command
        .get_env("LD_PRELOAD")
        .or_else(|| std::env::var_os("LD_PRELOAD").map(Into::into))
        .filter(|value| !value.is_empty())
    {
        ld_preload.push(OsStr::new(":"));
        let existing: &OsStr = existing.as_ref();
        ld_preload.push(existing);
    }
    command
        .env("LD_PRELOAD", ld_preload)
        .env(crate::runtime::HOST_RUNTIME_ENV, "1");
    Ok(preload)
}

fn effective_command_env(command: &Command, key: &OsStr) -> Option<OsString> {
    command.get_captured_envs().remove(key)
}

fn configure_in_guest_command_preload(command: &mut Command, preload: PathBuf) {
    let mut ld_preload = preload.into_os_string();
    if let Some(existing) =
        effective_command_env(command, OsStr::new("LD_PRELOAD")).filter(|value| !value.is_empty())
    {
        ld_preload.push(OsStr::new(":"));
        ld_preload.push(existing);
    }
    command.env("LD_PRELOAD", ld_preload);
}

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
        let preload = tool_preload_path()?;
        Self::run_with_preload::<T>(command, config, preload).await
    }

    async fn run_with_stats<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
    ) -> Result<(ExitStatus, T::GlobalState, Self::Stats), Error>
    where
        T: Tool + 'static,
    {
        let preload = tool_preload_path()?;
        let (status, global, stats) =
            Self::run_with_preload_and_stats::<T>(command, config, preload).await?;
        Ok((status, global, stats.backend_stats()))
    }

    async fn run_with_output<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
    ) -> Result<(ReverieOutput, T::GlobalState, Self::Stats), Error>
    where
        T: Tool + 'static,
    {
        // The preload path is resolved here rather than taken as a parameter:
        // it is a LiteInst mechanism, not part of the backend-agnostic
        // contract. See the `Backend` trait docs, "Why `preload` is
        // deliberately not on this trait".
        let preload = tool_preload_path()?;
        let (output, global, stats) =
            Self::run_with_output_and_preload_and_stats::<T>(command, config, preload).await?;
        // This family of LiteInst entry points predates the trait and reports
        // `std::process::Output`; the trait speaks Reverie's own `Output` so
        // that the captured status is the same `reverie::ExitStatus` that
        // `run` and `run_with_stats` return. Preserve the wait status's core
        // dump bit rather than using the older infallible conversion, which
        // cannot distinguish signal termination with and without a core dump.
        let output = ReverieOutput {
            status: ExitStatus::from_raw(output.status.into_raw()),
            stdout: output.stdout,
            stderr: output.stderr,
        };
        Ok((output, global, stats.backend_stats()))
    }
}

enum ChildWait {
    Status(std::process::ExitStatus),
    Output(Output),
}

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

async fn launch<T>(
    command: Command,
    config: <T::GlobalState as GlobalTool>::Config,
    preload: PathBuf,
    capture_output: bool,
    tool_data: Option<Vec<u8>>,
    stats_request: BackendStatsRequest,
) -> Result<
    (
        ChildWait,
        T::GlobalState,
        Option<crate::LiteinstBackendStatsSource>,
    ),
    Error,
>
where
    T: Tool + 'static,
{
    let (wait, global, stats, _) = launch_logged::<T>(
        command,
        config,
        preload,
        capture_output,
        tool_data,
        stats_request,
        None,
    )
    .await?;
    Ok((wait, global, stats))
}

async fn launch_logged<T>(
    mut command: Command,
    config: <T::GlobalState as GlobalTool>::Config,
    preload: PathBuf,
    capture_output: bool,
    tool_data: Option<Vec<u8>>,
    stats_request: BackendStatsRequest,
    log_limit: Option<usize>,
) -> Result<
    (
        ChildWait,
        T::GlobalState,
        Option<crate::LiteinstBackendStatsSource>,
        Option<crate::CapturedGuestLog>,
    ),
    Error,
>
where
    T: Tool + 'static,
{
    if !preload.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("tool preload {} is not a file", preload.display()),
        )
        .into());
    }

    let preload = preload.canonicalize()?;
    let arg0 = command.get_arg0().to_owned();
    let program = command.find_program()?;
    command.program(program).arg0(arg0);

    let directory = tempfile::Builder::new()
        .prefix("reverie-liteinst-")
        .tempdir_in("/tmp")?;
    let socket = directory.path().join("coordinator.sock");
    let global = Arc::new(T::GlobalState::init_global_state(&config).await);
    let connected = Arc::new(AtomicBool::new(false));
    let server = RpcServer::bind_with_connection_readiness(
        &socket,
        global.clone(),
        config,
        connected.clone(),
    )
    .map_err(|error| io::Error::other(error.to_string()))?;
    let mut connection_monitors = vec![server.connection_monitor()];
    let (stats_global, stats_server, stats_socket) = if stats_request.is_enabled() {
        let socket = directory.path().join("stats.sock");
        let global = Arc::new(crate::stats::LiteinstStatsGlobal::default());
        let server = RpcServer::bind(&socket, global.clone(), ())
            .map_err(|error| io::Error::other(error.to_string()))?;
        connection_monitors.push(server.connection_monitor());
        (Some(global), Some(server), Some(socket))
    } else {
        (None, None, None)
    };

    configure_in_guest_address_space(&mut command);
    configure_in_guest_command_preload(&mut command, preload);

    let mut log_task = None;
    let mut log = None;
    let log_exited = Arc::new(AtomicBool::new(false));

    let wait = match tool_data {
        Some(tool_data) => {
            let mut child_command = command.try_into_std()?;
            child_command.env_remove(STATS_COORDINATOR_ENV);
            if let Some(stats_socket) = &stats_socket {
                child_command.env(STATS_COORDINATOR_ENV, stats_socket);
            }

            let log_pair = log_limit
                .map(|_| crate::guest_log::channel_pair())
                .transpose()?;
            let log_fd = log_pair.as_ref().map(|(_, guest)| guest.as_raw_fd());
            let bootstrap = if let Some(fd) = log_fd {
                create_logged_preload_bootstrap(&socket, &tool_data, Some(fd))?
            } else {
                create_preload_bootstrap(&socket, &tool_data)?
            };
            let bootstrap_fd = bootstrap.as_raw_fd();
            unsafe {
                child_command.pre_exec(move || {
                    if libc::fcntl(bootstrap_fd, libc::F_SETFD, 0) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    if let Some(fd) = log_fd
                        && libc::fcntl(fd, libc::F_SETFD, 0) == -1
                    {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let log_guest = if let Some((host, guest)) = log_pair {
                let limit = log_limit.expect("log requested");
                let exited = log_exited.clone();
                let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
                log_task = Some(tokio::task::spawn_blocking(move || {
                    let _ = ready_tx.send(());
                    crate::guest_log::collect(host, limit, &exited)
                }));
                ready_rx
                    .recv()
                    .map_err(|error| io::Error::other(error.to_string()))?;
                Some(guest)
            } else {
                None
            };
            let mut child = child_command.spawn()?;
            drop(bootstrap);
            drop(log_guest);
            let wait = tokio::task::spawn_blocking(move || {
                let result = if capture_output {
                    child.wait_with_output().map(ChildWait::Output)
                } else {
                    child.wait().map(ChildWait::Status)
                };
                log_exited.store(true, Ordering::Release);
                result
            });
            serve_rpc_until(server, stats_server, connection_monitors, async {
                let result = wait
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))?;
                if let Some(task) = log_task.take() {
                    log = Some(
                        task.await
                            .map_err(|error| io::Error::other(error.to_string()))?,
                    );
                }
                result
            })
            .await?
        }
        None => {
            let mut child_command = command.try_into_std()?;
            child_command.env(COORDINATOR_ENV, &socket);
            child_command.env_remove(STATS_COORDINATOR_ENV);
            if let Some(stats_socket) = &stats_socket {
                child_command.env(STATS_COORDINATOR_ENV, stats_socket);
            }
            let mut child = child_command.spawn()?;
            let wait = tokio::task::spawn_blocking(move || {
                if capture_output {
                    child.wait_with_output().map(ChildWait::Output)
                } else {
                    child.wait().map(ChildWait::Status)
                }
            });
            serve_rpc_until(server, stats_server, connection_monitors, async move {
                wait.await
                    .map_err(|error| io::Error::other(error.to_string()))?
            })
            .await?
        }
    };
    if !connected.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "LiteInst guest exited before connecting to the coordinator; static executables and loader failures are unsupported",
        )
        .into());
    }
    let global = unwrap_global_after_connections(global).await?;
    let stats = match stats_global {
        Some(stats) => Some(unwrap_global_after_connections(stats).await?.into_source()),
        None => None,
    };
    Ok((wait, global, stats, log))
}

fn tool_preload_path() -> io::Result<PathBuf> {
    let path = std::env::var_os(TOOL_PRELOAD_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, TOOL_PRELOAD_ENV))?;
    if path.is_file() {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{TOOL_PRELOAD_ENV}={} is not a file", path.display()),
        ))
    }
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

    #[test]
    fn effective_command_environment_honors_override_remove_and_clear() {
        let ambient_path = std::env::var_os("PATH").expect("test process must have PATH");
        assert!(!ambient_path.is_empty());

        let mut command = Command::new("/bin/true");
        command.env("PATH", "/caller/bin");
        assert_eq!(
            effective_command_env(&command, OsStr::new("PATH")),
            Some(OsString::from("/caller/bin"))
        );

        command.env_remove("PATH");
        assert_eq!(effective_command_env(&command, OsStr::new("PATH")), None);

        let mut cleared = Command::new("/bin/true");
        cleared.env_clear();
        assert_eq!(effective_command_env(&cleared, OsStr::new("PATH")), None);
    }

    #[test]
    fn in_guest_preload_override_remove_and_clear_win_over_ambient() {
        const CHILD_ENV: &str = "REVERIE_LITEINST_PRELOAD_ENV_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "backend::tests::in_guest_preload_override_remove_and_clear_win_over_ambient",
                    "--test-threads=1",
                ])
                .env(CHILD_ENV, "1")
                .env("LD_PRELOAD", "libc.so.6")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "child test failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        assert_eq!(std::env::var_os("LD_PRELOAD"), Some("libc.so.6".into()));

        let mut command = Command::new("/bin/true");
        command.env("LD_PRELOAD", "/caller/tool.so");
        configure_in_guest_command_preload(&mut command, PathBuf::from("/liteinst/runtime.so"));
        let preload = command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("LD_PRELOAD"))
            .and_then(|(_, value)| value);
        assert_eq!(
            preload,
            Some(OsStr::new("/liteinst/runtime.so:/caller/tool.so"))
        );

        let mut removed = Command::new("/bin/true");
        removed.env_remove("LD_PRELOAD");
        configure_in_guest_command_preload(&mut removed, PathBuf::from("/liteinst/runtime.so"));
        assert_eq!(
            effective_command_env(&removed, OsStr::new("LD_PRELOAD")),
            Some(OsString::from("/liteinst/runtime.so"))
        );

        let mut cleared = Command::new("/bin/true");
        cleared.env_clear();
        configure_in_guest_command_preload(&mut cleared, PathBuf::from("/liteinst/runtime.so"));
        assert_eq!(
            effective_command_env(&cleared, OsStr::new("LD_PRELOAD")),
            Some(OsString::from("/liteinst/runtime.so"))
        );
    }
}
