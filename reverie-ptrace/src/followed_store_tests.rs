/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
//! Native backend/copy controls, not network syscall qualification.
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
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
    callbacks: AtomicUsize,
    done: AtomicBool,
    session: Mutex<Weak<super::super::FatalSession>>,
    terminals: Mutex<Vec<Arc<crate::tracer::FatalTaskStop>>>,
    outcomes: Mutex<Vec<O>>,
}
#[derive(Default)]
struct Global(Arc<Probe>);
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = (Option<usize>, bool);
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
#[derive(Debug)]
struct ControlledStoreUnwind;

#[derive(Default)]
struct Receiver;
#[reverie::tool]
impl Tool for Receiver {
    type GlobalState = Global;
    type ThreadState = ();
    fn subscriptions(_: &(Option<usize>, bool)) -> Subscription {
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
        if let Some(index) = guest.config().0 {
            let mut regs = guest.regs().await;
            let field = match index {
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
            *field ^= 1;
            guest.set_regs(regs).await?;
        }
        let panic_after = guest.config().1;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            guest.with_followed_store(call, |writer| {
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
                assert_eq!(writer.validate_context(), Ok(()));
                let outcome = writer.store(b"abcd");
                // The existing Call analogue receives the actual result while all
                // physical custody is still held, before callback return.
                probe.outcomes.lock().unwrap().push(outcome);
                assert_eq!(
                    outcome,
                    O::Attempted {
                        raw: Ok(4),
                        postcheck: Ok(())
                    }
                );
                assert_eq!(
                    writer.validate_context(),
                    Err(reverie::syscalls::NativeUserStoreRefusal::Evidence(
                        reverie::syscalls::NativeUserReadRefusal::TargetState(
                            safeptrace::Errno::EALREADY
                        )
                    ))
                );
                assert!(matches!(writer.store(b"WXYZ"), O::Refused(_)));
                if panic_after {
                    std::panic::panic_any(ControlledStoreUnwind);
                }
            })
        }));
        let result = match result {
            Ok(result) => {
                assert!(!panic_after);
                result
            }
            Err(payload) if panic_after && payload.is::<ControlledStoreUnwind>() => Ok(()),
            Err(payload) => std::panic::resume_unwind(payload),
        };
        result
            .map_err(|error| reverie::Error::Tool(anyhow::anyhow!("store refused: {error:?}")))?;
        let again = guest.with_followed_store(call, |_| {
            probe.callbacks.fetch_add(1, Ordering::SeqCst);
            panic!("fresh callback must not renew the original entry's store");
        });
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
async fn case(child: bool, recv: bool, changed: Option<usize>, panic_after: bool) {
    let mut command =
        reverie::process::Command::new(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
    command.arg(match (child, recv) {
        (false, false) => "store-read-root",
        (true, false) => "store-read-child",
        (false, true) => "store-recv-root",
        (true, true) => "store-recv-child",
    });
    let tracer = crate::TracerBuilder::<Receiver>::new(command)
        .config((changed, panic_after))
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
        panic!("store cleanup unconfirmed")
    };
    assert_eq!(completed.result.is_err(), changed.is_some());
    assert_eq!(
        probe.callbacks.load(Ordering::SeqCst),
        usize::from(changed.is_none())
    );
    assert_eq!(
        probe.outcomes.lock().unwrap().len(),
        usize::from(changed.is_none())
    );
    if changed.is_none() {
        assert!(matches!(completed.result, Ok(ExitStatus::Exited(0))));
    }
    let owners = std::mem::take(&mut *probe.terminals.lock().unwrap());
    assert_eq!(owners.len(), 2);
    for owner in owners {
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
async fn native_followed_store_root_and_child_scalar_destinations() {
    for child in [false, true] {
        for recv in [false, true] {
            case(child, recv, None, false).await;
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_followed_store_changed_original_context_refuses_before_callback() {
    for field in 0..9 {
        case(false, false, Some(field), false).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_followed_store_unwind_cannot_renew_original_entry() {
    case(false, false, None, true).await;
    case(true, true, None, true).await;
}
