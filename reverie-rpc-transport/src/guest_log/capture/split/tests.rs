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
            task_exits: TaskExits::default(),
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
        workers.settle_task_exits_blocking();
        assert!(!dropped.load(Ordering::Acquire));
        allowed.store(true, Ordering::Release);
        drop(workers.escrow.take());
        assert!(dropped.load(Ordering::Acquire));
    }
}

#[test]
fn split_publication_anchor_failure_retains_escrow_and_factories_until_recovery() {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;

    struct DestinationProbe {
        drops: Arc<AtomicUsize>,
        barrier_completed: Arc<AtomicBool>,
    }
    impl Write for DestinationProbe {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl CaptureDestination for DestinationProbe {
        fn progress(&self) -> DestinationProgress {
            DestinationProgress::default()
        }
    }
    impl Drop for DestinationProbe {
        fn drop(&mut self) {
            assert!(
                self.barrier_completed.load(Ordering::Acquire),
                "destination released before task-exit barrier"
            );
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }
    struct FactoryProbe {
        drops: Arc<AtomicUsize>,
        barrier_completed: Arc<AtomicBool>,
    }
    impl Drop for FactoryProbe {
        fn drop(&mut self) {
            assert!(
                self.barrier_completed.load(Ordering::Acquire),
                "factory released before task-exit barrier"
            );
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    let destination_drops = Arc::new(AtomicUsize::new(0));
    let factory_drops = Arc::new(AtomicUsize::new(0));
    let barrier_completed = Arc::new(AtomicBool::new(false));
    let blocked = Arc::new(AtomicBool::new(true));
    ANCHOR_FAULT.with(|fault| {
        assert!(fault.borrow().is_none());
        *fault.borrow_mut() = Some((AnchorWorker::Publication, blocked.clone()));
    });
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
    let mut workers = Workers {
        owner: None,
        escrow: None,
        task_exits: TaskExits::default(),
    };
    assert_eq!(
        workers.start(
            plan,
            DestinationProbe {
                drops: destination_drops.clone(),
                barrier_completed: barrier_completed.clone(),
            },
            Instant::now() + Duration::from_secs(2),
        ),
        Err(StartupError::Protocol)
    );
    ANCHOR_FAULT.with(|fault| assert!(fault.borrow().is_none()));
    assert!(workers.owner.is_some());
    assert!(workers.task_exits.publication.is_some());
    assert!(workers.task_exits.collector.is_none());
    workers
        .task_exits
        .install_barrier_probe(barrier_completed.clone());

    let mut run = SplitCaptureRun::<u8, FactoryProbe, FactoryProbe> {
        parent_factory: Some(FactoryProbe {
            drops: factory_drops.clone(),
            barrier_completed: barrier_completed.clone(),
        }),
        child_factory: Some(FactoryProbe {
            drops: factory_drops.clone(),
            barrier_completed: barrier_completed.clone(),
        }),
        plan: Cell::new(None),
        workers,
        child: None,
        result: None,
        status: None,
        failure: None,
        frozen: None,
        integrity_faults: IntegrityFaultSet::default(),
        frozen_integrity: None,
        failed_bytes: Vec::new(),
    };
    run.fail(
        IntegrityFault::OwnedChild,
        "injected publication anchor startup failure",
    );
    let run = match run.settle_until(Instant::now() + Duration::from_secs(1)) {
        SplitCaptureOutcome::Unjoined(run) => run,
        SplitCaptureOutcome::Joined(_) => panic!("blocked anchor released owned resources"),
    };
    assert_eq!(destination_drops.load(Ordering::Acquire), 0);
    assert_eq!(factory_drops.load(Ordering::Acquire), 0);
    assert!(!barrier_completed.load(Ordering::Acquire));

    blocked.store(false, Ordering::Release);
    let joined = match run.settle_until(Instant::now() + Duration::from_secs(2)) {
        SplitCaptureOutcome::Joined(joined) => joined,
        SplitCaptureOutcome::Unjoined(_) => panic!("restored anchor did not settle"),
    };
    assert_eq!(joined.actual_joins, (true, true));
    assert!(matches!(
        joined.integrity,
        TerminalCaptureIntegrity::Incomplete { faults }
            if faults.contains(IntegrityFault::OwnedChild)
                && faults.contains(IntegrityFault::Teardown)
    ));
    assert_eq!(
        joined.report.failure.as_deref(),
        Some("injected publication anchor startup failure")
    );
    assert!(barrier_completed.load(Ordering::Acquire));
    assert_eq!(destination_drops.load(Ordering::Acquire), 1);
    assert_eq!(factory_drops.load(Ordering::Acquire), 2);
}

#[test]
fn split_collector_anchor_failure_retains_both_workers_and_factories_until_recovery() {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;

    struct DestinationProbe {
        drops: Arc<AtomicUsize>,
        barrier_completed: Arc<AtomicBool>,
    }
    impl Write for DestinationProbe {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl CaptureDestination for DestinationProbe {
        fn progress(&self) -> DestinationProgress {
            DestinationProgress::default()
        }
    }
    impl Drop for DestinationProbe {
        fn drop(&mut self) {
            assert!(
                self.barrier_completed.load(Ordering::Acquire),
                "destination released before task-exit barrier"
            );
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }
    struct FactoryProbe {
        drops: Arc<AtomicUsize>,
        barrier_completed: Arc<AtomicBool>,
    }
    impl Drop for FactoryProbe {
        fn drop(&mut self) {
            assert!(
                self.barrier_completed.load(Ordering::Acquire),
                "factory released before task-exit barrier"
            );
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    let destination_drops = Arc::new(AtomicUsize::new(0));
    let factory_drops = Arc::new(AtomicUsize::new(0));
    let barrier_completed = Arc::new(AtomicBool::new(false));
    let blocked = Arc::new(AtomicBool::new(true));
    ANCHOR_FAULT.with(|fault| {
        assert!(fault.borrow().is_none());
        *fault.borrow_mut() = Some((AnchorWorker::Collector, blocked.clone()));
    });
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
    let mut workers = Workers {
        owner: None,
        escrow: None,
        task_exits: TaskExits::default(),
    };
    assert_eq!(
        workers.start(
            plan,
            DestinationProbe {
                drops: destination_drops.clone(),
                barrier_completed: barrier_completed.clone(),
            },
            Instant::now() + Duration::from_secs(2),
        ),
        Err(StartupError::Protocol)
    );
    ANCHOR_FAULT.with(|fault| assert!(fault.borrow().is_none()));
    assert!(workers.task_exits.publication.is_some());
    assert!(workers.task_exits.collector.is_some());
    {
        let owner = workers.owner.as_ref().expect("both workers have an owner");
        assert!(owner.shared.publication.snapshot().ready);
        assert!(!owner.shared.publication.worker_finished());
        let collector = owner.shared.collector.lock().unwrap();
        let collector = collector.as_ref().expect("collector handle retained");
        assert!(!collector.is_finished());
    }
    let publication_exit = workers.task_exits.publication.as_ref().unwrap().clone();
    let collector_exit = workers.task_exits.collector.as_ref().unwrap().clone();
    workers
        .task_exits
        .install_barrier_probe(barrier_completed.clone());

    let mut run = SplitCaptureRun::<u8, FactoryProbe, FactoryProbe> {
        parent_factory: Some(FactoryProbe {
            drops: factory_drops.clone(),
            barrier_completed: barrier_completed.clone(),
        }),
        child_factory: Some(FactoryProbe {
            drops: factory_drops.clone(),
            barrier_completed: barrier_completed.clone(),
        }),
        plan: Cell::new(None),
        workers,
        child: None,
        result: None,
        status: None,
        failure: None,
        frozen: None,
        integrity_faults: IntegrityFaultSet::default(),
        frozen_integrity: None,
        failed_bytes: Vec::new(),
    };
    run.fail(
        IntegrityFault::OwnedChild,
        "injected collector anchor startup failure",
    );
    let run = match run.settle_until(Instant::now() + Duration::from_secs(1)) {
        SplitCaptureOutcome::Unjoined(run) => run,
        SplitCaptureOutcome::Joined(_) => panic!("blocked collector released owned resources"),
    };
    assert_eq!(run.workers.joins(), (None, None));
    assert!(
        run.workers
            .owner
            .as_ref()
            .unwrap()
            .shared
            .collector
            .lock()
            .unwrap()
            .is_some()
    );
    assert_eq!(destination_drops.load(Ordering::Acquire), 0);
    assert_eq!(factory_drops.load(Ordering::Acquire), 0);
    assert!(!barrier_completed.load(Ordering::Acquire));

    blocked.store(false, Ordering::Release);
    let joined = match run.settle_until(Instant::now() + Duration::from_secs(2)) {
        SplitCaptureOutcome::Joined(joined) => joined,
        SplitCaptureOutcome::Unjoined(_) => panic!("restored collector anchor did not settle"),
    };
    assert_eq!(joined.actual_joins, (true, true));
    assert!(publication_exit.detached_for_test());
    assert!(collector_exit.detached_for_test());
    assert!(barrier_completed.load(Ordering::Acquire));
    assert!(matches!(
        joined.integrity,
        TerminalCaptureIntegrity::Incomplete { faults }
            if faults.contains(IntegrityFault::OwnedChild)
                && faults.contains(IntegrityFault::Teardown)
    ));
    assert_eq!(
        joined.report.failure.as_deref(),
        Some("injected collector anchor startup failure")
    );
    assert_eq!(destination_drops.load(Ordering::Acquire), 1);
    assert_eq!(factory_drops.load(Ordering::Acquire), 2);
}

#[test]
fn split_drop_waits_for_collector_anchor_and_barrier_before_resource_release() {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    struct DestinationProbe {
        drops: Arc<AtomicUsize>,
        barrier_completed: Arc<AtomicBool>,
    }
    impl Write for DestinationProbe {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl CaptureDestination for DestinationProbe {
        fn progress(&self) -> DestinationProgress {
            DestinationProgress::default()
        }
    }
    impl Drop for DestinationProbe {
        fn drop(&mut self) {
            assert!(
                self.barrier_completed.load(Ordering::Acquire),
                "Drop released destination before task-exit barrier"
            );
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }
    struct FactoryProbe {
        drops: Arc<AtomicUsize>,
        barrier_completed: Arc<AtomicBool>,
    }
    impl Drop for FactoryProbe {
        fn drop(&mut self) {
            assert!(
                self.barrier_completed.load(Ordering::Acquire),
                "Drop released factory before task-exit barrier"
            );
            self.drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    let blocked = Arc::new(AtomicBool::new(true));
    let barrier_completed = Arc::new(AtomicBool::new(false));
    let destination_drops = Arc::new(AtomicUsize::new(0));
    let factory_drops = Arc::new(AtomicUsize::new(0));
    let scenario_blocked = blocked.clone();
    let scenario_barrier = barrier_completed.clone();
    let scenario_destination_drops = destination_drops.clone();
    let scenario_factory_drops = factory_drops.clone();
    let (done_tx, done_rx) = mpsc::sync_channel(1);

    let scenario = std::thread::spawn(move || {
        ANCHOR_FAULT.with(|fault| {
            assert!(fault.borrow().is_none());
            *fault.borrow_mut() = Some((AnchorWorker::Collector, scenario_blocked.clone()));
        });
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
        let mut workers = Workers {
            owner: None,
            escrow: None,
            task_exits: TaskExits::default(),
        };
        assert_eq!(
            workers.start(
                plan,
                DestinationProbe {
                    drops: scenario_destination_drops.clone(),
                    barrier_completed: scenario_barrier.clone(),
                },
                Instant::now() + Duration::from_secs(2),
            ),
            Err(StartupError::Protocol)
        );
        ANCHOR_FAULT.with(|fault| assert!(fault.borrow().is_none()));
        let publication_exit = workers.task_exits.publication.as_ref().unwrap().clone();
        let collector_exit = workers.task_exits.collector.as_ref().unwrap().clone();
        let shared = workers.owner.as_ref().unwrap().shared.clone();
        workers
            .task_exits
            .install_barrier_probe(scenario_barrier.clone());

        let mut run = SplitCaptureRun::<u8, FactoryProbe, FactoryProbe> {
            parent_factory: Some(FactoryProbe {
                drops: scenario_factory_drops.clone(),
                barrier_completed: scenario_barrier.clone(),
            }),
            child_factory: Some(FactoryProbe {
                drops: scenario_factory_drops.clone(),
                barrier_completed: scenario_barrier.clone(),
            }),
            plan: Cell::new(None),
            workers,
            child: None,
            result: None,
            status: None,
            failure: None,
            frozen: None,
            integrity_faults: IntegrityFaultSet::default(),
            frozen_integrity: None,
            failed_bytes: Vec::new(),
        };
        run.fail(
            IntegrityFault::OwnedChild,
            "injected collector anchor Drop failure",
        );
        assert_eq!(scenario_destination_drops.load(Ordering::Acquire), 0);
        assert_eq!(scenario_factory_drops.load(Ordering::Acquire), 0);
        assert!(!scenario_barrier.load(Ordering::Acquire));

        let release_blocked = scenario_blocked.clone();
        let observe_retry = collector_exit.clone();
        let unblocker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while observe_retry.startup_attempt_for_test() < 2 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            let entered_drop_recovery = observe_retry.startup_attempt_for_test() >= 2;
            release_blocked.store(false, Ordering::Release);
            entered_drop_recovery
        });

        drop(run);
        assert!(
            unblocker.join().expect("anchor-unblocker panicked"),
            "Drop never requested a fresh collector anchor attempt"
        );
        assert_eq!(*shared.collector_join.lock().unwrap(), Some(true));
        assert_eq!(shared.publication.joined(), Some(true));
        assert!(publication_exit.detached_for_test());
        assert!(collector_exit.detached_for_test());
        assert!(scenario_barrier.load(Ordering::Acquire));
        assert_eq!(scenario_destination_drops.load(Ordering::Acquire), 1);
        assert_eq!(scenario_factory_drops.load(Ordering::Acquire), 2);
        done_tx
            .send(())
            .expect("test owner retained completion receiver");
    });

    match done_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(()) => scenario.join().expect("Drop scenario panicked"),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            scenario
                .join()
                .expect("Drop scenario panicked before completion");
            panic!("Drop scenario disconnected without completion");
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            blocked.store(false, Ordering::Release);
            panic!("Drop scenario exceeded its five-second bound");
        }
    }
    assert!(barrier_completed.load(Ordering::Acquire));
    assert_eq!(destination_drops.load(Ordering::Acquire), 1);
    assert_eq!(factory_drops.load(Ordering::Acquire), 2);
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

#[test]
fn terminal_status_preserves_actual_realtime_deaths_and_refuses_nonterminal_bits() {
    use std::os::unix::process::ExitStatusExt;
    for signal in [libc::SIGRTMIN(), libc::SIGRTMAX()] {
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("kill -{signal} $$")])
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(signal));
        assert_eq!(terminal_guest_status(status.into_raw()), Some(status));
    }
    for code in [0, 7, 255] {
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("exit {code}")])
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(code));
        assert_eq!(terminal_guest_status(status.into_raw()), Some(status));
    }
    for raw in [
        -1,
        0xffff,
        (libc::SIGSTOP << 8) | 0x7f,
        0x10000,
        0x80,
        libc::SIGRTMAX() + 1,
        (7 << 8) | libc::SIGTERM,
    ] {
        assert_eq!(terminal_guest_status(raw), None, "raw={raw:#x}");
    }
}

#[test]
fn real_publication_eio_is_sticky_in_both_first_failure_orders() {
    use std::os::fd::AsRawFd;
    use std::sync::Condvar;
    struct Eio {
        gate: Arc<(Mutex<(bool, bool)>, Condvar)>,
    }
    impl Write for Eio {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            let (lock, changed) = &*self.gate;
            let mut state = lock.lock().unwrap();
            state.0 = true;
            changed.notify_all();
            while !state.1 {
                state = changed.wait(state).unwrap();
            }
            Err(io::Error::from_raw_os_error(libc::EIO))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl CaptureDestination for Eio {
        fn progress(&self) -> DestinationProgress {
            DestinationProgress::default()
        }
    }
    for policy_first in [true, false] {
        let gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
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
        let (mut owner, mut sink, host) =
            unsafe { prepared_capture(options, Eio { gate: gate.clone() }) }.unwrap();
        let endpoint = sink.take_prepared_endpoint().unwrap().unwrap();
        let buffer = unsafe { ordered::Buffer::receive(endpoint.as_raw_fd()) }.unwrap();
        let mut guest = unsafe { buffer.activate(1, i64::from(std::process::id())) }.unwrap();
        guest.finish(|_, _| Ok(())).unwrap();
        drop((guest, buffer, endpoint));
        host.write_record(b"actual publication EIO").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !gate.0.lock().unwrap().0 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        if policy_first {
            owner.shared.record_guest_outcome_policy_failure();
        }
        {
            let mut state = gate.0.lock().unwrap();
            state.1 = true;
            gate.1.notify_all();
        }
        while owner.shared.publication.snapshot().error.is_none()
            || owner.shared.integrity_faults.load(Ordering::Acquire) == 0
        {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        if !policy_first {
            owner.shared.record_guest_outcome_policy_failure();
        }
        let report = owner.finish_until(deadline);
        assert_eq!(
            report.publication.error.as_deref(),
            Some("destination write failed")
        );
        assert_eq!(
            report.error.as_deref(),
            Some(if policy_first {
                "split coordinator facts/teardown do not qualify"
            } else {
                "canonical destination publication failed"
            })
        );
        let faults = IntegrityFaultSet(owner.shared.integrity_faults.load(Ordering::Acquire));
        assert!(faults.contains(IntegrityFault::Publication));
        assert!(faults.contains(IntegrityFault::SharedFailure));
        assert!(!report.qualifies());
        assert_eq!(*owner.shared.collector_join.lock().unwrap(), Some(true));
        assert_eq!(owner.shared.publication.joined(), Some(true));
    }
}

#[test]
fn decoded_envelope_binds_actual_wait_to_authoritative_lifecycle() {
    use std::os::unix::process::ExitStatusExt;
    for command in [
        "exit 0".to_string(),
        "exit 7".to_string(),
        format!("kill -{} $$", libc::SIGRTMIN()),
    ] {
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", &command])
            .status()
            .unwrap();
        let life = Lifecycle::new().unwrap();
        assert!(life.start());
        life.facts(1, status.success(), 0);
        life.close();
        life.finish();
        assert!(life.snapshot().integrity_ready());
        let envelope = SplitChildResult {
            value: Some(37u8),
            facts: CoordinatorFacts {
                disposition: CoordinatorDisposition::Completed,
                guest_wait_status: status.into_raw(),
                rpc_issues: vec![],
            },
            finalizer: None,
        };
        let bytes = bincode::serde::encode_to_vec(&envelope, bincode::config::legacy()).unwrap();
        let (decoded, used): (SplitChildResult<u8>, _) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
        assert_eq!(used, bytes.len());
        let facts = decoded.facts.clone();
        assert_eq!(
            facts.bound_terminal_status(Some(life.snapshot())),
            Some(status)
        );
        assert_eq!(facts.bound_terminal_status(None), None);
        let mut opposite = facts.clone();
        opposite.guest_wait_status = if status.success() {
            ExitStatus::Exited(7).into_raw()
        } else {
            0
        };
        assert_eq!(opposite.bound_terminal_status(Some(life.snapshot())), None);
        opposite = facts.clone();
        opposite.disposition = CoordinatorDisposition::CaughtPanic;
        assert_eq!(opposite.bound_terminal_status(Some(life.snapshot())), None);
        opposite = facts.clone();
        opposite.rpc_issues.push(SplitRpcIssue {
            connection: 1,
            failure: SplitRpcFailure::Transport,
        });
        assert_eq!(opposite.bound_terminal_status(Some(life.snapshot())), None);
        opposite = facts;
        opposite.guest_wait_status = (libc::SIGSTOP << 8) | 0x7f;
        assert_eq!(opposite.bound_terminal_status(Some(life.snapshot())), None);
        // A real late emitter entry faults the authoritative lifecycle even
        // though its previous serialized disposition/status/count still agree.
        assert!(!life.enter());
        assert!(!life.snapshot().integrity_ready());
        assert!(life.snapshot().faulted);
    }
}
