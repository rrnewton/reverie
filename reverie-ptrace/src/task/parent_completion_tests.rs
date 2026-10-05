/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Real creator death in the new parent-completion ptrace windows.
//! The stimulus uses the original PIDFD; the deferred death must be the
//! kernel's own report, never a supplied errno or wait: an actual safeptrace
//! Died from a kernel operation (ESRCH, or the PTRACE_EVENT_EXIT siginfo of
//! the held creator stop), or the creator's final status returned by the
//! completion path's own wait after its resume consumed that EXIT stop.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::time::Duration;

use super::*;
use crate::testing::test_fn_with_config;

/// The parent-completion step before which the test kills the creator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Window {
    /// Before the held child-creation stop is checked and resumed.
    Observation,
    /// Between the pre-resume death check and PTRACE_SYSCALL, with the
    /// creator already parked in PTRACE_EVENT_EXIT when PTRACE_SYSCALL runs.
    Resume,
    /// After the creator's syscall-exit stop, before its result is read, with
    /// the creator already parked in PTRACE_EVENT_EXIT when the reads run.
    Receipt,
    /// After the syscall-exit receipt, before the saved frame is restored.
    Restoration,
}
struct Log {
    window: Window,
    creator: Option<Pid>,
    child: Option<Pid>,
    creator_cleanup: Option<TerminalCleanup>,
    child_worker_drained: usize,
    child_events: usize,
    inherited: usize,
    retained: usize,
    boundary: usize,
    kills: usize,
    died: usize,
    receipts: usize,
    child_starts: usize,
    terminal: BTreeSet<Pid>,
    consumed: BTreeSet<Pid>,
    process_consumed: BTreeSet<Pid>,
    retired: BTreeSet<Pid>,
    counts: Option<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
    release_child: Option<oneshot::Sender<()>>,
    child_wait: Option<oneshot::Receiver<()>>,
}
impl Log {
    fn new(window: Window) -> Self {
        let (release_child, child_wait) = oneshot::channel();
        Self {
            window,
            creator: None,
            child: None,
            creator_cleanup: None,
            child_worker_drained: 0,
            child_events: 0,
            inherited: 0,
            retained: 0,
            boundary: 0,
            kills: 0,
            died: 0,
            receipts: 0,
            child_starts: 0,
            terminal: BTreeSet::new(),
            consumed: BTreeSet::new(),
            process_consumed: BTreeSet::new(),
            retired: BTreeSet::new(),
            counts: None,
            release_child: Some(release_child),
            child_wait: Some(child_wait),
        }
    }
    fn expected_receipts(&self) -> usize {
        usize::from(self.window == Window::Restoration)
    }
    fn expected_evidence(&self) -> CreatorDeathEvidence {
        match self.window {
            Window::Observation | Window::Receipt | Window::Restoration => {
                CreatorDeathEvidence::Died
            }
            Window::Resume => {
                CreatorDeathEvidence::FinalWait(ExitStatus::Signaled(Signal::SIGKILL, false))
            }
        }
    }
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

pub(super) fn retained(
    native: &NativeChild,
    creator_cleanup: TerminalCleanup,
    tasks: &Arc<AtomicUsize>,
    daemons: &Arc<AtomicUsize>,
) {
    let Some(log) = active() else { return };
    let mut state = log.lock().unwrap();
    assert_eq!(native.kind, ChildTaskKind::Process);
    assert_eq!(state.creator, Some(native.creator));
    assert_eq!(state.child, Some(native.id));
    assert_eq!(state.child_events, 1);
    assert!(native.child_restore_context.is_some());
    assert_eq!(native.creator_cleanup.thread_group_id(), Ok(native.creator));
    assert_eq!(native.cleanup.thread_group_id(), Ok(native.id));
    assert_eq!(state.retained, 0);
    state.retained += 1;
    // Retain the same tokens using their existing bound cleanup descriptions.
    // No numeric lookup and no additional wait or EXIT claimant is introduced.
    assert!(creator_cleanup.same_generation(&native.creator_cleanup));
    state.creator_cleanup = Some(creator_cleanup);
    state.counts = Some((Arc::clone(tasks), Arc::clone(daemons)));
}

fn before_boundary(parent: &Stopped, window: Window) {
    let Some(log) = active() else { return };
    let mut state = log.lock().unwrap();
    if state.window != window {
        return;
    }
    assert_eq!(Some(parent.pid()), state.creator);
    let cleanup = state.creator_cleanup.as_ref().unwrap();
    assert!(cleanup.same_generation(&parent.terminal_cleanup()));
    assert!(cleanup.observed_terminal().is_none());
    assert_eq!(state.retained, 1);
    assert_eq!(
        state.inherited, 1,
        "child construction and publication precede hook"
    );
    assert_eq!(state.receipts, state.expected_receipts());
    assert_eq!(state.boundary, 0);
    assert_eq!(state.kills, 0);
    // Production must report the kernel's own evidence of this death: a
    // request on the held stop fails with ESRCH (Died), or succeeds because
    // the creator is already in PTRACE_EVENT_EXIT, which its post-request
    // siginfo check reports as Died. Do not consume EXIT here: the original
    // run-owned future remains sole owner.
    cleanup
        .terminate_bound_task()
        .expect("signal original creator PIDFD");
    state.boundary += 1;
    state.kills += 1;
    if matches!(window, Window::Resume | Window::Receipt) {
        // Make the next request run with the creator in its EXIT stop, where
        // it succeeds. This probe only reads siginfo; it neither waits nor
        // resumes.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match parent.getsiginfo() {
                Ok(siginfo)
                    if siginfo.si_signo == libc::SIGTRAP
                        && siginfo.si_code == libc::SIGTRAP | (libc::PTRACE_EVENT_EXIT << 8) =>
                {
                    break;
                }
                Ok(siginfo) => assert_eq!(
                    siginfo.si_code,
                    if window == Window::Resume {
                        libc::SIGTRAP | (libc::PTRACE_EVENT_FORK << 8)
                    } else {
                        libc::SIGTRAP | 0x80
                    },
                    "creator still in its held stop or its EXIT stop"
                ),
                Err(TraceError::Died(_)) => {}
                Err(error) => panic!("siginfo probe of killed creator: {error:?}"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "SIGKILLed creator reached PTRACE_EVENT_EXIT"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

pub(super) fn before_observation(parent: &Stopped, has_saved_context: bool) {
    if active().is_some() {
        assert!(
            has_saved_context,
            "different pending syscall uses private frame"
        );
    }
    before_boundary(parent, Window::Observation);
}
pub(super) fn before_resume(parent: &Stopped) {
    before_boundary(parent, Window::Resume);
}
pub(super) fn before_receipt(parent: &Stopped) {
    before_boundary(parent, Window::Receipt);
}
pub(super) fn before_restoration(parent: &Stopped) {
    before_boundary(parent, Window::Restoration);
}
pub(super) fn deferred_death(
    creator: Pid,
    owner: &TerminalCleanup,
    evidence: CreatorDeathEvidence,
) {
    let Some(log) = active() else { return };
    let mut state = log.lock().unwrap();
    assert_eq!(state.creator, Some(creator));
    assert!(
        state
            .creator_cleanup
            .as_ref()
            .unwrap()
            .same_generation(owner)
    );
    assert_eq!(state.kills, 1);
    assert_eq!(state.receipts, state.expected_receipts());
    assert_eq!(evidence, state.expected_evidence());
    assert_eq!(state.died, 0);
    state.died += 1;
}
pub(super) fn child_worker_drained(id: Pid, cleanup: &TerminalCleanup) {
    let Some(log) = active() else { return };
    let mut state = log.lock().unwrap();
    assert_eq!(state.child, Some(id));
    assert!(matches!(
        cleanup.observed_terminal(),
        Some(Ok(ExitStatus::Exited(37)))
    ));
    assert!(
        cleanup.wait(Duration::from_secs(1)),
        "original child notifier drained"
    );
    assert_eq!(state.child_worker_drained, 0);
    state.child_worker_drained += 1;
}
pub(super) fn retired(tid: Pid, tasks: &Arc<AtomicUsize>, daemons: &Arc<AtomicUsize>) {
    let Some(log) = active() else { return };
    let mut state = log.lock().unwrap();
    assert!(state.terminal.contains(&tid));
    assert!(state.consumed.contains(&tid));
    assert!(state.process_consumed.contains(&tid));
    assert!(state.retired.insert(tid));
    let (saved_tasks, saved_daemons) = state.counts.as_ref().unwrap();
    assert!(Arc::ptr_eq(tasks, saved_tasks));
    assert!(Arc::ptr_eq(daemons, saved_daemons));
    assert_eq!(tasks.load(Ordering::SeqCst), 2 - state.retired.len());
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
    // Keep ordinary default no-op failure hooks: backend custody must suffice.
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct State {
    child: Option<i32>,
    terminal: bool,
}
#[derive(Default)]
struct Observer;
#[reverie::tool]
impl Tool for Observer {
    type GlobalState = Global;
    type ThreadState = State;
    fn subscriptions(_: &()) -> Subscription {
        let mut selected = Subscription::none();
        selected.syscall(Sysno::getuid);
        selected
    }
    fn observe_injected_syscalls(_: &()) -> bool {
        true
    }
    fn init_thread_state(&self, tid: Pid, parent: Option<(Pid, &State)>) -> State {
        if let Some((creator, inherited)) = parent {
            let log = active().unwrap();
            let mut state = log.lock().unwrap();
            assert_eq!(state.creator, Some(creator));
            assert_eq!(state.child, Some(tid));
            assert_eq!(inherited.child, Some(tid.as_raw()));
            assert!(!inherited.terminal);
            assert_eq!(state.inherited, 0);
            state.inherited += 1;
        }
        State::default()
    }
    fn on_injected_syscall_observed(
        &self,
        tid: Pid,
        _: &Global,
        thread: &mut State,
        nr: Sysno,
        _: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        let log = active().unwrap();
        let mut state = log.lock().unwrap();
        match event {
            InjectedSyscallEvent::ChildCreated(child) => {
                assert_eq!(nr, Sysno::fork);
                assert!(state.creator.replace(tid).is_none());
                assert!(state.child.replace(child).is_none());
                assert!(thread.child.replace(child.as_raw()).is_none());
                assert_eq!(state.child_events, 0);
                state.child_events += 1;
            }
            InjectedSyscallEvent::ChildSyscallReturned { child, raw } => {
                assert_eq!(nr, Sysno::fork);
                assert_eq!(state.creator, Some(tid));
                assert_eq!(state.child, Some(child));
                assert_eq!(raw, i64::from(child.as_raw()));
                assert_eq!(state.window, Window::Restoration);
                assert_eq!(state.receipts, 0);
                state.receipts += 1;
            }
            InjectedSyscallEvent::Returned(_) if nr == Sysno::fork => {
                panic!("ChildCreated remains distinct from generic Returned");
            }
            _ => {}
        }
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        assert!(matches!(syscall, Syscall::Getuid(_)));
        let _raw = guest.inject(reverie::syscalls::Fork::new()).await?;
        panic!("killed creator must not resume its borrowed Tool handler");
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        let log = active().unwrap();
        let wait = {
            let mut state = log.lock().unwrap();
            if state.child != Some(guest.tid()) {
                return Ok(());
            }
            assert_eq!(guest.pid(), guest.tid());
            assert_eq!(state.inherited, 1);
            assert_eq!(state.child_starts, 0);
            state.child_starts += 1;
            state.child_wait.take().unwrap()
        };
        // The child's real write/exit occurs after the creator's real final wait.
        // A mistaken broadcast failure would cancel this actor and fail its
        // exact Exited(37), output, and consuming-cleanup assertions.
        wait.await
            .expect("creator actual terminal callback releases child");
        Ok(())
    }
    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        _: &Global,
        thread: &mut State,
        status: ExitStatus,
    ) {
        assert!(!thread.terminal);
        thread.terminal = true;
        let log = active().unwrap();
        let mut state = log.lock().unwrap();
        assert!(state.terminal.insert(tid));
        if state.creator == Some(tid) {
            assert_eq!(status, ExitStatus::Signaled(Signal::SIGKILL, false));
            assert_eq!(
                state.died, 1,
                "actual generation-bound kernel death evidence was deferred"
            );
            assert_eq!(state.receipts, state.expected_receipts());
            assert_eq!(state.terminal.len(), 1);
            state.release_child.take().unwrap().send(()).unwrap();
        } else {
            assert_eq!(state.child, Some(tid));
            assert_eq!(status, ExitStatus::Exited(37));
            assert_eq!(state.terminal.len(), 2);
            assert_eq!(state.child_starts, 1);
        }
    }
    async fn on_exit_thread<G: GlobalRPC<Global>>(
        &self,
        tid: Pid,
        _: &G,
        thread: State,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert!(thread.terminal);
        let log = active().unwrap();
        let mut state = log.lock().unwrap();
        assert!(state.terminal.contains(&tid));
        assert!(state.consumed.insert(tid));
        assert_eq!(
            status,
            if state.creator == Some(tid) {
                ExitStatus::Signaled(Signal::SIGKILL, false)
            } else {
                assert_eq!(state.child, Some(tid));
                ExitStatus::Exited(37)
            }
        );
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Global>>(
        self,
        pid: Pid,
        _: &G,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        let log = active().unwrap();
        let mut state = log.lock().unwrap();
        assert!(state.terminal.contains(&pid));
        assert!(state.consumed.contains(&pid));
        assert!(state.process_consumed.insert(pid));
        assert_eq!(
            status,
            if state.creator == Some(pid) {
                ExitStatus::Signaled(Signal::SIGKILL, false)
            } else {
                assert_eq!(state.child, Some(pid));
                ExitStatus::Exited(37)
            }
        );
        Ok(())
    }
}

fn run_case(window: Window) {
    const MARKER: &[u8] = b"parent died; published child completed naturally\n";
    let log = Arc::new(StdMutex::new(Log::new(window)));
    ACTIVE.with(|slot| assert!(slot.borrow_mut().replace(Arc::clone(&log)).is_none()));
    let _active = Active;
    let (output, _) = test_fn_with_config::<Observer, _>(
        || unsafe {
            libc::alarm(5);
            // The Tool privately injects fork at this getuid. Child restoration
            // returns zero here; the parent is killed at the selected real boundary.
            let result = libc::syscall(libc::SYS_getuid);
            if result != 0 {
                libc::_exit(90)
            }
            libc::alarm(5);
            if libc::write(1, MARKER.as_ptr().cast(), MARKER.len()) != MARKER.len() as isize {
                libc::_exit(91)
            }
            libc::_exit(37)
        },
        (),
        true,
    )
    .expect("natural creator death must not become backend tree failure");
    assert_eq!(output.status, ExitStatus::Signaled(Signal::SIGKILL, false));
    assert_eq!(output.stdout, MARKER);
    let state = log.lock().unwrap();
    assert_eq!(state.child_events, 1);
    assert_eq!(state.inherited, 1);
    assert_eq!(state.retained, 1);
    assert_eq!(state.boundary, 1);
    assert_eq!(state.kills, 1);
    assert_eq!(state.died, 1);
    assert_eq!(state.receipts, state.expected_receipts());
    assert_eq!(state.child_starts, 1);
    assert_eq!(state.terminal.len(), 2);
    assert_eq!(state.consumed.len(), 2);
    assert_eq!(state.process_consumed.len(), 2);
    assert_eq!(state.retired.len(), 2);
    let (tasks, daemons) = state.counts.as_ref().unwrap();
    assert_eq!(tasks.load(Ordering::SeqCst), 0);
    assert_eq!(daemons.load(Ordering::SeqCst), 0);
    let cleanup = state.creator_cleanup.as_ref().unwrap();
    assert!(matches!(
        cleanup.observed_terminal(),
        Some(Ok(ExitStatus::Signaled(Signal::SIGKILL, false)))
    ));
    assert!(
        cleanup.wait(Duration::from_secs(1)),
        "original creator notifier drained"
    );
    assert_eq!(state.child_worker_drained, 1);
    println!(
        "parent-completion-death window={window:?} creator={} child={} actual-died=1 receipts={} final=2 consuming=2 process=2 retired=2 child-natural37=1 original-workers-drained=2",
        state.creator.unwrap(),
        state.child.unwrap(),
        state.receipts
    );
}
#[test]
fn actual_creator_death_before_parent_observation_preserves_published_child() {
    run_case(Window::Observation);
}
#[test]
fn actual_creator_death_before_parent_resume_preserves_published_child() {
    run_case(Window::Resume);
}
#[test]
fn actual_creator_death_before_parent_receipt_preserves_published_child() {
    run_case(Window::Receipt);
}
#[test]
fn actual_creator_death_after_parent_receipt_before_restore_preserves_published_child() {
    run_case(Window::Restoration);
}
