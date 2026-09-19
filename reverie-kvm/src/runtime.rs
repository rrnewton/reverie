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

use futures::FutureExt;
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
use crate::executor::ProcessExit;
use crate::executor::conventional_exit_code;
use crate::failure::FailureContext;
use crate::failure::RunFailure;
use crate::failure::wait_for_failure;
use crate::vm::CompletedSyscallBoundary;
use crate::vm::PageZeroFault;
use crate::vm::ProcessActionContinuation;
use crate::vm::ProcessActionOutcome;

const STACK_CAPACITY: usize = TOOL_STACK_SIZE as usize;

#[cfg(any(test, feature = "native-test-support"))]
pub mod native_test_support;

enum HandlerSignal {
    ParkedFatal(reverie::PreparedSignalToken),
    ParkedCancelled(reverie::ParkedSignalFailureContext),
    ThreadCancelled,
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

    pub(crate) fn tool_thread_gate(&self) -> Option<(i32, &ChildStartGate)> {
        match self.child {
            PendingChildKind::ToolThread(tid) => Some((tid, &self.start)),
            PendingChildKind::ForkProcess(_) => None,
        }
    }

    pub(crate) fn cancel(self) -> PendingChildCancellation {
        self.cancel_with_reason(false)
    }

    pub(crate) fn cancel_after_failure(self) -> PendingChildCancellation {
        self.cancel_with_reason(true)
    }

    fn cancel_with_reason(self, failed: bool) -> PendingChildCancellation {
        let cancelled = if failed {
            self.start.cancel_after_failure()
        } else {
            self.start.cancel()
        };
        match cancelled {
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
    fn read_clock(&self) -> Result<u64>;

    fn has_cpuid_interception(&self) -> bool {
        false
    }

    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64;

    /// Reserve backend bookkeeping before executing an injected syscall.
    /// Refusal is terminal backend failure, never an emulated syscall errno.
    fn prepare_signal_effects(&mut self) -> std::result::Result<(), Errno> {
        Ok(())
    }

    fn failure_subscription(&self) -> Option<crate::failure::FailureSubscription> {
        None
    }

    fn defer_signal_delivery(&mut self, _event: SignalEvent) -> std::result::Result<(), Errno> {
        Err(Errno::ENOSYS)
    }

    fn queue_child_exit_signal(&mut self, _event: SignalEvent) -> reverie::ChildExitSignalOutcome {
        reverie::ChildExitSignalOutcome::RejectedBeforeCommit {
            kind: reverie::ChildExitSignalErrorKind::Unsupported,
            errno: Errno::ENOSYS,
        }
    }

    fn queue_process_alarm_signal(
        &mut self,
        _event: SignalEvent,
    ) -> reverie::ProcessAlarmSignalOutcome {
        reverie::ProcessAlarmSignalOutcome::RejectedBeforeCommit {
            kind: reverie::ProcessAlarmSignalErrorKind::Unsupported,
            errno: Errno::ENOSYS,
        }
    }

    fn signal_controlled(&self) -> bool {
        false
    }
    fn finish_parked_delivery(&mut self) {}

    fn signal_task_identity(&self) -> Option<reverie::SignalTaskIdentity> {
        None
    }
    fn begin_signal_callback(&mut self) -> Option<reverie::CallbackSignalSite> {
        None
    }
    fn parked_signal_site(&self) -> Option<reverie::CallbackSignalSite> {
        None
    }
    fn captured_write_signal_site(
        &self,
        _call: reverie::syscalls::Write,
    ) -> Option<reverie::CallbackSignalSite> {
        None
    }
    fn set_signal_guard(&mut self, _guard: SignalGuard) -> SignalGuard {
        SignalGuard::Ordinary
    }
    fn admit_signal_observation(
        &mut self,
        _site: reverie::CallbackSignalSite,
        _lease: reverie::ParkedObservationLease,
    ) -> std::result::Result<(), Errno> {
        Err(Errno::ENOSYS)
    }
    fn signal_failure_context_is_current(
        &self,
        _context: reverie::ParkedSignalFailureContext,
    ) -> bool {
        false
    }
    fn signal_dequeue_front(&self) -> Option<reverie::SignalDequeue> {
        None
    }
    fn poll_signal_dequeue(
        &self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<reverie::SignalDequeue>>> {
        std::task::Poll::Ready(Ok(self.signal_dequeue_front()))
    }
    fn acknowledge_signal_dequeue(
        &mut self,
        _effect: reverie::SignalDequeue,
    ) -> std::result::Result<(), Errno> {
        Err(Errno::ENOSYS)
    }
    fn retain_signal_dequeue(&mut self, _effect: reverie::SignalDequeue) {}
    fn retain_signal_effect_result(&mut self, _raw: Option<i64>) {}
    fn signal_failure_context(&self) -> Option<reverie::ParkedSignalFailureContext> {
        None
    }
    fn with_signal_effects(&mut self, error: Error, _raw: Option<i64>) -> Error {
        error
    }
    fn take_signal_for_observation(&mut self) -> std::result::Result<Option<PendingSignal>, Errno> {
        Err(Errno::ENOSYS)
    }
    fn filter_signal_replacement(
        &mut self,
        _event: SignalEvent,
        _domain: crate::executor::PendingSignalDomain,
    ) -> std::result::Result<Option<PendingSignal>, Errno> {
        Err(Errno::ENOSYS)
    }
    fn signal_disposition(&self, _signal: i32) -> crate::executor::SignalDisposition {
        crate::executor::SignalDisposition::Terminate
    }
    fn reserve_signal_delivery(
        &mut self,
        _pending: PendingSignal,
        _id: reverie::DequeueId,
        _fatal: bool,
    ) -> std::result::Result<reverie::PreparedSignalToken, Errno> {
        Err(Errno::ENOSYS)
    }
    fn prepared_signal_is_fatal(&self, _token: reverie::PreparedSignalToken) -> bool {
        false
    }

    fn ordinary_injection_allowed(&self, _request: &SyscallRequest) -> bool {
        true
    }

    fn tail_injection_allowed(&self, _request: &SyscallRequest) -> bool {
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
    vcpu: &'a crate::clock::CountedVcpu,
}

impl<T: Tool> GuestSyscallExecutor<T> for DirectSyscallExecutor<'_> {
    fn read_clock(&self) -> Result<u64> {
        self.vcpu.read_clock()
    }

    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {
        self.executor.execute(request, memory)
    }
}

#[derive(Clone)]
enum ProcessExecutionContext {
    InitialExec(SyscallRequest),
    InitialExecCompleted,
    Lifecycle,
    Instruction,
    ThreadEntrySignal,
    SignalBoundary(CompletedSyscallBoundary),
    FaultBoundary(Box<PageZeroFault>),
    SyscallBoundary(CompletedSyscallBoundary),
}

impl ProcessExecutionContext {
    fn has_resumable_signal_boundary(&self) -> bool {
        // These contexts carry an exact user continuation for a signal frame.
        // Lifecycle and initial-exec callbacks have no such transport.
        matches!(
            self,
            Self::SignalBoundary(_)
                | Self::FaultBoundary(_)
                | Self::SyscallBoundary(_)
                | Self::ThreadEntrySignal
        )
    }

    fn tail_injection_allowed(&self, request: &SyscallRequest) -> bool {
        if matches!(self, Self::Instruction) {
            // Terminal exits do not need a syscall return frame. Every other
            // tail still requires a transport that can consume its result.
            return injection_is_explicit_exit(request);
        }
        !matches!(
            self,
            Self::SignalBoundary(_) | Self::FaultBoundary(_) | Self::ThreadEntrySignal
        )
    }

    fn ordinary_injection_allowed(&self, request: &SyscallRequest) -> bool {
        if matches!(self, Self::Instruction) && injection_is_explicit_exit(request) {
            // inject() itself is nonreturning once the real exit is staged;
            // tail_inject() uses that same path and both executor preflights.
            return true;
        }
        if matches!(self, Self::ThreadEntrySignal | Self::Instruction) {
            // There is a real user continuation, but no consumed syscall
            // transport to restore after an injected process action.
            return !injection_can_be_nonreturning(request)
                && !matches!(
                    request.number() as libc::c_long,
                    libc::SYS_fork | libc::SYS_vfork | libc::SYS_clone | libc::SYS_clone3
                );
        }
        !matches!(self, Self::SignalBoundary(_) | Self::FaultBoundary(_))
            || !injection_can_be_nonreturning(request)
    }

    fn injected_signal_allowed(&self, request: &SyscallRequest) -> bool {
        !signal_request_requires_return_frame(request)
            || matches!(
                self,
                Self::SignalBoundary(_)
                    | Self::SyscallBoundary(_)
                    | Self::FaultBoundary(_)
                    | Self::ThreadEntrySignal
            )
    }
}

fn injection_is_explicit_exit(request: &SyscallRequest) -> bool {
    matches!(
        request.number() as libc::c_long,
        libc::SYS_exit | libc::SYS_exit_group
    )
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SignalGuard {
    Ordinary,
    Observation,
    DequeueNotification,
}

struct StaticElfSyscallExecutor<'a> {
    backend: &'a mut KvmBackend,
    executor: &'a mut ElfExecutor,
    memory: GuestMemory,
    process_context: ProcessExecutionContext,
    last_result: Option<i64>,
    process_completed: &'a mut bool,
    callback_site: Option<reverie::CallbackSignalSite>,
    original_syscall: Option<SyscallRequest>,
    signal_guard: SignalGuard,
}

impl<T> GuestSyscallExecutor<T> for StaticElfSyscallExecutor<'_>
where
    T: Tool + 'static,
    T::ThreadState: 'static,
    T::GlobalState: 'static,
    <T::GlobalState as GlobalTool>::Config: 'static,
{
    fn read_clock(&self) -> Result<u64> {
        self.backend.vcpu.read_clock()
    }

    fn has_cpuid_interception(&self) -> bool {
        self.backend.has_cpuid_interception()
    }

    fn failure_subscription(&self) -> Option<crate::failure::FailureSubscription> {
        self.backend
            .tool_failure
            .as_ref()
            .map(|failure| failure.run.subscribe())
    }

    fn execute(&mut self, request: &SyscallRequest, memory: &GuestMemory) -> i64 {
        if !self.signal_injection_allowed(request) {
            // KvmGuest performs the same check before dispatch. Keep the
            // production executor fail-closed as well: a future Guest caller
            // must not execute an irreversible transition and only then learn
            // that SignalBoundary cannot resume its Tool hook.
            return -(i64::from(Errno::ENOSYS.into_raw()));
        }
        if matches!(
            self.process_context,
            ProcessExecutionContext::Lifecycle | ProcessExecutionContext::Instruction
        ) && let Some(result) = self
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

    fn prepare_signal_effects(&mut self) -> std::result::Result<(), Errno> {
        self.executor.reserve_signal_effects(64)
    }

    fn defer_signal_delivery(&mut self, event: SignalEvent) -> std::result::Result<(), Errno> {
        if self.signal_guard == SignalGuard::DequeueNotification {
            return Err(Errno::ENOSYS);
        }
        if self.process_context.has_resumable_signal_boundary() {
            self.executor.defer_signal_delivery(event)
        } else {
            Err(Errno::ENOSYS)
        }
    }

    fn queue_child_exit_signal(&mut self, event: SignalEvent) -> reverie::ChildExitSignalOutcome {
        if self.signal_guard == SignalGuard::DequeueNotification {
            return reverie::ChildExitSignalOutcome::RejectedBeforeCommit {
                kind: reverie::ChildExitSignalErrorKind::Unsupported,
                errno: Errno::ENOSYS,
            };
        }
        if self.process_context.has_resumable_signal_boundary() {
            self.executor.queue_child_exit_signal(event)
        } else {
            reverie::ChildExitSignalOutcome::RejectedBeforeCommit {
                kind: reverie::ChildExitSignalErrorKind::Unsupported,
                errno: Errno::ENOSYS,
            }
        }
    }

    fn queue_process_alarm_signal(
        &mut self,
        event: SignalEvent,
    ) -> reverie::ProcessAlarmSignalOutcome {
        if self.signal_guard == SignalGuard::DequeueNotification {
            return reverie::ProcessAlarmSignalOutcome::RejectedBeforeCommit {
                kind: reverie::ProcessAlarmSignalErrorKind::Unsupported,
                errno: Errno::ENOSYS,
            };
        }
        // Opted-in timer Tools require the exact original boundary and ledger.
        // Preserve the existing primitive contexts for unopted Tools.
        if self.executor.signal_dequeues_enabled() && self.current_parked_site().is_none() {
            return reverie::ProcessAlarmSignalOutcome::RejectedBeforeCommit {
                kind: reverie::ProcessAlarmSignalErrorKind::Unsupported,
                errno: Errno::ENOSYS,
            };
        }
        // Process actions may consume and restore this transport. This bounded
        // primitive requires the callback's original, unconsumed boundary.
        if *self.process_completed {
            return reverie::ProcessAlarmSignalOutcome::RejectedBeforeCommit {
                kind: reverie::ProcessAlarmSignalErrorKind::Unsupported,
                errno: Errno::ENOSYS,
            };
        }
        if self.process_context.has_resumable_signal_boundary() {
            if let Some(site) = self.current_parked_site()
                && let Err(errno) = self.executor.retain_parked_effects(site)
            {
                return reverie::ProcessAlarmSignalOutcome::RejectedBeforeCommit {
                    kind: reverie::ProcessAlarmSignalErrorKind::Backend,
                    errno,
                };
            }
            let outcome = self.executor.queue_process_alarm_signal(event);
            self.executor.retain_signal_publication(outcome);
            outcome
        } else {
            reverie::ProcessAlarmSignalOutcome::RejectedBeforeCommit {
                kind: reverie::ProcessAlarmSignalErrorKind::Unsupported,
                errno: Errno::ENOSYS,
            }
        }
    }

    fn signal_controlled(&self) -> bool {
        self.executor.signal_controlled()
    }
    fn finish_parked_delivery(&mut self) {
        self.executor.finish_parked_delivery();
    }

    fn signal_task_identity(&self) -> Option<reverie::SignalTaskIdentity> {
        self.executor.signal_task_identity()
    }
    fn begin_signal_callback(&mut self) -> Option<reverie::CallbackSignalSite> {
        self.callback_site = self.executor.begin_signal_callback();
        self.current_parked_site()
    }
    fn parked_signal_site(&self) -> Option<reverie::CallbackSignalSite> {
        self.current_parked_site()
    }
    fn captured_write_signal_site(
        &self,
        call: reverie::syscalls::Write,
    ) -> Option<reverie::CallbackSignalSite> {
        let request = SyscallRequest::from_syscall(call);
        if self.signal_guard != SignalGuard::Ordinary
            || self.last_result.is_some()
            || self.original_syscall != Some(request)
        {
            return None;
        }
        let site = self.current_parked_site()?;
        // Descriptor lookup uses Linux's low 32 bits only after exact raw-call
        // equality above; distinct upper argument bits do not share admission.
        let fd = request.args()[0] as libc::c_int;
        self.executor.captured_write_site(site, fd)
    }
    fn set_signal_guard(&mut self, guard: SignalGuard) -> SignalGuard {
        std::mem::replace(&mut self.signal_guard, guard)
    }
    fn admit_signal_observation(
        &mut self,
        site: reverie::CallbackSignalSite,
        lease: reverie::ParkedObservationLease,
    ) -> std::result::Result<(), Errno> {
        self.executor.admit_signal_observation(site, lease)
    }
    fn signal_failure_context_is_current(
        &self,
        context: reverie::ParkedSignalFailureContext,
    ) -> bool {
        self.callback_site == Some(context.site)
            && !*self.process_completed
            && self.executor.signal_failure_context_is_current(context)
    }
    fn signal_dequeue_front(&self) -> Option<reverie::SignalDequeue> {
        self.executor.signal_dequeue_front()
    }
    fn poll_signal_dequeue(
        &self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<reverie::SignalDequeue>>> {
        self.executor.poll_signal_dequeue(cx)
    }
    fn retain_signal_dequeue(&mut self, effect: reverie::SignalDequeue) {
        self.executor.retain_signal_dequeue(effect);
    }
    fn retain_signal_effect_result(&mut self, raw: Option<i64>) {
        self.executor.retain_signal_effect_result(raw);
    }
    fn acknowledge_signal_dequeue(
        &mut self,
        effect: reverie::SignalDequeue,
    ) -> std::result::Result<(), Errno> {
        self.executor.acknowledge_signal_dequeue(effect)?;
        self.executor
            .mark_signal_dequeue_acknowledged(effect.sequence);
        Ok(())
    }
    fn signal_failure_context(&self) -> Option<reverie::ParkedSignalFailureContext> {
        self.executor.signal_failure_context()
    }
    fn with_signal_effects(&mut self, error: Error, raw: Option<i64>) -> Error {
        self.executor.with_signal_effects(error, raw)
    }
    fn take_signal_for_observation(&mut self) -> std::result::Result<Option<PendingSignal>, Errno> {
        self.executor.reserve_signal_effects(1)?;
        self.executor.take_pending_signal_for_delivery()
    }
    fn filter_signal_replacement(
        &mut self,
        event: SignalEvent,
        domain: crate::executor::PendingSignalDomain,
    ) -> std::result::Result<Option<PendingSignal>, Errno> {
        self.executor
            .prepare_filtered_signal_delivery(event, domain)
    }
    fn signal_disposition(&self, signal: i32) -> crate::executor::SignalDisposition {
        self.executor.signal_disposition(signal)
    }
    fn reserve_signal_delivery(
        &mut self,
        pending: PendingSignal,
        id: reverie::DequeueId,
        fatal: bool,
    ) -> std::result::Result<reverie::PreparedSignalToken, Errno> {
        self.executor.reserve_signal_delivery(pending, id, fatal)
    }
    fn prepared_signal_is_fatal(&self, token: reverie::PreparedSignalToken) -> bool {
        self.callback_site == Some(token.site)
            && !*self.process_completed
            && self.executor.prepared_signal_is_fatal(token)
    }

    fn ordinary_injection_allowed(&self, request: &SyscallRequest) -> bool {
        self.signal_injection_allowed(request)
    }

    fn tail_injection_allowed(&self, request: &SyscallRequest) -> bool {
        self.signal_guard == SignalGuard::Ordinary
            && !self.executor.has_prepared_signal()
            && self.process_context.tail_injection_allowed(request)
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
                    ProcessExecutionContext::Instruction => Err(Error::UnexpectedVcpuExit(
                        "process injection from an instruction callback is unsupported".to_owned(),
                    )),
                    ProcessExecutionContext::ThreadEntrySignal => Err(Error::UnexpectedVcpuExit(
                        "process injection from a thread-entry signal hook is unsupported"
                            .to_owned(),
                    )),
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

include!("parked_signal_runtime.rs");

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
    observation_lease: Option<reverie::ParkedObservationLease>,
    notifying_dequeue: bool,
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
        executor.begin_signal_callback();
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
            observation_lease: None,
            notifying_dequeue: false,
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
        if self.notifying_dequeue {
            return self.global_state.receive_rpc(self.tid, message).await;
        }
        // Route by the issuing thread's tid: the scheduler keys each thread's
        // turn (and global-time accounting) on the RPC sender. For a
        // CLONE_THREAD worker this is the worker tid, not the thread-group pid.
        // An independent process may finish ordinary work after a peer fails,
        // but a pending ordinary RPC can prevent its owner's physical join.
        // Signal the driver and leave this response unconstructed on failure.
        // Consuming hooks use KvmGlobal instead and must still deregister.
        let response = {
            let mut failure = pin!(wait_for_failure(
                self.global_state,
                self.executor.failure_subscription(),
            ));
            let mut response = pin!(self.global_state.receive_rpc(self.tid, message));
            poll_fn(|context| {
                if failure.as_mut().poll(context).is_ready() {
                    return Poll::Ready(None);
                }
                let result = response.as_mut().poll(context);
                if failure.as_mut().poll(context).is_ready() {
                    return Poll::Ready(None);
                }
                result.map(Some)
            })
            .await
        };
        match response {
            Some(response) => response,
            None => {
                // Drop the ordinary request before publishing its nonlocal
                // result. A callback may poll this RPC again (for example,
                // after select returns its losing future); terminal polls must
                // neither resume the completed failure wait nor admit RPC work.
                self.signal_handler(HandlerSignal::RuntimeError(Error::RunAborted));
                std::future::pending().await
            }
        }
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        self.config
    }
}

#[reverie::tool]
impl<T: Tool> Guest<T> for KvmGuest<'_, T> {
    fn has_cpuid_interception(&self) -> bool {
        self.executor.has_cpuid_interception()
    }

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

    async fn queue_child_exit_signal(
        &mut self,
        event: SignalEvent,
    ) -> reverie::ChildExitSignalOutcome {
        self.executor.queue_child_exit_signal(event)
    }

    async fn queue_process_alarm_signal(
        &mut self,
        event: SignalEvent,
    ) -> reverie::ProcessAlarmSignalOutcome {
        self.executor.queue_process_alarm_signal(event)
    }

    fn signal_task_identity(&self) -> Option<reverie::SignalTaskIdentity> {
        self.executor.signal_task_identity()
    }
    fn parked_signal_site(&self) -> Option<reverie::CallbackSignalSite> {
        if self.notifying_dequeue || self.stack_checked_out.load(Ordering::Acquire) {
            None
        } else {
            self.executor.parked_signal_site()
        }
    }
    fn captured_write_signal_site(
        &self,
        call: reverie::syscalls::Write,
    ) -> Option<reverie::CallbackSignalSite> {
        if self.notifying_dequeue
            || self.observation_lease.is_some()
            || self.stack_checked_out.load(Ordering::Acquire)
        {
            None
        } else {
            self.executor.captured_write_signal_site(call)
        }
    }
    fn signal_observation_lease(&self) -> Option<reverie::ParkedObservationLease> {
        self.observation_lease
    }
    async fn observe_parked_signal(
        &mut self,
        site: reverie::CallbackSignalSite,
        lease: reverie::ParkedObservationLease,
    ) -> std::result::Result<reverie::ParkedSignalObservation, reverie::SignalObservationFailure>
    {
        self.observe_parked_signal_impl(site, lease).await
    }
    fn parked_signal_failure_context(&self) -> Option<reverie::ParkedSignalFailureContext> {
        self.executor.signal_failure_context()
    }
    async fn terminate_from_parked_signal(
        &mut self,
        selection: reverie::PreparedSignalToken,
    ) -> std::result::Result<Never, reverie::SignalObservationFailure> {
        if !self.executor.prepared_signal_is_fatal(selection) {
            return Err(reverie::SignalObservationFailure::RejectedBeforeRemoval {
                errno: Errno::EINVAL,
            });
        }
        self.signal_handler(HandlerSignal::ParkedFatal(selection));
        std::future::pending().await
    }
    async fn cancel_parked_signal(
        &mut self,
        context: reverie::ParkedSignalFailureContext,
    ) -> std::result::Result<Never, reverie::SignalObservationFailure> {
        if !self.executor.signal_failure_context_is_current(context) {
            return Err(reverie::SignalObservationFailure::RejectedBeforeRemoval {
                errno: Errno::EINVAL,
            });
        }
        self.signal_handler(HandlerSignal::ParkedCancelled(context));
        std::future::pending().await
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
        if let Err(errno) = self.executor.prepare_signal_effects() {
            let error = self
                .executor
                .with_signal_effects(Error::Reverie(errno.into()), None);
            self.signal_handler(HandlerSignal::RuntimeError(error));
            return std::future::pending().await;
        }
        let raw = self.executor.execute(&request, &self.memory);
        if let Err(error) = self.complete_signal_effects(Some(raw)).await {
            self.signal_handler(HandlerSignal::RuntimeError(error));
            return std::future::pending().await;
        }
        let mut result = raw_to_result(raw);
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
        let request = SyscallRequest::from_syscall(syscall);
        if !self.executor.tail_injection_allowed(&request) {
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

    async fn cancel_current_thread(&mut self) -> Never {
        if let Some(context) = self.executor.signal_failure_context() {
            self.signal_handler(HandlerSignal::ParkedCancelled(context));
            return std::future::pending().await;
        }
        if self.notifying_dequeue {
            // A consuming notification cannot turn an irreversible removal
            // into a successful thread cancellation, even outside a parked wait.
            let error = self.executor.with_signal_effects(Error::RunAborted, None);
            self.signal_handler(HandlerSignal::RuntimeError(error));
        } else {
            self.signal_handler(HandlerSignal::ThreadCancelled);
        }
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
        self.executor
            .read_clock()
            .map_err(|error| reverie::Error::Io(std::io::Error::other(error)))
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
    ParkedFatal(reverie::PreparedSignalToken),
    ParkedCancelled(reverie::ParkedSignalFailureContext),
    Returned(T),
    RunFailed,
    ThreadCancelled,
    TailInjected {
        result: std::result::Result<i64, Errno>,
        image_replaced: bool,
        process_exited: bool,
    },
    RuntimeError(Error),
}

/// A callback can terminate the thread without selecting a guest continuation.
enum CallbackOutcome<T> {
    Completed(T),
    ThreadCancelled,
}

async fn drive_handler<T>(
    future: impl Future<Output = T>,
    handler_signal: SharedHandlerSignal,
    pending_child_starts: SharedChildStarts,
    failure: impl Future<Output = ()>,
) -> HandlerOutcome<T> {
    // The callback may already have moved irreversible effects into this
    // terminal error. Failure still wins over ordinary values/child starts,
    // but must not replace the owned error with an empty cancellation marker.
    let terminal = || match handler_signal
        .lock()
        .expect("KVM handler signal lock poisoned")
        .take()
    {
        Some(HandlerSignal::RuntimeError(error)) => HandlerOutcome::RuntimeError(error),
        _ => HandlerOutcome::RunFailed,
    };
    let mut future = pin!(future);
    let mut failure = pin!(failure);
    poll_fn(|context| {
        if failure.as_mut().poll(context).is_ready() {
            return Poll::Ready(terminal());
        }
        let result = future.as_mut().poll(context);
        if failure.as_mut().poll(context).is_ready() {
            return Poll::Ready(terminal());
        }
        // A callback can select another ready future after an operation records
        // a nonlocal result. Consume that result before accepting either a
        // returned callback value or an ordinary suspension.
        let handler_signal = handler_signal
            .lock()
            .expect("KVM handler signal lock poisoned")
            .take();
        match handler_signal {
            Some(HandlerSignal::ParkedFatal(selection)) => {
                return Poll::Ready(HandlerOutcome::ParkedFatal(selection));
            }
            Some(HandlerSignal::ParkedCancelled(context)) => {
                return Poll::Ready(HandlerOutcome::ParkedCancelled(context));
            }
            Some(HandlerSignal::ThreadCancelled) => {
                return Poll::Ready(HandlerOutcome::ThreadCancelled);
            }
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
        match result {
            Poll::Ready(result) => Poll::Ready(HandlerOutcome::Returned(result)),
            Poll::Pending => {
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
) -> Result<CallbackOutcome<()>>
where
    T: Tool + 'static,
    T::ThreadState: 'static,
    T::GlobalState: 'static,
    <T::GlobalState as GlobalTool>::Config: 'static,
{
    let tool_stack_top = backend.tool_stack_top();
    let failure_subscription = backend.failure_subscription(executor.is_traced_tree_root());
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
                callback_site: None,
                original_syscall: None,
                signal_guard: SignalGuard::Ordinary,
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
                pending_child_starts.clone(),
                wait_for_failure(global_state.as_ref(), failure_subscription.clone()),
            )
            .await
        };
        hide_tool_scratch(memory, tool_stack_top)?;
        match outcome {
            HandlerOutcome::Returned(Ok(())) => return Ok(CallbackOutcome::Completed(())),
            HandlerOutcome::Returned(Err(error)) => return Err(Error::PostExec(error)),
            HandlerOutcome::ThreadCancelled => {
                backend.start_pending_tool_children(executor, &pending_child_starts)?;
                return Ok(CallbackOutcome::ThreadCancelled);
            }
            HandlerOutcome::RunFailed => {
                return Err(backend.cleanup_unstarted_tool_children_after_error(
                    executor,
                    &pending_child_starts,
                    Error::RunAborted,
                ));
            }
            HandlerOutcome::ParkedFatal(_) | HandlerOutcome::ParkedCancelled(_) => {
                return Err(Error::UnexpectedVcpuExit(
                    "parked outcome outside its original syscall callback".to_owned(),
                ));
            }
            HandlerOutcome::RuntimeError(error) => return Err(error),
            HandlerOutcome::TailInjected {
                process_exited: true,
                ..
            } => return Ok(CallbackOutcome::Completed(())),
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
) -> Result<CallbackOutcome<()>>
where
    T: Tool + 'static,
    T::ThreadState: 'static,
    T::GlobalState: 'static,
    <T::GlobalState as GlobalTool>::Config: 'static,
{
    let tool_stack_top = backend.tool_stack_top();
    let failure_subscription = backend.failure_subscription(executor.is_traced_tree_root());
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
            callback_site: None,
            original_syscall: None,
            signal_guard: SignalGuard::Ordinary,
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
            pending_child_starts.clone(),
            wait_for_failure(global_state.as_ref(), failure_subscription.clone()),
        )
        .await
    };
    hide_tool_scratch(memory, tool_stack_top)?;

    match outcome {
        HandlerOutcome::Returned(result) => result
            .map(|_| CallbackOutcome::Completed(()))
            .map_err(Error::Reverie),
        HandlerOutcome::TailInjected {
            result: Ok(_),
            process_exited: true,
            ..
        } => Ok(CallbackOutcome::Completed(())),
        HandlerOutcome::TailInjected {
            result: Ok(_),
            image_replaced: true,
            ..
        } => Ok(CallbackOutcome::Completed(())),
        HandlerOutcome::TailInjected {
            result: Err(error), ..
        } => Err(Error::Reverie(error.into())),
        HandlerOutcome::TailInjected { .. } => Err(Error::UnexpectedVcpuExit(
            "initial exec handler tail-injected without completing exec".to_owned(),
        )),
        HandlerOutcome::ThreadCancelled => {
            backend.start_pending_tool_children(executor, &pending_child_starts)?;
            Ok(CallbackOutcome::ThreadCancelled)
        }
        HandlerOutcome::RunFailed => Err(backend.cleanup_unstarted_tool_children_after_error(
            executor,
            &pending_child_starts,
            Error::RunAborted,
        )),
        HandlerOutcome::ParkedFatal(_) | HandlerOutcome::ParkedCancelled(_) => Err(
            Error::UnexpectedVcpuExit("parked outcome during initial exec".to_owned()),
        ),
        HandlerOutcome::RuntimeError(error) => Err(error),
    }
}

#[derive(Clone, Copy)]
struct ToolProcessExit {
    exit: ProcessExit,
    cancelled: bool,
}

impl From<ProcessExit> for ToolProcessExit {
    fn from(exit: ProcessExit) -> Self {
        Self {
            exit,
            cancelled: false,
        }
    }
}

/// A published run failure can interrupt a permitted CLONE_THREAD worker even
/// before the backend's ordinary cancellation flag becomes visible.
pub(crate) fn is_peer_cancelled_tool_worker<R>(
    identity: (Pid, Pid),
    start_permitted: bool,
    outcome: &Result<R>,
) -> bool {
    start_permitted
        && identity.0 != identity.1
        && outcome
            .as_ref()
            .is_err_and(|error| matches!(error.primary(), Error::RunAborted))
}

/// Keep the interrupted worker's error tree for final cleanup while preserving
/// ordinary cancellation retirement and any established guest exit status.
fn retire_peer_cancelled_tool_worker(
    executor: &mut ElfExecutor,
    identity: (Pid, Pid),
    start_permitted: bool,
    group_status: Option<ExitStatus>,
    outcome: &Result<ToolProcessExit>,
) -> Option<ToolProcessExit> {
    if !is_peer_cancelled_tool_worker(identity, start_permitted, outcome) {
        return None;
    }
    let exit = match group_status {
        Some(status) => executor.retire_current_thread(status, true),
        None => executor.cancel_current_thread(),
    };
    Some(ToolProcessExit {
        exit,
        cancelled: true,
    })
}

#[derive(Clone, Copy)]
struct ToolExit {
    status: ExitStatus,
    process_exited: bool,
}
#[allow(clippy::too_many_arguments)]
async fn notify_tool_exit<T: Tool>(
    tool: Arc<T>,
    pid: Pid,
    tid: Pid,
    global_state: &T::GlobalState,
    config: &<T::GlobalState as GlobalTool>::Config,
    thread_state: T::ThreadState,
    exit: ToolExit,
    failure: Option<&FailureContext>,
) -> Result<()> {
    // on_exit_thread deregisters this thread from the scheduler, so its RPCs
    // must be attributed to the exiting thread's tid.
    let thread_global = KvmGlobal {
        tid,
        state: global_state,
        config,
    };
    let thread_result = tool
        .on_exit_thread(tid, &thread_global, thread_state, exit.status)
        .await
        .map_err(|error| match failure {
            Some(failure) => failure.publish("thread exit hook", Error::Reverie(error)),
            None => {
                global_state.report_backend_failure(reverie::BackendFailure {
                    pid,
                    tid,
                    phase: "thread exit hook",
                });
                Error::Reverie(error)
            }
        });
    if !exit.process_exited {
        return thread_result;
    }
    // The process-exit hook belongs to the thread-group leader (tid == pid).
    let process_global = KvmGlobal {
        tid: pid,
        state: global_state,
        config,
    };
    // A failed thread hook has still consumed ThreadState. Attempt the
    // process hook as well, after every worker has dropped its process Arc.
    let process_result = match Arc::try_unwrap(tool) {
        Ok(tool) => tool
            .on_exit_process(pid, &process_global, exit.status)
            .await
            .map_err(Error::Reverie),
        Err(_) => Err(Error::UnexpectedVcpuExit(
            "KVM worker retained process Tool state after exit".to_owned(),
        )),
    };
    let process_result = process_result.map_err(|error| match failure {
        Some(failure) => failure.publish("process exit hook", error),
        None => {
            global_state.report_backend_failure(reverie::BackendFailure {
                pid,
                tid,
                phase: "process exit hook",
            });
            error
        }
    });
    match (thread_result, process_result) {
        (Ok(()), process) => process,
        (Err(error), Ok(())) => Err(error),
        (Err(thread), Err(process)) => Err(thread.with_cleanup(vec![process])),
    }
}

/// Result of a Tool run after owned workers and children have completed.
pub struct ToolRunCompletion<G> {
    /// Global Tool state, retained on runtime failure for consuming cleanup.
    pub global_state: G,
    /// Guest output/status, or a typed fatal runtime/Tool cause.
    pub result: Result<(i32, Vec<u8>, Vec<u8>)>,
}

/// Complete the owner after physical worker joins, publishing every newly
/// discovered failure before consuming hooks or joining independent children.
#[allow(clippy::too_many_arguments)]
async fn finish_tool_process_after_workers<T: Tool>(
    executor: &mut ElfExecutor,
    tool: Arc<T>,
    identity: (Pid, Pid),
    global_state: &T::GlobalState,
    config: &<T::GlobalState as GlobalTool>::Config,
    thread_state: T::ThreadState,
    outcome: Result<ToolProcessExit>,
    cancelled_exit: Option<ToolProcessExit>,
    workers: Result<()>,
    failure: Option<&FailureContext>,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let (pid, tid) = identity;
    let report = |phase, error| match failure {
        Some(failure) => failure.publish(phase, error),
        None => {
            global_state.report_backend_failure(reverie::BackendFailure { pid, tid, phase });
            error
        }
    };
    let workers = workers.map_err(|error| {
        let worker_tid = error.worker_tid().map(Pid::from_raw).unwrap_or(tid);
        match failure {
            Some(failure) => failure
                .for_thread(worker_tid)
                .publish("worker teardown", error),
            None => {
                global_state.report_backend_failure(reverie::BackendFailure {
                    pid,
                    tid: worker_tid,
                    phase: "worker teardown",
                });
                error
            }
        }
    });
    let natural_exit = outcome
        .as_ref()
        .is_ok_and(|exit| !exit.cancelled && !exit.exit.group);
    let mut status = cancelled_exit.map_or_else(
        || {
            outcome
                .as_ref()
                .map_or(ExitStatus::Exited(255), |exit| exit.exit.status)
        },
        |exit| exit.exit.status,
    );
    let mut process_status = Ok(());
    if pid == tid && natural_exit {
        match executor.process_exit_status() {
            Some(final_status) => status = final_status,
            None if workers.is_err() => status = ExitStatus::Exited(255),
            None => {
                status = ExitStatus::Exited(255);
                process_status = Err(report(
                    "process exit status",
                    Error::UnexpectedVcpuExit(
                        "KVM process has no final task exit status after joining workers"
                            .to_owned(),
                    ),
                ));
            }
        }
    }
    // Worker hooks precede the leader. Owner hooks precede independent forks
    // that may need the parent's deregistration/accounting to finish.
    let owner = notify_tool_exit(
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
        failure,
    )
    .await
    .map_err(|error| report("owner exit", error));
    // A peer can publish after this process's successful local outcome. Its
    // terminal state must still select Cancel for every pending child gate.
    let terminal = wait_for_failure(global_state, failure.map(|failure| failure.run.subscribe()))
        .now_or_never()
        .is_some();
    let children = if pid == tid {
        if terminal
            || outcome.is_err()
            || owner.is_err()
            || workers.is_err()
            || process_status.is_err()
        {
            executor.join_child_processes_after_failure()
        } else {
            executor.join_all_child_processes()
        }
    } else {
        executor.transfer_child_processes_to_owner();
        Ok(())
    };
    let errors = [
        outcome.map(|_| ()),
        workers,
        process_status,
        owner,
        children,
    ]
    .into_iter()
    .filter_map(Result::err)
    .collect();
    Error::combine(errors)?;
    let (stdout, stderr) = executor.take_output();
    Ok((status, stdout, stderr))
}

impl KvmBackend {
    async fn finish_signal_boundary<G: GlobalTool>(
        &mut self,
        executor: &mut ElfExecutor,
        global: &G,
        outcome: reverie::SignalBoundaryOutcome,
    ) -> Result<()> {
        let Some(permit) = executor.owned_delivery_permit() else {
            return Ok(());
        };
        // Consuming notification is after the actual frame/register/mask commit,
        // or on owned terminal/image cleanup. It is never an ordinary posthook
        // request and never waits for a guest rt_sigreturn.
        global
            .on_backend_signal_boundary(reverie::SignalBoundaryReceipt { permit, outcome })
            .await
            .map_err(Error::Reverie)?;
        executor
            .backend_signal_control()
            .process
            .release_delivery(permit)
            .map_err(|errno| Error::Reverie(errno.into()))
    }

    fn cancelled_tool_thread_status(&self, executor: &mut ElfExecutor) -> ToolProcessExit {
        let exit = if let Some(status) = self.guest_thread_group_exit_status() {
            executor.retire_current_thread(status, true)
        } else {
            executor.cancel_current_thread()
        };
        if exit.group {
            self.request_guest_thread_group_exit(exit.status);
        }
        ToolProcessExit {
            exit,
            cancelled: true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_tool_process<T: Tool>(
        &mut self,
        executor: &mut ElfExecutor,
        tool: Arc<T>,
        identity: (Pid, Pid),
        global_state: &T::GlobalState,
        config: &<T::GlobalState as GlobalTool>::Config,
        thread_state: T::ThreadState,
        outcome: Result<ToolProcessExit>,
        start_permitted: bool,
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
        let (pid, tid) = identity;
        // Failed exec already reports the joined worker errors as its primary
        // cause. Do not add those same cached diagnostics a second time.
        let (outcome, exec_worker_failure) = match outcome {
            Err(Error::ExecWorkerTeardown(primary)) => (Err(*primary), true),
            outcome => (outcome, false),
        };
        let settlement = self
            .finish_signal_boundary(
                executor,
                global_state,
                match &outcome {
                    Err(_) => reverie::SignalBoundaryOutcome::Failed,
                    Ok(exit) if exit.cancelled => reverie::SignalBoundaryOutcome::Cancelled,
                    Ok(_) => reverie::SignalBoundaryOutcome::Terminated,
                },
            )
            .await;
        let outcome = match (outcome, settlement) {
            (Ok(exit), Ok(())) => Ok(exit),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(settlement)) => Err(error.with_cleanup(vec![settlement])),
        };
        let outcome = outcome.map_err(|error| executor.with_signal_effects(error, None));
        let outcome = outcome.map_err(|error| self.report_tool_failure("execution", error));
        let natural_exit = outcome
            .as_ref()
            .is_ok_and(|exit| !exit.cancelled && !exit.exit.group);
        let cancelled_exit = retire_peer_cancelled_tool_worker(
            executor,
            identity,
            start_permitted,
            self.guest_thread_group_exit_status(),
            &outcome,
        );
        if let Some(exit) = cancelled_exit
            && exit.exit.group
        {
            self.request_guest_thread_group_exit(exit.exit.status);
        }
        // Retire the exact task generation before consuming hooks can wake a
        // peer. No guest callback can still borrow Tool or descriptor state.
        if cancelled_exit.is_none() {
            match &outcome {
                Ok(exit) => {
                    executor.retire_current_thread(exit.exit.status, exit.exit.group);
                }
                Err(_) => executor.retire_failed_thread(),
            }
        }
        self.release_thread_slot();
        self.clear_registered_worker_tid_before_exit(executor);
        executor.release_files_on_exit();
        self.release_stdin_on_exit();
        if outcome.is_err() && cancelled_exit.is_none() {
            // A failed worker must interrupt live siblings before a leader's
            // natural join can block on an earlier handle. Cancellation does
            // not supply a guest group-exit status; the original error remains.
            if pid == tid {
                self.cancel_guest_threads_after_failure();
            } else {
                self.record_guest_worker_failure(tid.as_raw());
            }
        } else if pid == tid {
            if natural_exit {
                self.join_guest_threads();
            } else {
                self.cancel_guest_threads();
            }
        }
        let workers = if exec_worker_failure {
            Ok(())
        } else {
            self.guest_worker_teardown_result()
        };
        finish_tool_process_after_workers(
            executor,
            tool,
            identity,
            global_state,
            config,
            thread_state,
            outcome,
            cancelled_exit,
            workers,
            self.tool_failure.as_ref(),
        )
        .await
    }

    /// Consume a constructed child whose start gate was cancelled or whose
    /// host spawn failed. Do not manufacture handle_thread_start or admission.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn finish_unstarted_tool<T: Tool>(
        &mut self,
        executor: &mut ElfExecutor,
        tool: Arc<T>,
        identity: (Pid, Pid),
        global_state: &T::GlobalState,
        config: &<T::GlobalState as GlobalTool>::Config,
        thread_state: T::ThreadState,
        failure: Option<Error>,
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
        let outcome = match failure {
            Some(error) => Err(error),
            None => Ok(self.cancelled_tool_thread_status(executor)),
        };
        self.finish_tool_process(
            executor,
            tool,
            identity,
            global_state,
            config,
            thread_state,
            outcome,
            false,
        )
        .await
    }

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
        let result = notify_tool_exit(
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
            self.tool_failure.as_ref(),
        )
        .await;
        // Run the owner's consuming hooks even when an earlier worker failed.
        // Intermediate joins retain errors, so no earlier caller can silently
        // turn the final process result into success.
        match (result, self.guest_worker_teardown_result()) {
            (Ok(()), workers) => workers,
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(workers)) => Err(error.with_cleanup(vec![workers])),
        }
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
        // This public non-ELF loop has no instruction consumer. Establish its
        // ownership locally even if the vCPU previously ran a subscribed Tool.
        self.set_rdtsc_interception(false)?;
        self.set_cpuid_interception(false)?;
        self.vcpu.track_clock()?;
        let tool_stack_top = self.tool_stack_top();
        let pid = Pid::from_raw(self.root_pid);
        let global_state = T::GlobalState::init_global_state(&config).await;
        global_state
            .install_backend_signal_control(None)
            .map_err(Error::Reverie)?;
        let tool = Arc::new(T::new(pid, &config));
        let subscriptions = T::subscriptions(&config);
        let mut thread_state = tool.init_thread_state(pid, None);
        let memory = self.memory.clone();
        let auxv = Vec::new();
        let stack_checked_out = Arc::new(AtomicBool::new(false));

        let outcome: Result<ExitStatus> = async {
            let registers = kvm_registers(self.vcpu.get_regs()?, 0);
            let handler_signal = Arc::new(Mutex::new(None));
            let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
            expose_tool_scratch(&memory, tool_stack_top)?;
            let start_outcome = {
                let mut guest_executor = DirectSyscallExecutor {
                    executor: &mut executor,
                    vcpu: &self.vcpu,
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
                    wait_for_failure(&global_state, None),
                )
                .await
            };
            hide_tool_scratch(&memory, tool_stack_top)?;
            match start_outcome {
                HandlerOutcome::Returned(result) => result.map_err(Error::Reverie)?,
                HandlerOutcome::ThreadCancelled => {
                    return Ok(ExitStatus::SUCCESS);
                }
                HandlerOutcome::RunFailed => return Err(Error::RunAborted),
                HandlerOutcome::ParkedFatal(_) | HandlerOutcome::ParkedCancelled(_) => {
                    return Err(Error::UnexpectedVcpuExit(
                        "parked outcome outside its original syscall callback".to_owned(),
                    ));
                }
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
                                        vcpu: &self.vcpu,
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
                                        wait_for_failure(&global_state, None),
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
                                    HandlerOutcome::ThreadCancelled => {
                                        return Ok(ExitStatus::SUCCESS);
                                    }
                                    HandlerOutcome::RunFailed => return Err(Error::RunAborted),
                                    HandlerOutcome::ParkedFatal(_)
                                    | HandlerOutcome::ParkedCancelled(_) => {
                                        return Err(Error::UnexpectedVcpuExit(
                                            "parked outcome outside its original syscall callback"
                                                .to_owned(),
                                        ));
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
                        return Ok(ExitStatus::SUCCESS);
                    }
                    exit => return Err(Error::UnexpectedVcpuExit(format!("{exit:?}"))),
                }
            }
        }
        .await;
        if outcome.is_err() {
            global_state.report_backend_failure(reverie::BackendFailure {
                pid,
                tid: pid,
                phase: "direct Tool execution",
            });
        }
        let status = outcome.as_ref().copied().unwrap_or(ExitStatus::Exited(255));
        let cleanup = self
            .notify_tool_exit(
                tool,
                (pid, pid),
                &global_state,
                &config,
                thread_state,
                status,
            )
            .await;
        Error::combine(
            [outcome.map(|_| ()), cleanup]
                .into_iter()
                .filter_map(Result::err)
                .collect(),
        )?;
        Ok(global_state)
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
        let completion = self
            .run_static_elf_with_tool_completion::<T>(config, capture_output)
            .await?;
        let (status, stdout, stderr) = completion.result?;
        Ok((completion.global_state, status, stdout, stderr))
    }

    /// Runs the installed ELF and retains GlobalState even after a runtime
    /// failure, so its owner can finish scheduler/global cleanup. An outer
    /// error means setup failed before the global state existed, or ownership
    /// could not be recovered after all owned children were joined.
    pub async fn run_static_elf_with_tool_completion<T>(
        &mut self,
        config: <T::GlobalState as GlobalTool>::Config,
        capture_output: bool,
    ) -> Result<ToolRunCompletion<T::GlobalState>>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        // Resolve thread ownership before any CLONE_THREAD worker is created: an
        // explicit caller override wins, otherwise follow the tool's
        // `Tool::thread_ownership` (default: Tool-owned "follow children"). This
        // is why the KVM backend no longer needs the caller to opt threads in.
        self.resolve_thread_ownership(T::thread_ownership(&config));
        if T::observe_signal_dequeues(&config) && self.thread_ownership.executes_on_host() {
            // This combination is known before GlobalState initialization or
            // consuming the installed image. An uninstrumented worker has no
            // removing Guest on which to acknowledge its pending-state effects.
            return Err(Error::SignalObservationRequiresToolThreads);
        }
        // Capture setup can fail before image consumption or any Tool state.
        // Keep this root owner until the later executor and its workers retire.
        let capture_owner = self.prepare_captured_output(capture_output)?;
        let mut loaded = self.static_elf.take().ok_or(Error::StaticElfNotInstalled)?;
        // Output capture replaces stdout and stderr with the executor's pipes,
        // but an explicitly configured stdin remains the guest's input. Use
        // /dev/null only when the caller supplied no stdin, matching
        // `run_static_elf_captured` while keeping the no-input default.
        if capture_output && loaded.stdin.is_none() {
            loaded.stdin = Some(std::fs::File::open("/dev/null")?);
        }
        let pid = Pid::from_raw(self.root_pid);
        let global_state = Arc::new(T::GlobalState::init_global_state(&config).await);
        let failure = RunFailure::new(&global_state);
        self.tool_failure = Some(FailureContext::new(failure.clone(), pid, pid));
        let mut executor = ElfExecutor::with_output(loaded, capture_owner.clone());
        // Atomic run-level installation precedes Tool/thread construction and
        // the first handle_thread_start. No capability is inferred from a PID.
        let mode = match global_state
            .install_backend_signal_control(Some(executor.backend_signal_control()))
        {
            Ok(mode) => mode,
            Err(error) => {
                let context = FailureContext::new(failure.clone(), pid, pid);
                let error =
                    context.publish("process signal control installation", Error::Reverie(error));
                self.tool_failure = None;
                let global_state = Arc::try_unwrap(global_state).map_err(|_| {
                    Error::UnexpectedVcpuExit("signal setup retained GlobalState".to_owned())
                })?;
                return Ok(ToolRunCompletion {
                    global_state,
                    result: Err(error),
                });
            }
        };
        executor.install_signal_control(mode, &failure);
        let tool = Arc::new(T::new(pid, &config));
        let subscriptions = T::subscriptions(&config);
        let thread_state = tool.init_thread_state(pid, None);
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
            .await;
        let result = match (result, executor.take_process_publication_failure()) {
            (result, None) => result,
            (Ok(_), Some(publication)) => Err(publication),
            (Err(error), Some(publication)) => Err(error.with_cleanup(vec![publication])),
        };
        // All owned children have returned. Do not leave the backend holding
        // a reporter whose Weak owner is about to be consumed or released.
        self.tool_failure = None;
        let global_state = match Arc::try_unwrap(global_state) {
            Ok(global) => global,
            Err(_) => {
                let ownership = Error::UnexpectedVcpuExit(
                    "KVM child retained global Tool state after exit".to_owned(),
                );
                return Err(match result {
                    Err(primary) => primary.with_cleanup(vec![ownership]),
                    Ok(_) => ownership,
                });
            }
        };
        let result = failure
            .complete(result)
            .map(|(status, stdout, stderr)| (conventional_exit_code(status), stdout, stderr));
        Ok(ToolRunCompletion {
            global_state,
            result,
        })
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
        thread_entry: bool,
    ) -> Result<CallbackOutcome<Option<PendingSignal>>>
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
                    thread_entry,
                )
                .await?;
            if matches!(
                pending,
                CallbackOutcome::ThreadCancelled | CallbackOutcome::Completed(Some(_))
            ) || executor.has_pending_exit()
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
        thread_entry: bool,
    ) -> Result<CallbackOutcome<Option<PendingSignal>>>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        if fault.is_none() && executor.signal_controlled() && executor.delivery_permit().is_none() {
            let identity = executor.signal_task_identity().ok_or_else(|| {
                Error::UnexpectedVcpuExit("signal return lost task generation".to_owned())
            })?;
            let permit = global_state
                .authorize_backend_signal_boundary(identity)
                .map_err(Error::Reverie)?;
            if permit.is_none() {
                return Ok(CallbackOutcome::Completed(None));
            }
            if executor.delivery_permit() != permit {
                return Err(Error::UnexpectedVcpuExit(
                    "unregistered signal return permit".to_owned(),
                ));
            }
        }
        let selected = if let Some(fault) = fault {
            Ok(Some(fault.pending()))
        } else {
            executor.take_pending_signal_for_delivery()
        };
        flush_pending_signal_effects_with_tool(
            self,
            executor,
            pid,
            tid,
            tool,
            memory,
            auxv,
            fault.map_or_else(
                || kvm_registers(registers, syscall_number),
                PageZeroFault::user_registers,
            ),
            thread_state,
            global_state,
            config,
            subscriptions,
            stack_checked_out,
            None,
        )
        .await?;
        let Some(pending) = selected.map_err(|errno| Error::Reverie(errno.into()))? else {
            return Ok(CallbackOutcome::Completed(None));
        };

        let handler_signal = Arc::new(Mutex::new(None));
        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
        let mut process_completed = false;
        let tool_stack_top = self.tool_stack_top();
        let failure_subscription = self.failure_subscription(executor.is_traced_tree_root());
        expose_tool_scratch(memory, tool_stack_top)?;
        let process_context = if let Some(fault) = fault {
            ProcessExecutionContext::FaultBoundary(Box::new(fault.clone()))
        } else if thread_entry {
            ProcessExecutionContext::ThreadEntrySignal
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
                callback_site: None,
                original_syscall: None,
                signal_guard: SignalGuard::Ordinary,
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
                wait_for_failure(global_state.as_ref(), failure_subscription.clone()),
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
            HandlerOutcome::ThreadCancelled => {
                self.start_pending_tool_children(executor, &pending_child_starts)?;
                return Ok(CallbackOutcome::ThreadCancelled);
            }
            HandlerOutcome::RunFailed => {
                return Err(self.cleanup_unstarted_tool_children_after_error(
                    executor,
                    &pending_child_starts,
                    Error::RunAborted,
                ));
            }
            HandlerOutcome::ParkedFatal(_) | HandlerOutcome::ParkedCancelled(_) => {
                return Err(Error::UnexpectedVcpuExit(
                    "parked outcome outside its original syscall callback".to_owned(),
                ));
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
                        .prepare_filtered_signal_delivery(event, pending.domain)
                        .map_err(|errno| Error::Reverie(errno.into()))?
                };
                if pending.is_some_and(|pending| {
                    executor.signal_disposition(pending.event.signal())
                        == crate::executor::SignalDisposition::Ignore
                }) {
                    Ok(CallbackOutcome::Completed(None))
                } else {
                    Ok(CallbackOutcome::Completed(pending))
                }
            }
            None => Ok(CallbackOutcome::Completed(None)),
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
        if let Some(failure) = &self.tool_failure {
            self.tool_failure = Some(failure.for_thread(tid));
        }
        executor.observe_ignored_signals_with_tool();
        if T::observe_signal_dequeues(config) {
            executor.enable_signal_dequeues();
        }
        let tool_stack_top = self.tool_stack_top();
        let failure_subscription = self.failure_subscription(executor.is_traced_tree_root());
        let mut _registration = None;
        let mut auxv = executor.auxv().to_vec();
        // Clones share the MAP_SHARED guest mapping; a mutable handle lets the
        // loop write syscall results back into the guest's frame.
        let mut memory = self.memory.clone();
        let stack_checked_out = Arc::new(AtomicBool::new(false));

        let handler_signal = Arc::new(Mutex::new(None));
        let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
        let mut _process_completed = false;
        let outcome: Result<ToolProcessExit> = async {
            // Each actual Tool consumer admits its own subscription before
            // any user instruction. New fork/thread vCPUs start unarmed; the
            // tool-less Host worker loop never inherits trapping without a hook.
            self.set_rdtsc_interception(subscriptions.has_rdtsc())?;
            self.set_cpuid_interception(subscriptions.has_cpuid())?;
            self.vcpu.track_clock()?;
            _registration = Some(self.register_guest_thread()?);
            let registers = kvm_registers(self.vcpu.get_regs()?, 0);
            expose_tool_scratch(&memory, tool_stack_top)?;
            let start_outcome = {
                let mut guest_executor = StaticElfSyscallExecutor {
                    backend: self,
                    executor,
                    memory: memory.clone(),
                    process_context: ProcessExecutionContext::Lifecycle,
                    callback_site: None,
                    original_syscall: None,
                    signal_guard: SignalGuard::Ordinary,
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
                    pending_child_starts.clone(),
                    wait_for_failure(global_state.as_ref(), failure_subscription.clone()),
                )
                .await
            };
            hide_tool_scratch(&memory, tool_stack_top)?;
            match start_outcome {
                HandlerOutcome::ThreadCancelled => {
                    self.start_pending_tool_children(executor, &pending_child_starts)?;
                    return Ok(self.cancelled_tool_thread_status(executor));
                }
                HandlerOutcome::Returned(result) => result.map_err(Error::Reverie)?,
                HandlerOutcome::RunFailed => return Err(Error::RunAborted),
                HandlerOutcome::ParkedFatal(_) | HandlerOutcome::ParkedCancelled(_) => {
                    return Err(Error::UnexpectedVcpuExit(
                        "parked outcome outside its original syscall callback".to_owned(),
                    ));
                }
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
                return Ok(self.cancelled_tool_thread_status(executor));
            }
            auxv = executor.auxv().to_vec();
            if let Some(exit) = executor.take_exit() {
                if exit.group {
                    self.request_guest_thread_group_exit(exit.status);
                }
                return Ok(exit.into());
            }

            if initial_post_exec {
                // The root ELF image is already installed when this backend begins.
                // Present the same initial exec syscall and successful-exec lifecycle
                // boundaries as ptrace without loading the installed image twice.
                if subscriptions
                    .iter_syscalls()
                    .any(|number| number == reverie::syscalls::Sysno::execve)
                {
                    let initial_outcome = run_initial_exec_handler(
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
                    if matches!(initial_outcome, CallbackOutcome::ThreadCancelled) {
                        return Ok(self.cancelled_tool_thread_status(executor));
                    }
                    auxv = executor.auxv().to_vec();
                    if let Some(exit) = executor.take_exit() {
                        if exit.group {
                            self.request_guest_thread_group_exit(exit.status);
                        }
                        return Ok(exit.into());
                    }
                }
                let post_exec_outcome = run_post_exec_handler(
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
                .await;
                let post_exec_error = match post_exec_outcome {
                    Ok(CallbackOutcome::ThreadCancelled) => {
                        return Ok(self.cancelled_tool_thread_status(executor));
                    }
                    Ok(CallbackOutcome::Completed(())) => None,
                    Err(error) => Some(error),
                };
                if let Some(error) = post_exec_error {
                    return Err(error);
                }
            }

            // CLONE_THREAD has a complete user continuation before its first
            // KVM_RUN. Admission comes first; then deliver an already queued event
            // without executing a synthetic guest syscall or the child's first
            // instruction. Root/exec lifecycle contexts keep their existing limit.
            if pid != tid && executor.has_eligible_pending_signal() {
                let entry_registers = self.vcpu.get_regs()?;
                executor.set_current_user_stack_pointer(entry_registers.rsp);
                let pending = self
                    .filter_one_pending_signal_with_tool(
                        executor,
                        pid,
                        tid,
                        &tool,
                        &memory,
                        &auxv,
                        entry_registers,
                        u64::MAX,
                        self.syscall_frame_address,
                        &mut thread_state,
                        &global_state,
                        config,
                        subscriptions,
                        &stack_checked_out,
                        true,
                    )
                    .await?;
                let pending = match pending {
                    CallbackOutcome::Completed(pending) => pending,
                    CallbackOutcome::ThreadCancelled => {
                        return Ok(self.cancelled_tool_thread_status(executor));
                    }
                };
                let delivered = if !executor.has_pending_exit()
                    && let Some(pending) = pending
                {
                    self.deliver_selected_signal_before_thread_entry(
                        executor,
                        entry_registers,
                        pending,
                    )?
                } else {
                    false
                };
                let outcome = if executor.has_pending_exit() {
                    reverie::SignalBoundaryOutcome::Terminated
                } else if delivered {
                    reverie::SignalBoundaryOutcome::Caught
                } else {
                    reverie::SignalBoundaryOutcome::NoHandler
                };
                self.finish_signal_boundary(executor, global_state.as_ref(), outcome)
                    .await?;
            }

            if let Some((segment, address)) = executor.take_segment() {
                set_user_segment_base(&self.vcpu, segment, address)?;
            }
            if let Some(exit) = executor.take_exit() {
                if exit.group {
                    self.request_guest_thread_group_exit(exit.status);
                }
                return Ok(exit.into());
            }

            // Read once so the per-syscall classifier can borrow it while `self` is
            // borrowed elsewhere in the loop body.
            let thread_ownership = self.thread_ownership;
            loop {
                if wait_for_failure(global_state.as_ref(), failure_subscription.clone())
                    .now_or_never()
                    .is_some()
                {
                    return Err(Error::RunAborted);
                }
                if let Some(status) = self.guest_thread_group_exit_status() {
                    return Ok(ProcessExit {
                        status,
                        group: true,
                    }
                    .into());
                }
                if self.guest_thread_is_cancelled() {
                    return Ok(self.cancelled_tool_thread_status(executor));
                }
                let vcpu_exit = match self.vcpu.run() {
                    Ok(exit) => exit,
                    Err(Error::Kvm(error)) if error.errno() == libc::EINTR => continue,
                    Err(error) => return Err(error),
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
                        if let Some(boundary) = self.tool_instruction_exception()? {
                            executor.set_current_user_stack_pointer(boundary.user_registers().rsp);
                            let handler_signal = Arc::new(Mutex::new(None));
                            let pending_child_starts = Arc::new(Mutex::new(Vec::new()));
                            let mut process_completed = false;
                            expose_tool_scratch(&memory, tool_stack_top)?;
                            let result = {
                                let mut guest_executor = StaticElfSyscallExecutor {
                                    backend: self,
                                    executor,
                                    memory: memory.clone(),
                                    process_context: ProcessExecutionContext::Instruction,
                                    callback_site: None,
                                    original_syscall: None,
                                    signal_guard: SignalGuard::Ordinary,
                                    last_result: None,
                                    process_completed: &mut process_completed,
                                };
                                let mut guest = KvmGuest::<T>::new(
                                    pid,
                                    tid,
                                    tool.clone(),
                                    memory.clone(),
                                    &auxv,
                                    boundary.user_registers(),
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
                                    async {
                                        match &boundary {
                                            crate::vm::ToolInstructionBoundary::Timestamp(
                                                boundary,
                                            ) => tool
                                                .handle_rdtsc_event(
                                                    &mut guest,
                                                    boundary.instruction.request,
                                                )
                                                .await
                                                .map(crate::vm::ToolInstructionResult::Timestamp),
                                            crate::vm::ToolInstructionBoundary::Cpuid(boundary) => {
                                                tool.handle_cpuid_event(
                                                    &mut guest,
                                                    boundary.registers.rax as u32,
                                                    boundary.registers.rcx as u32,
                                                )
                                                .await
                                                .map(crate::vm::ToolInstructionResult::Cpuid)
                                            }
                                        }
                                    },
                                    handler_signal,
                                    pending_child_starts,
                                    wait_for_failure(
                                        global_state.as_ref(),
                                        failure_subscription.clone(),
                                    ),
                                )
                                .await
                            };
                            // Hide scratch even on callback failure. The outer
                            // process supervisor retains any committed effects.
                            let hidden = hide_tool_scratch(&memory, tool_stack_top);
                            let value = match result {
                                HandlerOutcome::Returned(Ok(value)) => value,
                                HandlerOutcome::Returned(Err(error)) => {
                                    return Err(Error::Reverie(error.into())
                                        .with_cleanup(hidden.err().into_iter().collect()));
                                }
                                HandlerOutcome::ThreadCancelled => {
                                    hidden?;
                                    return Ok(self.cancelled_tool_thread_status(executor));
                                }
                                HandlerOutcome::RunFailed => {
                                    return Err(Error::RunAborted
                                        .with_cleanup(hidden.err().into_iter().collect()));
                                }
                                HandlerOutcome::RuntimeError(error) => {
                                    return Err(
                                        error.with_cleanup(hidden.err().into_iter().collect())
                                    );
                                }
                                HandlerOutcome::TailInjected {
                                    result: Ok(_),
                                    image_replaced: false,
                                    process_exited: true,
                                } => {
                                    hidden?;
                                    let exit = executor.take_exit().ok_or_else(|| {
                                        Error::UnexpectedVcpuExit(
                                            "terminal instruction injection lost its exit"
                                                .to_owned(),
                                        )
                                    })?;
                                    if exit.group {
                                        self.request_guest_thread_group_exit(exit.status);
                                    }
                                    // The original instruction never resumes. Normal
                                    // retirement and consuming hooks own cleanup.
                                    return Ok(exit.into());
                                }
                                HandlerOutcome::TailInjected { .. }
                                | HandlerOutcome::ParkedFatal(_)
                                | HandlerOutcome::ParkedCancelled(_) => {
                                    return Err(Error::UnexpectedVcpuExit(
                                        "nonreturning instruction callback outcome".to_owned(),
                                    )
                                    .with_cleanup(hidden.err().into_iter().collect()));
                                }
                            };
                            hidden?;
                            if process_completed {
                                return Err(Error::UnexpectedVcpuExit(
                                    "instruction callback changed the process continuation"
                                        .to_owned(),
                                ));
                            }
                            if let Some((segment, address)) = executor.take_segment() {
                                set_user_segment_base(&self.vcpu, segment, address)?;
                            }
                            self.resume_tool_instruction(boundary, value)?;
                            continue;
                        }
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
                                false,
                            )
                            .await?;
                        let pending = match pending {
                            CallbackOutcome::Completed(pending) => pending,
                            CallbackOutcome::ThreadCancelled => {
                                return Ok(self.cancelled_tool_thread_status(executor));
                            }
                        };
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
                            return Ok(exit.into());
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
                    let mut signal_delivered = false;
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
                                false,
                            )
                            .await?;
                        let pending = match pending {
                            CallbackOutcome::Completed(pending) => pending,
                            CallbackOutcome::ThreadCancelled => {
                                return Ok(self.cancelled_tool_thread_status(executor));
                            }
                        };
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
                        signal_delivered = delivered;
                        if !delivered && signal_exit.is_none() {
                            stage_process_syscall_return(
                                &mut memory,
                                &self.vcpu,
                                frame_address,
                                restored,
                            )?;
                        }
                    }
                    let outcome = if signal_exit.is_some() || executor.has_pending_exit() {
                        reverie::SignalBoundaryOutcome::Terminated
                    } else if signal_delivered {
                        reverie::SignalBoundaryOutcome::Caught
                    } else {
                        reverie::SignalBoundaryOutcome::NoHandler
                    };
                    self.finish_signal_boundary(executor, global_state.as_ref(), outcome)
                        .await?;
                    signal_exit = signal_exit.or_else(|| executor.take_exit());
                    if let Some(exit) = signal_exit {
                        self.discard_process_clear_tid_at_signal_return(executor);
                        if exit.group {
                            self.request_guest_thread_group_exit(exit.status);
                        }
                        return Ok(exit.into());
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
                let (
                    mut result,
                    handler_replaced_image,
                    _handler_process_completed,
                    restart_requested,
                ) = if subscribed {
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
                                callback_site: None,
                                original_syscall: Some(request),
                                signal_guard: SignalGuard::Ordinary,
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
                                wait_for_failure(
                                    global_state.as_ref(),
                                    failure_subscription.clone(),
                                ),
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
                                        && (executor.has_prepared_signal()
                                            || executor.has_eligible_pending_signal()) =>
                                    {
                                        // A prepared signal was already removed from
                                        // pending. Deliver its frame before another
                                        // callback invalidates the selection's nonce;
                                        // the frame's SA_RESTART policy decides whether
                                        // the syscall runs again after the handler.
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
                            HandlerOutcome::ThreadCancelled => {
                                self.start_pending_tool_children(executor, &pending_child_starts)?;
                                return Ok(self.cancelled_tool_thread_status(executor));
                            }
                            HandlerOutcome::RunFailed => return Err(Error::RunAborted),
                            HandlerOutcome::ParkedFatal(selection) => {
                                if !executor.prepared_signal_is_fatal(selection) {
                                    return Err(Error::UnexpectedVcpuExit(
                                        "fatal selection identity changed".to_owned(),
                                    ));
                                }
                                let pending = executor
                                    .take_prepared_signal()
                                    .map_err(|errno| Error::Reverie(errno.into()))?
                                    .expect("validated fatal selection");
                                self.deliver_selected_signal_from_registers(
                                    executor,
                                    frame_address,
                                    registers,
                                    pending,
                                )?;
                                let exit = executor.take_exit().ok_or_else(|| {
                                    Error::UnexpectedVcpuExit(
                                        "fatal parked selection did not terminate".to_owned(),
                                    )
                                })?;
                                self.finish_signal_boundary(
                                    executor,
                                    global_state.as_ref(),
                                    reverie::SignalBoundaryOutcome::Terminated,
                                )
                                .await?;
                                executor.finish_parked_delivery();
                                if exit.group {
                                    self.request_guest_thread_group_exit(exit.status);
                                }
                                return Ok(exit.into());
                            }
                            HandlerOutcome::ParkedCancelled(context) => {
                                if !executor.signal_failure_context_is_current(context) {
                                    return Err(Error::UnexpectedVcpuExit(
                                        "parked cancellation ledger identity changed".to_owned(),
                                    ));
                                }
                                return Err(executor.with_signal_effects(Error::RunAborted, None));
                            }
                            HandlerOutcome::RuntimeError(error) => {
                                let error = self.cleanup_unstarted_tool_children_after_error(
                                    executor,
                                    &pending_child_starts,
                                    error,
                                );
                                return Err(error);
                            }
                        };
                        self.start_pending_tool_children(executor, &pending_child_starts)?;
                        break classified;
                    }
                } else {
                    executor.reserve_signal_effects(64).map_err(|errno| {
                        executor.with_signal_effects(Error::Reverie(errno.into()), None)
                    })?;
                    let raw = executor.execute(&request, &memory);
                    flush_pending_signal_effects_with_tool(
                        self,
                        executor,
                        pid,
                        tid,
                        &tool,
                        &memory,
                        &auxv,
                        kvm_registers(registers, request.number()),
                        &mut thread_state,
                        &global_state,
                        config,
                        subscriptions,
                        &stack_checked_out,
                        Some(raw),
                    )
                    .await?;
                    (raw, false, false, false)
                };
                let mut returned_registers = process_syscall_return_registers(
                    &memory,
                    registers,
                    frame_address,
                    result,
                    None,
                )?;
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
                        .await;
                    let outcome = outcome?;
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
                            .then(|| {
                                restart_syscall_registers(returned_registers, request.number())
                            })
                            .transpose()?;
                    }
                    replaced_image |= outcome.image_replaced;
                    self.start_pending_tool_children(executor, &pending_child_starts)?;
                }
                if replaced_image {
                    self.finish_signal_boundary(
                        executor,
                        global_state.as_ref(),
                        reverie::SignalBoundaryOutcome::ImageReplaced,
                    )
                    .await?;
                }
                if replaced_image {
                    auxv = executor.auxv().to_vec();
                    let post_exec_outcome = run_post_exec_handler(
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
                    .await;
                    let post_exec_error = match post_exec_outcome {
                        Ok(CallbackOutcome::ThreadCancelled) => {
                            return Ok(self.cancelled_tool_thread_status(executor));
                        }
                        Ok(CallbackOutcome::Completed(())) => None,
                        Err(error) => Some(error),
                    };
                    if let Some(error) = post_exec_error {
                        return Err(error);
                    }
                }
                if let Some((segment, address)) = executor.take_segment() {
                    set_user_segment_base(&self.vcpu, segment, address)?;
                }
                let mut signal_boundary_outcome = reverie::SignalBoundaryOutcome::NoHandler;
                if !replaced_image && pending_exit.is_none() {
                    let pending = if let Some(prepared) = executor
                        .take_prepared_signal()
                        .map_err(|errno| Error::Reverie(errno.into()))?
                    {
                        CallbackOutcome::Completed(Some(prepared))
                    } else {
                        self.filter_one_pending_signal_with_tool(
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
                            false,
                        )
                        .await?
                    };
                    let pending = match pending {
                        CallbackOutcome::Completed(pending) => pending,
                        CallbackOutcome::ThreadCancelled => {
                            return Ok(self.cancelled_tool_thread_status(executor));
                        }
                    };
                    if let Some((segment, address)) = executor.take_segment() {
                        set_user_segment_base(&self.vcpu, segment, address)?;
                    }
                    pending_exit = pending_exit.or_else(|| executor.take_exit());
                    let mut delivered = false;
                    if pending_exit.is_none()
                        && let Some(pending) = pending
                    {
                        let signal_registers = if restart_requested
                            && executor.caught_signal_restarts_syscall(pending)
                        {
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
                    signal_boundary_outcome = if pending_exit.is_some() {
                        reverie::SignalBoundaryOutcome::Terminated
                    } else if delivered {
                        reverie::SignalBoundaryOutcome::Caught
                    } else {
                        reverie::SignalBoundaryOutcome::NoHandler
                    };
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
                if pending_exit.is_some() {
                    signal_boundary_outcome = reverie::SignalBoundaryOutcome::Terminated;
                }
                self.finish_signal_boundary(
                    executor,
                    global_state.as_ref(),
                    signal_boundary_outcome,
                )
                .await?;
                executor.finish_parked_delivery();
                pending_exit = pending_exit.or_else(|| executor.take_exit());
                if let Some(exit) = pending_exit {
                    if exit.group {
                        self.request_guest_thread_group_exit(exit.status);
                    }
                    return Ok(exit.into());
                }
            }
        }
        .await;
        let outcome = outcome.map_err(|error| {
            self.cleanup_unstarted_tool_children_after_error(executor, &pending_child_starts, error)
        });
        self.finish_tool_process(
            executor,
            tool,
            (pid, tid),
            global_state.as_ref(),
            config,
            thread_state,
            outcome,
            true,
        )
        .await
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
            Ok(ChildStartCommand::CancelAfterFailure) => {
                panic!("normal handler sent fatal cancellation")
            }
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
                std::future::pending(),
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
            std::future::pending(),
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
        fn read_clock(&self) -> Result<u64> {
            Err(Error::GuestClock(
                "side-effect test has no guest counter".into(),
            ))
        }

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

        fn tail_injection_allowed(&self, _request: &SyscallRequest) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct PermissiveSideEffectingExecutor {
        state: TailInjectionSideEffects,
    }

    impl GuestSyscallExecutor<crate::StraceTool> for PermissiveSideEffectingExecutor {
        fn read_clock(&self) -> Result<u64> {
            Err(Error::GuestClock(
                "side-effect test has no guest counter".into(),
            ))
        }

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
            std::future::pending(),
        )) {
            HandlerOutcome::RuntimeError(Error::Reverie(reverie::Error::Errno(errno))) => {
                assert_eq!(errno, Errno::ENOSYS)
            }
            HandlerOutcome::ParkedFatal(_) | HandlerOutcome::ParkedCancelled(_) => {
                panic!("tail refusal produced unrelated parked outcome")
            }
            HandlerOutcome::RuntimeError(error) => panic!("unexpected tail refusal: {error}"),
            HandlerOutcome::TailInjected { .. } => panic!("tail injection unexpectedly ran"),
            HandlerOutcome::ThreadCancelled => {
                panic!("tail injection unexpectedly cancelled the thread")
            }
            HandlerOutcome::Returned(_) => panic!("tail injection unexpectedly returned"),
            HandlerOutcome::RunFailed => panic!("unexpected run failure"),
        }
    }

    fn test_process_boundary() -> CompletedSyscallBoundary {
        CompletedSyscallBoundary::for_test()
    }

    #[test]
    fn signal_hook_tail_injection_is_rejected_before_every_executor_side_effect() {
        let boundary = test_process_boundary();
        let request = SyscallRequest::new(libc::SYS_write as u64, [1, 0x100, 4, 0, 0, 0]);
        assert!(
            !ProcessExecutionContext::SignalBoundary(boundary.clone())
                .tail_injection_allowed(&request)
        );
        assert!(
            ProcessExecutionContext::SyscallBoundary(boundary.clone())
                .tail_injection_allowed(&request)
        );
        assert!(ProcessExecutionContext::Lifecycle.tail_injection_allowed(&request));

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

#[cfg(test)]
#[path = "terminal_runtime_tests.rs"]
mod terminal_tests;

#[cfg(test)]
#[path = "child_exit_runtime_tests.rs"]
mod child_exit_tests;

#[cfg(test)]
#[path = "runtime/failure_tests.rs"]
mod failure_tests;

#[cfg(test)]
#[path = "process_alarm_runtime_tests.rs"]
mod process_alarm_tests;

#[cfg(test)]
#[path = "captured_write_runtime_tests.rs"]
mod captured_write_tests;
