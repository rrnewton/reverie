/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
*/

//! `TracedTask` and its methods.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::ops::DerefMut;
use std::os::unix::ffi::OsStringExt;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock as StdOnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;

use async_trait::async_trait;
use futures::future;
use futures::future::Either;
use futures::future::Future;
use futures::future::FutureExt;
use futures::future::TryFutureExt;
use nix::sys::mman::ProtFlags;
use nix::sys::signal::Signal;
use reverie::BackendFailure;
use reverie::Backtrace;
use reverie::Errno;
use reverie::ExitStatus;
use reverie::Frame;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::InjectedSyscallEvent;
use reverie::Never;
use reverie::Pid;
#[cfg(target_arch = "x86_64")]
use reverie::Rdtsc;
use reverie::Subscription;
use reverie::Tid;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Mprotect;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use safeptrace::ChildOp;
use safeptrace::Error as TraceError;
use safeptrace::Event;
use safeptrace::Running;
use safeptrace::Stopped;
use safeptrace::TerminalCleanup;
use safeptrace::Wait;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::LiteinstInstrumentationStats;
use crate::PtraceBackendStatsSource;
use crate::children;
use crate::cp;
use crate::error::Error;
use crate::error::LiteinstActivationFailure;
use crate::error::LiteinstActivationFailureReason;
use crate::error::LiteinstActivationOperation;
use crate::error::LiteinstActivationStage;
use crate::error::TraceResultExt;
use crate::error::liteinst_activation_failure_reason;
use crate::failure::PtraceCleanupFailure;
use crate::failure::PtraceRunFailure;
use crate::gdbstub::BreakpointType;
use crate::gdbstub::CoreRegs;
use crate::gdbstub::GdbRequest;
use crate::gdbstub::GdbServer;
use crate::gdbstub::ResumeAction;
use crate::gdbstub::ResumeInferior;
use crate::gdbstub::StopEvent;
use crate::gdbstub::StopReason;
use crate::gdbstub::StoppedInferior;
use crate::injected_syscall::InjectedSyscallFrame;
use crate::liteinst_stats::LiteinstPatchOutcome;
use crate::regs::Reg;
use crate::regs::RegAccess;
use crate::stack::GuestStack;
use crate::timer::HandleFailure;
use crate::timer::Timer;
use crate::timer::TimerEventRequest;
use crate::tracer::FatalNewborn;
use crate::tracer::FatalTaskStop;
use crate::tracer::HeldRootStop;
use crate::tracer::NewbornTracee;
use crate::tracer::RootStopLease;
use crate::tracer::TraceeIdentity;
use crate::vdso;

// One-use observation of the actual injected Seccomp stop. The failure-only
// backup comes from this same Stopped, never a raw decoder or new PID capture.
// It cannot repair the test's sealed pre-rescue lease observation.
#[cfg(all(test, target_arch = "x86_64"))]
pub(crate) struct SourceInjectedStopReceipt {
    pub(crate) tid: Pid,
    pub(crate) seccomp: bool,
    pub(crate) number: u64,
    pub(crate) arguments: [u64; 6],
    pub(crate) ip: usize,
    pub(crate) terminal: TerminalCleanup,
    pub(crate) held: Arc<StdMutex<Option<HeldRootStop>>>,
    pub(crate) failure_cleanup: Option<HeldRootStop>,
}

#[cfg(all(test, target_arch = "x86_64"))]
pub(crate) struct SourceInjectedStopGate {
    pub(crate) tid: Pid,
    pub(crate) entered: oneshot::Sender<SourceInjectedStopReceipt>,
}

#[cfg(all(test, target_arch = "x86_64"))]
thread_local! {
    pub(crate) static SOURCE_INJECTED_STOP_GATE: std::cell::RefCell<Option<SourceInjectedStopGate>> = const { std::cell::RefCell::new(None) };
}

// A lifecycle association failure must prevent the following resume, not just
// report a flag whose waiter may be polled after this future resumes the guest.
fn observe_ready_thread_state<T: Tool>(
    tool: &T,
    pid: Pid,
    tid: Tid,
    global: &T::GlobalState,
    state: &T::ThreadState,
) -> Result<(), Errno> {
    tool.on_thread_state_ready(tid, global, state)
        .map_err(|error| {
            tracing::error!(%pid, %tid, %error, "owned Tool state association failed");
            global.report_backend_failure(reverie::BackendFailure {
                pid,
                tid,
                phase: "owned Tool state association failed",
            });
            Errno::EPROTO
        })
}

#[cfg(test)]
mod thread_state_ready_tests;

#[cfg(test)]
mod local_global_tests;

fn validate_liteinst_user_regs_update(
    current: &libc::user_regs_struct,
    requested: &libc::user_regs_struct,
) -> Result<(), Errno> {
    if current.rsp == requested.rsp {
        Ok(())
    } else {
        Err(Errno::ENOTSUPP)
    }
}

#[cfg(target_arch = "x86_64")]
fn liteinst_helper_entry_rflags(flags: u64) -> u64 {
    const RFLAGS_TF: u64 = 1 << 8;
    const RFLAGS_DF: u64 = 1 << 10;
    const RFLAGS_RF: u64 = 1 << 16;
    const RFLAGS_AC: u64 = 1 << 18;
    flags & !(RFLAGS_TF | RFLAGS_DF | RFLAGS_RF | RFLAGS_AC)
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstCpuidPolicy {
    Unsupported,
    UnchangedEnabled,
    RestoreDisabled,
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstTscPolicy {
    Unsupported,
    UnchangedEnabled,
    RestoreFaulting,
}

#[cfg(target_arch = "x86_64")]
struct LiteinstHelperSavedState {
    cpuid_policy: LiteinstCpuidPolicy,
    tsc_policy: LiteinstTscPolicy,
    regs: libc::user_regs_struct,
    xstate: safeptrace::XState,
    stack_address: usize,
    stack_value: u64,
}

#[cfg(target_arch = "x86_64")]
fn is_legacy_vsyscall_ip(ip: Reg) -> bool {
    const VSYSCALL_START: Reg = 0xffff_ffff_ff60_0000;
    const VSYSCALL_END: Reg = VSYSCALL_START + 0x1000;

    (VSYSCALL_START..VSYSCALL_END).contains(&ip)
}

#[derive(Debug)]
struct Suspended {
    waker: Option<mpsc::Sender<Pid>>,
    suspended: Arc<AtomicBool>,
}

/// Expected resume action sent by gdb client, when the task is in a gdb stop.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ExpectedGdbResume {
    /// Expecting a normal gdb resume, either single step, until or continue
    Resume,
    /// Expecting a gdb step over, this happens the underlying task hit a sw
    /// breakpoint, gdb then needs to restore the original instruction --
    /// which implies deleting the breakpoint, single-step, then restore
    /// the breakpoint. This is a special case because we need to serialize
    /// the whole operation, otherwise when there's a different thread in
    /// the same process group which share the same breakpoint, removing
    /// breakpoint can cause the 2nd thread to miss the breakpoint.
    StepOver,
    /// Force single-step, even if Resume(continue) is requested. This
    /// is a workaround when fork/vfork/clone event is reported to gdb,
    /// gdb could then issue an `vCont;p<pid>:-1` to resume all threads in
    /// the thread group, which could cause the main thread to miss events.
    StepOnly,
}

enum OrdinaryStart {
    Stopped(Stopped),
    Exec(Stopped, Pid),
}

/// A same-process task rendezvous authorized only by an actual leader Exec
/// event naming this former TID. No numeric-PID inference can request it.
struct OrdinaryExecSlot<L: Tool> {
    stop: Arc<FatalTaskStop>,
    requested: AtomicBool,
    changed: Notify,
    transferred: StdMutex<Option<Box<TracedTask<L>>>>,
}
impl<L: Tool> OrdinaryExecSlot<L> {
    async fn requested(&self) {
        loop {
            let changed = self.changed.notified();
            if self.requested.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }
    async fn take(&self) -> Box<TracedTask<L>> {
        loop {
            let changed = self.changed.notified();
            if let Some(task) = self.transferred.lock().unwrap().take() {
                return task;
            }
            changed.await;
        }
    }
}

pub struct Child {
    id: Pid,
    /// Task is suspended, either stopped by gdb (client), or received
    /// SIGSTOP sent by other threads in the same process group.
    suspended: Arc<AtomicBool>,
    /// Notify a task reached SIGSTOP.
    wait_all_stop_tx: Option<mpsc::Sender<(Pid, Suspended)>>,
    /// Channel to receive if a child task is becoming a daemon, when
    /// `daemonize()` is called.
    pub(crate) daemonizer_rx: Option<mpsc::Receiver<broadcast::Receiver<()>>>,
    /// Join handle to let child task exit gracefully.
    pub(crate) handle: ChildCompletion,
    /// Subscription to a successfully captured original group. The session
    /// retains its authority until actual retirement and consuming hooks finish;
    /// completed Child history must not retain the generation's descriptors.
    pub(crate) ordinary_group: Option<OrdinaryGroupSubscription>,
    // Same admitted generation, retained only for a possible later adoption.
    pub(crate) terminal: Option<Arc<TerminalCleanup>>,
}

impl Child {
    /// Child task identifier.
    pub fn id(&self) -> Pid {
        self.id
    }
}

impl fmt::Debug for Child {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Child").field("id", &self.id).finish()
    }
}

pub(crate) enum ChildCompletion {
    Legacy(JoinHandle<Option<ExitStatus>>),
    Owned {
        receiver: oneshot::Receiver<Option<ExitStatus>>,
        finished: Arc<AtomicBool>,
    },
}

impl Future for ChildCompletion {
    type Output = Result<Option<ExitStatus>, reverie::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.get_mut() {
            Self::Legacy(handle) => handle
                .poll_unpin(cx)
                .map_err(|error| anyhow::Error::new(error).into()),
            Self::Owned { receiver, .. } => receiver
                .poll_unpin(cx)
                .map_err(|error| anyhow::Error::new(error).into()),
        }
    }
}

impl ChildCompletion {
    fn is_finished(&self) -> bool {
        match self {
            Self::Legacy(handle) => handle.is_finished(),
            // The session owns the JoinHandle, while this marker reports only
            // that the body crossed its consuming-cleanup boundary. It does
            // not consume the retained oneshot result needed by the final
            // owner drain.
            Self::Owned { finished, .. } => finished.load(Ordering::Acquire),
        }
    }
}

impl Future for Child {
    type Output = Result<Option<ExitStatus>, reverie::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        self.handle.poll_unpin(cx)
    }
}

pub type Children = children::Children<Child>;

/// The ptrace event opcode describes Linux's notification choice, not whether
/// the new task shares its creator's thread group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChildTaskKind {
    Thread,
    Process,
}

// One ownership slot, from native event custody through the ordinary list.
enum PendingChild {
    Native(Box<NativeChild>),
    Spawned(ChildTaskKind, Child),
}

struct NativeChild {
    id: Pid,
    creator: Pid,
    creator_cleanup: TerminalCleanup,
    cleanup: TerminalCleanup,
    kind: ChildTaskKind,
    initial: InitialChildWait,
    admission_required: bool,
    admission_done: bool,
    child_restore_context: Option<libc::user_regs_struct>,
    cohort: Option<source_cohort::ChildMembership>,
}

/// Only a real final wait admits the terminal variant. It owns no perf
/// resource and cannot be used as an unsupported-perf or zero-clock timer.
enum TaskTimer {
    Live(Timer),
    Terminal(ExitStatus),
}

impl std::ops::Deref for TaskTimer {
    type Target = Timer;

    fn deref(&self) -> &Timer {
        match self {
            Self::Live(timer) => timer,
            Self::Terminal(status) => {
                panic!("terminal-only child has no live timer: {status:?}")
            }
        }
    }
}

impl std::ops::DerefMut for TaskTimer {
    fn deref_mut(&mut self) -> &mut Timer {
        match self {
            Self::Live(timer) => timer,
            Self::Terminal(status) => {
                panic!("terminal-only child has no live timer: {status:?}")
            }
        }
    }
}

enum PreparedNewborn {
    Live {
        child: Stopped,
        event: Event,
        timer: Box<Timer>,
    },
    Terminal {
        id: Pid,
        status: ExitStatus,
    },
}

impl PreparedNewborn {
    fn into_parts(self) -> (Result<Wait, TraceError>, TaskTimer) {
        match self {
            Self::Live {
                child,
                event,
                timer,
            } => (Ok(Wait::Stopped(child, event)), TaskTimer::Live(*timer)),
            Self::Terminal { id, status } => {
                (Ok(Wait::Exited(id, status)), TaskTimer::Terminal(status))
            }
        }
    }
}

// The future and its exact outcome remain in the actor if the caller's await
// is dropped. In particular, Ready is stored before poll returns to the caller.
enum InitialChildWait {
    Waiting(Pin<Box<dyn Future<Output = Result<PreparedNewborn, TraceError>> + Send + Sync>>),
    Observed(Result<PreparedNewborn, TraceError>),
}

impl InitialChildWait {
    async fn observe(&mut self) {
        future::poll_fn(|cx| {
            let outcome = match self {
                Self::Waiting(wait) => match wait.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(outcome) => outcome,
                },
                Self::Observed(_) => return Poll::Ready(()),
            };
            *self = Self::Observed(outcome);
            Poll::Ready(())
        })
        .await
    }

    fn into_observed(self) -> Result<PreparedNewborn, TraceError> {
        match self {
            Self::Observed(outcome) => outcome,
            Self::Waiting(_) => panic!("native initial wait consumed before observation"),
        }
    }
}

struct ChildDebugChannels {
    request_tx: Option<mpsc::Sender<GdbRequest>>,
    resume_tx: Option<mpsc::Sender<ResumeInferior>>,
    stop_rx: mpsc::Receiver<StoppedInferior>,
}

fn classify_native_child(
    parent_process: Pid,
    parent_tgid: Pid,
    child: Pid,
    child_tgid: Pid,
) -> Result<ChildTaskKind, Errno> {
    if parent_process.as_raw() <= 0
        || parent_tgid != parent_process
        || child.as_raw() <= 0
        || child == parent_process
    {
        return Err(Errno::ECHILD);
    }
    if child_tgid == parent_tgid {
        Ok(ChildTaskKind::Thread)
    } else if child_tgid == child {
        Ok(ChildTaskKind::Process)
    } else {
        Err(Errno::ECHILD)
    }
}

/// Retrying an interrupted identity capture does not resume the guest or
/// repeat an injected syscall. Other errors retain their exact first cause.
fn register_newborn_wait(mut register: impl FnMut() -> Result<(), Errno>) -> Result<(), Errno> {
    loop {
        match register() {
            Err(Errno::EINTR) => continue,
            result => return result,
        }
    }
}

async fn publish_child_to_existing_list(
    pending: &mut Option<PendingChild>,
    threads: &Arc<Mutex<Children>>,
    processes: &Arc<Mutex<Children>>,
) {
    let kind = match pending.as_ref() {
        None => return,
        Some(PendingChild::Spawned(kind, _)) => kind,
        Some(PendingChild::Native(_)) => panic!("native child has no spawned join owner"),
    };
    let children = if *kind == ChildTaskKind::Thread {
        threads
    } else {
        processes
    };
    let mut children = children.lock().await;
    // No await separates consumption from transfer into the ordinary owner.
    let Some(PendingChild::Spawned(_, child)) = pending.take() else {
        unreachable!("spawned child changed while its owner was borrowed");
    };
    // Reuse the original owner, without duplicating its PIDFD. Release pins
    // proven HUP at existing list activity so repeated fork/wait does not
    // accumulate retired generations. Unreaped children survive until the
    // tree's real-parent drain even when their Tool tasks already completed.
    for previous in &mut *children {
        if previous.handle.is_finished()
            && previous
                .terminal
                .as_ref()
                .is_some_and(|owner| owner.is_reaped() == Ok(true))
        {
            previous.terminal = None;
        }
    }
    children.push(child);
}

// This is local to one ordinary NewChild continuation, not a reusable signal
// filter. The injected caller retains its original stopped Tool continuation.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy)]
struct ParentSyscallStepState {
    words: [u64; 27],
    siginfo: (i32, i32, i32),
    fault_address: Option<u64>,
    dr6: u64,
    syscall_opcode: [u8; cp::SYSCALL_INSTR_SIZE],
}

#[cfg(target_arch = "x86_64")]
fn parent_syscall_opcode(
    ip: u64,
    mut read_word: impl FnMut(usize) -> Result<u64, Errno>,
) -> Result<[u8; cp::SYSCALL_INSTR_SIZE], Errno> {
    let site = ip
        .checked_sub(cp::SYSCALL_INSTR_SIZE as u64)
        .ok_or(Errno::EFAULT)?;
    let mut bytes = [0; cp::SYSCALL_INSTR_SIZE];
    for (offset, byte) in bytes.iter_mut().enumerate() {
        let address = site.checked_add(offset as u64).ok_or(Errno::EFAULT)?;
        // Like source_observation::code_byte: PEEKDATA reads a full word.
        // The executed syscall may end at the final byte of a mapped page.
        let word = read_word((address & !7) as usize)?;
        *byte = (word >> ((address & 7) * 8)) as u8;
    }
    Ok(bytes)
}

#[cfg(target_arch = "x86_64")]
impl ParentSyscallStepState {
    fn read(task: &Stopped) -> Result<Self, TraceError> {
        let r = task.getregs()?;
        let info = task.getsiginfo()?;
        let offset = std::mem::offset_of!(libc::user, u_debugreg) + 6 * 8;
        let dr6 = nix::sys::ptrace::read_user(task.pid().into(), offset as *mut libc::c_void)
            .map_err(|error| Errno::new(error as i32))? as u64;
        let syscall_opcode = parent_syscall_opcode(r.rip, |address| {
            let address = Addr::from_raw(address).ok_or(Errno::EFAULT)?;
            task.read_value::<_, u64>(address)
        })?;
        Ok(Self {
            words: [
                r.r15, r.r14, r.r13, r.r12, r.rbp, r.rbx, r.r11, r.r10, r.r9, r.r8, r.rax, r.rcx,
                r.rdx, r.rsi, r.rdi, r.orig_rax, r.rip, r.cs, r.eflags, r.rsp, r.ss, r.fs_base,
                r.gs_base, r.ds, r.es, r.fs, r.gs,
            ],
            siginfo: (info.si_signo, info.si_code, info.si_errno),
            fault_address: (info.si_signo == libc::SIGTRAP && info.si_code == libc::TRAP_BRKPT)
                .then(|| unsafe { info.si_addr() as u64 }),
            dr6,
            syscall_opcode,
        })
    }
}

#[cfg(target_arch = "x86_64")]
fn is_parent_syscall_step_completion(
    op: ChildOp,
    child: Pid,
    before: &ParentSyscallStepState,
    after: &ParentSyscallStepState,
) -> bool {
    let event = match op {
        ChildOp::Fork => libc::PTRACE_EVENT_FORK,
        ChildOp::Vfork => libc::PTRACE_EVENT_VFORK,
        ChildOp::Clone => libc::PTRACE_EVENT_CLONE,
    };
    let syscall = before.words[15];
    let creating_call = syscall == libc::SYS_clone as u64
        || syscall == libc::SYS_clone3 as u64
        || (op == ChildOp::Fork && syscall == libc::SYS_fork as u64)
        || (op == ChildOp::Vfork && syscall == libc::SYS_vfork as u64);
    // Linux's syscall-exit SINGLESTEP report is TRAP_BRKPT before any user
    // instruction or pending-signal delivery. It does not update virtual DR6;
    // unchanged stale debug bits are not evidence of a new debug exception.
    child.as_raw() > 0
        && creating_call
        && before.siginfo == (libc::SIGTRAP, (event << 8) | libc::SIGTRAP, 0)
        && after.siginfo == (libc::SIGTRAP, libc::TRAP_BRKPT, 0)
        && after.fault_address == Some(after.words[16])
        && before.words[17] == 0x33 // native x86-64 ABI only
        && before.words[18] & 0x100 == 0
        && after.dr6 == before.dr6
        && before.syscall_opcode == [0x0f, 0x05]
        && after.syscall_opcode == before.syscall_opcode
        && before.words[10] == (-(libc::ENOSYS as i64)) as u64
        // The result is in the parent's PID namespace, unlike the event's
        // tracer-namespace child ID. Do not compare those integer domains.
        && after.words[10] > 0
        && after.words[10] <= i32::MAX as u64
        && before.words.iter().zip(&after.words).enumerate()
            .all(|(index, (before, after))| index == 10 || before == after)
}

#[cfg(target_arch = "x86_64")]
struct ParentSyscallStep {
    task: safeptrace::TaskIdentity,
    op: ChildOp,
    child: Pid,
    before: ParentSyscallStepState,
}

#[cfg(target_arch = "x86_64")]
impl ParentSyscallStep {
    fn capture(task: &Stopped, op: ChildOp, child: Pid) -> Result<Self, TraceError> {
        Ok(Self {
            task: task.terminal_cleanup().task_identity()?,
            op,
            child,
            before: ParentSyscallStepState::read(task)?,
        })
    }

    // Consuming self permits exactly one decision on the wait returned by the
    // existing dispatch. No wait owner, event or native return is manufactured.
    fn completed(self, wait: &Wait) -> Result<bool, TraceError> {
        let Wait::Stopped(task, Event::Signal(Signal::SIGTRAP)) = wait else {
            return Ok(false);
        };
        let current = task.terminal_cleanup().task_identity()?;
        if !self.task.same_generation(&current) {
            return Ok(false);
        }
        Ok(is_parent_syscall_step_completion(
            self.op,
            self.child,
            &self.before,
            &ParentSyscallStepState::read(task)?,
        ))
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod parent_syscall_step_tests {
    use super::*;

    fn pair(op: ChildOp, syscall: i64) -> (ParentSyscallStepState, ParentSyscallStepState) {
        let event = match op {
            ChildOp::Fork => libc::PTRACE_EVENT_FORK,
            ChildOp::Vfork => libc::PTRACE_EVENT_VFORK,
            ChildOp::Clone => libc::PTRACE_EVENT_CLONE,
        };
        let mut before = ParentSyscallStepState {
            words: std::array::from_fn(|i| i as u64 + 0x200),
            siginfo: (libc::SIGTRAP, (event << 8) | libc::SIGTRAP, 0),
            fault_address: None,
            dr6: 0xffff0ff0,
            syscall_opcode: [0x0f, 0x05],
        };
        before.words[10] = (-(libc::ENOSYS as i64)) as u64;
        before.words[15] = syscall as u64;
        before.words[16] = 0x10002;
        before.words[17] = 0x33;
        before.words[18] = 0x206;
        let mut after = before;
        after.words[10] = 42;
        after.siginfo = (libc::SIGTRAP, libc::TRAP_BRKPT, 0);
        after.fault_address = Some(before.words[16]);
        (before, after)
    }

    #[test]
    fn syscall_opcode_at_last_mapped_bytes_needs_no_following_mapping() {
        let mut reads = Vec::new();
        let opcode = parent_syscall_opcode(4096, |address| {
            reads.push(address);
            if address == 4088 {
                Ok(0x050f_0000_0000_0000)
            } else {
                Err(Errno::EFAULT)
            }
        });
        assert_eq!(opcode, Ok([0x0f, 0x05]));
        assert_eq!(reads, [4088, 4088]);
        assert_eq!(
            parent_syscall_opcode(1, |_| panic!("invalid address read")),
            Err(Errno::EFAULT)
        );
        assert_eq!(
            parent_syscall_opcode(4097, |address| if address == 4088 {
                Ok(0x0f00_0000_0000_0000)
            } else {
                Err(Errno::EFAULT)
            }),
            Err(Errno::EFAULT),
        );
        assert_eq!(
            parent_syscall_opcode(4097, |address| match address {
                4088 => Ok(0x0f00_0000_0000_0000),
                4096 => Ok(5),
                _ => Err(Errno::EFAULT),
            }),
            Ok([0x0f, 0x05]),
        );
    }

    #[test]
    fn exact_parent_syscall_exit_and_namespace_relative_result() {
        for (op, syscall) in [
            (ChildOp::Fork, libc::SYS_fork),
            (ChildOp::Vfork, libc::SYS_vfork),
            (ChildOp::Fork, libc::SYS_clone),
            (ChildOp::Clone, libc::SYS_clone),
            (ChildOp::Clone, libc::SYS_clone3),
            (ChildOp::Vfork, libc::SYS_clone3),
        ] {
            let (before, after) = pair(op, syscall);
            assert!(is_parent_syscall_step_completion(
                op,
                Pid::from_raw(104074),
                &before,
                &after
            ));
        }
    }

    #[test]
    fn preserves_every_non_return_word_and_actual_syscall_bytes() {
        let (before, after) = pair(ChildOp::Clone, libc::SYS_clone3);
        let accepts = |a: &ParentSyscallStepState, b: &ParentSyscallStepState| {
            is_parent_syscall_step_completion(ChildOp::Clone, Pid::from_raw(104074), a, b)
        };
        for index in 0..27 {
            if index != 10 {
                let mut changed = after;
                changed.words[index] ^= 1;
                assert!(!accepts(&before, &changed), "word {index}");
            }
        }
        for result in [0, u64::MAX, i32::MAX as u64 + 1] {
            let mut changed = after;
            changed.words[10] = result;
            assert!(!accepts(&before, &changed));
        }
        let mut changed = before;
        changed.words[10] = 0;
        assert!(!accepts(&changed, &after));
        for opcode in [[0xcc, 0x05], [0xcd, 0x80], [0x0f, 0x34]] {
            let mut a = before;
            let mut b = after;
            a.syscall_opcode = opcode;
            b.syscall_opcode = opcode;
            assert!(!accepts(&a, &b));
            assert!(!accepts(&before, &b));
        }
    }

    #[test]
    fn preserves_guest_tf_debug_and_nonowned_traps() {
        let (before, after) = pair(ChildOp::Clone, libc::SYS_clone3);
        let accepts = |a: &ParentSyscallStepState, b: &ParentSyscallStepState| {
            is_parent_syscall_step_completion(ChildOp::Clone, Pid::from_raw(104074), a, b)
        };
        let mut a = before;
        let mut b = after;
        a.words[18] |= 0x100;
        b.words[18] |= 0x100;
        assert!(!accepts(&a, &b));
        for cause in [1, 2, 4, 8, 1 << 13, 1 << 14, 1 << 15] {
            a = before;
            b = after;
            a.dr6 |= cause;
            b.dr6 |= cause;
            assert!(
                accepts(&a, &b),
                "unchanged stale DR6 is not a new debug cause"
            );
            assert!(!accepts(&before, &b));
        }
        for code in [
            libc::TRAP_TRACE,
            libc::SI_KERNEL,
            libc::SI_USER,
            libc::SI_TKILL,
        ] {
            b = after;
            b.siginfo.1 = code;
            assert!(!accepts(&before, &b));
        }
        for address in [None, Some(after.words[16] + 1)] {
            b = after;
            b.fault_address = address;
            assert!(!accepts(&before, &b));
        }
        a = before;
        b = after;
        a.words[17] = 0x23;
        b.words[17] = 0x23;
        assert!(!accepts(&a, &b));
        b = after;
        b.siginfo.0 = libc::SIGUSR1;
        assert!(!accepts(&before, &b));
        a = before;
        a.siginfo.1 = (libc::PTRACE_EVENT_FORK << 8) | libc::SIGTRAP;
        assert!(!accepts(&a, &after));
        assert!(!is_parent_syscall_step_completion(
            ChildOp::Clone,
            Pid::from_raw(0),
            &before,
            &after,
        ));
        let (a, b) = pair(ChildOp::Clone, libc::SYS_getpid);
        assert!(!accepts(&a, &b));
        let (a, b) = pair(ChildOp::Clone, libc::SYS_fork);
        assert!(!accepts(&a, &b));
    }
}

enum HandleSignalResult {
    /// Signal is suppressed with task resumed.
    SignalSuppressed(Wait),
    /// signal needs to be delivered.
    SignalToDeliver(Stopped, Signal),
}

#[cfg(target_arch = "x86_64")]
// Linux can report PTRACE_SINGLESTEP completion from a seccomp syscall skip as
// TRAP_BRKPT without advancing RIP. Distinguish that kernel transition from an
// external or guest breakpoint using the controller's exact pre-step state.
fn is_expected_syscall_skip_breakpoint(
    si_code: i32,
    pre_rip: u64,
    post_rip: u64,
    syscall_opcode: [u8; cp::SYSCALL_INSTR_SIZE],
    post_opcode: u8,
    forced_external_for_test: bool,
) -> bool {
    !forced_external_for_test
        && si_code == libc::TRAP_BRKPT
        && post_rip == pre_rip
        && syscall_opcode == [0x0f, 0x05]
        && post_opcode != 0xcc
}

fn is_expected_syscall_skip_trap(
    task: &Stopped,
    pre_rip: u64,
    forced_external_for_test: bool,
) -> Result<bool, TraceError> {
    if forced_external_for_test {
        return Ok(false);
    }
    let siginfo = task.getsiginfo()?;
    if siginfo.si_code == libc::TRAP_TRACE {
        return Ok(true);
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        return Ok(false);
    }
    #[cfg(target_arch = "x86_64")]
    if siginfo.si_code != libc::TRAP_BRKPT {
        return Ok(false);
    }
    #[cfg(target_arch = "x86_64")]
    {
        let post_rip = task.getregs()?.ip();
        let syscall_site = pre_rip
            .checked_sub(cp::SYSCALL_INSTR_SIZE as u64)
            .ok_or(Errno::EOVERFLOW)? as usize;
        let mut syscall_opcode = [0; cp::SYSCALL_INSTR_SIZE];
        task.read_exact(syscall_site, &mut syscall_opcode)?;
        let mut post_opcode = [0];
        task.read_exact(post_rip as usize, &mut post_opcode)?;
        Ok(is_expected_syscall_skip_breakpoint(
            siginfo.si_code,
            pre_rip,
            post_rip,
            syscall_opcode,
            post_opcode[0],
            forced_external_for_test,
        ))
    }
}

fn is_expected_breakpoint_trap(
    task: &Stopped,
    breakpoint_rip: u64,
    forced_external_for_test: bool,
) -> Result<bool, TraceError> {
    if forced_external_for_test {
        return Ok(false);
    }
    let siginfo = task.getsiginfo()?;
    let observed_rip = task.getregs()?.ip();
    let after_breakpoint = breakpoint_rip.checked_add(1);
    Ok((siginfo.si_code == libc::TRAP_BRKPT
        && (observed_rip == breakpoint_rip || Some(observed_rip) == after_breakpoint))
        || (siginfo.si_code == libc::SI_KERNEL && Some(observed_rip) == after_breakpoint))
}

fn is_expected_private_syscall_trap(
    task: &Stopped,
    expected_rip: u64,
    forced_external_for_test: bool,
) -> Result<bool, TraceError> {
    if forced_external_for_test {
        return Ok(false);
    }
    if task.getregs()?.ip() != expected_rip {
        return Ok(false);
    }
    let siginfo = task.getsiginfo()?;
    if !matches!(siginfo.si_code, libc::TRAP_TRACE | libc::TRAP_BRKPT) {
        return Ok(false);
    }

    // Some x86 kernels report PTRACE_SINGLESTEP completion after `syscall` as
    // TRAP_BRKPT rather than TRAP_TRACE. In either case, the private page is
    // RWX and therefore guest-mutable, so accept the stop only while the exact
    // controller-installed `syscall; ud2` stub remains intact.
    #[cfg(target_arch = "x86_64")]
    let expected_stub = [0x0f, 0x05, 0x0f, 0x0b];
    #[cfg(target_arch = "aarch64")]
    let expected_stub = [
        0x01, 0x00, 0x00, 0xd4, // svc 0
        0xad, 0xde, 0x00, 0x00, // udf 0xdead
    ];
    let mut observed_stub = [0; cp::SYSCALL_INSTR_SIZE * 2];
    task.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut observed_stub)?;
    Ok(observed_stub == expected_stub)
}

enum NestedTrapExpectation {
    None,
    SyscallSkip { pre_rip: u64 },
    Breakpoint(u64),
    PrivateSyscall(u64),
}
#[derive(Clone)]
pub(crate) struct InjectedSyscallTrap {
    pub(crate) marker: u64,
    pub(crate) rip: u64,
    pub(crate) provenance: Option<InjectedSyscallProvenance>,
}

#[derive(Clone)]
pub(crate) struct InjectedSyscallProvenance {
    pub(crate) image: PathBuf,
    pub(crate) image_inode: u64,
    pub(crate) image_entry_address: u64,
    pub(crate) patched_site_addresses: Arc<[u64]>,
}

#[derive(Debug)]
struct GuestMap {
    start: u64,
    end: u64,
    offset: u64,
    device_major: u64,
    device_minor: u64,
    readable: bool,
    writable: bool,
    executable: bool,
    shared: bool,
    inode: u64,
    path: Option<PathBuf>,
}

impl GuestMap {
    fn contains(&self, address: u64) -> bool {
        self.start <= address && address < self.end
    }

    fn contains_range(&self, range: GuestRange) -> bool {
        self.start <= range.start && range.end <= self.end
    }
}

fn guest_maps(pid: Pid) -> Option<Vec<GuestMap>> {
    let maps = std::fs::read(format!("/proc/{pid}/maps")).ok()?;
    Some(
        maps.split(|byte| *byte == b'\n')
            .filter_map(parse_guest_map)
            .collect(),
    )
}

fn next_proc_maps_field<'a>(line: &'a [u8], cursor: &mut usize) -> Option<&'a [u8]> {
    while line.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
        *cursor += 1;
    }
    let start = *cursor;
    while line
        .get(*cursor)
        .is_some_and(|byte| !byte.is_ascii_whitespace())
    {
        *cursor += 1;
    }
    (start < *cursor).then(|| &line[start..*cursor])
}

fn parse_guest_map(line: &[u8]) -> Option<GuestMap> {
    let mut cursor = 0;
    let range = std::str::from_utf8(next_proc_maps_field(line, &mut cursor)?).ok()?;
    let permissions = next_proc_maps_field(line, &mut cursor)?;
    let offset = std::str::from_utf8(next_proc_maps_field(line, &mut cursor)?).ok()?;
    let device = std::str::from_utf8(next_proc_maps_field(line, &mut cursor)?).ok()?;
    let inode = std::str::from_utf8(next_proc_maps_field(line, &mut cursor)?).ok()?;

    let (start, end) = range.split_once('-')?;
    let start = u64::from_str_radix(start, 16).ok()?;
    let end = u64::from_str_radix(end, 16).ok()?;
    let offset = u64::from_str_radix(offset, 16).ok()?;
    let (device_major, device_minor) = device.split_once(':')?;
    let device_major = u64::from_str_radix(device_major, 16).ok()?;
    let device_minor = u64::from_str_radix(device_minor, 16).ok()?;
    let inode = inode.parse::<u64>().ok()?;

    while line.get(cursor) == Some(&b' ') {
        cursor += 1;
    }
    let path = (cursor < line.len()).then(|| decode_proc_maps_path(&line[cursor..]));
    Some(GuestMap {
        start,
        end,
        offset,
        device_major,
        device_minor,
        readable: permissions.first() == Some(&b'r'),
        writable: permissions.get(1) == Some(&b'w'),
        executable: permissions.get(2) == Some(&b'x'),
        shared: permissions.get(3) == Some(&b's'),
        inode,
        path,
    })
}

fn decode_proc_maps_path(bytes: &[u8]) -> PathBuf {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..index + 4]
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'7'))
        {
            let value = u16::from(bytes[index + 1] - b'0') * 64
                + u16::from(bytes[index + 2] - b'0') * 8
                + u16::from(bytes[index + 3] - b'0');
            if let Ok(value) = u8::try_from(value) {
                decoded.push(value);
                index += 4;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    PathBuf::from(OsString::from_vec(decoded))
}

fn guest_auxv_entry(pid: Pid, key: u64) -> Option<u64> {
    let bytes = std::fs::read(format!("/proc/{pid}/auxv")).ok()?;
    bytes.as_chunks::<16>().0.iter().find_map(|entry| {
        let entry_key = u64::from_ne_bytes(entry[..8].try_into().ok()?);
        let value = u64::from_ne_bytes(entry[8..].try_into().ok()?);
        (entry_key == key).then_some(value)
    })
}

impl InjectedSyscallTrap {
    // TODO-HUMAN-REVIEW(PR-271): Review rewritten-image load-bias and patched-site
    // collision filtering before Tool dispatch.
    fn validates_site_provenance(
        &self,
        pid: Pid,
        trap_rip: u64,
        frame: &InjectedSyscallFrame,
    ) -> bool {
        let Some(provenance) = &self.provenance else {
            return trap_rip == self.rip;
        };
        let Some(maps) = guest_maps(pid) else {
            return false;
        };
        let matches_image = |mapping: &&GuestMap| {
            mapping.inode == provenance.image_inode
                && mapping.path.as_ref() == Some(&provenance.image)
        };
        let Some(load_bias) = guest_auxv_entry(pid, libc::AT_ENTRY)
            .and_then(|entry| entry.checked_sub(provenance.image_entry_address))
        else {
            return false;
        };
        self.rip.checked_add(load_bias) == Some(trap_rip)
            && maps
                .iter()
                .filter(matches_image)
                .any(|mapping| mapping.executable && mapping.contains(trap_rip))
            && maps
                .iter()
                .filter(matches_image)
                .any(|mapping| mapping.executable && mapping.contains(frame.instruction_pointer()))
            && frame
                .instruction_pointer()
                .checked_sub(load_bias)
                .is_some_and(|address| {
                    provenance
                        .patched_site_addresses
                        .binary_search(&address)
                        .is_ok()
                })
    }
}

#[derive(Clone)]
pub(crate) struct LiteinstRuntimeConfig {
    pub(crate) preload: PathBuf,
    pub(crate) begin_marker: u64,
    pub(crate) ready_marker: u64,
    pub(crate) helper_return_marker: u64,
    pub(crate) syscall_marker: u64,
    pub(crate) newborn_tracees: Arc<StdMutex<HashMap<Pid, NewbornTracee>>>,
    pub(crate) held_root_stop: Arc<StdMutex<Option<HeldRootStop>>>,
    /// Records a fail-closed LiteInst refusal raised by any task.
    ///
    /// A non-root task's error cannot reach the root's cleanup guard, so
    /// without this the session could finish "successfully" after a child was
    /// released untraced. The root consults it before reporting success.
    pub(crate) session_failure: Arc<StdMutex<Option<String>>>,
    /// Wakes the root task as soon as a non-root refusal records the shared
    /// failure. The root may otherwise remain blocked in a guest wait for the
    /// refused child and never return control to the session cleanup guard.
    pub(crate) session_failure_changed: Arc<Notify>,
    /// Set once the guest has created a second task.
    ///
    /// Hook installation is single-task-only (see `maybe_install_liteinst_site`).
    pub(crate) multi_task: Arc<AtomicBool>,
    /// TID of the session's root tracee, published once the guest is spawned.
    ///
    /// The root-stop lease and its cleanup guard are owned by exactly this
    /// TID. A forked child is its own thread-group leader, so the
    /// `tid == pid` shape cannot distinguish it from the root.
    pub(crate) root_tid: Arc<StdOnceLock<Pid>>,
    pub(crate) instrumentation_stats: Option<Arc<StdMutex<LiteinstInstrumentationStats>>>,
    #[cfg(test)]
    pub(crate) fail_preinit: bool,
    /// Synthesises a fail-closed error at the new-task boundary.
    ///
    /// Production no longer refuses task creation, so the cleanup guard's
    /// whole-group reaping needs an explicit trigger that still produces a
    /// multi-task tree at the moment of failure.
    #[cfg(test)]
    pub(crate) fail_new_task: bool,
    #[cfg(test)]
    pub(crate) pause_new_task: Option<mpsc::UnboundedSender<Pid>>,
    #[cfg(test)]
    pub(crate) pause_after_new_task: bool,
    #[cfg(test)]
    pub(crate) pause_before_new_task: Option<mpsc::UnboundedSender<Pid>>,
    #[cfg(test)]
    pub(crate) fail_discovery_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) fail_after_scan_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_task_scan_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) pause_root_stop: Option<(RootStopPause, mpsc::UnboundedSender<Pid>)>,
    #[cfg(test)]
    pub(crate) pause_preinit_step: Option<(usize, mpsc::UnboundedSender<Pid>)>,
    #[cfg(test)]
    pub(crate) pause_precise_timer_step: Option<mpsc::UnboundedSender<Pid>>,
    #[cfg(test)]
    pub(crate) activate_without_handshake: bool,
    #[cfg(test)]
    pub(crate) queue_pending_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_skip_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_context_none_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_context_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_preinit_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_post_exec_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_private_stub_mutation_once: Option<Arc<AtomicBool>>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum RootStopPause {
    Seccomp,
    Signal(Signal),
}

#[derive(Clone)]
struct LiteinstRootStopArmer {
    root_tid: Pid,
    held_root_stop: Arc<StdMutex<Option<HeldRootStop>>>,
}

impl LiteinstRootStopArmer {
    fn arm(&self, task: &Stopped, event: &Event) -> Result<(), TraceError> {
        if task.pid() != self.root_tid {
            return Ok(());
        }
        HeldRootStop::arm_empty(&self.held_root_stop, task, event)
    }

    fn ensure(&self, task: &Stopped, event: &Event) -> Result<(), TraceError> {
        if task.pid() != self.root_tid {
            return Ok(());
        }
        HeldRootStop::ensure_current(&self.held_root_stop, task, event)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct LiteinstHandshakeFrame {
    version: u64,
    begin_rip: u64,
    ready_rip: u64,
    install_helper: u64,
    helper_stack_top: u64,
    helper_return: u64,
    helper_return_rip: u64,
    syscall_trap_rip: u64,
    syscall_trap_return_rip: u64,
    install_result: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct LiteinstInstallResult {
    version: u64,
    site_start: u64,
    site_len: u64,
    relocated_tail: u64,
    trampoline_start: u64,
    trampoline_len: u64,
    arena_writable_start: u64,
    arena_writable_len: u64,
    arena_executable_start: u64,
    arena_executable_len: u64,
    instruction_len: u64,
    straddle_prefix: u64,
    complete: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GuestRange {
    start: u64,
    end: u64,
}

impl GuestRange {
    fn new(start: u64, len: u64) -> Option<Self> {
        let end = start.checked_add(len)?;
        (start < end).then_some(Self { start, end })
    }

    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    fn contains(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

fn kernel_page_range(start: u64, len: u64, page_size: u64) -> Result<Option<GuestRange>, ()> {
    if page_size == 0 || !page_size.is_power_of_two() {
        return Err(());
    }
    if len == 0 {
        return Ok(None);
    }

    let end = start.checked_add(len).ok_or(())?;
    let page_mask = page_size - 1;
    let start = start & !page_mask;
    let end = end.checked_add(page_mask).ok_or(())? & !page_mask;
    Ok(Some(GuestRange { start, end }))
}

fn host_page_size() -> Result<u64, Errno> {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size).map_err(|_| Errno::EIO)?;
    page_size
        .is_power_of_two()
        .then_some(page_size)
        .ok_or(Errno::EIO)
}

fn is_liteinst_mapping_syscall(nr: Sysno) -> bool {
    // TODO-HUMAN-REVIEW(PR-270): Review pkey_mprotect mapping-lifecycle classification.
    matches!(
        nr,
        // AUTONOMOUS-BOT-IMPLEMENTED
        Sysno::mmap | Sysno::munmap | Sysno::mremap | Sysno::mprotect | Sysno::pkey_mprotect
    )
}

/// Syscalls whose return lands in two tasks at once.
///
/// The kernel starts the new task at the instruction following the `syscall`,
/// so the site must still decode as the original instruction stream there.
fn is_task_creating_syscall(nr: Sysno) -> bool {
    matches!(
        nr,
        Sysno::clone | Sysno::clone3 | Sysno::fork | Sysno::vfork
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveHookFootprint {
    site: GuestRange,
    trampoline: GuestRange,
    arena_writable: GuestRange,
    arena_executable: GuestRange,
}

impl ActiveHookFootprint {
    fn protected_ranges(&self) -> [(GuestRange, i32); 4] {
        [
            (self.site, libc::PROT_READ | libc::PROT_EXEC),
            (self.trampoline, libc::PROT_READ | libc::PROT_EXEC),
            (self.arena_writable, libc::PROT_READ | libc::PROT_WRITE),
            (self.arena_executable, libc::PROT_READ | libc::PROT_EXEC),
        ]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstRuntimePhase {
    PreExec,
    Waiting,
    Bootstrap,
    Ready,
}

#[derive(Clone, Debug)]
struct LiteinstRuntimeState {
    phase: LiteinstRuntimePhase,
    frame: Option<LiteinstHandshakeFrame>,
    generation: u64,
    ready_generation: Option<u64>,
    attempted_sites: HashSet<u64>,
    fallback_sites: HashMap<u64, LiteinstPatchOutcome>,
    active_hooks: HashMap<u64, ActiveHookFootprint>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiteinstEntryGuard {
    address: u64,
    saved_instruction: u64,
}

impl Default for LiteinstRuntimeState {
    fn default() -> Self {
        Self {
            phase: LiteinstRuntimePhase::PreExec,
            frame: None,
            generation: 0,
            ready_generation: None,
            attempted_sites: HashSet::new(),
            fallback_sites: HashMap::new(),
            active_hooks: HashMap::new(),
        }
    }
}

impl LiteinstRuntimeState {
    fn after_exec(&self) -> Result<Self, Errno> {
        Ok(Self {
            phase: LiteinstRuntimePhase::Waiting,
            generation: self.generation.checked_add(1).ok_or(Errno::EOVERFLOW)?,
            ..Self::default()
        })
    }

    fn mapping_mutates_active_hook(&self, nr: Sysno, args: SyscallArgs, page_size: u64) -> bool {
        let operation_range = match nr {
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::mmap if args.arg3 as i32 & libc::MAP_FIXED != 0 => {
                kernel_page_range(args.arg0 as u64, args.arg1 as u64, page_size)
            }
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::munmap | Sysno::mprotect | Sysno::pkey_mprotect | Sysno::mremap => {
                kernel_page_range(args.arg0 as u64, args.arg1 as u64, page_size)
            }
            _ => return false,
        };
        let requested_protection = match nr {
            Sysno::mprotect => Some(args.arg2 as i32),
            Sysno::pkey_mprotect if args.arg3 == 0 => Some(args.arg2 as i32),
            _ => None,
        };
        let source_mutates_active_hook = match operation_range {
            Ok(Some(operation_range)) => self.active_hooks.values().any(|hook| {
                hook.protected_ranges()
                    .into_iter()
                    .any(|(range, protection)| {
                        range.overlaps(operation_range) && requested_protection != Some(protection)
                    })
            }),
            Ok(None) => false,
            Err(()) => !self.active_hooks.is_empty(),
        };
        if source_mutates_active_hook {
            return true;
        }
        if nr == Sysno::mremap && args.arg3 as i32 & libc::MREMAP_FIXED != 0 {
            let destination = match kernel_page_range(args.arg4 as u64, args.arg2 as u64, page_size)
            {
                Ok(Some(range)) => range,
                Ok(None) => return false,
                Err(()) => return !self.active_hooks.is_empty(),
            };
            return self.active_hooks.values().any(|hook| {
                hook.protected_ranges()
                    .into_iter()
                    .any(|(range, _)| range.overlaps(destination))
            });
        }
        false
    }

    fn invalidate_attempted_pages(&mut self, start: u64, len: u64, page_size: u64) {
        let range = match kernel_page_range(start, len, page_size) {
            Ok(Some(range)) => range,
            Ok(None) => return,
            Err(()) => {
                self.attempted_sites.clear();
                self.fallback_sites.clear();
                return;
            }
        };
        if range.start >= range.end {
            self.attempted_sites.clear();
            self.fallback_sites.clear();
            return;
        }
        self.attempted_sites
            .retain(|address| !(*address >= range.start && *address < range.end));
        self.fallback_sites
            .retain(|address, _| !(*address >= range.start && *address < range.end));
    }
}

fn callback_owner_decision(
    held: crate::PtraceCallbackStop,
    sample: &safeptrace::StopObservationSample,
) -> Result<bool, crate::PtraceCallbackRefusal> {
    callback_observation_decision(
        held,
        sample.refusal(),
        sample.siginfo(),
        sample.flags(),
        sample.pidfd_live(),
    )
}

// Keep the observed fields separate from physical ownership. The private seam
// also lets refusal tests supply exact failures without inventing kernel events.
fn callback_observation_decision(
    held: crate::PtraceCallbackStop,
    refusal: Option<safeptrace::StopObservationError>,
    siginfo: Option<Result<safeptrace::StopSiginfo, Errno>>,
    flags: Option<&Result<u32, safeptrace::ProcStatError>>,
    pidfd_live: Option<Result<bool, Errno>>,
) -> Result<bool, crate::PtraceCallbackRefusal> {
    use crate::PtraceCallbackRefusal as Refusal;
    use crate::PtraceCallbackStop as Stop;
    if let Some(error) = refusal {
        return Err(Refusal::Binding(error));
    }
    let info = siginfo.ok_or(Refusal::Inconsistent("missing siginfo query"))?;
    if let Err(error) = info
        && error != Errno::ESRCH
    {
        return Err(Refusal::Query(error));
    }
    let live = pidfd_live
        .ok_or(Refusal::Inconsistent("missing final pidfd query"))?
        .map_err(Refusal::Pidfd)?;
    if let Some(Err(error)) = flags {
        if !live
            && matches!(
                error,
                safeptrace::ProcStatError::Io(Errno::ESRCH | Errno::ENOENT)
            )
        {
            return Ok(true);
        }
        return Err(Refusal::Proc(error.clone()));
    }
    if !live || info == Err(Errno::ESRCH) {
        return Ok(true);
    }
    let info = info.map_err(Refusal::Query)?;
    if held == Stop::Stop {
        return Err(Refusal::Inconsistent("unexpected group-stop class"));
    }
    if info.has_exit_signature() {
        if held != Stop::Signal(libc::SIGTRAP) {
            return Ok(true);
        }
        let flags = flags
            .ok_or(Refusal::Inconsistent("missing ambiguous EXIT flags"))?
            .as_ref()
            .map_err(|error| Refusal::Proc(error.clone()))?;
        // PF_SIGNALED precedes EXIT for killed/zapped ordinary user tasks.
        // It permits waiting on the original owner, never terminal inference.
        return Ok(flags & 0x400 != 0);
    }
    let matches = match held {
        Stop::Signal(signal) => info.signo == signal,
        Stop::Event(event) => {
            info.signo == libc::SIGTRAP && info.code == (event << 8) | libc::SIGTRAP
        }
        Stop::Syscall => info.signo == libc::SIGTRAP && info.code == libc::SIGTRAP | 0x80,
        Stop::Stop => false,
    };
    if matches {
        Ok(false)
    } else {
        Err(Refusal::Inconsistent(
            "siginfo disagrees with held stop class",
        ))
    }
}

enum LiteinstTrap {
    HandshakeBegin,
    HandshakeReady,
    Syscall(usize),
    Invalid,
}

#[path = "source_cohort.rs"]
pub(crate) mod source_cohort;
#[path = "source_epoch.rs"]
pub(crate) mod source_epoch;
#[path = "source_jobs.rs"]
pub(crate) mod source_jobs;
#[cfg(target_arch = "x86_64")]
#[path = "source_observation.rs"]
pub(crate) mod source_observation;

/// The first ordinary-ptrace fatal error cancels every followed task. Keep the
/// actual error until the entire tree has completed its real terminal waits.
#[derive(Default)]
pub(crate) struct FatalSession {
    pub(crate) source_jobs: source_jobs::SourceJobs,
    source_epoch: Arc<source_epoch::SourceEpoch>,
    source_cohort: Arc<source_cohort::CohortHistory>,
    ptracer_thread: Option<std::thread::ThreadId>,
    callback_diagnostics: StdMutex<Vec<crate::PtraceCallbackDiagnostic>>,
    backend_signalling: AtomicBool,
    failure: StdMutex<Option<PtraceRunFailure>>,
    published: AtomicBool,
    reporter: Option<Box<dyn Fn(BackendFailure) -> bool + Send + Sync>>,
    closed: AtomicBool,
    root: Option<Pid>,
    changed: Notify,
    tree: StdMutex<FatalTree>,
    joins: StdMutex<Vec<JoinHandle<()>>>,
    retry_epoch: AtomicUsize,
    retry_changed: Notify,
    groups: StdMutex<Vec<Arc<FatalGroup>>>,
    cleanup_deadline: StdMutex<Option<std::time::Instant>>,
    #[cfg(test)]
    pub(crate) observed_child_ops: StdMutex<Vec<(Pid, ChildOp, Pid)>>,
}

#[derive(Debug, thiserror::Error)]
#[error("original global Tool owner was unavailable for synchronous failure publication")]
struct GlobalFailurePublicationLost;

struct FatalGroup {
    terminal: safeptrace::TerminalCleanup,
    identity: crate::tracer::TraceeIdentity,
}

/// Only subscribe_group constructs this after successful same-generation capture.
/// Weak expiry therefore means that this session completed that owner, rather
/// than that an unknown or failed capture may be treated as retired.
pub(crate) struct OrdinaryGroupSubscription {
    session: std::sync::Weak<FatalSession>,
    group: std::sync::Weak<FatalGroup>,
}

#[derive(Default)]
struct FatalTree {
    tasks: Vec<Arc<FatalTaskStop>>,
    newborns: Vec<FatalNewborn>,
    // Retained across initialization: a handed vfork child can still block its parent.
    vfork_children: Vec<Arc<safeptrace::TerminalCleanup>>,
    unconfirmed_newborns: Vec<(Pid, Arc<safeptrace::TerminalCleanup>)>,
    killing: bool,
    cleanup_refusal: Option<String>,
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct FatalForkPause {
    pub(crate) ready: Notify,
    pub(crate) child: StdMutex<Option<Running>>,
    pub(crate) waiting_word: std::sync::atomic::AtomicUsize,
    pub(crate) generation: StdMutex<Option<(u64, u64)>>,
    pub(crate) terminal_status: StdMutex<Option<(Pid, ExitStatus)>>,
    pub(crate) live_stop_opponent: AtomicBool,
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct FatalSetupControl {
    pub(crate) child: StdMutex<Option<(Pid, Arc<safeptrace::TerminalCleanup>)>>,
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct FatalFreezeControl {
    pub(crate) calls: std::sync::atomic::AtomicUsize,
    pub(crate) session: StdMutex<Option<Arc<FatalSession>>>,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ExecTimerTransfer {
    pub displaced: Option<crate::timer::ExecTimerIdentity>,
    pub before: Option<crate::timer::ExecTimerIdentity>,
    pub after: Option<crate::timer::ExecTimerIdentity>,
    pub displaced_fds_closed: bool,
}
#[cfg(test)]
thread_local! {
    pub(crate) static EXEC_TIMER_TRANSFERS: std::cell::RefCell<Option<Arc<StdMutex<Vec<ExecTimerTransfer>>>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static FATAL_FORK_PAUSE: std::cell::RefCell<Option<Arc<FatalForkPause>>> = const { std::cell::RefCell::new(None) };
    pub(crate) static FATAL_SETUP_CONTROL: std::cell::RefCell<Option<Arc<FatalSetupControl>>> = const { std::cell::RefCell::new(None) };
    pub(crate) static FATAL_FREEZE_CONTROL: std::cell::RefCell<Option<Arc<FatalFreezeControl>>> = const { std::cell::RefCell::new(None) };
}

impl FatalSession {
    fn capture(&self, parent: Pid, op: ChildOp, child: &Running) {
        self.capture_newborn(parent, op, child);
        self.capture_group(parent, op, child);
    }

    fn capture_newborn(&self, parent: Pid, op: ChildOp, child: &Running) {
        let mut tree = self.tree.lock().unwrap();
        let terminal = child.terminal_cleanup();
        if tree
            .newborns
            .iter()
            .any(|entry| entry.terminal.same_generation(&terminal))
            || tree
                .tasks
                .iter()
                .any(|entry| entry.terminal.same_generation(&terminal))
        {
            return;
        }
        if op == ChildOp::Vfork {
            tree.vfork_children.push(Arc::new(child.terminal_cleanup()));
        }
        #[cfg(test)]
        self.observed_child_ops
            .lock()
            .unwrap()
            .push((parent, op, child.pid()));
        // Store the original receiver before any fallible group capture.
        tree.newborns.push(FatalNewborn::new(parent, child));
    }

    fn capture_group(&self, parent: Pid, op: ChildOp, child: &Running) {
        let terminal = child.terminal_cleanup();
        if self
            .groups
            .lock()
            .unwrap()
            .iter()
            .any(|entry| entry.terminal.same_generation(&terminal))
        {
            return;
        }
        match crate::tracer::TraceeIdentity::capture_event_child(child.pid(), parent, op) {
            Ok(identity) => self
                .groups
                .lock()
                .unwrap()
                .push(Arc::new(FatalGroup { terminal, identity })),
            Err(error) => self.fail_at(
                BackendFailure {
                    pid: parent,
                    tid: child.pid(),
                    phase: "ptrace child group capture",
                },
                error.into(),
            ),
        }
    }

    pub(crate) fn capture_root(&self, stopped: &Stopped) {
        let root = stopped.pid();
        match crate::tracer::TraceeIdentity::open_root(root) {
            Ok(identity) => self.groups.lock().unwrap().push(Arc::new(FatalGroup {
                terminal: stopped.terminal_cleanup(),
                identity,
            })),
            Err(error) => self.fail_at(
                BackendFailure {
                    pid: root,
                    tid: root,
                    phase: "ptrace root group capture",
                },
                error.into(),
            ),
        }
    }

    pub(crate) fn signal_groups(&self) -> Vec<Errno> {
        if let Err(error) = self.source_cohort.before_group_signal() {
            return vec![error];
        }
        self.groups
            .lock()
            .unwrap()
            .iter()
            .filter_map(|group| {
                let _signal = match group.terminal.source_signal_guard() {
                    Ok(guard) => guard,
                    Err(error) => return Some(error),
                };
                group
                    .identity
                    .signal_owned_group()
                    .err()
                    .filter(|error| *error != Errno::ESRCH)
            })
            .collect()
    }

    fn handed(&self, child: Pid) {
        let mut tree = self.tree.lock().unwrap();
        tree.newborns
            .iter_mut()
            .find(|entry| entry.tid == child)
            .expect("child handoff requires retained kernel edge")
            .handed = true;
    }

    fn newborn_exited(&self, child: Pid) {
        self.tree
            .lock()
            .unwrap()
            .newborns
            .retain(|entry| entry.tid != child);
        self.changed.notify_waiters();
    }

    fn newborn_handoff_failed(&self, child: Pid) {
        if let Some(entry) = self
            .tree
            .lock()
            .unwrap()
            .newborns
            .iter_mut()
            .find(|entry| entry.tid == child)
        {
            // The spawned Tool body failed before register() transferred its
            // exact EXIT receiver. Return that retained newborn to the
            // session's unhanded cleanup owner; never leave a handed edge with
            // no task capable of registering it.
            entry.handed = false;
        }
        self.changed.notify_waiters();
    }

    fn refuse_cleanup(&self, error: reverie::Error) -> reverie::Error {
        let message = error.to_string();
        self.fail(error);
        self.tree
            .lock()
            .unwrap()
            .cleanup_refusal
            .get_or_insert(message.clone());
        // Retain every unconfirmed task/newborn authority. Refusal releases
        // other barrier waiters, but never marks those tracees as completed.
        self.changed.notify_waiters();
        anyhow::anyhow!("fatal ptrace cleanup refused: {message}").into()
    }

    #[cfg(test)]
    pub(crate) fn unconfirmed_task_count(&self) -> usize {
        self.tree.lock().unwrap().tasks.len()
    }

    #[cfg(test)]
    pub(crate) fn retained_group_counts_for_test(&self) -> (usize, usize, usize) {
        let tree = self.tree.lock().unwrap();
        (
            self.groups.lock().unwrap().len(),
            tree.vfork_children.len(),
            tree.unconfirmed_newborns.len(),
        )
    }

    #[cfg(test)]
    pub(crate) fn cleanup_was_refused(&self) -> bool {
        self.tree.lock().unwrap().cleanup_refusal.is_some()
    }

    fn register(&self, task: Arc<FatalTaskStop>) -> Option<FatalNewborn> {
        #[cfg(test)]
        crate::tracer::record_fatal_task_for_test(&task);
        let mut tree = self.tree.lock().unwrap();
        let newborn = tree
            .newborns
            .iter()
            .position(|entry| {
                entry.tid == task.tid && entry.terminal.same_generation(&task.terminal)
            })
            .map(|index| tree.newborns.remove(index));
        assert!(
            !tree
                .tasks
                .iter()
                .any(|entry| entry.terminal.same_generation(&task.terminal))
        );
        tree.tasks.push(task);
        self.changed.notify_waiters();
        newborn
    }

    fn finished(&self, stop: &FatalTaskStop) {
        let mut tree = self.tree.lock().unwrap();
        tree.tasks
            .retain(|task| !task.terminal.same_generation(&stop.terminal));
        tree.vfork_children
            .retain(|child| !child.same_generation(&stop.terminal));
        drop(tree);
        // Initialized leaders reach this only after child-thread joins and
        // consuming hooks. In particular, retain the regular group pidfd while
        // an exited leader still owns live daemon siblings.
        self.groups
            .lock()
            .unwrap()
            .retain(|group| !group.terminal.same_generation(&stop.terminal));
        #[cfg(test)]
        crate::tracer::record_capacity_finished_for_test(stop);
        self.changed.notify_waiters();
    }

    pub(crate) fn finished_unhanded(&self, terminal: &safeptrace::TerminalCleanup) {
        // Called by the sole newborn owner only after its actual final status
        // and worker retirement. A refusal/Pending keeps all these authorities.
        let mut tree = self.tree.lock().unwrap();
        tree.unconfirmed_newborns
            .retain(|(_, entry)| !entry.same_generation(terminal));
        tree.vfork_children
            .retain(|entry| !entry.same_generation(terminal));
        drop(tree);
        self.groups
            .lock()
            .unwrap()
            .retain(|group| !group.terminal.same_generation(terminal));
        self.changed.notify_waiters();
    }

    pub(crate) fn subscribe_group(
        self: &Arc<Self>,
        terminal: &safeptrace::TerminalCleanup,
    ) -> Result<OrdinaryGroupSubscription, Errno> {
        let groups = self.groups.lock().unwrap();
        let group = groups
            .iter()
            .find(|group| group.terminal.same_generation(terminal))
            .ok_or(Errno::ESTALE)?;
        Ok(OrdinaryGroupSubscription {
            session: Arc::downgrade(self),
            group: Arc::downgrade(group),
        })
    }

    pub(crate) fn signal_subscribed_group(
        self: &Arc<Self>,
        subscription: &OrdinaryGroupSubscription,
    ) -> Result<(), Errno> {
        if !std::sync::Weak::ptr_eq(&subscription.session, &Arc::downgrade(self)) {
            return Err(Errno::ESTALE);
        }
        let Some(group) = subscription.group.upgrade() else {
            // This exact successful capture was released at its owner-complete
            // boundary. Do not capture or signal a possibly reused numeric PID.
            return Ok(());
        };
        let groups = self.groups.lock().unwrap();
        if !groups.iter().any(|entry| Arc::ptr_eq(entry, &group)) {
            return Err(Errno::ESTALE);
        }
        self.backend_signalling.store(true, Ordering::Release);
        self.source_cohort.before_group_signal()?;
        group.identity.signal_owned_process_group()
    }

    pub(crate) fn ordinary_receipt(&self) -> crate::tracer::OrdinaryReceipt {
        crate::tracer::OrdinaryReceipt {
            failure_published: self.is_failed(),
            backend_signalling: self.backend_signalling.load(Ordering::Acquire),
        }
    }

    #[cfg(test)]
    pub(crate) fn owned_daemon_signal(
        &self,
        terminal: &safeptrace::TerminalCleanup,
    ) -> Result<(), Errno> {
        self.backend_signalling.store(true, Ordering::Release);
        self.source_cohort.before_group_signal()?;
        // The orphan carries its original generation across late exit hooks.
        // A numeric PID match could select a later captured process. A thread
        // pidfd can also accept SIGKILL without reaching an exited leader's
        // live siblings; use this generation's captured regular group pidfd.
        self.groups
            .lock()
            .unwrap()
            .iter()
            .find(|group| group.terminal.same_generation(terminal))
            .ok_or(Errno::ESTALE)?
            .identity
            .signal_owned_process_group()
    }

    pub(crate) async fn retry_after(&self, error: reverie::Error) {
        let epoch = self.retry_epoch.load(Ordering::Acquire);
        let _ = self.refuse_cleanup(error);
        self.wait_retry(epoch).await;
    }

    pub(crate) async fn freeze_and_kill(&self, task: &FatalTaskStop) {
        // Physical source custody survives callback cancellation. The original
        // CompletionWork keeps polling the actual OS join while this waits.
        self.source_jobs.wait_followed_retirement().await;
        self.backend_signalling.store(true, Ordering::Release);
        // A vfork parent can be kernel-blocked behind a captured child. Signal
        // these exact child generations before waiting for the parent stop.
        {
            let error = {
                let tree = self.tree.lock().unwrap();
                tree.vfork_children
                    .iter()
                    .find_map(|child| {
                        child
                            .request_sigkill()
                            .err()
                            .filter(|error| *error != Errno::ESRCH)
                    })
                    .or_else(|| {
                        tree.newborns.iter().find_map(|child| {
                            child.signal().err().filter(|error| *error != Errno::ESRCH)
                        })
                    })
            };
            if let Some(error) = error {
                failed_task_termination_is_fatal(
                    self.root.expect("fatal session has a root owner"),
                    error,
                );
            }
        }
        loop {
            match task
                .freeze(self.deadline(), |parent, op, child| {
                    self.capture(parent, op, child)
                })
                .await
            {
                Ok(()) => break,
                Err(reverie::Error::Errno(error)) => {
                    failed_task_termination_is_fatal(task.tid, error)
                }
                Err(error) => self.retry_after(error).await,
            }
        }
        #[cfg(test)]
        if FATAL_FREEZE_CONTROL.with(|slot| {
            slot.borrow()
                .as_ref()
                .is_some_and(|control| control.calls.fetch_add(1, Ordering::SeqCst) == 1)
        }) {
            self.retry_after(
                anyhow::anyhow!("injected refusal after actual owned freeze stop").into(),
            )
            .await;
        }
        task.frozen.store(true, Ordering::Release);
        self.changed.notify_waiters();
        if self
            .tree
            .lock()
            .unwrap()
            .vfork_children
            .iter()
            .any(|child| child.same_generation(&task.terminal))
        {
            // This captured vfork child has already been signalled through its
            // exact generation. Its existing exit owner must advance the real
            // EXIT stop before the kernel can release its suspended parent.
            // Waiting for that parent at the all-task barrier would deadlock.
            return;
        }
        loop {
            let changed = self.changed.notified();
            let cleanup = {
                let mut tree = self.tree.lock().unwrap();
                if tree.killing {
                    return;
                }
                if tree
                    .tasks
                    .iter()
                    .all(|task| task.frozen.load(Ordering::Acquire))
                    && tree.newborns.iter().all(|child| !child.handed)
                {
                    tree.killing = true;
                    tree.unconfirmed_newborns = tree
                        .newborns
                        .iter()
                        .map(|child| (child.tid, child.terminal.clone()))
                        .collect();
                    Some((tree.tasks.clone(), std::mem::take(&mut tree.newborns)))
                } else {
                    None
                }
            };
            if let Some((tasks, mut newborns)) = cleanup {
                // The selected sender's canceled native future leaves its peer
                // gates in these original task owners. Transfer only after the
                // actual all-task frozen barrier, before ANY cancellation signal.
                for owner in &tasks {
                    let peers = owner.peer_invocation.lock().unwrap().clone();
                    if let Some(peers) = peers {
                        loop {
                            match peers.retire_frozen(&tasks) {
                                Ok(()) => break,
                                Err(error) => self.retry_after(error.into()).await,
                            }
                        }
                    }
                }
                // This future, retained by the run driver on refusal, is the
                // sole owner of these unhanded child receivers until reaping.
                for newborn in &mut newborns {
                    let signal = newborn.signal();
                    match signal {
                        Ok(()) | Err(Errno::ESRCH) => (),
                        Err(error) => failed_task_termination_is_fatal(newborn.tid, error),
                    }
                }
                {
                    let errors = self.signal_groups();
                    if !errors.is_empty() {
                        failed_task_termination_is_fatal(
                            self.root.expect("fatal session has a root owner"),
                            errors[0],
                        );
                    }
                }
                for task in &tasks {
                    match task.terminal.request_sigkill() {
                        Ok(()) | Err(Errno::ESRCH) => (),
                        Err(error) => failed_task_termination_is_fatal(task.tid, error),
                    }
                }
                self.changed.notify_waiters();
                future::join_all(newborns.into_iter().map(|child| async move {
                    child.reap_owned(self).await;
                }))
                .await;
                return;
            }
            changed.await;
        }
    }
    fn new<G: GlobalTool + 'static>(global: &Arc<G>, root: Pid) -> Self {
        let weak = Arc::downgrade(global);
        Self {
            root: Some(root),
            ptracer_thread: Some(std::thread::current().id()),
            reporter: Some(Box::new(move |origin| {
                if let Some(global) = weak.upgrade() {
                    global.report_backend_failure(origin);
                    true
                } else {
                    false
                }
            })),
            ..Self::default()
        }
    }

    // Host component admission only: use the existing constructor/reporter,
    // not fabricated task, source-cohort or terminal authority.
    #[cfg(test)]
    pub(crate) fn source_completion_component<G: GlobalTool + 'static>(
        global: &Arc<G>,
        original_child: Pid,
    ) -> Self {
        Self::new(global, original_child)
    }

    pub(crate) fn fail_at(&self, origin: BackendFailure, error: reverie::Error) {
        self.try_fail_at(origin, error);
    }

    pub(crate) fn cohort_terminal(
        &self,
        terminal: &TerminalCleanup,
    ) -> Option<source_cohort::TerminalOperation> {
        self.source_cohort.terminal_owner(terminal)
    }

    fn try_fail_at(&self, origin: BackendFailure, error: reverie::Error) -> bool {
        self.source_cohort.fail();
        let first = {
            let mut failure = self.failure.lock().unwrap();
            if self.closed.load(Ordering::Acquire) {
                return false;
            }
            if let Some(failure) = failure.as_mut() {
                failure.secondary.push(PtraceCleanupFailure {
                    origin,
                    error: Arc::new(error),
                });
                false
            } else {
                *failure = Some(PtraceRunFailure {
                    primary: Arc::new(error),
                    origin,
                    secondary: Vec::new(),
                    captured_prefix: None,
                });
                true
            }
        };
        if first {
            *self.cleanup_deadline.lock().unwrap() =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
            // No cause/tree lock spans the synchronous Tool transition. Local
            // observers cannot see publication until the Tool closes its waits.
            if !self.reporter.as_ref().is_some_and(|report| report(origin)) {
                self.failure
                    .lock()
                    .unwrap()
                    .as_mut()
                    .expect("stored primary")
                    .secondary
                    .push(PtraceCleanupFailure {
                        origin: BackendFailure {
                            phase: "ptrace failure publication",
                            ..origin
                        },
                        error: Arc::new(anyhow::Error::new(GlobalFailurePublicationLost).into()),
                    });
            }
            self.published.store(true, Ordering::Release);
        }
        self.changed.notify_waiters();
        true
    }

    pub(crate) fn request_termination(&self, error: reverie::Error) -> bool {
        let Some(root) = self.root else {
            return false;
        };
        self.try_fail_at(
            BackendFailure {
                pid: root,
                tid: root,
                phase: "ptrace supervisor termination",
            },
            error,
        )
    }

    pub(crate) fn fail(&self, error: reverie::Error) {
        let root = self.root.expect("ordinary failure has a root owner");
        self.fail_at(
            BackendFailure {
                pid: root,
                tid: root,
                phase: "ptrace tree cleanup",
            },
            error,
        );
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.published.load(Ordering::Acquire)
    }

    pub(crate) fn callback_diagnostics(&self) -> Vec<crate::PtraceCallbackDiagnostic> {
        self.callback_diagnostics.lock().unwrap().clone()
    }

    pub(crate) fn take_callback_diagnostics(&self) -> Vec<crate::PtraceCallbackDiagnostic> {
        std::mem::take(&mut *self.callback_diagnostics.lock().unwrap())
    }

    pub(crate) fn failure_snapshot(&self) -> Option<PtraceRunFailure> {
        self.failure
            .lock()
            .unwrap()
            .as_ref()
            .map(PtraceRunFailure::snapshot)
    }

    pub(crate) async fn take_public_failure(&self) -> Option<PtraceRunFailure> {
        loop {
            let changed = self.changed.notified();
            {
                let mut failure = self.failure.lock().unwrap();
                // A supervisor can publish from another thread. Do not move
                // its cause while its synchronous logical publication runs.
                if failure.is_none() || self.published.load(Ordering::Acquire) {
                    self.closed.store(true, Ordering::Release);
                    return failure.take();
                }
            }
            changed.await;
        }
    }

    pub(crate) fn deadline(&self) -> std::time::Instant {
        self.cleanup_deadline
            .lock()
            .unwrap()
            .expect("cleanup follows failure publication")
    }

    pub(crate) fn resume_cleanup(&self) {
        self.tree.lock().unwrap().cleanup_refusal = None;
        *self.cleanup_deadline.lock().unwrap() =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
        self.retry_epoch.fetch_add(1, Ordering::AcqRel);
        self.retry_changed.notify_waiters();
    }

    async fn wait_retry(&self, epoch: usize) {
        loop {
            let changed = self.retry_changed.notified();
            if self.retry_epoch.load(Ordering::Acquire) != epoch {
                return;
            }
            changed.await;
        }
    }

    pub(crate) async fn join_owned(&self) {
        loop {
            let handles = std::mem::take(&mut *self.joins.lock().unwrap());
            if handles.is_empty() {
                break;
            }
            for handle in handles {
                if let Err(error) = handle.await {
                    self.fail(anyhow::Error::new(error).into());
                }
            }
        }
    }

    pub(crate) async fn cancelled(&self) {
        loop {
            let changed = self.changed.notified();
            if self.is_failed() {
                return;
            }
            changed.await;
        }
    }

    pub(crate) async fn cleanup_refused(&self) -> reverie::Error {
        loop {
            let changed = self.changed.notified();
            if let Some(message) = &self.tree.lock().unwrap().cleanup_refusal {
                return anyhow::anyhow!("fatal ptrace cleanup refused: {message}").into();
            }
            changed.await;
        }
    }
}

/// One retained observation failure for the existing traced process tree.
/// This is a failure wake/cause, never another task registry or wait owner.
enum TaskFailureCause {
    ParentCompletion(Pid, String),
    // The tag stays after the root takes the typed error, so late subscribers
    // cannot miss a failure just because its diagnostic was already consumed.
    RunLoop {
        tid: Pid,
        error: Option<reverie::Error>,
    },
}

#[derive(Default)]
pub(crate) struct ParentCompletionFailure {
    first: StdMutex<Option<TaskFailureCause>>,
    changed: Notify,
}

impl ParentCompletionFailure {
    fn record(&self, tid: Pid, error: &TraceError) {
        let mut first = self.first.lock().unwrap();
        if first.is_none() {
            *first = Some(TaskFailureCause::ParentCompletion(tid, error.to_string()));
        }
    }

    pub(crate) fn cause(&self) -> Option<(Pid, String)> {
        match self.first.lock().unwrap().as_ref() {
            Some(TaskFailureCause::ParentCompletion(tid, message)) => Some((*tid, message.clone())),
            _ => None,
        }
    }

    fn record_run_error(&self, tid: Pid, error: reverie::Error) {
        let mut first = self.first.lock().unwrap();
        if first.is_none() {
            tracing::error!(%tid, %error, "retaining original ptrace run-loop failure");
            *first = Some(TaskFailureCause::RunLoop {
                tid,
                error: Some(error),
            });
        } else {
            tracing::error!(%tid, %error, "additional ptrace failure after retained first cause");
        }
    }

    fn run_failed(&self) -> bool {
        matches!(
            *self.first.lock().unwrap(),
            Some(TaskFailureCause::RunLoop { .. })
        )
    }

    pub(crate) fn take_run_error(&self) -> Option<(Pid, reverie::Error)> {
        match self.first.lock().unwrap().as_mut() {
            Some(TaskFailureCause::RunLoop { tid, error }) => {
                error.take().map(|error| (*tid, error))
            }
            _ => None,
        }
    }

    async fn wait(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.first.lock().unwrap().is_some() {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod run_error_tests;

#[cfg(test)]
mod parent_completion_failure_tests {
    use std::task::Context;
    use std::task::Poll;

    use super::*;

    #[test]
    fn original_parent_error_precedes_subscriber_and_remains_sticky() {
        let failure = ParentCompletionFailure::default();
        let original = TraceError::Errno(Errno::EPROTO);
        failure.record(Pid::from_raw(41), &original);
        failure.record(Pid::from_raw(42), &TraceError::Errno(Errno::EIO));
        assert_eq!(
            failure.cause(),
            Some((Pid::from_raw(41), original.to_string()))
        );
        assert!(failure.wait().now_or_never().is_some());
    }

    #[test]
    fn canceled_parent_failure_subscriber_retains_actual_first_cause() {
        let failure = ParentCompletionFailure::default();
        let waker = futures::task::noop_waker();
        let mut context = Context::from_waker(&waker);
        let mut abandoned = Box::pin(failure.wait());
        assert_eq!(abandoned.as_mut().poll(&mut context), Poll::Pending);
        drop(abandoned);
        let mut current = Box::pin(failure.wait());
        assert_eq!(current.as_mut().poll(&mut context), Poll::Pending);
        let original = TraceError::Errno(Errno::EPROTO);
        failure.record(Pid::from_raw(43), &original);
        failure.changed.notify_waiters();
        assert_eq!(current.as_mut().poll(&mut context), Poll::Ready(()));
        assert_eq!(
            failure.cause(),
            Some((Pid::from_raw(43), original.to_string()))
        );
        // This validates the wake/cause, not a fabricated native final wait.
    }
}

async fn wait_for_task_failure<G: GlobalTool>(global: &G, parent: &ParentCompletionFailure) {
    let tool_failure = global.wait_for_backend_failure().fuse();
    let parent_failure = parent.wait().fuse();
    futures::pin_mut!(tool_failure, parent_failure);
    futures::select_biased! {
        _ = tool_failure => {},
        _ = parent_failure => {},
    }
}

/// Issued only after an actual final wait and consuming Tool/child cleanup.
struct CompletedTaskRun {
    status: ExitStatus,
    cleanup: safeptrace::TerminalCleanup,
}

/// All the info needed to be able to interact with the global state.
struct GlobalState<G: GlobalTool> {
    /// The tool's static configuration data.
    cfg: G::Config,

    /// Reference to the tool's global state. This is used to send it "rpc" messages.
    gs_ref: Arc<G>,

    /// Events the tool is subscripted (like interception)
    subscriptions: Arc<Subscription>,

    /// guests are sequentialized already (by detcore for example), gdbserver
    /// should avoid sequentialize threads.
    sequentialized_guest: Arc<bool>,

    /// Marker and exact RIP identifying a binary-rewriter syscall trap.
    injected_syscall_trap: Option<InjectedSyscallTrap>,

    /// Optional dynamic LiteInst runtime configuration.
    liteinst_runtime: Option<LiteinstRuntimeConfig>,

    fatal_session: Arc<FatalSession>,
    source_supported: bool,

    /// Optional collector for general ptrace lifecycle activity.
    backend_stats: Option<PtraceBackendStatsSource>,

    /// Backend-local failure is observable even when Tool failure hooks are no-ops.
    parent_completion_failure: Arc<ParentCompletionFailure>,
}

impl<G: GlobalTool> Clone for GlobalState<G> {
    fn clone(&self) -> Self {
        Self {
            cfg: self.cfg.clone(),
            gs_ref: self.gs_ref.clone(),
            subscriptions: self.subscriptions.clone(),
            sequentialized_guest: self.sequentialized_guest.clone(),
            injected_syscall_trap: self.injected_syscall_trap.clone(),
            liteinst_runtime: self.liteinst_runtime.clone(),
            fatal_session: self.fatal_session.clone(),
            source_supported: self.source_supported,
            backend_stats: self.backend_stats.clone(),
            parent_completion_failure: Arc::clone(&self.parent_completion_failure),
        }
    }
}

/// A raw argument remains exact unless both its type and launch ownership are known.
struct SyscallArgsForLog {
    nr: Sysno,
    args: SyscallArgs,
    command_bootstrap: bool,
}

struct CommandBootstrapAddress(usize);

impl fmt::Debug for CommandBootstrapAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            f.write_str("0")
        } else {
            write!(f, "<hostaddr {:#x}>", self.0)
        }
    }
}

impl fmt::Debug for SyscallArgsForLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.command_bootstrap || self.nr != Sysno::execve {
            return fmt::Debug::fmt(&self.args, f);
        }
        // Command::do_exec passes pathname, argv and envp pointers from the
        // inherited launcher image. The ABI-unused registers are still raw.
        f.debug_struct("SyscallArgs")
            .field("arg0", &CommandBootstrapAddress(self.args.arg0))
            .field("arg1", &CommandBootstrapAddress(self.args.arg1))
            .field("arg2", &CommandBootstrapAddress(self.args.arg2))
            .field("arg3", &self.args.arg3)
            .field("arg4", &self.args.arg4)
            .field("arg5", &self.args.arg5)
            .finish()
    }
}

/// The caller of one injection, retained through the same native execution path.
/// Backend setup is not a Tool operation even when both inject the same syscall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InjectionOrigin {
    Tool,
    Backend,
}

#[cfg(target_arch = "x86_64")]
mod original_context;
#[cfg(target_arch = "x86_64")]
mod original_read;
#[cfg(target_arch = "x86_64")]
mod original_setsockopt;
#[cfg(target_arch = "x86_64")]
mod private_signal;

// Private producer: consumers borrow this only at an actual stopped boundary.
struct InitialCommandStop<'a> {
    root_tid: Tid,
    former_tid: Option<Tid>,
    filter: &'a reverie::process::seccomp::Filter,
}
impl reverie::InitialCommandObservation for InitialCommandStop<'_> {
    fn root_tid(&self) -> Tid {
        self.root_tid
    }
    fn former_tid(&self) -> Option<Tid> {
        self.former_tid
    }
    fn seccomp_filter(&self) -> &reverie::process::seccomp::Filter {
        self.filter
    }
}

/// Event configuration supplied when a traced task is created.
pub(crate) struct TracedTaskOptions<'a> {
    pub(crate) command_bootstrap: bool,
    pub(crate) command_filter: Option<Arc<reverie::process::seccomp::Filter>>,
    pub(crate) events: &'a Subscription,
    pub(crate) injected_syscall_trap: Option<InjectedSyscallTrap>,
    pub(crate) liteinst_runtime: Option<LiteinstRuntimeConfig>,
    pub(crate) backend_stats: Option<PtraceBackendStatsSource>,
}

/// Our runtime representation of what Reverie knows about a guest thread. Its
/// lifetime matches the lifetime of the thread.
pub struct TracedTask<L: Tool> {
    ordinary_held_stop: Arc<StdMutex<Option<HeldRootStop>>>,
    // Session diagnostics are append-only until every task owner completes.
    // Therefore another task cannot invalidate this private entry index.
    pending_callback_diagnostic: Option<usize>,
    ordinary_exec: Arc<StdMutex<HashMap<Pid, Arc<OrdinaryExecSlot<L>>>>>,
    /// Thread ID.
    tid: Pid,

    /// Process ID.
    pid: Pid,

    /// Parent process ID.
    ppid: Option<Pid>,

    /// State associated with the thread. Unique for each thread.
    thread_state: L::ThreadState,

    /// Actual terminal wait already acknowledged to the Tool. Forwarding that
    /// same event through the run loop must not emit a second acknowledgement.
    observed_terminal: Option<ExitStatus>,

    /// Admitted native child, then spawned child, awaiting the ordinary list.
    /// The initial wait and its exact result belong to this actor rather than
    /// its cancellable syscall future. Cleanup never reconstructs an identity.
    pending_child: Option<PendingChild>,

    /// State associated with the process. This is shared among threads in the
    /// same thread group.
    process_state: Arc<L>,

    /// Global state. This is shared among all threads in a process tree.
    global_state: GlobalState<L::GlobalState>,

    /// True only for TracerBuilder::spawn's Command root until successful exec.
    /// Descendants and spawn_fn never inherit this logging provenance.
    command_bootstrap: bool,

    /// Exact owned installer input for the Command root only. Consumed at the
    /// first actual EXEC; children and function guests never inherit it.
    command_filter: Option<Arc<reverie::process::seccomp::Filter>>,

    /// True if we can intercept CPUID, false otherwise.
    has_cpuid_interception: bool,

    /// Original call still owned by the current syscall callback. Only an
    /// ordinary, unconverted seccomp callback owns a kernel entry to skip;
    /// an injected frame and an already-skipped call are logical operations.
    /// Taking this record consumes that authority, including fast tail injection
    /// which transfers the original call to the callback's final resume.
    pending_syscall: Option<(Sysno, SyscallArgs)>,

    /// Original native Read tuple retained before the Tool can alter registers.
    /// A capture error refuses range inspection without changing ordinary Read.
    original_read_entry: Option<Result<original_context::OriginalReadEntry, String>>,

    /// One actual native entry, retained before Tool code can change registers.
    #[cfg(target_arch = "x86_64")]
    original_setsockopt_entry:
        Option<Result<original_setsockopt::OriginalSetsockoptEntry, TraceError>>,

    /// Original consumed callback stop; never reconstructed by assume_stopped.
    source_stop: Option<Arc<safeptrace::SourceStop>>,
    cohort: Option<source_cohort::Member>,
    #[cfg(target_arch = "x86_64")]
    source_observer: Arc<StdMutex<source_observation::State>>,

    /// The pending syscall was converted out of its seccomp stop before Tool dispatch.
    pending_syscall_already_skipped: bool,

    /// Address of the writable e9tool register frame for the active event.
    injected_syscall_frame: Option<usize>,

    /// Per-process dynamic LiteInst handshake and patched-site state.
    liteinst_runtime: Arc<StdMutex<LiteinstRuntimeState>>,

    /// Controller-owned breakpoint preventing the executable entry before Ready.
    liteinst_entry_guard: Option<LiteinstEntryGuard>,

    /// Original typed fail-closed error retained while the exit waiter reaps root.
    liteinst_failure: Option<LiteinstActivationFailure>,

    /// pending signal to deliver. This can happen when
    /// syscall got interrupted (by signal)
    pending_signal: Option<Signal>,

    // Same stopped task, retained across the Tool's positive Call cancellation.
    // This is not an errno, an independent operation registry, or a result.
    #[cfg(target_arch = "x86_64")]
    interrupted_read: Option<original_read::InterruptedRead>,

    /// Original logical frame and explicit finite continuation, never a saved
    /// future across guest handler execution.
    #[cfg(target_arch = "x86_64")]
    private_signal: private_signal::State,

    /// A channel to allow short-circuiting the next state to main run loop. This
    /// is useful inside of `inject` or `tail_inject` where we might need to
    /// cancel a future early.
    next_state: mpsc::Sender<Result<Wait, TraceError>>,

    /// The receiving end of the next_state channel.
    next_state_rx: Option<mpsc::Receiver<Result<Wait, TraceError>>>,

    /// The timer tracking this task. Used to trigger RCB-based `timeouts`.
    timer: TaskTimer,

    /// Set when `tail_inject` needs to cancel the current tool handler.
    cancel_handler: Arc<AtomicBool>,

    /// Child processes to wait on. When one of the children exits, it should be
    /// removed from this list.
    child_procs: Arc<Mutex<Children>>,

    /// Child threads to wait on. When one of the child threads exits, it should
    /// be removed from this list.
    child_threads: Arc<Mutex<Children>>,

    /// Channel to send child processes to that are left over by the time this
    /// task exits.
    orphanage: mpsc::Sender<Child>,

    /// broadcast to kill all daemons
    daemon_kill_switch: broadcast::Sender<()>,

    /// Channel to damonize a process
    daemonizer: mpsc::Sender<broadcast::Receiver<()>>,

    /// The rx end of `daemonizer`.
    daemonizer_rx: Option<mpsc::Receiver<broadcast::Receiver<()>>>,

    /// Total number of tasks
    ntasks: Arc<AtomicUsize>,

    /// Total number of daemons
    ndaemons: Arc<AtomicUsize>,

    /// Task is a daemon
    is_a_daemon: bool,

    /// Software breakpoints.
    // NB: For multi-threaded programs, sw breakpoints apply to all threads
    // because they're in the same address space. Hence removing sw
    // breakpoint in one thread also remove it for the rest of the threads
    // in the same process group. *However*, our model is slightly different
    // because we use different tx/rx channels even the threads are in the
    // same process group, hence each threads owns `breakpoints: HashMap`
    // instead of `Arc<Mutex<..>>`.
    breakpoints: HashMap<u64, u64>,

    /// Notify gdbserver start accepting incoming packets.
    gdbserver_start_tx: Option<oneshot::Sender<()>>,

    /// task is suspended (received SIGSTOP)
    suspended: Arc<AtomicBool>,

    /// Notify gdbserver there's a new stop event.
    gdb_stop_tx: Option<mpsc::Sender<StoppedInferior>>,

    /// Task is attached by gdb.
    // NB: gdb doesn't always attach everything, when fork/clone is called.
    // gdb also allows detach from a task, and re-attach again.
    attached_by_gdb: bool,

    /// Task is resumed by gdb.
    // NB: gdb doesn't always attach everything, when fork/clone is called.
    // gdb also allows detach from a task, and re-attach again.
    resumed_by_gdb: Option<ResumeAction>,

    /// GDB resume request, gdbstub is the sender
    gdb_resume_tx: Option<mpsc::Sender<ResumeInferior>>,

    /// GDB resume request, reverie is the receiver
    gdb_resume_rx: Option<mpsc::Receiver<ResumeInferior>>,

    /// Request sent by gdb. the tx channel is used by gdb instead of
    /// `TracedTask`.
    gdb_request_tx: Option<mpsc::Sender<GdbRequest>>,

    /// Receiver to receive gdb request.
    gdb_request_rx: Option<mpsc::Receiver<GdbRequest>>,

    /// Wait to be resumed when in sigstop due to all stop mode.
    exit_suspend_tx: Option<mpsc::Sender<Pid>>,

    /// Wait to be resumed when in sigstop due to all stop mode.
    exit_suspend_rx: Option<mpsc::Receiver<Pid>>,

    /// Suspended task when hitting swbp. This is used to implement gdb's
    /// all stop mode.
    suspended_tasks: BTreeMap<Pid, Suspended>,

    /// Task needs (single) step over the swbp instruciton when a swbp is
    /// hit. unless this is done, if is not safe for other threads running
    /// in parallel to report breakpoint, otherwise there're could be an
    /// interleaved step-over, which might remove the breakpoint, hence
    /// causing others to miss the breakpoint.
    needs_step_over: Arc<Mutex<()>>,

    /// Whether or not the tool is currently holding a handle on the guest Stack (and thus
    /// potentially using actual stack memory within the guest).
    stack_checked_out: Arc<AtomicBool>,
}

impl<L: Tool> fmt::Debug for TracedTask<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TracedTask")
            .field("tid", &self.tid)
            .field("pid", &self.pid)
            .field("ppid", &self.ppid)
            .finish()
    }
}

impl<L: Tool> TracedTask<L> {
    /// Create a new TracedTask.
    pub(crate) fn new(
        tid: Pid,
        cfg: <L::GlobalState as GlobalTool>::Config,
        gs_ref: Arc<L::GlobalState>,
        options: TracedTaskOptions<'_>,
        orphanage: mpsc::Sender<Child>,
        daemon_kill_switch: broadcast::Sender<()>,
        mut gdbserver: Option<GdbServer>,
    ) -> Self
    where
        L::GlobalState: 'static,
    {
        let process_state = Arc::new(L::new(tid, &cfg));
        let fatal_session = Arc::new(FatalSession::new(&gs_ref, tid));
        let global_state = GlobalState {
            gs_ref,
            cfg,
            subscriptions: Arc::new(options.events.clone()),
            sequentialized_guest: Arc::new(
                gdbserver
                    .as_ref()
                    .map(|s| s.sequentialized_guest)
                    .unwrap_or(false),
            ),
            source_supported: cfg!(target_arch = "x86_64")
                && options.command_bootstrap
                && gdbserver.is_none()
                && options.liteinst_runtime.is_none()
                && options.injected_syscall_trap.is_none(),
            injected_syscall_trap: options.injected_syscall_trap.clone(),
            liteinst_runtime: options.liteinst_runtime,
            fatal_session,
            backend_stats: options.backend_stats,
            parent_completion_failure: Arc::new(ParentCompletionFailure::default()),
        };
        let thread_state = process_state.init_thread_state(tid, None);
        let (next_state, next_state_rx) = mpsc::channel(1);
        let (daemonizer, daemonizer_rx) = mpsc::channel(1);
        let (gdb_resume_tx, gdb_resume_rx) = mpsc::channel(1);
        let (gdb_request_tx, gdb_request_rx) = mpsc::channel(1);
        let (exit_suspend_tx, exit_suspend_rx) = mpsc::channel(16);
        Self {
            tid,
            pid: tid,
            ppid: None,
            thread_state,
            observed_terminal: None,
            pending_child: None,
            process_state,
            global_state,
            ordinary_held_stop: Arc::new(StdMutex::new(None)),
            pending_callback_diagnostic: None,
            ordinary_exec: Arc::new(StdMutex::new(HashMap::new())),
            command_bootstrap: options.command_bootstrap,
            command_filter: options.command_filter,
            has_cpuid_interception: false,
            pending_syscall: None,
            original_read_entry: None,
            #[cfg(target_arch = "x86_64")]
            original_setsockopt_entry: None,
            source_stop: None,
            cohort: None,
            #[cfg(target_arch = "x86_64")]
            source_observer: Arc::new(StdMutex::new(source_observation::State::startup())),
            pending_syscall_already_skipped: false,
            injected_syscall_frame: None,
            liteinst_runtime: Arc::new(StdMutex::new(LiteinstRuntimeState::default())),
            liteinst_entry_guard: None,
            liteinst_failure: None,
            next_state,
            next_state_rx: Some(next_state_rx),
            timer: TaskTimer::Live(if options.command_bootstrap {
                Timer::for_initial_command(tid, tid)
            } else {
                Timer::new(tid, tid)
            }),
            cancel_handler: Arc::new(AtomicBool::new(false)),
            pending_signal: None,
            #[cfg(target_arch = "x86_64")]
            interrupted_read: None,
            #[cfg(target_arch = "x86_64")]
            private_signal: private_signal::State::default(),
            child_procs: Arc::new(Mutex::new(Children::new())),
            child_threads: Arc::new(Mutex::new(Children::new())),
            orphanage,
            daemon_kill_switch,
            daemonizer,
            daemonizer_rx: Some(daemonizer_rx),
            ntasks: Arc::new(AtomicUsize::new(1)),
            ndaemons: Arc::new(AtomicUsize::new(0)),
            is_a_daemon: false,
            gdbserver_start_tx: gdbserver.as_mut().and_then(|s| s.server_tx.take()),
            gdb_stop_tx: gdbserver
                .as_mut()
                .and_then(|s| s.inferior_attached_tx.take()),
            attached_by_gdb: false,
            resumed_by_gdb: None,
            gdb_resume_tx: Some(gdb_resume_tx),
            gdb_resume_rx: Some(gdb_resume_rx),
            breakpoints: HashMap::new(),
            suspended: Arc::new(AtomicBool::new(false)),
            gdb_request_tx: Some(gdb_request_tx),
            gdb_request_rx: Some(gdb_request_rx),
            exit_suspend_tx: Some(exit_suspend_tx),
            exit_suspend_rx: Some(exit_suspend_rx),
            needs_step_over: Arc::new(Mutex::new(())),
            suspended_tasks: BTreeMap::new(),
            stack_checked_out: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a child TracedTask corresponding to a clone()
    fn cloned(&self, child: Pid, timer: TaskTimer, cohort: Option<source_cohort::Member>) -> Self {
        let global_state = self.global_state.clone();
        let process_state = self.process_state.clone();
        let thread_state =
            process_state.init_thread_state(child, Some((self.tid, &self.thread_state)));
        let (next_state, next_state_rx) = mpsc::channel(1);
        let (daemonizer, daemonizer_rx) = mpsc::channel(1);
        let (gdb_resume_tx, gdb_resume_rx) = mpsc::channel(1);
        let (gdb_request_tx, gdb_request_rx) = mpsc::channel(1);
        let (exit_suspend_tx, exit_suspend_rx) = mpsc::channel(16);
        self.ntasks.fetch_add(1, Ordering::SeqCst);
        // Enrollment is synchronous with task construction. A terminal newborn
        // takes the same consuming exit path as a child that reaches run().
        if self.is_a_daemon {
            self.ndaemons.fetch_add(1, Ordering::SeqCst);
        }
        Self {
            tid: child,
            pid: self.pid,
            ppid: self.ppid,
            thread_state,
            observed_terminal: None,
            pending_child: None,
            process_state,
            global_state,
            ordinary_held_stop: Arc::new(StdMutex::new(None)),
            pending_callback_diagnostic: None,
            ordinary_exec: self.ordinary_exec.clone(),
            command_bootstrap: false,
            command_filter: None,
            has_cpuid_interception: self.has_cpuid_interception,
            pending_syscall: None,
            original_read_entry: None,
            #[cfg(target_arch = "x86_64")]
            original_setsockopt_entry: None,
            source_stop: None,
            cohort,
            #[cfg(target_arch = "x86_64")]
            source_observer: Arc::new(StdMutex::new(source_observation::State::startup())),
            pending_syscall_already_skipped: false,
            injected_syscall_frame: None,
            liteinst_runtime: self.liteinst_runtime.clone(),
            liteinst_entry_guard: None,
            liteinst_failure: None,
            next_state,
            next_state_rx: Some(next_state_rx),
            timer,
            cancel_handler: Arc::new(AtomicBool::new(false)),
            pending_signal: None,
            #[cfg(target_arch = "x86_64")]
            interrupted_read: None,
            #[cfg(target_arch = "x86_64")]
            private_signal: private_signal::State::default(),
            child_procs: self.child_procs.clone(),
            child_threads: self.child_threads.clone(),
            orphanage: self.orphanage.clone(),
            daemon_kill_switch: self.daemon_kill_switch.clone(),
            daemonizer,
            daemonizer_rx: Some(daemonizer_rx),
            ntasks: self.ntasks.clone(),
            ndaemons: self.ndaemons.clone(),
            is_a_daemon: self.is_a_daemon,
            gdbserver_start_tx: None,
            gdb_stop_tx: None,
            attached_by_gdb: self.attached_by_gdb,
            resumed_by_gdb: self.resumed_by_gdb,
            gdb_resume_tx: Some(gdb_resume_tx),
            gdb_resume_rx: Some(gdb_resume_rx),
            breakpoints: self.breakpoints.clone(),
            suspended: Arc::new(AtomicBool::new(false)),
            gdb_request_tx: Some(gdb_request_tx),
            gdb_request_rx: Some(gdb_request_rx),
            exit_suspend_tx: Some(exit_suspend_tx),
            exit_suspend_rx: Some(exit_suspend_rx),
            needs_step_over: self.needs_step_over.clone(),
            suspended_tasks: BTreeMap::new(),
            stack_checked_out: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a child TracedTask corresponding to a fork()
    fn forked(&self, child: Pid, timer: TaskTimer, cohort: Option<source_cohort::Member>) -> Self {
        let process_state = Arc::new(L::new(child, &self.global_state.cfg));
        let thread_state =
            process_state.init_thread_state(child, Some((self.tid, &self.thread_state)));
        let (next_state, next_state_rx) = mpsc::channel(1);
        let (daemonizer, daemonizer_rx) = mpsc::channel(1);
        let (gdb_resume_tx, gdb_resume_rx) = mpsc::channel(1);
        let (gdb_request_tx, gdb_request_rx) = mpsc::channel(1);
        let (exit_suspend_tx, exit_suspend_rx) = mpsc::channel(16);
        self.ntasks.fetch_add(1, Ordering::SeqCst);
        Self {
            tid: child,
            pid: child,
            ppid: Some(self.pid),
            thread_state,
            observed_terminal: None,
            pending_child: None,
            process_state,
            global_state: self.global_state.clone(),
            ordinary_held_stop: Arc::new(StdMutex::new(None)),
            pending_callback_diagnostic: None,
            ordinary_exec: Arc::new(StdMutex::new(HashMap::new())),
            command_bootstrap: false,
            command_filter: None,
            has_cpuid_interception: self.has_cpuid_interception,
            pending_syscall: None,
            original_read_entry: None,
            #[cfg(target_arch = "x86_64")]
            original_setsockopt_entry: None,
            source_stop: None,
            cohort,
            #[cfg(target_arch = "x86_64")]
            source_observer: Arc::new(StdMutex::new(source_observation::State::startup())),
            pending_syscall_already_skipped: false,
            injected_syscall_frame: None,
            liteinst_runtime: Arc::new(StdMutex::new(
                self.liteinst_runtime.lock().unwrap().clone(),
            )),
            liteinst_entry_guard: None,
            liteinst_failure: None,
            next_state,
            next_state_rx: Some(next_state_rx),
            timer,
            cancel_handler: Arc::new(AtomicBool::new(false)),
            pending_signal: None,
            #[cfg(target_arch = "x86_64")]
            interrupted_read: None,
            #[cfg(target_arch = "x86_64")]
            private_signal: private_signal::State::default(),
            child_procs: Arc::new(Mutex::new(Children::new())),
            child_threads: Arc::new(Mutex::new(Children::new())),
            orphanage: self.orphanage.clone(),
            daemon_kill_switch: self.daemon_kill_switch.clone(),
            daemonizer,
            daemonizer_rx: Some(daemonizer_rx),
            ntasks: self.ntasks.clone(),
            ndaemons: self.ndaemons.clone(),
            // NB: if daemon forks, then its child's parent pid is no longer 1.
            is_a_daemon: false,
            gdbserver_start_tx: None,
            gdb_stop_tx: None,
            attached_by_gdb: self.attached_by_gdb,
            resumed_by_gdb: None,
            gdb_resume_tx: Some(gdb_resume_tx),
            gdb_resume_rx: Some(gdb_resume_rx),
            breakpoints: self.breakpoints.clone(),
            suspended: Arc::new(AtomicBool::new(false)),
            gdb_request_tx: Some(gdb_request_tx),
            gdb_request_rx: Some(gdb_request_rx),
            exit_suspend_tx: Some(exit_suspend_tx),
            exit_suspend_rx: Some(exit_suspend_rx),
            needs_step_over: Arc::new(Mutex::new(())),
            suspended_tasks: BTreeMap::new(),
            stack_checked_out: Arc::new(AtomicBool::new(false)),
        }
    }

    fn read_injected_syscall_frame(
        &self,
        task: &Stopped,
        address: usize,
    ) -> Result<InjectedSyscallFrame, TraceError> {
        let address = Addr::from_raw(address).ok_or(Errno::EFAULT)?;
        Ok(task.read_value(address)?)
    }

    fn write_injected_syscall_frame(
        &self,
        task: &Stopped,
        address: usize,
        frame: &InjectedSyscallFrame,
    ) -> Result<(), TraceError> {
        let address = AddrMut::from_raw(address).ok_or(Errno::EFAULT)?;
        let mut task = Stopped::new_unchecked(task.pid());
        Ok(task.write_value(address, frame)?)
    }

    fn write_injected_syscall_result(
        &self,
        task: &Stopped,
        result: Result<i64, Errno>,
    ) -> Result<(), TraceError> {
        let address = self.injected_syscall_frame.ok_or(Errno::EIO)?;
        let mut frame = self.read_injected_syscall_frame(task, address)?;
        let result = result.unwrap_or_else(|errno| -(errno.into_raw() as i64));
        frame.set_result(result);
        self.write_injected_syscall_frame(task, address, &frame)
    }

    fn read_guest_registers(&self, task: &Stopped) -> Result<libc::user_regs_struct, TraceError> {
        let mut regs = task.getregs()?;
        if let Some(address) = self.injected_syscall_frame {
            let frame = self.read_injected_syscall_frame(task, address)?;
            frame.copy_to_user_regs(&mut regs);
        }
        Ok(regs)
    }

    fn write_guest_registers(
        &self,
        task: &Stopped,
        regs: &libc::user_regs_struct,
    ) -> Result<(), TraceError> {
        if let Some(address) = self.injected_syscall_frame {
            let mut frame = self.read_injected_syscall_frame(task, address)?;
            let current = self.read_guest_registers(task)?;
            InjectedSyscallFrame::validate_user_regs_update(&current, regs)?;
            if self.global_state.liteinst_runtime.is_some() {
                validate_liteinst_user_regs_update(&current, regs)?;
            }
            frame.copy_from_user_regs(regs);
            self.write_injected_syscall_frame(task, address, &frame)
        } else {
            task.setregs(regs)
        }
    }

    fn get_syscall(&self, task: &Stopped) -> Result<Syscall, TraceError> {
        let regs = task.getregs()?;
        let nr = Sysno::from(regs.orig_syscall() as i32);

        let args = regs.args();

        Ok(Syscall::from_raw(
            nr,
            SyscallArgs::new(
                args.0 as usize,
                args.1 as usize,
                args.2 as usize,
                args.3 as usize,
                args.4 as usize,
                args.5 as usize,
            ),
        ))
    }
}

fn set_ret(task: &Stopped, ret: Reg) -> Result<Reg, TraceError> {
    let mut regs = task.getregs()?;
    let old = regs.ret();
    *regs.ret_mut() = ret;
    task.setregs(&regs)?;
    Ok(old)
}

/// Canonical marker emitted when a guest-thread task dies of a panic.
///
/// The token is what a harness greps for, in the same spirit as
/// `HERMIT_SKID_OVERSHOOT`; keep it stable. It exists because an exit code
/// alone cannot say *why* a run ended, and this failure mode was expensive
/// precisely because it was unreadable: the run hung, so a panic was
/// indistinguishable from a slow run to every harness that judges by wall time.
const TASK_PANIC_MARKER: &str = "HERMIT_TASK_PANIC";

/// Exit status used when a guest-thread task panics.
///
/// This is rustc's conventional panic status inside the tracer process. An
/// embedding executable may normalize it at an outer process boundary, so the
/// marker above remains the authoritative machine-readable diagnosis.
const TASK_PANIC_EXIT_CODE: i32 = 101;

/// Renders the one-line panic marker. Separate from the exit so it can be
/// tested without ending the test process.
fn format_task_panic_marker(tid: Pid, payload: &(dyn std::any::Any + Send)) -> String {
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&'static str>().copied())
        .unwrap_or("<non-string panic payload>");
    // Always exactly one line: a marker that can wrap is a marker a harness
    // cannot grep. The default hook may have written into a test capture
    // buffer; this independent line exists to be machine-read.
    let message: String = message
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    format!(
        "{} tid={} exit={} message={}",
        TASK_PANIC_MARKER,
        tid,
        TASK_PANIC_EXIT_CODE,
        message.trim()
    )
}

/// A guest-thread task died of a panic. End the run, loudly.
///
/// WHY THE PROCESS EXITS RATHER THAN PROPAGATING AN ERROR. Tokio's task harness
/// catches a panic in a `spawn_local` task and parks it in the `JoinHandle`,
/// which nothing polls until `tool_exit`. The run cannot reach `tool_exit`,
/// because by then the tool's scheduler is parked waiting for a turn request
/// this task will never post, and detcore's `Ivar` has no way to report that
/// its writer is gone -- so every other guest thread waits forever and the run
/// hangs until an external timeout kills it. There is no live party left to
/// hand an error to. detcore reached the same conclusion about its own
/// scheduler task and built `immediate_fatal_exit` for exactly this.
///
/// Exiting does not leak the guest: `postspawn` sets `PTRACE_O_EXITKILL`, so
/// the tracees die with the tracer. The terminal-deadlock path already relies
/// on that.
fn guest_task_panic_is_fatal(tid: Pid, payload: Box<dyn std::any::Any + Send>) -> ! {
    // Write directly: eprintln! can stop in libtest's capture buffer, which
    // process::exit never returns to the harness. Keep this marker free of
    // tracing's real wall-clock prefix and preserve the fatal exit on I/O error.
    let _ = writeln!(
        std::io::stderr(),
        "{}",
        format_task_panic_marker(tid, payload.as_ref())
    );
    exit_failed_tracer_process(TASK_PANIC_EXIT_CODE)
}

/// The run has already failed, and its exact task could not be terminated.
/// Returning would drop this actor and detach its retained child JoinHandles;
/// waiting for a final event after a refused kill has no progress guarantee.
/// Use the same process boundary as a fatal task panic. EXITKILL requests
/// kernel termination of the attached domain, but this is not a drain receipt:
/// the outside command owner must still observe every original actor's exit.
const TASK_TERMINATION_FAILURE_EXIT_CODE: i32 = 102;

fn check_failed_task_termination(result: Result<(), Errno>) -> Result<(), Errno> {
    match result {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(error),
    }
}

fn format_task_termination_failure(tid: Pid, error: Errno) -> String {
    format!(
        "HERMIT_TASK_TERMINATION_FAILED tid={tid} exit={} errno={} backend_failure=acknowledged cleanup=unconfirmed",
        TASK_TERMINATION_FAILURE_EXIT_CODE,
        error.into_raw(),
    )
}

fn failed_task_termination_is_fatal(tid: Pid, error: Errno) -> ! {
    // The primary cause remains the previously reported backend failure. Do
    // not replace it with a Tool exit, guest errno, or successful cleanup.
    eprintln!("{}", format_task_termination_failure(tid, error));
    exit_failed_tracer_process(TASK_TERMINATION_FAILURE_EXIT_CODE)
}

const CHILD_CUSTODY_FAILURE_EXIT_CODE: i32 = 103;

fn format_child_custody_failure(creator: Pid, child: Pid, phase: &str, error: &str) -> String {
    let error: String = error
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    format!(
        "HERMIT_CHILD_CUSTODY_FAILED creator={creator} child={child} exit={} phase={phase} error={error} cleanup=unconfirmed",
        CHILD_CUSTODY_FAILURE_EXIT_CODE,
    )
}

fn child_custody_is_fatal(creator: Pid, child: Pid, phase: &str, error: &str) -> ! {
    // A missing native outcome is not a Tool terminal event. Keep this failed
    // run distinct and let EXITKILL plus the outside original-identity owner
    // terminate and prove drain; returning would abandon the newborn actor.
    eprintln!(
        "{}",
        format_child_custody_failure(creator, child, phase, error)
    );
    exit_failed_tracer_process(CHILD_CUSTODY_FAILURE_EXIT_CODE)
}

fn exit_failed_tracer_process(status: i32) -> ! {
    let _ = std::io::stderr().flush();
    let _ = std::io::stdout().flush();
    std::process::exit(status)
}

/// A regular wait issued by this actor, with its original ordinary nonleader
/// owner when applicable. Newborn admission and terminal cleanup keep their
/// separate raw Running owners.
pub(crate) struct TaskRunning {
    running: Running,
    original_nonleader: Option<Arc<TerminalCleanup>>,
    cohort_operation: Option<source_cohort::ResumeOperation>,
    #[cfg(target_arch = "x86_64")]
    observation: Option<source_observation::Context>,
    #[cfg(target_arch = "x86_64")]
    administrative: bool,
    #[cfg(target_arch = "x86_64")]
    precise: Option<source_observation::Step>,
}

fn vanished_without_status(terminal: &TerminalCleanup) -> bool {
    terminal.registration_error().is_none()
        && terminal.observed_exit_status() == Err(Errno::ECHILD)
        && terminal.is_reaped() == Ok(true)
}

/// Both wait classes must make this decision after their own result arrives:
/// select_biased's earlier exit poll is not atomic with notifier publication.
/// Only an original wait may call this; Tool errors never pass through it.
async fn arbitrate_original_wait<T>(
    result: Result<T, TraceError>,
    vanished_nonleader: impl FnOnce() -> bool,
) -> Result<T, TraceError> {
    if matches!(&result, Err(TraceError::Errno(Errno::ECHILD))) && vanished_nonleader() {
        // No status, resume, or state transfer is invented. The existing outer
        // owner can still select cancellation or an authenticated leader Exec.
        return future::pending().await;
    }
    result
}

/// A refused exit wait publishes failure before waiting for a cleanup retry.
/// Once resumed, rejoin the failed-cleanup arm without another ordinary wait
/// or an exec-only park outside the cancellation select. None retains the
/// existing failure; it is not a terminal status or a transfer authorization.
async fn ordinary_exit_or_failure<T>(
    result: Result<T, TraceError>,
    has_terminal_status: bool,
    session: &FatalSession,
) -> Option<Result<T, TraceError>> {
    match result {
        Err(TraceError::Errno(error)) if !has_terminal_status => {
            session.retry_after(error.into()).await;
            None
        }
        result => Some(result),
    }
}

impl TaskRunning {
    #[cfg(target_arch = "x86_64")]
    async fn drive(
        mut self,
        stepping: bool,
    ) -> Result<source_observation::StepOutcome, TraceError> {
        loop {
            let result = self.running.next_state().await;
            if let Some(operation) = self.cohort_operation.take() {
                operation.observe(&result);
            }
            let wait = arbitrate_original_wait(result, || {
                self.original_nonleader
                    .as_ref()
                    .is_some_and(|terminal| vanished_without_status(terminal))
            })
            .await?;
            let owned_exit = match &self.observation {
                Some(context) => context.observe(&wait, self.administrative)?,
                None => None,
            };
            if self.administrative
                && let Wait::Stopped(task, Event::Syscall) = &wait
            {
                let info = task.syscall_stop_info()?;
                if let Some(step) = &mut self.precise
                    && let safeptrace::SyscallStopInfo::Entry(entry) = info
                {
                    step.entry(task, entry)?;
                }
                if !stepping || !matches!(info, safeptrace::SyscallStopInfo::Exit { .. }) {
                    let Wait::Stopped(task, _) = wait else {
                        unreachable!()
                    };
                    let context = self.observation.as_ref().ok_or(Errno::EPROTO)?;
                    let (running, operation) = context.resume(task, None)?;
                    self.running = running;
                    self.cohort_operation = operation;
                    continue;
                }
            }
            let transfer = if let Wait::Stopped(task, Event::Seccomp) = &wait
                && let (Some(context), Some(step)) = (&self.observation, self.precise.take())
            {
                Some(context.transfer_tool(step, task)?)
            } else {
                None
            };
            let (completion, guest_event) = if transfer.is_some() {
                (None, true)
            } else if let Some(step) = &mut self.precise {
                step.finish(&wait, owned_exit)?
            } else if stepping {
                // Legacy non-source stepping still requires a real debug cause;
                // a user-generated SIGTRAP is not a completion receipt.
                let receipt = if let Wait::Stopped(task, Event::Signal(Signal::SIGTRAP)) = &wait {
                    source_observation::Completion::legacy(task)?
                } else {
                    None
                };
                let guest_event = receipt.is_none();
                (receipt, guest_event)
            } else {
                (None, true)
            };
            if completion.is_none()
                && matches!(&wait, Wait::Stopped(..))
                && let (Some(context), Some(step)) = (&self.observation, self.precise.take())
            {
                context.retain_interrupted_step(step)?;
            }
            return Ok(source_observation::StepOutcome {
                wait,
                completion,
                guest_event,
                transfer,
            });
        }
    }
    pub(crate) async fn next_state(self) -> Result<Wait, TraceError> {
        #[cfg(target_arch = "x86_64")]
        {
            Ok(self.drive(false).await?.wait)
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let result = self.running.next_state().await;
            if let Some(operation) = self.cohort_operation {
                operation.observe(&result);
            }
            arbitrate_original_wait(result, || {
                self.original_nonleader
                    .as_ref()
                    .is_some_and(|terminal| vanished_without_status(terminal))
            })
            .await
        }
    }
    #[cfg(target_arch = "x86_64")]
    pub(crate) async fn next_step(self) -> Result<source_observation::StepOutcome, TraceError> {
        self.drive(true).await
    }
    async fn wait_for_signal(mut self, signal: Signal) -> Result<Wait, TraceError> {
        loop {
            let original_nonleader = self.original_nonleader.clone();
            #[cfg(target_arch = "x86_64")]
            let observation = self.observation.clone();
            let wait = self.next_state().await?;
            match wait {
                Wait::Stopped(task, event) if event != Event::Signal(signal) => {
                    let deliver = if let Event::Signal(signal) = event {
                        Some(signal)
                    } else {
                        None
                    };
                    #[cfg(target_arch = "x86_64")]
                    if let Some(context) = observation {
                        let (running, cohort_operation) = context.resume(task, deliver)?;
                        self = Self {
                            running,
                            original_nonleader,
                            cohort_operation,
                            observation: Some(context),
                            administrative: true,
                            precise: None,
                        };
                        continue;
                    }
                    self = Self {
                        running: task.resume(deliver)?,
                        original_nonleader,
                        cohort_operation: None,
                        #[cfg(target_arch = "x86_64")]
                        observation: None,
                        #[cfg(target_arch = "x86_64")]
                        administrative: false,
                        #[cfg(target_arch = "x86_64")]
                        precise: None,
                    };
                }
                wait => return Ok(wait),
            }
        }
    }
}

#[cfg(test)]
mod original_wait_arbitration_tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn publication_between_exit_and_regular_polls_remains_pending() {
        // Model only publication timing; native 572 remains the kernel oracle.
        for publish_before_exit in [false, true] {
            let published = Cell::new(publish_before_exit);
            let exit = async {
                futures::future::poll_fn(|cx| {
                    if published.get() {
                        std::task::Poll::Ready(Err::<(), _>(TraceError::Errno(Errno::ECHILD)))
                    } else {
                        published.set(true);
                        cx.waker().wake_by_ref();
                        std::task::Poll::Pending
                    }
                })
                .await
            };
            let exit = async { arbitrate_original_wait(exit.await, || true).await }.fuse();
            let regular = async {
                assert!(published.get());
                arbitrate_original_wait(Err::<(), _>(TraceError::Errno(Errno::ECHILD)), || true)
                    .await
            }
            .fuse();
            let drive = async {
                futures::pin_mut!(exit, regular);
                futures::select_biased! {
                    result = exit => result,
                    result = regular => result,
                }
            };
            assert!(drive.now_or_never().is_none());
        }
    }

    #[test]
    fn unrelated_wait_errors_and_unproven_echild_are_preserved() {
        for errno in [
            Errno::ECHILD,
            Errno::EIO,
            Errno::EPERM,
            Errno::ESRCH,
            Errno::EALREADY,
        ] {
            for vanished in [false, true] {
                if errno == Errno::ECHILD && vanished {
                    continue;
                }
                let result =
                    arbitrate_original_wait(Err::<(), _>(TraceError::Errno(errno)), || vanished)
                        .now_or_never()
                        .expect("unrelated failure was swallowed");
                assert!(matches!(result, Err(TraceError::Errno(actual)) if actual == errno));
            }
        }
        assert_eq!(
            arbitrate_original_wait(Ok(73), || panic!("success queried disappearance"))
                .now_or_never()
                .unwrap()
                .unwrap(),
            73
        );
    }

    #[test]
    fn arbitrary_callback_failure_keeps_its_typed_cause() {
        #[derive(Debug, thiserror::Error)]
        #[error("callback sentinel, not a native wait")]
        struct CallbackFailure;

        let parked =
            arbitrate_original_wait(Err::<(), _>(TraceError::Errno(Errno::ECHILD)), || true);
        let callback =
            handle_internal_error(Error::External(anyhow::Error::new(CallbackFailure).into()));
        let result = future::select(Box::pin(parked), Box::pin(callback))
            .now_or_never()
            .expect("callback failure must remain ready");
        let Either::Right((Err(reverie::Error::Tool(error)), _)) = result else {
            panic!("callback failure lost its error route");
        };
        assert!(error.downcast_ref::<CallbackFailure>().is_some());
    }

    #[test]
    fn refused_exit_wait_resumes_into_failed_cleanup() {
        use std::future::Future;
        use std::task::Context;
        use std::task::Poll;

        // Exercise the actual failure/retry handshake without any native task.
        // In particular, no second ordinary ECHILD wait may be polled after
        // the retry: None must select the existing failed-cleanup arm instead.
        for error in [Errno::EIO, Errno::EALREADY] {
            let session = FatalSession {
                root: Some(Pid::from_raw(42)),
                reporter: Some(Box::new(|_| true)),
                ..FatalSession::default()
            };
            let mut retry = Box::pin(ordinary_exit_or_failure(
                Err::<(), _>(TraceError::Errno(error)),
                false,
                &session,
            ));
            let mut cx = Context::from_waker(futures::task::noop_waker_ref());
            assert!(retry.as_mut().poll(&mut cx).is_pending());
            assert!(session.is_failed());
            assert!(session.cleanup_was_refused());
            session.resume_cleanup();
            assert!(matches!(retry.as_mut().poll(&mut cx), Poll::Ready(None)));
            assert!(session.is_failed());
            let failure = session.failure_snapshot().expect("original cause retained");
            assert!(matches!(failure.primary(), reverie::Error::Errno(actual) if *actual == error));

            let actual_terminal = ordinary_exit_or_failure(
                Err::<(), _>(TraceError::Errno(Errno::ECHILD)),
                true,
                &session,
            )
            .now_or_never()
            .expect("retained terminal status still reaches the terminal owner");
            assert!(matches!(
                actual_terminal,
                Some(Err(TraceError::Errno(Errno::ECHILD)))
            ));
        }
    }
}

fn log_guest_exit(tid: Pid, pid: Pid, exit_status: ExitStatus) {
    if let ExitStatus::Signaled(signal, core_dumped) = exit_status {
        tracing::error!(
            target: "reverie_ptrace::lifecycle",
            %tid,
            %pid,
            %signal,
            core_dumped,
            "guest terminated by signal"
        );
    }
}

/// Handles a potentially internal error, converting it to an exit status.
async fn handle_internal_error(err: Error) -> Result<ExitStatus, reverie::Error> {
    #[cfg(test)]
    crate::tracer::record_fatal_phase_for_test(|| {
        format!("handle_internal_error entered: {err:?}")
    });
    match err {
        Error::Internal(TraceError::Died(zombie))
        | Error::Tracee {
            source: TraceError::Died(zombie),
            ..
        } => zombie
            .reap()
            .await
            .map_err(|error| anyhow::anyhow!("failed to reap dead tracee: {error}").into()),
        Error::Internal(TraceError::Errno(errno)) => Err(errno.into()),
        Error::Tracee {
            operation,
            pid,
            source: TraceError::Errno(errno),
        } => Err(anyhow::anyhow!("{operation} failed for tracee {pid}: {errno}").into()),
        Error::Runtime {
            operation,
            pid,
            message,
        } => Err(anyhow::anyhow!("{operation} failed for tracee {pid}: {message}").into()),
        Error::External(err) => Err(err),
        Error::RunFailed => future::pending().await,
    }
}

// A cleanup failure must not replace the operation that first failed. Keep
// the secondary diagnostic visible without turning either into a guest result.
fn preserve_run_error(original: Option<reverie::Error>, cleanup: reverie::Error) -> reverie::Error {
    if let Some(original) = original {
        tracing::error!(%original, %cleanup, "owned cleanup also failed after a run-loop error");
        original
    } else {
        cleanup
    }
}

// Publish the typed first cause and the Tool's failure fence before waking
// dependent tasks. Keep this ordering identical to native parent failure.
fn publish_run_error<G: GlobalTool>(
    failure: &ParentCompletionFailure,
    global: &G,
    pid: Pid,
    tid: Pid,
    phase: &'static str,
    error: reverie::Error,
) {
    failure.record_run_error(tid, error);
    global.report_backend_failure(reverie::BackendFailure { pid, tid, phase });
    failure.changed.notify_waiters();
}

// Retain the existing terminal observation, including a final wait already
// consumed by the failing run-loop. Only an unobserved task needs termination
// and the original ExitFuture; never mint another numeric wait owner.
async fn wait_for_failed_run_terminal(
    cleanup: &TerminalCleanup,
    tid: Pid,
    exit_event: impl Future<Output = Result<Stopped, TraceError>>,
) -> Either<Result<Stopped, TraceError>, Result<ExitStatus, reverie::Error>> {
    match cleanup.observed_terminal() {
        Some(Ok(status)) => Either::Right(Ok(status)),
        Some(Err(error)) => Either::Left(Err(error)),
        None => {
            if let Err(error) = check_failed_task_termination(cleanup.terminate_bound_task()) {
                failed_task_termination_is_fatal(tid, error);
            }
            Either::Left(exit_event.await)
        }
    }
}

/// Helper for canceling handlers.
async fn cancellable<F>(cancel_handler: Arc<AtomicBool>, f: F) -> Option<F::Output>
where
    F: Future,
{
    futures::pin_mut!(f);
    future::poll_fn(|cx| {
        let result = f.as_mut().poll(cx);

        // `tail_inject` sets this while polling `f`, then remains pending. We
        // can cancel the handler in the same poll instead of waking the Tokio
        // task solely to make this future observe its own notification.
        if cancel_handler.swap(false, Ordering::SeqCst) {
            Poll::Ready(None)
        } else {
            result.map(Some)
        }
    })
    .await
}

#[cfg(target_arch = "x86_64")]
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum SegfaultTrapInfo {
    Cpuid,
    Rdtscs(Rdtsc),
}

#[cfg(target_arch = "x86_64")]
impl SegfaultTrapInfo {
    /// Check if segfault is called by cpuid/rdtsc trap
    pub fn decode_segfault(insn_at_rip: u64) -> Option<SegfaultTrapInfo> {
        if insn_at_rip & 0xffffu64 == 0xa20fu64 {
            Some(SegfaultTrapInfo::Cpuid)
        } else if insn_at_rip & 0xffffu64 == 0x310fu64 {
            Some(SegfaultTrapInfo::Rdtscs(Rdtsc::Tsc))
        } else if insn_at_rip & 0xffffffu64 == 0xf9010fu64 {
            Some(SegfaultTrapInfo::Rdtscs(Rdtsc::Tscp))
        } else {
            None
        }
    }
}

// restore syscall context when it returns. This is needed because we might
// have injected a different syscall (or arguments) in handle_seccomp.
fn restore_context(
    task: &Stopped,
    context: libc::user_regs_struct,
    retval: Option<Reg>,
    restore_stack: bool,
) -> Result<(), TraceError> {
    let regs = restored_context_registers(task.getregs()?, context, retval, restore_stack);
    task.setregs(&regs)
}

fn restored_context_registers(
    mut regs: libc::user_regs_struct,
    context: libc::user_regs_struct,
    retval: Option<Reg>,
    restore_stack: bool,
) -> libc::user_regs_struct {
    if let Some(ret) = retval {
        *regs.ret_mut() = ret;
    }
    // TODO-HUMAN-REVIEW(PR-103): Review injected parent-stack restoration.
    if restore_stack {
        *regs.stack_ptr_mut() = context.stack_ptr();
    }

    // Restore instruction pointer.
    *regs.ip_mut() = context.ip();

    // Restore syscall arguments.
    regs.set_args(context.args());

    // This is needed when syscall is interrupted by a signal (ERESTARTSYS)
    // we need restore the original syscall number as well because it is
    // possible syscall is reinjected as a different variant, like vfork ->
    // clone, which accepts different arguments.
    *regs.orig_syscall_mut() = context.orig_syscall();

    // The `syscall` instruction clobbers %rcx/%r11. When we injected a syscall
    // (or a different syscall variant) from the private trampoline page, %rcx
    // and %r11 now hold the *trampoline's* return RIP / RFLAGS rather than the
    // guest's. Although the ABI leaves these "undefined" after a syscall, an
    // injection should be transparent, and leaving Reverie's private trampoline
    // address in %rcx would leak a tracer-internal (and potentially
    // nondeterministic) pointer to the guest. Restore them from the guest's own
    // pre-syscall snapshot. (No-op on aarch64.)
    regs.restore_syscall_clobbers(&context);
    regs
}

impl<L: Tool + 'static> TracedTask<L> {
    #[cfg(target_arch = "x86_64")]
    async fn cpuid_state(&mut self) -> Result<i64, Errno> {
        use reverie::syscalls::ArchPrctl;
        use reverie::syscalls::ArchPrctlCmd;

        self.inject_backend_with_retry(
            ArchPrctl::new().with_cmd(ArchPrctlCmd::ARCH_GET_CPUID(None)),
        )
        .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn intercept_cpuid(&mut self) -> Result<(), Errno> {
        use reverie::syscalls::ArchPrctl;
        use reverie::syscalls::ArchPrctlCmd;

        self.inject_backend_with_retry(ArchPrctl::new().with_cmd(ArchPrctlCmd::ARCH_SET_CPUID(0)))
            .await
            .map(|_| ())
    }

    /// Perform the very first setup of a fresh tracee process:
    ///
    /// (1) Set up the special reverie/guest shared page in the tracee.
    ///
    /// (2) Also disables vdso within the guest
    ///
    /// Warning: this function MUTATES guest code to accomplish the modifications, even though this
    /// mutation is undone before it returns.  As a result, it  has an extra precondition.
    ///
    /// Precondition: all threads in the guest process are stopped. Otherwise a guest state may be
    /// executing the instructions that are mutated and may crash (due to problems with incoherent
    /// instruction fetch resulting in non-atomic writes to instructions that straddle cache line
    /// boundaries).
    ///
    /// Precondition: the caller is entitled to execute (blocking, destructive) waitpids against the
    /// target tracee.  This must not race with concurrent asynchronous tasks operating on the same
    /// TID.
    ///
    /// Postcondition: the guest registers and code memory are restored to their original state,
    /// including RIP, but the vdso page and special shared page are modified accordingly.
    #[tracing::instrument(
        target = "reverie_ptrace::lifecycle",
        name = "tracee.initialize",
        level = "debug",
        skip_all,
        fields(pid = %task.pid())
    )]
    pub async fn tracee_preinit(&mut self, task: Stopped) -> Result<Stopped, TraceError> {
        // A forked child can initialize a replacement image too. It must not
        // consume or overwrite the session root's held-stop cleanup lease.
        let held_root_stop = self.liteinst_root_stop_slot(&task);
        let reject_activation_signals = self.global_state.liteinst_runtime.is_some();
        let unexpected_preinit_signal = Arc::new(StdMutex::new(None));
        #[cfg(test)]
        let pause_preinit_step = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.pause_preinit_step.clone());
        #[cfg(test)]
        let force_preinit_signal_once = (self.liteinst_runtime.lock().unwrap().phase
            == LiteinstRuntimePhase::Waiting)
            .then(|| {
                self.global_state
                    .liteinst_runtime
                    .as_ref()
                    .and_then(|runtime| runtime.force_preinit_signal_once.clone())
            })
            .flatten();

        fn arm_preinit_stop(
            held_root_stop: &Option<Arc<StdMutex<Option<HeldRootStop>>>>,
            task: &Stopped,
            event: &Event,
        ) {
            if let Some(slot) = held_root_stop {
                let previous = slot
                    .lock()
                    .unwrap()
                    .replace(HeldRootStop::from_event(task, event));
                debug_assert!(
                    previous.is_none(),
                    "rearmed an undisarmed preinit stop lease"
                );
            }
        }

        #[cfg(test)]
        async fn pause_preinit(
            pause: &Option<(usize, mpsc::UnboundedSender<Pid>)>,
            step: usize,
            task: &Stopped,
        ) {
            if let Some((target, sender)) = pause
                && *target == step
            {
                let _ = sender.send(task.pid());
                future::pending::<()>().await;
            }
        }

        #[cfg(test)]
        if self
            .global_state
            .liteinst_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.fail_preinit)
        {
            return Err(Errno::EPERM.into());
        }

        type SavedInstructions = [u8; 8];

        /// Helper function for tracee_preinit that does the core work.
        async fn setup_special_mmap_page(
            task: Stopped,
            saved_regs: &libc::user_regs_struct,
            held_root_stop: &Option<Arc<StdMutex<Option<HeldRootStop>>>>,
            reject_activation_signals: bool,
            unexpected_signal: &Arc<StdMutex<Option<Signal>>>,
            #[cfg(test)] pause_preinit_step: &Option<(usize, mpsc::UnboundedSender<Pid>)>,
            #[cfg(test)] force_preinit_signal_once: &Option<Arc<AtomicBool>>,
        ) -> Result<Stopped, TraceError> {
            // NOTE: This point in the code assumes that a specific instruction
            // sequence "SYSCALL; INT3", has been patched into the guest, and
            // that RIP points to the syscall.
            let mut regs = *saved_regs;

            let page_addr = cp::PRIVATE_PAGE_OFFSET;

            *regs.syscall_mut() = Sysno::mmap as Reg;
            *regs.orig_syscall_mut() = regs.syscall();
            regs.set_args((
                page_addr as Reg,
                cp::PRIVATE_PAGE_SIZE as Reg,
                (libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) as Reg,
                (libc::MAP_PRIVATE | libc::MAP_FIXED | libc::MAP_ANONYMOUS) as Reg,
                -1i64 as Reg,
                0,
            ));

            task.setregs(&regs)?;
            // Execute the injected mmap call.
            let mut running = RootStopLease::new(task, held_root_stop.clone()).step(None)?;

            // loop until second breakpoint hit after injected syscall.
            #[cfg(test)]
            let mut step = 0;
            let task = loop {
                let (task, event) = running.next_state().await?.assume_stopped();
                arm_preinit_stop(held_root_stop, &task, &event);
                #[cfg(test)]
                let forced_external_sigtrap = event == Event::Signal(Signal::SIGTRAP)
                    && force_preinit_signal_once
                        .as_ref()
                        .is_some_and(|force_once| force_once.load(Ordering::SeqCst));
                #[cfg(not(test))]
                let forced_external_sigtrap = false;
                #[cfg(test)]
                if let Some((target, sender)) = pause_preinit_step
                    && *target == step
                {
                    let _ = sender.send(task.pid());
                    future::pending::<()>().await;
                }
                #[cfg(test)]
                {
                    step += 1;
                }
                match event {
                    Event::Signal(Signal::SIGTRAP) => {
                        let expected_rip = saved_regs
                            .ip()
                            .checked_add(cp::SYSCALL_INSTR_SIZE as u64)
                            .ok_or(Errno::EOVERFLOW)?;
                        if reject_activation_signals
                            && !is_expected_breakpoint_trap(
                                &task,
                                expected_rip,
                                forced_external_sigtrap,
                            )?
                        {
                            #[cfg(test)]
                            if forced_external_sigtrap
                                && let Some(force_once) = force_preinit_signal_once.as_ref()
                            {
                                force_once.store(false, Ordering::SeqCst);
                            }
                            *unexpected_signal.lock().unwrap() = Some(Signal::SIGTRAP);
                            return Err(Errno::EPROTO.into());
                        }
                        break task;
                    }
                    Event::Signal(sig) => {
                        if reject_activation_signals {
                            *unexpected_signal.lock().unwrap() = Some(sig);
                            return Err(Errno::EPROTO.into());
                        }
                        // We can catch spurious signals here, such as SIGWINCH.
                        // All we can do is skip over them.
                        tracing::debug!(
                            "[{}] Skipping {:?} during initialization",
                            task.pid(),
                            event
                        );
                        running = RootStopLease::new(task, held_root_stop.clone()).resume(sig)?;
                    }
                    Event::Seccomp => {
                        // Injected mmap trapped. We may not necessarily
                        // intercept a seccomp event here if the tool hasn't
                        // subscribed to the mmap syscall.
                        running = RootStopLease::new(task, held_root_stop.clone()).resume(None)?;
                    }
                    unknown => {
                        panic!("task {} returned unknown event {:?}", task.pid(), unknown);
                    }
                }
            };

            // Make sure we got our desired address.
            assert_eq!(
                Errno::from_ret(task.getregs()?.ret() as usize)?,
                page_addr,
                "Could not mmap address {}",
                page_addr
            );

            cp::populate_mmap_page(task.pid().into(), page_addr)?;

            // Restore our saved registers, including our instruction pointer.
            task.setregs(saved_regs)?;
            Ok(task)
        }

        /// Put the guest into the weird state where it has an
        /// "INT3;SYSCALL;INT3" patched into the code wherever RIP happens to be
        /// pointing. It leaves RIP pointing at the syscall instruction. This
        /// allows forcible injection of syscalls into the guest.
        async fn establish_injection_state(
            mut task: Stopped,
        ) -> Result<(Stopped, libc::user_regs_struct, SavedInstructions), TraceError> {
            #[cfg(target_arch = "x86_64")]
            const SYSCALL_BP: SavedInstructions = [
                0x0f, 0x05, // syscall
                0xcc, // int3
                0xcc, 0xcc, 0xcc, 0xcc, 0xcc, // padding
            ];

            #[cfg(target_arch = "aarch64")]
            const SYSCALL_BP: SavedInstructions = [
                0x01, 0x00, 0x00, 0xd4, // svc 0
                0x20, 0x00, 0x20, 0xd4, // brk 1
            ];

            // Save the original registers so we can restore them later.
            let regs = task.getregs()?;

            // Saved instruction memory
            let ip = AddrMut::from_raw(regs.ip() as usize).ok_or(Errno::EFAULT)?;
            let saved: SavedInstructions = task.read_value(ip)?;

            // Patch the tracee at the current instruction pointer.
            //
            // NOTE: `process_vm_writev` cannot write to write-protected pages,
            // but `PTRACE_POKEDATA` can! Thus, we need to make sure we only
            // write one word-sized chunk at a time. Luckily, the instructions
            // we want to inject fit inside of just one 64-bit word.
            task.write_value(ip.cast(), &SYSCALL_BP)?;

            Ok((task, regs, saved))
        }

        /// Undo the effects of `establish_injection_state` and put the program
        /// code memory and instruction pointer back to normal.
        fn remove_injection_state(
            task: &mut Stopped,
            regs: libc::user_regs_struct,
            saved: SavedInstructions,
        ) -> Result<(), TraceError> {
            // NOTE: Again, because `process_vm_writev` cannot write to
            // write-protected pages, we must write in word-sized chunks with
            // PTRACE_POKEDATA.
            let ip = AddrMut::from_raw(regs.ip() as usize).ok_or(Errno::EFAULT)?;
            task.write_value(ip, &saved)?;
            task.setregs(&regs)?;
            Ok(())
        }

        let (task, regs, prev_state) = establish_injection_state(task).await?;
        let task = setup_special_mmap_page(
            task,
            &regs,
            &held_root_stop,
            reject_activation_signals,
            &unexpected_preinit_signal,
            #[cfg(test)]
            &pause_preinit_step,
            #[cfg(test)]
            &force_preinit_signal_once,
        )
        .await;
        if let Some(sig) = unexpected_preinit_signal.lock().unwrap().take() {
            self.record_liteinst_failure(
                LiteinstActivationFailureReason::UnexpectedPreinitSignal,
                Error::runtime(
                    self.tid(),
                    "reject unexpected LiteInst activation signal",
                    format!(
                        "received {sig} before the required preload handshake completed: tracee pre-initialization observed an unexpected nested signal"
                    ),
                ),
            );
        }
        let mut task = task?;
        #[cfg(test)]
        pause_preinit(&pause_preinit_step, 1, &task).await;

        // Restore registers after adding our temporary injection state.
        remove_injection_state(&mut task, regs, prev_state)?;

        if vdso::is_patch_required(&self.global_state.subscriptions) {
            let subscriptions = self.global_state.subscriptions.clone();
            vdso::vdso_patch(self, &subscriptions)
                .await
                .expect("unable to patch vdso");
        }
        #[cfg(test)]
        pause_preinit(&pause_preinit_step, 2, &task).await;

        // Protect our trampoline page from being written to. We won't need to
        // change this again for the lifetime of the guest process.
        self.inject_backend_with_retry(
            Mprotect::new()
                .with_addr(AddrMut::from_raw(cp::TRAMPOLINE_BASE))
                .with_len(cp::TRAMPOLINE_SIZE)
                .with_protection(ProtFlags::PROT_READ | ProtFlags::PROT_EXEC),
        )
        .await?;
        #[cfg(test)]
        pause_preinit(&pause_preinit_step, 3, &task).await;

        // Try to intercept cpuid instructions on x86_64
        #[cfg(target_arch = "x86_64")]
        if self.global_state.subscriptions.has_cpuid() {
            self.has_cpuid_interception = match self.cpuid_state().await {
                Ok(initial_state @ (0 | 1)) => match self.intercept_cpuid().await {
                    Ok(()) => match self.cpuid_state().await {
                        Ok(0) => true,
                        Ok(state) => {
                            tracing::error!(
                                state,
                                "ARCH_SET_CPUID succeeded but ARCH_GET_CPUID did not report the disabled state; continuing without CPUID interception"
                            );
                            false
                        }
                        Err(err) => {
                            tracing::error!(
                                "Unable to verify ARCH_SET_CPUID with ARCH_GET_CPUID: {}; continuing without CPUID interception",
                                err
                            );
                            false
                        }
                    },
                    Err(Errno::ENODEV) => {
                        tracing::error!(
                            initial_state,
                            "ARCH_GET_CPUID reported a valid state, but ARCH_SET_CPUID returned ENODEV. The kernel exposes CPUID state without hardware faulting support. On AMD hosts, use Linux 6.17+ upstream or a kernel with CPUID faulting backported; continuing without CPUID interception"
                        );
                        false
                    }
                    Err(err) => {
                        tracing::error!(
                            "Unable to disable CPUID after ARCH_GET_CPUID reported a valid state: {}; continuing without CPUID interception",
                            err
                        );
                        false
                    }
                },
                Ok(state) => {
                    tracing::error!(
                        state,
                        "ARCH_GET_CPUID returned an unexpected state; continuing without CPUID interception"
                    );
                    false
                }
                Err(Errno::ENODEV) => {
                    tracing::error!(
                        "CPUID faulting is unavailable: arch_prctl(ARCH_GET_CPUID) returned ENODEV. On AMD hosts, use Linux 6.17+ upstream or a kernel with CPUID faulting backported; continuing without CPUID interception"
                    );
                    false
                }
                Err(err) => {
                    tracing::error!(
                        "Unable to query CPUID faulting with arch_prctl(ARCH_GET_CPUID): {}; continuing without CPUID interception",
                        err
                    );
                    false
                }
            };
        }
        #[cfg(test)]
        pause_preinit(&pause_preinit_step, 4, &task).await;

        // Restore registers again after we've injected syscalls so that we
        // don't leave the return value register (%rax) in a dirty state.
        task.setregs(&regs)?;

        Ok(task)
    }

    #[cfg(target_arch = "x86_64")]
    async fn handle_cpuid(
        &mut self,
        mut regs: libc::user_regs_struct,
    ) -> Result<libc::user_regs_struct, TraceError> {
        let eax = regs.rax as u32;
        let ecx = regs.rcx as u32;
        let result = self
            .process_state
            .clone()
            .handle_cpuid_event(self, eax, ecx)
            .await;
        let cpuid = self
            .ordinary_callback_errno("ptrace cpuid callback", result)
            .await?;
        regs.rax = cpuid.eax as u64;
        regs.rbx = cpuid.ebx as u64;
        regs.rcx = cpuid.ecx as u64;
        regs.rdx = cpuid.edx as u64;
        regs.rip += 2;
        self.ordinary_trace_continuation()?;
        self.timer.finalize_requests();
        Ok(regs)
    }

    #[cfg(target_arch = "x86_64")]
    async fn handle_rdtscs(
        &mut self,
        mut regs: libc::user_regs_struct,
        request: Rdtsc,
    ) -> Result<libc::user_regs_struct, TraceError> {
        let result = self
            .process_state
            .clone()
            .handle_rdtsc_event(self, request)
            .await;
        let retval = self
            .ordinary_callback_errno("ptrace rdtsc callback", result)
            .await?;
        regs.rax = retval.tsc & 0xffff_ffffu64;
        regs.rdx = retval.tsc >> 32;
        match request {
            Rdtsc::Tsc => {
                regs.rip += 2;
            }
            Rdtsc::Tscp => {
                regs.rip += 3;
                regs.rcx = retval.aux.unwrap_or(0) as u64;
            }
        }
        self.ordinary_trace_continuation()?;
        self.timer.finalize_requests();
        Ok(regs)
    }

    /// Returns `true` if the signal was actually meant for the timer, and
    /// therefore should not be forwarded to the tool / guest.
    async fn handle_timer(&mut self, task: Stopped) -> Result<(bool, Stopped), TraceError> {
        // Capture the original stopped actor before borrowing the timer. Each
        // successful step keeps this generation; a non-step event leaves the
        // timer loop. Only its native wait may defer vanished-nonleader ECHILD.
        let original_nonleader = self.original_nonleader(task.pid(), || task.terminal_cleanup());
        let armer = self.liteinst_root_stop_armer(&task);
        let held_root_stop = armer
            .as_ref()
            .map(|armer| Arc::clone(&armer.held_root_stop));
        let cohort = self.cohort.clone();
        #[cfg(target_arch = "x86_64")]
        let observation = self.source_context(Some(&task));
        let mut step = move |task: Stopped| {
            #[cfg(target_arch = "x86_64")]
            if let Some(context) = observation.clone()
                && let Some(precise) = context.begin_step(&task)?
            {
                let (running, cohort_operation) = context.resume(task, None)?;
                return Ok(TaskRunning {
                    running,
                    original_nonleader: original_nonleader.clone(),
                    cohort_operation,
                    observation: Some(context),
                    administrative: true,
                    precise: Some(precise),
                });
            }
            let cohort_operation = cohort
                .as_ref()
                .and_then(|member| member.before_resume(&task));
            RootStopLease::new(task, held_root_stop.clone())
                .step(None)
                .map(|running| TaskRunning {
                    running,
                    original_nonleader: original_nonleader.clone(),
                    cohort_operation,
                    #[cfg(target_arch = "x86_64")]
                    observation: None,
                    #[cfg(target_arch = "x86_64")]
                    administrative: false,
                    #[cfg(target_arch = "x86_64")]
                    precise: None,
                })
        };
        let mut observe = |wait: &Wait| {
            if let (Some(armer), Wait::Stopped(task, event)) = (armer.as_ref(), wait) {
                armer.ensure(task, event)?;
            }
            Ok(())
        };
        let task = match self
            .timer
            .handle_signal(task, &mut step, &mut observe)
            .await
        {
            Err(HandleFailure::ImproperSignal(task)) => return Ok((false, task)),
            Err(HandleFailure::Cancelled(task)) => return Ok((true, task)),
            Err(HandleFailure::TraceError(e)) => {
                #[cfg(test)]
                crate::tracer::record_fatal_phase_for_test(|| {
                    format!("handle_timer TraceError: {e:?}")
                });
                if self.ordinary_failure_enabled()
                    && let TraceError::Errno(errno) = &e
                {
                    // Keep the actual timer/query errno before the generic
                    // signal-delivery context projects it into a message.
                    // The existing task owner still owns physical cleanup.
                    self.publish_ordinary_failure("ptrace timer signal", (*errno).into());
                }
                return Err(e);
            }
            Err(HandleFailure::Event(wait)) => self.abort(Ok(wait)).await,
            Ok(task) => task,
        };
        #[cfg(test)]
        if let Some(sender) = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.pause_precise_timer_step.as_ref())
        {
            let _ = sender.send(task.pid());
            future::pending::<()>().await;
        }
        self.process_state.clone().handle_timer_event(self).await;
        self.ordinary_trace_continuation()?;
        self.timer.finalize_requests();
        Ok((true, task))
    }

    /// Handle a state change in the guest, and leave it in a stopped state.
    /// Return the signal that the process would be resumed with, if any.
    ///
    /// Preconditions:
    ///  * running on the ptracer pthread
    ///
    /// Postconditions:
    ///  * guest thread may or may not be stopped, depending on value of GuestNext
    async fn handle_stop_event(&mut self, stopped: Stopped, event: Event) -> Result<Wait, Error> {
        #[cfg(target_arch = "x86_64")]
        if self.cohort.is_some() && matches!(&event, Event::Syscall) {
            if !matches!(
                stopped.syscall_stop_info()?,
                safeptrace::SyscallStopInfo::Exit { .. }
            ) {
                return Err(Errno::EPROTO.into());
            }
            return Ok(self.resume_stopped(stopped, None)?.next_state().await?);
        }
        self.timer.observe_event();
        #[cfg(target_arch = "x86_64")]
        self.source_observer
            .lock()
            .unwrap()
            .retire_interrupted_step(&stopped)?;
        #[cfg(target_arch = "x86_64")]
        if !matches!(&event, Event::Seccomp) {
            self.source_observer
                .lock()
                .unwrap()
                .cancel_tool_for_guest_event(&stopped)?;
        }
        if matches!(&event, Event::NewChild(..)) {
            self.global_state.fatal_session.source_epoch.revoke();
        }
        let tid = self.tid();

        #[cfg(test)]
        if let Some((pause, sender)) = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.pause_root_stop.as_ref())
        {
            let selected = match &event {
                Event::Seccomp => match pause {
                    RootStopPause::Seccomp => true,
                    RootStopPause::Signal(_) => false,
                },
                Event::Signal(actual) => match pause {
                    RootStopPause::Seccomp => false,
                    RootStopPause::Signal(expected) => expected == actual,
                },
                Event::NewChild(..)
                | Event::Exec(_)
                | Event::VforkDone
                | Event::Exit
                | Event::Stop
                | Event::Syscall => false,
            };
            if selected {
                let _ = sender.send(stopped.pid());
                future::pending::<()>().await;
            }
        }

        self.ordinary_continuation()?;
        match event {
            Event::Signal(sig) => {
                self.handle_signal(stopped, sig)
                    .await
                    .map_err(|error| match error {
                        Error::Internal(source) => Error::Tracee {
                            operation: "handle signal-delivery stop",
                            pid: tid,
                            source,
                        },
                        error => error,
                    })
            }
            Event::Exec(former_tid) => self.handle_exec_event(stopped, former_tid).await,
            Event::Seccomp => self.handle_seccomp(stopped).await,
            Event::NewChild(op, child) => {
                // Capture the original native frame before child callbacks or
                // any context restoration. A failed optional observation must
                // not bypass dispatch's real newborn custody and cleanup.
                #[cfg(target_arch = "x86_64")]
                let parent_step = if !self.attached_by_gdb {
                    match ParentSyscallStep::capture(&stopped, op, child.pid()) {
                        Ok(step) => Some(step),
                        Err(error) => {
                            tracing::debug!(?error, "parent-step provenance unavailable");
                            None
                        }
                    }
                } else {
                    None
                };
                let result = async {
                    let (wait, _) = self
                        .dispatch_new_task(op, stopped, child, None, None, None)
                        .await?;
                    #[cfg(target_arch = "x86_64")]
                    let wait = {
                        let complete = match parent_step {
                            Some(step) => match step.completed(&wait) {
                                Ok(complete) => complete,
                                Err(error) => {
                                    tracing::debug!(?error, "parent-step completion unavailable");
                                    false
                                }
                            },
                            None => false,
                        };
                        if complete {
                            match wait {
                                Wait::Stopped(parent, Event::Signal(Signal::SIGTRAP)) => {
                                    // Only this ordinary caller resumes user execution.
                                    // status_to_result's injected continuation is unchanged.
                                    return self.resume_stopped(parent, None)?.next_state().await;
                                }
                                wait => wait,
                            }
                        } else {
                            wait
                        }
                    };
                    Ok::<_, TraceError>(wait)
                }
                .await;
                result.tracee_context(tid, "handle new tracee stop")
            }
            Event::VforkDone => self
                .handle_vfork_done_event(stopped)
                .await
                .tracee_context(tid, "handle vfork completion stop"),
            task_state => panic!("unknown task state for tracee {}: {:?}", tid, task_state),
        }
    }

    async fn get_stop_tx(&self) -> Option<(Arc<AtomicBool>, mpsc::Sender<(Pid, Suspended)>)> {
        for child in self.child_threads.lock().await.deref_mut().into_iter() {
            if child.id() == self.tid() {
                return Some((child.suspended.clone(), child.wait_all_stop_tx.take()?));
            }
        }
        None
    }

    // TODO-HUMAN-REVIEW(PR-103): Review rewritten rt_sigreturn tail execution.
    async fn resume_injected_rt_sigreturn(
        &mut self,
        task: Stopped,
        frame: &InjectedSyscallFrame,
    ) -> Result<Wait, TraceError> {
        let mut regs = task.getregs()?;
        frame.copy_to_user_regs(&mut regs);
        *regs.syscall_mut() = Sysno::rt_sigreturn as Reg;
        *regs.orig_syscall_mut() = Sysno::rt_sigreturn as Reg;

        // rt_sigreturn consumes the signal frame at the original guest stack
        // pointer and does not return to its caller. Run it from Reverie's
        // seccomp-allowed private page, then follow the restored guest state.
        *regs.ip_mut() = cp::PRIVATE_PAGE_OFFSET as Reg;
        task.setregs(&regs)?;
        self.resume_stopped(task, None)?.next_state().await
    }

    // TODO-HUMAN-REVIEW(PR-102): Review rewritten-syscall dispatch and result handling.
    async fn handle_injected_syscall(
        &mut self,
        task: Stopped,
        frame_address: usize,
        trap_rflags: u64,
    ) -> Result<Wait, Error> {
        let mut frame = self.read_injected_syscall_frame(&task, frame_address)?;
        let syscall = frame.syscall();
        let (nr, args) = syscall.into_parts();
        // AUTONOMOUS-BOT-IMPLEMENTED
        if nr == Sysno::rt_sigreturn {
            return Ok(self.resume_injected_rt_sigreturn(task, &frame).await?);
        }

        frame.emulate_syscall_entry(trap_rflags);
        self.write_injected_syscall_frame(&task, frame_address, &frame)?;

        if !self
            .global_state
            .subscriptions
            .iter_syscalls()
            .any(|subscribed| subscribed == nr)
        {
            self.injected_syscall_frame = Some(frame_address);
            let result = self.untraced_syscall(task, nr, args, false).await?;
            let task = self.assume_stopped();
            self.write_injected_syscall_result(&task, result)?;
            self.injected_syscall_frame = None;
            let signal = self.take_pending_signal_for_resume(
                LiteinstActivationOperation::ResumeInjectedSyscall,
            )?;
            return Ok(self.resume_stopped(task, signal)?.next_state().await?);
        }

        let span = tracing::trace_span!(
            target: "reverie_ptrace::syscall",
            "syscall.intercept",
            tid = %self.tid(),
            syscall = %nr,
            args = ?args,
            source = "injected-trap",
        );

        async {
            self.injected_syscall_frame = Some(frame_address);
            self.pending_syscall = Some((nr, args));
            #[cfg(target_arch = "x86_64")]
            {
                self.original_setsockopt_entry = None;
            }
            self.pending_syscall_already_skipped = false;

            let retval = cancellable(self.cancel_handler.clone(), async {
                self.process_state
                    .clone()
                    .handle_syscall_event(self, syscall)
                    .await
            })
            .await;

            let retval = match retval {
                Some(Err(error)) => match error.into_errno() {
                    Ok(errno) => Some(Err(errno)),
                    Err(error) => {
                        // Effects performed by the callback cannot be rolled back.
                        // Publish the original cause and park this exact stop for
                        // its tree owner; do not finalize timers, encode a guest
                        // errno, resume, or enter the legacy child-detach path.
                        self.publish_ordinary_failure("ptrace injected syscall callback", error);
                        return future::pending().await;
                    }
                },
                Some(Ok(value)) => Some(Ok(value)),
                None => None,
            };
            self.ordinary_trace_continuation()?;
            self.timer.finalize_requests();

            if let Some(retval) = retval {
                let result = match retval {
                    Ok(value) => value,
                    Err(errno) => -(errno.into_raw() as i64),
                };
                self.write_injected_syscall_result(&task, Ok(result))?;
            }

            self.pending_syscall = None;

            self.original_read_entry = None;
            self.pending_syscall_already_skipped = false;
            self.injected_syscall_frame = None;
            let signal = self.take_pending_signal_for_resume(
                LiteinstActivationOperation::ResumeInterceptedInjectedSyscall,
            )?;
            let wait = self.resume_stopped(task, signal)?.next_state().await?;
            tracing::trace!(
                target: "reverie_ptrace::syscall",
                "completed injected syscall interception"
            );
            Ok(wait)
        }
        .instrument(span)
        .await
    }

    fn validate_liteinst_handshake(
        &self,
        task: &Stopped,
        frame_address: usize,
        trap_rip: u64,
        ready: bool,
    ) -> Option<LiteinstHandshakeFrame> {
        let config = self.global_state.liteinst_runtime.as_ref()?;
        let address = Addr::from_raw(frame_address)?;
        let frame: LiteinstHandshakeFrame = task.read_value(address).ok()?;
        if frame.version != 4
            || frame.helper_stack_top < 8
            || frame.helper_stack_top & 0xf != 0
            || trap_rip
                != if ready {
                    frame.ready_rip
                } else {
                    frame.begin_rip
                }
        {
            return None;
        }
        let maps = guest_maps(task.pid())?;
        let preload_code = |address| {
            maps.iter().any(|mapping| {
                mapping.executable
                    && mapping.path.as_ref() == Some(&config.preload)
                    && mapping.contains(address)
            })
        };
        if ![
            frame.begin_rip,
            frame.ready_rip,
            frame.install_helper,
            frame.helper_return,
            frame.helper_return_rip,
            frame.syscall_trap_rip,
            frame.syscall_trap_return_rip,
        ]
        .into_iter()
        .all(preload_code)
        {
            return None;
        }
        let frame_readable = maps
            .iter()
            .any(|mapping| mapping.readable && mapping.contains(frame_address as u64));
        let helper_stack_map = maps.iter().find(|mapping| {
            mapping.writable && mapping.contains(frame.helper_stack_top.saturating_sub(8))
        });
        let install_result = GuestRange::new(
            frame.install_result,
            core::mem::size_of::<LiteinstInstallResult>() as u64,
        )?;
        let install_result_writable = maps.iter().any(|mapping| {
            Some((mapping.start, mapping.end))
                == helper_stack_map.map(|stack| (stack.start, stack.end))
                && mapping.readable
                && mapping.writable
                && mapping.contains_range(install_result)
        });
        (frame_readable && helper_stack_map.is_some() && install_result_writable).then_some(frame)
    }

    fn install_liteinst_entry_guard(&mut self, task: &mut Stopped) -> Result<(), TraceError> {
        if self.global_state.liteinst_runtime.is_none() {
            return Ok(());
        }
        if self.liteinst_entry_guard.is_some() {
            return Err(Errno::EALREADY.into());
        }
        let address = guest_auxv_entry(task.pid(), libc::AT_ENTRY).ok_or(Errno::ENOEXEC)?;
        let range =
            GuestRange::new(address, core::mem::size_of::<u64>() as u64).ok_or(Errno::ENOEXEC)?;
        if !guest_maps(task.pid()).is_some_and(|maps| {
            maps.iter().any(|mapping| {
                mapping.readable && mapping.executable && mapping.contains_range(range)
            })
        }) {
            return Err(Errno::ENOEXEC.into());
        }
        let read_address = Addr::<u64>::from_raw(address as usize).ok_or(Errno::EFAULT)?;
        let guard_address = AddrMut::<u64>::from_raw(address as usize).ok_or(Errno::EFAULT)?;
        let saved_instruction: u64 = task.read_value(read_address)?;
        if saved_instruction as u8 == 0xcc {
            return Err(Errno::EPROTO.into());
        }
        let guarded_instruction = (saved_instruction & !0xff) | 0xcc;
        task.write_value(guard_address, &guarded_instruction)?;
        let observed: u64 = task.read_value(read_address)?;
        if observed != guarded_instruction {
            let _ = task.write_value(guard_address, &saved_instruction);
            return Err(Errno::EIO.into());
        }
        self.liteinst_entry_guard = Some(LiteinstEntryGuard {
            address,
            saved_instruction,
        });
        Ok(())
    }

    fn restore_liteinst_entry_guard(&mut self, task: &mut Stopped) -> Result<(), TraceError> {
        let guard = self.liteinst_entry_guard.ok_or(Errno::EPROTO)?;
        let read_address = Addr::<u64>::from_raw(guard.address as usize).ok_or(Errno::EFAULT)?;
        let address = AddrMut::<u64>::from_raw(guard.address as usize).ok_or(Errno::EFAULT)?;
        let guarded_instruction = (guard.saved_instruction & !0xff) | 0xcc;
        let observed: u64 = task.read_value(read_address)?;
        if observed != guarded_instruction {
            return Err(Errno::EPROTO.into());
        }
        task.write_value(address, &guard.saved_instruction)?;
        let restored: u64 = task.read_value(read_address)?;
        if restored != guard.saved_instruction {
            return Err(Errno::EIO.into());
        }
        self.liteinst_entry_guard = None;
        Ok(())
    }

    fn classify_liteinst_trap(
        &mut self,
        task: &Stopped,
        regs: &libc::user_regs_struct,
    ) -> Option<LiteinstTrap> {
        let config = self.global_state.liteinst_runtime.as_ref()?;
        if regs.rax == config.begin_marker {
            let frame =
                self.validate_liteinst_handshake(task, regs.rdi as usize, regs.ip(), false)?;
            let mut state = self.liteinst_runtime.lock().unwrap();
            if state.phase != LiteinstRuntimePhase::Waiting {
                return None;
            }
            state.phase = LiteinstRuntimePhase::Bootstrap;
            state.frame = Some(frame);
            return Some(LiteinstTrap::HandshakeBegin);
        }
        if regs.rax == config.ready_marker {
            let frame =
                self.validate_liteinst_handshake(task, regs.rdi as usize, regs.ip(), true)?;
            let state = self.liteinst_runtime.lock().unwrap();
            if state.phase != LiteinstRuntimePhase::Bootstrap || state.frame != Some(frame) {
                return None;
            }
            return Some(LiteinstTrap::HandshakeReady);
        }
        if regs.rax != config.syscall_marker {
            return None;
        }
        let handshake = self.liteinst_runtime.lock().unwrap().frame?;
        if regs.ip() != handshake.syscall_trap_rip {
            return None;
        }
        let stack_address = usize::try_from(regs.rsp).ok()?;
        let frame_address = usize::try_from(regs.rdi).ok()?;
        let maps = guest_maps(task.pid())?;
        let controller_stack = maps.iter().find(|mapping| {
            mapping.readable
                && mapping.writable
                && mapping.contains(regs.rsp)
                && mapping.contains(
                    regs.rsp
                        .saturating_add(core::mem::size_of::<u64>() as u64 - 1),
                )
                && mapping.contains(regs.rdi)
                && mapping.contains(
                    regs.rdi
                        .saturating_add(core::mem::size_of::<InjectedSyscallFrame>() as u64 - 1),
                )
        });
        if controller_stack.is_none() || regs.rsp.abs_diff(regs.rdi) > 128 * 1024 {
            return None;
        }
        let return_address: u64 = task.read_value(Addr::from_raw(stack_address)?).ok()?;
        if return_address != handshake.syscall_trap_return_rip {
            // A same-process caller can find the raw trap entry, but only the
            // hidden runtime wrapper produces this exact inner return site.
            return None;
        }
        let frame = match self.read_injected_syscall_frame(task, frame_address) {
            Ok(frame) => frame,
            Err(_) => return Some(LiteinstTrap::Invalid),
        };
        let state = self.liteinst_runtime.lock().unwrap();
        if state.phase != LiteinstRuntimePhase::Ready
            || state.ready_generation != Some(state.generation)
            || !state
                .active_hooks
                .contains_key(&frame.instruction_pointer())
        {
            return Some(LiteinstTrap::Invalid);
        }
        Some(LiteinstTrap::Syscall(frame_address))
    }

    async fn handle_sigtrap(&mut self, mut task: Stopped) -> Result<HandleSignalResult, Error> {
        let resumed_by_gdb_step = self
            .resumed_by_gdb
            .is_some_and(|action| matches!(action, ResumeAction::Step(_)));
        let mut regs = task.getregs()?;
        if let Some(guard) = self.liteinst_entry_guard
            && regs.ip() == guard.address.saturating_add(1)
        {
            let address = Addr::from_raw(guard.address as usize).ok_or(Errno::EFAULT)?;
            let observed: u64 = task.read_value(address)?;
            let guarded_instruction = (guard.saved_instruction & !0xff) | 0xcc;
            if observed != guarded_instruction {
                return Err(Errno::EPROTO.into());
            }
            self.record_liteinst_failure(
                LiteinstActivationFailureReason::ExecutableEntryBeforeHandshake,
                Error::runtime(
                    self.tid(),
                    "verify LiteInst runtime before executable entry",
                    format!(
                        "tracee reached guarded executable entry {:#x} before the required preload handshake completed",
                        guard.address
                    ),
                ),
            );
            return Err(Errno::EPROTO.into());
        }
        match self.classify_liteinst_trap(&task, &regs) {
            Some(LiteinstTrap::HandshakeBegin) => {
                return Ok(HandleSignalResult::SignalSuppressed(
                    self.resume_stopped(task, None)?.next_state().await?,
                ));
            }
            Some(LiteinstTrap::HandshakeReady) => {
                if let Err(error) = self.restore_liteinst_entry_guard(&mut task) {
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::RestoreExecutableEntryGuard,
                        Error::runtime(
                            self.tid(),
                            "restore LiteInst executable-entry guard",
                            error.to_string(),
                        ),
                    );
                    return Err(error.into());
                }
                {
                    let mut state = self.liteinst_runtime.lock().unwrap();
                    if state.phase != LiteinstRuntimePhase::Bootstrap {
                        return Err(Errno::EPROTO.into());
                    }
                    state.phase = LiteinstRuntimePhase::Ready;
                    state.ready_generation = Some(state.generation);
                }
                return Ok(HandleSignalResult::SignalSuppressed(
                    self.resume_stopped(task, None)?.next_state().await?,
                ));
            }
            Some(LiteinstTrap::Syscall(frame_address)) => {
                if let Some(stats) = self
                    .global_state
                    .liteinst_runtime
                    .as_ref()
                    .and_then(|config| config.instrumentation_stats.as_ref())
                {
                    stats.lock().unwrap().record_direct_hook();
                }
                let next_state = self
                    .handle_injected_syscall(task, frame_address, regs.eflags)
                    .await?;
                return Ok(HandleSignalResult::SignalSuppressed(next_state));
            }
            Some(LiteinstTrap::Invalid) => return Err(Errno::EPROTO.into()),
            None => {}
        }
        let phase = self.liteinst_runtime.lock().unwrap().phase;
        if self.global_state.liteinst_runtime.is_some() && phase != LiteinstRuntimePhase::Ready {
            self.record_liteinst_failure(
                LiteinstActivationFailureReason::UnexpectedActivationTrap,
                Error::runtime(
                    self.tid(),
                    "reject unexpected LiteInst activation trap",
                    format!(
                        "received SIGTRAP at RIP {:#x} with RAX {:#x} that matched neither the entry guard nor a validated runtime handshake (phase {phase:?})",
                        regs.ip(), regs.rax
                    ),
                ),
            );
            return Err(Errno::EPROTO.into());
        }
        // TODO-HUMAN-REVIEW(PR-103): Review rewritten-trap provenance validation.
        if let Some(trap) = self.global_state.injected_syscall_trap.as_ref()
            && regs.rax == trap.marker
        {
            if let Ok(frame) = self.read_injected_syscall_frame(&task, regs.rdi as usize)
                && trap.validates_site_provenance(task.pid(), regs.ip(), &frame)
            {
                let next_state = self
                    .handle_injected_syscall(task, regs.rdi as usize, regs.eflags)
                    .await?;
                return Ok(HandleSignalResult::SignalSuppressed(next_state));
            }
            return Ok(HandleSignalResult::SignalToDeliver(task, Signal::SIGTRAP));
        }

        let rip_minus_one = regs.ip() - 1;

        Ok(if self.breakpoints.contains_key(&rip_minus_one) {
            *regs.ip_mut() = rip_minus_one;
            let next_state = self.resume_from_swbreak(task, regs).await?;
            HandleSignalResult::SignalSuppressed(next_state)
        } else if resumed_by_gdb_step {
            self.notify_gdb_stop(StopReason::stopped(
                task.pid(),
                self.pid(),
                StopEvent::Signal(Signal::SIGTRAP),
                regs.into(),
            ))
            .await?;
            let running = self
                .await_gdb_resume(task, ExpectedGdbResume::Resume)
                .await?;
            HandleSignalResult::SignalSuppressed(running.next_state().await?)
        } else {
            HandleSignalResult::SignalToDeliver(task, Signal::SIGTRAP)
        })
    }

    async fn handle_sigstop(&mut self, task: Stopped) -> Result<HandleSignalResult, TraceError> {
        let resumed_by_gdb_step = self
            .resumed_by_gdb
            .is_some_and(|action| matches!(action, ResumeAction::Step(_)));
        debug_assert!(!resumed_by_gdb_step);
        if let Some((suspended_flag, stop_tx)) = self.get_stop_tx().await {
            let notify_stop_tx = stop_tx
                .send((
                    task.pid(),
                    Suspended {
                        waker: self.exit_suspend_tx.clone(),
                        suspended: suspended_flag,
                    },
                ))
                .await;
            drop(stop_tx);
            if notify_stop_tx.is_ok()
                && let Some(rx) = self.exit_suspend_rx.as_mut()
                && rx.recv().await.is_none()
            {
                tracing::warn!(
                    tid = %self.tid(),
                    "tracee suspension channel closed before resume"
                );
            }
        }
        Ok(HandleSignalResult::SignalSuppressed(
            self.resume_stopped(task, None)?.next_state().await?,
        ))
    }

    #[cfg(target_arch = "x86_64")]
    async fn handle_sigsegv(&mut self, task: Stopped) -> Result<HandleSignalResult, TraceError> {
        let regs = task.getregs()?;
        let trap_info = Addr::from_raw(regs.rip as usize)
            .and_then(|addr| task.read_value(addr).ok())
            .and_then(SegfaultTrapInfo::decode_segfault);
        Ok(match trap_info {
            Some(SegfaultTrapInfo::Cpuid)
                if self.global_state.subscriptions.has_cpuid() && self.has_cpuid_interception =>
            {
                let regs = self.handle_cpuid(regs).await?;
                task.setregs(&regs)?;
                HandleSignalResult::SignalSuppressed(
                    self.resume_stopped(task, None)?.next_state().await?,
                )
            }
            Some(SegfaultTrapInfo::Rdtscs(req)) if self.global_state.subscriptions.has_rdtsc() => {
                let regs = self.handle_rdtscs(regs, req).await?;
                task.setregs(&regs)?;
                HandleSignalResult::SignalSuppressed(
                    self.resume_stopped(task, None)?.next_state().await?,
                )
            }
            _ => HandleSignalResult::SignalToDeliver(task, Signal::SIGSEGV),
        })
    }

    #[cfg(not(target_arch = "x86_64"))]
    async fn handle_sigsegv(&mut self, task: Stopped) -> Result<HandleSignalResult, TraceError> {
        Ok(HandleSignalResult::SignalToDeliver(task, Signal::SIGSEGV))
    }

    fn liteinst_activation_in_progress(&self) -> bool {
        #[cfg(test)]
        let test_activation_bypass = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.activate_without_handshake);
        #[cfg(not(test))]
        let test_activation_bypass = false;

        self.global_state.liteinst_runtime.is_some()
            && self.liteinst_runtime.lock().unwrap().phase != LiteinstRuntimePhase::Ready
            && !test_activation_bypass
    }

    fn record_liteinst_failure(&mut self, reason: LiteinstActivationFailureReason, error: Error) {
        let stage = match self.liteinst_runtime.lock().unwrap().phase {
            LiteinstRuntimePhase::Ready => LiteinstActivationStage::PostReady,
            LiteinstRuntimePhase::PreExec
            | LiteinstRuntimePhase::Waiting
            | LiteinstRuntimePhase::Bootstrap => LiteinstActivationStage::PreReady,
        };
        let failure = LiteinstActivationFailure::new(stage, reason, error);
        if let Some(runtime) = self.global_state.liteinst_runtime.clone() {
            let mut slot = runtime.session_failure.lock().unwrap();
            if slot.is_none() {
                *slot = Some(format!("tracee {}: {failure}", self.tid()));
                drop(slot);
                runtime.session_failure_changed.notify_waiters();
            }
        }
        self.liteinst_failure = Some(failure);
    }

    fn reject_liteinst_activation_signal(
        &mut self,
        sig: Signal,
        reason: LiteinstActivationFailureReason,
        detail: impl Into<String>,
    ) -> TraceError {
        self.record_liteinst_failure(
            reason,
            Error::runtime(
                self.tid(),
                "reject unexpected LiteInst activation signal",
                format!(
                    "received {sig} before the required preload handshake completed: {}",
                    detail.into()
                ),
            ),
        );
        Errno::EPROTO.into()
    }

    fn take_pending_signal_for_resume(
        &mut self,
        operation: LiteinstActivationOperation,
    ) -> Result<Option<Signal>, TraceError> {
        let signal = self.pending_signal.take();
        if self.liteinst_activation_in_progress()
            && let Some(sig) = signal
        {
            return Err(self.reject_liteinst_activation_signal(
                sig,
                LiteinstActivationFailureReason::SignalBeforeHandshake(operation),
                format!(
                    "{} attempted to deliver a queued signal",
                    operation.as_str()
                ),
            ));
        }
        Ok(signal)
    }

    fn validate_nested_liteinst_activation_signal(
        &mut self,
        task: &Stopped,
        sig: Signal,
        operation: LiteinstActivationOperation,
        expected_trap: NestedTrapExpectation,
        forced_external_for_test: bool,
    ) -> Result<(), TraceError> {
        if !self.liteinst_activation_in_progress() {
            return Ok(());
        }
        let expected = sig == Signal::SIGTRAP
            && match expected_trap {
                NestedTrapExpectation::None => false,
                NestedTrapExpectation::SyscallSkip { pre_rip } => {
                    is_expected_syscall_skip_trap(task, pre_rip, forced_external_for_test)?
                }
                NestedTrapExpectation::Breakpoint(expected_rip) => {
                    is_expected_breakpoint_trap(task, expected_rip, forced_external_for_test)?
                }
                NestedTrapExpectation::PrivateSyscall(expected_rip) => {
                    is_expected_private_syscall_trap(task, expected_rip, forced_external_for_test)?
                }
            };
        if expected {
            return Ok(());
        }
        Err(self.reject_liteinst_activation_signal(
            sig,
            LiteinstActivationFailureReason::UnexpectedControllerProvenance(operation),
            format!(
                "{} observed a nested signal without the expected controller provenance",
                operation.as_str()
            ),
        ))
    }

    // handle ptrace signal delivery stop
    async fn handle_signal(&mut self, task: Stopped, sig: Signal) -> Result<Wait, Error> {
        tracing::debug!("[{}] handle_signal: received signal {}", task.pid(), sig);
        if self.liteinst_activation_in_progress() {
            match sig {
                Signal::SIGTRAP => {}
                Signal::SIGSEGV => {
                    return match self.handle_sigsegv(task).await? {
                        HandleSignalResult::SignalSuppressed(wait) => Ok(wait),
                        HandleSignalResult::SignalToDeliver(_, _) => {
                            Err(self.reject_liteinst_activation_signal(
                                sig,
                                LiteinstActivationFailureReason::UnexpectedActivationSignal,
                                "the fault was not a subscribed, controller-intercepted CPUID or RDTSC instruction",
                            ).into())
                        }
                    };
                }
                sig if sig == Timer::signal_type() => {
                    let (was_timer, task) = self.handle_timer(task).await?;
                    if !was_timer {
                        return Err(self
                            .reject_liteinst_activation_signal(
                                sig,
                                LiteinstActivationFailureReason::UnexpectedActivationSignal,
                                "the signal was not generated by this tracee's controller timer",
                            )
                            .into());
                    }
                    return Ok(self.resume_stopped(task, None)?.next_state().await?);
                }
                sig => {
                    return Err(self
                        .reject_liteinst_activation_signal(
                            sig,
                            LiteinstActivationFailureReason::UnexpectedActivationSignal,
                            "the signal is outside the activation allowlist",
                        )
                        .into());
                }
            }
        }
        let result = match sig {
            Signal::SIGSEGV => self.handle_sigsegv(task).await?,
            Signal::SIGSTOP => self.handle_sigstop(task).await?,
            Signal::SIGTRAP => self.handle_sigtrap(task).await?,
            sig if sig == Timer::signal_type() => {
                let (was_timer, task) = self.handle_timer(task).await?;
                if was_timer {
                    HandleSignalResult::SignalSuppressed(
                        self.resume_stopped(task, None)?.next_state().await?,
                    )
                } else {
                    HandleSignalResult::SignalToDeliver(task, sig)
                }
            }
            sig => HandleSignalResult::SignalToDeliver(task, sig),
        };

        match result {
            HandleSignalResult::SignalSuppressed(wait) => Ok(wait),
            HandleSignalResult::SignalToDeliver(task, sig) => {
                let result = self
                    .process_state
                    .clone()
                    .handle_signal_event(self, sig)
                    .await;
                let sig = self
                    .ordinary_callback_errno("ptrace signal callback", result)
                    .await?;
                self.ordinary_trace_continuation()?;
                self.timer.finalize_requests();
                Ok(self.resume_stopped(task, sig)?.next_state().await?)
            }
        }
    }

    fn reject_liteinst_nonleader_exec(&mut self, former_tid: Pid) -> TraceError {
        self.record_liteinst_failure(
            LiteinstActivationFailureReason::PostStartExec,
            Error::runtime(
                self.tid(),
                "reject LiteInst post-start exec",
                format!(
                    "exec requires the original thread-group leader (former tid {former_tid}, event tid {}, pid {})",
                    self.tid(), self.pid()
                ),
            ),
        );
        Errno::ENOTSUPP.into()
    }

    // PTRACE_GETEVENTMSG reports the caller's former TID. A nonleader exec
    // already has the leader's TID at this stop, so is_main_thread alone cannot
    // establish which thread replaced the image.
    async fn handle_exec_event(&mut self, task: Stopped, former_tid: Pid) -> Result<Wait, Error> {
        if self.command_bootstrap && task.pid() != self.tid() {
            return Err(Error::runtime(
                self.tid(),
                "observe initial exec",
                "stopped task differs from retained root",
            ));
        }
        if self.command_bootstrap {
            if self.is_root_thread() && self.global_state.source_supported {
                self.cohort = self
                    .global_state
                    .fatal_session
                    .source_cohort
                    .initial_command(&task);
            }
            self.global_state.fatal_session.source_epoch.initial_exec(
                &task,
                self.is_root_thread()
                    && self.global_state.source_supported
                    && self.cohort.is_some(),
            );
        } else {
            if let Some(member) = &self.cohort {
                member.exec_observed();
            }
            self.global_state.fatal_session.source_epoch.revoke();
        }
        let initial_command = self
            .prepare_exec_event(former_tid)
            .tracee_context(self.tid(), "handle exec stop")?;
        if initial_command {
            let filter = self.command_filter.take().ok_or_else(|| {
                Error::runtime(
                    self.tid(),
                    "observe initial exec",
                    "missing backend-owned Command filter",
                )
            })?;
            let observation = InitialCommandStop {
                root_tid: self.tid(),
                former_tid: Some(former_tid),
                filter: &filter,
            };
            // No step/preinit has occurred in the new image. Error preserves
            // the original Tool cause through the existing terminal owner.
            self.process_state
                .clone()
                .handle_initial_exec(self, &observation)
                .await?;
        }
        self.finish_exec_event(task, initial_command)
            .await
            .tracee_context(self.tid(), "handle exec stop")
    }

    fn prepare_exec_event(&mut self, former_tid: Pid) -> Result<bool, TraceError> {
        // PTRACE_EVENT_EXEC proves replacement succeeded. Clear before any
        // post-exec Tool callback; failed exec attempts retain launch provenance.
        let initial_command = self.command_bootstrap;
        if initial_command {
            self.timer.begin_initial_exec();
        }
        self.command_bootstrap = false;
        if self.global_state.liteinst_runtime.is_some() {
            if former_tid != self.tid() {
                return Err(self.reject_liteinst_nonleader_exec(former_tid));
            }
            let state = self.liteinst_runtime.lock().unwrap();
            if state.phase != LiteinstRuntimePhase::PreExec
                && !(state.phase == LiteinstRuntimePhase::Ready && self.is_main_thread())
            {
                let phase = state.phase;
                drop(state);
                self.record_liteinst_failure(
                    LiteinstActivationFailureReason::PostStartExec,
                    Error::runtime(
                        self.tid(),
                        "reject LiteInst post-start exec",
                        format!(
                            "exec requires an activated thread-group leader (phase {phase:?}, tid {}, pid {})",
                            self.tid(), self.pid()
                        ),
                    ),
                );
                return Err(Errno::ENOTSUPP.into());
            }
            let next = state.after_exec();
            drop(state);
            let next = match next {
                Ok(next) => next,
                Err(error) => {
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::PostStartExec,
                        Error::runtime(
                            self.tid(),
                            "advance LiteInst execution generation",
                            error.to_string(),
                        ),
                    );
                    return Err(error.into());
                }
            };
            // The kernel has replaced this address space. Other holders of the
            // old image's state must not observe this reset, and no saved code
            // or controller-stack address may be reused by the new image.
            self.liteinst_runtime = Arc::new(StdMutex::new(next));
            self.liteinst_entry_guard = None;
        }
        // execve/execveat are tail injected, however, after exec, the new
        // program start as a clean slate, hence it is actually ok to do either
        // inject or tail inject after execve succeeded.
        self.pending_syscall = None;
        #[cfg(target_arch = "x86_64")]
        {
            self.original_setsockopt_entry = None;
        }
        self.original_read_entry = None;
        self.pending_syscall_already_skipped = false;
        self.injected_syscall_frame = None;

        Ok(initial_command)
    }

    async fn finish_exec_event(
        &mut self,
        task: Stopped,
        initial_command: bool,
    ) -> Result<Wait, TraceError> {
        // TODO: Update PID? Need to write a test checking this.

        // Step the tracee to get the SIGTRAP that immediately follows the
        // PTRACE_EVENT_EXEC. We can't call `tracee_preinit` until after this
        // because when it tries to step the tracee, it'll get this SIGTRAP
        // signal instead.
        let task = if self.cohort.is_some() {
            // The exec effect has happened, but the first image instruction
            // has not. Consume this same syscall's real EXIT without setting
            // TF or allowing an unobserved first instruction to execute.
            let wait = self.syscall_stopped(task, None)?.next_state().await?;
            self.arm_liteinst_wait(&wait);
            match wait {
                Wait::Stopped(task, Event::Syscall) => {
                    task.syscall_exit_result()?;
                    task
                }
                other => return Ok(other),
            }
        } else if self.global_state.liteinst_runtime.is_some() {
            let expected_post_exec_rip = task.getregs()?.ip();
            let wait = self.step_stopped(task, None)?.next_state().await?;
            self.arm_liteinst_wait(&wait);
            match wait {
                Wait::Stopped(task, Event::Signal(Signal::SIGTRAP)) => {
                    #[cfg(test)]
                    let forced_external_sigtrap = self
                        .global_state
                        .liteinst_runtime
                        .as_ref()
                        .and_then(|runtime| runtime.force_post_exec_signal_once.as_ref())
                        .is_some_and(|force_once| force_once.swap(false, Ordering::SeqCst));
                    #[cfg(not(test))]
                    let forced_external_sigtrap = false;
                    self.validate_nested_liteinst_activation_signal(
                        &task,
                        Signal::SIGTRAP,
                        LiteinstActivationOperation::WaitForPostExecTrap,
                        NestedTrapExpectation::Breakpoint(expected_post_exec_rip),
                        forced_external_sigtrap,
                    )?;
                    task
                }
                Wait::Stopped(task, Event::Signal(sig)) => {
                    self.validate_nested_liteinst_activation_signal(
                        &task,
                        sig,
                        LiteinstActivationOperation::WaitForPostExecTrap,
                        NestedTrapExpectation::None,
                        false,
                    )?;
                    unreachable!("activation validation must reject a non-SIGTRAP signal")
                }
                Wait::Stopped(_, event) => {
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::UnexpectedPostExecEvent,
                        Error::runtime(
                            self.tid(),
                            "validate LiteInst post-exec trap",
                            format!(
                                "received unexpected {event:?} before tracee pre-initialization"
                            ),
                        ),
                    );
                    return Err(Errno::EPROTO.into());
                }
                Wait::Exited(pid, exit_status) => {
                    self.observe_thread_terminal(exit_status);
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::ExitedBeforePostExecTrap,
                        Error::runtime(
                            pid,
                            "validate LiteInst post-exec trap",
                            format!(
                                "tracee exited with {exit_status:?} before the required post-exec SIGTRAP"
                            ),
                        ),
                    );
                    return Err(Errno::EPROTO.into());
                }
            }
        } else {
            let (task, event) = self
                .step_stopped(task, None)?
                .wait_for_signal(Signal::SIGTRAP)
                .await?
                .assume_stopped();
            assert_eq!(event, Event::Signal(Signal::SIGTRAP));
            self.arm_liteinst_root_stop(&task, &event);
            task
        };
        let mut task = self.tracee_preinit(task).await?;
        if let Err(error) = self.install_liteinst_entry_guard(&mut task) {
            self.record_liteinst_failure(
                LiteinstActivationFailureReason::InstallExecutableEntryGuard,
                Error::runtime(
                    self.tid(),
                    "install LiteInst executable-entry guard",
                    error.to_string(),
                ),
            );
            return Err(error);
        }

        #[cfg(test)]
        if self
            .global_state
            .liteinst_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.activate_without_handshake)
        {
            if let Err(error) = self.restore_liteinst_entry_guard(&mut task) {
                self.record_liteinst_failure(
                    LiteinstActivationFailureReason::RestoreExecutableEntryGuard,
                    Error::runtime(
                        self.tid(),
                        "restore test LiteInst executable-entry guard",
                        error.to_string(),
                    ),
                );
                return Err(error);
            }
            {
                let mut state = self.liteinst_runtime.lock().unwrap();
                state.phase = LiteinstRuntimePhase::Ready;
                state.ready_generation = Some(state.generation);
            }
        }

        if initial_command {
            self.timer.finish_initial_exec();
        }
        let result = self.process_state.clone().handle_post_exec(self).await;
        self.ordinary_callback_errno("ptrace post-exec callback", result)
            .await?;
        self.ordinary_trace_continuation()?;
        self.observe_ready_thread_state()?;
        self.timer.finalize_requests();
        if initial_command && let Some(member) = &self.cohort {
            member.initial_ready();
        }

        if self.attached_by_gdb {
            let request_tx = self.gdb_request_tx.clone();
            let resume_tx = self.gdb_resume_tx.clone();

            let proc_exe = format!("/proc/{}/exe", task.pid());
            let exe = std::fs::read_link(&proc_exe).unwrap_or_else(|err| {
                tracing::warn!(
                    tid = %self.tid(),
                    path = %proc_exe,
                    error = %err,
                    "failed to resolve executable after exec; reporting procfs path to GDB"
                );
                proc_exe.clone().into()
            });

            let stopped = StoppedInferior {
                reason: StopReason::stopped(
                    task.pid(),
                    self.pid(),
                    StopEvent::Exec(exe),
                    task.getregs()?.into(),
                ),
                request_tx: request_tx.ok_or(Errno::EIO)?,
                resume_tx: resume_tx.ok_or(Errno::EIO)?,
            };

            // NB: notify initial gdb stop, this is the first time we can
            // tell gdb tracee is ready, because a new memory map has been
            // loaded (due to execve). Otherwise gdb may try to manipulate
            // old process' address space.
            if let Some(attach_tx) = self.gdb_stop_tx.as_ref()
                && attach_tx.send(stopped).await.is_err()
            {
                tracing::warn!(
                    tid = %self.tid(),
                    "GDB stop channel closed while reporting exec"
                );
                self.attached_by_gdb = false;
                return self.step_stopped(task, None)?.next_state().await;
            }
            let running = self
                .await_gdb_resume(task, ExpectedGdbResume::Resume)
                .await?;
            Ok(running.next_state().await?)
        } else {
            if self.global_state.liteinst_runtime.is_some() {
                return self.resume_stopped(task, None)?.next_state().await;
            }
            #[cfg(target_arch = "x86_64")]
            if let Some(context) = self.source_context(Some(&task))
                && let Some(precise) = context.begin_step(&task)?
            {
                // The first instruction of the image is guest code too. Keep
                // its native ENTRY visible before effect; do not use an
                // unobserved bootstrap SINGLESTEP after initial_ready().
                let (running, operation) = context.resume(task, None)?;
                let mut running = self.task_running(running, operation);
                running.observation = Some(context);
                running.administrative = true;
                running.precise = Some(precise);
                let outcome = running.next_step().await?;
                // Context::observe already retained this exact startup wait.
                // Check the same generation/event; do not re-arm a consumed wait.
                self.ensure_liteinst_wait(&outcome.wait);
                if outcome.completion.is_some() && !outcome.guest_event {
                    let Wait::Stopped(task, _) = outcome.wait else {
                        return Err(Errno::EPROTO.into());
                    };
                    // This bootstrap step was already present before this
                    // change. It is not a timer-request completion.
                    self.ordinary_trace_continuation()?;
                    self.timer.finalize_requests();
                    return self.resume_stopped(task, None)?.next_state().await;
                }
                return Ok(outcome.wait);
            }
            let wait = self.step_stopped(task, None)?.next_state().await?;
            self.arm_liteinst_wait(&wait);
            match wait {
                Wait::Stopped(task, Event::Signal(Signal::SIGTRAP))
                    if task.getsiginfo()?.si_code == libc::TRAP_TRACE =>
                {
                    // This is the directly awaited controller single-step,
                    // with no debugger attached. It is not a Tool event and
                    // must not cancel the timer just requested in post-exec.
                    // A real signal, breakpoint, instruction fault, or other
                    // stop still goes through ordinary event accounting.
                    self.ordinary_trace_continuation()?;
                    self.timer.finalize_requests();
                    self.resume_stopped(task, None)?.next_state().await
                }
                wait => Ok(wait),
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_arch_prctl<S: SyscallInfo>(
        &mut self,
        task: Stopped,
        syscall: S,
    ) -> (Stopped, Result<Result<i64, Errno>, TraceError>) {
        let (nr, args) = syscall.into_parts();
        let result = self.untraced_syscall(task, nr, args, false).await;
        (Stopped::new_unchecked(self.tid()), result)
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_prctl(
        &mut self,
        task: Stopped,
        option: libc::c_int,
        arg2: usize,
    ) -> (Stopped, Result<Result<i64, Errno>, TraceError>) {
        let result = self
            .untraced_syscall(
                task,
                Sysno::prctl,
                SyscallArgs::new(option as usize, arg2, 0, 0, 0, 0),
                false,
            )
            .await;
        (Stopped::new_unchecked(self.tid()), result)
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_get_tsc_state(
        &mut self,
        task: Stopped,
        scratch_address: usize,
    ) -> (Stopped, Result<Result<libc::c_int, Errno>, String>) {
        // PR_GET_TSC writes a c_int through a tracee pointer. Reuse the
        // already-validated helper return slot: its original eight bytes are
        // saved before this call, the helper return address replaces them
        // before execution, and every exit path restores them.
        let (task, result) = self
            .liteinst_prctl(task, libc::PR_GET_TSC, scratch_address)
            .await;
        let result = match result {
            Ok(Ok(0)) => match Addr::<libc::c_int>::from_raw(scratch_address) {
                Some(address) => task
                    .read_value(address)
                    .map(Ok)
                    .map_err(|error| format!("read PR_GET_TSC state: {error}")),
                None => Err("PR_GET_TSC scratch address is null".to_owned()),
            },
            Ok(Ok(result)) => Err(format!("PR_GET_TSC returned unexpected value {result}")),
            Ok(Err(error)) => Ok(Err(error)),
            Err(error) => Err(format!("inject PR_GET_TSC: {error}")),
        };
        (task, result)
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_set_tsc_state(
        &mut self,
        task: Stopped,
        state: libc::c_int,
    ) -> (Stopped, Result<Result<i64, Errno>, TraceError>) {
        self.liteinst_prctl(task, libc::PR_SET_TSC, state as usize)
            .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_get_cpuid_state(
        &mut self,
        task: Stopped,
    ) -> (Stopped, Result<Result<i64, Errno>, TraceError>) {
        use reverie::syscalls::ArchPrctl;
        use reverie::syscalls::ArchPrctlCmd;

        self.liteinst_arch_prctl(
            task,
            ArchPrctl::new().with_cmd(ArchPrctlCmd::ARCH_GET_CPUID(None)),
        )
        .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_set_cpuid_state(
        &mut self,
        task: Stopped,
        state: u64,
    ) -> (Stopped, Result<Result<i64, Errno>, TraceError>) {
        use reverie::syscalls::ArchPrctl;
        use reverie::syscalls::ArchPrctlCmd;

        self.liteinst_arch_prctl(
            task,
            ArchPrctl::new().with_cmd(ArchPrctlCmd::ARCH_SET_CPUID(state)),
        )
        .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn set_and_verify_liteinst_cpuid_state(
        &mut self,
        task: Stopped,
        state: u64,
    ) -> (Stopped, Vec<String>) {
        let (task, set_result) = self.liteinst_set_cpuid_state(task, state).await;
        let mut failures = Vec::new();
        match set_result {
            Ok(Ok(0)) => {}
            Ok(Ok(result)) => failures.push(format!(
                "ARCH_SET_CPUID({state}) returned unexpected value {result}"
            )),
            Ok(Err(error)) => failures.push(format!("ARCH_SET_CPUID({state}): {error}")),
            Err(error) => failures.push(format!("inject ARCH_SET_CPUID({state}): {error}")),
        }
        let (task, verify_failures) = self.verify_liteinst_cpuid_state(task, state).await;
        failures.extend(verify_failures);
        (task, failures)
    }

    #[cfg(target_arch = "x86_64")]
    async fn verify_liteinst_cpuid_state(
        &mut self,
        task: Stopped,
        state: u64,
    ) -> (Stopped, Vec<String>) {
        let mut failures = Vec::new();
        let (task, get_result) = self.liteinst_get_cpuid_state(task).await;
        match get_result {
            Ok(Ok(observed)) if observed == state as i64 => {}
            Ok(Ok(observed)) => failures.push(format!(
                "ARCH_GET_CPUID returned {observed} after setting {state}"
            )),
            Ok(Err(error)) => failures.push(format!("verify ARCH_GET_CPUID({state}): {error}")),
            Err(error) => failures.push(format!("inject verification ARCH_GET_CPUID: {error}")),
        }
        (task, failures)
    }

    #[cfg(target_arch = "x86_64")]
    async fn prepare_liteinst_helper_cpuid(
        &mut self,
        task: Stopped,
    ) -> (Stopped, Result<LiteinstCpuidPolicy, String>) {
        let (task, result) = self.liteinst_get_cpuid_state(task).await;
        match result {
            Ok(Ok(1)) => (task, Ok(LiteinstCpuidPolicy::UnchangedEnabled)),
            Ok(Ok(0)) => {
                let (task, enable_failures) =
                    self.set_and_verify_liteinst_cpuid_state(task, 1).await;
                if enable_failures.is_empty() {
                    (task, Ok(LiteinstCpuidPolicy::RestoreDisabled))
                } else {
                    let (task, restore_failures) =
                        self.set_and_verify_liteinst_cpuid_state(task, 0).await;
                    let mut message = format!(
                        "enable native CPUID for patch helper: {}",
                        enable_failures.join("; ")
                    );
                    if !restore_failures.is_empty() {
                        message.push_str(&format!(
                            "; restore original CPUID policy after enable failure: {}",
                            restore_failures.join("; ")
                        ));
                    }
                    (task, Err(message))
                }
            }
            Ok(Ok(state)) => (
                task,
                Err(format!("ARCH_GET_CPUID returned unexpected value {state}")),
            ),
            Ok(Err(Errno::ENODEV)) => (task, Ok(LiteinstCpuidPolicy::Unsupported)),
            Ok(Err(error)) => (task, Err(format!("ARCH_GET_CPUID: {error}"))),
            Err(error) => (task, Err(format!("inject ARCH_GET_CPUID: {error}"))),
        }
    }

    #[cfg(target_arch = "x86_64")]
    async fn set_and_verify_liteinst_tsc_state(
        &mut self,
        task: Stopped,
        scratch_address: usize,
        state: libc::c_int,
    ) -> (Stopped, Vec<String>) {
        let (task, set_result) = self.liteinst_set_tsc_state(task, state).await;
        let mut failures = Vec::new();
        match set_result {
            Ok(Ok(0)) => {}
            Ok(Ok(result)) => {
                failures.push(format!(
                    "PR_SET_TSC({state}) returned unexpected value {result}"
                ));
            }
            Ok(Err(error)) => failures.push(format!("PR_SET_TSC({state}): {error}")),
            Err(error) => failures.push(format!("inject PR_SET_TSC({state}): {error}")),
        }
        let (task, verify_failures) = self
            .verify_liteinst_tsc_state(task, scratch_address, state)
            .await;
        failures.extend(verify_failures);
        (task, failures)
    }

    #[cfg(target_arch = "x86_64")]
    async fn verify_liteinst_tsc_state(
        &mut self,
        task: Stopped,
        scratch_address: usize,
        state: libc::c_int,
    ) -> (Stopped, Vec<String>) {
        let mut failures = Vec::new();
        let (task, get_result) = self.liteinst_get_tsc_state(task, scratch_address).await;
        match get_result {
            Ok(Ok(observed)) if observed == state => {}
            Ok(Ok(observed)) => failures.push(format!(
                "PR_GET_TSC returned {observed} after setting {state}"
            )),
            Ok(Err(error)) => failures.push(format!("verify PR_GET_TSC({state}): {error}")),
            Err(error) => failures.push(format!("verify PR_GET_TSC({state}): {error}")),
        }
        (task, failures)
    }

    #[cfg(target_arch = "x86_64")]
    async fn prepare_liteinst_helper_tsc(
        &mut self,
        task: Stopped,
        scratch_address: usize,
    ) -> (Stopped, Result<LiteinstTscPolicy, String>) {
        let (task, result) = self.liteinst_get_tsc_state(task, scratch_address).await;
        match result {
            Ok(Ok(libc::PR_TSC_ENABLE)) => (task, Ok(LiteinstTscPolicy::UnchangedEnabled)),
            Ok(Ok(libc::PR_TSC_SIGSEGV)) => {
                let (task, enable_failures) = self
                    .set_and_verify_liteinst_tsc_state(task, scratch_address, libc::PR_TSC_ENABLE)
                    .await;
                if enable_failures.is_empty() {
                    (task, Ok(LiteinstTscPolicy::RestoreFaulting))
                } else {
                    let (task, restore_failures) = self
                        .set_and_verify_liteinst_tsc_state(
                            task,
                            scratch_address,
                            libc::PR_TSC_SIGSEGV,
                        )
                        .await;
                    let mut message = format!(
                        "enable native TSC for patch helper: {}",
                        enable_failures.join("; ")
                    );
                    if !restore_failures.is_empty() {
                        message.push_str(&format!(
                            "; restore original TSC policy after enable failure: {}",
                            restore_failures.join("; ")
                        ));
                    }
                    (task, Err(message))
                }
            }
            Ok(Ok(state)) => (
                task,
                Err(format!("PR_GET_TSC returned unexpected state {state}")),
            ),
            // EINVAL is the documented prctl response when this option is not
            // supported by the running kernel/architecture.
            Ok(Err(Errno::EINVAL)) => (task, Ok(LiteinstTscPolicy::Unsupported)),
            Ok(Err(error)) => (task, Err(format!("PR_GET_TSC: {error}"))),
            Err(error) => (task, Err(error)),
        }
    }

    #[cfg(target_arch = "x86_64")]
    async fn restore_liteinst_helper_state(
        &mut self,
        task: Stopped,
        saved: &LiteinstHelperSavedState,
    ) -> (Stopped, Vec<String>) {
        let (task, mut failures) = match saved.tsc_policy {
            LiteinstTscPolicy::Unsupported => (task, Vec::new()),
            LiteinstTscPolicy::RestoreFaulting => {
                let (task, failures) = self
                    .set_and_verify_liteinst_tsc_state(
                        task,
                        saved.stack_address,
                        libc::PR_TSC_SIGSEGV,
                    )
                    .await;
                (
                    task,
                    failures
                        .into_iter()
                        .map(|failure| format!("TSC policy: {failure}"))
                        .collect(),
                )
            }
            LiteinstTscPolicy::UnchangedEnabled => {
                let (task, failures) = self
                    .verify_liteinst_tsc_state(task, saved.stack_address, libc::PR_TSC_ENABLE)
                    .await;
                (
                    task,
                    failures
                        .into_iter()
                        .map(|failure| format!("TSC policy: {failure}"))
                        .collect(),
                )
            }
        };
        let (mut task, cpuid_failures) = match saved.cpuid_policy {
            LiteinstCpuidPolicy::Unsupported => (task, Vec::new()),
            LiteinstCpuidPolicy::RestoreDisabled => {
                let (task, failures) = self.set_and_verify_liteinst_cpuid_state(task, 0).await;
                (
                    task,
                    failures
                        .into_iter()
                        .map(|failure| format!("CPUID policy: {failure}"))
                        .collect(),
                )
            }
            LiteinstCpuidPolicy::UnchangedEnabled => {
                let (task, failures) = self.verify_liteinst_cpuid_state(task, 1).await;
                (
                    task,
                    failures
                        .into_iter()
                        .map(|failure| format!("CPUID policy: {failure}"))
                        .collect(),
                )
            }
        };
        failures.extend(cpuid_failures);
        match AddrMut::from_raw(saved.stack_address) {
            Some(address) => {
                if let Err(error) = task.write_value(address, &saved.stack_value) {
                    failures.push(format!("helper stack: {error}"));
                }
            }
            None => failures.push("helper stack: invalid restore address".to_owned()),
        }
        if let Err(error) = task.setxstate(&saved.xstate) {
            failures.push(format!("XSTATE: {error}"));
        }
        if let Err(error) = task.setregs(&saved.regs) {
            failures.push(format!("general registers: {error}"));
        }
        (task, failures)
    }

    #[cfg(target_arch = "x86_64")]
    async fn rollback_liteinst_helper_error(
        &mut self,
        task: Stopped,
        saved: &LiteinstHelperSavedState,
        original: Error,
    ) -> Error {
        let (_, rollback_failures) = self.restore_liteinst_helper_state(task, saved).await;
        self.liteinst_helper_failure(original, rollback_failures)
    }

    fn liteinst_helper_failure(&self, original: Error, rollback_failures: Vec<String>) -> Error {
        if rollback_failures.is_empty() {
            original
        } else {
            Error::runtime(
                self.tid(),
                "restore LiteInst patch-helper state",
                format!(
                    "original failure: {original}; rollback failures: {}",
                    rollback_failures.join("; ")
                ),
            )
        }
    }

    fn record_liteinst_fallback_stats(
        &self,
        task: &Stopped,
        frame: LiteinstHandshakeFrame,
        site: u64,
    ) {
        let stats = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|config| config.instrumentation_stats.as_ref());
        crate::liteinst_stats::with_liteinst_stats(stats, |stats| {
            let shape = Addr::from_raw(frame.install_result as usize)
                .and_then(|address| {
                    let result: LiteinstInstallResult = task.read_value(address).ok()?;
                    Some(result)
                })
                .and_then(|result| {
                    let instruction_len = usize::try_from(result.instruction_len).ok()?;
                    let straddle_prefix = usize::try_from(result.straddle_prefix).ok()?;
                    (result.version == 2
                        && result.complete == 0
                        && result.site_start == site
                        && result.site_len == 8
                        && (1..=15).contains(&instruction_len)
                        && straddle_prefix < instruction_len.min(5))
                    .then_some((
                        instruction_len,
                        (straddle_prefix != 0).then_some(straddle_prefix),
                    ))
                });
            let outcome = if shape.as_ref().is_some_and(|(_, prefix)| prefix.is_some()) {
                LiteinstPatchOutcome::PtraceStraddlerBail
            } else {
                LiteinstPatchOutcome::PtraceOtherFallback
            };
            let process_identity =
                u64::try_from(self.pid.as_raw()).expect("tracee PID must be positive");
            let execution_generation = {
                let mut runtime = self.liteinst_runtime.lock().unwrap();
                runtime.fallback_sites.insert(site, outcome);
                runtime.generation
            };
            stats.record_process_site(process_identity, execution_generation, site, outcome, shape);
            match outcome {
                LiteinstPatchOutcome::PtraceStraddlerBail => {
                    stats.record_cacheline_straddler_fallback();
                }
                LiteinstPatchOutcome::PtraceOtherFallback => {
                    stats.record_unpatchable_or_other_fallback();
                }
                LiteinstPatchOutcome::DirectPunPatched | LiteinstPatchOutcome::RelocatedPatched => {
                    unreachable!("fallback accounting received a patched outcome")
                }
            }
        });
    }

    fn record_retained_liteinst_fallback_hit(&self, task: &Stopped) {
        let Some(stats) = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|config| config.instrumentation_stats.as_ref())
        else {
            return;
        };
        let Some(site) = task
            .getregs()
            .ok()
            .and_then(|regs| regs.ip().checked_sub(2))
        else {
            return;
        };
        let outcome = self
            .liteinst_runtime
            .lock()
            .unwrap()
            .fallback_sites
            .get(&site)
            .copied();
        crate::liteinst_stats::with_liteinst_stats(Some(stats), |stats| match outcome {
            Some(LiteinstPatchOutcome::PtraceStraddlerBail) => {
                stats.record_cacheline_straddler_fallback();
            }
            Some(LiteinstPatchOutcome::PtraceOtherFallback) => {
                stats.record_unpatchable_or_other_fallback();
            }
            Some(
                LiteinstPatchOutcome::DirectPunPatched | LiteinstPatchOutcome::RelocatedPatched,
            )
            | None => {}
        });
    }

    fn validate_liteinst_install_result(
        &self,
        task: &Stopped,
        frame: LiteinstHandshakeFrame,
        site: u64,
    ) -> Option<(u64, ActiveHookFootprint)> {
        let address = Addr::from_raw(frame.install_result as usize)?;
        let result: LiteinstInstallResult = task.read_value(address).ok()?;
        let instruction_len = usize::try_from(result.instruction_len).ok()?;
        let straddle_prefix = usize::try_from(result.straddle_prefix).ok()?;
        if result.version != 2
            || result.complete != 1
            || result.site_start != site
            || result.site_len != 8
            || !(1..=15).contains(&instruction_len)
            || straddle_prefix >= instruction_len.min(5)
        {
            return None;
        }
        let site = GuestRange::new(result.site_start, result.site_len)?;
        let trampoline = GuestRange::new(result.trampoline_start, result.trampoline_len)?;
        let arena_writable =
            GuestRange::new(result.arena_writable_start, result.arena_writable_len)?;
        let arena_executable =
            GuestRange::new(result.arena_executable_start, result.arena_executable_len)?;
        if !arena_executable.contains(trampoline)
            || !trampoline.contains(GuestRange::new(result.relocated_tail, 1)?)
        {
            return None;
        }
        let maps = guest_maps(task.pid())?;
        let site_map = maps.iter().find(|mapping| {
            mapping.readable
                && !mapping.writable
                && mapping.executable
                && mapping.contains_range(site)
        })?;
        let writable_map = maps.iter().find(|mapping| {
            mapping.start == arena_writable.start
                && mapping.end == arena_writable.end
                && mapping.offset == 0
                && mapping.inode != 0
                && mapping.shared
                && mapping.readable
                && mapping.writable
                && !mapping.executable
        })?;
        let executable_map = maps.iter().find(|mapping| {
            mapping.start == arena_executable.start
                && mapping.end == arena_executable.end
                && mapping.offset == 0
                && mapping.inode != 0
                && mapping.shared
                && mapping.readable
                && !mapping.writable
                && mapping.executable
        })?;
        if writable_map.device_major != executable_map.device_major
            || writable_map.device_minor != executable_map.device_minor
            || writable_map.inode != executable_map.inode
            || writable_map.end - writable_map.start != executable_map.end - executable_map.start
            || site_map.start == writable_map.start
            || site_map.start == executable_map.start
        {
            return None;
        }
        if let Some(stats) = self
            .global_state
            .liteinst_runtime
            .as_ref()?
            .instrumentation_stats
            .as_ref()
        {
            let mut stats = stats.lock().unwrap();
            let process_identity =
                u64::try_from(self.pid.as_raw()).expect("tracee PID must be positive");
            let execution_generation = self.liteinst_runtime.lock().unwrap().generation;
            stats.record_process_site(
                process_identity,
                execution_generation,
                result.site_start,
                LiteinstPatchOutcome::RelocatedPatched,
                Some((
                    instruction_len,
                    (straddle_prefix != 0).then_some(straddle_prefix),
                )),
            );
            stats.record_ptrace_installation();
        }
        Some((
            result.relocated_tail,
            ActiveHookFootprint {
                site,
                trampoline,
                arena_writable,
                arena_executable,
            },
        ))
    }

    #[cfg(target_arch = "x86_64")]
    async fn call_liteinst_install_helper(
        &mut self,
        task: Stopped,
        frame: LiteinstHandshakeFrame,
        site: u64,
    ) -> Result<(Stopped, Option<(u64, ActiveHookFootprint)>), Error> {
        let helper_return_marker = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .ok_or(Errno::EIO)?
            .helper_return_marker;
        let saved_regs = task.getregs()?;
        let saved_xstate = task.getxstate()?;
        let stack_address = frame.helper_stack_top.saturating_sub(8) as usize;
        let stack_read_address = Addr::from_raw(stack_address).ok_or(Errno::EFAULT)?;
        let stack_write_address = AddrMut::from_raw(stack_address).ok_or(Errno::EFAULT)?;
        let saved_stack: u64 = task.read_value(stack_read_address)?;
        let mut saved = LiteinstHelperSavedState {
            cpuid_policy: LiteinstCpuidPolicy::Unsupported,
            tsc_policy: LiteinstTscPolicy::Unsupported,
            regs: saved_regs,
            xstate: saved_xstate,
            stack_address,
            stack_value: saved_stack,
        };
        let (task, cpuid_policy) = self.prepare_liteinst_helper_cpuid(task).await;
        saved.cpuid_policy = match cpuid_policy {
            Ok(policy) => policy,
            Err(message) => {
                let original = Error::runtime(
                    self.tid(),
                    "prepare LiteInst patch-helper CPUID policy",
                    message,
                );
                let (_, rollback_failures) = self.restore_liteinst_helper_state(task, &saved).await;
                return Err(self.liteinst_helper_failure(original, rollback_failures));
            }
        };
        let (task, tsc_policy) = self.prepare_liteinst_helper_tsc(task, stack_address).await;
        saved.tsc_policy = match tsc_policy {
            Ok(policy) => policy,
            Err(message) => {
                let original = Error::runtime(
                    self.tid(),
                    "prepare LiteInst patch-helper TSC policy",
                    message,
                );
                let (_, rollback_failures) = self.restore_liteinst_helper_state(task, &saved).await;
                return Err(self.liteinst_helper_failure(original, rollback_failures));
            }
        };
        let mut task = task;
        if let Err(error) = task.write_value(stack_write_address, &frame.helper_return) {
            let original = Error::from(error);
            return Err(self
                .rollback_liteinst_helper_error(task, &saved, original)
                .await);
        }

        let mut helper_regs = saved.regs;
        *helper_regs.ip_mut() = frame.install_helper;
        *helper_regs.stack_ptr_mut() = frame.helper_stack_top - 8;
        helper_regs.rdi = site;
        *helper_regs.orig_syscall_mut() = -1_i64 as u64;
        helper_regs.eflags = liteinst_helper_entry_rflags(saved.regs.eflags);
        if let Err(error) = task.setregs(&helper_regs) {
            let original = Error::Internal(error);
            return Err(self
                .rollback_liteinst_helper_error(task, &saved, original)
                .await);
        }

        let running = match self.resume_stopped(task, None) {
            Ok(running) => running,
            Err(error) => {
                return Err(self
                    .rollback_liteinst_helper_error(
                        Stopped::new_unchecked(self.tid()),
                        &saved,
                        Error::Internal(error),
                    )
                    .await);
            }
        };
        let mut wait = match running.next_state().await {
            Ok(wait) => wait,
            Err(error) => {
                return Err(self
                    .rollback_liteinst_helper_error(
                        Stopped::new_unchecked(self.tid()),
                        &saved,
                        Error::Internal(error),
                    )
                    .await);
            }
        };
        self.arm_liteinst_wait(&wait);
        loop {
            match wait {
                Wait::Stopped(stopped, Event::Seccomp) => {
                    // Controller-owned helper syscalls execute natively and are
                    // never delivered to the user Tool.
                    let running = match self.resume_stopped(stopped, None) {
                        Ok(running) => running,
                        Err(error) => {
                            return Err(self
                                .rollback_liteinst_helper_error(
                                    Stopped::new_unchecked(self.tid()),
                                    &saved,
                                    Error::Internal(error),
                                )
                                .await);
                        }
                    };
                    wait = match running.next_state().await {
                        Ok(wait) => wait,
                        Err(error) => {
                            return Err(self
                                .rollback_liteinst_helper_error(
                                    Stopped::new_unchecked(self.tid()),
                                    &saved,
                                    Error::Internal(error),
                                )
                                .await);
                        }
                    };
                    self.arm_liteinst_wait(&wait);
                }
                Wait::Stopped(stopped, Event::Signal(Signal::SIGTRAP)) => {
                    let regs = match stopped.getregs() {
                        Ok(regs) => regs,
                        Err(error) => {
                            let original = Error::Internal(error);
                            return Err(self
                                .rollback_liteinst_helper_error(stopped, &saved, original)
                                .await);
                        }
                    };
                    if regs.r10 != helper_return_marker || regs.ip() != frame.helper_return_rip {
                        let original = Error::runtime(
                            self.tid(),
                            "validate LiteInst patch-helper return",
                            "unexpected helper return marker or instruction pointer",
                        );
                        return Err(self
                            .rollback_liteinst_helper_error(stopped, &saved, original)
                            .await);
                    }
                    let result = regs.rax as i64;
                    let install = if u64::try_from(result).is_ok() {
                        match self.validate_liteinst_install_result(&stopped, frame, site) {
                            Some(install) => Some(install),
                            None => {
                                let original = Error::runtime(
                                    self.tid(),
                                    "validate LiteInst patch-helper result",
                                    "successful helper returned invalid active-hook metadata",
                                );
                                return Err(self
                                    .rollback_liteinst_helper_error(stopped, &saved, original)
                                    .await);
                            }
                        }
                    } else {
                        self.record_liteinst_fallback_stats(&stopped, frame, site);
                        None
                    };
                    let (stopped, rollback) =
                        self.restore_liteinst_helper_state(stopped, &saved).await;
                    if rollback.is_empty() {
                        return Ok((stopped, install));
                    }
                    let original = Error::runtime(
                        self.tid(),
                        "restore LiteInst patch-helper state",
                        "patch helper completed successfully",
                    );
                    return Err(self.liteinst_helper_failure(original, rollback));
                }
                Wait::Stopped(stopped, event) => {
                    let original = Error::runtime(
                        self.tid(),
                        "run LiteInst patch helper",
                        format!("unexpected stopped event: {event:?}"),
                    );
                    return Err(self
                        .rollback_liteinst_helper_error(stopped, &saved, original)
                        .await);
                }
                Wait::Exited(_, exit_status) => self.exit(exit_status).await,
            }
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    async fn call_liteinst_install_helper(
        &mut self,
        _task: Stopped,
        _frame: LiteinstHandshakeFrame,
        _site: u64,
    ) -> Result<(Stopped, Option<(u64, ActiveHookFootprint)>), Error> {
        Err(Error::runtime(
            self.tid(),
            "run LiteInst patch helper",
            "the dynamic LiteInst hybrid requires x86-64 XSTATE support",
        ))
    }

    async fn maybe_install_liteinst_site(
        &mut self,
        task: Stopped,
        nr: Sysno,
    ) -> Result<(Stopped, bool, Option<u64>), Error> {
        if self.global_state.liteinst_runtime.is_none() {
            return Ok((task, false, None));
        }
        // Keep this Read on the existing native interception path. Installing
        // a first-site hook consumes its seccomp entry and would turn even the
        // first Read into a private attempt that can be interrupted before
        // kernel entry. A post-exit helper is also unsafe while the Read's
        // signal is pending. Decline this optimization, preserving the same
        // Tool callback and data-movement path; static E9 sites remain separate.
        if nr == Sysno::read {
            return Ok((task, false, None));
        }
        // A task-creating syscall must not be patched. Patching overwrites the
        // instruction bytes AT the site, and the new task is resumed with the
        // register context captured before the injection -- i.e. with `rip`
        // pointing just past the original two-byte `syscall`. Once the site
        // holds a longer relocating jump, that address is no longer an
        // instruction boundary and the child executes rubbish. Leaving these
        // sites unpatched costs nothing: they are entered once per task.
        if is_task_creating_syscall(nr) {
            return Ok((task, false, None));
        }
        if self
            .global_state
            .liteinst_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.multi_task.load(Ordering::Acquire))
        {
            return Ok((task, false, None));
        }
        let regs = task.getregs()?;
        let Some(site) = regs.ip().checked_sub(2) else {
            return Ok((task, false, None));
        };
        let site_address = Addr::from_raw(site as usize).ok_or(Errno::EFAULT)?;
        let instruction: u16 = task.read_value(site_address)?;
        if instruction != 0x050f {
            return Ok((task, false, None));
        }
        let frame = {
            let mut state = self.liteinst_runtime.lock().unwrap();
            if state.phase != LiteinstRuntimePhase::Ready
                || state.ready_generation != Some(state.generation)
                || !state.attempted_sites.insert(site)
            {
                return Ok((task, false, None));
            }
            state.frame.ok_or(Errno::EIO)?
        };

        if let Some(stats) = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|config| config.instrumentation_stats.as_ref())
        {
            stats.lock().unwrap().record_first_site_seccomp();
        }

        // Convert the active seccomp stop into an ordinary stopped state before
        // calling arbitrary tracee code. The original event is still serviced
        // exactly once by the host Tool below.
        let task = self.skip_seccomp_syscall(task).await?;
        let (task, install) = self.call_liteinst_install_helper(task, frame, site).await?;
        let relocated_tail = install.as_ref().map(|(address, _)| *address);
        if let Some((_, footprint)) = install {
            self.liteinst_runtime
                .lock()
                .unwrap()
                .active_hooks
                .insert(site, footprint);
        }
        Ok((task, true, relocated_tail))
    }

    fn validate_liteinst_mapping_execution(
        &self,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<(), Errno> {
        let page_size = host_page_size()?;
        if self
            .liteinst_runtime
            .lock()
            .unwrap()
            .mapping_mutates_active_hook(nr, args, page_size)
        {
            Err(Errno::ENOTSUPP)
        } else {
            Ok(())
        }
    }

    fn observe_liteinst_mapping_result(
        &mut self,
        nr: Sysno,
        args: SyscallArgs,
        result: Result<i64, Errno>,
    ) {
        if self.global_state.liteinst_runtime.is_none() {
            return;
        }
        let Ok(result) = result else {
            return;
        };
        let mut state = self.liteinst_runtime.lock().unwrap();
        let Ok(page_size) = host_page_size() else {
            state.attempted_sites.clear();
            state.fallback_sites.clear();
            return;
        };
        match nr {
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::mmap => {
                if let Ok(start) = u64::try_from(result) {
                    state.invalidate_attempted_pages(start, args.arg1 as u64, page_size);
                }
            }
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::munmap | Sysno::mprotect | Sysno::pkey_mprotect => {
                state.invalidate_attempted_pages(args.arg0 as u64, args.arg1 as u64, page_size);
            }
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::mremap => {
                state.invalidate_attempted_pages(args.arg0 as u64, args.arg1 as u64, page_size);
                if let Ok(start) = u64::try_from(result) {
                    state.invalidate_attempted_pages(start, args.arg2 as u64, page_size);
                }
            }
            _ => {}
        }
    }

    async fn handle_liteinst_mapping_syscall(
        &mut self,
        task: Stopped,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<Wait, Error> {
        let tid = self.tid();
        if self.validate_liteinst_mapping_execution(nr, args).is_err() {
            return Err(Error::runtime(
                tid,
                "validate LiteInst mapping mutation",
                format!("{nr} overlaps an active LiteInst hook footprint"),
            ));
        }
        let wait = self
            .syscall_stopped(task, None)
            .tracee_context(tid, "resume controller-observed mapping syscall")?
            .next_state()
            .await
            .tracee_context(tid, "wait for controller-observed mapping syscall")?;
        self.arm_liteinst_wait(&wait);
        match wait {
            Wait::Stopped(stopped, Event::Syscall) => {
                let regs = stopped
                    .getregs()
                    .tracee_context(tid, "read controller-observed mapping result")?;
                let result = Errno::from_ret(regs.ret() as usize).map(|value| value as i64);
                self.observe_liteinst_mapping_result(nr, args, result);
                self.resume_stopped(stopped, None)
                    .tracee_context(tid, "resume after controller-observed mapping syscall")?
                    .next_state()
                    .await
                    .tracee_context(tid, "wait after controller-observed mapping syscall")
            }
            Wait::Stopped(_, event) => Err(Error::runtime(
                tid,
                "observe LiteInst mapping syscall",
                format!("unexpected stopped event: {event:?}"),
            )),
            Wait::Exited(_, exit_status) => self.exit(exit_status).await,
        }
    }

    async fn handle_seccomp(&mut self, mut task: Stopped) -> Result<Wait, Error> {
        let tid = self.tid();
        // The source tier observes x32/high raw numbers without pretending they
        // are native Sysno values. Preserve their actual kernel execution/result.
        let raw_number = task.getregs()?.orig_syscall() as u32;
        if raw_number >= 0x4000_0000 {
            if (raw_number as i32) >= 0 {
                self.global_state.fatal_session.source_epoch.revoke();
            }
            return self
                .resume_stopped(task, None)?
                .next_state()
                .await
                .map_err(Into::into);
        }
        let syscall = self
            .get_syscall(&task)
            .tracee_context(tid, "read registers at seccomp stop")?;
        let (nr, args) = syscall.into_parts();
        self.global_state
            .fatal_session
            .source_epoch
            .observe_classified(nr, args, self.original_source_ioctl_matches(&task));
        let tool_subscribed = self
            .global_state
            .subscriptions
            .iter_syscalls()
            .any(|subscribed| subscribed == nr);
        // Source-only observation neither redispatches the Tool nor changes
        // syscall flags/results. The existing private-IP exemption still means
        // no Tool callback, even when the source tier observed the syscall.
        let private_ip = {
            let ip = task.getregs()?.ip() as usize;
            (cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE
                ..cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE + cp::UD_INSTR_SIZE)
                .contains(&ip)
        };
        if source_epoch::observes(nr) && (!tool_subscribed || private_ip) {
            return self
                .resume_stopped(task, None)?
                .next_state()
                .await
                .map_err(Into::into);
        }
        if is_liteinst_mapping_syscall(nr) && !tool_subscribed {
            return self.handle_liteinst_mapping_syscall(task, nr, args).await;
        }
        self.record_retained_liteinst_fallback_hit(&task);
        let (installed_task, syscall_already_skipped, liteinst_resume_rip) =
            self.maybe_install_liteinst_site(task, nr).await?;
        task = installed_task;
        #[cfg(target_arch = "x86_64")]
        let is_legacy_vsyscall = !syscall_already_skipped
            && is_legacy_vsyscall_ip(
                task.getregs()
                    .tracee_context(tid, "identify legacy vsyscall stop")?
                    .ip(),
            );
        #[cfg(not(target_arch = "x86_64"))]
        let is_legacy_vsyscall = false;
        let span = tracing::trace_span!(
            target: "reverie_ptrace::syscall",
            "syscall.intercept",
            tid = %tid,
            syscall = %nr,
            args = ?SyscallArgsForLog {
                nr,
                args,
                command_bootstrap: self.command_bootstrap,
            },
        );

        async {
            tracing::trace!(
                target: "reverie_ptrace::syscall",
                "intercepting guest syscall"
            );
            self.pending_syscall = Some((nr, args));
            self.pending_syscall_already_skipped = syscall_already_skipped;
            #[cfg(target_arch = "x86_64")]
            {
                self.original_setsockopt_entry =
                    if nr == Sysno::setsockopt && !syscall_already_skipped {
                        Some(original_setsockopt::OriginalSetsockoptEntry::capture(
                            &task, args,
                        ))
                    } else {
                        None
                    };
            }
            self.original_read_entry = (matches!(nr, Sysno::read | Sysno::recvfrom)
                && !syscall_already_skipped)
                .then(|| original_context::OriginalReadEntry::capture(&task, nr, args));

            self.source_stop = task.source_stop().ok().map(Arc::new);
            #[cfg(target_arch = "x86_64")]
            self.begin_private_logical_call(&task, (nr, args))?;
            let retval = cancellable(self.cancel_handler.clone(), async {
                self.process_state
                    .clone()
                    .handle_syscall_event(self, syscall)
                    .await
            })
            .await;
            self.source_stop = None;

            #[cfg(target_arch = "x86_64")]
            if let Some(wait) = self.finish_private_signal_callback(&retval).await? {
                return Ok(wait);
            }

            #[cfg(target_arch = "x86_64")]
            if self.interrupted_read.is_some() {
                self.original_read_entry = None;
                // InterruptedSyscall is the authenticated handback token for
                // this stop, not a backend failure. Let the established Read
                // path validate and retire it before generic Tool-error
                // classification can fail the ordinary session.
                return self
                    .finish_original_read_callback(&retval)
                    .tracee_context(tid, "complete interrupted original Read callback");
            }

            let retval = if self.ordinary_failure_enabled() {
                match retval {
                    Some(Err(error)) if !matches!(error, reverie::Error::Errno(_)) => {
                        self.publish_ordinary_failure("ptrace syscall callback", error);
                        return Err(Error::RunFailed);
                    }
                    result => result,
                }
            } else {
                retval
            };
            self.ordinary_continuation()?;

            // A returned emulation must consume its original stop just like an
            // injection. Keeping Some after skipping would let a later signal
            // or timer callback mistake an ordinary stop for a seccomp entry.
            let pending_syscall = self.pending_syscall.take();
            #[cfg(target_arch = "x86_64")]
            {
                self.original_setsockopt_entry = None;
            }
            self.original_read_entry = None;
            let emulate_legacy_vsyscall = is_legacy_vsyscall && pending_syscall.is_some();
            // The kernel owns the synthetic `ret` from the fixed vsyscall
            // page. That path stays at its seccomp stop and is marked skipped
            // below, so the kernel returns without stepping the caller.
            if pending_syscall.is_some() && !syscall_already_skipped && !emulate_legacy_vsyscall {
                task = self
                    .skip_seccomp_syscall(task)
                    .await
                    .tracee_context(tid, "skip intercepted syscall")?;
            }

            self.ordinary_trace_continuation()?;
            self.timer.finalize_requests();

            if let Some(retval) = retval {
                let ret = match retval {
                    Ok(x) => x as u64,
                    Err(err) => (-(err.into_errno()?.into_raw() as i64)) as u64,
                };

                #[cfg(target_arch = "x86_64")]
                if emulate_legacy_vsyscall {
                    let mut regs = task
                        .getregs()
                        .tracee_context(tid, "read legacy-vsyscall registers")?;
                    *regs.orig_syscall_mut() = -1i64 as u64;
                    *regs.ret_mut() = ret;
                    task.setregs(&regs)
                        .tracee_context(tid, "set legacy-vsyscall result")?;
                } else {
                    set_ret(&task, ret).tracee_context(tid, "set intercepted syscall result")?;
                }

                #[cfg(not(target_arch = "x86_64"))]
                set_ret(&task, ret).tracee_context(tid, "set intercepted syscall result")?;
            }

            self.pending_syscall_already_skipped = false;

            #[cfg(target_arch = "x86_64")]
            {
                let needs_original = self
                    .source_observer
                    .lock()
                    .unwrap()
                    .tool_needs_original_resume();
                if needs_original {
                    // Matching tail injection leaves the original SECCOMP
                    // attempt uneffected. Finish that very attempt, including
                    // native parent restoration, before completing the logical
                    // instruction transferred from the precise timer.
                    let native = self
                        .cohort
                        .as_ref()
                        .and_then(|member| member.native(nr, args));
                    let wait = self.syscall_stopped(task, None)?.next_state().await?;
                    self.arm_liteinst_wait(&wait);
                    // The native raw result remains in the actual registers;
                    // errno is a guest result, not a backend failure.
                    let _native_result = self
                        .status_to_result(wait, None, None, None, native)
                        .await?;
                    task = self.assume_stopped();
                }
                let completion = self.source_observer.lock().unwrap().finish_tool(&task)?;
                if let Some(completion) = completion {
                    self.timer.complete_tool_step(completion);
                }
            }

            if let Some(resume_rip) = liteinst_resume_rip {
                let mut regs = task
                    .getregs()
                    .tracee_context(tid, "read registers before LiteInst tail resume")?;
                *regs.ip_mut() = resume_rip;
                task.setregs(&regs)
                    .tracee_context(tid, "resume after displaced LiteInst window")?;
            }

            #[cfg(test)]
            if self.liteinst_runtime.lock().unwrap().phase == LiteinstRuntimePhase::Waiting
                && let Some(queue_once) = self
                    .global_state
                    .liteinst_runtime
                    .as_ref()
                    .and_then(|runtime| runtime.queue_pending_signal_once.as_ref())
                && queue_once.swap(false, Ordering::SeqCst)
            {
                self.pending_signal = Some(Signal::SIGUSR1);
            }
            let sig = self.take_pending_signal_for_resume(
                LiteinstActivationOperation::ResumeAfterSeccompStop,
            )?;
            let running = self
                .resume_stopped(task, sig)
                .tracee_context(tid, "resume after seccomp stop")?;
            let wait_result = running.next_state().await;
            let wait = wait_result.tracee_context(tid, "wait after seccomp resume")?;
            tracing::trace!(
                target: "reverie_ptrace::syscall",
                "completed guest syscall interception"
            );
            Ok(wait)
        }
        .instrument(span)
        .await
    }

    /// The LiteInst config, but only when this task is the session root.
    ///
    /// The root-stop lease and the fail-closed cleanup guard are owned by the
    /// single spawned root TID. `tid == pid` is NOT that predicate: a forked
    /// child is its own thread-group leader and would otherwise take the
    /// root's shared lease, whose `root_tid` check then rejects every
    /// transition with `EINVAL`.
    fn liteinst_root_runtime(&self, task: &Stopped) -> Option<&LiteinstRuntimeConfig> {
        let runtime = self.liteinst_root_config()?;
        (Some(&task.pid()) == runtime.root_tid.get()).then_some(runtime)
    }

    /// The LiteInst config, but only when *this task* is the session root.
    fn liteinst_root_config(&self) -> Option<&LiteinstRuntimeConfig> {
        let runtime = self.global_state.liteinst_runtime.as_ref()?;
        (Some(&self.tid()) == runtime.root_tid.get()).then_some(runtime)
    }

    fn liteinst_root_stop_slot(
        &self,
        task: &Stopped,
    ) -> Option<Arc<StdMutex<Option<HeldRootStop>>>> {
        if self.ordinary_failure_enabled() {
            Some(self.ordinary_held_stop.clone())
        } else {
            self.liteinst_root_runtime(task)
                .map(|runtime| Arc::clone(&runtime.held_root_stop))
        }
    }

    fn liteinst_root_stop_armer(&self, task: &Stopped) -> Option<LiteinstRootStopArmer> {
        let slot = self.liteinst_root_stop_slot(task)?;
        Some(LiteinstRootStopArmer {
            root_tid: task.pid(),
            held_root_stop: slot,
        })
    }

    pub(crate) fn arm_liteinst_root_stop(&self, task: &Stopped, event: &Event) {
        let Some(armer) = self.liteinst_root_stop_armer(task) else {
            return;
        };
        armer
            .arm(task, event)
            .expect("rearmed an undisarmed or mismatched root stop lease");
    }

    /// Gives the cleanup guard ownership of a newborn tracee's wait statuses.
    ///
    /// This is deliberately NOT root-scoped, unlike the root-stop lease: the
    /// guard has to be able to reap the whole descendant tree, and
    /// `handle_new_task` requires the entry to exist for every child it sees.
    /// A grandchild is reported to its own non-root parent, so scoping this to
    /// the root leaves it unregistered.
    fn register_liteinst_newborn(&self, task: &Stopped, event: &Event) {
        if self.ordinary_failure_enabled() {
            // Ordinary custody is captured synchronously in handle_new_task,
            // after notifier registration has bound the exact child. Calling
            // exit_event here would perform that registration too early and
            // consume its authoritative failure.
            let _ = (task, event);
            return;
        }
        let Some(runtime) = self.global_state.liteinst_runtime.as_ref() else {
            return;
        };
        if let Event::NewChild(op, child) = event {
            runtime
                .newborn_tracees
                .lock()
                .unwrap()
                .entry(child.pid())
                .or_insert_with(|| NewbornTracee::from_event(task.pid(), *op, child));
        }
    }

    fn arm_liteinst_wait(&self, wait: &Wait) {
        if let Wait::Stopped(task, event) = wait {
            self.register_liteinst_newborn(task, event);
            self.arm_liteinst_root_stop(task, event);
        }
    }

    fn ensure_liteinst_wait(&self, wait: &Wait) {
        if let Wait::Stopped(task, event) = wait {
            self.register_liteinst_newborn(task, event);
            let Some(armer) = self.liteinst_root_stop_armer(task) else {
                return;
            };
            armer
                .ensure(task, event)
                .expect("run-loop stop mismatched its armed root lease");
        }
    }

    fn lease_liteinst_root_stop(&self, task: Stopped) -> RootStopLease {
        let slot = self.liteinst_root_stop_slot(&task);
        RootStopLease::new(task, slot)
    }

    fn original_nonleader(
        &self,
        tid: Pid,
        terminal: impl FnOnce() -> TerminalCleanup,
    ) -> Option<Arc<TerminalCleanup>> {
        if self.ordinary_failure_enabled() && !self.is_main_thread() {
            let terminal = terminal();
            self.ordinary_exec
                .lock()
                .unwrap()
                .get(&self.tid())
                .and_then(|slot| {
                    (tid == self.tid()
                        && slot.stop.terminal.same_generation(&terminal)
                        && terminal.thread_group_id() == Ok(self.pid()))
                    .then_some(Arc::new(terminal))
                })
        } else {
            None
        }
    }

    fn original_source_ioctl_matches(&self, task: &Stopped) -> bool {
        #[cfg(target_arch = "x86_64")]
        if let Some(context) = self.source_context(Some(task)) {
            return context.pending_ioctl_matches(task);
        }
        false
    }

    #[cfg(target_arch = "x86_64")]
    fn source_context(&self, task: Option<&Stopped>) -> Option<source_observation::Context> {
        let tool = Arc::clone(&self.process_state);
        let global = Arc::clone(&self.global_state.gs_ref);
        Some(source_observation::Context {
            state: Arc::clone(&self.source_observer),
            member: self.cohort.clone()?,
            epoch: Arc::clone(&self.global_state.fatal_session.source_epoch),
            subscriptions: Arc::clone(&self.global_state.subscriptions),
            armer: task.and_then(|task| self.liteinst_root_stop_armer(task)),
            ioctl_classifier: Arc::new(move |entry| {
                tool.classify_original_source_ioctl(global.as_ref(), entry)
            }),
        })
    }

    fn task_running(
        &self,
        running: Running,
        cohort_operation: Option<source_cohort::ResumeOperation>,
    ) -> TaskRunning {
        let original_nonleader =
            self.original_nonleader(running.pid(), || running.terminal_cleanup());
        TaskRunning {
            running,
            original_nonleader,
            cohort_operation,
            #[cfg(target_arch = "x86_64")]
            observation: self.source_context(None),
            #[cfg(target_arch = "x86_64")]
            administrative: false,
            #[cfg(target_arch = "x86_64")]
            precise: None,
        }
    }

    fn resume_stopped<T: Into<Option<Signal>>>(
        &self,
        task: Stopped,
        signal: T,
    ) -> Result<TaskRunning, TraceError> {
        if self.global_state.fatal_session.is_failed() {
            return Err(Errno::ECANCELED.into());
        }
        #[cfg(target_arch = "x86_64")]
        if let Some(context) = self.source_context(Some(&task)) {
            let (running, operation) = context.resume(task, signal.into())?;
            let mut running = self.task_running(running, operation);
            running.observation = Some(context);
            running.administrative = true;
            return Ok(running);
        }
        self.global_state
            .fatal_session
            .source_epoch
            .observe_resume(&task);
        let operation = self.cohort.as_ref().and_then(|member| {
            member.before_resume_classified(&task, false, self.original_source_ioctl_matches(&task))
        });
        self.lease_liteinst_root_stop(task)
            .resume(signal)
            .map(|running| self.task_running(running, operation))
    }

    fn step_stopped<T: Into<Option<Signal>>>(
        &self,
        task: Stopped,
        signal: T,
    ) -> Result<TaskRunning, TraceError> {
        if self.global_state.fatal_session.is_failed() {
            return Err(Errno::ECANCELED.into());
        }
        #[cfg(target_arch = "x86_64")]
        if let Some(context) = self.source_context(Some(&task)) {
            // Remaining callers require the real kernel SINGLESTEP stop
            // protocol (debugger/legacy parent). Do not claim source coverage
            // across that execution. Ordinary root startup and timer stepping
            // have explicit SYSCALL paths and never reach this fallback.
            context.close();
        }
        self.global_state
            .fatal_session
            .source_epoch
            .observe_resume(&task);
        let operation = self.cohort.as_ref().and_then(|member| {
            member.before_resume_classified(&task, false, self.original_source_ioctl_matches(&task))
        });
        self.lease_liteinst_root_stop(task)
            .step(signal)
            .map(|running| self.task_running(running, operation))
    }

    fn syscall_stopped<T: Into<Option<Signal>>>(
        &self,
        task: Stopped,
        signal: T,
    ) -> Result<TaskRunning, TraceError> {
        self.syscall_stopped_owned(task, signal, None)
    }

    fn syscall_stopped_owned<T: Into<Option<Signal>>>(
        &self,
        task: Stopped,
        signal: T,
        peers: Option<&source_cohort::NativePeers>,
    ) -> Result<TaskRunning, TraceError> {
        if self.global_state.fatal_session.is_failed() {
            return Err(Errno::ECANCELED.into());
        }
        #[cfg(target_arch = "x86_64")]
        if let Some(context) = self.source_context(Some(&task)) {
            context.raw_resume(&task)?;
        }
        self.global_state
            .fatal_session
            .source_epoch
            .observe_resume_classified(&task, self.original_source_ioctl_matches(&task));
        let operation = self.cohort.as_ref().and_then(|member| {
            member.before_resume_classified(&task, false, self.original_source_ioctl_matches(&task))
        });
        if let Some(peers) = peers {
            peers.require_resume_owner(operation.as_ref())?;
        }
        self.lease_liteinst_root_stop(task)
            .syscall(signal)
            .map(|running| self.task_running(running, operation))
    }

    #[cfg(target_arch = "x86_64")]
    fn sysemu_from_exit_stopped(&self, task: Stopped) -> Result<TaskRunning, TraceError> {
        if self.global_state.fatal_session.is_failed() {
            return Err(Errno::ECANCELED.into());
        }
        if !matches!(
            task.syscall_stop_info()?,
            safeptrace::SyscallStopInfo::Exit { .. }
        ) {
            return Err(Errno::EPROTO.into());
        }
        if let Some(context) = self.source_context(Some(&task)) {
            context.raw_resume(&task)?;
        }
        self.global_state
            .fatal_session
            .source_epoch
            .observe_resume_classified(&task, self.original_source_ioctl_matches(&task));
        let operation = self.cohort.as_ref().and_then(|member| {
            member.before_resume_classified(&task, false, self.original_source_ioctl_matches(&task))
        });
        self.lease_liteinst_root_stop(task)
            .sysemu_from_exit()
            .map(|running| self.task_running(running, operation))
    }

    async fn dispatch_new_task(
        &mut self,
        op: ChildOp,
        parent: Stopped,
        child: Running,
        context: Option<libc::user_regs_struct>,
        child_context: Option<libc::user_regs_struct>,
        observation: Option<(Sysno, SyscallArgs)>,
    ) -> Result<(Wait, Option<i64>), TraceError> {
        #[cfg(test)]
        if let Some(sender) = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.pause_before_new_task.as_ref())
        {
            let _ = sender.send(child.pid());
            future::pending::<()>().await;
        }
        self.handle_new_task(op, parent, child, context, child_context, observation)
            .await
    }

    fn fail_newborn_custody(
        &self,
        creator: Pid,
        child: Pid,
        phase: &'static str,
        error: Errno,
    ) -> ! {
        self.fail_newborn_custody_detail(
            creator,
            child,
            phase,
            &format!("errno={}", error.into_raw()),
        )
    }

    fn fail_newborn_custody_detail(
        &self,
        creator: Pid,
        child: Pid,
        phase: &'static str,
        error: &str,
    ) -> ! {
        self.global_state
            .gs_ref
            .report_backend_failure(reverie::BackendFailure {
                pid: self.pid(),
                tid: self.tid(),
                phase,
            });
        child_custody_is_fatal(creator, child, phase, error)
    }

    async fn finish_newborn_terminal(mut self, creator: Pid, status: ExitStatus) -> ExitStatus {
        if let TaskTimer::Terminal(previous) = &self.timer {
            assert_eq!(*previous, status, "newborn terminal status changed");
        }
        // Startup retirement is unsupported by this inactive history. Refuse
        // it permanently; original owners still perform every physical cleanup.
        self.global_state.fatal_session.source_cohort.fail();
        #[cfg(test)]
        source_cohort::startup_tests::terminal_branch(&self, status);
        #[cfg(all(test, cohort_final_test))]
        source_cohort::final_tests::terminal_branch(&self, status);
        // A later startup death can leave an opened timer. Retire it before
        // terminal callbacks; those callbacks have no Guest/timer interface.
        self.timer = TaskTimer::Terminal(status);
        let id = self.tid();
        let pid = self.pid();
        let global = self.global_state.clone();
        self.observe_thread_terminal(status);
        if let Err(error) = self.tool_exit(status).await {
            const PHASE: &str = "newborn_terminal_tool_cleanup";
            global
                .gs_ref
                .report_backend_failure(reverie::BackendFailure {
                    pid,
                    tid: id,
                    phase: PHASE,
                });
            child_custody_is_fatal(creator, id, PHASE, &error.to_string());
        }
        status
    }

    async fn handle_new_task(
        &mut self,
        op: ChildOp,
        parent: Stopped,
        child: Running,
        context: Option<libc::user_regs_struct>,
        child_context: Option<libc::user_regs_struct>,
        observation: Option<(Sysno, SyscallArgs)>,
    ) -> Result<(Wait, Option<i64>), TraceError> {
        if let Some(runtime) = self.global_state.liteinst_runtime.clone() {
            runtime.multi_task.store(true, Ordering::Release);
            let newborn_tracees = Arc::clone(&runtime.newborn_tracees);
            let child_pid = child.pid();
            let registration_error = {
                let newborns = newborn_tracees.lock().unwrap();
                let Some(newborn) = newborns.get(&child_pid) else {
                    drop(newborns);
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::NewbornRegistration,
                        Error::runtime(
                            self.tid(),
                            "register LiteInst newborn tracee",
                            format!("newborn {child_pid} event ownership is absent"),
                        ),
                    );
                    return Err(Errno::ESRCH.into());
                };
                newborn.registration_error()
            };
            if let Some(error) = registration_error {
                self.record_liteinst_failure(
                    LiteinstActivationFailureReason::NewbornRegistration,
                    Error::runtime(
                        self.tid(),
                        "register LiteInst newborn tracee",
                        format!("newborn {child_pid} registration failed: {error}"),
                    ),
                );
                return Err(error.into());
            }
            let child_identity =
                match TraceeIdentity::capture_event_child(child_pid, parent.pid(), op) {
                    Ok(identity) => identity,
                    Err(error) => {
                        self.record_liteinst_failure(
                            LiteinstActivationFailureReason::NewbornIdentity,
                            Error::runtime(
                                self.tid(),
                                "capture LiteInst newborn identity",
                                format!("newborn {child_pid} identity capture failed: {error}"),
                            ),
                        );
                        return Err(error.into());
                    }
                };
            if op == ChildOp::Vfork {
                // A vfork child borrows the parent's memory and suspends it
                // until the child execs or exits. Bind and terminate this exact
                // child generation before returning the refusal; otherwise the
                // parent remains kernel-frozen and orderly task cleanup cannot
                // reach the session-level guard.
                self.record_liteinst_failure(
                    LiteinstActivationFailureReason::VforkUnsupported,
                    Error::runtime(
                        self.tid(),
                        "refuse vfork under the LiteInst hybrid",
                        format!(
                            "vfork child of {} refused: exec cannot preserve the preload runtime",
                            parent.pid()
                        ),
                    ),
                );
            }
            {
                let mut newborns = newborn_tracees.lock().unwrap();
                let Some(newborn) = newborns.get_mut(&child_pid) else {
                    drop(newborns);
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::NewbornRegistration,
                        Error::runtime(
                            self.tid(),
                            "store LiteInst newborn identity",
                            format!(
                                "newborn {child_pid} event ownership disappeared before identity storage"
                            ),
                        ),
                    );
                    return Err(Errno::ESRCH.into());
                };
                newborn.set_identity(child_identity);
            }
            #[cfg(test)]
            if let Some(sender) = self
                .global_state
                .liteinst_runtime
                .as_ref()
                .and_then(|runtime| runtime.pause_new_task.as_ref())
            {
                let _ = sender.send(child.pid());
                if self
                    .global_state
                    .liteinst_runtime
                    .as_ref()
                    .is_some_and(|runtime| runtime.pause_after_new_task)
                {
                    future::pending::<()>().await;
                }
            }
            #[cfg(test)]
            if runtime.fail_new_task {
                return Err(Errno::ENOTSUPP.into());
            }
            if op == ChildOp::Vfork {
                let termination = newborn_tracees
                    .lock()
                    .unwrap()
                    .get(&child_pid)
                    .ok_or(Errno::ESRCH)?
                    .terminate_vfork_child();
                termination?;
                return Err(Errno::ENOTSUPP.into());
            }
            // Any other new task proceeds under the ordinary ptrace lifecycle.
            // Root cleanup still owns every process child and CLONE_THREAD TID
            // if a later LiteInst failure does fail closed: it signals only
            // group-leader pidfds and drains every bound notifier generation on
            // the ptracer thread.
        }
        tracing::debug!(
            "[scheduler] handling fork from parent {} to child {}: {:?}",
            parent.pid(),
            child.pid(),
            op
        );

        // Both descriptions are already bound to the exact NewChild event.
        // Registration uses the same notifier/FIFO as subsequent next_state;
        // never select shared process state from PTRACE_EVENT_CLONE alone.
        let creator = parent.pid();
        if creator != self.tid() || child.pid() == creator {
            self.fail_newborn_custody(
                creator,
                child.pid(),
                "newborn_creator_identity",
                Errno::ECHILD,
            );
        }
        let child_cleanup = child.terminal_cleanup();
        if let Err(error) = register_newborn_wait(|| child_cleanup.ensure_registered()) {
            self.fail_newborn_custody(creator, child.pid(), "register_newborn_wait", error);
        }
        if self.ordinary_failure_enabled() {
            self.global_state.fatal_session.capture(creator, op, &child);
        }
        #[cfg(test)]
        if matches!(op, ChildOp::Fork | ChildOp::Vfork) && self.ordinary_failure_enabled() {
            let pause = FATAL_FORK_PAUSE.with(|slot| slot.borrow().clone());
            if let Some(pause) = pause {
                // Retain this registered real Event::NewChild capability for
                // emergency test cleanup before constructing a child Tool.
                let stat = std::fs::read_to_string(format!("/proc/{}/stat", child.pid())).unwrap();
                let start = stat
                    .rsplit_once(") ")
                    .unwrap()
                    .1
                    .split_whitespace()
                    .nth(19)
                    .unwrap()
                    .parse()
                    .unwrap();
                use std::os::unix::fs::MetadataExt;
                let inode = std::fs::metadata(format!("/proc/{}", child.pid()))
                    .unwrap()
                    .ino();
                *pause.generation.lock().unwrap() = Some((start, inode));
                *pause.child.lock().unwrap() = Some(child);
                pause.ready.notify_waiters();
                return future::pending().await;
            }
        }
        let parent_cleanup = parent.terminal_cleanup();
        let parent_tgid = parent_cleanup.thread_group_id().unwrap_or_else(|error| {
            self.fail_newborn_custody(creator, child.pid(), "retained_creator_tgid", error)
        });
        let child_tgid = child_cleanup.thread_group_id().unwrap_or_else(|error| {
            self.fail_newborn_custody(creator, child.pid(), "retained_newborn_tgid", error)
        });
        let child_kind = classify_native_child(self.pid(), parent_tgid, child.pid(), child_tgid)
            .unwrap_or_else(|error| {
                self.fail_newborn_custody(creator, child.pid(), "classify_newborn_tgid", error)
            });
        // The LiteInst prelude above is deliberately unchanged: its existing
        // newborn map owns pre-admission cancellation/refusal. From this common
        // admitted boundary, no await may take the native child out of this slot.
        let id = child.pid();
        assert!(self.pending_child.is_none(), "overlapping child dispatch");
        let (cohort, parent_completion) = match self
            .cohort
            .as_ref()
            .and_then(|member| member.retain_child(&child_cleanup))
        {
            Some((child, parent)) => (Some(child), Some(parent)),
            None => (None, None),
        };
        self.pending_child = Some(PendingChild::Native(Box::new(NativeChild {
            id,
            creator,
            creator_cleanup: parent_cleanup,
            cleanup: child_cleanup,
            kind: child_kind,
            initial: InitialChildWait::Waiting(Box::pin(Self::prepare_newborn(
                child,
                match child_kind {
                    ChildTaskKind::Thread => self.pid(),
                    ChildTaskKind::Process => id,
                },
            ))),
            // This decision belongs to the retained native child, not a
            // callback's mutable parent state after a canceled await.
            admission_required: self
                .process_state
                .requires_native_child_admission(&self.thread_state),
            admission_done: false,
            child_restore_context: child_context.or(context),
            cohort,
        })));
        #[cfg(test)]
        if let Some(PendingChild::Native(native)) = &self.pending_child {
            newborn_startup_tests::retained(
                native,
                parent.terminal_cleanup(),
                &self.ntasks,
                &self.ndaemons,
            );
            parent_completion_tests::retained(
                native,
                parent.terminal_cleanup(),
                &self.ntasks,
                &self.ndaemons,
            );
            if preconstruction_tests::creator_exit_after_native_retention(native) {
                // Only a real creator terminal event can cancel this dispatch.
                // The production completion path must recover the retained child.
                future::pending::<()>().await;
            }
        }
        #[cfg(test)]
        if newborn_startup_tests::resume_creator_before_child(self.tid()) {
            // The fixture transfers this original stop once and executes a real
            // leader-only SYS_exit. Its ordinary EXIT future cancels dispatch.
            drop(parent.resume(None)?);
            future::pending::<()>().await;
            unreachable!("test dispatch is canceled only by actual creator EXIT");
        }
        let debugger = self
            .materialize_pending_child()
            .await
            .expect("newly admitted native child must materialize exactly once");
        self.publish_pending_child().await;

        // The existing GDB stop/request/resume path remains unchanged. Its
        // step boundary does not supply the optional authenticated syscall-exit
        // receipt; no consumer may infer this knowledge or require it for that
        // preexisting path. Non-observing Tools also keep the original path.
        #[cfg(target_arch = "x86_64")]
        let source_birth = self.source_observer.lock().unwrap().birth();
        #[cfg(not(target_arch = "x86_64"))]
        let source_birth = None;
        let completion_observation = observation.or(source_birth);
        let completion_owner = (completion_observation.is_some() && !self.attached_by_gdb)
            .then(|| parent.terminal_cleanup());
        let (parent, native_return) = if let Some(completion_observation) = completion_observation
            && !self.attached_by_gdb
        {
            #[cfg(test)]
            parent_completion_tests::before_observation(&parent, context.is_some());
            match self
                .observe_child_syscall_return(
                    parent,
                    op,
                    id,
                    completion_observation,
                    observation.is_some(),
                )
                .await
            {
                Ok((parent, raw)) => (parent, Some(raw)),
                Err(error) => {
                    self.handle_parent_completion_error(
                        error,
                        completion_owner
                            .as_ref()
                            .expect("observed parent generation retained"),
                    )
                    .await
                }
            }
        } else {
            (parent, None)
        };

        // The parent may have died after NewChild. That does not revoke the
        // surviving child's inherited state, executor, or ordinary join owner.
        // An observed return was captured before either RAX or frame rewriting.
        // TODO-HUMAN-REVIEW(PR-103): Review rewritten clone parent/child restoration.
        if let Some(context) = context {
            #[cfg(test)]
            if native_return.is_some() {
                parent_completion_tests::before_restoration(&parent);
            }
            let restored = restore_context(
                &parent,
                context,
                Some(native_return.unwrap_or(i64::from(id.as_raw())) as u64),
                child_context.is_some(),
            );
            if let Err(error) = restored {
                if native_return.is_some() {
                    self.handle_parent_completion_error(
                        error,
                        completion_owner
                            .as_ref()
                            .expect("observed parent generation retained"),
                    )
                    .await;
                }
                return Err(error);
            }
        }
        if let Some(raw) = native_return {
            if let Some(completion) = parent_completion {
                completion.returned_and_restored(raw);
            }
            #[cfg(target_arch = "x86_64")]
            self.source_observer.lock().unwrap().birth_restored();
            #[cfg(test)]
            source_cohort::startup_tests::parent_restored(&parent, id, native_return);
            #[cfg(all(test, cohort_final_test))]
            source_cohort::final_tests::parent_restored(&parent, id, native_return);
            // PTRACE_SYSCALL already completed the native operation and stopped
            // before another guest instruction. Do not append the old internal
            // single-step, which would now execute a guest instruction.
            return Ok((Wait::Stopped(parent, Event::Syscall), native_return));
        }
        let parent_regs = parent.getregs()?;
        if self.attached_by_gdb {
            // NB: We report T05;create event (for clone). However gdbserver
            // from binutils-gdb doesn't report it, even after toggling
            // QThreadEvents, as mentioned in https://sourceware.org/gdb/onlinedocs/gdb/General-Query-Packets.html#QThreadEvents
            // We report `create` event anyway.
            self.notify_gdb_stop(StopReason::new_task(
                self.tid(),
                self.pid(),
                id,
                parent_regs.into(),
                op,
                debugger.request_tx,
                debugger.resume_tx,
                Some(debugger.stop_rx),
            ))
            .await?;
            // We just reported a new event, wait for gdb resume.
            let running = self
                .await_gdb_resume(parent, ExpectedGdbResume::StepOnly)
                .await?;
            // NB: We could potentially hit a breakpoint after above resume,
            // make sure we don't miss the breakpoint and await for gdb
            // resume (once again). This is possible because result of
            // handle_new_task in status_to_result is ignored, while it could be
            // a valid state like SIGTRAP, which could be a breakpoint is hit.
            running
                .next_state()
                .and_then(|wait| self.check_swbreak(wait))
                .await
                .map(|wait| (wait, None))
        } else {
            // This nested parent step consumes the root-stop lease, so the
            // resulting stop has to re-arm it before returning to a caller
            // that will transition the root again. Every other nested handler
            // does the same; this one only looks new because the whole
            // new-task path used to be unreachable under LiteInst.
            let wait = self.step_stopped(parent, None)?.next_state().await?;
            self.arm_liteinst_wait(&wait);
            Ok((wait, None))
        }
    }

    /// Natural death of this exact creator belongs to the existing run-owned
    /// EXIT/final continuation. Keep its borrowed handler pending so that owner
    /// can cancel it; do not reap, create another waiter, or fail healthy children.
    /// Other errors still enter the shared authentication-failure cleanup path.
    async fn handle_parent_completion_error(
        &self,
        error: TraceError,
        owner: &TerminalCleanup,
    ) -> ! {
        if let TraceError::Died(zombie) = &error
            && zombie.pid() == self.tid()
            && owner.same_generation(&zombie.terminal_cleanup())
        {
            #[cfg(test)]
            parent_completion_tests::deferred_died(self.tid(), owner);
            future::pending::<()>().await;
            unreachable!("the original EXIT owner cancels this borrowed handler");
        }
        self.fail_parent_syscall_completion(error).await
    }

    /// Keep the current syscall future pending until the original run-owned
    /// failure/EXIT branch consumes this generation and all published children.
    /// Returning Err here would drop that EXIT future in the ordinary tracer.
    async fn fail_parent_syscall_completion(&self, error: TraceError) -> ! {
        self.global_state
            .parent_completion_failure
            .record(self.tid(), &error);
        self.global_state
            .gs_ref
            .report_backend_failure(reverie::BackendFailure {
                pid: self.pid(),
                tid: self.tid(),
                phase: "native parent syscall completion could not be authenticated",
            });
        // The shared Tool acknowledgement precedes waking any task cleanup.
        self.global_state
            .parent_completion_failure
            .changed
            .notify_waiters();
        future::pending().await
    }

    /// Finish the exact creating syscall without executing another guest
    /// instruction. ChildCreated is already retained and the ordinary child
    /// owner is published, including the child whose progress releases vfork.
    async fn observe_child_syscall_return(
        &mut self,
        parent: Stopped,
        op: ChildOp,
        child: Pid,
        observation: (Sysno, SyscallArgs),
        observe_tool: bool,
    ) -> Result<(Stopped, i64), TraceError> {
        let creator = parent.pid();
        let generation = parent.terminal_cleanup();
        let (nr, _) = observation;
        if creator != self.tid() || parent.getregs()?.orig_syscall() as i32 != nr as i32 {
            return Err(Errno::EPROTO.into());
        }
        let mut running = self.syscall_stopped(parent, None)?;
        let mut vfork_done = false;
        loop {
            let wait = running.next_state().await?;
            self.arm_liteinst_wait(&wait);
            match wait {
                Wait::Stopped(parent, event) => {
                    if parent.pid() != creator
                        || !generation.same_generation(&parent.terminal_cleanup())
                    {
                        return Err(Errno::ECHILD.into());
                    }
                    match event {
                        Event::Syscall => {
                            let raw = parent.syscall_exit_result()?;
                            let regs = parent.getregs()?;
                            if regs.orig_syscall() as i32 != nr as i32 || regs.ret() as i64 != raw {
                                return Err(Errno::EPROTO.into());
                            }
                            self.observe_injected_syscall(
                                observe_tool.then_some(observation),
                                InjectedSyscallEvent::ChildSyscallReturned { child, raw },
                            );
                            return Ok((parent, raw));
                        }
                        Event::VforkDone if op == ChildOp::Vfork && !vfork_done => {
                            vfork_done = true;
                            running = self.syscall_stopped(parent, None)?;
                        }
                        // No signal, entry, duplicate child event or other stop
                        // is converted to a parent result. Actual EXIT/death is
                        // consumed by the original run-owned terminal future.
                        _ => return Err(Errno::EPROTO.into()),
                    }
                }
                Wait::Exited(_, status) => self.exit(status).await,
            }
        }
    }

    /// Prepare live resources inside the retained initial future. A task whose
    /// actual final wait is already known still inherits Tool state, but never
    /// opens a perf timer for a vanished numeric TID.
    async fn prepare_newborn(child: Running, process: Pid) -> Result<PreparedNewborn, TraceError> {
        let id = child.pid();
        let cleanup = child.terminal_cleanup();
        #[cfg(test)]
        let child = newborn_startup_tests::prepare_input(child).await;
        let prepared = async {
            match Self::wait_newborn_initial(child).await {
                Ok(Wait::Exited(exited, status)) => {
                    if exited != id {
                        return Err(Errno::ECHILD.into());
                    }
                    Ok(PreparedNewborn::Terminal { id, status })
                }
                Err(TraceError::Died(zombie)) => {
                    if zombie.pid() != id {
                        return Err(Errno::ECHILD.into());
                    }
                    let status = zombie.reap().await?;
                    Ok(PreparedNewborn::Terminal { id, status })
                }
                Err(error) => Err(error),
                Ok(Wait::Stopped(child, event)) => {
                    if child.pid() != id || !cleanup.same_generation(&child.terminal_cleanup()) {
                        return Err(Errno::ECHILD.into());
                    }
                    if event == Event::Exit {
                        let status =
                            Self::finish_newborn_startup_exit(child, Errno::EPROTO.into()).await?;
                        return Ok(PreparedNewborn::Terminal { id, status });
                    }
                    if event != Event::Signal(Signal::SIGSTOP) {
                        return Err(Errno::EPROTO.into());
                    }
                    #[cfg(test)]
                    let child = match newborn_startup_tests::startup_error_case(child).await {
                        Ok(child) => child,
                        Err(prepared) => return Ok(*prepared),
                    };
                    #[cfg(test)]
                    newborn_startup_tests::before_timer(&child).await;
                    let timer = Timer::try_new(process, id);
                    #[cfg(test)]
                    newborn_startup_tests::timer_result(id, &timer);
                    match timer {
                        Ok(timer) => Ok(PreparedNewborn::Live {
                            child,
                            event,
                            timer: Box::new(timer),
                        }),
                        Err(Errno::ESRCH) => {
                            let status =
                                Self::finish_newborn_startup_exit(child, Errno::ESRCH.into())
                                    .await?;
                            Ok(PreparedNewborn::Terminal { id, status })
                        }
                        Err(error) => Err(error.into()),
                    }
                }
            }
        }
        .await;
        #[cfg(test)]
        let prepared = newborn_startup_tests::preparation_result(id, prepared).await;
        prepared
    }

    /// A startup syscall error is not death evidence. Retain its original
    /// stopped generation and consume only its actual EXIT/final wait. A claim
    /// held or revoked elsewhere without a final wait preserves the failure.
    async fn finish_newborn_startup_exit(
        child: Stopped,
        original_error: TraceError,
    ) -> Result<ExitStatus, TraceError> {
        let id = child.pid();
        let cleanup = child.terminal_cleanup();
        if let Some(Ok(status)) = cleanup.observed_terminal() {
            #[cfg(all(test, cohort_final_test))]
            source_cohort::final_tests::already_final(&cleanup, status);
            return Ok(status);
        }
        let stopped = match child.exit_event().await {
            Ok(stopped) => stopped,
            Err(TraceError::Errno(Errno::ECHILD | Errno::EALREADY)) => {
                return match cleanup.observed_terminal() {
                    Some(Ok(status)) => Ok(status),
                    _ => Err(original_error),
                };
            }
            Err(error) => return Err(error),
        };
        if stopped.pid() != id || !cleanup.same_generation(&stopped.terminal_cleanup()) {
            return Err(Errno::ECHILD.into());
        }
        // The old initial-stop token is superseded by this single EXIT claim.
        // It is never resumed or used for a second final-wait future.
        drop(child);
        match Self::wait_after_exit_event(stopped, None).await {
            Ok(Wait::Exited(exited, status)) if exited == id => Ok(status),
            Ok(Wait::Exited(_, _)) => Err(Errno::ECHILD.into()),
            Ok(Wait::Stopped(_, _)) => Err(Errno::EPROTO.into()),
            Err(TraceError::Died(zombie)) => {
                if zombie.pid() != id {
                    return Err(Errno::ECHILD.into());
                }
                zombie.reap().await
            }
            Err(error) => Err(error),
        }
    }

    /// Own both ways an unstarted child can stop. The complete future remains
    /// in PendingChild, including a claimed EXIT stop and its final wait, if
    /// the creator's dispatch is canceled.
    async fn wait_newborn_initial(child: Running) -> Result<Wait, TraceError> {
        let id = child.pid();
        let cleanup = child.terminal_cleanup();
        let stopped = {
            let exit = child.exit_event().fuse();
            let initial = child.next_state();
            #[cfg(test)]
            let initial = newborn_startup_tests::deliver_initial(id, initial);
            let initial = initial.fuse();
            futures::pin_mut!(exit, initial);
            let stopped = futures::select_biased! {
                outcome = initial => {
                    // A final wait can already supersede an earlier queued
                    // SIGSTOP. Only an actual retained final status replaces it.
                    return match cleanup.observed_terminal() {
                        Some(Ok(status)) => Ok(Wait::Exited(id, status)),
                        Some(Err(error)) => Err(error),
                        None => outcome,
                    };
                },
                stopped = exit => match stopped {
                    Ok(stopped) => stopped,
                    // EALREADY also represents a competing claimant. Accept
                    // only the same generation's already published final wait;
                    // otherwise retain the original error, without waiting for
                    // another owner or returning a queued SIGSTOP as success.
                    Err(error @ TraceError::Errno(Errno::ECHILD | Errno::EALREADY)) => {
                        return match cleanup.observed_terminal() {
                            Some(Ok(status)) => Ok(Wait::Exited(id, status)),
                            _ => Err(error),
                        };
                    }
                    Err(error) => return Err(error),
                },
            };
            if stopped.pid() != id || !cleanup.same_generation(&stopped.terminal_cleanup()) {
                return Err(Errno::ECHILD.into());
            }

            #[cfg(test)]
            newborn_startup_tests::after_exit_claim(&stopped).await;

            // One notifier worker publishes the ordinary FIFO before its later
            // EXIT stop. SIGSTOP can have arrived between initial's Pending poll
            // and the EXIT claim above. Retire only that exact, now-superseded
            // initial notification; never resume its stale Stopped capability.
            match initial.as_mut().now_or_never() {
                Some(Ok(Wait::Stopped(earlier, Event::Signal(Signal::SIGSTOP)))) => {
                    if earlier.pid() != id || !cleanup.same_generation(&earlier.terminal_cleanup())
                    {
                        return Err(Errno::ECHILD.into());
                    }
                    #[cfg(test)]
                    newborn_startup_tests::stale_stop_discarded(&earlier);
                }
                Some(Ok(Wait::Stopped(_, _))) => return Err(Errno::EPROTO.into()),
                Some(outcome) => return outcome,
                None => {}
            }
            stopped
        };

        // A newborn is never the session root. Consume the sole EXIT-minted
        // capability through the existing transition; do not pass it to run(),
        // whose independent ExitFuture would try to claim it a second time.
        match Self::wait_after_exit_event(stopped, None).await {
            Ok(Wait::Stopped(_, _)) => Err(Errno::EPROTO.into()),
            Err(TraceError::Died(zombie)) => {
                let id = zombie.pid();
                Ok(Wait::Exited(id, zombie.reap().await?))
            }
            outcome => outcome,
        }
    }

    /// Finish the actor-owned initial wait before constructing inherited Tool
    /// state. Cancellation leaves both the wait/outcome and creator state owned.
    async fn materialize_pending_child(&mut self) -> Option<ChildDebugChannels> {
        let native = match self.pending_child.as_mut() {
            Some(PendingChild::Native(native)) => native,
            Some(PendingChild::Spawned(_, _)) | None => return None,
        };
        native.initial.observe().await;
        // This retained result remains in the slot until the synchronous
        // constructor/spawn handoff below. No PID lookup or repeated wait.
        let Some(PendingChild::Native(native)) = self.pending_child.as_ref() else {
            unreachable!("native child changed while its owner was borrowed");
        };
        let parent_tgid = native
            .creator_cleanup
            .thread_group_id()
            .unwrap_or_else(|error| {
                self.fail_newborn_custody(native.creator, native.id, "retained_creator_tgid", error)
            });
        let child_tgid = native.cleanup.thread_group_id().unwrap_or_else(|error| {
            self.fail_newborn_custody(native.creator, native.id, "retained_newborn_tgid", error)
        });
        let kind = classify_native_child(self.pid(), parent_tgid, native.id, child_tgid)
            .unwrap_or_else(|error| {
                self.fail_newborn_custody(native.creator, native.id, "classify_newborn_tgid", error)
            });
        assert_eq!(
            kind, native.kind,
            "held native child classification changed"
        );
        match &native.initial {
            InitialChildWait::Observed(Ok(PreparedNewborn::Live { child, event, .. })) => {
                if child.pid() != native.id
                    || !native.cleanup.same_generation(&child.terminal_cleanup())
                {
                    self.fail_newborn_custody(
                        native.creator,
                        native.id,
                        "newborn_initial_stop_identity",
                        Errno::ECHILD,
                    );
                }
                assert!(
                    *event == Event::Signal(Signal::SIGSTOP) || *event == Event::Exit,
                    "Got unexpected event {:?}",
                    event
                );
            }
            InitialChildWait::Observed(Ok(PreparedNewborn::Terminal { id, .. }))
                if *id != native.id =>
            {
                self.fail_newborn_custody(
                    native.creator,
                    native.id,
                    "newborn_final_wait_identity",
                    Errno::ECHILD,
                );
            }
            InitialChildWait::Observed(Err(TraceError::Died(zombie)))
                if zombie.pid() != native.id =>
            {
                self.fail_newborn_custody(
                    native.creator,
                    native.id,
                    "newborn_zombie_identity",
                    Errno::ECHILD,
                );
            }
            InitialChildWait::Observed(Err(TraceError::Errno(error))) => {
                self.fail_newborn_custody(
                    native.creator,
                    native.id,
                    "newborn_initial_wait_unavailable",
                    *error,
                );
            }
            InitialChildWait::Observed(_) => {}
            InitialChildWait::Waiting(_) => unreachable!("initial outcome was not retained"),
        }
        if native.admission_required && !native.admission_done {
            let id = native.id;
            let creator = native.creator;
            let terminal = match &native.initial {
                InitialChildWait::Observed(Ok(PreparedNewborn::Terminal { status, .. })) => {
                    Some(*status)
                }
                InitialChildWait::Observed(Ok(PreparedNewborn::Live { .. })) => None,
                InitialChildWait::Observed(Err(error)) => self.fail_newborn_custody_detail(
                    creator,
                    id,
                    "newborn_preparation_failed",
                    &error.to_string(),
                ),
                InitialChildWait::Waiting(_) => unreachable!("initial outcome was not retained"),
            };
            let pidfd = native
                .cleanup
                .duplicate_bound_thread_pidfd()
                .unwrap_or_else(|error| {
                    self.fail_newborn_custody(creator, id, "newborn_admission_pidfd", error)
                });
            // The same pending slot retains the prepared outcome and both
            // generations across cancellation of this borrowed callback.
            let outcome = self
                .process_state
                .admit_native_child(
                    creator,
                    id,
                    &self.global_state.gs_ref,
                    &mut self.thread_state,
                    std::os::fd::AsFd::as_fd(&pidfd),
                    terminal,
                )
                .await;
            if let Err(error) = outcome {
                self.fail_newborn_custody_detail(
                    creator,
                    id,
                    "newborn_physical_admission",
                    &error.to_string(),
                );
            }
            let Some(PendingChild::Native(native)) = self.pending_child.as_mut() else {
                unreachable!("child custody lost during admission");
            };
            native.admission_done = true;
        }
        #[cfg(test)]
        {
            let Some(PendingChild::Native(native)) = self.pending_child.as_ref() else {
                unreachable!("child custody lost after admission");
            };
            preconstruction_tests::creator_worker_drained_before_construction(native);
        }
        // Taking the stage is followed only by synchronous construction/spawn
        // and replacement. The spawned body owns the exact initial outcome.
        let Some(PendingChild::Native(native)) = self.pending_child.take() else {
            unreachable!("validated native child disappeared");
        };
        let NativeChild {
            id,
            creator,
            creator_cleanup: _creator_cleanup,
            cleanup: child_cleanup,
            kind: child_kind,
            initial,
            admission_required: _,
            admission_done: _,
            child_restore_context,
            cohort,
        } = *native;
        let (initial, timer, cohort) = match initial.into_observed() {
            Ok(prepared) => {
                let cohort = cohort.map(|membership| membership.prepared(&prepared));
                let (initial, timer) = prepared.into_parts();
                (initial, timer, cohort)
            }
            Err(error) => self.fail_newborn_custody_detail(
                creator,
                id,
                "newborn_preparation_failed",
                &error.to_string(),
            ),
        };
        let child_cleanup = Arc::new(child_cleanup);
        let terminal = (child_kind == ChildTaskKind::Process).then(|| Arc::clone(&child_cleanup));
        let mut child_task = match child_kind {
            ChildTaskKind::Thread => self.cloned(id, timer, cohort),
            ChildTaskKind::Process => self.forked(id, timer, cohort),
        };

        #[cfg(test)]
        newborn_startup_tests::constructed(&child_task);

        let (child_stop_tx, child_stop_rx) = mpsc::channel(1);
        child_task.gdb_stop_tx = Some(child_stop_tx);

        let daemonizer_rx = child_task.daemonizer_rx.take();
        let child_resume_tx = child_task.gdb_resume_tx.clone();
        let child_request_tx = child_task.gdb_request_tx.clone();
        let suspended = child_task.suspended.clone();

        // A panic anywhere in this body would otherwise be caught by tokio's
        // task harness and silently wedge the whole run; see
        // `guest_task_panic_is_fatal`. The body is built as its own future so
        // the catch sits at the task boundary and covers all of it.
        let panic_tid = id;
        let ordinary_failure = self
            .ordinary_failure_enabled()
            .then(|| self.fatal_session());
        let report_failure = ordinary_failure.clone();
        // Process children can retain real-parent adoption/reap work in the
        // final tree drain after this body returns. Only ordinary threads have
        // the complete continuation boundary certified by this private guard.
        let retirement = ordinary_failure
            .as_ref()
            .filter(|_| child_kind == ChildTaskKind::Thread)
            .and_then(|_| {
                child_task
                    .cohort
                    .as_ref()
                    .and_then(|member| member.ordinary_child(Arc::clone(&child_cleanup)))
            });
        let ordinary_group = ordinary_failure.as_ref().and_then(|session| {
            match session.subscribe_group(child_cleanup.as_ref()) {
                Ok(subscription) => Some(subscription),
                Err(error) => {
                    session.fail_at(
                        BackendFailure {
                            pid: self.pid(),
                            tid: id,
                            phase: "ptrace orphan group subscription",
                        },
                        error.into(),
                    );
                    None
                }
            }
        });
        // Heap-place the child operation before the catch/completion wrappers
        // capture it. Tokio's automatic boxing occurs after its by-value spawn
        // entry, which can already exhaust the container's small host stack.
        #[cfg(test)]
        let startup_counts = (
            Arc::clone(&child_task.ntasks),
            Arc::clone(&child_task.ndaemons),
        );
        #[cfg(test)]
        let completion_readback = Arc::clone(&child_cleanup);
        let body = Box::pin(async move {
            // waitid retries EINTR and notifier registration already owns this
            // exact generation. A genuine final wait can precede SIGSTOP; an
            // unavailable outcome must never become synthetic Exited(1).
            let (child, event) = match initial {
                Ok(Wait::Stopped(child, event)) => {
                    if child.pid() != id
                        || !child_cleanup.same_generation(&child.terminal_cleanup())
                    {
                        child_task.fail_newborn_custody(
                            creator,
                            id,
                            "newborn_initial_stop_identity",
                            Errno::ECHILD,
                        );
                    }
                    (child, event)
                }
                Ok(Wait::Exited(exited, status)) => {
                    if exited != id {
                        child_task.fail_newborn_custody(
                            creator,
                            id,
                            "newborn_final_wait_identity",
                            Errno::ECHILD,
                        );
                    }
                    let status = child_task.finish_newborn_terminal(creator, status).await;
                    #[cfg(test)]
                    source_cohort::startup_tests::callbacks_done(
                        &child_cleanup,
                        source_cohort::startup_tests::OwnerPath::Startup,
                        &startup_counts,
                    );
                    #[cfg(all(test, cohort_final_test))]
                    source_cohort::final_tests::callbacks_done(
                        &child_cleanup,
                        source_cohort::final_tests::OwnerPath::Startup,
                        &startup_counts,
                    );
                    return Ok(Some(status));
                }
                Err(TraceError::Died(zombie)) => {
                    if zombie.pid() != id {
                        child_task.fail_newborn_custody(
                            creator,
                            id,
                            "newborn_zombie_identity",
                            Errno::ECHILD,
                        );
                    }
                    let status = zombie.reap().await.unwrap_or_else(|error| {
                        child_task.fail_newborn_custody_detail(
                            creator,
                            id,
                            "newborn_final_wait_unavailable",
                            &error.to_string(),
                        )
                    });
                    let status = child_task.finish_newborn_terminal(creator, status).await;
                    #[cfg(test)]
                    source_cohort::startup_tests::callbacks_done(
                        &child_cleanup,
                        source_cohort::startup_tests::OwnerPath::Startup,
                        &startup_counts,
                    );
                    #[cfg(all(test, cohort_final_test))]
                    source_cohort::final_tests::callbacks_done(
                        &child_cleanup,
                        source_cohort::final_tests::OwnerPath::Startup,
                        &startup_counts,
                    );
                    return Ok(Some(status));
                }
                Err(TraceError::Errno(error)) => {
                    child_task.fail_newborn_custody(
                        creator,
                        id,
                        "newborn_initial_wait_unavailable",
                        error,
                    );
                }
            };

            assert!(
                event == Event::Signal(Signal::SIGSTOP) || event == Event::Exit,
                "Got unexpected event {:?}",
                event
            );

            child_task.arm_liteinst_root_stop(&child, &event);
            if let Some(context) = child_restore_context {
                // Restore context, but only if the child hasn't arrived at
                // `Event::Exit`.
                if event == Event::Signal(Signal::SIGSTOP) {
                    #[cfg(test)]
                    newborn_startup_tests::before_restore(&child, &child_task.timer).await;
                    #[cfg(test)]
                    source_cohort::startup_tests::before_restore(&child_task, &child).await;
                    #[cfg(all(test, cohort_final_test))]
                    source_cohort::final_tests::before_restore(&child_task, &child).await;
                    #[cfg(test)]
                    source_cohort::startup_tests::capture_before_restore(&child, &context);
                    let restored = restore_context(&child, context, None, false);
                    #[cfg(test)]
                    source_cohort::startup_tests::restore_result(&child, &restored);
                    #[cfg(all(test, cohort_final_test))]
                    source_cohort::final_tests::restore_result(&child, &restored);
                    #[cfg(test)]
                    let restored = newborn_startup_tests::restore_result(child.pid(), restored);
                    match restored {
                        Ok(()) => {
                            if let Some(member) = &child_task.cohort {
                                member.child_restored();
                            }
                            #[cfg(test)]
                            source_cohort::startup_tests::capture_adoption(&child_task, &child);
                        }
                        Err(error @ (TraceError::Died(_) | TraceError::Errno(Errno::ESRCH))) => {
                            // Keep construction and the original context order.
                            // A real death here still precedes thread-start; the
                            // spawned child body owns this terminal continuation.
                            let status = Self::finish_newborn_startup_exit(child, error)
                                .await
                                .unwrap_or_else(|error| {
                                    child_task.fail_newborn_custody_detail(
                                        creator,
                                        id,
                                        "newborn_context_terminal_unavailable",
                                        &error.to_string(),
                                    )
                                });
                            let status = child_task.finish_newborn_terminal(creator, status).await;
                            #[cfg(test)]
                            source_cohort::startup_tests::callbacks_done(
                                &child_cleanup,
                                source_cohort::startup_tests::OwnerPath::Startup,
                                &startup_counts,
                            );
                            #[cfg(all(test, cohort_final_test))]
                            source_cohort::final_tests::callbacks_done(
                                &child_cleanup,
                                source_cohort::final_tests::OwnerPath::Startup,
                                &startup_counts,
                            );
                            return Ok(Some(status));
                        }
                        Err(err) => {
                            tracing::error!(
                                tid = %child.pid(),
                                error = %err,
                                "failed to restore new tracee register context"
                            );
                            let origin = reverie::BackendFailure {
                                pid: child_task.pid(),
                                tid: id,
                                phase: "native child register restoration failed",
                            };
                            if let Some(session) = &ordinary_failure {
                                // The backend-owned fatal session is the
                                // mandatory fence. Its publication callback is
                                // advisory to cleanup, not the owner of this
                                // failure: the default Tool hook is a no-op.
                                session.fail_at(origin, anyhow::Error::new(err).into());
                            } else {
                                child_task
                                    .global_state
                                    .gs_ref
                                    .report_backend_failure(origin);
                                return Err(anyhow::Error::new(err).into());
                            }
                        }
                    }
                }
            } else if event == Event::Signal(Signal::SIGSTOP)
                && let Some(member) = &child_task.cohort
            {
                member.child_restored();
            }
            let tid = child.pid();
            let detach_held_root_stop = child_task
                .liteinst_root_config()
                .map(|runtime| Arc::clone(&runtime.held_root_stop));
            let parent_failure = Arc::clone(&child_task.global_state.parent_completion_failure);
            let ordinary_owned = ordinary_failure.is_some();
            let (result, completed_cleanup) = if ordinary_owned {
                (
                    child_task
                        .run_ordinary_owned(OrdinaryStart::Stopped(child))
                        .await,
                    None,
                )
            } else {
                match child_task.run_to_terminal(child).await {
                    Ok(completed) => (Ok(Some(completed.status)), Some(completed.cleanup)),
                    Err(error) => (Err(error), None),
                }
            };
            let exit_status = match result {
                Err(err) => {
                    if ordinary_owned {
                        // The fatal session retains the original cause and the
                        // generation; never detach or synthesize a child status.
                        return Err(err);
                    }
                    if parent_failure.cause().is_some() || parent_failure.run_failed() {
                        // A pending or failed terminal join is not completed
                        // cleanup and cannot authorize numeric detach/reuse.
                        child_custody_is_fatal(
                            creator,
                            id,
                            "parent_completion_terminal_unconfirmed",
                            &err.to_string(),
                        );
                    }
                    tracing::error!("Error in tracee tid {}: {}", tid, err);

                    if liteinst_activation_failure_reason(&err).is_some() {
                        // Every typed LiteInst activation failure has already
                        // notified the session root. Its cleanup guard owns the
                        // exact pidfds and notifier handles for this tree.
                        // Detaching here lets a guest parent consume the child
                        // before that notifier acknowledges terminal status.
                        // This includes failed reactivation after exec, as well
                        // as a refused exec or a kernel-frozen vfork parent.
                        return Ok(Some(ExitStatus::Exited(1)));
                    }

                    // We assume the tracee is stopped since this error likely
                    // originated from the tool itself when the tracee is
                    // already stopped. If the tracee is not in a stopped state,
                    // that's fine too and ignore the detach error.
                    let detach_span = tracing::debug_span!(
                        target: "reverie_ptrace::lifecycle",
                        "tracee.detach",
                        %tid,
                        reason = "handler error"
                    );
                    let detach_guard = detach_span.enter();
                    let running = match RootStopLease::new(
                        Stopped::new_unchecked(tid),
                        detach_held_root_stop,
                    )
                    .detach(None)
                    {
                        Err(err) => {
                            // If we get an error here, the child process may
                            // not be in a ptrace stop.
                            tracing::error!("Failed to detach from {}: {}", tid, err);
                            return Ok(Some(ExitStatus::Exited(1)));
                        }
                        Ok(running) => running,
                    };
                    drop(detach_guard);

                    match running.next_state().await {
                        Ok(wait) => wait.assume_exited().1,
                        Err(TraceError::Died(zombie)) => match zombie.reap().await {
                            Ok(exit_status) => exit_status,
                            Err(error) => {
                                tracing::error!(
                                    %tid,
                                    %error,
                                    "failed to reap detached tracee"
                                );
                                ExitStatus::Exited(1)
                            }
                        },
                        Err(TraceError::Errno(errno)) => {
                            tracing::error!(
                                %tid,
                                %errno,
                                "failed waiting for detached tracee exit"
                            );
                            ExitStatus::Exited(1)
                        }
                    }
                }
                Ok(None) => return Ok(None),
                Ok(Some(status)) => {
                    let cleanup_matches = completed_cleanup
                        .as_ref()
                        .is_none_or(|cleanup| child_cleanup.same_generation(cleanup));
                    if !cleanup_matches
                        || !child_cleanup.wait(std::time::Duration::ZERO)
                        || !matches!(child_cleanup.observed_terminal(), Some(Ok(actual)) if actual == status)
                    {
                        child_custody_is_fatal(
                            creator,
                            id,
                            "completed_child_terminal_identity",
                            "completed run lacks the retained actual final wait",
                        );
                    }
                    // The root still reports the shared original failure. This
                    // ordinary child join receives only its actual final status,
                    // after consuming cleanup, never the old numeric detach path.
                    status
                }
            };
            #[cfg(test)]
            preconstruction_tests::child_worker_drained_after_run(id, &child_cleanup);
            #[cfg(test)]
            parent_completion_tests::child_worker_drained(id, &child_cleanup);
            Ok(Some(exit_status))
        });
        if self.ordinary_failure_enabled() {
            self.global_state.fatal_session.handed(id);
        }
        let task_body = async move {
            match AssertUnwindSafe(body).catch_unwind().await {
                Ok(Ok(exit_status)) => {
                    if let Some(failure) = &report_failure {
                        failure.newborn_exited(panic_tid);
                    }
                    exit_status
                }
                Ok(Err(error)) => {
                    if let Some(failure) = report_failure {
                        failure.newborn_handoff_failed(panic_tid);
                        failure.fail(error);
                    }
                    None
                }
                Err(payload) => guest_task_panic_is_fatal(panic_tid, payload),
            }
        };
        let task = if self.ordinary_failure_enabled() {
            let (sender, receiver) = oneshot::channel();
            let finished = Arc::new(AtomicBool::new(false));
            let task_finished = Arc::clone(&finished);
            let handle = tokio::task::spawn_local(async move {
                let result = task_body.await;
                #[cfg(test)]
                let body_status = result;
                task_finished.store(true, Ordering::Release);
                let _sent = sender.send(result);
                #[cfg(test)]
                source_cohort::startup_tests::body_completed(
                    &completion_readback,
                    body_status,
                    _sent.is_ok(),
                );
                #[cfg(all(test, cohort_final_test))]
                source_cohort::final_tests::body_completed(
                    &completion_readback,
                    body_status,
                    _sent.is_ok(),
                );
                // The consuming task, final-status validation, newborn
                // bookkeeping and result publication have all finished.
                // The original session still owns and joins this same handle.
                if let Some(retirement) = retirement {
                    retirement.completed(result);
                }
            });
            self.global_state
                .fatal_session
                .joins
                .lock()
                .unwrap()
                .push(handle);
            ChildCompletion::Owned { receiver, finished }
        } else {
            ChildCompletion::Legacy(tokio::task::spawn_local(task_body))
        };

        self.pending_child = Some(PendingChild::Spawned(
            child_kind,
            Child {
                id,
                suspended,
                wait_all_stop_tx: None,
                daemonizer_rx,
                handle: task,
                ordinary_group,
                terminal,
            },
        ));
        Some(ChildDebugChannels {
            request_tx: child_request_tx,
            resume_tx: child_resume_tx,
            stop_rx: child_stop_rx,
        })
    }

    /// Transfer to the existing child owner. Cancellation before the lock is
    /// acquired leaves the handle in this actor; there is no suspension between
    /// taking it and inserting it in the selected ordinary child list.
    async fn publish_pending_child(&mut self) {
        // The creator's consuming Tool exit has not happened yet. A canceled
        // dispatch finishes the same native stage using that original state.
        let _ = self.materialize_pending_child().await;
        #[cfg(test)]
        let publishing = match &self.pending_child {
            Some(PendingChild::Spawned(_, child)) => Some(child.id()),
            _ => None,
        };
        publish_child_to_existing_list(
            &mut self.pending_child,
            &self.child_threads,
            &self.child_procs,
        )
        .await;
        #[cfg(test)]
        if let Some(id) = publishing {
            preconstruction_tests::child_published(id);
        }
    }

    async fn handle_vfork_done_event(&mut self, stopped: Stopped) -> Result<Wait, TraceError> {
        self.resume_stopped(stopped, None)?.next_state().await
    }

    async fn wait_after_exit_event(
        task: Stopped,
        held_root_stop: Option<Arc<StdMutex<Option<HeldRootStop>>>>,
    ) -> Result<Wait, TraceError> {
        #[cfg(test)]
        newborn_startup_tests::exit_resuming(&task);
        #[cfg(test)]
        let resume_readback = task.terminal_cleanup();
        // de_thread can replace the leader after its PTRACE_EVENT_EXIT, so
        // this wait may report Exec with the caller's former TID, not death.
        if let Some(slot) = held_root_stop.as_ref() {
            HeldRootStop::supersede_with_exit(slot, &task)?;
        }
        #[cfg(all(test, cohort_final_test))]
        source_cohort::final_tests::exit_attempt(&resume_readback);
        let resumed = RootStopLease::new(task, held_root_stop).resume(None);
        #[cfg(test)]
        source_cohort::startup_tests::exit_resumed(
            &resume_readback,
            source_cohort::startup_tests::OwnerPath::Startup,
            &resumed
                .as_ref()
                .map(|_| ())
                .map_err(|error| format!("{error:?}")),
        );
        #[cfg(all(test, cohort_final_test))]
        source_cohort::final_tests::exit_resumed(
            &resume_readback,
            source_cohort::final_tests::OwnerPath::Startup,
            &resumed
                .as_ref()
                .map(|_| ())
                .map_err(|error| format!("{error:?}")),
        );
        resumed?.next_state().await
    }

    #[cfg(test)]
    pub(crate) async fn handle_exit_event(
        task: Stopped,
        held_root_stop: Option<Arc<StdMutex<Option<HeldRootStop>>>>,
    ) -> Result<ExitStatus, TraceError> {
        let wait = Self::wait_after_exit_event(task, held_root_stop).await?;
        let (_pid, exit_status) = wait.assume_exited();
        Ok(exit_status)
    }

    /// Aborts the current handler. This just sends a result through a channel to
    /// the `run_loop`, which should cause the current future to be dropped and
    /// canceled. Thus, this function will never return so that execution of the
    /// current future doesn't proceed any further.
    async fn abort(&mut self, result: Result<Wait, TraceError>) -> ! {
        if self.next_state.send(result).await.is_err() {
            panic!(
                "failed to abort tracee {}: run-loop next-state channel is closed",
                self.tid()
            );
        }

        // Wait on a future that will never complete. This pending future will
        // be dropped when the channel receives the event just sent.
        future::pending().await
    }

    fn observe_injected_syscall(
        &mut self,
        observation: Option<(Sysno, SyscallArgs)>,
        event: InjectedSyscallEvent,
    ) {
        let Some((nr, args)) = observation else {
            return;
        };
        self.process_state.on_injected_syscall_observed(
            self.tid,
            self.global_state.gs_ref.as_ref(),
            &mut self.thread_state,
            nr,
            args,
            event,
        );
    }

    fn observe_thread_terminal(&mut self, status: ExitStatus) {
        if let Some(prior) = self.observed_terminal {
            assert_eq!(
                prior, status,
                "one owned task produced conflicting final waits"
            );
            return;
        }
        self.observed_terminal = Some(status);
        if let Some(member) = &self.cohort {
            member.terminal_observed();
        }
        self.process_state.on_backend_thread_terminal(
            self.tid,
            self.global_state.gs_ref.as_ref(),
            &mut self.thread_state,
            status,
        );
    }

    /// Marks the current task as exited via a channel. The receiver end of the
    /// channel should cause the current future to be dropped and canceled. Thus,
    /// this function will never return so that execution doesn't proceed any
    /// further.
    async fn exit(&mut self, exit_status: ExitStatus) -> ! {
        self.observe_thread_terminal(exit_status);
        self.abort(Ok(Wait::Exited(self.tid(), exit_status))).await
    }

    /// Marks the current task as having successfully called `execve` and so it
    /// should never return.
    async fn execve(&mut self, next_state: Wait) -> ! {
        self.abort(Ok(next_state)).await
    }

    /// Triggers the tool exit callbacks.
    async fn tool_exit(self, exit_status: ExitStatus) -> Result<(), reverie::Error> {
        let retain_errors = self.global_state.liteinst_runtime.is_none();
        let failure = Arc::clone(&self.global_state.parent_completion_failure);
        let failure_global = Arc::clone(&self.global_state.gs_ref);
        let failure_pid = self.pid();
        let failure_tid = self.tid();
        let retain_error = |error| {
            if !retain_errors {
                return Err(error);
            }
            // A callback failure does not undo native death. Keep its typed
            // cause, wake dependents before the next callback can wait on them,
            // and finish consuming the remaining owned state exactly once.
            publish_run_error(
                &failure,
                failure_global.as_ref(),
                failure_pid,
                failure_tid,
                "ptrace consuming Tool exit callback failed",
                error,
            );
            Ok(())
        };
        if self.is_main_thread() {
            // Wait for all child threads to fully exit. This *must* happen before
            // the main thread can exit.
            // TODO: Use FuturesUnordered instead of `join_all` for better
            // performance.
            {
                let children = self.child_threads.lock().await.take_inner();
                future::join_all(children).await;
            }

            // Transfer process children to the existing tree drain, including
            // completed Tool tasks: their ptrace final may have preceded real
            // parent adoption. This does not make the tracer a Linux subreaper;
            // an external real parent's separate wait remains its obligation.
            let orphans = if self.global_state.liteinst_runtime.is_some() {
                // A LiteInst session follows process children as part of one
                // fail-closed instrumentation domain. Do not let root exit end
                // the LocalSet while a child can still publish a session
                // failure: join those exact followed tasks first.
                let children = self.child_procs.lock().await.take_inner();
                future::join_all(children).await;
                Children::new()
            } else {
                let children = self.child_procs.lock().await.take_inner();
                let mut orphans = Children::new();
                for child in children {
                    orphans.push(child);
                }
                orphans
            };

            for orphan in orphans.into_inner() {
                // Bon voyage.
                if let Err(err) = self.orphanage.send(orphan).await {
                    let orphan = err.0;
                    tracing::warn!(
                        pid = %orphan.id(),
                        "orphan reaper closed; waiting for child inline"
                    );
                    let _ = orphan.await;
                }
            }

            let _ = self
                .notify_gdb_stop(StopReason::Exited(self.pid(), exit_status))
                .await;

            let wrapped = WrappedFrom(self.tid, &self.global_state);

            // Thread exit
            if let Err(error) = self
                .process_state
                .on_exit_thread(self.tid, &wrapped, self.thread_state, exit_status)
                .await
            {
                retain_error(error)?;
            }

            // The try_unwrap and subsequent unwrap are safe to do. ptrace
            // guarantees that all threads in the thread group have exited
            // before the main thread.
            let process_state = Arc::try_unwrap(self.process_state).unwrap_or_else(|_| {
                // If you end up seeing this panic, make sure that all clones of
                // `process_state` are dropped before reaching this point.
                panic!("Reverie internal invariant broken. try_unwrap on process state failed")
            });
            let wrapped = WrappedFrom(self.tid, &self.global_state);
            if let Err(error) = process_state
                .on_exit_process(self.tid, &wrapped, exit_status)
                .await
            {
                retain_error(error)?;
            }

            let ntasks_remaining = self.ntasks.fetch_sub(1, Ordering::SeqCst);
            let ndaemons = self.ndaemons.load(Ordering::SeqCst);

            if self.is_a_daemon {
                self.ndaemons.fetch_sub(1, Ordering::SeqCst);
            }

            if ntasks_remaining == 1 + ndaemons {
                // daemonize() might not get called, this is not an error.
                let _ = self.daemon_kill_switch.send(());
            }
        } else {
            let _ = self
                .notify_gdb_stop(StopReason::ThreadExited(
                    self.tid(),
                    self.pid(),
                    exit_status,
                ))
                .await;
            let wrapped = WrappedFrom(self.tid, &self.global_state);

            self.child_threads
                .lock()
                .await
                .retain(|child| child.id() != self.tid);

            // Thread exit
            if let Err(error) = self
                .process_state
                .on_exit_thread(self.tid, &wrapped, self.thread_state, exit_status)
                .await
            {
                retain_error(error)?;
            }

            self.ntasks.fetch_sub(1, Ordering::SeqCst);
            if self.is_a_daemon {
                self.ndaemons.fetch_sub(1, Ordering::SeqCst);
            }
        }

        #[cfg(test)]
        newborn_startup_tests::retired(self.tid, &self.ntasks, &self.ndaemons);
        #[cfg(test)]
        parent_completion_tests::retired(self.tid, &self.ntasks, &self.ndaemons);
        #[cfg(test)]
        run_error_tests::retired(self.tid, &self.ntasks, &self.ndaemons);
        #[cfg(test)]
        run_error_tests::additional::retired(self.tid, &self.ntasks, &self.ndaemons);
        Ok(())
    }

    async fn run_loop(&mut self, task: Stopped) -> Result<ExitStatus, reverie::Error> {
        match self.run_loop_internal(task).await {
            Ok(exit_status) => Ok(exit_status),
            Err(Error::RunFailed) => future::pending().await,
            Err(err) => {
                if self.global_state.liteinst_runtime.is_some() {
                    // Return immediately to the outer LiteInst cleanup guard.
                    // It owns the original root pidfd and every generation-
                    // bound notifier handle; this task must not reopen or
                    // numerically signal the root PID.
                    return Err(anyhow::Error::new(err).into());
                }
                // Note: Calling handle_internal_error cannot happen in the
                // `select!()` of the `run` function because then the exit
                // events that get generated in here cannot be caught by the
                // `select!()`.
                handle_internal_error(err).await
            }
        }
    }

    fn observe_ready_thread_state(&self) -> Result<(), Errno> {
        observe_ready_thread_state(
            self.process_state.as_ref(),
            self.pid(),
            self.tid(),
            self.global_state.gs_ref.as_ref(),
            &self.thread_state,
        )
    }

    async fn run_loop_internal(&mut self, task: Stopped) -> Result<ExitStatus, Error> {
        // This is the beginning of the life of the guest. Allow the tool to
        // inject syscalls as soon as the thread starts.
        match cancellable(self.cancel_handler.clone(), async {
            self.process_state.clone().handle_thread_start(self).await
        })
        .await
        {
            Some(Ok(())) => self.observe_ready_thread_state()?,
            Some(Err(err)) => {
                if self.ordinary_failure_enabled() && !matches!(err, reverie::Error::Errno(_)) {
                    self.publish_ordinary_failure("ptrace thread start", err);
                    return Err(Error::RunFailed);
                }
                // Legitimate guest errno keeps the existing startup behavior.
                err.into_errno()?;
            }
            None => {}
        }
        if self.command_bootstrap {
            if task.pid() != self.tid() {
                return Err(Error::runtime(
                    self.tid(),
                    "observe initial stop",
                    "stopped task differs from retained root",
                ));
            }
            let filter = self.command_filter.clone().ok_or_else(|| {
                Error::runtime(
                    self.tid(),
                    "observe initial stop",
                    "missing backend-owned Command filter",
                )
            })?;
            let observation = InitialCommandStop {
                root_tid: self.tid(),
                former_tid: None,
                filter: &filter,
            };
            // Unlike the preceding thread-start callback, this mandatory
            // observation cannot be skipped by its cancellation flag.
            self.process_state
                .clone()
                .handle_initial_stop(self, &observation)
                .await?;
        }
        self.ordinary_continuation()?;
        self.ordinary_trace_continuation()?;
        self.timer.finalize_requests();

        // Resume the guest for the first time. Note that the root task and
        // child tasks start out in a stopped state for different reasons: The
        // root task is stopped because of the SIGSTOP raised inside of `fork()`
        // after calling `traceme`. Child tasks start out in a running state,
        // but we wait for them to stop in `Event::NewChild`.
        //
        // NB: await_gdb_resume == resume if not attached_by_gdb.
        let running = self
            .await_gdb_resume(task, ExpectedGdbResume::Resume)
            .await
            .tracee_context(self.tid(), "initial tracee resume")?;

        // Notify gdb server (if any) that tracee is ready.
        if let Some(server_tx) = self.gdbserver_start_tx.take() {
            self.attached_by_gdb = true;
            if server_tx.send(()).is_err() {
                tracing::warn!(tid = %self.tid(), "GDB server closed before tracee attach");
                self.attached_by_gdb = false;
            }
        }

        let task_state = running
            .next_state()
            .await
            .tracee_context(self.tid(), "wait after initial tracee resume")?;
        self.run_loop_events(task_state).await
    }

    async fn run_loop_events(&mut self, mut task_state: Wait) -> Result<ExitStatus, Error> {
        let mut next_state_rx = self.next_state_rx.take().ok_or_else(|| {
            Error::runtime(
                self.tid(),
                "initialize run loop",
                "next-state receiver was already taken",
            )
        })?;

        loop {
            if let Some(stats) = &self.global_state.backend_stats {
                stats.record_wait(&task_state);
            }
            // A nested handler may forward a stop it already armed before
            // inspecting the status. Accept only that exact generation/status;
            // every ordinary returned transition still requires an empty slot.
            self.ensure_liteinst_wait(&task_state);
            match task_state {
                Wait::Stopped(stopped, event) => {
                    // Allow short-circuiting of the event stream. This makes it
                    // easier to send exit and execve events directly to the run
                    // loop from within `inject` or `tail_inject`.
                    let tid = self.tid();
                    let fut1 = next_state_rx.recv().fuse();
                    let fut2 = self.handle_stop_event(stopped, event).fuse();

                    futures::pin_mut!(fut1, fut2);

                    task_state = futures::select_biased! {
                        next_state = fut1 => {
                            if let Some(next_state) = next_state {
                                next_state.map_err(Error::Internal)
                            } else {
                                Err(Error::runtime(
                                    tid,
                                    "receive injected tracee state",
                                    "next-state channel closed unexpectedly",
                                ))
                            }
                        }
                        next_state = fut2 => next_state,
                    }?;
                }
                Wait::Exited(pid, exit_status) => {
                    self.observe_thread_terminal(exit_status);
                    self.notify_gdb_stop(StopReason::Exited(pid, exit_status))
                        .await?;
                    break Ok(exit_status);
                }
            }
        }
    }

    /// Errno erases causal provenance. Resolve against the current held stop,
    /// retaining the original return while only the original owner can exit.
    async fn ordinary_callback_errno<T>(
        &mut self,
        phase: &'static str,
        result: Result<T, Errno>,
    ) -> Result<T, TraceError> {
        let errno = match result {
            Ok(value) => return Ok(value),
            Err(errno) if !self.ordinary_failure_enabled() => return Err(errno.into()),
            Err(errno) => errno,
        };
        use crate::PtraceCallbackDecision as Decision;
        use crate::PtraceCallbackRefusal as Refusal;
        let session = self.fatal_session();
        let mut diagnostic = crate::PtraceCallbackDiagnostic {
            origin: BackendFailure {
                pid: self.pid(),
                tid: self.tid(),
                phase,
            },
            errno,
            held: None,
            sample: None,
            refusal: None,
            decision: Decision::AwaitingOwner,
            outcome: None,
            failure_published_at_outcome: false,
            backend_signalling_at_outcome: false,
        };
        let decision = if session.is_failed() {
            diagnostic.decision = Decision::Cancelled;
            session.fail_at(diagnostic.origin, errno.into());
            Ok(true)
        } else if session.ptracer_thread != Some(std::thread::current().id()) {
            Err(Refusal::WrongThread)
        } else {
            let stop = session
                .tree
                .lock()
                .unwrap()
                .tasks
                .iter()
                .find(|stop| stop.tid == self.tid())
                .cloned();
            let observation = match stop {
                Some(stop) => self
                    .ordinary_held_stop
                    .lock()
                    .unwrap()
                    .as_ref()
                    .ok_or(Refusal::HeldStop)
                    .and_then(|held| held.callback_observation(self.tid(), &stop.terminal)),
                None => Err(Refusal::HeldStop),
            };
            match observation {
                Ok((held, sample)) => {
                    let result = callback_owner_decision(held, &sample);
                    diagnostic.held = Some(held);
                    diagnostic.sample = Some(sample);
                    result
                }
                Err(error) => Err(error),
            }
        };
        let park = match decision {
            Ok(park) => {
                if !park {
                    diagnostic.decision = Decision::Fatal;
                }
                park
            }
            Err(refusal) => {
                diagnostic.decision = Decision::Refused;
                diagnostic.refusal = Some(refusal);
                false
            }
        };
        let refusal = diagnostic.refusal.clone();
        {
            let mut records = session.callback_diagnostics.lock().unwrap();
            self.pending_callback_diagnostic = Some(records.len());
            records.push(diagnostic);
        }
        if park {
            // No timer finalization, result encoding, continuation or new wait.
            // drive_ordinary retains and polls the one existing exit receiver.
            return future::pending().await;
        }
        self.publish_ordinary_failure(phase, errno.into());
        if let Some(refusal) = refusal {
            session.fail_at(
                BackendFailure {
                    pid: self.pid(),
                    tid: self.tid(),
                    phase: "ptrace callback lifecycle observation",
                },
                anyhow::Error::new(refusal).into(),
            );
        }
        Err(errno.into())
    }

    fn resolve_callback_diagnostic(
        &mut self,
        outcome: crate::PtraceCallbackOutcome,
        receipt: crate::tracer::OrdinaryReceipt,
    ) {
        if let Some(index) = self.pending_callback_diagnostic.take() {
            let session = self.fatal_session();
            let mut records = session.callback_diagnostics.lock().unwrap();
            let record = &mut records[index];
            record.outcome = Some(outcome);
            record.failure_published_at_outcome = receipt.failure_published;
            record.backend_signalling_at_outcome = receipt.backend_signalling;
            if record.decision == crate::PtraceCallbackDecision::AwaitingOwner
                && receipt.failure_published
            {
                record.decision = crate::PtraceCallbackDecision::Cancelled;
            }
        }
    }

    fn publish_ordinary_failure(&self, phase: &'static str, error: reverie::Error) {
        self.global_state.fatal_session.fail_at(
            BackendFailure {
                pid: self.pid(),
                tid: self.tid(),
                phase,
            },
            error,
        );
        if let Err(error) = self.timer.cancel() {
            self.global_state.fatal_session.fail_at(
                BackendFailure {
                    pid: self.pid(),
                    tid: self.tid(),
                    phase: "ptrace timer cancellation",
                },
                error.into(),
            );
        }
    }

    fn ordinary_failure_enabled(&self) -> bool {
        // Configured static traps use the same stopped-task ownership as
        // seccomp. Dynamic LiteInst retains its separate session cleanup guard.
        self.global_state.liteinst_runtime.is_none()
    }

    fn ordinary_trace_continuation(&self) -> Result<(), TraceError> {
        if self.global_state.fatal_session.is_failed() {
            Err(Errno::ECANCELED.into())
        } else {
            Ok(())
        }
    }

    fn ordinary_continuation(&self) -> Result<(), Error> {
        if self.global_state.fatal_session.is_failed() {
            Err(Error::RunFailed)
        } else {
            Ok(())
        }
    }

    pub(crate) fn fatal_session(&self) -> Arc<FatalSession> {
        self.global_state.fatal_session.clone()
    }

    async fn ordinary_start(&mut self, start: OrdinaryStart) -> Result<ExitStatus, reverie::Error> {
        let child = match start {
            OrdinaryStart::Exec(child, former) => {
                let result = match self.handle_exec_event(child, former).await {
                    Ok(state) => self.run_loop_events(state).await,
                    Err(error) => Err(error),
                };
                return match result {
                    Ok(status) => Ok(status),
                    Err(Error::RunFailed) => future::pending().await,
                    Err(error) => handle_internal_error(error).await,
                };
            }
            OrdinaryStart::Stopped(child) => {
                #[cfg(test)]
                if let Some(control) = FATAL_SETUP_CONTROL.with(|slot| slot.borrow_mut().take()) {
                    *control.child.lock().unwrap() =
                        Some((self.tid(), Arc::new(child.terminal_cleanup())));
                    return Err(
                        anyhow::Error::new(std::io::Error::from_raw_os_error(libc::EIO))
                            .context("injected newborn setup refusal after its actual initial stop")
                            .into(),
                    );
                }
                child
            }
        };
        self.run_loop(child).await
    }

    async fn run_ordinary(self, child: Stopped) -> Result<ExitStatus, reverie::Error> {
        match self
            .run_ordinary_owned(OrdinaryStart::Stopped(child))
            .await?
        {
            Some(status) => Ok(status),
            None => unreachable!("only a nonleader child can transfer at exec"),
        }
    }

    async fn run_ordinary_owned(
        mut self,
        mut start: OrdinaryStart,
    ) -> Result<Option<ExitStatus>, reverie::Error> {
        #[cfg(test)]
        {
            let (OrdinaryStart::Stopped(child) | OrdinaryStart::Exec(child, _)) = &start;
            run_error_tests::retained(child, &self.ntasks, &self.ndaemons);
            run_error_tests::additional::retained(child, &self.ntasks, &self.ndaemons);
        }
        let session = self.fatal_session();
        #[cfg(test)]
        FATAL_FREEZE_CONTROL.with(|slot| {
            if let Some(control) = slot.borrow().as_ref() {
                *control.session.lock().unwrap() = Some(session.clone());
            }
        });
        let terminal = match &start {
            OrdinaryStart::Stopped(child) | OrdinaryStart::Exec(child, _) => {
                child.terminal_cleanup()
            }
        };
        let stop = Arc::new(FatalTaskStop {
            tid: self.tid(),
            terminal,
            held: self.ordinary_held_stop.clone(),
            frozen: AtomicBool::new(false),
            peer_invocation: StdMutex::new(None),
        });
        let slot = Arc::new(OrdinaryExecSlot {
            stop: stop.clone(),
            requested: AtomicBool::new(false),
            changed: Notify::new(),
            transferred: StdMutex::new(None),
        });
        self.ordinary_exec
            .lock()
            .unwrap()
            .insert(self.tid(), slot.clone());
        let newborn = session.register(stop.clone());
        let mut exit_event = match newborn {
            Some(newborn) => newborn.into_exit(),
            None => match &start {
                OrdinaryStart::Stopped(child) | OrdinaryStart::Exec(child, _) => {
                    Box::pin(child.exit_event())
                }
            },
        };
        loop {
            // Covers the entire run, parked callbacks, terminal waits, and
            // failure cleanup. The actual Exec requester owns the replacement
            // stop before this original task can transfer its state.
            let outcome = {
                let drive = self
                    .drive_ordinary(start, &mut exit_event, &stop, &session)
                    .fuse();
                let transfer = slot.requested().fuse();
                futures::pin_mut!(drive, transfer);
                futures::select_biased! {
                    () = transfer => None,
                    result = drive => Some(result),
                }
            };
            match outcome {
                None => {
                    *slot.transferred.lock().unwrap() = Some(Box::new(self));
                    slot.changed.notify_waiters();
                    // No on_exit hook or fabricated ExitStatus for the
                    // surviving former thread. Its state now has one owner.
                    return Ok(None);
                }
                Some(crate::tracer::OrdinaryTerminal::Exited(status, receipt)) => {
                    // Every actual terminal route, including direct run-loop
                    // return, keeps its original owner registered until the
                    // notifier retires. Announce physical quiescence first so a
                    // later peer failure does not wait for this finished loop.
                    stop.frozen.store(true, Ordering::Release);
                    session.changed.notify_waiters();
                    crate::tracer::retire_ordinary_terminal(&stop.terminal, &stop.held).await;
                    self.resolve_callback_diagnostic(
                        crate::PtraceCallbackOutcome::Exited(status),
                        receipt,
                    );
                    self.ordinary_exec.lock().unwrap().remove(&self.tid());
                    if let Some(stats) = &self.global_state.backend_stats {
                        stats.record_tracee_exit();
                    }
                    log_guest_exit(self.tid(), self.pid(), status);
                    self.observe_thread_terminal(status);
                    // A canceled Tool callback can leave an authenticated
                    // native child in this actor. Preserve the established
                    // ordering: observe the creator's actual terminal result,
                    // then finish child admission/construction before the
                    // creator's consuming Tool callbacks run.
                    self.publish_pending_child().await;
                    #[cfg(test)]
                    let callback_counts = (Arc::clone(&self.ntasks), Arc::clone(&self.ndaemons));
                    let cohort = self.cohort.clone();
                    self.tool_exit_ordinary(status).await;
                    #[cfg(test)]
                    source_cohort::startup_tests::callbacks_done(
                        &stop.terminal,
                        source_cohort::startup_tests::OwnerPath::Ordinary,
                        &callback_counts,
                    );
                    #[cfg(all(test, cohort_final_test))]
                    source_cohort::final_tests::callbacks_done(
                        &stop.terminal,
                        source_cohort::final_tests::OwnerPath::Ordinary,
                        &callback_counts,
                    );
                    session.finished(&stop);
                    if let Some(member) = cohort {
                        member.ordinary_callbacks_completed(&stop.terminal);
                    }
                    return Ok(Some(status));
                }
                Some(crate::tracer::OrdinaryTerminal::Exec {
                    stopped,
                    former,
                    replaced_status,
                    receipt,
                }) => {
                    self.resolve_callback_diagnostic(
                        crate::PtraceCallbackOutcome::Exec {
                            former,
                            exit_stop_status: replaced_status,
                        },
                        receipt,
                    );
                    let former_slot = loop {
                        let candidate = self.ordinary_exec.lock().unwrap().get(&former).cloned();
                        if former != self.tid()
                            && self.is_main_thread()
                            && stopped.pid() == self.tid()
                            && stopped.terminal_cleanup().same_generation(&stop.terminal)
                            && let Some(candidate) = candidate
                        {
                            break candidate;
                        }
                        session.retry_after(anyhow::anyhow!("actual exec former {former} has no same-process initialized owner").into()).await;
                    };
                    former_slot.requested.store(true, Ordering::Release);
                    former_slot.changed.notify_waiters();
                    let mut former_task = former_slot.take().await;
                    while !former_slot.stop.terminal.wait(std::time::Duration::ZERO) {
                        tokio::task::yield_now().await;
                    }
                    // Registry membership and immutable stop generation bind
                    // the request; the actual kernel Exec supplies the edge.
                    assert!(Arc::ptr_eq(&self.process_state, &former_task.process_state));
                    self.ordinary_exec.lock().unwrap().remove(&former);
                    session.finished(&former_slot.stop);
                    #[cfg(test)]
                    let timer_before = EXEC_TIMER_TRANSFERS.with(|control| {
                        control.borrow().as_ref().map(|_| {
                            (
                                self.timer
                                    .exec_test_identity()
                                    .expect("read old leader perf identity"),
                                former_task
                                    .timer
                                    .exec_test_identity()
                                    .expect("read former perf identity"),
                            )
                        })
                    });
                    std::mem::swap(&mut self.thread_state, &mut former_task.thread_state);
                    std::mem::swap(&mut self.timer, &mut former_task.timer);
                    std::mem::swap(&mut self.is_a_daemon, &mut former_task.is_a_daemon);
                    former_task
                        .retire_replaced_leader(self.tid(), replaced_status)
                        .await;
                    let pid = self.pid();
                    let tid = self.tid();
                    for (phase, error) in self.timer.retarget_after_exec(pid, tid) {
                        session.fail_at(
                            BackendFailure {
                                pid: self.pid(),
                                tid: self.tid(),
                                phase,
                            },
                            error.into(),
                        );
                    }
                    #[cfg(test)]
                    if let Some((displaced, before)) = timer_before {
                        let after = self
                            .timer
                            .exec_test_identity()
                            .expect("read replacement perf identity");
                        let closed = displaced.as_ref().is_some_and(|old| {
                            [old.clock_fd, old.timer_fd].into_iter().all(|fd| {
                                (unsafe { libc::fcntl(fd, libc::F_GETFD) }) == -1
                                    && std::io::Error::last_os_error().raw_os_error()
                                        == Some(libc::EBADF)
                            })
                        });
                        EXEC_TIMER_TRANSFERS.with(|control| {
                            control.borrow().as_ref().unwrap().lock().unwrap().push(
                                ExecTimerTransfer {
                                    displaced,
                                    before,
                                    after,
                                    displaced_fds_closed: closed,
                                },
                            )
                        });
                    }
                    // The old callback and its receiver were cancelled. A new
                    // channel carries only events from the replacement image.
                    let (tx, rx) = mpsc::channel(1);
                    self.next_state = tx;
                    self.next_state_rx = Some(rx);
                    self.pending_signal = None;
                    self.pending_syscall = None;
                    self.pending_syscall_already_skipped = false;
                    self.cancel_handler.store(false, Ordering::Release);
                    stop.frozen.store(false, Ordering::Release);
                    self.arm_liteinst_root_stop(&stopped, &Event::Exec(former));
                    exit_event = Box::pin(stopped.exit_event());
                    start = OrdinaryStart::Exec(stopped, former);
                }
            }
        }
    }

    async fn retire_replaced_leader(mut self, leader: Pid, status: ExitStatus) {
        let session = self.fatal_session();
        let former = self.tid();
        let pid = self.pid();
        if let Err(error) = self.timer.cancel() {
            session.fail_at(
                BackendFailure {
                    pid,
                    tid: leader,
                    phase: "ptrace replaced leader timer cancellation",
                },
                error.into(),
            );
        }
        for (phase, error) in self.timer.close_after_failure() {
            session.fail_at(
                BackendFailure {
                    pid,
                    tid: leader,
                    phase,
                },
                error.into(),
            );
        }
        let wrapped = WrappedFrom(leader, &self.global_state);
        if let Err(error) = self
            .process_state
            .on_exit_thread(leader, &wrapped, self.thread_state, status)
            .await
        {
            session.fail_at(
                BackendFailure {
                    pid,
                    tid: leader,
                    phase: "ptrace replaced leader on_exit_thread",
                },
                error,
            );
        }
        // The process and surviving thread have not exited. Only the old
        // leader's actual Exit-event state is consumed here.
        self.child_threads
            .lock()
            .await
            .retain(|child| child.id() != former);
        self.ntasks.fetch_sub(1, Ordering::SeqCst);
        if self.is_a_daemon {
            self.ndaemons.fetch_sub(1, Ordering::SeqCst);
        }
        if let Some(stats) = &self.global_state.backend_stats {
            stats.record_tracee_exit();
        }
    }

    async fn drive_ordinary(
        &mut self,
        start: OrdinaryStart,
        exit_event: &mut futures::future::BoxFuture<'static, Result<Stopped, TraceError>>,
        stop: &Arc<FatalTaskStop>,
        session: &Arc<FatalSession>,
    ) -> crate::tracer::OrdinaryTerminal {
        let global = self.global_state.gs_ref.clone();
        let parent_failure = Arc::clone(&self.global_state.parent_completion_failure);
        let main_thread = self.is_main_thread();
        let outcome = {
            let run_loop = self.ordinary_start(start).fuse();
            let cancelled = session.cancelled().fuse();
            let task_failure = wait_for_task_failure(global.as_ref(), &parent_failure).fuse();
            futures::pin_mut!(run_loop, cancelled, task_failure);
            let exit = async {
                let result = (&mut *exit_event).await;
                arbitrate_original_wait(result, || {
                    !main_thread && vanished_without_status(&stop.terminal)
                })
                .await
            }
            .fuse();
            futures::pin_mut!(exit);
            let selection = async {
                futures::select_biased! {
                    () = cancelled => None,
                    () = task_failure => {
                        if !session.is_failed() {
                            let error = if let Some((tid, error)) = parent_failure.cause() {
                                anyhow::anyhow!(
                                    "native parent syscall completion failed for {tid}: {error}"
                                )
                            } else {
                                anyhow::anyhow!("GlobalTool reported a failed ptrace run")
                            };
                            session.fail(error.into());
                        }
                        None
                    },
                    task = exit => {
                        session.source_cohort.terminal_selected(&task);
                        Some(Either::Left(task))
                    },
                    result = run_loop => Some(Either::Right(result)),
                }
            };
            #[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
            let selection = source_cohort::peer_tests::gate_external_sender(stop, selection);
            selection.await
        };
        drop(global);
        let outcome = match outcome {
            Some(Either::Left(stopped)) => ordinary_exit_or_failure(
                stopped,
                stop.terminal
                    .observed_exit_status()
                    .ok()
                    .flatten()
                    .is_some(),
                session,
            )
            .await
            .map(Either::Left),
            outcome => outcome,
        };
        match outcome {
            Some(Either::Right(Ok(status))) => {
                crate::tracer::OrdinaryTerminal::Exited(status, session.ordinary_receipt())
            }
            Some(Either::Left(stopped)) => {
                stop.frozen.store(true, Ordering::Release);
                session.changed.notify_waiters();
                self.finish_ordinary_exit_with_pending_thread(stopped, stop, session)
                    .await
            }
            failure => {
                if let Some(Either::Right(Err(error))) = failure {
                    self.publish_ordinary_failure("ptrace task callback", error);
                } else if let Err(error) = self.timer.cancel() {
                    session.fail_at(
                        BackendFailure {
                            pid: self.pid(),
                            tid: self.tid(),
                            phase: "ptrace timer cancellation",
                        },
                        error.into(),
                    );
                }
                session.freeze_and_kill(stop).await;
                let stopped = loop {
                    match (&mut *exit_event).await {
                        Ok(stopped) => break Ok(stopped),
                        Err(TraceError::Died(zombie)) => break Err(TraceError::Died(zombie)),
                        Err(error) => session.retry_after(anyhow::Error::new(error).into()).await,
                    }
                };
                self.finish_ordinary_exit_with_pending_thread(stopped, stop, session)
                    .await
            }
        }
    }

    /// A group leader cannot complete its final wait while a retained native
    /// thread still needs construction and resume. Drive that one continuation
    /// alongside the existing fatal-session exit owner; process children keep
    /// their established creator-final-before-construction ordering.
    async fn finish_ordinary_exit_with_pending_thread(
        &mut self,
        stopped: Result<Stopped, TraceError>,
        stop: &FatalTaskStop,
        session: &FatalSession,
    ) -> crate::tracer::OrdinaryTerminal {
        let pending_thread = matches!(
            &self.pending_child,
            Some(PendingChild::Native(native)) if native.kind == ChildTaskKind::Thread
        );
        #[cfg(test)]
        if pending_thread && let Ok(stopped) = &stopped {
            newborn_startup_tests::creator_exit_selected(stopped);
        }
        #[cfg(test)]
        if let Ok(stopped) = &stopped {
            newborn_startup_tests::exit_resuming(stopped);
        }
        let finish = crate::tracer::finish_ordinary_exit(stopped, stop, session).fuse();
        if !pending_thread {
            return finish.await;
        }
        let materialize = self.materialize_pending_child().fuse();
        futures::pin_mut!(finish, materialize);
        futures::select_biased! {
            outcome = finish => outcome,
            _ = materialize => finish.await,
        }
    }

    async fn tool_exit_ordinary(mut self, status: ExitStatus) {
        let session = self.fatal_session();
        let pid = self.pid();
        let tid = self.tid();
        let main = self.is_main_thread();
        if main {
            let children = self.child_threads.lock().await.take_inner();
            for result in future::join_all(children).await {
                if let Err(error) = result {
                    session.fail_at(
                        BackendFailure {
                            pid,
                            tid,
                            phase: "ptrace thread join",
                        },
                        error,
                    );
                }
            }
            let (orphans, _) = {
                let mut children = self.child_procs.lock().await;
                children.deref_mut().await
            };
            for orphan in orphans.into_inner() {
                if let Err(error) = self.orphanage.send(orphan).await
                    && let Err(error) = error.0.await
                {
                    session.fail(error);
                }
            }
        }
        let reason = if main {
            StopReason::Exited(pid, status)
        } else {
            StopReason::ThreadExited(tid, pid, status)
        };
        let _ = self.notify_gdb_stop(reason).await;
        let wrapped = WrappedFrom(tid, &self.global_state);
        if let Err(error) = self
            .process_state
            .on_exit_thread(tid, &wrapped, self.thread_state, status)
            .await
        {
            session.fail_at(
                BackendFailure {
                    pid,
                    tid,
                    phase: "ptrace on_exit_thread",
                },
                error,
            );
        }
        if main {
            let mut process = self.process_state;
            let process = loop {
                match Arc::try_unwrap(process) {
                    Ok(process) => break process,
                    Err(retained) => {
                        process = retained;
                        session
                            .retry_after(
                                anyhow::anyhow!("process Tool still has owners after child joins")
                                    .into(),
                            )
                            .await;
                    }
                }
            };
            if let Err(error) = process.on_exit_process(tid, &wrapped, status).await {
                session.fail_at(
                    BackendFailure {
                        pid,
                        tid,
                        phase: "ptrace on_exit_process",
                    },
                    error,
                );
            }
        } else {
            // The session still owns the actual JoinHandle. Removing this
            // completion subscription cannot detach a pending consuming hook.
            self.child_threads
                .lock()
                .await
                .retain(|child| child.id() != tid);
        }
        if session.is_failed() {
            if let Err(error) = self.timer.cancel() {
                session.fail_at(
                    BackendFailure {
                        pid,
                        tid,
                        phase: "ptrace timer cancellation",
                    },
                    error.into(),
                );
            }
            for (phase, error) in self.timer.close_after_failure() {
                session.fail_at(BackendFailure { pid, tid, phase }, error.into());
            }
        }
        let remaining = self.ntasks.fetch_sub(1, Ordering::SeqCst);
        let daemons = self.ndaemons.load(Ordering::SeqCst);
        if self.is_a_daemon {
            self.ndaemons.fetch_sub(1, Ordering::SeqCst);
        }
        if main && remaining == 1 + daemons {
            let _ = self.daemon_kill_switch.send(());
        }
        #[cfg(test)]
        newborn_startup_tests::retired(tid, &self.ntasks, &self.ndaemons);
        #[cfg(test)]
        parent_completion_tests::retired(tid, &self.ntasks, &self.ndaemons);
        #[cfg(test)]
        run_error_tests::retired(tid, &self.ntasks, &self.ndaemons);
        #[cfg(test)]
        run_error_tests::additional::retired(tid, &self.ntasks, &self.ndaemons);
    }

    /// A group leader's native final wait can depend on a retained thread
    /// child. Drive its existing construction/spawn continuation after releasing
    /// the canceled run-loop borrow. Process children keep their prior ordering:
    /// creator final observation precedes canceled child construction.
    async fn wait_after_exit_with_pending_thread(
        &mut self,
        task: Stopped,
        held_root_stop: Option<Arc<StdMutex<Option<HeldRootStop>>>>,
    ) -> Result<Wait, TraceError> {
        #[cfg(test)]
        newborn_startup_tests::creator_exit_selected(&task);
        let final_wait = Self::wait_after_exit_event(task, held_root_stop).fuse();
        if !matches!(
            &self.pending_child,
            Some(PendingChild::Native(native)) if native.kind == ChildTaskKind::Thread
        ) {
            return final_wait.await;
        }
        let materialize = self.materialize_pending_child().fuse();
        futures::pin_mut!(final_wait, materialize);
        futures::select_biased! {
            outcome = final_wait => outcome,
            _ = materialize => final_wait.await,
        }
    }

    pub(crate) fn parent_completion_failure(&self) -> Arc<ParentCompletionFailure> {
        Arc::clone(&self.global_state.parent_completion_failure)
    }

    /// Drive a single guest thread to completion. Returns the final exit code
    /// when that guest thread exits.
    pub async fn run(self, child: Stopped) -> Result<ExitStatus, reverie::Error> {
        if self.ordinary_failure_enabled() {
            #[cfg(test)]
            let run_tid = self.tid();
            #[cfg(test)]
            let failure = Arc::clone(&self.global_state.parent_completion_failure);
            #[cfg(test)]
            let session = self.fatal_session();
            let outcome = self.run_ordinary(child).await;
            #[cfg(test)]
            if !session.is_failed()
                && let Ok(status) = &outcome
            {
                run_error_tests::additional::root_succeeded(run_tid, *status, &failure);
            }
            return outcome;
        }
        #[cfg(test)]
        let run_tid = self.tid();
        let failure = Arc::clone(&self.global_state.parent_completion_failure);
        let outcome = self.run_to_terminal(child).await;
        if let Some((_, original)) = failure.take_run_error() {
            return Err(match outcome {
                Ok(_) => original,
                Err(cleanup) => preserve_run_error(Some(original), cleanup),
            });
        }
        let completed = outcome?;
        if let Some((tid, error)) = failure.cause() {
            return Err(anyhow::anyhow!(
                "native parent syscall completion failed for {tid}: {error}; actual terminal and consuming cleanup completed"
            ).into());
        }
        #[cfg(test)]
        run_error_tests::additional::root_succeeded(run_tid, completed.status, &failure);
        Ok(completed.status)
    }

    async fn run_to_terminal(mut self, child: Stopped) -> Result<CompletedTaskRun, reverie::Error> {
        // Only the session root owns the shared root-stop lease; a child task
        // that superseded it would strand the root's cleanup handoff.
        let exit_held_root_stop = self
            .liteinst_root_config()
            .map(|runtime| Arc::clone(&runtime.held_root_stop));
        let root_session_failure = self.liteinst_root_config().map(|runtime| {
            (
                Arc::clone(&runtime.session_failure),
                Arc::clone(&runtime.session_failure_changed),
            )
        });
        let failure_tid = self.tid();
        let failure_global = Arc::clone(&self.global_state.gs_ref);
        let parent_completion_failure = Arc::clone(&self.global_state.parent_completion_failure);
        let exact_cleanup = child.terminal_cleanup();
        #[cfg(test)]
        run_error_tests::retained(&child, &self.ntasks, &self.ndaemons);
        #[cfg(test)]
        run_error_tests::additional::retained(&child, &self.ntasks, &self.ndaemons);
        let mut failed_run = false;
        let retain_error_until_terminal = self.global_state.liteinst_runtime.is_none();
        let failure_pid = self.pid();
        let completion = {
            let exit_event = child.exit_event().fuse();
            let run_loop = self.run_loop(child).fuse();
            let session_failure = async move {
                let Some((failure, changed)) = root_session_failure else {
                    return future::pending::<String>().await;
                };
                loop {
                    let notified = changed.notified();
                    if let Some(message) = failure.lock().unwrap().clone() {
                        return message;
                    }
                    notified.await;
                }
            }
            .fuse();
            let failed =
                wait_for_task_failure(failure_global.as_ref(), &parent_completion_failure).fuse();
            futures::pin_mut!(exit_event, run_loop, session_failure, failed);

            futures::select_biased! {
                _ = failed => {
                    failed_run = true;
                    // A sibling can fail after this run-loop consumed its
                    // final wait but while its stop notification is pending.
                    // Reconcile that exact receipt before touching EXIT again.
                    wait_for_failed_run_terminal(
                        &exact_cleanup, failure_tid, exit_event.as_mut(),
                    ).await
                },
                task = exit_event => match task {
                    Ok(task) => Either::Left(Ok(task)),
                    Err(err) => Either::Left(Err(err)),
                },
                message = session_failure => Either::Right(Err(anyhow::anyhow!(
                    "LiteInst session failed closed in a non-root task: {message}"
                ).into())),
                exit_status = run_loop => match exit_status {
                    Err(error) if retain_error_until_terminal => {
                        // Keep this task and its Tool state until the original
                        // notifier observes termination. Returning the error
                        // now would drop that state before outer cleanup kills
                        // the tracee, losing both terminal and consuming hooks.
                        publish_run_error(
                            &parent_completion_failure, failure_global.as_ref(),
                            failure_pid, failure_tid,
                            "ptrace run-loop error before owned terminal cleanup", error,
                        );
                        failed_run = true;
                        // A run-loop error can follow an already consumed
                        // final wait (for example, a stop notification error).
                        // Reuse only the notifier's actual terminal receipt;
                        // do not wait again for its expired EXIT capability.
                        // EXIT stays owned by the original future. The
                        // run-loop borrow drops before a pending child/final
                        // wait is advanced below.
                        wait_for_failed_run_terminal(
                            &exact_cleanup, failure_tid, exit_event.as_mut(),
                        ).await
                    }
                    outcome => Either::Right(outcome),
                },
            }
        };
        // The old run-loop future is now dropped. A same-group newborn may
        // need to run (leader SYS_exit) or resume its EXIT (group SIGKILL) before
        // Linux permits the creator's final wait. Retain whichever child stage
        // wins; construction/spawn/replacement remains a synchronous handoff.
        let completion = match completion {
            Either::Left(Ok(task)) => Either::Left(
                self.wait_after_exit_with_pending_thread(task, exit_held_root_stop)
                    .await,
            ),
            Either::Left(Err(error)) => Either::Left(Err(error)),
            Either::Right(outcome) => Either::Right(outcome),
        };
        // The original EXIT continuation carries the existing notifier
        // generation. Re-arm an actual replacement Exec stop before publishing
        // failure so session cleanup can consume it.
        let outcome = match completion {
            Either::Left(Ok(wait @ Wait::Stopped(_, Event::Exec(former_tid))))
                if self.global_state.liteinst_runtime.is_some() && former_tid != self.tid() =>
            {
                self.arm_liteinst_wait(&wait);
                let (stopped, _) = wait.assume_stopped();
                // SAFETY: this branch owns the continuation of the single
                // ExitFuture-minted Stopped. wait_after_exit_event consumed
                // that capability through resume, and the old run-loop future
                // was dropped above. Retire its claimed exit permission before
                // handing the actual Exec stop to cancellation cleanup.
                match unsafe { stopped.terminal_cleanup().revoke_owned_exit_stop() } {
                    Ok(()) => {
                        let error = self.reject_liteinst_nonleader_exec(former_tid);
                        handle_internal_error(error.into()).await
                    }
                    Err(error) => Err(error.into()),
                }
            }
            Either::Left(Ok(wait)) => Ok(wait.assume_exited().1),
            Either::Left(Err(error)) => handle_internal_error(error.into()).await,
            Either::Right(outcome) => outcome,
        };
        if self.global_state.fatal_session.is_failed() {
            // The legacy session owner will terminate this generation. In
            // particular, a child must not turn cancellation into detach or a
            // fabricated exit status, nor run another ordinary Tool observer.
            return future::pending().await;
        }
        // The run-loop future has been dropped. Only Ok here derives from a
        // real final Wait::Exited; a synthetic tool_exit failure has no such
        // observation. Acknowledge before LiteInst handling or any child join.
        if let Ok(status) = &outcome {
            self.observe_thread_terminal(*status);
        }
        if outcome.is_err() {
            // In particular, a synthetic LiteInst tool_exit must not wait on
            // admitted Tool operations before the common run-failure fence.
            // Preserve the original error below; this is never a native result
            // or evidence that an unobserved clone created no child.
            self.global_state
                .gs_ref
                .report_backend_failure(reverie::BackendFailure {
                    pid: self.pid(),
                    tid: self.tid(),
                    phase: "ptrace task outcome unavailable before consuming cleanup",
                });
        }
        // A cancelled dispatch can leave the native initial wait or spawned
        // child in this actor. The final-wait acknowledgement/failure fence
        // above precedes finishing that child and any dependent lock/join.
        self.publish_pending_child().await;
        if outcome.is_ok() && self.global_state.liteinst_runtime.is_some() {
            let phase = self.liteinst_runtime.lock().unwrap().phase;
            if phase != LiteinstRuntimePhase::Ready {
                self.record_liteinst_failure(
                    LiteinstActivationFailureReason::TerminatedBeforeHandshake,
                    Error::runtime(
                        self.tid(),
                        "verify LiteInst runtime activation",
                        format!(
                            "tracee terminated before the required preload handshake completed (phase {phase:?})"
                        ),
                    ),
                );
            }
        }
        let local_failure_reason = self
            .liteinst_failure
            .as_ref()
            .map(LiteinstActivationFailure::reason);
        let (exit_status, failure) = match (outcome, self.liteinst_failure.take()) {
            (_, Some(original)) => (
                None,
                Some(reverie::Error::from(anyhow::Error::new(original))),
            ),
            (Ok(exit_status), None) => (Some(exit_status), None),
            (Err(error), _) => (None, Some(error)),
        };
        if let Some(failure) = failure {
            self.global_state
                .gs_ref
                .report_backend_failure(reverie::BackendFailure {
                    pid: self.pid(),
                    tid: self.tid(),
                    phase: "ptrace typed failure before synthetic Tool exit",
                });
            let vfork_failure =
                local_failure_reason == Some(LiteinstActivationFailureReason::VforkUnsupported);
            if self.global_state.liteinst_runtime.is_some()
                && self.liteinst_root_config().is_none()
                && !vfork_failure
            {
                let tid = self.tid();
                if let Err(error) = self.tool_exit(ExitStatus::Exited(1)).await {
                    tracing::warn!(
                        %tid,
                        %error,
                        "tool exit hook failed while releasing a failed LiteInst task"
                    );
                }
            }
            // A vfork parent returns directly to the session-level cleanup
            // guard because orderly per-task exit cannot advance while the
            // kernel has it frozen behind that child. Other non-root failures
            // complete the existing tool-exit bookkeeping. The root failure
            // notification allows cleanup to proceed independently if that
            // bookkeeping blocks on a tracee which has not exited yet.
            return Err(failure);
        }
        let exit_status = exit_status.expect("a task without a failure has an exit status");
        let root_session_failure = self.liteinst_root_config().and_then(|_| {
            self.global_state.liteinst_runtime.as_ref().map(|runtime| {
                (
                    Arc::clone(&runtime.session_failure),
                    Arc::clone(&runtime.session_failure_changed),
                )
            })
        });

        // A fail-closed refusal raised by a non-root task cannot reach the
        // root's cleanup guard, and that task's tracee was released so the rest
        // of the guest could finish. Refuse to report success over it.
        if let Some(message) = root_session_failure
            .as_ref()
            .and_then(|(slot, _)| slot.lock().unwrap().clone())
        {
            return Err(anyhow::anyhow!(
                "LiteInst session failed closed in a non-root task: {message}"
            )
            .into());
        }

        if let Some(stats) = &self.global_state.backend_stats {
            stats.record_tracee_exit();
        }
        log_guest_exit(self.tid(), self.pid(), exit_status);

        let tool_exit = self.tool_exit(exit_status).fuse();
        let common_failure =
            wait_for_task_failure(failure_global.as_ref(), &parent_completion_failure).fuse();
        futures::pin_mut!(tool_exit, common_failure);
        let tool_exit = async {
            futures::select_biased! {
                _ = common_failure => {
                    // Other existing run actors observe the same fence and
                    // terminate their exact retained task before child joins.
                    tool_exit.await?;
                    Ok::<bool, reverie::Error>(true)
                },
                result = tool_exit => {
                    result?;
                    // A callback can publish failure in its final ready poll.
                    // Re-poll the same subscriber before exposing success.
                    Ok(common_failure.now_or_never().is_some())
                },
            }
        }
        .fuse();
        let cleanup_result = if let Some((failure, changed)) = root_session_failure.as_ref() {
            let session_failure = async {
                loop {
                    let notified = changed.notified();
                    if let Some(message) = failure.lock().unwrap().clone() {
                        return message;
                    }
                    notified.await;
                }
            }
            .fuse();
            futures::pin_mut!(tool_exit, session_failure);
            futures::select_biased! {
                message = session_failure => return Err(anyhow::anyhow!(
                    "LiteInst session failed closed in a non-root task: {message}"
                ).into()),
                result = tool_exit => result,
            }
        } else {
            tool_exit.await
        };
        let cleanup_failed = match cleanup_result {
            Ok(failed) => failed,
            Err(error) => return Err(error),
        };
        failed_run |= cleanup_failed;

        // A child can fail while the root is joining it in `tool_exit`, after
        // the fast-path check above.  The join is the final ordering boundary:
        // re-read the shared slot before allowing the root's success to escape.
        if let Some(message) = root_session_failure
            .as_ref()
            .and_then(|(slot, _)| slot.lock().unwrap().clone())
        {
            return Err(anyhow::anyhow!(
                "LiteInst session failed closed in a non-root task: {message}"
            )
            .into());
        }

        // A completed child always joins with its actual terminal status.
        // The sticky shared cause reaches run()/the final orphan drain, so a
        // consumed failure cannot fall into the caller's unchecked detach arm.
        if failed_run
            && parent_completion_failure.cause().is_none()
            && !parent_completion_failure.run_failed()
        {
            return Err(anyhow::anyhow!(
                "ptrace execution failed; consuming cleanup did not certify the lost native outcome"
            )
            .into());
        }
        Ok(CompletedTaskRun {
            status: exit_status,
            cleanup: exact_cleanup,
        })
    }

    /// Skip the syscall which is about to happen in the tracee, switching the tracee
    /// from Seccomp() to the same skipped attempt's actual syscall EXIT.
    ///
    /// This uses the convention that setting the syscall number to -1 causes the
    /// kernel to skip it. This function takes as argument the current register state
    /// and restores it after stepping over the skipped syscall instruction.
    ///
    /// Preconditions:
    ///  Ptrace tracee is in a (seccomp) stopped state.
    ///  The tracee was stopped with the RIP pointing just after a syscall instruction (+2).
    ///
    /// Postconditions:
    ///  Retain the actual syscall EXIT stop (no synthetic signal).
    ///  Restore the registers to the state specified by the regs arg.
    async fn skip_seccomp_syscall(&mut self, task: Stopped) -> Result<Stopped, TraceError> {
        if self.cohort.is_some() {
            return self.skip_source_seccomp_syscall(task).await;
        }
        // So here we are, at ptrace seccomp stop, if we simply resume, the kernel
        // would do the syscall, without our patch. we change to syscall number to
        // -1, so that kernel would simply skip the syscall, so that we can jump to
        // our patched syscall on the first run. Please note after calling this
        // function, the task state will no longer be in ptrace event seccomp.
        let regs = task.getregs()?;
        let pre_rip = regs.ip();

        #[cfg(target_arch = "x86_64")]
        {
            let mut new_regs = regs;
            *new_regs.orig_syscall_mut() = -1i64 as u64;
            task.setregs(&new_regs)?;
        }

        #[cfg(target_arch = "aarch64")]
        task.set_syscall(-1)?;

        let mut running = self.step_stopped(task, None)?;

        // After the step, wait for the next transition. Note that this can return
        // an exited state if there is a group exit while some thread is blocked on
        // a syscall.
        loop {
            let wait = running.next_state().await?;
            self.arm_liteinst_wait(&wait);
            match wait {
                Wait::Stopped(task, Event::Signal(Signal::SIGTRAP)) => {
                    #[cfg(test)]
                    let forced_external_sigtrap = self.liteinst_runtime.lock().unwrap().phase
                        == LiteinstRuntimePhase::Waiting
                        && self
                            .global_state
                            .liteinst_runtime
                            .as_ref()
                            .and_then(|runtime| runtime.force_skip_signal_once.as_ref())
                            .is_some_and(|force_once| force_once.swap(false, Ordering::SeqCst));
                    #[cfg(not(test))]
                    let forced_external_sigtrap = false;
                    self.validate_nested_liteinst_activation_signal(
                        &task,
                        Signal::SIGTRAP,
                        LiteinstActivationOperation::SkipInterceptedSyscall,
                        NestedTrapExpectation::SyscallSkip { pre_rip },
                        forced_external_sigtrap,
                    )?;
                    #[cfg(target_arch = "x86_64")]
                    task.setregs(&regs)?;
                    break Ok(task);
                }
                Wait::Stopped(task, Event::Signal(sig)) => {
                    self.validate_nested_liteinst_activation_signal(
                        &task,
                        sig,
                        LiteinstActivationOperation::SkipInterceptedSyscall,
                        NestedTrapExpectation::SyscallSkip { pre_rip },
                        false,
                    )?;
                    // We can get a spurious signal here, such as SIGWINCH. Skip
                    // past them until the tracee eventually arrives at SIGTRAP.
                    running = self.step_stopped(task, sig)?;
                }
                Wait::Stopped(task, event) => {
                    panic!(
                        "skip_seccomp_syscall: PID {} got unexpected event: {:?}",
                        task.pid(),
                        event
                    );
                }
                Wait::Exited(_pid, exit_status) => {
                    #[allow(unreachable_code)]
                    break self.exit(exit_status).await;
                }
            }
        }
    }

    async fn skip_source_seccomp_syscall(&mut self, task: Stopped) -> Result<Stopped, TraceError> {
        // So here we are, at ptrace seccomp stop, if we simply resume, the kernel
        // would do the syscall, without our patch. we change to syscall number to
        // -1, so that kernel would simply skip the syscall, so that we can jump to
        // our patched syscall on the first run. Please note after calling this
        // function, the task state will no longer be in ptrace event seccomp.
        let regs = task.getregs()?;

        #[cfg(target_arch = "x86_64")]
        {
            let mut new_regs = regs;
            *new_regs.orig_syscall_mut() = -1i64 as u64;
            task.setregs(&new_regs)?;
        }

        #[cfg(target_arch = "aarch64")]
        task.set_syscall(-1)?;

        let running = self.syscall_stopped(task, None)?;

        // After the step, wait for the next transition. Note that this can return
        // an exited state if there is a group exit while some thread is blocked on
        // a syscall.
        {
            let wait = running.next_state().await?;
            self.arm_liteinst_wait(&wait);
            match wait {
                Wait::Stopped(task, Event::Syscall) => {
                    let _raw = task.syscall_exit_result()?;
                    // Preserve the original negative control. A forced external
                    // event must still fail activation; EXIT is not SIGTRAP.
                    #[cfg(test)]
                    if self.liteinst_runtime.lock().unwrap().phase == LiteinstRuntimePhase::Waiting
                        && self
                            .global_state
                            .liteinst_runtime
                            .as_ref()
                            .and_then(|r| r.force_skip_signal_once.as_ref())
                            .is_some_and(|once| once.swap(false, Ordering::SeqCst))
                    {
                        self.validate_nested_liteinst_activation_signal(
                            &task,
                            Signal::SIGTRAP,
                            LiteinstActivationOperation::SkipInterceptedSyscall,
                            NestedTrapExpectation::SyscallSkip { pre_rip: regs.ip() },
                            true,
                        )?;
                    }
                    #[cfg(target_arch = "x86_64")]
                    task.setregs(&regs)?;
                    Ok(task)
                }
                Wait::Stopped(task, Event::Signal(sig)) => {
                    // Keep the actual signal stop; the original run owner handles
                    // delivery/cancellation. Do not fabricate a skip completion.
                    self.abort(Ok(Wait::Stopped(task, Event::Signal(sig))))
                        .await;
                }
                Wait::Stopped(task, event) => {
                    panic!(
                        "skip_seccomp_syscall: PID {} got unexpected event: {:?}",
                        task.pid(),
                        event
                    );
                }
                Wait::Exited(_pid, exit_status) =>
                {
                    #[allow(unreachable_code)]
                    self.exit(exit_status).await
                }
            }
        }
    }

    /// inject syscall for given tracee
    ///
    /// NB: limitations:
    /// - tracee must be in stopped state.
    /// - the tracee must have returned from PTRACE_EXEC_EVENT
    /// - must be called on the ptracer thread
    ///
    /// Side effects:
    /// - mutates contexts
    async fn untraced_syscall(
        &mut self,
        task: Stopped,
        nr: Sysno,
        args: SyscallArgs,
        observe_tool: bool,
    ) -> Result<Result<i64, Errno>, TraceError> {
        self.global_state
            .fatal_session
            .source_epoch
            .observe(nr, args);
        self.validate_liteinst_mapping_execution(nr, args)?;
        tracing::trace!(
            "[scheduler/tool] (pid = {}) untraced syscall: {:?}",
            task.pid(),
            nr
        );
        // TODO-HUMAN-REVIEW(PR-103): Review original-frame syscall injection.
        let oldregs = task.getregs()?;
        let mut regs = if self.injected_syscall_frame.is_some() {
            self.read_guest_registers(&task)?
        } else {
            oldregs
        };

        *regs.syscall_mut() = nr as Reg;
        *regs.orig_syscall_mut() = nr as Reg;
        regs.set_args((
            args.arg0 as Reg,
            args.arg1 as Reg,
            args.arg2 as Reg,
            args.arg3 as Reg,
            args.arg4 as Reg,
            args.arg5 as Reg,
        ));
        let child_context = self.injected_syscall_frame.is_some().then_some(regs);
        let mut cohort_native = None;

        // Jump to our private page to run the syscall instruction there. See
        // `populate_mmap_page` for details.
        *regs.ip_mut() = cp::PRIVATE_PAGE_OFFSET as Reg;

        task.setregs(&regs)?;

        // Reach this private instruction's actual ENTRY before its effect.
        // No SINGLESTEP completion signal can alter guest signal dispositions.
        let wait = if self.cohort.is_some() {
            self.syscall_stopped(task, None)?.next_state().await?
        } else {
            self.step_stopped(task, None)?.next_state().await?
        };
        let wait = match wait {
            Wait::Stopped(stopped, Event::Syscall) if self.cohort.is_some() => {
                // The previous transition consumed its lease. Retain this
                // actual new stop for validation failure/cancellation as well
                // as for the single effect transition below.
                self.arm_liteinst_root_stop(&stopped, &Event::Syscall);
                #[cfg(all(test, target_arch = "x86_64"))]
                if let Some(gate) = SOURCE_INJECTED_STOP_GATE.with(|slot| {
                    let mut gate = slot.borrow_mut();
                    if nr == Sysno::clone
                        && gate.as_ref().is_some_and(|gate| gate.tid == stopped.pid())
                    {
                        gate.take()
                    } else {
                        None
                    }
                }) {
                    let entry = stopped.syscall_entry()?;
                    let receipt = SourceInjectedStopReceipt {
                        tid: stopped.pid(),
                        seccomp: entry.seccomp,
                        number: entry.number,
                        arguments: entry.arguments,
                        ip: stopped.getregs()?.ip() as usize,
                        terminal: stopped.terminal_cleanup(),
                        held: self
                            .liteinst_root_stop_slot(&stopped)
                            .expect("actual injected stop must have its cleanup slot"),
                        failure_cleanup: Some(HeldRootStop::from_event(&stopped, &Event::Syscall)),
                    };
                    assert!(
                        gate.entered.send(receipt).is_ok(),
                        "actual stop receiver lost"
                    );
                    // Only the original session cancellation may release this
                    // callback. No second step or synthetic result is issued.
                    future::pending::<()>().await;
                }
                let entry = stopped.syscall_entry()?;
                let expected = [
                    args.arg0 as u64,
                    args.arg1 as u64,
                    args.arg2 as u64,
                    args.arg3 as u64,
                    args.arg4 as u64,
                    args.arg5 as u64,
                ];
                if entry.seccomp
                    || entry.number != nr as u64
                    || entry.arguments != expected
                    || stopped.getregs()?.ip() as usize
                        != cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE
                {
                    return Err(Errno::EPROTO.into());
                }
                self.global_state
                    .fatal_session
                    .source_epoch
                    .observe(nr, args);
                #[cfg(target_arch = "x86_64")]
                if self.private_signal.frame.is_some() {
                    original_context::check_entry(
                        &stopped,
                        nr,
                        args,
                        (cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE) as u64,
                        regs.stack_ptr(),
                        false,
                    )?;
                }
                // Every opted-in injection has reached the authenticated ENTRY
                // above, including calls without a private signal frame.
                self.observe_injected_syscall(
                    observe_tool.then_some((nr, args)),
                    InjectedSyscallEvent::Entered,
                );
                // Only this validated ENTRY establishes a native invocation.
                // Preparation or an earlier signal stop establishes no effect.
                cohort_native = self
                    .cohort
                    .as_ref()
                    .and_then(|member| member.native(nr, args));
                // Continue this SAME original private attempt from ENTRY.
                self.syscall_stopped(stopped, None)?.next_state().await?
            }
            Wait::Stopped(stopped, Event::Signal(sig)) if self.cohort.is_some() => {
                self.arm_liteinst_root_stop(&stopped, &Event::Signal(sig));
                // No private ENTRY or return exists. The explicit Tool contract
                // either drains this same continuation or settles its exact Read;
                // unsupported/opaque paths still fail under original custody.
                #[cfg(target_arch = "x86_64")]
                return match self
                    .continue_private_signal(
                        stopped,
                        sig,
                        nr,
                        args,
                        oldregs,
                        private_signal::ContinuationOptions {
                            observe_tool,
                            recorded: false,
                        },
                    )
                    .await
                {
                    Ok(result) => Ok(result),
                    Err(error) => {
                        self.publish_ordinary_failure(
                            "ptrace private syscall before ENTRY",
                            anyhow::anyhow!("logical signal continuation is unsupported: {error}")
                                .into(),
                        );
                        future::pending().await
                    }
                };
                #[cfg(not(target_arch = "x86_64"))]
                {
                    self.publish_ordinary_failure("ptrace private syscall before ENTRY",
                        anyhow::anyhow!(
                            "private syscall {nr:?} interrupted by {sig:?} before validated ENTRY; logical signal continuation is unsupported"
                        ).into());
                    return future::pending().await;
                }
            }
            other => other,
        };
        self.arm_liteinst_wait(&wait);

        #[cfg(target_arch = "x86_64")]
        if self.private_signal.frame.is_some() {
            // A finite drain accepts only this helper's real same-frame EXIT;
            // a second signal/child/exec cannot fall through the legacy helper
            // branch which models a restart errno.
            let Wait::Stopped(stopped, Event::Syscall) = &wait else {
                return Err(Errno::ENOTSUPP.into());
            };
            let actual = stopped.getregs()?;
            let raw = stopped.syscall_exit_result()?;
            if actual.orig_rax != nr as u64
                || actual.args() != regs.args()
                || actual.rsp != regs.rsp
                || actual.rip != (cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE) as u64
                || actual.rax as i64 != raw
            {
                return Err(Errno::EPROTO.into());
            }
        }

        // Get the result of the syscall to return to the caller.
        let result = self
            .status_to_result(
                wait,
                Some(oldregs),
                child_context,
                observe_tool.then_some((nr, args)),
                cohort_native,
            )
            .await?;
        self.observe_liteinst_mapping_result(nr, args, result);
        Ok(result)
    }

    // Replace an actual, unconverted seccomp entry. The caller must have taken
    // its pending record; this is not valid for a stopped task with no original
    // syscall left to consume (for example, a post-exec callback).
    async fn private_inject(
        &mut self,
        task: Stopped,
        nr: Sysno,
        args: SyscallArgs,
        observe_tool: bool,
    ) -> Result<Result<i64, Errno>, TraceError> {
        let task = self.skip_seccomp_syscall(task).await?;

        self.untraced_syscall(task, nr, args, observe_tool).await
    }

    async fn status_to_result(
        &mut self,
        wait_status: Wait,
        context: Option<libc::user_regs_struct>,
        child_context: Option<libc::user_regs_struct>,
        observation: Option<(Sysno, SyscallArgs)>,
        cohort_native: Option<source_cohort::NativeOperation>,
    ) -> Result<Result<i64, Errno>, TraceError> {
        #[cfg(test)]
        let forced_external_sigtrap = matches!(&wait_status, Wait::Stopped(_, _))
            && self.liteinst_runtime.lock().unwrap().phase == LiteinstRuntimePhase::Waiting
            && self
                .global_state
                .liteinst_runtime
                .as_ref()
                .is_some_and(|runtime| {
                    let force_once = if context.is_none() {
                        runtime.force_context_none_signal_once.as_ref()
                    } else {
                        runtime.force_context_signal_once.as_ref()
                    };
                    force_once.is_some_and(|force_once| force_once.swap(false, Ordering::SeqCst))
                });
        #[cfg(not(test))]
        let forced_external_sigtrap = false;
        #[cfg(test)]
        let wait_status = if forced_external_sigtrap {
            match wait_status {
                Wait::Stopped(task, _) => Wait::Stopped(task, Event::Signal(Signal::SIGTRAP)),
                other => other,
            }
        } else {
            wait_status
        };
        #[cfg(test)]
        if context.is_some()
            && self.liteinst_runtime.lock().unwrap().phase == LiteinstRuntimePhase::Waiting
            && let Wait::Stopped(stopped, _) = &wait_status
            && self
                .global_state
                .liteinst_runtime
                .as_ref()
                .and_then(|runtime| runtime.force_private_stub_mutation_once.as_ref())
                .is_some_and(|force_once| force_once.swap(false, Ordering::SeqCst))
        {
            let mut mutated_stub = [0; cp::SYSCALL_INSTR_SIZE * 2];
            stopped.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut mutated_stub)?;
            mutated_stub[0] ^= 0xff;
            let mut stopped_writer = Stopped::new_unchecked(stopped.pid());
            let address = AddrMut::from_raw(cp::PRIVATE_PAGE_OFFSET).ok_or(Errno::EFAULT)?;
            stopped_writer.write_value(address, &mutated_stub)?;
        }
        match wait_status {
            Wait::Stopped(stopped, event) => match event {
                Event::Signal(sig) if context.is_none() => {
                    self.validate_nested_liteinst_activation_signal(
                        &stopped,
                        sig,
                        LiteinstActivationOperation::FinishReinjectedSyscall,
                        NestedTrapExpectation::None,
                        forced_external_sigtrap,
                    )?;
                    let regs = stopped.getregs()?;
                    Ok(Ok(regs.ret() as i64))
                }
                Event::Signal(sig) => {
                    self.validate_nested_liteinst_activation_signal(
                        &stopped,
                        sig,
                        LiteinstActivationOperation::FinishInjectedSyscall,
                        NestedTrapExpectation::PrivateSyscall(
                            (cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE) as u64,
                        ),
                        forced_external_sigtrap,
                    )?;
                    let mut regs = stopped.getregs()?;
                    let cohort_return = cohort_native.and_then(|owner| {
                        (sig == Signal::SIGTRAP && !forced_external_sigtrap)
                            .then(|| owner.private_return(&stopped, regs.ret() as i64))
                            .flatten()
                    });
                    // NB: it is possible to get interrupted by signal (such as
                    // SIGCHLD) before single step finishes, while RIP still
                    // points at the private page.
                    debug_assert!(
                        regs.ip() as usize == cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE
                            || regs.ip() as usize == cp::PRIVATE_PAGE_OFFSET
                    );
                    if observation.is_some()
                        && sig == Signal::SIGTRAP
                        && is_expected_private_syscall_trap(
                            &stopped,
                            (cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE) as u64,
                            forced_external_sigtrap,
                        )
                        .unwrap_or(false)
                    {
                        self.observe_injected_syscall(
                            observation,
                            InjectedSyscallEvent::Returned(regs.ret() as i64),
                        );
                    }
                    // interrupted by signal, return -ERESTARTSYS so that tracee can do a
                    // restart_syscall.
                    if sig != Signal::SIGTRAP {
                        *regs.ret_mut() = (-(Errno::ERESTARTSYS.into_raw()) as i64) as u64;
                        self.pending_signal = Some(sig);
                    }
                    let result = Errno::from_ret(regs.ret() as usize).map(|x| x as i64);
                    if let Some(context) = context {
                        if child_context.is_some() {
                            // An injected-frame event temporarily replaces the
                            // controller's live trap registers with the logical
                            // guest frame. Restore every controller register;
                            // leaving even a callee-saved register (notably R12,
                            // used by LiteInst as its HookContext base) would
                            // corrupt the callback that resumes after injection.
                            stopped.setregs(&context)?;
                        } else {
                            // Restore syscall args to original values. This is
                            // needed when we convert syscalls like SYS_open ->
                            // SYS_openat, syscall args are modified need to restore
                            // it back.
                            restore_context(&stopped, context, None, false)?;
                        }
                    }
                    if let Some(receipt) = cohort_return {
                        receipt.restored();
                    }
                    Ok(result)
                }
                Event::NewChild(op, child) => {
                    let ret = child.pid().as_raw() as i64;
                    self.observe_injected_syscall(
                        observation,
                        InjectedSyscallEvent::ChildCreated(child.pid()),
                    );
                    let (_, native_return) = self
                        .dispatch_new_task(op, stopped, child, context, child_context, observation)
                        .await?;
                    if native_return.is_some()
                        && let Some(owner) = cohort_native
                    {
                        owner.child_returned();
                    }
                    Ok(Errno::from_ret(native_return.unwrap_or(ret) as usize)
                        .map(|value| value as i64))
                }
                Event::Exec(former_tid) => {
                    // Ordinary injection can execute the initial Command exec.
                    // Forward this exact stopped task to the existing run loop,
                    // which drops this handler before invoking fallible Tool
                    // callbacks. No image instruction is stepped here, and a
                    // Tool error never passes through the TraceError-only API.
                    self.execve(Wait::Stopped(stopped, Event::Exec(former_tid)))
                        .await
                }
                Event::Syscall => {
                    let raw = stopped.syscall_exit_result()?;
                    let regs = stopped.getregs()?;
                    if regs.ret() as i64 != raw {
                        return Err(Errno::EPROTO.into());
                    }
                    let cohort_return =
                        cohort_native.and_then(|owner| owner.syscall_return(&stopped, raw));
                    self.observe_injected_syscall(observation, InjectedSyscallEvent::Returned(raw));
                    if let Some(context) = context {
                        if child_context.is_some() {
                            stopped.setregs(&context)?;
                        } else {
                            restore_context(&stopped, context, None, false)?;
                        }
                    }
                    if let Some(receipt) = cohort_return {
                        receipt.restored();
                    }
                    Ok(Errno::from_ret(raw as usize).map(|x| x as i64))
                }
                Event::Seccomp => Err(Errno::EPROTO.into()),
                st => panic!("untraced_syscall returned unknown state: {:?}", st),
            },
            Wait::Exited(_pid, exit_status) => self.exit(exit_status).await,
        }
    }

    // Use the same execution, signal and abort machinery as Guest::inject,
    // with the same EINTR/ERESTARTSYS retry rule as Guest::inject_with_retry.
    // Only the caller origin differs: setup must not publish a Tool receipt.
    pub(crate) async fn inject_backend_with_retry<S: SyscallInfo>(
        &mut self,
        syscall: S,
    ) -> Result<i64, Errno> {
        if self.interrupted_read.is_some() {
            return Err(Errno::EPROTO);
        }
        loop {
            let (nr, args) = syscall.into_parts();
            match Box::pin(self.do_inject(nr, args, InjectionOrigin::Backend)).await {
                Ok(value) => return Ok(value),
                Err(Errno::EINTR) | Err(Errno::ERESTARTSYS) => continue,
                Err(error) => return Err(error),
            }
        }
    }

    async fn do_inject(
        &mut self,
        nr: Sysno,
        args: SyscallArgs,
        origin: InjectionOrigin,
    ) -> Result<i64, Errno> {
        #[cfg(target_arch = "x86_64")]
        if self.private_signal.consulting
            || self.private_signal.frame.is_some()
            || self.private_signal.read.is_some()
            || self.private_signal.completing.is_some()
        {
            // A hook cannot recursively inject to find its own checkpoint, and
            // the finite-drain promise permits no second helper after this one.
            self.publish_ordinary_failure(
                "private continuation injection",
                anyhow::anyhow!("unapproved nested injection during private handback").into(),
            );
            return future::pending().await;
        }
        // Interrupted Read custody owns the exact kernel stop until the Tool
        // hands its ticket back and the callback consumes it.  Any injection
        // would resume or replace that stop before the handback contract can
        // validate it.  Refuse through the fallible Guest API before emitting
        // Prepared or performing a native effect.
        if self.interrupted_read.is_some() {
            return Err(Errno::EPROTO);
        }
        match self.inner_inject(nr, args, origin).await {
            Ok(ret) => ret,
            Err(err) => self.abort(Err(err)).await,
        }
    }

    async fn inner_inject(
        &mut self,
        nr: Sysno,
        args: SyscallArgs,
        origin: InjectionOrigin,
    ) -> Result<Result<i64, Errno>, TraceError> {
        let task = self.assume_stopped();
        let observe_tool =
            origin == InjectionOrigin::Tool && L::observe_injected_syscalls(&self.global_state.cfg);
        #[cfg(target_arch = "x86_64")]
        let original_setsockopt = self.original_setsockopt_entry.take();
        // Same retained injection owner as Returned. No await/resume precedes
        // this preparation fact, and it cannot supply an execution result.
        self.observe_injected_syscall(
            (observe_tool && L::observe_injected_syscall_preparation(&self.global_state.cfg))
                .then_some((nr, args)),
            InjectedSyscallEvent::Prepared,
        );

        tracing::debug!(
            "[tool] (tid {}) beginning inject of syscall: {}, args {:?}",
            self.tid(),
            nr,
            args,
        );

        #[cfg(target_arch = "x86_64")]
        if let Some(original) = original_setsockopt::route_optval_rewrite(
            origin,
            self.pending_syscall,
            nr,
            args,
            self.injected_syscall_frame.is_some(),
            self.pending_syscall_already_skipped,
            original_setsockopt,
        )? {
            original.validate(&task)?;
            let (_, original_args) = self.pending_syscall.ok_or(Errno::EPROTO)?;
            let (task, context) = self
                .take_original_entry(nr, args, Some(original_args))?
                .ok_or(Errno::EPROTO)?;
            let observation = observe_tool.then_some((nr, args));
            // Prepared was emitted above. This is the SAME original entry;
            // do not skip it or create an administrative private attempt.
            self.observe_injected_syscall(observation, InjectedSyscallEvent::Entered);
            return self
                .finish_entered_original(task, nr, args, context, observation)
                .await;
        }

        if self.injected_syscall_frame.is_some() || self.pending_syscall_already_skipped {
            self.pending_syscall = None;
            self.original_read_entry = None;
            self.untraced_syscall(task, nr, args, observe_tool).await
        } else {
            self.original_read_entry = None;
            match self.pending_syscall.take() {
                Some(original) if original == (nr, args) => {
                    // Run the exact pending syscall and stop at its exit.
                    self.validate_liteinst_mapping_execution(nr, args)?;
                    #[cfg(target_arch = "x86_64")]
                    if observe_tool {
                        // The pending tuple alone cannot authenticate ENTRY:
                        // the Tool may have changed the actual registers. Bind
                        // this consumed original stop's task generation, then
                        // check the kernel's native SECCOMP tuple and IP/SP.
                        // This grants no source permission and resets no epoch.
                        // https://github.com/rrnewton/reverie/issues/899
                        let source_stop = self.source_stop.as_ref().ok_or(Errno::EPROTO)?;
                        if !source_stop.same_generation(&task.terminal_cleanup()) {
                            return Err(Errno::EPROTO.into());
                        }
                        let regs = task.getregs()?;
                        original_context::check_entry(
                            &task,
                            nr,
                            args,
                            regs.ip(),
                            regs.stack_ptr(),
                            true,
                        )?;
                        // No helper, resume or suspension separates the actual
                        // boundary from the observation in the owned Tool state.
                        self.observe_injected_syscall(
                            Some((nr, args)),
                            InjectedSyscallEvent::Entered,
                        );
                    }
                    #[cfg(target_arch = "x86_64")]
                    let cohort_native = self
                        .source_context(Some(&task))
                        .and_then(|context| context.native(&task, nr, args));
                    #[cfg(not(target_arch = "x86_64"))]
                    let cohort_native = self
                        .cohort
                        .as_ref()
                        .and_then(|member| member.native(nr, args));
                    let wait = self.syscall_stopped(task, None)?.next_state().await?;
                    self.arm_liteinst_wait(&wait);
                    let result = self
                        .status_to_result(
                            wait,
                            None,
                            None,
                            observe_tool.then_some((nr, args)),
                            cohort_native,
                        )
                        .await?;
                    self.observe_liteinst_mapping_result(nr, args, result);
                    Ok(result)
                }
                Some(_) => self.private_inject(task, nr, args, observe_tool).await,
                None => self.untraced_syscall(task, nr, args, observe_tool).await,
            }
        }
    }

    async fn do_tail_inject(&mut self, nr: Sysno, args: SyscallArgs) -> ! {
        match self.inner_tail_inject(nr, args).await {
            Ok(_) => {
                // Drop the handle_syscall_event future.
                self.cancel_handler.store(true, Ordering::SeqCst);
                future::pending().await
            }
            Err(err) => self.abort(Err(err)).await,
        }
    }

    async fn inner_tail_inject(
        &mut self,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<Result<i64, Errno>, TraceError> {
        let tid = self.tid();

        tracing::info!(
            "[tool] (tid {}) beginning tail_inject of syscall: {}",
            &tid,
            nr,
        );

        let task = self.assume_stopped();

        if self.injected_syscall_frame.is_some() {
            self.pending_syscall = None;
            self.original_read_entry = None;
            let result = self.untraced_syscall(task, nr, args, false).await?;
            let task = self.assume_stopped();
            self.write_injected_syscall_result(&task, result)?;
            return Ok(result);
        }

        if self.pending_syscall_already_skipped {
            self.pending_syscall = None;
            self.original_read_entry = None;
            let result = self.untraced_syscall(task, nr, args, false).await?;
            let task = self.assume_stopped();
            set_ret(
                &task,
                result.unwrap_or_else(|errno| -(errno.into_raw() as i64)) as u64,
            )?;
            return Ok(result);
        }

        if is_liteinst_mapping_syscall(nr) && self.pending_syscall == Some((nr, args)) {
            self.pending_syscall = None;
            self.original_read_entry = None;
            let result = self.private_inject(task, nr, args, false).await?;
            let task = self.assume_stopped();
            set_ret(
                &task,
                result.unwrap_or_else(|errno| -(errno.into_raw() as i64)) as u64,
            )?;
            return Ok(result);
        }

        self.original_read_entry = None;

        match self.pending_syscall.take() {
            Some(original) if original == (nr, args) => {
                // The callback is cancelled next. Its final resume still owns
                // execution of this original syscall; no second skip is due.
                Ok(Ok(0))
            }
            Some(_) => self.private_inject(task, nr, args, false).await,
            None => self.untraced_syscall(task, nr, args, false).await,
        }
    }

    /// Get a ptrace stub which can do ptrace operations
    // Assumption: Task is in stopped state as long as we have a valid
    // reference to `TracedTask`.
    fn assume_stopped(&self) -> Stopped {
        Stopped::new_unchecked(self.tid())
    }

    async fn notify_gdb_stop(&self, reason: StopReason) -> Result<(), TraceError> {
        if !self.attached_by_gdb {
            return Ok(());
        }

        if let Some(stop_tx) = self.gdb_stop_tx.as_ref() {
            let request_tx = self.gdb_request_tx.clone();
            let resume_tx = self.gdb_resume_tx.clone();
            let stop = StoppedInferior {
                reason,
                request_tx: request_tx.ok_or(Errno::EIO)?,
                resume_tx: resume_tx.ok_or(Errno::EIO)?,
            };
            if stop_tx.send(stop).await.is_err() {
                tracing::warn!(
                    tid = %self.tid(),
                    "GDB stop channel closed while reporting tracee stop"
                );
            }
        }
        Ok(())
    }

    async fn handle_gdb_request(&mut self, request: Option<GdbRequest>) {
        if let Some(request) = request {
            match request {
                GdbRequest::SetBreakpoint(bkpt, reply_tx) => {
                    if bkpt.ty == BreakpointType::Software {
                        let result = self.add_breakpoint(bkpt.addr).await;
                        let _ = reply_tx.send(result);
                    }
                }
                GdbRequest::RemoveBreakpoint(bkpt, reply_tx) => {
                    if bkpt.ty == BreakpointType::Software {
                        let result = self.remove_breakpoint(bkpt.addr).await;
                        let _ = reply_tx.send(result);
                    }
                }
                GdbRequest::ReadInferiorMemory(addr, length, reply_tx) => {
                    let result = self.read_inferior_memory(addr, length);
                    let _ = reply_tx.send(result);
                }
                GdbRequest::WriteInferiorMemory(addr, length, data, reply_tx) => {
                    let result = self.write_inferior_memory(addr, length, data);
                    let _ = reply_tx.send(result);
                }
                GdbRequest::ReadRegisters(reply_tx) => {
                    let result = self.read_registers();
                    let _ = reply_tx.send(result);
                }
                GdbRequest::WriteRegisters(core_regs, reply_tx) => {
                    let result = self.write_registers(core_regs);
                    let _ = reply_tx.send(result);
                }
            }
        }
    }

    async fn handle_gdb_resume(
        &mut self,
        resume: Option<ResumeInferior>,
        task: Stopped,
        resume_action: ExpectedGdbResume,
    ) -> Result<(TaskRunning, Option<ResumeInferior>), TraceError> {
        match resume {
            None => Ok((self.resume_stopped(task, None)?, None)),
            Some(resume) => {
                let is_resume = resume_action == ExpectedGdbResume::Resume || resume.detach;
                let is_step_only = resume_action == ExpectedGdbResume::StepOnly;
                // During a step-over, gdb normally single-steps over the
                // breakpoint installed at the current PC. But if gdb has already
                // removed that breakpoint it issues a plain continue instead of a
                // single-step. This happens, for example, after `finish`: gdb
                // implements it with a temporary breakpoint at the return address
                // which it deletes as soon as it is hit, so when the user then
                // resumes there is no breakpoint left to step over. No step-over
                // is required in that case, so resume normally rather than
                // treating the continue as an unexpected action (which used to
                // panic here).
                let is_step_over = resume_action == ExpectedGdbResume::StepOver;
                let running = match resume.action {
                    ResumeAction::Step(sig) => self.step_stopped(task, sig)?,
                    ResumeAction::Continue(sig) if is_resume => self.resume_stopped(task, sig)?,
                    ResumeAction::Continue(sig) if is_step_only => self.step_stopped(task, sig)?,
                    ResumeAction::Continue(sig) if is_step_over => {
                        self.resume_stopped(task, sig)?
                    }
                    action => panic!(
                        "[pid = {}] unexpected resume action {:?}, expecting: {:?}",
                        task.pid(),
                        action,
                        resume_action,
                    ),
                };
                Ok((running, Some(resume)))
            }
        }
    }

    async fn await_gdb_resume(
        &mut self,
        task: Stopped,
        resume_action: ExpectedGdbResume,
    ) -> Result<TaskRunning, TraceError> {
        if !self.attached_by_gdb {
            return self.resume_stopped(task, None);
        }

        let mut resume_rx = self.gdb_resume_rx.take().ok_or(Errno::EIO)?;
        let mut gdb_request_rx = self.gdb_request_rx.take().ok_or(Errno::EIO)?;

        let mut resume_future = Box::pin(resume_rx.recv());

        let (running, resumed) = loop {
            let request_future = Box::pin(gdb_request_rx.recv());

            match future::select(request_future, resume_future).await {
                Either::Left((gdb_request, pending_resume_future)) => {
                    self.handle_gdb_request(gdb_request).await;
                    resume_future = pending_resume_future;
                }
                Either::Right((resume_request, _)) => {
                    break self
                        .handle_gdb_resume(resume_request, task, resume_action)
                        .await?;
                }
            }
        };

        self.gdb_request_rx = Some(gdb_request_rx);
        self.gdb_resume_rx = Some(resume_rx);

        if let Some(resumed) = resumed {
            if resumed.detach {
                tracing::debug!(
                    target: "reverie_ptrace::lifecycle",
                    parent: &tracing::debug_span!(
                        target: "reverie_ptrace::lifecycle",
                        "tracee.detach",
                        tid = %self.tid(),
                        reason = "GDB detach"
                    ),
                    "GDB detached from tracee"
                );
                // no longer report stop event to gdb
                // self.gdb_stop_tx = None;
                self.attached_by_gdb = false;
            }

            self.resumed_by_gdb = Some(resumed.action);
        }

        Ok(running)
    }

    /// Resume from a software breakpoint set by gdb. The resume action is
    /// initiated from gdb (client).
    // NB: caller to %rip accordingly prior to hitting breakpoint.
    async fn resume_from_swbreak(
        &mut self,
        task: Stopped,
        regs: libc::user_regs_struct,
    ) -> Result<Wait, TraceError> {
        task.setregs(&regs)?;

        // Task could be hitting a breakpoint, after previously suspended by
        // a different task, need to notify this task is fully stopped.
        self.suspended.store(true, Ordering::SeqCst);
        if let Some((suspended_flag, stop_tx)) = self.get_stop_tx().await
            && stop_tx
                .send((
                    self.tid(),
                    Suspended {
                        waker: None,
                        suspended: suspended_flag,
                    },
                ))
                .await
                .is_err()
        {
            tracing::warn!(
                    tid = %self.tid(),
                    "tracee freeze channel closed during GDB breakpoint handling"
            );
        }

        // When resuming from breakpoint, gdb (client) needs to remove the
        // breakpoint (implying restore the original instruction), do a
        // single-step (step-over), and re-insert the breakpoint.
        // Because removing (sw) breakpoint modifies the instructions, other
        // thread might miss the breakpoint after the breakpoint is removed
        // and before the breakpoint is (re-)inserted. Hence we must make
        // serialize this sequence.
        let needs_step_over = self.needs_step_over.clone();
        let _guard = needs_step_over.lock().await;

        self.notify_gdb_stop(StopReason::stopped(
            task.pid(),
            self.pid(),
            StopEvent::SwBreak,
            regs.into(),
        ))
        .await?;

        self.freeze_all().await?;

        let running = self
            .await_gdb_resume(task, ExpectedGdbResume::StepOver)
            .await?;

        // If gdb removed the breakpoint at the current PC and issued a plain
        // continue instead of the usual step-over single-step (e.g. after a
        // `finish` temporary breakpoint was hit and deleted, and the user then
        // continues), there is no intermediate single-step stop to report back
        // to gdb. Just run to the next event and return it directly. The task
        // may run all the way to exit in this case, so we must not assume it
        // stops again.
        if !matches!(self.resumed_by_gdb, Some(ResumeAction::Step(_))) {
            // Release the siblings frozen above *before* waiting for this
            // task's next event. gdb has resumed everything, and this task may
            // now block on a sibling -- a join, a futex, a pipe read -- which a
            // frozen sibling can never satisfy. Waiting first deadlocks the
            // guest.
            self.thaw_all().await?;
            let wait = running.next_state().await?;
            self.arm_liteinst_wait(&wait);
            return Ok(wait);
        }

        let wait = running.next_state().await?.assume_stopped();
        let mut task = wait.0;
        let mut event = wait.1;
        self.arm_liteinst_root_stop(&task, &event);

        // Detached by client.
        if !self.attached_by_gdb {
            self.thaw_all().await?;
            return Ok(Wait::Stopped(task, event));
        }

        task = loop {
            match event {
                Event::Signal(Signal::SIGTRAP) => break task,
                Event::Signal(Signal::SIGSTOP) => {
                    let running = self.step_stopped(task, None)?;
                    let wait = running.next_state().await?.assume_stopped();
                    task = wait.0;
                    event = wait.1;
                    self.arm_liteinst_root_stop(&task, &event);
                }
                // TODO: combine with handle_signal!
                Event::Signal(Signal::SIGCHLD) => {
                    let running = self.step_stopped(task, Signal::SIGCHLD)?;
                    let wait = running.next_state().await?.assume_stopped();
                    task = wait.0;
                    event = wait.1;
                    self.arm_liteinst_root_stop(&task, &event);
                }
                unknown => panic!("[pid = {}] got unexpected event {:?}", self.tid(), unknown),
            }
        };
        self.notify_gdb_stop(StopReason::stopped(
            task.pid(),
            self.pid(),
            StopEvent::Signal(Signal::SIGTRAP),
            task.getregs()?.into(),
        ))
        .await?;

        let running = self
            .await_gdb_resume(task, ExpectedGdbResume::Resume)
            .await?;
        // Same ordering requirement as the plain-continue path above: the
        // step-over is finished and the breakpoint is back in place, so the
        // siblings must be released before this task's next event is awaited.
        self.thaw_all().await?;
        let wait = running.next_state().await?;
        self.arm_liteinst_wait(&wait);
        Ok(wait)
    }

    /// check if the stop is caused by sw breakpoint.
    async fn check_swbreak(&mut self, wait: Wait) -> Result<Wait, TraceError> {
        self.arm_liteinst_wait(&wait);
        match wait {
            Wait::Stopped(task, event) if event == Event::Signal(Signal::SIGTRAP) => {
                let mut regs = task.getregs()?;
                let rip_minus_one = regs.ip() - 1;
                if self.breakpoints.contains_key(&rip_minus_one) {
                    *regs.ip_mut() = rip_minus_one;
                    self.resume_from_swbreak(task, regs).await
                } else {
                    Ok(Wait::Stopped(task, event))
                }
            }
            other => Ok(other),
        }
    }

    async fn add_breakpoint(&mut self, addr: u64) -> Result<(), TraceError> {
        if let Some(bkpt_addr) = AddrMut::from_raw(addr as usize) {
            let mut task = self.assume_stopped();
            let saved_insn: u64 = task.read_value(bkpt_addr)?;
            let insn = (saved_insn & !0xffu64) | 0xccu64;
            task.write_value(bkpt_addr, &insn)?;
            self.breakpoints.insert(addr, saved_insn);
        }
        Ok(())
    }

    /// thaw all threads.
    async fn thaw_all(&mut self) -> Result<(), TraceError> {
        for (_pid, suspended_task) in core::mem::take(&mut self.suspended_tasks) {
            if let Some(tx) = suspended_task.waker.as_ref() {
                suspended_task.suspended.store(false, Ordering::SeqCst);
                let _sent = tx.try_send(self.tid());
            }
        }
        Ok(())
    }

    /// freeze all threads, except the caller.
    async fn freeze_all(&mut self) -> Result<(), TraceError> {
        // The tool have chosen to sequentialize thread execution, gdbserver
        // should avoid doing its own thread serialization, otherwise this
        // could lead to deadlock.
        if *self.global_state.sequentialized_guest {
            return Ok(());
        }
        let (stop_tx, mut stop_rx) = mpsc::channel(1);
        for child in self.child_threads.lock().await.deref_mut().into_iter() {
            if child.id() != self.tid() && !child.suspended.load(Ordering::SeqCst) {
                let killed = Errno::result(unsafe {
                    libc::syscall(libc::SYS_tgkill, self.pid(), child.id(), Signal::SIGSTOP)
                });
                if killed.is_ok() {
                    child.suspended.store(true, Ordering::SeqCst);
                    child.wait_all_stop_tx = Some(stop_tx.clone());
                }
            }
        }
        drop(stop_tx);
        while let Some((pid, suspended_task)) = stop_rx.recv().await {
            self.suspended_tasks.insert(pid, suspended_task);
        }
        Ok(())
    }

    async fn remove_breakpoint(&mut self, addr: u64) -> Result<(), TraceError> {
        let insn = self.breakpoints.remove(&addr).ok_or(Errno::ENOENT)?;
        let mut task = self.assume_stopped();
        if let Some(bkpt_addr) = AddrMut::from_raw(addr as usize) {
            task.write_value(bkpt_addr, &insn)?;
        }
        Ok(())
    }

    fn read_inferior_memory(&self, addr: u64, mut size: usize) -> Result<Vec<u8>, TraceError> {
        let task = self.assume_stopped();

        // NB: dont' trust size to be sane blindly.
        if size > 0x8000 {
            size = 0x8000;
        }

        let mut res = vec![0; size];
        if let Some(addr) = Addr::from_raw(addr as usize) {
            let nb = task.read(addr, &mut res)?;
            res.resize(nb, 0);
        }

        // There could be a software breakpoint within the address requested,
        // we should return the orignal contents without the breakpoint insn.
        // This is *not* documented in gdb remote protocol, however, both
        // gdbserver and rr does this. see:
        // rr: https://github.com/rr-debugger/rr/blob/master/src/GdbServer.cc#L561
        // gdbserver: https://github.com/bminor/binutils-gdb/blob/master/gdbserver/mem-break.cc#L1914
        for (bkpt, saved_insn) in self.breakpoints.iter() {
            if (addr..addr + res.len() as u64).contains(bkpt) {
                // This abuses bkpt insn 0xcc is single byte.
                res[*bkpt as usize - addr as usize] = *saved_insn as u8;
            }
        }

        Ok(res)
    }

    fn write_inferior_memory(
        &self,
        addr: u64,
        size: usize,
        data: Vec<u8>,
    ) -> Result<(), TraceError> {
        let mut task = self.assume_stopped();
        let size = std::cmp::min(size, data.len());
        let addr = AddrMut::from_raw(addr as usize).ok_or(Errno::EFAULT)?;
        task.write(addr, &data[..size])?;
        Ok(())
    }

    fn read_registers(&self) -> Result<CoreRegs, TraceError> {
        let task = self.assume_stopped();
        let regs = task.getregs()?;
        let fpregs = task.getfpregs()?;
        let core_regs = CoreRegs::from_parts(regs, fpregs);
        Ok(core_regs)
    }

    fn write_registers(&self, core_regs: CoreRegs) -> Result<(), TraceError> {
        let task = self.assume_stopped();
        let (regs, fpregs) = core_regs.into_parts();
        task.setregs(&regs)?;
        task.setfpregs(&fpregs)?;
        Ok(())
    }
}

#[async_trait]
impl<L: Tool + 'static> Guest<L> for TracedTask<L> {
    type Memory = Stopped;
    type Stack = GuestStack;

    #[inline]
    fn tid(&self) -> Pid {
        self.tid
    }

    #[inline]
    fn pid(&self) -> Pid {
        self.pid
    }

    #[inline]
    fn ppid(&self) -> Option<Pid> {
        self.ppid
    }

    fn is_command_bootstrap(&self) -> bool {
        self.command_bootstrap
    }

    fn memory(&self) -> Self::Memory {
        self.assume_stopped()
    }

    fn local_global_state(&self) -> Option<&L::GlobalState> {
        Some(self.global_state.gs_ref.as_ref())
    }

    fn inspect_original_read_range(
        &self,
        read: reverie::syscalls::Read,
    ) -> Result<reverie::OriginalReadRangeVerdict, reverie::Error> {
        self.inspect_native_read_range(read)
    }

    fn inspect_original_recvfrom_range(
        &self,
        receive: reverie::syscalls::Recvfrom,
    ) -> Result<reverie::OriginalReadRangeVerdict, reverie::Error> {
        self.inspect_native_recvfrom_range(receive)
    }

    async fn stage_followed_source(
        &mut self,
        address: usize,
        length: usize,
        retention: Box<dyn Send + Sync>,
    ) -> Result<Vec<u8>, reverie::syscalls::NativeUserReadError> {
        use reverie::syscalls::NativeUserReadError as E;
        use reverie::syscalls::NativeUserReadRefusal as R;
        let refused = |error| E::Refused(R::TargetState(error));
        let session = Arc::clone(&self.global_state.fatal_session);
        if !session.source_jobs.enabled()
            || session.is_failed()
            || self.cancel_handler.load(Ordering::Acquire)
        {
            return Err(refused(Errno::ECANCELED));
        }
        #[cfg(target_arch = "x86_64")]
        {
            let member = self
                .cohort
                .as_ref()
                .ok_or_else(|| E::Refused(R::UnsupportedBackend))?;
            if !session.source_jobs.idle() {
                return Err(refused(Errno::EBUSY));
            }
            let hold = Arc::new(member.acquire().map_err(refused)?);
            let plan = safeptrace::FollowedSourceReadPlan::prepare(hold.sender(), address, length)?;
            #[cfg(all(test, cohort_final_test))]
            let plan = source_cohort::hold_tests::prepare(plan, member);
            let observer = session
                .source_jobs
                .submit_followed(retention, hold, move || plan.run())?;
            let result = observer.await;
            if session.is_failed() || self.cancel_handler.load(Ordering::Acquire) {
                return Err(refused(Errno::ECANCELED));
            }
            result
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = (address, length, retention);
            Err(E::Refused(R::UnsupportedPlatform))
        }
    }

    async fn read_native_source(
        &mut self,
        address: usize,
        length: usize,
        retention: Box<dyn Send + Sync>,
    ) -> Result<Vec<u8>, reverie::syscalls::NativeUserReadError> {
        use reverie::syscalls::NativeUserReadError as E;
        use reverie::syscalls::NativeUserReadRefusal as R;
        let refused = |error| E::Refused(R::TargetState(error));
        let session = Arc::clone(&self.global_state.fatal_session);
        if !session.source_jobs.enabled()
            || session.is_failed()
            || self.cancel_handler.load(Ordering::Acquire)
        {
            return Err(refused(Errno::ECANCELED));
        }
        let stop = self
            .source_stop
            .as_ref()
            .cloned()
            .ok_or_else(|| E::Refused(R::UnsupportedBackend))?;
        session.source_epoch.validate(&stop).map_err(refused)?;
        #[cfg(target_arch = "x86_64")]
        {
            let acquisition = stop.begin_acquisition().map_err(refused)?;
            let plan = safeptrace::NativeSourceReadPlan::prepare(acquisition, address, length)?;
            let source = stop.clone();
            let observer = session.source_jobs.submit(
                retention,
                stop.clone(),
                session.source_epoch.clone(),
                move || {
                    source.validate_single_source_filter().map_err(refused)?;
                    plan.run()
                },
            )?;
            let result = observer.await;
            // Recheck after observer wake too: cancellation may have arrived
            // between registry retirement and this callback continuation.
            if session.is_failed() || self.cancel_handler.load(Ordering::Acquire) {
                return Err(refused(Errno::ECANCELED));
            }
            session.source_epoch.validate(&stop).map_err(refused)?;
            result
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = (address, length, retention);
            Err(E::Refused(R::UnsupportedPlatform))
        }
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        #[cfg(target_arch = "x86_64")]
        if let Some(regs) = self.private_logical_registers() {
            return regs;
        }
        let task = self.assume_stopped();

        match self.read_guest_registers(&task) {
            Ok(ret) => ret,
            Err(err) => self.abort(Err(err)).await,
        }
    }

    async fn set_regs(&mut self, regs: libc::user_regs_struct) -> Result<(), reverie::Error> {
        #[cfg(target_arch = "x86_64")]
        if self.set_private_logical_registers(regs)? {
            return Ok(());
        }
        let task = self.assume_stopped();

        if let Err(err) = self.write_guest_registers(&task, &regs) {
            // Mirror `regs()`: a ptrace register access failure aborts the task.
            self.abort(Err(err)).await;
        }
        Ok(())
    }

    async fn stack(&mut self) -> Self::Stack {
        match GuestStack::new(self.tid, self.stack_checked_out.clone()) {
            Ok(ret) => ret,
            Err(err) => self.abort(Err(err)).await,
        }
    }

    fn thread_state_mut(&mut self) -> &mut L::ThreadState {
        &mut self.thread_state
    }

    fn thread_state(&self) -> &L::ThreadState {
        &self.thread_state
    }

    fn claim_private_interruption(
        &mut self,
        ticket: &reverie::PrivateInterruption,
    ) -> Result<(), reverie::Error> {
        #[cfg(target_arch = "x86_64")]
        {
            self.claim_held_private_interruption(ticket)
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = ticket;
            Err(reverie::Error::Tool(anyhow::anyhow!(
                "backend has no matching private interruption"
            )))
        }
    }

    fn claim_private_read_completion(
        &mut self,
        ticket: &reverie::PrivateReadCompletion,
    ) -> Result<(), reverie::Error> {
        #[cfg(target_arch = "x86_64")]
        {
            self.claim_held_private_read_completion(ticket)
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = ticket;
            Err(reverie::Error::Tool(anyhow::anyhow!(
                "backend has no matching private Read completion"
            )))
        }
    }

    async fn await_recorded_private_interruption(
        &mut self,
        helper: reverie::syscalls::Syscall,
        signal: Signal,
    ) -> Result<reverie::Never, reverie::Error> {
        #[cfg(target_arch = "x86_64")]
        {
            self.wait_recorded_private_signal(helper, signal)
                .await
                .map_err(|error| {
                    reverie::Error::Tool(anyhow::anyhow!("recorded private interruption: {error}"))
                })
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = (helper, signal);
            Err(reverie::Error::Tool(anyhow::anyhow!(
                "backend has no recorded private interruption wait"
            )))
        }
    }

    async fn daemonize(&mut self) {
        let pid = self.pid();
        self.ndaemons.fetch_add(1, Ordering::SeqCst);
        self.is_a_daemon = true;

        tracing::info!("[reverie] daemonizing pid {} ..", pid);
        if self
            .daemonizer
            .send(self.daemon_kill_switch.subscribe())
            .await
            .is_err()
        {
            tracing::error!(%pid, "failed to notify orphan reaper while daemonizing tracee");
            self.ndaemons.fetch_sub(1, Ordering::SeqCst);
            self.is_a_daemon = false;
            return;
        }

        if self.ndaemons.load(Ordering::SeqCst) == self.ntasks.load(Ordering::SeqCst) {
            let _ = self.daemon_kill_switch.send(());
        }
    }

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno> {
        // Call a non-templatized function to reduce code bloat.
        let (nr, args) = syscall.into_parts();
        self.do_inject(nr, args, InjectionOrigin::Tool).await
    }

    async fn inject_original_read(
        &mut self,
        syscall: reverie::syscalls::Read,
    ) -> reverie::InjectedReadResult {
        #[cfg(target_arch = "x86_64")]
        {
            if self.private_signal.consulting
                || self.private_signal.completing.is_some()
                || self.private_signal.frame.is_some()
                || self.private_signal.read.is_some()
            {
                self.abort(Err(TraceError::Errno(Errno::EPROTO))).await;
            }
            let (nr, args) = syscall.into_parts();
            let outcome = self.inject_read_boundaries(nr, args).await;
            if self.pending_syscall.is_none() {
                self.original_read_entry = None;
            }
            match outcome {
                Ok(outcome) => outcome,
                Err(error) => self.abort(Err(error)).await,
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            reverie::InjectedReadResult::Complete(self.inject(syscall).await)
        }
    }

    async fn inject_original_sendto_with_stopped_peers(
        &mut self,
        syscall: reverie::syscalls::Sendto,
    ) -> Result<i64, reverie::Error> {
        #[cfg(target_arch = "x86_64")]
        {
            if self.private_signal.consulting
                || self.private_signal.completing.is_some()
                || self.private_signal.frame.is_some()
                || self.private_signal.read.is_some()
            {
                self.abort(Err(TraceError::Errno(Errno::EPROTO))).await;
            }
            match self.inject_peer_sendto_entry(syscall).await {
                Ok(result) => result.map_err(Into::into),
                Err(error) => self.abort(Err(error)).await,
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = syscall;
            Err(reverie::Error::Tool(anyhow::anyhow!(
                "backend has no peer-held original Sendto"
            )))
        }
    }

    async fn inject_epoll_ctl_copy(
        &mut self,
        syscall: reverie::syscalls::EpollCtl,
    ) -> Result<i64, reverie::Error> {
        #[cfg(target_arch = "x86_64")]
        {
            if self.private_signal.consulting
                || self.private_signal.completing.is_some()
                || self.private_signal.frame.is_some()
                || self.private_signal.read.is_some()
            {
                self.abort(Err(TraceError::Errno(Errno::EPROTO))).await;
            }
            match self.inject_epoll_ctl_copy_entry(syscall).await {
                Ok(result) => result.map_err(Into::into),
                Err(error) => self.abort(Err(error)).await,
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = syscall;
            Err(reverie::Error::Tool(anyhow::anyhow!(
                "backend has no original epoll copy entry"
            )))
        }
    }

    async fn await_recorded_read_interruption(
        &mut self,
        call: reverie::syscalls::Read,
        signal: Signal,
    ) -> Result<reverie::InterruptedSyscall, reverie::Error> {
        #[cfg(target_arch = "x86_64")]
        {
            if self.private_signal.consulting
                || self.private_signal.completing.is_some()
                || self.private_signal.frame.is_some()
                || self.private_signal.read.is_some()
            {
                self.abort(Err(TraceError::Errno(Errno::EPROTO))).await;
            }
            let outcome = self.observe_recorded_read_signal(call, signal).await;
            if self.pending_syscall.is_none() {
                self.original_read_entry = None;
            }
            match outcome {
                Ok(ticket) => Ok(ticket),
                Err(error) => self.abort(Err(error)).await,
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = (call, signal);
            Err(Errno::ENOSYS.into())
        }
    }

    async fn finish_interrupted_syscall(
        &mut self,
        ticket: reverie::InterruptedSyscall,
        completed: Option<i64>,
    ) -> Result<(), reverie::Error> {
        #[cfg(target_arch = "x86_64")]
        {
            if self.private_signal.consulting
                || self.private_signal.completing.is_some()
                || self.private_signal.frame.is_some()
                || self.private_signal.read.is_some()
            {
                self.abort(Err(TraceError::Errno(Errno::EPROTO))).await;
            }
            self.prepare_interrupted_read_handback(&ticket, completed)
                .map_err(|error| {
                    reverie::Error::Tool(anyhow::anyhow!("interrupted Read handback: {error}"))
                })
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = (ticket, completed);
            Err(Errno::ENOSYS.into())
        }
    }

    #[allow(unreachable_code)]
    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        #[cfg(target_arch = "x86_64")]
        if self.private_signal.consulting
            || self.private_signal.completing.is_some()
            || self.private_signal.frame.is_some()
            || self.private_signal.read.is_some()
        {
            self.abort(Err(TraceError::Errno(Errno::EPROTO))).await;
        }
        if self.interrupted_read.is_some() {
            return self.abort(Err(TraceError::Errno(Errno::EPROTO))).await;
        }
        // Call a non-templatized function to reduce code bloat.
        let (nr, args) = syscall.into_parts();
        self.do_tail_inject(nr, args).await
    }

    fn set_timer(&mut self, sched: TimerSchedule) -> Result<(), reverie::Error> {
        let rcbs = match sched {
            TimerSchedule::Rcbs(r) => r,
            TimerSchedule::Time(dur) => Timer::as_ticks(dur),
            //if timer is imprecise there is no really a point in trying to single step any further than r
            TimerSchedule::RcbsAndInstructions(r, _) => r,
        };
        self.timer
            .request_event(TimerEventRequest::Imprecise(rcbs))?;
        Ok(())
    }

    fn set_timer_precise(&mut self, sched: TimerSchedule) -> Result<(), reverie::Error> {
        match sched {
            TimerSchedule::Rcbs(r) => self.timer.request_event(TimerEventRequest::Precise(r))?,
            TimerSchedule::Time(dur) => self
                .timer
                .request_event(TimerEventRequest::Precise(Timer::as_ticks(dur)))?,
            TimerSchedule::RcbsAndInstructions(r, i) => self
                .timer
                .request_event(TimerEventRequest::PreciseInstruction(r, i))?,
        };
        Ok(())
    }

    fn read_clock(&mut self) -> Result<u64, reverie::Error> {
        Ok(self.timer.read_clock())
    }

    fn backtrace(&mut self) -> Option<Backtrace> {
        use unwind::Accessors;
        use unwind::AddressSpace;
        use unwind::Byteorder;
        use unwind::Cursor;
        use unwind::PTraceState;
        use unwind::RegNum;

        let mut frames = Vec::new();

        let space = AddressSpace::new(Accessors::ptrace(), Byteorder::DEFAULT).ok()?;
        let state = PTraceState::new(self.tid.as_raw() as u32).ok()?;
        let mut cursor = Cursor::remote(&space, &state).ok()?;

        loop {
            let ip = cursor.register(RegNum::IP).ok()?;
            let is_signal = cursor.is_signal_frame().ok()?;

            frames.push(Frame { ip, is_signal });

            if !cursor.step().ok()? {
                break;
            }
        }

        // TODO: Take a snapshot of `/proc/self/maps` so the backtrace can be
        // processed offline?

        Some(Backtrace::new(self.tid(), frames))
    }

    fn has_cpuid_interception(&self) -> bool {
        self.has_cpuid_interception
    }
}

#[async_trait]
impl<L: Tool + 'static> GlobalRPC<L::GlobalState> for TracedTask<L> {
    async fn send_rpc<'a>(
        &'a self,
        args: <L::GlobalState as GlobalTool>::Request,
    ) -> <L::GlobalState as GlobalTool>::Response {
        let wrapped = WrappedFrom(self.tid(), &self.global_state);
        wrapped.send_rpc(args).await
    }

    fn config(&self) -> &<L::GlobalState as GlobalTool>::Config {
        &self.global_state.cfg
    }
}

/// Wrap a GlobalState with a Tid from which the messages originate.  This enables the
/// GlobalRPC instance below.
struct WrappedFrom<'a, G: GlobalTool>(Tid, &'a GlobalState<G>);

#[async_trait]
impl<'a, G: GlobalTool> GlobalRPC<G> for WrappedFrom<'a, G> {
    async fn send_rpc(&self, args: G::Request) -> G::Response {
        // In debugging mode we round-trip through a serialized representation
        // to make sure it works.
        let deserial = if cfg!(debug_assertions) {
            let serial = bincode::serde::encode_to_vec(&args, bincode::config::legacy())
                .expect("GlobalRPC request must serialize in debug validation mode");
            bincode::serde::decode_from_slice(&serial, bincode::config::legacy())
                .expect("serialized GlobalRPC request must deserialize in debug validation mode")
                .0
        } else {
            args
        };
        self.1.gs_ref.receive_rpc(self.0, deserial).await
    }
    fn config(&self) -> &G::Config {
        &self.1.cfg
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn callback_observation_refuses_typed_faults_and_only_defers_disappearance_with_gone_identity()
    {
        use safeptrace::ProcStatError as P;
        use safeptrace::StopObservationError as B;
        use safeptrace::StopSiginfo;

        use crate::PtraceCallbackRefusal as R;
        use crate::PtraceCallbackStop as S;
        // These are explicitly projected field failures, not claims that the
        // kernel returned these errno values in the real lifecycle fixtures.
        let info = StopSiginfo {
            signo: libc::SIGTRAP,
            code: 1541,
            sender_pid: 42,
            sender_uid: 1,
        };
        fn decide(
            binding: Option<B>,
            query: Option<Result<StopSiginfo, Errno>>,
            flags: Option<&Result<u32, P>>,
            live: Option<Result<bool, Errno>>,
        ) -> Result<bool, R> {
            super::callback_observation_decision(
                S::Signal(libc::SIGTRAP),
                binding,
                query,
                flags,
                live,
            )
        }

        for error in [Errno::EPERM, Errno::EIO, Errno::EINVAL] {
            assert_eq!(
                decide(None, Some(Err(error)), None, Some(Ok(false))),
                Err(R::Query(error))
            );
        }
        assert_eq!(
            decide(None, Some(Ok(info)), None, Some(Err(Errno::EPERM))),
            Err(R::Pidfd(Errno::EPERM))
        );
        for error in [
            P::Io(Errno::EPERM),
            P::Io(Errno::EIO),
            P::Io(Errno::EINTR),
            P::Format("short record"),
            P::PidMismatch,
        ] {
            for live in [false, true] {
                let result = Err(error.clone());
                assert_eq!(
                    decide(None, Some(Ok(info)), Some(&result), Some(Ok(live))),
                    Err(R::Proc(error.clone()))
                );
            }
        }
        for errno in [Errno::ESRCH, Errno::ENOENT] {
            let flags = Err(P::Io(errno));
            assert_eq!(
                decide(None, Some(Ok(info)), Some(&flags), Some(Ok(false))),
                Ok(true)
            );
            assert_eq!(
                decide(None, Some(Ok(info)), Some(&flags), Some(Ok(true))),
                Err(R::Proc(P::Io(errno)))
            );
        }
        for error in [
            B::WrongThread,
            B::GenerationMismatch,
            B::ExecEpochMismatch,
            B::PidMismatch,
            B::Identity(Errno::ENODATA),
        ] {
            assert_eq!(
                decide(Some(error), None, None, None),
                Err(R::Binding(error))
            );
        }
        assert_eq!(
            decide(None, None, None, None),
            Err(R::Inconsistent("missing siginfo query"))
        );
        assert_eq!(
            decide(None, Some(Ok(info)), None, None),
            Err(R::Inconsistent("missing final pidfd query"))
        );
        assert_eq!(
            decide(None, Some(Ok(info)), None, Some(Ok(true))),
            Err(R::Inconsistent("missing ambiguous EXIT flags"))
        );
        assert_eq!(
            decide(None, Some(Err(Errno::ESRCH)), None, Some(Ok(true))),
            Ok(true)
        );
        assert_eq!(
            decide(None, Some(Ok(info)), Some(&Ok(0)), Some(Ok(true))),
            Ok(false)
        );
        assert_eq!(
            decide(None, Some(Ok(info)), Some(&Ok(0x400)), Some(Ok(true))),
            Ok(true)
        );
    }

    #[test]
    fn command_bootstrap_arguments_preserve_types_and_raw_tail() {
        let args = super::SyscallArgs::new(0x1000, 0x2000, 0, 41, 0x3000, 43);
        let render = |nr, command_bootstrap| {
            format!(
                "{:?}",
                super::SyscallArgsForLog {
                    nr,
                    args,
                    command_bootstrap,
                }
            )
        };
        assert_eq!(
            render(super::Sysno::execve, true),
            "SyscallArgs { arg0: <hostaddr 0x1000>, arg1: <hostaddr 0x2000>, arg2: 0, arg3: 41, arg4: 12288, arg5: 43 }"
        );
        for nr in [
            super::Sysno::execve,
            super::Sysno::write,
            super::Sysno::execveat,
        ] {
            assert_eq!(render(nr, false), format!("{args:?}"));
        }
        assert_eq!(render(super::Sysno::write, true), format!("{args:?}"));
        assert_eq!(render(super::Sysno::execveat, true), format!("{args:?}"));
        let aliased = super::SyscallArgsForLog {
            nr: super::Sysno::execve,
            args: super::SyscallArgs::new(0x1000, 0x1000, 0, 41, 0x3000, 43),
            command_bootstrap: true,
        };
        assert_ne!(format!("{aliased:?}"), render(super::Sysno::execve, true));
    }

    use super::*;

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn syscall_skip_breakpoint_requires_exact_captured_provenance() {
        let exec_rip = 0x7f00_1234_530b;
        let arch_prctl_rip = 0x7f00_1234_cb19;
        let syscall_opcode = [0x0f, 0x05];
        assert!(is_expected_syscall_skip_breakpoint(
            libc::TRAP_BRKPT,
            exec_rip,
            exec_rip,
            syscall_opcode,
            0x48,
            false,
        ));
        assert!(is_expected_syscall_skip_breakpoint(
            libc::TRAP_BRKPT,
            arch_prctl_rip,
            arch_prctl_rip,
            syscall_opcode,
            0x48,
            false,
        ));

        for rejected in [
            is_expected_syscall_skip_breakpoint(
                libc::SI_USER,
                exec_rip,
                exec_rip,
                syscall_opcode,
                0x48,
                false,
            ),
            is_expected_syscall_skip_breakpoint(
                libc::TRAP_BRKPT,
                exec_rip,
                exec_rip + 1,
                syscall_opcode,
                0x48,
                false,
            ),
            is_expected_syscall_skip_breakpoint(
                libc::TRAP_BRKPT,
                exec_rip,
                exec_rip,
                [0xcc, 0x05],
                0x48,
                false,
            ),
            is_expected_syscall_skip_breakpoint(
                libc::TRAP_BRKPT,
                exec_rip,
                exec_rip,
                syscall_opcode,
                0xcc,
                false,
            ),
            is_expected_syscall_skip_breakpoint(
                libc::TRAP_BRKPT,
                exec_rip,
                exec_rip,
                syscall_opcode,
                0x48,
                true,
            ),
        ] {
            assert!(!rejected);
        }
    }

    fn active_state() -> LiteinstRuntimeState {
        let mut state = LiteinstRuntimeState::default();
        state.active_hooks.insert(
            0x401005,
            ActiveHookFootprint {
                site: GuestRange::new(0x401005, 8).unwrap(),
                trampoline: GuestRange::new(0x7000_1000, 0x1000).unwrap(),
                arena_writable: GuestRange::new(0x7100_0000, 0x80_000).unwrap(),
                arena_executable: GuestRange::new(0x7000_0000, 0x80_000).unwrap(),
            },
        );
        state
    }

    #[test]
    fn exec_generation_replaces_image_state_without_changing_old_holders() {
        let mut old = active_state();
        old.phase = LiteinstRuntimePhase::Ready;
        old.generation = 41;
        old.ready_generation = Some(41);
        old.frame = Some(LiteinstHandshakeFrame {
            begin_rip: 0x7000_1000,
            ..Default::default()
        });
        old.attempted_sites.insert(0x401005);
        old.fallback_sites
            .insert(0x401005, LiteinstPatchOutcome::PtraceOtherFallback);
        let old = Arc::new(StdMutex::new(old));
        let holder = Arc::clone(&old);
        let next = Arc::new(StdMutex::new(old.lock().unwrap().after_exec().unwrap()));
        assert!(!Arc::ptr_eq(&holder, &next));
        let next = next.lock().unwrap();
        assert_eq!(next.phase, LiteinstRuntimePhase::Waiting);
        assert_eq!(next.generation, 42);
        assert!(next.ready_generation.is_none());
        assert!(next.frame.is_none());
        assert!(next.attempted_sites.is_empty());
        assert!(next.fallback_sites.is_empty());
        assert!(next.active_hooks.is_empty());
        let mut old = holder.lock().unwrap();
        assert_eq!(old.phase, LiteinstRuntimePhase::Ready);
        assert_eq!(old.ready_generation, Some(41));
        assert_eq!(old.frame.unwrap().begin_rip, 0x7000_1000);
        assert!(old.attempted_sites.contains(&0x401005));
        assert_eq!(
            old.fallback_sites.get(&0x401005),
            Some(&LiteinstPatchOutcome::PtraceOtherFallback)
        );
        assert_eq!(old.active_hooks.len(), 1);
        old.generation = u64::MAX;
        assert_eq!(old.after_exec().unwrap_err(), Errno::EOVERFLOW);
        assert_eq!(old.generation, u64::MAX);
        assert_eq!(old.phase, LiteinstRuntimePhase::Ready);
    }

    #[test]
    fn kernel_page_ranges_floor_ceil_and_reject_overflow() {
        assert_eq!(
            kernel_page_range(0x401005, 1, 4096),
            Ok(Some(GuestRange {
                start: 0x401000,
                end: 0x402000,
            }))
        );
        assert_eq!(kernel_page_range(0x401000, 0, 4096), Ok(None));
        assert_eq!(kernel_page_range(u64::MAX - 1, 4, 4096), Err(()));
        assert_eq!(kernel_page_range(0x401000, 1, 3000), Err(()));
    }

    #[test]
    fn short_successful_mapping_invalidates_the_whole_attempted_page() {
        let mut state = LiteinstRuntimeState::default();
        state.attempted_sites.extend([0x401005, 0x401fff, 0x402005]);

        state.invalidate_attempted_pages(0x401000, 1, 4096);

        assert_eq!(state.attempted_sites, HashSet::from([0x402005]));
    }

    #[test]
    fn proc_maps_paths_preserve_literal_whitespace_and_decode_octal_escapes() {
        let mapping = parse_guest_map(
            br"00400000-00401000 r-xp 00000000 08:02 123 /tmp/a  double	tab\040space\011escaped\134slash",
        )
        .unwrap();
        assert_eq!(
            mapping.path.unwrap(),
            PathBuf::from("/tmp/a  double\ttab space\tescaped\\slash")
        );
    }

    #[test]
    fn cancellable_returns_a_completed_result() {
        let cancel_handler = Arc::new(AtomicBool::new(false));

        assert_eq!(
            futures::executor::block_on(cancellable(cancel_handler, async { 42 })),
            Some(42)
        );
    }

    #[test]
    fn cancellable_observes_cancellation_in_the_same_poll() {
        let cancel_handler = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancel_handler);
        let pending = future::poll_fn(move |_| {
            signal.store(true, Ordering::SeqCst);
            Poll::<()>::Pending
        });

        assert_eq!(
            futures::executor::block_on(cancellable(Arc::clone(&cancel_handler), pending)),
            None
        );
        assert!(!cancel_handler.load(Ordering::SeqCst));
    }

    #[test]
    fn active_hook_footprint_rejects_destructive_mapping_overlap() {
        let state = active_state();
        assert!(state.mapping_mutates_active_hook(
            Sysno::mprotect,
            SyscallArgs::new(0x401000, 0x1000, libc::PROT_NONE as usize, 0, 0, 0),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::mremap,
            SyscallArgs::new(0x7000_1000, 0x1000, 0x2000, 0, 0, 0),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::munmap,
            SyscallArgs::new(0x7100_0000, 0x1000, 0, 0, 0, 0),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::mmap,
            SyscallArgs::new(
                0x7000_0000,
                0x1000,
                libc::PROT_READ as usize,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as usize,
                usize::MAX,
                0,
            ),
            4096,
        ));
    }

    #[test]
    fn short_mapping_lengths_cover_the_whole_active_page() {
        let state = active_state();
        for nr in [Sysno::mprotect, Sysno::pkey_mprotect, Sysno::munmap] {
            assert!(state.mapping_mutates_active_hook(
                nr,
                SyscallArgs::new(0x401000, 1, libc::PROT_NONE as usize, 0, 0, 0),
                4096,
            ));
        }
        assert!(state.mapping_mutates_active_hook(
            Sysno::mmap,
            SyscallArgs::new(0x401000, 1, 0, libc::MAP_FIXED as usize, 0, 0),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::mremap,
            SyscallArgs::new(0x5000_0000, 1, 1, libc::MREMAP_FIXED as usize, 0x401000, 0,),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::mremap,
            SyscallArgs::new(0x5000_0000, 0, 1, libc::MREMAP_FIXED as usize, 0x401000, 0,),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::mprotect,
            SyscallArgs::new(u64::MAX as usize - 1, 4, libc::PROT_NONE as usize, 0, 0, 0),
            4096,
        ));
    }

    #[test]
    fn pkey_mprotect_is_a_controller_mapping_syscall() {
        assert!(is_liteinst_mapping_syscall(Sysno::pkey_mprotect));
    }

    #[test]
    fn active_hook_noop_protection_retains_provenance() {
        let mut state = active_state();
        assert!(!state.mapping_mutates_active_hook(
            Sysno::mprotect,
            SyscallArgs::new(
                0x401000,
                0x1000,
                (libc::PROT_READ | libc::PROT_EXEC) as usize,
                0,
                0,
                0,
            ),
            4096,
        ));
        state.invalidate_attempted_pages(0x401000, 1, 4096);
        assert_eq!(state.active_hooks.len(), 1);
    }

    #[test]
    fn liteinst_rejects_stack_pointer_updates_without_weakening_shared_frame() {
        let current = libc::user_regs_struct {
            rsp: 0x7fff_1000,
            ..unsafe { core::mem::zeroed() }
        };
        let requested = libc::user_regs_struct {
            rsp: current.rsp + 8,
            ..current
        };
        assert_eq!(
            validate_liteinst_user_regs_update(&current, &requested),
            Err(Errno::ENOTSUPP)
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn liteinst_helper_clears_only_abi_sensitive_transient_flags() {
        let transient = (1 << 8) | (1 << 10) | (1 << 16) | (1 << 18);
        let preserved = (1 << 0) | (1 << 2) | (1 << 6) | (1 << 9) | (1 << 11);
        assert_eq!(
            liteinst_helper_entry_rflags(transient | preserved),
            preserved
        );
    }

    #[test]
    fn native_child_kind_uses_exact_tgid_not_ptrace_opcode() {
        let p = Pid::from_raw;
        assert_eq!(
            classify_native_child(p(40), p(40), p(41), p(40)),
            Ok(ChildTaskKind::Thread)
        );
        assert_eq!(
            classify_native_child(p(40), p(40), p(41), p(41)),
            Ok(ChildTaskKind::Process)
        );
        for (logical, native, child, group) in [
            (40, 39, 41, 41),
            (40, 40, 41, 42),
            (40, 40, 40, 40),
            (0, 0, 41, 41),
            (40, 40, 0, 0),
        ] {
            assert_eq!(
                classify_native_child(p(logical), p(native), p(child), p(group)),
                Err(Errno::ECHILD)
            );
        }
    }

    #[test]
    fn newborn_registration_retries_only_interrupted_capture() {
        let mut results = [Err(Errno::EINTR), Err(Errno::EINTR), Ok(())].into_iter();
        assert_eq!(register_newborn_wait(|| results.next().unwrap()), Ok(()));
        assert!(results.next().is_none());
        for error in [Errno::EMFILE, Errno::EAGAIN, Errno::ESRCH, Errno::ECHILD] {
            let mut calls = 0;
            assert_eq!(
                register_newborn_wait(|| {
                    calls += 1;
                    Err(error)
                }),
                Err(error)
            );
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn newborn_custody_failure_cannot_look_like_terminal_success() {
        assert_eq!(
            format_child_custody_failure(
                Pid::from_raw(40),
                Pid::from_raw(41),
                "register_newborn_wait",
                "errno=24"
            ),
            "HERMIT_CHILD_CUSTODY_FAILED creator=40 child=41 exit=103 phase=register_newborn_wait error=errno=24 cleanup=unconfirmed"
        );
        assert_eq!(
            format_child_custody_failure(
                Pid::from_raw(40),
                Pid::from_raw(41),
                "newborn_terminal_tool_cleanup",
                "first\nsecond\rline"
            )
            .lines()
            .count(),
            1
        );
        assert_ne!(CHILD_CUSTODY_FAILURE_EXIT_CODE, 0);
        assert_ne!(CHILD_CUSTODY_FAILURE_EXIT_CODE, TASK_PANIC_EXIT_CODE);
        assert_ne!(
            CHILD_CUSTODY_FAILURE_EXIT_CODE,
            TASK_TERMINATION_FAILURE_EXIT_CODE
        );
    }

    #[test]
    fn failed_task_termination_refuses_every_non_esrch_error() {
        assert_eq!(check_failed_task_termination(Ok(())), Ok(()));
        assert_eq!(check_failed_task_termination(Err(Errno::ESRCH)), Ok(()));
        for error in [Errno::EPERM, Errno::EBADF, Errno::ENXIO, Errno::EINTR] {
            assert_eq!(check_failed_task_termination(Err(error)), Err(error));
        }
    }

    #[test]
    fn failed_task_termination_marker_preserves_refusal_and_unconfirmed_cleanup() {
        assert_eq!(
            format_task_termination_failure(Pid::from_raw(4242), Errno::EPERM),
            "HERMIT_TASK_TERMINATION_FAILED tid=4242 exit=102 errno=1 backend_failure=acknowledged cleanup=unconfirmed"
        );
        assert_ne!(TASK_TERMINATION_FAILURE_EXIT_CODE, 0);
        assert_ne!(TASK_TERMINATION_FAILURE_EXIT_CODE, TASK_PANIC_EXIT_CODE);
    }

    #[test]
    fn task_panic_marker_has_canonical_shape() {
        // The token and the field order are what a harness greps for; keep
        // them stable.
        let line = format_task_panic_marker(
            Pid::from_raw(4242),
            &"Clock perf counter exceeds target value" as &(dyn std::any::Any + Send),
        );
        assert_eq!(
            line,
            "HERMIT_TASK_PANIC tid=4242 exit=101 \
             message=Clock perf counter exceeds target value"
        );
        assert!(line.starts_with(TASK_PANIC_MARKER));
        assert_eq!(TASK_PANIC_MARKER, "HERMIT_TASK_PANIC");
        assert_eq!(TASK_PANIC_EXIT_CODE, 101);
    }

    #[test]
    fn task_panic_marker_is_always_one_greppable_line() {
        // A `panic!` with a formatted message arrives as `String`, and a
        // multi-line message would otherwise split the marker across lines and
        // make it unmatchable.
        let payload = String::from("first line\nsecond line\r\nthird");
        let line =
            format_task_panic_marker(Pid::from_raw(7), &payload as &(dyn std::any::Any + Send));
        assert_eq!(line.lines().count(), 1);
        assert_eq!(
            line,
            "HERMIT_TASK_PANIC tid=7 exit=101 message=first line second line  third"
        );
    }

    #[test]
    fn task_panic_marker_survives_a_non_string_payload() {
        // `panic_any(42)` carries no string. The marker must still be emitted:
        // an unreadable reason is not a reason to go back to hanging.
        let line =
            format_task_panic_marker(Pid::from_raw(9), &42u32 as &(dyn std::any::Any + Send));
        assert_eq!(
            line,
            "HERMIT_TASK_PANIC tid=9 exit=101 message=<non-string panic payload>"
        );
    }
    // These tests use the real libtest default capture, then end the child
    // process through the real fatal leaf. Regular files avoid pipe-drain waits.
    mod fatal_marker_capture_tests {
        use std::fs::File;
        use std::fs::OpenOptions;
        use std::io::Read;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::process::CommandExt;
        use std::process::Child;
        use std::process::Command;
        use std::process::ExitStatus as ProcessStatus;
        use std::process::Stdio;
        use std::time::Duration;
        use std::time::Instant;

        use super::*;

        const ROLE: &str = "REVERIE_TASK_PANIC_CAPTURE_CHILD";
        const CAPTURED: &str = "REVERIE_CAPTURE_ONLY_MUST_NOT_REACH_REAL_STDERR";
        const LOG_LIMIT: u64 = 65536;

        struct MarkerChild {
            child: Child,
            status: Option<ProcessStatus>,
            deadline: Instant,
            cleanup_deadline: Option<Instant>,
        }
        impl MarkerChild {
            fn observe_until(&mut self, deadline: Instant) -> std::io::Result<ProcessStatus> {
                loop {
                    if let Some(status) = self.child.try_wait()? {
                        self.status = Some(status);
                        if Instant::now() >= deadline {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "marker terminal observation arrived after original bound",
                            ));
                        }
                        return Ok(status);
                    }
                    if Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "marker child exceeded original bound",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            fn finish(&mut self) -> std::io::Result<ProcessStatus> {
                if let Some(status) = self.status {
                    return Ok(status);
                }
                // This original Child has not been reaped or passed to any other
                // waiter. Never discover or signal descendants by numeric PID.
                let cleanup_deadline = *self.cleanup_deadline.get_or_insert_with(|| {
                    self.deadline.min(Instant::now() + Duration::from_secs(2))
                });
                let signal = self.child.kill();
                let result = self.observe_until(cleanup_deadline);
                if let Err(error) = &result {
                    let _ = writeln!(
                        std::io::stderr(),
                        "MARKER_OWNED_CLEANUP_UNCONFIRMED signal={signal:?} wait={error}"
                    );
                }
                result
            }
        }
        impl Drop for MarkerChild {
            fn drop(&mut self) {
                if self.status.is_none() {
                    // Unexpected Rust error/unwind does not obtain a fresh budget.
                    let _ = self.finish();
                }
            }
        }
        fn read_log(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            File::open(path)?
                .take(LOG_LIMIT + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > LOG_LIMIT {
                return Err(std::io::Error::other("marker log exceeded fixed cap"));
            }
            Ok(bytes)
        }
        fn capture_case(mode: &str, selector: &str) -> std::io::Result<()> {
            if let Some(role) = std::env::var_os(ROLE) {
                if role != mode {
                    return Err(std::io::Error::other("unexpected marker child role"));
                }
                // This self-exec child has no tracees or further children.
                // Contain an unexpected abort without relying on RLIMIT_CORE
                // to suppress an external piped core collector.
                if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0
                    || unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } != 0
                {
                    unsafe { libc::_exit(90) };
                }
                eprintln!("{CAPTURED}");
                writeln!(std::io::stderr(), "MARKER_CAPTURE_ENTER mode={mode}")?;
                if mode == "fatal" {
                    guest_task_panic_is_fatal(
                        Pid::from_raw(4242),
                        Box::new(String::from("first\nsecond\r\nthird")),
                    );
                }
                writeln!(std::io::stderr(), "MARKER_CAPTURE_RETURN mode=ordinary")?;
                return Ok(());
            }

            let started = Instant::now();
            let predicate_deadline = started + Duration::from_secs(3);
            let final_deadline = started + Duration::from_secs(5);
            let directory = std::env::temp_dir().join(format!(
                "reverie-marker-capture-{}-{mode}",
                std::process::id(),
            ));
            std::fs::create_dir(&directory)?; // exclusive; never reuse old logs
            let stdout_path = directory.join("stdout.log");
            let stderr_path = directory.join("stderr.log");
            let log = |path: &std::path::Path| {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)
            };
            let mut command = Command::new(std::env::current_exe()?);
            command
                .args([selector, "--exact", "--test-threads=1"])
                .env(ROLE, mode)
                .env_remove("RUST_TEST_NOCAPTURE")
                .stdin(Stdio::null())
                .stdout(log(&stdout_path)?)
                .stderr(log(&stderr_path)?);
            unsafe {
                command.pre_exec(|| {
                    let limit = libc::rlimit {
                        rlim_cur: LOG_LIMIT,
                        rlim_max: LOG_LIMIT,
                    };
                    if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut owned = MarkerChild {
                child: command.spawn()?,
                status: None,
                deadline: final_deadline,
                cleanup_deadline: None,
            };
            let original = owned.observe_until(predicate_deadline);
            // Seal original status/timeout before separate cleanup. All fallible
            // log reads and assertions occur after the actual owned wait.
            let retirement = owned.finish();
            let stdout = read_log(&stdout_path);
            let stderr = read_log(&stderr_path);
            let _ = writeln!(
                std::io::stderr(),
                "MARKER_CAPTURE_OBSERVED mode={mode} original={original:?} original_code={:?} retirement={retirement:?} elapsed={:?} stdout={stdout:?} stderr={stderr:?}",
                original.as_ref().ok().and_then(|status| status.code()),
                started.elapsed()
            );
            retirement?;
            let status = original?; // late cleanup never repairs the predicate
            let stdout = stdout?;
            let stderr = stderr?;
            std::fs::remove_file(&stdout_path)?;
            std::fs::remove_file(&stderr_path)?;
            std::fs::remove_dir(&directory)?;
            assert!(started.elapsed() < Duration::from_secs(5));
            assert_eq!(status.code(), Some(if mode == "fatal" { 101 } else { 0 }));
            assert!(
                !String::from_utf8_lossy(&stderr).contains(CAPTURED),
                "this discriminator must use real normal libtest capture"
            );
            let expected = if mode == "fatal" {
                b"MARKER_CAPTURE_ENTER mode=fatal\nHERMIT_TASK_PANIC tid=4242 exit=101 message=first second  third\n".as_slice()
            } else {
                b"MARKER_CAPTURE_ENTER mode=ordinary\nMARKER_CAPTURE_RETURN mode=ordinary\n"
                    .as_slice()
            };
            assert_eq!(stderr, expected, "real stderr marker missing or altered");
            assert!(String::from_utf8_lossy(&stdout).contains("running 1 test"));
            if mode == "ordinary" {
                assert!(String::from_utf8_lossy(&stdout).contains("1 passed; 0 failed"));
            } else {
                let output = String::from_utf8_lossy(&stdout);
                for returned_marker in ["... ok", "... FAILED", "test result:", "failures:"] {
                    assert!(
                        !output.contains(returned_marker),
                        "fatal leaf must not return a libtest result: {output}"
                    );
                }
            }
            Ok(())
        }
        #[test]
        fn captured_marker_survives_fatal_exit() -> std::io::Result<()> {
            capture_case(
                "fatal",
                "task::tests::fatal_marker_capture_tests::captured_marker_survives_fatal_exit",
            )
        }
        #[test]
        fn ordinary_return_uses_real_capture_without_fatal_marker() -> std::io::Result<()> {
            capture_case(
                "ordinary",
                "task::tests::fatal_marker_capture_tests::ordinary_return_uses_real_capture_without_fatal_marker",
            )
        }
    }
}

#[cfg(test)]
mod child_publication_cancellation_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_keeps_actual_child_until_existing_list_accepts_it() {
        // Preserve the original fork/thread/vfork cases and add the actual
        // non-SIGCHLD process form of PTRACE_EVENT_CLONE.
        for (_op, kind) in [
            (ChildOp::Fork, ChildTaskKind::Process),
            (ChildOp::Clone, ChildTaskKind::Thread),
            (ChildOp::Vfork, ChildTaskKind::Process),
            (ChildOp::Clone, ChildTaskKind::Process),
        ] {
            let threads = Arc::new(Mutex::new(Children::new()));
            let processes = Arc::new(Mutex::new(Children::new()));
            let selected = if kind == ChildTaskKind::Thread {
                &threads
            } else {
                &processes
            };
            let guard = selected.lock().await;
            let mut pending = Some(PendingChild::Spawned(
                kind,
                Child {
                    id: Pid::from_raw(61),
                    suspended: Arc::new(AtomicBool::new(false)),
                    wait_all_stop_tx: None,
                    daemonizer_rx: None,
                    handle: ChildCompletion::Legacy(tokio::spawn(async {
                        Some(ExitStatus::Exited(0))
                    })),
                    ordinary_group: None,
                    terminal: None,
                },
            ));
            {
                let transfer = publish_child_to_existing_list(&mut pending, &threads, &processes);
                futures::pin_mut!(transfer);
                assert!(futures::poll!(&mut transfer).is_pending());
                // Drop simulates cancellation of the syscall/dispatch future.
            }
            let Some(PendingChild::Spawned(_, child)) = pending.as_ref() else {
                panic!("cancellation lost the spawned child");
            };
            assert_eq!(child.id(), Pid::from_raw(61));
            drop(guard);
            publish_child_to_existing_list(&mut pending, &threads, &processes).await;
            assert!(pending.is_none());
            let mut actual = selected.lock().await.take_inner();
            assert_eq!(actual.len(), 1);
            assert_eq!(
                actual.pop().unwrap().await.unwrap(),
                Some(ExitStatus::Exited(0))
            );
            let other = if kind == ChildTaskKind::Thread {
                &processes
            } else {
                &threads
            };
            assert_eq!(other.lock().await.take_inner().len(), 0);
        }
    }
}

#[cfg(test)]
mod preconstruction_tests;

#[cfg(test)]
mod newborn_startup_tests;

#[cfg(test)]
mod parent_completion_tests;
