use core::arch::global_asm;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicI32;
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::AtomicU8;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::cell::Cell;
use std::ffi::OsStr;
use std::io;
use std::ptr;
use std::sync::OnceLock;

use liteinst2::patcher::PatchError;
use liteinst2::patcher::prepare_live_patching;
use liteinst2::scanner::InstructionScanner;
use liteinst2::trampoline::HookContext;
use liteinst2::trampoline::HookSite;
use liteinst2::trampoline::InstalledHook;
use liteinst2::trampoline::TrampolineArena;
use liteinst2::trampoline::TrampolineError;
use reverie_preload::dispatch::SyscallDispatcher;
use reverie_preload::dispatch::SyscallEvent as PreloadSyscallEvent;
use reverie_preload::dispatch::SyscallEventSource;
use reverie_preload::dispatch::is_fork_like;
use reverie_preload::fork::ForkHook;
use reverie_preload::lifecycle::InProcessSeccomp;
use reverie_preload::lifecycle::RuntimeConfig;
use reverie_preload::trap::raw_syscall6;

global_asm!(
    r#"
    .text
    # These instruction sites are reached only after the nested-hook path has
    # temporarily enabled native execution. Keeping them private to that path
    # guarantees they have never been patched when they are first executed.
    .p2align 4
    .global reverie_liteinst_native_cpuid
    .hidden reverie_liteinst_native_cpuid
    .type reverie_liteinst_native_cpuid,@function
reverie_liteinst_native_cpuid:
    push rbx
    mov r8, rdx
    mov eax, edi
    mov ecx, esi
    cpuid
    mov dword ptr [r8], eax
    mov dword ptr [r8 + 4], ebx
    mov dword ptr [r8 + 8], ecx
    mov dword ptr [r8 + 12], edx
    pop rbx
    ret
    .size reverie_liteinst_native_cpuid, .-reverie_liteinst_native_cpuid

    .p2align 4
    .global reverie_liteinst_native_rdtsc
    .hidden reverie_liteinst_native_rdtsc
    .type reverie_liteinst_native_rdtsc,@function
reverie_liteinst_native_rdtsc:
    rdtsc
    shl rdx, 32
    or rax, rdx
    ret
    .size reverie_liteinst_native_rdtsc, .-reverie_liteinst_native_rdtsc

    .p2align 4
    .global reverie_liteinst_native_rdtscp
    .hidden reverie_liteinst_native_rdtscp
    .type reverie_liteinst_native_rdtscp,@function
reverie_liteinst_native_rdtscp:
    mov r8, rdi
    rdtscp
    mov dword ptr [r8], ecx
    shl rdx, 32
    or rax, rdx
    ret
    .size reverie_liteinst_native_rdtscp, .-reverie_liteinst_native_rdtscp
"#
);

unsafe extern "C" {
    fn reverie_liteinst_native_cpuid(eax: u32, ecx: u32, result: *mut NativeCpuidResult);
    fn reverie_liteinst_native_rdtsc() -> u64;
    fn reverie_liteinst_native_rdtscp(aux: *mut u32) -> u64;
}

const UNSET_RESULT: i64 = i64::MIN;
const SYS_IO_PGETEVENTS: i64 = 333;
const TOOL_COMPAT: u8 = 2;
const TOOL_REVERIE: u8 = 3;

const EVENT_CHANNEL_IDENTITY_FAILURE_STATUS: i32 = 120;
const EVENT_CHANNEL_WRITE_FAILURE_STATUS: i32 = 121;
const IN_GUEST_STAGE_WRITE_FAILURE_STATUS: i32 = 123;
/// Enables fail-closed, allocation-free in-guest lifecycle stage markers on stderr.
pub const IN_GUEST_STAGE_STREAM_ENV: &str = "REVERIE_LITEINST_IN_GUEST_STAGE_STREAM";
const MAX_PATCH_SITES: usize = 4096;
const ARENA_SLOTS: usize = 128;
const PATCH_SNAPSHOT_BYTES: usize = 64;
const SITE_INSTALLING: u8 = 1;
const SITE_ACTIVE: u8 = 2;
const SITE_FALLBACK: u8 = 3;
const SITE_STALE: u8 = 4;
const INSTRUCTION_CPUID: u8 = 1;
const INSTRUCTION_RDTSC: u8 = 2;

static TOOL_MODE: AtomicU8 = AtomicU8::new(0);
static EVENT_FD: AtomicI32 = AtomicI32::new(libc::STDERR_FILENO);
static COORDINATOR_FD: AtomicI32 = AtomicI32::new(-1);
static EVENT_COOKIE: AtomicU64 = AtomicU64::new(0);
static EVENT_DEVICE: AtomicU64 = AtomicU64::new(0);
static EVENT_INODE: AtomicU64 = AtomicU64::new(0);
static IN_GUEST_STAGE_STREAM: AtomicBool = AtomicBool::new(false);

thread_local! {
    static CURRENT_EVENT: Cell<*mut SyscallEvent> = const { Cell::new(ptr::null_mut()) };
    // Reentry is a property of Tool execution, not of syscall-event storage:
    // instruction callbacks have no current SyscallEvent but must take the same
    // native/raw bypasses while holding Tool and thread-state locks.
    static TOOL_CALLBACK_ACTIVE: AtomicBool = const { AtomicBool::new(false) };
}

struct ToolCallbackGuard {
    previous: bool,
}

impl ToolCallbackGuard {
    fn enter() -> Self {
        let previous = TOOL_CALLBACK_ACTIVE.with(|active| active.swap(true, Ordering::Relaxed));
        Self { previous }
    }
}

impl Drop for ToolCallbackGuard {
    fn drop(&mut self) {
        TOOL_CALLBACK_ACTIVE.with(|active| active.store(self.previous, Ordering::Relaxed));
    }
}

struct CurrentEventGuard {
    previous: *mut SyscallEvent,
}

impl CurrentEventGuard {
    fn enter(event: *mut SyscallEvent) -> Self {
        let previous = CURRENT_EVENT.replace(event);
        Self { previous }
    }
}

impl Drop for CurrentEventGuard {
    fn drop(&mut self) {
        CURRENT_EVENT.set(self.previous);
    }
}

fn tool_callback_active() -> bool {
    TOOL_CALLBACK_ACTIVE.with(|active| active.load(Ordering::Relaxed))
}

static ARENAS: OnceLock<Vec<RuntimeArena>> = OnceLock::new();
static SITES: OnceLock<Box<[SiteSlot]>> = OnceLock::new();
static PAGE_SIZE: AtomicU64 = AtomicU64::new(0);
static INSTALL_HELD: AtomicBool = AtomicBool::new(false);
static INSTRUCTION_SUBSCRIPTIONS: AtomicU8 = AtomicU8::new(0);
static PATCH_PUBLICATION: AtomicU8 = AtomicU8::new(PatchPublication::Concurrent as u8);
static PROCESS_FORKS_ALLOWED: AtomicBool = AtomicBool::new(true);

pub(crate) use crate::instruction_event::Kind as InstructionEventKind;

#[derive(Default)]
#[repr(C)]
struct NativeCpuidResult {
    eax: u32,
    ebx: u32,
    ecx: u32,
    edx: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct InstructionSubscriptions {
    pub(crate) cpuid: bool,
    pub(crate) rdtsc: bool,
}

thread_local! {
    static RCB_CLOCK: Cell<*mut reverie::pmu::InGuestRcbCounter> =
        const { Cell::new(ptr::null_mut()) };
    static RCB_CLOCK_OWNER: Cell<libc::pid_t> = const { Cell::new(0) };
    static RCB_CLOCK_UNAVAILABLE: Cell<bool> = const { Cell::new(false) };
    static RCB_HANDLER_ENTRY: Cell<u64> = const { Cell::new(0) };
    static RCB_HANDLER_DEDUCTION: Cell<u64> = const { Cell::new(0) };
    static RCB_HANDLER_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Install the current thread's in-guest RCB clock before seccomp is active.
pub(crate) fn initialize_rcb_clock() -> io::Result<()> {
    if crate::clock_control::active() {
        return Err(io::Error::other(
            "clock boundary cannot rebind an active counter",
        ));
    }
    initialize_rcb_clock_with(|| unsafe {
        if crate::clock_control::requested() {
            reverie::pmu::InGuestRcbCounter::current_thread_disabled_with_syscall_gate(raw_syscall6)
        } else {
            reverie::pmu::InGuestRcbCounter::current_thread_with_syscall_gate(raw_syscall6)
        }
    })
}

fn initialize_rcb_clock_with(
    create: impl FnOnce() -> Result<reverie::pmu::InGuestRcbCounter, reverie::Errno>,
) -> io::Result<()> {
    let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as libc::pid_t;
    if owner <= 0 {
        return Err(io::Error::last_os_error());
    }
    // A fork child can first discover that its inherited counter has the wrong
    // owner from inside the still-active fork callback. Preserve that callback
    // depth while replacing the counter; resetting it would make the outer
    // leave underflow after child reconstruction completes.
    let active_depth = RCB_HANDLER_DEPTH.get();
    // Publish an unavailable sentinel before creating the perf event. When a
    // fork child first initializes after seccomp is active, the builder's own
    // syscalls can re-enter an already-patched syscall hook; that nested hook
    // must observe this owner as initialized instead of recursively creating
    // another counter.
    RCB_CLOCK.set(ptr::null_mut());
    RCB_CLOCK_OWNER.set(owner);
    RCB_CLOCK_UNAVAILABLE.set(true);
    RCB_HANDLER_ENTRY.set(0);
    RCB_HANDLER_DEDUCTION.set(0);
    RCB_HANDLER_DEPTH.set(active_depth);
    let clock = match create() {
        Ok(clock) => clock,
        // The in-guest clock is optional. CPU discovery, perf-event setup,
        // mmap, reset, and enable failures all mean unavailable, not a failed
        // Tool installation.
        Err(error) if crate::clock_control::requested() => return Err(io::Error::other(error)),
        Err(_) => return Ok(()),
    };
    let active_entry = if active_depth == 0 {
        0
    } else {
        clock
            .read()
            .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))?
    };
    let pointer = Box::into_raw(Box::new(clock));
    RCB_CLOCK.set(pointer);
    if crate::clock_control::requested() {
        crate::clock_control::publish(unsafe { &*pointer })?;
    }
    RCB_CLOCK_OWNER.set(owner);
    RCB_CLOCK_UNAVAILABLE.set(false);
    RCB_HANDLER_ENTRY.set(active_entry);
    RCB_HANDLER_DEDUCTION.set(0);
    RCB_HANDLER_DEPTH.set(active_depth);
    Ok(())
}

fn rcb_clock() -> io::Result<Option<&'static reverie::pmu::InGuestRcbCounter>> {
    let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as libc::pid_t;
    if RCB_CLOCK_OWNER.get() != owner {
        // A fork/clone child inherits the parent's TLS bytes, including an fd
        // that still measures the parent thread. Leak that inherited handle
        // and bind a fresh PMU event to this calling thread.
        initialize_rcb_clock()?;
    }
    let current = RCB_CLOCK.get();
    if current.is_null() {
        debug_assert!(RCB_CLOCK_UNAVAILABLE.get());
        Ok(None)
    } else {
        Ok(Some(unsafe { &*current }))
    }
}

/// Mark entry into an ordinary-context tool callback.
pub(crate) fn enter_rcb_handler() -> io::Result<()> {
    if crate::clock_control::active() {
        return if crate::clock_control::paused() {
            Ok(())
        } else {
            Err(io::Error::other(
                "Rust callback reached before clock exclusion",
            ))
        };
    }
    let Some(clock) = rcb_clock()? else {
        RCB_HANDLER_DEPTH.set(RCB_HANDLER_DEPTH.get().saturating_add(1));
        return Ok(());
    };
    let sample = clock
        .read()
        .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))?;
    if RCB_HANDLER_DEPTH.get() == 0 {
        RCB_HANDLER_ENTRY.set(sample);
    }
    RCB_HANDLER_DEPTH.set(RCB_HANDLER_DEPTH.get().saturating_add(1));
    Ok(())
}

/// Deduct all RCBs retired while the outermost tool callback was active.
pub(crate) fn leave_rcb_handler() -> io::Result<()> {
    if crate::clock_control::active() {
        return if crate::clock_control::paused() {
            Ok(())
        } else {
            Err(io::Error::other(
                "clock restarted before Rust callback teardown",
            ))
        };
    }
    let depth = RCB_HANDLER_DEPTH.get();
    if depth == 0 {
        return Err(io::Error::other("LiteInst RCB handler depth underflow"));
    }
    RCB_HANDLER_DEPTH.set(depth - 1);
    if depth != 1 {
        return Ok(());
    }
    let Some(clock) = rcb_clock()? else {
        return Ok(());
    };
    let sample = clock
        .read()
        .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))?;
    RCB_HANDLER_DEDUCTION.set(
        RCB_HANDLER_DEDUCTION
            .get()
            .saturating_add(sample.saturating_sub(RCB_HANDLER_ENTRY.get())),
    );
    RCB_HANDLER_ENTRY.set(0);
    Ok(())
}

/// Return guest-only RCB time, excluding all completed and currently-active
/// LiteInst handler branches.
pub(crate) fn read_guest_rcb_clock() -> io::Result<u64> {
    #[cfg(all(test, feature = "private-crt"))]
    if GUEST_CLOCK_READ_PROBE.with(|probe| {
        if let Some(reads) = probe.get() {
            probe.set(Some(reads + 1));
            true
        } else {
            false
        }
    }) {
        return Err(io::Error::other(
            "test intercepted an additional clock-provider read",
        ));
    }
    if crate::clock_control::active() {
        if !crate::clock_control::paused() {
            return Err(io::Error::other("clock read outside excluded runtime"));
        }
        let clock = rcb_clock()?.ok_or_else(|| io::Error::other("clock unavailable"))?;
        return unsafe { clock.read_paused_once() }.map_err(io::Error::other);
    }
    let Some(clock) = rcb_clock()? else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "LiteInst in-guest RCB clock is unavailable on this host",
        ));
    };
    let sample = clock
        .read()
        .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))?;
    let active = if RCB_HANDLER_DEPTH.get() == 0 {
        0
    } else {
        sample.saturating_sub(RCB_HANDLER_ENTRY.get())
    };
    Ok(sample
        .saturating_sub(RCB_HANDLER_DEDUCTION.get())
        .saturating_sub(active))
}

pub(crate) fn reserve_coordinator_fd(fd: libc::c_int) -> io::Result<()> {
    COORDINATOR_FD
        .compare_exchange(-1, fd, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "coordinator FD reserved twice",
            )
        })
}

/// Rebinds the protected coordinator descriptor after a fork child reconnects.
///
/// `COORDINATOR_FD` is process-local after fork, so this changes only the
/// child's protection slot; the parent's connection and descriptor are intact.
pub(crate) fn replace_coordinator_fd(old: libc::c_int, new: libc::c_int) -> io::Result<()> {
    COORDINATOR_FD
        .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|actual| {
            io::Error::other(format!(
                "coordinator FD changed concurrently: expected {old}, observed {actual}"
            ))
        })
}

struct RuntimeArena {
    mapping_start: u64,
    mapping_end: u64,
    mapping_name: Box<str>,
    arena: TrampolineArena,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeMap {
    start: u64,
    end: u64,
    offset: u64,
    device: String,
    inode: u64,
    readable: bool,
    writable: bool,
    executable: bool,
    shared: bool,
}

struct SiteSlot {
    address: AtomicU64,
    state: AtomicU8,
    hook: AtomicPtr<InstalledHook>,
    mapping_end: AtomicU64,
    trap_count: AtomicU64,
    hook_count: AtomicU64,
    instruction_len: AtomicU8,
    straddle_prefix: AtomicU8,
}

impl SiteSlot {
    fn new() -> Self {
        Self {
            address: AtomicU64::new(0),
            state: AtomicU8::new(0),
            hook: AtomicPtr::new(ptr::null_mut()),
            mapping_end: AtomicU64::new(0),
            trap_count: AtomicU64::new(0),
            hook_count: AtomicU64::new(0),
            instruction_len: AtomicU8::new(0),
            straddle_prefix: AtomicU8::new(0),
        }
    }
}

pub(crate) struct SyscallEvent {
    pub(crate) owned_binding: Option<crate::owned_context::exit::Binding>,
    pub(crate) exit: Option<crate::owned_context::exit::Request>,
    pub(crate) number: i64,
    pub(crate) args: [u64; 6],
    pub(crate) instruction_pointer: u64,
    pub(crate) result: i64,
    pub(crate) context: usize,
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-254): Review launcher-selected shared RuntimeConfig alt-stack knob.
/// Environment variable selecting the shared [`RuntimeConfig::use_alt_stack`]
/// knob for the in-guest runtime's `SIGSYS` handler.
///
/// The [`RuntimeConfig`] and the controller that honors it live in
/// `reverie-preload` and are reviewed exactly once. The LiteInst owned launch
/// path passes this setting through its explicit command configuration.
///
/// When unset the shared default applies ([`RuntimeConfig::default`], alt stack
/// **on**). It applies to the LiteInst-dispatcher install path
/// ([`install_runtime`], used by the Tool path).
pub const ALT_STACK_ENV: &str = "REVERIE_LITEINST_ALT_STACK";
/// Allows a caller to keep fork-family syscalls fail-closed while integrating
/// a Tool whose process lifecycle is not ready for the direct backend.
pub const PROCESS_FORK_ENV: &str = "REVERIE_LITEINST_PROCESS_FORK";

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-254): Review alt-stack env parse/reject contract.
/// Parses an [`ALT_STACK_ENV`] value into the `use_alt_stack` boolean.
///
/// `None` (unset) yields the shared default. Accepts `1`/`0`, `true`/`false`,
/// `on`/`off`, and `yes`/`no` (case-insensitive, surrounding whitespace
/// trimmed). Any other value is rejected. Kept pure so the parse/reject contract
/// is unit-testable without touching process-global state.
pub fn alt_stack_from_env_value(value: Option<&OsStr>) -> io::Result<bool> {
    let Some(value) = value else {
        return Ok(RuntimeConfig::default().use_alt_stack);
    };
    let text = value.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{ALT_STACK_ENV} must be valid UTF-8"),
        )
    })?;
    match text.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Ok(true),
        "0" | "false" | "off" | "no" => Ok(false),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported {ALT_STACK_ENV} value {value:?}"),
        )),
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-254): Review launcher-selected RuntimeConfig assembly.
/// Builds the shared [`RuntimeConfig`] the launcher selected via [`ALT_STACK_ENV`].
///
/// Reads the process environment once; the parse itself is delegated to the pure
/// [`alt_stack_from_env_value`].
fn runtime_config_from_env() -> io::Result<RuntimeConfig> {
    let use_alt_stack = alt_stack_from_env_value(std::env::var_os(ALT_STACK_ENV).as_deref())?;
    Ok(RuntimeConfig { use_alt_stack })
}

pub(crate) fn cpuid_interception_enabled() -> bool {
    INSTRUCTION_SUBSCRIPTIONS.load(Ordering::Acquire) & INSTRUCTION_CPUID != 0
}

pub(crate) fn rdtsc_interception_enabled() -> bool {
    INSTRUCTION_SUBSCRIPTIONS.load(Ordering::Acquire) & INSTRUCTION_RDTSC != 0
}

pub(crate) fn preflight_instruction_faulting(
    subscriptions: InstructionSubscriptions,
) -> io::Result<()> {
    if !subscriptions.cpuid && !subscriptions.rdtsc {
        return Ok(());
    }

    // The exact setter probes temporarily change this thread's instruction
    // controls. Keep inherited asynchronous handlers from running application
    // CPUID/RDTSC during that bounded window, and restore the caller's exact
    // signal mask on every return path.
    let all_signals = u64::MAX;
    let mut previous_mask = 0;
    let masked = unsafe {
        raw_syscall6(
            libc::SYS_rt_sigprocmask,
            [
                libc::SIG_SETMASK as u64,
                (&raw const all_signals) as u64,
                (&raw mut previous_mask) as u64,
                core::mem::size_of::<u64>() as u64,
                0,
                0,
            ],
        )
    };
    if masked != 0 {
        return Err(io::Error::from_raw_os_error((-masked) as i32));
    }
    let _signal_mask = SignalInstallGuard {
        restore_mask: previous_mask,
    };

    if subscriptions.cpuid {
        const ARCH_GET_CPUID: u64 = 0x1011;
        const ARCH_SET_CPUID: u64 = 0x1012;
        let previous =
            unsafe { raw_syscall6(libc::SYS_arch_prctl, [ARCH_GET_CPUID, 0, 0, 0, 0, 0]) };
        if previous < 0 {
            return Err(instruction_control_unavailable("CPUID faulting", previous));
        }
        let result = unsafe { raw_syscall6(libc::SYS_arch_prctl, [ARCH_SET_CPUID, 0, 0, 0, 0, 0]) };
        if result != 0 {
            return Err(instruction_control_unavailable("CPUID faulting", result));
        }
        let restored = unsafe {
            raw_syscall6(
                libc::SYS_arch_prctl,
                [ARCH_SET_CPUID, previous as u64, 0, 0, 0, 0],
            )
        };
        if restored != 0 {
            unsafe {
                reverie_preload::trap::terminal126(
                    "restore-ARCH_SET_CPUID",
                    "raw-result",
                    Some(restored),
                )
            };
        }
    }
    if subscriptions.rdtsc {
        let mut previous = 0;
        let result = unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [
                    libc::PR_GET_TSC as u64,
                    (&raw mut previous) as u64,
                    0,
                    0,
                    0,
                    0,
                ],
            )
        };
        if result != 0 {
            return Err(instruction_control_unavailable("TSC faulting", result));
        }
        let result = unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [
                    libc::PR_SET_TSC as u64,
                    libc::PR_TSC_SIGSEGV as u64,
                    0,
                    0,
                    0,
                    0,
                ],
            )
        };
        if result != 0 {
            return Err(instruction_control_unavailable("TSC faulting", result));
        }
        let restored = unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [libc::PR_SET_TSC as u64, previous as u64, 0, 0, 0, 0],
            )
        };
        if restored != 0 {
            unsafe {
                reverie_preload::trap::terminal126(
                    "restore-PR_SET_TSC",
                    "raw-result",
                    Some(restored),
                )
            };
        }
    }
    Ok(())
}

fn instruction_control_unavailable(control: &str, result: i64) -> io::Error {
    let error = io::Error::from_raw_os_error((-result) as i32);
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{control} is unavailable: {error}"),
    )
}

fn install_instruction_signal_handler(
    subscriptions: InstructionSubscriptions,
    on_alt_stack: bool,
) -> io::Result<()> {
    let mut bits = 0;
    if subscriptions.cpuid {
        bits |= INSTRUCTION_CPUID;
    }
    if subscriptions.rdtsc {
        bits |= INSTRUCTION_RDTSC;
    }
    INSTRUCTION_SUBSCRIPTIONS.store(bits, Ordering::Release);
    if bits == 0 {
        return Ok(());
    }

    if crate::syscall_mode::sud_only() {
        let action = KernelSigaction {
            handler: crate::owned_context::signal_entry as *const () as u64,
            flags: (libc::SA_SIGINFO | libc::SA_ONSTACK | 0x04000000) as u64,
            restorer: reverie_preload::trap::trusted_sigreturn_restorer as *const () as u64,
            mask: u64::MAX,
        };
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [libc::SIGSEGV as u64, (&raw const action) as u64, 0, 8, 0, 0],
            )
        };
        if result != 0 {
            return Err(io::Error::from_raw_os_error((-result) as i32));
        }
        let mut installed = KernelSigaction::default();
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [
                    libc::SIGSEGV as u64,
                    0,
                    (&raw mut installed) as u64,
                    8,
                    0,
                    0,
                ],
            )
        };
        let unmaskable = (1 << (libc::SIGKILL - 1)) | (1 << (libc::SIGSTOP - 1));
        if result != 0
            || installed.handler != action.handler
            || installed.flags != action.flags
            || installed.restorer != action.restorer
            || installed.mask != action.mask & !unmaskable
        {
            return Err(io::Error::other(
                "owned CPUID signal action readback mismatch",
            ));
        }
        return Ok(());
    }

    let mut action: libc::sigaction = unsafe { core::mem::zeroed() };
    action.sa_flags = libc::SA_SIGINFO | if on_alt_stack { libc::SA_ONSTACK } else { 0 };
    action.sa_sigaction = instruction_sigsegv_handler as *const () as usize;
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::sigaction(libc::SIGSEGV, &action, ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn enable_instruction_faulting(subscriptions: InstructionSubscriptions) -> io::Result<()> {
    if subscriptions.cpuid {
        const ARCH_SET_CPUID: u64 = 0x1012;
        let result = unsafe { raw_syscall6(libc::SYS_arch_prctl, [ARCH_SET_CPUID, 0, 0, 0, 0, 0]) };
        if result != 0 {
            return Err(io::Error::from_raw_os_error((-result) as i32));
        }
    }
    if subscriptions.rdtsc {
        let result = unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [
                    libc::PR_SET_TSC as u64,
                    libc::PR_TSC_SIGSEGV as u64,
                    0,
                    0,
                    0,
                    0,
                ],
            )
        };
        if result != 0 {
            return Err(io::Error::from_raw_os_error((-result) as i32));
        }
    }
    Ok(())
}

pub(crate) fn initialize_from_environment() -> io::Result<()> {
    if let Some(value) = std::env::var_os("REVERIE_LITEINST_TOOL") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "REVERIE_LITEINST_TOOL={value:?} is not a supported launch path; use a caller-owned PreparedCommand"
            ),
        ));
    }
    Ok(())
}

pub(crate) fn initialize_reverie_tool(
    stats: crate::stats::GuestStatsHooks,
    publication: PatchPublication,
    instructions: InstructionSubscriptions,
    vdso_sites: &[reverie::vdso::VdsoSyscallSite],
) -> io::Result<()> {
    let stage_stream = match std::env::var_os(IN_GUEST_STAGE_STREAM_ENV).as_deref() {
        None => false,
        Some(value) if value == OsStr::new("0") => false,
        Some(value) if value == OsStr::new("1") => true,
        Some(value) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported {IN_GUEST_STAGE_STREAM_ENV} value {value:?}"),
            ));
        }
    };
    IN_GUEST_STAGE_STREAM.store(stage_stream, Ordering::Release);
    let process_forks_allowed = match std::env::var_os(PROCESS_FORK_ENV).as_deref() {
        None => true,
        Some(value) if value == OsStr::new("1") => true,
        Some(value) if value == OsStr::new("0") => false,
        Some(value) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported {PROCESS_FORK_ENV} value {value:?}"),
            ));
        }
    };
    PROCESS_FORKS_ALLOWED.store(
        process_forks_allowed && !crate::syscall_mode::sud_only(),
        Ordering::Release,
    );
    TOOL_MODE.store(TOOL_REVERIE, Ordering::Release);
    install_runtime(stats, publication, instructions, vdso_sites)
}

fn install_runtime(
    stats: crate::stats::GuestStatsHooks,
    publication: PatchPublication,
    instructions: InstructionSubscriptions,
    vdso_sites: &[reverie::vdso::VdsoSyscallSite],
) -> io::Result<()> {
    let _runtime = crate::runtime_domain::Entry::enter();
    unsafe {
        reverie_preload::trap::register_runtime_entry_hooks(&crate::runtime_domain::PRELOAD_HOOKS)?
    };
    PATCH_PUBLICATION.store(publication as u8, Ordering::Release);
    if !crate::syscall_mode::sud_only() {
        prepare_instrumentation()?;
        install_vdso_sites(vdso_sites)?;
    }
    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(PR-254): Review launcher-selected RuntimeConfig at the install seam.
    let config = runtime_config_from_env()?;
    if crate::syscall_mode::sud_only()
        && (instructions.cpuid || instructions.rdtsc || crate::owned_context::syscall_mode())
        && !config.use_alt_stack
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned instructions require the runtime alternate stack",
        ));
    }
    install_instruction_signal_handler(instructions, config.use_alt_stack)?;
    unsafe {
        let controller: &dyn reverie_preload::lifecycle::LifecycleController =
            if crate::syscall_mode::sud_only() {
                &reverie_preload::user_dispatch::InProcessUserDispatch
            } else {
                &InProcessSeccomp
            };
        reverie_preload::install(
            Box::new(LiteinstDispatcher::new(stats, publication)),
            controller,
            &config,
        )
    }?;
    if crate::syscall_mode::sud_only()
        && (instructions.cpuid || instructions.rdtsc || crate::owned_context::syscall_mode())
    {
        crate::owned_context::arm()?;
    }
    enable_instruction_faulting(instructions)
}

fn read_runtime_maps() -> io::Result<Vec<RuntimeMap>> {
    let maps = std::fs::read_to_string("/proc/self/maps")?;
    Ok(maps
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (range, permissions, offset, device, inode) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            let (start, end) = range.split_once('-')?;
            let permissions = permissions.as_bytes();
            Some(RuntimeMap {
                start: u64::from_str_radix(start, 16).ok()?,
                end: u64::from_str_radix(end, 16).ok()?,
                offset: u64::from_str_radix(offset, 16).ok()?,
                device: device.to_owned(),
                inode: inode.parse().ok()?,
                readable: permissions.first() == Some(&b'r'),
                writable: permissions.get(1) == Some(&b'w'),
                executable: permissions.get(2) == Some(&b'x'),
                shared: permissions.get(3) == Some(&b's'),
            })
        })
        .collect())
}

fn discover_arena_aliases(
    before: &[RuntimeMap],
    after: &[RuntimeMap],
) -> io::Result<(RuntimeMap, RuntimeMap)> {
    let expected_len = (ARENA_SLOTS * 4096) as u64;
    let new_maps = after
        .iter()
        .filter(|mapping| {
            !before
                .iter()
                .any(|old| old.start == mapping.start && old.end == mapping.end)
                && mapping.end.checked_sub(mapping.start) == Some(expected_len)
                && mapping.offset == 0
                && mapping.inode != 0
                && mapping.shared
                && mapping.readable
        })
        .collect::<Vec<_>>();
    let pairs = new_maps
        .iter()
        .filter_map(|writable| {
            (writable.writable && !writable.executable).then_some(())?;
            let executable = new_maps.iter().find(|executable| {
                !executable.writable
                    && executable.executable
                    && executable.device == writable.device
                    && executable.inode == writable.inode
            })?;
            Some(((*writable).clone(), (**executable).clone()))
        })
        .collect::<Vec<_>>();
    match pairs.as_slice() {
        [(writable, executable)] => Ok((writable.clone(), executable.clone())),
        _ => Err(io::Error::other(format!(
            "LiteInst arena allocation produced {} identity-matched alias pairs",
            pairs.len()
        ))),
    }
}

fn prepare_instrumentation() -> io::Result<()> {
    crate::syscall_mode::planning()?;
    crate::straddler::initialize_from_environment()?;
    prepare_live_patching().map_err(|error| io::Error::other(error.to_string()))?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size)
        .ok()
        .filter(|size| size.is_power_of_two())
        .ok_or_else(|| io::Error::other("invalid operating-system page size"))?;
    PAGE_SIZE.store(page_size, Ordering::Release);

    let sites = (0..MAX_PATCH_SITES)
        .map(|_| SiteSlot::new())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    SITES
        .set(sites)
        .map_err(|_| io::Error::other("LiteInst site registry initialized twice"))?;

    let maps = std::fs::read_to_string("/proc/self/maps")?;
    let mut arenas = Vec::new();
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let Some(range) = fields.next() else {
            continue;
        };
        let Some(permissions) = fields.next() else {
            continue;
        };
        let mapping_name = fields
            .nth(3)
            .unwrap_or("[anonymous]")
            .rsplit('/')
            .next()
            .unwrap_or("[anonymous]")
            .to_owned()
            .into_boxed_str();
        if !permissions
            .as_bytes()
            .get(2)
            .is_some_and(|byte| *byte == b'x')
        {
            continue;
        }
        let Some((start, end)) = range.split_once('-') else {
            continue;
        };
        let Ok(mapping_start) = u64::from_str_radix(start, 16) else {
            continue;
        };
        let Ok(mapping_end) = u64::from_str_radix(end, 16) else {
            continue;
        };
        if mapping_start >= mapping_end {
            continue;
        }
        let before = read_runtime_maps()?;
        let Ok(arena) = TrampolineArena::allocate_near(mapping_start, ARENA_SLOTS) else {
            continue;
        };
        let after = read_runtime_maps()?;
        let _ = discover_arena_aliases(&before, &after)?;
        arenas.push(RuntimeArena {
            mapping_start,
            mapping_end,
            mapping_name,
            arena,
        });
    }
    if arenas.is_empty() {
        return Err(io::Error::other(
            "could not allocate a LiteInst arena near any executable mapping",
        ));
    }
    ARENAS
        .set(arenas)
        .map_err(|_| io::Error::other("LiteInst arenas initialized twice"))
}

fn site_hash(address: u64, len: usize) -> usize {
    ((address >> 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) as usize) % len
}

fn find_site(address: u64) -> Option<&'static SiteSlot> {
    let sites = SITES.get()?;
    let start = site_hash(address, sites.len());
    for offset in 0..sites.len() {
        let slot = &sites[(start + offset) % sites.len()];
        match slot.address.load(Ordering::Acquire) {
            observed if observed == address => return Some(slot),
            0 => return None,
            _ => {}
        }
    }
    None
}

fn claim_existing_site(slot: &'static SiteSlot) -> (&'static SiteSlot, bool) {
    loop {
        let state = slot.state.load(Ordering::Acquire);
        if state == 0 {
            core::hint::spin_loop();
            continue;
        }
        if state == SITE_STALE {
            match slot.state.compare_exchange(
                SITE_STALE,
                SITE_INSTALLING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return (slot, true),
                Err(_) => continue,
            }
        }
        return (slot, false);
    }
}

fn claim_site(address: u64) -> Option<(&'static SiteSlot, bool)> {
    let sites = SITES.get()?;
    let start = site_hash(address, sites.len());
    for offset in 0..sites.len() {
        let slot = &sites[(start + offset) % sites.len()];
        let observed = slot.address.load(Ordering::Acquire);
        if observed == address {
            return Some(claim_existing_site(slot));
        }
        if observed == 0 {
            match slot
                .address
                .compare_exchange(0, address, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    slot.state.store(SITE_INSTALLING, Ordering::Release);
                    return Some((slot, true));
                }
                Err(raced) if raced == address => return Some(claim_existing_site(slot)),
                Err(_) => {}
            }
        }
    }
    None
}

fn mark_site_range_stale(start: u64, len: u64, replacement_end: u64) {
    let Some(end) = start.checked_add(len) else {
        return;
    };
    let Some(sites) = SITES.get() else {
        return;
    };
    for site in sites {
        let address = site.address.load(Ordering::Acquire);
        if start <= address && address < end {
            site.mapping_end.store(replacement_end, Ordering::Release);
            let state = site.state.load(Ordering::Acquire);
            if matches!(state, SITE_ACTIVE | SITE_FALLBACK) {
                site.state.store(SITE_STALE, Ordering::Release);
            }
        }
    }
}

// TODO-HUMAN-REVIEW(PR-127): Review executable mapping-generation tracking.
fn observe_mapping_generation(event: &SyscallEvent) {
    if event.result < 0 {
        return;
    }
    match event.number {
        // AUTONOMOUS-BOT-IMPLEMENTED
        libc::SYS_mmap => {
            let start = event.result as u64;
            mark_site_range_stale(start, event.args[1], start.saturating_add(event.args[1]));
        }
        // AUTONOMOUS-BOT-IMPLEMENTED
        libc::SYS_munmap => mark_site_range_stale(event.args[0], event.args[1], 0),
        // AUTONOMOUS-BOT-IMPLEMENTED
        libc::SYS_mremap => {
            mark_site_range_stale(event.args[0], event.args[1], 0);
            let start = event.result as u64;
            mark_site_range_stale(start, event.args[2], start.saturating_add(event.args[2]));
        }
        _ => {}
    }
}

pub(crate) fn site_counts(address: u64) -> (u64, u64) {
    find_site(address).map_or((0, 0), |site| {
        (
            site.trap_count.load(Ordering::Acquire),
            site.hook_count.load(Ordering::Acquire),
        )
    })
}

/// Distinct syscall numbers broken out individually by the fallback counters.
///
/// x86-64 syscall numbers currently top out well under this bound; a number at
/// or above it (or negative) is still counted in the process-wide total but is
/// not tracked per-number. Sized to cover the whole current table with headroom.
const TRACKED_SYSCALLS: usize = 512;

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-249): Review public fallback-surface observability counters.
/// Total and per-syscall counts for [`LiteinstDispatcher`]'s escape surface —
/// trapped sites the runtime could not route to the Tool (un-patchable
/// `SITE_FALLBACK`, or an unclaimable site) and therefore failed closed with
/// `EOPNOTSUPP`.
struct FallbackCounters {
    total: AtomicU64,
    by_number: [AtomicU64; TRACKED_SYSCALLS],
}

impl FallbackCounters {
    const fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            by_number: [const { AtomicU64::new(0) }; TRACKED_SYSCALLS],
        }
    }

    fn record(&self, number: i64) {
        self.total.fetch_add(1, Ordering::Relaxed);
        if let Ok(index) = usize::try_from(number)
            && index < TRACKED_SYSCALLS
        {
            self.by_number[index].fetch_add(1, Ordering::Relaxed);
        }
    }

    fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    fn by_number(&self, number: i64) -> u64 {
        match usize::try_from(number) {
            Ok(index) if index < TRACKED_SYSCALLS => self.by_number[index].load(Ordering::Relaxed),
            _ => 0,
        }
    }

    fn reset(&self) {
        self.total.store(0, Ordering::Relaxed);
        for slot in &self.by_number {
            slot.store(0, Ordering::Relaxed);
        }
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-249): Review public fallback-surface observability counters.
/// Process-wide counters used by the installed runtime.
static FALLBACK_COUNTERS: FallbackCounters = FallbackCounters::new();

/// Record that one syscall reached the fail-closed escape surface.
///
/// Anything reaching this point is, by construction, a trapped syscall the
/// runtime could not route to the Tool, so this counts the size of LiteInst's
/// residual escape surface. It is the by-syscall-number analog of the per-site
/// `trap`/`hook` counters ([`site_counts`]) and the direct counterpart of
/// reverie-e9patch's `record_fallback_dispatch` (round 4), keyed the same way so
/// the two ld-preload backends expose a symmetric fallback-surface metric.
///
/// Async-signal-safe: only relaxed atomic increments, so it is safe to call from
/// the `SIGSYS` dispatch path. It does not change the forwarding decision.
pub(crate) fn record_fallback_dispatch(number: i64) {
    // AUTONOMOUS-BOT-IMPLEMENTED
    FALLBACK_COUNTERS.record(number);
}

/// Total syscalls that failed closed on the escape surface.
///
/// A large value relative to the guest's total syscall count indicates a large
/// residual escape surface — trapped sites the runtime could not route to the
/// Tool. For Detcore (`TOOL_REVERIE`) this directly bounds the set of syscalls
/// (e.g. a libc-internal `getrandom`) that bypass determinism, so a nonzero
/// count is a determinism-completeness signal, not merely a perf one.
pub(crate) fn fallback_dispatch_count() -> u64 {
    FALLBACK_COUNTERS.total()
}

/// Number of times syscall `number` reached the escape surface.
///
/// Returns `0` for a negative number or one at or above [`TRACKED_SYSCALLS`],
/// which are only ever reflected in [`fallback_dispatch_count`].
pub(crate) fn fallback_syscall_count(number: i64) -> u64 {
    FALLBACK_COUNTERS.by_number(number)
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-260): Review the fork-child per-process observability reset.
/// Reset every fallback-surface counter to zero for the current process.
///
/// LiteInst's process-wide [`FALLBACK_COUNTERS`] and each patch site's per-site
/// `trap`/`hook` counts ([`site_counts`]) are inherited by a `fork`/`clone`
/// child copy-on-write, so without a
/// reset the child would report the parent's residual surface and hook activity
/// as its own. This is the same per-process runtime state the shared
/// [`ForkHook`] seam ([`reverie_preload::fork`]) exists to re-establish in the
/// child — the exact mechanism reverie-e9patch uses for its per-process fallback
/// counters (round 7). Only the *observability* fields are cleared; the site
/// registry's functional patch state (`address`/`state`/`hook`/`mapping_end`) is
/// left intact because the child COW-inherits the installed hooks and the same
/// executable mappings, so its instrumentation must keep working.
///
/// Signature is `fn()` so it can be wrapped in a [`ForkHook`]. Async-signal-safe:
/// only relaxed atomic stores plus one lock-free [`OnceLock::get`], no allocation
/// and no locks, so it is safe to run in the child from inside the `SIGSYS`
/// handler.
fn reset_site_observability(sites: &[SiteSlot]) {
    for site in sites {
        site.trap_count.store(0, Ordering::Relaxed);
        site.hook_count.store(0, Ordering::Relaxed);
    }
}

pub(crate) fn reset_fallback_observability() {
    // AUTONOMOUS-BOT-IMPLEMENTED
    FALLBACK_COUNTERS.reset();
    if let Some(sites) = SITES.get() {
        reset_site_observability(sites);
    }
}

pub(crate) fn submit_process_stats(
    tid: reverie::Tid,
    stats: crate::stats::GuestStatsHooks,
) -> io::Result<()> {
    let mut direct_hooks = 0_u64;
    let sites = SITES
        .get()
        .into_iter()
        .flatten()
        .filter_map(|site| {
            let trap_hits = site.trap_count.load(Ordering::Relaxed);
            let hook_hits = site.hook_count.load(Ordering::Relaxed);
            direct_hooks += hook_hits;
            (trap_hits != 0 || hook_hits != 0).then(|| crate::stats::LiteinstProcessSiteStats {
                rip: site.address.load(Ordering::Relaxed),
                patched: site.state.load(Ordering::Relaxed) == SITE_ACTIVE,
                instruction_length: site.instruction_len.load(Ordering::Relaxed),
                straddle_after: site.straddle_prefix.load(Ordering::Relaxed),
            })
        })
        .collect();
    stats.submit(tid, direct_hooks, sites)
}

pub(crate) fn record_fork_child_direct_hook(instruction_pointer: u64) {
    if let Some(site) = find_site(instruction_pointer) {
        site.hook_count.fetch_add(1, Ordering::Relaxed);
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-260): Review the shared fork-following seam reuse.
/// The shared fork-following hook: in the child of a successful fork-like
/// syscall, reset this process's fallback observability (see
/// [`reset_fallback_observability`]).
///
/// LiteInst hosts its own `SIGSYS` dispatcher rather than the shared
/// [`PassthroughDispatcher`](reverie_preload::dispatch::PassthroughDispatcher),
/// so it invokes this hook itself from [`process_syscall`] after forwarding a
/// fork-like syscall — but it reuses the *same* reviewed-once
/// [`ForkHook`]/[`is_fork_like`] seam e9patch does, rather than a private
/// fork-detection path.
static FORK_HOOK: ForkHook = ForkHook::new(reset_fallback_observability);

fn arena_for(address: u64) -> Option<&'static RuntimeArena> {
    ARENAS.get()?.iter().find(|entry| {
        entry.mapping_start <= address
            && address < entry.mapping_end
            && entry.arena.can_reach(address)
    })
}

unsafe fn set_text_protection(address: u64, protection: i32) -> io::Result<()> {
    let page_size = PAGE_SIZE.load(Ordering::Acquire);
    if page_size == 0 {
        return Err(io::Error::other("LiteInst page size is not initialized"));
    }
    let page_start = address & !(page_size - 1);
    let patch_end = address
        .checked_add(liteinst2::patcher::WORD_PATCH_BYTES as u64)
        .ok_or_else(|| io::Error::other("patch address overflow"))?;
    let page_end = patch_end
        .checked_add(page_size - 1)
        .map(|value| value & !(page_size - 1))
        .ok_or_else(|| io::Error::other("patch page range overflow"))?;
    let result = unsafe {
        raw_syscall6(
            libc::SYS_mprotect,
            [
                page_start,
                page_end - page_start,
                protection as u64,
                0,
                0,
                0,
            ],
        )
    };
    if result < 0 {
        return Err(io::Error::from_raw_os_error((-result) as i32));
    }
    Ok(())
}

unsafe fn set_mapping_protection(start: u64, len: u64, protection: i32) -> io::Result<()> {
    let result =
        unsafe { raw_syscall6(libc::SYS_mprotect, [start, len, protection as u64, 0, 0, 0]) };
    if result < 0 {
        return Err(io::Error::from_raw_os_error((-result) as i32));
    }
    Ok(())
}

struct InstallGuard {
    _runtime: crate::runtime_domain::Entry,
}

#[derive(Clone, Copy)]
#[repr(u8)]
pub(crate) enum PatchPublication {
    /// The caller prevents every other thread from reaching live code.
    Quiescent,
    /// Other application threads may fetch the site during publication.
    Concurrent,
}

fn patch_publication() -> PatchPublication {
    if PATCH_PUBLICATION.load(Ordering::Acquire) == PatchPublication::Quiescent as u8 {
        PatchPublication::Quiescent
    } else {
        PatchPublication::Concurrent
    }
}

impl Drop for InstallGuard {
    fn drop(&mut self) {
        INSTALL_HELD.store(false, Ordering::Release);
    }
}

fn lock_installation() -> io::Result<InstallGuard> {
    let runtime = crate::runtime_domain::Entry::enter();
    INSTALL_HELD
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .map(|_| InstallGuard { _runtime: runtime })
        .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "LiteInst installation is busy"))
}

unsafe fn install_site_hook(
    address: u64,
    slot: &'static SiteSlot,
    callback: liteinst2::trampoline::HookCallback,
    publication: PatchPublication,
    expected_instruction: &[u8],
    manage_protection: bool,
) -> io::Result<()> {
    crate::syscall_mode::patching()?;
    let _install_guard = lock_installation()?;
    let _allocation_scope = crate::patch_alloc::enter();
    let arena = arena_for(address)
        .ok_or_else(|| io::Error::other("no reachable LiteInst arena for syscall site"))?;
    let mut mapping_end = slot.mapping_end.load(Ordering::Acquire);
    if mapping_end <= address {
        mapping_end = arena.mapping_end;
        slot.mapping_end.store(mapping_end, Ordering::Release);
    }
    let available = usize::try_from(mapping_end - address)
        .unwrap_or(0)
        .min(PATCH_SNAPSHOT_BYTES);
    if available < liteinst2::patcher::WORD_PATCH_BYTES {
        return Err(io::Error::other(
            "syscall site is too close to its executable mapping end",
        ));
    }
    // SAFETY: arena_for proved this byte range lies in a live executable VMA.
    let candidate =
        unsafe { core::slice::from_raw_parts(address as usize as *const u8, available) };
    if candidate.get(..expected_instruction.len()) != Some(expected_instruction) {
        return Err(io::Error::other(
            "fault site does not contain the expected x86-64 instruction",
        ));
    }
    let scanner = InstructionScanner::default();
    let scan = scanner
        .scan_prefix(candidate, address, liteinst2::patcher::WORD_PATCH_BYTES)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let instruction_len = scan
        .instructions()
        .first()
        .expect("a successful prefix scan contains an instruction")
        .len();
    let straddle_prefix = scanner
        .cache_line_size()
        .split_offset(
            address as usize,
            instruction_len.min(liteinst2::patcher::NEAR_JUMP_BYTES),
        )
        .unwrap_or(0);

    slot.instruction_len
        .store(instruction_len as u8, Ordering::Release);
    slot.straddle_prefix
        .store(straddle_prefix as u8, Ordering::Release);
    let staleness = match publication {
        PatchPublication::Quiescent => None,
        PatchPublication::Concurrent => Some(crate::straddler::budget_for_patch(
            address as usize,
            scanner.cache_line_size(),
        )?),
    };
    let code = scan.snapshot();

    if manage_protection {
        unsafe {
            set_text_protection(
                address,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            )?;
        }
    }
    // A guarded cross-line plan rejects a trampoline displacement containing
    // the temporary INT3 byte at another instruction head. Arena slots have
    // distinct rel32 displacements, so retry a bounded number of fresh slots;
    // this changes no guest bytes and preserves the same patch mechanism.
    let mut attempts = 0;
    let installed = loop {
        attempts += 1;
        let site = HookSite::new(
            &scanner,
            &scan,
            code,
            address,
            address,
            address as usize as *mut u8,
        );
        let candidate = match publication {
            PatchPublication::Quiescent => unsafe {
                InstalledHook::install_replacing_first_in_arena_quiescent(
                    site,
                    callback,
                    &arena.arena,
                )
            },
            PatchPublication::Concurrent => unsafe {
                InstalledHook::install_replacing_first_in_arena(
                    site,
                    callback,
                    staleness.expect("concurrent publication has a staleness budget"),
                    &arena.arena,
                )
            },
        };
        match candidate {
            Ok(installed) => break Ok(installed),
            Err(TrampolineError::Patch(PatchError::GuardByteConflict { .. })) if attempts < 16 => {
                continue;
            }
            Err(error) => break Err(error),
        }
    };
    let installed = match installed {
        Ok(installed) => installed,
        Err(error) => {
            if manage_protection {
                let _ = unsafe { set_text_protection(address, libc::PROT_READ | libc::PROT_EXEC) };
            }
            return Err(io::Error::other(error.to_string()));
        }
    };
    let activation = match publication {
        PatchPublication::Concurrent => installed.activate(),
        // SAFETY: the caller prevents every other thread from fetching the
        // site until publication completes.
        PatchPublication::Quiescent => unsafe { installed.activate_quiescent() },
    };
    if let Err(error) = activation {
        if manage_protection {
            let _ = unsafe { set_text_protection(address, libc::PROT_READ | libc::PROT_EXEC) };
        }
        return Err(io::Error::other(error.to_string()));
    }
    if manage_protection {
        unsafe {
            set_text_protection(address, libc::PROT_READ | libc::PROT_EXEC)?;
        }
    }

    crate::syscall_mode::installed();
    let installed = Box::into_raw(Box::new(installed));
    slot.hook.store(installed, Ordering::Release);
    slot.instruction_len
        .store(instruction_len as u8, Ordering::Release);
    slot.straddle_prefix
        .store(straddle_prefix as u8, Ordering::Release);
    slot.state.store(SITE_ACTIVE, Ordering::Release);
    Ok(())
}

fn install_vdso_sites(sites: &[reverie::vdso::VdsoSyscallSite]) -> io::Result<()> {
    for site_info in sites {
        let address = site_info.address;
        let (site, claimed) = claim_site(address)
            .ok_or_else(|| io::Error::other("LiteInst vDSO site table is full"))?;
        if !claimed {
            return Err(io::Error::other("LiteInst vDSO site was claimed twice"));
        }
        unsafe {
            set_mapping_protection(
                site_info.mapping_start,
                site_info.mapping_len,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            )?;
            install_site_hook(
                address,
                site,
                vdso_callback(site_info.number)?,
                PatchPublication::Quiescent,
                &[0x0f, 0x05],
                false,
            )
        }
        .map_err(|error| {
            site.state.store(SITE_FALLBACK, Ordering::Release);
            io::Error::other(format!("failed to install LiteInst vDSO hook: {error}"))
        })?;
        unsafe {
            set_mapping_protection(
                site_info.mapping_start,
                site_info.mapping_len,
                libc::PROT_READ | libc::PROT_EXEC,
            )?;
        }
    }
    Ok(())
}

fn vdso_callback(number: i64) -> io::Result<liteinst2::trampoline::HookCallback> {
    match number {
        libc::SYS_time => Ok(installed_vdso_time_hook),
        libc::SYS_clock_gettime => Ok(installed_vdso_clock_gettime_hook),
        libc::SYS_getcpu => Ok(installed_vdso_getcpu_hook),
        libc::SYS_gettimeofday => Ok(installed_vdso_gettimeofday_hook),
        libc::SYS_clock_getres => Ok(installed_vdso_clock_getres_hook),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported LiteInst vDSO syscall number {number}"),
        )),
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-133): Review nested Tool syscall guards and raw forwarding.
#[repr(C)]
#[derive(Default)]
struct KernelSigaction {
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
}

pub(crate) struct SignalInstallGuard {
    restore_mask: u64,
}

impl Drop for SignalInstallGuard {
    fn drop(&mut self) {
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_SETMASK as u64,
                    (&raw const self.restore_mask) as u64,
                    0,
                    core::mem::size_of::<u64>() as u64,
                    0,
                    0,
                ],
            )
        };
        if result < 0 {
            unsafe {
                reverie_preload::trap::terminal126(
                    "restore-signal-mask",
                    "raw-result",
                    Some(result),
                )
            };
        }
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-133): Review atomic signal-state preparation.
pub(crate) fn prepare_guest_signal_state(
    instructions: InstructionSubscriptions,
) -> io::Result<SignalInstallGuard> {
    let sigsys = 1_u64 << (libc::SIGSYS - 1);
    let sigsegv = if instructions.cpuid
        || instructions.rdtsc
        || reverie_preload::signal::owned_trace::configured()
    {
        1_u64 << (libc::SIGSEGV - 1)
    } else {
        0
    };
    let install_mask = u64::MAX;
    let mut previous_mask = 0_u64;
    let result = unsafe {
        raw_syscall6(
            libc::SYS_rt_sigprocmask,
            [
                libc::SIG_SETMASK as u64,
                (&raw const install_mask) as u64,
                (&raw mut previous_mask) as u64,
                core::mem::size_of::<u64>() as u64,
                0,
                0,
            ],
        )
    };
    if result < 0 {
        return Err(io::Error::from_raw_os_error((-result) as i32));
    }
    let guard = SignalInstallGuard {
        restore_mask: if crate::syscall_mode::sud_only() {
            previous_mask
        } else {
            previous_mask & !(sigsys | sigsegv)
        },
    };
    let runtime_signals = reverie_preload::signal::required_runtime_signal_mask();
    if crate::syscall_mode::sud_only() && previous_mask & (sigsys | sigsegv | runtime_signals) != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SUD-only requires unblocked owned runtime signals",
        ));
    }

    for signal in 1..=64 {
        if matches!(signal, libc::SIGKILL | libc::SIGSTOP) {
            continue;
        }
        let mut action = KernelSigaction::default();
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [
                    signal as u64,
                    0,
                    (&raw mut action) as u64,
                    core::mem::size_of::<u64>() as u64,
                    0,
                    0,
                ],
            )
        };
        if result < 0 {
            return Err(io::Error::from_raw_os_error((-result) as i32));
        }
        if crate::syscall_mode::sud_only()
            && runtime_signals & (1u64 << (signal - 1)) != 0
            && action.handler != libc::SIG_DFL as u64
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "owned runtime signal disposition conflict",
            ));
        }
        if action.handler != libc::SIG_DFL as u64 && action.handler != libc::SIG_IGN as u64 {
            if crate::syscall_mode::sud_only() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("SUD-only does not admit existing handler for signal {signal}"),
                ));
            }
            let default_action = KernelSigaction::default();
            let result = unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigaction,
                    [
                        signal as u64,
                        (&raw const default_action) as u64,
                        0,
                        core::mem::size_of::<u64>() as u64,
                        0,
                        0,
                    ],
                )
            };
            if result < 0 {
                return Err(io::Error::from_raw_os_error((-result) as i32));
            }
        }
    }
    if crate::syscall_mode::sud_only() && !reverie_preload::signal::runtime_signals_configured() {
        let arm_mask = install_mask & !sigsys;
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_SETMASK as u64,
                    (&raw const arm_mask) as u64,
                    0,
                    8,
                    0,
                    0,
                ],
            )
        };
        if result < 0 {
            return Err(io::Error::from_raw_os_error((-result) as i32));
        }
    }
    Ok(guard)
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-133): Review fault-safe guest signal-action decoding.
pub(crate) fn signal_action_supported(number: i64, args: [u64; 6]) -> bool {
    if (reverie_preload::signal::runtime_signals_configured()
        || reverie_preload::signal::owned_trace::configured())
        && matches!(
            number,
            libc::SYS_rt_sigsuspend
                | libc::SYS_pselect6
                | libc::SYS_ppoll
                | libc::SYS_epoll_pwait
                | libc::SYS_epoll_pwait2
                | SYS_IO_PGETEVENTS
        )
    {
        return false;
    }
    if number != libc::SYS_rt_sigaction || args[1] == 0 {
        return true;
    }
    if reverie_preload::signal::is_reserved(args[0] as i32)
        || (args[0] == libc::SIGSEGV as u64
            && INSTRUCTION_SUBSCRIPTIONS.load(Ordering::Acquire) != 0)
    {
        return false;
    }

    let mut handler = 0_u64;
    let local = libc::iovec {
        iov_base: (&raw mut handler).cast(),
        iov_len: core::mem::size_of::<u64>(),
    };
    let remote = libc::iovec {
        iov_base: args[1] as usize as *mut libc::c_void,
        iov_len: core::mem::size_of::<u64>(),
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
    read == core::mem::size_of::<u64>() as i64
        && matches!(handler, value if value == libc::SIG_DFL as u64 || value == libc::SIG_IGN as u64)
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-133): Review nested Tool syscall guards and raw forwarding.
fn forward_nested_tool_syscall(event: &mut SyscallEvent) {
    if !tool_callback_active() {
        event.result = -i64::from(libc::EPERM);
        return;
    }
    if crate::rpc::allows_channel_io(event.number, event.args[0] as i32) {
        event.result = unsafe { raw_syscall6(event.number, event.args) };
        return;
    }
    let unsupported_process =
        // AUTONOMOUS-BOT-IMPLEMENTED
        event.number == libc::SYS_clone
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_clone3
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_fork
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_vfork
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_execve
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_execveat;
    let unsupported_signal_state =
        // AUTONOMOUS-BOT-IMPLEMENTED
        event.number == libc::SYS_rt_sigaction
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_rt_sigprocmask
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_sigaltstack
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_rt_sigsuspend
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_pselect6
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_ppoll
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_epoll_pwait
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == libc::SYS_epoll_pwait2
        // AUTONOMOUS-BOT-IMPLEMENTED
        || event.number == SYS_IO_PGETEVENTS;
    if unsupported_process {
        event.result = -i64::from(libc::ENOTSUP);
    } else if unsupported_signal_state {
        event.result = -i64::from(libc::EPERM);
    } else if !(protect_runtime_control(event) || unsafe { protect_coordinator_channel(event) }) {
        event.result = guarded_raw_syscall(event.number, event.args);
        observe_mapping_generation(event);
    }
}

fn instruction_at(address: u64) -> Option<(InstructionEventKind, &'static [u8])> {
    let arena = arena_for(address)?;
    let available = usize::try_from(arena.mapping_end.checked_sub(address)?)
        .ok()?
        .min(3);
    if available < 2 {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(address as usize as *const u8, available) };
    let event = crate::instruction_event::InstructionEvent::decode(address, bytes)?;
    Some((event.kind, event.kind.bytes()))
}

fn instruction_is_subscribed(kind: InstructionEventKind) -> bool {
    let bits = INSTRUCTION_SUBSCRIPTIONS.load(Ordering::Acquire);
    match kind {
        InstructionEventKind::Cpuid => bits & INSTRUCTION_CPUID != 0,
        InstructionEventKind::Rdtsc | InstructionEventKind::Rdtscp => bits & INSTRUCTION_RDTSC != 0,
    }
}

fn instruction_callback(kind: InstructionEventKind) -> liteinst2::trampoline::HookCallback {
    match kind {
        InstructionEventKind::Cpuid => installed_cpuid_hook,
        InstructionEventKind::Rdtsc => installed_rdtsc_hook,
        InstructionEventKind::Rdtscp => installed_rdtscp_hook,
    }
}

struct NativeInstructionScope(u64);

unsafe extern "C" {
    fn reverie_liteinst_instruction_scope_enter(selected: u64) -> u64;
    fn reverie_liteinst_instruction_scope_leave(token: u64);
}

global_asm!(
    r#"
    .text
    .global reverie_liteinst_instruction_scope_enter
    .hidden reverie_liteinst_instruction_scope_enter
    .type reverie_liteinst_instruction_scope_enter,@function
reverie_liteinst_instruction_scope_enter:
    push r12
    push r13
    sub rsp, 24
    mov r12, rdi
    mov r13, rdi
    cmp rdi, 3
    ja .Linstruction_scope_fail
    test r12, 1
    jz .Linstruction_query_tsc
    mov eax, 158
    mov edi, 0x1011
    .global reverie_liteinst_instruction_get_cpuid
    .hidden reverie_liteinst_instruction_get_cpuid
reverie_liteinst_instruction_get_cpuid:
    call reverie_preload_trusted_syscall_ip
    .global reverie_liteinst_instruction_get_cpuid_returned
    .hidden reverie_liteinst_instruction_get_cpuid_returned
reverie_liteinst_instruction_get_cpuid_returned:
    cmp rax, 1
    ja .Linstruction_scope_fail
    shl rax, 2
    or r13, rax
.Linstruction_query_tsc:
    test r12, 2
    jz .Linstruction_enable_cpuid
    mov dword ptr [rsp], 0
    mov eax, 157
    mov edi, 25
    mov rsi, rsp
    .global reverie_liteinst_instruction_get_tsc
    .hidden reverie_liteinst_instruction_get_tsc
reverie_liteinst_instruction_get_tsc:
    call reverie_preload_trusted_syscall_ip
    .global reverie_liteinst_instruction_get_tsc_returned
    .hidden reverie_liteinst_instruction_get_tsc_returned
reverie_liteinst_instruction_get_tsc_returned:
    test rax, rax
    jnz .Linstruction_scope_fail
    mov eax, dword ptr [rsp]
    lea ecx, [eax - 1]
    cmp ecx, 1
    ja .Linstruction_scope_fail
    shl rax, 3
    or r13, rax
.Linstruction_enable_cpuid:
    test r12, 1
    jz .Linstruction_enable_tsc
    mov eax, 158
    mov edi, 0x1012
    mov esi, 1
    .global reverie_liteinst_instruction_set_cpuid
    .hidden reverie_liteinst_instruction_set_cpuid
reverie_liteinst_instruction_set_cpuid:
    call reverie_preload_trusted_syscall_ip
    .global reverie_liteinst_instruction_set_cpuid_returned
    .hidden reverie_liteinst_instruction_set_cpuid_returned
reverie_liteinst_instruction_set_cpuid_returned:
    test rax, rax
    jnz .Linstruction_scope_fail
.Linstruction_enable_tsc:
    test r12, 2
    jz .Linstruction_enter_done
    mov eax, 157
    mov edi, 26
    mov esi, 1
    .global reverie_liteinst_instruction_set_tsc
    .hidden reverie_liteinst_instruction_set_tsc
reverie_liteinst_instruction_set_tsc:
    call reverie_preload_trusted_syscall_ip
    .global reverie_liteinst_instruction_set_tsc_returned
    .hidden reverie_liteinst_instruction_set_tsc_returned
reverie_liteinst_instruction_set_tsc_returned:
    test rax, rax
    jnz .Linstruction_scope_fail
.Linstruction_enter_done:
    mov rax, r13
    add rsp, 24
    pop r13
    pop r12
    ret
    .size reverie_liteinst_instruction_scope_enter, .-reverie_liteinst_instruction_scope_enter

    .global reverie_liteinst_instruction_scope_leave
    .hidden reverie_liteinst_instruction_scope_leave
    .type reverie_liteinst_instruction_scope_leave,@function
reverie_liteinst_instruction_scope_leave:
    push r12
    mov r12, rdi
    cmp r12, 31
    ja .Linstruction_scope_fail
    test r12, 2
    jz .Linstruction_restore_cpuid
    mov rsi, r12
    shr rsi, 3
    lea ecx, [esi - 1]
    cmp ecx, 1
    ja .Linstruction_scope_fail
    mov eax, 157
    mov edi, 26
    .global reverie_liteinst_instruction_restore_tsc
    .hidden reverie_liteinst_instruction_restore_tsc
reverie_liteinst_instruction_restore_tsc:
    call reverie_preload_trusted_syscall_ip
    .global reverie_liteinst_instruction_restore_tsc_returned
    .hidden reverie_liteinst_instruction_restore_tsc_returned
reverie_liteinst_instruction_restore_tsc_returned:
    test rax, rax
    jnz .Linstruction_scope_fail
.Linstruction_restore_cpuid:
    test r12, 1
    jz .Linstruction_leave_done
    mov rsi, r12
    shr rsi, 2
    and esi, 1
    mov eax, 158
    mov edi, 0x1012
    .global reverie_liteinst_instruction_restore_cpuid
    .hidden reverie_liteinst_instruction_restore_cpuid
reverie_liteinst_instruction_restore_cpuid:
    call reverie_preload_trusted_syscall_ip
    .global reverie_liteinst_instruction_restore_cpuid_returned
    .hidden reverie_liteinst_instruction_restore_cpuid_returned
reverie_liteinst_instruction_restore_cpuid_returned:
    test rax, rax
    jnz .Linstruction_scope_fail
.Linstruction_leave_done:
    pop r12
    ret
    .size reverie_liteinst_instruction_scope_leave, .-reverie_liteinst_instruction_scope_leave
.Linstruction_scope_fail:
    mov eax, 231
    mov edi, 125
    call reverie_preload_trusted_syscall_ip
    ud2
"#
);

impl NativeInstructionScope {
    unsafe fn enter(selected: u64) -> Self {
        Self(unsafe { reverie_liteinst_instruction_scope_enter(selected) })
    }

    unsafe fn all() -> Self {
        unsafe { Self::enter(u64::from(INSTRUCTION_SUBSCRIPTIONS.load(Ordering::Acquire))) }
    }

    unsafe fn instruction(kind: InstructionEventKind) -> Self {
        unsafe {
            Self::enter(match kind {
                InstructionEventKind::Cpuid => 1,
                InstructionEventKind::Rdtsc | InstructionEventKind::Rdtscp => 2,
            })
        }
    }
}

impl Drop for NativeInstructionScope {
    fn drop(&mut self) {
        unsafe { reverie_liteinst_instruction_scope_leave(self.0) };
    }
}

unsafe fn deliver_default_sigsegv() -> ! {
    let default_action = KernelSigaction::default();
    let _ = unsafe {
        raw_syscall6(
            libc::SYS_rt_sigaction,
            [
                libc::SIGSEGV as u64,
                (&raw const default_action) as u64,
                0,
                core::mem::size_of::<u64>() as u64,
                0,
                0,
            ],
        )
    };
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
    let _ = unsafe {
        raw_syscall6(
            libc::SYS_tgkill,
            [pid as u64, tid as u64, libc::SIGSEGV as u64, 0, 0, 0],
        )
    };
    unsafe { exit_now(128 + libc::SIGSEGV) }
}

unsafe extern "C" fn instruction_sigsegv_body(
    signal: libc::c_int,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) -> reverie_preload::clock_boundary::Continuation {
    let _runtime = crate::runtime_domain::Entry::enter();
    if signal != libc::SIGSEGV || context.is_null() {
        emit_in_guest_stage(b"instruction-sigsegv-invalid-context");
        unsafe { deliver_default_sigsegv() };
    }
    let context = unsafe { &mut *context.cast::<libc::ucontext_t>() };
    let address = context.uc_mcontext.gregs[libc::REG_RIP as usize] as u64;
    let Some((kind, expected)) = instruction_at(address) else {
        if let Some(arena) = arena_for(address) {
            let available = usize::try_from(arena.mapping_end.saturating_sub(address))
                .unwrap_or(0)
                .min(8);
            let bytes =
                unsafe { core::slice::from_raw_parts(address as usize as *const u8, available) };
            let fault_address = if info.is_null() {
                0
            } else {
                unsafe { (*info).si_addr() as usize as u64 }
            };
            emit_instruction_refusal_stage(
                b"instruction-sigsegv-unrecognized-bytes",
                address.saturating_sub(arena.mapping_start),
                fault_address,
                context.uc_mcontext.gregs[libc::REG_RSP as usize] as u64,
                arena.mapping_name.as_bytes(),
                arena.mapping_end.saturating_sub(arena.mapping_start),
                bytes,
            );
        } else {
            emit_in_guest_stage(b"instruction-sigsegv-no-reachable-arena");
        }
        unsafe { deliver_default_sigsegv() };
    };
    if !instruction_is_subscribed(kind) {
        emit_in_guest_stage(b"instruction-sigsegv-unsubscribed");
        unsafe { deliver_default_sigsegv() };
    }

    // An unpatched instruction reached from an active Tool callback must not
    // allocate a trampoline. After fork, the arena cursor is process-private
    // but its backing pages are shared; child publication would let the parent
    // reuse and overwrite the same slot. Execute at the private native helper
    // and advance the faulting context instead.
    if tool_callback_active() {
        emit_in_guest_stage(match kind {
            InstructionEventKind::Cpuid => b"nested-instruction-fault-native-cpuid",
            InstructionEventKind::Rdtsc => b"nested-instruction-fault-native-rdtsc",
            InstructionEventKind::Rdtscp => b"nested-instruction-fault-native-rdtscp",
        });
        let native = unsafe { NativeInstructionScope::instruction(kind) };
        unsafe { execute_native_fault_instruction(kind, context, expected.len()) };
        drop(native);
        return reverie_preload::clock_boundary::Continuation::RUNTIME;
    }

    let Some((site, claimed)) = claim_site(address) else {
        emit_in_guest_stage(b"instruction-sigsegv-site-table-full");
        unsafe { deliver_default_sigsegv() };
    };
    site.trap_count.fetch_add(1, Ordering::Relaxed);
    let native = unsafe { NativeInstructionScope::all() };
    if claimed
        && unsafe {
            install_site_hook(
                address,
                site,
                instruction_callback(kind),
                patch_publication(),
                expected,
                true,
            )
        }
        .is_err()
    {
        site.state.store(SITE_FALLBACK, Ordering::Release);
    }
    drop(native);
    while matches!(site.state.load(Ordering::Acquire), 0 | SITE_INSTALLING) {
        core::hint::spin_loop();
    }
    if site.state.load(Ordering::Acquire) != SITE_ACTIVE {
        emit_in_guest_stage(b"instruction-sigsegv-site-install-failed");
        unsafe { deliver_default_sigsegv() };
    }
    let hook = site.hook.load(Ordering::Acquire);
    if hook.is_null() {
        emit_in_guest_stage(b"instruction-sigsegv-hook-missing");
        unsafe { deliver_default_sigsegv() };
    }
    context.uc_mcontext.gregs[libc::REG_RIP as usize] =
        unsafe { (*hook).trampoline().address() } as i64;
    let witness = crate::clock_control::callback_return_pc(unsafe { (*hook).trampoline() })
        .unwrap_or_else(|_| unsafe { exit_now(127) });
    reverie_preload::clock_boundary::Continuation::hook(witness)
}

reverie_preload::clocked_signal!(instruction_sigsegv_handler, instruction_sigsegv_body);

unsafe fn execute_native_fault_instruction(
    kind: InstructionEventKind,
    context: &mut libc::ucontext_t,
    instruction_len: usize,
) {
    let registers = &mut context.uc_mcontext.gregs;
    match kind {
        InstructionEventKind::Cpuid => {
            let mut result = NativeCpuidResult::default();
            unsafe {
                reverie_liteinst_native_cpuid(
                    registers[libc::REG_RAX as usize] as u32,
                    registers[libc::REG_RCX as usize] as u32,
                    &mut result,
                )
            };
            registers[libc::REG_RAX as usize] = i64::from(result.eax);
            registers[libc::REG_RBX as usize] = i64::from(result.ebx);
            registers[libc::REG_RCX as usize] = i64::from(result.ecx);
            registers[libc::REG_RDX as usize] = i64::from(result.edx);
        }
        InstructionEventKind::Rdtsc => {
            let value = unsafe { reverie_liteinst_native_rdtsc() };
            registers[libc::REG_RAX as usize] = i64::from(value as u32);
            registers[libc::REG_RDX as usize] = (value >> 32) as i64;
        }
        InstructionEventKind::Rdtscp => {
            let mut aux = 0;
            let value = unsafe { reverie_liteinst_native_rdtscp(&mut aux) };
            registers[libc::REG_RAX as usize] = i64::from(value as u32);
            registers[libc::REG_RDX as usize] = (value >> 32) as i64;
            registers[libc::REG_RCX as usize] = i64::from(aux);
        }
    }
    registers[libc::REG_RIP as usize] =
        registers[libc::REG_RIP as usize].saturating_add(instruction_len as i64);
}

unsafe fn installed_instruction_hook(context: *mut HookContext, kind: InstructionEventKind) {
    let _runtime = crate::runtime_domain::Entry::enter();
    if context.is_null() || enter_rcb_handler().is_err() {
        unsafe { exit_now(122) };
    }
    let context = unsafe { &mut *context };
    if let Some(site) = find_site(context.instruction_pointer) {
        site.hook_count.fetch_add(1, Ordering::Relaxed);
    }
    let native = unsafe { NativeInstructionScope::instruction(kind) };
    // A previously patched instruction still jumps here while native faulting
    // is enabled. Re-entering the Tool would deadlock on its already-held lock,
    // so execute the instruction at a private, never-patched site instead.
    if tool_callback_active() {
        emit_in_guest_stage(match kind {
            InstructionEventKind::Cpuid => b"nested-instruction-native-cpuid",
            InstructionEventKind::Rdtsc => b"nested-instruction-native-rdtsc",
            InstructionEventKind::Rdtscp => b"nested-instruction-native-rdtscp",
        });
        unsafe { execute_native_instruction(kind, context) };
        drop(native);
        if leave_rcb_handler().is_err() {
            unsafe { exit_now(122) };
        }
        return;
    }
    {
        let _tool_callback = ToolCallbackGuard::enter();
        crate::tool_host::dispatch_instruction(kind, context);
    }
    drop(native);
    if leave_rcb_handler().is_err() {
        unsafe { exit_now(122) };
    }
}

pub(crate) unsafe fn dispatch_owned_instruction(
    kind: InstructionEventKind,
    context: &mut HookContext,
) -> reverie_preload::signal::native_frame::InstructionResult {
    let _runtime = crate::runtime_domain::Entry::enter();
    let _native = unsafe { NativeInstructionScope::all() };
    let _callback = ToolCallbackGuard::enter();
    crate::tool_host::dispatch_instruction(kind, context)
}

pub(crate) unsafe fn dispatch_owned_timer(context: &mut HookContext) {
    let _runtime = crate::runtime_domain::Entry::enter();
    let _native = unsafe { NativeInstructionScope::all() };
    let _callback = ToolCallbackGuard::enter();
    crate::tool_host::dispatch_timer(context)
}

#[cfg(feature = "private-crt")]
pub(crate) unsafe fn dispatch_owned_initial(context: &mut HookContext) {
    let _runtime = crate::runtime_domain::Entry::enter();
    let _native = unsafe { NativeInstructionScope::all() };
    let _callback = ToolCallbackGuard::enter();
    crate::tool_host::dispatch_initial(context)
}

pub(crate) fn nested_tool_callback() -> bool {
    tool_callback_active()
}

pub(crate) unsafe fn dispatch_owned_syscall(
    context: &mut HookContext,
    number: i64,
    binding: crate::owned_context::exit::Binding,
) -> crate::owned_context::exit::Outcome {
    let _runtime = crate::runtime_domain::Entry::enter();
    let _native = unsafe { NativeInstructionScope::all() };
    if let Err(error) = enter_rcb_handler() {
        exit_io126("owned-syscall/enter-clock", &error);
    }
    let mut event = SyscallEvent {
        owned_binding: Some(binding),
        exit: None,
        number,
        args: [
            context.rdi,
            context.rsi,
            context.rdx,
            context.r10,
            context.r8,
            context.r9,
        ],
        instruction_pointer: context.instruction_pointer,
        result: UNSET_RESULT,
        context: context as *mut HookContext as usize,
    };
    {
        let _callback = ToolCallbackGuard::enter();
        let _event = CurrentEventGuard::enter(&mut event);
        unsafe { tool_trampoline() };
    }
    if let Err(error) = leave_rcb_handler() {
        exit_io126("owned-syscall/leave-clock", &error);
    }
    if let Some(request) = event.exit.take() {
        if event.result != UNSET_RESULT || event.owned_binding.is_some() {
            crate::owned_context::exit::failed("owned-exit/outcome", None);
        }
        return crate::owned_context::exit::Outcome::Exit(request);
    }
    if event.result == UNSET_RESULT {
        unsafe {
            reverie_preload::trap::terminal126(
                "owned-syscall/unset-result",
                "raw-result",
                Some(event.result),
            )
        };
    }
    crate::owned_context::exit::Outcome::Returned(event.result)
}

pub(crate) unsafe fn dispatch_owned_rng_snapshot(
    owner: i64,
) -> Result<reverie::vdso::VdsoRngSnapshot, reverie::Error> {
    let _runtime = crate::runtime_domain::Entry::enter();
    if !crate::clock_control::active() || !crate::clock_control::paused() {
        return Err(io::Error::other("modeled RNG input outside owned paused runtime").into());
    }
    let _native = unsafe { NativeInstructionScope::all() };
    let _callback = ToolCallbackGuard::enter();
    crate::tool_host::rng_snapshot(owner)
}

pub(crate) unsafe fn dispatch_owned_guest_progress(
    provenance: crate::owned_context::GuestProgressCapture<'_>,
    context: &mut HookContext,
    dispatch: impl FnOnce(i64, &mut HookContext, u64) -> Result<(), reverie::Error>,
) -> Result<(), reverie::Error> {
    let _runtime = crate::runtime_domain::Entry::enter();
    if !crate::clock_control::active()
        || !crate::clock_control::paused()
        || !CURRENT_EVENT.get().is_null()
        || tool_callback_active()
        || !crate::clock_control::handoff_clear()
    {
        return Err(io::Error::other("guest progress lacks its captured paused context").into());
    }
    let (owner, clock) = provenance.authenticate(context)?;
    let _native = unsafe { NativeInstructionScope::all() };
    let _callback = ToolCallbackGuard::enter();
    dispatch(owner, context, clock)
}

#[cfg(all(test, feature = "private-crt"))]
thread_local! {
    static GUEST_CLOCK_READ_PROBE: Cell<Option<usize>> = const { Cell::new(None) };
}

#[cfg(all(test, feature = "private-crt"))]
pub(crate) fn with_guest_clock_read_probe<Output>(
    operation: impl FnOnce() -> Output,
) -> (Output, usize) {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            GUEST_CLOCK_READ_PROBE.set(None);
        }
    }
    assert!(GUEST_CLOCK_READ_PROBE.get().is_none());
    GUEST_CLOCK_READ_PROBE.set(Some(0));
    let _restore = Restore;
    let output = operation();
    (output, GUEST_CLOCK_READ_PROBE.get().unwrap())
}

pub(crate) unsafe fn dispatch_owned_vdso(
    context: &mut HookContext,
    operation: crate::vdso::Operation,
) -> i64 {
    let _runtime = crate::runtime_domain::Entry::enter();
    let _native = unsafe { NativeInstructionScope::all() };
    if let Err(error) = enter_rcb_handler() {
        exit_io126("owned-vdso/enter-clock", &error);
    }
    let mut event = SyscallEvent {
        owned_binding: None,
        exit: None,
        number: operation.number(),
        args: operation.arguments(context),
        instruction_pointer: context.instruction_pointer,
        result: UNSET_RESULT,
        context: context as *mut HookContext as usize,
    };
    {
        let _callback = ToolCallbackGuard::enter();
        let _event = CurrentEventGuard::enter(&mut event);
        crate::tool_host::dispatch_vdso(&mut event);
    }
    if let Err(error) = leave_rcb_handler() {
        exit_io126("owned-vdso/leave-clock", &error);
    }
    if event.result == UNSET_RESULT {
        unsafe {
            reverie_preload::trap::terminal126(
                "owned-vdso/unset-result",
                "raw-result",
                Some(event.result),
            )
        };
    }
    event.result
}

unsafe fn execute_native_instruction(kind: InstructionEventKind, context: &mut HookContext) {
    match kind {
        InstructionEventKind::Cpuid => {
            let mut result = NativeCpuidResult::default();
            unsafe {
                reverie_liteinst_native_cpuid(context.rax as u32, context.rcx as u32, &mut result)
            };
            context.rax = u64::from(result.eax);
            context.rbx = u64::from(result.ebx);
            context.rcx = u64::from(result.ecx);
            context.rdx = u64::from(result.edx);
        }
        InstructionEventKind::Rdtsc => {
            let value = unsafe { reverie_liteinst_native_rdtsc() };
            context.rax = value as u32 as u64;
            context.rdx = value >> 32;
        }
        InstructionEventKind::Rdtscp => {
            let mut aux = 0;
            let value = unsafe { reverie_liteinst_native_rdtscp(&mut aux) };
            context.rax = value as u32 as u64;
            context.rdx = value >> 32;
            context.rcx = u64::from(aux);
        }
    }
}

unsafe extern "C" fn installed_cpuid_body(context: *mut HookContext) {
    unsafe { installed_instruction_hook(context, InstructionEventKind::Cpuid) }
}

unsafe extern "C" fn installed_rdtsc_body(context: *mut HookContext) {
    unsafe { installed_instruction_hook(context, InstructionEventKind::Rdtsc) }
}

unsafe extern "C" fn installed_rdtscp_body(context: *mut HookContext) {
    unsafe { installed_instruction_hook(context, InstructionEventKind::Rdtscp) }
}

unsafe fn installed_syscall_hook_for(context: *mut HookContext, number: Option<i64>) {
    let _runtime = crate::runtime_domain::Entry::enter();
    if let Some(context) = unsafe { context.as_ref() }
        && let Some(site) = find_site(context.instruction_pointer)
    {
        site.hook_count.fetch_add(1, Ordering::Relaxed);
    }
    unsafe { dispatch_syscall_context(context, number) };
}

pub(crate) unsafe fn dispatch_fallback_context(context: *mut HookContext) {
    unsafe { dispatch_syscall_context(context, None) };
}

unsafe fn dispatch_syscall_context(context: *mut HookContext, number: Option<i64>) {
    let _runtime = crate::runtime_domain::Entry::enter();
    if context.is_null() {
        unsafe {
            exit_now(122);
        }
    }
    if enter_rcb_handler().is_err() {
        unsafe { exit_now(122) };
    }
    // SAFETY: generated LiteInst code passes a unique mutable saved frame.
    let context_pointer = context as usize;
    let context = unsafe { &mut *context };
    let mut event = SyscallEvent {
        owned_binding: None,
        exit: None,
        number: number.unwrap_or(context.rax as i64),
        args: [
            context.rdi,
            context.rsi,
            context.rdx,
            context.r10,
            context.r8,
            context.r9,
        ],
        instruction_pointer: context.instruction_pointer,
        result: UNSET_RESULT,
        context: context_pointer,
    };
    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(PR-133): Review guarded installed-hook bypass for Tool-internal syscalls.
    if tool_callback_active() {
        forward_nested_tool_syscall(&mut event);
        context.rax = event.result as u64;
        context.rcx = context.instruction_pointer.saturating_add(2);
        context.r11 = context.rflags;
        if leave_rcb_handler().is_err() {
            unsafe { exit_now(122) };
        }
        return;
    }
    {
        let _tool_callback = ToolCallbackGuard::enter();
        let _current_event = CurrentEventGuard::enter(&mut event);
        unsafe { tool_trampoline() };
    }
    if event.result == UNSET_RESULT {
        event.result = -i64::from(libc::ENOSYS);
    }
    context.rax = event.result as u64;
    context.rcx = context.instruction_pointer.saturating_add(2);
    context.r11 = context.rflags;
    if leave_rcb_handler().is_err() {
        unsafe { exit_now(122) };
    }
}

unsafe extern "C" fn installed_syscall_body(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, None) }
}

#[cfg(test)]
pub(crate) fn domain_test_syscall(number: i64) -> i64 {
    TOOL_MODE.store(TOOL_REVERIE, Ordering::Relaxed);
    let mut context: HookContext = unsafe { core::mem::zeroed() };
    context.rax = number as u64;
    unsafe { installed_syscall_hook_for(&mut context, None) };
    context.rax as i64
}

/// Test-only, default-off synthetic dispatch.
///
/// Drives the installed handler from a zeroed `HookContext` with no SUD, no
/// patching, no signal and no native instruction. This is the **non-owned**
/// dispatch route: it never reaches owned staging, owned capture admission or
/// owned injection classification, and it cannot establish owned readiness.
///
/// # Safety
///
/// `args` are placed into the synthetic context verbatim and become real
/// syscall arguments and real `LocalMemory` accesses, so any element the
/// selected `number` interprets as an address is dereferenced by the kernel or
/// by the installed Tool. This is not a safe arbitrary-raw-memory execution
/// API. The caller must guarantee, for the operation `number` names:
///
/// - every pointer argument addresses a live, correctly sized, correctly
///   aligned allocation the caller owns, valid for the reads and writes that
///   operation performs, and outliving this call;
/// - every length or count argument bounds those allocations;
/// - no argument names a descriptor or address the runtime owns privately;
/// - a handler is installed and the process is in the single-threaded,
///   RCB-clock-armed state this seam requires, with the call made from the
///   thread that will own the dispatch.
///
/// Passing integers that merely look plausible is undefined behaviour. Host
/// controls satisfy this by passing pointers to their own live buffers.
#[cfg(any(test, feature = "test-tool-host-dispatch"))]
#[doc(hidden)]
pub unsafe fn __dispatch_test_syscall(number: i64, args: [u64; 6]) -> i64 {
    TOOL_MODE.store(TOOL_REVERIE, Ordering::Relaxed);
    let mut context: HookContext = unsafe { core::mem::zeroed() };
    context.rax = number as u64;
    context.rdi = args[0];
    context.rsi = args[1];
    context.rdx = args[2];
    context.r10 = args[3];
    context.r8 = args[4];
    context.r9 = args[5];
    unsafe { installed_syscall_hook_for(&mut context, None) };
    context.rax as i64
}

/// Test-only, default-off RCB clock installation for a synthetic dispatch host.
#[cfg(any(test, feature = "test-tool-host-dispatch"))]
#[doc(hidden)]
pub fn __initialize_test_rcb_clock() -> io::Result<()> {
    initialize_rcb_clock()
}

#[cfg(test)]
pub(crate) fn domain_test_instruction() -> u64 {
    let mut previous = 0i32;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [
                    libc::PR_GET_TSC as u64,
                    (&raw mut previous) as u64,
                    0,
                    0,
                    0,
                    0,
                ],
            )
        },
        0
    );
    let mut context: HookContext = unsafe { core::mem::zeroed() };
    unsafe { installed_instruction_hook(&mut context, InstructionEventKind::Rdtsc) };
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [libc::PR_SET_TSC as u64, previous as u64, 0, 0, 0, 0],
            )
        },
        0
    );
    context.rax | context.rdx << 32
}

unsafe extern "C" fn installed_vdso_time_body(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_time)) }
}

unsafe extern "C" fn installed_vdso_clock_gettime_body(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_clock_gettime)) }
}

unsafe extern "C" fn installed_vdso_getcpu_body(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_getcpu)) }
}

unsafe extern "C" fn installed_vdso_gettimeofday_body(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_gettimeofday)) }
}

unsafe extern "C" fn installed_vdso_clock_getres_body(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_clock_getres)) }
}

crate::clock_control::installed_hook!(installed_syscall_hook, installed_syscall_body);
crate::clock_control::installed_hook!(installed_cpuid_hook, installed_cpuid_body);
crate::clock_control::installed_hook!(installed_rdtsc_hook, installed_rdtsc_body);
crate::clock_control::installed_hook!(installed_rdtscp_hook, installed_rdtscp_body);
crate::clock_control::installed_hook!(installed_vdso_time_hook, installed_vdso_time_body);
crate::clock_control::installed_hook!(
    installed_vdso_clock_gettime_hook,
    installed_vdso_clock_gettime_body
);
crate::clock_control::installed_hook!(installed_vdso_getcpu_hook, installed_vdso_getcpu_body);
crate::clock_control::installed_hook!(
    installed_vdso_gettimeofday_hook,
    installed_vdso_gettimeofday_body
);
crate::clock_control::installed_hook!(
    installed_vdso_clock_getres_hook,
    installed_vdso_clock_getres_body
);

unsafe fn locate_syscall_site(resume_address: u64) -> Option<u64> {
    let candidates = [resume_address.checked_sub(2), Some(resume_address)];
    for address in candidates.into_iter().flatten() {
        let Some(arena) = arena_for(address) else {
            continue;
        };
        if address.checked_add(2)? > arena.mapping_end {
            continue;
        }
        // SAFETY: the candidate lies inside a live executable mapping.
        let bytes = unsafe { core::slice::from_raw_parts(address as usize as *const u8, 2) };
        if bytes == [0x0F, 0x05] {
            return Some(address);
        }
    }
    None
}

type RecordFallbackStats = fn(crate::stats::GuestStatsHooks, u64);

struct LiteinstDispatcher {
    stats: crate::stats::GuestStatsHooks,
    record_fallback_stats: RecordFallbackStats,
    publication: PatchPublication,
}

impl LiteinstDispatcher {
    fn new(stats: crate::stats::GuestStatsHooks, publication: PatchPublication) -> Self {
        Self {
            stats,
            record_fallback_stats: if stats.is_enabled() {
                record_enabled_fallback_stats
            } else {
                record_disabled_fallback_stats
            },
            publication,
        }
    }
}

fn record_disabled_fallback_stats(_stats: crate::stats::GuestStatsHooks, _address: u64) {}

#[cfg(test)]
static ENABLED_FALLBACK_CLASSIFICATIONS: AtomicU64 = AtomicU64::new(0);

fn record_enabled_fallback_stats(stats: crate::stats::GuestStatsHooks, address: u64) {
    #[cfg(test)]
    ENABLED_FALLBACK_CLASSIFICATIONS.fetch_add(1, Ordering::Relaxed);
    let straddler =
        find_site(address).is_some_and(|site| site.straddle_prefix.load(Ordering::Relaxed) != 0);
    stats.record_path(if straddler {
        crate::stats::IN_GUEST_STRADDLER_FALLBACK
    } else {
        crate::stats::IN_GUEST_OTHER_FALLBACK
    });
}

impl SyscallDispatcher for LiteinstDispatcher {
    fn dispatch(&self, event: &mut PreloadSyscallEvent) {
        let _runtime = crate::runtime_domain::Entry::enter();
        if event.source() == SyscallEventSource::UserDispatch {
            crate::syscall_mode::record_sud();
        }
        if tool_callback_active() {
            self.stats.record_path(crate::stats::IN_GUEST_NESTED_SIGSYS);
            let mut nested = SyscallEvent {
                owned_binding: None,
                exit: None,
                number: event.number(),
                args: event.args(),
                instruction_pointer: event.instruction_pointer(),
                result: UNSET_RESULT,
                context: 0,
            };
            forward_nested_tool_syscall(&mut nested);
            event.set_result(nested.result);
            return;
        }
        self.stats.record_path(crate::stats::IN_GUEST_SIGSYS);
        if crate::syscall_mode::sud_only() {
            if event.source() != SyscallEventSource::UserDispatch {
                unsafe { exit_now(126) };
            }
            let Some(mask) = reverie_preload::user_dispatch::dispatch_mask() else {
                unsafe { exit_now(125) };
            };
            let Some(entry) =
                crate::syscall_fallback::prepare_masked(event.instruction_pointer(), mask)
            else {
                unsafe { exit_now(125) };
            };
            crate::syscall_mode::record_deferred();
            event.defer_to_clocked_with_mask(entry, entry, crate::syscall_fallback::runtime_mask());
            return;
        }
        let mode = TOOL_MODE.load(Ordering::Relaxed);
        let args = event.args();
        let compatibility_trap_fallback =
            // AUTONOMOUS-BOT-IMPLEMENTED
            (event.number() == libc::SYS_clone && clone_is_fork_like(args[0], args[1]))
            // AUTONOMOUS-BOT-IMPLEMENTED
            || event.number() == libc::SYS_wait4;
        // TODO-HUMAN-REVIEW(PR-127): Review fork and wait libc-wrapper trap fallbacks.
        if mode != TOOL_REVERIE && compatibility_trap_fallback {
            let mut trapped = SyscallEvent {
                owned_binding: None,
                exit: None,
                number: event.number(),
                args,
                instruction_pointer: event.instruction_pointer(),
                result: UNSET_RESULT,
                context: 0,
            };
            unsafe {
                process_syscall(&mut trapped);
            }
            event.set_result(trapped.result);
            return;
        }

        let resume_address = event.instruction_pointer();
        let instruction_pointer =
            unsafe { locate_syscall_site(resume_address) }.unwrap_or(resume_address);

        if let Some((site, claimed)) = claim_site(instruction_pointer) {
            site.trap_count.fetch_add(1, Ordering::Relaxed);
            if claimed {
                let native = unsafe { NativeInstructionScope::all() };
                let installed = unsafe {
                    install_site_hook(
                        instruction_pointer,
                        site,
                        installed_syscall_hook,
                        self.publication,
                        &[0x0f, 0x05],
                        true,
                    )
                };
                drop(native);
                if installed.is_err() {
                    site.state.store(SITE_FALLBACK, Ordering::Release);
                }
            }
            while matches!(site.state.load(Ordering::Acquire), 0 | SITE_INSTALLING) {
                core::hint::spin_loop();
            }
            if site.state.load(Ordering::Acquire) == SITE_ACTIVE {
                let hook = site.hook.load(Ordering::Acquire);
                if !hook.is_null() {
                    // SAFETY: active sites retain their InstalledHook for process lifetime.
                    let trampoline = unsafe { (*hook).trampoline() };
                    let witness = crate::clock_control::callback_return_pc(trampoline)
                        .unwrap_or_else(|_| unsafe { exit_now(127) });
                    event.defer_to_clocked(trampoline.address(), witness);
                    return;
                }
            }
        }

        // AUTONOMOUS-BOT-IMPLEMENTED
        record_fallback_dispatch(event.number());
        (self.record_fallback_stats)(self.stats, instruction_pointer);
        if mode == TOOL_REVERIE
            && let Some(entry) = crate::syscall_fallback::prepare(resume_address)
        {
            event.defer_to_clocked(entry, entry);
            return;
        }
        event.fail(libc::EOPNOTSUPP);
    }
}

unsafe extern "C" fn tool_trampoline() {
    let _runtime = crate::runtime_domain::Entry::enter();
    let event = CURRENT_EVENT.get();
    if event.is_null() {
        unsafe {
            exit_now(123);
        }
    }
    unsafe {
        process_syscall(&mut *event);
    }
}

// TODO-HUMAN-REVIEW(PR-127): Review process-global preload safety guards.
fn protect_runtime_control(event: &mut SyscallEvent) -> bool {
    let unsupported_process =
        is_fork_like(event.number) && !PROCESS_FORKS_ALLOWED.load(Ordering::Acquire);
    let protected_signal =
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-133): Review fail-closed guest signal-handler policy.
        !signal_action_supported(event.number, event.args)
        // AUTONOMOUS-BOT-IMPLEMENTED
        || (event.number == libc::SYS_sigaltstack && event.args[0] != 0)
        // AUTONOMOUS-BOT-IMPLEMENTED
        || (event.number == libc::SYS_rt_sigprocmask && event.args[1] != 0);

    if unsupported_process {
        event.result = -i64::from(libc::ENOTSUP);
    } else if protected_signal {
        event.result = -i64::from(libc::EPERM);
    } else {
        return false;
    }
    true
}

unsafe fn process_syscall(event: &mut SyscallEvent) {
    let _runtime = crate::runtime_domain::Entry::enter();
    let tool_mode = TOOL_MODE.load(Ordering::Relaxed);
    // AUTONOMOUS-BOT-IMPLEMENTED
    if matches!(event.number, libc::SYS_execve | libc::SYS_execveat) {
        event.result = -i64::from(libc::ENOTSUP);
        if tool_mode != TOOL_REVERIE {
            unsafe {
                trace_event(event);
            }
        }
        return;
    }
    if tool_mode == TOOL_REVERIE && protect_runtime_control(event) {
        return;
    }
    if TOOL_MODE.load(Ordering::Relaxed) == TOOL_REVERIE {
        crate::tool_host::dispatch(event);
        observe_mapping_generation(event);
        return;
    }
    if TOOL_MODE.load(Ordering::Relaxed) == TOOL_COMPAT
        && EVENT_COOKIE.load(Ordering::Relaxed) != 0
        && unsafe { protect_compatibility_event_channel(event) }
    {
        return;
    }

    if TOOL_MODE.load(Ordering::Relaxed) == TOOL_COMPAT
        && matches!(
            event.number,
            libc::SYS_setpgid | libc::SYS_setsid | libc::SYS_setns | libc::SYS_unshare
        )
    {
        event.result = -i64::from(libc::EPERM);
        unsafe {
            trace_event(event);
        }
        return;
    }

    if event.number == libc::SYS_clone && !clone_is_fork_like(event.args[0], event.args[1]) {
        event.result = if TOOL_MODE.load(Ordering::Relaxed) == TOOL_COMPAT {
            -i64::from(libc::EPERM)
        } else {
            -i64::from(libc::ENOTSUP)
        };
        unsafe {
            trace_event(event);
        }
        return;
    }

    if event.number == libc::SYS_exit || event.number == libc::SYS_exit_group {
        unsafe {
            trace_event(event);
        }
    }

    let compatibility_fork = TOOL_MODE.load(Ordering::Relaxed) == TOOL_COMPAT
        && matches!(event.number, libc::SYS_clone | libc::SYS_fork);
    if compatibility_fork {
        unsafe {
            trace_event(event);
        }
    }
    event.result = forward_kernel_syscall(event.number, event.args, raw_syscall6);
    observe_mapping_generation(event);

    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(PR-260): Review the fork-following observability reset call.
    // In the child of a successful fork-like syscall (`result == 0`), the
    // COW-inherited observability counters describe the parent, not this child.
    // Reset them through the shared ForkHook seam so per-process attribution
    // starts clean. Gating on a zero result is sufficient and mirrors
    // e9patch's child-side reset.
    if is_fork_like(event.number) && event.result == 0 {
        FORK_HOOK.run_in_child();
    }

    if event.number != libc::SYS_exit && event.number != libc::SYS_exit_group && !compatibility_fork
    {
        unsafe {
            trace_event(event);
        }
    }
}

fn clone_is_fork_like(flags: u64, child_stack: u64) -> bool {
    const SIGNAL_MASK: u64 = 0xff;
    let allowed_flags =
        (libc::CLONE_CHILD_CLEARTID | libc::CLONE_CHILD_SETTID | libc::CLONE_PARENT_SETTID) as u64;
    child_stack == 0
        && flags & SIGNAL_MASK == libc::SIGCHLD as u64
        && flags & !(SIGNAL_MASK | allowed_flags) == 0
}

unsafe fn protect_coordinator_channel(event: &mut SyscallEvent) -> bool {
    if let Some(result) = crate::protected_fd::indirect_result(
        event.number,
        event.args,
        &[
            COORDINATOR_FD.load(Ordering::Acquire),
            crate::guest_log::LOG_FD.load(Ordering::Acquire),
            crate::clock_control::descriptor(),
            crate::clock_control::notification_descriptor(),
            reverie_preload::signal::runtime_signal_descriptors()[0],
            reverie_preload::signal::runtime_signal_descriptors()[1],
        ],
    ) {
        event.result = result;
        return true;
    }
    let first = u64::from(event.args[0] as u32);
    let second = u64::from(event.args[1] as u32);
    for fd in [
        COORDINATOR_FD.load(Ordering::Acquire),
        crate::guest_log::LOG_FD.load(Ordering::Acquire),
        crate::clock_control::descriptor(),
        crate::clock_control::notification_descriptor(),
        reverie_preload::signal::runtime_signal_descriptors()[0],
        reverie_preload::signal::runtime_signal_descriptors()[1],
    ] {
        if fd < 0 {
            continue;
        }
        let fd = fd as u64;
        if event.number == libc::SYS_close && first == fd {
            event.result = 0;
        } else if event.number == libc::SYS_close_range && first <= fd && fd <= second {
            event.result = unsafe { close_range_preserving_event_fd(event, fd) };
        } else if syscall_targets_event_fd(event, fd) {
            event.result = -i64::from(libc::EBADF);
        } else {
            continue;
        }
        return true;
    }
    false
}

pub(crate) fn protected_injected_syscall(number: i64, args: [u64; 6]) -> Option<i64> {
    let mut event = SyscallEvent {
        owned_binding: None,
        exit: None,
        number,
        args,
        instruction_pointer: 0,
        result: 0,
        context: 0,
    };
    if unsafe { protect_coordinator_channel(&mut event) } {
        Some(event.result)
    } else {
        None
    }
}

pub(crate) fn guarded_raw_syscall(number: i64, args: [u64; 6]) -> i64 {
    guarded_syscall_with(number, args, raw_syscall6)
}

pub(crate) fn guarded_scoped_syscall(number: i64, args: [u64; 6]) -> i64 {
    guarded_syscall_with(
        number,
        args,
        reverie_preload::user_dispatch::forward_syscall,
    )
}

fn guarded_syscall_with(
    number: i64,
    args: [u64; 6],
    forward: unsafe fn(i64, [u64; 6]) -> i64,
) -> i64 {
    if let Some(result) = protected_injected_syscall(number, args) {
        return result;
    }
    match crate::protected_fd::batch_args(
        number,
        args,
        &[
            COORDINATOR_FD.load(Ordering::Acquire),
            crate::guest_log::LOG_FD.load(Ordering::Acquire),
            crate::clock_control::descriptor(),
            crate::clock_control::notification_descriptor(),
            reverie_preload::signal::runtime_signal_descriptors()[0],
            reverie_preload::signal::runtime_signal_descriptors()[1],
        ],
    ) {
        Ok(args) => forward_kernel_syscall(number, args, forward),
        Err(error) => error,
    }
}

fn forward_kernel_syscall(
    number: i64,
    mut args: [u64; 6],
    forward: unsafe fn(i64, [u64; 6]) -> i64,
) -> i64 {
    let dirfds: &[usize] = match number {
        libc::SYS_openat | libc::SYS_newfstatat | libc::SYS_unlinkat => &[0],
        libc::SYS_renameat | libc::SYS_linkat => &[0, 2],
        _ => &[],
    };
    if dirfds.iter().any(|&index| args[index] as i32 >= 0) {
        let compatibility_fd = if TOOL_MODE.load(Ordering::Acquire) == TOOL_COMPAT
            && EVENT_COOKIE.load(Ordering::Acquire) != 0
        {
            EVENT_FD.load(Ordering::Acquire)
        } else {
            -1
        };
        let signal_fds = reverie_preload::signal::runtime_signal_descriptors();
        let protected = [
            COORDINATOR_FD.load(Ordering::Acquire),
            crate::guest_log::LOG_FD.load(Ordering::Acquire),
            crate::clock_control::descriptor(),
            crate::clock_control::notification_descriptor(),
            signal_fds[0],
            signal_fds[1],
            compatibility_fd,
        ];
        for &index in dirfds {
            if args[index] as i32 >= 0 && protected.contains(&(args[index] as i32)) {
                args[index] = (-1_i32) as u64;
            }
        }
    }
    unsafe { forward(number, args) }
}

unsafe fn protect_compatibility_event_channel(event: &mut SyscallEvent) -> bool {
    let event_fd = EVENT_FD.load(Ordering::Acquire) as u64;

    if event.number == libc::SYS_close && event.args[0] == event_fd {
        // The descriptor is controller-owned and intentionally invisible to
        // guest descriptor lifecycle management.
        event.result = 0;
    } else if event.number == libc::SYS_close_range
        && event.args[0] <= event_fd
        && event_fd <= event.args[1]
    {
        event.result = unsafe { close_range_preserving_event_fd(event, event_fd) };
    } else if syscall_targets_event_fd(event, event_fd) {
        event.result = -i64::from(libc::EBADF);
    } else {
        return false;
    }

    unsafe {
        trace_event(event);
    }
    true
}

unsafe fn close_range_preserving_event_fd(event: &SyscallEvent, event_fd: u64) -> i64 {
    const CLOSE_RANGE_UNSHARE: u64 = 1 << 1;
    const CLOSE_RANGE_CLOEXEC: u64 = 1 << 2;

    let mut first = u64::from(event.args[0] as u32);
    let last = u64::from(event.args[1] as u32);
    let mut flags = u64::from(event.args[2] as u32);
    if flags & !(CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC) != 0 {
        return -i64::from(libc::EINVAL);
    }
    if flags & CLOSE_RANGE_UNSHARE != 0 {
        let result =
            unsafe { raw_syscall6(libc::SYS_unshare, [libc::CLONE_FILES as u64, 0, 0, 0, 0, 0]) };
        if result < 0 {
            return result;
        }
        flags &= !CLOSE_RANGE_UNSHARE;
    }

    let mut reserved = [
        event_fd,
        COORDINATOR_FD.load(Ordering::Acquire) as u64,
        crate::guest_log::LOG_FD.load(Ordering::Acquire) as u64,
        crate::clock_control::descriptor() as u64,
        crate::clock_control::notification_descriptor() as u64,
        reverie_preload::signal::runtime_signal_descriptors()[0] as u64,
        reverie_preload::signal::runtime_signal_descriptors()[1] as u64,
    ];
    reserved.sort_unstable();
    for fd in reserved {
        if fd < first || fd > last {
            continue;
        }
        if first < fd {
            let result =
                unsafe { raw_syscall6(libc::SYS_close_range, [first, fd - 1, flags, 0, 0, 0]) };
            if result < 0 {
                return result;
            }
        }
        first = fd + 1;
    }
    if first <= last {
        let result = unsafe { raw_syscall6(libc::SYS_close_range, [first, last, flags, 0, 0, 0]) };
        if result < 0 {
            return result;
        }
    }
    0
}

/// Whether the operation names one of the runtime's private descriptors in a
/// descriptor operand.
///
/// `fstat` must not expose private-descriptor metadata. `openat` is deliberately
/// deferred to `forward_kernel_syscall`: Linux interprets its pathname with an
/// invalid directory operand, without a runtime pathname probe or an early open.
fn syscall_targets_event_fd(event: &SyscallEvent, event_fd: u64) -> bool {
    let args = event.args.map(|arg| u64::from(arg as u32));
    match event.number {
        libc::SYS_read
        | libc::SYS_readv
        | libc::SYS_pread64
        | libc::SYS_preadv
        | libc::SYS_preadv2
        | libc::SYS_write
        | libc::SYS_writev
        | libc::SYS_pwrite64
        | libc::SYS_pwritev
        | libc::SYS_pwritev2
        | libc::SYS_vmsplice
        | libc::SYS_lseek
        | libc::SYS_fsync
        | libc::SYS_ftruncate
        | libc::SYS_fallocate
        | libc::SYS_shutdown
        | libc::SYS_sendto
        | libc::SYS_recvfrom
        | libc::SYS_sendmsg
        | libc::SYS_recvmsg
        | libc::SYS_sendmmsg
        | libc::SYS_recvmmsg
        | libc::SYS_setsockopt
        | libc::SYS_getsockopt
        | libc::SYS_fcntl
        | libc::SYS_ioctl
        | libc::SYS_fstat
        | libc::SYS_inotify_add_watch
        | libc::SYS_getdents64
        | libc::SYS_bind
        | libc::SYS_getsockname
        | libc::SYS_dup => args[0] == event_fd,
        libc::SYS_dup2 | libc::SYS_dup3 | libc::SYS_sendfile => {
            args[0] == event_fd || args[1] == event_fd
        }
        libc::SYS_mmap => args[3] & libc::MAP_ANONYMOUS as u64 == 0 && args[4] == event_fd,
        libc::SYS_splice | libc::SYS_copy_file_range => args[0] == event_fd || args[2] == event_fd,
        libc::SYS_tee => args[0] == event_fd || args[1] == event_fd,
        _ => false,
    }
}

unsafe fn compatibility_event_channel_is_intact(output_fd: libc::c_int) -> bool {
    let mut metadata: libc::stat = unsafe { core::mem::zeroed() };
    let result = unsafe {
        raw_syscall6(
            libc::SYS_fstat,
            [output_fd as u64, (&raw mut metadata) as u64, 0, 0, 0, 0],
        )
    };
    result == 0
        && metadata.st_dev == EVENT_DEVICE.load(Ordering::Acquire)
        && metadata.st_ino == EVENT_INODE.load(Ordering::Acquire)
}

unsafe fn write_compatibility_event(output_fd: libc::c_int, bytes: &[u8]) {
    const MAX_BACKPRESSURE_RETRIES: usize = 20;
    const BACKPRESSURE_POLL_MILLISECONDS: u64 = 100;

    if unsafe { !compatibility_event_channel_is_intact(output_fd) } {
        unsafe {
            exit_now(EVENT_CHANNEL_IDENTITY_FAILURE_STATUS);
        }
    }
    for attempt in 0..=MAX_BACKPRESSURE_RETRIES {
        let written = unsafe {
            raw_syscall6(
                libc::SYS_write,
                [
                    output_fd as u64,
                    bytes.as_ptr() as u64,
                    bytes.len() as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        if written == bytes.len() as i64 {
            return;
        }
        if written != -i64::from(libc::EAGAIN) && written != -i64::from(libc::EINTR) {
            break;
        }
        if attempt == MAX_BACKPRESSURE_RETRIES {
            break;
        }
        let mut descriptor = libc::pollfd {
            fd: output_fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let _ = unsafe {
            raw_syscall6(
                libc::SYS_poll,
                [
                    (&raw mut descriptor) as u64,
                    1,
                    BACKPRESSURE_POLL_MILLISECONDS,
                    0,
                    0,
                    0,
                ],
            )
        };
    }
    unsafe {
        exit_now(EVENT_CHANNEL_WRITE_FAILURE_STATUS);
    }
}

unsafe fn trace_event(event: &SyscallEvent) {
    let mode = TOOL_MODE.load(Ordering::Relaxed);
    let output_fd;
    let mut line = StackLine::new();
    if mode == TOOL_COMPAT {
        output_fd = EVENT_FD.load(Ordering::Acquire);
        line.push_bytes(b"reverie-liteinst: tool=compat");
        let cookie = EVENT_COOKIE.load(Ordering::Acquire);
        if cookie != 0 {
            line.push_bytes(b" cookie=");
            line.push_unsigned(cookie);
            line.push_bytes(b" pid=");
            line.push_signed(unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) });
        }
        line.push_bytes(b" syscall=");
        line.push_signed(event.number);
    } else {
        return;
    }
    line.push_bytes(b"\n");

    if mode == TOOL_COMPAT && EVENT_COOKIE.load(Ordering::Relaxed) != 0 {
        unsafe {
            write_compatibility_event(output_fd, &line.bytes[..line.len]);
        }
    } else {
        let _ = unsafe {
            raw_syscall6(
                libc::SYS_write,
                [
                    output_fd as u64,
                    line.bytes.as_ptr() as u64,
                    line.len as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
    }
}

/// Emit an allocation-free stage marker from the in-guest Tool process.
///
/// This is opt-in because production guests own stderr. When enabled, a short
/// write is fail-closed so an absent marker cannot be mistaken for a negative
/// observation across the host/in-guest process boundary.
pub(crate) fn emit_in_guest_stage(stage: &[u8]) {
    if !IN_GUEST_STAGE_STREAM.load(Ordering::Acquire) {
        return;
    }
    let mut line = StackLine::new();
    line.push_bytes(b"INFO reverie_liteinst::tool_host: [in-guest pid=");
    line.push_signed(unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) });
    line.push_bytes(b" tid=");
    line.push_signed(unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) });
    line.push_bytes(b"] stage=");
    line.push_bytes(stage);
    line.push_bytes(b"\n");
    let written = unsafe {
        raw_syscall6(
            libc::SYS_write,
            [
                libc::STDERR_FILENO as u64,
                line.bytes.as_ptr() as u64,
                line.len as u64,
                0,
                0,
                0,
            ],
        )
    };
    if written != line.len as i64 {
        unsafe { exit_now(IN_GUEST_STAGE_WRITE_FAILURE_STATUS) };
    }
}

fn emit_instruction_refusal_stage(
    stage: &[u8],
    rip_offset: u64,
    fault_address: u64,
    stack_pointer: u64,
    mapping_name: &[u8],
    mapping_len: u64,
    bytes: &[u8],
) {
    if !IN_GUEST_STAGE_STREAM.load(Ordering::Acquire) {
        return;
    }
    let mut line = StackLine::new();
    line.push_bytes(b"INFO reverie_liteinst::tool_host: [in-guest pid=");
    line.push_signed(unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) });
    line.push_bytes(b" tid=");
    line.push_signed(unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) });
    line.push_bytes(b"] stage=");
    line.push_bytes(stage);
    line.push_bytes(b" rip-offset=0x");
    line.push_hex(rip_offset);
    line.push_bytes(b" fault=0x");
    line.push_hex(fault_address);
    line.push_bytes(b" rsp=0x");
    line.push_hex(stack_pointer);
    line.push_bytes(b" map=");
    line.push_bytes(mapping_name);
    line.push_bytes(b" map-len=0x");
    line.push_hex(mapping_len);
    line.push_bytes(b" bytes=");
    for (index, byte) in bytes.iter().enumerate() {
        if index != 0 {
            line.push_bytes(b"-");
        }
        line.push_hex_byte(*byte);
    }
    line.push_bytes(b"\n");
    let written = unsafe {
        raw_syscall6(
            libc::SYS_write,
            [
                libc::STDERR_FILENO as u64,
                line.bytes.as_ptr() as u64,
                line.len as u64,
                0,
                0,
                0,
            ],
        )
    };
    if written != line.len as i64 {
        unsafe { exit_now(IN_GUEST_STAGE_WRITE_FAILURE_STATUS) };
    }
}

#[track_caller]
pub(crate) fn exit_io126(operation: &str, error: &io::Error) -> ! {
    let (detail, value) = match error.raw_os_error() {
        Some(errno) => ("io-errno", i64::from(errno)),
        None => ("io-error-kind", error.kind() as i64),
    };
    unsafe { reverie_preload::trap::terminal126(operation, detail, Some(value)) }
}

#[track_caller]
pub(crate) unsafe fn exit_now(code: i32) -> ! {
    if code == 126 {
        unsafe { reverie_preload::trap::terminal126("runtime", "predicate", None) }
    }
    let _ = unsafe { raw_syscall6(libc::SYS_exit_group, [code as u64, 0, 0, 0, 0, 0]) };
    loop {
        core::hint::spin_loop();
    }
}

struct StackLine {
    bytes: [u8; 192],
    len: usize,
}

impl StackLine {
    const fn new() -> Self {
        Self {
            bytes: [0; 192],
            len: 0,
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        let available = self.bytes.len().saturating_sub(self.len);
        let count = available.min(bytes.len());
        self.bytes[self.len..self.len + count].copy_from_slice(&bytes[..count]);
        self.len += count;
    }

    fn push_signed(&mut self, value: i64) {
        if value < 0 {
            self.push_bytes(b"-");
        }
        self.push_unsigned(value.unsigned_abs());
    }

    fn push_unsigned(&mut self, mut value: u64) {
        let mut digits = [0_u8; 20];
        let mut cursor = digits.len();
        loop {
            cursor -= 1;
            digits[cursor] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        self.push_bytes(&digits[cursor..]);
    }

    fn push_hex(&mut self, mut value: u64) {
        let mut digits = [0_u8; 16];
        let mut cursor = digits.len();
        loop {
            cursor -= 1;
            let digit = (value & 0xf) as u8;
            digits[cursor] = if digit < 10 {
                b'0' + digit
            } else {
                b'a' + digit - 10
            };
            value >>= 4;
            if value == 0 {
                break;
            }
        }
        self.push_bytes(&digits[cursor..]);
    }

    fn push_hex_byte(&mut self, value: u8) {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        self.push_bytes(&[
            DIGITS[usize::from(value >> 4)],
            DIGITS[usize::from(value & 0xf)],
        ]);
    }
}

#[cfg(test)]
mod tests {
    #[derive(Default)]
    struct FdRoutingTool {
        expected: [u64; 6],
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        inject: bool,
    }

    #[reverie::tool]
    impl reverie::Tool for FdRoutingTool {
        type GlobalState = ();
        type ThreadState = ();

        async fn handle_syscall_event<G: reverie::Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: reverie::syscalls::Syscall,
        ) -> Result<i64, reverie::Error> {
            use reverie::syscalls::SyscallInfo;
            let (_, args) = syscall.into_parts();
            assert_eq!(
                [
                    args.arg0, args.arg1, args.arg2, args.arg3, args.arg4, args.arg5
                ],
                self.expected.map(|value| value as usize)
            );
            assert_eq!(self.calls.fetch_add(1, Ordering::SeqCst), 0);
            assert!(unsafe { libc::fcntl(self.expected[0] as i32, libc::F_GETFD) } >= 0);
            if self.inject {
                if crate::owned_context::syscall_mode() {
                    use std::io::Write;
                    assert_eq!(syscall.number(), reverie::syscalls::Sysno::close_range);
                    std::io::stderr()
                        .write_all(b"FD_ROUTING_BEFORE_OWNED_INJECT close_range\n")
                        .unwrap();
                }
                guest.inject(syscall).await.map_err(Into::into)
            } else {
                Err(reverie::Errno::ENOSYS.into())
            }
        }
    }

    fn fd_routing_child(test: &str, case: &str) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env("REVERIE_FD_ROUTING_CASE", case)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {:?}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fd_routing_case(case: &str, inject: bool) {
        use std::os::fd::AsRawFd;
        use std::os::fd::FromRawFd;

        use reverie::syscalls::Sysno;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("routing.sock");
        let server_path = path.clone();
        let (ready, wait) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let executor = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            executor.block_on(async {
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
        let rpc = crate::rpc::CoordinatorRpc::connect(&path).unwrap();
        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
            0
        );
        let reader = unsafe { std::fs::File::from_raw_fd(pipe[0]) };
        let writer = unsafe { std::fs::File::from_raw_fd(pipe[1]) };
        let protected = writer.as_raw_fd();
        super::COORDINATOR_FD.store(protected, Ordering::Release);
        super::TOOL_MODE.store(super::TOOL_REVERIE, Ordering::Release);
        let byte = b'R';
        let aliased_fd = protected as u64 | (1u64 << 32);
        let (number, args, expected) = match case {
            "close" => (libc::SYS_close, [aliased_fd, 19, 23, 29, 31, 37], 0),
            "write" | "neighbor" => (
                libc::SYS_write,
                [aliased_fd, (&raw const byte) as u64, 1, 29, 31, 37],
                if case == "neighbor" {
                    1
                } else {
                    -i64::from(libc::EBADF)
                },
            ),
            value if value.starts_with("range") => {
                let flags = case
                    .strip_prefix("range")
                    .unwrap()
                    .trim_end_matches("-clear")
                    .parse::<u64>()
                    .unwrap();
                (
                    libc::SYS_close_range,
                    [aliased_fd, aliased_fd, flags | (1u64 << 32), 29, 31, 37],
                    -i64::from(libc::ENOSYS),
                )
            }
            _ => panic!("unknown routing case"),
        };
        if case == "neighbor" || case.ends_with("-clear") {
            super::COORDINATOR_FD.store(reader.as_raw_fd(), Ordering::Release);
        }
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let subscriptions = [Sysno::new(number as usize).unwrap()].into_iter().collect();
        crate::tool_host::__install_dispatch_only_tool_host(
            FdRoutingTool {
                expected: args,
                calls: calls.clone(),
                inject,
            },
            rpc,
            &subscriptions,
        );
        let mut event = super::SyscallEvent {
            owned_binding: None,
            exit: None,
            number,
            args,
            instruction_pointer: 0x4000,
            result: super::UNSET_RESULT,
            context: 0,
        };
        if std::env::var_os("REVERIE_FD_ROUTING_OWNED").is_some() {
            crate::owned_context::with_routing_syscall_state(|| unsafe {
                super::process_syscall(&mut event)
            })
            .unwrap();
        } else {
            unsafe { super::process_syscall(&mut event) };
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "original operation must reach the installed Tool"
        );
        assert_eq!(event.number, number);
        assert_eq!(event.args, args);
        assert_eq!(event.instruction_pointer, 0x4000);
        assert_eq!(event.context, 0);
        assert_eq!(event.result, expected);
        assert!(unsafe { libc::fcntl(protected, libc::F_GETFD) } >= 0);
        let mut output = [0u8; 2];
        let count =
            unsafe { libc::read(reader.as_raw_fd(), output.as_mut_ptr().cast(), output.len()) };
        if case == "neighbor" {
            assert_eq!(
                count, 1,
                "exactly one native byte, not duplicate forwarding"
            );
            assert_eq!(output[0], byte);
        } else {
            assert_eq!(count, -1, "emulation/protection must not write");
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EAGAIN)
            );
        }
        super::COORDINATOR_FD.store(-1, Ordering::Release);
    }

    #[test]
    fn fd_routing_original_ranges_reach_installed_tool_without_effects() {
        if let Ok(case) = std::env::var("REVERIE_FD_ROUTING_CASE") {
            fd_routing_case(&case, false);
            return;
        }
        for case in [
            "range0",
            "range2",
            "range4",
            "range6",
            "range8",
            "range0-clear",
            "range2-clear",
            "range4-clear",
            "range8-clear",
        ] {
            fd_routing_child(
                "runtime::tests::fd_routing_original_ranges_reach_installed_tool_without_effects",
                case,
            );
        }
    }

    #[test]
    fn fd_routing_installed_tool_injection_preserves_protected_effects() {
        if let Ok(case) = std::env::var("REVERIE_FD_ROUTING_CASE") {
            fd_routing_case(&case, true);
            return;
        }
        for case in ["close", "write"] {
            fd_routing_child(
                "runtime::tests::fd_routing_installed_tool_injection_preserves_protected_effects",
                case,
            );
        }
    }

    #[test]
    fn fd_routing_installed_tool_neighbor_writes_once() {
        if let Ok(case) = std::env::var("REVERIE_FD_ROUTING_CASE") {
            fd_routing_case(&case, true);
            return;
        }
        fd_routing_child(
            "runtime::tests::fd_routing_installed_tool_neighbor_writes_once",
            "neighbor",
        );
    }

    fn fd_routing_owned_refusal(test: &str, case: &str) {
        if let Ok(selection) = std::env::var("REVERIE_FD_ROUTING_CASE") {
            assert_eq!(selection, case);
            fd_routing_case(&selection, true);
            panic!("owned close_range returned instead of refusing injection");
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env("REVERIE_FD_ROUTING_CASE", case)
            .env("REVERIE_FD_ROUTING_OWNED", "1")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(119),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!(
                "FD_ROUTING_BEFORE_OWNED_INJECT close_range\nhermit-liteinst owned injection refused: stage=owned-injection cause=unsupported-syscall number={}\n",
                libc::SYS_close_range
            )
        );
    }

    #[test]
    fn fd_routing_owned_overlap_injection_refuses() {
        fd_routing_owned_refusal(
            "runtime::tests::fd_routing_owned_overlap_injection_refuses",
            "range0",
        );
    }

    #[test]
    fn fd_routing_owned_nonoverlap_injection_refuses() {
        fd_routing_owned_refusal(
            "runtime::tests::fd_routing_owned_nonoverlap_injection_refuses",
            "range0-clear",
        );
    }

    #[test]
    fn owned_exit_event_scope_leaves_callback_and_restores_outer_event() {
        use super::*;
        let mut outer = SyscallEvent {
            owned_binding: None,
            exit: None,
            number: libc::SYS_getpid,
            args: [0; 6],
            instruction_pointer: 0x4000,
            result: UNSET_RESULT,
            context: 0,
        };
        let mut inner = SyscallEvent {
            owned_binding: None,
            exit: None,
            number: libc::SYS_exit_group,
            args: [130, 11, 22, 33, 44, 55],
            instruction_pointer: 0x5000,
            result: UNSET_RESULT,
            context: 0,
        };
        assert!(!nested_tool_callback());
        let previous = CURRENT_EVENT.get();
        {
            let _outer = CurrentEventGuard::enter(&mut outer);
            {
                let _callback = ToolCallbackGuard::enter();
                let _inner = CurrentEventGuard::enter(&mut inner);
                assert!(nested_tool_callback());
                assert_eq!(CURRENT_EVENT.get(), &raw mut inner);
                assert_eq!(inner.args, [130, 11, 22, 33, 44, 55]);
            }
            assert!(!nested_tool_callback());
            assert_eq!(CURRENT_EVENT.get(), &raw mut outer);
            assert_eq!(inner.result, UNSET_RESULT);
        }
        assert_eq!(CURRENT_EVENT.get(), previous);
        assert!(!nested_tool_callback());
    }

    mod nested_tool_forwarding {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;

        use super::super::*;

        fn event(number: i64, args: [u64; 6]) -> SyscallEvent {
            SyscallEvent {
                owned_binding: None,
                exit: None,
                number,
                args,
                instruction_pointer: 0,
                result: UNSET_RESULT,
                context: 0,
            }
        }

        #[test]
        fn clocked_preload_origin_preserves_actual_callback_authority() {
            crate::runtime_domain::tests::with_clocked_preload(|| {
                assert_eq!(
                    unsafe {
                        crate::runtime_domain::interrupted_runtime_in_clocked_preload_handler()
                    },
                    Some(false)
                );
                {
                    let _callback = ToolCallbackGuard::enter();
                    assert_eq!(
                        unsafe {
                            crate::runtime_domain::interrupted_runtime_in_clocked_preload_handler()
                        },
                        Some(true)
                    );
                    let mut request = event(libc::SYS_getpid, [0; 6]);
                    forward_nested_tool_syscall(&mut request);
                    assert_eq!(request.result, unsafe {
                        raw_syscall6(libc::SYS_getpid, [0; 6])
                    });
                    crate::runtime_domain::tests::with_clocked_preload(|| {
                        assert_eq!(
                            unsafe {
                                crate::runtime_domain::interrupted_runtime_in_clocked_preload_handler()
                            },
                            Some(true)
                        );
                    });
                }
                assert_eq!(
                    unsafe {
                        crate::runtime_domain::interrupted_runtime_in_clocked_preload_handler()
                    },
                    Some(false)
                );
            });
            assert!(!tool_callback_active());
            assert!(!crate::runtime_domain::allocation_active());
        }

        #[test]
        fn requires_callback_scope_and_retains_process_signal_control_guards() {
            let mut request = event(libc::SYS_getpid, [0; 6]);
            forward_nested_tool_syscall(&mut request);
            assert_eq!(request.result, -i64::from(libc::EPERM));
            {
                let _runtime = crate::runtime_domain::Entry::enter();
                let _callback = ToolCallbackGuard::enter();
                forward_nested_tool_syscall(&mut request);
                assert_eq!(request.result, i64::from(std::process::id()));
                for (number, expected) in [
                    (libc::SYS_clone, libc::ENOTSUP),
                    (libc::SYS_execve, libc::ENOTSUP),
                    (libc::SYS_rt_sigaction, libc::EPERM),
                    (libc::SYS_rt_sigprocmask, libc::EPERM),
                    (libc::SYS_sigaltstack, libc::EPERM),
                    (libc::SYS_ppoll, libc::EPERM),
                ] {
                    let mut request = event(number, [0; 6]);
                    forward_nested_tool_syscall(&mut request);
                    assert_eq!(request.result, -i64::from(expected));
                }
            }
            forward_nested_tool_syscall(&mut request);
            assert_eq!(request.result, -i64::from(libc::EPERM));
        }

        #[test]
        fn callback_output_keeps_captured_mask_scope_untouched() {
            let (writer, mut reader) = UnixStream::pair().unwrap();
            let message = b"nested Tool output";
            let mut actual_mask = 0u64;
            assert_eq!(
                unsafe {
                    raw_syscall6(
                        libc::SYS_rt_sigprocmask,
                        [0, 0, (&raw mut actual_mask) as u64, 8, 0, 0],
                    )
                },
                0
            );
            for captured in [0, 1 << (libc::SIGTRAP - 1)] {
                let mut captured_mask = captured;
                let _runtime = crate::runtime_domain::Entry::enter();
                let _callback = ToolCallbackGuard::enter();
                unsafe {
                    reverie_preload::user_dispatch::with_ordinary_dispatch_mask(
                        &mut captured_mask,
                        actual_mask,
                        || {
                            let mut request = event(
                                libc::SYS_write,
                                [
                                    writer.as_raw_fd() as u64,
                                    message.as_ptr() as u64,
                                    message.len() as u64,
                                    0,
                                    0,
                                    0,
                                ],
                            );
                            forward_nested_tool_syscall(&mut request);
                            assert_eq!(request.result, message.len() as i64);
                            assert_eq!(
                                reverie_preload::user_dispatch::dispatch_mask(),
                                Some(captured)
                            );
                        },
                    );
                }
                assert_eq!(captured_mask, captured);
                let mut after_mask = 0u64;
                assert_eq!(
                    unsafe {
                        raw_syscall6(
                            libc::SYS_rt_sigprocmask,
                            [0, 0, (&raw mut after_mask) as u64, 8, 0, 0],
                        )
                    },
                    0
                );
                assert_eq!(after_mask, actual_mask);
                let mut received = [0u8; 18];
                reader.read_exact(&mut received).unwrap();
                assert_eq!(&received, message);
            }
        }
    }

    mod native_scopes {
        use super::super::*;

        static HITS: AtomicU64 = AtomicU64::new(0);
        static EXPECTED: AtomicU64 = AtomicU64::new(0);
        static FAILURES: AtomicU64 = AtomicU64::new(0);
        static CONTINUATION: AtomicU64 = AtomicU64::new(0);

        unsafe fn pair() -> (u64, u64) {
            let cpuid = unsafe { raw_syscall6(libc::SYS_arch_prctl, [0x1011, 0, 0, 0, 0, 0]) };
            let mut tsc = 0u64;
            let result =
                unsafe { raw_syscall6(libc::SYS_prctl, [25, (&raw mut tsc) as u64, 0, 0, 0, 0]) };
            if cpuid < 0 || result != 0 {
                unsafe { exit_now(121) };
            }
            (cpuid as u64, tsc)
        }

        unsafe fn set_pair(cpuid: u64, tsc: u64) {
            if unsafe { raw_syscall6(libc::SYS_arch_prctl, [0x1012, cpuid, 0, 0, 0, 0]) } != 0
                || unsafe { raw_syscall6(libc::SYS_prctl, [26, tsc, 0, 0, 0, 0]) } != 0
            {
                unsafe { exit_now(121) };
            }
        }

        unsafe fn mask() -> u64 {
            let mut value = 0;
            if unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [0, 0, (&raw mut value) as u64, 8, 0, 0],
                )
            } != 0
            {
                unsafe { exit_now(121) };
            }
            value
        }

        #[unsafe(naked)]
        unsafe extern "C" fn enter() -> u64 {
            core::arch::naked_asm!("mov edi, 3", "jmp {enter}", enter = sym reverie_liteinst_instruction_scope_enter);
        }

        #[unsafe(naked)]
        unsafe extern "C" fn leave(token: u64) {
            core::arch::naked_asm!(
                "sub rsp, 8", "call {leave}", "add rsp, 8",
                "mov eax, 0xdead", "mov edx, 0xbeef", "ret",
                leave = sym reverie_liteinst_instruction_scope_leave,
            );
        }

        static SCOPE: reverie_preload::clock_boundary::SignalScope =
            reverie_preload::clock_boundary::SignalScope { enter, leave };

        unsafe extern "C" fn clock_enter(_witness: u64) -> u64 {
            0x1234
        }

        unsafe extern "C" fn clock_leave(token: u64, kind: u64, witness: u64) {
            if token != 0x1234 || kind != 0x4567 || witness != 0x89ab {
                FAILURES.fetch_add(1, Ordering::Relaxed);
            }
            CONTINUATION.fetch_add(1, Ordering::Relaxed);
        }

        static CLOCK: reverie_preload::clock_boundary::BoundaryHooks =
            reverie_preload::clock_boundary::BoundaryHooks {
                enter: clock_enter,
                leave: clock_leave,
            };

        unsafe extern "C" fn body(
            signal: i32,
            info: *mut libc::siginfo_t,
            frame: *mut libc::c_void,
            token: u64,
        ) -> reverie_preload::clock_boundary::Continuation {
            if info.is_null() || frame.is_null() || unsafe { (*info).si_code } != libc::SI_TKILL {
                unsafe { exit_now(120) };
            }
            let expected = if signal == libc::SIGUSR2 {
                EXPECTED.load(Ordering::Relaxed)
            } else {
                15
            };
            if token != expected || unsafe { pair() } != (1, 1) {
                FAILURES.fetch_add(1, Ordering::Relaxed);
            }
            let before = unsafe { mask() };
            if before & (1 << (libc::SIGSEGV - 1)) == 0 {
                FAILURES.fetch_add(1, Ordering::Relaxed);
            }
            let _cpuid = core::arch::x86_64::__cpuid(0);
            let _tsc = unsafe { core::arch::x86_64::_rdtsc() };
            let mut aux = 0;
            let _tscp = unsafe { core::arch::x86_64::__rdtscp(&mut aux) };
            HITS.fetch_add(1, Ordering::Relaxed);
            if signal == libc::SIGUSR2 {
                let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
                let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
                if unsafe {
                    raw_syscall6(
                        libc::SYS_tgkill,
                        [pid as u64, tid as u64, libc::SIGTRAP as u64, 0, 0, 0],
                    )
                } != 0
                {
                    unsafe { exit_now(120) };
                }
            }
            if unsafe { pair() } != (1, 1) || unsafe { mask() } != before {
                FAILURES.fetch_add(1, Ordering::Relaxed);
            }
            reverie_preload::clock_boundary::Continuation {
                kind: 0x4567,
                witness: 0x89ab,
            }
        }

        reverie_preload::clocked_signal!(handler, body, scope_token);

        #[test]
        fn actual_controls_and_nested_signal_tokens() {
            let Ok(case) = std::env::var("LITEINST_NATIVE_SCOPE_CASE") else {
                for case in 0..18 {
                    let status = std::process::Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", "runtime::tests::native_scopes::actual_controls_and_nested_signal_tokens", "--nocapture"])
                        .env("LITEINST_NATIVE_SCOPE_CASE", case.to_string())
                        .status().unwrap();
                    assert_eq!(
                        status.code(),
                        Some(if case < 16 { 0 } else { 125 }),
                        "case {case}: {status}"
                    );
                }
                return;
            };
            let case: u64 = case.parse().unwrap();
            if case == 16 {
                unsafe { reverie_liteinst_instruction_scope_enter(4) };
                panic!("invalid selection returned");
            }
            if case == 17 {
                unsafe { reverie_liteinst_instruction_scope_leave(31) };
                panic!("invalid restore returned");
            }
            unsafe {
                reverie_preload::clock_boundary::register(&CLOCK).unwrap();
                reverie_preload::clock_boundary::register_signal_scope(&SCOPE).unwrap();
            }
            let action = KernelSigaction {
                handler: handler as *const () as u64,
                flags: (libc::SA_SIGINFO as u64) | 0x04000000,
                restorer: reverie_preload::trap::trusted_sigreturn_restorer as *const () as u64,
                mask: 1 << (libc::SIGSEGV - 1),
            };
            for signal in [libc::SIGUSR2, libc::SIGTRAP] {
                assert_eq!(
                    unsafe {
                        raw_syscall6(
                            libc::SYS_rt_sigaction,
                            [signal as u64, (&raw const action) as u64, 0, 8, 0, 0],
                        )
                    },
                    0
                );
            }
            let selected = case & 3;
            let original = ((case >> 2) & 1, 1 + ((case >> 3) & 1));
            let original_mask = unsafe { mask() };
            unsafe { set_pair(original.0, original.1) };
            let outer = unsafe { reverie_liteinst_instruction_scope_enter(selected) };
            let expected = (
                if selected & 1 != 0 { 1 } else { original.0 },
                if selected & 2 != 0 { 1 } else { original.1 },
            );
            let actual = unsafe { pair() };
            EXPECTED.store(3 | (actual.0 << 2) | (actual.1 << 3), Ordering::Relaxed);
            let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
            let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
            let sent = unsafe {
                raw_syscall6(
                    libc::SYS_tgkill,
                    [pid as u64, tid as u64, libc::SIGUSR2 as u64, 0, 0, 0],
                )
            };
            let restored_parent = unsafe { pair() };
            unsafe { reverie_liteinst_instruction_scope_leave(outer) };
            let restored = unsafe { pair() };
            let restored_mask = unsafe { mask() };
            unsafe { set_pair(1, 1) };
            assert_eq!(sent, 0);
            assert_eq!(actual, expected);
            assert_eq!(restored_parent, expected);
            assert_eq!(restored, original);
            assert_eq!(restored_mask, original_mask);
            assert_eq!(FAILURES.load(Ordering::Relaxed), 0);
            assert_eq!(HITS.load(Ordering::Relaxed), 2);
            assert_eq!(CONTINUATION.load(Ordering::Relaxed), 2);
        }
    }

    #[test]
    fn protected_openat_reader_denial() {
        use std::ffi::CString;
        use std::os::fd::AsRawFd;

        let Some(denial) = std::env::var_os("LITEINST_READER_DENIAL_TEST") else {
            for denial in [libc::EPERM, libc::ENOSYS] {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "runtime::tests::protected_openat_reader_denial",
                        "--nocapture",
                    ])
                    .env("LITEINST_READER_DENIAL_TEST", denial.to_string())
                    .status()
                    .unwrap();
                assert!(status.success());
            }
            return;
        };
        let denial: i32 = denial.to_str().unwrap().parse().unwrap();
        assert!([libc::EPERM, libc::ENOSYS].contains(&denial));
        let directory = tempfile::tempdir().unwrap();
        let directory_file = std::fs::File::open(directory.path()).unwrap();
        let protected = directory_file.as_raw_fd();
        super::reserve_coordinator_fd(protected).unwrap();
        let path = directory.path().join("created-once");
        let pathname = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let mut filter = [
            libc::sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 0,
            },
            libc::sock_filter {
                code: 0x15,
                jt: 0,
                jf: 1,
                k: libc::SYS_process_vm_readv as u32,
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ERRNO | denial as u32,
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ALLOW,
            },
        ];
        let program = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_mut_ptr(),
        };
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
            0
        );
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_SECCOMP, 2, &raw const program) },
            0
        );
        let mut byte = 0_u8;
        let local = libc::iovec {
            iov_base: (&raw mut byte).cast(),
            iov_len: 1,
        };
        let remote = libc::iovec {
            iov_base: pathname.as_ptr().cast_mut().cast(),
            iov_len: 1,
        };
        let probe = unsafe {
            super::raw_syscall6(
                libc::SYS_process_vm_readv,
                [
                    libc::getpid() as u64,
                    (&raw const local) as u64,
                    1,
                    (&raw const remote) as u64,
                    1,
                    0,
                ],
            )
        };
        println!("reader denial errno={denial}, actual probe={probe}");
        assert_eq!(probe, -i64::from(denial));
        let args = [
            protected as u64,
            pathname.as_ptr() as u64,
            (libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL) as u64,
            0o600,
            0,
            0,
        ];
        assert_eq!(
            super::protected_injected_syscall(libc::SYS_openat, args),
            None
        );
        assert!(
            !path.exists(),
            "classification must not create a file before Tool execution"
        );
        let opened = super::guarded_raw_syscall(libc::SYS_openat, args);
        assert!(
            opened >= 0,
            "reader denial must not become a guest errno: {opened}"
        );
        assert_eq!(unsafe { libc::close(opened as i32) }, 0);
        assert_eq!(
            super::guarded_raw_syscall(libc::SYS_openat, args),
            -i64::from(libc::EEXIST)
        );
        for (pathname, error) in [
            (c"relative".as_ptr() as u64, libc::EBADF),
            (1, libc::EFAULT),
            (c"".as_ptr() as u64, libc::ENOENT),
        ] {
            let args = [protected as u64, pathname, libc::O_RDONLY as u64, 0, 0, 0];
            let mut kernel_args = args;
            kernel_args[0] = (-1_i32) as u64;
            assert_eq!(
                unsafe { super::raw_syscall6(libc::SYS_openat, kernel_args) },
                -i64::from(error)
            );
            assert_eq!(
                super::guarded_raw_syscall(libc::SYS_openat, args),
                -i64::from(error)
            );
        }
    }

    #[test]
    fn protected_openat_compatibility_forwarding() {
        use std::ffi::CString;
        use std::io::Read;
        use std::os::fd::AsRawFd;
        use std::os::fd::FromRawFd;

        if std::env::var_os("LITEINST_COMPAT_OPENAT_TEST").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime::tests::protected_openat_compatibility_forwarding",
                    "--nocapture",
                ])
                .env("LITEINST_COMPAT_OPENAT_TEST", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("created-once");
        let pathname = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let mut pipes = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipes.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
            0
        );
        let mut reader = unsafe { std::fs::File::from_raw_fd(pipes[0]) };
        let writer = unsafe { std::fs::File::from_raw_fd(pipes[1]) };
        let mut metadata: libc::stat = unsafe { core::mem::zeroed() };
        assert_eq!(
            unsafe { libc::fstat(writer.as_raw_fd(), &raw mut metadata) },
            0
        );
        super::EVENT_FD.store(writer.as_raw_fd(), Ordering::Release);
        super::EVENT_DEVICE.store(metadata.st_dev, Ordering::Release);
        super::EVENT_INODE.store(metadata.st_ino, Ordering::Release);
        super::EVENT_COOKIE.store(42, Ordering::Release);
        super::TOOL_MODE.store(super::TOOL_COMPAT, Ordering::Release);
        let args = [
            writer.as_raw_fd() as u64,
            pathname.as_ptr() as u64,
            (libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL) as u64,
            0o600,
            0,
            0,
        ];
        let mut event = super::SyscallEvent {
            owned_binding: None,
            exit: None,
            number: libc::SYS_openat,
            args,
            instruction_pointer: 0,
            result: 123,
            context: 0,
        };
        assert!(!unsafe { super::protect_compatibility_event_channel(&mut event) });
        assert_eq!(event.args, args);
        assert_eq!(event.result, 123);
        assert!(
            !path.exists(),
            "compatibility guard must not open before forwarding"
        );
        unsafe { super::process_syscall(&mut event) };
        assert!(event.result >= 0);
        assert_eq!(
            event.args, args,
            "original event operands must survive forwarding"
        );
        assert_eq!(unsafe { libc::close(event.result as i32) }, 0);
        unsafe { super::process_syscall(&mut event) };
        assert_eq!(event.result, -i64::from(libc::EEXIST));
        assert_eq!(event.args, args);
        for (pathname, error) in [
            (c"relative".as_ptr() as u64, libc::EBADF),
            (1, libc::EFAULT),
            (c"".as_ptr() as u64, libc::ENOENT),
        ] {
            let args = [
                writer.as_raw_fd() as u64,
                pathname,
                libc::O_RDONLY as u64,
                0,
                0,
                0,
            ];
            event.args = args;
            unsafe { super::process_syscall(&mut event) };
            assert_eq!(event.result, -i64::from(error));
            assert_eq!(event.args, args);
        }
        assert!(unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFD) } >= 0);
        drop(writer);
        let mut records = String::new();
        reader.read_to_string(&mut records).unwrap();
        assert_eq!(records.lines().count(), 5);
        assert!(records.lines().all(|line| line.contains("tool=compat cookie=42") && line.ends_with("syscall=257")));
        println!("actual compatibility records:\n{records}");
    }

    #[test]
    fn protected_openat_write_only_pathname() {
        use std::ffi::CString;
        use std::os::fd::AsRawFd;

        if std::env::var_os("LITEINST_WRITE_ONLY_PATH_TEST").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime::tests::protected_openat_write_only_pathname",
                    "--nocapture",
                ])
                .env("LITEINST_WRITE_ONLY_PATH_TEST", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("entry");
        std::fs::write(&path, b"resident write-only pathname").unwrap();
        let directory_file = std::fs::File::open(directory.path()).unwrap();
        let protected = directory_file.as_raw_fd();
        super::reserve_coordinator_fd(protected).unwrap();
        let pathname = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        assert!(pathname.as_bytes_with_nul().len() < page_size);
        let mapping = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                page_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        unsafe {
            core::ptr::copy_nonoverlapping(
                pathname.as_ptr(),
                mapping.cast(),
                pathname.as_bytes_with_nul().len(),
            )
        };
        assert_eq!(
            unsafe { libc::mprotect(mapping, page_size, libc::PROT_WRITE) },
            0
        );
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let address = mapping as usize;
        let vma = maps
            .lines()
            .find(|line| {
                let range = line.split_whitespace().next().unwrap();
                let (start, end) = range.split_once('-').unwrap();
                (usize::from_str_radix(start, 16).unwrap()..usize::from_str_radix(end, 16).unwrap())
                    .contains(&address)
            })
            .unwrap();
        assert_eq!(vma.split_whitespace().nth(1), Some("-w-p"));
        println!(
            "pathname={address:#x} bytes={} VMA={vma}",
            pathname.as_bytes_with_nul().len()
        );
        let args = [
            protected as u64,
            mapping as u64,
            libc::O_RDONLY as u64,
            0,
            0,
            0,
        ];
        let linux = unsafe { super::raw_syscall6(libc::SYS_openat, args) };
        println!("linux openat={linux}");
        assert!(linux >= 0);
        assert_eq!(unsafe { libc::close(linux as i32) }, 0);
        let guarded = super::guarded_raw_syscall(libc::SYS_openat, args);
        println!("guarded openat={guarded}");
        assert!(
            guarded >= 0,
            "resident write-only pathname is Linux-readable"
        );
        assert_eq!(unsafe { libc::close(guarded as i32) }, 0);
        assert_eq!(unsafe { libc::munmap(mapping, page_size) }, 0);
    }

    #[test]
    fn directory_forwarding_preserves_arguments_results_and_both_fd_operands() {
        use std::cell::Cell;
        if std::env::var_os("LITEINST_DIRECTORY_FORWARDING_TEST").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "runtime::tests::directory_forwarding_preserves_arguments_results_and_both_fd_operands"])
                .env("LITEINST_DIRECTORY_FORWARDING_TEST", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        thread_local! {
            static EXPECTED: Cell<(i64, [u64; 6], i64)> = const { Cell::new((0, [0; 6], 0)) };
        }
        unsafe fn forward(number: i64, args: [u64; 6]) -> i64 {
            let (expected_number, expected_args, result) = EXPECTED.get();
            assert_eq!((number, args), (expected_number, expected_args));
            result
        }
        struct Restore(i32);
        impl Drop for Restore {
            fn drop(&mut self) {
                super::COORDINATOR_FD.store(self.0, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore =
            Restore(super::COORDINATOR_FD.swap(771, std::sync::atomic::Ordering::AcqRel));
        for number in [
            libc::SYS_mkdir,
            libc::SYS_rename,
            libc::SYS_rmdir,
            libc::SYS_renameat,
            libc::SYS_openat,
            libc::SYS_newfstatat,
            libc::SYS_linkat,
            libc::SYS_unlinkat,
        ] {
            for source in [771, (3_u64 << 32) | 771, 772, libc::AT_FDCWD as u64] {
                for destination in [771, (5_u64 << 32) | 771, 773, libc::AT_FDCWD as u64] {
                    let args = [
                        source,
                        0x123456789abcdef0,
                        destination,
                        0xfedcba9876543210,
                        0xface,
                        0xbeef,
                    ];
                    let mut expected = args;
                    if matches!(
                        number,
                        libc::SYS_renameat
                            | libc::SYS_linkat
                            | libc::SYS_openat
                            | libc::SYS_newfstatat
                            | libc::SYS_unlinkat
                    ) && source as i32 == 771
                    {
                        expected[0] = (-1_i32) as u64;
                    }
                    if matches!(number, libc::SYS_renameat | libc::SYS_linkat)
                        && destination as i32 == 771
                    {
                        expected[2] = (-1_i32) as u64;
                    }
                    for result in [0, -i64::from(libc::EFAULT), -i64::from(libc::EACCES)] {
                        EXPECTED.set((number, expected, result));
                        assert_eq!(super::forward_kernel_syscall(number, args, forward), result);
                    }
                }
            }
        }
    }

    #[test]
    fn fixture_routes_protected_descriptors_and_linkat_kernel_ordering() {
        use std::ffi::CString;
        use std::os::fd::AsRawFd;
        if std::env::var_os("LITEINST_FIXTURE_FDS_TEST").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "runtime::tests::fixture_routes_protected_descriptors_and_linkat_kernel_ordering"])
                .env("LITEINST_FIXTURE_FDS_TEST", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        struct Restore(i32);
        impl Drop for Restore {
            fn drop(&mut self) {
                super::COORDINATOR_FD.store(self.0, std::sync::atomic::Ordering::Release);
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        std::fs::write(&source, b"protected dirfd fixture").unwrap();
        let handle = std::fs::File::open(directory.path()).unwrap();
        let protected = handle.as_raw_fd();
        let _restore =
            Restore(super::COORDINATOR_FD.swap(protected, std::sync::atomic::Ordering::AcqRel));
        let absolute = CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
        let target = CString::new(destination.as_os_str().as_encoded_bytes()).unwrap();
        for descriptor in [protected as u64, (7_u64 << 32) | protected as u64] {
            for number in [
                libc::SYS_inotify_add_watch,
                libc::SYS_getdents64,
                libc::SYS_bind,
                libc::SYS_getsockname,
                libc::SYS_sendto,
                libc::SYS_recvfrom,
                libc::SYS_dup,
                libc::SYS_fcntl,
            ] {
                let args = [descriptor, 1, 2, 3, 4, 5];
                assert_eq!(
                    super::protected_injected_syscall(number, args),
                    Some(-i64::from(libc::EBADF))
                );
                assert_eq!(
                    super::guarded_raw_syscall(number, args),
                    -i64::from(libc::EBADF)
                );
            }
            for (source_fd, path, target_fd, new_path, flags) in [
                (
                    descriptor,
                    c"source".as_ptr() as u64,
                    libc::AT_FDCWD as u64,
                    target.as_ptr() as u64,
                    0,
                ),
                (
                    libc::AT_FDCWD as u64,
                    absolute.as_ptr() as u64,
                    descriptor,
                    c"destination".as_ptr() as u64,
                    0,
                ),
                (descriptor, 1, descriptor, 1, 0),
                (descriptor, 0, descriptor, 0, u64::MAX),
            ] {
                let args = [source_fd, path, target_fd, new_path, flags, 0];
                let mut control = args;
                for index in [0, 2] {
                    if control[index] as i32 == protected {
                        control[index] = (-1_i32) as u64;
                    }
                }
                let expected = unsafe { super::raw_syscall6(libc::SYS_linkat, control) };
                assert!(expected < 0);
                assert_eq!(
                    super::protected_injected_syscall(libc::SYS_linkat, args),
                    None
                );
                assert_eq!(super::guarded_raw_syscall(libc::SYS_linkat, args), expected);
            }
            let args = [
                descriptor,
                absolute.as_ptr() as u64,
                descriptor,
                target.as_ptr() as u64,
                0,
                0,
            ];
            assert_eq!(super::guarded_raw_syscall(libc::SYS_linkat, args), 0);
            assert_eq!(
                std::fs::read(&destination).unwrap(),
                b"protected dirfd fixture"
            );
            let relative = [descriptor, c"destination".as_ptr() as u64, 0, 0, 0, 0];
            let control = [(-1_i32) as u64, relative[1], 0, 0, 0, 0];
            assert_eq!(
                super::guarded_raw_syscall(libc::SYS_unlinkat, relative),
                unsafe { super::raw_syscall6(libc::SYS_unlinkat, control) }
            );
            assert!(destination.exists());
            assert_eq!(
                super::guarded_raw_syscall(
                    libc::SYS_unlinkat,
                    [descriptor, target.as_ptr() as u64, 0, 0, 0, 0]
                ),
                0
            );
        }
        assert_eq!(std::fs::read(source).unwrap(), b"protected dirfd fixture");
    }

    #[test]
    fn directory_renameat_protected_fds_preserve_native_path_error_ordering() {
        use std::ffi::CString;
        use std::os::fd::AsRawFd;
        if std::env::var_os("LITEINST_RENAMEAT_PROTECTED_FDS_TEST").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "runtime::tests::directory_renameat_protected_fds_preserve_native_path_error_ordering"])
                .env("LITEINST_RENAMEAT_PROTECTED_FDS_TEST", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        struct Restore(i32);
        impl Drop for Restore {
            fn drop(&mut self) {
                super::COORDINATOR_FD.store(self.0, std::sync::atomic::Ordering::Release);
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let entry = directory.path().join("entry");
        std::fs::write(&entry, b"preserved renameat entry").unwrap();
        let protected_file = std::fs::File::open(directory.path()).unwrap();
        let ordinary_file = std::fs::File::open(directory.path()).unwrap();
        let protected = protected_file.as_raw_fd();
        let ordinary = ordinary_file.as_raw_fd() as u64;
        let _restore =
            Restore(super::COORDINATOR_FD.swap(protected, std::sync::atomic::Ordering::AcqRel));
        let absolute = CString::new(entry.as_os_str().as_encoded_bytes()).unwrap();
        let missing = CString::new(
            directory
                .path()
                .join("missing")
                .as_os_str()
                .as_encoded_bytes(),
        )
        .unwrap();
        for fd in [protected as u64, (0xace_u64 << 32) | protected as u64] {
            for (source_fd, source, destination_fd, destination) in [
                (fd, absolute.as_ptr() as u64, fd, absolute.as_ptr() as u64),
                (
                    fd,
                    c"entry".as_ptr() as u64,
                    ordinary,
                    c"entry".as_ptr() as u64,
                ),
                (
                    ordinary,
                    c"entry".as_ptr() as u64,
                    fd,
                    c"entry".as_ptr() as u64,
                ),
                (fd, c"entry".as_ptr() as u64, fd, c"entry".as_ptr() as u64),
                (
                    fd,
                    absolute.as_ptr() as u64,
                    ordinary,
                    c"entry".as_ptr() as u64,
                ),
                (
                    ordinary,
                    c"entry".as_ptr() as u64,
                    fd,
                    absolute.as_ptr() as u64,
                ),
                (fd, 0, fd, absolute.as_ptr() as u64),
                (fd, absolute.as_ptr() as u64, fd, 0),
                (fd, c"".as_ptr() as u64, fd, 0),
                (fd, missing.as_ptr() as u64, fd, 0),
                (
                    (-1_i32) as u64,
                    absolute.as_ptr() as u64,
                    (-1_i32) as u64,
                    absolute.as_ptr() as u64,
                ),
            ] {
                let args = [
                    source_fd,
                    source,
                    destination_fd,
                    destination,
                    0xdead,
                    0xbeef,
                ];
                let mut control = args;
                for index in [0, 2] {
                    if control[index] as i32 == protected {
                        control[index] = (-1_i32) as u64;
                    }
                }
                let expected = unsafe { super::raw_syscall6(libc::SYS_renameat, control) };
                assert_eq!(
                    super::protected_injected_syscall(libc::SYS_renameat, args),
                    None
                );
                assert_eq!(
                    super::guarded_raw_syscall(libc::SYS_renameat, args),
                    expected,
                    "{args:x?}"
                );
                assert_eq!(std::fs::read(&entry).unwrap(), b"preserved renameat entry");
            }
        }
    }

    #[test]
    fn protected_openat_path_semantics() {
        use std::ffi::CString;
        use std::os::fd::AsRawFd;

        if std::env::var_os("LITEINST_OPENAT_GUARD_TEST").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "runtime::tests::protected_openat_path_semantics"])
                .env("LITEINST_OPENAT_GUARD_TEST", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("entry");
        std::fs::write(&path, b"absolute openat retains Linux semantics").unwrap();
        let directory_file = std::fs::File::open(directory.path()).unwrap();
        let protected = directory_file.as_raw_fd();
        super::reserve_coordinator_fd(protected).unwrap();
        let absolute = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let relative = c"entry";
        let args = |address| [protected as u64, address, libc::O_RDONLY as u64, 0, 0, 0];
        let absolute_args = args(absolute.as_ptr() as u64);
        assert_eq!(
            super::protected_injected_syscall(libc::SYS_openat, absolute_args),
            None,
            "absolute openat must ignore even a protected dirfd"
        );
        let opened = super::guarded_raw_syscall(libc::SYS_openat, absolute_args);
        assert!(opened >= 0, "absolute openat failed: {opened}");
        assert_eq!(unsafe { libc::close(opened as i32) }, 0);
        let relative_args = args(relative.as_ptr() as u64);
        let linux_opened = unsafe { super::raw_syscall6(libc::SYS_openat, relative_args) };
        assert!(linux_opened >= 0);
        assert_eq!(unsafe { libc::close(linux_opened as i32) }, 0);
        assert_eq!(
            super::protected_injected_syscall(libc::SYS_openat, relative_args),
            None,
            "openat is decided only at authorized kernel forwarding"
        );
        assert_eq!(
            super::guarded_raw_syscall(libc::SYS_openat, relative_args),
            -i64::from(libc::EBADF)
        );
        for address in [0, 1, u64::MAX] {
            assert_eq!(
                super::guarded_raw_syscall(libc::SYS_openat, args(address)),
                -i64::from(libc::EFAULT)
            );
        }
        assert_eq!(
            super::guarded_raw_syscall(libc::SYS_openat, args(c"".as_ptr() as u64)),
            -i64::from(libc::ENOENT)
        );
        let unterminated = [b'x'; libc::PATH_MAX as usize];
        for result in [
            super::guarded_raw_syscall(libc::SYS_openat, args(unterminated.as_ptr() as u64)),
            unsafe { super::raw_syscall6(libc::SYS_openat, args(unterminated.as_ptr() as u64)) },
        ] {
            assert_eq!(result, -i64::from(libc::ENAMETOOLONG));
        }
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let mapping = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                2 * page_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        let inaccessible = unsafe { mapping.cast::<u8>().add(page_size) };
        assert_eq!(
            unsafe { libc::mprotect(inaccessible.cast(), page_size, libc::PROT_NONE) },
            0
        );
        assert_eq!(
            super::guarded_raw_syscall(libc::SYS_openat, args(inaccessible as u64)),
            -i64::from(libc::EFAULT)
        );
        let boundary = unsafe { inaccessible.sub(1) };
        for prefix in *b"./" {
            unsafe { boundary.write(prefix) };
            let boundary_args = args(boundary as u64);
            assert_eq!(
                unsafe { super::raw_syscall6(libc::SYS_openat, boundary_args) },
                -i64::from(libc::EFAULT)
            );
            assert_eq!(
                super::guarded_raw_syscall(libc::SYS_openat, boundary_args),
                -i64::from(libc::EFAULT)
            );
        }
        unsafe { boundary.write(0) };
        assert_eq!(
            super::guarded_raw_syscall(libc::SYS_openat, args(boundary as u64)),
            -i64::from(libc::ENOENT)
        );
        assert_eq!(unsafe { libc::munmap(mapping, 2 * page_size) }, 0);
        assert!(unsafe { libc::fcntl(protected, libc::F_GETFD) } >= 0);
    }

    #[test]
    fn protected_guest_log_descriptors() {
        use std::os::fd::AsRawFd;
        if std::env::var_os("LITEINST_LOG_GUARD_TEST").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "runtime::tests::protected_guest_log_descriptors"])
                .env("LITEINST_LOG_GUARD_TEST", "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        for fd in [200, 201, 202] {
            assert_eq!(
                unsafe { libc::dup3(socket.as_raw_fd(), fd, libc::O_CLOEXEC) },
                fd
            );
        }
        super::reserve_coordinator_fd(200).unwrap();
        crate::guest_log::LOG_FD.store(202, Ordering::Release);
        {
            let _channel = crate::rpc::ChannelIoGuard::enter(200);
            assert!(crate::rpc::allows_channel_io(libc::SYS_sendto, 200));
            assert!(!crate::rpc::allows_channel_io(libc::SYS_sendto, 202));
            assert_eq!(
                super::protected_injected_syscall(libc::SYS_sendto, [200, 0, 0, 0, 0, 0]),
                Some(-i64::from(libc::EBADF))
            );
            assert!(!crate::rpc::allows_channel_io(libc::SYS_close, 200));
        }
        assert!(!crate::rpc::allows_channel_io(libc::SYS_sendto, 200));
        for (number, args) in [
            (libc::SYS_write, [202, 0, 0, 0, 0, 0]),
            (libc::SYS_read, [202, 0, 0, 0, 0, 0]),
            (libc::SYS_lseek, [202, 0, 0, 0, 0, 0]),
            (libc::SYS_fcntl, [202, libc::F_DUPFD as u64, 0, 0, 0, 0]),
            (libc::SYS_dup2, [1, 202, 0, 0, 0, 0]),
            (libc::SYS_dup3, [202, 205, 0, 0, 0, 0]),
            (libc::SYS_shutdown, [202, 0, 0, 0, 0, 0]),
            (libc::SYS_sendfile, [1, 202, 0, 0, 0, 0]),
            (libc::SYS_fstat, [202, 0, 0, 0, 0, 0]),
            (libc::SYS_fstat, [200, 0, 0, 0, 0, 0]),
        ] {
            let mut event = super::SyscallEvent {
                owned_binding: None,
                exit: None,
                number,
                args,
                instruction_pointer: 0,
                result: 1,
                context: 0,
            };
            assert!(unsafe { super::protect_coordinator_channel(&mut event) });
            assert_eq!(event.result, -i64::from(libc::EBADF));
        }
        for fd in [200, 202] {
            let args = [fd, c"entry".as_ptr() as u64, 0, 0, 0, 0];
            assert_eq!(
                super::protected_injected_syscall(libc::SYS_openat, args),
                None
            );
            assert_eq!(
                super::guarded_raw_syscall(libc::SYS_openat, args),
                -i64::from(libc::EBADF)
            );
        }
        for (number, args) in [
            (libc::SYS_close, [202, 0, 0, 0, 0, 0]),
            (libc::SYS_close_range, [200, 202, 4, 0, 0, 0]),
            (libc::SYS_close_range, [200, 202, 0, 0, 0, 0]),
        ] {
            let mut event = super::SyscallEvent {
                owned_binding: None,
                exit: None,
                number,
                args,
                instruction_pointer: 0,
                result: 1,
                context: 0,
            };
            assert!(unsafe { super::protect_coordinator_channel(&mut event) });
            assert_eq!(event.result, 0);
        }
        assert!(unsafe { libc::fcntl(200, libc::F_GETFD) } >= 0);
        assert!(unsafe { libc::fcntl(202, libc::F_GETFD) } >= 0);
        assert_eq!(unsafe { libc::fcntl(201, libc::F_GETFD) }, -1);

        for (number, args) in [
            (libc::SYS_fstat, [203, 0, 0, 0, 0, 0]),
            (libc::SYS_openat, [203, 0, 0, 0, 0, 0]),
            (libc::SYS_openat, [libc::AT_FDCWD as u64, 0, 0, 0, 0, 0]),
            (libc::SYS_close, [203, 0, 0, 0, 0, 0]),
        ] {
            let mut event = super::SyscallEvent {
                owned_binding: None,
                exit: None,
                number,
                args,
                instruction_pointer: 0,
                result: 1,
                context: 0,
            };
            assert!(
                !unsafe { super::protect_coordinator_channel(&mut event) },
                "{number} on an unprotected descriptor must not be intercepted"
            );
            assert_eq!(event.result, 1, "{number} must be left untouched");
        }
    }

    use core::sync::atomic::Ordering;
    use std::ffi::OsStr;

    use super::ALT_STACK_ENV;
    use super::ENABLED_FALLBACK_CLASSIFICATIONS;
    use super::FORK_HOOK;
    use super::FallbackCounters;
    use super::LiteinstDispatcher;
    use super::MAX_PATCH_SITES;
    use super::RCB_CLOCK;
    use super::RCB_CLOCK_OWNER;
    use super::RCB_CLOCK_UNAVAILABLE;
    use super::SITE_ACTIVE;
    use super::SITE_FALLBACK;
    use super::SITE_INSTALLING;
    use super::SITE_STALE;
    use super::SITES;
    use super::SiteSlot;
    use super::StackLine;
    use super::alt_stack_from_env_value;
    use super::claim_site;
    use super::clone_is_fork_like;
    use super::fallback_dispatch_count;
    use super::fallback_syscall_count;
    use super::initialize_rcb_clock_with;
    use super::mark_site_range_stale;
    use super::raw_syscall6;
    use super::record_fallback_dispatch;
    use super::reset_site_observability;

    #[test]
    fn every_optional_rcb_setup_error_takes_the_real_unavailable_path() {
        let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as libc::pid_t;
        assert!(owner > 0);
        for error in [
            reverie::Errno::EACCES,
            reverie::Errno::EPERM,
            reverie::Errno::ENODEV,
            reverie::Errno::EOPNOTSUPP,
            reverie::Errno::EINVAL,
            reverie::Errno::EMFILE,
            reverie::Errno::ENFILE,
            reverie::Errno::EBUSY,
            reverie::Errno::EIO,
        ] {
            initialize_rcb_clock_with(|| Err(error)).unwrap();
            assert!(RCB_CLOCK.get().is_null());
            assert!(RCB_CLOCK_UNAVAILABLE.get());
            assert_eq!(RCB_CLOCK_OWNER.get(), owner);
        }
    }

    #[test]
    fn disabled_dispatch_does_not_classify_fallback_sites() {
        let before = ENABLED_FALLBACK_CLASSIFICATIONS.load(Ordering::Relaxed);
        let dispatcher = LiteinstDispatcher::new(
            crate::stats::GuestStatsHooks::DISABLED,
            super::PatchPublication::Concurrent,
        );

        (dispatcher.record_fallback_stats)(dispatcher.stats, 0xdead_beef);

        assert_eq!(
            ENABLED_FALLBACK_CLASSIFICATIONS.load(Ordering::Relaxed),
            before
        );
    }

    #[test]
    fn alt_stack_defaults_to_the_shared_default_when_unset() {
        // Unset must reproduce the shared reverie-preload default verbatim, so
        // the launcher-selected knob is a no-op by default (zero behavior change).
        use reverie_preload::lifecycle::RuntimeConfig;
        assert_eq!(
            alt_stack_from_env_value(None).unwrap(),
            RuntimeConfig::default().use_alt_stack
        );
    }

    #[test]
    fn alt_stack_parses_truthy_and_falsy_spellings() {
        for on in ["1", "true", "TRUE", "on", "On", "yes", "  yes  "] {
            assert!(
                alt_stack_from_env_value(Some(OsStr::new(on))).unwrap(),
                "{on:?} should parse as alt-stack on"
            );
        }
        for off in ["0", "false", "FALSE", "off", "Off", "no", "  no  "] {
            assert!(
                !alt_stack_from_env_value(Some(OsStr::new(off))).unwrap(),
                "{off:?} should parse as alt-stack off"
            );
        }
    }

    #[test]
    fn alt_stack_rejects_unknown_values() {
        for bad in ["maybe", "2", "", "onoff"] {
            assert!(
                alt_stack_from_env_value(Some(OsStr::new(bad))).is_err(),
                "{bad:?} must be rejected, not silently defaulted"
            );
        }
    }

    #[test]
    fn alt_stack_env_is_distinct_from_the_other_selectors() {
        // The alt-stack knob is orthogonal to the tool selector; a shared
        // build-time typo that aliased them would defeat launcher control.
        assert_eq!(ALT_STACK_ENV, "REVERIE_LITEINST_ALT_STACK");
        assert_ne!(ALT_STACK_ENV, "REVERIE_LITEINST_TOOL");
    }

    #[test]
    fn recording_a_fallback_bumps_total_and_the_matching_syscall() {
        let counters = FallbackCounters::new();
        let number: i64 = 402;

        counters.record(number);

        assert_eq!(counters.by_number(number), 1);
        assert_eq!(counters.total(), 1);
    }

    #[test]
    fn out_of_range_syscall_numbers_count_in_the_total_only() {
        let counters = FallbackCounters::new();
        // Above the tracked bound: total advances, per-number stays zero.
        let huge = i64::from(i32::MAX);
        counters.record(huge);
        assert_eq!(counters.by_number(huge), 0);
        assert_eq!(counters.total(), 1);

        // Negative numbers are never used to index the per-number table.
        counters.record(-1);
        assert_eq!(counters.by_number(-1), 0);
        assert_eq!(counters.total(), 2);
    }

    #[test]
    fn fallback_counter_reset_zeroes_total_and_per_number_counts() {
        let counters = FallbackCounters::new();
        let number: i64 = 404;
        counters.record(number);
        assert_eq!(counters.total(), 1);
        assert_eq!(counters.by_number(number), 1);

        counters.reset();

        assert_eq!(counters.total(), 0);
        assert_eq!(counters.by_number(number), 0);
    }

    #[test]
    fn fork_child_reset_zeroes_per_site_counts_but_preserves_patch_state() {
        // The per-site trap/hook counts are observability; the site's address and
        // state are functional patch metadata the COW-inherited child must keep.
        // Reset must clear the former without disturbing the latter.
        let site = SiteSlot::new();
        let address = 0x4321_9000;
        site.address.store(address, Ordering::Release);
        site.state.store(SITE_ACTIVE, Ordering::Release);
        site.trap_count.store(7, Ordering::Release);
        site.hook_count.store(11, Ordering::Release);
        assert_eq!(site.trap_count.load(Ordering::Acquire), 7);
        assert_eq!(site.hook_count.load(Ordering::Acquire), 11);

        reset_site_observability(std::slice::from_ref(&site));

        // Observability cleared...
        assert_eq!(site.trap_count.load(Ordering::Acquire), 0);
        assert_eq!(site.hook_count.load(Ordering::Acquire), 0);
        // ...but the functional patch state is intact, so the child's inherited
        // instrumentation keeps working.
        assert_eq!(site.address.load(Ordering::Acquire), address);
        assert_eq!(site.state.load(Ordering::Acquire), SITE_ACTIVE);
    }

    #[test]
    fn fork_hook_runs_the_observability_reset() {
        // The static FORK_HOOK must wrap `reset_fallback_observability`, so
        // invoking it (as `process_syscall` does in the fork child) clears both
        // process-global and per-site counters — proving the shared ForkHook seam
        // is wired to the complete production reset rather than a private path.
        SITES.get_or_init(|| {
            (0..MAX_PATCH_SITES)
                .map(|_| SiteSlot::new())
                .collect::<Vec<_>>()
                .into_boxed_slice()
        });
        let address = 0x7654_3000;
        let (site, claimed) = claim_site(address).unwrap();
        assert!(claimed);
        site.state.store(SITE_ACTIVE, Ordering::Release);
        site.trap_count.store(7, Ordering::Release);
        site.hook_count.store(11, Ordering::Release);
        record_fallback_dispatch(405);
        assert!(fallback_dispatch_count() > 0);

        FORK_HOOK.run_in_child();

        assert_eq!(fallback_dispatch_count(), 0);
        assert_eq!(site.trap_count.load(Ordering::Acquire), 0);
        assert_eq!(site.hook_count.load(Ordering::Acquire), 0);
        assert_eq!(site.address.load(Ordering::Acquire), address);
        assert_eq!(site.state.load(Ordering::Acquire), SITE_ACTIVE);
        assert_eq!(fallback_syscall_count(405), 0);
    }

    #[test]
    fn stack_line_formats_signed_and_hex_values() {
        let mut line = StackLine::new();
        line.push_signed(-123);
        line.push_bytes(b" ");
        line.push_hex(0xdead_beef);
        assert_eq!(&line.bytes[..line.len], b"-123 deadbeef");
    }

    #[test]
    fn reused_address_claims_a_new_site_generation() {
        SITES.get_or_init(|| {
            (0..MAX_PATCH_SITES)
                .map(|_| SiteSlot::new())
                .collect::<Vec<_>>()
                .into_boxed_slice()
        });
        let address = 0x1234_5000;
        let (site, claimed) = claim_site(address).unwrap();
        assert!(claimed);
        assert_eq!(site.state.load(Ordering::Acquire), SITE_INSTALLING);
        site.state.store(SITE_ACTIVE, Ordering::Release);

        mark_site_range_stale(address - 0x100, 0x200, address + 0x100);
        assert_eq!(site.state.load(Ordering::Acquire), SITE_STALE);
        assert_eq!(site.mapping_end.load(Ordering::Acquire), address + 0x100);

        let (same_site, claimed) = claim_site(address).unwrap();
        assert!(core::ptr::eq(site, same_site));
        assert!(claimed);
        assert_eq!(site.state.load(Ordering::Acquire), SITE_INSTALLING);
        site.state.store(SITE_FALLBACK, Ordering::Release);
    }

    #[test]
    fn clone_accepts_only_fork_like_flags() {
        let bookkeeping = (libc::CLONE_CHILD_CLEARTID
            | libc::CLONE_CHILD_SETTID
            | libc::CLONE_PARENT_SETTID) as u64;
        assert!(clone_is_fork_like(libc::SIGCHLD as u64, 0));
        assert!(clone_is_fork_like(libc::SIGCHLD as u64 | bookkeeping, 0));
        assert!(!clone_is_fork_like(libc::SIGCHLD as u64, 1));
        assert!(!clone_is_fork_like(0, 0));
        assert!(!clone_is_fork_like(libc::SIGUSR1 as u64, 0));

        for rejected in [
            libc::CLONE_VM,
            libc::CLONE_VFORK,
            libc::CLONE_THREAD,
            libc::CLONE_SETTLS,
            libc::CLONE_SIGHAND,
            libc::CLONE_FILES,
            libc::CLONE_FS,
            libc::CLONE_PARENT,
            libc::CLONE_NEWCGROUP,
            libc::CLONE_NEWIPC,
            libc::CLONE_NEWNET,
            libc::CLONE_NEWNS,
            libc::CLONE_NEWPID,
            libc::CLONE_NEWUSER,
            libc::CLONE_NEWUTS,
        ] {
            assert!(
                !clone_is_fork_like(libc::SIGCHLD as u64 | rejected as u64, 0),
                "accepted unsafe clone flag {rejected:#x}"
            );
        }
    }
}
