/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/// Force only the PIDFD_THREAD open to fail as it does before Linux 6.9.
/// Other ptracer test threads remain on their normal kernel path.
struct LegacyThreadGroup {
    pid: Pid,
    _ptracer: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl LegacyThreadGroup {
    fn new(pid: Pid) -> Self {
        FORCE_LEGACY_THREAD_GROUP.with(|group| {
            assert!(group.get().is_none());
            group.set(Some(pid.into()));
        });
        Self {
            pid,
            _ptracer: std::marker::PhantomData,
        }
    }
}

impl Drop for LegacyThreadGroup {
    fn drop(&mut self) {
        FORCE_LEGACY_THREAD_GROUP.with(|group| {
            if group.get() == Some(self.pid.into()) {
                group.set(None);
            }
        });
    }
}

fn run_legacy_test_outer(name: &str) -> bool {
    const INNER: &str = "SAFEPTRACE_LEGACY_THREAD_INNER";
    if env::var(INNER).as_deref() == Ok(name) {
        return false;
    }
    let result = run_exact_test_bounded(
        &format!("notifier::test::{name}"),
        &[(INNER, name)],
        false,
        Duration::from_secs(5),
    )
    .expect("start bounded legacy-thread test");
    assert!(
        !result.timed_out,
        "legacy-thread test timed out: {result:?}"
    );
    assert!(
        result.output.status.success(),
        "legacy-thread test failed: {result:?}"
    );
    true
}

#[test]
fn forced_legacy_leader_preserves_pidfd_waits_and_cancellation() {
    let (pid, mut cleanup) = spawn_stopped_process(None).unwrap();
    let _force = LegacyThreadGroup::new(pid);
    let running = Running::new(pid.into());
    cleanup.bind_running_notifier(&running).unwrap();
    let terminal = running.terminal_cleanup();
    assert!(matches!(
        terminal.event.identity().unwrap().pidfd,
        ThreadHandle::LegacyLeader { .. }
    ));
    terminal.request_sigkill().unwrap();
    assert!(terminal.wait(TRACEE_WAIT_TIMEOUT));
    assert_eq!(terminal.request_sigkill(), Err(Errno::ESRCH));
    cleanup.disarm();
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[cfg(not(sanitized))]
fn spawn_legacy_thread_tracee(
    pause_thread: bool,
) -> (Pid, Stopped, TraceeCleanupGuard, LegacyThreadGroup) {
    let root = match unsafe { fork() }.unwrap() {
        ForkResult::Parent { child } => child,
        ForkResult::Child => {
            crate::traceme_and_stop().unwrap();
            // Raw pthread creation avoids std's locks inherited across fork
            // from the multithreaded Rust test harness.
            extern "C" fn guest_thread(argument: *mut libc::c_void) -> *mut libc::c_void {
                let mode = argument as usize;
                if mode == 0 {
                    loop {
                        unsafe { libc::pause() };
                    }
                }
                if mode == 17 {
                    unsafe { libc::raise(libc::SIGTRAP) };
                }
                unsafe { libc::syscall(libc::SYS_exit, mode as i32) };
                std::ptr::null_mut()
            }
            let exits: &[usize] = if pause_thread { &[0] } else { &[17, 23] };
            for exit in exits {
                let mut thread = mem::MaybeUninit::<libc::pthread_t>::uninit();
                let created = unsafe {
                    libc::pthread_create(
                        thread.as_mut_ptr(),
                        std::ptr::null(),
                        guest_thread,
                        *exit as *mut libc::c_void,
                    )
                };
                if created != 0 {
                    unsafe { libc::_exit(126) };
                }
                let joined =
                    unsafe { libc::pthread_join(thread.assume_init(), std::ptr::null_mut()) };
                if joined != 0 {
                    unsafe { libc::_exit(127) };
                }
            }
            unsafe { libc::_exit(42) };
        }
    };
    let mut cleanup = TraceeCleanupGuard::new(root).unwrap();
    let force = LegacyThreadGroup::new(root);
    let status = waitpid_status_bounded(root, libc::WUNTRACED, TRACEE_WAIT_TIMEOUT).unwrap();
    assert!(libc::WIFSTOPPED(status));
    assert_eq!(libc::WSTOPSIG(status), libc::SIGSTOP);
    let stopped = Stopped::new_unchecked(root.into());
    stopped
        .setoptions(
            Options::PTRACE_O_TRACECLONE | Options::PTRACE_O_TRACEEXIT | Options::PTRACE_O_EXITKILL,
        )
        .unwrap();
    cleanup.bind_notifier(&stopped).unwrap();
    (root, stopped, cleanup, force)
}

fn legacy_child_cleanup(child: &Running) -> TraceeCleanupGuard {
    let identity = child.terminal_cleanup().event.identity().unwrap().clone();
    assert_ne!(
        child.pid(),
        identity.snapshot.tgid,
        "fixture must create a non-leader"
    );
    assert!(matches!(identity.pidfd, ThreadHandle::Procfs { .. }));
    let mut cleanup = TraceeCleanupGuard {
        pid: child.pid().into(),
        pidfd: identity.pidfd.try_clone().unwrap(),
        ownership: TraceeCleanupOwnership::PreRegistration,
        armed: true,
    };
    cleanup.bind_running_notifier(child).unwrap();
    cleanup
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(sanitized))]
async fn forced_legacy_nonleader_preserves_stops_exits_and_retirement() {
    if run_legacy_test_outer("forced_legacy_nonleader_preserves_stops_exits_and_retirement") {
        return;
    }
    let (root, stopped, mut root_cleanup, _force) = spawn_legacy_thread_tracee(false);
    let root_terminal = stopped.terminal_cleanup();
    let root_exit = root_cleanup.exit_event(&stopped).unwrap();
    let (parent, crate::Event::NewChild(crate::ChildOp::Clone, child)) = tokio::time::timeout(
        TRACEE_WAIT_TIMEOUT,
        stopped.resume(None).unwrap().wait_owned(),
    )
    .await
    .unwrap()
    .unwrap()
    .assume_stopped() else {
        panic!("missing first pthread clone");
    };
    let first_tid = child.pid();
    let mut first_cleanup = legacy_child_cleanup(&child);
    let first_terminal = child.terminal_cleanup();
    let identity = first_terminal.event.identity().unwrap().clone();
    let (child, event) = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, child.wait_owned())
        .await
        .unwrap()
        .unwrap()
        .assume_stopped();
    assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));
    let (child, event) = tokio::time::timeout(
        TRACEE_WAIT_TIMEOUT,
        child.resume(None).unwrap().wait_owned(),
    )
    .await
    .unwrap()
    .unwrap()
    .assume_stopped();
    assert_eq!(event, crate::Event::Signal(Signal::SIGTRAP));
    let mut stale_stop = Stopped::new_unchecked(first_tid);
    let exit = first_cleanup.exit_event(&child).unwrap();
    let running = child.resume(None).unwrap();
    // ExitFuture observes the worker notification without polling wait_owned.
    let exiting = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, exit)
        .await
        .unwrap()
        .unwrap();
    first_cleanup.mark_claimed_exit();
    drop(running);
    assert_eq!(exiting.pid(), first_tid);
    assert_eq!(exiting.getevent().unwrap(), 17 << 8);

    // The final status must remain unreaped while an earlier numeric request
    // owns the TID gate. A stop-only consuming wait must not release it.
    let event = Arc::clone(first_terminal.event.event());
    let held = event.hold_tid().unwrap();
    let final_running = exiting.resume_retaining(None).unwrap();
    let terminal_flags = WaitPidFlag::from_bits_retain(
        WaitPidFlag::WEXITED.bits()
            | WaitPidFlag::WNOWAIT.bits()
            | WaitPidFlag::WNOHANG.bits()
            | libc::__WALL,
    );
    let deadline = Instant::now() + TRACEE_WAIT_TIMEOUT;
    let observed = loop {
        match identity.pidfd.wait_status(terminal_flags).unwrap() {
            Some(status) => break status,
            None => assert!(Instant::now() < deadline, "thread never reached final exit"),
        }
        thread::sleep(Duration::from_millis(1));
    };
    assert!(libc::WIFEXITED(observed));
    assert_eq!(libc::WEXITSTATUS(observed), 17);
    assert!(identity.pidfd_is_live().unwrap());
    assert!(!first_terminal.wait(Duration::ZERO));
    drop(held);
    let exited = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, final_running.wait_owned())
        .await
        .unwrap()
        .unwrap()
        .assume_exited();
    assert_eq!(exited, (first_tid, crate::ExitStatus::Exited(17)));
    assert!(first_terminal.wait(TRACEE_WAIT_TIMEOUT));
    first_cleanup.disarm();
    assert!(!identity.pidfd_is_live().unwrap());
    // The leader is still held at its clone stop. Thread liveness must not
    // silently become the leader's liveness when the thread exits.
    assert!(
        root_terminal
            .event
            .identity()
            .unwrap()
            .pidfd_is_live()
            .unwrap()
    );

    let (parent, crate::Event::NewChild(crate::ChildOp::Clone, child)) = tokio::time::timeout(
        TRACEE_WAIT_TIMEOUT,
        parent.resume(None).unwrap().wait_owned(),
    )
    .await
    .unwrap()
    .unwrap()
    .assume_stopped() else {
        panic!("missing second pthread clone");
    };
    let second_tid = child.pid();
    let mut second_cleanup = legacy_child_cleanup(&child);
    let second_terminal = child.terminal_cleanup();
    let parent_running = parent.resume(None).unwrap();
    // Project the stale routing number onto a real live thread while keeping
    // the old proc descriptor. This is a reuse boundary, not a claim that the
    // kernel allocated the same TID in this unprivileged test.
    let mut projected = identity.pidfd.try_clone().unwrap();
    let ThreadHandle::Procfs { pid, .. } = &mut projected else {
        unreachable!()
    };
    *pid = second_tid;
    assert_eq!(projected.wait_status(terminal_flags), Err(Errno::ECHILD));
    assert_eq!(first_terminal.request_sigkill(), Err(Errno::ESRCH));
    stale_stop.0 = second_tid;
    assert!(stale_stop.1.event().hold_tid().is_none());
    let Err(Error::Died(zombie)) = stale_stop.getregs() else {
        panic!("stale stopped token reached the replacement thread");
    };
    assert_eq!(zombie.0.1.event(), &first_terminal.event);
    let (child, event) = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, child.wait_owned())
        .await
        .unwrap()
        .unwrap()
        .assume_stopped();
    assert_eq!(
        event,
        crate::Event::Signal(Signal::SIGSTOP),
        "stale wait stole the live stop"
    );
    child
        .getregs()
        .expect("replacement thread remains controllable");
    let second_exit = second_cleanup.exit_event(&child).unwrap();
    let second_running = child.resume(None).unwrap();
    let child = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, second_exit)
        .await
        .unwrap()
        .unwrap();
    second_cleanup.mark_claimed_exit();
    drop(second_running);
    assert_eq!(child.getevent().unwrap(), 23 << 8);
    assert_eq!(
        tokio::time::timeout(
            TRACEE_WAIT_TIMEOUT,
            child.resume(None).unwrap().wait_owned()
        )
        .await
        .unwrap()
        .unwrap()
        .assume_exited(),
        (second_tid, crate::ExitStatus::Exited(23)),
    );
    assert!(second_terminal.wait(TRACEE_WAIT_TIMEOUT));
    second_cleanup.disarm();
    let parent = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, root_exit)
        .await
        .unwrap()
        .unwrap();
    root_cleanup.mark_claimed_exit();
    drop(parent_running);
    assert_eq!(parent.getevent().unwrap(), 42 << 8);
    assert_eq!(
        tokio::time::timeout(
            TRACEE_WAIT_TIMEOUT,
            parent.resume(None).unwrap().wait_owned()
        )
        .await
        .unwrap()
        .unwrap()
        .assume_exited(),
        (root.into(), crate::ExitStatus::Exited(42)),
    );
    assert!(root_terminal.wait(TRACEE_WAIT_TIMEOUT));
    root_cleanup.disarm();
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(sanitized))]
async fn forced_legacy_nonleader_cancellation_targets_thread_and_notifies_exit() {
    if run_legacy_test_outer(
        "forced_legacy_nonleader_cancellation_targets_thread_and_notifies_exit",
    ) {
        return;
    }
    let (root, stopped, mut root_cleanup, _force) = spawn_legacy_thread_tracee(true);
    let root_exit = root_cleanup.exit_event(&stopped).unwrap();
    let root_terminal = stopped.terminal_cleanup();
    let (parent, crate::Event::NewChild(crate::ChildOp::Clone, child)) = tokio::time::timeout(
        TRACEE_WAIT_TIMEOUT,
        stopped.resume(None).unwrap().wait_owned(),
    )
    .await
    .unwrap()
    .unwrap()
    .assume_stopped() else {
        panic!("missing paused pthread clone");
    };
    let child_tid = child.pid();
    let mut child_cleanup = legacy_child_cleanup(&child);
    let terminal = child.terminal_cleanup();
    let (child, event) = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, child.wait_owned())
        .await
        .unwrap()
        .unwrap()
        .assume_stopped();
    assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));
    let child_running = child.resume(None).unwrap();
    let parent_running = parent.resume(None).unwrap();
    assert!(matches!(
        root_terminal.event.identity().unwrap().pidfd,
        ThreadHandle::LegacyLeader { .. }
    ));
    root_terminal.request_sigstop().unwrap();
    let (parent, event) = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, parent_running.wait_owned())
        .await
        .unwrap()
        .unwrap()
        .assume_stopped();
    assert_eq!(parent.pid(), root.into());
    assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));
    let parent_running = parent.resume(None).unwrap();
    terminal.request_sigstop().unwrap();
    let (child, event) = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, child_running.wait_owned())
        .await
        .unwrap()
        .unwrap()
        .assume_stopped();
    assert_eq!(child.pid(), child_tid);
    assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));

    let mut exit = Box::pin(child_cleanup.exit_event(&child).unwrap());
    let wakes = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&wakes));
    let mut context = Context::from_waker(&waker);
    assert!(exit.as_mut().poll(&mut context).is_pending());
    assert_eq!(wakes.0.load(Ordering::SeqCst), 0);
    terminal.request_sigkill().unwrap();
    drop(child);
    tokio::time::timeout(TRACEE_WAIT_TIMEOUT, async {
        while wakes.0.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("legacy worker lost the exit notification");
    let child = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, exit.as_mut())
        .await
        .unwrap()
        .unwrap();
    child_cleanup.mark_claimed_exit();
    assert!(
        wakes.0.load(Ordering::SeqCst) >= 1,
        "legacy worker lost the exit notification"
    );
    assert!(
        !terminal.wait(Duration::ZERO),
        "exit stop is not a terminal acknowledgment"
    );
    assert_eq!(
        tokio::time::timeout(
            TRACEE_WAIT_TIMEOUT,
            child.resume(None).unwrap().wait_owned()
        )
        .await
        .unwrap()
        .unwrap()
        .assume_exited(),
        (
            child_tid,
            crate::ExitStatus::Signaled(Signal::SIGKILL, false)
        ),
    );
    assert!(terminal.wait(TRACEE_WAIT_TIMEOUT));
    assert_eq!(terminal.request_sigstop(), Err(Errno::ESRCH));
    child_cleanup.disarm();

    let parent = tokio::time::timeout(TRACEE_WAIT_TIMEOUT, root_exit)
        .await
        .unwrap()
        .unwrap();
    root_cleanup.mark_claimed_exit();
    drop(parent_running);
    assert_eq!(
        tokio::time::timeout(
            TRACEE_WAIT_TIMEOUT,
            parent.resume(None).unwrap().wait_owned()
        )
        .await
        .unwrap()
        .unwrap()
        .assume_exited(),
        (
            root.into(),
            crate::ExitStatus::Signaled(Signal::SIGKILL, false)
        ),
    );
    assert!(root_terminal.wait(TRACEE_WAIT_TIMEOUT));
    root_cleanup.disarm();
}

#[tokio::test(flavor = "current_thread")]
#[cfg(not(sanitized))]
async fn forced_legacy_nonleader_exec_preserves_epochs_and_exact_wait_owners() {
    if run_legacy_test_outer("forced_legacy_nonleader_exec_preserves_epochs_and_exact_wait_owners")
    {
        return;
    }
    // Reuse every assertion of the native thread-pidfd regression. Only the
    // open result differs: this case must bind the non-leader's proc descriptor.
    actual_nonleader_exec_owned_case(true).await;
}
