/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

// A real Event/condvar transition control, not a kernel-status receipt.
mod worker_retirement_tests {
    use super::*;

    struct WorkerWakeProbe {
        event: Arc<Event>,
        deadline: Instant,
        publication: mpsc::Sender<()>,
        waiter_completion: Mutex<mpsc::Receiver<bool>>,
        waiter_completed: AtomicBool,
        lock_was_free: AtomicBool,
        calls: AtomicUsize,
    }

    impl std::task::Wake for WorkerWakeProbe {
        fn wake(self: Arc<Self>) {
            std::task::Wake::wake_by_ref(&self);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            // Publication has happened even in the unlocked-publisher mutant.
            // Let the owner release its paused waiter before awaiting return.
            let _ = self.publication.send(());
            let completed = matches!(
                self.waiter_completion
                    .lock()
                    .recv_timeout(self.deadline.saturating_duration_since(Instant::now())),
                Ok(true)
            );
            self.waiter_completed.store(completed, Ordering::Release);
            if completed {
                // The receipt is sent only after wait_worker_done returns and
                // releases its guard. Its reacquisition cannot race this probe.
                self.lock_was_free.store(
                    self.event.worker_done_lock.try_lock().is_some(),
                    Ordering::Release,
                );
            }
            self.calls.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn actual_worker_done_publication_serializes_with_check_to_wait() {
        let deadline = Instant::now() + Duration::from_secs(3);
        let event = Arc::new(Event::new());
        assert!(event.try_begin_worker_start());
        event.mark_worker_running();
        let (publication, publication_wait) = mpsc::channel();
        let (waiter_returned, waiter_completion) = mpsc::channel();
        let probe = Arc::new(WorkerWakeProbe {
            event: Arc::clone(&event),
            deadline,
            publication: publication.clone(),
            waiter_completion: Mutex::new(waiter_completion),
            waiter_completed: AtomicBool::new(false),
            lock_was_free: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        });
        let waiter_registration = Arc::new(ExitWaiter {
            waker: WakerSlot::default(),
            epoch: event.exit_epoch.load(Ordering::Acquire),
        });
        event
            .worker_done_waiters
            .register(&waiter_registration, &Waker::from(Arc::clone(&probe)));
        let (checked, checked_wait) = mpsc::sync_channel(1);
        let (release, release_wait) = mpsc::channel();
        *event.worker_done_wait_pause.lock() = Some(BoundedTestPause {
            captured: checked,
            resume: release_wait,
        });
        // The first receipt comes from either an actual contended lock attempt
        // (fixed publisher), or the waker after publication (unlocked publisher).
        // Both paths are events; no sleep or timed absence establishes order.
        *event.worker_done_lock_contended.lock() = Some(publication.clone());
        let waiting_event = Arc::clone(&event);
        let waiting = thread::spawn(move || {
            let waited =
                waiting_event.wait_worker_done(deadline.saturating_duration_since(Instant::now()));
            let _ = waiter_returned.send(waited);
            waited
        });
        let checked_result =
            checked_wait.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let publishing_event = Arc::clone(&event);
        let publishing = thread::spawn(move || {
            publishing_event.mark_worker_done();
            let _ = publication.send(());
        });
        let publication_result =
            publication_wait.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let state_before_release = event.worker_state.load(Ordering::Acquire);
        // Release on all result paths BEFORE either join or any assertion.
        // A failed/mutant run must not strand a publisher behind the waiter.
        drop(release);
        let waited = waiting.join();
        let published = publishing.join();
        checked_result.expect("waiter did not reach its actual predicate-to-wait gap");
        publication_result.expect("publisher neither attempted its lock nor completed");
        assert_eq!(
            state_before_release, WORKER_RUNNING,
            "WORKER_DONE was published while the waiter held worker_done_lock"
        );
        assert!(
            waited.unwrap(),
            "actual condvar waiter did not observe retirement"
        );
        published.unwrap();
        assert_eq!(event.worker_state.load(Ordering::Acquire), WORKER_DONE);
        assert_eq!(probe.calls.load(Ordering::Acquire), 1);
        assert!(
            probe.waiter_completed.load(Ordering::Acquire),
            "async waker did not receive the actual waiter completion before the original deadline"
        );
        assert!(
            probe.lock_was_free.load(Ordering::Acquire),
            "async waker ran with worker_done_lock held"
        );
        assert!(
            Instant::now() < deadline,
            "worker retirement exhausted the original three-second component bound"
        );
    }
}
