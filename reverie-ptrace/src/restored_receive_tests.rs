/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
//! Real helper, destination and cancellation controls; no network qualification.
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
use reverie::syscalls::NativeUserStoreOutcome as O;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

use super::*;

#[derive(Default)]
struct Probe {
    arrived: AtomicUsize,
    entered: AtomicUsize,
    returned: AtomicUsize,
    callbacks: AtomicUsize,
    done: AtomicBool,
    cancelled: AtomicBool,
    session: Mutex<Weak<super::super::FatalSession>>,
    terminals: Mutex<Vec<Arc<crate::tracer::FatalTaskStop>>>,
    outcomes: Mutex<Vec<O>>,
}
#[derive(Default)]
struct Global(Arc<Probe>);
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = (u8, Option<(u8, usize)>);
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
#[derive(Default)]
struct Receiver;
#[reverie::tool]
impl Tool for Receiver {
    type GlobalState = Global;
    type ThreadState = ();
    fn subscriptions(_: &(u8, Option<(u8, usize)>)) -> Subscription {
        [
            Sysno::read,
            Sysno::recvfrom,
            Sysno::write,
            Sysno::clone,
            Sysno::clone3,
            Sysno::exit,
            Sysno::exit_group,
        ]
        .into_iter()
        .collect()
    }
    fn observe_injected_syscalls(_: &(u8, Option<(u8, usize)>)) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        _: Pid,
        global: &Global,
        _: &mut (),
        nr: Sysno,
        _: SyscallArgs,
        event: reverie::InjectedSyscallEvent,
    ) {
        if nr == Sysno::ppoll {
            match event {
                reverie::InjectedSyscallEvent::Entered => {
                    global.0.entered.fetch_add(1, Ordering::SeqCst);
                }
                reverie::InjectedSyscallEvent::Returned(0) => {
                    global.0.returned.fetch_add(1, Ordering::SeqCst);
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
        let selected = matches!(nr, Sysno::read | Sysno::recvfrom) && args.arg0 == 744;
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
                tokio::task::yield_now().await;
            }
            return Ok(8);
        }
        let (kind, mutation) = *guest.config();
        if let Some((stage, field)) = mutation {
            super::super::followed_receive::test_mutation(stage, field);
        }
        let duration = Duration::from_millis(1);
        if kind == 4 {
            let session = probe.session.lock().unwrap().upgrade().unwrap();
            let mut timer = Box::pin(guest.inject_receive_observation_timer(call, duration));
            std::future::poll_fn(|cx| {
                let polled = timer.as_mut().poll(cx);
                assert!(
                    polled.is_pending(),
                    "timer must still own the original pending operation"
                );
                if probe.entered.load(Ordering::SeqCst) == 1 {
                    let h = session.source_cohort.0.lock().unwrap();
                    assert!(
                        h.tasks
                            .values()
                            .any(|t| t.life == Life::Executing && t.invocation.is_some())
                    );
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
            drop(timer);
            let owner = session
                .tree
                .lock()
                .unwrap()
                .tasks
                .iter()
                .find(|owner| owner.tid == guest.tid())
                .cloned()
                .unwrap();
            let retained = owner
                .receive_scratch
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .clone();
            assert!(retained.test_checkout_held());
            assert_eq!(
                owner.retire_receive_scratch_terminal(),
                Err(safeptrace::Errno::EBUSY)
            );
            assert!(retained.test_checkout_held());
            assert!(
                session.is_failed(),
                "dropping the real attempt must publish failure"
            );
            assert_eq!(probe.returned.load(Ordering::SeqCst), 0);
            probe.cancelled.store(true, Ordering::SeqCst);
            // This attempted Tool success must not resume the original callback.
            return Ok(4);
        }
        if kind != 3 {
            guest
                .inject_receive_observation_timer(call, duration)
                .await?;
            guest
                .inject_receive_observation_timer(call, duration)
                .await?;
            assert_eq!(probe.entered.load(Ordering::SeqCst), 2);
            assert_eq!(probe.returned.load(Ordering::SeqCst), 2);
        }
        if kind == 1 {
            assert!(guest.inject(reverie::syscalls::Getpid::new()).await? > 0);
        }
        if kind == 2 {
            let saved = guest.regs().await;
            let mut changed = saved;
            changed.r12 ^= 1;
            guest.set_regs(changed).await?;
            guest.set_regs(saved).await?;
        }
        let actual = if kind == 5 {
            match call {
                Syscall::Read(read) => read.with_len(read.len() + 1).into(),
                Syscall::Recvfrom(recv) => recv.with_len(recv.len() + 1).into(),
                _ => unreachable!(),
            }
        } else {
            call
        };
        guest
            .with_restored_followed_store(actual, |writer| {
                probe.callbacks.fetch_add(1, Ordering::SeqCst);
                let session = probe.session.lock().unwrap().upgrade().unwrap();
                let history = session.source_cohort.0.lock().unwrap();
                assert_eq!(history.tasks.len(), 2);
                assert!(history.hold.is_some());
                for task in history.tasks.values() {
                    let (_, checks) = task
                        .stop
                        .as_ref()
                        .unwrap()
                        .probe_held_controls(args.arg1)
                        .unwrap();
                    assert_eq!(checks, [true; 4]);
                }
                drop(history);
                let outcome = writer.store(b"abcd");
                probe.outcomes.lock().unwrap().push(outcome);
                assert_eq!(
                    outcome,
                    O::Attempted {
                        raw: Ok(4),
                        postcheck: Ok(())
                    }
                );
                assert!(matches!(writer.store(b"WXYZ"), O::Refused(_)));
            })
            .map_err(|e| reverie::Error::Tool(anyhow::anyhow!("restored store refused: {e:?}")))?;
        if kind == 6 {
            guest
                .inject_receive_observation_timer(call, duration)
                .await?;
        }
        let again =
            guest.with_restored_followed_store(call, |_| panic!("second writer was issued"));
        assert_eq!(
            again,
            Err(reverie::syscalls::NativeUserStoreRefusal::Evidence(
                reverie::syscalls::NativeUserReadRefusal::TargetState(safeptrace::Errno::EALREADY)
            ))
        );
        probe.done.store(true, Ordering::SeqCst);
        Ok(4)
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
}
async fn case(child: bool, recv: bool, kind: u8, mutation: Option<(u8, usize)>) {
    let mut command =
        reverie::process::Command::new(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
    command.arg(match (child, recv) {
        (false, false) => "store-read-root",
        (true, false) => "store-read-child",
        (false, true) => "store-recv-root",
        (true, true) => "store-recv-child",
    });
    let tracer = crate::TracerBuilder::<Receiver>::new(command)
        .config((kind, mutation))
        .spawn()
        .await
        .unwrap();
    let (session, global) = tracer.followed_source_test_context();
    let probe = Arc::clone(&global.0);
    *probe.session.lock().unwrap() = Arc::downgrade(&session);
    drop(global);
    let outcome = tokio::time::timeout(Duration::from_secs(3), tracer.wait_completion())
        .await
        .unwrap();
    let crate::ToolRunOutcome::Complete(completed) = outcome else {
        panic!("restored receive cleanup unconfirmed");
    };
    let success = matches!(kind, 0 | 6) && mutation.is_none();
    assert_eq!(completed.result.is_ok(), success, "{:?}", completed.result);
    assert_eq!(probe.callbacks.load(Ordering::SeqCst), usize::from(success));
    assert_eq!(probe.outcomes.lock().unwrap().len(), usize::from(success));
    if success {
        assert!(matches!(completed.result, Ok(ExitStatus::Exited(0))));
    }
    if kind == 4 {
        assert!(probe.cancelled.load(Ordering::SeqCst));
    }
    let hits = super::super::followed_receive::test_mutation_finish();
    assert_eq!(hits, usize::from(mutation.is_some()));
    let owners = std::mem::take(&mut *probe.terminals.lock().unwrap());
    assert_eq!(owners.len(), 2);
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
async fn native_restored_receive_root_child_read_recv_after_two_real_timers() {
    for child in [false, true] {
        for recv in [false, true] {
            case(child, recv, 0, None).await;
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_restored_receive_missing_wrong_original_and_aliases_refuse() {
    for kind in [1, 2, 3, 5] {
        case(false, false, kind, None).await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_restored_receive_next_timer_cannot_renew_store() {
    case(false, false, 6, None).await;
    case(true, true, 6, None).await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_restored_receive_actual_skip_entry_exit_mutations_refuse() {
    for mutation in [
        (1, 10),
        (1, 3),
        (2, 3),
        (3, 14),
        (3, 13),
        (3, 12),
        (3, 7),
        (3, 8),
        (3, 9),
        (3, 15),
        (3, 16),
        (3, 19),
        (4, 10),
        (4, 15),
        (4, 3),
        (4, 14),
        (4, 13),
        (4, 12),
        (4, 7),
        (4, 8),
        (4, 9),
        (4, 16),
        (4, 19),
        (5, 3),
    ] {
        case(false, false, 7, Some(mutation)).await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_restored_receive_dropped_executing_timer_retains_custody() {
    case(false, false, 4, None).await;
    case(true, true, 4, None).await;
}
