/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

// 500: native public-operation arbitration, not the 496 private poll model.
// Included beside the unchanged 490 native cases to reuse their exact custody.

pub(super) fn after_sync_return_begin(event: &Event, pid: Pid, raw: i32) {
    let hook = event.sync_return_hook.lock().take();
    if let Some(hook) = hook {
        hook.0(pid, raw);
    }
}

struct PublicRegistrationWake {
    original: Weak<Event>,
    retained: Mutex<Option<TerminalCleanup>>,
    reported: AtomicBool,
    done: mpsc::Sender<(u8, u8, bool, Result<(), Errno>)>,
}

impl Wake for PublicRegistrationWake {
    fn wake(self: Arc<Self>) {
        let event = self.original.upgrade().unwrap();
        let before = event.wait_owner.load(Ordering::Acquire);
        // Retain the original public cleanup owner across registry retirement.
        // A fresh Running::new here could no longer discover that generation.
        let retained = self.retained.lock();
        let (same, result) = match retained.as_ref() {
            Some(cleanup) => (
                Arc::ptr_eq(cleanup.event.event(), &event),
                cleanup.ensure_registered(),
            ),
            None => (false, Err(Errno::ENODATA)),
        };
        let after = event.wait_owner.load(Ordering::Acquire);
        eprintln!(
            "510 PUBLIC WAKE: owner={before}->{after}, same_original={same}, public registration={result:?}"
        );
        // Rust permits repeated and late callbacks. Record the required first
        // post-SYNC callback independently of later retirement notifications.
        if !self.reported.swap(true, Ordering::AcqRel) {
            let _ = self.done.send((before, after, same, result));
        }
    }
}

fn release_public_callback(
    event: Arc<Event>,
    callback: Arc<PublicRegistrationWake>,
    deadline: Instant,
) {
    // Only after real child/notifier cleanup and after dropping every returned
    // state/future. Keep the original identity through the live callback.
    assert_eq!(event.worker_state.load(Ordering::Acquire), WORKER_DONE);
    event.status_waker.register(&futures::task::noop_waker());
    let retained = callback.retained.lock().take().unwrap();
    assert!(Arc::ptr_eq(retained.event.event(), &event));
    let identity = Arc::downgrade(retained.event.identity().unwrap());
    let original = Arc::downgrade(&event);
    let weak_callback = Arc::downgrade(&callback);
    drop(retained);
    drop(callback);
    drop(event);
    // Worker DONE can precede its final stack destructors. This check uses
    // the same original total cleanup deadline, not an additional allowance.
    while original.strong_count() != 0
        || identity.strong_count() != 0
        || weak_callback.strong_count() != 0
    {
        assert!(
            Instant::now() <= deadline,
            "post-cleanup ownership release bound"
        );
        thread::yield_now();
    }
    assert!(Instant::now() <= deadline, "total ownership cleanup bound");
    eprintln!("514 POST-CLEANUP RELEASE: original Event=0, identity=0, callback=0 strong owners");
}

fn public_poll_during_sync(returning: bool, current_alias: bool) {
    let (running, mut custody, ptracer) = child();
    let event = Arc::clone(custody.terminal.event.event());
    let pid = running.pid();
    let expected_owner = if returning {
        WAIT_OWNER_SYNC_RETURNING
    } else {
        WAIT_OWNER_SYNC
    };
    let (wake_tx, wake_rx) = mpsc::channel();
    let callback = Arc::new(PublicRegistrationWake {
        original: Arc::downgrade(&event),
        retained: Mutex::new(None),
        reported: AtomicBool::new(false),
        done: wake_tx,
    });
    let (joined_tx, joined_rx) = mpsc::channel();
    let observed_event = Arc::clone(&event);
    let observed_callback = Arc::clone(&callback);
    let hook = SyncConsumedHook(Box::new(move |actual_pid, raw| {
        assert_eq!(actual_pid, pid);
        assert_eq!(raw, (libc::SIGSTOP << 8) | 0x7f);
        assert_eq!(gettid(), ptracer);
        assert_eq!(
            observed_event.wait_owner.load(Ordering::Acquire),
            expected_owner
        );
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        *observed_event.notifier_wait_owner_entered.lock() = Some(entered_tx);
        let waker = Waker::from(Arc::clone(&observed_callback));
        let (polled_tx, polled_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            // Running::new and wait_owned are public. Registration must adopt
            // the genuine registry authority and arbitrate before FIFO poll.
            let mut future = if current_alias {
                // The hook observed this actual SIGSTOP. This public checked
                // identity constructor joins its registered original Event.
                Stopped::try_new_current_unchecked(pid)
                    .unwrap()
                    .wait_owned()
            } else {
                Running::new(pid).wait_owned()
            };
            let result = Pin::new(&mut future).poll(&mut Context::from_waker(&waker));
            polled_tx.send((future, result)).unwrap();
        });
        // Transfer join custody before assertions that can unwind. The hook
        // never waits indefinitely; timeout unwinds the actual SYNC owner.
        joined_tx.send((waiter, polled_rx)).unwrap();
        assert_eq!(
            entered_rx
                .recv_timeout(CLEANUP)
                .expect("public poll reached actual arbitration"),
            expected_owner
        );
        assert!(observed_event.status_waker.waker.lock().is_none());
        let state = observed_event.status.lock();
        assert_eq!(state.reserved, returning);
        assert!(!state.reservation_waiter);
        assert!(!observed_callback.reported.load(Ordering::Acquire));
        eprintln!(
            "500 PUBLIC CONTENDER BLOCKED: owner={expected_owner}, reserved={}, reservation_waiter=false, status_waker=None; actual owner-condition wait observed",
            state.reserved
        );
    }));
    if returning {
        *event.sync_return_hook.lock() = Some(hook);
    } else {
        *event.sync_consumed_hook.lock() = Some(hook);
    }
    let first = stopped(sync_wait(running, &custody), &custody);
    let (waiter, polled_rx) = joined_rx.recv_timeout(COMPONENT).unwrap();
    let outcome = polled_rx.recv_timeout(COMPONENT);
    if outcome.is_err() {
        // Preserve the handoff failure, but release the actual outstanding
        // public registration before exact-pidfd cancellation. Registering
        // through the original custody handle can start its real worker.
        let start = Instant::now();
        let deadline = start + CLEANUP;
        let retained = first.terminal_cleanup();
        let registration = retained.ensure_registered();
        *callback.retained.lock() = Some(retained);
        let released = polled_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        if registration.is_err() || released.is_err() {
            eprintln!(
                "500 failed handoff cleanup: registration={registration:?}, released={}, elapsed={:?}",
                released.is_ok(),
                start.elapsed()
            );
            std::process::abort();
        }
        waiter.join().unwrap();
        event.status_waker.register(&futures::task::noop_waker());
        let cleaned = custody.cleanup(deadline);
        eprintln!(
            "500 failed handoff retained: outcome={:?}; original cleanup={cleaned:?}, total_elapsed={:?}, original bound={CLEANUP:?}",
            outcome.as_ref().err(),
            start.elapsed()
        );
        if cleaned.is_err() || Instant::now() > deadline {
            std::process::abort();
        }
        custody.done = true;
        drop(released);
        drop(first);
        drop(custody);
        release_public_callback(event, callback, deadline);
        assert!(
            outcome.is_ok(),
            "public handoff exceeded unchanged component bound, after exact cleanup"
        );
        unreachable!();
    }
    let (pending, polled) = outcome.unwrap();
    waiter.join().unwrap();
    assert!(
        polled.is_pending(),
        "old stop returned only to the SYNC caller"
    );
    assert_eq!(
        event.wait_owner.load(Ordering::Acquire),
        WAIT_OWNER_NOTIFIER
    );
    assert_eq!(event.worker_state.load(Ordering::Acquire), WORKER_RUNNING);
    assert!(!event.status.lock().reservation_waiter);
    assert!(
        event
            .status_waker
            .waker
            .lock()
            .as_ref()
            .unwrap()
            .will_wake(&Waker::from(Arc::clone(&callback)))
    );
    *callback.retained.lock() = Some(first.terminal_cleanup());
    authenticate(&first, ptracer);
    drop(pending);
    // A subsequent public synchronous wait joins this committed notifier.
    // The second actual SIGSTOP cannot create a new SYNC owner.
    let resumed = first.resume_retaining(None).unwrap();
    let callback_result = wake_rx
        .recv_timeout(COMPONENT)
        .expect("actual publication callback returned");
    let second = stopped(sync_wait(resumed, &custody), &custody);
    assert_eq!(
        event.wait_owner.load(Ordering::Acquire),
        WAIT_OWNER_NOTIFIER
    );
    authenticate(&second, ptracer);
    let cleanup_deadline = Instant::now() + CLEANUP;
    custody.finish();
    drop(second);
    drop(custody);
    release_public_callback(event, callback, cleanup_deadline);
    let (before, after, same, registration) = callback_result;
    assert_eq!(before, WAIT_OWNER_NOTIFIER);
    assert_eq!(after, WAIT_OWNER_NOTIFIER);
    assert!(same);
    assert_eq!(registration, Ok(()));
    eprintln!(
        "500 COMPLETE: genuine first/second stop authentication, public arbitration and public callback succeeded; original custody cleanup complete"
    );
}

#[test]
fn native_public_poll_waits_for_sync_owner() {
    public_poll_during_sync(false, false);
}

#[test]
fn native_public_poll_waits_for_sync_return_owner() {
    public_poll_during_sync(true, false);
}

#[test]
fn native_public_current_alias_waits_for_sync_owner() {
    public_poll_during_sync(false, true);
}

#[test]
fn native_public_current_alias_waits_for_sync_return_owner() {
    public_poll_during_sync(true, true);
}
