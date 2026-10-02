/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! STATE-MACHINE tests of private receipt/FIFO rules, not kernel-consumption
//! evidence. Native consumption and controls live in the separate native suite.
use super::*;

const STOP: i32 = (libc::SIGSTOP << 8) | 0x7f;

fn modeled_entry(handle: &EventHandle, raw: i32) -> StatusEntry {
    let receipt = handle
        .event()
        .source
        .lock()
        // Existing modeled ptrace-stop premise; not real kernel evidence.
        .record_consumed(handle.event(), raw, libc::CLD_TRAPPED);
    StatusEntry { raw, receipt }
}

fn claim_sync(event: &Event) -> SyncWaitOwner<'_> {
    match event.claim_sync_wait().unwrap() {
        SyncWaitOwnership::Claimed(owner) => owner,
        SyncWaitOwnership::Notifier => panic!("state-machine Event has no worker"),
    }
}

fn committed_model_return(handle: &EventHandle) -> Stopped {
    let event = handle.event();
    let mut owner = claim_sync(event);
    let reservation = event.try_status_reservation_sync().unwrap().unwrap();
    let token = reservation.token(handle.clone());
    match owner
        .decode_status_return(Pid::from_raw(7), reservation, |raw| {
            Wait::from_raw_with_token(Pid::from_raw(7), raw, token)
        })
        .unwrap()
    {
        StatusReturn::Returned(wait) => consumed(wait).assume_stopped().0,
        StatusReturn::Cancelled(_) => panic!("uncancelled state-machine return"),
    }
}

#[test]
fn model_exact_fifo_rollback_and_preview_preserve_receipt_not_newer_revision() {
    let handle = EventHandle::new();
    let event = handle.event();
    event.publish_status(modeled_entry(&handle, STOP));
    let original_entry = Arc::clone(event.status.lock().pending.entries.front().unwrap());
    let original_receipt = original_entry.receipt.clone().unwrap();
    {
        let mut owner = claim_sync(event);
        let reservation = event.try_status_reservation_sync().unwrap().unwrap();
        assert!(
            event.status.try_lock().is_some(),
            "reservation releases status mutex"
        );
        event.source.lock().invalidate(); // Explicitly modeled control.
        event.publish_status(modeled_entry(&handle, STOP)); // Same raw bits, NEW receipt.
        let token = reservation.token(handle.clone());
        assert_eq!(token.source.as_ref().unwrap().receipt, original_receipt);
        assert_ne!(
            token.source.as_ref().unwrap().receipt.revision,
            event.source.lock().published
        );
        let result = owner.decode_status_return(Pid::from_raw(7), reservation, |_| {
            Err::<Wait, _>(Errno::EIO.into())
        });
        assert!(matches!(result, Err(Error::Errno(Errno::EIO))));
    }
    assert_eq!(event.wait_owner.load(Ordering::Acquire), WAIT_OWNER_NONE);
    assert!(Arc::ptr_eq(
        event.status.lock().pending.entries.front().unwrap(),
        &original_entry
    ));
    let old = committed_model_return(&handle);
    assert_eq!(old.1.source.as_ref().unwrap().receipt, original_receipt);
    assert_eq!(old.source_stop().map(|_| ()), Err(Errno::EPERM));

    let newest = Arc::clone(event.status.lock().pending.entries.front().unwrap());
    assert!(!Arc::ptr_eq(&newest, &original_entry));
    let cleanup = TerminalCleanup::new_unregistered(
        Pid::from_raw(7),
        &TraceeToken::from_event(handle.clone()),
    );
    {
        let preview = cleanup
            .reserve_pending_for_cleanup(Duration::from_millis(1))
            .unwrap();
        assert!(
            event.status.try_lock().is_some(),
            "cleanup preview releases status mutex"
        );
        let stopped = preview.decode().unwrap().assume_stopped().0;
        assert_eq!(
            stopped.1.source.as_ref().unwrap().receipt,
            newest.receipt.clone().unwrap()
        );
        assert_eq!(stopped.source_stop().map(|_| ()), Err(Errno::EPERM));
        assert!(
            !event.source.lock().consumed,
            "preview is not a committed return"
        );
    }
    assert!(Arc::ptr_eq(
        event.status.lock().pending.entries.front().unwrap(),
        &newest
    ));
    let next = committed_model_return(&handle);
    assert!(next.1.source.as_ref().unwrap().consumed);
    // This is a model flag assertion, never a forged WorkerIdentity or native
    // SourceStop success. Public source still requires actual registered custody.
    assert_eq!(next.source_stop().map(|_| ()), Err(Errno::ENODATA));
    assert!(event.status.lock().pending.is_empty());
}

#[test]
fn model_revision_exhaustion_refuses_without_reusing_eligible_revision() {
    let handle = EventHandle::new();
    let event = handle.event();
    event.source.lock().revision = u64::MAX - 2;
    let last = modeled_entry(&handle, STOP);
    assert_eq!(last.receipt.as_ref().unwrap().revision, Some(u64::MAX - 1));
    event.publish_status(last);
    let last = committed_model_return(&handle);
    assert!(last.1.source.as_ref().unwrap().consumed);
    for _ in 0..2 {
        let exhausted = modeled_entry(&handle, STOP);
        assert_eq!(exhausted.receipt.as_ref().unwrap().revision, None);
        event.publish_status(exhausted);
        let stopped = committed_model_return(&handle);
        assert_eq!(stopped.source_stop().map(|_| ()), Err(Errno::EPERM));
        assert_eq!(event.source.lock().revision, u64::MAX);
        assert_eq!(event.source.lock().published, None);
    }
}

#[test]
fn model_synthetic_terminal_exit_and_cross_event_entries_cannot_authenticate() {
    let handle = EventHandle::new();
    assert!(modeled_entry(&handle, 0).receipt.is_none());
    assert!(
        modeled_entry(&handle, PTRACE_EVENT_EXIT_STOP)
            .receipt
            .is_none()
    );
    handle.event().update(STOP);
    let synthetic = committed_model_return(&handle);
    assert!(synthetic.1.source.is_none());
    assert_eq!(synthetic.source_stop().map(|_| ()), Err(Errno::ENODATA));
    let other = EventHandle::new();
    let foreign = modeled_entry(&handle, STOP);
    // A copied numeric revision in another Event is not the original receipt.
    other.event().source.lock().revision = handle.event().source.lock().revision;
    other.event().publish_status(foreign);
    let stopped = committed_model_return(&other);
    assert_eq!(stopped.source_stop().map(|_| ()), Err(Errno::EPERM));
}

#[test]
fn model_queued_receipt_has_no_strong_event_cycle() {
    let weak = {
        let handle = EventHandle::new();
        handle.event().publish_status(modeled_entry(&handle, STOP));
        Arc::downgrade(handle.event())
    };
    assert!(
        weak.upgrade().is_none(),
        "Event/FIFO/receipt must not form a strong cycle"
    );
}

#[test]
fn model_equal_raw_stops_retain_distinct_kernel_stop_kinds() {
    // Model only: same Event/revision/raw bits, different explicitly supplied
    // kernel-kind premise. Actual waitid transport is covered by real children.
    let handle = EventHandle::new();
    let trapped = modeled_entry(&handle, STOP).receipt.unwrap();
    let mut job_control = trapped.clone();
    job_control.si_code = libc::CLD_STOPPED;
    assert_ne!(trapped, job_control);
    assert!(trapped.is_ptrace_stop());
    assert!(!job_control.is_ptrace_stop());
}
