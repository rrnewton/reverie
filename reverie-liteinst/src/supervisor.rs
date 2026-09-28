/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! External creator and authenticated birth service for in-guest RCB clocks.
//! The generic Tool protocol is served on separate, independently owned streams.
//! This module is also compiled verbatim by the process fixture supervisor.

use std::collections::HashMap;
use std::fs::File;
use std::fs::OpenOptions;
use std::future::Future;
use std::io;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::pin::Pin;
use std::process::Child;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Output;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use reverie::GlobalTool;
use reverie::Tid;
use reverie_ptrace::DisabledRcbEvent;
use reverie_ptrace::RcbPmuProfile;
use reverie_rpc_transport::RpcError;
use tokio::io::unix::AsyncFd;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::control::Fd;
use super::control::Packet;
use super::control::Received;
use super::control::{self};

const SETUP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct Identity {
    pidfd: Fd,
    pid: u32,
    proc_number: u32,
    proc_dir: File,
    inode: u64,
    device: u64,
    start: u64,
    namespace: File,
    namespace_inode: u64,
    local_pid: u32,
}

impl Identity {
    fn capture(pidfd: Fd, local_pid: Option<u32>) -> io::Result<Self> {
        let info = control::pid_info(&pidfd)?;
        // fdinfo's Pid is in this procfs mount's namespace, which need not be
        // identical to the active namespace used by perf_event_open/GET_INFO.
        let fdinfo = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", pidfd.as_raw_fd()))?;
        let proc_number = field(&fdinfo, "Pid:")?
            .parse::<u32>()
            .map_err(|_| control::invalid("invalid pidfd proc identity"))?;
        if proc_number == 0 {
            return Err(control::invalid("zero pidfd proc identity"));
        }
        let path = format!("/proc/{proc_number}");
        let proc_dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(&path)?;
        let metadata = proc_dir.metadata()?;
        let start = start_time(&format!("/proc/self/fd/{}/stat", proc_dir.as_raw_fd()))?;
        let namespace = File::open(format!("/proc/self/fd/{}/ns/pid", proc_dir.as_raw_fd()))?;
        let namespace_inode = namespace.metadata()?.ino();
        let status =
            std::fs::read_to_string(format!("/proc/self/fd/{}/status", proc_dir.as_raw_fd()))?;
        let nspid = numbers(field(&status, "NSpid:")?)?;
        let own = std::fs::read_to_string("/proc/self/status")?;
        let own_ids = numbers(field(&own, "NSpid:")?)?;
        let index = own_ids
            .len()
            .checked_sub(1)
            .ok_or_else(|| control::invalid("empty creator namespace identity"))?;
        let actual_creator = unsafe { libc::getpid() } as u32;
        if own_ids[index] != actual_creator
            || nspid.get(index) != Some(&info.pid)
            || nspid.first() != Some(&proc_number)
        {
            return Err(control::invalid(
                "pidfd/proc namespace translation mismatch",
            ));
        }
        let observed_local = *nspid
            .last()
            .ok_or_else(|| control::invalid("empty target namespace identity"))?;
        if local_pid.is_some_and(|pid| pid != observed_local) {
            return Err(control::invalid(
                "guest-local PID disagrees with kernel namespace identity",
            ));
        }
        let value = Self {
            pidfd,
            pid: info.pid,
            proc_number,
            proc_dir,
            inode: metadata.ino(),
            device: metadata.dev(),
            start,
            namespace,
            namespace_inode,
            local_pid: observed_local,
        };
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> io::Result<control::PidInfo> {
        let info = control::pid_info(&self.pidfd)?;
        let current = std::fs::metadata(format!("/proc/{}", self.proc_number))?;
        if info.pid != self.pid
            || info.tgid != self.pid
            || current.ino() != self.inode
            || current.dev() != self.device
            || self.proc_dir.metadata()?.ino() != self.inode
            || start_time(&format!("/proc/self/fd/{}/stat", self.proc_dir.as_raw_fd()))?
                != self.start
            || self.namespace.metadata()?.ino() != self.namespace_inode
            || std::fs::metadata(format!(
                "/proc/self/fd/{}/ns/pid",
                self.proc_dir.as_raw_fd()
            ))?
            .ino()
                != self.namespace_inode
        {
            return Err(control::invalid(
                "target incarnation changed during disabled acquisition",
            ));
        }
        Ok(info)
    }
    fn check_message(&self, message: &Received) -> io::Result<()> {
        let current = self.validate()?;
        let creds = message
            .credentials
            .as_ref()
            .ok_or_else(|| control::invalid("missing kernel setup credentials"))?;
        if creds.pid <= 0
            || creds.pid as u32 != self.pid
            || creds.uid != current.ruid
            || creds.gid != current.rgid
        {
            return Err(control::invalid(
                "setup message belongs to another incarnation",
            ));
        }
        Ok(())
    }
    fn validate_cpu(&self, expected: u32) -> io::Result<()> {
        let mut words = [0_u64; 16];
        let result = unsafe {
            control::host_gate(
                libc::SYS_sched_getaffinity,
                [
                    u64::from(self.pid),
                    core::mem::size_of_val(&words) as u64,
                    words.as_mut_ptr() as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        if result < 0 {
            let errno = i32::try_from(-result).unwrap_or(libc::EIO);
            return Err(if errno == libc::EINVAL {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "kernel CPU affinity mask exceeds the authenticated 1024-bit wire format",
                )
            } else {
                io::Error::from_raw_os_error(errno)
            });
        }
        let mut selected = None;
        for (word_index, word) in words.into_iter().enumerate() {
            if word == 0 {
                continue;
            }
            if word.count_ones() != 1 || selected.is_some() {
                return Err(control::invalid(
                    "target CPU affinity changed during event acquisition",
                ));
            }
            selected = Some((word_index * 64 + word.trailing_zeros() as usize) as u32);
        }
        if selected == Some(expected) {
            Ok(())
        } else {
            Err(control::invalid(
                "target CPU differs from its captured PMU profile",
            ))
        }
    }
    fn terminate(&self) {
        // The held file, never a cached numeric PID, binds exceptional cleanup.
        unsafe {
            control::host_gate(
                libc::SYS_pidfd_send_signal,
                [
                    self.pidfd.as_raw_fd() as u64,
                    libc::SIGKILL as u64,
                    0,
                    0,
                    0,
                    0,
                ],
            );
        }
    }
}
fn field<'a>(text: &'a str, name: &str) -> io::Result<&'a str> {
    text.lines()
        .find_map(|line| line.strip_prefix(name))
        .map(str::trim)
        .ok_or_else(|| control::invalid("missing kernel identity field"))
}
fn numbers(text: &str) -> io::Result<Vec<u32>> {
    text.split_whitespace()
        .map(|v| {
            v.parse()
                .map_err(|_| control::invalid("invalid namespace PID"))
        })
        .collect()
}
fn start_time(path: &str) -> io::Result<u64> {
    let text = std::fs::read_to_string(path)?;
    text.rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .ok_or_else(|| control::invalid("missing task start identity"))?
        .parse()
        .map_err(|_| control::invalid("invalid task start identity"))
}

#[derive(Clone, Default)]
pub(crate) struct Monitor {
    count: Arc<AtomicUsize>,
    changed: Arc<Notify>,
}
impl Monitor {
    pub(crate) fn active(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
    pub(crate) async fn idle(&self) {
        loop {
            let notification = self.changed.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            if self.active() == 0 {
                return;
            }
            notification.await;
        }
    }
    fn enter(&self) -> Lease {
        self.count.fetch_add(1, Ordering::AcqRel);
        Lease(self.clone())
    }
}
struct Lease(Monitor);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.count.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}

// Struct fields drop in declaration order, including before the first poll.
// Every captured session/RPC resource is inside `future` and is destroyed
// before `lease` can announce idle. Independent async captures do not provide
// that order and must not replace this wrapper.
struct OwnedServiceFuture<F> {
    future: Pin<Box<F>>,
    _lease: Lease,
}
impl<F> OwnedServiceFuture<F> {
    fn new(future: F, lease: Lease) -> Self {
        Self {
            future: Box::pin(future),
            _lease: lease,
        }
    }
}
impl<F: Future> Future for OwnedServiceFuture<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().future.as_mut().poll(context)
    }
}

struct Shared<G: GlobalTool> {
    global: Arc<G>,
    config: G::Config,
    connected: Arc<AtomicBool>,
    next: AtomicU64,
    monitor: Monitor,
    targets: Mutex<HashMap<u64, Arc<Identity>>>,
    expected_cpu: u32,
    audit: Option<std::sync::mpsc::SyncSender<[u64; 10]>>,
}
impl<G: GlobalTool> Shared<G> {
    fn audit(&self, record: [u64; 10]) -> io::Result<()> {
        if let Some(sink) = &self.audit {
            sink.try_send(record).map_err(|error| {
                io::Error::other(format!("acquisition observation channel: {error}"))
            })?;
        }
        Ok(())
    }
    fn generation(&self) -> io::Result<u64> {
        self.next
            .try_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_add(1))
            .map_err(|_| control::invalid("setup generation exhausted"))
    }
}
impl<G: GlobalTool> Drop for Shared<G> {
    fn drop(&mut self) {
        for identity in self
            .targets
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            identity.terminate();
        }
    }
}

struct Pending {
    endpoint: Fd,
    generation: u64,
    birth: Birth,
    lease: Lease,
}
enum Birth {
    Root {
        identity: Arc<Identity>,
        acknowledged: std::sync::mpsc::SyncSender<()>,
    },
    Fork {
        parent: Arc<Identity>,
        parent_generation: u64,
    },
}

pub(crate) struct Supervisor<G: GlobalTool> {
    shared: Arc<Shared<G>>,
    root: Pending,
}
pub(crate) struct Waiter {
    child: Child,
    identity: Arc<Identity>,
    acknowledged: std::sync::mpsc::Receiver<()>,
}
pub(crate) enum WaitResult {
    Status(ExitStatus),
    Output(Output),
}
pub(crate) struct WaitReport {
    pub(crate) result: WaitResult,
    pub(crate) setup: io::Result<()>,
}
pub(crate) struct RunReport {
    pub(crate) wait: io::Result<WaitReport>,
    pub(crate) service: io::Result<()>,
}
impl Waiter {
    pub(crate) fn wait(mut self, capture: bool) -> io::Result<WaitReport> {
        // Drain actual pipes before the acquisition ACK. The Child remains
        // unreaped, so pipe pressure cannot deadlock its identity barrier.
        fn drain(
            pipe: Option<impl Read + Send + 'static>,
        ) -> Option<std::thread::JoinHandle<io::Result<Vec<u8>>>> {
            pipe.map(|mut pipe| {
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    pipe.read_to_end(&mut bytes)?;
                    Ok(bytes)
                })
            })
        }
        fn collect(
            reader: Option<std::thread::JoinHandle<io::Result<Vec<u8>>>>,
        ) -> io::Result<Vec<u8>> {
            match reader {
                Some(reader) => reader
                    .join()
                    .map_err(|_| io::Error::other("guest pipe reader panicked"))?,
                None => Ok(Vec::new()),
            }
        }
        let stdout = drain(self.child.stdout.take());
        let stderr = drain(self.child.stderr.take());
        let setup = self
            .acknowledged
            .recv_timeout(SETUP_TIMEOUT)
            .map_err(|error| {
                io::Error::other(format!(
                    "initial RCB acquisition was not acknowledged: {error}"
                ))
            });
        if setup.is_err() {
            self.identity.terminate();
        }
        let status = self.child.wait()?;
        let stdout = collect(stdout)?;
        let stderr = collect(stderr)?;
        let result = if capture {
            WaitResult::Output(Output {
                status,
                stdout,
                stderr,
            })
        } else {
            WaitResult::Status(status)
        };
        Ok(WaitReport { result, setup })
    }
}

impl<G: GlobalTool + 'static> Supervisor<G> {
    pub(crate) fn spawn(
        mut command: Command,
        coordinator: &std::path::Path,
        global: Arc<G>,
        config: G::Config,
        connected: Arc<AtomicBool>,
        expected_cpu: u32,
        audit: Option<std::sync::mpsc::SyncSender<[u64; 10]>>,
    ) -> io::Result<(Self, Waiter)> {
        let (host, child_endpoint) = control::pair(control::host_gate, false)?;
        let bootstrap = control::Bootstrap::new(child_endpoint, coordinator)?;
        bootstrap.configure(&mut command);
        let mut child = command.spawn()?;
        drop(bootstrap);
        // This is our still-unreaped direct child, not a client-supplied PID.
        let raw = match control::result(unsafe {
            control::host_gate(libc::SYS_pidfd_open, [u64::from(child.id()), 0, 0, 0, 0, 0])
        }) {
            Ok(fd) => fd,
            Err(error) => {
                let _ = child.kill();
                let actual = child.wait();
                return Err(io::Error::other(format!(
                    "initial child pidfd: {error}; actual wait={actual:?}"
                )));
            }
        };
        let pidfd = control::Fd::owned(
            unsafe { OwnedFd::from_raw_fd(raw as i32) },
            control::host_gate,
            false,
        )?;
        let identity = match Identity::capture(pidfd, None) {
            Ok(value) if value.pid == child.id() => Arc::new(value),
            Ok(value) => {
                value.terminate();
                let actual = child.wait();
                return Err(io::Error::other(format!(
                    "initial Child identity mismatch; actual wait={actual:?}"
                )));
            }
            Err(error) => {
                let _ = child.kill();
                let actual = child.wait();
                return Err(io::Error::other(format!(
                    "initial child identity: {error}; actual wait={actual:?}"
                )));
            }
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let monitor = Monitor::default();
        let shared = Arc::new(Shared {
            global,
            config,
            connected,
            next: AtomicU64::new(2),
            monitor: monitor.clone(),
            targets: Mutex::new(HashMap::new()),
            expected_cpu,
            audit,
        });
        let root = Pending {
            endpoint: host,
            generation: 1,
            birth: Birth::Root {
                identity: identity.clone(),
                acknowledged: tx,
            },
            lease: monitor.enter(),
        };
        Ok((
            Self { shared, root },
            Waiter {
                child,
                identity,
                acknowledged: rx,
            },
        ))
    }
    pub(crate) fn monitor(&self) -> Monitor {
        self.shared.monitor.clone()
    }
    /// Serve acquisition and ordinary RPC together, retain the direct child's
    /// actual wait independently of server errors, and drain pending births.
    pub(crate) async fn run(
        self,
        waiter: Waiter,
        capture: bool,
        mut serving: JoinSet<Result<(), RpcError>>,
        monitors: Vec<reverie_rpc_transport::ConnectionMonitor>,
    ) -> RunReport {
        let monitor = self.monitor();
        serving.spawn(self.serve());
        let mut wait = tokio::task::spawn_blocking(move || waiter.wait(capture));
        let mut actual = None;
        let mut lifetimes: Vec<_> = monitors
            .into_iter()
            .map(super::coordinator::Lifetime::Rpc)
            .collect();
        lifetimes.insert(0, super::coordinator::Lifetime::Birth(monitor.clone()));
        let completed_service = super::coordinator::run_tasks_until(
            serving,
            lifetimes,
            async {
                actual = Some((&mut wait).await);
                Ok(())
            },
            super::coordinator::DRAIN_TIMEOUT,
        )
        .await;
        let mut service = completed_service.result;
        // The driver has joined its direct tasks. Their nested JoinSets abort
        // on Drop; each pending/session/RPC future retains a lease until its
        // actual resources have dropped. Wait for that release before returning
        // the separately owned child wait, including an early server failure.
        // This shares the original drain deadline; it adds no second allowance.
        if tokio::time::timeout_at(completed_service.cleanup_deadline, monitor.idle())
            .await
            .is_err()
        {
            let message = format!(
                "supervisor retained {} service owner(s) after cancellation",
                monitor.active()
            );
            service = Err(match service {
                Ok(()) => io::Error::new(io::ErrorKind::TimedOut, message),
                Err(error) => io::Error::new(error.kind(), format!("{error}; {message}")),
            });
        }
        // Preserve the actual wait even if an independent server error won.
        let completed = match actual {
            Some(value) => value,
            None => wait.await,
        };
        let wait = completed
            .map_err(|error| io::Error::other(error.to_string()))
            .and_then(|result| result);
        RunReport { wait, service }
    }
    pub(crate) async fn serve(self) -> Result<(), RpcError> {
        let (sender, mut incoming) = mpsc::unbounded_channel();
        let mut tasks = JoinSet::new();
        spawn_session(&mut tasks, self.shared.clone(), self.root, sender.clone());
        loop {
            tokio::select! {
                pending = incoming.recv() => {
                    let pending = pending.ok_or_else(|| control::invalid("birth service channel closed"))?;
                    spawn_session(&mut tasks, self.shared.clone(), pending, sender.clone());
                }
                result = tasks.join_next(), if !tasks.is_empty() => {
                    match result {
                        Some(Ok(Ok(()))) => {},
                        Some(Ok(Err(error))) => return Err(error.into()),
                        Some(Err(error)) => return Err(io::Error::other(error.to_string()).into()),
                        None => return Err(control::invalid("birth task disappeared").into()),
                    }
                }
            }
        }
    }
}

async fn receive(socket: &AsyncFd<Fd>, setup: bool) -> io::Result<Received> {
    let operation = async {
        loop {
            let mut ready = socket.readable().await?;
            match ready.try_io(|socket| control::receive(socket.get_ref(), false, true)) {
                Ok(result) => return result,
                Err(_) => continue,
            }
        }
    };
    if setup {
        tokio::time::timeout(SETUP_TIMEOUT, operation)
            .await
            .map_err(|_| io::Error::from_raw_os_error(libc::ETIMEDOUT))?
    } else {
        operation.await
    }
}
async fn send(socket: &AsyncFd<Fd>, packet: Packet, rights: &[i32]) -> io::Result<()> {
    tokio::time::timeout(SETUP_TIMEOUT, async {
        loop {
            let mut ready = socket.writable().await?;
            match ready.try_io(|socket| control::send(socket.get_ref(), packet, rights, true)) {
                Ok(result) => return result,
                Err(_) => continue,
            }
        }
    })
    .await
    .map_err(|_| io::Error::from_raw_os_error(libc::ETIMEDOUT))?
}

struct TargetGuard<G: GlobalTool> {
    shared: Arc<Shared<G>>,
    generation: u64,
    identity: Arc<Identity>,
    armed: bool,
}
impl<G: GlobalTool> Drop for TargetGuard<G> {
    fn drop(&mut self) {
        self.shared
            .targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.generation);
        if self.armed {
            self.identity.terminate();
        }
    }
}

fn spawn_session<G: GlobalTool + 'static>(
    tasks: &mut JoinSet<io::Result<()>>,
    shared: Arc<Shared<G>>,
    pending: Pending,
    sender: mpsc::UnboundedSender<Pending>,
) {
    let Pending {
        endpoint,
        generation,
        birth,
        lease,
    } = pending;
    tasks.spawn(OwnedServiceFuture::new(
        session(shared, endpoint, generation, birth, sender),
        lease,
    ));
}

async fn session<G: GlobalTool + 'static>(
    shared: Arc<Shared<G>>,
    endpoint: Fd,
    generation: u64,
    birth: Birth,
    sender: mpsc::UnboundedSender<Pending>,
) -> io::Result<()> {
    let socket = AsyncFd::new(endpoint)?;
    let hello = match receive(&socket, true).await {
        Ok(message) => message,
        // Closing a prepared child endpoint without HELLO is cancellation after
        // an actual failed fork. It does not become an authenticated child.
        Err(error)
            if error.kind() == io::ErrorKind::UnexpectedEof
                && matches!(birth, Birth::Fork { .. }) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    hello.packet.require(control::HELLO, 0)?;
    let credentials = hello
        .credentials
        .ok_or_else(|| control::invalid("HELLO has no kernel credentials"))?;
    let local_pid =
        u32::try_from(hello.packet.0[4]).map_err(|_| control::invalid("HELLO PID overflow"))?;
    if local_pid == 0 || hello.packet.0[5] != u64::from(local_pid) {
        return Err(control::invalid(
            "nonleader HELLO is not an admitted Tool thread",
        ));
    }
    let (_, pidfd) = hello.only_fd()?;
    let identity = Arc::new(Identity::capture(pidfd, Some(local_pid))?);
    let current = identity.validate()?;
    if credentials.pid <= 0
        || credentials.pid as u32 != identity.pid
        || credentials.uid != current.ruid
        || credentials.gid != current.rgid
    {
        return Err(control::invalid("HELLO pidfd/credentials mismatch"));
    }
    match &birth {
        Birth::Root {
            identity: expected, ..
        } => {
            expected.validate()?;
            if identity.pid != expected.pid
                || identity.start != expected.start
                || identity.inode != expected.inode
                || identity.namespace_inode != expected.namespace_inode
            {
                return Err(control::invalid("HELLO is not the launched Child"));
            }
        }
        Birth::Fork {
            parent,
            parent_generation,
        } => {
            if *parent_generation == 0
                || identity.pid == parent.pid
                || generation == *parent_generation
            {
                return Err(control::invalid(
                    "fork reservation claimed by parent/replayed incarnation",
                ));
            }
            // The parent need not still be alive: this exact private endpoint
            // was registered before its trusted fork. No PPid snapshot or
            // post-fork parent acknowledgement is the birth authority.
        }
    }
    shared
        .targets
        .lock()
        .map_err(|_| control::invalid("target registry poisoned"))?
        .insert(generation, identity.clone());
    let mut target_guard = TargetGuard {
        shared: shared.clone(),
        generation,
        identity: identity.clone(),
        armed: true,
    };
    let (host_stream, guest_stream) = std::os::unix::net::UnixStream::pair()?;
    host_stream.set_nonblocking(true)?;
    guest_stream.set_read_timeout(Some(SETUP_TIMEOUT))?;
    guest_stream.set_write_timeout(Some(SETUP_TIMEOUT))?;
    let mut offered = Packet::new(control::RPC, generation);
    offered.0[4] = u64::from(identity.pid);
    offered.0[5] = u64::from(identity.local_pid);
    send(&socket, offered, &[guest_stream.as_raw_fd()]).await?;
    drop(guest_stream);
    let stream = tokio::net::UnixStream::from_std(host_stream)?;
    let mut rpc = JoinSet::new();
    let rpc_lease = shared.monitor.enter();
    let rpc_global = shared.global.clone();
    let rpc_config = shared.config.clone();
    rpc.spawn(OwnedServiceFuture::new(
        reverie_rpc_transport::serve_stream(rpc_global, rpc_config, stream),
        rpc_lease,
    ));
    let acquire = tokio::select! {
        message = receive(&socket, true) => message?,
        result = rpc.join_next() => return Err(io::Error::other(format!("RPC config handshake ended before acquisition: {result:?}"))),
    };
    acquire.no_fds()?;
    acquire.packet.require(control::ACQUIRE, generation)?;
    identity.check_message(&acquire)?;
    let target_cpu = u32::try_from(acquire.packet.0[4])
        .map_err(|_| control::invalid("target CPU overflow"))?;
    if target_cpu != shared.expected_cpu {
        return Err(control::invalid(
            "target CPU differs from the launcher-selected CPU",
        ));
    }
    let requested_profile = match acquire.packet.0[5] {
        0 if acquire.packet.0[6] == 0 && acquire.packet.0[7] == 0 => None,
        1 => Some(
            RcbPmuProfile::from_parts(
                u32::try_from(acquire.packet.0[6])
                    .map_err(|_| control::invalid("target event type overflow"))?,
                acquire.packet.0[7],
            )
            .ok_or_else(|| control::invalid("target supplied a non-RCB event type"))?,
        ),
        _ => return Err(control::invalid("invalid target PMU disposition")),
    };
    // This bracket is deliberately numeric and disabled. A target which exits
    // inside perf_event_open can cause discarded speculative construction; it
    // can never produce an accepted/enabled event for a replacement PID.
    identity.validate()?;
    identity.validate_cpu(target_cpu)?;
    #[allow(unused_mut)]
    let mut false_unsupported = false;
    #[cfg(feature = "rcb-qualification")]
    {
        false_unsupported = std::env::var_os("REVERIE_LITEINST_TEST_RCB_PROFILE_FAULT")
            .as_deref()
            == Some(std::ffi::OsStr::new("unsupported"));
    }
    let event = if let Some(profile) = requested_profile.filter(|_| !false_unsupported) {
        let event = match DisabledRcbEvent::for_thread_on_cpu(
            Tid::from_raw(identity.pid as i32),
            target_cpu,
            profile,
        ) {
            Ok(event) => event,
            Err(error) => {
                let mut failure = Packet::new(control::ERROR, generation);
                failure.0[4] = error.into_raw() as u64;
                let sent = send(&socket, failure, &[]).await;
                return Err(io::Error::other(format!(
                    "external perf_event_open failed: {error}; error response={sent:?}"
                )));
            }
        };
        identity.validate()?;
        identity.validate_cpu(target_cpu)?;
        let (event_fd, description) = event.into_parts();
        let parent = match &birth {
            Birth::Root { .. } => 0,
            Birth::Fork {
                parent_generation, ..
            } => *parent_generation,
        };
        shared.audit([
            1,
            generation,
            parent,
            u64::from(identity.pid),
            u64::from(identity.local_pid),
            description.event_id,
            u64::from(description.event_type),
            description.config,
            u64::from(target_cpu),
            0,
        ])?;
        let mut offered = Packet::new(control::EVENT, generation);
        offered.0[4] = description.event_id;
        #[allow(unused_mut)]
        let mut offered_type = description.event_type;
        #[allow(unused_mut)]
        let mut offered_config = description.config;
        #[cfg(feature = "rcb-qualification")]
        {
            if std::env::var_os("REVERIE_LITEINST_TEST_RCB_PROFILE_FAULT").as_deref()
                == Some(std::ffi::OsStr::new("wrong-event"))
            {
                offered_type ^= 1;
                offered_config ^= 1;
            }
        }
        offered.0[5] = u64::from(offered_type);
        offered.0[6] = offered_config;
        offered.0[7] = u64::from(description.version);
        send(&socket, offered, &[event_fd.as_raw_fd()]).await?;
        Some((event_fd, description.event_id))
    } else {
        // Only the target's once-captured profile can negotiate absence in
        // production. The supervisor never performs CPUID on its own CPU. The
        // feature-gated corrupt-offer control deliberately violates the target
        // decision and requires the root client to refuse. Every actual open,
        // affinity, transport, identity, mmap, ACK or control failure is fatal.
        send(
            &socket,
            Packet::new(control::UNSUPPORTED_CPU, generation),
            &[],
        )
        .await?;
        None
    };
    let ack = receive(&socket, true).await?;
    ack.no_fds()?;
    ack.packet.require(control::ACK, generation)?;
    identity.check_message(&ack)?;
    if ack.packet.0[4] != event.as_ref().map_or(0, |(_, id)| *id) {
        return Err(control::invalid("counter ACK event ID mismatch"));
    }
    drop(event); // Only the transfer reference; never enable/reset/disable.
    shared.connected.store(true, Ordering::Release);
    if let Birth::Root { acknowledged, .. } = birth {
        let _ = acknowledged.send(());
    }
    send(&socket, Packet::new(control::ACK, generation), &[]).await?;
    loop {
        let message = tokio::select! {
            message = receive(&socket, false) => match message { Ok(message) => message, Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => { target_guard.armed = false; return Ok(()); }, Err(error) => return Err(error) },
            result = rpc.join_next() => return match result { Some(Ok(Ok(()))) => { target_guard.armed = false; Ok(()) }, other => Err(io::Error::other(format!("ordinary RPC stream ended: {other:?}"))) },
        };
        message.no_fds()?;
        #[cfg(feature = "rcb-qualification")]
        if message.packet.0[1] == control::RUNNING_SIGNAL_PROBE {
            message
                .packet
                .require(control::RUNNING_SIGNAL_PROBE, generation)?;
            identity.check_message(&message)?;
            identity.validate_cpu(target_cpu)?;
            for signal in [libc::SIGWINCH, libc::SIGSYS] {
                let result = unsafe {
                    control::host_gate(
                        libc::SYS_tgkill,
                        [
                            u64::from(identity.pid),
                            u64::from(identity.pid),
                            signal as u64,
                            0,
                            0,
                            0,
                        ],
                    )
                };
                if result != 0 {
                    return Err(io::Error::from_raw_os_error(
                        i32::try_from(-result).unwrap_or(libc::EIO),
                    ));
                }
            }
            send(&socket, Packet::new(control::ACK, generation), &[]).await?;
            continue;
        }
        message.packet.require(control::PREPARE_FORK, generation)?;
        identity.check_message(&message)?;
        let child_generation = shared.generation()?;
        let (host, child) = control::pair(control::host_gate, false)?;
        let prepared = Pending {
            endpoint: host,
            generation: child_generation,
            birth: Birth::Fork {
                parent: identity.clone(),
                parent_generation: generation,
            },
            lease: shared.monitor.enter(),
        };
        sender
            .send(prepared)
            .map_err(|_| control::invalid("birth service stopped before registration"))?;
        let mut packet = Packet::new(control::FORK_CHANNEL, generation);
        packet.0[4] = child_generation;
        send(&socket, packet, &[child.as_raw_fd()]).await?;
        // The parent receives a real independent endpoint. It is closed there
        // before ordinary parent execution; the child claims it exactly once.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct NeverPolled {
        monitor: Monitor,
        dropped_while_owned: Arc<AtomicBool>,
        _stream: std::os::unix::net::UnixStream,
    }
    impl Future for NeverPolled {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
            panic!("unpolled cancellation control was unexpectedly polled");
        }
    }
    impl Drop for NeverPolled {
        fn drop(&mut self) {
            self.dropped_while_owned
                .store(self.monitor.active() == 1, Ordering::Release);
        }
    }
    #[test]
    fn unpolled_service_drops_resources_before_announcing_idle() {
        let monitor = Monitor::default();
        let observed = Arc::new(AtomicBool::new(false));
        let (stream, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let resource = NeverPolled {
            monitor: monitor.clone(),
            dropped_while_owned: observed.clone(),
            _stream: stream,
        };
        let future = OwnedServiceFuture::new(resource, monitor.enter());
        assert_eq!(monitor.active(), 1);
        drop(future);
        assert!(
            observed.load(Ordering::Acquire),
            "lease was released before actual unpolled resources"
        );
        assert_eq!(monitor.active(), 0);
        let byte = [0_u8];
        let sent = unsafe {
            control::host_gate(
                libc::SYS_sendto,
                [
                    peer.as_raw_fd() as u64,
                    byte.as_ptr() as u64,
                    1,
                    libc::MSG_NOSIGNAL as u64,
                    0,
                    0,
                ],
            )
        };
        assert_eq!(
            sent,
            -i64::from(libc::EPIPE),
            "owned stream must actually be closed"
        );
    }
}
