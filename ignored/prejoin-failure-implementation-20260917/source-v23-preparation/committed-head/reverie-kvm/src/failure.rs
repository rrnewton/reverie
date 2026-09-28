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

    #[cfg(test)]
    pub(crate) fn published_primary(&self) -> Option<Arc<Error>> {
        self.published
            .load(Ordering::Acquire)
            .then(|| self.primary())
            .flatten()
    }

    fn publish(&self, event: BackendFailure, error: Error) -> Error {
        if matches!(error.primary(), Error::RunAborted) {
            // A Tool subscriber may already be doing terminal cleanup while
            // the synchronous publisher is still returning. Do not sample its
            // unpublished primary or turn this cleanup marker into success.
            // Public completion attaches the retained cause after owned joins.
            return error;
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

    /// Called only after the run's owned workers and processes have returned.
    /// Terminal cleanup markers do not publish a second cause; retain the
    /// original typed cause here once its publisher has completed.
    pub(crate) fn complete<R>(&self, result: crate::Result<R>) -> crate::Result<R> {
        let first = self
            .primary
            .lock()
            .expect("KVM failure lock poisoned")
            .as_ref()
            .map(|(error, event)| (error.clone(), event.tid.as_raw()));
        match (first, result) {
            (Some((primary, tid)), Err(error)) => Err(error.complete_after_failure(primary, tid)),
            (Some((primary, _)), Ok(_)) => Err(Error::SharedFailure(primary)),
            (None, result) => result,
        }
    }
}

#[derive(Clone)]
pub(crate) struct FailureContext {
    pub(crate) run: Arc<RunFailure>,
    process: Arc<ProcessFailure>,
    pid: Pid,
    tid: Pid,
}

struct ProcessFailure {
    sender: Mutex<Option<oneshot::Sender<()>>>,
    receiver: FailureSubscription,
}

impl ProcessFailure {
    fn new() -> Arc<Self> {
        let (sender, receiver) = oneshot::channel();
        Arc::new(Self {
            sender: Mutex::new(Some(sender)),
            receiver: receiver.shared(),
        })
    }

    fn publish(&self) {
        if let Some(sender) = self
            .sender
            .lock()
            .expect("KVM process failure sender lock poisoned")
            .take()
        {
            let _ = sender.send(());
        }
    }
}

impl FailureContext {
    pub(crate) fn new(run: Arc<RunFailure>, pid: Pid, tid: Pid) -> Self {
        Self {
            run,
            process: ProcessFailure::new(),
            pid,
            tid,
        }
    }

    pub(crate) fn for_process(&self, pid: Pid) -> Self {
        Self::new(self.run.clone(), pid, pid)
    }

    pub(crate) fn for_thread(&self, tid: Pid) -> Self {
        Self {
            tid,
            ..self.clone()
        }
    }

    /// The initial owner must return every run failure. An independent process
    /// can finish ordinary work after another process fails, unless its Tool
    /// explicitly terminates the shared global state. Its RPCs still observe
    /// the run-wide notification at the actual request boundary.
    pub(crate) fn driver_subscription(&self, is_traced_tree_root: bool) -> FailureSubscription {
        if is_traced_tree_root {
            self.run.subscribe()
        } else {
            self.process.receiver.clone()
        }
    }

    pub(crate) fn publish(&self, phase: &'static str, error: Error) -> Error {
        let real_failure = !matches!(error.primary(), Error::RunAborted);
        let error = self.run.publish(
            BackendFailure {
                pid: self.pid,
                tid: self.tid,
                phase,
            },
            error,
        );
        // The synchronous Tool terminal transition has returned before either
        // a process peer or an owned join can begin failure cleanup. A derived
        // cancellation marker must not create a new process failure.
        if real_failure {
            self.process.publish();
        }
        error
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
    fn completion_promotes_joined_worker_cause_without_duplicate_diagnostic() {
        let global = Arc::new(());
        let failure = RunFailure::new(&global);
        let event = BackendFailure {
            pid: Pid::from_raw(1),
            tid: Pid::from_raw(2),
            phase: "worker exit",
        };
        let published =
            failure.publish(event, Error::Reverie(reverie::syscalls::Errno::EIO.into()));
        let first = failure.primary().unwrap();
        // This is the exact production shape observed in the original
        // static_elf mode 1: the parked owner sees RunAborted while its physical
        // join returns the already-published worker failure.
        let joined = Error::RunAborted.with_cleanup(vec![Error::WorkerFailure {
            tid: 2,
            error: Arc::new(published),
        }]);
        assert!(
            !joined.retains_primary(&first),
            "publication predicate stays on the primary chain"
        );
        let error = failure.complete::<()>(Err(joined)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "unexpected vCPU exit: KVM worker cleanup failed: thread 2: Reverie tool failed: -5 EIO (I/O error)"
        );
        assert!(error.retains_primary(&first));
        assert_eq!(error.worker_tid(), Some(2));
        assert!(std::ptr::eq(error.primary(), first.primary()));
        assert_eq!(failure.primary.lock().unwrap().as_ref().unwrap().1, event);

        let no_failure = RunFailure::new(&global);
        assert!(matches!(
            no_failure.complete::<()>(Err(Error::RunAborted)),
            Err(Error::RunAborted)
        ));

        // A canceled peer can return the root's published cause through its
        // own join handle. Its TID is propagation context, not the origin.
        let root_failure = RunFailure::new(&global);
        root_failure.publish(
            BackendFailure {
                pid: Pid::from_raw(1),
                tid: Pid::from_raw(1),
                phase: "root execution",
            },
            Error::InvalidGuestPid(-17),
        );
        let root_cause = root_failure.primary().unwrap();
        for direct_alias in [true, false] {
            let peer = Error::WorkerFailure {
                tid: 2,
                error: root_cause.clone(),
            };
            let result = if direct_alias {
                Error::SharedFailure(root_cause.clone()).with_cleanup(vec![peer])
            } else {
                peer
            };
            let error = root_failure.complete::<()>(Err(result)).unwrap_err();
            assert_eq!(error.to_string(), "invalid KVM root guest PID -17");
            assert_eq!(
                error.worker_tid(),
                None,
                "canceled peer replaced the root identity"
            );
            assert!(error.retains_primary(&root_cause));
            assert!(std::ptr::eq(error.primary(), root_cause.primary()));
        }
    }

    #[test]
    fn completion_keeps_distinct_shared_cleanup_causes_and_first_worker_identity() {
        fn references(error: &Error, target: &Arc<Error>) -> usize {
            fn shared(error: &Arc<Error>, target: &Arc<Error>) -> usize {
                if Arc::ptr_eq(error, target) {
                    1
                } else {
                    references(error, target)
                }
            }
            match error {
                Error::SharedFailure(error)
                | Error::WorkerFailure { error, .. }
                | Error::Cleanup { error, .. } => shared(error, target),
                Error::WithCleanup { primary, cleanup } => {
                    shared(primary, target)
                        + cleanup
                            .iter()
                            .map(|error| shared(error, target))
                            .sum::<usize>()
                }
                Error::ExecWorkerTeardown(error) => references(error, target),
                _ => 0,
            }
        }
        let global = Arc::new(());
        let failure = RunFailure::new(&global);
        let event = BackendFailure {
            pid: Pid::from_raw(1),
            tid: Pid::from_raw(9),
            phase: "worker execution",
        };
        failure.publish(event, Error::Reverie(reverie::syscalls::Errno::EIO.into()));
        let first = failure.primary().unwrap();
        // Equal diagnostic text is deliberately a different real cause.
        let lower_tid = Arc::new(Error::Reverie(reverie::syscalls::Errno::EIO.into()));
        let first_hook = Arc::new(Error::HostIo(std::io::Error::from_raw_os_error(
            libc::ENOSPC,
        )));
        let second_hook = Arc::new(Error::Reverie(reverie::syscalls::Errno::EACCES.into()));
        let cancelled_hook = Arc::new(Error::HostIo(std::io::Error::from_raw_os_error(
            libc::EPIPE,
        )));
        let worker = |error| Error::WorkerFailure { tid: 9, error };
        let aggregate = |primary, cleanup| Arc::new(Error::WithCleanup { primary, cleanup });
        let joined = Error::WorkerFailure {
            tid: 2,
            error: lower_tid.clone(),
        }
        .with_cleanup(vec![
            Error::RunAborted
                .with_cleanup(vec![
                    worker(aggregate(first.clone(), vec![first_hook.clone()])),
                    Error::ExecWorkerTeardown(Box::new(worker(aggregate(
                        first.clone(),
                        vec![second_hook.clone()],
                    )))),
                    Error::WorkerFailure {
                        tid: 10,
                        error: aggregate(Arc::new(Error::RunAborted), vec![cancelled_hook.clone()]),
                    },
                    Error::SharedFailure(first.clone()),
                ])
                .cleanup("owner cleanup"),
        ]);
        assert!(!joined.retains_primary(&first));
        let error = failure.complete::<()>(Err(joined)).unwrap_err();
        assert!(error.retains_primary(&first));
        assert!(std::ptr::eq(error.primary(), first.primary()));
        assert_eq!(
            error.worker_tid(),
            Some(9),
            "lower TID is not the first published cause"
        );
        for cause in [
            &first,
            &lower_tid,
            &first_hook,
            &second_hook,
            &cancelled_hook,
        ] {
            assert_eq!(
                references(&error, cause),
                1,
                "typed cause was lost or duplicated: {error:?}"
            );
        }
        assert_eq!(
            error.to_string().matches("EIO").count(),
            2,
            "same text is not cause identity"
        );
        assert!(
            !error
                .to_string()
                .contains("KVM execution stopped after a fatal run failure")
        );
        assert!(
            error.to_string().contains("thread 10:"),
            "cancelled worker's real hook context was lost"
        );
        assert_eq!(failure.primary.lock().unwrap().as_ref().unwrap().1, event);
    }

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
    fn process_failure_subscriptions_preserve_fork_and_thread_ownership() {
        let global = Arc::new(());
        let run = RunFailure::new(&global);
        let root = FailureContext::new(run.clone(), Pid::from_raw(71), Pid::from_raw(71));
        let child = root.for_process(Pid::from_raw(72));
        let worker = child.for_thread(Pid::from_raw(73));
        let nested = child.for_process(Pid::from_raw(74));
        let independent_process = false;
        assert!(
            child
                .driver_subscription(independent_process)
                .now_or_never()
                .is_none()
        );
        assert!(
            worker
                .driver_subscription(independent_process)
                .now_or_never()
                .is_none()
        );
        assert!(root.driver_subscription(true).now_or_never().is_none());

        worker.publish("worker", Error::InvalidGuestPid(-17));
        assert_eq!(
            child
                .driver_subscription(independent_process)
                .now_or_never(),
            Some(Ok(()))
        );
        assert_eq!(
            worker
                .driver_subscription(independent_process)
                .now_or_never(),
            Some(Ok(()))
        );
        assert_eq!(root.driver_subscription(true).now_or_never(), Some(Ok(())));
        assert!(nested.driver_subscription(false).now_or_never().is_none());
        assert!(matches!(
            run.primary().unwrap().primary(),
            Error::InvalidGuestPid(-17)
        ));
        assert_eq!(
            run.primary.lock().unwrap().as_ref().unwrap().1.tid,
            Pid::from_raw(73)
        );

        // A derived cleanup marker does not mark a healthy independent process
        // as a second source of failure or wake its ordinary execution driver.
        nested.publish("RPC cancellation", Error::RunAborted);
        assert!(nested.driver_subscription(false).now_or_never().is_none());
        let first = run.primary().unwrap();
        nested.publish(
            "later process failure",
            Error::HostIo(std::io::Error::from_raw_os_error(libc::EPIPE)),
        );
        assert_eq!(
            nested.driver_subscription(false).now_or_never(),
            Some(Ok(()))
        );
        assert!(Arc::ptr_eq(&first, &run.primary().unwrap()));
        let later = child.for_process(Pid::from_raw(75));
        assert!(later.driver_subscription(false).now_or_never().is_none());
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
        let mut process_subscription = context.driver_subscription(false);
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
        let process_pending = std::pin::Pin::new(&mut process_subscription)
            .poll(&mut Context::from_waker(&noop_waker()))
            .is_pending();
        release.send(()).unwrap();
        publisher.join().unwrap();
        assert!(
            entered && pending && unpublished,
            "local cleanup escaped before Tool publication completed"
        );
        assert_eq!(futures::executor::block_on(subscription), Ok(()));
        assert!(
            process_pending,
            "process cleanup escaped before Tool publication completed"
        );
        assert_eq!(futures::executor::block_on(process_subscription), Ok(()));
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
