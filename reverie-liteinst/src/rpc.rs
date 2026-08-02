//! Coordinator RPC adapter for in-guest Reverie tools.

use core::cell::UnsafeCell;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering;
use std::collections::HashMap;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Pid;
use reverie_preload::trap::raw_syscall6;
use reverie_rpc_transport::BlockingRpcClient;

pub(crate) struct SpinMutex<T> {
    held: AtomicBool,
    value: UnsafeCell<T>,
}

impl<T> SpinMutex<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            held: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub(crate) fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SpinGuard { mutex: self }
    }

    fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

unsafe impl<T: Send> Sync for SpinMutex<T> {}

pub(crate) struct SpinGuard<'a, T> {
    mutex: &'a SpinMutex<T>,
}

impl<T> core::ops::Deref for SpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.value.get() }
    }
}

impl<T> core::ops::DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.held.store(false, Ordering::Release);
    }
}

struct RpcConnections<G: GlobalTool> {
    pid: Pid,
    clients: HashMap<i32, Arc<BlockingRpcClient<G>>>,
}

// TODO-HUMAN-REVIEW(PR-326): Review the common blocking
// transport and fork-child reconnect used by LiteInst's synchronous Tool callback.
/// Blocking guest-side RPC handle backed by the common Reverie RPC transport.
///
/// Connections are process-local, reconnect after `fork`, and are independent
/// per guest thread. The connection-map lock is never held across a blocking
/// request: Detcore may delay one thread's response until another guest thread
/// reaches the coordinator.
pub struct CoordinatorRpc<G: GlobalTool> {
    connections: SpinMutex<RpcConnections<G>>,
    config: G::Config,
    path: PathBuf,
    initial_fd: libc::c_int,
}

impl<G: GlobalTool> CoordinatorRpc<G> {
    pub(crate) fn raw_fd(&self) -> libc::c_int {
        self.initial_fd
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
        let mut clients = HashMap::new();
        clients.insert(tid.as_raw(), Arc::new(client));
        Ok(Self {
            connections: SpinMutex::new(RpcConnections { pid, clients }),
            config,
            path,
            initial_fd: fd,
        })
    }

    fn client_for(&self, pid: Pid, tid: Pid) -> io::Result<Arc<BlockingRpcClient<G>>> {
        let mut connections = self.connections.lock();
        if connections.pid != pid {
            let client = Arc::new(connect_client::<G>(&self.path, tid)?);
            crate::runtime::reset_coordinator_fds_after_fork(client.as_raw_fd())?;
            connections.clients.clear();
            connections.pid = pid;
            connections.clients.insert(tid.as_raw(), client.clone());
            return Ok(client);
        }

        if let Some(client) = connections.clients.get(&tid.as_raw()) {
            return Ok(client.clone());
        }

        let client = Arc::new(connect_client::<G>(&self.path, tid)?);
        crate::runtime::reserve_coordinator_fd(client.as_raw_fd())?;
        connections.clients.insert(tid.as_raw(), client.clone());
        Ok(client)
    }
}

impl<G: GlobalTool> Drop for CoordinatorRpc<G> {
    fn drop(&mut self) {
        for client in self.connections.get_mut().clients.values() {
            crate::runtime::release_coordinator_fd(client.as_raw_fd());
        }
    }
}

#[reverie::tool]
impl<G: GlobalTool> GlobalRPC<G> for CoordinatorRpc<G> {
    async fn send_rpc(&self, message: G::Request) -> G::Response {
        let pid = current_id(libc::SYS_getpid).unwrap_or_else(|_| rpc_fatal(122));
        let tid = current_id(libc::SYS_gettid).unwrap_or_else(|_| rpc_fatal(122));
        let client = self.client_for(pid, tid).unwrap_or_else(|_| rpc_fatal(123));
        match client.try_send_rpc(message) {
            Ok(response) => response,
            Err(_) => rpc_fatal(123),
        }
    }

    fn config(&self) -> &G::Config {
        &self.config
    }
}

fn connect_client<G: GlobalTool>(path: &Path, tid: Pid) -> io::Result<BlockingRpcClient<G>> {
    BlockingRpcClient::connect(path, tid).map_err(|error| io::Error::other(error.to_string()))
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

#[cfg(test)]
mod tests {
    use core::future::Future;
    use core::pin::pin;
    use core::sync::atomic::AtomicBool;
    use core::sync::atomic::Ordering;
    use core::task::Context;
    use core::task::Poll;
    use core::task::Waker;
    use std::os::fd::AsRawFd;
    use std::sync::Arc;
    use std::time::Duration;
    use std::time::Instant;

    use reverie::GlobalRPC;
    use reverie::GlobalTool;
    use reverie::Tid;
    use reverie_rpc_transport::RpcServer;

    use super::CoordinatorRpc;
    use super::current_id;

    #[derive(Default)]
    struct Gate {
        waiting: AtomicBool,
        release: tokio::sync::Notify,
    }

    #[reverie::global_tool]
    impl GlobalTool for Gate {
        type Request = u8;
        type Response = i32;
        type Config = ();

        async fn receive_rpc(&self, from: Tid, request: u8) -> i32 {
            match request {
                0 => {
                    let released = self.release.notified();
                    self.waiting.store(true, Ordering::Release);
                    released.await;
                }
                1 => self.release.notify_one(),
                _ => panic!("unknown gate request"),
            }
            from.as_raw()
        }
    }

    fn send_rpc(rpc: &CoordinatorRpc<Gate>, request: u8) -> i32 {
        let mut future = pin!(rpc.send_rpc(request));
        let mut context = Context::from_waker(Waker::noop());
        match Future::poll(future.as_mut(), &mut context) {
            Poll::Ready(response) => response,
            Poll::Pending => panic!("blocking coordinator RPC returned Pending"),
        }
    }

    #[test]
    fn independent_thread_connections_release_blocked_rpc() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("coordinator.sock");
        let server_socket = socket.clone();
        let global = Arc::new(Gate::default());
        let server_global = global.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);

        let server_thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let server = RpcServer::bind(&server_socket, server_global, ()).unwrap();
                    ready_tx.send(()).unwrap();
                    tokio::try_join!(server.serve_one(), server.serve_one(), server.serve_one(),)
                        .map(|_| ())
                })
        });
        ready_rx.recv().unwrap();

        let rpc = Arc::new(CoordinatorRpc::<Gate>::connect(&socket).unwrap());
        crate::runtime::reserve_coordinator_fd(rpc.raw_fd()).unwrap();

        let (wait_tx, wait_rx) = std::sync::mpsc::sync_channel(0);
        let wait_rpc = rpc.clone();
        let wait_thread = std::thread::spawn(move || {
            let tid = current_id(libc::SYS_gettid).unwrap().as_raw();
            wait_tx.send((tid, send_rpc(&wait_rpc, 0))).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while !global.waiting.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "first thread RPC did not reach the coordinator"
            );
            std::thread::yield_now();
        }

        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let release_rpc = rpc.clone();
        let release_thread = std::thread::spawn(move || {
            let tid = current_id(libc::SYS_gettid).unwrap().as_raw();
            release_tx.send((tid, send_rpc(&release_rpc, 1))).unwrap();
        });

        let (release_tid, release_response) = release_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("second thread connection could not release the blocked RPC");
        assert_eq!(release_response, release_tid);
        let (wait_tid, wait_response) = wait_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first thread RPC remained blocked after release");
        assert_eq!(wait_response, wait_tid);
        assert_ne!(wait_tid, release_tid);

        wait_thread.join().unwrap();
        release_thread.join().unwrap();

        let client_fds = {
            let connections = rpc.connections.lock();
            assert_eq!(connections.clients.len(), 3);
            connections
                .clients
                .values()
                .map(|client| client.as_raw_fd())
                .collect::<Vec<_>>()
        };
        for &fd in &client_fds {
            assert!(crate::runtime::coordinator_fd_is_reserved(fd as u64));
        }

        drop(rpc);
        assert!(server_thread.join().unwrap().is_ok());
        for fd in client_fds {
            assert!(!crate::runtime::coordinator_fd_is_reserved(fd as u64));
        }
    }
}
