/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::cell::RefCell;

use super::*;
use crate::testing::test_fn_with_config;

#[tokio::test]
async fn cancellation_before_initial_ready_retains_the_same_wait() {
    let polls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&polls);
    let (sender, mut receiver) = oneshot::channel::<()>();
    let wait = future::poll_fn(move |cx| {
        count.fetch_add(1, Ordering::SeqCst);
        match Pin::new(&mut receiver).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) => Poll::Ready(Err(TraceError::Errno(Errno::ESRCH))),
            Poll::Ready(Err(_)) => panic!("test sender disappeared"),
        }
    });
    let mut initial = InitialChildWait::waiting(wait);
    {
        let observation = initial.observe();
        futures::pin_mut!(observation);
        assert!(futures::poll!(&mut observation).is_pending());
    }
    assert!(matches!(initial, InitialChildWait::Waiting(_)));
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    sender.send(()).unwrap();
    initial.observe().await;
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    initial.observe().await;
    assert_eq!(polls.load(Ordering::SeqCst), 2, "no repeated native wait");
    assert!(matches!(
        initial.into_observed(),
        Err(TraceError::Errno(Errno::ESRCH))
    ));
}

#[tokio::test]
async fn cancellation_after_initial_ready_keeps_the_exact_outcome() {
    let mut initial = InitialChildWait::waiting(async { Err(TraceError::Errno(Errno::EINTR)) });
    {
        let observation = initial.observe();
        futures::pin_mut!(observation);
        assert!(futures::poll!(&mut observation).is_ready());
        // Dropping the completed enclosing future must not drop its outcome.
    }
    assert!(matches!(
        &initial,
        InitialChildWait::Observed(Err(TraceError::Errno(Errno::EINTR)))
    ));
    initial.observe().await;
    assert!(matches!(
        initial.into_observed(),
        Err(TraceError::Errno(Errno::EINTR))
    ));
}

#[derive(Default)]
struct NativeLog {
    creator: Option<Pid>,
    child: Option<Pid>,
    child_events: usize,
    retained: usize,
    creator_terminal: usize,
    child_constructed: usize,
    child_inherited: usize,
    child_started: usize,
    child_terminal: usize,
    consuming_exits: usize,
    creator_worker_drained: usize,
    child_worker_drained: usize,
    child_published: usize,
}
thread_local! {
    // This callback state belongs to one native test's original LocalSet.
    static ACTIVE: RefCell<Option<Arc<StdMutex<NativeLog>>>> = const { RefCell::new(None) };
}
fn active() -> Option<Arc<StdMutex<NativeLog>>> {
    ACTIVE.with(|slot| slot.borrow().clone())
}
struct Active;
impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

pub(super) fn creator_exit_after_native_retention(native: &NativeChild) -> bool {
    admission_tests::retained(native);
    let Some(log) = active() else {
        return false;
    };
    {
        let mut log = log.lock().unwrap();
        assert_eq!(log.creator, Some(native.creator));
        assert_eq!(log.child, Some(native.id));
        assert_eq!(native.kind, ChildTaskKind::Process);
        assert_eq!(native.creator_cleanup.thread_group_id(), Ok(native.creator));
        assert_eq!(native.cleanup.thread_group_id(), Ok(native.id));
        assert!(matches!(&native.initial, InitialChildWait::Waiting(_)));
        assert_eq!(log.child_events, 1);
        assert_eq!(log.child_constructed, 0);
        assert_eq!(log.child_inherited, 0);
        assert_eq!(log.retained, 0);
        log.retained += 1;
    }
    // Intentional native SIGKILL stimulus, not cleanup or an invented Tool
    // outcome. This is the creator's original held notifier PIDFD_THREAD.
    // The production ExitFuture must consume the real terminal event next.
    native
        .creator_cleanup
        .terminate_bound_task()
        .expect("signal exact stopped creator through retained identity");
    true
}

pub(super) fn creator_worker_drained_before_construction(native: &NativeChild) {
    admission_tests::creator_worker_drained(native);
    let Some(log) = active() else {
        return;
    };
    assert!(
        native
            .creator_cleanup
            .wait(std::time::Duration::from_secs(1)),
        "original creator notifier must drain after its actual final observation"
    );
    let mut log = log.lock().unwrap();
    assert_eq!(log.creator, Some(native.creator));
    assert_eq!(log.creator_terminal, 1);
    assert_eq!(log.child_constructed, 0);
    assert_eq!(log.creator_worker_drained, 0);
    log.creator_worker_drained += 1;
}

pub(super) fn child_worker_drained_after_run(id: Pid, cleanup: &TerminalCleanup) {
    admission_tests::child_worker_drained(id, cleanup);
    let Some(log) = active() else {
        return;
    };
    assert!(
        cleanup.wait(std::time::Duration::from_secs(1)),
        "original child notifier must drain after its actual final observation"
    );
    let mut log = log.lock().unwrap();
    assert_eq!(log.child, Some(id));
    assert_eq!(log.child_terminal, 1);
    assert_eq!(log.child_worker_drained, 0);
    log.child_worker_drained += 1;
}

pub(super) fn child_published(id: Pid) {
    let Some(log) = active() else {
        return;
    };
    let mut log = log.lock().unwrap();
    assert_eq!(log.child, Some(id));
    assert_eq!(log.child_inherited, 1);
    assert_eq!(log.child_published, 0);
    log.child_published += 1;
}

#[derive(Default)]
struct NativeGlobal;
#[reverie::global_tool]
impl GlobalTool for NativeGlobal {
    type Config = ();
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _from: Pid, _request: ()) {}
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct NativeState {
    child: Option<i32>,
    terminal: bool,
}

#[derive(Default)]
struct NativeObserver {
    process: Option<Pid>,
}

#[reverie::tool]
impl Tool for NativeObserver {
    type GlobalState = NativeGlobal;
    type ThreadState = NativeState;

    fn new(pid: Pid, _: &()) -> Self {
        if let Some(log) = active() {
            let mut log = log.lock().unwrap();
            if log.child == Some(pid) {
                assert_eq!(
                    log.creator_terminal, 1,
                    "real final wait must precede construction"
                );
                assert_eq!(log.child_constructed, 0);
                log.child_constructed += 1;
            }
        }
        Self { process: Some(pid) }
    }

    fn subscriptions(_: &()) -> Subscription {
        let mut selected = Subscription::none();
        selected.syscalls([Sysno::clone, Sysno::getpid]);
        selected
    }

    fn observe_injected_syscalls(_: &()) -> bool {
        true
    }

    fn init_thread_state(&self, tid: Pid, parent: Option<(Pid, &NativeState)>) -> NativeState {
        if let Some((creator, parent)) = parent {
            let log = active().unwrap();
            let mut log = log.lock().unwrap();
            assert_eq!(log.child, Some(tid));
            assert_eq!(log.creator, Some(creator));
            assert_eq!(
                parent.child,
                Some(tid.as_raw()),
                "inherit exact original Tool state"
            );
            assert!(
                parent.terminal,
                "creator's actual terminal observation was retained"
            );
            assert_eq!(log.creator_terminal, 1);
            assert_eq!(log.child_constructed, 1);
            assert_eq!(log.child_inherited, 0);
            log.child_inherited += 1;
        }
        NativeState::default()
    }

    fn on_injected_syscall_observed(
        &self,
        tid: Pid,
        _: &NativeGlobal,
        state: &mut NativeState,
        nr: Sysno,
        _: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        if let InjectedSyscallEvent::ChildCreated(child) = event {
            assert_eq!(nr, Sysno::clone);
            assert!(state.child.replace(child.as_raw()).is_none());
            let log = active().unwrap();
            let mut log = log.lock().unwrap();
            assert_eq!(log.child_events, 0);
            assert_eq!(self.process, Some(tid));
            log.creator = Some(tid);
            log.child = Some(child);
            log.child_events += 1;
        }
    }

    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        let log = active().unwrap();
        let mut log = log.lock().unwrap();
        if log.child == Some(guest.tid()) {
            assert_eq!(guest.pid(), guest.tid());
            assert_eq!(self.process, Some(guest.pid()));
            assert_eq!(log.child_constructed, 1);
            assert_eq!(log.child_inherited, 1);
            assert_eq!(log.child_started, 0);
            log.child_started += 1;
        }
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        let pid_call = matches!(&syscall, Syscall::Getpid(_));
        let result = guest.inject(syscall).await?;
        if pid_call {
            assert_eq!(result, guest.pid().as_raw() as i64);
        }
        Ok(result)
    }

    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        _: &NativeGlobal,
        state: &mut NativeState,
        status: ExitStatus,
    ) {
        assert!(!state.terminal);
        state.terminal = true;
        let log = active().unwrap();
        let mut log = log.lock().unwrap();
        if log.creator == Some(tid) {
            assert_eq!(status, ExitStatus::Signaled(Signal::SIGKILL, false));
            assert_eq!(log.retained, 1);
            assert_eq!(log.child_constructed, 0);
            assert_eq!(log.creator_terminal, 0);
            log.creator_terminal += 1;
        } else {
            assert_eq!(log.child, Some(tid));
            assert_eq!(status, ExitStatus::Exited(37));
            assert_eq!(log.child_started, 1);
            assert_eq!(log.child_terminal, 0);
            log.child_terminal += 1;
        }
    }

    async fn on_exit_thread<G: GlobalRPC<NativeGlobal>>(
        &self,
        tid: Pid,
        _: &G,
        state: NativeState,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert!(state.terminal);
        let log = active().unwrap();
        let mut log = log.lock().unwrap();
        if log.creator == Some(tid) {
            assert_eq!(status, ExitStatus::Signaled(Signal::SIGKILL, false));
            assert_eq!(
                log.child_inherited, 1,
                "original parent state cannot be consumed early"
            );
        } else {
            assert_eq!(log.child, Some(tid));
            assert_eq!(status, ExitStatus::Exited(37));
        }
        log.consuming_exits += 1;
        Ok(())
    }
}

#[test]
fn native_creator_final_wait_precedes_surviving_child_construction() {
    const MARKER: &[u8] = b"owned-preconstruction surviving child\n";
    let log = Arc::new(StdMutex::new(NativeLog::default()));
    ACTIVE.with(|slot| {
        assert!(slot.borrow_mut().replace(Arc::clone(&log)).is_none());
    });
    let _active = Active;
    let (output, _) = test_fn_with_config::<NativeObserver, _>(
        || unsafe {
            let child = libc::syscall(
                libc::SYS_clone,
                libc::SIGCHLD,
                0usize,
                0usize,
                0usize,
                0usize,
            );
            if child == 0 {
                let id = libc::syscall(libc::SYS_getpid);
                if id <= 0 {
                    libc::_exit(90);
                }
                if libc::write(1, MARKER.as_ptr().cast(), MARKER.len()) != MARKER.len() as isize {
                    libc::_exit(91);
                }
                libc::_exit(37);
            }
            // The retained creator is killed at its actual NewChild stop, before
            // returning from clone. Reaching this sentinel is always a failure.
            libc::_exit(92);
        },
        (),
        true,
    )
    .expect("actual creator cancellation and surviving newborn");
    assert_eq!(output.status, ExitStatus::Signaled(Signal::SIGKILL, false));
    assert_eq!(output.stdout, MARKER);
    let log = log.lock().unwrap();
    assert_eq!(log.child_events, 1);
    assert_eq!(log.retained, 1);
    assert_eq!(log.creator_terminal, 1);
    assert_eq!(log.child_constructed, 1);
    assert_eq!(log.child_inherited, 1);
    assert_eq!(log.child_started, 1);
    assert_eq!(log.child_terminal, 1);
    assert_eq!(log.consuming_exits, 2);
    assert_eq!(log.child_published, 1);
    assert_eq!(log.creator_worker_drained, 1);
    assert_eq!(log.child_worker_drained, 1);
    println!(
        "owned-preconstruction creator={} child={} creator-final-before-construction=1 child-start=1 child-final37=1 consumed=2 child-list-transfer=1 original-workers-drained=2",
        log.creator.unwrap(),
        log.child.unwrap()
    );
}

#[cfg(test)]
mod admission_tests {
    use std::os::fd::AsFd;
    use std::os::fd::AsRawFd;
    use std::os::fd::BorrowedFd;
    use std::os::fd::OwnedFd;

    use super::*;

    #[derive(Default)]
    struct Log {
        creator: Option<Pid>,
        child: Option<Pid>,
        creator_pin: Option<OwnedFd>,
        child_pin: Option<OwnedFd>,
        requirement_queries: usize,
        calls: usize,
        completed: usize,
        constructed: usize,
        inherited: usize,
        started: usize,
        creator_final: usize,
        child_final: usize,
        consumed: usize,
        creator_worker_drained: usize,
        child_worker_drained: usize,
    }
    thread_local! {
        static LOG: RefCell<Option<Arc<StdMutex<Log>>>> = const { RefCell::new(None) };
    }
    fn log() -> Option<Arc<StdMutex<Log>>> {
        LOG.with(|slot| slot.borrow().clone())
    }
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            LOG.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }
    fn identity(pin: BorrowedFd<'_>) -> (u64, u64) {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        assert_eq!(
            unsafe { libc::fstat(pin.as_raw_fd(), stat.as_mut_ptr()) },
            0
        );
        let stat = unsafe { stat.assume_init() };
        (stat.st_dev, stat.st_ino)
    }
    pub(super) fn retained(native: &NativeChild) {
        let Some(log) = log() else {
            return;
        };
        let mut log = log.lock().unwrap();
        assert_eq!(
            (log.creator, log.child),
            (Some(native.creator), Some(native.id))
        );
        assert_eq!(native.kind, ChildTaskKind::Process);
        assert!(native.admission_required);
        assert!(!native.admission_done);
        assert!(
            log.creator_pin
                .replace(
                    native
                        .creator_cleanup
                        .duplicate_bound_thread_pidfd()
                        .unwrap()
                )
                .is_none()
        );
        assert!(
            log.child_pin
                .replace(native.cleanup.duplicate_bound_thread_pidfd().unwrap())
                .is_none()
        );
    }
    pub(super) fn creator_worker_drained(native: &NativeChild) {
        let Some(log) = log() else {
            return;
        };
        assert!(
            native
                .creator_cleanup
                .wait(std::time::Duration::from_secs(1))
        );
        assert_eq!(
            native.creator_cleanup.observed_terminal().unwrap().unwrap(),
            ExitStatus::Signaled(Signal::SIGKILL, false),
        );
        let mut log = log.lock().unwrap();
        assert_eq!(log.creator, Some(native.creator));
        assert_eq!(
            (
                log.creator_final,
                log.constructed,
                log.creator_worker_drained
            ),
            (1, 0, 0)
        );
        log.creator_worker_drained += 1;
    }
    pub(super) fn child_worker_drained(id: Pid, cleanup: &TerminalCleanup) {
        let Some(log) = log() else {
            return;
        };
        assert!(cleanup.wait(std::time::Duration::from_secs(1)));
        assert_eq!(
            cleanup.observed_terminal().unwrap().unwrap(),
            ExitStatus::Exited(37)
        );
        let mut log = log.lock().unwrap();
        assert_eq!(log.child, Some(id));
        assert_eq!((log.child_final, log.child_worker_drained), (1, 0));
        log.child_worker_drained += 1;
    }
    #[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
    struct State {
        child: Option<i32>,
        admitted: bool,
        admission_requested: bool,
        terminal: bool,
    }
    #[derive(Default)]
    struct Observer;
    #[reverie::tool]
    impl Tool for Observer {
        type GlobalState = NativeGlobal;
        type ThreadState = State;
        fn new(pid: Pid, _: &()) -> Self {
            let log = log().unwrap();
            let mut log = log.lock().unwrap();
            if log.child == Some(pid) {
                assert_eq!((log.calls, log.completed, log.creator_final), (2, 1, 1));
                assert_eq!(log.constructed, 0);
                log.constructed += 1;
            }
            Self
        }
        fn subscriptions(_: &()) -> Subscription {
            let mut selected = Subscription::none();
            selected.syscalls([Sysno::clone, Sysno::getpid]);
            selected
        }
        fn observe_injected_syscalls(_: &()) -> bool {
            true
        }
        fn requires_native_child_admission(&self, parent: &State) -> bool {
            let log = log().unwrap();
            let mut log = log.lock().unwrap();
            log.requirement_queries += 1;
            parent.child.is_some() && parent.admission_requested
        }
        async fn admit_native_child(
            &self,
            creator: Tid,
            child: Tid,
            _: &NativeGlobal,
            parent: &mut State,
            child_pidfd: BorrowedFd<'_>,
            terminal: Option<ExitStatus>,
        ) -> Result<(), reverie::Error> {
            let first = {
                let log = log().unwrap();
                let mut log = log.lock().unwrap();
                assert_eq!((log.creator, log.child), (Some(creator), Some(child)));
                assert_eq!(parent.child, Some(child.as_raw()));
                assert!(!parent.admitted);
                assert_eq!(
                    terminal, None,
                    "live child is still held at its original initial stop"
                );
                assert_eq!(
                    identity(child_pidfd),
                    identity(log.child_pin.as_ref().unwrap().as_fd())
                );
                assert_ne!(
                    identity(child_pidfd),
                    identity(log.creator_pin.as_ref().unwrap().as_fd())
                );
                assert_eq!((log.constructed, log.inherited, log.started), (0, 0, 0));
                log.calls += 1;
                if log.calls == 1 {
                    assert_eq!(log.creator_final, 0);
                    assert!(parent.admission_requested);
                    // A canceled callback can change Tool-owned state. The
                    // backend must retain its original required decision.
                    parent.admission_requested = false;
                    // Actual creator death cancels the real callback future.
                    // The test neither supplies a terminal fact nor resumes the child.
                    assert_eq!(
                        unsafe {
                            libc::syscall(
                                libc::SYS_pidfd_send_signal,
                                log.creator_pin.as_ref().unwrap().as_raw_fd(),
                                libc::SIGKILL,
                                std::ptr::null::<libc::siginfo_t>(),
                                0,
                            )
                        },
                        0
                    );
                    true
                } else {
                    assert_eq!((log.calls, log.creator_final, log.completed), (2, 1, 0));
                    assert!(parent.terminal);
                    assert!(!parent.admission_requested);
                    assert_eq!(log.requirement_queries, 1);
                    log.completed += 1;
                    false
                }
            };
            if first {
                future::pending::<()>().await;
            }
            parent.admitted = true;
            Ok(())
        }
        fn init_thread_state(&self, tid: Pid, parent: Option<(Pid, &State)>) -> State {
            if let Some((creator, parent)) = parent {
                let log = log().unwrap();
                let mut log = log.lock().unwrap();
                assert_eq!((log.creator, log.child), (Some(creator), Some(tid)));
                assert!(parent.admitted && parent.terminal);
                assert_eq!((log.completed, log.constructed, log.inherited), (1, 1, 0));
                log.inherited += 1;
            }
            State::default()
        }
        fn on_injected_syscall_observed(
            &self,
            tid: Pid,
            _: &NativeGlobal,
            state: &mut State,
            nr: Sysno,
            _: SyscallArgs,
            event: InjectedSyscallEvent,
        ) {
            if let InjectedSyscallEvent::ChildCreated(child) = event {
                assert_eq!(nr, Sysno::clone);
                assert!(state.child.replace(child.as_raw()).is_none());
                assert!(!state.admission_requested);
                state.admission_requested = true;
                let log = log().unwrap();
                let mut log = log.lock().unwrap();
                assert!(log.creator.replace(tid).is_none());
                assert!(log.child.replace(child).is_none());
            }
        }
        async fn handle_thread_start<G: Guest<Self>>(
            &self,
            guest: &mut G,
        ) -> Result<(), reverie::Error> {
            let log = log().unwrap();
            let mut log = log.lock().unwrap();
            if log.child == Some(guest.tid()) {
                assert_eq!((log.completed, log.inherited, log.started), (1, 1, 0));
                log.started += 1;
            }
            Ok(())
        }
        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            call: Syscall,
        ) -> Result<i64, reverie::Error> {
            Ok(guest.inject(call).await?)
        }
        fn on_backend_thread_terminal(
            &self,
            tid: Pid,
            _: &NativeGlobal,
            state: &mut State,
            status: ExitStatus,
        ) {
            assert!(!state.terminal);
            state.terminal = true;
            let log = log().unwrap();
            let mut log = log.lock().unwrap();
            if log.creator == Some(tid) {
                assert_eq!(status, ExitStatus::Signaled(Signal::SIGKILL, false));
                assert_eq!(
                    (log.calls, log.completed, log.constructed, log.creator_final),
                    (1, 0, 0, 0)
                );
                log.creator_final += 1;
            } else {
                assert_eq!(log.child, Some(tid));
                assert_eq!(status, ExitStatus::Exited(37));
                assert_eq!((log.started, log.child_final), (1, 0));
                log.child_final += 1;
            }
        }
        async fn on_exit_thread<G: GlobalRPC<NativeGlobal>>(
            &self,
            _: Pid,
            _: &G,
            state: State,
            _: ExitStatus,
        ) -> Result<(), reverie::Error> {
            assert!(state.terminal);
            let log = log().unwrap();
            let mut log = log.lock().unwrap();
            assert_eq!(log.inherited, 1);
            log.consumed += 1;
            Ok(())
        }
    }
    #[test]
    fn actual_admission_cancellation_retains_child_pin_and_precedes_construction() {
        use std::os::fd::FromRawFd;
        use std::os::unix::net::UnixDatagram;
        use std::os::unix::process::CommandExt;

        const TEST: &str = "task::preconstruction_tests::admission_tests::actual_admission_cancellation_retains_child_pin_and_precedes_construction";
        const CHILD_ENV: &str = "REVERIE_ADMISSION_REAPER_TEST_CHILD";
        const RECEIPT_ENV: &str = "REVERIE_ADMISSION_REAPER_TEST_RECEIPT";
        if std::env::var(CHILD_ENV).as_deref() != Ok(TEST) {
            // Keep the process-wide subreaper setting out of the ordinary
            // libtest process, including concurrent cargo-test executions.
            let (receipt, sender) = UnixDatagram::pair().unwrap();
            receipt.set_nonblocking(true).unwrap();
            let fd = sender.as_raw_fd();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
                .env(CHILD_ENV, TEST)
                .env(RECEIPT_ENV, fd.to_string());
            // Inherit the maintained owner's process group, deadline and
            // streaming output bounds. No detached process or captured buffer.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0
                        || libc::fcntl(fd, libc::F_SETFD, 0) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn().unwrap();
            drop(sender);
            let status = child.wait().unwrap();
            assert!(status.success(), "isolated admission test failed: {status}");
            // Only this exact test sends after every original assertion.
            // A stale selector producing a zero-test success cannot qualify.
            let mut byte = [0; 2];
            assert_eq!(receipt.recv(&mut byte).unwrap(), 1);
            assert_eq!(byte[0], 1);
            return;
        }
        let mut subreaper: libc::c_int = 0;
        assert_eq!(
            unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut subreaper, 0, 0, 0) },
            0
        );
        assert_eq!(subreaper, 1, "isolated tracer must own real-parent custody");
        let receipt_fd: i32 = std::env::var(RECEIPT_ENV).unwrap().parse().unwrap();
        assert!(receipt_fd >= 0);
        // This descriptor is transferred only to the private re-exec above.
        let receipt = unsafe { UnixDatagram::from_raw_fd(receipt_fd) };
        const MARKER: &[u8] = b"retained child admission\n";
        let log = Arc::new(StdMutex::new(Log::default()));
        LOG.with(|slot| assert!(slot.borrow_mut().replace(Arc::clone(&log)).is_none()));
        let _clear = Clear;
        let (output, _) = test_fn_with_config::<Observer, _>(
            || unsafe {
                let child = libc::syscall(
                    libc::SYS_clone,
                    libc::SIGCHLD,
                    0usize,
                    0usize,
                    0usize,
                    0usize,
                );
                if child == 0 {
                    if libc::syscall(libc::SYS_getpid) <= 0 {
                        libc::_exit(90);
                    }
                    if libc::write(1, MARKER.as_ptr().cast(), MARKER.len()) != MARKER.len() as isize
                    {
                        libc::_exit(91);
                    }
                    libc::_exit(37);
                }
                libc::_exit(92); // creator never returns from the retained clone
            },
            (),
            true,
        )
        .expect("real callback cancellation must preserve the exact child");
        assert_eq!(output.status, ExitStatus::Signaled(Signal::SIGKILL, false));
        assert_eq!(output.stdout, MARKER);
        let log = log.lock().unwrap();
        assert_eq!(log.requirement_queries, 1);
        assert_eq!(
            (
                log.calls,
                log.completed,
                log.constructed,
                log.inherited,
                log.started
            ),
            (2, 1, 1, 1, 1)
        );
        assert_eq!(
            (log.creator_final, log.child_final, log.consumed),
            (1, 1, 2)
        );
        assert_eq!(
            (log.creator_worker_drained, log.child_worker_drained),
            (1, 1)
        );
        // Both original notifier workers are finished and actual final statuses
        // were consumed. This exact retained child pin now belongs to this
        // isolated real parent; no concurrent notifier status can be stolen.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let reaped = unsafe {
            libc::waitid(
                libc::P_PIDFD,
                log.child_pin.as_ref().unwrap().as_raw_fd() as u32,
                &mut info,
                libc::WEXITED | libc::WNOHANG,
            )
        };
        if reaped == 0 {
            assert_eq!(unsafe { info.si_pid() }, log.child.unwrap().as_raw());
            assert_eq!(info.si_code, libc::CLD_EXITED);
            assert_eq!(unsafe { info.si_status() }, 37);
        } else {
            assert_eq!(reaped, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
            // Same-thread-group adoption can let the original ptrace final
            // already reap it. ECHILD alone is never success: HUP below is
            // required in both paths, together with actual final37 above.
        }
        for pin in [
            log.creator_pin.as_ref().unwrap(),
            log.child_pin.as_ref().unwrap(),
        ] {
            let mut pfd = libc::pollfd {
                fd: pin.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(unsafe { libc::poll(&mut pfd, 1, 0) }, 1);
            assert_eq!(
                pfd.revents & (libc::POLLIN | libc::POLLHUP),
                libc::POLLIN | libc::POLLHUP
            );
            assert_eq!(pfd.revents & (libc::POLLERR | libc::POLLNVAL), 0);
        }
        println!(
            "admission real-parent custody: creator={} child={} original-workers-drained=2 exact-extra-reap={} HUP17=2",
            log.creator.unwrap(),
            log.child.unwrap(),
            reaped == 0
        );
        assert_eq!(receipt.send(&[1]).unwrap(), 1);
    }
}
