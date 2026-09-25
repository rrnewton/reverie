/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! [`reverie::Guest`] over [`KernelServices`].

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::AtomicBool;

use async_trait::async_trait;
use reverie::Auxv;
use reverie::Error;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Never;
use reverie::Pid;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Errno;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie::syscalls::libc;

use crate::NarfSyscallOutcome;
use crate::NarfSyscallRequest;
use crate::OriginalSyscallError;
use crate::host::NarfFatal;
use crate::host::NarfToolHost;
use crate::host::Parked;
use crate::host::Redo;
use crate::host::TaskLock;
use crate::host::TaskTable;
use crate::services::KernelServices;
use crate::stack::NarfStack;

/// How a callback ended its use of the native transition.
pub(crate) struct Terminal {
    pub(crate) outcome: NarfSyscallOutcome,
    /// What the kernel must re-issue if it re-executes a parked syscall.
    pub(crate) parked: Option<Parked>,
}

/// Whether a request is the last thing a task does in its current context,
/// so a Tool that injects it without a tail cannot expect a return.
fn ends_context(number: u32) -> bool {
    matches!(
        Sysno::new(number as usize),
        Some(
            Sysno::exit | Sysno::exit_group | Sysno::execve | Sysno::execveat | Sysno::rt_sigreturn
        )
    )
}

/// The [`reverie::Guest`] one Tool callback sees.
///
/// It borrows the current task's [`KernelServices`] and `ThreadState` for
/// exactly one callback; the host drops it before the interceptor returns.
pub struct NarfGuest<'a, T, K, L>
where
    T: Tool,
    K: KernelServices,
{
    host: &'a NarfToolHost<T, L>,
    kernel: &'a mut K,
    tool: &'a Arc<T>,
    thread_state: &'a mut T::ThreadState,
    /// The intercepted request, or `None` for a lifecycle callback.
    original: Option<NarfSyscallRequest>,
    original_consumed: bool,
    stack_flag: Arc<AtomicBool>,
    pub(crate) terminal: Option<Terminal>,
    pub(crate) fatal: Option<NarfFatal>,
}

impl<'a, T, K, L> NarfGuest<'a, T, K, L>
where
    T: Tool,
    K: KernelServices,
    L: TaskLock<TaskTable<T>>,
{
    pub(crate) fn new(
        host: &'a NarfToolHost<T, L>,
        kernel: &'a mut K,
        tool: &'a Arc<T>,
        thread_state: &'a mut T::ThreadState,
        original: Option<NarfSyscallRequest>,
    ) -> Self {
        Self {
            host,
            kernel,
            tool,
            thread_state,
            original,
            original_consumed: false,
            stack_flag: Arc::new(AtomicBool::new(false)),
            terminal: None,
            fatal: None,
        }
    }

    fn fail(&mut self, fatal: NarfFatal) {
        self.fatal.get_or_insert(fatal);
    }

    /// Runs `request` through the kernel, as the original if it is the
    /// not-yet-run intercepted syscall and as an injection otherwise.
    ///
    /// Returns the outcome and what re-executing it would mean, or `None` if
    /// the callback has failed.
    fn execute(&mut self, request: NarfSyscallRequest) -> Option<(NarfSyscallOutcome, Redo)> {
        if self.fatal.is_some() || self.terminal.is_some() {
            self.fail(NarfFatal::TransitionAfterTerminal);
            return None;
        }
        let original = self
            .original
            .filter(|original| original.same_call(&request));
        let result = match original {
            Some(_) if !self.original_consumed => {
                self.original_consumed = true;
                match self.kernel.execute_original() {
                    Ok(outcome) => (outcome, Redo::Original),
                    Err(OriginalSyscallError::ContextManaged) => {
                        (NarfSyscallOutcome::ContextManaged, Redo::Nothing)
                    }
                    Err(OriginalSyscallError::AlreadyExecuted) => {
                        self.fail(NarfFatal::OriginalAlreadyExecuted);
                        return None;
                    }
                }
            }
            // A repeated forward of the original keeps its exact wire number,
            // including Narf's version byte.
            Some(original) => {
                let outcome = self.kernel.execute_injected(original);
                (outcome, Redo::Injected(original))
            }
            None => (
                self.kernel.execute_injected(request),
                Redo::Injected(request),
            ),
        };
        if let Some(created) = self.kernel.take_created_task() {
            let parent = self.kernel.tid();
            let parent_pid = self.kernel.pid();
            if let Err(fatal) = self.host.register_created(
                self.tool,
                parent,
                parent_pid,
                &*self.thread_state,
                created,
            ) {
                self.fail(fatal);
                return None;
            }
        }
        Some(result)
    }

    fn parked(&self, request: NarfSyscallRequest, redo: Redo) -> Option<Parked> {
        let entry = self.original?;
        match redo {
            Redo::Nothing => None,
            _ if ends_context(request.linux_number()) => None,
            redo => Some(Parked { entry, redo }),
        }
    }

    /// Runs `request` as the callback's terminal action.
    pub(crate) fn tail(&mut self, request: NarfSyscallRequest) {
        if let Some((outcome, redo)) = self.execute(request) {
            let parked = match outcome {
                NarfSyscallOutcome::ContextManaged => self.parked(request, redo),
                NarfSyscallOutcome::Returned(_) => None,
            };
            self.terminal = Some(Terminal { outcome, parked });
        }
    }

    /// Re-issues a parked transition at a kernel re-execution entry.
    pub(crate) fn redo(&mut self, parked: Parked) {
        match parked.redo {
            Redo::Original => self.tail(parked.entry),
            Redo::Injected(request) => {
                // Consume the original so the injected request, not the
                // guest's own syscall, is what re-executes.
                self.original_consumed = true;
                self.tail(request);
            }
            Redo::Nothing => self.fail(NarfFatal::UnexpectedReexecution),
        }
    }

    /// Runs `request` and returns its result to the Tool, if it returns.
    fn inject_request(&mut self, request: NarfSyscallRequest) -> Option<i64> {
        let (outcome, redo) = self.execute(request)?;
        match outcome {
            NarfSyscallOutcome::Returned(value) => Some(value),
            NarfSyscallOutcome::ContextManaged => {
                if !matches!(redo, Redo::Nothing) && !ends_context(request.linux_number()) {
                    // The Tool awaits a return that will not come in this
                    // callback: the kernel parked or redirected the task.
                    // Its continuation cannot run, so fail closed rather than
                    // silently skip it.
                    self.fail(NarfFatal::InjectParked {
                        number: request.number,
                    });
                } else {
                    self.terminal = Some(Terminal {
                        outcome,
                        parked: None,
                    });
                }
                None
            }
        }
    }
}

fn request_of<S: SyscallInfo>(syscall: S) -> NarfSyscallRequest {
    let (number, args) = syscall.into_parts();
    NarfSyscallRequest {
        number: number.id() as u32,
        args: [
            args.arg0 as u64,
            args.arg1 as u64,
            args.arg2 as u64,
            args.arg3 as u64,
            args.arg4 as u64,
            args.arg5 as u64,
        ],
    }
}

#[async_trait]
impl<T, K, L> GlobalRPC<T::GlobalState> for NarfGuest<'_, T, K, L>
where
    T: Tool,
    K: KernelServices,
    L: TaskLock<TaskTable<T>>,
{
    async fn send_rpc(
        &self,
        message: <T::GlobalState as GlobalTool>::Request,
    ) -> <T::GlobalState as GlobalTool>::Response {
        self.host.global().receive_rpc(self.tid(), message).await
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        self.host.config()
    }
}

#[async_trait]
impl<T, K, L> Guest<T> for NarfGuest<'_, T, K, L>
where
    T: Tool,
    K: KernelServices,
    L: TaskLock<TaskTable<T>>,
{
    type Memory = K::Memory;
    type Stack = NarfStack<K::Memory>;

    fn tid(&self) -> Pid {
        self.kernel.tid()
    }

    fn pid(&self) -> Pid {
        self.kernel.pid()
    }

    fn ppid(&self) -> Option<Pid> {
        self.kernel.ppid()
    }

    fn auxv(&self) -> Auxv {
        self.kernel.auxv()
    }

    fn memory(&self) -> Self::Memory {
        self.kernel.memory()
    }

    fn thread_state_mut(&mut self) -> &mut T::ThreadState {
        self.thread_state
    }

    fn thread_state(&self) -> &T::ThreadState {
        self.thread_state
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        self.kernel.regs()
    }

    async fn stack(&mut self) -> Self::Stack {
        let rsp = self.kernel.regs().rsp;
        NarfStack::new(self.kernel.memory(), rsp, &self.stack_flag)
    }

    async fn daemonize(&mut self) {
        if let Err(errno) = self.kernel.daemonize() {
            self.fail(NarfFatal::DaemonizeRefused(errno));
        }
    }

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno> {
        match self.inject_request(request_of(syscall)) {
            Some(value) => Errno::from_ret(value as usize).map(|value| value as i64),
            None => core::future::pending().await,
        }
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        self.tail(request_of(syscall));
        core::future::pending().await
    }

    fn set_timer(&mut self, _schedule: TimerSchedule) -> Result<(), Error> {
        Err(Errno::ENOSYS.into())
    }

    fn set_timer_precise(&mut self, _schedule: TimerSchedule) -> Result<(), Error> {
        Err(Errno::ENOSYS.into())
    }

    fn read_clock(&mut self) -> Result<u64, Error> {
        Err(Errno::ENOSYS.into())
    }
}
