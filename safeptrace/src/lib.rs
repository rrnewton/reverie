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
mod regs;
mod waitid;

use core::mem::MaybeUninit;
use std::fmt;

use nix::sys::ptrace;
// Re-exports so that nothing else needs to depend on `nix`.
pub use nix::sys::ptrace::Options;
pub use nix::sys::signal::Signal;
use nix::sys::wait::WaitPidFlag;
use nix::sys::wait::WaitStatus;
pub use reverie_process::ExitStatus;
pub use reverie_process::Pid;
pub use syscalls::Errno;
use syscalls::Sysno;
use thiserror::Error;

#[cfg(feature = "notifier")]
pub use crate::notifier::ParentReap;
#[cfg(feature = "notifier")]
pub use crate::notifier::ProcStatError;
#[cfg(feature = "notifier")]
pub use crate::notifier::StopObservationError;
#[cfg(feature = "notifier")]
pub use crate::notifier::StopObservationSample;
#[cfg(feature = "notifier")]
pub use crate::notifier::StopSiginfo;
#[cfg(feature = "notifier")]
pub use crate::notifier::StoppedObservation;
#[cfg(feature = "notifier")]
pub use crate::notifier::TaskIdentity;
#[cfg(feature = "notifier")]
pub use crate::notifier::TerminalCleanup;
#[cfg(feature = "notifier")]
pub use crate::notifier::source::ControlHold;
#[cfg(feature = "notifier")]
pub use crate::notifier::source::ControlStop;
#[cfg(feature = "notifier")]
pub use crate::notifier::source::SourceAcquisition;
#[cfg(feature = "notifier")]
pub use crate::notifier::source::SourceSignal;
#[cfg(feature = "notifier")]
pub use crate::notifier::source::SourceStop;
pub use crate::regs::*;
use crate::waitid::IdType;
use crate::waitid::waitid;

// libc 0.2.186 exposes the Linux syscall-info UAPI only for GNU targets.
#[cfg(target_env = "gnu")]
type NativeSyscallExitInfo = libc::ptrace_syscall_info;

// On other Linux libc targets the same kernel UAPI has no libc Rust type.
// Request only the fixed-width common header plus EXIT result prefix from
// include/uapi/linux/ptrace.h. A different op cannot pass the decoder below.
#[cfg(not(target_env = "gnu"))]
#[repr(C)]
struct NativeSyscallExitInfo {
    op: u8,
    reserved: [u8; 3],
    arch: u32,
    instruction_pointer: u64,
    stack_pointer: u64,
    raw: i64,
    is_error: u8,
}

fn decode_native_syscall_exit(
    info: &NativeSyscallExitInfo,
    returned_bytes: usize,
) -> Result<i64, Errno> {
    #[cfg(target_env = "gnu")]
    const PREFIX_END: usize = core::mem::offset_of!(libc::ptrace_syscall_info, u)
        + core::mem::offset_of!(libc::__c_anonymous_ptrace_syscall_info_exit, is_error)
        + 1;
    #[cfg(not(target_env = "gnu"))]
    const PREFIX_END: usize = core::mem::offset_of!(NativeSyscallExitInfo, is_error) + 1;
    const _: () = assert!(PREFIX_END == 33);
    #[cfg(target_env = "gnu")]
    const EXIT: u8 = libc::PTRACE_SYSCALL_INFO_EXIT;
    #[cfg(not(target_env = "gnu"))]
    const EXIT: u8 = 2;
    if returned_bytes < PREFIX_END || info.op != EXIT {
        return Err(Errno::EPROTO);
    }
    // SAFETY: all fields were initialized before ptrace, and its positive EXIT
    // discriminant/length identifies this integer-only union member.
    #[cfg(target_env = "gnu")]
    let (raw, is_error) = unsafe { (info.u.exit.sval, info.u.exit.is_error) };
    #[cfg(not(target_env = "gnu"))]
    let (raw, is_error) = (info.raw, info.is_error);
    if is_error > 1 || (is_error != 0) != (-4095..=-1).contains(&raw) {
        return Err(Errno::EPROTO);
    }
    Ok(raw)
}

// Integer-only Linux UAPI entry/seccomp prefix, independent of libc's union
// exposure. An entry requires all 80 bytes; NONE requires its full 24-byte
// header. Requesting the actual 88-byte buffer preserves the seccomp suffix.
#[repr(C)]
#[derive(Default)]
struct NativeSyscallEntryInfo {
    op: u8,
    reserved: [u8; 3],
    arch: u32,
    ip: u64,
    sp: u64,
    number: u64,
    arguments: [u64; 6],
    seccomp_data: u32,
    padding: u32,
}
const _: () = assert!(core::mem::offset_of!(NativeSyscallEntryInfo, arguments) == 32);
const _: () = assert!(core::mem::size_of::<NativeSyscallEntryInfo>() == 88);

/// Full original operands at an actual Linux ENTRY or SECCOMP stop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyscallEntry {
    /// Linux audit architecture, not inferred from the tracer build.
    pub arch: u32,
    /// Actual instruction pointer from the held stop's kernel receipt.
    pub instruction_pointer: u64,
    /// Actual stack pointer from that receipt.
    pub stack_pointer: u64,
    /// Unchanged full-width syscall number.
    pub number: u64,
    /// All six unchanged full-width operands.
    pub arguments: [u64; 6],
    /// Whether this receipt is from seccomp rather than syscall-entry tracing.
    pub seccomp: bool,
}
fn decode_native_syscall_entry(
    info: &NativeSyscallEntryInfo,
    size: usize,
) -> Result<SyscallEntry, Errno> {
    if !matches!(info.op, 1 | 3) || size < if info.op == 3 { 84 } else { 80 } {
        return Err(Errno::EPROTO);
    }
    Ok(SyscallEntry {
        arch: info.arch,
        instruction_pointer: info.ip,
        stack_pointer: info.sp,
        number: info.number,
        arguments: info.arguments,
        seccomp: info.op == 3,
    })
}

/// Kernel-reported architecture and instruction context of a syscall-info stop.
///
/// This is observation metadata, not a source, mutation, or resume capability.
/// In particular, a native tracer can receive a different tracee audit ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyscallStopContext {
    /// Linux audit architecture reported by the kernel.
    pub arch: u32,
    /// Actual stopped instruction pointer, without a guessed instruction rewind.
    pub instruction_pointer: u64,
    /// Actual stopped stack pointer.
    pub stack_pointer: u64,
}

/// A direction-bearing observation of an actual `PTRACE_GET_SYSCALL_INFO` reply.
///
/// An ENTRY is not a SECCOMP decision or a completed syscall. An EXIT does not
/// prove which logical operation owns it, whether a restart is final, or that
/// an injected context has been restored. Callers must retain those owners.
/// Copying this metadata grants no control, source-read, or retirement rights.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyscallStopInfo {
    /// This stop has no kernel syscall-entry, exit, or seccomp receipt.
    None(SyscallStopContext),
    /// Actual pre-effect syscall-entry tracing stop, not a Tool callback.
    Entry(SyscallEntry),
    /// Actual syscall-exit tracing stop with its unchanged signed kernel result.
    Exit {
        /// Architecture and instruction context from the same reply.
        context: SyscallStopContext,
        /// Raw result, preserving Linux restart pseudo-errors as well as errno.
        result: i64,
    },
    /// Actual seccomp tracing stop, distinct from an administrative ENTRY.
    Seccomp(SyscallEntry),
}

fn decode_native_syscall_stop(
    info: &NativeSyscallEntryInfo,
    size: usize,
) -> Result<SyscallStopInfo, Errno> {
    // The shared header ends at byte24. A NONE reply still needs that complete
    // header; zero-initialized unread bytes must not become a false NONE stop.
    if size < 24 {
        return Err(Errno::EPROTO);
    }
    let context = SyscallStopContext {
        arch: info.arch,
        instruction_pointer: info.ip,
        stack_pointer: info.sp,
    };
    match info.op {
        0 => Ok(SyscallStopInfo::None(context)),
        1 => decode_native_syscall_entry(info, size).map(SyscallStopInfo::Entry),
        3 => decode_native_syscall_entry(info, size).map(SyscallStopInfo::Seccomp),
        2 if size >= 33 => {
            // Linux's integer-only union puts EXIT.rval at offset24 and its
            // one-byte is_error at offset32. The entry-shaped buffer covers
            // both. Read that byte in native memory order, not by a lossy cast
            // of the entire first argument word (which assumes little endian).
            const _: () = assert!(core::mem::offset_of!(NativeSyscallEntryInfo, number) == 24);
            const _: () = assert!(core::mem::offset_of!(NativeSyscallEntryInfo, arguments) == 32);
            let result = info.number as i64;
            let is_error = info.arguments[0].to_ne_bytes()[0];
            if is_error > 1 || (is_error != 0) != (-4095..=-1).contains(&result) {
                return Err(Errno::EPROTO);
            }
            Ok(SyscallStopInfo::Exit { context, result })
        }
        _ => Err(Errno::EPROTO),
    }
}

/// Immutable generation token carried through every typed tracee state.
#[derive(Clone, Debug)]
struct TraceeToken {
    #[cfg(feature = "notifier")]
    event: notifier::EventHandle,
    #[cfg(feature = "notifier")]
    source: Option<notifier::source::SourceStamp>,
}

// Preserve generic generation equality/hash. The private source receipt is
// checked explicitly by source custody and does not change public state identity.
impl PartialEq for TraceeToken {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(feature = "notifier")]
        {
            self.event == other.event
        }
        #[cfg(not(feature = "notifier"))]
        {
            let _ = other;
            true
        }
    }
}
impl Eq for TraceeToken {}
impl std::hash::Hash for TraceeToken {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        #[cfg(feature = "notifier")]
        std::hash::Hash::hash(&self.event, state);
        #[cfg(not(feature = "notifier"))]
        let _ = state;
    }
}

impl TraceeToken {
    fn raw_control(pid: Pid) -> Self {
        #[cfg(not(feature = "notifier"))]
        let _ = pid;
        Self {
            #[cfg(feature = "notifier")]
            event: notifier::EventHandle::for_raw_control(pid),
            #[cfg(feature = "notifier")]
            source: None,
        }
    }

    fn new() -> Self {
        Self {
            #[cfg(feature = "notifier")]
            event: notifier::EventHandle::new(),
            #[cfg(feature = "notifier")]
            source: None,
        }
    }

    fn current_or_new(pid: Pid) -> Result<Self, Errno> {
        #[cfg(not(feature = "notifier"))]
        let _ = pid;
        Ok(Self {
            #[cfg(feature = "notifier")]
            event: notifier::EventHandle::current_or_new(pid)?,
            #[cfg(feature = "notifier")]
            source: None,
        })
    }

    fn current_or_error(pid: Pid) -> Self {
        #[cfg(not(feature = "notifier"))]
        let _ = pid;
        Self {
            #[cfg(feature = "notifier")]
            event: notifier::EventHandle::current_or_error(pid),
            #[cfg(feature = "notifier")]
            source: None,
        }
    }

    #[cfg(feature = "notifier")]
    fn from_event(event: notifier::EventHandle) -> Self {
        Self {
            event,
            source: None,
        }
    }

    #[cfg(feature = "notifier")]
    fn event(&self) -> &notifier::EventHandle {
        &self.event
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

#[cfg(feature = "notifier")]
pub use notifier::OwnedWaitError;
#[cfg(feature = "notifier")]
pub use notifier::OwnedWaitFuture;

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
    /// be `PTRACE_CONT`ed or `PTRACE_DETACH`ed to finish exiting.
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
                    Running::from_current_or_new(child_pid)?,
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
                    Running::from_current_or_new(child_pid)?,
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
                    Running::from_current_or_new(child_pid)?,
                ))
            }
            libc::PTRACE_EVENT_EXEC => {
                // The event arrives under the current leader TID. GETEVENTMSG
                // identifies the executing thread's former TID; it is not a
                // newly allocated PID or a terminal status for that thread.
                let former_tid = Pid::from_raw(task.getevent()? as i32);
                Ok(Self::Exec(former_tid))
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
    /// This does not authenticate consumption for source acquisition. Controls
    /// still participate in any retained notifier generation's exclusion.
    ///
    /// Preconditions:
    /// The process must not be in a running state.
    pub fn from_raw(pid: Pid, status: i32) -> Result<Self, Error> {
        Self::from_raw_with_token(pid, status, TraceeToken::raw_control(pid))
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
    /// This decodes status without issuing a consumed source receipt.
    ///
    /// Preconditions:
    /// The process must not be in a `StillAlive` state.
    fn try_from(wait_status: WaitStatus) -> Result<Self, Error> {
        let token = wait_status
            .pid()
            .map(|pid| TraceeToken::raw_control(pid.into()))
            .unwrap_or_else(TraceeToken::new);
        Self::from_wait_status_with_token(wait_status, token)
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

    /// Returns a generation-bound terminal cleanup acknowledgment.
    ///
    /// This is primarily useful with [`Stopped::new_unchecked`] during
    /// cancellation after the caller has independently validated that the TID
    /// still names the expected ptrace generation.
    #[cfg(feature = "notifier")]
    pub fn terminal_cleanup(&self) -> TerminalCleanup {
        TerminalCleanup::new(self.0, &self.1)
    }

    /// Retain a read-only observer for this actual token and its current exec epoch.
    /// It cannot resume, wait, or mint another owning capability, and makes no
    /// new PID/proc capture. Use it only while the original current stop remains
    /// under this caller's execution-control ownership.
    #[cfg(feature = "notifier")]
    pub fn observation(&self) -> StoppedObservation {
        StoppedObservation::new(self.0, &self.1)
    }

    /// Issue source custody from this exact consumed notifier stop. Unchecked
    /// states and raw status decoders cannot issue it; no PID lookup occurs.
    #[cfg(feature = "notifier")]
    pub fn source_stop(&self) -> Result<SourceStop, Errno> {
        SourceStop::from_stopped(self)
    }

    /// Issue a distinct, single-task control witness from this consumed wait.
    /// This inactive cohort prerequisite grants no source-read or cohort authority.
    /// It cannot renew a legacy [`SourceStop`] after a control mutation.
    #[cfg(feature = "notifier")]
    pub fn control_stop(&self) -> Result<ControlStop, Errno> {
        ControlStop::from_stopped(self)
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
        Self(pid, token)
    }

    /// Returns the process ID of the tracee.
    pub fn pid(&self) -> Pid {
        self.0
    }

    /// Sets the ptracer options.
    pub fn setoptions(&self, options: ptrace::Options) -> Result<(), Error> {
        #[cfg(feature = "notifier")]
        let _source_control = self
            .1
            .event()
            .source_control(self.0)
            .map_err(|error| self.map_err(error))?;
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

        let result = unsafe {
            syscalls::syscall!(
                Sysno::ptrace,
                // PTRACE_GETREGS isn't available on aarch64, so we must use
                // PTRACE_GETREGSET instead.
                libc::PTRACE_GETREGSET,
                self.0.as_raw(),
                which,
                &mut iov as *mut _
            )
        };
        #[cfg(all(feature = "notifier", cohort_final_test))]
        self.1.event().record_getregset_for_test(which, result);
        result.map_err(|err| self.map_err(err))?;

        // GETREGSET selects the target's ABI, which may differ from the
        // tracer's (for example, a compat PRSTATUS reply is shorter). Require
        // the entire typed layout in every build before assuming initialization.
        // Zero-filling the tail would not make a short reply a valid native ABI.
        if iov.iov_len != core::mem::size_of_val(&regs) {
            return Err(Error::Errno(Errno::EPROTO));
        }

        Ok(unsafe { regs.assume_init() })
    }

    fn setregset<T>(&self, which: i32, regs: &T) -> Result<(), Error> {
        #[cfg(feature = "notifier")]
        let _source_control = self
            .1
            .event()
            .source_control(self.0)
            .map_err(|error| self.map_err(error))?;
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
        #[cfg(feature = "notifier")]
        let _source_control = self
            .1
            .event()
            .source_control(self.0)
            .map_err(|error| self.map_err(error))?;
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
        {
            #[cfg(feature = "notifier")]
            let _source_control = self
                .1
                .event()
                .source_control(self.0)
                .map_err(|error| self.map_err(error))?;
            ptrace::cont(self.0.into(), sig).map_err(|err| self.map_nix_err(err))?;
        }
        Ok(Running::from_token(self.0, self.1))
    }

    /// Attempts to resume while retaining the original capability on error.
    ///
    /// The error arm returns the same generation-bound value, not a new value
    /// constructed from its numeric PID. This retains ownership; it does not
    /// guarantee that the kernel state is unchanged after every ptrace error.
    /// The refusal is a non-owning errno, including ESRCH: no second Zombie
    /// capability escapes beside the retained Stopped. An errno alone does not
    /// acknowledge terminal status or guarantee the same physical kernel stop.
    /// Callers must use their original event owner before another transition.
    pub fn resume_retaining<T: Into<Option<Signal>>>(
        self,
        sig: T,
    ) -> Result<Running, (Self, Errno)> {
        #[cfg(feature = "notifier")]
        let result = match self.1.event().source_control(self.0) {
            Ok(_control) => ptrace::cont(self.0.into(), sig),
            Err(error) => Err(nix::errno::Errno::from_raw(error.into_raw())),
        };
        #[cfg(not(feature = "notifier"))]
        let result = ptrace::cont(self.0.into(), sig);
        self.finish_retained_resume(result)
    }

    /// Transfers the original capability into a retained notifier wait.
    ///
    /// This performs no resume and constructs no replacement running or stopped
    /// value. Use it when an external lifecycle transition may have superseded
    /// this stop, such as an actual ESRCH from [`Stopped::resume_retaining`].
    /// The error is not an exit acknowledgement: only this same generation's
    /// actual next event settles the wait. If the task remains stopped, this
    /// future can remain pending; the caller must retain its ownership and
    /// arrange any necessary termination through its existing supervisor.
    /// Registration/decoding refusals obey [`Running::wait_owned`]'s retry
    /// contract and never return a second owning capability.
    #[cfg(feature = "notifier")]
    pub fn wait_owned(self) -> notifier::OwnedWaitFuture {
        notifier::OwnedWaitFuture::from_stopped(self)
    }

    pub(crate) fn finish_retained_resume(
        self,
        result: Result<(), nix::Error>,
    ) -> Result<Running, (Self, Errno)> {
        match result {
            Ok(()) => Ok(Running::from_token(self.0, self.1)),
            Err(error) => Err((self, Errno::new(error as i32))),
        }
    }

    /// Advances the execution of the process by a single step optionally
    /// delivering a signal specified by `sig`.
    pub fn step<T: Into<Option<Signal>>>(self, sig: T) -> Result<Running, Error> {
        {
            #[cfg(feature = "notifier")]
            let _source_control = self
                .1
                .event()
                .source_control(self.0)
                .map_err(|error| self.map_err(error))?;
            ptrace::step(self.0.into(), sig).map_err(|err| self.map_nix_err(err))?;
        }
        Ok(Running::from_token(self.0, self.1))
    }

    /// Like `step`, but arranges for the tracee to be stopped at the next
    /// entry to or exit from a system call.
    pub fn syscall<T: Into<Option<Signal>>>(self, sig: T) -> Result<Running, Error> {
        {
            #[cfg(feature = "notifier")]
            let _source_control = self
                .1
                .event()
                .source_control(self.0)
                .map_err(|error| self.map_err(error))?;
            ptrace::syscall(self.0.into(), sig).map_err(|err| self.map_nix_err(err))?;
        }
        Ok(Running::from_token(self.0, self.1))
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

    /// Read an actual kernel syscall-exit result from this held stop.
    ///
    /// This requires the `PTRACE_GET_SYSCALL_INFO` EXIT discriminant and its
    /// complete fixed result prefix. It refuses an entry, seccomp or ordinary
    /// signal stop; the caller must not have overwritten the result registers.
    pub fn syscall_exit_result(&self) -> Result<i64, Error> {
        // The Linux UAPI prefix is fixed-width even across tracee word sizes.
        // All fields are integers and initialized before ptrace may write a
        // shorter prefix. Passing the actual buffer size is
        // essential: a zero addr requests zero output bytes.
        // SAFETY: the UAPI contains only integer fields and an integer union;
        // all-zero is valid, including for a short kernel write.
        let mut info: NativeSyscallExitInfo = unsafe { core::mem::zeroed() };
        let size = unsafe {
            syscalls::syscall!(
                Sysno::ptrace,
                libc::PTRACE_GET_SYSCALL_INFO,
                self.0.as_raw(),
                core::mem::size_of_val(&info),
                &mut info as *mut NativeSyscallExitInfo
            )
        }
        .map_err(|error| self.map_err(error))?;
        decode_native_syscall_exit(&info, size).map_err(Error::Errno)
    }

    fn read_syscall_entry_info(&self) -> Result<(NativeSyscallEntryInfo, usize), Error> {
        let mut info = NativeSyscallEntryInfo::default();
        let size = unsafe {
            syscalls::syscall!(
                Sysno::ptrace,
                libc::PTRACE_GET_SYSCALL_INFO,
                self.0.as_raw(),
                core::mem::size_of_val(&info),
                &mut info as *mut _
            )
        }
        .map_err(|error| self.map_err(error))?;
        Ok((info, size))
    }

    /// Reads the full original operands from an authenticated kernel entry.
    /// EXIT, NONE, truncated and unrecognized operations are refused.
    pub fn syscall_entry(&self) -> Result<SyscallEntry, Error> {
        let (info, size) = self.read_syscall_entry_info()?;
        decode_native_syscall_entry(&info, size).map_err(Error::Errno)
    }

    /// Classify this held stop using the kernel's actual direction and ABI.
    ///
    /// Refuses unknown operations and incomplete reply prefixes. This keeps
    /// NONE distinct from EXIT and does not infer entry/exit by alternating
    /// on syscall-stop signals.
    /// The returned metadata is not a consumed-stop or source authority.
    pub fn syscall_stop_info(&self) -> Result<SyscallStopInfo, Error> {
        let (info, size) = self.read_syscall_entry_info()?;
        decode_native_syscall_stop(&info, size).map_err(Error::Errno)
    }

    /// Checks that this actual held stop has no ENTRY/EXIT/SECCOMP receipt.
    /// This does not by itself prove that no syscall ran: the caller must also
    /// retain its first-resume phase and exact task/frame through every wait.
    pub fn syscall_info_is_none(&self) -> Result<bool, Error> {
        let (info, size) = self.read_syscall_entry_info()?;
        if size < 24 {
            return Err(Error::Errno(Errno::EPROTO));
        }
        Ok(info.op == 0)
    }

    /// Gets info about the signal that caused the process to be stopped.
    pub fn getsiginfo(&self) -> Result<libc::siginfo_t, Error> {
        ptrace::getsiginfo(self.0.into()).map_err(|err| self.map_nix_err(err))
    }

    /// Sets info about the singal that caused the process to be stopped.
    pub fn setsiginfo(&self, siginfo: &libc::siginfo_t) -> Result<(), Error> {
        #[cfg(feature = "notifier")]
        let _source_control = self
            .1
            .event()
            .source_control(self.0)
            .map_err(|error| self.map_err(error))?;
        ptrace::setsiginfo(self.0.into(), siginfo).map_err(|err| self.map_nix_err(err))
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
        {
            #[cfg(feature = "notifier")]
            let _source_control = self
                .1
                .event()
                .source_control(self.0)
                .map_err(|error| self.map_err(error))?;
            ptrace::detach(self.0.into(), sig).map_err(|err| self.map_nix_err(err))?;
        }
        Ok(Running::from_token(self.0, self.1))
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

    fn from_token(pid: Pid, token: TraceeToken) -> Self {
        Self(pid, token)
    }

    fn from_current_or_new(pid: Pid) -> Result<Self, Errno> {
        Ok(Self::from_token(pid, TraceeToken::current_or_new(pid)?))
    }

    /// Attaches to a running process. The process becomes a tracee and a SIGSTOP
    /// is sent to it. By the time this function ends, the tracee may not yet
    /// have actually stopped. Thus, the tracee is still considered to be in a
    /// running state and needs to be waited upon to observe the SIGSTOP.
    pub fn attach(pid: Pid) -> Result<Self, Errno> {
        let token = TraceeToken::raw_control(pid);
        #[cfg(feature = "notifier")]
        let control = token.event().source_control(pid)?;
        ptrace::attach(pid.into()).map_err(|err| Errno::new(err as i32))?;
        #[cfg(feature = "notifier")]
        drop(control);
        Ok(Self::from_token(pid, token))
    }

    /// Similar to attach, but does not stop the process. This also affects the
    /// events that are later delivered. Upon clone, fork, or vfork, an
    /// `Event::Stop` is delivered instead of `Event::Signal(Signal::SIGSTOP)`.
    ///
    /// Unlike other modes, a seized process can also accept interrupts.
    pub fn seize(pid: Pid, options: Options) -> Result<Self, Errno> {
        let token = TraceeToken::raw_control(pid);
        #[cfg(feature = "notifier")]
        let control = token.event().source_control(pid)?;
        ptrace::seize(pid.into(), options).map_err(|err| Errno::new(err as i32))?;
        #[cfg(feature = "notifier")]
        drop(control);
        Ok(Self::from_token(pid, token))
    }

    /// Interrupts the running process, even if it is in the middle of a syscall.
    /// The next time the process is waited on, the process transitions to a
    /// stopped state and `Event::Stop` is returned.
    ///
    /// # Limitations
    ///
    /// This only works for processes being traced via `Running::seize`.
    pub fn interrupt(&self) -> Result<(), Errno> {
        #[cfg(feature = "notifier")]
        let _source_control = self.1.event().source_control(self.0)?;
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

    /// Transfers this generation into an owned notifier wait.
    ///
    /// Unlike the convenience async wrapper, this future retains its original
    /// generation after a returned error. It may be polled again after the
    /// caller has handled a registration or status-decoding refusal. An error
    /// does not prove exit. Its [`OwnedWaitError`] contains no second owning
    /// Zombie. After a successful state return this future releases its old
    /// owner and every subsequent poll returns `OwnedWaitError::Completed`.
    #[cfg(feature = "notifier")]
    pub fn wait_owned(self) -> notifier::OwnedWaitFuture {
        notifier::OwnedWaitFuture::new(self)
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

    /// Transfers the original generation into a retained notifier wait.
    ///
    /// The same retry contract as [`Running::wait_owned`] applies. A subsequent
    /// stop still requires its actual stopped capability to be continued; this
    /// method does not manufacture or acknowledge terminal status.
    #[cfg(feature = "notifier")]
    pub fn wait_owned(self) -> notifier::OwnedWaitFuture {
        self.0.wait_owned()
    }

    /// Returns the original generation's terminal acknowledgment without
    /// claiming its EXIT stop or consuming its final wait.
    #[cfg(feature = "notifier")]
    pub fn terminal_cleanup(&self) -> TerminalCleanup {
        self.0.terminal_cleanup()
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
    ptrace::traceme()
        .and_then(|()| nix::sys::signal::raise(Signal::SIGSTOP))
        .map_err(|e| Errno::new(e as i32))?;
    Ok(())
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
    async fn owned_wait_clone_parent_decode_refusal_retains_same_owner()
    -> Result<(), Box<dyn std::error::Error>> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
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
            let running = tracee.resume_retaining(None).map_err(|(_, error)| error)?;
            let cleanup = running.terminal_cleanup();
            let mut owner = running.wait_owned();

            notifier::inject_capture_error_for_current_thread(injected);
            assert_eq!(
                tokio::time::timeout(
                    deadline.saturating_duration_since(std::time::Instant::now()),
                    &mut owner
                )
                .await?,
                Err(OwnedWaitError::Errno(injected)),
                "first CLONE_PARENT decode did not surface the injected capture error"
            );

            let (parent, event) = tokio::time::timeout(
                deadline.saturating_duration_since(std::time::Instant::now()),
                &mut owner,
            )
            .await??
            .assume_stopped();
            assert_eq!(parent.pid(), pid);
            assert!(parent.terminal_cleanup().same_generation(&cleanup));
            assert!(matches!((&mut owner).await, Err(OwnedWaitError::Completed)));
            let child = match event {
                Event::NewChild(ChildOp::Fork, child) => child,
                event => panic!("retry lost the CLONE_PARENT event: {event:?}"),
            };
            let (child, event) = tokio::time::timeout(
                deadline.saturating_duration_since(std::time::Instant::now()),
                child.wait_owned(),
            )
            .await??
            .assume_stopped();
            assert!(matches!(
                event,
                Event::Stop | Event::Signal(Signal::SIGSTOP)
            ));
            assert_eq!(
                tokio::time::timeout(
                    deadline.saturating_duration_since(std::time::Instant::now()),
                    child.resume(None)?.wait_owned()
                )
                .await??
                .assume_exited()
                .1,
                ExitStatus::Exited(0)
            );
            assert_eq!(
                tokio::time::timeout(
                    deadline.saturating_duration_since(std::time::Instant::now()),
                    parent.resume(None)?.wait_owned()
                )
                .await??
                .assume_exited()
                .1,
                ExitStatus::Exited(0)
            );
        }
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

#[cfg(test)]
mod syscall_exit_info_tests {
    use super::*;

    fn exit_info(op: u8, raw: i64, is_error: u8) -> NativeSyscallExitInfo {
        // SAFETY: same integer-only UAPI initialization as the actual reader.
        let mut info: NativeSyscallExitInfo = unsafe { core::mem::zeroed() };
        info.op = op;
        #[cfg(target_env = "gnu")]
        {
            info.u.exit = libc::__c_anonymous_ptrace_syscall_info_exit {
                sval: raw,
                is_error,
            };
        }
        #[cfg(not(target_env = "gnu"))]
        {
            info.raw = raw;
            info.is_error = is_error;
        }
        info
    }

    #[test]
    fn native_exit_prefix_preserves_positive_and_negative_results() {
        for raw in [0, 1, 12345, -1, -512, -4095, -4096] {
            let info = exit_info(2, raw, u8::from((-4095..=-1).contains(&raw)));
            assert_eq!(decode_native_syscall_exit(&info, 33), Ok(raw));
        }
    }

    #[test]
    fn native_exit_prefix_refuses_short_or_other_stop_payloads() {
        let mut info = exit_info(2, 123, 0);
        for bytes in [0, 1, 24, 32] {
            assert_eq!(decode_native_syscall_exit(&info, bytes), Err(Errno::EPROTO));
        }
        for op in [0, 1, 3, 255] {
            info.op = op;
            assert_eq!(decode_native_syscall_exit(&info, 80), Err(Errno::EPROTO));
        }
    }

    #[test]
    fn native_exit_prefix_refuses_inconsistent_error_discriminants() {
        for (raw, is_error) in [(123, 1), (-libc::EINTR as i64, 0), (123, 2)] {
            let info = exit_info(2, raw, is_error);
            assert_eq!(decode_native_syscall_exit(&info, 33), Err(Errno::EPROTO));
        }
    }
}

#[cfg(test)]
mod syscall_entry_info_tests {
    use super::*;
    #[test]
    fn entry_requires_complete_exact_kind_and_all_full_width_operands() {
        let mut info = NativeSyscallEntryInfo {
            op: 1,
            arch: 0xc000003e,
            ip: 0x700000002,
            sp: 0x7ffff000,
            number: 0,
            arguments: [7, 0x123456789, 1u64 << 40, 0x11, 0x22, u64::MAX],
            ..Default::default()
        };
        let entry = decode_native_syscall_entry(&info, 80).unwrap();
        assert_eq!(entry.arguments, info.arguments);
        assert_eq!(entry.instruction_pointer, info.ip);
        assert_eq!(entry.stack_pointer, info.sp);
        assert_eq!(entry.arch, info.arch);
        assert_eq!(entry.number, info.number);
        assert!(!entry.seccomp);
        for size in 0..80 {
            assert_eq!(decode_native_syscall_entry(&info, size), Err(Errno::EPROTO));
        }
        for op in [0, 2, 4, 255] {
            info.op = op;
            assert_eq!(decode_native_syscall_entry(&info, 88), Err(Errno::EPROTO));
        }
        info.op = 3;
        for size in 0..84 {
            assert_eq!(decode_native_syscall_entry(&info, size), Err(Errno::EPROTO));
        }
        let seccomp = decode_native_syscall_entry(&info, 84).unwrap();
        assert!(seccomp.seccomp);
        assert_eq!(seccomp.arguments, entry.arguments);
    }
}

#[cfg(test)]
mod syscall_stop_info_tests {
    use super::*;

    fn info(op: u8) -> NativeSyscallEntryInfo {
        NativeSyscallEntryInfo {
            op,
            arch: 0xc000003e,
            ip: 0x700000002,
            sp: 0x7ffff000,
            number: 1u64 << 40,
            arguments: [7, 0x123456789, 1u64 << 48, 0x11, 0x22, u64::MAX],
            ..Default::default()
        }
    }

    fn context(info: &NativeSyscallEntryInfo) -> SyscallStopContext {
        SyscallStopContext {
            arch: info.arch,
            instruction_pointer: info.ip,
            stack_pointer: info.sp,
        }
    }

    fn exit_info(result: i64, is_error: u8) -> NativeSyscallEntryInfo {
        let mut info = info(2);
        info.number = result as u64;
        // Nonzero suffix bytes ensure the decoder uses exactly is_error's
        // byte and does not require unreported/padding fields to be zero.
        let mut bytes = [0xa5; 8];
        bytes[0] = is_error;
        info.arguments[0] = u64::from_ne_bytes(bytes);
        info
    }

    #[test]
    fn entry_is_not_seccomp_or_exit_and_keeps_all_operands() {
        let info = info(1);
        let expected = decode_native_syscall_entry(&info, 80).unwrap();
        assert_eq!(
            decode_native_syscall_stop(&info, 80),
            Ok(SyscallStopInfo::Entry(expected))
        );
        assert!(!expected.seccomp);
        assert_eq!(expected.arguments, info.arguments);
        assert_eq!(expected.number, 1u64 << 40);
        for size in 0..80 {
            assert_eq!(decode_native_syscall_stop(&info, size), Err(Errno::EPROTO));
        }
    }

    #[test]
    fn seccomp_is_not_administrative_entry_and_requires_its_suffix() {
        let info = info(3);
        let expected = decode_native_syscall_entry(&info, 84).unwrap();
        assert!(expected.seccomp);
        assert_eq!(
            decode_native_syscall_stop(&info, 84),
            Ok(SyscallStopInfo::Seccomp(expected))
        );
        for size in 0..84 {
            assert_eq!(decode_native_syscall_stop(&info, size), Err(Errno::EPROTO));
        }
    }

    #[test]
    fn exit_preserves_result_restart_and_context_without_becoming_none() {
        for result in [0, 1, i64::MAX, -1, -512, -513, -514, -516, -4095, -4096] {
            let info = exit_info(result, u8::from((-4095..=-1).contains(&result)));
            assert_eq!(
                decode_native_syscall_stop(&info, 33),
                Ok(SyscallStopInfo::Exit {
                    context: context(&info),
                    result,
                })
            );
            for size in 0..33 {
                assert_eq!(decode_native_syscall_stop(&info, size), Err(Errno::EPROTO));
            }
        }
    }

    #[test]
    fn exit_refuses_inconsistent_error_byte_without_normalizing_errno() {
        for (result, is_error) in [(1, 1), (-512, 0), (-4095, 0), (-4096, 1), (0, 2)] {
            assert_eq!(
                decode_native_syscall_stop(&exit_info(result, is_error), 33),
                Err(Errno::EPROTO)
            );
        }
    }

    #[test]
    fn none_requires_actual_complete_header_not_zeroed_unread_storage() {
        let info = info(0);
        assert_eq!(
            decode_native_syscall_stop(&info, 24),
            Ok(SyscallStopInfo::None(context(&info)))
        );
        for size in 0..24 {
            assert_eq!(decode_native_syscall_stop(&info, size), Err(Errno::EPROTO));
        }
    }

    #[test]
    fn unknown_discriminants_never_turn_into_a_completed_or_absent_syscall() {
        for op in [4, 5, 127, 255] {
            for size in [0, 24, 33, 80, 84, 88, 1024] {
                assert_eq!(
                    decode_native_syscall_stop(&info(op), size),
                    Err(Errno::EPROTO)
                );
            }
        }
    }

    #[test]
    fn actual_audit_abi_is_preserved_instead_of_inferred_from_low_number() {
        for arch in [0xc000003e, 0x40000003, 0xc00000b7, 0xffffffff] {
            let mut entry = info(1);
            entry.arch = arch;
            entry.number = 1;
            let SyscallStopInfo::Entry(observed) = decode_native_syscall_stop(&entry, 80).unwrap()
            else {
                panic!("actual ENTRY direction lost");
            };
            assert_eq!(observed.arch, arch);
            assert_eq!(observed.number, 1);
            let mut exit = exit_info(1, 0);
            exit.arch = arch;
            assert_eq!(
                decode_native_syscall_stop(&exit, 33),
                Ok(SyscallStopInfo::Exit {
                    context: context(&exit),
                    result: 1,
                })
            );
        }
    }
}
