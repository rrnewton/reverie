//! Native coupled continuation controls. UNRUN source candidate; not pure tests.
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::time::Instant;

use reverie::InjectedReadResult;
use reverie::PrivateInterruption;
use reverie::PrivateInterruptionAction;
use reverie::PrivateReadCompletion;
use reverie::Tid;
use serde::Deserialize;
use serde::Serialize;

use super::*;

type TimerObservation = (i32, i32, i32, i32, u64, bool, bool);

#[derive(Default, Serialize, Deserialize)]
struct ContinuationState {
    fd: Option<i32>,
    mode: u8,
    armed: bool,
    in_call: bool,
    completed: Option<i64>,
    pending_helper: bool,
    #[serde(skip)]
    pending_read: Option<PrivateInterruption>,
    #[serde(skip)]
    last_completion: Option<PrivateReadCompletion>,
    timer_observing: bool,
    timer_rearmed: bool,
    timer_start: Option<u64>,
    cancelled_markers: usize,
}
#[derive(Default)]
struct ContinuationLog {
    posthooks: AtomicUsize,
    handler_calls: AtomicUsize,
    interruptions: AtomicUsize,
    partial_native: AtomicUsize,
    helper_entries: AtomicUsize,
    helper_returns: AtomicUsize,
    exits: AtomicUsize,
    exit_states: std::sync::Mutex<Vec<(bool, ExitStatus)>>,
    read_settlements: AtomicUsize,
    read_tails: AtomicUsize,
    timer_requests: AtomicUsize,
    timers: AtomicUsize,
    timer_done: AtomicUsize,
    timer_observations: std::sync::Arc<std::sync::Mutex<Vec<TimerObservation>>>,
    timer_clocks: std::sync::Mutex<Vec<(u64, u64)>>,
}
#[reverie::global_tool]
impl GlobalTool for ContinuationLog {
    type Config = u16;
    type Request = (bool, ExitStatus);
    type Response = ();
    async fn receive_rpc(&self, _: Pid, event: (bool, ExitStatus)) {
        self.exits.fetch_add(1, Ordering::SeqCst);
        self.exit_states.lock().unwrap().push(event);
    }
}
#[derive(Default)]
struct ContinuationTool;
impl ContinuationTool {
    fn marker_matches(row: &TimerObservation, tid: Tid, cancelled: bool) -> bool {
        row.0 == tid.as_raw()
            && row.1 == reverie::PERF_EVENT_SIGNAL as i32
            && row.2 == libc::SI_TKILL
            && row.3 == unsafe { libc::getpid() }
            && row.5 == cancelled
            && row.6
    }
    fn post<G: Guest<Self>>(guest: &mut G) {
        assert!(guest.thread_state().in_call, "one original finalizer owner");
        guest.thread_state_mut().in_call = false;
        guest
            .local_global_state()
            .unwrap()
            .posthooks
            .fetch_add(1, Ordering::SeqCst);
    }
}
#[reverie::tool]
impl Tool for ContinuationTool {
    type GlobalState = ContinuationLog;
    type ThreadState = ContinuationState;
    fn subscriptions(_: &u16) -> Subscription {
        [Sysno::read, Sysno::write].into_iter().collect()
    }
    fn observe_injected_syscalls(_: &u16) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        _: Pid,
        log: &ContinuationLog,
        state: &mut ContinuationState,
        nr: Sysno,
        _: reverie::syscalls::SyscallArgs,
        event: reverie::InjectedSyscallEvent,
    ) {
        if state.pending_helper && nr == Sysno::getpid {
            match event {
                reverie::InjectedSyscallEvent::Entered => {
                    log.helper_entries.fetch_add(1, Ordering::SeqCst);
                }
                reverie::InjectedSyscallEvent::Returned(_) => {
                    log.helper_returns.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }
    async fn handle_private_interruption<G: Guest<Self>>(
        &self,
        guest: &mut G,
        event: &PrivateInterruption,
    ) -> Result<PrivateInterruptionAction, Error> {
        let forged =
            PrivateInterruption::new_backend(event.logical_call(), event.helper(), event.signal());
        assert!(
            guest.claim_private_interruption(&forged).is_err(),
            "equal numeric facts must not authenticate another stop"
        );
        guest.claim_private_interruption(event)?;
        assert!(
            guest.claim_private_interruption(event).is_err(),
            "the original held-stop claim is one-use"
        );
        let (nr, args) = event.logical_call();
        let state = guest.thread_state();
        assert!(state.in_call && state.pending_helper && !state.armed);
        assert_eq!(event.helper(), Getpid::new().into_parts());
        assert_eq!(event.signal(), Signal::SIGUSR1);
        guest
            .local_global_state()
            .unwrap()
            .interruptions
            .fetch_add(1, Ordering::SeqCst);
        if state.mode >= 6 {
            assert_eq!(nr, Sysno::write);
            assert_eq!(args.arg0, 784);
            return Ok(PrivateInterruptionAction::DrainHelperResult);
        }
        assert_eq!(nr, Sysno::read);
        assert_eq!(Some(args.arg0 as i32), state.fd);
        assert_eq!(args.arg2, 6);
        let completed = state.completed;
        let early = PrivateReadCompletion::new_backend(event.clone(), completed);
        assert!(
            guest.claim_private_read_completion(&early).is_err(),
            "first settlement phase has no logical-register handback authority"
        );
        assert!(guest.thread_state().pending_read.is_none());
        guest.thread_state_mut().pending_read = Some(event.clone());
        guest
            .local_global_state()
            .unwrap()
            .read_settlements
            .fetch_add(1, Ordering::SeqCst);
        Ok(PrivateInterruptionAction::FinishRead { completed })
    }
    async fn handle_private_read_completion<G: Guest<Self>>(
        &self,
        guest: &mut G,
        event: &PrivateReadCompletion,
    ) -> Result<(), Error> {
        let state = guest.thread_state();
        let pending = state
            .pending_read
            .as_ref()
            .expect("one settled original Read");
        assert!(pending.same(event.interruption()));
        assert_eq!(pending.logical_call(), event.logical_call());
        assert_eq!(state.completed, event.completed());
        assert!(state.in_call && state.pending_helper);
        let forged = PrivateReadCompletion::new_backend(pending.clone(), event.completed());
        assert!(
            guest.claim_private_read_completion(&forged).is_err(),
            "same facts do not authenticate another completion"
        );
        let foreign = PrivateInterruption::new_backend(
            Getpid::new().into_parts(),
            event.interruption().helper(),
            event.interruption().signal(),
        );
        let wrong_call = PrivateReadCompletion::new_backend(foreign, event.completed());
        assert!(
            guest.claim_private_read_completion(&wrong_call).is_err(),
            "wrong logical call cannot claim this handback"
        );
        guest.claim_private_read_completion(event)?;
        assert!(
            guest.claim_private_read_completion(event).is_err(),
            "completion claim is one-use"
        );
        let expected = event
            .completed()
            .unwrap_or(-i64::from(Errno::ERESTARTSYS.into_raw())) as u64;
        let actual = guest.regs().await;
        assert_eq!(
            actual.rax, expected,
            "common tail sees installed logical Read result"
        );
        assert_eq!(actual.orig_rax, Sysno::read as u64);
        assert_eq!(actual.rdi, event.logical_call().1.arg0 as u64);
        assert_eq!(actual.rdx, 6);
        // Independent read-only ptrace observation prevents a projected regs()
        // value from making this pass while the physical task is still Getpid.
        let mut physical = std::mem::MaybeUninit::<libc::user_regs_struct>::uninit();
        assert_eq!(
            unsafe {
                libc::ptrace(
                    libc::PTRACE_GETREGS,
                    guest.tid().as_raw(),
                    std::ptr::null_mut::<libc::c_void>(),
                    physical.as_mut_ptr().cast::<libc::c_void>(),
                )
            },
            0
        );
        let physical = unsafe { physical.assume_init() };
        assert_eq!(
            (physical.rax, physical.orig_rax, physical.rip, physical.rsp),
            (actual.rax, actual.orig_rax, actual.rip, actual.rsp),
            "logical observer and actual held task must agree"
        );
        let mut forbidden = actual;
        forbidden.rax ^= 1;
        assert!(
            guest.set_regs(forbidden).await.is_err(),
            "tail cannot replace the installed result"
        );
        let mut canonical = actual;
        canonical.rcx = actual.rip;
        canonical.r11 = actual.eflags;
        guest.set_regs(canonical).await?;
        let readback = guest.regs().await;
        assert_eq!(
            (readback.rax, readback.rcx, readback.r11),
            (expected, canonical.rcx, canonical.r11)
        );
        let original = guest.thread_state_mut().pending_read.take().unwrap();
        assert!(original.same(event.interruption()));
        guest.thread_state_mut().last_completion = Some(event.clone());
        guest
            .local_global_state()
            .unwrap()
            .read_tails
            .fetch_add(1, Ordering::SeqCst);
        Self::post(guest);
        if *guest.config() & 0x100 != 0 {
            let observations = guest
                .local_global_state()
                .unwrap()
                .timer_observations
                .clone();
            assert!(observations.lock().unwrap().is_empty());
            crate::timer::EXEC_SIGNAL_OBSERVATIONS.with(|slot| {
                assert!(
                    slot.borrow().is_none(),
                    "per-process observation starts empty"
                );
                *slot.borrow_mut() = Some(observations);
            });
            guest.thread_state_mut().timer_observing = true;
            guest.set_timer_precise(TimerSchedule::RcbsAndInstructions(1, 64))?;
            guest
                .local_global_state()
                .unwrap()
                .timer_requests
                .fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        let (nr, args) = call.into_parts();
        if nr == Sysno::write && args.arg0 == 882 {
            let state = guest.thread_state();
            let log = guest.local_global_state().unwrap();
            let observations = log.timer_observations.lock().unwrap();
            let ready = *guest.config() & 0x100 != 0
                && state.timer_observing
                && !state.in_call
                && state.pending_read.is_none()
                && !state.timer_rearmed
                && log.read_settlements.load(Ordering::SeqCst) == 1
                && log.read_tails.load(Ordering::SeqCst) == 1
                && log.posthooks.load(Ordering::SeqCst) == 1
                && log.timer_requests.load(Ordering::SeqCst) == 1
                && log.timers.load(Ordering::SeqCst) == 0
                && !observations.is_empty()
                && observations
                    .iter()
                    .all(|row| Self::marker_matches(row, guest.tid(), true));
            let cancelled = observations.len();
            drop(observations);
            // Let the wrapper retire normally with an exact failure code, so
            // a causal omission is judged only after actual original cleanup.
            if !ready {
                return Ok(-i64::from(libc::EPROTO));
            }
            let start = guest.read_clock()?;
            guest.set_timer_precise(TimerSchedule::RcbsAndInstructions(1, 64))?;
            let state = guest.thread_state_mut();
            state.timer_start = Some(start);
            state.timer_rearmed = true;
            state.cancelled_markers = cancelled;
            guest
                .local_global_state()
                .unwrap()
                .timer_requests
                .fetch_add(1, Ordering::SeqCst);
            return Ok(0);
        }
        if nr == Sysno::write && args.arg0 == 883 {
            let state = guest.thread_state();
            let log = guest.local_global_state().unwrap();
            let observations = log.timer_observations.lock().unwrap();
            let clocks = log.timer_clocks.lock().unwrap();
            let ready = *guest.config() & 0x100 != 0
                && state.timer_rearmed
                && !state.in_call
                && state.pending_read.is_none()
                && log.timer_requests.load(Ordering::SeqCst) == 2
                && log.timers.load(Ordering::SeqCst) == 1
                && log.timer_done.load(Ordering::SeqCst) == 0
                && observations.len() == state.cancelled_markers + 1
                && observations[..state.cancelled_markers]
                    .iter()
                    .all(|row| Self::marker_matches(row, guest.tid(), true))
                && observations
                    .last()
                    .is_some_and(|row| Self::marker_matches(row, guest.tid(), false))
                && clocks.len() == 1
                && state
                    .timer_start
                    .is_some_and(|start| clocks[0] == (start, start + 1));
            if !ready {
                return Ok(-i64::from(libc::EPROTO));
            }
            log.timer_done.fetch_add(1, Ordering::SeqCst);
            return Ok(0);
        }
        if nr == Sysno::write && args.arg0 == 880 {
            let s = guest.thread_state_mut();
            s.fd = Some(args.arg1 as i32);
            s.mode = args.arg2 as u8;
            s.armed = true;
            return Ok(0);
        }
        if nr == Sysno::write && args.arg0 == 879 {
            assert!(
                !guest.thread_state().in_call,
                "handler before logical finalizer"
            );
            if let Some(stale) = guest.thread_state().last_completion.clone() {
                assert!(
                    guest.claim_private_read_completion(&stale).is_err(),
                    "released handler cannot reuse a completed handback"
                );
                assert!(guest.thread_state().pending_read.is_none());
            }
            assert_eq!(
                guest
                    .local_global_state()
                    .unwrap()
                    .posthooks
                    .load(Ordering::SeqCst),
                1
            );
            guest
                .local_global_state()
                .unwrap()
                .handler_calls
                .fetch_add(1, Ordering::SeqCst);
            return Ok(1);
        }
        let selected = (nr == Sysno::read && guest.thread_state().fd == Some(args.arg0 as i32))
            || (nr == Sysno::write && args.arg0 == 784);
        if !selected {
            return Ok(guest.inject(call).await?);
        }
        if !guest.thread_state().armed {
            return Ok(guest.inject(call).await?);
        }
        guest.thread_state_mut().armed = false;
        guest.thread_state_mut().in_call = true;
        let mode = guest.thread_state().mode;
        if mode <= 1 || mode == 4 || mode == 5 {
            let Syscall::Read(read) = call else {
                panic!("partial owner must be Read")
            };
            let actual = guest.inject_original_read(read.with_len(3)).await;
            assert!(
                matches!(actual, InjectedReadResult::Complete(Ok(3))),
                "partial count comes from a real three-byte native Read"
            );
            guest.thread_state_mut().completed = Some(3);
            guest
                .local_global_state()
                .unwrap()
                .partial_native
                .fetch_add(1, Ordering::SeqCst);
        }
        let pid = guest.inject(Getpid::new()).await?;
        let tid = guest.inject(Gettid::new()).await?;
        guest
            .inject(
                Tgkill::new()
                    .with_tgid(pid as _)
                    .with_tid(tid as _)
                    .with_sig(libc::SIGUSR1),
            )
            .await?;
        guest.thread_state_mut().pending_helper = true;
        let result = guest.inject(Getpid::new()).await?;
        assert!(
            mode >= 6,
            "Read handback must not invent a private helper result"
        );
        Self::post(guest);
        Ok(result)
    }
    async fn on_exit_thread<G: reverie::GlobalRPC<ContinuationLog>>(
        &self,
        _: Tid,
        global: &G,
        state: ContinuationState,
        status: ExitStatus,
    ) -> Result<(), Error> {
        if state.timer_observing {
            crate::timer::EXEC_SIGNAL_OBSERVATIONS.with(|slot| {
                assert!(
                    slot.borrow_mut().take().is_some(),
                    "retire per-process observation owner"
                );
            });
        }
        global.send_rpc((state.in_call, status)).await;
        Ok(())
    }
    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        assert!(!guest.thread_state().in_call);
        assert_eq!(
            guest
                .local_global_state()
                .unwrap()
                .posthooks
                .load(Ordering::SeqCst),
            1
        );
        assert!(guest.thread_state().timer_rearmed);
        assert!(guest.thread_state().pending_read.is_none());
        let start = guest
            .thread_state()
            .timer_start
            .expect("actual later rearm boundary");
        let actual = guest.read_clock().unwrap();
        assert_eq!(
            actual,
            start.checked_add(1).unwrap(),
            "exact one-RCB target, no skid tolerance"
        );
        let log = guest.local_global_state().unwrap();
        assert_eq!(
            log.timers.fetch_add(1, Ordering::SeqCst),
            0,
            "one eligible timer callback"
        );
        log.timer_clocks.lock().unwrap().push((start, actual));
    }
}
async fn continuation_case(mode: u8) {
    continuation_case_with_timer(mode, false).await;
}
async fn continuation_case_with_timer(mode: u8, request_timer: bool) {
    let fixture_variable = if request_timer {
        "REVERIE_PRIVATE_TIMER_FIXTURE"
    } else {
        "REVERIE_PRIVATE_CONTINUATION_FIXTURE"
    };
    let fixture = PathBuf::from(crate::testing::fixture_path(fixture_variable));
    assert!(fixture.is_absolute(), "fixture must not use PATH lookup");
    let mut command = Command::new(fixture);
    command.arg(mode.to_string());
    command.stdout(reverie::process::Stdio::piped());
    command.stderr(reverie::process::Stdio::piped());
    let started = Instant::now();
    let deadline = started + Duration::from_secs(5);
    let config = u16::from(mode) | (u16::from(request_timer) << 8);
    let tracer = TracerBuilder::<ContinuationTool>::new(command)
        .config(config)
        .spawn()
        .await
        .unwrap();
    let root = tracer.guest_pid();
    let termination = tracer
        .termination_handle()
        .expect("ordinary original owner");
    let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, root.as_raw(), 0) };
    let open_error = (raw_fd == -1).then(std::io::Error::last_os_error);
    let pidfd = (raw_fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(raw_fd as i32) });
    if let Some(error) = &open_error {
        termination.terminate(anyhow::anyhow!("continuation original pidfd open: {error}").into());
    }
    let mut completion = Box::pin(tracer.wait_with_output_completion());
    let original = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        &mut completion,
    )
    .await;
    let complete = match original {
        Ok(ToolRunOutcome::Complete(complete)) => complete,
        other => {
            termination.terminate(anyhow::anyhow!("continuation original deadline/refusal").into());
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
                    "continuation rescue unconfirmed: {:#}",
                    pending.quarantine()
                );
            }
            panic!("continuation did not Complete under its original deadline");
        }
    };
    // All causal oracles follow actual original terminal/reap. Cleanup failure
    // cannot become a qualifying signal/Read failure.
    assert!(started.elapsed() <= Duration::from_secs(5));
    assert!(
        open_error.is_none(),
        "original pidfd acquisition: {open_error:?}"
    );
    let pidfd = pidfd.unwrap();
    let mut poll = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
    assert_ne!(poll.revents & libc::POLLIN, 0);
    assert_eq!(poll.revents & (libc::POLLERR | libc::POLLNVAL), 0);
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(root.as_raw(), &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(Errno::last(), Errno::ECHILD, "original owner did not reap");
    drop(pidfd);
    let output = complete.result.unwrap();
    let log = complete.global_state;
    if request_timer {
        eprintln!(
            "ACTUAL_PRIVATE_TIMER requests={} callbacks={} done={} observations={:?} clocks={:?}",
            log.timer_requests.load(Ordering::SeqCst),
            log.timers.load(Ordering::SeqCst),
            log.timer_done.load(Ordering::SeqCst),
            log.timer_observations.lock().unwrap(),
            log.timer_clocks.lock().unwrap()
        );
    }
    assert_eq!(output.status, ExitStatus::Exited(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(
        *log.exit_states.lock().unwrap(),
        vec![(false, ExitStatus::Exited(0))],
        "no continuation left across handler escape/exit"
    );
    assert_eq!(log.interruptions.load(Ordering::SeqCst), 1);
    assert_eq!(log.posthooks.load(Ordering::SeqCst), 1);
    assert_eq!(log.handler_calls.load(Ordering::SeqCst), 1);
    assert_eq!(log.exits.load(Ordering::SeqCst), 1);
    assert_eq!(
        log.partial_native.load(Ordering::SeqCst),
        usize::from(mode <= 1 || mode == 4 || mode == 5)
    );
    assert_eq!(
        log.helper_entries.load(Ordering::SeqCst),
        usize::from(mode >= 6)
    );
    assert_eq!(
        log.helper_returns.load(Ordering::SeqCst),
        usize::from(mode >= 6)
    );
    assert_eq!(
        log.read_settlements.load(Ordering::SeqCst),
        usize::from(mode < 6)
    );
    assert_eq!(log.read_tails.load(Ordering::SeqCst), usize::from(mode < 6));
    assert_eq!(
        log.timer_requests.load(Ordering::SeqCst),
        2 * usize::from(request_timer)
    );
    assert_eq!(
        log.timers.load(Ordering::SeqCst),
        usize::from(request_timer)
    );
    assert_eq!(
        log.timer_done.load(Ordering::SeqCst),
        usize::from(request_timer)
    );
}
#[tokio::test(flavor = "current_thread")]
async fn actual_partial_three_without_restart() {
    continuation_case(0).await;
}
#[tokio::test(flavor = "current_thread")]
async fn actual_partial_three_with_restart() {
    continuation_case(1).await;
}
#[tokio::test(flavor = "current_thread")]
async fn zero_progress_without_restart_is_real_eintr() {
    continuation_case(2).await;
}
#[tokio::test(flavor = "current_thread")]
async fn zero_progress_with_restart_reenters_original_read() {
    continuation_case(3).await;
}
#[tokio::test(flavor = "current_thread")]
async fn partial_handler_ucontext_edit_is_not_overwritten() {
    continuation_case(4).await;
}
#[tokio::test(flavor = "current_thread")]
async fn partial_handler_escape_has_no_parked_future() {
    continuation_case(5).await;
}
#[tokio::test(flavor = "current_thread")]
async fn finite_drain_handler_sees_actual_result_and_siginfo() {
    continuation_case(6).await;
}
#[tokio::test(flavor = "current_thread")]
async fn finite_drain_handler_ucontext_edit_is_not_overwritten() {
    continuation_case(7).await;
}
#[tokio::test(flavor = "current_thread")]
async fn finite_drain_handler_escape_has_no_parked_future() {
    continuation_case(8).await;
}
#[tokio::test(flavor = "current_thread")]
async fn partial_read_handback_cancels_then_rearms_precise_timer() {
    continuation_case_with_timer(0, true).await;
}
#[tokio::test(flavor = "current_thread")]
async fn interrupted_read_handback_cancels_then_rearms_precise_timer() {
    continuation_case_with_timer(2, true).await;
}
