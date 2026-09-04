use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::TryLockError;

use reverie_rpc_transport::guest_log::Issue;
use reverie_rpc_transport::guest_log::IssueKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdioMode {
    Captured,
    Inherited,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureEvidence {
    pub display: String,
    pub debug: String,
}

impl FailureEvidence {
    pub(super) fn new(error: &(impl std::fmt::Display + std::fmt::Debug)) -> Self {
        Self {
            display: error.to_string(),
            debug: format!("{error:?}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamState {
    NotStarted,
    Inherited,
    Reading,
    Eof,
    ReadFailed(FailureEvidence),
    Interrupted,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct StreamEvidence {
    pub state: StreamState,
    pub chunks: Vec<Arc<[u8]>>,
}

impl StreamEvidence {
    pub fn bytes(&self) -> Vec<u8> {
        self.chunks.concat()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunCompletion {
    Pending,
    Succeeded,
    Failed(FailureEvidence),
    Interrupted,
}

#[derive(Clone, Debug)]
pub struct RunEvidence {
    pub mode: StdioMode,
    pub polled: bool,
    pub worker_submitted: bool,
    pub caller_cancelled: bool,
    pub completion: RunCompletion,
    pub spawned: bool,
    pub pid: Option<u32>,
    pub wait_status: Option<ExitStatus>,
    pub reaped: bool,
    pub wait_error: Option<FailureEvidence>,
    pub first_error: Option<FailureEvidence>,
    pub cleanup_issue: Option<Issue>,
    pub stdout: StreamEvidence,
    pub stderr: StreamEvidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotUnavailable {
    Busy,
    Poisoned,
}

#[derive(Clone, Debug)]
pub struct RunObserver {
    state: Arc<Mutex<RunEvidence>>,
}

#[derive(Clone, Copy)]
pub(super) enum Stream {
    Stdout,
    Stderr,
}

impl RunObserver {
    pub fn try_snapshot(&self) -> Result<RunEvidence, SnapshotUnavailable> {
        match self.state.try_lock() {
            Ok(state) => Ok(state.clone()),
            Err(TryLockError::WouldBlock) => Err(SnapshotUnavailable::Busy),
            Err(TryLockError::Poisoned(_)) => Err(SnapshotUnavailable::Poisoned),
        }
    }

    pub(super) fn new(mode: StdioMode) -> Self {
        let stream = StreamEvidence {
            state: match mode {
                StdioMode::Captured => StreamState::NotStarted,
                StdioMode::Inherited => StreamState::Inherited,
            },
            chunks: Vec::new(),
        };
        Self {
            state: Arc::new(Mutex::new(RunEvidence {
                mode,
                polled: false,
                worker_submitted: false,
                caller_cancelled: false,
                completion: RunCompletion::Pending,
                spawned: false,
                pid: None,
                wait_status: None,
                reaped: false,
                wait_error: None,
                first_error: None,
                cleanup_issue: None,
                stdout: stream.clone(),
                stderr: stream,
            })),
        }
    }

    pub(super) fn polled(&self) {
        self.state.lock().unwrap().polled = true;
    }

    pub(super) fn worker_submitted(&self) {
        self.state.lock().unwrap().worker_submitted = true;
    }

    pub(super) fn dropped(&self, caller: bool) {
        let mut state = self.state.lock().unwrap();
        if caller {
            state.caller_cancelled = true;
            if !state.worker_submitted && state.completion == RunCompletion::Pending {
                state.completion = RunCompletion::Interrupted;
            }
        } else if state.completion == RunCompletion::Pending {
            state.completion = RunCompletion::Interrupted;
        }
    }

    pub(super) fn failed(&self, error: &(impl std::fmt::Display + std::fmt::Debug)) {
        let failure = FailureEvidence::new(error);
        self.state
            .lock()
            .unwrap()
            .first_error
            .get_or_insert(failure);
    }

    pub(super) fn cleanup_unwound(&self, error: &impl std::fmt::Display) {
        let mut state = self.state.lock().unwrap();
        if let Some(issue) = &mut state.cleanup_issue {
            issue.message.push_str("; additional cleanup failure: ");
            issue.message.push_str(&error.to_string());
            return;
        }
        state.cleanup_issue = Some(Issue {
            kind: IssueKind::Cleanup,
            message: error.to_string(),
        });
    }

    pub(super) fn finished(&self, error: Option<&super::Error>) {
        if let Some(error) = error {
            self.failed(error);
        }
        self.state.lock().unwrap().completion = match error {
            Some(error) => RunCompletion::Failed(FailureEvidence::new(error)),
            None => RunCompletion::Succeeded,
        };
    }

    pub(super) fn spawned(&self, pid: Option<u32>) {
        let mut state = self.state.lock().unwrap();
        state.spawned = true;
        state.pid = pid;
    }

    pub(super) fn reaped(&self, status: ExitStatus) {
        let mut state = self.state.lock().unwrap();
        state.wait_status = Some(status);
        state.reaped = true;
    }

    pub(super) fn reaped_without_status(&self) {
        self.state.lock().unwrap().reaped = true;
    }

    pub(super) fn wait_failed(&self, error: &std::io::Error) {
        self.failed(error);
        self.state
            .lock()
            .unwrap()
            .wait_error
            .get_or_insert_with(|| FailureEvidence::new(error));
    }

    fn stream(state: &mut RunEvidence, stream: Stream) -> &mut StreamEvidence {
        match stream {
            Stream::Stdout => &mut state.stdout,
            Stream::Stderr => &mut state.stderr,
        }
    }

    pub(super) fn stream_state(&self, stream: Stream, value: StreamState) {
        Self::stream(&mut self.state.lock().unwrap(), stream).state = value;
    }

    pub(super) fn unavailable(&self, stream: Stream) {
        let mut state = self.state.lock().unwrap();
        if state.mode == StdioMode::Captured {
            Self::stream(&mut state, stream).state = StreamState::Unavailable;
        }
    }

    pub(super) fn append(&self, stream: Stream, bytes: &[u8]) {
        let chunk = Arc::from(bytes);
        Self::stream(&mut self.state.lock().unwrap(), stream)
            .chunks
            .push(chunk);
    }

    pub(super) fn output(&self) -> (Vec<u8>, Vec<u8>) {
        let state = self.state.lock().unwrap().clone();
        (state.stdout.bytes(), state.stderr.bytes())
    }
}

pub(super) struct Reading {
    observer: RunObserver,
    stream: Stream,
}

impl Reading {
    pub(super) fn new(observer: RunObserver, stream: Stream) -> Self {
        observer.stream_state(stream, StreamState::Reading);
        Self { observer, stream }
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        let mut state = self.observer.state.lock().unwrap();
        let stream = RunObserver::stream(&mut state, self.stream);
        if stream.state == StreamState::Reading {
            stream.state = StreamState::Interrupted;
        }
    }
}

#[cfg(test)]
mod tests;
