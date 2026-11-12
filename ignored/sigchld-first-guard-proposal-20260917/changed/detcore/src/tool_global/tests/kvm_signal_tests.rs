/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use super::*;
use super::super::TraceSchedEventResponse;
use crate::KvmSignalRefusal;
use crate::signal_test_support::OwnedSignalTarget;

fn stacktrace_config(signal: i32, refusal: bool) -> Config {
    Config {
        backend_rejects_host_signals: refusal,
        record_preemptions: true,
        stacktrace_event: vec![(0, None)],
        stacktrace_signal: Some(SigWrapper(signal)),
        ..Config::default()
    }
}

async fn publish_stacktrace_refusal(state: &GlobalState, tid: DetTid) {
    let mut first = std::pin::pin!(state.wait_for_backend_failure());
    let mut second = std::pin::pin!(state.wait_for_backend_failure());
    assert!(futures::poll!(first.as_mut()).is_pending());
    assert!(futures::poll!(second.as_mut()).is_pending());
    let mut time = DetTime::new(&state.cfg);
    time.add_syscall_with_cost(37);
    let mut rpc = std::pin::pin!(state.receive_rpc(
        Tid::from_raw(tid.as_raw()),
        (time, MmId::initial(tid), GlobalRequest::TraceSchedEvent(SchedEvent::branches(tid, 1), tid)),
    ));
    assert!(futures::poll!(rpc.as_mut()).is_pending(), "refused RPC cannot return any normal reply");
    assert!(state.signal_refusal().is_some());
    assert!(futures::poll!(first.as_mut()).is_ready());
    assert!(futures::poll!(second.as_mut()).is_ready());
    let mut late = std::pin::pin!(state.wait_for_backend_failure());
    assert!(futures::poll!(late.as_mut()).is_ready());
    assert!(futures::poll!(rpc.as_mut()).is_pending());
}

#[tokio::test]
async fn kvm_stacktrace_refusal_retains_named_and_realtime_signals_and_blocks_later_rpc_time() {
    for raw in [libc::SIGUSR1, libc::SIGRTMIN() + 1] {
        for sequential in [false, true] {
            let mut target = OwnedSignalTarget::new(&[raw]);
            let tid = DetTid::from_raw(target.pid());
            let mut config = stacktrace_config(raw, true);
            config.sequentialize_threads = sequential;
            let state = GlobalState::initialize(&config, false);
            state.sched.lock().unwrap().turn = 23;
            publish_stacktrace_refusal(&state, tid).await;
            assert_eq!(state.signal_refusal(), Some(KvmSignalRefusal { signal: raw, target: tid, turn: 23 }));
            target.assert_pending(raw, false);
            let before = serde_json::to_value(&*state.global_time.lock().unwrap()).unwrap();
            let mut later = DetTime::new(&config);
            later.add_syscall_with_cost(91);
            let mut ordinary = std::pin::pin!(state.receive_rpc(
                Tid::from_raw(tid.as_raw()),
                (later, MmId::initial(tid), GlobalRequest::GlobalTimeLowerBound),
            ));
            assert!(futures::poll!(ordinary.as_mut()).is_pending());
            assert_eq!(serde_json::to_value(&*state.global_time.lock().unwrap()).unwrap(), before);
            assert_eq!(state.sched.lock().unwrap().turn, 23);
        }
    }
}

#[tokio::test]
async fn native_stacktrace_named_and_realtime_signals_still_deliver_and_reply() {
    for raw in [libc::SIGUSR1, libc::SIGRTMIN() + 1] {
        let mut target = OwnedSignalTarget::new(&[raw]);
        let tid = DetTid::from_raw(target.pid());
        let config = stacktrace_config(raw, false);
        let state = GlobalState::initialize(&config, false);
        target.assert_pending(raw, false);
        let response = state.receive_rpc(
            Tid::from_raw(tid.as_raw()),
            (DetTime::new(&config), MmId::initial(tid), GlobalRequest::TraceSchedEvent(SchedEvent::branches(tid, 1), tid)),
        ).await;
        assert!(matches!(response.1, GlobalResponse::TraceSchedEvent(TraceSchedEventResponse { print_stack_strace: Some(None), .. })));
        target.assert_pending(raw, true);
        assert!(state.signal_refusal().is_none());
        assert!(!state.sched.lock().unwrap().backend_failed());
    }
}

#[tokio::test]
async fn kvm_stacktrace_refusal_closes_a_selected_transaction_before_waking_the_daemon() {
    let mut target = OwnedSignalTarget::new(&[libc::SIGUSR1]);
    let tid = DetTid::from_raw(target.pid());
    let config = stacktrace_config(libc::SIGUSR1, true);
    let state = GlobalState::initialize(&config, false);
    install_test_registration(&state, tid, Ivar::new());
    let (chosen, request, response) = state.sched.lock().unwrap().select_test_turn().unwrap();
    let mut daemon = std::pin::pin!(crate::scheduler::finish_selected_turn(
        state.sched.clone(), state.global_time.clone(), chosen, request, response.clone(),
    ));
    assert!(futures::poll!(daemon.as_mut()).is_pending());
    assert!(state.sched.lock().unwrap().run_queue.tentative_pop_in_progress());
    publish_stacktrace_refusal(&state, tid).await;
    assert!(!state.sched.lock().unwrap().run_queue.tentative_pop_in_progress());
    assert!(matches!(futures::poll!(daemon.as_mut()), std::task::Poll::Ready(Err(_))));
    assert!(response.try_read().is_none());
    assert_eq!(state.sched.lock().unwrap().turn, 0);
    target.assert_pending(libc::SIGUSR1, false);
}

#[tokio::test]
async fn kvm_signal_refusal_survives_natural_join_and_partial_recording_cleanup() {
    let mut target = OwnedSignalTarget::new(&[libc::SIGUSR1]);
    let tid = DetTid::from_raw(target.pid());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("partial.json");
    let mut config = stacktrace_config(libc::SIGUSR1, true);
    config.record_preemptions_to = Some(path.clone());
    let mut state = GlobalState::initialize(&config, false);
    publish_stacktrace_refusal(&state, tid).await;
    state.sched_handle = Some(tokio::spawn(async {}));
    state.join_internal_scheduler().await.unwrap();
    let expected = KvmSignalRefusal { signal: libc::SIGUSR1, target: tid, turn: 0 };
    assert_eq!(state.signal_refusal(), Some(expected.clone()));
    let cleanup = state.clean_up_after_backend_failure().await;
    assert_eq!(cleanup.signal_refusal, Some(expected));
    assert!(cleanup.scheduler.is_ok());
    assert!(cleanup.preemption_recording.is_ok());
    let retained = crate::preemptions::read_trace(&path);
    assert_eq!(retained, vec![SchedEvent::branches(tid, 1)]);
    target.assert_pending(libc::SIGUSR1, false);
}

#[tokio::test]
async fn kvm_signal_refusal_does_not_replace_scheduler_or_recording_failure() {
    let mut target = OwnedSignalTarget::new(&[libc::SIGUSR1]);
    let tid = DetTid::from_raw(target.pid());
    let directory = tempfile::tempdir().unwrap();
    let mut config = stacktrace_config(libc::SIGUSR1, true);
    config.record_preemptions_to = Some(directory.path().join("absent").join("partial.json"));
    let mut state = GlobalState::initialize(&config, false);
    publish_stacktrace_refusal(&state, tid).await;
    state.sched_handle = Some(tokio::spawn(async { panic!("independent scheduler failure"); }));
    let cleanup = state.clean_up_after_backend_failure().await;
    assert_eq!(cleanup.signal_refusal, Some(KvmSignalRefusal { signal: libc::SIGUSR1, target: tid, turn: 0 }));
    assert!(cleanup.scheduler.unwrap_err().is_panic());
    assert!(cleanup.preemption_recording.is_err());
    target.assert_pending(libc::SIGUSR1, false);
}
