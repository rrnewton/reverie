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
/// the core polls the Tool exactly once, synchronously, on the calling CPU and
/// drops the guest before the interceptor returns.
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
}
