// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-PENDING): https://github.com/rrnewton/reverie/issues/916.

fn pdeath_adopt(executor: &ElfExecutor) -> Arc<crate::failure::RunFailure> {
    executor.backend_signal_control().process.enable_parent_death_control().unwrap();
    let global = Arc::new(());
    let run = crate::failure::RunFailure::new(&global);
    executor.install_signal_control(reverie::BackendSignalControlMode::ToolControlled, &run);
    run
}

fn pdeath_set(executor: &mut ElfExecutor, memory: &GuestMemory, signal: u64) -> i64 {
    executor.execute(&SyscallRequest::new(libc::SYS_prctl as u64,
        [libc::PR_SET_PDEATHSIG as u64, signal, 0, 0, 0, 0]), memory)
}

fn pdeath_permit(executor: &ElfExecutor, sequence: u64) -> reverie::SignalDeliveryPermit {
    let permit = reverie::SignalDeliveryPermit {
        task: executor.admitted_signal_identity(), sequence, site: None,
    };
    executor.backend_signal_control().process.reserve_delivery(permit).unwrap();
    permit
}

fn pdeath_publish_exit(executor: &mut ElfExecutor, group: bool, sequence: u64)
    -> reverie::ParentDeathPublication
{
    let permit = pdeath_permit(executor, sequence);
    let exit = commit_test_exit(executor, 0, group);
    let boundary = reverie::SignalBoundaryReceipt { permit,
        outcome: reverie::SignalBoundaryOutcome::Terminated { group: exit.group, wait_status: exit.status.into_raw() },
    };
    let control = executor.backend_signal_control().process;
    let result = control.publish_parent_death(boundary);
    let reverie::ParentDeathPublicationResult::Committed(receipt) = result else {
        panic!("parent-death publication did not commit: {result:?}");
    };
    executor.check_parent_death_boundary(boundary).unwrap();
    assert_eq!(control.publish_parent_death(boundary),
        reverie::ParentDeathPublicationResult::Committed(receipt.clone()));
    control.release_delivery(permit).unwrap();
    receipt
}

#[test]
fn pdeath_abi_adoption_full_width_and_scalar_copyout_are_exact() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), true);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    assert_eq!(pdeath_set(&mut executor, &memory, libc::SIGUSR1 as u64), -i64::from(libc::ENOSYS));
    let _run = pdeath_adopt(&executor);
    assert_eq!(executor.backend_signal_control().process.enable_parent_death_control(), Err(reverie::syscalls::Errno::EBUSY));
    for bad in [65, u64::MAX, (1_u64 << 32) | libc::SIGUSR1 as u64] {
        assert_eq!(pdeath_set(&mut executor, &memory, bad), -i64::from(libc::EINVAL));
    }
    for unsupported in [libc::SIGKILL, libc::SIGCONT, libc::SIGSTOP, libc::SIGTSTP,
        libc::SIGTTIN, libc::SIGTTOU, 32, 64]
    {
        assert_eq!(pdeath_set(&mut executor, &memory, unsupported as u64), -i64::from(libc::ENOSYS));
    }
    for signal in [libc::SIGUSR1, libc::SIGUSR2, libc::SIGPIPE, libc::SIGCHLD, libc::SIGWINCH] {
        assert_eq!(executor.execute(&SyscallRequest::new(libc::SYS_prctl as u64,
            [(1_u64 << 32) | libc::PR_SET_PDEATHSIG as u64, signal as u64, 11, 22, 33, 0]), &memory), 0);
        memory.write(0x100, &[0xa5; 8]).unwrap();
        assert_eq!(executor.execute(&SyscallRequest::new(libc::SYS_prctl as u64,
            [(1_u64 << 32) | libc::PR_GET_PDEATHSIG as u64, 0x100, 11, 22, 33, 0]), &memory), 0);
        let mut bytes = [0; 8]; memory.read(0x100, &mut bytes).unwrap();
        assert_eq!(&bytes[..4], &signal.to_ne_bytes());
        assert_eq!(&bytes[4..], &[0xa5; 4]);
        assert_eq!(executor.execute(&SyscallRequest::new(libc::SYS_prctl as u64,
            [libc::PR_GET_PDEATHSIG as u64, PAGE_SIZE - 2, 0, 0, 0, 0]), &memory), -i64::from(libc::EFAULT));
        assert_eq!(executor.state.task_lifecycle.lock().unwrap().parent_death_signal(1), Ok(signal));
    }
    assert_eq!(pdeath_set(&mut executor, &memory, 0), 0);
    assert!(executor.parent_death_enrolled(), "clear cannot discard old pending delivery obligations");
}

#[test]
fn pdeath_old_controlled_consumer_and_uncontrolled_mode_refuse_before_enrollment() {
    for mode in [reverie::BackendSignalControlMode::Unchanged, reverie::BackendSignalControlMode::ToolControlled] {
        let root = TestDir::new();
        let mut executor = ElfExecutor::new(test_state(&root.0), true);
        let memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
        let global = Arc::new(()); let run = crate::failure::RunFailure::new(&global);
        executor.install_signal_control(mode, &run);
        assert_eq!(pdeath_set(&mut executor, &memory, libc::SIGUSR1 as u64), -i64::from(libc::ENOSYS));
        assert!(!executor.parent_death_enrolled());
        assert_eq!(pdeath_set(&mut executor, &memory, 0), 0);
    }
}

#[test]
fn pdeath_creator_thread_exit_publishes_process_si_user_once() {
    let root = TestDir::new();
    let leader = ElfExecutor::new(test_state(&root.0), true);
    let _run = pdeath_adopt(&leader);
    let mut creator = leader.thread_child(2).unwrap();
    let mut child = creator.fork_child(3, false, false).unwrap();
    let memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    assert_eq!(pdeath_set(&mut child, &memory, libc::SIGUSR1 as u64), 0);
    child.state.thread_signals.lock().blocked.insert(libc::SIGUSR1);
    let receipt = pdeath_publish_exit(&mut creator, false, 1);
    assert_eq!(receipt.signals.len(), 1);
    assert_eq!(receipt.signals[0].process, child.admitted_signal_identity().process);
    assert_eq!(receipt.signals[0].signal, libc::SIGUSR1);
    assert!(!receipt.signals[0].coalesced);
    assert!(!receipt.signals[0].discarded);
    assert!(leader.state.task_lifecycle.lock().unwrap().get(1).is_some(), "parent process remains live");
    assert_eq!(child.state.task_lifecycle.lock().unwrap().get(3).unwrap().real_parent,
        Some(leader.admitted_signal_identity()));
    assert!(child.state.thread_signals.lock().pending.is_empty(), "process-directed is not thread-private");
    assert!(child.take_pending_signal_for_delivery().unwrap().is_none(), "no scheduler permit");
    let permit = pdeath_permit(&child, 2);
    assert!(child.take_pending_signal_for_delivery().unwrap().is_none(), "blocked remains pending");
    child.state.thread_signals.lock().blocked.remove(libc::SIGUSR1);
    let pending = child.take_pending_signal_for_delivery().unwrap().expect("unblocked shared event");
    assert_eq!(pending.domain, PendingSignalDomain::Process);
    assert_eq!(pending.event.signal(), libc::SIGUSR1);
    let info = pending.event.siginfo();
    assert_eq!(i32::from_ne_bytes(info[8..12].try_into().unwrap()), libc::SI_USER);
    assert_eq!(i32::from_ne_bytes(info[16..20].try_into().unwrap()), 1, "sender TGID, never creator TID 2");
    assert_eq!(u32::from_ne_bytes(info[20..24].try_into().unwrap()), 0);
    assert!(child.take_pending_signal_for_delivery().unwrap().is_none());
    child.backend_signal_control().process.release_delivery(permit).unwrap();
    // A later physical cleanup cannot invent another death event.
    creator.retire_current_thread(ExitStatus::SUCCESS, false);
    assert!(child.take_pending_signal_for_delivery().unwrap().is_none());
}

#[test]
fn pdeath_registration_reset_sticky_domain_and_exec_capability_gain() {
    let root = TestDir::new();
    let mut leader = ElfExecutor::new(test_state(&root.0), true);
    let _run = pdeath_adopt(&leader);
    let sibling = leader.thread_child(2).unwrap();
    let memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    assert_eq!(pdeath_set(&mut leader, &memory, libc::SIGUSR1 as u64), -i64::from(libc::ENOSYS));
    assert!(!leader.parent_death_enrolled());
    drop(sibling);
    assert_eq!(pdeath_set(&mut leader, &memory, libc::SIGUSR1 as u64), 0);
    let child = leader.fork_child(3, false, false).unwrap();
    assert_eq!(child.state.task_lifecycle.lock().unwrap().parent_death_signal(3), Ok(0));
    assert!(!child.parent_death_enrolled());
    assert!(leader.thread_child(4).is_err());
    assert_eq!(pdeath_set(&mut leader, &memory, 0), 0);
    assert!(leader.thread_child(4).is_err(), "clear does not revoke already pending obligations");
    assert_eq!(pdeath_set(&mut leader, &memory, libc::SIGUSR2 as u64), 0);
    leader.replace_after_exec(test_state(&root.0));
    assert_eq!(leader.state.task_lifecycle.lock().unwrap().parent_death_signal(1), Ok(libc::SIGUSR2));
    assert!(leader.parent_death_enrolled());
    leader.state.capability_permitted = 0;
    leader.state.capability_effective = 0;
    leader.replace_after_exec(test_state(&root.0));
    assert_eq!(leader.state.task_lifecycle.lock().unwrap().parent_death_signal(1), Ok(0));
    assert!(leader.parent_death_enrolled());
}

#[test]
fn pdeath_late_clear_preserves_frozen_event_and_disposition_generation_discards_it() {
    for discard_generation in [false, true] {
        let root = TestDir::new();
        let leader = ElfExecutor::new(test_state(&root.0), true);
        let _run = pdeath_adopt(&leader);
        let mut creator = leader.thread_child(2).unwrap();
        let mut child = creator.fork_child(3, false, false).unwrap();
        let memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
        assert_eq!(pdeath_set(&mut child, &memory, libc::SIGUSR1 as u64), 0);
        let permit = pdeath_permit(&creator, 1);
        commit_test_exit(&mut creator, 0, false);
        assert_eq!(pdeath_set(&mut child, &memory, 0), 0);
        if discard_generation {
            child.state.process_signals.lock().unwrap().advance_pending_generation(libc::SIGUSR1).unwrap();
        }
        let boundary = reverie::SignalBoundaryReceipt { permit, outcome:
            reverie::SignalBoundaryOutcome::Terminated { group: false, wait_status: 0 } };
        let result = creator.backend_signal_control().process.publish_parent_death(boundary);
        let reverie::ParentDeathPublicationResult::Committed(receipt) = result else { panic!("{result:?}"); };
        assert_eq!(receipt.signals.len(), 1);
        assert_eq!(receipt.signals[0].discarded, discard_generation);
        let process = child.state.process_signals.lock().unwrap();
        assert_eq!(process.shared_pending.pending_mask(&process.pending_generations).contains(libc::SIGUSR1), !discard_generation);
    }
}

#[test]
fn pdeath_pipe_chld_ignore_and_standard_coalescing_preserve_first_siginfo() {
    for signal in [libc::SIGPIPE, libc::SIGCHLD, libc::SIGWINCH, libc::SIGUSR1] {
        for ignored in [false, true] {
            let root = TestDir::new();
            let leader = ElfExecutor::new(test_state(&root.0), true);
            let _run = pdeath_adopt(&leader);
            let mut creator = leader.thread_child(2).unwrap();
            let mut child = creator.fork_child(3, false, false).unwrap();
            let memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
            assert_eq!(pdeath_set(&mut child, &memory, signal as u64), 0);
            if ignored {
                child.state.process_signals.lock().unwrap().dispositions.insert(signal,
                    KernelSigaction { handler: libc::SIG_IGN as u64, ..KernelSigaction::default() });
            } else {
                let first = event_for_process(signal, child.state.pid).unwrap();
                let mut process = child.state.process_signals.lock().unwrap();
                let generation = process.pending_generation(signal);
                process.shared_pending.enqueue(first, generation).unwrap();
            }
            let receipt = pdeath_publish_exit(&mut creator, false, 1);
            assert_eq!(receipt.signals.len(), 1);
            assert_eq!(receipt.signals[0].discarded, ignored);
            assert_eq!(receipt.signals[0].coalesced, !ignored);
            if !ignored {
                let permit = pdeath_permit(&child, 2);
                let pending = child.take_pending_signal_for_delivery().unwrap().unwrap();
                assert_eq!(i32::from_ne_bytes(pending.event.siginfo()[16..20].try_into().unwrap()), 3,
                    "coalescing retains the earlier sender, not parent TGID1");
                child.backend_signal_control().process.release_delivery(permit).unwrap();
            }
        }
    }
}

#[test]
fn pdeath_exec_uses_image_boundary_and_cleanup_never_impersonates_it() {
    let root = TestDir::new();
    let mut leader = ElfExecutor::new(test_state(&root.0), true);
    let _run = pdeath_adopt(&leader);
    let mut creator = leader.thread_child(2).unwrap();
    let mut child = creator.fork_child(3, false, false).unwrap();
    let memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    assert_eq!(pdeath_set(&mut child, &memory, libc::SIGUSR1 as u64), 0);
    let permit = pdeath_permit(&leader, 1);
    leader.prepare_parent_death_exec().unwrap();
    creator.retire_current_thread(ExitStatus::SUCCESS, false);
    assert!(!child.has_eligible_pending_signal(), "cleanup is not publication authority");
    leader.replace_after_exec(test_state(&root.0));
    let boundary = reverie::SignalBoundaryReceipt { permit, outcome: reverie::SignalBoundaryOutcome::ImageReplaced };
    let wrong = reverie::SignalBoundaryReceipt { outcome:
        reverie::SignalBoundaryOutcome::Terminated { group: false, wait_status: 0 }, ..boundary };
    // A mismatched outcome must not consume or publish the image batch.
    let wrong_result = leader.backend_signal_control().process.publish_parent_death(wrong);
    assert!(matches!(wrong_result, reverie::ParentDeathPublicationResult::RejectedBeforeCommit(_)));
    assert!(!child.has_eligible_pending_signal());
    let result = leader.backend_signal_control().process.publish_parent_death(boundary);
    let reverie::ParentDeathPublicationResult::Committed(receipt) = result else { panic!("{result:?}"); };
    assert_eq!(receipt.signals.len(), 1);
    assert!(child.has_eligible_pending_signal());
    leader.check_parent_death_boundary(boundary).unwrap();
}

#[test]
fn pdeath_enrolled_raw_waits_refuse_before_effect_and_capture_remains_supported() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), true);
    let _run = pdeath_adopt(&executor);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    assert_eq!(pdeath_set(&mut executor, &memory, libc::SIGUSR1 as u64), 0);
    memory.write(0x100, &[0x7b; 32]).unwrap();
    for request in [
        SyscallRequest::new(libc::SYS_futex as u64, [0x100, libc::FUTEX_WAIT as u64, 0, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_fcntl as u64, [1, libc::F_SETLKW as u64, 0x100, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_nanosleep as u64, [0x100, 0x108, 0, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_read as u64, [0, 0x100, 0, 0, 0, 0]),
    ] {
        // Install an actual nonregular stdin without relying on ambient stdin.
        if request.number() == libc::SYS_read as u64 {
            let (read, _write) = std::os::unix::net::UnixStream::pair().unwrap();
            use std::os::fd::{FromRawFd, IntoRawFd};
            // SAFETY: into_raw_fd transfers the sole owned descriptor to File.
            executor.file_table.lock().unwrap().stdin = Some(unsafe { std::fs::File::from_raw_fd(read.into_raw_fd()) });
        }
        assert!(matches!(executor.execute_checked(&request, &memory), Err(crate::Error::ParentDeathSignal { errno: libc::ENOSYS, .. })));
        let mut bytes = [0; 32]; memory.read(0x100, &mut bytes).unwrap();
        assert_eq!(bytes, [0x7b; 32]);
    }
    assert_eq!(executor.execute(&SyscallRequest::new(libc::SYS_write as u64, [1, 0x100, 4, 0, 0, 0]), &memory), 4);
    let (stdout, stderr) = executor.take_output();
    assert_eq!(stdout, [0x7b; 4]); assert!(stderr.is_empty());
}
