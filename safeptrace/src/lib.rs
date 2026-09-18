/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg(target_os = "linux")]

//! A safe ptrace API. This API forces correct usage of ptrace in that it is
//! not possible to call ptrace on a process not in a stopped state.
#[cfg(feature = "memory")]
mod memory;
#[cfg(feature = "notifier")]
mod notifier;
mod physical_observer;
mod regs;
mod waitid;

use core::mem::MaybeUninit;
use std::fmt;
use std::num::NonZeroU64;

use nix::sys::ptrace;
// Re-exports so that nothing else needs to depend on `nix`.
pub use nix::sys::ptrace::Options;
pub use nix::sys::signal::Signal;
use nix::sys::wait::WaitPidFlag;
use nix::sys::wait::WaitStatus;
pub use reverie_process::ExitStatus;
pub use reverie_process::Pid;
#[cfg(feature = "notifier")]
pub use reverie_process::ControllerSpawnToken;
pub use syscalls::Errno;
use syscalls::Sysno;
use thiserror::Error;

#[cfg(feature = "notifier")]
pub use crate::notifier::CleanupStopLease;
#[cfg(feature = "notifier")]
pub use crate::notifier::CleanupStopTransfer;
#[cfg(feature = "notifier")]
pub use crate::notifier::OriginalRootStartup;
#[cfg(feature = "notifier")]
pub use crate::notifier::OriginalRootCleanupAuthority;
#[cfg(feature = "notifier")]
pub use crate::notifier::OriginalRootStartupError;
#[cfg(feature = "notifier")]
pub use crate::notifier::OriginalRootStartupIdentity;
#[cfg(feature = "notifier")]
pub use crate::notifier::StopResolutionOutcome;
#[cfg(feature = "notifier")]
pub use crate::notifier::StopResolutionLaterStatus;
#[cfg(feature = "notifier")]
pub use crate::notifier::StopResolutionResumeErrorOutcome;
#[cfg(feature = "notifier")]
pub use crate::notifier::StopResolutionWatcher;
#[cfg(feature = "notifier")]
pub use crate::notifier::TerminalCleanup;
#[cfg(feature = "notifier")]
pub use crate::notifier::TerminalCleanupContinue;
#[cfg(feature = "notifier")]
pub use crate::notifier::TransferredStopCompletion;
#[cfg(feature = "notifier")]
pub use crate::notifier::TransferredStopResolution;
pub use crate::physical_observer::*;
pub use crate::regs::*;
use crate::waitid::IdType;
use crate::waitid::waitid;

/// Event-generation-local identity assigned to one decoded ptrace stop.
///
/// Unlike [`PhysicalStatusId`], this identity is always present even when no
/// physical-event observer is attached. Its numeric value is meaningful only
/// together with the immutable notifier generation carried by the same typed
/// tracee state.
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub struct LogicalStopId(NonZeroU64);

impl LogicalStopId {
    pub(crate) fn from_raw(raw: u64) -> Option<Self> {
        NonZeroU64::new(raw).map(Self)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }

    /// Returns whether this stop was allocated strictly after `predecessor`.
    ///
    /// The ordering is meaningful only after the caller has proved that both
    /// IDs belong to the same immutable notifier Event generation.
    pub const fn is_strictly_after(self, predecessor: Self) -> bool {
        self.0.get() > predecessor.0.get()
    }
}

/// Raw result of a specialized observed ptrace continuation.
///
/// Unlike [`Error`], this preserves ESRCH as its exact kernel errno and keeps
/// the observer attempt needed for causal successor resolution.
#[derive(Debug)]
pub struct PhysicalResumeFailure {
    error: Errno,
    attempt: Option<PhysicalResumeAttempt>,
}

impl PhysicalResumeFailure {
    /// Returns the exact ptrace errno without zombie reinterpretation.
    pub fn error(&self) -> Errno {
        self.error
    }

    /// Returns the exact physical resume attempt, when an observer was bound.
    pub fn attempt(&self) -> Option<PhysicalResumeAttempt> {
        self.attempt
    }
}

/// Immutable generation token carried through every typed tracee state.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct TraceeToken {
    #[cfg(feature = "notifier")]
    event: notifier::EventHandle,
    #[cfg(feature = "notifier")]
    physical_status: Option<PhysicalStatusId>,
    #[cfg(feature = "notifier")]
    logical_stop: Option<LogicalStopId>,
    #[cfg(feature = "notifier")]
    owns_claimed_exit_stop: bool,
    #[cfg(feature = "notifier")]
    failed_resume_disposition_retained_for_cleanup: Option<PhysicalStatusId>,
}

impl TraceeToken {
    fn new() -> Self {
        Self {
            #[cfg(feature = "notifier")]
            event: notifier::EventHandle::new(),
            #[cfg(feature = "notifier")]
            physical_status: None,
            #[cfg(feature = "notifier")]
            logical_stop: None,
            #[cfg(feature = "notifier")]
            owns_claimed_exit_stop: false,
            #[cfg(feature = "notifier")]
            failed_resume_disposition_retained_for_cleanup: None,
        }
    }

    #[cfg(feature = "notifier")]
    fn from_controller_launch(token: ControllerSpawnToken) -> Self {
        Self {
            event: notifier::EventHandle::from_controller_launch(token),
            physical_status: None,
            logical_stop: None,
            owns_claimed_exit_stop: false,
            failed_resume_disposition_retained_for_cleanup: None,
        }
    }

    fn current_or_new(pid: Pid) -> Result<Self, Errno> {
        #[cfg(not(feature = "notifier"))]
        let _ = pid;
        Ok(Self {
            #[cfg(feature = "notifier")]
            event: notifier::EventHandle::current_or_new(pid)?,
            #[cfg(feature = "notifier")]
            physical_status: None,
            #[cfg(feature = "notifier")]
            logical_stop: None,
            #[cfg(feature = "notifier")]
            owns_claimed_exit_stop: false,
            #[cfg(feature = "notifier")]
            failed_resume_disposition_retained_for_cleanup: None,
        })
    }

    fn current_or_error(pid: Pid) -> Self {
        #[cfg(not(feature = "notifier"))]
        let _ = pid;
        Self {
            #[cfg(feature = "notifier")]
            event: notifier::EventHandle::current_or_error(pid),
            #[cfg(feature = "notifier")]
            physical_status: None,
            #[cfg(feature = "notifier")]
            logical_stop: None,
            #[cfg(feature = "notifier")]
            owns_claimed_exit_stop: false,
            #[cfg(feature = "notifier")]
            failed_resume_disposition_retained_for_cleanup: None,
        }
    }

    #[cfg(feature = "notifier")]
    fn from_event(event: notifier::EventHandle) -> Self {
        Self {
            event,
            physical_status: None,
            logical_stop: None,
            owns_claimed_exit_stop: false,
            failed_resume_disposition_retained_for_cleanup: None,
        }
    }

    #[cfg(feature = "notifier")]
    fn from_observed_event(
        event: notifier::EventHandle,
        physical_status: Option<PhysicalStatusId>,
        logical_stop: Option<LogicalStopId>,
    ) -> Self {
        Self {
            event,
            physical_status,
            logical_stop,
            owns_claimed_exit_stop: false,
            failed_resume_disposition_retained_for_cleanup: None,
        }
    }

    #[cfg(feature = "notifier")]
    fn from_claimed_exit_event(
        event: notifier::EventHandle,
        physical_status: Option<PhysicalStatusId>,
        logical_stop: LogicalStopId,
    ) -> Self {
        Self {
            event,
            physical_status,
            logical_stop: Some(logical_stop),
            owns_claimed_exit_stop: true,
            failed_resume_disposition_retained_for_cleanup: None,
        }
    }

    #[cfg(feature = "notifier")]
    fn event(&self) -> &notifier::EventHandle {
        &self.event
    }

    #[cfg(feature = "notifier")]
    fn into_running(mut self) -> Self {
        self.physical_status = None;
        self.logical_stop = None;
        self.owns_claimed_exit_stop = false;
        self.failed_resume_disposition_retained_for_cleanup = None;
        self
    }

    #[cfg(feature = "notifier")]
    fn into_observed_stopped(mut self, physical_status: PhysicalStatusId) -> Self {
        self.physical_status = Some(physical_status);
        self.owns_claimed_exit_stop = false;
        self
    }

    #[cfg(feature = "notifier")]
    fn into_stopped(mut self) -> Self {
        if self.logical_stop.is_none() {
            self.logical_stop = Some(self.event.allocate_logical_stop());
        }
        self
    }

    #[cfg(not(feature = "notifier"))]
    fn into_running(self) -> Self {
        self
    }
}

#[cfg(target_arch = "x86_64")]
const NT_X86_XSTATE: i32 = 0x202;

/// An error that occurred during tracing.
#[derive(Error, Debug, Eq, PartialEq)]
pub enum Error {
    /// A low-level errno.
    #[error(transparent)]
    Errno(#[from] Errno),

    /// The tracee died unexpectedly. This should be handled gracefully by
    /// reaping the zombie.
    #[error("tracee {0} is a zombie")]
    Died(Zombie),
}

impl From<nix::errno::Errno> for Error {
    fn from(err: nix::errno::Errno) -> Self {
        Self::Errno(Errno::new(err as i32))
    }
}

/// Represents an invalid state. Useful for errors.
#[derive(Debug, Eq, PartialEq)]
struct InvalidState(pub TryWait);

impl From<InvalidState> for TryWait {
    fn from(error: InvalidState) -> TryWait {
        error.0
    }
}

impl fmt::Display for InvalidState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "got unexpected status {}", self.0)
    }
}

impl std::error::Error for InvalidState {}

/// Indicates how a child was created (i.e., via `fork`, `vfork`, or `clone`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ChildOp {
    /// Stop before return from `fork(2)` or `clone(2)` with the exit signal set
    /// to `SIGCHLD`.
    Fork,

    /// Stop before return from `vfork(2)` or `clone(2)` with the `CLONE_VFORK`
    /// flag. When the tracee is continued after this stop, it will wait for
    /// child to exit/exec before continuing its execution (in other words, the
    /// usual behavior on `vfork(2)`).
    Vfork,

    /// Stop before return from `clone(2)`.
    Clone,
}

/// A stop event. Documentation is from `ptrace(2)`.
#[derive(Debug, Eq, PartialEq)]
pub enum Event {
    /// Stop event after a new child has been created (i.e., via `fork`, `vfork`,
    /// or `clone`).
    NewChild(ChildOp, Running),

    /// Stop before return from `execve(2)`. Since Linux 3.0,
    /// `PTRACE_GETEVENTMSG` returns the former thread ID.
    Exec(Pid),

    /// Stop before return from `vfork(2)` or `clone(2)` with the `CLONE_VFORK`
    /// flag, but after the child unblocked this tracee by exiting or execing.
    VforkDone,

    /// Stop before exit (including death from `exit_group(2)`), signal death, or
    /// exit caused by `execve(2)` in a multithreaded process.
    /// `PTRACE_GETEVENTMSG` returns the exit status. Registers can be examined
    /// (unlike when "real" exit happens). The tracee is still alive; it needs to
    /// be `PTRACE_CONT`ed or `PTRACE_DETACH`ed to finish exiting. A stop claimed
    /// asynchronously through `ExitFuture` is narrower: it must be continued,
    /// because its exact-once continuation is shared with terminal cleanup.
    Exit,

    /// Stop triggered by a `seccomp(2)` rule on tracee syscall entry when
    /// `PTRACE_O_TRACESECCOMP` has been set by the tracer. The seccomp event
    /// message data (from the `SECCOMP_RET_DATA` portion of the seccomp filter
    /// rule) can be retrieved with `PTRACE_GETEVENTMSG`. The semantics of this
    /// stop are described in detail in a separate section below.
    Seccomp,

    /// Stop induced by PTRACE_INTERRUPT command, or group-stop, or initial
    /// ptrace-stop when a new child is attached (only if attached using
    /// PTRACE_SEIZE).
    Stop,

    /// The tracee was stopped by execution of a system call.
    Syscall,

    /// The tracee was stopped by delivery of a signal.
    Signal(Signal),
}

impl Event {
    fn new_child(task: &Stopped, child_pid: Pid) -> Result<Running, Error> {
        let child = Running::from_current_or_new(child_pid)?;
        #[cfg(feature = "notifier")]
        if let Some(observer) = task.physical_event_observer() {
            child
                .attach_physical_event_observer(&observer)
                .map_err(|_| Errno::EPROTO)?;
        }
        Ok(child)
    }

    /// Converts a raw i32 to a ptrace event and gets any associated data.
    fn from_ptrace_event(task: &Stopped, event: i32) -> Result<Self, Error> {
        // Note that there is no danger in calling ptrace here because the
        // process is guaranteed to be in a ptrace-stop state when this function
        // is called.
        match event {
            libc::PTRACE_EVENT_FORK => {
                // Get the pid of the child immediately since we almost always
                // want that.
                let child_pid = Pid::from_raw(task.getevent()? as i32);
                #[cfg(all(test, feature = "notifier"))]
                notifier::register_new_child_for_test_cleanup(task.1.event(), child_pid)?;
                Ok(Self::NewChild(
                    ChildOp::Fork,
                    Self::new_child(task, child_pid)?,
                ))
            }
            libc::PTRACE_EVENT_VFORK => {
                // Get the pid of the child immediately since we almost always
                // want that.
                let child_pid = Pid::from_raw(task.getevent()? as i32);
                #[cfg(all(test, feature = "notifier"))]
                notifier::register_new_child_for_test_cleanup(task.1.event(), child_pid)?;
                Ok(Self::NewChild(
                    ChildOp::Vfork,
                    Self::new_child(task, child_pid)?,
                ))
            }
            libc::PTRACE_EVENT_CLONE => {
                // Get the pid of the child immediately since we almost always
                // want that.
                let child_pid = Pid::from_raw(task.getevent()? as i32);
                #[cfg(all(test, feature = "notifier"))]
                notifier::register_new_child_for_test_cleanup(task.1.event(), child_pid)?;
                Ok(Self::NewChild(
                    ChildOp::Clone,
                    Self::new_child(task, child_pid)?,
                ))
            }
            libc::PTRACE_EVENT_EXEC => {
                // Get the pid of the thread group leader that this call to exec
                // is replacing. This is not necessarily equal to `pid` since
                // another thread besides the main thread can call `exec`. This
                // information is necessary to track the "death" of a process.
                let new_pid = Pid::from_raw(task.getevent()? as i32);
                Ok(Self::Exec(new_pid))
            }
            libc::PTRACE_EVENT_VFORK_DONE => Ok(Self::VforkDone),
            libc::PTRACE_EVENT_EXIT => {
                // Note that we can get the exit status here using `getevent`,
                // but that's almost never what we want to do. It is better to
                // get that during the final exit event.
                Ok(Self::Exit)
            }
            libc::PTRACE_EVENT_SECCOMP => Ok(Self::Seccomp),
            libc::PTRACE_EVENT_STOP => Ok(Self::Stop),
            _ => unreachable!("unknown ptrace event {:#x}", event),
        }
    }
}

/// Helper function for waiting on one or more processes. Returns `None` if
/// `WaitPidFlag::WNOHANG` was specified and the process is still running.
fn wait(id: IdType, flags: WaitPidFlag) -> Result<Option<WaitStatus>, Errno> {
    loop {
        let result = waitid(id, flags).map(|status| {
            if status == WaitStatus::StillAlive {
                None
            } else {
                Some(status)
            }
        });

        if result == Err(Errno::EINTR) {
            continue;
        }

        return result;
    }
}

/// The result of a non-blocking wait. A process can be in one of three main
/// states: running, ptrace-stopped, or exited.
///
/// Both `Clone` and `Copy` are intentionally not implemented. This is to enforce
/// type safety.
#[derive(Debug, Eq, PartialEq)]
pub enum TryWait {
    /// The process is in either a stopped state or an exited state.
    Wait(Wait),

    /// The process is in a running state and thus can only be waited on.
    ///
    /// When the process is successfully waited on, it transitions to a waited
    /// state.
    Running(Running),
}

impl TryWait {
    /// Returns the PID for this attempted wait.
    pub fn pid(&self) -> Pid {
        match self {
            Self::Wait(wait) => wait.pid(),
            Self::Running(running) => running.pid(),
        }
    }

    /// Returns true if we're in a running state. Note that this may not reflect
    /// the real *current* state that we may not yet have observed.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running(_))
    }

    /// Returns true if we're in a stopped state. Note that this may not reflect
    /// the real *current* state that we may not yet have observed.
    pub fn is_stopped(&self) -> bool {
        matches!(self, Self::Wait(Wait::Stopped(_, _)))
    }

    /// Assumes the process is in a stopped state. Panics if it isn't.
    pub fn assume_stopped(self) -> (Stopped, Event) {
        match self {
            Self::Wait(Wait::Stopped(stopped, event)) => (stopped, event),
            status => panic!("{:?}", InvalidState(status)),
        }
    }

    /// Assumes the process is in a running state. Panics if it isn't.
    pub fn assume_running(self) -> Running {
        match self {
            Self::Running(running) => running,
            status => panic!("{:?}", InvalidState(status)),
        }
    }

    /// Assumes the process is in an exited state. Panics if it isn't.
    pub fn assume_exited(self) -> (Pid, ExitStatus) {
        match self {
            Self::Wait(Wait::Exited(pid, exit_status)) => (pid, exit_status),
            status => panic!("{:?}", InvalidState(status)),
        }
    }
}

impl fmt::Display for TryWait {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Wait(wait) => write!(f, "{}", wait),
            Self::Running(running) => write!(f, "pid {} is running", running.pid()),
        }
    }
}

impl From<Running> for TryWait {
    fn from(status: Running) -> Self {
        Self::Running(status)
    }
}

impl From<Wait> for TryWait {
    fn from(wait: Wait) -> Self {
        Self::Wait(wait)
    }
}

/// The result of a blocking wait. A process in this state is guaranteed to not
/// be in a running state.
///
/// Both `Clone` and `Copy` are intentionally not implemented. This is to enforce
/// type safety.
#[derive(Debug, Eq, PartialEq)]
pub enum Wait {
    /// The process is in a stopped state and thus only operations that can be
    /// done during a stopped state are allowed (i.e., ptrace operations).
    ///
    /// When the process is resumed, it transitions to a running state.
    Stopped(Stopped, Event),

    /// The process has exited with an exit status.
    Exited(Pid, ExitStatus),
}

impl Wait {
    /// Returns the PID for this state.
    pub fn pid(&self) -> Pid {
        match self {
            Self::Stopped(stopped, _) => stopped.pid(),
            Self::Exited(pid, _exit_status) => *pid,
        }
    }

    /// Returns the physical status identity retained by a stopped result.
    ///
    /// Final exit identities remain in the observer's retained terminal
    /// records because the legacy `Wait::Exited` variant carries no token.
    #[cfg(feature = "notifier")]
    pub fn physical_status_id(&self) -> Option<PhysicalStatusId> {
        match self {
            Self::Stopped(stopped, _) => stopped.physical_status_id(),
            Self::Exited(_, _) => None,
        }
    }

    /// Assumes the process is in a stopped state. Panics if it isn't.
    pub fn assume_stopped(self) -> (Stopped, Event) {
        match self {
            Self::Stopped(stopped, event) => (stopped, event),
            state => panic!("{:?}", InvalidState(state.into())),
        }
    }

    /// Assumes the process is in an exited state. Panics if it isn't.
    pub fn assume_exited(self) -> (Pid, ExitStatus) {
        match self {
            Self::Exited(pid, exit_status) => (pid, exit_status),
            state => panic!("{:?}", InvalidState(state.into())),
        }
    }

    /// Converts a raw `i32` status to this type.
    ///
    /// Preconditions:
    /// The process must not be in a running state.
    pub fn from_raw(pid: Pid, status: i32) -> Result<Self, Error> {
        Self::from_raw_with_token(pid, status, TraceeToken::new())
    }

    fn from_raw_with_token(pid: Pid, status: i32, token: TraceeToken) -> Result<Self, Error> {
        Ok(if libc::WIFEXITED(status) {
            Wait::Exited(pid, ExitStatus::Exited(libc::WEXITSTATUS(status)))
        } else if libc::WIFSIGNALED(status) {
            let sig = Signal::try_from(libc::WTERMSIG(status)).map_err(|_| Errno::EINVAL)?;
            Wait::Exited(pid, ExitStatus::Signaled(sig, libc::WCOREDUMP(status)))
        } else if libc::WIFSTOPPED(status) {
            let task = Stopped::from_token(pid, token);

            let event = if libc::WSTOPSIG(status) == libc::SIGTRAP | 0x80 {
                Event::Syscall
            } else if (status >> 16) == 0 {
                let sig = Signal::try_from(libc::WSTOPSIG(status)).map_err(|_| Errno::EINVAL)?;
                Event::Signal(sig)
            } else {
                let sig = Signal::try_from(libc::WSTOPSIG(status)).map_err(|_| Errno::EINVAL)?;

                let event = status >> 16;

                // PTRACE_EVENT_STOP is not guaranteed to return the correct
                // signal, so we ignore it here.
                debug_assert!(event == libc::PTRACE_EVENT_STOP || sig == Signal::SIGTRAP);

                let event = Event::from_ptrace_event(&task, event)?;
                #[cfg(all(test, feature = "notifier"))]
                if matches!(event, Event::NewChild(..)) {
                    notifier::pause_sync_new_child_decode(task.1.event());
                }
                event
            };

            Wait::Stopped(task, event)
        } else if libc::WIFCONTINUED(status) {
            // TODO: Handle continued status.
            unimplemented!("Continued status not yet handled")
        } else {
            panic!("PID {} got unexpected status: {:#x}", pid, status)
        })
    }
}

impl TryFrom<WaitStatus> for Wait {
    type Error = Error;

    /// Converts a `WaitStatus` to this type.
    ///
    /// Preconditions:
    /// The process must not be in a `StillAlive` state.
    fn try_from(wait_status: WaitStatus) -> Result<Self, Error> {
        Self::from_wait_status_with_token(wait_status, TraceeToken::new())
    }
}

impl Wait {
    fn from_wait_status_with_token(
        wait_status: WaitStatus,
        token: TraceeToken,
    ) -> Result<Self, Error> {
        Ok(match wait_status {
            WaitStatus::Exited(pid, code) => Self::Exited(pid.into(), ExitStatus::Exited(code)),
            WaitStatus::Signaled(pid, sig, coredump) => {
                Self::Exited(pid.into(), ExitStatus::Signaled(sig, coredump))
            }
            WaitStatus::Stopped(pid, sig) => {
                let event = Event::Signal(sig);
                Self::Stopped(Stopped::from_token(pid.into(), token), event)
            }
            WaitStatus::PtraceEvent(pid, sig, event) => {
                // PTRACE_EVENT_STOP is not guaranteed to return the correct
                // signal, so we ignore it here.
                debug_assert!(event == libc::PTRACE_EVENT_STOP || sig == Signal::SIGTRAP);
                let task = Stopped::from_token(pid.into(), token);
                let event = Event::from_ptrace_event(&task, event)?;
                Self::Stopped(task, event)
            }
            WaitStatus::PtraceSyscall(pid) => {
                let event = Event::Syscall;
                Self::Stopped(Stopped::from_token(pid.into(), token), event)
            }
            WaitStatus::Continued(_pid) => {
                // Not possible because we aren't using WaitPidFlag::WCONTINUED
                // anywhere.
                unreachable!("unexpected WaitStatus::Continued");
            }
            WaitStatus::StillAlive => {
                // The precondition of this function forbids this.
                unreachable!("precondition violated with WaitStatus::StillAlive");
            }
        })
    }
}

impl fmt::Display for Wait {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Stopped(stopped, event) => {
                write!(f, "pid {} stopped ({:?})", stopped.pid(), event)
            }
            Self::Exited(pid, exit_status) => write!(f, "pid {} exited ({:?})", pid, exit_status),
        }
    }
}

// libc crate doesn't provide this struct
#[repr(C)]
struct ptrace_peeksiginfo_args {
    off: u64,
    flags: u32,
    nr: u32,
}

/// Exact byte length of Linux's kernel signal-set ABI.
///
/// On safeptrace's x86-64 and AArch64 targets, Linux exposes 64 signals, so the
/// kernel `sigset_t` copied by `PTRACE_GETSIGMASK` and `PTRACE_SETSIGMASK` is
/// exactly eight bytes. This is deliberately independent of `libc::sigset_t`,
/// whose size and unused storage belong to the C library ABI instead.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub const LINUX_KERNEL_SIGSET_SIZE: usize = 64 / u8::BITS as usize;

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("the Linux kernel sigset ABI size needs auditing for this target architecture");

// Keep a compile-time witness for the byte count passed in ptrace's `addr`
// argument. If the signal-count expression above changes, this must be
// reviewed against the Linux ptrace ABI rather than silently changing calls.
const _: [(); 8] = [(); LINUX_KERNEL_SIGSET_SIZE];

/// Opaque bytes for Linux `PTRACE_GETSIGMASK` and `PTRACE_SETSIGMASK`.
///
/// The bytes use the tracee's native Linux kernel ABI representation. They are
/// intentionally not converted through `libc::sigset_t`: callers can preserve
/// and restore the exact bytes returned by [`Stopped::getsigmask`], or provide
/// exactly eight bytes obtained from the same ABI.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PtraceSigmask([u8; LINUX_KERNEL_SIGSET_SIZE]);

impl PtraceSigmask {
    /// Creates a ptrace signal mask from its exact kernel-ABI bytes.
    pub const fn from_bytes(bytes: [u8; LINUX_KERNEL_SIGSET_SIZE]) -> Self {
        Self(bytes)
    }

    /// Returns the exact kernel-ABI bytes without changing them.
    pub const fn as_bytes(&self) -> &[u8; LINUX_KERNEL_SIGSET_SIZE] {
        &self.0
    }

    /// Consumes the mask and returns its exact kernel-ABI bytes.
    pub const fn into_bytes(self) -> [u8; LINUX_KERNEL_SIGSET_SIZE] {
        self.0
    }
}

impl TryFrom<&[u8]> for PtraceSigmask {
    type Error = std::array::TryFromSliceError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        <[u8; LINUX_KERNEL_SIGSET_SIZE]>::try_from(bytes).map(Self::from_bytes)
    }
}

bitflags::bitflags! {
    /// Flags for ptrace peeksiginfo
    #[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Clone, Copy)]
    pub struct PeekSigInfoFlags: u32 {
        /// dumping signals from the process-wide signal queue. signals are
        /// read from the per-thread queue of the specified thread if this
        /// flag is not set.
        const SHARED = 1;
    }
}

/// A process that is in a stopped state and allows ptrace operations to be
/// performed.
#[derive(Debug, Hash, Eq, PartialEq)]
pub struct Stopped(Pid, TraceeToken);

/// Failed conversion of a typed stopped capability into cleanup authority.
///
/// The original [`Stopped`] is retained so callers never lose the only typed
/// transition owner when cleanup-state preflight refuses the lease.
#[cfg(feature = "notifier")]
#[derive(Debug)]
pub struct CleanupStopLeaseError {
    error: Errno,
    stopped: Stopped,
}

#[cfg(feature = "notifier")]
impl CleanupStopLeaseError {
    /// Returns the exact cleanup protocol error.
    pub fn errno(&self) -> Errno {
        self.error
    }

    /// Recovers the original typed stopped capability.
    pub fn into_stopped(self) -> Stopped {
        self.stopped
    }

    /// Recovers both the exact error and the original stopped capability.
    pub fn into_parts(self) -> (Errno, Stopped) {
        (self.error, self.stopped)
    }
}

#[cfg(feature = "notifier")]
impl fmt::Display for CleanupStopLeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cleanup stop lease preflight failed: {}", self.error)
    }
}

#[cfg(feature = "notifier")]
impl std::error::Error for CleanupStopLeaseError {}

/// A non-resuming memory handle derived from an observed stopped state.
///
/// This handle deliberately exposes only [`reverie_memory::MemoryAccess`]. It
/// cannot perform a ptrace state transition or create a second resume owner.
#[cfg(feature = "memory")]
#[derive(Debug)]
pub struct StoppedMemory(Stopped);

#[cfg(feature = "memory")]
impl reverie_memory::MemoryAccess for StoppedMemory {
    fn read_vectored(
        &self,
        remote: &[std::io::IoSlice],
        local: &mut [std::io::IoSliceMut],
    ) -> Result<usize, Errno> {
        reverie_memory::MemoryAccess::read_vectored(&self.0, remote, local)
    }

    fn write_vectored(
        &mut self,
        local: &[std::io::IoSlice],
        remote: &mut [std::io::IoSliceMut],
    ) -> Result<usize, Errno> {
        reverie_memory::MemoryAccess::write_vectored(&mut self.0, local, remote)
    }

    fn read<'a, A>(&self, addr: A, buf: &mut [u8]) -> Result<usize, Errno>
    where
        A: Into<reverie_memory::Addr<'a, u8>>,
    {
        reverie_memory::MemoryAccess::read(&self.0, addr, buf)
    }

    fn read_exact_with_user_access<'a, A>(&self, addr: A, buf: &mut [u8]) -> Result<(), Errno>
    where
        A: Into<reverie_memory::Addr<'a, u8>>,
    {
        reverie_memory::MemoryAccess::read_exact_with_user_access(&self.0, addr, buf)
    }

    fn write(&mut self, addr: reverie_memory::AddrMut<u8>, buf: &[u8]) -> Result<usize, Errno> {
        reverie_memory::MemoryAccess::write(&mut self.0, addr, buf)
    }
}

impl Stopped {
    /// Helper for converting from the Errno type.
    ///
    /// # Why is this needed?
    ///
    /// According to ptrace(2), any ptrace operation may return ESRCH
    /// ("No such process") for one of three reasons:
    ///  1. The process was observed to be in a stopped state and died
    ///     unexpectedly.
    ///  2. The process is not currently being traced by the caller.
    ///  3. The process is not in a stopped state.
    ///
    /// Since we know that reasons (2) and (3) only occur due to
    /// programmer errors that this API is designed to prevent, we can
    /// safely assume that this ESRCH means the tracee has died
    /// unexpectedly while in a stopped state.
    ///
    /// For more information, please see the "Death under ptrace" section
    /// in `man 2 ptrace`.
    fn map_err(&self, err: Errno) -> Error {
        if err == Errno::ESRCH {
            Error::Died(Zombie::from_token(self.0, self.1.clone()))
        } else {
            Error::Errno(err)
        }
    }

    // Helper for converting from the nix::Error type.
    fn map_nix_err(&self, err: nix::Error) -> Error {
        self.map_err(Errno::new(err as i32))
    }

    /// Returns a future that is notified when the next exit stop occurs. This
    /// is received asynchronously regardless of what the process was doing at
    /// the time. This is useful for canceling futures when a process enters a
    /// `PTRACE_EVENT_EXIT` (such as when one thread calls `exit_group` and
    /// causes all other threads to suddenly exit).
    ///
    /// Exactly one future for this immutable tracee generation can claim the
    /// exit stop and return a [`Stopped`] capability. Duplicate or re-polled
    /// futures return [`Errno::EALREADY`]. An unclaimed capability also expires
    /// before terminal publication or cancellation cleanup advances the
    /// tracee.
    #[cfg(feature = "notifier")]
    pub fn exit_event(&self) -> notifier::ExitFuture {
        notifier::ExitFuture::new(self.0, &self.1)
    }

    /// Arms the one generation-bound side channel used to distinguish a
    /// canceled SIGSTOP delivery from a genuine group stop and its later
    /// `WCONTINUED` status.
    ///
    /// The watcher must be created before resuming this stopped capability
    /// with SIGSTOP. Continued statuses are never returned through [`Wait`].
    #[cfg(feature = "notifier")]
    pub fn watch_stop_resolution(&self) -> Result<StopResolutionWatcher, Errno> {
        StopResolutionWatcher::new(self.0, &self.1)
    }

    /// Returns a generation-bound terminal cleanup acknowledgment.
    ///
    /// This is primarily useful with [`Stopped::new_unchecked`] during
    /// cancellation after the caller has independently validated that the TID
    /// still names the expected ptrace generation.
    #[cfg(feature = "notifier")]
    pub fn terminal_cleanup(&self) -> TerminalCleanup {
        TerminalCleanup::new(self.0, &self.1)
    }

    /// Creates a new stopped state. This is useful when we know the process is
    /// in a stopped state already.
    ///
    /// Using this method is unsound because there is no check to verify that the
    /// pid really is in a stopped state. It is better to arrive at a stopped
    /// state via other methods such as `Running::wait`.
    pub fn new_unchecked(pid: Pid) -> Self {
        Self::from_token(pid, TraceeToken::current_or_error(pid))
    }

    /// Creates an unchecked stopped state carrying an externally observed
    /// physical status.
    ///
    /// This is the pre-notifier counterpart of [`Stopped::new_unchecked`]. The
    /// caller must prove that `status` was returned for this exact unreaped
    /// child and that no other stopped capability exists. The new Event is not
    /// registered until the returned state is resumed and waited again.
    #[cfg(feature = "notifier")]
    pub fn new_observed_unchecked(
        pid: Pid,
        observer: &PhysicalEventObserver,
        status: PhysicalStatusId,
    ) -> Result<Self, PhysicalObserverAttachError> {
        let mut token = TraceeToken::new();
        token.event().attach_physical_observer(observer)?;
        token.physical_status = Some(status);
        let generation = token.event().physical_generation();
        observer.link_pre_registration_task(PhysicalTaskIdentity::direct_child(pid), generation);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::DirectStopped,
        );
        Ok(Self::from_token(pid, token))
    }

    /// Creates an unchecked stopped state joined to the currently registered
    /// proc generation for `pid`.
    ///
    /// Like [`Stopped::new_unchecked`], the caller must independently prove
    /// that the exact TID is stopped. Unlike that constructor, generation
    /// capture/read failures are returned rather than creating an unbound
    /// notifier state.
    #[cfg(feature = "notifier")]
    pub fn try_new_current_unchecked(pid: Pid) -> Result<Self, Errno> {
        Ok(Self::from_token(pid, TraceeToken::current_or_new(pid)?))
    }

    fn from_token(pid: Pid, token: TraceeToken) -> Self {
        #[cfg(feature = "notifier")]
        let token = token.into_stopped();
        Self(pid, token)
    }

    /// Returns the process ID of the tracee.
    pub fn pid(&self) -> Pid {
        self.0
    }

    /// Attaches a bounded physical-event observer before this generation's
    /// first kernel wait owner starts.
    #[cfg(feature = "notifier")]
    pub fn attach_physical_event_observer(
        &self,
        observer: &PhysicalEventObserver,
    ) -> Result<(), PhysicalObserverAttachError> {
        self.1.event().attach_physical_observer(observer)
    }

    /// Returns the observer attached to this immutable tracee generation.
    #[cfg(feature = "notifier")]
    pub fn physical_event_observer(&self) -> Option<PhysicalEventObserver> {
        self.1.event().physical_observer()
    }

    /// Returns this immutable notifier generation's diagnostic identity.
    #[cfg(feature = "notifier")]
    pub fn physical_event_generation(&self) -> PhysicalEventGenerationId {
        self.1.event().physical_generation()
    }

    /// Returns whether this exact notifier generation still owns the original
    /// root's process-wide continued-status authority.
    #[cfg(feature = "notifier")]
    pub fn continued_status_authority_is_live(&self) -> bool {
        self.1.event().continued_authority_is_live()
    }

    /// Permanently revokes this generation's ability to arm another
    /// stop-resolution watch. Existing worker wait flags remain immutable.
    #[cfg(feature = "notifier")]
    pub fn revoke_continued_status_authority(&self) {
        self.1.event().revoke_continued_authority();
    }

    /// Returns the physical status that produced this stopped capability.
    #[cfg(feature = "notifier")]
    pub fn physical_status_id(&self) -> Option<PhysicalStatusId> {
        self.1.physical_status
    }

    /// Returns the mandatory identity of this exact logical stopped state.
    ///
    /// The identity is local to this value's immutable notifier generation;
    /// compare it only while also comparing [`TerminalCleanup::same_generation`].
    #[cfg(feature = "notifier")]
    pub fn logical_stop_id(&self) -> LogicalStopId {
        self.1
            .logical_stop
            .expect("every stopped notifier token has a logical stop identity")
    }

    /// Consumes this exact stopped capability into cancellation-cleanup
    /// authority for the same immutable notifier generation.
    ///
    /// This is the normal way to retain a decoded stop after cancellation. The
    /// consumed [`Stopped`] can no longer independently issue a ptrace
    /// transition, so the returned lease remains the only transition owner.
    #[cfg(feature = "notifier")]
    pub fn into_cleanup_stop_lease(self) -> Result<CleanupStopLease, CleanupStopLeaseError> {
        let Self(pid, token) = self;
        TerminalCleanup::lease_stopped_token(pid, token).map_err(|(error, token)| {
            CleanupStopLeaseError {
                error,
                stopped: Self(pid, token),
            }
        })
    }

    /// Copies this stopped capability into an opaque, generation-bound
    /// cleanup transfer without granting ptrace transition authority.
    ///
    /// This escape hatch is for cancellation shadows that must outlive the
    /// future currently holding the typed stop. Prefer
    /// [`Stopped::into_cleanup_stop_lease`] whenever the stopped value can be
    /// consumed directly.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the returned transfer is either discarded
    /// when this stopped capability transitions successfully, or activated
    /// only after this stopped capability and every equivalent transition
    /// owner have become unreachable. Creating more than one transfer for the
    /// same stopped capability is invalid.
    #[cfg(feature = "notifier")]
    pub unsafe fn transfer_cleanup_stop(&self) -> CleanupStopTransfer {
        CleanupStopTransfer::from_stopped(self.0, &self.1)
    }

    /// Assigns any failed transition disposition to matching cancellation
    /// cleanup ownership for this exact stopped state and physical status.
    ///
    /// The policy persists in this value when it is consumed by `resume`,
    /// `step`, `syscall`, or `detach`. A failed transition then leaves the
    /// physical status undisposed so the supplied cleanup generation can cite
    /// it as [`PhysicalStatusDisposition::CancellationCleanup`].
    ///
    /// # Safety
    ///
    /// The caller must retain independent, durable ownership of `cleanup` and
    /// `status` until a failed transition is made unreachable and recorded as
    /// [`PhysicalStatusDisposition::CancellationCleanup`]. The borrowed handle
    /// alone does not establish that lifetime or disposition.
    #[cfg(feature = "notifier")]
    pub unsafe fn retain_failed_resume_disposition_for_cleanup(
        &mut self,
        cleanup: &TerminalCleanup,
        status: PhysicalStatusId,
    ) -> Result<(), Errno> {
        if !cleanup.matches_stopped(self.0, &self.1) || self.1.physical_status != Some(status) {
            return Err(Errno::EINVAL);
        }
        self.1.failed_resume_disposition_retained_for_cleanup = Some(status);
        Ok(())
    }

    /// Returns memory access bound to this stopped generation without exposing
    /// another ptrace transition capability.
    #[cfg(feature = "memory")]
    pub fn memory(&self) -> StoppedMemory {
        StoppedMemory(Self::from_token(self.0, self.1.clone()))
    }

    #[cfg(feature = "notifier")]
    fn begin_physical_resume(
        &self,
        operation: PhysicalResumeOperation,
        signal: Option<Signal>,
    ) -> Option<(PhysicalEventObserver, PhysicalResumeAttempt)> {
        let observer = self.1.event().physical_observer()?;
        let attempt = observer.begin_resume(PhysicalResumeContext {
            generation: Some(self.1.event().physical_generation()),
            task: self.1.event().physical_task_identity(self.0),
            source_status: self.1.physical_status,
            operation,
            signal: signal.map(|signal| signal as i32),
            owner: PhysicalResumeOwner::TypedStopped,
        });
        Some((observer, attempt))
    }

    #[cfg(not(feature = "notifier"))]
    fn begin_physical_resume(
        &self,
        _operation: PhysicalResumeOperation,
        _signal: Option<Signal>,
    ) -> Option<(PhysicalEventObserver, PhysicalResumeAttempt)> {
        None
    }

    #[cfg(feature = "notifier")]
    fn finish_physical_resume(
        &self,
        observed: Option<(PhysicalEventObserver, PhysicalResumeAttempt)>,
        result: &Result<(), nix::errno::Errno>,
    ) {
        if let Some((observer, attempt)) = observed {
            observer.finish_resume(
                attempt,
                match result {
                    Ok(()) => PhysicalResumeOutcome::Success,
                    Err(error) => PhysicalResumeOutcome::Error(*error as i32),
                },
            );
            if matches!(result, Err(nix::errno::Errno::ESRCH))
                && let Some(status) = self.1.physical_status
                && self.1.failed_resume_disposition_retained_for_cleanup != Some(status)
            {
                observer.finish_status(status, PhysicalStatusDisposition::OrdinaryHandled);
            }
        }
    }

    #[cfg(not(feature = "notifier"))]
    fn finish_physical_resume(
        &self,
        _observed: Option<(PhysicalEventObserver, PhysicalResumeAttempt)>,
        _result: &Result<(), nix::errno::Errno>,
    ) {
    }

    /// Sets the ptracer options.
    pub fn setoptions(&self, options: ptrace::Options) -> Result<(), Error> {
        ptrace::setoptions(self.0.into(), options).map_err(|err| self.map_nix_err(err))
    }

    /// Gets a set of registers.
    ///
    /// `which` corresponds to one of:
    ///  * `libc::NT_PRSTATUS` for the general registers.
    ///  * `libc::NT_PRFPREG` for the floating point registers.
    ///
    /// There are others, but we don't use them.
    fn getregset<T>(&self, which: i32) -> Result<T, Error> {
        let mut regs = MaybeUninit::<T>::uninit();

        let mut iov = libc::iovec {
            iov_base: regs.as_mut_ptr() as *mut libc::c_void,
            iov_len: core::mem::size_of_val(&regs),
        };

        unsafe {
            syscalls::syscall!(
                Sysno::ptrace,
                // PTRACE_GETREGS isn't available on aarch64, so we must use
                // PTRACE_GETREGSET instead.
                libc::PTRACE_GETREGSET,
                self.0.as_raw(),
                which,
                &mut iov as *mut _
            )
        }
        .map_err(|err| self.map_err(err))?;

        // PTRACE_GETREGSET modifies the length to the real length of the
        // registers, but we should already know the exact number of registers
        // for this architecture.
        debug_assert_eq!(iov.iov_len, core::mem::size_of_val(&regs));

        Ok(unsafe { regs.assume_init() })
    }

    fn setregset<T>(&self, which: i32, regs: &T) -> Result<(), Error> {
        let iov = libc::iovec {
            iov_base: regs as *const _ as *mut _,
            iov_len: core::mem::size_of::<T>(),
        };

        unsafe {
            syscalls::syscall!(
                Sysno::ptrace,
                // PTRACE_SETREGS isn't available on aarch64, so we must use
                // PTRACE_SETREGSET instead.
                libc::PTRACE_SETREGSET,
                self.0.as_raw(),
                which,
                &iov as *const _
            )
        }
        .map_err(|err| self.map_err(err))?;

        Ok(())
    }

    /// Gets the current state of the general purpose registers.
    pub fn getregs(&self) -> Result<Regs, Error> {
        self.getregset(libc::NT_PRSTATUS)
    }

    /// Sets the general purpose registers.
    pub fn setregs(&self, regs: &Regs) -> Result<(), Error> {
        self.setregset(libc::NT_PRSTATUS, regs)
    }

    /// Gets the floating point registers.
    pub fn getfpregs(&self) -> Result<FpRegs, Error> {
        self.getregset(libc::NT_PRFPREG)
    }

    /// Sets the floating point registers.
    pub fn setfpregs(&self, regs: &FpRegs) -> Result<(), Error> {
        self.setregset(libc::NT_PRFPREG, regs)
    }

    /// Gets the complete variable-length x86 XSAVE state for the tracee.
    // TODO-HUMAN-REVIEW(PR-270): Review complete ptrace XSTATE preservation API.
    #[cfg(target_arch = "x86_64")]
    pub fn getxstate(&self) -> Result<XState, Error> {
        // CPUID.(EAX=0xD,ECX=0):ECX reports the maximum XSAVE area for all
        // processor-supported user components. The kernel returns the exact
        // active regset length through iov_len.
        let maximum = core::arch::x86_64::__cpuid_count(0x0d, 0).ecx as usize;
        let mut bytes = vec![0_u8; maximum.max(4096)];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        unsafe {
            syscalls::syscall!(
                Sysno::ptrace,
                libc::PTRACE_GETREGSET,
                self.0.as_raw(),
                NT_X86_XSTATE,
                &mut iov as *mut _
            )
        }
        .map_err(|err| self.map_err(err))?;
        if iov.iov_len > bytes.len() {
            return Err(Error::Errno(Errno::EOVERFLOW));
        }
        bytes.truncate(iov.iov_len);
        Ok(XState(bytes))
    }

    /// Restores a complete x86 XSAVE state previously returned by
    /// [`Stopped::getxstate`].
    // TODO-HUMAN-REVIEW(PR-270): Review complete ptrace XSTATE preservation API.
    #[cfg(target_arch = "x86_64")]
    pub fn setxstate(&self, state: &XState) -> Result<(), Error> {
        let iov = libc::iovec {
            iov_base: state.0.as_ptr() as *mut libc::c_void,
            iov_len: state.0.len(),
        };
        unsafe {
            syscalls::syscall!(
                Sysno::ptrace,
                libc::PTRACE_SETREGSET,
                self.0.as_raw(),
                NT_X86_XSTATE,
                &iov as *const _
            )
        }
        .map_err(|err| self.map_err(err))?;
        Ok(())
    }

    /// Resumes the process and transitions it back to a running state.
    pub fn resume<T: Into<Option<Signal>>>(self, sig: T) -> Result<Running, Error> {
        let signal = sig.into();
        #[cfg(feature = "notifier")]
        if self.1.owns_claimed_exit_stop {
            let result =
                self.1
                    .event()
                    .continue_claimed_exit_stop(
                        self.0,
                        self.logical_stop_id(),
                        self.1.physical_status,
                        signal,
                    );
            result.map_err(|err| self.map_nix_err(err))?;
            return Ok(Running::from_token(self.0, self.1.into_running()));
        }
        let observed = self.begin_physical_resume(PhysicalResumeOperation::Continue, signal);
        let result = ptrace::cont(self.0.into(), signal);
        self.finish_physical_resume(observed, &result);
        result.map_err(|err| self.map_nix_err(err))?;
        Ok(Running::from_token(self.0, self.1.into_running()))
    }

    /// Performs one ordinary continuation while preserving its exact observer
    /// attempt and raw errno for generation-bound stop-resolution logic.
    ///
    /// This rejects claimed exit stops; their continuation protocol remains
    /// exclusively owned by [`Stopped::resume`].
    #[cfg(feature = "notifier")]
    pub fn resume_with_physical_attempt<T: Into<Option<Signal>>>(
        self,
        sig: T,
    ) -> Result<(Running, PhysicalResumeAttempt), PhysicalResumeFailure> {
        if self.1.owns_claimed_exit_stop {
            return Err(PhysicalResumeFailure {
                error: Errno::EINVAL,
                attempt: None,
            });
        }
        let signal = sig.into();
        let Some(observed) = self.begin_physical_resume(PhysicalResumeOperation::Continue, signal)
        else {
            return Err(PhysicalResumeFailure {
                error: Errno::EPROTO,
                attempt: None,
            });
        };
        let attempt = observed.1;
        let result = ptrace::cont(self.0.into(), signal);
        self.finish_physical_resume(Some(observed), &result);
        match result {
            Ok(()) => Ok((Running::from_token(self.0, self.1.into_running()), attempt)),
            Err(error) => Err(PhysicalResumeFailure {
                error: Errno::new(error as i32),
                attempt: Some(attempt),
            }),
        }
    }

    /// Advances the execution of the process by a single step optionally
    /// delivering a signal specified by `sig`.
    pub fn step<T: Into<Option<Signal>>>(self, sig: T) -> Result<Running, Error> {
        #[cfg(feature = "notifier")]
        if self.1.owns_claimed_exit_stop {
            return Err(Error::Errno(Errno::EINVAL));
        }
        let signal = sig.into();
        let observed = self.begin_physical_resume(PhysicalResumeOperation::SingleStep, signal);
        let result = ptrace::step(self.0.into(), signal);
        self.finish_physical_resume(observed, &result);
        result.map_err(|err| self.map_nix_err(err))?;
        Ok(Running::from_token(self.0, self.1.into_running()))
    }

    /// Like `step`, but arranges for the tracee to be stopped at the next
    /// entry to or exit from a system call.
    pub fn syscall<T: Into<Option<Signal>>>(self, sig: T) -> Result<Running, Error> {
        #[cfg(feature = "notifier")]
        if self.1.owns_claimed_exit_stop {
            return Err(Error::Errno(Errno::EINVAL));
        }
        let signal = sig.into();
        let observed = self.begin_physical_resume(PhysicalResumeOperation::Syscall, signal);
        let result = ptrace::syscall(self.0.into(), signal);
        self.finish_physical_resume(observed, &result);
        result.map_err(|err| self.map_nix_err(err))?;
        Ok(Running::from_token(self.0, self.1.into_running()))
    }

    /// Sets the syscall to be executed. Only available on `aarch64`.
    ///
    /// Normally, on x86_64, the register `orig_rax` should be set instead to
    /// modify the syscall number, which typically involves 3 ptrace calls:
    ///  1. getregs to get the current registers.
    ///  2. setregs to change `orig_rax` to set the syscall number.
    ///  3. setregs again to restore the original registers after the syscall
    ///     has been executed.
    ///
    /// `set_syscall` on `aarch64` has the advantage of only requiring a single
    /// ptrace call.
    #[cfg(target_arch = "aarch64")]
    pub fn set_syscall(&self, nr: i32) -> Result<(), Error> {
        const NT_ARM_SYSTEM_CALL: i32 = 0x404;
        self.setregset(NT_ARM_SYSTEM_CALL, &nr)
    }

    /// Gets info about the signal that caused the process to be stopped.
    pub fn getsiginfo(&self) -> Result<libc::siginfo_t, Error> {
        ptrace::getsiginfo(self.0.into()).map_err(|err| self.map_nix_err(err))
    }

    /// Sets info about the singal that caused the process to be stopped.
    pub fn setsiginfo(&self, siginfo: &libc::siginfo_t) -> Result<(), Error> {
        ptrace::setsiginfo(self.0.into(), siginfo).map_err(|err| self.map_nix_err(err))
    }

    /// Gets the tracee's exact Linux kernel signal-mask bytes.
    ///
    /// This borrows the current typed stop: it neither resumes the tracee nor
    /// creates another owner of the stopped state.
    pub fn getsigmask(&self) -> Result<PtraceSigmask, Error> {
        let mut bytes = MaybeUninit::<[u8; LINUX_KERNEL_SIGSET_SIZE]>::uninit();
        unsafe {
            syscalls::syscall!(
                Sysno::ptrace,
                libc::PTRACE_GETSIGMASK,
                self.0.as_raw(),
                LINUX_KERNEL_SIGSET_SIZE,
                bytes.as_mut_ptr()
            )
        }
        .map_err(|err| self.map_err(err))?;

        Ok(PtraceSigmask::from_bytes(unsafe { bytes.assume_init() }))
    }

    /// Sets the tracee's exact Linux kernel signal-mask bytes.
    ///
    /// This borrows the current typed stop and does not resume the tracee.
    /// `PTRACE_SETSIGMASK` reports only whether the write succeeded; callers
    /// that require verified installation must call [`Stopped::getsigmask`]
    /// on this same still-stopped value and compare the returned mask.
    pub fn setsigmask(&self, mask: &PtraceSigmask) -> Result<(), Error> {
        unsafe {
            syscalls::syscall!(
                Sysno::ptrace,
                libc::PTRACE_SETSIGMASK,
                self.0.as_raw(),
                LINUX_KERNEL_SIGSET_SIZE,
                mask.as_bytes().as_ptr()
            )
        }
        .map_err(|err| self.map_err(err))?;

        Ok(())
    }

    /// Like `getsiginfo`, but do not remove the signal info from an internal
    /// queue.
    pub fn peeksiginfo<T: Into<Option<PeekSigInfoFlags>>>(
        &self,
        flags: T,
    ) -> Result<Vec<libc::siginfo_t>, Error> {
        const SIGNAL_MAX: usize = 8 * core::mem::size_of::<u64>();
        let mut data = MaybeUninit::<[libc::siginfo_t; SIGNAL_MAX]>::zeroed();
        let mut siginfo_args = ptrace_peeksiginfo_args {
            off: 0,
            flags: flags.into().map_or(0, |x| x.bits()),
            nr: SIGNAL_MAX as u32,
        };
        let count = Errno::result(unsafe {
            libc::ptrace(
                libc::PTRACE_PEEKSIGINFO,
                self.0.as_raw(),
                &mut siginfo_args as *mut _,
                data.as_mut_ptr() as *const _ as *const libc::c_void,
            )
        })
        .map_err(|err| self.map_err(err))?;
        Ok(unsafe { data.assume_init() }[0..count as usize].to_vec())
    }

    /// Retrieve a message about the ptrace event that just happened.
    ///
    /// It shouldn't be necessary to call this in most cases because `Event`
    /// provides the necessary context for certain ptrace events.
    pub fn getevent(&self) -> Result<i64, Error> {
        ptrace::getevent(self.0.into()).map_err(|err| self.map_nix_err(err))
    }

    /// Detaches from and then resumes the stopped tracee.
    pub fn detach<T: Into<Option<Signal>>>(self, sig: T) -> Result<Running, Error> {
        #[cfg(feature = "notifier")]
        if self.1.owns_claimed_exit_stop {
            return Err(Error::Errno(Errno::EINVAL));
        }
        let signal = sig.into();
        let observed = self.begin_physical_resume(PhysicalResumeOperation::Detach, signal);
        let result = ptrace::detach(self.0.into(), signal);
        self.finish_physical_resume(observed, &result);
        result.map_err(|err| self.map_nix_err(err))?;
        Ok(Running::from_token(self.0, self.1.into_running()))
    }
}

/// Waits for any child processes to change state, blocking until the next event.
/// This is equivalent to `waitpid(-1)`.
pub fn wait_all() -> Result<Option<Wait>, Error> {
    let result = wait(IdType::All, WaitPidFlag::WEXITED | WaitPidFlag::WSTOPPED)
        .map_err(Error::from)
        .and_then(|status| {
            // Unwrap is OK because the process cannot be left in a running
            // state without WNOHANG.
            Wait::try_from(status.unwrap())
        });

    match result {
        Ok(state) => Ok(Some(state)),
        Err(Error::Errno(Errno::ECHILD)) => {
            // waitpid(-1) only returns ECHILD when there are no more children
            // to wait for. Returning `None` here makes it easy to write a while
            // loop that terminates when there are no more children left.
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// Like `wait_all`, but immediately returns `Ok(None)` if no state transition
/// will occur.
///
/// This is the non-blocking version of `wait_all`.
pub fn try_wait_all() -> Result<Option<Wait>, Error> {
    wait(
        IdType::All,
        WaitPidFlag::WEXITED | WaitPidFlag::WSTOPPED | WaitPidFlag::WNOHANG,
    )?
    .map(Wait::try_from)
    .transpose()
}

/// Waits for any child in a process group to change state, blocking until the
/// next event.
pub fn wait_group(pid: Pid) -> Result<Option<Wait>, Error> {
    let result = wait(
        IdType::Pgid(pid.into()),
        WaitPidFlag::WEXITED | WaitPidFlag::WSTOPPED,
    )
    .map_err(Error::from)
    .and_then(|status| {
        // Unwrap is OK because the process cannot be left in a running
        // state without WNOHANG.
        Wait::try_from(status.unwrap())
    });

    match result {
        Ok(state) => Ok(Some(state)),
        Err(Error::Errno(Errno::ECHILD)) => {
            // This only returns ECHILD when there are no more children to wait
            // for. Returning `None` here makes it easy to write a while loop
            // that terminates when there are no more children left.
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// Blocks until a state change is ready to consume, but does not consume it.
/// Returns the pid that has the pending state change. Returns `Ok(None)` if
/// there are no child processes to wait on.
///
/// This is useful for deciding which processes to consume events for.
///
/// # Examples
///
/// ```ignore
/// while let Some(process) = peek_all()? {
///     match process.wait()? {
///         Wait::Stopped(tracee, _event) => {
///             tracee.resume(None)?;
///         }
///         Wait::Exited(pid, exit_status) => {
///             println!("pid {} exited ({})", pid, exit_status);
///         }
///     }
/// }
/// ```
pub fn peek_all() -> Result<Option<Running>, Errno> {
    let result = wait(
        IdType::All,
        WaitPidFlag::WEXITED | WaitPidFlag::WSTOPPED | WaitPidFlag::WNOWAIT,
    )
    .map(|state| {
        // Unwrap is OK because the process cannot be in a running state without
        // WNOHANG.
        state.unwrap()
    });

    match result {
        Ok(status) => Ok(status.pid().map(|pid| Running::new(pid.into()))),
        Err(Errno::ECHILD) => {
            // waitpid(-1) only returns ECHILD when there are no more children
            // to wait for. Returning `None` here makes it easy to write a while
            // loop that terminates when there are no more children left.
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// Returns a process that is ready to change state. If there are no child
/// processes ready to change, returns immediately.
///
/// This is the non-blocking version of `peek_all`.
pub fn try_peek_all() -> Result<Option<Running>, Errno> {
    let next = wait(
        IdType::All,
        WaitPidFlag::WEXITED | WaitPidFlag::WSTOPPED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
    )?;

    Ok(next.and_then(|state| state.pid().map(|pid| Running::new(pid.into()))))
}

/// A running child.
#[derive(Debug, Hash, Eq, PartialEq)]
pub struct Running(Pid, TraceeToken);

impl Running {
    /// Creates a new running process. This is generally the entry point for a
    /// new process as soon as it is created.
    pub fn new(pid: Pid) -> Self {
        Self::from_token(pid, TraceeToken::new())
    }

    /// Creates the original controller-spawned tracee from its one-shot
    /// atomic pidfd launch capability.
    ///
    /// Generic, adopted, forked, and `CLONE_PARENT` tracees cannot construct
    /// this value because they do not own a [`ControllerSpawnToken`].
    #[cfg(feature = "notifier")]
    pub fn from_controller_launch(token: ControllerSpawnToken) -> Self {
        let pid = token.child();
        Self::from_token(
            pid,
            TraceeToken::from_controller_launch(token),
        )
    }

    fn from_token(pid: Pid, token: TraceeToken) -> Self {
        Self(pid, token)
    }

    fn from_current_or_new(pid: Pid) -> Result<Self, Errno> {
        Ok(Self::from_token(pid, TraceeToken::current_or_new(pid)?))
    }

    #[cfg(feature = "notifier")]
    fn token(&self) -> &TraceeToken {
        &self.1
    }

    /// Attaches to a running process. The process becomes a tracee and a SIGSTOP
    /// is sent to it. By the time this function ends, the tracee may not yet
    /// have actually stopped. Thus, the tracee is still considered to be in a
    /// running state and needs to be waited upon to observe the SIGSTOP.
    pub fn attach(pid: Pid) -> Result<Self, Errno> {
        ptrace::attach(pid.into()).map_err(|err| Errno::new(err as i32))?;
        Ok(Self::new(pid))
    }

    /// Similar to attach, but does not stop the process. This also affects the
    /// events that are later delivered. Upon clone, fork, or vfork, an
    /// `Event::Stop` is delivered instead of `Event::Signal(Signal::SIGSTOP)`.
    ///
    /// Unlike other modes, a seized process can also accept interrupts.
    pub fn seize(pid: Pid, options: Options) -> Result<Self, Errno> {
        ptrace::seize(pid.into(), options).map_err(|err| Errno::new(err as i32))?;
        Ok(Self::new(pid))
    }

    /// Interrupts the running process, even if it is in the middle of a syscall.
    /// The next time the process is waited on, the process transitions to a
    /// stopped state and `Event::Stop` is returned.
    ///
    /// # Limitations
    ///
    /// This only works for processes being traced via `Running::seize`.
    pub fn interrupt(&self) -> Result<(), Errno> {
        // nix doesn't provide `ptrace::interrupt` yet, so we need to roll our
        // own.
        Errno::result(unsafe {
            libc::ptrace(
                libc::PTRACE_INTERRUPT,
                self.0.as_raw(),
                std::ptr::null_mut::<libc::c_void>(),
                std::ptr::null_mut::<libc::c_void>(),
            )
        })
        .map(drop)
    }

    /// Returns the pid of the running process.
    pub fn pid(&self) -> Pid {
        self.0
    }

    /// Attaches a bounded observer before this generation's first wait owner.
    #[cfg(feature = "notifier")]
    pub fn attach_physical_event_observer(
        &self,
        observer: &PhysicalEventObserver,
    ) -> Result<(), PhysicalObserverAttachError> {
        self.1.event().attach_physical_observer(observer)
    }

    /// Attaches an observer and consumes this generation's exact
    /// controller-spawn capability to mint the sole original-root launch
    /// proof.
    #[cfg(feature = "notifier")]
    pub fn attach_original_root_physical_observer(
        &self,
        observer: &PhysicalEventObserver,
    ) -> Result<OriginalRootLaunchToken, PhysicalObserverAttachError> {
        self.1
            .event()
            .attach_original_root_physical_observer(self.0, observer)
    }

    /// Returns the physical observer attached to this tracee generation.
    #[cfg(feature = "notifier")]
    pub fn physical_event_observer(&self) -> Option<PhysicalEventObserver> {
        self.1.event().physical_observer()
    }

    /// Returns this immutable notifier generation's diagnostic identity.
    #[cfg(feature = "notifier")]
    pub fn physical_event_generation(&self) -> PhysicalEventGenerationId {
        self.1.event().physical_generation()
    }

    /// Grants this pre-registration generation sole authority to consume the
    /// controller-launched root thread group's `WCONTINUED` state.
    ///
    #[cfg(feature = "notifier")]
    pub fn prepare_original_root_continued_status_authority(
        &self,
        launch: OriginalRootLaunchToken,
    ) -> Result<OriginalRootStartup, OriginalRootStartupError> {
        self.1
            .event()
            .prepare_original_root_startup(self.0, launch)
    }

    /// Exact-pidfd cleanup for a controller launch which failed after clone
    /// but before startup authority preparation.
    ///
    /// A cleanup failure returns a typed non-Clone continuation retaining the
    /// same Event and pidfd; this method never retries or reopens the PID.
    #[cfg(feature = "notifier")]
    pub fn cleanup_failed_controller_launch(
        &self,
        cause: Errno,
    ) -> Result<(), OriginalRootStartupError> {
        self.1
            .event()
            .cleanup_failed_controller_launch(self.0, cause)
    }

    /// Terminates and reaps this unregistered original-root generation using
    /// one exact pidfd. No reusable numeric-PID kill or polling wait is used.
    ///
    /// # Safety
    ///
    /// The caller must still own the controller-spawned child before notifier
    /// registration and must not race another waiter or cleanup owner.
    #[cfg(feature = "notifier")]
    pub unsafe fn terminate_unregistered_original_root(
        &self,
        cause: Errno,
    ) -> Result<(), Errno> {
        self.1
            .event()
            .terminate_unregistered_original_root(self.0, cause)
    }

    /// Returns an unregistered cleanup handle for the exact Event generation.
    ///
    /// # Safety
    ///
    /// The caller must retain sole pre-registration wait ownership until this
    /// handle is either registered or used for unstarted cleanup.
    #[cfg(feature = "notifier")]
    pub unsafe fn unregistered_terminal_cleanup(&self) -> TerminalCleanup {
        TerminalCleanup::new_unregistered(self.0, &self.1)
    }

    /// Converts an externally observed stop into the stopped capability for
    /// this exact generation.
    ///
    /// The caller must prove that `status` came from a successful kernel wait
    /// for this unreaped child and that this [`Running`] value is the sole
    /// typed capability for it. The observer must already have been attached
    /// before that wait was attempted.
    #[cfg(feature = "notifier")]
    pub fn into_observed_stopped_unchecked(
        self,
        observer: &PhysicalEventObserver,
        status: PhysicalStatusId,
    ) -> Result<Stopped, PhysicalObserverAttachError> {
        self.1.event().attach_physical_observer(observer)?;
        let generation = self.1.event().physical_generation();
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::DirectStopped,
        );
        Ok(Stopped::from_token(
            self.0,
            self.1.into_observed_stopped(status),
        ))
    }

    /// Blocks until a state change occurs. This may transition the process to
    /// either a stopped state or exited state, but never a running state.
    pub fn wait(self) -> Result<Wait, Error> {
        let pid = self.0;
        let token = self.1;
        #[cfg(feature = "notifier")]
        {
            notifier::wait_sync(pid, token)
        }
        #[cfg(not(feature = "notifier"))]
        {
            wait(
                IdType::Pid(pid.into()),
                WaitPidFlag::WEXITED | WaitPidFlag::WSTOPPED,
            )
            .map_err(Error::from)
            .and_then(|status| {
                // Unwrap is OK because the process cannot be in a running state without
                // WNOHANG.
                Wait::from_wait_status_with_token(status.unwrap(), token)
            })
        }
    }

    /// Like `wait`, but filters out events we don't care about by resuming the
    /// tracee when encountering them. This is useful for skipping past spurious
    /// events until a point we know the tracee must stop.
    #[cfg(feature = "notifier")]
    pub async fn wait_until<F>(mut self, mut pred: F) -> Result<Wait, Error>
    where
        F: FnMut(&Event) -> bool,
    {
        loop {
            match self.next_state().await? {
                Wait::Stopped(stopped, event) => {
                    if pred(&event) {
                        break Ok(Wait::Stopped(stopped, event));
                    } else if let Event::Signal(sig) = event {
                        self = stopped.resume(Some(sig))?;
                    } else {
                        self = stopped.resume(None)?;
                    }
                }
                task => break Ok(task),
            }
        }
    }

    /// Waits until we receive a specific stop signal. Useful for skipping past
    /// spurious signals.
    #[cfg(feature = "notifier")]
    pub async fn wait_for_signal(self, sig: Signal) -> Result<Wait, Error> {
        self.wait_until(|event| event == &Event::Signal(sig)).await
    }

    /// Waits for the next exit stop to occur. This is received asynchronously
    /// regardless of what the process was doing at the time. This is useful for
    /// canceling futures when a process enters a `PTRACE_EVENT_EXIT` (such as
    /// when one thread calls `exit_group` and causes all other threads to
    /// suddenly exit).
    ///
    /// Exactly one future for this immutable tracee generation can claim the
    /// exit stop and return a [`Stopped`] capability. Duplicate or re-polled
    /// futures return [`Errno::EALREADY`]. An unclaimed capability also expires
    /// before terminal publication or cancellation cleanup advances the
    /// tracee.
    #[cfg(feature = "notifier")]
    pub fn exit_event(&self) -> notifier::ExitFuture {
        notifier::ExitFuture::new(self.0, &self.1)
    }

    /// Registers this process with the async notifier and returns a bounded
    /// synchronous acknowledgment handle for terminal cleanup.
    #[cfg(feature = "notifier")]
    pub fn terminal_cleanup(&self) -> TerminalCleanup {
        TerminalCleanup::new(self.0, &self.1)
    }

    /// Like `wait`, but wait asynchronously for the next state change.
    ///
    /// NOTE: This call should not be mixed with [`Running::wait`]!! Once
    /// [`Running::next_state`] is called once, [`Running::wait`] should never
    /// be called again for that PID. This is because a notifier thread takes
    /// over and calls `wait` in a continuous loop.
    #[cfg(feature = "notifier")]
    pub async fn next_state(self) -> Result<Wait, Error> {
        notifier::WaitFuture::new(self).await
    }
}

/// A process that is no longer running, but hasn't yet fully exited. The only
/// thing zombie can do is exit.
#[derive(Debug, Hash, Eq, PartialEq)]
pub struct Zombie(Running);

impl Zombie {
    /// Creates a new instance.
    fn from_token(pid: Pid, token: TraceeToken) -> Self {
        Zombie(Running::from_token(pid, token))
    }

    /// Returns the PID of the zombie.
    pub fn pid(&self) -> Pid {
        self.0.pid()
    }

    /// Reaps the zombie by waiting for it to fully exit.
    #[cfg(feature = "notifier")]
    pub async fn reap(self) -> Result<ExitStatus, Error> {
        // The tracee may not be fully dead yet. It is still possible for it to
        // still enter an `Event::Exit` state by waiting on it. For more info,
        // see the "BUGS" section in `man 2 ptrace`.
        let mut next_state = self.0.next_state().await;

        loop {
            match next_state {
                Ok(wait) => match wait {
                    Wait::Stopped(stopped, event) => {
                        if let Event::Exit = event {
                            next_state = match stopped.resume(None) {
                                Ok(task) => task.next_state().await,
                                Err(err) => Err(err),
                            };
                        } else {
                            panic!("Task {:?} unexpected stop event {:?}", stopped, event)
                        }
                    }
                    Wait::Exited(_pid, exit_status) => break Ok(exit_status),
                },
                Err(Error::Died(zombie)) => next_state = zombie.0.next_state().await,
                Err(error) => break Err(error),
            }
        }
    }
}

impl fmt::Display for Zombie {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.pid())
    }
}

/// Sets up this process to be traced by its parent and raises a SIGSTOP.
pub fn traceme_and_stop() -> Result<(), Errno> {
    traceme()?;
    stop_for_tracer()?;
    Ok(())
}

/// Establishes `PTRACE_TRACEME` ownership without stopping the child yet.
pub fn traceme() -> Result<(), Errno> {
    ptrace::traceme().map_err(|error| Errno::new(error as i32))
}

/// Raises the initial SIGSTOP after spawn's error pipe has been closed.
///
/// Call this only after a successful [`traceme`] in the same pre-exec child.
pub fn stop_for_tracer() -> Result<(), Errno> {
    nix::sys::signal::raise(Signal::SIGSTOP).map_err(|error| Errno::new(error as i32))
}

/// These tests are meant to test this API but also to show how ptrace works.
#[cfg(test)]
mod test {
    use std::io;
    use std::os::fd::BorrowedFd;
    use std::thread;

    use nix::sys::signal;
    use nix::sys::signal::Signal;
    use nix::unistd::ForkResult;
    use nix::unistd::fork;
    // Make sure tokio is referenced in all configurations.
    use tokio as _;

    use super::*;

    #[test]
    fn ptrace_sigmask_preserves_exact_kernel_bytes() {
        let bytes = [0x00, 0x01, 0x7f, 0x80, 0xaa, 0x55, 0xfe, 0xff];
        let mask = PtraceSigmask::from_bytes(bytes);

        assert_eq!(LINUX_KERNEL_SIGSET_SIZE, 8);
        assert_eq!(core::mem::size_of::<PtraceSigmask>(), 8);
        assert_eq!(mask.as_bytes(), &bytes);
        assert_eq!(mask.into_bytes(), bytes);
    }

    #[test]
    fn ptrace_sigmask_rejects_non_kernel_lengths() {
        assert!(PtraceSigmask::try_from(&[0_u8; 7][..]).is_err());
        assert!(PtraceSigmask::try_from(&[0_u8; 9][..]).is_err());
        let exact = PtraceSigmask::try_from(&[0x5a_u8; LINUX_KERNEL_SIGSET_SIZE][..])
            .expect("exact kernel sigset length");
        assert_eq!(
            exact,
            PtraceSigmask::from_bytes([0x5a; LINUX_KERNEL_SIGSET_SIZE])
        );
    }

    #[test]
    fn ptrace_sigmask_propagates_stopped_errors() {
        // Linux's pid_max is lower than i32::MAX, so no live tracee can own
        // this TID. This exercises the real ptrace error path without a guest.
        let pid = Pid::from_raw(i32::MAX);
        let stopped = Stopped::new_unchecked(pid);
        assert!(matches!(
            stopped.getsigmask(),
            Err(Error::Died(zombie)) if zombie.pid() == pid
        ));

        let mask = PtraceSigmask::from_bytes([0; LINUX_KERNEL_SIGSET_SIZE]);
        assert!(matches!(
            stopped.setsigmask(&mask),
            Err(Error::Died(zombie)) if zombie.pid() == pid
        ));
    }

    #[test]
    fn ptrace_sigmask_live_roundtrip_restores_exact_kernel_bytes() {
        let (_pid, stopped) = trace(|| 0, Options::empty()).expect("spawn stopped sigmask child");
        let roundtrip = (|| -> Result<_, Error> {
            let original = stopped.getsigmask()?;
            let sigusr1_bit = 1_u64 << (Signal::SIGUSR1 as u32 - 1);
            let changed = PtraceSigmask::from_bytes(
                (u64::from_ne_bytes(original.into_bytes()) ^ sigusr1_bit).to_ne_bytes(),
            );
            stopped.setsigmask(&changed)?;
            let installed = stopped.getsigmask()?;
            stopped.setsigmask(&original)?;
            let restored = stopped.getsigmask()?;
            Ok((original, changed, installed, restored))
        })();
        let final_wait = stopped.resume(None).and_then(Running::wait);

        let (original, changed, installed, restored) =
            roundtrip.expect("round-trip live ptrace signal mask");
        assert_ne!(changed, original);
        assert_eq!(
            installed, changed,
            "kernel did not install exact mask bytes"
        );
        assert_eq!(
            restored, original,
            "kernel did not restore exact mask bytes"
        );
        assert_eq!(
            final_wait.expect("reap sigmask child").assume_exited().1,
            ExitStatus::Exited(0)
        );
    }

    #[cfg(feature = "notifier")]
    #[test]
    fn claimed_exit_marker_is_cleared_from_successor_tokens() {
        let event = notifier::EventHandle::new();
        let exit_stop = event.allocate_logical_stop();
        let successor_stop = event.allocate_logical_stop();
        assert!(successor_stop.is_strictly_after(exit_stop));
        assert!(!exit_stop.is_strictly_after(successor_stop));
        assert!(!exit_stop.is_strictly_after(exit_stop));
        let claimed =
            TraceeToken::from_claimed_exit_event(event.clone(), None, exit_stop);
        assert!(claimed.owns_claimed_exit_stop);
        assert!(!claimed.into_running().owns_claimed_exit_stop);
        assert!(!TraceeToken::from_event(event.clone()).owns_claimed_exit_stop);
        assert!(
            !TraceeToken::from_observed_event(event, None, Some(successor_stop))
                .owns_claimed_exit_stop,
            "ordinary successor stop inherited claimed exit ownership"
        );
    }

    #[cfg(feature = "notifier")]
    #[test]
    fn claimed_exit_stop_rejects_non_continue_transitions() {
        let pid = Pid::from_raw(i32::MAX);
        let claimed = || {
            let event = notifier::EventHandle::new();
            let stop_id = event.allocate_logical_stop();
            Stopped::from_token(
                pid,
                TraceeToken::from_claimed_exit_event(event, None, stop_id),
            )
        };

        assert!(matches!(
            claimed().step(None),
            Err(Error::Errno(Errno::EINVAL))
        ));
        assert!(matches!(
            claimed().syscall(None),
            Err(Error::Errno(Errno::EINVAL))
        ));
        assert!(matches!(
            claimed().detach(None),
            Err(Error::Errno(Errno::EINVAL))
        ));
    }

    // Traces a closure in a forked process. The forked process starts in a
    // stopped state so that ptrace options may be set.
    fn trace<F>(f: F, options: Options) -> Result<(Pid, Stopped), Error>
    where
        F: FnOnce() -> i32,
    {
        match unsafe { fork() }? {
            ForkResult::Parent { child, .. } => {
                let mut running = Running::seize(child.into(), options)?;

                // Keep consuming events until we reach a SIGSTOP or group stop.
                let stopped = loop {
                    match running.wait()? {
                        Wait::Stopped(stopped, event) => {
                            if event == Event::Signal(Signal::SIGSTOP) || event == Event::Stop {
                                break stopped;
                            } else if let Event::Signal(sig) = event {
                                running = stopped.resume(Some(sig))?;
                            } else {
                                running = stopped.resume(None)?;
                            }
                        }
                        task => panic!("Got unexpected exit: {:?}", task),
                    }
                };

                Ok((stopped.pid(), stopped))
            }
            ForkResult::Child => {
                // Create a new process group so we can wait on this process and
                // every child more efficiently.
                let _ = unsafe { libc::setpgid(0, 0) };

                // Suppress core dumps for testing purposes.
                let limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                let _ = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) };

                // PTRACE_SEIZE is inherently racey, so we stop the child
                // process here.
                signal::raise(Signal::SIGSTOP).unwrap();

                // Run the child when the process is resumed.
                let exit_code = f();

                // Note: We can't use the normal exit function here because we
                // don't want to call atexit handlers since `execve` was never
                // called. `_exit` diverges (`-> !`), so there is nothing to bind.
                unsafe { ::libc::_exit(exit_code) };
            }
        }
    }

    #[cfg(feature = "notifier")]
    fn stopped_with_observer(
        observer: &PhysicalEventObserver,
        pid: Pid,
        status: PhysicalStatusId,
    ) -> Stopped {
        let mut token = TraceeToken::new();
        token
            .event()
            .attach_physical_observer(observer)
            .expect("attach resume disposition observer");
        token.physical_status = Some(status);
        Stopped::from_token(pid, token)
    }

    #[cfg(feature = "notifier")]
    #[test]
    fn direct_failed_resume_keeps_ordinary_disposition() {
        let pid = Pid::from_raw(i32::MAX - 43);
        let status = PhysicalStatusId::from_raw(19).expect("nonzero physical status");

        let ordinary = PhysicalEventObserver::new(PhysicalEventObserverConfig::new(8, 4))
            .expect("create ordinary failed-resume observer");
        let stopped = stopped_with_observer(&ordinary, pid, status);
        let attempt = stopped
            .begin_physical_resume(PhysicalResumeOperation::Continue, None)
            .expect("attached observer produces a resume attempt");
        stopped.finish_physical_resume(attempt.into(), &Err(nix::errno::Errno::ESRCH));
        ordinary.close();
        assert!(ordinary.snapshot().records().iter().any(|record| matches!(
            record.kind(),
            PhysicalEventRecordKind::StatusDisposition {
                status: observed,
                disposition: PhysicalStatusDisposition::OrdinaryHandled,
            } if observed == status
        )));
    }

    #[test]
    fn basic() -> Result<(), Box<dyn std::error::Error + 'static>> {
        // Do nothing but exit.
        let (pid, tracee) = trace(|| 42, Options::empty())?;
        assert_eq!(
            tracee.resume(None)?.wait()?,
            Wait::Exited(pid, ExitStatus::Exited(42))
        );

        Ok(())
    }

    #[test]
    fn stop_on_exit() -> Result<(), Box<dyn std::error::Error + 'static>> {
        let (pid, tracee) = trace(
            || 42,
            Options::PTRACE_O_EXITKILL | Options::PTRACE_O_TRACEEXIT,
        )?;

        let running = tracee.resume(None)?;
        let (stopped, event) = running.wait()?.assume_stopped();

        // The tracee has stopped just before exiting. Resuming or detaching now
        // will let the process exit.
        assert_eq!(event, Event::Exit);

        assert_eq!(
            stopped.resume(None)?.wait()?,
            Wait::Exited(pid, ExitStatus::Exited(42))
        );

        Ok(())
    }

    #[test]
    #[cfg(not(sanitized))]
    fn serialized_threads() -> Result<(), Box<dyn std::error::Error + 'static>> {
        const THREAD_COUNT: usize = 8;

        let (pid, tracee) = trace(
            move || {
                // Create a handful of threads that do nothing but exit.
                let threads = (0..THREAD_COUNT)
                    .map(|i| thread::spawn(move || i))
                    .collect::<Vec<_>>();

                for t in threads {
                    t.join().unwrap();
                }

                42
            },
            Options::PTRACE_O_EXITKILL
                | Options::PTRACE_O_TRACEEXIT
                | ptrace::Options::PTRACE_O_TRACECLONE,
        )?;

        let mut parent = tracee.resume(None)?;

        // We should observe threads getting created.
        for _ in 0..THREAD_COUNT {
            let (stopped, event) = parent.wait()?.assume_stopped();

            let child = match event {
                Event::NewChild(ChildOp::Clone, child) => child,
                e => panic!("Expected clone event, got {:?}", e),
            };

            // Should be at a group stop.
            let (child, event) = child.wait()?.assume_stopped();
            assert_eq!(event, Event::Stop);

            // Resume the child.
            let child = child.resume(None)?;

            // Wait for it to exit.
            let (child, event) = child.wait()?.assume_stopped();
            assert_eq!(event, Event::Exit);

            // Resume one last time to let it fully exit.
            let (_child_pid, exit_status) = child.resume(None)?.wait()?.assume_exited();
            assert_eq!(exit_status, ExitStatus::Exited(0));

            // Resume the parent.
            parent = stopped.resume(None)?;
        }

        // ptrace stop just before fully exiting.
        let (parent, event) = parent.wait()?.assume_stopped();
        assert_eq!(event, Event::Exit);

        // Fully exited.
        let parent = parent.resume(None)?;
        assert_eq!(parent.wait()?, Wait::Exited(pid, ExitStatus::Exited(42)));

        Ok(())
    }

    #[cfg(not(sanitized))]
    fn group_exit(thread_count: usize) -> Result<(), Box<dyn std::error::Error + 'static>> {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        let (parent_pid, tracee) = trace(
            move || {
                let counter = Arc::new(AtomicUsize::new(0));

                // Create a handful of threads that sleep forever.
                let _threads = (0..thread_count)
                    .map(|_i| {
                        let counter = counter.clone();

                        thread::spawn(move || {
                            counter.fetch_add(1, Ordering::Relaxed);
                            thread::sleep(Duration::from_mins(1));
                        })
                    })
                    .collect::<Vec<_>>();

                // Wait for each of the threads to actually get initialized.
                while counter.load(Ordering::Relaxed) != thread_count {
                    thread::yield_now();
                }

                // All threads should be alive at this point. SYS_exit_group
                // should force all threads to exit.
                let _ = unsafe { libc::syscall(libc::SYS_exit_group, 42) };

                unreachable!()
            },
            Options::PTRACE_O_EXITKILL
                | Options::PTRACE_O_TRACEEXIT
                | ptrace::Options::PTRACE_O_TRACECLONE,
        )?;

        tracee.resume(None)?;

        let mut exited = Vec::new();

        // Keep consuming events until everything has exited.
        while let Some(wait) = wait_group(parent_pid)? {
            match wait {
                Wait::Stopped(tracee, _event) => {
                    tracee.resume(None)?;
                }
                Wait::Exited(pid, exit_status) => {
                    exited.push((pid, exit_status));
                }
            }
        }

        // The parent should have exited last.
        assert_eq!(exited.pop(), Some((parent_pid, ExitStatus::Exited(42))));

        // The only things left should be the threads that were spawned.
        assert_eq!(exited.len(), thread_count);

        // All others should have exited with the same exit status.
        for (_pid, exit_status) in exited {
            assert_eq!(exit_status, ExitStatus::Exited(42));
        }

        Ok(())
    }

    /// Tests that we receive an exit for all threads in the right order even
    /// when the main thread calls `exit_group`.
    #[test]
    #[cfg(not(sanitized))]
    fn group_exit_stress() {
        // Test a variety of thread counts. Super-high thread counts makes
        // ptrace very slow, so we keep this to a relatively low number.
        for i in 0..100 {
            group_exit(i / 2).unwrap();
        }
    }

    /// Tests that trying to trace from another thread does not work.
    #[test]
    fn trace_from_another_thread() -> Result<(), Box<dyn std::error::Error + 'static>> {
        let (pid, tracee) = trace(|| 42, Options::empty()).unwrap();

        // Try resuming from another thread, which should fail. The process
        // didn't actually die; this is just how ESRCH is interpreted.
        let error = thread::spawn(move || tracee.resume(None))
            .join()
            .unwrap()
            .unwrap_err();
        assert!(matches!(error, Error::Died(zombie) if zombie.pid() == pid));

        assert_eq!(
            Stopped::new_unchecked(pid).resume(None)?.wait()?,
            Wait::Exited(pid, ExitStatus::Exited(42))
        );

        Ok(())
    }

    #[test]
    fn trace_killed_by_signal() -> Result<(), Box<dyn std::error::Error + 'static>> {
        let (pid, tracee) = trace(
            || {
                signal::raise(Signal::SIGILL).unwrap();
                unreachable!()
            },
            Options::PTRACE_O_EXITKILL,
        )?;

        let running = tracee.resume(None)?;

        let (stopped, event) = running.wait()?.assume_stopped();

        // The tracee has stopped just before exiting. Resuming or detaching now
        // will let the process exit.
        assert_eq!(event, Event::Signal(Signal::SIGILL));

        assert_eq!(
            stopped.resume(Some(Signal::SIGILL))?.wait()?,
            Wait::Exited(pid, ExitStatus::Signaled(Signal::SIGILL, true))
        );

        Ok(())
    }

    #[cfg(feature = "notifier")]
    #[cfg(not(sanitized))]
    #[tokio::test]
    async fn notifier_basic() -> Result<(), Box<dyn std::error::Error + 'static>> {
        let (pid, tracee) = trace(|| 42, Options::empty())?;
        assert_eq!(
            tracee.resume(None)?.next_state().await?,
            Wait::Exited(pid, ExitStatus::Exited(42))
        );

        Ok(())
    }

    #[cfg(feature = "notifier")]
    #[cfg(not(sanitized))]
    #[tokio::test]
    async fn notifier_generation_preserves_exit_and_signal_status()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pid, tracee) = trace(|| 42, Options::empty())?;
        let token = tracee.1.clone();
        let stale_running = Running::from_token(pid, token.clone());
        let stale_zombie = Zombie::from_token(pid, token);
        let expected = ExitStatus::Exited(42);

        assert_eq!(
            tracee.resume(None)?.next_state().await?,
            Wait::Exited(pid, expected)
        );
        assert_eq!(
            stale_running.next_state().await?,
            Wait::Exited(pid, expected),
            "old Running rebound after terminal registry removal"
        );
        assert_eq!(stale_zombie.reap().await?, expected);
        assert_eq!(
            Running::new(pid).next_state().await,
            Err(Error::Errno(Errno::ECHILD)),
            "fresh generation inherited stale terminal status"
        );

        let (pid, tracee) = trace(
            || {
                signal::raise(Signal::SIGILL).unwrap();
                unreachable!()
            },
            Options::PTRACE_O_EXITKILL,
        )?;
        let token = tracee.1.clone();
        let stale_running = Running::from_token(pid, token.clone());
        let stale_zombie = Zombie::from_token(pid, token);
        let expected = ExitStatus::Signaled(Signal::SIGILL, true);
        let (stopped, event) = tracee.resume(None)?.next_state().await?.assume_stopped();
        assert_eq!(event, Event::Signal(Signal::SIGILL));

        assert_eq!(
            stopped.resume(Some(Signal::SIGILL))?.next_state().await?,
            Wait::Exited(pid, expected)
        );
        assert_eq!(
            stale_running.next_state().await?,
            Wait::Exited(pid, expected),
            "old Running lost the terminating signal"
        );
        assert_eq!(stale_zombie.reap().await?, expected);
        assert_eq!(
            Running::new(pid).next_state().await,
            Err(Error::Errno(Errno::ECHILD))
        );

        Ok(())
    }

    #[cfg(feature = "notifier")]
    #[cfg(not(sanitized))]
    #[tokio::test(flavor = "current_thread")]
    async fn notifier_clone_parent_decode_error_preserves_fifo_front()
    -> Result<(), Box<dyn std::error::Error>> {
        for injected in [Errno::EMFILE, Errno::EIO] {
            let (pid, tracee) = trace(
                || {
                    let flags = libc::CLONE_PARENT | libc::SIGCHLD;
                    let result = unsafe {
                        libc::syscall(libc::SYS_clone, flags, 0usize, 0usize, 0usize, 0usize)
                    };
                    if result == 0 {
                        unsafe { libc::_exit(0) };
                    }
                    i32::from(result == -1)
                },
                Options::PTRACE_O_EXITKILL | Options::PTRACE_O_TRACEFORK,
            )?;
            let token = tracee.1.clone();
            let running = tracee.resume(None)?;
            let retry = Running::from_token(pid, token);
            let _cleanup = running.terminal_cleanup();

            notifier::inject_capture_error_for_current_thread(injected);
            assert_eq!(
                running.next_state().await,
                Err(Error::Errno(injected)),
                "first CLONE_PARENT decode did not surface the injected capture error"
            );

            let (parent, event) = retry.next_state().await?.assume_stopped();
            let child = match event {
                Event::NewChild(ChildOp::Fork, child) => child,
                event => panic!("retry lost the CLONE_PARENT event: {event:?}"),
            };
            let (child, event) = child.next_state().await?.assume_stopped();
            assert!(matches!(
                event,
                Event::Stop | Event::Signal(Signal::SIGSTOP)
            ));
            assert_eq!(
                child.resume(None)?.next_state().await?.assume_exited().1,
                ExitStatus::Exited(0)
            );
            assert_eq!(
                parent.resume(None)?.next_state().await?.assume_exited().1,
                ExitStatus::Exited(0)
            );
        }
        Ok(())
    }

    #[cfg(feature = "notifier")]
    #[cfg(not(sanitized))]
    #[test]
    fn synchronous_clone_parent_decode_error_preserves_fifo_front()
    -> Result<(), Box<dyn std::error::Error>> {
        for injected in [Errno::EMFILE, Errno::EIO] {
            let (pid, tracee) = trace(
                || {
                    let flags = libc::CLONE_PARENT | libc::SIGCHLD;
                    let result = unsafe {
                        libc::syscall(libc::SYS_clone, flags, 0usize, 0usize, 0usize, 0usize)
                    };
                    if result == 0 {
                        unsafe { libc::_exit(0) };
                    }
                    i32::from(result == -1)
                },
                Options::PTRACE_O_EXITKILL | Options::PTRACE_O_TRACEFORK,
            )?;
            let token = tracee.1.clone();
            let running = tracee.resume(None)?;
            let retry = Running::from_token(pid, token);

            notifier::inject_sync_decode_capture_error(pid, injected);
            assert_eq!(
                running.wait(),
                Err(Error::Errno(injected)),
                "first synchronous CLONE_PARENT decode did not surface the injected error"
            );

            let (parent, event) = retry.wait()?.assume_stopped();
            let child = match event {
                Event::NewChild(ChildOp::Fork, child) => child,
                event => panic!("synchronous retry lost the CLONE_PARENT event: {event:?}"),
            };
            let (child, event) = child.wait()?.assume_stopped();
            assert!(matches!(
                event,
                Event::Stop | Event::Signal(Signal::SIGSTOP)
            ));
            assert_eq!(
                child.resume(None)?.wait()?.assume_exited().1,
                ExitStatus::Exited(0)
            );
            assert_eq!(
                parent.resume(None)?.wait()?.assume_exited().1,
                ExitStatus::Exited(0)
            );
        }
        Ok(())
    }

    #[cfg(feature = "notifier")]
    #[cfg(not(sanitized))]
    #[tokio::test]
    async fn late_waits_after_terminal_reap_return_echild() -> Result<(), Box<dyn std::error::Error>>
    {
        let (pid, tracee) = trace(|| 42, Options::empty())?;
        assert_eq!(
            tracee.resume(None)?.next_state().await?,
            Wait::Exited(pid, ExitStatus::Exited(42))
        );

        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                Running::new(pid).next_state(),
            )
            .await
            .expect("late next_state hung after terminal reap"),
            Err(Error::Errno(Errno::ECHILD))
        );
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                Running::new(pid).exit_event(),
            )
            .await
            .expect("late exit_event hung after terminal reap"),
            Err(Error::Errno(Errno::ECHILD))
        );

        Ok(())
    }

    // kernel_sigset_t used by naked syscall
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct KernelSigset(u64);

    impl From<&[Signal]> for KernelSigset {
        fn from(signals: &[Signal]) -> Self {
            let mut set: u64 = 0;
            for &sig in signals {
                set |= 1u64 << (sig as usize - 1);
            }
            KernelSigset(set)
        }
    }

    #[unsafe(no_mangle)]
    extern "C" fn sigalrm_handler(
        _sig: i32,
        _siginfo: *mut libc::siginfo_t,
        _ucontext: *const libc::c_void,
    ) {
        nix::unistd::write(unsafe { BorrowedFd::borrow_raw(2) }, b"caught SIGALRM!").unwrap();
    }

    #[allow(dead_code)]
    unsafe fn install_sigalrm_handler() -> i32 {
        unsafe {
            let mut sa: libc::sigaction = MaybeUninit::zeroed().assume_init();
            sa.sa_flags = libc::SA_RESTART | libc::SA_SIGINFO | libc::SA_NODEFER;
            sa.sa_sigaction = sigalrm_handler as *const () as _;

            libc::sigaction(libc::SIGALRM, &sa as *const _, std::ptr::null_mut())
        }
    }

    #[allow(dead_code)]
    // unblock signal(s) and set its handler to SIG_DFL
    unsafe fn unblock_signals(signals: &[Signal]) -> io::Result<KernelSigset> {
        unsafe {
            let set = KernelSigset::from(signals);
            let mut oldset = MaybeUninit::<u64>::uninit();

            if libc::syscall(
                libc::SYS_rt_sigprocmask,
                libc::SIG_UNBLOCK,
                &set as *const _,
                oldset.as_mut_ptr(),
                8,
            ) != 0
            {
                Err(io::Error::last_os_error())
            } else {
                Ok(KernelSigset(oldset.assume_init()))
            }
        }
    }

    #[allow(dead_code)]
    unsafe fn block_signals(signals: &[Signal]) -> io::Result<KernelSigset> {
        unsafe {
            let set = KernelSigset::from(signals);
            let mut oldset = MaybeUninit::<u64>::uninit();

            if libc::syscall(
                libc::SYS_rt_sigprocmask,
                libc::SIG_BLOCK,
                &set as *const _,
                oldset.as_mut_ptr(),
                8,
            ) != 0
            {
                Err(io::Error::last_os_error())
            } else {
                Ok(KernelSigset(oldset.assume_init()))
            }
        }
    }

    #[cfg(not(sanitized))]
    #[test]
    fn peeksiginfo_returns_pending_siginfo() -> Result<(), Box<dyn std::error::Error + 'static>> {
        let (parent_pid, tracee) = trace(
            move || {
                let _ = unsafe {
                    block_signals(&[Signal::SIGALRM, Signal::SIGVTALRM, Signal::SIGPROF])
                };
                assert!(signal::raise(Signal::SIGALRM).is_ok());
                assert!(signal::raise(Signal::SIGVTALRM).is_ok());
                assert!(signal::raise(Signal::SIGPROF).is_ok());

                // All threads should be alive at this point. SYS_exit_group
                // should force all threads to exit.
                let _ = unsafe { libc::syscall(libc::SYS_exit_group, 0) };

                unreachable!()
            },
            Options::PTRACE_O_EXITKILL
                | Options::PTRACE_O_TRACEEXIT
                | ptrace::Options::PTRACE_O_TRACECLONE,
        )?;

        tracee.resume(None)?;

        let mut exited = Vec::new();

        // Keep consuming events until everything has exited.
        while let Some(wait) = wait_group(parent_pid)? {
            match wait {
                Wait::Stopped(tracee, Event::Exit) => {
                    let pending: Vec<_> = tracee
                        .peeksiginfo(None)?
                        .iter()
                        .map(|&si| Signal::try_from(si.si_signo).unwrap())
                        .collect();
                    assert_eq!(
                        pending,
                        [Signal::SIGALRM, Signal::SIGVTALRM, Signal::SIGPROF]
                    );
                    // do a second peek here to demostrate peek doesn't
                    // *pop* pending signals.
                    let pending: Vec<_> = tracee
                        .peeksiginfo(None)?
                        .iter()
                        .map(|&si| Signal::try_from(si.si_signo).unwrap())
                        .collect();
                    assert_eq!(
                        pending,
                        [Signal::SIGALRM, Signal::SIGVTALRM, Signal::SIGPROF]
                    );
                    tracee.resume(None)?;
                }
                Wait::Stopped(tracee, _event) => {
                    tracee.resume(None)?;
                }
                Wait::Exited(pid, exit_status) => {
                    exited.push((pid, exit_status));
                }
            }
        }

        // The parent should have exited last
        assert_eq!(exited.pop(), Some((parent_pid, ExitStatus::Exited(0))));

        Ok(())
    }

    #[cfg(not(sanitized))]
    #[test]
    fn getsiginfo_should_success() -> Result<(), Box<dyn std::error::Error + 'static>> {
        let (parent_pid, tracee) = trace(
            move || {
                let _ = unsafe { unblock_signals(&[Signal::SIGALRM]) };
                let _ = unsafe { block_signals(&[Signal::SIGVTALRM, Signal::SIGPROF]) };
                assert_eq!(unsafe { install_sigalrm_handler() }, 0);
                assert!(signal::raise(Signal::SIGALRM).is_ok());

                // All threads should be alive at this point. SYS_exit_group
                // should force all threads to exit.
                let _ = unsafe { libc::syscall(libc::SYS_exit_group, 0) };

                unreachable!()
            },
            Options::PTRACE_O_EXITKILL
                | Options::PTRACE_O_TRACEEXIT
                | ptrace::Options::PTRACE_O_TRACECLONE,
        )?;

        tracee.resume(None)?;

        let mut exited = Vec::new();

        // Keep consuming events until everything has exited.
        while let Some(wait) = wait_group(parent_pid)? {
            match wait {
                Wait::Stopped(tracee, Event::Signal(Signal::SIGALRM)) => {
                    let siginfo = tracee.getsiginfo()?;
                    assert_eq!(siginfo.si_signo, Signal::SIGALRM as i32);
                    tracee.resume(Signal::SIGALRM)?;
                }
                Wait::Stopped(tracee, Event::Signal(other_signal)) => {
                    tracee.resume(other_signal)?;
                }
                Wait::Stopped(tracee, _event) => {
                    tracee.resume(None)?;
                }
                Wait::Exited(pid, exit_status) => {
                    exited.push((pid, exit_status));
                }
            }
        }

        // The parent should have exited last
        assert_eq!(exited.pop(), Some((parent_pid, ExitStatus::Exited(0))));

        Ok(())
    }
}
