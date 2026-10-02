/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! STATE-MODEL evidence for actual private ownership/reservation APIs and Waker
//! reentry. No tracee, WorkerIdentity, consuming kernel wait or source receipt.
use std::process::Command;
use std::task::Wake;

use super::*;

const COMPONENT: Duration = Duration::from_secs(3);
const CLEANUP: Duration = Duration::from_secs(2);
const STOP: i32 = (libc::SIGSTOP << 8) | 0x7f;

#[derive(Clone, Copy, Debug)]
enum Case {
    SyncRollback,
    SyncCancel,
    SyncCommit,
    SyncUnwind,
    AsyncRollback,
    AsyncCancel,
    AsyncCommit,
    AsyncDied,
}

impl Case {
    fn synchronous(self) -> bool {
        matches!(
            self,
            Self::SyncRollback | Self::SyncCancel | Self::SyncCommit | Self::SyncUnwind
        )
    }

    fn consumes(self) -> bool {
        matches!(self, Self::SyncCommit | Self::AsyncCommit | Self::AsyncDied)
    }
}

#[derive(Debug)]
struct Observation {
    before_owner: u8,
    after_owner: u8,
    reserved: bool,
    pending: usize,
    acquired_new: bool,
}

struct ReenterNotifier {
    event: Weak<Event>,
    original: Arc<StatusEntry>,
    case: Case,
    calls: AtomicUsize,
    observation: Mutex<Option<Observation>>,
}

impl Wake for ReenterNotifier {
    fn wake(self: Arc<Self>) {
        assert_eq!(
            self.calls.fetch_add(1, Ordering::AcqRel),
            0,
            "one pending contender wake"
        );
        let event = self.event.upgrade().unwrap();
        let before_owner = event.wait_owner.load(Ordering::Acquire);
        let (reserved, pending, original_front) = {
            let state = event.status.lock();
            (
                state.reserved,
                state.pending.entries.len(),
                state
                    .pending
                    .entries
                    .front()
                    .is_some_and(|front| Arc::ptr_eq(front, &self.original)),
            )
        };
        eprintln!(
            "496 {:?} RELEASE CALLBACK ENTER: owner={before_owner}, reserved={reserved}, pending={pending}, original_front={original_front}; calling real blocking claim_notifier_wait INLINE",
            self.case
        );
        assert!(!reserved);
        assert_eq!(pending, usize::from(!self.case.consumes()));
        assert_eq!(original_front, !self.case.consumes());
        // No try_lock/try_claim shortcut and no EBUSY acceptance. Before the
        // repair this actual call blocks behind the callback's own SYNC owner.
        let acquired_new = match event.claim_notifier_wait() {
            NotifierWaitOwnership::Claimed(owner) => {
                assert!(event.try_begin_worker_start());
                event.mark_worker_running(); // Model transition, NO worker spawned.
                owner.commit();
                true
            }
            NotifierWaitOwnership::Existing => false,
        };
        let after_owner = event.wait_owner.load(Ordering::Acquire);
        eprintln!(
            "496 {:?} RELEASE CALLBACK RETURN: before_owner={before_owner}, after_owner={after_owner}, acquired_new={acquired_new}, reserved={}, pending={}",
            self.case,
            event.status.lock().reserved,
            pending
        );
        assert_eq!(
            before_owner,
            if self.case.synchronous() {
                WAIT_OWNER_NONE
            } else {
                WAIT_OWNER_NOTIFIER
            }
        );
        assert_eq!(after_owner, WAIT_OWNER_NOTIFIER);
        assert_eq!(acquired_new, self.case.synchronous());
        *self.observation.lock() = Some(Observation {
            before_owner,
            after_owner,
            reserved,
            pending,
            acquired_new,
        });
    }
}

fn arm_contender(event: &Event, callback: &Arc<ReenterNotifier>) {
    assert!(event.status.lock().reserved);
    let waker = Waker::from(Arc::clone(callback));
    assert!(
        event.poll_status_reservation(&waker).is_pending(),
        "real second poll must encounter the reserved FIFO front"
    );
    let state = event.status.lock();
    assert!(state.reservation_waiter);
    assert!(state.reserved);
    assert!(
        state.pending.entries.front().unwrap().receipt.is_none(),
        "synthetic model grants no source authority"
    );
    eprintln!(
        "496 {:?} CONTENDER PENDING: owner={}, reserved=true, reservation_waiter=true",
        callback.case,
        event.wait_owner.load(Ordering::Acquire)
    );
}

fn exercise(case: Case) {
    let event = Arc::new(Event::new());
    event.update(STOP); // Synthetic helper, explicitly no consumption receipt.
    let original = Arc::clone(event.status.lock().pending.entries.front().unwrap());
    assert!(original.receipt.is_none());
    let callback = Arc::new(ReenterNotifier {
        event: Arc::downgrade(&event),
        original: Arc::clone(&original),
        case,
        calls: AtomicUsize::new(0),
        observation: Mutex::new(None),
    });
    if case.synchronous() {
        let mut owner = match event.claim_sync_wait().unwrap() {
            SyncWaitOwnership::Claimed(owner) => owner,
            SyncWaitOwnership::Notifier => panic!("model starts without a notifier"),
        };
        let reservation = event.try_status_reservation_sync().unwrap().unwrap();
        if matches!(case, Case::SyncCancel) {
            arm_contender(&event, &callback);
            assert!(matches!(
                event.try_claim_cancellable_notifier_wait(),
                CancellableNotifierWaitOwnership::Synchronous
            ));
            let result = owner.decode_status_return(
                Pid::from_raw(7),
                reservation,
                |_| -> Result<(), Error> {
                    panic!("cancelled return must not decode");
                },
            );
            assert!(matches!(result, Ok(StatusReturn::Cancelled(STOP))));
        } else if matches!(case, Case::SyncUnwind) {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _: Result<StatusReturn<()>, Error> =
                    owner.decode_status_return(Pid::from_raw(7), reservation, |_| {
                        arm_contender(&event, &callback);
                        panic!("496 deliberately modeled decoder unwind");
                    });
            }));
            assert!(
                result.is_err(),
                "unwind remains an unwind, not a successful decode"
            );
        } else {
            let result = owner.decode_status_return(Pid::from_raw(7), reservation, |_| {
                arm_contender(&event, &callback);
                if matches!(case, Case::SyncRollback) {
                    Err(Errno::EIO.into())
                } else {
                    Ok(())
                }
            });
            if matches!(case, Case::SyncRollback) {
                assert!(matches!(result, Err(Error::Errno(Errno::EIO))));
            } else {
                assert!(matches!(result, Ok(StatusReturn::Returned(()))));
            }
        }
        if !case.consumes() {
            // Cancellation's real caller may still resume/drain its tracee.
            // Do not grant a notifier ownership while that SYNC guard is live.
            assert_eq!(event.wait_owner.load(Ordering::Acquire), WAIT_OWNER_SYNC);
            assert_eq!(callback.calls.load(Ordering::Acquire), 0);
            assert!(!event.status.lock().reserved);
            assert!(Arc::ptr_eq(
                event.status.lock().pending.entries.front().unwrap(),
                &original
            ));
            eprintln!(
                "496 {case:?} OUTER OWNER RETAINED: owner=SYNC, reserved=false, original FIFO retained; releasing owner now"
            );
        }
        drop(owner); // Actual owner-release API, no direct atomic state rewrite.
    } else {
        let owner = match event.claim_notifier_wait() {
            NotifierWaitOwnership::Claimed(owner) => owner,
            NotifierWaitOwnership::Existing => panic!("fresh Event already owns a notifier"),
        };
        assert!(event.try_begin_worker_start());
        event.mark_worker_running(); // Model only; no WorkerIdentity or real worker.
        owner.commit();
        let reservation = event.try_status_reservation_sync().unwrap().unwrap();
        let result = if matches!(case, Case::AsyncCancel) {
            arm_contender(&event, &callback);
            assert!(matches!(
                event.try_claim_cancellable_notifier_wait(),
                CancellableNotifierWaitOwnership::Existing
            ));
            event.decode_status_return(reservation, |_| -> Result<(), Error> {
                panic!("cancelled async return must not decode");
            })
        } else {
            event.decode_status_return(reservation, |_| {
                arm_contender(&event, &callback);
                match case {
                    Case::AsyncRollback => Err(Errno::EIO.into()),
                    Case::AsyncDied => Err(Error::Died(crate::Zombie::from_token(
                        Pid::from_raw(7),
                        TraceeToken::from_event(EventHandle::new()),
                    ))),
                    Case::AsyncCommit => Ok(()),
                    _ => unreachable!(),
                }
            })
        };
        match case {
            Case::AsyncRollback => assert!(matches!(result, Err(Error::Errno(Errno::EIO)))),
            Case::AsyncCancel => assert!(matches!(result, Ok(StatusReturn::Cancelled(STOP)))),
            Case::AsyncDied => assert!(matches!(result, Err(Error::Died(_)))),
            Case::AsyncCommit => assert!(matches!(result, Ok(StatusReturn::Returned(())))),
            _ => unreachable!(),
        }
    }
    assert_eq!(callback.calls.load(Ordering::Acquire), 1);
    let observation = callback
        .observation
        .lock()
        .take()
        .expect("actual reentrant claim completed");
    assert_eq!(
        observation.before_owner,
        if case.synchronous() {
            WAIT_OWNER_NONE
        } else {
            WAIT_OWNER_NOTIFIER
        }
    );
    assert_eq!(observation.after_owner, WAIT_OWNER_NOTIFIER);
    assert!(!observation.reserved);
    assert_eq!(observation.pending, usize::from(!case.consumes()));
    assert_eq!(observation.acquired_new, case.synchronous());
    assert_eq!(
        event.wait_owner.load(Ordering::Acquire),
        WAIT_OWNER_NOTIFIER
    );
    assert_eq!(
        event.status.lock().pending.entries.len(),
        usize::from(!case.consumes())
    );
    eprintln!(
        "496 {case:?} COMPLETE: real reentrant acquisition returned, FIFO identity and return outcome preserved; STATE-MODEL ONLY"
    );
}

fn bounded_case(name: &str, case: Case) {
    let selector = format!("notifier::reservation_wake_tests::{name}");
    if std::env::var("SAFEPTRACE_RESERVATION_WAKE496_CHILD")
        .ok()
        .as_deref()
        == Some(&selector)
    {
        exercise(case);
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &selector, "--nocapture", "--test-threads=1"])
        .env("SAFEPTRACE_RESERVATION_WAKE496_CHILD", &selector)
        .spawn()
        .expect("spawn owned state-model test process");
    let deadline = Instant::now() + COMPONENT;
    let mut failure = None;
    let mut status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(1)),
            Ok(None) => {
                failure =
                    Some("actual reentrant operation exceeded 3-second component bound".to_owned());
                break None;
            }
            Err(error) => {
                failure = Some(format!("owned model wait failed: {error}"));
                break None;
            }
        }
    };
    if status.is_none() {
        let start = Instant::now();
        let killed = child.kill();
        while start.elapsed() <= CLEANUP {
            match child.try_wait() {
                Ok(Some(reaped)) => {
                    status = Some(reaped);
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(1)),
                Err(error) => {
                    eprintln!("496 owned model reap error: {error}");
                    break;
                }
            }
        }
        eprintln!(
            "496 bounded model failure: {failure:?}; owned child kill={killed:?}, actual reap={status:?}, cleanup_elapsed={:?}",
            start.elapsed()
        );
        if status.is_none() || start.elapsed() > CLEANUP {
            eprintln!("496 failed to prove owned model reaping within original 2-second bound");
            std::process::abort();
        }
    }
    let status = status.expect("owned model was reaped");
    assert!(
        failure.is_none() && status.success(),
        "{case:?}: {failure:?}; actual owned child status={status}"
    );
}

#[test]
fn model_sync_rollback_release_waker_reenters_notifier() {
    bounded_case(
        "model_sync_rollback_release_waker_reenters_notifier",
        Case::SyncRollback,
    );
}
#[test]
fn model_sync_cancel_release_waker_reenters_notifier() {
    bounded_case(
        "model_sync_cancel_release_waker_reenters_notifier",
        Case::SyncCancel,
    );
}
#[test]
fn model_sync_commit_release_waker_reenters_notifier() {
    bounded_case(
        "model_sync_commit_release_waker_reenters_notifier",
        Case::SyncCommit,
    );
}
#[test]
fn model_sync_unwind_release_waker_reenters_notifier() {
    bounded_case(
        "model_sync_unwind_release_waker_reenters_notifier",
        Case::SyncUnwind,
    );
}
#[test]
fn model_async_rollback_release_waker_reenters_notifier() {
    bounded_case(
        "model_async_rollback_release_waker_reenters_notifier",
        Case::AsyncRollback,
    );
}
#[test]
fn model_async_cancel_release_waker_reenters_notifier() {
    bounded_case(
        "model_async_cancel_release_waker_reenters_notifier",
        Case::AsyncCancel,
    );
}
#[test]
fn model_async_commit_release_waker_reenters_notifier() {
    bounded_case(
        "model_async_commit_release_waker_reenters_notifier",
        Case::AsyncCommit,
    );
}
#[test]
fn model_async_died_release_waker_reenters_notifier() {
    bounded_case(
        "model_async_died_release_waker_reenters_notifier",
        Case::AsyncDied,
    );
}
