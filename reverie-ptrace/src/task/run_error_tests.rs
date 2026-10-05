//! Ordinary run-loop failure must consume the real tree even when the Tool's
//! backend-failure hooks use their default no-op/pending implementations.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::os::fd::AsRawFd;
use std::time::Duration;

use super::*;

// The ptrace final wait and the real parent's reap are distinct when a guest
// process outlives its guest parent. Give only these cases an isolated real
// parent, following the existing admission-cancellation fixture's protocol.
fn isolated_real_parent(test: &str) -> Option<std::os::unix::net::UnixDatagram> {
    use std::os::fd::FromRawFd;
    use std::os::unix::net::UnixDatagram;
    use std::os::unix::process::CommandExt;
    const CHILD: &str = "REVERIE_RUN_ERROR_REAPER_TEST_CHILD";
    const RECEIPT: &str = "REVERIE_RUN_ERROR_REAPER_TEST_RECEIPT";
    if std::env::var(CHILD).as_deref() != Ok(test) {
        let (receipt, sender) = UnixDatagram::pair().unwrap();
        receipt.set_nonblocking(true).unwrap();
        let fd = sender.as_raw_fd();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD, test)
            .env(RECEIPT, fd.to_string());
        // Preserve the maintained owner's group, deadline and streaming bounds.
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
        assert!(status.success(), "isolated run-error test failed: {status}");
        let mut byte = [0; 2];
        assert_eq!(receipt.recv(&mut byte).unwrap(), 1);
        assert_eq!(byte[0], 1);
        return None;
    }
    let mut subreaper: libc::c_int = 0;
    assert_eq!(
        unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut subreaper, 0, 0, 0) },
        0
    );
    assert_eq!(
        subreaper, 1,
        "isolated fixture must own its orphaned real children"
    );
    let fd: i32 = std::env::var(RECEIPT).unwrap().parse().unwrap();
    assert!(fd >= 0);
    Some(unsafe { UnixDatagram::from_raw_fd(fd) })
}

#[derive(Debug)]
struct OriginalFailure;
impl std::fmt::Display for OriginalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("original child Tool failure with a live sibling")
    }
}
impl std::error::Error for OriginalFailure {}
#[derive(Default)]
struct Log {
    owners: BTreeMap<Pid, TerminalCleanup>,
    counts: Option<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
    failing: Option<Pid>,
    sibling: Option<Pid>,
    released: Option<oneshot::Sender<()>>,
    wait: Option<oneshot::Receiver<()>>,
    terminal: BTreeMap<Pid, ExitStatus>,
    consumed: BTreeSet<Pid>,
    processes: BTreeSet<Pid>,
    retired: BTreeSet<Pid>,
}
thread_local! {
    static ACTIVE: RefCell<Option<Arc<StdMutex<Log>>>> = const { RefCell::new(None) };
}
fn active() -> Option<Arc<StdMutex<Log>>> {
    ACTIVE.with(|slot| slot.borrow().clone())
}
struct Active;
impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.with(|slot| assert!(slot.borrow_mut().take().is_some()));
    }
}
pub(super) fn retained(task: &Stopped, tasks: &Arc<AtomicUsize>, daemons: &Arc<AtomicUsize>) {
    parent_join_tests::retained(task);
    #[cfg(target_arch = "x86_64")]
    injected_trap::retained(task, tasks, daemons);
    let Some(log) = active() else { return };
    let mut log = log.lock().unwrap();
    assert!(
        log.owners
            .insert(task.pid(), task.terminal_cleanup())
            .is_none()
    );
    if let Some((old_tasks, old_daemons)) = &log.counts {
        assert!(Arc::ptr_eq(old_tasks, tasks));
        assert!(Arc::ptr_eq(old_daemons, daemons));
    } else {
        log.counts = Some((Arc::clone(tasks), Arc::clone(daemons)));
    }
}
pub(super) fn retired(tid: Pid, tasks: &Arc<AtomicUsize>, daemons: &Arc<AtomicUsize>) {
    #[cfg(target_arch = "x86_64")]
    injected_trap::retired(tid, tasks, daemons);
    let Some(log) = active() else { return };
    let mut log = log.lock().unwrap();
    assert!(log.terminal.contains_key(&tid));
    assert!(log.consumed.contains(&tid));
    assert!(log.processes.contains(&tid));
    assert!(log.retired.insert(tid));
    assert_eq!(tasks.load(Ordering::SeqCst), 3 - log.retired.len());
    assert_eq!(daemons.load(Ordering::SeqCst), 0);
}
#[derive(Default)]
struct Global;
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = ();
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
    // Deliberately inherit both default failure hooks.
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct State {
    terminal: bool,
}
#[derive(Default)]
struct Observer;
#[reverie::tool]
impl Tool for Observer {
    type GlobalState = Global;
    type ThreadState = State;
    fn subscriptions(_: &()) -> Subscription {
        let mut sub = Subscription::none();
        sub.syscall(Sysno::getuid);
        sub
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        assert!(matches!(call, Syscall::Getuid(_)));
        let (_, args) = call.into_parts();
        let log = active().unwrap();
        match args.arg0 {
            1 => {
                let wait = {
                    let mut log = log.lock().unwrap();
                    assert!(log.failing.replace(guest.tid()).is_none());
                    log.wait.take().unwrap()
                };
                wait.await
                    .expect("actual sibling entered its pending Tool callback");
                {
                    let log = log.lock().unwrap();
                    assert_eq!(log.owners.len(), 3);
                    assert!(log.sibling.is_some());
                    assert!(log.terminal.is_empty());
                    assert!(
                        log.owners
                            .values()
                            .all(|owner| owner.observed_terminal().is_none())
                    );
                }
                Err(reverie::Error::Tool(anyhow::Error::new(OriginalFailure)))
            }
            2 => {
                {
                    let mut log = log.lock().unwrap();
                    assert!(log.sibling.replace(guest.tid()).is_none());
                    log.released.take().unwrap().send(()).unwrap();
                }
                future::pending().await
            }
            value => panic!("unexpected control operand {value}"),
        }
    }
    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        _: &Global,
        state: &mut State,
        status: ExitStatus,
    ) {
        assert!(!state.terminal);
        state.terminal = true;
        assert_eq!(status, ExitStatus::Signaled(Signal::SIGKILL, false));
        let log = active().unwrap();
        let mut log = log.lock().unwrap();
        assert!(
            matches!(log.owners[&tid].observed_terminal(), Some(Ok(actual)) if actual == status)
        );
        assert!(log.terminal.insert(tid, status).is_none());
    }
    async fn on_exit_thread<G: GlobalRPC<Global>>(
        &self,
        tid: Pid,
        _: &G,
        state: State,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert!(state.terminal);
        let log = active().unwrap();
        let mut log = log.lock().unwrap();
        assert_eq!(log.terminal.get(&tid), Some(&status));
        assert!(log.consumed.insert(tid));
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Global>>(
        self,
        pid: Pid,
        _: &G,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        let log = active().unwrap();
        let mut log = log.lock().unwrap();
        assert_eq!(log.terminal.get(&pid), Some(&status));
        assert!(log.consumed.contains(&pid));
        assert!(log.processes.insert(pid));
        Ok(())
    }
}
#[test]
fn ordinary_child_error_drains_actual_root_and_pending_sibling_with_default_failure_hooks() {
    let Some(receipt) = isolated_real_parent(
        "task::run_error_tests::ordinary_child_error_drains_actual_root_and_pending_sibling_with_default_failure_hooks",
    ) else {
        return;
    };
    let (released, wait) = oneshot::channel();
    let log = Arc::new(StdMutex::new(Log {
        released: Some(released),
        wait: Some(wait),
        ..Log::default()
    }));
    ACTIVE.with(|slot| assert!(slot.borrow_mut().replace(Arc::clone(&log)).is_none()));
    let _active = Active;
    let result = crate::testing::test_fn::<Observer, _>(|| unsafe {
        for role in [1usize, 2] {
            let child = libc::fork();
            assert!(child >= 0);
            if child == 0 {
                libc::syscall(libc::SYS_getuid, role, 0, 0, 0, 0, 0);
                panic!("failed or cancelled child callback returned to guest");
            }
        }
        loop {
            libc::pause();
        }
    });
    match result {
        Err(reverie::Error::Tool(error)) => {
            assert!(error.is::<OriginalFailure>(), "original type lost: {error}")
        }
        _ => panic!("tree did not preserve the original typed child error"),
    }
    let log = log.lock().unwrap();
    assert_eq!(log.owners.len(), 3);
    assert_ne!(log.failing, log.sibling);
    assert_eq!(log.terminal.len(), 3);
    assert_eq!(log.consumed.len(), 3);
    assert_eq!(log.processes.len(), 3);
    assert_eq!(log.retired.len(), 3);
    let (tasks, daemons) = log.counts.as_ref().unwrap();
    assert_eq!(tasks.load(Ordering::SeqCst), 0);
    assert_eq!(daemons.load(Ordering::SeqCst), 0);
    for (tid, owner) in &log.owners {
        assert!(
            owner.wait(Duration::from_secs(1)),
            "original notifier did not drain {tid}"
        );
        assert!(matches!(
            owner.observed_terminal(),
            Some(Ok(ExitStatus::Signaled(Signal::SIGKILL, false)))
        ));
        assert_eq!(unsafe { libc::kill(tid.as_raw(), 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert_eq!(
            unsafe { libc::waitpid(tid.as_raw(), std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
    assert_eq!(receipt.send(&[1]).unwrap(), 1);
}
#[test]
fn consumed_run_error_keeps_failure_wake_sticky_and_original_kind_distinct() {
    let failure = ParentCompletionFailure::default();
    assert!(failure.wait().now_or_never().is_none());
    failure.record_run_error(
        Pid::from_raw(41),
        reverie::Error::Tool(anyhow::Error::new(OriginalFailure)),
    );
    assert!(failure.wait().now_or_never().is_some());
    assert!(
        failure.cause().is_none(),
        "run error was relabelled native parent completion"
    );
    let (tid, error) = failure.take_run_error().unwrap();
    assert_eq!(tid, Pid::from_raw(41));
    assert!(matches!(error, reverie::Error::Tool(error) if error.is::<OriginalFailure>()));
    assert!(failure.run_failed());
    assert!(failure.wait().now_or_never().is_some());
    failure.record_run_error(Pid::from_raw(42), Errno::EIO.into());
    assert!(
        failure.take_run_error().is_none(),
        "consumed first cause was overwritten"
    );
    assert!(failure.run_failed());
    assert!(failure.wait().now_or_never().is_some());
}

/// These scenarios use separate state so the preceding child-error control and
/// each of its original assertions remain unchanged.
pub(super) mod additional {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Case {
        RootError,
        LateChildError,
        CleanupError,
        ConsumedFinal,
    }
    type ExitWaiter = std::pin::Pin<Box<dyn Future<Output = Result<Stopped, TraceError>> + Send>>;

    struct Scenario {
        case: Case,
        owners: BTreeMap<Pid, TerminalCleanup>,
        exit_waiters: BTreeMap<Pid, ExitWaiter>,
        counts: Option<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
        root: Option<Pid>,
        roles: BTreeMap<usize, Pid>,
        ready_tx: Option<oneshot::Sender<()>>,
        ready_rx: Option<oneshot::Receiver<()>>,
        error_tx: Option<oneshot::Sender<()>>,
        error_rx: Option<oneshot::Receiver<()>>,
        root_success: bool,
        cleanup_tx: Option<oneshot::Sender<()>>,
        cleanup_rx: Option<oneshot::Receiver<()>>,
        cleanup_waited: bool,
        terminal: BTreeMap<Pid, ExitStatus>,
        consumed: BTreeSet<Pid>,
        processes: BTreeSet<Pid>,
        retired: BTreeSet<Pid>,
    }
    impl Scenario {
        fn new(case: Case) -> Self {
            let (ready_tx, ready_rx) = oneshot::channel();
            let (error_tx, error_rx) = oneshot::channel();
            let (cleanup_tx, cleanup_rx) = oneshot::channel();
            Self {
                case,
                owners: BTreeMap::new(),
                exit_waiters: BTreeMap::new(),
                counts: None,
                root: None,
                roles: BTreeMap::new(),
                ready_tx: Some(ready_tx),
                ready_rx: Some(ready_rx),
                error_tx: Some(error_tx),
                error_rx: Some(error_rx),
                root_success: false,
                cleanup_tx: Some(cleanup_tx),
                cleanup_rx: Some(cleanup_rx),
                cleanup_waited: false,
                terminal: BTreeMap::new(),
                consumed: BTreeSet::new(),
                processes: BTreeSet::new(),
                retired: BTreeSet::new(),
            }
        }
        fn expected_count(&self) -> usize {
            if self.case == Case::ConsumedFinal {
                1
            } else {
                3
            }
        }
        fn expected_status(&self, tid: Pid) -> ExitStatus {
            if Some(tid) == self.root
                && matches!(self.case, Case::LateChildError | Case::CleanupError)
            {
                ExitStatus::Exited(17)
            } else if self.case == Case::ConsumedFinal {
                ExitStatus::Exited(23)
            } else {
                ExitStatus::Signaled(Signal::SIGKILL, false)
            }
        }
    }
    thread_local! {
        static SCENARIO: RefCell<Option<Arc<StdMutex<Scenario>>>> = const { RefCell::new(None) };
    }
    fn scenario() -> Option<Arc<StdMutex<Scenario>>> {
        SCENARIO.with(|s| s.borrow().clone())
    }
    struct ActiveScenario;
    impl Drop for ActiveScenario {
        fn drop(&mut self) {
            SCENARIO.with(|s| assert!(s.borrow_mut().take().is_some()));
        }
    }
    pub(in super::super) fn retained(
        task: &Stopped,
        tasks: &Arc<AtomicUsize>,
        daemons: &Arc<AtomicUsize>,
    ) {
        let Some(log) = scenario() else { return };
        let mut log = log.lock().unwrap();
        if log.root.is_none() {
            log.root = Some(task.pid());
        }
        assert!(task.terminal_cleanup().observed_terminal().is_none());
        assert!(
            log.owners
                .insert(task.pid(), task.terminal_cleanup())
                .is_none()
        );
        assert!(
            log.exit_waiters
                .insert(task.pid(), Box::pin(task.exit_event()))
                .is_none()
        );
        if let Some((old_tasks, old_daemons)) = &log.counts {
            assert!(Arc::ptr_eq(old_tasks, tasks));
            assert!(Arc::ptr_eq(old_daemons, daemons));
        } else {
            log.counts = Some((Arc::clone(tasks), Arc::clone(daemons)));
        }
    }
    pub(in super::super) fn retired(
        tid: Pid,
        tasks: &Arc<AtomicUsize>,
        daemons: &Arc<AtomicUsize>,
    ) {
        let Some(log) = scenario() else { return };
        let mut log = log.lock().unwrap();
        assert_eq!(log.terminal.get(&tid), Some(&log.expected_status(tid)));
        assert!(log.consumed.contains(&tid));
        assert!(log.processes.contains(&tid));
        assert!(log.retired.insert(tid));
        assert_eq!(
            tasks.load(Ordering::SeqCst),
            log.expected_count() - log.retired.len()
        );
        assert_eq!(daemons.load(Ordering::SeqCst), 0);
        if log.case == Case::CleanupError && Some(tid) != log.root && log.retired.len() == 2 {
            assert!(!log.retired.contains(&log.root.unwrap()));
            log.cleanup_tx.take().unwrap().send(()).unwrap();
        }
    }
    pub(in super::super) fn root_succeeded(
        tid: Pid,
        status: ExitStatus,
        failure: &ParentCompletionFailure,
    ) {
        let Some(log) = scenario() else { return };
        let mut log = log.lock().unwrap();
        assert_eq!(Some(tid), log.root);
        assert_eq!(status, log.expected_status(tid));
        assert!(failure.cause().is_none());
        assert!(!failure.run_failed());
        assert!(log.consumed.contains(&tid));
        assert!(log.processes.contains(&tid));
        assert!(log.retired.contains(&tid));
        assert!(!log.root_success);
        log.root_success = true;
        if log.case == Case::LateChildError {
            assert_eq!(log.roles.len(), 3);
            assert_eq!(log.terminal.len(), 1);
            assert_eq!(log.counts.as_ref().unwrap().0.load(Ordering::SeqCst), 2);
            // A wake cannot poll the child until this synchronous root poll has
            // returned Ok to run_task_tree. No timer or host delay defines it.
            log.error_tx.take().unwrap().send(()).unwrap();
        }
    }
    #[derive(Default)]
    struct Observer;
    #[reverie::tool]
    impl Tool for Observer {
        type GlobalState = Global;
        type ThreadState = State;
        fn subscriptions(_: &()) -> Subscription {
            let mut sub = Subscription::none();
            sub.syscall(Sysno::getuid);
            sub
        }
        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            call: Syscall,
        ) -> Result<i64, reverie::Error> {
            assert!(matches!(call, Syscall::Getuid(_)));
            let (_, args) = call.into_parts();
            let role = args.arg0;
            let log = scenario().unwrap();
            let case = {
                let mut log = log.lock().unwrap();
                assert!(log.roles.insert(role, guest.tid()).is_none());
                if role != 0 && log.roles.contains_key(&1) && log.roles.contains_key(&2) {
                    log.ready_tx.take().unwrap().send(()).unwrap();
                }
                log.case
            };
            if role == 0 {
                let ready = log.lock().unwrap().ready_rx.take().unwrap();
                ready
                    .await
                    .expect("both real children reached their pending callbacks");
                {
                    let log = log.lock().unwrap();
                    assert_eq!(log.root, Some(guest.tid()));
                    assert_eq!(log.owners.len(), 3);
                    assert!(log.terminal.is_empty());
                    assert!(
                        log.owners
                            .values()
                            .all(|owner| owner.observed_terminal().is_none())
                    );
                }
                return match case {
                    Case::RootError => {
                        Err(reverie::Error::Tool(anyhow::Error::new(OriginalFailure)))
                    }
                    Case::LateChildError | Case::CleanupError => Ok(0),
                    Case::ConsumedFinal => {
                        panic!("natural-final fixture executed an unexpected syscall")
                    }
                };
            }
            if role == 1 && case == Case::LateChildError {
                let wait = log.lock().unwrap().error_rx.take().unwrap();
                wait.await
                    .expect("root completed its actual successful run before late child failure");
                {
                    let log = log.lock().unwrap();
                    let root = log.root.unwrap();
                    assert!(log.root_success);
                    assert!(log.retired.contains(&root));
                    assert_eq!(log.terminal.get(&root), Some(&ExitStatus::Exited(17)));
                    assert_eq!(log.terminal.len(), 1);
                    for (tid, owner) in &log.owners {
                        if *tid != root {
                            assert!(owner.observed_terminal().is_none());
                        }
                    }
                }
                return Err(reverie::Error::Tool(anyhow::Error::new(OriginalFailure)));
            }
            assert!(role == 1 || role == 2);
            future::pending().await
        }
        fn on_backend_thread_terminal(
            &self,
            tid: Pid,
            _: &Global,
            state: &mut State,
            status: ExitStatus,
        ) {
            assert!(!state.terminal);
            state.terminal = true;
            let log = scenario().unwrap();
            let mut log = log.lock().unwrap();
            assert_eq!(status, log.expected_status(tid));
            assert!(
                matches!(log.owners[&tid].observed_terminal(), Some(Ok(actual)) if actual == status)
            );
            assert!(log.terminal.insert(tid, status).is_none());
        }
        async fn on_exit_thread<G: GlobalRPC<Global>>(
            &self,
            tid: Pid,
            _: &G,
            state: State,
            status: ExitStatus,
        ) -> Result<(), reverie::Error> {
            assert!(state.terminal);
            let log = scenario().unwrap();
            let mut log = log.lock().unwrap();
            assert_eq!(log.terminal.get(&tid), Some(&status));
            assert!(log.consumed.insert(tid));
            if Some(tid) == log.root {
                match log.case {
                    Case::RootError => return Err(Errno::EIO.into()), // secondary to original run error
                    Case::CleanupError => {
                        return Err(reverie::Error::Tool(anyhow::Error::new(OriginalFailure)));
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        async fn on_exit_process<G: GlobalRPC<Global>>(
            self,
            pid: Pid,
            _: &G,
            status: ExitStatus,
        ) -> Result<(), reverie::Error> {
            let log = scenario().unwrap();
            let wait = {
                let mut log = log.lock().unwrap();
                if Some(pid) == log.root && log.case == Case::CleanupError {
                    assert_eq!(log.terminal.get(&pid), Some(&ExitStatus::Exited(17)));
                    assert!(log.consumed.contains(&pid));
                    Some(log.cleanup_rx.take().unwrap())
                } else {
                    None
                }
            };
            if let Some(wait) = wait {
                wait.await.expect(
                    "failure fence released both pending children before later callback wait",
                );
                let mut log = log.lock().unwrap();
                assert_eq!(log.retired.len(), 2);
                assert_eq!(log.terminal.len(), 3);
                assert!(!log.retired.contains(&pid));
                assert!(!log.cleanup_waited);
                log.cleanup_waited = true;
            }
            let mut log = log.lock().unwrap();
            assert_eq!(log.terminal.get(&pid), Some(&status));
            assert!(log.consumed.contains(&pid));
            assert!(log.processes.insert(pid));
            Ok(())
        }
    }
    fn run(case: Case) -> Arc<StdMutex<Scenario>> {
        let log = Arc::new(StdMutex::new(Scenario::new(case)));
        SCENARIO.with(|s| assert!(s.borrow_mut().replace(Arc::clone(&log)).is_none()));
        let _active = ActiveScenario;
        let result = crate::testing::test_fn::<Observer, _>(move || unsafe {
            if case == Case::ConsumedFinal {
                libc::_exit(23);
            }
            for role in [1usize, 2] {
                let child = libc::fork();
                assert!(child >= 0);
                if child == 0 {
                    libc::syscall(libc::SYS_getuid, role, 0, 0, 0, 0, 0);
                    panic!("failed or cancelled child returned to guest");
                }
            }
            assert_eq!(libc::syscall(libc::SYS_getuid, 0, 0, 0, 0, 0, 0), 0);
            assert!(matches!(case, Case::LateChildError | Case::CleanupError));
            libc::_exit(17);
        });
        match (case, result) {
            (Case::ConsumedFinal, Ok((output, _))) => {
                assert_eq!(output.status, ExitStatus::Exited(23))
            }
            (
                Case::RootError | Case::LateChildError | Case::CleanupError,
                Err(reverie::Error::Tool(error)),
            ) => assert!(error.is::<OriginalFailure>(), "original type lost: {error}"),
            _ => panic!("tree returned neither exact native success nor original typed error"),
        }
        {
            let log = log.lock().unwrap();
            let count = log.expected_count();
            assert_eq!(log.owners.len(), count);
            assert_eq!(log.terminal.len(), count);
            assert_eq!(log.consumed.len(), count);
            assert_eq!(log.processes.len(), count);
            assert_eq!(log.retired.len(), count);
            assert_eq!(
                log.root_success,
                matches!(case, Case::LateChildError | Case::ConsumedFinal)
            );
            assert_eq!(log.cleanup_waited, case == Case::CleanupError);
            let (tasks, daemons) = log.counts.as_ref().unwrap();
            assert_eq!(tasks.load(Ordering::SeqCst), 0);
            assert_eq!(daemons.load(Ordering::SeqCst), 0);
            for (tid, owner) in &log.owners {
                assert!(
                    owner.wait(Duration::from_secs(1)),
                    "original notifier did not drain {tid}"
                );
                assert!(
                    matches!(owner.observed_terminal(), Some(Ok(actual)) if actual == log.expected_status(*tid))
                );
                assert_eq!(unsafe { libc::kill(tid.as_raw(), 0) }, -1);
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
                assert_eq!(
                    unsafe { libc::waitpid(tid.as_raw(), std::ptr::null_mut(), libc::WNOHANG) },
                    -1
                );
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ECHILD)
                );
            }
        }
        log
    }
    #[test]
    fn ordinary_root_error_drains_two_pending_process_children_with_default_failure_hooks() {
        let Some(receipt) = isolated_real_parent(
            "task::run_error_tests::additional::ordinary_root_error_drains_two_pending_process_children_with_default_failure_hooks",
        ) else {
            return;
        };
        run(Case::RootError);
        assert_eq!(receipt.send(&[1]).unwrap(), 1);
    }
    #[test]
    fn late_orphan_error_overrides_completed_root_success_after_owned_drain() {
        let Some(receipt) = isolated_real_parent(
            "task::run_error_tests::additional::late_orphan_error_overrides_completed_root_success_after_owned_drain",
        ) else {
            return;
        };
        run(Case::LateChildError);
        assert_eq!(receipt.send(&[1]).unwrap(), 1);
    }
    #[test]
    fn exit_callback_error_wakes_children_before_later_consuming_callback_waits() {
        let Some(receipt) = isolated_real_parent(
            "task::run_error_tests::additional::exit_callback_error_wakes_children_before_later_consuming_callback_waits",
        ) else {
            return;
        };
        run(Case::CleanupError);
        assert_eq!(receipt.send(&[1]).unwrap(), 1);
    }
    #[test]
    fn already_consumed_actual_final_uses_receipt_without_second_wait_or_numeric_action() {
        let log = run(Case::ConsumedFinal);
        let log = log.lock().unwrap();
        let root = log.root.unwrap();
        let owner = &log.owners[&root];
        let never_wait_again = future::poll_fn(|_| -> Poll<Result<Stopped, TraceError>> {
            panic!("attempted a second wait after the actual final status was consumed")
        });
        match futures::executor::block_on(wait_for_failed_run_terminal(
            owner,
            root,
            never_wait_again,
        )) {
            Either::Right(Ok(status)) => assert_eq!(status, ExitStatus::Exited(23)),
            _ => panic!("actual natural final receipt was replaced or lost"),
        }
        assert!(owner.wait(Duration::from_secs(1)));
        assert!(matches!(
            owner.observed_terminal(),
            Some(Ok(ExitStatus::Exited(23)))
        ));
    }
    #[test]
    fn common_failure_after_actual_final_and_pending_gdb_send_preserves_exact_receipt() {
        let log = run(Case::ConsumedFinal);
        let (tid, owner, expired_exit) = {
            let mut log = log.lock().unwrap();
            let tid = log.root.unwrap();
            (
                tid,
                log.owners.remove(&tid).unwrap(),
                log.exit_waiters.remove(&tid).unwrap(),
            )
        };
        assert!(matches!(
            owner.observed_terminal(),
            Some(Ok(ExitStatus::Exited(23)))
        ));
        let (stop_tx, mut stop_rx) = mpsc::channel(1);
        let (request_tx, _request_rx) = mpsc::channel(1);
        let (resume_tx, _resume_rx) = mpsc::channel(1);
        let notification = || StoppedInferior {
            reason: StopReason::Exited(tid, ExitStatus::Exited(23)),
            request_tx: request_tx.clone(),
            resume_tx: resume_tx.clone(),
        };
        stop_tx
            .try_send(notification())
            .expect("fill actual bounded GDB notification channel");
        let blocked_send = stop_tx.send(notification()).fuse();
        futures::pin_mut!(blocked_send);
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(blocked_send.as_mut().poll(&mut cx).is_pending());

        let failure = ParentCompletionFailure::default();
        let global = Global;
        let failed = wait_for_task_failure(&global, &failure).fuse();
        futures::pin_mut!(failed);
        assert!(failed.as_mut().poll(&mut cx).is_pending());
        publish_run_error(
            &failure,
            &global,
            tid,
            tid,
            "component failure while actual final notification is pending",
            reverie::Error::Tool(anyhow::Error::new(OriginalFailure)),
        );
        assert!(blocked_send.as_mut().poll(&mut cx).is_pending());

        let polled_exit = std::cell::Cell::new(false);
        let original_exit = async {
            polled_exit.set(true);
            expired_exit.await
        };
        let completion = futures::executor::block_on(async {
            futures::select_biased! {
                _ = failed => wait_for_failed_run_terminal(&owner, tid, original_exit).await,
                _ = blocked_send => panic!("full notification unexpectedly completed before failure join"),
            }
        });
        assert!(
            !polled_exit.get(),
            "common failure polled the expired original EXIT capability"
        );
        assert!(matches!(
            completion,
            Either::Right(Ok(ExitStatus::Exited(23)))
        ));
        let (_, error) = failure.take_run_error().unwrap();
        assert!(matches!(error, reverie::Error::Tool(error) if error.is::<OriginalFailure>()));
        assert!(failure.run_failed());
        let first = stop_rx.try_recv().unwrap();
        assert!(
            matches!(first.reason, StopReason::Exited(actual, ExitStatus::Exited(23)) if actual == tid)
        );
        assert!(owner.wait(Duration::from_secs(1)));
        assert!(matches!(
            owner.observed_terminal(),
            Some(Ok(ExitStatus::Exited(23)))
        ));
    }
    #[test]
    fn original_run_error_precedes_cleanup_and_native_parent_failure_keeps_its_kind() {
        let error = preserve_run_error(
            Some(reverie::Error::Tool(anyhow::Error::new(OriginalFailure))),
            Errno::EIO.into(),
        );
        assert!(matches!(error, reverie::Error::Tool(error) if error.is::<OriginalFailure>()));
        assert!(matches!(
            preserve_run_error(None, Errno::EIO.into()),
            reverie::Error::Errno(Errno::EIO)
        ));
        let failure = ParentCompletionFailure::default();
        let native = TraceError::Errno(Errno::EPROTO);
        failure.record(Pid::from_raw(41), &native);
        failure.record_run_error(
            Pid::from_raw(42),
            reverie::Error::Tool(anyhow::Error::new(OriginalFailure)),
        );
        assert_eq!(
            failure.cause(),
            Some((Pid::from_raw(41), native.to_string()))
        );
        assert!(!failure.run_failed());
        assert!(failure.take_run_error().is_none());
        assert!(failure.wait().now_or_never().is_some());
        let reverse = ParentCompletionFailure::default();
        reverse.record_run_error(
            Pid::from_raw(42),
            reverie::Error::Tool(anyhow::Error::new(OriginalFailure)),
        );
        reverse.record(Pid::from_raw(41), &native);
        assert!(reverse.cause().is_none());
        assert!(reverse.run_failed());
        assert!(
            matches!(reverse.take_run_error(), Some((_, reverie::Error::Tool(error))) if error.is::<OriginalFailure>())
        );
        assert!(reverse.run_failed());
        assert!(reverse.wait().now_or_never().is_some());
    }
}

// Real backend controls for the separate real-parent outcome. No fixture reap
// or host subreaper/disposition change supplies these results.
mod parent_join_tests {
    use safeptrace::ParentReap;

    use super::*;

    #[derive(Default)]
    struct Log {
        root: Option<Pid>,
        owners: BTreeMap<Pid, Arc<TerminalCleanup>>,
        terminal: BTreeMap<Pid, ExitStatus>,
        child_done_tx: Option<oneshot::Sender<()>>,
        child_done_rx: Option<oneshot::Receiver<()>>,
        other_parent: usize,
        guest_reaped: usize,
    }
    thread_local! {
        static LOG: RefCell<Option<Arc<StdMutex<Log>>>> = const { RefCell::new(None) };
    }
    fn log() -> Option<Arc<StdMutex<Log>>> {
        LOG.with(|s| s.borrow().clone())
    }
    struct Active;
    impl Drop for Active {
        fn drop(&mut self) {
            LOG.with(|s| assert!(s.borrow_mut().take().is_some()));
        }
    }
    pub(super) fn retained(task: &Stopped) {
        let Some(log) = log() else { return };
        let mut log = log.lock().unwrap();
        if log.root.is_none() {
            log.root = Some(task.pid());
        }
        assert!(
            log.owners
                .insert(task.pid(), Arc::new(task.terminal_cleanup()))
                .is_none()
        );
    }
    fn begin() -> (Arc<StdMutex<Log>>, Active) {
        let (tx, rx) = oneshot::channel();
        let log = Arc::new(StdMutex::new(Log {
            child_done_tx: Some(tx),
            child_done_rx: Some(rx),
            ..Log::default()
        }));
        LOG.with(|s| assert!(s.borrow_mut().replace(Arc::clone(&log)).is_none()));
        (log, Active)
    }
    #[derive(Default)]
    struct Observer;
    #[reverie::tool]
    impl Tool for Observer {
        type GlobalState = super::Global;
        type ThreadState = ();
        fn subscriptions(_: &()) -> Subscription {
            let mut s = Subscription::none();
            s.syscall(Sysno::getuid);
            s
        }
        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            call: Syscall,
        ) -> Result<i64, reverie::Error> {
            assert!(matches!(call, Syscall::Getuid(_)));
            let (_, args) = call.into_parts();
            let log = log().unwrap();
            let child = Pid::from_raw(args.arg1 as i32);
            assert_eq!(log.lock().unwrap().root, Some(guest.tid()));
            match args.arg0 {
                101 => {
                    let root_owner = Arc::clone(&log.lock().unwrap().owners[&guest.tid()]);
                    assert!(root_owner.observed_terminal().is_none());
                    assert_eq!(root_owner.reap_parent_terminal().await, Err(Errno::EBUSY));
                    let wait = log.lock().unwrap().child_done_rx.take().unwrap();
                    wait.await.expect("actual child terminal callback");
                    let owner = Arc::clone(&log.lock().unwrap().owners[&child]);
                    assert!(matches!(
                        owner.observed_terminal(),
                        Some(Ok(ExitStatus::Exited(37)))
                    ));
                    assert!(!owner.is_reaped().unwrap());
                    assert_eq!(
                        owner.reap_parent_terminal().await.unwrap(),
                        ParentReap::OtherParent
                    );
                    assert!(
                        !owner.is_reaped().unwrap(),
                        "tracer stole live guest parent's wait"
                    );
                    log.lock().unwrap().other_parent += 1;
                }
                102 => {
                    let owner = Arc::clone(&log.lock().unwrap().owners[&child]);
                    assert!(owner.is_reaped().unwrap());
                    assert_eq!(
                        owner.reap_parent_terminal().await.unwrap(),
                        ParentReap::AlreadyReaped
                    );
                    log.lock().unwrap().guest_reaped += 1;
                }
                other => panic!("unexpected parent-wait control {other}"),
            }
            Ok(7)
        }
        fn on_backend_thread_terminal(
            &self,
            tid: Pid,
            _: &super::Global,
            _: &mut (),
            status: ExitStatus,
        ) {
            let log = log().unwrap();
            let mut log = log.lock().unwrap();
            assert!(
                matches!(log.owners[&tid].observed_terminal(), Some(Ok(actual)) if actual == status)
            );
            assert!(log.terminal.insert(tid, status).is_none());
            if log.root != Some(tid)
                && let Some(done) = log.child_done_tx.take()
            {
                done.send(()).unwrap();
            }
        }
    }
    #[test]
    fn parent_join_preserves_live_guest_wait_and_distinguishes_already_reaped() {
        let (log, _active) = begin();
        let (output, _) = crate::testing::test_fn::<Observer, _>(|| unsafe {
            let child = libc::fork();
            assert!(child >= 0);
            if child == 0 {
                libc::_exit(37);
            }
            assert_eq!(
                libc::syscall(libc::SYS_getuid, 101usize, child, 0, 0, 0, 0),
                7
            );
            let mut status = 0;
            assert_eq!(libc::waitpid(child, &mut status, 0), child);
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 37);
            assert_eq!(
                libc::syscall(libc::SYS_getuid, 102usize, child, 0, 0, 0, 0),
                7
            );
        })
        .expect("actual guest parent retains its native child wait");
        assert_eq!(output.status, ExitStatus::Exited(0));
        let log = log.lock().unwrap();
        assert_eq!(
            (
                log.other_parent,
                log.guest_reaped,
                log.owners.len(),
                log.terminal.len()
            ),
            (1, 1, 2, 2)
        );
        for (tid, owner) in &log.owners {
            assert!(owner.wait(Duration::from_secs(1)));
            assert!(owner.is_reaped().unwrap());
            assert_eq!(
                futures::executor::block_on(owner.reap_parent_terminal()).unwrap(),
                ParentReap::AlreadyReaped
            );
            assert_eq!(unsafe { libc::kill(tid.as_raw(), 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            assert_eq!(
                unsafe { libc::waitpid(tid.as_raw(), std::ptr::null_mut(), libc::WNOHANG) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
    }
    #[test]
    fn parent_join_refuses_actual_nonleader_terminal_without_a_process_wait() {
        let (log, _active) = begin();
        let (output, _) = crate::testing::test_fn::<Observer, _>(|| {
            std::thread::spawn(|| {}).join().unwrap();
        })
        .expect("actual native thread joins normally");
        assert_eq!(output.status, ExitStatus::Exited(0));
        let log = log.lock().unwrap();
        assert_eq!((log.owners.len(), log.terminal.len()), (2, 2));
        let root = log.root.unwrap();
        let (&tid, owner) = log.owners.iter().find(|(tid, _)| **tid != root).unwrap();
        assert_eq!(owner.thread_group_id().unwrap(), root);
        assert!(owner.wait(Duration::from_secs(1)));
        assert!(owner.is_reaped().unwrap());
        assert_eq!(
            futures::executor::block_on(owner.reap_parent_terminal()),
            Err(Errno::EINVAL)
        );
        assert_eq!(unsafe { libc::kill(tid.as_raw(), 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert_eq!(
            unsafe { libc::waitpid(tid.as_raw(), std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}

#[cfg(target_arch = "x86_64")]
mod injected_trap;
