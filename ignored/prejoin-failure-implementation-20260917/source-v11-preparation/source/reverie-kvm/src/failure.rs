/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Run-owned failure notification and typed cause retention.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use futures::FutureExt;
use futures::channel::oneshot;
use futures::future::Shared;
use futures::future::select;
use reverie::BackendFailure;
use reverie::GlobalTool;
use reverie::Pid;

use crate::Error;

pub(crate) type FailureSubscription = Shared<oneshot::Receiver<()>>;

pub(crate) struct RunFailure {
    primary: Mutex<Option<(Arc<Error>, BackendFailure)>>,
    publication: Mutex<()>,
    published: AtomicBool,
    sender: Mutex<Option<oneshot::Sender<()>>>,
    receiver: FailureSubscription,
    report: Box<dyn Fn(BackendFailure) + Send + Sync>,
}

impl RunFailure {
    pub(crate) fn new<G: GlobalTool + 'static>(global: &Arc<G>) -> Arc<Self> {
        let (sender, receiver) = oneshot::channel();
        let global = Arc::downgrade(global);
        Arc::new(Self {
            primary: Mutex::new(None),
            publication: Mutex::new(()),
            published: AtomicBool::new(false),
            sender: Mutex::new(Some(sender)),
            receiver: receiver.shared(),
            report: Box::new(move |event| {
                global
                    .upgrade()
                    .expect("KVM failure outlived its GlobalState")
                    .report_backend_failure(event);
            }),
        })
    }

    pub(crate) fn subscribe(&self) -> FailureSubscription {
        self.receiver.clone()
    }

    pub(crate) fn primary(&self) -> Option<Arc<Error>> {
        self.primary
            .lock()
            .expect("KVM failure lock poisoned")
            .as_ref()
            .map(|(cause, _)| cause.clone())
    }

    pub(crate) fn published_primary(&self) -> Option<Arc<Error>> {
        self.published
            .load(Ordering::Acquire)
            .then(|| self.primary())
            .flatten()
    }

    fn publish(&self, event: BackendFailure, error: Error) -> Error {
        if matches!(error, Error::RunAborted) {
            return self.primary().map_or(error, Error::SharedFailure);
        }
        // Select the first cause and its event in the same publication order.
        // Keep this lock only through the synchronous terminal hook and local
        // notification, never through a guest RPC or a physical join.
        let _publication = self
            .publication
            .lock()
            .expect("KVM failure publication lock poisoned");
        if self
            .primary()
            .is_some_and(|primary| error.retains_primary(&primary))
        {
            return error;
        }
        let error = Arc::new(error);
        self.primary
            .lock()
            .expect("KVM failure lock poisoned")
            .get_or_insert_with(|| (error.clone(), event));
        // This synchronous hook must close the Tool's terminal transaction
        // before either its subscribers or the local driver wake into cleanup.
        (self.report)(event);
        self.published.store(true, Ordering::Release);
        if let Some(sender) = self
            .sender
            .lock()
            .expect("KVM failure sender lock poisoned")
            .take()
        {
            let _ = sender.send(());
        }
        Error::SharedFailure(error)
    }
}

#[derive(Clone)]
pub(crate) struct FailureContext {
    pub(crate) run: Arc<RunFailure>,
    pid: Pid,
    tid: Pid,
}

impl FailureContext {
    pub(crate) fn new(run: Arc<RunFailure>, pid: Pid, tid: Pid) -> Self {
        Self { run, pid, tid }
    }

    pub(crate) fn for_process(&self, pid: Pid) -> Self {
        Self::new(self.run.clone(), pid, pid)
    }

    pub(crate) fn for_thread(&self, tid: Pid) -> Self {
        Self::new(self.run.clone(), self.pid, tid)
    }

    pub(crate) fn publish(&self, phase: &'static str, error: Error) -> Error {
        self.run.publish(
            BackendFailure {
                pid: self.pid,
                tid: self.tid,
                phase,
            },
            error,
        )
    }
}

pub(crate) async fn wait_for_failure<G: GlobalTool>(
    global: &G,
    local: Option<FailureSubscription>,
) {
    let local = async {
        match local {
            Some(receiver) => {
                let _ = receiver.await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    let _ = select(std::pin::pin!(local), global.wait_for_backend_failure()).await;
}

/// Keep the initialized child state recoverable if the OS refuses the spawn.
/// A successful worker takes sole ownership before doing any child work.
pub(crate) fn spawn_owned<S, R, F>(
    builder: std::thread::Builder,
    state: S,
    run: F,
) -> std::result::Result<std::thread::JoinHandle<R>, (std::io::Error, S)>
where
    S: Send + 'static,
    R: Send + 'static,
    F: FnOnce(S) -> R + Send + 'static,
{
    let state = Arc::new(Mutex::new(Some(state)));
    let child_state = state.clone();
    match builder.spawn(move || {
        let state = child_state
            .lock()
            .expect("KVM child state lock poisoned")
            .take()
            .expect("KVM child state consumed twice");
        run(state)
    }) {
        Ok(handle) => Ok(handle),
        Err(error) => {
            let state = state
                .lock()
                .expect("KVM child state lock poisoned")
                .take()
                .expect("failed KVM spawn consumed child state");
            Err((error, state))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::task::Context;
    use std::task::Poll;

    use futures::task::noop_waker;

    use super::*;

    #[test]
    fn independent_failure_subscribers_wake_before_and_after_publication() {
        let global = Arc::new(());
        let failure = RunFailure::new(&global);
        let mut first = failure.subscribe();
        let mut second = failure.subscribe();
        let first_wakes = Arc::new(Wakes::default());
        let second_wakes = Arc::new(Wakes::default());
        let waker1 = futures::task::waker(first_wakes.clone());
        let waker2 = futures::task::waker(second_wakes.clone());
        assert!(
            std::pin::Pin::new(&mut first)
                .poll(&mut Context::from_waker(&waker1))
                .is_pending()
        );
        assert!(
            std::pin::Pin::new(&mut second)
                .poll(&mut Context::from_waker(&waker2))
                .is_pending()
        );
        let context = FailureContext::new(failure.clone(), Pid::from_raw(1), Pid::from_raw(2));
        let original = context.publish("setup", Error::GuestClock("original".to_owned()));
        assert!(matches!(original.primary(), Error::GuestClock(_)));
        assert!(first_wakes.0.load(Ordering::SeqCst) > 0);
        assert!(second_wakes.0.load(Ordering::SeqCst) > 0);
        assert_eq!(futures::executor::block_on(first), Ok(()));
        assert_eq!(futures::executor::block_on(second), Ok(()));
        assert_eq!(futures::executor::block_on(failure.subscribe()), Ok(()));
        context.publish("cleanup", Error::HostIo(std::io::Error::other("secondary")));
        assert!(matches!(
            failure.primary().unwrap().primary(),
            Error::GuestClock(_)
        ));
        drop(failure);
        drop(context);
        assert!(
            Arc::try_unwrap(global).is_ok(),
            "reporter retained global ownership"
        );
    }

    #[derive(Default)]
    struct Wakes(std::sync::atomic::AtomicUsize);
    impl futures::task::ArcWake for Wakes {
        fn wake_by_ref(value: &Arc<Self>) {
            value.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct OrderedGlobal {
        entered: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }
    #[reverie::global_tool]
    impl GlobalTool for OrderedGlobal {
        type Request = ();
        type Response = ();
        type Config = ();
        async fn receive_rpc(&self, _: Pid, _: ()) {}
        fn report_backend_failure(&self, _: BackendFailure) {
            self.entered
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(())
                .unwrap();
            self.release.lock().unwrap().take().unwrap().recv().unwrap();
        }
    }

    #[test]
    fn local_failure_wake_waits_for_synchronous_tool_terminal_transition() {
        let global = Arc::new(OrderedGlobal::default());
        let (entered, receive_entered) = std::sync::mpsc::channel();
        let (release, receive_release) = std::sync::mpsc::channel();
        *global.entered.lock().unwrap() = Some(entered);
        *global.release.lock().unwrap() = Some(receive_release);
        let failure = RunFailure::new(&global);
        let mut subscription = failure.subscribe();
        let context = FailureContext::new(failure.clone(), Pid::from_raw(1), Pid::from_raw(2));
        let publisher = std::thread::spawn(move || {
            context.publish("worker", Error::GuestClock("primary".to_owned()))
        });
        let entered = receive_entered
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_ok();
        let pending = matches!(
            std::pin::Pin::new(&mut subscription).poll(&mut Context::from_waker(&noop_waker())),
            Poll::Pending
        );
        let unpublished = failure.published_primary().is_none();
        release.send(()).unwrap();
        publisher.join().unwrap();
        assert!(
            entered && pending && unpublished,
            "local cleanup escaped before Tool publication completed"
        );
        assert_eq!(futures::executor::block_on(subscription), Ok(()));
        assert!(failure.published_primary().is_some());
    }

    #[derive(Default)]
    struct ConcurrentGlobal {
        events: Mutex<Vec<BackendFailure>>,
        selected: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    #[reverie::global_tool]
    impl GlobalTool for ConcurrentGlobal {
        type Request = ();
        type Response = ();
        type Config = ();
        async fn receive_rpc(&self, _: Pid, _: ()) {}
        fn report_backend_failure(&self, event: BackendFailure) {
            if event.phase == "first worker" {
                self.selected
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                self.release.lock().unwrap().take().unwrap().recv().unwrap();
            }
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn concurrent_publishers_pair_first_typed_cause_with_first_terminal_event() {
        let global = Arc::new(ConcurrentGlobal::default());
        let (selected, selected_receiver) = std::sync::mpsc::channel();
        let (release, release_receiver) = std::sync::mpsc::channel();
        *global.selected.lock().unwrap() = Some(selected);
        *global.release.lock().unwrap() = Some(release_receiver);
        let failure = RunFailure::new(&global);
        let first = FailureContext::new(failure.clone(), Pid::from_raw(11), Pid::from_raw(12));
        let second = FailureContext::new(failure.clone(), Pid::from_raw(21), Pid::from_raw(22));
        let first_worker = std::thread::spawn(move || {
            first.publish(
                "first worker",
                Error::GuestClock("first typed cause".to_owned()),
            )
        });
        let reached = selected_receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_ok();
        let (attempted, attempted_receiver) = std::sync::mpsc::channel();
        let (finished, finished_receiver) = std::sync::mpsc::channel();
        let second_worker = std::thread::spawn(move || {
            attempted.send(()).unwrap();
            let result = second.publish(
                "second cleanup",
                Error::HostIo(std::io::Error::other("second typed cause")),
            );
            finished.send(()).unwrap();
            result
        });
        let attempted = attempted_receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_ok();
        let second_waited = finished_receiver
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err();
        let no_event = global.events.lock().unwrap().is_empty();
        let unpublished = failure.published_primary().is_none();
        // Release and reap both publishers before asserting any precondition.
        let _ = release.send(());
        let first_result = first_worker.join().unwrap();
        let second_result = second_worker.join().unwrap();
        assert!(reached && attempted && second_waited && no_event && unpublished);
        let events = global.events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            BackendFailure {
                pid: Pid::from_raw(11),
                tid: Pid::from_raw(12),
                phase: "first worker",
            }
        );
        assert_eq!(
            events[1],
            BackendFailure {
                pid: Pid::from_raw(21),
                tid: Pid::from_raw(22),
                phase: "second cleanup",
            }
        );
        assert_eq!(
            failure.primary.lock().unwrap().as_ref().unwrap().1,
            events[0]
        );
        assert!(
            matches!(first_result.primary(), Error::GuestClock(message) if message == "first typed cause")
        );
        assert!(
            matches!(failure.primary().unwrap().primary(), Error::GuestClock(message) if message == "first typed cause")
        );
        assert!(
            matches!(second_result.primary(), Error::HostIo(error) if error.to_string() == "second typed cause")
        );
        assert_eq!(futures::executor::block_on(failure.subscribe()), Ok(()));
    }

    #[test]
    fn refused_host_spawn_returns_exact_initialized_owner() {
        let owner = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = owner.clone();
        // An impossible stack allocation deterministically refuses before a
        // host worker starts. No process limit or host configuration changes.
        let result = spawn_owned(
            std::thread::Builder::new().stack_size(usize::MAX / 2),
            state,
            |state| {
                state.fetch_add(1, Ordering::SeqCst);
            },
        );
        let (error, recovered) = match result {
            Err(failure) => failure,
            Ok(handle) => {
                handle.join().unwrap();
                panic!("impossible host stack unexpectedly spawned");
            }
        };
        assert!(error.raw_os_error().is_some());
        assert!(Arc::ptr_eq(&owner, &recovered));
        assert_eq!(owner.load(Ordering::SeqCst), 0);
        assert_eq!(Arc::strong_count(&owner), 2);
    }
}
