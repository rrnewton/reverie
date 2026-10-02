/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Real Command containment controls, separate from faithful signal delivery.
//! The external bounded caller builds and binds the C fixture before selection.
use reverie::Guest;
use reverie::InjectedSyscallEvent;
use reverie::syscalls::Getpid;
use reverie::syscalls::Gettid;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Tgkill;
use serde::Deserialize;
use serde::Serialize;

use super::*;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum Observation {
    Armed(Pid),
    Prepared(Pid),
    Returned(Pid, i64),
    ToolResult(Pid, Result<i64, i32>),
    ThreadExit(Pid, ExitStatus),
    ProcessExit(Pid, ExitStatus),
}

#[derive(Default)]
struct Log(StdMutex<Vec<Observation>>);

#[reverie::global_tool]
impl GlobalTool for Log {
    type Config = bool;
    type Request = Observation;
    type Response = ();

    async fn receive_rpc(&self, _: Pid, event: Observation) {
        self.0.lock().unwrap().push(event);
    }
}

#[derive(Default)]
struct SignalTool {
    queue_signal: bool,
}

#[reverie::tool]
impl Tool for SignalTool {
    type GlobalState = Log;
    // True only for the final private getpid, after actual tgkill completed.
    type ThreadState = bool;

    fn new(_: Pid, queue_signal: &bool) -> Self {
        Self {
            queue_signal: *queue_signal,
        }
    }

    fn subscriptions(_: &bool) -> Subscription {
        [Sysno::write].into_iter().collect()
    }

    fn observe_injected_syscalls(_: &bool) -> bool {
        true
    }

    fn observe_injected_syscall_preparation(_: &bool) -> bool {
        true
    }

    fn on_injected_syscall_observed(
        &self,
        tid: Pid,
        global: &Log,
        observing: &mut bool,
        nr: Sysno,
        _: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        if *observing && nr == Sysno::getpid {
            let observation = match event {
                InjectedSyscallEvent::Prepared => Some(Observation::Prepared(tid)),
                InjectedSyscallEvent::Returned(raw) => Some(Observation::Returned(tid, raw)),
                _ => None,
            };
            if let Some(observation) = observation {
                global.0.lock().unwrap().push(observation);
            }
        }
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        let (_, args) = call.into_parts();
        if args.arg0 != 784 {
            return Ok(guest.inject(call).await?);
        }
        let pid = guest.inject(Getpid::new()).await?;
        let tid = guest.inject(Gettid::new()).await?;
        if self.queue_signal {
            guest
                .inject(
                    Tgkill::new()
                        .with_tgid(pid as _)
                        .with_tid(tid as _)
                        .with_sig(libc::SIGUSR1),
                )
                .await?;
        }
        *guest.thread_state_mut() = true;
        guest
            .local_global_state()
            .unwrap()
            .0
            .lock()
            .unwrap()
            .push(Observation::Armed(guest.tid()));
        let result = guest.inject(Getpid::new()).await;
        // Record even Errno before propagating it. The old ERESTARTSYS branch
        // must not hide its fabricated result behind the guest's later failure.
        guest
            .local_global_state()
            .unwrap()
            .0
            .lock()
            .unwrap()
            .push(Observation::ToolResult(
                guest.tid(),
                result.map_err(Errno::into_raw),
            ));
        *guest.thread_state_mut() = false;
        result.map_err(Error::from)
    }

    async fn on_exit_thread<G: reverie::GlobalRPC<Log>>(
        &self,
        tid: Pid,
        global: &G,
        _: bool,
        status: ExitStatus,
    ) -> Result<(), Error> {
        global.send_rpc(Observation::ThreadExit(tid, status)).await;
        Ok(())
    }

    async fn on_exit_process<G: reverie::GlobalRPC<Log>>(
        self,
        pid: Pid,
        global: &G,
        status: ExitStatus,
    ) -> Result<(), Error> {
        global.send_rpc(Observation::ProcessExit(pid, status)).await;
        Ok(())
    }
}

async fn run(queue_signal: bool) {
    let fixture = crate::testing::fixture_path("REVERIE_PRIVATE_SIGNAL_FIXTURE");
    let fixture = PathBuf::from(fixture);
    assert!(
        fixture.is_absolute(),
        "fixture path must not use PATH lookup"
    );
    let mut command = Command::new(fixture);
    command.arg(if queue_signal { "signal" } else { "ordinary" });
    command.stdout(reverie::process::Stdio::piped());
    command.stderr(reverie::process::Stdio::piped());
    let started = Instant::now();
    let deadline = started + Duration::from_secs(5);
    let tracer = TracerBuilder::<SignalTool>::new(command)
        .config(queue_signal)
        .spawn()
        .await
        .expect("spawn actual Command fixture");
    let root = tracer.guest_pid();
    let termination = tracer
        .termination_handle()
        .expect("ordinary original owner");
    // The child is still stopped at bootstrap and has not entered Tool code.
    let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, root.as_raw(), 0) };
    let open_error = (raw_fd == -1).then(std::io::Error::last_os_error);
    let pidfd = (raw_fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(raw_fd as i32) });
    if let Some(error) = &open_error {
        termination.terminate(anyhow::anyhow!("fixture original pidfd open: {error}").into());
    }
    let mut completion = Box::pin(tracer.wait_with_output_completion());
    let original = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        &mut completion,
    )
    .await;
    let completed = match original {
        Ok(ToolRunOutcome::Complete(completed)) => completed,
        other => {
            // Rescue polls the identical original owner, never another waiter.
            // It is cleanup of a failed test, not success under a renewed clock.
            termination.terminate(
                anyhow::anyhow!("private signal fixture original deadline/refusal").into(),
            );
            let rescue = match other {
                Err(_) => tokio::time::timeout(Duration::from_secs(2), &mut completion).await,
                Ok(ToolRunOutcome::CleanupPending(pending)) => {
                    tokio::time::timeout(Duration::from_secs(2), pending.resume_cleanup()).await
                }
                Ok(ToolRunOutcome::UnsupportedBackend(tracer)) => {
                    tokio::time::timeout(
                        Duration::from_secs(2),
                        tracer.wait_with_output_completion(),
                    )
                    .await
                }
                Ok(ToolRunOutcome::Complete(_)) => unreachable!(),
            };
            if let Ok(ToolRunOutcome::CleanupPending(pending)) = rescue {
                eprintln!(
                    "private signal rescue remains unconfirmed: {:#}",
                    pending.quarantine()
                );
            }
            panic!("private signal fixture did not Complete under its original deadline");
        }
    };
    // Establish real terminal/reap before the causal semantic assertion in
    // BOTH images. A cleanup failure cannot count as the intended old failure.
    assert!(started.elapsed() <= Duration::from_secs(5));
    assert!(
        open_error.is_none(),
        "original pidfd acquisition failed: {open_error:?}"
    );
    let pidfd = pidfd.unwrap();
    let mut poll = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&mut poll, 1, 0) },
        1,
        "original pidfd not terminal"
    );
    // A reaped pidfd also reports HUP; request/read POLLIN, reject invalid FDs.
    assert_ne!(poll.revents & libc::POLLIN, 0);
    assert_eq!(poll.revents & (libc::POLLERR | libc::POLLNVAL), 0);
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(root.as_raw(), &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(Errno::last(), Errno::ECHILD, "original owner did not reap");
    let events = completed.global_state.0.lock().unwrap().clone();
    eprintln!(
        "private signal: root={root}, queue={queue_signal}, original_terminal=true, reaped=true, events={events:?}, result={:?}",
        completed.result
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Observation::Armed(_)))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Observation::Prepared(_)))
            .count(),
        1
    );
    let results: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, Observation::ToolResult(..)))
        .collect();
    if queue_signal {
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Observation::Returned(..))),
            "pre-ENTRY signal must not publish a Returned observation"
        );
        if !results.is_empty() {
            assert_eq!(
                results,
                vec![&Observation::ToolResult(
                    root,
                    Err(Errno::ERESTARTSYS.into_raw())
                )],
                "before/omission must expose the original fabricated restart result, not a different failure"
            );
        }
        assert_eq!(
            results.len(),
            0,
            "pre-ENTRY signal must not fabricate a Tool result"
        );
        let failure = completed
            .result
            .expect_err("unsupported continuation became native success");
        assert_eq!(
            failure.origin(),
            reverie::BackendFailure {
                pid: root,
                tid: root,
                phase: "ptrace private syscall before ENTRY",
            }
        );
        assert!(matches!(failure.primary(), Error::Tool(_)));
        assert!(
            failure
                .primary()
                .to_string()
                .contains("logical signal continuation is unsupported")
        );
        let prefix = failure.captured_prefix().expect("requested output capture");
        assert!(
            prefix.stdout().is_empty(),
            "guest continued after refused private attempt"
        );
        assert!(prefix.stderr().is_empty());
        assert_eq!(
            events,
            vec![
                Observation::Armed(root),
                Observation::Prepared(root),
                Observation::ThreadExit(root, ExitStatus::Signaled(Signal::SIGKILL, false)),
                Observation::ProcessExit(root, ExitStatus::Signaled(Signal::SIGKILL, false)),
            ]
        );
    } else {
        let output = completed.result.expect("ordinary private call refused");
        assert_eq!(output.status, ExitStatus::Exited(0));
        assert_eq!(output.stdout, b"private-returned\n");
        assert!(output.stderr.is_empty());
        assert_eq!(
            events,
            vec![
                Observation::Armed(root),
                Observation::Prepared(root),
                Observation::Returned(root, root.as_raw() as i64),
                Observation::ToolResult(root, Ok(root.as_raw() as i64)),
                Observation::ThreadExit(root, ExitStatus::Exited(0)),
                Observation::ProcessExit(root, ExitStatus::Exited(0)),
            ]
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn private_preentry_signal_refuses_without_tool_result_and_retires() {
    run(true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_private_getpid_returns_original_result_and_retires() {
    run(false).await;
}
