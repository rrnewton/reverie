use std::future::Future;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::pin::Pin;
use std::process::Stdio;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use std::time::Instant;

use reverie_rpc_transport::guest_log::LogSink;
use reverie_rpc_transport::guest_log::Options;
use reverie_rpc_transport::guest_log::retained_log;
use tokio::io::AsyncRead;
use tokio::io::ReadBuf;

use super::*;
use crate::LiteinstBackend;
use crate::backend::Command;
use crate::backend::logged::LoggedRunError;
use crate::backend::logged::Owner;
use crate::backend::logged::prepare_command;
use crate::backend::logged::read_output;
use crate::backend::logged::select_rpc_error;

type Launch = Pin<Box<dyn Future<Output = Result<(std::process::Output, ()), LoggedRunError>>>>;

fn adapter(inherited: bool, sink: LogSink) -> (RunObserver, Launch) {
    if inherited {
        let (observer, future) =
            LiteinstBackend::prepare_with_inherited_stdio_and_preload_data_and_log_sink::<()>(
                Command::new("/bin/true"),
                (),
                "/missing-run-evidence-preload",
                Vec::new(),
                sink,
            );
        (observer, Box::pin(future))
    } else {
        let (observer, future) =
            LiteinstBackend::prepare_with_output_and_preload_data_and_log_sink::<()>(
                Command::new("/bin/true"),
                (),
                "/missing-run-evidence-preload",
                Vec::new(),
                sink,
            );
        (observer, Box::pin(future))
    }
}

#[test]
fn both_prepared_adapters_retain_unpolled_and_startup_failure_facts() {
    for inherited in [false, true] {
        for poll in [false, true] {
            let (sink, handle) = retained_log(Options::bounded(1024));
            let (observer, future) = adapter(inherited, sink);
            let before = observer.try_snapshot().unwrap();
            assert!(!before.polled);
            assert!(!before.spawned);
            assert!(before.wait_status.is_none());
            let expected = if inherited {
                StreamState::Inherited
            } else {
                StreamState::NotStarted
            };
            assert_eq!(before.stdout.state, expected);
            assert_eq!(before.stderr.state, expected);
            if poll {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let error = runtime.block_on(future).unwrap_err();
                drop(runtime);
                let after = observer.try_snapshot().unwrap();
                let RunCompletion::Failed(failure) = after.completion else {
                    panic!("startup failure missing")
                };
                assert_eq!(failure.display, error.to_string());
                assert!(matches!(error.cause, reverie::Error::Io(_)));
                assert!(!after.caller_cancelled);
                assert!(after.polled);
            } else {
                drop(future);
                let after = observer.try_snapshot().unwrap();
                assert!(after.caller_cancelled);
                assert_eq!(after.completion, RunCompletion::Interrupted);
            }
            let after = observer.try_snapshot().unwrap();
            assert_eq!(after.stdout.state, expected);
            assert_eq!(after.stderr.state, expected);
            assert!(!after.reaped);
            assert!(after.wait_status.is_none());
            assert!(!handle.snapshot().root_reaped);
        }
    }
}

#[tokio::test]
async fn ordinary_child_eof_keeps_empty_capture_and_actual_wait_through_run_error() {
    let (sink, handle) = retained_log(Options::bounded(1024));
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", "printf 'out\\000tail'; exit 7"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (observer, future) =
        prepare_command::<()>(|| Ok(command), (), Vec::new(), sink, StdioMode::Captured);
    let error = future.await.unwrap_err();
    let snapshot = observer.try_snapshot().unwrap();
    assert_eq!(snapshot.stdout.state, StreamState::Eof);
    assert_eq!(snapshot.stderr.state, StreamState::Eof);
    assert_eq!(snapshot.stdout.bytes(), b"out\0tail");
    assert_eq!(snapshot.stderr.bytes(), b"");
    assert_eq!(error.stdout, snapshot.stdout.bytes());
    assert_eq!(error.stderr, snapshot.stderr.bytes());
    assert!(snapshot.reaped);
    assert_eq!(snapshot.wait_status.unwrap().code(), Some(7));
    assert!(handle.snapshot().root_reaped);
    assert!(matches!(snapshot.completion, RunCompletion::Failed(_)));
}

#[test]
fn ordinary_child_prefix_observed_before_cancellation_survives_runtime_drop() {
    let (sink, handle) = retained_log(Options::bounded(1024));
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args([
            "-c",
            "printf 'out\\000prefix'; printf 'err\\377prefix' >&2; sleep 30",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (observer, future) =
        prepare_command::<()>(|| Ok(command), (), Vec::new(), sink, StdioMode::Captured);
    assert!(!observer.try_snapshot().unwrap().polled);
    let before = runtime.block_on(async {
        let mut future = Box::pin(future);
        let before = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                tokio::select! {
                    result = &mut future => panic!("ordinary blocked child returned: {result:?}"),
                    _ = tokio::task::yield_now() => {},
                }
                if let Ok(snapshot) = observer.try_snapshot()
                    && snapshot.stdout.bytes() == b"out\0prefix"
                    && snapshot.stderr.bytes() == b"err\xffprefix"
                {
                    break snapshot;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(before.stdout.state, StreamState::Reading);
        assert_eq!(before.stderr.state, StreamState::Reading);
        assert!(!before.reaped);
        drop(future);
        before
    });
    drop(runtime);
    let deadline = Instant::now() + Duration::from_secs(2);
    let after = loop {
        let snapshot = observer.try_snapshot().unwrap();
        if snapshot.reaped {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "owned worker did not reap ordinary child after caller cancellation"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(after.stdout.bytes(), before.stdout.bytes());
    assert_eq!(after.stderr.bytes(), before.stderr.bytes());
    assert_eq!(after.stdout.state, StreamState::Interrupted);
    assert_eq!(after.stderr.state, StreamState::Interrupted);
    assert_eq!(after.completion, RunCompletion::Interrupted);
    assert!(after.caller_cancelled);
    assert!(after.worker_submitted);
    assert!(after.reaped);
    assert_eq!(after.wait_status.unwrap().signal(), Some(libc::SIGKILL));
    assert!(handle.snapshot().root_reaped);
}

#[tokio::test]
async fn ordinary_inherited_child_has_no_captured_stream_even_after_reap() {
    let (sink, _handle) = retained_log(Options::bounded(1024));
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", "exit 0"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let (observer, future) =
        prepare_command::<()>(|| Ok(command), (), Vec::new(), sink, StdioMode::Inherited);
    let result = future.await;
    assert!(result.is_err());
    let snapshot = observer.try_snapshot().unwrap();
    assert!(snapshot.reaped);
    assert_eq!(snapshot.wait_status.unwrap().code(), Some(0));
    assert_eq!(snapshot.stdout.state, StreamState::Inherited);
    assert_eq!(snapshot.stderr.state, StreamState::Inherited);
    assert!(snapshot.stdout.chunks.is_empty());
    assert!(snapshot.stderr.chunks.is_empty());
}

#[tokio::test]
async fn read_error_retains_prefix_and_original_failure_before_cleanup() {
    struct FailedReader;
    impl AsyncRead for FailedReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::other("original read error")))
        }
    }
    use tokio::io::AsyncReadExt;
    let observer = RunObserver::new(StdioMode::Captured);
    let reader = std::io::Cursor::new(b"prefix\0".to_vec()).chain(FailedReader);
    let error = read_output(reader, observer.clone(), Stream::Stdout)
        .await
        .unwrap_err();
    observer.failed(&io::Error::other("later cleanup error"));
    let snapshot = observer.try_snapshot().unwrap();
    assert_eq!(snapshot.stdout.bytes(), b"prefix\0");
    let StreamState::ReadFailed(failure) = snapshot.stdout.state else {
        panic!("read failure missing")
    };
    assert_eq!(failure.display, error.to_string());
    assert_eq!(snapshot.first_error.unwrap().display, error.to_string());
    assert_eq!(snapshot.stderr.state, StreamState::NotStarted);
}

#[test]
fn snapshot_never_waits_for_publication_lock() {
    let observer = RunObserver::new(StdioMode::Captured);
    let guard = observer.state.lock().unwrap();
    assert_eq!(
        observer.try_snapshot().unwrap_err(),
        SnapshotUnavailable::Busy
    );
    drop(guard);
    assert!(observer.try_snapshot().is_ok());
}

#[test]
fn caller_cancellation_remains_terminal_during_worker_cleanup() {
    let observer = RunObserver::new(StdioMode::Captured);
    observer.polled();
    observer.worker_submitted();
    observer.stream_state(Stream::Stdout, StreamState::Reading);
    observer.stream_state(Stream::Stderr, StreamState::Reading);
    observer.dropped(true);
    let cleanup_error = reverie::Error::from(io::Error::other("worker cleanup failure"));
    observer.stream_state(Stream::Stdout, StreamState::Eof);
    observer.stream_state(
        Stream::Stderr,
        StreamState::ReadFailed(FailureEvidence::new(&cleanup_error)),
    );
    observer.finished(Some(&cleanup_error));

    let snapshot = observer.try_snapshot().unwrap();
    assert!(snapshot.caller_cancelled);
    assert_eq!(snapshot.completion, RunCompletion::Interrupted);
    assert_eq!(snapshot.stdout.state, StreamState::Interrupted);
    assert_eq!(snapshot.stderr.state, StreamState::Interrupted);
    assert_eq!(
        snapshot.first_error,
        Some(FailureEvidence::new(&cleanup_error))
    );
}

#[test]
fn absent_runtime_uses_owned_worker_without_child_spawn_after_cancellation() {
    use std::sync::mpsc;

    let (sink, _handle) = retained_log(Options::bounded(1024));
    let (entered, entry) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let prepared =
        crate::backend::logged::PreparedCommand::new(std::process::Command::new("/bin/true"), ())
            .with_spawn_check(move |_, _| {
                entered.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(2)).unwrap();
                Ok(())
            });
    let (observer, future) = LiteinstBackend::prepare_with_owned_command_data_and_log_sink::<(), _>(
        prepared,
        (),
        Vec::new(),
        sink,
        StdioMode::Captured,
    );
    let mut future = Box::pin(future);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut context = Context::from_waker(std::task::Waker::noop());
        future.as_mut().poll(&mut context)
    }));
    assert!(matches!(result, Ok(Poll::Pending)));
    entry.recv_timeout(Duration::from_secs(2)).unwrap();
    let submitted = observer.try_snapshot().unwrap();
    assert!(submitted.polled);
    assert!(submitted.worker_submitted);
    assert!(!submitted.caller_cancelled);
    assert_eq!(submitted.completion, RunCompletion::Pending);
    assert!(!submitted.spawned);
    drop(future);
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let snapshot = loop {
        let snapshot = observer.try_snapshot().unwrap();
        if snapshot.completion == RunCompletion::Interrupted && snapshot.first_error.is_some() {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "owned worker did not finish cancellation before spawn"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(snapshot.polled);
    assert!(snapshot.worker_submitted);
    assert!(snapshot.caller_cancelled);
    assert_eq!(snapshot.completion, RunCompletion::Interrupted);
    assert!(!snapshot.spawned);
    assert!(!snapshot.reaped);
    assert!(snapshot.wait_status.is_none());
    assert_eq!(snapshot.stdout.state, StreamState::NotStarted);
}

#[tokio::test]
async fn unpolled_reader_is_interrupted_not_empty_eof() {
    let observer = RunObserver::new(StdioMode::Captured);
    let reader = read_output(tokio::io::empty(), observer.clone(), Stream::Stdout);
    drop(reader);
    let snapshot = observer.try_snapshot().unwrap();
    assert_eq!(snapshot.stdout.state, StreamState::Interrupted);
    assert!(snapshot.stdout.chunks.is_empty());
    read_output(tokio::io::empty(), observer.clone(), Stream::Stderr)
        .await
        .unwrap();
    assert_eq!(
        observer.try_snapshot().unwrap().stderr.state,
        StreamState::Eof
    );
}

fn late_rpc_boundary(cancel: bool, earlier: bool) {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let observer = RunObserver::new(StdioMode::Captured);
    runtime.block_on(async {
        read_output(
            std::io::Cursor::new(b"out\0prefix"),
            observer.clone(),
            Stream::Stdout,
        )
        .await
        .unwrap();
        read_output(
            std::io::Cursor::new(b"err\xffprefix"),
            observer.clone(),
            Stream::Stderr,
        )
        .await
        .unwrap();
    });
    let expected = reverie::Error::from(io::Error::other(if earlier {
        "earlier primary error"
    } else {
        "retained RPC connection failure"
    }));
    let expected_diagnostic = FailureEvidence::new(&expected);
    let (sink, handle) = retained_log(Options::bounded(1024));
    let (selected, entered) = tokio::sync::oneshot::channel();
    let (release, cleanup) = tokio::sync::oneshot::channel();
    let cleaned = Arc::new(AtomicBool::new(false));
    let worker_observer = observer.clone();
    let worker_cleaned = cleaned.clone();
    let task = runtime.spawn(async move {
        let mut owner = Owner {
            handle,
            evidence: worker_observer.clone(),
            completed: false,
            caller: false,
        };
        let mut first_error = if earlier { Some(expected) } else { None };
        if let Some(error) = &first_error {
            worker_observer.failed(error);
        }
        select_rpc_error(&mut first_error, true, &worker_observer);
        selected.send(()).unwrap();
        cleanup.await.unwrap();
        worker_cleaned.store(true, Ordering::Release);
        let result: Result<(), reverie::Error> = Err(first_error.unwrap());
        worker_observer.finished(result.as_ref().err());
        owner.complete();
        result
    });
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
    });
    let before = observer.try_snapshot().unwrap();
    assert!(!cleaned.load(Ordering::Acquire));
    assert_eq!(before.completion, RunCompletion::Pending);
    let returned = if cancel {
        drop(task);
        None
    } else {
        release.send(()).unwrap();
        Some(runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
        }))
    };
    drop(runtime);
    drop(sink);
    let after = observer.try_snapshot().unwrap();
    for snapshot in [&before, &after] {
        assert_eq!(snapshot.stdout.bytes(), b"out\0prefix");
        assert_eq!(snapshot.stderr.bytes(), b"err\xffprefix");
        assert!(!snapshot.reaped);
        assert!(snapshot.wait_status.is_none());
    }
    if let Some(error) = returned {
        assert!(matches!(error, reverie::Error::Io(_)));
        assert_eq!(FailureEvidence::new(&error), expected_diagnostic);
        assert!(cleaned.load(Ordering::Acquire));
        assert_eq!(
            after.completion,
            RunCompletion::Failed(expected_diagnostic.clone())
        );
    } else {
        assert!(!cleaned.load(Ordering::Acquire));
        assert_eq!(after.completion, RunCompletion::Interrupted);
    }
    assert_eq!(
        (before.first_error, after.first_error),
        (Some(expected_diagnostic.clone()), Some(expected_diagnostic)),
        "selected primary error must survive the awaited cleanup boundary"
    );
}

#[test]
fn late_rpc_error_survives_runtime_destruction_at_cleanup_boundary() {
    late_rpc_boundary(true, false);
}

#[test]
fn late_rpc_error_is_retained_before_successful_cleanup_and_returned_unchanged() {
    late_rpc_boundary(false, false);
}

#[test]
fn late_rpc_error_does_not_replace_an_earlier_primary() {
    late_rpc_boundary(false, true);
}
