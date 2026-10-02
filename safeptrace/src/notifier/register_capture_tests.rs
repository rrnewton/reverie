/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * Licensed under the BSD-style license found in the LICENSE file. */

// These tests use child_stop's actual TRACEME child and committed wait, the
// production two-register reader, and the real notifier's consuming wait.
// They prove no foreign-waiter, whole-backend, source-byte or ptracer-domain claim.
mod register_capture_tests {
    use super::*;
    use crate::FollowedSourceReadPlan;
    use crate::NativeSourceReadPlan;
    use reverie_memory::NativeUserReadError as ReadError;
    use reverie_memory::NativeUserReadRefusal as ReadRefusal;

    struct HookReset;
    impl Drop for HookReset {
        fn drop(&mut self) {
            NativeSourceReadPlan::set_capture_hook_for_test(|_| {});
        }
    }

    fn signal_original(fd: &OwnedFd) -> Result<(), Errno> {
        Errno::result(unsafe {
            libc::syscall(libc::SYS_pidfd_send_signal, fd.as_raw_fd(), libc::SIGKILL,
                          std::ptr::null::<libc::siginfo_t>(), 0)
        }).map(|_| ())
    }

    // Always run before a protection assertion; exact original owner, no PID reopen.
    fn finish_original(cleanup: &ExactChildGuard, deadline: Instant) -> Result<(), Errno> {
        // Also close failed setup/controller paths; a missing earlier signal
        // must not turn the cleanup oracle into a wait on a live stopped child.
        match signal_original(&cleanup.0.duplicate_bound_thread_pidfd()?) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => return Err(error),
        }
        if !cleanup.0.wait(deadline.saturating_duration_since(Instant::now())) {
            return Err(Errno::ETIMEDOUT);
        }
        cleanup.0.observed_exit_status()?.ok_or(Errno::ENODATA)?;
        futures::executor::block_on(cleanup.0.reap_parent_terminal())?;
        if !cleanup.0.is_reaped()? {
            return Err(Errno::EBUSY);
        }
        Ok(())
    }

    fn defers_actual_consumption(point: u8, panic_after_release: bool, cancel: bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let (cleanup, stopped) = child_stop();
        let source = stopped.source_stop().unwrap();
        let acquisition = source.begin_acquisition().unwrap();
        let original = cleanup.0.duplicate_bound_thread_pidfd().unwrap();
        let event = Arc::clone(stopped.1.event().event());
        let (observed, observation) = mpsc::channel();
        *event.capture_consume_observer.lock() = Some(observed);
        let (entered, entry) = mpsc::sync_channel(1);
        let (release, released) = mpsc::channel();
        NativeSourceReadPlan::capture_steps_for_test();
        NativeSourceReadPlan::set_capture_hook_for_test(move |step| {
            if step == point {
                entered.try_send(()).expect("one actual capture boundary");
                // Sender drop releases both fixed and failed paths. The single
                // original test deadline bounds a missing controller response.
                match released.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
                    Err(error) => panic!("capture controller did not release: {error}"),
                }
                if panic_after_release {
                    panic!("injected panic at actual register boundary");
                }
            }
        });
        let reset = HookReset;
        let cancelling = stopped.terminal_cleanup();
        let controller = thread::spawn(move || {
            let entered = entry.recv_timeout(deadline.saturating_duration_since(Instant::now()));
            let killed = if entered.is_ok() {
                if cancel { cancelling.request_sigkill() } else { signal_original(&original) }
            } else {
                Err(Errno::ETIMEDOUT)
            };
            // Waiting is emitted only after real original-pidfd WNOWAIT;
            // Consumed is emitted only after actual nonblocking consumption.
            let observation = if killed.is_ok() {
                observation.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            } else {
                Err(mpsc::RecvTimeoutError::Disconnected)
            };
            drop(release);
            (entered, killed, observation)
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            NativeSourceReadPlan::prepare(acquisition, 4096, 1)
        }));
        drop(reset);
        let steps = NativeSourceReadPlan::capture_steps_for_test();
        // The controller is bounded and its sender is dropped on every return.
        let controller = controller.join();
        let finished = finish_original(&cleanup, deadline);
        *event.capture_consume_observer.lock() = None;
        // Release any returned plan/acquisition before fallback cleanup or oracle.
        let normal_error = matches!(result,
            Ok(Err(ReadError::Refused(ReadRefusal::TargetState(Errno::ESRCH)))));
        let was_panic = result.is_err();
        drop(result);
        finished.expect("original child/notifier were not completely reaped");
        let (entered, killed, observation) = controller.expect("controller must join");
        entered.expect("real register boundary was not entered");
        killed.expect("original-pidfd SIGKILL failed");
        let observation = observation.expect("no actual waiter transition");
        if let CaptureConsumeObservation::Consumed(raw) = observation {
            assert!(libc::WIFSIGNALED(raw));
            assert_eq!(libc::WTERMSIG(raw), libc::SIGKILL);
        }
        assert!(
            matches!(observation, CaptureConsumeObservation::Waiting { mutation: false }),
            "terminal status was consumed during the actual register capture"
        );
        assert_eq!(steps, if point == 0 { vec![0] } else { vec![0, 1] });
        assert_eq!(was_panic, panic_after_release);
        if !panic_after_release {
            assert!(normal_error, "actual killed register read must refuse ESRCH");
        }
        assert!(Instant::now() < deadline, "original three-second component deadline");
    }

    #[test]
    fn actual_capture_defers_terminal_before_prstatus() {
        defers_actual_consumption(0, false, false);
    }

    #[test]
    fn actual_capture_defers_terminal_between_register_reads() {
        defers_actual_consumption(1, false, false);
    }

    #[test]
    fn actual_capture_panic_releases_waiting_consumer() {
        defers_actual_consumption(1, true, false);
    }

    #[test]
    fn actual_capture_cancellation_releases_waiting_consumer() {
        defers_actual_consumption(1, false, true);
    }

    #[test]
    fn actual_capture_pair_finishes_before_binding_lifetime() {
        let deadline = Instant::now() + Duration::from_secs(3);
        let (cleanup, stopped) = child_stop();
        let source = stopped.source_stop().unwrap();
        NativeSourceReadPlan::capture_steps_for_test();
        let plan = NativeSourceReadPlan::prepare(source.begin_acquisition().unwrap(), 4096, 1);
        let steps = NativeSourceReadPlan::capture_steps_for_test();
        let killed = signal_original(&cleanup.0.duplicate_bound_thread_pidfd().unwrap());
        // Keep the actual plan (and acquiring) alive during terminal wait.
        let finished = finish_original(&cleanup, deadline);
        let prepared = plan.is_ok();
        drop(plan);
        finished.expect("capture ticket extended through the later binding lifetime");
        killed.unwrap();
        assert!(prepared, "actual PRSTATUS and XSTATE must both succeed");
        assert_eq!(steps, vec![0, 1]);
        assert_eq!(source.validate_current(), Err(Errno::ESTALE));
        assert!(Instant::now() < deadline);
    }

    #[test]
    fn actual_followed_capture_pair_finishes_before_hold_lifetime() {
        let deadline = Instant::now() + Duration::from_secs(3);
        let (cleanup, stopped) = child_stop();
        let control = stopped.control_stop().unwrap();
        let hold = Arc::new(control.hold().unwrap());
        NativeSourceReadPlan::capture_steps_for_test();
        let plan = FollowedSourceReadPlan::prepare(Arc::clone(&hold), 4096, 1);
        let steps = NativeSourceReadPlan::capture_steps_for_test();
        let killed = signal_original(&cleanup.0.duplicate_bound_thread_pidfd().unwrap());
        let finished = finish_original(&cleanup, deadline);
        let prepared = plan.is_ok();
        drop(plan);
        drop(hold);
        finished.expect("capture ticket extended through the later hold lifetime");
        killed.unwrap();
        assert!(prepared, "actual followed PRSTATUS and XSTATE must both succeed");
        assert_eq!(steps, vec![0, 1]);
        assert!(Instant::now() < deadline);
    }

    #[test]
    fn actual_nested_capture_cannot_steal_original_ticket() {
        let deadline = Instant::now() + Duration::from_secs(3);
        let (cleanup, stopped) = child_stop();
        let control = stopped.control_stop().unwrap();
        let hold = Arc::new(control.hold().unwrap());
        let nested_hold = Arc::clone(&hold);
        let nested = Arc::new(Mutex::new(None));
        let outcome = Arc::clone(&nested);
        NativeSourceReadPlan::capture_steps_for_test();
        NativeSourceReadPlan::set_capture_hook_for_test(move |step| {
            if step == 1 {
                let result = FollowedSourceReadPlan::prepare(Arc::clone(&nested_hold), 4096, 1);
                *outcome.lock() = Some(matches!(result,
                    Err(ReadError::Refused(ReadRefusal::TargetState(Errno::EBUSY)))));
            }
        });
        let reset = HookReset;
        let outer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            FollowedSourceReadPlan::prepare(Arc::clone(&hold), 4096, 1)
        }));
        drop(reset);
        let steps = NativeSourceReadPlan::capture_steps_for_test();
        let prepared = matches!(outer, Ok(Ok(_)));
        drop(outer);
        drop(hold);
        let killed = signal_original(&cleanup.0.duplicate_bound_thread_pidfd().unwrap());
        let finished = finish_original(&cleanup, deadline);
        finished.unwrap();
        killed.unwrap();
        assert_eq!(*nested.lock(), Some(true), "nested capture must refuse before either read");
        assert!(prepared, "nested refusal must not release or poison the outer ticket");
        assert_eq!(steps, vec![0, 1]);
        assert!(Instant::now() < deadline);
    }

    #[test]
    fn actual_capture_range_error_releases_ticket() {
        let deadline = Instant::now() + Duration::from_secs(3);
        let (cleanup, stopped) = child_stop();
        let source = stopped.source_stop().unwrap();
        NativeSourceReadPlan::capture_steps_for_test();
        let result = NativeSourceReadPlan::prepare(source.begin_acquisition().unwrap(), 4096, 0);
        let steps = NativeSourceReadPlan::capture_steps_for_test();
        let killed = signal_original(&cleanup.0.duplicate_bound_thread_pidfd().unwrap());
        let finished = finish_original(&cleanup, deadline);
        finished.unwrap();
        killed.unwrap();
        assert!(matches!(result, Err(ReadError::Refused(ReadRefusal::UnsupportedRange))));
        assert!(steps.is_empty());
        assert!(Instant::now() < deadline);
    }
}
