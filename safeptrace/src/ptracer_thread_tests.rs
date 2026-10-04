/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
*/

#[test]
fn explicit_ptracer_namespace_guard_requires_complete_original_self_field() {
    require_aligned_proc_pid_namespace().unwrap();
    let tid = super::Pid::from_raw(123);
    assert_eq!(
        aligned_proc_pid_namespace(b"Name:\t\xff-name\nNSpid:\t123\n", tid),
        Ok(())
    );
    assert_eq!(
        aligned_proc_pid_namespace(b"NSpid:\t123 1\n", tid),
        Err(Errno::EXDEV)
    );
    assert_eq!(
        aligned_proc_pid_namespace(b"NSpid:\t456\n", tid),
        Err(Errno::EXDEV)
    );
    assert_eq!(
        aligned_proc_pid_namespace(b"NSpid:\t123", tid),
        Err(Errno::EOPNOTSUPP)
    );
    assert_eq!(
        aligned_proc_pid_namespace(b"NSpid:\t123 ", tid),
        Err(Errno::EOPNOTSUPP)
    );
    let mut large = b"Name:\tlarge-groups\nGroups:\t".to_vec();
    for _ in 0..65536 {
        large.extend_from_slice(b"4294967294 ");
    }
    large.extend_from_slice(b"\nNSpid:\t123\n");
    assert!(large.len() < 1024 * 1024);
    assert_eq!(aligned_proc_pid_namespace(&large, tid), Ok(()));
}

#[test]
#[cfg(not(sanitized))]
fn explicit_ptracer_namespace_guard_refuses_inherited_outer_proc() {
    const NAME: &str = "explicit_ptracer_namespace_guard_refuses_inherited_outer_proc";
    const INNER: &str = "SAFEPTRACE_PROC_NAMESPACE_GUARD_INNER";
    const MARKER: &str = "ACTUAL_EXPLICIT_PTRACER_PROC_NAMESPACE_GUARD_EXERCISED";
    if env::var_os(INNER).is_none() {
        let output =
            run_exact_in_pid_namespace_bounded(&format!("notifier::test::{NAME}"), &[(INNER, "1")]);
        match classify_exact_reuse_output(
            output.as_ref(),
            MARKER,
            "PROC_NAMESPACE_GUARD_UNAVAILABLE",
        )
        .unwrap()
        {
            ExactReuseOutcome::Exercised => println!("{MARKER}"),
            ExactReuseOutcome::Unavailable => println!("PROC_NAMESPACE_GUARD_UNAVAILABLE"),
        }
        return;
    }
    require_aligned_proc_pid_namespace().unwrap();
    assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWPID) }, 0);
    let child = match unsafe { fork() }.unwrap() {
        ForkResult::Parent { child } => child,
        ForkResult::Child => {
            let tid = super::Pid::from(nix::unistd::gettid());
            assert_eq!(tid.as_raw(), 1);
            // The inherited proc mount still names the outer namespace.
            // Both the exact target capture and host anchor must refuse it
            // before opening a numeric proc target or sending any signal.
            assert_eq!(require_aligned_proc_pid_namespace(), Err(Errno::EXDEV));
            assert_eq!(
                Running::new_on_ptracer_thread(tid).unwrap_err(),
                Errno::EXDEV
            );
            assert_eq!(
                LegacyWaitOwner::capture_current().unwrap_err(),
                Errno::EXDEV
            );
            let exact_self = pidfd_open_with_flags(tid, 0).unwrap();
            assert_eq!(descriptor_is_live(exact_self.as_raw_fd()), Ok(true));
            unsafe { libc::_exit(0) };
        }
    };
    let status = waitpid_status_bounded(child, 0, TRACEE_WAIT_TIMEOUT).unwrap();
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
    println!("{MARKER}");
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(sanitized))]
async fn explicit_ptracer_drivers_retain_authority_after_foreign_polls() {
    const MARKER: &str = "ACTUAL_EXPLICIT_PTRACER_DRIVER_RECOVERY_EXERCISED";
    if run_legacy_test_outer_with_outcome(
        "explicit_ptracer_drivers_retain_authority_after_foreign_polls",
        Some(MARKER),
    ) {
        return;
    }
    for forced in [false, true] {
        let (root, tid, release, mut root_cleanup) = legacy_owner_guest();
        let _force = forced.then(|| LegacyThreadGroup::new(root));
        let running =
            Running::seize_on_ptracer_thread(tid.into(), legacy_thread_options()).unwrap();
        let terminal = running.terminal_cleanup();
        let native = terminal.has_thread_pidfd().unwrap();
        if forced {
            assert!(!native);
            let before = terminal.registration_error();
            assert_eq!(
                EventHandle::current_or_new(tid.into()).unwrap_err(),
                Errno::EINVAL
            );
            assert_eq!(
                NOTIFIER.event(tid.into(), running.1.event()).unwrap_err(),
                Errno::EINVAL
            );
            assert_eq!(terminal.registration_error(), before);
        }
        running.interrupt().unwrap();
        let mut driver = running.wait_owned_on_ptracer_thread().into_driver();
        driver = thread::spawn(move || {
            let waker = futures::task::noop_waker();
            assert!(matches!(
                driver.poll_on_ptracer_thread(&mut Context::from_waker(&waker)),
                Poll::Ready(Err(OwnedWaitError::Errno(Errno::EPERM)))
            ));
            driver
        })
        .join()
        .unwrap();
        let (stopped, event) = tokio::time::timeout(
            TRACEE_WAIT_TIMEOUT,
            futures::future::poll_fn(|cx| driver.poll_on_ptracer_thread(cx)),
        )
        .await
        .unwrap()
        .unwrap()
        .assume_stopped();
        assert_eq!(event, crate::Event::Stop);
        assert_eq!(stopped.pid(), tid.into());
        assert!(matches!(
            driver.poll_on_ptracer_thread(&mut Context::from_waker(&futures::task::noop_waker())),
            Poll::Ready(Err(OwnedWaitError::Completed))
        ));

        let mut exit = stopped.exit_event_on_ptracer_thread().into_driver();
        let running = stopped.resume(None).unwrap();
        assert_eq!(
            unsafe { libc::write(release.as_raw_fd(), b"x".as_ptr().cast(), 1) },
            1
        );
        exit = thread::spawn(move || {
            let waker = futures::task::noop_waker();
            assert!(matches!(
                exit.poll_on_ptracer_thread(&mut Context::from_waker(&waker)),
                Poll::Ready(Err(Error::Errno(Errno::EPERM)))
            ));
            exit
        })
        .join()
        .unwrap();
        let stopped = tokio::time::timeout(
            TRACEE_WAIT_TIMEOUT,
            futures::future::poll_fn(|cx| exit.poll_on_ptracer_thread(cx)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(stopped.getevent().unwrap(), 23 << 8);
        drop(running);
        let mut final_wait = stopped
            .resume(None)
            .unwrap()
            .wait_owned_on_ptracer_thread()
            .into_driver();
        final_wait = thread::spawn(move || {
            let waker = futures::task::noop_waker();
            assert!(matches!(
                final_wait.poll_on_ptracer_thread(&mut Context::from_waker(&waker)),
                Poll::Ready(Err(OwnedWaitError::Errno(Errno::EPERM)))
            ));
            final_wait
        })
        .join()
        .unwrap();
        assert!(matches!(
            tokio::time::timeout(TRACEE_WAIT_TIMEOUT,
                futures::future::poll_fn(|cx| final_wait.poll_on_ptracer_thread(cx)),
            ).await.unwrap().unwrap(),
            Wait::Exited(pid, crate::ExitStatus::Exited(23)) if pid == tid.into()
        ));
        assert!(terminal.wait(TRACEE_WAIT_TIMEOUT));
        assert_eq!(
            terminal.observed_exit_status(),
            Ok(Some(crate::ExitStatus::Exited(23)))
        );
        assert!(matches!(
            exit.poll_on_ptracer_thread(&mut Context::from_waker(&futures::task::noop_waker())),
            Poll::Ready(Err(Error::Errno(Errno::EALREADY | Errno::ECHILD)))
        ));
        legacy_owner_reap_root(root, &mut root_cleanup);
        assert!(!std::path::Path::new(&format!("/proc/{tid}")).exists());
        println!(
            "EXPLICIT_PTRACER_DRIVER_RECOVERY native={native} forced={forced} actual_exit=23 done=true root_reaped=true member_absent=true"
        );
    }
    println!("{MARKER}");
}

#[test]
#[cfg(not(sanitized))]
fn explicit_ptracer_mode_cannot_recapture_an_unbound_generic_token() {
    let (pid, mut cleanup) = spawn_stopped_process(None).unwrap();
    let _force = LegacyThreadGroup::new(pid);
    let unbound = Running::new(pid.into());
    assert!(unbound.1.event().identity().is_none());
    let mut driver = unbound.wait_owned_on_ptracer_thread().into_driver();
    let waker = futures::task::noop_waker();
    assert!(matches!(
        driver.poll_on_ptracer_thread(&mut Context::from_waker(&waker)),
        Poll::Ready(Err(OwnedWaitError::Errno(Errno::ENODATA)))
    ));
    assert!(
        driver
            .inner
            .inner
            .as_ref()
            .unwrap()
            .token
            .event()
            .identity()
            .is_none()
    );
    let running = Running::new_on_ptracer_thread(pid.into()).unwrap();
    cleanup.bind_running_notifier(&running).unwrap();
    let terminal = running.terminal_cleanup();
    terminal.request_sigkill().unwrap();
    assert!(terminal.wait(TRACEE_WAIT_TIMEOUT));
    cleanup.disarm();
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[test]
fn explicit_ptracer_interfaces_preserve_portable_auto_trait_contracts() {
    fn send<T: Send>() {}
    fn sync<T: Sync>() {}
    send::<WaitFuture>();
    send::<OwnedWaitFuture>();
    send::<ExitFuture>();
    send::<TerminalCleanup>();
    sync::<TerminalCleanup>();
    send::<PtracerWaitDriver>();
    send::<PtracerExitDriver>();
    send::<PtracerCleanupDriver>();
    send::<PtracerSyncDriver>();

    // Stable negative auto-trait checks: the inferred marker becomes
    // ambiguous if the concrete interface ever acquires the forbidden trait.
    macro_rules! not_trait {
        ($ty:ty, $bound:path) => {{
            trait Ambiguous<A> {
                fn witness() {}
            }
            impl<T: ?Sized> Ambiguous<()> for T {}
            struct Forbidden;
            impl<T: ?Sized + $bound> Ambiguous<Forbidden> for T {}
            let _ = <$ty as Ambiguous<_>>::witness;
        }};
    }
    macro_rules! local {
        ($ty:ty) => {{
            not_trait!($ty, Send);
            not_trait!($ty, Sync);
        }};
    }
    local!(PtracerOwnedWaitFuture);
    local!(PtracerExitFuture);
    local!(PtracerTerminalCleanup);
    local!(PtracerPendingStatusReservation<'static>);
    local!(PtracerSyncWait);
    not_trait!(PtracerWaitDriver, Future);
    not_trait!(PtracerExitDriver, Future);
    not_trait!(PtracerCleanupDriver, Future);
    not_trait!(PtracerSyncDriver, Future);
}

#[test]
#[cfg(not(sanitized))]
fn explicit_attachment_retains_successful_state_and_exact_capture_refusal() {
    if run_legacy_test_outer(
        "explicit_attachment_retains_successful_state_and_exact_capture_refusal",
    ) {
        return;
    }
    for seize in [false, true] {
        for error in [Errno::EMFILE, Errno::EPERM, Errno::EIO] {
            let (root, tid, _release, mut root_cleanup) = legacy_owner_guest();
            let _force = LegacyThreadGroup::new(root);
            CAPTURE_ERRORS.lock().insert(tid.into(), error);
            let running = if seize {
                Running::seize_on_ptracer_thread(tid.into(), legacy_thread_options()).unwrap()
            } else {
                Running::attach_on_ptracer_thread(tid.into()).unwrap()
            };
            assert_eq!(
                worker_proc_snapshot(tid.into()).unwrap().tracer_pid,
                nix::unistd::gettid().into()
            );
            assert!(running.1.event().identity().is_none());
            let original = running.1.event().clone();
            let terminal = running.terminal_cleanup();
            assert_eq!(terminal.ensure_registered(), Err(error));
            assert_eq!(terminal.registration_error(), Some(error));
            assert_eq!(running.interrupt(), Err(error));
            let mut driver = running.wait_owned_on_ptracer_thread().into_driver();
            let waker = futures::task::noop_waker();
            for _ in 0..2 {
                assert!(matches!(
                    driver.poll_on_ptracer_thread(&mut Context::from_waker(&waker)),
                    Poll::Ready(Err(OwnedWaitError::Errno(actual))) if actual == error
                ));
                assert_eq!(
                    *driver.inner.inner.as_ref().unwrap().token.event(),
                    original
                );
                assert!(original.identity().is_none());
            }
            // The controlled fixture cannot exec or allocate a replacement.
            // Its real parent's already-retained regular pidfd terminates the
            // group; this raw fixture cleanup consumes only this member's
            // genuine owned stops and final SIGKILL report, never a SDK hint.
            pidfd_send_signal(&root_cleanup.pidfd, libc::SIGKILL).unwrap();
            let deadline = Instant::now() + TRACEE_WAIT_TIMEOUT;
            loop {
                assert!(
                    Instant::now() < deadline,
                    "original attachment cleanup deadline"
                );
                let flags = WaitPidFlag::from_bits_retain(
                    WaitPidFlag::WEXITED.bits()
                        | WaitPidFlag::WSTOPPED.bits()
                        | WaitPidFlag::WNOHANG.bits()
                        | libc::__WALL
                        | libc::__WNOTHREAD,
                );
                let Some(status) = waitid::wait_raw(waitid::IdType::Pid(tid), flags).unwrap()
                else {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                };
                if libc::WIFSTOPPED(status) {
                    assert_eq!(
                        unsafe {
                            libc::ptrace(
                                libc::PTRACE_CONT,
                                tid.as_raw(),
                                std::ptr::null_mut::<libc::c_void>(),
                                std::ptr::null_mut::<libc::c_void>(),
                            )
                        },
                        0
                    );
                    continue;
                }
                assert!(libc::WIFSIGNALED(status));
                assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
                break;
            }
            legacy_owner_reap_root(root, &mut root_cleanup);
            assert!(!std::path::Path::new(&format!("/proc/{tid}")).exists());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(sanitized))]
async fn explicit_legacy_cache_admits_real_native_sibling_without_another_owner() {
    const NAME: &str = "explicit_legacy_cache_admits_real_native_sibling_without_another_owner";
    const INNER: &str = "SAFEPTRACE_NATIVE_CACHE_CONTROL_INNER";
    const MARKER: &str = "ACTUAL_NATIVE_CACHE_PROMOTION_EXERCISED";
    const UNAVAILABLE: &str = "NATIVE_THREAD_PIDFD_UNAVAILABLE_FOR_CACHE_CONTROL";
    if env::var_os(INNER).is_none() {
        let output = run_exact_test_bounded(
            &format!("notifier::test::{NAME}"),
            &[(INNER, "1")],
            false,
            Duration::from_secs(5),
        )
        .unwrap();
        match classify_exact_reuse_output(Some(&output.output), MARKER, UNAVAILABLE).unwrap() {
            ExactReuseOutcome::Exercised => println!("{MARKER}"),
            ExactReuseOutcome::Unavailable => println!("{UNAVAILABLE}"),
        }
        assert!(!output.timed_out);
        return;
    }
    let (root, tid, release, mut root_cleanup) = legacy_owner_guest();
    match pidfd_open_with_flags(tid.into(), libc::O_EXCL) {
        Ok(reference) => drop(reference),
        Err(Errno::EINVAL) => {
            legacy_owner_reap_root(root, &mut root_cleanup);
            println!("NATIVE_THREAD_PIDFD_UNAVAILABLE_FOR_CACHE_CONTROL");
            return;
        }
        Err(error) => panic!("native reference acquisition refused: {error}"),
    }
    let force = LegacyThreadGroup::new(root);
    let running = Running::seize_on_ptracer_thread(tid.into(), legacy_thread_options()).unwrap();
    let generation = running.generation();
    let legacy = running.terminal_cleanup();
    assert_eq!(legacy.has_thread_pidfd(), Ok(false));
    assert_eq!(legacy.request_sigstop(), Err(Errno::EOPNOTSUPP));
    running.interrupt().unwrap();
    // A consumes a genuine original stop before admission. Whether it is
    // still in the consumed-producer queue or already in the published FIFO,
    // B must receive that exact report before the one worker waits natively.
    legacy_owner_until(|| {
        running.1.event().try_assist_legacy_wait().unwrap();
        !running
            .1
            .event()
            .event()
            .legacy_wait
            .lock()
            .consumed
            .is_empty()
            || !legacy.queued_raw_statuses().is_empty()
    });
    let pid = super::Pid::from(tid);
    let stopped = thread::spawn(move || {
        let native_reference = pidfd_open_with_flags(pid, libc::O_EXCL).unwrap();
        assert_eq!(descriptor_is_live(native_reference.as_raw_fd()), Ok(true));
        let native = Running::try_new(pid).unwrap();
        let native_terminal = native.terminal_cleanup();
        assert_eq!(native_terminal.has_thread_pidfd(), Ok(true));
        let reservation = native_terminal
            .reserve_pending_for_cleanup(TRACEE_WAIT_TIMEOUT)
            .expect("actual native shared FIFO reservation");
        let (stopped, event) = reservation.decode().unwrap().assume_stopped();
        assert_eq!(event, crate::Event::Stop);
        assert!(native_terminal.same_generation(&stopped.terminal_cleanup()));
        reservation.commit();
        drop(native);
        stopped
    })
    .join()
    .unwrap();
    assert_eq!(
        generation,
        stopped.generation(),
        "mode/view does not change generation identity"
    );
    assert!(legacy.same_generation(&stopped.terminal_cleanup()));
    assert_eq!(legacy.has_thread_pidfd(), Ok(false));
    assert_eq!(legacy.request_sigstop(), Err(Errno::EOPNOTSUPP));
    assert_eq!(SPAWN_WORKER_COUNTS.lock().get(&pid).copied(), Some(1));
    // The new facade retains a real native descriptor, including actual
    // exact-thread SIGSTOP and the existing original siginfo contract.
    let native_terminal = stopped.terminal_cleanup();
    assert_eq!(native_terminal.has_thread_pidfd(), Ok(true));
    native_terminal.request_sigstop().unwrap();
    let native = stopped.resume(None).unwrap();
    let stopped = thread::spawn(move || {
        let (stopped, event) = futures::executor::block_on(native.next_state())
            .unwrap()
            .assume_stopped();
        assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));
        stopped
    })
    .join()
    .unwrap();
    assert_eq!(stopped.getsiginfo().unwrap().si_signo, libc::SIGSTOP);
    let native = stopped.resume(None).unwrap();
    native.interrupt().unwrap();
    let stopped = thread::spawn(move || {
        let (stopped, event) = futures::executor::block_on(native.next_state())
            .unwrap()
            .assume_stopped();
        assert_eq!(event, crate::Event::Stop);
        stopped
    })
    .join()
    .unwrap();
    let exit = stopped.exit_event();
    let native = stopped.resume(None).unwrap();
    assert_eq!(
        unsafe { libc::write(release.as_raw_fd(), b"x".as_ptr().cast(), 1) },
        1
    );
    let stopped = thread::spawn(move || futures::executor::block_on(exit).unwrap())
        .join()
        .unwrap();
    assert_eq!(stopped.getevent().unwrap(), 23 << 8);
    drop(native);
    let native = stopped.resume(None).unwrap();
    assert!(
        matches!(thread::spawn(move || futures::executor::block_on(native.next_state()).unwrap()).join().unwrap(),
        Wait::Exited(pid, crate::ExitStatus::Exited(23)) if pid == tid.into())
    );
    assert!(native_terminal.wait(TRACEE_WAIT_TIMEOUT));
    assert!(legacy.wait(TRACEE_WAIT_TIMEOUT));
    assert_eq!(
        legacy.observed_exit_status(),
        Ok(Some(crate::ExitStatus::Exited(23)))
    );
    assert_eq!(SPAWN_WORKER_COUNTS.lock().get(&pid).copied(), Some(1));
    drop(force);
    legacy_owner_reap_root(root, &mut root_cleanup);
    assert!(!std::path::Path::new(&format!("/proc/{tid}")).exists());
    println!("{MARKER}");
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(sanitized))]
async fn native_capture_before_legacy_open_refusal_keeps_native_wait_authority() {
    const NAME: &str = "native_capture_before_legacy_open_refusal_keeps_native_wait_authority";
    const INNER: &str = "SAFEPTRACE_NATIVE_BEFORE_REFUSAL_INNER";
    const MARKER: &str = "ACTUAL_NATIVE_BEFORE_REFUSAL_EXERCISED";
    const UNAVAILABLE: &str = "NATIVE_THREAD_PIDFD_UNAVAILABLE_FOR_REGISTRATION_CONTROL";
    if env::var_os(INNER).is_none() {
        let output = run_exact_test_bounded(
            &format!("notifier::test::{NAME}"),
            &[(INNER, "1")],
            false,
            Duration::from_secs(5),
        )
        .unwrap();
        match classify_exact_reuse_output(Some(&output.output), MARKER, UNAVAILABLE).unwrap() {
            ExactReuseOutcome::Exercised => println!("{MARKER}"),
            ExactReuseOutcome::Unavailable => println!("{UNAVAILABLE}"),
        }
        assert!(!output.timed_out);
        return;
    }
    for local_registration in [false, true] {
        let (root, tid, release, mut root_cleanup) = legacy_owner_guest();
        let running = match Running::seize(tid.into(), legacy_thread_options()) {
            Ok(running) => running,
            Err(Errno::EINVAL) => {
                legacy_owner_reap_root(root, &mut root_cleanup);
                println!("{UNAVAILABLE}");
                return;
            }
            Err(error) => panic!("original native attachment refused: {error}"),
        };
        let generation = running.generation();
        let original = running.1.event().clone();
        assert!(matches!(
            original.identity().unwrap().pidfd,
            ThreadHandle::Pidfd(_)
        ));
        let force = LegacyThreadGroup::new(root);
        let terminal = if local_registration {
            // Explicit registration may capture a fresh legacy view under
            // this caller's refusal. The already bound native descriptor
            // must remain the worker's exact consuming authority.
            NOTIFIER
                .event_for_policy(tid.into(), &original, WaitPolicy::PtracerThread)
                .unwrap();
            assert!(matches!(
                NOTIFIER
                    .pids
                    .lock()
                    .get(&tid.into())
                    .unwrap()
                    .identity
                    .pidfd,
                ThreadHandle::Pidfd(_)
            ));
            TerminalCleanup::new_unregistered(tid.into(), &running.1)
        } else {
            // A native-bound shared facade preserves the original typed
            // registration EINVAL; it must not silently downgrade to an
            // observer-only worker that the generic waiter cannot progress.
            let terminal = running.terminal_cleanup();
            assert_eq!(terminal.ensure_registered(), Err(Errno::EINVAL));
            assert_eq!(terminal.registration_error(), Some(Errno::EINVAL));
            assert_eq!(
                original.event().worker_state.load(Ordering::Acquire),
                WORKER_NOT_STARTED
            );
            terminal
        };
        assert_eq!(terminal.has_thread_pidfd(), Ok(true));
        running.interrupt().unwrap();
        let owned = running.wait_owned();
        let stopped = thread::spawn(move || {
            let (stopped, event) = futures::executor::block_on(owned).unwrap().assume_stopped();
            assert_eq!(event, crate::Event::Stop);
            stopped
        })
        .join()
        .unwrap();
        assert_eq!(stopped.generation(), generation);
        assert!(terminal.same_generation(&stopped.terminal_cleanup()));
        assert_eq!(terminal.registration_error(), None);
        assert_eq!(
            SPAWN_WORKER_COUNTS.lock().get(&tid.into()).copied(),
            Some(1)
        );
        let exit = stopped.exit_event();
        let running = stopped.resume(None).unwrap();
        assert_eq!(
            unsafe { libc::write(release.as_raw_fd(), b"x".as_ptr().cast(), 1) },
            1
        );
        let stopped = thread::spawn(move || futures::executor::block_on(exit).unwrap())
            .join()
            .unwrap();
        assert_eq!(stopped.getevent().unwrap(), 23 << 8);
        drop(running);
        let running = stopped.resume(None).unwrap();
        assert!(
            matches!(thread::spawn(move || futures::executor::block_on(running.wait_owned()).unwrap()).join().unwrap(),
            Wait::Exited(pid, crate::ExitStatus::Exited(23)) if pid == tid.into())
        );
        assert!(terminal.wait(TRACEE_WAIT_TIMEOUT));
        assert_eq!(
            terminal.observed_exit_status(),
            Ok(Some(crate::ExitStatus::Exited(23)))
        );
        assert_eq!(
            SPAWN_WORKER_COUNTS.lock().get(&tid.into()).copied(),
            Some(1)
        );
        drop(force);
        legacy_owner_reap_root(root, &mut root_cleanup);
        assert!(!std::path::Path::new(&format!("/proc/{tid}")).exists());
    }
    println!("{MARKER}");
}

#[test]
#[cfg(not(sanitized))]
fn explicit_native_cache_admission_preserves_actual_local_sync_claim() {
    const NAME: &str = "explicit_native_cache_admission_preserves_actual_local_sync_claim";
    const INNER: &str = "SAFEPTRACE_NATIVE_SYNC_CLAIM_INNER";
    const MARKER: &str = "ACTUAL_NATIVE_CACHE_SYNC_ARBITRATION_EXERCISED";
    const UNAVAILABLE: &str = "NATIVE_THREAD_PIDFD_UNAVAILABLE_FOR_SYNC_CONTROL";
    if env::var_os(INNER).is_none() {
        let output = run_exact_test_bounded(
            &format!("notifier::test::{NAME}"),
            &[(INNER, "1")],
            false,
            Duration::from_secs(5),
        )
        .unwrap();
        match classify_exact_reuse_output(Some(&output.output), MARKER, UNAVAILABLE).unwrap() {
            ExactReuseOutcome::Exercised => println!("{MARKER}"),
            ExactReuseOutcome::Unavailable => println!("{UNAVAILABLE}"),
        }
        assert!(!output.timed_out);
        return;
    }
    let (root, tid, release, mut root_cleanup) = legacy_owner_guest();
    match pidfd_open_with_flags(tid.into(), libc::O_EXCL) {
        Ok(reference) => drop(reference),
        Err(Errno::EINVAL) => {
            legacy_owner_reap_root(root, &mut root_cleanup);
            println!("{UNAVAILABLE}");
            return;
        }
        Err(error) => panic!("native sync reference refused: {error}"),
    }
    let force = LegacyThreadGroup::new(root);
    let running = Running::seize_on_ptracer_thread(tid.into(), legacy_thread_options()).unwrap();
    let generation = running.generation();
    let original = running.1.event().clone();
    let legacy = TerminalCleanup::new_unregistered(tid.into(), &running.1);
    running.interrupt().unwrap();
    let claimed = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    SYNC_WAIT_CLAIM_PAUSES.lock().insert(
        tid.into(),
        EventCapturePause {
            captured: claimed.clone(),
            resume: resume.clone(),
        },
    );
    let controller_original = original.clone();
    let controller = thread::spawn(move || {
        claimed.wait();
        assert_eq!(
            controller_original
                .event()
                .wait_owner
                .load(Ordering::Acquire),
            WAIT_OWNER_SYNC
        );
        assert_eq!(
            controller_original
                .event()
                .worker_state
                .load(Ordering::Acquire),
            WORKER_NOT_STARTED
        );
        let native = Running::try_new(tid.into()).unwrap();
        assert_eq!(*native.1.event(), controller_original);
        assert!(matches!(
            native.1.event().identity().unwrap().pidfd,
            ThreadHandle::Pidfd(_)
        ));
        assert_eq!(
            controller_original
                .event()
                .wait_owner
                .load(Ordering::Acquire),
            WAIT_OWNER_SYNC
        );
        assert_eq!(SPAWN_WORKER_COUNTS.lock().get(&tid.into()).copied(), None);
        let observe = WaitPidFlag::from_bits_retain(
            WaitPidFlag::WSTOPPED.bits()
                | WaitPidFlag::WNOHANG.bits()
                | WaitPidFlag::WNOWAIT.bits()
                | libc::__WALL,
        );
        let identity = native.1.event().identity().unwrap();
        legacy_owner_until(|| identity.pidfd.wait_status(observe).unwrap().is_some());
        // Admission retains the same actual synchronous claim; no native
        // worker consumes the report before that owner returns its stop.
        assert_eq!(
            controller_original
                .event()
                .wait_owner
                .load(Ordering::Acquire),
            WAIT_OWNER_SYNC
        );
        assert_eq!(
            controller_original
                .event()
                .worker_state
                .load(Ordering::Acquire),
            WORKER_NOT_STARTED
        );
        resume.wait();
        native
    });
    let mut sync_wait = running.wait_sync_on_ptracer_thread().into_driver();
    let (stopped, event) = sync_wait.wait_on_ptracer_thread().unwrap().assume_stopped();
    assert_eq!(event, crate::Event::Stop);
    assert_eq!(stopped.generation(), generation);
    assert_eq!(
        original.event().wait_owner.load(Ordering::Acquire),
        WAIT_OWNER_NONE
    );
    assert_eq!(SPAWN_WORKER_COUNTS.lock().get(&tid.into()).copied(), None);
    let native = controller.join().unwrap();
    let running = stopped.resume(None).unwrap();
    running.interrupt().unwrap();
    drop(running);
    let stopped = thread::spawn(move || {
        let (stopped, event) = futures::executor::block_on(native.wait_owned())
            .unwrap()
            .assume_stopped();
        assert_eq!(event, crate::Event::Stop);
        stopped
    })
    .join()
    .unwrap();
    assert_eq!(stopped.generation(), generation);
    assert_eq!(legacy.has_thread_pidfd(), Ok(false));
    let native_terminal = stopped.terminal_cleanup();
    assert_eq!(native_terminal.has_thread_pidfd(), Ok(true));
    let exit = stopped.exit_event();
    let running = stopped.resume(None).unwrap();
    assert_eq!(
        unsafe { libc::write(release.as_raw_fd(), b"x".as_ptr().cast(), 1) },
        1
    );
    let stopped = thread::spawn(move || futures::executor::block_on(exit).unwrap())
        .join()
        .unwrap();
    assert_eq!(stopped.getevent().unwrap(), 23 << 8);
    drop(running);
    let running = stopped.resume(None).unwrap();
    assert!(
        matches!(thread::spawn(move || futures::executor::block_on(running.wait_owned()).unwrap()).join().unwrap(),
        Wait::Exited(pid, crate::ExitStatus::Exited(23)) if pid == tid.into())
    );
    assert!(native_terminal.wait(TRACEE_WAIT_TIMEOUT));
    assert!(legacy.wait(TRACEE_WAIT_TIMEOUT));
    assert_eq!(
        SPAWN_WORKER_COUNTS.lock().get(&tid.into()).copied(),
        Some(1)
    );
    drop(force);
    legacy_owner_reap_root(root, &mut root_cleanup);
    assert!(!std::path::Path::new(&format!("/proc/{tid}")).exists());
    println!("{MARKER}");
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(sanitized))]
async fn explicit_cleanup_retains_identity_before_starting_any_wait_owner() {
    if run_legacy_test_outer("explicit_cleanup_retains_identity_before_starting_any_wait_owner") {
        return;
    }
    for forced in [false, true] {
        let (root, tid, release, mut root_cleanup) = legacy_owner_guest();
        let _force = forced.then(|| LegacyThreadGroup::new(root));
        let running =
            Running::seize_on_ptracer_thread(tid.into(), legacy_thread_options()).unwrap();
        let original = running.1.event().clone();
        let generation = running.generation();
        let mut cleanup = running.terminal_cleanup_on_ptracer_thread().into_driver();
        assert!(cleanup.shared().event == original);
        assert_eq!(
            original.event().worker_state.load(Ordering::Acquire),
            WORKER_NOT_STARTED
        );
        assert_eq!(
            original.event().wait_owner.load(Ordering::Acquire),
            WAIT_OWNER_NONE
        );
        assert_eq!(SPAWN_WORKER_COUNTS.lock().get(&tid.into()).copied(), None);
        running.interrupt().unwrap();
        cleanup.progress_on_ptracer_thread().unwrap();
        assert_eq!(
            SPAWN_WORKER_COUNTS.lock().get(&tid.into()).copied(),
            Some(1)
        );
        let (stopped, event) =
            tokio::time::timeout(TRACEE_WAIT_TIMEOUT, running.wait_owned_on_ptracer_thread())
                .await
                .unwrap()
                .unwrap()
                .assume_stopped();
        assert_eq!(event, crate::Event::Stop);
        assert_eq!(stopped.generation(), generation);
        let exit = stopped.exit_event_on_ptracer_thread();
        let running = stopped.resume(None).unwrap();
        assert_eq!(
            unsafe { libc::write(release.as_raw_fd(), b"x".as_ptr().cast(), 1) },
            1
        );
        let stopped = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, exit)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stopped.getevent().unwrap(), 23 << 8);
        drop(running);
        assert!(matches!(tokio::time::timeout(TRACEE_WAIT_TIMEOUT,
            stopped.resume(None).unwrap().wait_owned_on_ptracer_thread()).await.unwrap().unwrap(),
            Wait::Exited(pid, crate::ExitStatus::Exited(23)) if pid == tid.into()));
        assert!(cleanup.wait_on_ptracer_thread(TRACEE_WAIT_TIMEOUT).unwrap());
        legacy_owner_reap_root(root, &mut root_cleanup);
        assert!(!std::path::Path::new(&format!("/proc/{tid}")).exists());
    }
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(sanitized))]
async fn explicit_child_capture_policy_follows_each_request_before_parent_commit() {
    const NAME: &str = "explicit_child_capture_policy_follows_each_request_before_parent_commit";
    const INNER: &str = "SAFEPTRACE_CHILD_CAPTURE_POLICY_INNER";
    const MARKER: &str = "ACTUAL_REQUEST_CHILD_CAPTURE_POLICY_EXERCISED";
    const UNAVAILABLE: &str = "NATIVE_THREAD_PIDFD_UNAVAILABLE_FOR_CHILD_POLICY_CONTROL";
    if env::var_os(INNER).is_none() {
        let output = run_exact_test_bounded(
            &format!("notifier::test::{NAME}"),
            &[(INNER, "1")],
            false,
            Duration::from_secs(5),
        )
        .unwrap();
        match classify_exact_reuse_output(Some(&output.output), MARKER, UNAVAILABLE).unwrap() {
            ExactReuseOutcome::Exercised => println!("{MARKER}"),
            ExactReuseOutcome::Unavailable => println!("{UNAVAILABLE}"),
        }
        assert!(!output.timed_out);
        return;
    }
    async fn finish(stopped: Stopped, deadline: Instant) {
        let terminal = stopped.terminal_cleanup();
        let exit = stopped.exit_event_on_ptracer_thread();
        terminal.request_sigkill().unwrap();
        let stopped = tokio::time::timeout_at(deadline.into(), exit)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stopped.getevent().unwrap(), libc::SIGKILL as i64);
        assert_eq!(
            tokio::time::timeout_at(
                deadline.into(),
                stopped.resume(None).unwrap().wait_owned_on_ptracer_thread()
            )
            .await
            .unwrap()
            .unwrap()
            .assume_exited(),
            (
                terminal.pid,
                crate::ExitStatus::Signaled(Signal::SIGKILL, false)
            )
        );
        assert!(terminal.wait(deadline.saturating_duration_since(Instant::now())));
        assert!(!std::path::Path::new(&format!("/proc/{}", terminal.pid)).exists());
    }
    for explicit_request in [false, true] {
        let deadline = Instant::now() + TRACEE_WAIT_TIMEOUT;
        let root = match unsafe { fork() }.unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                crate::traceme_and_stop().unwrap();
                // The genuine event parent is this tracee; CLONE_PARENT gives
                // the harness actual natural-parent reap authority as well.
                if unsafe {
                    libc::syscall(
                        libc::SYS_clone,
                        libc::CLONE_PARENT | libc::SIGCHLD,
                        0,
                        0,
                        0,
                        0,
                    )
                } < 0
                {
                    unsafe { libc::_exit(126) };
                }
                loop {
                    unsafe { libc::pause() };
                }
            }
        };
        let mut root_cleanup = TraceeCleanupGuard::new(root).unwrap();
        let initial = waitpid_status_bounded(root, libc::WUNTRACED, TRACEE_WAIT_TIMEOUT).unwrap();
        assert_eq!(initial, (libc::SIGSTOP << 8) | 0x7f);
        match pidfd_open_with_flags(root.into(), libc::O_EXCL) {
            Ok(reference) => drop(reference),
            Err(Errno::EINVAL) => {
                pidfd_send_signal(&root_cleanup.pidfd, libc::SIGKILL).unwrap();
                let killed = waitpid_status_bounded(root, 0, TRACEE_WAIT_TIMEOUT).unwrap();
                assert!(libc::WIFSIGNALED(killed));
                assert_eq!(libc::WTERMSIG(killed), libc::SIGKILL);
                root_cleanup.disarm();
                println!("{UNAVAILABLE}");
                return;
            }
            Err(error) => panic!("native parent descriptor refused: {error}"),
        }
        let stopped = Stopped::new_unchecked(root.into());
        let generation = stopped.generation();
        stopped
            .setoptions(
                Options::PTRACE_O_TRACEFORK
                    | Options::PTRACE_O_TRACEEXIT
                    | Options::PTRACE_O_EXITKILL,
            )
            .unwrap();
        // Merely retaining an explicit facade must not turn a subsequent
        // generic request into an implicit legacy child capture.
        let mut local = stopped.terminal_cleanup_on_ptracer_thread().into_driver();
        let terminal = stopped.terminal_cleanup();
        let running = stopped.resume(None).unwrap();
        root_cleanup.bind_running_notifier(&running).unwrap();
        let fork_status = (libc::PTRACE_EVENT_FORK << 16) | (libc::SIGTRAP << 8) | 0x7f;
        legacy_owner_until(|| terminal.queued_raw_statuses() == [fork_status]);
        let child_pid = super::Pid::from_raw(nix::sys::ptrace::getevent(root).unwrap() as i32);
        let child_native_before = PIDFD_OPEN_ERRORS.lock().insert(child_pid, Errno::EINVAL);
        assert!(child_native_before.is_none());
        let forced = explicit_request.then(|| LegacyThreadGroup::new(child_pid.into()));
        let (parent, child) = if explicit_request {
            let pending = local
                .reserve_pending_on_ptracer_thread(Duration::ZERO)
                .unwrap()
                .unwrap();
            let (parent, event) = pending.decode().unwrap().assume_stopped();
            let crate::Event::NewChild(crate::ChildOp::Fork, child) = event else {
                panic!("real fork request lost its child");
            };
            assert_eq!(child.1.policy, WaitPolicy::PtracerThread);
            assert!(child.1.event().identity().is_some());
            assert!(child.1.ptracer_owner.is_some());
            assert_eq!(
                child
                    .terminal_cleanup_on_ptracer_thread()
                    .shared()
                    .has_thread_pidfd(),
                Ok(false)
            );
            assert_eq!(parent.generation(), generation);
            assert_eq!(
                pending.inner.state.pending.front().copied(),
                Some(fork_status),
                "child identity must be captured before parent FIFO commit"
            );
            pending.commit();
            drop(running);
            (parent, child)
        } else {
            let mut original = running.wait_owned();
            assert!(matches!(
                tokio::time::timeout_at(deadline.into(), &mut original)
                    .await
                    .unwrap(),
                Err(OwnedWaitError::Errno(Errno::EINVAL))
            ));
            assert!(original.inner.is_some());
            assert_eq!(
                original.inner.as_ref().unwrap().token.policy,
                WaitPolicy::Native
            );
            assert_eq!(terminal.queued_raw_statuses(), [fork_status]);
            assert_eq!(terminal.registration_error(), None);
            let (parent, event) = tokio::time::timeout_at(deadline.into(), &mut original)
                .await
                .unwrap()
                .unwrap()
                .assume_stopped();
            let crate::Event::NewChild(crate::ChildOp::Fork, child) = event else {
                panic!("generic retry lost its original fork front");
            };
            assert_eq!(child.1.policy, WaitPolicy::Native);
            assert!(matches!(
                child.1.event().identity().unwrap().pidfd,
                ThreadHandle::Pidfd(_)
            ));
            (parent, child)
        };
        assert!(
            PIDFD_OPEN_ERRORS.lock().get(&child_pid).is_none(),
            "actual child thread-open refusal was consumed"
        );
        assert!(terminal.queued_raw_statuses().is_empty());
        assert_eq!(parent.generation(), generation);
        let before_local = child.generation();
        let (child, event) =
            tokio::time::timeout_at(deadline.into(), child.wait_owned_on_ptracer_thread())
                .await
                .unwrap()
                .unwrap()
                .assume_stopped();
        assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));
        assert_eq!(
            child.generation(),
            before_local,
            "generic-native to explicit request keeps original generation equality"
        );
        let mut before_hash = DefaultHasher::new();
        before_local.hash(&mut before_hash);
        let mut after_hash = DefaultHasher::new();
        child.generation().hash(&mut after_hash);
        assert_eq!(before_hash.finish(), after_hash.finish());
        finish(child, deadline).await;
        finish(parent, deadline).await;
        root_cleanup.disarm();
        drop(forced);
        assert!(Instant::now() <= deadline);
    }
    println!("{MARKER}");
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(sanitized))]
async fn explicit_native_reattach_preserves_current_owner_and_original_driver() {
    const NAME: &str = "explicit_native_reattach_preserves_current_owner_and_original_driver";
    const INNER: &str = "SAFEPTRACE_NATIVE_REATTACH_INNER";
    const MARKER: &str = "ACTUAL_EXPLICIT_NATIVE_REATTACH_EXERCISED";
    const UNAVAILABLE: &str = "NATIVE_THREAD_PIDFD_UNAVAILABLE_FOR_REATTACH_CONTROL";
    if env::var_os(INNER).is_none() {
        let output = run_exact_test_bounded(
            &format!("notifier::test::{NAME}"),
            &[(INNER, "1")],
            false,
            Duration::from_secs(5),
        )
        .unwrap();
        match classify_exact_reuse_output(Some(&output.output), MARKER, UNAVAILABLE).unwrap() {
            ExactReuseOutcome::Exercised => println!("{MARKER}"),
            ExactReuseOutcome::Unavailable => println!("{UNAVAILABLE}"),
        }
        assert!(!output.timed_out);
        return;
    }
    for method in [LegacyAttachMethod::Attach, LegacyAttachMethod::Seize] {
        for changed_owner in [false, true] {
            let (root, tid, release, mut root_cleanup) = legacy_owner_guest();
            match pidfd_open_with_flags(tid.into(), libc::O_EXCL) {
                Ok(reference) => drop(reference),
                Err(Errno::EINVAL) => {
                    legacy_owner_reap_root(root, &mut root_cleanup);
                    println!("{UNAVAILABLE}");
                    return;
                }
                Err(error) => panic!("native reattachment reference refused: {error}"),
            }
            let running =
                Running::seize_on_ptracer_thread(tid.into(), legacy_thread_options()).unwrap();
            assert_eq!(running.terminal_cleanup().has_thread_pidfd(), Ok(true));
            running.interrupt().unwrap();
            let (stopped, event) =
                tokio::time::timeout(TRACEE_WAIT_TIMEOUT, running.wait_owned_on_ptracer_thread())
                    .await
                    .unwrap()
                    .unwrap()
                    .assume_stopped();
            assert_eq!(event, crate::Event::Stop);
            let generation = stopped.generation();
            let terminal = stopped.terminal_cleanup();
            let event = terminal.event.event().clone();
            let count = SPAWN_WORKER_COUNTS
                .lock()
                .get(&tid.into())
                .copied()
                .unwrap();
            let mut original = stopped
                .detach(None)
                .unwrap()
                .wait_owned_on_ptracer_thread()
                .into_driver();
            let original_owner = original.affinity.owner.as_ref().unwrap().tid;
            assert_eq!(original_owner, super::Pid::from(nix::unistd::gettid()));
            let release_fd = release.as_raw_fd();
            if changed_owner {
                let (captured, capture) = mpsc::sync_channel(1);
                let (resume, resumed) = mpsc::sync_channel(1);
                let owner = thread::spawn(move || {
                    let running = if method == LegacyAttachMethod::Attach {
                        Running::attach_on_ptracer_thread(tid.into()).unwrap()
                    } else {
                        let running =
                            Running::seize_on_ptracer_thread(tid.into(), legacy_thread_options())
                                .unwrap();
                        running.interrupt().unwrap();
                        running
                    };
                    let current_owner = super::Pid::from(nix::unistd::gettid());
                    let fresh = running.terminal_cleanup();
                    captured.send((current_owner, fresh)).unwrap();
                    resumed.recv_timeout(TRACEE_WAIT_TIMEOUT).unwrap();
                    tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()
                        .unwrap()
                        .block_on(legacy_owner_finish_member(running, tid, release_fd, method))
                });
                let (new_owner, fresh) = capture.recv_timeout(TRACEE_WAIT_TIMEOUT).unwrap();
                assert_ne!(new_owner, original_owner);
                assert!(fresh.same_generation(&terminal));
                assert!(Arc::ptr_eq(fresh.event.event(), &event));
                legacy_owner_until(|| !terminal.queued_raw_statuses().is_empty());
                let pending_before = terminal.queued_raw_statuses();
                let waker = futures::task::noop_waker();
                for _ in 0..2 {
                    assert!(matches!(
                        original.poll_on_ptracer_thread(&mut Context::from_waker(&waker)),
                        Poll::Ready(Err(OwnedWaitError::Errno(Errno::EPERM)))
                    ));
                    assert_eq!(
                        original.inner.inner.as_ref().unwrap().token.event(),
                        &terminal.event
                    );
                    assert_eq!(
                        original.affinity.owner.as_ref().unwrap().tid,
                        original_owner
                    );
                    assert_eq!(
                        terminal.queued_raw_statuses(),
                        pending_before,
                        "old owner's refusal must leave the new owner's actual stop untouched"
                    );
                }
                resume.send(()).unwrap();
                let fresh = owner.join().unwrap();
                assert!(fresh.same_generation(&terminal));
                assert_eq!(
                    fresh.observed_exit_status(),
                    Ok(Some(crate::ExitStatus::Exited(23)))
                );
            } else {
                let running = if method == LegacyAttachMethod::Attach {
                    Running::attach_on_ptracer_thread(tid.into()).unwrap()
                } else {
                    let running =
                        Running::seize_on_ptracer_thread(tid.into(), legacy_thread_options())
                            .unwrap();
                    running.interrupt().unwrap();
                    running
                };
                assert_eq!(running.1.event(), &terminal.event);
                drop(running);
                let (stopped, decoded) = tokio::time::timeout(
                    TRACEE_WAIT_TIMEOUT,
                    futures::future::poll_fn(|cx| original.poll_on_ptracer_thread(cx)),
                )
                .await
                .unwrap()
                .unwrap()
                .assume_stopped();
                assert_eq!(
                    decoded,
                    if method == LegacyAttachMethod::Attach {
                        crate::Event::Signal(Signal::SIGSTOP)
                    } else {
                        crate::Event::Stop
                    }
                );
                assert_eq!(stopped.generation(), generation);
                stopped.setoptions(legacy_thread_options()).unwrap();
                let exit = stopped.exit_event_on_ptracer_thread();
                assert_eq!(
                    unsafe { libc::write(release_fd, b"x".as_ptr().cast(), 1) },
                    1
                );
                let running = stopped.resume(None).unwrap();
                let stopped = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, exit)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(stopped.getevent().unwrap(), 23 << 8);
                drop(running);
                assert_eq!(
                    tokio::time::timeout(
                        TRACEE_WAIT_TIMEOUT,
                        stopped.resume(None).unwrap().wait_owned_on_ptracer_thread()
                    )
                    .await
                    .unwrap()
                    .unwrap()
                    .assume_exited(),
                    (tid.into(), crate::ExitStatus::Exited(23))
                );
            }
            assert!(terminal.wait(TRACEE_WAIT_TIMEOUT));
            assert_eq!(
                terminal.observed_exit_status(),
                Ok(Some(crate::ExitStatus::Exited(23)))
            );
            assert_eq!(
                SPAWN_WORKER_COUNTS.lock().get(&tid.into()).copied(),
                Some(count)
            );
            legacy_owner_reap_root(root, &mut root_cleanup);
            assert!(!std::path::Path::new(&format!("/proc/{tid}")).exists());
            drop(release);
        }
    }
    println!("{MARKER}");
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(sanitized))]
async fn explicit_copied_owner_namespace_refuses_before_consuming_any_report() {
    const NAME: &str = "explicit_copied_owner_namespace_refuses_before_consuming_any_report";
    const INNER: &str = "SAFEPTRACE_COPIED_OWNER_NAMESPACE_INNER";
    const MARKER: &str = "ACTUAL_EXPLICIT_COPIED_OWNER_NAMESPACE_REFUSAL_EXERCISED";
    if env::var_os(INNER).is_none() {
        let output =
            run_exact_in_pid_namespace_bounded(&format!("notifier::test::{NAME}"), &[(INNER, "1")])
                .expect("start copied-owner namespace control");
        assert!(output.status.success(), "copied-owner control: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == MARKER)
        );
        print!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    assert_eq!(nix::unistd::getpid().as_raw(), 1);
    assert_eq!(nix::unistd::gettid().as_raw(), 2);
    for forced in [false, true] {
        for realign_proc in [false, true] {
            let (root, tid, release, mut root_cleanup) = legacy_owner_guest();
            let _force = forced.then(|| LegacyThreadGroup::new(root));
            let running =
                Running::seize_on_ptracer_thread(tid.into(), legacy_thread_options()).unwrap();
            let terminal = running.terminal_cleanup();
            let native = terminal.has_thread_pidfd().unwrap();
            if forced {
                assert!(
                    !native,
                    "forced copied-owner cell did not select legacy mode"
                );
            }
            terminal.ensure_registered().unwrap();
            let original = running.1.event().clone();
            let sync = Running::from_token(running.0, running.1.clone())
                .wait_sync_on_ptracer_thread()
                .into_driver();
            let exit = running.exit_event_on_ptracer_thread().into_driver();
            let cleanup = running.terminal_cleanup_on_ptracer_thread().into_driver();
            running.interrupt().unwrap();
            let mut owned = running.wait_owned_on_ptracer_thread().into_driver();
            let owner = owned.affinity.owner.as_ref().unwrap().clone();
            assert_eq!(owner.tid.as_raw(), 2);
            assert!(owner.is_current().unwrap());
            let (resume, capture) = if native {
                legacy_owner_until(|| !terminal.queued_raw_statuses().is_empty());
                (None, None)
            } else {
                // Park the original observer outside its progress/status
                // locks before copying the process. The child never uses
                // the copied observer's channels or publication thread.
                let (captured, capture) = mpsc::sync_channel(1);
                let (resume, resumed) = mpsc::sync_channel(1);
                *original.event().legacy_worker_cycle_pause.lock() = Some(BoundedTestPause {
                    captured,
                    resume: resumed,
                });
                capture.recv_timeout(TRACEE_WAIT_TIMEOUT).unwrap();
                (Some(resume), Some(capture))
            };
            // libc fork first leaves one host thread. Only that intermediate
            // child unshares its own future PID/mount namespace; the original
            // owner A remains live and holds its actual original tracee.
            let intermediate = match unsafe { fork() }.unwrap() {
                ForkResult::Parent { child } => child,
                ForkResult::Child => {
                    assert_eq!(
                        unsafe { libc::unshare(libc::CLONE_NEWPID | libc::CLONE_NEWNS) },
                        0
                    );
                    assert_eq!(
                        unsafe {
                            libc::mount(
                                std::ptr::null(),
                                c"/".as_ptr(),
                                std::ptr::null(),
                                libc::MS_REC | libc::MS_PRIVATE,
                                std::ptr::null(),
                            )
                        },
                        0
                    );
                    let inner = match unsafe { fork() }.unwrap() {
                        ForkResult::Parent { child } => child,
                        ForkResult::Child => {
                            assert_eq!(nix::unistd::getpid().as_raw(), 1);
                            if realign_proc {
                                assert_eq!(
                                    unsafe {
                                        libc::mount(
                                            c"proc".as_ptr(),
                                            c"/proc".as_ptr(),
                                            c"proc".as_ptr(),
                                            0,
                                            std::ptr::null(),
                                        )
                                    },
                                    0
                                );
                            }
                            let proof = thread::spawn(move || {
                                let mut sync = sync;
                                let mut exit = exit;
                                let mut cleanup = cleanup;
                                assert_eq!(nix::unistd::gettid().as_raw(), owner.tid.as_raw());
                                assert_eq!(nix::unistd::getpid().as_raw(), owner.tgid.as_raw());
                                assert!(
                                    owner.is_live().unwrap(),
                                    "original A retired during the control"
                                );
                                assert_eq!(
                                    original.identity().unwrap().current_tracer_pid(),
                                    Ok(owner.tid)
                                );
                                assert_eq!(original.identity().unwrap().pidfd_is_live(), Ok(true));
                                assert_eq!(
                                    require_aligned_proc_pid_namespace(),
                                    if realign_proc {
                                        Ok(())
                                    } else {
                                        Err(Errno::EXDEV)
                                    }
                                );
                                // Same namespace-relative gettid is not the
                                // same host task in the original pinned mount.
                                assert!(!owner.is_current().unwrap());
                                let (replacement, replacement_cleanup, stop) =
                                    legacy_owner_replacement(Some(tid))
                                        .expect("actual inner same-number child");
                                assert_eq!(replacement, tid);
                                let pending: Vec<_> = original
                                    .event()
                                    .status
                                    .lock()
                                    .pending
                                    .iter()
                                    .copied()
                                    .collect();
                                let exit_epoch =
                                    original.event().exit_epoch.load(Ordering::Acquire);
                                let waker = futures::task::noop_waker();
                                for _ in 0..2 {
                                    assert!(matches!(
                                        owned.poll_on_ptracer_thread(&mut Context::from_waker(
                                            &waker
                                        )),
                                        Poll::Ready(Err(OwnedWaitError::Errno(Errno::EPERM)))
                                    ));
                                    assert!(matches!(
                                        exit.poll_on_ptracer_thread(&mut Context::from_waker(
                                            &waker
                                        )),
                                        Poll::Ready(Err(Error::Errno(Errno::EPERM)))
                                    ));
                                    assert_eq!(
                                        cleanup.progress_on_ptracer_thread(),
                                        Err(Errno::EPERM)
                                    );
                                    assert!(matches!(
                                        cleanup.reserve_pending_on_ptracer_thread(Duration::ZERO),
                                        Err(Errno::EPERM)
                                    ));
                                    assert!(matches!(
                                        sync.wait_on_ptracer_thread(),
                                        Err(OwnedWaitError::Errno(Errno::EPERM))
                                    ));
                                    assert_eq!(
                                        owned.inner.inner.as_ref().unwrap().token.event(),
                                        &original
                                    );
                                    assert_eq!(sync.input.as_ref().unwrap().1.event(), &original);
                                    assert_eq!(
                                        original
                                            .event()
                                            .status
                                            .lock()
                                            .pending
                                            .iter()
                                            .copied()
                                            .collect::<Vec<_>>(),
                                        pending
                                    );
                                    assert_eq!(
                                        original.event().exit_epoch.load(Ordering::Acquire),
                                        exit_epoch
                                    );
                                    assert_eq!(
                                        legacy_owner_observe(replacement, WaitPidFlag::WSTOPPED),
                                        stop,
                                        "copied authority consumed an inner replacement report"
                                    );
                                }
                                // Report preservation above is checked in
                                // the inherited mount as well. Align only
                                // this child's cleanup view afterward so
                                // the unchanged actual-reap/absence helper
                                // names B's reaped child rather than A's
                                // still-live ancestor target.
                                if !realign_proc {
                                    assert_eq!(
                                        unsafe {
                                            libc::mount(
                                                c"proc".as_ptr(),
                                                c"/proc".as_ptr(),
                                                c"proc".as_ptr(),
                                                0,
                                                std::ptr::null(),
                                            )
                                        },
                                        0
                                    );
                                }
                                legacy_owner_finish_replacement(
                                    replacement,
                                    replacement_cleanup,
                                    stop,
                                );
                                assert!(owner.is_live().unwrap());
                                assert_eq!(original.identity().unwrap().pidfd_is_live(), Ok(true));
                                println!(
                                    "COPIED_OWNER_REFUSAL native={native} forced={forced} realigned={realign_proc} current_tid={} original_tid={} original_live=true target_live=true replacement_stop_preserved=true replacement_exit=42",
                                    nix::unistd::gettid(),
                                    owner.tid
                                );
                            });
                            proof.join().unwrap();
                            unsafe { libc::_exit(0) };
                        }
                    };
                    let status = waitpid_status_bounded(inner, 0, TRACEE_WAIT_TIMEOUT).unwrap();
                    assert!(libc::WIFEXITED(status));
                    assert_eq!(libc::WEXITSTATUS(status), 0);
                    unsafe { libc::_exit(0) };
                }
            };
            let status = waitpid_status_bounded(intermediate, 0, TRACEE_WAIT_TIMEOUT).unwrap();
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0);
            if let Some(resume) = resume {
                resume.send(()).unwrap();
            }
            drop(capture);
            // A's SAME original driver remains usable after every foreign
            // copy refuses. Finish actual original stop, EXIT23 and DONE.
            let (stopped, event) = tokio::time::timeout(
                TRACEE_WAIT_TIMEOUT,
                futures::future::poll_fn(|cx| owned.poll_on_ptracer_thread(cx)),
            )
            .await
            .unwrap()
            .unwrap()
            .assume_stopped();
            assert_eq!(event, crate::Event::Stop);
            let exit = stopped.exit_event_on_ptracer_thread();
            let running = stopped.resume(None).unwrap();
            assert_eq!(
                unsafe { libc::write(release.as_raw_fd(), b"x".as_ptr().cast(), 1) },
                1
            );
            let stopped = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, exit)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stopped.getevent().unwrap(), 23 << 8);
            drop(running);
            assert_eq!(
                tokio::time::timeout(
                    TRACEE_WAIT_TIMEOUT,
                    stopped.resume(None).unwrap().wait_owned_on_ptracer_thread()
                )
                .await
                .unwrap()
                .unwrap()
                .assume_exited(),
                (tid.into(), crate::ExitStatus::Exited(23))
            );
            assert!(terminal.wait(TRACEE_WAIT_TIMEOUT));
            legacy_owner_reap_root(root, &mut root_cleanup);
            assert!(!std::path::Path::new(&format!("/proc/{tid}")).exists());
        }
    }
    println!("{MARKER}");
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(sanitized))]
async fn explicit_original_owner_progresses_through_retained_proc_overmount() {
    const NAME: &str = "explicit_original_owner_progresses_through_retained_proc_overmount";
    const INNER: &str = "SAFEPTRACE_OWNER_PROC_OVERMOUNT_INNER";
    const MARKER: &str = "ACTUAL_EXPLICIT_OWNER_PROC_OVERMOUNT_EXERCISED";
    if env::var_os(INNER).is_none() {
        let output =
            run_exact_in_pid_namespace_bounded(&format!("notifier::test::{NAME}"), &[(INNER, "1")])
                .expect("start original-owner overmount control");
        assert!(
            output.status.success(),
            "owner overmount control: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == MARKER)
        );
        print!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    for forced in [false, true] {
        let (root, tid, release, mut root_cleanup) = legacy_owner_guest();
        let _force = forced.then(|| LegacyThreadGroup::new(root));
        let running =
            Running::seize_on_ptracer_thread(tid.into(), legacy_thread_options()).unwrap();
        let terminal = running.terminal_cleanup();
        terminal.ensure_registered().unwrap();
        let native = terminal.has_thread_pidfd().unwrap();
        if forced {
            assert!(!native, "forced overmount cell did not select legacy mode");
        }
        let generation = running.generation();
        let owner = running.1.ptracer_owner.as_ref().unwrap().clone();
        let mut cleanup = running.terminal_cleanup_on_ptracer_thread().into_driver();
        let mut owned = running.wait_owned_on_ptracer_thread().into_driver();
        assert_eq!(
            unsafe {
                libc::mount(
                    c"tmpfs".as_ptr(),
                    c"/proc".as_ptr(),
                    c"tmpfs".as_ptr(),
                    libc::MS_NOSUID | libc::MS_NODEV,
                    c"size=4096".as_ptr().cast(),
                )
            },
            0
        );
        assert!(
            matches!(fs::read("/proc/thread-self/status"), Err(error) if error.raw_os_error() == Some(libc::ENOENT))
        );
        assert_eq!(require_aligned_proc_pid_namespace(), Err(Errno::EXDEV));
        assert!(
            owner.is_current().unwrap(),
            "a later absolute overmount changed the original host identity"
        );
        assert_eq!(
            owner.proc_root.current_thread_status().unwrap().pid,
            owner.tid
        );
        // Retained root/target/host descriptors permit the actual original
        // owner to register progress, consume a stop, resume and reap.
        let event_handle = owned.inner.inner.as_ref().unwrap().token.event();
        Running::from_token(
            tid.into(),
            owned.inner.inner.as_ref().unwrap().token.clone(),
        )
        .interrupt()
        .unwrap();
        let event_before = event_handle.clone();
        cleanup.progress_on_ptracer_thread().unwrap();
        let (stopped, event) = tokio::time::timeout(
            TRACEE_WAIT_TIMEOUT,
            futures::future::poll_fn(|cx| owned.poll_on_ptracer_thread(cx)),
        )
        .await
        .unwrap()
        .unwrap()
        .assume_stopped();
        assert_eq!(event, crate::Event::Stop);
        assert_eq!(stopped.generation(), generation);
        assert_eq!(stopped.1.event(), &event_before);
        let exit = stopped.exit_event_on_ptracer_thread();
        let running = stopped.resume(None).unwrap();
        assert_eq!(
            unsafe { libc::write(release.as_raw_fd(), b"x".as_ptr().cast(), 1) },
            1
        );
        let stopped = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, exit)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stopped.getevent().unwrap(), 23 << 8);
        drop(running);
        assert_eq!(
            tokio::time::timeout(
                TRACEE_WAIT_TIMEOUT,
                stopped.resume(None).unwrap().wait_owned_on_ptracer_thread()
            )
            .await
            .unwrap()
            .unwrap()
            .assume_exited(),
            (tid.into(), crate::ExitStatus::Exited(23))
        );
        assert!(cleanup.wait_on_ptracer_thread(TRACEE_WAIT_TIMEOUT).unwrap());
        assert_eq!(
            terminal.observed_exit_status(),
            Ok(Some(crate::ExitStatus::Exited(23)))
        );
        assert_eq!(
            terminal.event.identity().unwrap().pidfd_is_live(),
            Ok(false)
        );
        assert_eq!(
            unsafe { libc::umount2(c"/proc".as_ptr(), libc::MNT_DETACH) },
            0
        );
        require_aligned_proc_pid_namespace().unwrap();
        legacy_owner_reap_root(root, &mut root_cleanup);
        assert!(!std::path::Path::new(&format!("/proc/{tid}")).exists());
        println!(
            "ORIGINAL_OWNER_OVERMOUNT native={native} forced={forced} actual_exit=23 done=true root_reaped=true member_absent=true"
        );
    }
    println!("{MARKER}");
}
