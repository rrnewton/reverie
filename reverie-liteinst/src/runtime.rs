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

use liteinst2::patcher::JumpPatchPlan;
use liteinst2::patcher::LiveJumpPatch;
use liteinst2::patcher::PatchError;
use liteinst2::patcher::prepare_live_patching;
use liteinst2::scanner::InstructionScanner;
use liteinst2::trampoline::ExecutableTrampoline;
use liteinst2::trampoline::HookContext;
use liteinst2::trampoline::HookSite;
use liteinst2::trampoline::InstalledHook;
use liteinst2::trampoline::TrampolineArena;
use liteinst2::trampoline::TrampolineError;
use liteinst2::trampoline::TrampolinePlan;
use reverie_preload::BuiltinTool;
use reverie_preload::dispatch::SyscallDispatcher;
use reverie_preload::dispatch::SyscallEvent as PreloadSyscallEvent;
use reverie_preload::dispatch::is_fork_like;
use reverie_preload::fork::ForkHook;
use reverie_preload::lifecycle::InProcessSeccomp;
use reverie_preload::lifecycle::RuntimeConfig;
use reverie_preload::trap::raw_syscall6;

use crate::COMPAT_EVENT_COOKIE_ENV;
use crate::COMPAT_EVENT_FD_ENV;

pub(crate) const HOST_RUNTIME_ENV: &str = "REVERIE_LITEINST_HOST_RUNTIME";
pub(crate) const HOST_BEGIN_MARKER: u64 = 0x7265_766c_6900_0001;
pub(crate) const HOST_READY_MARKER: u64 = 0x7265_766c_6900_0002;
pub(crate) const HOST_HELPER_RETURN_MARKER: u64 = 0x7265_766c_6900_0003;
pub(crate) const HOST_SYSCALL_MARKER: u64 = 0x7265_766c_6900_0004;
const HOST_HANDSHAKE_VERSION: u64 = 4;
const HOST_INSTALL_RESULT_VERSION: u64 = 2;
const HOST_HELPER_STACK_BYTES: usize = 256 * 1024;

global_asm!(
    r#"
    .text
    .p2align 4
    .global reverie_liteinst_host_begin
    .type reverie_liteinst_host_begin,@function
reverie_liteinst_host_begin:
    mov rax, 0x7265766c69000001
    int3
    .global reverie_liteinst_host_begin_rip
reverie_liteinst_host_begin_rip:
    ret
    .size reverie_liteinst_host_begin, .-reverie_liteinst_host_begin

    .p2align 4
    .global reverie_liteinst_host_ready
    .type reverie_liteinst_host_ready,@function
reverie_liteinst_host_ready:
    mov rax, 0x7265766c69000002
    int3
    .global reverie_liteinst_host_ready_rip
reverie_liteinst_host_ready_rip:
    ret
    .size reverie_liteinst_host_ready, .-reverie_liteinst_host_ready

    .p2align 4
    .global reverie_liteinst_host_helper_return
    .type reverie_liteinst_host_helper_return,@function
reverie_liteinst_host_helper_return:
    mov r10, 0x7265766c69000003
    int3
    .global reverie_liteinst_host_helper_return_rip
reverie_liteinst_host_helper_return_rip:
    ret
    .size reverie_liteinst_host_helper_return, .-reverie_liteinst_host_helper_return

    .p2align 4
    .global reverie_liteinst_host_syscall_trap
    .type reverie_liteinst_host_syscall_trap,@function
reverie_liteinst_host_syscall_trap:
    mov rax, 0x7265766c69000004
    int3
    .global reverie_liteinst_host_syscall_trap_rip
reverie_liteinst_host_syscall_trap_rip:
    ret
    .size reverie_liteinst_host_syscall_trap, .-reverie_liteinst_host_syscall_trap

    .p2align 4
    .global reverie_liteinst_host_syscall_trap_call
    .hidden reverie_liteinst_host_syscall_trap_call
    .type reverie_liteinst_host_syscall_trap_call,@function
reverie_liteinst_host_syscall_trap_call:
    call reverie_liteinst_host_syscall_trap
    .global reverie_liteinst_host_syscall_trap_return_rip
reverie_liteinst_host_syscall_trap_return_rip:
    ret
    .size reverie_liteinst_host_syscall_trap_call, .-reverie_liteinst_host_syscall_trap_call

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
    fn reverie_liteinst_host_begin(frame: *const HostHandshakeFrame);
    static reverie_liteinst_host_begin_rip: u8;
    fn reverie_liteinst_host_ready(frame: *const HostHandshakeFrame);
    static reverie_liteinst_host_ready_rip: u8;
    fn reverie_liteinst_host_helper_return();
    static reverie_liteinst_host_helper_return_rip: u8;
    fn reverie_liteinst_host_syscall_trap_call(frame: *mut HostSyscallFrame);
    fn reverie_liteinst_host_syscall_trap(frame: *mut HostSyscallFrame);
    static reverie_liteinst_host_syscall_trap_rip: u8;
    static reverie_liteinst_host_syscall_trap_return_rip: u8;
    fn reverie_liteinst_native_cpuid(eax: u32, ecx: u32, result: *mut NativeCpuidResult);
    fn reverie_liteinst_native_rdtsc() -> u64;
    fn reverie_liteinst_native_rdtscp(aux: *mut u32) -> u64;
}

// TODO-HUMAN-REVIEW(PR-270): Review raw hot-trap test/provenance ABI. This
// exposes an address for negative testing; caller validation, not secrecy, is
// the accidental-collision boundary.
#[unsafe(no_mangle)]
pub extern "C" fn reverie_liteinst_host_syscall_trap_address() -> *const libc::c_void {
    reverie_liteinst_host_syscall_trap as *const libc::c_void
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct HostHandshakeFrame {
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

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct HostInstallResult {
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

#[repr(align(16))]
struct HostHelperStack([u8; HOST_HELPER_STACK_BYTES]);

static mut HOST_HELPER_STACK: HostHelperStack = HostHelperStack([0; HOST_HELPER_STACK_BYTES]);
static mut HOST_INSTALL_RESULT: HostInstallResult = HostInstallResult {
    version: 0,
    site_start: 0,
    site_len: 0,
    relocated_tail: 0,
    trampoline_start: 0,
    trampoline_len: 0,
    arena_writable_start: 0,
    arena_writable_len: 0,
    arena_executable_start: 0,
    arena_executable_len: 0,
    instruction_len: 0,
    straddle_prefix: 0,
    complete: 0,
};

const UNSET_RESULT: i64 = i64::MIN;
const SYS_IO_PGETEVENTS: i64 = 333;
const TOOL_STRACE: u8 = 1;
const TOOL_COMPAT: u8 = 2;
const TOOL_REVERIE: u8 = 3;

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-252): Review shared reverie-preload built-in tool selection.
/// `REVERIE_LITEINST_TOOL` value selecting the shared passthrough built-in.
pub const TOOL_PASSTHROUGH: &str = "passthrough";
/// `REVERIE_LITEINST_TOOL` value selecting the shared getpid-spoofing built-in.
pub const TOOL_SPOOF_GETPID: &str = "spoof-getpid";
const EVENT_CHANNEL_IDENTITY_FAILURE_STATUS: i32 = 120;
const EVENT_CHANNEL_WRITE_FAILURE_STATUS: i32 = 121;
const IN_GUEST_STAGE_WRITE_FAILURE_STATUS: i32 = 123;
pub(crate) const VDSO_PUBLICATION_FAILURE_STATUS: i32 = 124;
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
static VDSO_INDIRECT_GUARD_READY: AtomicBool = AtomicBool::new(false);
static VDSO_NORMAL_REWRITE_START: AtomicU64 = AtomicU64::new(0);
static VDSO_FALLBACK_REWRITE_START: AtomicU64 = AtomicU64::new(0);
static VDSO_INDIRECT_GUARD_START: AtomicU64 = AtomicU64::new(0);
static VDSO_INDIRECT_GUARD_END: AtomicU64 = AtomicU64::new(0);
static VDSO_PKRU_SUPPORT: AtomicU8 = AtomicU8::new(0);

#[cfg(test)]
static VDSO_PROTECTION_FAILURE_CALL: AtomicU8 = AtomicU8::new(0);
#[cfg(test)]
static VDSO_PROTECTION_SECOND_FAILURE_CALL: AtomicU8 = AtomicU8::new(0);
#[cfg(test)]
static VDSO_PROTECTION_CALLS: AtomicU8 = AtomicU8::new(0);
#[cfg(test)]
static VDSO_POST_TARGET_ENTRY_MUTATION: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static VDSO_AFTER_GUARD_FAILURE: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static VDSO_AFTER_FALLBACK_FAILURE: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static VDSO_PKRU_TEST_OVERRIDE: AtomicU64 = AtomicU64::new(u64::MAX);

#[cfg(test)]
const VDSO_ROLLBACK_INITIAL_FAILURE_MARKER: &[u8] = b"vdso-terminal-rollback-initial-rx-failure\n";
#[cfg(test)]
const VDSO_ROLLBACK_ATTEMPT_MARKER: &[u8] = b"vdso-terminal-rollback-attempt\n";
#[cfg(test)]
const VDSO_ROLLBACK_TERMINAL_MARKER: &[u8] = b"vdso-terminal-rollback-rx-failure\n";
#[cfg(test)]
const VDSO_REDIRECT_OWNERSHIP_TERMINAL_MARKER: &[u8] = b"vdso-terminal-redirect-ownership\n";
#[cfg(test)]
const VDSO_PRELOAD_TERMINAL_MARKER: &[u8] = b"vdso-terminal-preload-error\n";
#[cfg(test)]
const VDSO_FAULTING_TERMINAL_MARKER: &[u8] = b"vdso-terminal-faulting-error\n";
#[cfg(test)]
const VDSO_GUARD_LOAD_TERMINAL_MARKER: &[u8] = b"vdso-terminal-sgx-guard-load\n";
#[cfg(test)]
const VDSO_GUARD_RANGES_UNPUBLISHED_TERMINAL_MARKER: &[u8] =
    b"vdso-terminal-sgx-guard-ranges-unpublished\n";
#[cfg(test)]
const VDSO_GUARD_PKRU_SUPPORT_UNPUBLISHED_TERMINAL_MARKER: &[u8] =
    b"vdso-terminal-sgx-guard-pkru-support-unpublished\n";
#[cfg(test)]
const VDSO_GUARD_TARGET_TERMINAL_MARKER: &[u8] = b"vdso-terminal-sgx-guard-target\n";
#[cfg(test)]
const VDSO_GUARD_PKRU_TERMINAL_MARKER: &[u8] = b"vdso-terminal-sgx-guard-pkru\n";
#[cfg(test)]
const VDSO_BATCH_COMPLETE_MARKER: &[u8] = b"vdso-terminal-batch-complete\n";

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

// Host initialization cannot be retried after Begin: preparation can publish
// process-global OnceLocks before returning an error. This does not claim the
// other runtime installers or their reversible preflight work.
static HOST_INITIALIZATION_STARTED: AtomicBool = AtomicBool::new(false);
static ARENAS: OnceLock<Vec<RuntimeArena>> = OnceLock::new();
static SITES: OnceLock<Box<[SiteSlot]>> = OnceLock::new();
static PAGE_SIZE: AtomicU64 = AtomicU64::new(0);
static INSTALL_HELD: AtomicBool = AtomicBool::new(false);
static INSTRUCTION_SUBSCRIPTIONS: AtomicU8 = AtomicU8::new(0);
static PATCH_PUBLICATION: AtomicU8 = AtomicU8::new(PatchPublication::Concurrent as u8);
static PROCESS_FORKS_ALLOWED: AtomicBool = AtomicBool::new(true);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstructionEventKind {
    Cpuid,
    Rdtsc,
    Rdtscp,
}

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
    static RCB_CLOCK: Cell<*mut reverie_ptrace::InGuestRcbCounter> =
        const { Cell::new(ptr::null_mut()) };
    static RCB_CLOCK_OWNER: Cell<libc::pid_t> = const { Cell::new(0) };
    static RCB_CLOCK_UNAVAILABLE: Cell<bool> = const { Cell::new(false) };
    static RCB_HANDLER_ENTRY: Cell<u64> = const { Cell::new(0) };
    static RCB_HANDLER_DEDUCTION: Cell<u64> = const { Cell::new(0) };
    static RCB_HANDLER_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Install the current thread's in-guest RCB clock before seccomp is active.
pub(crate) fn initialize_rcb_clock() -> io::Result<()> {
    initialize_rcb_clock_with(|| unsafe {
        reverie_ptrace::InGuestRcbCounter::current_thread_with_syscall_gate(raw_syscall6)
    })
}

fn initialize_rcb_clock_with(
    create: impl FnOnce() -> Result<reverie_ptrace::InGuestRcbCounter, reverie::Errno>,
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
        Err(_) => return Ok(()),
    };
    let active_entry = if active_depth == 0 {
        0
    } else {
        clock
            .read()
            .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))?
    };
    RCB_CLOCK.set(Box::into_raw(Box::new(clock)));
    RCB_CLOCK_OWNER.set(owner);
    RCB_CLOCK_UNAVAILABLE.set(false);
    RCB_HANDLER_ENTRY.set(active_entry);
    RCB_HANDLER_DEDUCTION.set(0);
    RCB_HANDLER_DEPTH.set(active_depth);
    Ok(())
}

fn rcb_clock() -> io::Result<Option<&'static reverie_ptrace::InGuestRcbCounter>> {
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
    writable_start: u64,
    writable_end: u64,
    executable_start: u64,
    executable_end: u64,
    arena: TrampolineArena,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeMap {
    start: u64,
    end: u64,
    offset: u64,
    device: (u64, u64),
    inode: u64,
    pathname: String,
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

#[derive(Clone, Copy)]
pub(crate) enum SyscallDispatch {
    Trap,
    InstalledHook(DirectHookSource),
    Fallback,
}

#[derive(Clone, Copy)]
pub(crate) struct SyscallEvent {
    pub(crate) number: i64,
    pub(crate) args: [u64; 6],
    pub(crate) instruction_pointer: u64,
    pub(crate) result: i64,
    pub(crate) context: usize,
    pub(crate) dispatch: SyscallDispatch,
    pub(crate) guest_pkru: Option<u32>,
}

impl SyscallEvent {
    /// Forward only this guest operation. Runtime-private syscall buffers must
    /// retain caller access and continue to use the ordinary raw gate.
    pub(crate) unsafe fn forward(&mut self) -> i64 {
        let result = unsafe {
            reverie_preload::trap::raw_syscall6_with_result(self.number, self.args, self.guest_pkru)
        };
        // Permission effects survive negative errno and later Tool result
        // transformation. Private injection never calls this operation.
        self.guest_pkru = result.pkru;
        result.result
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-252): Review shared reverie-preload built-in tool parser.
/// Parses a shared `reverie-preload` [`BuiltinTool`] from a `REVERIE_LITEINST_TOOL`
/// value, returning `None` for the LiteInst-native `strace`/`compat` modes and
/// any other value.
///
/// This is the LiteInst analog of e9patch's `builtin_tool_from_env_value`: it
/// lets the single `REVERIE_LITEINST_TOOL` selector name a shared built-in
/// installed verbatim through [`reverie_preload::install_builtin`], bypassing the
/// LiteInst patching dispatcher.
pub fn builtin_tool_from_env_value(value: &OsStr) -> Option<BuiltinTool> {
    match value.to_str()? {
        TOOL_PASSTHROUGH => Some(BuiltinTool::Passthrough),
        TOOL_SPOOF_GETPID => Some(BuiltinTool::SpoofGetpid),
        _ => None,
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-252): Review shared built-in installation entry point.
/// Installs a shared `reverie-preload` built-in tool verbatim.
///
/// Unlike [`install_runtime`], this does NOT prepare LiteInst instrumentation:
/// the shared built-ins install their own SIGSYS handler and seccomp filter via
/// [`reverie_preload::install_builtin`] and do not patch syscall sites. This
/// proves the LiteInst fallback/trap path can service and MUTATE a syscall
/// result (for example `getpid` -> `SPOOF_PID`), matching e9patch's
/// `install_builtin_runtime`.
///
/// # Safety
///
/// The dynamic loader must call this exactly once before application threads
/// start; it installs process-wide, irreversible seccomp state.
pub(crate) unsafe fn install_builtin_runtime(tool: BuiltinTool) -> io::Result<()> {
    unsafe { reverie_preload::install_builtin(tool) }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-254): Review launcher-selected shared RuntimeConfig alt-stack knob.
/// Environment variable selecting the shared [`RuntimeConfig::use_alt_stack`]
/// knob for the in-guest runtime's `SIGSYS` handler.
///
/// The [`RuntimeConfig`] and the controller that honors it live in
/// `reverie-preload` and are reviewed exactly once; both ld-preload backends
/// install through that same shared seam. Only the env-var spelling is
/// LiteInst's, exactly as with `REVERIE_LITEINST_TOOL`. This is the LiteInst
/// analog of e9patch's `REVERIE_E9PATCH_ALT_STACK`.
///
/// When unset the shared default applies ([`RuntimeConfig::default`], alt stack
/// **on**). It applies to the LiteInst-dispatcher install path
/// ([`install_runtime`], used by the `strace`/`compat`/Detcore modes); a shared
/// [`BuiltinTool`] runs through `install_builtin`, which uses the shared default.
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
/// is unit-testable without touching process-global state, matching
/// [`builtin_tool_from_env_value`] and e9patch's `alt_stack_from_env_value`.
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
            unsafe { exit_now(126) };
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
            unsafe { exit_now(126) };
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
    if std::env::var_os(HOST_RUNTIME_ENV).as_deref() == Some(OsStr::new("1")) {
        return initialize_host_runtime();
    }
    let tool_value = std::env::var_os("REVERIE_LITEINST_TOOL");
    // Prefer a shared reverie-preload built-in when the selector names one, so a
    // single env var is a superset of the LiteInst-native strace/compat modes
    // (matches e9patch's single TOOL_ENV selecting shared built-ins).
    if let Some(value) = tool_value.as_deref()
        && let Some(tool) = builtin_tool_from_env_value(value)
    {
        // SAFETY: the loader calls this once before application threads start.
        return unsafe { install_builtin_runtime(tool) };
    }
    let mode = match tool_value.as_deref() {
        None => return Ok(()),
        Some(value) if value == OsStr::new("strace") => TOOL_STRACE,
        Some(value) if value == OsStr::new("compat") => TOOL_COMPAT,
        Some(value) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported REVERIE_LITEINST_TOOL value {value:?}"),
            ));
        }
    };
    TOOL_MODE.store(mode, Ordering::Release);
    let event_channel = if mode == TOOL_COMPAT {
        compatibility_event_channel()?
    } else {
        None
    };
    let event_fd = event_channel
        .as_ref()
        .map_or(libc::STDERR_FILENO, |channel| channel.fd);
    EVENT_FD.store(event_fd, Ordering::Release);
    if let Some(channel) = event_channel {
        EVENT_COOKIE.store(channel.cookie, Ordering::Release);
        EVENT_DEVICE.store(channel.device, Ordering::Release);
        EVENT_INODE.store(channel.inode, Ordering::Release);
        // SAFETY: initialization runs before application threads start.
        unsafe {
            std::env::remove_var(COMPAT_EVENT_FD_ENV);
            std::env::remove_var(COMPAT_EVENT_COOKIE_ENV);
        }
    }

    install_runtime(
        crate::stats::GuestStatsHooks::DISABLED,
        PatchPublication::Concurrent,
        InstructionSubscriptions::default(),
        &[],
    )
}

fn host_handshake_frame() -> HostHandshakeFrame {
    // SAFETY: this only forms the address of the dedicated static helper stack;
    // it neither reads nor creates a Rust reference to its mutable contents.
    let stack_start = unsafe { core::ptr::addr_of_mut!(HOST_HELPER_STACK.0) as *mut u8 as usize };
    HostHandshakeFrame {
        version: HOST_HANDSHAKE_VERSION,
        begin_rip: core::ptr::addr_of!(reverie_liteinst_host_begin_rip) as usize as u64,
        ready_rip: core::ptr::addr_of!(reverie_liteinst_host_ready_rip) as usize as u64,
        install_helper: reverie_liteinst_install_site_for_ptrace as *const () as usize as u64,
        helper_stack_top: (stack_start + HOST_HELPER_STACK_BYTES) as u64,
        helper_return: reverie_liteinst_host_helper_return as *const () as usize as u64,
        helper_return_rip: core::ptr::addr_of!(reverie_liteinst_host_helper_return_rip) as usize
            as u64,
        syscall_trap_rip: core::ptr::addr_of!(reverie_liteinst_host_syscall_trap_rip) as usize
            as u64,
        syscall_trap_return_rip: core::ptr::addr_of!(reverie_liteinst_host_syscall_trap_return_rip)
            as usize as u64,
        install_result: core::ptr::addr_of!(HOST_INSTALL_RESULT) as usize as u64,
    }
}

fn initialize_host_runtime() -> io::Result<()> {
    initialize_host_runtime_with(prepare_instrumentation)
}

pub(crate) fn initialize_host_runtime_explicit(config: crate::HostRuntimeConfig) -> io::Result<()> {
    if config.version != crate::HOST_RUNTIME_CONFIG_VERSION {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let staleness = liteinst2::patcher::StalenessBudget::new(config.straddler_staleness_ticks);
    initialize_host_runtime_with(|| {
        crate::straddler::initialize(staleness)?;
        prepare_instrumentation_state()
    })
}

fn initialize_host_runtime_with(prepare: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    if reverie_preload::trap::has_dispatcher()
        || crate::straddler::is_initialized()
        || SITES.get().is_some()
        || ARENAS.get().is_some()
    {
        return Err(io::Error::from_raw_os_error(libc::EALREADY));
    }
    HOST_INITIALIZATION_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| io::Error::from_raw_os_error(libc::EALREADY))?;
    let frame = host_handshake_frame();
    // SAFETY: the launcher validates this exact DSO/RIP/frame before suppressing
    // the trap. The function returns normally after ptrace resumes the tracee.
    unsafe { reverie_liteinst_host_begin(&frame) };
    prepare()?;
    // SAFETY: identical handshake contract; all helper state is now published.
    unsafe { reverie_liteinst_host_ready(&frame) };
    Ok(())
}

pub(crate) fn initialize_reverie_tool(
    stats: crate::stats::GuestStatsHooks,
    publication: PatchPublication,
    instructions: InstructionSubscriptions,
    vdso_sites: &[reverie_ptrace::VdsoSyscallSite],
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
    PROCESS_FORKS_ALLOWED.store(process_forks_allowed, Ordering::Release);
    TOOL_MODE.store(TOOL_REVERIE, Ordering::Release);
    install_runtime(stats, publication, instructions, vdso_sites)
}

fn finish_after_vdso_install(result: io::Result<()>, vdso_was_installed: bool) -> io::Result<()> {
    if result.is_err() && vdso_was_installed {
        // The public Tool installer has already published HANDLER and this
        // function has published the complete vDSO batch. Returning would
        // expose an installation that a caller cannot safely retry.
        terminal_vdso_publication_failure();
    }
    result
}

fn complete_runtime_after_vdso(
    vdso_was_installed: bool,
    install_preload: impl FnOnce() -> io::Result<()>,
    enable_faulting: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    finish_after_vdso_install(install_preload(), vdso_was_installed)?;
    finish_after_vdso_install(enable_faulting(), vdso_was_installed)
}

/// No initialization error may return after public process activation starts.
/// Signal actions, fallback storage, descriptor ownership, the Tool handler,
/// instrumentation registries, and seccomp are not jointly reversible.
pub(crate) fn finish_after_activation_started(result: io::Result<()>) -> io::Result<()> {
    if result.is_err() {
        terminal_vdso_publication_failure();
    }
    result
}

/// A panic after the public activation boundary is no more recoverable than an
/// `io::Error`: process-global Tool and runtime state may already be visible.
pub(crate) fn terminal_after_activation_started() -> ! {
    terminal_vdso_publication_failure()
}

fn install_runtime(
    stats: crate::stats::GuestStatsHooks,
    publication: PatchPublication,
    instructions: InstructionSubscriptions,
    vdso_sites: &[reverie_ptrace::VdsoSyscallSite],
) -> io::Result<()> {
    PATCH_PUBLICATION.store(publication as u8, Ordering::Release);
    prepare_instrumentation()?;
    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(PR-254): Review launcher-selected RuntimeConfig at the install seam.
    let config = runtime_config_from_env()?;
    install_instruction_signal_handler(instructions, config.use_alt_stack)?;
    install_vdso_sites(vdso_sites)?;
    complete_runtime_after_vdso(
        !vdso_sites.is_empty(),
        || unsafe {
            reverie_preload::install(
                Box::new(LiteinstDispatcher::new(stats, publication)),
                &InProcessSeccomp,
                &config,
            )
        },
        || enable_instruction_faulting(instructions),
    )
}

struct CompatibilityEventChannel {
    fd: libc::c_int,
    cookie: u64,
    device: u64,
    inode: u64,
}

fn compatibility_event_channel() -> io::Result<Option<CompatibilityEventChannel>> {
    let Some(value) = std::env::var_os(COMPAT_EVENT_FD_ENV) else {
        if std::env::var_os(COMPAT_EVENT_COOKIE_ENV).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{COMPAT_EVENT_COOKIE_ENV} requires {COMPAT_EVENT_FD_ENV}"),
            ));
        }
        return Ok(None);
    };
    let value = value.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{COMPAT_EVENT_FD_ENV} must be valid UTF-8"),
        )
    })?;
    let fd = value.parse::<libc::c_int>().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{COMPAT_EVENT_FD_ENV} must be a non-negative descriptor"),
        )
    })?;
    let flags = if fd < 0 {
        -1
    } else {
        unsafe { libc::fcntl(fd, libc::F_GETFL) }
    };
    if flags < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{COMPAT_EVENT_FD_ENV} does not name an open descriptor"),
        ));
    }
    if flags & libc::O_ACCMODE == libc::O_RDONLY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{COMPAT_EVENT_FD_ENV} must name a writable descriptor"),
        ));
    }
    let cookie = std::env::var(COMPAT_EVENT_COOKIE_ENV)
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{COMPAT_EVENT_COOKIE_ENV} is required with {COMPAT_EVENT_FD_ENV}"),
            )
        })?
        .parse::<u64>()
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{COMPAT_EVENT_COOKIE_ENV} must be a nonzero decimal u64"),
            )
        })?;
    if cookie == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{COMPAT_EVENT_COOKIE_ENV} must be a nonzero decimal u64"),
        ));
    }

    let mut metadata: libc::stat = unsafe { core::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut metadata) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{COMPAT_EVENT_FD_ENV} metadata could not be read"),
        ));
    }
    if metadata.st_mode & libc::S_IFMT != libc::S_IFIFO {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{COMPAT_EVENT_FD_ENV} must name a pipe"),
        ));
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{COMPAT_EVENT_FD_ENV} could not be made nonblocking"),
        ));
    }

    Ok(Some(CompatibilityEventChannel {
        fd,
        cookie,
        device: metadata.st_dev,
        inode: metadata.st_ino,
    }))
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
            let (device_major, device_minor) = device.split_once(':')?;
            let permissions = permissions.as_bytes();
            Some(RuntimeMap {
                start: u64::from_str_radix(start, 16).ok()?,
                end: u64::from_str_radix(end, 16).ok()?,
                offset: u64::from_str_radix(offset, 16).ok()?,
                device: (
                    u64::from_str_radix(device_major, 16).ok()?,
                    u64::from_str_radix(device_minor, 16).ok()?,
                ),
                inode: inode.parse().ok()?,
                pathname: fields.next().unwrap_or("").to_owned(),
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
    crate::straddler::initialize_from_environment()?;
    prepare_instrumentation_state()
}

fn prepare_instrumentation_state() -> io::Result<()> {
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
        let (writable, executable) = discover_arena_aliases(&before, &after)?;
        arenas.push(RuntimeArena {
            mapping_start,
            mapping_end,
            mapping_name,
            writable_start: writable.start,
            writable_end: writable.end,
            executable_start: executable.start,
            executable_end: executable.end,
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
/// Total and per-syscall counts for trapped sites without an installed hook
/// (`SITE_FALLBACK` or an unclaimable site), including successful typed Tool
/// dispatch after signal return.
struct FallbackCounters {
    total: AtomicU64,
    by_number: [AtomicU64; TRACKED_SYSCALLS],
}

/// Direct callback counts that do not have an installed source-site hook.
///
/// Ordinary hooks retain their per-`SiteSlot` counters. The getrandom adapter
/// branches from a different instruction and deliberately owns no source slot,
/// so its Tool callbacks require separate process-local accounting.
struct DirectHookCounters {
    adapters: AtomicU64,
}

impl DirectHookCounters {
    const fn new() -> Self {
        Self {
            adapters: AtomicU64::new(0),
        }
    }

    fn record_adapter(&self) {
        self.adapters.fetch_add(1, Ordering::Relaxed);
    }

    fn adapter_count(&self) -> u64 {
        self.adapters.load(Ordering::Relaxed)
    }

    fn reset(&self) {
        self.adapters.store(0, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectHookSource {
    InstalledSite,
    VdsoAdapter,
}

static DIRECT_HOOK_COUNTERS: DirectHookCounters = DirectHookCounters::new();

fn record_direct_hook(
    counters: &DirectHookCounters,
    source: DirectHookSource,
    site: Option<&SiteSlot>,
) {
    match source {
        DirectHookSource::InstalledSite => {
            if let Some(site) = site {
                site.hook_count.fetch_add(1, Ordering::Relaxed);
            }
        }
        DirectHookSource::VdsoAdapter => counters.record_adapter(),
    }
}

fn direct_hook_count(counters: &DirectHookCounters, sites: &[SiteSlot]) -> u64 {
    sites.iter().fold(counters.adapter_count(), |total, site| {
        total.wrapping_add(site.hook_count.load(Ordering::Relaxed))
    })
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
static FALLBACK_REFUSALS: FallbackCounters = FallbackCounters::new();

/// Record that one syscall reached fallback dispatch without an installed hook.
///
/// Typed Tool mode can service these calls after signal return. This is the
/// by-syscall-number analog of the per-site
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

/// Total syscalls that reached fallback dispatch.
///
/// Includes successful typed Tool calls; this counter alone does not identify
/// unsupported syscalls or establish determinism coverage.
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

pub(crate) fn fallback_refusal_count() -> u64 {
    FALLBACK_REFUSALS.total()
}

pub(crate) fn fallback_syscall_refusal_count(number: i64) -> u64 {
    FALLBACK_REFUSALS.by_number(number)
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-260): Review the fork-child per-process observability reset.
/// Reset every fallback-surface counter to zero for the current process.
///
/// LiteInst's process-wide [`FALLBACK_COUNTERS`], adapter direct-hook counter,
/// and each patch site's per-site `trap`/`hook` counts ([`site_counts`]) are
/// inherited by a `fork`/`clone` child copy-on-write, so without a
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
    FALLBACK_REFUSALS.reset();
    DIRECT_HOOK_COUNTERS.reset();
    if let Some(sites) = SITES.get() {
        reset_site_observability(sites);
    }
}

pub(crate) fn submit_process_stats(
    tid: reverie::Tid,
    stats: crate::stats::GuestStatsHooks,
) -> io::Result<()> {
    let site_registry = SITES.get().map_or(&[][..], |sites| sites.as_ref());
    let direct_hooks = direct_hook_count(&DIRECT_HOOK_COUNTERS, site_registry);
    let sites = site_registry
        .iter()
        .filter_map(|site| {
            let trap_hits = site.trap_count.load(Ordering::Relaxed);
            let hook_hits = site.hook_count.load(Ordering::Relaxed);
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

pub(crate) fn record_fork_child_dispatch(
    event: &SyscallEvent,
    stats: crate::stats::GuestStatsHooks,
) {
    match event.dispatch {
        SyscallDispatch::InstalledHook(source) => {
            let site = match source {
                DirectHookSource::InstalledSite => find_site(event.instruction_pointer),
                DirectHookSource::VdsoAdapter => None,
            };
            record_direct_hook(&DIRECT_HOOK_COUNTERS, source, site);
        }
        SyscallDispatch::Fallback => {
            if let Some(site) = find_site(event.instruction_pointer) {
                site.trap_count.fetch_add(1, Ordering::Relaxed);
            }
            record_fallback_dispatch(event.number);
            stats.record_path(crate::LiteinstDispatchPath::InGuestSigsys);
            if stats.is_enabled() {
                record_enabled_fallback_stats(stats, event.instruction_pointer);
            }
        }
        SyscallDispatch::Trap => {}
    }
}

/// Start a fork child's process-local counters from zero, then reattribute the
/// callback that produced the fork result. The event retains whether an
/// installed callback came from a source `SiteSlot` or the slotless getrandom
/// adapter, so the reset cannot lose or duplicate that current callback.
pub(crate) fn reset_and_record_fork_child_dispatch(
    event: &SyscallEvent,
    stats: crate::stats::GuestStatsHooks,
) {
    reset_fallback_observability();
    stats.reset_after_fork();
    record_fork_child_dispatch(event, stats);
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

fn set_vdso_mapping_protection(start: u64, len: u64, protection: i32) -> io::Result<()> {
    #[cfg(test)]
    {
        let call = VDSO_PROTECTION_CALLS.fetch_add(1, Ordering::AcqRel) + 1;
        let first_failure = VDSO_PROTECTION_FAILURE_CALL.load(Ordering::Acquire);
        let second_failure = VDSO_PROTECTION_SECOND_FAILURE_CALL.load(Ordering::Acquire);
        if first_failure == call || second_failure == call {
            if second_failure != 0 {
                assert_eq!(protection, libc::PROT_READ | libc::PROT_EXEC);
            }
            if second_failure != 0 && first_failure == call {
                emit_vdso_terminal_test_marker(VDSO_ROLLBACK_INITIAL_FAILURE_MARKER);
            }
            if second_failure == call {
                emit_vdso_terminal_test_marker(VDSO_ROLLBACK_TERMINAL_MARKER);
            }
            return Err(io::Error::other(format!(
                "injected LiteInst vDSO protection failure at call {call}"
            )));
        }
    }
    // SAFETY: callers bind this operation to the complete vDSO mapping range.
    unsafe { set_mapping_protection(start, len, protection) }
}

struct InstallGuard;

#[derive(Clone, Copy)]
#[repr(u8)]
pub(crate) enum PatchPublication {
    /// The stopped-tracee helper is the only thread able to reach live code.
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
    INSTALL_HELD
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .map(|_| InstallGuard)
        .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "LiteInst installation is busy"))
}

unsafe fn install_site_hook(
    address: u64,
    slot: &'static SiteSlot,
    callback: liteinst2::trampoline::HookCallback,
    publication: PatchPublication,
    expected_instruction: &[u8],
    manage_protection: bool,
) -> io::Result<HostInstallResult> {
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

    // Publish candidate metadata before installation so a failed helper can
    // still classify its explicit ptrace fallback branch.
    let candidate_result = HostInstallResult {
        version: HOST_INSTALL_RESULT_VERSION,
        site_start: address,
        site_len: liteinst2::patcher::WORD_PATCH_BYTES as u64,
        instruction_len: instruction_len as u64,
        straddle_prefix: straddle_prefix as u64,
        ..HostInstallResult::default()
    };
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(HOST_INSTALL_RESULT),
            candidate_result,
        );
    }
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
        // SAFETY: the ptrace controller serializes this helper while every
        // other tracee thread is stopped. Hermit likewise schedules only one
        // guest thread at a time, so no other thread can fetch the site.
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

    let relocated_tail = installed.trampoline().relocated_tail_address();
    let trampoline_start = installed.trampoline().address();
    let trampoline_len = installed.trampoline().allocation_len() as u64;
    let result = HostInstallResult {
        version: HOST_INSTALL_RESULT_VERSION,
        site_start: address,
        site_len: liteinst2::patcher::WORD_PATCH_BYTES as u64,
        relocated_tail,
        trampoline_start,
        trampoline_len,
        arena_writable_start: arena.writable_start,
        arena_writable_len: arena.writable_end - arena.writable_start,
        arena_executable_start: arena.executable_start,
        arena_executable_len: arena.executable_end - arena.executable_start,
        instruction_len: instruction_len as u64,
        straddle_prefix: straddle_prefix as u64,
        complete: 1,
    };
    let installed = Box::into_raw(Box::new(installed));
    slot.hook.store(installed, Ordering::Release);
    slot.instruction_len
        .store(instruction_len as u8, Ordering::Release);
    slot.straddle_prefix
        .store(straddle_prefix as u8, Ordering::Release);
    slot.state.store(SITE_ACTIVE, Ordering::Release);
    Ok(result)
}

fn reset_vdso_site_for_retry(site: &SiteSlot) {
    debug_assert!(site.hook.load(Ordering::Acquire).is_null());
    site.mapping_end.store(0, Ordering::Release);
    site.instruction_len.store(0, Ordering::Release);
    site.straddle_prefix.store(0, Ordering::Release);
    // Publish the reclaimable state last. `claim_existing_site` may claim the
    // slot as soon as this store becomes visible.
    site.state.store(SITE_STALE, Ordering::Release);
}

struct InstalledVdsoHook<'a> {
    info: &'a reverie_ptrace::VdsoSyscallSite,
    site: &'static SiteSlot,
    original_hook_bytes: [u8; liteinst2::patcher::WORD_PATCH_BYTES],
    bound_hook_bytes: [u8; liteinst2::patcher::WORD_PATCH_BYTES],
}

struct PreparedVdsoIndirectGuard {
    trampoline: ExecutableTrampoline,
    patch_plan: JumpPatchPlan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VdsoAdapterRedirects {
    None,
    FallbackOnly,
    Complete,
}

impl VdsoAdapterRedirects {
    fn normal_is_published(self) -> bool {
        self == Self::Complete
    }

    fn fallback_is_published(self) -> bool {
        matches!(self, Self::FallbackOnly | Self::Complete)
    }
}

struct InstalledVdsoAdapter<'a> {
    info: &'a reverie_ptrace::VdsoSyscallSite,
    adapter_trampoline: ExecutableTrampoline,
    guard_trampoline: ExecutableTrampoline,
    guard_patch: LiveJumpPatch,
    guard_original: [u8; liteinst2::patcher::WORD_PATCH_BYTES],
    guard_replacement: [u8; liteinst2::patcher::WORD_PATCH_BYTES],
    fallback_replacement: [u8; 5],
}

enum InstalledVdsoSite<'a> {
    Hook(InstalledVdsoHook<'a>),
    Adapter(Box<InstalledVdsoAdapter<'a>>),
}

unsafe fn read_vdso_hook_window(address: u64) -> [u8; liteinst2::patcher::WORD_PATCH_BYTES] {
    let mut bytes = [0; liteinst2::patcher::WORD_PATCH_BYTES];
    // SAFETY: the caller binds `address` to a readable executable vDSO range.
    unsafe {
        core::ptr::copy_nonoverlapping(
            address as usize as *const u8,
            bytes.as_mut_ptr(),
            bytes.len(),
        );
    }
    bytes
}

fn terminal_vdso_publication_failure() -> ! {
    // A caller reaches this only when rollback cannot prove the original
    // executable bytes, mapping protection, and registry ownership.
    unsafe { exit_now(VDSO_PUBLICATION_FAILURE_STATUS) }
}

fn terminal_vdso_rollback_failure() -> ! {
    terminal_vdso_publication_failure()
}

fn terminal_vdso_redirect_ownership_failure() -> ! {
    #[cfg(test)]
    emit_vdso_terminal_test_marker(VDSO_REDIRECT_OWNERSHIP_TERMINAL_MARKER);
    terminal_vdso_publication_failure()
}

#[cfg(test)]
fn emit_vdso_terminal_test_marker(marker: &[u8]) {
    let written = unsafe {
        raw_syscall6(
            libc::SYS_write,
            [
                libc::STDERR_FILENO as u64,
                marker.as_ptr() as u64,
                marker.len() as u64,
                0,
                0,
                0,
            ],
        )
    };
    if written != marker.len() as i64 {
        unsafe { exit_now(127) };
    }
}

fn validate_vdso_site(site_info: &reverie_ptrace::VdsoSyscallSite) -> io::Result<()> {
    let mapping_end = site_info
        .mapping_start
        .checked_add(site_info.mapping_len)
        .ok_or_else(|| io::Error::other("LiteInst vDSO mapping range overflows"))?;
    let range_is_mapped = |address: u64, len: usize| {
        address >= site_info.mapping_start
            && address
                .checked_add(len as u64)
                .is_some_and(|end| end <= mapping_end)
    };
    let live_mapping = read_runtime_maps()?.into_iter().any(|mapping| {
        mapping.start == site_info.mapping_start
            && mapping.end == mapping_end
            && mapping.offset == site_info.mapping_identity.offset
            && mapping.device == site_info.mapping_identity.device
            && mapping.inode == site_info.mapping_identity.inode
            && mapping.pathname == site_info.mapping_identity.pathname.as_ref()
            && mapping.shared == site_info.mapping_identity.shared
            && mapping.readable
            && !mapping.writable
            && mapping.executable
    });
    if !live_mapping {
        return Err(io::Error::other(
            "LiteInst vDSO mapping identity or protection changed",
        ));
    }
    if !range_is_mapped(site_info.address, liteinst2::patcher::WORD_PATCH_BYTES) {
        return Err(io::Error::other(
            "LiteInst vDSO hook window lies outside its mapping",
        ));
    }
    match &site_info.entry_patch {
        reverie_ptrace::VdsoEntryPatch::BeforeHook {
            address,
            expected,
            replacement,
        } => {
            if *address != site_info.address
                || expected.len() != replacement.len()
                || expected.len() < liteinst2::patcher::WORD_PATCH_BYTES
                || replacement.get(..2) != Some(&[0x0f, 0x05])
                || !range_is_mapped(*address, expected.len())
            {
                return Err(io::Error::other(
                    "LiteInst ordinary vDSO rewrite has invalid bound geometry",
                ));
            }
        }
        reverie_ptrace::VdsoEntryPatch::AfterTarget {
            mapping_expected,
            function_address,
            function_expected,
            normal_address,
            normal_expected,
            normal_replacement,
            fallback_address,
            fallback_expected,
            adapter_source_address,
            adapter_source_expected,
            indirect_guard_address,
            indirect_guard_expected,
            indirect_guard_displaced_len,
        } => {
            if site_info.address != *adapter_source_address
                || usize::try_from(site_info.mapping_len).ok() != Some(mapping_expected.len())
                || function_expected.is_empty()
                || normal_replacement.first() != Some(&0xe9)
                || normal_expected.len() != normal_replacement.len()
                || fallback_expected.first() != Some(&0xb8)
                || adapter_source_expected.get(..2) != Some(&[0x0f, 0x05])
                || adapter_source_expected.get(2) != Some(&0xe9)
                || indirect_guard_expected
                    != &[0x48, 0x8b, 0x40, 0x18, 0x0f, 0xae, 0xe8, 0xff, 0xd0]
                || usize::from(*indirect_guard_displaced_len) != 7
                || indirect_guard_address.checked_add(u64::from(*indirect_guard_displaced_len))
                    != indirect_guard_address.checked_add(7)
                || !range_is_mapped(*function_address, function_expected.len())
                || !range_is_mapped(*normal_address, normal_expected.len())
                || !range_is_mapped(*fallback_address, fallback_expected.len())
                || !range_is_mapped(*adapter_source_address, adapter_source_expected.len())
                || !range_is_mapped(*indirect_guard_address, indirect_guard_expected.len())
            {
                return Err(io::Error::other(
                    "LiteInst getrandom vDSO adapter has invalid bound geometry",
                ));
            }
        }
    }
    Ok(())
}

unsafe fn vdso_entry_matches_original(site_info: &reverie_ptrace::VdsoSyscallSite) -> bool {
    match &site_info.entry_patch {
        reverie_ptrace::VdsoEntryPatch::BeforeHook {
            address, expected, ..
        } => {
            let current = unsafe {
                core::slice::from_raw_parts(*address as usize as *const u8, expected.len())
            };
            current == expected.as_ref()
        }
        reverie_ptrace::VdsoEntryPatch::AfterTarget {
            mapping_expected,
            function_address,
            function_expected,
            adapter_source_address,
            adapter_source_expected,
            ..
        } => {
            let mapping = unsafe {
                core::slice::from_raw_parts(
                    site_info.mapping_start as usize as *const u8,
                    mapping_expected.len(),
                )
            };
            let function = unsafe {
                core::slice::from_raw_parts(
                    *function_address as usize as *const u8,
                    function_expected.len(),
                )
            };
            let adapter_source = unsafe {
                core::slice::from_raw_parts(
                    *adapter_source_address as usize as *const u8,
                    adapter_source_expected.len(),
                )
            };
            mapping == mapping_expected.as_ref()
                && function == function_expected.as_ref()
                && adapter_source == adapter_source_expected.as_slice()
        }
    }
}

unsafe fn publish_vdso_before_hook(site_info: &reverie_ptrace::VdsoSyscallSite) -> io::Result<()> {
    let reverie_ptrace::VdsoEntryPatch::BeforeHook {
        address,
        replacement,
        ..
    } = &site_info.entry_patch
    else {
        return Ok(());
    };
    // SAFETY: initialization owns this writable vDSO mapping exclusively.
    unsafe {
        core::ptr::copy_nonoverlapping(
            replacement.as_ptr(),
            *address as usize as *mut u8,
            replacement.len(),
        );
    }
    let current =
        unsafe { core::slice::from_raw_parts(*address as usize as *const u8, replacement.len()) };
    if current != replacement.as_ref() {
        return Err(io::Error::other(
            "LiteInst ordinary vDSO rewrite did not publish the bound bytes",
        ));
    }
    Ok(())
}

fn rel32_jump(address: u64, target: u64) -> io::Result<[u8; 5]> {
    let next = address
        .checked_add(5)
        .ok_or_else(|| io::Error::other("LiteInst vDSO branch address overflows"))?;
    let displacement = i128::from(target) - i128::from(next);
    let displacement = i32::try_from(displacement)
        .map_err(|_| io::Error::other("LiteInst vDSO callback target is outside rel32 reach"))?;
    let mut branch = [0; 5];
    branch[0] = 0xe9;
    branch[1..].copy_from_slice(&displacement.to_le_bytes());
    Ok(branch)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VdsoRewrittenRange {
    start: u64,
    end: u64,
}

impl VdsoRewrittenRange {
    fn new(start: u64, len: usize) -> io::Result<Self> {
        let end = start
            .checked_add(len as u64)
            .ok_or_else(|| io::Error::other("LiteInst vDSO rewritten range overflows"))?;
        Ok(Self { start, end })
    }

    fn contains_strict_interior(self, target: u64) -> bool {
        self.start < target && target < self.end
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VdsoIndirectGuardRanges {
    normal: VdsoRewrittenRange,
    fallback: VdsoRewrittenRange,
    guard: VdsoRewrittenRange,
}

impl VdsoIndirectGuardRanges {
    fn rejects(self, target: u64) -> bool {
        [self.normal, self.fallback, self.guard]
            .into_iter()
            .any(|range| range.contains_strict_interior(target))
    }
}

fn vdso_indirect_guard_ranges(
    site_info: &reverie_ptrace::VdsoSyscallSite,
) -> io::Result<VdsoIndirectGuardRanges> {
    let reverie_ptrace::VdsoEntryPatch::AfterTarget {
        normal_address,
        normal_expected,
        fallback_address,
        fallback_expected,
        indirect_guard_address,
        indirect_guard_displaced_len,
        ..
    } = &site_info.entry_patch
    else {
        return Err(io::Error::other(
            "LiteInst indirect guard received an ordinary vDSO site",
        ));
    };
    Ok(VdsoIndirectGuardRanges {
        normal: VdsoRewrittenRange::new(*normal_address, normal_expected.len())?,
        fallback: VdsoRewrittenRange::new(*fallback_address, fallback_expected.len())?,
        guard: VdsoRewrittenRange::new(
            *indirect_guard_address,
            usize::from(*indirect_guard_displaced_len),
        )?,
    })
}

fn publish_vdso_indirect_guard_ranges(ranges: VdsoIndirectGuardRanges) -> io::Result<()> {
    if VDSO_INDIRECT_GUARD_READY.load(Ordering::Acquire) {
        return Err(io::Error::other(
            "LiteInst vDSO indirect guard was published twice",
        ));
    }
    VDSO_NORMAL_REWRITE_START.store(ranges.normal.start, Ordering::Relaxed);
    VDSO_FALLBACK_REWRITE_START.store(ranges.fallback.start, Ordering::Relaxed);
    VDSO_INDIRECT_GUARD_START.store(ranges.guard.start, Ordering::Relaxed);
    VDSO_INDIRECT_GUARD_END.store(ranges.guard.end, Ordering::Relaxed);
    VDSO_INDIRECT_GUARD_READY.store(true, Ordering::Release);
    Ok(())
}

fn unpublish_vdso_indirect_guard_ranges() {
    // Installation and rollback are quiescent. Leaving the last addresses in
    // place means a callback that had already acquired `true` would still see
    // one coherent range set; no later callback can acquire readiness.
    VDSO_INDIRECT_GUARD_READY.store(false, Ordering::Release);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VdsoIndirectGuardError {
    GuardRangesUnpublished,
    PkruSupportUnpublished,
    UnreadableTarget,
    ProtectionKeyDenied,
    RewrittenInterior,
}

fn detect_vdso_sgx_pkru_support() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::__cpuid;
        use core::arch::x86_64::__cpuid_count;

        // Installation invokes this before CPUID faulting can be enabled.
        // CPUID is architectural in x86-64 mode.
        let maximum_leaf = __cpuid(0).eax;
        if maximum_leaf < 7 {
            return false;
        }
        let features = __cpuid_count(7, 0).ecx;
        const PKU_AND_OSPKE: u32 = (1 << 3) | (1 << 4);
        features & PKU_AND_OSPKE == PKU_AND_OSPKE
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

fn current_vdso_sgx_pkru() -> Result<Option<u32>, VdsoIndirectGuardError> {
    #[cfg(test)]
    {
        let injected = VDSO_PKRU_TEST_OVERRIDE.load(Ordering::Acquire);
        if injected != u64::MAX {
            return Ok(Some(injected as u32));
        }
    }
    #[cfg(target_arch = "x86_64")]
    {
        match VDSO_PKRU_SUPPORT.load(Ordering::Acquire) {
            1 => return Ok(None),
            2 => {}
            _ => return Err(VdsoIndirectGuardError::PkruSupportUnpublished),
        }
        let pkru: u32;
        // SAFETY: installation stored state 2 only after CPUID advertised PKU
        // and OSPKE. Reviewed LiteInst2 4dab7f0 saves PKRU with XSAVE, does not
        // alter it before this callback, and restores it with XRSTOR afterward;
        // RDPKRU therefore observes the interrupted application's guest value.
        unsafe {
            core::arch::asm!(
                "rdpkru",
                in("ecx") 0_u32,
                out("eax") pkru,
                out("edx") _,
                options(nomem, nostack, preserves_flags),
            );
        }
        Ok(Some(pkru))
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        match VDSO_PKRU_SUPPORT.load(Ordering::Acquire) {
            1 => Ok(None),
            _ => Err(VdsoIndirectGuardError::PkruSupportUnpublished),
        }
    }
}

fn active_vdso_indirect_guard_ranges() -> Result<VdsoIndirectGuardRanges, VdsoIndirectGuardError> {
    if !VDSO_INDIRECT_GUARD_READY.load(Ordering::Acquire) {
        return Err(VdsoIndirectGuardError::GuardRangesUnpublished);
    }
    let normal = VDSO_NORMAL_REWRITE_START.load(Ordering::Relaxed);
    let fallback = VDSO_FALLBACK_REWRITE_START.load(Ordering::Relaxed);
    let guard = VDSO_INDIRECT_GUARD_START.load(Ordering::Relaxed);
    let guard_end = VDSO_INDIRECT_GUARD_END.load(Ordering::Relaxed);
    let normal_end = normal
        .checked_add(5)
        .ok_or(VdsoIndirectGuardError::GuardRangesUnpublished)?;
    let fallback_end = fallback
        .checked_add(5)
        .ok_or(VdsoIndirectGuardError::GuardRangesUnpublished)?;
    Ok(VdsoIndirectGuardRanges {
        normal: VdsoRewrittenRange {
            start: normal,
            end: normal_end,
        },
        fallback: VdsoRewrittenRange {
            start: fallback,
            end: fallback_end,
        },
        guard: VdsoRewrittenRange {
            start: guard,
            end: guard_end,
        },
    })
}

fn read_vdso_sgx_user_handler(run: u64) -> Result<u64, VdsoIndirectGuardError> {
    // process_vm_readv does not honor the caller's PKRU. Without a supported
    // page-to-pkey query, conservatively refuse whenever any protection-key
    // restriction is active. This rejects write-only restrictions and keys
    // unrelated to `run`, but it prevents a kernel copy from turning a
    // PKRU-faulting MOV into a call.
    if current_vdso_sgx_pkru()?.is_some_and(|pkru| pkru != 0) {
        return Err(VdsoIndirectGuardError::ProtectionKeyDenied);
    }
    let mut target = 0_u64;
    let local = libc::iovec {
        iov_base: (&raw mut target).cast(),
        iov_len: core::mem::size_of::<u64>(),
    };
    let remote = libc::iovec {
        iov_base: run.wrapping_add(0x18) as usize as *mut libc::c_void,
        iov_len: core::mem::size_of::<u64>(),
    };
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    // This is the one target-field read. `raw_syscall6` is the reviewed
    // reverie_preload_trusted_syscall gate, whose instruction is excluded from
    // recursive interception. A short read or EFAULT is fail-closed below;
    // it is deliberately not claimed to reproduce the original load's SIGSEGV.
    // Equivalence is likewise limited to a readable handler field that remains
    // stable from the PKRU observation through this read.
    // Concurrent mutation or signal interruption can choose a different
    // observation point than the original single MOV. A concurrent PKRU change
    // is likewise outside the qualified semantic domain.
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
    if read != core::mem::size_of::<u64>() as i64 {
        return Err(VdsoIndirectGuardError::UnreadableTarget);
    }
    Ok(target)
}

fn prepare_vdso_sgx_guard_context_with_ranges(
    context: &mut HookContext,
    ranges: VdsoIndirectGuardRanges,
    read_target: impl FnOnce(u64) -> Result<u64, VdsoIndirectGuardError>,
) -> Result<(), VdsoIndirectGuardError> {
    let target = read_target(context.rax)?;
    if ranges.rejects(target) {
        return Err(VdsoIndirectGuardError::RewrittenInterior);
    }
    // The replace-first trampoline omits only `mov rax,[rax+0x18]`, restores
    // this admitted value, executes the relocated LFENCE, and resumes at the
    // untouched original `call *rax`. No other application field is changed.
    context.rax = target;
    Ok(())
}

unsafe extern "C" fn vdso_sgx_indirect_guard(context: *mut HookContext) {
    let Some(context) = (unsafe { context.as_mut() }) else {
        terminal_vdso_publication_failure();
    };
    let result = active_vdso_indirect_guard_ranges().and_then(|ranges| {
        prepare_vdso_sgx_guard_context_with_ranges(context, ranges, read_vdso_sgx_user_handler)
    });
    if let Err(error) = result {
        #[cfg(test)]
        emit_vdso_terminal_test_marker(match error {
            VdsoIndirectGuardError::UnreadableTarget => VDSO_GUARD_LOAD_TERMINAL_MARKER,
            VdsoIndirectGuardError::ProtectionKeyDenied => VDSO_GUARD_PKRU_TERMINAL_MARKER,
            VdsoIndirectGuardError::GuardRangesUnpublished => {
                VDSO_GUARD_RANGES_UNPUBLISHED_TERMINAL_MARKER
            }
            VdsoIndirectGuardError::PkruSupportUnpublished => {
                VDSO_GUARD_PKRU_SUPPORT_UNPUBLISHED_TERMINAL_MARKER
            }
            VdsoIndirectGuardError::RewrittenInterior => VDSO_GUARD_TARGET_TERMINAL_MARKER,
        });
        #[cfg(not(test))]
        let _ = error;
        terminal_vdso_publication_failure();
    }
}

unsafe fn publish_exact_vdso_instruction(
    address: u64,
    expected: &[u8; 5],
    replacement: &[u8; 5],
    description: &str,
) -> io::Result<()> {
    let current = unsafe { core::slice::from_raw_parts(address as usize as *const u8, 5) };
    if current != expected {
        return Err(io::Error::other(format!(
            "LiteInst vDSO {description} changed before publication"
        )));
    }
    unsafe {
        core::ptr::copy_nonoverlapping(replacement.as_ptr(), address as usize as *mut u8, 5);
    }
    let current = unsafe { core::slice::from_raw_parts(address as usize as *const u8, 5) };
    if current != replacement {
        // The pre-write ownership check succeeded and this transaction issued
        // the only write. A different post-write value cannot be rolled back
        // without overwriting an unowned mutation.
        terminal_vdso_publication_failure();
    }
    Ok(())
}

unsafe fn restore_vdso_original_bytes(
    site_info: &reverie_ptrace::VdsoSyscallSite,
    original_hook_bytes: &[u8; liteinst2::patcher::WORD_PATCH_BYTES],
) -> io::Result<()> {
    unsafe {
        core::ptr::copy_nonoverlapping(
            original_hook_bytes.as_ptr(),
            site_info.address as usize as *mut u8,
            original_hook_bytes.len(),
        );
    }
    let (address, expected): (u64, &[u8]) = match &site_info.entry_patch {
        reverie_ptrace::VdsoEntryPatch::BeforeHook {
            address, expected, ..
        } => (*address, expected),
        reverie_ptrace::VdsoEntryPatch::AfterTarget { .. } => {
            return Err(io::Error::other(
                "LiteInst callback adapter cannot use hook-byte rollback",
            ));
        }
    };
    unsafe {
        core::ptr::copy_nonoverlapping(
            expected.as_ptr(),
            address as usize as *mut u8,
            expected.len(),
        );
    }
    if unsafe { read_vdso_hook_window(site_info.address) } != *original_hook_bytes {
        return Err(io::Error::other(
            "LiteInst vDSO rollback did not restore the original hook bytes",
        ));
    }
    let restored =
        unsafe { core::slice::from_raw_parts(address as usize as *const u8, expected.len()) };
    if restored != expected {
        return Err(io::Error::other(
            "LiteInst vDSO rollback did not restore the original entry bytes",
        ));
    }
    Ok(())
}

unsafe fn rollback_active_vdso_hook(installed: &InstalledVdsoHook<'_>) -> io::Result<()> {
    set_vdso_mapping_protection(
        installed.info.mapping_start,
        installed.info.mapping_len,
        libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
    )?;
    let hook = installed.site.hook.load(Ordering::Acquire);
    if hook.is_null() {
        return Err(io::Error::other(
            "LiteInst vDSO rollback lost the installed hook",
        ));
    }
    // SAFETY: `install_site_hook` used quiescent publication, initialization
    // remains quiescent, and this transaction still owns the installed hook.
    let changed = unsafe { (&*hook).deactivate_quiescent() }
        .map_err(|error| io::Error::other(error.to_string()))?;
    if !changed {
        return Err(io::Error::other(
            "LiteInst vDSO rollback found an inactive hook",
        ));
    }
    let restored_hook = unsafe { read_vdso_hook_window(installed.info.address) };
    if restored_hook != installed.bound_hook_bytes {
        return Err(io::Error::other(
            "LiteInst vDSO hook rollback did not restore the bound bytes",
        ));
    }
    unsafe {
        restore_vdso_original_bytes(installed.info, &installed.original_hook_bytes)?;
    }
    set_vdso_mapping_protection(
        installed.info.mapping_start,
        installed.info.mapping_len,
        libc::PROT_READ | libc::PROT_EXEC,
    )?;
    installed
        .site
        .hook
        .compare_exchange(hook, ptr::null_mut(), Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| io::Error::other("LiteInst vDSO rollback lost hook ownership"))?;
    // SAFETY: the successful compare-exchange removed the sole published raw
    // pointer while initialization is quiescent.
    unsafe { drop(Box::from_raw(hook)) };
    reset_vdso_site_for_retry(installed.site);
    Ok(())
}

unsafe fn restore_vdso_adapter_original_bytes(
    site_info: &reverie_ptrace::VdsoSyscallSite,
    redirects: VdsoAdapterRedirects,
) -> io::Result<()> {
    let reverie_ptrace::VdsoEntryPatch::AfterTarget {
        function_address,
        function_expected,
        normal_address,
        normal_expected,
        fallback_address,
        fallback_expected,
        adapter_source_address,
        adapter_source_expected,
        ..
    } = &site_info.entry_patch
    else {
        return Err(io::Error::other(
            "LiteInst callback adapter rollback received an ordinary vDSO site",
        ));
    };
    // Remove reachability in normal, fallback, guard order. Only bytes whose
    // successful publication is recorded below are owned by this transaction.
    if redirects.normal_is_published() {
        unsafe {
            core::ptr::copy_nonoverlapping(
                normal_expected.as_ptr(),
                *normal_address as usize as *mut u8,
                normal_expected.len(),
            );
        }
    }
    if redirects.fallback_is_published() {
        unsafe {
            core::ptr::copy_nonoverlapping(
                fallback_expected.as_ptr(),
                *fallback_address as usize as *mut u8,
                fallback_expected.len(),
            );
        }
    }
    let function = unsafe {
        core::slice::from_raw_parts(
            *function_address as usize as *const u8,
            function_expected.len(),
        )
    };
    let source = unsafe {
        core::slice::from_raw_parts(
            *adapter_source_address as usize as *const u8,
            adapter_source_expected.len(),
        )
    };
    if function != function_expected.as_ref() || source != adapter_source_expected.as_slice() {
        return Err(io::Error::other(
            "LiteInst callback adapter rollback did not restore the complete function",
        ));
    }
    Ok(())
}

unsafe fn rollback_vdso_adapter(
    installed: &InstalledVdsoAdapter<'_>,
    redirects: VdsoAdapterRedirects,
) -> io::Result<()> {
    #[cfg(test)]
    if VDSO_PROTECTION_SECOND_FAILURE_CALL.load(Ordering::Acquire) != 0 {
        emit_vdso_terminal_test_marker(VDSO_ROLLBACK_ATTEMPT_MARKER);
    }
    set_vdso_mapping_protection(
        installed.info.mapping_start,
        installed.info.mapping_len,
        libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
    )?;
    let reverie_ptrace::VdsoEntryPatch::AfterTarget {
        normal_address,
        normal_expected,
        normal_replacement,
        fallback_address,
        fallback_expected,
        indirect_guard_address,
        ..
    } = &installed.info.entry_patch
    else {
        return Err(io::Error::other(
            "LiteInst callback adapter rollback lost its patch description",
        ));
    };
    let current_normal =
        unsafe { core::slice::from_raw_parts(*normal_address as usize as *const u8, 5) };
    let current_fallback =
        unsafe { core::slice::from_raw_parts(*fallback_address as usize as *const u8, 5) };
    let current_guard = unsafe {
        core::slice::from_raw_parts(
            *indirect_guard_address as usize as *const u8,
            liteinst2::patcher::WORD_PATCH_BYTES,
        )
    };
    let owned_normal: &[u8] = if redirects.normal_is_published() {
        normal_replacement
    } else {
        normal_expected
    };
    let owned_fallback: &[u8] = if redirects.fallback_is_published() {
        &installed.fallback_replacement
    } else {
        fallback_expected
    };
    if current_normal != owned_normal
        || current_fallback != owned_fallback
        || current_guard != installed.guard_replacement
        || rel32_jump(
            *indirect_guard_address,
            installed.guard_trampoline.address(),
        )? != installed.guard_replacement[..5]
        || rel32_jump(*fallback_address, installed.adapter_trampoline.address())?
            != installed.fallback_replacement
    {
        return Err(io::Error::other(
            "LiteInst callback adapter rollback lost branch or guard ownership",
        ));
    }
    unsafe { restore_vdso_adapter_original_bytes(installed.info, redirects)? };
    // SAFETY: initialization is quiescent, the vDSO mapping is writable, and
    // the exact replacement word above proves ownership of this guard patch.
    unsafe { installed.guard_patch.revert_quiescent() }
        .map_err(|error| io::Error::other(error.to_string()))?;
    if unsafe { read_vdso_hook_window(*indirect_guard_address) } != installed.guard_original {
        return Err(io::Error::other(
            "LiteInst SGX guard rollback did not restore its original word",
        ));
    }
    unpublish_vdso_indirect_guard_ranges();
    if !unsafe { vdso_entry_matches_original(installed.info) } {
        return Err(io::Error::other(
            "LiteInst callback adapter rollback did not restore the complete mapping",
        ));
    }
    set_vdso_mapping_protection(
        installed.info.mapping_start,
        installed.info.mapping_len,
        libc::PROT_READ | libc::PROT_EXEC,
    )
}

unsafe fn rollback_active_vdso_adapter(installed: &InstalledVdsoAdapter<'_>) -> io::Result<()> {
    unsafe { rollback_vdso_adapter(installed, VdsoAdapterRedirects::Complete) }
}

unsafe fn rollback_active_vdso_site(installed: &InstalledVdsoSite<'_>) -> io::Result<()> {
    match installed {
        InstalledVdsoSite::Hook(installed) => unsafe { rollback_active_vdso_hook(installed) },
        InstalledVdsoSite::Adapter(installed) => unsafe {
            rollback_active_vdso_adapter(installed.as_ref())
        },
    }
}

unsafe fn rollback_inactive_vdso_site(
    site_info: &reverie_ptrace::VdsoSyscallSite,
    site: &'static SiteSlot,
    original_hook_bytes: &[u8; liteinst2::patcher::WORD_PATCH_BYTES],
) -> io::Result<()> {
    if !site.hook.load(Ordering::Acquire).is_null() {
        return Err(io::Error::other(
            "LiteInst vDSO failed installation published a hook",
        ));
    }
    unsafe { restore_vdso_original_bytes(site_info, original_hook_bytes)? };
    set_vdso_mapping_protection(
        site_info.mapping_start,
        site_info.mapping_len,
        libc::PROT_READ | libc::PROT_EXEC,
    )?;
    reset_vdso_site_for_retry(site);
    Ok(())
}

fn install_one_vdso_hook<'a>(
    site_info: &'a reverie_ptrace::VdsoSyscallSite,
    callback: liteinst2::trampoline::HookCallback,
) -> io::Result<InstalledVdsoSite<'a>> {
    if !matches!(
        &site_info.entry_patch,
        reverie_ptrace::VdsoEntryPatch::BeforeHook { .. }
    ) {
        return Err(io::Error::other(
            "LiteInst ordinary vDSO hook received a callback-adapter site",
        ));
    }
    let address = site_info.address;
    let (site, claimed) =
        claim_site(address).ok_or_else(|| io::Error::other("LiteInst vDSO site table is full"))?;
    if !claimed {
        return Err(io::Error::other("LiteInst vDSO site was claimed twice"));
    }
    if !site.hook.load(Ordering::Acquire).is_null() {
        terminal_vdso_publication_failure();
    }
    let original_hook_bytes = unsafe { read_vdso_hook_window(address) };

    if let Err(error) = set_vdso_mapping_protection(
        site_info.mapping_start,
        site_info.mapping_len,
        libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
    ) {
        // Linux mprotect failure is atomic for this single mapped range; no
        // executable byte or mapping permission changed.
        reset_vdso_site_for_retry(site);
        return Err(error);
    }
    if !unsafe { vdso_entry_matches_original(site_info) } {
        if set_vdso_mapping_protection(
            site_info.mapping_start,
            site_info.mapping_len,
            libc::PROT_READ | libc::PROT_EXEC,
        )
        .is_err()
        {
            terminal_vdso_rollback_failure();
        }
        reset_vdso_site_for_retry(site);
        return Err(io::Error::other(
            "LiteInst vDSO entry does not contain the bound bytes",
        ));
    }
    if let Err(error) = unsafe { publish_vdso_before_hook(site_info) } {
        if unsafe { rollback_inactive_vdso_site(site_info, site, &original_hook_bytes) }.is_err() {
            terminal_vdso_rollback_failure();
        }
        return Err(error);
    }
    let bound_hook_bytes = unsafe { read_vdso_hook_window(address) };

    if let Err(error) = unsafe {
        install_site_hook(
            address,
            site,
            callback,
            PatchPublication::Quiescent,
            &[0x0f, 0x05],
            false,
        )
    } {
        if unsafe { rollback_inactive_vdso_site(site_info, site, &original_hook_bytes) }.is_err() {
            terminal_vdso_rollback_failure();
        }
        return Err(io::Error::other(format!(
            "failed to install LiteInst vDSO hook: {error}"
        )));
    }

    let installed = InstalledVdsoSite::Hook(InstalledVdsoHook {
        info: site_info,
        site,
        original_hook_bytes,
        bound_hook_bytes,
    });
    if let Err(error) = set_vdso_mapping_protection(
        site_info.mapping_start,
        site_info.mapping_len,
        libc::PROT_READ | libc::PROT_EXEC,
    ) {
        if unsafe { rollback_active_vdso_site(&installed) }.is_err() {
            terminal_vdso_rollback_failure();
        }
        return Err(error);
    }
    Ok(installed)
}

fn prepare_vdso_getrandom_adapter(
    site_info: &reverie_ptrace::VdsoSyscallSite,
    callback: liteinst2::trampoline::HookCallback,
) -> io::Result<ExecutableTrampoline> {
    let reverie_ptrace::VdsoEntryPatch::AfterTarget {
        normal_address,
        normal_replacement,
        fallback_address,
        adapter_source_address,
        adapter_source_expected,
        ..
    } = &site_info.entry_patch
    else {
        return Err(io::Error::other(
            "LiteInst getrandom adapter received an ordinary vDSO site",
        ));
    };
    if rel32_jump(*normal_address, *fallback_address)? != *normal_replacement {
        return Err(io::Error::other(
            "LiteInst getrandom normal redirect has the wrong bound target",
        ));
    }
    let scanner = InstructionScanner::default();
    let source = &adapter_source_expected[..7];
    let scan = scanner
        .scan_prefix(source, *adapter_source_address, source.len())
        .map_err(|error| io::Error::other(error.to_string()))?;
    let plan = TrampolinePlan::from_scan_replacing_first(&scan, *adapter_source_address, callback)
        .map_err(|error| io::Error::other(error.to_string()))?;
    if !plan.replaces_first()
        || plan.displaced_len() != 7
        || plan.return_address()
            != adapter_source_address
                .checked_add(7)
                .ok_or_else(|| io::Error::other("LiteInst getrandom adapter return overflows"))?
    {
        return Err(io::Error::other(
            "LiteInst getrandom callback adapter did not bind syscall plus tail jump",
        ));
    }
    let arena = arena_for(*fallback_address)
        .filter(|arena| arena.arena.can_reach(*normal_address))
        .ok_or_else(|| io::Error::other("no reachable LiteInst arena for getrandom adapter"))?;
    let trampoline = arena
        .arena
        .allocate(&plan)
        .map_err(|error| io::Error::other(error.to_string()))?;
    rel32_jump(*fallback_address, trampoline.address())?;
    Ok(trampoline)
}

fn prepare_vdso_sgx_indirect_guard(
    site_info: &reverie_ptrace::VdsoSyscallSite,
) -> io::Result<PreparedVdsoIndirectGuard> {
    let reverie_ptrace::VdsoEntryPatch::AfterTarget {
        indirect_guard_address,
        indirect_guard_expected,
        indirect_guard_displaced_len,
        ..
    } = &site_info.entry_patch
    else {
        return Err(io::Error::other(
            "LiteInst SGX guard received an ordinary vDSO site",
        ));
    };
    let scanner = InstructionScanner::default();
    let scan = scanner
        .scan_prefix(
            indirect_guard_expected,
            *indirect_guard_address,
            liteinst2::patcher::WORD_PATCH_BYTES,
        )
        .map_err(|error| io::Error::other(error.to_string()))?;
    let trampoline_plan = TrampolinePlan::from_scan_replacing_first(
        &scan,
        *indirect_guard_address,
        vdso_sgx_indirect_guard,
    )
    .map_err(|error| io::Error::other(error.to_string()))?;
    let displaced_len = usize::from(*indirect_guard_displaced_len);
    if !trampoline_plan.replaces_first()
        || trampoline_plan.displaced_len() != displaced_len
        || trampoline_plan.return_address()
            != indirect_guard_address
                .checked_add(displaced_len as u64)
                .ok_or_else(|| io::Error::other("LiteInst SGX guard return overflows"))?
    {
        return Err(io::Error::other(
            "LiteInst SGX guard did not replace only the target load and relocate LFENCE",
        ));
    }
    let arena = arena_for(*indirect_guard_address)
        .ok_or_else(|| io::Error::other("no reachable LiteInst arena for SGX guard"))?;
    let trampoline = arena
        .arena
        .allocate(&trampoline_plan)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let patch_plan = JumpPatchPlan::from_scan(
        &scanner,
        &scan,
        scan.snapshot(),
        *indirect_guard_address,
        *indirect_guard_address,
        trampoline.address(),
    )
    .map_err(|error| io::Error::other(error.to_string()))?;
    if patch_plan.displaced_len() != displaced_len
        || patch_plan.original_bytes() != indirect_guard_expected[..8]
        || patch_plan.replacement_bytes()[0] != 0xe9
        || patch_plan.replacement_bytes()[7] != indirect_guard_expected[7]
    {
        return Err(io::Error::other(
            "LiteInst SGX guard patch did not preserve the untouched call head",
        ));
    }
    Ok(PreparedVdsoIndirectGuard {
        trampoline,
        patch_plan,
    })
}

fn install_one_vdso_adapter<'a>(
    site_info: &'a reverie_ptrace::VdsoSyscallSite,
    callback: liteinst2::trampoline::HookCallback,
) -> io::Result<InstalledVdsoSite<'a>> {
    let _install_guard = lock_installation()?;
    let _allocation_scope = crate::patch_alloc::enter();
    if !unsafe { vdso_entry_matches_original(site_info) } {
        return Err(io::Error::other(
            "LiteInst getrandom vDSO mapping changed after guarded planning",
        ));
    }
    let adapter_trampoline = prepare_vdso_getrandom_adapter(site_info, callback)?;
    let guard = prepare_vdso_sgx_indirect_guard(site_info)?;
    let guard_original = guard.patch_plan.original_bytes();
    let guard_replacement = guard.patch_plan.replacement_bytes();
    let guard_address = guard.patch_plan.execute_address();
    let fallback_replacement = match &site_info.entry_patch {
        reverie_ptrace::VdsoEntryPatch::AfterTarget {
            fallback_address, ..
        } => rel32_jump(*fallback_address, adapter_trampoline.address())?,
        reverie_ptrace::VdsoEntryPatch::BeforeHook { .. } => unreachable!(),
    };
    let guard_ranges = vdso_indirect_guard_ranges(site_info)?;
    // Allocate the final owner before any guard-range publication, vDSO
    // permission change, or executable-byte write. Initializing this same box
    // after the guard binds cannot invoke an allocator while a live branch
    // needs rollback ownership.
    let mut installed_adapter_storage = Box::<InstalledVdsoAdapter<'a>>::new_uninit();
    VDSO_PKRU_SUPPORT.store(
        if detect_vdso_sgx_pkru_support() { 2 } else { 1 },
        Ordering::Release,
    );
    // Executable targets and reverse-PC descriptions now exist, but no vDSO
    // byte or permission has changed. Revalidate both the exact VMA identity
    // and all 8,192 bound bytes immediately before making the mapping writable.
    validate_vdso_site(site_info)?;
    if !unsafe { vdso_entry_matches_original(site_info) } {
        return Err(io::Error::other(
            "LiteInst getrandom vDSO mapping changed before publication",
        ));
    }
    set_vdso_mapping_protection(
        site_info.mapping_start,
        site_info.mapping_len,
        libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
    )?;
    if !unsafe { vdso_entry_matches_original(site_info) } {
        if set_vdso_mapping_protection(
            site_info.mapping_start,
            site_info.mapping_len,
            libc::PROT_READ | libc::PROT_EXEC,
        )
        .is_err()
        {
            terminal_vdso_rollback_failure();
        }
        return Err(io::Error::other(
            "LiteInst getrandom vDSO mapping changed before target publication",
        ));
    }
    adapter_trampoline.publish_program_counter_mappings();
    guard.trampoline.publish_program_counter_mappings();
    let guard_patch = match unsafe {
        LiveJumpPatch::bind_quiescent(guard.patch_plan, guard_address as usize as *mut u8)
    } {
        Ok(patch) => patch,
        Err(error) => {
            if set_vdso_mapping_protection(
                site_info.mapping_start,
                site_info.mapping_len,
                libc::PROT_READ | libc::PROT_EXEC,
            )
            .is_err()
            {
                terminal_vdso_publication_failure();
            }
            return Err(io::Error::other(error.to_string()));
        }
    };
    // SAFETY: the allocation above is still uniquely owned and uninitialized.
    // A plain in-place write followed by `assume_init` neither allocates nor
    // exposes a partially initialized owner. The completed box owns every
    // rollback resource before guard metadata or executable bytes go live.
    unsafe {
        installed_adapter_storage
            .as_mut_ptr()
            .write(InstalledVdsoAdapter {
                info: site_info,
                adapter_trampoline,
                guard_trampoline: guard.trampoline,
                guard_patch,
                guard_original,
                guard_replacement,
                fallback_replacement,
            });
    }
    let installed_adapter = unsafe { installed_adapter_storage.assume_init() };
    if let Err(error) = publish_vdso_indirect_guard_ranges(guard_ranges) {
        if set_vdso_mapping_protection(
            site_info.mapping_start,
            site_info.mapping_len,
            libc::PROT_READ | libc::PROT_EXEC,
        )
        .is_err()
        {
            terminal_vdso_publication_failure();
        }
        return Err(error);
    }
    // SAFETY: all signals remain blocked, initialization is single-threaded,
    // and the full mapping was revalidated after the final permission change.
    if let Err(error) = unsafe { installed_adapter.guard_patch.apply_quiescent() } {
        unpublish_vdso_indirect_guard_ranges();
        if unsafe { read_vdso_hook_window(guard_address) } != guard_original
            || !unsafe { vdso_entry_matches_original(site_info) }
            || set_vdso_mapping_protection(
                site_info.mapping_start,
                site_info.mapping_len,
                libc::PROT_READ | libc::PROT_EXEC,
            )
            .is_err()
        {
            terminal_vdso_publication_failure();
        }
        return Err(io::Error::other(error.to_string()));
    }
    #[cfg(test)]
    if VDSO_AFTER_GUARD_FAILURE.swap(false, Ordering::AcqRel) {
        if unsafe { rollback_vdso_adapter(&installed_adapter, VdsoAdapterRedirects::None) }.is_err()
        {
            terminal_vdso_rollback_failure();
        }
        return Err(io::Error::other(
            "injected failure after SGX guard publication",
        ));
    }
    #[cfg(test)]
    if VDSO_POST_TARGET_ENTRY_MUTATION.swap(false, Ordering::AcqRel) {
        let reverie_ptrace::VdsoEntryPatch::AfterTarget { normal_address, .. } =
            &site_info.entry_patch
        else {
            unreachable!();
        };
        unsafe { (*normal_address as usize as *mut u8).write(0xcc) };
    }
    let reverie_ptrace::VdsoEntryPatch::AfterTarget {
        normal_address,
        normal_expected,
        normal_replacement,
        fallback_address,
        fallback_expected,
        ..
    } = &site_info.entry_patch
    else {
        unreachable!();
    };
    // Publish the private fallback first. Until the normal head is redirected,
    // no getrandom path can reach it. Each successful step is recorded in the
    // rollback state; a pre-write mismatch leaves that step unowned.
    if let Err(error) = unsafe {
        publish_exact_vdso_instruction(
            *fallback_address,
            fallback_expected,
            &installed_adapter.fallback_replacement,
            "getrandom fallback",
        )
    } {
        if unsafe { rollback_vdso_adapter(&installed_adapter, VdsoAdapterRedirects::None) }.is_err()
        {
            terminal_vdso_rollback_failure();
        }
        return Err(error);
    }
    #[cfg(test)]
    if VDSO_AFTER_FALLBACK_FAILURE.swap(false, Ordering::AcqRel) {
        if unsafe { rollback_vdso_adapter(&installed_adapter, VdsoAdapterRedirects::FallbackOnly) }
            .is_err()
        {
            terminal_vdso_rollback_failure();
        }
        return Err(io::Error::other(
            "injected failure after getrandom fallback publication",
        ));
    }
    if let Err(error) = unsafe {
        publish_exact_vdso_instruction(
            *normal_address,
            normal_expected,
            normal_replacement,
            "getrandom normal redirect",
        )
    } {
        if unsafe { rollback_vdso_adapter(&installed_adapter, VdsoAdapterRedirects::FallbackOnly) }
            .is_err()
        {
            terminal_vdso_redirect_ownership_failure();
        }
        return Err(error);
    }
    let installed = InstalledVdsoSite::Adapter(installed_adapter);
    if let Err(error) = set_vdso_mapping_protection(
        site_info.mapping_start,
        site_info.mapping_len,
        libc::PROT_READ | libc::PROT_EXEC,
    ) {
        if unsafe { rollback_active_vdso_site(&installed) }.is_err() {
            terminal_vdso_rollback_failure();
        }
        return Err(error);
    }
    Ok(installed)
}

fn install_one_vdso_site<'a>(
    site_info: &'a reverie_ptrace::VdsoSyscallSite,
    callback: liteinst2::trampoline::HookCallback,
) -> io::Result<InstalledVdsoSite<'a>> {
    match &site_info.entry_patch {
        reverie_ptrace::VdsoEntryPatch::BeforeHook { .. } => {
            install_one_vdso_hook(site_info, callback)
        }
        reverie_ptrace::VdsoEntryPatch::AfterTarget { .. } => {
            install_one_vdso_adapter(site_info, callback)
        }
    }
}

/// Install each vDSO hook or private callback adapter while initialization
/// is quiescent.
///
/// A getrandom adapter is executable before its fallback and normal branches
/// are published; the original internal syscall is never patched. Ordinary
/// functions are rewritten only immediately before their hook is installed.
/// Every returned error rolls the complete batch back in reverse order, leaving
/// original entry and syscall bytes, RX mapping protection, null hook pointers,
/// and reclaimable `SITE_STALE` slots. Failure to prove any part of rollback
/// terminates the process instead of exposing a partial installation.
fn install_vdso_sites(sites: &[reverie_ptrace::VdsoSyscallSite]) -> io::Result<()> {
    let callbacks = sites
        .iter()
        .map(|site| {
            validate_vdso_site(site)?;
            vdso_callback(site.number)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let mut installed = Vec::with_capacity(sites.len());
    for (site_info, callback) in sites.iter().zip(callbacks) {
        match install_one_vdso_site(site_info, callback.callback) {
            Ok(site) => installed.push(site),
            Err(error) => {
                for prior in installed.iter().rev() {
                    if unsafe { rollback_active_vdso_site(prior) }.is_err() {
                        terminal_vdso_rollback_failure();
                    }
                }
                return Err(error);
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct VdsoCallback {
    callback: liteinst2::trampoline::HookCallback,
    source: DirectHookSource,
}

fn vdso_callback(number: i64) -> io::Result<VdsoCallback> {
    let (callback, source): (liteinst2::trampoline::HookCallback, DirectHookSource) = match number {
        libc::SYS_time => (installed_vdso_time_hook, DirectHookSource::InstalledSite),
        libc::SYS_clock_gettime => (
            installed_vdso_clock_gettime_hook,
            DirectHookSource::InstalledSite,
        ),
        libc::SYS_getcpu => (installed_vdso_getcpu_hook, DirectHookSource::InstalledSite),
        libc::SYS_getrandom => (installed_vdso_getrandom_hook, DirectHookSource::VdsoAdapter),
        libc::SYS_gettimeofday => (
            installed_vdso_gettimeofday_hook,
            DirectHookSource::InstalledSite,
        ),
        libc::SYS_clock_getres => (
            installed_vdso_clock_getres_hook,
            DirectHookSource::InstalledSite,
        ),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported LiteInst vDSO syscall number {number}"),
        ))?,
    };
    Ok(VdsoCallback { callback, source })
}

// TODO-HUMAN-REVIEW(PR-270): Review stopped-tracee patch helper ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn reverie_liteinst_install_site_for_ptrace(address: u64) -> i64 {
    // SAFETY: the ptrace helper is serialized and the controller reads this
    // fixed-size record only after the helper-return trap.
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(HOST_INSTALL_RESULT),
            HostInstallResult::default(),
        );
    }
    if let Some(site) = find_site(address) {
        // The controller calls this helper only after observing the original
        // syscall bytes at the address. If a prior generation is still marked
        // active, its mapping was replaced and the old hook is no longer
        // installed. Transition it to STALE so claim_site installs a new hook
        // rather than returning the prior generation's relocated tail.
        let instruction = unsafe { core::ptr::read_unaligned(address as usize as *const u16) };
        if instruction == 0x050f
            && matches!(
                site.state.load(Ordering::Acquire),
                SITE_ACTIVE | SITE_FALLBACK
            )
        {
            site.state.store(SITE_STALE, Ordering::Release);
        }
    }
    let Some((site, claimed)) = claim_site(address) else {
        return -i64::from(libc::ENOSPC);
    };
    site.trap_count.fetch_add(1, Ordering::Relaxed);
    let mut install_result = None;
    if claimed {
        match unsafe {
            install_site_hook(
                address,
                site,
                host_syscall_hook,
                PatchPublication::Quiescent,
                &[0x0f, 0x05],
                true,
            )
        } {
            Ok(result) => install_result = Some(result),
            Err(_) => site.state.store(SITE_FALLBACK, Ordering::Release),
        }
    }
    while matches!(site.state.load(Ordering::Acquire), 0 | SITE_INSTALLING) {
        core::hint::spin_loop();
    }
    if site.state.load(Ordering::Acquire) == SITE_ACTIVE {
        let result = install_result.or_else(|| {
            let hook = site.hook.load(Ordering::Acquire);
            if hook.is_null() {
                return None;
            }
            let hook = unsafe { &*hook };
            let arena = arena_for(address)?;
            Some(HostInstallResult {
                version: HOST_INSTALL_RESULT_VERSION,
                site_start: address,
                site_len: liteinst2::patcher::WORD_PATCH_BYTES as u64,
                relocated_tail: hook.trampoline().relocated_tail_address(),
                trampoline_start: hook.trampoline().address(),
                trampoline_len: hook.trampoline().allocation_len() as u64,
                arena_writable_start: arena.writable_start,
                arena_writable_len: arena.writable_end - arena.writable_start,
                arena_executable_start: arena.executable_start,
                arena_executable_len: arena.executable_end - arena.executable_start,
                instruction_len: u64::from(site.instruction_len.load(Ordering::Acquire)),
                straddle_prefix: u64::from(site.straddle_prefix.load(Ordering::Acquire)),
                complete: 1,
            })
        });
        if let Some(result) = result {
            // SAFETY: see the reset above. Publishing `complete` is part of the
            // same stopped-helper call and the host validates every field.
            unsafe {
                core::ptr::write_volatile(core::ptr::addr_of_mut!(HOST_INSTALL_RESULT), result);
            }
        }
        result
            .and_then(|result| i64::try_from(result.relocated_tail).ok())
            .unwrap_or(-i64::from(libc::EOVERFLOW))
    } else {
        -i64::from(libc::EOPNOTSUPP)
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
            unsafe { exit_now(126) };
        }
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-133): Review atomic signal-state preparation.
/// Block every signal while retaining the caller's exact mask for a clean
/// pre-activation planning return.
pub(crate) fn block_all_signals_for_install() -> io::Result<SignalInstallGuard> {
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
    Ok(SignalInstallGuard {
        restore_mask: previous_mask,
    })
}

/// Reset inherited handlers only after the public installer has crossed its
/// fail-closed activation boundary. The guard then leaves the installed
/// SIGSYS/SIGSEGV routes unblocked when installation returns.
pub(crate) fn prepare_guest_signal_actions(
    guard: &mut SignalInstallGuard,
    instructions: InstructionSubscriptions,
) -> io::Result<()> {
    let sigsys = 1_u64 << (libc::SIGSYS - 1);
    let sigsegv = if instructions.cpuid || instructions.rdtsc {
        1_u64 << (libc::SIGSEGV - 1)
    } else {
        0
    };
    guard.restore_mask &= !(sigsys | sigsegv);

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
        if action.handler != libc::SIG_DFL as u64 && action.handler != libc::SIG_IGN as u64 {
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
    Ok(())
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-133): Review fault-safe guest signal-action decoding.
pub(crate) fn signal_action_supported(number: i64, args: [u64; 6]) -> bool {
    if number != libc::SYS_rt_sigaction || args[1] == 0 {
        return true;
    }
    if args[0] == libc::SIGSYS as u64
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
        event.result = unsafe { event.forward() };
        observe_mapping_generation(event);
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
struct HostSyscallFrame {
    flags: u64,
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rdi: u64,
    rsi: u64,
    rbp: u64,
    rbx: u64,
    rdx: u64,
    rcx: u64,
    rax: u64,
    rsp: u64,
    rip: u64,
}

impl HostSyscallFrame {
    const FLAGS_OF: u64 = 0x0001;
    const FLAGS_CF: u64 = 0x0100;
    const FLAGS_PF: u64 = 0x0400;
    const FLAGS_AF: u64 = 0x1000;
    const FLAGS_ZF: u64 = 0x4000;
    const FLAGS_SF: u64 = 0x8000;
    const STATUS_RFLAGS: u64 = 0x0001 | 0x0004 | 0x0010 | 0x0040 | 0x0080 | 0x0800;

    fn from_context(context: &HookContext) -> Self {
        Self {
            flags: Self::encode_flags(context.rflags),
            r15: context.r15,
            r14: context.r14,
            r13: context.r13,
            r12: context.r12,
            r11: context.r11,
            r10: context.r10,
            r9: context.r9,
            r8: context.r8,
            rdi: context.rdi,
            rsi: context.rsi,
            rbp: context.rbp,
            rbx: context.rbx,
            rdx: context.rdx,
            rcx: context.rcx,
            rax: context.rax,
            rsp: context.stack_pointer,
            rip: context.instruction_pointer,
        }
    }

    fn copy_to_context(self, context: &mut HookContext, original_rflags: u64) {
        context.r15 = self.r15;
        context.r14 = self.r14;
        context.r13 = self.r13;
        context.r12 = self.r12;
        context.r11 = self.r11;
        context.r10 = self.r10;
        context.r9 = self.r9;
        context.r8 = self.r8;
        context.rdi = self.rdi;
        context.rsi = self.rsi;
        context.rbp = self.rbp;
        context.rbx = self.rbx;
        context.rdx = self.rdx;
        context.rcx = self.rcx;
        context.rax = self.rax;
        context.rflags = (original_rflags & !Self::STATUS_RFLAGS) | Self::decode_flags(self.flags);
    }

    fn encode_flags(flags: u64) -> u64 {
        let mut encoded = 0;
        for (native, e9) in [
            (0x0001, Self::FLAGS_CF),
            (0x0004, Self::FLAGS_PF),
            (0x0010, Self::FLAGS_AF),
            (0x0040, Self::FLAGS_ZF),
            (0x0080, Self::FLAGS_SF),
            (0x0800, Self::FLAGS_OF),
        ] {
            if flags & native != 0 {
                encoded |= e9;
            }
        }
        encoded
    }

    fn decode_flags(flags: u64) -> u64 {
        let mut native = 0;
        for (e9, bit) in [
            (Self::FLAGS_CF, 0x0001),
            (Self::FLAGS_PF, 0x0004),
            (Self::FLAGS_AF, 0x0010),
            (Self::FLAGS_ZF, 0x0040),
            (Self::FLAGS_SF, 0x0080),
            (Self::FLAGS_OF, 0x0800),
        ] {
            if flags & e9 != 0 {
                native |= bit;
            }
        }
        native
    }
}

unsafe extern "C" fn host_syscall_hook(context: *mut HookContext) {
    if context.is_null() {
        unsafe { exit_now(122) };
    }
    let context = unsafe { &mut *context };
    let original_rflags = context.rflags;
    if let Some(site) = find_site(context.instruction_pointer) {
        site.hook_count.fetch_add(1, Ordering::Relaxed);
    }
    let mut frame = HostSyscallFrame::from_context(context);
    // SAFETY: the host validates the configured marker, exact trap/caller RIPs,
    // readable frame, stack relationship, and current patched-site provenance
    // before dispatch. These checks resist accidental collisions; same-process
    // arbitrary code remains outside the threat model.
    unsafe { reverie_liteinst_host_syscall_trap_call(&mut frame) };
    frame.copy_to_context(context, original_rflags);
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
    match bytes {
        [0x0f, 0xa2, ..] => Some((InstructionEventKind::Cpuid, &[0x0f, 0xa2])),
        [0x0f, 0x31, ..] => Some((InstructionEventKind::Rdtsc, &[0x0f, 0x31])),
        [0x0f, 0x01, 0xf9] => Some((InstructionEventKind::Rdtscp, &[0x0f, 0x01, 0xf9])),
        _ => None,
    }
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

unsafe fn set_all_instruction_native(enabled: bool) -> io::Result<()> {
    if cpuid_interception_enabled() {
        unsafe { set_instruction_native(InstructionEventKind::Cpuid, enabled) }?;
    }
    if rdtsc_interception_enabled() {
        unsafe { set_instruction_native(InstructionEventKind::Rdtsc, enabled) }?;
    }
    Ok(())
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

unsafe extern "C" fn instruction_sigsegv_handler(
    signal: libc::c_int,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
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
        if unsafe { set_instruction_native(kind, true) }.is_err() {
            emit_in_guest_stage(b"instruction-sigsegv-enable-native-failed");
            unsafe { deliver_default_sigsegv() };
        }
        unsafe { execute_native_fault_instruction(kind, context, expected.len()) };
        if unsafe { set_instruction_native(kind, false) }.is_err() {
            emit_in_guest_stage(b"instruction-sigsegv-disable-native-failed");
            unsafe { deliver_default_sigsegv() };
        }
        return;
    }

    let Some((site, claimed)) = claim_site(address) else {
        emit_in_guest_stage(b"instruction-sigsegv-site-table-full");
        unsafe { deliver_default_sigsegv() };
    };
    site.trap_count.fetch_add(1, Ordering::Relaxed);
    if unsafe { set_all_instruction_native(true) }.is_err() {
        emit_in_guest_stage(b"instruction-sigsegv-enable-native-failed");
        unsafe { deliver_default_sigsegv() };
    }
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
    if unsafe { set_all_instruction_native(false) }.is_err() {
        emit_in_guest_stage(b"instruction-sigsegv-disable-native-failed");
        unsafe { deliver_default_sigsegv() };
    }
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
}

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

unsafe fn set_instruction_native(kind: InstructionEventKind, enabled: bool) -> io::Result<()> {
    let result = match kind {
        InstructionEventKind::Cpuid => unsafe {
            const ARCH_SET_CPUID: u64 = 0x1012;
            raw_syscall6(
                libc::SYS_arch_prctl,
                [ARCH_SET_CPUID, u64::from(enabled), 0, 0, 0, 0],
            )
        },
        InstructionEventKind::Rdtsc | InstructionEventKind::Rdtscp => unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [
                    libc::PR_SET_TSC as u64,
                    if enabled {
                        libc::PR_TSC_ENABLE as u64
                    } else {
                        libc::PR_TSC_SIGSEGV as u64
                    },
                    0,
                    0,
                    0,
                    0,
                ],
            )
        },
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error((-result) as i32))
    }
}

unsafe fn installed_instruction_hook(context: *mut HookContext, kind: InstructionEventKind) {
    if context.is_null() || enter_rcb_handler().is_err() {
        unsafe { exit_now(122) };
    }
    let context = unsafe { &mut *context };
    if let Some(site) = find_site(context.instruction_pointer) {
        site.hook_count.fetch_add(1, Ordering::Relaxed);
    }
    if unsafe { set_instruction_native(kind, true) }.is_err() {
        unsafe { exit_now(122) };
    }
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
        if unsafe { set_instruction_native(kind, false) }.is_err() || leave_rcb_handler().is_err() {
            unsafe { exit_now(122) };
        }
        return;
    }
    {
        let _tool_callback = ToolCallbackGuard::enter();
        crate::tool_host::dispatch_instruction(kind, context);
    }
    if unsafe { set_instruction_native(kind, false) }.is_err() || leave_rcb_handler().is_err() {
        unsafe { exit_now(122) };
    }
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

unsafe extern "C" fn installed_cpuid_hook(context: *mut HookContext) {
    unsafe { installed_instruction_hook(context, InstructionEventKind::Cpuid) }
}

unsafe extern "C" fn installed_rdtsc_hook(context: *mut HookContext) {
    unsafe { installed_instruction_hook(context, InstructionEventKind::Rdtsc) }
}

unsafe extern "C" fn installed_rdtscp_hook(context: *mut HookContext) {
    unsafe { installed_instruction_hook(context, InstructionEventKind::Rdtscp) }
}

unsafe fn installed_syscall_hook_for(
    context: *mut HookContext,
    number: Option<i64>,
    source: DirectHookSource,
) {
    if let Some(context) = unsafe { context.as_ref() } {
        let site = match source {
            DirectHookSource::InstalledSite => find_site(context.instruction_pointer),
            DirectHookSource::VdsoAdapter => None,
        };
        record_direct_hook(&DIRECT_HOOK_COUNTERS, source, site);
    }
    unsafe {
        dispatch_syscall_context(
            context,
            number,
            SyscallDispatch::InstalledHook(source),
            None,
        )
    };
}

unsafe fn installed_vdso_syscall_hook_for(context: *mut HookContext, number: i64) {
    let source = vdso_callback(number)
        .map(|callback| callback.source)
        .unwrap_or_else(|_| unsafe { exit_now(122) });
    unsafe { installed_syscall_hook_for(context, Some(number), source) }
}

pub(crate) unsafe fn dispatch_fallback_context(context: *mut HookContext, pkru: &mut Option<u32>) {
    unsafe { dispatch_syscall_context(context, None, SyscallDispatch::Fallback, Some(pkru)) };
}

unsafe fn dispatch_syscall_context(
    context: *mut HookContext,
    number: Option<i64>,
    dispatch: SyscallDispatch,
    guest_pkru: Option<&mut Option<u32>>,
) {
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
        dispatch,
        // Fallback supplies its owned genuine entry. Installed hooks still
        // need independent provenance; never borrow a stale signal's rights.
        guest_pkru: guest_pkru.as_ref().and_then(|value| **value),
    };
    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(PR-133): Review guarded installed-hook bypass for Tool-internal syscalls.
    if tool_callback_active() {
        forward_nested_tool_syscall(&mut event);
        context.rax = event.result as u64;
        context.rcx = context.instruction_pointer.saturating_add(2);
        context.r11 = context.rflags;
        if let Some(output) = guest_pkru {
            *output = event.guest_pkru;
        }
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
    if let Some(output) = guest_pkru {
        *output = event.guest_pkru;
    }
    if leave_rcb_handler().is_err() {
        unsafe { exit_now(122) };
    }
}

unsafe extern "C" fn installed_syscall_hook(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, None, DirectHookSource::InstalledSite) }
}

unsafe extern "C" fn installed_vdso_time_hook(context: *mut HookContext) {
    unsafe { installed_vdso_syscall_hook_for(context, libc::SYS_time) }
}

unsafe extern "C" fn installed_vdso_clock_gettime_hook(context: *mut HookContext) {
    unsafe { installed_vdso_syscall_hook_for(context, libc::SYS_clock_gettime) }
}

unsafe extern "C" fn installed_vdso_getcpu_hook(context: *mut HookContext) {
    unsafe { installed_vdso_syscall_hook_for(context, libc::SYS_getcpu) }
}

unsafe extern "C" fn installed_vdso_getrandom_hook(context: *mut HookContext) {
    unsafe { installed_vdso_syscall_hook_for(context, libc::SYS_getrandom) }
}

unsafe extern "C" fn installed_vdso_gettimeofday_hook(context: *mut HookContext) {
    unsafe { installed_vdso_syscall_hook_for(context, libc::SYS_gettimeofday) }
}

unsafe extern "C" fn installed_vdso_clock_getres_hook(context: *mut HookContext) {
    unsafe { installed_vdso_syscall_hook_for(context, libc::SYS_clock_getres) }
}

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
    fn refuse_fallback(&self, event: &mut PreloadSyscallEvent) {
        FALLBACK_REFUSALS.record(event.number());
        self.stats
            .record_path(crate::LiteinstDispatchPath::FallbackRefusal);
        event.fail(libc::EOPNOTSUPP);
    }

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
        crate::LiteinstDispatchPath::CachelineStraddlerFallback
    } else {
        crate::LiteinstDispatchPath::UnpatchableOrOtherFallback
    });
}

impl SyscallDispatcher for LiteinstDispatcher {
    fn dispatch(&self, event: &mut PreloadSyscallEvent) {
        self.dispatch_with_frame(event, None);
    }

    fn dispatch_private_signal(
        &self,
        frame: &mut reverie_preload::trap::frame::SignalFrame<'_>,
    ) -> bool {
        self.stats
            .record_path(crate::LiteinstDispatchPath::InGuestPhysicalSigsys);
        match crate::syscall_fallback::complete(frame) {
            Ok(true) => {
                self.stats
                    .record_path(crate::LiteinstDispatchPath::FallbackCompletionSigsys);
                true
            }
            Ok(false) => false,
            Err(_) => unsafe { exit_now(126) },
        }
    }

    fn dispatch_signal(
        &self,
        event: &mut PreloadSyscallEvent,
        frame: &mut reverie_preload::trap::frame::SignalFrame<'_>,
    ) {
        self.dispatch_with_frame(event, Some(frame));
    }
}

impl LiteinstDispatcher {
    fn dispatch_with_frame(
        &self,
        event: &mut PreloadSyscallEvent,
        frame: Option<&mut reverie_preload::trap::frame::SignalFrame<'_>>,
    ) {
        if tool_callback_active() {
            crate::syscall_fallback::enable_nested_runtime_access();
            self.stats
                .record_path(crate::LiteinstDispatchPath::InGuestNestedSigsys);
            let mut nested = SyscallEvent {
                number: event.number(),
                args: event.args(),
                instruction_pointer: event.instruction_pointer(),
                result: UNSET_RESULT,
                context: 0,
                dispatch: SyscallDispatch::Trap,
                guest_pkru: event.guest_pkru(),
            };
            forward_nested_tool_syscall(&mut nested);
            event.set_native_result(reverie_preload::trap::NativeSyscallResult {
                result: nested.result,
                pkru: nested.guest_pkru,
            });
            return;
        }
        self.stats
            .record_path(crate::LiteinstDispatchPath::InGuestSigsys);
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
                number: event.number(),
                args,
                instruction_pointer: event.instruction_pointer(),
                result: UNSET_RESULT,
                context: 0,
                dispatch: SyscallDispatch::Trap,
                guest_pkru: event.guest_pkru(),
            };
            unsafe {
                process_syscall(&mut trapped);
            }
            event.set_native_result(reverie_preload::trap::NativeSyscallResult {
                result: trapped.result,
                pkru: trapped.guest_pkru,
            });
            return;
        }

        let resume_address = event.instruction_pointer();
        let instruction_pointer = unsafe { locate_syscall_site(resume_address) }
            .unwrap_or(resume_address.saturating_sub(2));

        if let Some((site, claimed)) = claim_site(instruction_pointer) {
            site.trap_count.fetch_add(1, Ordering::Relaxed);
            if claimed {
                let native = unsafe { set_all_instruction_native(true) };
                let installed = native.and_then(|()| unsafe {
                    install_site_hook(
                        instruction_pointer,
                        site,
                        installed_syscall_hook,
                        self.publication,
                        &[0x0f, 0x05],
                        true,
                    )
                });
                let restored = unsafe { set_all_instruction_native(false) };
                if restored.is_err() {
                    site.state.store(SITE_FALLBACK, Ordering::Release);
                    record_fallback_dispatch(event.number());
                    self.refuse_fallback(event);
                    return;
                }
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
                    event.defer_to(unsafe { (*hook).trampoline().address() });
                    return;
                }
            }
        }

        // AUTONOMOUS-BOT-IMPLEMENTED
        record_fallback_dispatch(event.number());
        if mode == TOOL_REVERIE
            && let Some(frame) = frame
        {
            match crate::syscall_fallback::prepare_signal(instruction_pointer, frame) {
                Ok(Some(entry)) => {
                    (self.record_fallback_stats)(self.stats, instruction_pointer);
                    event.defer_to(entry);
                    return;
                }
                Ok(None) => {}
                Err(_) => unsafe { exit_now(126) },
            }
        }
        self.refuse_fallback(event);
    }
}

unsafe extern "C" fn tool_trampoline() {
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
    let tool_mode = TOOL_MODE.load(Ordering::Relaxed);
    // AUTONOMOUS-BOT-IMPLEMENTED
    if matches!(event.number, libc::SYS_execve | libc::SYS_execveat) {
        event.result = -i64::from(libc::ENOTSUP);
        if tool_mode != TOOL_REVERIE {
            unsafe {
                trace_event(event, Some(event.result));
            }
        }
        return;
    }
    if tool_mode == TOOL_REVERIE && protect_runtime_control(event) {
        return;
    }
    if tool_mode == TOOL_REVERIE && unsafe { protect_coordinator_channel(event) } {
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
            trace_event(event, Some(event.result));
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
            trace_event(event, Some(event.result));
        }
        return;
    }

    if event.number == libc::SYS_exit || event.number == libc::SYS_exit_group {
        unsafe {
            trace_event(event, None);
        }
    }

    let compatibility_fork = TOOL_MODE.load(Ordering::Relaxed) == TOOL_COMPAT
        && matches!(event.number, libc::SYS_clone | libc::SYS_fork);
    if compatibility_fork {
        unsafe {
            trace_event(event, None);
        }
    }
    event.result = unsafe { event.forward() };
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
            trace_event(event, Some(event.result));
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
    let fd = COORDINATOR_FD.load(Ordering::Acquire);
    if fd < 0 {
        return false;
    }
    let fd = fd as u64;
    if event.number == libc::SYS_close && event.args[0] == fd {
        event.result = 0;
    } else if event.number == libc::SYS_close_range && event.args[0] <= fd && fd <= event.args[1] {
        event.result = unsafe { close_range_preserving_event_fd(event, fd) };
    } else if syscall_targets_event_fd(event, fd) {
        event.result = -i64::from(libc::EBADF);
    } else {
        return false;
    }
    true
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
        trace_event(event, Some(event.result));
    }
    true
}

unsafe fn close_range_preserving_event_fd(event: &SyscallEvent, event_fd: u64) -> i64 {
    const CLOSE_RANGE_UNSHARE: u64 = 1 << 1;
    const CLOSE_RANGE_CLOEXEC: u64 = 1 << 2;

    let first = event.args[0];
    let last = event.args[1];
    let mut flags = event.args[2];
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

    if first < event_fd {
        let result =
            unsafe { raw_syscall6(libc::SYS_close_range, [first, event_fd - 1, flags, 0, 0, 0]) };
        if result < 0 {
            return result;
        }
    }
    if event_fd < last {
        let result =
            unsafe { raw_syscall6(libc::SYS_close_range, [event_fd + 1, last, flags, 0, 0, 0]) };
        if result < 0 {
            return result;
        }
    }
    0
}

fn syscall_targets_event_fd(event: &SyscallEvent, event_fd: u64) -> bool {
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
        | libc::SYS_sendfile
        | libc::SYS_fcntl
        | libc::SYS_ioctl
        | libc::SYS_dup => event.args[0] == event_fd,
        libc::SYS_dup2 | libc::SYS_dup3 => event.args[0] == event_fd || event.args[1] == event_fd,
        libc::SYS_splice | libc::SYS_copy_file_range => {
            event.args[0] == event_fd || event.args[2] == event_fd
        }
        libc::SYS_tee => event.args[0] == event_fd || event.args[1] == event_fd,
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

unsafe fn trace_event(event: &SyscallEvent, result: Option<i64>) {
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
    } else if mode == TOOL_STRACE {
        output_fd = libc::STDERR_FILENO;
        let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
        line.push_bytes(b"[liteinst strace pid ");
        line.push_signed(pid);
        line.push_bytes(b"] syscall(");
        line.push_signed(event.number);
        line.push_bytes(b", ip=0x");
        line.push_hex(event.instruction_pointer);
        line.push_bytes(b") = ");
        match result {
            Some(result) => line.push_signed(result),
            None => line.push_bytes(b"?"),
        }
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

unsafe fn exit_now(code: i32) -> ! {
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
    use core::sync::atomic::Ordering;
    use std::ffi::OsStr;

    use reverie_preload::BuiltinTool;

    use super::ALT_STACK_ENV;
    use super::DIRECT_HOOK_COUNTERS;
    use super::DirectHookCounters;
    use super::DirectHookSource;
    use super::ENABLED_FALLBACK_CLASSIFICATIONS;
    use super::FORK_HOOK;
    use super::FallbackCounters;
    use super::HookContext;
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
    use super::SyscallDispatch;
    use super::SyscallEvent;
    use super::TOOL_PASSTHROUGH;
    use super::TOOL_SPOOF_GETPID;
    use super::alt_stack_from_env_value;
    use super::builtin_tool_from_env_value;
    use super::claim_site;
    use super::clone_is_fork_like;
    use super::direct_hook_count;
    use super::fallback_dispatch_count;
    use super::fallback_syscall_count;
    use super::initialize_rcb_clock_with;
    use super::mark_site_range_stale;
    use super::raw_syscall6;
    use super::record_direct_hook;
    use super::record_fallback_dispatch;
    use super::reset_and_record_fork_child_dispatch;
    use super::reset_fallback_observability;
    use super::reset_site_observability;
    use super::vdso_callback;

    const VDSO_TRANSACTION_CHILD_ENV: &str = "REVERIE_LITEINST_VDSO_TRANSACTION_TEST_CHILD";
    const VDSO_TERMINAL_CHILD_ENV: &str = "REVERIE_LITEINST_VDSO_TERMINAL_TEST_CHILD";
    const FORK_ACCOUNTING_CHILD_ENV: &str = "REVERIE_LITEINST_FORK_ACCOUNTING_TEST_CHILD";
    const VDSO_TEST_ENTRY: [u8; 5] = [0x90; 5];
    const VDSO_TEST_REDIRECT: [u8; 5] = [0xe9, 0x06, 0x00, 0x00, 0x00];
    const VDSO_TEST_SYSCALL_OFFSET: usize = 16;
    const VDSO_TEST_FUNCTION_LEN: usize = 32;
    const VDSO_TEST_GUARD_OFFSET: usize = 40;
    const _: () = assert!(
        VDSO_TEST_FUNCTION_LEN <= VDSO_TEST_GUARD_OFFSET,
        "synthetic getrandom function and SGX guard must be disjoint"
    );
    const VDSO_TEST_GUARD_SOURCE: [u8; 9] = [0x48, 0x8b, 0x40, 0x18, 0x0f, 0xae, 0xe8, 0xff, 0xd0];
    const VDSO_TEST_ORDINARY_OFFSET: usize = 64;
    const VDSO_TEST_ORDINARY_LEN: usize = 32;
    const ALL_VDSO_TERMINAL_MARKERS: [&[u8]; 12] = [
        super::VDSO_ROLLBACK_INITIAL_FAILURE_MARKER,
        super::VDSO_ROLLBACK_ATTEMPT_MARKER,
        super::VDSO_ROLLBACK_TERMINAL_MARKER,
        super::VDSO_REDIRECT_OWNERSHIP_TERMINAL_MARKER,
        super::VDSO_PRELOAD_TERMINAL_MARKER,
        super::VDSO_FAULTING_TERMINAL_MARKER,
        super::VDSO_GUARD_LOAD_TERMINAL_MARKER,
        super::VDSO_GUARD_RANGES_UNPUBLISHED_TERMINAL_MARKER,
        super::VDSO_GUARD_PKRU_SUPPORT_UNPUBLISHED_TERMINAL_MARKER,
        super::VDSO_GUARD_TARGET_TERMINAL_MARKER,
        super::VDSO_GUARD_PKRU_TERMINAL_MARKER,
        super::VDSO_BATCH_COMPLETE_MARKER,
    ];

    fn assert_vdso_terminal_markers(stderr: &[u8], expected: &[&[u8]], scenario: &str) {
        for marker in ALL_VDSO_TERMINAL_MARKERS {
            let count = stderr
                .windows(marker.len())
                .filter(|candidate| *candidate == marker)
                .count();
            assert_eq!(
                count,
                usize::from(expected.contains(&marker)),
                "vDSO terminal scenario {scenario} emitted marker {:?} {count} times; stderr:\n{}",
                String::from_utf8_lossy(marker),
                String::from_utf8_lossy(stderr),
            );
        }
    }

    fn protect_vdso_test_mapping(start: u64, len: usize, protection: i32) {
        let result =
            unsafe { libc::mprotect(start as usize as *mut libc::c_void, len, protection) };
        assert_eq!(
            result,
            0,
            "test mapping mprotect failed: {}",
            std::io::Error::last_os_error()
        );
    }

    fn vdso_test_image(start: u64) -> Vec<u8> {
        unsafe { core::slice::from_raw_parts(start as usize as *const u8, 128) }.to_vec()
    }

    fn assert_vdso_test_mapping_is_rx(address: u64) {
        let mapping = super::read_runtime_maps()
            .unwrap()
            .into_iter()
            .find(|mapping| mapping.start <= address && address < mapping.end)
            .expect("synthetic vDSO mapping must remain mapped");
        assert!(mapping.readable);
        assert!(!mapping.writable);
        assert!(mapping.executable);
    }

    fn assert_vdso_test_guard_is_active(start: u64, original: &[u8; 128]) {
        let image = vdso_test_image(start);
        assert_ne!(
            &image[VDSO_TEST_GUARD_OFFSET..VDSO_TEST_GUARD_OFFSET + 7],
            &original[VDSO_TEST_GUARD_OFFSET..VDSO_TEST_GUARD_OFFSET + 7],
        );
        assert_eq!(
            &image[VDSO_TEST_GUARD_OFFSET + 7..VDSO_TEST_GUARD_OFFSET + 9],
            &[0xff, 0xd0],
            "the SGX indirect call must remain at its original address",
        );
    }

    fn vdso_test_original() -> [u8; 128] {
        let mut original = [0x90_u8; 128];
        // The normal redirect lands at +11. The callback adapter is built from
        // the untouched syscall at +16 and its jump to the return at +32.
        original[11..16].copy_from_slice(&[0xb8, 0x3e, 0x01, 0x00, 0x00]);
        original[VDSO_TEST_SYSCALL_OFFSET..VDSO_TEST_SYSCALL_OFFSET + 2]
            .copy_from_slice(&[0x0f, 0x05]);
        original[VDSO_TEST_SYSCALL_OFFSET + 2..VDSO_TEST_SYSCALL_OFFSET + 7]
            .copy_from_slice(&[0xe9, 0x09, 0x00, 0x00, 0x00]);
        original[32] = 0xc3;
        original[VDSO_TEST_GUARD_OFFSET..VDSO_TEST_GUARD_OFFSET + VDSO_TEST_GUARD_SOURCE.len()]
            .copy_from_slice(&VDSO_TEST_GUARD_SOURCE);
        for (index, byte) in original
            [VDSO_TEST_ORDINARY_OFFSET..VDSO_TEST_ORDINARY_OFFSET + VDSO_TEST_ORDINARY_LEN]
            .iter_mut()
            .enumerate()
        {
            *byte = 0x40 + index as u8;
        }
        original
    }

    fn vdso_test_mapping_identity(
        start: u64,
        page_len: usize,
    ) -> reverie_ptrace::VdsoMappingIdentity {
        let mapping = super::read_runtime_maps()
            .unwrap()
            .into_iter()
            .find(|mapping| mapping.start == start && mapping.end == start + page_len as u64)
            .expect("synthetic vDSO must have one exact mapping");
        reverie_ptrace::VdsoMappingIdentity {
            offset: mapping.offset,
            device: mapping.device,
            inode: mapping.inode,
            pathname: mapping.pathname.into_boxed_str(),
            shared: mapping.shared,
        }
    }

    fn vdso_test_getrandom_site(
        start: u64,
        page_len: usize,
        original: &[u8; 128],
    ) -> reverie_ptrace::VdsoSyscallSite {
        let mut mapping_expected = vec![0; page_len];
        mapping_expected[..original.len()].copy_from_slice(original);
        reverie_ptrace::VdsoSyscallSite {
            address: start + VDSO_TEST_SYSCALL_OFFSET as u64,
            number: libc::SYS_getrandom,
            mapping_start: start,
            mapping_len: page_len as u64,
            mapping_identity: vdso_test_mapping_identity(start, page_len),
            entry_patch: reverie_ptrace::VdsoEntryPatch::AfterTarget {
                mapping_expected: mapping_expected.into_boxed_slice(),
                function_address: start,
                function_expected: original[..VDSO_TEST_FUNCTION_LEN]
                    .to_vec()
                    .into_boxed_slice(),
                normal_address: start,
                normal_expected: VDSO_TEST_ENTRY,
                normal_replacement: VDSO_TEST_REDIRECT,
                fallback_address: start + 11,
                fallback_expected: original[11..16].try_into().unwrap(),
                adapter_source_address: start + VDSO_TEST_SYSCALL_OFFSET as u64,
                adapter_source_expected: original
                    [VDSO_TEST_SYSCALL_OFFSET..VDSO_TEST_SYSCALL_OFFSET + 8]
                    .try_into()
                    .unwrap(),
                indirect_guard_address: start + VDSO_TEST_GUARD_OFFSET as u64,
                indirect_guard_expected: VDSO_TEST_GUARD_SOURCE,
                indirect_guard_displaced_len: 7,
            },
        }
    }

    fn vdso_test_ordinary_site(
        start: u64,
        page_len: usize,
        original: &[u8; 128],
    ) -> reverie_ptrace::VdsoSyscallSite {
        let mut replacement = vec![0x90; VDSO_TEST_ORDINARY_LEN];
        replacement[..3].copy_from_slice(&[0x0f, 0x05, 0xc3]);
        reverie_ptrace::VdsoSyscallSite {
            address: start + VDSO_TEST_ORDINARY_OFFSET as u64,
            number: libc::SYS_time,
            mapping_start: start,
            mapping_len: page_len as u64,
            mapping_identity: vdso_test_mapping_identity(start, page_len),
            entry_patch: reverie_ptrace::VdsoEntryPatch::BeforeHook {
                address: start + VDSO_TEST_ORDINARY_OFFSET as u64,
                expected: original
                    [VDSO_TEST_ORDINARY_OFFSET..VDSO_TEST_ORDINARY_OFFSET + VDSO_TEST_ORDINARY_LEN]
                    .to_vec()
                    .into_boxed_slice(),
                replacement: replacement.into_boxed_slice(),
            },
        }
    }

    fn run_vdso_transaction_child(scenario: &str) {
        let page_len = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
        let mapping = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                page_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        let start = mapping as usize as u64;
        let original = vdso_test_original();
        unsafe {
            core::ptr::copy_nonoverlapping(original.as_ptr(), mapping.cast::<u8>(), original.len());
        }

        let getrandom_site = vdso_test_getrandom_site(start, page_len, &original);
        let ordinary_site = vdso_test_ordinary_site(start, page_len, &original);

        match scenario {
            "entry-mismatch" => unsafe {
                mapping.cast::<u8>().write(0xcc);
            },
            "adapter-source-mismatch" => unsafe {
                mapping
                    .cast::<u8>()
                    .add(VDSO_TEST_SYSCALL_OFFSET)
                    .write(0xcc);
            },
            "outside-prefix-mismatch" => unsafe {
                mapping.cast::<u8>().add(56).write(0xcc);
            },
            "mapping-replacement" => unsafe {
                let fd = libc::memfd_create(c"reverie-vdso-identity".as_ptr(), libc::MFD_CLOEXEC);
                assert!(
                    fd >= 0,
                    "memfd_create failed: {}",
                    std::io::Error::last_os_error()
                );
                assert_eq!(libc::ftruncate(fd, page_len as libc::off_t), 0);
                assert_eq!(
                    libc::pwrite(fd, original.as_ptr().cast(), original.len(), 0),
                    original.len() as isize
                );
                assert_eq!(libc::munmap(mapping, page_len), 0);
                let replacement = libc::mmap(
                    mapping,
                    page_len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_FIXED,
                    fd,
                    0,
                );
                assert_eq!(libc::close(fd), 0);
                assert_eq!(replacement, mapping);
                assert_eq!(vdso_test_image(start), original);
            },
            "initial-protection"
            | "identity-unchanged-control"
            | "after-guard-error"
            | "after-fallback-error"
            | "post-target-entry-mismatch"
            | "final-protection"
            | "batch-final-protection"
            | "rollback-protection-terminal"
            | "malformed-geometry"
            | "callback-refusal" => {}
            other => panic!("unknown vDSO transaction scenario {other}"),
        }
        let before_attempt = vdso_test_image(start);
        protect_vdso_test_mapping(start, page_len, libc::PROT_READ | libc::PROT_EXEC);

        super::prepare_instrumentation_state().unwrap();
        super::VDSO_PROTECTION_CALLS.store(0, Ordering::Release);
        super::VDSO_PROTECTION_FAILURE_CALL.store(
            match scenario {
                "initial-protection" => 1,
                "final-protection" | "rollback-protection-terminal" => 2,
                // First site uses calls 1 and 2. The ordinary second site
                // publishes its complete rewrite, activates its hook, and
                // then encounters this final RX failure at call 4.
                "batch-final-protection" => 4,
                _ => 0,
            },
            Ordering::Release,
        );
        super::VDSO_PROTECTION_SECOND_FAILURE_CALL.store(
            if scenario == "rollback-protection-terminal" {
                4
            } else {
                0
            },
            Ordering::Release,
        );
        let mut sites = if matches!(scenario, "batch-final-protection" | "callback-refusal") {
            let mut sites = vec![getrandom_site, ordinary_site];
            if scenario == "callback-refusal" {
                sites[1].number = -1;
            }
            sites
        } else {
            vec![getrandom_site]
        };
        if scenario == "malformed-geometry" {
            sites[0].mapping_len = (VDSO_TEST_SYSCALL_OFFSET + 7) as u64;
        }
        if scenario == "post-target-entry-mismatch" {
            super::VDSO_POST_TARGET_ENTRY_MUTATION.store(true, Ordering::Release);
        }
        if scenario == "after-guard-error" {
            super::VDSO_AFTER_GUARD_FAILURE.store(true, Ordering::Release);
        }
        if scenario == "after-fallback-error" {
            super::VDSO_AFTER_FALLBACK_FAILURE.store(true, Ordering::Release);
        }

        if scenario == "identity-unchanged-control" {
            super::install_vdso_sites(&sites).unwrap();
            assert_eq!(vdso_test_image(start)[0], 0xe9);
            assert_eq!(vdso_test_image(start)[11], 0xe9);
            assert_vdso_test_guard_is_active(start, &original);
            assert_eq!(
                &vdso_test_image(start)[VDSO_TEST_SYSCALL_OFFSET..VDSO_TEST_SYSCALL_OFFSET + 8],
                &original[VDSO_TEST_SYSCALL_OFFSET..VDSO_TEST_SYSCALL_OFFSET + 8]
            );
            assert_vdso_test_mapping_is_rx(start);
            return;
        }

        assert!(super::install_vdso_sites(&sites).is_err());
        assert_eq!(vdso_test_image(start), before_attempt);
        assert_vdso_test_mapping_is_rx(start);
        assert!(super::find_site(sites[0].address).is_none());
        if scenario == "callback-refusal" {
            assert!(super::find_site(sites[1].address).is_none());
            sites[1].number = libc::SYS_time;
        }
        if scenario == "batch-final-protection" {
            let ordinary = super::find_site(sites[1].address).unwrap();
            assert_eq!(ordinary.state.load(Ordering::Acquire), SITE_STALE);
            assert!(ordinary.hook.load(Ordering::Acquire).is_null());
            assert_eq!(ordinary.mapping_end.load(Ordering::Acquire), 0);
            assert_eq!(ordinary.instruction_len.load(Ordering::Acquire), 0);
            assert_eq!(ordinary.straddle_prefix.load(Ordering::Acquire), 0);
        }

        if scenario == "mapping-replacement" {
            unsafe {
                assert_eq!(libc::munmap(mapping, page_len), 0);
                let replacement = libc::mmap(
                    mapping,
                    page_len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
                    -1,
                    0,
                );
                assert_eq!(replacement, mapping);
                core::ptr::copy_nonoverlapping(
                    original.as_ptr(),
                    replacement.cast::<u8>(),
                    original.len(),
                );
            }
            protect_vdso_test_mapping(start, page_len, libc::PROT_READ | libc::PROT_EXEC);
            sites[0] = vdso_test_getrandom_site(start, page_len, &original);
        } else if matches!(
            scenario,
            "entry-mismatch" | "adapter-source-mismatch" | "outside-prefix-mismatch"
        ) {
            protect_vdso_test_mapping(
                start,
                page_len,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            );
            unsafe {
                core::ptr::copy_nonoverlapping(
                    original.as_ptr(),
                    mapping.cast::<u8>(),
                    original.len(),
                );
            }
            protect_vdso_test_mapping(start, page_len, libc::PROT_READ | libc::PROT_EXEC);
        }
        if scenario == "malformed-geometry" {
            sites[0].mapping_len = page_len as u64;
        }
        super::VDSO_PROTECTION_CALLS.store(0, Ordering::Release);
        super::VDSO_PROTECTION_FAILURE_CALL.store(0, Ordering::Release);
        super::VDSO_PROTECTION_SECOND_FAILURE_CALL.store(0, Ordering::Release);

        super::install_vdso_sites(&sites).unwrap();
        assert!(super::find_site(sites[0].address).is_none());
        assert_eq!(
            &vdso_test_image(start)[..VDSO_TEST_ENTRY.len()],
            VDSO_TEST_REDIRECT.as_slice()
        );
        assert_eq!(
            &vdso_test_image(start)[VDSO_TEST_SYSCALL_OFFSET..VDSO_TEST_SYSCALL_OFFSET + 8],
            &original[VDSO_TEST_SYSCALL_OFFSET..VDSO_TEST_SYSCALL_OFFSET + 8]
        );
        assert_eq!(vdso_test_image(start)[11], 0xe9);
        assert_vdso_test_guard_is_active(start, &original);
        assert_vdso_test_mapping_is_rx(start);
        if matches!(scenario, "batch-final-protection" | "callback-refusal") {
            let ordinary = super::find_site(sites[1].address).unwrap();
            assert_eq!(ordinary.state.load(Ordering::Acquire), SITE_ACTIVE);
            assert!(!ordinary.hook.load(Ordering::Acquire).is_null());
            assert_ne!(
                &vdso_test_image(start)[VDSO_TEST_ORDINARY_OFFSET..VDSO_TEST_ORDINARY_OFFSET + 2],
                &original[VDSO_TEST_ORDINARY_OFFSET..VDSO_TEST_ORDINARY_OFFSET + 2]
            );
        }
    }

    #[test]
    fn vdso_install_transaction_restores_and_retries() {
        if let Some(scenario) = std::env::var_os(VDSO_TRANSACTION_CHILD_ENV) {
            run_vdso_transaction_child(scenario.to_str().unwrap());
            return;
        }

        for scenario in [
            "initial-protection",
            "identity-unchanged-control",
            "entry-mismatch",
            "adapter-source-mismatch",
            "outside-prefix-mismatch",
            "mapping-replacement",
            "after-guard-error",
            "after-fallback-error",
            "post-target-entry-mismatch",
            "final-protection",
            "batch-final-protection",
            "malformed-geometry",
            "callback-refusal",
            "rollback-protection-terminal",
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime::tests::vdso_install_transaction_restores_and_retries",
                    "--test-threads=1",
                ])
                .env(VDSO_TRANSACTION_CHILD_ENV, scenario)
                .output()
                .unwrap();
            if matches!(
                scenario,
                "post-target-entry-mismatch" | "rollback-protection-terminal"
            ) {
                assert_eq!(
                    output.status.code(),
                    Some(super::VDSO_PUBLICATION_FAILURE_STATUS)
                );
            } else {
                assert!(
                    output.status.success(),
                    "vDSO transaction child {scenario} failed:\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            assert_vdso_terminal_markers(
                &output.stderr,
                match scenario {
                    "post-target-entry-mismatch" => {
                        &[super::VDSO_REDIRECT_OWNERSHIP_TERMINAL_MARKER]
                    }
                    "rollback-protection-terminal" => &[
                        super::VDSO_ROLLBACK_INITIAL_FAILURE_MARKER,
                        super::VDSO_ROLLBACK_ATTEMPT_MARKER,
                        super::VDSO_ROLLBACK_TERMINAL_MARKER,
                    ],
                    _ => &[],
                },
                scenario,
            );
        }
    }

    fn install_real_synthetic_vdso_batch() {
        let page_len = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
        let mapping = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                page_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        let start = mapping as usize as u64;
        let original = vdso_test_original();
        unsafe {
            core::ptr::copy_nonoverlapping(original.as_ptr(), mapping.cast::<u8>(), original.len());
        }
        protect_vdso_test_mapping(start, page_len, libc::PROT_READ | libc::PROT_EXEC);
        super::prepare_instrumentation_state().unwrap();
        let site = vdso_test_getrandom_site(start, page_len, &original);
        super::install_vdso_sites(&[site]).unwrap();
        assert_eq!(vdso_test_image(start)[0], 0xe9);
        assert_eq!(vdso_test_image(start)[11], 0xe9);
        assert_vdso_test_guard_is_active(start, &original);
        assert_eq!(
            &vdso_test_image(start)[VDSO_TEST_SYSCALL_OFFSET..VDSO_TEST_SYSCALL_OFFSET + 8],
            &original[VDSO_TEST_SYSCALL_OFFSET..VDSO_TEST_SYSCALL_OFFSET + 8]
        );
        assert_vdso_test_mapping_is_rx(start);
        super::emit_vdso_terminal_test_marker(super::VDSO_BATCH_COMPLETE_MARKER);
    }

    fn terminal_test_guard_ranges() -> super::VdsoIndirectGuardRanges {
        super::VdsoIndirectGuardRanges {
            normal: super::VdsoRewrittenRange {
                start: 0x1000,
                end: 0x1005,
            },
            fallback: super::VdsoRewrittenRange {
                start: 0x2000,
                end: 0x2005,
            },
            guard: super::VdsoRewrittenRange {
                start: 0x3000,
                end: 0x3007,
            },
        }
    }

    fn hook_context_with_rax(rax: u64) -> HookContext {
        HookContext {
            instruction_pointer: 0x01,
            stack_pointer: 0x02,
            r15: 0x03,
            r14: 0x04,
            r13: 0x05,
            r12: 0x06,
            r11: 0x07,
            r10: 0x08,
            r9: 0x09,
            r8: 0x0a,
            rdi: 0x0b,
            rsi: 0x0c,
            rbp: 0x0d,
            rbx: 0x0e,
            rdx: 0x0f,
            rcx: 0x10,
            rax,
            rflags: 0x12,
        }
    }

    fn run_vdso_terminal_child(scenario: &str) {
        match scenario {
            "preload" => {
                install_real_synthetic_vdso_batch();
                let _ = super::complete_runtime_after_vdso(
                    true,
                    || {
                        super::emit_vdso_terminal_test_marker(super::VDSO_PRELOAD_TERMINAL_MARKER);
                        Err(std::io::Error::other("injected preload failure"))
                    },
                    || unreachable!("fault enabling must not follow preload failure"),
                );
                unreachable!("a preload error after a real vDSO batch must terminate");
            }
            "faulting" => {
                install_real_synthetic_vdso_batch();
                let _ = super::complete_runtime_after_vdso(
                    true,
                    || Ok(()),
                    || {
                        super::emit_vdso_terminal_test_marker(super::VDSO_FAULTING_TERMINAL_MARKER);
                        Err(std::io::Error::other("injected fault-enabling failure"))
                    },
                );
                unreachable!("a fault-enabling error after a real vDSO batch must terminate");
            }
            "no-sites" => {
                assert!(
                    super::complete_runtime_after_vdso(
                        false,
                        || Err(std::io::Error::other("injected no-site preload failure")),
                        || unreachable!("fault enabling must not follow preload failure"),
                    )
                    .is_err()
                );
            }
            "sgx-load-fault" => {
                super::publish_vdso_indirect_guard_ranges(terminal_test_guard_ranges()).unwrap();
                super::VDSO_PKRU_TEST_OVERRIDE.store(0, Ordering::Release);
                let page_len =
                    usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
                let mapping = unsafe {
                    libc::mmap(
                        core::ptr::null_mut(),
                        page_len,
                        libc::PROT_NONE,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        -1,
                        0,
                    )
                };
                assert_ne!(mapping, libc::MAP_FAILED);
                let run = (mapping as usize).wrapping_sub(0x18) as u64;
                let mut context = hook_context_with_rax(run);
                unsafe { super::vdso_sgx_indirect_guard(&mut context) };
                unreachable!("an unreadable SGX target field must terminate");
            }
            "sgx-readable-control" => {
                super::VDSO_PKRU_TEST_OVERRIDE.store(0, Ordering::Release);
                let expected = 0x7777_u64;
                let mut run = [0_u64; 4];
                run[3] = expected;
                assert_eq!(
                    super::read_vdso_sgx_user_handler(run.as_mut_ptr() as usize as u64).unwrap(),
                    expected,
                );
            }
            "sgx-pkru-denied" => {
                super::publish_vdso_indirect_guard_ranges(terminal_test_guard_ranges()).unwrap();
                super::VDSO_PKRU_TEST_OVERRIDE.store(1, Ordering::Release);
                let mut run = [0_u64; 4];
                run[3] = 0x7777;
                let mut context = hook_context_with_rax(run.as_mut_ptr() as usize as u64);
                unsafe { super::vdso_sgx_indirect_guard(&mut context) };
                unreachable!("a read-disabled PKRU state must terminate before the kernel copy");
            }
            "sgx-ranges-unpublished" => {
                assert!(!super::VDSO_INDIRECT_GUARD_READY.load(Ordering::Acquire));
                super::VDSO_PKRU_TEST_OVERRIDE.store(0, Ordering::Release);
                let mut run = [0_u64; 4];
                run[3] = 0x7777;
                let mut context = hook_context_with_rax(run.as_mut_ptr() as usize as u64);
                unsafe { super::vdso_sgx_indirect_guard(&mut context) };
                unreachable!("unpublished guard ranges must terminate at their own source");
            }
            "sgx-pkru-support-unpublished" => {
                super::publish_vdso_indirect_guard_ranges(terminal_test_guard_ranges()).unwrap();
                assert_eq!(super::VDSO_PKRU_SUPPORT.load(Ordering::Acquire), 0);
                assert_eq!(
                    super::VDSO_PKRU_TEST_OVERRIDE.load(Ordering::Acquire),
                    u64::MAX,
                );
                let mut run = [0_u64; 4];
                run[3] = 0x7777;
                let mut context = hook_context_with_rax(run.as_mut_ptr() as usize as u64);
                unsafe { super::vdso_sgx_indirect_guard(&mut context) };
                unreachable!("unpublished PKRU support must terminate at its own source");
            }
            "sgx-target-interior" => {
                let ranges = terminal_test_guard_ranges();
                super::publish_vdso_indirect_guard_ranges(ranges).unwrap();
                super::VDSO_PKRU_TEST_OVERRIDE.store(0, Ordering::Release);
                let mut run = [0_u8; 32];
                run[24..].copy_from_slice(&(ranges.normal.start + 1).to_ne_bytes());
                let mut context = hook_context_with_rax(run.as_mut_ptr() as usize as u64);
                unsafe { super::vdso_sgx_indirect_guard(&mut context) };
                unreachable!("an SGX target inside a rewritten instruction must terminate");
            }
            "sgx-baseline-fault" => {
                let page_len =
                    usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
                let mapping = unsafe {
                    libc::mmap(
                        core::ptr::null_mut(),
                        page_len,
                        libc::PROT_NONE,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        -1,
                        0,
                    )
                };
                assert_ne!(mapping, libc::MAP_FAILED);
                let run = (mapping as usize).wrapping_sub(0x18);
                // Baseline the original `mov rax,[rax+0x18]` fault class. The
                // guarded path intentionally changes this SIGSEGV into the
                // uniquely marked status-124 fail-closed result above. This
                // PROT_NONE case does not qualify protection-key denial, which
                // process_vm_readv may bypass and remains an explicit gap.
                let _ = unsafe { core::ptr::read_volatile(run.wrapping_add(0x18) as *const u64) };
                unreachable!("the baseline unreadable load must fault");
            }
            other => panic!("unknown vDSO terminal scenario {other}"),
        }
    }

    #[test]
    fn real_vdso_batch_and_publication_boundaries_fail_closed() {
        use std::os::unix::process::ExitStatusExt;

        if let Some(scenario) = std::env::var_os(VDSO_TERMINAL_CHILD_ENV) {
            run_vdso_terminal_child(scenario.to_str().unwrap());
            return;
        }
        for scenario in [
            "preload",
            "faulting",
            "no-sites",
            "sgx-load-fault",
            "sgx-readable-control",
            "sgx-pkru-denied",
            "sgx-ranges-unpublished",
            "sgx-pkru-support-unpublished",
            "sgx-target-interior",
            "sgx-baseline-fault",
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime::tests::real_vdso_batch_and_publication_boundaries_fail_closed",
                    "--test-threads=1",
                ])
                .env(VDSO_TERMINAL_CHILD_ENV, scenario)
                .output()
                .unwrap();
            if matches!(scenario, "no-sites" | "sgx-readable-control") {
                assert!(output.status.success());
            } else if scenario == "sgx-baseline-fault" {
                assert_eq!(output.status.signal(), Some(libc::SIGSEGV));
            } else {
                assert_eq!(
                    output.status.code(),
                    Some(super::VDSO_PUBLICATION_FAILURE_STATUS),
                    "terminal scenario {scenario} produced stderr:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let expected_markers: &[&[u8]] = match scenario {
                "preload" => &[
                    super::VDSO_BATCH_COMPLETE_MARKER,
                    super::VDSO_PRELOAD_TERMINAL_MARKER,
                ],
                "faulting" => &[
                    super::VDSO_BATCH_COMPLETE_MARKER,
                    super::VDSO_FAULTING_TERMINAL_MARKER,
                ],
                "sgx-load-fault" => &[super::VDSO_GUARD_LOAD_TERMINAL_MARKER],
                "sgx-pkru-denied" => &[super::VDSO_GUARD_PKRU_TERMINAL_MARKER],
                "sgx-ranges-unpublished" => &[super::VDSO_GUARD_RANGES_UNPUBLISHED_TERMINAL_MARKER],
                "sgx-pkru-support-unpublished" => {
                    &[super::VDSO_GUARD_PKRU_SUPPORT_UNPUBLISHED_TERMINAL_MARKER]
                }
                "sgx-target-interior" => &[super::VDSO_GUARD_TARGET_TERMINAL_MARKER],
                "no-sites" | "sgx-readable-control" | "sgx-baseline-fault" => &[],
                _ => unreachable!(),
            };
            assert_vdso_terminal_markers(&output.stderr, expected_markers, scenario);
        }
    }

    #[test]
    fn sgx_guard_rejects_every_rewritten_interior_and_changes_only_rax() {
        let ranges = terminal_test_guard_ranges();
        for range in [ranges.normal, ranges.fallback, ranges.guard] {
            assert!(!ranges.rejects(range.start));
            assert!(!ranges.rejects(range.end));
            for target in range.start + 1..range.end {
                assert!(
                    ranges.rejects(target),
                    "admitted interior target {target:#x}"
                );
            }
        }
        assert!(!ranges.rejects(ranges.normal.start - 1));
        assert!(!ranges.rejects(ranges.guard.end + 1));

        fn except_rax(context: &HookContext) -> [u64; 17] {
            [
                context.instruction_pointer,
                context.stack_pointer,
                context.r15,
                context.r14,
                context.r13,
                context.r12,
                context.r11,
                context.r10,
                context.r9,
                context.r8,
                context.rdi,
                context.rsi,
                context.rbp,
                context.rbx,
                context.rdx,
                context.rcx,
                context.rflags,
            ]
        }

        let original_run = 0x5555_u64;
        let admitted_target = 0x7777_u64;
        let mut context = hook_context_with_rax(original_run);
        let before = context;
        let reads = std::cell::Cell::new(0);
        super::prepare_vdso_sgx_guard_context_with_ranges(&mut context, ranges, |run| {
            reads.set(reads.get() + 1);
            assert_eq!(run, original_run);
            Ok(admitted_target)
        })
        .unwrap();
        assert_eq!(reads.get(), 1);
        assert_eq!(context.rax, admitted_target);
        assert_eq!(except_rax(&context), except_rax(&before));

        // The admitted semantic domain requires a stable readable field. This
        // causal control mutates the modeled field after its sole observation:
        // the guard forwards the captured value and never performs a second
        // read. It does not claim parity for a genuinely concurrent mutation.
        let first_snapshot = 0x8888_u64;
        let later_value = 0x9999_u64;
        let modeled_field = std::cell::Cell::new(first_snapshot);
        let snapshot_reads = std::cell::Cell::new(0);
        let mut snapshot_context = before;
        super::prepare_vdso_sgx_guard_context_with_ranges(&mut snapshot_context, ranges, |_| {
            snapshot_reads.set(snapshot_reads.get() + 1);
            let observed = modeled_field.get();
            modeled_field.set(later_value);
            Ok(observed)
        })
        .unwrap();
        assert_eq!(snapshot_reads.get(), 1);
        assert_eq!(modeled_field.get(), later_value);
        assert_eq!(snapshot_context.rax, first_snapshot);
        assert_eq!(except_rax(&snapshot_context), except_rax(&before));

        let mut faulting = before;
        let fault_reads = std::cell::Cell::new(0);
        assert_eq!(
            super::prepare_vdso_sgx_guard_context_with_ranges(&mut faulting, ranges, |_| {
                fault_reads.set(fault_reads.get() + 1);
                Err(super::VdsoIndirectGuardError::UnreadableTarget)
            }),
            Err(super::VdsoIndirectGuardError::UnreadableTarget),
        );
        assert_eq!(fault_reads.get(), 1);
        assert_eq!(faulting.rax, before.rax);
        assert_eq!(except_rax(&faulting), except_rax(&before));
    }

    #[test]
    fn vdso_callback_routes_getrandom_and_refuses_unknown_numbers() {
        assert!(vdso_callback(libc::SYS_getrandom).is_ok());
        assert!(vdso_callback(-1).is_err());
    }

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
    fn builtin_tool_selector_maps_shared_values_only() {
        assert_eq!(
            builtin_tool_from_env_value(OsStr::new(TOOL_PASSTHROUGH)),
            Some(BuiltinTool::Passthrough)
        );
        assert_eq!(
            builtin_tool_from_env_value(OsStr::new(TOOL_SPOOF_GETPID)),
            Some(BuiltinTool::SpoofGetpid)
        );
        // LiteInst-native modes and unknown values are not shared built-ins.
        assert_eq!(builtin_tool_from_env_value(OsStr::new("strace")), None);
        assert_eq!(builtin_tool_from_env_value(OsStr::new("compat")), None);
        assert_eq!(builtin_tool_from_env_value(OsStr::new("bogus")), None);
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
    fn adapter_hook_accounting_is_exact_disjoint_and_resettable() {
        let counters = DirectHookCounters::new();
        let site = SiteSlot::new();
        let adapter = vdso_callback(libc::SYS_getrandom).unwrap();
        let ordinary = vdso_callback(libc::SYS_time).unwrap();
        assert_eq!(adapter.source, DirectHookSource::VdsoAdapter);
        assert_eq!(ordinary.source, DirectHookSource::InstalledSite);

        record_direct_hook(&counters, adapter.source, None);
        assert_eq!(counters.adapter_count(), 1);
        assert_eq!(site.hook_count.load(Ordering::Acquire), 0);
        assert_eq!(direct_hook_count(&counters, std::slice::from_ref(&site)), 1);

        // A missing ordinary source site must not alias the adapter counter.
        record_direct_hook(&counters, ordinary.source, None);
        assert_eq!(counters.adapter_count(), 1);
        assert_eq!(direct_hook_count(&counters, std::slice::from_ref(&site)), 1);

        record_direct_hook(&counters, ordinary.source, Some(&site));
        assert_eq!(counters.adapter_count(), 1);
        assert_eq!(site.hook_count.load(Ordering::Acquire), 1);
        assert_eq!(direct_hook_count(&counters, std::slice::from_ref(&site)), 2);

        counters.reset();
        reset_site_observability(std::slice::from_ref(&site));
        assert_eq!(direct_hook_count(&counters, std::slice::from_ref(&site)), 0);
    }

    #[test]
    fn fork_child_reset_reattributes_each_real_dispatch_source_exactly_once() {
        if std::env::var_os(FORK_ACCOUNTING_CHILD_ENV).as_deref() != Some(OsStr::new("1")) {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime::tests::fork_child_reset_reattributes_each_real_dispatch_source_exactly_once",
                    "--test-threads=1",
                ])
                .env(FORK_ACCOUNTING_CHILD_ENV, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "fork accounting child failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            return;
        }

        SITES.get_or_init(|| {
            (0..MAX_PATCH_SITES)
                .map(|_| SiteSlot::new())
                .collect::<Vec<_>>()
                .into_boxed_slice()
        });
        let ordinary_address = 0x5a17_4000;
        let missing_address = ordinary_address + 2;
        let (site, claimed) = claim_site(ordinary_address).unwrap();
        assert!(claimed);
        site.state.store(SITE_ACTIVE, Ordering::Release);

        let adapter_source = vdso_callback(libc::SYS_getrandom).unwrap().source;
        let ordinary_source = vdso_callback(libc::SYS_time).unwrap().source;
        assert_eq!(adapter_source, DirectHookSource::VdsoAdapter);
        assert_eq!(ordinary_source, DirectHookSource::InstalledSite);
        let event = |number, instruction_pointer, dispatch| SyscallEvent {
            number,
            args: [0; 6],
            instruction_pointer,
            result: 0,
            context: 0,
            dispatch,
            guest_pkru: None,
        };
        let stats = crate::stats::GuestStatsHooks::DISABLED;

        // Model the real callback-entry increment inherited across fork. The
        // production child seam must clear it and then restore this slotless
        // adapter event once, without manufacturing a source SiteSlot.
        reset_fallback_observability();
        record_direct_hook(&DIRECT_HOOK_COUNTERS, adapter_source, None);
        let adapter = event(
            libc::SYS_getrandom,
            ordinary_address,
            SyscallDispatch::InstalledHook(adapter_source),
        );
        reset_and_record_fork_child_dispatch(&adapter, stats);
        assert_eq!(DIRECT_HOOK_COUNTERS.adapter_count(), 1);
        assert_eq!(site.hook_count.load(Ordering::Acquire), 0);
        assert_eq!(
            direct_hook_count(&DIRECT_HOOK_COUNTERS, std::slice::from_ref(site)),
            1,
        );

        // An ordinary installed callback is restored only through its actual
        // source slot. A missing slot remains a negative control and must not
        // alias the adapter counter.
        record_direct_hook(&DIRECT_HOOK_COUNTERS, ordinary_source, Some(site));
        let ordinary = event(
            libc::SYS_time,
            ordinary_address,
            SyscallDispatch::InstalledHook(ordinary_source),
        );
        reset_and_record_fork_child_dispatch(&ordinary, stats);
        assert_eq!(DIRECT_HOOK_COUNTERS.adapter_count(), 0);
        assert_eq!(site.hook_count.load(Ordering::Acquire), 1);
        assert_eq!(
            direct_hook_count(&DIRECT_HOOK_COUNTERS, std::slice::from_ref(site)),
            1,
        );

        let ordinary_missing = event(
            libc::SYS_time,
            missing_address,
            SyscallDispatch::InstalledHook(ordinary_source),
        );
        reset_and_record_fork_child_dispatch(&ordinary_missing, stats);
        assert_eq!(DIRECT_HOOK_COUNTERS.adapter_count(), 0);
        assert_eq!(site.hook_count.load(Ordering::Acquire), 0);
        assert_eq!(
            direct_hook_count(&DIRECT_HOOK_COUNTERS, std::slice::from_ref(site)),
            0,
        );

        // Deferred fallback owns both its per-site trap attribution and the
        // process-wide fallback count. Both survive reset as exactly one
        // current child event, while direct-hook accounting stays empty.
        site.trap_count.store(1, Ordering::Release);
        record_fallback_dispatch(libc::SYS_fork);
        let fallback = event(libc::SYS_fork, ordinary_address, SyscallDispatch::Fallback);
        reset_and_record_fork_child_dispatch(&fallback, stats);
        assert_eq!(site.trap_count.load(Ordering::Acquire), 1);
        assert_eq!(site.hook_count.load(Ordering::Acquire), 0);
        assert_eq!(fallback_dispatch_count(), 1);
        assert_eq!(fallback_syscall_count(libc::SYS_fork), 1);
        assert_eq!(DIRECT_HOOK_COUNTERS.adapter_count(), 0);

        reset_fallback_observability();
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
        super::FALLBACK_REFUSALS.record(405);
        record_direct_hook(&DIRECT_HOOK_COUNTERS, DirectHookSource::VdsoAdapter, None);
        assert!(fallback_dispatch_count() > 0);
        assert!(super::fallback_refusal_count() > 0);
        assert!(DIRECT_HOOK_COUNTERS.adapter_count() > 0);

        FORK_HOOK.run_in_child();

        assert_eq!(fallback_dispatch_count(), 0);
        assert_eq!(super::fallback_refusal_count(), 0);
        assert_eq!(super::fallback_syscall_refusal_count(405), 0);
        assert_eq!(DIRECT_HOOK_COUNTERS.adapter_count(), 0);
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
