/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! [`reverie::Guest`] over [`KernelServices`].
//!
//! A Tool future owns its [`NarfGuest`], so the future can outlive the
//! interceptor call that started it: when a non-tail `inject` parks the task,
//! the host keeps the future and resumes it at the kernel's re-execution
//! entry. Everything the guest reaches through (the host, the current task's
//! [`KernelServices`], the thread state and the callback's bookkeeping) lives
//! in a [`Frame`] on the host's stack, which the host publishes to the guest
//! through a [`FrameSlot`] for the duration of one poll only.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::marker::PhantomData;
use core::ptr;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::Ordering;
use core::task::Poll;

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
use reverie::syscalls::MemoryAccess;
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

/// The value an inject interrupted while parked returns to the Tool, as the
/// ptrace backend returns it for a signal that interrupts an injected syscall
/// (`reverie-ptrace/src/task.rs`, the `sig != Signal::SIGTRAP` branch of the
/// injected-syscall wait).
pub(crate) fn interrupted() -> i64 {
    -i64::from(Errno::ERESTARTSYS.into_raw())
}

/// One Tool callback's bookkeeping. It survives a park together with the
/// Tool's suspended future.
pub(crate) struct CallState {
    /// The intercepted request, or `None` for a lifecycle callback.
    pub(crate) original: Option<NarfSyscallRequest>,
    original_consumed: bool,
    stack_flag: Arc<AtomicBool>,
    pub(crate) terminal: Option<Terminal>,
    pub(crate) fatal: Option<NarfFatal>,
    /// A non-tail inject parked the task; the Tool awaits its value.
    pub(crate) awaiting: Option<Parked>,
    /// The value the awaiting inject returns on the next poll.
    pub(crate) resume: Option<i64>,
    /// The parked inject was interrupted: the task now runs a different
    /// context, so no further transition may run for this callback.
    pub(crate) interrupted: bool,
}

impl CallState {
    pub(crate) fn new(original: Option<NarfSyscallRequest>) -> Self {
        Self {
            original,
            original_consumed: false,
            stack_flag: Arc::new(AtomicBool::new(false)),
            terminal: None,
            fatal: None,
            awaiting: None,
            resume: None,
            interrupted: false,
        }
    }

    fn fail(&mut self, fatal: NarfFatal) {
        self.fatal.get_or_insert(fatal);
    }
}

/// What a non-tail inject produced.
pub(crate) enum Injected {
    /// The request returned this value.
    Returned(i64),
    /// The request parked the task; its value arrives at re-execution.
    Awaiting,
    /// The callback ended: a terminal transition or a fatal error.
    Stopped,
}

/// Everything a Tool callback may touch while the host polls it within one
/// interceptor entry, borrowed from the host's stack; the host publishes it
/// for one poll at a time.
///
/// No code replaces a reference field of a published frame; the guest only
/// calls through them and mutates `call`.
pub(crate) struct Frame<'a, T: Tool, L, M> {
    pub(crate) host: &'a NarfToolHost<T, L>,
    pub(crate) kernel: &'a mut dyn KernelServices<Memory = M>,
    pub(crate) tool: &'a Arc<T>,
    pub(crate) thread_state: &'a mut T::ThreadState,
    pub(crate) call: CallState,
}

impl<T, L, M> Frame<'_, T, L, M>
where
    T: Tool,
    L: TaskLock<TaskTable<T>>,
    M: MemoryAccess + Send,
{
    fn fail(&mut self, fatal: NarfFatal) {
        self.call.fail(fatal);
    }

    /// Runs `request` through the kernel, as the original if it is the
    /// not-yet-run intercepted syscall and as an injection otherwise.
    ///
    /// Returns the outcome and what re-executing it would mean, or `None` if
    /// the callback has failed.
    fn execute(&mut self, request: NarfSyscallRequest) -> Option<(NarfSyscallOutcome, Redo)> {
        if self.call.interrupted {
            self.fail(NarfFatal::TransitionAfterInterruption);
            return None;
        }
        if self.call.fatal.is_some() || self.call.terminal.is_some() || self.call.awaiting.is_some()
        {
            self.fail(NarfFatal::TransitionAfterTerminal);
            return None;
        }
        let original = self
            .call
            .original
            .filter(|original| original.same_call(&request));
        let result = match original {
            Some(_) if !self.call.original_consumed => {
                self.call.original_consumed = true;
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
        let entry = self.call.original?;
        match redo {
            Redo::Nothing => None,
            _ if ends_context(request.linux_number()) => None,
            redo => Some(Parked { entry, redo }),
        }
    }

    fn end_with(&mut self, request: NarfSyscallRequest, outcome: NarfSyscallOutcome, redo: Redo) {
        let parked = match outcome {
            NarfSyscallOutcome::ContextManaged => self.parked(request, redo),
            NarfSyscallOutcome::Returned(_) => None,
        };
        self.call.terminal = Some(Terminal { outcome, parked });
    }

    /// Runs `request` as the callback's terminal action.
    pub(crate) fn tail(&mut self, request: NarfSyscallRequest) {
        if let Some((outcome, redo)) = self.execute(request) {
            self.end_with(request, outcome, redo);
        }
    }

    /// Re-issues a parked transition on the current kernel entry. Returns
    /// the request that ran, its outcome and what re-executing it would mean.
    fn rerun(&mut self, parked: Parked) -> Option<(NarfSyscallRequest, NarfSyscallOutcome, Redo)> {
        let request = match parked.redo {
            // The re-execution entry's own original has not run yet.
            Redo::Original => {
                self.call.original_consumed = false;
                parked.entry
            }
            // Consume the original so the injected request, not the guest's
            // own syscall, is what re-executes.
            Redo::Injected(request) => {
                self.call.original_consumed = true;
                request
            }
            Redo::Nothing => {
                self.fail(NarfFatal::UnexpectedReexecution);
                return None;
            }
        };
        let (outcome, redo) = self.execute(request)?;
        Some((request, outcome, redo))
    }

    /// Re-issues a parked tail transition at a kernel re-execution entry.
    pub(crate) fn redo(&mut self, parked: Parked) {
        if let Some((request, outcome, redo)) = self.rerun(parked) {
            self.end_with(request, outcome, redo);
        }
    }

    /// Re-issues the transition a suspended inject parked in, at the kernel
    /// re-execution entry for `reexecuted`. Returns whether the Tool's inject
    /// now has its value in `call.resume`.
    pub(crate) fn resume_awaited(&mut self, reexecuted: NarfSyscallRequest) -> bool {
        let Some(parked) = self.call.awaiting.take() else {
            self.fail(NarfFatal::UnexpectedReexecution);
            return false;
        };
        if !parked.entry.same_call(&reexecuted) {
            self.fail(NarfFatal::ReexecutionMismatch {
                parked: parked.entry,
                reexecuted,
            });
            return false;
        }
        let Some((request, outcome, redo)) = self.rerun(parked) else {
            return false;
        };
        match self.inject_outcome(request, outcome, redo) {
            Injected::Returned(value) => {
                self.call.resume = Some(value);
                true
            }
            Injected::Awaiting | Injected::Stopped => false,
        }
    }

    /// Runs `request` for a non-tail inject.
    fn inject_request(&mut self, request: NarfSyscallRequest) -> Injected {
        match self.execute(request) {
            Some((outcome, redo)) => self.inject_outcome(request, outcome, redo),
            None => Injected::Stopped,
        }
    }

    fn inject_outcome(
        &mut self,
        request: NarfSyscallRequest,
        outcome: NarfSyscallOutcome,
        redo: Redo,
    ) -> Injected {
        match outcome {
            NarfSyscallOutcome::Returned(value) => Injected::Returned(value),
            // A task that is ending will not re-execute anything: the
            // callback ends here, even where no guest syscall could have been
            // re-executed.
            NarfSyscallOutcome::ContextManaged if self.kernel.killed() => {
                self.call.terminal = Some(Terminal {
                    outcome,
                    parked: None,
                });
                Injected::Stopped
            }
            NarfSyscallOutcome::ContextManaged
                if !matches!(redo, Redo::Nothing) && !ends_context(request.linux_number()) =>
            {
                // The kernel parked the task and will re-execute the guest's
                // syscall. The host keeps the Tool's future and delivers the
                // value then. A lifecycle or RDTSC callback has no guest
                // syscall to re-execute, so its continuation cannot run.
                match self.parked(request, redo) {
                    Some(parked) => {
                        self.call.awaiting = Some(parked);
                        Injected::Awaiting
                    }
                    None => {
                        self.fail(NarfFatal::InjectParked {
                            number: request.number,
                        });
                        Injected::Stopped
                    }
                }
            }
            NarfSyscallOutcome::ContextManaged => {
                self.call.terminal = Some(Terminal {
                    outcome,
                    parked: None,
                });
                Injected::Stopped
            }
        }
    }
}

/// Where the host publishes the current poll's [`Frame`] to a guest.
#[derive(Default)]
pub(crate) struct FrameSlot(AtomicPtr<()>);

impl FrameSlot {
    /// Publishes `frame` while `f` runs and clears it on every exit,
    /// including unwind. `f` cannot touch `frame`: it is borrowed here.
    pub(crate) fn enter<T: Tool, L, M, R>(
        &self,
        frame: &mut Frame<'_, T, L, M>,
        f: impl FnOnce() -> R,
    ) -> R {
        struct Clear<'s>(&'s AtomicPtr<()>);
        impl Drop for Clear<'_> {
            fn drop(&mut self) {
                self.0.store(ptr::null_mut(), Ordering::Release);
            }
        }
        let previous = self
            .0
            .swap(ptr::from_mut(frame).cast::<()>(), Ordering::AcqRel);
        assert!(
            previous.is_null(),
            "a reverie-narf frame was published twice"
        );
        let _clear = Clear(&self.0);
        f()
    }
}

/// The [`reverie::Guest`] one Tool callback sees.
///
/// The Tool's future owns it, so it can outlive a park; every method reaches
/// the current task through the frame the host publishes for one poll, and
/// panics if called outside a poll. A Tool cannot keep anything borrowed from
/// the guest across `inject(..).await`, because `inject` takes `&mut self`:
///
/// ```compile_fail,E0502
/// use reverie::Guest;
/// use reverie_narf_core::{NarfGuest, TaskLock, TaskTable};
/// async fn held<T, L, M>(guest: &mut NarfGuest<T, L, M>, s: reverie::syscalls::Syscall)
/// where
///     T: reverie::Tool + 'static,
///     L: TaskLock<TaskTable<T>> + 'static,
///     M: reverie::syscalls::MemoryAccess + Send + 'static,
/// {
///     let state = guest.thread_state();
///     let _ = guest.inject(s).await;
///     let _ = state;
/// }
/// ```
///
/// whereas taking the borrow again after the await is fine:
///
/// ```
/// use reverie::Guest;
/// use reverie_narf_core::NarfGuest;
/// use reverie_narf_core::TaskLock;
/// use reverie_narf_core::TaskTable;
/// async fn reborrowed<T, L, M>(guest: &mut NarfGuest<T, L, M>, s: reverie::syscalls::Syscall)
/// where
///     T: reverie::Tool + 'static,
///     L: TaskLock<TaskTable<T>> + 'static,
///     M: reverie::syscalls::MemoryAccess + Send + 'static,
/// {
///     let _ = guest.thread_state();
///     let _ = guest.inject(s).await;
///     let _ = guest.thread_state();
/// }
/// ```
///
/// A borrow taken through the guest can still be held across an await that
/// does not need the guest mutably: `send_rpc(..).await`, or a future of the
/// Tool's own. The host polls such a future again only within the same
/// interceptor entry, after the kernel let other tasks run
/// ([`KernelServices::wait_for_repoll`]), and publishes the same frame
/// again: the host, Tool and thread state it borrows have not moved, and
/// between the two polls the host touches only the frame's call state and
/// kernel, of which no guest method returns a borrow. The host keeps a
/// future from one entry to the next only while it awaits a parked inject,
/// and `&mut self` has ended every such borrow by then.
pub struct NarfGuest<T, L, M> {
    slot: Arc<FrameSlot>,
    types: Types<T, L, M>,
}

/// Names the frame type a guest reads without owning any of its parts.
type Types<T, L, M> = PhantomData<fn() -> (T, L, M)>;

impl<T, L, M> NarfGuest<T, L, M>
where
    T: Tool + 'static,
    L: 'static,
    M: 'static,
{
    pub(crate) fn new(slot: Arc<FrameSlot>) -> Self {
        Self {
            slot,
            types: PhantomData,
        }
    }

    fn published<'s>(&'s self) -> *mut Frame<'s, T, L, M> {
        let frame = self.slot.0.load(Ordering::Acquire);
        assert!(
            !frame.is_null(),
            "a reverie-narf Guest method ran outside the host's poll"
        );
        frame.cast::<Frame<'s, T, L, M>>()
    }

    fn frame(&self) -> &Frame<'_, T, L, M> {
        let frame = self.published();
        // SAFETY: the slot is non-null only inside `FrameSlot::enter`, which
        // the host calls with a `&mut Frame<T, L, M>` it does not touch until
        // `enter` returns and which clears the slot on every exit, including
        // unwind; `published` asserted non-null, so the frame is live for
        // this call. The host publishes only frames of this guest's `T`, `L`
        // and `M` (a stored continuation records `M` and is resumed only with
        // the same type). The returned borrow is tied to `&self`. What
        // `thread_state` and `config` derive from it can outlive this poll,
        // but only across an await that does not need the guest mutably:
        // every method that can park the task takes `&mut self`. So a future
        // holding such a borrow is polled again only within the same entry,
        // with the same frame, whose `host`, `tool` and `thread_state`
        // referents have not moved; between the polls the host touches only
        // `call` and `kernel`, and no guest method returns a borrow of either.
        // Shared borrows here alias only other shared borrows: the exclusive
        // one below needs `&mut self`. Shortening the frame's lifetime
        // parameter is sound because no code replaces a frame's reference
        // fields.
        unsafe { &*frame }
    }

    fn frame_mut(&mut self) -> &mut Frame<'_, T, L, M> {
        let frame = self.published();
        // SAFETY: as for `frame`; `&mut self` excludes every other borrow
        // obtained through this guest, and the host does not touch the frame
        // while it is published.
        unsafe { &mut *frame }
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
impl<T, L, M> GlobalRPC<T::GlobalState> for NarfGuest<T, L, M>
where
    T: Tool + 'static,
    L: TaskLock<TaskTable<T>> + 'static,
    M: MemoryAccess + Send + 'static,
{
    async fn send_rpc(
        &self,
        message: <T::GlobalState as GlobalTool>::Request,
    ) -> <T::GlobalState as GlobalTool>::Response {
        let host = self.frame().host;
        host.global().receive_rpc(self.tid(), message).await
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        self.frame().host.config()
    }
}

#[async_trait]
impl<T, L, M> Guest<T> for NarfGuest<T, L, M>
where
    T: Tool + 'static,
    L: TaskLock<TaskTable<T>> + 'static,
    M: MemoryAccess + Send + 'static,
{
    type Memory = M;
    type Stack = NarfStack<M>;

    fn tid(&self) -> Pid {
        self.frame().kernel.tid()
    }

    fn pid(&self) -> Pid {
        self.frame().kernel.pid()
    }

    fn ppid(&self) -> Option<Pid> {
        self.frame().kernel.ppid()
    }

    fn auxv(&self) -> Auxv {
        self.frame().kernel.auxv()
    }

    fn memory(&self) -> Self::Memory {
        self.frame().kernel.memory()
    }

    fn thread_state_mut(&mut self) -> &mut T::ThreadState {
        self.frame_mut().thread_state
    }

    fn thread_state(&self) -> &T::ThreadState {
        self.frame().thread_state
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        self.frame().kernel.regs()
    }

    async fn stack(&mut self) -> Self::Stack {
        let frame = self.frame();
        let rsp = frame.kernel.regs().rsp;
        NarfStack::new(frame.kernel.memory(), rsp, &frame.call.stack_flag)
    }

    async fn daemonize(&mut self) {
        let frame = self.frame_mut();
        if let Err(errno) = frame.kernel.daemonize() {
            frame.fail(NarfFatal::DaemonizeRefused(errno));
        }
    }

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno> {
        let value = match self.frame_mut().inject_request(request_of(syscall)) {
            Injected::Returned(value) => value,
            // Each poll re-reads the frame the host published for it.
            Injected::Awaiting => {
                core::future::poll_fn(|_| match self.frame_mut().call.resume.take() {
                    Some(value) => Poll::Ready(value),
                    None => Poll::Pending,
                })
                .await
            }
            Injected::Stopped => core::future::pending().await,
        };
        Errno::from_ret(value as usize).map(|value| value as i64)
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        self.frame_mut().tail(request_of(syscall));
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
