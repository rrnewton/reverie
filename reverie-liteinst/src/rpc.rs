//! Coordinator RPC adapter for in-guest Reverie tools.

use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicI32;
use core::sync::atomic::Ordering;
use std::io;
use std::mem::ManuallyDrop;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Pid;
// The async-signal-safe spinlock is shared across the in-guest tool hosts.
pub(crate) use reverie_preload::sync::SpinMutex;
use reverie_preload::trap::raw_syscall6;
use reverie_rpc_transport::BlockingRpcClient;
use reverie_rpc_transport::mapped::MappedStream;

mod mapped_setup;
pub use mapped_setup::InstalledCoordinator;
pub use mapped_setup::InstalledSetupListener;
pub use mapped_setup::InstalledStreams;
pub use mapped_setup::MappedCoordinator;
pub use mapped_setup::MappedSetupListener;

/// Set in a freshly forked child by [`note_fork_in_child`]. The guest RPC hot
/// path consults this flag instead of issuing a `getpid` syscall on every hop,
/// so an ordinary (non-forking) round-trip performs no identity syscalls at all.
static FORKED_SINCE_LAST_RPC: AtomicBool = AtomicBool::new(false);

/// Record that this process is a freshly forked child whose inherited
/// coordinator connection still belongs to the parent.
///
/// The tool host calls this from `finish_fork_child`, which is driven by
/// *syscall interception* rather than libc. That placement is load-bearing for
/// two reasons:
///
///   - It observes every supported fork, including a raw `SYS_fork` or a raw
///     plain `SYS_clone` issued without libc (Go's runtime, hand-written
///     `syscall(2)` call sites). A `pthread_atfork` child handler would see
///     only forks that went through glibc's `fork()` wrapper and would silently
///     leave such a child on the parent's connection.
///   - It runs before the child's `handle_thread_start` callback. A
///     `pthread_atfork` handler runs later, inside the libc wrapper after the
///     fork syscall returns. This historical socket point does not precede
///     Tool-future capture destruction; the mapped path instead completes its
///     replacement immediately after the physical fork.
///
/// The actual reconnect happens lazily on the child's next
/// [`CoordinatorRpc::send_rpc`]; this is only a flag store, so it stays safe in
/// the restricted post-fork context.
pub(crate) fn note_fork_in_child() {
    FORKED_SINCE_LAST_RPC.store(true, Ordering::Release);
}

struct RpcConnection<G: GlobalTool> {
    pid: Pid,
    process_identity: u64,
    client: RpcClient<G>,
    statistics: Option<BlockingRpcClient<crate::stats::LiteinstStatsGlobal, MappedStream>>,
}

enum RpcClient<G: GlobalTool> {
    Socket(BlockingRpcClient<G>),
    Mapped(BlockingRpcClient<G, MappedStream>),
}

/// Owned across exactly the raw fork result boundary. No implicit Drop may
/// close an inherited mapped endpoint before child disarming. The caller must
/// complete parent/error restoration or child replacement without unwinding.
pub(crate) struct PreparedFork<G: GlobalTool> {
    connection: ManuallyDrop<RpcConnection<G>>,
    logs: Option<crate::installed_log::PreparedLogs>,
}

// TODO-HUMAN-REVIEW(PR-326): Review the common blocking
// transport and fork-child reconnect used by LiteInst's synchronous Tool callback.
/// Blocking guest-side RPC handle backed by the common Reverie RPC transport.
///
/// The connection is process-local and reconnects after `fork`. Thread-style
/// clone remains rejected: [`BlockingRpcClient`] stamps its connect-time TID,
/// and Detcore requires one independently blocking connection per guest thread.
pub struct CoordinatorRpc<G: GlobalTool> {
    connection: SpinMutex<Option<RpcConnection<G>>>,
    mapped: Option<MappedCoordinator>,
    installed: Option<InstalledCoordinator>,
    setup_timeout: Duration,
    config: G::Config,
    path: PathBuf,
    fd: AtomicI32,
}

impl<G: GlobalTool> CoordinatorRpc<G> {
    pub(crate) fn raw_fd(&self) -> libc::c_int {
        self.fd.load(Ordering::Acquire)
    }

    /// Construct an explicitly selected mapped runtime connection.
    ///
    /// # Safety
    /// Call before application threads or filters, with a trusted setup owner
    /// satisfying MappedSetupListener::accept's contract. The runtime must keep
    /// one guest thread, intercept every admitted process fork and complete the
    /// ownership protocol below before any child use. This connection does not
    /// own host tasks or process lifetime and is not a launcher switch.
    pub unsafe fn connect_mapped(endpoint: MappedCoordinator) -> io::Result<Self> {
        // The initial Arc, as well as child replacements, must be allocated in
        // the reusable private Tool heap. System allocations made before the
        // filter cannot be safely deallocated while a guest allocator is paused.
        let pid = current_id(libc::SYS_getpid)?;
        let tid = current_id(libc::SYS_gettid)?;
        let stream = {
            let _allocation = crate::patch_alloc::enter_dispatch();
            unsafe { endpoint.connect(pid, tid) }
        }?;
        // Preserve the original root config allocation context and capacity.
        // Only runtime setup and Mapping/Arc metadata use the Tool heap here.
        let client: BlockingRpcClient<G, MappedStream> =
            BlockingRpcClient::from_connected_stream(stream, tid)
                .map_err(|error| io::Error::other(error.to_string()))?;
        let config = client.config().clone();
        Ok(Self {
            connection: SpinMutex::new(Some(RpcConnection {
                pid,
                process_identity: 0,
                client: RpcClient::Mapped(client),
                statistics: None,
            })),
            mapped: Some(endpoint),
            installed: None,
            setup_timeout: Duration::ZERO,
            config,
            path: PathBuf::new(),
            fd: AtomicI32::new(-1),
        })
    }

    /// Connect the complete installed endpoint set before Tool construction.
    ///
    /// # Safety
    /// The caller satisfies InstalledSetupListener's trusted mapping, host
    /// access, child ownership and single-thread fork contracts. Call once before
    /// application threads/filters. Metadata is allocated in the reusable Tool
    /// heap; arbitrary root Config decoding/cloning retains its original heap.
    /// New-config construction may log after descriptor closure, but recursively
    /// using an as-yet-unpublished RPC client remains unsupported.
    pub unsafe fn connect_installed(
        endpoint: InstalledCoordinator,
        setup_timeout: Duration,
        blocked_publication: Duration,
    ) -> io::Result<Self> {
        if setup_timeout.is_zero()
            || std::time::Instant::now()
                .checked_add(setup_timeout)
                .is_none()
        {
            return Err(io::Error::other("invalid installed setup deadline"));
        }
        let deadline = std::time::Instant::now() + setup_timeout;
        let pid = current_id(libc::SYS_getpid)?;
        let tid = current_id(libc::SYS_gettid)?;
        let set = {
            let _allocation = crate::patch_alloc::enter_dispatch();
            unsafe { endpoint.connect(pid, tid, [1, 1], None, deadline) }
        }?;
        let installation = crate::installed_log::install(
            [set.public, set.private],
            pid.as_raw(),
            blocked_publication,
        )?;
        let client: BlockingRpcClient<G, MappedStream> =
            BlockingRpcClient::from_connected_stream_until(set.main, tid, deadline)
                .map_err(|error| io::Error::other(error.to_string()))?;
        let statistics = set
            .statistics
            .map(|stream| {
                BlockingRpcClient::from_connected_stream_until(stream, tid, deadline)
                    .map_err(|error| io::Error::other(error.to_string()))
            })
            .transpose()?;
        installed_setup_deadline(deadline)?;
        let config = client.config().clone();
        installed_setup_deadline(deadline)?;
        let rpc = Self {
            connection: SpinMutex::new(Some(RpcConnection {
                pid,
                process_identity: set.process_identity,
                client: RpcClient::Mapped(client),
                statistics,
            })),
            mapped: None,
            installed: Some(endpoint),
            setup_timeout,
            config,
            path: PathBuf::new(),
            fd: AtomicI32::new(-1),
        };
        installation.complete();
        Ok(rpc)
    }

    pub(crate) fn installed_statistics(&self) -> Option<bool> {
        self.installed
            .as_ref()
            .map(InstalledCoordinator::statistics_enabled)
    }

    /// Explicitly retire the two process clients after Tool exit callbacks,
    /// statistics submission and producer FINISH. Static cached Config lifetime
    /// is unchanged; an inner Config cannot run a recursive destructor after
    /// its connection has been retired. Retain it until physical process exit.
    pub(crate) fn finish_installed(&self) {
        if self.installed.is_none() {
            return;
        }
        crate::installed_log::finish();
        let connection = self
            .connection
            .lock()
            .take()
            .unwrap_or_else(|| rpc_fatal(123));
        let RpcClient::Mapped(client) = connection.client else {
            rpc_fatal(123)
        };
        let (_, config, stream) = client.into_parts();
        let _config = ManuallyDrop::new(config);
        drop(stream);
        if let Some(client) = connection.statistics {
            let (_, (), stream) = client.into_parts();
            drop(stream);
        }
    }

    /// Take ownership while the parent's synchronization owner can still run.
    /// The single-threaded ToolHost calls this after state migration, with no
    /// callback or allocator work between this return and the physical fork.
    pub(crate) fn prepare_fork(&self) -> Option<PreparedFork<G>> {
        if self.mapped.is_none() && self.installed.is_none() {
            return None;
        }
        let connection = self
            .connection
            .lock()
            .take()
            .unwrap_or_else(|| rpc_fatal(123));
        Some(PreparedFork {
            connection: ManuallyDrop::new(connection),
            logs: self
                .installed
                .as_ref()
                .map(|_| crate::installed_log::prepare_fork()),
        })
    }

    pub(crate) fn restore_fork_parent(&self, prepared: PreparedFork<G>, result: i64) {
        let connection = ManuallyDrop::into_inner(prepared.connection);
        let mut slot = self.connection.lock();
        if slot.is_some() {
            rpc_fatal(123);
        }
        *slot = Some(connection);
        drop(slot);
        if let Some(logs) = prepared.logs {
            crate::installed_log::restore_parent(logs, result);
        }
    }

    /// # Safety
    /// This is the actual COW child after prepare_fork on its sole admitted
    /// guest thread. No alias or concurrent/reentrant mapped use can exist:
    /// imports are private, never converted to async and never export abort
    /// handles. Tool dispatch has entered the reusable private allocator and
    /// its allocator lock is not held at the raw syscall boundary.
    pub(crate) unsafe fn complete_fork_child(&self, prepared: PreparedFork<G>) {
        let connection = ManuallyDrop::into_inner(prepared.connection);
        let parent = (connection.process_identity, connection.pid);
        let RpcClient::Mapped(client) = connection.client else {
            rpc_fatal(123)
        };
        let (_, retired_config, inherited) = client.into_parts();
        let retired_config = ManuallyDrop::new(retired_config);
        // Nothing fallible, allocating or callback-capable may go between
        // extraction and disarming: ordinary inherited Drop closes the parent.
        let released = unsafe { inherited.discard_inherited_after_fork() };
        if let Err(_pending) = released {
            // Retain the disarmed owner through actual process teardown. A
            // failed release is not cleanup success or a supported-fork refusal.
            rpc_fatal(123);
        }
        if let Some(statistics) = connection.statistics {
            let (_, (), inherited) = statistics.into_parts();
            // The optional statistics endpoint obeys the same physical fork.
            // Do not run ordinary inherited Drop, including on setup failure.
            if let Err(_pending) = unsafe { inherited.discard_inherited_after_fork() } {
                rpc_fatal(123);
            }
        }
        let pid = current_id(libc::SYS_getpid).unwrap_or_else(|_| rpc_fatal(122));
        let tid = current_id(libc::SYS_gettid).unwrap_or_else(|_| rpc_fatal(122));
        let deadline = self
            .installed
            .as_ref()
            .map(|_| std::time::Instant::now() + self.setup_timeout);
        let created = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (stream, statistics, process_identity) = if let Some(endpoint) = &self.installed {
                let logs = prepared.logs.unwrap_or_else(|| rpc_fatal(123));
                let set = unsafe {
                    endpoint.connect(
                        pid,
                        tid,
                        logs.incarnations,
                        Some(parent),
                        deadline.expect("installed setup deadline"),
                    )
                }?;
                crate::installed_log::complete_child(
                    logs,
                    [set.public, set.private],
                    pid.as_raw(),
                )?;
                (set.main, set.statistics, set.process_identity)
            } else {
                let endpoint = self.mapped.as_ref().unwrap_or_else(|| rpc_fatal(123));
                (unsafe { endpoint.connect(pid, tid) }?, None, 0)
            };
            let client = match deadline {
                Some(deadline) => {
                    BlockingRpcClient::from_connected_stream_until(stream, tid, deadline)
                }
                None => BlockingRpcClient::from_connected_stream(stream, tid),
            }
            .map_err(|error| io::Error::other(error.to_string()))?;
            let statistics = statistics
                .map(|stream| {
                    BlockingRpcClient::from_connected_stream_until(
                        stream,
                        tid,
                        deadline.expect("installed statistics deadline"),
                    )
                    .map_err(|error| io::Error::other(error.to_string()))
                })
                .transpose()?;
            if let Some(deadline) = deadline {
                installed_setup_deadline(deadline)?;
            }
            Ok::<_, io::Error>((client, statistics, process_identity))
        }));
        let (client, statistics, process_identity) = match created {
            Ok(Ok(client)) => client,
            Ok(Err(_error)) => rpc_fatal(123),
            Err(payload) => {
                // The panic hook already observed the original panic. Neither
                // its arbitrary payload nor the old Config may run Drop while
                // this connection is empty. Keep both owned until actual exit.
                let _payload = ManuallyDrop::new(payload);
                rpc_fatal(118);
            }
        };
        {
            let mut slot = self.connection.lock();
            if slot.is_some() {
                rpc_fatal(123);
            }
            *slot = Some(RpcConnection {
                pid,
                process_identity,
                client: RpcClient::Mapped(client),
                statistics,
            });
        }
        // The outer cached config was never moved or replaced. Its shared
        // borrows survive. Retiring only the inner config permits ordinary
        // destructor RPC now that the fresh client is installed and unlocked.
        drop(ManuallyDrop::into_inner(retired_config));
    }

    pub(crate) fn note_fork_in_child(&self) {
        if self.mapped.is_none() && self.installed.is_none() {
            note_fork_in_child();
        }
    }

    /// Connect before installing seccomp and decode the coordinator config.
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let pid = current_id(libc::SYS_getpid)?;
        let tid = current_id(libc::SYS_gettid)?;
        let client: BlockingRpcClient<G> = BlockingRpcClient::connect(&path, tid)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let config = client.config().clone();
        let fd = client.as_raw_fd();
        Ok(Self {
            connection: SpinMutex::new(Some(RpcConnection {
                pid,
                process_identity: 0,
                client: RpcClient::Socket(client),
                statistics: None,
            })),
            mapped: None,
            installed: None,
            setup_timeout: Duration::ZERO,
            config,
            path,
            fd: AtomicI32::new(fd),
        })
    }
}

impl<G: GlobalTool> crate::stats::MappedStatsRpc for CoordinatorRpc<G> {
    fn submit(&self, request: crate::stats::LiteinstProcessStats) -> io::Result<()> {
        let slot = self.connection.lock();
        let client = slot
            .as_ref()
            .and_then(|connection| connection.statistics.as_ref())
            .ok_or_else(|| io::Error::other("installed statistics client is not active"))?;
        client
            .try_send_rpc(request)
            .map_err(|error| io::Error::other(error.to_string()))
    }
}

#[reverie::tool]
impl<G: GlobalTool> GlobalRPC<G> for CoordinatorRpc<G> {
    async fn send_rpc(&self, message: G::Request) -> G::Response {
        let _runtime = (self.mapped.is_some() || self.installed.is_some())
            .then(crate::runtime::enter_mapped_runtime_io);
        let mut slot = self.connection.lock();
        let connection = slot.as_mut().unwrap_or_else(|| rpc_fatal(123));
        // Fork detection without a per-hop syscall: the common round-trip only
        // reads the atfork flag. It is set exclusively in a freshly forked
        // child, so `getpid`/`gettid` are issued only when a fork has actually
        // happened and the inherited connection may still belong to the parent.
        if self.mapped.is_none()
            && self.installed.is_none()
            && FORKED_SINCE_LAST_RPC.swap(false, Ordering::AcqRel)
        {
            let pid = current_id(libc::SYS_getpid).unwrap_or_else(|_| rpc_fatal(122));
            if connection.pid != pid {
                let tid = current_id(libc::SYS_gettid).unwrap_or_else(|_| rpc_fatal(122));
                let client =
                    BlockingRpcClient::connect(&self.path, tid).unwrap_or_else(|_| rpc_fatal(123));
                let new_fd = client.as_raw_fd();
                let old_fd = self.fd.swap(new_fd, Ordering::AcqRel);
                crate::runtime::replace_coordinator_fd(old_fd, new_fd)
                    .unwrap_or_else(|_| rpc_fatal(123));
                *connection = RpcConnection {
                    pid,
                    process_identity: 0,
                    client: RpcClient::Socket(client),
                    statistics: None,
                };
            }
        }
        let response = match &connection.client {
            RpcClient::Socket(client) => client.try_send_rpc(message),
            RpcClient::Mapped(client) => client.try_send_rpc_ref(&message),
        };
        // A mapped request remains caller-owned. Release endpoint and runtime
        // scopes before its argument is destroyed (including an arbitrary Drop
        // callback or a System allocation made outside Tool dispatch).
        drop(slot);
        drop(_runtime);
        match response {
            Ok(response) => response,
            Err(_) => rpc_fatal(123),
        }
    }

    fn config(&self) -> &G::Config {
        &self.config
    }
}

fn current_id(number: i64) -> io::Result<Pid> {
    let id = unsafe { raw_syscall6(number, [0; 6]) };
    if id <= 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(Pid::from_raw(id as i32))
    }
}

fn rpc_fatal(status: i32) -> ! {
    unsafe {
        let _ = raw_syscall6(libc::SYS_exit_group, [status as u64, 0, 0, 0, 0, 0]);
    }
    loop {
        core::hint::spin_loop();
    }
}

fn installed_setup_deadline(deadline: std::time::Instant) -> io::Result<()> {
    if std::time::Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}
