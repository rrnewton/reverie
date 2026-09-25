/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Tracer` type, plus ways to spawn it and retrieve its output.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock as StdOnceLock;
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::thread::ThreadId;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use close_err::Closable;
use futures::future;
use futures::future::BoxFuture;
use futures::future::Either;
use futures::stream::StreamExt;
use nix::sys::ptrace;
use nix::sys::signal;
use nix::sys::signal::Signal;
use nix::unistd;
use nix::unistd::ForkResult;
use reverie::BackendStatsRequest;
use reverie::Errno;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::process::ChildStderr;
use reverie::process::ChildStdin;
use reverie::process::ChildStdout;
use reverie::process::Command;
use reverie::process::Output;
use reverie::process::seccomp;
use reverie::syscalls::Sysno;
use safeptrace::ChildOp;
use safeptrace::Error as TraceError;
use safeptrace::Event;
use safeptrace::Running;
use safeptrace::Stopped;
use safeptrace::TerminalCleanup;
use safeptrace::Wait;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::LiteinstInstrumentationStats;
use crate::LiteinstInstrumentationStatsHandle;
use crate::PtraceBackendStatsSource;
use crate::cp;
use crate::gdbstub::GdbServer;
use crate::task::Child;
use crate::task::FatalSession;
use crate::task::InjectedSyscallProvenance;
use crate::task::InjectedSyscallTrap;
use crate::task::LiteinstRuntimeConfig;
#[cfg(test)]
use crate::task::RootStopPause;
use crate::task::TracedTask;
use crate::task::TracedTaskOptions;

/// Represents the tracer.
///
/// We need to simultaneously capture stderr/stdout while handling events. These
/// can be two separate futures. The stderr/stdout future will finish when the
/// pipes are closed.
///
/// The stderr/stdout capture can be a `Stream<Item = Either<Bytes, Bytes>>`
/// where each item is either a chunk of stderr bytes or stdout bytes. Zipping
/// together the two streams like this preserves ordering.
pub struct Tracer<G> {
    /// PID of the root guest process.
    guest_pid: Pid,

    // Future of the running handler.
    tracer: BoxFuture<'static, Result<ExitStatus, Error>>,

    // A reference to the global state.
    gref: Arc<G>,

    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,

    // Dynamic LiteInst keeps its established session guard. Static injected
    // traps use ordinary stopped-task ownership internally, while their public
    // completion API remains explicitly unsupported.
    liteinst_cleanup: Option<LiteinstTraceeCleanup>,
    liteinst_instrumentation_stats: Option<Arc<StdMutex<LiteinstInstrumentationStats>>>,

    // Present only when the caller requested general ptrace activity stats.
    backend_stats: Option<PtraceBackendStatsSource>,
    ordinary_session: Arc<FatalSession>,
    ptracer_thread: ThreadId,
    ordinary_completion_supported: bool,
}

struct LegacyInjectedOwner<G> {
    tracer: Tracer<G>,
    local: tokio::task::LocalSet,
    stdout: crate::capture::CaptureDrain,
    stderr: crate::capture::CaptureDrain,
    permit: Option<QuarantinePermit>,
    // The diagnostic may be dropped while cleanup is still unconfirmed. Keep
    // the identical primary, secondary errors, and captured bytes with its owner.
    failure: Option<Arc<crate::PtraceRunFailure>>,
}

/// An injected-mode run failed and its original guard could not confirm cleanup.
///
/// The guard, task futures, GlobalTool, and output readers remain deliberately
/// retained on the original ptracer thread, even if this diagnostic is dropped.
/// This is not successful cleanup and offers no public resume operation. New
/// spawns are refused; retained resources live until process exit, with the
/// existing PTRACE_O_EXITKILL policy. Callers must retain their own resources
/// needed by this guest as well.
#[derive(Debug, thiserror::Error)]
#[error("injected ptrace cleanup unconfirmed; original owner {id} retained: {failure}")]
pub struct InjectedCleanupUnconfirmed {
    id: u64,
    failure: Arc<crate::PtraceRunFailure>,
}

impl InjectedCleanupUnconfirmed {
    /// Original typed cause, actual later cleanup failures, and captured prefix.
    pub fn failure(&self) -> &crate::PtraceRunFailure {
        &self.failure
    }
}

struct LiteinstTraceeCleanup {
    identity: TraceeIdentity,
    newborn_tracees: Arc<StdMutex<HashMap<Pid, NewbornTracee>>>,
    armed: bool,
    terminal: Option<TerminalCleanup>,
    notifier_owner: Option<ThreadId>,
    retained_descendants: HashMap<Pid, RegisteredTraceeCleanup>,
    retained_terminal_descendants: HashMap<Pid, TraceeIdentity>,
    // Fatal injected callbacks request stronger confirmation than the legacy
    // ownership policy. These generations are already notifier-terminal and
    // confer no further signaling, ptrace, or natural-parent wait authority.
    fatal_terminal_observations: Option<HashMap<Pid, TraceeIdentity>>,
    held_root_stop: Arc<StdMutex<Option<HeldRootStop>>>,
    root_frozen: bool,
    #[cfg(test)]
    fail_discovery_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    fail_discovery_while: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    fail_after_scan_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    force_task_scan_once: Option<Arc<AtomicBool>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TraceeSnapshot {
    tgid: Pid,
    ppid: Pid,
    tracer_pid: Pid,
    start_time: u64,
}

#[derive(Debug)]
pub(crate) struct TraceeIdentity {
    tid: Pid,
    snapshot: TraceeSnapshot,
    proc_dir: OwnedFd,
    proc_inode: u64,
    pidfd: Option<OwnedFd>,
    parent: Option<(Pid, Pid, Option<ChildOp>)>,
}

pub(crate) struct NewbornTracee {
    link: EventChildLink,
    identity: Option<TraceeIdentity>,
    terminal: TerminalCleanup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EventChildLink {
    tid: Pid,
    parent_tid: Pid,
    op: ChildOp,
}

pub(crate) struct HeldRootStop {
    terminal: TerminalCleanup,
    observation: safeptrace::StoppedObservation,
    root_tid: Pid,
    status: HeldRootStopStatus,
    armed: bool,
}

/// Ordinary cancellation retains the same event/stop authority as the running
/// task. No new procfs identity or descriptor is captured for this guard.
pub(crate) struct FatalTaskStop {
    pub(crate) tid: Pid,
    pub(crate) terminal: TerminalCleanup,
    pub(crate) held: Arc<StdMutex<Option<HeldRootStop>>>,
    pub(crate) frozen: AtomicBool,
}

pub(crate) struct FatalNewborn {
    pub(crate) tid: Pid,
    pub(crate) parent: Pid,
    pub(crate) handed: bool,
    pub(crate) terminal: Arc<TerminalCleanup>,
    exit: BoxFuture<'static, Result<Stopped, TraceError>>,
}

impl FatalNewborn {
    pub(crate) fn new(parent: Pid, child: &Running) -> Self {
        Self {
            tid: child.pid(),
            parent,
            handed: false,
            terminal: Arc::new(child.terminal_cleanup()),
            exit: Box::pin(child.exit_event()),
        }
    }

    pub(crate) fn signal(&self) -> Result<(), Errno> {
        #[cfg(test)]
        if self.is_live_stop_opponent() {
            return Ok(());
        }
        self.terminal.request_sigkill()
    }

    #[cfg(test)]
    fn is_live_stop_opponent(&self) -> bool {
        crate::task::FATAL_FORK_PAUSE.with(|slot| {
            slot.borrow().as_ref().is_some_and(|pause| {
                pause.live_stop_opponent.load(Ordering::SeqCst)
                    && pause
                        .child
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|child| child.pid() == self.tid)
            })
        })
    }

    #[cfg(test)]
    async fn reap(self) -> Result<(), Error> {
        while let Some(pending) = self.terminal.reserve_pending_for_cleanup(Duration::ZERO) {
            let _stopped = pending.decode().map_err(anyhow::Error::new)?;
            pending.commit();
        }
        let stopped = self.exit.await.map_err(anyhow::Error::new)?;
        let status = stopped
            .resume(None)
            .map_err(anyhow::Error::new)?
            .next_state()
            .await
            .map_err(anyhow::Error::new)?
            .assume_exited()
            .1;
        if status != ExitStatus::Signaled(Signal::SIGKILL, false)
            || !self.terminal.wait(Duration::from_secs(2))
        {
            return Err(
                anyhow::anyhow!("test rescue did not observe actual SIGKILL retirement").into(),
            );
        }
        Ok(())
    }

    pub(crate) fn into_exit(self) -> BoxFuture<'static, Result<Stopped, TraceError>> {
        self.exit
    }

    pub(crate) async fn reap_owned(mut self, session: &FatalSession) {
        #[cfg(test)]
        if self.is_live_stop_opponent() {
            return;
        }
        while let Some(pending) = self.terminal.reserve_pending_for_cleanup(Duration::ZERO) {
            match pending.decode() {
                Ok(_) => pending.commit(),
                Err(error) => {
                    drop(pending);
                    session.retry_after(anyhow::Error::new(error).into()).await;
                }
            }
        }
        let stopped = loop {
            match (&mut self.exit).await {
                Ok(stopped) => break Ok(stopped),
                Err(TraceError::Died(zombie)) => break Err(TraceError::Died(zombie)),
                Err(error) => session.retry_after(anyhow::Error::new(error).into()).await,
            }
        };
        let held = Arc::new(StdMutex::new(None));
        let mut next = stopped;
        let status = loop {
            match finish_ordinary_terminal(next, &self.terminal, &held, session).await {
                OrdinaryTerminal::Exited(status, _) => break status,
                OrdinaryTerminal::Exec {
                    stopped, former, ..
                } => {
                    session.fail(
                        anyhow::anyhow!(
                            "uninitialized newborn {} from parent {} unexpectedly replaced former task {former}", self.tid, self.parent
                        )
                        .into(),
                    );
                    for error in session.signal_groups() {
                        session.retry_after(error.into()).await;
                    }
                    // Retain the actual replacement stop; this uninitialized
                    // owner has no former Tool state to hand over.
                    next = Ok(stopped);
                }
            }
        };
        #[cfg(test)]
        crate::task::FATAL_FORK_PAUSE.with(|slot| {
            if let Some(pause) = slot.borrow().as_ref() {
                *pause.terminal_status.lock().unwrap() = Some((self.tid, status));
            }
        });
        let _ = status;
        session.finished_unhanded(&self.terminal);
    }
}

/// Individual monotonic flag reads made when the sole owner receives a result.
/// These do not claim an atomic multi-field kernel or failure-state snapshot.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OrdinaryReceipt {
    pub(crate) failure_published: bool,
    pub(crate) backend_signalling: bool,
}

#[cfg(test)]
#[derive(Default)]
struct ExitPayloadControl {
    payload: StdMutex<Option<(Pid, Pid, ExitStatus)>>,
    restore_for_teardown: AtomicBool,
}
#[cfg(test)]
thread_local! {
    static EXIT_PAYLOAD_CONTROL: std::cell::RefCell<Option<Arc<ExitPayloadControl>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[derive(Default)]
struct ExitResumeControl {
    target: std::sync::atomic::AtomicUsize,
    entered: AtomicBool,
    release: AtomicBool,
    payload: std::sync::atomic::AtomicUsize,
    results: StdMutex<Vec<(Pid, Result<(), Errno>)>>,
}
#[cfg(test)]
thread_local! {
    static EXIT_RESUME_CONTROL: std::cell::RefCell<Option<Arc<ExitResumeControl>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Debug, thiserror::Error)]
#[error("actual exec for {pid} from {former} has no retained preceding EXIT event status")]
struct ExitEventStatusUnavailable {
    pid: Pid,
    former: Pid,
}

/// Actual outcomes after consuming the original exit-stop capability.
pub(crate) enum OrdinaryTerminal {
    Exited(ExitStatus, OrdinaryReceipt),
    Exec {
        stopped: Stopped,
        former: Pid,
        replaced_status: ExitStatus,
        receipt: OrdinaryReceipt,
    },
}

#[cfg(test)]
#[derive(Default)]
struct CallbackExitPause {
    after_receipt: bool,
    received: StdMutex<Option<(Pid, ExitStatus)>>,
    entered: AtomicBool,
    released: AtomicBool,
    changed: tokio::sync::Notify,
}
#[cfg(test)]
thread_local! {
    static CALLBACK_EXIT_PAUSE: std::cell::RefCell<Option<Arc<CallbackExitPause>>> = const { std::cell::RefCell::new(None) };
}

// Test-only readback of the existing physical owner, never a second wait owner.
#[cfg(test)]
thread_local! {
    static FATAL_REAP_OBSERVATIONS: std::cell::RefCell<Option<Vec<Arc<FatalTaskStop>>>> = const { std::cell::RefCell::new(None) };
    static FATAL_REAP_CHRONOLOGY: std::cell::RefCell<Option<(Vec<String>, usize)>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(crate) fn record_fatal_phase_for_test(message: impl FnOnce() -> String) {
    FATAL_REAP_CHRONOLOGY.with(|slot| {
        if let Some((records, omitted)) = slot.borrow_mut().as_mut() {
            if records.len() < 64 {
                records.push(message());
            } else {
                *omitted += 1;
            }
        }
    });
}
#[cfg(test)]
pub(crate) fn record_capacity_finished_for_test(stop: &FatalTaskStop) {
    tests::fatal_capacity_tests::record_finished(stop);
}
#[cfg(test)]
pub(crate) fn record_fatal_task_for_test(task: &Arc<FatalTaskStop>) {
    FATAL_REAP_OBSERVATIONS.with(|slot| {
        if let Some(observations) = slot.borrow_mut().as_mut() {
            observations.push(Arc::clone(task));
        }
    });
}

/// Fence an already observed terminal outcome on its original notifier.
/// This observes acknowledgement only; it never waits for a new kernel status.
pub(crate) async fn retire_ordinary_terminal(
    terminal: &TerminalCleanup,
    held: &Arc<StdMutex<Option<HeldRootStop>>>,
) {
    while !terminal.wait(Duration::ZERO) {
        tokio::task::yield_now().await;
    }
    // A queued stop may have been superseded by the actual terminal result.
    // Release its lease only after the original worker acknowledges retirement.
    held.lock().unwrap().take();
}

/// Own the actual exit capability and its successor wait across refusals.
pub(crate) async fn finish_ordinary_exit(
    stopped: Result<Stopped, TraceError>,
    stop: &FatalTaskStop,
    session: &FatalSession,
) -> OrdinaryTerminal {
    #[cfg(test)]
    pause_callback_exit_for_test(session, stop.tid, None).await;
    finish_ordinary_terminal(stopped, &stop.terminal, &stop.held, session).await
}

#[cfg(test)]
async fn pause_callback_exit_for_test(
    session: &FatalSession,
    tid: Pid,
    received: Option<ExitStatus>,
) {
    if let Some(control) = CALLBACK_EXIT_PAUSE.with(|slot| slot.borrow().clone())
        && control.after_receipt == received.is_some()
        && session.callback_diagnostics().iter().any(|record| {
            record.origin().tid == tid
                && record.decision() == crate::PtraceCallbackDecision::AwaitingOwner
        })
    {
        // Before resume: holds the actual EXIT capability. After receipt: holds
        // an actual terminal result before retirement is acknowledged; the
        // worker may already have retired. Neither barrier fabricates a status.
        *control.received.lock().unwrap() = received.map(|status| (tid, status));
        control.entered.store(true, Ordering::SeqCst);
        control.changed.notify_waiters();
        loop {
            let changed = control.changed.notified();
            if control.released.load(Ordering::SeqCst) {
                break;
            }
            changed.await;
        }
    }
}

async fn finish_ordinary_terminal(
    stopped: Result<Stopped, TraceError>,
    terminal: &TerminalCleanup,
    held: &Arc<StdMutex<Option<HeldRootStop>>>,
    session: &FatalSession,
) -> OrdinaryTerminal {
    let mut current = stopped;
    let mut exit_status = None;
    #[cfg(test)]
    record_fatal_phase_for_test(|| format!("finish_ordinary_terminal entered: {current:?}"));
    loop {
        let mut wait = match current {
            Ok(mut stopped) => {
                loop {
                    if let Err(error) = HeldRootStop::supersede_with_exit(held, &stopped) {
                        session.retry_after(anyhow::Error::new(error).into()).await;
                        continue;
                    }
                    if exit_status.is_none() {
                        let observed_event = stopped.getevent();
                        #[cfg(test)]
                        record_fatal_phase_for_test(|| {
                            format!(
                                "finish getevent: tid={}, result={observed_event:?}, terminal={:?}, retired={}",
                                stopped.pid(),
                                terminal.observed_exit_status(),
                                terminal.wait(Duration::ZERO)
                            )
                        });
                        match observed_event {
                            Ok(raw) => {
                                exit_status = Some(ExitStatus::from_raw(raw as i32));
                                #[cfg(test)]
                                if let Some(control) =
                                    EXIT_RESUME_CONTROL.with(|slot| slot.borrow().clone())
                                    && control.target.load(Ordering::SeqCst)
                                        == stopped.pid().as_raw() as usize
                                    && !control.entered.swap(true, Ordering::SeqCst)
                                {
                                    control.payload.store(raw as usize, Ordering::SeqCst);
                                    while !control.release.load(Ordering::SeqCst) {
                                        tokio::task::yield_now().await;
                                    }
                                }
                            }
                            Err(TraceError::Died(zombie)) => {
                                // A synchronous transition (for example a timer
                                // step) can have advanced the physical EXIT
                                // stop before its queued ExitFuture is polled.
                                // ESRCH does not supply an exit status. Drop the
                                // superseded capability and transfer this same
                                // generation to one retained wait; only its
                                // actual terminal/Exec result can settle it.
                                drop(stopped);
                                break zombie.wait_owned();
                            }
                            Err(error) => {
                                session.retry_after(anyhow::Error::new(error).into()).await;
                                continue;
                            }
                        }
                    }
                    #[cfg(test)]
                    let resumed_pid = stopped.pid();
                    let resumed = stopped.resume_retaining(None);
                    #[cfg(test)]
                    EXIT_RESUME_CONTROL.with(|slot| {
                        if let Some(control) = slot.borrow().as_ref() {
                            control.results.lock().unwrap().push((
                                resumed_pid,
                                resumed.as_ref().map(|_| ()).map_err(|(_, error)| *error),
                            ));
                        }
                    });
                    match resumed {
                        Ok(running) => {
                            // The sole capability has made the transition.
                            held.lock().unwrap().take();
                            break running.wait_owned();
                        }
                        Err((retained, Errno::ESRCH)) => {
                            // A real group-fatal signal may advance this EXIT
                            // after GETEVENTMSG and before CONT. Keep its sole
                            // generation owner; ESRCH itself supplies no status.
                            break retained.wait_owned();
                        }
                        Err((retained, error)) => {
                            stopped = retained;
                            session.retry_after(anyhow::Error::new(error).into()).await;
                        }
                    }
                }
            }
            Err(TraceError::Died(zombie)) => zombie.wait_owned(),
            Err(error) => {
                if let Ok(Some(status)) = terminal.observed_exit_status() {
                    let receipt = session.ordinary_receipt();
                    retire_ordinary_terminal(terminal, held).await;
                    return OrdinaryTerminal::Exited(status, receipt);
                }
                // The original ExitFuture remains with its task owner. This
                // path cannot invent a replacement stopped capability.
                session.retry_after(anyhow::Error::new(error).into()).await;
                return future::pending().await;
            }
        };
        let (state, receipt) = loop {
            match (&mut wait).await {
                Ok(state) => break (state, session.ordinary_receipt()),
                Err(error) => session.retry_after(anyhow::Error::new(error).into()).await,
            }
        };
        match state {
            Wait::Exited(pid, status) => {
                #[cfg(test)]
                pause_callback_exit_for_test(session, pid, Some(status)).await;
                #[cfg(not(test))]
                let _ = pid;
                retire_ordinary_terminal(terminal, held).await;
                return OrdinaryTerminal::Exited(status, receipt);
            }
            Wait::Stopped(stopped, event) => {
                if let Event::Exec(former) = event {
                    #[cfg(test)]
                    EXIT_PAYLOAD_CONTROL.with(|slot| {
                        if let Some(control) = slot.borrow().as_ref() {
                            let mut payload = control.payload.lock().unwrap();
                            if payload.is_none() {
                                *payload = Some((
                                    stopped.pid(),
                                    former,
                                    exit_status
                                        .take()
                                        .expect("test erases an actual captured EXIT payload"),
                                ));
                            }
                        }
                    });
                    let replaced_status = if let Some(status) = exit_status {
                        status
                    } else {
                        // Retain the genuine replacement stop. Losing the old
                        // EXIT payload cannot authorize a fabricated consuming
                        // hook, successful completion, or a panic that drops it.
                        *held.lock().unwrap() = Some(HeldRootStop::from_event(&stopped, &event));
                        loop {
                            #[cfg(test)]
                            if let Some(status) = EXIT_PAYLOAD_CONTROL.with(|slot| {
                                let control = slot.borrow();
                                let control = control.as_ref()?;
                                if !control.restore_for_teardown.load(Ordering::SeqCst) {
                                    return None;
                                }
                                let (pid, recorded_former, status) =
                                    control.payload.lock().unwrap().expect(
                                        "test-only restore needs its actual erased payload",
                                    );
                                assert_eq!((pid, recorded_former), (stopped.pid(), former));
                                Some(status)
                            }) {
                                break status;
                            }
                            session
                                .retry_after(
                                    anyhow::Error::new(ExitEventStatusUnavailable {
                                        pid: stopped.pid(),
                                        former,
                                    })
                                    .into(),
                                )
                                .await;
                        }
                    };
                    held.lock().unwrap().take();
                    return OrdinaryTerminal::Exec {
                        stopped,
                        former,
                        // Read from the actual preceding Exit stop, before
                        // resuming it. Never a fabricated former-task status.
                        replaced_status,
                        receipt,
                    };
                } else if event != Event::Exit {
                    session
                        .fail(anyhow::anyhow!("unexpected ptrace terminal stop: {event:?}").into());
                    for error in session.signal_groups() {
                        session.retry_after(error.into()).await;
                    }
                }
                current = Ok(stopped);
            }
        }
    }
}

impl FatalTaskStop {
    pub(crate) async fn freeze(
        &self,
        deadline: Instant,
        capture: impl Fn(Pid, ChildOp, &Running),
    ) -> Result<(), Error> {
        let already_stopped = {
            let held = self.held.lock().unwrap();
            if let Some(held) = held.as_ref() {
                if !held.armed
                    || held.root_tid != self.tid
                    || !held.terminal.same_generation(&self.terminal)
                {
                    return Err(
                        anyhow::anyhow!("fatal stop generation mismatch for {}", self.tid).into(),
                    );
                }
            } else {
                match self.terminal.request_sigstop() {
                    Ok(()) | Err(Errno::ESRCH) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            held.is_some()
        };
        let mut stopped = already_stopped;
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Tool(anyhow::Error::new(CleanupDeadlineExceeded)));
            }
            let Some(pending) = self.terminal.reserve_pending_for_cleanup(Duration::ZERO) else {
                if stopped
                    || self.terminal.exit_stop_observed()
                    || self.terminal.wait(Duration::ZERO)
                {
                    return Ok(());
                }
                // Other owned tasks, notably a vfork child at its EXIT stop,
                // need this same ptracer thread to make physical progress.
                // Retain the original authority and one monotonic deadline.
                tokio::task::yield_now().await;
                continue;
            };
            let wait = pending.decode().map_err(anyhow::Error::new)?;
            if let Wait::Stopped(task, Event::NewChild(op, child)) = &wait {
                capture(task.pid(), *op, child);
            }
            if let Wait::Stopped(task, event) = &wait {
                HeldRootStop::ensure_current(&self.held, task, event)
                    .map_err(anyhow::Error::new)?;
            }
            // Child ownership is retained before removing its parent's FIFO
            // front. The stopped task cannot create another child meanwhile.
            pending.commit();
            stopped = true;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeldRootStopStatus {
    Signal(Signal),
    NewChild(EventChildLink),
    Exec(Pid),
    VforkDone,
    Exit,
    Seccomp,
    Stop,
    Syscall,
}

impl HeldRootStop {
    pub(crate) fn from_event(task: &Stopped, event: &Event) -> Self {
        let status = match event {
            Event::Signal(signal) => HeldRootStopStatus::Signal(*signal),
            Event::NewChild(op, child) => HeldRootStopStatus::NewChild(EventChildLink {
                tid: child.pid(),
                parent_tid: task.pid(),
                op: *op,
            }),
            Event::Exec(pid) => HeldRootStopStatus::Exec(*pid),
            Event::VforkDone => HeldRootStopStatus::VforkDone,
            Event::Exit => HeldRootStopStatus::Exit,
            Event::Seccomp => HeldRootStopStatus::Seccomp,
            Event::Stop => HeldRootStopStatus::Stop,
            Event::Syscall => HeldRootStopStatus::Syscall,
        };
        Self {
            terminal: task.terminal_cleanup(),
            observation: task.observation(),
            root_tid: task.pid(),
            status,
            armed: true,
        }
    }

    pub(crate) fn callback_observation(
        &self,
        tid: Pid,
        terminal: &TerminalCleanup,
    ) -> Result<
        (crate::PtraceCallbackStop, safeptrace::StopObservationSample),
        crate::PtraceCallbackRefusal,
    > {
        use crate::PtraceCallbackStop as Stop;
        if !self.armed
            || self.root_tid != tid
            || !self.terminal.same_generation(terminal)
            || !self.observation.same_generation(terminal)
        {
            return Err(crate::PtraceCallbackRefusal::HeldStop);
        }
        let class = match self.status {
            HeldRootStopStatus::Signal(signal) => Stop::Signal(signal as i32),
            HeldRootStopStatus::NewChild(link) => Stop::Event(match link.op {
                ChildOp::Fork => libc::PTRACE_EVENT_FORK,
                ChildOp::Vfork => libc::PTRACE_EVENT_VFORK,
                ChildOp::Clone => libc::PTRACE_EVENT_CLONE,
            }),
            HeldRootStopStatus::Exec(_) => Stop::Event(libc::PTRACE_EVENT_EXEC),
            HeldRootStopStatus::VforkDone => Stop::Event(libc::PTRACE_EVENT_VFORK_DONE),
            HeldRootStopStatus::Exit => Stop::Event(libc::PTRACE_EVENT_EXIT),
            HeldRootStopStatus::Seccomp => Stop::Event(libc::PTRACE_EVENT_SECCOMP),
            HeldRootStopStatus::Syscall => Stop::Syscall,
            HeldRootStopStatus::Stop => Stop::Stop,
        };
        let sample = self
            .observation
            .sample(class == Stop::Signal(libc::SIGTRAP));
        Ok((class, sample))
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }

    fn same_root_generation(&self, other: &Self) -> bool {
        self.root_tid == other.root_tid && self.terminal.same_generation(&other.terminal)
    }

    pub(crate) fn arm_empty(
        slot: &Arc<StdMutex<Option<Self>>>,
        task: &Stopped,
        event: &Event,
    ) -> Result<(), TraceError> {
        let mut held = slot.lock().unwrap();
        if held.is_some() {
            return Err(Errno::EINVAL.into());
        }
        *held = Some(Self::from_event(task, event));
        Ok(())
    }

    pub(crate) fn ensure_current(
        slot: &Arc<StdMutex<Option<Self>>>,
        task: &Stopped,
        event: &Event,
    ) -> Result<(), TraceError> {
        let replacement = Self::from_event(task, event);
        let mut held = slot.lock().unwrap();
        match held.as_ref() {
            None => {
                *held = Some(replacement);
                Ok(())
            }
            Some(current)
                if current.armed
                    && current.same_root_generation(&replacement)
                    && current.status == replacement.status =>
            {
                Ok(())
            }
            Some(_) => Err(Errno::EINVAL.into()),
        }
    }

    pub(crate) fn supersede_with_exit(
        slot: &Arc<StdMutex<Option<Self>>>,
        task: &Stopped,
    ) -> Result<(), TraceError> {
        let replacement = Self::from_event(task, &Event::Exit);
        let mut held = slot.lock().unwrap();
        match held.as_ref() {
            None => {
                *held = Some(replacement);
                Ok(())
            }
            Some(current) if current.armed && current.same_root_generation(&replacement) => {
                *held = Some(replacement);
                Ok(())
            }
            Some(_) => Err(Errno::EINVAL.into()),
        }
    }
}

/// Exclusive transition capability for a stopped LiteInst root generation.
///
/// Dropping this value without a transition intentionally leaves the shared
/// cleanup lease armed. Every transition consumes the value and disarms only
/// after validating the exact carried Event generation.
pub(crate) struct RootStopLease {
    task: Option<Stopped>,
    held_root_stop: Option<Arc<StdMutex<Option<HeldRootStop>>>>,
}

impl RootStopLease {
    pub(crate) fn new(
        task: Stopped,
        held_root_stop: Option<Arc<StdMutex<Option<HeldRootStop>>>>,
    ) -> Self {
        Self {
            task: Some(task),
            held_root_stop,
        }
    }

    fn take_for_transition(&mut self) -> Result<Stopped, TraceError> {
        let task = self.task.take().expect("root stop lease consumed once");
        if let Some(slot) = self.held_root_stop.as_ref() {
            let mut held = slot.lock().unwrap().take().ok_or(Errno::EINVAL)?;
            let current = task.terminal_cleanup();
            if held.root_tid != task.pid()
                || !held.armed
                || !held.terminal.same_generation(&current)
            {
                *slot.lock().unwrap() = Some(held);
                return Err(Errno::EINVAL.into());
            }
            held.disarm();
        }
        Ok(task)
    }

    pub(crate) fn resume<T: Into<Option<Signal>>>(
        mut self,
        signal: T,
    ) -> Result<Running, TraceError> {
        self.take_for_transition()?.resume(signal)
    }

    pub(crate) fn step<T: Into<Option<Signal>>>(
        mut self,
        signal: T,
    ) -> Result<Running, TraceError> {
        let task = self.take_for_transition()?;
        #[cfg(test)]
        record_fatal_phase_for_test(|| {
            format!(
                "RootStopLease step before: tid={}, siginfo={:?}, terminal={:?}",
                task.pid(),
                task.getsiginfo().map(|info| (info.si_signo, info.si_code)),
                task.terminal_cleanup().observed_exit_status()
            )
        });
        let result = task.step(signal);
        #[cfg(test)]
        record_fatal_phase_for_test(|| format!("RootStopLease step result: {result:?}"));
        result
    }

    pub(crate) fn syscall<T: Into<Option<Signal>>>(
        mut self,
        signal: T,
    ) -> Result<Running, TraceError> {
        self.take_for_transition()?.syscall(signal)
    }

    pub(crate) fn detach<T: Into<Option<Signal>>>(
        mut self,
        signal: T,
    ) -> Result<Running, TraceError> {
        self.take_for_transition()?.detach(signal)
    }
}

impl std::ops::Deref for RootStopLease {
    type Target = Stopped;

    fn deref(&self) -> &Self::Target {
        self.task.as_ref().expect("root stop lease consumed once")
    }
}

impl std::ops::DerefMut for RootStopLease {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.task.as_mut().expect("root stop lease consumed once")
    }
}

impl NewbornTracee {
    pub(crate) fn from_event(parent_tid: Pid, op: ChildOp, task: &Running) -> Self {
        Self {
            link: EventChildLink {
                tid: task.pid(),
                parent_tid,
                op,
            },
            identity: None,
            terminal: task.terminal_cleanup(),
        }
    }

    pub(crate) fn set_identity(&mut self, identity: TraceeIdentity) {
        self.identity = Some(identity);
    }

    pub(crate) fn registration_error(&self) -> Option<Errno> {
        self.terminal.registration_error()
    }

    pub(crate) fn terminate_vfork_child(&self) -> Result<(), TraceError> {
        let identity = self.identity.as_ref().ok_or(Errno::ESRCH)?;
        match identity.send_signal(Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => return Err(error.into()),
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if self.terminal.wait(Duration::ZERO) {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(reservation) = self.terminal.reserve_pending_for_cleanup(remaining) else {
                if self.terminal.wait(Duration::ZERO) {
                    return Ok(());
                }
                return Err(Errno::ETIMEDOUT.into());
            };
            let state = reservation.decode()?;
            let Wait::Stopped(stopped, _) = state else {
                reservation.commit();
                continue;
            };
            reservation.commit();
            match stopped.resume(None) {
                Ok(_) | Err(TraceError::Died(_)) | Err(TraceError::Errno(Errno::ESRCH)) => {
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl TraceeIdentity {
    pub(crate) fn open_root(pid: Pid) -> Result<Self, Errno> {
        // `Command::spawn` is deliberately unblocked just before the child
        // calls PTRACE_TRACEME. Bind the same procfs generation repeatedly
        // until TracerPid becomes visible; resource/read errors remain fatal.
        for _ in 0..2_000 {
            match Self::capture(pid, None, false) {
                Ok(identity)
                    if identity.tid == identity.snapshot.tgid && identity.pidfd.is_some() =>
                {
                    return Ok(identity);
                }
                Ok(_) | Err(Errno::ESRCH | Errno::ENOENT)
                    if std::path::Path::new(&format!("/proc/{pid}")).exists() =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(_) => return Err(Errno::ESRCH),
                Err(error) => return Err(error),
            }
        }
        Err(Errno::ETIMEDOUT)
    }

    pub(crate) fn capture_event_child(
        tid: Pid,
        parent_tid: Pid,
        op: ChildOp,
    ) -> Result<Self, Errno> {
        // PTRACE_GETEVENTMSG is the authoritative parent-child ownership edge.
        // CLONE_PARENT intentionally makes PPid disagree with the event parent.
        let link = EventChildLink {
            tid,
            parent_tid,
            op,
        };
        Self::capture(link.tid, Some((link.parent_tid, Some(link.op))), false)
    }

    fn open_task_tid(tid: Pid, root_tgid: Pid) -> std::io::Result<Option<Self>> {
        let identity = match Self::capture(tid, None, false) {
            Ok(identity) => identity,
            Err(error) if skippable_tracee_open_error(tid, error) => return Ok(None),
            Err(error) => return Err(std::io::Error::from_raw_os_error(error.into_raw())),
        };
        if tid == root_tgid || identity.snapshot.tgid != root_tgid || !identity.is_our_tracee() {
            return Ok(None);
        }
        Ok(Some(identity))
    }

    fn open_discovered(tid: Pid, parent_tid: Pid) -> std::io::Result<Option<Self>> {
        let identity = match Self::capture(tid, Some((parent_tid, None)), true) {
            Ok(identity) => identity,
            Err(error) if skippable_tracee_open_error(tid, error) => return Ok(None),
            Err(error) => return Err(std::io::Error::from_raw_os_error(error.into_raw())),
        };

        // Re-read the kernel children relationship after opening the procfs
        // identity and pidfd. A list/open race may otherwise bind a replacement
        // tracee that reused the numeric child PID.
        if !direct_children(parent_tid)?.contains(&tid) {
            if !identity.is_our_tracee() || tracee_absent_or_replaced(parent_tid) {
                return Ok(None);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("tracee {tid} no longer belongs to listed parent {parent_tid}"),
            ));
        }
        Ok(Some(identity))
    }

    fn capture(
        tid: Pid,
        parent: Option<(Pid, Option<ChildOp>)>,
        validate_proc_parent: bool,
    ) -> Result<Self, Errno> {
        let before = tracee_snapshot(tid).map_err(io_errno)?;
        let parent_snapshot = parent
            .map(|(parent_tid, _)| tracee_snapshot(parent_tid).map_err(io_errno))
            .transpose()?;
        let proc_dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(format!("/proc/{tid}"))
            .map_err(io_errno)?;
        let proc_inode = proc_dir.metadata().map_err(io_errno)?.ino();
        let pidfd = if tid == before.tgid {
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, tid.as_raw(), 0) };
            if fd == -1 {
                return Err(Errno::last());
            }
            Some(unsafe { OwnedFd::from_raw_fd(fd as i32) })
        } else {
            None
        };
        let after = tracee_snapshot(tid).map_err(io_errno)?;
        let current_inode = fs::metadata(format!("/proc/{tid}"))
            .map_err(io_errno)?
            .ino();
        if before != after || current_inode != proc_inode || !tracer_is_current(after.tracer_pid) {
            return Err(Errno::ESRCH);
        }

        let parent = match (parent, parent_snapshot) {
            (Some((parent_tid, op)), Some(parent_snapshot)) => {
                let parent_after = tracee_snapshot(parent_tid).map_err(io_errno)?;
                if parent_after != parent_snapshot
                    || (validate_proc_parent
                        && after.tgid != parent_snapshot.tgid
                        && after.ppid != parent_snapshot.tgid)
                {
                    return Err(Errno::ESRCH);
                }
                Some((parent_tid, parent_snapshot.tgid, op))
            }
            (None, None) => None,
            _ => unreachable!("parent snapshot and relation must be paired"),
        };

        Ok(Self {
            tid,
            snapshot: after,
            proc_dir: proc_dir.into(),
            proc_inode,
            pidfd,
            parent,
        })
    }

    pub(crate) fn signal_owned_process_group(&self) -> Result<(), Errno> {
        if self.tid != self.snapshot.tgid || self.pidfd.is_none() {
            return Err(Errno::EINVAL);
        }
        self.send_signal(Signal::SIGKILL)
    }

    pub(crate) fn signal_owned_group(&self) -> Result<(), Errno> {
        if self.pidfd.is_some() {
            self.send_signal(Signal::SIGKILL)
        } else {
            Ok(())
        }
    }

    pub(crate) fn send_signal(&self, signal: Signal) -> Result<(), Errno> {
        self.send_raw_signal(signal as i32)
    }

    fn same_process(&self) -> bool {
        let path = format!("/proc/{}", self.tid);
        let Some(current) = tracee_snapshot(self.tid).ok() else {
            return false;
        };
        fd_inode(&self.proc_dir).ok() == Some(self.proc_inode)
            && fs::metadata(path).ok().map(|metadata| metadata.ino()) == Some(self.proc_inode)
            && current.tgid == self.snapshot.tgid
            && current.start_time == self.snapshot.start_time
    }

    fn observe_same_process(&self) -> std::io::Result<bool> {
        let observe = || -> std::io::Result<bool> {
            let current = tracee_snapshot(self.tid)?;
            Ok(fd_inode(&self.proc_dir)? == self.proc_inode
                && fs::metadata(format!("/proc/{}", self.tid))?.ino() == self.proc_inode
                && current.tgid == self.snapshot.tgid
                && current.start_time == self.snapshot.start_time)
        };
        match observe() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            result => result,
        }
    }

    fn is_our_tracee(&self) -> bool {
        self.same_process()
            && tracee_snapshot(self.tid).ok().is_some_and(|current| {
                current.tracer_pid == self.snapshot.tracer_pid
                    && tracer_is_current(current.tracer_pid)
            })
            && self.parent.is_none_or(|(_, parent_tgid, event_op)| {
                event_op.is_some()
                    || self.snapshot.tgid == parent_tgid
                    || self.snapshot.ppid == parent_tgid
            })
    }

    fn send_raw_signal(&self, signal: i32) -> Result<(), Errno> {
        let Some(pidfd) = self.pidfd.as_ref() else {
            return Err(Errno::EOPNOTSUPP);
        };
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result == -1 {
            Err(Errno::last())
        } else {
            Ok(())
        }
    }
}

struct RegisteredTraceeCleanup {
    identity: TraceeIdentity,
    terminal: TerminalCleanup,
    event_link: Option<EventChildLink>,
}

fn terminal_descendant_remains_owned(identity: &TraceeIdentity) -> bool {
    if identity.is_our_tracee() {
        return true;
    }
    let Some((_, parent_tgid, _)) = identity.parent else {
        return false;
    };
    identity.same_process()
        && tracee_snapshot(identity.tid)
            .ok()
            .is_some_and(|current| current.ppid == parent_tgid)
}

impl LiteinstTraceeCleanup {
    fn new(
        pid: Pid,
        newborn_tracees: Arc<StdMutex<HashMap<Pid, NewbornTracee>>>,
        held_root_stop: Arc<StdMutex<Option<HeldRootStop>>>,
    ) -> Result<Self, Errno> {
        Ok(Self {
            identity: TraceeIdentity::open_root(pid)?,
            newborn_tracees,
            armed: true,
            terminal: None,
            notifier_owner: None,
            retained_descendants: HashMap::new(),
            retained_terminal_descendants: HashMap::new(),
            fatal_terminal_observations: None,
            held_root_stop,
            root_frozen: false,
            #[cfg(test)]
            fail_discovery_once: None,
            #[cfg(test)]
            fail_discovery_while: None,
            #[cfg(test)]
            fail_after_scan_once: None,
            #[cfg(test)]
            force_task_scan_once: None,
        })
    }

    fn pid(&self) -> Pid {
        self.identity.tid
    }

    fn register_notifier(&mut self, task: &Running) {
        debug_assert!(self.terminal.is_none());
        self.notifier_owner = Some(std::thread::current().id());
        self.terminal = Some(task.terminal_cleanup());
    }

    fn capture_pending_children(&self, terminal: &TerminalCleanup) -> std::io::Result<()> {
        while let Some(reservation) = terminal.reserve_pending_for_cleanup(Duration::ZERO) {
            let state = reservation.decode().map_err(|error| {
                std::io::Error::other(format!("decode queued cancellation state: {error}"))
            })?;
            if let Wait::Stopped(stopped, Event::NewChild(op, child)) = state {
                let child_pid = child.pid();
                let mut newborns = self.newborn_tracees.lock().unwrap();
                newborns
                    .entry(child_pid)
                    .or_insert_with(|| NewbornTracee::from_event(stopped.pid(), op, &child));
            }
            // The raw FIFO front remains present until any child cleanup
            // ownership above is durably stored.
            reservation.commit();
        }
        Ok(())
    }

    fn freeze_root_generation(&mut self, deadline: Option<Instant>) -> std::io::Result<()> {
        let terminal = self
            .terminal
            .as_ref()
            .expect("registered LiteInst cleanup has a root terminal handle");
        terminal
            .ensure_registered()
            .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
        if self.root_frozen {
            return self.capture_pending_children(terminal);
        }
        if let Some(mut held) = self.held_root_stop.lock().unwrap().take() {
            let owns_claimed_exit = matches!(held.status, HeldRootStopStatus::Exit);
            let matching_status = match held.status {
                HeldRootStopStatus::Signal(signal) => {
                    let _exact_signal = signal;
                    true
                }
                HeldRootStopStatus::NewChild(link) => self
                    .newborn_tracees
                    .lock()
                    .unwrap()
                    .get(&link.tid)
                    .is_some_and(|newborn| {
                        newborn.link.parent_tid == link.parent_tid && newborn.link.op == link.op
                    }),
                HeldRootStopStatus::Exec(replaced_tid) => {
                    let _exact_replaced_tid = replaced_tid;
                    true
                }
                HeldRootStopStatus::VforkDone
                | HeldRootStopStatus::Exit
                | HeldRootStopStatus::Seccomp
                | HeldRootStopStatus::Stop
                | HeldRootStopStatus::Syscall => true,
            };
            if !held.armed
                || held.root_tid != self.pid()
                || !terminal.same_generation(&held.terminal)
                || !matching_status
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "held root stop lease did not match the exact event generation/status",
                ));
            }
            if owns_claimed_exit {
                // SAFETY: taking the exact-generation held lease is the
                // cancellation handoff for the ExitFuture-minted Stopped. The
                // handler future has been dropped, so no independent Stopped
                // capability survives this exclusive slot transfer.
                unsafe { terminal.revoke_owned_exit_stop() }
            } else {
                terminal.revoke_unclaimed_exit_stop()
            }
            .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
            held.disarm();
            self.root_frozen = true;
            return self.capture_pending_children(terminal);
        }

        match self.identity.send_signal(Signal::SIGSTOP) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => return Err(std::io::Error::from_raw_os_error(error.into_raw())),
        }

        let deadline = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(2));
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Some(reservation) = terminal.reserve_pending_for_cleanup(remaining) {
                let state = reservation.decode().map_err(|error| {
                    std::io::Error::other(format!(
                        "decode exact root freeze state for {}: {error}",
                        self.pid()
                    ))
                })?;
                if let Wait::Stopped(stopped, Event::NewChild(op, child)) = state {
                    let child_pid = child.pid();
                    self.newborn_tracees
                        .lock()
                        .unwrap()
                        .entry(child_pid)
                        .or_insert_with(|| NewbornTracee::from_event(stopped.pid(), op, &child));
                }
                terminal
                    .revoke_unclaimed_exit_stop()
                    .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
                reservation.commit();
                // Any exact-generation nonterminal wait status means the root
                // is kernel-stopped. Drain the remaining FIFO while it cannot
                // execute and create another child.
                self.capture_pending_children(terminal)?;
                self.root_frozen = true;
                return Ok(());
            }
            if terminal.exit_stop_observed() {
                terminal
                    .revoke_unclaimed_exit_stop()
                    .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
                self.root_frozen = true;
                return Ok(());
            }
            if terminal.wait(Duration::ZERO) && terminal.pending_is_empty() {
                self.root_frozen = true;
                return Ok(());
            }
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("root {} did not enter an exact notifier stop", self.pid()),
                ));
            }
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn confirm_reaped(&mut self) -> std::io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        let notifier_finished = self
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.wait(Duration::ZERO) && terminal.pending_is_empty());
        let identity_absent = !self.identity.same_process();
        let unregistered_absent = self.terminal.is_none() && identity_absent;
        let newborns_empty = self.newborn_tracees.lock().unwrap().is_empty();
        let retained_empty = self.retained_descendants.is_empty()
            && self.retained_terminal_descendants.is_empty()
            && self
                .fatal_terminal_observations
                .as_ref()
                .is_none_or(HashMap::is_empty);
        if newborns_empty
            && retained_empty
            && ((notifier_finished && identity_absent) || unregistered_absent)
        {
            self.armed = false;
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "LiteInst root still exists after typed task cleanup",
            ))
        }
    }

    fn terminate_and_confirm(&mut self) -> std::io::Result<()> {
        self.terminate_and_confirm_before(None)
    }

    fn terminate_fatal_and_confirm(
        &mut self,
        deadline: Instant,
        mut record_refusal: impl FnMut(std::io::Error),
    ) -> bool {
        self.fatal_terminal_observations
            .get_or_insert_with(HashMap::new);
        // The original failure deadline covers freezing, physical cleanup,
        // retries, and external terminal observation. Every refused attempt
        // restores its ownership records before another attempt can begin.
        loop {
            match self.terminate_and_confirm_before(Some(deadline)) {
                Ok(()) => return true,
                Err(error) => record_refusal(error),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            std::thread::sleep(remaining.min(Duration::from_millis(1)));
        }
    }

    fn terminate_and_confirm_before(&mut self, deadline: Option<Instant>) -> std::io::Result<()> {
        if self.confirm_reaped().is_ok() {
            return Ok(());
        }

        let mut descendants = std::mem::take(&mut self.retained_descendants);
        let mut terminal_descendants = std::mem::take(&mut self.retained_terminal_descendants);
        let result = self.terminate_and_confirm_attempt(
            &mut descendants,
            &mut terminal_descendants,
            deadline,
        );
        if result.is_err() {
            self.retained_descendants.extend(descendants);
            self.retained_terminal_descendants
                .extend(terminal_descendants);
        }
        result
    }

    fn terminate_and_confirm_attempt(
        &mut self,
        descendants: &mut HashMap<Pid, RegisteredTraceeCleanup>,
        terminal_descendants: &mut HashMap<Pid, TraceeIdentity>,
        deadline: Option<Instant>,
    ) -> std::io::Result<()> {
        if self.terminal.is_none() {
            terminate_and_reap_new_child_with_identity(Running::new(self.pid()), &self.identity)
                .map_err(|error| {
                    std::io::Error::other(format!("pre-registration LiteInst cleanup: {error}"))
                })?;
            self.armed = false;
            return Ok(());
        }

        if self.identity.same_process() {
            self.freeze_root_generation(deadline)?;
        } else {
            // The exact root generation is already gone, so it cannot create
            // another descendant. Drain any child event the notifier published
            // before terminal acknowledgment and continue with the retained
            // generation-bound descendants; trying to freeze a completed root
            // would only collide with its consumed exit capability.
            if let Some(terminal) = self.terminal.as_ref() {
                self.capture_pending_children(terminal)?;
            }
            self.root_frozen = true;
        }
        #[cfg(test)]
        if self
            .force_task_scan_once
            .as_ref()
            .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
        {
            self.newborn_tracees.lock().unwrap().clear();
        }
        self.discover_descendants(descendants, terminal_descendants)?;
        let root_terminal = self.terminal.as_ref().unwrap();
        self.capture_pending_children(root_terminal)?;
        if !root_terminal.pending_is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "root notifier FIFO changed while frozen",
            ));
        }
        match self.identity.send_signal(Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => return Err(std::io::Error::from_raw_os_error(error.into_raw())),
        }
        for tracee in descendants.values() {
            tracee
                .terminal
                .ensure_registered()
                .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
            send_identity_sigkill(&tracee.identity)?;
        }

        let deadline = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(2));
        while Instant::now() < deadline {
            if let Some(terminal) = self.terminal.as_ref() {
                self.capture_pending_children(terminal)?;
            }
            for tracee in descendants.values() {
                self.capture_pending_children(&tracee.terminal)?;
            }
            self.discover_descendants(descendants, terminal_descendants)?;
            for tracee in descendants.values() {
                tracee
                    .terminal
                    .ensure_registered()
                    .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
                send_identity_sigkill(&tracee.identity)?;
            }

            let root_done = self
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.wait(Duration::ZERO));
            let completed = descendants
                .iter()
                .filter_map(|(pid, tracee)| tracee.terminal.wait(Duration::ZERO).then_some(*pid))
                .collect::<Vec<_>>();
            for pid in completed {
                if let Some(tracee) = descendants.get(&pid) {
                    self.capture_pending_children(&tracee.terminal)?;
                }
                let tracee = descendants
                    .remove(&pid)
                    .expect("completed descendant must remain registered");
                terminal_descendants.insert(pid, tracee.identity);
            }
            // Once the exact notifier generation is terminal, retain its proc
            // identity as ownership only while it remains our tracee or its
            // recorded parent still owns the zombie. Reparenting cannot grant
            // natural-parent wait authority. Fatal injected cleanup separately
            // retains read-only observation for its stronger confirmation.
            let released = terminal_descendants
                .iter()
                .filter_map(|(pid, identity)| {
                    (!terminal_descendant_remains_owned(identity)).then_some(*pid)
                })
                .collect::<Vec<_>>();
            for pid in released {
                let identity = terminal_descendants
                    .remove(&pid)
                    .expect("retained terminal generation");
                if let Some(observations) = self.fatal_terminal_observations.as_mut() {
                    observations.insert(pid, identity);
                }
            }
            if let Some(observations) = self.fatal_terminal_observations.as_mut() {
                // This map never authorizes a signal or wait: Linux may leave a
                // terminal zombie for a different natural parent after our
                // ptracer wait. Only observe that same retained generation.
                let mut absent = Vec::new();
                for (pid, identity) in observations.iter() {
                    if !identity.observe_same_process()? {
                        absent.push(*pid);
                    }
                }
                for pid in absent {
                    observations.remove(&pid);
                }
            }
            let root_absent = !self.identity.same_process();
            let newborns_empty = self.newborn_tracees.lock().unwrap().is_empty();
            if root_done
                && root_absent
                && descendants.is_empty()
                && terminal_descendants.is_empty()
                && self
                    .fatal_terminal_observations
                    .as_ref()
                    .is_none_or(HashMap::is_empty)
                && newborns_empty
            {
                self.armed = false;
                return Ok(());
            }

            if self.notifier_owner == Some(std::thread::current().id()) {
                // The pidfd-bound SIGKILL is already pending. Numeric ptrace
                // operations only advance an extant ptrace relationship and
                // never inject a signal into a potentially reused PID.
                // Preserve parentage until every descendant is terminal and
                // reaped. Otherwise an auto-attached child can be reparented
                // before its notifier consumes the final wait status.
                if !root_done && descendants.is_empty() && self.identity.is_our_tracee() {
                    root_terminal
                        .revoke_unclaimed_exit_stop()
                        .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
                    let _ = ptrace::cont(self.pid().into(), None);
                }
                for tracee in descendants.values() {
                    if tracee.identity.is_our_tracee() {
                        tracee
                            .terminal
                            .revoke_unclaimed_exit_stop()
                            .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
                        let _ = ptrace::cont(tracee.identity.tid.into(), None);
                    }
                }
            }
            if let Some(terminal) = self.terminal.as_ref() {
                terminal.wait(Duration::from_millis(1));
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            if self.fatal_terminal_observations.is_some() {
                format!(
                    "LiteInst tracee {} cleanup or fatal terminal-disappearance confirmation timed out",
                    self.pid()
                )
            } else {
                format!(
                    "notifier did not acknowledge terminal cleanup for LiteInst tracee {}",
                    self.pid()
                )
            },
        ))
    }

    fn discover_descendants(
        &self,
        descendants: &mut HashMap<Pid, RegisteredTraceeCleanup>,
        terminal_descendants: &HashMap<Pid, TraceeIdentity>,
    ) -> std::io::Result<()> {
        let mut queue = VecDeque::from([self.pid()]);
        queue.extend(descendants.keys().copied());
        let newborn_tids = self
            .newborn_tracees
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let mut transferred = Vec::new();
        let mut absorbed = Vec::new();
        for tid in newborn_tids {
            if tid == self.pid() || terminal_descendants.contains_key(&tid) {
                self.newborn_tracees.lock().unwrap().remove(&tid);
                continue;
            }
            if let Some(existing) = descendants.get_mut(&tid) {
                let newborn = self
                    .newborn_tracees
                    .lock()
                    .unwrap()
                    .remove(&tid)
                    .expect("listed newborn must remain registered");
                existing.event_link.get_or_insert(newborn.link);
                absorbed.push((tid, newborn));
                continue;
            }

            let mut newborn = self
                .newborn_tracees
                .lock()
                .unwrap()
                .remove(&tid)
                .expect("listed newborn must remain registered");
            let identity = match newborn.identity.take() {
                Some(identity) => identity,
                None => match TraceeIdentity::capture_event_child(
                    newborn.link.tid,
                    newborn.link.parent_tid,
                    newborn.link.op,
                ) {
                    Ok(identity) => identity,
                    Err(error) => {
                        self.newborn_tracees.lock().unwrap().insert(tid, newborn);
                        self.restore_transferred_newborns(
                            descendants,
                            &mut transferred,
                            &mut absorbed,
                        );
                        return Err(std::io::Error::from_raw_os_error(error.into_raw()));
                    }
                },
            };
            if identity.snapshot.tgid == tid {
                queue.push_back(tid);
            }
            descendants.insert(
                tid,
                RegisteredTraceeCleanup {
                    identity,
                    terminal: newborn.terminal,
                    event_link: Some(newborn.link),
                },
            );
            transferred.push(tid);
        }

        #[cfg(test)]
        if self
            .fail_discovery_while
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            self.restore_transferred_newborns(descendants, &mut transferred, &mut absorbed);
            return Err(std::io::Error::from_raw_os_error(libc::EIO));
        }
        #[cfg(test)]
        if self
            .fail_discovery_once
            .as_ref()
            .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
        {
            self.restore_transferred_newborns(descendants, &mut transferred, &mut absorbed);
            return Err(std::io::Error::from_raw_os_error(libc::EIO));
        }

        let task_tids = match task_tids(self.pid()) {
            Ok(tids) => tids,
            Err(error) => {
                self.restore_transferred_newborns(descendants, &mut transferred, &mut absorbed);
                return Err(error);
            }
        };
        for tid in task_tids {
            if tid == self.pid()
                || descendants.contains_key(&tid)
                || terminal_descendants.contains_key(&tid)
            {
                continue;
            }
            let identity = match TraceeIdentity::open_task_tid(tid, self.identity.snapshot.tgid) {
                Ok(Some(identity)) => identity,
                Ok(None) => continue,
                Err(error) => {
                    self.restore_transferred_newborns(descendants, &mut transferred, &mut absorbed);
                    return Err(error);
                }
            };
            let terminal = Stopped::try_new_current_unchecked(tid)
                .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?
                .terminal_cleanup();
            descendants.insert(
                tid,
                RegisteredTraceeCleanup {
                    identity,
                    terminal,
                    event_link: None,
                },
            );
        }

        let mut visited = HashSet::new();
        while let Some(parent) = queue.pop_front() {
            if !visited.insert(parent) {
                continue;
            }
            let children = match direct_children(parent) {
                Ok(children) => children,
                Err(error) => {
                    let error = std::io::Error::new(
                        error.kind(),
                        format!("read direct children of bound tracee {parent}: {error}"),
                    );
                    self.restore_transferred_newborns(descendants, &mut transferred, &mut absorbed);
                    return Err(error);
                }
            };
            for child in children {
                if child == self.pid()
                    || descendants.contains_key(&child)
                    || terminal_descendants.contains_key(&child)
                {
                    continue;
                }
                let identity = match TraceeIdentity::open_discovered(child, parent) {
                    Ok(Some(identity)) => identity,
                    Ok(None) => continue,
                    Err(error) => {
                        let error = std::io::Error::new(
                            error.kind(),
                            format!("bind listed tracee {child} under parent {parent}: {error}"),
                        );
                        self.restore_transferred_newborns(
                            descendants,
                            &mut transferred,
                            &mut absorbed,
                        );
                        return Err(error);
                    }
                };
                queue.push_back(child);
                let terminal = Running::new(child).terminal_cleanup();
                descendants.insert(
                    child,
                    RegisteredTraceeCleanup {
                        identity,
                        terminal,
                        event_link: None,
                    },
                );
            }
        }
        #[cfg(test)]
        if self
            .fail_after_scan_once
            .as_ref()
            .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
        {
            self.restore_transferred_newborns(descendants, &mut transferred, &mut absorbed);
            return Err(std::io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(())
    }

    fn restore_transferred_newborns(
        &self,
        descendants: &mut HashMap<Pid, RegisteredTraceeCleanup>,
        transferred: &mut Vec<Pid>,
        absorbed: &mut Vec<(Pid, NewbornTracee)>,
    ) {
        let mut newborns = self.newborn_tracees.lock().unwrap();
        newborns.extend(absorbed.drain(..));
        for tid in transferred.drain(..) {
            let registered = descendants
                .remove(&tid)
                .expect("transferred newborn must remain in local cleanup map");
            newborns.insert(
                tid,
                NewbornTracee {
                    link: registered
                        .event_link
                        .expect("transferred newborn retains kernel event link"),
                    identity: Some(registered.identity),
                    terminal: registered.terminal,
                },
            );
        }
    }
}

fn send_identity_sigkill(identity: &TraceeIdentity) -> std::io::Result<()> {
    if identity.pidfd.is_none() {
        // Nonleader TIDs are terminated by their TGID leader's pidfd. They are
        // never signaled numerically; their bound ptrace statuses are drained
        // separately on the owning tracer thread.
        return Ok(());
    }
    match identity.send_signal(Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(std::io::Error::from_raw_os_error(error.into_raw())),
    }
}

impl Drop for LiteinstTraceeCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Cancellation cannot await an orderly drain, so synchronously request
        // termination and wait for the notifier-owned final reap. Before async
        // registration, the bounded raw-wait fallback owns cleanup instead.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.terminate_and_confirm() {
                Ok(()) => return,
                Err(error) if Instant::now() < deadline => {
                    // Every failed attempt restores all descendant/newborn
                    // ownership to this guard. Retry transient discovery and
                    // registration errors without dropping cleanup records.
                    std::thread::sleep(Duration::from_millis(1));
                    tracing::debug!(pid = %self.pid(), %error, "retrying LiteInst cancellation cleanup");
                }
                Err(error) => {
                    tracing::error!(pid = %self.pid(), %error, "LiteInst cancellation cleanup failed");
                    return;
                }
            }
        }
    }
}

fn io_errno(error: std::io::Error) -> Errno {
    Errno::new(error.raw_os_error().unwrap_or(libc::EIO))
}

fn process_start_time(tid: Pid) -> std::io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{tid}/stat"))?;
    let fields = stat
        .rsplit_once(") ")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed stat"))?
        .1;
    fields
        .split_ascii_whitespace()
        .nth(19)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing starttime"))?
        .parse()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn status_pid(status: &str, name: &str) -> std::io::Result<Pid> {
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix(name))
        .and_then(|value| value.trim().parse::<i32>().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("missing or malformed {name}"),
            )
        })?;
    Ok(Pid::from_raw(value))
}

fn tracee_snapshot(tid: Pid) -> std::io::Result<TraceeSnapshot> {
    let start_time = process_start_time(tid)?;
    let status = fs::read_to_string(format!("/proc/{tid}/status"))?;
    let snapshot = TraceeSnapshot {
        tgid: status_pid(&status, "Tgid:")?,
        ppid: status_pid(&status, "PPid:")?,
        tracer_pid: status_pid(&status, "TracerPid:")?,
        start_time,
    };
    if process_start_time(tid)? != start_time {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "tracee identity changed while reading procfs",
        ));
    }
    Ok(snapshot)
}

fn tracer_is_current(tracer_tid: Pid) -> bool {
    tracer_tid.as_raw() > 0
        && std::path::Path::new(&format!("/proc/self/task/{tracer_tid}")).exists()
}

fn fd_inode(fd: &OwnedFd) -> std::io::Result<u64> {
    fs::metadata(format!("/proc/self/fd/{}", fd.as_raw_fd())).map(|metadata| metadata.ino())
}

fn tracee_absent_or_replaced(tid: Pid) -> bool {
    let path = format!("/proc/{tid}");
    if !std::path::Path::new(&path).exists() {
        return true;
    }
    tracee_snapshot(tid)
        .ok()
        .is_some_and(|snapshot| !tracer_is_current(snapshot.tracer_pid))
}

fn skippable_tracee_open_error(tid: Pid, error: Errno) -> bool {
    matches!(error, Errno::ENOENT | Errno::ESRCH) && tracee_absent_or_replaced(tid)
}

static PROC_CHILDREN_SUPPORTED: LazyLock<bool> = LazyLock::new(|| {
    fs::read_dir("/proc/self/task")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|task| task.path().join("children").exists())
});

fn task_tids(root: Pid) -> std::io::Result<Vec<Pid>> {
    let process_path = format!("/proc/{root}");
    let tasks = match fs::read_dir(format!("{process_path}/task")) {
        Ok(tasks) => tasks,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && !std::path::Path::new(&process_path).exists() =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };
    tasks
        .map(|task| {
            let task = task?;
            let tid = task.file_name().into_string().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "non-UTF8 task TID")
            })?;
            tid.parse::<i32>()
                .map(Pid::from_raw)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .collect()
}

fn direct_children(pid: Pid) -> std::io::Result<Vec<Pid>> {
    let process_path = format!("/proc/{pid}");
    let task_dir = match fs::read_dir(format!("{process_path}/task")) {
        Ok(task_dir) => task_dir,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && !std::path::Path::new(&process_path).exists() =>
        {
            return Ok(Vec::new());
        }
        Err(error) => {
            return Err(std::io::Error::new(
                error.kind(),
                format!("read {process_path}/task: {error}"),
            ));
        }
    };
    let mut children = Vec::new();
    for task in task_dir {
        let task = task?;
        let contents = match fs::read_to_string(task.path().join("children")) {
            Ok(contents) => contents,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && !*PROC_CHILDREN_SUPPORTED =>
            {
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !task.path().exists() => {
                continue;
            }
            Err(error) => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!("read {}: {error}", task.path().join("children").display()),
                ));
            }
        };
        for child in contents.split_ascii_whitespace() {
            children.push(Pid::from_raw(child.parse::<i32>().map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, error)
            })?));
        }
    }
    Ok(children)
}

fn terminate_and_reap_new_child_with_identity(
    task: Running,
    identity: &TraceeIdentity,
) -> Result<(), TraceError> {
    match identity.send_signal(Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => return Err(error.into()),
    }
    drain_unregistered_child(task)
}

fn drain_unregistered_child(task: Running) -> Result<(), TraceError> {
    let pid = task.pid();
    for _ in 0..2_000 {
        let mut status = 0;
        let waited =
            unsafe { libc::waitpid(pid.as_raw(), &mut status, libc::__WALL | libc::WNOHANG) };
        if waited == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        if waited == -1 {
            let errno = Errno::last();
            match errno {
                Errno::EINTR => continue,
                Errno::ECHILD
                    if unsafe { libc::kill(pid.as_raw(), 0) } == -1
                        && Errno::last() == Errno::ESRCH =>
                {
                    return Ok(());
                }
                Errno::ECHILD => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                _ => return Err(errno.into()),
            }
        }
        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
            return Ok(());
        }
        if libc::WIFSTOPPED(status) {
            let stopped = Stopped::new_unchecked(pid);
            match stopped.resume(None) {
                Ok(_) | Err(TraceError::Died(_)) | Err(TraceError::Errno(Errno::ESRCH)) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Err(Errno::ETIMEDOUT.into())
}

fn liteinst_pidfd_setup_error(
    pid: Pid,
    setup_error: Errno,
    kill_error: Option<Errno>,
    drain_result: Result<(), TraceError>,
) -> anyhow::Error {
    let setup_failure = if setup_error == Errno::ETIMEDOUT {
        format!(
            "LiteInst tracee {pid} root identity did not become a stable traced thread-group leader with a pidfd within the 2,000-attempt retry budget"
        )
    } else {
        format!("failed to open pidfd for LiteInst tracee {pid}: {setup_error}")
    };
    match (kill_error, drain_result) {
        (Some(kill_error), drain_result) => anyhow::anyhow!(
            "{setup_failure}; numeric setup-failure kill also failed: {kill_error}; drain result: {drain_result:?}"
        ),
        (None, Err(drain_error)) => {
            anyhow::anyhow!("{setup_failure}; cleanup drain also failed: {drain_error}")
        }
        (None, Ok(())) => anyhow::anyhow!(setup_failure),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("ordinary ptrace cleanup exceeded its two-second monotonic attempt budget")]
struct CleanupDeadlineExceeded;

thread_local! {
    static CLEANUP_QUARANTINE: std::cell::RefCell<HashMap<u64, std::mem::ManuallyDrop<Box<dyn std::any::Any>>>> = std::cell::RefCell::new(HashMap::new());
}
static NEXT_QUARANTINE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
// Serializes both admission and quarantine publication. An admitted spawn is
// the one which passed this mutex before any Tool init, pipe, or fork effects.
// Already-admitted trees remain owned. No later ptrace spawn may add resources
// until each quarantined tree has actually completed, even on another thread.
static UNCONFIRMED_QUARANTINES: StdMutex<usize> = StdMutex::new(0);

struct OrdinaryAdmission;
impl OrdinaryAdmission {
    fn acquire() -> Result<Self, Error> {
        let count = UNCONFIRMED_QUARANTINES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *count == 0 {
            Ok(Self)
        } else {
            Err(Error::Tool(anyhow::Error::new(CleanupAdmissionRefused {
                retained: *count,
            })))
        }
    }
}

struct QuarantinePermit {
    id: u64,
}
impl QuarantinePermit {
    fn new() -> Self {
        let mut count = UNCONFIRMED_QUARANTINES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *count += 1;
        Self {
            id: NEXT_QUARANTINE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }
    fn complete(self) {
        let mut count = UNCONFIRMED_QUARANTINES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *count -= 1;
    }
}
// Deliberately no Drop decrement: abandoning or losing the original ptracer
// thread is not successful cleanup and cannot reopen admission.

/// A new ptrace spawn was refused before Tool initialization, pipes, or fork.
///
/// Previously admitted trees remain owned. Recover and complete each retained
/// legacy cleanup on its original thread before admitting additional trees.
#[derive(Debug, thiserror::Error)]
#[error("ptrace spawn refused while {retained} cleanup owners remain unconfirmed")]
pub struct CleanupAdmissionRefused {
    retained: usize,
}
impl CleanupAdmissionRefused {
    /// Number of retained cleanup owners, including permanently unbound guards.
    pub fn retained(&self) -> usize {
        self.retained
    }
}

/// A resource needed by a failed traced tree until cleanup is confirmed.
///
/// `cleanup` runs on the original ptracer thread after physical task, consuming
/// callback, and requested output cleanup. On error it must retain its original
/// resource so a later bounded cleanup attempt can retry. It must not block
/// indefinitely; a synchronous cleanup operation cannot be preempted by a timer.
pub trait PtraceCleanupResource {
    /// Release the original resource, or retain it and return the actual error.
    fn cleanup(&mut self) -> Result<(), Error>;
}

/// Permanently retain an original resource whose cleanup cannot be confirmed.
///
/// This closes later ptrace spawn admission and offers no recovery operation.
/// It neither runs cleanup nor resumes guest work. Already-admitted trees keep
/// their existing owners. TLS teardown or a borrowed registry deliberately leaks
/// the same allocation and permit; dropping a diagnostic cannot release either.
pub fn quarantine_cleanup_resource<C: PtraceCleanupResource + 'static>(resource: C) {
    quarantine_boxed_resource(std::mem::ManuallyDrop::new(Box::new(resource)));
}

fn quarantine_boxed_resource(resource: std::mem::ManuallyDrop<Box<dyn PtraceCleanupResource>>) {
    let permit = QuarantinePermit::new();
    let id = permit.id;
    let mut owner = Some(std::mem::ManuallyDrop::new(
        Box::new(UnboundCleanupResource {
            _resource: std::mem::ManuallyDrop::into_inner(resource),
            _permit: permit,
        }) as Box<dyn std::any::Any>,
    ));
    let stored = CLEANUP_QUARANTINE
        .try_with(|owners| {
            let Ok(mut owners) = owners.try_borrow_mut() else {
                return false;
            };
            // Keep ownership even in the otherwise-impossible key collision.
            let std::collections::hash_map::Entry::Vacant(entry) = owners.entry(id) else {
                return false;
            };
            entry.insert(owner.take().unwrap());
            true
        })
        .unwrap_or(false);
    if !stored {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "ptrace cleanup unconfirmed: registry unavailable; original resource {id} and permanent admission permit retained"
        );
        // `owner` contains ManuallyDrop. TLS refusal does not run its destructor
        // or release the permanent permit, even during an existing unwind.
    }
}

#[derive(Debug)]
struct CleanupOwnerIdentity {
    pid: libc::pid_t,
    namespace: fs::File,
    namespace_device: u64,
    namespace_inode: u64,
    pidfd: OwnedFd,
}

impl CleanupOwnerIdentity {
    fn capture() -> std::io::Result<Self> {
        let namespace = fs::File::open("/proc/self/ns/pid")?;
        let metadata = namespace.metadata()?;
        let pid = unsafe { libc::getpid() };
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if raw == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            pid,
            namespace,
            namespace_device: metadata.dev(),
            namespace_inode: metadata.ino(),
            pidfd: unsafe { OwnedFd::from_raw_fd(raw as i32) },
        })
    }

    fn verify(&self) -> Result<(), CleanupLookupError> {
        if unsafe { libc::getpid() } != self.pid {
            return Err(CleanupLookupError::WrongProcess);
        }
        let metadata = fs::metadata("/proc/self/ns/pid")
            .map_err(|error| CleanupLookupError::Identity(Arc::new(error)))?;
        let retained = self
            .namespace
            .metadata()
            .map_err(|error| CleanupLookupError::Identity(Arc::new(error)))?;
        if (metadata.dev(), metadata.ino()) != (self.namespace_device, self.namespace_inode)
            || (retained.dev(), retained.ino()) != (self.namespace_device, self.namespace_inode)
        {
            return Err(CleanupLookupError::WrongProcess);
        }
        let mut poll = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        match unsafe { libc::poll(&mut poll, 1, 0) } {
            0 => Ok(()),
            -1 => Err(CleanupLookupError::Identity(Arc::new(
                std::io::Error::last_os_error(),
            ))),
            _ if poll.revents & libc::POLLIN != 0 => Err(CleanupLookupError::WrongProcess),
            _ => Err(CleanupLookupError::Identity(Arc::new(
                std::io::Error::other(format!(
                    "original process pidfd poll refused with events {}",
                    poll.revents
                )),
            ))),
        }
    }
}

struct UnboundCleanupResource {
    _resource: Box<dyn PtraceCleanupResource>,
    _permit: QuarantinePermit,
}

/// A legacy wait reached its bound while its complete cleanup owner was retained.
///
/// This is explicitly not confirmed cleanup. The owner stays in the original
/// ptracer thread's quarantine even if this diagnostic is dropped. Call
/// [`Self::take_cleanup`] on that thread to recover it. Final process teardown
/// still relies on the existing PTRACE_O_EXITKILL option. At ptracer-thread exit
/// the retained owner is deliberately not dropped: Tool/timer destructors cannot
/// run safely there. Its fds and allocations remain until process exit, and new
/// ptrace spawns in every instrumentation mode stay refused. Notifier workers
/// retain their own exact waits.
///
/// Downcast the legacy error to this type, then inspect [`Self::primary`]. The
/// pending route cannot move the original non-clone Tool marker into the legacy
/// error because that primary remains with its quarantined owner.
///
/// Static injected traps can reach this failed-cleanup recovery route through
/// legacy waits. Recovering an already-owned failed tree does not opt that
/// backend into the normal `wait_*_completion` or supervisor-start contract.
#[derive(Debug, thiserror::Error)]
#[error("ordinary ptrace cleanup unconfirmed; owner {id} retained on {thread:?}: {primary}")]
pub struct CleanupUnconfirmed {
    id: u64,
    thread: ThreadId,
    primary: Arc<Error>,
    origin: reverie::BackendFailure,
    owner_identity: Arc<Result<CleanupOwnerIdentity, Arc<std::io::Error>>>,
}

/// A quarantine lookup failed without removing or changing the retained owner.
#[derive(Debug, thiserror::Error)]
pub enum CleanupLookupError {
    /// A copied diagnostic is not in its original process generation/namespace.
    #[error("cleanup belongs to a different original process")]
    WrongProcess,
    /// The retained original-process identity could not be established.
    #[error("cleanup owner identity could not be confirmed: {0}")]
    Identity(#[source] Arc<std::io::Error>),
    /// The keyed entry does not carry the diagnostic's original failure/permit.
    #[error("cleanup owner generation does not match the diagnostic")]
    WrongGeneration,
    /// Ptrace operations require the thread which created the tracer.
    #[error("cleanup belongs to a different ptracer thread")]
    WrongThread,
    /// The requested global state or successful-result type does not match.
    #[error("cleanup result type does not match the retained owner")]
    WrongType,
    /// The owner was already recovered by an earlier lookup.
    #[error("cleanup owner was already recovered")]
    AlreadyTaken,
    /// The original thread's registry is borrowed or unavailable during teardown.
    #[error("cleanup owner storage is unavailable")]
    StorageUnavailable,
}

impl CleanupUnconfirmed {
    /// The stable key identifying this retained cleanup owner.
    pub fn recovery_key(&self) -> u64 {
        self.id
    }

    /// The origin atomically captured with the primary failure.
    pub fn origin(&self) -> reverie::BackendFailure {
        self.origin
    }

    /// The original failure retained by the quarantined run.
    pub fn primary(&self) -> &Error {
        &self.primary
    }

    fn verify_owner(&self) -> Result<(), CleanupLookupError> {
        if self.thread != std::thread::current().id() {
            return Err(CleanupLookupError::WrongThread);
        }
        self.owner_identity
            .as_ref()
            .as_ref()
            .map_err(|error| CleanupLookupError::Identity(error.clone()))?
            .verify()
    }

    fn matches_owner<G, R>(&self, owner: &PendingPtraceCleanup<G, R>) -> bool {
        owner
            .driver
            .quarantine
            .as_ref()
            .is_some_and(|permit| permit.id == self.id)
            && Arc::ptr_eq(&self.primary, &owner.failure.primary)
    }

    /// Retain an executable or similar guard with this already-failed cleanup.
    ///
    /// `G` and `R` have the same meaning as in [`Self::take_cleanup`]. No guest
    /// work is resumed here. The exact resource follows this owner across
    /// quarantine/recovery and is released before spawn admission reopens.
    /// A cleanup error remains diagnostic and retains the resource for retry.
    ///
    /// If identity/type lookup refuses, the resource is deliberately retained
    /// in a separate process-lifetime quarantine and spawns remain refused even
    /// if the original owner later completes. The returned error does not imply
    /// that the resource was dropped or released; there is no recovery API for
    /// this unbound-resource case. Dropping the marker never releases a guard.
    pub fn retain_cleanup_resource<G: 'static, R: 'static, C: PtraceCleanupResource + 'static>(
        &self,
        resource: C,
    ) -> Result<(), CleanupLookupError> {
        // Protect the original before *any* identity or TLS lookup, not merely
        // in the eventual unbound fallback.
        let mut resource = Some(std::mem::ManuallyDrop::new(
            Box::new(resource) as Box<dyn PtraceCleanupResource>
        ));
        let result = self.verify_owner().and_then(|()| {
            CLEANUP_QUARANTINE
                .try_with(|owners| {
                    let mut owners = owners
                        .try_borrow_mut()
                        .map_err(|_| CleanupLookupError::StorageUnavailable)?;
                    let owner = owners
                        .get_mut(&self.id)
                        .ok_or(CleanupLookupError::AlreadyTaken)?;
                    let owner = owner
                        .downcast_mut::<PendingPtraceCleanup<G, R>>()
                        .ok_or(CleanupLookupError::WrongType)?;
                    if !self.matches_owner(owner) {
                        return Err(CleanupLookupError::WrongGeneration);
                    }
                    owner.driver.resources.push(resource.take().unwrap());
                    Ok(())
                })
                .unwrap_or(Err(CleanupLookupError::StorageUnavailable))
        });
        if let Some(resource) = resource {
            quarantine_boxed_resource(resource);
        }
        result
    }

    /// Recover the same pending owner without executing it or reconstructing state.
    ///
    /// `G` is the original GlobalTool type; `R` is `ExitStatus` for plain/discard
    /// waits and `Output` for captured waits. A failed lookup retains the owner.
    pub fn take_cleanup<G: 'static, R: 'static>(
        &self,
    ) -> Result<PendingPtraceCleanup<G, R>, CleanupLookupError> {
        self.verify_owner()?;
        CLEANUP_QUARANTINE.with(|owners| {
            let mut owners = owners.borrow_mut();
            let owner = owners
                .get(&self.id)
                .ok_or(CleanupLookupError::AlreadyTaken)?;
            let pending = owner
                .downcast_ref::<PendingPtraceCleanup<G, R>>()
                .ok_or(CleanupLookupError::WrongType)?;
            if !self.matches_owner(pending) {
                return Err(CleanupLookupError::WrongGeneration);
            }
            let owner = std::mem::ManuallyDrop::into_inner(owners.remove(&self.id).unwrap());
            Ok(*owner
                .downcast::<PendingPtraceCleanup<G, R>>()
                .unwrap_or_else(|_| unreachable!("checked owner type")))
        })
    }
}

impl<G: 'static, R: 'static> PendingPtraceCleanup<G, R> {
    fn quarantine(mut self) -> Error {
        let id = self
            .driver
            .quarantine
            .get_or_insert_with(QuarantinePermit::new)
            .id;
        let owner_identity = self
            .driver
            .owner_identity
            .get_or_insert_with(|| Arc::new(CleanupOwnerIdentity::capture().map_err(Arc::new)))
            .clone();
        let error = CleanupUnconfirmed {
            owner_identity,
            id,
            thread: self.driver.work.tracer.ptracer_thread,
            primary: self.failure.primary.clone(),
            origin: self.failure.origin,
        };
        CLEANUP_QUARANTINE.with(|owners| {
            assert!(
                owners
                    .borrow_mut()
                    .insert(id, std::mem::ManuallyDrop::new(Box::new(self)))
                    .is_none()
            );
        });
        Error::Tool(anyhow::Error::new(error))
    }
}

/// A supervisor trigger obtained before consuming an ordinary tracer's wait.
///
/// This handle does not own physical cleanup and cannot reap or resume a tracee.
/// Keep polling the original completion future after requesting termination.
#[derive(Clone)]
pub struct PtraceTerminationHandle {
    session: Arc<FatalSession>,
}
impl PtraceTerminationHandle {
    /// Publish the caller's typed cause and wake the same bounded cleanup owner.
    ///
    /// The first cause wins; a deadline requested after a Tool failure remains a
    /// secondary cause. Returns false after confirmed completion. This does not
    /// drop or complete the consuming wait future.
    pub fn terminate(&self, cause: Error) -> bool {
        self.session.request_termination(cause)
    }
}

/// The completed run, or the owner of cleanup which could not yet finish.
#[must_use = "a pending outcome owns unfinished ptrace cleanup"]
pub enum ToolRunOutcome<G, R = ExitStatus> {
    /// Every ordinary task, consuming hook, and requested output drain finished.
    Complete(crate::ToolRunCompletion<G, R>),
    /// The same owners remain available for another bounded cleanup attempt.
    CleanupPending(PendingPtraceCleanup<G, R>),
    /// This instrumentation route does not support starting the public ordinary
    /// completion contract. The original tracer and its pipes remain untouched.
    /// Legacy static-injected waits nevertheless retain failed cleanup through
    /// [`CleanupUnconfirmed`]; dynamic LiteInst uses [`InjectedCleanupUnconfirmed`].
    /// Neither route makes normal completion or supervisor setup supported.
    UnsupportedBackend(Box<Tracer<G>>),
}

/// Retained ordinary-owned cleanup, bound to its original ptracer thread.
/// This includes failed static-injected legacy waits.
///
/// This value is neither Send nor Sync. Dropping it, or abandoning an in-flight
/// wait, is not a completed-cleanup operation. Legacy waits retain a refused
/// owner in thread-local quarantine; explicit completion callers receive it.
#[must_use = "resume or retain this owner; dropping it does not certify cleanup"]
pub struct PendingPtraceCleanup<G, R = ExitStatus> {
    driver: CompletionDriver<G, R>,
    failure: crate::PtraceRunFailure,
}

struct CompletionDriver<G, R> {
    quarantine: Option<QuarantinePermit>,
    owner_identity: Option<Arc<Result<CleanupOwnerIdentity, Arc<std::io::Error>>>>,
    // Abandonment does not release guards. Successful cleanup explicitly takes
    // and drops each one before completing the quarantine permit.
    resources: Vec<std::mem::ManuallyDrop<Box<dyn PtraceCleanupResource>>>,
    local: tokio::task::LocalSet,
    work: Box<CompletionWork<G, R>>,
}

struct CompletionWork<G, R> {
    tracer: Tracer<G>,
    stdout: crate::capture::CaptureDrain,
    stderr: crate::capture::CaptureDrain,
    tree_done: bool,
    status: Option<ExitStatus>,
    captured: bool,
    result: fn(ExitStatus, Vec<u8>, Vec<u8>) -> R,
}

impl<G, R> PendingPtraceCleanup<G, R> {
    /// The retained original cause, later errors, and prefix at this yield.
    pub fn failure(&self) -> &crate::PtraceRunFailure {
        &self.failure
    }

    /// Snapshot original callback errnos retained by this same cleanup owner.
    pub fn callback_diagnostics(&self) -> Vec<crate::PtraceCallbackDiagnostic> {
        self.driver
            .work
            .tracer
            .ordinary_session
            .callback_diagnostics()
    }

    /// Continue the same owned cleanup on the original ptracer thread.
    ///
    /// This does not restart callbacks, reconstruct task state, or retry a
    /// failed output reader. Its exposed prefix moves back to the same drains.
    pub async fn resume_cleanup(mut self) -> ToolRunOutcome<G, R> {
        let prefix = self.failure.captured_prefix.take();
        let (stdout, stderr) = match prefix {
            Some(prefix) => (Some(prefix.stdout), Some(prefix.stderr)),
            None => (None, None),
        };
        self.driver
            .work
            .stdout
            .restore_prefix(stdout)
            .expect("same owned stdout prefix");
        self.driver
            .work
            .stderr
            .restore_prefix(stderr)
            .expect("same owned stderr prefix");
        drop(self.failure);
        self.driver.work.tracer.ordinary_session.resume_cleanup();
        self.driver.drive().await
    }
}

impl<G, R> CompletionWork<G, R> {
    fn take_prefix(&mut self) -> Option<crate::CapturedPrefix> {
        let stdout = self.stdout.take_prefix().expect("one stdout owner");
        let stderr = self.stderr.take_prefix().expect("one stderr owner");
        self.captured.then(|| crate::CapturedPrefix {
            stdout: stdout.expect("capturing stdout"),
            stderr: stderr.expect("capturing stderr"),
        })
    }

    async fn round(&mut self) -> bool {
        let session = self.tracer.ordinary_session.clone();
        let completion = future::poll_fn(|cx| {
            if !self.tree_done
                && let std::task::Poll::Ready(result) = self.tracer.tracer.as_mut().poll(cx)
            {
                self.tree_done = true;
                self.tracer.tracer = Box::pin(future::pending());
                match result {
                    Ok(status) => self.status = Some(status),
                    Err(error) => session.fail(error),
                }
            }
            for (drain, phase) in [
                (&mut self.stdout, "ptrace stdout capture"),
                (&mut self.stderr, "ptrace stderr capture"),
            ] {
                match drain.poll(cx) {
                    std::task::Poll::Ready(crate::capture::DrainEvent::Error(error)) => {
                        session.fail_at(
                            reverie::BackendFailure {
                                pid: self.tracer.guest_pid,
                                tid: self.tracer.guest_pid,
                                phase,
                            },
                            error.into(),
                        );
                    }
                    std::task::Poll::Ready(crate::capture::DrainEvent::Progress) => {
                        cx.waker().wake_by_ref()
                    }
                    _ => {}
                }
            }
            if self.tree_done && self.stdout.is_finished() && self.stderr.is_finished() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        });
        futures::pin_mut!(completion);
        let failed = session.cancelled();
        futures::pin_mut!(failed);
        tokio::select! {
            biased;
            () = &mut completion => return true,
            () = failed => {}
        }
        tokio::select! {
            biased;
            () = &mut completion => true,
            _ = session.cleanup_refused() => false,
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(session.deadline())) => {
                session.fail(Error::Tool(anyhow::Error::new(CleanupDeadlineExceeded)));
                false
            },
        }
    }
}

impl<G, R> CompletionDriver<G, R> {
    async fn drive(mut self) -> ToolRunOutcome<G, R> {
        assert_eq!(
            self.work.tracer.ptracer_thread,
            std::thread::current().id(),
            "ptrace cleanup must stay on its original thread"
        );
        let mut complete = self.local.run_until(self.work.round()).await;
        let session = self.work.tracer.ordinary_session.clone();
        if complete && Arc::strong_count(&self.work.tracer.gref) != 1 {
            session.fail(
                anyhow::anyhow!("global Tool still has owners after the task tree joined").into(),
            );
        }
        if complete && Arc::strong_count(&self.work.tracer.gref) == 1 {
            let mut index = 0;
            while index < self.resources.len() {
                #[cfg(all(test, target_arch = "x86_64"))]
                injected_error_tests::static_driver_observation(
                    session.failure_snapshot().as_ref(),
                    self.work.stdout.observed_prefix_for_test(),
                    self.work.stderr.observed_prefix_for_test(),
                    self.work.stdout.is_finished() && self.work.stderr.is_finished(),
                );
                match self.resources[index].cleanup() {
                    Ok(()) => drop(std::mem::ManuallyDrop::into_inner(
                        self.resources.remove(index),
                    )),
                    Err(error) => {
                        session.fail_at(
                            reverie::BackendFailure {
                                pid: self.work.tracer.guest_pid,
                                tid: self.work.tracer.guest_pid,
                                phase: "ptrace retained cleanup resource",
                            },
                            error,
                        );
                        complete = false;
                        index += 1;
                    }
                }
            }
        }
        if !complete || Arc::strong_count(&self.work.tracer.gref) != 1 {
            let mut failure = session
                .failure_snapshot()
                .expect("bounded cleanup starts after publication");
            failure.captured_prefix = self.work.take_prefix();
            return ToolRunOutcome::CleanupPending(PendingPtraceCleanup {
                driver: self,
                failure,
            });
        }
        let mut prefix = self.work.take_prefix();
        let failure = session.take_public_failure().await.map(|mut failure| {
            failure.captured_prefix = prefix.take();
            failure
        });
        let result = match failure {
            Some(failure) => Err(failure),
            None => {
                let (stdout, stderr) = match prefix {
                    Some(prefix) => (prefix.stdout, prefix.stderr),
                    None => (Vec::new(), Vec::new()),
                };
                Ok((self.work.result)(
                    self.work
                        .status
                        .expect("successful tree returned an actual status"),
                    stdout,
                    stderr,
                ))
            }
        };
        let global_state = Arc::try_unwrap(self.work.tracer.gref)
            .unwrap_or_else(|_| unreachable!("checked after all owners joined"));
        if let Some(permit) = self.quarantine.take() {
            permit.complete();
        }
        ToolRunOutcome::Complete(crate::ToolRunCompletion {
            global_state,
            result,
            callback_diagnostics: session.take_callback_diagnostics(),
        })
    }
}

impl<G: Default + 'static> Tracer<G> {
    fn completion<R>(
        mut self,
        mode: u8,
        result: fn(ExitStatus, Vec<u8>, Vec<u8>) -> R,
    ) -> CompletionDriver<G, R> {
        use crate::capture::BoxedRead;
        use crate::capture::CaptureDrain;
        if mode != 0 {
            drop(self.stdin.take());
        }
        let stdout = if mode != 0 {
            self.stdout.take().map(|io| Box::pin(io) as BoxedRead)
        } else {
            None
        };
        let stderr = if mode != 0 {
            self.stderr.take().map(|io| Box::pin(io) as BoxedRead)
        } else {
            None
        };
        let (stdout, stderr) = if mode == 1 {
            (CaptureDrain::capture(stdout), CaptureDrain::capture(stderr))
        } else {
            (CaptureDrain::discard(stdout), CaptureDrain::discard(stderr))
        };
        CompletionDriver {
            quarantine: None,
            owner_identity: None,
            resources: Vec::new(),
            local: tokio::task::LocalSet::new(),
            work: Box::new(CompletionWork {
                tracer: self,
                stdout,
                stderr,
                tree_done: false,
                status: None,
                captured: mode == 1,
                result,
            }),
        }
    }

    /// Wait for ordinary-ptrace completion, retaining failed global Tool state.
    ///
    /// Like `wait`, this does not drain piped output. A failed cleanup yields its
    /// original owner after at most the cleanup attempt's two-second wait bound;
    /// a synchronous kernel operation may itself take longer. Arbitrary future
    /// abandonment and experimental instrumentation-backend teardown are outside
    /// this ordinary-ptrace completion contract.
    pub async fn wait_completion(self) -> ToolRunOutcome<G> {
        if !self.ordinary_completion_supported {
            return ToolRunOutcome::UnsupportedBackend(Box::new(self));
        }
        self.completion(0, |status, _, _| status).drive().await
    }

    /// Capture output and retain exact byte prefixes on an ordinary-ptrace failure.
    pub async fn wait_with_output_completion(self) -> ToolRunOutcome<G, Output> {
        if !self.ordinary_completion_supported {
            return ToolRunOutcome::UnsupportedBackend(Box::new(self));
        }
        self.completion(1, |status, stdout, stderr| Output {
            status,
            stdout,
            stderr,
        })
        .drive()
        .await
    }

    /// Drain output without accumulating prefixes while retaining failed state.
    pub async fn wait_discarding_output_completion(self) -> ToolRunOutcome<G> {
        if !self.ordinary_completion_supported {
            return ToolRunOutcome::UnsupportedBackend(Box::new(self));
        }
        self.completion(2, |status, _, _| status).drive().await
    }

    /// Obtain a supervisor trigger before consuming an ordinary completion wait.
    /// Returns `None` for the explicitly unsupported experimental routes.
    pub fn termination_handle(&self) -> Option<PtraceTerminationHandle> {
        self.ordinary_completion_supported
            .then(|| PtraceTerminationHandle {
                session: self.ordinary_session.clone(),
            })
    }

    /// Returns the PID of the root guest process.
    pub fn guest_pid(&self) -> Pid {
        self.guest_pid
    }

    /// Returns a live observer for this tracer's LiteInst patch-site statistics.
    pub fn liteinst_instrumentation_stats(&self) -> Option<LiteinstInstrumentationStatsHandle> {
        self.liteinst_instrumentation_stats
            .as_ref()
            .map(|stats| LiteinstInstrumentationStatsHandle::from_shared(Arc::clone(stats)))
    }

    /// Returns the live ptrace activity-statistics source when collection was enabled.
    pub fn backend_stats(&self) -> Option<PtraceBackendStatsSource> {
        self.backend_stats.clone()
    }

    /// Simultaneously waits for the tracee to exit and collect all remaining
    /// output on the stdout/stderr handles, returning an `Output` instance.
    ///
    /// The stdin handle to the child process, if any, will be closed before
    /// waiting. This helps avoid deadlock: it ensures that the child does not
    /// block waiting for input from the parent, while the parent waits for the
    /// child to exit.
    ///
    /// By default, stdin, stdout and stderr are inherited from the parent. In
    /// order to capture the output it is necessary to create new pipes between
    /// parent and child. Use `stdout(Stdio::piped())` or
    /// `stderr(Stdio::piped())`, respectively.
    ///
    /// This legacy result projects away nonfatal callback diagnostics. Use the
    /// additive completion API when those raw errors and owner outcomes matter.
    pub async fn wait_with_output(self) -> Result<(Output, G), Error> {
        if self.liteinst_cleanup.is_none() {
            let outcome = self
                .completion(1, |status, stdout, stderr| Output {
                    status,
                    stdout,
                    stderr,
                })
                .drive()
                .await;
            return match outcome {
                ToolRunOutcome::Complete(completion) => completion
                    .result
                    .map(|result| (result, completion.global_state))
                    .map_err(crate::PtraceRunFailure::into_legacy_error),
                ToolRunOutcome::CleanupPending(pending) => Err(pending.quarantine()),
                ToolRunOutcome::UnsupportedBackend(tracer) => {
                    Box::pin(tracer.wait_with_output()).await
                }
            };
        }
        self.wait_injected_legacy(1).await
    }

    /// Waits for the tracee to exit while concurrently draining and discarding
    /// any piped stdout/stderr, returning its exit status and global state.
    ///
    /// This is the discard-output counterpart of [`Tracer::wait_with_output`]
    /// and shares its deadlock-avoidance behavior: the stdin handle, if any, is
    /// closed before waiting, and both output pipes are read as the guest
    /// produces bytes. Unlike `wait_with_output` the bytes are sunk rather than
    /// buffered, so a guest that writes unbounded output costs no memory here.
    ///
    /// Prefer this over the bare [`Tracer::wait`] whenever the caller piped the
    /// guest's stdio but does not want the output. `wait` never touches the
    /// pipes, so a guest that fills the (64 KiB by default) pipe buffer blocks
    /// in `write(2)` forever while the parent waits for a process that can
    /// never exit.
    pub async fn wait_discarding_output(self) -> Result<(ExitStatus, G), Error> {
        if self.liteinst_cleanup.is_none() {
            let outcome = self.completion(2, |status, _, _| status).drive().await;
            return match outcome {
                ToolRunOutcome::Complete(completion) => completion
                    .result
                    .map(|result| (result, completion.global_state))
                    .map_err(crate::PtraceRunFailure::into_legacy_error),
                ToolRunOutcome::CleanupPending(pending) => Err(pending.quarantine()),
                ToolRunOutcome::UnsupportedBackend(tracer) => {
                    Box::pin(tracer.wait_discarding_output()).await
                }
            };
        }
        self.wait_injected_legacy(2)
            .await
            .map(|(output, state)| (output.status, state))
    }

    /// Waits for the tracee to exit and returns its exit status and global
    /// state.
    ///
    /// This does **not** touch the guest's stdio handles. If the caller piped
    /// stdout or stderr, use [`Tracer::wait_with_output`] or
    /// [`Tracer::wait_discarding_output`] instead; otherwise a guest that fills
    /// an unread pipe buffer deadlocks against this wait.
    ///
    /// This legacy result projects away nonfatal callback diagnostics. Use the
    /// additive completion API when those raw errors and owner outcomes matter.
    pub async fn wait(self) -> Result<(ExitStatus, G), Error> {
        if self.liteinst_cleanup.is_none() {
            let outcome = self.completion(0, |status, _, _| status).drive().await;
            return match outcome {
                ToolRunOutcome::Complete(completion) => completion
                    .result
                    .map(|result| (result, completion.global_state))
                    .map_err(crate::PtraceRunFailure::into_legacy_error),
                ToolRunOutcome::CleanupPending(pending) => Err(pending.quarantine()),
                ToolRunOutcome::UnsupportedBackend(tracer) => Box::pin(tracer.wait()).await,
            };
        }
        self.wait_injected_legacy(0)
            .await
            .map(|(output, state)| (output.status, state))
    }

    async fn wait_injected_legacy(mut self, mode: u8) -> Result<(Output, G), Error> {
        use std::task::Poll;

        use crate::capture::BoxedRead;
        use crate::capture::CaptureDrain;
        use crate::capture::DrainEvent;

        assert_eq!(self.ptracer_thread, std::thread::current().id());
        if mode != 0 {
            drop(self.stdin.take());
        }
        let stdout = (mode != 0)
            .then(|| self.stdout.take())
            .flatten()
            .map(|io| Box::pin(io) as BoxedRead);
        let stderr = (mode != 0)
            .then(|| self.stderr.take())
            .flatten()
            .map(|io| Box::pin(io) as BoxedRead);
        let mut owner = LegacyInjectedOwner {
            tracer: self,
            local: tokio::task::LocalSet::new(),
            stdout: if mode == 1 {
                CaptureDrain::capture(stdout)
            } else {
                CaptureDrain::discard(stdout)
            },
            stderr: if mode == 1 {
                CaptureDrain::capture(stderr)
            } else {
                CaptureDrain::discard(stderr)
            },
            permit: None,
            failure: None,
        };
        let session = owner.tracer.ordinary_session.clone();
        let mut status = None;
        let work = future::poll_fn(|cx| {
            if session.is_failed() {
                return Poll::Ready(());
            }
            if status.is_none() {
                match owner.tracer.tracer.as_mut().poll(cx) {
                    Poll::Ready(Ok(value)) => status = Some(value),
                    Poll::Ready(Err(error)) => session.fail(error),
                    Poll::Pending => {}
                }
            }
            if session.is_failed() {
                return Poll::Ready(());
            }
            for (drain, phase) in [
                (&mut owner.stdout, "injected stdout capture"),
                (&mut owner.stderr, "injected stderr capture"),
            ] {
                match drain.poll(cx) {
                    Poll::Ready(DrainEvent::Error(error)) => session.fail_at(
                        reverie::BackendFailure {
                            pid: owner.tracer.guest_pid,
                            tid: owner.tracer.guest_pid,
                            phase,
                        },
                        error.into(),
                    ),
                    Poll::Ready(DrainEvent::Progress) => cx.waker().wake_by_ref(),
                    _ => {}
                }
            }
            if session.is_failed()
                || (status.is_some() && owner.stdout.is_finished() && owner.stderr.is_finished())
            {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        // Register a failure wake independently of the root: a descendant can
        // fail while the root is blocked in a guest wait or consuming callback.
        owner
            .local
            .run_until(async {
                tokio::select! {
                    biased;
                    () = session.cancelled() => {},
                    () = work => {},
                }
            })
            .await;
        if session.is_failed() {
            // Cancel the root run-loop before transferring its held-stop lease
            // to the guard. Child futures stay owned by this same LocalSet and
            // cannot run while synchronous physical cleanup is in progress.
            owner.tracer.tracer = Box::pin(future::pending());
            let origin = reverie::BackendFailure {
                pid: owner.tracer.guest_pid,
                tid: owner.tracer.guest_pid,
                phase: "injected tracee cleanup confirmation",
            };
            if !owner
                .tracer
                .liteinst_cleanup
                .as_mut()
                .expect("legacy injected owner has its original guard")
                .terminate_fatal_and_confirm(session.deadline(), |error| {
                    session.fail_at(origin, error.into());
                })
            {
                let mut failure = session
                    .failure_snapshot()
                    .expect("published original failure");
                if mode == 1 {
                    failure.captured_prefix = Some(crate::CapturedPrefix {
                        stdout: owner
                            .stdout
                            .take_prefix()
                            .expect("one stdout owner")
                            .unwrap(),
                        stderr: owner
                            .stderr
                            .take_prefix()
                            .expect("one stderr owner")
                            .unwrap(),
                    });
                }
                let permit = QuarantinePermit::new();
                let id = permit.id;
                owner.permit = Some(permit);
                let failure = Arc::new(failure);
                owner.failure = Some(failure.clone());
                let error = InjectedCleanupUnconfirmed { id, failure };
                CLEANUP_QUARANTINE.with(|owners| {
                    assert!(
                        owners
                            .borrow_mut()
                            .insert(id, std::mem::ManuallyDrop::new(Box::new(owner)))
                            .is_none()
                    );
                });
                return Err(anyhow::Error::new(error).into());
            }
            // Only confirmed physical cleanup permits destruction of remaining
            // child handlers/global state. No fabricated guest status is used.
            drop(owner);
            return Err(session
                .take_public_failure()
                .await
                .expect("published failure")
                .into_legacy_error());
        }
        owner
            .tracer
            .liteinst_cleanup
            .as_mut()
            .expect("original guard")
            .disarm();
        let stdout = owner
            .stdout
            .take_prefix()
            .expect("one stdout owner")
            .unwrap_or_default();
        let stderr = owner
            .stderr
            .take_prefix()
            .expect("one stderr owner")
            .unwrap_or_default();
        let global = Arc::try_unwrap(owner.tracer.gref).unwrap_or_else(|_| {
            panic!("Reverie internal invariant broken. Arc::try_unwrap on global state failed.")
        });
        Ok((
            Output {
                status: status.expect("completed tree has a real status"),
                stdout,
                stderr,
            },
            global,
        ))
    }
}

fn from_nix_error(err: nix::Error) -> Errno {
    Errno::new(err as i32)
}

// Private initialization outcomes. Exited carries an already observed status,
// never a manufactured Running/Stopped/Zombie capability.
#[derive(Debug)]
enum PostspawnError {
    Trace(TraceError),
    Exited { pid: Pid, exit_status: ExitStatus },
}

impl From<TraceError> for PostspawnError {
    fn from(error: TraceError) -> Self {
        Self::Trace(error)
    }
}

impl From<Errno> for PostspawnError {
    fn from(error: Errno) -> Self {
        Self::Trace(error.into())
    }
}

fn initialization_exit_error(pid: Pid, exit_status: ExitStatus) -> Error {
    tracing::error!(
        target: "reverie_ptrace::lifecycle",
        %pid,
        ?exit_status,
        "guest exited during ptrace initialization"
    );
    anyhow::anyhow!("tracee {pid} exited during ptrace initialization with {exit_status:?}").into()
}

async fn postspawn_error(pid: Pid, error: PostspawnError) -> Error {
    match error {
        PostspawnError::Trace(error) => initialization_error(pid, error).await,
        PostspawnError::Exited { pid, exit_status } => initialization_exit_error(pid, exit_status),
    }
}

async fn initialization_error(pid: Pid, err: TraceError) -> Error {
    match err {
        TraceError::Errno(errno) => {
            anyhow::anyhow!("failed to initialize ptrace for tracee {pid}: {errno}").into()
        }
        TraceError::Died(zombie) => {
            let exit_status = match zombie.reap().await {
                Ok(exit_status) => exit_status,
                Err(reap_error) => {
                    return anyhow::anyhow!(
                        "tracee {pid} died during ptrace initialization and its terminal status could not be reaped: {reap_error}"
                    )
                    .into();
                }
            };
            initialization_exit_error(pid, exit_status)
        }
    }
}

fn report_pre_exec_capability_error(message: &'static [u8]) -> Errno {
    let errno = Errno::last();
    // SAFETY: write is async-signal-safe and message has static storage. This
    // runs after fork, where tracing and allocation are not safe.
    let _ = unsafe { libc::write(libc::STDERR_FILENO, message.as_ptr().cast(), message.len()) };
    errno
}

/// Sets up the child process for ptracing right before execve is called.
fn init_tracee(intercept_rdtsc: bool) -> Result<(), Errno> {
    // NOTE: There should be *NO* allocations along the happy path here.
    // Allocating between a fork() and execve() can cause deadlocks in glibc
    // when using jemalloc.

    // hardcoded because `libc` does not export these.
    const PER_LINUX: u64 = 0x0;
    const ADDR_NO_RANDOMIZE: u64 = 0x0004_0000;

    if intercept_rdtsc {
        // Intercepting rdtsc is only possible on x86
        #[cfg(target_arch = "x86_64")]
        unsafe {
            if libc::prctl(libc::PR_SET_TSC, libc::PR_TSC_SIGSEGV, 0, 0, 0) != 0 {
                return Err(report_pre_exec_capability_error(
                    b"ERROR: Reverie could not enable RDTSC interception with prctl(PR_SET_TSC)\n",
                ));
            }
        };
    }

    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(report_pre_exec_capability_error(
                b"ERROR: Reverie could not enable PR_SET_NO_NEW_PRIVS for seccomp interception\n",
            ));
        }
        if libc::personality(PER_LINUX | ADDR_NO_RANDOMIZE) == -1 {
            return Err(report_pre_exec_capability_error(
                b"ERROR: Reverie could not disable address-space randomization with personality(2)\n",
            ));
        }
    }

    // FIXME: This is a hacky workaround for `std::process::Command::spawn`
    // getting stuck in a deadlock because of the SIGSTOP below.
    // `Command::spawn` uses a pipe to communicate the error code to the parent
    // process if the `execve` fails. The idea is that the write end of the pipe
    // will be closed upon a successful call to `execve` and the parent will
    // abort the blocking read on the read end of the pipe. We don't know
    // exactly which file descriptor the pipe uses, so we attempt to close the
    // first N file descriptors hoping it is among those. Unfortunately, in
    // doing so, we lose the ability to capture `execve` failures.
    //
    // There are a couple options for a better implementation:
    //  1. Recreate the entire `std::process` module to provide better ptrace
    //     support. (A lot of work!)
    //  2. Don't raise a SIGSTOP, but instead let the ptracer stop on the call to
    //     `execve` and have the parent set the ptrace options at that point.
    for i in 3..256 {
        unsafe {
            libc::close(i);
        }
    }

    safeptrace::traceme_and_stop()?;

    unsafe {
        signal::sigaction(
            signal::SIGTTIN,
            &signal::SigAction::new(
                signal::SigHandler::SigIgn,
                signal::SaFlags::SA_RESTART,
                signal::SigSet::empty(),
            ),
        )
        .map_err(from_nix_error)?;

        signal::sigaction(
            signal::SIGTTOU,
            &signal::SigAction::new(
                signal::SigHandler::SigIgn,
                signal::SaFlags::SA_RESTART,
                signal::SigSet::empty(),
            ),
        )
        .map_err(from_nix_error)?;
    }

    Ok(())
}

async fn run_orphaned(orphans: mpsc::Receiver<Child>, session: Option<Arc<FatalSession>>) {
    tokio_stream::wrappers::ReceiverStream::new(orphans)
        .for_each_concurrent(None, |orphan| {
            let session = session.clone();
            async move {
                let pid = orphan.id();
                let Some(mut daemonizer) = orphan.daemonizer_rx else {
                    tracing::error!(
                        %pid,
                        "orphan is missing its daemonization channel; waiting for exit"
                    );
                    let status = orphan.handle.await;
                    tracing::debug!(%pid, ?status, "orphan exited");
                    return;
                };

                let daemonizer = daemonizer.recv();
                futures::pin_mut!(daemonizer);

                match future::select(Box::pin(orphan.handle), daemonizer).await {
                    Either::Left((exit_status, _)) => {
                        tracing::debug!(
                            "[reverie] Orphan {} exited with status {:?}",
                            pid,
                            exit_status
                        );
                    }
                    Either::Right((kill_switch, handle)) => {
                        tracing::debug!("[reverie] pid {} daemonized", pid);
                        if let Some(mut kill_switch) = kill_switch {
                            let kill_switch = kill_switch.recv();
                            futures::pin_mut!(kill_switch);
                            match future::select(Box::pin(handle), kill_switch).await {
                                Either::Left((exit_status, _)) => {
                                    tracing::debug!(
                                        "[reverie] Daemon {} exited with status {:?}",
                                        pid,
                                        exit_status
                                    );
                                }
                                Either::Right((_, handle)) => {
                                    tracing::debug!("sending sigkill {}", pid);
                                    if let Some(session) = &session {
                                        if !session.is_failed() {
                                            let signal = orphan
                                                .ordinary_group
                                                .as_ref()
                                                .ok_or(Errno::ESTALE)
                                                .and_then(|subscription| {
                                                    session.signal_subscribed_group(subscription)
                                                });
                                            if let Err(error) = signal
                                                && error != Errno::ESRCH
                                            {
                                                session.fail(error.into());
                                            }
                                        }
                                    } else {
                                        unsafe {
                                            libc::kill(pid.as_raw(), libc::SIGKILL);
                                        }
                                    }
                                    let status = handle.await;
                                    tracing::debug!(
                                        "[reverie] Daemon {} exited with status {:?}",
                                        pid,
                                        status
                                    );
                                }
                            }
                        }
                    }
                }
            }
        })
        .await;
}

/// Runs the task tree to completion and returns the exit status of the root
/// task.
async fn run_task_tree<T: Tool + 'static>(
    root: TracedTask<T>,
    child: Stopped,
    orphanage: mpsc::Receiver<Child>,
    liteinst_fail_closed: bool,
    ordinary_owned: bool,
) -> Result<ExitStatus, Error> {
    let failure = root.fatal_session();
    let root = root.run(child);
    let orphans = run_orphaned(orphanage, ordinary_owned.then(|| failure.clone()));
    futures::pin_mut!(root, orphans);
    let result = match future::select(root, orphans).await {
        future::Either::Left((result, orphans)) => {
            if result.is_ok() || !liteinst_fail_closed {
                // A successful root, and every non-LiteInst backend, still
                // owns orderly orphan completion.
                orphans.await;
            }
            // A failed LiteInst root must return control to its session cleanup
            // guard immediately. A failed descendant can retain an orphanage
            // sender while its Tool exit callback is pending, and waiting for
            // that channel to close would prevent the guard from terminating
            // the exact tracee generations which make the callback pending.
            result
        }
        future::Either::Right(((), root)) => root.await,
    };
    if ordinary_owned {
        failure.join_owned().await;
    }
    result
}

type AttachedRun = (
    BoxFuture<'static, Result<ExitStatus, Error>>,
    Arc<FatalSession>,
);

/// Helper function for everything after the child is spawned.
#[tracing::instrument(
    target = "reverie_ptrace::lifecycle",
    name = "tracee.attach",
    level = "debug",
    skip_all,
    fields(pid = %child.pid())
)]
async fn postspawn<L: Tool + 'static>(
    child: Running,
    gref: Arc<L::GlobalState>,
    config: <L::GlobalState as GlobalTool>::Config,
    options: TracedTaskOptions<'_>,
    gdbserver: Option<GdbServer>,
) -> Result<AttachedRun, PostspawnError> {
    let pid = child.pid();

    // Wait for the child to enter a stopped state. The child will enter a
    // stopped state immediately after ptrace::traceme is called.
    //
    // NOTE: We may rarely get spurious signals here, like SIGWINCH, so we must
    // skip past them.
    let (mut child, event) = match child.wait_for_signal(Signal::SIGSTOP).await? {
        Wait::Stopped(child, event) => (child, event),
        Wait::Exited(pid, exit_status) => {
            return Err(PostspawnError::Exited { pid, exit_status });
        }
    };
    assert_eq!(event, Event::Signal(Signal::SIGSTOP));

    child.setoptions(
        ptrace::Options::PTRACE_O_TRACEEXEC
            | ptrace::Options::PTRACE_O_EXITKILL
            | ptrace::Options::PTRACE_O_TRACECLONE
            | ptrace::Options::PTRACE_O_TRACEFORK
            | ptrace::Options::PTRACE_O_TRACEVFORK
            | ptrace::Options::PTRACE_O_TRACEVFORKDONE
            | ptrace::Options::PTRACE_O_TRACEEXIT
            | ptrace::Options::PTRACE_O_TRACESECCOMP
            | ptrace::Options::PTRACE_O_TRACESYSGOOD,
    )?;

    let (orphan_sender, orphan_receiver) = mpsc::channel(1);
    let (daemon_kill, _) = broadcast::channel(1);
    let liteinst_fail_closed = options.liteinst_runtime.is_some();
    let ordinary_owned = options.liteinst_runtime.is_none();

    // This is the root task, so there's no reason to make run its init routine
    // asynchronously, as there isn't any other work to do.
    let mut tracer = TracedTask::<L>::new(
        pid,
        config,
        gref,
        options,
        orphan_sender,
        daemon_kill,
        gdbserver,
    );

    let ordinary_session = tracer.fatal_session();
    tracer.arm_liteinst_root_stop(&child, &Event::Signal(Signal::SIGSTOP));
    if ordinary_owned {
        ordinary_session.capture_root(&child);
    }
    if !ordinary_session.is_failed() {
        child = tracer.tracee_preinit(child).await?;
    }

    let tracer = Box::pin(run_task_tree(
        tracer,
        child,
        orphan_receiver,
        liteinst_fail_closed,
        ordinary_owned,
    ));
    Ok((tracer, ordinary_session))
}

/// Creates the seccomp filter. This lets us control which syscalls are traced
/// and which ones are allowed through.
fn seccomp_filter(events: &Subscription) -> seccomp::Filter {
    use reverie::process::seccomp::Action;

    seccomp::FilterBuilder::new()
        // By default, all syscalls are allowed through untraced. Then, we can
        // intercept only the syscalls we are interested in.
        .default_action(Action::Allow)
        .syscalls(
            events
                .iter_syscalls()
                .map(|syscall| (syscall, Action::Trace(0))),
        )
        // rt_sigreturn must execute from Reverie's private page while restoring
        // a signal frame. restart_syscall deliberately has no unconditional
        // override: like every ordinary syscall, it is traced exactly when the
        // Tool subscribes to it and otherwise falls through to the Allow default.
        .syscall(Sysno::rt_sigreturn, Action::Allow)
        // Allow untraced syscalls through without tracing them.
        .ip_range(
            (cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE) as u64,
            (cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE + cp::UD_INSTR_SIZE) as u64,
            Action::Allow,
        )
        .build()
}

/// Specifies *how* the GDB server should listen for incoming connections.
pub enum GdbConnection {
    /// The server shall bind to and listen on the given socket address.
    Addr(SocketAddr),

    /// The server shall bind to and listen on the given unix domain socket. This
    /// path must not exist, otherwise the bind will fail with `EADDRINUSE`.
    Path(PathBuf),
}

impl From<SocketAddr> for GdbConnection {
    fn from(addr: SocketAddr) -> Self {
        Self::Addr(addr)
    }
}

impl From<PathBuf> for GdbConnection {
    fn from(path: PathBuf) -> Self {
        Self::Path(path)
    }
}

impl From<u16> for GdbConnection {
    fn from(port: u16) -> Self {
        Self::Addr(([127, 0, 0, 1], port).into())
    }
}

/// A builder for creating a tracer.
pub struct TracerBuilder<T: Tool + 'static> {
    /// The program to execute that will be traced.
    command: Command,

    /// The global state static config.
    config: Option<<T::GlobalState as GlobalTool>::Config>,

    /// Set to `Some` if we should spawn a GDB server.
    gdbserver: Option<GdbConnection>,

    /// Indicates that the guest's scheduling will be serialized by the Reverie
    /// tool. This is only relevant for the GDB server.
    sequentialized_guest: bool,

    /// Marker and exact RIP identifying an injected syscall trap, when enabled.
    injected_syscall_trap: Option<InjectedSyscallTrap>,

    /// Dynamic LiteInst runtime handshake and hot-site configuration.
    liteinst_runtime: Option<LiteinstRuntimeConfig>,

    /// Whether to collect general ptrace activity statistics.
    backend_stats_request: BackendStatsRequest,

    #[cfg(all(test, target_arch = "x86_64"))]
    clock_test_launcher_branches: u64,
}

impl<T: Tool + 'static> TracerBuilder<T> {
    /// Creates the builder with the given command.
    pub fn new(command: Command) -> Self {
        Self {
            command,
            config: None,
            gdbserver: None,
            sequentialized_guest: false,
            injected_syscall_trap: None,
            liteinst_runtime: None,
            backend_stats_request: BackendStatsRequest::DISABLED,
            #[cfg(all(test, target_arch = "x86_64"))]
            clock_test_launcher_branches: 0,
        }
    }

    /// Returns a reference to the command to be traced.
    pub fn command(&self) -> &Command {
        &self.command
    }

    /// Sets the static configuration that will be made available to the tool.
    pub fn config(mut self, config: <T::GlobalState as GlobalTool>::Config) -> Self {
        self.config = Some(config);
        self
    }

    /// Configures the tracer to create a GDB server and listen for incoming
    /// connections. The tracer will start in a stopped state and will not
    /// proceed until a connection is made. This allows the GDB client to observe
    /// the full execution of the guest.
    pub fn gdbserver<C: Into<GdbConnection>>(mut self, connection: C) -> Self {
        self.gdbserver = Some(connection.into());
        self
    }

    /// Make the GDB server aware that guest threads are sequentialized. This is
    /// needed when the Reverie tool has full control of scheduling and already
    /// sequentializes thread execution. This helps avoid deadlocks.
    pub fn sequentialized_guest(mut self) -> Self {
        self.sequentialized_guest = true;
        self
    }

    /// Enables or disables general ptrace activity statistics for this run.
    pub fn backend_stats(mut self, request: BackendStatsRequest) -> Self {
        self.backend_stats_request = request;
        self
    }

    /// Routes matching `SIGTRAP` stops through `Tool::handle_syscall_event`.
    ///
    /// A binary rewriter must place `marker` in RAX, an e9tool-compatible
    /// writable `state` frame pointer in RDI, and execute `int3` at `rip - 1`.
    /// All other traps retain their normal signal/debugger semantics.
    // TODO-HUMAN-REVIEW(PR-103): Review the injected syscall event provenance API.
    pub fn injected_syscall_trap(mut self, marker: u64, rip: u64) -> Self {
        self.injected_syscall_trap = Some(InjectedSyscallTrap {
            marker,
            rip,
            provenance: None,
        });
        self
    }

    /// Enables the dynamic LiteInst runtime handshake and injected hot-site path.
    ///
    /// The preload path validates handshake instruction pointers against the
    /// expected executable mapping. Distinct markers, exact return sites, and
    /// mapping generations reject accidental collisions; they are not a
    /// security boundary against arbitrary code already running in the tracee.
    /// Dynamic mode follows threads and child processes under the ordinary
    /// ptrace lifecycle, but hook installation is single-task only: the patch
    /// helper runs on a process-global stack and the installer is not
    /// re-entrant across tasks, so the hook set freezes at the first task
    /// creation. It still fails closed on a vfork child and on an exec after
    /// start, neither of which can preserve the preload runtime.
    // TODO-HUMAN-REVIEW(PR-270): Review dynamic LiteInst provenance API.
    pub fn liteinst_runtime(
        self,
        preload: impl Into<PathBuf>,
        begin_marker: u64,
        ready_marker: u64,
        helper_return_marker: u64,
        syscall_marker: u64,
    ) -> Self {
        self.liteinst_runtime_with_stats(
            preload,
            begin_marker,
            ready_marker,
            helper_return_marker,
            syscall_marker,
            BackendStatsRequest::DISABLED,
        )
    }

    /// Enables the dynamic LiteInst runtime and optionally collects patch statistics.
    pub fn liteinst_runtime_with_stats(
        mut self,
        preload: impl Into<PathBuf>,
        begin_marker: u64,
        ready_marker: u64,
        helper_return_marker: u64,
        syscall_marker: u64,
        stats_request: BackendStatsRequest,
    ) -> Self {
        self.liteinst_runtime = Some(LiteinstRuntimeConfig {
            preload: preload.into(),
            begin_marker,
            ready_marker,
            helper_return_marker,
            syscall_marker,
            newborn_tracees: Arc::new(StdMutex::new(HashMap::new())),
            held_root_stop: Arc::new(StdMutex::new(None)),
            root_tid: Arc::new(StdOnceLock::new()),
            multi_task: Arc::new(AtomicBool::new(false)),
            session_failure: Arc::new(StdMutex::new(None)),
            session_failure_changed: Arc::new(tokio::sync::Notify::new()),
            instrumentation_stats: stats_request
                .is_enabled()
                .then(|| Arc::new(StdMutex::new(LiteinstInstrumentationStats::default()))),
            #[cfg(test)]
            fail_preinit: false,
            #[cfg(test)]
            fail_new_task: false,
            #[cfg(test)]
            pause_new_task: None,
            #[cfg(test)]
            pause_after_new_task: false,
            #[cfg(test)]
            pause_before_new_task: None,
            #[cfg(test)]
            fail_discovery_once: None,
            #[cfg(test)]
            fail_after_scan_once: None,
            #[cfg(test)]
            force_task_scan_once: None,
            #[cfg(test)]
            pause_root_stop: None,
            #[cfg(test)]
            pause_preinit_step: None,
            #[cfg(test)]
            pause_precise_timer_step: None,
            #[cfg(test)]
            activate_without_handshake: false,
            #[cfg(test)]
            queue_pending_signal_once: None,
            #[cfg(test)]
            force_skip_signal_once: None,
            #[cfg(test)]
            force_context_none_signal_once: None,
            #[cfg(test)]
            force_context_signal_once: None,
            #[cfg(test)]
            force_preinit_signal_once: None,
            #[cfg(test)]
            force_post_exec_signal_once: None,
            #[cfg(test)]
            force_private_stub_mutation_once: None,
        });
        self
    }

    #[cfg(test)]
    fn fail_liteinst_preinit_for_test(mut self) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before preinit failure injection")
            .fail_preinit = true;
        self
    }

    #[cfg(test)]
    fn fail_liteinst_new_task_for_test(mut self) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before new-task failure injection")
            .fail_new_task = true;
        self
    }

    #[cfg(test)]
    fn pause_liteinst_new_task_for_test(mut self, sender: mpsc::UnboundedSender<Pid>) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before child-event pause")
            .pause_new_task = Some(sender);
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before child-event pause")
            .pause_after_new_task = true;
        self
    }

    #[cfg(test)]
    fn observe_liteinst_new_task_for_test(mut self, sender: mpsc::UnboundedSender<Pid>) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before child-event observation")
            .pause_new_task = Some(sender);
        self
    }

    #[cfg(test)]
    fn pause_before_liteinst_new_task_for_test(
        mut self,
        sender: mpsc::UnboundedSender<Pid>,
    ) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before pre-handler pause")
            .pause_before_new_task = Some(sender);
        self
    }

    #[cfg(test)]
    fn fail_liteinst_discovery_once_for_test(mut self, flag: Arc<AtomicBool>) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before discovery failure injection")
            .fail_discovery_once = Some(flag);
        self
    }

    #[cfg(test)]
    fn fail_liteinst_after_task_scan_once_for_test(
        mut self,
        fail: Arc<AtomicBool>,
        force_scan: Arc<AtomicBool>,
    ) -> Self {
        let runtime = self
            .liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before scan failure injection");
        runtime.fail_after_scan_once = Some(fail);
        runtime.force_task_scan_once = Some(force_scan);
        self
    }

    #[cfg(test)]
    fn pause_liteinst_root_stop_for_test(
        mut self,
        stop: RootStopPause,
        sender: mpsc::UnboundedSender<Pid>,
    ) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before root-stop pause")
            .pause_root_stop = Some((stop, sender));
        self
    }

    #[cfg(test)]
    fn pause_liteinst_preinit_step_for_test(
        mut self,
        step: usize,
        sender: mpsc::UnboundedSender<Pid>,
    ) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before preinit pause")
            .pause_preinit_step = Some((step, sender));
        self
    }

    #[cfg(test)]
    fn pause_liteinst_precise_timer_step_for_test(
        mut self,
        sender: mpsc::UnboundedSender<Pid>,
    ) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before precise-timer pause")
            .pause_precise_timer_step = Some(sender);
        self
    }

    #[cfg(test)]
    fn activate_liteinst_without_handshake_for_test(mut self) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before test-only activation")
            .activate_without_handshake = true;
        self
    }

    #[cfg(test)]
    fn queue_liteinst_pending_signal_once_for_test(mut self, queue_once: Arc<AtomicBool>) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before pending-signal injection")
            .queue_pending_signal_once = Some(queue_once);
        self
    }

    #[cfg(test)]
    fn force_liteinst_skip_signal_once_for_test(mut self, force_once: Arc<AtomicBool>) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before skip-signal injection")
            .force_skip_signal_once = Some(force_once);
        self
    }

    #[cfg(test)]
    fn force_liteinst_context_none_signal_once_for_test(
        mut self,
        force_once: Arc<AtomicBool>,
    ) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before reinjection-signal injection")
            .force_context_none_signal_once = Some(force_once);
        self
    }

    #[cfg(test)]
    fn force_liteinst_context_signal_once_for_test(mut self, force_once: Arc<AtomicBool>) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before injection-signal injection")
            .force_context_signal_once = Some(force_once);
        self
    }

    #[cfg(test)]
    fn force_liteinst_preinit_signal_once_for_test(mut self, force_once: Arc<AtomicBool>) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before preinit-signal injection")
            .force_preinit_signal_once = Some(force_once);
        self
    }

    #[cfg(test)]
    fn force_liteinst_post_exec_signal_once_for_test(
        mut self,
        force_once: Arc<AtomicBool>,
    ) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before post-exec-signal injection")
            .force_post_exec_signal_once = Some(force_once);
        self
    }

    #[cfg(test)]
    fn force_liteinst_private_stub_mutation_once_for_test(
        mut self,
        force_once: Arc<AtomicBool>,
    ) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before private-stub mutation")
            .force_private_stub_mutation_once = Some(force_once);
        self
    }

    /// Filters a binary-rewriter trap unless its logical instruction address
    /// names an ahead-of-time patched site in the configured executable's
    /// canonical pathname/inode identity.
    ///
    /// This rejects accidental marker/frame collisions; it is not a security
    /// boundary against guest code that deliberately forges a real site.
    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(PR-271): Review site-validated binary-rewriter trap API.
    pub fn site_validated_injected_syscall_trap(
        mut self,
        marker: u64,
        rip: u64,
        image: impl Into<PathBuf>,
        image_entry_address: u64,
        patched_site_addresses: impl IntoIterator<Item = u64>,
    ) -> Result<Self, Error> {
        let image = std::fs::canonicalize(image.into())?;
        let image_metadata = std::fs::metadata(&image)?;
        let mut patched_site_addresses = patched_site_addresses.into_iter().collect::<Vec<_>>();
        patched_site_addresses.sort_unstable();
        patched_site_addresses.dedup();
        if patched_site_addresses.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "site-validated injected-syscall traps require at least one patched site",
            )
            .into());
        }
        self.injected_syscall_trap = Some(InjectedSyscallTrap {
            marker,
            rip,
            provenance: Some(InjectedSyscallProvenance {
                image,
                image_inode: image_metadata.ino(),
                image_entry_address,
                patched_site_addresses: patched_site_addresses.into(),
            }),
        });
        Ok(self)
    }

    /// Spawns the tracer.
    pub async fn spawn(self) -> Result<Tracer<T::GlobalState>, Error> {
        // A retained failed tree must not acquire peers through another mode.
        let _ordinary_admission = OrdinaryAdmission::acquire()?;
        if self.liteinst_runtime.is_some() && self.gdbserver.is_some() {
            return Err(Error::Tool(anyhow::anyhow!(
                "LiteInst runtime activation with a GDB server is unsupported ({}): both controllers would own the executable-entry software breakpoint",
                Errno::ENOTSUPP
            )));
        }
        let backend_stats = PtraceBackendStatsSource::from_request(self.backend_stats_request);
        let mut command = self.command;
        let config = self.config.unwrap_or_default();
        let liteinst_fail_closed = self.liteinst_runtime.is_some();
        let ordinary_completion_supported =
            self.liteinst_runtime.is_none() && self.injected_syscall_trap.is_none();

        // Because this ptrace backend is CENTRALIZED, it can keep all the
        // tool's state here in a single address space.
        let global_state = <T::GlobalState as GlobalTool>::init_global_state(&config).await;
        let events = T::subscriptions(&config);
        let mut traced_events = events.clone();
        if self.liteinst_runtime.is_some() {
            // Mapping operations are controller-only lifecycle observations:
            // trace them so successful VMA churn can invalidate patched-site
            // provenance, without adding them to the Tool's subscription set.
            traced_events.syscalls([
                Sysno::mmap,
                Sysno::munmap,
                Sysno::mremap,
                Sysno::mprotect,
                Sysno::pkey_mprotect,
            ]);
        }
        let gref = Arc::new(global_state);

        // Get the full path to the program and change the command to use it. This
        // also checks that the path exists and provides an early exit just in case
        // it doesn't.
        //
        // Normally, we'd rely upon the `exit(1)` following a failed call to
        // `execve`, but that is tricky when ptracing the `execve` call.
        resolve_program(&mut command)?;

        // Disable sanitizers that use ptrace from running on tracer.
        command.env("LSAN_OPTIONS", "detect_leaks=0");
        command.env("ASAN_OPTIONS", "detect_leaks=0");

        let intercept_rdtsc = events.has_rdtsc();
        #[cfg(all(test, target_arch = "x86_64"))]
        let clock_test_launcher_branches = self.clock_test_launcher_branches;
        unsafe {
            command.pre_exec(move || {
                init_tracee(intercept_rdtsc)?;
                // A caller's earlier pre_exec callback runs before init_tracee
                // stops. This private test seam instead runs after the parent
                // has constructed the stopped child's clock and resumed it.
                #[cfg(all(test, target_arch = "x86_64"))]
                if clock_test_launcher_branches != 0 {
                    core::arch::asm!(
                        "2:",
                        "dec {count}",
                        "jnz 2b",
                        count = inout(reg) clock_test_launcher_branches => _,
                        options(nomem, nostack),
                    );
                }
                Ok(())
            });
        }

        command.seccomp(seccomp_filter(&traced_events));

        let mut child = command.spawn().context("Failed to spawn tracee")?;
        let guest_pid = child.id();
        if let Some(runtime) = self.liteinst_runtime.as_ref() {
            // Publish the session root before any task can observe the config.
            // Everything LiteInst-root-scoped keys off this exact TID rather
            // than the `tid == pid` shape, which a forked child also has.
            runtime
                .root_tid
                .set(guest_pid)
                .expect("LiteInst root TID is published exactly once per spawn");
        }
        let running_child = Running::new(guest_pid);
        let liteinst_newborn_tracees = self
            .liteinst_runtime
            .as_ref()
            .map(|runtime| Arc::clone(&runtime.newborn_tracees));
        let liteinst_held_root_stop = self
            .liteinst_runtime
            .as_ref()
            .map(|runtime| Arc::clone(&runtime.held_root_stop));
        let liteinst_instrumentation_stats = self
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.instrumentation_stats.as_ref().map(Arc::clone));
        #[cfg(test)]
        let fail_discovery_once = self
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.fail_discovery_once.clone());
        #[cfg(test)]
        let fail_after_scan_once = self
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.fail_after_scan_once.clone());
        #[cfg(test)]
        let force_task_scan_once = self
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.force_task_scan_once.clone());
        let mut liteinst_cleanup = if liteinst_fail_closed {
            match LiteinstTraceeCleanup::new(
                guest_pid,
                liteinst_newborn_tracees.expect("LiteInst runtime config must exist"),
                liteinst_held_root_stop.expect("LiteInst runtime config must exist"),
            ) {
                Ok(cleanup) => {
                    #[cfg(test)]
                    let cleanup = {
                        let mut cleanup = cleanup;
                        cleanup.fail_discovery_once = fail_discovery_once;
                        cleanup.fail_after_scan_once = fail_after_scan_once;
                        cleanup.force_task_scan_once = force_task_scan_once;
                        cleanup
                    };
                    Some(cleanup)
                }
                Err(error) => {
                    // pidfd is a required LiteInst cleanup capability. The
                    // just-spawned, unreaped PID cannot have been reused yet,
                    // so a one-time numeric kill is safe only on this setup
                    // failure path; all active guards signal through pidfd.
                    let kill_result = unsafe { libc::kill(guest_pid.as_raw(), libc::SIGKILL) };
                    let kill_error = (kill_result == -1).then(Errno::last);
                    let drain_result = drain_unregistered_child(Running::new(guest_pid));
                    return Err(liteinst_pidfd_setup_error(
                        guest_pid,
                        error,
                        kill_error,
                        drain_result,
                    )
                    .into());
                }
            }
        } else {
            None
        };

        // Configure the gdb server (if any).
        let gdbserver = match self.gdbserver {
            None => None,
            Some(connection) => {
                let server = match connection {
                    GdbConnection::Addr(addr) => GdbServer::from_addr(addr).await,
                    GdbConnection::Path(path) => GdbServer::from_path(&path).await,
                };

                let mut server = server.with_context(|| {
                    format!("failed to start GDB server for tracee {guest_pid}")
                })?;

                if self.sequentialized_guest {
                    server.sequentialized_guest();
                }

                Some(server)
            }
        };

        // From this point on, every wait status belongs to safeptrace's
        // notifier. Cancellation and initialization errors must request
        // termination through the guard and await notifier unregistration;
        // they must never call raw waitpid for this PID.
        if let Some(cleanup) = liteinst_cleanup.as_mut() {
            cleanup.register_notifier(&running_child);
        }

        let (tracer, ordinary_session) = match postspawn::<T>(
            running_child,
            gref.clone(),
            config,
            TracedTaskOptions {
                command_bootstrap: true,
                events: &events,
                injected_syscall_trap: self.injected_syscall_trap,
                liteinst_runtime: self.liteinst_runtime,
                backend_stats: backend_stats.clone(),
            },
            gdbserver,
        )
        .await
        {
            Ok(tracer) => tracer,
            Err(err) => {
                let error = postspawn_error(guest_pid, err).await;
                if let Some(cleanup) = liteinst_cleanup.as_mut()
                    && let Err(cleanup_error) = cleanup.terminate_and_confirm()
                {
                    return Err(anyhow::anyhow!(
                        "LiteInst tracee cleanup failed after {error}: {cleanup_error}"
                    )
                    .into());
                }
                return Err(error);
            }
        };

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Don't let the drop logic run for the child. Tokio will add the child to a
        // "orphan queue" that will try to call `waitpid` on the process when a
        // `SIGCHLD` signal is received. This interferes with our own process
        // handling where we need full control over the lifetime of the child
        // process.
        core::mem::forget(child);

        Ok(Tracer {
            guest_pid,
            tracer,
            ordinary_session,
            ptracer_thread: std::thread::current().id(),
            ordinary_completion_supported,
            gref,
            stdin,
            stdout,
            stderr,
            liteinst_cleanup,
            liteinst_instrumentation_stats,
            backend_stats,
        })
    }
}

fn resolve_program(command: &mut Command) -> Result<(), Error> {
    let arg0 = command.get_arg0().to_owned();
    let program = command
        .find_program()
        .with_context(|| format!("Could not execute {:?}", command.get_program()))?;
    command.program(program).arg0(arg0);
    Ok(())
}

/// Spawn a *function* to be executed under instrumentation instrumentation
/// (rather than a subprocess indicated with a Command).
///
/// This still creates a fresh child process and runs it under ptrace. However,
/// the child process is a fork of the current process, and is used to run the
/// indicated function.
pub async fn spawn_fn<L, F>(fun: F) -> Result<Tracer<L::GlobalState>, Error>
where
    L: Tool + 'static,
    F: FnOnce(),
{
    spawn_fn_with_config::<L, F>(fun, Default::default(), true).await
}

/// Spawn a function with instrumentation rather than a subprocess indicated with
/// a Command. This still creates a fresh child process and runs it under ptrace.
/// However, the child process is a fork of the current process, and is used to
/// run the indicated function.
///
/// The main use case for this entrypoint into the library is testing.
pub async fn spawn_fn_with_config<L, F>(
    fun: F,
    config: <L::GlobalState as GlobalTool>::Config,
    capture_output: bool,
) -> Result<Tracer<L::GlobalState>, Error>
where
    L: Tool + 'static,
    F: FnOnce(),
{
    let _ordinary_admission = OrdinaryAdmission::acquire()?;
    // Because this ptrace backend is CENTRALIZED, it can keep all the
    // tool's state here in a single address space.
    let global_state = <L::GlobalState as GlobalTool>::init_global_state(&config).await;
    let events = L::subscriptions(&config);
    let gref = Arc::new(global_state);

    let seccomp_filter = seccomp_filter(&events);

    let (read1, write1) = unistd::pipe().map_err(from_nix_error)?;
    let (read2, write2) = unistd::pipe().map_err(from_nix_error)?;

    // Disable io redirection just before forking. We want the child process to
    // be able to call `println!()` and have that output go to stdout.
    //
    // See: https://github.com/rust-lang/rust/issues/35136
    let output_capture = std::io::set_output_capture(None);

    // Warning: fork is wildely unsafe in Rust because of runtime issues (printing,
    // panicking, etc).  We make a best-effort attempt to solve some of these issues.
    match unsafe { unistd::fork() }.expect("unistd::fork failed") {
        ForkResult::Child => {
            read1.close()?;
            read2.close()?;
            if capture_output {
                unistd::dup2_stdout(&write1).map_err(from_nix_error)?;
                unistd::dup2_stderr(&write2).map_err(from_nix_error)?;
                write1.close()?;
                write2.close()?;
            }

            init_tracee(events.has_rdtsc()).expect("init_tracee failed");

            seccomp_filter.load().expect("Failed to set seccomp filter");

            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(fun)) {
                Ok(()) => {
                    std::io::stdout().flush()?;
                    std::process::exit(0);
                }
                Err(e) => {
                    std::io::stdout().flush()?;
                    let _ = nix::unistd::write(
                        unsafe { BorrowedFd::borrow_raw(2) },
                        format!("Forked Rust process panicked, cause: {:?}", e).as_ref(),
                    );
                    std::process::exit(1);
                }
            };
        }
        ForkResult::Parent { child } => {
            std::io::set_output_capture(output_capture);

            let guest_pid = Pid::from(child);
            let child = Running::new(guest_pid);
            write1.close()?;
            write2.close()?;

            let stdout = read1.into();
            let stderr = read2.into();
            let (tracer, ordinary_session) = match postspawn::<L>(
                child,
                gref.clone(),
                config,
                TracedTaskOptions {
                    command_bootstrap: false,
                    events: &events,
                    injected_syscall_trap: None,
                    liteinst_runtime: None,
                    backend_stats: None,
                },
                None,
            )
            .await
            {
                Ok(tracer) => tracer,
                Err(err) => return Err(postspawn_error(guest_pid, err).await),
            };

            Ok(Tracer {
                guest_pid,
                tracer,
                ordinary_session,
                ptracer_thread: std::thread::current().id(),
                ordinary_completion_supported: true,
                gref,
                stdin: None,
                stdout: Some(stdout),
                stderr: Some(stderr),
                liteinst_cleanup: None,
                liteinst_instrumentation_stats: None,
                backend_stats: None,
            })
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
#[path = "clock_origin_tests.rs"]
mod clock_origin_tests;

#[cfg(all(test, target_arch = "x86_64"))]
#[path = "injection_stop_tests.rs"]
mod injection_stop_tests;

#[cfg(all(test, target_arch = "x86_64"))]
#[path = "injected_error_tests.rs"]
mod injected_error_tests;

#[cfg(test)]
mod tests {
    include!("tracer/fatal_callback_tests.rs");
    include!("tracer/fatal_vfork_tests.rs");
    include!("tracer/fatal_parent_kill_tests.rs");
    include!("tracer/fatal_callback_owner_tests.rs");
    include!("tracer/fatal_quarantine_tests.rs");
    include!("tracer/fatal_namespace_timer_tests.rs");
    include!("tracer/fatal_exit_payload_tests.rs");
    include!("tracer/fatal_daemon_group_tests.rs");
    include!("tracer/fatal_capacity_tests.rs");
    include!("tracer/fatal_group_lifetime_tests.rs");
    #[tokio::test(flavor = "current_thread")]
    async fn unsupported_injected_completion_returns_original_pipes_and_usable_tracer() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf unchanged-out; printf unchanged-err >&2"])
            .stdout(reverie::process::Stdio::piped())
            .stderr(reverie::process::Stdio::piped());
        let tracer = TracerBuilder::<()>::new(command)
            .injected_syscall_trap(u64::MAX, u64::MAX)
            .spawn()
            .await
            .unwrap();
        let pid = tracer.guest_pid();
        let state = Arc::as_ptr(&tracer.gref);
        let stdout = format!("{:?}", tracer.stdout.as_ref().unwrap());
        let stderr = format!("{:?}", tracer.stderr.as_ref().unwrap());
        let ToolRunOutcome::UnsupportedBackend(tracer) = tracer.wait_with_output_completion().await
        else {
            panic!("experimental injected route was misclassified as ordinary completion");
        };
        assert_eq!(tracer.guest_pid(), pid);
        assert_eq!(Arc::as_ptr(&tracer.gref), state);
        assert_eq!(format!("{:?}", tracer.stdout.as_ref().unwrap()), stdout);
        assert_eq!(format!("{:?}", tracer.stderr.as_ref().unwrap()), stderr);
        let (output, ()) = tokio::time::timeout(Duration::from_secs(3), tracer.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(output.status, ExitStatus::Exited(0));
        assert_eq!(output.stdout, b"unchanged-out");
        assert_eq!(output.stderr, b"unchanged-err");
    }

    #[test]
    fn completion_future_storage_sizes() {
        fn output_size<A, B>(_: impl FnOnce(A) -> B) -> usize {
            std::mem::size_of::<B>()
        }
        eprintln!(
            "future_storage_bytes: tracer={} driver={} capture_drain={} completion={} legacy={} resume={} combined_refusal_fixture={} spawn={}",
            std::mem::size_of::<Tracer<FatalLog>>(),
            std::mem::size_of::<CompletionDriver<FatalLog, ExitStatus>>(),
            std::mem::size_of::<crate::capture::CaptureDrain>(),
            output_size(Tracer::<FatalLog>::wait_completion),
            output_size(Tracer::<FatalLog>::wait),
            output_size(PendingPtraceCleanup::<FatalLog>::resume_cleanup),
            output_size(|()| freeze_refusal_recovery(false)),
            output_size(|()| spawn_fn_with_config::<FatalTool, _>(|| {}, 0, false))
        );
    }

    #[derive(Debug, thiserror::Error)]
    #[error("supervisor test deadline")]
    struct TestDeadline;

    #[tokio::test(flavor = "current_thread")]
    async fn supervisor_termination_keeps_healthy_loop_owned_until_actual_completion() {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(3);
        let words = FatalWords::new();
        let address = words.0 as usize;
        let tracer = spawn_fn::<(), _>(move || {
            unsafe { &*(address as *const std::sync::atomic::AtomicUsize) }
                .store(1, Ordering::SeqCst);
            loop {
                unsafe {
                    libc::pause();
                }
            }
        })
        .await
        .unwrap();
        let root = tracer.guest_pid();
        let handle = tracer.termination_handle().unwrap();
        let completion = tracer.wait_with_output_completion();
        futures::pin_mut!(completion);
        tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), async {
            while words.read(0) == 0 {
                tokio::select! {
                    _ = &mut completion => panic!("healthy looping guest completed before termination"),
                    () = tokio::task::yield_now() => {}
                }
            }
        }).await.expect("healthy loop did not start within fixture bound");
        let before_publication = started.elapsed();
        let published = Instant::now();
        assert!(handle.terminate(Error::Tool(anyhow::Error::new(TestDeadline))));
        let result = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            &mut completion,
        )
        .await
        .expect("supervisor cleanup exceeded single fixture deadline");
        let ToolRunOutcome::Complete(completed) = result else {
            panic!("healthy loop cleanup unconfirmed")
        };
        let failure = completed
            .result
            .expect_err("deadline turned into a guest status");
        assert!(
            matches!(failure.primary(), Error::Tool(error) if error.downcast_ref::<TestDeadline>().is_some())
        );
        assert_eq!(failure.origin().phase, "ptrace supervisor termination");
        assert_eq!(failure.captured_prefix().unwrap().stdout(), b"");
        assert_eq!(failure.captured_prefix().unwrap().stderr(), b"");
        assert!(
            !handle.terminate(Error::Tool(anyhow::Error::new(TestDeadline))),
            "completed run accepted another cause"
        );
        assert_reaped("supervisor terminated root", root);
        eprintln!(
            "supervisor timings: before_publication={before_publication:?}, cleanup={:?}, total={:?}",
            published.elapsed(),
            started.elapsed()
        );
    }

    async fn captured_tool_result(fail: bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let tracer = spawn_fn_with_config::<FatalTool, _>(
            || {
                assert_eq!(
                    unsafe { libc::write(1, b"out\0\xff".as_ptr().cast(), 5) },
                    5
                );
                assert_eq!(
                    unsafe { libc::write(2, [b'e', b'r', b'r', 0xfe, 0].as_ptr().cast(), 5) },
                    5
                );
                std::thread::spawn(|| unsafe {
                    libc::syscall(libc::SYS_getpgid, 0);
                })
                .join()
                .unwrap();
            },
            u8::from(fail),
            true,
        )
        .await
        .unwrap();
        let root = tracer.guest_pid();
        let outcome = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            tracer.wait_with_output_completion(),
        )
        .await
        .expect("captured result exceeded one fixture deadline");
        let ToolRunOutcome::Complete(completed) = outcome else {
            panic!("capture did not complete")
        };
        if fail {
            let failure = completed.result.expect_err("Tool failure lost");
            assert!(
                matches!(failure.primary(), Error::Tool(error) if error.downcast_ref::<NonleaderFailure>().is_some())
            );
            let bytes = failure.captured_prefix().expect("public failure prefix");
            assert_eq!(bytes.stdout(), b"out\0\xff");
            assert_eq!(bytes.stderr(), b"err\xfe\0");
        } else {
            let output = completed.result.unwrap();
            assert_eq!(output.status, ExitStatus::Exited(0));
            assert_eq!(output.stdout, b"out\0\xff");
            assert_eq!(output.stderr, b"err\xfe\0");
        }
        assert_reaped("captured result root", root);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn public_failure_capture_preserves_exact_binary_prefixes() {
        captured_tool_result(true).await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn public_success_capture_preserves_exact_binary_output() {
        captured_tool_result(false).await;
    }

    thread_local! {
        static FATAL_REAP_IDENTITIES: std::cell::RefCell<Option<Vec<TraceeIdentity>>> = const { std::cell::RefCell::new(None) };
    }
    struct FatalReapObservationScope;
    impl FatalReapObservationScope {
        fn new() -> Self {
            FATAL_REAP_CHRONOLOGY.with(|slot| *slot.borrow_mut() = Some((Vec::new(), 0)));
            FATAL_REAP_OBSERVATIONS.with(|slot| {
                assert!(slot.borrow().is_none());
                *slot.borrow_mut() = Some(Vec::new());
            });
            FATAL_REAP_IDENTITIES.with(|slot| {
                assert!(slot.borrow().is_none());
                *slot.borrow_mut() = Some(Vec::new());
            });
            Self
        }
    }
    impl Drop for FatalReapObservationScope {
        fn drop(&mut self) {
            FATAL_REAP_CHRONOLOGY.with(|slot| *slot.borrow_mut() = None);
            FATAL_REAP_OBSERVATIONS.with(|slot| *slot.borrow_mut() = None);
            FATAL_REAP_IDENTITIES.with(|slot| *slot.borrow_mut() = None);
        }
    }

    fn fatal_proc_read(
        identity: &TraceeIdentity,
        path: &std::ffi::CStr,
    ) -> std::io::Result<String> {
        use std::io::Read;
        let raw = unsafe {
            libc::openat(
                identity.proc_dir.as_raw_fd(),
                path.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let file = unsafe { fs::File::from_raw_fd(raw) };
        let mut text = String::new();
        file.take(16384).read_to_string(&mut text)?;
        Ok(text)
    }

    fn fatal_reap_readback() -> String {
        use std::fmt::Write as _;
        let mut output = String::new();
        FATAL_REAP_IDENTITIES.with(|slot| {
            for identity in slot.borrow().as_ref().unwrap() {
                // All observations are captured before printing or emergency cleanup.
                // Procfs and pidfd values are individually sampled, not atomic.
                let stat = fatal_proc_read(identity, c"stat");
                let status = fatal_proc_read(identity, c"status").map(|text| {
                    text.lines().filter(|line| ["State:", "PPid:", "TracerPid:", "Tgid:", "Pid:"].iter().any(|prefix| line.starts_with(prefix))).collect::<Vec<_>>().join("; ")
                });
                let mut pollfd = libc::pollfd { fd: identity.pidfd.as_ref().unwrap().as_raw_fd(), events: libc::POLLIN, revents: 0 };
                let polled = unsafe { libc::poll(&mut pollfd, 1, 0) };
                let signal_zero = identity.send_raw_signal(0);
                writeln!(&mut output, "fatal physical identity: tid={}, initial={:?}, proc_inode={}, same_process={}, stat={stat:?}, status={status:?}, pidfd_poll={polled}, pidfd_revents={}, pidfd_signal0={signal_zero:?}", identity.tid, identity.snapshot, identity.proc_inode, identity.same_process(), pollfd.revents).unwrap();
            }
        });
        FATAL_REAP_OBSERVATIONS.with(|slot| {
            for task in slot.borrow().as_ref().unwrap() {
                let terminal = task.terminal.observed_exit_status();
                let retired = task.terminal.wait(Duration::ZERO);
                let held = task.held.lock().unwrap().is_some();
                writeln!(&mut output, "fatal original notifier: tid={}, terminal={terminal:?}, retired={retired}, held_stop={held}", task.tid).unwrap();
            }
        });
        FATAL_REAP_CHRONOLOGY.with(|slot| {
            writeln!(
                &mut output,
                "fatal chronology: {:?}",
                slot.borrow().as_ref()
            )
            .unwrap();
        });
        output
    }

    type FatalEvents = Vec<(Pid, Option<ExitStatus>)>;
    #[derive(Default)]
    struct FatalLog(Arc<StdMutex<FatalEvents>>);

    #[reverie::global_tool]
    impl GlobalTool for FatalLog {
        type Config = u8;
        type Request = (Pid, Option<ExitStatus>);
        type Response = ();

        async fn receive_rpc(&self, _from: Pid, event: Self::Request) {
            self.0.lock().unwrap().push(event);
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("ordinary nonleader failure control")]
    struct NonleaderFailure;

    #[derive(Default)]
    struct FatalTool {
        starts: std::sync::atomic::AtomicUsize,
        blocked_start: AtomicBool,
    }

    #[reverie::tool]
    impl Tool for FatalTool {
        type GlobalState = FatalLog;
        type ThreadState = bool;

        fn subscriptions(_config: &u8) -> Subscription {
            [Sysno::getpgid].into_iter().collect()
        }

        async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
            if guest.tid() == guest.pid() {
                FATAL_REAP_IDENTITIES.with(|slot| {
                    if let Some(identities) = slot.borrow_mut().as_mut() {
                        identities.push(untraced_process_identity(guest.tid()));
                    }
                });
            }
            guest.send_rpc((guest.tid(), None)).await;
            if *guest.config() == 2
                && guest.tid() != guest.pid()
                && self.starts.fetch_add(1, Ordering::SeqCst) == 0
            {
                struct PendingStart<'a>(&'a AtomicBool);
                impl Drop for PendingStart<'_> {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::SeqCst);
                    }
                }
                *guest.thread_state_mut() = true;
                self.blocked_start.store(true, Ordering::SeqCst);
                let _pending = PendingStart(&self.blocked_start);
                future::pending::<()>().await;
            }
            Ok(())
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            assert_ne!(guest.tid(), guest.pid(), "failure must be a live nonleader");
            assert!(std::path::Path::new(&format!("/proc/{}", guest.tid())).exists());
            if *guest.config() == 3 {
                let pause = crate::task::FATAL_FORK_PAUSE
                    .with(|slot| slot.borrow().clone())
                    .unwrap();
                let address = pause.waiting_word.load(Ordering::SeqCst);
                unsafe { &*(address as *const std::sync::atomic::AtomicUsize) }
                    .store(1, Ordering::SeqCst);
                loop {
                    let ready = pause.ready.notified();
                    if pause.child.lock().unwrap().is_some() {
                        break;
                    }
                    ready.await;
                }
            }
            if *guest.config() == 2 {
                assert!(
                    self.blocked_start.load(Ordering::SeqCst),
                    "newborn must still be in its start callback at failure"
                );
            }
            if *guest.config() != 0 {
                eprintln!(
                    "fatal control raises original error in live tid {} (mode {})",
                    guest.tid(),
                    guest.config()
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
            blocked_start: bool,
            status: ExitStatus,
        ) -> Result<(), Error> {
            if blocked_start {
                assert!(
                    !self.blocked_start.load(Ordering::SeqCst),
                    "pending startup future was not cancelled"
                );
                assert_eq!(status, ExitStatus::Signaled(Signal::SIGKILL, false));
            }
            global.send_rpc((tid, Some(status))).await;
            Ok(())
        }
    }

    // These words are shared only by this test and its actual forked guests.
    // The post-syscall word distinguishes termination from detaching a failed
    // thread and allowing its next user instruction to execute.
    struct FatalWords(*mut std::sync::atomic::AtomicUsize);

    impl FatalWords {
        fn new() -> Self {
            let pointer = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    4096,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(pointer, libc::MAP_FAILED);
            Self(pointer.cast())
        }

        fn read(&self, index: usize) -> usize {
            unsafe { &*self.0.add(index) }.load(Ordering::SeqCst)
        }
    }

    impl Drop for FatalWords {
        fn drop(&mut self) {
            assert_eq!(unsafe { libc::munmap(self.0.cast(), 4096) }, 0);
        }
    }

    async fn ordinary_nonleader_control(fail: bool, fork_descendant: bool, blocked_newborn: bool) {
        let handed_deadline = std::env::var("REVERIE_FATAL_HANDED_DEADLINE_NS")
            .ok()
            .map(|value| value.parse::<u64>().unwrap());
        let deadline = Instant::now()
            + handed_deadline
                .map(fatal_remaining)
                .unwrap_or(Duration::from_secs(3));
        let mut natural_reaper = handed_deadline.map(|_| {
            assert!(fail && fork_descendant && !blocked_newborn);
            assert_eq!(
                std::env::var("REVERIE_FATAL_REAP_ROLE").as_deref(),
                Ok("tracer")
            );
            let raw = unsafe { libc::dup(libc::STDIN_FILENO) };
            assert!(raw >= 0);
            let channel = unsafe { std::os::unix::net::UnixStream::from_raw_fd(raw) };
            channel
                .set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
                .unwrap();
            channel
                .set_write_timeout(Some(deadline.saturating_duration_since(Instant::now())))
                .unwrap();
            channel
        });
        let _observations = FatalReapObservationScope::new();
        let words = FatalWords::new();
        let address = words.0 as usize;
        let sentinel = fork_paused_child();
        let sentinel_identity = untraced_process_identity(sentinel);
        let tracer = spawn_fn_with_config::<FatalTool, _>(
            move || {
                if fork_descendant {
                    match unsafe { unistd::fork() }.unwrap() {
                        ForkResult::Child => {
                            unsafe {
                                &*((address as *const std::sync::atomic::AtomicUsize).add(1))
                            }
                            .store(unsafe { libc::getpid() } as usize, Ordering::SeqCst);
                            loop {
                                unsafe { libc::pause() };
                            }
                        }
                        ForkResult::Parent { .. } => {
                            while unsafe {
                                &*((address as *const std::sync::atomic::AtomicUsize).add(1))
                            }
                            .load(Ordering::SeqCst)
                                == 0
                            {
                                std::thread::yield_now();
                            }
                        }
                    }
                }
                let newborn = blocked_newborn
                    .then(|| std::thread::spawn(|| panic!("cancelled newborn reached guest code")));
                std::thread::spawn(move || {
                    unsafe { libc::syscall(libc::SYS_getpgid, 0) };
                    unsafe { &*(address as *const std::sync::atomic::AtomicUsize) }
                        .store(1, Ordering::SeqCst);
                    if fail {
                        loop {
                            unsafe { libc::pause() };
                        }
                    }
                })
                .join()
                .unwrap();
                if let Some(newborn) = newborn {
                    newborn.join().unwrap();
                }
            },
            if blocked_newborn { 2 } else { u8::from(fail) },
            false,
        )
        .await
        .expect("spawn ordinary ptrace control");
        assert!(tracer.liteinst_cleanup.is_none());
        let root = tracer.guest_pid;
        let log = fail.then(|| Arc::clone(&tracer.gref.0));
        if let Some(channel) = natural_reaper.as_mut() {
            let identity = untraced_process_identity(root);
            fatal_control_write(
                channel,
                [
                    root.as_raw() as u64,
                    identity.snapshot.start_time,
                    identity.proc_inode,
                ],
            );
            assert_eq!(fatal_control_read::<1>(channel), [handed_deadline.unwrap()]);
        }
        // Test-only emergency cleanup is invoked *after* recording the actual
        // result and survivor state. It cannot satisfy the product assertions.
        let mut emergency = LiteinstTraceeCleanup::new(
            root,
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(StdMutex::new(None)),
        )
        .unwrap();
        emergency.register_notifier(&Running::new(root));
        let result = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            tracer.wait(),
        )
        .await;
        let post_syscall = words.read(0);
        let descendant = words.read(1);
        let root_absent = !std::path::Path::new(&format!("/proc/{root}")).exists();
        let descendant_absent =
            descendant == 0 || !std::path::Path::new(&format!("/proc/{descendant}")).exists();
        let sentinel_untouched = sentinel_identity.same_process()
            && unsafe { libc::waitpid(sentinel.as_raw(), std::ptr::null_mut(), libc::WNOHANG) }
                == 0;
        let physical_readback = fatal_reap_readback();
        let legacy_result = match &result {
            Ok(Ok((status, _))) => format!("success({status:?})"),
            Ok(Err(error)) => format!(
                "error({error:?}), original_nonleader={}",
                matches!(error, Error::Tool(inner) if inner.downcast_ref::<NonleaderFailure>().is_some())
            ),
            Err(error) => format!("timeout({error:?})"),
        };
        let hook_events = log.as_ref().map(|log| log.lock().unwrap().clone());
        eprintln!(
            "ordinary cleanup control: root={root}, descendant={descendant}, fail={fail}, fork={fork_descendant}, newborn={blocked_newborn}, timeout={}, root_absent={root_absent}, descendant_absent={descendant_absent}, post_syscall={post_syscall}, sentinel_untouched={sentinel_untouched}",
            result.is_err()
        );
        eprintln!(
            "{physical_readback}fatal legacy result: {legacy_result}; consuming hooks: {hook_events:?}"
        );
        let natural_reap_confirmed = if let Some(channel) = natural_reaper.as_mut() {
            let child = Pid::from_raw(i32::try_from(descendant).unwrap());
            let (start_time, inode) = FATAL_REAP_IDENTITIES.with(|slot| {
                let identities = slot.borrow();
                let identity = identities
                    .as_ref()
                    .unwrap()
                    .iter()
                    .find(|identity| identity.tid == child)
                    .expect("startup-bound fork identity");
                (identity.snapshot.start_time, identity.proc_inode)
            });
            let (actual_sigkill, retired_without_stop) = FATAL_REAP_OBSERVATIONS.with(|slot| {
                let owners = slot.borrow();
                let owners = owners.as_ref().unwrap();
                (
                    owners.len() == 3
                        && owners.iter().all(|owner| {
                            owner.terminal.observed_exit_status()
                                == Ok(Some(ExitStatus::Signaled(Signal::SIGKILL, false)))
                        }),
                    owners.len() == 3
                        && owners.iter().all(|owner| {
                            owner.terminal.wait(Duration::ZERO)
                                && owner.held.lock().unwrap().is_none()
                        }),
                )
            });
            let original_error = matches!(&result, Ok(Err(Error::Tool(error))) if error.downcast_ref::<NonleaderFailure>().is_some());
            let events = hook_events.as_ref().unwrap();
            let starts: Vec<_> = events
                .iter()
                .filter_map(|(tid, status)| status.is_none().then_some(*tid))
                .collect();
            let callbacks_valid = starts.len() == 3
                && events.len() == 6
                && starts.iter().all(|tid| {
                    events
                        .iter()
                        .filter(|(exited, status)| {
                            exited == tid
                                && *status == Some(ExitStatus::Signaled(Signal::SIGKILL, false))
                        })
                        .count()
                        == 1
                });
            // Seal every product predicate before allowing the separate natural
            // parent to wait. No emergency signal/reap has run at this point.
            fatal_control_write(
                channel,
                [
                    child.as_raw() as u64,
                    start_time,
                    inode,
                    u64::from(actual_sigkill),
                    u64::from(retired_without_stop),
                    u64::from(original_error),
                    post_syscall as u64,
                    u64::from(callbacks_valid && root_absent && sentinel_untouched),
                    unsafe { libc::syscall(libc::SYS_gettid) } as u64,
                ],
            );
            assert_eq!(fatal_control_read::<1>(channel), [1]);
            assert!(
                actual_sigkill
                    && retired_without_stop
                    && original_error
                    && callbacks_valid
                    && sentinel_untouched
                    && root_absent
            );
            assert_eq!(post_syscall, 0);
            assert!(!std::path::Path::new(&format!("/proc/{child}")).exists());
            let _remaining = fatal_remaining(handed_deadline.unwrap());
            true
        } else {
            false
        };
        emergency
            .terminate_and_confirm()
            .expect("emergency test cleanup");
        if fork_descendant && descendant != 0 && fatal_subreaper_state() == 1 {
            let pid = Pid::from_raw(i32::try_from(descendant).unwrap());
            // The controlled diagnostic wrapper adopts the zombie but does not
            // wait until after the original product predicate was recorded.
            // This is natural-parent teardown, never product qualification.
            let mut status = 0;
            let reaped = unsafe { libc::waitpid(pid.as_raw(), &mut status, libc::WNOHANG) };
            eprintln!(
                "diagnostic natural-parent teardown: pid={pid}, waited={reaped}, raw_status={status}, errno={:?}",
                Errno::last()
            );
        }
        sentinel_identity.send_signal(Signal::SIGKILL).unwrap();
        assert_eq!(
            unsafe { libc::waitpid(sentinel.as_raw(), std::ptr::null_mut(), 0) },
            sentinel.as_raw()
        );
        assert!(
            sentinel_untouched,
            "cleanup touched an unrelated live child"
        );
        assert!(
            root_absent,
            "ordinary failure returned or timed out with root alive"
        );
        if natural_reaper.is_some() {
            assert!(
                natural_reap_confirmed,
                "natural-parent wait was not confirmed"
            );
            assert!(
                !std::path::Path::new(&format!("/proc/{descendant}")).exists(),
                "fork descendant remains after exact backend and natural-parent reaping"
            );
        } else {
            assert!(
                descendant_absent,
                "ordinary failure left a fork descendant alive"
            );
        }
        if fail {
            assert_eq!(post_syscall, 0, "failed nonleader resumed guest code");
            let error = result
                .expect("fatal cleanup exceeded 3s")
                .err()
                .expect("Tool error lost");
            assert!(
                matches!(error, Error::Tool(ref inner) if inner.downcast_ref::<NonleaderFailure>().is_some()),
                "original typed Tool error lost: {error}"
            );
            let log = log.unwrap();
            let events = log.lock().unwrap();
            let starts: Vec<_> = events
                .iter()
                .filter_map(|(tid, status)| status.is_none().then_some(*tid))
                .collect();
            assert_eq!(
                starts.len(),
                if fork_descendant || blocked_newborn {
                    3
                } else {
                    2
                }
            );
            for tid in starts {
                assert_reaped("ordinary task", tid);
                assert_eq!(
                    events
                        .iter()
                        .filter(|(exited, status)| *exited == tid
                            && *status == Some(ExitStatus::Signaled(Signal::SIGKILL, false)))
                        .count(),
                    1,
                    "missing or invented terminal callback for {tid}: {events:?}"
                );
            }
        } else {
            let (status, log) = result.expect("normal control exceeded 3s").unwrap();
            assert_eq!(status, ExitStatus::Exited(0));
            assert_eq!(post_syscall, 1);
            assert_eq!(
                log.0
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, status)| *status == Some(ExitStatus::Exited(0)))
                    .count(),
                2
            );
            assert_reaped("normal root", root);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_nonleader_tool_failure_reaps_threads_without_resuming() {
        ordinary_nonleader_control(true, false, false).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_nonleader_tool_failure_reaps_fork_descendants() {
        use std::os::unix::process::CommandExt;
        const TEST: &str = "tracer::tests::ordinary_nonleader_tool_failure_reaps_fork_descendants";
        if std::env::var("REVERIE_FATAL_REAP_TEST").as_deref() == Ok(TEST) {
            assert!(std::env::args().any(|arg| arg == TEST));
            assert!(std::env::args().any(|arg| arg == "--exact"));
            match std::env::var("REVERIE_FATAL_REAP_ROLE").as_deref() {
                Ok("reaper") => fatal_natural_reaper(TEST, false),
                Ok("tracer") => ordinary_nonleader_control(true, true, false).await,
                role => panic!("unexpected handed-fork test role: {role:?}"),
            }
            return;
        }
        let deadline = fatal_monotonic_ns() + 3_000_000_000;
        let mut reaper = fatal_child_command(TEST, "reaper");
        reaper.env("REVERIE_FATAL_HANDED_DEADLINE_NS", deadline.to_string());
        unsafe {
            reaper.pre_exec(|| {
                if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        assert!(
            reaper.status().unwrap().success(),
            "isolated handed-fork proof failed"
        );
        let _remaining = fatal_remaining(deadline);
    }

    type ExecOwnerEvents = Vec<(u8, Pid, usize, Option<ExitStatus>)>;
    #[derive(Default)]
    struct ExecOwnerLog(Arc<StdMutex<ExecOwnerEvents>>);
    #[reverie::global_tool]
    impl GlobalTool for ExecOwnerLog {
        type Config = (bool, u8);
        type Request = (u8, Pid, usize, Option<ExitStatus>);
        type Response = ();
        async fn receive_rpc(&self, _from: Pid, event: Self::Request) {
            self.0.lock().unwrap().push(event);
        }
    }
    #[derive(Default)]
    struct ExecOwnerTool {
        fail_displaced: bool,
        timer_mode: u8,
        timer_requested: AtomicBool,
    }
    #[reverie::tool]
    impl Tool for ExecOwnerTool {
        type GlobalState = ExecOwnerLog;
        type ThreadState = (usize, u64);
        fn new(_pid: Pid, config: &(bool, u8)) -> Self {
            Self {
                fail_displaced: config.0,
                timer_mode: config.1,
                timer_requested: AtomicBool::new(false),
            }
        }
        fn init_thread_state(
            &self,
            tid: Pid,
            _parent: Option<(Pid, &Self::ThreadState)>,
        ) -> Self::ThreadState {
            (tid.as_raw() as usize, 0)
        }
        async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
            let clock = guest.read_clock()?;
            guest.thread_state_mut().1 = clock;
            guest
                .send_rpc((0, guest.tid(), guest.thread_state().0, None))
                .await;
            Ok(())
        }
        async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
            let (former, before) = *guest.thread_state();
            assert_ne!(
                former,
                guest.tid().as_raw() as usize,
                "displaced leader state survived exec"
            );
            assert!(
                guest.read_clock().unwrap() >= before,
                "surviving thread clock reset"
            );
            guest.send_rpc((1, guest.tid(), former, None)).await;
            if self.timer_mode >= 3 {
                let status =
                    std::fs::read_to_string(format!("/proc/{}/status", guest.tid())).unwrap();
                let pending = status
                    .lines()
                    .find_map(|line| line.strip_prefix("SigPnd:\t"))
                    .map(|word| u64::from_str_radix(word.trim(), 16).unwrap())
                    .unwrap();
                eprintln!(
                    "old timer pending at actual postexec: tid={}, mask={pending:#x}, mode={}",
                    guest.tid(),
                    self.timer_mode
                );
                assert_ne!(
                    pending & (1u64 << (reverie::PERF_EVENT_SIGNAL as u32 - 1)),
                    0,
                    "fixture did not queue the old signal across actual exec"
                );
            }
            if matches!(self.timer_mode, 1 | 2) {
                guest
                    .set_timer(reverie::TimerSchedule::Rcbs(100_000))
                    .unwrap();
                if self.timer_mode == 2 {
                    // A real signal-delivery stop must still cancel this timer.
                    assert_eq!(
                        unsafe {
                            libc::syscall(
                                libc::SYS_tgkill,
                                guest.pid().as_raw(),
                                guest.tid().as_raw(),
                                libc::SIGUSR1,
                            )
                        },
                        0
                    );
                }
            }
            Ok(())
        }
        fn subscriptions(config: &(bool, u8)) -> Subscription {
            if config.1 == 0 || config.1 >= 3 {
                [Sysno::getpgid].into_iter().collect()
            } else {
                Subscription::none()
            }
        }
        async fn handle_signal_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            signal: Signal,
        ) -> Result<Option<Signal>, Errno> {
            guest
                .send_rpc((7, guest.tid(), signal as usize, None))
                .await;
            if (self.timer_mode == 2 && signal == Signal::SIGUSR1)
                || (matches!(self.timer_mode, 6 | 7) && signal == reverie::PERF_EVENT_SIGNAL)
            {
                Ok(None)
            } else {
                Err(Errno::EPROTO)
            }
        }
        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            let result = guest.inject(syscall).await?;
            if self.timer_mode >= 3 && guest.tid() != guest.pid() {
                if matches!(self.timer_mode, 6 | 7) {
                    return Ok(result);
                }
                guest
                    .send_rpc((8, guest.tid(), self.timer_mode as usize, None))
                    .await;
                if self.timer_mode == 5 {
                    guest.set_timer(reverie::TimerSchedule::Rcbs(100))?;
                } else {
                    // Zero is rejected before queuing. One branch is a valid
                    // precise request whose notification is an artificial kick.
                    guest.set_timer_precise(reverie::TimerSchedule::Rcbs(1))?;
                }
                return Ok(result);
            }
            let clock = guest.read_clock()?;
            let first = !self.timer_requested.swap(true, Ordering::SeqCst);
            guest
                .send_rpc((if first { 5 } else { 6 }, guest.tid(), clock as usize, None))
                .await;
            if first && self.timer_mode == 7 {
                // A guest-origin queued marker must remain a Tool signal even
                // while a current controller kick is outstanding/coalesced.
                guest.set_timer_precise(reverie::TimerSchedule::Rcbs(1))?;
            } else if first && !matches!(self.timer_mode, 4 | 6) {
                guest.set_timer(reverie::TimerSchedule::Rcbs(100_000))?;
            }
            Ok(result)
        }
        async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
            let clock = guest.read_clock().unwrap();
            guest.send_rpc((9, guest.tid(), clock as usize, None)).await;
            guest
                .send_rpc((4, guest.tid(), guest.thread_state().0, None))
                .await;
        }
        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            tid: Pid,
            global: &G,
            state: Self::ThreadState,
            status: ExitStatus,
        ) -> Result<(), Error> {
            global.send_rpc((2, tid, state.0, Some(status))).await;
            if self.fail_displaced && state.0 == tid.as_raw() as usize {
                return Err(anyhow::Error::new(NonleaderFailure).into());
            }
            Ok(())
        }
        async fn on_exit_process<G: reverie::GlobalRPC<Self::GlobalState>>(
            self,
            pid: Pid,
            global: &G,
            status: ExitStatus,
        ) -> Result<(), Error> {
            global.send_rpc((3, pid, 0, Some(status))).await;
            Ok(())
        }
    }
    fn fatal_exec_timer_payload() -> &'static std::ffi::CString {
        static PAYLOAD: LazyLock<std::ffi::CString> = LazyLock::new(|| {
            let source =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fatal_exec_timer.c");
            let output = std::env::temp_dir()
                .join(format!("reverie-fatal-exec-timer-{}", std::process::id()));
            let status = std::process::Command::new("timeout")
                .args([
                    "--signal=TERM",
                    "--kill-after=1s",
                    "1s",
                    "cc",
                    "-std=c11",
                    "-O1",
                ])
                .arg(&source)
                .arg("-o")
                .arg(&output)
                .status()
                .unwrap();
            assert!(status.success(), "compile bounded exec timer payload");
            eprintln!("exec timer payload: {}", output.display());
            std::ffi::CString::new(output.as_os_str().as_encoded_bytes()).unwrap()
        });
        &PAYLOAD
    }
    async fn ordinary_exec_owner_control(fail: bool, timer_mode: u8) {
        let marker_observations = Arc::new(StdMutex::new(Vec::new()));
        crate::timer::EXEC_SIGNAL_OBSERVATIONS
            .with(|slot| *slot.borrow_mut() = Some(marker_observations.clone()));
        let timer_transfers = Arc::new(StdMutex::new(Vec::new()));
        crate::task::EXEC_TIMER_TRANSFERS
            .with(|slot| *slot.borrow_mut() = Some(timer_transfers.clone()));
        let started = Instant::now();
        let deadline = started + Duration::from_secs(3);
        let payload = (timer_mode == 0 || timer_mode >= 3).then(fatal_exec_timer_payload);
        let tracer = spawn_fn_with_config::<ExecOwnerTool, _>(
            move || {
                std::thread::spawn(move || {
                    if timer_mode >= 3 {
                        let mut signals = unsafe { std::mem::zeroed::<libc::sigset_t>() };
                        unsafe {
                            libc::sigemptyset(&mut signals);
                            libc::sigaddset(&mut signals, reverie::PERF_EVENT_SIGNAL as i32);
                            assert_eq!(
                                libc::pthread_sigmask(
                                    libc::SIG_BLOCK,
                                    &signals,
                                    std::ptr::null_mut()
                                ),
                                0
                            );
                            assert!(libc::syscall(libc::SYS_getpgid, 0) >= 0);
                            if matches!(timer_mode, 6 | 7) {
                                assert_eq!(
                                    libc::syscall(
                                        libc::SYS_tgkill,
                                        libc::getpid(),
                                        libc::syscall(libc::SYS_gettid),
                                        reverie::PERF_EVENT_SIGNAL as i32
                                    ),
                                    0
                                );
                            }
                        }
                        for index in 0..100_000u64 {
                            std::hint::black_box(index);
                        }
                    }

                    if let Some(payload) = payload {
                        let args = [payload.as_ptr(), std::ptr::null()];
                        unsafe {
                            libc::execv(args[0], args.as_ptr());
                        }
                    } else {
                        // Preserve the exact original exec-owner-03 payload.
                        let args = [
                            c"/bin/sh".as_ptr(),
                            c"-c".as_ptr(),
                            c"i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; printf survived"
                                .as_ptr(),
                            std::ptr::null(),
                        ];
                        unsafe {
                            libc::execv(args[0], args.as_ptr());
                        }
                    }
                    unsafe {
                        libc::_exit(127);
                    }
                });
                loop {
                    unsafe {
                        libc::pause();
                    }
                }
            },
            (fail, timer_mode),
            true,
        )
        .await
        .unwrap();
        let root = tracer.guest_pid();
        let log = tracer.gref.0.clone();
        let identity = untraced_process_identity(root);
        let termination = tracer.termination_handle().unwrap();
        let mut completion = Box::pin(tracer.wait_with_output_completion());
        let result = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            &mut completion,
        )
        .await;
        crate::task::EXEC_TIMER_TRANSFERS.with(|slot| *slot.borrow_mut() = None);
        crate::timer::EXEC_SIGNAL_OBSERVATIONS.with(|slot| *slot.borrow_mut() = None);
        let markers = marker_observations.lock().unwrap().clone();
        eprintln!(
            "actual delivered marker observations: tracer={}, rows={markers:?}",
            std::process::id()
        );
        let root_absent = !std::path::Path::new(&format!("/proc/{root}")).exists();
        let events = log.lock().unwrap().clone();
        let description = match &result {
            Ok(ToolRunOutcome::Complete(completed)) => format!("Complete({:?})", completed.result),
            Ok(ToolRunOutcome::CleanupPending(pending)) => {
                format!("Pending({:?})", pending.failure())
            }
            Ok(ToolRunOutcome::UnsupportedBackend(_)) => "UnsupportedBackend".to_owned(),
            Err(error) => format!("Timeout({error})"),
        };
        eprintln!(
            "exec owner before rescue: fail={fail}, timer_mode={timer_mode}, root_absent={root_absent}, events={events:?}, elapsed={:?}, outcome={description}",
            started.elapsed()
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
                    "exec owner rescue only: signal={signal:?}, completed={}",
                    matches!(rescued, Ok(ToolRunOutcome::Complete(_)))
                );
                panic!("exec owner did not Complete within original3s predicate: {description}");
            }
        };
        assert!(started.elapsed() <= Duration::from_secs(3));
        assert!(
            root_absent,
            "original product completion did not reap root before rescue"
        );
        assert_reaped("exec owner root", root);
        let timers = timer_transfers.lock().unwrap();
        assert_eq!(
            timers.len(),
            1,
            "actual exec timer handoff omitted or duplicated"
        );
        let transfer = &timers[0];
        let old = transfer
            .displaced
            .as_ref()
            .expect("PMU counters required by this fixture");
        let before = transfer.before.as_ref().expect("former counters missing");
        let after = transfer
            .after
            .as_ref()
            .expect("replacement counters missing");
        assert_ne!(old.clock_fd, before.clock_fd);
        assert_ne!(old.timer_fd, before.timer_fd);
        assert_eq!(
            (after.clock_fd, after.timer_fd),
            (before.clock_fd, before.timer_fd),
            "surviving counters reopened or replaced"
        );
        assert_ne!(old.clock_event_id, before.clock_event_id);
        assert_ne!(old.timer_event_id, before.timer_event_id);
        assert_eq!(
            (after.clock_event_id, after.timer_event_id),
            (before.clock_event_id, before.timer_event_id),
            "PERF_EVENT_IOC_ID changed across surviving transfer"
        );
        assert_ne!(before.guest_tid, root);
        // Linux de_thread can make the retained former pid object report 0.
        // The discriminating premise is that it is not already the leader.
        eprintln!("actual perf exec handoff: {transfer:?}");
        assert_ne!(
            before.kernel_owner_tid, root,
            "fixture did not exercise a stale signal owner"
        );
        assert_eq!(
            (after.guest_pid, after.guest_tid, after.kernel_owner_tid),
            (root, root, root),
            "stored tgkill or actual kernel F_OWNER_TID remains stale"
        );
        assert!(
            transfer.displaced_fds_closed,
            "old leader counters not consumed"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.0 == 4 && event.1 == root)
                .count(),
            usize::from(!fail && !matches!(timer_mode, 2 | 4 | 6 | 7)),
            "hardware timer failed to reach surviving task exactly once"
        );
        assert_eq!(
            events.iter().filter(|event| event.0 == 7).count(),
            usize::from(!fail && matches!(timer_mode, 2 | 6 | 7)),
            "real signal callback missed or duplicated"
        );
        if !fail && matches!(timer_mode, 3 | 4 | 6 | 7) {
            let sender = if matches!(timer_mode, 6 | 7) {
                root.as_raw()
            } else {
                std::process::id() as i32
            };
            assert!(
                markers.iter().any(|row| row.0 == root.as_raw()
                    && row.1 == reverie::PERF_EVENT_SIGNAL as i32
                    && row.2 == libc::SI_TKILL
                    && row.3 == sender),
                "missing actual SI_TKILL sender provenance: {markers:?}"
            );
        }
        if !fail && timer_mode == 7 {
            assert!(
                markers
                    .iter()
                    .any(|row| row.2 == libc::SI_TKILL && row.3 == root.as_raw() && row.6),
                "guest signal did not oppose an active artificial request: {markers:?}"
            );
        }
        if !fail && (timer_mode == 0 || timer_mode >= 3) {
            let before_clock: Vec<_> = events.iter().filter(|event| event.0 == 5).collect();
            let after_clock: Vec<_> = events.iter().filter(|event| event.0 == 6).collect();
            assert_eq!(before_clock.len(), 1);
            assert_eq!(after_clock.len(), 1);
            if !matches!(timer_mode, 4 | 6 | 7) {
                let callback_clocks: Vec<_> = events.iter().filter(|event| event.0 == 9).collect();
                assert_eq!(callback_clocks.len(), 1);
                assert!(
                    callback_clocks[0].2 >= before_clock[0].2 + 100_000,
                    "old pending notification triggered the fresh callback prematurely"
                );
            }
            assert!(
                after_clock[0].2 >= before_clock[0].2 + 100_000,
                "payload did not cross requested physical RCB threshold"
            );
        }
        assert_eq!(
            events.iter().filter(|event| event.0 == 8).count(),
            usize::from(matches!(timer_mode, 3..=5)),
            "old request was not made exactly once"
        );
        if timer_mode >= 3 {
            assert!(
                after.cancelled,
                "exec failed to retire previous logical request"
            );
            assert!(
                !after.artificial_pending,
                "exec would resend old artificial request"
            );
            assert_eq!(before.artificial_pending, matches!(timer_mode, 3 | 4));
            assert_eq!(before.artificial_signal_sent, matches!(timer_mode, 3 | 4));
            assert_eq!(
                after.artificial_signal_sent, before.artificial_signal_sent,
                "exec discarded sent-but-unconsumed signal ownership"
            );
        }
        let starts: Vec<_> = events.iter().filter(|event| event.0 == 0).collect();
        assert_eq!(starts.len(), 2, "thread start repeated or omitted");
        let former = starts.iter().find(|event| event.1 != root).unwrap().2;
        let exits: Vec<_> = events.iter().filter(|event| event.0 == 2).collect();
        assert_eq!(
            exits.len(),
            2,
            "every constructed state must be consumed exactly once"
        );
        assert_eq!(
            exits
                .iter()
                .filter(|event| event.2 == root.as_raw() as usize
                    && event.3 == Some(ExitStatus::Exited(0)))
                .count(),
            1
        );
        assert_eq!(exits.iter().filter(|event| event.2 == former).count(), 1);
        assert_eq!(
            events.iter().filter(|event| event.0 == 3).count(),
            1,
            "process hook duplicated at leader displacement"
        );
        assert_eq!(
            events.iter().filter(|event| event.0 == 1).count(),
            usize::from(!fail)
        );
        if fail {
            let failure = completed
                .result
                .expect_err("displaced leader Tool failure lost");
            assert!(
                matches!(failure.primary(), Error::Tool(error) if error.downcast_ref::<NonleaderFailure>().is_some())
            );
            assert_eq!(
                failure.origin().phase,
                "ptrace replaced leader on_exit_thread"
            );
            assert_eq!(
                failure.captured_prefix().unwrap().stdout(),
                b"",
                "replacement guest continued after failure"
            );
            assert_eq!(
                exits.iter().find(|event| event.2 == former).unwrap().3,
                Some(ExitStatus::Signaled(Signal::SIGKILL, false))
            );
        } else {
            let output = completed.result.unwrap();
            assert_eq!(output.status, ExitStatus::Exited(0));
            assert_eq!(output.stdout, b"survived");
            assert_eq!(output.stderr, b"");
            assert_eq!(
                exits.iter().find(|event| event.2 == former).unwrap().3,
                Some(ExitStatus::Exited(0))
            );
        }
        assert_reaped("exec root", root);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_exec_old_artificial_signal_preserves_fresh_timer() {
        ordinary_exec_owner_control(false, 3).await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_exec_old_artificial_signal_without_fresh_request_is_suppressed() {
        ordinary_exec_owner_control(false, 4).await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_exec_real_guest_marker_is_not_swallowed() {
        ordinary_exec_owner_control(false, 6).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_exec_guest_marker_with_active_request_is_not_swallowed() {
        ordinary_exec_owner_control(false, 7).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_exec_old_perf_signal_preserves_fresh_timer() {
        ordinary_exec_owner_control(false, 5).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_nonleader_exec_postexec_timer_survives_internal_step() {
        ordinary_exec_owner_control(false, 1).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_nonleader_exec_real_signal_cancels_postexec_timer() {
        ordinary_exec_owner_control(false, 2).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_nonleader_exec_transfers_state_and_consumes_old_leader_once() {
        ordinary_exec_owner_control(false, 0).await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_nonleader_exec_displaced_hook_failure_fences_replacement() {
        ordinary_exec_owner_control(true, 0).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_nonleader_success_preserves_exit_callbacks() {
        ordinary_nonleader_control(false, false, false).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_nonleader_tool_failure_cancels_pending_newborn_start() {
        ordinary_nonleader_control(true, false, true).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_newborn_setup_refusal_retains_cleanup_ownership() {
        let control = Arc::new(crate::task::FatalSetupControl::default());
        crate::task::FATAL_SETUP_CONTROL.with(|slot| *slot.borrow_mut() = Some(control.clone()));
        let words = FatalWords::new();
        let address = words.0 as usize;
        let tracer = spawn_fn_with_config::<FatalTool, _>(
            move || {
                std::thread::spawn(move || {
                    unsafe { &*(address as *const std::sync::atomic::AtomicUsize) }
                        .store(1, Ordering::SeqCst);
                })
                .join()
                .unwrap();
            },
            0,
            false,
        )
        .await
        .unwrap();
        let root = tracer.guest_pid;
        let log = tracer.gref.0.clone();
        let mut emergency = LiteinstTraceeCleanup::new(
            root,
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(StdMutex::new(None)),
        )
        .unwrap();
        emergency.register_notifier(&Running::new(root));
        let result = tokio::time::timeout(Duration::from_secs(3), tracer.wait()).await;
        crate::task::FATAL_SETUP_CONTROL.with(|slot| *slot.borrow_mut() = None);
        let (child, terminal) = control
            .child
            .lock()
            .unwrap()
            .take()
            .expect("real newborn initial stop reached");
        let root_absent = !std::path::Path::new(&format!("/proc/{root}")).exists();
        let child_absent = !std::path::Path::new(&format!("/proc/{child}")).exists();
        let acknowledged = terminal.wait(Duration::ZERO);
        eprintln!(
            "newborn setup refusal: root={root}, child={child}, root_absent={root_absent}, child_absent={child_absent}, terminal_ack={acknowledged}, resumed={}",
            words.read(0)
        );
        let emergency_result = emergency.terminate_and_confirm();
        eprintln!("newborn setup emergency cleanup: {emergency_result:?}");
        assert!(root_absent && child_absent && acknowledged);
        assert_eq!(words.read(0), 0, "unstarted child reached guest code");
        let error = result
            .expect("setup cleanup exceeded 3s")
            .err()
            .expect("setup refusal lost");
        assert!(
            error.to_string().contains("injected newborn setup refusal"),
            "{error}"
        );
        let events = log.lock().unwrap();
        assert!(
            !events
                .iter()
                .any(|(tid, status)| *tid == child && status.is_none()),
            "unstarted child entered its start callback: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|(tid, status)| *tid == child
                    && *status == Some(ExitStatus::Signaled(Signal::SIGKILL, false)))
                .count(),
            1,
            "constructed child state must be consumed exactly once after actual death: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|(tid, status)| *tid == root
                    && *status == Some(ExitStatus::Signaled(Signal::SIGKILL, false)))
                .count(),
            1
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_freeze_refusal_retains_owner_then_resumes_same_consumers() {
        freeze_refusal_recovery(false).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn legacy_quarantine_retains_owner_rejects_spawns_and_reopens_only_after_completion() {
        const NAME: &str = "tracer::tests::legacy_quarantine_retains_owner_rejects_spawns_and_reopens_only_after_completion";
        const CHILD: &str = "REVERIE_FATAL_QUARANTINE_CHILD";
        const DEADLINE: &str = "REVERIE_FATAL_QUARANTINE_DEADLINE_NS";
        if std::env::var(CHILD).as_deref() == Ok(NAME) {
            freeze_refusal_recovery(true).await;
            return;
        }
        let deadline = fatal_monotonic_ns() + 3_000_000_000;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD, NAME)
            .env(DEADLINE, deadline.to_string())
            .spawn()
            .expect("spawn isolated process-global admission fixture");
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                eprintln!(
                    "isolated quarantine exact child: status={status}, remaining_ns={}",
                    deadline.saturating_sub(fatal_monotonic_ns())
                );
                assert!(status.success());
                assert!(
                    fatal_monotonic_ns() <= deadline,
                    "re-exec exceeded original pre-start deadline"
                );
                return;
            }
            if fatal_monotonic_ns() >= deadline {
                eprintln!("isolated quarantine original pre-start deadline FAILED");
                // This is still our unreaped direct child; no reused-PID lookup.
                let signal = child.kill();
                let rescue_deadline = Instant::now() + Duration::from_secs(2);
                let rescue = loop {
                    let status = child.try_wait().unwrap();
                    if status.is_some() || Instant::now() >= rescue_deadline {
                        break status;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                };
                panic!(
                    "quarantine child timeout; rescue only: signal={signal:?} status={rescue:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    async fn freeze_refusal_recovery(legacy: bool) {
        let control = Arc::new(crate::task::FatalFreezeControl::default());
        crate::task::FATAL_FREEZE_CONTROL.with(|slot| *slot.borrow_mut() = Some(control.clone()));
        let words = FatalWords::new();
        let address = words.0 as usize;
        let duration = if legacy {
            let deadline = std::env::var("REVERIE_FATAL_QUARANTINE_DEADLINE_NS")
                .expect("legacy process-global fixture must be isolated")
                .parse::<u64>()
                .unwrap();
            fatal_remaining(deadline)
        } else {
            Duration::from_secs(3)
        };
        let deadline = Instant::now() + duration;
        let tracer = spawn_fn_with_config::<FatalTool, _>(
            move || {
                std::thread::spawn(move || {
                    unsafe { libc::syscall(libc::SYS_getpgid, 0) };
                    unsafe { &*(address as *const std::sync::atomic::AtomicUsize) }
                        .store(1, Ordering::SeqCst);
                })
                .join()
                .unwrap();
            },
            1,
            false,
        )
        .await
        .unwrap();
        let root = tracer.guest_pid;
        let log = tracer.gref.0.clone();
        let termination = tracer.termination_handle().unwrap();
        let pending = if legacy {
            let error = tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                tracer.wait(),
            )
            .await
            .expect("legacy refusal exceeded shared deadline")
            .err()
            .expect("legacy refusal lost");
            let Error::Tool(error) = error else {
                panic!("typed cleanup diagnostic required")
            };
            let retained = error
                .downcast_ref::<CleanupUnconfirmed>()
                .expect("public pending type");
            assert_eq!(retained.origin().phase, "ptrace syscall callback");
            assert!(retained.recovery_key() > 0);
            assert!(matches!(
                retained.take_cleanup::<(), ExitStatus>(),
                Err(CleanupLookupError::WrongType)
            ));
            std::thread::scope(|scope| {
                scope.spawn(|| {
                assert!(matches!(retained.take_cleanup::<FatalLog, ExitStatus>(), Err(CleanupLookupError::WrongThread)));
                assert!(matches!(OrdinaryAdmission::acquire(), Err(Error::Tool(error)) if error.downcast_ref::<CleanupAdmissionRefused>().is_some()));
            }).join().unwrap()
            });
            let refused = spawn_fn::<(), _>(|| panic!("refused spawn executed guest code")).await;
            assert!(
                matches!(refused, Err(Error::Tool(error)) if error.downcast_ref::<CleanupAdmissionRefused>().is_some())
            );
            let owner = retained
                .take_cleanup::<FatalLog, ExitStatus>()
                .expect("same owner recovery");
            assert!(matches!(
                retained.take_cleanup::<FatalLog, ExitStatus>(),
                Err(CleanupLookupError::AlreadyTaken)
            ));
            assert!(
                OrdinaryAdmission::acquire().is_err(),
                "lookup alone cannot reopen admission"
            );
            owner
        } else {
            let outcome = tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                tracer.wait_completion(),
            )
            .await
            .expect("refusal did not yield its owner within the single deadline");
            match outcome {
                ToolRunOutcome::CleanupPending(pending) => pending,
                ToolRunOutcome::Complete(_) => panic!("refused cleanup was falsely completed"),
                ToolRunOutcome::UnsupportedBackend(_) => {
                    panic!("ordinary failure misclassified as unsupported")
                }
            }
        };
        crate::task::FATAL_FREEZE_CONTROL.with(|slot| *slot.borrow_mut() = None);
        assert!(
            matches!(pending.failure().primary(), Error::Tool(error) if error.downcast_ref::<NonleaderFailure>().is_some())
        );
        assert!(termination.terminate(Error::Tool(anyhow::Error::new(TestDeadline))));
        let session = control.session.lock().unwrap().take().unwrap();
        assert!(session.cleanup_was_refused());
        assert_eq!(session.unconfirmed_task_count(), 2);
        assert_eq!(
            session.retained_group_counts_for_test(),
            (2, 0, 0),
            "Pending lost active generation group authority"
        );
        assert_eq!(words.read(0), 0, "failed thread resumed");
        assert!(std::path::Path::new(&format!("/proc/{root}")).exists());
        {
            let events = log.lock().unwrap();
            assert_eq!(
                events.len(),
                2,
                "unconfirmed task acquired a fabricated consuming hook"
            );
            assert!(events.iter().all(|(_, status)| status.is_none()));
        }
        let completed = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            pending.resume_cleanup(),
        )
        .await
        .expect("same owner did not complete within the original single deadline");
        let ToolRunOutcome::Complete(completed) = completed else {
            panic!("transient refusal did not resume");
        };
        let error = completed.result.expect_err("primary Tool failure was lost");
        assert!(
            matches!(error.primary(), Error::Tool(error) if error.downcast_ref::<NonleaderFailure>().is_some())
        );
        assert!(error.secondary().iter().any(|item| matches!(item.error(), Error::Tool(error) if error.downcast_ref::<TestDeadline>().is_some())), "later supervisor cause lost");
        assert!(!termination.terminate(Error::Tool(anyhow::Error::new(TestDeadline))));
        assert_eq!(session.unconfirmed_task_count(), 0);
        assert_eq!(
            session.retained_group_counts_for_test(),
            (0, 0, 0),
            "completed owners retained group descriptors"
        );
        assert_eq!(
            log.lock()
                .unwrap()
                .iter()
                .filter(|(_, status)| *status == Some(ExitStatus::Signaled(Signal::SIGKILL, false)))
                .count(),
            2
        );
        assert_reaped("resumed refusal root", root);
        if legacy {
            assert!(
                OrdinaryAdmission::acquire().is_ok(),
                "actual completion restores admission"
            );
            let tracer = spawn_fn::<(), _>(|| {})
                .await
                .expect("later spawn admitted");
            let (status, ()) = tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                tracer.wait(),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(status, ExitStatus::Exited(0));
        }
    }

    fn fatal_control_write<const N: usize>(
        stream: &mut std::os::unix::net::UnixStream,
        words: [u64; N],
    ) {
        for word in words {
            stream.write_all(&word.to_ne_bytes()).unwrap();
        }
    }

    fn fatal_control_read<const N: usize>(stream: &mut std::os::unix::net::UnixStream) -> [u64; N] {
        use std::io::Read;
        std::array::from_fn(|_| {
            let mut bytes = [0; 8];
            stream.read_exact(&mut bytes).unwrap();
            u64::from_ne_bytes(bytes)
        })
    }

    fn fatal_monotonic_ns() -> u64 {
        let mut now: libc::timespec = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) },
            0
        );
        u64::try_from(now.tv_sec).unwrap() * 1_000_000_000 + u64::try_from(now.tv_nsec).unwrap()
    }

    fn fatal_remaining(deadline: u64) -> Duration {
        let remaining = deadline
            .checked_sub(fatal_monotonic_ns())
            .expect("combined fatal cleanup exceeded its one 3s deadline");
        assert_ne!(remaining, 0);
        Duration::from_nanos(remaining)
    }

    fn fatal_subreaper_state() -> libc::c_int {
        let mut state = 0;
        assert_eq!(
            unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut state, 0, 0, 0) },
            0
        );
        state
    }

    fn fatal_child_command(test: &str, role: &str) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env("REVERIE_FATAL_REAP_TEST", test)
            .env("REVERIE_FATAL_REAP_ROLE", role);
        command
    }

    fn fatal_natural_reaper(test: &str, opponent: bool) {
        use std::os::unix::net::UnixStream;
        assert_eq!(
            fatal_subreaper_state(),
            1,
            "isolated natural reaper was not established"
        );
        let (mut channel, child_channel) = UnixStream::pair().unwrap();
        let handed_deadline = std::env::var("REVERIE_FATAL_HANDED_DEADLINE_NS")
            .ok()
            .map(|value| value.parse::<u64>().unwrap());
        if let Some(deadline) = handed_deadline {
            channel
                .set_read_timeout(Some(fatal_remaining(deadline)))
                .unwrap();
            channel
                .set_write_timeout(Some(fatal_remaining(deadline)))
                .unwrap();
        }
        let mut tracer = fatal_child_command(test, "tracer")
            .stdin(std::process::Stdio::from(OwnedFd::from(child_channel)))
            .spawn()
            .unwrap();
        let [root, root_start, root_inode] = fatal_control_read::<3>(&mut channel);
        let root = Pid::from_raw(i32::try_from(root).unwrap());
        assert_eq!(tracee_snapshot(root).unwrap().start_time, root_start);
        assert_eq!(
            fs::metadata(format!("/proc/{root}")).unwrap().ino(),
            root_inode
        );
        let deadline = handed_deadline.unwrap_or_else(|| fatal_monotonic_ns() + 3_000_000_000);
        channel
            .set_read_timeout(Some(fatal_remaining(deadline)))
            .unwrap();
        channel
            .set_write_timeout(Some(fatal_remaining(deadline)))
            .unwrap();
        fatal_control_write(&mut channel, [deadline]);
        let observation = fatal_control_read::<9>(&mut channel);
        let child = Pid::from_raw(i32::try_from(observation[0]).unwrap());
        let identity = untraced_process_identity(child);
        assert_eq!(identity.snapshot.start_time, observation[1]);
        assert_eq!(identity.proc_inode, observation[2]);
        assert_eq!(
            identity.snapshot.ppid.as_raw(),
            unsafe { libc::getpid() },
            "child was not adopted by this natural reaper"
        );
        let status = fs::read_to_string(format!("/proc/{child}/status")).unwrap();
        let zombie = status.lines().any(|line| line.starts_with("State:\tZ"));
        let ready = observation[3] == 1
            && observation[4] == 1
            && observation[5] == 1
            && observation[6] == 0
            && observation[7] == 1
            && zombie
            && identity.snapshot.tracer_pid.as_raw() == 0
            && !std::path::Path::new(&format!("/proc/{root}")).exists();
        eprintln!(
            "natural-reaper product predicate: opponent={opponent}, ready={ready}, root={root}, child={child}, start={}, inode={}, backend_sigkill={}, terminal_ack={}, original_error={}, post_syscall={}, callbacks_valid={}, zombie={zombie}, tracer={}",
            observation[1],
            observation[2],
            observation[3],
            observation[4],
            observation[5],
            observation[6],
            observation[7],
            identity.snapshot.tracer_pid
        );
        assert_eq!(
            ready, !opponent,
            "same complete predicate accepted a live stop or refused actual terminal completion"
        );
        if opponent {
            assert!(status.lines().any(|line| line.starts_with("State:\tt")));
            assert_eq!(identity.snapshot.tracer_pid.as_raw() as u64, observation[8]);
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe {
                    libc::waitid(
                        libc::P_PIDFD,
                        identity.pidfd.as_ref().unwrap().as_raw_fd() as u32,
                        &mut info,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    )
                },
                0
            );
            assert_eq!(
                unsafe { info.si_pid() },
                0,
                "live-stop opponent unexpectedly had natural terminal status"
            );
            // This verdict is sealed before teardown. The natural reaper never
            // signals/resumes/detaches any tracee, even for the negative case.
            fatal_control_write(&mut channel, [0]);
            assert_eq!(fatal_control_read::<1>(&mut channel), [1]);
        }
        let _remaining = fatal_remaining(deadline);
        let after = tracee_snapshot(child).unwrap();
        assert_eq!(after.start_time, identity.snapshot.start_time);
        assert_eq!(after.tracer_pid.as_raw(), 0);
        let mut held: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let fd = identity.pidfd.as_ref().unwrap().as_raw_fd() as u32;
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    fd,
                    &mut held,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0
        );
        assert_eq!(unsafe { held.si_pid() }, child.as_raw());
        assert_eq!(held.si_code, libc::CLD_KILLED);
        assert_eq!(unsafe { held.si_status() }, libc::SIGKILL);
        assert!(
            std::path::Path::new(&format!("/proc/{child}")).exists(),
            "WNOWAIT was silently treated as reap"
        );
        eprintln!(
            "owned held zombie: child={child}, status=SIGKILL, WNOWAIT=true, proc_present=true"
        );
        let mut reaped: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    fd,
                    &mut reaped,
                    libc::WEXITED | libc::WNOHANG,
                )
            },
            0
        );
        assert_eq!(unsafe { reaped.si_pid() }, child.as_raw());
        assert_eq!(reaped.si_code, libc::CLD_KILLED);
        assert_eq!(unsafe { reaped.si_status() }, libc::SIGKILL);
        assert!(!std::path::Path::new(&format!("/proc/{root}")).exists());
        assert!(!std::path::Path::new(&format!("/proc/{child}")).exists());
        let remaining = fatal_remaining(deadline);
        eprintln!(
            "actual natural wait: child={child}, status=SIGKILL, root_absent=true, child_absent=true, remaining_ns={}",
            remaining.as_nanos()
        );
        fatal_control_write(&mut channel, [1]);
        assert!(
            tracer.wait().unwrap().success(),
            "isolated tracer assertions failed"
        );
        assert_eq!(
            unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            Errno::last(),
            Errno::ECHILD,
            "isolated natural reaper retained an extra owned child"
        );
    }

    async fn fatal_unhanded_tracer(opponent: bool, vfork: bool) {
        use std::os::unix::net::UnixStream;
        assert_eq!(
            fatal_subreaper_state(),
            0,
            "tracer must be distinct from natural reaper"
        );
        let mut channel = unsafe { UnixStream::from_raw_fd(libc::STDIN_FILENO) };
        let pause = Arc::new(crate::task::FatalForkPause::default());
        pause.live_stop_opponent.store(opponent, Ordering::SeqCst);
        let words = FatalWords::new();
        let address = words.0 as usize;
        pause.waiting_word.store(address, Ordering::SeqCst);
        crate::task::FATAL_FORK_PAUSE.with(|slot| *slot.borrow_mut() = Some(pause.clone()));
        let sentinel = fork_paused_child();
        let sentinel_identity = untraced_process_identity(sentinel);
        let tracer = spawn_fn_with_config::<FatalTool, _>(
            move || {
                let thread = std::thread::spawn(move || {
                    unsafe { libc::syscall(libc::SYS_getpgid, 0) };
                    unsafe { &*((address as *const std::sync::atomic::AtomicUsize).add(1)) }
                        .store(1, Ordering::SeqCst);
                });
                while unsafe { &*(address as *const std::sync::atomic::AtomicUsize) }
                    .load(Ordering::SeqCst)
                    == 0
                {
                    std::thread::yield_now();
                }
                if vfork {
                    extern "C" fn child_body(address: *mut libc::c_void) -> libc::c_int {
                        unsafe { &*((address as *const std::sync::atomic::AtomicUsize).add(2)) }
                            .store(1, Ordering::SeqCst);
                        7
                    }
                    let mut stack = vec![0u8; 16 * 1024];
                    let top = (unsafe { stack.as_mut_ptr().add(stack.len()) } as usize & !15usize)
                        as *mut libc::c_void;
                    let child = unsafe {
                        libc::clone(
                            child_body,
                            top,
                            libc::CLONE_VM | libc::CLONE_VFORK | libc::SIGCHLD,
                            address as *mut libc::c_void,
                        )
                    };
                    assert!(child > 0);
                    thread.join().unwrap();
                } else {
                    match unsafe { unistd::fork() }.unwrap() {
                        ForkResult::Child => panic!("unhanded newborn reached guest code"),
                        ForkResult::Parent { .. } => {
                            thread.join().unwrap();
                        }
                    }
                }
            },
            3,
            false,
        )
        .await
        .unwrap();
        let root = tracer.guest_pid;
        let log = tracer.gref.0.clone();
        let session = tracer.ordinary_session.clone();
        let root_start = tracee_snapshot(root).unwrap().start_time;
        let root_inode = fs::metadata(format!("/proc/{root}")).unwrap().ino();
        fatal_control_write(&mut channel, [root.as_raw() as u64, root_start, root_inode]);
        let [deadline] = fatal_control_read::<1>(&mut channel);
        channel
            .set_read_timeout(Some(fatal_remaining(deadline)))
            .unwrap();
        let result = tokio::time::timeout(fatal_remaining(deadline), tracer.wait()).await;
        let child = pause
            .child
            .lock()
            .unwrap()
            .take()
            .expect("actual fork event was reached");
        let child_pid = child.pid();
        let edges = session.observed_child_ops.lock().unwrap().clone();
        eprintln!(
            "unhanded real child edges: {edges:?}; child_body={}",
            words.read(2)
        );
        assert!(edges.contains(&(
            root,
            if vfork { ChildOp::Vfork } else { ChildOp::Fork },
            child_pid
        )));
        assert_eq!(words.read(2), 0, "unhanded child executed its guest body");
        let (start, inode) = pause.generation.lock().unwrap().unwrap();
        let terminal = child.terminal_cleanup();
        let acknowledged = terminal.wait(Duration::ZERO);
        let group_retention = session.retained_group_counts_for_test();
        eprintln!(
            "unhanded original generation resource retention: groups/vfork/unconfirmed={group_retention:?}, opponent={opponent}"
        );
        assert_eq!(
            group_retention,
            if opponent { (1, 0, 1) } else { (0, 0, 0) },
            "release only the confirmed original newborn generation"
        );
        let original_error = matches!(&result, Ok(Err(Error::Tool(inner))) if inner.downcast_ref::<NonleaderFailure>().is_some());
        let backend_sigkill = *pause.terminal_status.lock().unwrap()
            == Some((child_pid, ExitStatus::Signaled(Signal::SIGKILL, false)));
        let events = log.lock().unwrap().clone();
        let callbacks_valid = events.iter().filter(|(_, status)| status.is_none()).count() == 2
            && events
                .iter()
                .filter(|(_, status)| *status == Some(ExitStatus::Signaled(Signal::SIGKILL, false)))
                .count()
                == 2
            && !events.iter().any(|(pid, _)| *pid == child_pid);
        assert!(sentinel_identity.same_process());
        assert_eq!(
            unsafe { libc::waitpid(sentinel.as_raw(), std::ptr::null_mut(), libc::WNOHANG) },
            0,
            "cleanup touched unrelated sentinel"
        );
        sentinel_identity.send_signal(Signal::SIGKILL).unwrap();
        assert_eq!(
            unsafe { libc::waitpid(sentinel.as_raw(), std::ptr::null_mut(), 0) },
            sentinel.as_raw()
        );
        fatal_control_write(
            &mut channel,
            [
                child_pid.as_raw() as u64,
                start,
                inode,
                u64::from(backend_sigkill),
                u64::from(acknowledged),
                u64::from(original_error),
                words.read(1) as u64,
                u64::from(callbacks_valid),
                unsafe { libc::syscall(libc::SYS_gettid) } as u64,
            ],
        );
        let [verdict] = fatal_control_read::<1>(&mut channel);
        if opponent {
            assert_eq!(
                verdict, 0,
                "real live-stop opponent was not rejected before cleanup"
            );
            pause.live_stop_opponent.store(false, Ordering::SeqCst);
            // Only the tracer performs negative-control teardown, after the
            // separate reaper sealed the failed product predicate. This cannot
            // convert that predicate into a successful cleanup observation.
            let rescue = FatalNewborn::new(root, &child);
            rescue.signal().unwrap();
            tokio::time::timeout(fatal_remaining(deadline), rescue.reap())
                .await
                .unwrap()
                .unwrap();
            eprintln!(
                "live-stop opponent teardown completed after rejection; not product completion"
            );
            fatal_control_write(&mut channel, [1]);
            assert_eq!(fatal_control_read::<1>(&mut channel), [1]);
        } else {
            assert_eq!(verdict, 1);
        }
        crate::task::FATAL_FORK_PAUSE.with(|slot| *slot.borrow_mut() = None);
        assert!(original_error && callbacks_valid);
        assert_eq!(words.read(1), 0);
        assert!(!std::path::Path::new(&format!("/proc/{root}")).exists());
        assert!(!std::path::Path::new(&format!("/proc/{child_pid}")).exists());
        let _remaining = fatal_remaining(deadline);
    }

    async fn fatal_unhanded_control(test: &str, opponent: bool) {
        use std::os::unix::process::CommandExt;
        if std::env::var("REVERIE_FATAL_REAP_TEST").as_deref() == Ok(test) {
            assert!(std::env::args().any(|arg| arg == test));
            assert!(std::env::args().any(|arg| arg == "--exact"));
            match std::env::var("REVERIE_FATAL_REAP_ROLE").as_deref() {
                Ok("reaper") => fatal_natural_reaper(test, opponent),
                Ok("tracer") => {
                    fatal_unhanded_tracer(opponent, test.ends_with("vfork_child")).await
                }
                other => panic!("invalid isolated test role: {other:?}"),
            }
            return;
        }
        let mut reaper = fatal_child_command(test, "reaper");
        // The only post-fork operation changes this isolated helper's flag;
        // no global subreaper setting is installed in the test suite or library.
        unsafe {
            reaper.pre_exec(|| {
                if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        assert!(
            reaper.status().unwrap().success(),
            "owned natural-reaper control failed"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_nonleader_tool_failure_reaps_unhanded_fork_child() {
        fatal_unhanded_control(
            "tracer::tests::ordinary_nonleader_tool_failure_reaps_unhanded_fork_child",
            false,
        )
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_nonleader_tool_failure_reaps_unhanded_vfork_child() {
        fatal_unhanded_control(
            "tracer::tests::ordinary_nonleader_tool_failure_reaps_unhanded_vfork_child",
            false,
        )
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordinary_unhanded_reaper_rejects_real_live_stop_opponent() {
        fatal_unhanded_control(
            "tracer::tests::ordinary_unhanded_reaper_rejects_real_live_stop_opponent",
            true,
        )
        .await;
    }

    #[derive(Default)]
    struct CommandBootstrapTool;

    #[reverie::tool]
    impl Tool for CommandBootstrapTool {
        type GlobalState = ();
        type ThreadState = (usize, usize);

        fn subscriptions(_config: &()) -> Subscription {
            [Sysno::execve].into_iter().collect()
        }

        async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
            assert!(guest.is_command_bootstrap());
            Ok(())
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            assert_eq!(syscall.number(), Sysno::execve);
            assert_eq!(guest.is_command_bootstrap(), guest.thread_state().0 == 0);
            guest.thread_state_mut().0 += 1;
            guest.tail_inject(syscall).await
        }

        async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
            assert!(!guest.is_command_bootstrap());
            guest.thread_state_mut().1 += 1;
            Ok(())
        }

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            _tid: reverie::Tid,
            _global: &G,
            state: Self::ThreadState,
            status: ExitStatus,
        ) -> Result<(), Error> {
            assert_eq!(state, (2, 2), "both initial and guest exec must complete");
            assert_eq!(status, ExitStatus::Exited(0));
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn command_bootstrap_ends_before_post_exec_and_later_guest_exec() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exec /bin/true"]);
        let tracer = TracerBuilder::<CommandBootstrapTool>::new(command)
            .spawn()
            .await
            .expect("spawn two-exec command");
        let (status, ()) = tokio::time::timeout(Duration::from_secs(5), tracer.wait())
            .await
            .expect("two-exec command hung")
            .expect("two-exec tracing failed");
        assert_eq!(status, ExitStatus::Exited(0));
    }

    #[derive(Default)]
    struct FunctionBootstrapTool;

    #[reverie::tool]
    impl Tool for FunctionBootstrapTool {
        type GlobalState = ();
        type ThreadState = usize;

        fn subscriptions(_config: &()) -> Subscription {
            [Sysno::getpid].into_iter().collect()
        }

        async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
            assert!(!guest.is_command_bootstrap());
            Ok(())
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            assert!(!guest.is_command_bootstrap());
            assert_eq!(syscall.number(), Sysno::getpid);
            *guest.thread_state_mut() += 1;
            Ok(guest.inject(syscall).await?)
        }

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            _tid: reverie::Tid,
            _global: &G,
            state: Self::ThreadState,
            status: ExitStatus,
        ) -> Result<(), Error> {
            assert_eq!(state, 1, "function guest must reach its syscall");
            assert_eq!(status, ExitStatus::Exited(0));
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_fn_never_has_command_bootstrap_provenance() {
        let tracer = spawn_fn::<FunctionBootstrapTool, _>(|| {
            assert!(unsafe { libc::syscall(libc::SYS_getpid) } > 0);
        })
        .await
        .expect("spawn function provenance control");
        let (status, ()) = tokio::time::timeout(Duration::from_secs(5), tracer.wait())
            .await
            .expect("function control hung")
            .expect("function tracing failed");
        assert_eq!(status, ExitStatus::Exited(0));
    }

    use reverie::Guest;
    use reverie::syscalls::Syscall;
    use reverie::syscalls::SyscallInfo;

    use super::*;
    use crate::error::LiteinstActivationFailureCategory;
    use crate::error::LiteinstActivationFailureReason;
    use crate::error::LiteinstActivationOperation;
    use crate::error::LiteinstActivationStage;
    use crate::error::liteinst_activation_failure_category;
    use crate::error::liteinst_activation_failure_reason;

    fn assert_liteinst_activation_failure(
        error: &Error,
        expected: LiteinstActivationFailureReason,
    ) {
        assert_eq!(
            liteinst_activation_failure_reason(error),
            Some(expected),
            "{error}"
        );
    }

    fn assert_general_pre_ready_liteinst_activation_failure(error: &Error) {
        assert_eq!(
            liteinst_activation_failure_category(error),
            Some(LiteinstActivationFailureCategory::General(
                LiteinstActivationStage::PreReady,
            )),
            "{error}"
        );
    }

    fn fork_paused_child() -> Pid {
        match unsafe { unistd::fork() }.expect("fork test child") {
            ForkResult::Child => loop {
                unsafe { libc::pause() };
            },
            ForkResult::Parent { child } => Pid::from(child),
        }
    }

    fn fork_paused_grandchild() -> (Pid, Pid, std::os::unix::net::UnixStream) {
        let (mut control, mut child_control) =
            std::os::unix::net::UnixStream::pair().expect("create parent control socket");
        match unsafe { unistd::fork() }.expect("fork recorded parent") {
            ForkResult::Child => {
                drop(control);
                match unsafe { unistd::fork() }.expect("fork retained descendant") {
                    ForkResult::Child => loop {
                        unsafe { libc::pause() };
                    },
                    ForkResult::Parent { child } => {
                        child_control
                            .write_all(&child.as_raw().to_ne_bytes())
                            .expect("publish retained descendant pid");
                        let mut release = [0];
                        std::io::Read::read_exact(&mut child_control, &mut release)
                            .expect("wait for recorded-parent release");
                        unsafe { libc::_exit(0) };
                    }
                }
            }
            ForkResult::Parent { child } => {
                drop(child_control);
                let mut raw_pid = [0; std::mem::size_of::<i32>()];
                std::io::Read::read_exact(&mut control, &mut raw_pid)
                    .expect("read retained descendant pid");
                (
                    Pid::from(child),
                    Pid::from_raw(i32::from_ne_bytes(raw_pid)),
                    control,
                )
            }
        }
    }

    fn untraced_process_identity(pid: Pid) -> TraceeIdentity {
        let snapshot = tracee_snapshot(pid).expect("read child identity");
        let proc_dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(format!("/proc/{pid}"))
            .expect("open child proc identity");
        let proc_inode = proc_dir.metadata().expect("stat child proc identity").ino();
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.as_raw(), 0) };
        assert_ne!(fd, -1, "open child pidfd: {}", Errno::last());
        TraceeIdentity {
            tid: pid,
            snapshot,
            proc_dir: proc_dir.into(),
            proc_inode,
            pidfd: Some(unsafe { OwnedFd::from_raw_fd(fd as i32) }),
            parent: None,
        }
    }

    fn assert_reaped(role: &str, pid: Pid) {
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "{role} tracee {pid} remains in procfs"
        );
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid.as_raw(), &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(Errno::last(), Errno::ECHILD);
    }

    fn assert_eventually_reaped(role: &str, pid: Pid) {
        for _ in 0..2_000 {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_reaped(role, pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normal_preinit_resume_clears_held_root_stop_lease() {
        let pid = match unsafe { unistd::fork() }.expect("fork held-stop resume child") {
            ForkResult::Child => {
                safeptrace::traceme_and_stop().expect("TRACEME held-stop resume child");
                unsafe { libc::_exit(0) };
            }
            ForkResult::Parent { child } => Pid::from(child),
        };
        let (stopped, event) = Running::new(pid)
            .wait()
            .expect("wait held-stop resume child")
            .assume_stopped();
        assert_eq!(event, Event::Signal(Signal::SIGSTOP));

        let slot = Arc::new(StdMutex::new(Some(HeldRootStop::from_event(
            &stopped,
            &Event::Signal(Signal::SIGSTOP),
        ))));
        let running = RootStopLease::new(stopped, Some(Arc::clone(&slot)))
            .resume(None)
            .expect("resume held-stop child");
        assert!(
            slot.lock().unwrap().is_none(),
            "normal transition left a stale lease"
        );

        let exited = running
            .next_state()
            .await
            .expect("wait resumed held-stop child");
        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
    }

    fn spawn_held_stop_child(role: &str) -> (Pid, Stopped) {
        let pid = match unsafe { unistd::fork() }
            .unwrap_or_else(|error| panic!("fork {role}: {error}"))
        {
            ForkResult::Child => {
                safeptrace::traceme_and_stop()
                    .unwrap_or_else(|error| panic!("TRACEME {role}: {error}"));
                unsafe { libc::_exit(0) };
            }
            ForkResult::Parent { child } => Pid::from(child),
        };
        let (stopped, event) = Running::new(pid)
            .wait()
            .unwrap_or_else(|error| panic!("wait {role}: {error}"))
            .assume_stopped();
        assert_eq!(event, Event::Signal(Signal::SIGSTOP));
        (pid, stopped)
    }

    async fn resume_held_stop_child(role: &str, stopped: Stopped) {
        let wait = stopped
            .resume(None)
            .unwrap_or_else(|error| panic!("resume {role}: {error}"))
            .next_state()
            .await
            .unwrap_or_else(|error| panic!("wait resumed {role}: {error}"));
        assert_eq!(wait.assume_exited().1, ExitStatus::Exited(0));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exit_stop_atomically_supersedes_same_generation_lease() {
        let (_pid, stopped) = spawn_held_stop_child("exit supersession child");
        let generation = stopped.terminal_cleanup();
        let slot = Arc::new(StdMutex::new(Some(HeldRootStop::from_event(
            &stopped,
            &Event::Signal(Signal::SIGSTOP),
        ))));

        HeldRootStop::supersede_with_exit(&slot, &stopped)
            .expect("same-generation exit stop must supersede existing lease");
        {
            let held = slot.lock().unwrap();
            let held = held.as_ref().expect("exit supersession cleared the lease");
            assert!(held.armed);
            assert!(held.terminal.same_generation(&generation));
            assert!(matches!(held.status, HeldRootStopStatus::Exit));
        }

        slot.lock().unwrap().take();
        resume_held_stop_child("exit supersession child", stopped).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_exit_path_supersedes_preempted_same_generation_lease() {
        let (_pid, stopped) = spawn_held_stop_child("async exit supersession child");
        let slot = Arc::new(StdMutex::new(Some(HeldRootStop::from_event(
            &stopped,
            &Event::Signal(Signal::SIGSTOP),
        ))));

        let status =
            TracedTask::<InitFailureTool>::handle_exit_event(stopped, Some(Arc::clone(&slot)))
                .await
                .expect("async exit path rejected same-generation lease supersession");
        assert_eq!(status, ExitStatus::Exited(0));
        assert!(
            slot.lock().unwrap().is_none(),
            "async exit path left its superseded lease armed"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exit_stop_rejects_mismatched_generation_without_replacement() {
        for _ in 0..16 {
            let (_first_pid, first) = spawn_held_stop_child("first generation child");
            let (_second_pid, second) = spawn_held_stop_child("second generation child");
            let first_generation = first.terminal_cleanup();
            let slot = Arc::new(StdMutex::new(Some(HeldRootStop::from_event(
                &first,
                &Event::Signal(Signal::SIGSTOP),
            ))));

            assert_eq!(
                HeldRootStop::supersede_with_exit(&slot, &second),
                Err(TraceError::Errno(Errno::EINVAL))
            );
            {
                let held = slot.lock().unwrap();
                let held = held.as_ref().expect("mismatch removed the original lease");
                assert!(held.terminal.same_generation(&first_generation));
                assert!(matches!(
                    held.status,
                    HeldRootStopStatus::Signal(Signal::SIGSTOP)
                ));
            }

            slot.lock().unwrap().take();
            resume_held_stop_child("first generation child", first).await;
            resume_held_stop_child("second generation child", second).await;
        }
    }

    #[derive(Default)]
    struct InitFailureTool;

    #[reverie::tool]
    impl Tool for InitFailureTool {
        type GlobalState = ();
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            Subscription::none()
        }
    }

    #[test]
    fn liteinst_stats_collector_is_allocated_only_when_requested() {
        let disabled = TracerBuilder::<InitFailureTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4);
        assert!(
            disabled
                .liteinst_runtime
                .as_ref()
                .unwrap()
                .instrumentation_stats
                .is_none()
        );

        let enabled = TracerBuilder::<InitFailureTool>::new(Command::new("/bin/true"))
            .liteinst_runtime_with_stats(
                PathBuf::from("/not/used.so"),
                1,
                2,
                3,
                4,
                BackendStatsRequest::ENABLED,
            );
        assert!(
            enabled
                .liteinst_runtime
                .as_ref()
                .unwrap()
                .instrumentation_stats
                .is_some()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn liteinst_runtime_rejects_gdbserver_before_spawning_tracee() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock predates Unix epoch")
            .as_nanos();
        let side_effect = std::env::temp_dir().join(format!(
            "reverie-liteinst-gdb-rejected-{}-{nonce}",
            std::process::id()
        ));
        let socket = side_effect.with_extension("sock");
        assert!(!side_effect.exists());
        assert!(!socket.exists());
        let mut command = Command::new("/usr/bin/touch");
        command.arg(&side_effect);

        let error = match TracerBuilder::<InitFailureTool>::new(command)
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .gdbserver(socket.clone())
            .spawn()
            .await
        {
            Ok(_) => panic!("LiteInst plus GDB unexpectedly spawned a tracee"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("ENOTSUPP"), "{error}");
        assert!(
            error
                .to_string()
                .contains("executable-entry software breakpoint"),
            "{error}"
        );
        assert!(
            !side_effect.exists(),
            "rejected configuration ran the tracee"
        );
        assert!(
            !socket.exists(),
            "rejected configuration opened a GDB server"
        );
    }

    #[derive(Default)]
    struct RootStopTool;

    #[reverie::tool]
    impl Tool for RootStopTool {
        type GlobalState = ();
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            [Sysno::getpid].into_iter().collect()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            Ok(guest.inject(syscall).await?)
        }
    }

    #[derive(Default)]
    struct AllSyscallsTool;

    #[reverie::tool]
    impl Tool for AllSyscallsTool {
        type GlobalState = ();
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            Subscription::all_syscalls()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            Ok(guest.inject(syscall).await?)
        }
    }

    #[derive(Default)]
    struct SubscribedRestartSyscallTool;

    #[reverie::tool]
    impl Tool for SubscribedRestartSyscallTool {
        type GlobalState = ();
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            [Sysno::restart_syscall].into_iter().collect()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            _guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            assert_eq!(syscall.number(), Sysno::restart_syscall);
            Ok(0x5a)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn subscribed_restart_syscall_reaches_the_tool() {
        let tracer = spawn_fn::<SubscribedRestartSyscallTool, _>(|| {
            let result = unsafe { libc::syscall(libc::SYS_restart_syscall) };
            assert_eq!(result, 0x5a, "subscribed restart_syscall bypassed the Tool");
        })
        .await
        .expect("spawn subscribed restart_syscall guest");

        let (status, _) = tokio::time::timeout(Duration::from_secs(3), tracer.wait())
            .await
            .expect("subscribed restart_syscall guest hung")
            .expect("subscribed restart_syscall guest failed");
        assert_eq!(status, ExitStatus::Exited(0));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unsubscribed_restart_syscall_retains_the_linux_result() {
        let tracer = spawn_fn::<InitFailureTool, _>(|| {
            let result = unsafe { libc::syscall(libc::SYS_restart_syscall) };
            assert_eq!(result, -1, "unsubscribed restart_syscall was intercepted");
            assert_eq!(Errno::last(), Errno::EINTR);
        })
        .await
        .expect("spawn unsubscribed restart_syscall guest");

        let (status, _) = tokio::time::timeout(Duration::from_secs(3), tracer.wait())
            .await
            .expect("unsubscribed restart_syscall guest hung")
            .expect("unsubscribed restart_syscall guest failed");
        assert_eq!(status, ExitStatus::Exited(0));
    }

    #[derive(Default)]
    struct TimedExecTransitionTool;

    #[reverie::tool]
    impl Tool for TimedExecTransitionTool {
        type GlobalState = ();
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            Subscription::all()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            guest.set_timer_precise(reverie::TimerSchedule::Rcbs(20_000_000))?;
            Ok(guest.inject(syscall).await?)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_ready_exec_skip_accepts_exact_kernel_breakpoint_transition() {
        let tracer = TracerBuilder::<TimedExecTransitionTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .spawn()
            .await
            .expect("spawn timed exec-transition activation tracee");
        let root_pid = tracer.guest_pid();
        let error = tokio::time::timeout(Duration::from_secs(3), tracer.wait())
            .await
            .expect("timed exec-transition activation tracee hung")
            .expect_err("missing LiteInst runtime unexpectedly activated");

        // The exact fail-closed reason depends on which activation signal wins
        // after the valid syscall-skip transition. What matters here is that
        // the skip itself did not fail and activation remained pre-Ready.
        assert_general_pre_ready_liteinst_activation_failure(&error);
        assert_reaped("timed exec-transition activation", root_pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_ready_pending_signal_is_rejected_before_seccomp_resume() {
        let queue_once = Arc::new(AtomicBool::new(true));
        let tracer = TracerBuilder::<AllSyscallsTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .queue_liteinst_pending_signal_once_for_test(Arc::clone(&queue_once))
            .spawn()
            .await
            .expect("spawn pending-signal activation tracee");
        let root_pid = tracer.guest_pid();
        let error = tokio::time::timeout(Duration::from_secs(3), tracer.wait())
            .await
            .expect("pending-signal activation tracee hung")
            .expect_err("queued pre-Ready signal unexpectedly resumed the tracee");

        assert!(!queue_once.load(Ordering::SeqCst));
        assert_liteinst_activation_failure(
            &error,
            LiteinstActivationFailureReason::SignalBeforeHandshake(
                LiteinstActivationOperation::ResumeAfterSeccompStop,
            ),
        );
        assert_reaped("pending-signal activation", root_pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_ready_nested_signal_is_rejected_during_context_none_reinjection() {
        let force_once = Arc::new(AtomicBool::new(true));
        let tracer = TracerBuilder::<AllSyscallsTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .force_liteinst_context_none_signal_once_for_test(Arc::clone(&force_once))
            .spawn()
            .await
            .expect("spawn context-none activation tracee");
        let root_pid = tracer.guest_pid();
        let error = tokio::time::timeout(Duration::from_secs(3), tracer.wait())
            .await
            .expect("context-none activation tracee hung")
            .expect_err("nested pre-Ready reinjection signal was silently dropped");

        assert!(!force_once.load(Ordering::SeqCst), "{error}");
        assert_liteinst_activation_failure(
            &error,
            LiteinstActivationFailureReason::UnexpectedControllerProvenance(
                LiteinstActivationOperation::FinishReinjectedSyscall,
            ),
        );
        assert_reaped("context-none activation", root_pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_ready_external_sigtrap_is_rejected_during_injected_syscall_step() {
        let force_once = Arc::new(AtomicBool::new(true));
        let tracer = TracerBuilder::<ReplaceMmapTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .force_liteinst_context_signal_once_for_test(Arc::clone(&force_once))
            .spawn()
            .await
            .expect("spawn injected-step activation tracee");
        let root_pid = tracer.guest_pid();
        let error = tokio::time::timeout(Duration::from_secs(3), tracer.wait())
            .await
            .expect("injected-step activation tracee hung")
            .expect_err("external pre-Ready SIGTRAP impersonated injected-step completion");

        assert!(!force_once.load(Ordering::SeqCst), "{error}");
        assert_liteinst_activation_failure(
            &error,
            LiteinstActivationFailureReason::UnexpectedControllerProvenance(
                LiteinstActivationOperation::FinishInjectedSyscall,
            ),
        );
        assert_reaped("injected-step activation", root_pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_ready_mutated_private_stub_cannot_impersonate_injected_syscall_completion() {
        let mutate_once = Arc::new(AtomicBool::new(true));
        let tracer = TracerBuilder::<ReplaceMmapTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .force_liteinst_private_stub_mutation_once_for_test(Arc::clone(&mutate_once))
            .spawn()
            .await
            .expect("spawn private-stub-mutation activation tracee");
        let root_pid = tracer.guest_pid();
        let error = tokio::time::timeout(Duration::from_secs(3), tracer.wait())
            .await
            .expect("private-stub-mutation activation tracee hung")
            .expect_err("mutated private stub impersonated injected-syscall completion");

        assert!(!mutate_once.load(Ordering::SeqCst), "{error}");
        let ptrace_write_rejected = matches!(
            &error,
            Error::Tool(error)
                if matches!(
                    error.downcast_ref::<crate::error::Error>(),
                    Some(crate::error::Error::Internal(TraceError::Errno(Errno::EFAULT)))
                )
        );
        // Some kernels reject the forced ptrace write before the mutated stub
        // executes. Otherwise, the exact-stub provenance check must reject it.
        assert!(
            ptrace_write_rejected
                || liteinst_activation_failure_reason(&error)
                    == Some(
                        LiteinstActivationFailureReason::UnexpectedControllerProvenance(
                            LiteinstActivationOperation::FinishInjectedSyscall,
                        ),
                    ),
            "{error}"
        );
        assert_reaped("private-stub-mutation activation", root_pid);
    }

    #[derive(Default)]
    struct ReplaceMmapTool;

    #[reverie::tool]
    impl Tool for ReplaceMmapTool {
        type GlobalState = ();
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            [Sysno::mmap].into_iter().collect()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            assert_eq!(syscall.number(), Sysno::mmap);
            Ok(guest.inject(reverie::syscalls::Getpid::new()).await?)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_ready_nested_signal_is_rejected_while_skipping_seccomp_syscall() {
        let force_once = Arc::new(AtomicBool::new(true));
        let tracer = TracerBuilder::<ReplaceMmapTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .force_liteinst_skip_signal_once_for_test(Arc::clone(&force_once))
            .spawn()
            .await
            .expect("spawn skip-seccomp activation tracee");
        let root_pid = tracer.guest_pid();
        let error = tokio::time::timeout(Duration::from_secs(3), tracer.wait())
            .await
            .expect("skip-seccomp activation tracee hung")
            .expect_err("nested pre-Ready skip signal was delivered by single-step");

        assert!(!force_once.load(Ordering::SeqCst));
        assert_liteinst_activation_failure(
            &error,
            LiteinstActivationFailureReason::UnexpectedControllerProvenance(
                LiteinstActivationOperation::SkipInterceptedSyscall,
            ),
        );
        assert_reaped("skip-seccomp activation", root_pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_ready_nested_signal_is_rejected_during_tracee_preinit() {
        let force_once = Arc::new(AtomicBool::new(true));
        let tracer = TracerBuilder::<InitFailureTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .force_liteinst_preinit_signal_once_for_test(Arc::clone(&force_once))
            .spawn()
            .await
            .expect("spawn preinit-signal activation tracee");
        let root_pid = tracer.guest_pid();
        let error = tokio::time::timeout(Duration::from_secs(3), tracer.wait())
            .await
            .expect("preinit-signal activation tracee hung")
            .expect_err("nested pre-Ready preinit signal unexpectedly resumed the tracee");

        assert!(!force_once.load(Ordering::SeqCst));
        assert_liteinst_activation_failure(
            &error,
            LiteinstActivationFailureReason::UnexpectedPreinitSignal,
        );
        assert_reaped("preinit-signal activation", root_pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_ready_external_sigtrap_is_rejected_after_exec_event() {
        let force_once = Arc::new(AtomicBool::new(true));
        let tracer = TracerBuilder::<InitFailureTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .force_liteinst_post_exec_signal_once_for_test(Arc::clone(&force_once))
            .spawn()
            .await
            .expect("spawn post-exec-signal activation tracee");
        let root_pid = tracer.guest_pid();
        let error = tokio::time::timeout(Duration::from_secs(3), tracer.wait())
            .await
            .expect("post-exec-signal activation tracee hung")
            .expect_err("external SIGTRAP impersonated the required post-exec trap");

        assert!(!force_once.load(Ordering::SeqCst));
        assert_liteinst_activation_failure(
            &error,
            LiteinstActivationFailureReason::UnexpectedControllerProvenance(
                LiteinstActivationOperation::WaitForPostExecTrap,
            ),
        );
        assert_reaped("post-exec-signal activation", root_pid);
    }

    #[derive(Default)]
    struct PreciseTimerTool;

    #[reverie::tool]
    impl Tool for PreciseTimerTool {
        type GlobalState = ();
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            Subscription::none()
        }

        async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
            guest
                .set_timer_precise(reverie::TimerSchedule::RcbsAndInstructions(100, 8))
                .expect("configure precise timer after exec");
            Ok(())
        }
    }

    #[derive(Default)]
    struct PreciseTimerDeliveryTool {
        delivered: AtomicBool,
    }

    #[reverie::tool]
    impl Tool for PreciseTimerDeliveryTool {
        type GlobalState = ();
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            Subscription::none()
        }

        async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
            guest
                .set_timer_precise(reverie::TimerSchedule::Rcbs(100))
                .expect("configure precise timer at thread start");
            Ok(())
        }

        async fn handle_timer_event<G: Guest<Self>>(&self, _guest: &mut G) {
            self.delivered.store(true, Ordering::SeqCst);
        }

        async fn on_exit_process<G: reverie::GlobalRPC<Self::GlobalState>>(
            self,
            _pid: Pid,
            _global_state: &G,
            _exit_status: ExitStatus,
        ) -> Result<(), Error> {
            assert!(
                self.delivered.into_inner(),
                "precise timer event did not reach the Tool"
            );
            Ok(())
        }
    }

    async fn run_precise_timer_delivery() -> u64 {
        let _ = reverie::take_skid_overshoot_count();
        let tracer = spawn_fn_with_config::<PreciseTimerDeliveryTool, _>(
            || {
                let mut value = 0u64;
                for i in 0..1_000_000 {
                    value = std::hint::black_box(value.wrapping_add(i));
                }
                std::hint::black_box(value);
            },
            (),
            false,
        )
        .await
        .expect("spawn precise-timer tracee");
        let (status, ()) = tokio::time::timeout(Duration::from_secs(5), tracer.wait())
            .await
            .expect("precise-timer tracee timed out")
            .expect("wait precise-timer tracee");
        assert_eq!(status, ExitStatus::Exited(0));

        reverie::take_skid_overshoot_count()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn precise_timer_delivery_reaches_tool() {
        const PRECISE_TIMER_CHILD: &str = "REVERIE_PTRACE_PRECISE_TIMER_CHILD";

        if let Some(mode) = std::env::var_os(PRECISE_TIMER_CHILD) {
            let overshoot_count = run_precise_timer_delivery().await;
            match mode.to_str().expect("precise-timer child mode is UTF-8") {
                "ordinary" => assert_eq!(
                    overshoot_count, 0,
                    "ordinary precise-timer delivery unexpectedly overshot"
                ),
                "overshoot" => assert!(
                    overshoot_count > 0,
                    "zero skid margin did not exercise the overshoot path"
                ),
                other => panic!("unknown precise-timer child mode {other:?}"),
            }
            return;
        }

        if !crate::perf::is_perf_supported() {
            return;
        }

        // Both controls run in fresh exact-test processes. The overshoot count
        // is process-global, so touching it in this parent would race the timer
        // module's count assertions under Rust's parallel test runner.
        let run_child = |mode: &str, force_overshoot: bool| {
            let mut command =
                std::process::Command::new(std::env::current_exe().expect("locate test binary"));
            command
                .args([
                    "--exact",
                    "tracer::tests::precise_timer_delivery_reaches_tool",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(PRECISE_TIMER_CHILD, mode);
            if force_overshoot {
                command.env(crate::timer::SKID_MARGIN_OVERRIDE_ENV, "0");
            }
            command
                .output()
                .unwrap_or_else(|error| panic!("run {mode} precise-timer child test: {error}"))
        };

        let ordinary = run_child("ordinary", false);
        assert!(
            ordinary.status.success(),
            "ordinary precise-timer child failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&ordinary.stdout),
            String::from_utf8_lossy(&ordinary.stderr)
        );

        let output = run_child("overshoot", true);
        assert!(
            output.status.success(),
            "forced-overshoot child failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output
                .stderr
                .starts_with(crate::timer::SKID_OVERSHOOT_MARKER.as_bytes())
                || output
                    .stderr
                    .windows(crate::timer::SKID_OVERSHOOT_MARKER.len())
                    .any(|window| window == crate::timer::SKID_OVERSHOOT_MARKER.as_bytes()),
            "forced-overshoot child did not emit {}:\n{}",
            crate::timer::SKID_OVERSHOOT_MARKER,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn root_stop_guest_command(mode: &str) -> Command {
        if mode == "timer" {
            return Command::new("/bin/true");
        }
        if mode == "signal" {
            let mut command = Command::new("/usr/bin/tail");
            command.args(["-f", "/dev/null"]);
            return command;
        }
        let mut command = Command::new(std::env::current_exe().expect("locate test binary"));
        command.args([
            "--exact",
            "tracer::tests::liteinst_root_stop_pause_guest",
            "--nocapture",
        ]);
        command.env("REVERIE_LITEINST_ROOT_STOP_GUEST", mode);
        command
    }

    #[test]
    fn liteinst_root_stop_pause_guest() {
        let Some(mode) = std::env::var_os("REVERIE_LITEINST_ROOT_STOP_GUEST") else {
            return;
        };
        match mode.to_str().expect("root-stop mode is UTF-8") {
            "syscall" => {
                unsafe { libc::syscall(libc::SYS_getpid) };
            }
            "signal" => {
                signal::raise(Signal::SIGUSR1).expect("raise root-stop signal");
            }
            mode => panic!("unknown root-stop guest mode {mode}"),
        }
        loop {
            unsafe { libc::pause() };
        }
    }

    async fn cancel_at_root_stop(pause: RootStopPause, mode: &str) {
        let injected_signal = match pause {
            RootStopPause::Signal(signal) => Some(signal),
            RootStopPause::Seccomp => None,
        };
        let (stop_tx, mut stop_rx) = mpsc::unbounded_channel();
        let tracer = TracerBuilder::<RootStopTool>::new(root_stop_guest_command(mode))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test()
            .pause_liteinst_root_stop_for_test(pause, stop_tx)
            .spawn()
            .await
            .expect("spawn root-stop cancellation tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        if let Some(signal) = injected_signal {
            signal::kill(root_pid.into(), signal).expect("send root-stop test signal");
        }
        let stopped_pid = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => panic!("root-stop tracee completed before cancellation: {result:?}"),
                pid = stop_rx.recv() => pid.expect("root-stop pause channel closed"),
            }
        })
        .await
        .expect("tracee did not reach requested root stop");
        assert_eq!(stopped_pid, root_pid);

        drop(wait);
        assert_reaped("cancelled root stop", root_pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_at_generic_syscall_handler_reaps_root() {
        cancel_at_root_stop(RootStopPause::Seccomp, "syscall").await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_at_signal_handler_reaps_root() {
        cancel_at_root_stop(RootStopPause::Signal(Signal::SIGUSR1), "signal").await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_ready_liteinst_precise_timer_is_controller_handled_and_reaped() {
        if !crate::perf::is_perf_supported() {
            return;
        }
        let (step_tx, mut step_rx) = mpsc::unbounded_channel();
        let builder = TracerBuilder::<PreciseTimerTool>::new(root_stop_guest_command("timer"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .pause_liteinst_precise_timer_step_for_test(step_tx);
        let held = Arc::clone(
            &builder
                .liteinst_runtime
                .as_ref()
                .expect("LiteInst runtime configured")
                .held_root_stop,
        );
        let tracer = builder.spawn().await.expect("spawn precise-timer tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        let stopped_pid = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => panic!("precise-timer tracee completed before cancellation: {result:?}"),
                pid = step_rx.recv() => pid.expect("precise-timer pause channel closed"),
            }
        })
        .await
        .expect("precise timer did not reach its lease-backed step");
        assert_eq!(stopped_pid, root_pid);
        assert!(
            matches!(
                held.lock().unwrap().as_ref().map(|held| &held.status),
                Some(HeldRootStopStatus::Signal(Signal::SIGTRAP))
            ),
            "precise-timer step did not rearm the returned SIGTRAP stop"
        );

        drop(wait);
        assert_reaped("cancelled precise-timer step", root_pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normal_liteinst_precise_timer_completion_clears_root_lease() {
        if !crate::perf::is_perf_supported() {
            return;
        }
        let builder = TracerBuilder::<PreciseTimerTool>::new(root_stop_guest_command("timer"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test();
        let held = Arc::clone(
            &builder
                .liteinst_runtime
                .as_ref()
                .expect("LiteInst runtime configured")
                .held_root_stop,
        );
        let tracer = builder.spawn().await.expect("spawn precise-timer tracee");
        let (status, ()) = tokio::time::timeout(Duration::from_secs(5), tracer.wait())
            .await
            .expect("normal precise-timer tracee timed out")
            .expect("wait normal precise-timer tracee");
        assert_eq!(status, ExitStatus::Exited(0));
        assert!(
            held.lock().unwrap().is_none(),
            "normal precise-timer path left a stale lease"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_at_each_preinit_step_reaps_root() {
        for step in 0..=4 {
            let (step_tx, mut step_rx) = mpsc::unbounded_channel();
            let builder = TracerBuilder::<InitFailureTool>::new(Command::new("/bin/true"))
                .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
                .pause_liteinst_preinit_step_for_test(step, step_tx);
            let mut spawn = Box::pin(builder.spawn());
            let root_pid = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::select! {
                    _result = &mut spawn => panic!("preinit completed before step {step}"),
                    pid = step_rx.recv() => pid.expect("preinit pause channel closed"),
                }
            })
            .await
            .unwrap_or_else(|_| panic!("tracee did not reach preinit step {step}"));

            drop(spawn);
            assert_reaped("cancelled preinit root", root_pid);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normal_liteinst_completion_leaves_no_stale_root_stop_lease() {
        let builder = TracerBuilder::<InitFailureTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test();
        let held = Arc::clone(
            &builder
                .liteinst_runtime
                .as_ref()
                .expect("LiteInst runtime configured")
                .held_root_stop,
        );
        let tracer = builder.spawn().await.expect("spawn normal LiteInst tracee");
        let (status, ()) = tracer.wait().await.expect("wait normal LiteInst tracee");
        assert_eq!(status, ExitStatus::Exited(0));
        assert!(
            held.lock().unwrap().is_none(),
            "normal path left stale lease"
        );
    }

    #[test]
    fn resolving_program_preserves_explicit_arg0() {
        let mut command = Command::new("/bin/echo");
        command.arg0("chosen-name");
        resolve_program(&mut command).unwrap();
        assert_eq!(command.get_program(), "/bin/echo");
        assert_eq!(command.get_arg0(), "chosen-name");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn liteinst_preinit_failure_reaps_and_unregisters_root() {
        let error = match TracerBuilder::<InitFailureTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .fail_liteinst_preinit_for_test()
            .spawn()
            .await
        {
            Ok(_) => panic!("injected LiteInst preinit failure unexpectedly succeeded"),
            Err(error) => error,
        };
        let message = error.to_string();
        let pid = message
            .split("tracee ")
            .nth(1)
            .and_then(|suffix| suffix.split(':').next())
            .and_then(|pid| pid.parse::<i32>().ok())
            .unwrap_or_else(|| panic!("preinit error omitted tracee PID: {message}"));

        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "failed LiteInst preinit left tracee {pid} in procfs: {message}"
        );
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD),
            "failed LiteInst preinit left tracee {pid} waitable"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_drop_retries_first_discovery_failure() {
        let fail_once = Arc::new(AtomicBool::new(true));
        let tracer = TracerBuilder::<InitFailureTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .fail_liteinst_discovery_once_for_test(Arc::clone(&fail_once))
            .spawn()
            .await
            .expect("spawn direct-Drop cleanup tracee");
        let root_pid = tracer.guest_pid();

        drop(tracer);
        let reaped_by_drop = !std::path::Path::new(&format!("/proc/{root_pid}")).exists();
        if !reaped_by_drop {
            // Preserve a clean host after recording the pre-fix failure.
            unsafe { libc::kill(root_pid.as_raw(), libc::SIGKILL) };
            for _ in 0..2_000 {
                if !std::path::Path::new(&format!("/proc/{root_pid}")).exists() {
                    break;
                }
                let _ = ptrace::cont(root_pid.into(), None);
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        assert!(!fail_once.load(Ordering::SeqCst));
        assert!(reaped_by_drop, "direct Drop stopped after its first error");
        assert_reaped("direct-Drop root", root_pid);
    }

    #[test]
    fn stale_pidfd_identity_never_signals_reused_numeric_pid() {
        let old_pid = fork_paused_child();
        let mut identity = untraced_process_identity(old_pid);
        identity
            .send_signal(Signal::SIGKILL)
            .expect("kill old child");
        Running::new(old_pid).wait().expect("reap old child");

        let unrelated_pid = fork_paused_child();
        identity.tid = unrelated_pid;
        assert_eq!(identity.send_signal(Signal::SIGKILL), Err(Errno::ESRCH));
        assert_eq!(unsafe { libc::kill(unrelated_pid.as_raw(), 0) }, 0);

        unsafe { libc::kill(unrelated_pid.as_raw(), libc::SIGKILL) };
        Running::new(unrelated_pid)
            .wait()
            .expect("reap unrelated child");
    }

    #[test]
    fn discovery_skips_only_confirmed_absence_or_replacement() {
        let absent = Pid::from_raw(i32::MAX - 71);
        assert!(skippable_tracee_open_error(absent, Errno::ENOENT));
        assert!(skippable_tracee_open_error(absent, Errno::ESRCH));
        for error in [Errno::EMFILE, Errno::ENFILE, Errno::EIO] {
            assert!(
                !skippable_tracee_open_error(absent, error),
                "resource/read error {error} was silently skipped"
            );
        }

        let replacement = fork_paused_child();
        assert!(skippable_tracee_open_error(replacement, Errno::ESRCH));
        assert!(!skippable_tracee_open_error(replacement, Errno::EMFILE));
        unsafe { libc::kill(replacement.as_raw(), libc::SIGKILL) };
        Running::new(replacement)
            .wait()
            .expect("reap replacement fixture");
    }

    #[test]
    fn pidfd_setup_error_retains_cleanup_drain_failure() {
        let message = liteinst_pidfd_setup_error(
            Pid::from_raw(42),
            Errno::EMFILE,
            None,
            Err(TraceError::Errno(Errno::EIO)),
        )
        .to_string();
        assert!(
            message.contains("failed to open pidfd for LiteInst tracee 42"),
            "genuine pidfd failure lost its cause: {message}"
        );
        assert!(
            message.contains("EMFILE"),
            "missing pidfd failure: {message}"
        );
        assert!(message.contains("EIO"), "missing drain failure: {message}");
    }

    #[test]
    fn pidfd_setup_timeout_names_thread_group_leader_retry_exhaustion() {
        let message = liteinst_pidfd_setup_error(Pid::from_raw(42), Errno::ETIMEDOUT, None, Ok(()))
            .to_string();
        assert!(
            message.contains(
                "root identity did not become a stable traced thread-group leader with a pidfd within the 2,000-attempt retry budget"
            ),
            "missing root-identity retry exhaustion: {message}"
        );
        assert!(
            !message.contains("failed to open pidfd"),
            "timeout still blames pidfd_open: {message}"
        );
    }

    #[test]
    fn tracee_generation_survives_zombie_until_real_reap() {
        let pid = fork_paused_child();
        let identity = untraced_process_identity(pid);
        identity
            .send_signal(Signal::SIGKILL)
            .expect("kill child through pidfd");

        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid.as_raw() as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        assert_eq!(result, 0, "observe child zombie without reaping");
        assert!(identity.same_process(), "zombie lost generation identity");
        assert!(
            !identity.is_our_tracee(),
            "untraced zombie became active tracee"
        );
        assert!(
            !terminal_descendant_remains_owned(&identity),
            "terminal cleanup retained a zombie after its ptrace relationship ended"
        );

        Running::new(pid).wait().expect("reap child");
        assert!(
            !identity.same_process(),
            "reaped child still matched identity"
        );
    }

    #[test]
    fn terminal_descendant_retention_does_not_touch_an_untraced_process() {
        let (traced_pid, stopped) = spawn_held_stop_child("terminal descendant retention child");
        let traced_identity =
            TraceeIdentity::open_root(traced_pid).expect("capture traced child identity");
        assert!(
            terminal_descendant_remains_owned(&traced_identity),
            "cleanup dropped a live tracee"
        );
        let wait = stopped
            .resume(None)
            .expect("resume traced child")
            .wait()
            .expect("reap traced child");
        assert_eq!(wait.assume_exited().1, ExitStatus::Exited(0));

        let unrelated_pid = fork_paused_child();
        let unrelated_identity = untraced_process_identity(unrelated_pid);
        assert!(
            !terminal_descendant_remains_owned(&unrelated_identity),
            "cleanup treated an untraced process as its descendant"
        );
        assert_eq!(unsafe { libc::kill(unrelated_pid.as_raw(), 0) }, 0);
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(unrelated_pid.as_raw(), &mut status, libc::WNOHANG) },
            0,
            "retention check changed an unrelated process"
        );

        unsafe { libc::kill(unrelated_pid.as_raw(), libc::SIGKILL) };
        Running::new(unrelated_pid)
            .wait()
            .expect("reap unrelated child");
    }

    #[test]
    fn terminal_descendant_remains_owned_until_recorded_parent_exits() {
        let (parent_pid, descendant_pid, mut control) = fork_paused_grandchild();
        ptrace::attach(descendant_pid.into()).expect("attach retained descendant");
        let (stopped, event) = Running::new(descendant_pid)
            .wait()
            .expect("wait for retained descendant attach")
            .assume_stopped();
        assert_eq!(event, Event::Signal(Signal::SIGSTOP));
        let identity = TraceeIdentity::capture(
            descendant_pid,
            Some((parent_pid, Some(ChildOp::Fork))),
            true,
        )
        .expect("capture production-shaped descendant identity");
        stopped
            .detach(None)
            .expect("detach retained descendant after identity capture");

        assert!(identity.same_process(), "descendant identity changed");
        assert!(
            !identity.is_our_tracee(),
            "detached descendant still reports this process as tracer"
        );
        assert_eq!(
            tracee_snapshot(descendant_pid)
                .expect("read retained descendant parent")
                .ppid,
            parent_pid
        );
        let retained_while_parent_alive = terminal_descendant_remains_owned(&identity);

        control.write_all(&[1]).expect("release recorded parent");
        drop(control);
        Running::new(parent_pid)
            .wait()
            .expect("reap recorded parent");
        for _ in 0..2_000 {
            if tracee_snapshot(descendant_pid).is_ok_and(|snapshot| snapshot.ppid != parent_pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_ne!(
            tracee_snapshot(descendant_pid)
                .expect("read reparented descendant")
                .ppid,
            parent_pid,
            "descendant did not leave its recorded parent"
        );
        let released_after_reparenting = !terminal_descendant_remains_owned(&identity);

        identity
            .send_signal(Signal::SIGKILL)
            .expect("kill reparented descendant through pidfd");
        for _ in 0..2_000 {
            let mut status = 0;
            let waited = unsafe {
                libc::waitpid(
                    descendant_pid.as_raw(),
                    &mut status,
                    libc::WNOHANG | libc::__WALL,
                )
            };
            if waited == descendant_pid.as_raw()
                || !std::path::Path::new(&format!("/proc/{descendant_pid}")).exists()
            {
                assert!(
                    retained_while_parent_alive,
                    "cleanup released a terminal descendant while its recorded parent still owned it"
                );
                assert!(
                    released_after_reparenting,
                    "cleanup retained a terminal descendant after reparenting"
                );
                return;
            }
            assert!(
                waited == 0 || (waited == -1 && Errno::last() == Errno::ECHILD),
                "unexpected wait result while cleaning reparented descendant: {waited}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("reparented descendant {descendant_pid} was not reaped");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_death_before_initialization_error_does_not_hang() {
        let pid = match unsafe { unistd::fork() }.expect("fork dying child") {
            ForkResult::Child => std::process::exit(42),
            ForkResult::Parent { child } => Pid::from(child),
        };
        assert!(matches!(
            Running::new(pid).next_state().await.unwrap(),
            safeptrace::Wait::Exited(_, ExitStatus::Exited(42))
        ));
        let died = Stopped::new_unchecked(pid)
            .resume(None)
            .expect_err("resuming a reaped child must report Died");
        assert!(matches!(died, TraceError::Died(_)));

        tokio::time::timeout(Duration::from_secs(1), initialization_error(pid, died))
            .await
            .expect("initialization_error hung reaping an already terminal child");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_at_new_child_event_reaps_root_and_child() {
        let (child_tx, mut child_rx) = mpsc::unbounded_channel();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 60 & wait"]);
        let tracer = TracerBuilder::<InitFailureTool>::new(command)
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test()
            .pause_liteinst_new_task_for_test(child_tx)
            .spawn()
            .await
            .expect("spawn fork-cancellation tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        let child_pid = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => panic!("tracee completed before cancellation: {result:?}"),
                child = child_rx.recv() => child.expect("new-child hook closed"),
            }
        })
        .await
        .expect("tracee did not reach new-child cancellation window");

        drop(wait);
        assert_reaped("root", root_pid);
        // A terminal child may have been reparented before the notifier
        // releases its identity. At that point this process cannot reap it;
        // require the new parent to finish reaping it within the same bounded
        // interval used by fail-closed cleanup instead of racing procfs.
        assert_eventually_reaped("child", child_pid);
        for (role, pid) in [("root", root_pid), ("child", child_pid)] {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), Running::new(pid).next_state())
                    .await
                    .unwrap_or_else(|_| panic!("late {role} notifier wait hung")),
                Err(TraceError::Errno(Errno::ECHILD))
            );
        }
    }

    fn clone_thread_guest_command() -> Command {
        let mut command = Command::new(std::env::current_exe().expect("locate test binary"));
        command.args([
            "--exact",
            "tracer::tests::liteinst_clone_thread_guest",
            "--nocapture",
        ]);
        command.env("REVERIE_LITEINST_CLONE_THREAD_GUEST", "1");
        command
    }

    fn clone_parent_guest_command() -> Command {
        static GUEST: LazyLock<PathBuf> = LazyLock::new(|| {
            let source =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/clone_parent.c");
            let output =
                std::env::temp_dir().join(format!("reverie-clone-parent-{}", std::process::id()));
            let status = std::process::Command::new("cc")
                .args(["-O0", "-g"])
                .arg(&source)
                .arg("-o")
                .arg(&output)
                .status()
                .expect("invoke cc for CLONE_PARENT fixture");
            assert!(status.success(), "compile {}", source.display());
            output
        });
        Command::new(GUEST.as_path())
    }

    #[test]
    fn liteinst_clone_thread_guest() {
        if std::env::var_os("REVERIE_LITEINST_CLONE_THREAD_GUEST").is_none() {
            return;
        }
        let thread = std::thread::spawn(|| {
            loop {
                std::thread::park();
            }
        });
        thread.join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn liteinst_clone_thread_fails_closed_and_reaps_group() {
        let (child_tx, mut child_rx) = mpsc::unbounded_channel();
        let tracer = TracerBuilder::<InitFailureTool>::new(clone_thread_guest_command())
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test()
            .fail_liteinst_new_task_for_test()
            .observe_liteinst_new_task_for_test(child_tx)
            .spawn()
            .await
            .expect("spawn CLONE_THREAD fail-closed tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        let first = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => Either::Left(result),
                child = child_rx.recv() => Either::Right(child.expect("new-thread observer closed")),
            }
        })
        .await
        .expect("tracee did not report CLONE_THREAD identity");
        let (child_tid, completed) = match first {
            Either::Left(result) => (
                child_rx
                    .recv()
                    .await
                    .expect("completed tracee omitted bound thread identity"),
                Some(result),
            ),
            Either::Right(child_tid) => (child_tid, None),
        };
        assert_ne!(root_pid, child_tid, "thread event reused root TID");

        let result = match completed {
            Some(result) => result,
            None => tokio::time::timeout(Duration::from_secs(3), &mut wait)
                .await
                .expect("CLONE_THREAD fail-closed cleanup hung"),
        };
        let error = result.expect_err("CLONE_THREAD LiteInst tracee unexpectedly succeeded");
        assert!(
            error.to_string().contains("ENOTSUPP"),
            "fail-closed error omitted unsupported-thread cause: {error}"
        );
        assert_reaped("root", root_pid);
        assert_reaped("thread", child_tid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_at_clone_thread_event_reaps_group() {
        let (child_tx, mut child_rx) = mpsc::unbounded_channel();
        let tracer = TracerBuilder::<InitFailureTool>::new(clone_thread_guest_command())
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test()
            .pause_liteinst_new_task_for_test(child_tx)
            .spawn()
            .await
            .expect("spawn CLONE_THREAD cancellation tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        let child_tid = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => panic!("CLONE_THREAD tracee completed before cancellation: {result:?}"),
                child = child_rx.recv() => child.expect("new-thread hook closed"),
            }
        })
        .await
        .expect("tracee did not reach CLONE_THREAD cancellation window");
        assert_ne!(root_pid, child_tid, "thread event reused root TID");

        drop(wait);
        assert_reaped("root", root_pid);
        assert_reaped("thread", child_tid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_before_clone_thread_handler_reaps_group() {
        let (child_tx, mut child_rx) = mpsc::unbounded_channel();
        let tracer = TracerBuilder::<InitFailureTool>::new(clone_thread_guest_command())
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test()
            .pause_before_liteinst_new_task_for_test(child_tx)
            .spawn()
            .await
            .expect("spawn pre-handler CLONE_THREAD cancellation tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        let child_tid = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => panic!("CLONE_THREAD tracee completed before pre-handler cancellation: {result:?}"),
                child = child_rx.recv() => child.expect("pre-handler new-thread hook closed"),
            }
        })
        .await
        .expect("tracee did not reach pre-handler CLONE_THREAD window");

        drop(wait);
        assert_reaped("root", root_pid);
        assert_reaped("thread", child_tid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn discovery_error_restores_newborn_for_cleanup_retry() {
        let (child_tx, mut child_rx) = mpsc::unbounded_channel();
        let fail_once = Arc::new(AtomicBool::new(true));
        let tracer = TracerBuilder::<InitFailureTool>::new(clone_thread_guest_command())
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test()
            .fail_liteinst_new_task_for_test()
            .observe_liteinst_new_task_for_test(child_tx)
            .fail_liteinst_discovery_once_for_test(Arc::clone(&fail_once))
            .spawn()
            .await
            .expect("spawn discovery-retry tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        let first = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => Either::Left(result),
                child = child_rx.recv() => Either::Right(child.expect("new-thread observer closed")),
            }
        })
        .await
        .expect("tracee did not reach discovery-retry event");
        let (child_tid, completed) = match first {
            Either::Left(result) => (
                child_rx
                    .recv()
                    .await
                    .expect("completed tracee omitted retry child identity"),
                Some(result),
            ),
            Either::Right(child_tid) => (child_tid, None),
        };
        let result = match completed {
            Some(result) => result,
            None => tokio::time::timeout(Duration::from_secs(3), &mut wait)
                .await
                .expect("discovery cleanup retry hung"),
        };
        let error = result.expect_err("unsupported CLONE_THREAD unexpectedly succeeded");
        assert!(
            error.to_string().contains("Input/output error"),
            "injected discovery failure was not reported: {error}"
        );
        assert!(!fail_once.load(Ordering::SeqCst));
        assert_reaped("root", root_pid);
        assert_reaped("thread", child_tid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn post_task_scan_error_retains_exact_cleanup_for_retry() {
        let (child_tx, mut child_rx) = mpsc::unbounded_channel();
        let fail_once = Arc::new(AtomicBool::new(true));
        let force_scan_once = Arc::new(AtomicBool::new(true));
        let tracer = TracerBuilder::<InitFailureTool>::new(clone_thread_guest_command())
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test()
            .fail_liteinst_new_task_for_test()
            .observe_liteinst_new_task_for_test(child_tx)
            .fail_liteinst_after_task_scan_once_for_test(
                Arc::clone(&fail_once),
                Arc::clone(&force_scan_once),
            )
            .spawn()
            .await
            .expect("spawn post-task-scan retry tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        let first = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => Either::Left(result),
                child = child_rx.recv() => Either::Right(child.expect("task-scan observer closed")),
            }
        })
        .await
        .expect("tracee did not reach post-task-scan event");
        let (child_tid, completed) = match first {
            Either::Left(result) => (
                child_rx
                    .recv()
                    .await
                    .expect("completed tracee omitted task-scan TID"),
                Some(result),
            ),
            Either::Right(child_tid) => (child_tid, None),
        };
        let result = match completed {
            Some(result) => result,
            None => tokio::time::timeout(Duration::from_secs(3), &mut wait)
                .await
                .expect("post-task-scan cleanup retry hung"),
        };
        let error = result.expect_err("injected post-task-scan error unexpectedly succeeded");
        assert!(error.to_string().contains("Input/output error"), "{error}");
        assert!(!fail_once.load(Ordering::SeqCst));
        assert!(!force_scan_once.load(Ordering::SeqCst));
        assert_reaped("root", root_pid);
        assert_reaped("task-scan thread", child_tid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn clone_parent_fails_closed_and_reaps_sibling() {
        let (child_tx, mut child_rx) = mpsc::unbounded_channel();
        let tracer = TracerBuilder::<InitFailureTool>::new(clone_parent_guest_command())
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test()
            .fail_liteinst_new_task_for_test()
            .observe_liteinst_new_task_for_test(child_tx)
            .spawn()
            .await
            .expect("spawn CLONE_PARENT fail-closed tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        let first = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => Either::Left(result),
                child = child_rx.recv() => Either::Right(child.expect("CLONE_PARENT observer closed")),
            }
        })
        .await
        .expect("tracee did not report CLONE_PARENT event");
        let (sibling_pid, completed) = match first {
            Either::Left(result) => (
                child_rx
                    .recv()
                    .await
                    .expect("completed tracee omitted CLONE_PARENT identity"),
                Some(result),
            ),
            Either::Right(child_pid) => (child_pid, None),
        };
        let result = match completed {
            Some(result) => result,
            None => tokio::time::timeout(Duration::from_secs(3), &mut wait)
                .await
                .expect("CLONE_PARENT fail-closed cleanup hung"),
        };
        let error = result.expect_err("CLONE_PARENT LiteInst tracee unexpectedly succeeded");
        assert!(error.to_string().contains("ENOTSUPP"), "{error}");
        assert_reaped("root", root_pid);
        assert_reaped("CLONE_PARENT sibling", sibling_pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_at_clone_parent_event_reaps_sibling() {
        let (child_tx, mut child_rx) = mpsc::unbounded_channel();
        let tracer = TracerBuilder::<InitFailureTool>::new(clone_parent_guest_command())
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test()
            .pause_liteinst_new_task_for_test(child_tx)
            .spawn()
            .await
            .expect("spawn CLONE_PARENT cancellation tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        let sibling_pid = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => panic!("CLONE_PARENT tracee completed before cancellation: {result:?}"),
                child = child_rx.recv() => child.expect("CLONE_PARENT hook closed"),
            }
        })
        .await
        .expect("tracee did not reach CLONE_PARENT cancellation window");

        drop(wait);
        assert_reaped("root", root_pid);
        assert_reaped("CLONE_PARENT sibling", sibling_pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_before_clone_parent_handler_reaps_sibling() {
        let (child_tx, mut child_rx) = mpsc::unbounded_channel();
        let tracer = TracerBuilder::<InitFailureTool>::new(clone_parent_guest_command())
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .activate_liteinst_without_handshake_for_test()
            .pause_before_liteinst_new_task_for_test(child_tx)
            .spawn()
            .await
            .expect("spawn pre-handler CLONE_PARENT tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        let sibling_pid = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => panic!("CLONE_PARENT tracee completed before pre-handler cancellation: {result:?}"),
                child = child_rx.recv() => child.expect("pre-handler CLONE_PARENT hook closed"),
            }
        })
        .await
        .expect("tracee did not reach pre-handler CLONE_PARENT window");

        drop(wait);
        assert_reaped("root", root_pid);
        assert_reaped("CLONE_PARENT sibling", sibling_pid);
    }

    // Start from a real consumed SIGSTOP, retain that capability, and make the
    // kernel report its death without consuming the terminal wait status.
    // This deliberately uses only synchronous waiting until initialization_error
    // takes over: an async notifier must not pre-consume the pending test status.
    async fn initial_wait_pending_death_control(consume_elsewhere: bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let pid = match unsafe { unistd::fork() }.expect("fork stopped initialization child") {
            ForkResult::Child => {
                if safeptrace::traceme_and_stop().is_err() {
                    unsafe { libc::_exit(91) };
                }
                unsafe { libc::_exit(92) };
            }
            ForkResult::Parent { child } => Pid::from(child),
        };
        let peek = |options: i32| {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe {
                    libc::waitid(
                        libc::P_PID,
                        pid.as_raw() as u32,
                        &mut info,
                        options | libc::WNOWAIT | libc::WNOHANG,
                    )
                },
                0,
                "nonconsuming exact-child wait failed: {}",
                Errno::last()
            );
            info
        };
        loop {
            let info = peek(libc::WSTOPPED);
            if unsafe { info.si_pid() } == pid.as_raw() {
                assert_eq!(unsafe { info.si_status() }, libc::SIGSTOP);
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "initial stop deadline"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let (stopped, event) = Running::new(pid)
            .wait()
            .expect("consume the already observed stop")
            .assume_stopped();
        assert_eq!(event, Event::Signal(Signal::SIGSTOP));
        let before = tracee_snapshot(pid).expect("real stopped child generation");
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.as_raw(), 0) };
        assert!(fd >= 0, "open held child pidfd: {}", Errno::last());
        let pidfd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        assert_eq!(
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            },
            0,
            "signal only the held stopped generation"
        );
        loop {
            let info = peek(libc::WEXITED);
            if unsafe { info.si_pid() } == pid.as_raw() {
                assert_eq!(info.si_code, libc::CLD_KILLED);
                assert_eq!(unsafe { info.si_status() }, libc::SIGKILL);
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "pending death deadline"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let zombie = tracee_snapshot(pid).expect("WNOWAIT must retain the actual zombie");
        assert_eq!(before.start_time, zombie.start_time);
        let died = stopped
            .getregs()
            .expect_err("actual killed stop must report death");
        assert!(matches!(died, TraceError::Died(_)));
        eprintln!(
            "initial-wait pending-reap pid={pid} start={} kernel_signal={} consume_elsewhere={consume_elsewhere}",
            zombie.start_time,
            libc::SIGKILL
        );
        if consume_elsewhere {
            let mut status = 0;
            assert_eq!(
                unsafe { libc::waitpid(pid.as_raw(), &mut status, libc::WNOHANG) },
                pid.as_raw(),
                "opposing waiter consumes the already observed real status"
            );
            assert!(libc::WIFSIGNALED(status));
            assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
        }
        let error = tokio::time::timeout_at(deadline, initialization_error(pid, died))
            .await
            .expect("initialization conversion exceeded the one total three-second bound");
        assert!(matches!(error, Error::Tool(_)));
        let message = error.to_string();
        if consume_elsewhere {
            assert!(
                message.contains("terminal status could not be reaped"),
                "{message}"
            );
            assert!(
                !message.contains("exited during ptrace initialization with"),
                "{message}"
            );
        } else {
            assert_eq!(
                message,
                format!(
                    "tracee {pid} exited during ptrace initialization with Signaled(SIGKILL, false)"
                )
            );
        }
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "real child remains after its terminal status should be consumed"
        );
        assert!(tokio::time::Instant::now() <= deadline);
        eprintln!("initial-wait final pid={pid} root_absent=true error={message}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn initial_wait_reaps_a_genuinely_pending_died_status() {
        initial_wait_pending_death_control(false).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn initial_wait_refuses_a_died_status_consumed_by_another_waiter() {
        initial_wait_pending_death_control(true).await;
    }

    static INITIAL_WAIT_CALLBACKS: [std::sync::atomic::AtomicUsize; 3] =
        [const { std::sync::atomic::AtomicUsize::new(0) }; 3];

    #[derive(Default)]
    struct InitialWaitCallbackWitness;

    #[reverie::tool]
    impl Tool for InitialWaitCallbackWitness {
        type GlobalState = ();
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            Subscription::none()
        }

        async fn handle_thread_start<G: Guest<Self>>(&self, _guest: &mut G) -> Result<(), Error> {
            INITIAL_WAIT_CALLBACKS[0].fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            _tid: reverie::Tid,
            _global: &G,
            _state: Self::ThreadState,
            _status: ExitStatus,
        ) -> Result<(), Error> {
            INITIAL_WAIT_CALLBACKS[1].fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn on_exit_process<G: reverie::GlobalRPC<Self::GlobalState>>(
            self,
            _pid: Pid,
            _global: &G,
            _status: ExitStatus,
        ) -> Result<(), Error> {
            INITIAL_WAIT_CALLBACKS[2].fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn initial_wait_monotonic_ns() -> u64 {
        let mut now: libc::timespec = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) },
            0
        );
        u64::try_from(now.tv_sec)
            .unwrap()
            .checked_mul(1_000_000_000)
            .unwrap()
            .checked_add(u64::try_from(now.tv_nsec).unwrap())
            .unwrap()
    }

    fn command_pretraceme_exit_control(expected_exit: i32, test_name: &str) {
        const INNER: &str = "REVERIE_INITIAL_WAIT_EXIT_CHILD";
        const DEADLINE: &str = "REVERIE_INITIAL_WAIT_EXIT_DEADLINE_NS";

        if let Some(selected_test) = std::env::var_os(INNER) {
            assert_eq!(selected_test, test_name);
            let deadline: u64 = std::env::var(DEADLINE)
                .expect("parent-issued absolute deadline")
                .parse()
                .expect("monotonic deadline must be an integer");
            assert!(initial_wait_monotonic_ns() < deadline);

            // This fresh exact-test process owns its signal policy and callback
            // counters. The parallel library runner's process is not changed.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = libc::SIG_DFL;
            assert_eq!(unsafe { libc::sigemptyset(&mut action.sa_mask) }, 0);
            assert_eq!(
                unsafe { libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) },
                0
            );

            let (mut witness, child_witness) =
                std::os::unix::net::UnixStream::pair().expect("initial-wait PID witness");
            witness.set_nonblocking(true).unwrap();
            let witness_fd = child_witness.as_raw_fd();
            let mut command = Command::new("/bin/true");
            unsafe {
                command.pre_exec(move || {
                    let bytes = libc::getpid().to_ne_bytes();
                    let written = libc::write(witness_fd, bytes.as_ptr().cast(), bytes.len());
                    if written != bytes.len() as isize {
                        return Err(Errno::EIO);
                    }
                    // Caller callbacks precede Reverie's TRACEME/init callback.
                    libc::_exit(expected_exit);
                });
            }
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("initial-wait test runtime");
            let error = runtime.block_on(async {
                let remaining =
                    Duration::from_nanos(deadline.saturating_sub(initial_wait_monotonic_ns()));
                match tokio::time::timeout(
                    remaining,
                    TracerBuilder::<InitialWaitCallbackWitness>::new(command).spawn(),
                )
                .await
                .expect("public spawn exceeded the one total three-second bound")
                {
                    Err(error) => error,
                    Ok(_) => panic!("early pre-TRACEME exit must not construct a Tracer"),
                }
            });
            drop(child_witness);
            let mut bytes = [0; std::mem::size_of::<i32>()];
            std::io::Read::read_exact(&mut witness, &mut bytes)
                .expect("the real pre_exec callback must publish its PID before exiting");
            let pid = i32::from_ne_bytes(bytes);
            assert!(pid > 0);
            assert!(matches!(error, Error::Tool(_)), "{error}");
            assert_eq!(
                error.to_string(),
                format!(
                    "tracee {pid} exited during ptrace initialization with Exited({expected_exit})"
                )
            );
            assert_eq!(
                INITIAL_WAIT_CALLBACKS
                    .each_ref()
                    .map(|count| count.load(Ordering::SeqCst)),
                [0; 3],
                "an uninitialized guest must not receive Tool lifecycle callbacks"
            );
            assert!(
                !std::path::Path::new(&format!("/proc/{pid}")).exists(),
                "early-exit guest remains after its observed terminal status"
            );
            assert!(initial_wait_monotonic_ns() <= deadline);
            eprintln!("initial-wait public pid={pid} callbacks=0 error={error}");
            return;
        }

        // As with the precise-timer control, re-exec just this test to isolate
        // process-global state. One deadline includes startup and final wait.
        let deadline = initial_wait_monotonic_ns() + 3_000_000_000;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
            .env(INNER, test_name)
            .env(DEADLINE, deadline.to_string())
            .spawn()
            .expect("spawn isolated initial-wait regression");
        loop {
            if let Some(status) = child.try_wait().expect("observe exact regression child") {
                assert!(
                    status.success(),
                    "isolated initial-wait regression: {status}"
                );
                assert!(initial_wait_monotonic_ns() <= deadline);
                return;
            }
            assert!(
                initial_wait_monotonic_ns() < deadline,
                "initial-wait regression exceeded its one total three-second bound; child not known terminal"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn command_pretraceme_exit_zero_is_initialization_error() {
        command_pretraceme_exit_control(
            0,
            "tracer::tests::command_pretraceme_exit_zero_is_initialization_error",
        );
    }

    #[test]
    fn command_pretraceme_exit_73_is_initialization_error() {
        command_pretraceme_exit_control(
            73,
            "tracer::tests::command_pretraceme_exit_73_is_initialization_error",
        );
    }
}
