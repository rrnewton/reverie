//! Generic in-guest host for Reverie tools.

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::Arc;

use liteinst2::trampoline::HookContext;
use reverie::Error;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Never;
use reverie::Pid;
use reverie::Rdtsc;
use reverie::Stack;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::Errno;
use reverie::syscalls::LocalMemory;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_preload::signal::native_frame::InstructionResult;
use reverie_preload::tool_host::DrivenSyscall;
use reverie_preload::tool_host::TailResult;
use reverie_preload::tool_host::drive_ready;
use reverie_preload::tool_host::drive_tool_syscall;
use reverie_preload::trap::raw_syscall6;

use crate::rpc::CoordinatorRpc;
use crate::rpc::SpinMutex;
use crate::runtime;
use crate::runtime::SyscallEvent;

const STACK_CAPACITY: usize = 4096;

mod scratch;
use scratch::ScratchOwner;
mod ownership;

#[cfg(test)]
mod resource_tests;
use ownership::Invocation;
use ownership::Registry;
#[cfg(test)]
mod ownership_tests;

struct DispatchScratchScope {
    owner: ScratchOwner,
    _allocation_scope: crate::patch_alloc::DispatchAllocationScope,
    _runtime: crate::runtime_domain::Entry,
}

impl DispatchScratchScope {
    fn enter() -> Self {
        let runtime = crate::runtime_domain::Entry::enter();
        let allocation_scope = crate::patch_alloc::enter_dispatch();
        #[cfg(test)]
        crate::runtime_domain::tests::at(crate::runtime_domain::tests::SCRATCH_ENTER);
        Self {
            owner: ScratchOwner::new(),
            _allocation_scope: allocation_scope,
            _runtime: runtime,
        }
    }
}

impl Drop for DispatchScratchScope {
    fn drop(&mut self) {
        #[cfg(test)]
        crate::runtime_domain::tests::at(crate::runtime_domain::tests::SCRATCH_DROP);
        self.owner.close();
    }
}

/// Test-only record of what the shared Tool actually did during a dispatch.
///
/// Default builds contain none of this module. It observes; it never decides.
#[cfg(feature = "test-tool-host-dispatch")]
#[doc(hidden)]
pub mod dispatch_observer {
    use super::SpinMutex;

    /// One observed effect, in the order it occurred.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Event {
        /// A syscall the Tool injected through the guarded raw route, with the
        /// raw kernel return value.
        Inject { number: i64, result: i64 },
        /// A guest scratch arena committed by `Stack::commit`.
        StackCommit { bytes: usize },
        /// Arenas still committed when the dispatch scope released them.
        ScratchRelease { arenas: usize },
    }

    static EVENTS: SpinMutex<Vec<Event>> = SpinMutex::new(Vec::new());

    pub(super) fn record(event: Event) {
        EVENTS.lock().push(event);
    }

    /// Drain everything observed so far.
    pub fn take() -> Vec<Event> {
        core::mem::take(&mut *EVENTS.lock())
    }
}

trait ToolHandler: Send + Sync {
    fn guest_progress(
        &self,
        owner: i64,
        context: &mut HookContext,
        clock: u64,
    ) -> Result<(), Error>;
    fn rng_snapshot(&self, owner: i64) -> Result<reverie::vdso::VdsoRngSnapshot, Error>;
    #[cfg(feature = "private-crt")]
    fn prepare_initial(&self) -> io::Result<()>;
    #[cfg(feature = "private-crt")]
    fn dispatch_initial(&self, context: &mut HookContext);
    fn dispatch(&self, event: &mut SyscallEvent);
    fn observes_syscall(&self, number: i64) -> bool;
    fn dispatch_owned(
        &self,
        kind: Option<runtime::InstructionEventKind>,
        context: &mut HookContext,
    ) -> Option<InstructionResult>;
}

static HANDLER: std::sync::OnceLock<Box<dyn ToolHandler>> = std::sync::OnceLock::new();

#[derive(Clone, Copy)]
enum InstructionAdmission {
    Public,
    #[cfg(feature = "test-owned-cpuid")]
    OwnedCpuidFixture,
    #[cfg(feature = "test-owned-cpuid")]
    OwnedInstructionFixture,
    #[cfg(feature = "test-owned-cpuid")]
    OwnedClockedInstructionFixture,
    #[cfg(feature = "test-owned-cpuid")]
    OwnedSingleStepFixture,
    #[cfg(feature = "test-owned-cpuid")]
    OwnedPreciseTimerFixture,
    #[cfg(feature = "test-owned-cpuid")]
    OwnedSyscallTimerFixture,
    #[cfg(feature = "test-owned-cpuid")]
    OwnedCompiledStepFixture {
        evidence_bytes: usize,
    },
}

impl InstructionAdmission {
    #[cfg(feature = "test-owned-cpuid")]
    fn native_evidence(self) -> Option<usize> {
        if let Self::OwnedCompiledStepFixture { evidence_bytes } = self {
            return Some(evidence_bytes);
        }
        None
    }
    fn results_only(self) -> bool {
        !matches!(self, Self::Public)
    }

    #[cfg(feature = "test-owned-cpuid")]
    fn counting_only(self) -> bool {
        if matches!(
            self,
            Self::OwnedClockedInstructionFixture
                | Self::OwnedSingleStepFixture
                | Self::OwnedPreciseTimerFixture
                | Self::OwnedSyscallTimerFixture
                | Self::OwnedCompiledStepFixture { .. }
        ) {
            return true;
        }
        false
    }

    #[cfg(feature = "test-owned-cpuid")]
    fn single_step(self) -> bool {
        if matches!(
            self,
            Self::OwnedSingleStepFixture
                | Self::OwnedPreciseTimerFixture
                | Self::OwnedSyscallTimerFixture
                | Self::OwnedCompiledStepFixture { .. }
        ) {
            return true;
        }
        false
    }

    #[cfg(feature = "test-owned-cpuid")]
    fn precise_timer(self) -> bool {
        if matches!(
            self,
            Self::OwnedPreciseTimerFixture
                | Self::OwnedSyscallTimerFixture
                | Self::OwnedCompiledStepFixture { .. }
        ) {
            return true;
        }
        false
    }

    fn owned_syscalls(self) -> bool {
        #[cfg(feature = "test-owned-cpuid")]
        if matches!(
            self,
            Self::OwnedSyscallTimerFixture | Self::OwnedCompiledStepFixture { .. }
        ) {
            return true;
        }
        false
    }
}

/// The ordered owned-execution setup shared by every route that owns guest
/// continuation: the owned-trace signal policy, the owned context and its
/// syscall capture, the shared RCB clock and the precise controller.
///
/// This is deliberately **not** a capability selector. Each route names exactly
/// one fixed value of it — the private fixtures derive theirs from the existing
/// [`InstructionAdmission`] they already carry, and the public route uses the
/// single constant [`OwnedSetup::public_native`]. Nothing chooses between owned
/// capabilities at runtime, and no caller supplies these fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OwnedSetup {
    clocked: bool,
    single_step: bool,
    precise_timer: bool,
    syscalls: bool,
    native_evidence: Option<usize>,
}

impl OwnedSetup {
    /// The one public owned-native execution setup. Every field is fixed: a
    /// published shared RCB clock, the owned-trace single-step policy, the
    /// precise owned controller, owned guest-origin syscall capture, and native
    /// evidence. `evidence_bytes` is only the native history budget;
    /// `owned_context::initialize` requires all four capabilities whenever
    /// native evidence is present, so they cannot be selected apart.
    fn public_native(evidence_bytes: usize) -> Self {
        Self {
            clocked: true,
            single_step: true,
            precise_timer: true,
            syscalls: true,
            native_evidence: Some(evidence_bytes),
        }
    }

    /// Whether this is the complete public owned-native shape.
    ///
    /// This restates the conjunction `owned_context::initialize` itself
    /// requires whenever native evidence is present, so it recognizes the one
    /// backed shape rather than a caller-chosen capability subset. Every public
    /// exception is keyed on this predicate; an incomplete shape is refused
    /// exactly as it was before.
    fn is_public_native(self) -> bool {
        self.clocked
            && self.single_step
            && self.precise_timer
            && self.syscalls
            && self.native_evidence.is_some()
    }
}

impl InstructionAdmission {
    /// The owned setup a private instruction admission already implies. This
    /// reproduces the exact predicate values the install path read directly
    /// before this was factored out, so every existing fixture route keeps its
    /// current ordered setup unchanged.
    #[cfg(feature = "test-owned-cpuid")]
    fn owned_setup(self) -> Option<OwnedSetup> {
        self.results_only().then(|| OwnedSetup {
            clocked: self.counting_only(),
            single_step: self.single_step(),
            precise_timer: self.precise_timer(),
            syscalls: self.owned_syscalls(),
            native_evidence: self.native_evidence(),
        })
    }
}

/// Whether the guest may not write registers back through `Guest::set_regs`.
///
/// Every owned route refuses those writes: the owned context validates and
/// resumes an authentic native frame, and an arbitrary register write would
/// invalidate the step plan it already decoded.
fn instruction_results_only(admission: InstructionAdmission, owned: Option<OwnedSetup>) -> bool {
    admission.results_only() || owned.is_some()
}

// TODO-HUMAN-REVIEW(PR-127): Review generic in-guest Tool hosting.
/// Install a concrete Reverie tool in this guest and connect it to its coordinator.
///
/// The caller is normally a tool-specific preload DSO. It must invoke this
/// before application threads start and before any seccomp filter is active.
///
/// # Safety
///
/// Installs process-global signal, seccomp, allocator, and instrumentation state.
// TODO-HUMAN-REVIEW(PR-133): Review fail-closed preinstalled signal-handler boundary.
pub unsafe fn install_tool<T>(coordinator: impl AsRef<Path>) -> io::Result<()>
where
    T: Tool + 'static,
{
    unsafe {
        install_tool_inner::<T>(
            coordinator.as_ref(),
            true,
            runtime::PatchPublication::Concurrent,
            crate::SyscallMode::SeccompWithPatching,
            InstructionAdmission::Public,
            None,
        )
    }
}

/// Install a concrete Reverie tool with quiescent patch publication.
///
/// This has the same process-global effects as [`install_tool`], but skips the
/// concurrent instruction-tearing and straddler protocol when publishing a new
/// site. The caller must keep every other application thread from fetching
/// guest text for the full lifetime of the installed tool.
///
/// # Safety
///
/// In addition to [`install_tool`]'s requirements, the caller asserts that no
/// other application thread can execute while a syscall site is installed.
pub unsafe fn install_tool_quiescent<T>(coordinator: impl AsRef<Path>) -> io::Result<()>
where
    T: Tool + 'static,
{
    unsafe {
        install_tool_inner::<T>(
            coordinator.as_ref(),
            true,
            runtime::PatchPublication::Quiescent,
            crate::SyscallMode::SeccompWithPatching,
            InstructionAdmission::Public,
            None,
        )
    }
}

// TODO-HUMAN-REVIEW(PR-139): Review the environment-preserving bootstrap install API.
/// Installs a concrete tool using a consumed bootstrap coordinator path.
///
/// Unlike the legacy install entry point, this does not remove its coordinator
/// environment variable because the bootstrap path did not introduce one.
///
/// # Safety
///
/// Installs process-global signal, seccomp, allocator, and instrumentation state.
pub unsafe fn install_tool_from_bootstrap<T>(coordinator: impl AsRef<Path>) -> io::Result<()>
where
    T: Tool + 'static,
{
    unsafe {
        install_tool_inner::<T>(
            coordinator.as_ref(),
            false,
            runtime::PatchPublication::Concurrent,
            crate::SyscallMode::SeccompWithPatching,
            InstructionAdmission::Public,
            None,
        )
    }
}

/// Installs a concrete tool from a consumed bootstrap path with an explicit mode.
///
/// Unlike [`install_tool_with_mode`], this preserves the coordinator environment
/// variable because the bootstrap path did not introduce it.
///
/// # Safety
///
/// The caller must satisfy all process-global ownership, signal, reentry,
/// failure, and process-lifetime obligations documented by
/// [`install_tool_with_mode`].
pub unsafe fn install_tool_from_bootstrap_with_mode<T>(
    coordinator: impl AsRef<Path>,
    mode: crate::SyscallMode,
) -> io::Result<()>
where
    T: Tool + 'static,
{
    unsafe {
        install_tool_inner::<T>(
            coordinator.as_ref(),
            false,
            runtime::PatchPublication::Concurrent,
            mode,
            InstructionAdmission::Public,
            None,
        )
    }
}

/// Install a typed Tool with an explicit syscall interception mode.
///
/// SUD-only handles native x86-64 syscalls after installation, not loader
/// startup, vDSO fast paths, instruction events, exec or additional threads.
/// Instruction/vDSO subscriptions are rejected, not silently made native.
/// Selected shared-clock execution requires explicitly registered runtime
/// signals through [`reverie_preload::signal::configure_runtime_signals`],
/// including its unsafe source-validation and process-lifetime requirements.
/// Runtime Rust allocation isolation is retained; asynchronous libc/TLS/Tool
/// reentry is not qualified.
/// Both timer setters return EOPNOTSUPP in SUD-only mode; no timer is armed.
///
/// # Safety
/// The caller owns the installing thread and process signal dispositions for
/// the remaining process lifetime. No other application thread may run, and no
/// application signal handler, nonlocal signal exit or asynchronous callback
/// may enter during Tool execution or a deferred continuation. All preinstalled
/// handlers must be default/ignored; installation rejects custom handlers.
/// The caller must not replace runtime handlers, change masks/altstacks, invoke
/// the trusted gate as a guest bypass, or register unrelated clock hooks.
/// Quiescence during installation alone does not meet these lifetime duties.
/// On failure runtime/RPC resources may remain; do not resume as an instrumented
/// guest. There is no general signal, lifecycle or deterministic-clock guarantee.
pub unsafe fn install_tool_with_mode<T>(
    coordinator: impl AsRef<Path>,
    mode: crate::SyscallMode,
) -> io::Result<()>
where
    T: Tool + 'static,
{
    unsafe {
        install_tool_inner::<T>(
            coordinator.as_ref(),
            true,
            runtime::PatchPublication::Concurrent,
            mode,
            InstructionAdmission::Public,
            None,
        )
    }
}

/// Install a typed Tool for public owned native-step execution, under
/// `UserDispatchWithoutPatching` and the assembly-owned shared RCB clock.
///
/// This is one fixed operation, not a configurable capability. It always
/// establishes the same setup, in the same order the qualified native profile
/// establishes it: the owned-trace signal policy, the owned context with its
/// guest-origin syscall capture, the published active-but-paused counter, and
/// the precise owned controller — all before the constructor's physical enable
/// and therefore before the admitted owned interval. Loader and startup code
/// have already run by then; this is not "before any guest instruction". The
/// caller must reach this from inside one assembly bracket that calls
/// `__clock_constructor_begin`, this initializer, then
/// `__clock_constructor_finish`, so the finalizer performs the physical
/// enable/leave handoff, and must run no Rust after that enable. The
/// `clocked_initializer!` `.init_array` constructor is one such bracket; a
/// directly linked assembly entry is another.
///
/// # Accepted coverage
///
/// The public policy accepts a nonempty set of native syscall observations,
/// including unchanged full Detcore subscriptions, only with the complete owned
/// setup. vDSO observations additionally require the real retained mapping owner
/// during context initialization. No caller-provided capability replaces that
/// check. Every other public SUD caller retains its instruction/vDSO refusals.
/// Observation does not grant kernel injection permission: unsupported effects,
/// unknown typed syscall numbers and unrequested owned entries terminate before
/// effects. Existing ownership, native frame, FP, control and clock checks still
/// govern each capture and completion. Successful policy validation alone is not
/// startup/guest-FS readiness, successful installation or an executed guest.
///
/// Subscribing CPUID or RDTSC adds two process-global effects, both identical
/// to the qualified private native route: the `SIGSEGV` action installed with a
/// readback comparison, and the `ARCH_SET_CPUID` / `PR_SET_TSC` arming prctls.
/// RDTSC additionally requires `CPUID.1:EDX[4]` and `CPUID.8000_0001:EDX[27]`
/// on the executing host and is refused without them.
///
/// Returns the optional POSIX timer inventory actually observed. `Unavailable`
/// is not an empty inventory, and `Empty` is **not** proof that no POSIX timer
/// was ever created since exec: it is one additional rejection check, never the
/// basis of the caller's historical guarantee below.
///
/// # Safety
///
/// This entry has no safe or unverified wrapper, and the obligations below are
/// not checked for you. They are in addition to, and strictly beyond, the
/// obligations of [`install_tool_with_mode`], whose contract is unchanged.
///
/// The caller must satisfy every ownership requirement of
/// [`install_tool_with_mode`]: it owns the installing thread and all process
/// signal dispositions for the remaining process lifetime, no other application
/// thread may run, no application signal handler, nonlocal signal exit or
/// asynchronous callback may enter during Tool execution or a deferred
/// continuation, all preinstalled handlers are default or ignored, and it does
/// not replace runtime handlers, change masks or altstacks, invoke the trusted
/// gate as a guest bypass, or register unrelated clock hooks.
///
/// It must additionally satisfy every duty of the owned continuation profile:
///
/// - **No POSIX timer may have been created since the process freshly execed**,
///   including by the loader, any preload, a library constructor or runtime
///   startup, and none may be created or armed during the bounded run. Exec
///   deletes POSIX timers but does not establish this later history, and the
///   procfs inventory cannot establish it either; the caller must audit the
///   finite startup path itself.
/// - Executable mappings, TLS, xstate permissions and native controls
///   (segments, XCR0, PKRU, CPUID faulting, TSC) remain stable for the whole
///   run, and the caller owns them exclusively.
/// - No asynchronous callbacks, no additional threads, and no nonlocal exits.
/// - No writes to runtime storage by the application.
/// - Every Tool callback returns normally within the owned runtime stack, and
///   Tool stack use stays below 512 KiB.
/// - Once this returns `Ok`, SUD is armed and owned syscall capture is live, so
///   an ordinary syscall issued by the installer runtime before the assembly
///   handoff — including libc logging or output, allocation, and destructor
///   work — can be captured as the guest's first event prematurely. The
///   installer must issue none, or route any necessary runtime cleanup through
///   the existing trusted raw syscall gate. That gate is for this installer
///   runtime only: it is not an application bypass and grants no runtime-depth
///   exemption. All such cleanup must finish before the initializer returns
///   into the assembly finalizer, and no Rust may run after the physical
///   enable.
/// - This wrapper borrows the coordinator path, so it adds no caller-owned path
///   destructor after SUD installation. The existing inner mask and runtime
///   guards remain in place and release through their existing cleanup path;
///   this is not a claim that no destructor runs across installation.
///
/// This is ordinary in-process ownership, not a hostile-guest isolation
/// boundary. Installation additionally rejects the machine-checkable subset it
/// can observe — exact host qualification, single task, absent interval timers,
/// no pending signals, no writable-executable mapping, expected native controls
/// — and those checks are fixed in the implementation, never chosen by the
/// caller. A host that does not match them is refused, not accommodated.
///
/// # Failure
///
/// The first irreversible effect is `syscall_fallback::initialize`, followed by
/// the coordinator connection and the permanent `COORDINATOR_FD` reservation —
/// all of which precede subscription admission. So:
///
/// - The POSIX timer inventory, `admit_clock_setup` and the SUD signal-mask
///   guard refuse with nothing published at all.
/// - Subscription refusals — the public gate and the capturable-syscall bound — return
///   `Err` with the syscall fallback initialized and the coordinator descriptor
///   number already reserved. The connection itself is dropped and closed on
///   that path; the numeric reservation is what persists, and it has no release
///   API here. So this `Err` does not mean an untouched process; it means no
///   owned state was published.
/// - Signal preparation — the owned-trace reservation and the guest signal state
///   — returns its original `io::Error`. Nothing owned is published there.
/// - From the first owned publication onward, every failure on this route is
///   terminal: the process reports the original cause and its stage through the
///   trusted raw gate and exits 127, rather than returning an `Err` that would
///   suggest a usable partially activated guest.
///
/// Signal preparation — the owned-trace policy reservation and the guest signal
/// state — is **not** in that terminal region. It returns its original `io::Error`
/// so the caller can report the cause, because nothing owned has been published
/// at that point: no owned context, syscall capture, RCB clock, precise
/// controller or Tool handler exists, SUD is not enabled and the physical
/// constructor enable has not run. What does persist on such an `Err`, and is
/// **not** rolled back: the syscall fallback initialization, the permanent
/// coordinator descriptor reservation, any stats state, the selected syscall
/// mode, and — if the owned-trace call succeeded before guest signal preparation
/// refused — the owned-trace reservation. The coordinator connection itself is
/// dropped and closed. The guest signal mask is restored by the existing install
/// guard; a failed restoration is the separate pre-existing fatal path.
/// Returning here authorises neither a retry nor resuming as an instrumented
/// guest.
///
/// As for every other installation entry, a returned `Err` does not promise
/// that runtime or RPC resources were released; do not resume as an
/// instrumented guest.
pub unsafe fn install_tool_owned_native_from_bootstrap<T>(
    coordinator: &Path,
    evidence_bytes: usize,
) -> io::Result<crate::owned_context::PosixTimerInventory>
where
    T: Tool + 'static,
{
    let inventory = crate::owned_context::inspect_posix_timer_inventory(std::fs::read_to_string(
        "/proc/self/timers",
    ))?;
    unsafe {
        install_tool_inner::<T>(
            coordinator,
            false,
            runtime::PatchPublication::Concurrent,
            crate::SyscallMode::UserDispatchWithoutPatching,
            InstructionAdmission::Public,
            Some(OwnedSetup::public_native(evidence_bytes)),
        )?;
    }
    Ok(inventory)
}

/// Test-only installation seam for the reviewed single-thread CPUID fixture.
/// This requires the default-off `test-owned-cpuid` feature. It is not a backend
/// capability or an alternative production installation API. CPUID is the only
/// permitted subscription; all `Guest::set_regs` calls return EOPNOTSUPP, and
/// only the typed CPUID result can update the captured instruction state.
/// Returns the actual optional POSIX timer inventory observation. `Unavailable`
/// is not an empty inventory or proof of timer absence; nonempty available
/// inventory and active interval timers are still rejected.
///
/// # Safety
/// In addition to `install_tool_with_mode`'s ownership requirements, the caller
/// must use a reviewed finite Tool with default lifecycle callbacks and only
/// synchronous RPC, register reads, and bounded ordinary memory work. No other
/// threads, timers, asynchronous callbacks or nonlocal exits may occur. Mappings,
/// code, TLS, native controls and xstate permissions remain stable. Tool stack
/// use must stay below 512 KiB; runtime allocations must not be modified by the
/// application. FP and errno clobbers inside the callback are restored at return.
/// Both original and owned-frame returns rely on this process-lifetime contract.
/// The process must have freshly execed, and no POSIX timer may have been created
/// since that exec, including by loader/preload/library constructors or runtime
/// startup. Exec deletes POSIX timers, but does not establish this later history.
/// No timer may be created or armed during the bounded run. The caller must audit
/// the finite startup/Tool path; this guarantee is required even when procfs timer
/// inventory is unavailable. It does not admit an arbitrary program or preload.
#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
pub unsafe fn __install_owned_cpuid_fixture<T>(
    coordinator: impl AsRef<Path>,
) -> io::Result<crate::owned_context::PosixTimerInventory>
where
    T: Tool + 'static,
{
    unsafe {
        install_owned_fixture::<T>(
            coordinator.as_ref(),
            InstructionAdmission::OwnedCpuidFixture,
        )
    }
}

/// Private CPUID and/or RDTSC/RDTSCP fixture, using `test-owned-cpuid` unchanged.
/// At least one instruction subscription is required; all syscall/vDSO
/// subscriptions are rejected. Public SUD installation still rejects both.
/// Only typed instruction outputs are writable; `Guest::set_regs` still refuses.
/// This does not admit Detcore or establish general FP representation fidelity.
///
/// # Safety
/// All ownership, native-profile, finite Tool, timer-history and lifetime
/// requirements of `__install_owned_cpuid_fixture` apply. The caller additionally
/// owns the TSC faulting control for the whole run. No clock/timer mechanism or
/// additional thread is activated by this fixture contract.
#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
pub unsafe fn __install_owned_instruction_fixture<T>(
    coordinator: impl AsRef<Path>,
) -> io::Result<crate::owned_context::PosixTimerInventory>
where
    T: Tool + 'static,
{
    unsafe {
        install_owned_fixture::<T>(
            coordinator.as_ref(),
            InstructionAdmission::OwnedInstructionFixture,
        )
    }
}

#[cfg(feature = "test-owned-cpuid")]
unsafe fn install_owned_fixture<T: Tool + 'static>(
    coordinator: &Path,
    admission: InstructionAdmission,
) -> io::Result<crate::owned_context::PosixTimerInventory> {
    let inventory = crate::owned_context::inspect_posix_timer_inventory(std::fs::read_to_string(
        "/proc/self/timers",
    ))?;
    unsafe {
        install_tool_inner::<T>(
            coordinator,
            true,
            runtime::PatchPublication::Concurrent,
            crate::SyscallMode::UserDispatchWithoutPatching,
            admission,
            admission.owned_setup(),
        )?;
    }
    Ok(inventory)
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
/// Install the finite instruction fixture with the existing guest-only RCB clock.
///
/// # Safety
/// All requirements of `__install_owned_instruction_fixture` apply, except that
/// this entry requires an assembly-owned clock constructor activation. Its
/// initializer must return through the existing constructor finalizer with no
/// Rust cleanup after the final enable. Only the initial counter allocation is
/// admitted: no reset, rebind, descriptor access or lifecycle transition.
/// There must be no configured, pending or asynchronously generated returning
/// notification. Only the non-sampling shared counter is created; no notification
/// timer is installed. Timer requests still fail. Public SUD admission is unchanged.
pub unsafe fn __install_owned_clocked_instruction_fixture<T>(
    coordinator: impl AsRef<Path>,
) -> io::Result<crate::owned_context::PosixTimerInventory>
where
    T: Tool + 'static,
{
    unsafe {
        install_owned_fixture::<T>(
            coordinator.as_ref(),
            InstructionAdmission::OwnedClockedInstructionFixture,
        )
    }
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
/// Install the finite, stepping-from-arming precise timer fixture.
///
/// # Safety
/// All source, frame-lifetime, startup and clock requirements of
/// `__install_owned_single_step_fixture` apply, but its mandatory post-callback
/// NOP is replaced by the vocabulary below, and precise setters can stage a
/// request in the supported return windows. While a request is armed, guest
/// execution must consist only of NOP, MOV ECX immediate, DEC ECX, direct
/// JZ/JNZ/JMP, and subscribed CPUID/RDTSC/RDTSCP faults. Code and mappings must
/// remain stable, with every successor inside the admitted executable mappings.
/// Syscalls and other faults are not admitted cancellation paths.
/// Only owned instruction/timer callbacks may request RCB plus instruction-suffix
/// timers; requests outside those return windows refuse. No sampling source,
/// arbitrary asynchronous guest signal or public timer capability is admitted.
pub unsafe fn __install_owned_precise_timer_fixture<T: Tool + 'static>(
    coordinator: impl AsRef<Path>,
) -> io::Result<crate::owned_context::PosixTimerInventory> {
    unsafe {
        install_owned_fixture::<T>(
            coordinator.as_ref(),
            InstructionAdmission::OwnedPreciseTimerFixture,
        )
    }
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
/// Install the finite owned SUD getpid/ready-read and precise timer fixture.
///
/// # Safety
/// All requirements of [`__install_owned_precise_timer_fixture`] apply, except
/// that native x86-64 getpid and ready, bounded pipe read are admitted guest
/// syscalls and cancellation points. No blocking, restart, lifecycle or other
/// syscall is admitted. Arguments and pipe storage remain valid throughout
/// injection. Qualification must run unarmed SIGSYS before attempting the exact
/// armed TF SIGSYS ordering and RCX/R11/flags profile on the executing kernel;
/// an unexpected source/profile fails closed, not as a retired step. Only the
/// final guest return stages TF; first ThreadStart may request a timer only
/// with a supported next instruction. All no-async and normal-return lifetime
/// obligations remain. This is not public/default Tool capability admission.
pub unsafe fn __install_owned_syscall_timer_fixture<T: Tool + 'static>(
    coordinator: impl AsRef<Path>,
) -> io::Result<crate::owned_context::PosixTimerInventory> {
    unsafe {
        install_owned_fixture::<T>(
            coordinator.as_ref(),
            InstructionAdmission::OwnedSyscallTimerFixture,
        )
    }
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
/// Install the private native-effect integer stepping profile.
///
/// # Safety
/// All owned-syscall-timer fixture lifetime, source, mask and native qualification
/// preconditions apply. Executed code is stable RX, with no writable alias or
/// external mutator. Only the documented integer forms and mapped private data
/// are admitted; unsupported faults, debug/flag exposure, REP, JIT and lifecycle
/// remain fatal. Evidence bytes are reserved before interception; zero explicitly
/// disables optional history, never correctness checks. Nonzero exhaustion is
/// terminal; qualification must declare its checked complete path-bound budget.
pub unsafe fn __install_owned_compiled_step_fixture<T: Tool + 'static>(
    coordinator: impl AsRef<Path>,
    evidence_bytes: usize,
) -> io::Result<crate::owned_context::PosixTimerInventory> {
    unsafe {
        install_owned_fixture::<T>(
            coordinator.as_ref(),
            InstructionAdmission::OwnedCompiledStepFixture { evidence_bytes },
        )
    }
}

fn admit_subscriptions(
    mode: crate::SyscallMode,
    admission: InstructionAdmission,
    owned: Option<OwnedSetup>,
    subscriptions: &reverie::Subscription,
) -> io::Result<()> {
    if let Some(setup) = owned.filter(|setup| setup.is_public_native())
        && !admission.results_only()
    {
        return admit_public_owned_syscalls(mode, setup, subscriptions);
    }
    if admission.owned_syscalls() {
        if mode != crate::SyscallMode::UserDispatchWithoutPatching
            || subscriptions.iter_syscalls().next().is_none()
            || subscriptions
                .iter_syscalls()
                .any(|number| !crate::syscall_event::backed_returning(number as i64))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "owned execution requires only capturable syscalls",
            ));
        }
        return Ok(());
    }
    if admission.results_only()
        && (mode != crate::SyscallMode::UserDispatchWithoutPatching
            || !(subscriptions.has_cpuid() || subscriptions.has_rdtsc())
            || subscriptions.iter_syscalls().next().is_some())
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned fixture requires only CPUID and/or RDTSC/RDTSCP subscriptions",
        ));
    }
    #[cfg(feature = "test-owned-cpuid")]
    if matches!(admission, InstructionAdmission::OwnedCpuidFixture)
        && (!subscriptions.has_cpuid() || subscriptions.has_rdtsc())
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned CPUID fixture requires CPUID-only subscriptions",
        ));
    }
    if mode == crate::SyscallMode::UserDispatchWithoutPatching
        && ((!admission.results_only() && (subscriptions.has_cpuid() || subscriptions.has_rdtsc()))
            || subscriptions.iter_syscalls().any(|number| {
                matches!(
                    number,
                    Sysno::time
                        | Sysno::gettimeofday
                        | Sysno::clock_gettime
                        | Sysno::clock_getres
                        | Sysno::getcpu
                )
            }))
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "SUD-only does not cover instruction or vDSO subscriptions",
        ));
    }
    Ok(())
}

/// Public observation policy. This does not install or prove ownership: actual
/// context, vDSO, signal, clock and controller setup must still succeed before
/// installation returns, and each capture/completion retains its control checks.
fn admit_public_owned_syscalls(
    mode: crate::SyscallMode,
    owned: OwnedSetup,
    subscriptions: &reverie::Subscription,
) -> io::Result<()> {
    if !owned.is_public_native()
        || mode != crate::SyscallMode::UserDispatchWithoutPatching
        || subscriptions.iter_syscalls().next().is_none()
        || subscriptions
            .iter_syscalls()
            .any(|number| !crate::syscall_event::observable(number as i64))
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "public owned observation requires the complete native setup and native syscall entries",
        ));
    }
    Ok(())
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
/// Installs the finite one-NOP step primitive; neither timer setter succeeds.
/// Each subscribed instruction callback must be followed by one literal NOP.
///
/// # Safety
/// All requirements of [`__install_owned_clocked_instruction_fixture`] apply,
/// except this owns one synchronous SIGTRAP completion source. The caller must
/// additionally exclude pre-existing TF, debuggers/hardware breakpoints and all
/// competing debug sources. SIGTRAP is initially default/unblocked; no runtime
/// signal registration may coexist. No guest code may inspect/change TF while
/// stepping, and no arbitrary syscall/fault/async stepping is supported.
pub unsafe fn __install_owned_single_step_fixture<T: Tool + 'static>(
    coordinator: impl AsRef<Path>,
) -> io::Result<crate::owned_context::PosixTimerInventory> {
    unsafe {
        install_owned_fixture::<T>(
            coordinator.as_ref(),
            InstructionAdmission::OwnedSingleStepFixture,
        )
    }
}

unsafe fn install_tool_inner<T>(
    coordinator: &Path,
    remove_legacy_environment: bool,
    publication: runtime::PatchPublication,
    mode: crate::SyscallMode,
    admission: InstructionAdmission,
    owned: Option<OwnedSetup>,
) -> io::Result<()>
where
    T: Tool + 'static,
{
    let _runtime = crate::runtime_domain::Entry::enter();
    if let Some(setup) = owned {
        crate::owned_context::admit_clock_setup(setup.clocked)?;
    }
    if mode == crate::SyscallMode::UserDispatchWithoutPatching
        && (crate::clock_control::requested() || crate::clock_control::active())
        && !reverie_preload::signal::runtime_signals_configured()
        && !owned.is_some_and(|setup| setup.clocked)
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "SUD-only shared-clock execution needs an owned signal-mask policy",
        ));
    }
    crate::syscall_fallback::initialize()?;
    let resources = crate::rpc::resources::Resources::new();
    let rpc = CoordinatorRpc::<T::GlobalState>::connect_bootstrap(coordinator, resources.clone())?;
    let stats = if let Some(stats_coordinator) =
        std::env::var_os(crate::bootstrap::STATS_COORDINATOR_ENV)
    {
        let stats = crate::stats::initialize_guest_stats(Path::new(&stats_coordinator))?;
        // SAFETY: tool installation runs before application-created threads.
        unsafe { std::env::remove_var(crate::bootstrap::STATS_COORDINATOR_ENV) };
        stats
    } else {
        crate::stats::GuestStatsHooks::DISABLED
    };
    let pid = Pid::from_raw(unsafe { libc::getpid() });
    let subscriptions = T::subscriptions(rpc.config());
    admit_subscriptions(mode, admission, owned, &subscriptions)?;
    if let Some(setup) = owned
        && !admission.results_only()
    {
        admit_public_owned_syscalls(mode, setup, &subscriptions)?;
    }
    let instruction_subscriptions = runtime::InstructionSubscriptions {
        cpuid: subscriptions.has_cpuid(),
        rdtsc: subscriptions.has_rdtsc(),
    };
    let early_signal_state = prepare_owned_signal_state(mode, owned, instruction_subscriptions)?;
    let installation = (|| -> Result<(), (SetupStage, io::Error)> {
        if let Some(setup) = owned {
            crate::owned_context::initialize(
                instruction_subscriptions,
                setup.clocked,
                setup.single_step,
                setup.precise_timer,
                setup.syscalls,
                setup.native_evidence,
            )
            .map_err(|error| (SetupStage::OwnedContext, error))?;
            if setup.is_public_native() && !admission.results_only() {
                crate::owned_context::require_observation_coverage(&subscriptions)
                    .map_err(|error| (SetupStage::OwnedContext, error))?;
            }
            if setup.syscalls {
                unsafe {
                    reverie_preload::trap::register_owned_user_dispatch(
                        &crate::owned_context::SYSCALL_CAPTURE,
                    )
                    .map_err(|error| (SetupStage::SyscallCapture, error))?;
                }
            }
        }
        runtime::initialize_rcb_clock().map_err(|error| (SetupStage::RcbClock, error))?;
        if owned.is_some_and(|setup| setup.precise_timer) {
            crate::timer::initialize_owned()
                .map_err(|error| (SetupStage::PreciseController, error))?;
        }
        if !owned.is_some_and(|setup| setup.clocked) {
            crate::timer::initialize().map_err(|error| (SetupStage::Timer, error))?;
        }
        runtime::preflight_instruction_faulting(instruction_subscriptions)
            .map_err(|error| (SetupStage::InstructionPreflight, error))?;
        let vdso_sites = if crate::syscall_mode::sud_only() {
            Vec::new()
        } else {
            crate::syscall_mode::vdso_rewrite()
                .map_err(|error| (SetupStage::VdsoRewrite, error))?;
            reverie::vdso::patch_current_vdso(&subscriptions)
                .map_err(|error| (SetupStage::VdsoRewrite, io::Error::other(error.to_string())))?
        };
        let _signal_state = if early_signal_state.is_none() {
            Some(
                runtime::prepare_guest_signal_state(instruction_subscriptions)
                    .map_err(|error| (SetupStage::GuestSignalState, error))?,
            )
        } else {
            None
        };
        let syscall_subscriptions = subscriptions.iter_syscalls().collect();
        if remove_legacy_environment {
            // SAFETY: legacy tool installation runs before application-created threads.
            unsafe { std::env::remove_var(crate::bootstrap::COORDINATOR_ENV) };
        }
        let tool = T::new(pid, rpc.config());
        HANDLER
            .set(Box::new(ToolHost::<T> {
                registry: Registry::with_resources(pid, tool, rpc.resources()),
                rpc: Arc::new(rpc),
                root_pid: pid,
                subscriptions: syscall_subscriptions,
                instruction_subscriptions,
                instruction_results_only: instruction_results_only(admission, owned),
                stats,
            }))
            .map_err(|_| {
                (
                    SetupStage::ToolHandler,
                    io::Error::new(io::ErrorKind::AlreadyExists, "Reverie tool installed twice"),
                )
            })?;
        runtime::initialize_reverie_tool(stats, publication, instruction_subscriptions, &vdso_sites)
            .map_err(|error| (SetupStage::ReverieTool, error))
    })();
    if let Err((stage, error)) = &installation {
        unsafe { terminate_after_publication(*stage, error) };
    }
    installation.map_err(|(_, error)| error)
}

/// Signal preparation shared by every installation route.
///
/// Kept as one named function so host controls exercise the same code the
/// install path runs. An error here is returned to the caller: nothing owned has
/// been published yet, so the caller can report the cause exactly as the private
/// fixture does. Effects that persist are documented on
/// [`install_tool_owned_native_from_bootstrap`]; this is not a rollback and
/// authorises neither a retry nor resuming an instrumented guest.
fn prepare_owned_signal_state(
    mode: crate::SyscallMode,
    owned: Option<OwnedSetup>,
    instruction_subscriptions: runtime::InstructionSubscriptions,
) -> io::Result<Option<runtime::SignalInstallGuard>> {
    crate::syscall_mode::select(mode);
    if owned.is_some_and(|setup| setup.single_step) {
        unsafe {
            reverie_preload::signal::owned_trace::configure(crate::owned_context::signal_entry)?
        };
    }
    if crate::syscall_mode::sud_only() {
        Ok(Some(runtime::prepare_guest_signal_state(
            instruction_subscriptions,
        )?))
    } else {
        Ok(None)
    }
}

/// Terminates a clocked SUD installation whose committed region failed, after
/// reporting the original cause and its stage through the trusted raw gate.
///
/// Returns normally when the route is not a clocked SUD install, preserving the
/// existing `Err` behaviour for every other route. The exit status is unchanged
/// at 127 for every sink outcome. A sink failure is **unobservable in
/// production**: there is by definition nowhere to write it, so it is discarded
/// here and never substitutes a different status or a signal. Only the
/// direct-call host tests inspect it. No signal disposition is touched.
///
/// This boundary is **conservative**. It begins at the owned-context
/// initialization *attempt*, so reaching it does not prove that owned context was
/// published — only that preparation completed and commitment was attempted. It
/// claims no transactional rollback, and covers neither a panic nor a `Drop` path.
///
/// SIGPIPE is blocked here by the live guard that `prepare_guest_signal_state`
/// installed, in both modes: with no returning source configured the owned-trace
/// path requests all signals but SIGSYS, and with a configured returning source
/// the all-signals install mask stays in place. Linux never blocks SIGKILL or
/// SIGSTOP, which is irrelevant to a write. So a reader-less stderr pipe yields
/// `EPIPE` and a closed descriptor yields `EBADF`; neither can raise a signal
/// here.
///
/// # Safety
/// Only valid after the committed region has failed: the process has published
/// global state it cannot retract, so it must not resume as a guest.
unsafe fn terminate_after_publication(stage: SetupStage, error: &io::Error) {
    if crate::syscall_mode::sud_only() && crate::clock_control::requested() {
        let _unreportable = report_terminal_setup_failure(stage, error);
        unsafe { runtime::exit_now(127) };
    }
}

pub(crate) fn dispatch(event: &mut SyscallEvent) {
    let _runtime = crate::runtime_domain::Entry::enter();
    match HANDLER.get() {
        Some(handler) => handler.dispatch(event),
        None if crate::owned_context::syscall_mode() => {
            terminate_owned_observation(event.number, ObservationRefusal::MissingHandler)
        }
        None => event.result = -i64::from(libc::ENOSYS),
    }
}

pub(crate) fn dispatch_instruction(
    kind: runtime::InstructionEventKind,
    context: &mut HookContext,
) -> InstructionResult {
    let _runtime = crate::runtime_domain::Entry::enter();
    match HANDLER.get() {
        Some(handler) => handler
            .dispatch_owned(Some(kind), context)
            .unwrap_or_else(|| fatal(126)),
        None => fatal(126),
    }
}

pub(crate) fn dispatch_vdso(event: &mut SyscallEvent) {
    let _runtime = crate::runtime_domain::Entry::enter();
    let handler = HANDLER.get().unwrap_or_else(|| fatal(126));
    if !handler.observes_syscall(event.number) {
        fatal(126);
    }
    handler.dispatch(event);
}

pub(crate) fn rng_snapshot(owner: i64) -> Result<reverie::vdso::VdsoRngSnapshot, Error> {
    let _runtime = crate::runtime_domain::Entry::enter();
    HANDLER
        .get()
        .ok_or_else(|| io::Error::other("modeled RNG input has no registered Tool"))?
        .rng_snapshot(owner)
}

pub(crate) fn guest_progress(
    owner: i64,
    context: &mut HookContext,
    clock: u64,
) -> Result<(), Error> {
    let _runtime = crate::runtime_domain::Entry::enter();
    HANDLER
        .get()
        .ok_or_else(|| io::Error::other("guest progress has no registered Tool"))?
        .guest_progress(owner, context, clock)
}

pub(crate) fn dispatch_timer(context: &mut HookContext) {
    let _runtime = crate::runtime_domain::Entry::enter();
    match HANDLER.get() {
        Some(handler) => {
            handler.dispatch_owned(None, context);
        }
        None => fatal(126),
    }
}

#[cfg(feature = "private-crt")]
pub(crate) fn prepare_initial() -> io::Result<()> {
    HANDLER
        .get()
        .ok_or_else(|| io::Error::other("initial transition has no installed Tool"))?
        .prepare_initial()
}

#[cfg(feature = "private-crt")]
pub(crate) fn dispatch_initial(context: &mut HookContext) {
    HANDLER
        .get()
        .unwrap_or_else(|| fatal(126))
        .dispatch_initial(context);
}

struct ToolHost<T: Tool> {
    registry: Registry<T>,
    rpc: Arc<CoordinatorRpc<T::GlobalState>>,
    root_pid: Pid,
    subscriptions: HashSet<Sysno>,
    instruction_subscriptions: runtime::InstructionSubscriptions,
    instruction_results_only: bool,
    stats: crate::stats::GuestStatsHooks,
}

#[cfg(test)]
pub(crate) fn install_domain_test_tool(
    rpc: CoordinatorRpc<crate::runtime_domain::tests::DomainGlobal>,
) {
    use crate::runtime_domain::tests::DomainTool;
    let _runtime = crate::runtime_domain::Entry::enter();
    let host = ToolHost::<DomainTool> {
        registry: Registry::with_resources(raw_pid(libc::SYS_getpid), DomainTool, rpc.resources()),
        rpc: Arc::new(rpc),
        root_pid: raw_pid(libc::SYS_getpid),
        subscriptions: [Sysno::getpid].into_iter().collect(),
        instruction_subscriptions: runtime::InstructionSubscriptions {
            cpuid: false,
            rdtsc: true,
        },
        instruction_results_only: false,
        stats: crate::stats::GuestStatsHooks::DISABLED,
    };
    assert!(HANDLER.set(Box::new(host)).is_ok());
}

/// Test-only, default-off dispatch harness.
///
/// Installs `tool` as the process handler **without** SUD, without patching and
/// without any owned or native setup, so a host control can drive Tool
/// callbacks from a synthetic `HookContext`. It deliberately bypasses the
/// installation admission gates, so it establishes nothing about what public
/// installation admits, and it is neither a backend capability nor a production
/// route. Non-owned dispatch only: it cannot produce owned state or readiness.
#[cfg(any(test, feature = "test-tool-host-dispatch"))]
#[doc(hidden)]
pub fn __install_dispatch_only_tool_host<T: Tool + 'static>(
    tool: T,
    rpc: CoordinatorRpc<T::GlobalState>,
    subscriptions: &reverie::Subscription,
) {
    let _runtime = crate::runtime_domain::Entry::enter();
    let host = ToolHost::<T> {
        registry: Registry::with_resources(raw_pid(libc::SYS_getpid), tool, rpc.resources()),
        rpc: Arc::new(rpc),
        root_pid: raw_pid(libc::SYS_getpid),
        subscriptions: subscriptions.iter_syscalls().collect(),
        instruction_subscriptions: runtime::InstructionSubscriptions {
            cpuid: subscriptions.has_cpuid(),
            rdtsc: subscriptions.has_rdtsc(),
        },
        instruction_results_only: false,
        stats: crate::stats::GuestStatsHooks::DISABLED,
    };
    assert!(HANDLER.set(Box::new(host)).is_ok());
}

impl<T: Tool + 'static> ToolHost<T> {
    fn begin_invocation(
        &self,
        pid: Pid,
        tid: Pid,
        scratch: &ScratchOwner,
        allow_root: bool,
    ) -> Result<(Invocation<'_, T>, bool), Error> {
        #[cfg(test)]
        crate::runtime_domain::tests::at(crate::runtime_domain::tests::TOOL);
        #[cfg(test)]
        crate::runtime_domain::tests::at(crate::runtime_domain::tests::STATES);
        if allow_root && self.registry.unstarted(pid) && self.rpc.identity() == (pid, tid) {
            let mut invocation =
                self.registry
                    .reserve(pid, tid, self.rpc.clone(), scratch, true)?;
            invocation.initialize(None)?;
            Ok((invocation, true))
        } else {
            let identity = self.registry.identity(pid, tid)?;
            Ok((self.registry.acquire(identity, scratch)?, false))
        }
    }

    async fn progress_future(
        &self,
        owner: i64,
        context: &mut HookContext,
        clock: u64,
        scratch: &ScratchOwner,
    ) -> Result<(), Error> {
        let tid = raw_pid(libc::SYS_gettid);
        if i64::from(tid.as_raw()) != owner {
            return Err(io::Error::other("guest progress owner mismatch").into());
        }
        let pid = raw_pid(libc::SYS_getpid);
        let (mut invocation, _) = self.begin_invocation(pid, tid, scratch, false)?;
        let (tool, state, rpc) = invocation.parts();
        let tail = TailResult::default();
        let mut guest = LiteinstGuest::<T> {
            scratch,
            stop: GuestStop::Progress { context, clock },
            tid,
            pid,
            ppid: (pid != self.root_pid).then(|| raw_pid(libc::SYS_getppid)),
            state,
            rpc,
            tail: &tail,
            cpuid_interception: self.instruction_subscriptions.cpuid,
            instruction_results_only: self.instruction_results_only,
            fork_parent_state: None,
        };
        tool.handle_guest_progress(&mut guest).await?;
        drop(guest);
        invocation.complete()?;
        Ok(())
    }
}

impl<T> ToolHandler for ToolHost<T>
where
    T: Tool + 'static,
{
    fn guest_progress(
        &self,
        owner: i64,
        context: &mut HookContext,
        clock: u64,
    ) -> Result<(), Error> {
        let scratch = DispatchScratchScope::enter();
        drive_ready(self.progress_future(owner, context, clock, &scratch.owner))
    }

    fn rng_snapshot(&self, owner: i64) -> Result<reverie::vdso::VdsoRngSnapshot, Error> {
        let tid = raw_pid(libc::SYS_gettid);
        if i64::from(tid.as_raw()) != owner {
            return Err(io::Error::other("modeled RNG input owner mismatch").into());
        }
        let scratch = DispatchScratchScope::enter();
        let (mut invocation, _) =
            self.begin_invocation(raw_pid(libc::SYS_getpid), tid, &scratch.owner, false)?;
        let (tool, state, _) = invocation.parts();
        let snapshot = tool.vdso_rng_snapshot(state)?;
        invocation.complete()?;
        Ok(snapshot)
    }

    #[cfg(feature = "private-crt")]
    fn prepare_initial(&self) -> io::Result<()> {
        if raw_pid(libc::SYS_getpid) != self.root_pid
            || raw_pid(libc::SYS_gettid) != self.root_pid
            || !self.registry.unstarted(self.root_pid)
        {
            return Err(io::Error::other(
                "initial transition requires an unstarted installed root Tool",
            ));
        }
        Ok(())
    }

    #[cfg(feature = "private-crt")]
    fn dispatch_initial(&self, context: &mut HookContext) {
        let _scratch_scope = DispatchScratchScope::enter();
        self.prepare_initial().unwrap_or_else(|error| {
            runtime::exit_io126("initial-dispatch/unstarted-root-tool", &error)
        });
        let (mut invocation, _) = self
            .begin_invocation(self.root_pid, self.root_pid, &_scratch_scope.owner, true)
            .unwrap_or_else(|error| tool_fatal(126, &error));
        let (tool, state, rpc) = invocation.parts();
        let tail = TailResult::default();
        let mut carrier = SyscallEvent {
            owned_binding: None,
            exit: None,
            number: -1,
            args: [0; 6],
            result: 0,
            instruction_pointer: context.instruction_pointer,
            context: context as *mut HookContext as usize,
        };
        let mut guest = LiteinstGuest::<T> {
            scratch: &_scratch_scope.owner,
            stop: GuestStop::Event(&mut carrier),
            tid: self.root_pid,
            pid: self.root_pid,
            ppid: None,
            state,
            rpc,
            tail: &tail,
            cpuid_interception: self.instruction_subscriptions.cpuid,
            instruction_results_only: self.instruction_results_only,
            fork_parent_state: None,
        };
        drive_ready(tool.handle_thread_start(&mut guest))
            .unwrap_or_else(|error| tool_fatal(124, &error));
        drive_ready(tool.handle_post_exec(&mut guest))
            .unwrap_or_else(|error| tool_fatal(124, &error.into()));
        drop(guest);
        invocation
            .complete()
            .unwrap_or_else(|error| tool_fatal(126, &error.into()));
    }

    fn dispatch(&self, event: &mut SyscallEvent) {
        if let Err(reason) = validate_owned_observation(
            crate::owned_context::syscall_mode(),
            event.number,
            &self.subscriptions,
        ) {
            terminate_owned_observation(event.number, reason);
        }
        let _scratch_scope = DispatchScratchScope::enter();
        let tid = raw_pid(libc::SYS_gettid);
        let pid = raw_pid(libc::SYS_getpid);
        let ppid = (pid != self.root_pid).then(|| raw_pid(libc::SYS_getppid));

        let (mut invocation, is_new) = self
            .begin_invocation(pid, tid, &_scratch_scope.owner, true)
            .unwrap_or_else(|error| tool_fatal(126, &error));
        let (tool, state, rpc) = invocation.parts();
        let tail = TailResult::default();
        let mut guest = LiteinstGuest::<T> {
            scratch: &_scratch_scope.owner,
            stop: GuestStop::Event(event),
            tid,
            pid,
            ppid,
            state,
            rpc,
            tail: &tail,
            cpuid_interception: self.instruction_subscriptions.cpuid,
            instruction_results_only: self.instruction_results_only,
            fork_parent_state: None,
        };

        if is_new && let Err(error) = drive_ready(tool.handle_thread_start(&mut guest)) {
            tool_fatal(124, &error);
        }

        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-liteinst-post-exec): The kernel exec'd the guest
        // image before this LD_PRELOAD backend attached, so — unlike the ptrace
        // backend — the `Tool::handle_post_exec` lifecycle callback never fired.
        // A Tool such as Detcore relies on it to determinize the auxv AT_RANDOM
        // vector and to advance per-thread state (e.g. its seeded PRNG) exactly
        // as the ptrace backend does; without it the guest-visible getrandom(2)
        // stream is offset relative to ptrace and cross-backend parity fails.
        // The guest genuinely did execve into this image, so emitting the event
        // once for the root process's main thread restores contract parity. It
        // is intentionally not emitted for child threads (there is none in the
        // current single-process/thread tool mode) nor re-emitted per dispatch.
        if is_new
            && tid.as_raw() == self.root_pid.as_raw()
            && let Err(error) = drive_ready(tool.handle_post_exec(&mut guest))
        {
            // handle_post_exec returns Errno; tool_fatal expects reverie::Error.
            tool_fatal(124, &Error::from(error));
        }

        let Some(number) = usize::try_from(guest.event().number)
            .ok()
            .and_then(Sysno::new)
        else {
            guest.event_mut().result = -i64::from(libc::ENOSYS);
            drop(guest);
            invocation
                .complete()
                .unwrap_or_else(|error| tool_fatal(126, &error.into()));
            return;
        };
        if !self.subscriptions.contains(&number) {
            let number = guest.event().number;
            let args = guest.event().args;
            if crate::owned_context::syscall_mode() && crate::mapping::operation(number) {
                unsafe { terminate_unsupported_owned_injection(number) };
            }
            if is_plain_fork(number, args) {
                guest.prepare_fork_parent_state();
                let result = forward_plain_fork(number, args);
                if result == 0 {
                    let parent_state = guest.take_fork_parent_state();
                    drop(guest);
                    drop(invocation);
                    finish_fork_child(
                        &self.registry,
                        &self.rpc,
                        self.stats,
                        event,
                        parent_state,
                        ForkChildContext {
                            parent_tid: tid,
                            parent_pid: pid,
                            child_tid: raw_pid(libc::SYS_gettid),
                            child_pid: raw_pid(libc::SYS_getpid),
                        },
                    );
                } else {
                    guest.event_mut().result = result;
                    drop(guest);
                    invocation
                        .complete()
                        .unwrap_or_else(|error| tool_fatal(126, &error.into()));
                }
                return;
            } else if is_exit_syscall(number) {
                drop(guest);
                finish_tool_exit(
                    &self.registry,
                    invocation,
                    self.stats,
                    ToolExitContext {
                        tid,
                        pid,
                        number,
                        args,
                    },
                );
                event.result = guarded_raw_injection(number, args);
                return;
            } else if let Some(result) = injected_syscall_guard(number, args) {
                guest.event_mut().result = result;
                drop(guest);
                invocation
                    .complete()
                    .unwrap_or_else(|error| tool_fatal(126, &error.into()));
                return;
            }
            guest.event_mut().result = guarded_raw_injection(number, args);
            drop(guest);
            invocation
                .complete()
                .unwrap_or_else(|error| tool_fatal(126, &error.into()));
            return;
        }
        let args = guest.event().args.map(|arg| arg as usize);
        let syscall = Syscall::from_raw(
            number,
            SyscallArgs::new(args[0], args[1], args[2], args[3], args[4], args[5]),
        );

        // Drive the Tool handler to a terminal outcome. The shared driver owns
        // the ERESTARTSYS restart protocol (Reverie #362) so it cannot
        // drift between the in-guest backends; this host maps each terminal
        // outcome onto its own per-thread lifecycle (exit/fork-child) state.
        match drive_tool_syscall(tool, &mut guest, syscall, &tail) {
            DrivenSyscall::Result(value) => {
                guest.event_mut().result = value;
                drop(guest);
                invocation
                    .complete()
                    .unwrap_or_else(|error| tool_fatal(126, &error.into()));
            }
            DrivenSyscall::Exit { number, args } => {
                drop(guest);
                let owned_exit = if crate::owned_context::syscall_mode() {
                    if event.exit.is_some() {
                        crate::owned_context::exit::failed("owned-exit/duplicate", None);
                    }
                    Some(
                        crate::owned_context::exit::authorize(
                            event.owned_binding.take(),
                            i64::from(self.root_pid.as_raw()),
                            i64::from(tid.as_raw()),
                            i64::from(pid.as_raw()),
                            self.registry.retained_entries(),
                            number,
                            args,
                        )
                        .unwrap_or_else(|_| {
                            crate::owned_context::exit::failed("owned-exit/authorization", None)
                        }),
                    )
                } else {
                    None
                };
                if let Some(request) = owned_exit {
                    let completed = finish_tool_exit_callbacks(
                        &self.registry,
                        invocation,
                        ToolExitContext {
                            tid,
                            pid,
                            number,
                            args,
                        },
                        |tid| submit_exit_stats(tid, self.stats),
                        || self.rpc.retire().map_err(Error::from),
                    )
                    .unwrap_or_else(|error| {
                        crate::guest_log::mark_failed();
                        tool_fatal(125, &error)
                    });
                    if !completed {
                        crate::owned_context::exit::failed("owned-exit/process-hook", None);
                    }
                    event.exit = Some(request);
                    return;
                }
                finish_tool_exit(
                    &self.registry,
                    &self.rpc,
                    invocation,
                    self.stats,
                    ToolExitContext {
                        tid,
                        pid,
                        number,
                        args,
                    },
                );
                event.result = unsafe { raw_syscall6(number, args) };
            }
            DrivenSyscall::ForkChild {
                parent_tid,
                parent_pid,
                child_tid,
                child_pid,
            } => {
                let parent_state = guest.take_fork_parent_state();
                drop(guest);
                drop(invocation);
                finish_fork_child(
                    &self.registry,
                    &self.rpc,
                    self.stats,
                    event,
                    parent_state,
                    ForkChildContext {
                        parent_tid,
                        parent_pid,
                        child_tid,
                        child_pid,
                    },
                );
            }
            DrivenSyscall::Fatal(error) => tool_fatal(125, &error),
        }
    }

    fn observes_syscall(&self, number: i64) -> bool {
        usize::try_from(number)
            .ok()
            .and_then(Sysno::new)
            .is_some_and(|number| self.subscriptions.contains(&number))
    }

    fn dispatch_owned(
        &self,
        kind: Option<runtime::InstructionEventKind>,
        context: &mut HookContext,
    ) -> Option<InstructionResult> {
        let _scratch_scope = DispatchScratchScope::enter();
        let tid = raw_pid(libc::SYS_gettid);
        let pid = raw_pid(libc::SYS_getpid);
        let ppid = (pid != self.root_pid).then(|| raw_pid(libc::SYS_getppid));
        let (mut invocation, is_new) = self
            .begin_invocation(pid, tid, &_scratch_scope.owner, true)
            .unwrap_or_else(|error| tool_fatal(126, &error));
        let (tool, state, rpc) = invocation.parts();
        let tail = TailResult::default();
        let mut event = SyscallEvent {
            owned_binding: None,
            exit: None,
            number: -1,
            args: [0; 6],
            instruction_pointer: context.instruction_pointer,
            result: 0,
            context: context as *mut HookContext as usize,
        };
        let mut guest = LiteinstGuest::<T> {
            scratch: &_scratch_scope.owner,
            stop: GuestStop::Event(&mut event),
            tid,
            pid,
            ppid,
            state,
            rpc,
            tail: &tail,
            cpuid_interception: self.instruction_subscriptions.cpuid,
            instruction_results_only: self.instruction_results_only,
            fork_parent_state: None,
        };
        if is_new && let Err(error) = drive_ready(tool.handle_thread_start(&mut guest)) {
            tool_fatal(124, &error);
        }
        if is_new
            && tid.as_raw() == self.root_pid.as_raw()
            && let Err(error) = drive_ready(tool.handle_post_exec(&mut guest))
        {
            tool_fatal(124, &Error::from(error));
        }

        let Some(kind) = kind else {
            drive_ready(tool.handle_timer_event(&mut guest));
            drop(guest);
            invocation
                .complete()
                .unwrap_or_else(|error| tool_fatal(126, &error.into()));
            return None;
        };
        let result = match kind {
            runtime::InstructionEventKind::Cpuid => {
                let result = drive_ready(tool.handle_cpuid_event(
                    &mut guest,
                    context.rax as u32,
                    context.rcx as u32,
                ))
                .unwrap_or_else(|error| tool_fatal(125, &Error::from(error)));
                InstructionResult::Cpuid(result)
            }
            runtime::InstructionEventKind::Rdtsc | runtime::InstructionEventKind::Rdtscp => {
                let request = if kind == runtime::InstructionEventKind::Rdtscp {
                    Rdtsc::Tscp
                } else {
                    Rdtsc::Tsc
                };
                let result = drive_ready(tool.handle_rdtsc_event(&mut guest, request))
                    .unwrap_or_else(|error| tool_fatal(125, &Error::from(error)));
                InstructionResult::Rdtsc { request, result }
            }
        };
        drop(guest);
        invocation
            .complete()
            .unwrap_or_else(|error| tool_fatal(126, &error.into()));
        crate::instruction_event::apply_result(context, result);
        Some(result)
    }
}

fn finish_fork_child<T: Tool>(
    registry: &Registry<T>,
    rpc: &Arc<CoordinatorRpc<T::GlobalState>>,
    stats: crate::stats::GuestStatsHooks,
    event: &mut SyscallEvent,
    parent_snapshot: T::ThreadState,
    context: ForkChildContext,
) {
    let scratch = DispatchScratchScope::enter();
    let ForkChildContext {
        parent_tid,
        parent_pid,
        child_tid,
        child_pid,
    } = context;
    runtime::emit_in_guest_stage(b"fork-child-thread-start-begin");
    // This child inherited the parent's coordinator connection. Flag it before
    // any child-side callback can issue an RPC (`handle_thread_start` below is
    // the first such opportunity) so the next `send_rpc` reconnects under the
    // child's own identity. Doing it here rather than from a `pthread_atfork`
    // hook also covers forks that never enter libc, such as a raw `SYS_fork` or
    // a raw plain `SYS_clone`.
    crate::rpc::note_fork_in_child();
    rpc.reconnect_after_fork();
    let child_tool = T::new(child_pid, rpc.config());
    registry
        .replace_fork_process(parent_pid, child_pid, child_tool)
        .unwrap_or_else(|error| tool_fatal(126, &error.into()));
    let mut invocation = registry
        .reserve(child_pid, child_tid, rpc.clone(), &scratch.owner, true)
        .unwrap_or_else(|error| tool_fatal(126, &error.into()));
    invocation
        .initialize(Some((parent_tid, &parent_snapshot)))
        .unwrap_or_else(|error| tool_fatal(126, &error.into()));
    runtime::reset_fallback_observability();
    stats.reset_after_fork();
    if event.context != 0 {
        runtime::record_fork_child_direct_hook(event.instruction_pointer);
    }

    let (tool, state, rpc) = invocation.parts();
    let child_tail = TailResult::default();
    let mut child_guest = LiteinstGuest::<T> {
        scratch: &scratch.owner,
        stop: GuestStop::Event(event),
        tid: child_tid,
        pid: child_pid,
        ppid: Some(parent_pid),
        state,
        rpc,
        tail: &child_tail,
        cpuid_interception: runtime::cpuid_interception_enabled(),
        instruction_results_only: false,
        fork_parent_state: None,
    };
    if let Err(error) = drive_ready(tool.handle_thread_start(&mut child_guest)) {
        tool_fatal(124, &error);
    }
    runtime::emit_in_guest_stage(b"fork-child-thread-start-complete");
    child_guest.event_mut().result = 0;
    drop(child_guest);
    invocation
        .complete()
        .unwrap_or_else(|error| tool_fatal(126, &error.into()));
}

struct ForkChildContext {
    parent_tid: Pid,
    parent_pid: Pid,
    child_tid: Pid,
    child_pid: Pid,
}

// TODO-HUMAN-REVIEW(PR-143): Review single-process Tool exit lifecycle.
fn finish_tool_exit<T: Tool>(
    registry: &Registry<T>,
    rpc: &CoordinatorRpc<T::GlobalState>,
    invocation: Invocation<'_, T>,
    stats: crate::stats::GuestStatsHooks,
    context: ToolExitContext,
) {
    if finish_tool_exit_callbacks(
        registry,
        invocation,
        context,
        |tid| submit_exit_stats(tid, stats),
        || rpc.retire().map_err(Error::from),
    )
    .unwrap_or_else(|error| tool_fatal(125, &error))
    {
        crate::guest_log::finish();
    }
}

fn submit_exit_stats(tid: Pid, stats: crate::stats::GuestStatsHooks) -> Result<(), Error> {
    if stats.is_enabled() {
        runtime::submit_process_stats(tid, stats)?;
    }
    Ok(())
}

fn finish_tool_exit_callbacks<T: Tool, Client: crate::rpc::BoundRpc<T::GlobalState>>(
    registry: &Registry<T, Client>,
    mut invocation: Invocation<'_, T, Client>,
    context: ToolExitContext,
    submit_stats: impl FnOnce(Pid) -> Result<(), Error>,
    retire_rpc: impl FnOnce() -> Result<(), Error>,
) -> Result<bool, Error> {
    let ToolExitContext {
        tid,
        pid,
        number,
        args,
    } = context;
    let process_exit = is_process_exit(number, tid, pid);
    let terminal = if process_exit {
        Some(registry.terminal_resource()?)
    } else {
        None
    };
    if process_exit {
        registry.close(invocation.identity())?;
    }
    let client = invocation.client();
    let state = invocation.take_state();
    let status = reverie::ExitStatus::Exited((args[0] & 0xff) as i32);
    let (tool, rpc) = invocation.exit_parts();
    drive_ready(tool.on_exit_thread(tid, rpc, state, status))?;
    drop(invocation);
    if process_exit {
        let tool = drive_ready(registry.consume(pid))?;
        drive_ready(tool.on_exit_process(pid, &*client, status))?;
        submit_stats(tid)?;
        retire_rpc()?;
        drop(client);
        drop(terminal);
        drive_ready(registry.drain_resources())?;
        Ok(true)
    } else {
        Ok(false)
    }
}

struct ToolExitContext {
    tid: Pid,
    pid: Pid,
    number: i64,
    args: [u64; 6],
}

// TODO-HUMAN-REVIEW(PR-143): Review exit syscall lifecycle classification.
fn is_exit_syscall(number: i64) -> bool {
    // AUTONOMOUS-BOT-IMPLEMENTED
    matches!(number, libc::SYS_exit | libc::SYS_exit_group)
}

// TODO-HUMAN-REVIEW(PR-143): Review single-process exit classification.
fn is_process_exit(number: i64, tid: Pid, pid: Pid) -> bool {
    // AUTONOMOUS-BOT-IMPLEMENTED
    number == libc::SYS_exit_group || tid == pid
}

fn raw_pid(number: i64) -> Pid {
    let value = unsafe { raw_syscall6(number, [0; 6]) };
    if value <= 0 {
        unsafe {
            reverie_preload::trap::terminal126("tool-host/raw-pid", "raw-result", Some(value))
        };
    }
    Pid::from_raw(value as i32)
}

enum GuestStop<'a> {
    Event(&'a mut SyscallEvent),
    Progress {
        context: &'a mut HookContext,
        clock: u64,
    },
}

struct LiteinstGuest<'a, T: Tool> {
    scratch: &'a ScratchOwner,
    stop: GuestStop<'a>,
    tid: Pid,
    pid: Pid,
    ppid: Option<Pid>,
    state: &'a mut T::ThreadState,
    rpc: &'a CoordinatorRpc<T::GlobalState>,
    tail: &'a TailResult,
    cpuid_interception: bool,
    instruction_results_only: bool,
    fork_parent_state: Option<T::ThreadState>,
}

impl<T: Tool> LiteinstGuest<'_, T> {
    fn event(&self) -> &SyscallEvent {
        match &self.stop {
            GuestStop::Event(event) => event,
            GuestStop::Progress { .. } => fatal(126),
        }
    }

    fn event_mut(&mut self) -> &mut SyscallEvent {
        match &mut self.stop {
            GuestStop::Event(event) => event,
            GuestStop::Progress { .. } => fatal(126),
        }
    }

    /// Materialize the parent view while every synchronization owner still
    /// exists. A raw process fork can otherwise copy a locked Tool-state mutex
    /// into the child after its owning thread disappeared. Round-tripping via
    /// the existing ThreadState migration contract gives the child private,
    /// unlocked synchronization primitives without a backend-specific Tool API.
    fn prepare_fork_parent_state(&mut self) {
        let encoded = bincode::serde::encode_to_vec(&*self.state, bincode::config::standard())
            .unwrap_or_else(|_| fatal(126));
        let (snapshot, consumed) = bincode::serde::decode_from_slice::<T::ThreadState, _>(
            &encoded,
            bincode::config::standard(),
        )
        .unwrap_or_else(|_| fatal(126));
        if consumed != encoded.len() {
            fatal(126);
        }
        self.fork_parent_state = Some(snapshot);
    }

    fn take_fork_parent_state(&mut self) -> T::ThreadState {
        self.fork_parent_state.take().unwrap_or_else(|| fatal(126))
    }
}

#[reverie::tool]
impl<T: Tool> GlobalRPC<T::GlobalState> for LiteinstGuest<'_, T> {
    async fn send_rpc(
        &self,
        message: <T::GlobalState as GlobalTool>::Request,
    ) -> <T::GlobalState as GlobalTool>::Response {
        self.rpc.send_rpc(message).await
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        self.rpc.config()
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-326): Review the plain-fork injection boundary.
fn is_plain_fork(number: i64, args: [u64; 6]) -> bool {
    if crate::syscall_mode::sud_only() {
        return false;
    }
    if number == libc::SYS_fork {
        return true;
    }
    if number == libc::SYS_vfork {
        return true;
    }
    if number == libc::SYS_clone3 {
        return clone3_is_plain_fork(args[0], args[1]);
    }
    if number != libc::SYS_clone {
        return false;
    }
    const SIGNAL_MASK: u64 = 0xff;
    let allowed_flags =
        (libc::CLONE_CHILD_CLEARTID | libc::CLONE_CHILD_SETTID | libc::CLONE_PARENT_SETTID) as u64;
    args[1] == 0
        && args[0] & SIGNAL_MASK == libc::SIGCHLD as u64
        && args[0] & !(SIGNAL_MASK | allowed_flags) == 0
}

fn clone3_is_plain_fork(address: u64, size: u64) -> bool {
    const CLONE_ARGS_SIZE_VER0: usize = 64;
    const CLONE_ARGS_SIZE_VER2: usize = 88;
    if address == 0 || size < CLONE_ARGS_SIZE_VER0 as u64 {
        return false;
    }
    let mut fields = [0_u64; CLONE_ARGS_SIZE_VER2 / core::mem::size_of::<u64>()];
    let read_len = usize::try_from(size)
        .unwrap_or(usize::MAX)
        .min(core::mem::size_of_val(&fields));
    let local = libc::iovec {
        iov_base: fields.as_mut_ptr().cast(),
        iov_len: read_len,
    };
    let remote = libc::iovec {
        iov_base: address as usize as *mut libc::c_void,
        iov_len: read_len,
    };
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let read = unsafe {
        raw_syscall6(
            libc::SYS_process_vm_readv,
            [
                pid as u64,
                (&raw const local) as u64,
                1,
                (&raw const remote) as u64,
                1,
                0,
            ],
        )
    };
    if read != read_len as i64 {
        return false;
    }

    let allowed_flags =
        (libc::CLONE_CHILD_CLEARTID | libc::CLONE_CHILD_SETTID | libc::CLONE_PARENT_SETTID) as u64;
    let flags = fields[0];
    flags & !allowed_flags == 0
        && fields[4] == libc::SIGCHLD as u64
        && fields[5] == 0
        && fields[6] == 0
        && fields[8..].iter().all(|field| *field == 0)
}

fn forward_plain_fork(number: i64, args: [u64; 6]) -> i64 {
    let log_fork = crate::guest_log::prepare_fork();
    let result = if number == libc::SYS_vfork {
        // A real vfork child would run the instrumentation callback on the
        // parent's shared stack. Use a COW fork and preserve vfork's parent
        // suspension until the child exits. Exec remains fail-closed, so exit
        // is the only supported vfork completion boundary for now.
        unsafe { raw_syscall6(libc::SYS_fork, [0; 6]) }
    } else {
        unsafe { raw_syscall6(number, args) }
    };
    crate::guest_log::complete_fork(log_fork, result);
    if number == libc::SYS_vfork && result > 0 {
        let mut info = core::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        loop {
            let waited = unsafe {
                raw_syscall6(
                    libc::SYS_waitid,
                    [
                        libc::P_PID as u64,
                        result as u64,
                        info.as_mut_ptr() as u64,
                        (libc::WEXITED | libc::WNOWAIT) as u64,
                        0,
                        0,
                    ],
                )
            };
            if waited == -i64::from(libc::EINTR) {
                continue;
            }
            if waited < 0 {
                return waited;
            }
            break;
        }
    }
    result
}

// TODO-HUMAN-REVIEW(PR-127): Review injected process/signal safety policy.
fn injected_syscall_guard(number: i64, args: [u64; 6]) -> Option<i64> {
    if let Some(result) = runtime::protected_injected_syscall(number, args) {
        return Some(result);
    }
    let unsupported_process =
        // AUTONOMOUS-BOT-IMPLEMENTED
        (crate::syscall_mode::sud_only() && reverie_preload::dispatch::is_fork_like(number))
        || (matches!(number, libc::SYS_clone | libc::SYS_clone3 | libc::SYS_vfork)
            && !is_plain_fork(number, args))
        // AUTONOMOUS-BOT-IMPLEMENTED
        || matches!(number, libc::SYS_execve | libc::SYS_execveat);
    let protected_signal =
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-133): Review fail-closed guest signal-handler policy.
        !runtime::signal_action_supported(number, args)
        // AUTONOMOUS-BOT-IMPLEMENTED
        || (number == libc::SYS_sigaltstack && args[0] != 0)
        // AUTONOMOUS-BOT-IMPLEMENTED
        || (number == libc::SYS_rt_sigprocmask && args[1] != 0);

    if unsupported_process {
        Some(-i64::from(Errno::EOPNOTSUPP.into_raw()))
    } else if protected_signal {
        Some(-i64::from(Errno::EPERM.into_raw()))
    } else {
        None
    }
}

fn guarded_raw_injection(number: i64, args: [u64; 6]) -> i64 {
    #[cfg(feature = "private-crt")]
    if crate::owned_context::syscall_mode() && number == libc::SYS_arch_prctl {
        return crate::owned_context::tls::inject(args).unwrap_or_else(|cause| {
            use core::fmt::Write as _;
            let mut sink = TerminalDiagnostic::new();
            let rendered = write!(
                sink,
                "hermit-liteinst owned arch_prctl refused: cause={cause}"
            );
            if rendered.is_err() && !sink.exhausted() {
                sink.record_format_failure();
            }
            let _unreportable = sink.finish();
            unsafe { runtime::exit_now(UNSUPPORTED_OWNED_INJECTION_STATUS) }
        });
    }
    let result = if crate::owned_context::syscall_mode() && crate::mapping::operation(number) {
        crate::mapping::inject(number, args).unwrap_or_else(|error| {
            use core::fmt::Write as _;
            let mut sink = TerminalDiagnostic::new();
            let rendered = write!(
                sink,
                "hermit-liteinst owned mapping refused: number={number} cause={error}"
            );
            if rendered.is_err() && !sink.exhausted() {
                sink.record_format_failure();
            }
            let _unreportable = sink.finish();
            unsafe { runtime::exit_now(UNSUPPORTED_OWNED_INJECTION_STATUS) }
        })
    } else {
        injected_syscall_guard(number, args)
            .unwrap_or_else(|| runtime::guarded_scoped_syscall(number, args))
    };
    #[cfg(feature = "test-tool-host-dispatch")]
    dispatch_observer::record(dispatch_observer::Event::Inject { number, result });
    result
}

#[reverie::tool]
impl<T: Tool> Guest<T> for LiteinstGuest<'_, T> {
    type Memory = LocalMemory;
    type Stack = LocalStack;

    fn tid(&self) -> Pid {
        self.tid
    }

    fn pid(&self) -> Pid {
        self.pid
    }

    fn ppid(&self) -> Option<Pid> {
        self.ppid
    }

    fn memory(&self) -> Self::Memory {
        LocalMemory::new()
    }

    fn thread_state_mut(&mut self) -> &mut T::ThreadState {
        self.state
    }

    fn thread_state(&self) -> &T::ThreadState {
        self.state
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        let mut regs = unsafe { core::mem::zeroed::<libc::user_regs_struct>() };
        #[cfg(feature = "private-crt")]
        if crate::owned_context::tls::installed() {
            let bases = crate::owned_context::tls::bases().unwrap_or_else(|_| fatal(119));
            regs.fs_base = bases[0];
            regs.gs_base = bases[1];
        }
        let context = match &self.stop {
            GuestStop::Progress { context, .. } => Some(&**context),
            GuestStop::Event(event) if event.context != 0 => {
                Some(unsafe { &*(event.context as *const HookContext) })
            }
            _ => None,
        };
        if let Some(context) = context {
            regs.r15 = context.r15;
            regs.r14 = context.r14;
            regs.r13 = context.r13;
            regs.r12 = context.r12;
            regs.rbp = context.rbp;
            regs.rbx = context.rbx;
            regs.r11 = context.r11;
            regs.r10 = context.r10;
            regs.r9 = context.r9;
            regs.r8 = context.r8;
            regs.rax = context.rax;
            regs.rcx = context.rcx;
            regs.rdx = context.rdx;
            regs.rsi = context.rsi;
            regs.rdi = context.rdi;
            regs.orig_rax = context.rax;
            regs.rip = context.instruction_pointer;
            regs.rsp = context.stack_pointer;
            regs.eflags = context.rflags;
            return regs;
        }
        let event = self.event();
        regs.rax = event.number as u64;
        regs.orig_rax = event.number as u64;
        regs.rdi = event.args[0];
        regs.rsi = event.args[1];
        regs.rdx = event.args[2];
        regs.r10 = event.args[3];
        regs.r8 = event.args[4];
        regs.r9 = event.args[5];
        regs.rip = event.instruction_pointer;
        regs
    }
    async fn set_regs(&mut self, regs: libc::user_regs_struct) -> Result<(), Error> {
        match &mut self.stop {
            GuestStop::Event(event) => {
                set_guest_registers(event, regs, self.instruction_results_only)
            }
            GuestStop::Progress { context, .. } => {
                validate_guest_registers(&regs, self.instruction_results_only)?;
                set_context_registers(context, &regs);
                context.instruction_pointer = regs.rip;
                Ok(())
            }
        }
    }
    async fn stack(&mut self) -> Self::Stack {
        LocalStack::new(self.scratch.clone())
    }

    async fn daemonize(&mut self) {}

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno> {
        let (number, args) = syscall.into_parts();
        let number = number.id() as i64;
        let mut raw_args = [
            args.arg0 as u64,
            args.arg1 as u64,
            args.arg2 as u64,
            args.arg3 as u64,
            args.arg4 as u64,
            args.arg5 as u64,
        ];
        if classify_owned_injection_with_args(
            crate::owned_context::syscall_mode(),
            number,
            raw_args,
        ) == OwnedInjection::Terminal
        {
            // SAFETY: first statement after decoding the request. Nothing has
            // been forwarded, no register or guest memory has been touched for
            // it, so the operation is refused before any effect.
            unsafe { terminate_unsupported_owned_injection(number) };
        }
        if is_plain_fork(number, raw_args) {
            let parent_tid = self.tid;
            let parent_pid = self.pid;
            self.prepare_fork_parent_state();
            let result = forward_plain_fork(number, raw_args);
            if result == 0 {
                let child_tid = raw_pid(libc::SYS_gettid);
                let child_pid = raw_pid(libc::SYS_getpid);
                self.tail
                    .set_fork_child(parent_tid, parent_pid, child_tid, child_pid);
                return std::future::pending().await;
            }
            return Errno::from_ret(result as usize).map(|value| value as i64);
        }

        // AUTONOMOUS-BOT-IMPLEMENTED
        if matches!(number, libc::SYS_clone | libc::SYS_clone3 | libc::SYS_vfork) {
            const MESSAGE: &[u8] = b"reverie-liteinst: clone injection requires ptrace fallback\n";
            unsafe {
                let _ = raw_syscall6(
                    libc::SYS_write,
                    [
                        libc::STDERR_FILENO as u64,
                        MESSAGE.as_ptr() as u64,
                        MESSAGE.len() as u64,
                        0,
                        0,
                        0,
                    ],
                );
            }
            return Err(Errno::EOPNOTSUPP);
        }
        if !(crate::owned_context::syscall_mode() && crate::mapping::operation(number))
            && let Some(result) = injected_syscall_guard(number, raw_args)
        {
            return Errno::from_ret(result as usize).map(|value| value as i64);
        }
        if is_exit_syscall(number) {
            self.tail.set_exit(number, raw_args);
            return std::future::pending().await;
        }
        let kernel_signal_mask =
            (number == libc::SYS_rt_sigprocmask && raw_args[1] != 0).then(|| {
                let requested = unsafe { (raw_args[1] as *const u64).read_unaligned() };
                let mut reserved = 1_u64 << (libc::SIGSYS - 1);
                if runtime::cpuid_interception_enabled() || runtime::rdtsc_interception_enabled() {
                    reserved |= 1_u64 << (libc::SIGSEGV - 1);
                }
                requested & !reserved
            });
        if let Some(mask) = kernel_signal_mask.as_ref() {
            raw_args[1] = mask as *const u64 as u64;
        }

        let result = guarded_raw_injection(number, raw_args);
        Errno::from_ret(result as usize).map(|value| value as i64)
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        if matches!(self.stop, GuestStop::Progress { .. }) {
            tool_fatal(
                126,
                &io::Error::other("tail injection has no guest-progress continuation").into(),
            );
        }
        let (number, syscall_args) = syscall.into_parts();
        let args = [
            syscall_args.arg0 as u64,
            syscall_args.arg1 as u64,
            syscall_args.arg2 as u64,
            syscall_args.arg3 as u64,
            syscall_args.arg4 as u64,
            syscall_args.arg5 as u64,
        ];
        let number = number.id() as i64;
        if crate::owned_context::syscall_mode() && crate::owned_context::exit::operation(number) {
            if self.event().owned_binding.is_none() {
                crate::owned_context::exit::failed("owned-exit/missing-binding", None);
            }
            self.tail.set_exit(number, args);
            return std::future::pending().await;
        }
        if classify_owned_injection_with_args(crate::owned_context::syscall_mode(), number, args)
            == OwnedInjection::Terminal
        {
            // SAFETY: reached before `is_plain_fork`, the injection guard and
            // any forwarding, and before the tail result is set, so the
            // operation is refused before any effect. Unlike the previous
            // `set_result` arm this does not hand the driver a fabricated
            // errno to deliver to the guest.
            unsafe { terminate_unsupported_owned_injection(number) };
        }
        if is_plain_fork(number, args) {
            let parent_tid = self.tid;
            let parent_pid = self.pid;
            self.prepare_fork_parent_state();
            let result = forward_plain_fork(number, args);
            if result == 0 {
                self.tail.set_fork_child(
                    parent_tid,
                    parent_pid,
                    raw_pid(libc::SYS_gettid),
                    raw_pid(libc::SYS_getpid),
                );
            } else {
                self.tail.set_result(result);
            }
        } else if crate::owned_context::syscall_mode() && crate::mapping::operation(number) {
            self.tail.set_result(guarded_raw_injection(number, args));
        } else if let Some(result) = injected_syscall_guard(number, args) {
            self.tail.set_result(result);
        } else if is_exit_syscall(number) {
            self.tail.set_exit(number, args);
        } else {
            let value = guarded_raw_injection(number, args);
            self.tail.set_result(value);
        }
        std::future::pending().await
    }

    // TODO-HUMAN-REVIEW(PR-326): Review delivery before enabling timer success.
    fn set_timer(&mut self, sched: TimerSchedule) -> Result<(), Error> {
        if matches!(self.stop, GuestStop::Progress { .. }) || crate::syscall_mode::sud_only() {
            return crate::timer::request_owned(sched, false);
        }
        crate::timer::request(sched, false)
    }

    fn set_timer_precise(&mut self, sched: TimerSchedule) -> Result<(), Error> {
        if matches!(self.stop, GuestStop::Progress { .. }) || crate::syscall_mode::sud_only() {
            return crate::timer::request_owned(sched, true);
        }
        crate::timer::request(sched, true)
    }

    fn read_clock(&mut self) -> Result<u64, Error> {
        match &self.stop {
            GuestStop::Progress { clock, .. } => Ok(*clock),
            GuestStop::Event(_) => runtime::read_guest_rcb_clock().map_err(Error::from),
        }
    }

    fn has_cpuid_interception(&self) -> bool {
        self.cpuid_interception
    }
}

pub struct LocalStack {
    owner: ScratchOwner,
    arena: Box<[u8]>,
    offset: usize,
}

impl LocalStack {
    fn new(owner: ScratchOwner) -> Self {
        Self {
            owner,
            arena: vec![0; STACK_CAPACITY].into_boxed_slice(),
            offset: 0,
        }
    }

    fn allocate<'stack, V>(&mut self, value: V) -> AddrMut<'stack, V> {
        let align = core::mem::align_of::<V>();
        let base = self.arena.as_ptr() as usize;
        let start = (base + self.offset + align - 1) & !(align - 1);
        let offset = start - base;
        let end = offset + core::mem::size_of::<V>();
        assert!(end <= self.arena.len(), "LiteInst guest stack overflow");
        let pointer = unsafe { self.arena.as_mut_ptr().add(offset).cast::<V>() };
        unsafe { pointer.write(value) };
        self.offset = end;
        AddrMut::from_raw(pointer as usize).expect("LiteInst stack produced a null address")
    }
}

pub struct LocalStackGuard {
    owner: ScratchOwner,
    arena: Option<Box<[u8]>>,
}

impl Drop for LocalStackGuard {
    fn drop(&mut self) {
        let _runtime = crate::runtime_domain::Entry::enter();
        if let Some(arena) = self.arena.take() {
            #[cfg(test)]
            crate::runtime_domain::tests::at(crate::runtime_domain::tests::STACK_COMMIT);
            #[cfg(feature = "test-tool-host-dispatch")]
            dispatch_observer::record(dispatch_observer::Event::StackCommit { bytes: arena.len() });
            self.owner.retain(arena);
        }
    }
}

impl Stack for LocalStack {
    type StackGuard = LocalStackGuard;

    fn size(&self) -> usize {
        self.offset
    }

    fn capacity(&self) -> usize {
        self.arena.len()
    }

    fn push<'stack, V>(&mut self, value: V) -> Addr<'stack, V> {
        self.allocate(value).into()
    }

    fn reserve<'stack, V>(&mut self) -> AddrMut<'stack, V> {
        let value = unsafe { core::mem::MaybeUninit::zeroed().assume_init() };
        self.allocate(value)
    }

    fn commit(self) -> Result<Self::StackGuard, Errno> {
        Ok(LocalStackGuard {
            owner: self.owner,
            arena: Some(self.arena),
        })
    }
}

fn tool_fatal(status: i32, error: &Error) -> ! {
    let message = format!("reverie-liteinst tool error: {error:?}\n");
    let _ = raw_stderr_write(message.as_bytes());
    fatal(status)
}

#[track_caller]
fn fatal(status: i32) -> ! {
    if status == 126 {
        unsafe { reverie_preload::trap::terminal126("tool-host", "predicate", None) }
    }
    unsafe {
        let _ = raw_syscall6(libc::SYS_exit_group, [status as u64, 0, 0, 0, 0, 0]);
    }
    loop {
        core::hint::spin_loop();
    }
}

fn set_guest_registers(
    event: &mut SyscallEvent,
    regs: libc::user_regs_struct,
    instruction_results_only: bool,
) -> Result<(), Error> {
    validate_guest_registers(&regs, instruction_results_only)?;
    event.number = regs.rax as i64;
    event.args = [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9];
    if event.context != 0 {
        let context = unsafe { &mut *(event.context as *mut HookContext) };
        set_context_registers(context, &regs);
    }
    Ok(())
}

fn validate_guest_registers(
    regs: &libc::user_regs_struct,
    instruction_results_only: bool,
) -> Result<(), Error> {
    if instruction_results_only {
        return Err(Errno::EOPNOTSUPP.into());
    }
    #[cfg(feature = "private-crt")]
    if crate::owned_context::tls::installed() {
        let bases =
            crate::owned_context::tls::bases().map_err(|_| Error::from(Errno::EOPNOTSUPP))?;
        if [regs.fs_base, regs.gs_base] != bases || regs.fs != 0 || regs.gs != 0 {
            return Err(Errno::EOPNOTSUPP.into());
        }
    }
    Ok(())
}

fn set_context_registers(context: &mut HookContext, regs: &libc::user_regs_struct) {
    context.r15 = regs.r15;
    context.r14 = regs.r14;
    context.r13 = regs.r13;
    context.r12 = regs.r12;
    context.rbp = regs.rbp;
    context.rbx = regs.rbx;
    context.r11 = regs.r11;
    context.r10 = regs.r10;
    context.r9 = regs.r9;
    context.r8 = regs.r8;
    context.rax = regs.rax;
    context.rcx = regs.rcx;
    context.rdx = regs.rdx;
    context.rsi = regs.rsi;
    context.rdi = regs.rdi;
    context.stack_pointer = regs.rsp;
    context.rflags = regs.eflags;
}

/// What an owned Tool injection may do, decided before the operation can have
/// any effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnedInjection {
    /// Continue to the existing guarded raw route, which still applies the
    /// protected-descriptor, lifecycle and signal-ownership refusals.
    Admitted,
    /// Owned, and outside [`crate::syscall_event::injectable`]. The backend
    /// cannot perform the operation, so it must stop rather than answer.
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObservationRefusal {
    UnknownNumber,
    Unsubscribed,
    MissingHandler,
}

fn validate_owned_observation(
    owned: bool,
    number: i64,
    subscriptions: &HashSet<Sysno>,
) -> Result<(), ObservationRefusal> {
    if !owned {
        return Ok(());
    }
    let number = usize::try_from(number)
        .ok()
        .and_then(Sysno::new)
        .ok_or(ObservationRefusal::UnknownNumber)?;
    if !subscriptions.contains(&number) {
        return Err(ObservationRefusal::Unsubscribed);
    }
    Ok(())
}

fn terminate_owned_observation(number: i64, reason: ObservationRefusal) -> ! {
    use core::fmt::Write as _;
    let mut sink = TerminalDiagnostic::new();
    if write!(sink, "hermit-liteinst owned observation refused: stage=owned-observation cause={reason:?} number={number}").is_err()
        && !sink.exhausted()
    {
        sink.record_format_failure();
    }
    let _unreportable = sink.finish();
    unsafe { runtime::exit_now(UNSUPPORTED_OWNED_INJECTION_STATUS) }
}

/// Classifies an owned injection. Pure, so it is directly testable; the effect
/// is at the two call sites.
///
/// A non-owned injection is always [`OwnedInjection::Admitted`], exactly as
/// before — this decision is about the owned route only.
///
/// The previous form of this gate returned `EOPNOTSUPP` for the owned case.
/// That was wrong in kind, not merely in value: `EOPNOTSUPP` is a Linux answer,
/// and returning it tells the Tool that the kernel declined an operation the
/// kernel never saw. A Tool that determinizes errno then records a backend
/// limitation as guest-visible behaviour, and the resulting trace is
/// self-consistent and wrong. `ENOSYS` would be the same mistake with a
/// different number. There is no correct errno here, because the honest
/// statement is not about the guest at all, so the outcome is terminal instead.
fn classify_owned_injection(owned: bool, number: i64) -> OwnedInjection {
    if owned
        && !crate::syscall_event::injectable(number)
        && !(crate::mapping::operation(number) && crate::mapping::ready())
        && !(number == libc::SYS_arch_prctl && crate::owned_context::tls_mode())
    {
        OwnedInjection::Terminal
    } else {
        OwnedInjection::Admitted
    }
}

fn classify_owned_injection_with_args(owned: bool, number: i64, args: [u64; 6]) -> OwnedInjection {
    if owned && number == libc::SYS_fcntl {
        if matches!(args[1] as i32, libc::F_GETPIPE_SZ | libc::F_SETPIPE_SZ) {
            OwnedInjection::Admitted
        } else {
            OwnedInjection::Terminal
        }
    } else {
        classify_owned_injection(owned, number)
    }
}

/// Terminal status for an owned injection outside the injected-operation
/// closure.
///
/// Distinct from every status already in use — 120, 121 and 123 are the event
/// channel and in-guest stage write failures, 122 and 125 through 127 are the
/// runtime, owned-context and setup terminals — so this outcome is attributable
/// from the exit status alone, without parsing the diagnostic.
const UNSUPPORTED_OWNED_INJECTION_STATUS: i32 = 119;

/// Reports the retained stage and cause for an unsupported owned injection.
///
/// Shares [`TerminalDiagnostic`], so it allocates nothing and costs at most
/// [`DIAGNOSTIC_WRITE_ATTEMPTS`] writes. Returns the sink failure, if any, for
/// the caller to discard deliberately: the process is ending either way, and a
/// stderr that cannot be written to must not change the exit status.
fn report_unsupported_owned_injection(number: i64) -> Option<SinkFailure> {
    use core::fmt::Write as _;
    let mut sink = TerminalDiagnostic::new();
    let rendered = write!(
        sink,
        "hermit-liteinst owned injection refused: stage={} cause=unsupported-syscall number={number}",
        SetupStage::OwnedInjection.name()
    );
    if rendered.is_err() && !sink.exhausted() {
        sink.record_format_failure();
    }
    sink.finish()
}

/// Ends the process for an owned injection the backend cannot perform.
///
/// # Safety
/// Only valid before the operation has been attempted. Every caller classifies
/// first and reaches this ahead of `is_plain_fork`, [`injected_syscall_guard`]
/// and any raw forwarding, so no kernel effect and no guest-visible register or
/// memory change has occurred for the refused operation.
unsafe fn terminate_unsupported_owned_injection(number: i64) -> ! {
    let _unreportable = report_unsupported_owned_injection(number);
    unsafe { runtime::exit_now(UNSUPPORTED_OWNED_INJECTION_STATUS) }
}

/// Total diagnostic payload the terminal reporter may emit, excluding its own
/// truncation suffix and newline.
const DIAGNOSTIC_BYTE_BUDGET: usize = 512;

/// Maximum `write` syscalls one diagnostic drain may issue, counting `EINTR`
/// resumptions and short writes. Bounds work and attempts; it is not a
/// wall-clock bound, because a single blocking `write` cannot be bounded here.
const DIAGNOSTIC_WRITE_ATTEMPTS: u32 = 64;

/// Maximum cause-chain levels rendered before the reporter records that the
/// chain was deeper.
const DIAGNOSTIC_CAUSE_DEPTH: usize = 3;

/// Why a diagnostic could not be delivered. Internal to the terminal path: it
/// never changes the exit status and is never itself reported to a sink.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SinkFailure {
    Errno(i32),
    AttemptsExhausted,
}

/// Writes every byte to standard error through the trusted raw syscall gate,
/// within [`DIAGNOSTIC_WRITE_ATTEMPTS`]. Reports the first blocking failure
/// instead of success, and never changes a status or a signal disposition.
fn raw_stderr_write(bytes: &[u8]) -> Result<(), SinkFailure> {
    drain_with(bytes, DIAGNOSTIC_WRITE_ATTEMPTS, |chunk| unsafe {
        raw_syscall6(
            libc::SYS_write,
            [
                libc::STDERR_FILENO as u64,
                chunk.as_ptr() as u64,
                chunk.len() as u64,
                0,
                0,
                0,
            ],
        )
    })
}

/// The bounded retry rule shared by every raw diagnostic write: resume on
/// `EINTR`, resume after a short write, treat a zero-length write as `EIO`, and
/// give up once `attempts` is exhausted. Each syscall consumes one attempt, so
/// repeated `EINTR` terminates instead of looping. Separated so a pure host test
/// can drive it; production always drives it with the real `write` syscall.
fn drain_with(
    mut bytes: &[u8],
    mut attempts: u32,
    mut write: impl FnMut(&[u8]) -> i64,
) -> Result<(), SinkFailure> {
    while !bytes.is_empty() {
        if attempts == 0 {
            return Err(SinkFailure::AttemptsExhausted);
        }
        attempts -= 1;
        let written = write(bytes);
        if written < 0 {
            let errno = (-written) as i32;
            if errno == libc::EINTR {
                continue;
            }
            return Err(SinkFailure::Errno(errno));
        }
        if written == 0 {
            return Err(SinkFailure::Errno(libc::EIO));
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
}

/// The committed step a terminal failure came from. Stable text, chosen at the
/// call site rather than parsed from a message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SetupStage {
    OwnedContext,
    SyscallCapture,
    RcbClock,
    PreciseController,
    Timer,
    InstructionPreflight,
    VdsoRewrite,
    GuestSignalState,
    ToolHandler,
    ReverieTool,
    /// Not a setup step: an owned Tool injection refused before its effect.
    /// It shares this table so every terminal outcome names its stage from one
    /// stable list.
    OwnedInjection,
}

impl SetupStage {
    fn name(self) -> &'static str {
        match self {
            Self::OwnedContext => "owned-context",
            Self::SyscallCapture => "syscall-capture",
            Self::RcbClock => "rcb-clock",
            Self::PreciseController => "precise-controller",
            Self::Timer => "timer",
            Self::InstructionPreflight => "instruction-preflight",
            Self::VdsoRewrite => "vdso-rewrite",
            Self::GuestSignalState => "guest-signal-state",
            Self::ToolHandler => "tool-handler",
            Self::ReverieTool => "reverie-tool",
            Self::OwnedInjection => "owned-injection",
        }
    }
}

/// Accumulates a bounded terminal diagnostic and delivers it with one drain.
///
/// It allocates nothing. Everything is staged in one fixed buffer, so the whole
/// report costs at most [`DIAGNOSTIC_WRITE_ATTEMPTS`] `write` syscalls in total —
/// an aggregate bound, not a per-flush one. Payload is capped at
/// [`DIAGNOSTIC_BYTE_BUDGET`]; reaching the cap sets `truncated`, reported as an
/// explicit `truncated=1` field, and makes `write_str` fail so `write!` stops
/// formatting the remainder immediately. A formatter that itself fails is
/// recorded as `format-failed=1` rather than ignored. No claim is made that every
/// reachable message fits the cap: an oversize cause is reported as truncated.
struct TerminalDiagnostic {
    buffer: [u8; 576],
    len: usize,
    remaining: usize,
    truncated: bool,
    format_failed: bool,
}

impl TerminalDiagnostic {
    fn new() -> Self {
        Self {
            buffer: [0; 576],
            len: 0,
            remaining: DIAGNOSTIC_BYTE_BUDGET,
            truncated: false,
            format_failed: false,
        }
    }

    /// Whether further rendering must stop. Checked before each cause node so no
    /// arbitrary `Display` or `source` is invoked once the report is unusable.
    fn exhausted(&self) -> bool {
        self.truncated || self.format_failed
    }

    fn push(&mut self, bytes: &[u8]) {
        let room = self.buffer.len() - self.len;
        let count = bytes.len().min(room);
        self.buffer[self.len..self.len + count].copy_from_slice(&bytes[..count]);
        self.len += count;
    }

    fn record_format_failure(&mut self) {
        self.format_failed = true;
    }

    fn finish(mut self) -> Option<SinkFailure> {
        if self.truncated {
            self.push(b" truncated=1");
        }
        if self.format_failed {
            self.push(b" format-failed=1");
        }
        self.push(b"\n");
        let len = self.len;
        raw_stderr_write(&self.buffer[..len]).err()
    }
}

impl core::fmt::Write for TerminalDiagnostic {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        if self.exhausted() {
            return Err(core::fmt::Error);
        }
        let bytes = text.as_bytes();
        let accepted = bytes.len().min(self.remaining);
        self.push(&bytes[..accepted]);
        self.remaining -= accepted;
        if accepted != bytes.len() {
            self.truncated = true;
            return Err(core::fmt::Error);
        }
        Ok(())
    }
}

/// A position in the cause chain. An `io::Error` is followed through its own
/// `get_ref` payload, because `io::Error::source` skips it; anything else is
/// followed through `source`.
enum CauseNode<'a> {
    Io(&'a io::Error),
    Other(&'a (dyn std::error::Error + 'static)),
}

/// Renders the whole cause chain without allocating, under one depth bound.
///
/// Every node counts against [`DIAGNOSTIC_CAUSE_DEPTH`], including nested
/// wrappers. An `io::Error` node is emitted as its exact wrapper kind and raw
/// errno and is **never** passed to `Display`, because an operating-system
/// message allocates. A `reverie::Errno` is emitted numerically. Any other
/// payload is rendered by `Display` into the bounded buffer. Traversal stops as
/// soon as the buffer truncates or a formatter fails, so no further `Display` or
/// `source` is invoked once the report is unusable.
fn render_cause_chain(sink: &mut TerminalDiagnostic, error: &io::Error) {
    use core::fmt::Write as _;
    let mut node = CauseNode::Io(error);
    let mut depth = 0;
    loop {
        if sink.exhausted() {
            return;
        }
        let next = match node {
            CauseNode::Io(current) => current.get_ref().map(|inner| {
                let inner: &(dyn std::error::Error + 'static) = inner;
                inner
            }),
            CauseNode::Other(current) => current.source(),
        };
        let Some(cause) = next else {
            return;
        };
        if depth == DIAGNOSTIC_CAUSE_DEPTH {
            let _ = sink.write_str(" cause-depth-exceeded=1");
            return;
        }
        if write!(sink, " cause{depth}=").is_err() {
            return;
        }
        if let Some(nested) = cause.downcast_ref::<io::Error>() {
            let rendered = write!(sink, "io kind={:?} errno=", nested.kind()).and_then(|()| {
                match nested.raw_os_error() {
                    Some(errno) => write!(sink, "{errno}"),
                    None => sink.write_str("none"),
                }
            });
            if rendered.is_err() {
                return;
            }
            node = CauseNode::Io(nested);
        } else if let Some(errno) = cause.downcast_ref::<Errno>() {
            if write!(sink, "errno {}", errno.into_raw()).is_err() {
                if !sink.exhausted() {
                    sink.record_format_failure();
                }
                return;
            }
            node = CauseNode::Other(cause);
        } else {
            if write!(sink, "{cause}").is_err() {
                if !sink.exhausted() {
                    sink.record_format_failure();
                }
                return;
            }
            node = CauseNode::Other(cause);
        }
        depth += 1;
    }
}

fn report_terminal_setup_failure(stage: SetupStage, error: &io::Error) -> Option<SinkFailure> {
    use core::fmt::Write as _;
    let mut sink = TerminalDiagnostic::new();
    let header = write!(
        sink,
        "hermit-liteinst owned setup failed: stage={} kind={:?} errno=",
        stage.name(),
        error.kind()
    );
    if header.is_ok() {
        let rendered = match error.raw_os_error() {
            Some(errno) => write!(sink, "{errno}"),
            None => sink.write_str("none"),
        };
        if rendered.is_ok() {
            render_cause_chain(&mut sink, error);
        }
    }
    sink.finish()
}

#[cfg(test)]
mod robust_list_tests;

#[cfg(test)]
#[path = "tool_host_file_io_tests.rs"]
mod file_io_tests;

#[cfg(test)]
pub(crate) mod tests {
    use std::os::fd::AsRawFd;

    use super::*;

    #[test]
    fn owned_guard_advice_uses_mapping_not_ordinary_injection() {
        const CHILD: &str = "REVERIE_GUARD_ROUTE_MODEL";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tool_host::tests::owned_guard_advice_uses_mapping_not_ordinary_injection",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = std::str::from_utf8(&output.stdout).unwrap();
            assert!(stdout.lines().any(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")), "{stdout}");
            return;
        }
        assert!(crate::mapping::operation(libc::SYS_madvise));
        assert!(!crate::syscall_event::injectable(libc::SYS_madvise));
        assert!(!crate::mapping::operation(libc::SYS_msync));
        assert!(!crate::syscall_event::injectable(libc::SYS_msync));
        assert_eq!(
            classify_owned_injection(false, libc::SYS_madvise),
            OwnedInjection::Admitted
        );
        crate::mapping::with_guard_route_model(|ready| {
            assert_eq!(crate::mapping::ready(), ready);
            assert_eq!(
                classify_owned_injection(true, libc::SYS_madvise),
                if ready {
                    OwnedInjection::Admitted
                } else {
                    OwnedInjection::Terminal
                }
            );
            assert_eq!(
                classify_owned_injection(true, libc::SYS_msync),
                OwnedInjection::Terminal
            );
        });
    }

    mod fixture_routes {
        use std::ffi::CString;
        use std::os::fd::FromRawFd;
        use std::os::fd::OwnedFd;
        use std::os::unix::fs::MetadataExt;
        use std::sync::atomic::Ordering;

        use super::*;

        fn inject(number: i64, args: [u64; 6]) -> i64 {
            assert_eq!(
                classify_owned_injection_with_args(true, number, args),
                OwnedInjection::Admitted
            );
            guarded_raw_injection(number, args)
        }

        fn descriptor(number: i64, args: [u64; 6]) -> OwnedFd {
            let result = inject(number, args);
            assert!(result >= 0, "{number}: {result}");
            unsafe { OwnedFd::from_raw_fd(result as i32) }
        }

        fn error_matches_kernel(number: i64, args: [u64; 6]) {
            let expected = unsafe { raw_syscall6(number, args) };
            assert!(expected < 0, "control must fail: {number} {args:x?}");
            assert_eq!(inject(number, args), expected, "{number} {args:x?}");
        }

        fn kernel_flags(fd: u64, command: i32) -> i64 {
            let result = unsafe { raw_syscall6(libc::SYS_fcntl, [fd, command as u64, 0, 0, 0, 0]) };
            assert!(result >= 0);
            result
        }

        #[test]
        fn fcntl_commands_are_finite_and_preserve_native_width() {
            for command in [libc::F_GETPIPE_SZ, libc::F_SETPIPE_SZ] {
                for raw in [command as u64, (9_u64 << 32) | command as u64] {
                    let args = [u64::MAX, raw, 0, 0, 0, 0];
                    assert_eq!(
                        classify_owned_injection(true, libc::SYS_fcntl),
                        OwnedInjection::Terminal
                    );
                    error_matches_kernel(libc::SYS_fcntl, args);
                }
            }
            for command in [
                libc::F_GETFD,
                libc::F_GETFL,
                libc::F_SETFL,
                libc::F_SETOWN,
                reverie::syscalls::FcntlCmd::F_SETSIG(0).into_raw().0,
                libc::F_SETLEASE,
                libc::F_SETLK,
                libc::F_DUPFD,
                libc::F_SETFD,
                -1,
            ] {
                let args = [0, command as u64, 0, 0, 0, 0];
                assert_eq!(
                    classify_owned_injection_with_args(true, libc::SYS_fcntl, args),
                    OwnedInjection::Terminal
                );
                assert_eq!(
                    classify_owned_injection_with_args(false, libc::SYS_fcntl, args),
                    OwnedInjection::Admitted
                );
            }
        }

        #[test]
        fn inotify_creation_watch_flags_and_errors_are_kernel_effects() {
            let flags = (libc::IN_NONBLOCK | libc::IN_CLOEXEC) as u64;
            let handle = descriptor(libc::SYS_inotify_init1, [flags, 0, 0, 0, 0, 0]);
            let fd = handle.as_raw_fd() as u64;
            assert_ne!(
                kernel_flags(fd, libc::F_GETFD) & i64::from(libc::FD_CLOEXEC),
                0
            );
            assert_ne!(
                kernel_flags(fd, libc::F_GETFL) & i64::from(libc::O_NONBLOCK),
                0
            );
            let directory = tempfile::tempdir().unwrap();
            let path = CString::new(directory.path().as_os_str().as_encoded_bytes()).unwrap();
            let watch_args = [fd, path.as_ptr() as u64, libc::IN_MODIFY as u64, 0, 0, 0];
            let watch_result = inject(libc::SYS_inotify_add_watch, watch_args);
            assert!(
                watch_result >= 0,
                "inotify_add_watch result={watch_result} fd={fd} post_failure_guard={:?} log_fd={} clock_fd={} notification_fd={} signal_fds={:?}",
                runtime::protected_injected_syscall(libc::SYS_inotify_add_watch, watch_args),
                crate::guest_log::LOG_FD.load(Ordering::Acquire),
                crate::clock_control::descriptor(),
                crate::clock_control::notification_descriptor(),
                reverie_preload::signal::runtime_signal_descriptors(),
            );
            error_matches_kernel(libc::SYS_inotify_init1, [u64::MAX, 0, 0, 0, 0, 0]);
            error_matches_kernel(
                libc::SYS_inotify_add_watch,
                [u64::MAX, 1, libc::IN_MODIFY as u64, 0, 0, 0],
            );
            error_matches_kernel(
                libc::SYS_inotify_add_watch,
                [fd, 1, libc::IN_MODIFY as u64, 0, 0, 0],
            );
        }

        #[test]
        fn pipe2_kernel_precedence_creation_and_capacity_controls() {
            for pointer in [0, 1] {
                for flags in [0, u64::MAX, libc::O_APPEND as u64] {
                    error_matches_kernel(libc::SYS_pipe2, [pointer, flags, 0, 0, 0, 0]);
                }
            }
            let mut output = [-77_i32; 2];
            let invalid = [
                output.as_mut_ptr() as u64,
                libc::O_APPEND as u64,
                0,
                0,
                0,
                0,
            ];
            let expected = unsafe { raw_syscall6(libc::SYS_pipe2, invalid) };
            let expected_output = output;
            output = [-77; 2];
            assert_eq!(
                inject(
                    libc::SYS_pipe2,
                    [output.as_mut_ptr() as u64, invalid[1], 0, 0, 0, 0]
                ),
                expected
            );
            assert_eq!(output, expected_output);
            assert_eq!(
                inject(
                    libc::SYS_pipe2,
                    [
                        output.as_mut_ptr() as u64,
                        (libc::O_CLOEXEC | libc::O_NONBLOCK) as u64,
                        0,
                        0,
                        0,
                        0
                    ]
                ),
                0
            );
            let read_fd = unsafe { OwnedFd::from_raw_fd(output[0]) };
            let write_fd = unsafe { OwnedFd::from_raw_fd(output[1]) };
            assert_ne!(read_fd.as_raw_fd(), write_fd.as_raw_fd());
            for fd in [read_fd.as_raw_fd() as u64, write_fd.as_raw_fd() as u64] {
                assert_ne!(
                    kernel_flags(fd, libc::F_GETFD) & i64::from(libc::FD_CLOEXEC),
                    0
                );
                assert_ne!(
                    kernel_flags(fd, libc::F_GETFL) & i64::from(libc::O_NONBLOCK),
                    0
                );
            }
            let fd = read_fd.as_raw_fd() as u64;
            assert_eq!(
                inject(
                    libc::SYS_fcntl,
                    [fd, libc::F_SETPIPE_SZ as u64, 4096, 0, 0, 0]
                ),
                4096
            );
            assert_eq!(
                inject(libc::SYS_fcntl, [fd, libc::F_GETPIPE_SZ as u64, 0, 0, 0, 0]),
                4096
            );
            assert_eq!(
                inject(
                    libc::SYS_write,
                    [
                        write_fd.as_raw_fd() as u64,
                        c"x".as_ptr() as u64,
                        1,
                        0,
                        0,
                        0
                    ]
                ),
                1
            );
            let mut byte = 0_u8;
            assert_eq!(
                inject(libc::SYS_read, [fd, (&raw mut byte) as u64, 1, 0, 0, 0]),
                1
            );
            assert_eq!(byte, b'x');
        }

        #[test]
        fn cwd_statfs_and_directory_output_preserve_kernel_results() {
            let mut cwd = [0_u8; 4096];
            let length = inject(
                libc::SYS_getcwd,
                [cwd.as_mut_ptr() as u64, cwd.len() as u64, 0, 0, 0, 0],
            );
            assert!(length > 0);
            assert_eq!(
                &cwd[..length as usize - 1],
                std::env::current_dir()
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
            );
            assert_eq!(cwd[length as usize - 1], 0);
            for args in [[1, 4096, 0, 0, 0, 0], [0, 0, 0, 0, 0, 0]] {
                error_matches_kernel(libc::SYS_getcwd, args);
            }
            let mut expected = [0xa5_u8; 1];
            let mut observed = expected;
            let result = unsafe {
                raw_syscall6(
                    libc::SYS_getcwd,
                    [expected.as_mut_ptr() as u64, 1, 0, 0, 0, 0],
                )
            };
            assert_eq!(
                inject(
                    libc::SYS_getcwd,
                    [observed.as_mut_ptr() as u64, 1, 0, 0, 0, 0]
                ),
                result
            );
            assert_eq!(observed, expected);
            let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
            assert_eq!(
                inject(
                    libc::SYS_statfs,
                    [c".".as_ptr() as u64, stat.as_mut_ptr() as u64, 0, 0, 0, 0]
                ),
                0
            );
            assert_ne!(unsafe { stat.assume_init() }.f_bsize, 0);
            error_matches_kernel(libc::SYS_statfs, [1, 1, 0, 0, 0, 0]);
            error_matches_kernel(libc::SYS_statfs, [c".".as_ptr() as u64, 1, 0, 0, 0, 0]);
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("entry"), b"data").unwrap();
            let handle = std::fs::File::open(directory.path()).unwrap();
            let fd = handle.as_raw_fd() as u64;
            let mut expected = [0_u8; 4096];
            let mut observed = expected;
            let count = unsafe {
                raw_syscall6(
                    libc::SYS_getdents64,
                    [fd, expected.as_mut_ptr() as u64, 4096, 0, 0, 0],
                )
            };
            assert!(count > 0);
            let current = [fd, 0, libc::SEEK_CUR as u64, 0, 0, 0];
            let end = unsafe { raw_syscall6(libc::SYS_lseek, current) };
            assert!(end >= 0);
            assert_eq!(
                inject(libc::SYS_lseek, [fd, 0, libc::SEEK_SET as u64, 0, 0, 0]),
                0
            );
            assert_eq!(
                inject(
                    libc::SYS_getdents64,
                    [fd, observed.as_mut_ptr() as u64, 4096, 0, 0, 0]
                ),
                count
            );
            assert_eq!(observed, expected);
            assert_eq!(inject(libc::SYS_lseek, current), end);
            error_matches_kernel(libc::SYS_getdents64, [u64::MAX, 1, 4096, 0, 0, 0]);
            let zero_capacity = [fd, 1, 0, 0, 0, 0];
            assert_eq!(
                unsafe { raw_syscall6(libc::SYS_getdents64, zero_capacity) },
                0
            );
            let eof_position = unsafe { raw_syscall6(libc::SYS_lseek, current) };
            assert!(eof_position >= 0);
            assert_eq!(
                inject(
                    libc::SYS_lseek,
                    [fd, end as u64, libc::SEEK_SET as u64, 0, 0, 0]
                ),
                end
            );
            assert_eq!(inject(libc::SYS_getdents64, zero_capacity), 0);
            assert_eq!(inject(libc::SYS_lseek, current), eof_position);
            let rewind = [fd, 0, libc::SEEK_SET as u64, 0, 0, 0];
            assert_eq!(unsafe { raw_syscall6(libc::SYS_lseek, rewind) }, 0);
            let result = unsafe { raw_syscall6(libc::SYS_getdents64, zero_capacity) };
            assert!(result < 0);
            let position = unsafe { raw_syscall6(libc::SYS_lseek, current) };
            assert!(position >= 0);
            assert_eq!(inject(libc::SYS_lseek, rewind), 0);
            assert_eq!(inject(libc::SYS_getdents64, zero_capacity), result);
            assert_eq!(inject(libc::SYS_lseek, current), position);
        }

        #[test]
        fn socket_local_ipc_descriptor_and_buffer_effects() {
            let kind = (libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK) as u64;
            let receiver = descriptor(libc::SYS_socket, [libc::AF_UNIX as u64, kind, 0, 0, 0, 0]);
            let sender = descriptor(libc::SYS_socket, [libc::AF_UNIX as u64, kind, 0, 0, 0, 0]);
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("datagram");
            let bytes = path.as_os_str().as_encoded_bytes();
            let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
            address.sun_family = libc::AF_UNIX as libc::sa_family_t;
            assert!(bytes.len() < address.sun_path.len());
            for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
                *target = *byte as libc::c_char;
            }
            let length =
                (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as u64;
            let pointer = (&raw const address) as u64;
            let receiver_fd = receiver.as_raw_fd() as u64;
            assert_eq!(
                inject(libc::SYS_bind, [receiver_fd, pointer, length, 0, 0, 0]),
                0
            );
            let mut named: libc::sockaddr_un = unsafe { std::mem::zeroed() };
            let mut named_len = std::mem::size_of_val(&named) as libc::socklen_t;
            assert_eq!(
                inject(
                    libc::SYS_getsockname,
                    [
                        receiver_fd,
                        (&raw mut named) as u64,
                        (&raw mut named_len) as u64,
                        0,
                        0,
                        0
                    ]
                ),
                0
            );
            assert_eq!(u64::from(named_len), length);
            assert_eq!(named.sun_path, address.sun_path);
            assert_eq!(
                inject(
                    libc::SYS_sendto,
                    [
                        sender.as_raw_fd() as u64,
                        c"x".as_ptr() as u64,
                        1,
                        0,
                        pointer,
                        length
                    ]
                ),
                1
            );
            let mut output = [0xa5_u8; 8];
            let duplicate = descriptor(libc::SYS_dup, [receiver_fd, 0, 0, 0, 0, 0]);
            assert_eq!(
                inject(
                    libc::SYS_recvfrom,
                    [
                        duplicate.as_raw_fd() as u64,
                        output.as_mut_ptr() as u64,
                        output.len() as u64,
                        libc::MSG_DONTWAIT as u64,
                        0,
                        0
                    ]
                ),
                1
            );
            assert_eq!(output, [b'x', 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5]);
            error_matches_kernel(libc::SYS_socket, [u64::MAX, kind, 0, 0, 0, 0]);
            for number in [
                libc::SYS_bind,
                libc::SYS_getsockname,
                libc::SYS_sendto,
                libc::SYS_recvfrom,
                libc::SYS_dup,
            ] {
                error_matches_kernel(number, [u64::MAX, 1, 1, 0, 1, 1]);
            }
        }

        #[test]
        fn linkat_unlinkat_symlink_preserve_aliases_flags_and_errors() {
            let directory = tempfile::tempdir().unwrap();
            let handle = std::fs::File::open(directory.path()).unwrap();
            let fd = (5_u64 << 32) | handle.as_raw_fd() as u64;
            let original = directory.path().join("original");
            std::fs::write(&original, b"link content").unwrap();
            assert_eq!(
                inject(
                    libc::SYS_linkat,
                    [
                        fd,
                        c"original".as_ptr() as u64,
                        fd,
                        c"alias".as_ptr() as u64,
                        0,
                        0
                    ]
                ),
                0
            );
            assert_eq!(std::fs::metadata(&original).unwrap().nlink(), 2);
            assert_eq!(
                inject(
                    libc::SYS_unlinkat,
                    [fd, c"alias".as_ptr() as u64, 0, 0, 0, 0]
                ),
                0
            );
            assert_eq!(std::fs::metadata(&original).unwrap().nlink(), 1);
            std::fs::create_dir(directory.path().join("empty")).unwrap();
            assert_eq!(
                inject(
                    libc::SYS_unlinkat,
                    [
                        fd,
                        c"empty".as_ptr() as u64,
                        libc::AT_REMOVEDIR as u64,
                        0,
                        0,
                        0
                    ]
                ),
                0
            );
            let symbolic = CString::new(
                directory
                    .path()
                    .join("symbolic")
                    .as_os_str()
                    .as_encoded_bytes(),
            )
            .unwrap();
            assert_eq!(
                inject(
                    libc::SYS_symlink,
                    [
                        c"original".as_ptr() as u64,
                        symbolic.as_ptr() as u64,
                        0,
                        0,
                        0,
                        0
                    ]
                ),
                0
            );
            assert_eq!(
                inject(
                    libc::SYS_linkat,
                    [
                        fd,
                        c"symbolic".as_ptr() as u64,
                        fd,
                        c"followed".as_ptr() as u64,
                        libc::AT_SYMLINK_FOLLOW as u64,
                        0
                    ]
                ),
                0
            );
            assert_eq!(
                std::fs::metadata(directory.path().join("followed"))
                    .unwrap()
                    .ino(),
                std::fs::metadata(&original).unwrap().ino()
            );
            assert_eq!(std::fs::read(original).unwrap(), b"link content");
            error_matches_kernel(libc::SYS_linkat, [u64::MAX, 1, u64::MAX, 1, 0, 0]);
            error_matches_kernel(
                libc::SYS_linkat,
                [
                    fd,
                    c"original".as_ptr() as u64,
                    fd,
                    c"never".as_ptr() as u64,
                    u64::MAX,
                    0,
                ],
            );
            error_matches_kernel(
                libc::SYS_unlinkat,
                [fd, c"missing".as_ptr() as u64, 0, 0, 0, 0],
            );
            error_matches_kernel(libc::SYS_symlink, [1, 1, 0, 0, 0, 0]);
        }
    }

    thread_local! {
        static LAST_STATE_DROP: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    }

    #[derive(Default, serde::Serialize, serde::Deserialize)]
    struct ObservedState(u64);

    impl Drop for ObservedState {
        fn drop(&mut self) {
            LAST_STATE_DROP.set(Some(self.0));
        }
    }

    #[derive(Default)]
    struct SnapshotTool;

    #[reverie::tool]
    impl Tool for SnapshotTool {
        type GlobalState = ();
        type ThreadState = ObservedState;

        fn init_thread_state(
            &self,
            _tid: Pid,
            _parent: Option<(Pid, &ObservedState)>,
        ) -> ObservedState {
            panic!("snapshot must use already registered state")
        }

        fn vdso_rng_snapshot(
            &self,
            state: &ObservedState,
        ) -> Result<reverie::vdso::VdsoRngSnapshot, Error> {
            if state.0 == 0 {
                return Err(Errno::EIO.into());
            }
            Ok(reverie::vdso::VdsoRngSnapshot {
                ready: state.0 % 2 == 0,
                generation: state.0,
            })
        }
    }

    fn with_rng_host<T: Tool<GlobalState = (), ThreadState = ObservedState> + 'static>(
        tool: T,
        test: impl FnOnce(&ToolHost<T>),
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rng.sock");
        let server_path = path.clone();
        let (ready, wait) = std::sync::mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let server = reverie_rpc_transport::RpcServer::bind(
                    server_path,
                    std::sync::Arc::new(()),
                    (),
                )
                .unwrap();
                ready.send(()).unwrap();
                server.serve_one().await.unwrap();
            });
        });
        wait.recv().unwrap();
        let host = ToolHost {
            registry: Registry::new(raw_pid(libc::SYS_getpid), tool),
            rpc: Arc::new(CoordinatorRpc::connect(&path).unwrap()),
            root_pid: raw_pid(libc::SYS_getpid),
            subscriptions: Default::default(),
            instruction_subscriptions: runtime::InstructionSubscriptions {
                cpuid: false,
                rdtsc: false,
            },
            instruction_results_only: false,
            stats: crate::stats::GuestStatsHooks::DISABLED,
        };
        {
            let scratch = DispatchScratchScope::enter();
            let mut initial = host
                .registry
                .reserve(
                    host.root_pid,
                    raw_pid(libc::SYS_gettid),
                    host.rpc.clone(),
                    &scratch.owner,
                    true,
                )
                .unwrap();
            initial.install_state(ObservedState(42)).unwrap();
            initial.complete().unwrap();
        }
        LAST_STATE_DROP.set(None);
        test(&host);
        drop(host);
        server.join().unwrap();
    }

    fn inspect_registered_state<
        T: Tool<GlobalState = (), ThreadState = ObservedState> + 'static,
    >(
        host: &ToolHost<T>,
        replacement: Option<u64>,
    ) -> Result<u64, Error> {
        let scratch = DispatchScratchScope::enter();
        let (mut invocation, _) = host.begin_invocation(
            host.root_pid,
            raw_pid(libc::SYS_gettid),
            &scratch.owner,
            false,
        )?;
        let (_, state, _) = invocation.parts();
        let observed = state.0;
        if let Some(replacement) = replacement {
            state.0 = replacement;
        }
        invocation.complete()?;
        Ok(observed)
    }

    #[test]
    fn modeled_rng_snapshot_uses_registered_state_and_releases_both_borrows() {
        with_rng_host(SnapshotTool, |host| {
            let tid = raw_pid(libc::SYS_gettid);
            let owner = i64::from(tid.as_raw());
            let snapshot = host.rng_snapshot(owner).unwrap();
            assert_eq!((snapshot.ready, snapshot.generation), (true, 42));
            assert!(host.registry.tool_present());
            assert_eq!(inspect_registered_state(host, Some(43)).unwrap(), 42);
            let second = host.rng_snapshot(owner).unwrap();
            assert_eq!((second.ready, second.generation), (false, 43));
            assert_eq!(snapshot.generation, 42);
        });
    }

    #[test]
    fn modeled_rng_snapshot_preserves_missing_owner_state_tool_and_typed_error() {
        with_rng_host(SnapshotTool, |host| {
            let tid = raw_pid(libc::SYS_gettid);
            let owner = i64::from(tid.as_raw());
            assert!(host.rng_snapshot(owner + 1).is_err());
            assert_eq!(host.registry.active_entries(), 1);
            host.registry
                .retire(host.registry.identity(host.root_pid, tid).unwrap())
                .unwrap();
            assert!(host.rng_snapshot(owner).is_err());
            assert_eq!(host.registry.active_entries(), 0);
            assert_eq!(LAST_STATE_DROP.get(), Some(42));
        });
        with_rng_host(SnapshotTool, |host| {
            let owner = i64::from(raw_pid(libc::SYS_gettid).as_raw());
            assert_eq!(inspect_registered_state(host, Some(0)).unwrap(), 42);
            assert!(matches!(
                host.rng_snapshot(owner),
                Err(Error::Errno(Errno::EIO))
            ));
            assert_eq!(LAST_STATE_DROP.get(), Some(0));
            assert_eq!(host.registry.active_entries(), 0);
            assert!(host.rng_snapshot(owner).is_err());
        });
        with_rng_host(SnapshotTool, |host| {
            let owner = i64::from(raw_pid(libc::SYS_gettid).as_raw());
            let scratch = DispatchScratchScope::enter();
            let (invocation, _) = host
                .begin_invocation(
                    host.root_pid,
                    raw_pid(libc::SYS_gettid),
                    &scratch.owner,
                    false,
                )
                .unwrap();
            host.registry.close(invocation.identity()).unwrap();
            drop(invocation);
            drop(drive_ready(host.registry.consume(host.root_pid)).unwrap());
            assert!(!host.registry.tool_present());
            assert!(host.rng_snapshot(owner).is_err());
        });
        with_rng_host(ExitTool::default(), |host| {
            let error = host
                .rng_snapshot(i64::from(raw_pid(libc::SYS_gettid).as_raw()))
                .unwrap_err();
            let Error::Tool(error) = error else {
                panic!("default snapshot must retain typed unsupported")
            };
            assert!(matches!(
                error.downcast_ref::<reverie::vdso::UnsupportedVdsoEvent>(),
                Some(reverie::vdso::UnsupportedVdsoEvent::RngSnapshot)
            ));
        });
    }

    #[derive(Clone, Copy, Default)]
    pub(crate) enum ProgressMode {
        #[default]
        Preserve,
        Rearm(u64, u64),
        Fail,
        Pending,
        Mutate,
    }

    #[derive(Default)]
    struct ProgressTool(ProgressMode);

    #[reverie::tool]
    impl Tool for ProgressTool {
        type GlobalState = ();
        type ThreadState = ObservedState;

        async fn handle_guest_progress<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
            let clock = guest.read_clock()?;
            assert_eq!(clock, 42);
            let registers = guest.regs().await;
            assert_eq!(registers.eflags & 0x100, 0x100);
            guest.send_rpc(()).await;
            assert_eq!(guest.read_clock()?, clock);
            guest.thread_state_mut().0 += 1;
            match self.0 {
                ProgressMode::Preserve => (),
                ProgressMode::Rearm(rcbs, suffix) => {
                    guest.set_timer_precise(TimerSchedule::RcbsAndInstructions(rcbs, suffix))?
                }
                ProgressMode::Fail | ProgressMode::Pending => {
                    guest.set_timer_precise(TimerSchedule::Rcbs(8))?;
                    if matches!(self.0, ProgressMode::Pending) {
                        std::future::pending::<()>().await;
                    }
                    return Err(Errno::EIO.into());
                }
                ProgressMode::Mutate => {
                    guest
                        .set_regs(libc::user_regs_struct {
                            rax: registers.rax ^ 1,
                            ..registers
                        })
                        .await?;
                }
            }
            assert_eq!(guest.read_clock()?, clock);
            Ok(())
        }

        fn vdso_rng_snapshot(
            &self,
            state: &ObservedState,
        ) -> Result<reverie::vdso::VdsoRngSnapshot, Error> {
            assert_eq!(state.0, 43);
            Ok(reverie::vdso::VdsoRngSnapshot {
                ready: true,
                generation: state.0,
            })
        }
    }

    pub(crate) fn with_progress_callbacks(
        mode: ProgressMode,
        test: impl FnOnce(
            &dyn Fn(i64, &mut HookContext, u64) -> Result<(), Error>,
            &dyn Fn(i64) -> Result<reverie::vdso::VdsoRngSnapshot, Error>,
        ),
    ) {
        with_rng_host(ProgressTool(mode), |host| {
            test(
                &|owner, context, clock| host.guest_progress(owner, context, clock),
                &|owner| host.rng_snapshot(owner),
            );
            assert!(host.registry.tool_present());
            assert_eq!(host.registry.entry_count(), 1);
        });
    }

    #[test]
    fn modeled_rng_progress_default_is_typed_and_never_initializes_missing_state() {
        with_rng_host(ExitTool::default(), |host| {
            let owner = i64::from(raw_pid(libc::SYS_gettid).as_raw());
            let mut context: HookContext = unsafe { core::mem::zeroed() };
            let error = host.guest_progress(owner, &mut context, 42).unwrap_err();
            assert!(
                matches!(error, Error::Tool(error) if error.is::<reverie::UnsupportedGuestProgress>())
            );
            assert_eq!(LAST_STATE_DROP.get(), Some(42));
            assert!(host.guest_progress(owner + 1, &mut context, 42).is_err());
            assert!(host.guest_progress(owner, &mut context, 42).is_err());
            assert_eq!(host.registry.active_entries(), 0);
        });
    }

    #[test]
    fn modeled_rng_progress_future_drop_preserves_partial_state_and_releases_borrows() {
        crate::timer::owned_tests::with_paused_model(|| {
            with_rng_host(ProgressTool(ProgressMode::Pending), |host| {
                let owner = i64::from(raw_pid(libc::SYS_gettid).as_raw());
                let mut context: HookContext = unsafe { core::mem::zeroed() };
                context.instruction_pointer = 0x4000;
                context.rflags = 0x302;
                let token = crate::timer::begin_modeled_interruption(None, 0x4000, 42).unwrap();
                crate::timer::open_window(0x4000, 42).unwrap();
                let scratch = DispatchScratchScope::enter();
                let mut future =
                    Box::pin(host.progress_future(owner, &mut context, 42, &scratch.owner));
                let mut poll = core::task::Context::from_waker(core::task::Waker::noop());
                assert!(matches!(
                    core::future::Future::poll(future.as_mut(), &mut poll),
                    core::task::Poll::Pending
                ));
                drop(future);
                crate::timer::fail_modeled_interruption(&token).unwrap();
                crate::timer::close_window().unwrap();
                assert!(host.registry.tool_present());
                assert_eq!(LAST_STATE_DROP.get(), Some(43));
                assert!(host.rng_snapshot(owner).is_err());
                assert_eq!(host.registry.active_entries(), 0);
                assert_eq!(crate::timer::next_step(), Err(Errno::EBUSY));
                assert_eq!(
                    crate::timer::resolve_modeled_interruption(&token, None),
                    Err(Errno::ECANCELED)
                );
            });
        });
    }

    #[derive(Default)]
    struct ExitTool {
        events: std::sync::Arc<std::sync::Mutex<Vec<(&'static str, i32)>>>,
        fail: Option<&'static str>,
    }

    #[reverie::tool]
    impl Tool for ExitTool {
        type GlobalState = ();
        type ThreadState = ObservedState;

        async fn on_exit_thread<G: GlobalRPC<()>>(
            &self,
            tid: Pid,
            rpc: &G,
            state: ObservedState,
            status: reverie::ExitStatus,
        ) -> Result<(), Error> {
            assert_eq!(tid, Pid::from_raw(5));
            assert_eq!(state.0, 42);
            let reverie::ExitStatus::Exited(code) = status else {
                panic!("wrong status origin")
            };
            self.events.lock().unwrap().push(("thread", code));
            rpc.send_rpc(()).await;
            if self.fail == Some("thread") {
                return Err(Errno::EIO.into());
            }
            Ok(())
        }

        async fn on_exit_process<G: GlobalRPC<()>>(
            self,
            pid: Pid,
            _rpc: &G,
            status: reverie::ExitStatus,
        ) -> Result<(), Error> {
            assert_eq!(pid, Pid::from_raw(5));
            let reverie::ExitStatus::Exited(code) = status else {
                panic!("wrong status origin")
            };
            self.events.lock().unwrap().push(("process", code));
            if self.fail == Some("process") {
                return Err(Errno::ENOSPC.into());
            }
            Ok(())
        }
    }

    struct ExitRpc(std::sync::Arc<std::sync::Mutex<Vec<(&'static str, i32)>>>);

    impl crate::rpc::BoundRpc<()> for ExitRpc {
        fn identity(&self) -> (Pid, Pid) {
            (Pid::from_raw(5), Pid::from_raw(5))
        }
    }

    #[reverie::tool]
    impl GlobalRPC<()> for ExitRpc {
        async fn send_rpc(&self, _message: ()) {
            self.0.lock().unwrap().push(("rpc", 0));
            std::future::poll_fn({
                let mut pending = true;
                move |_| {
                    if std::mem::replace(&mut pending, false) {
                        std::task::Poll::Pending
                    } else {
                        std::task::Poll::Ready(())
                    }
                }
            })
            .await;
        }
        fn config(&self) -> &() {
            &()
        }
    }

    struct ExitFutureDrop(std::sync::Arc<std::sync::Mutex<Vec<(&'static str, i32)>>>);
    impl Drop for ExitFutureDrop {
        fn drop(&mut self) {
            self.0.lock().unwrap().push(("future-drop", 0));
        }
    }

    #[test]
    fn owned_exit_driver_drops_tail_future_before_exactly_once_hooks_and_stats() {
        for number in [libc::SYS_exit, libc::SYS_exit_group] {
            for status in [0, 7, 125, 130, 255, 256, u64::MAX, 0x1234_5678_0000_0007] {
                let tool = ExitTool::default();
                let events = tool.events.clone();
                let tail = TailResult::default();
                let args = [status, 11, 22, 33, 44, 55];
                let outcome = reverie_preload::tool_host::drive_syscall(
                    async {
                        let _guard = ExitFutureDrop(events.clone());
                        tail.set_exit(number, args);
                        std::future::pending::<Result<i64, Error>>().await
                    },
                    &tail,
                );
                let reverie_preload::tool_host::SyscallOutcome::Exit {
                    number: selected,
                    args: raw,
                } = outcome
                else {
                    panic!("tail exit resumed")
                };
                assert_eq!((selected, raw), (number, args));
                let scratch = DispatchScratchScope::enter();
                let registry = Registry::new(Pid::from_raw(5), tool);
                let mut invocation = registry
                    .reserve(
                        Pid::from_raw(5),
                        Pid::from_raw(5),
                        Arc::new(ExitRpc(events.clone())),
                        &scratch.owner,
                        true,
                    )
                    .unwrap();
                invocation.install_state(ObservedState(42)).unwrap();
                let completed = finish_tool_exit_callbacks(
                    &registry,
                    invocation,
                    ToolExitContext {
                        tid: Pid::from_raw(5),
                        pid: Pid::from_raw(5),
                        number: selected,
                        args: raw,
                    },
                    |tid| {
                        assert_eq!(tid, Pid::from_raw(5));
                        events.lock().unwrap().push(("stats", 0));
                        Ok(())
                    },
                    || Ok(()),
                )
                .unwrap();
                assert!(completed && !registry.tool_present() && registry.active_entries() == 0);
                let polls = std::cell::Cell::new(0);
                let second = reverie_preload::tool_host::drive_syscall(
                    std::future::poll_fn(|_| {
                        polls.set(polls.get() + 1);
                        if polls.get() == 1 {
                            std::task::Poll::Pending
                        } else {
                            std::task::Poll::Ready(Ok(19))
                        }
                    }),
                    &tail,
                );
                assert!(matches!(
                    second,
                    reverie_preload::tool_host::SyscallOutcome::Return(Ok(19))
                ));
                assert_eq!(polls.get(), 2, "the consumed tail action must not replay");
                let code = (status & 255) as i32;
                assert_eq!(
                    *events.lock().unwrap(),
                    [
                        ("future-drop", 0),
                        ("thread", code),
                        ("rpc", 0),
                        ("process", code),
                        ("stats", 0)
                    ]
                );
            }
        }
    }

    #[test]
    fn owned_exit_callback_failures_stop_before_completion_and_preserve_error() {
        for failure in ["thread", "process", "stats"] {
            let tool = ExitTool {
                fail: Some(failure),
                ..ExitTool::default()
            };
            let events = tool.events.clone();
            let scratch = DispatchScratchScope::enter();
            let registry = Registry::new(Pid::from_raw(5), tool);
            let mut invocation = registry
                .reserve(
                    Pid::from_raw(5),
                    Pid::from_raw(5),
                    Arc::new(ExitRpc(events.clone())),
                    &scratch.owner,
                    true,
                )
                .unwrap();
            invocation.install_state(ObservedState(42)).unwrap();
            let result = finish_tool_exit_callbacks(
                &registry,
                invocation,
                ToolExitContext {
                    tid: Pid::from_raw(5),
                    pid: Pid::from_raw(5),
                    number: libc::SYS_exit_group,
                    args: [7, 0, 0, 0, 0, 0],
                },
                |_| {
                    events.lock().unwrap().push(("stats", 0));
                    Err(Errno::EPIPE.into())
                },
                || Ok(()),
            );
            let expected = match failure {
                "thread" => Errno::EIO,
                "process" => Errno::ENOSPC,
                _ => Errno::EPIPE,
            };
            assert_eq!(result.unwrap_err().into_errno().unwrap(), expected);
            assert_eq!(registry.active_entries(), 0);
            assert_eq!(registry.tool_present(), failure == "thread");
            let expected_events = [("thread", 7), ("rpc", 0), ("process", 7), ("stats", 0)];
            let length = match failure {
                "thread" => 2,
                "process" => 3,
                _ => 4,
            };
            assert_eq!(*events.lock().unwrap(), expected_events[..length]);
        }
    }

    #[test]
    fn owned_observation_refusal_precedes_callbacks_and_legacy_forwarding() {
        let subscriptions: HashSet<_> = [Sysno::getpid].into_iter().collect();
        for (number, reason) in [
            (-1, ObservationRefusal::UnknownNumber),
            (0x3fff_ffff, ObservationRefusal::UnknownNumber),
            (
                libc::SYS_getpid | 0x4000_0000,
                ObservationRefusal::UnknownNumber,
            ),
            (libc::SYS_execve, ObservationRefusal::Unsubscribed),
            (libc::SYS_clone, ObservationRefusal::Unsubscribed),
            (libc::SYS_exit_group, ObservationRefusal::Unsubscribed),
        ] {
            let mut callbacks = 0;
            let result = validate_owned_observation(true, number, &subscriptions).map(|()| {
                callbacks += 1;
            });
            assert_eq!(result, Err(reason));
            assert_eq!(callbacks, 0);
            assert_eq!(
                validate_owned_observation(false, number, &subscriptions),
                Ok(())
            );
        }
        let all = reverie::Subscription::all().iter_syscalls().collect();
        for number in [
            Sysno::getpid,
            Sysno::execve,
            Sysno::clone,
            Sysno::exit_group,
        ] {
            assert_eq!(
                validate_owned_observation(true, number as i64, &all),
                Ok(())
            );
        }
    }

    #[test]
    fn owned_observation_terminal_retains_cause_without_guest_errno() {
        let test = "tool_host::tests::owned_observation_terminal_retains_cause_without_guest_errno";
        if let Some(case) = owned_setup_case() {
            let number = case.parse::<i64>().unwrap();
            let subscriptions = [Sysno::getpid].into_iter().collect();
            if let Err(reason) = validate_owned_observation(true, number, &subscriptions) {
                terminate_owned_observation(number, reason);
            }
            return;
        }
        for (number, reason) in [
            (0x3fff_ffff, "UnknownNumber"),
            (libc::SYS_execve, "Unsubscribed"),
            (libc::SYS_exit_group, "Unsubscribed"),
        ] {
            let child = owned_setup_child(test, &number.to_string());
            assert_eq!(
                child.status.code(),
                Some(UNSUPPORTED_OWNED_INJECTION_STATUS)
            );
            assert_eq!(
                String::from_utf8(child.stderr).unwrap(),
                format!(
                    "hermit-liteinst owned observation refused: stage=owned-observation cause={reason} number={number}\n"
                )
            );
        }
        assert_child_ok(
            &owned_setup_child(test, &libc::SYS_getpid.to_string()),
            "subscribed",
        );
    }

    #[test]
    fn owned_syscall_injection_refuses_lifecycle_before_effects() {
        for number in [
            libc::SYS_exit,
            libc::SYS_exit_group,
            libc::SYS_fork,
            libc::SYS_clone,
            libc::SYS_execve,
            libc::SYS_rt_sigreturn,
            libc::SYS_rt_sigprocmask,
            libc::SYS_pwritev2,
            libc::SYS_getpid | 0x40000000,
        ] {
            assert_eq!(
                classify_owned_injection(true, number),
                OwnedInjection::Terminal,
                "owned injection of {number} must be terminal, not an errno"
            );
            assert_eq!(
                classify_owned_injection(false, number),
                OwnedInjection::Admitted
            );
        }
        for number in [
            libc::SYS_getpid,
            libc::SYS_read,
            libc::SYS_openat,
            libc::SYS_fstat,
            libc::SYS_close,
            libc::SYS_gettid,
            libc::SYS_getppid,
        ] {
            assert_eq!(
                classify_owned_injection(true, number),
                OwnedInjection::Admitted
            );
        }
    }

    #[test]
    fn owned_set_tid_address_preserves_kernel_registration() {
        if owned_setup_case().as_deref() != Some("tid-registration") {
            assert_child_ok(
                &owned_setup_child(
                    "tool_host::tests::owned_set_tid_address_preserves_kernel_registration",
                    "tid-registration",
                ),
                "tid-registration",
            );
            return;
        }

        struct RegistrationCase {
            initial: i32,
            guest: i32,
            spare: i32,
            before_exit: [i32; 3],
            tid: i64,
            restored: i64,
            calls: usize,
            mode: u8,
        }

        struct RestoreRegistration {
            address: u64,
            result: *mut i64,
            armed: bool,
        }

        impl Drop for RestoreRegistration {
            fn drop(&mut self) {
                if self.armed {
                    unsafe {
                        *self.result = guarded_raw_injection(
                            libc::SYS_set_tid_address,
                            [self.address, 0, 0, 0, 0, 0],
                        );
                    }
                }
            }
        }

        extern "C" fn registration_child(argument: *mut libc::c_void) -> i32 {
            let state = unsafe { &mut *argument.cast::<RegistrationCase>() };
            let mut restore = RestoreRegistration {
                address: &mut state.initial as *mut i32 as u64,
                result: &mut state.restored,
                armed: true,
            };
            let tid = guarded_raw_injection(libc::SYS_gettid, [0; 6]);
            state.tid = tid;
            static READ_ONLY: i32 = 0x1234_5678;
            let guest = &mut state.guest as *mut i32 as u64;
            for address in [
                guest,
                &mut state.spare as *mut i32 as u64,
                guest + 1,
                &READ_ONLY as *const i32 as u64,
                1,
                1_u64 << 63,
                u64::MAX,
                0,
            ] {
                let address = match state.mode {
                    3 => 0,
                    4 => 1,
                    _ => address,
                };
                let args = [address, 0, 0, 0, 0, 0];
                if tid <= 0 || guarded_raw_injection(libc::SYS_set_tid_address, args) != tid {
                    return 41;
                }
                state.calls += 1;
                state.before_exit = unsafe {
                    [
                        std::ptr::read_volatile(&state.initial),
                        std::ptr::read_volatile(&state.guest),
                        std::ptr::read_volatile(&state.spare),
                    ]
                };
                if state.mode == 1 {
                    return 42;
                }
                if state.mode >= 2 {
                    restore.armed = false;
                    return 0;
                }
            }
            0
        }

        struct Stack(*mut libc::c_void, usize);

        impl Drop for Stack {
            fn drop(&mut self) {
                if unsafe { libc::munmap(self.0, self.1) } != 0 {
                    std::process::abort();
                }
            }
        }

        assert_eq!(
            classify_owned_injection(true, libc::SYS_set_tid_address),
            OwnedInjection::Admitted
        );
        assert!(!crate::mapping::operation(libc::SYS_set_tid_address));
        for mode in 0..5 {
            let mut state = RegistrationCase {
                initial: 0x1234_5678,
                guest: 0x2345_6789,
                spare: 0x3456_789a,
                before_exit: [0; 3],
                tid: 0,
                restored: -1,
                calls: 0,
                mode,
            };
            let stack_size = 1024 * 1024;
            let allocation = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    stack_size,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(allocation, libc::MAP_FAILED);
            let stack = Stack(allocation, stack_size);
            let initial = &mut state.initial as *mut i32;
            let child = unsafe {
                libc::clone(
                    registration_child,
                    stack.0.add(stack_size),
                    libc::CLONE_VM | libc::CLONE_VFORK | libc::CLONE_CHILD_CLEARTID | libc::SIGCHLD,
                    (&mut state as *mut RegistrationCase).cast(),
                    std::ptr::null_mut::<libc::c_void>(),
                    std::ptr::null_mut::<libc::c_void>(),
                    initial,
                )
            };
            assert!(child > 0);
            let mut status = 0;
            loop {
                let waited = unsafe { libc::waitpid(child, &mut status, 0) };
                if waited < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                {
                    continue;
                }
                assert_eq!(waited, child);
                break;
            }
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), if mode == 1 { 42 } else { 0 });
            assert_eq!(state.tid, i64::from(child));
            assert_eq!(state.calls, if mode == 0 { 8 } else { 1 });
            assert_eq!(state.before_exit, [0x1234_5678, 0x2345_6789, 0x3456_789a]);
            assert_eq!(state.restored, if mode < 2 { i64::from(child) } else { -1 });
            assert_eq!(
                unsafe { std::ptr::read_volatile(initial) },
                if mode < 2 { 0 } else { 0x1234_5678 }
            );
            assert_eq!(
                unsafe { std::ptr::read_volatile(&state.guest) },
                if mode == 2 { 0 } else { 0x2345_6789 }
            );
            assert_eq!(
                unsafe { std::ptr::read_volatile(&state.spare) },
                0x3456_789a
            );
        }
    }

    #[test]
    fn owned_set_tid_address_does_not_admit_adjacent_lifecycle_effects() {
        assert!(crate::syscall_event::observable(libc::SYS_set_tid_address));
        assert!(!crate::syscall_event::backed_returning(
            libc::SYS_set_tid_address
        ));
        for number in [
            libc::SYS_set_tid_address | 0x4000_0000,
            libc::SYS_get_robust_list,
            libc::SYS_clone,
            libc::SYS_clone3,
            libc::SYS_fork,
            libc::SYS_vfork,
            libc::SYS_execve,
            libc::SYS_execveat,
            libc::SYS_exit,
            libc::SYS_exit_group,
            libc::SYS_futex,
        ] {
            assert_eq!(
                classify_owned_injection(true, number),
                OwnedInjection::Terminal
            );
        }
    }

    #[test]
    fn owned_directory_injection_preserves_creation_replacement_move_and_cleanup() {
        use std::ffi::CString;
        use std::os::unix::fs::PermissionsExt;
        fn inject(number: i64, args: [u64; 6]) -> i64 {
            assert_eq!(
                classify_owned_injection(true, number),
                OwnedInjection::Admitted
            );
            assert_eq!(injected_syscall_guard(number, args), None);
            assert!(!crate::mapping::operation(number));
            guarded_raw_injection(number, args)
        }
        let root = tempfile::tempdir().unwrap();
        let sub = root.path().join("sub");
        let control = root.path().join("control");
        let path =
            |path: &std::path::Path| CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let sub_c = path(&sub);
        let control_c = path(&control);
        assert_eq!(
            inject(libc::SYS_mkdir, [sub_c.as_ptr() as u64, 0o750, 0, 0, 0, 0]),
            0
        );
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_mkdir,
                    [control_c.as_ptr() as u64, 0o750, 0, 0, 0, 0],
                )
            },
            0
        );
        assert_eq!(
            std::fs::metadata(&sub).unwrap().permissions().mode(),
            std::fs::metadata(&control).unwrap().permissions().mode()
        );
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        std::fs::write(&source, b"original payload").unwrap();
        std::fs::write(&destination, b"replaced").unwrap();
        let held = std::fs::File::open(&source).unwrap();
        let source_c = path(&source);
        let destination_c = path(&destination);
        assert_eq!(
            inject(
                libc::SYS_rename,
                [
                    source_c.as_ptr() as u64,
                    destination_c.as_ptr() as u64,
                    0,
                    0,
                    0,
                    0
                ]
            ),
            0
        );
        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"original payload");
        assert_eq!(held.metadata().unwrap().len(), 16);
        let root_fd = std::fs::File::open(root.path()).unwrap();
        let sub_fd = std::fs::File::open(&sub).unwrap();
        assert_eq!(
            inject(
                libc::SYS_renameat,
                [
                    root_fd.as_raw_fd() as u64,
                    c"destination".as_ptr() as u64,
                    sub_fd.as_raw_fd() as u64,
                    c"moved".as_ptr() as u64,
                    0,
                    0
                ]
            ),
            0
        );
        assert!(!destination.exists());
        assert_eq!(
            std::fs::read(sub.join("moved")).unwrap(),
            b"original payload"
        );
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_lseek,
                    [held.as_raw_fd() as u64, 0, libc::SEEK_CUR as u64, 0, 0, 0],
                )
            },
            0
        );
        std::fs::remove_file(sub.join("moved")).unwrap();
        assert_eq!(
            inject(libc::SYS_rmdir, [sub_c.as_ptr() as u64, 0, 0, 0, 0, 0]),
            0
        );
        assert!(!sub.exists());
    }

    #[test]
    fn owned_directory_injection_retains_kernel_errors_and_inputs() {
        use std::ffi::CString;
        let root = tempfile::tempdir().unwrap();
        let existing = root.path().join("entry");
        std::fs::write(&existing, b"unchanged").unwrap();
        let path =
            |path: &std::path::Path| CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let entry = path(&existing);
        let missing = path(&root.path().join("missing/child"));
        let nondirectory = path(&existing.join("child"));
        let nonempty = path(root.path());
        let input = entry.as_bytes_with_nul().to_vec();
        for (number, args) in [
            (libc::SYS_mkdir, [entry.as_ptr() as u64, 0o700, 0, 0, 0, 0]),
            (
                libc::SYS_mkdir,
                [missing.as_ptr() as u64, 0o700, 0, 0, 0, 0],
            ),
            (
                libc::SYS_mkdir,
                [nondirectory.as_ptr() as u64, 0o700, 0, 0, 0, 0],
            ),
            (libc::SYS_mkdir, [0, 0o700, 0, 0, 0, 0]),
            (
                libc::SYS_rename,
                [missing.as_ptr() as u64, entry.as_ptr() as u64, 0, 0, 0, 0],
            ),
            (libc::SYS_rename, [entry.as_ptr() as u64, 0, 0, 0, 0, 0]),
            (
                libc::SYS_renameat,
                [
                    (-1_i32) as u64,
                    c"entry".as_ptr() as u64,
                    (-1_i32) as u64,
                    c"other".as_ptr() as u64,
                    0,
                    0,
                ],
            ),
            (
                libc::SYS_renameat,
                [
                    libc::AT_FDCWD as u64,
                    0,
                    libc::AT_FDCWD as u64,
                    entry.as_ptr() as u64,
                    0,
                    0,
                ],
            ),
            (libc::SYS_rmdir, [entry.as_ptr() as u64, 0, 0, 0, 0, 0]),
            (libc::SYS_rmdir, [nonempty.as_ptr() as u64, 0, 0, 0, 0, 0]),
            (libc::SYS_rmdir, [0, 0, 0, 0, 0, 0]),
        ] {
            let expected = unsafe { raw_syscall6(number, args) };
            assert!(expected < 0);
            assert_eq!(
                classify_owned_injection(true, number),
                OwnedInjection::Admitted
            );
            assert_eq!(injected_syscall_guard(number, args), None);
            let actual = guarded_raw_injection(number, args);
            assert_eq!(actual, expected, "number={number} args={args:x?}");
            assert_eq!(
                Errno::from_ret(actual as usize),
                Errno::from_ret(expected as usize)
            );
        }
        assert_eq!(entry.as_bytes_with_nul(), input);
        assert_eq!(std::fs::read(existing).unwrap(), b"unchanged");
    }

    #[test]
    fn owned_access_injection_preserves_kernel_path_mode_and_result() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;

        let cwd = std::env::current_dir().unwrap();
        let directory = tempfile::tempdir_in(&cwd).unwrap();
        let path = directory.path().join("ordinary");
        std::fs::write(&path, b"access fixture").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink("ordinary", &alias).unwrap();
        let missing = directory.path().join("missing");
        let not_directory = path.join("child");
        let absolute = CString::new(path.as_os_str().as_bytes()).unwrap();
        let relative =
            CString::new(path.strip_prefix(&cwd).unwrap().as_os_str().as_bytes()).unwrap();
        let alias = CString::new(alias.as_os_str().as_bytes()).unwrap();
        let missing = CString::new(missing.as_os_str().as_bytes()).unwrap();
        let not_directory = CString::new(not_directory.as_os_str().as_bytes()).unwrap();
        let empty = c"";
        for (path, mode, expected) in [
            (absolute.as_c_str(), libc::F_OK, 0),
            (relative.as_c_str(), libc::R_OK, 0),
            (alias.as_c_str(), libc::R_OK | libc::W_OK, 0),
            (absolute.as_c_str(), libc::X_OK, -i64::from(libc::EACCES)),
            (absolute.as_c_str(), 8, -i64::from(libc::EINVAL)),
            (missing.as_c_str(), libc::F_OK, -i64::from(libc::ENOENT)),
            (
                not_directory.as_c_str(),
                libc::F_OK,
                -i64::from(libc::ENOTDIR),
            ),
            (empty, libc::F_OK, -i64::from(libc::ENOENT)),
        ] {
            let original = path.to_bytes_with_nul().to_vec();
            let args = [path.as_ptr() as u64, mode as u64, 0, 0, 0, 0];
            assert_eq!(
                classify_owned_injection(true, libc::SYS_access),
                OwnedInjection::Admitted
            );
            assert!(!crate::mapping::operation(libc::SYS_access));
            assert_eq!(injected_syscall_guard(libc::SYS_access, args), None);
            let result = guarded_raw_injection(libc::SYS_access, args);
            assert_eq!(result, expected, "{path:?} mode={mode}");
            assert_eq!(path.to_bytes_with_nul(), original);
            let converted = Errno::from_ret(result as usize).map(|value| value as i64);
            if expected < 0 {
                assert_eq!(i64::from(converted.unwrap_err().into_raw()), -expected);
            } else {
                assert_eq!(converted.unwrap(), expected);
            }
        }
        let args = [0, libc::F_OK as u64, 0, 0, 0, 0];
        assert_eq!(injected_syscall_guard(libc::SYS_access, args), None);
        assert_eq!(
            guarded_raw_injection(libc::SYS_access, args),
            -i64::from(libc::EFAULT)
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"access fixture");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::env::current_dir().unwrap(), cwd);
        directory.close().unwrap();
    }

    #[test]
    fn owned_pread64_preserves_data_offset_and_file_position() {
        use std::io::Seek;
        use std::io::SeekFrom;
        use std::io::Write;
        use std::os::unix::fs::FileExt;

        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"0123456789").unwrap();
        file.seek(SeekFrom::Start(3)).unwrap();
        assert_eq!(
            classify_owned_injection(true, libc::SYS_pread64),
            OwnedInjection::Admitted
        );
        assert!(!crate::mapping::operation(libc::SYS_pread64));
        for (offset, count, expected) in [
            (2, 4, &b"2345"[..]),
            (8, 8, &b"89"[..]),
            (10, 8, &b""[..]),
            (0x1_0000_0002, 8, &b""[..]),
            (2, 0, &b""[..]),
        ] {
            let mut buffer = [0xa5; 12];
            let args = [
                file.as_raw_fd() as u64,
                buffer.as_mut_ptr() as u64,
                count,
                offset,
                0,
                0,
            ];
            assert_eq!(injected_syscall_guard(libc::SYS_pread64, args), None);
            let result = guarded_raw_injection(libc::SYS_pread64, args);
            assert_eq!(result, expected.len() as i64);
            assert_eq!(&buffer[..expected.len()], expected);
            assert!(buffer[expected.len()..].iter().all(|byte| *byte == 0xa5));
            assert_eq!(file.stream_position().unwrap(), 3);
        }
        let mut sparse = tempfile::NamedTempFile::new().unwrap();
        let offset = 0x1_0000_0002;
        sparse.as_file().write_all_at(b"wide", offset).unwrap();
        sparse.seek(SeekFrom::Start(7)).unwrap();
        let mut buffer = [0xa5; 8];
        assert_eq!(
            guarded_raw_injection(
                libc::SYS_pread64,
                [
                    sparse.as_raw_fd() as u64,
                    buffer.as_mut_ptr() as u64,
                    8,
                    offset,
                    0,
                    0
                ]
            ),
            4
        );
        assert_eq!(&buffer[..4], b"wide");
        assert_eq!(&buffer[4..], &[0xa5; 4]);
        assert_eq!(sparse.stream_position().unwrap(), 7);
        sparse.close().unwrap();
        file.close().unwrap();
    }

    fn newfstatat_kernel_comparison(
        descriptor: u64,
        kernel_descriptor: u64,
        pathname: u64,
        flags: u64,
    ) -> (i64, [u8; core::mem::size_of::<libc::stat>()]) {
        let mut expected = [0xa5; core::mem::size_of::<libc::stat>()];
        let mut observed = expected;
        let args = |descriptor, output| [descriptor, pathname, output, flags, 0, 0];
        let kernel = unsafe {
            raw_syscall6(
                libc::SYS_newfstatat,
                args(kernel_descriptor, expected.as_mut_ptr() as u64),
            )
        };
        let result = guarded_raw_injection(
            libc::SYS_newfstatat,
            args(descriptor, observed.as_mut_ptr() as u64),
        );
        assert_eq!(result, kernel, "dirfd={descriptor:#x} flags={flags:#x}");
        assert_eq!(observed, expected, "dirfd={descriptor:#x} flags={flags:#x}");
        (result, observed)
    }

    #[test]
    fn owned_newfstatat_admission_preserves_other_effect_boundaries() {
        assert_eq!(
            classify_owned_injection(true, libc::SYS_newfstatat),
            OwnedInjection::Admitted
        );
        assert!(crate::syscall_event::observable(libc::SYS_newfstatat));
        assert!(!crate::syscall_event::backed_returning(
            libc::SYS_newfstatat
        ));
        assert!(!crate::mapping::operation(libc::SYS_newfstatat));
        for number in [
            -1,
            libc::SYS_newfstatat | 0x4000_0000,
            libc::SYS_statx,
            libc::SYS_stat,
            libc::SYS_lstat,
            libc::SYS_faccessat,
            libc::SYS_execve,
        ] {
            assert_eq!(
                classify_owned_injection(true, number),
                OwnedInjection::Terminal
            );
        }
    }

    #[test]
    fn owned_newfstatat_paths_flags_and_directory_operands_match_linux() {
        use std::ffi::CString;
        use std::os::unix::fs::MetadataExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("entry");
        std::fs::write(&path, b"stat fixture").unwrap();
        std::os::unix::fs::symlink("entry", directory.path().join("link")).unwrap();
        let handle = std::fs::File::open(directory.path()).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let absolute = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let descriptor = handle.as_raw_fd() as u64;
        let cwd = std::env::current_dir().unwrap();
        for (dirfd, pathname, flags, wanted) in [
            (descriptor, c"entry".as_ptr(), 0, 0),
            (descriptor | (1 << 32), c"entry".as_ptr(), 0, 0),
            (u64::MAX, absolute.as_ptr(), 0, 0),
            (libc::AT_FDCWD as u64, absolute.as_ptr(), 0, 0),
            (descriptor, c"link".as_ptr(), 0, 0),
            (
                descriptor,
                c"link".as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW as u64,
                0,
            ),
            (
                file.as_raw_fd() as u64,
                c"".as_ptr(),
                libc::AT_EMPTY_PATH as u64,
                0,
            ),
            (
                descriptor,
                c"entry".as_ptr(),
                libc::AT_NO_AUTOMOUNT as u64,
                0,
            ),
            (u64::MAX, c"entry".as_ptr(), 0, -i64::from(libc::EBADF)),
            (
                file.as_raw_fd() as u64,
                c"entry".as_ptr(),
                0,
                -i64::from(libc::ENOTDIR),
            ),
            (descriptor, c"missing".as_ptr(), 0, -i64::from(libc::ENOENT)),
            (descriptor, c"".as_ptr(), 0, -i64::from(libc::ENOENT)),
            (
                descriptor,
                c"entry".as_ptr(),
                0x4000_0000,
                -i64::from(libc::EINVAL),
            ),
        ] {
            let (result, _) = newfstatat_kernel_comparison(dirfd, dirfd, pathname as u64, flags);
            assert_eq!(result, wanted);
        }
        let (result, bytes) =
            newfstatat_kernel_comparison(descriptor, descriptor, c"entry".as_ptr() as u64, 0);
        assert_eq!(result, 0);
        let metadata = unsafe { bytes.as_ptr().cast::<libc::stat>().read_unaligned() };
        let expected = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.st_ino, expected.ino());
        assert_eq!(metadata.st_size, 12);
        assert_eq!(std::env::current_dir().unwrap(), cwd);
    }

    #[test]
    fn owned_newfstatat_fault_and_error_ordering_matches_linux() {
        let file = tempfile::NamedTempFile::new().unwrap();
        for address in [0, 1, u64::MAX] {
            for flags in [0, libc::AT_EMPTY_PATH as u64, 0x4000_0000] {
                newfstatat_kernel_comparison(
                    file.as_raw_fd() as u64,
                    file.as_raw_fd() as u64,
                    address,
                    flags,
                );
            }
        }
        for descriptor in [file.as_raw_fd() as u64, u64::MAX] {
            for pathname in [c"".as_ptr() as u64, c"missing".as_ptr() as u64, 0] {
                for flags in [0, libc::AT_EMPTY_PATH as u64, 0x4000_0000] {
                    for output in [0, 1, u64::MAX] {
                        let args = [descriptor, pathname, output, flags, 0, 0];
                        let expected = unsafe { raw_syscall6(libc::SYS_newfstatat, args) };
                        assert_eq!(guarded_raw_injection(libc::SYS_newfstatat, args), expected);
                        assert!(expected < 0);
                    }
                }
            }
        }
    }

    #[test]
    fn owned_newfstatat_partial_output_matches_linux_bytes() {
        struct OutputMapping(*mut libc::c_void, usize);
        impl Drop for OutputMapping {
            fn drop(&mut self) {
                assert_eq!(unsafe { libc::munmap(self.0, self.1) }, 0);
            }
        }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page > 0);
        let page = page as usize;
        let file = tempfile::NamedTempFile::new().unwrap();
        for prefix in [0, 8, core::mem::size_of::<libc::stat>() / 2] {
            let mut outcomes = Vec::new();
            for guarded in [false, true] {
                let address = unsafe {
                    libc::mmap(
                        core::ptr::null_mut(),
                        2 * page,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        -1,
                        0,
                    )
                };
                assert_ne!(address, libc::MAP_FAILED);
                let mapping = OutputMapping(address, 2 * page);
                unsafe { address.cast::<u8>().write_bytes(0xa5, 2 * page) };
                let boundary = unsafe { address.cast::<u8>().add(page) };
                assert_eq!(
                    unsafe { libc::mprotect(boundary.cast(), page, libc::PROT_READ) },
                    0
                );
                let output = unsafe { boundary.sub(prefix) };
                let args = [
                    file.as_raw_fd() as u64,
                    c"".as_ptr() as u64,
                    output as u64,
                    libc::AT_EMPTY_PATH as u64,
                    0,
                    0,
                ];
                let result = if guarded {
                    guarded_raw_injection(libc::SYS_newfstatat, args)
                } else {
                    unsafe { raw_syscall6(libc::SYS_newfstatat, args) }
                };
                assert_eq!(result, -i64::from(libc::EFAULT));
                let bytes =
                    unsafe { core::slice::from_raw_parts(address.cast::<u8>(), 2 * page) }.to_vec();
                outcomes.push((result, bytes));
                drop(mapping);
            }
            assert_eq!(outcomes[0], outcomes[1], "accessible prefix={prefix}");
        }
    }

    #[test]
    fn owned_newfstatat_protected_dirfd_keeps_kernel_path_semantics() {
        use std::ffi::CString;
        use std::sync::atomic::Ordering;

        let test = "tool_host::tests::owned_newfstatat_protected_dirfd_keeps_kernel_path_semantics";
        if owned_setup_case().is_none() {
            let child = owned_setup_child(test, "newfstatat");
            assert!(child.status.success(), "{child:?}");
            return;
        }
        struct RestoreLogFd(i32);
        impl Drop for RestoreLogFd {
            fn drop(&mut self) {
                crate::guest_log::LOG_FD.store(self.0, Ordering::Release);
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("entry");
        std::fs::write(&path, b"protected dirfd fixture").unwrap();
        let handle = std::fs::File::open(directory.path()).unwrap();
        let descriptor = handle.as_raw_fd();
        let absolute = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let restore = RestoreLogFd(crate::guest_log::LOG_FD.swap(descriptor, Ordering::AcqRel));
        for dirfd in [descriptor as u64, (1 << 32) | descriptor as u64] {
            for (pathname, flags) in [
                (absolute.as_ptr() as u64, 0),
                (c"entry".as_ptr() as u64, 0),
                (c"".as_ptr() as u64, 0),
                (c"".as_ptr() as u64, libc::AT_EMPTY_PATH as u64),
                (0, libc::AT_EMPTY_PATH as u64),
                (1, 0),
                (absolute.as_ptr() as u64, 0x4000_0000),
            ] {
                newfstatat_kernel_comparison(dirfd, u64::MAX, pathname, flags);
            }
        }
        assert!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) } >= 0);
        drop(restore);
        let (result, _) = newfstatat_kernel_comparison(
            descriptor as u64,
            descriptor as u64,
            c"entry".as_ptr() as u64,
            0,
        );
        assert_eq!(result, 0);
        assert_eq!(std::fs::read(path).unwrap(), b"protected dirfd fixture");
    }

    #[test]
    fn owned_pread64_preserves_kernel_error_and_zero_length_ordering() {
        use std::io::Seek;
        use std::io::SeekFrom;
        use std::io::Write;

        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"data").unwrap();
        file.seek(SeekFrom::Start(2)).unwrap();
        let write_only = std::fs::OpenOptions::new()
            .write(true)
            .open(file.path())
            .unwrap();
        let fd = file.as_raw_fd() as u64;
        let write_fd = write_only.as_raw_fd() as u64;
        let mut buffer = [0xa5; 8];
        let address = buffer.as_mut_ptr() as u64;
        for (descriptor, pointer, count, offset, expected) in [
            (u64::MAX, address, 1, 0, -libc::EBADF),
            (write_fd, address, 1, 0, -libc::EBADF),
            (fd, address, 1, u64::MAX, -libc::EINVAL),
            (fd, 0, 1, 0, -libc::EFAULT),
            (fd, 0, 0, 0, 0),
            (u64::MAX, 0, 0, 0, -libc::EBADF),
            (write_fd, 0, 0, 0, -libc::EBADF),
            (fd, 0, 0, u64::MAX, -libc::EINVAL),
            (u64::MAX, 0, 0, u64::MAX, -libc::EINVAL),
            (fd, u64::MAX, 0, 0, -libc::EFAULT),
        ] {
            let args = [descriptor, pointer, count, offset, 0, 0];
            let direct = unsafe { raw_syscall6(libc::SYS_pread64, args) };
            assert_eq!(direct, i64::from(expected), "direct {args:x?}");
            assert_eq!(injected_syscall_guard(libc::SYS_pread64, args), None);
            let result = guarded_raw_injection(libc::SYS_pread64, args);
            assert_eq!(result, direct, "guarded {args:x?}");
            let converted = Errno::from_ret(result as usize).map(|value| value as i64);
            if expected < 0 {
                assert_eq!(converted.unwrap_err().into_raw(), -expected);
            } else {
                assert_eq!(converted.unwrap(), 0);
            }
            assert_eq!(buffer, [0xa5; 8]);
            assert_eq!(file.stream_position().unwrap(), 2);
        }
        drop(write_only);
        file.close().unwrap();
    }

    #[test]
    fn owned_pread64_protected_fd_is_refused_without_buffer_or_position_effects() {
        use std::io::Seek;
        use std::io::SeekFrom;
        use std::io::Write;
        use std::sync::atomic::Ordering;

        const CHILD: &str = "LITEINST_PREAD_PROTECTED_HOST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tool_host::tests::owned_pread64_protected_fd_is_refused_without_buffer_or_position_effects"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"private").unwrap();
        file.seek(SeekFrom::Start(2)).unwrap();
        let previous = crate::guest_log::LOG_FD.swap(file.as_raw_fd(), Ordering::AcqRel);
        for descriptor in [file.as_raw_fd() as u64, (1 << 32) | file.as_raw_fd() as u64] {
            for count in [0, 4] {
                let mut buffer = [0xa5; 8];
                let args = [descriptor, buffer.as_mut_ptr() as u64, count, 0, 0, 0];
                assert_eq!(
                    injected_syscall_guard(libc::SYS_pread64, args),
                    Some(-i64::from(libc::EBADF))
                );
                assert_eq!(
                    guarded_raw_injection(libc::SYS_pread64, args),
                    -i64::from(libc::EBADF)
                );
                assert_eq!(buffer, [0xa5; 8]);
                assert_eq!(file.stream_position().unwrap(), 2);
            }
        }
        crate::guest_log::LOG_FD.store(previous, Ordering::Release);
        let mut buffer = [0; 7];
        assert_eq!(
            guarded_raw_injection(
                libc::SYS_pread64,
                [
                    file.as_raw_fd() as u64,
                    buffer.as_mut_ptr() as u64,
                    7,
                    0,
                    0,
                    0
                ]
            ),
            7
        );
        assert_eq!(&buffer, b"private");
        assert_eq!(file.stream_position().unwrap(), 2);
        file.close().unwrap();
    }

    /// The auxiliary identity operations are injectable but not capturable, and
    /// the two classes must not be allowed to collapse into one another.
    #[test]
    fn injected_closure_is_wider_than_capture_and_capture_is_unchanged_for_aux() {
        for number in [libc::SYS_gettid, libc::SYS_getppid] {
            assert!(crate::syscall_event::injectable(number));
            assert!(
                !crate::syscall_event::backed_returning(number),
                "{number} must not become stageable from guest execution"
            );
        }
        for number in [
            libc::SYS_getpid,
            libc::SYS_read,
            libc::SYS_openat,
            libc::SYS_fstat,
            libc::SYS_close,
        ] {
            assert!(crate::syscall_event::backed_returning(number));
            assert!(crate::syscall_event::injectable(number));
        }
    }

    /// The terminal status must stay distinct from every status already in use,
    /// or the outcome stops being attributable from the exit status alone.
    #[test]
    fn unsupported_owned_injection_status_is_unshared() {
        for used in [120, 121, 122, 123, 124, 125, 126, 127] {
            assert_ne!(UNSUPPORTED_OWNED_INJECTION_STATUS, used);
        }
        assert_eq!(SetupStage::OwnedInjection.name(), "owned-injection");
    }

    /// The terminal outcome observed as an actual process outcome rather than a
    /// return value.
    ///
    /// This is the part a pure classifier cannot show: that the refused
    /// operation ends the process, with the distinct status and the retained
    /// stage and cause on stderr, and that no errno is delivered to anyone. The
    /// admitted cases in the same shape are the control — they reach the end of
    /// the child and exit normally, so the terminal is reached only for the
    /// refused class.
    ///
    /// Scope: this exercises the refusal and its reporting directly. No Tool is
    /// installed, no coordinator connected, no SUD enabled and no guest run, so
    /// it is not evidence about any Tool's behaviour.
    #[test]
    fn unsupported_owned_injection_terminates_before_effects() {
        let test = "tool_host::tests::unsupported_owned_injection_terminates_before_effects";
        if let Some(case) = owned_setup_case() {
            let number: i64 = case.parse().expect("the case is the classified number");
            match classify_owned_injection(true, number) {
                // SAFETY: nothing has been injected in this child and no
                // execution state is published, so the refusal is the only
                // observable effect.
                OwnedInjection::Terminal => unsafe {
                    terminate_unsupported_owned_injection(number)
                },
                OwnedInjection::Admitted => return,
            }
        }
        for number in [
            libc::SYS_pwritev2,
            libc::SYS_mmap,
            libc::SYS_execve,
            libc::SYS_preadv2,
            libc::SYS_statx,
            libc::SYS_close_range,
        ] {
            let child = owned_setup_child(test, &number.to_string());
            assert_eq!(
                child.status.code(),
                Some(UNSUPPORTED_OWNED_INJECTION_STATUS),
                "{number} must terminate, got {:?}",
                child.status
            );
            assert_eq!(
                String::from_utf8_lossy(&child.stderr),
                format!(
                    "hermit-liteinst owned injection refused: stage=owned-injection \
                     cause=unsupported-syscall number={number}\n"
                ),
                "{number} must retain its stage and cause"
            );
        }
        for number in [
            libc::SYS_getpid,
            libc::SYS_read,
            libc::SYS_openat,
            libc::SYS_fstat,
            libc::SYS_close,
            libc::SYS_gettid,
            libc::SYS_getppid,
        ] {
            let child = owned_setup_child(test, &number.to_string());
            assert_child_ok(&child, "admitted");
            assert_eq!(
                child.stderr,
                Vec::<u8>::new(),
                "{number} must produce no refusal diagnostic"
            );
        }
    }

    /// The refusal diagnostic must fit the budget: a truncated report would lose
    /// the number that identifies which operation was refused.
    #[test]
    fn unsupported_owned_injection_diagnostic_fits_its_budget() {
        use core::fmt::Write as _;
        let mut sink = TerminalDiagnostic::new();
        write!(
            sink,
            "hermit-liteinst owned injection refused: stage={} cause=unsupported-syscall number={}",
            SetupStage::OwnedInjection.name(),
            i64::MIN
        )
        .expect("widest refusal must render");
        assert!(!sink.exhausted());
    }

    #[cfg(feature = "test-owned-cpuid")]
    #[test]
    fn owned_compiled_admission_retains_public_and_lifecycle_refusals() {
        let admission = InstructionAdmission::OwnedCompiledStepFixture {
            evidence_bytes: 8192,
        };
        let mode = crate::SyscallMode::UserDispatchWithoutPatching;
        let mut subscriptions: reverie::Subscription =
            [Sysno::getpid, Sysno::read].into_iter().collect();
        subscriptions.cpuid().rdtsc();
        assert!(admit_subscriptions(mode, admission, None, &subscriptions).is_ok());
        assert!(
            admit_subscriptions(mode, InstructionAdmission::Public, None, &subscriptions).is_err()
        );
        assert!(
            admit_subscriptions(
                crate::SyscallMode::SeccompWithPatching,
                admission,
                None,
                &subscriptions
            )
            .is_err()
        );
        for number in [
            Sysno::fork,
            Sysno::execve,
            Sysno::clock_gettime,
            Sysno::mprotect,
        ] {
            assert!(
                admit_subscriptions(mode, admission, None, &[number].into_iter().collect())
                    .is_err()
            );
        }
    }

    #[cfg(feature = "test-owned-cpuid")]
    #[test]
    fn owned_syscall_admission_is_finite_and_not_public_instruction_admission() {
        let mode = crate::SyscallMode::UserDispatchWithoutPatching;
        let mut subscriptions: reverie::Subscription =
            [Sysno::getpid, Sysno::read].into_iter().collect();
        assert!(
            admit_subscriptions(
                mode,
                InstructionAdmission::OwnedSyscallTimerFixture,
                None,
                &subscriptions
            )
            .is_ok()
        );
        subscriptions.cpuid();
        assert!(
            admit_subscriptions(mode, InstructionAdmission::Public, None, &subscriptions).is_err()
        );
        assert!(
            admit_subscriptions(
                crate::SyscallMode::SeccompWithPatching,
                InstructionAdmission::OwnedSyscallTimerFixture,
                None,
                &subscriptions
            )
            .is_err()
        );
        for number in [
            Sysno::exit,
            Sysno::fork,
            Sysno::execve,
            Sysno::clock_gettime,
        ] {
            let subscriptions = [number].into_iter().collect();
            assert!(
                admit_subscriptions(
                    mode,
                    InstructionAdmission::OwnedSyscallTimerFixture,
                    None,
                    &subscriptions
                )
                .is_err()
            );
        }
    }

    #[test]
    fn owned_cpuid_public_subscriptions_remain_unsupported() {
        let mut subscriptions = reverie::Subscription::default();
        subscriptions.cpuid();
        assert_eq!(
            admit_subscriptions(
                crate::SyscallMode::UserDispatchWithoutPatching,
                InstructionAdmission::Public,
                None,
                &subscriptions
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Unsupported
        );
        assert!(
            admit_subscriptions(
                crate::SyscallMode::SeccompWithPatching,
                InstructionAdmission::Public,
                None,
                &subscriptions
            )
            .is_ok()
        );
    }

    #[cfg(feature = "test-owned-cpuid")]
    #[test]
    fn owned_cpuid_private_admission_is_cpuid_only() {
        let check = |subscriptions: &reverie::Subscription| {
            admit_subscriptions(
                crate::SyscallMode::UserDispatchWithoutPatching,
                InstructionAdmission::OwnedCpuidFixture,
                None,
                subscriptions,
            )
        };
        let mut subscriptions = reverie::Subscription::default();
        assert_eq!(
            check(&subscriptions).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        subscriptions.cpuid();
        assert!(check(&subscriptions).is_ok());
        assert_eq!(
            admit_subscriptions(
                crate::SyscallMode::SeccompWithPatching,
                InstructionAdmission::OwnedCpuidFixture,
                None,
                &subscriptions
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Unsupported
        );
        subscriptions.rdtsc();
        assert_eq!(
            check(&subscriptions).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        let mut subscriptions: reverie::Subscription = [Sysno::getpid].into_iter().collect();
        subscriptions.cpuid();
        assert_eq!(
            check(&subscriptions).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn owned_rdtsc_public_subscriptions_remain_unsupported() {
        let mut subscriptions = reverie::Subscription::default();
        subscriptions.rdtsc();
        for cpuid in [false, true] {
            if cpuid {
                subscriptions.cpuid();
            }
            assert_eq!(
                admit_subscriptions(
                    crate::SyscallMode::UserDispatchWithoutPatching,
                    InstructionAdmission::Public,
                    None,
                    &subscriptions
                )
                .unwrap_err()
                .kind(),
                io::ErrorKind::Unsupported
            );
        }
    }

    #[cfg(feature = "test-owned-cpuid")]
    #[test]
    fn owned_instruction_private_admission_is_instruction_only() {
        for admission in [
            InstructionAdmission::OwnedInstructionFixture,
            InstructionAdmission::OwnedClockedInstructionFixture,
            InstructionAdmission::OwnedSingleStepFixture,
            InstructionAdmission::OwnedPreciseTimerFixture,
        ] {
            for cpuid in [false, true] {
                for rdtsc in [false, true] {
                    let mut subscriptions = reverie::Subscription::default();
                    if cpuid {
                        subscriptions.cpuid();
                    }
                    if rdtsc {
                        subscriptions.rdtsc();
                    }
                    for mode in [
                        crate::SyscallMode::UserDispatchWithoutPatching,
                        crate::SyscallMode::SeccompWithPatching,
                    ] {
                        assert_eq!(
                            admit_subscriptions(mode, admission, None, &subscriptions).is_ok(),
                            mode == crate::SyscallMode::UserDispatchWithoutPatching
                                && (cpuid || rdtsc)
                        );
                    }
                }
            }
            for syscall in [Sysno::getpid, Sysno::clock_gettime, Sysno::getcpu] {
                let mut subscriptions: reverie::Subscription = [syscall].into_iter().collect();
                subscriptions.cpuid().rdtsc();
                assert!(
                    admit_subscriptions(
                        crate::SyscallMode::UserDispatchWithoutPatching,
                        admission,
                        None,
                        &subscriptions
                    )
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn owned_cpuid_register_replacement_refuses_without_mutation() {
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.rbp = 0x1234;
        context.stack_pointer = 0x4560;
        context.rflags = 0x202;
        context.instruction_pointer = 0x7890;
        let original = format!("{context:?}");
        let mut event = SyscallEvent {
            owned_binding: None,
            exit: None,
            number: 99,
            args: [1, 2, 3, 4, 5, 6],
            instruction_pointer: 0x7890,
            result: 42,
            context: (&raw mut context) as usize,
        };
        let original_event = (
            event.number,
            event.args,
            event.instruction_pointer,
            event.result,
            event.context,
            event.owned_binding,
        );
        for field in 0..6 {
            let mut registers: libc::user_regs_struct = unsafe { core::mem::zeroed() };
            match field {
                0 => registers.rbp = 0xbeef,
                1 => registers.rsp = 0xbeef,
                2 => registers.eflags = 0xbeef,
                3 => registers.rip = 0xbeef,
                4 => registers.rax = 0xbeef,
                5 => (),
                _ => unreachable!(),
            }
            assert!(matches!(
                set_guest_registers(&mut event, registers, true),
                Err(Error::Errno(Errno::EOPNOTSUPP))
            ));
            assert_eq!(format!("{context:?}"), original);
            assert_eq!(
                (
                    event.number,
                    event.args,
                    event.instruction_pointer,
                    event.result,
                    event.context,
                    event.owned_binding,
                ),
                original_event
            );
            assert!(event.exit.is_none());
        }
        let mut registers: libc::user_regs_struct = unsafe { core::mem::zeroed() };
        registers.rbp = 0xbeef;
        registers.rsp = 0x1000;
        registers.eflags = 0x246;
        registers.rax = 123;
        set_guest_registers(&mut event, registers, false).unwrap();
        assert_eq!(
            (
                context.rbp,
                context.stack_pointer,
                context.rflags,
                context.rax
            ),
            (0xbeef, 0x1000, 0x246, 123)
        );
        assert_eq!(event.number, 123);
    }

    #[test]
    fn guest_log_direct_injection_preserves_neighboring_operations() {
        if std::env::var_os("LITEINST_LOG_INJECTION_TEST").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tool_host::tests::guest_log_direct_injection_preserves_neighboring_operations",
                ])
                .env("LITEINST_LOG_INJECTION_TEST", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let (log, _log_peer) = crate::guest_log::channel_pair().unwrap();
        crate::guest_log::LOG_FD.store(log.as_raw_fd(), std::sync::atomic::Ordering::Release);
        let (neighbor, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        for (fd, expected) in [
            (log.as_raw_fd(), -i64::from(libc::EBADF)),
            (neighbor.as_raw_fd(), 1),
        ] {
            let result = guarded_raw_injection(
                libc::SYS_write,
                [fd as u64, b"x".as_ptr() as u64, 1, 0, 0, 0],
            );
            assert_eq!(result, expected);
        }
        let mut byte = [0];
        std::io::Read::read_exact(&mut peer, &mut byte).unwrap();
        assert_eq!(byte, *b"x");
        assert_eq!(
            guarded_raw_injection(libc::SYS_close, [log.as_raw_fd() as u64, 0, 0, 0, 0, 0]),
            0
        );
        assert!(unsafe { libc::fcntl(log.as_raw_fd(), libc::F_GETFD) } >= 0);
        let mapping = guarded_raw_injection(
            libc::SYS_mmap,
            [
                0,
                4096,
                libc::PROT_READ as u64,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
                log.as_raw_fd() as u64,
                0,
            ],
        );
        assert!(mapping > 0);
        assert_eq!(
            guarded_raw_injection(libc::SYS_munmap, [mapping as u64, 4096, 0, 0, 0, 0]),
            0
        );
        assert_eq!(
            guarded_raw_injection(
                libc::SYS_mmap,
                [
                    0,
                    4096,
                    libc::PROT_READ as u64,
                    libc::MAP_PRIVATE as u64,
                    log.as_raw_fd() as u64,
                    0
                ]
            ),
            -i64::from(libc::EBADF)
        );
    }

    #[test]
    fn public_owned_setup_is_one_fixed_native_shape() {
        let setup = OwnedSetup::public_native(8192);
        assert_eq!(
            setup,
            OwnedSetup {
                clocked: true,
                single_step: true,
                precise_timer: true,
                syscalls: true,
                native_evidence: Some(8192),
            }
        );
        assert!(
            setup.native_evidence.is_some()
                && setup.clocked
                && setup.single_step
                && setup.precise_timer
                && setup.syscalls
        );
        let larger = OwnedSetup::public_native(65536);
        assert_eq!(larger.native_evidence, Some(65536));
        assert_eq!(
            OwnedSetup {
                native_evidence: setup.native_evidence,
                ..larger
            },
            setup
        );
    }

    #[test]
    fn public_install_without_owned_setup_keeps_register_writes_available() {
        assert!(!instruction_results_only(
            InstructionAdmission::Public,
            None
        ));
    }

    #[test]
    fn every_owned_route_refuses_guest_register_writes() {
        assert!(instruction_results_only(
            InstructionAdmission::Public,
            Some(OwnedSetup::public_native(8192))
        ));
        let mut event = SyscallEvent {
            owned_binding: None,
            exit: None,
            number: libc::SYS_getpid,
            args: [0; 6],
            instruction_pointer: 0,
            result: 0,
            context: 0,
        };
        let regs: libc::user_regs_struct = unsafe { core::mem::zeroed() };
        assert!(set_guest_registers(&mut event, regs, true).is_err());
        assert_eq!(event.number, libc::SYS_getpid);
        assert!(set_guest_registers(&mut event, regs, false).is_ok());
        assert_eq!(event.number, 0);
    }

    #[cfg(feature = "test-owned-cpuid")]
    #[test]
    fn owned_setup_derivation_reproduces_every_fixture_admission() {
        for admission in [
            InstructionAdmission::Public,
            InstructionAdmission::OwnedCpuidFixture,
            InstructionAdmission::OwnedInstructionFixture,
            InstructionAdmission::OwnedClockedInstructionFixture,
            InstructionAdmission::OwnedSingleStepFixture,
            InstructionAdmission::OwnedPreciseTimerFixture,
            InstructionAdmission::OwnedSyscallTimerFixture,
            InstructionAdmission::OwnedCompiledStepFixture {
                evidence_bytes: 4096,
            },
        ] {
            let expected = admission.results_only().then(|| OwnedSetup {
                clocked: admission.counting_only(),
                single_step: admission.single_step(),
                precise_timer: admission.precise_timer(),
                syscalls: admission.owned_syscalls(),
                native_evidence: admission.native_evidence(),
            });
            assert_eq!(admission.owned_setup(), expected);
            assert_eq!(
                instruction_results_only(admission, admission.owned_setup()),
                admission.results_only()
            );
        }
        assert_eq!(InstructionAdmission::Public.owned_setup(), None);
        assert_eq!(
            InstructionAdmission::OwnedCompiledStepFixture {
                evidence_bytes: 4096
            }
            .owned_setup(),
            Some(OwnedSetup {
                clocked: true,
                single_step: true,
                precise_timer: true,
                syscalls: true,
                native_evidence: Some(4096),
            })
        );
    }

    #[test]
    fn public_gate_rejects_instruction_and_vdso_before_the_owned_syscall_bound() {
        let mode = crate::SyscallMode::UserDispatchWithoutPatching;
        for extra in [Sysno::clock_gettime, Sysno::getcpu, Sysno::gettimeofday] {
            let subscriptions: reverie::Subscription =
                [Sysno::getpid, Sysno::read, extra].into_iter().collect();
            assert!(
                admit_subscriptions(mode, InstructionAdmission::Public, None, &subscriptions)
                    .is_err(),
                "{extra:?} must be refused by the unchanged public gate"
            );
        }
        let mut instruction: reverie::Subscription = [Sysno::getpid].into_iter().collect();
        instruction.cpuid();
        assert!(
            admit_subscriptions(mode, InstructionAdmission::Public, None, &instruction).is_err(),
            "CPUID must be refused by the unchanged public gate"
        );
        let mut rdtsc: reverie::Subscription = [Sysno::getpid].into_iter().collect();
        rdtsc.rdtsc();
        assert!(admit_subscriptions(mode, InstructionAdmission::Public, None, &rdtsc).is_err());
        // The owned syscall bound no longer repeats the instruction refusal
        // above; it now refuses an owned setup short of the complete native
        // shape, and accepts these sets only with that shape.
        for incomplete in super::public_owned_native::INCOMPLETE_PUBLIC_NATIVE {
            assert!(admit_public_owned_syscalls(mode, incomplete, &instruction).is_err());
            assert!(admit_public_owned_syscalls(mode, incomplete, &rdtsc).is_err());
        }
        let complete = OwnedSetup::public_native(8192);
        assert!(admit_public_owned_syscalls(mode, complete, &instruction).is_ok());
        assert!(admit_public_owned_syscalls(mode, complete, &rdtsc).is_ok());
    }

    #[test]
    fn public_owned_observation_policy_keeps_nonempty_native_requirement() {
        let mode = crate::SyscallMode::UserDispatchWithoutPatching;
        for accepted in [
            vec![Sysno::getpid],
            vec![Sysno::read],
            vec![Sysno::getpid, Sysno::read],
            vec![Sysno::openat],
            vec![Sysno::fstat],
            vec![Sysno::close],
            vec![
                Sysno::getpid,
                Sysno::read,
                Sysno::openat,
                Sysno::fstat,
                Sysno::close,
            ],
        ] {
            for number in &accepted {
                assert!(
                    crate::syscall_event::backed_returning(*number as i64),
                    "the bound must admit only capturable numbers, not {number:?}"
                );
            }
            let subscriptions: reverie::Subscription = accepted.iter().copied().collect();
            assert!(
                admit_subscriptions(mode, InstructionAdmission::Public, None, &subscriptions)
                    .is_ok()
            );
            assert!(
                admit_public_owned_syscalls(mode, OwnedSetup::public_native(8192), &subscriptions)
                    .is_ok(),
                "{accepted:?} must be accepted"
            );
        }
        assert!(
            admit_public_owned_syscalls(
                mode,
                OwnedSetup::public_native(8192),
                &reverie::Subscription::default()
            )
            .is_err()
        );
        for refused in [
            Sysno::write,
            Sysno::exit,
            Sysno::execve,
            Sysno::gettid,
            Sysno::getppid,
            Sysno::open,
            Sysno::newfstatat,
            Sysno::lseek,
            Sysno::fcntl,
        ] {
            assert!(
                !crate::syscall_event::backed_returning(refused as i64),
                "{refused:?} is outside the unchanged finite fixture effect set"
            );
            let subscriptions: reverie::Subscription =
                [Sysno::getpid, refused].into_iter().collect();
            assert!(
                admit_public_owned_syscalls(mode, OwnedSetup::public_native(8192), &subscriptions)
                    .is_ok(),
                "{refused:?} entry observation does not execute its kernel effect"
            );
            if !crate::syscall_event::injectable(refused as i64) {
                assert_eq!(
                    classify_owned_injection(true, refused as i64),
                    OwnedInjection::Terminal
                );
            }
        }
        let capturable: reverie::Subscription = [Sysno::getpid, Sysno::read].into_iter().collect();
        assert!(
            admit_public_owned_syscalls(
                crate::SyscallMode::SeccompWithPatching,
                OwnedSetup::public_native(8192),
                &capturable
            )
            .is_err()
        );
    }

    #[test]
    fn full_observation_policy_still_refuses_unowned_or_incomplete_setup() {
        let subscriptions = reverie::Subscription::all();
        let mode = crate::SyscallMode::UserDispatchWithoutPatching;
        assert!(
            admit_subscriptions(mode, InstructionAdmission::Public, None, &subscriptions).is_err()
        );
        assert!(
            admit_subscriptions(
                mode,
                InstructionAdmission::Public,
                Some(OwnedSetup::public_native(8192)),
                &subscriptions
            )
            .is_ok()
        );
        assert!(
            admit_public_owned_syscalls(mode, OwnedSetup::public_native(8192), &subscriptions)
                .is_ok()
        );
        for incomplete in super::public_owned_native::INCOMPLETE_PUBLIC_NATIVE {
            assert!(admit_public_owned_syscalls(mode, incomplete, &subscriptions).is_err());
        }
    }

    const OWNED_SETUP_CHILD: &str = "REVERIE_LITEINST_OWNED_SETUP_CHILD";

    /// Runs one case of `test` in a fresh child of this same test binary.
    ///
    /// The observations below mutate process-global `OnceLock`s and the shared
    /// clock control block, so each case needs its own process. The child never
    /// installs a Tool, enables SUD or TF, runs a guest, or loads a DSO.
    fn owned_setup_child(test: &str, case: &str) -> std::process::Output {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--test-threads=1", "--nocapture"])
            .env(OWNED_SETUP_CHILD, case)
            .output()
            .unwrap()
    }

    fn owned_setup_case() -> Option<String> {
        std::env::var(OWNED_SETUP_CHILD).ok()
    }

    fn assert_child_ok(output: &std::process::Output, case: &str) {
        assert!(
            output.status.success(),
            "{case}: {:?}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Every global the owned setup would publish, as actually observed.
    fn observed_globals() -> [bool; 8] {
        [
            crate::syscall_mode::sud_only(),
            reverie_preload::signal::owned_trace::configured(),
            reverie_preload::signal::runtime_signals_configured(),
            crate::clock_control::active(),
            crate::clock_control::notification_free(),
            crate::clock_control::handoff_clear(),
            crate::owned_context::owned_state_published(),
            crate::timer::owned_controller_installed(),
        ]
    }

    const QUIESCENT_GLOBALS: [bool; 8] = [false, false, false, false, true, true, false, false];

    #[test]
    fn refused_owned_setup_publishes_no_global_state() {
        let test = "tool_host::tests::refused_owned_setup_publishes_no_global_state";
        let Some(case) = owned_setup_case() else {
            assert_child_ok(&owned_setup_child(test, "clean_refusal"), "clean_refusal");
            return;
        };
        assert_eq!(case, "clean_refusal");
        assert_eq!(observed_globals(), QUIESCENT_GLOBALS);
        for _ in 0..2 {
            assert!(crate::owned_context::admit_clock_setup(true).is_err());
            assert_eq!(observed_globals(), QUIESCENT_GLOBALS);
        }
        assert!(crate::owned_context::admit_clock_setup(false).is_ok());
        assert_eq!(observed_globals(), QUIESCENT_GLOBALS);
    }

    #[test]
    fn owned_controller_refuses_until_the_counter_is_active_and_paused() {
        let test =
            "tool_host::tests::owned_controller_refuses_until_the_counter_is_active_and_paused";
        let Some(case) = owned_setup_case() else {
            assert_child_ok(&owned_setup_child(test, "controller"), "controller");
            return;
        };
        assert_eq!(case, "controller");
        use crate::clock_control::test_control;
        let saved = test_control::snapshot();
        assert!(!crate::timer::owned_controller_installed());
        for (name, ready, running, armed) in [
            ("inactive counter", 0, 0, 0),
            ("running counter", 1, 1, 0),
            ("armed notification", 1, 0, 1),
        ] {
            test_control::set_ready(ready);
            test_control::set_running(running);
            test_control::set_notification_armed(armed);
            assert!(crate::timer::initialize_owned().is_err(), "{name}");
            assert!(
                !crate::timer::owned_controller_installed(),
                "{name} installed a controller"
            );
        }
        test_control::set_ready(1);
        test_control::set_running(0);
        test_control::set_notification_armed(0);
        assert!(crate::timer::initialize_owned().is_ok());
        assert!(crate::timer::owned_controller_installed());
        assert!(crate::timer::initialize_owned().is_err());
        assert!(crate::timer::owned_controller_installed());
        assert!(crate::timer::request_owned(TimerSchedule::Rcbs(1), true).is_err());
        test_control::restore(saved);
    }

    #[test]
    fn runtime_signal_registration_blocks_owned_trace_without_publishing_it() {
        let test = "tool_host::tests::runtime_signal_registration_blocks_owned_trace_without_publishing_it";
        let Some(case) = owned_setup_case() else {
            assert_child_ok(&owned_setup_child(test, "signals_first"), "signals_first");
            return;
        };
        assert_eq!(case, "signals_first");
        assert_eq!(observed_globals(), QUIESCENT_GLOBALS);
        unsafe { reverie_preload::signal::configure_runtime_signals(&[]) }.unwrap();
        assert!(reverie_preload::signal::runtime_signals_configured());
        assert!(!reverie_preload::signal::owned_trace::configured());
        assert!(
            unsafe {
                reverie_preload::signal::owned_trace::configure(crate::owned_context::signal_entry)
            }
            .is_err()
        );
        assert!(!reverie_preload::signal::owned_trace::configured());
        assert!(reverie_preload::signal::runtime_signals_configured());
        assert!(!crate::owned_context::owned_state_published());
        assert!(!crate::timer::owned_controller_installed());
        assert!(crate::owned_context::admit_clock_setup(true).is_err());
        assert!(crate::owned_context::admit_clock_setup(false).is_err());
    }

    #[test]
    fn owned_trace_registration_blocks_runtime_signals_without_publishing_them() {
        let test = "tool_host::tests::owned_trace_registration_blocks_runtime_signals_without_publishing_them";
        let Some(case) = owned_setup_case() else {
            assert_child_ok(&owned_setup_child(test, "trace_first"), "trace_first");
            return;
        };
        assert_eq!(case, "trace_first");
        assert_eq!(observed_globals(), QUIESCENT_GLOBALS);
        unsafe {
            reverie_preload::signal::owned_trace::configure(crate::owned_context::signal_entry)
        }
        .unwrap();
        assert!(reverie_preload::signal::owned_trace::configured());
        assert!(unsafe { reverie_preload::signal::configure_runtime_signals(&[]) }.is_err());
        assert!(!reverie_preload::signal::runtime_signals_configured());
        assert!(!crate::owned_context::owned_state_published());
        assert!(!crate::timer::owned_controller_installed());
        assert!(crate::owned_context::admit_clock_setup(false).is_err());
    }

    #[test]
    fn terminal_guard_returns_for_every_non_clocked_sud_route() {
        let test = "tool_host::tests::terminal_guard_returns_for_every_non_clocked_sud_route";
        let Some(case) = owned_setup_case() else {
            for case in ["sud_unclocked", "clocked_not_sud"] {
                assert_child_ok(&owned_setup_child(test, case), case);
            }
            return;
        };
        use crate::clock_control::test_control;
        let saved = test_control::snapshot();
        let (mode, requested) = match case.as_str() {
            "sud_unclocked" => (crate::SyscallMode::UserDispatchWithoutPatching, 0),
            "clocked_not_sud" => (crate::SyscallMode::SeccompWithPatching, 1),
            other => panic!("unknown case {other}"),
        };
        crate::syscall_mode::select(mode);
        test_control::set_requested(requested);
        let mask_before = current_signal_mask();
        let error = crate::timer::initialize_owned()
            .err()
            .expect("an inactive counter must refuse the precise controller");
        unsafe { terminate_after_publication(SetupStage::PreciseController, &error) };
        assert_eq!(current_signal_mask(), mask_before);
        assert!(!crate::timer::owned_controller_installed());
        test_control::restore(saved);
        crate::syscall_mode::select(crate::SyscallMode::SeccompWithPatching);
    }

    fn current_signal_mask() -> u64 {
        let mut mask = 0_u64;
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [0, 0, (&raw mut mask) as u64, 8, 0, 0],
            )
        };
        assert_eq!(result, 0, "could not read the signal mask");
        mask
    }

    fn block_signal(signal: i32) -> u64 {
        let add = 1_u64 << (signal - 1);
        let mut previous = 0_u64;
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_BLOCK as u64,
                    (&raw const add) as u64,
                    (&raw mut previous) as u64,
                    8,
                    0,
                    0,
                ],
            )
        };
        assert_eq!(result, 0, "could not block signal {signal}");
        previous | add
    }

    /// No execution state is published by a refused preparation: no owned
    /// context, counter, precise controller or Tool handler.
    ///
    /// Two things that *are* published are asserted rather than left implied: the
    /// selected syscall mode, and the constructor's `requested` flag. A successful
    /// owned-trace policy reservation may also persist; where that applies it is
    /// asserted separately by the caller, since it depends on which preparation
    /// step refused.
    fn assert_no_execution_state_published() {
        assert!(crate::syscall_mode::sud_only());
        assert!(crate::clock_control::requested());
        assert!(!crate::owned_context::owned_state_published());
        assert!(!crate::timer::owned_controller_installed());
        assert!(!crate::clock_control::active());
        assert!(HANDLER.get().is_none());
    }

    /// Puts the child in the exact state the discarding branch keyed on: SUD
    /// selected and the constructor's `requested` flag set. Revision 04 fired the
    /// terminal predicate here and exited 127; revision 05 removed that branch. A
    /// child reaching the end of one of these tests is the regression evidence.
    fn arm_clocked_sud_preconditions() {
        use crate::clock_control::test_control;
        test_control::set_requested(1);
        test_control::set_ready(0);
        test_control::set_running(0);
        test_control::set_notification_armed(0);
        crate::syscall_mode::select(crate::SyscallMode::UserDispatchWithoutPatching);
        assert!(crate::clock_control::requested());
        assert!(crate::syscall_mode::sud_only());
    }

    fn signal_disposition(signal: i32) -> u64 {
        let mut action = [0_u64; 4];
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [signal as u64, 0, action.as_mut_ptr() as u64, 8, 0, 0],
            )
        };
        assert_eq!(result, 0, "could not read the disposition of {signal}");
        action[0]
    }

    fn reset_disposition(signal: i32) {
        let action = [libc::SIG_DFL as u64, 0, 0, 0];
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [signal as u64, action.as_ptr() as u64, 0, 8, 0, 0],
            )
        };
        assert_eq!(result, 0, "could not reset the disposition of {signal}");
    }

    const OWNED_SUD: (crate::SyscallMode, Option<OwnedSetup>) = (
        crate::SyscallMode::UserDispatchWithoutPatching,
        Some(OwnedSetup {
            clocked: true,
            single_step: true,
            precise_timer: true,
            syscalls: true,
            native_evidence: Some(8192),
        }),
    );

    #[test]
    fn preparation_returns_the_original_conflicting_trace_source_error() {
        let test =
            "tool_host::tests::preparation_returns_the_original_conflicting_trace_source_error";
        let Some(case) = owned_setup_case() else {
            assert_child_ok(&owned_setup_child(test, "trace_conflict"), "trace_conflict");
            return;
        };
        assert_eq!(case, "trace_conflict");
        arm_clocked_sud_preconditions();
        unsafe { reverie_preload::signal::configure_runtime_signals(&[]) }.unwrap();
        let mask_before = current_signal_mask();
        let (mode, owned) = OWNED_SUD;
        let error = prepare_owned_signal_state(
            mode,
            owned,
            runtime::InstructionSubscriptions {
                cpuid: false,
                rdtsc: false,
            },
        )
        .err()
        .expect("a conflicting runtime source must refuse");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(error.raw_os_error(), None);
        assert_eq!(error.to_string(), "runtime source conflict");
        assert_eq!(current_signal_mask(), mask_before);
        assert!(reverie_preload::signal::runtime_signals_configured());
        assert!(!reverie_preload::signal::owned_trace::configured());
        assert_no_execution_state_published();
    }

    #[test]
    fn preparation_returns_the_original_signal_state_error_and_keeps_the_trace_reservation() {
        let test = "tool_host::tests::preparation_returns_the_original_signal_state_error_and_keeps_the_trace_reservation";
        let Some(case) = owned_setup_case() else {
            assert_child_ok(&owned_setup_child(test, "signal_state"), "signal_state");
            return;
        };
        assert_eq!(case, "signal_state");
        arm_clocked_sud_preconditions();
        let mask_before = block_signal(libc::SIGSYS);
        let (mode, owned) = OWNED_SUD;
        let error = prepare_owned_signal_state(
            mode,
            owned,
            runtime::InstructionSubscriptions {
                cpuid: false,
                rdtsc: false,
            },
        )
        .err()
        .expect("a blocked SIGSYS must refuse guest signal preparation");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(error.raw_os_error(), None);
        assert_eq!(
            error.to_string(),
            "SUD-only requires unblocked owned runtime signals"
        );
        assert_eq!(current_signal_mask(), mask_before);
        assert!(reverie_preload::signal::owned_trace::configured());
        assert!(!reverie_preload::signal::runtime_signals_configured());
        assert_no_execution_state_published();
    }

    #[test]
    fn preparation_returns_the_original_blocked_trace_source_error() {
        let test = "tool_host::tests::preparation_returns_the_original_blocked_trace_source_error";
        let Some(case) = owned_setup_case() else {
            assert_child_ok(&owned_setup_child(test, "trace_blocked"), "trace_blocked");
            return;
        };
        assert_eq!(case, "trace_blocked");
        arm_clocked_sud_preconditions();
        let mask_before = block_signal(libc::SIGTRAP);
        let (mode, owned) = OWNED_SUD;
        let error = prepare_owned_signal_state(
            mode,
            owned,
            runtime::InstructionSubscriptions {
                cpuid: false,
                rdtsc: false,
            },
        )
        .err()
        .expect("a blocked SIGTRAP must refuse the owned trace source");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(error.raw_os_error(), None);
        assert_eq!(error.to_string(), "unavailable owned trace source");
        assert_eq!(current_signal_mask(), mask_before);
        assert!(!reverie_preload::signal::owned_trace::configured());
        assert_no_execution_state_published();
    }

    /// A stand-in initializer shaped like the private fixture's: it calls the
    /// real shared preparation itself, matches its real `Result`, reports the
    /// actual `Err` through the same trusted raw write the fixture uses, and
    /// returns 42 to its caller.
    ///
    /// Scope, stated plainly: this is a lower-layer real preparation caller. It
    /// is **not** a full public RPC installation and **not** the original
    /// fixture executing — no coordinator is connected, no Tool installed, no SUD
    /// enabled, no guest run. Tying the original fixture's own reporting and its
    /// 42 -> assembly 127 path still requires the linked audit.
    fn reporting_initializer() -> i32 {
        use core::fmt::Write as _;
        let (mode, owned) = OWNED_SUD;
        match prepare_owned_signal_state(
            mode,
            owned,
            runtime::InstructionSubscriptions {
                cpuid: false,
                rdtsc: false,
            },
        ) {
            Ok(_guard) => 1,
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
                assert_eq!(error.raw_os_error(), None);
                assert_eq!(
                    error.to_string(),
                    "SUD-only requires unblocked owned runtime signals"
                );
                assert_no_execution_state_published();
                let mut sink = TerminalDiagnostic::new();
                let _ = write!(sink, "fixture-style installation failed: {error}");
                let _ = sink.finish();
                42
            }
        }
    }

    #[test]
    fn preparation_error_reaches_a_caller_that_reports_it_and_returns_42() {
        let test =
            "tool_host::tests::preparation_error_reaches_a_caller_that_reports_it_and_returns_42";
        let Some(case) = owned_setup_case() else {
            let child = owned_setup_child(test, "caller");
            assert_child_ok(&child, "caller");
            assert_eq!(
                String::from_utf8_lossy(&child.stderr),
                "fixture-style installation failed: SUD-only requires unblocked owned runtime signals\n"
            );
            return;
        };
        assert_eq!(case, "caller");
        arm_clocked_sud_preconditions();
        block_signal(libc::SIGSYS);
        let returned = reporting_initializer();
        assert_eq!(returned, 42, "the caller must continue and return 42");
        assert_no_execution_state_published();
    }

    #[test]
    fn raw_write_retry_is_finite_and_resumes_on_eintr_and_short_writes() {
        let mut seen: Vec<usize> = Vec::new();
        let mut emitted: Vec<u8> = Vec::new();
        let mut step = 0;
        let result = drain_with(b"abcdefgh", DIAGNOSTIC_WRITE_ATTEMPTS, |chunk| {
            seen.push(chunk.len());
            step += 1;
            let accepted = match step {
                1 => -i64::from(libc::EINTR),
                2 => 3,
                3 => -i64::from(libc::EINTR),
                _ => chunk.len() as i64,
            };
            if accepted > 0 {
                emitted.extend_from_slice(&chunk[..accepted as usize]);
            }
            accepted
        });
        assert_eq!(result, Ok(()));
        assert_eq!(seen, vec![8, 8, 5, 5]);
        assert_eq!(emitted, b"abcdefgh".to_vec());

        let mut attempts = 0;
        assert_eq!(
            drain_with(b"abc", DIAGNOSTIC_WRITE_ATTEMPTS, |_| {
                attempts += 1;
                -i64::from(libc::EINTR)
            }),
            Err(SinkFailure::AttemptsExhausted)
        );
        assert_eq!(attempts, DIAGNOSTIC_WRITE_ATTEMPTS);

        let mut short = 0;
        assert_eq!(
            drain_with(b"abcdef", 3, |_| {
                short += 1;
                1
            }),
            Err(SinkFailure::AttemptsExhausted)
        );
        assert_eq!(short, 3);

        for errno in [libc::EPIPE, libc::EBADF, libc::EIO] {
            assert_eq!(
                drain_with(b"abc", DIAGNOSTIC_WRITE_ATTEMPTS, |_| -i64::from(errno)),
                Err(SinkFailure::Errno(errno))
            );
        }
        assert_eq!(
            drain_with(b"abc", DIAGNOSTIC_WRITE_ATTEMPTS, |_| 0),
            Err(SinkFailure::Errno(libc::EIO))
        );
        assert_eq!(
            drain_with(b"", DIAGNOSTIC_WRITE_ATTEMPTS, |_| panic!(
                "empty input must not write"
            )),
            Ok(())
        );
    }

    /// A payload whose `Display` fails, to drive the `format-failed=1` path.
    #[derive(Debug)]
    struct UnformattableCause;

    impl std::fmt::Display for UnformattableCause {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            Err(std::fmt::Error)
        }
    }

    impl std::error::Error for UnformattableCause {}

    #[test]
    fn terminal_diagnostic_bounds_output_and_renders_exact_causes() {
        let test = "tool_host::tests::terminal_diagnostic_bounds_output_and_renders_exact_causes";
        let Some(case) = owned_setup_case() else {
            for case in [
                "long_cause",
                "os_error",
                "errno_cause",
                "nested_io",
                "nested_string",
                "nested_errno",
                "nested_nested_raw",
                "depth_exceeded",
                "format_error",
            ] {
                assert_child_ok(&owned_setup_child(test, case), case);
            }
            return;
        };
        let (stage, error): (SetupStage, io::Error) = match case.as_str() {
            "long_cause" => (
                SetupStage::OwnedContext,
                io::Error::new(io::ErrorKind::Unsupported, "y".repeat(4096)),
            ),
            "os_error" => (
                SetupStage::RcbClock,
                io::Error::from_raw_os_error(libc::ENODEV),
            ),
            "errno_cause" => (SetupStage::RcbClock, io::Error::other(Errno::EBUSY)),
            "nested_io" => (
                SetupStage::ToolHandler,
                io::Error::other(io::Error::from_raw_os_error(libc::EACCES)),
            ),
            "nested_string" => (
                SetupStage::ToolHandler,
                io::Error::other(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Reverie tool installed twice",
                )),
            ),
            "nested_errno" => (
                SetupStage::RcbClock,
                io::Error::other(io::Error::other(Errno::EBUSY)),
            ),
            "nested_nested_raw" => (
                SetupStage::RcbClock,
                io::Error::other(io::Error::other(io::Error::from_raw_os_error(libc::EACCES))),
            ),
            "depth_exceeded" => (
                SetupStage::OwnedContext,
                io::Error::other(io::Error::other(io::Error::other(io::Error::other(
                    io::Error::from_raw_os_error(libc::EACCES),
                )))),
            ),
            "format_error" => (SetupStage::Timer, io::Error::other(UnformattableCause)),
            other => panic!("unknown case {other}"),
        };
        let other_kind = io::Error::other("x").kind();
        let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let saved = redirect_stderr(writer.as_raw_fd());
        let failure = report_terminal_setup_failure(stage, &error);
        restore_stderr(saved);
        drop(writer);
        let mut text = String::new();
        std::io::Read::read_to_string(&mut { reader }, &mut text).unwrap();
        assert_eq!(
            failure, None,
            "{case}: the sink must have accepted the line"
        );
        assert!(text.ends_with('\n'), "{case}: {text}");
        let expected = match case.as_str() {
            "long_cause" => {
                assert!(
                    text.starts_with(
                        "hermit-liteinst owned setup failed: stage=owned-context \
                         kind=Unsupported errno=none cause0=yyy"
                    ),
                    "{text}"
                );
                assert!(text.ends_with(" truncated=1\n"), "{text}");
                assert!(!text.contains("format-failed=1"), "{text}");
                assert!(
                    text.len() <= DIAGNOSTIC_BYTE_BUDGET + 32,
                    "budget exceeded: {}",
                    text.len()
                );
                return;
            }
            "os_error" => format!(
                "hermit-liteinst owned setup failed: stage=rcb-clock kind={:?} errno={}\n",
                io::Error::from_raw_os_error(libc::ENODEV).kind(),
                libc::ENODEV
            ),
            "errno_cause" => format!(
                "hermit-liteinst owned setup failed: stage=rcb-clock kind={other_kind:?} \
                 errno=none cause0=errno {}\n",
                Errno::EBUSY.into_raw()
            ),
            "nested_io" => format!(
                "hermit-liteinst owned setup failed: stage=tool-handler kind={other_kind:?} \
                 errno=none cause0=io kind={:?} errno={}\n",
                io::Error::from_raw_os_error(libc::EACCES).kind(),
                libc::EACCES
            ),
            "nested_string" => format!(
                "hermit-liteinst owned setup failed: stage=tool-handler kind={other_kind:?} \
                 errno=none cause0=io kind={:?} errno=none \
                 cause1=Reverie tool installed twice\n",
                io::ErrorKind::AlreadyExists
            ),
            "nested_errno" => format!(
                "hermit-liteinst owned setup failed: stage=rcb-clock kind={other_kind:?} \
                 errno=none cause0=io kind={other_kind:?} errno=none cause1=errno {}\n",
                Errno::EBUSY.into_raw()
            ),
            "nested_nested_raw" => format!(
                "hermit-liteinst owned setup failed: stage=rcb-clock kind={other_kind:?} \
                 errno=none cause0=io kind={other_kind:?} errno=none \
                 cause1=io kind={:?} errno={}\n",
                io::Error::from_raw_os_error(libc::EACCES).kind(),
                libc::EACCES
            ),
            "depth_exceeded" => format!(
                "hermit-liteinst owned setup failed: stage=owned-context kind={other_kind:?} \
                 errno=none cause0=io kind={other_kind:?} errno=none \
                 cause1=io kind={other_kind:?} errno=none \
                 cause2=io kind={other_kind:?} errno=none cause-depth-exceeded=1\n"
            ),
            "format_error" => format!(
                "hermit-liteinst owned setup failed: stage=timer kind={other_kind:?} \
                 errno=none cause0= format-failed=1\n"
            ),
            other => panic!("unknown case {other}"),
        };
        assert_eq!(text, expected, "{case}");
    }

    /// The one further signal this libtest process was **observed** to carry a
    /// handler for, which real SUD preparation refuses with
    /// `SUD-only does not admit existing handler for signal 33`.
    ///
    /// This is observed host libtest setup for this isolated child. It is not a
    /// production exemption, not a general real-time signal-range clear, and not
    /// any loader or guest qualification. The observation does not identify which
    /// library owns the handler, and nothing here asserts one.
    const OBSERVED_LIBTEST_HANDLER_SIGNAL: i32 = 33;

    /// Test-only setup, disclosed rather than implied:
    ///
    /// - `test_control::set_ready(1)` writes a control-block word to make the
    ///   counter read as active. **No physical counter exists and none is
    ///   created**; `timer::initialize_owned` builds only a `Controller::default`
    ///   plus a `gettid`, and no perf event, SUD or TF is ever installed.
    /// - Resetting this child's SIGSEGV, SIGBUS, SIGPIPE and
    ///   [`OBSERVED_LIBTEST_HANDLER_SIGNAL`] dispositions is host-test-only setup
    ///   so the real SUD preparation can run in a libtest process. It widens no
    ///   public admission and changes no production policy.
    #[test]
    fn published_controller_failure_terminates_at_127_through_every_sink() {
        let test =
            "tool_host::tests::published_controller_failure_terminates_at_127_through_every_sink";
        let Some(case) = owned_setup_case() else {
            let working = owned_setup_child(test, "working_sink");
            assert_eq!(
                working.status.code(),
                Some(127),
                "working sink must exit 127; status {:?}\nstdout:\n{}\nstderr:\n{}",
                working.status,
                String::from_utf8_lossy(&working.stdout),
                String::from_utf8_lossy(&working.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&working.stderr),
                "hermit-liteinst owned setup failed: stage=precise-controller \
                 kind=Other errno=none cause0=precise controller already installed\n"
            );
            for case in ["pipe_sink", "closed_sink"] {
                let child = owned_setup_child(test, case);
                assert_eq!(
                    child.status.code(),
                    Some(127),
                    "{case} must still exit 127, not a signal; status {:?}\nstdout:\n{}\nstderr:\n{}",
                    child.status,
                    String::from_utf8_lossy(&child.stdout),
                    String::from_utf8_lossy(&child.stderr)
                );
                assert!(
                    child.stderr.is_empty(),
                    "{case}: {:?}\nstdout:\n{}",
                    child.stderr,
                    String::from_utf8_lossy(&child.stdout)
                );
            }
            return;
        };
        use crate::clock_control::test_control;
        arm_clocked_sud_preconditions();
        reset_disposition(libc::SIGSEGV);
        reset_disposition(libc::SIGBUS);
        reset_disposition(libc::SIGPIPE);
        reset_disposition(OBSERVED_LIBTEST_HANDLER_SIGNAL);
        assert_eq!(
            signal_disposition(OBSERVED_LIBTEST_HANDLER_SIGNAL),
            libc::SIG_DFL as u64
        );
        assert_eq!(
            signal_disposition(libc::SIGPIPE),
            libc::SIG_DFL as u64,
            "the child must prove EPIPE safety under the DEFAULT SIGPIPE disposition, \
             not under the SIG_IGN that Rust installs"
        );
        let (mode, owned) = OWNED_SUD;
        let guard = prepare_owned_signal_state(
            mode,
            owned,
            runtime::InstructionSubscriptions {
                cpuid: false,
                rdtsc: false,
            },
        )
        .expect("the real SUD preparation must succeed in this child");
        assert_eq!(
            current_signal_mask() & (1_u64 << (libc::SIGPIPE - 1)),
            1_u64 << (libc::SIGPIPE - 1),
            "the prepared guard must leave SIGPIPE blocked"
        );
        assert_eq!(
            current_signal_mask() & (1_u64 << (libc::SIGSYS - 1)),
            0,
            "the prepared guard leaves SIGSYS unblocked"
        );
        test_control::set_ready(1);
        test_control::set_running(0);
        test_control::set_notification_armed(0);
        crate::timer::initialize_owned().expect("the controller must publish");
        assert!(
            crate::timer::owned_controller_installed(),
            "the controller must be genuinely published before the duplicate error"
        );
        let error = crate::timer::initialize_owned()
            .err()
            .expect("a second controller initialization must refuse");
        assert_eq!(signal_disposition(libc::SIGPIPE), libc::SIG_DFL as u64);
        match case.as_str() {
            "working_sink" => {}
            "pipe_sink" => {
                let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
                let _ = redirect_stderr(writer.as_raw_fd());
                drop(reader);
            }
            "closed_sink" => {
                assert_eq!(unsafe { libc::close(libc::STDERR_FILENO) }, 0);
            }
            other => panic!("unknown case {other}"),
        }
        unsafe { terminate_after_publication(SetupStage::PreciseController, &error) };
        panic!(
            "the terminal guard returned after a published failure: {guard:p}",
            guard = &guard
        );
    }

    fn redirect_stderr(target: std::os::fd::RawFd) -> std::os::fd::RawFd {
        let saved = unsafe { libc::dup(libc::STDERR_FILENO) };
        assert!(saved >= 0);
        assert!(unsafe { libc::dup2(target, libc::STDERR_FILENO) } >= 0);
        saved
    }

    fn restore_stderr(saved: std::os::fd::RawFd) {
        assert!(unsafe { libc::dup2(saved, libc::STDERR_FILENO) } >= 0);
        assert_eq!(unsafe { libc::close(saved) }, 0);
    }
}

/// Admission and shape policy for the public owned-native instruction route.
///
/// This is a sibling of [`tests`] so the `tool_host::tests::` filter used by the
/// existing host recipes keeps exactly its previous case list. These cases are
/// pure predicate checks: they install nothing, arm nothing, and run no guest.
/// They do not establish that a real CPUID or RDTSC reached the Tool; the
/// `owned_public_instruction` regression owns that claim.
#[cfg(test)]
mod public_owned_native {
    #[test]
    fn all_syscall_observation_does_not_grant_kernel_effects() {
        let subscriptions = reverie::Subscription::all();
        let setup = super::OwnedSetup::public_native(4096);
        assert!(
            super::admit_subscriptions(
                crate::SyscallMode::UserDispatchWithoutPatching,
                super::InstructionAdmission::Public,
                Some(setup),
                &subscriptions,
            )
            .is_ok()
        );
        assert!(
            super::admit_public_owned_syscalls(
                crate::SyscallMode::UserDispatchWithoutPatching,
                setup,
                &subscriptions,
            )
            .is_ok()
        );
        for number in [
            libc::SYS_execve,
            libc::SYS_clone,
            libc::SYS_rt_sigaction,
            libc::SYS_exit_group,
            libc::SYS_pwritev2,
        ] {
            assert_eq!(
                super::classify_owned_injection(true, number),
                super::OwnedInjection::Terminal
            );
        }
    }
    use reverie::syscalls::Sysno;

    use super::InstructionAdmission;
    use super::OwnedSetup;
    use super::admit_public_owned_syscalls;
    use super::admit_subscriptions;
    use super::instruction_results_only;
    use super::runtime;

    const EVIDENCE: usize = 8192;
    const SUD: crate::SyscallMode = crate::SyscallMode::UserDispatchWithoutPatching;

    /// Every one-field-short neighbour of the complete public native shape.
    pub(super) const INCOMPLETE_PUBLIC_NATIVE: [OwnedSetup; 5] = [
        OwnedSetup {
            clocked: false,
            single_step: true,
            precise_timer: true,
            syscalls: true,
            native_evidence: Some(EVIDENCE),
        },
        OwnedSetup {
            clocked: true,
            single_step: false,
            precise_timer: true,
            syscalls: true,
            native_evidence: Some(EVIDENCE),
        },
        OwnedSetup {
            clocked: true,
            single_step: true,
            precise_timer: false,
            syscalls: true,
            native_evidence: Some(EVIDENCE),
        },
        OwnedSetup {
            clocked: true,
            single_step: true,
            precise_timer: true,
            syscalls: false,
            native_evidence: Some(EVIDENCE),
        },
        OwnedSetup {
            clocked: true,
            single_step: true,
            precise_timer: true,
            syscalls: true,
            native_evidence: None,
        },
    ];

    /// Each admitted syscall subset crossed with each instruction subset, named
    /// so a failure reports which pair failed. `Subscription` has no `Debug`.
    fn instruction_sets() -> Vec<(String, reverie::Subscription)> {
        let mut sets = Vec::new();
        for syscalls in [
            vec![Sysno::getpid],
            vec![Sysno::read],
            vec![Sysno::getpid, Sysno::read],
        ] {
            for (cpuid, rdtsc) in [(true, false), (false, true), (true, true)] {
                let mut subscriptions: reverie::Subscription = syscalls.iter().copied().collect();
                if cpuid {
                    subscriptions.cpuid();
                }
                if rdtsc {
                    subscriptions.rdtsc();
                }
                sets.push((
                    format!("{syscalls:?} cpuid={cpuid} rdtsc={rdtsc}"),
                    subscriptions,
                ));
            }
        }
        sets
    }

    #[test]
    fn complete_shape_admits_instructions_with_the_getpid_read_bound() {
        let complete = OwnedSetup::public_native(EVIDENCE);
        assert!(complete.is_public_native());
        for (name, subscriptions) in instruction_sets() {
            assert!(
                admit_subscriptions(
                    SUD,
                    InstructionAdmission::Public,
                    Some(complete),
                    &subscriptions
                )
                .is_ok(),
                "{name} must be admitted for the complete public native shape"
            );
            assert!(admit_public_owned_syscalls(SUD, complete, &subscriptions).is_ok());
        }
    }

    #[test]
    fn the_same_sets_stay_refused_without_an_owned_setup() {
        for (name, subscriptions) in instruction_sets() {
            let error =
                admit_subscriptions(SUD, InstructionAdmission::Public, None, &subscriptions)
                    .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Unsupported, "{name}");
        }
    }

    #[test]
    fn every_incomplete_shape_is_refused() {
        for incomplete in INCOMPLETE_PUBLIC_NATIVE {
            assert!(!incomplete.is_public_native());
            for (name, subscriptions) in instruction_sets() {
                assert_eq!(
                    admit_subscriptions(
                        SUD,
                        InstructionAdmission::Public,
                        Some(incomplete),
                        &subscriptions
                    )
                    .unwrap_err()
                    .kind(),
                    std::io::ErrorKind::Unsupported,
                    "{incomplete:?} must not admit {name}"
                );
                assert!(admit_public_owned_syscalls(SUD, incomplete, &subscriptions).is_err());
            }
        }
    }

    #[test]
    fn instruction_only_sets_stay_outside_the_bounded_scope() {
        let complete = OwnedSetup::public_native(EVIDENCE);
        for (cpuid, rdtsc) in [(true, false), (false, true), (true, true)] {
            let mut subscriptions = reverie::Subscription::default();
            if cpuid {
                subscriptions.cpuid();
            }
            if rdtsc {
                subscriptions.rdtsc();
            }
            assert!(
                admit_subscriptions(
                    SUD,
                    InstructionAdmission::Public,
                    Some(complete),
                    &subscriptions
                )
                .is_err()
            );
            assert!(admit_public_owned_syscalls(SUD, complete, &subscriptions).is_err());
        }
    }

    #[test]
    fn vdso_and_unsupported_effect_subscriptions_require_the_complete_owned_shape() {
        let complete = OwnedSetup::public_native(EVIDENCE);
        for extra in [
            Sysno::clock_gettime,
            Sysno::getcpu,
            Sysno::gettimeofday,
            Sysno::time,
            Sysno::clock_getres,
            Sysno::write,
            Sysno::exit,
            Sysno::execve,
            Sysno::gettid,
            Sysno::getppid,
            Sysno::open,
            Sysno::newfstatat,
            Sysno::statx,
            Sysno::lseek,
            Sysno::pread64,
            Sysno::fcntl,
        ] {
            let mut subscriptions: reverie::Subscription =
                [Sysno::getpid, Sysno::read, extra].into_iter().collect();
            subscriptions.cpuid().rdtsc();
            assert!(
                admit_subscriptions(
                    SUD,
                    InstructionAdmission::Public,
                    Some(complete),
                    &subscriptions
                )
                .is_ok(),
                "{extra:?} observation must not imply kernel injection permission"
            );
            assert!(admit_public_owned_syscalls(SUD, complete, &subscriptions).is_ok());
            assert!(
                admit_subscriptions(SUD, InstructionAdmission::Public, None, &subscriptions)
                    .is_err()
            );
            for incomplete in INCOMPLETE_PUBLIC_NATIVE {
                assert!(admit_public_owned_syscalls(SUD, incomplete, &subscriptions).is_err());
            }
            assert_eq!(
                super::classify_owned_injection(true, extra as i64),
                if crate::syscall_event::injectable(extra as i64) {
                    super::OwnedInjection::Admitted
                } else {
                    super::OwnedInjection::Terminal
                }
            );
        }
    }

    #[test]
    fn returning_fd_and_full_subscriptions_are_observable() {
        let complete = OwnedSetup::public_native(EVIDENCE);
        for extra in [Sysno::openat, Sysno::fstat, Sysno::close] {
            let mut subscriptions: reverie::Subscription =
                [Sysno::getpid, Sysno::read, extra].into_iter().collect();
            subscriptions.cpuid().rdtsc();
            assert!(
                admit_subscriptions(
                    SUD,
                    InstructionAdmission::Public,
                    Some(complete),
                    &subscriptions
                )
                .is_ok(),
                "{extra:?} must be admitted"
            );
            assert!(admit_public_owned_syscalls(SUD, complete, &subscriptions).is_ok());
        }

        let mut whole_closure: reverie::Subscription = [
            Sysno::getpid,
            Sysno::read,
            Sysno::openat,
            Sysno::fstat,
            Sysno::close,
        ]
        .into_iter()
        .collect();
        whole_closure.cpuid().rdtsc();
        assert!(
            admit_subscriptions(
                SUD,
                InstructionAdmission::Public,
                Some(complete),
                &whole_closure
            )
            .is_ok()
        );
        assert!(admit_public_owned_syscalls(SUD, complete, &whole_closure).is_ok());

        let everything = reverie::Subscription::all();
        assert!(everything.iter_syscalls().next().is_some());
        assert!(
            everything
                .iter_syscalls()
                .any(|number| !crate::syscall_event::backed_returning(number as i64))
        );
        assert!(
            admit_subscriptions(
                SUD,
                InstructionAdmission::Public,
                Some(complete),
                &everything
            )
            .is_ok(),
            "Subscription::all() requests observations, not arbitrary kernel effects"
        );
        assert!(admit_public_owned_syscalls(SUD, complete, &everything).is_ok());
    }

    #[test]
    fn seccomp_patching_mode_stays_refused_with_the_complete_shape() {
        let complete = OwnedSetup::public_native(EVIDENCE);
        for (name, subscriptions) in instruction_sets() {
            assert!(
                admit_subscriptions(
                    crate::SyscallMode::SeccompWithPatching,
                    InstructionAdmission::Public,
                    Some(complete),
                    &subscriptions
                )
                .is_err(),
                "{name}"
            );
            assert!(
                admit_public_owned_syscalls(
                    crate::SyscallMode::SeccompWithPatching,
                    complete,
                    &subscriptions
                )
                .is_err()
            );
        }
    }

    #[test]
    fn admitting_instructions_does_not_reopen_guest_register_writes() {
        assert!(instruction_results_only(
            InstructionAdmission::Public,
            Some(OwnedSetup::public_native(EVIDENCE))
        ));
        for incomplete in INCOMPLETE_PUBLIC_NATIVE {
            assert!(instruction_results_only(
                InstructionAdmission::Public,
                Some(incomplete)
            ));
        }
    }

    /// The restricted decoder must be consulted before the native planner, or a
    /// subscribed instruction fault at a step boundary would be planned as an
    /// ordinary integer instruction. Admission makes that ordering load-bearing
    /// in production, not only in the private fixture.
    #[test]
    fn restricted_decode_precedes_the_native_planner_for_faults() {
        for (bytes, kind) in [
            (
                [0x0f, 0xa2, 0x90, 0x90, 0x90, 0x90].as_slice(),
                runtime::InstructionEventKind::Cpuid,
            ),
            (
                [0x0f, 0x31, 0x90, 0x90, 0x90, 0x90].as_slice(),
                runtime::InstructionEventKind::Rdtsc,
            ),
            (
                [0x0f, 0x01, 0xf9, 0x90, 0x90, 0x90].as_slice(),
                runtime::InstructionEventKind::Rdtscp,
            ),
        ] {
            let decoded = crate::owned_step::Instruction::decode(0x1000, bytes)
                .expect("subscribed instruction must decode");
            let crate::owned_step::Instruction::Fault(event) = decoded else {
                panic!("{kind:?} must decode as a fault, not as a native plan");
            };
            assert_eq!(event.kind, kind);
            assert_eq!(event.fault_pc, 0x1000);
            assert_eq!(event.resume_pc, 0x1000 + kind.bytes().len() as u64);
        }
    }
}
