/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::future::Future;
use std::future::poll_fn;
use std::pin::Pin;
use std::pin::pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::task::Poll;

use kvm_bindings::kvm_regs;
use kvm_ioctls::VcpuExit;
use reverie::Auxv;
use reverie::DetlogMemoryRegion;
use reverie::DetlogRegionKind;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Never;
use reverie::Pid;
use reverie::SignalEvent;
use reverie::Stack;
use reverie::Subscription;
use reverie::ThreadOwnership;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::Errno;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::SyscallInfo;

use crate::Error;
use crate::GuestMemory;
use crate::KvmBackend;
use crate::Result;
use crate::SyscallRequest;
use crate::VMCALL_SYSCALL_TRANSPORT;
use crate::bootstrap::TOOL_STACK_SIZE;
use crate::bootstrap::process_syscall_return_registers;
use crate::bootstrap::set_user_segment_base;
use crate::bootstrap::stage_process_syscall_return;
use crate::executor::ChildStartCancellation;
#[cfg(test)]
use crate::executor::ChildStartCommand;
use crate::executor::ChildStartGate;
use crate::executor::ElfExecutor;
use crate::executor::PendingSignal;
use crate::executor::ProcessAction;
use crate::executor::conventional_exit_code;
use crate::vm::CompletedSyscallBoundary;
use crate::vm::PageZeroFault;
use crate::vm::ProcessActionContinuation;
use crate::vm::ProcessActionOutcome;

const STACK_CAPACITY: usize = TOOL_STACK_SIZE as usize;

enum HandlerSignal {
    TailInjected {
        result: std::result::Result<i64, Errno>,
        image_replaced: bool,
        process_exited: bool,
    },
    RuntimeError(Error),
}

type SharedHandlerSignal = Arc<Mutex<Option<HandlerSignal>>>;
pub(crate) type SharedChildStarts = Arc<Mutex<Vec<PendingChildStart>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingChildKind {
    ForkProcess(i32),
    ToolThread(i32),
}

pub(crate) enum PendingChildCancellation {
    NewlyCancelled {
        child: PendingChildKind,
        delivery_failed: bool,
    },
    AlreadyStarted,
    AlreadyCancelled,
}

pub(crate) struct PendingChildStart {
    child: PendingChildKind,
    start: ChildStartGate,
}

impl PendingChildStart {
    pub(crate) fn fork_process(pid: i32, start: ChildStartGate) -> Self {
        Self {
            child: PendingChildKind::ForkProcess(pid),
            start,
        }
    }

    pub(crate) fn tool_thread(tid: i32, start: ChildStartGate) -> Self {
        Self {
            child: PendingChildKind::ToolThread(tid),
            start,
        }
    }

    fn start(&self) -> std::result::Result<(), std::sync::mpsc::SendError<()>> {
        self.start.start().map(|_| ())
    }

    fn is_pending(&self) -> bool {
        self.start.is_pending()
    }

    pub(crate) fn cancel(self) -> PendingChildCancellation {
        match self.start.cancel() {
            ChildStartCancellation::NewlyCancelled { delivery_failed } => {
                PendingChildCancellation::NewlyCancelled {
                    child: self.child,
                    delivery_failed,
                }
            }
            ChildStartCancellation::AlreadyStarted => PendingChildCancellation::AlreadyStarted,
            ChildStartCancellation::AlreadyCancelled => PendingChildCancellation::AlreadyCancelled,
        }
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED: Keep root syscalls that share worker state in one backend.
// TODO-HUMAN-REVIEW(PR-173): Review KVM root syscall ownership.
pub(crate) fn is_backend_owned_syscall(number: u64, thread_ownership: ThreadOwnership) -> bool {
    // `futex` ownership follows the thread's `ThreadOwnership`, so it can never
    // disagree with how that thread executes:
    //
    // * `ThreadOwnership::Tool`: every thread — root and worker alike — is
    //   registered in the Tool's (Detcore's) scheduler. `futex` must therefore
    //   route to the Tool so that a join's `FUTEX_WAIT` becomes a logical
    //   scheduler wait woken by the exiting worker's logical `CLONE_CHILD_CLEARTID`
    //   wake. Executing it as a real host futex here deadlocks: the exiting
    //   worker's wake is only simulated inside Detcore and never reaches a real
    //   host futex word, so the waiter sleeps forever.
    // * `ThreadOwnership::Host`: workers run uninstrumented outside the Tool's
    //   scheduler, so the root's futex must use the same host-backed words as
    //   those siblings and stays backend-owned.
    if number == libc::SYS_futex as u64 {
        return thread_ownership.futex_is_host_owned();
    }
    // QEMU's root event loop waits on worker eventfds. KVM syscall
    // injection cannot perform ppoll, so use translated host descriptors in
    // either ownership mode.
    if number == libc::SYS_ppoll as u64 {
        return true;
    }

    // Host-owned workers execute outside the Tool and can create descriptors
    // that the root event loop consumes. Their scalar and vectored reads must
    // therefore use the backend's shared descriptor table. Tool-owned workers,
    // however, are registered with the Tool's scheduler, so their reads must
    // reach Tool::handle_syscall_event. In particular, Detcore makes internal
    // pipes physically nonblocking while keeping them logically blocking; if
    // the backend consumes those reads itself, the implementation-only EAGAIN
    // leaks to the guest instead of entering Detcore's polling retry path.
    thread_ownership.executes_on_host()
        && (number == libc::SYS_read as u64 || number == libc::SYS_readv as u64)
}

/// Executes a syscall on behalf of a KVM guest.
///
/// A full KVM backend will delegate this operation to its guest kernel. The
/// current bare-guest prototype accepts an executor explicitly so that Reverie
/// tools can use `Guest::inject` and `Guest::tail_inject` with the same contract
/// as the ptrace backend.
pub trait SyscallExecutor: Send + Sync {
    /// Executes `request` and returns its raw Linux syscall result.
    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64;
}

impl<F> SyscallExecutor for F
where
    F: FnMut(&SyscallRequest, &GuestMemory) -> i64 + Send + Sync,
{
    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {
        self(request, memory)
    }
}

enum InjectionCompletion {
    Returns {
        syscall_result: Option<i64>,
    },
    DoesNotReturn {
        image_replaced: bool,
        process_exited: bool,
    },
}

// TODO-HUMAN-REVIEW(PR-192): Review awaitable KVM injection Tool context.
pub(crate) struct ToolContext<'a, T: Tool> {
    /// The process (thread-group) identity of the thread issuing the action.
    pub(crate) pid: Pid,
    /// The thread identity of the thread issuing the action. Equals `pid` for a
    /// process leader; differs for a CLONE_THREAD worker.
    pub(crate) tid: Pid,
    /// Process Tool state shared by every CLONE_THREAD worker.
    pub(crate) process_state: Arc<T>,
    pub(crate) thread_state: &'a T::ThreadState,
    // TODO-HUMAN-REVIEW(PR-235): Review shared GlobalTool ownership across KVM forks.
    pub(crate) global_state: Option<Arc<T::GlobalState>>,
    pub(crate) config: <T::GlobalState as GlobalTool>::Config,
    pub(crate) subscriptions: Subscription,
    // TODO-HUMAN-REVIEW(PR-235): Review child release on parent handler suspension.
    pub(crate) pending_child_starts: SharedChildStarts,
}

// TODO-HUMAN-REVIEW(PR-192): Review async KVM process-action completion.
trait GuestSyscallExecutor<T: Tool>: Send + Sync {
    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64;

    fn defer_signal_delivery(&mut self, _event: SignalEvent) -> std::result::Result<(), Errno> {
        Err(Errno::ENOSYS)
    }

    fn ordinary_injection_allowed(&self, _request: &SyscallRequest) -> bool {
        true
    }

    fn tail_injection_allowed(&self) -> bool {
        true
    }

    // TODO-HUMAN-REVIEW(PR-235): Review virtual process ancestry exposed to Tool handlers.
    fn parent_pid(&self) -> Option<Pid> {
        None
    }

    /// The brk-managed heap region `[heap_base, program_break)` of the guest,
    /// when this executor backs a loaded static ELF. `None` for executors that
    /// do not model a heap (e.g. the direct pass-through executor). Used to
    /// report the guest heap region for deterministic memory-map logging.
    fn heap_region(&self) -> Option<(u64, u64)> {
        None
    }

    fn complete_injection<'a>(
        &'a mut self,
        _context: ToolContext<'a, T>,
    ) -> Pin<Box<dyn Future<Output = Result<InjectionCompletion>> + Send + 'a>>
    where
        T: 'a,
    {
        Box::pin(async {
            Ok(InjectionCompletion::Returns {
                syscall_result: None,
            })
        })
    }
}

struct DirectSyscallExecutor<'a> {
    executor: &'a mut dyn SyscallExecutor,
}

impl<T: Tool> GuestSyscallExecutor<T> for DirectSyscallExecutor<'_> {
    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {
        self.executor.execute(request, memory)
    }
}

#[derive(Clone)]
enum ProcessExecutionContext {
    InitialExec(SyscallRequest),
    InitialExecCompleted,
    Lifecycle,
    SignalBoundary(CompletedSyscallBoundary),
    FaultBoundary(Box<PageZeroFault>),
    SyscallBoundary(CompletedSyscallBoundary),
}

impl ProcessExecutionContext {
    fn tail_injection_allowed(&self) -> bool {
        !matches!(self, Self::SignalBoundary(_) | Self::FaultBoundary(_))
    }

    fn ordinary_injection_allowed(&self, request: &SyscallRequest) -> bool {
        !matches!(self, Self::SignalBoundary(_) | Self::FaultBoundary(_))
            || !injection_can_be_nonreturning(request)
    }

    fn injected_signal_allowed(&self, request: &SyscallRequest) -> bool {
        !signal_request_requires_return_frame(request)
            || matches!(
                self,
                Self::SignalBoundary(_) | Self::SyscallBoundary(_) | Self::FaultBoundary(_)
            )
    }
}

/// Returns whether a successful injected syscall can abandon the current Tool
/// handler instead of producing an ordinary scalar result.
fn injection_can_be_nonreturning(request: &SyscallRequest) -> bool {
    match request.number() {
        number
            if number == libc::SYS_execve as u64
                || number == libc::SYS_execveat as u64
                || number == libc::SYS_exit as u64
                || number == libc::SYS_exit_group as u64 =>
        {
            true
        }
        number if number == libc::SYS_kill as u64 || number == libc::SYS_tkill as u64 => {
            request.args()[1] == libc::SIGKILL as u64
        }
        number if number == libc::SYS_tgkill as u64 => request.args()[2] == libc::SIGKILL as u64,
        _ => false,
    }
}

/// Returns whether an injected request is the already-installed initial exec.
///
/// Tools may forward the synthetic `execve` unchanged, or use Reverie's
/// canonical `From<Execve> for Execveat` conversion. Only those two exact
/// requests are equivalent: accepting any other `execveat` would suppress a
/// real image replacement requested by the tool.
fn matches_initial_exec(expected: &SyscallRequest, request: &SyscallRequest) -> bool {
    if expected.number() != libc::SYS_execve as u64 || expected.args()[3..] != [0, 0, 0] {
        return false;
    }
    if expected == request {
        return true;
    }

    let [path, argv, envp, _, _, _] = *expected.args();
    request.number() == libc::SYS_execveat as u64
        && *request.args() == [libc::AT_FDCWD as u64, path, argv, envp, 0, 0]
}

fn signal_request_requires_return_frame(request: &SyscallRequest) -> bool {
    let signal = match request.number() {
        number if number == libc::SYS_kill as u64 || number == libc::SYS_tkill as u64 => {
            request.args()[1]
        }
        number if number == libc::SYS_tgkill as u64 => request.args()[2],
        _ => 0,
    };
    // Signal zero is an existence/permission probe and cannot create pending
    // delivery, so it is safe in lifecycle callbacks without a return frame.
    signal != 0
}

struct StaticElfSyscallExecutor<'a> {
    backend: &'a mut KvmBackend,
    executor: &'a mut ElfExecutor,
    memory: GuestMemory,
    process_context: ProcessExecutionContext,
    last_result: Option<i64>,
    process_completed: &'a mut bool,
}

impl<T> GuestSyscallExecutor<T> for StaticElfSyscallExecutor<'_>
where
    T: Tool + 'static,
    T::ThreadState: 'static,
    T::GlobalState: 'static,
    <T::GlobalState as GlobalTool>::Config: 'static,
{
    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {
        if !self.process_context.ordinary_injection_allowed(request) {
            // KvmGuest performs the same check before dispatch. Keep the
            // production executor fail-closed as well: a future Guest caller
            // must not execute an irreversible transition and only then learn
            // that SignalBoundary cannot resume its Tool hook.
            return -(i64::from(Errno::ENOSYS.into_raw()));
        }
        if matches!(self.process_context, ProcessExecutionContext::Lifecycle)
            && let Some(result) = self
                .executor
                .lifecycle_signal_mask_preflight(request, memory)
        {
            return result;
        }
        if !self.process_context.injected_signal_allowed(request) {
            // A successful injected self-signal would become pending, but a
            // lifecycle callback has no transported userspace context in which
            // to run the structured hook or build a signal frame. Refuse before
            // mutating pending state instead of delaying it to another syscall.
            return -(i64::from(Errno::ENOSYS.into_raw()));
        }
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-233): Review synthetic initial exec completion.
        if matches!(
            &self.process_context,
            ProcessExecutionContext::InitialExec(expected)
                if matches_initial_exec(expected, request)
        ) {
            self.last_result = Some(0);
            self.process_context = ProcessExecutionContext::InitialExecCompleted;
            return 0;
        }
        let result = self.executor.execute(request, memory);
        self.last_result = Some(result);
        result
    }

    fn defer_signal_delivery(&mut self, event: SignalEvent) -> std::result::Result<(), Errno> {
        match self.process_context {
            // The stopped syscall transport provides an exact userspace
            // register file and a frame slot for delivery at this boundary.
            // SignalBoundary is the same transport while the structured hook
            // filters the selected event.
            ProcessExecutionContext::SignalBoundary(_)
            | ProcessExecutionContext::FaultBoundary(_)
            | ProcessExecutionContext::SyscallBoundary(_) => {
                self.executor.defer_signal_delivery(event)
            }
            // Initial-start/post-exec callbacks do not have that transport.
            // Refuse rather than silently delaying until an unrelated syscall.
            _ => Err(Errno::ENOSYS),
        }
    }

    fn ordinary_injection_allowed(&self, request: &SyscallRequest) -> bool {
        self.process_context.ordinary_injection_allowed(request)
    }

    fn tail_injection_allowed(&self) -> bool {
        self.process_context.tail_injection_allowed()
    }

    fn parent_pid(&self) -> Option<Pid> {
        self.executor.parent_pid()
    }

    fn heap_region(&self) -> Option<(u64, u64)> {
        Some(self.executor.heap_region())
    }

    fn complete_injection<'a>(
        &'a mut self,
        context: ToolContext<'a, T>,
    ) -> Pin<Box<dyn Future<Output = Result<InjectionCompletion>> + Send + 'a>>
    where
        T: 'a,
    {
        Box::pin(async move {
            if matches!(
                self.process_context,
                ProcessExecutionContext::InitialExecCompleted
            ) {
                *self.process_completed = true;
                return Ok(InjectionCompletion::DoesNotReturn {
                    image_replaced: true,
                    process_exited: false,
                });
            }
            let Some(action) = self.executor.take_process_action() else {
                return Ok(if self.executor.has_pending_exit() {
                    InjectionCompletion::DoesNotReturn {
                        image_replaced: false,
                        process_exited: true,
                    }
                } else {
                    InjectionCompletion::Returns {
                        syscall_result: None,
                    }
                });
            };
            if !action.returns_to_original_image() && self.executor.has_eligible_pending_signal() {
                return Err(Error::UnexpectedVcpuExit(
                    "KVM Tool exec with an eligible deferred signal is unsupported; \
                     delivery requires a syscall return frame"
                        .to_owned(),
                ));
            }
            hide_tool_scratch(&self.memory, self.backend.tool_stack_top())?;
            let action_result: Result<ProcessActionOutcome> = async {
                match self.process_context.clone() {
                    ProcessExecutionContext::FaultBoundary(fault) => {
                        self.backend
                            .run_process_action_with_tool_from_fault(
                                self.executor,
                                action,
                                context,
                                &fault,
                            )
                            .await
                    }
                    ProcessExecutionContext::SignalBoundary(boundary)
                    | ProcessExecutionContext::SyscallBoundary(boundary) => {
                        let result = self
                            .last_result
                            .expect("process action must have an injected syscall result");
                        boundary.stage_action_result(self.backend, result)?;
                        let continuation =
                            ProcessActionContinuation::from_captured(&action, boundary);
                        self.backend
                            .run_process_action_with_tool_at_boundary(
                                self.executor,
                                action,
                                context,
                                continuation,
                            )
                            .await
                    }
                    ProcessExecutionContext::InitialExec(_)
                    | ProcessExecutionContext::Lifecycle => match action {
                        ProcessAction::Exec {
                            executable_path,
                            executable_file,
                            image,
                            argv,
                            envp,
                        } => {
                            self.backend.exec_process(
                                self.executor,
                                (&executable_path, executable_file),
                                &image,
                                &argv,
                                &envp,
                            )?;
                            Ok(ProcessActionOutcome::replaced())
                        }
                        _ => Err(Error::UnexpectedVcpuExit(
                            "fork/clone injection requires a guest syscall boundary".to_owned(),
                        )),
                    },
                    ProcessExecutionContext::InitialExecCompleted => unreachable!(
                        "synthetic initial exec completes before process actions are inspected"
                    ),
                }
            }
            .await;
            let expose_result = expose_tool_scratch(&self.memory, self.backend.tool_stack_top());
            let outcome = action_result?;
            expose_result?;
            *self.process_completed = true;
            if outcome.image_replaced || self.executor.has_pending_exit() {
                Ok(InjectionCompletion::DoesNotReturn {
                    image_replaced: outcome.image_replaced,
                    process_exited: self.executor.has_pending_exit(),
                })
            } else {
                Ok(InjectionCompletion::Returns {
                    syscall_result: Some(outcome.syscall_result),
                })
            }
        })
    }
}

struct KvmGlobal<'a, G: GlobalTool> {
    // The scheduler derives the requesting DetTid from the RPC sender, so this
    // is the issuing thread's tid (equal to the pid for a process leader,
    // distinct for a CLONE_THREAD worker), not the thread-group pid.
    tid: Pid,
    state: &'a G,
    config: &'a G::Config,
}

#[reverie::tool]
impl<G: GlobalTool> GlobalRPC<G> for KvmGlobal<'_, G> {
    async fn send_rpc(&self, message: G::Request) -> G::Response {
        self.state.receive_rpc(self.tid, message).await
    }

    fn config(&self) -> &G::Config {
        self.config
    }
}

struct KvmGuest<'a, T: Tool> {
    pid: Pid,
    tid: Pid,
    process_state: Arc<T>,
    memory: GuestMemory,
    auxv: &'a [(libc::c_ulong, libc::c_ulong)],
    registers: libc::user_regs_struct,
    thread_state: &'a mut T::ThreadState,
    executor: &'a mut dyn GuestSyscallExecutor<T>,
    global_state: &'a T::GlobalState,
    shared_global_state: Option<Arc<T::GlobalState>>,
    config: &'a <T::GlobalState as GlobalTool>::Config,
    subscriptions: &'a Subscription,
    handler_signal: SharedHandlerSignal,
    pending_child_starts: SharedChildStarts,
    tool_stack_top: u64,
    stack_checked_out: Arc<AtomicBool>,
}

impl<'a, T: Tool> KvmGuest<'a, T> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        pid: Pid,
        tid: Pid,
        process_state: Arc<T>,
        memory: GuestMemory,
        auxv: &'a [(libc::c_ulong, libc::c_ulong)],
        registers: libc::user_regs_struct,
        thread_state: &'a mut T::ThreadState,
        executor: &'a mut dyn GuestSyscallExecutor<T>,
        global_state: &'a T::GlobalState,
        shared_global_state: Option<Arc<T::GlobalState>>,
        config: &'a <T::GlobalState as GlobalTool>::Config,
        subscriptions: &'a Subscription,
        handler_signal: SharedHandlerSignal,
        pending_child_starts: SharedChildStarts,
        tool_stack_top: u64,
        stack_checked_out: Arc<AtomicBool>,
    ) -> Self {
        Self {
            pid,
            tid,
            process_state,
            memory,
            auxv,
            registers,
            thread_state,
            executor,
            global_state,
            shared_global_state,
            config,
            subscriptions,
            handler_signal,
            pending_child_starts,
            tool_stack_top,
            stack_checked_out,
        }
    }

    fn signal_handler(&self, signal: HandlerSignal) {
        *self
            .handler_signal
            .lock()
            .expect("KVM handler signal lock poisoned") = Some(signal);
    }
}

#[reverie::tool]
impl<T: Tool> GlobalRPC<T::GlobalState> for KvmGuest<'_, T> {
    async fn send_rpc(
        &self,
        message: <T::GlobalState as GlobalTool>::Request,
    ) -> <T::GlobalState as GlobalTool>::Response {
        // Route by the issuing thread's tid: the scheduler keys each thread's
        // turn (and global-time accounting) on the RPC sender. For a
        // CLONE_THREAD worker this is the worker tid, not the thread-group pid.
        self.global_state.receive_rpc(self.tid, message).await
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        self.config
    }
}

#[reverie::tool]
impl<T: Tool> Guest<T> for KvmGuest<'_, T> {
    type Memory = GuestMemory;
    type Stack = KvmStack;

    fn tid(&self) -> Pid {
        self.tid
    }

    fn pid(&self) -> Pid {
        self.pid
    }

    fn ppid(&self) -> Option<Pid> {
        self.executor.parent_pid()
    }

    fn memory(&self) -> Self::Memory {
        self.memory.clone()
    }

    fn auxv(&self) -> Auxv {
        Auxv::from_entries(self.auxv.iter().copied())
    }

    fn thread_state_mut(&mut self) -> &mut T::ThreadState {
        self.thread_state
    }

    fn thread_state(&self) -> &T::ThreadState {
        self.thread_state
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        self.registers
    }

    async fn defer_signal_delivery(
        &mut self,
        event: SignalEvent,
    ) -> std::result::Result<(), reverie::Error> {
        self.executor
            .defer_signal_delivery(event)
            .map_err(Into::into)
    }

    async fn stack(&mut self) -> Self::Stack {
        KvmStack::new(
            self.memory.clone(),
            self.tool_stack_top,
            self.stack_checked_out.clone(),
        )
    }

    async fn daemonize(&mut self) {}

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> std::result::Result<i64, Errno> {
        let request = SyscallRequest::from_syscall(syscall);
        if !self.executor.ordinary_injection_allowed(&request) {
            return Err(Errno::ENOSYS);
        }
        if injection_can_be_nonreturning(&request)
            && self
                .pending_child_starts
                .lock()
                .expect("KVM child-start lock poisoned")
                .iter()
                .any(PendingChildStart::is_pending)
        {
            return Err(Errno::ENOSYS);
        }
        let mut result = raw_to_result(self.executor.execute(&request, &self.memory));
        if result.is_ok() {
            let context = ToolContext {
                pid: self.pid,
                tid: self.tid,
                process_state: self.process_state.clone(),
                thread_state: self.thread_state,
                global_state: self.shared_global_state.clone(),
                config: self.config.clone(),
                subscriptions: self.subscriptions.clone(),
                pending_child_starts: self.pending_child_starts.clone(),
            };
            match self.executor.complete_injection(context).await {
                Ok(InjectionCompletion::DoesNotReturn {
                    image_replaced,
                    process_exited,
                }) => {
                    // TODO-HUMAN-REVIEW(PR-156): Review non-returning exec/exit injection.
                    // Successful exec and exit injection cannot resume the old
                    // handler after their process state transition completes.
                    self.signal_handler(HandlerSignal::TailInjected {
                        result,
                        image_replaced,
                        process_exited,
                    });
                    return std::future::pending().await;
                }
                Ok(InjectionCompletion::Returns { syscall_result }) => {
                    if let Some(raw) = syscall_result {
                        result = raw_to_result(raw);
                    }
                }
                Err(error) => {
                    self.signal_handler(HandlerSignal::RuntimeError(error));
                    return std::future::pending().await;
                }
            }
        }
        result
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        if !self.executor.tail_injection_allowed() {
            // Refuse before executing any syscall so write/fd/process/address-
            // space/pending/exit state cannot change before the hook errors.
            self.signal_handler(HandlerSignal::RuntimeError(Error::Reverie(
                Errno::ENOSYS.into(),
            )));
            return std::future::pending().await;
        }
        let result = self.inject(syscall).await;
        self.signal_handler(HandlerSignal::TailInjected {
            result,
            image_replaced: false,
            process_exited: false,
        });
        std::future::pending().await
    }

    fn set_timer(&mut self, _schedule: TimerSchedule) -> std::result::Result<(), reverie::Error> {
        Ok(())
    }

    fn set_timer_precise(
        &mut self,
        _schedule: TimerSchedule,
    ) -> std::result::Result<(), reverie::Error> {
        Ok(())
    }

    fn read_clock(&mut self) -> std::result::Result<u64, reverie::Error> {
        // The single-vCPU process personality does not yet expose a PMU. Returning
        // a stable zero clock preserves deterministic syscall time while the
        // executor remains cooperative at every syscall boundary.
        Ok(0)
    }

    fn detlog_memory_regions(&self) -> Option<Vec<DetlogMemoryRegion>> {
        // For KVM, `pid()` is the host VMM process, so the default
        // `/proc/<pid>/maps` enumeration would hash the VMM's own stack/heap at
        // host addresses that are not valid guest addresses. Report the real
        // guest-address regions instead, readable through `memory()`.
        let mut regions = Vec::new();

        // Heap: the brk-managed heap spans [heap_base, program_break), where
        // heap_base is the initial break (align_up(main_end)). `brk()` maps
        // exactly these pages as it grows, so hashing this range reads only
        // mapped guest memory. The gap below heap_base (down to
        // BOOT_RESERVED_END) is unmapped and must NOT be hashed. Skip an empty
        // heap (guest never grew its break).
        if let Some((heap_base, program_break)) = self.executor.heap_region()
            && program_break > heap_base
        {
            regions.push(DetlogMemoryRegion {
                kind: DetlogRegionKind::Heap,
                start: heap_base,
                end: program_break,
            });
        }

        // Stack: the live user stack spans [rsp, guest_end). The unused pages
        // below rsp are deterministically zeroed at setup, so hashing the live
        // region is both cheaper than the full 8 MiB mapping and deterministic
        // across the two runs of a `--verify` pair (execution is deterministic,
        // so rsp is identical at the same syscall stop).
        let guest_end = self.memory.guest_end();
        let rsp = self.registers.rsp;
        if rsp >= self.memory.guest_base() && rsp < guest_end {
            regions.push(DetlogMemoryRegion {
                kind: DetlogRegionKind::Stack,
                start: rsp,
                end: guest_end,
            });
        }

        Some(regions)
    }
}

/// A stack allocator backed by a low page reserved for Tool injection buffers.
pub struct KvmStack {
    memory: GuestMemory,
    top: u64,
    stack_pointer: u64,
    capacity: usize,
    writes: Vec<(u64, Vec<u8>)>,
    checked_out: Option<Arc<AtomicBool>>,
}

impl KvmStack {
    fn new(memory: GuestMemory, top: u64, checked_out: Arc<AtomicBool>) -> Self {
        let bottom = top
            .checked_sub(TOOL_STACK_SIZE)
            .expect("KVM Tool stack address underflow");
        assert!(
            memory.guest_base() <= bottom && top <= memory.guest_end(),
            "KVM Tool stack lies outside guest memory"
        );
        assert!(
            !checked_out.swap(true, Ordering::SeqCst),
            "cannot retrieve a KVM guest stack while its previous guard is live",
        );
        Self {
            capacity: STACK_CAPACITY,
            memory,
            top,
            stack_pointer: top,
            writes: Vec::new(),
            checked_out: Some(checked_out),
        }
    }

    fn allocate<'stack, T>(&mut self, bytes: Vec<u8>) -> AddrMut<'stack, T> {
        let alignment = std::mem::align_of::<T>() as u64;
        let unaligned = self
            .stack_pointer
            .checked_sub(bytes.len() as u64)
            .expect("KVM guest stack address underflow");
        let address = unaligned & !(alignment - 1);
        assert!(
            self.top - address <= self.capacity as u64,
            "KVM guest stack overflow: capacity={} requested={}",
            self.capacity,
            self.top - address,
        );
        self.stack_pointer = address;
        self.writes.push((address, bytes));
        AddrMut::from_raw(address as usize)
            .expect("KVM guest stack allocation produced a null address")
    }
}

impl Drop for KvmStack {
    fn drop(&mut self) {
        if let Some(checked_out) = self.checked_out.take() {
            assert!(
                checked_out.swap(false, Ordering::SeqCst),
                "KVM stack dropped without a checked-out stack",
            );
        }
    }
}

/// Guard returned after KVM guest stack writes are committed.
pub struct KvmStackGuard {
    checked_out: Arc<AtomicBool>,
}

impl Drop for KvmStackGuard {
    fn drop(&mut self) {
        assert!(
            self.checked_out.swap(false, Ordering::SeqCst),
            "KVM stack guard dropped without a checked-out stack",
        );
    }
}

impl Stack for KvmStack {
    type StackGuard = KvmStackGuard;

    fn size(&self) -> usize {
        (self.top - self.stack_pointer) as usize
    }

    fn capacity(&self) -> usize {
        self.capacity
    }

    fn push<'stack, T>(&mut self, value: T) -> Addr<'stack, T> {
        let bytes = unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(&value).cast::<u8>(),
                std::mem::size_of::<T>(),
            )
        }
        .to_vec();
        self.allocate(bytes).into()
    }

    fn reserve<'stack, T>(&mut self) -> AddrMut<'stack, T> {
        self.allocate(vec![0; std::mem::size_of::<T>()])
    }

    fn commit(mut self) -> std::result::Result<Self::StackGuard, Errno> {
        for (address, bytes) in &self.writes {
            self.memory
                .write_raw(*address, bytes)
                .map_err(|_| Errno::EFAULT)?;
        }
        Ok(KvmStackGuard {
            checked_out: self
                .checked_out
                .take()
                .expect("KVM stack commit lost its checkout"),
        })
    }
}

impl MemoryAccess for KvmStack {
    fn read_vectored(
        &self,
        read_from: &[std::io::IoSlice],
        write_to: &mut [std::io::IoSliceMut],
    ) -> std::result::Result<usize, Errno> {
        self.memory.read_vectored(read_from, write_to)
    }

    fn write_vectored(
        &mut self,
        read_from: &[std::io::IoSlice],
        write_to: &mut [std::io::IoSliceMut],
    ) -> std::result::Result<usize, Errno> {
        self.memory.write_vectored(read_from, write_to)
    }
}

enum HandlerOutcome<T> {
    Returned(T),
    TailInjected {
        result: std::result::Result<i64, Errno>,
        image_replaced: bool,
        process_exited: bool,
    },
    RuntimeError(Error),
}

async fn drive_handler<T>(
    future: impl Future<Output = T>,
    handler_signal: SharedHandlerSignal,
    pending_child_starts: SharedChildStarts,
) -> HandlerOutcome<T> {
    let mut future = pin!(future);
    poll_fn(|context| match future.as_mut().poll(context) {
        Poll::Ready(result) => Poll::Ready(HandlerOutcome::Returned(result)),
        Poll::Pending => {
            let handler_signal = handler_signal
                .lock()
                .expect("KVM handler signal lock poisoned")
                .take();
            match handler_signal {
                Some(HandlerSignal::TailInjected {
                    result,
                    image_replaced,
                    process_exited,
                }) => {
                    return Poll::Ready(HandlerOutcome::TailInjected {
                        result,
                        image_replaced,
                        process_exited,
                    });
                }
                Some(HandlerSignal::RuntimeError(error)) => {
                    return Poll::Ready(HandlerOutcome::RuntimeError(error));
                }
                None => {}
            }
            let mut starts = pending_child_starts
                .lock()
                .expect("KVM child-start lock poisoned");
            if starts.iter().any(|start| start.start().is_err()) {
                return Poll::Ready(HandlerOutcome::RuntimeError(Error::UnexpectedVcpuExit(
                    "KVM child exited before its parent suspended registration".to_owned(),
                )));
            }
            starts.clear();
            Poll::Pending
        }
    })
    .await
}

pub(crate) fn start_pending_children(pending_child_starts: &SharedChildStarts) -> Result<()> {
    let mut starts = pending_child_starts
        .lock()
        .expect("KVM child-start lock poisoned");
    for start in starts.iter() {
        // Both fork and Tool-thread workers block on this receiver as their
        // first operation after a successful host spawn. A disconnected
        // receiver therefore indicates an internal registration invariant
        // violation rather than a guest-visible failure.
        start.start().map_err(|_| {
            Error::UnexpectedVcpuExit("registered KVM child lost its parent start gate".to_owned())
        })?;
    }
    starts.clear();
    Ok(())
}

fn tool_stack_bottom(tool_stack_top: u64) -> u64 {
    tool_stack_top - TOOL_STACK_SIZE
}

fn expose_tool_scratch(memory: &GuestMemory, tool_stack_top: u64) -> Result<()> {
    memory.map_user_range(tool_stack_bottom(tool_stack_top), TOOL_STACK_SIZE, false)
}

fn hide_tool_scratch(memory: &GuestMemory, tool_stack_top: u64) -> Result<()> {
    memory.unmap_user_range(tool_stack_bottom(tool_stack_top), TOOL_STACK_SIZE)
}

// TODO-HUMAN-REVIEW(PR-156): Review repeated post-exec lifecycle delivery.
#[allow(clippy::too_many_arguments)]
async fn run_post_exec_handler<T>(
    backend: &mut KvmBackend,
    tool: &Arc<T>,
    pid: Pid,
    memory: &GuestMemory,
    auxv: &mut Vec<(libc::c_ulong, libc::c_ulong)>,
    thread_state: &mut T::ThreadState,
    executor: &mut ElfExecutor,
    global_state: Arc<T::GlobalState>,
    config: &<T::GlobalState as GlobalTool>::Config,
    subscriptions: &Subscription,
    stack_checked_out: &Arc<AtomicBool>,
) -> Result<()>
where
    T: Tool + 'static,
    T::ThreadState: 'static,
    T::GlobalState: 'static,
    <T::GlobalState as GlobalTool>::Config: 'static,
{
    let tool_stack_top = backend.tool_stack_top();
    loop {
        if executor.has_eligible_pending_signal() {
            return Err(Error::UnexpectedVcpuExit(
                "KVM post-exec lifecycle has an eligible preserved signal; delivery requires a \
                 syscall return frame"
                    .to_owned(),
            ));
        }

        let registers = kvm_registers(backend.vcpu.get_regs()?, 0);
        let handler_signal = Arc::new(Mutex::new(None));
        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
        expose_tool_scratch(memory, tool_stack_top)?;
        let mut _process_completed = false;
        let outcome = {
            let mut guest_executor = StaticElfSyscallExecutor {
                backend,
                executor,
                memory: memory.clone(),
                process_context: ProcessExecutionContext::Lifecycle,
                last_result: None,
                process_completed: &mut _process_completed,
            };
            let mut guest = KvmGuest::<T>::new(
                pid,
                // A process leader (root, fork child, or the post-exec thread
                // that became the new leader) has tid == pid.
                pid,
                tool.clone(),
                memory.clone(),
                auxv,
                registers,
                thread_state,
                &mut guest_executor,
                global_state.as_ref(),
                Some(global_state.clone()),
                config,
                subscriptions,
                handler_signal.clone(),
                pending_child_starts.clone(),
                tool_stack_top,
                stack_checked_out.clone(),
            );
            drive_handler(
                tool.handle_post_exec(&mut guest),
                handler_signal,
                pending_child_starts,
            )
            .await
        };
        hide_tool_scratch(memory, tool_stack_top)?;
        match outcome {
            HandlerOutcome::Returned(Ok(())) => return Ok(()),
            HandlerOutcome::Returned(Err(error)) => return Err(Error::PostExec(error)),
            HandlerOutcome::RuntimeError(error) => return Err(error),
            HandlerOutcome::TailInjected {
                process_exited: true,
                ..
            } => return Ok(()),
            HandlerOutcome::TailInjected {
                image_replaced: true,
                ..
            } => *auxv = executor.auxv().to_vec(),
            HandlerOutcome::TailInjected { .. } => {
                return Err(Error::UnexpectedVcpuExit(
                    "post-exec handler tail-injected a syscall".to_owned(),
                ));
            }
        }
    }
}

fn initial_exec_request(memory: &GuestMemory, stack_pointer: u64) -> Result<SyscallRequest> {
    fn read_word(memory: &GuestMemory, address: u64) -> Result<u64> {
        let mut bytes = [0; std::mem::size_of::<u64>()];
        memory.read(address, &mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    let argc = read_word(memory, stack_pointer)?;
    let argv = stack_pointer
        .checked_add(std::mem::size_of::<u64>() as u64)
        .ok_or(Error::LongModeMemoryTooSmall)?;
    let path = read_word(memory, argv)?;
    let envp = argc
        .checked_add(1)
        .and_then(|words| words.checked_mul(std::mem::size_of::<u64>() as u64))
        .and_then(|offset| argv.checked_add(offset))
        .ok_or(Error::LongModeMemoryTooSmall)?;

    Ok(SyscallRequest::new(
        libc::SYS_execve as u64,
        [path, argv, envp, 0, 0, 0],
    ))
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-233): Review synthetic initial exec Tool delivery.
// TODO-HUMAN-REVIEW(PR-235): Review shared Tool state during initial exec.
#[allow(clippy::too_many_arguments)]
async fn run_initial_exec_handler<T>(
    backend: &mut KvmBackend,
    tool: &Arc<T>,
    pid: Pid,
    memory: &GuestMemory,
    auxv: &[(libc::c_ulong, libc::c_ulong)],
    thread_state: &mut T::ThreadState,
    executor: &mut ElfExecutor,
    global_state: &Arc<T::GlobalState>,
    config: &<T::GlobalState as GlobalTool>::Config,
    subscriptions: &Subscription,
    stack_checked_out: &Arc<AtomicBool>,
) -> Result<()>
where
    T: Tool + 'static,
    T::ThreadState: 'static,
    T::GlobalState: 'static,
    <T::GlobalState as GlobalTool>::Config: 'static,
{
    let tool_stack_top = backend.tool_stack_top();
    let request = initial_exec_request(memory, executor.initial_stack_pointer())?;
    let syscall = request.into_syscall()?;
    let mut registers = kvm_registers(backend.vcpu.get_regs()?, request.number());
    registers.rdi = request.args()[0];
    registers.rsi = request.args()[1];
    registers.rdx = request.args()[2];

    let handler_signal = Arc::new(Mutex::new(None));
    let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
    expose_tool_scratch(memory, tool_stack_top)?;
    let mut _process_completed = false;
    let outcome = {
        let mut guest_executor = StaticElfSyscallExecutor {
            backend,
            executor,
            memory: memory.clone(),
            process_context: ProcessExecutionContext::InitialExec(request),
            last_result: None,
            process_completed: &mut _process_completed,
        };
        let mut guest = KvmGuest::<T>::new(
            pid,
            // The initial exec runs on the root thread, where tid == pid.
            pid,
            tool.clone(),
            memory.clone(),
            auxv,
            registers,
            thread_state,
            &mut guest_executor,
            global_state.as_ref(),
            Some(global_state.clone()),
            config,
            subscriptions,
            handler_signal.clone(),
            pending_child_starts.clone(),
            tool_stack_top,
            stack_checked_out.clone(),
        );
        drive_handler(
            tool.handle_syscall_event(&mut guest, syscall),
            handler_signal,
            pending_child_starts,
        )
        .await
    };
    hide_tool_scratch(memory, tool_stack_top)?;

    match outcome {
        HandlerOutcome::Returned(result) => result.map(|_| ()).map_err(Error::Reverie),
        HandlerOutcome::TailInjected {
            result: Ok(_),
            process_exited: true,
            ..
        } => Ok(()),
        HandlerOutcome::TailInjected {
            result: Ok(_),
            image_replaced: true,
            ..
        } => Ok(()),
        HandlerOutcome::TailInjected {
            result: Err(error), ..
        } => Err(Error::Reverie(error.into())),
        HandlerOutcome::TailInjected { .. } => Err(Error::UnexpectedVcpuExit(
            "initial exec handler tail-injected without completing exec".to_owned(),
        )),
        HandlerOutcome::RuntimeError(error) => Err(error),
    }
}

#[derive(Clone, Copy)]
struct ToolExit {
    status: ExitStatus,
    process_exited: bool,
}
async fn notify_tool_exit<T: Tool>(
    tool: Arc<T>,
    pid: Pid,
    tid: Pid,
    global_state: &T::GlobalState,
    config: &<T::GlobalState as GlobalTool>::Config,
    thread_state: T::ThreadState,
    exit: ToolExit,
) -> Result<()> {
    // on_exit_thread deregisters this thread from the scheduler, so its RPCs
    // must be attributed to the exiting thread's tid.
    let thread_global = KvmGlobal {
        tid,
        state: global_state,
        config,
    };
    tool.on_exit_thread(tid, &thread_global, thread_state, exit.status)
        .await
        .map_err(Error::Reverie)?;
    if !exit.process_exited {
        return Ok(());
    }
    // The process-exit hook belongs to the thread-group leader (tid == pid).
    let process_global = KvmGlobal {
        tid: pid,
        state: global_state,
        config,
    };
    // Every worker has completed its exit callback and dropped its process
    // reference before the leader reaches this consuming hook.
    let tool = Arc::try_unwrap(tool).map_err(|_| {
        Error::UnexpectedVcpuExit("KVM worker retained process Tool state after exit".to_owned())
    })?;
    tool.on_exit_process(pid, &process_global, exit.status)
        .await
        .map_err(Error::Reverie)
}

impl KvmBackend {
    /// Releases a worker's reusable slot before its exit becomes visible to
    /// the scheduler. A newly admitted guest thread can then make the same
    /// first-free choice independent of host-thread destruction timing.
    pub(crate) async fn notify_tool_exit<T: Tool>(
        &mut self,
        tool: Arc<T>,
        identity: (Pid, Pid),
        global_state: &T::GlobalState,
        config: &<T::GlobalState as GlobalTool>::Config,
        thread_state: T::ThreadState,
        status: ExitStatus,
    ) -> Result<()> {
        let (pid, tid) = identity;
        if pid == tid {
            // This also covers a terminal lifecycle-hook error, whose caller
            // may not already have joined the workers.
            self.cancel_guest_threads();
        }
        // No guest execution or Tool callback can use this backend's transport
        // or scratch page after a terminal exit has been observed. Release it
        // before on_exit_thread can wake and admit another guest thread.
        self.release_thread_slot();
        notify_tool_exit(
            tool,
            pid,
            tid,
            global_state,
            config,
            thread_state,
            ToolExit {
                status,
                process_exited: pid == tid,
            },
        )
        .await
    }

    /// Runs the installed guest program through a shared Reverie `Tool`.
    ///
    /// The executor supplies Linux syscall semantics that a future guest kernel
    /// will provide. Tool lifecycle, typed syscall dispatch, thread state,
    /// global RPC, memory, stack, injection, and tail injection use the same
    /// Reverie contracts as the ptrace backend.
    pub async fn run_with_tool<T, E>(
        &mut self,
        config: <T::GlobalState as GlobalTool>::Config,
        mut executor: E,
    ) -> Result<T::GlobalState>
    where
        T: Tool,
        E: SyscallExecutor,
    {
        let tool_stack_top = self.tool_stack_top();
        let pid = Pid::from_raw(self.root_pid);
        let global_state = T::GlobalState::init_global_state(&config).await;
        let tool = Arc::new(T::new(pid, &config));
        let subscriptions = T::subscriptions(&config);
        let mut thread_state = tool.init_thread_state(pid, None);
        let memory = self.memory.clone();
        let auxv = Vec::new();
        let stack_checked_out = Arc::new(AtomicBool::new(false));

        let registers = kvm_registers(self.vcpu.get_regs()?, 0);
        let handler_signal = Arc::new(Mutex::new(None));
        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
        expose_tool_scratch(&memory, tool_stack_top)?;
        let start_outcome = {
            let mut guest_executor = DirectSyscallExecutor {
                executor: &mut executor,
            };
            let mut guest = KvmGuest::<T>::new(
                pid,
                // run_with_tool drives a single root thread (tid == pid).
                pid,
                tool.clone(),
                memory.clone(),
                &auxv,
                registers,
                &mut thread_state,
                &mut guest_executor,
                &global_state,
                None,
                &config,
                &subscriptions,
                handler_signal.clone(),
                pending_child_starts.clone(),
                tool_stack_top,
                stack_checked_out.clone(),
            );
            drive_handler(
                tool.handle_thread_start(&mut guest),
                handler_signal,
                pending_child_starts,
            )
            .await
        };
        hide_tool_scratch(&memory, tool_stack_top)?;
        match start_outcome {
            HandlerOutcome::Returned(result) => result.map_err(Error::Reverie)?,
            HandlerOutcome::RuntimeError(error) => return Err(error),
            HandlerOutcome::TailInjected { .. } => {}
        }

        loop {
            let vcpu_exit = self.vcpu.run()?;
            Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
            match vcpu_exit {
                VcpuExit::Hypercall(exit) => {
                    if exit.nr != VMCALL_SYSCALL_TRANSPORT {
                        return Err(Error::UnexpectedHypercall(exit.nr));
                    }
                    let frame_address = exit.args[0];
                    let return_slot = std::ptr::from_mut(exit.ret) as usize;
                    let registers = self.vcpu.get_regs()?;
                    let request = SyscallRequest::read_from(&memory, frame_address)?;
                    let syscall = request.into_syscall()?;
                    let subscribed = subscriptions
                        .iter_syscalls()
                        .any(|number| number == syscall.number());
                    let result = if subscribed {
                        // Same `ERESTARTSYS` restart protocol as the
                        // process-syscall path below; see
                        // `classify_handler_result`.
                        loop {
                            let handler_signal = Arc::new(Mutex::new(None));
                            let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
                            expose_tool_scratch(&memory, tool_stack_top)?;
                            let outcome = {
                                let mut guest_executor = DirectSyscallExecutor {
                                    executor: &mut executor,
                                };
                                let mut guest = KvmGuest::<T>::new(
                                    pid,
                                    // run_with_tool drives a single root thread.
                                    pid,
                                    tool.clone(),
                                    memory.clone(),
                                    &auxv,
                                    kvm_registers(registers, request.number()),
                                    &mut thread_state,
                                    &mut guest_executor,
                                    &global_state,
                                    None,
                                    &config,
                                    &subscriptions,
                                    handler_signal.clone(),
                                    pending_child_starts.clone(),
                                    tool_stack_top,
                                    stack_checked_out.clone(),
                                );
                                drive_handler(
                                    tool.handle_syscall_event(&mut guest, syscall),
                                    handler_signal,
                                    pending_child_starts,
                                )
                                .await
                            };
                            hide_tool_scratch(&memory, tool_stack_top)?;
                            break match outcome {
                                HandlerOutcome::Returned(result) => {
                                    match classify_handler_result(result)? {
                                        Some(raw) => raw,
                                        None => continue,
                                    }
                                }
                                HandlerOutcome::TailInjected { result, .. } => {
                                    result_to_raw(result)
                                }
                                HandlerOutcome::RuntimeError(error) => return Err(error),
                            };
                        }
                    } else {
                        executor.execute(&request, &memory)
                    };
                    // SAFETY: return_slot points into this vCPU's stable KVM_RUN
                    // mapping. The vCPU remains stopped and is not run again while
                    // the tool callback is active.
                    unsafe {
                        (return_slot as *mut u64).write(result as u64);
                    }
                }
                VcpuExit::Hlt => {
                    let status = ExitStatus::SUCCESS;
                    self.notify_tool_exit(
                        tool,
                        (pid, pid),
                        &global_state,
                        &config,
                        thread_state,
                        status,
                    )
                    .await?;
                    return Ok(global_state);
                }
                exit => return Err(Error::UnexpectedVcpuExit(format!("{exit:?}"))),
            }
        }
    }

    /// Runs an installed static ELF through a Reverie `Tool`.
    ///
    /// This is the integration of the M1 ELF guest kernel
    /// ([`Self::run_static_elf`]) with the tool-interception path of
    /// [`Self::run_with_tool`]. A static ELF loaded by
    /// [`Self::install_static_elf`]/[`Self::install_static_elf_with_args`] runs
    /// in long mode. Root-thread syscalls selected by the tool's subscriptions
    /// are delivered to `Tool::handle_syscall_event`, including deferred
    /// fork/clone/exec/wait operations. A successful injected exec replaces the
    /// image without resuming the old handler. Forked process children receive
    /// their own process/thread tool state and dispatch subscribed syscalls through
    /// the same global state. `CLONE_THREAD` workers are Tool-owned by default and
    /// may explicitly opt into host ownership through the existing thread-ownership
    /// contract. Tool `inject`/`tail_inject` calls are serviced by the ELF guest kernel
    /// ([`ElfExecutor`]). Unlike [`Self::run_with_tool`], results are written
    /// back into the guest's syscall frame (the trampoline reads them and
    /// `SYSRET`s) and the guest exits via `exit`/`exit_group` rather than `HLT`.
    ///
    /// Returns the tool's global state, guest exit code, stdout, and stderr.
    pub async fn run_static_elf_with_tool<T>(
        &mut self,
        config: <T::GlobalState as GlobalTool>::Config,
        capture_output: bool,
    ) -> Result<(T::GlobalState, i32, Vec<u8>, Vec<u8>)>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        let mut loaded = self.static_elf.take().ok_or(Error::StaticElfNotInstalled)?;
        // Output capture replaces stdout and stderr with the executor's pipes,
        // but an explicitly configured stdin remains the guest's input. Use
        // /dev/null only when the caller supplied no stdin, matching
        // `run_static_elf_captured` while keeping the no-input default.
        if capture_output && loaded.stdin.is_none() {
            loaded.stdin = Some(std::fs::File::open("/dev/null")?);
        }
        let pid = Pid::from_raw(self.root_pid);
        // Resolve thread ownership before any CLONE_THREAD worker is created: an
        // explicit caller override wins, otherwise follow the tool's
        // `Tool::thread_ownership` (default: Tool-owned "follow children"). This
        // is why the KVM backend no longer needs the caller to opt threads in.
        self.resolve_thread_ownership(T::thread_ownership(&config));
        let global_state = Arc::new(T::GlobalState::init_global_state(&config).await);
        let tool = Arc::new(T::new(pid, &config));
        let subscriptions = T::subscriptions(&config);
        let thread_state = tool.init_thread_state(pid, None);
        let mut executor = ElfExecutor::new(loaded, capture_output);
        let result = self
            .run_static_elf_process_with_tool(
                &mut executor,
                pid,
                // The root process leader has tid == pid.
                pid,
                tool,
                thread_state,
                global_state.clone(),
                &config,
                &subscriptions,
                true,
            )
            .await?;
        let global_state = Arc::try_unwrap(global_state).map_err(|_| {
            Error::UnexpectedVcpuExit("KVM child retained global Tool state after exit".to_owned())
        })?;
        let (status, stdout, stderr) = result;
        Ok((global_state, conventional_exit_code(status), stdout, stderr))
    }

    #[allow(clippy::too_many_arguments)]
    async fn filter_one_pending_signal_with_tool<T>(
        &mut self,
        executor: &mut ElfExecutor,
        pid: Pid,
        tid: Pid,
        tool: &Arc<T>,
        memory: &GuestMemory,
        auxv: &[(libc::c_ulong, libc::c_ulong)],
        registers: kvm_regs,
        syscall_number: u64,
        frame_address: u64,
        thread_state: &mut T::ThreadState,
        global_state: &Arc<T::GlobalState>,
        config: &<T::GlobalState as GlobalTool>::Config,
        subscriptions: &Subscription,
        stack_checked_out: &Arc<AtomicBool>,
    ) -> Result<Option<PendingSignal>>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        // Linux repeats signal selection on the same exit-to-user path when a
        // tracer suppresses a signal or the replacement disposition ignores
        // it. There are at most two standard-signal domains of 31 entries; the
        // bound also prevents a Tool that keeps generating events from
        // monopolizing this boundary forever.
        for _ in 0..64 {
            let pending = self
                .filter_next_pending_signal_with_tool(
                    executor,
                    pid,
                    tid,
                    tool,
                    memory,
                    auxv,
                    registers,
                    syscall_number,
                    frame_address,
                    thread_state,
                    global_state,
                    config,
                    subscriptions,
                    stack_checked_out,
                    None,
                )
                .await?;
            if pending.is_some()
                || executor.has_pending_exit()
                || !executor.has_eligible_pending_signal()
            {
                return Ok(pending);
            }
        }
        Err(Error::UnexpectedVcpuExit(
            "KVM signal hook exhausted one return boundary without selecting delivery".to_owned(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn filter_next_pending_signal_with_tool<T>(
        &mut self,
        executor: &mut ElfExecutor,
        pid: Pid,
        tid: Pid,
        tool: &Arc<T>,
        memory: &GuestMemory,
        auxv: &[(libc::c_ulong, libc::c_ulong)],
        registers: kvm_regs,
        syscall_number: u64,
        frame_address: u64,
        thread_state: &mut T::ThreadState,
        global_state: &Arc<T::GlobalState>,
        config: &<T::GlobalState as GlobalTool>::Config,
        subscriptions: &Subscription,
        stack_checked_out: &Arc<AtomicBool>,
        fault: Option<&PageZeroFault>,
    ) -> Result<Option<PendingSignal>>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        let pending = if let Some(fault) = fault {
            fault.pending()
        } else {
            let Some(pending) = executor
                .take_pending_signal_for_delivery()
                .map_err(|errno| Error::Reverie(errno.into()))?
            else {
                return Ok(None);
            };
            pending
        };

        let handler_signal = Arc::new(Mutex::new(None));
        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
        let mut process_completed = false;
        let tool_stack_top = self.tool_stack_top();
        expose_tool_scratch(memory, tool_stack_top)?;
        let process_context = if let Some(fault) = fault {
            ProcessExecutionContext::FaultBoundary(Box::new(fault.clone()))
        } else {
            ProcessExecutionContext::SignalBoundary(CompletedSyscallBoundary::capture(
                self,
                frame_address,
                Some(registers),
            )?)
        };
        let outcome = {
            let mut guest_executor = StaticElfSyscallExecutor {
                backend: self,
                executor,
                memory: memory.clone(),
                process_context,
                last_result: None,
                process_completed: &mut process_completed,
            };
            let mut guest = KvmGuest::<T>::new(
                pid,
                tid,
                tool.clone(),
                memory.clone(),
                auxv,
                fault.map_or_else(
                    || kvm_registers(registers, syscall_number),
                    PageZeroFault::user_registers,
                ),
                thread_state,
                &mut guest_executor,
                global_state.as_ref(),
                Some(global_state.clone()),
                config,
                subscriptions,
                handler_signal.clone(),
                pending_child_starts.clone(),
                tool_stack_top,
                stack_checked_out.clone(),
            );
            drive_handler(
                tool.handle_structured_signal_event(&mut guest, pending.event),
                handler_signal,
                pending_child_starts.clone(),
            )
            .await
        };
        hide_tool_scratch(memory, tool_stack_top)?;
        let replacement = match outcome {
            HandlerOutcome::Returned(Ok(replacement)) => replacement,
            HandlerOutcome::Returned(Err(errno)) => {
                let error = self.cleanup_unstarted_tool_children_after_error(
                    executor,
                    &pending_child_starts,
                    Error::Reverie(errno.into()),
                );
                return Err(error);
            }
            HandlerOutcome::RuntimeError(error) => {
                return Err(self.cleanup_unstarted_tool_children_after_error(
                    executor,
                    &pending_child_starts,
                    error,
                ));
            }
            HandlerOutcome::TailInjected { .. } => {
                let error = Error::UnexpectedVcpuExit(
                    "tail injection from a KVM signal hook is unsupported".to_owned(),
                );
                return Err(self.cleanup_unstarted_tool_children_after_error(
                    executor,
                    &pending_child_starts,
                    error,
                ));
            }
        };
        self.start_pending_tool_children(executor, &pending_child_starts)?;
        if executor.take_process_action().is_some() {
            return Err(Error::UnexpectedVcpuExit(
                "process action from a KVM signal hook is unsupported".to_owned(),
            ));
        }
        match replacement {
            Some(event) => {
                let pending = if let Some(fault) = fault.filter(|fault| event == fault.event) {
                    Some(fault.pending())
                } else {
                    executor
                        .prepare_filtered_signal_delivery(event)
                        .map_err(|errno| Error::Reverie(errno.into()))?
                };
                if pending.is_some_and(|pending| {
                    executor.signal_disposition(pending.event.signal())
                        == crate::executor::SignalDisposition::Ignore
                }) {
                    Ok(None)
                } else {
                    Ok(pending)
                }
            }
            None => Ok(None),
        }
    }

    // TODO-HUMAN-REVIEW(PR-192): Review recursive KVM process Tool runtime.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_static_elf_process_with_tool<T>(
        &mut self,
        executor: &mut ElfExecutor,
        pid: Pid,
        tid: Pid,
        tool: Arc<T>,
        mut thread_state: T::ThreadState,
        global_state: Arc<T::GlobalState>,
        config: &<T::GlobalState as GlobalTool>::Config,
        subscriptions: &Subscription,
        initial_post_exec: bool,
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        let tool_stack_top = self.tool_stack_top();
        let _registration = self.register_guest_thread()?;
        let mut auxv = executor.auxv().to_vec();
        // Clones share the MAP_SHARED guest mapping; a mutable handle lets the
        // loop write syscall results back into the guest's frame.
        let mut memory = self.memory.clone();
        let stack_checked_out = Arc::new(AtomicBool::new(false));

        let registers = kvm_registers(self.vcpu.get_regs()?, 0);
        let handler_signal = Arc::new(Mutex::new(None));
        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
        expose_tool_scratch(&memory, tool_stack_top)?;
        let mut _process_completed = false;
        let start_outcome = {
            let mut guest_executor = StaticElfSyscallExecutor {
                backend: self,
                executor,
                memory: memory.clone(),
                process_context: ProcessExecutionContext::Lifecycle,
                last_result: None,
                process_completed: &mut _process_completed,
            };
            let mut guest = KvmGuest::<T>::new(
                pid,
                tid,
                tool.clone(),
                memory.clone(),
                &auxv,
                registers,
                &mut thread_state,
                &mut guest_executor,
                global_state.as_ref(),
                Some(global_state.clone()),
                config,
                subscriptions,
                handler_signal.clone(),
                pending_child_starts.clone(),
                tool_stack_top,
                stack_checked_out.clone(),
            );
            drive_handler(
                tool.handle_thread_start(&mut guest),
                handler_signal,
                pending_child_starts,
            )
            .await
        };
        hide_tool_scratch(&memory, tool_stack_top)?;
        match start_outcome {
            HandlerOutcome::Returned(result) => result.map_err(Error::Reverie)?,
            HandlerOutcome::RuntimeError(error) => return Err(error),
            HandlerOutcome::TailInjected { .. } => {}
        }
        if self.guest_thread_is_cancelled() {
            // The parent releases the start gate after it has begun scheduler
            // registration. handle_thread_start completes the child-side
            // ordering. Preserve handle_thread_start -> on_exit ordering,
            // but do not execute post-exec hooks or a guest instruction after
            // cancellation. Clear CHILD_CLEARTID immediately before the Tool
            // exit callback; the worker wrapper then observes that the address
            // has already been consumed.
            self.clear_registered_worker_tid_before_exit(executor);
            self.notify_tool_exit(
                tool,
                (pid, tid),
                global_state.as_ref(),
                config,
                thread_state,
                ExitStatus::SUCCESS,
            )
            .await?;
            let (stdout, stderr) = executor.take_output();
            return Ok((ExitStatus::SUCCESS, stdout, stderr));
        }
        auxv = executor.auxv().to_vec();
        if let Some(exit) = executor.take_exit() {
            if exit.group {
                self.request_guest_thread_group_exit(exit.status);
            }
            if exit.group || executor.is_thread_group_leader() {
                self.cancel_guest_threads();
            }
            self.clear_registered_worker_tid_before_exit(executor);
            self.notify_tool_exit(
                tool,
                (pid, tid),
                global_state.as_ref(),
                config,
                thread_state,
                exit.status,
            )
            .await?;
            let (stdout, stderr) = executor.take_output();
            return Ok((exit.status, stdout, stderr));
        }

        if initial_post_exec {
            // The root ELF image is already installed when this backend begins.
            // Present the same initial exec syscall and successful-exec lifecycle
            // boundaries as ptrace without loading the installed image twice.
            if subscriptions
                .iter_syscalls()
                .any(|number| number == reverie::syscalls::Sysno::execve)
            {
                run_initial_exec_handler(
                    self,
                    &tool,
                    pid,
                    &memory,
                    &auxv,
                    &mut thread_state,
                    executor,
                    &global_state,
                    config,
                    subscriptions,
                    &stack_checked_out,
                )
                .await?;
                auxv = executor.auxv().to_vec();
                if let Some(exit) = executor.take_exit() {
                    if exit.group {
                        self.request_guest_thread_group_exit(exit.status);
                    }
                    if exit.group || executor.is_thread_group_leader() {
                        self.cancel_guest_threads();
                    }
                    self.clear_registered_worker_tid_before_exit(executor);
                    self.notify_tool_exit(
                        tool,
                        (pid, tid),
                        global_state.as_ref(),
                        config,
                        thread_state,
                        exit.status,
                    )
                    .await?;
                    let (stdout, stderr) = executor.take_output();
                    return Ok((exit.status, stdout, stderr));
                }
            }
            let post_exec_error = run_post_exec_handler(
                self,
                &tool,
                pid,
                &memory,
                &mut auxv,
                &mut thread_state,
                executor,
                global_state.clone(),
                config,
                subscriptions,
                &stack_checked_out,
            )
            .await
            .err();
            if let Some(error) = post_exec_error {
                self.clear_registered_worker_tid_before_exit(executor);
                self.notify_tool_exit(
                    tool,
                    (pid, tid),
                    global_state.as_ref(),
                    config,
                    thread_state,
                    ExitStatus::Exited(255),
                )
                .await?;
                return Err(error);
            }
        }

        if let Some((segment, address)) = executor.take_segment() {
            set_user_segment_base(&self.vcpu, segment, address)?;
        }
        if let Some(exit) = executor.take_exit() {
            if exit.group {
                self.request_guest_thread_group_exit(exit.status);
            }
            if exit.group || executor.is_thread_group_leader() {
                self.cancel_guest_threads();
            }
            self.clear_registered_worker_tid_before_exit(executor);
            self.notify_tool_exit(
                tool,
                (pid, tid),
                global_state.as_ref(),
                config,
                thread_state,
                exit.status,
            )
            .await?;
            let (stdout, stderr) = executor.take_output();
            return Ok((exit.status, stdout, stderr));
        }

        // Read once so the per-syscall classifier can borrow it while `self` is
        // borrowed elsewhere in the loop body.
        let thread_ownership = self.thread_ownership;
        loop {
            if let Some(status) = self.guest_thread_group_exit_status() {
                self.cancel_guest_threads();
                self.clear_registered_worker_tid_before_exit(executor);
                self.notify_tool_exit(
                    tool,
                    (pid, tid),
                    global_state.as_ref(),
                    config,
                    thread_state,
                    status,
                )
                .await?;
                let (stdout, stderr) = executor.take_output();
                return Ok((status, stdout, stderr));
            }
            if self.guest_thread_is_cancelled() {
                self.clear_registered_worker_tid_before_exit(executor);
                self.notify_tool_exit(
                    tool,
                    (pid, tid),
                    global_state.as_ref(),
                    config,
                    thread_state,
                    ExitStatus::SUCCESS,
                )
                .await?;
                let (stdout, stderr) = executor.take_output();
                return Ok((ExitStatus::SUCCESS, stdout, stderr));
            }
            let vcpu_exit = match self.vcpu.run() {
                Ok(exit) => exit,
                Err(error) if error.errno() == libc::EINTR => continue,
                Err(error) => return Err(error.into()),
            };
            Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
            let (frame_address, return_slot) = match vcpu_exit {
                VcpuExit::Hypercall(exit) => {
                    if exit.nr != VMCALL_SYSCALL_TRANSPORT {
                        return Err(Error::UnexpectedHypercall(exit.nr));
                    }
                    (exit.args[0], std::ptr::from_mut(exit.ret) as usize)
                }
                VcpuExit::Hlt => {
                    if self.try_resume_vmware_backdoor_probe()? {
                        continue;
                    }
                    let Some(fault) = self.capture_page_zero_fault(executor)? else {
                        return Err(self.static_elf_halt_error()?);
                    };
                    executor.prepare_captured_page_zero_fault();
                    executor.set_current_user_stack_pointer(fault.registers.rsp);
                    let pending = self
                        .filter_next_pending_signal_with_tool(
                            executor,
                            pid,
                            tid,
                            &tool,
                            &memory,
                            &auxv,
                            fault.registers,
                            u64::MAX,
                            self.syscall_frame_address,
                            &mut thread_state,
                            &global_state,
                            config,
                            subscriptions,
                            &stack_checked_out,
                            Some(&fault),
                        )
                        .await?;
                    if let Some((segment, address)) = executor.take_segment() {
                        set_user_segment_base(&self.vcpu, segment, address)?;
                    }
                    if !executor.has_pending_exit() {
                        if let Some(pending) = pending {
                            self.deliver_page_zero_fault(executor, &fault, pending)?;
                        } else {
                            fault.resume_user(self, fault.registers)?;
                        }
                    }
                    if let Some(exit) = executor.take_exit() {
                        self.discard_process_clear_tid_at_signal_return(executor);
                        if exit.group {
                            self.request_guest_thread_group_exit(exit.status);
                        }
                        if executor.is_thread_group_leader() {
                            self.cancel_guest_threads();
                        }
                        executor.join_all_child_processes()?;
                        self.clear_registered_worker_tid_before_exit(executor);
                        self.notify_tool_exit(
                            tool,
                            (pid, tid),
                            global_state.as_ref(),
                            config,
                            thread_state,
                            exit.status,
                        )
                        .await?;
                        let (stdout, stderr) = executor.take_output();
                        return Ok((exit.status, stdout, stderr));
                    }
                    continue;
                }
                exit => return Err(Error::UnexpectedVcpuExit(format!("{exit:?}"))),
            };
            // A CLONE_THREAD worker runs on its own vCPU with a per-thread
            // syscall area, so the transported frame is `self.syscall_frame_address`
            // (equal to the root constant for the process leader, distinct for
            // each worker), not the fixed root `SYSCALL_FRAME_ADDRESS`.
            if frame_address != self.syscall_frame_address {
                return Err(Error::UnexpectedVcpuExit(format!(
                    "syscall frame is at unexpected address {frame_address:#x}"
                )));
            }
            let registers = self.vcpu.get_regs()?;
            let request = SyscallRequest::read_from(&memory, frame_address)?;
            // The KVM_RUN hypercall return slot is one-shot storage. Publish
            // its unused value exactly once while this decoded exit is live;
            // process actions may re-enter the vCPU before the Tool callback
            // returns, after which this pointer must never be reused.
            unsafe {
                (return_slot as *mut u64).write(0);
            }
            let userspace =
                process_syscall_return_registers(&memory, registers, frame_address, 0, None)?;
            executor.set_current_user_stack_pointer(userspace.rsp);
            if request.number() == libc::SYS_rt_sigreturn as u64 {
                // `rt_sigreturn` is backend-owned: its apparent syscall result
                // is the register file restored from the guest frame, not an
                // ordinary scalar return value that a Tool can replace.
                let mut signal_exit = None;
                if let Some(restored) = self.restore_rt_sigreturn(executor, frame_address)? {
                    executor.set_current_user_stack_pointer(restored.rsp);
                    let pending = self
                        .filter_one_pending_signal_with_tool(
                            executor,
                            pid,
                            tid,
                            &tool,
                            &memory,
                            &auxv,
                            restored,
                            request.number(),
                            frame_address,
                            &mut thread_state,
                            &global_state,
                            config,
                            subscriptions,
                            &stack_checked_out,
                        )
                        .await?;
                    if let Some((segment, address)) = executor.take_segment() {
                        set_user_segment_base(&self.vcpu, segment, address)?;
                    }
                    signal_exit = executor.take_exit();
                    let delivered = if signal_exit.is_none()
                        && let Some(pending) = pending
                    {
                        self.deliver_selected_signal_from_registers(
                            executor,
                            frame_address,
                            restored,
                            pending,
                        )?
                    } else {
                        false
                    };
                    signal_exit = signal_exit.or_else(|| executor.take_exit());
                    if !delivered && signal_exit.is_none() {
                        stage_process_syscall_return(
                            &mut memory,
                            &self.vcpu,
                            frame_address,
                            restored,
                        )?;
                    }
                }
                signal_exit = signal_exit.or_else(|| executor.take_exit());
                if let Some(exit) = signal_exit {
                    self.discard_process_clear_tid_at_signal_return(executor);
                    if exit.group {
                        self.request_guest_thread_group_exit(exit.status);
                    }
                    if executor.is_thread_group_leader() {
                        self.cancel_guest_threads();
                    }
                    executor.join_all_child_processes()?;
                    self.clear_registered_worker_tid_before_exit(executor);
                    self.notify_tool_exit(
                        tool,
                        (pid, tid),
                        global_state.as_ref(),
                        config,
                        thread_state,
                        exit.status,
                    )
                    .await?;
                    let (stdout, stderr) = executor.take_output();
                    return Ok((exit.status, stdout, stderr));
                }
                continue;
            }
            let syscall = request.into_syscall()?;
            // TODO-HUMAN-REVIEW(PR-156): Review root process-syscall Tool dispatch.
            // CLONE_THREAD is deliberately NOT backend-owned: the parent's
            // clone is delivered to the Tool (Detcore) so the worker inherits
            // process-shared Tool state (fd table, memory identity) and joins
            // Detcore's scheduler. `run_process_action_with_tool` then spawns
            // the worker on the Tool loop, which issues the matching
            // `handle_thread_start` the parent's clone handler waits for.
            let backend_owned = is_backend_owned_syscall(request.number(), thread_ownership)
                && !executor.is_random_device_read(&request)
                && !executor.is_tool_visible_read(&request);
            let subscribed = !backend_owned
                && subscriptions
                    .iter_syscalls()
                    .any(|number| number == syscall.number());
            let (mut result, handler_replaced_image, _handler_process_completed, restart_requested) =
                if subscribed {
                    let mut handler_process_completed = false;
                    // With no eligible virtual signal, ERESTARTSYS immediately
                    // re-enters the callback as before. With one, leave the loop so
                    // the structured signal hook and disposition decide whether
                    // the saved context reports EINTR or rewinds the syscall under
                    // SA_RESTART. The private errno itself never reaches userspace.
                    loop {
                        let handler_signal = Arc::new(Mutex::new(None));
                        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
                        expose_tool_scratch(&memory, tool_stack_top)?;
                        let boundary =
                            CompletedSyscallBoundary::capture(self, frame_address, None)?;
                        let outcome = {
                            let mut guest_executor = StaticElfSyscallExecutor {
                                backend: self,
                                executor,
                                memory: memory.clone(),
                                process_context: ProcessExecutionContext::SyscallBoundary(boundary),
                                last_result: None,
                                process_completed: &mut handler_process_completed,
                            };
                            let mut guest = KvmGuest::<T>::new(
                                pid,
                                tid,
                                tool.clone(),
                                memory.clone(),
                                &auxv,
                                kvm_registers(registers, request.number()),
                                &mut thread_state,
                                &mut guest_executor,
                                global_state.as_ref(),
                                Some(global_state.clone()),
                                config,
                                subscriptions,
                                handler_signal.clone(),
                                pending_child_starts.clone(),
                                tool_stack_top,
                                stack_checked_out.clone(),
                            );
                            drive_handler(
                                tool.handle_syscall_event(&mut guest, syscall),
                                handler_signal,
                                pending_child_starts.clone(),
                            )
                            .await
                        };
                        hide_tool_scratch(&memory, tool_stack_top)?;
                        let classified = match outcome {
                            HandlerOutcome::Returned(result) => {
                                let classified = match classify_handler_result(result) {
                                    Ok(classified) => classified,
                                    Err(error) => {
                                        return Err(self
                                            .cleanup_unstarted_tool_children_after_error(
                                                executor,
                                                &pending_child_starts,
                                                error,
                                            ));
                                    }
                                };
                                match classified {
                                    Some(raw) => (raw, false, handler_process_completed, false),
                                    None if !handler_process_completed
                                        && executor.has_eligible_pending_signal() =>
                                    {
                                        (-(libc::EINTR as i64), false, false, true)
                                    }
                                    // A restart is only meaningful while the process
                                    // is still live to re-run the syscall.
                                    None if !handler_process_completed => {
                                        self.start_pending_tool_children(
                                            executor,
                                            &pending_child_starts,
                                        )?;
                                        continue;
                                    }
                                    None => (
                                        -(i64::from(Errno::ERESTARTSYS.into_raw())),
                                        false,
                                        handler_process_completed,
                                        false,
                                    ),
                                }
                            }
                            HandlerOutcome::TailInjected {
                                result,
                                image_replaced,
                                ..
                            } => (
                                result_to_raw(result),
                                image_replaced,
                                handler_process_completed,
                                false,
                            ),
                            HandlerOutcome::RuntimeError(error) => {
                                return Err(self.cleanup_unstarted_tool_children_after_error(
                                    executor,
                                    &pending_child_starts,
                                    error,
                                ));
                            }
                        };
                        self.start_pending_tool_children(executor, &pending_child_starts)?;
                        break classified;
                    }
                } else {
                    (executor.execute(&request, &memory), false, false, false)
                };
            let mut returned_registers =
                process_syscall_return_registers(&memory, registers, frame_address, result, None)?;
            let mut restarted_registers = restart_requested
                .then(|| restart_syscall_registers(returned_registers, request.number()))
                .transpose()?;
            // The ring0 trampoline reads the result from the frame and then
            // SYSRETs, so the hypercall return slot is unused here.
            SyscallRequest::write_result(&mut memory, frame_address, result)?;
            let pending_segment = executor.take_segment();
            let mut pending_exit = executor.take_exit();
            let pending_process = executor.take_process_action();

            if let Some((segment, address)) = pending_segment {
                set_user_segment_base(&self.vcpu, segment, address)?;
            }
            let mut replaced_image = handler_replaced_image;
            if let Some(action) = pending_process {
                let continuation = CompletedSyscallBoundary::capture_for_action(
                    self,
                    frame_address,
                    None,
                    &action,
                )?;
                let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
                let context: ToolContext<'_, T> = ToolContext {
                    pid,
                    tid,
                    process_state: tool.clone(),
                    thread_state: &thread_state,
                    global_state: Some(global_state.clone()),
                    config: config.clone(),
                    subscriptions: subscriptions.clone(),
                    pending_child_starts: pending_child_starts.clone(),
                };
                let outcome = self
                    .run_process_action_with_tool_at_boundary(
                        executor,
                        action,
                        context,
                        continuation,
                    )
                    .await?;
                if !outcome.image_replaced {
                    result = outcome.syscall_result;
                    returned_registers = process_syscall_return_registers(
                        &memory,
                        registers,
                        frame_address,
                        result,
                        None,
                    )?;
                    restarted_registers = restart_requested
                        .then(|| restart_syscall_registers(returned_registers, request.number()))
                        .transpose()?;
                }
                replaced_image |= outcome.image_replaced;
                self.start_pending_tool_children(executor, &pending_child_starts)?;
            }
            if replaced_image {
                auxv = executor.auxv().to_vec();
                let post_exec_error = run_post_exec_handler(
                    self,
                    &tool,
                    pid,
                    &memory,
                    &mut auxv,
                    &mut thread_state,
                    executor,
                    global_state.clone(),
                    config,
                    subscriptions,
                    &stack_checked_out,
                )
                .await
                .err();
                if let Some(error) = post_exec_error {
                    self.clear_registered_worker_tid_before_exit(executor);
                    self.notify_tool_exit(
                        tool,
                        (pid, tid),
                        global_state.as_ref(),
                        config,
                        thread_state,
                        ExitStatus::Exited(255),
                    )
                    .await?;
                    return Err(error);
                }
            }
            if let Some((segment, address)) = executor.take_segment() {
                set_user_segment_base(&self.vcpu, segment, address)?;
            }
            if !replaced_image && pending_exit.is_none() {
                let pending = self
                    .filter_one_pending_signal_with_tool(
                        executor,
                        pid,
                        tid,
                        &tool,
                        &memory,
                        &auxv,
                        returned_registers,
                        request.number(),
                        frame_address,
                        &mut thread_state,
                        &global_state,
                        config,
                        subscriptions,
                        &stack_checked_out,
                    )
                    .await?;
                if let Some((segment, address)) = executor.take_segment() {
                    set_user_segment_base(&self.vcpu, segment, address)?;
                }
                pending_exit = pending_exit.or_else(|| executor.take_exit());
                let mut delivered = false;
                if pending_exit.is_none()
                    && let Some(pending) = pending
                {
                    let signal_registers =
                        if restart_requested && executor.caught_signal_restarts_syscall(pending) {
                            restarted_registers.expect("restart registers were constructed")
                        } else {
                            returned_registers
                        };
                    delivered = self.deliver_selected_signal_from_registers(
                        executor,
                        frame_address,
                        signal_registers,
                        pending,
                    )?;
                }
                pending_exit = pending_exit.or_else(|| executor.take_exit());
                if restart_requested && !delivered && pending_exit.is_none() {
                    // Suppression, an ignored replacement, or a replacement
                    // newly blocked by its signal number means no handler ran;
                    // resume by re-executing the original syscall rather than
                    // leaking EINTR or the kernel-private ERESTARTSYS value.
                    stage_process_syscall_return(
                        &mut memory,
                        &self.vcpu,
                        frame_address,
                        restarted_registers.expect("restart registers were constructed"),
                    )?;
                }
            }
            pending_exit = pending_exit.or_else(|| executor.take_exit());
            if let Some(exit) = pending_exit {
                executor.join_all_child_processes()?;
                if exit.group {
                    self.request_guest_thread_group_exit(exit.status);
                }
                if exit.group || executor.is_thread_group_leader() {
                    self.cancel_guest_threads();
                }
                self.clear_registered_worker_tid_before_exit(executor);
                self.notify_tool_exit(
                    tool,
                    (pid, tid),
                    global_state.as_ref(),
                    config,
                    thread_state,
                    exit.status,
                )
                .await?;
                let (stdout, stderr) = executor.take_output();
                return Ok((exit.status, stdout, stderr));
            }
        }
    }
}

/// Resolve a Tool handler's return value into the raw word written to the
/// guest's syscall frame, or `None` when the `ERESTARTSYS` protocol requires
/// either re-running the Tool callback or applying signal-disposition restart
/// policy at the static-ELF return boundary.
///
/// `ERESTARTSYS` is kernel-private: Linux never delivers it to userspace. It
/// either re-issues the interrupted syscall or reports `EINTR`. Detcore returns
/// it from `signal_interrupt_errno()` for the syscalls it models as restartable
/// (`read`, `futex`, ...) to mean exactly "re-run me". Under `reverie-ptrace`
/// the host kernel consumes it: the tracee's return register is set to
/// `-ERESTARTSYS` alongside a pending signal and Linux's signal-delivery path
/// rewinds and re-issues the syscall. A KVM guest is not a host process resumed
/// through that path, so this backend must repeat the callback itself. Without
/// it the private 512 reaches the guest as an application-visible errno.
///
/// With no eligible virtual signal the callback is re-entered immediately. If
/// one is pending, the static-ELF loop runs the structured signal hook first,
/// then saves either `EINTR` or a rewound syscall according to the final
/// signal's disposition and `SA_RESTART`. The other Linux-internal restart
/// classes are not part of Reverie's current Tool contract and fail explicitly
/// if a Tool manufactures one.
///
/// Isolating the policy in one pure function keeps it directly testable. Note
/// that testing it does not test restart execution; integration tests cover
/// both callback re-entry and signal-disposition-dependent frame restoration.
fn classify_handler_result(
    result: std::result::Result<i64, reverie::Error>,
) -> Result<Option<i64>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) => match error.into_errno().map_err(Error::Reverie)? {
            Errno::ERESTARTSYS => Ok(None),
            errno if matches!(errno.into_raw(), 513 | 514 | 516) => {
                Err(Error::UnexpectedVcpuExit(format!(
                    "unsupported Linux-internal syscall restart class {}",
                    errno.into_raw(),
                )))
            }
            errno => Ok(Some(-(i64::from(errno.into_raw())))),
        },
    }
}

fn restart_syscall_registers(mut registers: kvm_regs, syscall_number: u64) -> Result<kvm_regs> {
    registers.rip = registers.rip.checked_sub(2).ok_or_else(|| {
        Error::UnexpectedVcpuExit(
            "cannot rewind a syscall at an instruction pointer below two".to_owned(),
        )
    })?;
    registers.rax = syscall_number;
    Ok(registers)
}

fn raw_to_result(result: i64) -> std::result::Result<i64, Errno> {
    Errno::from_ret(result as usize).map(|value| value as i64)
}

fn result_to_raw(result: std::result::Result<i64, Errno>) -> i64 {
    match result {
        Ok(value) => value,
        Err(error) => -(error.into_raw() as i64),
    }
}

pub(crate) fn kvm_registers(registers: kvm_regs, syscall_number: u64) -> libc::user_regs_struct {
    libc::user_regs_struct {
        r15: registers.r15,
        r14: registers.r14,
        r13: registers.r13,
        r12: registers.r12,
        rbp: registers.rbp,
        rbx: registers.rbx,
        r11: registers.r11,
        r10: registers.r10,
        r9: registers.r9,
        r8: registers.r8,
        rax: registers.rax,
        rcx: registers.rcx,
        rdx: registers.rdx,
        rsi: registers.rsi,
        rdi: registers.rdi,
        orig_rax: syscall_number,
        rip: registers.rip,
        cs: 0,
        eflags: registers.rflags,
        rsp: registers.rsp,
        ss: 0,
        fs_base: 0,
        gs_base: 0,
        ds: 0,
        es: 0,
        fs: 0,
        gs: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::BOOT_RESERVED_END;
    use crate::bootstrap::TOOL_STACK_TOP;
    use crate::bootstrap::thread_tool_stack_top;

    fn synthetic_initial_exec() -> SyscallRequest {
        SyscallRequest::new(libc::SYS_execve as u64, [0x100, 0x200, 0x300, 0, 0, 0])
    }

    #[test]
    fn initial_exec_matches_original_and_canonical_execveat() {
        let expected = synthetic_initial_exec();
        assert!(matches_initial_exec(&expected, &expected));
        assert!(matches_initial_exec(
            &expected,
            &SyscallRequest::new(
                libc::SYS_execveat as u64,
                [libc::AT_FDCWD as u64, 0x100, 0x200, 0x300, 0, 0],
            )
        ));
    }

    #[test]
    fn initial_exec_rejects_every_other_execveat_shape() {
        let expected = synthetic_initial_exec();
        let canonical = [libc::AT_FDCWD as u64, 0x100, 0x200, 0x300, 0, 0];

        for index in 0..canonical.len() {
            let mut args = canonical;
            args[index] ^= 1;
            assert!(
                !matches_initial_exec(
                    &expected,
                    &SyscallRequest::new(libc::SYS_execveat as u64, args),
                ),
                "accepted execveat with argument {index} changed"
            );
        }

        assert!(!matches_initial_exec(
            &expected,
            &SyscallRequest::new(libc::SYS_execve as u64 + 1, *expected.args()),
        ));
    }

    #[test]
    fn initial_exec_match_requires_a_well_formed_execve_expectation() {
        let non_exec = SyscallRequest::new(libc::SYS_read as u64, [0x100, 0x200, 0x300, 0, 0, 0]);
        assert!(!matches_initial_exec(&non_exec, &non_exec));

        let malformed =
            SyscallRequest::new(libc::SYS_execve as u64, [0x100, 0x200, 0x300, 1, 0, 0]);
        assert!(!matches_initial_exec(&malformed, &malformed));
    }

    #[test]
    fn converts_linux_error_results() {
        assert_eq!(raw_to_result(7), Ok(7));
        assert_eq!(raw_to_result(-(libc::EIO as i64)), Err(Errno::EIO));
        assert_eq!(result_to_raw(Err(Errno::EFAULT)), -(libc::EFAULT as i64));
    }

    #[test]
    fn worker_shared_syscall_ownership_follows_thread_ownership() {
        // ppoll always stays backend-owned because KVM injection cannot execute it.
        for ownership in [ThreadOwnership::Host, ThreadOwnership::Tool] {
            assert!(is_backend_owned_syscall(libc::SYS_ppoll as u64, ownership));
            assert!(!is_backend_owned_syscall(
                libc::SYS_clock_gettime as u64,
                ownership
            ));
        }

        // Host-owned workers share descriptors outside the Tool, so reads stay
        // backend-owned. Tool-owned reads must reach the Tool's subscriptions.
        for number in [libc::SYS_read, libc::SYS_readv] {
            assert!(is_backend_owned_syscall(
                number as u64,
                ThreadOwnership::Host
            ));
            assert!(!is_backend_owned_syscall(
                number as u64,
                ThreadOwnership::Tool
            ));
        }
    }

    #[test]
    fn futex_ownership_follows_thread_ownership() {
        // Host-owned threads (uninstrumented workers): the root shares host
        // futex words, so futex stays backend-owned.
        assert!(is_backend_owned_syscall(
            libc::SYS_futex as u64,
            ThreadOwnership::Host
        ));
        // Tool-owned threads: futex routes to the Tool (Detcore) so joins are
        // logical scheduler waits woken by the exiting worker's CLEARTID.
        assert!(!is_backend_owned_syscall(
            libc::SYS_futex as u64,
            ThreadOwnership::Tool
        ));
    }

    #[test]
    fn handler_suspension_releases_registered_child_start() {
        let handler_signal = Arc::new(Mutex::new(None));
        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
        let (start_sender, start_receiver) = std::sync::mpsc::channel();
        let start_gate = ChildStartGate::new(start_sender);
        pending_child_starts
            .lock()
            .unwrap()
            .push(PendingChildStart::fork_process(2, start_gate));
        let handler = poll_fn(|context| match start_receiver.try_recv() {
            Ok(ChildStartCommand::Start) => Poll::Ready(true),
            Ok(ChildStartCommand::Cancel) => Poll::Ready(false),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                context.waker().wake_by_ref();
                Poll::Pending
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Poll::Ready(false),
        });

        assert!(matches!(
            futures::executor::block_on(drive_handler(
                handler,
                handler_signal,
                pending_child_starts,
            )),
            HandlerOutcome::Returned(true)
        ));
    }

    #[test]
    fn handler_runtime_error_precedes_and_preserves_unstarted_child_gate() {
        let handler_signal = Arc::new(Mutex::new(Some(HandlerSignal::RuntimeError(
            Error::UnexpectedVcpuExit("forced boundary restore failure".to_owned()),
        ))));
        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
        let (start_sender, start_receiver) = std::sync::mpsc::channel();
        let start_gate = ChildStartGate::new(start_sender);
        pending_child_starts
            .lock()
            .unwrap()
            .push(PendingChildStart::tool_thread(2, start_gate));

        let outcome = futures::executor::block_on(drive_handler(
            std::future::pending::<()>(),
            handler_signal,
            pending_child_starts.clone(),
        ));
        assert!(matches!(
            outcome,
            HandlerOutcome::RuntimeError(Error::UnexpectedVcpuExit(message))
                if message == "forced boundary restore failure"
        ));
        assert!(matches!(
            start_receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        let pending = pending_child_starts.lock().unwrap().pop().unwrap();
        assert!(matches!(
            pending.cancel(),
            PendingChildCancellation::NewlyCancelled {
                child: PendingChildKind::ToolThread(2),
                delivery_failed: false
            }
        ));
        assert_eq!(start_receiver.recv().unwrap(), ChildStartCommand::Cancel);
    }

    #[test]
    fn erestartsys_requests_a_restart_and_every_other_result_is_returned() {
        // The whole point of the protocol: the kernel-private 512 must never
        // become the guest's syscall result, so it maps to "re-run", not to a
        // raw word.
        assert_eq!(
            classify_handler_result(Err(Errno::ERESTARTSYS.into())).unwrap(),
            None
        );

        // Ordinary errnos still reach the guest, negated, exactly as before.
        assert_eq!(
            classify_handler_result(Err(Errno::EINTR.into())).unwrap(),
            Some(-(libc::EINTR as i64))
        );
        assert_eq!(
            classify_handler_result(Err(Errno::EBADF.into())).unwrap(),
            Some(-(libc::EBADF as i64))
        );

        for unsupported in [513, 514, 516] {
            let error = classify_handler_result(Err(Errno::new(unsupported).into())).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("unsupported Linux-internal syscall restart class"),
                "unexpected error for restart class {unsupported}: {error}",
            );
        }

        // Success values pass through untouched, including 0 and large reads.
        assert_eq!(classify_handler_result(Ok(0)).unwrap(), Some(0));
        assert_eq!(classify_handler_result(Ok(4096)).unwrap(), Some(4096));

        // A guard against the defect this replaced: no input may produce the
        // private restart value as a guest-visible result.
        let private = -(i64::from(Errno::ERESTARTSYS.into_raw()));
        for result in [
            Err(Errno::ERESTARTSYS.into()),
            Err(Errno::EINTR.into()),
            Err(Errno::EAGAIN.into()),
            Ok(0),
        ] {
            assert_ne!(classify_handler_result(result).unwrap(), Some(private));
        }
    }

    #[derive(Debug, Default, Eq, PartialEq)]
    struct TailInjectionSideEffects {
        output_bytes: usize,
        descriptors: usize,
        tasks: usize,
        address_space_generation: usize,
        pending_signal: bool,
        pending_process_action: bool,
        exited: bool,
        completion_callbacks: usize,
    }

    #[derive(Default)]
    struct SideEffectingExecutor {
        state: TailInjectionSideEffects,
    }

    impl GuestSyscallExecutor<crate::StraceTool> for SideEffectingExecutor {
        fn execute(&mut self, request: &SyscallRequest, _memory: &GuestMemory) -> i64 {
            match request.number() as libc::c_long {
                libc::SYS_write => self.state.output_bytes += request.args()[2] as usize,
                libc::SYS_pipe2 => self.state.descriptors += 2,
                libc::SYS_fork | libc::SYS_clone => {
                    self.state.tasks += 1;
                    self.state.pending_process_action = true;
                }
                libc::SYS_execve | libc::SYS_execveat => {
                    self.state.address_space_generation += 1;
                    self.state.pending_process_action = true;
                }
                libc::SYS_mmap => self.state.address_space_generation += 1,
                libc::SYS_kill | libc::SYS_tkill | libc::SYS_tgkill => {
                    self.state.pending_signal = true
                }
                libc::SYS_exit | libc::SYS_exit_group => self.state.exited = true,
                number => panic!("unexpected side-effect probe syscall {number}"),
            }
            0
        }

        fn ordinary_injection_allowed(&self, request: &SyscallRequest) -> bool {
            !injection_can_be_nonreturning(request)
        }

        fn complete_injection<'a>(
            &'a mut self,
            _context: ToolContext<'a, crate::StraceTool>,
        ) -> Pin<Box<dyn Future<Output = Result<InjectionCompletion>> + Send + 'a>>
        where
            crate::StraceTool: 'a,
        {
            self.state.completion_callbacks += 1;
            Box::pin(async {
                Ok(InjectionCompletion::DoesNotReturn {
                    image_replaced: true,
                    process_exited: true,
                })
            })
        }

        fn tail_injection_allowed(&self) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct PermissiveSideEffectingExecutor {
        state: TailInjectionSideEffects,
    }

    impl GuestSyscallExecutor<crate::StraceTool> for PermissiveSideEffectingExecutor {
        fn execute(&mut self, request: &SyscallRequest, _memory: &GuestMemory) -> i64 {
            match request.number() as libc::c_long {
                libc::SYS_execve | libc::SYS_execveat => {
                    self.state.address_space_generation += 1;
                    self.state.pending_process_action = true;
                }
                libc::SYS_kill | libc::SYS_tkill | libc::SYS_tgkill => {
                    self.state.pending_signal = true
                }
                libc::SYS_exit | libc::SYS_exit_group => self.state.exited = true,
                number => panic!("unexpected nonreturning probe syscall {number}"),
            }
            0
        }

        fn complete_injection<'a>(
            &'a mut self,
            _context: ToolContext<'a, crate::StraceTool>,
        ) -> Pin<Box<dyn Future<Output = Result<InjectionCompletion>> + Send + 'a>>
        where
            crate::StraceTool: 'a,
        {
            self.state.completion_callbacks += 1;
            Box::pin(async {
                Ok(InjectionCompletion::Returns {
                    syscall_result: None,
                })
            })
        }
    }

    #[test]
    fn unresolved_children_refuse_every_nonreturning_injection_before_mutation() {
        let requests = [
            SyscallRequest::new(libc::SYS_execve as u64, [0x100, 0x200, 0x300, 0, 0, 0]),
            SyscallRequest::new(
                libc::SYS_execveat as u64,
                [libc::AT_FDCWD as u64, 0x100, 0x200, 0x300, 0, 0],
            ),
            SyscallRequest::new(libc::SYS_exit as u64, [7, 0, 0, 0, 0, 0]),
            SyscallRequest::new(libc::SYS_exit_group as u64, [8, 0, 0, 0, 0, 0]),
            SyscallRequest::new(libc::SYS_kill as u64, [1, libc::SIGKILL as u64, 0, 0, 0, 0]),
            SyscallRequest::new(
                libc::SYS_tkill as u64,
                [1, libc::SIGKILL as u64, 0, 0, 0, 0],
            ),
            SyscallRequest::new(
                libc::SYS_tgkill as u64,
                [1, 1, libc::SIGKILL as u64, 0, 0, 0],
            ),
        ];
        for thread in [false, true] {
            for request in requests {
                let memory = GuestMemory::new(0, STACK_CAPACITY).unwrap();
                let auxv = [];
                let mut thread_state = ();
                let global_state = crate::StraceLog::default();
                let config = ();
                let subscriptions = Subscription::none();
                let handler_signal = Arc::new(Mutex::new(None));
                let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
                let (start_sender, start_receiver) = std::sync::mpsc::channel();
                let start_gate = ChildStartGate::new(start_sender);
                pending_child_starts.lock().unwrap().push(if thread {
                    PendingChildStart::tool_thread(2, start_gate)
                } else {
                    PendingChildStart::fork_process(2, start_gate)
                });
                let child = std::thread::spawn(move || {
                    assert_eq!(start_receiver.recv().unwrap(), ChildStartCommand::Cancel);
                });
                let mut executor = PermissiveSideEffectingExecutor::default();
                let mut guest = KvmGuest::<crate::StraceTool>::new(
                    Pid::from_raw(1),
                    Pid::from_raw(1),
                    Arc::new(crate::StraceTool),
                    memory,
                    &auxv,
                    // SAFETY: the test does not inspect any register field.
                    unsafe { std::mem::zeroed() },
                    &mut thread_state,
                    &mut executor,
                    &global_state,
                    None,
                    &config,
                    &subscriptions,
                    handler_signal.clone(),
                    pending_child_starts.clone(),
                    crate::bootstrap::TOOL_STACK_TOP,
                    Arc::new(AtomicBool::new(false)),
                );
                let result =
                    futures::FutureExt::now_or_never(guest.inject(request.into_syscall().unwrap()));
                assert_eq!(result, Some(Err(Errno::ENOSYS)));
                assert!(handler_signal.lock().unwrap().is_none());
                assert_eq!(executor.state, TailInjectionSideEffects::default());

                let pending = pending_child_starts.lock().unwrap().pop().unwrap();
                let expected = if thread {
                    PendingChildKind::ToolThread(2)
                } else {
                    PendingChildKind::ForkProcess(2)
                };
                assert!(matches!(
                    pending.cancel(),
                    PendingChildCancellation::NewlyCancelled {
                        child,
                        delivery_failed: false
                    } if child == expected
                ));
                child.join().unwrap();
            }
        }

        for started_gate in [false, true] {
            let memory = GuestMemory::new(0, STACK_CAPACITY).unwrap();
            let auxv = [];
            let mut thread_state = ();
            let global_state = crate::StraceLog::default();
            let config = ();
            let subscriptions = Subscription::none();
            let handler_signal = Arc::new(Mutex::new(None));
            let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
            if started_gate {
                let (start_sender, start_receiver) = std::sync::mpsc::channel();
                let start_gate = ChildStartGate::new(start_sender);
                assert_eq!(start_gate.start(), Ok(true));
                assert_eq!(start_receiver.recv().unwrap(), ChildStartCommand::Start);
                pending_child_starts
                    .lock()
                    .unwrap()
                    .push(PendingChildStart::fork_process(2, start_gate));
            }
            let mut executor = PermissiveSideEffectingExecutor::default();
            let mut guest = KvmGuest::<crate::StraceTool>::new(
                Pid::from_raw(1),
                Pid::from_raw(1),
                Arc::new(crate::StraceTool),
                memory,
                &auxv,
                // SAFETY: the test does not inspect any register field.
                unsafe { std::mem::zeroed() },
                &mut thread_state,
                &mut executor,
                &global_state,
                None,
                &config,
                &subscriptions,
                handler_signal,
                pending_child_starts,
                crate::bootstrap::TOOL_STACK_TOP,
                Arc::new(AtomicBool::new(false)),
            );
            let result = futures::FutureExt::now_or_never(
                guest.inject(
                    SyscallRequest::new(libc::SYS_execve as u64, [0x100, 0x200, 0x300, 0, 0, 0])
                        .into_syscall()
                        .unwrap(),
                ),
            );
            assert_eq!(result, Some(Ok(0)));
            assert_eq!(executor.state.address_space_generation, 1);
            assert_eq!(executor.state.completion_callbacks, 1);
        }
    }

    fn assert_tail_injection_rejected_before_execute(
        executor: &mut SideEffectingExecutor,
        request: SyscallRequest,
    ) {
        let memory = GuestMemory::new(0, STACK_CAPACITY).unwrap();
        let auxv = [];
        let mut thread_state = ();
        let global_state = crate::StraceLog::default();
        let config = ();
        let subscriptions = Subscription::none();
        let handler_signal = Arc::new(Mutex::new(None));
        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
        let mut guest = KvmGuest::<crate::StraceTool>::new(
            Pid::from_raw(1),
            Pid::from_raw(1),
            Arc::new(crate::StraceTool),
            memory,
            &auxv,
            // SAFETY: the test does not inspect any register field.
            unsafe { std::mem::zeroed() },
            &mut thread_state,
            executor,
            &global_state,
            None,
            &config,
            &subscriptions,
            handler_signal.clone(),
            pending_child_starts.clone(),
            crate::bootstrap::TOOL_STACK_TOP,
            Arc::new(AtomicBool::new(false)),
        );
        let syscall = request.into_syscall().unwrap();
        match futures::executor::block_on(drive_handler(
            guest.tail_inject(syscall),
            handler_signal,
            pending_child_starts,
        )) {
            HandlerOutcome::RuntimeError(Error::Reverie(reverie::Error::Errno(errno))) => {
                assert_eq!(errno, Errno::ENOSYS)
            }
            HandlerOutcome::RuntimeError(error) => panic!("unexpected tail refusal: {error}"),
            HandlerOutcome::TailInjected { .. } => panic!("tail injection unexpectedly ran"),
            HandlerOutcome::Returned(_) => panic!("tail injection unexpectedly returned"),
        }
    }

    fn test_process_boundary() -> CompletedSyscallBoundary {
        CompletedSyscallBoundary::for_test()
    }

    #[test]
    fn signal_hook_tail_injection_is_rejected_before_every_executor_side_effect() {
        let boundary = test_process_boundary();
        assert!(
            !ProcessExecutionContext::SignalBoundary(boundary.clone()).tail_injection_allowed()
        );
        assert!(
            ProcessExecutionContext::SyscallBoundary(boundary.clone()).tail_injection_allowed()
        );
        assert!(ProcessExecutionContext::Lifecycle.tail_injection_allowed());

        let nonfatal_signal =
            SyscallRequest::new(libc::SYS_kill as u64, [1, libc::SIGUSR1 as u64, 0, 0, 0, 0]);
        assert!(
            ProcessExecutionContext::SignalBoundary(boundary)
                .injected_signal_allowed(&nonfatal_signal)
        );
        assert!(
            !ProcessExecutionContext::Lifecycle.injected_signal_allowed(&nonfatal_signal),
            "a lifecycle callback still lacks a resumable signal frame",
        );

        let requests = [
            SyscallRequest::new(libc::SYS_write as u64, [1, 0x100, 4, 0, 0, 0]),
            SyscallRequest::new(libc::SYS_pipe2 as u64, [0x200, 0, 0, 0, 0, 0]),
            SyscallRequest::new(libc::SYS_fork as u64, [0; 6]),
            SyscallRequest::new(
                libc::SYS_clone as u64,
                [libc::SIGCHLD as u64, 0, 0, 0, 0, 0],
            ),
            SyscallRequest::new(libc::SYS_execve as u64, [0x300, 0, 0, 0, 0, 0]),
            SyscallRequest::new(
                libc::SYS_mmap as u64,
                [
                    0,
                    4096,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
                    (-1_i32) as u64,
                    0,
                ],
            ),
            SyscallRequest::new(libc::SYS_kill as u64, [1, libc::SIGUSR1 as u64, 0, 0, 0, 0]),
            SyscallRequest::new(libc::SYS_exit as u64, [7, 0, 0, 0, 0, 0]),
        ];
        let mut executor = SideEffectingExecutor::default();
        for request in requests {
            assert_tail_injection_rejected_before_execute(&mut executor, request);
        }
        assert_eq!(executor.state, TailInjectionSideEffects::default());
    }

    #[test]
    fn signal_hook_ordinary_nonreturning_injection_is_rejected_before_side_effects() {
        let requests = [
            SyscallRequest::new(libc::SYS_execve as u64, [0x100, 0x200, 0x300, 0, 0, 0]),
            SyscallRequest::new(
                libc::SYS_execveat as u64,
                [libc::AT_FDCWD as u64, 0x100, 0x200, 0x300, 0, 0],
            ),
            SyscallRequest::new(libc::SYS_exit as u64, [7, 0, 0, 0, 0, 0]),
            SyscallRequest::new(libc::SYS_exit_group as u64, [8, 0, 0, 0, 0, 0]),
            SyscallRequest::new(libc::SYS_kill as u64, [1, libc::SIGKILL as u64, 0, 0, 0, 0]),
            SyscallRequest::new(
                libc::SYS_tkill as u64,
                [1, libc::SIGKILL as u64, 0, 0, 0, 0],
            ),
            SyscallRequest::new(
                libc::SYS_tgkill as u64,
                [1, 1, libc::SIGKILL as u64, 0, 0, 0],
            ),
        ];
        let mut executor = SideEffectingExecutor::default();
        for request in requests {
            let boundary = test_process_boundary();
            assert!(
                !ProcessExecutionContext::SignalBoundary(boundary)
                    .ordinary_injection_allowed(&request)
            );
            let memory = GuestMemory::new(0, STACK_CAPACITY).unwrap();
            let auxv = [];
            let mut thread_state = ();
            let global_state = crate::StraceLog::default();
            let config = ();
            let subscriptions = Subscription::none();
            let handler_signal = Arc::new(Mutex::new(None));
            let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
            let mut guest = KvmGuest::<crate::StraceTool>::new(
                Pid::from_raw(1),
                Pid::from_raw(1),
                Arc::new(crate::StraceTool),
                memory,
                &auxv,
                // SAFETY: the test does not inspect any register field.
                unsafe { std::mem::zeroed() },
                &mut thread_state,
                &mut executor,
                &global_state,
                None,
                &config,
                &subscriptions,
                handler_signal.clone(),
                pending_child_starts.clone(),
                crate::bootstrap::TOOL_STACK_TOP,
                Arc::new(AtomicBool::new(false)),
            );
            let result =
                futures::FutureExt::now_or_never(guest.inject(request.into_syscall().unwrap()));
            assert_eq!(result, Some(Err(Errno::ENOSYS)));
            assert!(handler_signal.lock().unwrap().is_none());
            assert!(pending_child_starts.lock().unwrap().is_empty());
            assert_eq!(executor.state, TailInjectionSideEffects::default());
        }

        // Returning injections remain available to a structured hook.
        assert!(
            ProcessExecutionContext::SignalBoundary(test_process_boundary())
                .ordinary_injection_allowed(&SyscallRequest::new(
                    libc::SYS_write as u64,
                    [1, 0x100, 4, 0, 0, 0],
                ))
        );
        assert!(
            ProcessExecutionContext::SignalBoundary(test_process_boundary())
                .ordinary_injection_allowed(&SyscallRequest::new(
                    libc::SYS_kill as u64,
                    [1, libc::SIGUSR1 as u64, 0, 0, 0, 0],
                ))
        );
    }

    #[test]
    fn stack_commits_to_shared_guest_memory() {
        let memory = GuestMemory::new(0, TOOL_STACK_TOP as usize).unwrap();
        let checked_out = Arc::new(AtomicBool::new(false));
        let mut stack = KvmStack::new(memory.clone(), TOOL_STACK_TOP, checked_out.clone());
        let address = stack.push(0x1122_3344_u32);
        let guard = stack.commit().unwrap();

        let value = memory.read_value(address).unwrap();
        assert_eq!(value, 0x1122_3344_u32);
        assert!(checked_out.load(Ordering::SeqCst));

        drop(guard);
        assert!(!checked_out.load(Ordering::SeqCst));
    }

    #[test]
    fn dropping_uncommitted_stack_releases_checkout() {
        let memory = GuestMemory::new(0, TOOL_STACK_TOP as usize).unwrap();
        let checked_out = Arc::new(AtomicBool::new(false));

        drop(KvmStack::new(
            memory.clone(),
            TOOL_STACK_TOP,
            checked_out.clone(),
        ));
        assert!(!checked_out.load(Ordering::SeqCst));

        drop(KvmStack::new(memory, TOOL_STACK_TOP, checked_out));
    }

    #[test]
    fn failed_stack_commit_releases_checkout() {
        let memory = GuestMemory::new(0, TOOL_STACK_TOP as usize).unwrap();
        let checked_out = Arc::new(AtomicBool::new(false));
        let mut stack = KvmStack::new(memory.clone(), TOOL_STACK_TOP, checked_out.clone());
        stack.writes.push((memory.guest_end(), vec![0]));

        assert!(matches!(stack.commit(), Err(Errno::EFAULT)));
        assert!(!checked_out.load(Ordering::SeqCst));

        drop(KvmStack::new(memory, TOOL_STACK_TOP, checked_out));
    }

    #[test]
    fn guest_threads_use_disjoint_tool_stacks() {
        let memory = GuestMemory::new(0, BOOT_RESERVED_END as usize).unwrap();
        let first_top = thread_tool_stack_top(0);
        let second_top = thread_tool_stack_top(1);
        let first_checked_out = Arc::new(AtomicBool::new(false));
        let second_checked_out = Arc::new(AtomicBool::new(false));

        expose_tool_scratch(&memory, first_top).unwrap();
        expose_tool_scratch(&memory, second_top).unwrap();
        let mut first = KvmStack::new(memory.clone(), first_top, first_checked_out.clone());
        let mut second = KvmStack::new(memory.clone(), second_top, second_checked_out.clone());
        let first_address = first.push(0x1122_3344_u32);
        let second_address = second.push(0x5566_7788_u32);
        let first_guard = first.commit().unwrap();
        let second_guard = second.commit().unwrap();

        assert_ne!(first_address, second_address);
        assert_eq!(memory.read_value(first_address).unwrap(), 0x1122_3344_u32);
        assert_eq!(memory.read_value(second_address).unwrap(), 0x5566_7788_u32);
        assert!(first_checked_out.load(Ordering::SeqCst));
        assert!(second_checked_out.load(Ordering::SeqCst));

        hide_tool_scratch(&memory, first_top).unwrap();
        assert!(!memory.user_range_is_mapped(tool_stack_bottom(first_top), TOOL_STACK_SIZE));
        assert!(memory.user_range_is_mapped(tool_stack_bottom(second_top), TOOL_STACK_SIZE));

        drop(first_guard);
        drop(second_guard);
        hide_tool_scratch(&memory, second_top).unwrap();
    }

    #[test]
    #[should_panic(expected = "cannot retrieve a KVM guest stack while its previous guard is live")]
    fn same_thread_stack_checkout_still_panics() {
        let memory = GuestMemory::new(0, TOOL_STACK_TOP as usize).unwrap();
        let checked_out = Arc::new(AtomicBool::new(false));
        let _first = KvmStack::new(memory.clone(), TOOL_STACK_TOP, checked_out.clone());

        let _second = KvmStack::new(memory, TOOL_STACK_TOP, checked_out);
    }
}
