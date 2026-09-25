//! Process-local worker observations. No capture resources or shared-memory ABI.
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinPath {
    Finished,
    Blocking,
}

#[derive(Debug, Eq, PartialEq)]
pub struct WorkerSnapshot {
    /// None means spawning was not attempted, not that a worker was joined.
    pub spawned: Option<bool>,
    pub tid: Option<u32>,
    /// Result of the caught worker body, separate from the actual thread join.
    pub body_returned: Option<bool>,
    pub join_path: Option<JoinPath>,
    pub joining_tid: Option<u32>,
    pub join_succeeded: Option<bool>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct CaptureWorkerSnapshot {
    pub owner_pid: u32,
    pub collector: WorkerSnapshot,
    pub publication: WorkerSnapshot,
}

/// Default-off diagnostic handle. Keeping it alive retains only atomic facts,
/// never a capture owner, destination, factory, descriptor or join handle.
/// Snapshots taken during worker activity are not a transactional view.
#[derive(Clone)]
pub struct CaptureWorkerObservations {
    owner_pid: u32,
    pub(crate) collector: Arc<WorkerObservation>,
    pub(crate) publication: Arc<WorkerObservation>,
}

impl CaptureWorkerObservations {
    pub(crate) fn new() -> Self {
        Self {
            owner_pid: std::process::id(),
            collector: Arc::new(WorkerObservation::default()),
            publication: Arc::new(WorkerObservation::default()),
        }
    }

    pub fn snapshot(&self) -> CaptureWorkerSnapshot {
        CaptureWorkerSnapshot {
            owner_pid: self.owner_pid,
            collector: self.collector.snapshot(),
            publication: self.publication.snapshot(),
        }
    }
}

#[derive(Default)]
pub(crate) struct WorkerObservation {
    spawned: AtomicU8,
    tid: AtomicU32,
    body_returned: AtomicU8,
    join_path: AtomicU8,
    joining_tid: AtomicU32,
    join_succeeded: AtomicU8,
}

fn tid() -> u32 {
    unsafe { libc::syscall(libc::SYS_gettid) as u32 }
}

fn outcome(value: u8) -> Option<bool> {
    match value {
        0 => None,
        1 => Some(true),
        2 => Some(false),
        _ => unreachable!("private observation value"),
    }
}

impl WorkerObservation {
    pub(crate) fn spawned(&self, succeeded: bool) {
        self.spawned
            .store(if succeeded { 1 } else { 2 }, Ordering::Release);
    }
    pub(crate) fn entered(&self) {
        self.tid.store(tid(), Ordering::Release);
    }
    pub(crate) fn body_returned(&self, succeeded: bool) {
        self.body_returned
            .store(if succeeded { 1 } else { 2 }, Ordering::Release);
    }
    pub(crate) fn joining(&self, path: JoinPath) {
        self.joining_tid.store(tid(), Ordering::Relaxed);
        self.join_path.store(
            match path {
                JoinPath::Finished => 1,
                JoinPath::Blocking => 2,
            },
            Ordering::Release,
        );
    }
    pub(crate) fn joined(&self, succeeded: bool) {
        self.join_succeeded
            .store(if succeeded { 1 } else { 2 }, Ordering::Release);
    }
    fn snapshot(&self) -> WorkerSnapshot {
        let join_succeeded = outcome(self.join_succeeded.load(Ordering::Acquire));
        let join_path = match self.join_path.load(Ordering::Acquire) {
            0 => None,
            1 => Some(JoinPath::Finished),
            2 => Some(JoinPath::Blocking),
            _ => unreachable!("private join path"),
        };
        let nonzero = |value| (value != 0).then_some(value);
        WorkerSnapshot {
            spawned: outcome(self.spawned.load(Ordering::Acquire)),
            tid: nonzero(self.tid.load(Ordering::Acquire)),
            body_returned: outcome(self.body_returned.load(Ordering::Acquire)),
            join_path,
            joining_tid: nonzero(self.joining_tid.load(Ordering::Acquire)),
            join_succeeded,
        }
    }
}
