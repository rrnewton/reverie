// Actual executor consumers and their post-removal error paths.
#[test]
fn signal_dequeue_all_consumers_preserve_domain_and_post_copyout_effects() {
    use reverie::PendingDomain;
    use reverie::SignalConsumer;
    let root = TestDir::new();
    for consumer in [
        SignalConsumer::ReturnToUser,
        SignalConsumer::SignalFd,
        SignalConsumer::SignalTimedWait,
    ] {
        for domain in [PendingDomain::Thread, PendingDomain::Process] {
            let mut executor = ElfExecutor::new(test_state(&root.0), false);
            executor.enable_signal_dequeues();
            executor.observe_ignored_signals_with_tool();
            let identity = executor.signal_task_identity().unwrap();
            let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
            let fd = process_alarm_signalfd(&mut executor, &mut memory);
            let event = process_alarm_event(executor.state.pid);
            if domain == PendingDomain::Thread {
                executor.defer_signal_delivery(event).unwrap();
            } else {
                assert!(matches!(
                    executor.queue_process_alarm_signal(event),
                    reverie::ProcessAlarmSignalOutcome::Accepted(_)
                ));
            }
            match consumer {
                SignalConsumer::ReturnToUser => assert_eq!(
                    executor
                        .take_pending_signal_for_delivery()
                        .unwrap()
                        .unwrap()
                        .event,
                    event
                ),
                SignalConsumer::SignalFd => assert_eq!(
                    signalfd_read(
                        &mut memory,
                        &mut executor.state,
                        fd,
                        PAGE_SIZE - 1,
                        SIGNALFD_RECORD_SIZE
                    ),
                    Some(negative_errno(libc::EFAULT))
                ),
                SignalConsumer::SignalTimedWait => assert_eq!(
                    rt_sigtimedwait(
                        &mut memory,
                        &mut executor.state,
                        &[0x80, PAGE_SIZE - 1, 0, 8, 0, 0]
                    ),
                    negative_errno(libc::EFAULT)
                ),
            }
            let effect = executor.signal_dequeue_front().unwrap();
            assert_eq!(
                effect,
                reverie::SignalDequeue {
                    process: identity.process,
                    sequence: 1,
                    consumer,
                    domain,
                    event
                }
            );
            assert!(!executor.has_eligible_pending_signal());
            let mut conflicting = effect;
            conflicting.event =
                event_for_thread(libc::SIGUSR1, executor.state.pid, executor.state.tid).unwrap();
            assert_eq!(
                executor.acknowledge_signal_dequeue(conflicting),
                Err(reverie::syscalls::Errno::EINVAL)
            );
            assert_eq!(executor.signal_dequeue_front(), Some(effect));
            executor.acknowledge_signal_dequeue(effect).unwrap();
            executor.acknowledge_signal_dequeue(effect).unwrap();
            assert_eq!(executor.signal_dequeue_front(), None);
            assert_eq!(
                executor
                    .state
                    .process_signals
                    .lock()
                    .unwrap()
                    .dequeue_sequence,
                1
            );
        }
    }
}

#[test]
fn signal_dequeue_readiness_error_retains_complete_removal() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), false);
    executor.enable_signal_dequeues();
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    let fd = process_alarm_signalfd(&mut executor, &mut memory);
    let event = process_alarm_event(executor.state.pid);
    assert!(matches!(
        executor.queue_process_alarm_signal(event),
        reverie::ProcessAlarmSignalOutcome::Accepted(_)
    ));
    executor.state.files.insert(
        fd,
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .unwrap(),
    );
    assert_eq!(
        executor.take_pending_signal_for_delivery(),
        Err(reverie::syscalls::Errno::EBADF)
    );
    let effect = executor.signal_dequeue_front().unwrap();
    assert_eq!(effect.event, event);
    assert_eq!(effect.domain, reverie::PendingDomain::Process);
    assert!(!executor.has_eligible_pending_signal());
}

#[test]
fn signal_dequeue_sequence_exhaustion_refuses_before_removal() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), false);
    executor.enable_signal_dequeues();
    let event = process_alarm_event(executor.state.pid);
    assert!(matches!(
        executor.queue_process_alarm_signal(event),
        reverie::ProcessAlarmSignalOutcome::Accepted(_)
    ));
    executor
        .state
        .process_signals
        .lock()
        .unwrap()
        .dequeue_sequence = u64::MAX;
    let before = process_alarm_snapshot(&executor);
    assert_eq!(
        executor.take_pending_signal_for_delivery(),
        Err(reverie::syscalls::Errno::EOVERFLOW)
    );
    assert_eq!(process_alarm_snapshot(&executor), before);
    assert_eq!(executor.signal_dequeue_front(), None);
}

#[test]
fn signal_dequeue_fork_resets_and_exec_retains_process_sequence() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), false);
    executor.enable_signal_dequeues();
    let event = process_alarm_event(executor.state.pid);
    assert!(matches!(
        executor.queue_process_alarm_signal(event),
        reverie::ProcessAlarmSignalOutcome::Accepted(_)
    ));
    executor
        .take_pending_signal_for_delivery()
        .unwrap()
        .unwrap();
    let effect = executor.signal_dequeue_front().unwrap();
    executor.acknowledge_signal_dequeue(effect).unwrap();
    let process = executor.state.process_signals.lock().unwrap();
    let fork = process.for_fork();
    let exec = process.after_exec();
    assert!(fork.dequeue_enabled && exec.dequeue_enabled);
    assert_eq!(fork.dequeue_sequence, 0);
    assert!(fork.dequeue_journal.is_empty());
    assert_eq!(fork.dequeue_acknowledged, None);
    assert_eq!(exec.dequeue_sequence, 1);
    assert!(exec.dequeue_journal.is_empty());
    assert_eq!(exec.dequeue_acknowledged, Some(effect));
}

#[test]
fn signal_dequeue_reused_task_refuses_before_all_three_removals() {
    use reverie::SignalConsumer;
    let root = TestDir::new();
    for consumer in [
        SignalConsumer::ReturnToUser,
        SignalConsumer::SignalFd,
        SignalConsumer::SignalTimedWait,
    ] {
        let mut executor = ElfExecutor::new(test_state(&root.0), false);
        executor.enable_signal_dequeues();
        let original = executor.signal_task_identity().unwrap();
        let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
        let fd = process_alarm_signalfd(&mut executor, &mut memory);
        let event = process_alarm_event(executor.state.pid);
        assert!(matches!(
            executor.queue_process_alarm_signal(event),
            reverie::ProcessAlarmSignalOutcome::Accepted(_)
        ));
        {
            let mut lifecycle = executor.state.task_lifecycle.lock().unwrap();
            lifecycle.remove(executor.state.tid, executor.task_generation);
            let generation = lifecycle.register(
                executor.state.tid,
                executor.state.pid,
                executor.state.pgid,
                true,
            );
            assert_ne!(generation, original.task_generation);
        }
        assert!(executor.signal_task_identity().is_none());
        let before = process_alarm_snapshot(&executor);
        let raw = match consumer {
            SignalConsumer::ReturnToUser => executor
                .take_pending_signal_for_delivery()
                .map(|pending| i64::from(pending.is_some()))
                .unwrap_or_else(|errno| -(i64::from(errno.into_raw()))),
            SignalConsumer::SignalFd => signalfd_read(
                &mut memory,
                &mut executor.state,
                fd,
                0x100,
                SIGNALFD_RECORD_SIZE,
            )
            .unwrap(),
            SignalConsumer::SignalTimedWait => {
                rt_sigtimedwait(&mut memory, &mut executor.state, &[0x80, 0x100, 0, 8, 0, 0])
            }
        };
        assert_eq!(
            raw,
            negative_errno(libc::ESRCH),
            "stale {consumer:?} cannot observe a replacement lifetime"
        );
        assert_eq!(process_alarm_snapshot(&executor), before);
    }
}

#[test]
fn signal_dequeue_first_private_nonalarm_is_bound_before_any_parked_call() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), false);
    executor.enable_signal_dequeues();
    let identity = executor.signal_task_identity().unwrap();
    let event = event_for_thread(libc::SIGUSR1, executor.state.pid, executor.state.tid).unwrap();
    executor.defer_signal_delivery(event).unwrap();
    assert_eq!(
        executor
            .take_pending_signal_for_delivery()
            .unwrap()
            .unwrap()
            .event,
        event
    );
    assert_eq!(
        executor.signal_dequeue_front(),
        Some(reverie::SignalDequeue {
            process: identity.process,
            sequence: 1,
            consumer: reverie::SignalConsumer::ReturnToUser,
            domain: reverie::PendingDomain::Thread,
            event,
        })
    );
    let thread = executor.state.thread_signals.lock();
    assert_eq!(thread.dequeue_identity, Some(identity));
    assert_eq!(thread.after_exec().dequeue_identity, Some(identity));
    assert_eq!(thread.for_fork().dequeue_identity, None);
    assert_eq!(thread.for_clone_thread().dequeue_identity, None);
}
