use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

pub use ordered::RecordCommit;
pub use publication::Attempt as PublicationAttempt;
pub use publication::Report as PublicationReport;

use super::IssueKind;
use super::LogHandle;
use super::LogSink;
use super::Phase;
use super::PublishError;
use super::ReaderState;
use super::Retention;
use super::RunState;
use super::Stream;
use super::ordered;

pub(crate) mod publication;

#[derive(Clone, Copy, Debug)]
pub struct CaptureLimits {
    pub producers: usize,
    pub slots_per_producer: usize,
    pub max_record_bytes: usize,
    pub host_pending_bytes: usize,
    pub guest_pending_bytes: usize,
    pub pending_records: usize,
    pub diagnostic_bytes: usize,
}

impl CaptureLimits {
    fn ordered(self) -> ordered::Limits {
        ordered::Limits {
            producers: self.producers,
            slots: self.slots_per_producer,
            max_record_bytes: self.max_record_bytes,
            host_pending_bytes: self.host_pending_bytes,
            guest_pending_bytes: self.guest_pending_bytes,
            pending_records: self.pending_records,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CaptureTimeouts {
    pub startup: Duration,
    pub blocked_publication: Duration,
    pub final_drain: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct CaptureOptions {
    pub limits: CaptureLimits,
    pub timeouts: CaptureTimeouts,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DestinationProgress {
    pub acknowledged_data_bytes: u64,
    pub discarded_bytes: u64,
    pub marker_bytes: u64,
    pub marker_complete: bool,
    pub marker_failed: bool,
    pub output_ceiling: bool,
}

pub trait CaptureDestination: io::Write + Send + 'static {
    fn progress(&self) -> DestinationProgress;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactStability {
    Stable,
    MayAppend,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestStopReason {
    Cancelled,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug)]
pub struct GuestReport {
    pub phase: Phase,
    pub run: RunState,
    pub root_reaped: bool,
    pub peer_closed: bool,
    pub issues: Vec<super::Issue>,
    pub rpc_issues: Vec<crate::ConnectionIssue>,
}

impl GuestReport {
    pub fn terminal(&self) -> bool {
        matches!(self.phase, Phase::Complete | Phase::Incomplete)
    }
}

/// Worker observations are independent of whether a join handle is stored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerState {
    NeverCreated,
    Created,
    Ready,
    Ended,
    Joined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CaptureLifecycle {
    Prepared,
    Starting,
    Running,
    Finalizing,
    Terminal,
}

/// Who owns the exact destination after a failed explicit operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationOwnership {
    NotSupplied,
    Recoverable,
    Worker,
    Recovered,
}

#[derive(Clone, Debug)]
pub struct CaptureReport {
    pub lifecycle: CaptureLifecycle,
    pub collector_worker: WorkerState,
    pub guest: GuestReport,
    pub host: ordered::Admission,
    pub guest_admission: ordered::Admission,
    pub active_host_calls: usize,
    pub late_host_writes: u64,
    pub commits_observed: bool,
    pub collector_finished: bool,
    pub publication: PublicationReport,
    pub error: Option<String>,
    pub streams: Vec<Stream>,
    pub omitted_diagnostic_bytes: u64,
    pub omitted_issues: u64,
}

impl CaptureReport {
    pub fn qualifies(&self) -> bool {
        self.guest.phase == Phase::Complete
            && self.guest.run == RunState::Succeeded
            && self.guest.root_reaped
            && self.guest.peer_closed
            && self.guest.issues.is_empty()
            && self.guest.rpc_issues.is_empty()
            && self.host.closed
            && self.host.entrants == 0
            && self.guest_admission.closed
            && self.guest_admission.entrants == 0
            && self.active_host_calls == 0
            && self.late_host_writes == 0
            && self.commits_observed
            && self.collector_finished
            && self.error.is_none()
            && self.publication.error.is_none()
            && self.publication.drained
            && self.publication.stability == ArtifactStability::Stable
            && self.omitted_issues == 0
            && !self.publication.progress.output_ceiling
            && !self.publication.progress.marker_failed
    }
}

pub struct CaptureStartError {
    pub cause: io::Error,
    pub handle: Option<LogHandle>,
    destination: Mutex<Option<Box<dyn CaptureDestination>>>,
    ownership: DestinationOwnership,
}

impl CaptureStartError {
    pub fn destination_ownership(&self) -> DestinationOwnership {
        self.ownership
    }

    /// Take the original destination if transfer never happened. Its later Drop
    /// is the caller's operation and may block, just like its Write callbacks.
    pub fn take_destination(&mut self) -> Option<Box<dyn CaptureDestination>> {
        // Exclusive access recovers D without locking or invoking user code.
        let destination = self
            .destination
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if destination.is_some() {
            self.ownership = DestinationOwnership::Recovered;
        }
        destination
    }

    fn recover(mut self, destination: Box<dyn CaptureDestination>) -> Self {
        *self
            .destination
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(destination);
        self.ownership = DestinationOwnership::Recoverable;
        self
    }
}

impl std::fmt::Debug for CaptureStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureStartError")
            .field("cause", &self.cause)
            .field("has_evidence", &self.handle.is_some())
            .field("destination_ownership", &self.ownership)
            .finish()
    }
}
impl std::fmt::Display for CaptureStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cause.fmt(formatter)
    }
}
impl std::error::Error for CaptureStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}
impl From<io::Error> for CaptureStartError {
    fn from(cause: io::Error) -> Self {
        Self {
            cause,
            handle: None,
            destination: Mutex::new(None),
            ownership: DestinationOwnership::NotSupplied,
        }
    }
}

struct State {
    ready: bool,
    collector_worker: WorkerState,
    guest_phase: Phase,
    peer_closed: bool,
    guest_stop: Option<Instant>,
    deadline: Option<Instant>,
    commits_observed: bool,
    collector_finished: bool,
    error: Option<String>,
    streams: Vec<Stream>,
    omitted_diagnostic_bytes: u64,
}

// The reserved collector stays reachable from the original LogHandle even
// when a spawn closure is discarded or a prestart emitter outlives the owner.
// Cancellation retains diagnostics; it never returns the one collector token.
enum CollectorStart {
    Available {
        socket: UnixStream,
        collector: ordered::Collector,
    },
    Taken,
    Cancelled {
        collector: ordered::Collector,
    },
}

pub(super) struct Shared {
    retention: Weak<Retention>,
    options: CaptureOptions,
    buffer: Arc<ordered::Buffer>,
    publication: publication::Publication,
    state: Mutex<State>,
    lifecycle: AtomicU8,
    host: Mutex<ordered::Writer>,
    active_host_calls: AtomicUsize,
    late_host_writes: AtomicU64,
    pub(super) omitted_issues: AtomicU64,
    collector: Mutex<Option<JoinHandle<()>>>,
    collector_start: Mutex<CollectorStart>,
}

impl Shared {
    fn lifecycle(&self) -> CaptureLifecycle {
        match self.lifecycle.load(Ordering::Acquire) {
            0 => CaptureLifecycle::Prepared,
            1 => CaptureLifecycle::Starting,
            2 => CaptureLifecycle::Running,
            3 => CaptureLifecycle::Finalizing,
            _ => CaptureLifecycle::Terminal,
        }
    }
    pub(super) fn guest_stopped(&self) -> bool {
        self.buffer.guest_stopped()
    }
    pub(super) fn publication_finalized(&self) -> bool {
        self.publication.snapshot().finalized
    }
    fn notify(&self) {
        if let Some(retention) = self.retention.upgrade() {
            retention.changed.notify_waiters();
        }
    }

    pub(super) fn stop_guest(&self) {
        self.buffer.close(ordered::Role::Guest);
        let mut state = self.state.lock().unwrap();
        state.guest_stop.get_or_insert_with(Instant::now);
        if !matches!(state.guest_phase, Phase::Complete | Phase::Incomplete) {
            state.guest_phase = Phase::Draining;
        }
        if matches!(
            self.lifecycle(),
            CaptureLifecycle::Prepared | CaptureLifecycle::Starting
        ) {
            self.lifecycle
                .store(CaptureLifecycle::Finalizing as u8, Ordering::Release);
            self.buffer.close(ordered::Role::Host);
            state.guest_phase = Phase::Incomplete;
        }
        drop(state);
        self.notify();
    }

    fn fail(&self, message: &str) {
        self.state
            .lock()
            .unwrap()
            .error
            .get_or_insert_with(|| message.to_owned());
        if let Some(retention) = self.retention.upgrade() {
            LogHandle(retention).stop(IssueKind::Publication, message);
        } else {
            self.stop_guest();
        }
    }

    fn close(&self, deadline: Instant) {
        self.buffer.close(ordered::Role::Host);
        self.buffer.close(ordered::Role::Guest);
        let mut state = self.state.lock().unwrap();
        if self.lifecycle() != CaptureLifecycle::Terminal {
            self.lifecycle
                .store(CaptureLifecycle::Finalizing as u8, Ordering::Release);
        }
        state.deadline = Some(state.deadline.map_or(deadline, |old| old.min(deadline)));
        drop(state);
        self.notify();
    }

    fn take_collector(&self) -> Option<(UnixStream, ordered::Collector)> {
        let mut start = self.collector_start.lock().unwrap();
        if self.lifecycle() != CaptureLifecycle::Starting {
            return None;
        }
        match std::mem::replace(&mut *start, CollectorStart::Taken) {
            CollectorStart::Available { socket, collector } => Some((socket, collector)),
            other => {
                *start = other;
                None
            }
        }
    }

    fn cancel_unstarted_collector(&self) -> bool {
        let mut start = self.collector_start.lock().unwrap();
        let socket = match std::mem::replace(&mut *start, CollectorStart::Taken) {
            CollectorStart::Available { socket, collector } => {
                *start = CollectorStart::Cancelled { collector };
                Some(socket)
            }
            other => {
                *start = other;
                None
            }
        };
        let cancelled = matches!(*start, CollectorStart::Cancelled { .. });
        drop(start);
        // This is our descriptor only; caller endpoints and aliases survive.
        drop(socket);
        cancelled
    }

    fn refresh_unstarted_diagnostics(&self) {
        let start = self.collector_start.lock().unwrap();
        if let CollectorStart::Cancelled { collector } = &*start {
            let diagnostics = collector.diagnostics(self.options.limits.diagnostic_bytes);
            // One lock order (collector-start then state) prevents an older
            // concurrent sample overwriting a newer one. No user code, wait or
            // LogHandle operation is called while these locks are held.
            let mut state = self.state.lock().unwrap();
            (state.streams, state.omitted_diagnostic_bytes) = diagnostics;
        }
    }

    fn active_prestart_host(&self) -> bool {
        self.active_host_calls.load(Ordering::Acquire) != 0
            || self.buffer.admission(ordered::Role::Host).entrants != 0
    }

    fn join_finished(&self) -> bool {
        let mut thread = self.collector.lock().unwrap();
        if thread.as_ref().is_some_and(|thread| thread.is_finished()) {
            let _ = thread.take().unwrap().join();
        }
        if thread.is_none() {
            let mut state = self.state.lock().unwrap();
            if state.collector_worker != WorkerState::NeverCreated {
                state.collector_worker = WorkerState::Joined;
                return true;
            }
        }
        false
    }
}

pub struct CaptureOwner {
    handle: LogHandle,
    shared: Arc<Shared>,
    finalized: bool,
}

#[derive(Clone)]
pub struct HostProducer {
    handle: LogHandle,
    shared: Arc<Shared>,
}

impl HostProducer {
    pub fn record_failed(&self) {
        self.shared
            .fail("host formatter failed before complete-record commit");
    }
    pub fn write_record(&self, bytes: &[u8]) -> Result<RecordCommit, PublishError> {
        self.write_record_with(bytes, || {}, || {})
    }

    fn write_record_with(
        &self,
        bytes: &[u8],
        before_emit: impl FnOnce(),
        after_emit: impl FnOnce(),
    ) -> Result<RecordCommit, PublishError> {
        let closed = || self.shared.buffer.admission(ordered::Role::Host).closed;
        if closed() {
            let _ = self.shared.late_host_writes.try_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |count| count.checked_add(1),
            );
            self.handle.issue(
                IssueKind::Publication,
                "host record attempted after capture close",
            );
            return Err(PublishError::Stopped);
        }
        self.shared.active_host_calls.fetch_add(1, Ordering::AcqRel);
        before_emit();
        let result = self.write_inner(bytes);
        after_emit();
        self.shared
            .active_host_calls
            .fetch_sub(1, Ordering::Release);
        if result.is_err() {
            self.shared.fail("host record did not commit");
        }
        result
    }

    fn write_inner(&self, bytes: &[u8]) -> Result<RecordCommit, PublishError> {
        let deadline = Instant::now() + self.shared.options.timeouts.blocked_publication;
        // Snapshot before admission: a call begun before Running never waits
        // for workers whose launch intentionally follows this source prefix.
        let nonwaiting = self.shared.lifecycle() != CaptureLifecycle::Running;
        let mut writer = loop {
            if self.shared.buffer.admission(ordered::Role::Host).closed {
                return Err(PublishError::Stopped);
            }
            match self.shared.host.try_lock() {
                Ok(writer) => break writer,
                Err(std::sync::TryLockError::Poisoned(_)) => return Err(PublishError::Invalid),
                Err(std::sync::TryLockError::WouldBlock) => {
                    if nonwaiting || Instant::now() >= deadline {
                        return Err(PublishError::Full);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        };
        writer.write_record(bytes, |_, _| {
            if nonwaiting || Instant::now() >= deadline {
                return Err(PublishError::Full);
            }
            std::thread::sleep(Duration::from_millis(1));
            Ok(())
        })
    }
}

impl CaptureOwner {
    pub fn handle(&self) -> LogHandle {
        self.handle.clone()
    }

    /// Start both workers once, after actual root task-ID allocation if needed.
    /// On failure, inspect/take the destination from the returned error. This
    /// call returns explicit startup errors without invoking destination code
    /// or its destructor on the caller. General caller-unwind recovery and
    /// reuse of state poisoned by an earlier caught panic are not guaranteed.
    /// The owner is neither cloneable nor startable through shared access:
    ///
    /// ```compile_fail,E0599
    /// use reverie_rpc_transport::guest_log::CaptureOwner;
    /// fn duplicate(owner: CaptureOwner) { let _ = owner.clone(); }
    /// ```
    /// ```compile_fail,E0596
    /// use reverie_rpc_transport::guest_log::{CaptureOwner, CaptureDestination};
    /// fn shared_start<D: CaptureDestination>(owner: &CaptureOwner, destination: D) {
    ///     let _ = owner.start_workers(destination);
    /// }
    /// ```
    pub fn start_workers<D: CaptureDestination>(
        &mut self,
        destination: D,
    ) -> Result<(), CaptureStartError> {
        self.start_with(Box::new(destination), StartHooks::default())
    }

    fn start_with(
        &mut self,
        destination: Box<dyn CaptureDestination>,
        hooks: StartHooks,
    ) -> Result<(), CaptureStartError> {
        let deadline = Instant::now() + self.shared.options.timeouts.startup;
        {
            let state = self.shared.state.lock().unwrap();
            if self.shared.lifecycle() != CaptureLifecycle::Prepared || state.error.is_some() {
                let mut error: CaptureStartError =
                    io::Error::other("capture start capability already consumed or closed").into();
                error.handle = Some(self.handle());
                return Err(error.recover(destination));
            }
            self.shared
                .lifecycle
                .store(CaptureLifecycle::Starting as u8, Ordering::Release);
        }
        // Until offer(), D stays here. No worker spawn closure owns it.
        let mut destination = Some(destination);
        let result = (|| -> io::Result<()> {
            #[cfg(test)]
            self.shared
                .publication
                .spawn_with_ready_hook(hooks.output_spawn, hooks.before_take)?;
            #[cfg(not(test))]
            self.shared.publication.spawn_with(hooks.output_spawn)?;
            let shared = self.shared.clone();
            let thread =
                (hooks.collector_spawn)(Box::new(move || {
                    // Creation failure drops only this Shared reference. The
                    // original reservation remains available for diagnostics.
                    let collection = shared.take_collector();
                    let ran = collection.is_some();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if let Some((socket, collector)) = collection {
                            collect(&shared, socket, collector);
                        }
                    }));
                    if result.is_err() {
                        shared.fail("capture collector panicked");
                    }
                    {
                        let mut state = shared.state.lock().unwrap();
                        state.collector_finished = ran;
                        state.collector_worker = WorkerState::Ended;
                        if !matches!(state.guest_phase, Phase::Complete | Phase::Incomplete) {
                            state.guest_phase = Phase::Incomplete;
                        }
                    }
                    let deadline =
                        shared.state.lock().unwrap().deadline.unwrap_or_else(|| {
                            Instant::now() + shared.options.timeouts.final_drain
                        });
                    shared.publication.finish_until(deadline);
                    shared.notify();
                }))?;
            *self.shared.collector.lock().unwrap() = Some(thread);
            {
                let mut state = self.shared.state.lock().unwrap();
                if state.collector_worker == WorkerState::NeverCreated {
                    state.collector_worker = WorkerState::Created;
                }
            }
            if !self.shared.publication.wait_ready(deadline) {
                return Err(io::Error::other(
                    "capture destination startup failed/deadline",
                ));
            }
            loop {
                {
                    let state = self.shared.state.lock().unwrap();
                    if state.error.is_some()
                        || self.shared.lifecycle() != CaptureLifecycle::Starting
                    {
                        return Err(io::Error::other("capture closed during startup"));
                    }
                    if state.ready {
                        break;
                    }
                }
                if Instant::now() >= deadline || self.shared.join_finished() {
                    return Err(io::Error::other(
                        "capture collector startup failed/deadline",
                    ));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            self.shared.publication.offer_with_hook(
                destination.take().expect("caller owns destination"),
                || {
                    #[cfg(test)]
                    (hooks.after_offer)(&self.shared);
                },
            );
            if !self.shared.publication.wait_taken(deadline) {
                return Err(io::Error::other(
                    "capture destination handoff failed/deadline",
                ));
            }
            #[cfg(test)]
            (hooks.after_taken)(&self.shared);
            let state = self.shared.state.lock().unwrap();
            if self.shared.lifecycle() != CaptureLifecycle::Starting
                || state.error.is_some()
                || self.shared.publication.failed()
                || self.shared.buffer.guest_stopped()
                || self.shared.buffer.admission(ordered::Role::Host).closed
                || Instant::now() >= deadline
            {
                return Err(io::Error::other("capture closed before Running"));
            }
            self.shared
                .lifecycle
                .store(CaptureLifecycle::Running as u8, Ordering::Release);
            Ok(())
        })();
        if let Err(cause) = result {
            // Recovery and Take are serialized by the same private lock. Never
            // let an offered destination be destroyed by an internal Arc drop.
            let recovered = self.shared.publication.cancel_handoff();
            let destination = destination.or(recovered);
            let ownership = if destination.is_some() {
                DestinationOwnership::Recoverable
            } else if self.shared.publication.taken() {
                DestinationOwnership::Worker
            } else {
                DestinationOwnership::NotSupplied
            };
            self.handle.stop(IssueKind::Startup, &cause);
            self.shared
                .fail("capture worker startup did not reach Running");
            self.shared
                .publication
                .revoke("capture worker startup failed");
            self.finish_until(deadline);
            return Err(CaptureStartError {
                cause,
                handle: Some(self.handle()),
                destination: Mutex::new(destination),
                ownership,
            });
        }
        {
            let mut state = self.handle.0.state.lock().unwrap();
            state.report.reader = ReaderState::Ready;
            state.report.phase = Phase::Collecting;
        }
        self.shared.notify();
        Ok(())
    }

    pub fn finish_until(&mut self, deadline: Instant) -> CaptureReport {
        self.shared.close(deadline);
        let deadline = self
            .shared
            .state
            .lock()
            .unwrap()
            .deadline
            .expect("installed deadline");
        if self.shared.cancel_unstarted_collector() {
            while self.shared.active_prestart_host() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            if self.shared.active_prestart_host() {
                self.shared
                    .fail("prestart host emitter unsettled at diagnostic cutoff");
            }
            self.shared.refresh_unstarted_diagnostics();
            self.shared.state.lock().unwrap().guest_phase = Phase::Incomplete;
        }
        if self.shared.state.lock().unwrap().collector_worker == WorkerState::NeverCreated {
            self.shared.fail("capture collector never started");
            self.shared.state.lock().unwrap().guest_phase = Phase::Incomplete;
        } else {
            while !self.shared.join_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            if !self.shared.join_finished() {
                self.shared.fail("collector unsettled at final deadline");
            }
        }
        self.shared.publication.finish_until(deadline);
        self.shared
            .lifecycle
            .store(CaptureLifecycle::Terminal as u8, Ordering::Release);
        self.shared.notify();
        self.finalized = true;
        self.handle.capture_snapshot().expect("prepared capture")
    }
}

impl Drop for CaptureOwner {
    fn drop(&mut self) {
        if !self.finalized {
            self.handle.issue(
                IssueKind::Interrupted,
                "capture owner dropped without explicit finalization",
            );
            self.shared.stop_guest();
            self.shared.close(Instant::now());
            if self.shared.cancel_unstarted_collector()
                || self.shared.state.lock().unwrap().collector_worker == WorkerState::NeverCreated
            {
                // Never-started diagnostics remain owned by the original handle;
                // a delayed thread cannot take this cancelled collector.
                self.finish_until(Instant::now());
            } else {
                self.shared.publication.revoke("capture owner dropped");
                self.shared.publication.close();
            }
        }
    }
}

impl LogHandle {
    pub fn is_prepared_capture(&self) -> bool {
        self.0.capture.get().is_some()
    }

    pub fn capture_snapshot(&self) -> Option<CaptureReport> {
        let shared = self.0.capture.get()?;
        // A deadline does not freeze future source activity into nonexistence.
        // Refresh retained never-started cursor facts, without upgrading failure.
        shared.refresh_unstarted_diagnostics();
        let legacy = self.snapshot();
        let state = shared.state.lock().unwrap();
        Some(CaptureReport {
            lifecycle: shared.lifecycle(),
            collector_worker: state.collector_worker,
            guest: GuestReport {
                phase: state.guest_phase,
                run: legacy.run,
                root_reaped: legacy.root_reaped,
                peer_closed: state.peer_closed,
                issues: legacy.issues,
                rpc_issues: legacy.rpc_issues,
            },
            host: shared.buffer.admission(ordered::Role::Host),
            guest_admission: shared.buffer.admission(ordered::Role::Guest),
            active_host_calls: shared.active_host_calls.load(Ordering::Acquire),
            late_host_writes: shared.late_host_writes.load(Ordering::Acquire),
            commits_observed: state.commits_observed,
            collector_finished: state.collector_finished,
            publication: shared.publication.snapshot(),
            error: state.error.clone(),
            streams: state.streams.clone(),
            omitted_diagnostic_bytes: state.omitted_diagnostic_bytes,
            omitted_issues: shared.omitted_issues.load(Ordering::Acquire),
        })
    }

    pub fn request_guest_stop(&self, reason: GuestStopReason) {
        let (kind, run) = match reason {
            GuestStopReason::Cancelled => (IssueKind::Cancelled, RunState::Cancelled),
            GuestStopReason::Failed => (IssueKind::Child, RunState::Failed),
            GuestStopReason::Interrupted => (IssueKind::Interrupted, RunState::Interrupted),
        };
        self.run_state(run);
        self.stop(kind, "guest domain stop requested");
    }

    pub async fn guest_finished(&self) -> GuestReport {
        loop {
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(report) = self.capture_snapshot() {
                if report.guest.terminal() {
                    return report.guest;
                }
            } else {
                let report = self.finished().await;
                return GuestReport {
                    phase: report.phase,
                    run: report.run,
                    root_reaped: report.root_reaped,
                    peer_closed: report.peer_closed,
                    issues: report.issues,
                    rpc_issues: report.rpc_issues,
                };
            }
            notified.await;
        }
    }
}

/// Start the collector and destination worker before guest startup.
///
/// # Safety
/// The returned guest endpoint may be transferred only to trusted cooperating
/// writers under the [module contract](super). Every descriptor/mapping alias
/// must preserve initialized layout and the protocol for all capture workers'
/// lifetimes. Retain guest endpoints through admitted writer quiescence, and use
/// distinct fork incarnations. Do not fork and then use inherited host worker,
/// lock or producer state in the child. The owner must coordinate emitters,
/// actual process reap and finalization; setup closure alone is not completion.
///
/// ```compile_fail,E0133
/// use reverie_rpc_transport::guest_log as g;
/// fn requires_ownership_contract<D: g::CaptureDestination>(options: g::CaptureOptions, destination: D) {
///     let _ = g::prepared_capture(options, destination);
/// }
/// ```
pub unsafe fn prepared_capture<D: CaptureDestination>(
    options: CaptureOptions,
    destination: D,
) -> Result<(CaptureOwner, LogSink, HostProducer), CaptureStartError> {
    let destination: Box<dyn CaptureDestination> = Box::new(destination);
    let (mut owner, sink, host) = match unsafe { prepare_capture_unstarted(options) } {
        Ok(capture) => capture,
        Err(error) => return Err(error.recover(destination)),
    };
    owner.start_with(destination, StartHooks::default())?;
    Ok((owner, sink, host))
}

/// Prepare the ordered capture channel without creating any OS worker tasks.
/// No destination is accepted and no arbitrary destination callback can run.
/// Source commits before start require enough ring and whole-record credit;
/// failure is immediate, sticky and never constitutes a commit.
///
/// # Safety
/// The trusted-peer, mapping, descriptor, unique-writer, fork-incarnation and
/// process-lifetime contracts of [`prepared_capture`] apply unchanged. Do not
/// fork and use this process's HostProducer or CaptureOwner in the child. The
/// guest endpoint alone may be transferred under the initialized wire contract.
///
/// ```compile_fail,E0133
/// use reverie_rpc_transport::guest_log as g;
/// fn requires_contract(options: g::CaptureOptions) {
///     let _ = g::prepare_capture_unstarted(options);
/// }
/// ```
pub unsafe fn prepare_capture_unstarted(
    options: CaptureOptions,
) -> Result<(CaptureOwner, LogSink, HostProducer), CaptureStartError> {
    if options.limits.diagnostic_bytes == 0
        || options.limits.diagnostic_bytes > 32 * 1024 * 1024
        || [
            options.timeouts.startup,
            options.timeouts.blocked_publication,
            options.timeouts.final_drain,
        ]
        .iter()
        .any(|duration| duration.is_zero() || Instant::now().checked_add(*duration).is_none())
    {
        return Err(io::Error::other("invalid capture bounds/deadlines").into());
    }
    let (host, guest) = unsafe { ordered::channel_pair(options.limits.ordered()) }?;
    let buffer = unsafe { ordered::Buffer::receive(host.as_raw_fd()) }?;
    let writer = unsafe { buffer.activate(0, i64::from(std::process::id())) }
        .map_err(|_| io::Error::other("host registration failed"))?;
    let collector = buffer
        .collector()
        .map_err(|_| io::Error::other("collector registration failed"))?;
    let (mut sink, handle) = super::retained_log_with_drain(
        super::Options {
            byte_limit: options.limits.host_pending_bytes + options.limits.guest_pending_bytes,
            producers: options.limits.producers,
            slots: options.limits.slots_per_producer,
        },
        options.timeouts.final_drain,
    );
    let publication = publication::Publication::prepare(
        options.limits.pending_records,
        options.limits.diagnostic_bytes,
        options.timeouts.blocked_publication,
    )?;
    let shared = Arc::new(Shared {
        retention: Arc::downgrade(&handle.0),
        options,
        buffer,
        publication,
        state: Mutex::new(State {
            ready: false,
            collector_worker: WorkerState::NeverCreated,
            guest_phase: Phase::NotStarted,
            peer_closed: false,
            guest_stop: None,
            deadline: None,
            commits_observed: false,
            collector_finished: false,
            error: None,
            streams: Vec::new(),
            omitted_diagnostic_bytes: 0,
        }),
        lifecycle: AtomicU8::new(CaptureLifecycle::Prepared as u8),
        host: Mutex::new(writer),
        active_host_calls: AtomicUsize::new(0),
        late_host_writes: AtomicU64::new(0),
        omitted_issues: AtomicU64::new(0),
        collector: Mutex::new(None),
        collector_start: Mutex::new(CollectorStart::Available {
            socket: host,
            collector,
        }),
    });
    assert!(handle.0.capture.set(shared.clone()).is_ok());
    sink.prepared = Some(guest);
    let owner = CaptureOwner {
        handle: handle.clone(),
        shared: shared.clone(),
        finalized: false,
    };
    let producer = HostProducer { handle, shared };
    Ok((owner, sink, producer))
}

type SpawnWorker = Box<dyn FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<JoinHandle<()>>>;
struct StartHooks {
    output_spawn: SpawnWorker,
    collector_spawn: SpawnWorker,
    #[cfg(test)]
    after_offer: Box<dyn FnOnce(&Shared)>,
    #[cfg(test)]
    before_take: Box<dyn FnOnce() + Send>,
    #[cfg(test)]
    after_taken: Box<dyn FnOnce(&Shared)>,
}
impl Default for StartHooks {
    fn default() -> Self {
        Self {
            output_spawn: Box::new(|worker| {
                std::thread::Builder::new()
                    .name("capture-output".into())
                    .spawn(worker)
            }),
            collector_spawn: Box::new(|worker| {
                std::thread::Builder::new()
                    .name("capture-collector".into())
                    .spawn(worker)
            }),
            #[cfg(test)]
            after_offer: Box::new(|_| {}),
            #[cfg(test)]
            before_take: Box::new(|| {}),
            #[cfg(test)]
            after_taken: Box::new(|_| {}),
        }
    }
}

#[cfg(test)]
unsafe fn prepared_capture_with<D: CaptureDestination>(
    options: CaptureOptions,
    destination: D,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<JoinHandle<()>> + 'static,
) -> Result<(CaptureOwner, LogSink, HostProducer), CaptureStartError> {
    let destination: Box<dyn CaptureDestination> = Box::new(destination);
    let (mut owner, sink, host) = match unsafe { prepare_capture_unstarted(options) } {
        Ok(capture) => capture,
        Err(error) => return Err(error.recover(destination)),
    };
    owner.start_with(
        destination,
        StartHooks {
            collector_spawn: Box::new(spawn),
            ..StartHooks::default()
        },
    )?;
    Ok((owner, sink, host))
}

fn collect(shared: &Shared, socket: UnixStream, mut collector: ordered::Collector) {
    {
        let mut state = shared.state.lock().unwrap();
        state.ready = true;
        state.collector_worker = WorkerState::Ready;
    }
    shared.notify();
    let mut pending = None;
    loop {
        let now = Instant::now();
        if shared.buffer.guest_failed() {
            if let Some(retention) = shared.retention.upgrade() {
                LogHandle(retention)
                    .stop(IssueKind::Producer, "guest complete-record emission failed");
            } else {
                shared.stop_guest();
            }
        }
        let publication_failed = shared
            .publication
            .check_blocked(now, shared.options.timeouts.blocked_publication);
        if publication_failed {
            shared.fail("canonical destination publication failed");
        }
        if pending.is_none() {
            match collector.poll() {
                Ok(record) => pending = record,
                Err(_) => shared.fail("source ordered collection failed"),
            }
        }
        if let Some(record) = pending.take() {
            if publication_failed {
                shared.publication.discard(record);
            } else {
                pending = shared.publication.enqueue(record).err();
            }
        }
        let closed = match super::peer_closed(&socket) {
            Ok(closed) => closed,
            Err(_) => {
                shared.fail("guest lifetime endpoint protocol failure");
                false
            }
        };
        let (guest_cutoff, deadline) = {
            let state = shared.state.lock().unwrap();
            (
                state.guest_stop.is_some_and(|stop| {
                    now.saturating_duration_since(stop) >= shared.options.timeouts.final_drain
                }),
                state.deadline,
            )
        };
        let complete = closed && collector.guest_complete();
        let drained = closed && collector.guest_drained();
        if guest_cutoff && shared.buffer.unresolved_guest_commit() {
            shared.fail(
                "guest commit entrant unresolved at cutoff; ordered continuation unavailable",
            );
        }
        if closed {
            shared.buffer.close(ordered::Role::Guest);
        }
        {
            let mut state = shared.state.lock().unwrap();
            state.peer_closed = closed;
            if complete && state.guest_stop.is_none() {
                state.guest_phase = Phase::Complete;
            } else if guest_cutoff || drained || (closed && shared.buffer.order_failed()) {
                state.guest_phase = Phase::Incomplete;
                (state.streams, state.omitted_diagnostic_bytes) =
                    collector.diagnostics(shared.options.limits.diagnostic_bytes);
            }
        }
        shared.notify();
        let finalizing = shared.buffer.admission(ordered::Role::Host).closed;
        let observed = match collector.commits_observed() {
            Ok(observed) => observed,
            Err(_) => {
                shared.fail("source commit order has an unresolved hole");
                false
            }
        };
        let expired = deadline.is_some_and(|deadline| now >= deadline);
        if finalizing && ((observed && pending.is_none() && (drained || guest_cutoff)) || expired) {
            if expired && !(observed && pending.is_none() && complete) {
                shared.fail("capture final drain deadline exceeded");
            }
            if collector.host_partial() || shared.active_host_calls.load(Ordering::Acquire) != 0 {
                shared.fail("host emission incomplete at close");
            }
            let mut state = shared.state.lock().unwrap();
            state.commits_observed = observed && pending.is_none();
            (state.streams, state.omitted_diagnostic_bytes) =
                collector.diagnostics(shared.options.limits.diagnostic_bytes);
            if !complete {
                state.guest_phase = Phase::Incomplete;
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod deferred_tests;
