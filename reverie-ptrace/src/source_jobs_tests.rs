/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Actual OS/TLS join components, with explicitly MODELED source admission.
//! No SourceStop, epoch, FollowedHold, guest bytes or retirement token is issued.
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use super::*;

pub(crate) const BYTES: &[u8] = &[0, 255, 7, 42];

struct Retained(Arc<AtomicUsize>);
impl Drop for Retained {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

pub(crate) async fn wait_flag(flag: &AtomicBool, deadline: Instant) -> Result<(), String> {
    while !flag.load(Ordering::Acquire) {
        if Instant::now() >= deadline {
            return Err("original component deadline before event".into());
        }
        tokio::task::yield_now().await;
    }
    if Instant::now() >= deadline {
        return Err("event arrived after original component deadline".into());
    }
    Ok(())
}

pub(crate) fn poll_jobs(jobs: &SourceJobs, cancelled: bool) -> SourcePoll {
    jobs.poll(
        &mut Context::from_waker(std::task::Waker::noop()),
        cancelled,
    )
}

#[derive(Debug)]
pub(crate) struct Snapshot {
    pub(crate) complete: bool,
    pub(crate) failure: Option<String>,
    pub(crate) jobs: usize,
    pub(crate) drops: usize,
    pub(crate) has_result: bool,
    pub(crate) cancelled: bool,
    pub(crate) join_failed: bool,
    pub(crate) os_handle_retained: bool,
}

pub(crate) struct HeldSourceJob {
    entered: Arc<AtomicBool>,
    release: Option<std::sync::mpsc::Sender<()>>,
    drops: Arc<AtomicUsize>,
    observer: Option<SourceObserver>,
    observation: Arc<Observation>,
    // Same original handoff cell, retained for failure teardown if a mutant
    // discards its Job. Never read as authority or used to invent completion.
    os_thread: Arc<Mutex<Option<JoinHandle<ResultBytes>>>>,
}

impl HeldSourceJob {
    pub(crate) fn start(jobs: &SourceJobs, reader_error: bool) -> Self {
        let entered = Arc::new(AtomicBool::new(false));
        let (release, receiver) = std::sync::mpsc::channel();
        jobs.pause_next_retirement(RetirementPause {
            entered: entered.clone(),
            release: receiver,
        });
        let drops = Arc::new(AtomicUsize::new(0));
        // The production reservation, launch gate, worker and blocking join
        // are unchanged. Only stop/cohort admission is a declared premise.
        let observer = jobs
            .submit_owned(
                Box::new(Retained(drops.clone())),
                Authority::ComponentAdmissionPremise,
                move || {
                    if reader_error {
                        Err(refused(Errno::EIO))
                    } else {
                        Ok(BYTES.to_vec())
                    }
                },
            )
            .expect("component source launch");
        let observation = observer.0.clone();
        let os_thread = jobs
            .state
            .lock()
            .unwrap()
            .jobs
            .iter()
            .find(|job| Arc::ptr_eq(&job.observer, &observation))
            .expect("original registry slot")
            .os_thread
            .clone();
        Self {
            entered,
            release: Some(release),
            drops,
            observer: Some(observer),
            observation,
            os_thread,
        }
    }

    pub(crate) async fn wait_tls(&self, deadline: Instant) -> Result<(), String> {
        wait_flag(&self.entered, deadline).await
    }

    pub(crate) fn drop_observer(&mut self) {
        drop(self.observer.take());
    }

    pub(crate) fn release(&mut self) -> Result<(), String> {
        match self.release.take() {
            Some(sender) => sender.send(()).map_err(|e| format!("TLS release: {e}")),
            None => Ok(()),
        }
    }

    pub(crate) fn snapshot(&self, jobs: &SourceJobs, cancelled: bool) -> Snapshot {
        let observed = poll_jobs(jobs, cancelled);
        let state = jobs.state.lock().unwrap();
        let job = state
            .jobs
            .iter()
            .find(|job| Arc::ptr_eq(&job.observer, &self.observation));
        Snapshot {
            complete: observed.complete,
            failure: observed.failure,
            jobs: state.jobs.len(),
            drops: self.drops.load(Ordering::SeqCst),
            has_result: self.observation.result.lock().unwrap().is_some(),
            cancelled: self.observation.cancelled.load(Ordering::Acquire),
            join_failed: job.is_some_and(|job| job.join_failed),
            os_handle_retained: self.os_thread.lock().unwrap().is_some(),
        }
    }

    pub(crate) fn take_result(&mut self) -> Option<ResultBytes> {
        if let Some(observer) = self.observer.as_mut() {
            match Pin::new(observer).poll(&mut Context::from_waker(std::task::Waker::noop())) {
                Poll::Ready(result) => Some(result),
                Poll::Pending => None,
            }
        } else {
            self.observation.result.lock().unwrap().take()
        }
    }

    pub(crate) async fn finish(
        &mut self,
        jobs: &SourceJobs,
        deadline: Instant,
    ) -> Result<(), String> {
        let released = self.release();
        let mut errors = Vec::new();
        if let Err(error) = released {
            errors.push(error);
        }
        // Do not use SourcePoll.complete as the cleanup loop condition: the
        // early-complete mutant must still drive the original actual join.
        while jobs.pending_jobs() != 0 {
            if Instant::now() >= deadline {
                errors.push("original deadline before actual source join".into());
                break;
            }
            if let Some(error) = poll_jobs(jobs, false).failure {
                errors.push(error);
            }
            tokio::task::yield_now().await;
        }
        if Instant::now() >= deadline {
            errors.push("source cleanup finished after original component deadline".into());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    async fn finish_lost_executor(
        &mut self,
        jobs: &SourceJobs,
        deadline: Instant,
    ) -> Result<ResultBytes, String> {
        let released = self.release();
        let handle = self
            .os_thread
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| "lost executor no longer retains original OS handle".to_string())?;
        // Readiness is not proof. We still consume this exact handle in join.
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                *self.os_thread.lock().unwrap() = Some(handle);
                return Err("original deadline before failed-executor teardown join".into());
            }
            tokio::task::yield_now().await;
        }
        let result = handle
            .join()
            .map_err(|_| "original OS worker panicked".to_string());
        // Test-only teardown AFTER actual join, never production completion.
        // Keep join_failed intact; do not reset or grant a retirement token.
        let mut state = jobs.state.lock().unwrap();
        state
            .jobs
            .retain(|job| !Arc::ptr_eq(&job.observer, &self.observation));
        drop(state);
        released?;
        if Instant::now() >= deadline {
            return Err("failed-executor teardown finished after original deadline".into());
        }
        result
    }
}

impl Drop for HeldSourceJob {
    fn drop(&mut self) {
        // Failure-path progress only. Drop is never recorded as joined cleanup.
        let _ = self.release();
    }
}

async fn joined_case(drop_observer: bool, cancel_poll: bool, reader_error: bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let jobs = SourceJobs::default();
    jobs.enable();
    let mut held = HeldSourceJob::start(&jobs, reader_error);
    let arrival = held.wait_tls(deadline).await;
    if drop_observer {
        held.drop_observer();
    }
    let before = held.snapshot(&jobs, cancel_poll);
    let cleanup = held.finish(&jobs, deadline).await;
    let after = held.snapshot(&jobs, false);
    let result = held.take_result();
    // All test-oracle assertions follow original join cleanup, including on
    // causal mutants which prematurely reported completion or published bytes.
    assert!(cleanup.is_ok(), "source cleanup: {cleanup:?}");
    assert!(arrival.is_ok(), "TLS arrival: {arrival:?}");
    assert!(
        !before.complete,
        "closure return was substituted for true TLS join: {before:?}"
    );
    assert_eq!(before.jobs, 1);
    assert_eq!(before.drops, 0);
    assert!(!before.has_result);
    assert!(before.failure.is_none());
    assert_eq!(before.cancelled, drop_observer || cancel_poll);
    assert!(after.complete && after.failure.is_none(), "{after:?}");
    assert_eq!(after.jobs, 0);
    assert_eq!(after.drops, 1);
    let expected = if drop_observer || cancel_poll {
        Err(refused(Errno::ECANCELED))
    } else if reader_error {
        Err(refused(Errno::EIO))
    } else {
        Ok(BYTES.to_vec())
    };
    assert_eq!(result, Some(expected), "exact original result/cancellation");
}

#[tokio::test(flavor = "current_thread")]
async fn result_waits_for_actual_tls_join() {
    joined_case(false, false, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_observer_keeps_join_and_discards_bytes() {
    joined_case(true, false, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_poll_keeps_join_and_returns_ecanceled() {
    joined_case(false, true, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn reader_refusal_retires_only_after_join() {
    joined_case(false, false, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn already_joined_result_retires_once() {
    let deadline = Instant::now() + Duration::from_secs(3);
    let jobs = SourceJobs::default();
    jobs.enable();
    let mut held = HeldSourceJob::start(&jobs, false);
    let arrival = held.wait_tls(deadline).await;
    let release = held.release();
    let mut ready = false;
    while Instant::now() < deadline {
        ready = jobs.state.lock().unwrap().jobs[0]
            .join
            .as_ref()
            .unwrap()
            .is_finished();
        if ready {
            break;
        }
        tokio::task::yield_now().await;
    }
    // Readiness is only scheduling observation. finish polls the unchanged
    // actual JoinHandle and consumes Ready(Ok(OS-join result)) before removal.
    let cleanup = held.finish(&jobs, deadline).await;
    let result = held.take_result();
    let first = held.snapshot(&jobs, false);
    let second = held.snapshot(&jobs, false);
    assert!(cleanup.is_ok(), "{cleanup:?}");
    assert!(arrival.is_ok() && release.is_ok() && ready);
    assert_eq!(result, Some(Ok(BYTES.to_vec())));
    assert!(
        held.take_result().is_none(),
        "original result consumed twice"
    );
    for observed in [first, second] {
        assert!(
            observed.complete && observed.failure.is_none(),
            "{observed:?}"
        );
        assert_eq!((observed.jobs, observed.drops), (0, 1));
    }
}

#[test]
fn lost_join_executor_retains_original_thread_after_failure_is_consumed() {
    let deadline = Instant::now() + Duration::from_secs(3);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let occupied = Arc::new(AtomicBool::new(false));
        let worker_occupied = occupied.clone();
        let (release_blocker, blocker_gate) = std::sync::mpsc::channel();
        let mut blocker = tokio::task::spawn_blocking(move || {
            worker_occupied.store(true, Ordering::Release);
            blocker_gate.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        });
        let blocker_started = wait_flag(&occupied, deadline).await;
        // A missing start is a setup failure, not permission to launch another
        // worker after the original bound. Dropping the sender releases it.
        if blocker_started.is_err() {
            drop(release_blocker);
            let cleanup =
                tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut blocker)
                    .await;
            panic!("blocking-slot setup failed: {blocker_started:?}; cleanup={cleanup:?}");
        }
        let jobs = SourceJobs::default();
        jobs.enable();
        let mut held = HeldSourceJob::start(&jobs, false);
        let arrival = held.wait_tls(deadline).await;
        let before_abort = held.snapshot(&jobs, false);
        {
            let state = jobs.state.lock().unwrap();
            state.jobs[0].join.as_ref().unwrap().abort();
        }
        let released_blocker = release_blocker.send(());
        let blocker_join =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut blocker).await;
        let mut first = held.snapshot(&jobs, false);
        while first.failure.is_none() && Instant::now() < deadline {
            tokio::task::yield_now().await;
            first = held.snapshot(&jobs, false);
        }
        let second = held.snapshot(&jobs, false);
        let result_before_cleanup = held.take_result();
        let cleanup = held.finish_lost_executor(&jobs, deadline).await;
        assert_eq!(
            cleanup,
            Ok(Ok(BYTES.to_vec())),
            "actual original OS teardown join"
        );
        assert!(arrival.is_ok(), "{arrival:?}");
        assert!(released_blocker.is_ok());
        assert!(
            matches!(blocker_join, Ok(Ok(Ok(())))),
            "blocking-slot helper did not join: {blocker_join:?}"
        );
        assert!(!before_abort.complete && before_abort.os_handle_retained);
        assert_eq!(before_abort.drops, 0);
        assert!(
            first
                .failure
                .as_ref()
                .is_some_and(|e| e.starts_with("source join executor lost retirement:")),
            "{first:?}"
        );
        assert!(
            !first.complete && first.join_failed && first.os_handle_retained,
            "{first:?}"
        );
        assert_eq!((first.jobs, first.drops), (1, 0));
        assert!(first.cancelled && !first.has_result);
        assert!(
            second.failure.is_none() && !second.complete && second.join_failed,
            "{second:?}"
        );
        assert_eq!((second.jobs, second.drops), (1, 0));
        assert!(second.os_handle_retained);
        assert!(result_before_cleanup.is_none());
        assert_eq!(
            held.drops.load(Ordering::SeqCst),
            1,
            "test teardown only after real join"
        );
    });
}
