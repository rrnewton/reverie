/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Real child/socket controls. These prove transport facts, not Tool installation
//! or observed PID reuse. Child isolation bounds process-global fixture changes.

use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;
use std::os::unix::net::UnixDatagram;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use reverie::GlobalTool;
use reverie::Tid;
use serde::Deserialize;
use serde::Serialize;

use super::peer_pidfd;
use crate::BlockingRpcClient;
use crate::InitialRootReadyMonitor;
use crate::RpcServer;
use crate::codec;
use crate::read_config_frame;

const CHILD_MODE: &str = "REVERIE_ROOT_READY_CHARACTERIZATION_CHILD";
const CONFIG: &str = "root-ready characterization config";
const IO_BOUND: Duration = Duration::from_secs(2);
const CASE_BOUND: Duration = Duration::from_secs(10);
const CLEANUP_BOUND: Duration = Duration::from_secs(2);
static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Counting {
    calls: AtomicUsize,
    last_from: AtomicI32,
    last_request: AtomicU64,
}

#[async_trait]
impl GlobalTool for Counting {
    type Config = String;
    type Request = u64;
    type Response = u64;

    async fn receive_rpc(&self, from: Tid, request: u64) -> u64 {
        self.last_from.store(from.as_raw(), Ordering::SeqCst);
        self.last_request.store(request, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        request
    }
}

#[derive(Serialize, Deserialize)]
enum Role {
    RawControl,
    Script(Vec<Step>, i32),
    Isolated,
    ConnectAndTransfer,
    WaitForRelease,
    Exit42AfterRelease,
}

#[derive(Serialize, Deserialize)]
enum Step {
    Connect,
    App(u64),
    Ready(bool),
    BorrowedReady(i32, bool),
    ClaimReady(i32, bool),
    Raw(Vec<u8>),
    Close,
    Hold,
}

#[derive(Serialize, Deserialize)]
struct Packet {
    role: Role,
    socket: PathBuf,
    aliases: PathBuf,
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("reverie-ready-{}-{sequence}", std::process::id()));
        // Never reuse or remove a pre-existing directory on a collision.
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("owned scratch cleanup failed: {error}");
            if !std::thread::panicking() {
                panic!("owned scratch cleanup failed");
            }
        }
    }
}

/// Own every real child until it has been reaped. No guessed numeric kill.
struct OwnedChild(Option<Child>);

impl OwnedChild {
    fn spawn(test: &str, packet: Packet) -> Self {
        let exact = format!("server::root_ready_characterization_tests::{test}");
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &exact, "--nocapture", "--test-threads=1"])
            .env(CHILD_MODE, test)
            .env("RUST_BACKTRACE", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut owned = Self(Some(child));
        let bytes = codec::encode(&packet).unwrap();
        assert!(bytes.len() < 4096);
        write_frame(owned.0.as_mut().unwrap().stdin.as_mut().unwrap(), &bytes);
        owned
    }

    fn id(&self) -> i32 {
        i32::try_from(self.0.as_ref().unwrap().id()).unwrap()
    }

    fn release(&mut self) {
        self.0
            .as_mut()
            .unwrap()
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"X")
            .unwrap();
    }

    async fn finish(&mut self) -> Output {
        let deadline = Instant::now() + CASE_BOUND;
        loop {
            if self.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                // try_wait has already observed/reaped the child. Only this
                // fixture writes its small captured output; no descendants
                // inherit the pipe and keep wait_with_output blocked.
                let output = self.0.take().unwrap().wait_with_output().unwrap();
                eprintln!(
                    "child status={} stdout={:?} stderr={:?}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return output;
            }
            assert!(Instant::now() < deadline, "owned child exceeded case bound");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    fn live_pidfd(&mut self) -> OwnedFd {
        let child = self.0.as_mut().unwrap();
        assert!(observe_exit(child.id()).is_none());
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0) };
        assert!(fd >= 0, "pidfd_open: {}", std::io::Error::last_os_error());
        let fd = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
        assert_eq!(pidfd_events(&fd), 0);
        fd
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let Some(child) = self.0.as_mut() else {
            return;
        };
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = child.kill();
        let deadline = Instant::now() + CLEANUP_BOUND;
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        eprintln!("owned child {} failed bounded kill/reap", child.id());
        // The containing test scope must remain RED; outer containment owns
        // final subtree cleanup if even SIGKILL cannot finish within this bound.
        if !std::thread::panicking() {
            panic!("owned child cleanup deadline exceeded");
        }
    }
}

fn write_frame(writer: &mut impl Write, bytes: &[u8]) {
    writer
        .write_all(&u32::try_from(bytes.len()).unwrap().to_be_bytes())
        .unwrap();
    writer.write_all(bytes).unwrap();
}

fn wait_for_release(input: &mut impl Read) {
    let mut byte = [0];
    input.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"X");
}

fn child_entry(test: &str) -> bool {
    let Some(selected) = std::env::var_os(CHILD_MODE) else {
        return false;
    };
    assert_eq!(selected, test);
    let mut input = std::io::stdin().lock();
    let bytes = read_config_frame(&mut input).unwrap();
    assert!(bytes.len() < 4096);
    let packet: Packet = codec::decode_exact(&bytes).unwrap();
    match packet.role {
        Role::Isolated => panic!("isolated fixture has its own entry point"),
        Role::Script(steps, status) => {
            wait_for_release(&mut input);
            let tid = Tid::from_raw(std::process::id() as i32);
            let mut client: Option<BlockingRpcClient<Counting>> = None;
            let mut original_fd = None;
            for step in steps {
                match step {
                    Step::Connect => {
                        assert!(client.is_none());
                        let stream = UnixStream::connect(&packet.socket).unwrap();
                        stream.set_read_timeout(Some(IO_BOUND)).unwrap();
                        stream.set_write_timeout(Some(IO_BOUND)).unwrap();
                        original_fd = Some(stream.as_raw_fd());
                        client =
                            Some(BlockingRpcClient::from_connected_stream(stream, tid).unwrap());
                    }
                    Step::App(value) => {
                        assert_eq!(client.as_ref().unwrap().try_send_rpc(value).unwrap(), value)
                    }
                    Step::Ready(expected) => {
                        let client = client.as_ref().unwrap();
                        assert_eq!(client.as_raw_fd(), original_fd.unwrap());
                        assert_eq!(client.declare_initial_root_ready().is_ok(), expected);
                        assert_eq!(client.as_raw_fd(), original_fd.unwrap());
                    }
                    Step::BorrowedReady(offset, expected) => {
                        let (_, config, mut stream) = client.take().unwrap().into_parts();
                        let borrowed = BlockingRpcClient::<Counting>::from_borrowed_stream(
                            &mut stream,
                            Tid::from_raw(tid.as_raw() + offset),
                            &config,
                        );
                        assert_eq!(borrowed.declare_initial_root_ready().is_ok(), expected);
                        assert_eq!(stream.as_raw_fd(), original_fd.unwrap());
                        assert_eq!(
                            unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) }
                                & libc::FD_CLOEXEC,
                            libc::FD_CLOEXEC
                        );
                    }
                    Step::ClaimReady(sender, expected) => {
                        let (_, config, mut stream) = client.take().unwrap().into_parts();
                        let borrowed = BlockingRpcClient::<Counting>::from_borrowed_stream(
                            &mut stream,
                            Tid::from_raw(sender),
                            &config,
                        );
                        assert_eq!(borrowed.declare_initial_root_ready().is_ok(), expected);
                    }
                    Step::Raw(bytes) => {
                        let (_, _, mut stream) = client.take().unwrap().into_parts();
                        write_frame(&mut stream, &bytes);
                        assert!(read_config_frame(&mut stream).is_err());
                    }
                    Step::Close => {
                        drop(client.take());
                    }
                    Step::Hold => wait_for_release(&mut input),
                }
            }
            drop(client);
            if status != 0 {
                std::process::exit(status);
            }
        }
        Role::RawControl => {
            let mut stream = UnixStream::connect(packet.socket).unwrap();
            stream.set_read_timeout(Some(IO_BOUND)).unwrap();
            stream.set_write_timeout(Some(IO_BOUND)).unwrap();
            let config = read_config_frame(&mut stream).unwrap();
            assert_eq!(codec::decode_exact::<String>(&config).unwrap(), CONFIG);
            let sender = i32::try_from(std::process::id()).unwrap();
            let mut control = vec![255, 255, 255, 255, b'R', b'I', 1, 0];
            control.extend_from_slice(&sender.to_le_bytes());
            assert_eq!(control.len(), 12);
            write_frame(&mut stream, &control);
            // Observation only. The parent checks server status and actual G
            // invocations; accepting EOF here cannot manufacture a test pass.
            println!("raw-control response={:?}", read_config_frame(&mut stream));
        }
        Role::ConnectAndTransfer => {
            let stream = UnixStream::connect(packet.socket).unwrap();
            let aliases = UnixDatagram::unbound().unwrap();
            aliases.connect(packet.aliases).unwrap();
            aliases.set_write_timeout(Some(IO_BOUND)).unwrap();
            send_alias(&aliases, stream.as_raw_fd());
            wait_for_release(&mut input);
            drop(stream);
        }
        Role::WaitForRelease => wait_for_release(&mut input),
        Role::Exit42AfterRelease => {
            wait_for_release(&mut input);
            std::process::exit(42);
        }
    }
    println!("root-ready-characterization-child:{test}:complete");
    true
}

// Existing pattern: tests/fixtures/guest_log_lifecycle_fixture.rs:912-968.
// This passes an actual socket reference; it does not synthesize credentials.
fn send_alias(socket: &UnixDatagram, fd: RawFd) {
    unsafe {
        let mut byte = b'A';
        let mut vector = libc::iovec {
            iov_base: (&mut byte as *mut u8).cast(),
            iov_len: 1,
        };
        let mut control = [0usize; 4];
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as _) as usize;
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as _) as usize;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd);
        assert_eq!(libc::sendmsg(socket.as_raw_fd(), &message, 0), 1);
    }
}

fn receive_alias(socket: &UnixDatagram) -> UnixStream {
    socket.set_read_timeout(Some(IO_BOUND)).unwrap();
    unsafe {
        let mut byte = 0u8;
        let mut vector = libc::iovec {
            iov_base: (&mut byte as *mut u8).cast(),
            iov_len: 1,
        };
        let mut control = [0usize; 4];
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&control);
        assert_eq!(
            libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC),
            1
        );
        assert_eq!(byte, b'A');
        assert_eq!(message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC), 0);
        let header = libc::CMSG_FIRSTHDR(&message);
        assert!(!header.is_null());
        assert_eq!((*header).cmsg_level, libc::SOL_SOCKET);
        assert_eq!((*header).cmsg_type, libc::SCM_RIGHTS);
        assert_eq!(
            (*header).cmsg_len,
            libc::CMSG_LEN(std::mem::size_of::<RawFd>() as _) as usize
        );
        let fd = std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<RawFd>());
        assert!(fd >= 0);
        let owned = OwnedFd::from_raw_fd(fd);
        assert!(libc::CMSG_NXTHDR(&message, header).is_null());
        UnixStream::from(owned)
    }
}

fn peer_pid(stream: &UnixStream) -> i32 {
    unsafe {
        let mut credentials: libc::ucred = std::mem::zeroed();
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        assert_eq!(
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length
            ),
            0
        );
        assert_eq!(length as usize, std::mem::size_of_val(&credentials));
        credentials.pid
    }
}

fn pidfd_events(fd: &OwnedFd) -> libc::c_short {
    let mut item = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut item, 1, 0) };
    assert!(
        result >= 0,
        "pidfd poll: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(result == 0, item.revents == 0);
    item.revents
}

/// Fixture observation only: never consumes the child's wait status.
fn observe_exit(pid: u32) -> Option<(i32, i32)> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    assert_eq!(result, 0, "waitid: {}", std::io::Error::last_os_error());
    let observed = unsafe { info.si_pid() };
    if observed == 0 {
        None
    } else {
        assert_eq!(observed as u32, pid);
        Some((info.si_code, unsafe { info.si_status() }))
    }
}

fn assert_exited(fd: &OwnedFd) {
    let events = pidfd_events(fd);
    assert_ne!(events & (libc::POLLIN | libc::POLLHUP), 0);
    assert_eq!(events & !(libc::POLLIN | libc::POLLHUP), 0);
}

fn accept_bounded(listener: &UnixListener) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + IO_BOUND;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "queued accept deadline exceeded");
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("accept failed: {error}"),
        }
    }
}

fn prove_endpoint_still_open(mut accepted: &UnixStream, held: &mut UnixStream) {
    accepted.set_write_timeout(Some(IO_BOUND)).unwrap();
    held.set_read_timeout(Some(IO_BOUND)).unwrap();
    accepted.write_all(b"H").unwrap();
    let mut byte = [0];
    held.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"H");
}

fn packet(role: Role, scratch: &Scratch) -> Packet {
    Packet {
        role,
        socket: scratch.path("rpc.sock"),
        aliases: scratch.path("alias.sock"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn reserved_root_ready_never_dispatches_application_rpc() {
    const NAME: &str = "reserved_root_ready_never_dispatches_application_rpc";
    if child_entry(NAME) {
        return;
    }
    let scratch = Scratch::new();
    let global = Arc::new(Counting::default());
    let application_ready = Arc::new(AtomicBool::new(false));
    let server = RpcServer::bind_with_readiness(
        scratch.path("rpc.sock"),
        global.clone(),
        CONFIG.to_owned(),
        application_ready.clone(),
    )
    .unwrap();
    let serving = tokio::spawn(async move {
        tokio::time::timeout(CASE_BOUND, server.serve_one())
            .await
            .expect("server exceeded characterization bound")
    });
    let mut child = OwnedChild::spawn(NAME, packet(Role::RawControl, &scratch));
    let sender = child.id().to_le_bytes();
    let expected_base_value =
        u64::from_le_bytes([b'R', b'I', 1, 0, sender[0], sender[1], sender[2], sender[3]]);
    let output = child.finish().await;
    let server_result = serving.await.unwrap();
    let calls = global.calls.load(Ordering::SeqCst);
    let from = global.last_from.load(Ordering::SeqCst);
    let request = global.last_request.load(Ordering::SeqCst);
    let ready = application_ready.load(Ordering::SeqCst);
    eprintln!(
        "base observation: server={server_result:?} calls={calls} from={from} request={request} expected_base_value={expected_base_value} app_ready={ready} admission=none"
    );
    assert!(output.status.success());
    if calls != 0 {
        // Pin the behavioral RED rather than crediting an unrelated failure.
        assert_eq!(calls, 1);
        assert_eq!(from, -1);
        assert_eq!(request, expected_base_value);
    }
    // Exact ec5 produced the retained calls=1/from=-1 behavioral RED.
    assert_eq!(calls, 0, "reserved control reached GlobalTool::receive_rpc");
    assert!(!ready, "reserved control counted as an application request");
    assert!(
        server_result.is_err(),
        "unconfigured ready control was accepted"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn exited_root_status_remains_waitable_after_nonreaping_observation() {
    const NAME: &str = "exited_root_status_remains_waitable_after_nonreaping_observation";
    if child_entry(NAME) {
        return;
    }
    let scratch = Scratch::new();
    let mut child = OwnedChild::spawn(NAME, packet(Role::Exit42AfterRelease, &scratch));
    let pid = child.id() as u32;
    child.release();
    let deadline = Instant::now() + CASE_BOUND;
    loop {
        if let Some(status) = observe_exit(pid) {
            assert_eq!(status, (libc::CLD_EXITED, 42));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned exit-42 child did not exit"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let error = InitialRootReadyMonitor::for_child(child.0.as_mut().unwrap())
        .err()
        .expect("exited root was accepted");
    // The kernel status, not std::Child's cached exit status, remains waitable.
    assert_eq!(observe_exit(pid), Some((libc::CLD_EXITED, 42)));
    assert!(error.to_string().contains("root already exited"));
    assert_eq!(child.0.as_mut().unwrap().wait().unwrap().code(), Some(42));
    assert!(
        InitialRootReadyMonitor::for_child(child.0.as_mut().unwrap())
            .err()
            .unwrap()
            .to_string()
            .contains("wait observation failed")
    );
    let output = child.finish().await;
    assert_eq!(output.status.code(), Some(42));
}

#[tokio::test(flavor = "current_thread")]
async fn queued_connector_identity_is_dead_with_endpoint_held() {
    const NAME: &str = "queued_connector_identity_is_dead_with_endpoint_held";
    if child_entry(NAME) {
        return;
    }
    let scratch = Scratch::new();
    let listener = UnixListener::bind(scratch.path("rpc.sock")).unwrap();
    let aliases = UnixDatagram::bind(scratch.path("alias.sock")).unwrap();
    let mut connector = OwnedChild::spawn(NAME, packet(Role::ConnectAndTransfer, &scratch));
    let old_pid = connector.id();
    let mut held = receive_alias(&aliases);
    connector.release();
    assert!(connector.finish().await.status.success());

    // The genuine new root is created only after the original connector was
    // reaped. No PID equality/reuse is fabricated or asserted.
    let mut root = OwnedChild::spawn(NAME, packet(Role::WaitForRelease, &scratch));
    let numeric_pid_reused = old_pid == root.id();
    let root_pidfd = root.live_pidfd();
    let monitor = InitialRootReadyMonitor::for_child(root.0.as_mut().unwrap()).unwrap();
    monitor.attach().unwrap();
    let accepted = accept_bounded(&listener);
    let facts = monitor.capture(&accepted);
    assert!(
        monitor
            .check_declared()
            .unwrap_err()
            .to_string()
            .contains("missing")
    );
    assert_eq!(peer_pid(&accepted), old_pid);
    match peer_pidfd(&accepted) {
        Ok(peer) => assert_exited(&peer),
        Err(error) => assert_eq!(
            error.raw_os_error(),
            Some(libc::ESRCH),
            "a missing feature/setup error is not evidence of a dead connector"
        ),
    }
    assert_eq!(pidfd_events(&root_pidfd), 0);

    prove_endpoint_still_open(&accepted, &mut held);
    let (result, global) = held_exchange(accepted, held, facts, root.id(), true).await;
    assert_eq!(global.calls.load(Ordering::SeqCst), 0);
    let error = result.unwrap_err().to_string();
    assert!(
        error.contains("connecting peer unavailable")
            || error.contains("connecting peer is not live"),
        "{error}"
    );
    eprintln!(
        "queued stale connector={old_pid} new_live_root={} held_endpoint_read=1 numeric_pid_reused={numeric_pid_reused}; no forced PID churn",
        root.id()
    );
    root.release();
    assert!(root.finish().await.status.success());
}

#[tokio::test(flavor = "current_thread")]
async fn retained_connector_pidfd_turns_dead_with_endpoint_held() {
    const NAME: &str = "retained_connector_pidfd_turns_dead_with_endpoint_held";
    if child_entry(NAME) {
        return;
    }
    let scratch = Scratch::new();
    let listener = UnixListener::bind(scratch.path("rpc.sock")).unwrap();
    let aliases = UnixDatagram::bind(scratch.path("alias.sock")).unwrap();
    let mut connector = OwnedChild::spawn(NAME, packet(Role::ConnectAndTransfer, &scratch));
    let old_pid = connector.id();
    let mut held = receive_alias(&aliases);
    let accepted = accept_bounded(&listener);
    assert_eq!(peer_pid(&accepted), old_pid);
    let peer = peer_pidfd(&accepted).unwrap();
    assert_eq!(pidfd_events(&peer), 0);
    let mut root = OwnedChild::spawn(NAME, packet(Role::WaitForRelease, &scratch));
    let monitor = InitialRootReadyMonitor::for_child(root.0.as_mut().unwrap()).unwrap();
    monitor.attach().unwrap();
    let facts = monitor.capture(&accepted);
    connector.release();
    assert!(connector.finish().await.status.success());

    let numeric_pid_reused = old_pid == root.id();
    let root_pidfd = root.live_pidfd();
    assert_exited(&peer);
    assert_eq!(pidfd_events(&root_pidfd), 0);
    prove_endpoint_still_open(&accepted, &mut held);
    let (result, global) = held_exchange(accepted, held, facts, root.id(), true).await;
    assert_eq!(global.calls.load(Ordering::SeqCst), 0);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("connecting peer is not live")
    );
    eprintln!(
        "retained peer={old_pid} became_dead=1 new_live_root={} held_endpoint_read=1 numeric_pid_reused={numeric_pid_reused}; no forced PID churn",
        root.id()
    );
    root.release();
    assert!(root.finish().await.status.success());
}

struct RootAdmission(i32);
impl super::ConnectionAdmission for RootAdmission {
    fn admit(&self, peer: OwnedFd) -> std::io::Result<super::Admitted> {
        assert_eq!(pidfd_events(&peer), 0);
        Ok(super::Admitted { process_id: self.0 })
    }
}

// Actual public server + client paths; no monitor state is synthesized.
async fn script_case(
    test: &str,
    steps: Vec<Step>,
    status: i32,
    admission: bool,
) -> (
    InitialRootReadyMonitor,
    Arc<Counting>,
    bool,
    Result<(), crate::RpcError>,
) {
    let scratch = Scratch::new();
    let global = Arc::new(Counting::default());
    let application = Arc::new(AtomicBool::new(false));
    let mut root = OwnedChild::spawn(test, packet(Role::Script(steps, status), &scratch));
    let monitor = InitialRootReadyMonitor::for_child(root.0.as_mut().unwrap()).unwrap();
    let mut server = RpcServer::bind_with_readiness(
        scratch.path("rpc.sock"),
        global.clone(),
        CONFIG.to_owned(),
        application.clone(),
    )
    .unwrap()
    .require_initial_root_ready(monitor.clone())
    .unwrap();
    if admission {
        server.set_connection_admission(Arc::new(RootAdmission(root.id())));
    }
    let serving = tokio::spawn(async move {
        tokio::time::timeout(CASE_BOUND, server.serve_one())
            .await
            .unwrap()
    });
    root.release();
    assert_eq!(root.finish().await.status.code(), Some(status));
    let result = serving.await.unwrap();
    (monitor, global, application.load(Ordering::SeqCst), result)
}

#[tokio::test]
async fn configuration_and_application_do_not_declare_root() {
    const NAME: &str = "configuration_and_application_do_not_declare_root";
    if child_entry(NAME) {
        return;
    }
    for (app, status) in [(false, 0), (true, 42)] {
        let mut steps = vec![Step::Connect];
        if app {
            steps.push(Step::App(193));
        }
        steps.push(Step::Close);
        let (monitor, global, application, result) = script_case(NAME, steps, status, false).await;
        result.unwrap();
        assert!(
            monitor
                .check_declared()
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        assert_eq!(global.calls.load(Ordering::SeqCst), usize::from(app));
        assert_eq!(application, app);
    }
}

#[tokio::test]
async fn root_declaration_and_real_application_are_independent() {
    const NAME: &str = "root_declaration_and_real_application_are_independent";
    if child_entry(NAME) {
        return;
    }
    for (before, after, borrowed, status) in [
        (false, false, false, 0),
        (false, true, false, 42),
        (true, false, false, 0),
        (false, false, true, 0),
    ] {
        let mut steps = vec![Step::Connect];
        if before {
            steps.push(Step::App(194));
        }
        steps.push(if borrowed {
            Step::BorrowedReady(0, true)
        } else {
            Step::Ready(true)
        });
        if after {
            steps.push(Step::App(195));
        }
        steps.push(Step::Close);
        let (monitor, global, application, result) = script_case(NAME, steps, status, true).await;
        result.unwrap();
        monitor.check_declared().unwrap(); // after real child exit and server drain
        assert_eq!(
            global.calls.load(Ordering::SeqCst),
            usize::from(before || after)
        );
        assert_eq!(application, before || after);
    }
}

#[tokio::test]
async fn root_sender_duplicate_and_malformed_controls_refuse() {
    const NAME: &str = "root_sender_duplicate_and_malformed_controls_refuse";
    if child_entry(NAME) {
        return;
    }
    let mut malformed = crate::root_ready::request(Tid::from_raw(1))
        .unwrap()
        .to_vec();
    malformed[6] = 2;
    for (steps, reason) in [
        (
            vec![Step::Connect, Step::BorrowedReady(1, false)],
            "encoded sender differs",
        ),
        (
            vec![Step::Connect, Step::Ready(true), Step::Ready(false)],
            "duplicate",
        ),
        (vec![Step::Connect, Step::Raw(malformed)], "malformed"),
    ] {
        let (monitor, global, application, result) = script_case(NAME, steps, 0, false).await;
        assert!(result.unwrap_err().to_string().contains(reason));
        assert!(monitor.check_declared().is_err());
        assert_eq!(global.calls.load(Ordering::SeqCst), 0);
        assert!(!application);
    }
}

#[tokio::test]
async fn other_peer_cannot_declare_live_or_exited_root() {
    const NAME: &str = "other_peer_cannot_declare_live_or_exited_root";
    if child_entry(NAME) {
        return;
    }
    for exited in [false, true] {
        let scratch = Scratch::new();
        let mut root = OwnedChild::spawn(NAME, packet(Role::WaitForRelease, &scratch));
        let monitor = InitialRootReadyMonitor::for_child(root.0.as_mut().unwrap()).unwrap();
        let global = Arc::new(Counting::default());
        let server = RpcServer::bind(scratch.path("rpc.sock"), global.clone(), CONFIG.to_owned())
            .unwrap()
            .require_initial_root_ready(monitor.clone())
            .unwrap();
        let root_pid = root.id();
        if exited {
            root.release();
            assert!(root.finish().await.status.success());
        }
        let mut other = OwnedChild::spawn(
            NAME,
            packet(
                Role::Script(vec![Step::Connect, Step::ClaimReady(root_pid, false)], 0),
                &scratch,
            ),
        );
        let serving = tokio::spawn(async move {
            tokio::time::timeout(CASE_BOUND, server.serve_one())
                .await
                .unwrap()
        });
        other.release();
        assert!(other.finish().await.status.success());
        let error = serving.await.unwrap().unwrap_err().to_string();
        assert!(
            error.contains(if exited {
                "root is not live"
            } else {
                "connecting peer differs"
            }),
            "{error}"
        );
        assert!(monitor.check_declared().is_err());
        assert_eq!(global.calls.load(Ordering::SeqCst), 0);
        if !exited {
            root.release();
            assert!(root.finish().await.status.success());
        }
    }
}

#[tokio::test]
async fn declaration_survives_nondeclaring_dead_connector_accepted_later() {
    const NAME: &str = "declaration_survives_nondeclaring_dead_connector_accepted_later";
    if child_entry(NAME) {
        return;
    }
    let scratch = Scratch::new();
    let global = Arc::new(Counting::default());
    let mut root = OwnedChild::spawn(
        NAME,
        packet(
            Role::Script(
                vec![Step::Connect, Step::Ready(true), Step::Close, Step::Hold],
                0,
            ),
            &scratch,
        ),
    );
    let monitor = InitialRootReadyMonitor::for_child(root.0.as_mut().unwrap()).unwrap();
    let server = RpcServer::bind(scratch.path("rpc.sock"), global.clone(), CONFIG.to_owned())
        .unwrap()
        .require_initial_root_ready(monitor.clone())
        .unwrap();
    root.release();
    tokio::time::timeout(CASE_BOUND, server.serve_one())
        .await
        .unwrap()
        .unwrap();
    monitor.check_declared().unwrap();

    // Connect AND accept only after Declared; the actual original connector
    // exits/reaps while the held socket remains usable. No ready frame follows.
    let aliases = UnixDatagram::bind(scratch.path("alias.sock")).unwrap();
    let mut connector = OwnedChild::spawn(NAME, packet(Role::ConnectAndTransfer, &scratch));
    let mut held = receive_alias(&aliases);
    connector.release();
    assert!(connector.finish().await.status.success());
    let client = tokio::task::spawn_blocking(move || {
        held.set_read_timeout(Some(IO_BOUND)).unwrap();
        assert_eq!(
            codec::decode_exact::<String>(&read_config_frame(&mut held).unwrap()).unwrap(),
            CONFIG
        );
        let config = CONFIG.to_owned();
        let borrowed = BlockingRpcClient::<Counting>::from_borrowed_stream(
            &mut held,
            Tid::from_raw(123),
            &config,
        );
        assert_eq!(borrowed.try_send_rpc(196).unwrap(), 196);
    });
    tokio::time::timeout(CASE_BOUND, server.serve_one())
        .await
        .unwrap()
        .unwrap();
    client.await.unwrap();
    monitor.check_declared().unwrap();
    assert_eq!(global.calls.load(Ordering::SeqCst), 1);
    assert_eq!(global.last_request.load(Ordering::SeqCst), 196);
    root.release();
    assert!(root.finish().await.status.success());
}

#[tokio::test]
async fn second_connection_duplicate_fails_existing_declaration() {
    const NAME: &str = "second_connection_duplicate_fails_existing_declaration";
    if child_entry(NAME) {
        return;
    }
    let scratch = Scratch::new();
    let mut root = OwnedChild::spawn(
        NAME,
        packet(
            Role::Script(
                vec![
                    Step::Connect,
                    Step::Ready(true),
                    Step::Close,
                    Step::Connect,
                    Step::Ready(false),
                ],
                0,
            ),
            &scratch,
        ),
    );
    let monitor = InitialRootReadyMonitor::for_child(root.0.as_mut().unwrap()).unwrap();
    let global = Arc::new(Counting::default());
    let server = RpcServer::bind(scratch.path("rpc.sock"), global.clone(), CONFIG.to_owned())
        .unwrap()
        .require_initial_root_ready(monitor.clone())
        .unwrap();
    root.release();
    tokio::time::timeout(CASE_BOUND, server.serve_one())
        .await
        .unwrap()
        .unwrap();
    monitor.check_declared().unwrap();
    let error = tokio::time::timeout(CASE_BOUND, server.serve_one())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("duplicate"));
    assert!(root.finish().await.status.success());
    assert!(monitor.check_declared().is_err());
    assert_eq!(global.calls.load(Ordering::SeqCst), 0);
}

fn setup(facts: crate::root_ready::InitialRootConnection) -> super::ConnectionSetup {
    super::ConnectionSetup {
        initial_root_ready: Some(facts),
        ..Default::default()
    }
}

async fn held_exchange(
    accepted: UnixStream,
    mut held: UnixStream,
    facts: crate::root_ready::InitialRootConnection,
    sender: i32,
    declaration: bool,
) -> (Result<(), crate::RpcError>, Arc<Counting>) {
    accepted.set_nonblocking(true).unwrap();
    let stream = tokio::net::UnixStream::from_std(accepted).unwrap();
    let global = Arc::new(Counting::default());
    let client = tokio::task::spawn_blocking(move || {
        held.set_read_timeout(Some(IO_BOUND)).unwrap();
        held.set_write_timeout(Some(IO_BOUND)).unwrap();
        let config: String = codec::decode_exact(&read_config_frame(&mut held).unwrap()).unwrap();
        assert_eq!(config, CONFIG);
        let borrowed = BlockingRpcClient::<Counting>::from_borrowed_stream(
            &mut held,
            Tid::from_raw(sender),
            &config,
        );
        if declaration {
            assert!(borrowed.declare_initial_root_ready().is_err());
        } else {
            assert_eq!(borrowed.try_send_rpc(197).unwrap(), 197);
        }
    });
    let result = tokio::time::timeout(
        CASE_BOUND,
        super::serve_connection_inner(global.clone(), CONFIG.to_owned(), stream, setup(facts)),
    )
    .await
    .unwrap();
    client.await.unwrap();
    (result, global)
}

#[derive(Clone, Copy)]
enum AckFault {
    WriteError,
    ParkWrite,
    PanicWrite,
    ParkFlush,
    Short,
}
struct AckGate {
    reached: tokio::sync::Notify,
    released: AtomicBool,
    waker: std::sync::Mutex<Option<std::task::Waker>>,
}
impl AckGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            reached: tokio::sync::Notify::new(),
            released: AtomicBool::new(false),
            waker: std::sync::Mutex::new(None),
        })
    }
    fn park(&self, cx: &std::task::Context<'_>) -> bool {
        *self.waker.lock().unwrap() = Some(cx.waker().clone());
        self.reached.notify_one();
        !self.released.load(Ordering::SeqCst)
    }
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}
struct FaultStream {
    inner: tokio::net::UnixStream,
    fault: AckFault,
    config_end: usize,
    written: usize,
    gate: Arc<AckGate>,
}
impl tokio::io::AsyncRead for FaultStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if matches!(self.fault, AckFault::Short) {
            let mut bytes = [0; 3];
            let size = bytes.len().min(buf.remaining());
            let mut short = tokio::io::ReadBuf::new(&mut bytes[..size]);
            let result = std::pin::Pin::new(&mut self.inner).poll_read(cx, &mut short);
            buf.put_slice(short.filled());
            result
        } else {
            std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }
}
impl tokio::io::AsyncWrite for FaultStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.written >= self.config_end {
            match self.fault {
                AckFault::WriteError => {
                    return std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
                }
                AckFault::ParkWrite if self.gate.park(cx) => return std::task::Poll::Pending,
                AckFault::PanicWrite => panic!("injected ACK writer panic"),
                _ => {}
            }
        }
        let buf = if matches!(self.fault, AckFault::Short) {
            &buf[..buf.len().min(2)]
        } else {
            buf
        };
        let result = std::pin::Pin::new(&mut self.inner).poll_write(cx, buf);
        if let std::task::Poll::Ready(Ok(written)) = result {
            self.written += written;
        }
        result
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if matches!(self.fault, AckFault::ParkFlush)
            && self.written > self.config_end
            && self.gate.park(cx)
        {
            return std::task::Poll::Pending;
        }
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[tokio::test]
async fn ack_failure_cancellation_panic_and_duplicate_are_sticky() {
    const NAME: &str = "ack_failure_cancellation_panic_and_duplicate_are_sticky";
    if child_entry(NAME) {
        return;
    }
    for fault in [
        AckFault::WriteError,
        AckFault::ParkWrite,
        AckFault::PanicWrite,
        AckFault::ParkFlush,
        AckFault::Short,
    ] {
        let scratch = Scratch::new();
        let listener = UnixListener::bind(scratch.path("rpc.sock")).unwrap();
        let success = matches!(fault, AckFault::Short | AckFault::ParkFlush);
        let mut root = OwnedChild::spawn(
            NAME,
            packet(
                Role::Script(
                    vec![Step::Connect, Step::Ready(success), Step::Close, Step::Hold],
                    0,
                ),
                &scratch,
            ),
        );
        let monitor = InitialRootReadyMonitor::for_child(root.0.as_mut().unwrap()).unwrap();
        monitor.attach().unwrap();
        root.release();
        let accepted = accept_bounded(&listener);
        let facts = monitor.capture(&accepted);
        let duplicate = monitor.capture(&accepted);
        accepted.set_nonblocking(true).unwrap();
        let gate = AckGate::new();
        let stream = FaultStream {
            inner: tokio::net::UnixStream::from_std(accepted).unwrap(),
            fault,
            config_end: 4 + codec::encode(&CONFIG.to_owned()).unwrap().len(),
            written: 0,
            gate: gate.clone(),
        };
        let global = Arc::new(Counting::default());
        let g = global.clone();
        let serving = tokio::spawn(async move {
            super::serve_connection_inner(g, CONFIG.to_owned(), stream, setup(facts)).await
        });
        if matches!(fault, AckFault::ParkWrite | AckFault::ParkFlush) {
            tokio::time::timeout(IO_BOUND, gate.reached.notified())
                .await
                .unwrap();
            assert!(
                monitor
                    .check_declared()
                    .unwrap_err()
                    .to_string()
                    .contains("incomplete")
            );
            if matches!(fault, AckFault::ParkWrite) {
                serving.abort();
            } else {
                assert!(duplicate.begin(Tid::from_raw(root.id())).is_err());
                gate.release(); // ACK reached client, but commit must not overwrite duplicate failure.
            }
        }
        let result = tokio::time::timeout(CASE_BOUND, serving).await.unwrap();
        match fault {
            AckFault::Short => {
                result.unwrap().unwrap();
                monitor.check_declared().unwrap();
            }
            AckFault::PanicWrite => {
                assert!(result.unwrap_err().is_panic());
                assert!(monitor.check_declared().is_err());
            }
            AckFault::ParkWrite => {
                assert!(result.unwrap_err().is_cancelled());
                assert!(monitor.check_declared().is_err());
            }
            _ => {
                assert!(result.unwrap().is_err());
                assert!(monitor.check_declared().is_err());
            }
        }
        if !matches!(fault, AckFault::Short) {
            let reason = monitor.check_declared().unwrap_err().to_string();
            assert!(
                reason.contains(if matches!(fault, AckFault::ParkFlush) {
                    "duplicate"
                } else {
                    "acknowledgement did not complete"
                }),
                "{reason}"
            );
        }
        assert_eq!(global.calls.load(Ordering::SeqCst), 0);
        root.release();
        assert!(root.finish().await.status.success());
    }
}

struct ExhaustedDescriptors {
    original: libc::rlimit,
    fillers: Vec<OwnedFd>,
}

impl ExhaustedDescriptors {
    fn new(source: RawFd) -> Self {
        let mut original: libc::rlimit = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut original) },
            0
        );
        assert!(
            original.rlim_cur >= 8,
            "fixture needs a usable initial fd limit"
        );
        let limit = libc::rlimit {
            rlim_cur: original.rlim_cur.min(64),
            rlim_max: original.rlim_max,
        };
        let mut guard = Self {
            original,
            fillers: Vec::new(),
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
        // At most 64 descriptor slots can be allocated under this soft limit.
        // Duplicate only this test's actual open fd, never close another fd.
        for _ in 0..=64 {
            let fd = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 0) };
            if fd < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EMFILE)
                );
                return guard;
            }
            guard.fillers.push(unsafe { OwnedFd::from_raw_fd(fd) });
        }
        panic!("fixture did not establish EMFILE within its bounded fd count");
    }
}

impl Drop for ExhaustedDescriptors {
    fn drop(&mut self) {
        let rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &self.original) };
        self.fillers.clear();
        if rc != 0 {
            eprintln!(
                "owned fixture could not restore RLIMIT_NOFILE: {}",
                std::io::Error::last_os_error()
            );
            if !std::thread::panicking() {
                panic!("owned fixture fd limit restoration failed");
            }
        }
    }
}

#[tokio::test]
async fn actual_emfile_capture_preserves_prior_declared_for_ordinary_rpc() {
    const NAME: &str = "actual_emfile_capture_preserves_prior_declared_for_ordinary_rpc";
    if std::env::var_os(CHILD_MODE).is_none() {
        let scratch = Scratch::new();
        let mut child = OwnedChild::spawn(NAME, packet(Role::Isolated, &scratch));
        assert!(child.finish().await.status.success());
        return;
    }
    // Its nested root is a real subprocess using the ordinary script entry.
    if std::env::var_os("REVERIE_READY_EMFILE_ROOT").is_some() {
        assert!(child_entry(NAME));
        return;
    }
    let bytes = read_config_frame(&mut std::io::stdin().lock()).unwrap();
    let _: Packet = codec::decode_exact(&bytes).unwrap();
    let scratch = Scratch::new();
    // This isolated process has no other tests/threads changing env or limits.
    unsafe {
        std::env::set_var("REVERIE_READY_EMFILE_ROOT", "1");
    }
    let mut root = OwnedChild::spawn(
        NAME,
        packet(
            Role::Script(
                vec![Step::Connect, Step::Ready(true), Step::Close, Step::Hold],
                0,
            ),
            &scratch,
        ),
    );
    unsafe {
        std::env::remove_var("REVERIE_READY_EMFILE_ROOT");
    }
    let monitor = InitialRootReadyMonitor::for_child(root.0.as_mut().unwrap()).unwrap();
    let mut server = RpcServer::bind(
        scratch.path("rpc.sock"),
        Arc::new(Counting::default()),
        CONFIG.to_owned(),
    )
    .unwrap()
    .require_initial_root_ready(monitor.clone())
    .unwrap();
    root.release();
    tokio::time::timeout(CASE_BOUND, server.serve_one())
        .await
        .unwrap()
        .unwrap();
    monitor.check_declared().unwrap();
    let listener = UnixListener::bind(scratch.path("emfile.sock")).unwrap();
    let held = UnixStream::connect(scratch.path("emfile.sock")).unwrap();
    let accepted = accept_bounded(&listener);
    let exhausted = ExhaustedDescriptors::new(accepted.as_raw_fd());
    assert_eq!(
        peer_pidfd(&accepted).unwrap_err().raw_os_error(),
        Some(libc::EMFILE)
    );
    let facts = monitor.capture(&accepted);
    let declaring = monitor.capture(&accepted);
    drop(exhausted);
    monitor.check_declared().unwrap();
    let (result, global) = held_exchange(accepted, held, facts, 123, false).await;
    result.unwrap();
    assert_eq!(global.calls.load(Ordering::SeqCst), 1);
    assert_eq!(global.last_request.load(Ordering::SeqCst), 197);
    monitor.check_declared().unwrap();
    // Production accept ordering: leave exactly two fd slots, one for accept
    // and one for existing admission. Optional readiness capture must be last.
    let admission = Arc::new(HoldingAdmission {
        pid: std::process::id() as i32,
        peer: std::sync::Mutex::new(None),
    });
    server.set_connection_admission(admission.clone());
    let mut client = UnixStream::connect(scratch.path("rpc.sock")).unwrap();
    client.set_read_timeout(Some(IO_BOUND)).unwrap();
    let ordinary = tokio::task::spawn_blocking(move || {
        let config: String = codec::decode_exact(&read_config_frame(&mut client).unwrap()).unwrap();
        let borrowed = BlockingRpcClient::<Counting>::from_borrowed_stream(
            &mut client,
            Tid::from_raw(std::process::id() as i32),
            &config,
        );
        assert_eq!(borrowed.try_send_rpc(198).unwrap(), 198);
    });
    let mut exhausted = ExhaustedDescriptors::new(listener.as_raw_fd());
    assert!(exhausted.fillers.len() >= 2);
    drop(exhausted.fillers.pop());
    drop(exhausted.fillers.pop());
    let result = tokio::time::timeout(CASE_BOUND, server.serve_one())
        .await
        .unwrap();
    drop(exhausted);
    result.unwrap();
    ordinary.await.unwrap();
    assert!(admission.peer.lock().unwrap().is_some());
    assert_eq!(server.global().last_request.load(Ordering::SeqCst), 198);
    monitor.check_declared().unwrap();

    // The same unavailable fact gains authority only on control dispatch.
    assert!(
        declaring
            .begin(Tid::from_raw(root.id()))
            .err()
            .unwrap()
            .to_string()
            .contains("connecting peer unavailable")
    );
    assert!(monitor.check_declared().is_err());
    root.release();
    assert!(root.finish().await.status.success());
}

struct HoldingAdmission {
    pid: i32,
    peer: std::sync::Mutex<Option<OwnedFd>>,
}
impl super::ConnectionAdmission for HoldingAdmission {
    fn admit(&self, peer: OwnedFd) -> std::io::Result<super::Admitted> {
        assert_eq!(pidfd_events(&peer), 0);
        *self.peer.lock().unwrap() = Some(peer);
        Ok(super::Admitted {
            process_id: self.pid,
        })
    }
}

struct NoPrivateRedemption;
impl crate::initial_image::InitialImageAdmission for NoPrivateRedemption {
    fn redeem(
        &self,
        _: OwnedFd,
        _: super::Admitted,
        _: &[u8],
        _: crate::initial_image::ConfigDigest,
    ) -> std::io::Result<()> {
        panic!("configuration test must not redeem authority")
    }
}
impl crate::successor::SuccessorAdmission for NoPrivateRedemption {
    fn redeem(
        &self,
        _: OwnedFd,
        _: super::Admitted,
        _: crate::successor::SuccessorRequest<'_>,
    ) -> std::io::Result<Vec<u8>> {
        panic!("configuration test must not redeem authority")
    }
}

#[tokio::test]
async fn private_admission_is_incompatible_in_both_configuration_orders() {
    const NAME: &str = "private_admission_is_incompatible_in_both_configuration_orders";
    if child_entry(NAME) {
        return;
    }
    let scratch = Scratch::new();
    let mut root = OwnedChild::spawn(NAME, packet(Role::WaitForRelease, &scratch));
    for private_first in [false, true] {
        for native in [false, true] {
            let path = scratch.path(&format!("private-{private_first}-{native}.sock"));
            let monitor = InitialRootReadyMonitor::for_child(root.0.as_mut().unwrap()).unwrap();
            let mut server =
                RpcServer::bind(path, Arc::new(Counting::default()), CONFIG.to_owned()).unwrap();
            server.set_connection_admission(Arc::new(RootAdmission(root.id())));
            server.retain_connection_issues();
            let configure = |server: &mut RpcServer<Counting>| {
                if native {
                    server.set_native_image_admission(
                        Arc::new(NoPrivateRedemption),
                        Arc::new(NoPrivateRedemption),
                    )
                } else {
                    server.set_initial_image_admission(Arc::new(NoPrivateRedemption))
                }
            };
            if private_first {
                configure(&mut server).unwrap();
                assert!(
                    server
                        .require_initial_root_ready(monitor)
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("incompatible")
                );
            } else {
                let mut server = server.require_initial_root_ready(monitor.clone()).unwrap();
                assert!(
                    configure(&mut server)
                        .unwrap_err()
                        .to_string()
                        .contains("incompatible")
                );
                let second = RpcServer::bind(
                    scratch.path(&format!("second-{native}.sock")),
                    Arc::new(Counting::default()),
                    CONFIG.to_owned(),
                )
                .unwrap();
                assert!(
                    second
                        .require_initial_root_ready(monitor)
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("already attached")
                );
            }
        }
    }
    root.release();
    assert!(root.finish().await.status.success());
}

#[test]
fn ready_wire_and_client_ack_are_exact_and_bounded() {
    let tid = Tid::from_raw(123);
    let request = crate::root_ready::request(tid).unwrap();
    assert_eq!(
        request,
        [255, 255, 255, 255, b'R', b'I', 1, 0, 123, 0, 0, 0]
    );
    assert_eq!(
        crate::root_ready::decode_request(&request).unwrap(),
        Some(tid)
    );
    for length in 1..12 {
        assert!(crate::root_ready::decode_request(&request[..length]).is_err());
    }
    let mut invalid = vec![];
    for (index, value) in [(6, 2), (7, 1)] {
        let mut bytes = request.to_vec();
        bytes[index] = value;
        invalid.push(bytes);
    }
    for sender in [0i32, -1] {
        let mut bytes = request.to_vec();
        bytes[8..].copy_from_slice(&sender.to_le_bytes());
        invalid.push(bytes);
    }
    let mut extra = request.to_vec();
    extra.push(0);
    invalid.push(extra);
    for bytes in invalid {
        assert!(crate::root_ready::decode_request(&bytes).is_err());
    }
    assert!(
        crate::root_ready::decode_request(&codec::encode(&(tid, 77u64)).unwrap())
            .unwrap()
            .is_none()
    );
    let ack = crate::root_ready::ack(tid).unwrap();
    let mut responses = vec![
        ack.to_vec(),
        request.to_vec(),
        ack[..11].to_vec(),
        vec![0; 13],
    ];
    for index in 0..12 {
        let mut changed = ack.to_vec();
        changed[index] ^= 1;
        responses.push(changed);
    }
    for (index, response) in responses.into_iter().enumerate() {
        let (mut server, stream) = UnixStream::pair().unwrap();
        server.set_read_timeout(Some(IO_BOUND)).unwrap();
        stream.set_read_timeout(Some(IO_BOUND)).unwrap();
        let responder = std::thread::spawn(move || {
            write_frame(&mut server, &codec::encode(&CONFIG.to_owned()).unwrap());
            assert_eq!(read_config_frame(&mut server).unwrap(), request);
            write_frame(&mut server, &response);
        });
        let client = BlockingRpcClient::<Counting>::from_connected_stream(stream, tid).unwrap();
        assert_eq!(client.declare_initial_root_ready().is_ok(), index == 0);
        responder.join().unwrap();
    }
}
