//! Additive O (observer) / C (coordinator) capture ownership.
use std::cell::Cell;
use std::io::Write;
use std::io::{self};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie::process::ChildCleanupObservation;
use reverie::process::ChildStartContext;
use reverie::process::Container;
use reverie::process::ExitStatus;
use reverie::process::OwnedDeferredContainerRun;
use reverie::process::OwnedFinalization;
use reverie::process::OwnedFinalize;
use reverie::process::OwnedReapedResult;
use reverie::process::ParentStartContext;
use reverie::process::StartupError;
use reverie::process::StartupOwnedFailure;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de::DeserializeOwned;

use super::*;
pub(super) mod lifecycle;
use lifecycle::Lifecycle;
pub use lifecycle::LifecycleSnapshot;

/// Inactive mappings/endpoints only. Construct before the first clone.
pub struct SplitCapturePlan {
    inert: InertCapturePlan,
    lifecycle: Lifecycle,
}
impl SplitCapturePlan {
    /// # Safety
    /// Same mapping/alias/threadless contract as [`InertCapturePlan::new`].
    pub unsafe fn new(options: CaptureOptions) -> io::Result<Self> {
        Ok(Self {
            inert: unsafe { InertCapturePlan::new(options) }?,
            lifecycle: Lifecycle::new()?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CoordinatorDisposition {
    Completed,
    CaughtPanic,
    PolicyRefusal,
    RunTimeout,
    CoordinatorError,
}
impl CoordinatorDisposition {
    fn number(self) -> u64 {
        match self {
            Self::Completed => 1,
            Self::CaughtPanic => 2,
            Self::PolicyRefusal => 3,
            Self::RunTimeout => 4,
            Self::CoordinatorError => 5,
        }
    }
}

/// Bounded classification; the adapter must retain its original detailed errors.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SplitRpcFailure {
    Transport,
    Panicked,
    UnresolvedPanic,
    Interrupted,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SplitRpcIssue {
    pub connection: u64,
    pub failure: SplitRpcFailure,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoordinatorFacts {
    pub disposition: CoordinatorDisposition,
    pub guest_wait_status: i32,
    pub rpc_issues: Vec<SplitRpcIssue>,
}
impl CoordinatorFacts {
    fn qualifies(&self) -> bool {
        self.disposition == CoordinatorDisposition::Completed
            && self.guest_wait_status == 0
            && self.rpc_issues.is_empty()
    }
}

struct EmitterLocal {
    buffer: Arc<ordered::Buffer>,
    writer: Mutex<ordered::Writer>,
    lifecycle: Arc<Lifecycle>,
    options: CaptureOptions,
}
/// Process-local C emitter. Clones have no completion authority.
#[derive(Clone)]
pub struct CoordinatorEmitter(Arc<EmitterLocal>);
struct Entry<'a>(&'a Lifecycle);
impl Drop for Entry<'_> {
    fn drop(&mut self) {
        self.0.leave();
    }
}
impl CoordinatorEmitter {
    pub fn write_record(&self, bytes: &[u8]) -> Result<RecordCommit, PublishError> {
        if !self.0.lifecycle.enter() {
            return Err(PublishError::Stopped);
        }
        let _entry = Entry(&self.0.lifecycle);
        let deadline = Instant::now() + self.0.options.timeouts.blocked_publication;
        let result = (|| {
            let mut writer = self.writer_until(deadline)?;
            writer.write_record(bytes, |_, _| wait_until(deadline))
        })();
        if result.is_err() {
            self.0.lifecycle.fault();
        }
        result
    }
    pub fn record_failed(&self) {
        self.0.lifecycle.fault();
    }
    fn writer_until(
        &self,
        deadline: Instant,
    ) -> Result<std::sync::MutexGuard<'_, ordered::Writer>, PublishError> {
        loop {
            if Instant::now() >= deadline {
                return Err(PublishError::Full);
            }
            match self.0.writer.try_lock() {
                Ok(writer) => return Ok(writer),
                Err(std::sync::TryLockError::Poisoned(_)) => return Err(PublishError::Invalid),
                Err(std::sync::TryLockError::WouldBlock) => wait_until(deadline)?,
            }
        }
    }
}
fn wait_until(deadline: Instant) -> Result<(), PublishError> {
    if Instant::now() >= deadline {
        return Err(PublishError::Full);
    }
    std::thread::sleep(Duration::from_millis(1));
    Ok(())
}

struct Finalizer {
    emitter: CoordinatorEmitter,
    endpoint: UnixStream,
}
impl Drop for Finalizer {
    fn drop(&mut self) {
        let local = &self.emitter.0;
        if std::thread::panicking() {
            local.lifecycle.fault();
        }
        // This deadline starts only after all declared user teardown has returned.
        let deadline = Instant::now() + local.options.timeouts.final_drain;
        local.lifecycle.close();
        let completed = (|| {
            while local.lifecycle.snapshot().entrants != 0 {
                wait_until(deadline)?;
            }
            let finish_deadline =
                deadline.min(Instant::now() + local.options.timeouts.blocked_publication);
            let mut writer = self.emitter.writer_until(finish_deadline)?;
            if local.lifecycle.snapshot().faulted {
                return Err(PublishError::Invalid);
            }
            // Host admission stays OPEN for the real FINISH publication.
            writer.finish(|_, _| wait_until(finish_deadline))
        })();
        if completed.is_err() {
            local.lifecycle.fault();
        }
        local.buffer.close(ordered::Role::Host);
        if completed.is_ok() {
            local.lifecycle.finish();
        }
        // The endpoint remains owned until after FINISH (including T/U Drop logs).
        let _ = &self.endpoint;
    }
}

/// C-only, one-use context. It does not start a helper thread/runtime.
pub struct CoordinatorContext {
    finalizer: Option<Finalizer>,
    guest_taken: bool,
}
impl CoordinatorContext {
    pub fn emitter(&self) -> CoordinatorEmitter {
        self.finalizer.as_ref().unwrap().emitter.clone()
    }
    /// The one pending V4 import for G. C keeps a declared lifetime anchor.
    pub fn guest_endpoint(&mut self) -> io::Result<UnixStream> {
        if self.guest_taken {
            return Err(io::Error::other("guest endpoint already handed off"));
        }
        let endpoint = self.finalizer.as_ref().unwrap().endpoint.try_clone()?;
        self.guest_taken = true;
        Ok(endpoint)
    }
    /// Finish the adapter's work, retaining T until its serializer and destructor
    /// have run. G must really have been reaped. Serving task termination AND
    /// actual connection/runtime teardown must precede the issue snapshot.
    /// Planned cancellation is not a clean serving result; classify C explicitly.
    ///
    /// This is part of the unsafe run contract, not a verifier of arbitrary
    /// callback claims. No generic T / exit0 / idle counter establishes these facts.
    pub fn after_teardown<T>(
        mut self,
        value: T,
        disposition: CoordinatorDisposition,
        guest_status: ExitStatus,
        issues: &[crate::ConnectionIssue],
    ) -> SplitChildResult<T> {
        let finalizer = self.finalizer.take().unwrap();
        if !self.guest_taken || issues.len() > 64 {
            finalizer.emitter.0.lifecycle.fault();
        }
        let facts = CoordinatorFacts {
            disposition,
            guest_wait_status: guest_status.into_raw(),
            rpc_issues: issues
                .iter()
                .take(64)
                .map(|issue| SplitRpcIssue {
                    connection: issue.connection as u64,
                    failure: match &issue.failure {
                        crate::ConnectionFailure::Transport(_) => SplitRpcFailure::Transport,
                        crate::ConnectionFailure::Panicked(_) => SplitRpcFailure::Panicked,
                        crate::ConnectionFailure::UnresolvedPanic => {
                            SplitRpcFailure::UnresolvedPanic
                        }
                        crate::ConnectionFailure::Interrupted => SplitRpcFailure::Interrupted,
                    },
                })
                .collect(),
        };
        finalizer.emitter.0.lifecycle.facts(
            disposition.number(),
            guest_status.success(),
            issues.len() as u64,
        );
        SplitChildResult {
            value: Some(value),
            facts,
            finalizer: Some(finalizer),
        }
    }
}
impl Drop for CoordinatorContext {
    fn drop(&mut self) {
        if let Some(finalizer) = &self.finalizer {
            finalizer.emitter.0.lifecycle.fault();
        }
    }
}

/// Child-side result envelope. Parent deserialization never creates a finalizer.
pub struct SplitChildResult<T> {
    value: Option<T>,
    facts: CoordinatorFacts,
    finalizer: Option<Finalizer>,
}
impl<T: Serialize> Serialize for SplitChildResult<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (
            &self.facts,
            self.value.as_ref().expect("result value before Drop"),
        )
            .serialize(serializer)
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for SplitChildResult<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (facts, value) = <(CoordinatorFacts, T)>::deserialize(deserializer)?;
        if facts.rpc_issues.len() > 64 {
            return Err(serde::de::Error::custom("RPC facts exceed bound"));
        }
        Ok(Self {
            value: Some(value),
            facts,
            finalizer: None,
        })
    }
}
impl<T> Drop for SplitChildResult<T> {
    fn drop(&mut self) {
        // Serialization borrows T. A2 drops U before this envelope. Neither is
        // evidence T was destroyed: explicitly do so while emission is open.
        drop(self.value.take());
        drop(self.finalizer.take());
    }
}

// O retains its escrow even if thread creation fails or a worker panics. D is
// dropped only after the real child and both capture-owned workers have settled.
struct Destination<D>(Arc<Mutex<D>>);
impl<D: CaptureDestination> Write for Destination<D> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).flush()
    }
}
impl<D: CaptureDestination> CaptureDestination for Destination<D> {
    fn progress(&self) -> DestinationProgress {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).progress()
    }
}
struct Workers {
    owner: Option<CaptureOwner>,
    escrow: Option<Box<dyn std::any::Any>>,
}
// Unit-only fault injection at each startup ownership boundary. Production
// always uses the real spawn/readiness paths below; no public bypass exists.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq)]
enum StartFault {
    PublicationSpawn,
    CollectorSpawn,
    PublicationReady,
    CollectorReady,
}
#[cfg(test)]
thread_local! { static START_FAULT: Cell<Option<StartFault>> = const { Cell::new(None) }; }
#[cfg(test)]
fn startup_fault(stage: StartFault) -> bool {
    START_FAULT.with(|fault| {
        if fault.get() == Some(stage) {
            fault.set(None);
            true
        } else {
            false
        }
    })
}
impl Workers {
    fn start<D: CaptureDestination>(
        &mut self,
        plan: SplitCapturePlan,
        destination: D,
        deadline: Instant,
    ) -> Result<(), StartupError> {
        let escrow = Arc::new(Mutex::new(destination));
        self.escrow = Some(Box::new(escrow.clone()));
        let (options, buffer, host, guest) = plan.inert.into_local_parts();
        drop(guest); // O never retains a peer endpoint that could hide C/G EOF.
        let buffer = Arc::new(buffer);
        let collector = buffer.collector().map_err(|_| StartupError::Protocol)?;
        let (mut sink, handle) = super::super::retained_log_with_drain(
            super::super::Options {
                byte_limit: options.limits.host_pending_bytes + options.limits.guest_pending_bytes,
                producers: options.limits.producers,
                slots: options.limits.slots_per_producer,
            },
            options.timeouts.final_drain,
        );
        sink.transferred = true;
        drop(sink);
        #[cfg(test)]
        if startup_fault(StartFault::PublicationSpawn) {
            return Err(StartupError::Protocol);
        }
        let publication = publication::Publication::start(
            Destination(escrow),
            options.limits.pending_records,
            options.limits.diagnostic_bytes,
            options.timeouts.blocked_publication,
        )
        .map_err(|_| StartupError::Protocol)?;
        let shared = Arc::new(Shared {
            retention: Arc::downgrade(&handle.0),
            options,
            buffer,
            publication,
            state: Mutex::new(State {
                ready: false,
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
            host: None,
            split: Some(Arc::new(plan.lifecycle)),
            collector_join: Mutex::new(None),
            host_complete: std::sync::atomic::AtomicBool::new(false),
            active_host_calls: AtomicUsize::new(0),
            late_host_writes: AtomicU64::new(0),
            omitted_issues: AtomicU64::new(0),
            collector: Mutex::new(None),
        });
        assert!(handle.0.capture.set(shared.clone()).is_ok());
        self.owner = Some(CaptureOwner {
            handle,
            shared: shared.clone(),
            finalized: false,
        });
        let worker = shared.clone();
        #[cfg(test)]
        if startup_fault(StartFault::CollectorSpawn) {
            return Err(StartupError::Protocol);
        }
        let thread =
            std::thread::Builder::new()
                .name("split-capture-collector".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        collect(&worker, host, collector)
                    }));
                    if result.is_err() {
                        worker.fail("split collector panicked");
                    }
                    {
                        let mut state = worker.state.lock().unwrap();
                        state.collector_finished = true;
                        if !matches!(state.guest_phase, Phase::Complete | Phase::Incomplete) {
                            state.guest_phase = Phase::Incomplete;
                        }
                    }
                    let deadline =
                        worker.state.lock().unwrap().deadline.unwrap_or_else(|| {
                            Instant::now() + worker.options.timeouts.final_drain
                        });
                    worker.publication.finish_until(deadline);
                    worker.notify();
                })
                .map_err(|_| StartupError::Protocol)?;
        *shared.collector.lock().unwrap() = Some(thread);
        #[cfg(test)]
        if startup_fault(StartFault::PublicationReady) {
            return Err(StartupError::Protocol);
        }
        if !shared.publication.wait_ready(deadline) {
            return Err(StartupError::Protocol);
        }
        #[cfg(test)]
        if startup_fault(StartFault::CollectorReady) {
            return Err(StartupError::TimedOut);
        }
        while !shared.state.lock().unwrap().ready {
            if Instant::now() >= deadline || shared.join_finished() {
                return Err(StartupError::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
    fn finish(&mut self, deadline: Instant) -> Option<CaptureReport> {
        self.owner
            .as_mut()
            .map(|owner| owner.finish_until(deadline))
    }
    fn joins(&self) -> (Option<bool>, Option<bool>) {
        match &self.owner {
            None => (Some(true), Some(true)),
            Some(owner) => {
                owner.shared.join_finished();
                let collector = *owner.shared.collector_join.lock().unwrap();
                // Failed spawn owns no collector handle, but is still a failed run.
                let collector =
                    if collector.is_none() && owner.shared.collector.lock().unwrap().is_none() {
                        Some(true)
                    } else {
                        collector
                    };
                (collector, owner.shared.publication.joined())
            }
        }
    }
    fn wait_joins(&self, deadline: Instant) -> (Option<bool>, Option<bool>) {
        loop {
            let joins = self.joins();
            if (joins.0.is_some() && joins.1.is_some()) || Instant::now() >= deadline {
                return joins;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn join_blocking(&mut self) {
        if let Some(owner) = &mut self.owner {
            owner
                .shared
                .close(Instant::now() + owner.shared.options.timeouts.final_drain);
            if let Some(thread) = owner.shared.collector.lock().unwrap().take() {
                *owner.shared.collector_join.lock().unwrap() = Some(thread.join().is_ok());
            }
            owner.shared.publication.join_blocking();
            owner.finalized = true;
        }
    }
}

/// A frozen classification; later cleanup cannot promote an earlier failure.
#[derive(Clone, Debug)]
pub struct SplitReport {
    pub capture: Option<CaptureReport>,
    pub lifecycle: Option<LifecycleSnapshot>,
    pub host_complete: bool,
    pub collector_join: Option<bool>,
    pub publication_join: Option<bool>,
    pub coordinator_status: Option<ExitStatus>,
    pub failure: Option<String>,
}
impl SplitReport {
    pub fn qualifies(&self) -> bool {
        self.failure.is_none()
            && self.coordinator_status.is_some_and(|s| s.success())
            && self.collector_join == Some(true)
            && self.publication_join == Some(true)
            && self.host_complete
            && self.lifecycle.is_some_and(LifecycleSnapshot::qualifies)
            && self.capture.as_ref().is_some_and(CaptureReport::qualifies)
    }
}
enum Child<T> {
    Running(OwnedDeferredContainerRun<SplitChildResult<T>>),
    Pending(OwnedFinalization<SplitChildResult<T>>),
}

/// Owns factories, child capability, encoded bytes and both worker joins. Drop
/// can block; a finite retry never releases those resources on an unknown wait.
#[must_use]
pub struct SplitCaptureRun<T, P, B> {
    parent_factory: Option<P>,
    child_factory: Option<B>,
    plan: Cell<Option<SplitCapturePlan>>,
    workers: Workers,
    child: Option<Child<T>>,
    result: Option<OwnedReapedResult<SplitChildResult<T>>>,
    status: Option<ExitStatus>,
    failure: Option<String>,
    frozen: Option<SplitReport>,
    failed_bytes: Vec<u8>,
}
pub enum SplitCaptureOutcome<T, P, B> {
    Joined(Box<JoinedCapture<T>>),
    Unjoined(Box<SplitCaptureRun<T, P, B>>),
}
/// Only capture-owned quiescence is certified. User Drop/Deserialize may create
/// unrelated workers; re-establish the threadless contract before another clone.
pub struct JoinedCapture<T> {
    /// Actual joins at return, independent of any earlier frozen failure snapshot.
    pub actual_joins: (bool, bool),
    pub actual_coordinator_status: Option<ExitStatus>,
    pub report: SplitReport,
    pub facts: Option<CoordinatorFacts>,
    pub value: Option<T>,
    pub encoded_bytes: Vec<u8>,
}

impl<T, P, B> SplitCaptureRun<T, P, B> {
    fn fail(&mut self, reason: impl Into<String>) {
        self.failure.get_or_insert_with(|| reason.into());
    }
    fn observe(&mut self, deadline: Instant, cancel: bool) {
        let Some(child) = self.child.take() else {
            return;
        };
        let result = match child {
            Child::Running(run) => {
                if cancel {
                    run.cancel_until(deadline)
                } else {
                    run.finalize_until(deadline)
                }
            }
            Child::Pending(run) => {
                if cancel {
                    run.cancel_until(deadline)
                } else {
                    run.retry_until(deadline)
                }
            }
        };
        match result {
            OwnedFinalize::Complete(result) => {
                self.status = Some(result.status());
                self.result = Some(result);
            }
            OwnedFinalize::Pending(run) => self.child = Some(Child::Pending(run)),
            OwnedFinalize::Failed { cause, cleanup } => {
                self.fail(format!("owned coordinator failure: {cause:?}"));
                self.failed_bytes = cleanup.provisional_bytes().to_vec();
                match cleanup.cleanup().observation() {
                    ChildCleanupObservation::Reaped(status) => {
                        self.status = Some(status);
                        drop(cleanup);
                    }
                    ChildCleanupObservation::ExitedWithoutWaitStatus => drop(cleanup),
                    _ => self.child = Some(Child::Pending(cleanup)),
                }
            }
        }
    }
    fn report(&self, capture: Option<CaptureReport>) -> SplitReport {
        let (collector_join, publication_join) = self.workers.joins();
        SplitReport {
            capture,
            lifecycle: self
                .workers
                .owner
                .as_ref()
                .and_then(|o| o.shared.split.as_ref().map(|l| l.snapshot())),
            host_complete: self
                .workers
                .owner
                .as_ref()
                .is_some_and(|o| o.shared.host_complete.load(Ordering::Acquire)),
            collector_join,
            publication_join,
            coordinator_status: self.status,
            failure: self.failure.clone(),
        }
    }
    pub fn cancel_until(mut self, deadline: Instant) -> SplitCaptureOutcome<T, P, B>
    where
        T: DeserializeOwned,
    {
        self.fail("caller cancelled split capture");
        self.settle_until(deadline)
    }
    pub fn settle_until(mut self, deadline: Instant) -> SplitCaptureOutcome<T, P, B>
    where
        T: DeserializeOwned,
    {
        self.observe(deadline, self.failure.is_some());
        if self.child.is_some() {
            self.fail("coordinator unsettled at observation deadline");
            if self.frozen.is_none() {
                self.frozen = Some(self.report(None));
            }
            return SplitCaptureOutcome::Unjoined(Box::new(self));
        }
        if let Some(owner) = &self.workers.owner {
            if let Some(lifecycle) = &owner.shared.split {
                let facts = lifecycle.snapshot();
                if facts.disposition != 0 {
                    owner.handle.root_reaped();
                    owner
                        .handle
                        .run_state(if facts.disposition == 1 && facts.guest_success {
                            RunState::Succeeded
                        } else {
                            RunState::Failed
                        });
                }
                if !facts.qualifies() {
                    owner
                        .shared
                        .fail("split coordinator facts/teardown do not qualify");
                }
            }
        }
        let capture = self.workers.finish(deadline);
        let report = self.report(capture);
        if self.frozen.is_none() {
            self.frozen = Some(report);
        }
        let (collector, publication) = self.workers.wait_joins(deadline);
        if collector.is_none() || publication.is_none() {
            self.fail("capture workers remain unjoined");
            return SplitCaptureOutcome::Unjoined(Box::new(self));
        }
        // Resources and generic factories are reclaimed before arbitrary decode.
        drop(self.workers.escrow.take());
        drop(self.parent_factory.take());
        drop(self.child_factory.take());
        drop(self.plan.take());
        let mut report = self.frozen.take().unwrap();
        let mut encoded_bytes = std::mem::take(&mut self.failed_bytes);
        let mut facts = None;
        let mut value = None;
        if let Some(result) = self.result.take() {
            encoded_bytes.extend_from_slice(result.encoded_bytes());
            match result.decode() {
                Ok(mut envelope) => {
                    if !envelope.facts.qualifies() {
                        report
                            .failure
                            .get_or_insert_with(|| "coordinator result does not qualify".into());
                    }
                    facts = Some(envelope.facts.clone());
                    value = envelope.value.take();
                }
                Err(error) => {
                    report
                        .failure
                        .get_or_insert_with(|| format!("coordinator decode refused: {error:?}"));
                }
            }
        }
        SplitCaptureOutcome::Joined(Box::new(JoinedCapture {
            actual_joins: (collector.unwrap(), publication.unwrap()),
            actual_coordinator_status: self.status,
            report,
            facts,
            value,
            encoded_bytes,
        }))
    }
}
impl<T, P, B> Drop for SplitCaptureRun<T, P, B> {
    fn drop(&mut self) {
        if let Some(owner) = &self.workers.owner {
            if self.child.is_some() {
                owner.shared.fail("split owner disposed before settlement");
            }
        }
        // Owned A2 Drop settles the real child (possibly blocking), never a PID
        // guess. Drainers remain alive until this completes.
        drop(self.child.take());
        self.workers.join_blocking();
        drop(self.workers.escrow.take());
        drop(self.parent_factory.take());
        drop(self.child_factory.take());
        drop(self.plan.take());
    }
}

/// Start split capture using the landed owned-startup API. No generic value is
/// decoded until real C termination, O joins and factory reclamation.
///
/// # Safety
/// Caller must satisfy A2's threadless/no-competing-reaper/clone contract, all
/// inert mapping aliases, and the callback teardown contract documented above.
/// B is borrowed in C; it may only construct fresh process-local W. W must wait
/// G and settle its full RPC/runtime lifecycle before after_teardown. No active
/// inherited Arc/lock/runtime can be reused. C result acquisition and arbitrary
/// user callbacks/Drop are NOT bounded by startup or settlement deadlines.
pub unsafe fn run_split_capture<P, B, W, D, T, U>(
    container: &mut Container,
    plan: SplitCapturePlan,
    parent_destination: P,
    child_factory: B,
) -> SplitCaptureRun<T, P, B>
where
    P: FnMut(ParentStartContext<'_>) -> Result<D, StartupError>,
    B: FnMut(&mut ChildStartContext) -> Result<W, StartupError>,
    W: FnOnce(CoordinatorContext) -> (SplitChildResult<T>, U),
    D: CaptureDestination,
    T: Serialize,
{
    let timeout = plan.inert.options().timeouts.startup;
    let mut owner = SplitCaptureRun {
        parent_factory: Some(parent_destination),
        child_factory: Some(child_factory),
        plan: Cell::new(Some(plan)),
        workers: Workers {
            owner: None,
            escrow: None,
        },
        child: None,
        result: None,
        status: None,
        failure: None,
        frozen: None,
        failed_bytes: Vec::new(),
    };
    let plan = &owner.plan;
    let parent_factory = &mut owner.parent_factory;
    let child_factory = &mut owner.child_factory;
    let workers = &mut owner.workers;
    let mut parent = |context: ParentStartContext<'_>| {
        let deadline = context.deadline();
        let destination = parent_factory.as_mut().unwrap()(context)?;
        workers.start(
            plan.take().expect("O plan consumed once"),
            destination,
            deadline,
        )
    };
    let mut child = |context: &mut ChildStartContext| {
        let work = child_factory.as_mut().unwrap()(context)?;
        Ok((plan.take().expect("C plan consumed once"), work))
    };
    let mut run = |(plan, work): (SplitCapturePlan, W)| {
        let (options, buffer, host, guest) = plan.inert.into_local_parts();
        drop(host);
        let buffer = Arc::new(buffer);
        let lifecycle = Arc::new(plan.lifecycle);
        assert!(lifecycle.start(), "one coordinator role");
        let writer = unsafe { buffer.activate(0, i64::from(std::process::id())) }
            .expect("coordinator activation");
        let emitter = CoordinatorEmitter(Arc::new(EmitterLocal {
            buffer,
            writer: Mutex::new(writer),
            lifecycle,
            options,
        }));
        work(CoordinatorContext {
            finalizer: Some(Finalizer {
                emitter,
                endpoint: guest,
            }),
            guest_taken: false,
        })
    };
    match container.run_with_startup_owned(timeout, &mut parent, &mut child, &mut run) {
        Ok(run) => owner.child = Some(Child::Running(run)),
        Err(StartupOwnedFailure::BeforeClone { cause }) => {
            owner.fail(format!("startup before clone: {cause:?}"))
        }
        Err(StartupOwnedFailure::AfterClone { cause, run }) => {
            owner.fail(format!("startup after clone: {cause:?}"));
            owner.child = Some(Child::Pending(run));
        }
    }
    owner
}

#[cfg(test)]
mod tests;
