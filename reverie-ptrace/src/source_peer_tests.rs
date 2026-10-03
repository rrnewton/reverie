/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
//! Native proposal. Compile/run admission is separate from source review.
use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::InjectedSyscallEvent;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

use super::*;

type ExitOrder = (&'static str, i32, bool, bool, usize);

#[derive(Default)]
struct Probe {
    arrived: AtomicUsize,
    done: AtomicBool,
    entered: AtomicUsize,
    returned: AtomicUsize,
    source: AtomicUsize,
    boundary: AtomicUsize,
    peer_attempted: AtomicBool,
    boundary_taken: AtomicBool,
    boundary_refused: AtomicBool,
    peer_exit_entered: AtomicUsize,
    peer_exit_transferred: AtomicUsize,
    sender_exit_deferred: AtomicBool,
    external_peer: Mutex<Option<Arc<crate::tracer::FatalTaskStop>>>,
    external_sender: Mutex<Option<safeptrace::TaskIdentity>>,
    exit_order: Mutex<Vec<ExitOrder>>,
    checks: Mutex<Vec<bool>>,
    session: Mutex<Weak<super::super::FatalSession>>,
    terminals: Mutex<Vec<Arc<crate::tracer::FatalTaskStop>>>,
}
const PEER_RESUME: usize = 1;
const CANCEL_RETURNED: usize = 2;
const LOSE_RESTORATION: usize = 3;
const MISSING_STOP: usize = 4;
const UNARMED_STOP: usize = 5;
const EXTERNAL_EXIT: usize = 6;
thread_local! {
    static ACTIVE: RefCell<Weak<Probe>> = const { RefCell::new(Weak::new()) };
}
fn active() -> Option<Arc<Probe>> {
    ACTIVE.with(|slot| slot.borrow().upgrade())
}
pub(super) fn bind_controls(
    bindings: &[(
        Arc<crate::tracer::FatalTaskStop>,
        Arc<safeptrace::ControlHold>,
    )],
) -> Result<(), safeptrace::Errno> {
    if let Some(probe) = active() {
        let boundary = probe.boundary.load(Ordering::SeqCst);
        if matches!(boundary, MISSING_STOP | UNARMED_STOP)
            && !probe.boundary_taken.swap(true, Ordering::SeqCst)
        {
            let (result, refused_before_rescue) =
                crate::tracer::FatalTaskStop::probe_missing_peer_stop(
                    bindings,
                    boundary == UNARMED_STOP,
                );
            probe
                .boundary_refused
                .store(refused_before_rescue, Ordering::SeqCst);
            // Unexpected success is a failed test, never permission to resume.
            return result.and(Err(safeptrace::Errno::EPROTO));
        }
    }
    crate::tracer::FatalTaskStop::bind_peer_controls(bindings)
}
pub(super) fn abandon_restoration(peers: &Arc<NativePeers>) -> bool {
    active().is_some_and(|probe| {
        probe.boundary.load(Ordering::SeqCst) == LOSE_RESTORATION
            && probe.returned.load(Ordering::SeqCst) == 1
            && peers.state.lock().unwrap().phase == NativePeerPhase::Executing
            && !probe.boundary_taken.swap(true, Ordering::SeqCst)
    })
}
/// Schedule only the test's genuine externally observed EXIT ordering. No
/// capability/event is changed and the original completion deadline still owns
/// this wait. The sender resumes polling as soon as the peer enters its actual
/// terminal barrier, before that barrier waits for all frozen members.
pub(crate) async fn gate_external_sender<T>(
    stop: &crate::tracer::FatalTaskStop,
    selected: impl std::future::Future<Output = T>,
) -> T {
    futures::pin_mut!(selected);
    futures::future::poll_fn(|cx| {
        if let Some(probe) = active()
            && probe.boundary.load(Ordering::SeqCst) == EXTERNAL_EXIT
            && probe.boundary_taken.load(Ordering::SeqCst)
        {
            let sender_matches =
                probe
                    .external_sender
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|sender| {
                        stop.terminal
                            .task_identity()
                            .is_ok_and(|actual| sender.same_generation(&actual))
                    });
            let (peer_matches, peer_observed) = probe
                .external_peer
                .lock()
                .unwrap()
                .as_ref()
                .map(|peer| {
                    (
                        peer.terminal.same_generation(&stop.terminal),
                        peer.terminal.exit_stop_observed(),
                    )
                })
                .unwrap_or((false, false));
            let entered = probe.peer_exit_entered.load(Ordering::SeqCst);
            {
                let mut order = probe.exit_order.lock().unwrap();
                if order.len() < 32 {
                    order.push((
                        "poll",
                        stop.tid.as_raw(),
                        sender_matches,
                        peer_observed,
                        entered,
                    ));
                }
            }
            if sender_matches && peer_observed && entered == 0 {
                probe.sender_exit_deferred.store(true, Ordering::SeqCst);
                cx.waker().wake_by_ref();
                return std::task::Poll::Pending;
            }
            // In the child-sender case the root peer can otherwise enter its
            // real EXIT barrier before the sender's next poll. Delay only that
            // same observed peer until the sender's required deferred poll;
            // then its actual branch entry releases the sender above.
            if peer_matches
                && peer_observed
                && entered == 0
                && !probe.sender_exit_deferred.load(Ordering::SeqCst)
            {
                cx.waker().wake_by_ref();
                return std::task::Poll::Pending;
            }
        }
        std::future::Future::poll(selected.as_mut(), cx)
    })
    .await
}
/// Side observation of the real terminal owner before/after its real barrier.
/// This neither clears a hold nor drives a retry/cleanup rescue.
pub(crate) fn terminal_transfer(
    stop: &crate::tracer::FatalTaskStop,
    peer_held: bool,
    complete: bool,
) {
    let Some(probe) = active() else { return };
    if probe.boundary.load(Ordering::SeqCst) != EXTERNAL_EXIT || !peer_held {
        return;
    }
    assert!(stop.frozen.load(Ordering::Acquire));
    probe.exit_order.lock().unwrap().push((
        if complete { "transfer" } else { "entry" },
        stop.tid.as_raw(),
        false,
        peer_held,
        probe.peer_exit_entered.load(Ordering::SeqCst),
    ));
    if complete {
        assert!(stop.peer_invocation.lock().unwrap().is_none());
        probe.peer_exit_transferred.fetch_add(1, Ordering::SeqCst);
    } else {
        probe.peer_exit_entered.fetch_add(1, Ordering::SeqCst);
    }
}
fn sender_is_running(session: &super::super::FatalSession) -> bool {
    session.tree.lock().unwrap().tasks.iter().any(|task| {
        task.peer_invocation
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|peers| {
                peers.state.lock().unwrap().phase == NativePeerPhase::Executing
                    && task.held.lock().unwrap().is_none()
            })
    })
}
#[derive(Default)]
struct Global(Arc<Probe>);
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = (bool, bool, Option<usize>); // child sender, blocked, changed register
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
#[derive(Default)]
struct Sender;
#[reverie::tool]
impl Tool for Sender {
    type GlobalState = Global;
    type ThreadState = ();
    fn subscriptions(_: &(bool, bool, Option<usize>)) -> Subscription {
        [
            Sysno::write,
            Sysno::sendto,
            Sysno::clone,
            Sysno::clone3,
            Sysno::exit,
            Sysno::exit_group,
        ]
        .into_iter()
        .collect()
    }
    fn observe_injected_syscalls(_: &(bool, bool, Option<usize>)) -> bool {
        true
    }
    fn observe_injected_syscall_preparation(_: &(bool, bool, Option<usize>)) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        tid: Pid,
        global: &Global,
        _: &mut (),
        nr: Sysno,
        _: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        if nr != Sysno::sendto {
            return;
        }
        if event == InjectedSyscallEvent::Entered {
            global.0.entered.fetch_add(1, Ordering::SeqCst);
            let session = global.0.session.lock().unwrap().upgrade().unwrap();
            let tasks = session.tree.lock().unwrap().tasks.clone();
            let sender = tasks.iter().find(|task| task.tid == tid).unwrap();
            let peers = sender.peer_invocation.lock().unwrap().clone().unwrap();
            let h = peers.member.history.0.lock().unwrap();
            let state = peers.state.lock().unwrap();
            let mut checks = global.0.checks.lock().unwrap();
            checks.push(state.bindings.len() == 1 && peers.matches_history(&h, &state));
            // These are duplicates of the actual live bindings. Both helpers
            // must refuse BEFORE locking the same original held slot twice.
            let duplicate = vec![state.bindings[0].clone(), state.bindings[0].clone()];
            checks.push(
                crate::tracer::FatalTaskStop::bind_peer_controls(&duplicate)
                    == Err(safeptrace::Errno::EINVAL),
            );
            checks.push(
                crate::tracer::FatalTaskStop::release_peer_controls(&duplicate, false)
                    == Err(safeptrace::Errno::EINVAL),
            );
            checks.push(crate::tracer::FatalTaskStop::peer_controls_match(
                &state.bindings,
                false,
            ));
            for (id, task) in &h.tasks {
                if *id != peers.member.index {
                    let (_, probes) = task
                        .stop
                        .as_ref()
                        .unwrap()
                        .probe_held_controls(global.0.source.load(Ordering::SeqCst))
                        .unwrap();
                    checks.extend(probes);
                }
            }
        }
        if matches!(event, InjectedSyscallEvent::Returned(8)) {
            global.0.returned.fetch_add(1, Ordering::SeqCst);
            let session = global.0.session.lock().unwrap().upgrade().unwrap();
            let tasks = session.tree.lock().unwrap().tasks.clone();
            let sender = tasks.iter().find(|task| task.tid == tid).unwrap();
            let peers = sender.peer_invocation.lock().unwrap().clone().unwrap();
            let state = peers.state.lock().unwrap();
            global.0.checks.lock().unwrap().push(
                state.phase == NativePeerPhase::Executing
                    && crate::tracer::FatalTaskStop::peer_controls_match(&state.bindings, false)
                    && state
                        .bindings
                        .iter()
                        .all(|(_, control)| control.validate().is_ok()),
            );
            drop(state);
            if global.0.boundary.load(Ordering::SeqCst) == CANCEL_RETURNED {
                let accepted = session.request_termination(
                    anyhow::anyhow!("cancel at actual native Returned before restoration").into(),
                );
                global.0.boundary_taken.store(accepted, Ordering::SeqCst);
            }
        }
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        let (nr, args) = call.into_parts();
        let selected = nr == Sysno::sendto;
        if !selected && !(nr == Sysno::write && args.arg0 == 688) {
            return Ok(guest.inject(call).await?);
        }
        let probe = Arc::clone(&guest.local_global_state().unwrap().0);
        probe.arrived.fetch_add(1, Ordering::SeqCst);
        while probe.arrived.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
        if !selected {
            while !probe.done.load(Ordering::SeqCst) {
                if probe.boundary.load(Ordering::SeqCst) == PEER_RESUME {
                    let session = probe.session.lock().unwrap().upgrade().unwrap();
                    if sender_is_running(&session) {
                        probe.peer_attempted.store(true, Ordering::SeqCst);
                        // Actual original peer callback tries the actual native
                        // write. RootStopLease must refuse before taking its stop.
                        let _ = guest.inject(call).await?;
                        panic!("held peer native injection returned");
                    }
                }
                tokio::task::yield_now().await;
            }
            return Ok(8); // this explicit marker has no native output oracle
        }
        probe.source.store(args.arg1, Ordering::SeqCst);
        if let Some(index) = guest.config().2 {
            let mut regs = guest.regs().await;
            let register = match index {
                0 => &mut regs.rdi,
                1 => &mut regs.rsi,
                2 => &mut regs.rdx,
                3 => &mut regs.r10,
                4 => &mut regs.r8,
                5 => &mut regs.r9,
                6 => &mut regs.orig_rax,
                7 => &mut regs.rip,
                8 => &mut regs.rsp,
                _ => unreachable!(),
            };
            *register ^= 1;
            guest.set_regs(regs).await?;
        }
        let Syscall::Sendto(sendto) = call else {
            unreachable!()
        };
        let result = guest
            .inject_original_sendto_with_stopped_peers(sendto)
            .await?;
        assert_eq!(result, 8);
        probe.done.store(true, Ordering::SeqCst);
        Ok(result)
    }
    fn on_backend_thread_terminal(&self, tid: Pid, global: &Global, _: &mut (), _: ExitStatus) {
        let session = global.0.session.lock().unwrap().upgrade().unwrap();
        let task = session
            .tree
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|task| task.tid == tid)
            .unwrap()
            .clone();
        global.0.terminals.lock().unwrap().push(task);
    }
    async fn on_exit_thread<G: reverie::GlobalRPC<Global>>(
        &self,
        _: Pid,
        _: &G,
        _: (),
        _: ExitStatus,
    ) -> Result<(), reverie::Error> {
        Ok(())
    }
}
async fn case(child: bool, blocked: bool, changed: Option<usize>) {
    case_boundary(child, blocked, changed, 0).await;
}
async fn case_boundary(child: bool, blocked: bool, changed: Option<usize>, boundary: usize) {
    let fixture = std::path::PathBuf::from(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
    let mut command = reverie::process::Command::new(fixture);
    command.arg(match (child, blocked) {
        (false, false) => "peer-sendto-root",
        (true, false) => "peer-sendto-child",
        (false, true) => "peer-sendto-blocked-root",
        (true, true) => "peer-sendto-blocked-child",
    });
    let tracer = crate::TracerBuilder::<Sender>::new(command)
        .config((child, blocked, changed))
        .spawn()
        .await
        .unwrap();
    let (session, global) = tracer.followed_source_test_context();
    let probe = Arc::clone(&global.0);
    *probe.session.lock().unwrap() = Arc::downgrade(&session);
    probe.boundary.store(boundary, Ordering::SeqCst);
    ACTIVE.with(|slot| *slot.borrow_mut() = Arc::downgrade(&probe));
    drop(global);
    let terminate = tracer.termination_handle().unwrap();
    let completion = tracer.wait_completion();
    futures::pin_mut!(completion);
    if blocked && boundary != PEER_RESUME {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let actual_running = sender_is_running(&session);
                if actual_running {
                    break;
                }
                tokio::select! {
                    _ = &mut completion => panic!("run ended before the actual sender resume"),
                    _ = tokio::task::yield_now() => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(probe.entered.load(Ordering::SeqCst), 1);
        assert_eq!(probe.returned.load(Ordering::SeqCst), 0);
        if boundary == EXTERNAL_EXIT {
            // Simulate an actual outside actor through the already retained
            // root PIDFD, without asking the backend to bypass its hold gate.
            // The original group identity is neither reopened nor guessed.
            let senders: Vec<_> = session
                .tree
                .lock()
                .unwrap()
                .tasks
                .iter()
                .filter_map(|task| task.peer_invocation.lock().unwrap().clone())
                .collect();
            assert_eq!(senders.len(), 1);
            let peers = &senders[0];
            let history = peers.member.history.0.lock().unwrap();
            let sender = peers.sender.upgrade().unwrap();
            let sender_identity = sender.terminal.task_identity().unwrap();
            assert!(
                history.tasks[&peers.member.index]
                    .identity
                    .same_generation(&sender_identity)
            );
            let state = peers.state.lock().unwrap();
            assert_eq!(state.bindings.len(), 1);
            let peer = Arc::clone(&state.bindings[0].0);
            assert!(
                !peer
                    .terminal
                    .task_identity()
                    .unwrap()
                    .same_generation(&sender_identity)
            );
            *probe.external_peer.lock().unwrap() = Some(peer);
            *probe.external_sender.lock().unwrap() = Some(sender_identity);
            drop(state);
            let roots: Vec<_> = history
                .tasks
                .values()
                .filter(|task| matches!(task.origin, Origin::Command))
                .map(|task| &task.identity)
                .collect();
            assert_eq!(roots.len(), 1);
            let groups = session.groups.lock().unwrap();
            // This registry has one row per captured task, including the
            // nonleader thread whose TraceeIdentity has no regular PIDFD.
            assert_eq!(groups.len(), peers.members.len());
            for group in groups.iter() {
                let identity = group.terminal.task_identity().unwrap();
                assert_eq!(
                    peers
                        .members
                        .values()
                        .filter(|member| member.same_generation(&identity))
                        .count(),
                    1
                );
            }
            let original_root: Vec<_> = groups
                .iter()
                .filter(|group| {
                    group
                        .terminal
                        .task_identity()
                        .is_ok_and(|identity| roots[0].same_generation(&identity))
                })
                .collect();
            assert_eq!(original_root.len(), 1);
            original_root[0]
                .identity
                .signal_owned_process_group()
                .unwrap();
            probe.boundary_taken.store(true, Ordering::SeqCst);
        } else {
            assert!(
                terminate
                    .terminate(anyhow::anyhow!("cancel actual blocked peer-held sender").into())
            );
        }
    }
    let outcome = tokio::time::timeout(Duration::from_secs(3), async {
        if boundary == EXTERNAL_EXIT {
            let peer = probe
                .external_peer
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .clone();
            // The original notifier observes this real externally induced EXIT
            // without polling/canceling either Tool continuation. This wait and
            // subsequent completion share the unchanged three-second bound.
            while !peer.terminal.exit_stop_observed() {
                tokio::task::yield_now().await;
            }
        }
        (&mut completion).await
    })
    .await
    .unwrap();
    let crate::ToolRunOutcome::Complete(completed) = outcome else {
        panic!("native peer cleanup unconfirmed");
    };
    let failed = blocked || changed.is_some() || boundary != 0;
    assert_eq!(completed.result.is_err(), failed);
    if !failed {
        assert!(matches!(completed.result, Ok(ExitStatus::Exited(0))));
        assert_eq!(probe.returned.load(Ordering::SeqCst), 1);
        assert!(probe.done.load(Ordering::SeqCst));
    }
    if changed.is_some() || matches!(boundary, MISSING_STOP | UNARMED_STOP) {
        assert_eq!(probe.entered.load(Ordering::SeqCst), 0);
    }
    if changed.is_none() && !matches!(boundary, MISSING_STOP | UNARMED_STOP) {
        let checks = probe.checks.lock().unwrap();
        assert!(!checks.is_empty() && checks.iter().all(|check| *check));
    }
    if boundary == PEER_RESUME {
        assert!(probe.peer_attempted.load(Ordering::SeqCst));
        assert!(!probe.done.load(Ordering::SeqCst));
        assert_eq!(probe.returned.load(Ordering::SeqCst), 0);
    }
    if matches!(boundary, CANCEL_RETURNED | LOSE_RESTORATION) {
        assert!(probe.boundary_taken.load(Ordering::SeqCst));
        assert_eq!(probe.returned.load(Ordering::SeqCst), 1);
        assert!(!probe.done.load(Ordering::SeqCst));
    }
    if boundary == EXTERNAL_EXIT {
        eprintln!(
            "external EXIT child={child} deferred={} entered={} transferred={} refused={} done={} returned={} order={:?}",
            probe.sender_exit_deferred.load(Ordering::SeqCst),
            probe.peer_exit_entered.load(Ordering::SeqCst),
            probe.peer_exit_transferred.load(Ordering::SeqCst),
            session.cleanup_was_refused(),
            probe.done.load(Ordering::SeqCst),
            probe.returned.load(Ordering::SeqCst),
            probe.exit_order.lock().unwrap(),
        );
        assert!(probe.boundary_taken.load(Ordering::SeqCst));
        assert!(!probe.done.load(Ordering::SeqCst));
        assert_eq!(probe.returned.load(Ordering::SeqCst), 0);
        assert!(probe.sender_exit_deferred.load(Ordering::SeqCst));
        assert_eq!(probe.peer_exit_entered.load(Ordering::SeqCst), 1);
        assert_eq!(probe.peer_exit_transferred.load(Ordering::SeqCst), 1);
        assert!(!session.cleanup_was_refused());
    }
    if matches!(boundary, MISSING_STOP | UNARMED_STOP) {
        assert!(probe.boundary_taken.load(Ordering::SeqCst));
        assert!(probe.boundary_refused.load(Ordering::SeqCst));
        assert!(!probe.done.load(Ordering::SeqCst));
    }
    let owners = std::mem::take(&mut *probe.terminals.lock().unwrap());
    assert_eq!(owners.len(), 2);
    for owner in owners {
        assert!(owner.peer_invocation.lock().unwrap().is_none());
        let worker = owner
            .terminal
            .take_final_test_worker()
            .expect("original notifier worker");
        tokio::time::timeout(Duration::from_secs(1), async {
            while !worker.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(worker.join().is_ok());
        assert!(owner.terminal.final_test_activity().2);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_peer_sendto_root_and_child_return_then_release() {
    case(false, false, None).await;
    case(true, false, None).await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_peer_sendto_cancellation_freezes_and_reaps_original_cohort() {
    case(false, true, None).await;
    case(true, true, None).await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_peer_sendto_changed_original_tuple_never_enters() {
    for field in 0..9 {
        case(false, false, Some(field)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_peer_sendto_peer_resume_refuses_without_losing_cleanup_stop() {
    case_boundary(false, true, None, PEER_RESUME).await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_peer_sendto_return_cancellation_and_lost_restoration_keep_custody() {
    case_boundary(false, false, None, CANCEL_RETURNED).await;
    case_boundary(true, false, None, LOSE_RESTORATION).await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_peer_sendto_missing_or_unarmed_original_cleanup_stop_refuses() {
    case_boundary(false, false, None, MISSING_STOP).await;
    case_boundary(true, false, None, UNARMED_STOP).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_peer_sendto_external_group_exit_transfers_then_reaps_without_retry() {
    case_boundary(false, true, None, EXTERNAL_EXIT).await;
    case_boundary(true, true, None, EXTERNAL_EXIT).await;
}
