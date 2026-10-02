/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! The ordinary completion owner retains actual OS-thread handles through join.
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::thread::JoinHandle;
#[cfg(test)]
use std::time::Duration;

use futures::task::AtomicWaker;
use reverie::syscalls::NativeUserReadError as Error;
use reverie::syscalls::NativeUserReadRefusal as Refusal;
use safeptrace::Errno;
use safeptrace::SourceStop;

use super::source_cohort::FollowedHold;
use super::source_epoch::SourceEpoch;

type ResultBytes = Result<Vec<u8>, Error>;
fn refused(errno: Errno) -> Error {
    Error::Refused(Refusal::TargetState(errno))
}

#[derive(Default)]
pub(crate) struct SourceJobs {
    enabled: AtomicBool,
    state: Mutex<Registry>,
    wake: Arc<AtomicWaker>,
    retired: tokio::sync::Notify,
    #[cfg(test)]
    retirement_pause: Mutex<Option<RetirementPause>>,
    // A modeled launch Err, not OS resource-exhaustion evidence. No thread is
    // launched when set; the ordinary proven-no-thread retirement arm runs.
    #[cfg(test)]
    no_thread_launch_error: AtomicBool,
}
#[derive(Default)]
struct Registry {
    jobs: Vec<Job>,
    startup_failure: Option<String>,
}
struct Job {
    // Neither closure return, observer drop, nor cancellation drops this box.
    _retention: Option<Box<dyn Send + Sync>>,
    // The registry keeps the handoff cell before launching a retained join
    // task. Taking the OS handle transfers custody to that task, never to the
    // callback. The cell remains populated if the join task never starts.
    os_thread: Arc<Mutex<Option<JoinHandle<ResultBytes>>>>,
    join: Option<tokio::task::JoinHandle<ResultBytes>>,
    join_failed: bool,
    observer: Arc<Observation>,
    authority: Option<Authority>,
    // This SAME registry slot remains pending while its original resources
    // are destroyed outside the lock. The bit is not source authority.
    retiring: bool,
    polling: bool,
    followed: bool,
}

// Moved original resources, never a new admission or retirement certificate.
// The matching Job remains in Registry until every destruction completes.
struct Retirement {
    retention: Option<Box<dyn Send + Sync>>,
    authority: Option<Authority>,
    join: Option<tokio::task::JoinHandle<ResultBytes>>,
    observer: Arc<Observation>,
}

impl Job {
    fn take_retirement(&mut self) -> Retirement {
        assert!(!self.retiring && !self.polling);
        assert!(self._retention.is_some() && self.authority.is_some());
        self.retiring = true;
        Retirement {
            retention: self._retention.take(),
            authority: self.authority.take(),
            join: self.join.take(),
            observer: self.observer.clone(),
        }
    }
}
enum Authority {
    // Modeled admission for host join components only. It cannot construct a
    // SourceStop, SourceEpoch or FollowedHold and is absent outside libtests.
    #[cfg(test)]
    ComponentAdmissionPremise,
    // Models only the registry's followed-kind wait predicate. This does not
    // contain or issue physical cohort authority, just like the premise above.
    #[cfg(test)]
    ComponentFollowedAdmissionPremise,
    Legacy {
        stop: Arc<SourceStop>,
        epoch: Arc<SourceEpoch>,
    },
    Followed(Arc<FollowedHold>),
}
impl Authority {
    fn is_followed(&self) -> bool {
        #[cfg(test)]
        if matches!(self, Self::ComponentFollowedAdmissionPremise) {
            return true;
        }
        matches!(self, Self::Followed(_))
    }

    fn validate(&self) -> Result<(), Errno> {
        match self {
            #[cfg(test)]
            Self::ComponentAdmissionPremise => Ok(()),
            #[cfg(test)]
            Self::ComponentFollowedAdmissionPremise => Ok(()),
            Self::Legacy { stop, epoch } => epoch.validate(stop),
            Self::Followed(hold) => hold.validate(),
        }
    }
}
struct Observation {
    cancelled: AtomicBool,
    result: Mutex<Option<ResultBytes>>,
    wake: AtomicWaker,
}
impl Observation {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}
pub(crate) struct SourceObserver(Arc<Observation>);

impl Future for SourceObserver {
    type Output = ResultBytes;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.wake.register(cx.waker());
        match self.0.result.lock().unwrap().take() {
            Some(result) => Poll::Ready(result),
            None => Poll::Pending,
        }
    }
}
impl Drop for SourceObserver {
    fn drop(&mut self) {
        self.0.cancelled.store(true, Ordering::Release);
    }
}

impl SourceJobs {
    pub(crate) fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }
    pub(crate) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    // Called synchronously on the original ptracer before acquiring a cohort.
    // A completed closure or a dropped observer is not an empty registry.
    pub(crate) fn idle(&self) -> bool {
        self.state.lock().unwrap().jobs.is_empty()
    }

    pub(crate) fn submit(
        &self,
        retention: Box<dyn Send + Sync>,
        stop: Arc<SourceStop>,
        epoch: Arc<SourceEpoch>,
        work: impl FnOnce() -> ResultBytes + Send + 'static,
    ) -> Result<SourceObserver, Error> {
        self.submit_owned(retention, Authority::Legacy { stop, epoch }, work)
    }

    pub(crate) fn submit_followed(
        &self,
        retention: Box<dyn Send + Sync>,
        hold: Arc<FollowedHold>,
        work: impl FnOnce() -> ResultBytes + Send + 'static,
    ) -> Result<SourceObserver, Error> {
        self.submit_owned(retention, Authority::Followed(hold), work)
    }

    // Called before any cleanup signalling. Only the original CompletionWork
    // polls/joins jobs. Unknown ownership stays pending under its existing bound.
    pub(crate) async fn wait_followed_retirement(&self) {
        loop {
            let changed = self.retired.notified();
            if !self
                .state
                .lock()
                .unwrap()
                .jobs
                .iter()
                .any(|job| job.followed)
            {
                return;
            }
            changed.await;
        }
    }

    fn submit_owned(
        &self,
        retention: Box<dyn Send + Sync>,
        authority: Authority,
        work: impl FnOnce() -> ResultBytes + Send + 'static,
    ) -> Result<SourceObserver, Error> {
        if !self.enabled() {
            return Err(Error::Refused(Refusal::UnsupportedBackend));
        }
        authority.validate().map_err(refused)?;
        let observation = Arc::new(Observation {
            cancelled: AtomicBool::new(false),
            result: Mutex::new(None),
            wake: AtomicWaker::new(),
        });
        let mut state = self.state.lock().unwrap();
        // Reserve custody BEFORE even attempting launch. No effect can precede
        // this slot, and no callback owns the actual JoinHandle.
        state.jobs.push(Job {
            _retention: Some(retention),
            os_thread: Arc::new(Mutex::new(None)),
            join: None,
            join_failed: false,
            observer: observation.clone(),
            followed: authority.is_followed(),
            authority: Some(authority),
            retiring: false,
            polling: false,
        });
        let (start, gate) = std::sync::mpsc::sync_channel(1);
        let wake = self.wake.clone();
        #[cfg(test)]
        let retirement_pause = self.retirement_pause.lock().unwrap().take();
        // The reserved slot already prevents empty/complete observations.
        // A failed spawn can destroy its captured closure: do that unlocked too.
        drop(state);
        let launched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            if self.no_thread_launch_error.swap(false, Ordering::AcqRel) {
                return Err(std::io::Error::from_raw_os_error(libc::EAGAIN));
            }
            std::thread::Builder::new()
                .name("reverie-source".into())
                .spawn(move || {
                    struct WakeOnReturn(Arc<AtomicWaker>);
                    impl Drop for WakeOnReturn {
                        fn drop(&mut self) {
                            self.0.wake();
                        }
                    }
                    let _wake = WakeOnReturn(wake);
                    // Submission owns the handle before the first proc acquisition.
                    gate.recv().map_err(|_| refused(Errno::ECANCELED))?;
                    #[cfg(test)]
                    RETIREMENT_PAUSE
                        .with(|slot| *slot.borrow_mut() = retirement_pause.map(PauseDuringDrop));
                    work()
                })
        }));
        let mut state = self.state.lock().unwrap();
        let index = state
            .jobs
            .iter()
            .position(|job| Arc::ptr_eq(&job.observer, &observation))
            .expect("original reserved source slot");
        match launched {
            Ok(Ok(handle)) => {
                let job = &mut state.jobs[index];
                *job.os_thread.lock().unwrap() = Some(handle);
                let os_thread = job.os_thread.clone();
                // This blocking task is owned and polled by this exact slot.
                // It performs the TRUE OS join off the ptracer/executor thread,
                // including TLS destruction; is_finished is not used as proof.
                let join = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tokio::task::spawn_blocking(move || {
                        let handle = os_thread
                            .lock()
                            .unwrap()
                            .take()
                            .expect("one registered OS join owner");
                        handle.join().unwrap_or_else(|_| Err(refused(Errno::EIO)))
                    })
                }));
                match join {
                    Ok(join) => job.join = Some(join),
                    Err(_) => {
                        job.join_failed = true;
                        job.observer.cancelled.store(true, Ordering::Release);
                        state.startup_failure =
                            Some("source join executor did not return a handle".into());
                        // Dropping start prevents remote IO, but cannot certify
                        // actual thread retirement. Retain the registered slot.
                        return Err(refused(Errno::EIO));
                    }
                }
                // Both the actual thread and its join executor are now owned.
                // Even a gate failure retains them until actual retirement.
                let _ = start.send(());
            }
            Ok(Err(error)) => {
                // std::thread::Builder::spawn Err proves no thread was created.
                // Opaque Drop is still external code: keep a pending marker
                // until it completes, but never invoke it under Registry.
                let retirement = state.jobs[index].take_retirement();
                drop(state);
                if let Err(failure) = self.finish_retirement(retirement, None) {
                    self.state.lock().unwrap().startup_failure = Some(failure);
                    self.wake.wake();
                    return Err(refused(Errno::EIO));
                }
                return Err(refused(Errno::new(
                    error.raw_os_error().unwrap_or(libc::EIO),
                )));
            }
            Err(_) => {
                // A panicking launch did not return proof of non-execution or
                // a join handle. Keep the original slot and admission closed.
                state.jobs[index].join_failed = true;
                state.jobs[index]
                    .observer
                    .cancelled
                    .store(true, Ordering::Release);
                state.startup_failure = Some("source launch did not return ownership".into());
                return Err(refused(Errno::EIO));
            }
        }
        drop(state);
        self.wake.wake();
        Ok(SourceObserver(observation))
    }

    fn finish_retirement(
        &self,
        mut retirement: Retirement,
        result: Option<ResultBytes>,
    ) -> Result<(), String> {
        // Physical exclusion goes away before the caller's last admission
        // fence. Both are outside Registry. On panic, preserve everything not
        // yet destroyed and the original pending marker, never deliver bytes.
        let destroyed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(retirement.authority.take());
        }));
        let destroyed = destroyed.and_then(|()| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                drop(retirement.retention.take());
            }))
        });
        let destroyed = destroyed.and_then(|()| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                drop(retirement.join.take());
            }))
        });
        if let Err(payload) = destroyed {
            let mut state = self.state.lock().unwrap();
            let job = state
                .jobs
                .iter_mut()
                .find(|job| Arc::ptr_eq(&job.observer, &retirement.observer))
                .expect("original retiring registry slot");
            job.join_failed = true;
            job.observer.cancelled.store(true, Ordering::Release);
            drop(state);
            // A panic payload can itself have arbitrary Drop. Retain it and
            // the remaining original custody rather than risking double panic.
            std::mem::forget(payload);
            let _ = Box::leak(Box::new(retirement));
            return Err("source retirement destruction did not complete".into());
        }

        let mut state = self.state.lock().unwrap();
        let index = state
            .jobs
            .iter()
            .position(|job| Arc::ptr_eq(&job.observer, &retirement.observer))
            .expect("original retiring registry slot");
        assert!(state.jobs[index].retiring);
        let marker = state.jobs.remove(index);
        drop(state);
        // Its opaque fields and executor were moved above. Keep this drop and
        // the observation's last Arc/waker drop outside Registry nevertheless.
        drop(marker);
        if let Some(result) = result {
            let result = if retirement.observer.is_cancelled() {
                Err(refused(Errno::ECANCELED))
            } else {
                result
            };
            *retirement.observer.result.lock().unwrap() = Some(result);
        }
        let observer_notified = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            retirement.observer.wake.wake();
        }));
        // An observer panic must not skip the distinct retirement notification.
        let retired_notified = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.retired.notify_waiters();
        }));
        let mut notification_failed = false;
        for notified in [observer_notified, retired_notified] {
            if let Err(payload) = notified {
                std::mem::forget(payload);
                notification_failed = true;
            }
        }
        if notification_failed {
            return Err("source retirement notification panicked".into());
        }
        Ok(())
    }

    /// Poll from the ORIGINAL ordinary CompletionWork, before checking its
    /// terminal predicate. A result channel is deliberately not a retirement.
    pub(crate) fn poll(&self, cx: &mut Context<'_>, cancelled: bool) -> SourcePoll {
        self.wake.register(cx.waker());
        let mut state = self.state.lock().unwrap();
        let mut failure = state.startup_failure.take();
        let mut observations = Vec::new();
        for job in &state.jobs {
            if cancelled {
                job.observer.cancelled.store(true, Ordering::Release);
            }
            observations.push(job.observer.clone());
        }
        drop(state);
        for observation in observations {
            let mut state = self.state.lock().unwrap();
            let Some(index) = state
                .jobs
                .iter()
                .position(|job| Arc::ptr_eq(&job.observer, &observation))
            else {
                // A reentrant poll already retired this original slot.
                continue;
            };
            let job = &mut state.jobs[index];
            if job.polling || job.retiring || job.join_failed {
                continue;
            }
            let Some(mut join) = job.join.take() else {
                // Unknown/abandoned startup is still owned, never completed.
                continue;
            };
            // Tokio's poll may clone/drop/wake user-defined Context wakers.
            // The same reserved slot stays busy and marks this unique poll.
            job.polling = true;
            drop(state);
            let polled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Pin::new(&mut join).poll(cx)
            }));
            let mut state = self.state.lock().unwrap();
            let index = state
                .jobs
                .iter()
                .position(|job| Arc::ptr_eq(&job.observer, &observation))
                .expect("original polling registry slot");
            let job = &mut state.jobs[index];
            assert!(job.polling && job.join.is_none());
            job.join = Some(join);
            job.polling = false;
            let result = match polled {
                Ok(Poll::Pending) => continue,
                Ok(Poll::Ready(Ok(result))) => result,
                Err(payload) => {
                    job.join_failed = true;
                    job.observer.cancelled.store(true, Ordering::Release);
                    failure = Some("source join executor poll panicked".into());
                    drop(state);
                    std::mem::forget(payload);
                    continue;
                }
                Ok(Poll::Ready(Err(error))) => {
                    // A join-executor failure does NOT prove that the source
                    // thread joined. Retain original handle/custody as Pending;
                    // report once, so existing run cancellation/deadline governs.
                    job.join_failed = true;
                    job.observer.cancelled.store(true, Ordering::Release);
                    failure = Some(format!("source join executor lost retirement: {error}"));
                    drop(state);
                    drop(error);
                    continue;
                }
            };
            // Only this owned executor's successful return proves the actual
            // source OS handle joined. Its Result may still be a reader refusal.
            let result = if job.observer.is_cancelled() {
                Err(refused(Errno::ECANCELED))
            } else {
                job.authority
                    .as_ref()
                    .expect("active source authority")
                    .validate()
                    .map_err(refused)
                    .and(result)
            };
            let retirement = job.take_retirement();
            drop(state);
            if let Err(error) = self.finish_retirement(retirement, Some(result))
                && failure.is_none()
            {
                failure = Some(error);
            }
        }
        // External callbacks may have admitted another real job. Never return
        // a stale pre-callback terminal decision or ignore retiring markers.
        let state = self.state.lock().unwrap();
        SourcePoll {
            complete: state.jobs.is_empty(),
            failure,
        }
    }
}

pub(crate) struct SourcePoll {
    pub(crate) complete: bool,
    pub(crate) failure: Option<String>,
}

impl Drop for SourceJobs {
    fn drop(&mut self) {
        // Abandonment is not completion. Preserve the actual handles AND caller
        // custody if the outer owner itself is destroyed without driving cleanup.
        // Normal/Pending completion retains and polls this registry instead.
        for job in self
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .jobs
            .drain(..)
        {
            job.observer.cancelled.store(true, Ordering::Release);
            let _ = Box::leak(Box::new(job));
        }
    }
}

// This hook pauses actual TLS destruction AFTER the closure returned its real
// reader result. It supplies no admission/stop/MM evidence.
#[cfg(test)]
thread_local! {
    static RETIREMENT_PAUSE: std::cell::RefCell<Option<PauseDuringDrop>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(crate) struct RetirementPause {
    pub(crate) entered: Arc<AtomicBool>,
    pub(crate) release: std::sync::mpsc::Receiver<()>,
}
#[cfg(test)]
struct PauseDuringDrop(RetirementPause);
#[cfg(test)]
impl Drop for PauseDuringDrop {
    fn drop(&mut self) {
        self.0.entered.store(true, Ordering::Release);
        self.0
            .release
            .recv_timeout(Duration::from_secs(5))
            .expect("release actual source TLS destructor");
    }
}
#[cfg(test)]
impl SourceJobs {
    pub(crate) fn pause_next_retirement(&self, pause: RetirementPause) {
        *self.retirement_pause.lock().unwrap() = Some(pause);
    }
    pub(crate) fn pending_jobs(&self) -> usize {
        self.state.lock().unwrap().jobs.len()
    }
}

#[cfg(test)]
#[path = "source_jobs_tests.rs"]
pub(crate) mod current_registry_tests;

#[cfg(test)]
#[path = "source_jobs_retirement_tests.rs"]
mod retirement_tests;
