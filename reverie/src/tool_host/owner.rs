/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The Tool and task owner shared by in-process interception adapters.

use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering;
use std::collections::HashMap;
use std::io;

use super::CallbackOutcome;
use super::PendingDriver;
use super::TailResult;
use super::drive_callback_with;
use super::drive_ready_with;
use super::sync::SpinGuard;
use super::sync::SpinMutex;
use crate::Error;
use crate::ExitStatus;
use crate::GlobalRPC;
use crate::GlobalTool;
use crate::Guest;
use crate::Pid;
use crate::Tool;

/// An owned connection binding that lends callback-scoped RPC ports.
///
/// An implementation may own a stream, or refer to an adapter's existing
/// protected connection owner. It must never retain a borrowed callback's I/O
/// context. Construction of a port must validate that context and must not
/// substitute a local GlobalTool or another connection on failure.
pub trait CallbackPorts<G: GlobalTool>: Sync {
    /// Physical context valid only during the current intercepted callback.
    type Context<'call>
    where
        Self: 'call;
    /// RPC access borrowed from this connection binding for that callback.
    type Port<'call>: GlobalRPC<G>
    where
        Self: 'call;

    /// Immutable configuration obtained from this connection's real peer.
    fn config(&self) -> &G::Config;
    /// Borrow this connection for the actual callback context.
    fn borrow<'call>(
        &'call self,
        context: Self::Context<'call>,
    ) -> Result<Self::Port<'call>, Error>;
}

/// Physical task identities supplied by the interception adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskIdentity {
    /// Linux task identifier in the runtime's PID namespace.
    pub tid: Pid,
    /// Linux thread-group identifier in the same namespace.
    pub pid: Pid,
}

/// The task domain that an adapter can physically host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionDomain {
    /// Preserve the existing synchronous trap host's process/fork contract.
    ExistingProcess,
    /// Admit only the initial root task; physical birth and exec must refuse.
    InitialRootOnly,
}

/// The kind of task-domain transition requested by a physical syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTransition {
    /// A new process or thread.
    Creation,
    /// Replacement of the current image.
    Exec,
}

/// Classify the physical syscall family without reading guest memory.
pub fn task_transition(number: i64) -> Option<TaskTransition> {
    match number {
        libc::SYS_fork | libc::SYS_vfork | libc::SYS_clone | libc::SYS_clone3 => {
            Some(TaskTransition::Creation)
        }
        libc::SYS_execve | libc::SYS_execveat => Some(TaskTransition::Exec),
        _ => None,
    }
}

/// Shared classification before an adapter performs a physical operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalOperation {
    /// No task-domain transition is involved.
    Ordinary,
    /// The existing trap host's supported plain-fork corridor.
    PlainFork,
    /// The selected domain cannot host this physical task transition.
    Refused {
        /// The physical operation's family, even when its arguments are refused.
        transition: TaskTransition,
        /// Named admission failure.
        reason: &'static str,
    },
}

impl PhysicalOperation {
    /// Whether this operation creates a task, including a refused creation.
    pub fn is_creation(self) -> bool {
        matches!(
            self,
            Self::PlainFork
                | Self::Refused {
                    transition: TaskTransition::Creation,
                    ..
                }
        )
    }

    /// A named refusal, without converting it to a Linux errno.
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Refused { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

/// Classify physical creation/exec without reading guest pointers.
///
/// `clone3_fields` is the adapter's optional, safely read and zero-padded
/// eleven-word clone_args value. InitialRootOnly never needs that read.
pub fn physical_operation(
    domain: AdmissionDomain,
    number: i64,
    args: [u64; 6],
    clone3_fields: Option<&[u64; 11]>,
) -> PhysicalOperation {
    let Some(transition) = task_transition(number) else {
        return PhysicalOperation::Ordinary;
    };
    if domain == AdmissionDomain::InitialRootOnly {
        return PhysicalOperation::Refused {
            transition,
            reason: super::initial_root_refusal(number),
        };
    }
    let plain = match number {
        libc::SYS_fork | libc::SYS_vfork => true,
        libc::SYS_clone => {
            let allowed = (libc::CLONE_CHILD_CLEARTID
                | libc::CLONE_CHILD_SETTID
                | libc::CLONE_PARENT_SETTID) as u64;
            args[1] == 0
                && args[0] & 0xff == libc::SIGCHLD as u64
                && args[0] & !(0xff | allowed) == 0
        }
        libc::SYS_clone3 => clone3_fields.is_some_and(clone_args_are_plain_fork),
        _ => false,
    };
    if plain {
        PhysicalOperation::PlainFork
    } else {
        PhysicalOperation::Refused {
            transition,
            reason: "physical task transition is outside the synchronous trap domain",
        }
    }
}

/// Existing trap plain-fork clone3 rule, over validated physical input bytes.
pub fn clone_args_are_plain_fork(fields: &[u64; 11]) -> bool {
    let allowed =
        (libc::CLONE_CHILD_CLEARTID | libc::CLONE_CHILD_SETTID | libc::CLONE_PARENT_SETTID) as u64;
    fields[0] & !allowed == 0
        && fields[4] == libc::SIGCHLD as u64
        && fields[5] == 0
        && fields[6] == 0
        && fields[8..11].iter().all(|field| *field == 0)
}

/// Whether a terminal syscall ends this task's whole process.
///
/// Both admitted domains refuse thread birth. The main-thread rule is valid
/// under that existing synchronous-process restriction, not arbitrary Linux
/// multithreaded exit semantics.
// TODO-HUMAN-REVIEW(PR-143): Review single-process exit classification.
pub fn is_process_exit(number: i64, identity: TaskIdentity) -> bool {
    // AUTONOMOUS-BOT-IMPLEMENTED
    number == libc::SYS_exit_group || identity.tid == identity.pid
}

/// Progress of the real startup callbacks, kept with their task state.
#[derive(Default)]
struct StartupState {
    thread_started: bool,
    post_exec: bool,
    needs_post_exec: bool,
    restored: bool,
    failed: bool,
}

impl StartupState {
    fn new(root: bool) -> Self {
        Self {
            needs_post_exec: root,
            ..Self::default()
        }
    }

    fn restored() -> Self {
        Self {
            thread_started: true,
            post_exec: true,
            restored: true,
            ..Self::default()
        }
    }

    fn complete(&self) -> bool {
        !self.failed && self.thread_started && (!self.needs_post_exec || self.post_exec)
    }
}

struct Task<T: Tool> {
    state: T::ThreadState,
    startup: StartupState,
}

/// One actual Tool, its task states and one connection binding.
///
/// The two locks retain the existing tool-before-tasks order. A callback lease
/// holds both until its Guest and future have dropped, so exit cannot move a
/// state still borrowed by a pending callback. This synchronous owner does not
/// promise progress for callbacks waiting on a sibling; adapters must not admit
/// a physical task domain that requires such progress.
pub struct ToolOwner<T: Tool, P: CallbackPorts<T::GlobalState>> {
    tool: SpinMutex<Option<T>>,
    tasks: SpinMutex<HashMap<i32, Task<T>>>,
    ports: P,
    root: TaskIdentity,
    domain: AdmissionDomain,
    completion_taken: AtomicBool,
    subscriptions: crate::Subscription,
}

impl<T: Tool, P: CallbackPorts<T::GlobalState>> ToolOwner<T, P> {
    /// Take ownership of the selected Tool and its genuine connection binding.
    pub fn new(tool: T, ports: P, root: TaskIdentity, domain: AdmissionDomain) -> Self {
        let subscriptions = T::subscriptions(ports.config());
        Self {
            subscriptions,
            tool: SpinMutex::new(Some(tool)),
            tasks: SpinMutex::new(HashMap::new()),
            ports,
            root,
            domain,
            completion_taken: AtomicBool::new(false),
        }
    }

    /// The selected Tool's actual event subscriptions.
    pub fn subscriptions(&self) -> &crate::Subscription {
        &self.subscriptions
    }

    /// The stable concrete connection owner, for physical adapter operations.
    pub fn connection(&self) -> &P {
        &self.ports
    }

    /// Try to acquire a complete callback lease without waiting on a sibling.
    pub fn try_borrow<'a>(
        &'a self,
        context: P::Context<'a>,
    ) -> Result<CallbackLease<'a, T, P>, Error> {
        let tool = self.tool.try_lock().ok_or_else(active_callback)?;
        let tasks = self.tasks.try_lock().ok_or_else(active_callback)?;
        self.finish_borrow(tool, tasks, context)
    }

    /// Acquire the existing trap host's synchronous callback lease.
    pub fn borrow<'a>(&'a self, context: P::Context<'a>) -> Result<CallbackLease<'a, T, P>, Error> {
        let tool = self.tool.lock();
        let tasks = self.tasks.lock();
        self.finish_borrow(tool, tasks, context)
    }

    fn finish_borrow<'a>(
        &'a self,
        tool: SpinGuard<'a, Option<T>>,
        tasks: SpinGuard<'a, HashMap<i32, Task<T>>>,
        context: P::Context<'a>,
    ) -> Result<CallbackLease<'a, T, P>, Error> {
        if tool.is_none() {
            return Err(io::Error::other("Tool process has already exited").into());
        }
        let port = self.ports.borrow(context)?;
        Ok(CallbackLease {
            tool,
            tasks,
            port,
            root: self.root,
            current: None,
            domain: self.domain,
            completion_taken: &self.completion_taken,
        })
    }
}

fn active_callback() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "Tool callback is still active")
}

/// Exclusive callback and retirement ownership, including a borrowed RPC port.
pub struct CallbackLease<'a, T: Tool, P: CallbackPorts<T::GlobalState> + 'a> {
    tool: SpinGuard<'a, Option<T>>,
    tasks: SpinGuard<'a, HashMap<i32, Task<T>>>,
    port: P::Port<'a>,
    root: TaskIdentity,
    domain: AdmissionDomain,
    current: Option<TaskIdentity>,
    completion_taken: &'a AtomicBool,
}

/// The actual owner references used to construct a temporary callback Guest.
pub struct GuestParts<'a, T: Tool, R> {
    /// The selected Tool, not a newly constructed substitute.
    pub tool: &'a T,
    /// The one admitted state for this physical task.
    pub state: &'a mut T::ThreadState,
    /// The active physical task identity.
    pub identity: TaskIdentity,
    /// The port borrowed from this owner's connection for this callback.
    pub rpc: &'a R,
}

/// Adapter construction of a callback Guest from the common owner's parts.
///
/// A factory stores physical interception facts only. It must use the supplied
/// state and RPC port; it cannot retain either after the returned Guest drops.
pub trait TaskGuestFactory<T: Tool, R: GlobalRPC<T::GlobalState>> {
    /// Guest borrowing the owner's state, port and this physical factory.
    type Guest<'lease>: Guest<T>
    where
        Self: 'lease,
        R: 'lease;
    /// Adapter-specific ending selected while delivering a staged event.
    type Ending;
    /// Construct a temporary Guest on the supplied actual state and port.
    fn guest<'lease>(
        &'lease mut self,
        parts: GuestParts<'lease, T, R>,
        tail: &'lease TailResult,
    ) -> Self::Guest<'lease>;
    /// Deliver this adapter's staged events between startup callbacks.
    ///
    /// The existing trap implementation can invoke Tool signal callbacks and
    /// choose a signal ending here. That residual signal policy is not claimed
    /// to be shared by adapters that explicitly refuse signal delivery.
    fn after_callback<'lease>(tool: &T, guest: &mut Self::Guest<'lease>) -> Option<Self::Ending>
    where
        Self: 'lease,
        R: 'lease;
}

/// An exact terminal event, before physical termination of the task.
#[derive(Clone, Copy)]
pub enum TerminalEvent {
    /// A Linux exit or exit_group with its unmodified arguments.
    Syscall {
        /// Physical Linux syscall number.
        number: i64,
        /// Original physical arguments.
        args: [u64; 6],
    },
    /// A signal already selected by the adapter's actual signal-delivery policy.
    SignalDeath {
        /// Signal delivered to this process.
        signal: crate::Signal,
        /// Whether that signal produces a core dump under the active policy.
        core_dumped: bool,
    },
}

impl TerminalEvent {
    fn classify(self, identity: TaskIdentity) -> Result<(ExitStatus, bool), Error> {
        match self {
            Self::Syscall { number, args }
                if matches!(number, libc::SYS_exit | libc::SYS_exit_group) =>
            {
                Ok((
                    ExitStatus::Exited((args[0] & 0xff) as i32),
                    is_process_exit(number, identity),
                ))
            }
            Self::SignalDeath {
                signal,
                core_dumped,
            } => Ok((ExitStatus::Signaled(signal, core_dumped), true)),
            Self::Syscall { .. } => {
                Err(io::Error::other("nonterminal syscall cannot retire Tool state").into())
            }
        }
    }
}

impl<'a, T: Tool, P: CallbackPorts<T::GlobalState>> CallbackLease<'a, T, P> {
    /// The actual task selected by admission or fork reseeding.
    pub fn identity(&self) -> Option<TaskIdentity> {
        self.current
    }

    /// Read the live Tool while this lease protects it.
    pub fn tool(&self) -> &T {
        self.tool.as_ref().expect("live Tool lease")
    }

    /// Whether this owner has no admitted task state.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Whether the exact task already has state.
    pub fn contains(&self, tid: Pid) -> bool {
        self.tasks.contains_key(&tid.as_raw())
    }

    /// Inspect existing state without initializing it or changing callbacks.
    pub fn state(&self, tid: Pid) -> Option<&T::ThreadState> {
        self.tasks.get(&tid.as_raw()).map(|task| &task.state)
    }

    /// Initialize a physical task exactly once, without running callbacks yet.
    pub fn admit(&mut self, identity: TaskIdentity) -> Result<(), Error> {
        if self.domain == AdmissionDomain::InitialRootOnly && identity != self.root {
            return Err(
                io::Error::other("initial-root Tool domain refuses another physical task").into(),
            );
        }
        if !self.contains(identity.tid) {
            let state = self.tool().init_thread_state(identity.tid, None);
            self.tasks.insert(
                identity.tid.as_raw(),
                Task {
                    state,
                    startup: StartupState::new(
                        identity.tid == self.root.tid && identity.pid == self.root.pid,
                    ),
                },
            );
        }
        self.current = Some(identity);
        Ok(())
    }

    /// Restore an authenticated continuation whose callbacks already completed.
    ///
    /// This operation cannot produce an initial-root admission proof. The
    /// physical caller remains responsible for authenticating the continuation.
    pub fn restore_started(&mut self, tid: Pid, state: T::ThreadState) -> Result<(), Error> {
        if self.contains(tid) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Tool state already exists for this thread",
            )
            .into());
        }
        if self.domain != AdmissionDomain::ExistingProcess {
            return Err(io::Error::other("initial-root Tool domain refuses restored state").into());
        }
        self.tasks.insert(
            tid.as_raw(),
            Task {
                state,
                startup: StartupState::restored(),
            },
        );
        Ok(())
    }

    /// Roll back a failed authenticated continuation publication.
    pub fn remove_restored(&mut self, tid: Pid) -> Result<(), Error> {
        if !self
            .tasks
            .get(&tid.as_raw())
            .is_some_and(|task| task.startup.restored)
        {
            return Err(io::Error::other("Tool state is not a restored continuation").into());
        }
        self.tasks.remove(&tid.as_raw());
        Ok(())
    }

    /// Run real startup callbacks using a Guest constructed by this lease.
    ///
    /// The startup phase never leaves the owner. An adapter cannot pair it
    /// with a foreign Guest, state or connection to manufacture completion.
    pub fn startup<F: TaskGuestFactory<T, P::Port<'a>>>(
        &mut self,
        factory: &mut F,
        tail: &TailResult,
        driver: &impl PendingDriver,
    ) -> Result<(), StartupFailure<F::Ending>> {
        let identity = self.current.ok_or_else(|| {
            StartupFailure::Tool(io::Error::other("Tool task is not admitted").into())
        })?;
        let task = self
            .tasks
            .get_mut(&identity.tid.as_raw())
            .expect("admitted task state");
        let tool = self.tool.as_ref().expect("live Tool lease");
        let mut guest = factory.guest(
            GuestParts {
                tool,
                state: &mut task.state,
                identity,
                rpc: &self.port,
            },
            tail,
        );
        drive_startup(
            tool,
            &mut guest,
            &mut task.startup,
            tail,
            driver,
            F::after_callback,
        )
    }

    /// Construct a post-startup Guest borrowing only this owner's state/port.
    pub fn guest<'lease, F: TaskGuestFactory<T, P::Port<'a>>>(
        &'lease mut self,
        factory: &'lease mut F,
        tail: &'lease TailResult,
    ) -> Result<(&'lease T, F::Guest<'lease>), Error> {
        let identity = self
            .current
            .ok_or_else(|| io::Error::other("Tool task is not admitted"))?;
        let task = self
            .tasks
            .get_mut(&identity.tid.as_raw())
            .expect("admitted task state");
        if !task.startup.complete() {
            return Err(io::Error::other("Tool task startup is incomplete").into());
        }
        let tool = self.tool.as_ref().expect("live Tool lease");
        let guest = factory.guest(
            GuestParts {
                tool,
                state: &mut task.state,
                identity,
                rpc: &self.port,
            },
            tail,
        );
        Ok((tool, guest))
    }

    /// Reseed a fork child after the adapter has rebound physical RPC ownership.
    ///
    /// The caller must mark the inherited connection for child reconnect before
    /// this call and before driving the child's startup callback. Snapshot
    /// serialization and physical fork remain adapter operations.
    pub fn reseed_fork_child(
        &mut self,
        parent_tid: Pid,
        child: TaskIdentity,
        parent_snapshot: &T::ThreadState,
    ) -> Result<(), Error> {
        if self.domain != AdmissionDomain::ExistingProcess {
            return Err(io::Error::other("initial-root Tool domain refuses fork reseeding").into());
        }
        let inherited = self
            .tasks
            .remove(&parent_tid.as_raw())
            .ok_or_else(|| io::Error::other("fork parent Tool state is absent"))?;
        drop(inherited);
        let tool = T::new(child.pid, self.port.config());
        let state = tool.init_thread_state(child.tid, Some((parent_tid, parent_snapshot)));
        self.tasks.clear();
        self.tasks.insert(
            child.tid.as_raw(),
            Task {
                state,
                startup: StartupState::new(false),
            },
        );
        *self.tool = Some(tool);
        self.current = Some(child);
        Ok(())
    }

    /// Retire the exact task and run thread then process exit callbacks.
    ///
    /// This uses the same callback-valid RPC port. It consumes state only
    /// after all earlier Guest/future borrows have ended. Physical termination
    /// and the adapter's statistics publication happen after this returns.
    pub fn finish_exit(
        &mut self,
        terminal: TerminalEvent,
        driver: &impl PendingDriver,
    ) -> Result<bool, Error> {
        let identity = self
            .current
            .ok_or_else(|| io::Error::other("Tool task is not admitted"))?;
        let (status, process_exit) = terminal.classify(identity)?;
        if process_exit && self.domain == AdmissionDomain::InitialRootOnly && self.tasks.len() != 1
        {
            return Err(
                io::Error::other("initial-root process still owns other task states").into(),
            );
        }
        let task = self
            .tasks
            .remove(&identity.tid.as_raw())
            .ok_or_else(|| io::Error::other("Tool task disappeared before exit"))?;
        drive_ready_with(
            self.tool()
                .on_exit_thread(identity.tid, &self.port, task.state, status),
            driver,
        )?;
        if process_exit {
            let tool = self.tool.take().expect("live Tool lease");
            drive_ready_with(
                tool.on_exit_process(identity.pid, &self.port, status),
                driver,
            )?;
        }
        Ok(process_exit)
    }

    /// Release Tool/state locks and attest completion of real root callbacks.
    ///
    /// The affine result carries the very port used by those callbacks. It
    /// cannot be made from Config delivery or restored state and does not itself
    /// send a transport declaration.
    pub fn finish_initial_root(self) -> Result<InitialRootAdmission<P::Port<'a>>, Error> {
        let task = self
            .tasks
            .get(&self.root.tid.as_raw())
            .ok_or_else(|| io::Error::other("initial-root task is absent"))?;
        if self.domain != AdmissionDomain::InitialRootOnly
            || task.startup.restored
            || !task.startup.needs_post_exec
            || !task.startup.complete()
        {
            return Err(io::Error::other("initial-root startup callbacks are incomplete").into());
        }
        self.completion_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| io::Error::other("initial-root completion was already consumed"))?;
        let Self {
            tool,
            tasks,
            port,
            root,
            ..
        } = self;
        drop(tasks);
        drop(tool);
        Ok(InitialRootAdmission { port, root })
    }
}

/// Affine completion from an actually admitted initial-root callback lease.
pub struct InitialRootAdmission<P> {
    port: P,
    root: TaskIdentity,
}

impl<P> InitialRootAdmission<P> {
    /// Consume completion with its original port and physical root identity.
    pub fn into_port(self) -> (P, TaskIdentity) {
        (self.port, self.root)
    }
}

/// A startup callback failed or staged a physical ending.
pub enum StartupFailure<E> {
    /// The Tool returned an actual error.
    Tool(Error),
    /// The callback staged an exit through its Guest.
    Exit {
        /// Native syscall number.
        number: i64,
        /// Exact physical arguments.
        args: [u64; 6],
    },
    /// The callback requested an unsupported transition.
    Unsupported(&'static str),
    /// The adapter delivered a staged physical event that ends the process.
    Physical(E),
}

/// Run thread-start and (for the initial root) post-exec exactly once.
///
/// The adapter's hook preserves physical signal-delivery ordering after each
/// actual callback. It cannot declare a callback complete. Private successor
/// post-exec restoration remains a different authenticated protocol.
fn drive_startup<T: Tool, G: Guest<T>, E>(
    tool: &T,
    guest: &mut G,
    phase: &mut StartupState,
    tail: &TailResult,
    driver: &impl PendingDriver,
    mut after_callback: impl FnMut(&T, &mut G) -> Option<E>,
) -> Result<(), StartupFailure<E>> {
    if phase.failed {
        return Err(StartupFailure::Tool(
            io::Error::other("Tool startup previously failed").into(),
        ));
    }
    let result = (|| {
        if !phase.thread_started {
            startup_result(drive_callback_with(
                tool.handle_thread_start(guest),
                tail,
                driver,
            ))?;
            phase.thread_started = true;
            if let Some(ending) = after_callback(tool, guest) {
                return Err(StartupFailure::Physical(ending));
            }
        }
        if phase.needs_post_exec && !phase.post_exec {
            let outcome = drive_callback_with(tool.handle_post_exec(guest), tail, driver);
            startup_result(match outcome {
                CallbackOutcome::Ready(value) => CallbackOutcome::Ready(value.map_err(Error::from)),
                CallbackOutcome::Exit { number, args } => CallbackOutcome::Exit { number, args },
                CallbackOutcome::Unsupported(what) => CallbackOutcome::Unsupported(what),
            })?;
            phase.post_exec = true;
            if let Some(ending) = after_callback(tool, guest) {
                return Err(StartupFailure::Physical(ending));
            }
        }
        Ok(())
    })();
    if result.is_err() {
        phase.failed = true;
    }
    result
}

fn startup_result<E>(outcome: CallbackOutcome<Result<(), Error>>) -> Result<(), StartupFailure<E>> {
    match outcome {
        CallbackOutcome::Ready(value) => value.map_err(StartupFailure::Tool),
        CallbackOutcome::Exit { number, args } => Err(StartupFailure::Exit { number, args }),
        CallbackOutcome::Unsupported(what) => Err(StartupFailure::Unsupported(what)),
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod contract_tests;
