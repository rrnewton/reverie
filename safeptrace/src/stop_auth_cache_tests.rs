/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#[cfg(not(sanitized))]
#[test]
fn stop_authentication_is_shared_and_revalidated_after_real_transitions() {
    const NAME: &str = "stop_authentication_is_shared_and_revalidated_after_real_transitions";
    if run_legacy_test_outer_with_outcome(
        NAME,
        Some("ACTUAL_STOP_AUTHENTICATION_CACHE_TRANSITIONS_EXERCISED"),
    ) {
        return;
    }
    for forced in [false, true] {
        let root = match unsafe { fork() }.unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                crate::traceme_and_stop().unwrap();
                unsafe {
                    libc::raise(libc::SIGTRAP);
                    libc::_exit(23);
                }
            }
        };
        let mut cleanup = TraceeCleanupGuard::new(root).unwrap();
        let _force = forced.then(|| LegacyThreadGroup::new(root));
        let status = waitpid_status_bounded(root, libc::__WALL, TRACEE_WAIT_TIMEOUT).unwrap();
        assert!(libc::WIFSTOPPED(status));
        assert_eq!(libc::WSTOPSIG(status), libc::SIGSTOP);
        let stopped = Stopped::new_unchecked_on_ptracer_thread(root.into()).unwrap();
        let original = stopped.1.event().clone();
        // Force both optional descriptor proofs to refuse. The unchanged
        // exact read counts below then exercise the strong directory/status
        // fallback and its real transition invalidation on either kernel.
        stopped
            .1
            .ptracer_owner
            .as_ref()
            .unwrap()
            .force_directory_proof
            .store(true, Ordering::Relaxed);
        original
            .event()
            .numeric_auth_force_directory
            .store(true, Ordering::Relaxed);
        let generation = stopped.generation();
        assert_eq!(
            original
                .event()
                .numeric_auth_status_reads
                .load(Ordering::Relaxed),
            0
        );
        let registers = stopped.getregs().unwrap();
        for _ in 0..128 {
            assert_eq!(stopped.getregs().unwrap(), registers);
            assert_eq!(stopped.getsiginfo().unwrap().si_signo, libc::SIGSTOP);
        }
        assert_eq!(
            original
                .event()
                .numeric_auth_status_reads
                .load(Ordering::Relaxed),
            1
        );
        stopped
            .setoptions(Options::PTRACE_O_TRACESYSGOOD | Options::PTRACE_O_TRACEEXIT)
            .unwrap();
        assert_eq!(
            original
                .event()
                .numeric_auth_status_reads
                .load(Ordering::Relaxed),
            1
        );

        // SINGLESTEP and SYSCALL each resume the real stop. Their actual
        // consumed reports must require a new full proof, even though the
        // generation, ptracer and retained proc directory remain the same.
        let running = stopped.step(None).unwrap();
        let (stopped, event) = running
            .wait_sync_on_ptracer_thread()
            .wait()
            .unwrap()
            .assume_stopped();
        assert_eq!(event, crate::Event::Signal(Signal::SIGTRAP));
        assert_eq!(stopped.generation(), generation);
        let registers = stopped.getregs().unwrap();
        for _ in 0..128 {
            assert_eq!(stopped.getregs().unwrap(), registers);
        }
        assert_eq!(
            original
                .event()
                .numeric_auth_status_reads
                .load(Ordering::Relaxed),
            2
        );

        let running = stopped.syscall(None).unwrap();
        let (stopped, event) = running
            .wait_sync_on_ptracer_thread()
            .wait()
            .unwrap()
            .assume_stopped();
        assert_eq!(event, crate::Event::Syscall);
        assert_eq!(stopped.generation(), generation);
        let registers = stopped.getregs().unwrap();
        for _ in 0..128 {
            assert_eq!(stopped.getregs().unwrap(), registers);
        }
        assert_eq!(
            original
                .event()
                .numeric_auth_status_reads
                .load(Ordering::Relaxed),
            3
        );

        // DETACH invalidates the proof too and returns reaping to the real
        // parent. The original descriptor still supplies exact cleanup.
        let epoch = original.event().numeric_auth_epoch.load(Ordering::Acquire);
        let _detached = stopped.detach(Signal::SIGKILL).unwrap();
        assert_ne!(
            original.event().numeric_auth_epoch.load(Ordering::Acquire),
            epoch
        );
        let terminal = waitpid_status_bounded(root, libc::__WALL, TRACEE_WAIT_TIMEOUT).unwrap();
        assert!(libc::WIFSIGNALED(terminal));
        assert_eq!(libc::WTERMSIG(terminal), libc::SIGKILL);
        cleanup.disarm();
        assert!(!std::path::Path::new(&format!("/proc/{root}")).exists());
    }
    emit_completion_marker("ACTUAL_STOP_AUTHENTICATION_CACHE_TRANSITIONS_EXERCISED");
}

#[cfg(not(sanitized))]
#[test]
fn retained_descriptor_proofs_authenticate_the_real_owner_and_target() {
    const NAME: &str = "retained_descriptor_proofs_authenticate_the_real_owner_and_target";
    if run_legacy_test_outer_with_outcome(
        NAME,
        Some("ACTUAL_RETAINED_DESCRIPTOR_AUTHENTICATION_EXERCISED"),
    ) {
        return;
    }
    for forced in [false, true] {
        let root = match unsafe { fork() }.unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                crate::traceme_and_stop().unwrap();
                unsafe {
                    libc::_exit(23);
                }
            }
        };
        let mut cleanup = TraceeCleanupGuard::new(root).unwrap();
        let _force = forced.then(|| LegacyThreadGroup::new(root));
        let status = waitpid_status_bounded(root, libc::__WALL, TRACEE_WAIT_TIMEOUT).unwrap();
        assert!(libc::WIFSTOPPED(status));
        assert_eq!(libc::WSTOPSIG(status), libc::SIGSTOP);
        let stopped = Stopped::new_unchecked_on_ptracer_thread(root.into()).unwrap();
        let original = stopped.1.event().clone();
        let owner = stopped.1.ptracer_owner.as_ref().unwrap();
        let owner_fd = owner
            .self_signal_fd
            .as_ref()
            .expect("real cold descriptor proof");
        assert!(descriptor_is_current_task(owner_fd.as_raw_fd()));
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    assert!(!descriptor_is_current_task(owner_fd.as_raw_fd()));
                    assert!(!owner.is_current().unwrap());
                })
                .join()
                .unwrap();
        });
        assert_eq!(original.identity().unwrap().pidfd_is_live(), Ok(true));
        let registers = stopped.getregs().unwrap();
        for _ in 0..128 {
            assert_eq!(stopped.getregs().unwrap(), registers);
            assert_eq!(stopped.getsiginfo().unwrap().si_signo, libc::SIGSTOP);
        }
        assert_eq!(
            original
                .event()
                .numeric_auth_status_reads
                .load(Ordering::Relaxed),
            0
        );
        let _detached = stopped.detach(Signal::SIGKILL).unwrap();
        let terminal = waitpid_status_bounded(root, libc::__WALL, TRACEE_WAIT_TIMEOUT).unwrap();
        assert!(libc::WIFSIGNALED(terminal));
        assert_eq!(libc::WTERMSIG(terminal), libc::SIGKILL);
        assert_eq!(original.identity().unwrap().pidfd_is_live(), Ok(false));
        cleanup.disarm();
        assert!(!std::path::Path::new(&format!("/proc/{root}")).exists());
    }
    emit_completion_marker("ACTUAL_RETAINED_DESCRIPTOR_AUTHENTICATION_EXERCISED");
}

#[cfg(not(sanitized))]
#[test]
fn owned_cleanup_preserves_original_anchor_without_discarded_capture() {
    const NAME: &str = "owned_cleanup_preserves_original_anchor_without_discarded_capture";
    if run_legacy_test_outer_with_outcome(
        NAME,
        Some("ACTUAL_OWNED_CLEANUP_CAPTURE_REUSE_EXERCISED"),
    ) {
        return;
    }
    for forced in [false, true] {
        let root = match unsafe { fork() }.unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                crate::traceme_and_stop().unwrap();
                unsafe {
                    libc::_exit(23);
                }
            }
        };
        let mut cleanup = TraceeCleanupGuard::new(root).unwrap();
        let _force = forced.then(|| LegacyThreadGroup::new(root));
        let status = waitpid_status_bounded(root, libc::__WALL, TRACEE_WAIT_TIMEOUT).unwrap();
        assert!(libc::WIFSTOPPED(status));
        assert_eq!(libc::WSTOPSIG(status), libc::SIGSTOP);
        let stopped = Stopped::new_unchecked_on_ptracer_thread(root.into()).unwrap();
        let original = stopped.1.event().clone();
        let owner = stopped.1.ptracer_owner.as_ref().unwrap();
        let captures = PTRACER_OWNER_CAPTURES.with(Cell::get);
        for _ in 0..128 {
            let local = stopped.terminal_cleanup_on_ptracer_thread();
            assert!(Arc::ptr_eq(
                local.driver.affinity.owner.as_ref().unwrap(),
                owner
            ));
            assert_eq!(local.driver.affinity.wait_role, stopped.1.ptracer_wait_role);
            assert!(
                local
                    .shared()
                    .same_generation(&TerminalCleanup::new_unregistered(root.into(), &stopped.1))
            );
        }
        assert_eq!(PTRACER_OWNER_CAPTURES.with(Cell::get), captures);

        // Dropping a role witness never changes its host anchor. A passive
        // cleanup constructor must retain false, leaving first-operation
        // role authentication to the original operation path.
        let mut constructor_only = stopped.1.clone();
        constructor_only.ptracer_wait_role = false;
        let conservative = PtracerTerminalCleanup::new(root.into(), &constructor_only);
        assert!(!conservative.driver.affinity.wait_role);
        assert!(Arc::ptr_eq(
            conservative.driver.affinity.owner.as_ref().unwrap(),
            owner
        ));
        assert_eq!(PTRACER_OWNER_CAPTURES.with(Cell::get), captures);

        // A shared-only facade genuinely has no host anchor. Keep the cold
        // binding behavior instead of extending the owned-state fast path.
        let shared = TerminalCleanup::new_unregistered(root.into(), &stopped.1);
        let cold = shared.on_ptracer_thread();
        assert!(cold.driver.affinity.owner.is_some());
        assert!(cold.driver.affinity.wait_role);
        assert_eq!(PTRACER_OWNER_CAPTURES.with(Cell::get), captures + 1);
        assert_eq!(
            original.event().worker_state.load(Ordering::Acquire),
            WORKER_NOT_STARTED
        );
        assert!(!*original.event().terminal_reaping.read());
        let registers = stopped.getregs().unwrap();
        assert_eq!(stopped.getregs().unwrap(), registers);
        assert_eq!(stopped.getsiginfo().unwrap().si_signo, libc::SIGSTOP);
        let _detached = stopped.detach(Signal::SIGKILL).unwrap();
        let terminal = waitpid_status_bounded(root, libc::__WALL, TRACEE_WAIT_TIMEOUT).unwrap();
        assert!(libc::WIFSIGNALED(terminal));
        assert_eq!(libc::WTERMSIG(terminal), libc::SIGKILL);
        cleanup.disarm();
        assert!(!std::path::Path::new(&format!("/proc/{root}")).exists());
    }
    emit_completion_marker("ACTUAL_OWNED_CLEANUP_CAPTURE_REUSE_EXERCISED");
}
