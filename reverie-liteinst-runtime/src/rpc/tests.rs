use std::cell::RefCell;
use std::future::Future;
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::panic::AssertUnwindSafe;
use std::panic::catch_unwind;
use std::pin::pin;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::mpsc;
use std::task::Context;
use std::task::Poll;
use std::task::Wake;
use std::task::Waker;

use super::*;

type FdHook = RefCell<Option<Box<dyn FnOnce(i32)>>>;

thread_local! {
    static PUBLISHED: FdHook = RefCell::new(None);
    static BEFORE_CLOSE: FdHook = RefCell::new(None);
}

pub(super) fn published(fd: i32) {
    let hook = PUBLISHED.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook(fd);
    }
}

pub(super) fn before_close(fd: i32) {
    let hook = BEFORE_CLOSE.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook(fd);
    }
}

pub(crate) fn isolated(name: &str, body: impl FnOnce()) {
    const MARKER: &str = "LITEINST_RPC_RESOURCE_CONTROL";
    if std::env::var(MARKER).ok().as_deref() == Some(name) {
        println!("\nRESOURCE_BODY_BEGIN {name}");
        body();
        println!("\nRESOURCE_BODY_END {name}");
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(MARKER, name)
        .output()
        .unwrap();
    std::io::stdout().write_all(&output.stdout).unwrap();
    std::io::stderr().write_all(&output.stderr).unwrap();
    assert!(
        output.status.success(),
        "isolated resource control failed: {}",
        output.status
    );
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    assert_eq!(
        stdout
            .lines()
            .filter(|line| *line == "running 1 test")
            .count(),
        1
    );
    let summaries: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with("test result: "))
        .collect();
    assert_eq!(summaries.len(), 1);
    assert!(
        summaries[0].starts_with("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; ")
    );
    for boundary in ["BEGIN", "END"] {
        let marker = format!("RESOURCE_BODY_{boundary} {name}");
        assert_eq!(stdout.lines().filter(|line| *line == marker).count(), 1);
    }
}

pub(crate) fn write_frame(stream: &mut UnixStream, bytes: &[u8]) {
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(bytes).unwrap();
}

pub(crate) fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let mut bytes = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut bytes).unwrap();
    bytes
}

fn fd_open(fd: i32) -> bool {
    unsafe {
        raw_syscall6(
            libc::SYS_fcntl,
            [fd as u64, libc::F_GETFD as u64, 0, 0, 0, 0],
        ) >= 0
    }
}

fn protected(fd: i32) -> bool {
    crate::runtime::protected_injected_syscall(libc::SYS_read, [fd as u64, 0, 0, 0, 0, 0])
        == Some(-i64::from(libc::EBADF))
}

#[test]
fn bootstrap_publication_precedes_delayed_config_without_callback() {
    isolated(
        "rpc::tests::bootstrap_publication_precedes_delayed_config_without_callback",
        || {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("rpc");
            let listener = UnixListener::bind(&path).unwrap();
            let resources = resources::Resources::new();
            let worker_resources = resources.clone();
            let (published_tx, published_rx) = mpsc::channel();
            let (accepted_tx, accepted_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut peer, _) = listener.accept().unwrap();
                accepted_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                write_frame(
                    &mut peer,
                    &reverie_rpc_transport::codec::encode(&()).unwrap(),
                );
                let mut byte = [0];
                assert_eq!(peer.read(&mut byte).unwrap(), 0);
            });
            let client = std::thread::spawn(move || {
                PUBLISHED.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(move |fd| published_tx.send(fd).unwrap()))
                });
                CoordinatorRpc::<()>::connect_bootstrap(&path, worker_resources).unwrap()
            });
            let fd = published_rx.recv().unwrap();
            accepted_rx.recv().unwrap();
            assert!(fd_open(fd) && protected(fd));
            assert_eq!(resources.active(), 1);
            assert!(!allows_channel_io(libc::SYS_read, fd));
            release_tx.send(()).unwrap();
            let client = client.join().unwrap();
            assert_eq!(client.raw_fd(), fd);
            assert_eq!(client.config(), &());
            resources.close();
            client.retire().unwrap();
            assert!(!fd_open(fd) && !protected(fd));
            assert_eq!(resources.active(), 0);
            reverie_preload::tool_host::drive_ready(resources.drain()).unwrap();
            assert!(client.retire().is_err());
            server.join().unwrap();
        },
    );
}

#[test]
fn bootstrap_connect_error_retires_published_resource() {
    isolated(
        "rpc::tests::bootstrap_connect_error_retires_published_resource",
        || {
            let directory = tempfile::tempdir().unwrap();
            let resources = resources::Resources::new();
            let recorded = Arc::new(AtomicI32::new(-1));
            let capture = recorded.clone();
            PUBLISHED.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |fd| capture.store(fd, Ordering::Release)))
            });
            let error = CoordinatorRpc::<()>::connect_bootstrap(
                &directory.path().join("missing"),
                resources.clone(),
            )
            .err()
            .unwrap();
            assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
            let fd = recorded.load(Ordering::Acquire);
            assert!(fd >= 0 && !fd_open(fd) && !protected(fd));
            assert_eq!(resources.active(), 0);
            resources.close();
            reverie_preload::tool_host::drive_ready(resources.drain()).unwrap();
        },
    );
}

#[test]
fn bootstrap_config_error_and_publication_unwind_release_exact_owner() {
    isolated(
        "rpc::tests::bootstrap_config_error_and_publication_unwind_release_exact_owner",
        || {
            for unwind in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("rpc");
                let listener = UnixListener::bind(&path).unwrap();
                let resources = resources::Resources::new();
                let recorded = Arc::new(AtomicI32::new(-1));
                let capture = recorded.clone();
                PUBLISHED.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(move |fd| {
                        capture.store(fd, Ordering::Release);
                        assert!(!unwind, "publication unwind control");
                    }))
                });
                let server = (!unwind).then(|| {
                    std::thread::spawn(move || {
                        let (peer, _) = listener.accept().unwrap();
                        drop(peer);
                    })
                });
                let result = catch_unwind(AssertUnwindSafe(|| {
                    CoordinatorRpc::<()>::connect_bootstrap(&path, resources.clone())
                }));
                if unwind {
                    assert!(result.is_err());
                } else {
                    assert!(result.unwrap().is_err());
                }
                let fd = recorded.load(Ordering::Acquire);
                assert!(fd >= 0 && !fd_open(fd) && !protected(fd));
                assert_eq!(resources.active(), 0);
                resources.close();
                reverie_preload::tool_host::drive_ready(resources.drain()).unwrap();
                if let Some(server) = server {
                    server.join().unwrap();
                }
            }
        },
    );
}

#[test]
fn bootstrap_conflicting_slot_preserves_existing_owner() {
    isolated(
        "rpc::tests::bootstrap_conflicting_slot_preserves_existing_owner",
        || {
            let directory = tempfile::tempdir().unwrap();
            let (existing, _peer) = UnixStream::pair().unwrap();
            let fd = existing.as_raw_fd();
            crate::runtime::reserve_coordinator_fd(fd).unwrap();
            let resources = resources::Resources::new();
            let error = CoordinatorRpc::<()>::connect_bootstrap(
                &directory.path().join("missing"),
                resources.clone(),
            )
            .err()
            .unwrap();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert!(fd_open(fd) && protected(fd));
            assert_eq!(resources.active(), 0);
            crate::runtime::replace_coordinator_fd(fd, -1).unwrap();
            resources.close();
            reverie_preload::tool_host::drive_ready(resources.drain()).unwrap();
        },
    );
}

#[test]
fn native_close_stays_counted_and_published_through_blocking_destructor() {
    isolated(
        "rpc::tests::native_close_stays_counted_and_published_through_blocking_destructor",
        || {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("rpc");
            let listener = UnixListener::bind(&path).unwrap();
            let server = std::thread::spawn(move || {
                let (mut peer, _) = listener.accept().unwrap();
                write_frame(
                    &mut peer,
                    &reverie_rpc_transport::codec::encode(&()).unwrap(),
                );
                let mut byte = [0];
                assert_eq!(peer.read(&mut byte).unwrap(), 0);
            });
            let resources = resources::Resources::new();
            let client = CoordinatorRpc::<()>::connect_bootstrap(&path, resources.clone()).unwrap();
            let fd = client.raw_fd();
            let entered = Arc::new(Barrier::new(2));
            let release = Arc::new(Barrier::new(2));
            resources.close();
            let hook_resources = resources.clone();
            let hook_entered = entered.clone();
            let hook_release = release.clone();
            let worker = std::thread::spawn(move || {
                BEFORE_CLOSE.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(move |actual_fd| {
                        assert_eq!(actual_fd, fd);
                        assert_eq!(hook_resources.active(), 1);
                        assert!(hook_resources.acquire().is_err());
                        hook_entered.wait();
                        hook_release.wait();
                    }))
                });
                client.retire().unwrap();
            });
            entered.wait();
            let mut drain = pin!(resources.drain());
            let mut context = Context::from_waker(Waker::noop());
            assert!(drain.as_mut().poll(&mut context).is_pending());
            assert!(fd_open(fd) && protected(fd));
            release.wait();
            worker.join().unwrap();
            assert!(matches!(
                drain.as_mut().poll(&mut context),
                Poll::Ready(Ok(()))
            ));
            assert!(!fd_open(fd) && !protected(fd));
            server.join().unwrap();
        },
    );
}

struct ReentrantWake(resources::Resources);

impl Wake for ReentrantWake {
    fn wake(self: Arc<Self>) {
        assert_eq!(self.0.active(), 0);
        assert!(self.0.acquire().is_err());
    }
}

#[test]
fn resource_terminal_lease_is_separate_and_wake_is_outside_lock() {
    let resources = resources::Resources::new();
    let pending = resources.acquire().unwrap();
    let terminal = resources.acquire().unwrap();
    resources.close();
    let waker = Waker::from(Arc::new(ReentrantWake(resources.clone())));
    let mut context = Context::from_waker(&waker);
    let mut drain = pin!(resources.drain());
    assert!(drain.as_mut().poll(&mut context).is_pending());
    drop(pending);
    assert_eq!(resources.active(), 1);
    assert!(drain.as_mut().poll(&mut context).is_pending());
    drop(terminal);
    assert!(matches!(
        drain.as_mut().poll(&mut context),
        Poll::Ready(Ok(()))
    ));
}

#[test]
fn failed_resource_cleanup_is_not_successful_drain() {
    let resources = resources::Resources::new();
    let pending = resources.acquire().unwrap();
    pending.failed(libc::EIO);
    assert!(resources.acquire().is_err());
    resources.close();
    drop(pending);
    assert_eq!(
        reverie_preload::tool_host::drive_ready(resources.drain())
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EIO)
    );
}

#[test]
fn bootstrap_socket_allocation_error_releases_unpublished_lease() {
    isolated(
        "rpc::tests::bootstrap_socket_allocation_error_releases_unpublished_lease",
        || {
            struct RestoreLimit(libc::rlimit);
            impl Drop for RestoreLimit {
                fn drop(&mut self) {
                    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &self.0) }, 0);
                }
            }
            let resources = resources::Resources::new();
            let mut saved: libc::rlimit = unsafe { core::mem::zeroed() };
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut saved) },
                0
            );
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: saved.rlim_max,
            };
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
            let restore = RestoreLimit(saved);
            let result = CoordinatorRpc::<()>::connect_bootstrap(
                Path::new("/rpc-allocation-control"),
                resources.clone(),
            );
            drop(restore);
            let error = result.err().unwrap();
            assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
            assert_eq!(resources.active(), 0);
            let (probe, _peer) = UnixStream::pair().unwrap();
            crate::runtime::reserve_coordinator_fd(probe.as_raw_fd()).unwrap();
            crate::runtime::replace_coordinator_fd(probe.as_raw_fd(), -1).unwrap();
            resources.close();
            reverie_preload::tool_host::drive_ready(resources.drain()).unwrap();
        },
    );
}

#[test]
fn native_close_ebadf_poison_is_from_actual_stream_drop() {
    isolated(
        "rpc::tests::native_close_ebadf_poison_is_from_actual_stream_drop",
        || {
            let resources = resources::Resources::new();
            let (socket, peer) = UnixStream::pair().unwrap();
            let fd = socket.as_raw_fd();
            crate::runtime::reserve_coordinator_fd(fd).unwrap();
            let stream = stream::Stream::connected(
                socket,
                Arc::new(AtomicBool::new(true)),
                resources.acquire().unwrap(),
            );
            let result = std::rc::Rc::new(std::cell::Cell::new(i64::MIN));
            let captured = result.clone();
            BEFORE_CLOSE.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |actual_fd| {
                    captured.set(unsafe {
                        raw_syscall6(libc::SYS_close, [actual_fd as u64, 0, 0, 0, 0, 0])
                    });
                }))
            });
            resources.close();
            drop(stream);
            assert_eq!(result.get(), 0);
            assert!(!fd_open(fd) && !protected(fd));
            assert!(fd_open(peer.as_raw_fd()));
            assert_eq!(resources.active(), 0);
            assert_eq!(
                reverie_preload::tool_host::drive_ready(resources.drain())
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::EBADF)
            );
        },
    );
}

#[test]
fn actual_publication_cleanup_error_preserves_replacement() {
    isolated(
        "rpc::tests::actual_publication_cleanup_error_preserves_replacement",
        || {
            struct RestorePublication(i32);
            impl Drop for RestorePublication {
                fn drop(&mut self) {
                    crate::runtime::replace_coordinator_fd(self.0, -1).unwrap();
                }
            }
            let resources = resources::Resources::new();
            let (socket, peer) = UnixStream::pair().unwrap();
            let (replacement, _replacement_peer) = UnixStream::pair().unwrap();
            let fd = socket.as_raw_fd();
            let replacement_fd = replacement.as_raw_fd();
            crate::runtime::reserve_coordinator_fd(fd).unwrap();
            let stream = stream::Stream::connected(
                socket,
                Arc::new(AtomicBool::new(true)),
                resources.acquire().unwrap(),
            );
            crate::runtime::replace_coordinator_fd(fd, replacement_fd).unwrap();
            let restore = RestorePublication(replacement_fd);
            resources.close();
            drop(stream);
            assert!(!fd_open(fd) && !protected(fd));
            assert!(fd_open(peer.as_raw_fd()));
            assert!(fd_open(replacement_fd) && protected(replacement_fd));
            assert_eq!(resources.active(), 0);
            assert_eq!(
                reverie_preload::tool_host::drive_ready(resources.drain())
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::EIO)
            );
            drop(restore);
            assert!(fd_open(replacement_fd) && !protected(replacement_fd));
        },
    );
}
