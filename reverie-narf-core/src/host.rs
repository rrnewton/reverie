/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Tool lifetime, per-task state and single-poll dispatch.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::task::Context;
use core::task::Poll;
use core::task::Waker;

use async_trait::async_trait;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Pid;
use reverie::ThreadOwnership;
use reverie::Tool;
use reverie::syscalls::Errno;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;

use crate::LINUX_MAX_ERRNO;
use crate::NarfSyscallOutcome;
use crate::NarfSyscallRequest;
use crate::SyscallEntry;
use crate::guest::NarfGuest;
use crate::guest::Terminal;
use crate::services::CreatedTask;
use crate::services::CreatedTaskKind;
use crate::services::KernelServices;

type Config<T> = <<T as Tool>::GlobalState as GlobalTool>::Config;

/// A named reason the core stopped instead of guessing.
///
/// Every variant is fatal to the run: the kernel must stop the hosted process
/// tree rather than resume the task.
pub enum NarfFatal {
    /// The Tool's future was still pending after its single poll and had not
    /// made a terminal transition. The core never polls it again.
    ToolSuspended,
    /// The Tool failed with a non-errno error.
    Tool(Error),
    /// The Tool returned an errno outside Linux's `1..=4095` range.
    InvalidErrno(i32),
    /// The kernel reported the original syscall as already executed although
    /// the core had not run it in this callback.
    OriginalAlreadyExecuted,
    /// A Guest method tried to run a syscall after the callback's terminal
    /// transition.
    TransitionAfterTerminal,
    /// A non-tail `inject` of this wire number parked or redirected the task,
    /// so the Tool's continuation cannot run.
    InjectParked {
        /// Wire number of the injected request.
        number: u32,
    },
    /// The kernel flagged a re-execution for a task with no parked syscall.
    UnexpectedReexecution,
    /// The kernel re-executed a syscall other than the one that parked.
    ReexecutionMismatch {
        /// The syscall that parked.
        parked: NarfSyscallRequest,
        /// The syscall the kernel re-executed.
        reexecuted: NarfSyscallRequest,
    },
    /// A callback arrived for a task already inside a callback.
    RecursiveEntry(Pid),
    /// A callback or exit arrived for a task the host does not know.
    UnknownTask(Pid),
    /// A task was registered twice.
    DuplicateTask(Pid),
    /// A created thread's process is not its creator's process.
    CreatedTaskMismatch(CreatedTask),
    /// The kernel reported a task exit while that task was inside a callback.
    ExitDuringCallback(Pid),
    /// A process's Tool was still shared when its last thread exited.
    ProcessToolShared(Pid),
    /// The Tool subscribed to CPUID or RDTSC events, which Narf cannot yet
    /// deliver.
    UnsupportedSubscription,
    /// The Tool asked for host-owned threads, which this backend lacks.
    UnsupportedThreadOwnership,
    /// The Tool asked to observe signal dequeues, which Narf cannot yet
    /// deliver.
    UnsupportedSignalDequeues,
    /// The kernel refused to daemonize the task.
    DaemonizeRefused(Errno),
    /// `handle_post_exec` failed with this errno.
    PostExec(Errno),
    /// A lifecycle callback tail-injected a syscall that returned; there is
    /// no guest syscall to deliver the value to.
    TailInjectOutsideSyscall,
}

impl fmt::Debug for NarfFatal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolSuspended => f.write_str("ToolSuspended"),
            Self::Tool(error) => f.debug_tuple("Tool").field(error).finish(),
            Self::InvalidErrno(raw) => f.debug_tuple("InvalidErrno").field(raw).finish(),
            Self::OriginalAlreadyExecuted => f.write_str("OriginalAlreadyExecuted"),
            Self::TransitionAfterTerminal => f.write_str("TransitionAfterTerminal"),
            Self::InjectParked { number } => f
                .debug_struct("InjectParked")
                .field("number", number)
                .finish(),
            Self::UnexpectedReexecution => f.write_str("UnexpectedReexecution"),
            Self::ReexecutionMismatch { parked, reexecuted } => f
                .debug_struct("ReexecutionMismatch")
                .field("parked", parked)
                .field("reexecuted", reexecuted)
                .finish(),
            Self::RecursiveEntry(tid) => f.debug_tuple("RecursiveEntry").field(tid).finish(),
            Self::UnknownTask(tid) => f.debug_tuple("UnknownTask").field(tid).finish(),
            Self::DuplicateTask(tid) => f.debug_tuple("DuplicateTask").field(tid).finish(),
            Self::CreatedTaskMismatch(task) => {
                f.debug_tuple("CreatedTaskMismatch").field(task).finish()
            }
            Self::ExitDuringCallback(tid) => {
                f.debug_tuple("ExitDuringCallback").field(tid).finish()
            }
            Self::ProcessToolShared(pid) => f.debug_tuple("ProcessToolShared").field(pid).finish(),
            Self::UnsupportedSubscription => f.write_str("UnsupportedSubscription"),
            Self::UnsupportedThreadOwnership => f.write_str("UnsupportedThreadOwnership"),
            Self::UnsupportedSignalDequeues => f.write_str("UnsupportedSignalDequeues"),
            Self::DaemonizeRefused(errno) => {
                f.debug_tuple("DaemonizeRefused").field(errno).finish()
            }
            Self::PostExec(errno) => f.debug_tuple("PostExec").field(errno).finish(),
            Self::TailInjectOutsideSyscall => f.write_str("TailInjectOutsideSyscall"),
        }
    }
}

/// What the kernel does with the intercepted syscall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    /// Resume the guest with this Linux-ABI return value (negative errno for
    /// failure).
    Complete(i64),
    /// A transition parked, exited, execed or redirected the task; the kernel
    /// owns its continuation and no value may be fabricated.
    ContextManaged,
}

/// What the kernel does after a lifecycle callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleOutcome {
    /// Let the task continue.
    Continue,
    /// A tail-injected syscall exited or redirected the task.
    ContextManaged,
}

/// What a task exit tore down.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskExit {
    /// The thread's process had no threads left, so its Tool was torn down.
    pub process_exited: bool,
}

/// A lock the kernel provides for the host's task table.
///
/// The host holds it only for table lookups and updates, never while a Tool
/// runs, so an interrupt-safe spin lock is suitable.
pub trait TaskLock<V>: Send + Sync {
    /// Wraps `value`.
    fn new(value: V) -> Self
    where
        Self: Sized;

    /// Runs `f` with exclusive access to the value.
    fn with<R>(&self, f: impl FnOnce(&mut V) -> R) -> R;
}

/// What a re-executed parked syscall must run again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Redo {
    /// The guest's own syscall.
    Original,
    /// A syscall the Tool injected in its place.
    Injected(NarfSyscallRequest),
    /// Nothing ran: the context was already managed.
    Nothing,
}

/// A syscall whose transition parked the task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Parked {
    /// The guest syscall the Tool observed.
    pub(crate) entry: NarfSyscallRequest,
    pub(crate) redo: Redo,
}

struct ProcessEntry<T> {
    tool: Arc<T>,
    threads: usize,
}

struct ThreadEntry<T: Tool> {
    pid: Pid,
    /// `None` while the thread is inside a callback.
    state: Option<T::ThreadState>,
    parked: Option<Parked>,
}

/// Per-process Tools and per-thread states, keyed by the kernel's IDs.
pub struct TaskTable<T: Tool> {
    processes: BTreeMap<i32, ProcessEntry<T>>,
    threads: BTreeMap<i32, ThreadEntry<T>>,
}

struct Checkout<T: Tool> {
    tool: Arc<T>,
    state: T::ThreadState,
    parked: Option<Parked>,
}

struct Removal<T> {
    tool: Arc<T>,
    pid: Pid,
    last: bool,
}

impl<T: Tool> TaskTable<T> {
    fn new() -> Self {
        Self {
            processes: BTreeMap::new(),
            threads: BTreeMap::new(),
        }
    }

    fn insert_process(&mut self, pid: Pid, tool: Arc<T>) -> Result<(), NarfFatal> {
        if self.processes.contains_key(&pid.as_raw()) {
            return Err(NarfFatal::DuplicateTask(pid));
        }
        self.processes
            .insert(pid.as_raw(), ProcessEntry { tool, threads: 0 });
        Ok(())
    }

    fn insert_thread(
        &mut self,
        tid: Pid,
        pid: Pid,
        state: T::ThreadState,
    ) -> Result<(), NarfFatal> {
        if self.threads.contains_key(&tid.as_raw()) {
            return Err(NarfFatal::DuplicateTask(tid));
        }
        let process = self
            .processes
            .get_mut(&pid.as_raw())
            .ok_or(NarfFatal::UnknownTask(pid))?;
        process.threads += 1;
        self.threads.insert(
            tid.as_raw(),
            ThreadEntry {
                pid,
                state: Some(state),
                parked: None,
            },
        );
        Ok(())
    }

    fn checkout(&mut self, tid: Pid) -> Result<Checkout<T>, NarfFatal> {
        let thread = self
            .threads
            .get_mut(&tid.as_raw())
            .ok_or(NarfFatal::UnknownTask(tid))?;
        let state = thread.state.take().ok_or(NarfFatal::RecursiveEntry(tid))?;
        let parked = thread.parked.take();
        let tool = self
            .processes
            .get(&thread.pid.as_raw())
            .ok_or(NarfFatal::UnknownTask(thread.pid))?
            .tool
            .clone();
        Ok(Checkout {
            tool,
            state,
            parked,
        })
    }

    fn checkin(
        &mut self,
        tid: Pid,
        state: T::ThreadState,
        parked: Option<Parked>,
    ) -> Result<(), NarfFatal> {
        let thread = self
            .threads
            .get_mut(&tid.as_raw())
            .ok_or(NarfFatal::UnknownTask(tid))?;
        thread.state = Some(state);
        thread.parked = parked;
        Ok(())
    }

    fn remove_thread(&mut self, tid: Pid) -> Result<(Removal<T>, T::ThreadState), NarfFatal> {
        let thread = self
            .threads
            .get(&tid.as_raw())
            .ok_or(NarfFatal::UnknownTask(tid))?;
        if thread.state.is_none() {
            return Err(NarfFatal::ExitDuringCallback(tid));
        }
        let thread = self.threads.remove(&tid.as_raw()).expect("present above");
        let state = thread.state.expect("checked above");
        let pid = thread.pid;
        let process = self
            .processes
            .get_mut(&pid.as_raw())
            .ok_or(NarfFatal::UnknownTask(pid))?;
        process.threads -= 1;
        let removal = if process.threads == 0 {
            let process = self.processes.remove(&pid.as_raw()).expect("present above");
            Removal {
                tool: process.tool,
                pid,
                last: true,
            }
        } else {
            Removal {
                tool: process.tool.clone(),
                pid,
                last: false,
            }
        };
        Ok((removal, state))
    }
}

/// Polls `future` exactly once with a waker that does nothing.
///
/// Nothing in the core ever wakes a Tool future, so a `Pending` result means
/// the future is waiting for something that will not arrive in this callback.
fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

fn errno_result(errno: Errno) -> Result<i64, NarfFatal> {
    let raw = errno.into_raw();
    if (1..=LINUX_MAX_ERRNO).contains(&raw) {
        Ok(-i64::from(raw))
    } else {
        Err(NarfFatal::InvalidErrno(raw))
    }
}

/// Hosts one Tool for one run inside the Narf kernel.
pub struct NarfToolHost<T: Tool, L> {
    global: T::GlobalState,
    config: Config<T>,
    subscribed: [u64; 8],
    tasks: L,
}

impl<T, L> NarfToolHost<T, L>
where
    T: Tool,
    L: TaskLock<TaskTable<T>>,
{
    /// Creates the run's global state from `config`.
    ///
    /// Fails closed if the Tool needs an event source Narf cannot deliver, or
    /// if `init_global_state` does not complete in one poll.
    pub fn new(config: Config<T>) -> Result<Self, NarfFatal> {
        let subscription = T::subscriptions(&config);
        if subscription.has_cpuid() || subscription.has_rdtsc() {
            return Err(NarfFatal::UnsupportedSubscription);
        }
        if T::thread_ownership(&config) != ThreadOwnership::Tool {
            return Err(NarfFatal::UnsupportedThreadOwnership);
        }
        if T::observe_signal_dequeues(&config) {
            return Err(NarfFatal::UnsupportedSignalDequeues);
        }
        let mut subscribed = [0u64; 8];
        for sysno in subscription.iter_syscalls() {
            let id = sysno.id() as usize;
            if let Some(word) = subscribed.get_mut(id / 64) {
                *word |= 1 << (id % 64);
            }
        }
        let global = {
            let mut init = T::GlobalState::init_global_state(&config);
            match poll_once(init.as_mut()) {
                Poll::Ready(global) => global,
                Poll::Pending => return Err(NarfFatal::ToolSuspended),
            }
        };
        Ok(Self {
            global,
            config,
            subscribed,
            tasks: L::new(TaskTable::new()),
        })
    }

    /// The run's global state, shared directly by every callback.
    pub fn global(&self) -> &T::GlobalState {
        &self.global
    }

    /// The Tool configuration.
    pub fn config(&self) -> &Config<T> {
        &self.config
    }

    /// Whether the Tool subscribed to `sysno`.
    pub fn is_subscribed(&self, sysno: Sysno) -> bool {
        let id = sysno.id() as usize;
        self.subscribed
            .get(id / 64)
            .is_some_and(|word| word & (1 << (id % 64)) != 0)
    }

    /// Number of live threads the host is tracking.
    pub fn live_threads(&self) -> usize {
        self.tasks.with(|table| table.threads.len())
    }

    /// Number of live processes the host is tracking.
    pub fn live_processes(&self) -> usize {
        self.tasks.with(|table| table.processes.len())
    }

    /// Runs `f` on thread `tid`'s state, if the thread is live and not inside
    /// a callback.
    pub fn with_thread_state<R>(
        &self,
        tid: Pid,
        f: impl FnOnce(&T::ThreadState) -> R,
    ) -> Option<R> {
        self.tasks.with(|table| {
            table
                .threads
                .get(&tid.as_raw())
                .and_then(|thread| thread.state.as_ref())
                .map(f)
        })
    }

    /// Runs `f` on process `pid`'s Tool, if the process is live.
    pub fn with_process_tool<R>(&self, pid: Pid, f: impl FnOnce(&T) -> R) -> Option<R> {
        let tool = self.tasks.with(|table| {
            table
                .processes
                .get(&pid.as_raw())
                .map(|process| process.tool.clone())
        })?;
        Some(f(&tool))
    }

    /// Registers the root task of the hosted process tree.
    pub fn register_root(&self, tid: Pid, pid: Pid) -> Result<(), NarfFatal> {
        let tool = Arc::new(T::new(pid, &self.config));
        let state = tool.init_thread_state(tid, None);
        self.tasks.with(|table| {
            table.insert_process(pid, tool)?;
            table.insert_thread(tid, pid, state)
        })
    }

    /// Registers a task the current callback's transition created, before it
    /// can run, exactly as the ptrace backend's `cloned` and `forked` do.
    pub(crate) fn register_created(
        &self,
        parent_tool: &Arc<T>,
        parent_tid: Pid,
        parent_pid: Pid,
        parent_state: &T::ThreadState,
        created: CreatedTask,
    ) -> Result<(), NarfFatal> {
        match created.kind {
            CreatedTaskKind::Thread => {
                if created.pid != parent_pid {
                    return Err(NarfFatal::CreatedTaskMismatch(created));
                }
                let state =
                    parent_tool.init_thread_state(created.tid, Some((parent_tid, parent_state)));
                self.tasks
                    .with(|table| table.insert_thread(created.tid, created.pid, state))
            }
            CreatedTaskKind::Process => {
                if created.pid != created.tid {
                    return Err(NarfFatal::CreatedTaskMismatch(created));
                }
                let tool = Arc::new(T::new(created.pid, &self.config));
                let state = tool.init_thread_state(created.tid, Some((parent_tid, parent_state)));
                self.tasks.with(|table| {
                    table.insert_process(created.pid, tool)?;
                    table.insert_thread(created.tid, created.pid, state)
                })
            }
        }
    }

    /// Handles one interceptor entry for the current task.
    ///
    /// A new subscribed syscall is delivered to the Tool's
    /// `handle_syscall_event`, polled once. An unsubscribed syscall, or one
    /// whose number Reverie does not know, runs natively through the core so
    /// that created tasks are still registered. A park re-execution re-issues
    /// the parked transition without calling the Tool again.
    pub fn handle_syscall<K: KernelServices>(
        &self,
        kernel: &mut K,
        entry: SyscallEntry,
    ) -> Result<Disposition, NarfFatal> {
        let tid = kernel.tid();
        let Checkout {
            tool,
            mut state,
            parked,
        } = self.tasks.with(|table| table.checkout(tid))?;
        let (result, parked) = self.dispatch(&tool, kernel, entry, &mut state, parked);
        let checkin = self.tasks.with(|table| table.checkin(tid, state, parked));
        let disposition = result?;
        checkin?;
        Ok(disposition)
    }

    fn dispatch<K: KernelServices>(
        &self,
        tool: &Arc<T>,
        kernel: &mut K,
        entry: SyscallEntry,
        state: &mut T::ThreadState,
        parked: Option<Parked>,
    ) -> (Result<Disposition, NarfFatal>, Option<Parked>) {
        let request = entry.request;
        let mut guest = NarfGuest::new(self, kernel, tool, state, Some(request));
        if entry.park_reexecution {
            match parked {
                None => return (Err(NarfFatal::UnexpectedReexecution), None),
                Some(parked) if !parked.entry.same_call(&request) => {
                    return (
                        Err(NarfFatal::ReexecutionMismatch {
                            parked: parked.entry,
                            reexecuted: request,
                        }),
                        None,
                    );
                }
                Some(parked) => guest.redo(parked),
            }
            return settle(guest, Poll::Pending);
        }
        let sysno =
            Sysno::new(request.linux_number() as usize).filter(|sysno| self.is_subscribed(*sysno));
        let Some(sysno) = sysno else {
            guest.tail(request);
            return settle(guest, Poll::Pending);
        };
        let args = request.args;
        let syscall = Syscall::from_raw(
            sysno,
            SyscallArgs::new(
                args[0] as usize,
                args[1] as usize,
                args[2] as usize,
                args[3] as usize,
                args[4] as usize,
                args[5] as usize,
            ),
        );
        let poll = {
            let mut future = tool.handle_syscall_event(&mut guest, syscall);
            poll_once(future.as_mut())
        };
        settle(guest, poll)
    }

    /// Runs the Tool's `handle_thread_start` for the current task, which the
    /// kernel calls before the task first enters user mode.
    pub fn handle_thread_start<K: KernelServices>(
        &self,
        kernel: &mut K,
    ) -> Result<LifecycleOutcome, NarfFatal> {
        self.lifecycle(kernel, |tool, guest| {
            let mut future = tool.handle_thread_start(guest);
            poll_once(future.as_mut()).map(|result| result.map_err(NarfFatal::Tool))
        })
    }

    /// Runs the Tool's `handle_post_exec` for the current task, which the
    /// kernel calls after a successful exec and before the new image runs.
    pub fn handle_post_exec<K: KernelServices>(
        &self,
        kernel: &mut K,
    ) -> Result<LifecycleOutcome, NarfFatal> {
        self.lifecycle(kernel, |tool, guest| {
            let mut future = tool.handle_post_exec(guest);
            poll_once(future.as_mut()).map(|result| result.map_err(NarfFatal::PostExec))
        })
    }

    fn lifecycle<K, F>(&self, kernel: &mut K, run: F) -> Result<LifecycleOutcome, NarfFatal>
    where
        K: KernelServices,
        F: FnOnce(&T, &mut NarfGuest<'_, T, K, L>) -> Poll<Result<(), NarfFatal>>,
    {
        let tid = kernel.tid();
        let Checkout {
            tool, mut state, ..
        } = self.tasks.with(|table| table.checkout(tid))?;
        let result = {
            let mut guest = NarfGuest::new(self, kernel, &tool, &mut state, None);
            let poll = run(&tool, &mut guest);
            match (guest.fatal.take(), poll, guest.terminal.take()) {
                (Some(fatal), _, _) => Err(fatal),
                (None, Poll::Ready(Ok(())), _) => Ok(LifecycleOutcome::Continue),
                (None, Poll::Ready(Err(fatal)), _) => Err(fatal),
                (None, Poll::Pending, Some(terminal)) => match terminal.outcome {
                    NarfSyscallOutcome::ContextManaged => Ok(LifecycleOutcome::ContextManaged),
                    NarfSyscallOutcome::Returned(_) => Err(NarfFatal::TailInjectOutsideSyscall),
                },
                (None, Poll::Pending, None) => Err(NarfFatal::ToolSuspended),
            }
        };
        let checkin = self.tasks.with(|table| table.checkin(tid, state, None));
        let outcome = result?;
        checkin?;
        Ok(outcome)
    }

    /// Tears down thread `tid` after the kernel has finished it.
    ///
    /// Runs `on_exit_thread` with the thread's state, and `on_exit_process`
    /// when it was its process's last thread. Each runs at most once: the
    /// thread leaves the table before either hook runs, so a repeated exit
    /// reports [`NarfFatal::UnknownTask`] and runs nothing. The teardown
    /// completes even if a hook fails; the first failure is returned.
    pub fn task_exited(&self, tid: Pid, status: ExitStatus) -> Result<TaskExit, NarfFatal> {
        let (removal, state) = self.tasks.with(|table| table.remove_thread(tid))?;
        let Removal { tool, pid, last } = removal;
        let rpc = DirectRpc {
            host: self,
            from: tid,
        };
        let thread_result = {
            let mut future = tool.on_exit_thread(tid, &rpc, state, status);
            exit_result(poll_once(future.as_mut()))
        };
        if !last {
            return thread_result.map(|()| TaskExit {
                process_exited: false,
            });
        }
        let process_result = match Arc::try_unwrap(tool) {
            Ok(tool) => {
                let mut future = tool.on_exit_process(pid, &rpc, status);
                exit_result(poll_once(future.as_mut()))
            }
            Err(_) => Err(NarfFatal::ProcessToolShared(pid)),
        };
        thread_result.and(process_result).map(|()| TaskExit {
            process_exited: true,
        })
    }
}

fn exit_result(poll: Poll<Result<(), Error>>) -> Result<(), NarfFatal> {
    match poll {
        Poll::Ready(Ok(())) => Ok(()),
        Poll::Ready(Err(error)) => Err(NarfFatal::Tool(error)),
        Poll::Pending => Err(NarfFatal::ToolSuspended),
    }
}

/// Turns one polled Tool callback into the kernel's disposition.
///
/// A recorded fatal error wins over anything the Tool returned. A pending
/// future is accepted only if it made its terminal transition.
fn settle<T, K, L>(
    mut guest: NarfGuest<'_, T, K, L>,
    poll: Poll<Result<i64, Error>>,
) -> (Result<Disposition, NarfFatal>, Option<Parked>)
where
    T: Tool,
    K: KernelServices,
    L: TaskLock<TaskTable<T>>,
{
    if let Some(fatal) = guest.fatal.take() {
        return (Err(fatal), None);
    }
    match poll {
        Poll::Ready(Ok(value)) => (Ok(Disposition::Complete(value)), None),
        Poll::Ready(Err(error)) => match error.into_errno() {
            Ok(errno) => (errno_result(errno).map(Disposition::Complete), None),
            Err(error) => (Err(NarfFatal::Tool(error)), None),
        },
        Poll::Pending => match guest.terminal.take() {
            Some(Terminal {
                outcome: NarfSyscallOutcome::Returned(value),
                ..
            }) => (Ok(Disposition::Complete(value)), None),
            Some(Terminal {
                outcome: NarfSyscallOutcome::ContextManaged,
                parked,
            }) => (Ok(Disposition::ContextManaged), parked),
            None => (Err(NarfFatal::ToolSuspended), None),
        },
    }
}

/// Global RPC for exit hooks: a direct call on the singleton, attributed to
/// the exiting thread as the ptrace backend's `WrappedFrom` does.
struct DirectRpc<'a, T: Tool, L> {
    host: &'a NarfToolHost<T, L>,
    from: Pid,
}

#[async_trait]
impl<T, L> GlobalRPC<T::GlobalState> for DirectRpc<'_, T, L>
where
    T: Tool,
    L: TaskLock<TaskTable<T>>,
{
    async fn send_rpc(
        &self,
        message: <T::GlobalState as GlobalTool>::Request,
    ) -> <T::GlobalState as GlobalTool>::Response {
        self.host.global.receive_rpc(self.from, message).await
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        &self.host.config
    }
}
