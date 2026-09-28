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
use core::any::TypeId;
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
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;

use crate::LINUX_MAX_ERRNO;
use crate::NarfSyscallOutcome;
use crate::NarfSyscallRequest;
use crate::SyscallEntry;
use crate::guest::CallState;
use crate::guest::Frame;
use crate::guest::FrameSlot;
use crate::guest::NarfGuest;
use crate::guest::Terminal;
use crate::guest::interrupted;
use crate::services::CreatedTask;
use crate::services::CreatedTaskKind;
use crate::services::KernelServices;
use crate::services::RepollWait;

type Config<T> = <<T as Tool>::GlobalState as GlobalTool>::Config;

/// A named reason the core stopped instead of guessing.
///
/// Every variant is fatal to the run: the kernel must stop the hosted process
/// tree rather than resume the task.
pub enum NarfFatal {
    /// The Tool's future was pending without having made a terminal
    /// transition and without awaiting a parked inject, where the core
    /// cannot poll it again: the kernel could not let other tasks run
    /// ([`KernelServices::wait_for_repoll`]), or the future belongs to a
    /// hook that is polled once. So it waits for something the core will
    /// never deliver, and the core never polls it again.
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
    /// A non-tail `inject` of this wire number parked the task where the
    /// Tool's continuation cannot be kept: in a lifecycle callback, which has
    /// no guest syscall for the kernel to re-execute, or in a Tool that
    /// completed without awaiting the parked inject.
    InjectParked {
        /// Wire number of the injected request.
        number: u32,
    },
    /// A Tool whose parked inject was interrupted (the task left the parked
    /// syscall for another context, such as a signal handler) tried to run a
    /// syscall. Its inject returned `ERESTARTSYS`; nothing may run on its
    /// behalf in the new context.
    TransitionAfterInterruption,
    /// A suspended Tool future was resumed by a kernel whose memory accessor
    /// type differs from the one it was started with.
    ContinuationKernelMismatch,
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
            Self::TransitionAfterInterruption => f.write_str("TransitionAfterInterruption"),
            Self::ContinuationKernelMismatch => f.write_str("ContinuationKernelMismatch"),
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

type ToolFuture = Pin<Box<dyn Future<Output = Result<i64, Error>> + Send>>;

/// A Tool future suspended in a non-tail inject whose syscall parked the
/// task, kept until the kernel re-executes that syscall.
struct Continuation {
    future: ToolFuture,
    slot: Arc<FrameSlot>,
    /// The callback's bookkeeping; `call.awaiting` names the parked syscall.
    call: CallState,
    /// The memory accessor type the future's guest was built for.
    memory: TypeId,
}

/// What a task left behind when its last callback parked it.
enum Suspended {
    /// A tail transition parked; re-execution re-issues it.
    Parked(Parked),
    /// A non-tail inject parked; re-execution re-issues it and resumes the
    /// Tool with its value.
    Continuation(Box<Continuation>),
}

struct ProcessEntry<T> {
    tool: Arc<T>,
    threads: usize,
}

struct ThreadEntry<T: Tool> {
    pid: Pid,
    /// `None` while the thread is inside a callback.
    state: Option<T::ThreadState>,
    parked: Option<Suspended>,
}

/// Per-process Tools and per-thread states, keyed by the kernel's IDs.
pub struct TaskTable<T: Tool> {
    processes: BTreeMap<i32, ProcessEntry<T>>,
    threads: BTreeMap<i32, ThreadEntry<T>>,
}

struct Checkout<T: Tool> {
    tool: Arc<T>,
    state: T::ThreadState,
    parked: Option<Suspended>,
}

/// A removed thread's process share, its state, and what it left suspended.
type Removed<T> = (Removal<T>, <T as Tool>::ThreadState, Option<Suspended>);

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
        parked: Option<Suspended>,
    ) -> Result<(), NarfFatal> {
        let thread = self
            .threads
            .get_mut(&tid.as_raw())
            .ok_or(NarfFatal::UnknownTask(tid))?;
        thread.state = Some(state);
        thread.parked = parked;
        Ok(())
    }

    fn remove_thread(&mut self, tid: Pid) -> Result<Removed<T>, NarfFatal> {
        let thread = self
            .threads
            .get(&tid.as_raw())
            .ok_or(NarfFatal::UnknownTask(tid))?;
        if thread.state.is_none() {
            return Err(NarfFatal::ExitDuringCallback(tid));
        }
        let thread = self.threads.remove(&tid.as_raw()).expect("present above");
        let state = thread.state.expect("checked above");
        let suspended = thread.parked;
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
        Ok((removal, state, suspended))
    }
}

/// Polls `future` exactly once with a waker that does nothing.
///
/// Nothing ever wakes a Tool future. The core polls one again only when the
/// kernel re-executes the syscall a parked inject awaits, or after the kernel
/// let other tasks run while it waited for one of them ([`poll_repolling`]).
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
    /// Builds each hosted process's Tool (see [`Self::with_tool_constructor`]).
    new_tool: fn(Pid, &Config<T>) -> T,
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
            new_tool: T::new,
            tasks: L::new(TaskTable::new()),
        })
    }

    /// Builds every hosted process's Tool with `new_tool` instead of
    /// `T::new`, from the same process ID and configuration.
    ///
    /// For a backend that configures each Tool it hosts the same way, as
    /// reverie-dbt gives counter2 a thread-exit reporter that writes to its
    /// runtime's output.
    pub fn with_tool_constructor(mut self, new_tool: fn(Pid, &Config<T>) -> T) -> Self {
        self.new_tool = new_tool;
        self
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
        let tool = Arc::new((self.new_tool)(pid, &self.config));
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
                let tool = Arc::new((self.new_tool)(created.pid, &self.config));
                let state = tool.init_thread_state(created.tid, Some((parent_tid, parent_state)));
                self.tasks.with(|table| {
                    table.insert_process(created.pid, tool)?;
                    table.insert_thread(created.tid, created.pid, state)
                })
            }
        }
    }
}

/// Callback dispatch. A Tool future can outlive one callback, so it and the
/// guest it owns are `'static`.
impl<T, L> NarfToolHost<T, L>
where
    T: Tool + 'static,
    L: TaskLock<TaskTable<T>> + 'static,
{
    /// Handles one interceptor entry for the current task.
    ///
    /// A new subscribed syscall is delivered to the Tool's
    /// `handle_syscall_event`. An unsubscribed syscall, or one whose number
    /// Reverie does not know, runs natively through the core so that created
    /// tasks are still registered. A park re-execution re-issues the parked
    /// transition without calling the Tool again.
    ///
    /// The Tool's future is polled with a waker that does nothing. While it
    /// is pending without a terminal transition, a parked inject or a
    /// failure, it is waiting for another task (through the global state):
    /// the host asks the kernel to let other tasks run
    /// ([`KernelServices::wait_for_repoll`]) and polls it again, within this
    /// entry, until it finishes, makes its terminal transition, parks, or the
    /// task is killed. A killed task's future is dropped and the entry
    /// returns [`Disposition::ContextManaged`]. If the kernel cannot wait,
    /// the future fails closed with [`NarfFatal::ToolSuspended`].
    ///
    /// The future may stay pending across entries in exactly one case: a
    /// non-tail `inject` whose syscall parked the task. The host keeps the
    /// future, returns [`Disposition::ContextManaged`], and at the kernel's
    /// re-execution of the parked syscall re-issues it and polls the future
    /// again with its value. If the task's next entry is not that
    /// re-execution (a signal handler ran instead), the inject returns
    /// `ERESTARTSYS` as under ptrace, the future is polled to completion
    /// without any further syscall, its result is discarded (the kernel
    /// restarts the guest's syscall after the handler), and the new entry is
    /// handled normally.
    pub fn handle_syscall<K>(
        &self,
        kernel: &mut K,
        entry: SyscallEntry,
    ) -> Result<Disposition, NarfFatal>
    where
        K: KernelServices,
        K::Memory: 'static,
    {
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

    fn dispatch<M: MemoryAccess + Send + 'static>(
        &self,
        tool: &Arc<T>,
        kernel: &mut dyn KernelServices<Memory = M>,
        entry: SyscallEntry,
        state: &mut T::ThreadState,
        suspended: Option<Suspended>,
    ) -> (Result<Disposition, NarfFatal>, Option<Suspended>) {
        let parked = match suspended {
            None => None,
            Some(Suspended::Parked(parked)) => Some(parked),
            Some(Suspended::Continuation(continuation)) => {
                if entry.park_reexecution {
                    return self.resume(tool, kernel, entry.request, state, *continuation);
                }
                if let Err(fatal) = self.interrupt(tool, kernel, state, *continuation) {
                    return (Err(fatal), None);
                }
                None
            }
        };
        self.dispatch_new(tool, kernel, entry, state, parked)
    }

    fn dispatch_new<M: MemoryAccess + Send + 'static>(
        &self,
        tool: &Arc<T>,
        kernel: &mut dyn KernelServices<Memory = M>,
        entry: SyscallEntry,
        state: &mut T::ThreadState,
        parked: Option<Parked>,
    ) -> (Result<Disposition, NarfFatal>, Option<Suspended>) {
        let request = entry.request;
        let mut frame = Frame {
            host: self,
            kernel,
            tool,
            thread_state: state,
            call: CallState::new(Some(request)),
        };
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
                Some(parked) => frame.redo(parked),
            }
            return settle(frame.call, Poll::Pending);
        }
        let sysno =
            Sysno::new(request.linux_number() as usize).filter(|sysno| self.is_subscribed(*sysno));
        let Some(sysno) = sysno else {
            frame.tail(request);
            return settle(frame.call, Poll::Pending);
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
        let slot = Arc::new(FrameSlot::default());
        let guest = NarfGuest::<T, L, M>::new(slot.clone());
        let tool = tool.clone();
        let mut future: ToolFuture = Box::pin(async move {
            let mut guest = guest;
            tool.handle_syscall_event(&mut guest, syscall).await
        });
        match poll_repolling(&mut frame, &slot, &mut future) {
            Some(poll) => finish(frame, poll, future, slot),
            None => killed(frame, future, slot),
        }
    }

    /// Resumes a Tool suspended in a parked inject at the kernel's
    /// re-execution of `reexecuted`.
    fn resume<M: MemoryAccess + Send + 'static>(
        &self,
        tool: &Arc<T>,
        kernel: &mut dyn KernelServices<Memory = M>,
        reexecuted: NarfSyscallRequest,
        state: &mut T::ThreadState,
        continuation: Continuation,
    ) -> (Result<Disposition, NarfFatal>, Option<Suspended>) {
        let Continuation {
            mut future,
            slot,
            call,
            memory,
        } = continuation;
        if memory != TypeId::of::<M>() {
            // No frame of the future's type exists to drop it under.
            return (Err(NarfFatal::ContinuationKernelMismatch), None);
        }
        let mut frame = Frame {
            host: self,
            kernel,
            tool,
            thread_state: state,
            call,
        };
        let poll = if frame.resume_awaited(reexecuted) {
            match poll_repolling(&mut frame, &slot, &mut future) {
                Some(poll) => poll,
                None => return killed(frame, future, slot),
            }
        } else {
            Poll::Pending
        };
        finish(frame, poll, future, slot)
    }

    /// Ends a Tool suspended in a parked inject that the task left for
    /// another context: the inject returns `ERESTARTSYS`, and the Tool must
    /// complete in this one poll without running any syscall.
    fn interrupt<M: MemoryAccess + Send + 'static>(
        &self,
        tool: &Arc<T>,
        kernel: &mut dyn KernelServices<Memory = M>,
        state: &mut T::ThreadState,
        continuation: Continuation,
    ) -> Result<(), NarfFatal> {
        let Continuation {
            mut future,
            slot,
            mut call,
            memory,
        } = continuation;
        if memory != TypeId::of::<M>() {
            return Err(NarfFatal::ContinuationKernelMismatch);
        }
        call.awaiting = None;
        call.interrupted = true;
        call.resume = Some(interrupted());
        let mut frame = Frame {
            host: self,
            kernel,
            tool,
            thread_state: state,
            call,
        };
        let poll = slot.enter(&mut frame, move || {
            let poll = poll_once(future.as_mut());
            drop(future);
            poll
        });
        if let Some(fatal) = frame.call.fatal.take() {
            return Err(fatal);
        }
        match poll {
            // The kernel restarts the guest's syscall after the other
            // context returns; the Tool sees it then as a new syscall.
            Poll::Ready(Ok(_)) => Ok(()),
            Poll::Ready(Err(error)) => error.into_errno().map(|_| ()).map_err(NarfFatal::Tool),
            Poll::Pending => Err(NarfFatal::ToolSuspended),
        }
    }

    /// Runs the Tool's `handle_thread_start` for the current task, which the
    /// kernel calls before the task first enters user mode.
    pub fn handle_thread_start<K>(&self, kernel: &mut K) -> Result<LifecycleOutcome, NarfFatal>
    where
        K: KernelServices,
        K::Memory: 'static,
    {
        self.lifecycle(kernel, |tool, guest| {
            let mut future = tool.handle_thread_start(guest);
            poll_once(future.as_mut()).map(|result| result.map_err(NarfFatal::Tool))
        })
    }

    /// Runs the Tool's `handle_post_exec` for the current task, which the
    /// kernel calls after a successful exec and before the new image runs.
    pub fn handle_post_exec<K>(&self, kernel: &mut K) -> Result<LifecycleOutcome, NarfFatal>
    where
        K: KernelServices,
        K::Memory: 'static,
    {
        self.lifecycle(kernel, |tool, guest| {
            let mut future = tool.handle_post_exec(guest);
            poll_once(future.as_mut()).map(|result| result.map_err(NarfFatal::PostExec))
        })
    }

    fn lifecycle<K, F>(&self, kernel: &mut K, run: F) -> Result<LifecycleOutcome, NarfFatal>
    where
        K: KernelServices,
        K::Memory: 'static,
        F: FnOnce(&T, &mut NarfGuest<T, L, K::Memory>) -> Poll<Result<(), NarfFatal>>,
    {
        let tid = kernel.tid();
        let Checkout {
            tool, mut state, ..
        } = self.tasks.with(|table| table.checkout(tid))?;
        let result = {
            let slot = Arc::new(FrameSlot::default());
            let mut guest = NarfGuest::new(slot.clone());
            let mut frame = Frame {
                host: self,
                kernel: kernel as &mut dyn KernelServices<Memory = K::Memory>,
                tool: &tool,
                thread_state: &mut state,
                call: CallState::new(None),
            };
            // The future borrows `guest` and is dropped inside the poll.
            let poll = slot.enter(&mut frame, || run(&tool, &mut guest));
            let call = &mut frame.call;
            match (call.fatal.take(), poll, call.terminal.take()) {
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
}

impl<T, L> NarfToolHost<T, L>
where
    T: Tool,
    L: TaskLock<TaskTable<T>>,
{
    /// Tears down thread `tid` after the kernel has finished it.
    ///
    /// Runs `on_exit_thread` with the thread's state and `status`, and
    /// `on_exit_process` with `process_status` when it was its process's last
    /// thread. `status` is the thread's own exit status (its `exit` or
    /// `exit_group` code, or its group's status when the group's exit or a
    /// signal ended it); `process_status` is the status `wait4` reports for
    /// the process (the first group exit's status, else the last thread's
    /// own), as reverie-ptrace passes the thread-group leader's wait status
    /// to `on_exit_process`. The two differ when, for example, a thread
    /// calls `exit(5)` and the leader later calls `exit_group(7)`: the thread
    /// gets 5, the leader and the process 7. `process_status` is ignored for
    /// a thread that is not the last. Each
    /// runs at most once: the
    /// thread leaves the table before either hook runs, so a repeated exit
    /// reports [`NarfFatal::UnknownTask`] and runs nothing. The teardown
    /// completes even if a hook fails; the first failure is returned.
    ///
    /// A Tool future suspended in a parked inject of the exiting thread is
    /// dropped first, without being polled, so that its share of the
    /// process's Tool is released before `on_exit_process`. It is dropped
    /// outside any poll, so a Tool whose drop glue calls a Guest method
    /// panics.
    pub fn task_exited(
        &self,
        tid: Pid,
        status: ExitStatus,
        process_status: ExitStatus,
    ) -> Result<TaskExit, NarfFatal> {
        let (removal, state, suspended) = self.tasks.with(|table| table.remove_thread(tid))?;
        drop(suspended);
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
                let mut future = tool.on_exit_process(pid, &rpc, process_status);
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

/// Polls a new or resumed Tool future, and polls it again each time the
/// kernel let other tasks run.
///
/// A future pending without a terminal transition, a parked inject or a
/// failure is waiting for another task, so the core asks the kernel to wait
/// ([`KernelServices::wait_for_repoll`]) and polls it again. The wait runs
/// outside [`FrameSlot::enter`], so no frame is published while the task is
/// switched out. Returns the last poll, or `None` if the task was killed
/// during a wait.
fn poll_repolling<T: Tool, L, M: MemoryAccess + Send>(
    frame: &mut Frame<'_, T, L, M>,
    slot: &FrameSlot,
    future: &mut ToolFuture,
) -> Option<Poll<Result<i64, Error>>> {
    loop {
        let poll = slot.enter(frame, || poll_once(future.as_mut()));
        let call = &frame.call;
        if poll.is_ready()
            || call.fatal.is_some()
            || call.awaiting.is_some()
            || call.terminal.is_some()
        {
            return Some(poll);
        }
        match frame.kernel.wait_for_repoll() {
            RepollWait::Yielded => {}
            RepollWait::Killed => return None,
            RepollWait::Unsupported => return Some(poll),
        }
    }
}

/// Ends a callback whose task was killed while its Tool future waited: the
/// kernel owns the task's context, the future is dropped, and the entry
/// returns [`Disposition::ContextManaged`].
///
/// The terminal is recorded before the drop, so [`Frame::execute`] refuses
/// any transition from then on.
fn killed<T: Tool, L, M>(
    mut frame: Frame<'_, T, L, M>,
    future: ToolFuture,
    slot: Arc<FrameSlot>,
) -> (Result<Disposition, NarfFatal>, Option<Suspended>) {
    frame.call.terminal = Some(Terminal {
        outcome: NarfSyscallOutcome::ContextManaged,
        parked: None,
    });
    slot.enter(&mut frame, move || drop(future));
    settle(frame.call, Poll::Pending)
}

/// Keeps a Tool future that awaits a parked inject, or drops it under the
/// frame and settles the callback.
fn finish<T, L, M>(
    mut frame: Frame<'_, T, L, M>,
    poll: Poll<Result<i64, Error>>,
    future: ToolFuture,
    slot: Arc<FrameSlot>,
) -> (Result<Disposition, NarfFatal>, Option<Suspended>)
where
    T: Tool,
    M: 'static,
{
    if frame.call.fatal.is_none() && poll.is_pending() && frame.call.awaiting.is_some() {
        let continuation = Continuation {
            future,
            slot,
            call: frame.call,
            memory: TypeId::of::<M>(),
        };
        return (
            Ok(Disposition::ContextManaged),
            Some(Suspended::Continuation(Box::new(continuation))),
        );
    }
    // Drop glue may still reach the guest, so the frame stays published.
    slot.enter(&mut frame, move || drop(future));
    settle(frame.call, poll)
}

/// Turns one polled Tool callback into the kernel's disposition.
///
/// A recorded fatal error wins over anything the Tool returned. A pending
/// future is accepted only if it made its terminal transition.
fn settle(
    mut call: CallState,
    poll: Poll<Result<i64, Error>>,
) -> (Result<Disposition, NarfFatal>, Option<Suspended>) {
    if let Some(fatal) = call.fatal.take() {
        return (Err(fatal), None);
    }
    if let Some(parked) = call.awaiting.take() {
        // The Tool finished (or gave up) without awaiting the inject that
        // parked the task; the kernel still owns that syscall.
        let number = match parked.redo {
            Redo::Injected(request) => request.number,
            Redo::Original | Redo::Nothing => parked.entry.number,
        };
        return (Err(NarfFatal::InjectParked { number }), None);
    }
    match poll {
        Poll::Ready(Ok(value)) => (Ok(Disposition::Complete(value)), None),
        Poll::Ready(Err(error)) => match error.into_errno() {
            Ok(errno) => (errno_result(errno).map(Disposition::Complete), None),
            Err(error) => (Err(NarfFatal::Tool(error)), None),
        },
        Poll::Pending => match call.terminal.take() {
            Some(Terminal {
                outcome: NarfSyscallOutcome::Returned(value),
                ..
            }) => (Ok(Disposition::Complete(value)), None),
            Some(Terminal {
                outcome: NarfSyscallOutcome::ContextManaged,
                parked,
            }) => (
                Ok(Disposition::ContextManaged),
                parked.map(Suspended::Parked),
            ),
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
