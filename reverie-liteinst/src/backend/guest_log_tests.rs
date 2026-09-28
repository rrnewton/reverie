use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::os::fd::IntoRawFd;

use super::*;

fn isolated(name: &str) -> bool {
    let name = format!("backend::guest_log_tests::{name}");
    crate::test_process::isolated(&name, "LITEINST_BOOTSTRAP_LOG_TEST")
}

fn assert_closed(fd: i32) {
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
}

#[test]
fn collector_readiness_does_not_need_a_free_blocking_worker() {
    if !isolated("collector_readiness_does_not_need_a_free_blocking_worker") {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (release, released) = std::sync::mpsc::channel();
        let (started, running) = tokio::sync::oneshot::channel();
        let busy = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            released.recv().unwrap();
        });
        running.await.unwrap();
        let (host, peer) = crate::guest_log::channel_pair().unwrap();
        let mut collector = GuestLogCollector::start(host, 100, Arc::default()).unwrap();
        collector.ready().await.unwrap();
        assert!(!busy.is_finished());
        drop(peer);
        let log = collector.finish().await.unwrap();
        assert_eq!(log.bytes, b"");
        assert_eq!(
            log.error.as_deref(),
            Some("guest log missing process completion")
        );
        release.send(()).unwrap();
        busy.await.unwrap();
    });
}

#[test]
fn cancelled_collector_joins_with_the_peer_still_open() {
    if !isolated("cancelled_collector_joins_with_the_peer_still_open") {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for during_collection in [false, true] {
        let (host, mut peer) = crate::guest_log::channel_pair().unwrap();
        let exited = Arc::new(AtomicBool::new(false));
        let weak = Arc::downgrade(&exited);
        let mut collector = GuestLogCollector::start(host, 100, exited).unwrap();
        if during_collection {
            runtime.block_on(collector.ready()).unwrap();
            let mut future = Box::pin(collector.finish());
            assert!(
                future
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                    .is_pending()
            );
            drop(future);
        } else {
            // Dropping even an unpolled readiness future owns the worker; no
            // caller has to reach finish() to stop collection on an error.
            drop(async move { collector.ready().await });
        }
        assert_eq!(weak.strong_count(), 0, "collector still owns its exit flag");
        peer.set_nonblocking(true).unwrap();
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }
}

#[test]
fn collector_failure_closes_the_peer_before_finish_is_awaited() {
    if !isolated("collector_failure_closes_the_peer_before_finish_is_awaited") {
        return;
    }
    let (host, mut peer) = crate::guest_log::channel_pair().unwrap();
    let collector = GuestLogCollector::start(host, 0, Arc::default()).unwrap();
    peer.write_all(&[1, 0, 0, 0, 0]).unwrap(); // START
    peer.write_all(&[1, 0, 0, 0, 1, b'a']).unwrap(); // DATA exceeds limit.
    // Collection has stopped, but the launcher has not yet finished waiting
    // for its child. Keeping the socket open here could block that writer.
    while !collector.worker.as_ref().unwrap().is_finished() {
        std::thread::yield_now();
    }
    assert_eq!(
        peer.write(&[0]).unwrap_err().raw_os_error(),
        Some(libc::EPIPE)
    );
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let log = runtime.block_on(collector.finish()).unwrap();
    assert_eq!(log.bytes, b"");
    assert_eq!(
        log.error.as_deref(),
        Some("guest log byte limit exceeded; truncated")
    );
}

#[test]
fn collector_preserves_errors_and_reports_worker_panics() {
    if !isolated("collector_preserves_errors_and_reports_worker_panics") {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let (host, mut peer) = crate::guest_log::channel_pair().unwrap();
    let collector = GuestLogCollector::start(host, 2, Arc::default()).unwrap();
    peer.write_all(&[1, 0, 0, 0, 0]).unwrap(); // START
    peer.write_all(&[1, 0, 0, 0, 1, b'a', b'b', b'c']).unwrap(); // DATA
    let log = runtime.block_on(collector.finish()).unwrap();
    assert_eq!(log.bytes, b"ab");
    assert_eq!(
        log.error.as_deref(),
        Some("guest log byte limit exceeded; truncated")
    );

    let (shutdown, mut peer) = crate::guest_log::channel_pair().unwrap();
    let shutdown = Arc::new(shutdown);
    let worker_shutdown = GuestLogShutdown(shutdown.clone());
    let (ready_tx, ready) = tokio::sync::oneshot::channel();
    let (result_tx, result) = tokio::sync::oneshot::channel();
    let worker = std::thread::spawn(move || {
        let _shutdown = worker_shutdown;
        let _result_tx = result_tx;
        ready_tx.send(()).unwrap();
        panic!("deliberate collector panic");
    });
    let mut collector = GuestLogCollector {
        shutdown,
        worker: Some(worker),
        ready,
        result,
    };
    runtime.block_on(collector.ready()).unwrap();
    let error = match runtime.block_on(collector.finish()) {
        Err(error) => error,
        Ok(_) => panic!("worker panic was reported as a captured log"),
    };
    assert_eq!(error.to_string(), "guest log collector panicked");
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
}

#[test]
fn legacy_two_field_literal_and_v1_consumer_are_preserved() {
    if !isolated("legacy_two_field_literal_and_v1_consumer_are_preserved") {
        return;
    }
    let expected = PreloadBootstrap {
        coordinator: PathBuf::from("/tmp/legacy-coordinator.sock"),
        tool_data: vec![0, 17, 255],
    };
    let fd = create_preload_bootstrap(&expected.coordinator, &expected.tool_data)
        .unwrap()
        .into_raw_fd();
    let decoded = unsafe { take_preload_bootstrap() }.unwrap().unwrap();
    assert_eq!(decoded.coordinator, expected.coordinator);
    assert_eq!(decoded.tool_data, expected.tool_data);
    assert_closed(fd);
    let fd = create_preload_bootstrap(&expected.coordinator, &expected.tool_data)
        .unwrap()
        .into_raw_fd();
    let decoded = unsafe { take_preload_bootstrap_with_log() }
        .unwrap()
        .unwrap();
    assert_eq!(decoded.bootstrap.coordinator, expected.coordinator);
    assert_eq!(decoded.bootstrap.tool_data, expected.tool_data);
    assert!(decoded.log.is_none());
    assert_closed(fd);
}

#[test]
fn guest_log_bootstrap_round_trip() {
    if !isolated("guest_log_bootstrap_round_trip") {
        return;
    }
    let (guest, mut peer) = crate::guest_log::channel_pair().unwrap();
    let fd = guest.into_raw_fd();
    let packet = create_logged_preload_bootstrap(
        Path::new("/tmp/coordinator"),
        b"typed\0\xfftool",
        Some(fd),
    )
    .unwrap()
    .into_raw_fd();
    let decoded = unsafe { take_preload_bootstrap_with_log() }
        .unwrap()
        .unwrap();
    assert_eq!(decoded.bootstrap.coordinator, Path::new("/tmp/coordinator"));
    assert_eq!(decoded.bootstrap.tool_data, b"typed\0\xfftool");
    assert!(decoded.log.is_some());
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
        libc::FD_CLOEXEC
    );
    assert_closed(packet);
    drop(decoded);
    assert_closed(fd);
    peer.set_nonblocking(true).unwrap();
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
}

#[test]
fn legacy_consumer_refuses_v2_without_silently_discarding_requested_log() {
    if !isolated("legacy_consumer_refuses_v2_without_silently_discarding_requested_log") {
        return;
    }
    let (guest, mut peer) = crate::guest_log::channel_pair().unwrap();
    let fd = guest.into_raw_fd();
    let packet = create_logged_preload_bootstrap(Path::new("/tmp/coordinator"), b"typed", Some(fd))
        .unwrap()
        .into_raw_fd();
    let error = match unsafe { take_preload_bootstrap() } {
        Err(error) => error,
        Ok(_) => panic!("legacy consumer accepted V2"),
    };
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_closed(packet);
    assert_closed(fd);
    peer.set_nonblocking(true).unwrap();
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
}

#[test]
fn duplicate_bootstraps_close_each_owned_endpoint_once() {
    if !isolated("duplicate_bootstraps_close_each_owned_endpoint_once") {
        return;
    }
    for duplicate_integer in [false, true] {
        let (guest, mut peer) = crate::guest_log::channel_pair().unwrap();
        let first = guest.into_raw_fd();
        let second = if duplicate_integer {
            let fd = unsafe { libc::fcntl(first, libc::F_DUPFD_CLOEXEC, 3) };
            assert!(fd >= 3);
            fd
        } else {
            first
        };
        let packets = [first, second].map(|fd| {
            create_logged_preload_bootstrap(
                Path::new("/tmp/duplicate-coordinator"),
                b"typed",
                Some(fd),
            )
            .unwrap()
            .into_raw_fd()
        });
        let error = match unsafe { take_preload_bootstrap_with_log() } {
            Err(error) => error,
            Ok(_) => panic!("duplicate bootstraps accepted"),
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "multiple LiteInst preload bootstraps");
        for fd in packets {
            assert_closed(fd);
        }
        assert_closed(first);
        assert_closed(second);
        peer.set_nonblocking(true).unwrap();
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }
}

#[test]
fn refused_identity_closes_bootstrap_without_closing_unowned_socket() {
    if !isolated("refused_identity_closes_bootstrap_without_closing_unowned_socket") {
        return;
    }
    let (guest, _peer) = crate::guest_log::channel_pair().unwrap();
    let original = create_logged_preload_bootstrap(
        Path::new("/tmp/coordinator"),
        b"typed",
        Some(guest.as_raw_fd()),
    )
    .unwrap();
    let mut original = File::from(original);
    original.seek(SeekFrom::Start(0)).unwrap();
    let mut packet = Vec::new();
    original.read_to_end(&mut packet).unwrap();
    drop(original);
    *packet.last_mut().unwrap() ^= 1;
    let raw = unsafe {
        libc::memfd_create(
            c"bad-log-bootstrap".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    assert!(raw >= 3);
    let mut malformed = unsafe { File::from_raw_fd(raw) };
    malformed.write_all(&packet).unwrap();
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    assert_eq!(unsafe { libc::fcntl(raw, libc::F_ADD_SEALS, seals) }, 0);
    let fd = malformed.into_raw_fd();
    assert!(unsafe { take_preload_bootstrap_with_log() }.is_err());
    assert_closed(fd);
    assert!(unsafe { libc::fcntl(guest.as_raw_fd(), libc::F_GETFD) } >= 0);
    assert!(crate::guest_log::identity(guest.as_raw_fd()).is_ok());
}
