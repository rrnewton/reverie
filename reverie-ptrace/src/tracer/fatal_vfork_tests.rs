/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

mod fatal_vfork_tests {
    use super::*;

    type VforkEvents = Vec<(u8, Pid, Option<ExitStatus>)>;
    #[derive(Default)]
    struct VforkLog(Arc<StdMutex<VforkEvents>>);

    #[reverie::global_tool]
    impl GlobalTool for VforkLog {
        type Config = (bool, usize);
        type Request = (u8, Pid, Option<ExitStatus>);
        type Response = ();
        async fn receive_rpc(&self, _from: Pid, event: Self::Request) {
            self.0.lock().unwrap().push(event);
        }
    }

    #[derive(Default)]
    struct VforkTool;

    fn word(address: usize, index: usize) -> &'static std::sync::atomic::AtomicUsize {
        // The parent fixture retains this shared mapping until all owned tasks
        // have completed, including any separately reported rescue.
        unsafe { &*(address as *const std::sync::atomic::AtomicUsize).add(index) }
    }

    #[reverie::tool]
    impl Tool for VforkTool {
        type GlobalState = VforkLog;
        type ThreadState = bool;

        fn subscriptions(_config: &(bool, usize)) -> Subscription {
            [Sysno::getpgid].into_iter().collect()
        }

        async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
            guest.send_rpc((0, guest.tid(), None)).await;
            let (fail, address) = *guest.config();
            let root = word(address, 2).load(Ordering::SeqCst);
            if root != 0 && guest.pid().as_raw() as usize != root {
                word(address, 1).store(guest.pid().as_raw() as usize, Ordering::SeqCst);
                if fail {
                    struct Parked(usize);
                    impl Drop for Parked {
                        fn drop(&mut self) {
                            word(self.0, 3).store(0, Ordering::SeqCst);
                        }
                    }
                    *guest.thread_state_mut() = true;
                    word(address, 3).store(1, Ordering::SeqCst);
                    let _parked = Parked(address);
                    future::pending::<()>().await;
                }
            }
            Ok(())
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            let (fail, address) = *guest.config();
            assert_ne!(guest.tid(), guest.pid());
            assert_ne!(word(address, 1).load(Ordering::SeqCst), 0);
            guest.send_rpc((1, guest.tid(), None)).await;
            if fail {
                assert_eq!(
                    word(address, 3).load(Ordering::SeqCst),
                    1,
                    "vfork child must still be parked in its initialized callback"
                );
                Err(anyhow::Error::new(NonleaderFailure).into())
            } else {
                Ok(guest.inject(syscall).await?)
            }
        }

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            tid: Pid,
            global: &G,
            parked: bool,
            status: ExitStatus,
        ) -> Result<(), Error> {
            if parked {
                assert_eq!(status, ExitStatus::Signaled(Signal::SIGKILL, false));
            }
            global.send_rpc((2, tid, Some(status))).await;
            Ok(())
        }

        async fn on_exit_process<G: reverie::GlobalRPC<Self::GlobalState>>(
            self,
            pid: Pid,
            global: &G,
            status: ExitStatus,
        ) -> Result<(), Error> {
            global.send_rpc((3, pid, Some(status))).await;
            Ok(())
        }
    }

    async fn control(fail: bool) {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(3);
        let words = FatalWords::new();
        let address = words.0 as usize;
        let tracer = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            spawn_fn_with_config::<VforkTool, _>(
                move || {
                    word(address, 2).store(unsafe { libc::getpid() } as usize, Ordering::SeqCst);
                    let sibling = std::thread::spawn(move || {
                        while word(address, 1).load(Ordering::SeqCst) == 0 {
                            std::thread::yield_now();
                        }
                        unsafe {
                            libc::syscall(libc::SYS_getpgid, 0);
                        }
                        word(address, 0).store(1, Ordering::SeqCst);
                        if fail {
                            loop {
                                unsafe {
                                    libc::pause();
                                }
                            }
                        }
                    });
                    // CLONE_VFORK gives the real kernel vfork parent suspension
                    // and PTRACE_EVENT_VFORK, while a distinct stack avoids the
                    // unsupported Rust call-frame sharing of libc::vfork.
                    extern "C" fn child_body(_: *mut libc::c_void) -> libc::c_int {
                        7
                    }
                    let mut stack = vec![0u8; 16 * 1024];
                    let stack_top = (unsafe { stack.as_mut_ptr().add(stack.len()) } as usize
                        & !15usize) as *mut libc::c_void;
                    let child = unsafe {
                        libc::clone(
                            child_body,
                            stack_top,
                            libc::CLONE_VM | libc::CLONE_VFORK | libc::SIGCHLD,
                            std::ptr::null_mut::<libc::c_void>(),
                        )
                    };
                    assert!(child > 0);
                    let mut status = 0;
                    assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
                    assert!(libc::WIFEXITED(status));
                    assert_eq!(libc::WEXITSTATUS(status), 7);
                    sibling.join().unwrap();
                },
                (fail, address),
                true,
            ),
        )
        .await
        .expect("vfork spawn exceeded single deadline")
        .unwrap();
        let root = tracer.guest_pid();
        let session = tracer.ordinary_session.clone();
        let identity = untraced_process_identity(root);
        let termination = tracer.termination_handle().unwrap();
        let log = tracer.gref.0.clone();
        let mut completion = Box::pin(tracer.wait_with_output_completion());
        let result = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            &mut completion,
        )
        .await;
        let elapsed = started.elapsed();
        let root_retired = !identity.same_process();
        let child = Pid::from_raw(words.read(1) as i32);
        let after = words.read(0);
        let parked = words.read(3);
        let events = log.lock().unwrap().clone();
        let edges = session.observed_child_ops.lock().unwrap().clone();
        eprintln!("actual ptrace child edges: {edges:?}");
        let description = match &result {
            Ok(ToolRunOutcome::Complete(completed)) => format!("Complete({:?})", completed.result),
            Ok(ToolRunOutcome::CleanupPending(pending)) => {
                format!("Pending({:?})", pending.failure())
            }
            Ok(ToolRunOutcome::UnsupportedBackend(_)) => "UnsupportedBackend".to_owned(),
            Err(error) => format!("Timeout({error})"),
        };
        eprintln!(
            "vfork before rescue: fail={fail}, root={root}, child={child}, root_retired={root_retired}, after={after}, parked={parked}, elapsed={elapsed:?}, events={events:?}, outcome={description}"
        );
        let completed = match result {
            Ok(ToolRunOutcome::Complete(completed)) => completed,
            other => {
                let rescue_deadline = Instant::now() + Duration::from_secs(2);
                termination.terminate(Error::Tool(anyhow::Error::new(TestDeadline)));
                let signal = identity.send_signal(Signal::SIGKILL);
                let rescued = match other {
                    Err(_) => {
                        tokio::time::timeout(
                            rescue_deadline.saturating_duration_since(Instant::now()),
                            &mut completion,
                        )
                        .await
                    }
                    Ok(ToolRunOutcome::CleanupPending(pending)) => {
                        tokio::time::timeout(
                            rescue_deadline.saturating_duration_since(Instant::now()),
                            pending.resume_cleanup(),
                        )
                        .await
                    }
                    Ok(ToolRunOutcome::UnsupportedBackend(tracer)) => {
                        tokio::time::timeout(
                            rescue_deadline.saturating_duration_since(Instant::now()),
                            tracer.wait_with_output_completion(),
                        )
                        .await
                    }
                    Ok(ToolRunOutcome::Complete(_)) => unreachable!(),
                };
                eprintln!(
                    "vfork rescue only: signal={signal:?}, completed={}",
                    matches!(rescued, Ok(ToolRunOutcome::Complete(_)))
                );
                panic!("vfork did not Complete within original3s predicate: {description}");
            }
        };
        assert!(elapsed <= Duration::from_secs(3));
        assert!(root_retired);
        assert_eq!(
            edges
                .iter()
                .filter(|edge| **edge == (root, safeptrace::ChildOp::Vfork, child))
                .count(),
            1
        );
        assert_eq!(parked, 0, "initialized child callback was not cancelled");
        assert_reaped("vfork root", root);
        assert_reaped("vfork child", child);
        assert_eq!(*completed.global_state.0.lock().unwrap(), events);
        assert_eq!(events.iter().filter(|e| e.0 == 0).count(), 3);
        assert_eq!(events.iter().filter(|e| e.0 == 1).count(), 1);
        assert_eq!(events.iter().filter(|e| e.0 == 2).count(), 3);
        assert_eq!(events.iter().filter(|e| e.0 == 3).count(), 2);
        for start in events.iter().filter(|e| e.0 == 0) {
            let expected = if fail {
                ExitStatus::Signaled(Signal::SIGKILL, false)
            } else {
                ExitStatus::Exited(if start.1 == child { 7 } else { 0 })
            };
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e.0 == 2 && e.1 == start.1 && e.2 == Some(expected))
                    .count(),
                1
            );
            assert_reaped("vfork task", start.1);
        }
        if fail {
            assert_eq!(after, 0, "failed sibling resumed user code");
            let failure = completed.result.expect_err("vfork failure lost");
            assert!(
                matches!(failure.primary(), Error::Tool(e) if e.downcast_ref::<NonleaderFailure>().is_some())
            );
            assert_eq!(failure.origin().phase, "ptrace syscall callback");
            assert_eq!(failure.captured_prefix().unwrap().stdout(), b"");
            assert_eq!(failure.captured_prefix().unwrap().stderr(), b"");
        } else {
            assert_eq!(after, 1);
            assert_eq!(completed.result.unwrap().status, ExitStatus::Exited(0));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failure_owns_initialized_vfork_child_before_parent_retirement() {
        control(true).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successful_vfork_consumes_every_state_once() {
        control(false).await;
    }
}
