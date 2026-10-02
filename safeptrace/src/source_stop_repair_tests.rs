/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

// Included beneath notifier::source::tests to reuse its actual TRACEME child
// and exact cleanup owner. These are control/notification components, not a
// source-backend, MM-permission, or record/replay qualification.
mod repair_tests {
    use super::*;

    #[cfg(target_arch = "x86_64")]
    #[derive(Clone, Copy, Debug)]
    enum RawRoute {
        Status,
        WaitStatus,
    }

    #[cfg(target_arch = "x86_64")]
    impl RawRoute {
        fn stopped(self, pid: Pid) -> Stopped {
            let wait = match self {
                Self::Status => Wait::from_raw(pid, (libc::SIGSTOP << 8) | 0x7f),
                Self::WaitStatus => {
                    Wait::try_from(nix::sys::wait::WaitStatus::Stopped(pid.into(), Signal::SIGSTOP))
                }
            };
            let (stopped, event) = wait.expect("decode actual held stop").assume_stopped();
            assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));
            assert!(
                stopped.source_stop().is_err(),
                "raw conversion must not consume the original stop"
            );
            stopped
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn registers(regs: &crate::Regs) -> [u64; 27] {
        [
            regs.r15, regs.r14, regs.r13, regs.r12, regs.rbp, regs.rbx, regs.r11, regs.r10,
            regs.r9, regs.r8, regs.rax, regs.rcx, regs.rdx, regs.rsi, regs.rdi, regs.orig_rax,
            regs.rip, regs.cs, regs.eflags, regs.rsp, regs.ss, regs.fs_base, regs.gs_base,
            regs.ds, regs.es, regs.fs, regs.gs,
        ]
    }

    #[cfg(target_arch = "x86_64")]
    fn busy<T: std::fmt::Debug>(name: &str, result: Result<T, crate::Error>) {
        assert!(
            matches!(result, Err(crate::Error::Errno(Errno::EBUSY))),
            "{name} did not refuse the actual acquisition: {result:?}"
        );
    }

    #[cfg(target_arch = "x86_64")]
    fn actual_raw_control_exclusion(route: RawRoute) {
        let (_cleanup, stopped) = child_stop();
        let pid = stopped.pid();
        let terminal = stopped.terminal_cleanup();
        // SourceStop and acquisition come only from the actual committed wait.
        let source = stopped.source_stop().unwrap();
        let acquisition = source.begin_acquisition().unwrap();
        let regs = stopped.getregs().unwrap();
        let fpregs = stopped.getfpregs().unwrap();
        let xstate = stopped.getxstate().unwrap();
        let siginfo = stopped.getsiginfo().unwrap();
        let alias = route.stopped(pid);

        // The same-value write is deliberate: success would be a real ptrace
        // effect even if a later register comparison could not detect it.
        busy("PRSTATUS", alias.setregs(&regs));
        busy("FPREG", alias.setfpregs(&fpregs));
        busy("XSTATE", alias.setxstate(&xstate));
        busy("options", alias.setoptions(crate::Options::empty()));
        busy("siginfo", alias.setsiginfo(&siginfo));
        busy("resume", route.stopped(pid).resume(None));
        let retained = match route.stopped(pid).resume_retaining(None) {
            Err((retained, Errno::EBUSY)) => retained,
            other => panic!("retaining resume bypassed acquisition: {other:?}"),
        };
        busy("step", route.stopped(pid).step(None));
        busy("syscall", route.stopped(pid).syscall(None));
        busy("detach", route.stopped(pid).detach(None));
        assert_eq!(Running::new(pid).interrupt(), Err(Errno::EBUSY));
        assert!(matches!(Running::attach(pid), Err(Errno::EBUSY)));
        assert!(matches!(
            Running::seize(pid, crate::Options::empty()),
            Err(Errno::EBUSY)
        ));
        assert_eq!(terminal.continue_for_cleanup(), Err(Errno::EBUSY));

        // These read the actual original kernel stop on the original ptracer
        // thread. No modeled child state or synthetic SourceStamp is installed.
        assert_eq!(registers(&stopped.getregs().unwrap()), registers(&regs));
        assert_eq!(stopped.getxstate().unwrap(), xstate);
        let after = stopped.getsiginfo().unwrap();
        assert_eq!(after.si_signo, siginfo.si_signo);
        assert_eq!(after.si_errno, siginfo.si_errno);
        assert_eq!(after.si_code, siginfo.si_code);
        unsafe {
            assert_eq!(after.si_pid(), siginfo.si_pid());
            assert_eq!(after.si_uid(), siginfo.si_uid());
        }
        source.validate_current().unwrap();
        acquisition
            .with_stopped(|original| {
                assert_eq!(registers(&original.getregs().unwrap()), registers(&regs));
            })
            .unwrap();

        acquisition.finish_binding();
        source.validate_current().unwrap();
        // Generic raw control is still allowed after the conflicting interval.
        retained.setregs(&regs).unwrap();
        assert_eq!(registers(&stopped.getregs().unwrap()), registers(&regs));
        assert_eq!(source.validate_current(), Err(Errno::ESTALE));
        assert!(source.begin_acquisition().is_err());
        assert!(retained.source_stop().is_err());
        let final_wait = stopped.resume_retaining(None).unwrap().wait().unwrap();
        assert_eq!(final_wait.assume_exited(), (pid, crate::ExitStatus::Exited(0)));
        assert!(terminal.wait(Duration::from_secs(2)));
        assert!(terminal.is_reaped().unwrap());
        // Registry removal cannot make this retained alias numeric again.
        assert!(matches!(
            alias.setregs(&regs),
            Err(crate::Error::Died(zombie)) if zombie.pid() == pid
        ));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn actual_from_raw_alias_refuses_all_control_during_source_acquisition() {
        actual_raw_control_exclusion(RawRoute::Status);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn actual_try_from_alias_refuses_all_control_during_source_acquisition() {
        actual_raw_control_exclusion(RawRoute::WaitStatus);
    }

    struct WorkerWakeProbe {
        event: Arc<Event>,
        deadline: Instant,
        publication: mpsc::Sender<()>,
        waiter_completion: Mutex<mpsc::Receiver<bool>>,
        waiter_completed: AtomicBool,
        lock_was_free: AtomicBool,
        calls: AtomicUsize,
    }

    impl std::task::Wake for WorkerWakeProbe {
        fn wake(self: Arc<Self>) {
            std::task::Wake::wake_by_ref(&self);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            // Publication has happened even in the unlocked-publisher mutant.
            // Let the owner release its paused waiter before awaiting return.
            let _ = self.publication.send(());
            let completed = matches!(
                self.waiter_completion
                    .lock()
                    .recv_timeout(self.deadline.saturating_duration_since(Instant::now())),
                Ok(true)
            );
            self.waiter_completed.store(completed, Ordering::Release);
            if completed {
                // The receipt is sent only after wait_worker_done returns and
                // releases its guard. Its reacquisition cannot race this probe.
                self.lock_was_free.store(
                    self.event.worker_done_lock.try_lock().is_some(),
                    Ordering::Release,
                );
            }
            self.calls.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn actual_worker_done_publication_serializes_with_check_to_wait() {
        let deadline = Instant::now() + Duration::from_secs(3);
        let event = Arc::new(Event::new());
        assert!(event.try_begin_worker_start());
        event.mark_worker_running();
        let (publication, publication_wait) = mpsc::channel();
        let (waiter_returned, waiter_completion) = mpsc::channel();
        let probe = Arc::new(WorkerWakeProbe {
            event: Arc::clone(&event),
            deadline,
            publication: publication.clone(),
            waiter_completion: Mutex::new(waiter_completion),
            waiter_completed: AtomicBool::new(false),
            lock_was_free: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        });
        let waiter_registration = Arc::new(ExitWaiter {
            waker: WakerSlot::default(),
            epoch: event.exit_epoch.load(Ordering::Acquire),
        });
        event
            .worker_done_waiters
            .register(&waiter_registration, &Waker::from(Arc::clone(&probe)));
        let (checked, checked_wait) = mpsc::sync_channel(1);
        let (release, release_wait) = mpsc::channel();
        *event.worker_done_wait_pause.lock() = Some(BoundedTestPause {
            captured: checked,
            resume: release_wait,
        });
        // The first receipt comes from either an actual contended lock attempt
        // (fixed publisher), or the waker after publication (unlocked publisher).
        // Both paths are events; no sleep or timed absence establishes order.
        *event.worker_done_lock_contended.lock() = Some(publication.clone());
        let waiting_event = Arc::clone(&event);
        let waiting = thread::spawn(move || {
            let waited =
                waiting_event.wait_worker_done(deadline.saturating_duration_since(Instant::now()));
            let _ = waiter_returned.send(waited);
            waited
        });
        let checked_result =
            checked_wait.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let publishing_event = Arc::clone(&event);
        let publishing = thread::spawn(move || {
            publishing_event.mark_worker_done();
            let _ = publication.send(());
        });
        let publication_result =
            publication_wait.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let state_before_release = event.worker_state.load(Ordering::Acquire);
        // Release on all result paths BEFORE either join or any assertion.
        // A failed/mutant run must not strand a publisher behind the waiter.
        drop(release);
        let waited = waiting.join();
        let published = publishing.join();
        checked_result.expect("waiter did not reach its actual predicate-to-wait gap");
        publication_result.expect("publisher neither attempted its lock nor completed");
        assert_eq!(
            state_before_release, WORKER_RUNNING,
            "WORKER_DONE was published while the waiter held worker_done_lock"
        );
        assert!(waited.unwrap(), "actual condvar waiter did not observe retirement");
        published.unwrap();
        assert_eq!(event.worker_state.load(Ordering::Acquire), WORKER_DONE);
        assert_eq!(probe.calls.load(Ordering::Acquire), 1);
        assert!(
            probe.waiter_completed.load(Ordering::Acquire),
            "async waker did not receive the actual waiter completion before the original deadline"
        );
        assert!(
            probe.lock_was_free.load(Ordering::Acquire),
            "async waker ran with worker_done_lock held"
        );
        assert!(
            Instant::now() < deadline,
            "worker retirement exhausted the original three-second component bound"
        );
    }
}
