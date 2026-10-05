/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Controls use the ordinary native Tool runner and original notifier tokens.
//! The delivery-gap cases delay one real SIGSTOP result by one Pending poll;
//! they do not claim to measure the kernel's publication order.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::time::Duration;
use std::time::Instant;

use super::*;
use crate::testing::test_fn_with_config;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Case {
    BothReady,
    DeliveryGap,
    CancelAfterClaim,
    TimerDeath,
    RestoreDeath,
    CompetingClaim,
    RevokedClaim,
    UnexpectedStop,
    StartupCompetingClaim,
    StartupRevokedClaim,
    Daemon,
    DaemonPending,
    LeaderExit,
}

impl Case {
    fn group(self) -> bool {
        matches!(self, Self::Daemon | Self::DaemonPending | Self::LeaderExit)
    }
    fn daemon(self) -> bool {
        matches!(self, Self::Daemon | Self::DaemonPending)
    }
}
const THREAD_MARKER: &[u8] = b"live child before leader final\n";

type ExitWait = Pin<Box<dyn Future<Output = Result<Stopped, TraceError>> + Send + Sync>>;

struct Log {
    case: Case,
    root: Option<Pid>,
    creator: Option<Pid>,
    target: Option<Pid>,
    creator_cleanup: Option<Arc<TerminalCleanup>>,
    target_cleanup: Option<Arc<TerminalCleanup>>,
    counts: Option<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
    constructed: BTreeSet<Pid>,
    retired: BTreeSet<Pid>,
    terminal: BTreeSet<Pid>,
    consumed: BTreeSet<Pid>,
    inherited: BTreeSet<Pid>,
    target_starts: usize,
    target_writes: usize,
    creator_resumes: usize,
    creator_exit_joins: usize,
    preparation_entries: usize,
    held_delivery: usize,
    exit_claims: usize,
    exit_resumes: usize,
    stale_discarded: usize,
    claim_pause_pending: usize,
    claim_pause_completed: usize,
    release_claim: Option<oneshot::Sender<()>>,
    root_release: Option<oneshot::Sender<()>>,
    root_wait: Option<oneshot::Receiver<()>>,
    root_barrier_completed: usize,
    cleanup_claim: Option<Stopped>,
    cleanup_wait: Option<ExitWait>,
    refusal: Option<Errno>,
    timer_attempts: usize,
    timer_esrch: usize,
    restore_entries: usize,
    test_final_waits: usize,
}

impl Log {
    fn new(case: Case) -> Self {
        let (root_release, root_wait) = oneshot::channel();
        Self {
            case,
            root: None,
            creator: None,
            target: None,
            creator_cleanup: None,
            target_cleanup: None,
            counts: None,
            constructed: BTreeSet::new(),
            retired: BTreeSet::new(),
            terminal: BTreeSet::new(),
            consumed: BTreeSet::new(),
            inherited: BTreeSet::new(),
            target_starts: 0,
            target_writes: 0,
            creator_resumes: 0,
            creator_exit_joins: 0,
            preparation_entries: 0,
            held_delivery: 0,
            exit_claims: 0,
            exit_resumes: 0,
            stale_discarded: 0,
            claim_pause_pending: 0,
            claim_pause_completed: 0,
            release_claim: None,
            root_release: Some(root_release),
            root_wait: Some(root_wait),
            root_barrier_completed: 0,
            cleanup_claim: None,
            cleanup_wait: None,
            refusal: None,
            timer_attempts: 0,
            timer_esrch: 0,
            restore_entries: 0,
            test_final_waits: 0,
        }
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<Arc<StdMutex<Log>>>> = const { RefCell::new(None) };
}
fn active() -> Option<Arc<StdMutex<Log>>> {
    ACTIVE.with(|slot| slot.borrow().clone())
}
fn selected(id: Pid) -> Option<(Arc<StdMutex<Log>>, Case)> {
    let log = active()?;
    let state = log.lock().unwrap();
    let selected = state.target == Some(id);
    let case = state.case;
    drop(state);
    selected.then_some((log, case))
}
struct Active;
impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "native fixture publication exceeded one second"
        );
        std::thread::yield_now();
    }
}
fn kill_to_exit(cleanup: &TerminalCleanup) {
    assert!(cleanup.observed_terminal().is_none());
    cleanup
        .terminate_bound_task()
        .expect("signal original held pidfd");
    wait_until(|| cleanup.exit_stop_observed());
    assert!(
        cleanup.observed_terminal().is_none(),
        "EXIT stop is not a final wait"
    );
}

pub(super) fn retained(
    native: &NativeChild,
    creator_cleanup: TerminalCleanup,
    tasks: &Arc<AtomicUsize>,
    daemons: &Arc<AtomicUsize>,
) {
    let Some(log) = active() else { return };
    let mut state = log.lock().unwrap();
    state
        .counts
        .get_or_insert_with(|| (Arc::clone(tasks), Arc::clone(daemons)));
    if state.case.group() && native.kind == ChildTaskKind::Process {
        assert_eq!(tasks.load(Ordering::SeqCst), 1);
        assert_eq!(daemons.load(Ordering::SeqCst), 0);
        return;
    }
    assert!(state.target.replace(native.id).is_none());
    state.creator = Some(native.creator);
    assert!(creator_cleanup.same_generation(&native.creator_cleanup));
    state.creator_cleanup = Some(Arc::new(creator_cleanup));
    assert!(matches!(&native.initial, InitialChildWait::Waiting(_)));
    if state.case == Case::LeaderExit {
        assert!(
            native.child_restore_context.is_none(),
            "ordinary unsubscribed clone context"
        );
    } else {
        assert!(
            native.child_restore_context.is_some(),
            "original injection saved its child context"
        );
    }
    assert_eq!(
        native.creator_cleanup.thread_group_id().unwrap(),
        native.creator
    );
    let case = state.case;
    if case.group() {
        assert_eq!(native.kind, ChildTaskKind::Thread);
        assert_eq!(native.cleanup.thread_group_id().unwrap(), native.creator);
        assert_ne!(state.root, Some(native.creator));
        assert_eq!(tasks.load(Ordering::SeqCst), 2);
        assert_eq!(daemons.load(Ordering::SeqCst), usize::from(case.daemon()));
    } else {
        assert_eq!(native.kind, ChildTaskKind::Process);
        assert_eq!(native.cleanup.thread_group_id().unwrap(), native.id);
    }
    drop(state);
    if matches!(case, Case::BothReady | Case::Daemon) {
        // Genuine queued SIGSTOP and genuine EXIT before the first helper poll.
        wait_until(|| !native.cleanup.pending_is_empty());
        kill_to_exit(&native.cleanup);
        assert!(!native.cleanup.pending_is_empty());
    }
}

pub(super) fn creator_exit_selected(stopped: &Stopped) {
    let Some(log) = active() else { return };
    let mut state = log.lock().unwrap();
    if state.creator != Some(stopped.pid())
        || !matches!(state.case, Case::DaemonPending | Case::LeaderExit)
    {
        return;
    }
    assert!(
        stopped
            .terminal_cleanup()
            .same_generation(state.creator_cleanup.as_ref().unwrap())
    );
    assert_eq!(state.creator_exit_joins, 0);
    state.creator_exit_joins += 1;
    assert_eq!(state.target_starts, 0);
    if state.case == Case::DaemonPending {
        assert_eq!(state.exit_claims, 1);
        assert_eq!(state.claim_pause_pending, 1);
        assert_eq!(
            state.claim_pause_completed, 0,
            "creator selection precedes retained child resume"
        );
        assert_eq!(state.preparation_entries, 1);
    } else {
        assert_eq!(
            stopped
                .getevent()
                .expect("original creator EXIT event message"),
            0,
            "leader-only SYS_exit(0) precedes target execution"
        );
        assert_eq!(state.creator_resumes, 1);
        assert_eq!(state.preparation_entries, 0);
    }
}

pub(super) fn resume_creator_before_child(id: Pid) -> bool {
    let Some(log) = active() else { return false };
    let mut state = log.lock().unwrap();
    if state.case != Case::LeaderExit || state.creator != Some(id) {
        return false;
    }
    assert_eq!(state.creator_resumes, 0);
    assert_eq!(state.target_starts, 0);
    assert!(!state.creator_cleanup.as_ref().unwrap().exit_stop_observed());
    state.creator_resumes += 1;
    true
}

pub(super) async fn prepare_input(child: Running) -> Running {
    let Some((log, case)) = selected(child.pid()) else {
        return child;
    };
    {
        let mut state = log.lock().unwrap();
        assert_eq!(
            state.preparation_entries, 0,
            "same retained preparation must not restart"
        );
        state.preparation_entries += 1;
        assert!(
            state
                .target_cleanup
                .replace(Arc::new(child.terminal_cleanup()))
                .is_none()
        );
    }
    if case != Case::UnexpectedStop {
        return child;
    }
    let cleanup = child.terminal_cleanup();
    let (stopped, event) = child.next_state().await.unwrap().assume_stopped();
    assert_eq!(event, Event::Signal(Signal::SIGSTOP));
    assert!(cleanup.same_generation(&stopped.terminal_cleanup()));
    log.lock().unwrap().cleanup_wait = Some(Box::pin(stopped.exit_event()));
    // The original ptrace stop still reserves this exact TID. Queue a real
    // signal while stopped, then suppress SIGSTOP on resume; Linux reports the
    // pending SIGUSR1 delivery stop before executing guest instructions.
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_tgkill,
                cleanup.thread_group_id().unwrap().as_raw(),
                stopped.pid().as_raw(),
                libc::SIGUSR1,
            )
        },
        0
    );
    stopped
        .resume(None)
        .expect("generate actual unexpected initial stop")
}

pub(super) async fn deliver_initial(
    id: Pid,
    initial: impl Future<Output = Result<Wait, TraceError>>,
) -> Result<Wait, TraceError> {
    let outcome = initial.await;
    let Some((log, case)) = selected(id) else {
        return outcome;
    };
    if case == Case::UnexpectedStop {
        assert!(matches!(
            &outcome,
            Ok(Wait::Stopped(_, Event::Signal(Signal::SIGUSR1)))
        ));
    }
    if !matches!(
        case,
        Case::DeliveryGap
            | Case::CancelAfterClaim
            | Case::CompetingClaim
            | Case::RevokedClaim
            | Case::DaemonPending
    ) {
        return outcome;
    }
    let Ok(Wait::Stopped(stopped, Event::Signal(Signal::SIGSTOP))) = &outcome else {
        panic!("delivery control requires the original actual SIGSTOP");
    };
    let cleanup = stopped.terminal_cleanup();
    {
        let state = log.lock().unwrap();
        assert!(cleanup.same_generation(state.target_cleanup.as_ref().unwrap()));
    }
    kill_to_exit(&cleanup);
    if case == Case::DaemonPending {
        let creator = log.lock().unwrap().creator_cleanup.clone().unwrap();
        wait_until(|| creator.exit_stop_observed());
    }
    if matches!(case, Case::CompetingClaim | Case::RevokedClaim) {
        let claim = stopped
            .exit_event()
            .await
            .expect("test owns actual sole EXIT claim");
        assert!(cleanup.same_generation(&claim.terminal_cleanup()));
        if case == Case::RevokedClaim {
            // The exact claimed value is transferred exclusively to the test's
            // cleanup slot. It is never given back to production or duplicated.
            unsafe { cleanup.revoke_owned_exit_stop() }.unwrap();
        }
        let mut state = log.lock().unwrap();
        assert!(state.cleanup_claim.replace(claim).is_none());
    }
    let mut first = true;
    future::poll_fn(|cx| {
        if first {
            first = false;
            let mut state = log.lock().unwrap();
            assert_eq!(state.held_delivery, 0);
            state.held_delivery += 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    outcome
}

pub(super) async fn after_exit_claim(stopped: &Stopped) {
    let Some((log, case)) = selected(stopped.pid()) else {
        return;
    };
    if case == Case::DaemonPending {
        {
            let mut state = log.lock().unwrap();
            assert!(
                stopped
                    .terminal_cleanup()
                    .same_generation(state.target_cleanup.as_ref().unwrap())
            );
            assert_eq!(state.exit_claims, 0);
            state.exit_claims += 1;
            assert!(state.creator_cleanup.as_ref().unwrap().exit_stop_observed());
            assert!(
                state
                    .creator_cleanup
                    .as_ref()
                    .unwrap()
                    .observed_terminal()
                    .is_none()
            );
        }
        let mut first = true;
        future::poll_fn(|cx| {
            let mut state = log.lock().unwrap();
            if first {
                first = false;
                state.claim_pause_pending += 1;
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                assert_eq!(state.claim_pause_completed, 0);
                state.claim_pause_completed += 1;
                Poll::Ready(())
            }
        })
        .await;
        return;
    }
    let (creator, receiver) = {
        let mut state = log.lock().unwrap();
        assert!(
            stopped
                .terminal_cleanup()
                .same_generation(state.target_cleanup.as_ref().unwrap())
        );
        assert_eq!(
            state.exit_claims, 0,
            "EXIT capability may be claimed only once"
        );
        state.exit_claims += 1;
        if case != Case::CancelAfterClaim {
            return;
        }
        let (sender, receiver) = oneshot::channel();
        assert!(state.release_claim.replace(sender).is_none());
        (state.creator_cleanup.clone().unwrap(), receiver)
    };
    creator
        .terminate_bound_task()
        .expect("cancel creator after actual child EXIT claim");
    let mut receiver = Box::pin(receiver);
    future::poll_fn(|cx| match receiver.as_mut().poll(cx) {
        Poll::Pending => {
            log.lock().unwrap().claim_pause_pending += 1;
            Poll::Pending
        }
        Poll::Ready(result) => {
            result.expect("creator final callback releases original retained future");
            let mut state = log.lock().unwrap();
            assert!(state.claim_pause_pending > 0);
            assert_eq!(state.claim_pause_completed, 0);
            state.claim_pause_completed += 1;
            Poll::Ready(())
        }
    })
    .await;
}

pub(super) fn stale_stop_discarded(stopped: &Stopped) {
    let Some((log, _)) = selected(stopped.pid()) else {
        return;
    };
    let mut state = log.lock().unwrap();
    assert!(
        stopped
            .terminal_cleanup()
            .same_generation(state.target_cleanup.as_ref().unwrap())
    );
    assert_eq!(state.stale_discarded, 0);
    state.stale_discarded += 1;
}

pub(super) fn exit_resuming(stopped: &Stopped) {
    let Some((log, _)) = selected(stopped.pid()) else {
        return;
    };
    let mut state = log.lock().unwrap();
    assert!(
        stopped
            .terminal_cleanup()
            .same_generation(state.target_cleanup.as_ref().unwrap())
    );
    assert_eq!(
        state.exit_resumes, 0,
        "only the sole EXIT capability may resume"
    );
    state.exit_resumes += 1;
}

async fn actual_final(stopped: Stopped) -> ExitStatus {
    let id = stopped.pid();
    let cleanup = stopped.terminal_cleanup();
    let waits = Arc::new(PtracerWaitOwner::default());
    waits.bind_stopped(&stopped);
    let final_wait = TracedTask::<Observer>::wait_after_exit_event(stopped, &waits, None).await;
    let (exited, status) = match final_wait {
        Ok(wait) => wait.assume_exited(),
        Err(TraceError::Died(zombie)) => (zombie.pid(), zombie.reap().await.unwrap()),
        Err(error) => panic!("actual fixture final wait failed: {error}"),
    };
    assert_eq!(exited, id);
    assert_eq!(status, ExitStatus::Signaled(Signal::SIGKILL, false));
    assert!(matches!(cleanup.observed_terminal(), Some(Ok(observed)) if observed == status));
    status
}
async fn die_after_stop(stopped: &Stopped) {
    let cleanup = stopped.terminal_cleanup();
    kill_to_exit(&cleanup);
    let claim = stopped.exit_event().await.unwrap();
    assert!(cleanup.same_generation(&claim.terminal_cleanup()));
    actual_final(claim).await;
    selected(stopped.pid())
        .unwrap()
        .0
        .lock()
        .unwrap()
        .test_final_waits += 1;
}
pub(super) async fn startup_error_case(stopped: Stopped) -> Result<Stopped, PreparedNewborn> {
    let Some((log, case)) = selected(stopped.pid()) else {
        return Ok(stopped);
    };
    if !matches!(
        case,
        Case::StartupCompetingClaim | Case::StartupRevokedClaim
    ) {
        return Ok(stopped);
    }
    let id = stopped.pid();
    let cleanup = stopped.terminal_cleanup();
    kill_to_exit(&cleanup);
    let claim = stopped.exit_event().await.unwrap();
    assert!(cleanup.same_generation(&claim.terminal_cleanup()));
    if case == Case::StartupRevokedClaim {
        // The sole claim is transferred to this test-owned cleanup continuation.
        unsafe { cleanup.revoke_owned_exit_stop() }.unwrap();
    }
    assert!(cleanup.observed_terminal().is_none());
    // Supply the startup-error premise directly. This case tests the real
    // helper's refusal rule; TimerDeath independently requires actual perf ESRCH.
    let waits = Arc::new(PtracerWaitOwner::default());
    let refused =
        TracedTask::<Observer>::finish_newborn_startup_exit(stopped, &waits, Errno::ESRCH.into())
            .await;
    assert!(matches!(refused, Err(TraceError::Errno(Errno::ESRCH))));
    assert!(cleanup.observed_terminal().is_none());
    assert!(log.lock().unwrap().refusal.replace(Errno::ESRCH).is_none());
    let status = actual_final(claim).await;
    log.lock().unwrap().test_final_waits += 1;
    Err(PreparedNewborn::Terminal { id, status })
}

pub(super) async fn before_timer(stopped: &Stopped) {
    if selected(stopped.pid()).is_some_and(|(_, case)| case == Case::TimerDeath) {
        die_after_stop(stopped).await;
    }
}
pub(super) fn timer_result(id: Pid, result: &Result<Timer, Errno>) {
    let Some((log, case)) = selected(id) else {
        return;
    };
    let mut state = log.lock().unwrap();
    state.timer_attempts += 1;
    if case == Case::TimerDeath {
        assert!(
            matches!(result, Err(Errno::ESRCH)),
            "real vanished TID must produce ESRCH"
        );
        state.timer_esrch += 1;
    }
}
pub(super) async fn before_restore(stopped: &Stopped, timer: &TaskTimer) {
    let Some((log, case)) = selected(stopped.pid()) else {
        return;
    };
    if case != Case::RestoreDeath {
        return;
    }
    assert!(matches!(timer, TaskTimer::Live(_)));
    let _actual_clock = timer.read_clock();
    log.lock().unwrap().restore_entries += 1;
    die_after_stop(stopped).await;
}

pub(super) async fn preparation_result(
    id: Pid,
    result: Result<PreparedNewborn, TraceError>,
) -> Result<PreparedNewborn, TraceError> {
    let Some((log, case)) = selected(id) else {
        return result;
    };
    let expected = match case {
        Case::CompetingClaim | Case::RevokedClaim => Errno::EALREADY,
        Case::UnexpectedStop => Errno::EPROTO,
        _ => return result,
    };
    assert!(
        matches!(&result, Err(TraceError::Errno(error)) if *error == expected),
        "the real preparation must refuse before fixture cleanup"
    );
    let (cleanup, claim, wait) = {
        let mut state = log.lock().unwrap();
        assert!(state.refusal.replace(expected).is_none());
        let cleanup = state.target_cleanup.clone().unwrap();
        assert!(
            cleanup.observed_terminal().is_none(),
            "no invented final can justify the refusal"
        );
        (
            cleanup,
            state.cleanup_claim.take(),
            state.cleanup_wait.take(),
        )
    };
    // This is test-owned cleanup AFTER capturing the exact production refusal.
    // It does not convert the refused operation into production success.
    let stopped = match claim {
        Some(stopped) => {
            assert!(wait.is_none());
            stopped
        }
        None => {
            kill_to_exit(&cleanup);
            wait.unwrap().await.unwrap()
        }
    };
    assert!(cleanup.same_generation(&stopped.terminal_cleanup()));
    let status = actual_final(stopped).await;
    log.lock().unwrap().test_final_waits += 1;
    Ok(PreparedNewborn::Terminal { id, status })
}

pub(super) fn constructed<L: Tool>(task: &TracedTask<L>) {
    let Some(log) = active() else { return };
    let mut state = log.lock().unwrap();
    assert!(state.constructed.insert(task.tid), "double task enrollment");
    let expected_tasks = if state.case.group() && task.tid != task.pid {
        3
    } else {
        2
    };
    assert_eq!(task.ntasks.load(Ordering::SeqCst), expected_tasks);
    let expected_daemons = if state.case.daemon() && task.tid != task.pid {
        2
    } else {
        0
    };
    assert_eq!(
        task.ndaemons.load(Ordering::SeqCst),
        expected_daemons,
        "daemon enrollment must precede every terminal or live path"
    );
    assert_eq!(task.is_a_daemon, expected_daemons != 0);
}
pub(super) fn retired(id: Pid, tasks: &Arc<AtomicUsize>, daemons: &Arc<AtomicUsize>) {
    let Some(log) = active() else { return };
    let mut state = log.lock().unwrap();
    assert!(state.retired.insert(id), "double consuming retirement");
    let total = if state.case.group() { 3 } else { 2 };
    assert_eq!(tasks.load(Ordering::SeqCst), total - state.retired.len());
    assert!(daemons.load(Ordering::SeqCst) <= tasks.load(Ordering::SeqCst));
    if state.target == Some(id) {
        // Actual consuming retirement, after the counter updates, releases the
        // normal root's guest-causal barrier. Early release is retained.
        state.root_release.take().unwrap().send(()).unwrap();
    }
}

#[derive(Default)]
struct Global;
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = ();
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _from: Pid, _request: ()) {}
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
    fn new(pid: Pid, _: &()) -> Self {
        let log = active().unwrap();
        log.lock().unwrap().root.get_or_insert(pid);
        Self
    }
    fn subscriptions(_: &()) -> Subscription {
        let mut selected = Subscription::none();
        let case = active().unwrap().lock().unwrap().case;
        if case == Case::LeaderExit {
            selected.syscalls([Sysno::getpid, Sysno::write]);
        } else {
            selected.syscalls([Sysno::clone, Sysno::getpid]);
        }
        selected
    }
    fn init_thread_state(&self, tid: Pid, parent: Option<(Pid, &State)>) -> State {
        if let Some((creator, parent)) = parent {
            let log = active().unwrap();
            let mut state = log.lock().unwrap();
            assert!(state.inherited.insert(tid));
            if state.case == Case::LeaderExit && state.target == Some(tid) {
                assert!(
                    !parent.terminal,
                    "live thread must be constructed before leader final"
                );
            }
            if state.case == Case::CancelAfterClaim {
                assert_eq!(state.creator, Some(creator));
                assert!(
                    parent.terminal,
                    "inherit from the retained actual terminal parent"
                );
            }
        }
        State::default()
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        let log = active().unwrap();
        let mut state = log.lock().unwrap();
        if state.target == Some(guest.tid()) {
            state.target_starts += 1;
        }
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        let daemonize = {
            let log = active().unwrap();
            let state = log.lock().unwrap();
            state.case.daemon()
                && state.root != Some(guest.pid())
                && matches!(&syscall, Syscall::Getpid(_))
        };
        if daemonize {
            guest.daemonize().await;
        }
        let root_wait = {
            let log = active().unwrap();
            let mut state = log.lock().unwrap();
            if state.root == Some(guest.tid()) && matches!(&syscall, Syscall::Getpid(_)) {
                Some(
                    state
                        .root_wait
                        .take()
                        .expect("root barrier entered exactly once"),
                )
            } else {
                None
            }
        };
        if let Some(wait) = root_wait {
            tokio::time::timeout(Duration::from_secs(1), wait)
                .await
                .expect("target retirement releases root within original one-second bound")
                .expect("target retirement sender remains owned");
            let log = active().unwrap();
            let mut state = log.lock().unwrap();
            assert!(state.retired.contains(&state.target.unwrap()));
            assert_eq!(state.root_barrier_completed, 0);
            state.root_barrier_completed += 1;
        }
        let writing = matches!(&syscall, Syscall::Write(_));
        if matches!(&syscall, Syscall::Clone(_)) {
            // Consume the intercepted entry with a different real syscall so
            // the unchanged clone below uses the private injection path and
            // retains the genuine child context required by these controls.
            let pid = guest.inject(reverie::syscalls::Getpid::default()).await?;
            assert_eq!(pid, guest.pid().as_raw() as i64);
        }
        let result = guest.inject(syscall).await?;
        if writing {
            let log = active().unwrap();
            let mut state = log.lock().unwrap();
            if state.target == Some(guest.tid()) {
                assert_eq!(state.case, Case::LeaderExit);
                assert_eq!(result, THREAD_MARKER.len() as i64);
                assert_eq!(state.target_writes, 0);
                state.target_writes += 1;
            }
        }
        Ok(result)
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
        if state.case == Case::LeaderExit {
            if state.target == Some(tid) || state.creator == Some(tid) {
                // The last thread's SYS_exit(37) determines the group final,
                // distinct from the creator's earlier EXIT event message 0.
                assert_eq!(status, ExitStatus::Exited(37));
            } else {
                assert_eq!(state.root, Some(tid));
                assert_eq!(status, ExitStatus::Exited(0));
            }
            if state.creator == Some(tid) {
                assert_eq!(state.target_starts, 1);
                assert_eq!(
                    state.target_writes, 1,
                    "actual child write precedes actual leader final"
                );
            }
        } else if state.target == Some(tid)
            || (state.case == Case::CancelAfterClaim && state.creator == Some(tid))
            || (state.case.daemon() && state.root != Some(tid))
        {
            assert_eq!(status, ExitStatus::Signaled(Signal::SIGKILL, false));
        } else {
            assert_eq!(status, ExitStatus::Exited(0));
        }
        if state.case == Case::CancelAfterClaim && state.creator == Some(tid) {
            state.release_claim.take().unwrap().send(()).unwrap();
        }
    }
    async fn on_exit_thread<G: GlobalRPC<Global>>(
        &self,
        tid: Pid,
        _: &G,
        thread: State,
        _: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert!(thread.terminal);
        assert!(active().unwrap().lock().unwrap().consumed.insert(tid));
        Ok(())
    }
}

extern "C" fn live_thread(_: *mut libc::c_void) -> libc::c_int {
    unsafe {
        if libc::write(1, THREAD_MARKER.as_ptr().cast(), THREAD_MARKER.len())
            != THREAD_MARKER.len() as isize
        {
            libc::syscall(libc::SYS_exit, 86);
        }
        libc::syscall(libc::SYS_exit, 37);
        libc::_exit(87);
    }
}

fn run(case: Case) {
    const MARKER: &[u8] = b"newborn startup supervisor\n";
    let log = Arc::new(StdMutex::new(Log::new(case)));
    ACTIVE.with(|slot| assert!(slot.borrow_mut().replace(Arc::clone(&log)).is_none()));
    let _active = Active;
    let (output, _) = test_fn_with_config::<Observer, _>(
        move || unsafe {
            libc::alarm(5);
            let child = libc::syscall(
                libc::SYS_clone,
                libc::SIGCHLD,
                0usize,
                0usize,
                0usize,
                0usize,
            );
            if child == 0 {
                if case.group() {
                    if case.daemon() {
                        libc::syscall(libc::SYS_getpid);
                    } // Tool daemonizes once.
                    let stack = libc::mmap(
                        std::ptr::null_mut(),
                        65536,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        -1,
                        0,
                    );
                    if stack == libc::MAP_FAILED {
                        libc::_exit(80);
                    }
                    if case == Case::LeaderExit {
                        if libc::clone(
                            live_thread,
                            stack.cast::<u8>().add(65536).cast(),
                            libc::CLONE_THREAD | libc::CLONE_VM | libc::CLONE_SIGHAND,
                            std::ptr::null_mut(),
                        ) < 0
                        {
                            libc::_exit(88);
                        }
                        libc::syscall(libc::SYS_exit, 0); // Only the real leader exits.
                    } else {
                        libc::syscall(
                            libc::SYS_clone,
                            libc::CLONE_THREAD | libc::CLONE_VM | libc::CLONE_SIGHAND,
                            stack.cast::<u8>().add(65536),
                            0usize,
                            0usize,
                            0usize,
                        );
                    }
                }
                libc::_exit(81); // No target child may execute this instruction.
            }
            if child < 0 {
                libc::_exit(82);
            }
            let mut status = 0;
            if libc::syscall(libc::SYS_wait4, child, &mut status, 0usize, 0usize) != child
                || if case == Case::LeaderExit {
                    !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 37
                } else {
                    !libc::WIFSIGNALED(status) || libc::WTERMSIG(status) != libc::SIGKILL
                }
            {
                libc::_exit(83);
            }
            // The subscribed call holds this normal root until the actual
            // target Tool retirement, not merely Linux's earlier final wait.
            if libc::syscall(libc::SYS_getpid) <= 0 {
                libc::_exit(85);
            }
            if libc::write(1, MARKER.as_ptr().cast(), MARKER.len()) != MARKER.len() as isize {
                libc::_exit(84);
            }
            libc::_exit(0);
        },
        (),
        true,
    )
    .expect("original native startup fixture");
    let state = log.lock().unwrap();
    if case == Case::CancelAfterClaim {
        assert_eq!(output.status, ExitStatus::Signaled(Signal::SIGKILL, false));
        assert!(output.stdout.is_empty());
    } else {
        assert_eq!(output.status, ExitStatus::Exited(0));
        if case == Case::LeaderExit {
            assert_eq!(output.stdout, [THREAD_MARKER, MARKER].concat());
        } else {
            assert_eq!(output.stdout, MARKER);
        }
    }
    assert_eq!(
        state.root_barrier_completed,
        usize::from(case != Case::CancelAfterClaim)
    );
    let total = if case.group() { 3 } else { 2 };
    assert_eq!(state.target_starts, usize::from(case == Case::LeaderExit));
    assert_eq!(state.preparation_entries, 1);
    assert_eq!(state.exit_resumes, 1);
    if matches!(case, Case::DaemonPending | Case::LeaderExit) {
        assert_eq!(state.creator_exit_joins, 1);
    }
    assert_eq!(state.constructed.len(), total - 1);
    assert_eq!(state.inherited.len(), total - 1);
    assert_eq!(state.terminal.len(), total);
    assert_eq!(state.consumed.len(), total);
    assert_eq!(state.retired.len(), total);
    let (tasks, daemons) = state.counts.as_ref().unwrap();
    assert_eq!(tasks.load(Ordering::SeqCst), 0);
    assert_eq!(daemons.load(Ordering::SeqCst), 0);
    assert!(
        state
            .target_cleanup
            .as_ref()
            .unwrap()
            .wait(Duration::from_secs(1))
    );
    assert!(
        state
            .creator_cleanup
            .as_ref()
            .unwrap()
            .wait(Duration::from_secs(1))
    );
    match case {
        Case::DeliveryGap | Case::CancelAfterClaim | Case::DaemonPending => {
            assert_eq!(state.held_delivery, 1);
            assert_eq!(state.exit_claims, 1);
            assert_eq!(state.stale_discarded, 1);
            assert_eq!(state.test_final_waits, 0);
            assert_eq!(state.timer_attempts, 0);
            if matches!(case, Case::CancelAfterClaim | Case::DaemonPending) {
                assert_eq!(state.claim_pause_completed, 1);
            }
        }
        Case::TimerDeath => {
            assert_eq!(state.timer_attempts, 1);
            assert_eq!(state.timer_esrch, 1);
            assert_eq!(state.test_final_waits, 1);
        }
        Case::RestoreDeath => {
            assert_eq!(state.timer_attempts, 1);
            assert_eq!(state.restore_entries, 1);
            assert_eq!(state.test_final_waits, 1);
        }
        Case::CompetingClaim | Case::RevokedClaim => {
            assert_eq!(state.refusal, Some(Errno::EALREADY));
            assert_eq!(state.exit_claims, 0);
            assert_eq!(state.test_final_waits, 1);
        }
        Case::UnexpectedStop => {
            assert_eq!(state.refusal, Some(Errno::EPROTO));
            assert_eq!(state.test_final_waits, 1);
        }
        Case::StartupCompetingClaim | Case::StartupRevokedClaim => {
            assert_eq!(state.refusal, Some(Errno::ESRCH));
            assert_eq!(state.timer_attempts, 0);
            assert_eq!(state.test_final_waits, 1);
        }
        Case::LeaderExit => {
            assert_eq!(state.creator_resumes, 1);
            assert_eq!(state.target_writes, 1);
            assert_eq!(state.timer_attempts, 1);
            assert_eq!(state.test_final_waits, 0);
        }
        Case::BothReady | Case::Daemon => {}
    }
    println!(
        "newborn-startup case={case:?} creator={} target={} actual-final={total} target-start={} consumed={total} tasks=0 daemons=0 controlled-delivery={} refusal={:?}",
        state.creator.unwrap(),
        state.target.unwrap(),
        state.target_starts,
        state.held_delivery,
        state.refusal
    );
}

#[test]
fn native_both_ready_before_first_poll_keeps_saved_context() {
    run(Case::BothReady);
}
#[test]
fn native_initial_delivery_gap_discards_only_original_sigstop() {
    run(Case::DeliveryGap);
}
#[test]
fn native_post_claim_creator_cancellation_retains_same_future() {
    run(Case::CancelAfterClaim);
}
#[test]
fn native_actual_timer_esrch_requires_same_generation_final() {
    run(Case::TimerDeath);
}
#[test]
fn native_death_before_saved_context_restore_retains_final() {
    run(Case::RestoreDeath);
}
#[test]
fn native_competing_exit_claim_refuses_without_final() {
    run(Case::CompetingClaim);
}
#[test]
fn native_revoked_exit_claim_refuses_without_final() {
    run(Case::RevokedClaim);
}
#[test]
fn native_unexpected_initial_stop_refuses_before_start() {
    run(Case::UnexpectedStop);
}
#[test]
fn native_daemon_thread_prestart_group_death_balances_enrollment() {
    run(Case::Daemon);
}

#[test]
fn native_startup_esrch_competing_claim_requires_actual_final() {
    run(Case::StartupCompetingClaim);
}
#[test]
fn native_startup_esrch_revoked_claim_requires_actual_final() {
    run(Case::StartupRevokedClaim);
}

#[test]
fn native_pending_daemon_group_death_resumes_retained_thread() {
    run(Case::DaemonPending);
}
#[test]
fn native_leader_only_exit_runs_retained_live_thread_before_final() {
    run(Case::LeaderExit);
}

#[test]
#[should_panic(expected = "terminal-only child has no live timer")]
fn terminal_timer_refuses_clock_read() {
    TaskTimer::Terminal(ExitStatus::Exited(37)).read_clock();
}
#[test]
#[should_panic(expected = "terminal-only child has no live timer")]
fn terminal_timer_refuses_mutating_observation() {
    TaskTimer::Terminal(ExitStatus::Exited(37)).observe_event(&Event::Seccomp);
}
