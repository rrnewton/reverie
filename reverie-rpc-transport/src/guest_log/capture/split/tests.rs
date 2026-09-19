use super::*;

#[test]
fn split_each_worker_startup_failure_retains_destination_until_actual_joins() {
    struct DropProbe {
        allowed: Arc<std::sync::atomic::AtomicBool>,
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }
    impl Write for DropProbe {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl CaptureDestination for DropProbe {
        fn progress(&self) -> DestinationProgress {
            DestinationProgress::default()
        }
    }
    impl Drop for DropProbe {
        fn drop(&mut self) {
            assert!(
                self.allowed.load(Ordering::Acquire),
                "destination released before actual joins"
            );
            assert!(!self.dropped.swap(true, Ordering::AcqRel));
        }
    }
    for fault in [
        StartFault::PublicationSpawn,
        StartFault::CollectorSpawn,
        StartFault::PublicationReady,
        StartFault::CollectorReady,
    ] {
        let allowed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut workers = Workers {
            owner: None,
            escrow: None,
        };
        let options = CaptureOptions {
            limits: CaptureLimits {
                producers: 2,
                slots_per_producer: 8,
                max_record_bytes: 64,
                host_pending_bytes: 256,
                guest_pending_bytes: 256,
                pending_records: 8,
                diagnostic_bytes: 128,
            },
            timeouts: CaptureTimeouts {
                startup: Duration::from_secs(2),
                blocked_publication: Duration::from_secs(2),
                final_drain: Duration::from_secs(2),
            },
        };
        let plan = unsafe { SplitCapturePlan::new(options) }.unwrap();
        START_FAULT.with(|slot| {
            assert!(slot.get().is_none());
            slot.set(Some(fault));
        });
        assert!(
            workers
                .start(
                    plan,
                    DropProbe {
                        allowed: allowed.clone(),
                        dropped: dropped.clone()
                    },
                    Instant::now() + Duration::from_secs(2)
                )
                .is_err()
        );
        START_FAULT.with(|slot| assert!(slot.get().is_none()));
        assert!(!dropped.load(Ordering::Acquire));
        assert!(workers.escrow.is_some());
        if let Some(report) = workers.finish(Instant::now() + Duration::from_secs(2)) {
            assert!(!report.qualifies());
        }
        workers.join_blocking();
        assert_eq!(workers.joins(), (Some(true), Some(true)));
        assert!(!dropped.load(Ordering::Acquire));
        allowed.store(true, Ordering::Release);
        drop(workers.escrow.take());
        assert!(dropped.load(Ordering::Acquire));
    }
}

#[test]
fn split_lifecycle_one_use_and_late_write_refuse() {
    let lifecycle = Lifecycle::new().unwrap();
    assert!(!lifecycle.snapshot().qualifies());
    assert!(lifecycle.start());
    lifecycle.facts(1, true, 0);
    assert!(lifecycle.enter());
    lifecycle.close();
    assert_eq!(lifecycle.snapshot().entrants, 1);
    lifecycle.leave();
    lifecycle.finish();
    assert!(lifecycle.snapshot().qualifies());
    assert!(!lifecycle.enter());
    assert_eq!(lifecycle.snapshot().late_writes, 1);
    assert!(!lifecycle.snapshot().qualifies());
    assert!(!lifecycle.start());
}

#[test]
fn split_caught_panic_is_distinct_from_ordinary_nonzero_guest() {
    let panic = CoordinatorFacts {
        disposition: CoordinatorDisposition::CaughtPanic,
        guest_wait_status: 0,
        rpc_issues: vec![],
    };
    assert!(!panic.qualifies());
    let nonzero = CoordinatorFacts {
        disposition: CoordinatorDisposition::Completed,
        guest_wait_status: ExitStatus::Exited(7).into_raw(),
        rpc_issues: vec![],
    };
    assert!(!nonzero.qualifies());
    assert_ne!(panic, nonzero);
}

fn closing_fixture(slots: usize) -> (Finalizer, Arc<ordered::Buffer>, UnixStream) {
    let options = CaptureOptions {
        limits: CaptureLimits {
            producers: 2,
            slots_per_producer: slots,
            max_record_bytes: 64,
            host_pending_bytes: 256,
            guest_pending_bytes: 256,
            pending_records: 8,
            diagnostic_bytes: 128,
        },
        timeouts: CaptureTimeouts {
            startup: Duration::from_secs(2),
            blocked_publication: Duration::from_secs(2),
            final_drain: Duration::from_secs(2),
        },
    };
    let plan = unsafe { SplitCapturePlan::new(options) }.unwrap();
    let (_, buffer, host, guest) = plan.inert.into_local_parts();
    let buffer = Arc::new(buffer);
    let lifecycle = Arc::new(plan.lifecycle);
    assert!(lifecycle.start());
    lifecycle.facts(1, true, 0);
    let writer = unsafe { buffer.activate(0, i64::from(std::process::id())) }.unwrap();
    (
        Finalizer {
            emitter: CoordinatorEmitter(Arc::new(EmitterLocal {
                buffer: buffer.clone(),
                writer: Mutex::new(writer),
                lifecycle,
                options,
            })),
            endpoint: guest,
        },
        buffer,
        host,
    )
}
#[test]
fn split_closing_preserves_held_entrant_then_late_fault() {
    let (finalizer, buffer, _host) = closing_fixture(8);
    let life = finalizer.emitter.0.lifecycle.clone();
    assert!(life.enter());
    let begin = Instant::now();
    drop(finalizer);
    println!("held entrant closing duration {:?}", begin.elapsed());
    assert!(begin.elapsed() >= Duration::from_secs(2));
    let before = life.snapshot();
    assert!(before.closed && before.faulted);
    assert_eq!(before.entrants, 1);
    assert!(!before.finished);
    assert!(buffer.admission(ordered::Role::Host).closed);
    life.leave();
    assert!(!life.snapshot().qualifies());
}
#[test]
fn split_closing_writer_lock_and_poison_refuse_without_finish() {
    let (finalizer, _buffer, _host) = closing_fixture(8);
    let emitter = finalizer.emitter.clone();
    let writer = emitter.0.writer.lock().unwrap();
    let begin = Instant::now();
    drop(finalizer);
    println!("held writer closing duration {:?}", begin.elapsed());
    assert!(begin.elapsed() >= Duration::from_secs(2));
    assert!(emitter.0.lifecycle.snapshot().faulted);
    assert!(!emitter.0.lifecycle.snapshot().finished);
    drop(writer);
    let (finalizer, _buffer, _host) = closing_fixture(8);
    let emitter = finalizer.emitter.clone();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _writer = emitter.0.writer.lock().unwrap();
            panic!("actual poisoned emitter lock");
        }))
        .is_err()
    );
    drop(finalizer);
    assert!(emitter.0.lifecycle.snapshot().faulted);
    assert!(!emitter.0.lifecycle.snapshot().finished);
}
#[test]
fn split_closing_full_ring_without_collector_cannot_invent_finish() {
    let (finalizer, buffer, _host) = closing_fixture(4);
    let emitter = finalizer.emitter.clone();
    emitter.write_record(b"").unwrap();
    emitter.write_record(b"").unwrap();
    let begin = Instant::now();
    drop(finalizer);
    println!("full ring closing duration {:?}", begin.elapsed());
    assert!(begin.elapsed() >= Duration::from_secs(2));
    assert!(emitter.0.lifecycle.snapshot().faulted);
    assert!(!emitter.0.lifecycle.snapshot().finished);
    // Only now release the paused collector; actual committed records remain.
    let mut collector = buffer.collector().unwrap();
    for expected in 1..=2 {
        let record = collector.poll().unwrap().unwrap();
        assert_eq!(record.order(), expected);
        record.release().unwrap();
    }
    assert!(collector.poll().unwrap().is_none());
    assert!(!collector.host_complete());
    assert!(!emitter.0.lifecycle.snapshot().qualifies());
}

#[test]
fn split_wire_facts_refuse_missing_unknown_oversized_and_trailing_claims() {
    let envelope = SplitChildResult {
        value: Some(37u8),
        facts: CoordinatorFacts {
            disposition: CoordinatorDisposition::Completed,
            guest_wait_status: 0,
            rpc_issues: vec![],
        },
        finalizer: None,
    };
    let bytes = bincode::serde::encode_to_vec(&envelope, bincode::config::legacy()).unwrap();
    let (decoded, used): (SplitChildResult<u8>, _) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
    assert_eq!(used, bytes.len());
    assert!(decoded.facts.qualifies());
    assert_eq!(decoded.value, Some(37));
    assert!(
        bincode::serde::decode_from_slice::<SplitChildResult<u8>, _>(
            &[],
            bincode::config::legacy()
        )
        .is_err()
    );
    let mut unknown = bytes.clone();
    unknown[..4].copy_from_slice(&99u32.to_le_bytes());
    assert!(
        bincode::serde::decode_from_slice::<SplitChildResult<u8>, _>(
            &unknown,
            bincode::config::legacy()
        )
        .is_err()
    );
    let mut oversized = envelope;
    oversized.facts.rpc_issues = vec![
        SplitRpcIssue {
            connection: 1,
            failure: SplitRpcFailure::Interrupted
        };
        65
    ];
    let oversized = bincode::serde::encode_to_vec(&oversized, bincode::config::legacy()).unwrap();
    assert!(
        bincode::serde::decode_from_slice::<SplitChildResult<u8>, _>(
            &oversized,
            bincode::config::legacy()
        )
        .is_err()
    );
    let mut trailing = bytes;
    trailing.push(99);
    let (_, used): (SplitChildResult<u8>, _) =
        bincode::serde::decode_from_slice(&trailing, bincode::config::legacy()).unwrap();
    // Landed OwnedReapedResult::decode enforces this complete-buffer predicate;
    // a successful inner decoder alone is not full result acceptance.
    assert_ne!(used, trailing.len());
}
