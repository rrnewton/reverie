//! Coordinator RPC adapter for in-guest Reverie tools.

use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicI32;
use core::sync::atomic::Ordering;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Pid;
// The async-signal-safe spinlock is shared across the in-guest tool hosts.
pub(crate) use reverie_preload::sync::SpinMutex;
use reverie_preload::trap::raw_syscall6;
use reverie_rpc_transport::BlockingRpcClient;

pub(crate) mod resources;
mod stream;

#[cfg(test)]
pub(crate) mod tests;

thread_local! {
    static TRUSTED_RPC_FD: core::cell::Cell<i32> = const { core::cell::Cell::new(-1) };
}

pub(crate) struct ChannelIoGuard(i32);

impl ChannelIoGuard {
    pub(crate) fn enter(fd: i32) -> Self {
        Self(TRUSTED_RPC_FD.replace(fd))
    }
}

impl Drop for ChannelIoGuard {
    fn drop(&mut self) {
        TRUSTED_RPC_FD.set(self.0);
    }
}

pub(crate) fn allows_channel_io(number: i64, fd: i32) -> bool {
    fd >= 0
        && TRUSTED_RPC_FD.get() == fd
        && matches!(
            number,
            libc::SYS_read
                | libc::SYS_readv
                | libc::SYS_write
                | libc::SYS_writev
                | libc::SYS_sendto
                | libc::SYS_recvfrom
                | libc::SYS_sendmsg
                | libc::SYS_recvmsg
        )
}

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
///   - It runs before the child's `handle_thread_start` callback, which is the
///     child's first opportunity to issue an RPC. A `pthread_atfork` handler
///     runs later, inside the libc wrapper after the fork syscall returns, so a
///     tool that sends an RPC from `handle_thread_start` would be attributed to
///     the parent.
///
/// The actual reconnect happens lazily on the child's next
/// [`CoordinatorRpc::send_rpc`]; this is only a flag store, so it stays safe in
/// the restricted post-fork context.
pub(crate) fn note_fork_in_child() {
    FORKED_SINCE_LAST_RPC.store(true, Ordering::Release);
}

struct RpcConnection<G: GlobalTool> {
    pid: Pid,
    client: BlockingRpcClient<G, stream::Stream>,
    publication: Arc<AtomicBool>,
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
    config: G::Config,
    path: PathBuf,
    fd: AtomicI32,
    pid: AtomicI32,
    tid: AtomicI32,
    resources: resources::Resources,
}

pub(crate) trait BoundRpc<G: GlobalTool>: GlobalRPC<G> + Send {
    fn identity(&self) -> (Pid, Pid);
}

impl<G: GlobalTool> BoundRpc<G> for CoordinatorRpc<G> {
    fn identity(&self) -> (Pid, Pid) {
        self.identity()
    }
}

impl<G: GlobalTool> CoordinatorRpc<G> {
    pub(crate) fn raw_fd(&self) -> libc::c_int {
        self.fd.load(Ordering::Acquire)
    }

    /// Connect before installing seccomp and decode the coordinator config.
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::connect_with_resources(path.as_ref(), resources::Resources::new(), false)
    }

    pub(crate) fn connect_bootstrap(
        path: &Path,
        resources: resources::Resources,
    ) -> io::Result<Self> {
        Self::connect_with_resources(path, resources, true)
    }

    fn connect_with_resources(
        path: &Path,
        resources: resources::Resources,
        bootstrap: bool,
    ) -> io::Result<Self> {
        let path = path.to_path_buf();
        let pid = current_id(libc::SYS_getpid)?;
        let tid = current_id(libc::SYS_gettid)?;
        let lease = resources.acquire()?;
        let publication = Arc::new(AtomicBool::new(false));
        let stream = if bootstrap {
            stream::Stream::bootstrap(&path, publication.clone(), lease)?
        } else {
            stream::Stream::connected(
                std::os::unix::net::UnixStream::connect(&path)?,
                publication.clone(),
                lease,
            )
        };
        let fd = stream.as_raw_fd();
        let client: BlockingRpcClient<G, stream::Stream> =
            BlockingRpcClient::from_connected_stream(stream, tid)
                .map_err(|error| io::Error::other(error.to_string()))?;
        let config = client.config().clone();
        Ok(Self {
            connection: SpinMutex::new(Some(RpcConnection {
                pid,
                client,
                publication,
            })),
            config,
            path,
            fd: AtomicI32::new(fd),
            pid: AtomicI32::new(pid.as_raw()),
            tid: AtomicI32::new(tid.as_raw()),
            resources,
        })
    }

    pub(crate) fn resources(&self) -> resources::Resources {
        self.resources.clone()
    }

    pub(crate) fn retire(&self) -> io::Result<()> {
        let connection = self.connection.lock().take();
        let connection = connection
            .ok_or_else(|| io::Error::other("RPC connection already retired"))?;
        self.fd.store(-1, Ordering::Release);
        drop(connection);
        Ok(())
    }

    pub(crate) fn identity(&self) -> (Pid, Pid) {
        (
            Pid::from_raw(self.pid.load(Ordering::Acquire)),
            Pid::from_raw(self.tid.load(Ordering::Acquire)),
        )
    }

    pub(crate) fn reconnect_after_fork(&self) {
        let mut connection = self.connection.lock();
        if FORKED_SINCE_LAST_RPC.swap(false, Ordering::AcqRel) {
            let pid = current_id(libc::SYS_getpid).unwrap_or_else(|_| rpc_fatal(122));
            if connection.as_ref().unwrap_or_else(|| rpc_fatal(122)).pid != pid {
                let tid = current_id(libc::SYS_gettid).unwrap_or_else(|_| rpc_fatal(122));
                let lease = self
                    .resources
                    .acquire()
                    .unwrap_or_else(|_| rpc_fatal(123));
                let publication = Arc::new(AtomicBool::new(false));
                let stream = stream::Stream::connected(
                    std::os::unix::net::UnixStream::connect(&self.path)
                        .unwrap_or_else(|_| rpc_fatal(123)),
                    publication.clone(),
                    lease,
                );
                let new_fd = stream.as_raw_fd();
                let client = BlockingRpcClient::from_connected_stream(stream, tid)
                    .unwrap_or_else(|_| rpc_fatal(123));
                let old_fd = self.fd.swap(new_fd, Ordering::AcqRel);
                crate::runtime::replace_coordinator_fd(old_fd, new_fd)
                    .unwrap_or_else(|_| rpc_fatal(123));
                let old = connection.replace(RpcConnection {
                    pid,
                    client,
                    publication,
                });
                old.as_ref()
                    .unwrap()
                    .publication
                    .store(false, Ordering::Release);
                connection
                    .as_ref()
                    .unwrap()
                    .publication
                    .store(true, Ordering::Release);
                self.pid.store(pid.as_raw(), Ordering::Release);
                self.tid.store(tid.as_raw(), Ordering::Release);
                drop(connection);
                drop(old);
            }
        }
    }
}

#[reverie::tool]
impl<G: GlobalTool> GlobalRPC<G> for CoordinatorRpc<G> {
    async fn send_rpc(&self, message: G::Request) -> G::Response {
        let _runtime = crate::runtime_domain::Entry::enter();
        if FORKED_SINCE_LAST_RPC.load(Ordering::Acquire) {
            self.reconnect_after_fork();
        }
        let connection = self.connection.lock();
        let connection = connection.as_ref().unwrap_or_else(|| rpc_fatal(123));
        #[cfg(test)]
        crate::runtime_domain::tests::at(crate::runtime_domain::tests::RPC);
        // Fork detection without a per-hop syscall: the common round-trip only
        // reads the atfork flag. It is set exclusively in a freshly forked
        // child, so `getpid`/`gettid` are issued only when a fork has actually
        // happened and the inherited connection may still belong to the parent.
        let _channel = ChannelIoGuard::enter(self.raw_fd());
        match connection.client.try_send_rpc(message) {
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
