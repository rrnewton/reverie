use std::io::Write;
use std::sync::Condvar;
use std::sync::atomic::AtomicBool;

use super::*;

fn options() -> CaptureOptions {
    CaptureOptions {
        limits: CaptureLimits {
            producers: 4,
            slots_per_producer: 16,
            max_record_bytes: 4096,
            host_pending_bytes: 8192,
            guest_pending_bytes: 8192,
            pending_records: 8,
            diagnostic_bytes: 64,
        },
        timeouts: CaptureTimeouts {
            startup: Duration::from_millis(80),
            blocked_publication: Duration::from_millis(40),
            final_drain: Duration::from_millis(40),
        },
    }
}

#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}
impl Gate {
    fn block(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        while !state.1 {
            state = self.changed.wait(state).unwrap();
        }
    }
    fn entered(&self) -> bool {
        self.state.lock().unwrap().0
    }
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}
fn until(mut predicate: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < end, "bounded control did not settle");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Block {
    None,
    Progress,
    Write,
    Flush,
    Drop,
}
struct Destination {
    bytes: Arc<Mutex<Vec<u8>>>,
    calls: Arc<Mutex<Vec<(String, std::thread::ThreadId)>>>,
    progress: DestinationProgress,
    block: Block,
    gate: Arc<Gate>,
    destroyed: Arc<AtomicBool>,
}
impl Destination {
    fn new(block: Block) -> Self {
        Self {
            bytes: Arc::default(),
            calls: Arc::default(),
            progress: DestinationProgress::default(),
            block,
            gate: Arc::default(),
            destroyed: Arc::default(),
        }
    }
    fn called(&self, call: &str, at: Block) {
        self.calls
            .lock()
            .unwrap()
            .push((call.into(), std::thread::current().id()));
        if self.block == at {
            self.gate.block();
        }
    }
}
impl Write for Destination {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.called("write", Block::Write);
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        self.progress.acknowledged_data_bytes += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.called("flush", Block::Flush);
        Ok(())
    }
}
impl CaptureDestination for Destination {
    fn progress(&self) -> DestinationProgress {
        self.called("progress", Block::Progress);
        self.progress
    }
}
impl Drop for Destination {
    fn drop(&mut self) {
        self.called("drop", Block::Drop);
        assert!(
            !self.destroyed.swap(true, Ordering::SeqCst),
            "double destination destruction"
        );
    }
}
fn guest(sink: &mut LogSink) -> (UnixStream, ordered::Writer) {
    let socket = sink.take_prepared_endpoint().unwrap().unwrap();
    let buffer = unsafe { ordered::Buffer::receive(socket.as_raw_fd()) }.unwrap();
    let writer = unsafe { buffer.activate(1, i64::from(std::process::id())) }.unwrap();
    (socket, writer)
}
fn wait(_: &super::super::SharedBuffer, _: u32) -> Result<(), PublishError> {
    std::thread::sleep(Duration::from_millis(1));
    Ok(())
}
fn terminal(owner: &mut CaptureOwner) -> CaptureReport {
    let now = Instant::now();
    let report = owner.finish_until(now + Duration::from_millis(100));
    assert!(
        now.elapsed() < Duration::from_secs(1),
        "explicit finish blocked in destination code"
    );
    assert!(!report.qualifies());
    report
}

#[test]
fn prepared_prefix_uses_real_source_order_and_zero_acknowledgment() {
    let (mut owner, mut sink, host) = unsafe { prepare_capture_unstarted(options()) }.unwrap();
    let original = [format!("seed={}\n", 100), format!("startup value={}\n", 7)];
    for (i, line) in original.iter().enumerate() {
        assert_eq!(
            host.write_record(line.as_bytes()).unwrap().order,
            i as u64 + 1
        );
    }
    let before = owner.handle().capture_snapshot().unwrap();
    assert_eq!(before.lifecycle, CaptureLifecycle::Prepared);
    assert_eq!(before.collector_worker, WorkerState::NeverCreated);
    assert_eq!(before.publication.worker, WorkerState::NeverCreated);
    assert!(!before.publication.ready && !before.collector_finished && !before.commits_observed);
    assert_eq!(before.publication.progress.acknowledged_data_bytes, 0);
    let output = Destination::new(Block::None);
    let bytes = output.bytes.clone();
    owner.start_workers(output).unwrap();
    let (socket, mut writer) = guest(&mut sink);
    assert_eq!(writer.write_record(b"guest\n", wait).unwrap().order, 3);
    writer.finish(wait).unwrap();
    drop(socket);
    // This unit control does not supply a fake reap; the separately spawned
    // fresh-child integration control proves the full qualification predicate.
    let report = owner.finish_until(Instant::now() + Duration::from_secs(2));
    assert!(!report.qualifies());
    assert!(!report.guest.root_reaped);
    assert_eq!(report.guest.phase, Phase::Complete);
    assert_eq!(report.publication.stability, ArtifactStability::Stable);
    assert_eq!(report.collector_worker, WorkerState::Joined);
    assert_eq!(report.publication.worker, WorkerState::Joined);
    assert_eq!(
        &*bytes.lock().unwrap(),
        format!("{}{}guest\n", original[0], original[1]).as_bytes()
    );
}

#[tokio::test]
async fn never_started_finish_and_drop_wake_both_waiters_without_eof() {
    for drop_owner in [false, true] {
        let (mut owner, mut sink, host) = unsafe { prepare_capture_unstarted(options()) }.unwrap();
        host.write_record(b"committed prefix").unwrap();
        let socket = sink.take_prepared_endpoint().unwrap().unwrap();
        let fd = socket.as_raw_fd();
        let alias = socket.try_clone().unwrap();
        let handle = owner.handle();
        let guest_wait = handle.guest_finished();
        let legacy_wait = handle.finished();
        if drop_owner {
            drop(owner);
        } else {
            terminal(&mut owner);
        }
        let guest = tokio::time::timeout(Duration::from_secs(1), guest_wait)
            .await
            .unwrap();
        let legacy = tokio::time::timeout(Duration::from_secs(1), legacy_wait)
            .await
            .unwrap();
        assert_eq!(guest.phase, Phase::Incomplete);
        assert!(!guest.peer_closed && !guest.root_reaped);
        assert!(legacy.terminal() && !legacy.qualifies());
        assert!(handle.ready().await.is_err());
        let report = handle.capture_snapshot().unwrap();
        assert_eq!(report.collector_worker, WorkerState::NeverCreated);
        assert_eq!(report.publication.worker, WorkerState::NeverCreated);
        assert!(
            !report.publication.ready && !report.publication.drained && !report.collector_finished
        );
        assert!(!report.commits_observed);
        assert_eq!(report.streams[0].unread_frames, 3);
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, libc::FD_CLOEXEC);
        assert!(alias.peer_addr().is_ok());
    }
}

#[test]
fn prestart_capacity_and_lock_failures_are_nonwaiting_and_sticky() {
    // The publication budget is deliberately much longer than the bound below;
    // the control catches using the running wait loop while no worker exists.
    for mode in [
        "partial-begin",
        "missing-end",
        "ring",
        "bytes",
        "records",
        "oversized",
        "lock",
    ] {
        let mut settings = options();
        settings.timeouts.blocked_publication = Duration::from_secs(5);
        match mode {
            "partial-begin" => settings.limits.slots_per_producer = 1,
            "missing-end" => settings.limits.slots_per_producer = 2,
            "ring" => settings.limits.slots_per_producer = 3,
            "bytes" => {
                settings.limits.max_record_bytes = 4;
                settings.limits.host_pending_bytes = 4;
            }
            "records" => settings.limits.pending_records = 2,
            _ => {}
        }
        let (mut owner, _sink, host) = unsafe { prepare_capture_unstarted(settings) }.unwrap();
        if ["ring", "bytes", "records"].contains(&mode) {
            host.write_record(b"abcd").unwrap();
        }
        let lock = (mode == "lock").then(|| owner.shared.host.lock().unwrap());
        let now = Instant::now();
        let result = if mode == "oversized" {
            host.write_record(&vec![0; 4097])
        } else {
            host.write_record(b"x")
        };
        assert!(result.is_err(), "{mode}");
        assert!(
            now.elapsed() < Duration::from_millis(250),
            "{mode} waited for absent collector"
        );
        drop(lock);
        let report = terminal(&mut owner);
        assert!(report.error.is_some(), "{mode}: {report:?}");
        assert!(!report.commits_observed && !report.collector_finished);
        assert!(!report.streams[0].finished);
        if mode == "partial-begin" {
            assert_eq!(report.streams[0].unread_frames, 1);
        }
        if mode == "missing-end" {
            assert_eq!(report.streams[0].unread_frames, 2);
        }
        let destination = Destination::new(Block::Drop);
        let gate = destination.gate.clone();
        let dropped = destination.destroyed.clone();
        let mut error = owner.start_workers(destination).unwrap_err();
        assert_eq!(
            error.destination_ownership(),
            DestinationOwnership::Recoverable
        );
        assert!(!gate.entered() && !dropped.load(Ordering::SeqCst));
        let recovered = error.take_destination().unwrap();
        gate.release();
        drop(recovered);
        assert!(dropped.load(Ordering::SeqCst));
    }
}

#[test]
fn wrapper_preparation_error_returns_original_blocking_drop_destination() {
    let mut invalid = options();
    invalid.timeouts.startup = Duration::ZERO;
    let destination = Destination::new(Block::Drop);
    let gate = destination.gate.clone();
    let bytes = destination.bytes.clone();
    let mut error = unsafe { prepared_capture(invalid, destination) }
        .err()
        .unwrap();
    assert!(error.handle.is_none());
    assert_eq!(
        error.destination_ownership(),
        DestinationOwnership::Recoverable
    );
    assert!(!gate.entered());
    let mut recovered = error.take_destination().unwrap();
    recovered.write_all(b"same object").unwrap();
    assert_eq!(&*bytes.lock().unwrap(), b"same object");
    assert!(error.take_destination().is_none());
    gate.release();
    drop(recovered);
}

#[test]
fn each_creation_readiness_and_handoff_failure_preserves_one_destination_owner() {
    for boundary in [
        "output-spawn",
        "collector-spawn",
        "output-ready",
        "collector-ready",
        "offered",
        "handoff-timeout",
        "taken",
    ] {
        let (mut owner, _sink, host) = unsafe { prepare_capture_unstarted(options()) }.unwrap();
        host.write_record(b"prefix").unwrap();
        let destination = Destination::new(Block::Drop);
        let destination_gate = destination.gate.clone();
        let destroyed = destination.destroyed.clone();
        let bytes = destination.bytes.clone();
        let calls = destination.calls.clone();
        let worker_gate = Arc::new(Gate::default());
        let mut hooks = StartHooks::default();
        if boundary.ends_with("spawn") {
            let spawn: SpawnWorker = Box::new(|_| Err(io::Error::from_raw_os_error(libc::EAGAIN)));
            if boundary == "output-spawn" {
                hooks.output_spawn = spawn;
            } else {
                hooks.collector_spawn = spawn;
            }
        }
        if boundary.ends_with("ready") {
            let gate = worker_gate.clone();
            let spawn: SpawnWorker = Box::new(move |worker| {
                Ok(std::thread::spawn(move || {
                    gate.block();
                    worker();
                }))
            });
            if boundary == "output-ready" {
                hooks.output_spawn = spawn;
            } else {
                hooks.collector_spawn = spawn;
            }
        }
        if boundary == "handoff-timeout" {
            let gate = worker_gate.clone();
            hooks.before_take = Box::new(move || gate.block());
        }
        if boundary == "offered" {
            hooks.after_offer = Box::new(|shared| {
                shared.publication.revoke("injected Offered cancellation");
                shared.stop_guest();
            });
        }
        if boundary == "taken" {
            hooks.after_taken = Box::new(|shared| {
                shared.stop_guest();
            });
        }
        let start = Instant::now();
        let mut error = owner.start_with(Box::new(destination), hooks).unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "{boundary}: caller ran arbitrary Drop"
        );
        if boundary.ends_with("spawn") {
            assert_eq!(error.cause.raw_os_error(), Some(libc::EAGAIN));
        }
        let report = owner.handle().capture_snapshot().unwrap();
        assert!(!report.qualifies());
        if boundary == "taken" {
            assert_eq!(error.destination_ownership(), DestinationOwnership::Worker);
            assert!(error.take_destination().is_none());
            assert_eq!(report.publication.stability, ArtifactStability::MayAppend);
            assert!(!destroyed.load(Ordering::SeqCst));
        } else {
            assert_eq!(
                error.destination_ownership(),
                DestinationOwnership::Recoverable,
                "{boundary}"
            );
            assert!(!destination_gate.entered() && !destroyed.load(Ordering::SeqCst));
            assert!(calls.lock().unwrap().is_empty());
            let mut recovered = error.take_destination().unwrap();
            recovered.write_all(b"recovered exact object").unwrap();
            assert_eq!(&*bytes.lock().unwrap(), b"recovered exact object");
            destination_gate.release();
            drop(recovered);
        }
        // Record failure first; only the test's external owner can unblock Drop
        // or a delayed worker initialization for cleanup.
        destination_gate.release();
        worker_gate.release();
        until(|| {
            owner.shared.publication.worker_finished()
                || owner.shared.publication.snapshot().worker == WorkerState::NeverCreated
        });
        until(|| {
            owner.shared.join_finished()
                || owner.shared.state.lock().unwrap().collector_worker == WorkerState::NeverCreated
        });
        assert!(destroyed.load(Ordering::SeqCst));
        let after = owner.finish_until(Instant::now() + Duration::from_secs(1));
        assert!(!after.qualifies());
        assert_eq!(after.publication.stability, report.publication.stability);
        assert_eq!(after.publication.progress, report.publication.progress);
        if boundary == "taken" {
            assert!(
                calls
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|(_, thread)| *thread != std::thread::current().id())
            );
        }
    }
}

#[test]
fn duplicate_start_returns_second_destination_without_replacing_first() {
    let (mut owner, mut sink, host) = unsafe { prepare_capture_unstarted(options()) }.unwrap();
    let first = Destination::new(Block::None);
    let first_bytes = first.bytes.clone();
    owner.start_workers(first).unwrap();
    let second = Destination::new(Block::Drop);
    let second_bytes = second.bytes.clone();
    let gate = second.gate.clone();
    let mut error = owner.start_workers(second).unwrap_err();
    assert_eq!(
        error.destination_ownership(),
        DestinationOwnership::Recoverable
    );
    assert!(!gate.entered());
    assert_eq!(
        owner.handle().capture_snapshot().unwrap().lifecycle,
        CaptureLifecycle::Running
    );
    host.write_record(b"first only").unwrap();
    let (socket, mut writer) = guest(&mut sink);
    writer.finish(wait).unwrap();
    drop(socket);
    let report = owner.finish_until(Instant::now() + Duration::from_secs(1));
    assert!(report.publication.error.is_none());
    assert_eq!(&*first_bytes.lock().unwrap(), b"first only");
    assert!(second_bytes.lock().unwrap().is_empty());
    gate.release();
    drop(error.take_destination());
}

#[test]
fn first_progress_write_flush_and_destructor_have_bounded_frozen_failure() {
    for block in [Block::Progress, Block::Write, Block::Flush, Block::Drop] {
        let (mut owner, mut sink, host) = unsafe { prepare_capture_unstarted(options()) }.unwrap();
        host.write_record(b"original").unwrap();
        let output = Destination::new(block);
        let gate = output.gate.clone();
        let bytes = output.bytes.clone();
        let calls = output.calls.clone();
        let destroyed = output.destroyed.clone();
        owner.start_workers(output).unwrap();
        let (socket, mut writer) = guest(&mut sink);
        writer.finish(wait).unwrap();
        drop(socket);
        if block == Block::Progress || block == Block::Write {
            until(|| gate.entered());
        }
        let report = terminal(&mut owner);
        assert!(gate.entered());
        assert_eq!(report.publication.stability, ArtifactStability::MayAppend);
        assert!(report.publication.error.is_some());
        if block == Block::Progress {
            assert!(report.publication.attempt.is_none());
            assert_eq!(report.publication.progress.acknowledged_data_bytes, 0);
            assert_eq!(report.publication.unpublished_bytes, 8);
            assert_eq!(report.publication.first_unpublished_order, Some(1));
            assert!(bytes.lock().unwrap().is_empty());
            assert!(
                report
                    .publication
                    .error
                    .as_ref()
                    .unwrap()
                    .contains("publication budget")
            );
        }
        if block == Block::Write {
            assert_eq!(report.publication.attempt.unwrap().acknowledged, 0);
            assert_eq!(report.publication.unpublished_bytes, 0);
        }
        if block == Block::Flush || block == Block::Drop {
            assert_eq!(report.publication.progress.acknowledged_data_bytes, 8);
        }
        assert!(!destroyed.load(Ordering::SeqCst));
        gate.release();
        until(|| owner.shared.publication.worker_finished());
        until(|| owner.shared.join_finished());
        let after = owner.finish_until(Instant::now() + Duration::from_secs(2));
        assert_eq!(after.publication.progress, report.publication.progress);
        assert_eq!(after.publication.attempt, report.publication.attempt);
        assert_eq!(
            after.publication.unpublished_bytes,
            report.publication.unpublished_bytes
        );
        assert_eq!(after.publication.stability, ArtifactStability::MayAppend);
        assert!(!after.qualifies());
        assert!(destroyed.load(Ordering::SeqCst));
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|(_, thread)| *thread != std::thread::current().id())
        );
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(call, _)| call == "write")
                .count()
                <= 1
        );
    }
}

#[test]
fn invalid_preparation_checks_size_and_deadline_overflow_without_workers() {
    for mode in ["sum", "producers", "slots", "duration", "diagnostics"] {
        let mut settings = options();
        match mode {
            "sum" => settings.limits.host_pending_bytes = usize::MAX,
            "producers" => settings.limits.producers = usize::MAX,
            "slots" => settings.limits.slots_per_producer = usize::MAX,
            "duration" => settings.timeouts.startup = Duration::MAX,
            "diagnostics" => settings.limits.diagnostic_bytes = 0,
            _ => unreachable!(),
        }
        let error = unsafe { prepare_capture_unstarted(settings) }
            .err()
            .unwrap();
        assert_eq!(
            error.destination_ownership(),
            DestinationOwnership::NotSupplied
        );
        assert!(error.handle.is_none(), "{mode}");
    }
}

struct Panicking {
    output: Destination,
    at: Block,
}
impl Write for Panicking {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert!(self.at != Block::Write, "injected write panic");
        self.output.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        assert!(self.at != Block::Flush, "injected flush panic");
        self.output.flush()
    }
}
impl CaptureDestination for Panicking {
    fn progress(&self) -> DestinationProgress {
        assert!(self.at != Block::Progress, "injected progress panic");
        self.output.progress()
    }
}
impl Drop for Panicking {
    fn drop(&mut self) {
        assert!(self.at != Block::Drop, "injected destructor panic");
    }
}
#[test]
fn every_destination_panic_is_incomplete_even_after_successful_flush() {
    for at in [Block::Progress, Block::Write, Block::Flush, Block::Drop] {
        let (mut owner, mut sink, host) = unsafe { prepare_capture_unstarted(options()) }.unwrap();
        host.write_record(b"original").unwrap();
        let output = Destination::new(Block::None);
        let destroyed = output.destroyed.clone();
        let calls = output.calls.clone();
        let start = owner.start_workers(Panicking { output, at });
        if let Err(error) = start {
            assert_eq!(error.destination_ownership(), DestinationOwnership::Worker);
        }
        // No guest is needed for this destination-panic control. Close only
        // this test's actual endpoint; never activate a writer after a panic
        // may already have closed admission.
        drop(sink.take_prepared_endpoint().unwrap());
        let report = owner.finish_until(Instant::now() + Duration::from_secs(1));
        assert!(!report.qualifies());
        assert!(
            report
                .publication
                .error
                .as_ref()
                .unwrap()
                .contains("panicked")
        );
        until(|| owner.shared.publication.worker_finished());
        until(|| owner.shared.join_finished());
        assert!(destroyed.load(Ordering::SeqCst));
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|(_, thread)| *thread != std::thread::current().id())
        );
    }
}
