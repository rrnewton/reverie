/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/// Cleanup owners meeting an Exec status that the tracee left through a
/// fatal signal: <https://github.com/rrnewton/reverie/issues/686>.
mod fatal_dead_exec_tests {
    use super::*;

    const EXEC_STOP: i32 = (libc::PTRACE_EVENT_EXEC << 16) | (libc::SIGTRAP << 8) | 0x7f;

    /// Forks a single-threaded tracee that execs `/bin/sleep`, resumes it
    /// into its exec stop and, once the notifier has queued that stop,
    /// SIGKILLs it. The kill takes the tracee out of the exec stop into its
    /// exit stop, so the queued Exec names a stop it has left. Returns once
    /// the exit stop is published.
    fn killed_exec_tracee(deadline: Instant) -> (Pid, Running) {
        let pid = match unsafe { unistd::fork() }.expect("fork exec tracee") {
            ForkResult::Child => {
                safeptrace::traceme_and_stop().expect("TRACEME exec tracee");
                let args = [c"/bin/sleep".as_ptr(), c"30".as_ptr(), std::ptr::null()];
                unsafe {
                    libc::execv(args[0], args.as_ptr());
                    libc::_exit(127)
                };
            }
            ForkResult::Parent { child } => Pid::from(child),
        };
        let (stopped, event) = Running::new(pid)
            .wait()
            .expect("wait exec tracee")
            .assume_stopped();
        assert_eq!(event, Event::Signal(Signal::SIGSTOP));
        stopped
            .setoptions(
                ptrace::Options::PTRACE_O_TRACEEXEC
                    | ptrace::Options::PTRACE_O_TRACEEXIT
                    | ptrace::Options::PTRACE_O_EXITKILL,
            )
            .expect("set exec tracee options");
        let terminal = stopped.terminal_cleanup();
        let running = stopped.resume(None).expect("resume exec tracee");
        while terminal.pending_is_empty() {
            assert!(Instant::now() < deadline, "no exec stop queued");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(terminal.queued_raw_statuses(), [EXEC_STOP]);
        assert_eq!(unsafe { libc::kill(pid.as_raw(), libc::SIGKILL) }, 0);
        while !terminal.exit_stop_observed() {
            assert!(Instant::now() < deadline, "no exit stop observed");
            std::thread::sleep(Duration::from_millis(1));
        }
        (pid, running)
    }

    /// Claims the killed tracee's exit stop and requires its actual final
    /// SIGKILL status.
    async fn reap_killed_exec(pid: Pid, running: Running, deadline: Instant) {
        let exit_stop = tokio::time::timeout_at(deadline.into(), running.exit_event())
            .await
            .expect("exit stop claim is bounded")
            .expect("claim the exit stop");
        let exited = tokio::time::timeout_at(
            deadline.into(),
            exit_stop.resume(None).expect("resume exit stop").next_state(),
        )
        .await
        .expect("final status is bounded")
        .expect("wait final status");
        assert_eq!(
            exited.assume_exited(),
            (pid, ExitStatus::Signaled(Signal::SIGKILL, false))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fatal_freeze_consumes_a_killed_exec_without_holding_it() {
        let deadline = Instant::now() + Duration::from_secs(3);
        let (pid, running) = killed_exec_tracee(deadline);
        let stop = FatalTaskStop {
            tid: pid,
            terminal: running.terminal_cleanup(),
            held: Arc::new(StdMutex::new(None)),
            frozen: AtomicBool::new(false),
        };
        let frozen = tokio::time::timeout_at(
            deadline.into(),
            stop.freeze(deadline, |parent, op, child| {
                panic!(
                    "a killed exec reported child {} of {parent} by {op:?}",
                    child.pid()
                )
            }),
        )
        .await
        .expect("freeze of a killed exec is bounded");
        assert!(frozen.is_ok(), "freeze of a killed exec failed: {frozen:?}");
        assert!(
            stop.held.lock().unwrap().is_none(),
            "a dead Exec was recorded as the held stop"
        );
        assert!(stop.terminal.pending_is_empty(), "the dead Exec stayed queued");
        reap_killed_exec(pid, running, deadline).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fatal_newborn_reap_consumes_a_killed_exec() {
        let deadline = Instant::now() + Duration::from_secs(3);
        let (pid, running) = killed_exec_tracee(deadline);
        let session = FatalSession::for_test(pid);
        let newborn = FatalNewborn::new(pid, &running);
        let terminal = running.terminal_cleanup();
        drop(running);
        tokio::time::timeout_at(deadline.into(), newborn.reap_owned(&session))
            .await
            .expect("reap of a killed exec is bounded");
        assert!(!session.cleanup_was_refused());
        assert!(!session.ordinary_receipt().failure_published);
        assert!(terminal.pending_is_empty(), "the dead Exec stayed queued");
        assert_eq!(
            terminal.observed_exit_status(),
            Ok(Some(ExitStatus::Signaled(Signal::SIGKILL, false)))
        );
        assert!(terminal.wait(deadline.saturating_duration_since(Instant::now())));
    }
}
