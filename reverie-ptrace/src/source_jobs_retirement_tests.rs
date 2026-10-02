/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Real OS/TLS workers and actual callback bodies; source admission is the
//! unchanged ComponentAdmissionPremise, NOT a stopped-task/cohort certificate.
use std::sync::Weak;
use std::sync::atomic::AtomicUsize;
use std::task::RawWaker;
use std::task::RawWakerVTable;
use std::task::Wake;
use std::task::Waker;
use std::time::Instant;

use super::current_registry_tests::BYTES;
use super::current_registry_tests::poll_jobs;
use super::current_registry_tests::wait_flag;
use super::*;

#[derive(Clone, Debug, Default)]
struct DropObservation {
    called: bool,
    unlocked: bool,
    pending_jobs: usize,
    result_visible: bool,
    idle: bool,
    nested_complete: bool,
    followed_wait_pending: bool,
}

struct DropProbe {
    jobs: Weak<SourceJobs>,
    observed: Arc<Mutex<DropObservation>>,
    dropped: Arc<AtomicBool>,
    cancel: bool,
    panic: bool,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        let jobs = self.jobs.upgrade().expect("original component owner");
        let mut observed = DropObservation {
            called: true,
            ..Default::default()
        };
        // Never block a negative test on the mutex whose misuse it diagnoses.
        // The original worker has already been actually joined at this point.
        if let Ok(state) = jobs.state.try_lock() {
            observed.unlocked = true;
            observed.pending_jobs = state.jobs.len();
            observed.result_visible = state
                .jobs
                .iter()
                .any(|job| job.observer.result.lock().unwrap().is_some());
            drop(state);
            observed.idle = jobs.idle();
            observed.nested_complete = poll_jobs(&jobs, self.cancel).complete;
            let mut followed = Box::pin(jobs.wait_followed_retirement());
            observed.followed_wait_pending = followed
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending();
        }
        *self.observed.lock().unwrap() = observed;
        self.dropped.store(true, Ordering::Release);
        if self.panic {
            panic!("injected original retention destructor panic");
        }
    }
}

#[derive(Clone, Debug, Default)]
struct WakeObservation {
    calls: usize,
    unlocked: bool,
    custody_released: bool,
    idle: bool,
    launched: bool,
}

struct WakeProbe {
    jobs: Weak<SourceJobs>,
    dropped: Arc<AtomicBool>,
    observed: Mutex<WakeObservation>,
    next: Mutex<Option<SourceObserver>>,
    reenter: bool,
}

impl Wake for WakeProbe {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let jobs = self.jobs.upgrade().expect("original component owner");
        let mut observed = self.observed.lock().unwrap();
        observed.calls += 1;
        observed.custody_released = self.dropped.load(Ordering::Acquire);
        if let Ok(state) = jobs.state.try_lock() {
            observed.unlocked = true;
            drop(state);
            observed.idle = jobs.idle();
            if self.reenter && observed.calls == 1 {
                // A genuine second production submission, not a fake empty
                // registry or a result injected through Observation.
                match jobs.submit_owned(Box::new(()), Authority::ComponentAdmissionPremise, || {
                    Ok(BYTES.to_vec())
                }) {
                    Ok(observer) => {
                        *self.next.lock().unwrap() = Some(observer);
                        observed.launched = true;
                    }
                    Err(_) => observed.launched = false,
                }
            }
        }
    }
}

fn make_jobs() -> Arc<SourceJobs> {
    let jobs = Arc::new(SourceJobs::default());
    jobs.enable();
    jobs
}

fn drop_probe(
    jobs: &Arc<SourceJobs>,
    cancel: bool,
    panic: bool,
) -> (Box<DropProbe>, Arc<Mutex<DropObservation>>, Arc<AtomicBool>) {
    let observed = Arc::new(Mutex::new(DropObservation::default()));
    let dropped = Arc::new(AtomicBool::new(false));
    (
        Box::new(DropProbe {
            jobs: Arc::downgrade(jobs),
            observed: observed.clone(),
            dropped: dropped.clone(),
            cancel,
            panic,
        }),
        observed,
        dropped,
    )
}

fn wake_probe(jobs: &Arc<SourceJobs>, dropped: Arc<AtomicBool>, reenter: bool) -> Arc<WakeProbe> {
    Arc::new(WakeProbe {
        jobs: Arc::downgrade(jobs),
        dropped,
        reenter,
        observed: Mutex::new(WakeObservation::default()),
        next: Mutex::new(None),
    })
}

struct Original {
    observer: SourceObserver,
    entered: Arc<AtomicBool>,
    release: Option<std::sync::mpsc::Sender<()>>,
}

impl Original {
    fn start(jobs: &SourceJobs, retention: Box<dyn Send + Sync>) -> Self {
        Self::start_with(jobs, retention, Authority::ComponentAdmissionPremise)
    }

    fn start_with(
        jobs: &SourceJobs,
        retention: Box<dyn Send + Sync>,
        authority: Authority,
    ) -> Self {
        let entered = Arc::new(AtomicBool::new(false));
        let (release, receive) = std::sync::mpsc::channel();
        jobs.pause_next_retirement(RetirementPause {
            entered: entered.clone(),
            release: receive,
        });
        let observer = jobs
            .submit_owned(retention, authority, || Ok(BYTES.to_vec()))
            .expect("original source launch");
        Self {
            observer,
            entered,
            release: Some(release),
        }
    }

    async fn ready(&mut self, jobs: &SourceJobs, deadline: Instant) -> Result<(), String> {
        let arrived = wait_flag(&self.entered, deadline).await;
        let released = self
            .release
            .take()
            .unwrap()
            .send(())
            .map_err(|e| e.to_string());
        // Readiness is only scheduling. The following real SourceJobs::poll
        // must consume Ready(Ok(actual OS join result)) before any assertion.
        let mut ready = false;
        while Instant::now() < deadline {
            ready = jobs
                .state
                .lock()
                .unwrap()
                .jobs
                .iter()
                .find(|job| Arc::ptr_eq(&job.observer, &self.observer.0))
                .and_then(|job| job.join.as_ref())
                .is_some_and(|join| join.is_finished());
            if ready {
                break;
            }
            tokio::task::yield_now().await;
        }
        arrived?;
        released?;
        if ready && Instant::now() < deadline {
            Ok(())
        } else {
            Err("original join readiness deadline".into())
        }
    }

    fn take(&mut self) -> Poll<ResultBytes> {
        Pin::new(&mut self.observer).poll(&mut Context::from_waker(Waker::noop()))
    }
}

impl Drop for Original {
    fn drop(&mut self) {
        // Progress on a setup failure, never a claimed join receipt.
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

async fn settle(jobs: &SourceJobs, deadline: Instant) -> Result<(), String> {
    let mut failure = None;
    while jobs.pending_jobs() != 0 && Instant::now() < deadline {
        if let Some(error) = poll_jobs(jobs, false).failure {
            failure = Some(error);
        }
        tokio::task::yield_now().await;
    }
    if let Some(error) = failure {
        return Err(error);
    }
    if jobs.pending_jobs() == 0 && Instant::now() < deadline {
        Ok(())
    } else {
        Err("original retirement cleanup deadline".into())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn observer_wake_is_unlocked_after_real_join() {
    let deadline = Instant::now() + Duration::from_secs(3);
    let jobs = make_jobs();
    let (retention, _, dropped) = drop_probe(&jobs, false, false);
    let mut original = Original::start(&jobs, retention);
    let probe = wake_probe(&jobs, dropped, false);
    let waker = Waker::from(probe.clone());
    let before = Pin::new(&mut original.observer).poll(&mut Context::from_waker(&waker));
    let ready = original.ready(&jobs, deadline).await;
    let outcome = poll_jobs(&jobs, false);
    let cleanup = settle(&jobs, deadline).await;
    let result = original.take();
    let seen = probe.observed.lock().unwrap().clone();
    assert!(
        cleanup.is_ok() && ready.is_ok(),
        "cleanup={cleanup:?}; readiness={ready:?}"
    );
    assert!(before.is_pending());
    assert!(outcome.complete && outcome.failure.is_none());
    assert_eq!(result, Poll::Ready(Ok(BYTES.to_vec())));
    assert_eq!(seen.calls, 1);
    assert!(
        seen.unlocked && seen.custody_released && seen.idle,
        "observer callback preceded unlocked custody retirement: {seen:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn opaque_drop_is_unlocked_but_retirement_and_result_stay_pending() {
    let deadline = Instant::now() + Duration::from_secs(3);
    let jobs = make_jobs();
    let (retention, observed, _) = drop_probe(&jobs, false, false);
    let mut original = Original::start(&jobs, retention);
    let ready = original.ready(&jobs, deadline).await;
    let outcome = poll_jobs(&jobs, false);
    let cleanup = settle(&jobs, deadline).await;
    let result = original.take();
    let seen = observed.lock().unwrap().clone();
    assert!(
        cleanup.is_ok() && ready.is_ok(),
        "cleanup={cleanup:?}; readiness={ready:?}"
    );
    assert!(outcome.complete && outcome.failure.is_none());
    assert_eq!(result, Poll::Ready(Ok(BYTES.to_vec())));
    assert!(
        seen.called && seen.unlocked,
        "opaque Drop held Registry: {seen:?}"
    );
    assert_eq!(
        seen.pending_jobs, 1,
        "retirement disappeared before Drop returned: {seen:?}"
    );
    assert!(
        !seen.result_visible && !seen.idle && !seen.nested_complete,
        "in-progress retirement was certified: {seen:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_during_retirement_discards_result() {
    let deadline = Instant::now() + Duration::from_secs(3);
    let jobs = make_jobs();
    let (retention, observed, _) = drop_probe(&jobs, true, false);
    let mut original = Original::start(&jobs, retention);
    let ready = original.ready(&jobs, deadline).await;
    let outcome = poll_jobs(&jobs, false);
    let cleanup = settle(&jobs, deadline).await;
    let result = original.take();
    assert!(
        cleanup.is_ok() && ready.is_ok(),
        "cleanup={cleanup:?}; readiness={ready:?}"
    );
    assert!(outcome.complete && outcome.failure.is_none());
    assert!(observed.lock().unwrap().unlocked);
    assert_eq!(
        result,
        Poll::Ready(Err(refused(Errno::ECANCELED))),
        "cancellation during actual retention Drop was lost"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn modeled_followed_wait_cannot_finish_inside_retention_drop() {
    let deadline = Instant::now() + Duration::from_secs(3);
    let jobs = make_jobs();
    let (retention, observed, _) = drop_probe(&jobs, false, false);
    let mut original = Original::start_with(
        &jobs,
        retention,
        Authority::ComponentFollowedAdmissionPremise,
    );
    let ready = original.ready(&jobs, deadline).await;
    let outcome = poll_jobs(&jobs, false);
    let cleanup = settle(&jobs, deadline).await;
    let result = original.take();
    let mut followed = Box::pin(jobs.wait_followed_retirement());
    let after = followed
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()));
    let seen = observed.lock().unwrap().clone();
    assert!(
        cleanup.is_ok() && ready.is_ok(),
        "cleanup={cleanup:?}; readiness={ready:?}"
    );
    assert!(outcome.complete && outcome.failure.is_none() && after.is_ready());
    assert_eq!(result, Poll::Ready(Ok(BYTES.to_vec())));
    assert!(
        seen.unlocked && seen.followed_wait_pending,
        "followed-kind waiter escaped while original custody Drop was in progress: {seen:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reentrant_wake_submits_real_neighbor_before_terminal_decision() {
    let deadline = Instant::now() + Duration::from_secs(3);
    let jobs = make_jobs();
    let (retention, _, dropped) = drop_probe(&jobs, false, false);
    let mut original = Original::start(&jobs, retention);
    let probe = wake_probe(&jobs, dropped, true);
    let waker = Waker::from(probe.clone());
    let before = Pin::new(&mut original.observer).poll(&mut Context::from_waker(&waker));
    let ready = original.ready(&jobs, deadline).await;
    let first = poll_jobs(&jobs, false);
    let cleanup = settle(&jobs, deadline).await;
    let result = original.take();
    let neighbor = probe
        .next
        .lock()
        .unwrap()
        .as_mut()
        .map(|observer| Pin::new(observer).poll(&mut Context::from_waker(Waker::noop())));
    let seen = probe.observed.lock().unwrap().clone();
    assert!(
        cleanup.is_ok() && ready.is_ok(),
        "cleanup={cleanup:?}; readiness={ready:?}"
    );
    assert!(before.is_pending());
    assert_eq!(result, Poll::Ready(Ok(BYTES.to_vec())));
    assert!(
        seen.unlocked && seen.custody_released && seen.idle && seen.launched,
        "{seen:?}"
    );
    assert!(
        !first.complete && first.failure.is_none(),
        "stale terminal decision across reentry"
    );
    assert_eq!(
        neighbor,
        Some(Poll::Ready(Ok(BYTES.to_vec()))),
        "original second worker did not join"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn retired_waiter_wake_is_unlocked_after_custody_release() {
    let deadline = Instant::now() + Duration::from_secs(3);
    let jobs = make_jobs();
    let (retention, _, dropped) = drop_probe(&jobs, false, false);
    let mut original = Original::start(&jobs, retention);
    let probe = wake_probe(&jobs, dropped, false);
    let waker = Waker::from(probe.clone());
    let mut notified = Box::pin(jobs.retired.notified());
    let before = notified.as_mut().poll(&mut Context::from_waker(&waker));
    let ready = original.ready(&jobs, deadline).await;
    let outcome = poll_jobs(&jobs, false);
    let cleanup = settle(&jobs, deadline).await;
    let notification = notified
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()));
    let seen = probe.observed.lock().unwrap().clone();
    assert!(
        cleanup.is_ok() && ready.is_ok(),
        "cleanup={cleanup:?}; readiness={ready:?}"
    );
    assert!(before.is_pending() && notification.is_ready());
    assert!(outcome.complete && outcome.failure.is_none());
    assert_eq!(original.take(), Poll::Ready(Ok(BYTES.to_vec())));
    assert_eq!(seen.calls, 1);
    assert!(
        seen.unlocked && seen.custody_released && seen.idle,
        "retired notification under Registry: {seen:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn panicking_retention_never_certifies_retirement_or_result() {
    let deadline = Instant::now() + Duration::from_secs(3);
    let jobs = make_jobs();
    let (retention, observed, dropped) = drop_probe(&jobs, false, true);
    let mut original = Original::start(&jobs, retention);
    let ready = original.ready(&jobs, deadline).await;
    // Also catches the unfixed baseline's uncaught destructor panic, after its
    // real join, so the unchanged assertion diagnoses outcome not a hung worker.
    let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| poll_jobs(&jobs, false)));
    let poisoned = jobs.state.is_poisoned();
    let (count, failed, published) = {
        let state = jobs.state.lock().unwrap_or_else(|e| e.into_inner());
        (
            state.jobs.len(),
            state.jobs.iter().any(|job| job.join_failed),
            original.observer.0.result.lock().unwrap().is_some(),
        )
    };
    // No test-only removal/regrant: unknown destruction remains quarantined.
    assert!(ready.is_ok(), "{ready:?}");
    assert!(
        dropped.load(Ordering::Acquire),
        "actual post-join Drop was not reached"
    );
    assert!(
        first.is_ok(),
        "retention panic escaped/poisoned original owner"
    );
    let first = first.unwrap();
    assert!(
        !poisoned
            && !first.complete
            && first.failure.as_deref() == Some("source retirement destruction did not complete")
    );
    assert_eq!(count, 1);
    assert!(failed && !published && observed.lock().unwrap().unlocked);
    let next = poll_jobs(&jobs, false);
    assert!(!next.complete && next.failure.is_none());
    assert!(original.take().is_pending());
    assert!(Instant::now() < deadline);
}

#[tokio::test(flavor = "current_thread")]
async fn modeled_no_thread_error_uses_unlocked_retirement_branch() {
    let deadline = Instant::now() + Duration::from_secs(3);
    let jobs = make_jobs();
    let (retention, observed, dropped) = drop_probe(&jobs, false, false);
    let calls = Arc::new(AtomicUsize::new(0));
    let worker_calls = calls.clone();
    let (capture, capture_observed, capture_dropped) = drop_probe(&jobs, false, false);
    jobs.no_thread_launch_error.store(true, Ordering::Release);
    let result = jobs.submit_owned(retention, Authority::ComponentAdmissionPremise, move || {
        let _captured = &capture;
        worker_calls.fetch_add(1, Ordering::SeqCst);
        Ok(BYTES.to_vec())
    });
    let seen = observed.lock().unwrap().clone();
    assert!(matches!(result, Err(error) if error == refused(Errno::EAGAIN)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(dropped.load(Ordering::Acquire));
    let captured = capture_observed.lock().unwrap().clone();
    assert!(
        capture_dropped.load(Ordering::Acquire)
            && captured.unlocked
            && captured.pending_jobs == 1
            && !captured.result_visible
            && !captured.idle
            && !captured.nested_complete,
        "launch closure was destroyed under Registry: {captured:?}"
    );
    assert!(
        seen.called
            && seen.unlocked
            && seen.pending_jobs == 1
            && !seen.idle
            && !seen.nested_complete
            && !seen.result_visible,
        "modeled no-thread arm bypassed unlocked pending retirement: {seen:?}"
    );
    assert!(jobs.idle() && poll_jobs(&jobs, false).complete);
    assert!(Instant::now() < deadline);
}

#[derive(Debug)]
struct WakerEvent {
    operation: &'static str,
    original_thread: bool,
    unlocked: bool,
    pending_without_handle: bool,
}

struct ContextProbe {
    jobs: Weak<SourceJobs>,
    original_thread: std::thread::ThreadId,
    events: Mutex<Vec<WakerEvent>>,
    cancel_on_pending: bool,
    cancelled: AtomicBool,
    nested_complete: AtomicBool,
}

impl ContextProbe {
    fn record(&self, operation: &'static str) {
        if let Some(jobs) = self.jobs.upgrade() {
            let original_thread = std::thread::current().id() == self.original_thread;
            let (unlocked, pending_without_handle) = match jobs.state.try_lock() {
                Ok(state) => (true, state.jobs.iter().any(|job| job.join.is_none())),
                Err(_) => (false, false),
            };
            self.events.lock().unwrap().push(WakerEvent {
                operation,
                original_thread,
                unlocked,
                pending_without_handle,
            });
            if self.cancel_on_pending
                && original_thread
                && operation == "clone"
                && unlocked
                && pending_without_handle
                && !self.cancelled.swap(true, Ordering::AcqRel)
            {
                self.nested_complete
                    .store(poll_jobs(&jobs, true).complete, Ordering::Release);
            }
        }
    }
}

// A real Waker vtable, not injected passing observations. Each clone owns one
// Arc reference; wake/drop each release exactly one, wake_by_ref owns none.
unsafe fn context_clone(data: *const ()) -> RawWaker {
    let original =
        std::mem::ManuallyDrop::new(unsafe { Arc::from_raw(data.cast::<ContextProbe>()) });
    original.record("clone");
    context_raw(Arc::clone(&original))
}
unsafe fn context_wake(data: *const ()) {
    drop(unsafe { Arc::from_raw(data.cast::<ContextProbe>()) });
}
unsafe fn context_wake_by_ref(_data: *const ()) {}
unsafe fn context_drop(data: *const ()) {
    let original = unsafe { Arc::from_raw(data.cast::<ContextProbe>()) };
    original.record("drop");
    drop(original);
}
static CONTEXT_VTABLE: RawWakerVTable = RawWakerVTable::new(
    context_clone,
    context_wake,
    context_wake_by_ref,
    context_drop,
);
fn context_raw(probe: Arc<ContextProbe>) -> RawWaker {
    RawWaker::new(Arc::into_raw(probe).cast(), &CONTEXT_VTABLE)
}

async fn join_context_case(cancel: bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let jobs = make_jobs();
    let mut original = Original::start(&jobs, Box::new(()));
    let arrival = wait_flag(&original.entered, deadline).await;
    let probe = Arc::new(ContextProbe {
        jobs: Arc::downgrade(&jobs),
        original_thread: std::thread::current().id(),
        events: Mutex::new(Vec::new()),
        cancel_on_pending: cancel,
        cancelled: AtomicBool::new(false),
        nested_complete: AtomicBool::new(false),
    });
    let waker = unsafe { Waker::from_raw(context_raw(probe.clone())) };
    // TLS is still held: the real blocking join must register a pending waker.
    let before = jobs.poll(&mut Context::from_waker(&waker), false);
    let ready = original.ready(&jobs, deadline).await;
    let cleanup = settle(&jobs, deadline).await;
    let result = original.take();
    drop(waker);
    let events = probe.events.lock().unwrap();
    assert!(
        arrival.is_ok() && ready.is_ok() && cleanup.is_ok(),
        "arrival={arrival:?}; readiness={ready:?}; cleanup={cleanup:?}"
    );
    assert!(!before.complete && before.failure.is_none());
    // Other-thread lock contention is not same-thread lock ownership. Retain
    // those events, but this oracle targets the actual polling-thread callback.
    assert!(
        events
            .iter()
            .filter(|event| event.original_thread)
            .all(|event| event.unlocked),
        "join waker callback held original Registry: {events:?}"
    );
    assert!(
        events.iter().any(|event| event.original_thread
            && event.operation == "clone"
            && event.pending_without_handle),
        "actual pending join did not clone through its retained slot: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event.original_thread && event.operation == "drop")
    );
    assert_eq!(probe.cancelled.load(Ordering::Acquire), cancel);
    assert!(!probe.nested_complete.load(Ordering::Acquire));
    let expected = if cancel {
        Err(refused(Errno::ECANCELED))
    } else {
        Ok(BYTES.to_vec())
    };
    assert_eq!(
        result,
        Poll::Ready(expected),
        "poll restoration lost reentrant cancellation"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn join_context_clone_and_drop_are_unlocked_with_original_slot_pending() {
    join_context_case(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn reentrant_join_context_cancellation_survives_pending_restoration() {
    join_context_case(true).await;
}
