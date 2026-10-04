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
        assert_eq!(executor.state.task_lifecycle.lock().unwrap().parent_death_signal(executor.admitted_signal_identity()), Ok(signal));
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
    assert_eq!(child.state.task_lifecycle.lock().unwrap().parent_death_signal(child.admitted_signal_identity()), Ok(0));
    assert!(!child.parent_death_enrolled());
    assert!(leader.thread_child(4).is_err());
    assert_eq!(pdeath_set(&mut leader, &memory, 0), 0);
    assert!(leader.thread_child(4).is_err(), "clear does not revoke already pending obligations");
    assert_eq!(pdeath_set(&mut leader, &memory, libc::SIGUSR2 as u64), 0);
    leader.replace_after_exec(test_state(&root.0));
    assert_eq!(leader.state.task_lifecycle.lock().unwrap().parent_death_signal(leader.admitted_signal_identity()), Ok(libc::SIGUSR2));
    assert!(leader.parent_death_enrolled());
    leader.state.capability_permitted = 0;
    leader.state.capability_effective = 0;
    leader.replace_after_exec(test_state(&root.0));
    assert_eq!(leader.state.task_lifecycle.lock().unwrap().parent_death_signal(leader.admitted_signal_identity()), Ok(0));
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
                process.dispositions.insert(signal,
                    KernelSigaction { handler: 0x4000, ..KernelSigaction::default() });
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

fn pdeath_action(executor: &mut ElfExecutor, memory: &mut GuestMemory, signal: i32, handler: u64) {
    memory.write(0x200, &KernelSigaction { handler, ..KernelSigaction::default() }.encode()).unwrap();
    assert_eq!(executor.execute(&SyscallRequest::new(libc::SYS_rt_sigaction as u64,
        [signal as u64, 0x200, 0, KERNEL_SIGSET_SIZE as u64, 0, 0]), memory), 0);
}

fn pdeath_mask(executor: &mut ElfExecutor, memory: &mut GuestMemory, signal: i32, how: i32) {
    let mut mask = KernelSigset::default(); mask.insert(signal);
    memory.write(0x100, &mask.to_bytes()).unwrap();
    assert_eq!(executor.execute(&SyscallRequest::new(libc::SYS_rt_sigprocmask as u64,
        [how as u64, 0x100, 0, KERNEL_SIGSET_SIZE as u64, 0, 0]), memory), 0);
}

#[test]
fn pdeath_ignored_generation_obeys_mask_observer_and_real_disposition_transitions() {
    // kernel/signal.c sig_ignored: blocked or observed signals are retained;
    // both SIG_IGN and an unobserved default-ignore action discard otherwise.
    for explicit in [false, true] {
        for blocked in [false, true] {
            for observed in [false, true] {
                for handler_before_publication in [false, true] {
                    let root = TestDir::new();
                    let leader = ElfExecutor::new(test_state(&root.0), true);
                    let _run = pdeath_adopt(&leader);
                    let mut creator = leader.thread_child(2).unwrap();
                    let mut child = creator.fork_child(3, false, false).unwrap();
                    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
                    let signal = if explicit { libc::SIGUSR1 } else { libc::SIGWINCH };
                    pdeath_action(&mut child, &mut memory, signal,
                        if explicit { libc::SIG_IGN as u64 } else { libc::SIG_DFL as u64 });
                    if blocked { pdeath_mask(&mut child, &mut memory, signal, libc::SIG_BLOCK); }
                    if observed { child.observe_ignored_signals_with_tool(); }
                    assert_eq!(pdeath_set(&mut child, &memory, signal as u64), 0);
                    let permit = pdeath_permit(&creator, 1);
                    let exit = commit_test_exit(&mut creator, 0, false);
                    if handler_before_publication { pdeath_action(&mut child, &mut memory, signal, 0x4000); }
                    let boundary = reverie::SignalBoundaryReceipt { permit, outcome:
                        reverie::SignalBoundaryOutcome::Terminated { group: false, wait_status: exit.status.into_raw() } };
                    let result = creator.backend_signal_control().process.publish_parent_death(boundary);
                    let reverie::ParentDeathPublicationResult::Committed(receipt) = result else { panic!("{result:?}"); };
                    let retained = blocked || observed;
                    assert_eq!(receipt.signals.len(), 1);
                    assert_eq!(receipt.signals[0].discarded, !retained,
                        "explicit={explicit} blocked={blocked} observed={observed} before={handler_before_publication}");
                    if !handler_before_publication { pdeath_action(&mut child, &mut memory, signal, 0x4000); }
                    if blocked { pdeath_mask(&mut child, &mut memory, signal, libc::SIG_UNBLOCK); }
                    let child_permit = pdeath_permit(&child, 2);
                    let pending = child.take_pending_signal_for_delivery().unwrap();
                    assert_eq!(pending.is_some(), retained);
                    if let Some(pending) = pending {
                        assert_eq!(pending.domain, PendingSignalDomain::Process);
                        assert_eq!(pending.event.signal(), signal);
                        assert_eq!(i32::from_ne_bytes(pending.event.siginfo()[8..12].try_into().unwrap()), libc::SI_USER);
                        assert_eq!(i32::from_ne_bytes(pending.event.siginfo()[16..20].try_into().unwrap()), 1);
                    }
                    child.backend_signal_control().process.release_delivery(child_permit).unwrap();
                    creator.check_parent_death_boundary(boundary).unwrap();
                }
            }
        }
    }
}

#[test]
fn pdeath_real_ignore_transition_invalidates_only_the_frozen_generation() {
    let root = TestDir::new();
    let leader = ElfExecutor::new(test_state(&root.0), true);
    let _run = pdeath_adopt(&leader);
    let mut creator = leader.thread_child(2).unwrap();
    let mut child = creator.fork_child(3, false, false).unwrap();
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    pdeath_mask(&mut child, &mut memory, libc::SIGUSR1, libc::SIG_BLOCK);
    assert_eq!(pdeath_set(&mut child, &memory, libc::SIGUSR1 as u64), 0);
    let permit = pdeath_permit(&creator, 1);
    commit_test_exit(&mut creator, 0, false);
    pdeath_action(&mut child, &mut memory, libc::SIGUSR1, libc::SIG_IGN as u64);
    pdeath_action(&mut child, &mut memory, libc::SIGUSR1, 0x4000);
    let boundary = reverie::SignalBoundaryReceipt { permit, outcome:
        reverie::SignalBoundaryOutcome::Terminated { group: false, wait_status: 0 } };
    let result = creator.backend_signal_control().process.publish_parent_death(boundary);
    let reverie::ParentDeathPublicationResult::Committed(receipt) = result else { panic!("{result:?}"); };
    assert_eq!(receipt.signals.len(), 1);
    assert!(receipt.signals[0].discarded, "a later handler cannot resurrect an old generation");
    let signals = child.state.process_signals.lock().unwrap();
    assert!(!signals.shared_pending.pending_mask(&signals.pending_generations).contains(libc::SIGUSR1));
}

#[test]
fn pdeath_reparented_sibling_death_repeats_but_reused_creator_tid_does_not() {
    let root = TestDir::new();
    let mut leader = ElfExecutor::new(test_state(&root.0), true);
    let _run = pdeath_adopt(&leader);
    let mut creator = leader.thread_child(2).unwrap();
    let survivor = leader.thread_child(4).unwrap();
    let mut child = creator.fork_child(3, false, false).unwrap();
    let memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    assert_eq!(pdeath_set(&mut child, &memory, libc::SIGUSR1 as u64), 0);
    assert_eq!(pdeath_publish_exit(&mut creator, false, 1).signals.len(), 1);
    let child_permit = pdeath_permit(&child, 2);
    assert!(child.take_pending_signal_for_delivery().unwrap().is_some());
    child.backend_signal_control().process.release_delivery(child_permit).unwrap();
    let mut replacement = leader.thread_child(2).unwrap();
    assert_ne!(replacement.admitted_signal_identity(), creator.admitted_signal_identity());
    assert!(pdeath_publish_exit(&mut replacement, false, 3).signals.is_empty());
    assert!(!child.has_eligible_pending_signal());
    let second = pdeath_publish_exit(&mut leader, false, 4);
    assert_eq!(second.signals.len(), 1, "second actual parent thread death is a new event");
    assert!(!second.signals[0].coalesced);
    assert_eq!(child.state.task_lifecycle.lock().unwrap().get(3).unwrap().real_parent,
        Some(survivor.admitted_signal_identity()));
    let child_permit = pdeath_permit(&child, 5);
    let pending = child.take_pending_signal_for_delivery().unwrap().unwrap();
    assert_eq!(i32::from_ne_bytes(pending.event.siginfo()[16..20].try_into().unwrap()), 1);
    child.backend_signal_control().process.release_delivery(child_permit).unwrap();
}

#[test]
fn pdeath_stale_registration_get_and_frozen_receiver_do_not_target_reused_pid() {
    let root = TestDir::new();
    let leader = ElfExecutor::new(test_state(&root.0), true);
    let _run = pdeath_adopt(&leader);
    let mut creator = leader.thread_child(2).unwrap();
    let mut child = creator.fork_child(3, false, false).unwrap();
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    assert_eq!(pdeath_set(&mut child, &memory, libc::SIGUSR1 as u64), 0);
    let stale = child.admitted_signal_identity();
    let permit = pdeath_permit(&creator, 1);
    commit_test_exit(&mut creator, 0, false);
    child.retire_current_thread(ExitStatus::SUCCESS, false);
    let replacement = leader.fork_child(3, false, false).unwrap();
    assert_ne!(stale, replacement.admitted_signal_identity());
    for raw in [0, libc::SIGUSR1 as u64] {
        assert_eq!(child.state.task_lifecycle.lock().unwrap().set_parent_death_signal(stale, raw), Err(reverie::syscalls::Errno::ESRCH));
    }
    assert_eq!(child.state.task_lifecycle.lock().unwrap().set_parent_death_signal(stale, u64::MAX), Err(reverie::syscalls::Errno::EINVAL));
    memory.write(0x100, &[0x5a; 8]).unwrap();
    assert_eq!(super::prctl(&mut memory, &mut child.state,
        &[libc::PR_GET_PDEATHSIG as u64, 0x100, 0, 0, 0, 0]), -i64::from(libc::ESRCH));
    let mut bytes = [0; 8]; memory.read(0x100, &mut bytes).unwrap(); assert_eq!(bytes, [0x5a; 8]);
    let boundary = reverie::SignalBoundaryReceipt { permit, outcome:
        reverie::SignalBoundaryOutcome::Terminated { group: false, wait_status: 0 } };
    let result = creator.backend_signal_control().process.publish_parent_death(boundary);
    let reverie::ParentDeathPublicationResult::Committed(receipt) = result else { panic!("{result:?}"); };
    assert_eq!(receipt.signals.len(), 1); assert!(receipt.signals[0].discarded);
    assert!(!replacement.has_eligible_pending_signal());
    assert!(!replacement.parent_death_enrolled());
}

#[test]
fn pdeath_publication_failure_retains_exact_prefix_and_survives_sender_cleanup() {
    let root = TestDir::new();
    let leader = ElfExecutor::new(test_state(&root.0), true);
    let _run = pdeath_adopt(&leader);
    let mut creator = leader.thread_child(2).unwrap();
    let mut first = creator.fork_child(3, false, false).unwrap();
    let mut second = creator.fork_child(4, false, false).unwrap();
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    let mut mask = KernelSigset::default(); mask.insert(libc::SIGUSR1);
    memory.write(0x100, &mask.to_bytes()).unwrap();
    let fd = first.execute(&SyscallRequest::new(libc::SYS_signalfd4 as u64,
        [u64::MAX, 0x100, KERNEL_SIGSET_SIZE as u64, libc::SFD_NONBLOCK as u64, 0, 0]), &memory);
    assert!(fd >= 3, "real signalfd setup failed: {fd}");
    // This real carrier write gets EBADF only AFTER the shared queue insertion.
    let retained_carrier = first.state.process_signals.lock().unwrap().signalfd_carriers.insert(fd as i32,
        crate::signal::SignalFdCarrier::pin_eventfd(&std::fs::File::open("/dev/null").unwrap()).unwrap()).unwrap();
    assert_eq!(pdeath_set(&mut first, &memory, libc::SIGUSR1 as u64), 0);
    assert_eq!(pdeath_set(&mut second, &memory, libc::SIGUSR1 as u64), 0);
    let permit = pdeath_permit(&creator, 1);
    commit_test_exit(&mut creator, 0, false);
    let boundary = reverie::SignalBoundaryReceipt { permit, outcome:
        reverie::SignalBoundaryOutcome::Terminated { group: false, wait_status: 0 } };
    let control = creator.backend_signal_control().process;
    let result = control.publish_parent_death(boundary);
    let reverie::ParentDeathPublicationResult::FailedAfterCommit { receipt, errno } = &result else { panic!("{result:?}"); };
    assert_eq!(*errno, reverie::syscalls::Errno::EBADF);
    assert_eq!(receipt.batches.len(), 1);
    assert_eq!(receipt.signals.len(), 1);
    assert_eq!(receipt.signals[0].process, first.admitted_signal_identity().process);
    assert!(!receipt.signals[0].discarded);
    assert!(first.has_eligible_pending_signal());
    assert!(!second.has_eligible_pending_signal(), "publication stops at the exact failed prefix");
    let mut forged = receipt.clone(); forged.signals.clear();
    assert_eq!(control.finish_parent_death_failure(&forged), Err(reverie::syscalls::Errno::EINVAL));
    let mut duplicate = receipt.clone(); duplicate.batches.extend_from_within(..);
    assert_eq!(control.finish_parent_death_failure(&duplicate), Err(reverie::syscalls::Errno::EINVAL));
    control.release_delivery(permit).unwrap();
    drop(creator);
    drop(leader);
    assert_eq!(control.publish_parent_death(boundary), result,
        "a retained committed prefix does not depend on sender registry lifetime");
    assert!(!second.has_eligible_pending_signal());
    first.state.process_signals.lock().unwrap().signalfd_carriers.insert(fd as i32, retained_carrier);
}

#[test]
fn pdeath_original_preflight_is_exact_uncached_and_preserves_zero_readiness() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), true);
    let _run = pdeath_adopt(&executor);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    executor.bind_address_space(&memory);
    let call = SyscallRequest::new(libc::SYS_getuid as u64, [1, 2, 3, 4, 5, 6]);
    let old = executor.begin_signal_callback().unwrap();
    assert_eq!(executor.parent_death_original_syscall_preflight(old, &call, None, false).unwrap(),
        reverie::ParentDeathSyscallAdmission::Unenrolled);
    let site = executor.begin_signal_callback().unwrap();
    assert!(executor.parent_death_original_syscall_preflight(old, &call, None, false).is_err());
    assert_eq!(pdeath_set(&mut executor, &memory, libc::SIGUSR1 as u64), 0);
    assert!(executor.parent_death_original_syscall_preflight(site, &call, None, false).is_err());
    let mut changed = *call.args(); changed[5] ^= 1_u64 << 32;
    let changed = SyscallRequest::new(call.number(), changed);
    assert!(executor.parent_death_original_syscall_preflight(site, &changed, Some(call), false).is_err());
    assert_eq!(executor.parent_death_original_syscall_preflight(site, &call, Some(call), false).unwrap(),
        reverie::ParentDeathSyscallAdmission::Admitted);
    let wrong = reverie::CallbackSignalSite { task_generation: site.task_generation + 1, ..site };
    assert!(executor.parent_death_original_syscall_preflight(wrong, &call, Some(call), false).is_err());
    memory.write(0x100, &[0; 16]).unwrap();
    for request in [
        SyscallRequest::new(libc::SYS_poll as u64, [0, 0, 0, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_pselect6 as u64, [0, 0, 0, 0, 0x100, 0]),
        SyscallRequest::new(libc::SYS_wait4 as u64, [1, 0, libc::WNOHANG as u64, 0, 0, 0]),
    ] {
        assert_eq!(executor.parent_death_original_syscall_preflight(site, &request, Some(request), false).unwrap(),
            reverie::ParentDeathSyscallAdmission::Admitted);
    }
    for request in [
        SyscallRequest::new(libc::SYS_openat as u64, [libc::AT_FDCWD as u64, 0x100, 0, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_recvmmsg as u64, [3, 0x100, 1, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_poll as u64, [0, 0, 1, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_pselect6 as u64, [0, 0, 0, 0, 0, 0]),
    ] {
        assert!(matches!(executor.parent_death_original_syscall_preflight(site, &request, Some(request), false),
            Err(crate::Error::ParentDeathSignal { errno: libc::ENOSYS, .. })));
    }
    let pause = SyscallRequest::new(libc::SYS_pause as u64, [0; 6]);
    assert!(executor.parent_death_original_syscall_preflight(site, &pause, Some(pause), false).is_err());
    assert_eq!(executor.parent_death_original_syscall_preflight(site, &pause, Some(pause), true).unwrap(),
        reverie::ParentDeathSyscallAdmission::Admitted);
    assert!(executor.parent_death_injection_preflight(&pause).is_err(), "original admission is not injection authority");
    let mut bytes = [1; 16]; memory.read(0x100, &mut bytes).unwrap(); assert_eq!(bytes, [0; 16]);
}

#[test]
fn pdeath_sendfile_checks_both_owned_endpoints_before_offset_or_output_changes() {
    use std::os::fd::{FromRawFd, IntoRawFd};
    let root = TestDir::new();
    let input_path = root.0.join("pdeath-sendfile-in");
    let output_path = root.0.join("pdeath-sendfile-out");
    std::fs::write(&input_path, b"source").unwrap();
    std::fs::write(&output_path, b"guard!").unwrap();
    let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut state = test_state(&root.0);
    state.files.insert(3, std::fs::File::open(&input_path).unwrap());
    state.files.insert(4, std::fs::OpenOptions::new().read(true).write(true).open(&output_path).unwrap());
    // SAFETY: ownership transfers exactly once from each UnixStream to File.
    state.files.insert(5, unsafe { std::fs::File::from_raw_fd(reader.into_raw_fd()) });
    state.files.insert(6, unsafe { std::fs::File::from_raw_fd(writer.into_raw_fd()) });
    state.stdout_alias_fds.insert(6);
    let mut executor = ElfExecutor::new(state, true);
    let _run = pdeath_adopt(&executor);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    assert_eq!(pdeath_set(&mut executor, &memory, libc::SIGUSR1 as u64), 0);
    memory.write(0x100, &0_i64.to_ne_bytes()).unwrap();
    for (output, input) in [(6, 3), (4, 5)] {
        let request = SyscallRequest::new(libc::SYS_sendfile as u64, [output, input, 0x100, 1, 0, 0]);
        assert!(matches!(executor.execute_checked(&request, &memory),
            Err(crate::Error::ParentDeathSignal { errno: libc::ENOSYS, .. })));
        let mut offset = [1; 8]; memory.read(0x100, &mut offset).unwrap(); assert_eq!(offset, [0; 8]);
        assert_eq!(std::fs::read(&output_path).unwrap(), b"guard!");
        assert_eq!(executor.take_output(), (Vec::new(), Vec::new()));
    }
    assert_eq!(executor.execute(&SyscallRequest::new(libc::SYS_sendfile as u64,
        [4, 3, 0x100, 6, 0, 0]), &memory), 6);
    assert_eq!(std::fs::read(&output_path).unwrap(), b"source");
    memory.write(0x100, &0_i64.to_ne_bytes()).unwrap();
    assert_eq!(executor.execute(&SyscallRequest::new(libc::SYS_sendfile as u64,
        [1, 3, 0x100, 6, 0, 0]), &memory), 6);
    assert_eq!(executor.take_output(), (b"source".to_vec(), Vec::new()));
}


fn pdeath_static_image() -> Vec<u8> {
    // Genuine minimal x86-64 ELF, not a pathname or magic-prefix permission.
    let mut image = vec![0; 0x1002];
    image[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    image[16..18].copy_from_slice(&2_u16.to_le_bytes());
    image[18..20].copy_from_slice(&62_u16.to_le_bytes());
    image[20..24].copy_from_slice(&1_u32.to_le_bytes());
    image[24..32].copy_from_slice(&0x20_0000_u64.to_le_bytes());
    image[32..40].copy_from_slice(&64_u64.to_le_bytes());
    image[52..54].copy_from_slice(&64_u16.to_le_bytes());
    image[54..56].copy_from_slice(&56_u16.to_le_bytes());
    image[56..58].copy_from_slice(&1_u16.to_le_bytes());
    image[64..68].copy_from_slice(&1_u32.to_le_bytes());
    image[68..72].copy_from_slice(&5_u32.to_le_bytes());
    image[72..80].copy_from_slice(&0x1000_u64.to_le_bytes());
    image[80..88].copy_from_slice(&0x20_0000_u64.to_le_bytes());
    image[88..96].copy_from_slice(&0x20_0000_u64.to_le_bytes());
    image[96..104].copy_from_slice(&2_u64.to_le_bytes());
    image[104..112].copy_from_slice(&0x2000_u64.to_le_bytes());
    image[112..120].copy_from_slice(&0x1000_u64.to_le_bytes());
    image[0x1000..].copy_from_slice(&[0x90, 0xc3]);
    image
}

fn pdeath_exec_executor(root: &TestDir) -> (ElfExecutor, GuestMemory, Arc<crate::failure::RunFailure>) {
    use std::os::unix::fs::PermissionsExt;
    let image = pdeath_static_image();
    let path = root.0.join("retained-static");
    std::fs::write(&path, &image).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut state = test_state(&root.0);
    state.executable_file = Some(Arc::new(std::fs::File::open(path).unwrap()));
    state.executable_image = Arc::from(image);
    let mut executor = ElfExecutor::new(state, true);
    let mut memory = GuestMemory::new(0, 16 * 1024 * 1024).unwrap();
    memory.write(0x100, b"/proc/self/exe\0").unwrap();
    let run = pdeath_adopt(&executor);
    assert_eq!(pdeath_set(&mut executor, &memory, libc::SIGUSR1 as u64), 0);
    executor.bind_address_space(&memory);
    (executor, memory, run)
}

#[test]
fn pdeath_retained_exec_snapshot_survives_path_mutation_and_consumes_exact_conversion_once() {
    let root = TestDir::new();
    let (mut executor, mut memory, _run) = pdeath_exec_executor(&root);
    let original = SyscallRequest::new(libc::SYS_execve as u64, [0x100, 0, 0, 17, 29, 0x55]);
    let site = executor.begin_signal_callback().unwrap();
    assert_eq!(executor.parent_death_original_syscall_preflight(site, &original, Some(original), false).unwrap(),
        reverie::ParentDeathSyscallAdmission::Admitted);
    let owned = executor.state.executable_file.as_ref().unwrap().clone();
    let image = executor.state.executable_image.clone();
    // A second process can alter shared pathname storage while the Tool owns
    // the callback. The first captured target must remain authoritative.
    memory.write(0x100, b"/path/changed/to/fifo\0").unwrap();
    assert_eq!(executor.parent_death_original_syscall_preflight(site, &original, Some(original), false).unwrap(),
        reverie::ParentDeathSyscallAdmission::Admitted);
    let converted = SyscallRequest::new(libc::SYS_execveat as u64,
        [libc::AT_FDCWD as u64, 0x100, 0, 0, 0, 0x55]);
    for index in 0..6 {
        let mut args = *converted.args(); args[index] ^= 1;
        assert!(executor.parent_death_injection_preflight(&SyscallRequest::new(libc::SYS_execveat as u64, args)).is_err(),
            "changed register {index} must not consume or widen authority");
    }
    assert!(executor.parent_death_injection_preflight(&SyscallRequest::new(libc::SYS_openat as u64,
        [libc::AT_FDCWD as u64, 0x100, libc::O_PATH as u64, 0, 0, 0])).is_err());
    assert_eq!(executor.execute_checked(&converted, &memory).unwrap(), 0);
    let Some(ProcessAction::Exec { executable_path, executable_file: Some(file), image: prepared, .. }) = executor.process_action.take()
        else { panic!("exact retained exec did not prepare an image"); };
    assert_eq!(executable_path, std::path::Path::new("/proc/self/exe"));
    assert!(Arc::ptr_eq(&file, &owned));
    assert_eq!(prepared.as_slice(), image.as_ref());
    assert!(executor.parent_death_injection_preflight(&converted).is_err());
    assert!(executor.parent_death_original_syscall_preflight(site, &original, Some(original), false).is_err());
    let mut bytes = [0_u8; 21]; memory.read(0x100, &mut bytes).unwrap();
    assert_eq!(&bytes, b"/path/changed/to/fifo\0");
}

#[test]
fn pdeath_retained_exec_rejects_missing_or_dynamic_authority_and_stale_callback_identity() {
    let root = TestDir::new();
    let (mut executor, mut memory, _run) = pdeath_exec_executor(&root);
    let original = SyscallRequest::new(libc::SYS_execveat as u64,
        [libc::AT_FDCWD as u64, 0x100, 0, 0, 0, 0x77]);
    let first = executor.begin_signal_callback().unwrap();
    memory.write(0x100, b"/ordinary/path\0").unwrap();
    assert!(executor.parent_death_original_syscall_preflight(first, &original, Some(original), false).is_err());
    memory.write(0x100, b"/proc/self/exe\0").unwrap();
    let file = executor.state.executable_file.take().unwrap();
    assert!(executor.parent_death_original_syscall_preflight(first, &original, Some(original), false).is_err());
    executor.state.executable_file = Some(file);
    let static_image = executor.state.executable_image.clone();
    let mut dynamic_image = static_image.to_vec();
    dynamic_image[64..68].copy_from_slice(&goblin::elf::program_header::PT_INTERP.to_le_bytes());
    executor.state.executable_image = Arc::from(dynamic_image);
    assert!(executor.parent_death_original_syscall_preflight(first, &original, Some(original), false).is_err());
    executor.state.executable_image = static_image;
    assert_eq!(executor.parent_death_original_syscall_preflight(first, &original, Some(original), false).unwrap(),
        reverie::ParentDeathSyscallAdmission::Admitted);
    let next = executor.begin_signal_callback().unwrap();
    assert!(executor.parent_death_injection_preflight(&original).is_err(), "new callback cannot reuse old snapshot");
    assert!(executor.parent_death_original_syscall_preflight(first, &original, Some(original), false).is_err());
    let mut reused = next; reused.task_generation += 1;
    assert!(executor.parent_death_original_syscall_preflight(reused, &original, Some(original), false).is_err());
    let mut reused = next; reused.process.generation += 1;
    assert!(executor.parent_death_original_syscall_preflight(reused, &original, Some(original), false).is_err());
    assert_eq!(executor.parent_death_original_syscall_preflight(next, &original, Some(original), false).unwrap(),
        reverie::ParentDeathSyscallAdmission::Admitted);
    executor.retire_parent_death_exec(next);
    assert!(executor.parent_death_injection_preflight(&original).is_err(), "callback refusal retires the staged target");
    assert!(executor.parent_death_original_syscall_preflight(next, &original, Some(original), false).is_err());
    executor.retire_current_thread(ExitStatus::SUCCESS, false);
    assert!(executor.parent_death_injection_preflight(&original).is_err(), "cancellation does not resurrect a snapshot");
    assert!(executor.parent_death_original_syscall_preflight(next, &original, Some(original), false).is_err());
}

#[test]
fn pdeath_finite_domain_rejects_unmodeled_io_and_shared_task_creation_before_effects() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), true);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    let _run = pdeath_adopt(&executor);
    assert_eq!(pdeath_set(&mut executor, &memory, libc::SIGUSR1 as u64), 0);
    executor.bind_address_space(&memory);
    memory.write(0x100, &[0xa5; 128]).unwrap();
    let site = executor.begin_signal_callback().unwrap();
    let next_pid = executor.next_pid.load(Ordering::SeqCst);
    for call in [
        SyscallRequest::new(libc::SYS_splice as u64, [0x100; 6]),
        SyscallRequest::new(libc::SYS_copy_file_range as u64, [0x100; 6]),
        SyscallRequest::new(libc::SYS_vfork as u64, [0; 6]),
        SyscallRequest::new(libc::SYS_clone as u64, [libc::CLONE_VM as u64, 0x100, 0x110, 0x120, 0, 0]),
        SyscallRequest::new(libc::SYS_clone3 as u64, [0x100, 88, 0, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_futex as u64, [0x100, 13, 0, 0, 0, 0]),
    ] {
        assert!(matches!(executor.parent_death_original_syscall_preflight(site, &call, Some(call), false),
            Err(crate::Error::ParentDeathSignal { errno: libc::ENOSYS, .. })));
        assert!(matches!(executor.execute_checked(&call, &memory), Err(crate::Error::ParentDeathSignal { .. })));
    }
    let mut bytes = [0; 128]; memory.read(0x100, &mut bytes).unwrap();
    assert_eq!(bytes, [0xa5; 128]);
    assert_eq!(executor.next_pid.load(Ordering::SeqCst), next_pid);
    assert!(executor.process_action.is_none());
    assert!(executor.pending_processes.is_empty());
}

#[test]
fn pdeath_close_replacement_and_exec_cloexec_require_owned_completion_authority() {
    use std::os::fd::{FromRawFd, IntoRawFd};
    let root = TestDir::new();
    let (mut executor, memory, _run) = pdeath_exec_executor(&root);
    let (socket, peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let path = root.0.join("ordinary-close");
    std::fs::write(&path, b"unchanged").unwrap();
    {
        let mut table = executor.file_table.lock().unwrap();
        // SAFETY: transfer ownership of the real socket descriptor to File.
        table.files.insert(5, unsafe { std::fs::File::from_raw_fd(socket.into_raw_fd()) });
        table.files.insert(7, std::fs::File::open(&path).unwrap());
        table.cloexec_fds.insert(5);
    }
    let site = executor.begin_signal_callback().unwrap();
    let original = SyscallRequest::new(libc::SYS_execve as u64, [0x100, 0, 0, 0, 0, 0]);
    for call in [
        SyscallRequest::new(libc::SYS_close as u64, [5, 0, 0, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_close_range as u64, [3, 10, 0, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_close_range as u64, [3, 10, 4, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_dup2 as u64, [7, 5, 0, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_dup3 as u64, [7, 5, 0, 0, 0, 0]),
        original,
    ] {
        assert!(matches!(executor.parent_death_original_syscall_preflight(site, &call, Some(call), false),
            Err(crate::Error::ParentDeathSignal { .. })));
        assert!(matches!(executor.execute_checked(&call, &memory), Err(crate::Error::ParentDeathSignal { .. })));
    }
    assert!(executor.process_action.is_none());
    {
        let table = executor.file_table.lock().unwrap();
        assert_eq!(file_mode(table.files.get(&5).unwrap()).unwrap() & libc::S_IFMT, libc::S_IFSOCK);
        assert!(table.cloexec_fds.contains(&5));
        assert!(table.files.contains_key(&7));
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
    // The guest may clear CLOEXEC without closing the endpoint. The first
    // retained-image snapshot can then be admitted; arbitrary Tool mutations
    // cannot use that snapshot to waive a later implicit-close check.
    assert_eq!(executor.execute_checked(&SyscallRequest::new(libc::SYS_fcntl as u64,
        [5, libc::F_SETFD as u64, 0, 0, 0, 0]), &memory).unwrap(), 0);
    assert_eq!(executor.parent_death_original_syscall_preflight(site, &original, Some(original), false).unwrap(),
        reverie::ParentDeathSyscallAdmission::Admitted);
    assert_eq!(executor.execute_checked(&SyscallRequest::new(libc::SYS_fcntl as u64,
        [5, libc::F_SETFD as u64, libc::FD_CLOEXEC as u64, 0, 0, 0]), &memory).unwrap(), 0);
    assert!(executor.parent_death_injection_preflight(&original).is_err());
    assert_eq!(executor.execute_checked(&SyscallRequest::new(libc::SYS_close as u64,
        [7, 0, 0, 0, 0, 0]), &memory).unwrap(), 0);
    assert!(!executor.file_table.lock().unwrap().files.contains_key(&7));
    assert_eq!(executor.execute_checked(&SyscallRequest::new(libc::SYS_close as u64,
        [1, 0, 0, 0, 0, 0]), &memory).unwrap(), 0, "genuine captured stdout is a supported close");
    drop(peer);
}

#[test]
fn pdeath_retained_exec_preserves_permission_and_copyin_errors_after_admission() {
    use std::os::unix::fs::PermissionsExt;
    let root = TestDir::new();
    let (mut executor, memory, _run) = pdeath_exec_executor(&root);
    let file = executor.state.executable_file.as_ref().unwrap().clone();
    file.set_permissions(std::fs::Permissions::from_mode(0o600)).unwrap();
    let original = SyscallRequest::new(libc::SYS_execve as u64, [0x100, 0, 0, 0, 0, 0]);
    let site = executor.begin_signal_callback().unwrap();
    assert_eq!(executor.parent_death_original_syscall_preflight(site, &original, Some(original), false).unwrap(),
        reverie::ParentDeathSyscallAdmission::Admitted);
    assert_eq!(executor.execute_checked(&original, &memory).unwrap(), -i64::from(libc::EACCES));
    assert!(executor.process_action.is_none());
    assert!(executor.parent_death_injection_preflight(&original).is_err(), "failed preparation still consumes its snapshot");
    file.set_permissions(std::fs::Permissions::from_mode(0o700)).unwrap();
    let original = SyscallRequest::new(libc::SYS_execve as u64, [0x100, u64::MAX, 0, 0, 0, 0]);
    let site = executor.begin_signal_callback().unwrap();
    assert_eq!(executor.parent_death_original_syscall_preflight(site, &original, Some(original), false).unwrap(),
        reverie::ParentDeathSyscallAdmission::Admitted);
    assert_eq!(executor.execute_checked(&original, &memory).unwrap(), -i64::from(libc::EFAULT));
    assert!(executor.process_action.is_none());
}
