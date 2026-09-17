use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use super::ArtifactStability;
use super::CaptureDestination;
use super::DestinationProgress;
use super::WorkerState;
use super::ordered::Record;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Attempt {
    pub order: u64,
    pub acknowledged: usize,
    pub attempted_end: usize,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub ready: bool,
    pub worker: WorkerState,
    pub drained: bool,
    pub finalized: bool,
    pub stability: ArtifactStability,
    pub progress: DestinationProgress,
    pub attempt: Option<Attempt>,
    pub error: Option<String>,
    pub diagnostic_prefix: Vec<u8>,
    pub omitted_bytes: u64,
    pub unpublished_bytes: u64,
    pub first_unpublished_order: Option<u64>,
}

struct State {
    report: Report,
    frozen: Option<Report>,
    revoked: bool,
    deadline: Option<Instant>,
    operation_started: Option<Instant>,
    unattempted: BTreeMap<u64, u64>,
    diagnostic_bytes: usize,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

pub struct Publication {
    shared: Shared,
    sender: Mutex<Option<mpsc::SyncSender<Record>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    receiver: Mutex<Option<mpsc::Receiver<Record>>>,
    handoff: Arc<(Mutex<Handoff>, Condvar)>,
    blocked_budget: Duration,
}

// Only the start caller and the one publication worker can access this cell.
// Cancellation explicitly extracts Offered; an Arc/closure drop never owns D.
enum Handoff {
    Waiting,
    Offered(Box<dyn CaptureDestination>),
    Taken,
    Cancelled,
}

fn failure(state: &mut State, message: &str) {
    if state.report.error.is_none() {
        state.report.error = Some(message.to_owned());
    }
    state.revoked = true;
    for (order, bytes) in std::mem::take(&mut state.unattempted) {
        account_unpublished(state, order, bytes);
    }
}

fn account_unpublished(state: &mut State, order: u64, bytes: u64) {
    state.report.first_unpublished_order = Some(
        state
            .report
            .first_unpublished_order
            .map_or(order, |first| first.min(order)),
    );
    match state.report.unpublished_bytes.checked_add(bytes) {
        Some(total) => state.report.unpublished_bytes = total,
        None => {
            state
                .report
                .error
                .get_or_insert_with(|| "unpublished byte count overflow".into());
            state.revoked = true;
        }
    }
}

fn retain_diagnostic(state: &mut State, prefix: &[u8], length: usize) {
    state.report.diagnostic_prefix.extend_from_slice(prefix);
    match state
        .report
        .omitted_bytes
        .checked_add((length - prefix.len()) as u64)
    {
        Some(total) => state.report.omitted_bytes = total,
        None => failure(state, "diagnostic omission count overflow"),
    }
}

fn unattempted(state: &mut State, order: u64, bytes: u64) {
    if state.revoked {
        account_unpublished(state, order, bytes);
    } else if state.unattempted.insert(order, bytes).is_some() {
        failure(state, "duplicate publication order");
    }
}

impl Publication {
    #[cfg(test)]
    pub fn worker_finished(&self) -> bool {
        self.join_finished()
    }
    pub fn prepare(
        records: usize,
        diagnostic_bytes: usize,
        blocked_budget: Duration,
    ) -> io::Result<Self> {
        if records == 0 || records > 4096 || diagnostic_bytes == 0 {
            return Err(io::Error::other("invalid capture publication bounds"));
        }
        let shared = Arc::new((
            Mutex::new(State {
                report: Report {
                    ready: false,
                    worker: WorkerState::NeverCreated,
                    drained: false,
                    finalized: false,
                    stability: ArtifactStability::MayAppend,
                    progress: DestinationProgress::default(),
                    attempt: None,
                    error: None,
                    diagnostic_prefix: Vec::new(),
                    omitted_bytes: 0,
                    unpublished_bytes: 0,
                    first_unpublished_order: None,
                },
                frozen: None,
                revoked: false,
                deadline: None,
                operation_started: None,
                unattempted: BTreeMap::new(),
                diagnostic_bytes,
            }),
            Condvar::new(),
        ));
        let (sender, receiver) = mpsc::sync_channel(records);
        Ok(Self {
            shared,
            sender: Mutex::new(Some(sender)),
            thread: Mutex::new(None),
            receiver: Mutex::new(Some(receiver)),
            handoff: Arc::new((Mutex::new(Handoff::Waiting), Condvar::new())),
            blocked_budget,
        })
    }

    pub(super) fn spawn_with(
        &self,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<JoinHandle<()>>,
    ) -> io::Result<()> {
        self.spawn_with_ready_hook(spawn, || {})
    }

    pub(super) fn spawn_with_ready_hook(
        &self,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<JoinHandle<()>>,
        before_take: impl FnOnce() + Send + 'static,
    ) -> io::Result<()> {
        let receiver = self
            .receiver
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| io::Error::other("destination worker already attempted"))?;
        let worker = self.shared.clone();
        let handoff = self.handoff.clone();
        // These captures contain internal resources only, never a destination.
        let thread = spawn(Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                worker.0.lock().unwrap().report.worker = WorkerState::Ready;
                worker.0.lock().unwrap().report.ready = true;
                worker.1.notify_all();
                before_take();
                let destination = {
                    let mut cell = handoff.0.lock().unwrap();
                    loop {
                        if matches!(*cell, Handoff::Cancelled) {
                            return;
                        }
                        if worker.0.lock().unwrap().revoked {
                            return;
                        }
                        if matches!(*cell, Handoff::Offered(_)) {
                            let Handoff::Offered(destination) =
                                std::mem::replace(&mut *cell, Handoff::Taken)
                            else {
                                unreachable!()
                            };
                            handoff.1.notify_all();
                            break destination;
                        }
                        cell = handoff.1.wait(cell).unwrap();
                    }
                };
                run(destination, receiver, &worker);
            }));
            let mut state = worker.0.lock().unwrap();
            if result.is_err() {
                failure(&mut state, "destination worker panicked");
            }
            // run includes destruction of D; a blocked destructor cannot reach here.
            state.report.worker = WorkerState::Ended;
            drop(state);
            worker.1.notify_all();
            handoff.1.notify_all();
        }))?;
        *self.thread.lock().unwrap() = Some(thread);
        let mut state = self.shared.0.lock().unwrap();
        if state.report.worker == WorkerState::NeverCreated {
            state.report.worker = WorkerState::Created;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn offer(&self, destination: Box<dyn CaptureDestination>) {
        self.offer_with_hook(destination, || {});
    }

    pub(super) fn offer_with_hook(
        &self,
        destination: Box<dyn CaptureDestination>,
        after_offer: impl FnOnce(),
    ) {
        let mut cell = self.handoff.0.lock().unwrap();
        assert!(matches!(*cell, Handoff::Waiting));
        *cell = Handoff::Offered(destination);
        after_offer();
        drop(cell);
        self.handoff.1.notify_all();
    }

    pub(super) fn wait_taken(&self, deadline: Instant) -> bool {
        let mut cell = self.handoff.0.lock().unwrap();
        loop {
            if matches!(*cell, Handoff::Taken) {
                return true;
            }
            if matches!(*cell, Handoff::Cancelled) || self.failed() {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            cell = self.handoff.1.wait_timeout(cell, deadline - now).unwrap().0;
        }
    }

    pub(super) fn cancel_handoff(&self) -> Option<Box<dyn CaptureDestination>> {
        let mut cell = self.handoff.0.lock().unwrap();
        let destination = match std::mem::replace(&mut *cell, Handoff::Cancelled) {
            Handoff::Offered(destination) => Some(destination),
            Handoff::Taken => {
                *cell = Handoff::Taken;
                None
            }
            Handoff::Waiting | Handoff::Cancelled => None,
        };
        drop(cell);
        self.handoff.1.notify_all();
        destination
    }

    pub(super) fn taken(&self) -> bool {
        matches!(*self.handoff.0.lock().unwrap(), Handoff::Taken)
    }

    pub(super) fn failed(&self) -> bool {
        self.shared.0.lock().unwrap().revoked
    }

    pub(super) fn revoke(&self, message: &str) {
        failure(&mut self.shared.0.lock().unwrap(), message);
        self.shared.1.notify_all();
        self.handoff.1.notify_all();
    }

    // Legacy test-only helpers retained for the unchanged V4 publication tests.
    // Those tests use nonblocking Output values on creation failures. Production
    // startup uses prepare/spawn/offer and returns recovery in CaptureStartError.
    #[cfg(test)]
    pub fn start<D: CaptureDestination>(
        destination: D,
        records: usize,
        diagnostic_bytes: usize,
        blocked_budget: Duration,
    ) -> io::Result<Self> {
        Self::start_with(
            destination,
            records,
            diagnostic_bytes,
            blocked_budget,
            |worker| {
                std::thread::Builder::new()
                    .name("capture-output".into())
                    .spawn(worker)
            },
        )
    }

    #[cfg(test)]
    pub(super) fn start_with<D: CaptureDestination>(
        destination: D,
        records: usize,
        diagnostic_bytes: usize,
        blocked_budget: Duration,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<JoinHandle<()>>,
    ) -> io::Result<Self> {
        let publication = Self::prepare(records, diagnostic_bytes, blocked_budget)?;
        publication.spawn_with(spawn)?;
        publication.offer(Box::new(destination));
        Ok(publication)
    }

    pub fn enqueue(&self, record: Record) -> Result<(), Record> {
        let sender = self.sender.lock().unwrap();
        let mut state = self.shared.0.lock().unwrap();
        if state.revoked {
            return Err(record);
        }
        let order = record.order();
        let length = record.bytes().len();
        let retained = (state.diagnostic_bytes - state.report.diagnostic_prefix.len()).min(length);
        let prefix = record.bytes()[..retained].to_vec();
        match sender.as_ref() {
            Some(sender) => sender
                .try_send(record)
                .map(|()| {
                    retain_diagnostic(&mut state, &prefix, length);
                    unattempted(&mut state, order, length as u64);
                })
                .map_err(|error| match error {
                    mpsc::TrySendError::Full(record) | mpsc::TrySendError::Disconnected(record) => {
                        record
                    }
                }),
            None => Err(record),
        }
    }

    pub fn discard(&self, record: Record) {
        let mut state = self.shared.0.lock().unwrap();
        let retained = (state.diagnostic_bytes - state.report.diagnostic_prefix.len())
            .min(record.bytes().len());
        retain_diagnostic(
            &mut state,
            &record.bytes()[..retained],
            record.bytes().len(),
        );
        account_unpublished(&mut state, record.order(), record.bytes().len() as u64);
        drop(state);
        if record.release().is_err() {
            failure(
                &mut self.shared.0.lock().unwrap(),
                "discard credit release failed",
            );
        }
    }

    pub fn snapshot(&self) -> Report {
        let state = self.shared.0.lock().unwrap();
        state.frozen.as_ref().unwrap_or(&state.report).clone()
    }

    pub fn wait_ready(&self, deadline: Instant) -> bool {
        let mut state = self.shared.0.lock().unwrap();
        while !state.report.ready && state.report.error.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self.shared.1.wait_timeout(state, deadline - now).unwrap().0;
        }
        state.report.ready && state.report.error.is_none()
    }

    pub fn check_blocked(&self, now: Instant, budget: Duration) -> bool {
        let mut state = self.shared.0.lock().unwrap();
        if state
            .operation_started
            .is_some_and(|start| now.saturating_duration_since(start) >= budget)
        {
            failure(
                &mut state,
                "capture destination blocked beyond publication budget",
            );
        }
        state.revoked
    }

    pub fn close(&self) {
        self.sender.lock().unwrap().take();
        self.handoff.1.notify_all();
    }

    fn join_finished(&self) -> bool {
        let mut thread = self.thread.lock().unwrap();
        if thread.as_ref().is_some_and(|thread| thread.is_finished()) {
            let _ = thread.take().unwrap().join();
        }
        if thread.is_none() {
            let mut state = self.shared.0.lock().unwrap();
            if state.report.worker != WorkerState::NeverCreated {
                state.report.worker = WorkerState::Joined;
            }
            return matches!(state.report.worker, WorkerState::Joined);
        }
        false
    }

    pub fn finish_until(&self, deadline: Instant) -> Report {
        self.close();
        {
            let mut state = self.shared.0.lock().unwrap();
            state.deadline = Some(state.deadline.map_or(deadline, |old| old.min(deadline)));
        }
        loop {
            self.check_blocked(Instant::now(), self.blocked_budget);
            let joined = self.join_finished();
            let mut state = self.shared.0.lock().unwrap();
            if let Some(report) = &state.frozen {
                return report.clone();
            }
            if joined || state.report.worker == WorkerState::NeverCreated {
                state.report.finalized = true;
                state.report.stability = ArtifactStability::Stable;
                if !state.report.drained && state.report.error.is_none() {
                    failure(&mut state, "destination ended without a complete flush");
                }
                let report = state.report.clone();
                state.frozen = Some(report.clone());
                return report;
            }
            let now = Instant::now();
            let deadline = state.deadline.expect("deadline installed");
            if now >= deadline {
                state.report.finalized = true;
                failure(
                    &mut state,
                    "capture destination unsettled at final deadline",
                );
                let report = state.report.clone();
                state.frozen = Some(report.clone());
                return report;
            }
            let wait = (deadline - now).min(Duration::from_millis(1));
            let _ = self.shared.1.wait_timeout(state, wait).unwrap();
        }
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        self.sender.lock().unwrap().take();
        let mut state = self.shared.0.lock().unwrap();
        failure(&mut state, "publication owner dropped");
        drop(state);
        self.shared.1.notify_all();
    }
}

fn run(
    mut destination: Box<dyn CaptureDestination>,
    receiver: mpsc::Receiver<Record>,
    shared: &Shared,
) {
    run_output(&mut *destination, receiver, shared);
    shared
        .0
        .lock()
        .unwrap()
        .operation_started
        .get_or_insert_with(Instant::now);
    drop(destination);
    shared.0.lock().unwrap().operation_started = None;
}

fn run_output(
    destination: &mut dyn CaptureDestination,
    receiver: mpsc::Receiver<Record>,
    shared: &Shared,
) {
    let mut next = 1;
    while let Ok(record) = receiver.recv() {
        let revoked = {
            let mut state = shared.0.lock().unwrap();
            if record.order() != next {
                failure(
                    &mut state,
                    "destination received non-contiguous source order",
                );
            }
            state.revoked
        };
        if !revoked {
            publish(destination, &record, shared);
        }
        if record.release().is_err() {
            failure(
                &mut shared.0.lock().unwrap(),
                "capture credit release failed",
            );
        }
        next = match next.checked_add(1) {
            Some(next) => next,
            None => {
                failure(&mut shared.0.lock().unwrap(), "publication order overflow");
                break;
            }
        };
        shared.1.notify_all();
    }
    {
        let mut state = shared.0.lock().unwrap();
        if state.revoked {
            return;
        }
        state.operation_started = Some(Instant::now());
    }
    let result = destination.flush();
    let progress = destination.progress();
    {
        let mut state = shared.0.lock().unwrap();
        state.report.progress = progress;
        state.operation_started = None;
        match result {
            Ok(()) if !state.revoked => state.report.drained = true,
            Ok(()) => {}
            Err(_) => failure(&mut state, "destination flush failed"),
        }
    }
    shared.1.notify_all();
}

fn publish(destination: &mut dyn CaptureDestination, record: &Record, shared: &Shared) {
    if record.bytes().is_empty() {
        let mut state = shared.0.lock().unwrap();
        if !state.revoked && state.unattempted.remove(&record.order()) != Some(0) {
            failure(&mut state, "empty publication has no matching reservation");
        }
        return;
    }
    let mut offset = 0;
    while offset < record.bytes().len() {
        // progress is arbitrary external code, including this first call. The
        // operation clock does not claim that a byte range was attempted.
        {
            let mut state = shared.0.lock().unwrap();
            if state.revoked {
                return;
            }
            state.operation_started.get_or_insert_with(Instant::now);
        }
        let before = destination.progress();
        {
            let mut state = shared.0.lock().unwrap();
            if state.revoked {
                return;
            }
            if state.unattempted.remove(&record.order())
                != Some((record.bytes().len() - offset) as u64)
            {
                failure(
                    &mut state,
                    "publication attempt has no matching reservation",
                );
                return;
            }
            state.report.attempt = Some(Attempt {
                order: record.order(),
                acknowledged: offset,
                attempted_end: record.bytes().len(),
            });
            state.operation_started.get_or_insert_with(Instant::now);
        }
        let result = destination.write(&record.bytes()[offset..]);
        let after = destination.progress();
        let acknowledged = after
            .acknowledged_data_bytes
            .checked_sub(before.acknowledged_data_bytes)
            .and_then(|data| {
                after
                    .discarded_bytes
                    .checked_sub(before.discarded_bytes)
                    .and_then(|discarded| data.checked_add(discarded))
            })
            .and_then(|bytes| usize::try_from(bytes).ok());
        let mut state = shared.0.lock().unwrap();
        state.report.progress = after;
        let Some(count) = acknowledged.filter(|count| *count <= record.bytes().len() - offset)
        else {
            failure(&mut state, "invalid destination progress accounting");
            return;
        };
        offset += count;
        if let Some(attempt) = &mut state.report.attempt {
            attempt.acknowledged = offset;
        }
        match result {
            Ok(written) if written == count && written != 0 => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                failure(&mut state, "destination write failed");
                return;
            }
            _ => {
                failure(&mut state, "destination made no valid progress");
                return;
            }
        }
        state.report.attempt = None;
        if offset < record.bytes().len() {
            unattempted(
                &mut state,
                record.order(),
                (record.bytes().len() - offset) as u64,
            );
        }
        if state.revoked {
            return;
        }
        if count != 0 {
            state.operation_started = None;
        }
        drop(state);
        shared.1.notify_all();
    }
    let mut state = shared.0.lock().unwrap();
    state.report.attempt = None;
    state.operation_started = None;
}
