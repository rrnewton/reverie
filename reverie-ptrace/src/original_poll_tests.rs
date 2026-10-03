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
    post_fault: AtomicBool,
    caught: AtomicBool,
    session: Mutex<Weak<super::super::FatalSession>>,
    terminals: Mutex<Vec<Arc<crate::tracer::FatalTaskStop>>>,
    outcomes: Mutex<Vec<O>>,
}
#[derive(Default)]
struct Global(Arc<Probe>);
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = (u8, bool, Option<(u8, usize)>);
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
#[derive(Default)]
struct Poller;
#[reverie::tool]
impl Tool for Poller {
    type GlobalState = Global;
    type ThreadState = ();
    fn subscriptions(_: &(u8, bool, Option<(u8, usize)>)) -> Subscription {
        [
            Sysno::poll,
            Sysno::write,
            Sysno::clone,
            Sysno::clone3,
            Sysno::exit,
            Sysno::exit_group,
        ]
        .into_iter()
        .collect()
    }
    fn observe_injected_syscalls(_: &(u8, bool, Option<(u8, usize)>)) -> bool {
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
        let selected = nr == Sysno::poll && args.arg1 as u32 == 1;
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
        let (kind, _, mutation) = *guest.config();
        if let Some((stage, field)) = mutation {
            super::super::followed_poll::test_mutation(stage, field);
        }
        let duration = Duration::from_millis(1);
        let input = guest
            .capture_original_followed_poll(call, Box::new(()))
            .await
            .map_err(|e| reverie::Error::Tool(anyhow::anyhow!("Poll capture: {e:?}")))?;
        assert_eq!((input.fd, input.events), (744, libc::POLLIN));
        assert_eq!(input.timeout_millis, if kind == 1 { 0 } else { 5000 });
        if matches!(kind, 21..=23) {
            let saved = guest.regs().await;
            let mut changed = saved;
            match kind {
                21 => changed.orig_rax = Sysno::ppoll as u64,
                22 => changed.rdi ^= 8,
                23 => changed.rip ^= 1,
                _ => unreachable!(),
            }
            guest.set_regs(changed).await?;
            assert!(
                guest
                    .with_followed_poll_store(call, |_| panic!("damaged original issued writer"))
                    .is_err()
            );
            // Returning the registers to their old values cannot reset failure.
            guest.set_regs(saved).await?;
            probe.caught.store(true, Ordering::SeqCst);
            return Ok(1);
        }
        if kind == 24 {
            let mut wrong = args;
            wrong.arg2 += 1;
            assert!(
                guest
                    .with_followed_poll_store(Syscall::from_raw(nr, wrong), |_| panic!(
                        "wrong API tuple issued writer"
                    ))
                    .is_err()
            );
            probe.caught.store(true, Ordering::SeqCst);
            // A mere wrong request cannot invalidate the actual original.
        }
        if kind == 9 {
            let session = probe.session.lock().unwrap().upgrade().unwrap();
            let mut timer = Box::pin(guest.inject_poll_observation_timer(call, duration));
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
        if kind == 10 {
            guest
                .inject_receive_observation_timer(call, duration)
                .await?;
            panic!("Poll acquired Receive role");
        }
        if !matches!(kind, 1 | 2 | 13 | 24) {
            guest.inject_poll_observation_timer(call, duration).await?;
            guest.inject_poll_observation_timer(call, duration).await?;
            assert_eq!(probe.entered.load(Ordering::SeqCst), 2);
            assert_eq!(probe.returned.load(Ordering::SeqCst), 2);
        }
        if kind == 4 {
            let saved = guest.regs().await;
            let mut changed = saved;
            changed.r12 ^= 1;
            guest.set_regs(changed).await?;
            guest.set_regs(saved).await?;
        }
        // Controlled external host mutation deliberately avoids notifying the
        // backend's register/control revision. These cases exercise the fresh
        // actual row/limit comparisons, not a stale-stop shortcut. This is not
        // evidence that arbitrary external writers are admitted.
        if matches!(kind, 5 | 18) {
            external_write(guest.tid(), args.arg0, &745i32.to_ne_bytes());
        }
        if kind == 15 {
            external_write(guest.tid(), args.arg0 + 4, &libc::POLLOUT.to_ne_bytes());
        }
        if kind == 16 {
            let mut old = libc::rlimit64 {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(
                unsafe {
                    libc::prlimit64(
                        guest.tid().as_raw(),
                        libc::RLIMIT_NOFILE,
                        std::ptr::null(),
                        &mut old,
                    )
                },
                0
            );
            assert!(old.rlim_cur > 1);
            let changed = libc::rlimit64 {
                rlim_cur: old.rlim_cur - 1,
                rlim_max: old.rlim_max,
            };
            assert_eq!(
                unsafe {
                    libc::prlimit64(
                        guest.tid().as_raw(),
                        libc::RLIMIT_NOFILE,
                        &changed,
                        std::ptr::null_mut(),
                    )
                },
                0
            );
        }
        if matches!(kind, 17 | 19) {
            let tid = guest.tid();
            let address = args.arg0;
            let observed = Arc::clone(&probe);
            super::super::followed_poll::test_after_store(move || {
                external_write(tid, address, &745i32.to_ne_bytes());
                observed.post_fault.store(true, Ordering::SeqCst);
            });
        }
        let actual = if kind == 3 {
            let mut changed = args;
            changed.arg2 += 1;
            Syscall::from_raw(nr, changed)
        } else {
            call
        };
        if kind == 18 {
            assert!(
                guest
                    .with_followed_poll_store(actual, |_| panic!("stale row issued writer"))
                    .is_err()
            );
            probe.caught.store(true, Ordering::SeqCst);
            return Ok(1); // Actual callback exit must reject this caught error.
        }
        guest
            .with_followed_poll_store(actual, |writer| {
                probe.callbacks.fetch_add(1, Ordering::SeqCst);
                assert_eq!(writer.input(), input);
                assert_eq!(writer.validate_context(), Ok(()));
                let output = if kind == 1 { 0 } else { libc::POLLIN };
                let outcome = writer.store_revents(output);
                probe.outcomes.lock().unwrap().push(outcome);
                if matches!(kind, 17 | 19) {
                    assert!(probe.post_fault.load(Ordering::SeqCst));
                    assert!(matches!(
                        outcome,
                        O::Attempted {
                            raw: Ok(2),
                            postcheck: Err(_)
                        }
                    ));
                } else if kind == 6 {
                    assert!(matches!(
                        outcome,
                        O::Refused(reverie::syscalls::NativeUserStoreRefusal::WriteDenied)
                    ));
                } else {
                    assert_eq!(
                        outcome,
                        O::Attempted {
                            raw: Ok(2),
                            postcheck: Ok(())
                        }
                    );
                }
                assert!(writer.validate_context().is_err());
                assert!(matches!(writer.store_revents(output), O::Refused(_)));
            })
            .map_err(|e| reverie::Error::Tool(anyhow::anyhow!("Poll store: {e:?}")))?;
        if kind == 19 {
            probe.caught.store(true, Ordering::SeqCst);
            return Ok(1); // Actual raw2 survives, but failed postcheck blocks exit.
        }
        if matches!(kind, 6 | 17) {
            return Err(reverie::Error::Tool(anyhow::anyhow!(
                "retained output permission refusal"
            )));
        }
        assert!(
            guest
                .with_followed_poll_store(call, |_| panic!("second Poll writer"))
                .is_err()
        );
        if kind == 11 {
            guest.inject_poll_observation_timer(call, duration).await?;
            panic!("timer renewed used Poll writer");
        }
        probe.done.store(true, Ordering::SeqCst);
        Ok(if kind == 1 { 0 } else { 1 })
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
fn external_write(tid: Pid, address: usize, bytes: &[u8]) {
    let local = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let remote = libc::iovec {
        iov_base: address as *mut libc::c_void,
        iov_len: bytes.len(),
    };
    assert_eq!(
        unsafe { libc::process_vm_writev(tid.as_raw(), &local, 1, &remote, 1, 0) },
        bytes.len() as isize
    );
}

async fn case(child: bool, kind: u8) {
    case_with_mutation(child, kind, None).await;
}
async fn case_with_mutation(child: bool, kind: u8, mutation: Option<(u8, usize)>) {
    let mut command =
        reverie::process::Command::new(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
    command
        .arg("original-poll")
        .arg(u8::from(child).to_string())
        .arg(kind.to_string());
    let tracer = crate::TracerBuilder::<Poller>::new(command)
        .config((kind, child, mutation))
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
    let success = matches!(kind, 0 | 1 | 2 | 14 | 24);
    let output_attempt = success || matches!(kind, 6 | 11 | 17 | 19);
    assert_eq!(completed.result.is_ok(), success, "{:?}", completed.result);
    assert_eq!(
        probe.callbacks.load(Ordering::SeqCst),
        usize::from(output_attempt)
    );
    assert_eq!(
        probe.outcomes.lock().unwrap().len(),
        usize::from(output_attempt)
    );
    assert_eq!(
        probe.caught.load(Ordering::SeqCst),
        matches!(kind, 18 | 19 | 21..=24)
    );
    assert_eq!(probe.done.load(Ordering::SeqCst), success);
    if success {
        assert!(matches!(completed.result, Ok(ExitStatus::Exited(0))));
    }
    if kind == 9 {
        assert!(probe.cancelled.load(Ordering::SeqCst));
    }
    assert_eq!(
        super::super::followed_poll::test_mutation_finish(),
        usize::from(mutation.is_some())
    );
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
async fn native_original_poll_root_child_positive_zero_timeout_and_native_casts() {
    for child in [false, true] {
        for kind in [0, 1, 2, 14] {
            case(child, kind).await;
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_original_poll_wrong_inputs_aliases_roles_and_output_permissions_refuse() {
    for kind in [3, 4, 5, 6, 10, 11, 13, 15, 16, 17] {
        case(false, kind).await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_original_poll_executing_cancellation_retains_scratch_to_final_wait() {
    for child in [false, true] {
        case(child, 9).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_original_poll_caught_input_or_actual_poststore_failure_cannot_resume() {
    for child in [false, true] {
        for kind in [18, 19] {
            case(child, kind).await;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_original_poll_actual_skip_entry_exit_full_frame_mutations_refuse() {
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
        case_with_mutation(false, 20, Some(mutation)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_original_poll_caught_initial_frame_failure_is_sticky_after_restore() {
    for child in [false, true] {
        for kind in [21, 22, 23] {
            case(child, kind).await;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_original_poll_wrong_api_tuple_preserves_exact_original_store() {
    for child in [false, true] {
        case(child, 24).await;
    }
}
