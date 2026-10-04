/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Actual original MADV_DONTNEED and source cuts, including cleanup before the
//! required-success oracle: https://github.com/rrnewton/reverie/issues/926.
use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reverie::GlobalTool;
use reverie::Guest;
use reverie::InjectedSyscallEvent;
use reverie::Subscription;
use reverie::Tool;
use reverie::process::ExitStatus;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

use super::*;

#[derive(Default)]
struct Probe {
    session: Mutex<std::sync::Weak<super::super::FatalSession>>,
    page: AtomicUsize,
    initial_done: AtomicBool,
    advised: AtomicBool,
    release: AtomicBool,
    initial: Mutex<Option<Result<Vec<u8>, String>>>,
    fresh: Mutex<Option<Result<Vec<u8>, String>>>,
    retired: Mutex<Option<Result<Vec<u8>, String>>>,
    injected: Mutex<Vec<(SyscallArgs, InjectedSyscallEvent)>>,
    terminal: Mutex<Vec<Arc<super::super::FatalTaskStop>>>,
    before_exit: AtomicBool,
    native_checks: AtomicUsize,
    proof_checks: AtomicUsize,
    omitted: AtomicBool,
    mode: u8,
}
#[derive(Default)]
struct Global(Arc<Probe>);
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = u8;
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: reverie::Pid, _: ()) {}
}
#[derive(Default)]
struct Reader;
#[reverie::tool]
impl Tool for Reader {
    type GlobalState = Global;
    type ThreadState = ();
    fn subscriptions(mode: &u8) -> Subscription {
        [
            Sysno::write,
            Sysno::madvise,
            Sysno::clone,
            Sysno::clone3,
            Sysno::exit,
            Sysno::exit_group,
        ]
        .into_iter()
        .filter(|nr| *mode != 1 || *nr != Sysno::madvise)
        .collect()
    }
    fn observe_injected_syscalls(_: &u8) -> bool {
        true
    }
    fn observe_injected_syscall_preparation(_: &u8) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        _: reverie::Pid,
        global: &Global,
        _: &mut (),
        nr: Sysno,
        args: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        if nr == Sysno::madvise {
            global.0.injected.lock().unwrap().push((args, event));
        }
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        let (nr, args) = call.into_parts();
        if nr == Sysno::madvise && *guest.config() != 0 {
            let mode = *guest.config();
            if mode == 2 {
                assert_eq!(guest.inject(call).await?, 0);
            }
            let saved = if mode == 5 {
                let original = guest.regs().await;
                let mut changed = original;
                changed.rsp += 8;
                guest.set_regs(changed).await?;
                Some(original)
            } else {
                None
            };
            let actual = match mode {
                3 => Syscall::Other(
                    nr,
                    SyscallArgs::new(args.arg0, 0, args.arg2, args.arg3, args.arg4, args.arg5),
                ),
                4 => Syscall::Other(
                    nr,
                    SyscallArgs::new(
                        args.arg0,
                        args.arg1,
                        libc::MADV_NORMAL as usize,
                        args.arg3,
                        args.arg4,
                        args.arg5,
                    ),
                ),
                _ => call,
            };
            let result = guest.inject(actual).await;
            if let Some(original) = saved {
                guest.set_regs(original).await?;
            }
            return Ok(result?);
        }
        if nr != Sysno::write || !matches!(args.arg0, 926..=928) {
            return Ok(guest.inject(call).await?);
        }
        let p = Arc::clone(&guest.local_global_state().unwrap().0);
        if args.arg0 == 926 && !guest.is_root_thread() {
            assert_eq!(args.arg2, 4096);
            assert_eq!(p.page.swap(args.arg1, Ordering::SeqCst), 0);
            while !p.initial_done.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            if *guest.config() == 6 {
                assert_eq!(
                    guest
                        .inject(Syscall::Other(
                            Sysno::madvise,
                            SyscallArgs::new(
                                args.arg1,
                                4096,
                                libc::MADV_DONTNEED as usize,
                                0,
                                0,
                                0
                            )
                        ))
                        .await?,
                    0
                );
            }
        } else if args.arg0 == 927 {
            assert!(!guest.is_root_thread());
            assert_eq!(args.arg1, p.page.load(Ordering::SeqCst));
            assert_eq!(args.arg2, 4096);
            p.advised.store(true, Ordering::SeqCst);
            while !p.release.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        } else if args.arg0 == 926 {
            assert!(guest.is_root_thread());
            while p.page.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
            // The source reader's unchanged finite profile allows 512 bytes;
            // the child independently verifies all 4096 zero-filled bytes.
            let address = p.page.load(Ordering::SeqCst);
            let result = guest
                .stage_followed_source(address, 512, Box::new(()))
                .await
                .map_err(|e| format!("{e:?}"));
            *p.initial.lock().unwrap() = Some(result);
            p.initial_done.store(true, Ordering::SeqCst);
            while !p.advised.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            let session = p.session.lock().unwrap().upgrade().unwrap();
            assert!(p.terminal.lock().unwrap().is_empty());
            assert_eq!(session.tree.lock().unwrap().tasks.len(), 2);
            p.before_exit.store(true, Ordering::SeqCst);
            let result = guest
                .stage_followed_source(address, 512, Box::new(()))
                .await
                .map_err(|e| format!("{e:?}"));
            *p.fresh.lock().unwrap() = Some(result);
            // Do not panic on the expected old-source refusal before original
            // child exit, consuming callbacks and real notifier joins finish.
            p.release.store(true, Ordering::SeqCst);
        } else {
            assert_eq!(args.arg0, 928);
            assert!(guest.is_root_thread());
            let session = p.session.lock().unwrap().upgrade().unwrap();
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
            let result = guest
                .stage_followed_source(args.arg1, 512, Box::new(()))
                .await
                .map_err(|e| format!("{e:?}"));
            *p.retired.lock().unwrap() = Some(result);
        }
        Ok(args.arg2 as i64)
    }
    fn on_backend_thread_terminal(
        &self,
        tid: reverie::Pid,
        global: &Global,
        _: &mut (),
        _: ExitStatus,
    ) {
        let session = global.0.session.lock().unwrap().upgrade().unwrap();
        let owner = session
            .tree
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|task| task.tid == tid)
            .unwrap()
            .clone();
        global.0.terminal.lock().unwrap().push(owner);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_original_dontneed_preserves_fresh_source_before_child_exit() {
    let mut command =
        reverie::process::Command::new(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
    command.arg("source-madvise-926");
    let tracer = crate::TracerBuilder::<Reader>::new(command)
        .spawn()
        .await
        .unwrap();
    let (session, global) = tracer.followed_source_test_context();
    let p = Arc::clone(&global.0);
    *p.session.lock().unwrap() = Arc::downgrade(&session);
    drop(global);
    let outcome = tokio::time::timeout(Duration::from_secs(3), tracer.wait_completion())
        .await
        .unwrap();
    let crate::ToolRunOutcome::Complete(completed) = outcome else {
        panic!("original madvise fixture cleanup remains pending");
    };
    let owners = std::mem::take(&mut *p.terminal.lock().unwrap());
    assert_eq!(owners.len(), 2);
    for owner in owners {
        let worker = owner
            .terminal
            .take_final_test_worker()
            .expect("original notifier owner");
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
    assert!(
        matches!(completed.result, Ok(ExitStatus::Exited(0))),
        "{:?}",
        completed.result
    );
    assert_eq!(session.source_jobs.pending_jobs(), 0);
    assert!(p.before_exit.load(Ordering::SeqCst));
    let events = p.injected.lock().unwrap();
    assert_eq!(events.len(), 3);
    let expected = SyscallArgs::new(
        p.page.load(Ordering::SeqCst),
        4096,
        libc::MADV_DONTNEED as usize,
        0,
        0,
        0,
    );
    // Only the first three syscall operands are specified by this libc syscall
    // invocation. The complete actual tuple must agree across all three events.
    let actual = events[0].0;
    assert_eq!(
        (actual.arg0, actual.arg1, actual.arg2),
        (expected.arg0, expected.arg1, expected.arg2)
    );
    assert_eq!(
        events.as_slice(),
        &[
            (actual, InjectedSyscallEvent::Prepared),
            (actual, InjectedSyscallEvent::Entered),
            (actual, InjectedSyscallEvent::Returned(0)),
        ]
    );
    assert_eq!(*p.initial.lock().unwrap(), Some(Ok(vec![0x5a; 512])));
    let fresh = p.fresh.lock().unwrap();
    let retired = p.retired.lock().unwrap();
    println!(
        "DONTNEED_926 fresh={:?} retired={:?} cleanup=complete",
        fresh.as_ref().map(|r| r.as_ref().map(Vec::len)),
        retired.as_ref().map(|r| r.as_ref().map(Vec::len))
    );
    assert_eq!(
        *fresh,
        Some(Ok(vec![0; 512])),
        "real DONTNEED must permit a fresh source before any child exit"
    );
    assert_eq!(
        *retired,
        Some(Ok(vec![0; 512])),
        "real child retirement must preserve a fresh source"
    );
}

thread_local! {
    static ACTIVE: RefCell<Option<Arc<Probe>>> = const { RefCell::new(None) };
}
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        ACTIVE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
fn active() -> Option<Arc<Probe>> {
    ACTIVE.with(|slot| slot.borrow().clone())
}

pub(in crate::task) fn original_proof(
    proof: &super::super::source_observation::OriginalDontneed,
    task: &Stopped,
    entry: safeptrace::SyscallEntry,
) {
    let Some(p) = active() else {
        return;
    };
    if !proof.matches(task, entry) {
        return;
    }
    // Mutate the independently retained actual entry comparison, without
    // constructing a task, stop, admission or successful syscall result.
    for field in 0..10 {
        let mut changed = entry;
        match field {
            0 => changed.arch ^= 1,
            1 => changed.number ^= 1,
            2 => changed.instruction_pointer ^= 1,
            3 => changed.stack_pointer ^= 1,
            field => changed.arguments[field - 4] ^= 1,
        }
        assert!(!proof.matches(task, changed), "entry mutant {field}");
    }
    p.proof_checks.fetch_add(1, Ordering::SeqCst);
}
pub(super) fn native_registered(member: &Member, number: u64, syscall: Sysno) {
    let Some(p) = active() else {
        return;
    };
    if syscall != Sysno::madvise {
        return;
    }
    {
        let h = member.history.0.lock().unwrap();
        if h.failed {
            return;
        }
        let task = &h.tasks[&member.index];
        assert_eq!(task.invocation, Some(number));
        assert!(task.operations[&number].original_source.is_classified());
    }
    assert!(
        matches!(member.acquire(), Err(safeptrace::Errno::EBUSY)),
        "an actual registered native effect is not a source cut"
    );
    p.native_checks.fetch_add(1, Ordering::SeqCst);
}
pub(super) fn abandon_restoration(owner: &NativeOperation, raw: i64) -> bool {
    active().is_some_and(|p| {
        if p.mode != 8 || owner.syscall != Sysno::madvise {
            return false;
        }
        assert_eq!(raw, 0);
        assert!(!p.omitted.swap(true, Ordering::SeqCst));
        // Explicit injected omission after actual native return. Dropping the
        // real NativeOperation must retain failure, never mint completion.
        true
    })
}
async fn control_case(mode: u8) {
    let _reset = Reset;
    let p = Arc::new(Probe {
        mode,
        ..Default::default()
    });
    ACTIVE.with(|slot| assert!(slot.replace(Some(Arc::clone(&p))).is_none()));
    let mut command =
        reverie::process::Command::new(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
    command
        .arg("source-madvise-controls-926")
        .arg(mode.to_string());
    let tracer = crate::TracerBuilder::<Reader>::new(command)
        .config(mode)
        .spawn()
        .await
        .unwrap();
    let (session, global) = tracer.followed_source_test_context();
    // Global is created by the tracer. Its real Tool observations are separate
    // from the thread-local hook's configured fault and invariant counters.
    let actual = Arc::clone(&global.0);
    // No hook constructs a task, stop, wait, source result or completion.
    *actual.session.lock().unwrap() = Arc::downgrade(&session);
    drop(global);
    let outcome = tokio::time::timeout(Duration::from_secs(3), tracer.wait_completion())
        .await
        .unwrap();
    let crate::ToolRunOutcome::Complete(completed) = outcome else {
        panic!("control cleanup pending");
    };
    let owners = std::mem::take(&mut *actual.terminal.lock().unwrap());
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
    assert!(
        matches!(completed.result, Ok(ExitStatus::Exited(0))),
        "mode={mode}: {:?}",
        completed.result
    );
    assert_eq!(session.source_jobs.pending_jobs(), 0);
    assert!(actual.before_exit.load(Ordering::SeqCst));
    assert_eq!(*actual.initial.lock().unwrap(), Some(Ok(vec![0x5a; 512])));
    let fresh = actual.fresh.lock().unwrap();
    let retired = actual.retired.lock().unwrap();
    println!(
        "DONTNEED_CONTROL mode={mode} fresh={:?} retired={:?} native_checks={} proof_checks={}",
        fresh.as_ref().map(|r| r.as_ref().map(Vec::len)),
        retired.as_ref().map(|r| r.as_ref().map(Vec::len)),
        p.native_checks.load(Ordering::SeqCst),
        p.proof_checks.load(Ordering::SeqCst)
    );
    if matches!(mode, 1 | 7) {
        assert_eq!(*fresh, Some(Ok(vec![0; 512])));
        assert_eq!(*retired, Some(Ok(vec![0; 512])));
    } else {
        assert_eq!(
            fresh.as_ref().unwrap().as_ref().unwrap_err(),
            "Refused(TargetState(ESTALE))"
        );
        assert_eq!(
            retired.as_ref().unwrap().as_ref().unwrap_err(),
            "Refused(TargetState(ESTALE))"
        );
    }
    let events = actual.injected.lock().unwrap();
    if mode == 1 {
        assert!(events.is_empty());
    }
    if mode == 7 {
        assert_eq!(events.len(), 3);
        let args = events[0].0;
        assert_eq!(args.arg1, 12288);
        assert_eq!(
            events.as_slice(),
            &[
                (args, InjectedSyscallEvent::Prepared),
                (args, InjectedSyscallEvent::Entered),
                (args, InjectedSyscallEvent::Returned(-(libc::ENOMEM as i64)))
            ]
        );
    }
    if matches!(mode, 1 | 2 | 7 | 8) {
        assert_eq!(p.native_checks.load(Ordering::SeqCst), 1);
        assert!(p.proof_checks.load(Ordering::SeqCst) > 0);
    }
    assert_eq!(p.omitted.load(Ordering::SeqCst), mode == 8);
}
#[tokio::test(flavor = "current_thread")]
async fn native_original_dontneed_unsubscribed_has_real_completion() {
    control_case(1).await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_original_dontneed_duplicate_and_private_cannot_reuse_proof() {
    control_case(2).await;
    control_case(6).await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_original_dontneed_changed_tuple_advice_and_frame_refuse() {
    control_case(3).await;
    control_case(4).await;
    control_case(5).await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_original_dontneed_partial_enomem_preserves_effects_and_errno() {
    control_case(7).await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_original_dontneed_missing_restoration_retains_failure() {
    control_case(8).await;
}
