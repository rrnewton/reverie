/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Actual Exec wait/group ownership with a constructed callback and host
//! worker. KVM setup is required; no guest instruction or KVM_RUN is used.

use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use super::*;

const WAIT: Duration = Duration::from_secs(5);

struct WakeCount(AtomicUsize);

impl futures::task::ArcWake for WakeCount {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct Callback<'a> {
    wait: Pin<Box<dyn Future<Output = Result<()>> + 'a>>,
    destroyed: Arc<AtomicBool>,
}

impl Future for Callback<'_> {
    type Output = Result<()>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.wait.as_mut().poll(cx)
    }
}

impl Drop for Callback<'_> {
    fn drop(&mut self) {
        self.destroyed.store(true, Ordering::Release);
    }
}

struct WorkerDone(mpsc::Sender<()>);

impl Drop for WorkerDone {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

struct Rescue {
    group: Arc<GuestThreadGroup>,
    release: Option<mpsc::Sender<()>>,
    done: mpsc::Receiver<()>,
    finished: bool,
    joined: bool,
}

impl Drop for Rescue {
    fn drop(&mut self) {
        if self.joined {
            return;
        }
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if self.finished || self.done.recv_timeout(WAIT).is_ok() {
            self.group.join_workers();
        }
    }
}

#[test]
fn exec_wait_private_failure_leaves_worker_owned_until_callback_destruction() {
    let mut backend = KvmBackend::new(0x10000).expect("Exec wait control requires /dev/kvm");
    let global = Arc::new(());
    let run = crate::failure::RunFailure::new(&global);
    backend.set_tool_failure(Some(crate::failure::FailureContext::new(
        run.clone(),
        Pid::from_raw(1),
        Pid::from_raw(1),
    )));
    let driver = backend.start_entry_driver();
    let callback_scope = backend.begin_entry_callback().unwrap().unwrap();
    let origin = callback_scope.origin();
    let memory = backend.memory.clone();
    let group = backend.thread_group.clone();
    let destroyed = Arc::new(AtomicBool::new(false));
    let worker_destroyed = destroyed.clone();
    let worker_run = run.clone();
    let (start, wait_start) = mpsc::channel();
    let gate = ChildStartGate::new(start);
    let (cancelled, wait_cancelled) = mpsc::channel();
    let (release, wait_release) = mpsc::channel();
    let (done, wait_done) = mpsc::channel();
    let notice = WorkerCompletionNotice::new(group.clone());
    let returning = notice.returning.clone();
    let handle = std::thread::spawn(move || {
        let _done = WorkerDone(done);
        let _notice = notice;
        (move || -> GuestWorkerResult {
            assert_eq!(
                wait_start.recv_timeout(WAIT).unwrap(),
                ChildStartCommand::Cancel
            );
            cancelled.send(()).unwrap();
            // A regressed blocking join is finite too: this receive times out
            // and fails the worker rather than stranding the control process.
            wait_release.recv_timeout(WAIT).unwrap();
            assert!(worker_destroyed.load(Ordering::Acquire));
            assert!(worker_run.published_primary().is_some());
            Err(Error::RunAborted)
        })()
    });
    group.add_worker_handle_with_completion(2, Some(gate.clone()), Some(returning.clone()), handle);
    let mut rescue = Rescue {
        group: group.clone(),
        release: Some(release),
        done: wait_done,
        finished: false,
        joined: false,
    };
    let wakes = Arc::new(WakeCount(AtomicUsize::new(0)));
    let waker = futures::task::waker(wakes.clone());
    let mut cx = Context::from_waker(&waker);
    let mut stop = Box::pin(std::future::pending());
    let mut callback = Box::pin(Callback {
        wait: Box::pin(backend.cancel_guest_threads_for_exec(stop.as_mut())),
        destroyed: destroyed.clone(),
    });
    assert!(callback.as_mut().poll(&mut cx).is_pending());
    wait_cancelled
        .recv_timeout(WAIT)
        .expect("Exec did not request worker cancellation");
    assert!(gate.is_cancelled());
    assert_eq!(group.worker_handles.lock().unwrap().len(), 1);
    assert!(!returning.load(Ordering::Acquire));
    assert!(run.published_primary().is_none());

    let cause = Arc::new(Error::GuestClock(
        "private failure during Exec wait".to_owned(),
    ));
    let before = wakes.0.load(Ordering::SeqCst);
    memory
        .entry_gate()
        .poison(memory.entry_origin(), Error::SharedFailure(cause.clone()));
    assert!(wakes.0.load(Ordering::SeqCst) > before);
    let error = match callback.as_mut().poll(&mut cx) {
        Poll::Ready(Err(error)) => error,
        _ => panic!("private cause must interrupt the pending Exec callback"),
    };
    assert!(crate::failure::references_shared_error(&error, &cause));
    assert!(!destroyed.load(Ordering::Acquire));
    assert!(!origin.callback_dropped());
    assert!(run.published_primary().is_none());
    assert_eq!(group.worker_handles.lock().unwrap().len(), 1);
    assert!(group.worker_errors.lock().unwrap().is_empty());
    assert!(!returning.load(Ordering::Acquire));

    drop(callback);
    drop(callback_scope);
    backend.restore_entry_origin();
    assert!(destroyed.load(Ordering::Acquire));
    assert!(origin.callback_dropped());
    let error =
        futures::executor::block_on(backend.route_entry_outcome::<()>(Err(error))).unwrap_err();
    let error = backend.report_tool_failure("constructed Exec callback", error);
    assert!(run.published_primary().is_some());
    let mut completion = Box::pin(group.subscribe_worker_completion());
    assert!(completion.as_mut().poll(&mut cx).is_pending());
    rescue.release.take().unwrap().send(()).unwrap();
    rescue
        .done
        .recv_timeout(WAIT)
        .expect("released worker did not finish");
    rescue.finished = true;
    assert!(completion.as_mut().poll(&mut cx).is_ready());
    assert!(returning.load(Ordering::Acquire));
    group.join_workers();
    rescue.joined = true;
    assert!(group.worker_handles.lock().unwrap().is_empty());
    assert!(group.worker_start_gates.lock().unwrap().is_empty());
    let completed = backend
        .finish_entry_driver::<()>(driver, Err(error))
        .unwrap_err();
    assert!(crate::failure::references_shared_error(&completed, &cause));
}

struct TlsExitProbe {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    done: mpsc::Sender<()>,
    timed_out: Arc<AtomicBool>,
    callback_destroyed: Arc<AtomicBool>,
    run: Arc<crate::failure::RunFailure>,
    ordering_satisfied: Arc<AtomicBool>,
}

impl Drop for TlsExitProbe {
    fn drop(&mut self) {
        // A failed assertion in a TLS destructor aborts the process. Report
        // observations to the parent instead, and bound a regressed join.
        let _ = self.entered.send(());
        if self.release.recv_timeout(WAIT).is_err() {
            self.timed_out.store(true, Ordering::Release);
        }
        self.ordering_satisfied.store(
            self.callback_destroyed.load(Ordering::Acquire)
                && self.run.published_primary().is_some(),
            Ordering::Release,
        );
        let _ = self.done.send(());
    }
}

thread_local! {
    static TLS_EXIT_PROBE: std::cell::RefCell<Option<TlsExitProbe>> = const {
        std::cell::RefCell::new(None)
    };
}

#[test]
fn exec_wait_private_failure_interrupts_worker_tls_destructor() {
    let mut backend = KvmBackend::new(0x10000).expect("Exec TLS control requires /dev/kvm");
    let global = Arc::new(());
    let run = crate::failure::RunFailure::new(&global);
    backend.set_tool_failure(Some(crate::failure::FailureContext::new(
        run.clone(),
        Pid::from_raw(1),
        Pid::from_raw(1),
    )));
    let driver = backend.start_entry_driver();
    let callback_scope = backend.begin_entry_callback().unwrap().unwrap();
    let origin = callback_scope.origin();
    let memory = backend.memory.clone();
    let group = backend.thread_group.clone();
    let destroyed = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    let ordering_satisfied = Arc::new(AtomicBool::new(false));
    let worker_cause = Arc::new(Error::GuestClock("worker returned before TLS exit".into()));
    let retained_cause = worker_cause.clone();
    let (entered, wait_entered) = mpsc::channel();
    let (release, wait_release) = mpsc::channel();
    let (done, wait_done) = mpsc::channel();
    let probe = TlsExitProbe {
        entered,
        release: wait_release,
        done,
        timed_out: timed_out.clone(),
        callback_destroyed: destroyed.clone(),
        run: run.clone(),
        ordering_satisfied: ordering_satisfied.clone(),
    };
    let handle = std::thread::spawn(move || -> GuestWorkerResult {
        TLS_EXIT_PROBE.with(|slot| *slot.borrow_mut() = Some(probe));
        Err(Error::SharedFailure(retained_cause))
    });
    wait_entered
        .recv_timeout(WAIT)
        .expect("worker did not enter its real TLS destructor");
    // This is the distinction the old closure-only control could not make.
    assert!(handle.is_finished());
    assert!(!timed_out.load(Ordering::Acquire));
    group.add_worker_handle(2, handle);
    let mut rescue = Rescue {
        group: group.clone(),
        release: Some(release),
        done: wait_done,
        finished: false,
        joined: false,
    };
    let wakes = Arc::new(WakeCount(AtomicUsize::new(0)));
    let waker = futures::task::waker(wakes.clone());
    let mut cx = Context::from_waker(&waker);
    let mut stop = Box::pin(std::future::pending());
    let mut callback = Box::pin(Callback {
        wait: Box::pin(backend.cancel_guest_threads_for_exec(stop.as_mut())),
        destroyed: destroyed.clone(),
    });
    assert!(
        callback.as_mut().poll(&mut cx).is_pending(),
        "Exec must remain interruptible while the worker TLS destructor is blocked"
    );
    assert!(!timed_out.load(Ordering::Acquire));
    assert!(group.worker_errors.lock().unwrap().is_empty());
    let cause = Arc::new(Error::GuestClock("private failure during worker TLS".into()));
    let before = wakes.0.load(Ordering::SeqCst);
    memory
        .entry_gate()
        .poison(memory.entry_origin(), Error::SharedFailure(cause.clone()));
    assert!(wakes.0.load(Ordering::SeqCst) > before);
    let error = match callback.as_mut().poll(&mut cx) {
        Poll::Ready(Err(error)) => error,
        _ => panic!("private cause must interrupt Exec before TLS destruction completes"),
    };
    assert!(crate::failure::references_shared_error(&error, &cause));
    assert!(!destroyed.load(Ordering::Acquire));
    assert!(!origin.callback_dropped());
    assert!(run.published_primary().is_none());
    assert!(rescue.done.try_recv().is_err());
    assert!(group.worker_errors.lock().unwrap().is_empty());

    drop(callback);
    drop(callback_scope);
    backend.restore_entry_origin();
    let error =
        futures::executor::block_on(backend.route_entry_outcome::<()>(Err(error))).unwrap_err();
    let error = backend.report_tool_failure("constructed Exec TLS callback", error);
    rescue.release.take().unwrap().send(()).unwrap();
    rescue.done.recv_timeout(WAIT).expect("TLS did not complete");
    rescue.finished = true;
    group.join_workers();
    rescue.joined = true;
    assert!(!timed_out.load(Ordering::Acquire));
    assert!(ordering_satisfied.load(Ordering::Acquire));
    assert!(group.worker_handles.lock().unwrap().is_empty());
    let worker_error = group.teardown_result().unwrap_err();
    assert!(crate::failure::references_shared_error(
        &worker_error,
        &worker_cause
    ));
    assert_eq!(group.worker_errors.lock().unwrap().get(&2).unwrap().len(), 1);
    group.join_workers();
    assert_eq!(group.worker_errors.lock().unwrap().get(&2).unwrap().len(), 1);
    let completed = backend
        .finish_entry_driver::<()>(driver, Err(error))
        .unwrap_err();
    assert!(crate::failure::references_shared_error(&completed, &cause));
}
