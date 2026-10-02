/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Genuine Command proposal. COMPILE ONLY in703; Main owns native admission.
use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tool;
use reverie::process::ExitStatus;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

use super::*;

const SUCCESS: u8 = 0;
const ERROR: u8 = 1;
const CANCEL: u8 = 2;
const DROP: u8 = 3;

#[derive(Default)]
struct Probe {
    mode: u8,
    session: Mutex<Option<Arc<super::super::FatalSession>>>,
    arrived: AtomicUsize,
    first_read: AtomicBool,
    callback_entered: AtomicBool,
    pending_refused: AtomicBool,
    release: AtomicBool,
    callbacks: AtomicUsize,
    initial: Mutex<Vec<u8>>,
    fresh: Mutex<Option<Result<Vec<u8>, String>>>,
    old_stop: Mutex<Option<ControlStop>>,
    revision: Mutex<Option<u64>>,
    terminals: Mutex<Vec<Arc<super::super::FatalTaskStop>>>,
}
thread_local! {
    static ACTIVE: RefCell<Option<Arc<Probe>>> = const { RefCell::new(None) };
}
fn probe() -> Arc<Probe> {
    ACTIVE.with(|slot| slot.borrow().as_ref().unwrap().clone())
}
pub(super) fn abandon_completion() -> bool {
    ACTIVE.with(|slot| slot.borrow().as_ref().is_some_and(|p| p.mode == DROP))
}

#[derive(Default)]
struct Global;
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = ();
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: reverie::Pid, _: ()) {}
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct State {
    child: bool,
    terminal: bool,
}
#[derive(Default)]
struct Reader;
#[reverie::tool]
impl Tool for Reader {
    type GlobalState = Global;
    type ThreadState = State;
    fn subscriptions(_: &()) -> Subscription {
        [
            Sysno::write,
            Sysno::clone,
            Sysno::clone3,
            Sysno::exit,
            Sysno::exit_group,
        ]
        .into_iter()
        .collect()
    }
    fn observe_injected_syscalls(_: &()) -> bool {
        true
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        let child = !guest.is_root_thread();
        guest.thread_state_mut().child = child;
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        let (nr, args) = call.into_parts();
        if nr != Sysno::write || !matches!(args.arg0, 703 | 704) {
            return Ok(guest.inject(call).await?);
        }
        let p = probe();
        if args.arg0 == 703 {
            p.arrived.fetch_add(1, Ordering::SeqCst);
            while p.arrived.load(Ordering::SeqCst) != 2 {
                tokio::task::yield_now().await;
            }
            if guest.is_root_thread() {
                let session = p.session.lock().unwrap().as_ref().unwrap().clone();
                *p.terminals.lock().unwrap() = session.tree.lock().unwrap().tasks.clone();
                assert_eq!(p.terminals.lock().unwrap().len(), 2);
                let bytes = guest
                    .stage_followed_source(args.arg1, args.arg2, Box::new(()))
                    .await
                    .map_err(|error| anyhow::anyhow!("initial real source: {error:?}"))?;
                *p.initial.lock().unwrap() = bytes;
                {
                    let mut h = session.source_cohort.0.lock().unwrap();
                    let root = h
                        .tasks
                        .values_mut()
                        .find(|task| matches!(task.origin, Origin::Command))
                        .unwrap();
                    // Move the real non-Clone witness, as the existing cohort
                    // stale-stop test does. No source can use this missing
                    // observer stop before the next original resume/wait.
                    *p.old_stop.lock().unwrap() = root.stop.take();
                    *p.revision.lock().unwrap() = Some(h.revision);
                    assert!(h.read_open() && h.hold.is_none());
                }
                p.first_read.store(true, Ordering::SeqCst);
            } else {
                while !p.first_read.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
            }
        } else {
            assert!(guest.is_root_thread());
            // The fixture got here only after its actual clear-TID/futex join.
            while !p.callback_entered.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            let pending = guest
                .stage_followed_source(args.arg1, args.arg2, Box::new(()))
                .await;
            assert!(matches!(
                pending,
                Err(reverie::syscalls::NativeUserReadError::Refused(
                    reverie::syscalls::NativeUserReadRefusal::TargetState(
                        safeptrace::Errno::ESTALE
                    )
                ))
            ));
            assert!(
                p.old_stop
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .validate_current()
                    .is_err()
            );
            p.pending_refused.store(true, Ordering::SeqCst);
            if p.mode != CANCEL {
                p.release.store(true, Ordering::SeqCst);
            }
            let session = p.session.lock().unwrap().as_ref().unwrap().clone();
            // Scheduling observation only. It cannot issue source authority or
            // consume/replace the session's retained actual JoinHandle.
            loop {
                let done = {
                    let joins = session.joins.lock().unwrap();
                    assert_eq!(joins.len(), 1);
                    joins[0].is_finished()
                };
                if done {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                p.old_stop
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .validate_current()
                    .is_err()
            );
            assert!(
                session.source_cohort.0.lock().unwrap().revision
                    > p.revision.lock().unwrap().unwrap()
            );
            assert!(
                guest
                    .read_native_source(args.arg1, args.arg2, Box::new(()))
                    .await
                    .is_err()
            );
            // Exactly one fresh request: baseline688 records ESTALE here. Do
            // not panic until original cleanup and actual notifier joins finish.
            let result = guest
                .stage_followed_source(args.arg1, args.arg2, Box::new(()))
                .await;
            *p.fresh.lock().unwrap() = Some(result.map_err(|error| format!("{error:?}")));
        }
        Ok(args.arg2 as i64)
    }
    fn on_backend_thread_terminal(
        &self,
        _: reverie::Pid,
        _: &Global,
        state: &mut State,
        _: ExitStatus,
    ) {
        assert!(!state.terminal);
        state.terminal = true;
    }
    async fn on_exit_thread<G: GlobalRPC<Global>>(
        &self,
        _: reverie::Pid,
        _: &G,
        state: State,
        exit_status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert!(state.terminal);
        let p = probe();
        if state.child {
            assert_eq!(exit_status, ExitStatus::Exited(0));
            // Two exact original terminal owners were captured while held.
            // Root is still stopped in its callback; only the child retired.
            assert_eq!(
                p.terminals
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|owner| owner.terminal.wait(Duration::ZERO))
                    .count(),
                1
            );
            p.callback_entered.store(true, Ordering::SeqCst);
            while !p.release.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            p.callbacks.fetch_add(1, Ordering::SeqCst);
            if p.mode == ERROR {
                return Err(anyhow::anyhow!("703 original exit callback error").into());
            }
        }
        Ok(())
    }
}

async fn case(mode: u8) {
    let fixture = std::path::PathBuf::from(
        std::env::var_os("COHORT_BRIDGE_FIXTURE")
            .expect("Main must bind and admit the compile-only703 fixture"),
    );
    assert!(fixture.is_absolute());
    let p = Arc::new(Probe {
        mode,
        ..Default::default()
    });
    ACTIVE.with(|slot| assert!(slot.replace(Some(p.clone())).is_none()));
    assert_eq!(abandon_completion(), mode == DROP);
    let mut command = reverie::process::Command::new(fixture);
    command.arg("source-retirement-703");
    let tracer = crate::TracerBuilder::<Reader>::new(command)
        .spawn()
        .await
        .unwrap();
    let (session, global) = tracer.followed_source_test_context();
    drop(global);
    *p.session.lock().unwrap() = Some(session.clone());
    let terminate = tracer.termination_handle().unwrap();
    let completion = tracer.wait_completion();
    futures::pin_mut!(completion);
    if mode == CANCEL {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !p.pending_refused.load(Ordering::SeqCst) {
                tokio::select! {
                    _ = &mut completion => panic!("completion before paused consuming callback"),
                    _ = tokio::time::sleep(Duration::from_millis(1)) => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(
            terminate
                .terminate(anyhow::anyhow!("703 cancellation inside consuming callback").into())
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut completion)
                .await
                .is_err()
        );
        assert!(!session.source_cohort.0.lock().unwrap().read_open());
        p.release.store(true, Ordering::SeqCst);
    }
    let outcome = tokio::time::timeout(Duration::from_secs(3), &mut completion)
        .await
        .unwrap();
    let crate::ToolRunOutcome::Complete(completed) = outcome else {
        panic!("original cleanup remains pending");
    };
    let mut checks = vec![
        completed.result.is_err() == matches!(mode, ERROR | CANCEL),
        p.initial.lock().unwrap().as_slice() == b"root-703",
        p.pending_refused.load(Ordering::SeqCst),
        p.callbacks.load(Ordering::SeqCst) == 1,
        session.source_jobs.pending_jobs() == 0,
    ];
    if !matches!(mode, ERROR | CANCEL) {
        checks.push(matches!(completed.result, Ok(ExitStatus::Exited(0))));
    }
    let terminals = p.terminals.lock().unwrap().clone();
    for owner in &terminals {
        let terminal = &owner.terminal;
        let handle = terminal
            .take_final_test_worker()
            .expect("original notifier OS handle");
        tokio::time::timeout(Duration::from_secs(1), async {
            while !handle.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        checks.push(handle.join().is_ok() && terminal.final_test_activity().2);
    }
    let fresh = p.fresh.lock().unwrap().clone();
    println!("SOURCE_RETIREMENT_703 mode={mode} fresh={fresh:?} checks={checks:?}");
    ACTIVE.with(|slot| {
        slot.borrow_mut().take();
    });
    assert!(
        checks.into_iter().all(|check| check),
        "original ownership and callback oracle"
    );
    match mode {
        SUCCESS => assert_eq!(
            fresh,
            Some(Ok(b"root-703".to_vec())),
            "fresh request after real child completion (baseline source-closed boundary)"
        ),
        DROP => assert!(
            fresh
                .as_ref()
                .is_some_and(|result| result.as_ref().is_err_and(|error| error.contains("ESTALE")))
        ),
        ERROR | CANCEL => assert!(!fresh.as_ref().is_some_and(Result::is_ok)),
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_child_retirement_fresh_source_and_callback_negatives() {
    case(SUCCESS).await;
    case(ERROR).await;
    case(CANCEL).await;
    case(DROP).await;
}
