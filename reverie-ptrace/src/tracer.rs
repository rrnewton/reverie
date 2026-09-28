/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Tracer` type, plus ways to spawn it and retrieve its output.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
#[cfg(test)]
use std::collections::HashMap;
use std::collections::VecDeque;
#[cfg(target_arch = "x86_64")]
use std::ffi::OsString;
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
#[cfg(test)]
use std::sync::Barrier;
use std::sync::LazyLock;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock as StdOnceLock;
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
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
use reverie::process::Child as ProcessChild;
use reverie::process::ChildStderr;
use reverie::process::ChildStdin;
use reverie::process::ChildStdout;
use reverie::process::Command;
#[cfg(target_arch = "x86_64")]
use reverie::process::ControllerLaunchParts;
#[cfg(target_arch = "x86_64")]
use reverie::process::ControllerSpawnError;
#[cfg(target_arch = "x86_64")]
use reverie::process::ControllerStartupPublisher;
use reverie::process::Output;
use reverie::process::seccomp;
use reverie::syscalls::Sysno;
use safeptrace::ChildOp;
use safeptrace::CleanupStopLease;
use safeptrace::CleanupStopTransfer;
use safeptrace::Error as TraceError;
use safeptrace::Event;
use safeptrace::OriginalRootStartup;
use safeptrace::OriginalRootStartupError;
use safeptrace::OriginalRootStartupIdentity;
use safeptrace::PhysicalEventGenerationId;
use safeptrace::PhysicalEventObserver;
#[cfg(test)]
use safeptrace::PhysicalEventObserverConfig;
use safeptrace::PhysicalResumeAttempt;
use safeptrace::PhysicalResumeOwner;
use safeptrace::PhysicalStatusDisposition;
use safeptrace::PhysicalTaskIdentity;
use safeptrace::Running;
use safeptrace::StopResolutionLaterStatus;
use safeptrace::Stopped;
use safeptrace::TerminalCleanup;
use safeptrace::TerminalCleanupContinue;
use safeptrace::TransferredStopCompletion;
use safeptrace::TransferredStopResolution;
use safeptrace::TransferredStopSuccessor;
use safeptrace::Wait;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::LiteinstInstrumentationStats;
use crate::LiteinstInstrumentationStatsHandle;
use crate::PtraceBackendStatsSource;
use crate::cp;
#[cfg(target_arch = "x86_64")]
use crate::error::LiteinstAfterLoaderAuthenticationFailure;
#[cfg(target_arch = "x86_64")]
use crate::error::LiteinstAfterLoaderAuthenticationStage;
use crate::gdbstub::GdbServer;
use crate::task::Child;
use crate::task::InjectedSyscallProvenance;
use crate::task::InjectedSyscallTrap;
use crate::task::LiteinstRuntimeConfig;
#[cfg(test)]
use crate::task::RootStopPause;
use crate::task::TracedTask;
use crate::task::TracedTaskOptions;

#[cfg(target_arch = "x86_64")]
fn freeze_after_loader_environment(
    command: &mut Command,
    expected: &BTreeMap<OsString, OsString>,
) -> std::io::Result<()> {
    let actual = command.get_captured_envs();
    if &actual != expected {
        return Err(std::io::Error::other(
            "complete after-loader environment differs before final capture",
        ));
    }
    command.env_clear().envs(&actual);
    if &command.get_captured_envs() != expected {
        return Err(std::io::Error::other(
            "complete after-loader environment differs after final capture",
        ));
    }
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn after_loader_authentication_refusal(
    stage: LiteinstAfterLoaderAuthenticationStage,
    source: std::io::Error,
) -> Error {
    Error::Tool(anyhow::Error::new(
        LiteinstAfterLoaderAuthenticationFailure::new(stage, source),
    ))
}

type SpawnStdio = (Option<ChildStdin>, Option<ChildStdout>, Option<ChildStderr>);

fn take_spawn_stdio(
    ordinary: &mut Option<ProcessChild>,
    controller: &mut Option<SpawnStdio>,
) -> SpawnStdio {
    if let Some(stdio) = controller.take() {
        debug_assert!(ordinary.is_none());
        return stdio;
    }
    let child = ordinary
        .as_mut()
        .expect("ordinary spawn child is absent without controller stdio");
    let stdio = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    core::mem::forget(
        ordinary
            .take()
            .expect("ordinary spawn child disappeared while taking stdio"),
    );
    stdio
}

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

    // Present only for the single-process dynamic LiteInst host. Ordinary
    // ptrace and e9patch lifecycles retain their existing teardown behavior.
    liteinst_cleanup: Option<LiteinstTraceeCleanup>,
    liteinst_instrumentation_stats: Option<Arc<StdMutex<LiteinstInstrumentationStats>>>,
    #[cfg(target_arch = "x86_64")]
    liteinst_physical_observer: Option<(
        safeptrace::PhysicalEventObserver,
        crate::LiteinstCallerDiagnostics,
    )>,

    // Present only when the caller requested general ptrace activity stats.
    backend_stats: Option<PtraceBackendStatsSource>,
}

struct LiteinstTraceeCleanup {
    identity: TraceeIdentity,
    physical_observer: Option<PhysicalEventObserver>,
    exact_controller_startup: bool,
    newborn_tracees: NewbornTracees,
    armed: bool,
    unstarted_terminal: Option<TerminalCleanup>,
    terminal: Option<TerminalCleanup>,
    notifier_owner: Option<ThreadId>,
    retained_descendants: BTreeMap<PhysicalEventGenerationId, RegisteredTraceeCleanup>,
    retained_terminal_descendants: BTreeMap<PhysicalEventGenerationId, TraceeIdentity>,
    held_task_stops: HeldTaskStops,
    root_frozen: bool,
    root_frozen_stop: Option<CleanupStopLease>,
    #[cfg(test)]
    fail_discovery_once: Option<Arc<AtomicBool>>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueuedParentCheck {
    Active(TraceeSnapshot),
    Unavailable,
    LiveTracerChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParentScanErrorResolution {
    DiscardParent,
}

fn resolve_parent_scan_error(
    parent: QueuedParentCheck,
    active_error: std::io::Error,
) -> std::io::Result<ParentScanErrorResolution> {
    match parent {
        QueuedParentCheck::Active(_) => Err(active_error),
        QueuedParentCheck::Unavailable => Ok(ParentScanErrorResolution::DiscardParent),
        QueuedParentCheck::LiveTracerChanged => Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "parent generation changed live tracer authority during descendant discovery",
        )),
    }
}

fn same_descendant_scan_authority(left: TraceeSnapshot, right: TraceeSnapshot) -> bool {
    left.tgid == right.tgid
        && left.start_time == right.start_time
        && left.tracer_pid == right.tracer_pid
}

fn classify_queued_parent(
    baseline: Option<TraceeSnapshot>,
    observed: TraceeSnapshot,
    tracer_is_current: bool,
) -> QueuedParentCheck {
    if !tracer_is_current {
        QueuedParentCheck::Unavailable
    } else if baseline.map_or(true, |baseline| {
        same_descendant_scan_authority(baseline, observed)
    }) {
        QueuedParentCheck::Active(observed)
    } else {
        QueuedParentCheck::LiveTracerChanged
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TraceeGenerationState {
    Same(TraceeSnapshot),
    GoneOrReplaced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalOwnership {
    TracerOwned,
    CapturedParentOwned,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalOwnershipSample {
    Stable(TerminalOwnership),
    Changed,
}

fn require_stable_terminal_ownership(
    tid: Pid,
    sample: TerminalOwnershipSample,
) -> std::io::Result<TerminalOwnership> {
    match sample {
        TerminalOwnershipSample::Stable(ownership) => Ok(ownership),
        TerminalOwnershipSample::Changed => Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!("tracee {tid} ownership changed while sampled"),
        )),
    }
}

fn terminal_descendant_stably_released(sample: TerminalOwnershipSample) -> bool {
    sample == TerminalOwnershipSample::Stable(TerminalOwnership::Released)
}

fn terminal_ownership_permits_continuation(sample: TerminalOwnershipSample) -> bool {
    sample == TerminalOwnershipSample::Stable(TerminalOwnership::TracerOwned)
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
    generation: PhysicalEventGenerationId,
    tid: Pid,
    parent_tid: Pid,
    op: ChildOp,
}

pub(crate) struct HeldRootStop {
    terminal: TerminalCleanup,
    task_tid: Pid,
    status: HeldRootStopStatus,
    cleanup_transfer: Option<CleanupStopTransfer>,
    cleanup_lease: Option<CleanupStopLease>,
    armed: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct HeldTaskStopKey {
    task_tid: Pid,
    generation: PhysicalEventGenerationId,
}

impl HeldTaskStopKey {
    fn from_stopped(task: &Stopped) -> Self {
        let terminal = task.terminal_cleanup();
        Self::from_terminal(task.pid(), &terminal)
    }

    fn from_terminal(task_tid: Pid, terminal: &TerminalCleanup) -> Self {
        Self {
            task_tid,
            generation: terminal.physical_event_generation(),
        }
    }

    fn validates(self, held: &HeldRootStop) -> bool {
        held.task_tid == self.task_tid
            && held.terminal.physical_event_generation() == self.generation
    }
}

pub(crate) type HeldTaskStops = Arc<StdMutex<BTreeMap<HeldTaskStopKey, HeldRootStop>>>;
pub(crate) type NewbornTracees = Arc<StdMutex<BTreeMap<PhysicalEventGenerationId, NewbornTracee>>>;

#[cfg(test)]
struct CleanupCapturePreflightPause {
    captured: Arc<Barrier>,
    resume: Arc<Barrier>,
}

#[cfg(test)]
static CLEANUP_CAPTURE_PREFLIGHT_PAUSES: LazyLock<
    StdMutex<HashMap<PhysicalEventGenerationId, CleanupCapturePreflightPause>>,
> = LazyLock::new(|| StdMutex::new(HashMap::new()));

#[cfg(test)]
static CLEANUP_CAPTURE_DECODED_PAUSES: LazyLock<
    StdMutex<HashMap<PhysicalEventGenerationId, CleanupCapturePreflightPause>>,
> = LazyLock::new(|| StdMutex::new(HashMap::new()));

#[cfg(test)]
static TYPED_RETIREMENT_WAIT_PAUSES: LazyLock<
    StdMutex<HashMap<PhysicalEventGenerationId, CleanupCapturePreflightPause>>,
> = LazyLock::new(|| StdMutex::new(HashMap::new()));

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
    pub(crate) fn retire_causally_resolved_stop(
        slot: &HeldTaskStops,
        pid: Pid,
        generation: PhysicalEventGenerationId,
        resolution: StopResolutionLaterStatus,
    ) -> Result<Running, TraceError> {
        let mut held = slot.lock().unwrap();
        let key = HeldTaskStopKey {
            task_tid: pid,
            generation,
        };
        let record = held.get_mut(&key).ok_or(Errno::EALREADY)?;
        if !record.armed || !key.validates(record) {
            return Err(Errno::EINVAL.into());
        }
        let running = resolution
            .retire_transfer_and_wait(&mut record.cleanup_transfer)
            .map_err(TraceError::from)?;
        let mut record = held
            .remove(&key)
            .expect("causally resolved held stop remained present while locked");
        record.disarm();
        Ok(running)
    }

    fn status(task: &Stopped, event: &Event) -> HeldRootStopStatus {
        match event {
            Event::Signal(signal) => HeldRootStopStatus::Signal(*signal),
            Event::NewChild(op, child) => HeldRootStopStatus::NewChild(EventChildLink {
                generation: child.physical_event_generation(),
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
        }
    }

    pub(crate) fn from_event(task: &Stopped, event: &Event) -> Self {
        let status = Self::status(task, event);
        Self {
            terminal: task.terminal_cleanup(),
            task_tid: task.pid(),
            status,
            // SAFETY: HeldRootStop is the single shared cancellation shadow.
            // Normal transitions remove and discard it before returning
            // success; cleanup activates it only after the handler future and
            // its typed Stopped owner have been destroyed.
            cleanup_transfer: Some(unsafe { task.transfer_cleanup_stop() }),
            cleanup_lease: None,
            armed: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }

    /// Replaces a stale completed shadow with a newly delivered stop while the
    /// sole held-stop registry remains serialized. Live and ambiguous shadows
    /// are left untouched for their existing owner.
    fn replace_completed_shadow(
        held: &mut BTreeMap<HeldTaskStopKey, HeldRootStop>,
        task: &Stopped,
        event: &Event,
    ) -> Result<bool, TraceError> {
        Self::replace_completed_shadow_with(held, task, event, |terminal, transfer, successor| {
            terminal
                .consume_transferred_stop_completion_for_successor(transfer, successor)
                .map_err(TraceError::from)
        })
    }

    fn replace_completed_shadow_with(
        held: &mut BTreeMap<HeldTaskStopKey, HeldRootStop>,
        task: &Stopped,
        event: &Event,
        resolve: impl FnOnce(
            &TerminalCleanup,
            &mut Option<CleanupStopTransfer>,
            &TransferredStopSuccessor,
        ) -> Result<Option<TransferredStopCompletion>, TraceError>,
    ) -> Result<bool, TraceError> {
        let pid = task.pid();
        let key = HeldTaskStopKey::from_stopped(task);
        let terminal = task.terminal_cleanup();
        let Some(current) = held.get_mut(&key) else {
            return Ok(false);
        };
        if !current.armed
            || !key.validates(current)
            || current.task_tid != pid
            || !current.terminal.same_generation(&terminal)
        {
            return Err(Errno::EINVAL.into());
        }
        let successor = TransferredStopSuccessor::from_stopped(task);
        let Some(completion) =
            resolve(&current.terminal, &mut current.cleanup_transfer, &successor)?
        else {
            return Ok(false);
        };
        let mut completed = held
            .remove(&key)
            .expect("completed held stop must remain present while locked");
        completed.disarm();

        let replaced = held.insert(key, Self::from_event(task, event));
        debug_assert!(replaced.is_none(), "completed held stop was not removed");
        match completion {
            TransferredStopCompletion::Finished => Ok(true),
            // Keep the valid successor shadow durable even though the sticky
            // predecessor failure must be returned unchanged on this call.
            TransferredStopCompletion::Failed(error) => Err(error.into()),
        }
    }

    pub(crate) fn arm_empty(
        slot: &HeldTaskStops,
        task: &Stopped,
        event: &Event,
    ) -> Result<(), TraceError> {
        Self::arm_empty_with(slot, task, event, || {})
    }

    pub(crate) fn arm_empty_with(
        slot: &HeldTaskStops,
        task: &Stopped,
        event: &Event,
        on_committed: impl FnOnce(),
    ) -> Result<(), TraceError> {
        Self::arm_empty_with_resolver(
            slot,
            task,
            event,
            on_committed,
            |terminal, transfer, successor| {
                terminal
                    .consume_transferred_stop_completion_for_successor(transfer, successor)
                    .map_err(TraceError::from)
            },
        )
    }

    fn arm_empty_with_resolver(
        slot: &HeldTaskStops,
        task: &Stopped,
        event: &Event,
        on_committed: impl FnOnce(),
        resolve: impl FnOnce(
            &TerminalCleanup,
            &mut Option<CleanupStopTransfer>,
            &TransferredStopSuccessor,
        ) -> Result<Option<TransferredStopCompletion>, TraceError>,
    ) -> Result<(), TraceError> {
        let mut held = slot.lock().unwrap();
        let key = HeldTaskStopKey::from_stopped(task);
        if held.contains_key(&key) {
            let predecessor_stop_id = held
                .get(&key)
                .and_then(|current| current.cleanup_transfer.as_ref())
                .map(CleanupStopTransfer::logical_stop_id);
            let result = Self::replace_completed_shadow_with(&mut held, task, event, resolve);
            let successor_installed = held
                .get(&key)
                .is_some_and(|current| Self::matches_current(current, task, event))
                && predecessor_stop_id.is_some_and(|predecessor| {
                    task.logical_stop_id().is_strictly_after(predecessor)
                });
            if matches!(&result, Ok(true)) || (result.is_err() && successor_installed) {
                on_committed();
            }
            return match result {
                Ok(true) => Ok(()),
                Ok(false) => Err(Errno::EINVAL.into()),
                Err(error) => Err(error),
            };
        }
        held.insert(key, Self::from_event(task, event));
        on_committed();
        Ok(())
    }

    fn matches_current(current: &HeldRootStop, task: &Stopped, event: &Event) -> bool {
        let terminal = task.terminal_cleanup();
        current.armed
            && current.task_tid == task.pid()
            && current.terminal.same_generation(&terminal)
            && current.status == Self::status(task, event)
            && current.cleanup_transfer.as_ref().is_some_and(|transfer| {
                transfer.logical_stop_id() == task.logical_stop_id()
                    && transfer.physical_status_id() == task.physical_status_id()
            })
    }

    pub(crate) fn ensure_current_with(
        slot: &HeldTaskStops,
        task: &Stopped,
        event: &Event,
        on_committed: impl FnOnce(),
    ) -> Result<(), TraceError> {
        let mut held = slot.lock().unwrap();
        let key = HeldTaskStopKey::from_stopped(task);
        let exact_current = held
            .get(&key)
            .is_some_and(|current| Self::matches_current(current, task, event));
        if held.contains_key(&key) {
            let predecessor_stop_id = held
                .get(&key)
                .and_then(|current| current.cleanup_transfer.as_ref())
                .map(CleanupStopTransfer::logical_stop_id);
            let result = Self::replace_completed_shadow(&mut held, task, event);
            let successor_installed = held
                .get(&key)
                .is_some_and(|current| Self::matches_current(current, task, event))
                && predecessor_stop_id.is_some_and(|predecessor| {
                    task.logical_stop_id().is_strictly_after(predecessor)
                });
            if matches!(&result, Ok(true))
                || (exact_current && matches!(&result, Ok(false)))
                || (result.is_err() && successor_installed)
            {
                on_committed();
            }
            return match result {
                Ok(true) => Ok(()),
                Ok(false) if exact_current => Ok(()),
                Ok(false) => Err(Errno::EINVAL.into()),
                Err(error) => Err(error),
            };
        }
        match held.get(&key) {
            None => {
                held.insert(key, Self::from_event(task, event));
                on_committed();
                Ok(())
            }
            Some(_) => unreachable!("occupied held stop handled before vacant insertion"),
        }
    }

    pub(crate) fn supersede_with_exit(
        slot: &HeldTaskStops,
        task: &Stopped,
    ) -> Result<(), TraceError> {
        let terminal = task.terminal_cleanup();
        let key = HeldTaskStopKey::from_stopped(task);
        let replacement_status = task.physical_status_id();
        let successor = TransferredStopSuccessor::from_stopped(task);
        let mut held = slot.lock().unwrap();
        if let Some(current) = held.get_mut(&key) {
            if !current.armed
                || !key.validates(current)
                || current.task_tid != task.pid()
                || !current.terminal.same_generation(&terminal)
            {
                return Err(Errno::EINVAL.into());
            }
            let transfer = current.cleanup_transfer.as_ref().ok_or(Errno::EINVAL)?;
            let old_stop_id = transfer.logical_stop_id();
            let old_status = transfer.physical_status_id();
            if old_stop_id != task.logical_stop_id()
                && old_status.is_some()
                && old_status == replacement_status
            {
                return Err(Errno::EPROTO.into());
            }
            if let Some(completion) = current
                .terminal
                .classify_transferred_stop_for_supersession(
                    &mut current.cleanup_transfer,
                    &successor,
                )?
            {
                let mut completed = held
                    .remove(&key)
                    .expect("classified held stop must remain present while locked");
                completed.disarm();
                let replaced = held.insert(key, Self::from_event(task, &Event::Exit));
                debug_assert!(replaced.is_none(), "classified held stop was not removed");
                return match completion {
                    TransferredStopCompletion::Finished => Ok(()),
                    TransferredStopCompletion::Failed(error) => Err(error.into()),
                };
            }
        }
        match held.get(&key) {
            None => {
                held.insert(key, Self::from_event(task, &Event::Exit));
                Ok(())
            }
            Some(current)
                if current.armed
                    && current.task_tid == task.pid()
                    && current.terminal.same_generation(&terminal) =>
            {
                if current.status == HeldRootStopStatus::Exit
                    && current.cleanup_transfer.as_ref().is_some_and(|transfer| {
                        transfer.logical_stop_id() == task.logical_stop_id()
                            && transfer.physical_status_id() == replacement_status
                    })
                {
                    return Ok(());
                }
                if current
                    .cleanup_transfer
                    .as_ref()
                    .is_some_and(|transfer| transfer.logical_stop_id() == task.logical_stop_id())
                {
                    return Err(Errno::EPROTO.into());
                }
                let current_status = current
                    .cleanup_transfer
                    .as_ref()
                    .ok_or(Errno::EINVAL)?
                    .physical_status_id();
                if current_status != replacement_status
                    && let Some(status) = current_status
                {
                    task.physical_event_observer()
                        .ok_or(Errno::EPROTO)?
                        .finish_status(
                            status,
                            PhysicalStatusDisposition::KernelSupersededByExitStop,
                        );
                }
                held.insert(key, Self::from_event(task, &Event::Exit));
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
    held_task_stops: Option<HeldTaskStops>,
}

impl RootStopLease {
    pub(crate) fn new(task: Stopped, held_task_stops: Option<HeldTaskStops>) -> Self {
        Self {
            task: Some(task),
            held_task_stops,
        }
    }

    fn transition(
        &mut self,
        operation: impl FnOnce(Stopped) -> Result<Running, TraceError>,
    ) -> Result<Running, TraceError> {
        let mut task = self.task.take().expect("root stop lease consumed once");
        if let Some(slot) = self.held_task_stops.as_ref() {
            let mut held = slot.lock().unwrap();
            let task_pid = task.pid();
            let current = task.terminal_cleanup();
            let key = HeldTaskStopKey::from_terminal(task_pid, &current);
            let Some(record) = held.get(&key) else {
                return Err(Errno::EINVAL.into());
            };
            if !key.validates(record)
                || record.task_tid != task_pid
                || !record.armed
                || !record.terminal.same_generation(&current)
                || !record.cleanup_transfer.as_ref().is_some_and(|transfer| {
                    transfer.logical_stop_id() == task.logical_stop_id()
                        && transfer.physical_status_id() == task.physical_status_id()
                })
            {
                return Err(Errno::EINVAL.into());
            }
            if let Some(status) = record
                .cleanup_transfer
                .as_ref()
                .and_then(CleanupStopTransfer::physical_status_id)
            {
                // SAFETY: the validated held-stop record remains in the shared
                // map unless the ptrace transition succeeds. On failure,
                // whole-session cleanup owns this exact TerminalCleanup and
                // physical status until it records CancellationCleanup.
                unsafe {
                    task.retain_failed_resume_disposition_for_cleanup(&record.terminal, status)?
                };
            }
            let result = operation(task);
            if result.is_ok() {
                let mut record = held
                    .remove(&key)
                    .expect("validated held stop must remain present while locked");
                record.disarm();
            }
            return result;
        }
        operation(task)
    }

    pub(crate) fn resume<T: Into<Option<Signal>>>(
        mut self,
        signal: T,
    ) -> Result<Running, TraceError> {
        self.transition(|task| task.resume(signal))
    }

    pub(crate) fn resume_with_physical_attempt<T: Into<Option<Signal>>>(
        mut self,
        signal: T,
    ) -> Result<
        (Running, PhysicalResumeAttempt),
        (TraceError, Option<PhysicalResumeAttempt>, Option<Errno>),
    > {
        let mut task = self.task.take().expect("root stop lease consumed once");
        let task_pid = task.pid();
        let transition = |task: Stopped| task.resume_with_physical_attempt(signal);
        if let Some(slot) = self.held_task_stops.as_ref() {
            let mut held = slot.lock().unwrap();
            let current = task.terminal_cleanup();
            let key = HeldTaskStopKey::from_terminal(task_pid, &current);
            let Some(record) = held.get(&key) else {
                return Err((Errno::EINVAL.into(), None, None));
            };
            if !key.validates(record)
                || record.task_tid != task_pid
                || !record.armed
                || !record.terminal.same_generation(&current)
                || !record.cleanup_transfer.as_ref().is_some_and(|transfer| {
                    transfer.logical_stop_id() == task.logical_stop_id()
                        && transfer.physical_status_id() == task.physical_status_id()
                })
            {
                return Err((Errno::EINVAL.into(), None, None));
            }
            if let Some(status) = record
                .cleanup_transfer
                .as_ref()
                .and_then(CleanupStopTransfer::physical_status_id)
            {
                // SAFETY: the held transfer remains durable on raw failure and
                // is retired only by the exact causal-successor protocol.
                unsafe {
                    task.retain_failed_resume_disposition_for_cleanup(&record.terminal, status)
                        .map_err(|error| (error.into(), None, None))?;
                }
            }
            return match transition(task) {
                Ok(success) => {
                    let mut record = held
                        .remove(&key)
                        .expect("validated held stop must remain present while locked");
                    record.disarm();
                    Ok(success)
                }
                Err(failure) => Err((
                    failure.error().into(),
                    failure.attempt(),
                    Some(failure.error()),
                )),
            };
        }
        transition(task).map_err(|failure| {
            (
                failure.error().into(),
                failure.attempt(),
                Some(failure.error()),
            )
        })
    }

    pub(crate) fn step<T: Into<Option<Signal>>>(
        mut self,
        signal: T,
    ) -> Result<Running, TraceError> {
        self.transition(|task| task.step(signal))
    }

    pub(crate) fn syscall<T: Into<Option<Signal>>>(
        mut self,
        signal: T,
    ) -> Result<Running, TraceError> {
        self.transition(|task| task.syscall(signal))
    }

    pub(crate) fn detach<T: Into<Option<Signal>>>(
        mut self,
        signal: T,
    ) -> Result<Running, TraceError> {
        self.transition(|task| task.detach(signal))
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
    fn identities_match(left: &TraceeIdentity, right: &TraceeIdentity) -> bool {
        left.tid == right.tid
            && left.snapshot.tgid == right.snapshot.tgid
            && left.snapshot.start_time == right.snapshot.start_time
            && left.proc_inode == right.proc_inode
            && left.parent == right.parent
    }

    pub(crate) fn from_event(parent_tid: Pid, op: ChildOp, task: &Running) -> Self {
        let generation = task.physical_event_generation();
        Self {
            link: EventChildLink {
                generation,
                tid: task.pid(),
                parent_tid,
                op,
            },
            identity: None,
            terminal: task.terminal_cleanup(),
        }
    }

    fn generation(&self) -> PhysicalEventGenerationId {
        self.terminal.physical_event_generation()
    }

    fn validates_key(&self, generation: PhysicalEventGenerationId) -> bool {
        self.link.generation == generation && self.generation() == generation
    }

    pub(crate) fn same_event(&self, parent_tid: Pid, op: ChildOp, task: &Running) -> bool {
        self.validates_key(task.physical_event_generation())
            && self.link.tid == task.pid()
            && self.link.parent_tid == parent_tid
            && self.link.op == op
            && self.terminal.same_generation(&task.terminal_cleanup())
    }

    pub(crate) fn register_event(
        newborns: &mut BTreeMap<PhysicalEventGenerationId, Self>,
        parent_tid: Pid,
        op: ChildOp,
        task: &Running,
    ) -> Result<PhysicalEventGenerationId, Errno> {
        use std::collections::btree_map::Entry;

        let generation = task.physical_event_generation();
        match newborns.entry(generation) {
            Entry::Vacant(entry) => {
                entry.insert(Self::from_event(parent_tid, op, task));
                Ok(generation)
            }
            Entry::Occupied(entry) if entry.get().same_event(parent_tid, op, task) => {
                Ok(generation)
            }
            Entry::Occupied(_) => Err(Errno::EPROTO),
        }
    }

    fn restore_removed(
        newborns: &mut BTreeMap<PhysicalEventGenerationId, Self>,
        generation: PhysicalEventGenerationId,
        mut newborn: Self,
    ) -> std::io::Result<()> {
        use std::collections::btree_map::Entry;

        if !newborn.validates_key(generation) {
            tracing::error!(
                ?generation,
                "refusing to drop a removed newborn whose generation key changed"
            );
            std::process::abort();
        }
        match newborns.entry(generation) {
            Entry::Vacant(entry) => {
                entry.insert(newborn);
            }
            Entry::Occupied(mut entry)
                if entry.get().validates_key(generation)
                    && entry.get().link == newborn.link
                    && entry.get().terminal.same_generation(&newborn.terminal) =>
            {
                match (entry.get().identity.as_ref(), newborn.identity.take()) {
                    (None, Some(identity)) => entry.get_mut().identity = Some(identity),
                    (Some(current), Some(identity))
                        if Self::identities_match(current, &identity) => {}
                    (Some(_), None) | (None, None) => {}
                    (Some(_), Some(_)) => {
                        tracing::error!(
                            ?generation,
                            "conflicting exact-generation newborn identities during restoration"
                        );
                        std::process::abort();
                    }
                }
            }
            Entry::Occupied(_) => {
                tracing::error!(
                    ?generation,
                    "conflicting newborn owner appeared during restoration"
                );
                std::process::abort();
            }
        }
        Ok(())
    }

    pub(crate) fn set_identity(&mut self, identity: TraceeIdentity) -> Result<(), Errno> {
        if !self.validates_key(self.generation())
            || identity.tid != self.link.tid
            || !identity.parent.is_some_and(|(parent_tid, _, op)| {
                parent_tid == self.link.parent_tid && op == Some(self.link.op)
            })
        {
            return Err(Errno::EPROTO);
        }
        if self.identity.is_some() {
            return Err(Errno::EALREADY);
        }
        self.identity = Some(identity);
        Ok(())
    }

    pub(crate) fn registration_error(&self) -> Option<Errno> {
        self.terminal.registration_error()
    }

    pub(crate) fn terminate_vfork_child(&self) -> Result<(), TraceError> {
        self.identity.as_ref().ok_or(Errno::ESRCH)?;
        match self.terminal.send_sigkill_for_cleanup() {
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
            let state = reservation.decode_guard()?.commit();
            let Wait::Stopped(stopped, _) = state else {
                continue;
            };
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
    fn matches_terminal_task(&self, terminal: &TerminalCleanup) -> bool {
        let physical = terminal.physical_task_identity();
        physical.tid() == self.tid.as_raw()
            && physical.tgid() == Some(self.snapshot.tgid.as_raw())
            && physical.start_time() == Some(self.snapshot.start_time)
            && physical.proc_inode() == Some(self.proc_inode)
    }

    pub(crate) fn open_root(pid: Pid) -> Result<Self, Errno> {
        let identity = Self::capture(pid, None, false)?;
        if identity.tid == identity.snapshot.tgid && identity.pidfd.is_some() {
            Ok(identity)
        } else {
            Err(Errno::ESRCH)
        }
    }

    fn from_original_root_startup(
        proof: OriginalRootStartupIdentity,
    ) -> (Self, PhysicalEventGenerationId) {
        let (generation, tid, tgid, ppid, tracer_pid, start_time, proc_inode, pidfd, proc_dir) =
            proof.into_parts();
        (
            Self {
                tid,
                snapshot: TraceeSnapshot {
                    tgid,
                    ppid,
                    tracer_pid,
                    start_time,
                },
                proc_dir,
                proc_inode,
                pidfd: Some(pidfd),
                parent: None,
            },
            generation,
        )
    }

    pub(crate) fn capture_event_child(
        tid: Pid,
        parent_tid: Pid,
        op: ChildOp,
    ) -> Result<Self, Errno> {
        // PTRACE_GETEVENTMSG is the authoritative parent-child ownership edge.
        // CLONE_PARENT intentionally makes PPid disagree with the event parent.
        Self::capture(tid, Some((parent_tid, Some(op))), false)
    }

    fn open_task_tid(tid: Pid, root_tgid: Pid) -> std::io::Result<Option<Self>> {
        let identity = match Self::capture(tid, None, false) {
            Ok(identity) => identity,
            Err(error) => {
                if checked_tracee_open_absence(tid, error)? {
                    return Ok(None);
                }
                unreachable!("non-absence open errors return Err")
            }
        };
        if tid == root_tgid
            || identity.snapshot.tgid != root_tgid
            || identity.checked_terminal_ownership()? != TerminalOwnership::TracerOwned
        {
            return Ok(None);
        }
        Ok(Some(identity))
    }

    fn open_discovered(tid: Pid, parent_tid: Pid) -> std::io::Result<Option<Self>> {
        let identity = match Self::capture(tid, Some((parent_tid, None)), true) {
            Ok(identity) => identity,
            Err(error) => {
                if checked_tracee_open_absence(tid, error)? {
                    return Ok(None);
                }
                unreachable!("non-absence open errors return Err")
            }
        };

        // Re-read the kernel children relationship after opening the procfs
        // identity and pidfd. A list/open race may otherwise bind a replacement
        // tracee that reused the numeric child PID.
        if !direct_children(parent_tid)?.contains(&tid) {
            if identity.checked_terminal_ownership()? == TerminalOwnership::Released {
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
        if before != after
            || current_inode != proc_inode
            || !checked_tracer_is_current(after.tracer_pid).map_err(io_errno)?
        {
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

    pub(crate) fn send_signal(&self, signal: Signal) -> Result<(), Errno> {
        self.send_raw_signal(signal as i32)
    }

    #[cfg(test)]
    fn same_process(&self) -> bool {
        matches!(
            self.checked_generation(),
            Ok(TraceeGenerationState::Same(_))
        )
    }

    #[cfg(test)]
    fn is_our_tracee(&self) -> bool {
        matches!(
            self.checked_terminal_ownership(),
            Ok(TerminalOwnership::TracerOwned)
        )
    }

    fn checked_generation(&self) -> std::io::Result<TraceeGenerationState> {
        if fd_inode(&self.proc_dir)? != self.proc_inode {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "saved procfs descriptor for tracee {} changed immutable inode",
                    self.tid
                ),
            ));
        }
        let before_inode = match proc_path_inode(self.tid) {
            Ok(Some(inode)) if inode == self.proc_inode => inode,
            Ok(_) => return Ok(TraceeGenerationState::GoneOrReplaced),
            Err(error) => return Err(error),
        };
        let current = match tracee_snapshot(self.tid) {
            Ok(snapshot) => snapshot,
            Err(error) if process_gone_error(&error) => {
                return reconcile_snapshot_failure(
                    self.tid,
                    self.proc_inode,
                    proc_path_inode(self.tid)?,
                    error,
                );
            }
            Err(error) => return Err(error),
        };
        let after_inode = match proc_path_inode(self.tid)? {
            Some(inode) => inode,
            None => return Ok(TraceeGenerationState::GoneOrReplaced),
        };
        if before_inode != after_inode
            || after_inode != self.proc_inode
            || current.tgid != self.snapshot.tgid
            || current.start_time != self.snapshot.start_time
        {
            return Ok(TraceeGenerationState::GoneOrReplaced);
        }
        Ok(TraceeGenerationState::Same(current))
    }

    fn checked_terminal_ownership_sample(&self) -> std::io::Result<TerminalOwnershipSample> {
        self.checked_terminal_ownership_sample_with(
            || self.checked_generation(),
            checked_tracer_is_current,
        )
    }

    fn checked_terminal_ownership_sample_with(
        &self,
        mut checked_generation: impl FnMut() -> std::io::Result<TraceeGenerationState>,
        tracer_is_current: impl FnOnce(Pid) -> std::io::Result<bool>,
    ) -> std::io::Result<TerminalOwnershipSample> {
        let TraceeGenerationState::Same(before) = checked_generation()? else {
            return Ok(TerminalOwnershipSample::Stable(TerminalOwnership::Released));
        };
        let tracer_owned = tracer_is_current(before.tracer_pid)?;
        let TraceeGenerationState::Same(after) = checked_generation()? else {
            return Ok(TerminalOwnershipSample::Stable(TerminalOwnership::Released));
        };
        if before != after {
            return Ok(TerminalOwnershipSample::Changed);
        }
        if tracer_owned {
            return Ok(TerminalOwnershipSample::Stable(
                TerminalOwnership::TracerOwned,
            ));
        }
        if self.parent.is_some() && after.ppid == self.snapshot.ppid {
            return Ok(TerminalOwnershipSample::Stable(
                TerminalOwnership::CapturedParentOwned,
            ));
        }
        Ok(TerminalOwnershipSample::Stable(TerminalOwnership::Released))
    }

    fn checked_terminal_ownership(&self) -> std::io::Result<TerminalOwnership> {
        require_stable_terminal_ownership(self.tid, self.checked_terminal_ownership_sample()?)
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
    frozen_stop: Option<CleanupStopLease>,
    /// True only when the terminal Event's captured stable task identity
    /// matches `identity`. A provisional notifier generation retained across
    /// a PID-reuse race remains observable but can never signal or continue
    /// the replacement.
    signal_authority: bool,
}

impl RegisteredTraceeCleanup {
    fn insert_exact(
        descendants: &mut BTreeMap<PhysicalEventGenerationId, Self>,
        generation: PhysicalEventGenerationId,
        tracee: Self,
    ) {
        use std::collections::btree_map::Entry;

        if tracee.terminal.physical_event_generation() != generation || tracee.frozen_stop.is_some()
        {
            tracing::error!(
                ?generation,
                "refusing to lose a mis-keyed or frozen discovered cleanup owner"
            );
            std::process::abort();
        }
        match descendants.entry(generation) {
            Entry::Vacant(entry) => {
                entry.insert(tracee);
            }
            Entry::Occupied(mut entry)
                if entry.get().terminal.same_generation(&tracee.terminal)
                    && entry.get().event_link == tracee.event_link
                    && NewbornTracee::identities_match(&entry.get().identity, &tracee.identity) =>
            {
                entry.get_mut().signal_authority |= tracee.signal_authority;
            }
            Entry::Occupied(_) => {
                tracing::error!(
                    ?generation,
                    "conflicting registered cleanup owner appeared during discovery"
                );
                std::process::abort();
            }
        }
    }

    fn continue_exit_stop(&mut self) -> std::io::Result<()> {
        if !self.signal_authority {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "descendant cleanup lacks exact signal/ptrace authority",
            ));
        }
        continue_registered_exit_stop(
            &self.terminal,
            &mut self.frozen_stop,
            PhysicalResumeOwner::DescendantCleanup,
        )
    }

    fn send_sigkill(&self) -> std::io::Result<()> {
        if !self.signal_authority {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "descendant cleanup lacks exact signal authority",
            ));
        }
        send_identity_sigkill(&self.identity, &self.terminal)
    }

    fn refresh_signal_authority(&mut self) -> std::io::Result<()> {
        let identity = &self.identity;
        let terminal = &self.terminal;
        Self::refresh_signal_authority_with(
            &mut self.signal_authority,
            || identity.checked_generation(),
            || {
                terminal
                    .ensure_registered()
                    .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))
            },
            || identity.matches_terminal_task(terminal),
        )
    }

    fn refresh_signal_authority_with(
        signal_authority: &mut bool,
        mut checked_generation: impl FnMut() -> std::io::Result<TraceeGenerationState>,
        ensure_registered: impl FnOnce() -> std::io::Result<()>,
        matches_terminal_task: impl FnOnce() -> bool,
    ) -> std::io::Result<()> {
        if *signal_authority {
            return Ok(());
        }
        let TraceeGenerationState::Same(before) = checked_generation()? else {
            return Ok(());
        };
        ensure_registered()?;
        if !matches_terminal_task() {
            return Ok(());
        }
        if matches!(
            checked_generation()?,
            TraceeGenerationState::Same(after) if after == before
        ) {
            *signal_authority = true;
        }
        Ok(())
    }
}

#[cfg(test)]
fn terminal_descendant_remains_owned(identity: &TraceeIdentity) -> bool {
    !matches!(
        identity.checked_terminal_ownership(),
        Ok(TerminalOwnership::Released)
    )
}

fn retain_cleanup_stop(
    retained: &mut Option<CleanupStopLease>,
    observed: Option<CleanupStopLease>,
) -> std::io::Result<()> {
    let Some(observed) = observed else {
        return Ok(());
    };
    match retained {
        None => {
            *retained = Some(observed);
            Ok(())
        }
        Some(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "more than one unconsumed logical stop reached cancellation cleanup",
        )),
    }
}

fn finish_cancelled_stop(
    terminal: &TerminalCleanup,
    stop: Option<CleanupStopLease>,
) -> std::io::Result<()> {
    if let Some(stop) = stop {
        terminal
            .dispose_cleanup_stop(stop)
            .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
    }
    Ok(())
}

fn continue_registered_exit_stop(
    terminal: &TerminalCleanup,
    frozen_stop: &mut Option<CleanupStopLease>,
    owner: PhysicalResumeOwner,
) -> std::io::Result<()> {
    if terminal
        .continue_external_startup_cleanup()
        .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?
    {
        return Ok(());
    }
    let wait_failure_cleanup = terminal.terminal_error().is_some();
    match terminal
        .continue_exit_stop_for_cleanup(frozen_stop, owner)
        .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?
    {
        TerminalCleanupContinue::WaitingForExitStop => {}
        TerminalCleanupContinue::AlreadyFinished { .. } => {}
        TerminalCleanupContinue::ControllerHandoffCompleted { .. } => {}
        TerminalCleanupContinue::Attempted { error, .. } => {
            if wait_failure_cleanup
                && error.is_none()
                && let Some(stop) = frozen_stop.take()
            {
                finish_cancelled_stop(terminal, Some(stop))?;
            }
        }
    }
    Ok(())
}

fn continue_frozen_exit_stop(
    terminal: &TerminalCleanup,
    frozen_stop: &mut Option<CleanupStopLease>,
    owner: PhysicalResumeOwner,
) -> std::io::Result<()> {
    continue_registered_exit_stop(terminal, frozen_stop, owner)
}

impl LiteinstTraceeCleanup {
    fn observation_only_retirement_ready(
        worker_done: bool,
        pending_empty: bool,
        registration_error: Option<Errno>,
        terminal_error: Option<Errno>,
        has_frozen_stop: bool,
        checked_ownership: impl FnOnce() -> std::io::Result<TerminalOwnership>,
    ) -> std::io::Result<bool> {
        if !worker_done {
            return Ok(false);
        }
        if !pending_empty
            || registration_error.is_some()
            || terminal_error.is_some()
            || has_frozen_stop
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "completed quarantine lacks clean observation-only proof",
            ));
        }
        Ok(checked_ownership()? == TerminalOwnership::Released)
    }

    fn rekey_resolved_descendants(
        descendants: &mut BTreeMap<PhysicalEventGenerationId, RegisteredTraceeCleanup>,
    ) -> std::io::Result<()> {
        // Validate every pre-rekey edge before moving any record. A later bad
        // edge must not leave an earlier generation rekeyed and therefore make
        // rollback depend on iteration order.
        for (old_generation, tracee) in descendants.iter() {
            if let Some(link) = tracee.event_link
                && link.generation != *old_generation
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "event edge generation {:?} does not match its pre-rekey key {old_generation:?}",
                        link.generation,
                    ),
                ));
            }
        }
        let keys = descendants.keys().copied().collect::<Vec<_>>();
        for old_generation in keys {
            let Some(current) = descendants.get(&old_generation) else {
                continue;
            };
            let resolved_generation = current.terminal.physical_event_generation();
            if resolved_generation == old_generation {
                continue;
            }
            if let Some(existing) = descendants.get(&resolved_generation) {
                let mut moving_link = current.event_link;
                if let Some(link) = moving_link.as_mut() {
                    link.generation = resolved_generation;
                }
                if !existing.terminal.same_generation(&current.terminal)
                    || !NewbornTracee::identities_match(&existing.identity, &current.identity)
                    || existing
                        .event_link
                        .zip(moving_link)
                        .is_some_and(|(left, right)| left != right)
                    || (existing.frozen_stop.is_some() && current.frozen_stop.is_some())
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "resolved generation {resolved_generation:?} conflicts with provisional key {old_generation:?}"
                        ),
                    ));
                }
            }

            let mut moving = descendants
                .remove(&old_generation)
                .expect("preflighted provisional descendant remains present");
            if let Some(link) = moving.event_link.as_mut() {
                link.generation = resolved_generation;
            }
            match descendants.entry(resolved_generation) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(moving);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    if existing.event_link.is_none() {
                        existing.event_link = moving.event_link;
                    }
                    if existing.frozen_stop.is_none() {
                        existing.frozen_stop = moving.frozen_stop.take();
                    }
                    existing.signal_authority |= moving.signal_authority;
                }
            }
        }
        Ok(())
    }

    fn retire_released_quarantine(
        descendants: &mut BTreeMap<PhysicalEventGenerationId, RegisteredTraceeCleanup>,
    ) -> std::io::Result<()> {
        let mut released = Vec::new();
        for (generation, tracee) in descendants
            .iter()
            .filter(|(_, tracee)| !tracee.signal_authority)
        {
            if tracee.terminal.physical_event_generation() != *generation {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("quarantined descendant has stale key {generation:?}"),
                ));
            }
            let ready = Self::observation_only_retirement_ready(
                tracee.terminal.wait(Duration::ZERO),
                tracee.terminal.pending_is_empty(),
                tracee.terminal.registration_error(),
                tracee.terminal.terminal_error(),
                tracee.frozen_stop.is_some(),
                || tracee.identity.checked_terminal_ownership(),
            )
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "quarantined descendant {} generation {generation:?} completed without clean observation-only proof: {error}",
                        tracee.identity.tid
                    ),
                )
            })?;
            if ready {
                released.push(*generation);
            }
        }
        for generation in released {
            descendants.remove(&generation);
        }
        Ok(())
    }

    fn new_after_loader(
        task: &Running,
        startup_identity: OriginalRootStartupIdentity,
        newborn_tracees: NewbornTracees,
        held_task_stops: HeldTaskStops,
    ) -> Self {
        debug_assert_eq!(startup_identity.pid(), task.pid());
        debug_assert_eq!(
            startup_identity.generation(),
            task.physical_event_generation()
        );
        let (identity, startup_generation) =
            TraceeIdentity::from_original_root_startup(startup_identity);
        debug_assert_eq!(identity.tid, task.pid());
        debug_assert_eq!(startup_generation, task.physical_event_generation());
        let physical_observer = task.physical_event_observer();
        Self {
            identity,
            physical_observer,
            exact_controller_startup: true,
            newborn_tracees,
            armed: true,
            unstarted_terminal: Some(unsafe { task.unregistered_terminal_cleanup() }),
            terminal: None,
            notifier_owner: None,
            retained_descendants: BTreeMap::new(),
            retained_terminal_descendants: BTreeMap::new(),
            held_task_stops,
            root_frozen: false,
            root_frozen_stop: None,
            #[cfg(test)]
            fail_discovery_once: None,
            #[cfg(test)]
            fail_after_scan_once: None,
            #[cfg(test)]
            force_task_scan_once: None,
        }
    }

    fn new_dynamic(
        task: &Running,
        newborn_tracees: NewbornTracees,
        held_task_stops: HeldTaskStops,
    ) -> Result<Self, Errno> {
        Ok(Self {
            identity: TraceeIdentity::open_root(task.pid())?,
            physical_observer: None,
            exact_controller_startup: false,
            newborn_tracees,
            armed: true,
            unstarted_terminal: Some(unsafe { task.unregistered_terminal_cleanup() }),
            terminal: None,
            notifier_owner: None,
            retained_descendants: BTreeMap::new(),
            retained_terminal_descendants: BTreeMap::new(),
            held_task_stops,
            root_frozen: false,
            root_frozen_stop: None,
            #[cfg(test)]
            fail_discovery_once: None,
            #[cfg(test)]
            fail_after_scan_once: None,
            #[cfg(test)]
            force_task_scan_once: None,
        })
    }

    fn pid(&self) -> Pid {
        self.identity.tid
    }

    fn ready_for_observer_close(&self) -> bool {
        !self.armed && self.held_task_stops.lock().unwrap().is_empty()
    }

    fn register_notifier(&mut self, task: &Running) -> Result<(), Errno> {
        debug_assert!(self.terminal.is_none());
        let terminal = self
            .unstarted_terminal
            .take()
            .expect("unstarted LiteInst cleanup handle was already consumed");
        debug_assert_eq!(
            terminal.physical_event_generation(),
            task.physical_event_generation()
        );
        if let Err(error) = terminal.ensure_registered() {
            self.unstarted_terminal = Some(terminal);
            return Err(error);
        }
        self.notifier_owner = Some(std::thread::current().id());
        self.terminal = Some(terminal);
        Ok(())
    }

    fn capture_pending_children(
        newborn_tracees: &NewbornTracees,
        held_task_stops: &HeldTaskStops,
        terminal: &TerminalCleanup,
        retained_stop: &mut Option<CleanupStopLease>,
    ) -> std::io::Result<()> {
        // A second FIFO front cannot be represented by the one-stop cleanup
        // protocol. Leave it uncommitted until the retained lease is resolved.
        if retained_stop.is_some() {
            return Ok(());
        }
        #[cfg(test)]
        if let Some(pause) = CLEANUP_CAPTURE_PREFLIGHT_PAUSES
            .lock()
            .unwrap()
            .remove(&terminal.physical_event_generation())
        {
            pause.captured.wait();
            pause.resume.wait();
        }
        loop {
            let Some(reservation) = terminal.reserve_pending_for_cleanup(Duration::ZERO) else {
                terminal
                    .reclaim_available_cleanup_stop(retained_stop)
                    .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
                return Ok(());
            };
            let decoded = reservation.decode_guard().map_err(|error| {
                std::io::Error::other(format!("decode queued cancellation state: {error}"))
            })?;
            #[cfg(test)]
            if let Some(pause) = CLEANUP_CAPTURE_DECODED_PAUSES
                .lock()
                .unwrap()
                .remove(&terminal.physical_event_generation())
            {
                pause.captured.wait();
                pause.resume.wait();
            }
            // Hold the sole shadow-owner registry from preflight through FIFO
            // commit and capability conversion. A competing shadow therefore
            // leaves the reservation uncommitted, while a conversion error can
            // always retain the returned Stopped without a post-commit race.
            // Decoding retains the notifier StatusState lock. Existing terminal
            // cleanup sometimes takes held_task_stops before consulting that
            // state, so do not wait in the reverse order: contention rolls the
            // decoded reservation back without committing it.
            let (decoded, mut held) = match held_task_stops.try_lock() {
                Ok(held) => (decoded, held),
                Err(std::sync::TryLockError::WouldBlock) => {
                    // Release and roll back StatusState before waiting in the
                    // established held->StatusState order, then reacquire the same
                    // FIFO front while the held registry is serialized.
                    drop(decoded);
                    let held = held_task_stops.lock().unwrap();
                    let Some(reservation) = terminal.reserve_pending_for_cleanup(Duration::ZERO)
                    else {
                        return Ok(());
                    };
                    let decoded = reservation.decode_guard().map_err(|error| {
                        std::io::Error::other(format!(
                            "decode rolled-back cancellation state: {error}"
                        ))
                    })?;
                    (decoded, held)
                }
                Err(std::sync::TryLockError::Poisoned(error)) => panic!("{error}"),
            };
            let stopped_key = match decoded.wait() {
                Wait::Stopped(stopped, _) => HeldTaskStopKey::from_stopped(stopped),
                Wait::Exited(..) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "nonterminal notifier FIFO decoded a terminal state",
                    ));
                }
            };
            let stopped_pid = stopped_key.task_tid;
            if stopped_key.generation != terminal.physical_event_generation() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "decoded stop for {stopped_pid} belongs to generation {:?}, not {:?}",
                        stopped_key.generation,
                        terminal.physical_event_generation(),
                    ),
                ));
            }
            if let Some(shadow) = held.get(&stopped_key) {
                if !stopped_key.validates(shadow) || !shadow.terminal.same_generation(terminal) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "held stop for {stopped_pid} does not match generation {:?}",
                            stopped_key.generation,
                        ),
                    ));
                }
                // Destroy the duplicate decoded capability and roll its FIFO front
                // back before activating the already-durable shadow. This is
                // progress, not a cleanup failure; the next pass can consume the
                // untouched FIFO successor after resolving the retained owner.
                drop(decoded);
                drop(held);
                if let Some(held_stop) =
                    Self::take_held_task_stop_from(held_task_stops, stopped_pid, terminal)?
                {
                    retain_cleanup_stop(retained_stop, held_stop.cleanup_lease)?;
                    return Ok(());
                }
                // A completed predecessor shadow was consumed with zero ptrace
                // transition. Retry the rolled-back FIFO front in this call so the
                // already-published successor cannot remain stranded.
                continue;
            }
            if let Wait::Stopped(stopped, Event::NewChild(op, child)) = decoded.wait() {
                NewbornTracee::register_event(
                    &mut newborn_tracees.lock().unwrap(),
                    stopped.pid(),
                    *op,
                    child,
                )
                .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
            }
            let (stopped, event) = match decoded.commit() {
                Wait::Stopped(stopped, event) => (stopped, event),
                Wait::Exited(..) => unreachable!("decoded FIFO kind changed while reserved"),
            };
            match stopped.into_cleanup_stop_lease() {
                Ok(lease) => *retained_stop = Some(lease),
                Err(error) => {
                    let (errno, stopped) = error.into_parts();
                    let stopped_key = HeldTaskStopKey::from_stopped(&stopped);
                    let replaced =
                        held.insert(stopped_key, HeldRootStop::from_event(&stopped, &event));
                    debug_assert!(
                        replaced.is_none(),
                        "preflighted cleanup slot changed while locked"
                    );
                    return Err(std::io::Error::from_raw_os_error(errno.into_raw()));
                }
            }
            return Ok(());
        }
    }

    fn take_held_task_stop(
        &self,
        tid: Pid,
        terminal: &TerminalCleanup,
    ) -> std::io::Result<Option<HeldRootStop>> {
        Self::take_held_task_stop_from(&self.held_task_stops, tid, terminal)
    }

    fn take_held_task_stop_from(
        held_task_stops: &HeldTaskStops,
        tid: Pid,
        terminal: &TerminalCleanup,
    ) -> std::io::Result<Option<HeldRootStop>> {
        let mut held_stops = held_task_stops.lock().unwrap();
        let key = HeldTaskStopKey::from_terminal(tid, terminal);
        let Some(held) = held_stops.get(&key) else {
            return Ok(None);
        };
        if !key.validates(held)
            || !terminal.same_generation(&held.terminal)
            || !held.armed
            || held.task_tid != tid
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("held LiteInst task stop did not match {tid}'s exact generation"),
            ));
        }
        let resolution = terminal
            .resolve_transferred_stop(
                &mut held_stops
                    .get_mut(&key)
                    .expect("validated held task stop must remain present while locked")
                    .cleanup_transfer,
            )
            .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
        let mut held = held_stops
            .remove(&key)
            .expect("resolved held task stop must remain present while locked");
        held.disarm();
        match resolution {
            TransferredStopResolution::Leased(cleanup_lease) => {
                held.cleanup_lease = Some(cleanup_lease);
                Ok(Some(held))
            }
            TransferredStopResolution::Finished => Ok(None),
            TransferredStopResolution::Failed(error) => {
                Err(std::io::Error::from_raw_os_error(error.into_raw()))
            }
        }
    }

    fn transfer_held_descendant_stops(
        &self,
        descendants: &mut BTreeMap<PhysicalEventGenerationId, RegisteredTraceeCleanup>,
    ) -> std::io::Result<()> {
        let generations = descendants.keys().copied().collect::<Vec<_>>();
        for generation in generations {
            let tracee = descendants
                .get(&generation)
                .expect("listed descendant must remain registered");
            let tid = tracee.identity.tid;
            let Some(held) = self.take_held_task_stop(tid, &tracee.terminal)? else {
                continue;
            };
            let stop = held.cleanup_lease;
            retain_cleanup_stop(
                &mut descendants
                    .get_mut(&generation)
                    .expect("held descendant must remain registered")
                    .frozen_stop,
                stop,
            )?;
        }
        Ok(())
    }

    fn finish_terminal_held_stops(&self) -> std::io::Result<()> {
        let mut held_stops = self.held_task_stops.lock().unwrap();
        let keys = held_stops.keys().copied().collect::<Vec<_>>();
        for key in keys {
            let Some(held) = held_stops.get(&key) else {
                continue;
            };
            if !key.validates(held) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("held stop key {key:?} does not match its cleanup owner"),
                ));
            }
            if !held.terminal.wait(Duration::ZERO) || !held.terminal.pending_is_empty() {
                continue;
            }
            let resolution = {
                let held = held_stops
                    .get_mut(&key)
                    .expect("terminal held stop must remain present while locked");
                held.terminal
                    .resolve_transferred_stop(&mut held.cleanup_transfer)
                    .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?
            };
            let mut held = held_stops
                .remove(&key)
                .expect("resolved terminal held stop must remain present while locked");
            held.disarm();
            match resolution {
                TransferredStopResolution::Leased(cleanup_lease) => {
                    finish_cancelled_stop(&held.terminal, Some(cleanup_lease))?;
                }
                TransferredStopResolution::Finished => {}
                TransferredStopResolution::Failed(error) => {
                    return Err(std::io::Error::from_raw_os_error(error.into_raw()));
                }
            }
        }
        Ok(())
    }

    fn freeze_root_generation(&mut self) -> std::io::Result<()> {
        let terminal = self
            .terminal
            .as_ref()
            .expect("registered LiteInst cleanup has a root terminal handle");
        terminal
            .ensure_registered()
            .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
        if self.root_frozen {
            Self::capture_pending_children(
                &self.newborn_tracees,
                &self.held_task_stops,
                terminal,
                &mut self.root_frozen_stop,
            )?;
            return Ok(());
        }
        if let Some(held) = self.take_held_task_stop(self.pid(), terminal)? {
            let matching_status = match held.status {
                HeldRootStopStatus::Signal(signal) => {
                    let _exact_signal = signal;
                    true
                }
                HeldRootStopStatus::NewChild(link) => self
                    .newborn_tracees
                    .lock()
                    .unwrap()
                    .get(&link.generation)
                    .is_some_and(|newborn| {
                        newborn.link == link && newborn.validates_key(link.generation)
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
            if !matching_status {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "held root stop lease did not match the exact event status",
                ));
            }
            self.root_frozen_stop = held.cleanup_lease;
            self.root_frozen = true;
            Self::capture_pending_children(
                &self.newborn_tracees,
                &self.held_task_stops,
                terminal,
                &mut self.root_frozen_stop,
            )?;
            return Ok(());
        }

        if terminal.terminal_error().is_some()
            && self.notifier_owner == Some(std::thread::current().id())
            && self.identity.checked_terminal_ownership()? == TerminalOwnership::TracerOwned
        {
            continue_registered_exit_stop(
                terminal,
                &mut self.root_frozen_stop,
                PhysicalResumeOwner::RootCleanup,
            )?;
        } else {
            match self.identity.send_signal(Signal::SIGSTOP) {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(std::io::Error::from_raw_os_error(error.into_raw())),
            }
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            Self::capture_pending_children(
                &self.newborn_tracees,
                &self.held_task_stops,
                terminal,
                &mut self.root_frozen_stop,
            )?;
            if self.root_frozen_stop.is_some() {
                self.root_frozen = true;
                return Ok(());
            }
            if terminal.terminal_error().is_some()
                && self.notifier_owner == Some(std::thread::current().id())
                && self.identity.checked_terminal_ownership()? == TerminalOwnership::TracerOwned
            {
                continue_registered_exit_stop(
                    terminal,
                    &mut self.root_frozen_stop,
                    PhysicalResumeOwner::RootCleanup,
                )?;
            }
            if self.root_frozen_stop.is_none()
                && let Some(reservation) = terminal.reserve_pending_for_cleanup(remaining)
            {
                let decoded = reservation.decode_guard().map_err(|error| {
                    std::io::Error::other(format!(
                        "decode exact root freeze state for {}: {error}",
                        self.pid()
                    ))
                })?;
                let (decoded, mut held) = match self.held_task_stops.try_lock() {
                    Ok(held) => (decoded, held),
                    Err(std::sync::TryLockError::WouldBlock) => {
                        drop(decoded);
                        let held = self.held_task_stops.lock().unwrap();
                        let Some(reservation) =
                            terminal.reserve_pending_for_cleanup(Duration::ZERO)
                        else {
                            continue;
                        };
                        let decoded = reservation.decode_guard().map_err(|error| {
                            std::io::Error::other(format!(
                                "decode rolled-back root freeze state for {}: {error}",
                                self.pid()
                            ))
                        })?;
                        (decoded, held)
                    }
                    Err(std::sync::TryLockError::Poisoned(error)) => panic!("{error}"),
                };
                let stopped_key = match decoded.wait() {
                    Wait::Stopped(stopped, _) => HeldTaskStopKey::from_stopped(stopped),
                    Wait::Exited(..) => unreachable!("pending cleanup status is nonterminal"),
                };
                let stopped_pid = stopped_key.task_tid;
                if stopped_key.generation != terminal.physical_event_generation() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "decoded root stop for {stopped_pid} belongs to generation {:?}, not {:?}",
                            stopped_key.generation,
                            terminal.physical_event_generation(),
                        ),
                    ));
                }
                if let Some(shadow) = held.get(&stopped_key) {
                    if !stopped_key.validates(shadow) || !shadow.terminal.same_generation(terminal)
                    {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "held root stop for {stopped_pid} does not match generation {:?}",
                                stopped_key.generation,
                            ),
                        ));
                    }
                    drop(decoded);
                    drop(held);
                    if let Some(held_stop) = Self::take_held_task_stop_from(
                        &self.held_task_stops,
                        stopped_pid,
                        terminal,
                    )? {
                        retain_cleanup_stop(&mut self.root_frozen_stop, held_stop.cleanup_lease)?;
                    }
                    continue;
                }
                if let Wait::Stopped(stopped, Event::NewChild(op, child)) = decoded.wait() {
                    NewbornTracee::register_event(
                        &mut self.newborn_tracees.lock().unwrap(),
                        stopped.pid(),
                        *op,
                        child,
                    )
                    .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
                }
                let (stopped, event) = match decoded.commit() {
                    Wait::Stopped(stopped, event) => (stopped, event),
                    Wait::Exited(..) => unreachable!("pending cleanup status is nonterminal"),
                };
                let cleanup_lease = match stopped.into_cleanup_stop_lease() {
                    Ok(lease) => lease,
                    Err(error) => {
                        let (errno, stopped) = error.into_parts();
                        let stopped_key = HeldTaskStopKey::from_stopped(&stopped);
                        let replaced =
                            held.insert(stopped_key, HeldRootStop::from_event(&stopped, &event));
                        debug_assert!(
                            replaced.is_none(),
                            "preflighted root cleanup slot changed while locked"
                        );
                        return Err(std::io::Error::from_raw_os_error(errno.into_raw()));
                    }
                };
                drop(held);
                retain_cleanup_stop(&mut self.root_frozen_stop, Some(cleanup_lease))?;
                self.root_frozen = true;
                return Ok(());
            }
            if terminal.exit_stop_observed() {
                continue_frozen_exit_stop(
                    terminal,
                    &mut self.root_frozen_stop,
                    PhysicalResumeOwner::RootCleanup,
                )?;
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

    fn retire_typed_descendants(
        newborn_tracees: &NewbornTracees,
        deadline: Instant,
    ) -> std::io::Result<()> {
        let mut newborns = newborn_tracees.lock().unwrap();

        // Deterministic category passes keep both the reported failure class
        // and the named generation independent of map iteration state.
        for (generation, newborn) in newborns.iter() {
            let tid = newborn.link.tid;
            let identity = newborn.identity.as_ref().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "typed LiteInst descendant {tid} generation {generation:?} lacks its captured identity"
                    ),
                )
            })?;
            let exact_event_child = newborn.validates_key(*generation)
                && identity.tid == tid
                && identity.parent.is_some_and(|(parent_tid, _, op)| {
                    parent_tid == newborn.link.parent_tid && op == Some(newborn.link.op)
                });
            if !exact_event_child {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "typed LiteInst descendant {tid} generation {generation:?} does not match its exact NewChild ownership edge"
                    ),
                ));
            }
        }
        for (generation, newborn) in newborns.iter() {
            let identity = newborn
                .identity
                .as_ref()
                .expect("identity pass validated every typed descendant");
            if !identity.matches_terminal_task(&newborn.terminal) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "typed LiteInst descendant {} generation {generation:?} does not match its terminal physical task identity",
                        newborn.link.tid,
                    ),
                ));
            }
        }

        // Await each worker-DONE bit with what remains of one shared budget.
        // This does not reserve, decode, consume, or retry a notifier status.
        for (generation, newborn) in newborns.iter() {
            #[cfg(test)]
            if let Some(pause) = TYPED_RETIREMENT_WAIT_PAUSES
                .lock()
                .unwrap()
                .remove(generation)
            {
                pause.captured.wait();
                pause.resume.wait();
            }
            if !newborn
                .terminal
                .wait(deadline.saturating_duration_since(Instant::now()))
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "typed LiteInst descendant {} generation {generation:?} lacks terminal notifier worker-DONE proof",
                        newborn.link.tid,
                    ),
                ));
            }
        }
        for (generation, newborn) in newborns.iter() {
            if !newborn.terminal.pending_is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "typed LiteInst descendant {} generation {generation:?} retains a notifier FIFO status",
                        newborn.link.tid,
                    ),
                ));
            }
        }
        for (generation, newborn) in newborns.iter() {
            if let Some(error) = newborn.registration_error() {
                return Err(std::io::Error::other(format!(
                    "typed LiteInst descendant {} generation {generation:?} retained notifier registration error: {error}",
                    newborn.link.tid,
                )));
            }
        }
        for (generation, newborn) in newborns.iter() {
            if let Some(error) = newborn.terminal.terminal_error() {
                return Err(std::io::Error::other(format!(
                    "typed LiteInst descendant {} generation {generation:?} retained terminal notifier error: {error}",
                    newborn.link.tid,
                )));
            }
        }
        for (generation, newborn) in newborns.iter() {
            let identity = newborn
                .identity
                .as_ref()
                .expect("identity pass validated every typed descendant");
            match identity.checked_terminal_ownership()? {
                TerminalOwnership::Released => {}
                TerminalOwnership::TracerOwned | TerminalOwnership::CapturedParentOwned => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        format!(
                            "typed LiteInst descendant {} generation {generation:?} remains tracee- or captured-parent-owned after terminal notification",
                            newborn.link.tid,
                        ),
                    ));
                }
            }
        }

        // The preflight above is deliberately all-or-nothing. A missing or
        // ambiguous proof leaves every generation-bound cleanup record in the
        // shared map. Reaching this point means Tool exit already joined the
        // followed tasks, every exact notifier worker is terminal with no queued
        // stop, and no recorded parent still owns any descendant generation.
        newborns.clear();
        Ok(())
    }

    fn finish_typed_completion(&mut self) -> std::io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let terminal = self.terminal.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "typed LiteInst completion lacks its registered root terminal handle",
            )
        })?;
        if !terminal.wait(deadline.saturating_duration_since(Instant::now())) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "typed LiteInst root lacks terminal notifier worker-DONE proof",
            ));
        }
        if !terminal.pending_is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "typed LiteInst root retains a notifier FIFO status",
            ));
        }
        if let Some(error) = terminal.registration_error() {
            return Err(std::io::Error::other(format!(
                "typed LiteInst root retained notifier registration error: {error}"
            )));
        }
        if let Some(error) = terminal.terminal_error() {
            return Err(std::io::Error::other(format!(
                "typed LiteInst root retained terminal notifier error: {error}"
            )));
        }
        if !matches!(
            self.identity.checked_generation()?,
            TraceeGenerationState::GoneOrReplaced
        ) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "typed LiteInst root retains its captured procfs generation",
            ));
        }
        if !self.retained_descendants.is_empty() || !self.retained_terminal_descendants.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "typed LiteInst completion retains cancellation-only descendants",
            ));
        }
        self.finish_terminal_held_stops()?;
        if !self.held_task_stops.lock().unwrap().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "typed LiteInst completion retains an unresolved task stop",
            ));
        }
        Self::retire_typed_descendants(&self.newborn_tracees, deadline)?;
        self.armed = false;
        Ok(())
    }

    fn confirm_reaped(&mut self) -> std::io::Result<bool> {
        if !self.armed {
            return Ok(true);
        }
        // Prove identity absence before consuming or retiring any retained
        // physical stop. Procfs uncertainty must leave every cleanup owner
        // intact for a later bounded retry.
        let identity_absent = matches!(
            self.identity.checked_generation()?,
            TraceeGenerationState::GoneOrReplaced
        );
        let notifier_finished = self
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.wait(Duration::ZERO) && terminal.pending_is_empty());
        let unregistered_absent =
            self.terminal.is_none() && identity_absent && self.physical_observer.is_none();
        self.finish_terminal_held_stops()?;
        let newborns_empty = self.newborn_tracees.lock().unwrap().is_empty();
        let retained_empty =
            self.retained_descendants.is_empty() && self.retained_terminal_descendants.is_empty();
        let held_stops_empty = self.held_task_stops.lock().unwrap().is_empty();
        if newborns_empty
            && retained_empty
            && held_stops_empty
            && ((notifier_finished && identity_absent) || unregistered_absent)
        {
            if let Some(terminal) = self.terminal.as_ref() {
                finish_cancelled_stop(terminal, self.root_frozen_stop.take())?;
            }
            self.armed = false;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn terminate_and_confirm(&mut self) -> std::io::Result<()> {
        match self.confirm_reaped() {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return Err(error),
        }

        let mut descendants = std::mem::take(&mut self.retained_descendants);
        let mut terminal_descendants = std::mem::take(&mut self.retained_terminal_descendants);
        let result =
            self.terminate_and_confirm_attempt(&mut descendants, &mut terminal_descendants);
        if result.is_err() {
            self.restore_retained_after_attempt(descendants, terminal_descendants)?;
        }
        result
    }

    fn restore_retained_after_attempt(
        &mut self,
        descendants: BTreeMap<PhysicalEventGenerationId, RegisteredTraceeCleanup>,
        terminal_descendants: BTreeMap<PhysicalEventGenerationId, TraceeIdentity>,
    ) -> std::io::Result<()> {
        // `terminate_and_confirm` exclusively moves both maps out before an
        // attempt. Repopulating either field behind that exclusive borrow
        // would make capability-preserving rollback impossible: no duplicate
        // frozen stop may be discarded or merged by equality. Fail-stop before
        // running destructors if this invariant is ever violated.
        if !self.retained_descendants.is_empty() || !self.retained_terminal_descendants.is_empty() {
            tracing::error!(
                pid = %self.pid(),
                "retained cleanup maps were repopulated during an exclusive cancellation attempt"
            );
            std::process::abort();
        }
        self.retained_descendants = descendants;
        self.retained_terminal_descendants = terminal_descendants;
        Ok(())
    }

    fn terminate_and_confirm_attempt(
        &mut self,
        descendants: &mut BTreeMap<PhysicalEventGenerationId, RegisteredTraceeCleanup>,
        terminal_descendants: &mut BTreeMap<PhysicalEventGenerationId, TraceeIdentity>,
    ) -> std::io::Result<()> {
        if self.terminal.is_none() {
            if self.exact_controller_startup {
                let terminal = self.unstarted_terminal.as_ref().ok_or_else(|| {
                    std::io::Error::other("pre-registration LiteInst cleanup authority is absent")
                })?;
                unsafe { terminal.terminate_unregistered_original_root(Errno::ECANCELED) }
                    .map_err(|error| {
                        std::io::Error::other(format!("pre-registration LiteInst cleanup: {error}"))
                    })?;
            } else {
                match self.identity.send_signal(Signal::SIGKILL) {
                    Ok(()) | Err(Errno::ESRCH) => {}
                    Err(error) => {
                        return Err(std::io::Error::from_raw_os_error(error.into_raw()));
                    }
                }
                drain_unregistered_child(self.pid()).map_err(|error| {
                    std::io::Error::other(format!(
                        "pre-registration dynamic LiteInst cleanup: {error}"
                    ))
                })?;
            }
            self.unstarted_terminal = None;
            self.armed = false;
            return Ok(());
        }

        match self.identity.checked_generation()? {
            TraceeGenerationState::Same(_) => self.freeze_root_generation()?,
            TraceeGenerationState::GoneOrReplaced => {
                // The exact root generation is already gone, so it cannot create
                // another descendant. Drain any child event the notifier published
                // before terminal acknowledgment and continue with the retained
                // generation-bound descendants; trying to freeze a completed root
                // would only collide with its consumed exit capability.
                if let Some(terminal) = self.terminal.as_ref() {
                    if let Some(held) = self.take_held_task_stop(self.pid(), terminal)? {
                        self.root_frozen_stop = held.cleanup_lease;
                    }
                    Self::capture_pending_children(
                        &self.newborn_tracees,
                        &self.held_task_stops,
                        terminal,
                        &mut self.root_frozen_stop,
                    )?;
                    finish_cancelled_stop(terminal, self.root_frozen_stop.take())?;
                }
                self.root_frozen = true;
            }
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
        for tracee in descendants.values_mut() {
            tracee.refresh_signal_authority()?;
        }
        Self::rekey_resolved_descendants(descendants)?;
        Self::retire_released_quarantine(descendants)?;
        if let Some((generation, tracee)) = descendants
            .iter()
            .find(|(_, tracee)| !tracee.signal_authority)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "descendant {} generation {generation:?} is quarantined without signal authority",
                    tracee.identity.tid
                ),
            ));
        }
        self.transfer_held_descendant_stops(descendants)?;
        self.finish_terminal_held_stops()?;
        let root_terminal = self.terminal.as_ref().unwrap();
        Self::capture_pending_children(
            &self.newborn_tracees,
            &self.held_task_stops,
            root_terminal,
            &mut self.root_frozen_stop,
        )?;
        if !root_terminal.pending_is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "root notifier FIFO changed while frozen",
            ));
        }
        if root_terminal.terminal_error().is_some() {
            continue_registered_exit_stop(
                root_terminal,
                &mut self.root_frozen_stop,
                PhysicalResumeOwner::RootCleanup,
            )?;
        } else {
            match root_terminal.send_sigkill_for_cleanup() {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(std::io::Error::from_raw_os_error(error.into_raw())),
            }
        }
        for tracee in descendants.values() {
            tracee
                .terminal
                .ensure_registered()
                .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
        }
        Self::rekey_resolved_descendants(descendants)?;
        for tracee in descendants.values_mut() {
            if !tracee.signal_authority {
                continue;
            }
            if !tracee.identity.matches_terminal_task(&tracee.terminal) {
                tracee.signal_authority = false;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "registered descendant lost exact signal authority",
                ));
            }
            if tracee.terminal.terminal_error().is_some() {
                tracee.continue_exit_stop()?;
            } else {
                tracee.send_sigkill()?;
            }
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Some(terminal) = self.terminal.as_ref() {
                Self::capture_pending_children(
                    &self.newborn_tracees,
                    &self.held_task_stops,
                    terminal,
                    &mut self.root_frozen_stop,
                )?;
            }
            for tracee in descendants.values_mut() {
                Self::capture_pending_children(
                    &self.newborn_tracees,
                    &self.held_task_stops,
                    &tracee.terminal,
                    &mut tracee.frozen_stop,
                )?;
            }
            self.discover_descendants(descendants, terminal_descendants)?;
            for tracee in descendants.values_mut() {
                tracee.refresh_signal_authority()?;
            }
            Self::rekey_resolved_descendants(descendants)?;
            Self::retire_released_quarantine(descendants)?;
            if let Some((generation, tracee)) = descendants
                .iter()
                .find(|(_, tracee)| !tracee.signal_authority)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "descendant {} generation {generation:?} is quarantined without signal authority",
                        tracee.identity.tid
                    ),
                ));
            }
            self.transfer_held_descendant_stops(descendants)?;
            self.finish_terminal_held_stops()?;
            for tracee in descendants.values() {
                tracee
                    .terminal
                    .ensure_registered()
                    .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
            }
            Self::rekey_resolved_descendants(descendants)?;
            for tracee in descendants.values_mut() {
                if !tracee.signal_authority {
                    continue;
                }
                if !tracee.identity.matches_terminal_task(&tracee.terminal) {
                    tracee.signal_authority = false;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "registered descendant lost exact signal authority",
                    ));
                }
                if tracee.terminal.terminal_error().is_some() {
                    tracee.continue_exit_stop()?;
                } else {
                    tracee.send_sigkill()?;
                }
            }

            let root_done = self
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.wait(Duration::ZERO));
            if root_done {
                finish_cancelled_stop(root_terminal, self.root_frozen_stop.take())?;
            }
            let completed = descendants
                .iter()
                .filter_map(|(generation, tracee)| {
                    tracee.terminal.wait(Duration::ZERO).then_some(*generation)
                })
                .collect::<Vec<_>>();
            for generation in completed {
                if let Some(tracee) = descendants.get_mut(&generation) {
                    Self::capture_pending_children(
                        &self.newborn_tracees,
                        &self.held_task_stops,
                        &tracee.terminal,
                        &mut tracee.frozen_stop,
                    )?;
                    finish_cancelled_stop(&tracee.terminal, tracee.frozen_stop.take())?;
                }
                let generation_state = descendants
                    .get(&generation)
                    .expect("completed descendant must remain registered")
                    .identity
                    .checked_generation()?;
                if matches!(generation_state, TraceeGenerationState::Same(_))
                    && terminal_descendants.contains_key(&generation)
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "terminal descendant generation {generation:?} already has an owner"
                        ),
                    ));
                }
                let tracee = descendants
                    .remove(&generation)
                    .expect("completed descendant must remain registered");
                if matches!(generation_state, TraceeGenerationState::Same(_)) {
                    terminal_descendants.insert(generation, tracee.identity);
                }
            }
            // Once the exact notifier generation is terminal, retain its proc
            // identity only while it remains our tracee or its recorded parent
            // still owns the zombie. After reparenting, waiting for another
            // process to reap it cannot strengthen our cleanup proof and can
            // never make progress here.
            let mut released = Vec::new();
            for (generation, identity) in terminal_descendants.iter() {
                // A PPid/TracerPid transition supplies no release authority.
                // Retain it until a later bounded iteration obtains a stable
                // ownership proof.
                if terminal_descendant_stably_released(
                    identity.checked_terminal_ownership_sample()?,
                ) {
                    released.push(*generation);
                }
            }
            for generation in released {
                terminal_descendants.remove(&generation);
            }
            let root_absent = matches!(
                self.identity.checked_generation()?,
                TraceeGenerationState::GoneOrReplaced
            );
            let newborns_empty = self.newborn_tracees.lock().unwrap().is_empty();
            let held_stops_empty = self.held_task_stops.lock().unwrap().is_empty();
            if root_done
                && root_absent
                && descendants.is_empty()
                && terminal_descendants.is_empty()
                && newborns_empty
                && held_stops_empty
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
                // A changing ownership sample performs no raw continuation;
                // the next bounded iteration must obtain a stable local-tracer
                // proof first.
                if !root_done
                    && (descendants.is_empty() || root_terminal.terminal_error().is_some())
                    && terminal_ownership_permits_continuation(
                        self.identity.checked_terminal_ownership_sample()?,
                    )
                {
                    continue_registered_exit_stop(
                        root_terminal,
                        &mut self.root_frozen_stop,
                        PhysicalResumeOwner::RootCleanup,
                    )?;
                }
                for tracee in descendants.values_mut() {
                    if tracee.signal_authority
                        && terminal_ownership_permits_continuation(
                            tracee.identity.checked_terminal_ownership_sample()?,
                        )
                    {
                        tracee.continue_exit_stop()?;
                    }
                }
            }
            if let Some(terminal) = self.terminal.as_ref() {
                terminal.wait(Duration::from_millis(1));
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "notifier did not acknowledge terminal cleanup for LiteInst tracee {}",
                self.pid()
            ),
        ))
    }

    fn discover_descendants(
        &self,
        descendants: &mut BTreeMap<PhysicalEventGenerationId, RegisteredTraceeCleanup>,
        terminal_descendants: &BTreeMap<PhysicalEventGenerationId, TraceeIdentity>,
    ) -> std::io::Result<()> {
        let root_generation = self
            .terminal
            .as_ref()
            .expect("descendant discovery requires a registered root")
            .physical_event_generation();
        let root_snapshot = match self.identity.checked_generation()? {
            TraceeGenerationState::Same(snapshot) => Some(snapshot),
            TraceeGenerationState::GoneOrReplaced => None,
        };
        let root_present = root_snapshot.is_some();
        let mut queue = VecDeque::new();
        if root_present {
            queue.push_back((root_generation, self.pid()));
        }
        queue.extend(descendants.iter().filter_map(|(generation, tracee)| {
            (tracee.identity.snapshot.tgid == tracee.identity.tid)
                .then_some((*generation, tracee.identity.tid))
        }));
        let newborn_generations = self
            .newborn_tracees
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let mut transferred = Vec::new();
        let mut absorbed = Vec::new();
        for generation in newborn_generations {
            if generation == root_generation {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "a NewChild record reused the root's exact physical generation",
                ));
            }
            if terminal_descendants.contains_key(&generation) {
                let newborns = self.newborn_tracees.lock().unwrap();
                let newborn = newborns
                    .get(&generation)
                    .expect("listed terminal newborn must remain registered");
                let identity = terminal_descendants
                    .get(&generation)
                    .expect("terminal generation disappeared while borrowed");
                let exact = newborn.validates_key(generation)
                    && identity.tid == newborn.link.tid
                    && identity.parent.is_some_and(|(parent_tid, _, op)| {
                        parent_tid == newborn.link.parent_tid && op == Some(newborn.link.op)
                    });
                drop(newborns);
                if !exact {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "terminal generation {generation:?} conflicts with its newborn ownership"
                        ),
                    ));
                }
                self.newborn_tracees.lock().unwrap().remove(&generation);
                continue;
            }
            if let Some(existing) = descendants.get_mut(&generation) {
                let newborn = self
                    .newborn_tracees
                    .lock()
                    .unwrap()
                    .remove(&generation)
                    .expect("listed newborn must remain registered");
                if !newborn.validates_key(generation)
                    || !existing.terminal.same_generation(&newborn.terminal)
                    || existing.event_link.is_some_and(|link| link != newborn.link)
                {
                    NewbornTracee::restore_removed(
                        &mut self.newborn_tracees.lock().unwrap(),
                        generation,
                        newborn,
                    )?;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "duplicate descendant generation {generation:?} has conflicting ownership"
                        ),
                    ));
                }
                let prior_link = existing.event_link;
                existing.event_link = Some(newborn.link);
                absorbed.push((generation, newborn, prior_link));
                continue;
            }

            let mut newborn = self
                .newborn_tracees
                .lock()
                .unwrap()
                .remove(&generation)
                .expect("listed newborn must remain registered");
            if !newborn.validates_key(generation) {
                NewbornTracee::restore_removed(
                    &mut self.newborn_tracees.lock().unwrap(),
                    generation,
                    newborn,
                )?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("newborn generation {generation:?} has mismatched ownership"),
                ));
            }
            if let Some(observer) = self.physical_observer.as_ref() {
                if newborn
                    .terminal
                    .attach_physical_event_observer(observer)
                    .is_err()
                {
                    NewbornTracee::restore_removed(
                        &mut self.newborn_tracees.lock().unwrap(),
                        generation,
                        newborn,
                    )?;
                    Self::restore_transferred_newborns(
                        &self.newborn_tracees,
                        descendants,
                        &mut transferred,
                        &mut absorbed,
                    )?;
                    return Err(std::io::Error::from_raw_os_error(libc::EPROTO));
                }
            }
            let identity = match newborn.identity.take() {
                Some(identity) => identity,
                None => match TraceeIdentity::capture_event_child(
                    newborn.link.tid,
                    newborn.link.parent_tid,
                    newborn.link.op,
                ) {
                    Ok(identity) => identity,
                    Err(error) => {
                        NewbornTracee::restore_removed(
                            &mut self.newborn_tracees.lock().unwrap(),
                            generation,
                            newborn,
                        )?;
                        Self::restore_transferred_newborns(
                            &self.newborn_tracees,
                            descendants,
                            &mut transferred,
                            &mut absorbed,
                        )?;
                        return Err(std::io::Error::from_raw_os_error(error.into_raw()));
                    }
                },
            };
            let queued_group_leader =
                (identity.snapshot.tgid == identity.tid).then_some(identity.tid);
            let registration_error = newborn.terminal.ensure_registered().err();
            let resolved_generation = newborn.terminal.physical_event_generation();
            newborn.link.generation = resolved_generation;
            let generation = resolved_generation;
            if generation == root_generation {
                NewbornTracee::restore_removed(
                    &mut self.newborn_tracees.lock().unwrap(),
                    generation,
                    newborn,
                )?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "a resolved NewChild record reused the root's exact physical generation",
                ));
            }
            if terminal_descendants.contains_key(&generation) {
                NewbornTracee::restore_removed(
                    &mut self.newborn_tracees.lock().unwrap(),
                    generation,
                    newborn,
                )?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "resolved NewChild generation {generation:?} already has terminal ownership"
                    ),
                ));
            }
            let signal_authority = identity.matches_terminal_task(&newborn.terminal);
            RegisteredTraceeCleanup::insert_exact(
                descendants,
                generation,
                RegisteredTraceeCleanup {
                    identity,
                    terminal: newborn.terminal,
                    event_link: Some(newborn.link),
                    frozen_stop: None,
                    signal_authority,
                },
            );
            transferred.push(generation);
            if let Some(error) = registration_error {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "NewChild generation {generation:?} retained registration error {error}"
                    ),
                ));
            }
            if !signal_authority {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "NewChild generation {generation:?} did not match its captured procfs identity; retained without signal authority"
                    ),
                ));
            }
            if let Some(tid) = queued_group_leader {
                queue.push_back((generation, tid));
            }
        }

        #[cfg(test)]
        if self
            .fail_discovery_once
            .as_ref()
            .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
        {
            Self::restore_transferred_newborns(
                &self.newborn_tracees,
                descendants,
                &mut transferred,
                &mut absorbed,
            )?;
            return Err(std::io::Error::from_raw_os_error(libc::EIO));
        }

        let task_tids = match root_present.then(|| task_tids(self.pid())).transpose() {
            Ok(Some(tids)) => tids,
            Ok(None) => Vec::new(),
            Err(error) => {
                Self::restore_transferred_newborns(
                    &self.newborn_tracees,
                    descendants,
                    &mut transferred,
                    &mut absorbed,
                )?;
                return Err(error);
            }
        };
        if let Some(expected) = root_snapshot {
            match self.identity.checked_generation()? {
                TraceeGenerationState::Same(after) if after == expected => {}
                _ => {
                    Self::restore_transferred_newborns(
                        &self.newborn_tracees,
                        descendants,
                        &mut transferred,
                        &mut absorbed,
                    )?;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "root generation changed while its task directory was scanned",
                    ));
                }
            }
        }
        for tid in task_tids {
            if tid == self.pid()
                || Self::contains_current_tid(descendants, terminal_descendants, tid)?
            {
                continue;
            }
            let identity = match TraceeIdentity::open_task_tid(tid, self.identity.snapshot.tgid) {
                Ok(Some(identity)) => identity,
                Ok(None) => continue,
                Err(error) => {
                    Self::restore_transferred_newborns(
                        &self.newborn_tracees,
                        descendants,
                        &mut transferred,
                        &mut absorbed,
                    )?;
                    return Err(error);
                }
            };
            let identity_snapshot = match identity.checked_generation()? {
                TraceeGenerationState::Same(snapshot) => snapshot,
                TraceeGenerationState::GoneOrReplaced => continue,
            };
            let stopped = Stopped::try_new_current_unchecked(tid)
                .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
            if identity.checked_generation()? != TraceeGenerationState::Same(identity_snapshot) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!("task {tid} changed generation while its notifier was bound"),
                ));
            }
            if let Some(observer) = self.physical_observer.as_ref() {
                if stopped.attach_physical_event_observer(observer).is_err() {
                    Self::restore_transferred_newborns(
                        &self.newborn_tracees,
                        descendants,
                        &mut transferred,
                        &mut absorbed,
                    )?;
                    return Err(std::io::Error::from_raw_os_error(libc::EPROTO));
                }
            }
            let terminal = stopped.terminal_cleanup();
            let registration_error = terminal.ensure_registered().err();
            // Registration may adopt an already-authoritative EventHandle.
            // Read the generation only after that redirect is resolved.
            let generation = terminal.physical_event_generation();
            let signal_authority = identity.matches_terminal_task(&terminal);
            RegisteredTraceeCleanup::insert_exact(
                descendants,
                generation,
                RegisteredTraceeCleanup {
                    identity,
                    terminal,
                    event_link: None,
                    frozen_stop: None,
                    signal_authority,
                },
            );
            if let Some(error) = registration_error {
                Self::restore_transferred_newborns(
                    &self.newborn_tracees,
                    descendants,
                    &mut transferred,
                    &mut absorbed,
                )?;
                return Err(std::io::Error::from_raw_os_error(error.into_raw()));
            }
            if !signal_authority {
                Self::restore_transferred_newborns(
                    &self.newborn_tracees,
                    descendants,
                    &mut transferred,
                    &mut absorbed,
                )?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "task {tid} notifier generation did not match its captured procfs identity; retained without signal authority"
                    ),
                ));
            }
            if descendants
                .get(&generation)
                .expect("task-scan generation was just retained")
                .identity
                .checked_generation()?
                != TraceeGenerationState::Same(identity_snapshot)
            {
                Self::restore_transferred_newborns(
                    &self.newborn_tracees,
                    descendants,
                    &mut transferred,
                    &mut absorbed,
                )?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "task {tid} changed generation during observer attachment; its bound notifier generation remains retained"
                    ),
                ));
            }
        }

        let mut visited = BTreeSet::new();
        'parents: while let Some((parent_generation, parent)) = queue.pop_front() {
            if !visited.insert(parent_generation) {
                continue;
            }
            let Some(parent_snapshot) = self.resample_queued_parent(
                descendants,
                root_generation,
                parent_generation,
                parent,
                None,
            )?
            else {
                continue;
            };
            let children = match direct_children(parent) {
                Ok(children) => children,
                Err(error) => {
                    let parent_check = match self.checked_queued_parent(
                        descendants,
                        root_generation,
                        parent_generation,
                        parent,
                        Some(parent_snapshot),
                    ) {
                        Ok(parent_check) => parent_check,
                        Err(error) => {
                            Self::restore_transferred_newborns(
                                &self.newborn_tracees,
                                descendants,
                                &mut transferred,
                                &mut absorbed,
                            )?;
                            return Err(error);
                        }
                    };
                    let error = std::io::Error::new(
                        error.kind(),
                        format!("read direct children of bound tracee {parent}: {error}"),
                    );
                    match resolve_parent_scan_error(parent_check, error) {
                        Ok(ParentScanErrorResolution::DiscardParent) => continue 'parents,
                        Err(error) => {
                            Self::restore_transferred_newborns(
                                &self.newborn_tracees,
                                descendants,
                                &mut transferred,
                                &mut absorbed,
                            )?;
                            return Err(error);
                        }
                    }
                }
            };
            if self
                .resample_queued_parent(
                    descendants,
                    root_generation,
                    parent_generation,
                    parent,
                    Some(parent_snapshot),
                )?
                .is_none()
            {
                continue 'parents;
            }
            for child in children {
                if Self::contains_current_tid(descendants, terminal_descendants, child)?
                    || (child == self.pid()
                        && matches!(
                            self.identity.checked_generation()?,
                            TraceeGenerationState::Same(_)
                        ))
                {
                    continue;
                }
                let identity = match TraceeIdentity::open_discovered(child, parent) {
                    Ok(Some(identity)) => identity,
                    Ok(None) => continue,
                    Err(error) => {
                        let parent_check = match self.checked_queued_parent(
                            descendants,
                            root_generation,
                            parent_generation,
                            parent,
                            Some(parent_snapshot),
                        ) {
                            Ok(parent_check) => parent_check,
                            Err(error) => {
                                Self::restore_transferred_newborns(
                                    &self.newborn_tracees,
                                    descendants,
                                    &mut transferred,
                                    &mut absorbed,
                                )?;
                                return Err(error);
                            }
                        };
                        let error = std::io::Error::new(
                            error.kind(),
                            format!("bind listed tracee {child} under parent {parent}: {error}"),
                        );
                        match resolve_parent_scan_error(parent_check, error) {
                            Ok(ParentScanErrorResolution::DiscardParent) => continue 'parents,
                            Err(error) => {
                                Self::restore_transferred_newborns(
                                    &self.newborn_tracees,
                                    descendants,
                                    &mut transferred,
                                    &mut absorbed,
                                )?;
                                return Err(error);
                            }
                        }
                    }
                };
                let identity_snapshot = match identity.checked_generation()? {
                    TraceeGenerationState::Same(snapshot) => snapshot,
                    TraceeGenerationState::GoneOrReplaced => continue,
                };
                if self
                    .resample_queued_parent(
                        descendants,
                        root_generation,
                        parent_generation,
                        parent,
                        Some(parent_snapshot),
                    )?
                    .is_none()
                {
                    continue 'parents;
                }
                let running = match Running::try_new_current(child) {
                    Ok(running) => running,
                    Err(error) => {
                        Self::restore_transferred_newborns(
                            &self.newborn_tracees,
                            descendants,
                            &mut transferred,
                            &mut absorbed,
                        )?;
                        return Err(std::io::Error::from_raw_os_error(error.into_raw()));
                    }
                };
                let provisional_generation = running.physical_event_generation();
                if identity.checked_generation()? != TraceeGenerationState::Same(identity_snapshot)
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        format!(
                            "discovered child {child} changed generation while its notifier was bound"
                        ),
                    ));
                }
                if let Some(observer) = self.physical_observer.as_ref() {
                    running
                        .attach_physical_event_observer(observer)
                        .map_err(|_| std::io::Error::from_raw_os_error(libc::EPROTO))?;
                    observer.link_pre_registration_task(
                        PhysicalTaskIdentity::direct_child(child),
                        provisional_generation,
                    );
                }
                let terminal = running.terminal_cleanup();
                let registration_error = terminal.ensure_registered().err();
                // Registration can redirect the provisional generation to an
                // existing authoritative notifier EventHandle.
                let generation = terminal.physical_event_generation();
                let signal_authority = identity.matches_terminal_task(&terminal);
                RegisteredTraceeCleanup::insert_exact(
                    descendants,
                    generation,
                    RegisteredTraceeCleanup {
                        identity,
                        terminal,
                        event_link: None,
                        frozen_stop: None,
                        signal_authority,
                    },
                );
                if let Some(error) = registration_error {
                    Self::restore_transferred_newborns(
                        &self.newborn_tracees,
                        descendants,
                        &mut transferred,
                        &mut absorbed,
                    )?;
                    return Err(std::io::Error::from_raw_os_error(error.into_raw()));
                }
                if !signal_authority {
                    Self::restore_transferred_newborns(
                        &self.newborn_tracees,
                        descendants,
                        &mut transferred,
                        &mut absorbed,
                    )?;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        format!(
                            "discovered child {child} notifier generation did not match its captured procfs identity; retained without signal authority"
                        ),
                    ));
                }
                let registered = descendants
                    .get(&generation)
                    .expect("discovered generation was just retained");
                if registered.identity.checked_generation()?
                    != TraceeGenerationState::Same(identity_snapshot)
                {
                    Self::restore_transferred_newborns(
                        &self.newborn_tracees,
                        descendants,
                        &mut transferred,
                        &mut absorbed,
                    )?;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        format!(
                            "discovered child {child} changed generation during observer attachment; its bound notifier generation remains retained"
                        ),
                    ));
                }
                if registered.identity.snapshot.tgid == registered.identity.tid {
                    queue.push_back((generation, child));
                }
            }
        }
        #[cfg(test)]
        if self
            .fail_after_scan_once
            .as_ref()
            .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
        {
            Self::restore_transferred_newborns(
                &self.newborn_tracees,
                descendants,
                &mut transferred,
                &mut absorbed,
            )?;
            return Err(std::io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(())
    }

    fn contains_current_tid(
        descendants: &BTreeMap<PhysicalEventGenerationId, RegisteredTraceeCleanup>,
        terminal_descendants: &BTreeMap<PhysicalEventGenerationId, TraceeIdentity>,
        tid: Pid,
    ) -> std::io::Result<bool> {
        for identity in descendants
            .values()
            .map(|tracee| &tracee.identity)
            .chain(terminal_descendants.values())
            .filter(|identity| identity.tid == tid)
        {
            if matches!(
                identity.checked_generation()?,
                TraceeGenerationState::Same(_)
            ) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn checked_queued_parent(
        &self,
        descendants: &BTreeMap<PhysicalEventGenerationId, RegisteredTraceeCleanup>,
        root_generation: PhysicalEventGenerationId,
        generation: PhysicalEventGenerationId,
        tid: Pid,
        baseline: Option<TraceeSnapshot>,
    ) -> std::io::Result<QueuedParentCheck> {
        let identity = if generation == root_generation {
            &self.identity
        } else {
            let Some(tracee) = descendants.get(&generation) else {
                return Ok(QueuedParentCheck::Unavailable);
            };
            &tracee.identity
        };
        match identity.checked_generation()? {
            TraceeGenerationState::Same(snapshot)
                if snapshot.tgid == identity.tid && identity.tid == tid =>
            {
                let tracer_is_current = checked_tracer_is_current(snapshot.tracer_pid)?;
                Ok(classify_queued_parent(
                    baseline,
                    snapshot,
                    tracer_is_current,
                ))
            }
            TraceeGenerationState::Same(_) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "descendant generation {generation:?} is not the queued group leader {tid}"
                ),
            )),
            TraceeGenerationState::GoneOrReplaced => Ok(QueuedParentCheck::Unavailable),
        }
    }

    fn resample_queued_parent(
        &self,
        descendants: &BTreeMap<PhysicalEventGenerationId, RegisteredTraceeCleanup>,
        root_generation: PhysicalEventGenerationId,
        generation: PhysicalEventGenerationId,
        tid: Pid,
        baseline: Option<TraceeSnapshot>,
    ) -> std::io::Result<Option<TraceeSnapshot>> {
        match self.checked_queued_parent(descendants, root_generation, generation, tid, baseline)? {
            QueuedParentCheck::Active(snapshot) => Ok(Some(snapshot)),
            QueuedParentCheck::Unavailable => Ok(None),
            QueuedParentCheck::LiveTracerChanged => Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "parent generation {generation:?} changed live tracer authority during descendant discovery"
                ),
            )),
        }
    }

    fn restore_transferred_newborns(
        newborn_tracees: &NewbornTracees,
        descendants: &mut BTreeMap<PhysicalEventGenerationId, RegisteredTraceeCleanup>,
        transferred: &mut Vec<PhysicalEventGenerationId>,
        absorbed: &mut Vec<(
            PhysicalEventGenerationId,
            NewbornTracee,
            Option<EventChildLink>,
        )>,
    ) -> std::io::Result<()> {
        let mut newborns = newborn_tracees.lock().unwrap();
        for (generation, newborn, prior_link) in absorbed.drain(..) {
            let registered = descendants.get_mut(&generation).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("rollback lost absorbed generation {generation:?}"),
                )
            })?;
            if registered.event_link != Some(newborn.link) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("rollback found changed absorbed generation {generation:?}"),
                ));
            }
            registered.event_link = prior_link;
            NewbornTracee::restore_removed(&mut newborns, generation, newborn)?;
        }
        for generation in transferred.drain(..) {
            let registered = descendants.remove(&generation).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("rollback lost local descendant generation {generation:?}"),
                )
            })?;
            if registered.frozen_stop.is_some() {
                tracing::error!(
                    ?generation,
                    "rollback cannot move a frozen descendant stop into a newborn record"
                );
                std::process::abort();
            }
            let newborn = NewbornTracee {
                link: registered
                    .event_link
                    .expect("transferred newborn retains kernel event link"),
                identity: Some(registered.identity),
                terminal: registered.terminal,
            };
            NewbornTracee::restore_removed(&mut newborns, generation, newborn)?;
        }
        Ok(())
    }
}

fn send_identity_sigkill(
    identity: &TraceeIdentity,
    terminal: &TerminalCleanup,
) -> std::io::Result<()> {
    if identity.pidfd.is_none() {
        // Nonleader TIDs are terminated by their TGID leader's pidfd. They are
        // never signaled numerically; their bound ptrace statuses are drained
        // separately on the owning tracer thread.
        return Ok(());
    }
    match terminal.send_sigkill_for_cleanup() {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(std::io::Error::from_raw_os_error(error.into_raw())),
    }
}

impl Drop for LiteinstTraceeCleanup {
    fn drop(&mut self) {
        if self.armed {
            // Cancellation cannot await an orderly drain, so synchronously
            // request termination and wait for the notifier-owned final reap.
            // Before async registration, the bounded raw-wait fallback owns
            // cleanup instead.
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match self.terminate_and_confirm() {
                    Ok(()) => break,
                    Err(error) if Instant::now() < deadline => {
                        // Every failed attempt restores all descendant/newborn
                        // ownership to this guard. Retry transient discovery
                        // and registration errors without dropping cleanup
                        // records.
                        std::thread::sleep(Duration::from_millis(1));
                        tracing::debug!(pid = %self.pid(), %error, "retrying LiteInst cancellation cleanup");
                    }
                    Err(error) => {
                        let fail_stop_preparation = self
                            .terminal
                            .as_ref()
                            .or(self.unstarted_terminal.as_ref())
                            .map(TerminalCleanup::prepare_startup_cleanup_fail_stop)
                            .transpose();
                        tracing::error!(
                            pid = %self.pid(),
                            %error,
                            ?fail_stop_preparation,
                            "LiteInst cancellation cleanup exhausted its bounded retries; aborting controller"
                        );
                        // The typed helper above either spends the sole
                        // exact-pidfd SIGKILL request or observes its retained
                        // accepted/ESRCH/error state. It never retries a spent
                        // request. EXITKILL is the registered-tracee backstop.
                        std::process::abort();
                    }
                }
            }
        }
        if !self.ready_for_observer_close() {
            tracing::error!(
                pid = %self.pid(),
                held_stops = self.held_task_stops.lock().unwrap().len(),
                "leaving LiteInst physical observer open because cancellation cleanup is still armed"
            );
            return;
        }
        if let Some(observer) = self.physical_observer.as_ref() {
            observer.close();
            let snapshot = observer.snapshot();
            let validation = snapshot.validate();
            if !validation.is_valid() {
                tracing::error!(
                    pid = %self.pid(),
                    observer = ?snapshot.observer(),
                    ordinary_lost = snapshot.ordinary_lost(),
                    cleanup_lost = snapshot.cleanup_lost(),
                    after_close = snapshot.after_close(),
                    violations = ?validation.violations,
                    "LiteInst physical partition failed during cancellation drop"
                );
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

fn checked_tracer_is_current(tracer_tid: Pid) -> std::io::Result<bool> {
    if tracer_tid.as_raw() <= 0 {
        return Ok(false);
    }
    match fs::metadata(format!("/proc/self/task/{tracer_tid}")) {
        Ok(_) => Ok(true),
        Err(error) if process_gone_error(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

fn process_gone_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
        || matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH))
}

fn proc_path_inode(tid: Pid) -> std::io::Result<Option<u64>> {
    match fs::metadata(format!("/proc/{tid}")) {
        Ok(metadata) => Ok(Some(metadata.ino())),
        Err(error) if process_gone_error(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn fd_inode(fd: &OwnedFd) -> std::io::Result<u64> {
    fs::metadata(format!("/proc/self/fd/{}", fd.as_raw_fd())).map(|metadata| metadata.ino())
}

fn checked_tracee_open_absence(tid: Pid, error: Errno) -> std::io::Result<bool> {
    if !matches!(error, Errno::ENOENT | Errno::ESRCH) {
        return Err(std::io::Error::from_raw_os_error(error.into_raw()));
    }
    match proc_path_inode(tid)? {
        None => Ok(true),
        Some(_) => Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "tracee {tid} still has a procfs identity after transient capture error {error}"
            ),
        )),
    }
}

fn reconcile_snapshot_failure(
    tid: Pid,
    saved_inode: u64,
    current_inode: Option<u64>,
    error: std::io::Error,
) -> std::io::Result<TraceeGenerationState> {
    match current_inode {
        None => Ok(TraceeGenerationState::GoneOrReplaced),
        Some(inode) if inode != saved_inode => Ok(TraceeGenerationState::GoneOrReplaced),
        Some(_) => Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!("tracee {tid} retained its proc inode while identity sampling failed: {error}"),
        )),
    }
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
    let mut tids = tasks
        .map(|task| {
            let task = task?;
            let tid = task.file_name().into_string().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "non-UTF8 task TID")
            })?;
            tid.parse::<i32>()
                .map(Pid::from_raw)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    tids.sort_by_key(|tid| tid.as_raw());
    tids.dedup();
    Ok(tids)
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
    children.sort_by_key(|pid| pid.as_raw());
    children.dedup();
    Ok(children)
}

fn drain_unregistered_child(pid: Pid) -> Result<(), TraceError> {
    for _ in 0..2_000 {
        let mut status = 0;
        let waited =
            unsafe { libc::waitpid(pid.as_raw(), &mut status, libc::__WALL | libc::WNOHANG) };
        if waited == 0 {
            std::thread::sleep(Duration::from_millis(1));
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
                    std::thread::sleep(Duration::from_millis(1));
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

impl<G: Default> Tracer<G> {
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

    #[cfg(target_arch = "x86_64")]
    fn finalize_liteinst_physical_observer(
        guest_pid: Pid,
        slot: &mut Option<(
            safeptrace::PhysicalEventObserver,
            crate::LiteinstCallerDiagnostics,
        )>,
    ) -> Result<(), Error> {
        let Some((observer, diagnostics)) = slot.take() else {
            return Ok(());
        };
        observer.close();
        let snapshot = observer.snapshot();
        let validation = snapshot.validate();
        diagnostics.record(
            "physical event partition validated",
            None,
            format!(
                "observer={:?} records={} physical_statuses={} successful_resumes={} explicit_dispositions={} ordinary_lost={} cleanup_lost={} after_close={} violations={:?}",
                snapshot.observer(),
                snapshot.records().len(),
                validation.physical_statuses,
                validation.successful_resumes,
                validation.explicit_dispositions,
                snapshot.ordinary_lost(),
                snapshot.cleanup_lost(),
                snapshot.after_close(),
                validation.violations,
            ),
        )?;
        if !validation.is_valid() {
            let failure_tail = snapshot
                .records()
                .iter()
                .rev()
                .take(64)
                .copied()
                .collect::<Vec<_>>();
            diagnostics.record(
                "physical event partition failure tail",
                None,
                format!("newest_first={failure_tail:?}"),
            )?;
            return Err(anyhow::anyhow!(
                "validate physical wait and resume partition failed for tracee {guest_pid}: {:?}",
                validation.violations,
            )
            .into());
        }
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn finalize_liteinst_physical_observer_after_cleanup(
        guest_pid: Pid,
        cleanup: Option<&LiteinstTraceeCleanup>,
        slot: &mut Option<(
            safeptrace::PhysicalEventObserver,
            crate::LiteinstCallerDiagnostics,
        )>,
    ) -> Result<(), Error> {
        if cleanup.is_some_and(|cleanup| !cleanup.ready_for_observer_close()) {
            return Ok(());
        }
        Self::finalize_liteinst_physical_observer(guest_pid, slot)
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn finalize_liteinst_physical_observer(_guest_pid: Pid) -> Result<(), Error> {
        Ok(())
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
    pub async fn wait_with_output(mut self) -> Result<(Output, G), Error> {
        use tokio::io::AsyncRead;
        use tokio::io::AsyncReadExt;

        async fn read_to_end<A: AsyncRead + Unpin>(io: Option<A>) -> Result<Vec<u8>, Error> {
            let mut vec = Vec::new();
            if let Some(mut io) = io {
                io.read_to_end(&mut vec).await?;
            }
            Ok(vec)
        }

        drop(self.stdin.take());

        let stdout = read_to_end(self.stdout.take());
        let stderr = read_to_end(self.stderr.take());

        let ((status, state), stdout, stderr) =
            future::try_join3(self.wait(), stdout, stderr).await?;

        Ok((
            Output {
                status,
                stdout,
                stderr,
            },
            state,
        ))
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
    pub async fn wait_discarding_output(mut self) -> Result<(ExitStatus, G), Error> {
        use tokio::io::AsyncRead;

        async fn drain<A: AsyncRead + Unpin>(io: Option<A>) -> Result<(), Error> {
            if let Some(mut io) = io {
                tokio::io::copy(&mut io, &mut tokio::io::sink()).await?;
            }
            Ok(())
        }

        drop(self.stdin.take());

        let stdout = drain(self.stdout.take());
        let stderr = drain(self.stderr.take());

        let ((status, state), (), ()) = future::try_join3(self.wait(), stdout, stderr).await?;

        Ok((status, state))
    }

    /// Waits for the tracee to exit and returns its exit status and global
    /// state.
    ///
    /// This does **not** touch the guest's stdio handles. If the caller piped
    /// stdout or stderr, use [`Tracer::wait_with_output`] or
    /// [`Tracer::wait_discarding_output`] instead; otherwise a guest that fills
    /// an unread pipe buffer deadlocks against this wait.
    pub async fn wait(mut self) -> Result<(ExitStatus, G), Error> {
        // Note: The usage of LocalSet is *very* important here. Once polled,
        // the `tracer` future drives all tracees to completion. The `fork` for
        // the root tracee and all subsequent ptrace operations *MUST* be done
        // on the same thread. Thus, we use `LocalSet` in combination with
        // `tokio::task::spawn_local` to ensure that everything happens on the
        // same thread. Otherwise, ptrace operations will start returning
        // `ESRCH` errors and they will be (incorrectly) interpretted to mean
        // that the tracee has died unexpectedly.
        let local_set = tokio::task::LocalSet::new();
        let exit_status = match local_set.run_until(self.tracer).await {
            Ok(status) => {
                let cleanup_error = self
                    .liteinst_cleanup
                    .as_mut()
                    .and_then(|cleanup| cleanup.finish_typed_completion().err());
                #[cfg(target_arch = "x86_64")]
                let observer_error = Self::finalize_liteinst_physical_observer_after_cleanup(
                    self.guest_pid,
                    self.liteinst_cleanup.as_ref(),
                    &mut self.liteinst_physical_observer,
                )
                .err();
                #[cfg(not(target_arch = "x86_64"))]
                let observer_error =
                    Self::finalize_liteinst_physical_observer(self.guest_pid).err();
                match (cleanup_error, observer_error) {
                    (Some(cleanup_error), Some(observer_error)) => {
                        return Err(anyhow::anyhow!(
                            "LiteInst tracee completion cleanup failed: {cleanup_error}; physical partition also failed: {observer_error}"
                        )
                        .into());
                    }
                    (Some(cleanup_error), None) => {
                        return Err(anyhow::anyhow!(
                            "LiteInst tracee completion cleanup failed: {cleanup_error}"
                        )
                        .into());
                    }
                    (None, Some(observer_error)) => return Err(observer_error),
                    (None, None) => {}
                }
                status
            }
            Err(error) => {
                let cleanup_error = if let Some(cleanup) = self.liteinst_cleanup.as_mut() {
                    cleanup.terminate_and_confirm().err()
                } else {
                    None
                };
                #[cfg(target_arch = "x86_64")]
                let observer_error = Self::finalize_liteinst_physical_observer_after_cleanup(
                    self.guest_pid,
                    self.liteinst_cleanup.as_ref(),
                    &mut self.liteinst_physical_observer,
                )
                .err();
                #[cfg(not(target_arch = "x86_64"))]
                let observer_error =
                    Self::finalize_liteinst_physical_observer(self.guest_pid).err();
                match (cleanup_error, observer_error) {
                    (Some(cleanup_error), Some(observer_error)) => {
                        return Err(anyhow::anyhow!(
                            "LiteInst tracee cleanup failed after {error}: {cleanup_error}; physical partition also failed: {observer_error}"
                        ).into());
                    }
                    (Some(cleanup_error), None) => {
                        return Err(anyhow::anyhow!(
                            "LiteInst tracee cleanup failed after {error}: {cleanup_error}"
                        )
                        .into());
                    }
                    (None, Some(observer_error)) => {
                        return Err(anyhow::anyhow!(
                            "LiteInst physical partition failed after {error}: {observer_error}"
                        )
                        .into());
                    }
                    (None, None) => {}
                }
                return Err(error);
            }
        };

        let g = Arc::try_unwrap(self.gref).unwrap_or_else(|_| {
            panic!("Reverie internal invariant broken. Arc::try_unwrap on global state failed.")
        });

        Ok((exit_status, g))
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

fn init_tracee_capabilities(intercept_rdtsc: bool) -> Result<(), Errno> {
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

    Ok(())
}

/// Sets up the child process for ptracing right before execve is called.
fn init_tracee(intercept_rdtsc: bool) -> Result<(), Errno> {
    init_tracee_capabilities(intercept_rdtsc)?;

    // Establish ptrace ownership while the spawn error pipe is still open, so
    // a TRACEME failure reaches the parent as a real pre-exec error. The child
    // raises SIGSTOP only after closing that pipe below; Command::spawn can
    // therefore return before the stop, and the exact WNOWAIT pidfd barrier
    // owns that finite transition without polling TracerPid.
    safeptrace::traceme()?;

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

    safeptrace::stop_for_tracer()?;
    finish_tracee_init()
}

fn finish_tracee_init() -> Result<(), Errno> {
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

#[cfg(target_arch = "x86_64")]
fn init_controller_tracee(
    intercept_rdtsc: bool,
    publisher: &mut ControllerStartupPublisher,
) -> Result<(), Errno> {
    init_tracee_capabilities(intercept_rdtsc)?;
    safeptrace::traceme()?;
    publisher.publish_ready()?;
    safeptrace::stop_for_tracer()?;
    Errno::result(unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, 0, 0, 0, 0) })?;
    finish_tracee_init()
}

async fn run_orphaned(orphans: mpsc::Receiver<Child>) {
    tokio_stream::wrappers::ReceiverStream::new(orphans)
        .for_each_concurrent(None, |orphan| async {
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
                                unsafe {
                                    libc::kill(pid.as_raw(), libc::SIGKILL);
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
) -> Result<ExitStatus, Error> {
    let root = root.run(child);
    let orphans = run_orphaned(orphanage);
    futures::pin_mut!(root, orphans);
    match future::select(root, orphans).await {
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
    }
}

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
) -> Result<BoxFuture<'static, Result<ExitStatus, Error>>, PostspawnError> {
    let pid = child.pid();

    // Wait for the child to enter a stopped state. The child will enter a
    // stopped state immediately after ptrace::traceme is called.
    //
    // NOTE: We may rarely get spurious signals here, like SIGWINCH, so we must
    // skip past them.
    let held_task_stops = options
        .liteinst_runtime
        .as_ref()
        .map(|runtime| Arc::clone(&runtime.held_task_stops));
    let (mut child, event) = if let Some(held_task_stops) = held_task_stops {
        let mut running = child;
        loop {
            match running.next_state().await? {
                Wait::Stopped(stopped, event) => {
                    HeldRootStop::arm_empty(&held_task_stops, &stopped, &event)?;
                    if event == Event::Signal(Signal::SIGSTOP) {
                        break (stopped, event);
                    }
                    let signal = match event {
                        Event::Signal(signal) => Some(signal),
                        _ => None,
                    };
                    running = RootStopLease::new(stopped, Some(Arc::clone(&held_task_stops)))
                        .resume(signal)?;
                }
                Wait::Exited(pid, exit_status) => {
                    return Err(PostspawnError::Exited { pid, exit_status });
                }
            }
        }
    } else {
        match child.wait_for_signal(Signal::SIGSTOP).await? {
            Wait::Stopped(child, event) => (child, event),
            Wait::Exited(pid, exit_status) => {
                return Err(PostspawnError::Exited { pid, exit_status });
            }
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

    child = tracer.tracee_preinit(child).await?;

    let tracer = Box::pin(run_task_tree(
        tracer,
        child,
        orphan_receiver,
        liteinst_fail_closed,
    ));
    Ok(tracer)
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

/// Creates the after-loader experiment's process-wide syscall filter.
///
/// Tool subscriptions remain a separate dispatch decision. Every syscall is
/// reported to ptrace first, including raw numbers that `Sysno` cannot
/// represent, `rt_sigreturn`, and calls from Reverie's private page. Later
/// admission decides whether the stopped operation may execute.
#[cfg(target_arch = "x86_64")]
fn after_loader_seccomp_filter() -> seccomp::Filter {
    use reverie::process::seccomp::Action;

    seccomp::FilterBuilder::new()
        .default_action(Action::Trace(0))
        .build()
}

#[cfg(target_arch = "x86_64")]
fn spawn_seccomp_filter(events: &Subscription, after_loader: bool) -> seccomp::Filter {
    if after_loader {
        after_loader_seccomp_filter()
    } else {
        seccomp_filter(events)
    }
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
            #[cfg(target_arch = "x86_64")]
            after_loader: None,
            begin_marker,
            ready_marker,
            helper_return_marker,
            syscall_marker,
            newborn_tracees: Arc::new(StdMutex::new(BTreeMap::new())),
            held_task_stops: Arc::new(StdMutex::new(BTreeMap::new())),
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

    /// Select the explicitly bound one-task after-loader experiment.
    /// Call after liteinst_runtime; the constructor launch remains the default.
    #[cfg(all(
        target_arch = "x86_64",
        any(test, feature = "liteinst-after-loader-experiment")
    ))]
    pub fn liteinst_after_loader(
        mut self,
        config: crate::LiteinstAfterLoaderConfig,
    ) -> Result<Self, Error> {
        config
            .validate_environment(&self.command.get_captured_envs())
            .map_err(|error| {
                after_loader_authentication_refusal(
                    LiteinstAfterLoaderAuthenticationStage::CommandEnvironmentAuthentication,
                    error,
                )
            })?;
        let runtime = self.liteinst_runtime.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "LiteInst runtime is absent",
            )
        })?;
        if runtime.preload != config.runtime.path {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "runtime path differs from bound input",
            )
            .into());
        }
        runtime.after_loader = Some(config);
        Ok(self)
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
        #[cfg(target_arch = "x86_64")]
        let after_loader_diagnostics = self
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.after_loader.as_ref())
            .map(|config| config.diagnostics.clone());

        // Because this ptrace backend is CENTRALIZED, it can keep all the
        // tool's state here in a single address space.
        #[cfg(target_arch = "x86_64")]
        if let Some(diagnostics) = &after_loader_diagnostics {
            diagnostics.record(
                "Tool lifecycle: GlobalTool::init_global_state",
                None,
                "begin",
            )?;
        }
        let global_state = <T::GlobalState as GlobalTool>::init_global_state(&config).await;
        #[cfg(target_arch = "x86_64")]
        if let Some(diagnostics) = &after_loader_diagnostics {
            diagnostics.record(
                "Tool lifecycle: GlobalTool::init_global_state",
                None,
                "complete",
            )?;
            diagnostics.record("Tool lifecycle: Tool::subscriptions", None, "begin")?;
        }
        let events = T::subscriptions(&config);
        #[cfg(target_arch = "x86_64")]
        if let Some(diagnostics) = &after_loader_diagnostics {
            diagnostics.record("Tool lifecycle: Tool::subscriptions", None, "complete")?;
        }
        let mut traced_events = events.clone();
        if self.liteinst_runtime.is_some() {
            // Mapping operations are controller-only lifecycle observations:
            // trace them so successful VMA churn can invalidate patched-site
            // provenance, without adding them to the Tool's subscription set.
            traced_events.syscalls([
                Sysno::clone,
                Sysno::clone3,
                Sysno::fork,
                Sysno::mmap,
                Sysno::munmap,
                Sysno::mremap,
                Sysno::mprotect,
                Sysno::pkey_mprotect,
                Sysno::vfork,
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

        #[cfg(target_arch = "x86_64")]
        let after_loader = self
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.after_loader.as_ref());

        #[cfg(target_arch = "x86_64")]
        if let Some(after_loader) = after_loader {
            // The after-loader contract preserves the caller's complete
            // environment. Capture inherited values once, compare that final
            // map with the reviewed configuration, then make the command
            // independent of later mutations to the controller environment.
            // In particular, this mode must not inject the sanitizer variables
            // used by the ordinary ptrace launcher.
            freeze_after_loader_environment(&mut command, &after_loader.environment).map_err(
                |error| {
                    after_loader_authentication_refusal(
                        LiteinstAfterLoaderAuthenticationStage::CommandEnvironmentAuthentication,
                        error,
                    )
                },
            )?;
        } else {
            // Disable sanitizers that use ptrace from running on tracer.
            command.env("LSAN_OPTIONS", "detect_leaks=0");
            command.env("ASAN_OPTIONS", "detect_leaks=0");
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            // Disable sanitizers that use ptrace from running on tracer.
            command.env("LSAN_OPTIONS", "detect_leaks=0");
            command.env("ASAN_OPTIONS", "detect_leaks=0");
        }

        let intercept_rdtsc = events.has_rdtsc();
        #[cfg(all(test, target_arch = "x86_64"))]
        let clock_test_launcher_branches = self.clock_test_launcher_branches;
        #[cfg(target_arch = "x86_64")]
        if after_loader.is_none() {
            unsafe {
                command.pre_exec(move || {
                    init_tracee(intercept_rdtsc)?;
                    // A caller's earlier pre_exec callback runs before init_tracee
                    // stops. This private test seam instead runs after the parent
                    // has constructed the stopped child's clock and resumed it.
                    #[cfg(test)]
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
        }
        #[cfg(not(target_arch = "x86_64"))]
        unsafe {
            command.pre_exec(move || init_tracee(intercept_rdtsc));
        }

        #[cfg(target_arch = "x86_64")]
        command.seccomp(spawn_seccomp_filter(&traced_events, after_loader.is_some()));
        #[cfg(not(target_arch = "x86_64"))]
        command.seccomp(seccomp_filter(&traced_events));

        #[cfg(target_arch = "x86_64")]
        if let Some(after_loader) = after_loader {
            // This is the final check before Env::array serializes the child's
            // envp in spawn. A mismatch remains a launch refusal, never a
            // backend parity result.
            after_loader
                .validate_environment(&command.get_captured_envs())
                .map_err(|error| {
                    after_loader_authentication_refusal(
                        LiteinstAfterLoaderAuthenticationStage::CommandEnvironmentAuthentication,
                        error,
                    )
                })?;
        }

        let mut ordinary_child: Option<ProcessChild> = None;
        let mut controller_stdio: Option<SpawnStdio> = None;
        #[cfg(target_arch = "x86_64")]
        let (guest_pid, running_child) = if after_loader.is_some() {
            // SAFETY: the closure captures only a bool and performs raw
            // prctl/personality/ptrace/write/close/raise/sigaction operations
            // on stack/static data. It neither allocates nor acquires a lock,
            // and every failure is returned instead of unwinding.
            match unsafe {
                command.spawn_controller_with(|publisher| {
                    init_controller_tracee(intercept_rdtsc, publisher)
                })
            } {
                Ok(launch) => {
                    let ControllerLaunchParts {
                        token,
                        stdin,
                        stdout,
                        stderr,
                        phase: _,
                    } = launch.into_parts();
                    let guest_pid = token.child();
                    let running_child = Running::from_controller_launch(token);
                    controller_stdio = Some((stdin, stdout, stderr));
                    (guest_pid, running_child)
                }
                Err(
                    ControllerSpawnError::BeforeClone(error)
                    | ControllerSpawnError::UnsupportedKernelContract(error),
                ) => return Err(Error::Tool(anyhow::Error::new(error))),
                Err(ControllerSpawnError::AfterClone { source, authority }) => {
                    let cause = source.errno();
                    let ControllerLaunchParts {
                        token,
                        stdin: _,
                        stdout: _,
                        stderr: _,
                        phase: _,
                    } = authority.into_parts();
                    let running_child = Running::from_controller_launch(token);
                    return match running_child.cleanup_failed_controller_launch(cause) {
                        Ok(()) => Err(Error::Tool(anyhow::Error::new(source))),
                        Err(cleanup) => Err(Error::Tool(anyhow::Error::new(cleanup).context(
                            format!("controller startup failed before ownership-ready: {source}"),
                        ))),
                    };
                }
            }
        } else {
            let child = command.spawn().context("Failed to spawn tracee")?;
            let guest_pid = child.id();
            let running_child = Running::new(guest_pid);
            ordinary_child = Some(child);
            (guest_pid, running_child)
        };
        #[cfg(not(target_arch = "x86_64"))]
        let (guest_pid, running_child) = {
            let child = command.spawn().context("Failed to spawn tracee")?;
            let guest_pid = child.id();
            let running_child = Running::new(guest_pid);
            ordinary_child = Some(child);
            (guest_pid, running_child)
        };
        if let Some(runtime) = self.liteinst_runtime.as_ref() {
            // Publish the session root before any task can observe the config.
            // Everything LiteInst-root-scoped keys off this exact TID rather
            // than the `tid == pid` shape, which a forked child also has.
            runtime
                .root_tid
                .set(guest_pid)
                .expect("LiteInst root TID is published exactly once per spawn");
        }
        #[cfg(target_arch = "x86_64")]
        let mut liteinst_physical_observer = self
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.after_loader.as_ref())
            .map(|config| (config.physical_observer.clone(), config.diagnostics.clone()));
        let mut liteinst_startup_identity = None;
        let mut liteinst_startup_terminal = None;
        #[cfg(target_arch = "x86_64")]
        let mut liteinst_original_root_launch = None;
        #[cfg(target_arch = "x86_64")]
        if let Some(config) = self
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.after_loader.as_ref())
        {
            let observer_setup = (|| -> Result<(), Error> {
                let launch = running_child
                    .attach_original_root_physical_observer(&config.physical_observer)
                    .map_err(|_| Error::from(Errno::EPROTO))?;
                let generation = running_child.physical_event_generation();
                liteinst_original_root_launch = Some(launch);
                config.diagnostics.record(
                    "physical event observer attached",
                    None,
                    format!(
                        "pid={guest_pid} observer={:?} generation={:?}",
                        config.physical_observer.id(),
                        generation,
                    ),
                )?;
                Ok(())
            })();
            if let Err(error) = observer_setup {
                let observer_was_attached = liteinst_original_root_launch.is_some();
                if let Err(cleanup) = running_child.cleanup_failed_controller_launch(Errno::EPROTO)
                {
                    // Do not close the observer while its exact continuation
                    // authority may still need to append cleanup evidence.
                    return Err(anyhow::Error::new(cleanup)
                        .context(format!("LiteInst physical observer setup failed: {error}"))
                        .into());
                }
                let observer_error = observer_was_attached
                    .then(|| {
                        Tracer::<T::GlobalState>::finalize_liteinst_physical_observer(
                            guest_pid,
                            &mut liteinst_physical_observer,
                        )
                    })
                    .transpose()
                    .err();
                return match observer_error {
                    Some(observer_error) => Err(anyhow::anyhow!(
                        "LiteInst physical observer setup failed: {error}; physical partition also failed: {observer_error}"
                    )
                    .into()),
                    None => Err(error),
                };
            }

            // The one retained exact-pidfd barrier closes Command::spawn's
            // TracerPid race without polling. It either authenticates and
            // returns the exact generation capabilities used by cleanup, or
            // itself consumes/closes a terminal or failed setup generation.
            match running_child.prepare_original_root_continued_status_authority(
                liteinst_original_root_launch
                    .take()
                    .expect("original-root observer setup did not mint launch authority"),
            ) {
                Ok(OriginalRootStartup::Ready(identity)) => {
                    liteinst_startup_identity = Some(identity);
                }
                Ok(OriginalRootStartup::Exited(_, generation, status)) => {
                    liteinst_startup_terminal = Some((generation, status));
                }
                Err(OriginalRootStartupError::BeforeBarrier(error)) => {
                    if let Err(cleanup) = running_child.cleanup_failed_controller_launch(error) {
                        // Preserve the exact Event/pidfd continuation and leave
                        // its observer open for the eventual terminal proof.
                        return Err(anyhow::Error::new(cleanup)
                            .context(format!(
                                "LiteInst startup barrier failed before retaining a status: {error}"
                            ))
                            .into());
                    }
                    let observer_error =
                        Tracer::<T::GlobalState>::finalize_liteinst_physical_observer(
                            guest_pid,
                            &mut liteinst_physical_observer,
                        )
                        .err();
                    return match observer_error {
                        Some(observer) => Err(anyhow::anyhow!(
                            "LiteInst startup barrier failed before retaining a status: {error}; physical partition also failed: {observer}"
                        )
                        .into()),
                        None => Err(anyhow::anyhow!(
                            "LiteInst startup barrier failed before retaining a status: {error}"
                        )
                        .into()),
                    };
                }
                Err(OriginalRootStartupError::CleanedExactGeneration { cause }) => {
                    let observer_error =
                        Tracer::<T::GlobalState>::finalize_liteinst_physical_observer(
                            guest_pid,
                            &mut liteinst_physical_observer,
                        )
                        .err();
                    return match observer_error {
                        Some(observer) => Err(anyhow::anyhow!(
                            "LiteInst startup barrier authentication failed: {cause}; physical partition also failed: {observer}"
                        )
                        .into()),
                        None => Err(anyhow::anyhow!(
                            "LiteInst startup barrier authentication failed: {cause}"
                        )
                        .into()),
                    };
                }
                Err(OriginalRootStartupError::CleanedExactGenerationWithDiagnostic {
                    cause,
                    cleanup_error,
                }) => {
                    let observer_error =
                        Tracer::<T::GlobalState>::finalize_liteinst_physical_observer(
                            guest_pid,
                            &mut liteinst_physical_observer,
                        )
                        .err();
                    return match observer_error {
                        Some(observer) => Err(anyhow::anyhow!(
                            "LiteInst startup barrier authentication failed: {cause}; exact cleanup diagnostic: {cleanup_error}; physical partition also failed: {observer}"
                        )
                        .into()),
                        None => Err(anyhow::anyhow!(
                            "LiteInst startup barrier authentication failed: {cause}; exact cleanup diagnostic: {cleanup_error}"
                        )
                        .into()),
                    };
                }
                Err(
                    error @ (OriginalRootStartupError::CleanupIncomplete { .. }
                    | OriginalRootStartupError::CleanupAlreadyClaimed { .. }),
                ) => {
                    // Preserve the sole exact-Event/pidfd continuation
                    // authority inside the returned typed error. Startup does
                    // not retry a semantic cleanup operation here.
                    return Err(anyhow::Error::new(error).into());
                }
            }
        }
        let liteinst_newborn_tracees = self
            .liteinst_runtime
            .as_ref()
            .map(|runtime| Arc::clone(&runtime.newborn_tracees));
        let liteinst_held_task_stops = self
            .liteinst_runtime
            .as_ref()
            .map(|runtime| Arc::clone(&runtime.held_task_stops));
        let liteinst_instrumentation_stats = self
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.instrumentation_stats.as_ref().map(Arc::clone));
        if let Some((generation, exit_status)) = liteinst_startup_terminal {
            if generation != running_child.physical_event_generation() {
                return Err(anyhow::anyhow!(
                    "LiteInst startup terminal generation changed: observed={generation:?}, running={:?}",
                    running_child.physical_event_generation(),
                )
                .into());
            }
            #[cfg(target_arch = "x86_64")]
            Tracer::<T::GlobalState>::finalize_liteinst_physical_observer(
                guest_pid,
                &mut liteinst_physical_observer,
            )?;
            let (stdin, stdout, stderr) =
                take_spawn_stdio(&mut ordinary_child, &mut controller_stdio);
            return Ok(Tracer {
                guest_pid,
                tracer: Box::pin(async move { Ok(exit_status) }),
                gref,
                stdin,
                stdout,
                stderr,
                liteinst_cleanup: None,
                liteinst_instrumentation_stats,
                #[cfg(target_arch = "x86_64")]
                liteinst_physical_observer,
                backend_stats,
            });
        }
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
        #[cfg(target_arch = "x86_64")]
        let liteinst_requires_exact_startup_identity = liteinst_physical_observer.is_some();
        #[cfg(not(target_arch = "x86_64"))]
        let liteinst_requires_exact_startup_identity = false;
        let mut liteinst_cleanup = if liteinst_fail_closed {
            let (Some(newborn_tracees), Some(held_task_stops)) =
                (liteinst_newborn_tracees, liteinst_held_task_stops)
            else {
                unreachable!("LiteInst fail-closed mode requires its runtime ownership maps");
            };
            let cleanup = if let Some(startup_identity) = liteinst_startup_identity.take() {
                Ok(LiteinstTraceeCleanup::new_after_loader(
                    &running_child,
                    startup_identity,
                    newborn_tracees,
                    held_task_stops,
                ))
            } else if !liteinst_requires_exact_startup_identity {
                LiteinstTraceeCleanup::new_dynamic(&running_child, newborn_tracees, held_task_stops)
            } else {
                let cleanup_result =
                    unsafe { running_child.terminate_unregistered_original_root(Errno::EPROTO) };
                #[cfg(target_arch = "x86_64")]
                let observer_error = Tracer::<T::GlobalState>::finalize_liteinst_physical_observer(
                    guest_pid,
                    &mut liteinst_physical_observer,
                )
                .err();
                #[cfg(not(target_arch = "x86_64"))]
                let observer_error = None::<Error>;
                return Err(anyhow::anyhow!(
                    "LiteInst startup ownership was incomplete before cleanup construction; exact_pidfd_cleanup={cleanup_result:?}; physical_partition={observer_error:?}"
                )
                .into());
            };
            let cleanup = match cleanup {
                Ok(cleanup) => cleanup,
                Err(setup_error) => {
                    // Default dynamic LiteInst still owns the just-spawned,
                    // unreaped child, so its numeric PID cannot have been
                    // reused. This containment fallback is mode-local: the
                    // after-loader path above never falls back from its exact
                    // controller-spawn identity.
                    let kill_result = unsafe { libc::kill(guest_pid.as_raw(), libc::SIGKILL) };
                    let kill_error = (kill_result == -1).then(Errno::last);
                    let drain_result = drain_unregistered_child(guest_pid);
                    return Err(liteinst_pidfd_setup_error(
                        guest_pid,
                        setup_error,
                        kill_error,
                        drain_result,
                    )
                    .into());
                }
            };
            #[cfg(test)]
            let cleanup = {
                let mut cleanup = cleanup;
                cleanup.fail_discovery_once = fail_discovery_once;
                cleanup.fail_after_scan_once = fail_after_scan_once;
                cleanup.force_task_scan_once = force_task_scan_once;
                cleanup
            };
            Some(cleanup)
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

                let mut server = match server
                    .with_context(|| format!("failed to start GDB server for tracee {guest_pid}"))
                {
                    Ok(server) => server,
                    Err(error) => {
                        let cleanup_error = liteinst_cleanup
                            .as_mut()
                            .and_then(|cleanup| cleanup.terminate_and_confirm().err());
                        #[cfg(target_arch = "x86_64")]
                        let observer_error =
                            Tracer::<T::GlobalState>::finalize_liteinst_physical_observer_after_cleanup(
                                guest_pid,
                                liteinst_cleanup.as_ref(),
                                &mut liteinst_physical_observer,
                            )
                            .err();
                        #[cfg(not(target_arch = "x86_64"))]
                        let observer_error = None::<Error>;
                        return match (cleanup_error, observer_error) {
                            (Some(cleanup_error), Some(observer_error)) => Err(anyhow::anyhow!(
                                "{error}; LiteInst tracee cleanup failed: {cleanup_error}; physical partition also failed: {observer_error}"
                            )
                            .into()),
                            (Some(cleanup_error), None) => Err(anyhow::anyhow!(
                                "{error}; LiteInst tracee cleanup failed: {cleanup_error}"
                            )
                            .into()),
                            (None, Some(observer_error)) => Err(anyhow::anyhow!(
                                "{error}; physical partition also failed: {observer_error}"
                            )
                            .into()),
                            (None, None) => Err(error.into()),
                        };
                    }
                };

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
        if let Some(cleanup) = liteinst_cleanup.as_mut()
            && let Err(registration_error) = cleanup.register_notifier(&running_child)
        {
            let cleanup_error = cleanup.terminate_and_confirm().err();
            #[cfg(target_arch = "x86_64")]
            let observer_error =
                Tracer::<T::GlobalState>::finalize_liteinst_physical_observer_after_cleanup(
                    guest_pid,
                    liteinst_cleanup.as_ref(),
                    &mut liteinst_physical_observer,
                )
                .err();
            #[cfg(not(target_arch = "x86_64"))]
            let observer_error = None::<Error>;
            return Err(anyhow::anyhow!(
                "LiteInst notifier registration failed: {registration_error}; cleanup={cleanup_error:?}; physical_partition={observer_error:?}"
            )
            .into());
        }

        let tracer = match postspawn::<T>(
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
                let cleanup_error = liteinst_cleanup
                    .as_mut()
                    .and_then(|cleanup| cleanup.terminate_and_confirm().err());
                #[cfg(target_arch = "x86_64")]
                let observer_error =
                    Tracer::<T::GlobalState>::finalize_liteinst_physical_observer_after_cleanup(
                        guest_pid,
                        liteinst_cleanup.as_ref(),
                        &mut liteinst_physical_observer,
                    )
                    .err();
                #[cfg(not(target_arch = "x86_64"))]
                let observer_error = None::<Error>;
                return match (cleanup_error, observer_error) {
                    (Some(cleanup_error), Some(observer_error)) => Err(anyhow::anyhow!(
                        "LiteInst tracee cleanup failed after {error}: {cleanup_error}; physical partition also failed: {observer_error}"
                    )
                    .into()),
                    (Some(cleanup_error), None) => Err(anyhow::anyhow!(
                        "LiteInst tracee cleanup failed after {error}: {cleanup_error}"
                    )
                    .into()),
                    (None, Some(observer_error)) => Err(anyhow::anyhow!(
                        "LiteInst physical partition failed after {error}: {observer_error}"
                    )
                    .into()),
                    (None, None) => Err(error),
                };
            }
        };

        // The ordinary Child is still forgotten at this exact handoff, while
        // the controller path has no Child/numeric-wait capability at all.
        let (stdin, stdout, stderr) = take_spawn_stdio(&mut ordinary_child, &mut controller_stdio);

        Ok(Tracer {
            guest_pid,
            tracer,
            gref,
            stdin,
            stdout,
            stderr,
            liteinst_cleanup,
            liteinst_instrumentation_stats,
            #[cfg(target_arch = "x86_64")]
            liteinst_physical_observer,
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
            let tracer = match postspawn::<L>(
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
                gref,
                stdin: None,
                stdout: Some(stdout),
                stderr: Some(stderr),
                liteinst_cleanup: None,
                liteinst_instrumentation_stats: None,
                #[cfg(target_arch = "x86_64")]
                liteinst_physical_observer: None,
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

#[cfg(test)]
mod tests {
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

    #[cfg(target_arch = "x86_64")]
    fn evaluate_seccomp_filter(
        filter: &seccomp::Filter,
        architecture: u32,
        syscall_number: u32,
        instruction_pointer: u64,
    ) -> u32 {
        const BPF_LD_W_ABS: u16 = 0x20;
        const BPF_LD_W_MEM: u16 = 0x60;
        const BPF_ST: u16 = 0x02;
        const BPF_JMP_JEQ_K: u16 = 0x15;
        const BPF_JMP_JGT_K: u16 = 0x25;
        const BPF_JMP_JGE_K: u16 = 0x35;
        const BPF_RET_K: u16 = 0x06;

        let mut accumulator = 0u32;
        let mut memory = [0u32; 16];
        let mut pc = 0usize;
        let instructions = filter.instructions();
        while let Some(instruction) = instructions.get(pc) {
            match instruction.code {
                BPF_LD_W_ABS => {
                    accumulator = match instruction.k {
                        0 => syscall_number,
                        4 => architecture,
                        8 => instruction_pointer as u32,
                        12 => (instruction_pointer >> 32) as u32,
                        offset => panic!("unexpected seccomp_data load offset {offset}"),
                    };
                    pc += 1;
                }
                BPF_LD_W_MEM => {
                    accumulator = memory[instruction.k as usize];
                    pc += 1;
                }
                BPF_ST => {
                    memory[instruction.k as usize] = accumulator;
                    pc += 1;
                }
                BPF_JMP_JEQ_K | BPF_JMP_JGT_K | BPF_JMP_JGE_K => {
                    let matches = match instruction.code {
                        BPF_JMP_JEQ_K => accumulator == instruction.k,
                        BPF_JMP_JGT_K => accumulator > instruction.k,
                        BPF_JMP_JGE_K => accumulator >= instruction.k,
                        _ => unreachable!(),
                    };
                    pc += 1 + usize::from(if matches {
                        instruction.jt
                    } else {
                        instruction.jf
                    });
                }
                BPF_RET_K => return instruction.k,
                code => panic!("unexpected seccomp-BPF instruction {code:#x}"),
            }
        }
        panic!("seccomp-BPF program reached the end without returning")
    }

    #[cfg(target_arch = "x86_64")]
    fn seccomp_action(
        filter: &seccomp::Filter,
        syscall_number: u32,
        instruction_pointer: u64,
    ) -> u32 {
        // AUDIT_ARCH_X86_64 from linux/audit.h.
        evaluate_seccomp_filter(filter, 0xc000_003e, syscall_number, instruction_pointer)
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn after_loader_filter_traces_subscribed_unsubscribed_unknown_and_x32_syscalls() {
        let mut subscriptions = Subscription::none();
        subscriptions.syscall(Sysno::write);
        let filter = spawn_seccomp_filter(&subscriptions, true);
        let guest_ip = 0x0040_1000;
        let expected = libc::SECCOMP_RET_TRACE;

        assert_eq!(
            filter.instructions().len(),
            4,
            "strict filter must contain only architecture validation and its Trace default",
        );
        assert!(
            filter
                .instructions()
                .iter()
                .all(|instruction| instruction.code != 0x06
                    || instruction.k != libc::SECCOMP_RET_ALLOW),
            "strict filter must contain no Allow return",
        );
        assert_eq!(
            filter
                .instructions()
                .iter()
                .filter(|instruction| instruction.code == 0x20)
                .map(|instruction| instruction.k)
                .collect::<Vec<_>>(),
            [4],
            "strict filter must inspect the architecture, not syscall number or IP",
        );

        assert_eq!(
            seccomp_action(&filter, Sysno::write as i32 as u32, guest_ip),
            expected,
            "subscribed syscall must reach after-loader admission",
        );
        assert_eq!(
            seccomp_action(&filter, Sysno::getpid as i32 as u32, guest_ip),
            expected,
            "unsubscribed syscall must reach after-loader admission",
        );
        assert_eq!(
            seccomp_action(&filter, 0x3fff_fffe, guest_ip),
            expected,
            "raw unknown syscall number must not require Sysno conversion",
        );
        assert_eq!(
            seccomp_action(&filter, 0x4000_0000 | Sysno::getpid as i32 as u32, guest_ip,),
            expected,
            "x32-marked syscall number must not bypass admission",
        );
        assert_eq!(
            evaluate_seccomp_filter(&filter, 0, Sysno::write as i32 as u32, guest_ip,),
            libc::SECCOMP_RET_KILL_PROCESS,
            "strict filter must retain architecture validation",
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn after_loader_filter_has_no_rt_sigreturn_or_private_page_allow_rule() {
        let mut subscriptions = Subscription::none();
        subscriptions.syscalls([Sysno::write, Sysno::rt_sigreturn]);
        let filter = spawn_seccomp_filter(&subscriptions, true);
        let private_ip = (cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE) as u64;

        assert_eq!(
            seccomp_action(&filter, Sysno::rt_sigreturn as i32 as u32, 0x0040_1000,),
            libc::SECCOMP_RET_TRACE,
            "rt_sigreturn must reach after-loader admission",
        );
        assert_eq!(
            seccomp_action(&filter, Sysno::write as i32 as u32, private_ip),
            libc::SECCOMP_RET_TRACE,
            "private-page syscall must reach after-loader admission",
        );
        assert_eq!(
            seccomp_action(&filter, 0x3fff_fffe, private_ip),
            libc::SECCOMP_RET_TRACE,
            "raw private-page syscall must reach after-loader admission",
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn ordinary_filter_keeps_subscription_and_private_page_policy() {
        let mut subscriptions = Subscription::none();
        subscriptions.syscalls([Sysno::write, Sysno::rt_sigreturn]);
        let filter = spawn_seccomp_filter(&subscriptions, false);
        let guest_ip = 0x0040_1000;
        let private_ip = (cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE) as u64;

        assert_eq!(
            seccomp_action(&filter, Sysno::write as i32 as u32, guest_ip),
            libc::SECCOMP_RET_TRACE,
        );
        assert_eq!(
            seccomp_action(&filter, Sysno::getpid as i32 as u32, guest_ip),
            libc::SECCOMP_RET_ALLOW,
        );
        assert_eq!(
            seccomp_action(&filter, Sysno::rt_sigreturn as i32 as u32, guest_ip,),
            libc::SECCOMP_RET_ALLOW,
        );
        assert_eq!(
            seccomp_action(&filter, Sysno::write as i32 as u32, private_ip),
            libc::SECCOMP_RET_ALLOW,
        );
    }

    #[test]
    fn after_loader_environment_freeze_preserves_exact_map_and_adds_no_sanitizer_values() {
        let expected = BTreeMap::from([
            (OsString::from("PATH"), OsString::from("/usr/bin")),
            (OsString::from("FIXTURE"), OsString::from("after-loader")),
        ]);
        let mut command = Command::new("/bin/true");
        command.env_clear().envs(&expected);

        freeze_after_loader_environment(&mut command, &expected)
            .expect("capture exact reviewed environment");
        assert_eq!(command.get_captured_envs(), expected);
        assert!(
            !command
                .get_captured_envs()
                .contains_key(std::ffi::OsStr::new("LSAN_OPTIONS"))
        );
        assert!(
            !command
                .get_captured_envs()
                .contains_key(std::ffi::OsStr::new("ASAN_OPTIONS"))
        );

        command.env("ASAN_OPTIONS", "detect_leaks=0");
        assert!(freeze_after_loader_environment(&mut command, &expected).is_err());
        assert_eq!(
            command
                .get_captured_envs()
                .get(std::ffi::OsStr::new("ASAN_OPTIONS")),
            Some(&OsString::from("detect_leaks=0"))
        );

        let mut empty = Command::new("/bin/true");
        empty.env_clear();
        freeze_after_loader_environment(&mut empty, &BTreeMap::new())
            .expect("capture explicit empty environment");
        assert!(empty.get_captured_envs().is_empty());
    }

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

    fn synthetic_tracee_identity(
        tid: Pid,
        tgid: Pid,
        ppid: Pid,
        tracer_pid: Pid,
        start_time: u64,
        proc_inode: u64,
        parent: Option<(Pid, Pid, Option<ChildOp>)>,
    ) -> TraceeIdentity {
        let proc_dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open("/proc/self")
            .expect("open synthetic identity anchor");
        TraceeIdentity {
            tid,
            snapshot: TraceeSnapshot {
                tgid,
                ppid,
                tracer_pid,
                start_time,
            },
            proc_dir: proc_dir.into(),
            proc_inode,
            pidfd: None,
            parent,
        }
    }

    fn observer_raw_attempt_counts(observer: &PhysicalEventObserver) -> (usize, usize) {
        observer
            .snapshot()
            .records()
            .iter()
            .fold((0, 0), |(resumes, signals), record| match record.kind() {
                safeptrace::PhysicalEventRecordKind::ResumeAttempt { .. } => (resumes + 1, signals),
                safeptrace::PhysicalEventRecordKind::PidfdSignalAttempt { .. } => {
                    (resumes, signals + 1)
                }
                _ => (resumes, signals),
            })
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

    fn held_task_stops(task: &Stopped, event: &Event) -> HeldTaskStops {
        let mut stops = BTreeMap::new();
        stops.insert(
            HeldTaskStopKey::from_stopped(task),
            HeldRootStop::from_event(task, event),
        );
        Arc::new(StdMutex::new(stops))
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

        let slot = held_task_stops(&stopped, &Event::Signal(Signal::SIGSTOP));
        let running = RootStopLease::new(stopped, Some(Arc::clone(&slot)))
            .resume(None)
            .expect("resume held-stop child");
        assert!(
            slot.lock().unwrap().is_empty(),
            "normal transition left a stale lease"
        );

        let exited = running
            .next_state()
            .await
            .expect("wait resumed held-stop child");
        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_transition_retains_exact_task_stop_for_cleanup() {
        let (pid, stopped, observer) = spawn_observed_held_stop_child("failed transition child");
        let generation = stopped.terminal_cleanup();
        let physical_status = stopped
            .physical_status_id()
            .expect("observed stop carries its physical status");
        let slot = held_task_stops(&stopped, &Event::Signal(Signal::SIGSTOP));
        let transition_slot = Arc::clone(&slot);
        let result = std::thread::spawn(move || {
            RootStopLease::new(stopped, Some(transition_slot)).resume(None)
        })
        .join()
        .expect("join wrong-thread ptrace transition");
        assert!(matches!(result, Err(TraceError::Died(_))));
        {
            let held = slot.lock().unwrap();
            let held = held
                .get(&HeldTaskStopKey::from_terminal(pid, &generation))
                .expect("failed transition discarded cleanup ownership");
            assert!(held.armed);
            assert!(held.terminal.same_generation(&generation));
            assert_eq!(
                held.cleanup_transfer
                    .as_ref()
                    .and_then(CleanupStopTransfer::physical_status_id),
                Some(physical_status)
            );
        }

        let mut held = slot
            .lock()
            .unwrap()
            .remove(&HeldTaskStopKey::from_terminal(pid, &generation))
            .expect("failed transition retained cleanup stop");
        assert_eq!(unsafe { libc::kill(pid.as_raw(), libc::SIGKILL) }, 0);
        assert!(
            held.terminal.wait(Duration::from_secs(2)),
            "killed failed-transition tracee did not reach terminal cleanup"
        );
        assert!(held.terminal.pending_is_empty());
        held.disarm();
        let cleanup_lease = held
            .terminal
            .lease_transferred_stop(&mut held.cleanup_transfer)
            .expect("activate failed transition cleanup transfer");
        finish_cancelled_stop(&held.terminal, Some(cleanup_lease))
            .expect("dispose failed transition stop");
        observer.close();

        let snapshot = observer.snapshot();
        let dispositions = snapshot
            .records()
            .iter()
            .filter_map(|record| match record.kind() {
                safeptrace::PhysicalEventRecordKind::StatusDisposition {
                    status,
                    disposition,
                } if status == physical_status => Some(disposition),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            dispositions,
            [PhysicalStatusDisposition::CancellationCleanup]
        );
        assert!(snapshot.validate().is_valid(), "{:#?}", snapshot.validate());
        assert_eventually_reaped("failed transition child", pid);
    }

    #[test]
    fn registered_cleanup_resumes_exit_stop_that_superseded_retained_stop() {
        for (role, owner) in [
            (
                "root cleanup exit supersession child",
                PhysicalResumeOwner::RootCleanup,
            ),
            (
                "descendant cleanup exit supersession child",
                PhysicalResumeOwner::DescendantCleanup,
            ),
        ] {
            let (pid, stopped, observer, identity, event_link, recorded_parent) =
                if owner == PhysicalResumeOwner::DescendantCleanup {
                    let (parent_pid, pid, control) = fork_paused_grandchild();
                    ptrace::attach(pid.into()).expect("attach cleanup descendant");
                    let running = Running::new(pid);
                    let observer =
                        PhysicalEventObserver::new(PhysicalEventObserverConfig::new(128, 32))
                            .expect("create descendant-cleanup observer");
                    running
                        .attach_physical_event_observer(&observer)
                        .expect("attach descendant observer before wait ownership");
                    let (stopped, event) = running
                        .wait()
                        .expect("wait for cleanup descendant attach")
                        .assume_stopped();
                    assert_eq!(event, Event::Signal(Signal::SIGSTOP));
                    let identity =
                        TraceeIdentity::capture(pid, Some((parent_pid, Some(ChildOp::Fork))), true)
                            .expect("capture production-shaped cleanup descendant identity");
                    let event_link = EventChildLink {
                        generation: stopped.physical_event_generation(),
                        tid: pid,
                        parent_tid: parent_pid,
                        op: ChildOp::Fork,
                    };
                    assert_eq!(
                        identity.parent,
                        Some((parent_pid, parent_pid, Some(ChildOp::Fork)))
                    );
                    (
                        pid,
                        stopped,
                        observer,
                        identity,
                        Some(event_link),
                        Some((parent_pid, control)),
                    )
                } else {
                    let (pid, stopped, observer) = spawn_observed_held_stop_child(role);
                    let identity =
                        TraceeIdentity::open_root(pid).expect("capture cleanup root identity");
                    (pid, stopped, observer, identity, None, None)
                };
            let retained_status = stopped
                .physical_status_id()
                .expect("observed retained stop carries its physical status");
            stopped
                .setoptions(ptrace::Options::PTRACE_O_TRACEEXIT)
                .expect("enable exit-stop observation for cleanup child");
            let terminal = stopped.terminal_cleanup();
            let retained_stop = stopped
                .into_cleanup_stop_lease()
                .expect("lease retained stop for cancellation cleanup");

            identity
                .send_signal(Signal::SIGKILL)
                .expect("send exact-pidfd SIGKILL to cleanup child");
            let deadline = Instant::now() + Duration::from_secs(2);
            while terminal.exit_stop_physical_status_id().is_none() && Instant::now() < deadline {
                std::thread::yield_now();
            }
            assert!(
                terminal.exit_stop_observed(),
                "cleanup child did not publish its PTRACE_EVENT_EXIT stop"
            );
            let exit_status = terminal
                .exit_stop_physical_status_id()
                .expect("published exit stop carries its physical status");
            assert_ne!(retained_status, exit_status);

            let (terminal, frozen_stop) = if owner == PhysicalResumeOwner::DescendantCleanup {
                let mut registered = RegisteredTraceeCleanup {
                    identity,
                    terminal,
                    event_link,
                    frozen_stop: Some(retained_stop),
                    signal_authority: true,
                };
                assert_eq!(registered.event_link, event_link);
                registered
                    .continue_exit_stop()
                    .expect("continue registered descendant exit stop");
                registered
                    .continue_exit_stop()
                    .expect("recognize already-finished descendant exit stop");
                (registered.terminal, registered.frozen_stop)
            } else {
                drop(identity);
                let mut frozen_stop = Some(retained_stop);
                continue_registered_exit_stop(&terminal, &mut frozen_stop, owner)
                    .expect("continue exact root exit stop");
                continue_registered_exit_stop(&terminal, &mut frozen_stop, owner)
                    .expect("recognize already-finished root exit stop");
                (terminal, frozen_stop)
            };
            assert!(frozen_stop.is_none());
            assert!(
                terminal.wait(Duration::from_secs(2)),
                "cleanup child notifier did not publish terminal state"
            );
            assert!(terminal.pending_is_empty());
            observer.close();

            let snapshot = observer.snapshot();
            let validation = snapshot.validate();
            assert_eq!(validation.physical_statuses, 3);
            assert_eq!(validation.successful_resumes, 1);
            assert_eq!(validation.explicit_dispositions, 2);
            assert!(validation.is_valid(), "{validation:#?}");
            let dispositions = snapshot
                .records()
                .iter()
                .filter_map(|record| match record.kind() {
                    safeptrace::PhysicalEventRecordKind::StatusDisposition {
                        status,
                        disposition,
                    } => Some((status, disposition)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                dispositions,
                [(
                    retained_status,
                    PhysicalStatusDisposition::KernelSupersededByExitStop,
                )]
            );
            let resume_sources = snapshot
                .records()
                .iter()
                .filter_map(|record| match record.kind() {
                    safeptrace::PhysicalEventRecordKind::ResumeAttempt { context, .. } => {
                        Some((context.source_status, context.owner))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(resume_sources, [(Some(exit_status), owner)]);
            if let Some((parent_pid, mut control)) = recorded_parent {
                control
                    .write_all(&[1])
                    .expect("release cleanup descendant's recorded parent");
                drop(control);
                Running::new(parent_pid)
                    .wait()
                    .expect("reap cleanup descendant's recorded parent");
            }
            assert_eventually_reaped(role, pid);
        }
    }

    #[test]
    fn registered_cleanup_remembers_unobserved_exit_stop_completion() {
        let role = "unobserved registered cleanup child";
        let (pid, stopped) = spawn_held_stop_child(role);
        stopped
            .setoptions(ptrace::Options::PTRACE_O_TRACEEXIT)
            .expect("enable unobserved exit-stop cleanup");
        let terminal = stopped.terminal_cleanup();
        let retained_stop = stopped
            .into_cleanup_stop_lease()
            .expect("lease observerless retained stop for cleanup");
        let identity =
            TraceeIdentity::open_root(pid).expect("capture unobserved cleanup child identity");
        identity
            .send_signal(Signal::SIGKILL)
            .expect("kill unobserved cleanup child through pidfd");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !terminal.exit_stop_observed() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            terminal.exit_stop_observed(),
            "unobserved cleanup child did not publish its exit stop"
        );
        assert_eq!(terminal.exit_stop_physical_status_id(), None);

        let mut registered = RegisteredTraceeCleanup {
            identity,
            terminal,
            event_link: None,
            frozen_stop: Some(retained_stop),
            signal_authority: true,
        };
        registered
            .continue_exit_stop()
            .expect("continue unobserved registered exit stop");
        assert!(registered.frozen_stop.is_none());
        registered
            .continue_exit_stop()
            .expect("recognize completed unobserved registered exit stop");
        assert!(registered.frozen_stop.is_none());
        assert!(
            registered.terminal.wait(Duration::from_secs(2)),
            "unobserved cleanup notifier did not publish terminal state"
        );
        assert!(registered.terminal.pending_is_empty());
        drop(registered);
        assert_eventually_reaped(role, pid);
    }

    fn spawn_observed_held_stop_child(role: &str) -> (Pid, Stopped, PhysicalEventObserver) {
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
        let running = Running::new(pid);
        let observer = PhysicalEventObserver::new(PhysicalEventObserverConfig::new(128, 32))
            .expect("create failed-transition observer");
        running
            .attach_physical_event_observer(&observer)
            .expect("attach failed-transition observer before wait ownership");
        let (stopped, event) = running
            .wait()
            .unwrap_or_else(|error| panic!("wait {role}: {error}"))
            .assume_stopped();
        assert_eq!(event, Event::Signal(Signal::SIGSTOP));
        (pid, stopped, observer)
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

    fn spawn_gated_held_stop_child(role: &str) -> (Pid, Stopped, std::os::unix::net::UnixStream) {
        let (control, mut child_control) =
            std::os::unix::net::UnixStream::pair().expect("create held-stop control socket");
        let pid = match unsafe { unistd::fork() }
            .unwrap_or_else(|error| panic!("fork {role}: {error}"))
        {
            ForkResult::Child => {
                drop(control);
                safeptrace::traceme_and_stop()
                    .unwrap_or_else(|error| panic!("TRACEME {role}: {error}"));
                let mut release = [0];
                std::io::Read::read_exact(&mut child_control, &mut release)
                    .unwrap_or_else(|error| panic!("read {role} release: {error}"));
                unsafe { libc::_exit(0) };
            }
            ForkResult::Parent { child } => {
                drop(child_control);
                Pid::from(child)
            }
        };
        let (stopped, event) = Running::new(pid)
            .wait()
            .unwrap_or_else(|error| panic!("wait {role}: {error}"))
            .assume_stopped();
        assert_eq!(event, Event::Signal(Signal::SIGSTOP));
        (pid, stopped, control)
    }

    fn wait_for_pending_cleanup_stop(terminal: &TerminalCleanup, role: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while terminal.pending_is_empty() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            !terminal.pending_is_empty(),
            "{role} did not publish its later notifier FIFO stop"
        );
    }

    #[test]
    fn capture_conflict_rolls_back_fifo_before_same_tid_shadow_retry() {
        let role = "same-tid capture-conflict child";
        let (pid, stopped, mut control) = spawn_gated_held_stop_child(role);
        let old_stop_id = stopped.logical_stop_id();
        let terminal = stopped.terminal_cleanup();
        let capture_terminal = stopped.terminal_cleanup();
        ptrace::cont(pid.into(), None).expect("resume capture-conflict child directly");
        signal::kill(pid.into(), Signal::SIGSTOP).expect("stop capture-conflict child again");
        wait_for_pending_cleanup_stop(&terminal, role);

        let held_task_stops = Arc::new(StdMutex::new(BTreeMap::new()));
        let newborn_tracees = Arc::new(StdMutex::new(BTreeMap::new()));
        let preflight_captured = Arc::new(Barrier::new(2));
        let preflight_resume = Arc::new(Barrier::new(2));
        let decoded_captured = Arc::new(Barrier::new(2));
        let decoded_resume = Arc::new(Barrier::new(2));
        CLEANUP_CAPTURE_PREFLIGHT_PAUSES.lock().unwrap().insert(
            terminal.physical_event_generation(),
            CleanupCapturePreflightPause {
                captured: Arc::clone(&preflight_captured),
                resume: Arc::clone(&preflight_resume),
            },
        );
        CLEANUP_CAPTURE_DECODED_PAUSES.lock().unwrap().insert(
            terminal.physical_event_generation(),
            CleanupCapturePreflightPause {
                captured: Arc::clone(&decoded_captured),
                resume: Arc::clone(&decoded_resume),
            },
        );
        let capture_held = Arc::clone(&held_task_stops);
        let capture_newborns = Arc::clone(&newborn_tracees);
        let capture = std::thread::spawn(move || {
            let mut retained = None;
            let result = LiteinstTraceeCleanup::capture_pending_children(
                &capture_newborns,
                &capture_held,
                &capture_terminal,
                &mut retained,
            );
            (result, retained)
        });
        preflight_captured.wait();
        HeldRootStop::arm_empty(&held_task_stops, &stopped, &Event::Signal(Signal::SIGSTOP))
            .expect("arm competing same-TID shadow after vacancy preflight");
        let held = held_task_stops.lock().unwrap();
        let shadow = held
            .get(&HeldTaskStopKey::from_terminal(pid, &terminal))
            .expect("competing shadow remains durable");
        assert_eq!(
            shadow
                .cleanup_transfer
                .as_ref()
                .expect("armed competing shadow carries its transfer")
                .logical_stop_id(),
            old_stop_id
        );
        drop(stopped);
        preflight_resume.wait();
        decoded_captured.wait();
        decoded_resume.wait();
        assert!(
            !terminal.pending_is_empty(),
            "try_lock contention did not release and roll back StatusState"
        );
        drop(held);
        let (conflict, mut retained) = capture.join().expect("join capture-conflict thread");
        conflict.expect("competing same-TID shadow is recoverable cleanup progress");
        assert!(
            !terminal.pending_is_empty(),
            "same-TID conflict committed and lost the notifier FIFO front"
        );
        assert!(newborn_tracees.lock().unwrap().is_empty());
        assert!(held_task_stops.lock().unwrap().is_empty());
        let old_lease = retained
            .take()
            .expect("conflict resolution activated the existing durable owner");
        assert_eq!(old_lease.logical_stop_id(), old_stop_id);
        terminal
            .dispose_cleanup_stop(old_lease)
            .expect("retire conflict control owner before FIFO retry");

        LiteinstTraceeCleanup::capture_pending_children(
            &newborn_tracees,
            &held_task_stops,
            &terminal,
            &mut retained,
        )
        .expect("retry exact FIFO front after competing shadow clears");
        let later_lease = retained
            .take()
            .expect("successful retry retains the later committed stop");
        assert_ne!(later_lease.logical_stop_id(), old_stop_id);
        assert!(terminal.pending_is_empty());
        terminal
            .dispose_cleanup_stop(later_lease)
            .expect("dispose later capture-conflict stop");

        control
            .write_all(&[1])
            .expect("release capture-conflict child");
        ptrace::cont(pid.into(), None).expect("resume later capture-conflict stop");
        assert!(terminal.wait(Duration::from_secs(2)));
        assert_eventually_reaped(role, pid);
    }

    #[test]
    fn committed_conversion_error_installs_successor_until_old_lease_retires() {
        let role = "committed conversion-error child";
        let (pid, stopped, mut control) = spawn_gated_held_stop_child(role);
        let old_stop_id = stopped.logical_stop_id();
        let terminal = stopped.terminal_cleanup();
        let capture_terminal = stopped.terminal_cleanup();
        let mut raw_cont_calls = 0usize;
        ptrace::cont(pid.into(), None).expect("resume conversion-error child directly");
        raw_cont_calls += 1;
        signal::kill(pid.into(), Signal::SIGSTOP).expect("stop conversion-error child again");
        wait_for_pending_cleanup_stop(&terminal, role);

        let held_task_stops = Arc::new(StdMutex::new(BTreeMap::new()));
        let newborn_tracees = Arc::new(StdMutex::new(BTreeMap::new()));
        let preflight_captured = Arc::new(Barrier::new(2));
        let preflight_resume = Arc::new(Barrier::new(2));
        CLEANUP_CAPTURE_PREFLIGHT_PAUSES.lock().unwrap().insert(
            terminal.physical_event_generation(),
            CleanupCapturePreflightPause {
                captured: Arc::clone(&preflight_captured),
                resume: Arc::clone(&preflight_resume),
            },
        );
        let capture_held = Arc::clone(&held_task_stops);
        let capture_newborns = Arc::clone(&newborn_tracees);
        let capture = std::thread::spawn(move || {
            let mut retained = None;
            let result = LiteinstTraceeCleanup::capture_pending_children(
                &capture_newborns,
                &capture_held,
                &capture_terminal,
                &mut retained,
            );
            assert!(retained.is_none());
            result
        });
        preflight_captured.wait();
        // Force the cross-registry race after capture's vacancy preflight. The
        // direct raw transition above deliberately leaves this stale typed
        // value available so the test can install the older authority at this
        // exact boundary; production transitions cannot retain it this way.
        let old_lease = stopped
            .into_cleanup_stop_lease()
            .expect("install older lease after capture preflight");
        preflight_resume.wait();
        let conversion_error = capture.join().expect("join conversion-error capture");
        assert_eq!(
            conversion_error
                .expect_err("older lease must reject committed successor conversion")
                .raw_os_error(),
            Some(libc::EALREADY)
        );
        assert_eq!(raw_cont_calls, 1, "capture issued an unexpected raw CONT");
        assert!(
            terminal.pending_is_empty(),
            "successor FIFO was not committed"
        );
        assert!(newborn_tracees.lock().unwrap().is_empty());
        let successor_stop_id = {
            let held = held_task_stops.lock().unwrap();
            assert_eq!(held.len(), 1);
            held.get(&HeldTaskStopKey::from_terminal(pid, &terminal))
                .and_then(|stop| stop.cleanup_transfer.as_ref())
                .expect("committed successor retained one durable transfer")
                .logical_stop_id()
        };
        assert_ne!(successor_stop_id, old_stop_id);

        terminal
            .dispose_cleanup_stop(old_lease)
            .expect("retire older conflicting lease");
        let mut held =
            LiteinstTraceeCleanup::take_held_task_stop_from(&held_task_stops, pid, &terminal)
                .expect("activate held committed successor after old retirement")
                .expect("committed successor remains in the held map");
        assert!(held.cleanup_transfer.is_none());
        let successor_lease = held
            .cleanup_lease
            .take()
            .expect("held committed successor activates exactly once");
        assert_eq!(successor_lease.logical_stop_id(), successor_stop_id);
        assert!(held_task_stops.lock().unwrap().is_empty());
        terminal
            .dispose_cleanup_stop(successor_lease)
            .expect("dispose committed successor lease");

        control
            .write_all(&[1])
            .expect("release conversion-error child");
        ptrace::cont(pid.into(), None).expect("resume committed successor stop");
        raw_cont_calls += 1;
        assert_eq!(raw_cont_calls, 2);
        assert!(terminal.wait(Duration::from_secs(2)));
        assert_eventually_reaped(role, pid);
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
        let (pid, stopped) = spawn_held_stop_child("exit supersession child");
        stopped
            .setoptions(ptrace::Options::PTRACE_O_TRACEEXIT)
            .expect("enable exit-stop supersession");
        let generation = stopped.terminal_cleanup();
        let ordinary_stop_id = stopped.logical_stop_id();
        let slot = held_task_stops(&stopped, &Event::Signal(Signal::SIGSTOP));
        let running = stopped
            .resume(None)
            .expect("resume ordinary stop while retaining its cleanup shadow");
        let exit_event = running.exit_event();
        drop(running);
        let exit_stopped = tokio::time::timeout(Duration::from_secs(2), exit_event)
            .await
            .expect("exit-stop supersession timed out")
            .expect("claim distinct exit-stop capability");
        assert_ne!(exit_stopped.logical_stop_id(), ordinary_stop_id);

        HeldRootStop::supersede_with_exit(&slot, &exit_stopped)
            .expect("same-generation exit stop must supersede existing lease");
        {
            let held = slot.lock().unwrap();
            let held = held
                .get(&HeldTaskStopKey::from_terminal(pid, &generation))
                .expect("exit supersession cleared the lease");
            assert!(held.armed);
            assert!(held.terminal.same_generation(&generation));
            assert!(matches!(held.status, HeldRootStopStatus::Exit));
            assert!(held.cleanup_transfer.as_ref().is_some_and(|transfer| {
                transfer.logical_stop_id() == exit_stopped.logical_stop_id()
                    && transfer.physical_status_id() == exit_stopped.physical_status_id()
            }));
        }

        let final_running = RootStopLease::new(exit_stopped, Some(Arc::clone(&slot)))
            .resume(None)
            .expect("resume superseding exit stop");
        assert!(slot.lock().unwrap().is_empty());
        let exited = final_running
            .next_state()
            .await
            .expect("wait supersession child final status");
        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
        assert!(generation.wait(Duration::from_secs(2)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_exit_path_supersedes_preempted_same_generation_lease() {
        let (_pid, stopped) = spawn_held_stop_child("async exit supersession child");
        stopped
            .setoptions(ptrace::Options::PTRACE_O_TRACEEXIT)
            .expect("enable async exit-stop supersession");
        let ordinary_stop_id = stopped.logical_stop_id();
        let slot = held_task_stops(&stopped, &Event::Signal(Signal::SIGSTOP));
        let running = stopped
            .resume(None)
            .expect("resume ordinary stop while retaining its cleanup shadow");
        let exit_event = running.exit_event();
        drop(running);
        let exit_stopped = tokio::time::timeout(Duration::from_secs(2), exit_event)
            .await
            .expect("async exit-stop supersession timed out")
            .expect("claim async exit-stop capability");
        assert_ne!(exit_stopped.logical_stop_id(), ordinary_stop_id);

        let status =
            TracedTask::<InitFailureTool>::handle_exit_event(exit_stopped, Some(Arc::clone(&slot)))
                .await
                .expect("async exit path rejected same-generation lease supersession");
        assert_eq!(status, ExitStatus::Exited(0));
        assert!(
            slot.lock().unwrap().is_empty(),
            "async exit path left its superseded lease armed"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_supersede_rejects_physical_shape_and_wrong_kind_without_mutation() {
        let role = "live supersede provenance child";
        let (pid, stopped, observer) = spawn_observed_held_stop_child(role);
        let terminal = stopped.terminal_cleanup();
        let observed_status = stopped
            .physical_status_id()
            .expect("observed predecessor carries physical status");
        let observed_stop_id = stopped.logical_stop_id();
        let unobserved_successor = Stopped::try_new_current_unchecked(pid)
            .expect("create same-generation unobserved successor token");
        let unobserved_stop_id = unobserved_successor.logical_stop_id();
        assert!(unobserved_stop_id.is_strictly_after(observed_stop_id));
        assert_eq!(unobserved_successor.physical_status_id(), None);

        let some_to_none = held_task_stops(&stopped, &Event::Signal(Signal::SIGSTOP));
        assert!(matches!(
            HeldRootStop::supersede_with_exit(&some_to_none, &unobserved_successor),
            Err(TraceError::Errno(Errno::EPROTO))
        ));
        {
            let held = some_to_none.lock().unwrap();
            let held = held
                .get(&HeldTaskStopKey::from_terminal(pid, &terminal))
                .expect("Some->None rejection lost predecessor");
            assert!(held.terminal.same_generation(&terminal));
            assert!(matches!(
                held.status,
                HeldRootStopStatus::Signal(Signal::SIGSTOP)
            ));
            assert!(held.cleanup_transfer.as_ref().is_some_and(|transfer| {
                transfer.logical_stop_id() == observed_stop_id
                    && transfer.physical_status_id() == Some(observed_status)
            }));
        }
        some_to_none.lock().unwrap().clear();

        let none_to_some = held_task_stops(&unobserved_successor, &Event::Signal(Signal::SIGSTOP));
        assert!(matches!(
            HeldRootStop::supersede_with_exit(&none_to_some, &stopped),
            Err(TraceError::Errno(Errno::EPROTO))
        ));
        {
            let held = none_to_some.lock().unwrap();
            let held = held
                .get(&HeldTaskStopKey::from_terminal(pid, &terminal))
                .expect("None->Some rejection lost predecessor");
            assert!(held.terminal.same_generation(&terminal));
            assert!(matches!(
                held.status,
                HeldRootStopStatus::Signal(Signal::SIGSTOP)
            ));
            assert!(held.cleanup_transfer.as_ref().is_some_and(|transfer| {
                transfer.logical_stop_id() == unobserved_stop_id
                    && transfer.physical_status_id().is_none()
            }));
        }
        none_to_some.lock().unwrap().clear();

        let repeated_some_wrong_kind = held_task_stops(&stopped, &Event::Signal(Signal::SIGSTOP));
        assert!(matches!(
            HeldRootStop::supersede_with_exit(&repeated_some_wrong_kind, &stopped),
            Err(TraceError::Errno(Errno::EPROTO))
        ));
        {
            let held = repeated_some_wrong_kind.lock().unwrap();
            let held = held
                .get(&HeldTaskStopKey::from_terminal(pid, &terminal))
                .expect("wrong-kind repeated status rejection lost predecessor");
            assert!(held.terminal.same_generation(&terminal));
            assert!(matches!(
                held.status,
                HeldRootStopStatus::Signal(Signal::SIGSTOP)
            ));
            assert!(held.cleanup_transfer.as_ref().is_some_and(|transfer| {
                transfer.logical_stop_id() == observed_stop_id
                    && transfer.physical_status_id() == Some(observed_status)
            }));
        }

        repeated_some_wrong_kind.lock().unwrap().clear();
        drop(unobserved_successor);
        resume_held_stop_child(role, stopped).await;
        assert!(terminal.wait(Duration::from_secs(2)));
        observer.close();
        let snapshot = observer.snapshot();
        let validation = snapshot.validate();
        assert!(validation.is_valid(), "{validation:#?}");
        assert_eq!(
            snapshot
                .records()
                .iter()
                .filter(|record| matches!(
                    record.kind(),
                    safeptrace::PhysicalEventRecordKind::StatusDisposition {
                        status,
                        disposition: PhysicalStatusDisposition::KernelSupersededByExitStop,
                    } if status == observed_status
                ))
                .count(),
            0
        );
        assert_eventually_reaped(role, pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_supersede_rejects_lower_observerless_stop_without_mutation() {
        let role = "lower observerless supersede child";
        let (pid, stopped) = spawn_held_stop_child(role);
        let terminal = stopped.terminal_cleanup();
        let lower_stop_id = stopped.logical_stop_id();
        let later = Stopped::try_new_current_unchecked(pid)
            .expect("create later same-generation observerless token");
        let later_stop_id = later.logical_stop_id();
        assert!(later_stop_id.is_strictly_after(lower_stop_id));
        let slot = held_task_stops(&later, &Event::Signal(Signal::SIGSTOP));

        assert!(matches!(
            HeldRootStop::supersede_with_exit(&slot, &stopped),
            Err(TraceError::Errno(Errno::EPROTO))
        ));
        {
            let held = slot.lock().unwrap();
            let held = held
                .get(&HeldTaskStopKey::from_terminal(pid, &terminal))
                .expect("lower rejection lost later owner");
            assert!(held.terminal.same_generation(&terminal));
            assert!(matches!(
                held.status,
                HeldRootStopStatus::Signal(Signal::SIGSTOP)
            ));
            assert!(held.cleanup_transfer.as_ref().is_some_and(|transfer| {
                transfer.logical_stop_id() == later_stop_id
                    && transfer.physical_status_id().is_none()
            }));
        }

        slot.lock().unwrap().clear();
        drop(later);
        resume_held_stop_child(role, stopped).await;
        assert!(terminal.wait(Duration::from_secs(2)));
        assert_eventually_reaped(role, pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn typed_exit_success_freeze_observes_finished_without_raw_retry() {
        let (pid, stopped, observer) = spawn_observed_held_stop_child("typed exit freeze child");
        stopped
            .setoptions(ptrace::Options::PTRACE_O_TRACEEXIT)
            .expect("enable typed exit-stop observation");
        let terminal = stopped.terminal_cleanup();
        let slot = held_task_stops(&stopped, &Event::Signal(Signal::SIGSTOP));
        let running = RootStopLease::new(stopped, Some(Arc::clone(&slot)))
            .resume(None)
            .expect("resume typed-exit child to exit stop");
        assert!(slot.lock().unwrap().is_empty());

        let exit_event = running.exit_event();
        drop(running);
        let exit_stopped = tokio::time::timeout(Duration::from_secs(2), exit_event)
            .await
            .expect("typed exit future timed out")
            .expect("claim typed exit stop");
        let exit_status = exit_stopped
            .physical_status_id()
            .expect("typed exit stop carries physical status");
        HeldRootStop::supersede_with_exit(&slot, &exit_stopped)
            .expect("arm exact claimed-exit cleanup lease");
        let final_running = RootStopLease::new(exit_stopped, Some(Arc::clone(&slot)))
            .resume(None)
            .expect("continue typed exit stop");
        assert!(
            slot.lock().unwrap().is_empty(),
            "typed exit success left its root-stop lease armed"
        );

        let mut frozen_stop = None;
        continue_frozen_exit_stop(
            &terminal,
            &mut frozen_stop,
            PhysicalResumeOwner::RootCleanup,
        )
        .expect("freeze path recognizes typed exit completion");
        assert!(frozen_stop.is_none());

        let exited = final_running
            .next_state()
            .await
            .expect("wait for typed-exit child's final status");
        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
        assert!(terminal.wait(Duration::from_secs(2)));
        observer.close();

        let snapshot = observer.snapshot();
        let validation = snapshot.validate();
        assert!(validation.is_valid(), "{validation:#?}");
        let exit_resumes = snapshot
            .records()
            .iter()
            .filter_map(|record| match record.kind() {
                safeptrace::PhysicalEventRecordKind::ResumeAttempt { context, .. }
                    if context.source_status == Some(exit_status) =>
                {
                    Some(context.owner)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(exit_resumes, [PhysicalResumeOwner::TypedStopped]);
        assert_eventually_reaped("typed exit freeze child", pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normal_armer_replaces_finished_shadow_with_distinct_successor_token() {
        let role = "finished-shadow normal-armer child";
        let (pid, stopped) = spawn_held_stop_child(role);
        stopped
            .setoptions(ptrace::Options::PTRACE_O_TRACEEXIT)
            .expect("enable finished-shadow exit stop");
        let terminal = stopped.terminal_cleanup();
        let lower_candidate = Stopped::try_new_current_unchecked(pid)
            .expect("create pre-exit same-generation lower candidate");
        let lower_stop_id = lower_candidate.logical_stop_id();
        let slot = held_task_stops(&stopped, &Event::Signal(Signal::SIGSTOP));
        let running = stopped
            .resume(None)
            .expect("resume finished-shadow child to exit stop");
        let exit_event = running.exit_event();
        drop(running);
        let exit_stopped = tokio::time::timeout(Duration::from_secs(2), exit_event)
            .await
            .expect("finished-shadow ExitFuture timed out")
            .expect("claim finished-shadow exit stop");
        HeldRootStop::supersede_with_exit(&slot, &exit_stopped)
            .expect("arm finished-shadow claimed exit stop");
        let exit_stop_id = exit_stopped.logical_stop_id();
        assert!(exit_stop_id.is_strictly_after(lower_stop_id));
        // SAFETY: this deliberately forges an equal-ID negative-control view.
        // ManuallyDrop prevents a second Arc ownership decrement, and the view
        // is never used for ptrace or any successful ownership transfer.
        let equal_candidate = std::mem::ManuallyDrop::new(unsafe { std::ptr::read(&exit_stopped) });

        // This unchecked token is deliberately not presented as de-thread
        // evidence. It supplies a distinct same-generation logical successor
        // solely to exercise the normal armer's stale-shadow ownership path.
        let successor = Stopped::try_new_current_unchecked(pid)
            .expect("create distinct same-generation successor token");
        let successor_stop_id = successor.logical_stop_id();
        assert!(successor_stop_id.is_strictly_after(exit_stop_id));
        let final_running = exit_stopped
            .resume(None)
            .expect("continue claimed exit stop exactly once");

        let rejected_callbacks = AtomicUsize::new(0);
        assert!(matches!(
            HeldRootStop::arm_empty_with(&slot, &lower_candidate, &Event::Exec(pid), || {
                rejected_callbacks.fetch_add(1, Ordering::SeqCst);
            },),
            Err(TraceError::Errno(Errno::EPROTO))
        ));
        assert_eq!(rejected_callbacks.load(Ordering::SeqCst), 0);
        {
            let held = slot.lock().unwrap();
            assert_eq!(held.len(), 1);
            let held = held
                .get(&HeldTaskStopKey::from_terminal(pid, &terminal))
                .expect("lower candidate removed Finished shadow");
            assert!(matches!(held.status, HeldRootStopStatus::Exit));
            assert!(held.cleanup_transfer.as_ref().is_some_and(|transfer| {
                transfer.logical_stop_id() == exit_stop_id
                    && transfer.physical_status_id().is_none()
            }));
            assert!(held.terminal.same_generation(&terminal));
        }
        assert!(matches!(
            HeldRootStop::arm_empty_with(&slot, &equal_candidate, &Event::Exec(pid), || {
                rejected_callbacks.fetch_add(1, Ordering::SeqCst);
            },),
            Err(TraceError::Errno(Errno::EPROTO))
        ));
        assert_eq!(rejected_callbacks.load(Ordering::SeqCst), 0);
        {
            let held = slot.lock().unwrap();
            let held = held
                .get(&HeldTaskStopKey::from_terminal(pid, &terminal))
                .expect("equal candidate removed Finished shadow");
            assert!(matches!(held.status, HeldRootStopStatus::Exit));
            assert!(held.cleanup_transfer.as_ref().is_some_and(|transfer| {
                transfer.logical_stop_id() == exit_stop_id
                    && transfer.physical_status_id().is_none()
            }));
        }

        let committed_callbacks = AtomicUsize::new(0);
        HeldRootStop::arm_empty_with(&slot, &successor, &Event::Exec(pid), || {
            committed_callbacks.fetch_add(1, Ordering::SeqCst);
        })
        .expect("normal armer must consume Finished predecessor shadow");
        assert_eq!(committed_callbacks.load(Ordering::SeqCst), 1);
        {
            let held = slot.lock().unwrap();
            assert_eq!(held.len(), 1);
            let held = held
                .get(&HeldTaskStopKey::from_terminal(pid, &terminal))
                .expect("successor shadow remains durable");
            assert!(matches!(held.status, HeldRootStopStatus::Exec(replaced) if replaced == pid));
            assert!(held.cleanup_transfer.as_ref().is_some_and(|transfer| {
                transfer.logical_stop_id() == successor_stop_id
                    && transfer.physical_status_id() == successor.physical_status_id()
            }));
        }
        slot.lock().unwrap().clear();
        drop(lower_candidate);
        drop(successor);

        let exited = final_running
            .next_state()
            .await
            .expect("wait finished-shadow child final status");
        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
        assert!(terminal.wait(Duration::from_secs(2)));
        assert_eventually_reaped(role, pid);
    }

    #[test]
    fn failed_predecessor_arm_commits_newchild_successor_before_exact_error() {
        let role = "failed-predecessor NewChild armer child";
        let (pid, stopped, mut control) = spawn_gated_held_stop_child(role);
        let terminal = stopped.terminal_cleanup();
        let predecessor_stop_id = stopped.logical_stop_id();
        let slot = held_task_stops(&stopped, &Event::Signal(Signal::SIGSTOP));
        let successor = Stopped::try_new_current_unchecked(pid)
            .expect("create same-generation NewChild successor token");
        let successor_stop_id = successor.logical_stop_id();
        assert!(successor_stop_id.is_strictly_after(predecessor_stop_id));
        let child_pid = Pid::from_raw(i32::MAX - 20);
        let child = Running::new(child_pid);
        let expected_link = EventChildLink {
            generation: child.physical_event_generation(),
            tid: child_pid,
            parent_tid: pid,
            op: ChildOp::Fork,
        };
        let event = Event::NewChild(ChildOp::Fork, child);
        let committed_metadata = StdMutex::new(None);
        drop(stopped);

        let result = HeldRootStop::arm_empty_with_resolver(
            &slot,
            &successor,
            &event,
            || {
                let previous = committed_metadata.lock().unwrap().replace(expected_link);
                assert!(previous.is_none(), "NewChild metadata callback repeated");
            },
            |current, transfer, observed_successor| {
                assert!(current.same_generation(&terminal));
                assert_eq!(observed_successor.logical_stop_id(), successor_stop_id);
                assert_eq!(observed_successor.physical_status_id(), None);
                let predecessor = transfer
                    .take()
                    .expect("Failed predecessor resolver lost durable transfer");
                assert_eq!(predecessor.logical_stop_id(), predecessor_stop_id);
                Ok(Some(TransferredStopCompletion::Failed(Errno::EPERM)))
            },
        );
        assert!(matches!(result, Err(TraceError::Errno(Errno::EPERM))));
        assert_eq!(*committed_metadata.lock().unwrap(), Some(expected_link));
        {
            let held = slot.lock().unwrap();
            assert_eq!(held.len(), 1);
            let held = held
                .get(&HeldTaskStopKey::from_terminal(pid, &terminal))
                .expect("Failed predecessor lost successor shadow");
            assert!(
                matches!(held.status, HeldRootStopStatus::NewChild(link) if link == expected_link)
            );
            assert!(held.cleanup_transfer.as_ref().is_some_and(|transfer| {
                transfer.logical_stop_id() == successor_stop_id
                    && transfer.physical_status_id().is_none()
            }));
        }
        drop(successor);

        let mut held = LiteinstTraceeCleanup::take_held_task_stop_from(&slot, pid, &terminal)
            .expect("activate committed NewChild successor")
            .expect("committed NewChild successor remains owned");
        let successor_lease = held
            .cleanup_lease
            .take()
            .expect("committed NewChild successor converts exactly once");
        assert_eq!(successor_lease.logical_stop_id(), successor_stop_id);
        assert!(slot.lock().unwrap().is_empty());
        terminal
            .dispose_cleanup_stop(successor_lease)
            .expect("dispose committed NewChild successor without raw transition");
        assert_eq!(
            terminal
                .continue_exit_stop_for_cleanup(&mut None, PhysicalResumeOwner::RootCleanup,)
                .expect("retired ordinary successor needs no raw exit continuation"),
            TerminalCleanupContinue::WaitingForExitStop
        );

        control
            .write_all(&[1])
            .expect("release failed-predecessor armer child");
        ptrace::cont(pid.into(), None).expect("resume failed-predecessor armer child");
        assert!(terminal.wait(Duration::from_secs(2)));
        assert_eventually_reaped(role, pid);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn held_stop_map_retains_independent_task_generations() {
        for _ in 0..16 {
            let (_first_pid, first) = spawn_held_stop_child("first generation child");
            let (_second_pid, second) = spawn_held_stop_child("second generation child");
            let first_generation = first.terminal_cleanup();
            let second_generation = second.terminal_cleanup();
            let slot = held_task_stops(&first, &Event::Signal(Signal::SIGSTOP));

            HeldRootStop::supersede_with_exit(&slot, &second)
                .expect("a second task must receive an independent held-stop slot");
            {
                let held = slot.lock().unwrap();
                let first_held = held
                    .get(&HeldTaskStopKey::from_terminal(
                        first.pid(),
                        &first_generation,
                    ))
                    .expect("first task lease disappeared");
                assert!(first_held.terminal.same_generation(&first_generation));
                assert!(matches!(
                    first_held.status,
                    HeldRootStopStatus::Signal(Signal::SIGSTOP)
                ));
                let second_held = held
                    .get(&HeldTaskStopKey::from_terminal(
                        second.pid(),
                        &second_generation,
                    ))
                    .expect("second task lease absent");
                assert!(second_held.terminal.same_generation(&second_generation));
                assert!(matches!(second_held.status, HeldRootStopStatus::Exit));
            }

            slot.lock().unwrap().clear();
            resume_held_stop_child("first generation child", first).await;
            resume_held_stop_child("second generation child", second).await;
        }
    }

    #[test]
    fn held_stop_same_pid_generations_coexist_and_retire_independently() {
        let pid = Pid::from_raw(i32::MAX - 17);
        let first = Stopped::new_unchecked(pid);
        let replacement = Stopped::new_unchecked(pid);
        let first_generation = first.terminal_cleanup();
        let replacement_generation = replacement.terminal_cleanup();
        assert_ne!(
            first_generation.physical_event_generation(),
            replacement_generation.physical_event_generation()
        );
        let slot = held_task_stops(&first, &Event::Signal(Signal::SIGSTOP));

        HeldRootStop::supersede_with_exit(&slot, &replacement)
            .expect("same numeric PID's replacement generation gets an independent owner");
        {
            let held = slot.lock().unwrap();
            assert_eq!(held.len(), 2);
            let first_held = held
                .get(&HeldTaskStopKey::from_terminal(pid, &first_generation))
                .expect("first same-PID generation remains durable");
            assert!(first_held.terminal.same_generation(&first_generation));
            assert!(matches!(
                first_held.status,
                HeldRootStopStatus::Signal(Signal::SIGSTOP)
            ));
            let replacement_held = held
                .get(&HeldTaskStopKey::from_terminal(
                    pid,
                    &replacement_generation,
                ))
                .expect("replacement same-PID generation remains durable");
            assert!(
                replacement_held
                    .terminal
                    .same_generation(&replacement_generation)
            );
            assert!(matches!(replacement_held.status, HeldRootStopStatus::Exit));
        }
        let unrelated_generation = Stopped::new_unchecked(pid);
        let unrelated_terminal = unrelated_generation.terminal_cleanup();
        assert!(!unrelated_terminal.same_generation(&first_generation));
        assert!(!unrelated_terminal.same_generation(&replacement_generation));
        assert!(matches!(
            RootStopLease::new(unrelated_generation, Some(Arc::clone(&slot))).resume(None),
            Err(TraceError::Errno(Errno::EINVAL))
        ));
        assert_eq!(
            slot.lock().unwrap().len(),
            2,
            "wrong-generation transition touched or stranded an exact owner"
        );
        drop(replacement);
        drop(first);

        let first_exact =
            LiteinstTraceeCleanup::take_held_task_stop_from(&slot, pid, &first_generation)
                .expect("resolve first same-PID generation")
                .expect("first same-PID generation returns its exact cleanup lease");
        assert!(first_exact.terminal.same_generation(&first_generation));
        assert!(first_exact.cleanup_lease.is_some());
        {
            let held = slot.lock().unwrap();
            assert_eq!(held.len(), 1);
            assert!(held.contains_key(&HeldTaskStopKey::from_terminal(
                pid,
                &replacement_generation,
            )));
        }
        let replacement_exact =
            LiteinstTraceeCleanup::take_held_task_stop_from(&slot, pid, &replacement_generation)
                .expect("resolve replacement same-PID generation")
                .expect("replacement same-PID generation returns its exact cleanup lease");
        assert!(
            replacement_exact
                .terminal
                .same_generation(&replacement_generation)
        );
        assert!(replacement_exact.cleanup_lease.is_some());
        assert!(slot.lock().unwrap().is_empty());
    }

    #[test]
    fn newchild_arm_is_generation_local_and_commits_only_its_exact_callback() {
        let pid = Pid::from_raw(i32::MAX - 18);
        let child_pid = Pid::from_raw(i32::MAX - 19);
        let first = Stopped::new_unchecked(pid);
        let replacement = Stopped::new_unchecked(pid);
        let first_generation = first.terminal_cleanup();
        let replacement_generation = replacement.terminal_cleanup();
        let first_stop_id = first.logical_stop_id();
        let slot = held_task_stops(&first, &Event::Signal(Signal::SIGSTOP));
        let child = Running::new(child_pid);
        let event = Event::NewChild(ChildOp::Fork, child);
        let metadata_callbacks = AtomicUsize::new(0);

        assert!(matches!(
            HeldRootStop::arm_empty_with(&slot, &first, &event, || {
                metadata_callbacks.fetch_add(1, Ordering::SeqCst);
            }),
            Err(TraceError::Errno(Errno::EINVAL))
        ));
        assert_eq!(metadata_callbacks.load(Ordering::SeqCst), 0);
        HeldRootStop::arm_empty_with(&slot, &replacement, &event, || {
            metadata_callbacks.fetch_add(1, Ordering::SeqCst);
        })
        .expect("distinct same-PID generation installs its own NewChild owner");
        assert_eq!(metadata_callbacks.load(Ordering::SeqCst), 1);
        let held = slot.lock().unwrap();
        assert_eq!(held.len(), 2);
        let first_held = held
            .get(&HeldTaskStopKey::from_terminal(pid, &first_generation))
            .expect("rejected same-generation NewChild arm lost predecessor");
        assert!(first_held.terminal.same_generation(&first_generation));
        assert!(matches!(
            first_held.status,
            HeldRootStopStatus::Signal(Signal::SIGSTOP)
        ));
        assert!(
            first_held
                .cleanup_transfer
                .as_ref()
                .is_some_and(|transfer| { transfer.logical_stop_id() == first_stop_id })
        );
        let replacement_held = held
            .get(&HeldTaskStopKey::from_terminal(
                pid,
                &replacement_generation,
            ))
            .expect("distinct generation NewChild owner missing");
        assert!(matches!(
            replacement_held.status,
            HeldRootStopStatus::NewChild(link)
                if link.tid == child_pid && link.parent_tid == pid && link.op == ChildOp::Fork
        ));
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

    #[tokio::test(flavor = "current_thread")]
    async fn successful_fork_and_clone_do_not_receive_a_synthetic_sigtrap() {
        let tracer = spawn_fn::<InitFailureTool, _>(|| {
            let child = match unsafe { nix::unistd::fork() }.expect("fork successful child") {
                nix::unistd::ForkResult::Child => unsafe { libc::_exit(0) },
                nix::unistd::ForkResult::Parent { child } => child,
            };
            assert_eq!(
                nix::sys::wait::waitpid(child, None).expect("wait successful fork child"),
                nix::sys::wait::WaitStatus::Exited(child, 0),
            );
            std::thread::spawn(|| {})
                .join()
                .expect("join successful clone child");
        })
        .await
        .expect("spawn successful fork/clone guest");

        let (status, _) = tokio::time::timeout(Duration::from_secs(3), tracer.wait())
            .await
            .expect("successful fork/clone guest hung")
            .expect("successful fork/clone tracing failed");
        assert_eq!(status, ExitStatus::Exited(0));
    }

    #[cfg(not(target_arch = "aarch64"))]
    #[derive(Default, serde::Deserialize, serde::Serialize)]
    struct ForkContinuationState {
        order: Vec<String>,
        child: Option<i32>,
    }

    #[cfg(not(target_arch = "aarch64"))]
    #[derive(Default)]
    struct ForkContinuationLog {
        exited: StdMutex<Vec<ForkContinuationState>>,
    }

    #[cfg(not(target_arch = "aarch64"))]
    #[reverie::global_tool]
    impl GlobalTool for ForkContinuationLog {
        type Request = ForkContinuationState;
        type Response = ();
        type Config = ();

        async fn receive_rpc(&self, _from: Pid, state: ForkContinuationState) {
            self.exited.lock().unwrap().push(state);
        }
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn assert_one_injected_parent(log: ForkContinuationLog, expected_order: &[&str]) {
        let states = log.exited.into_inner().unwrap();
        let mut parents = states.iter().filter(|state| state.child.is_some());
        let parent = parents
            .next()
            .expect("no parent state recorded an injected child");
        assert!(
            parents.next().is_none(),
            "more than one parent injected a child"
        );
        assert_eq!(
            parent.order,
            expected_order
                .iter()
                .map(|step| (*step).to_owned())
                .collect::<Vec<_>>(),
        );
        let child_states = states
            .iter()
            .filter(|state| state.child.is_none())
            .collect::<Vec<_>>();
        assert_eq!(child_states.len(), 1, "expected one explicit child state");
        assert!(
            child_states[0].order.is_empty(),
            "child unexpectedly executed a parent callback"
        );

        let child = parent.child.expect("parent callback did not record child");
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(child, &mut status, libc::WNOHANG) },
            -1,
            "injected child {child} remained waitable after guest wait",
        );
        assert_eq!(Errno::last(), Errno::ECHILD);
    }

    #[cfg(not(target_arch = "aarch64"))]
    #[derive(Default)]
    struct SameSyscallForkTool;

    #[cfg(not(target_arch = "aarch64"))]
    #[reverie::tool]
    impl Tool for SameSyscallForkTool {
        type GlobalState = ForkContinuationLog;
        type ThreadState = ForkContinuationState;

        fn subscriptions(_config: &()) -> Subscription {
            [Sysno::fork, Sysno::getpid].into_iter().collect()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            match syscall.number() {
                Sysno::fork => {
                    guest.thread_state_mut().order.push("fork-enter".into());
                    let child = guest.inject(syscall).await?;
                    guest.thread_state_mut().order.push("fork-return".into());
                    guest.thread_state_mut().child = Some(child as i32);
                    Ok(child)
                }
                Sysno::getpid => {
                    guest.thread_state_mut().order.push("getpid-enter".into());
                    let result = guest.inject(syscall).await?;
                    guest.thread_state_mut().order.push("getpid-return".into());
                    Ok(result)
                }
                other => panic!("unexpected same-syscall fork callback: {other}"),
            }
        }

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            _tid: reverie::Tid,
            global: &G,
            state: Self::ThreadState,
            status: ExitStatus,
        ) -> Result<(), Error> {
            assert_eq!(status, ExitStatus::Exited(0));
            global.send_rpc(state).await;
            Ok(())
        }
    }

    #[cfg(not(target_arch = "aarch64"))]
    #[tokio::test(flavor = "current_thread")]
    async fn same_syscall_fork_reaches_exact_exit_before_one_following_callback() {
        let tracer = spawn_fn::<SameSyscallForkTool, _>(|| {
            let child = unsafe { libc::syscall(libc::SYS_fork) } as i32;
            if child == 0 {
                unsafe { libc::_exit(0) };
            }
            assert!(child > 0);
            assert!(unsafe { libc::syscall(libc::SYS_getpid) } > 0);
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0);
        })
        .await
        .expect("spawn same-syscall fork continuation guest");

        let (status, log) = tokio::time::timeout(Duration::from_secs(5), tracer.wait())
            .await
            .expect("same-syscall fork continuation hung")
            .expect("same-syscall fork continuation failed");
        assert_eq!(status, ExitStatus::Exited(0));
        assert_one_injected_parent(
            log,
            &["fork-enter", "fork-return", "getpid-enter", "getpid-return"],
        );
    }

    #[cfg(not(target_arch = "aarch64"))]
    #[derive(Default)]
    struct PrivateForkTool;

    #[cfg(not(target_arch = "aarch64"))]
    #[reverie::tool]
    impl Tool for PrivateForkTool {
        type GlobalState = ForkContinuationLog;
        type ThreadState = ForkContinuationState;

        fn subscriptions(_config: &()) -> Subscription {
            [Sysno::getpid, Sysno::getppid].into_iter().collect()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            match syscall.number() {
                Sysno::getpid => {
                    guest.thread_state_mut().order.push("getpid-enter".into());
                    let child = guest.inject(reverie::syscalls::Fork::new()).await?;
                    guest
                        .thread_state_mut()
                        .order
                        .push("private-fork-return".into());
                    guest.thread_state_mut().child = Some(child as i32);
                    // The injected fork replaces getpid deliberately: its child
                    // observes zero, while the parent receives the child PID.
                    Ok(child)
                }
                Sysno::getppid => {
                    guest.thread_state_mut().order.push("getppid-enter".into());
                    let result = guest.inject(syscall).await?;
                    guest.thread_state_mut().order.push("getppid-return".into());
                    Ok(result)
                }
                other => panic!("unexpected private-fork callback: {other}"),
            }
        }

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            _tid: reverie::Tid,
            global: &G,
            state: Self::ThreadState,
            status: ExitStatus,
        ) -> Result<(), Error> {
            assert_eq!(status, ExitStatus::Exited(0));
            global.send_rpc(state).await;
            Ok(())
        }
    }

    #[cfg(not(target_arch = "aarch64"))]
    #[tokio::test(flavor = "current_thread")]
    async fn private_fork_from_other_callback_finishes_step_then_reaps_once() {
        let tracer = spawn_fn::<PrivateForkTool, _>(|| {
            let child = unsafe { libc::syscall(libc::SYS_getpid) } as i32;
            if child == 0 {
                unsafe { libc::_exit(0) };
            }
            assert!(child > 0);
            assert!(unsafe { libc::syscall(libc::SYS_getppid) } > 0);
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0);
        })
        .await
        .expect("spawn private-fork continuation guest");

        let (status, log) = tokio::time::timeout(Duration::from_secs(5), tracer.wait())
            .await
            .expect("private-fork continuation hung")
            .expect("private-fork continuation failed");
        assert_eq!(status, ExitStatus::Exited(0));
        assert_one_injected_parent(
            log,
            &[
                "getpid-enter",
                "private-fork-return",
                "getppid-enter",
                "getppid-return",
            ],
        );
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
                .held_task_stops,
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
        {
            let held = held.lock().unwrap();
            let matching = held
                .iter()
                .filter(|(key, record)| key.task_tid == root_pid && key.validates(record))
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1);
            assert!(matches!(
                matching[0].1.status,
                HeldRootStopStatus::Signal(Signal::SIGTRAP)
            ));
        }

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
                .held_task_stops,
        );
        let tracer = builder.spawn().await.expect("spawn precise-timer tracee");
        let (status, ()) = tokio::time::timeout(Duration::from_secs(5), tracer.wait())
            .await
            .expect("normal precise-timer tracee timed out")
            .expect("wait normal precise-timer tracee");
        assert_eq!(status, ExitStatus::Exited(0));
        assert!(
            held.lock().unwrap().is_empty(),
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
                .held_task_stops,
        );
        let tracer = builder.spawn().await.expect("spawn normal LiteInst tracee");
        let (status, ()) = tracer.wait().await.expect("wait normal LiteInst tracee");
        assert_eq!(status, ExitStatus::Exited(0));
        assert!(
            held.lock().unwrap().is_empty(),
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
        assert!(checked_tracee_open_absence(absent, Errno::ENOENT).unwrap());
        assert!(checked_tracee_open_absence(absent, Errno::ESRCH).unwrap());
        for error in [
            Errno::EMFILE,
            Errno::ENFILE,
            Errno::EIO,
            Errno::EACCES,
            Errno::EPERM,
        ] {
            let retained = checked_tracee_open_absence(absent, error)
                .expect_err("resource/read error was silently skipped");
            assert_eq!(retained.raw_os_error(), Some(error.into_raw()));
        }

        let replacement = fork_paused_child();
        assert_eq!(
            checked_tracee_open_absence(replacement, Errno::ESRCH)
                .expect_err("an extant proc identity was treated as absent")
                .kind(),
            std::io::ErrorKind::WouldBlock,
        );
        unsafe { libc::kill(replacement.as_raw(), libc::SIGKILL) };
        Running::new(replacement)
            .wait()
            .expect("reap replacement fixture");

        let same_inode = reconcile_snapshot_failure(
            absent,
            77,
            Some(77),
            std::io::Error::from_raw_os_error(libc::ENOENT),
        )
        .expect_err("transient snapshot ENOENT with the same inode proved absence");
        assert_eq!(same_inode.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(
            reconcile_snapshot_failure(
                absent,
                77,
                Some(78),
                std::io::Error::from_raw_os_error(libc::ENOENT),
            )
            .unwrap(),
            TraceeGenerationState::GoneOrReplaced,
        );
    }

    #[test]
    fn descendant_scan_authority_ignores_reparenting_but_requires_a_live_tracer() {
        let baseline = TraceeSnapshot {
            tgid: Pid::from_raw(41),
            ppid: Pid::from_raw(42),
            tracer_pid: Pid::from_raw(43),
            start_time: 44,
        };
        assert_eq!(
            classify_queued_parent(None, baseline, true),
            QueuedParentCheck::Active(baseline)
        );

        let reparented = TraceeSnapshot {
            ppid: Pid::from_raw(45),
            ..baseline
        };
        assert_eq!(
            classify_queued_parent(Some(baseline), reparented, true),
            QueuedParentCheck::Active(reparented)
        );
        assert_eq!(
            classify_queued_parent(Some(baseline), baseline, false),
            QueuedParentCheck::Unavailable
        );

        let other_live_tracer = TraceeSnapshot {
            tracer_pid: Pid::from_raw(46),
            ..baseline
        };
        assert_eq!(
            classify_queued_parent(Some(baseline), other_live_tracer, true),
            QueuedParentCheck::LiveTracerChanged
        );
    }

    #[test]
    fn terminal_ownership_sampler_distinguishes_stable_release_from_change() {
        let tid = Pid::from_raw(41);
        let parent = Pid::from_raw(42);
        let tracer = Pid::from_raw(43);
        let identity = synthetic_tracee_identity(
            tid,
            tid,
            parent,
            tracer,
            44,
            45,
            Some((parent, parent, Some(ChildOp::Fork))),
        );
        let baseline = identity.snapshot;
        let sample = |states: Vec<TraceeGenerationState>, expected_tracer, tracer_owned| {
            let mut states = VecDeque::from(states);
            let result = identity
                .checked_terminal_ownership_sample_with(
                    || {
                        Ok(states
                            .pop_front()
                            .expect("terminal ownership generation sample"))
                    },
                    |observed| {
                        assert_eq!(observed, expected_tracer);
                        Ok(tracer_owned)
                    },
                )
                .expect("sample terminal ownership");
            assert!(states.is_empty(), "sampler did not consume both snapshots");
            result
        };

        assert_eq!(
            sample(
                vec![
                    TraceeGenerationState::Same(baseline),
                    TraceeGenerationState::Same(baseline),
                ],
                tracer,
                true,
            ),
            TerminalOwnershipSample::Stable(TerminalOwnership::TracerOwned),
        );
        assert_eq!(
            sample(
                vec![
                    TraceeGenerationState::Same(baseline),
                    TraceeGenerationState::Same(baseline),
                ],
                tracer,
                false,
            ),
            TerminalOwnershipSample::Stable(TerminalOwnership::CapturedParentOwned),
        );

        let released = TraceeSnapshot {
            ppid: Pid::from_raw(46),
            tracer_pid: Pid::from_raw(0),
            ..baseline
        };
        assert_eq!(
            sample(
                vec![
                    TraceeGenerationState::Same(released),
                    TraceeGenerationState::Same(released),
                ],
                Pid::from_raw(0),
                false,
            ),
            TerminalOwnershipSample::Stable(TerminalOwnership::Released),
        );

        let reparented = TraceeSnapshot {
            ppid: Pid::from_raw(46),
            ..baseline
        };
        assert_eq!(
            sample(
                vec![
                    TraceeGenerationState::Same(baseline),
                    TraceeGenerationState::Same(reparented),
                ],
                tracer,
                true,
            ),
            TerminalOwnershipSample::Changed,
        );
        let untraced = TraceeSnapshot {
            tracer_pid: Pid::from_raw(0),
            ..baseline
        };
        assert_eq!(
            sample(
                vec![
                    TraceeGenerationState::Same(baseline),
                    TraceeGenerationState::Same(untraced),
                ],
                tracer,
                true,
            ),
            TerminalOwnershipSample::Changed,
        );
        assert_eq!(
            sample(
                vec![
                    TraceeGenerationState::Same(baseline),
                    TraceeGenerationState::GoneOrReplaced,
                ],
                tracer,
                true,
            ),
            TerminalOwnershipSample::Stable(TerminalOwnership::Released),
        );
        assert_eq!(
            identity
                .checked_terminal_ownership_sample_with(
                    || Ok(TraceeGenerationState::GoneOrReplaced),
                    |_| panic!("absent generation must not inspect tracer ownership"),
                )
                .expect("absent generation is stably released"),
            TerminalOwnershipSample::Stable(TerminalOwnership::Released),
        );
    }

    #[test]
    fn checked_terminal_ownership_preserves_changed_would_block_contract() {
        let tid = Pid::from_raw(51);
        let error = require_stable_terminal_ownership(tid, TerminalOwnershipSample::Changed)
            .expect_err("changed sample must remain refused for authority callers");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(
            error.to_string(),
            "tracee 51 ownership changed while sampled"
        );
        for ownership in [
            TerminalOwnership::TracerOwned,
            TerminalOwnership::CapturedParentOwned,
            TerminalOwnership::Released,
        ] {
            assert_eq!(
                require_stable_terminal_ownership(tid, TerminalOwnershipSample::Stable(ownership),)
                    .expect("stable ownership must remain exact"),
                ownership,
            );
        }
    }

    #[test]
    fn terminal_cleanup_actions_require_stable_ownership_proof() {
        for (sample, released, continuation) in [
            (TerminalOwnershipSample::Changed, false, false),
            (
                TerminalOwnershipSample::Stable(TerminalOwnership::TracerOwned),
                false,
                true,
            ),
            (
                TerminalOwnershipSample::Stable(TerminalOwnership::CapturedParentOwned),
                false,
                false,
            ),
            (
                TerminalOwnershipSample::Stable(TerminalOwnership::Released),
                true,
                false,
            ),
        ] {
            assert_eq!(terminal_descendant_stably_released(sample), released);
            assert_eq!(
                terminal_ownership_permits_continuation(sample),
                continuation
            );
        }
    }

    #[test]
    fn parent_scan_error_resolution_is_exhaustive_and_fail_closed() {
        let active = TraceeSnapshot {
            tgid: Pid::from_raw(51),
            ppid: Pid::from_raw(52),
            tracer_pid: Pid::from_raw(53),
            start_time: 54,
        };

        let active_error = resolve_parent_scan_error(
            QueuedParentCheck::Active(active),
            std::io::Error::from_raw_os_error(libc::EIO),
        )
        .expect_err("an active parent must propagate its scan error");
        assert_eq!(active_error.raw_os_error(), Some(libc::EIO));

        assert_eq!(
            resolve_parent_scan_error(
                QueuedParentCheck::Unavailable,
                std::io::Error::from_raw_os_error(libc::EIO),
            )
            .expect("an unavailable parent invalidates its stale child list"),
            ParentScanErrorResolution::DiscardParent,
        );

        let changed_error = resolve_parent_scan_error(
            QueuedParentCheck::LiveTracerChanged,
            std::io::Error::from_raw_os_error(libc::ENOENT),
        )
        .expect_err("a different live tracer must fail closed");
        assert_eq!(changed_error.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(changed_error.raw_os_error(), None);
        assert_eq!(
            changed_error.to_string(),
            "parent generation changed live tracer authority during descendant discovery"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_task_match_accepts_only_the_exact_immutable_identity() {
        let role = "terminal-task identity control child";
        let (pid, stopped, observer) = spawn_observed_held_stop_child(role);
        let terminal = stopped.terminal_cleanup();
        let mut identity = TraceeIdentity::open_root(pid).expect("capture terminal-task identity");
        assert!(identity.matches_terminal_task(&terminal));

        let original_tid = identity.tid;
        let original = identity.snapshot;
        let original_inode = identity.proc_inode;
        identity.snapshot.ppid = Pid::from_raw(original.ppid.as_raw().saturating_add(1));
        identity.snapshot.tracer_pid =
            Pid::from_raw(original.tracer_pid.as_raw().saturating_add(1));
        assert!(
            identity.matches_terminal_task(&terminal),
            "mutable PPid/TracerPid snapshots must not redefine a physical generation"
        );

        identity.tid = Pid::from_raw(original_tid.as_raw().saturating_add(1));
        assert!(!identity.matches_terminal_task(&terminal));
        identity.tid = original_tid;
        identity.snapshot.tgid = Pid::from_raw(original.tgid.as_raw().saturating_add(1));
        assert!(!identity.matches_terminal_task(&terminal));
        identity.snapshot.tgid = original.tgid;
        identity.snapshot.start_time = original.start_time.saturating_add(1);
        assert!(!identity.matches_terminal_task(&terminal));
        identity.snapshot.start_time = original.start_time;
        identity.proc_inode = original_inode.saturating_add(1);
        assert!(!identity.matches_terminal_task(&terminal));

        identity.tid = original_tid;
        identity.snapshot = original;
        identity.proc_inode = original_inode;
        drop(identity);
        let exited = stopped
            .resume(None)
            .expect("resume terminal-task identity child")
            .next_state()
            .await
            .expect("wait terminal-task identity child");
        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
        observer.close();
        assert_eventually_reaped(role, pid);
    }

    #[test]
    fn immutable_identity_equality_ignores_only_mutable_ownership_snapshots() {
        let tid = Pid::from_raw(i32::MAX - 72);
        let tgid = Pid::from_raw(i32::MAX - 73);
        let event_parent = Pid::from_raw(i32::MAX - 74);
        let parent_tgid = Pid::from_raw(i32::MAX - 75);
        let relation = Some((event_parent, parent_tgid, Some(ChildOp::Clone)));
        let left = synthetic_tracee_identity(
            tid,
            tgid,
            Pid::from_raw(11),
            Pid::from_raw(12),
            101,
            103,
            relation,
        );
        let mut right = synthetic_tracee_identity(
            tid,
            tgid,
            Pid::from_raw(21),
            Pid::from_raw(22),
            101,
            103,
            relation,
        );
        assert_ne!(left.snapshot.ppid, right.snapshot.ppid);
        assert_ne!(left.snapshot.tracer_pid, right.snapshot.tracer_pid);
        assert!(NewbornTracee::identities_match(&left, &right));

        right.snapshot.start_time += 1;
        assert!(!NewbornTracee::identities_match(&left, &right));
        right.snapshot.start_time = left.snapshot.start_time;
        right.parent = None;
        assert!(!NewbornTracee::identities_match(&left, &right));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn false_signal_authority_refuses_raw_cleanup_until_checked_refresh() {
        let role = "false signal-authority control child";
        let (pid, stopped, observer) = spawn_observed_held_stop_child(role);
        let terminal = stopped.terminal_cleanup();
        let identity = TraceeIdentity::open_root(pid).expect("capture authority-control identity");
        let mut registered = RegisteredTraceeCleanup {
            identity,
            terminal,
            event_link: None,
            frozen_stop: None,
            signal_authority: false,
        };
        let before = observer_raw_attempt_counts(&observer);
        assert_eq!(
            registered
                .send_sigkill()
                .expect_err("false authority sent SIGKILL")
                .kind(),
            std::io::ErrorKind::PermissionDenied,
        );
        assert_eq!(
            registered
                .continue_exit_stop()
                .expect_err("false authority attempted PTRACE_CONT")
                .kind(),
            std::io::ErrorKind::PermissionDenied,
        );
        assert_eq!(observer_raw_attempt_counts(&observer), before);
        registered
            .refresh_signal_authority()
            .expect("refresh exact stopped generation authority");
        assert!(
            registered.signal_authority,
            "exact before/register/match/after sandwich did not grant authority"
        );
        drop(registered);

        let exited = stopped
            .resume(None)
            .expect("resume authority-control child")
            .next_state()
            .await
            .expect("wait authority-control child");
        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
        observer.close();
        assert_eventually_reaped(role, pid);
    }

    #[test]
    fn authority_refresh_requires_an_unchanged_checked_generation_sandwich() {
        let before = TraceeSnapshot {
            tgid: Pid::from_raw(31),
            ppid: Pid::from_raw(32),
            tracer_pid: Pid::from_raw(33),
            start_time: 34,
        };
        let mut changed = before;
        changed.ppid = Pid::from_raw(35);

        let order = Arc::new(StdMutex::new(Vec::new()));
        let states = Arc::new(StdMutex::new(VecDeque::from([
            TraceeGenerationState::Same(before),
            TraceeGenerationState::Same(before),
        ])));
        let mut authority = false;
        RegisteredTraceeCleanup::refresh_signal_authority_with(
            &mut authority,
            {
                let order = Arc::clone(&order);
                let states = Arc::clone(&states);
                move || {
                    order.lock().unwrap().push("generation");
                    Ok(states
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("generation sample"))
                }
            },
            {
                let order = Arc::clone(&order);
                move || {
                    order.lock().unwrap().push("register");
                    Ok(())
                }
            },
            {
                let order = Arc::clone(&order);
                move || {
                    order.lock().unwrap().push("match");
                    true
                }
            },
        )
        .expect("qualifying authority refresh");
        assert!(authority);
        assert_eq!(
            *order.lock().unwrap(),
            ["generation", "register", "match", "generation"]
        );

        let states = StdMutex::new(VecDeque::from([
            TraceeGenerationState::Same(before),
            TraceeGenerationState::Same(changed),
        ]));
        let mut authority = false;
        RegisteredTraceeCleanup::refresh_signal_authority_with(
            &mut authority,
            || {
                Ok(states
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("generation sample"))
            },
            || Ok(()),
            || true,
        )
        .expect("changed ownership snapshot is a closed refresh");
        assert!(!authority);
    }

    #[test]
    fn newborn_registry_keys_exact_generations_and_rejects_link_overwrite() {
        let child_pid = Pid::from_raw(i32::MAX - 81);
        let parent = Pid::from_raw(i32::MAX - 82);
        let other_parent = Pid::from_raw(i32::MAX - 83);
        let first = Running::new(child_pid);
        let second = Running::new(child_pid);
        let first_generation = first.physical_event_generation();
        let second_generation = second.physical_event_generation();
        assert_ne!(first_generation, second_generation);

        let mut newborns = BTreeMap::new();
        NewbornTracee::register_event(&mut newborns, parent, ChildOp::Fork, &first)
            .expect("register first numeric-PID generation");
        NewbornTracee::register_event(&mut newborns, parent, ChildOp::Fork, &second)
            .expect("register reused numeric-PID generation");
        assert_eq!(newborns.len(), 2);
        assert_eq!(newborns[&first_generation].link.tid, child_pid);
        assert_eq!(newborns[&second_generation].link.tid, child_pid);

        assert_eq!(
            NewbornTracee::register_event(&mut newborns, other_parent, ChildOp::Fork, &first,),
            Err(Errno::EPROTO),
        );
        assert_eq!(newborns.len(), 2);
        assert_eq!(newborns[&first_generation].link.parent_tid, parent);
    }

    #[test]
    fn resolved_generation_rekey_is_exact_and_updates_the_event_edge() {
        let tid = Pid::from_raw(i32::MAX - 84);
        let parent = Pid::from_raw(i32::MAX - 85);
        let authoritative = Running::new(tid);
        let terminal = authoritative.terminal_cleanup();
        let authoritative_generation = terminal.physical_event_generation();
        let provisional_generation = Running::new(tid).physical_event_generation();
        assert_ne!(provisional_generation, authoritative_generation);
        let identity = synthetic_tracee_identity(
            tid,
            tid,
            parent,
            Pid::from_raw(1),
            401,
            403,
            Some((parent, parent, Some(ChildOp::Fork))),
        );
        let mut descendants = BTreeMap::from([(
            provisional_generation,
            RegisteredTraceeCleanup {
                identity,
                terminal,
                event_link: Some(EventChildLink {
                    generation: provisional_generation,
                    tid,
                    parent_tid: parent,
                    op: ChildOp::Fork,
                }),
                frozen_stop: None,
                signal_authority: false,
            },
        )]);

        LiteinstTraceeCleanup::rekey_resolved_descendants(&mut descendants)
            .expect("rekey provisional cleanup generation");
        assert!(!descendants.contains_key(&provisional_generation));
        let resolved = descendants
            .get(&authoritative_generation)
            .expect("resolved cleanup generation");
        assert_eq!(
            resolved.event_link.expect("resolved event edge").generation,
            authoritative_generation
        );
        assert!(
            resolved
                .terminal
                .same_generation(&authoritative.terminal_cleanup())
        );
    }

    #[test]
    fn resolved_generation_collision_preserves_both_frozen_owners() {
        let tid = Pid::from_raw(i32::MAX - 86);
        let parent = Pid::from_raw(i32::MAX - 87);
        let authoritative = Running::new(tid);
        let authoritative_terminal = authoritative.terminal_cleanup();
        let moving_terminal = authoritative.terminal_cleanup();
        let authoritative_generation = authoritative_terminal.physical_event_generation();
        let provisional_generation = Running::new(tid).physical_event_generation();
        assert_ne!(provisional_generation, authoritative_generation);
        let existing_identity = synthetic_tracee_identity(
            tid,
            tid,
            parent,
            Pid::from_raw(1),
            501,
            503,
            Some((parent, parent, Some(ChildOp::Fork))),
        );
        let moving_identity = synthetic_tracee_identity(
            tid,
            tid,
            Pid::from_raw(parent.as_raw().saturating_add(1)),
            Pid::from_raw(2),
            501,
            503,
            Some((parent, parent, Some(ChildOp::Fork))),
        );
        assert!(NewbornTracee::identities_match(
            &existing_identity,
            &moving_identity
        ));
        let existing_frozen = Stopped::new_unchecked(Pid::from_raw(i32::MAX - 88))
            .into_cleanup_stop_lease()
            .expect("create existing frozen-stop control");
        let existing_stop = existing_frozen.logical_stop_id();
        let moving_frozen = Stopped::new_unchecked(Pid::from_raw(i32::MAX - 89))
            .into_cleanup_stop_lease()
            .expect("create moving frozen-stop control");
        let moving_stop = moving_frozen.logical_stop_id();
        let authoritative_link = EventChildLink {
            generation: authoritative_generation,
            tid,
            parent_tid: parent,
            op: ChildOp::Fork,
        };
        let provisional_link = EventChildLink {
            generation: provisional_generation,
            ..authoritative_link
        };
        let mut descendants = BTreeMap::from([
            (
                authoritative_generation,
                RegisteredTraceeCleanup {
                    identity: existing_identity,
                    terminal: authoritative_terminal,
                    event_link: Some(authoritative_link),
                    frozen_stop: Some(existing_frozen),
                    signal_authority: true,
                },
            ),
            (
                provisional_generation,
                RegisteredTraceeCleanup {
                    identity: moving_identity,
                    terminal: moving_terminal,
                    event_link: Some(provisional_link),
                    frozen_stop: Some(moving_frozen),
                    signal_authority: false,
                },
            ),
        ]);

        assert_eq!(
            LiteinstTraceeCleanup::rekey_resolved_descendants(&mut descendants)
                .expect_err("two frozen owners must not merge")
                .kind(),
            std::io::ErrorKind::InvalidData,
        );
        assert_eq!(descendants.len(), 2);
        let existing = descendants
            .get(&authoritative_generation)
            .expect("collision retained authoritative owner");
        assert_eq!(existing.event_link, Some(authoritative_link));
        assert_eq!(
            existing
                .frozen_stop
                .as_ref()
                .expect("collision retained authoritative frozen stop")
                .logical_stop_id(),
            existing_stop
        );
        let moving = descendants
            .get(&provisional_generation)
            .expect("collision retained provisional owner");
        assert_eq!(moving.event_link, Some(provisional_link));
        assert_eq!(
            moving
                .frozen_stop
                .as_ref()
                .expect("collision retained provisional frozen stop")
                .logical_stop_id(),
            moving_stop
        );
    }

    #[test]
    fn rekey_rejects_mismatched_old_edge_before_mutating_any_owner() {
        let first_tid = Pid::from_raw(i32::MAX - 90);
        let second_tid = Pid::from_raw(i32::MAX - 91);
        let parent = Pid::from_raw(i32::MAX - 92);
        let first_terminal = Running::new(first_tid).terminal_cleanup();
        let second_terminal = Running::new(second_tid).terminal_cleanup();
        let old_a = Running::new(first_tid).physical_event_generation();
        let old_b = Running::new(second_tid).physical_event_generation();
        let (valid_old, mismatched_old) = if old_a < old_b {
            (old_a, old_b)
        } else {
            (old_b, old_a)
        };
        assert_ne!(
            valid_old,
            first_terminal.physical_event_generation(),
            "valid control must require a rekey"
        );
        assert_ne!(
            mismatched_old,
            second_terminal.physical_event_generation(),
            "mismatched control must require a rekey"
        );
        let first_frozen = Stopped::new_unchecked(Pid::from_raw(i32::MAX - 93))
            .into_cleanup_stop_lease()
            .expect("create valid-old frozen-stop control");
        let first_stop = first_frozen.logical_stop_id();
        let second_frozen = Stopped::new_unchecked(Pid::from_raw(i32::MAX - 94))
            .into_cleanup_stop_lease()
            .expect("create mismatched-old frozen-stop control");
        let second_stop = second_frozen.logical_stop_id();
        let valid_link = EventChildLink {
            generation: valid_old,
            tid: first_tid,
            parent_tid: parent,
            op: ChildOp::Fork,
        };
        let mismatched_link = EventChildLink {
            generation: valid_old,
            tid: second_tid,
            parent_tid: parent,
            op: ChildOp::Clone,
        };
        assert_ne!(mismatched_link.generation, mismatched_old);
        let mut descendants = BTreeMap::from([
            (
                valid_old,
                RegisteredTraceeCleanup {
                    identity: synthetic_tracee_identity(
                        first_tid,
                        first_tid,
                        parent,
                        Pid::from_raw(1),
                        601,
                        603,
                        Some((parent, parent, Some(ChildOp::Fork))),
                    ),
                    terminal: first_terminal,
                    event_link: Some(valid_link),
                    frozen_stop: Some(first_frozen),
                    signal_authority: true,
                },
            ),
            (
                mismatched_old,
                RegisteredTraceeCleanup {
                    identity: synthetic_tracee_identity(
                        second_tid,
                        second_tid,
                        parent,
                        Pid::from_raw(1),
                        701,
                        703,
                        Some((parent, parent, Some(ChildOp::Clone))),
                    ),
                    terminal: second_terminal,
                    event_link: Some(mismatched_link),
                    frozen_stop: Some(second_frozen),
                    signal_authority: false,
                },
            ),
        ]);
        let first_target = descendants[&valid_old].terminal.physical_event_generation();
        let second_target = descendants[&mismatched_old]
            .terminal
            .physical_event_generation();

        assert_eq!(
            LiteinstTraceeCleanup::rekey_resolved_descendants(&mut descendants)
                .expect_err("mismatched pre-rekey event edge must fail atomically")
                .kind(),
            std::io::ErrorKind::InvalidData,
        );
        assert_eq!(descendants.len(), 2);
        let first = descendants
            .get(&valid_old)
            .expect("mismatched later edge moved the earlier valid owner");
        assert_eq!(first.event_link, Some(valid_link));
        assert_eq!(first.terminal.physical_event_generation(), first_target);
        assert_eq!(
            first
                .frozen_stop
                .as_ref()
                .expect("mismatched edge dropped earlier frozen stop")
                .logical_stop_id(),
            first_stop
        );
        let second = descendants
            .get(&mismatched_old)
            .expect("mismatched edge removed its own owner");
        assert_eq!(second.event_link, Some(mismatched_link));
        assert_eq!(second.terminal.physical_event_generation(), second_target);
        assert_eq!(
            second
                .frozen_stop
                .as_ref()
                .expect("mismatched edge dropped its frozen stop")
                .logical_stop_id(),
            second_stop
        );
    }

    #[test]
    fn observation_only_retirement_requires_every_nonmutating_proof() {
        let ownership_checks = AtomicUsize::new(0);
        assert!(
            !LiteinstTraceeCleanup::observation_only_retirement_ready(
                false,
                true,
                None,
                None,
                false,
                || {
                    ownership_checks.fetch_add(1, Ordering::SeqCst);
                    Ok(TerminalOwnership::Released)
                },
            )
            .expect("unfinished quarantine remains retained")
        );
        assert_eq!(ownership_checks.load(Ordering::SeqCst), 0);

        for (pending_empty, registration_error, terminal_error, frozen) in [
            (false, None, None, false),
            (true, Some(Errno::EIO), None, false),
            (true, None, Some(Errno::EIO), false),
            (true, None, None, true),
        ] {
            assert_eq!(
                LiteinstTraceeCleanup::observation_only_retirement_ready(
                    true,
                    pending_empty,
                    registration_error,
                    terminal_error,
                    frozen,
                    || {
                        ownership_checks.fetch_add(1, Ordering::SeqCst);
                        Ok(TerminalOwnership::Released)
                    },
                )
                .expect_err("incomplete observation-only proof was accepted")
                .kind(),
                std::io::ErrorKind::WouldBlock,
            );
        }
        assert_eq!(ownership_checks.load(Ordering::SeqCst), 0);
        assert!(
            !LiteinstTraceeCleanup::observation_only_retirement_ready(
                true,
                true,
                None,
                None,
                false,
                || {
                    ownership_checks.fetch_add(1, Ordering::SeqCst);
                    Ok(TerminalOwnership::TracerOwned)
                },
            )
            .expect("owned quarantine remains retained")
        );
        assert_eq!(ownership_checks.load(Ordering::SeqCst), 1);
        assert!(
            LiteinstTraceeCleanup::observation_only_retirement_ready(
                true,
                true,
                None,
                None,
                false,
                || {
                    ownership_checks.fetch_add(1, Ordering::SeqCst);
                    Ok(TerminalOwnership::Released)
                },
            )
            .expect("released quarantine retirement proof")
        );
        assert_eq!(ownership_checks.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn released_false_authority_quarantine_retires_without_raw_operations() {
        let role = "observation-only quarantine child";
        let (pid, stopped, observer) = spawn_observed_held_stop_child(role);
        let terminal = stopped.terminal_cleanup();
        let generation = terminal.physical_event_generation();
        let identity = TraceeIdentity::open_root(pid).expect("capture quarantine identity");
        let exited = stopped
            .resume(None)
            .expect("resume quarantine child")
            .next_state()
            .await
            .expect("wait quarantine child");
        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
        assert!(terminal.wait(Duration::from_secs(2)));
        assert!(terminal.pending_is_empty());
        assert_eq!(terminal.registration_error(), None);
        assert_eq!(terminal.terminal_error(), None);
        observer.close();

        let mut descendants = BTreeMap::from([(
            generation,
            RegisteredTraceeCleanup {
                identity,
                terminal,
                event_link: None,
                frozen_stop: None,
                signal_authority: false,
            },
        )]);
        let before = observer_raw_attempt_counts(&observer);
        LiteinstTraceeCleanup::retire_released_quarantine(&mut descendants)
            .expect("retire released observation-only quarantine");
        assert!(descendants.is_empty());
        assert_eq!(observer_raw_attempt_counts(&observer), before);
        assert_eventually_reaped(role, pid);
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

    #[tokio::test(flavor = "current_thread")]
    async fn typed_descendant_retirement_is_atomic_and_requires_exact_terminal_proof() {
        let parent_tid = Pid::from_raw(std::process::id() as i32);
        let newborns = Arc::new(StdMutex::new(BTreeMap::new()));

        let (first_pid, first_stopped) =
            spawn_held_stop_child("first typed descendant retirement child");
        let first_terminal = first_stopped.terminal_cleanup();
        let first_generation = first_terminal.physical_event_generation();
        let first_identity =
            TraceeIdentity::capture(first_pid, Some((parent_tid, Some(ChildOp::Fork))), true)
                .expect("capture first production-shaped descendant identity");
        newborns.lock().unwrap().insert(
            first_generation,
            NewbornTracee {
                link: EventChildLink {
                    generation: first_generation,
                    tid: first_pid,
                    parent_tid,
                    op: ChildOp::Fork,
                },
                identity: Some(first_identity),
                terminal: first_stopped.terminal_cleanup(),
            },
        );

        let (second_pid, second_stopped) =
            spawn_held_stop_child("second typed descendant retirement child");
        let second_terminal = second_stopped.terminal_cleanup();
        let second_generation = second_terminal.physical_event_generation();
        let second_identity =
            TraceeIdentity::capture(second_pid, Some((parent_tid, Some(ChildOp::Fork))), true)
                .expect("capture second production-shaped descendant identity");
        newborns.lock().unwrap().insert(
            second_generation,
            NewbornTracee {
                link: EventChildLink {
                    generation: second_generation,
                    tid: second_pid,
                    parent_tid,
                    op: ChildOp::Fork,
                },
                identity: Some(second_identity),
                terminal: second_stopped.terminal_cleanup(),
            },
        );

        newborns
            .lock()
            .unwrap()
            .get_mut(&first_generation)
            .expect("first descendant remains registered")
            .link
            .parent_tid = first_pid;
        let mismatch = LiteinstTraceeCleanup::retire_typed_descendants(&newborns, Instant::now())
            .expect_err("a mismatched NewChild edge must fail closed");
        assert_eq!(mismatch.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(newborns.lock().unwrap().len(), 2);
        newborns
            .lock()
            .unwrap()
            .get_mut(&first_generation)
            .expect("rejected descendant remains registered")
            .link
            .parent_tid = parent_tid;

        let original_start_time = {
            let mut retained = newborns.lock().unwrap();
            let identity = retained
                .get_mut(&first_generation)
                .and_then(|newborn| newborn.identity.as_mut())
                .expect("first descendant retains its real identity");
            let original = identity.snapshot.start_time;
            identity.snapshot.start_time = original.saturating_add(1);
            original
        };
        let terminal_mismatch =
            LiteinstTraceeCleanup::retire_typed_descendants(&newborns, Instant::now())
                .expect_err("a real terminal with mismatched immutable identity must fail closed");
        assert_eq!(terminal_mismatch.kind(), std::io::ErrorKind::InvalidData);
        {
            let retained = newborns.lock().unwrap();
            assert_eq!(retained.len(), 2);
            let first = retained
                .get(&first_generation)
                .expect("terminal mismatch removed first descendant");
            assert!(first.terminal.same_generation(&first_terminal));
            assert_eq!(
                first
                    .identity
                    .as_ref()
                    .expect("terminal mismatch dropped first identity")
                    .snapshot
                    .start_time,
                original_start_time.saturating_add(1)
            );
            assert!(retained.contains_key(&second_generation));
        }
        newborns
            .lock()
            .unwrap()
            .get_mut(&first_generation)
            .and_then(|newborn| newborn.identity.as_mut())
            .expect("restore rejected real identity")
            .snapshot
            .start_time = original_start_time;

        let incomplete = LiteinstTraceeCleanup::retire_typed_descendants(&newborns, Instant::now())
            .expect_err("live descendants must not be retired");
        assert_eq!(incomplete.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(newborns.lock().unwrap().len(), 2);

        let first_exit = first_stopped
            .resume(None)
            .expect("resume first typed descendant")
            .next_state()
            .await
            .expect("wait for first typed descendant");
        assert_eq!(first_exit.assume_exited().1, ExitStatus::Exited(0));
        let partial = LiteinstTraceeCleanup::retire_typed_descendants(&newborns, Instant::now())
            .expect_err("one incomplete descendant must preserve the complete peer");
        assert_eq!(partial.kind(), std::io::ErrorKind::WouldBlock);
        {
            let retained = newborns.lock().unwrap();
            assert_eq!(retained.len(), 2);
            assert!(retained.contains_key(&first_generation));
            assert!(retained.contains_key(&second_generation));
        }

        let second_exit = second_stopped
            .resume(None)
            .expect("resume second typed descendant")
            .next_state()
            .await
            .expect("wait for second typed descendant");
        assert_eq!(second_exit.assume_exited().1, ExitStatus::Exited(0));
        LiteinstTraceeCleanup::retire_typed_descendants(
            &newborns,
            Instant::now() + Duration::from_secs(2),
        )
        .expect("exact terminal descendants should retire together");
        assert!(first_terminal.pending_is_empty());
        assert_eq!(first_terminal.terminal_error(), None);
        assert!(second_terminal.pending_is_empty());
        assert_eq!(second_terminal.terminal_error(), None);
        assert!(newborns.lock().unwrap().is_empty());
    }

    #[test]
    fn typed_retirement_shared_deadline_waits_for_worker_done_without_preawait() {
        let role = "typed retirement deadline child";
        let parent_tid = Pid::from_raw(std::process::id() as i32);
        let (pid, stopped, mut control) = spawn_gated_held_stop_child(role);
        let terminal = stopped.terminal_cleanup();
        let generation = terminal.physical_event_generation();
        let identity = TraceeIdentity::capture(pid, Some((parent_tid, Some(ChildOp::Fork))), true)
            .expect("capture deadline-control descendant identity");
        let newborns = Arc::new(StdMutex::new(BTreeMap::from([(
            generation,
            NewbornTracee {
                link: EventChildLink {
                    generation,
                    tid: pid,
                    parent_tid,
                    op: ChildOp::Fork,
                },
                identity: Some(identity),
                terminal: stopped.terminal_cleanup(),
            },
        )])));
        let waiting = Arc::new(Barrier::new(2));
        let release_wait = Arc::new(Barrier::new(2));
        TYPED_RETIREMENT_WAIT_PAUSES.lock().unwrap().insert(
            generation,
            CleanupCapturePreflightPause {
                captured: Arc::clone(&waiting),
                resume: Arc::clone(&release_wait),
            },
        );
        let running = stopped.resume(None).expect("resume gated deadline child");
        assert!(
            !terminal.wait(Duration::ZERO),
            "control pre-awaited worker-DONE before typed retirement"
        );

        let retirement_newborns = Arc::clone(&newborns);
        let retirement = std::thread::spawn(move || {
            LiteinstTraceeCleanup::retire_typed_descendants(
                &retirement_newborns,
                Instant::now() + Duration::from_secs(2),
            )
        });
        waiting.wait();
        assert!(
            !terminal.wait(Duration::ZERO),
            "worker completed before the retirement wait boundary"
        );
        control
            .write_all(&[1])
            .expect("release gated deadline child");
        drop(control);
        release_wait.wait();
        retirement
            .join()
            .expect("join typed retirement deadline control")
            .expect("shared retirement deadline waits for worker-DONE");
        assert!(terminal.wait(Duration::ZERO));
        assert!(terminal.pending_is_empty());
        assert!(newborns.lock().unwrap().is_empty());
        drop(running);
        assert_eventually_reaped(role, pid);
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

    #[test]
    fn clone_parent_shaped_identity_uses_actual_ppid_not_event_parent() {
        let (actual_parent, descendant, mut control) = fork_paused_grandchild();
        let event_parent = fork_paused_child();
        assert_ne!(actual_parent, event_parent);

        ptrace::attach(descendant.into()).expect("attach CLONE_PARENT-shaped descendant");
        let running = Running::new(descendant);
        let (stopped, event) = running
            .wait()
            .expect("wait for CLONE_PARENT-shaped descendant attach")
            .assume_stopped();
        assert_eq!(event, Event::Signal(Signal::SIGSTOP));
        let generation = stopped.physical_event_generation();
        let identity =
            TraceeIdentity::capture_event_child(descendant, event_parent, ChildOp::Clone)
                .expect("capture event-parent provenance independent of PPid");
        assert_eq!(identity.snapshot.ppid, actual_parent);
        assert_eq!(
            identity.parent,
            Some((event_parent, event_parent, Some(ChildOp::Clone)))
        );

        let mut newborn = NewbornTracee {
            link: EventChildLink {
                generation,
                tid: descendant,
                parent_tid: event_parent,
                op: ChildOp::Clone,
            },
            identity: None,
            terminal: stopped.terminal_cleanup(),
        };
        newborn
            .set_identity(identity)
            .expect("bind exact event edge despite deliberately different PPid");
        stopped
            .detach(None)
            .expect("detach CLONE_PARENT-shaped descendant");
        assert_eq!(
            newborn
                .identity
                .as_ref()
                .expect("newborn retained identity")
                .checked_terminal_ownership()
                .expect("classify CLONE_PARENT-shaped ownership"),
            TerminalOwnership::CapturedParentOwned,
        );

        newborn
            .identity
            .as_ref()
            .unwrap()
            .send_signal(Signal::SIGKILL)
            .expect("kill CLONE_PARENT-shaped descendant through pidfd");
        control
            .write_all(&[1])
            .expect("release actual kernel parent");
        drop(control);
        Running::new(actual_parent)
            .wait()
            .expect("reap actual kernel parent");
        unsafe { libc::kill(event_parent.as_raw(), libc::SIGKILL) };
        Running::new(event_parent)
            .wait()
            .expect("reap synthetic event parent");
        assert_eventually_reaped("CLONE_PARENT-shaped descendant", descendant);
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
