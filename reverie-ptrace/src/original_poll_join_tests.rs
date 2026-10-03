/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
//! Actual original three-task callbacks and original timer completion owners.
//! The marker return values are controlled Tool behavior, not network evidence.
use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Poll;
use std::time::Duration;

use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

use super::*;

#[derive(Default)]
struct Probe {
    kind: AtomicUsize,
    first: AtomicUsize,
    tids: Mutex<[Option<Pid>; 2]>,
    publication_reached: AtomicBool,
    publication_release: AtomicBool,
    arrived: AtomicUsize,
    start: AtomicBool,
    captured: [AtomicBool; 2],
    outputs: [AtomicBool; 2],
    ready: [AtomicBool; 2],
    allow: [AtomicBool; 2],
    entered: [AtomicUsize; 2],
    returned: [AtomicUsize; 2],
    finished: [AtomicBool; 2],
    joined: AtomicBool,
    pending_checks: AtomicUsize,
    done: AtomicBool,
    cancelled: AtomicBool,
    session: Mutex<Weak<super::super::FatalSession>>,
    terminals: Mutex<Vec<Arc<crate::tracer::FatalTaskStop>>>,
}
std::thread_local! {
    static ACTIVE: std::cell::RefCell<Weak<Probe>> = const { std::cell::RefCell::new(Weak::new()) };
}
pub(crate) async fn pause_publication(task: &Stopped) {
    let Some(probe) = ACTIVE.with(|active| active.borrow().upgrade()) else {
        return;
    };
    if !matches!(probe.kind.load(Ordering::SeqCst), 2 | 8 | 10) {
        return;
    }
    let first = probe.first.load(Ordering::SeqCst);
    if probe.tids.lock().unwrap()[first] != Some(task.pid()) {
        return;
    }
    probe.publication_reached.store(true, Ordering::SeqCst);
    while !probe.publication_release.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
}

#[derive(Default)]
struct Global(Arc<Probe>);
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = (u8, usize, u8);
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
#[derive(Default)]
struct PollJoiner;
#[reverie::tool]
impl Tool for PollJoiner {
    type GlobalState = Global;
    type ThreadState = Option<usize>;
    fn subscriptions(_: &(u8, usize, u8)) -> Subscription {
        [
            Sysno::read,
            Sysno::sendto,
            Sysno::poll,
            Sysno::clone,
            Sysno::clone3,
            Sysno::exit,
            Sysno::exit_group,
        ]
        .into_iter()
        .collect()
    }
    fn observe_injected_syscalls(_: &(u8, usize, u8)) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        _: Pid,
        global: &Global,
        thread: &mut Option<usize>,
        nr: Sysno,
        _: SyscallArgs,
        event: reverie::InjectedSyscallEvent,
    ) {
        if let Some(index) = *thread
            && matches!(nr, Sysno::ppoll | Sysno::getpid)
        {
            match event {
                reverie::InjectedSyscallEvent::Entered => {
                    global.0.entered[index].fetch_add(1, Ordering::SeqCst);
                }
                reverie::InjectedSyscallEvent::Returned(0) => {
                    global.0.returned[index].fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        let (nr, args) = call.into_parts();
        let peer = (nr == Sysno::read && args.arg0 == 745)
            || (nr == Sysno::poll && matches!(args.arg3, 744 | 745));
        let parent = (matches!(nr, Sysno::read | Sysno::sendto) && args.arg0 == 746)
            || (nr == Sysno::poll && args.arg0 == 0 && args.arg1 == 0 && args.arg2 == 0);
        if !peer && !parent {
            return Ok(guest.inject(call).await?);
        }
        let probe = Arc::clone(&guest.local_global_state().unwrap().0);
        let (_, first, kind) = *guest.config();
        let session = probe.session.lock().unwrap().upgrade().unwrap();
        probe.arrived.fetch_add(1, Ordering::SeqCst);
        while probe.arrived.load(Ordering::SeqCst) != 3 {
            tokio::task::yield_now().await;
        }
        if peer {
            let index = if nr == Sysno::poll {
                args.arg3 - 744
            } else {
                args.arg0 - 744
            };
            *guest.thread_state_mut() = Some(index);
            probe.tids.lock().unwrap()[index] = Some(guest.tid());
            if index == 1 {
                while !probe.captured[0].load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
            }
            if nr == Sysno::poll {
                let input = guest
                    .capture_original_followed_poll(call, Box::new(()))
                    .await
                    .map_err(|e| {
                        reverie::Error::Tool(anyhow::anyhow!("Poll join capture: {e:?}"))
                    })?;
                assert_eq!(
                    (input.fd, input.events, input.timeout_millis),
                    ((744 + index) as i32, libc::POLLIN, 5000)
                );
            }
            probe.captured[index].store(true, Ordering::SeqCst);
            while !probe.start.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            let tid = guest.tid();
            let mut timer = Box::pin(async {
                if kind == 7 && index == first {
                    guest.inject(reverie::syscalls::Getpid::new()).await?;
                    Ok(())
                } else if nr == Sysno::poll {
                    guest
                        .inject_poll_observation_timer(call, Duration::from_millis(1))
                        .await
                } else {
                    guest
                        .inject_receive_observation_timer(call, Duration::from_millis(1))
                        .await
                }
            });
            std::future::poll_fn(|cx| {
                assert!(timer.as_mut().poll(cx).is_pending());
                if probe.entered[index].load(Ordering::SeqCst) == 1 {
                    let owner = session
                        .tree
                        .lock()
                        .unwrap()
                        .tasks
                        .iter()
                        .find(|t| t.tid == tid)
                        .unwrap()
                        .clone();
                    let identity = owner.terminal.task_identity().unwrap();
                    let h = session.source_cohort.0.lock().unwrap();
                    assert!(
                        h.tasks
                            .values()
                            .any(|t| t.identity.same_generation(&identity)
                                && t.life == Life::Executing
                                && t.invocation.is_some())
                    );
                    probe.ready[index].store(true, Ordering::SeqCst);
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
            while !probe.allow[index].load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            if kind == 3 && index == first {
                drop(timer);
                assert!(session.is_failed());
                probe.cancelled.store(true, Ordering::SeqCst);
                return Ok(0); // The unfinished callback must refuse this attempt.
            }
            if kind == 8 && index == first {
                std::future::poll_fn(|cx| {
                    assert!(timer.as_mut().poll(cx).is_pending());
                    if probe.publication_reached.load(Ordering::SeqCst) {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                })
                .await;
                drop(timer);
                assert!(session.is_failed());
                probe.cancelled.store(true, Ordering::SeqCst);
                return Ok(0);
            }
            timer.await?;
            probe.finished[index].store(true, Ordering::SeqCst);
            while !probe.joined.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            if nr == Sysno::poll {
                guest
                    .with_followed_poll_store(call, |writer| {
                        assert_eq!(writer.validate_context(), Ok(()));
                        assert_eq!(
                            writer.store_revents(libc::POLLIN),
                            reverie::syscalls::NativeUserStoreOutcome::Attempted {
                                raw: Ok(2),
                                postcheck: Ok(())
                            }
                        );
                    })
                    .map_err(|e| {
                        reverie::Error::Tool(anyhow::anyhow!("Poll join output: {e:?}"))
                    })?;
            }
            probe.outputs[index].store(true, Ordering::SeqCst);
            while !probe.done.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            return Ok(if nr == Sysno::poll { 1 } else { 0 });
        }
        if nr == Sysno::read {
            guest
                .inject_receive_observation_timer(call, Duration::from_millis(1))
                .await?;
        }
        while !probe
            .captured
            .iter()
            .all(|ready| ready.load(Ordering::SeqCst))
        {
            tokio::task::yield_now().await;
        }
        probe.start.store(true, Ordering::SeqCst);
        while !probe.ready.iter().all(|ready| ready.load(Ordering::SeqCst)) {
            tokio::task::yield_now().await;
        }
        let owner = session
            .tree
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|t| t.tid == guest.tid())
            .unwrap()
            .clone();
        let identity = owner.terminal.task_identity().unwrap();
        let index = session
            .source_cohort
            .0
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|(_, t)| t.identity.same_generation(&identity))
            .map(|(i, _)| *i)
            .unwrap();
        let member = Member {
            history: Arc::clone(&session.source_cohort),
            index,
        };
        assert!(matches!(member.acquire(), Err(safeptrace::Errno::EBUSY)));
        if kind == 1 {
            for allow in &probe.allow {
                allow.store(true, Ordering::SeqCst);
            }
            while !probe
                .finished
                .iter()
                .all(|done| done.load(Ordering::SeqCst))
            {
                tokio::task::yield_now().await;
            }
        }
        let selected = if kind == 5 {
            match call {
                Syscall::Sendto(send) => send.with_size(send.size() + 1).into(),
                _ => unreachable!(),
            }
        } else {
            call
        };
        if kind == 6 {
            let original = guest.regs().await;
            let mut changed = original;
            changed.r12 ^= 1;
            guest.set_regs(changed).await?;
            guest.set_regs(original).await?;
        }
        let mut join = Box::pin(guest.join_followed_observation_timers(selected));
        if kind != 1 {
            std::future::poll_fn(|cx| {
                assert!(join.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            if matches!(kind, 5..=7) {
                return std::future::pending().await;
            }
            probe.pending_checks.fetch_add(1, Ordering::SeqCst);
            if kind == 4 {
                drop(join);
                assert!(session.is_failed());
                probe.cancelled.store(true, Ordering::SeqCst);
                return Ok(if nr == Sysno::sendto { 8 } else { 0 });
            }
            probe.allow[first].store(true, Ordering::SeqCst);
            if matches!(kind, 3 | 8) {
                return join.await.map(|_| panic!("canceled peer join succeeded"));
            }
            if kind == 10 {
                while !probe.publication_reached.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
                probe.allow[1 - first].store(true, Ordering::SeqCst);
                while !probe.finished[1 - first].load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
                {
                    let h = session.source_cohort.0.lock().unwrap();
                    assert!(
                        h.tasks.values().all(Task::quiescent),
                        "the old physical predicate alone would admit"
                    );
                    assert_eq!(
                        h.tasks
                            .values()
                            .filter(|task| task.receive_timer.as_ref().is_some_and(
                                |timer| matches!(
                                    *timer.progress.lock().unwrap(),
                                    ReceiveTimerProgress::PendingPublication { .. }
                                )
                            ))
                            .count(),
                        1
                    );
                }
                assert!(
                    matches!(member.acquire(), Err(safeptrace::Errno::EBUSY)),
                    "unpublished real restoration cannot issue a hold"
                );
                std::future::poll_fn(|cx| {
                    assert!(join.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                })
                .await;
                probe.pending_checks.fetch_add(1, Ordering::SeqCst);
                probe.publication_release.store(true, Ordering::SeqCst);
            }
            if kind == 2 {
                while !probe.publication_reached.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
                std::future::poll_fn(|cx| {
                    assert!(
                        join.as_mut().poll(cx).is_pending(),
                        "restoration without publication is not complete"
                    );
                    Poll::Ready(())
                })
                .await;
                probe.pending_checks.fetch_add(1, Ordering::SeqCst);
                probe.publication_release.store(true, Ordering::SeqCst);
            }
            while !probe.finished[first].load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            if kind != 10 {
                std::future::poll_fn(|cx| {
                    assert!(
                        join.as_mut().poll(cx).is_pending(),
                        "one of two restores is insufficient"
                    );
                    Poll::Ready(())
                })
                .await;
                probe.pending_checks.fetch_add(1, Ordering::SeqCst);
                probe.allow[1 - first].store(true, Ordering::SeqCst);
            }
            if kind == 9 {
                while !probe
                    .finished
                    .iter()
                    .all(|done| done.load(Ordering::SeqCst))
                {
                    tokio::task::yield_now().await;
                }
                let intervening = member.acquire().unwrap();
                intervening.validate().unwrap();
                drop(intervening);
            }
        }
        join.await?;
        assert!(
            probe
                .finished
                .iter()
                .all(|done| done.load(Ordering::SeqCst))
        );
        let hold = member.acquire().unwrap();
        hold.validate().unwrap();
        assert_eq!(hold.controls.len(), 3);
        drop(hold);
        probe.joined.store(true, Ordering::SeqCst);
        while !probe
            .outputs
            .iter()
            .all(|ready| ready.load(Ordering::SeqCst))
        {
            tokio::task::yield_now().await;
        }
        probe.done.store(true, Ordering::SeqCst);
        Ok(if nr == Sysno::sendto { 8 } else { 0 })
    }
    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        global: &Global,
        _: &mut Option<usize>,
        _: ExitStatus,
    ) {
        let session = global.0.session.lock().unwrap().upgrade().unwrap();
        let owner = session
            .tree
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|t| t.tid == tid)
            .unwrap()
            .clone();
        global.0.terminals.lock().unwrap().push(owner);
    }
}

async fn case(parent: u8, first: usize, kind: u8) {
    let mut command =
        reverie::process::Command::new(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
    command.arg(if parent == 0 {
        "original-poll-join"
    } else {
        "original-poll-mixed"
    });
    let tracer = crate::TracerBuilder::<PollJoiner>::new(command)
        .config((parent, first, kind))
        .spawn()
        .await
        .unwrap();
    let (session, global) = tracer.followed_source_test_context();
    let probe = Arc::clone(&global.0);
    *probe.session.lock().unwrap() = Arc::downgrade(&session);
    probe.kind.store(kind as usize, Ordering::SeqCst);
    probe.first.store(first, Ordering::SeqCst);
    ACTIVE.with(|active| *active.borrow_mut() = Arc::downgrade(&probe));
    drop(global);
    let outcome = tokio::time::timeout(Duration::from_secs(3), tracer.wait_completion())
        .await
        .unwrap();
    let crate::ToolRunOutcome::Complete(completed) = outcome else {
        panic!("timer join cleanup unconfirmed");
    };
    let success = kind <= 2 || kind == 10;
    assert_eq!(completed.result.is_ok(), success, "{:?}", completed.result);
    assert_eq!(probe.joined.load(Ordering::SeqCst), success);
    assert_eq!(probe.done.load(Ordering::SeqCst), success);
    if success {
        assert!(matches!(completed.result, Ok(ExitStatus::Exited(0))));
        assert_eq!(
            probe.pending_checks.load(Ordering::SeqCst),
            if kind == 1 {
                0
            } else if kind == 2 {
                3
            } else {
                2
            }
        );
        for index in 0..2 {
            assert_eq!(probe.entered[index].load(Ordering::SeqCst), 1);
            assert_eq!(probe.returned[index].load(Ordering::SeqCst), 1);
        }
    }
    if matches!(kind, 3 | 4 | 8) {
        assert!(probe.cancelled.load(Ordering::SeqCst));
    }
    let owners = std::mem::take(&mut *probe.terminals.lock().unwrap());
    assert_eq!(owners.len(), 3);
    for owner in owners {
        assert!(owner.receive_scratch.lock().unwrap().is_none());
        let worker = owner
            .terminal
            .take_final_test_worker()
            .expect("original notifier");
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
async fn native_original_poll_join_two_poll_and_mixed_receive_both_orders() {
    for mixed in 0..2 {
        for first in 0..2 {
            case(mixed, first, 0).await;
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_original_poll_join_publication_gap_and_direct_acquire_refuse() {
    for first in 0..2 {
        for kind in [2, 10] {
            case(0, first, kind).await;
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_original_poll_join_cancellation_unmarked_and_foreign_revision_refuse() {
    for kind in [3, 4, 7, 8, 9] {
        case(0, 0, kind).await;
    }
}
