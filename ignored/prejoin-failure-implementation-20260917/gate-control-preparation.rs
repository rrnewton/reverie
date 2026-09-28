    #[derive(Default)]
    struct GateFailureLog {
        events: Mutex<Vec<(u8, i32, i32)>>,
        failures: Mutex<Vec<reverie::BackendFailure>>,
        wake: Mutex<Option<futures::channel::oneshot::Sender<()>>>,
        notification: Option<crate::failure::FailureSubscription>,
        release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    #[reverie::global_tool]
    impl GlobalTool for GateFailureLog {
        type Request = (u8, i32);
        type Response = ();
        type Config = ();

        async fn receive_rpc(&self, from: Pid, (kind, status): Self::Request) {
            self.events.lock().unwrap().push((kind, from.as_raw(), status));
        }

        fn report_backend_failure(&self, event: reverie::BackendFailure) {
            self.failures.lock().unwrap().push(event);
            // Model a Tool that has finished its terminal transition and woken
            // its subscribers, but has not yet returned to the local publisher.
            if let Some(wake) = self.wake.lock().unwrap().take() { let _ = wake.send(()); }
            if let Some(release) = self.release.lock().unwrap().take() { release.recv().unwrap(); }
        }

        async fn wait_for_backend_failure(&self) {
            if let Some(notification) = &self.notification { let _ = notification.clone().await; }
            else { std::future::pending::<()>().await; }
        }
    }

    #[derive(Default)]
    struct GateFailureTool;

    #[reverie::tool]
    impl Tool for GateFailureTool {
        type GlobalState = GateFailureLog;
        type ThreadState = i32;

        fn init_thread_state(&self, tid: Pid, _: Option<(Pid, &i32)>) -> i32 { tid.as_raw() }

        async fn handle_thread_start<G: reverie::Guest<Self>>(&self, _: &mut G)
            -> std::result::Result<(), reverie::Error> {
            panic!("cancelled constructed child entered its start callback")
        }

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self, tid: Pid, global: &G, state: i32, status: ExitStatus,
        ) -> std::result::Result<(), reverie::Error> {
            assert_eq!(tid.as_raw(), state);
            global.send_rpc((1, conventional_exit_code(status))).await;
            Ok(())
        }

        async fn on_exit_process<G: reverie::GlobalRPC<Self::GlobalState>>(
            self, _: Pid, global: &G, status: ExitStatus,
        ) -> std::result::Result<(), reverie::Error> {
            global.send_rpc((2, conventional_exit_code(status))).await;
            Ok(())
        }
    }

    // Install all rescue ownership before constructing a gated OS child.
    struct GateFailureCleanup {
        backend: KvmBackend,
        executor: ElfExecutor,
        starts: SharedChildStarts,
        release: Option<std::sync::mpsc::Sender<()>>,
        publisher: Option<std::thread::JoinHandle<Error>>,
        done: Option<std::sync::mpsc::Sender<()>>,
        watchdog: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for GateFailureCleanup {
        fn drop(&mut self) {
            if let Some(release) = self.release.take() { let _ = release.send(()); }
            if let Some(publisher) = self.publisher.take() { let _ = publisher.join(); }
            if let Some(done) = self.done.take() { let _ = done.send(()); }
            if let Some(watchdog) = self.watchdog.take() { let _ = watchdog.join(); }
            let _ = self.backend.discard_unstarted_tool_children(&mut self.executor, &self.starts);
            self.backend.cancel_guest_threads();
            let _ = self.executor.join_child_processes_after_failure();
        }
    }

    #[test]
    fn real_fork_and_thread_cancel_keep_status_while_terminal_hook_is_returning() {
        use futures::FutureExt;
        for thread in [false, true] {
            for fatal in [false, true] {
                let (backend, executor, boundary) = backend_at_completed_tool_boundary()
                    .expect("real cancellation control requires KVM and PMU");
                let starts = Arc::new(Mutex::new(Vec::new()));
                let mut cleanup = GateFailureCleanup {
                    backend, executor, starts: starts.clone(), release: None,
                    publisher: None, done: None, watchdog: None,
                };
                cleanup.backend.thread_ownership = ThreadOwnership::Tool;
                let (wake, notification) = futures::channel::oneshot::channel();
                let (release, wait_release) = std::sync::mpsc::channel();
                cleanup.release = Some(release.clone());
                let global = Arc::new(GateFailureLog {
                    wake: Mutex::new(Some(wake)), notification: Some(notification.shared()),
                    release: Mutex::new(Some(wait_release)), ..Default::default()
                });
                let failure = crate::failure::RunFailure::new(&global);
                let context = crate::failure::FailureContext::new(
                    failure.clone(), Pid::from_raw(1), Pid::from_raw(1),
                );
                cleanup.backend.tool_failure = Some(context.clone());
                let action = if thread {
                    ProcessAction::Thread {
                        child_tid: 2, child_stack: boundary.registers.rsp,
                        parent_tid: None, child_tid_address: None, clear_child_tid: None, tls: None,
                    }
                } else {
                    ProcessAction::Fork {
                        child_pid: 2, child_stack: None, parent_tid: None, child_tid: None,
                        clear_child_tid: None, clear_sighand: false, share_address_space: false,
                    }
                };
                let continuation = ProcessActionContinuation::from_captured(&action, boundary);
                let tool_context = ToolContext::<GateFailureTool> {
                    process_state: Arc::new(GateFailureTool), pid: Pid::from_raw(1),
                    tid: Pid::from_raw(1), thread_state: &1, global_state: Some(global.clone()),
                    config: (), subscriptions: reverie::Subscription::none(),
                    pending_child_starts: starts.clone(),
                };
                let result = futures::executor::block_on(
                    cleanup.backend.run_process_action_with_tool_at_boundary(
                        &mut cleanup.executor, action, tool_context, continuation,
                    ),
                ).unwrap();
                assert_eq!(result, ProcessActionOutcome::returned(2));
                assert_eq!(starts.lock().unwrap().len(), 1);
                let rescued = Arc::new(AtomicBool::new(false));
                let result = if fatal {
                    let (done, wait_done) = std::sync::mpsc::channel();
                    cleanup.done = Some(done);
                    let watchdog_rescued = rescued.clone();
                    cleanup.watchdog = Some(std::thread::spawn(move || {
                        if wait_done.recv_timeout(std::time::Duration::from_secs(2)).is_err() {
                            watchdog_rescued.store(true, Ordering::Release);
                            let _ = release.send(());
                        }
                    }));
                    cleanup.publisher = Some(std::thread::spawn(move || {
                        context.publish("controlled terminal transition", Error::GuestClock("gate primary".to_owned()))
                    }));
                    futures::executor::block_on(global.wait_for_backend_failure());
                    assert!(failure.published_primary().is_none());
                    assert!(failure.subscribe().now_or_never().is_none(), "local wake escaped the Tool hook");
                    let result = cleanup.backend.cleanup_unstarted_tool_children_after_error(
                        &mut cleanup.executor, &starts, Error::RunAborted,
                    );
                    assert!(failure.published_primary().is_none(), "child cleanup waited for hook return");
                    Err(result)
                } else {
                    cleanup.backend.discard_unstarted_tool_children(&mut cleanup.executor, &starts)
                };
                let status = if fatal { 255 } else { 0 };
                let mut expected = vec![(1, 2, status)];
                if !thread { expected.push((2, 2, status)); }
                assert_eq!(*global.events.lock().unwrap(), expected);
                assert!(starts.lock().unwrap().is_empty());
                assert!(!cleanup.executor.has_pending_child_process(2));
                assert!(!cleanup.backend.thread_group.has_worker_handles());
                if fatal {
                    assert!(!rescued.load(Ordering::Acquire), "watchdog released a blocked cleanup");
                    cleanup.release.take().unwrap().send(()).unwrap();
                    let published = cleanup.publisher.take().unwrap().join().unwrap();
                    cleanup.done.take().unwrap().send(()).unwrap();
                    cleanup.watchdog.take().unwrap().join().unwrap();
                    assert!(matches!(published.primary(), Error::GuestClock(message) if message == "gate primary"));
                    let result = failure.complete(result).unwrap_err();
                    assert!(matches!(result.primary(), Error::GuestClock(message) if message == "gate primary"));
                    assert_eq!(*global.failures.lock().unwrap(), vec![reverie::BackendFailure {
                        pid: Pid::from_raw(1), tid: Pid::from_raw(1), phase: "controlled terminal transition",
                    }]);
                } else {
                    result.unwrap();
                    assert!(global.failures.lock().unwrap().is_empty());
                }
            }
        }
    }

