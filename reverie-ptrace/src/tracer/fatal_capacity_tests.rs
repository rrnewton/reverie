/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

pub(super) mod fatal_capacity_tests {
    use super::*;

    type FinishedObservations = Vec<(i32, bool, bool, bool)>;
    thread_local! {
        static FINISHED: std::cell::RefCell<Option<FinishedObservations>> = const { std::cell::RefCell::new(None) };
    }

    pub(crate) fn record_finished(stop: &FatalTaskStop) {
        FINISHED.with(|slot| {
            if let Some(records) = slot.borrow_mut().as_mut() {
                records.push((
                    stop.tid.as_raw(),
                    matches!(stop.terminal.observed_exit_status(), Ok(Some(_))),
                    stop.terminal.wait(Duration::ZERO),
                    stop.held.lock().unwrap().is_none(),
                ));
            }
        });
    }

    const TASKS: usize = 96;
    const FD_LIMIT: libc::rlim_t = 128;

    #[derive(Default)]
    struct CapacityLog(Arc<StdMutex<Vec<(usize, usize)>>>);

    #[reverie::global_tool]
    impl GlobalTool for CapacityLog {
        type Config = ();
        type Request = (usize, usize);
        type Response = ();
        async fn receive_rpc(&self, _: Pid, sample: Self::Request) {
            self.0.lock().unwrap().push(sample);
        }
    }

    #[derive(Default)]
    struct CapacityTool;

    #[reverie::tool]
    impl Tool for CapacityTool {
        type GlobalState = CapacityLog;
        type ThreadState = ();

        fn subscriptions(_: &()) -> Subscription {
            [Sysno::getpgid].into_iter().collect()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            // The guest join/wait has completed. A yield permits the backend
            // owner to progress but does not itself prove retirement. Record
            // the actual post-hook session.finished boundary separately using
            // scalar observations only; no descriptor authority is retained.
            tokio::task::yield_now().await;
            let (_, args) = syscall.into_parts();
            let fds = fs::read_dir("/proc/self/fd")
                .map_err(anyhow::Error::new)?
                .count();
            let finished = FINISHED.with(|slot| slot.borrow().as_ref().unwrap().clone());
            eprintln!(
                "capacity marker: iteration={}, fds={fds}, finished_after_hooks={finished:?}",
                args.arg0
            );
            guest.send_rpc((args.arg0, fds)).await;
            Ok(0)
        }
    }

    async fn isolated_capacity(threads: bool, deadline: u64) {
        FINISHED.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
        let mut original: libc::rlimit = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut original) },
            0
        );
        assert!(original.rlim_max >= FD_LIMIT);
        let limited = libc::rlimit {
            rlim_cur: FD_LIMIT,
            rlim_max: original.rlim_max,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limited) }, 0);
        let tracer = tokio::time::timeout(
            fatal_remaining(deadline),
            spawn_fn_with_config::<CapacityTool, _>(
                move || {
                    for iteration in 0..TASKS {
                        if threads {
                            std::thread::spawn(|| 7).join().unwrap();
                        } else {
                            let child = unsafe { libc::fork() };
                            assert!(child >= 0);
                            if child == 0 {
                                unsafe { libc::_exit(7) };
                            }
                            let mut status = 0;
                            assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
                            assert!(libc::WIFEXITED(status));
                            assert_eq!(libc::WEXITSTATUS(status), 7);
                        }
                        assert_eq!(unsafe { libc::syscall(libc::SYS_getpgid, iteration) }, 0);
                    }
                },
                (),
                true,
            ),
        )
        .await
        .expect("capacity spawn exceeded the original shared deadline")
        .unwrap();
        let root = tracer.guest_pid();
        let termination = tracer.termination_handle().unwrap();
        let samples = tracer.gref.0.clone();
        let mut owner = Box::pin(tracer.wait_with_output_completion());
        let result = tokio::time::timeout(fatal_remaining(deadline), &mut owner).await;
        let captured = samples.lock().unwrap().clone();
        let description = match &result {
            Ok(ToolRunOutcome::Complete(done)) => format!("Complete({:?})", done.result),
            Ok(ToolRunOutcome::CleanupPending(pending)) => {
                format!("Pending({:?})", pending.failure())
            }
            Ok(ToolRunOutcome::UnsupportedBackend(_)) => "UnsupportedBackend".to_owned(),
            Err(error) => format!("Timeout({error})"),
        };
        eprintln!(
            "capacity original predicate: threads={threads}, soft_nofile={FD_LIMIT}, required_tasks={TASKS}, samples={captured:?}, outcome={description}"
        );
        let complete = match result {
            Ok(ToolRunOutcome::Complete(done)) => done,
            other => {
                let rescue_deadline = Instant::now() + Duration::from_secs(2);
                termination.terminate(Error::Tool(anyhow::Error::new(TestDeadline)));
                let rescued = match other {
                    Err(_) => tokio::time::timeout_at(rescue_deadline.into(), &mut owner).await,
                    Ok(ToolRunOutcome::CleanupPending(pending)) => {
                        tokio::time::timeout_at(rescue_deadline.into(), pending.resume_cleanup())
                            .await
                    }
                    Ok(ToolRunOutcome::UnsupportedBackend(tracer)) => {
                        tokio::time::timeout_at(
                            rescue_deadline.into(),
                            tracer.wait_with_output_completion(),
                        )
                        .await
                    }
                    Ok(ToolRunOutcome::Complete(_)) => unreachable!(),
                };
                eprintln!(
                    "capacity rescue only: completed={}",
                    matches!(rescued, Ok(ToolRunOutcome::Complete(_)))
                );
                panic!(
                    "capacity did not complete under the original shared 3s deadline: {description}"
                );
            }
        };
        let output = complete
            .result
            .expect("short-lived tasks must not exhaust tracer fds");
        assert_eq!(output.status, ExitStatus::Exited(0));
        assert_eq!(captured.len(), TASKS);
        for (index, sample) in captured.iter().enumerate() {
            assert_eq!(sample.0, index);
        }
        // Fixed headroom accounts for the original task/hook completing beside
        // the next marker. It cannot hide the 3/4-fd-per-task baseline slope.
        let baseline = captured[0].1;
        assert!(
            captured.iter().all(|sample| sample.1 <= baseline + 8),
            "retired tasks retained descriptors: {captured:?}"
        );
        assert!(
            captured[TASKS - 1].1 <= baseline + 2,
            "descriptor count did not return to the initial steady state"
        );
        assert_reaped("capacity root", root);
        let _remaining = fatal_remaining(deadline);
    }

    async fn capacity_control(threads: bool, name: &str) {
        if std::env::var("REVERIE_FATAL_CAPACITY_TEST").as_deref() == Ok(name) {
            assert!(std::env::args().any(|arg| arg == name));
            let deadline = std::env::var("REVERIE_FATAL_CAPACITY_DEADLINE_NS")
                .unwrap()
                .parse()
                .unwrap();
            isolated_capacity(threads, deadline).await;
            return;
        }
        // A reduced process-wide fd limit belongs only to this exact isolated
        // test process. The deadline begins before the re-exec, not after it.
        let deadline = fatal_monotonic_ns() + 3_000_000_000;
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([name, "--exact", "--nocapture", "--test-threads=1"])
            .env("REVERIE_FATAL_CAPACITY_TEST", name)
            .env("REVERIE_FATAL_CAPACITY_DEADLINE_NS", deadline.to_string())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "isolated reduced-NOFILE capacity predicate failed: {status}"
        );
        let _remaining = fatal_remaining(deadline);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retired_processes_release_descriptors_under_reduced_nofile() {
        capacity_control(false, "tracer::tests::fatal_capacity_tests::retired_processes_release_descriptors_under_reduced_nofile").await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retired_threads_release_descriptors_under_reduced_nofile() {
        capacity_control(true, "tracer::tests::fatal_capacity_tests::retired_threads_release_descriptors_under_reduced_nofile").await;
    }
}
