/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use std::time::Instant;

use futures::channel::oneshot;

use super::*;
use crate::vm::GuestThreadGroup;

#[derive(Default)]
struct RpcGlobal {
    response: Mutex<Option<oneshot::Receiver<i64>>>,
    pending: AtomicBool,
    events: Mutex<Vec<(u8, i32, i32)>>,
    failures: AtomicUsize,
}

#[reverie::global_tool]
impl GlobalTool for RpcGlobal {
    type Request = (u8, i32);
    type Response = i64;
    type Config = bool;

    async fn receive_rpc(&self, from: Pid, (kind, code): (u8, i32)) -> i64 {
        if kind != 0 {
            self.events
                .lock()
                .unwrap()
                .push((kind, from.as_raw(), code));
            return 0;
        }
        let mut response = self.response.lock().unwrap().take().unwrap();
        poll_fn(|cx| {
            let result = Pin::new(&mut response).poll(cx);
            if result.is_pending() {
                self.pending.store(true, Ordering::Release);
            }
            result
        })
        .await
        .expect("test controller dropped RPC response")
    }

    fn report_backend_failure(&self, _event: reverie::BackendFailure) {
        self.failures.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct RpcTool;

#[reverie::tool]
impl Tool for RpcTool {
    type GlobalState = RpcGlobal;
    type ThreadState = i32;

    fn init_thread_state(&self, tid: Pid, _parent: Option<(Pid, &i32)>) -> i32 {
        tid.as_raw()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        _call: reverie::syscalls::Syscall,
    ) -> std::result::Result<i64, reverie::Error> {
        Ok(guest.send_rpc((0, 0)).await)
    }

    async fn on_exit_thread<G: GlobalRPC<RpcGlobal>>(
        &self,
        tid: Pid,
        global: &G,
        state: i32,
        status: ExitStatus,
    ) -> std::result::Result<(), reverie::Error> {
        assert_eq!(state, tid.as_raw(), "wrong consuming owner");
        global.send_rpc((1, conventional_exit_code(status))).await;
        if *global.config() && tid.as_raw() == 2 {
            Err(Errno::EIO.into())
        } else {
            Ok(())
        }
    }

    async fn on_exit_process<G: GlobalRPC<RpcGlobal>>(
        self,
        _pid: Pid,
        global: &G,
        status: ExitStatus,
    ) -> std::result::Result<(), reverie::Error> {
        global.send_rpc((2, conventional_exit_code(status))).await;
        Ok(())
    }
}

struct NoGuestExecution;
impl GuestSyscallExecutor<RpcTool> for NoGuestExecution {
    fn read_clock(&self) -> Result<u64> {
        panic!("native RPC control read a guest clock")
    }
    fn execute(&mut self, _: &SyscallRequest, _: &GuestMemory) -> i64 {
        panic!("native RPC control continued guest execution")
    }
}

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "native control did not reach its ordering boundary"
        );
        std::thread::yield_now();
    }
}

fn joined_rpc_control(fail: bool, cleanup_fails: bool) {
    let global = Arc::new(RpcGlobal::default());
    let (response, receiver) = oneshot::channel();
    *global.response.lock().unwrap() = Some(receiver);
    let failure = RunFailure::new(&global);
    let reporter = FailureContext::new(failure.clone(), Pid::from_raw(1), Pid::from_raw(3));
    let tool = Arc::new(RpcTool);
    let group = Arc::new(GuestThreadGroup::default());
    let worker_global = global.clone();
    let worker_tool = tool.clone();
    let subscription = failure.subscribe();
    let worker_failure = failure.clone();
    let worker = std::thread::spawn(move || {
        let pid = Pid::from_raw(1);
        let tid = Pid::from_raw(2);
        let mut state = worker_tool.init_thread_state(tid, None);
        let signal = Arc::new(Mutex::new(None));
        let starts = Arc::new(Mutex::new(Vec::new()));
        let mut executor = NoGuestExecution;
        let memory = GuestMemory::new(0, STACK_CAPACITY).unwrap();
        let subscriptions = Subscription::none();
        let outcome = {
            let mut guest = KvmGuest::new(
                pid,
                tid,
                worker_tool.clone(),
                memory,
                &[],
                // No instruction executes; only send_rpc uses this guest.
                unsafe { std::mem::zeroed() },
                &mut state,
                &mut executor,
                worker_global.as_ref(),
                Some(worker_global.clone()),
                &cleanup_fails,
                &subscriptions,
                signal.clone(),
                starts.clone(),
                crate::bootstrap::TOOL_STACK_TOP,
                Arc::new(AtomicBool::new(false)),
            );
            futures::executor::block_on(drive_handler(
                worker_tool.handle_syscall_event(
                    &mut guest,
                    SyscallRequest::new(libc::SYS_getpid as u64, [0; 6])
                        .into_syscall()
                        .unwrap(),
                ),
                signal,
                starts,
                wait_for_failure(worker_global.as_ref(), Some(subscription)),
            ))
        };
        let status = if fail {
            assert!(
                matches!(outcome, HandlerOutcome::RunFailed),
                "watchdog/ordinary response is not fatal cancellation"
            );
            ExitStatus::Exited(255)
        } else {
            match outcome {
                HandlerOutcome::Returned(Ok(37)) => ExitStatus::Exited(37),
                _ => panic!("normal RPC/status changed"),
            }
        };
        let cleanup = futures::executor::block_on(notify_tool_exit(
            worker_tool,
            pid,
            tid,
            worker_global.as_ref(),
            &cleanup_fails,
            state,
            ToolExit {
                status,
                process_exited: false,
            },
            None,
        ));
        if fail {
            let primary =
                Error::SharedFailure(worker_failure.primary().expect("missing typed failure"));
            Err(primary.with_cleanup(cleanup.err().into_iter().collect()))
        } else {
            cleanup?;
            Ok((status, Vec::new(), Vec::new()))
        }
    });
    group.add_worker_handle(2, worker);
    wait_until(|| global.pending.load(Ordering::Acquire));
    let join_group = group.clone();
    let (joined, joined_receiver) = std::sync::mpsc::channel();
    let joiner = std::thread::spawn(move || {
        join_group.join_workers();
        joined.send(()).unwrap();
    });
    // The worker is parked in the actual Tool RPC, and the actual production
    // joiner has taken its OS JoinHandle out of the group's registry.
    wait_until(|| !group.has_worker_handles());
    assert!(matches!(
        joined_receiver.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    let mut response = Some(response);
    if fail {
        reporter.publish(
            "controlled worker failure",
            Error::GuestClock("typed primary".to_owned()),
        );
    } else {
        response.take().unwrap().send(37).unwrap();
    }
    let completed = joined_receiver.recv_timeout(Duration::from_secs(2)).is_ok();
    if !completed {
        // Rescue only reaps the failed control. The completed assertion remains
        // false, so moving publication after join/removing the subscription fails.
        if let Some(response) = response.take() {
            let _ = response.send(99);
        }
    }
    joiner.join().unwrap();
    assert!(
        completed,
        "fatal publication did not release the actual join"
    );
    let result = group.teardown_result();
    if fail {
        let error = result.unwrap_err();
        assert!(
            has_guest_clock_primary(&error),
            "typed worker cause was lost: {error:?}"
        );
        if cleanup_fails {
            assert!(has_cleanup_eio(&error), "cleanup cause was lost: {error:?}");
        }
        assert!(global.failures.load(Ordering::SeqCst) > 0);
    } else {
        result.unwrap();
        assert_eq!(global.failures.load(Ordering::SeqCst), 0);
    }
    let status = ExitStatus::Exited(if fail { 255 } else { 37 });
    futures::executor::block_on(notify_tool_exit(
        tool,
        Pid::from_raw(1),
        Pid::from_raw(1),
        global.as_ref(),
        &cleanup_fails,
        1,
        ToolExit {
            status,
            process_exited: true,
        },
        None,
    ))
    .unwrap();
    assert_eq!(
        *global.events.lock().unwrap(),
        vec![
            (1, 2, conventional_exit_code(status)),
            (1, 1, conventional_exit_code(status)),
            (2, 1, conventional_exit_code(status))
        ]
    );
    assert!(!group.has_worker_handles());
}

fn has_guest_clock_primary(error: &Error) -> bool {
    match error {
        Error::GuestClock(_) => true,
        Error::SharedFailure(error) => has_guest_clock_primary(error),
        Error::WithCleanup { primary, .. } => has_guest_clock_primary(primary),
        _ => false,
    }
}
fn has_cleanup_eio(error: &Error) -> bool {
    match error {
        Error::Reverie(reverie::Error::Errno(errno)) => *errno == Errno::EIO,
        Error::SharedFailure(error) => has_cleanup_eio(error),
        Error::WithCleanup { primary, cleanup } => {
            has_cleanup_eio(primary) || cleanup.iter().any(has_cleanup_eio)
        }
        _ => false,
    }
}

#[test]
fn fatal_notification_releases_pending_tool_rpc_before_owned_join() {
    joined_rpc_control(true, false);
}
#[test]
fn fatal_worker_primary_survives_separate_consuming_hook_error() {
    joined_rpc_control(true, true);
}
#[test]
fn normal_rpc_keeps_status_and_worker_before_leader_hooks() {
    joined_rpc_control(false, false);
}

#[test]
fn failure_precedes_ready_callback_and_keeps_child_start_gate_closed() {
    let global = Arc::new(RpcGlobal::default());
    let failure = RunFailure::new(&global);
    let reporter = FailureContext::new(failure.clone(), Pid::from_raw(1), Pid::from_raw(1));
    reporter.publish("setup", Error::GuestClock("setup".to_owned()));
    let (sender, receiver) = std::sync::mpsc::channel();
    let gate = ChildStartGate::new(sender);
    let starts = Arc::new(Mutex::new(vec![PendingChildStart::tool_thread(2, gate)]));
    let polled = AtomicBool::new(false);
    let outcome = futures::executor::block_on(drive_handler(
        async {
            polled.store(true, Ordering::SeqCst);
            37
        },
        Arc::new(Mutex::new(None)),
        starts.clone(),
        wait_for_failure(global.as_ref(), Some(failure.subscribe())),
    ));
    assert!(matches!(outcome, HandlerOutcome::RunFailed));
    assert!(!polled.load(Ordering::SeqCst));
    assert!(matches!(
        receiver.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    starts.lock().unwrap().pop().unwrap().cancel();
    assert_eq!(receiver.recv().unwrap(), ChildStartCommand::Cancel);
}
