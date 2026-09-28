/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use super::*;
use crate::signal_test_support::OwnedSignalTarget;

fn parked_target(scheduler: &mut Scheduler, target: DetTid) {
    register_known_thread(scheduler, target);
    scheduler.thread_tree.add_child(target, target, true);
    scheduler.next_turns.get_mut(&target).unwrap().req = Ivar::full(Ok(Resources::new(target)));
}

#[tokio::test]
async fn kvm_signal_refusal_precedes_both_host_delivery_paths_and_preserves_first_cause() {
    for with_pidfd in [false, true] {
        for requires_pidfd in [false, true] {
            let mut target = OwnedSignalTarget::new(&[libc::SIGUSR1, libc::SIGCHLD]);
            let tid = DetTid::from_raw(target.pid());
            let config = Config {
                backend_rejects_host_signals: true,
                backend_requires_thread_directed_process_signals: requires_pidfd,
                ..Config::default()
            };
            let mut scheduler = Scheduler::new(&config);
            parked_target(&mut scheduler, tid);
            scheduler.runqueue_push_back(tid);
            scheduler.turn = 17;
            if with_pidfd {
                scheduler.register_physical_thread(tid, MmId::initial(tid), target.pid(), target.pid()).unwrap();
            }
            let request = scheduler.next_turns[&tid].req.try_read().unwrap();
            let before_queue = format!("{:?}", scheduler.run_queue);
            let mut wake = scheduler.backend_failure_waiter();
            assert!(futures::poll!(&mut wake).is_pending());
            target.assert_pending(libc::SIGUSR1, false);
            scheduler.signal_guest(tid, Signal::SIGUSR1);
            let cause = KvmSignalRefusal { signal: libc::SIGUSR1, target: tid, turn: 17 };
            assert_eq!(scheduler.signal_refusal(), Some(cause.clone()));
            assert!(scheduler.backend_failed());
            assert!(scheduler.backend_failure.is_none());
            assert!(scheduler.terminal_deadlock.is_none());
            assert_eq!(scheduler.next_turns[&tid].req.try_read().unwrap(), request);
            assert!(scheduler.next_turns[&tid].resp.try_read().is_none());
            assert_eq!(format!("{:?}", scheduler.run_queue), before_queue);
            assert_eq!(scheduler.turn, 17);
            target.assert_pending(libc::SIGUSR1, false);
            assert!(futures::poll!(&mut wake).is_pending(), "retention precedes publication");
            scheduler.take_failure_notification().unwrap().send(()).unwrap();
            assert!(futures::poll!(&mut wake).is_ready());
            scheduler.signal_guest(tid, Signal::SIGCHLD);
            assert_eq!(scheduler.signal_refusal(), Some(cause));
            assert!(scheduler.take_failure_notification().is_none());
            target.assert_pending(libc::SIGCHLD, false);
        }
    }
}

#[test]
fn kvm_signal_guard_preserves_an_existing_backend_failure() {
    let mut target = OwnedSignalTarget::new(&[libc::SIGUSR1]);
    let tid = DetTid::from_raw(target.pid());
    let config = Config { backend_rejects_host_signals: true, ..Config::default() };
    let mut scheduler = Scheduler::new(&config);
    let failure = reverie::BackendFailure {
        pid: reverie::Tid::from_raw(target.pid()),
        tid: reverie::Tid::from_raw(target.pid()),
        phase: "existing independent backend failure",
    };
    scheduler.report_backend_failure(failure).unwrap().send(()).unwrap();
    scheduler.signal_guest(tid, Signal::SIGUSR1);
    assert_eq!(scheduler.backend_failure.as_ref().unwrap().phase, "existing independent backend failure");
    assert!(scheduler.signal_refusal().is_none());
    assert!(scheduler.take_failure_notification().is_none());
    target.assert_pending(libc::SIGUSR1, false);
}

#[test]
fn native_signal_delivery_still_reaches_owned_raw_and_pidfd_targets() {
    for with_pidfd in [false, true] {
        let mut target = OwnedSignalTarget::new(&[libc::SIGUSR1]);
        let tid = DetTid::from_raw(target.pid());
        let mut scheduler = Scheduler::new(&Config::default());
        parked_target(&mut scheduler, tid);
        scheduler.runqueue_push_back(tid);
        if with_pidfd {
            scheduler.register_physical_thread(tid, MmId::initial(tid), target.pid(), target.pid()).unwrap();
        }
        target.assert_pending(libc::SIGUSR1, false);
        scheduler.signal_guest(tid, Signal::SIGUSR1);
        target.assert_pending(libc::SIGUSR1, true);
        assert!(!scheduler.backend_failed());
        assert!(scheduler.signal_refusal().is_none());
        assert!(scheduler.run_queue.contains_tid(tid));
    }
}

#[tokio::test]
async fn due_kvm_child_signal_stops_ready_io_and_deferred_admissions_without_freezing_time() {
    let mut target = OwnedSignalTarget::new(&[libc::SIGCHLD]);
    let tid = DetTid::from_raw(target.pid());
    let deferred = DetTid::from_raw(13);
    let external = DetTid::from_raw(19);
    let config = Config { backend_rejects_host_signals: true, ..Config::default() };
    let mut scheduler = Scheduler::new(&config);
    parked_target(&mut scheduler, tid);
    for thread in [deferred, external] {
        register_known_thread(&mut scheduler, thread);
        scheduler.thread_tree.add_child(tid, thread, false);
        scheduler.next_turns.get_mut(&thread).unwrap().req = Ivar::full(Ok(Resources::new(thread)));
    }
    let op = ExternalOpId::new(external, 7);
    let mut ready = Resources::new(external);
    ready.insert(ResourceID::VforkFailed(op), Permission::RW);
    scheduler.next_turns.get_mut(&external).unwrap().req = Ivar::full(Ok(ready.clone()));
    scheduler.blocked.external_io_blockers.insert(external, op);
    scheduler.blocked.sigchld_deferred.insert(deferred);
    let global_time = Arc::new(Mutex::new(GlobalTime::new(&config)));
    let before = global_time.lock().unwrap().as_nanos();
    scheduler.blocked.timed_waiters.insert_child_exit(before, DetTid::from_raw(23), tid, tid);
    scheduler.turn = 17;
    let scheduler = Arc::new(Mutex::new(scheduler));
    let mut wake = scheduler.lock().unwrap().backend_failure_waiter();
    assert!(futures::poll!(&mut wake).is_pending());
    let mut expected_clock = GlobalTime::new(&config);
    expected_clock.add_scheduler_time();
    let expected_time = expected_clock.as_nanos();
    let last_turn = Ok(Resources::new(tid));
    assert!(do_a_turn_blocking(scheduler.clone(), global_time.clone(), &last_turn).await.is_err());
    assert!(futures::poll!(&mut wake).is_ready(), "real turn must publish refusal even on SkipTurn");
    let sched = scheduler.lock().unwrap();
    assert_eq!(sched.signal_refusal(), Some(KvmSignalRefusal { signal: libc::SIGCHLD, target: tid, turn: 17 }));
    assert_eq!(global_time.lock().unwrap().as_nanos(), expected_time);
    assert!(expected_time > before);
    assert_eq!(sched.committed_time, expected_time);
    assert_eq!(sched.turn, 17);
    assert_eq!(sched.blocked.external_io_blockers.get(&external), Some(&op));
    assert_eq!(sched.next_turns[&external].req.try_read(), Some(Ok(ready)));
    assert!(sched.blocked.sigchld_deferred.contains(&deferred));
    assert!(sched.run_queue.is_empty());
    assert!(!sched.run_queue.tentative_pop_in_progress());
    for thread in [tid, deferred, external] {
        assert!(sched.next_turns[&thread].resp.try_read().is_none());
    }
    target.assert_pending(libc::SIGCHLD, false);
}

#[tokio::test]
async fn empty_queue_kvm_signal_refusal_keeps_the_reached_deadline_and_stops_the_turn() {
    let mut target = OwnedSignalTarget::new(&[libc::SIGUSR1]);
    let tid = DetTid::from_raw(target.pid());
    let config = Config { backend_rejects_host_signals: true, ..Config::default() };
    let mut scheduler = Scheduler::new(&config);
    parked_target(&mut scheduler, tid);
    let global_time = Arc::new(Mutex::new(GlobalTime::new(&config)));
    let before = global_time.lock().unwrap().as_nanos();
    let deadline = before + LogicalTime::from_nanos(1_000);
    scheduler.blocked.timed_waiters.insert_alarm(deadline, tid, tid, Signal::SIGUSR1, LogicalTime::ZERO);
    let scheduler = Arc::new(Mutex::new(scheduler));
    let mut wake = scheduler.lock().unwrap().backend_failure_waiter();
    let last_turn = Err(SkipTurn);
    assert!(do_a_turn_blocking(scheduler.clone(), global_time.clone(), &last_turn).await.is_err());
    assert!(futures::poll!(&mut wake).is_ready());
    let sched = scheduler.lock().unwrap();
    assert_eq!(sched.signal_refusal(), Some(KvmSignalRefusal { signal: libc::SIGUSR1, target: tid, turn: 0 }));
    assert_eq!(global_time.lock().unwrap().as_nanos(), deadline);
    assert_eq!(sched.committed_time, before);
    assert_eq!(sched.turn, 0);
    assert!(sched.run_queue.is_empty());
    assert!(sched.next_turns[&tid].resp.try_read().is_none());
    assert!(sched.blocked.timed_waiters.is_empty());
    target.assert_pending(libc::SIGUSR1, false);
}
