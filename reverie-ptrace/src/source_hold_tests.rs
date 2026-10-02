/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Pure predicate/refusal controls and a separately admitted native proposal.
use super::*;

#[test]
fn pure_stopped_phase_requires_all_original_obligations_settled() {
    assert!(settled_phase(Life::Stopped, 0, None, true));
    for life in [
        Life::Initializing,
        Life::Executing,
        Life::Exiting,
        Life::Terminal,
    ] {
        assert!(!settled_phase(life, 0, None, true));
    }
    assert!(!settled_phase(Life::Stopped, 1, None, true));
    assert!(!settled_phase(Life::Stopped, 0, Some(0), true));
    assert!(!settled_phase(Life::Stopped, 0, None, false));
}

#[test]
fn pure_empty_or_failed_history_never_issues_a_hold() {
    let history = Arc::new(CohortHistory::default());
    // This constructed Member is a NEGATIVE pure input, not a runtime identity.
    let member = Member {
        history: history.clone(),
        index: 0,
    };
    assert!(member.acquire().is_err());
    history.0.lock().unwrap().initialized = true;
    assert!(member.acquire().is_err());
    history.fail();
    assert!(member.acquire().is_err());
    assert!(history.0.lock().unwrap().failed);
}

#[test]
fn pure_shutdown_cannot_clear_an_outstanding_interval() {
    let history = CohortHistory::default();
    let ticket = Arc::new(());
    history.0.lock().unwrap().hold = Some(ticket.clone());
    assert_eq!(history.before_group_signal(), Err(safeptrace::Errno::EBUSY));
    let h = history.0.lock().unwrap();
    assert!(h.failed);
    assert!(Arc::ptr_eq(h.hold.as_ref().unwrap(), &ticket));
    // No ControlStop or physical hold was constructed by this state test.
}

#[test]
fn pure_terminal_callback_debt_does_not_reopen_after_compaction() {
    let mut h = History {
        initialized: true,
        ..Default::default()
    };
    assert!(h.read_open());
    h.source_closed = true;
    h.tasks.clear();
    assert!(!h.read_open());
    h.advance();
    assert!(!h.read_open());
}

#[cfg(all(cohort_final_test, target_arch = "x86_64"))]
pub(crate) use native::prepare;

#[cfg(all(cohort_final_test, target_arch = "x86_64"))]
mod native {
    use std::cell::RefCell;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use reverie::GlobalTool;
    use reverie::Guest;
    use reverie::Subscription;
    use reverie::Tool;
    use reverie::syscalls::Syscall;
    use reverie::syscalls::SyscallInfo;

    use super::*;

    struct Hook {
        history: Option<Arc<CohortHistory>>,
        entered: Arc<AtomicBool>,
        release: Option<std::sync::mpsc::Receiver<()>>,
    }
    thread_local! {
        static ACTIVE: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }
    pub(crate) fn prepare(
        plan: safeptrace::FollowedSourceReadPlan,
        member: &Member,
    ) -> safeptrace::FollowedSourceReadPlan {
        ACTIVE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(hook) = slot.as_mut() else {
                return plan;
            };
            hook.history = Some(Arc::clone(&member.history));
            plan.pause_before_read(hook.entered.clone(), hook.release.take().unwrap())
        })
    }
    #[derive(Default)]
    struct ProbeState {
        arrived: AtomicUsize,
        done: AtomicBool,
        address: AtomicUsize,
        bytes: Mutex<Vec<u8>>,
        drops: Arc<AtomicUsize>,
    }
    #[derive(Default)]
    struct Global(Arc<ProbeState>);
    impl std::ops::Deref for Global {
        type Target = ProbeState;
        fn deref(&self) -> &ProbeState {
            &self.0
        }
    }
    #[reverie::global_tool]
    impl GlobalTool for Global {
        type Config = bool; // select sender only; cannot issue any authority
        type Request = ();
        type Response = ();
        async fn receive_rpc(&self, _: reverie::Pid, _: ()) {}
    }
    struct Retention(Arc<AtomicUsize>);
    impl Drop for Retention {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[derive(Default)]
    struct Reader;
    #[reverie::tool]
    impl Tool for Reader {
        type GlobalState = Global;
        type ThreadState = ();
        fn subscriptions(_: &bool) -> Subscription {
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
        fn observe_injected_syscalls(_: &bool) -> bool {
            true
        }
        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            call: Syscall,
        ) -> Result<i64, reverie::Error> {
            let (nr, args) = call.into_parts();
            if nr != Sysno::write || args.arg0 != 688 {
                return Ok(guest.inject(call).await?);
            }
            guest
                .local_global_state()
                .unwrap()
                .arrived
                .fetch_add(1, Ordering::SeqCst);
            while guest
                .local_global_state()
                .unwrap()
                .arrived
                .load(Ordering::SeqCst)
                != 2
            {
                tokio::task::yield_now().await;
            }
            if guest.is_root_thread() != *guest.config() {
                let g = guest.local_global_state().unwrap();
                g.address.store(args.arg1, Ordering::SeqCst);
                let retention = Box::new(Retention(g.drops.clone()));
                // The legacy epoch stays revoked after this genuine clone.
                assert!(
                    guest
                        .read_native_source(args.arg1, args.arg2, Box::new(()))
                        .await
                        .is_err()
                );
                let bytes = guest
                    .stage_followed_source(args.arg1, args.arg2, retention)
                    .await
                    .map_err(|e| anyhow::anyhow!("followed staged read refused: {e:?}"))?;
                let g = guest.local_global_state().unwrap();
                *g.bytes.lock().unwrap() = bytes;
                g.done.store(true, Ordering::SeqCst);
            } else {
                while !guest
                    .local_global_state()
                    .unwrap()
                    .done
                    .load(Ordering::SeqCst)
                {
                    tokio::task::yield_now().await;
                }
            }
            Ok(args.arg2 as i64)
        }
    }

    async fn case(child_sender: bool, cancel: bool) {
        let fixture =
            std::path::PathBuf::from(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
        assert!(fixture.is_absolute());
        let entered = Arc::new(AtomicBool::new(false));
        let (release, gate) = std::sync::mpsc::channel();
        ACTIVE.with(|s| {
            assert!(
                s.replace(Some(Hook {
                    history: None,
                    entered: entered.clone(),
                    release: Some(gate),
                }))
                .is_none()
            )
        });
        let mut command = reverie::process::Command::new(fixture);
        command.arg("source-hold-688");
        let tracer = crate::TracerBuilder::<Reader>::new(command)
            .config(child_sender)
            .spawn()
            .await
            .unwrap();
        let (session, global) = tracer.followed_source_test_context();
        let g = Arc::clone(&global.0);
        // Completion requires the original GlobalTool Arc to have one owner.
        // Keep only independent probe data, never bypass that production gate.
        drop(global);
        let terminate = tracer.termination_handle().unwrap();
        let joined = Arc::new(AtomicBool::new(false));
        let (release_join, gate_join) = std::sync::mpsc::channel();
        session
            .source_jobs
            .pause_next_retirement(crate::task::source_jobs::RetirementPause {
                entered: joined.clone(),
                release: gate_join,
            });
        let completion = tracer.wait_completion();
        futures::pin_mut!(completion);
        let mut controls = Vec::new();
        let mut checks = Vec::new();
        for boundary in [&entered, &joined] {
            tokio::time::timeout(Duration::from_secs(3), async {
                while !boundary.load(Ordering::Acquire) {
                    tokio::select! {
                        _ = &mut completion => panic!("completion preceded held read/join boundary"),
                        _ = tokio::time::sleep(Duration::from_millis(1)) => {}
                    }
                }
            }).await.expect("actual read/join boundary unavailable");
            let history = ACTIVE.with(|s| s.borrow().as_ref().unwrap().history.clone().unwrap());
            let h = history.0.lock().unwrap();
            checks.push(!h.failed && h.tasks.len() == 2 && h.hold.is_some());
            checks.push(session.source_jobs.pending_jobs() == 1);
            checks.push(g.drops.load(Ordering::SeqCst) == 0);
            for task in h.tasks.values() {
                let (cleanup, probes) = task
                    .stop
                    .as_ref()
                    .unwrap()
                    .probe_held_controls(g.address.load(Ordering::SeqCst))
                    .unwrap();
                checks.extend(probes);
                if Arc::ptr_eq(boundary, &entered) {
                    controls.push(cleanup);
                }
            }
            drop(h);
            if Arc::ptr_eq(boundary, &entered) {
                release.send(()).unwrap();
            }
        }
        if cancel {
            checks.push(
                terminate.terminate(anyhow::anyhow!("held-read cancellation control").into()),
            );
            checks.push(
                tokio::time::timeout(Duration::from_millis(25), &mut completion)
                    .await
                    .is_err(),
            );
            checks.push(g.drops.load(Ordering::SeqCst) == 0);
            for cleanup in &controls {
                checks.push(cleanup.continue_for_cleanup() == Err(safeptrace::Errno::EBUSY));
            }
        }
        release_join.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), &mut completion)
            .await
            .unwrap();
        let crate::ToolRunOutcome::Complete(completed) = result else {
            panic!("original cleanup remains unconfirmed");
        };
        checks.push(completed.result.is_err() == cancel);
        if !cancel {
            checks.push(matches!(
                completed.result,
                Ok(reverie::process::ExitStatus::Exited(0))
            ));
        }
        let bytes = g.bytes.lock().unwrap().clone();
        checks.push(if cancel {
            bytes.is_empty()
        } else {
            bytes
                == if child_sender {
                    b"child688"
                } else {
                    b"root-688"
                }
        });
        checks.push(g.drops.load(Ordering::SeqCst) == 1);
        checks.push(session.source_jobs.pending_jobs() == 0);
        // Same original-worker transfer/join mechanism as the startup controls.
        for cleanup in controls {
            if !cancel {
                checks.push(matches!(
                    cleanup.observed_exit_status(),
                    Ok(Some(reverie::process::ExitStatus::Exited(0)))
                ));
            }
            let handle = cleanup
                .take_final_test_worker()
                .expect("original notifier worker");
            tokio::time::timeout(Duration::from_secs(1), async {
                while !handle.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            checks.push(handle.join().is_ok() && cleanup.final_test_activity().2);
        }
        ACTIVE.with(|s| {
            s.borrow_mut().take();
        });
        println!("SOURCE_HOLD_688 child_sender={child_sender} cancel={cancel} checks={checks:?}");
        assert!(
            checks.into_iter().all(|x| x),
            "exact source/read/join exclusion oracle"
        );
    }

    // COMPILE ONLY in this packet. Do not execute without new Main admission.
    #[tokio::test(flavor = "current_thread")]
    async fn native_two_followed_tasks_hold_read_and_join() {
        case(false, false).await;
        case(true, false).await;
        case(false, true).await;
    }
}
