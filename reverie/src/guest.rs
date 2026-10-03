/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Guest (i.e. thread) structure and traits

use async_trait::async_trait;
use reverie_syscalls::Errno;
use reverie_syscalls::MemoryAccess;
use reverie_syscalls::SyscallInfo;

use crate::Never;
use crate::Pid;
use crate::Signal;
use crate::SignalEvent;
use crate::auxv::Auxv;
use crate::backtrace::Backtrace;
use crate::error::Error;
use crate::stack::Stack;
use crate::timer::TimerSchedule;
use crate::tool::GlobalRPC;
use crate::tool::GlobalTool;
use crate::tool::Tool;

/// The native scalar Read user-address range check only. This is not a
/// mapping, writable-memory, copied-payload, or network-source capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginalReadRangeVerdict {
    /// The native ABI accepts the original pointer/count range.
    Allowed,
    /// The native ABI rejects the original pointer/count with EFAULT.
    Fault,
}

/// The logical kind of a guest memory region reported by
/// [`Guest::detlog_memory_regions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetlogRegionKind {
    /// The current thread's user stack.
    Stack,
    /// The program-break heap.
    Heap,
}

/// A guest-address-space memory region a backend can expose for deterministic
/// memory-map logging (`--detlog-stack` / `--detlog-heap`).
///
/// The `[start, end)` bounds are guest virtual addresses readable through
/// [`Guest::memory`]. This exists for out-of-process backends (for example the
/// KVM backend) where [`Guest::pid`] is the host VMM process rather than a
/// process whose `/proc/<pid>/maps` describes the guest's own address space, so
/// the default `/proc`-based enumeration would read the wrong process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetlogMemoryRegion {
    /// Which logical region this is.
    pub kind: DetlogRegionKind,
    /// Inclusive start guest virtual address.
    pub start: u64,
    /// Exclusive end guest virtual address.
    pub end: u64,
}

/// A backend-owned interruption before an exact injected syscall entered Linux.
/// This value is not an errno or a completed syscall. The backend must retain
/// its matching stopped task until [`Guest::finish_interrupted_syscall`].
#[derive(Clone, Debug)]
pub struct InterruptedSyscall(std::sync::Arc<Option<Signal>>);

impl InterruptedSyscall {
    /// Allocates a fresh backend ticket. Constructing one supplies no authority:
    /// completion must match the ticket retained by that backend's held task.
    pub fn new() -> Self {
        Self(std::sync::Arc::new(None))
    }

    /// Allocates a ticket naming the actual signal stop retained by a backend.
    /// The cause does not replace the identity check against that stopped task.
    pub fn with_signal(signal: Signal) -> Self {
        Self(std::sync::Arc::new(Some(signal)))
    }

    /// The actual stopped signal, when supplied by the retaining backend.
    pub fn signal(&self) -> Option<Signal> {
        *self.0
    }

    /// Tests identity without accepting an equal-looking or cloned owner ID.
    pub fn same(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}
impl std::fmt::Display for InterruptedSyscall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("original syscall interrupted before kernel entry")
    }
}
impl std::error::Error for InterruptedSyscall {}
impl Default for InterruptedSyscall {
    fn default() -> Self {
        Self::new()
    }
}

/// The outcome of the shared scalar Read delegate boundary.
/// A completed value alone is not a native receipt: Tools still require their
/// existing exact Prepared/Returned observations and provider custody.
#[derive(Debug)]
pub enum InjectedReadResult {
    /// The invocation supplied its ordinary result, including Linux errors.
    Complete(Result<i64, Errno>),
    /// No kernel entry occurred; no result or recordable errno exists.
    Interrupted(InterruptedSyscall),
    /// Replay consumed an explicit interruption control, not a Read result.
    /// Native injection paths must reject this variant.
    RecordedInterruption(InterruptedSyscall),
}

/// Identity of an offered private-helper interruption, not physical authority
/// by itself. A Tool MUST first claim it through the current Guest before
/// changing any operation state. Only the backend retaining this exact ticket
/// and its original task/call/signal stop can accept that one-use claim.
#[derive(Clone, Debug)]
pub struct PrivateInterruption(std::sync::Arc<PrivateInterruptionFacts>);
#[derive(Debug)]
struct PrivateInterruptionFacts {
    call: (reverie_syscalls::Sysno, reverie_syscalls::SyscallArgs),
    helper: (reverie_syscalls::Sysno, reverie_syscalls::SyscallArgs),
    signal: Signal,
    recorded: bool,
}
impl PrivateInterruption {
    /// Allocate a backend ticket. This public backend construction interface
    /// grants no authority: a fabricated or stale ticket fails Guest's claim.
    pub fn new_backend(
        call: (reverie_syscalls::Sysno, reverie_syscalls::SyscallArgs),
        helper: (reverie_syscalls::Sysno, reverie_syscalls::SyscallArgs),
        signal: Signal,
    ) -> Self {
        Self(std::sync::Arc::new(PrivateInterruptionFacts {
            call,
            helper,
            signal,
            recorded: false,
        }))
    }
    /// Allocate the distinct replay-wait offer. As with new_backend, only an
    /// exact claim against the currently retaining backend supplies authority.
    pub fn new_recorded_backend(
        call: (reverie_syscalls::Sysno, reverie_syscalls::SyscallArgs),
        helper: (reverie_syscalls::Sysno, reverie_syscalls::SyscallArgs),
        signal: Signal,
    ) -> Self {
        Self(std::sync::Arc::new(PrivateInterruptionFacts {
            call,
            helper,
            signal,
            recorded: true,
        }))
    }
    /// Logical call whose original Tool future remains suspended.
    pub fn logical_call(&self) -> (reverie_syscalls::Sysno, reverie_syscalls::SyscallArgs) {
        self.0.call
    }
    /// Private helper at the retained pre-ENTRY delivery stop.
    pub fn helper(&self) -> (reverie_syscalls::Sysno, reverie_syscalls::SyscallArgs) {
        self.0.helper
    }
    /// Original signal. Its complete siginfo remains with the held backend stop.
    pub fn signal(&self) -> Signal {
        self.0.signal
    }
    /// True only for the backend's explicit recorded-control wait. This is not
    /// a native helper attempt or completion; the actual signal stop is real.
    pub fn recorded(&self) -> bool {
        self.0.recorded
    }
    /// Identity only, never equality of caller-supplied numbers.
    pub fn same(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

/// One offered logical Read register handback. This is neither a private-helper
/// EXIT nor an original native Read completion. The original signal is still
/// held and no guest handler instruction has run. Claim through the current
/// Guest before changing Tool state or running the common tail.
#[derive(Clone, Debug)]
pub struct PrivateReadCompletion(std::sync::Arc<PrivateReadCompletionFacts>);
#[derive(Debug)]
struct PrivateReadCompletionFacts {
    interruption: PrivateInterruption,
    completed: Option<i64>,
}
impl PrivateReadCompletion {
    /// Backend construction alone grants no authority. A fabricated, stale or
    /// wrong-phase ticket cannot satisfy Guest's exact one-use offer claim.
    pub fn new_backend(interruption: PrivateInterruption, completed: Option<i64>) -> Self {
        Self(std::sync::Arc::new(PrivateReadCompletionFacts {
            interruption,
            completed,
        }))
    }
    /// The exact already-claimed original private interruption identity.
    pub fn interruption(&self) -> &PrivateInterruption {
        &self.0.interruption
    }
    /// Original logical Read, not the interrupted private helper.
    pub fn logical_call(&self) -> (reverie_syscalls::Sysno, reverie_syscalls::SyscallArgs) {
        self.0.interruption.logical_call()
    }
    /// Some(n) is positive committed progress. None is an INTERRUPTED,
    /// RESTART-PENDING boundary with real logical -ERESTARTSYS registers, not a
    /// completed syscall result. Linux has not yet applied the signal action.
    pub fn completed(&self) -> Option<i64> {
        self.0.completed
    }
    /// Identity only; caller-supplied numeric equality is never authority.
    pub fn same(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

/// An explicit Tool semantic contract, not evidence of a physical helper EXIT.
/// Unknown or not-yet-published Tool metadata must select `Unsupported`.
#[derive(Debug)]
pub enum PrivateInterruptionAction {
    /// Retain the existing fatal containment behavior; do not invent a result.
    Unsupported,
    /// Complete this ONE pending helper and the SAME callback before any signal
    /// handler instruction. The callback's return must equal the actual helper
    /// result; no additional injection is allowed. The Tool certifies its entire
    /// remaining tail (including post-hook) is finite, signal-state insensitive,
    /// and needs no handler/guest progress or lock held by that progress. It must
    /// also permit the ordinary signal hook to run now without same-task locks.
    /// A helper's name alone is not this contract. General callbacks are not
    /// opted in, and no suspended future is kept through a guest handler.
    DrainHelperResult,
    /// The Tool has settled the exact logical Read/request, but MUST NOT yet run
    /// its register-observing common tail. The backend first installs and reads
    /// back real logical registers on the SAME held stop, then offers the
    /// one-use handle_private_read_completion hook for that tail exactly once.
    /// `Some(n)` is its already committed positive byte count;
    /// `None` is its explicit modeled no-progress Read interruption. Neither is
    /// the unentered private helper's return. The backend drops that callback
    /// without redispatch and gives the original signal to Linux with the logical
    /// Read context, so Linux applies EINTR/SA_RESTART. This cannot acknowledge an
    /// outstanding provider/native owner or substitute for its real cancellation.
    FinishRead {
        /// Positive committed count, or no progress. Zero/negative is refused.
        completed: Option<i64>,
    },
}

/// A representation of a guest task (thread).
#[async_trait]
pub trait Guest<T: Tool>: Send + GlobalRPC<T::GlobalState> {
    /// Access to guest memory
    type Memory: MemoryAccess + Send;

    /// Access to guest stack
    type Stack: Send + Stack;

    /// Thread ID of the guest task.
    fn tid(&self) -> Pid;

    /// Process ID of the process containing the guest task.
    fn pid(&self) -> Pid;

    /// Process ID of the parent process. Returns `None` if this is the root of
    /// the traced process tree. A return value of `None` does not necessarily
    /// mean it is the root process in the system.
    fn ppid(&self) -> Option<Pid>;

    /// Returns true if this thread is the thread group leader (i.e., the main
    /// thread).
    fn is_main_thread(&self) -> bool {
        self.tid() == self.pid()
    }

    /// Returns true if this is considered the root process of the traced task
    /// tree (i.e., if `getppid()` returns `None`).
    fn is_root_process(&self) -> bool {
        self.ppid().is_none()
    }

    /// Returns true if this is considered the root thread of the traced task
    /// tree (i.e., if `getppid()` returns `None` and `is_main_thread` returns
    /// true).
    fn is_root_thread(&self) -> bool {
        self.is_root_process() && self.is_main_thread()
    }

    /// Whether this task is still executing the launcher for a spawned Command.
    ///
    /// This is logging provenance, not a guest identity or execution-mode test.
    /// A backend may return true only for its Command-launch root before the
    /// first successful exec replaces the inherited launcher address space.
    /// Function tests, attached tasks, descendants and post-exec guest tasks
    /// must return false. Callers must additionally establish the type of any
    /// value before formatting a launch-image pointer as a host address.
    fn is_command_bootstrap(&self) -> bool {
        false
    }

    /// Reads and returns the auxv table for this process.
    fn auxv(&self) -> Auxv {
        Auxv::new(self.pid()).expect("failed to read auxv table")
    }

    /// Returns a representation of the address space associated with this guest
    /// thread.
    fn memory(&self) -> Self::Memory;

    /// Borrow the actual run-global Tool instance when this backend owns it in
    /// the same process. A remote RPC proxy or a process-local copy of another
    /// coordinator's state cannot supply this reference. Unsupported backends
    /// return `None`; callers must not reconstruct the instance from config,
    /// serialized thread state, or numeric identities.
    ///
    /// This association grants no stopped-task, MM, scheduler, native-worker,
    /// or memory-access authority. A Tool must still validate those properties
    /// against this Guest's current thread state and actual memory interface.
    /// The reference borrows this Guest; no new owner or serialized capability
    /// is created. In particular, a Tool may retain immutable borrows during
    /// preparation, then use one actual Memory value for a synchronous checked
    /// operation before resuming or mutably operating on the Guest.
    fn local_global_state(&self) -> Option<&T::GlobalState> {
        None
    }

    /// Inspect the exact retained original native scalar Read without consuming,
    /// rewriting, resuming, or injecting it. A successful verdict checks only
    /// the kernel's address-range rule, never whether guest memory is mapped.
    /// Unsupported backends refuse rather than guessing a user-address limit.
    fn inspect_original_read_range(
        &self,
        _read: reverie_syscalls::Read,
    ) -> Result<OriginalReadRangeVerdict, Error> {
        Err(Error::Tool(anyhow::anyhow!(
            "backend has no authenticated original native Read range check"
        )))
    }

    /// Inspect the exact retained original native stream `recvfrom` with
    /// flags=0 and no source-address outputs. The verdict has the same narrow
    /// meaning as [`Guest::inspect_original_read_range`].
    fn inspect_original_recvfrom_range(
        &self,
        _receive: reverie_syscalls::Recvfrom,
    ) -> Result<OriginalReadRangeVerdict, Error> {
        Err(Error::Tool(anyhow::anyhow!(
            "backend has no authenticated original native recvfrom range check"
        )))
    }

    /// Read a source through backend-owned stopped-task/MM acquisition and
    /// actual asynchronous worker retirement. The caller must independently
    /// retain and revalidate its original scheduling/MM/FD/prefix authority.
    /// `retention` is opaque resource custody only, kept by the backend registry
    /// before submission through TRUE join, even when this future is dropped.
    /// The existing outer run timeout/cancellation continues to govern the run;
    /// this operation creates no per-Send host deadline.
    async fn read_native_source(
        &mut self,
        _address: usize,
        _length: usize,
        _retention: Box<dyn Send + Sync>,
    ) -> Result<Vec<u8>, crate::syscalls::NativeUserReadError> {
        Err(crate::syscalls::NativeUserReadError::Refused(
            crate::syscalls::NativeUserReadRefusal::UnsupportedBackend,
        ))
    }

    /// Stage a bounded, permission-checked source under physical exclusion of
    /// every authenticated followed task, through the original worker's join.
    /// Busy/incomplete native histories refuse; this never interrupts tasks.
    ///
    /// This primitive is INACTIVE in Detcore. Bytes are not a network source
    /// certificate: independent external-writer enforcement and the original
    /// scheduler/MM/FD/entry consumer remain prerequisites for publication.
    /// Opaque retention is resource custody, never caller-asserted authority.
    async fn stage_followed_source(
        &mut self,
        _address: usize,
        _length: usize,
        _retention: Box<dyn Send + Sync>,
    ) -> Result<Vec<u8>, crate::syscalls::NativeUserReadError> {
        Err(crate::syscalls::NativeUserReadError::Refused(
            crate::syscalls::NativeUserReadRefusal::UnsupportedBackend,
        ))
    }

    /// Borrow a backend-issued one-use scalar receive writer while every
    /// followed task remains physically held. Only the exact retained original
    /// native Read or flags=0/addressless Recvfrom is supported. No guest code,
    /// injection or asynchronous continuation occurs inside the callback.
    /// Helpers that consume the original entry currently cause refusal.
    ///
    /// The callback must retain actual/possible effects on its existing Call
    /// before returning. A positive raw count alone is not completion: inspect
    /// the postcheck too. This capability remains inactive in Detcore and does
    /// not supply independent MM/FD/prefix/external-writer authority.
    fn with_followed_store<R>(
        &self,
        _original: reverie_syscalls::Syscall,
        _action: impl FnOnce(&mut dyn crate::syscalls::FollowedStore) -> R,
    ) -> Result<R, crate::syscalls::NativeUserStoreRefusal> {
        Err(crate::syscalls::NativeUserStoreRefusal::Evidence(
            crate::syscalls::NativeUserReadRefusal::UnsupportedBackend,
        ))
    }

    /// Run one explicitly marked receive observation timer (0 < timeout <= 1ms)
    /// while retaining the same original scalar receive callback. This is host
    /// observation latency, never a guest timeout or a virtual-time increment.
    /// The backend must authenticate actual native entry/exit and restoration.
    /// It does not wait for a peer's scheduler continuation or grant a turn.
    async fn inject_receive_observation_timer(
        &mut self,
        _original: reverie_syscalls::Syscall,
        _timeout: std::time::Duration,
    ) -> Result<(), crate::Error> {
        Err(crate::Error::Tool(anyhow::anyhow!(
            "backend has no retained receive timer"
        )))
    }

    /// Join only the exact already-running, backend-marked peer observation
    /// timers while this original callback stays stopped. This grants no
    /// scheduler turn, source/store authority, or guest timeout. A fresh held
    /// acquisition and the caller's current policy evidence remain required.
    async fn join_followed_observation_timers(
        &mut self,
        _original: reverie_syscalls::Syscall,
    ) -> Result<(), crate::Error> {
        Err(crate::Error::Tool(anyhow::anyhow!(
            "backend has no peer timer join"
        )))
    }

    /// Borrow a held writer after the dedicated receive timer restored this
    /// original callback. Ordinary inject does not qualify this path. The same
    /// per-original one-use claim and complete cohort custody remain required;
    /// the caller must retain the actual raw/postcheck outcome before return.
    fn with_restored_followed_store<R>(
        &self,
        _original: reverie_syscalls::Syscall,
        _action: impl FnOnce(&mut dyn crate::syscalls::FollowedStore) -> R,
    ) -> Result<R, crate::syscalls::NativeUserStoreRefusal> {
        Err(crate::syscalls::NativeUserStoreRefusal::Evidence(
            crate::syscalls::NativeUserReadRefusal::UnsupportedBackend,
        ))
    }

    /// Capture a separate original one-row Poll under complete followed-task
    /// custody and true source-worker join. Initial revents is output-only.
    /// Returned input values are observations, not readiness or FD authority.
    async fn capture_original_followed_poll(
        &mut self,
        _original: reverie_syscalls::Syscall,
        _retention: Box<dyn Send + Sync>,
    ) -> Result<crate::syscalls::OriginalPollInput, crate::syscalls::NativeUserReadError> {
        Err(crate::syscalls::NativeUserReadError::Refused(
            crate::syscalls::NativeUserReadRefusal::UnsupportedBackend,
        ))
    }

    /// Run a separately marked Poll-origin observation timer. Raw zero is only
    /// physical completion; original deadline and full PollState stay external.
    async fn inject_poll_observation_timer(
        &mut self,
        _original: reverie_syscalls::Syscall,
        _timeout: std::time::Duration,
    ) -> Result<(), crate::Error> {
        Err(crate::Error::Tool(anyhow::anyhow!(
            "backend has no retained Poll timer"
        )))
    }

    /// Borrow the current original or positively restored Poll writer. The
    /// exact two-byte effect, including zero, must be retained before returning.
    fn with_followed_poll_store<R>(
        &self,
        _original: reverie_syscalls::Syscall,
        _action: impl FnOnce(&mut dyn crate::syscalls::FollowedPollStore) -> R,
    ) -> Result<R, crate::syscalls::NativeUserStoreRefusal> {
        Err(crate::syscalls::NativeUserStoreRefusal::Evidence(
            crate::syscalls::NativeUserReadRefusal::UnsupportedBackend,
        ))
    }

    /// Returns a mutable reference to thread state.
    fn thread_state_mut(&mut self) -> &mut T::ThreadState;

    /// Returns an immutable reference to thread state.
    fn thread_state(&self) -> &T::ThreadState;

    /// Returns the current register values of the guest thread.
    async fn regs(&mut self) -> libc::user_regs_struct;

    /// Overwrites the register values of the guest thread. This is the write
    /// counterpart to [`Guest::regs`].
    ///
    /// This is a generic, determinism-agnostic mechanism: it lets a tool control
    /// the guest's register file at a stop, and the tool decides what values to
    /// write. For example, a determinism tool can use it to canonicalize
    /// registers that the syscall instruction clobbers (`%rcx`/`%r11` on
    /// x86-64) so that even a misbehaving guest observes deterministic state.
    ///
    /// Preconditions: the guest is in a stopped state and Reverie is currently
    /// running a handler on that guest thread's behalf.
    ///
    /// The default implementation returns [`Errno::ENOSYS`] for backends that
    /// cannot write guest registers.
    async fn set_regs(&mut self, regs: libc::user_regs_struct) -> Result<(), Error> {
        let _ = regs;
        Err(Errno::ENOSYS.into())
    }

    /// Returns the current stack pointer with this guest thread.
    async fn stack(&mut self) -> Self::Stack;

    /// Task is trying to become a daemon. The tracer may choose to kill all
    /// remaining tasks when daemons are the only ones left.
    async fn daemonize(&mut self);

    /// Inject a system call into the guest and wait for the return value. This
    /// function dirties the register file while its executing, but restores at
    /// the end.
    ///
    /// Preconditions: the guest is in a stopped state and Reverie is currently
    /// running a handler on that guest thread's behalf.
    ///
    /// Postconditions: the register file is the same as before the call to this
    /// function. However, any side effects, including to guest memory, persist
    /// after the injected call.
    ///
    /// # Caveats
    ///
    /// A few syscalls are special and behave differently from the rest:
    ///  - `exit` or `exit_group` will never return when injected. Since these
    ///    syscalls will cause the current thread or process to exit, no code that
    ///    comes after can be executed.
    ///  - `execve` will never return when *successfully* injected. If you wish to
    ///    handle successful calls to `execve`, use [`Tool::handle_post_exec`].
    ///    Failed calls to `execve` will still return, however. Thus, it is safe to
    ///    use [`Result::unwrap_err`] on the result of the `inject`.
    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno>;

    /// Consume the one allowed claim of the interruption currently offered by
    /// this backend. Call before changing Tool/model/request state. This only
    /// authenticates a held pre-ENTRY signal, never a native helper EXIT or a
    /// completed Read. Default and foreign/stale/duplicate claims refuse.
    fn claim_private_interruption(&mut self, _ticket: &PrivateInterruption) -> Result<(), Error> {
        Err(Error::Tool(anyhow::anyhow!(
            "backend has no matching private interruption"
        )))
    }

    /// Authenticate the current logical-register handback, once. This is only
    /// available inside Tool::handle_private_read_completion after the backend
    /// has installed/read back the actual held task registers. It does not
    /// authenticate a native EXIT or permit another injection/reentry.
    fn claim_private_read_completion(
        &mut self,
        _ticket: &PrivateReadCompletion,
    ) -> Result<(), Error> {
        Err(Error::Tool(anyhow::anyhow!(
            "backend has no matching private Read completion"
        )))
    }

    /// Wait for a real signal at the recorded emulated-Read helper checkpoint
    /// without executing the helper or a native Read. The backend calls the
    /// same private-interruption hook with a one-use recorded ticket. Successful
    /// FinishRead retires the original callback out-of-band; this cannot return
    /// a fabricated helper result. The record itself supplies no stop authority.
    async fn await_recorded_private_interruption(
        &mut self,
        _helper: reverie_syscalls::Syscall,
        _signal: Signal,
    ) -> Result<Never, Error> {
        Err(Error::Tool(anyhow::anyhow!(
            "backend has no recorded private interruption wait"
        )))
    }

    /// Executes one scalar Read without representing a pre-entry signal as an
    /// errno. The compatibility default uses `inject`; it cannot mint an
    /// interruption ticket or substitute for native observation authority.
    async fn inject_original_read(&mut self, syscall: crate::syscalls::Read) -> InjectedReadResult {
        InjectedReadResult::Complete(self.inject(syscall).await)
    }

    /// Execute only the unchanged original Sendto while every followed peer
    /// retains its actual stopped control. Unsupported backends must refuse;
    /// neither generic injection nor a private helper supplies this custody.
    async fn inject_original_sendto_with_stopped_peers(
        &mut self,
        _call: crate::syscalls::Sendto,
    ) -> Result<i64, Error> {
        Err(Error::Tool(anyhow::anyhow!(
            "backend has no peer-held original Sendto"
        )))
    }

    /// Execute the epoll_ctl copy shape at its retained original syscall entry.
    /// Only epfd and fd become full-width -1; op, event pointer, and all other
    /// operands retain their original values. This does not register interest
    /// and does not bypass seccomp: filters may reject the changed operands.
    /// The provider must authenticate post-filter bytes separately. A returned
    /// EBADF is not a copy receipt, and DEL has no event copy. Missing original
    /// context is a Tool/backend error, never a fresh private helper fallback.
    async fn inject_epoll_ctl_copy(
        &mut self,
        _call: crate::syscalls::EpollCtl,
    ) -> Result<i64, Error> {
        Err(Error::Tool(anyhow::anyhow!(
            "backend has no original epoll copy entry"
        )))
    }

    /// Waits at the same attempt boundary for the real signal named by a
    /// recorded pre-entry Read interruption. No Read may execute or supply a
    /// native result. The returned ticket retains the actual signal stop until
    /// `finish_interrupted_syscall`; the record alone grants no signal custody.
    async fn await_recorded_read_interruption(
        &mut self,
        _call: crate::syscalls::Read,
        _signal: Signal,
    ) -> Result<InterruptedSyscall, Error> {
        Err(Error::Tool(anyhow::anyhow!(
            "backend has no recorded Read signal boundary"
        )))
    }

    /// Ends a positively canceled, unentered invocation after the Tool has
    /// released its exact operation custody. `completed` is a prior partial
    /// count from the same logical syscall, never a result for the canceled
    /// attempt. This prepares the handback; the Tool must finish its ordinary
    /// post-hook and return that count or the same ticket as `Error::Tool`.
    /// Only the backend's exact retained callback consumes the ticket and
    /// dispatches the original signal, with no synthetic syscall errno.
    async fn finish_interrupted_syscall(
        &mut self,
        _ticket: InterruptedSyscall,
        _completed: Option<i64>,
    ) -> Result<(), Error> {
        Err(Error::Tool(anyhow::anyhow!(
            "backend has no interrupted syscall handback"
        )))
    }

    /// Similar to [`Guest::inject`], except that it never returns. Since it does
    /// not return to the caller, the syscall return value cannot be altered or
    /// inspected. This method exists as an optimization for the `ptrace`
    /// backend, so that we can avoid interrupting the guest if we don't care
    /// about the syscall return value.
    ///
    /// # Caveats
    ///
    /// This method comes with a major footgun. Any code written after
    /// `tail_inject` will never be executed:
    ///
    /// ```no_run
    /// use reverie::syscalls::*;
    /// use reverie::*;
    ///
    /// #[derive(Debug, Default, Clone)]
    /// struct MyTool;
    ///
    /// #[reverie::tool]
    /// impl Tool for MyTool {
    ///     /// Global state is unused
    ///     type GlobalState = ();
    ///     /// Count of successful syscalls.
    ///     type ThreadState = u64;
    ///
    ///     async fn handle_syscall_event<T: Guest<Self>>(
    ///         &self,
    ///         guest: &mut T,
    ///         syscall: Syscall,
    ///     ) -> Result<i64, Error> {
    ///         let ret = match syscall {
    ///             Syscall::Open(syscall) => guest.tail_inject(syscall).await,
    ///             _ => guest.inject(syscall).await?,
    ///         };
    ///
    ///         // This is never called if we got the `open` syscall above!!
    ///         *guest.thread_state_mut() += 1;
    ///
    ///         Ok(ret)
    ///     }
    /// }
    /// ```
    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never;

    /// Terminates the current guest thread with status zero after the Tool has
    /// determined that this thread must never resume guest execution.
    ///
    /// This abandons the current callback and runs the backend's consuming
    /// thread-exit cleanup exactly once. It accepts no syscall and does not
    /// authorize other nonreturning injections from restricted callbacks.
    /// It does not request termination of other live threads. Backends whose
    /// ordinary exit injection already provides this contract use that path.
    /// An already-established backend exit retains its status.
    ///
    /// Backend process-lifetime limits still apply. Explicit KVM leader cancellation
    /// cancels live siblings, while normal raw leader `SYS_exit` leaves them running.
    /// Nonleader cancellation also leaves live siblings running.
    async fn cancel_current_thread(&mut self) -> Never {
        self.tail_inject(reverie_syscalls::Exit::default()).await
    }

    /// Retires only the current guest thread after the Tool has determined
    /// that it must never resume, without requesting cancellation of peers.
    ///
    /// This abandons the callback and runs consuming thread cleanup once. It
    /// defaults to a raw thread exit with status zero; an established backend
    /// exit keeps its status. A KVM leader retains and joins live workers, then
    /// adopts the final process status, including a peer's later group exit.
    /// Unlike explicit cancellation, retirement does not discard their work.
    /// Restricted signal callbacks do not gain arbitrary syscall injection.
    async fn retire_current_thread(&mut self) -> Never {
        self.tail_inject(reverie_syscalls::Exit::default()).await
    }

    /// Defers one already-selected signal for delivery by the backend at its
    /// next safe return-to-userspace boundary.
    ///
    /// Backends may return `ENOSYS` when the current callback has no resumable
    /// userspace register context (for example, a lifecycle callback), when
    /// signal provenance is unsupported, or when deterministic recipient
    /// selection is not available. Callers must handle that refusal rather
    /// than assuming the event was queued.
    ///
    /// This is additive to the historical host-signal path. Backends that do
    /// not own a virtual guest signal frame retain the default explicit
    /// `ENOSYS`; adding this method does not change ptrace signal delivery.
    async fn defer_signal_delivery(&mut self, _event: SignalEvent) -> Result<(), Error> {
        Err(Errno::ENOSYS.into())
    }

    /// Queues a Tool-selected terminal child event for the current process.
    ///
    /// The caller supplies a complete process-directed `SIGCHLD` event with
    /// `CLD_EXITED`, `CLD_KILLED`, or `CLD_DUMPED`, and owns its child-status
    /// provenance and deterministic ordering. `CLD_EXITED` carries an unsigned
    /// exit byte; `CLD_KILLED` carries a terminal-default Linux signal number;
    /// `CLD_DUMPED` carries a core-default signal number. The backend validates
    /// the receiver and that class-specific status domain, preserves
    /// process-wide pending ownership and first-siginfo coalescing, and reports
    /// whether queue publication preceded any failure. Wait status and child reaping remain
    /// independent. This operation never recursively invokes a Tool hook or
    /// resumes guest instructions; normal receiver boundaries own delivery.
    ///
    /// Backends may refuse unsupported contexts or process lifetimes. In
    /// particular, KVM initially supports only a live single-thread parent at
    /// a transported return-to-user boundary. A KVM run that installs
    /// [`crate::BackendSignalControlMode::ToolControlled`] must instead use the
    /// generation-bound run-scoped
    /// [`crate::ProcessSignalControl::publish_child_exit`] operation; this
    /// generation-free compatibility surface is then refused before mutation.
    /// The historical private deferral operation and its refusal policy are
    /// otherwise unchanged.
    async fn queue_child_exit_signal(
        &mut self,
        _event: SignalEvent,
    ) -> crate::ChildExitSignalOutcome {
        crate::ChildExitSignalOutcome::RejectedBeforeCommit {
            kind: crate::ChildExitSignalErrorKind::Unsupported,
            errno: Errno::ENOSYS,
        }
    }

    /// Publishes a Tool-selected process alarm at this stopped task's boundary.
    ///
    /// The caller owns deterministic ordering and supplies the complete normal
    /// Linux SIGALRM/SI_KERNEL siginfo (zero except for signo and code). KVM
    /// supports only the current sole live receiver, with no pending process
    /// action and a resumable transported boundary that has not completed an
    /// injected process action. The operation preserves
    /// shared pending ownership and first siginfo, including when blocked or
    /// ignored. Installing SIG_IGN later invalidates older pending generations.
    ///
    /// No Tool hook, guest instruction, timer operation, or wait completion is
    /// performed. The receipt is only pending-state publication. The historical
    /// private [`Guest::defer_signal_delivery`] operation remains independent.
    async fn queue_process_alarm_signal(
        &mut self,
        _event: SignalEvent,
    ) -> crate::ProcessAlarmSignalOutcome {
        crate::ProcessAlarmSignalOutcome::RejectedBeforeCommit {
            kind: crate::ProcessAlarmSignalErrorKind::Unsupported,
            errno: Errno::ENOSYS,
        }
    }

    /// Backend process/task lifetime identity, including at thread start.
    fn signal_task_identity(&self) -> Option<crate::SignalTaskIdentity> {
        None
    }

    /// Current parked-observation capability, bound to this exact callback.
    fn parked_signal_site(&self) -> Option<crate::CallbackSignalSite> {
        None
    }

    /// Authenticates a zero-effect attempt of this exact original scalar read.
    ///
    /// A site is returned only after an actual injection of the identical raw
    /// syscall and arguments returned EAGAIN/EWOULDBLOCK, while its original
    /// callback remains live. A later injection invalidates that attempt. A
    /// positive/partial result, EOF, another errno or another syscall is never
    /// eligible. This query does not execute or restart the read, consume a
    /// signal or grant scheduler ownership. Unsupported backends return None.
    fn polled_read_signal_site(
        &self,
        _call: crate::syscalls::Read,
    ) -> Option<crate::CallbackSignalSite> {
        None
    }

    /// Authenticates the current original scalar write to backend-captured output.
    ///
    /// This read-only query returns the full callback identity only when `call`
    /// is the exact unconsumed original syscall (including all raw arguments)
    /// and its current descriptor aliases an enabled captured stdout/stderr
    /// stream. It does not execute the write, publish or consume a signal,
    /// validate the buffer, or promise a successful byte count.
    ///
    /// The caller must query again with the identical call immediately before
    /// publication and require the same identity, without an intervening guest
    /// operation or injection. KVM additionally requires its existing sole-live-
    /// leader boundary, no prior injected execution, and no active observation
    /// or checked-out stack. Ordinary files, pipes, sockets and uncaptured host
    /// streams are not admitted by this query. The query does not change signal
    /// publication admission; callers must act on `None` themselves. Unsupported
    /// backends return `None`.
    fn captured_write_signal_site(
        &self,
        _call: crate::syscalls::Write,
    ) -> Option<crate::CallbackSignalSite> {
        None
    }

    /// Active nested observation, available to the Tool's real signal-hook RPCs.
    fn signal_observation_lease(&self) -> Option<crate::ParkedObservationLease> {
        None
    }

    /// Sequentially observes real pending events without abandoning the original syscall.
    async fn observe_parked_signal(
        &mut self,
        _site: crate::CallbackSignalSite,
        _lease: crate::ParkedObservationLease,
    ) -> Result<crate::ParkedSignalObservation, crate::SignalObservationFailure> {
        Err(crate::SignalObservationFailure::RejectedBeforeRemoval {
            errno: Errno::ENOSYS,
        })
    }

    /// Transfers a reserved fatal selection to the driver; success never returns.
    async fn terminate_from_parked_signal(
        &mut self,
        _selection: crate::PreparedSignalToken,
    ) -> Result<Never, crate::SignalObservationFailure> {
        Err(crate::SignalObservationFailure::RejectedBeforeRemoval {
            errno: Errno::ENOSYS,
        })
    }

    /// Retained irreversible effects, independently of the current observation lease.
    fn parked_signal_failure_context(&self) -> Option<crate::ParkedSignalFailureContext> {
        None
    }

    /// Cancels through the driver without tail-injecting Exit or rolling back effects.
    async fn cancel_parked_signal(
        &mut self,
        _context: crate::ParkedSignalFailureContext,
    ) -> Result<Never, crate::SignalObservationFailure> {
        Err(crate::SignalObservationFailure::RejectedBeforeRemoval {
            errno: Errno::ENOSYS,
        })
    }

    /// Like [`Guest::inject`], but will retry the syscall if `EINTR` or
    /// `ERESTARTSYS` are returned.
    ///
    /// This is useful if we need to inject a syscall other than the one
    /// currently being handled in `handle_syscall_event`. If we don't retry
    /// interrupted syscalls, we could end up running the real syscall more than
    /// once.
    async fn inject_with_retry<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno> {
        loop {
            match self.inject(syscall).await {
                Ok(x) => return Ok(x),
                Err(Errno::EINTR) | Err(Errno::ERESTARTSYS) => continue,
                Err(other) => return Err(other),
            }
        }
    }

    /// Converts this `Guest<T>` such that it implements `Guest<U>`. This is
    /// useful when forwarding callbacks to a "child" tool.
    #[allow(clippy::wrong_self_convention)]
    fn into_guest(&mut self) -> IntoGuest<'_, Self, T> {
        IntoGuest::new(self)
    }

    /// Request that a single timer event occur in the future according to
    /// `sched`.
    ///
    /// There is only a single timer, so repeatedly setting a timer event delays
    /// the delivery of the single timer event that will eventually fire.
    ///
    /// Timer events are cancelled by the delivery of other event types. If
    /// receiving timer events is critical, your tool must override all event
    /// listeners and reschedule your timer within them.
    ///
    /// This requests a non-deterministic timer event, which will occur after _at
    /// least_ `sched` has elapsed, but no guarantees are made for delivery. As a
    /// result, the event will likely have much less overhead than one set with
    /// [`Guest::set_timer_precise`].
    fn set_timer(&mut self, sched: TimerSchedule) -> Result<(), Error>;

    /// Request that a single timer event occur in the future according to
    /// `sched`.
    ///
    /// Functions identically to [`Guest::set_timer`], except that the resulting
    /// event will be delivered _exactly_ when `sched` has elapsed. This results
    /// in a far higher overhead to deliver an event.
    fn set_timer_precise(&mut self, sched: TimerSchedule) -> Result<(), Error>;

    /// Read a thread-local monotonic clock which is never reset. The starting
    /// value, resolution, and semantics of the ticks are
    /// implementation-specific.
    fn read_clock(&mut self) -> Result<u64, Error>;

    /// Returns a stack trace starting at the current location of the guest
    /// thread. If a backtrace is not available, returns `None`.
    ///
    /// # Example
    ///
    /// ```
    /// use reverie::syscalls::*;
    /// use reverie::*;
    ///
    /// #[derive(Debug, Default, Clone)]
    /// struct MyTool;
    ///
    /// #[reverie::tool]
    /// impl Tool for MyTool {
    ///     type GlobalState = ();
    ///     type ThreadState = ();
    ///
    ///     async fn handle_syscall_event<T: Guest<Self>>(
    ///         &self,
    ///         guest: &mut T,
    ///         syscall: Syscall,
    ///     ) -> Result<i64, Error> {
    ///         // Generate a backtrace whenever we receive a call to getpid().
    ///         if let Syscall::Getpid(_) = &syscall {
    ///             if let Some(frames) = guest.backtrace() {
    ///                 println!("Backtrace for getpid():");
    ///                 for frame in frames {
    ///                     println!("  {}", frame);
    ///                 }
    ///             }
    ///         }
    ///
    ///         Ok(guest.inject(syscall).await?)
    ///     }
    /// }
    /// ```
    fn backtrace(&mut self) -> Option<Backtrace> {
        None
    }

    /// Returns true if all of the following conditions are true:
    ///  1. [`Tool::subscriptions`] returns an interest in intercepting CPUID.
    ///  2. We're able to trap and intercept the CPUID instruction. We may not
    ///     be able to do this for virtual machines as this functionality is
    ///     often disabled for VMs.
    ///  3. We're running on x86-64. Other architectures don't have the CPUID
    ///     instruction.
    fn has_cpuid_interception(&self) -> bool {
        false
    }

    /// Returns the guest-address memory regions this backend wants hashed for
    /// deterministic memory-map logging, or `None` to fall back to reading
    /// `/proc/<pid>/maps` for the process returned by [`Guest::pid`].
    ///
    /// The default is `None`, which preserves the historical behavior used by
    /// the ptrace backend, where `pid()` is the guest process and its
    /// `/proc/<pid>/maps` correctly describes the guest address space.
    ///
    /// Out-of-process backends whose `pid()` is not the guest (for example the
    /// KVM backend, where it is the host VMM process) override this to return
    /// real guest stack/heap ranges readable through [`Guest::memory`], so the
    /// determinism engine hashes the guest's memory instead of the VMM's.
    fn detlog_memory_regions(&self) -> Option<Vec<DetlogMemoryRegion>> {
        None
    }
}

/// Wraps a `Guest<T>` such that it implements `Guest<U>`.
///
/// # Limitations
///
/// `T` and `U` must have the same global state. This limitation may be removed
/// in the future.
pub struct IntoGuest<'a, G: ?Sized, U> {
    inner: &'a mut G,
    _phantom: core::marker::PhantomData<U>,
}

impl<'a, G: ?Sized, U> IntoGuest<'a, G, U> {
    /// Creates a new `IntoGuest`.
    pub fn new(guest: &'a mut G) -> Self {
        Self {
            inner: guest,
            _phantom: core::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<'a, G, U> GlobalRPC<U::GlobalState> for IntoGuest<'a, G, U>
where
    G: Guest<U> + ?Sized,
    U: Tool,
{
    async fn send_rpc(
        &self,
        message: <U::GlobalState as GlobalTool>::Request,
    ) -> <U::GlobalState as GlobalTool>::Response {
        self.inner.send_rpc(message).await
    }

    fn config(&self) -> &<U::GlobalState as GlobalTool>::Config {
        self.inner.config()
    }
}

#[async_trait]
impl<'a, G, U, L> Guest<L> for IntoGuest<'a, G, U>
where
    G: Guest<U> + ?Sized,
    L: Tool<GlobalState = U::GlobalState>,
    U: Tool + AsMut<L>,
    U::ThreadState: AsRef<L::ThreadState> + AsMut<L::ThreadState>,
{
    type Memory = G::Memory;
    type Stack = G::Stack;

    fn tid(&self) -> Pid {
        self.inner.tid()
    }

    fn pid(&self) -> Pid {
        self.inner.pid()
    }

    fn ppid(&self) -> Option<Pid> {
        self.inner.ppid()
    }

    fn is_command_bootstrap(&self) -> bool {
        self.inner.is_command_bootstrap()
    }

    fn is_main_thread(&self) -> bool {
        self.inner.is_main_thread()
    }

    fn is_root_process(&self) -> bool {
        self.inner.is_root_process()
    }

    fn is_root_thread(&self) -> bool {
        self.inner.is_root_thread()
    }

    fn memory(&self) -> Self::Memory {
        self.inner.memory()
    }

    fn local_global_state(&self) -> Option<&L::GlobalState> {
        self.inner.local_global_state()
    }

    fn inspect_original_read_range(
        &self,
        read: reverie_syscalls::Read,
    ) -> Result<OriginalReadRangeVerdict, Error> {
        self.inner.inspect_original_read_range(read)
    }

    fn inspect_original_recvfrom_range(
        &self,
        receive: reverie_syscalls::Recvfrom,
    ) -> Result<OriginalReadRangeVerdict, Error> {
        self.inner.inspect_original_recvfrom_range(receive)
    }

    async fn read_native_source(
        &mut self,
        address: usize,
        length: usize,
        retention: Box<dyn Send + Sync>,
    ) -> Result<Vec<u8>, crate::syscalls::NativeUserReadError> {
        self.inner
            .read_native_source(address, length, retention)
            .await
    }

    async fn stage_followed_source(
        &mut self,
        address: usize,
        length: usize,
        retention: Box<dyn Send + Sync>,
    ) -> Result<Vec<u8>, crate::syscalls::NativeUserReadError> {
        self.inner
            .stage_followed_source(address, length, retention)
            .await
    }

    fn with_followed_store<R>(
        &self,
        original: reverie_syscalls::Syscall,
        action: impl FnOnce(&mut dyn crate::syscalls::FollowedStore) -> R,
    ) -> Result<R, crate::syscalls::NativeUserStoreRefusal> {
        self.inner.with_followed_store(original, action)
    }

    async fn inject_receive_observation_timer(
        &mut self,
        original: reverie_syscalls::Syscall,
        timeout: std::time::Duration,
    ) -> Result<(), crate::Error> {
        self.inner
            .inject_receive_observation_timer(original, timeout)
            .await
    }

    async fn join_followed_observation_timers(
        &mut self,
        original: reverie_syscalls::Syscall,
    ) -> Result<(), crate::Error> {
        self.inner.join_followed_observation_timers(original).await
    }

    fn with_restored_followed_store<R>(
        &self,
        original: reverie_syscalls::Syscall,
        action: impl FnOnce(&mut dyn crate::syscalls::FollowedStore) -> R,
    ) -> Result<R, crate::syscalls::NativeUserStoreRefusal> {
        self.inner.with_restored_followed_store(original, action)
    }

    async fn capture_original_followed_poll(
        &mut self,
        original: reverie_syscalls::Syscall,
        retention: Box<dyn Send + Sync>,
    ) -> Result<crate::syscalls::OriginalPollInput, crate::syscalls::NativeUserReadError> {
        self.inner
            .capture_original_followed_poll(original, retention)
            .await
    }
    async fn inject_poll_observation_timer(
        &mut self,
        original: reverie_syscalls::Syscall,
        timeout: std::time::Duration,
    ) -> Result<(), crate::Error> {
        self.inner
            .inject_poll_observation_timer(original, timeout)
            .await
    }
    fn with_followed_poll_store<R>(
        &self,
        original: reverie_syscalls::Syscall,
        action: impl FnOnce(&mut dyn crate::syscalls::FollowedPollStore) -> R,
    ) -> Result<R, crate::syscalls::NativeUserStoreRefusal> {
        self.inner.with_followed_poll_store(original, action)
    }

    fn thread_state_mut(&mut self) -> &mut L::ThreadState {
        self.inner.thread_state_mut().as_mut()
    }

    fn thread_state(&self) -> &L::ThreadState {
        self.inner.thread_state().as_ref()
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        self.inner.regs().await
    }

    async fn set_regs(&mut self, regs: libc::user_regs_struct) -> Result<(), Error> {
        self.inner.set_regs(regs).await
    }

    async fn stack(&mut self) -> Self::Stack {
        self.inner.stack().await
    }

    async fn daemonize(&mut self) {
        self.inner.daemonize().await
    }

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno> {
        self.inner.inject(syscall).await
    }

    fn claim_private_interruption(&mut self, ticket: &PrivateInterruption) -> Result<(), Error> {
        self.inner.claim_private_interruption(ticket)
    }

    fn claim_private_read_completion(
        &mut self,
        ticket: &PrivateReadCompletion,
    ) -> Result<(), Error> {
        self.inner.claim_private_read_completion(ticket)
    }

    async fn await_recorded_private_interruption(
        &mut self,
        helper: reverie_syscalls::Syscall,
        signal: Signal,
    ) -> Result<Never, Error> {
        self.inner
            .await_recorded_private_interruption(helper, signal)
            .await
    }

    async fn inject_original_read(&mut self, syscall: crate::syscalls::Read) -> InjectedReadResult {
        self.inner.inject_original_read(syscall).await
    }

    async fn inject_original_sendto_with_stopped_peers(
        &mut self,
        call: crate::syscalls::Sendto,
    ) -> Result<i64, Error> {
        self.inner
            .inject_original_sendto_with_stopped_peers(call)
            .await
    }

    async fn inject_epoll_ctl_copy(
        &mut self,
        call: crate::syscalls::EpollCtl,
    ) -> Result<i64, Error> {
        self.inner.inject_epoll_ctl_copy(call).await
    }

    async fn await_recorded_read_interruption(
        &mut self,
        call: crate::syscalls::Read,
        signal: Signal,
    ) -> Result<InterruptedSyscall, Error> {
        self.inner
            .await_recorded_read_interruption(call, signal)
            .await
    }

    async fn finish_interrupted_syscall(
        &mut self,
        ticket: InterruptedSyscall,
        completed: Option<i64>,
    ) -> Result<(), Error> {
        self.inner
            .finish_interrupted_syscall(ticket, completed)
            .await
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        #![allow(unreachable_code)]
        self.inner.tail_inject(syscall).await
    }

    async fn cancel_current_thread(&mut self) -> Never {
        self.inner.cancel_current_thread().await
    }

    async fn retire_current_thread(&mut self) -> Never {
        self.inner.retire_current_thread().await
    }

    async fn defer_signal_delivery(&mut self, event: SignalEvent) -> Result<(), Error> {
        self.inner.defer_signal_delivery(event).await
    }

    async fn queue_child_exit_signal(
        &mut self,
        event: SignalEvent,
    ) -> crate::ChildExitSignalOutcome {
        self.inner.queue_child_exit_signal(event).await
    }

    async fn queue_process_alarm_signal(
        &mut self,
        event: SignalEvent,
    ) -> crate::ProcessAlarmSignalOutcome {
        self.inner.queue_process_alarm_signal(event).await
    }

    fn signal_task_identity(&self) -> Option<crate::SignalTaskIdentity> {
        self.inner.signal_task_identity()
    }
    fn parked_signal_site(&self) -> Option<crate::CallbackSignalSite> {
        self.inner.parked_signal_site()
    }
    fn polled_read_signal_site(
        &self,
        call: crate::syscalls::Read,
    ) -> Option<crate::CallbackSignalSite> {
        self.inner.polled_read_signal_site(call)
    }
    fn captured_write_signal_site(
        &self,
        call: crate::syscalls::Write,
    ) -> Option<crate::CallbackSignalSite> {
        self.inner.captured_write_signal_site(call)
    }
    fn signal_observation_lease(&self) -> Option<crate::ParkedObservationLease> {
        self.inner.signal_observation_lease()
    }
    async fn observe_parked_signal(
        &mut self,
        site: crate::CallbackSignalSite,
        lease: crate::ParkedObservationLease,
    ) -> Result<crate::ParkedSignalObservation, crate::SignalObservationFailure> {
        self.inner.observe_parked_signal(site, lease).await
    }
    async fn terminate_from_parked_signal(
        &mut self,
        selection: crate::PreparedSignalToken,
    ) -> Result<Never, crate::SignalObservationFailure> {
        self.inner.terminate_from_parked_signal(selection).await
    }
    fn parked_signal_failure_context(&self) -> Option<crate::ParkedSignalFailureContext> {
        self.inner.parked_signal_failure_context()
    }
    async fn cancel_parked_signal(
        &mut self,
        context: crate::ParkedSignalFailureContext,
    ) -> Result<Never, crate::SignalObservationFailure> {
        self.inner.cancel_parked_signal(context).await
    }

    fn set_timer(&mut self, sched: TimerSchedule) -> Result<(), Error> {
        self.inner.set_timer(sched)
    }

    fn set_timer_precise(&mut self, sched: TimerSchedule) -> Result<(), Error> {
        self.inner.set_timer_precise(sched)
    }

    fn read_clock(&mut self) -> Result<u64, Error> {
        self.inner.read_clock()
    }

    fn backtrace(&mut self) -> Option<Backtrace> {
        self.inner.backtrace()
    }

    fn has_cpuid_interception(&self) -> bool {
        self.inner.has_cpuid_interception()
    }

    fn detlog_memory_regions(&self) -> Option<Vec<DetlogMemoryRegion>> {
        self.inner.detlog_memory_regions()
    }
}

#[cfg(test)]
mod private_interruption_identity_tests {
    use reverie_syscalls::SyscallArgs;
    use reverie_syscalls::Sysno;

    use super::*;

    fn call() -> (Sysno, SyscallArgs) {
        (
            Sysno::read,
            SyscallArgs {
                arg0: 7,
                arg1: 0x1234_5000,
                arg2: 9,
                arg3: 13,
                arg4: 17,
                arg5: 19,
            },
        )
    }

    fn helper() -> (Sysno, SyscallArgs) {
        (Sysno::getpid, SyscallArgs::new(0, 0, 0, 0, 0, 0))
    }

    #[test]
    fn interruption_clone_preserves_identity_and_all_facts() {
        let original = PrivateInterruption::new_backend(call(), helper(), Signal::SIGUSR1);
        let clone = original.clone();
        assert!(original.same(&clone));
        assert!(clone.same(&original));
        for ticket in [&original, &clone] {
            assert_eq!(ticket.logical_call(), call());
            assert_eq!(ticket.helper(), helper());
            assert_eq!(ticket.signal(), Signal::SIGUSR1);
            assert!(!ticket.recorded());
        }
    }

    #[test]
    fn equal_facts_do_not_supply_interruption_identity() {
        let original = PrivateInterruption::new_backend(call(), helper(), Signal::SIGUSR1);
        let other = PrivateInterruption::new_backend(call(), helper(), Signal::SIGUSR1);
        assert_eq!(original.logical_call(), other.logical_call());
        assert_eq!(original.helper(), other.helper());
        assert_eq!(original.signal(), other.signal());
        assert_eq!(original.recorded(), other.recorded());
        assert!(!original.same(&other));
        assert!(!other.same(&original));

        let recorded = PrivateInterruption::new_recorded_backend(call(), helper(), Signal::SIGUSR1);
        let other_recorded =
            PrivateInterruption::new_recorded_backend(call(), helper(), Signal::SIGUSR1);
        assert!(recorded.recorded());
        assert!(other_recorded.recorded());
        assert!(recorded.same(&recorded.clone()));
        assert!(!recorded.same(&other_recorded));
        assert!(!recorded.same(&original));
        assert_eq!(recorded.logical_call(), original.logical_call());
        assert_eq!(recorded.helper(), original.helper());
        assert_eq!(recorded.signal(), original.signal());
    }

    #[test]
    fn completion_clone_preserves_original_interruption_and_exact_count() {
        let interruption = PrivateInterruption::new_backend(call(), helper(), Signal::SIGUSR1);
        for completed in [Some(3), None] {
            let original = PrivateReadCompletion::new_backend(interruption.clone(), completed);
            let clone = original.clone();
            assert!(original.same(&clone));
            assert!(clone.same(&original));
            for ticket in [&original, &clone] {
                assert!(ticket.interruption().same(&interruption));
                assert_eq!(ticket.logical_call(), call());
                assert_eq!(ticket.completed(), completed);
            }
            let new_offer = PrivateReadCompletion::new_backend(interruption.clone(), completed);
            assert!(new_offer.interruption().same(&interruption));
            assert_eq!(new_offer.logical_call(), original.logical_call());
            assert_eq!(new_offer.completed(), original.completed());
            assert!(!new_offer.same(&original));
        }
    }

    #[test]
    fn completion_does_not_substitute_equal_facts_or_another_result() {
        let interruption = PrivateInterruption::new_backend(call(), helper(), Signal::SIGUSR1);
        let foreign = PrivateInterruption::new_backend(call(), helper(), Signal::SIGUSR1);
        let original = PrivateReadCompletion::new_backend(interruption.clone(), Some(3));
        let foreign_completion = PrivateReadCompletion::new_backend(foreign.clone(), Some(3));
        let interrupted = PrivateReadCompletion::new_backend(interruption.clone(), None);

        assert_eq!(original.logical_call(), foreign_completion.logical_call());
        assert_eq!(original.completed(), foreign_completion.completed());
        assert!(!original.same(&foreign_completion));
        assert!(!foreign_completion.interruption().same(&interruption));
        assert!(foreign_completion.interruption().same(&foreign));
        assert!(interrupted.interruption().same(&interruption));
        assert_eq!(interrupted.logical_call(), original.logical_call());
        assert_eq!(interrupted.completed(), None);
        assert_eq!(original.completed(), Some(3));
        assert!(!original.same(&interrupted));
    }
}
