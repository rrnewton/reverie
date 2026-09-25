/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

mod owned_deferred_tests {
    use std::os::fd::FromRawFd;
    use std::os::fd::IntoRawFd;

    use super::*;

    // The harness is threaded. Run each API caller in a separate single-thread
    // process, before that process creates any workers. The outer native clone
    // is the same containment mechanism used by the existing serializer test;
    // this does not establish universal fork safety for arbitrary harnesses.
    fn isolated(test: fn(Instant)) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stack = child_stack();
        let child = super::super::super::clone::clone_with_stack_owned(
            || {
                assert_eq!(std::fs::read_dir("/proc/self/task").unwrap().count(), 1);
                test(deadline);
                assert!(Instant::now() < deadline);
                0
            },
            Namespace::USER | Namespace::PID,
            &mut stack,
        )
        .unwrap();
        let mut child = OwnedContainerCleanup::new(child);
        let original = child.wait_until(deadline);
        eprintln!("owned-deferred isolated original={original:?}");
        if original != ChildCleanupObservation::Reaped(ExitStatus::Exited(0)) {
            // Rescue is separate evidence, never a successful original result.
            let rescue = child.cancel_and_wait_until(Instant::now() + Duration::from_secs(2));
            eprintln!("owned-deferred isolated rescue={rescue:?}");
        }
        assert_eq!(
            original,
            ChildCleanupObservation::Reaped(ExitStatus::Exited(0))
        );
    }

    fn wait_flag(flag: &AtomicBool, deadline: Instant) {
        while !flag.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "original fixture deadline");
            std::thread::yield_now();
        }
        assert!(Instant::now() < deadline);
    }

    struct Guard<'a>(&'a std::cell::Cell<bool>);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    #[test]
    fn owned_deferred_large_bytes_pending_then_actual_wait() {
        isolated(|deadline| {
            let (mapping, shared) = new_shared_drop_state();
            let dropped = std::cell::Cell::new(false);
            let guard = Guard(&dropped);
            OWNED_DEFERRED_DRAIN_HOOK.with(|hook| {
                hook.set(Some(|fd| {
                    OWNED_RESULT_PIPE_CAPACITY.with(|capacity| {
                        capacity.set(Some(
                            Errno::result(unsafe { libc::fcntl(fd, libc::F_GETPIPE_SZ) }).unwrap(),
                        ));
                    });
                }))
            });
            let payload = vec![37_u8; 1024 * 1024];
            let run = Container::new()
                .run_with_deferred_drop_owned(&mut || (payload.clone(), BlockingDrop { shared }))
                .unwrap();
            let bytes = run.provisional_bytes().to_vec();
            let capacity = OWNED_RESULT_PIPE_CAPACITY
                .with(|value| value.get())
                .unwrap();
            assert!(bytes.len() > capacity as usize);
            let pid = run.cleanup().child_pid();
            let fd = run.cleanup().pidfd.as_ref().unwrap().as_raw_fd();
            let pending = match run.finalize_until(Instant::now()) {
                OwnedFinalize::Pending(pending) => pending,
                other => panic!("held child was {}", outcome_kind(&other)),
            };
            assert!(!dropped.get());
            assert_eq!(pending.cleanup().child_pid(), pid);
            assert_eq!(pending.cleanup().pidfd.as_ref().unwrap().as_raw_fd(), fd);
            assert_eq!(pending.provisional_bytes(), bytes);
            assert!(pending.result_eof());
            unsafe { &*shared }.release.store(true, Ordering::Release);
            let complete = match pending.retry_until(deadline) {
                OwnedFinalize::Complete(complete) => complete,
                other => panic!("released child was {}", outcome_kind(&other)),
            };
            assert_eq!(complete.status(), ExitStatus::Exited(0));
            assert_eq!(complete.encoded_bytes(), bytes);
            assert_eq!(complete.decode().unwrap(), payload);
            assert!(!dropped.get());
            drop(guard);
            assert!(dropped.get());
            assert_reaped(pid);
            eprintln!(
                "large result bytes={} pipe_capacity={capacity} actual_exit=0",
                bytes.len()
            );
            unsafe { unmap_shared_drop_state(mapping, shared) };
        });
    }

    #[test]
    fn owned_deferred_general_workload_can_join_thread_and_fork() {
        isolated(|deadline| {
            let run = Container::new()
                .unshare(Namespace::PID)
                .run_with_deferred_drop_owned(&mut || (namespace_population_probe(), ()))
                .unwrap();
            let complete = match run.finalize_until(deadline) {
                OwnedFinalize::Complete(complete) => complete,
                other => panic!("joined workers: {}", outcome_kind(&other)),
            };
            assert_eq!(complete.status(), ExitStatus::Exited(0));
            assert_eq!(complete.decode().unwrap(), (1, 2, 3));
        });
    }

    struct ExitGroup;
    impl Drop for ExitGroup {
        fn drop(&mut self) {
            // A CLI-only model. The general library never adds this policy.
            unsafe { libc::_exit(79) }
        }
    }

    #[test]
    fn owned_deferred_namespace_group_exit_retains_diagnosis_and_guards() {
        isolated(|deadline| {
            let dropped = std::cell::Cell::new(false);
            let guard = Guard(&dropped);
            let (parent_socket, child_socket) = StartupSocket::pair(deadline).unwrap();
            let run = Container::new()
                .unshare(Namespace::PID)
                .run_with_deferred_drop_owned(&mut || {
                    assert_eq!(Pid::this().as_raw(), 1);
                    let child = unsafe { libc::fork() };
                    assert!(child >= 0);
                    if child == 0 {
                        loop {
                            unsafe { libc::pause() };
                        }
                    }
                    let descendant = Fd::pidfd_open(child, 0).unwrap();
                    let mut fds = StartupFds::default();
                    fds.push(unsafe {
                        std::os::fd::OwnedFd::from_raw_fd(descendant.into_raw_fd())
                    })
                    .unwrap();
                    child_socket.send(STARTUP_REQUEST, &fds, None).unwrap();
                    let ready = std::sync::Arc::new(AtomicBool::new(false));
                    let ready_worker = ready.clone();
                    let _worker = std::thread::spawn(move || {
                        ready_worker.store(true, Ordering::Release);
                        loop {
                            unsafe { libc::pause() };
                        }
                    });
                    wait_flag(&ready, deadline);
                    (
                        String::from("original cleanup-unconfirmed diagnostic"),
                        ExitGroup,
                    )
                })
                .unwrap();
            assert!(!dropped.get());
            let bytes = run.provisional_bytes().to_vec();
            let expected = bincode::serde::encode_to_vec(
                Ok::<_, StartupError>(String::from("original cleanup-unconfirmed diagnostic")),
                bincode::config::legacy(),
            )
            .unwrap();
            assert_eq!(bytes, expected);
            let pid = run.cleanup().child_pid();
            let failed = match run.finalize_until(deadline) {
                OwnedFinalize::Failed { cause, cleanup } => {
                    assert_eq!(cause, OwnedRunFailure::ChildStatus(ExitStatus::Exited(79)));
                    cleanup
                }
                other => panic!("hard fallback cannot become {}", outcome_kind(&other)),
            };
            assert_eq!(failed.provisional_bytes(), expected);
            assert_eq!(
                failed.cleanup().observation(),
                ChildCleanupObservation::Reaped(ExitStatus::Exited(79))
            );
            let fds = parent_socket.receive(Some(STARTUP_REQUEST)).unwrap();
            assert_eq!(fds.len, 1);
            let mut poll = libc::pollfd {
                fd: fds.values[0].as_ref().unwrap().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
            assert_ne!(poll.revents & libc::POLLIN, 0, "real descendant exit");
            assert!(!dropped.get());
            drop(failed);
            assert_reaped(pid);
            drop(guard);
            assert!(dropped.get());
            eprintln!(
                "hard fallback actual_init_exit=79 descendant_pidfd_ready=true diagnosis_bytes={}",
                bytes.len()
            );
        });
    }

    #[test]
    fn owned_deferred_missing_pidfd_keeps_original_wait_without_cancel() {
        isolated(|deadline| {
            let (mapping, shared) = new_shared_drop_state();
            let _fault = OwnedCloneFaultGuard::install(
                super::super::super::clone::OwnedCloneTestFault::MissingPidfd,
            );
            let failed = Container::new()
                .run_with_deferred_drop_owned(&mut || {
                    unsafe { &*shared }.started.store(true, Ordering::Release);
                    wait_flag(&unsafe { &*shared }.release, deadline);
                    (31_u32, ())
                })
                .unwrap_err();
            let run = match failed {
                StartupOwnedFailure::AfterClone { cause, run } => {
                    assert_eq!(cause, OwnedRunFailure::Startup(StartupError::Protocol));
                    run
                }
                _ => panic!("must retain actual clone"),
            };
            wait_flag(&unsafe { &*shared }.started, deadline);
            assert!(run.cleanup().pidfd.is_none());
            assert!(run.cleanup().wait_owned);
            assert_eq!(
                run.cleanup().observation(),
                ChildCleanupObservation::Pending
            );
            assert_eq!(
                run.cleanup().last_error(),
                None,
                "no implicit cancellation attempt"
            );
            assert!(run.provisional_bytes().is_empty());
            assert!(!run.result_eof());
            let pid = run.cleanup().child_pid();
            unsafe { &*shared }.release.store(true, Ordering::Release);
            match run.retry_until(deadline) {
                OwnedFinalize::Failed { cause, cleanup } => {
                    assert_eq!(cause, OwnedRunFailure::Startup(StartupError::Protocol));
                    assert_eq!(
                        cleanup.cleanup().observation(),
                        ChildCleanupObservation::Reaped(ExitStatus::Exited(0))
                    );
                    assert_eq!(cleanup.cleanup().last_error(), Some(Errno::EBADF));
                    assert!(
                        cleanup.provisional_bytes().is_empty(),
                        "failed owner never redrains"
                    );
                    assert!(!cleanup.result_eof());
                }
                other => panic!("missing identity became {}", outcome_kind(&other)),
            }
            assert_reaped(pid);
            unsafe { unmap_shared_drop_state(mapping, shared) };
        });
    }

    thread_local! {
        static READ_DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
    }
    fn partial_read_hook(fd: RawFd) {
        let deadline = READ_DEADLINE.with(|value| value.get()).unwrap();
        loop {
            let mut available: libc::c_int = 0;
            assert_eq!(
                unsafe { libc::ioctl(fd, libc::FIONREAD, &mut available) },
                0
            );
            if available > 0 {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let flags = Errno::result(unsafe { libc::fcntl(fd, libc::F_GETFL) }).unwrap();
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
    }
    #[derive(Debug)]
    struct HeldSerializer {
        shared: *mut SharedDropState,
        deadline: Instant,
    }
    impl serde::Serialize for HeldSerializer {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            let mut seq = serializer.serialize_seq(Some(32 * 1024))?;
            for _ in 0..16 * 1024 {
                seq.serialize_element(&73_u8)?;
            }
            unsafe { &*self.shared }
                .started
                .store(true, Ordering::Release);
            wait_flag(&unsafe { &*self.shared }.release, self.deadline);
            for _ in 0..16 * 1024 {
                seq.serialize_element(&73_u8)?;
            }
            seq.end()
        }
    }

    #[test]
    fn owned_deferred_real_read_failure_keeps_partial_bytes_reader_and_owner() {
        isolated(|deadline| {
            let (mapping, shared) = new_shared_drop_state();
            READ_DEADLINE.with(|value| value.set(Some(deadline)));
            OWNED_DEFERRED_DRAIN_HOOK.with(|hook| hook.set(Some(partial_read_hook)));
            let failed = Container::new()
                .run_with_deferred_drop_owned(&mut || (HeldSerializer { shared, deadline }, ()))
                .unwrap_err();
            let run = match failed {
                StartupOwnedFailure::AfterClone { cause, run } => {
                    assert_eq!(cause, OwnedRunFailure::ResultRead(Errno::EAGAIN));
                    run
                }
                _ => panic!("expected actual nonblocking read refusal"),
            };
            let expected = bincode::serde::encode_to_vec(
                Ok::<_, StartupError>(vec![73_u8; 32 * 1024]),
                bincode::config::legacy(),
            )
            .unwrap();
            let bytes = run.provisional_bytes().to_vec();
            assert!(!bytes.is_empty());
            assert!(
                bytes.len() < expected.len(),
                "strictly incomplete encoded payload"
            );
            assert!(expected.starts_with(&bytes));
            assert!(run.reader.is_some());
            let reader_fd = run.reader.as_ref().unwrap().as_raw_fd();
            assert!(!run.result_eof());
            assert_eq!(
                run.cleanup().observation(),
                ChildCleanupObservation::Pending
            );
            assert_eq!(run.cleanup().last_error(), None);
            let pid = run.cleanup().child_pid();
            let pidfd = run.cleanup().pidfd.as_ref().unwrap().as_raw_fd();
            eprintln!(
                "original read errno=EAGAIN prefix_bytes={} reader_fd={reader_fd} pidfd={pidfd} automatic_cleanup=false",
                bytes.len()
            );
            match run.cancel_until(deadline) {
                OwnedFinalize::Failed { cause, cleanup } => {
                    assert_eq!(cause, OwnedRunFailure::ResultRead(Errno::EAGAIN));
                    assert_eq!(cleanup.provisional_bytes(), bytes);
                    assert_eq!(cleanup.reader.as_ref().unwrap().as_raw_fd(), reader_fd);
                    assert_eq!(cleanup.cleanup().pidfd.as_ref().unwrap().as_raw_fd(), pidfd);
                    assert_eq!(
                        cleanup.cleanup().observation(),
                        ChildCleanupObservation::Reaped(ExitStatus::Signaled(
                            Signal::SIGKILL,
                            false
                        ))
                    );
                    assert!(!cleanup.result_eof());
                }
                other => panic!("read refusal became {}", outcome_kind(&other)),
            }
            assert_reaped(pid);
            unsafe { unmap_shared_drop_state(mapping, shared) };
        });
    }

    #[test]
    fn owned_deferred_setup_refusal_decodes_only_after_actual_wait() {
        isolated(|deadline| {
            let (mapping, shared) = new_shared_drop_state();
            let dir = tempfile::tempdir().unwrap();
            let absent = dir.path().join("absent");
            let run = Container::new()
                .current_dir(absent)
                .run_with_deferred_drop_owned(&mut || {
                    unsafe { &*shared }.started.store(true, Ordering::Release);
                    (42, ())
                })
                .unwrap();
            let complete = match run.finalize_until(deadline) {
                OwnedFinalize::Complete(complete) => complete,
                other => panic!("setup-refusal publisher was {}", outcome_kind(&other)),
            };
            assert_eq!(complete.status(), ExitStatus::Exited(0));
            let refusal = complete.decode().unwrap_err();
            assert_eq!(
                refusal.cause(),
                OwnedRunFailure::Startup(StartupError::Setup(Error::new(
                    Errno::ENOENT,
                    Context::Chdir
                )))
            );
            assert!(!unsafe { &*shared }.started.load(Ordering::Acquire));
            unsafe { unmap_shared_drop_state(mapping, shared) };
        });
    }

    #[test]
    fn owned_deferred_preclone_refusals_have_no_workload_effects() {
        isolated(|_deadline| {
            let (mapping, shared) = new_shared_drop_state();
            {
                let fault = super::super::super::clone::OwnedCloneTestFault::Probe(Errno::ENOSYS);
                let _fault = OwnedCloneFaultGuard::install(fault);
                assert!(matches!(
                    Container::new().run_with_deferred_drop_owned(&mut || {
                        unsafe { &*shared }.started.store(true, Ordering::Release);
                        ((), ())
                    }),
                    Err(StartupOwnedFailure::BeforeClone {
                        cause: StartupError::Io(Errno::ENOSYS)
                    })
                ));
            }
            for flags in [0, libc::SA_NOCLDWAIT] {
                let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
                action.sa_sigaction = if flags == 0 {
                    libc::SIG_IGN
                } else {
                    libc::SIG_DFL
                };
                action.sa_flags = flags;
                assert_eq!(
                    unsafe { libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) },
                    0
                );
                assert!(matches!(
                    Container::new().run_with_deferred_drop_owned(&mut || {
                        unsafe { &*shared }.started.store(true, Ordering::Release);
                        ((), ())
                    }),
                    Err(StartupOwnedFailure::BeforeClone {
                        cause: StartupError::Io(Errno::ECHILD)
                    })
                ));
            }
            assert!(!unsafe { &*shared }.started.load(Ordering::Acquire));
            unsafe { unmap_shared_drop_state(mapping, shared) };
        });
    }

    #[test]
    fn owned_deferred_wait_refusal_preserves_owner_bytes_and_parent_guard() {
        isolated(|deadline| {
            let (mapping, shared) = new_shared_drop_state();
            let dropped = std::cell::Cell::new(false);
            let guard = Guard(&dropped);
            let mut run = Container::new()
                .run_with_deferred_drop_owned(&mut || (57, BlockingDrop { shared }))
                .unwrap();
            let bytes = run.provisional_bytes().to_vec();
            let pid = run.cleanup().child_pid();
            let fd = run.cleanup().pidfd.as_ref().unwrap().as_raw_fd();
            run.inner.child.as_mut().unwrap().wait_error_once = Some(Errno::EIO);
            let failed = match run.finalize_until(deadline) {
                OwnedFinalize::Failed { cause, cleanup } => {
                    assert_eq!(cause, OwnedRunFailure::Cleanup(Errno::EIO));
                    assert_eq!(
                        cleanup.cleanup().observation(),
                        ChildCleanupObservation::Unknown
                    );
                    cleanup
                }
                other => panic!("wait-refusal seam became {}", outcome_kind(&other)),
            };
            assert!(!dropped.get());
            assert_eq!(failed.provisional_bytes(), bytes);
            assert_eq!(failed.cleanup().pidfd.as_ref().unwrap().as_raw_fd(), fd);
            match failed.cancel_until(deadline) {
                OwnedFinalize::Failed { cause, cleanup } => {
                    assert_eq!(cause, OwnedRunFailure::Cleanup(Errno::EIO));
                    assert_eq!(
                        cleanup.cleanup().observation(),
                        ChildCleanupObservation::Reaped(ExitStatus::Signaled(
                            Signal::SIGKILL,
                            false
                        ))
                    );
                    assert_eq!(cleanup.provisional_bytes(), bytes);
                    assert!(!dropped.get());
                }
                other => panic!("wait refusal became {}", outcome_kind(&other)),
            }
            assert_reaped(pid);
            drop(guard);
            assert!(dropped.get());
            unsafe { unmap_shared_drop_state(mapping, shared) };
        });
    }

    #[test]
    fn owned_deferred_opposing_reaper_never_makes_complete() {
        isolated(|deadline| {
            let run = Container::new()
                .run_with_deferred_drop_owned(&mut || (89, ()))
                .unwrap();
            let pid = run.cleanup().child_pid();
            let bytes = run.provisional_bytes().to_vec();
            let mut status = 0;
            assert_eq!(
                unsafe { libc::waitpid(pid.as_raw(), &mut status, 0) },
                pid.as_raw()
            );
            assert_eq!(ExitStatus::from_raw(status), ExitStatus::Exited(0));
            match run.finalize_until(deadline) {
                OwnedFinalize::Failed { cause, cleanup } => {
                    assert_eq!(cause, OwnedRunFailure::WaitStatusUnavailable);
                    assert_eq!(
                        cleanup.cleanup().observation(),
                        ChildCleanupObservation::ExitedWithoutWaitStatus
                    );
                    assert_eq!(cleanup.provisional_bytes(), bytes);
                }
                other => panic!("opposing owner became {}", outcome_kind(&other)),
            }
        });
    }
}
