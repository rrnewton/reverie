/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Tracer` type, plus ways to spawn it and retrieve its output.

#[cfg(target_arch = "x86_64")]
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
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
use safeptrace::PhysicalEventGenerationId;
use safeptrace::PhysicalEventObserver;
#[cfg(test)]
use safeptrace::PhysicalEventObserverConfig;
use safeptrace::PhysicalResumeContext;
use safeptrace::PhysicalResumeOperation;
use safeptrace::PhysicalResumeOutcome;
use safeptrace::PhysicalResumeOwner;
use safeptrace::PhysicalStatusDisposition;
use safeptrace::PhysicalStatusId;
use safeptrace::PhysicalTaskIdentity;
use safeptrace::PhysicalWaitContext;
use safeptrace::PhysicalWaitProducer;
use safeptrace::Running;
use safeptrace::Stopped;
use safeptrace::TerminalCleanup;
use safeptrace::TerminalCleanupContinue;
use safeptrace::Wait;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::LiteinstInstrumentationStats;
use crate::LiteinstInstrumentationStatsHandle;
use crate::PtraceBackendStatsSource;
use crate::cp;
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
    physical_generation: Option<PhysicalEventGenerationId>,
    newborn_tracees: Arc<StdMutex<HashMap<Pid, NewbornTracee>>>,
    armed: bool,
    terminal: Option<TerminalCleanup>,
    notifier_owner: Option<ThreadId>,
    retained_descendants: HashMap<Pid, RegisteredTraceeCleanup>,
    retained_terminal_descendants: HashMap<Pid, TraceeIdentity>,
    held_task_stops: HeldTaskStops,
    root_frozen: bool,
    root_frozen_status: Option<PhysicalStatusId>,
    root_finished: bool,
    root_finished_status: Option<PhysicalStatusId>,
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
    task_tid: Pid,
    status: HeldRootStopStatus,
    physical_status: Option<PhysicalStatusId>,
    claimed_exit_capability: bool,
    armed: bool,
}

pub(crate) type HeldTaskStops = Arc<StdMutex<HashMap<Pid, HeldRootStop>>>;

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
            task_tid: task.pid(),
            status,
            physical_status: task.physical_status_id(),
            claimed_exit_capability: false,
            armed: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }

    fn same_task_generation(&self, other: &Self) -> bool {
        self.task_tid == other.task_tid && self.terminal.same_generation(&other.terminal)
    }

    pub(crate) fn arm_empty(
        slot: &HeldTaskStops,
        task: &Stopped,
        event: &Event,
    ) -> Result<(), TraceError> {
        let mut held = slot.lock().unwrap();
        if held.contains_key(&task.pid()) {
            return Err(Errno::EINVAL.into());
        }
        held.insert(task.pid(), Self::from_event(task, event));
        Ok(())
    }

    pub(crate) fn ensure_current(
        slot: &HeldTaskStops,
        task: &Stopped,
        event: &Event,
    ) -> Result<(), TraceError> {
        let replacement = Self::from_event(task, event);
        let mut held = slot.lock().unwrap();
        match held.get(&task.pid()) {
            None => {
                held.insert(task.pid(), replacement);
                Ok(())
            }
            Some(current)
                if current.armed
                    && current.same_task_generation(&replacement)
                    && current.status == replacement.status
                    && current.physical_status == replacement.physical_status =>
            {
                Ok(())
            }
            Some(_) => Err(Errno::EINVAL.into()),
        }
    }

    pub(crate) fn supersede_with_exit(
        slot: &HeldTaskStops,
        task: &Stopped,
    ) -> Result<(), TraceError> {
        let mut replacement = Self::from_event(task, &Event::Exit);
        replacement.claimed_exit_capability = true;
        let mut held = slot.lock().unwrap();
        match held.get(&task.pid()) {
            None => {
                held.insert(task.pid(), replacement);
                Ok(())
            }
            Some(current) if current.armed && current.same_task_generation(&replacement) => {
                if current.physical_status != replacement.physical_status
                    && let Some(status) = current.physical_status
                {
                    task.physical_event_observer()
                        .ok_or(Errno::EPROTO)?
                        .finish_status(
                            status,
                            PhysicalStatusDisposition::KernelSupersededByExitStop,
                        );
                }
                held.insert(task.pid(), replacement);
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
            let Some(record) = held.get(&task_pid) else {
                return Err(Errno::EINVAL.into());
            };
            if record.task_tid != task_pid
                || !record.armed
                || !record.terminal.same_generation(&current)
                || record.physical_status != task.physical_status_id()
            {
                return Err(Errno::EINVAL.into());
            }
            if let Some(status) = record.physical_status {
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
                    .remove(&task_pid)
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
    frozen_status: Option<PhysicalStatusId>,
    finished: bool,
    finished_status: Option<PhysicalStatusId>,
}

impl RegisteredTraceeCleanup {
    fn continue_exit_stop(&mut self) -> std::io::Result<()> {
        continue_registered_exit_stop(
            &self.terminal,
            &mut self.frozen_status,
            &mut self.finished,
            &mut self.finished_status,
            PhysicalResumeOwner::DescendantCleanup,
        )
    }
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

fn retain_cleanup_stop(
    retained: &mut Option<PhysicalStatusId>,
    observed: Option<PhysicalStatusId>,
) -> std::io::Result<()> {
    let Some(observed) = observed else {
        return Ok(());
    };
    match retained {
        None => {
            *retained = Some(observed);
            Ok(())
        }
        Some(current) if *current == observed => Ok(()),
        Some(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "more than one unconsumed physical stop reached cancellation cleanup",
        )),
    }
}

fn finish_cancelled_stop(terminal: &TerminalCleanup, status: Option<PhysicalStatusId>) {
    if let (Some(observer), Some(status)) = (terminal.physical_event_observer(), status) {
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
    }
}

fn continue_registered_exit_stop(
    terminal: &TerminalCleanup,
    frozen_status: &mut Option<PhysicalStatusId>,
    finished: &mut bool,
    finished_status: &mut Option<PhysicalStatusId>,
    owner: PhysicalResumeOwner,
) -> std::io::Result<()> {
    match terminal
        .continue_exit_stop_for_cleanup(*frozen_status, *finished, *finished_status, owner)
        .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?
    {
        TerminalCleanupContinue::WaitingForExitStop => {}
        TerminalCleanupContinue::AlreadyFinished { source_status } => {
            *frozen_status = None;
            *finished = true;
            *finished_status = source_status;
        }
        TerminalCleanupContinue::Attempted {
            source_status,
            error,
        } => {
            *frozen_status = None;
            if error.is_none() || error == Some(Errno::ESRCH) {
                *finished = true;
                *finished_status = source_status;
            } else {
                *finished = false;
                *frozen_status = source_status;
            }
        }
    }
    Ok(())
}

impl LiteinstTraceeCleanup {
    fn new(
        task: &Running,
        newborn_tracees: Arc<StdMutex<HashMap<Pid, NewbornTracee>>>,
        held_task_stops: HeldTaskStops,
    ) -> Result<Self, Errno> {
        let physical_observer = task.physical_event_observer();
        let physical_generation = physical_observer
            .as_ref()
            .map(|_| task.physical_event_generation());
        Ok(Self {
            identity: TraceeIdentity::open_root(task.pid())?,
            physical_observer,
            physical_generation,
            newborn_tracees,
            armed: true,
            terminal: None,
            notifier_owner: None,
            retained_descendants: HashMap::new(),
            retained_terminal_descendants: HashMap::new(),
            held_task_stops,
            root_frozen: false,
            root_frozen_status: None,
            root_finished: false,
            root_finished_status: None,
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

    fn register_notifier(&mut self, task: &Running) {
        debug_assert!(self.terminal.is_none());
        self.notifier_owner = Some(std::thread::current().id());
        self.terminal = Some(task.terminal_cleanup());
    }

    fn capture_pending_children(
        &self,
        terminal: &TerminalCleanup,
    ) -> std::io::Result<Option<PhysicalStatusId>> {
        let mut retained_status = None;
        while let Some(reservation) = terminal.reserve_pending_for_cleanup(Duration::ZERO) {
            let physical_status = reservation.physical_status_id();
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
            retain_cleanup_stop(&mut retained_status, physical_status)?;
        }
        Ok(retained_status)
    }

    fn take_held_task_stop(
        &self,
        tid: Pid,
        terminal: &TerminalCleanup,
    ) -> std::io::Result<Option<HeldRootStop>> {
        let mut held_stops = self.held_task_stops.lock().unwrap();
        let Some(held) = held_stops.get(&tid) else {
            return Ok(None);
        };
        if !held.armed || held.task_tid != tid || !terminal.same_generation(&held.terminal) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("held LiteInst task stop did not match {tid}'s exact generation"),
            ));
        }
        if held.claimed_exit_capability {
            // SAFETY: the handler future has been dropped before whole-session
            // cleanup takes this exclusive map entry. No independent Stopped
            // capability survives the transfer.
            unsafe { terminal.revoke_owned_exit_stop() }
        } else {
            terminal.revoke_unclaimed_exit_stop()
        }
        .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
        let mut held = held_stops
            .remove(&tid)
            .expect("validated held task stop must remain present while locked");
        held.disarm();
        Ok(Some(held))
    }

    fn transfer_held_descendant_stops(
        &self,
        descendants: &mut HashMap<Pid, RegisteredTraceeCleanup>,
    ) -> std::io::Result<()> {
        let tids = descendants.keys().copied().collect::<Vec<_>>();
        for tid in tids {
            let terminal = &descendants
                .get(&tid)
                .expect("listed descendant must remain registered")
                .terminal;
            let Some(held) = self.take_held_task_stop(tid, terminal)? else {
                continue;
            };
            let status = held
                .physical_status
                .or_else(|| terminal.exit_stop_physical_status_id());
            retain_cleanup_stop(
                &mut descendants
                    .get_mut(&tid)
                    .expect("held descendant must remain registered")
                    .frozen_status,
                status,
            )?;
        }
        Ok(())
    }

    fn finish_terminal_held_stops(&self) -> std::io::Result<()> {
        let mut held_stops = self.held_task_stops.lock().unwrap();
        let tids = held_stops.keys().copied().collect::<Vec<_>>();
        for tid in tids {
            let Some(held) = held_stops.get(&tid) else {
                continue;
            };
            if !held.terminal.wait(Duration::ZERO) || !held.terminal.pending_is_empty() {
                continue;
            }
            if held.claimed_exit_capability {
                // SAFETY: terminal acknowledgment proves the task handler can
                // no longer use the ExitFuture-minted capability, and this map
                // entry is its sole retained cleanup ownership.
                unsafe { held.terminal.revoke_owned_exit_stop() }
            } else {
                held.terminal.revoke_unclaimed_exit_stop()
            }
            .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
            let held = held_stops
                .remove(&tid)
                .expect("terminal held stop must remain present while locked");
            finish_cancelled_stop(&held.terminal, held.physical_status);
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
            let pending_status = self.capture_pending_children(terminal)?;
            retain_cleanup_stop(&mut self.root_frozen_status, pending_status)?;
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
            if !matching_status {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "held root stop lease did not match the exact event status",
                ));
            }
            self.root_frozen_status = held
                .physical_status
                .or_else(|| terminal.exit_stop_physical_status_id());
            self.root_frozen = true;
            let pending_status = self.capture_pending_children(terminal)?;
            retain_cleanup_stop(&mut self.root_frozen_status, pending_status)?;
            return Ok(());
        }

        match self.identity.send_signal(Signal::SIGSTOP) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => return Err(std::io::Error::from_raw_os_error(error.into_raw())),
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Some(reservation) = terminal.reserve_pending_for_cleanup(remaining) {
                let physical_status = reservation.physical_status_id();
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
                self.root_frozen_status = physical_status;
                // Any exact-generation nonterminal wait status means the root
                // is kernel-stopped. Drain the remaining FIFO while it cannot
                // execute and create another child.
                let pending_status = self.capture_pending_children(terminal)?;
                retain_cleanup_stop(&mut self.root_frozen_status, pending_status)?;
                self.root_frozen = true;
                return Ok(());
            }
            if terminal.exit_stop_observed() {
                terminal
                    .revoke_unclaimed_exit_stop()
                    .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
                self.root_frozen_status = terminal.exit_stop_physical_status_id();
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

    fn finish_typed_completion(&mut self) -> std::io::Result<()> {
        if let Some(terminal) = self.terminal.as_ref() {
            terminal.wait(Duration::from_secs(2));
        }
        self.confirm_reaped()
    }

    fn confirm_reaped(&mut self) -> std::io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        self.finish_terminal_held_stops()?;
        let notifier_finished = self
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.wait(Duration::ZERO) && terminal.pending_is_empty());
        let identity_absent = !self.identity.same_process();
        let unregistered_absent =
            self.terminal.is_none() && identity_absent && self.physical_observer.is_none();
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
                finish_cancelled_stop(terminal, self.root_frozen_status.take());
            }
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
        if self.confirm_reaped().is_ok() {
            return Ok(());
        }

        let mut descendants = std::mem::take(&mut self.retained_descendants);
        let mut terminal_descendants = std::mem::take(&mut self.retained_terminal_descendants);
        let result =
            self.terminate_and_confirm_attempt(&mut descendants, &mut terminal_descendants);
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
    ) -> std::io::Result<()> {
        if self.terminal.is_none() {
            terminate_and_reap_new_child_with_identity(
                self.pid(),
                &self.identity,
                self.physical_observer.as_ref(),
                self.physical_generation,
            )
            .map_err(|error| {
                std::io::Error::other(format!("pre-registration LiteInst cleanup: {error}"))
            })?;
            self.armed = false;
            return Ok(());
        }

        if self.identity.same_process() {
            self.freeze_root_generation()?;
        } else {
            // The exact root generation is already gone, so it cannot create
            // another descendant. Drain any child event the notifier published
            // before terminal acknowledgment and continue with the retained
            // generation-bound descendants; trying to freeze a completed root
            // would only collide with its consumed exit capability.
            if let Some(terminal) = self.terminal.as_ref() {
                if let Some(held) = self.take_held_task_stop(self.pid(), terminal)? {
                    self.root_frozen_status = held
                        .physical_status
                        .or_else(|| terminal.exit_stop_physical_status_id());
                }
                let status = self.capture_pending_children(terminal)?;
                retain_cleanup_stop(&mut self.root_frozen_status, status)?;
                finish_cancelled_stop(terminal, self.root_frozen_status.take());
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
        self.transfer_held_descendant_stops(descendants)?;
        self.finish_terminal_held_stops()?;
        let root_terminal = self.terminal.as_ref().unwrap();
        let root_status = self.capture_pending_children(root_terminal)?;
        retain_cleanup_stop(&mut self.root_frozen_status, root_status)?;
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

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Some(terminal) = self.terminal.as_ref() {
                let status = self.capture_pending_children(terminal)?;
                retain_cleanup_stop(&mut self.root_frozen_status, status)?;
            }
            for tracee in descendants.values_mut() {
                let status = self.capture_pending_children(&tracee.terminal)?;
                retain_cleanup_stop(&mut tracee.frozen_status, status)?;
            }
            self.discover_descendants(descendants, terminal_descendants)?;
            self.transfer_held_descendant_stops(descendants)?;
            self.finish_terminal_held_stops()?;
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
            if root_done {
                finish_cancelled_stop(root_terminal, self.root_frozen_status.take());
            }
            let completed = descendants
                .iter()
                .filter_map(|(pid, tracee)| tracee.terminal.wait(Duration::ZERO).then_some(*pid))
                .collect::<Vec<_>>();
            for pid in completed {
                if let Some(tracee) = descendants.get_mut(&pid) {
                    let status = self.capture_pending_children(&tracee.terminal)?;
                    retain_cleanup_stop(&mut tracee.frozen_status, status)?;
                    finish_cancelled_stop(&tracee.terminal, tracee.frozen_status.take());
                }
                let tracee = descendants
                    .remove(&pid)
                    .expect("completed descendant must remain registered");
                if tracee.identity.same_process() {
                    terminal_descendants.insert(pid, tracee.identity);
                }
            }
            // Once the exact notifier generation is terminal, retain its proc
            // identity only while it remains our tracee or its recorded parent
            // still owns the zombie. After reparenting, waiting for another
            // process to reap it cannot strengthen our cleanup proof and can
            // never make progress here.
            terminal_descendants.retain(|_, identity| terminal_descendant_remains_owned(identity));
            let root_absent = !self.identity.same_process();
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
                if !root_done && descendants.is_empty() && self.identity.is_our_tracee() {
                    continue_registered_exit_stop(
                        root_terminal,
                        &mut self.root_frozen_status,
                        &mut self.root_finished,
                        &mut self.root_finished_status,
                        PhysicalResumeOwner::RootCleanup,
                    )?;
                }
                for tracee in descendants.values_mut() {
                    if tracee.identity.is_our_tracee() {
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
            if let Some(observer) = self.physical_observer.as_ref() {
                newborn
                    .terminal
                    .attach_physical_event_observer(observer)
                    .map_err(|_| std::io::Error::from_raw_os_error(libc::EPROTO))?;
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
                    frozen_status: None,
                    finished: false,
                    finished_status: None,
                },
            );
            transferred.push(tid);
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
            let stopped = Stopped::try_new_current_unchecked(tid)
                .map_err(|error| std::io::Error::from_raw_os_error(error.into_raw()))?;
            if let Some(observer) = self.physical_observer.as_ref() {
                stopped
                    .attach_physical_event_observer(observer)
                    .map_err(|_| std::io::Error::from_raw_os_error(libc::EPROTO))?;
            }
            let terminal = stopped.terminal_cleanup();
            descendants.insert(
                tid,
                RegisteredTraceeCleanup {
                    identity,
                    terminal,
                    event_link: None,
                    frozen_status: None,
                    finished: false,
                    finished_status: None,
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
                let running = Running::new(child);
                if let Some(observer) = self.physical_observer.as_ref() {
                    running
                        .attach_physical_event_observer(observer)
                        .map_err(|_| std::io::Error::from_raw_os_error(libc::EPROTO))?;
                    observer.link_pre_registration_task(
                        PhysicalTaskIdentity::direct_child(child),
                        running.physical_event_generation(),
                    );
                }
                let terminal = running.terminal_cleanup();
                descendants.insert(
                    child,
                    RegisteredTraceeCleanup {
                        identity,
                        terminal,
                        event_link: None,
                        frozen_status: None,
                        finished: false,
                        finished_status: None,
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
                        tracing::error!(pid = %self.pid(), %error, "LiteInst cancellation cleanup failed");
                        break;
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
    pid: Pid,
    identity: &TraceeIdentity,
    observer: Option<&PhysicalEventObserver>,
    generation: Option<PhysicalEventGenerationId>,
) -> Result<(), TraceError> {
    match identity.send_signal(Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => return Err(error.into()),
    }
    drain_unregistered_child(pid, observer, generation)
}

fn kill_and_drain_unregistered_child(task: &Running) -> (Option<Errno>, Result<(), TraceError>) {
    let pid = task.pid();
    let kill_result = unsafe { libc::kill(pid.as_raw(), libc::SIGKILL) };
    let kill_error = (kill_result == -1).then(Errno::last);
    let observer = task.physical_event_observer();
    let generation = observer.as_ref().map(|_| task.physical_event_generation());
    let drain_result = drain_unregistered_child(pid, observer.as_ref(), generation);
    (kill_error, drain_result)
}

fn drain_unregistered_child(
    pid: Pid,
    observer: Option<&PhysicalEventObserver>,
    generation: Option<PhysicalEventGenerationId>,
) -> Result<(), TraceError> {
    debug_assert_eq!(observer.is_some(), generation.is_some());
    let task_identity = PhysicalTaskIdentity::direct_child(pid);
    for _ in 0..2_000 {
        let wait_attempt = observer.zip(generation).map(|(observer, generation)| {
            observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task: task_identity,
                producer: PhysicalWaitProducer::PreRegistrationCleanup,
                flags: libc::__WALL | libc::WNOHANG,
            })
        });
        let mut status = 0;
        let waited =
            unsafe { libc::waitpid(pid.as_raw(), &mut status, libc::__WALL | libc::WNOHANG) };
        if waited == 0 {
            if let (Some(observer), Some(wait_attempt)) = (observer, wait_attempt) {
                observer.finish_wait_no_status(wait_attempt, None);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        if waited == -1 {
            let errno = Errno::last();
            if let (Some(observer), Some(wait_attempt)) = (observer, wait_attempt) {
                observer.finish_wait_error(wait_attempt, errno.into_raw());
            }
            match errno {
                Errno::EINTR => continue,
                Errno::ECHILD
                    if unsafe { libc::kill(pid.as_raw(), 0) } == -1
                        && Errno::last() == Errno::ESRCH =>
                {
                    if let (Some(observer), Some(generation), Some(wait_attempt)) =
                        (observer, generation, wait_attempt)
                    {
                        observer.finish_unregistered_generation(generation, wait_attempt.id());
                    }
                    return Ok(());
                }
                Errno::ECHILD => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                _ => return Err(errno.into()),
            }
        }
        let physical_status = match (observer, wait_attempt) {
            (Some(observer), Some(wait_attempt)) => {
                Some(observer.finish_wait_status(wait_attempt, status, None))
            }
            _ => None,
        };
        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
            if let (Some(observer), Some(generation), Some(wait_attempt), Some(physical_status)) =
                (observer, generation, wait_attempt, physical_status)
            {
                observer.finish_external_cleanup_status(physical_status);
                observer.finish_unregistered_generation(generation, wait_attempt.id());
            }
            return Ok(());
        }
        if libc::WIFSTOPPED(status) {
            if let (Some(observer), Some(generation), Some(physical_status)) =
                (observer, generation, physical_status)
            {
                observer.publish_external_cleanup_stop(generation, physical_status);
            }
            let resume_attempt = observer.map(|observer| {
                observer.begin_resume(PhysicalResumeContext {
                    generation,
                    task: task_identity,
                    source_status: physical_status,
                    operation: PhysicalResumeOperation::Continue,
                    signal: None,
                    owner: PhysicalResumeOwner::PreRegistrationCleanup,
                })
            });
            let result = ptrace::cont(pid.into(), None);
            if let (Some(observer), Some(resume_attempt)) = (observer, resume_attempt) {
                observer.finish_resume(
                    resume_attempt,
                    match result {
                        Ok(()) => PhysicalResumeOutcome::Success,
                        Err(error) => PhysicalResumeOutcome::Error(error as i32),
                    },
                );
            }
            match result {
                Ok(()) => {}
                Err(error @ nix::errno::Errno::ESRCH) => {
                    if let (Some(observer), Some(resume_attempt), Some(physical_status)) =
                        (observer, resume_attempt, physical_status)
                    {
                        observer.tolerate_resume_error(resume_attempt, error as i32);
                        observer.finish_status(
                            physical_status,
                            PhysicalStatusDisposition::CancellationCleanup,
                        );
                    }
                }
                Err(error) => return Err(Errno::new(error as i32).into()),
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
            tracing::error!(
                target: "reverie_ptrace::lifecycle",
                %pid,
                ?exit_status,
                "guest exited during ptrace initialization"
            );
            anyhow::anyhow!("tracee {pid} exited during ptrace initialization with {exit_status:?}")
                .into()
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
) -> Result<BoxFuture<'static, Result<ExitStatus, Error>>, TraceError> {
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
                Wait::Exited(_, _) => return Err(Errno::ECHILD.into()),
            }
        }
    } else {
        child
            .wait_for_signal(Signal::SIGSTOP)
            .await?
            .assume_stopped()
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
            newborn_tracees: Arc::new(StdMutex::new(HashMap::new())),
            held_task_stops: Arc::new(StdMutex::new(HashMap::new())),
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
        config.validate_environment(&self.command.get_captured_envs())?;
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
            freeze_after_loader_environment(&mut command, &after_loader.environment)?;
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
            after_loader.validate_environment(&command.get_captured_envs())?;
        }

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
        #[cfg(target_arch = "x86_64")]
        let mut liteinst_physical_observer = self
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.after_loader.as_ref())
            .map(|config| (config.physical_observer.clone(), config.diagnostics.clone()));
        #[cfg(target_arch = "x86_64")]
        if let Some(config) = self
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.after_loader.as_ref())
        {
            let observer_setup = (|| -> Result<(), Error> {
                running_child
                    .attach_physical_event_observer(&config.physical_observer)
                    .map_err(|_| Error::from(Errno::EPROTO))?;
                let generation = running_child.physical_event_generation();
                config.physical_observer.link_pre_registration_task(
                    PhysicalTaskIdentity::direct_child(guest_pid),
                    generation,
                );
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
                let (kill_error, drain_result) = kill_and_drain_unregistered_child(&running_child);
                let drain_error = drain_result.err();
                let cleanup_error = match (kill_error, drain_error) {
                    (Some(kill_error), Some(drain_error)) => Some(format!(
                        "numeric kill failed: {kill_error}; drain failed: {drain_error}"
                    )),
                    (Some(kill_error), None) => Some(format!("numeric kill failed: {kill_error}")),
                    (None, Some(drain_error)) => Some(format!("drain failed: {drain_error}")),
                    (None, None) => None,
                };
                let observer_error = Tracer::<T::GlobalState>::finalize_liteinst_physical_observer(
                    guest_pid,
                    &mut liteinst_physical_observer,
                )
                .err();
                return match (cleanup_error, observer_error) {
                    (Some(cleanup_error), Some(observer_error)) => Err(anyhow::anyhow!(
                        "LiteInst physical observer setup failed: {error}; cleanup failed: {cleanup_error}; physical partition also failed: {observer_error}"
                    )
                    .into()),
                    (Some(cleanup_error), None) => Err(anyhow::anyhow!(
                        "LiteInst physical observer setup failed: {error}; cleanup failed: {cleanup_error}"
                    )
                    .into()),
                    (None, Some(observer_error)) => Err(anyhow::anyhow!(
                        "LiteInst physical observer setup failed: {error}; physical partition also failed: {observer_error}"
                    )
                    .into()),
                    (None, None) => Err(error),
                };
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
                &running_child,
                liteinst_newborn_tracees.expect("LiteInst runtime config must exist"),
                liteinst_held_task_stops.expect("LiteInst runtime config must exist"),
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
                    let (kill_error, drain_result) =
                        kill_and_drain_unregistered_child(&running_child);
                    let setup_error =
                        liteinst_pidfd_setup_error(guest_pid, error, kill_error, drain_result);
                    #[cfg(target_arch = "x86_64")]
                    let observer_error =
                        Tracer::<T::GlobalState>::finalize_liteinst_physical_observer(
                            guest_pid,
                            &mut liteinst_physical_observer,
                        )
                        .err();
                    #[cfg(not(target_arch = "x86_64"))]
                    let observer_error = None::<Error>;
                    return match observer_error {
                        Some(observer_error) => Err(anyhow::anyhow!(
                            "{setup_error}; physical partition also failed: {observer_error}"
                        )
                        .into()),
                        None => Err(setup_error.into()),
                    };
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
        if let Some(cleanup) = liteinst_cleanup.as_mut() {
            cleanup.register_notifier(&running_child);
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
                let error = initialization_error(guest_pid, err).await;
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
                Err(err) => return Err(initialization_error(guest_pid, err).await),
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
            command
                .get_captured_envs()
                .get(std::ffi::OsStr::new("LSAN_OPTIONS"))
                .is_none()
        );
        assert!(
            command
                .get_captured_envs()
                .get(std::ffi::OsStr::new("ASAN_OPTIONS"))
                .is_none()
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
        let mut stops = HashMap::new();
        stops.insert(task.pid(), HeldRootStop::from_event(task, event));
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
                .get(&pid)
                .expect("failed transition discarded cleanup ownership");
            assert!(held.armed);
            assert!(held.terminal.same_generation(&generation));
            assert_eq!(held.physical_status, Some(physical_status));
        }

        let mut held = slot
            .lock()
            .unwrap()
            .remove(&pid)
            .expect("failed transition retained cleanup stop");
        assert_eq!(unsafe { libc::kill(pid.as_raw(), libc::SIGKILL) }, 0);
        assert!(
            held.terminal.wait(Duration::from_secs(2)),
            "killed failed-transition tracee did not reach terminal cleanup"
        );
        assert!(held.terminal.pending_is_empty());
        held.disarm();
        finish_cancelled_stop(&held.terminal, held.physical_status);
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
            let (pid, stopped, observer) = spawn_observed_held_stop_child(role);
            let retained_status = stopped
                .physical_status_id()
                .expect("observed retained stop carries its physical status");
            stopped
                .setoptions(ptrace::Options::PTRACE_O_TRACEEXIT)
                .expect("enable exit-stop observation for cleanup child");
            let terminal = stopped.terminal_cleanup();
            let identity = TraceeIdentity::open_root(pid).expect("capture cleanup child identity");

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

            let (terminal, frozen_status, finished, finished_status) =
                if owner == PhysicalResumeOwner::DescendantCleanup {
                    let mut registered = RegisteredTraceeCleanup {
                        identity,
                        terminal,
                        event_link: None,
                        frozen_status: Some(retained_status),
                        finished: false,
                        finished_status: None,
                    };
                    registered
                        .continue_exit_stop()
                        .expect("continue registered descendant exit stop");
                    registered
                        .continue_exit_stop()
                        .expect("recognize already-finished descendant exit stop");
                    (
                        registered.terminal,
                        registered.frozen_status,
                        registered.finished,
                        registered.finished_status,
                    )
                } else {
                    drop(identity);
                    let mut frozen_status = Some(retained_status);
                    let mut finished = false;
                    let mut finished_status = None;
                    continue_registered_exit_stop(
                        &terminal,
                        &mut frozen_status,
                        &mut finished,
                        &mut finished_status,
                        owner,
                    )
                    .expect("continue exact root exit stop");
                    continue_registered_exit_stop(
                        &terminal,
                        &mut frozen_status,
                        &mut finished,
                        &mut finished_status,
                        owner,
                    )
                    .expect("recognize already-finished root exit stop");
                    (terminal, frozen_status, finished, finished_status)
                };
            assert_eq!(frozen_status, None);
            assert!(finished);
            assert_eq!(finished_status, Some(exit_status));
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
            assert_eventually_reaped(role, pid);
        }
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
        let slot = held_task_stops(&stopped, &Event::Signal(Signal::SIGSTOP));

        HeldRootStop::supersede_with_exit(&slot, &stopped)
            .expect("same-generation exit stop must supersede existing lease");
        {
            let held = slot.lock().unwrap();
            let held = held
                .get(&stopped.pid())
                .expect("exit supersession cleared the lease");
            assert!(held.armed);
            assert!(held.terminal.same_generation(&generation));
            assert!(matches!(held.status, HeldRootStopStatus::Exit));
            assert!(held.claimed_exit_capability);
        }

        slot.lock().unwrap().remove(&stopped.pid());
        resume_held_stop_child("exit supersession child", stopped).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_exit_path_supersedes_preempted_same_generation_lease() {
        let (_pid, stopped) = spawn_held_stop_child("async exit supersession child");
        let slot = held_task_stops(&stopped, &Event::Signal(Signal::SIGSTOP));

        let status =
            TracedTask::<InitFailureTool>::handle_exit_event(stopped, Some(Arc::clone(&slot)))
                .await
                .expect("async exit path rejected same-generation lease supersession");
        assert_eq!(status, ExitStatus::Exited(0));
        assert!(
            slot.lock().unwrap().is_empty(),
            "async exit path left its superseded lease armed"
        );
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
                    .get(&first.pid())
                    .expect("first task lease disappeared");
                assert!(first_held.terminal.same_generation(&first_generation));
                assert!(matches!(
                    first_held.status,
                    HeldRootStopStatus::Signal(Signal::SIGSTOP)
                ));
                let second_held = held.get(&second.pid()).expect("second task lease absent");
                assert!(second_held.terminal.same_generation(&second_generation));
                assert!(matches!(second_held.status, HeldRootStopStatus::Exit));
            }

            slot.lock().unwrap().clear();
            resume_held_stop_child("first generation child", first).await;
            resume_held_stop_child("second generation child", second).await;
        }
    }

    #[test]
    fn held_stop_rejects_replacement_generation_for_same_task() {
        let pid = Pid::from_raw(i32::MAX - 17);
        let first = Stopped::new_unchecked(pid);
        let replacement = Stopped::new_unchecked(pid);
        let first_generation = first.terminal_cleanup();
        let slot = held_task_stops(&first, &Event::Signal(Signal::SIGSTOP));

        assert!(matches!(
            HeldRootStop::supersede_with_exit(&slot, &replacement),
            Err(TraceError::Errno(Errno::EINVAL))
        ));
        let held = slot.lock().unwrap();
        let held = held
            .get(&pid)
            .expect("generation mismatch removed the original task stop");
        assert!(held.terminal.same_generation(&first_generation));
        assert!(matches!(
            held.status,
            HeldRootStopStatus::Signal(Signal::SIGSTOP)
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
        assert!(
            matches!(
                held.lock().unwrap().get(&root_pid).map(|held| held.status),
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
}
