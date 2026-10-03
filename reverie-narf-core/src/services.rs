/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The kernel services the core needs from Narf.

use reverie::Auxv;
use reverie::Pid;
use reverie::syscalls::Errno;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::libc;

use crate::NarfSyscallOutcome;
use crate::NarfSyscallRequest;
use crate::OriginalSyscallError;

/// Whether a transition created a thread in the caller's process or a new
/// process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreatedTaskKind {
    /// A thread sharing the caller's process (`CLONE_THREAD`).
    Thread,
    /// The leader of a new process (`fork`, `vfork`, `clone` without
    /// `CLONE_THREAD`).
    Process,
}

/// A task created by the transition the core just ran.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreatedTask {
    /// Thread ID of the new task.
    pub tid: Pid,
    /// Process ID of the new task. Equal to `tid` for [`CreatedTaskKind::Process`].
    pub pid: Pid,
    /// Thread or process.
    pub kind: CreatedTaskKind,
}

/// Narf's services for the task whose syscall is being intercepted.
///
/// One value describes one task for the duration of one interceptor call (or
/// one lifecycle call such as thread start). Every method runs directly in the
/// kernel's address space; none is an RPC.
///
/// The kernel's native transition is used at most as the transition contract
/// allows: [`execute_original`](Self::execute_original) runs the intercepted
/// handler at most once, [`execute_injected`](Self::execute_injected) is
/// repeatable, and after either reports [`NarfSyscallOutcome::ContextManaged`]
/// nothing further executes. The core never calls `execute_original` twice
/// and treats [`OriginalSyscallError::AlreadyExecuted`] as a kernel contract
/// violation.
///
/// `Send + Sync` is required because [`reverie::Guest`] is `Send` and
/// [`reverie::GlobalRPC`] is `Sync`. Narf's transition object is not `Send`;
/// the kernel's wrapper supplies these bounds on the strength of the fact that
/// the core reaches a `KernelServices` only synchronously, from the calling
/// task, inside the interceptor call that created it: a Tool future kept
/// across a park holds no reference to it, and sees the next call's services
/// only while that call polls it. The task may be switched out inside
/// [`wait_for_repoll`](Self::wait_for_repoll) and resume on another CPU,
/// still inside the same call, so the wrapper must not rely on the CPU
/// staying the same (by keeping per-CPU state across the wait, say).
pub trait KernelServices: Send + Sync {
    /// Guest-memory accessor bound to the current task's address space.
    type Memory: MemoryAccess + Send;

    /// Thread ID of the current task.
    fn tid(&self) -> Pid;

    /// Process ID of the current task.
    fn pid(&self) -> Pid;

    /// Parent process ID, or `None` for the root of the hosted process tree.
    fn ppid(&self) -> Option<Pid>;

    /// The auxiliary vector the kernel installed for the current process.
    fn auxv(&self) -> Auxv;

    /// Memory accessor for the current task's address space.
    fn memory(&self) -> Self::Memory;

    /// Snapshot of the current task's user registers at syscall entry, with
    /// `orig_rax` holding the wire number.
    fn regs(&self) -> libc::user_regs_struct;

    /// Run the intercepted original syscall. At most once per interceptor call.
    fn execute_original(&mut self) -> Result<NarfSyscallOutcome, OriginalSyscallError>;

    /// Run an explicit request through the native handler, bypassing
    /// interception. Repeatable until a transition becomes context-managed.
    fn execute_injected(&mut self, request: NarfSyscallRequest) -> NarfSyscallOutcome;

    /// The task created by the most recent transition, reported at most once.
    ///
    /// The kernel keeps a created task from entering user mode until the
    /// interceptor call that created it has returned, so the core registers
    /// the task before it can issue its first syscall.
    fn take_created_task(&mut self) -> Option<CreatedTask>;

    /// Mark the current task as a daemon: the run does not wait for it.
    fn daemonize(&mut self) -> Result<(), Errno>;

    /// Let other tasks run before the core polls this callback's pending
    /// Tool future again, and report how the wait ended.
    ///
    /// The core calls this only between two polls of a future that is
    /// pending without having made a terminal transition, parked an inject
    /// or failed, which it treats as waiting for another task (through a
    /// global-state RPC, say). Nothing wakes such a future, so the core
    /// polls it again after every [`RepollWait::Yielded`], with no limit:
    /// any bound on the wait, or deadlock detection, belongs to the kernel
    /// (or to the Tool). Without one, a future that never becomes ready
    /// keeps its task yielding until the task is killed.
    ///
    /// The task may be switched out here and resume on another CPU; see the
    /// `Send + Sync` requirement above. The default cannot wait and returns
    /// [`RepollWait::Unsupported`], which ends the callback as
    /// [`NarfFatal::ToolSuspended`](crate::NarfFatal::ToolSuspended).
    fn wait_for_repoll(&mut self) -> RepollWait {
        RepollWait::Unsupported
    }

    /// Whether the current task is ending: it was killed, or its process
    /// exited, during this callback, so it will not run user code again.
    ///
    /// The core asks after a non-tail inject reports
    /// [`NarfSyscallOutcome::ContextManaged`], and when an RDTSC callback
    /// ends with the task context-managed. If the task is ending, the
    /// callback ends there: the kernel owns the task's context, and the core
    /// drops the Tool future. Otherwise the core treats the inject as parked,
    /// which only a syscall callback survives, by keeping its future until
    /// the guest's syscall is re-executed. A callback with no guest syscall
    /// to re-execute, such as thread start or an RDTSC event, then fails
    /// closed with [`NarfFatal::InjectParked`](crate::NarfFatal::InjectParked),
    /// or, after a tail inject in an RDTSC event, with
    /// [`NarfFatal::RdtscContextManaged`](crate::NarfFatal::RdtscContextManaged).
    ///
    /// The default answers `false`, so every such inject is treated as
    /// parked.
    fn killed(&self) -> bool {
        false
    }
}

/// How the kernel's [`KernelServices::wait_for_repoll`] ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepollWait {
    /// Other tasks could run, and this one runs again: the core polls the
    /// Tool future again.
    Yielded,
    /// The task was killed while it waited. The kernel owns its context and
    /// runs no further transition for this callback; the core drops the
    /// Tool future.
    Killed,
    /// The kernel cannot switch this task out here.
    Unsupported,
}
