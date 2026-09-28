use core::arch::global_asm;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicI32;
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::AtomicU8;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;
use std::cell::Cell;
use std::ffi::OsStr;
use std::io;
use std::io::BufRead;
use std::ptr;
use std::sync::OnceLock;

use liteinst2::patcher::GuardSignalAction;
use liteinst2::patcher::GuardSignalHandler;
use liteinst2::patcher::GuardSignalRuntime;
use liteinst2::patcher::PatchError;
use liteinst2::patcher::prepare_live_patching_with_signal_runtime;
use liteinst2::scanner::InstructionScanner;
use liteinst2::trampoline::HOOK_CONTEXT_STACK_PREFIX_BYTES;
use liteinst2::trampoline::HookContext;
use liteinst2::trampoline::HookSite;
use liteinst2::trampoline::InstalledHook;
use liteinst2::trampoline::SAVED_EXTENDED_STATE_COMPONENT_CAPACITY;
use liteinst2::trampoline::SavedExtendedStateComponent;
use liteinst2::trampoline::SavedExtendedStateDescriptor;
use liteinst2::trampoline::SavedExtendedStateLayout;
use liteinst2::trampoline::TrampolineArena;
use liteinst2::trampoline::TrampolineError;
use reverie::Errno;
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
const HOST_HANDSHAKE_VERSION: u64 = 12;
const HOST_INSTALL_REQUEST_VERSION: u64 = 1;
const HOST_INSTALL_RESULT_VERSION: u64 = 6;
const HOST_INSTALL_PC_MAPPINGS: usize = 16;
const HOST_HELPER_STACK_BYTES: usize = 256 * 1024;
const HOST_CALLBACK_EXECUTION_HEADROOM_BYTES: usize = 8 * 1024 * 1024;
const HOST_CALLBACK_SAVED_XSTATE_RESERVE_CEILING_BYTES: usize = 1024 * 1024;
#[cfg(test)]
const X32_SYSCALL_BIT: i64 = 0x4000_0000;
const UFFD_IOCTL_TYPE: u64 = 0xaa;
const PR_SET_MM: u64 = 35;
const PR_SET_MM_START_BRK: u64 = 6;
const PR_SET_MM_BRK: u64 = 7;
const PR_SET_MM_MAP: u64 = 14;

global_asm!(
    r#"
    .pushsection .liteinst_helper,"ax",@progbits
    .p2align 12
    .popsection

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
    .global reverie_liteinst_host_install_helper
    .type reverie_liteinst_host_install_helper,@function
    .hidden reverie_liteinst_install_site_for_ptrace_body
reverie_liteinst_host_install_helper:
    int3
    .global reverie_liteinst_host_install_helper_rip
reverie_liteinst_host_install_helper_rip:
    jmp reverie_liteinst_install_site_for_ptrace_body
    .size reverie_liteinst_host_install_helper, .-reverie_liteinst_host_install_helper

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
    # Rust may use callee-saved R12 for its own frame after entering the hook.
    # Publish the authenticated HookContext base explicitly at the trap while
    # preserving the caller's live value for the ordinary return path.
    push r12
    mov r12, rsi
    call reverie_liteinst_host_syscall_trap
    .global reverie_liteinst_host_syscall_trap_return_rip
reverie_liteinst_host_syscall_trap_return_rip:
    pop r12
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
    static __reverie_liteinst_helper_page_start: u8;
    static __reverie_liteinst_helper_page_end: u8;
    fn reverie_liteinst_host_begin(frame: *const HostHandshakeFrame);
    static reverie_liteinst_host_begin_rip: u8;
    fn reverie_liteinst_host_ready(frame: *const HostHandshakeFrame);
    static reverie_liteinst_host_ready_rip: u8;
    fn reverie_liteinst_host_install_helper();
    static reverie_liteinst_host_install_helper_rip: u8;
    fn reverie_liteinst_host_helper_return();
    static reverie_liteinst_host_helper_return_rip: u8;
    fn reverie_liteinst_host_syscall_trap_call(
        frame: *mut HostSyscallFrame,
        context: *const HookContext,
    );
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
    install_helper_rip: u64,
    install_helper_page_start: u64,
    install_helper_page_len: u64,
    helper_stack_top: u64,
    callback_stack_start: u64,
    callback_stack_len: u64,
    callback_stack_top: u64,
    helper_return: u64,
    helper_return_rip: u64,
    syscall_trap_rip: u64,
    syscall_trap_return_rip: u64,
    install_request: u64,
    install_result: u64,
    start_program_break: u64,
    initial_program_break: u64,
    callback_execution_headroom_len: u64,
    saved_xstate_reserve_len: u64,
    saved_xstate_alignment: u64,
}

const _: () = assert!(core::mem::offset_of!(HostHandshakeFrame, initial_program_break) == 18 * 8);
const _: () =
    assert!(core::mem::offset_of!(HostHandshakeFrame, callback_execution_headroom_len) == 19 * 8);
const _: () =
    assert!(core::mem::offset_of!(HostHandshakeFrame, saved_xstate_reserve_len) == 20 * 8);
const _: () = assert!(core::mem::offset_of!(HostHandshakeFrame, saved_xstate_alignment) == 21 * 8);
const _: () = assert!(core::mem::size_of::<HostHandshakeFrame>() == 22 * 8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HostCallbackStack {
    usable_start: u64,
    usable_len: u64,
    top: u64,
    execution_headroom_len: u64,
    saved_xstate_reserve_len: u64,
    saved_xstate_alignment: u64,
    saved_xstate_layout: SavedExtendedStateLayout,
}

static HOST_CALLBACK_STACK: OnceLock<HostCallbackStack> = OnceLock::new();

fn checked_page_round_up(value: usize, page: usize) -> Option<usize> {
    if page == 0 || !page.is_power_of_two() {
        return None;
    }
    value
        .checked_add(page - 1)
        .map(|rounded| rounded & !(page - 1))
}

fn host_callback_usable_len(
    execution_headroom: usize,
    frame_prefix: usize,
    saved_xstate_reserve: usize,
    saved_xstate_alignment: usize,
    page: usize,
) -> Option<usize> {
    if saved_xstate_reserve == 0
        || saved_xstate_reserve > HOST_CALLBACK_SAVED_XSTATE_RESERVE_CEILING_BYTES
        || !saved_xstate_alignment.is_power_of_two()
    {
        return None;
    }
    let required = execution_headroom
        .checked_add(frame_prefix)?
        .checked_add(saved_xstate_alignment - 1)?
        .checked_add(saved_xstate_reserve)?;
    checked_page_round_up(required, page)
}

fn host_callback_saved_state_start(
    usable_start: u64,
    top: u64,
    execution_headroom: u64,
    frame_prefix: u64,
    saved_xstate_reserve: u64,
    saved_xstate_alignment: u64,
) -> Option<u64> {
    if saved_xstate_reserve == 0 || !saved_xstate_alignment.is_power_of_two() {
        return None;
    }
    let context_base = top.checked_sub(frame_prefix)?;
    let aligned_context = context_base & !(saved_xstate_alignment - 1);
    let state_start = aligned_context.checked_sub(saved_xstate_reserve)?;
    let available_headroom = state_start.checked_sub(usable_start)?;
    (available_headroom >= execution_headroom).then_some(state_start)
}

fn host_callback_stack_accepts_layout(
    stack: &HostCallbackStack,
    layout: SavedExtendedStateLayout,
) -> bool {
    layout == stack.saved_xstate_layout
        && layout.len() == stack.saved_xstate_reserve_len
        && layout.format().required_alignment() == Some(stack.saved_xstate_alignment)
        && host_callback_saved_state_start(
            stack.usable_start,
            stack.top,
            stack.execution_headroom_len,
            HOOK_CONTEXT_STACK_PREFIX_BYTES as u64,
            layout.len(),
            stack.saved_xstate_alignment,
        )
        .is_some()
}

fn prepare_host_callback_stack() -> io::Result<&'static HostCallbackStack> {
    if let Some(stack) = HOST_CALLBACK_STACK.get() {
        return Ok(stack);
    }
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 {
        return Err(io::Error::other("invalid host callback-stack page size"));
    }
    let page = usize::try_from(page)
        .map_err(|_| io::Error::other("host callback-stack page size is not representable"))?;
    if page != 4096 {
        return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    let saved_xstate_layout = SavedExtendedStateLayout::detect()
        .map_err(|error| io::Error::other(format!("detect callback XSTATE layout: {error}")))?;
    let saved_xstate_reserve = usize::try_from(saved_xstate_layout.len())
        .map_err(|_| io::Error::other("callback XSTATE reserve is not representable"))?;
    let saved_xstate_alignment = saved_xstate_layout
        .format()
        .required_alignment()
        .and_then(|alignment| usize::try_from(alignment).ok())
        .ok_or_else(|| io::Error::other("callback XSTATE alignment is unavailable"))?;
    let usable_len = host_callback_usable_len(
        HOST_CALLBACK_EXECUTION_HEADROOM_BYTES,
        HOOK_CONTEXT_STACK_PREFIX_BYTES,
        saved_xstate_reserve,
        saved_xstate_alignment,
        page,
    )
    .ok_or_else(|| io::Error::other("callback stack geometry is unsupported"))?;
    let mapping_len = usable_len
        .checked_add(
            page.checked_mul(2)
                .ok_or_else(|| io::Error::other("host callback-stack guard size overflow"))?,
        )
        .ok_or_else(|| io::Error::other("host callback-stack mapping size overflow"))?;
    let mapping = unsafe {
        libc::mmap(
            ptr::null_mut(),
            mapping_len,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_STACK,
            -1,
            0,
        )
    };
    if mapping == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    let mapping_start = mapping as usize;
    let usable_start = match mapping_start.checked_add(page) {
        Some(start) => start,
        None => {
            unsafe { libc::munmap(mapping, mapping_len) };
            return Err(io::Error::other("host callback-stack address overflow"));
        }
    };
    let top = match usable_start.checked_add(usable_len) {
        Some(top) => top,
        None => {
            unsafe { libc::munmap(mapping, mapping_len) };
            return Err(io::Error::other("host callback-stack address overflow"));
        }
    };
    if unsafe {
        libc::mprotect(
            usable_start as *mut libc::c_void,
            usable_len,
            libc::PROT_READ | libc::PROT_WRITE,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        unsafe { libc::munmap(mapping, mapping_len) };
        return Err(error);
    }
    let stack = HostCallbackStack {
        usable_start: usable_start as u64,
        usable_len: usable_len as u64,
        top: top as u64,
        execution_headroom_len: HOST_CALLBACK_EXECUTION_HEADROOM_BYTES as u64,
        saved_xstate_reserve_len: saved_xstate_reserve as u64,
        saved_xstate_alignment: saved_xstate_alignment as u64,
        saved_xstate_layout,
    };
    if stack.top & 0xf != 0
        || stack.usable_start.checked_add(stack.usable_len) != Some(stack.top)
        || !host_callback_stack_accepts_layout(&stack, saved_xstate_layout)
    {
        unsafe { libc::munmap(mapping, mapping_len) };
        return Err(io::Error::other(
            "host callback stack has invalid alignment or geometry",
        ));
    }
    if HOST_CALLBACK_STACK.set(stack).is_err() {
        unsafe { libc::munmap(mapping, mapping_len) };
        return Err(io::Error::from_raw_os_error(libc::EALREADY));
    }
    Ok(HOST_CALLBACK_STACK
        .get()
        .expect("host callback stack was just published"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
struct HostInstallRequest {
    version: u64,
    site_start: u64,
    mapping_end: u64,
    source_len: u64,
    source: [u8; PATCH_SNAPSHOT_BYTES],
}

impl Default for HostInstallRequest {
    fn default() -> Self {
        Self {
            version: 0,
            site_start: 0,
            mapping_end: 0,
            source_len: 0,
            source: [0; PATCH_SNAPSHOT_BYTES],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct HostProgramCounterMapping {
    generated_start: u64,
    generated_end: u64,
    logical_address: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct HostSavedXstateComponent {
    xfeature: u64,
    offset: u64,
    size: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct HostSavedXstatePublication {
    allocation_len: u64,
    mask: u64,
    format: u64,
    image_len: u64,
    component_count: u64,
    components: [HostSavedXstateComponent; SAVED_EXTENDED_STATE_COMPONENT_CAPACITY],
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct HostInstallResult {
    version: u64,
    site_start: u64,
    site_len: u64,
    ptrace_entry_stop_rip: u64,
    ptrace_completion_stop_rip: u64,
    relocated_tail: u64,
    trampoline_start: u64,
    trampoline_len: u64,
    trampoline_code_len: u64,
    arena_writable_start: u64,
    arena_writable_len: u64,
    arena_executable_start: u64,
    arena_executable_len: u64,
    instruction_len: u64,
    straddle_prefix: u64,
    program_counter_count: u64,
    program_counters: [HostProgramCounterMapping; HOST_INSTALL_PC_MAPPINGS],
    complete: u64,
    saved_xstate_len: u64,
    saved_xstate_mask: u64,
    saved_xstate_format: u64,
    saved_xstate_image_len: u64,
    saved_xstate_component_count: u64,
    saved_xstate_components: [HostSavedXstateComponent; SAVED_EXTENDED_STATE_COMPONENT_CAPACITY],
}

impl HostInstallResult {
    #[cfg(test)]
    fn saved_xstate_publication(self) -> HostSavedXstatePublication {
        HostSavedXstatePublication {
            allocation_len: self.saved_xstate_len,
            mask: self.saved_xstate_mask,
            format: self.saved_xstate_format,
            image_len: self.saved_xstate_image_len,
            component_count: self.saved_xstate_component_count,
            components: self.saved_xstate_components,
        }
    }
}

const _: () = assert!(core::mem::size_of::<HostProgramCounterMapping>() == 24);
const _: () = assert!(core::mem::size_of::<HostSavedXstateComponent>() == 24);
const _: () = assert!(core::mem::offset_of!(HostSavedXstateComponent, xfeature) == 0);
const _: () = assert!(core::mem::offset_of!(HostSavedXstateComponent, offset) == 8);
const _: () = assert!(core::mem::offset_of!(HostSavedXstateComponent, size) == 16);
const _: () = assert!(core::mem::size_of::<HostSavedXstatePublication>() == 232);
const _: () = assert!(core::mem::offset_of!(HostInstallResult, program_counters) == 128);
const _: () = assert!(core::mem::offset_of!(HostInstallResult, complete) == 512);
const _: () = assert!(core::mem::offset_of!(HostInstallResult, saved_xstate_len) == 520);
const _: () = assert!(core::mem::offset_of!(HostInstallResult, saved_xstate_mask) == 528);
const _: () = assert!(core::mem::offset_of!(HostInstallResult, saved_xstate_format) == 536);
const _: () = assert!(core::mem::offset_of!(HostInstallResult, saved_xstate_image_len) == 544);
const _: () =
    assert!(core::mem::offset_of!(HostInstallResult, saved_xstate_component_count) == 552);
const _: () = assert!(core::mem::offset_of!(HostInstallResult, saved_xstate_components) == 560);
const _: () = assert!(core::mem::size_of::<HostInstallResult>() == 752);

#[repr(align(16))]
struct HostHelperStack([u8; HOST_HELPER_STACK_BYTES]);

static mut HOST_HELPER_STACK: HostHelperStack = HostHelperStack([0; HOST_HELPER_STACK_BYTES]);
static mut HOST_INSTALL_REQUEST: HostInstallRequest = HostInstallRequest {
    version: 0,
    site_start: 0,
    mapping_end: 0,
    source_len: 0,
    source: [0; PATCH_SNAPSHOT_BYTES],
};
static mut HOST_INSTALL_RESULT: HostInstallResult = HostInstallResult {
    version: 0,
    site_start: 0,
    site_len: 0,
    ptrace_entry_stop_rip: 0,
    ptrace_completion_stop_rip: 0,
    relocated_tail: 0,
    trampoline_start: 0,
    trampoline_len: 0,
    trampoline_code_len: 0,
    arena_writable_start: 0,
    arena_writable_len: 0,
    arena_executable_start: 0,
    arena_executable_len: 0,
    instruction_len: 0,
    straddle_prefix: 0,
    program_counter_count: 0,
    program_counters: [HostProgramCounterMapping {
        generated_start: 0,
        generated_end: 0,
        logical_address: 0,
    }; HOST_INSTALL_PC_MAPPINGS],
    complete: 0,
    saved_xstate_len: 0,
    saved_xstate_mask: 0,
    saved_xstate_format: 0,
    saved_xstate_image_len: 0,
    saved_xstate_component_count: 0,
    saved_xstate_components: [HostSavedXstateComponent {
        xfeature: 0,
        offset: 0,
        size: 0,
    }; SAVED_EXTENDED_STATE_COMPONENT_CAPACITY],
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
/// Enables fail-closed, allocation-free in-guest lifecycle stage markers on stderr.
pub const IN_GUEST_STAGE_STREAM_ENV: &str = "REVERIE_LITEINST_IN_GUEST_STAGE_STREAM";
const MAX_PATCH_SITES: usize = 4096;
const ARENA_SLOTS: usize = 128;
const ARENA_SLOT_BYTES: u64 = 4096;
const MAX_PREPARED_ARENAS: usize = MAX_PATCH_SITES.div_ceil(ARENA_SLOTS);
const _: () = assert!(MAX_PREPARED_ARENAS * ARENA_SLOTS >= MAX_PATCH_SITES);
const _: () = assert!((MAX_PREPARED_ARENAS - 1) * ARENA_SLOTS < MAX_PATCH_SITES);
const PATCH_SNAPSHOT_BYTES: usize = 64;
const MAX_PROC_SELF_MAPS_BYTES: usize = 2 * 1024 * 1024;
const SITE_INSTALLING: u8 = 1;
const SITE_ACTIVE: u8 = 2;
const SITE_FALLBACK: u8 = 3;
const SITE_STALE: u8 = 4;
const SITE_EXHAUSTED: u8 = 5;
const INSTRUCTION_CPUID: u8 = 1;
const INSTRUCTION_RDTSC: u8 = 2;
const MAX_LIFETIME_PATCH_ATTEMPTS: usize = MAX_PATCH_SITES;
// Pinned liteinst2 retains fewer than 4 KiB including allocator headers for
// each attempted install that can reserve an arena slot or publish PC maps.
// Counting the attempt before that call also covers activation failures and
// all guarded-publication retries. The separate contiguous headroom probe
// covers the bounded scan/plan/encoder live set and heap fragmentation.
const PATCH_PERSISTENT_BYTES_PER_ATTEMPT: usize = 4 * 1024;
const _: () = assert!(
    MAX_LIFETIME_PATCH_ATTEMPTS * PATCH_PERSISTENT_BYTES_PER_ATTEMPT
        + crate::patch_alloc::PATCH_INSTALL_HEADROOM_BYTES
        <= crate::patch_alloc::PATCH_HEAP_BYTES,
    "lifetime patch admission exceeds its reusable heap"
);

static TOOL_MODE: AtomicU8 = AtomicU8::new(0);
static EVENT_FD: AtomicI32 = AtomicI32::new(libc::STDERR_FILENO);
static COORDINATOR_FD: AtomicI32 = AtomicI32::new(-1);
static EVENT_COOKIE: AtomicU64 = AtomicU64::new(0);
static EVENT_DEVICE: AtomicU64 = AtomicU64::new(0);
static EVENT_INODE: AtomicU64 = AtomicU64::new(0);
static IN_GUEST_STAGE_STREAM: AtomicBool = AtomicBool::new(false);
static LIFETIME_PATCH_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
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
// Once explicit host preparation starts, every publication in this runtime
// must remain controller-verified quiescent. No guard-trap router is installed.
static EXPLICIT_HOST_QUIESCENT: AtomicBool = AtomicBool::new(false);
static ARENAS: OnceLock<Vec<RuntimeArena>> = OnceLock::new();
static SITES: OnceLock<Box<[SiteSlot]>> = OnceLock::new();
static PAGE_SIZE: AtomicU64 = AtomicU64::new(0);
// Exact (not page-rounded) program break. Successful brk calls update this so
// a shrink can be rejected only when the pages Linux would release contain a
// live patch, while unrelated allocator traffic keeps its native semantics.
static PROGRAM_BREAK: AtomicU64 = AtomicU64::new(0);
static PROGRAM_BREAK_START: AtomicU64 = AtomicU64::new(0);
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
    reservation_start: u64,
    reservation_end: u64,
    source_valid: AtomicBool,
    arena: TrampolineArena,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeMap {
    start: u64,
    end: u64,
    offset: u64,
    device_major: u32,
    device_minor: u32,
    inode: u64,
    readable: bool,
    writable: bool,
    executable: bool,
    shared: bool,
    path: Option<Box<str>>,
}

struct SourceMapping {
    start: u64,
    end: u64,
    name: Box<str>,
}

#[derive(Clone, Copy)]
struct ArenaControlRanges {
    writable: (u64, u64),
    executable: (u64, u64),
    reservation: (u64, u64),
}

#[derive(Debug, Default)]
struct ArenaMapValidationStats {
    baseline_comparisons: usize,
    control_lookup_comparisons: usize,
}

// `/proc/<pid>/maps` prints at least two eight-digit addresses, four
// permissions, an eight-digit offset, a two-digit major/minor device, one
// inode digit, separators, and a newline: 40 bytes per record on Linux.
const MIN_PROC_MAP_RECORD_BYTES: usize = 40;
const MAX_PROC_MAP_RECORDS: usize = MAX_PROC_SELF_MAPS_BYTES / MIN_PROC_MAP_RECORD_BYTES + 1;
const PREPARATION_MAP_TEXT_BYTES: usize = 3 * MAX_PROC_SELF_MAPS_BYTES;
// Baseline plus final RuntimeMap vectors, each with at most 2x capacity.
const PREPARATION_RUNTIME_MAP_BYTES: usize =
    4 * MAX_PROC_MAP_RECORDS * std::mem::size_of::<RuntimeMap>();
const PREPARATION_SOURCE_RUNTIME_MAP_BYTES: usize =
    2 * MAX_PROC_MAP_RECORDS * std::mem::size_of::<RuntimeMap>();
const PREPARATION_SOURCE_MAP_BYTES: usize =
    2 * MAX_PROC_MAP_RECORDS * std::mem::size_of::<SourceMapping>();
// Conservatively cover eight liteinst2 range/candidate vectors at 2x capacity.
const PREPARATION_PLANNER_BYTES: usize =
    16 * MAX_PROC_MAP_RECORDS * std::mem::size_of::<(usize, usize)>();
// Arena construction samples at most MAX_PREPARED_ARENAS source mappings and
// pushes at most one RuntimeArena for each sample. Two times that exact cap
// covers geometric Vec capacity without charging every proc-map record as an
// arena after the source vector has already been dropped.
const PREPARATION_ARENA_BYTES: usize =
    2 * MAX_PREPARED_ARENAS * std::mem::size_of::<RuntimeArena>();
// The final snapshot delta retains one reference per record plus one matched
// bit per record while exact prepared controls are authenticated.
const PREPARATION_VALIDATION_BYTES: usize =
    MAX_PROC_MAP_RECORDS * std::mem::size_of::<&RuntimeMap>() + MAX_PROC_MAP_RECORDS.div_ceil(8);
// Baseline/final snapshots own every path while the arena table retains one
// source name. The earlier source-selection phase owns at most two name sets.
const PREPARATION_NAME_BYTES: usize = 3 * MAX_PROC_SELF_MAPS_BYTES;
const PREPARATION_SOURCE_NAME_BYTES: usize = 2 * MAX_PROC_SELF_MAPS_BYTES;
const PREPARATION_FORK_ADVICE_BYTES: usize =
    2 * MAX_PROC_MAP_RECORDS * std::mem::size_of::<(u64, u64)>();
const PREPARATION_BLOCK_OVERHEAD_BYTES: usize = 64 * MAX_PROC_MAP_RECORDS;
const PREPARATION_FIXED_BYTES: usize =
    MAX_PATCH_SITES * std::mem::size_of::<SiteSlot>() + 4 * 1024 * 1024;
const PREPARATION_SOURCE_PHASE_BYTES: usize = PREPARATION_MAP_TEXT_BYTES
    + PREPARATION_SOURCE_RUNTIME_MAP_BYTES
    + PREPARATION_SOURCE_MAP_BYTES
    + PREPARATION_SOURCE_NAME_BYTES
    + PREPARATION_FORK_ADVICE_BYTES;
const PREPARATION_ARENA_PHASE_BYTES: usize = PREPARATION_MAP_TEXT_BYTES
    + PREPARATION_RUNTIME_MAP_BYTES
    // `sources.into_iter()` retains its original allocation while the bounded
    // arena vector is built. Its Box<str> names are already included in the
    // phase-wide name bound below, but the SourceMapping buffer overlaps.
    + PREPARATION_SOURCE_MAP_BYTES
    + PREPARATION_PLANNER_BYTES
    + PREPARATION_ARENA_BYTES
    + PREPARATION_VALIDATION_BYTES
    + PREPARATION_NAME_BYTES;
const PREPARATION_WORST_CASE_BYTES: usize =
    if PREPARATION_SOURCE_PHASE_BYTES > PREPARATION_ARENA_PHASE_BYTES {
        PREPARATION_SOURCE_PHASE_BYTES
    } else {
        PREPARATION_ARENA_PHASE_BYTES
    } + PREPARATION_BLOCK_OVERHEAD_BYTES
        + PREPARATION_FIXED_BYTES;
const _: () = assert!(
    PREPARATION_WORST_CASE_BYTES < crate::patch_alloc::PREPARATION_HEAP_BYTES,
    "bounded preparation storage exceeds its dedicated heap"
);

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
    InstalledHook,
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

    unsafe {
        reverie_preload::signal::install_runtime_siginfo_handler(
            libc::SIGSEGV,
            instruction_sigsegv_handler,
            on_alt_stack,
        )
    }
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

    // Concurrent publication may block signals while a SIGSYS install is in
    // flight and defer restoration to rt_sigreturn. Establish the same
    // DFL/IGN-only handler boundary used by ToolHost before enabling legacy
    // strace/compat interception; later nondefault rt_sigaction requests are
    // rejected in process_syscall for every tool mode.
    let _signal_state = prepare_guest_signal_state(InstructionSubscriptions::default())?;
    install_runtime(
        crate::stats::GuestStatsHooks::DISABLED,
        PatchPublication::Concurrent,
        InstructionSubscriptions::default(),
        &[],
    )
}

fn host_handshake_frame(
    start_program_break: u64,
    initial_program_break: u64,
) -> HostHandshakeFrame {
    // SAFETY: this only forms the address of the dedicated static helper stack;
    // it neither reads nor creates a Rust reference to its mutable contents.
    let stack_start = unsafe { core::ptr::addr_of_mut!(HOST_HELPER_STACK.0) as *mut u8 as usize };
    let helper_page_start =
        core::ptr::addr_of!(__reverie_liteinst_helper_page_start) as usize as u64;
    let helper_page_end = core::ptr::addr_of!(__reverie_liteinst_helper_page_end) as usize as u64;
    let callback_stack = HOST_CALLBACK_STACK
        .get()
        .expect("host callback stack is prepared before the handshake");
    HostHandshakeFrame {
        version: HOST_HANDSHAKE_VERSION,
        begin_rip: core::ptr::addr_of!(reverie_liteinst_host_begin_rip) as usize as u64,
        ready_rip: core::ptr::addr_of!(reverie_liteinst_host_ready_rip) as usize as u64,
        install_helper: reverie_liteinst_host_install_helper as *const () as usize as u64,
        install_helper_rip: core::ptr::addr_of!(reverie_liteinst_host_install_helper_rip) as usize
            as u64,
        install_helper_page_start: helper_page_start,
        install_helper_page_len: helper_page_end - helper_page_start,
        helper_stack_top: (stack_start + HOST_HELPER_STACK_BYTES) as u64,
        callback_stack_start: callback_stack.usable_start,
        callback_stack_len: callback_stack.usable_len,
        callback_stack_top: callback_stack.top,
        helper_return: reverie_liteinst_host_helper_return as *const () as usize as u64,
        helper_return_rip: core::ptr::addr_of!(reverie_liteinst_host_helper_return_rip) as usize
            as u64,
        syscall_trap_rip: core::ptr::addr_of!(reverie_liteinst_host_syscall_trap_rip) as usize
            as u64,
        syscall_trap_return_rip: core::ptr::addr_of!(reverie_liteinst_host_syscall_trap_return_rip)
            as usize as u64,
        install_request: core::ptr::addr_of!(HOST_INSTALL_REQUEST) as usize as u64,
        install_result: core::ptr::addr_of!(HOST_INSTALL_RESULT) as usize as u64,
        start_program_break,
        initial_program_break,
        callback_execution_headroom_len: callback_stack.execution_headroom_len,
        saved_xstate_reserve_len: callback_stack.saved_xstate_reserve_len,
        saved_xstate_alignment: callback_stack.saved_xstate_alignment,
    }
}

fn initialize_host_runtime() -> io::Result<()> {
    initialize_host_runtime_with(false, prepare_instrumentation_after_preflight)
}

pub(crate) fn initialize_host_runtime_explicit(config: crate::HostRuntimeConfig) -> io::Result<()> {
    if config.version != crate::HOST_RUNTIME_CONFIG_VERSION {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let staleness = liteinst2::patcher::StalenessBudget::new(config.straddler_staleness_ticks);
    initialize_host_runtime_with(true, || {
        crate::straddler::initialize(staleness)?;
        prepare_instrumentation_state()
    })
}

fn initialize_host_runtime_with(
    explicit_quiescent: bool,
    prepare: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if reverie_preload::trap::has_dispatcher()
        || crate::straddler::is_initialized()
        || SITES.get().is_some()
        || ARENAS.get().is_some()
    {
        return Err(io::Error::from_raw_os_error(libc::EALREADY));
    }
    // The explicit after-loader controller keeps every other task stopped.
    // Enter the non-TLS preparation allocator before even allocating preflight
    // work so the first allocator action preserves the entry disassembly
    // contract; the scope remains live through Ready.
    let _preparation_scope = if explicit_quiescent {
        Some(
            crate::patch_alloc::enter_preparation()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EALREADY))?,
        )
    } else {
        None
    };
    // This scan must precede straddler/live-patching initialization, OnceLock
    // publication, and the Begin trap. Returning an error after any of those
    // process-global transitions would leave a partially installed runtime.
    refuse_preexisting_async_mapping_engines()?;
    HOST_INITIALIZATION_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| io::Error::from_raw_os_error(libc::EALREADY))?;
    if explicit_quiescent {
        EXPLICIT_HOST_QUIESCENT.store(true, Ordering::Release);
        PATCH_PUBLICATION.store(PatchPublication::Quiescent as u8, Ordering::Release);
    }
    let (start_program_break, initial_program_break) = bind_program_break_geometry()?;
    prepare_host_callback_stack()?;
    let mut frame = host_handshake_frame(start_program_break, initial_program_break);
    // SAFETY: the launcher validates this exact DSO/RIP/frame before suppressing
    // the trap. The function returns normally after ptrace resumes the tracee.
    unsafe { reverie_liteinst_host_begin(&frame) };
    prepare()?;
    let (start_program_break, initial_program_break) = bind_program_break_geometry()?;
    frame.start_program_break = start_program_break;
    frame.initial_program_break = initial_program_break;
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
    if EXPLICIT_HOST_QUIESCENT.load(Ordering::Acquire) {
        return Err(io::Error::from_raw_os_error(libc::EALREADY));
    }
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

fn install_runtime(
    stats: crate::stats::GuestStatsHooks,
    publication: PatchPublication,
    instructions: InstructionSubscriptions,
    vdso_sites: &[reverie_ptrace::VdsoSyscallSite],
) -> io::Result<()> {
    if EXPLICIT_HOST_QUIESCENT.load(Ordering::Acquire) {
        return Err(io::Error::from_raw_os_error(libc::EALREADY));
    }
    PATCH_PUBLICATION.store(publication as u8, Ordering::Release);
    prepare_instrumentation()?;
    install_vdso_sites(vdso_sites)?;
    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(PR-254): Review launcher-selected RuntimeConfig at the install seam.
    let config = runtime_config_from_env()?;
    install_instruction_signal_handler(instructions, config.use_alt_stack)?;
    unsafe {
        reverie_preload::install(
            Box::new(LiteinstDispatcher::new(stats, publication)),
            &InProcessSeccomp,
            &config,
        )
    }?;
    enable_instruction_faulting(instructions)
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

fn read_proc_self_maps() -> io::Result<String> {
    let descriptor = unsafe {
        libc::openat(
            libc::AT_FDCWD,
            b"/proc/self/maps\0".as_ptr().cast(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_LARGEFILE,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }

    let result = (|| {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let amount = unsafe { libc::read(descriptor, chunk.as_mut_ptr().cast(), chunk.len()) };
            if amount < 0 {
                return Err(io::Error::last_os_error());
            }
            let amount = usize::try_from(amount)
                .map_err(|_| io::Error::other("negative proc-maps read length"))?;
            if amount == 0 {
                break;
            }
            let new_length = bytes
                .len()
                .checked_add(amount)
                .filter(|length| *length <= MAX_PROC_SELF_MAPS_BYTES)
                .ok_or_else(|| io::Error::other("/proc/self/maps exceeds its byte bound"))?;
            bytes.reserve(new_length - bytes.len());
            bytes.extend_from_slice(&chunk[..amount]);
        }
        String::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.utf8_error()))
    })();
    let close_result = unsafe { libc::close(descriptor) };
    match (result, close_result) {
        (Err(error), _) => Err(error),
        (Ok(_), result) if result != 0 => Err(io::Error::last_os_error()),
        (Ok(maps), _) => Ok(maps),
    }
}

fn parse_runtime_map_line(line: &str, index: usize) -> io::Result<RuntimeMap> {
    let invalid = |field: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid /proc/self/maps {} on record {}", field, index + 1),
        )
    };
    let mut fields = line.split_whitespace();
    let (range, permissions, offset, device, inode) = (
        fields.next().ok_or_else(|| invalid("range"))?,
        fields.next().ok_or_else(|| invalid("permissions"))?,
        fields.next().ok_or_else(|| invalid("offset"))?,
        fields.next().ok_or_else(|| invalid("device"))?,
        fields.next().ok_or_else(|| invalid("inode"))?,
    );
    let (start, end) = range.split_once('-').ok_or_else(|| invalid("range"))?;
    if end.contains('-') {
        return Err(invalid("range"));
    }
    let start = u64::from_str_radix(start, 16).map_err(|_| invalid("range start"))?;
    let end = u64::from_str_radix(end, 16).map_err(|_| invalid("range end"))?;
    if start >= end {
        return Err(invalid("range geometry"));
    }
    let (device_major, device_minor) = device.split_once(':').ok_or_else(|| invalid("device"))?;
    if device_minor.contains(':') {
        return Err(invalid("device"));
    }
    let permissions = permissions.as_bytes();
    if permissions.len() != 4
        || !matches!(permissions[0], b'r' | b'-')
        || !matches!(permissions[1], b'w' | b'-')
        || !matches!(permissions[2], b'x' | b'-')
        || !matches!(permissions[3], b'p' | b's')
    {
        return Err(invalid("permissions"));
    }
    let path = fields.collect::<Vec<_>>().join(" ");
    let path = (!path.is_empty()).then(|| path.into_boxed_str());
    Ok(RuntimeMap {
        start,
        end,
        offset: u64::from_str_radix(offset, 16).map_err(|_| invalid("offset"))?,
        device_major: u32::from_str_radix(device_major, 16).map_err(|_| invalid("device major"))?,
        device_minor: u32::from_str_radix(device_minor, 16).map_err(|_| invalid("device minor"))?,
        inode: inode.parse().map_err(|_| invalid("inode"))?,
        readable: permissions.first() == Some(&b'r'),
        writable: permissions.get(1) == Some(&b'w'),
        executable: permissions.get(2) == Some(&b'x'),
        shared: permissions.get(3) == Some(&b's'),
        path,
    })
}

fn read_runtime_maps() -> io::Result<Vec<RuntimeMap>> {
    let maps = read_proc_self_maps()?;
    maps.lines()
        .enumerate()
        .map(|(index, line)| parse_runtime_map_line(line, index))
        .collect()
}

fn read_fork_safe_mapping_ranges_from(reader: impl BufRead) -> io::Result<Vec<(u64, u64)>> {
    let mut reader = reader;
    let mut line = String::new();
    let mut current = None;
    let mut protection_key = None;
    let mut safe = Vec::new();
    let mut mapping_records = 0_usize;
    loop {
        line.clear();
        let amount = reader.read_line(&mut line)?;
        if amount == 0 {
            break;
        }
        if line.len() > MAX_PROC_SELF_MAPS_BYTES {
            return Err(io::Error::other(
                "/proc/self/smaps line exceeds its byte bound",
            ));
        }
        if let Ok(mapping) = parse_runtime_map_line(&line, mapping_records) {
            if mapping_records == MAX_PROC_MAP_RECORDS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "/proc/self/smaps exceeds its mapping-record bound",
                ));
            }
            mapping_records += 1;
            current = Some((mapping.start, mapping.end));
            protection_key = None;
            continue;
        }
        let mut fields = line.split_whitespace();
        let first = fields.next();
        let second = fields.next();
        let permissions = second.is_some_and(|field| {
            let field = field.as_bytes();
            field.len() == 4
                && matches!(field[0], b'r' | b'-')
                && matches!(field[1], b'w' | b'-')
                && matches!(field[2], b'x' | b'-')
                && matches!(field[3], b'p' | b's')
        });
        if !first.is_some_and(|field| field.ends_with(':')) || permissions {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed /proc/self/smaps metadata or mapping header",
            ));
        }
        if let Some(raw_key) = line.strip_prefix("ProtectionKey:") {
            if current.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "out-of-order /proc/self/smaps ProtectionKey",
                ));
            }
            if protection_key.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate /proc/self/smaps ProtectionKey",
                ));
            }
            let raw_key = raw_key.trim();
            let key = (!raw_key.is_empty())
                .then(|| raw_key.parse::<u64>().ok())
                .flatten()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid /proc/self/smaps ProtectionKey",
                    )
                })?;
            protection_key = Some(key);
            continue;
        }
        if let Some(flags) = line.strip_prefix("VmFlags:") {
            let range = current.take().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate or out-of-order /proc/self/smaps VmFlags",
                )
            })?;
            let key = protection_key.take();
            // `ht` mappings use VMA-specific huge-page geometry that the
            // allocation-free signal observer cannot recover exactly.
            if key == Some(0)
                && !flags
                    .split_whitespace()
                    .any(|flag| matches!(flag, "dc" | "wf" | "ht"))
            {
                safe.push(range);
            }
        }
    }
    Ok(safe)
}

fn read_fork_safe_mapping_ranges() -> io::Result<Vec<(u64, u64)>> {
    let file = std::fs::File::open("/proc/self/smaps")?;
    read_fork_safe_mapping_ranges_from(std::io::BufReader::with_capacity(8192, file))
}

fn runtime_maps_are_strictly_ordered(maps: &[RuntimeMap]) -> bool {
    maps.iter().all(|mapping| mapping.start < mapping.end)
        && maps.windows(2).all(|pair| pair[0].end <= pair[1].start)
}

fn collect_new_runtime_maps<'a>(
    before: &[RuntimeMap],
    after: &'a [RuntimeMap],
    stats: &mut ArenaMapValidationStats,
) -> io::Result<Vec<&'a RuntimeMap>> {
    if !runtime_maps_are_strictly_ordered(before) || !runtime_maps_are_strictly_ordered(after) {
        return Err(io::Error::other(
            "LiteInst map snapshots are not strictly ordered and disjoint",
        ));
    }

    let mut baseline_index = 0;
    let mut final_index = 0;
    let mut new_maps: Vec<&RuntimeMap> = Vec::new();
    new_maps
        .try_reserve_exact(after.len())
        .map_err(|_| io::Error::other("could not reserve the LiteInst map-delta table"))?;
    while let Some(mapping) = after.get(final_index) {
        let Some(existing) = before.get(baseline_index) else {
            new_maps.extend(after[final_index..].iter());
            final_index = after.len();
            break;
        };
        stats.baseline_comparisons += 1;
        match existing.start.cmp(&mapping.start) {
            std::cmp::Ordering::Less => {
                return Err(io::Error::other(
                    "a baseline mapping disappeared during LiteInst preparation",
                ));
            }
            std::cmp::Ordering::Equal => {
                if existing != mapping {
                    return Err(io::Error::other(
                        "a baseline mapping changed during LiteInst preparation",
                    ));
                }
                baseline_index += 1;
                final_index += 1;
            }
            std::cmp::Ordering::Greater => {
                new_maps.push(mapping);
                final_index += 1;
            }
        }
    }
    debug_assert_eq!(final_index, after.len());
    if baseline_index != before.len() {
        return Err(io::Error::other(
            "a baseline mapping disappeared during LiteInst preparation",
        ));
    }
    Ok(new_maps)
}

fn exact_new_runtime_map<'a>(
    new_maps: &[&'a RuntimeMap],
    range: (u64, u64),
    stats: &mut ArenaMapValidationStats,
) -> Option<(usize, &'a RuntimeMap)> {
    let mut left = 0;
    let mut right = new_maps.len();
    while left < right {
        stats.control_lookup_comparisons += 1;
        let middle = left + (right - left) / 2;
        if new_maps[middle].start < range.0 {
            left = middle + 1;
        } else {
            right = middle;
        }
    }
    new_maps
        .get(left)
        .copied()
        .filter(|mapping| mapping.start == range.0 && mapping.end == range.1)
        .map(|mapping| (left, mapping))
}

fn take_exact_new_runtime_map<'a>(
    new_maps: &[&'a RuntimeMap],
    matched: &mut [bool],
    range: (u64, u64),
    missing: &'static str,
    stats: &mut ArenaMapValidationStats,
) -> io::Result<&'a RuntimeMap> {
    if range.0 >= range.1 {
        return Err(io::Error::other(
            "LiteInst prepared arena control has invalid geometry",
        ));
    }
    let (index, mapping) =
        exact_new_runtime_map(new_maps, range, stats).ok_or_else(|| io::Error::other(missing))?;
    if matched[index] {
        return Err(io::Error::other(
            "LiteInst prepared arena controls overlap or reuse one mapping",
        ));
    }
    matched[index] = true;
    Ok(mapping)
}

fn validate_prepared_control_maps<T>(
    before: &[RuntimeMap],
    after: &[RuntimeMap],
    arenas: &[T],
    controls: impl Fn(&T) -> ArenaControlRanges,
    arena_mapping_bytes: u64,
    reservation_bytes: u64,
) -> io::Result<ArenaMapValidationStats> {
    if arena_mapping_bytes == 0 || reservation_bytes == 0 {
        return Err(io::Error::other(
            "LiteInst prepared-arena control sizes are invalid",
        ));
    }
    let expected_maps = arenas
        .len()
        .checked_mul(3)
        .ok_or_else(|| io::Error::other("LiteInst prepared-arena map count overflow"))?;
    let mut stats = ArenaMapValidationStats::default();
    let new_maps = collect_new_runtime_maps(before, after, &mut stats)?;
    if new_maps.len() != expected_maps {
        return Err(io::Error::other(format!(
            "LiteInst prepared {} arenas but produced {} new mappings",
            arenas.len(),
            new_maps.len(),
        )));
    }
    let mut matched = vec![false; new_maps.len()];
    for arena in arenas {
        let controls = controls(arena);
        if controls.writable.1.checked_sub(controls.writable.0) != Some(arena_mapping_bytes)
            || controls.executable.1.checked_sub(controls.executable.0) != Some(arena_mapping_bytes)
            || controls.reservation.1.checked_sub(controls.reservation.0) != Some(reservation_bytes)
        {
            return Err(io::Error::other(
                "LiteInst prepared arena controls have unexpected sizes",
            ));
        }
        let writable = take_exact_new_runtime_map(
            &new_maps,
            &mut matched,
            controls.writable,
            "LiteInst writable arena alias is missing",
            &mut stats,
        )?;
        let executable = take_exact_new_runtime_map(
            &new_maps,
            &mut matched,
            controls.executable,
            "LiteInst executable arena alias is missing",
            &mut stats,
        )?;
        let reservation = take_exact_new_runtime_map(
            &new_maps,
            &mut matched,
            controls.reservation,
            "LiteInst shared reservation page is missing",
            &mut stats,
        )?;
        let arena_path = Some("/memfd:liteinst2-trampoline (deleted)");
        let aliases_valid = writable.offset == 0
            && writable.inode != 0
            && writable.shared
            && writable.readable
            && writable.writable
            && !writable.executable
            && writable.path.as_deref() == arena_path
            && executable.offset == 0
            && executable.shared
            && executable.readable
            && !executable.writable
            && executable.executable
            && executable.path.as_deref() == arena_path
            && executable.device_major == writable.device_major
            && executable.device_minor == writable.device_minor
            && executable.inode == writable.inode;
        let reservation_valid = reservation.offset == 0
            && reservation.device_major == 0
            && reservation.device_minor == 1
            && reservation.inode != 0
            && reservation.shared
            && reservation.readable
            && reservation.writable
            && !reservation.executable
            && reservation.path.as_deref() == Some("/dev/zero (deleted)");
        if !aliases_valid || !reservation_valid {
            return Err(io::Error::other(
                "LiteInst arena mappings do not match the exact sealed-alias/reservation contract",
            ));
        }
        let alias_identity_maps = new_maps
            .iter()
            .filter(|mapping| {
                mapping.device_major == writable.device_major
                    && mapping.device_minor == writable.device_minor
                    && mapping.inode == writable.inode
                    && mapping.path.as_deref() == arena_path
            })
            .count();
        if alias_identity_maps != 2 {
            return Err(io::Error::other(
                "LiteInst prepared arenas reuse one sealed alias backing identity",
            ));
        }
        let reservation_identity_maps = new_maps
            .iter()
            .filter(|mapping| {
                mapping.device_major == reservation.device_major
                    && mapping.device_minor == reservation.device_minor
                    && mapping.inode == reservation.inode
                    && mapping.path.as_deref() == Some("/dev/zero (deleted)")
            })
            .count();
        if reservation_identity_maps != 1 {
            return Err(io::Error::other(
                "LiteInst prepared arenas reuse one shared reservation identity",
            ));
        }
    }
    if matched.iter().any(|matched| !matched) {
        return Err(io::Error::other(
            "LiteInst preparation produced an unauthenticated extra mapping",
        ));
    }
    Ok(stats)
}

fn validate_prepared_arena_maps(
    before: &[RuntimeMap],
    after: &[RuntimeMap],
    arenas: &[RuntimeArena],
) -> io::Result<()> {
    let arena_mapping_bytes = (ARENA_SLOTS as u64)
        .checked_mul(ARENA_SLOT_BYTES)
        .ok_or_else(|| io::Error::other("LiteInst arena size overflow"))?;
    let reservation_bytes = PAGE_SIZE.load(Ordering::Acquire);
    validate_prepared_control_maps(
        before,
        after,
        arenas,
        |arena| ArenaControlRanges {
            writable: (arena.writable_start, arena.writable_end),
            executable: (arena.executable_start, arena.executable_end),
            reservation: (arena.reservation_start, arena.reservation_end),
        },
        arena_mapping_bytes,
        reservation_bytes,
    )
    .map(|_| ())
}

fn is_async_mapping_engine_target(target: &[u8]) -> bool {
    matches!(
        target,
        b"anon_inode:[io_uring]" | b"anon_inode:[userfaultfd]"
    )
}

fn is_io_uring_mapping_target(target: &[u8]) -> bool {
    matches!(target, b"anon_inode:[io_uring]" | b"[io_uring]")
}

fn parse_ascii_hex(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0_u64, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => u64::from(byte - b'0'),
            b'a'..=b'f' => u64::from(byte - b'a' + 10),
            b'A'..=b'F' => u64::from(byte - b'A' + 10),
            _ => return None,
        };
        value.checked_mul(16)?.checked_add(digit)
    })
}

fn proc_maps_line_async_engine_at(line: &[u8], address: u64) -> Result<Option<bool>, ()> {
    let first_space = line.iter().position(u8::is_ascii_whitespace).ok_or(())?;
    let range = &line[..first_space];
    let dash = range.iter().position(|byte| *byte == b'-').ok_or(())?;
    if range[dash + 1..].contains(&b'-') {
        return Err(());
    }
    let start = parse_ascii_hex(&range[..dash]).ok_or(())?;
    let end = parse_ascii_hex(&range[dash + 1..]).ok_or(())?;
    if start >= end {
        return Err(());
    }
    if !(start <= address && address < end) {
        return Ok(None);
    }

    let mut cursor = first_space;
    // Skip permissions, offset, device, and inode after the range.
    for _ in 0..4 {
        while cursor < line.len() && line[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == line.len() {
            return Err(());
        }
        while cursor < line.len() && !line[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
    }
    while cursor < line.len() && line[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    let target = &line[cursor..];
    Ok(Some(is_io_uring_mapping_target(target)))
}

fn mmap_result_is_async_mapping_engine(address: u64) -> Result<bool, Errno> {
    const MAPS: &[u8] = b"/proc/self/maps\0";
    let descriptor = unsafe {
        raw_syscall6(
            libc::SYS_openat,
            [
                libc::AT_FDCWD as i64 as u64,
                MAPS.as_ptr() as u64,
                (libc::O_RDONLY | libc::O_CLOEXEC) as u64,
                0,
                0,
                0,
            ],
        )
    };
    if descriptor < 0 {
        return Err(Errno::new((-descriptor) as i32));
    }
    let result = (|| {
        let mut chunk = [0_u8; 1024];
        let mut line = [0_u8; 4096];
        let mut line_len = 0_usize;
        loop {
            let amount = unsafe {
                raw_syscall6(
                    libc::SYS_read,
                    [
                        descriptor as u64,
                        chunk.as_mut_ptr() as u64,
                        chunk.len() as u64,
                        0,
                        0,
                        0,
                    ],
                )
            };
            if amount < 0 {
                let error = Errno::new((-amount) as i32);
                if error == Errno::EINTR {
                    continue;
                }
                return Err(error);
            }
            if amount == 0 {
                if line_len != 0
                    && let Some(is_async) =
                        proc_maps_line_async_engine_at(&line[..line_len], address)
                            .map_err(|_| Errno::EPROTO)?
                {
                    return Ok(is_async);
                }
                return Err(Errno::ENOENT);
            }
            for byte in &chunk[..amount as usize] {
                if *byte == b'\n' {
                    if let Some(is_async) =
                        proc_maps_line_async_engine_at(&line[..line_len], address)
                            .map_err(|_| Errno::EPROTO)?
                    {
                        return Ok(is_async);
                    }
                    line_len = 0;
                } else {
                    let Some(slot) = line.get_mut(line_len) else {
                        return Err(Errno::EOVERFLOW);
                    };
                    *slot = *byte;
                    line_len += 1;
                }
            }
        }
    })();
    let close = unsafe { raw_syscall6(libc::SYS_close, [descriptor as u64, 0, 0, 0, 0, 0]) };
    match (result, close) {
        (Err(error), _) => Err(error),
        (Ok(_), result) if result < 0 => Err(Errno::new((-result) as i32)),
        (Ok(is_async), _) => Ok(is_async),
    }
}

fn proc_self_fd_path(fd: libc::c_int, path: &mut [u8; 64]) -> Option<usize> {
    if fd < 0 {
        return None;
    }
    let prefix = b"/proc/self/fd/";
    path[..prefix.len()].copy_from_slice(prefix);
    let mut digits = [0_u8; 10];
    let mut value = fd as u32;
    let mut count = 0;
    loop {
        digits[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let path_len = prefix.len().checked_add(count)?;
    if path_len >= path.len() {
        return None;
    }
    for (destination, source) in path[prefix.len()..path_len]
        .iter_mut()
        .zip(digits[..count].iter().rev())
    {
        *destination = *source;
    }
    path[path_len] = 0;
    Some(path_len)
}

fn fd_is_async_mapping_engine(fd: libc::c_int) -> Result<bool, Errno> {
    let mut path = [0_u8; 64];
    proc_self_fd_path(fd, &mut path).ok_or(Errno::EBADF)?;
    let mut target = [0_u8; 128];
    let target_len = unsafe {
        raw_syscall6(
            libc::SYS_readlinkat,
            [
                libc::AT_FDCWD as i64 as u64,
                path.as_ptr() as u64,
                target.as_mut_ptr() as u64,
                target.len() as u64,
                0,
                0,
            ],
        )
    };
    if target_len < 0 {
        return Err(Errno::new((-target_len) as i32));
    }
    let target_len = target_len as usize;
    if target_len == target.len() {
        return Err(Errno::ENAMETOOLONG);
    }
    Ok(is_async_mapping_engine_target(&target[..target_len]))
}

fn mmap_imports_async_mapping_engine_with(
    number: i64,
    args: [u64; 6],
    mut resolve: impl FnMut(libc::c_int) -> Result<bool, Errno>,
) -> bool {
    if number != libc::SYS_mmap || args[3] & libc::MAP_ANONYMOUS as u64 != 0 {
        return false;
    }
    let fd = args[4] as libc::c_int;
    if fd < 0 {
        return false;
    }
    match resolve(fd) {
        Ok(is_async) => is_async,
        // A concurrently closed/replaced descriptor will receive the kernel's
        // native EBADF/ENOENT outcome. Any other authentication failure closes
        // the asynchronous-mapping hole rather than forwarding an unknown fd.
        Err(Errno::EBADF | Errno::ENOENT) => false,
        Err(_) => true,
    }
}

fn refuse_preexisting_async_mapping_engines() -> io::Result<()> {
    const DIRECTORY: &[u8] = b"/proc/self/fd\0";
    let directory = unsafe {
        raw_syscall6(
            libc::SYS_openat,
            [
                libc::AT_FDCWD as i64 as u64,
                DIRECTORY.as_ptr() as u64,
                (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
                0,
                0,
                0,
            ],
        )
    };
    if directory < 0 {
        return Err(io::Error::from_raw_os_error((-directory) as i32));
    }
    let result = (|| {
        let mut entries = [0_u8; 8192];
        loop {
            let amount = unsafe {
                raw_syscall6(
                    libc::SYS_getdents64,
                    [
                        directory as u64,
                        entries.as_mut_ptr() as u64,
                        entries.len() as u64,
                        0,
                        0,
                        0,
                    ],
                )
            };
            if amount < 0 {
                return Err(io::Error::from_raw_os_error((-amount) as i32));
            }
            let amount = amount as usize;
            if amount == 0 {
                return Ok(());
            }
            let mut cursor = 0;
            while cursor < amount {
                if amount - cursor < 20 {
                    return Err(io::Error::other("truncated /proc/self/fd directory record"));
                }
                let record_len = u16::from_ne_bytes(
                    entries[cursor + 16..cursor + 18]
                        .try_into()
                        .expect("fixed directory record field"),
                ) as usize;
                if record_len < 20
                    || cursor
                        .checked_add(record_len)
                        .is_none_or(|end| end > amount)
                {
                    return Err(io::Error::other("malformed /proc/self/fd directory record"));
                }
                let name_bytes = &entries[cursor + 19..cursor + record_len];
                let name_len = name_bytes
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(name_bytes.len());
                let name = &name_bytes[..name_len];
                if !name.is_empty() && name.iter().all(u8::is_ascii_digit) {
                    let mut path = [0_u8; 64];
                    let prefix = b"/proc/self/fd/";
                    let path_len = prefix.len() + name.len();
                    if path_len + 1 > path.len() {
                        return Err(io::Error::other("/proc/self/fd entry name is too long"));
                    }
                    path[..prefix.len()].copy_from_slice(prefix);
                    path[prefix.len()..path_len].copy_from_slice(name);
                    let mut target = [0_u8; 128];
                    let target_len = unsafe {
                        raw_syscall6(
                            libc::SYS_readlinkat,
                            [
                                libc::AT_FDCWD as i64 as u64,
                                path.as_ptr() as u64,
                                target.as_mut_ptr() as u64,
                                target.len() as u64,
                                0,
                                0,
                            ],
                        )
                    };
                    if target_len >= 0 {
                        if target_len as usize == target.len() {
                            return Err(io::Error::other(
                                "preexisting fd target exceeds its authentication bound",
                            ));
                        }
                        let target = &target[..target_len as usize];
                        if is_async_mapping_engine_target(target) {
                            return Err(io::Error::other(
                                "preexisting io_uring or userfaultfd can mutate LiteInst mappings asynchronously",
                            ));
                        }
                    } else if target_len != -i64::from(libc::ENOENT) {
                        return Err(io::Error::from_raw_os_error((-target_len) as i32));
                    }
                }
                cursor += record_len;
            }
        }
    })();
    let close = unsafe { raw_syscall6(libc::SYS_close, [directory as u64, 0, 0, 0, 0, 0]) };
    match (result, close) {
        (Err(error), _) => Err(error),
        (Ok(()), result) if result < 0 => Err(io::Error::from_raw_os_error((-result) as i32)),
        (Ok(()), _) => Ok(()),
    }
}

fn bind_program_break_geometry() -> io::Result<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    let fields = stat
        .rsplit_once(") ")
        .map(|(_, fields)| fields)
        .ok_or_else(|| io::Error::other("malformed /proc/self/stat command field"))?;
    let start = fields
        .split_whitespace()
        .nth(44)
        .and_then(|field| field.parse::<u64>().ok())
        .filter(|address| *address != 0)
        .ok_or_else(|| io::Error::other("could not bind the starting program break"))?;
    let current = unsafe { raw_syscall6(libc::SYS_brk, [0; 6]) };
    let current = u64::try_from(current)
        .ok()
        .filter(|address| *address != 0)
        .ok_or_else(|| io::Error::other("could not bind the current program break"))?;
    if current < start {
        return Err(io::Error::other(
            "current program break precedes its starting address",
        ));
    }
    PROGRAM_BREAK_START.store(start, Ordering::Release);
    PROGRAM_BREAK.store(current, Ordering::Release);
    Ok((start, current))
}

fn prepare_instrumentation() -> io::Result<()> {
    refuse_preexisting_async_mapping_engines()?;
    bind_program_break_geometry()?;
    prepare_instrumentation_after_preflight()
}

fn exact_signal_syscall_result(result: i64) -> Result<(), i32> {
    if result == 0 {
        Ok(())
    } else if (-4095..=-1).contains(&result) {
        Err((-result) as i32)
    } else {
        Err(libc::EPROTO)
    }
}

fn guard_prior_signal_action_is_admitted(action: &GuardSignalAction) -> bool {
    matches!(action.handler, libc::SIG_DFL | libc::SIG_IGN)
}

unsafe fn install_guard_signal_handler(
    signal: libc::c_int,
    handler: GuardSignalHandler,
    flags: libc::c_int,
    previous: *mut GuardSignalAction,
) -> Result<(), i32> {
    if previous.is_null() {
        return Err(libc::EFAULT);
    }
    let queried = unsafe {
        raw_syscall6(
            libc::SYS_rt_sigaction,
            [
                signal as u64,
                0,
                previous as u64,
                core::mem::size_of::<u64>() as u64,
                0,
                0,
            ],
        )
    };
    exact_signal_syscall_result(queried)?;
    if !guard_prior_signal_action_is_admitted(unsafe { &*previous }) {
        return Err(libc::EPERM);
    }
    unsafe {
        reverie_preload::signal::install_runtime_siginfo_handler_with_flags(signal, handler, flags)
    }
    .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))
}

unsafe fn restore_default_guard_signal(
    signal: libc::c_int,
    previous: &GuardSignalAction,
) -> Result<(), i32> {
    if previous.handler != libc::SIG_DFL {
        return Err(libc::EINVAL);
    }
    let restored = unsafe {
        raw_syscall6(
            libc::SYS_rt_sigaction,
            [
                signal as u64,
                previous as *const GuardSignalAction as u64,
                0,
                core::mem::size_of::<u64>() as u64,
                0,
                0,
            ],
        )
    };
    exact_signal_syscall_result(restored)?;

    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
    if pid <= 0 || tid <= 0 {
        return Err(libc::EPROTO);
    }
    let redelivered = unsafe {
        raw_syscall6(
            libc::SYS_tgkill,
            [pid as u64, tid as u64, signal as u64, 0, 0, 0],
        )
    };
    exact_signal_syscall_result(redelivered)
}

fn prepare_instrumentation_after_preflight() -> io::Result<()> {
    crate::straddler::initialize_from_environment()?;
    prepare_live_patching_with_signal_runtime(GuardSignalRuntime {
        install: install_guard_signal_handler,
        restore_default: restore_default_guard_signal,
    })
    .map_err(|error| io::Error::other(error.to_string()))?;
    prepare_instrumentation_state()?;
    bind_program_break_geometry()?;
    Ok(())
}

fn sampled_source_index(ordinal: usize, total: usize, count: usize) -> Option<usize> {
    if count == 0 || count > total || ordinal >= count {
        return None;
    }
    if count == 1 {
        return Some(0);
    }
    ordinal.checked_mul(total - 1)?.checked_div(count - 1)
}

fn prepare_instrumentation_state() -> io::Result<()> {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size)
        .ok()
        .filter(|size| size.is_power_of_two())
        .ok_or_else(|| io::Error::other("invalid operating-system page size"))?;
    PAGE_SIZE.store(page_size, Ordering::Release);
    if PROGRAM_BREAK.load(Ordering::Acquire) == 0 {
        bind_program_break_geometry()?;
    }

    let sites = (0..MAX_PATCH_SITES)
        .map(|_| SiteSlot::new())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    SITES
        .set(sites)
        .map_err(|_| io::Error::other("LiteInst site registry initialized twice"))?;

    let maps = read_runtime_maps()?;
    let mut fork_safe_ranges = read_fork_safe_mapping_ranges()?;
    fork_safe_ranges.sort_unstable();
    fork_safe_ranges.dedup();
    let sources = maps
        .iter()
        .filter(|mapping| {
            mapping.readable
                && !mapping.writable
                && mapping.executable
                && !mapping.shared
                && fork_safe_ranges
                    .binary_search(&(mapping.start, mapping.end))
                    .is_ok()
        })
        .map(|mapping| SourceMapping {
            start: mapping.start,
            end: mapping.end,
            name: mapping
                .path
                .as_deref()
                .unwrap_or("[anonymous]")
                .rsplit('/')
                .next()
                .unwrap_or("[anonymous]")
                .to_owned()
                .into_boxed_str(),
        })
        .collect::<Vec<_>>();
    drop(maps);
    drop(fork_safe_ranges);

    // One baseline/final pair proves that all successful allocations produced
    // exactly three authenticated controls and no extra mapping resources.
    let baseline = read_runtime_maps()?;
    let mut arenas = Vec::new();
    let source_count = sources.len();
    let selected_count = source_count.min(MAX_PREPARED_ARENAS);
    let mut selected_ordinal = 0;
    for (source_index, source) in sources.into_iter().enumerate() {
        if selected_ordinal == selected_count {
            break;
        }
        if sampled_source_index(selected_ordinal, source_count, selected_count)
            != Some(source_index)
        {
            continue;
        }
        selected_ordinal += 1;
        // Shared, writable, or execute-only pages remain executable via
        // fallback, but must never receive a live inline patch.
        let Ok(arena) = TrampolineArena::allocate_near(source.start, ARENA_SLOTS) else {
            continue;
        };
        let writable_range = arena.writable_range();
        let executable_range = arena.executable_range();
        let reservation_range = arena.reservation_range();
        arenas.push(RuntimeArena {
            mapping_start: source.start,
            mapping_end: source.end,
            mapping_name: source.name,
            writable_start: writable_range.start as u64,
            writable_end: writable_range.end as u64,
            executable_start: executable_range.start as u64,
            executable_end: executable_range.end as u64,
            reservation_start: reservation_range.start as u64,
            reservation_end: reservation_range.end as u64,
            source_valid: AtomicBool::new(true),
            arena,
        });
    }
    debug_assert_eq!(selected_ordinal, selected_count);
    if arenas.is_empty() {
        return Err(io::Error::other(
            "could not allocate a LiteInst arena near any executable mapping",
        ));
    }
    let final_maps = read_runtime_maps()?;
    validate_prepared_arena_maps(&baseline, &final_maps, &arenas)?;
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

fn claim_lifetime_patch_attempt(counter: &AtomicUsize) -> bool {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |attempts| {
            (attempts < MAX_LIFETIME_PATCH_ATTEMPTS).then_some(attempts + 1)
        })
        .is_ok()
}

fn run_bounded_patch_attempts<T>(
    counter: &AtomicUsize,
    mut attempt: impl FnMut() -> Result<T, TrampolineError>,
) -> Result<T, InstallSiteError> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        if !claim_lifetime_patch_attempt(counter) {
            return Err(exhausted_site_install(
                "process lifetime patch-attempt budget is exhausted",
            ));
        }
        match attempt() {
            Ok(installed) => return Ok(installed),
            Err(TrampolineError::Patch(PatchError::GuardByteConflict { .. })) if attempts < 16 => {
                continue;
            }
            Err(TrampolineError::ArenaFull) => {
                return Err(exhausted_site_install(
                    "prepared trampoline arena is permanently full",
                ));
            }
            Err(_) => {
                return Err(InstallSiteError::Failed(
                    "trampoline planning, binding, or publication failed",
                ));
            }
        }
    }
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

fn mark_site_range_stale_in(sites: &[SiteSlot], start: u64, len: u64, replacement_end: u64) {
    let Some(end) = start.checked_add(len) else {
        mark_all_sites_stale_in(sites);
        return;
    };
    for site in sites {
        let address = site.address.load(Ordering::Acquire);
        if address != 0
            && address
                .checked_add(liteinst2::patcher::WORD_PATCH_BYTES as u64)
                .is_none_or(|site_end| start < site_end && address < end)
        {
            mark_site_generation_stale(site, replacement_end);
        }
    }
}

fn mark_all_sites_stale_in(sites: &[SiteSlot]) {
    for site in sites {
        mark_site_generation_stale(site, 0);
    }
}

fn madvise_preserves_source_generation(advice: u64) -> bool {
    matches!(
        advice as u32 as i32,
        0 | 1 | 2 | 3 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 19 | 20 | 21 | 22 | 23 | 25
    )
}

fn invalidate_arena_source_span(span: MappingPageSpan) {
    let Some(arenas) = ARENAS.get() else {
        return;
    };
    match span {
        MappingPageSpan::Range { start, end } => {
            for arena in arenas {
                if start < arena.mapping_end && arena.mapping_start < end {
                    arena.source_valid.store(false, Ordering::Release);
                }
            }
        }
        MappingPageSpan::Invalid => {
            for arena in arenas {
                arena.source_valid.store(false, Ordering::Release);
            }
        }
        MappingPageSpan::Empty => {}
    }
}

fn mark_site_generation_stale(site: &SiteSlot, replacement_end: u64) {
    site.mapping_end.store(replacement_end, Ordering::Release);
    let mut state = site.state.load(Ordering::Acquire);
    while matches!(state, SITE_ACTIVE | SITE_FALLBACK) {
        match site
            .state
            .compare_exchange(state, SITE_STALE, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return,
            Err(observed) => state = observed,
        }
    }
}

fn mark_original_instruction_stale(site: &SiteSlot, instruction: u16) {
    if instruction == 0x050f {
        let _ = site.state.compare_exchange(
            SITE_ACTIVE,
            SITE_STALE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

// TODO-HUMAN-REVIEW(PR-127): Review executable mapping-generation tracking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MappingPageSpan {
    Empty,
    Range { start: u64, end: u64 },
    Invalid,
}

fn checked_mapping_page_span(
    start: u64,
    raw_length: u64,
    page_size: u64,
    allow_empty: bool,
) -> MappingPageSpan {
    if page_size == 0 || !page_size.is_power_of_two() || !start.is_multiple_of(page_size) {
        return MappingPageSpan::Invalid;
    }
    if raw_length == 0 {
        return if allow_empty {
            MappingPageSpan::Empty
        } else {
            MappingPageSpan::Invalid
        };
    }
    let Some(effective_length) = raw_length
        .checked_add(page_size - 1)
        .map(|length| length & !(page_size - 1))
    else {
        return MappingPageSpan::Invalid;
    };
    let Some(end) = start.checked_add(effective_length) else {
        return MappingPageSpan::Invalid;
    };
    MappingPageSpan::Range { start, end }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MremapEffectSpans {
    source: MappingPageSpan,
    destination: MappingPageSpan,
    clones_shared_mapping: bool,
}

fn mremap_effect_spans(args: [u64; 6], page_size: u64) -> Result<MremapEffectSpans, ()> {
    let flags = args[3];
    let may_move = flags & libc::MREMAP_MAYMOVE as u64 != 0;
    let fixed = flags & libc::MREMAP_FIXED as u64 != 0;
    let dont_unmap = flags & libc::MREMAP_DONTUNMAP as u64 != 0;
    let allowed_flags = (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED | libc::MREMAP_DONTUNMAP) as u64;
    if page_size == 0
        || !page_size.is_power_of_two()
        || flags & !allowed_flags != 0
        || (fixed && !may_move)
        || (dont_unmap && !may_move)
    {
        return Err(());
    }

    let new_at_source = checked_mapping_page_span(args[0], args[2], page_size, false);
    let MappingPageSpan::Range {
        start: new_source_start,
        end: new_source_end,
    } = new_at_source
    else {
        return Err(());
    };
    let clones_shared_mapping = args[1] == 0;
    if clones_shared_mapping && (!may_move || dont_unmap) {
        return Err(());
    }
    let source = if clones_shared_mapping {
        new_at_source
    } else {
        checked_mapping_page_span(args[0], args[1], page_size, false)
    };
    let MappingPageSpan::Range {
        start: source_start,
        end: source_end,
    } = source
    else {
        return Err(());
    };
    if dont_unmap && source_end - source_start != new_source_end - new_source_start {
        return Err(());
    }

    let requested_destination = if fixed || dont_unmap {
        checked_mapping_page_span(args[4], args[2], page_size, false)
    } else {
        MappingPageSpan::Empty
    };
    if let MappingPageSpan::Range {
        start: destination_start,
        end: destination_end,
    } = requested_destination
    {
        if !clones_shared_mapping
            && source_start < destination_end
            && destination_start < source_end
        {
            return Err(());
        }
    } else if fixed || dont_unmap {
        return Err(());
    }
    let destination = if fixed {
        requested_destination
    } else {
        MappingPageSpan::Empty
    };
    Ok(MremapEffectSpans {
        source,
        destination,
        clones_shared_mapping,
    })
}

#[cfg(test)]
fn mremap_source_page_span(args: [u64; 6], page_size: u64) -> MappingPageSpan {
    mremap_effect_spans(args, page_size)
        .map(|effect| effect.source)
        .unwrap_or(MappingPageSpan::Invalid)
}

#[cfg(test)]
fn mremap_fixed_destination_page_span(args: [u64; 6], page_size: u64) -> MappingPageSpan {
    mremap_effect_spans(args, page_size)
        .map(|effect| effect.destination)
        .unwrap_or(MappingPageSpan::Invalid)
}

fn remap_file_pages_span(args: [u64; 6], page_size: u64) -> MappingPageSpan {
    if page_size == 0 || !page_size.is_power_of_two() || args[2] != 0 || args[4] != 0 {
        return MappingPageSpan::Invalid;
    }
    let start = args[0] & !(page_size - 1);
    let length = args[1] & !(page_size - 1);
    let Some(end) = start.checked_add(length).filter(|end| *end > start) else {
        return MappingPageSpan::Invalid;
    };
    let pages = length / page_size;
    if args[3].checked_add(pages).is_none() {
        return MappingPageSpan::Invalid;
    }
    MappingPageSpan::Range { start, end }
}

fn page_ceil(value: u64, page_size: u64) -> Option<u64> {
    value
        .checked_add(page_size.checked_sub(1)?)
        .map(|value| value & !(page_size - 1))
}

fn brk_shrink_page_span(
    start_break: u64,
    current_break: u64,
    requested_break: u64,
    page_size: u64,
) -> MappingPageSpan {
    if page_size == 0
        || !page_size.is_power_of_two()
        || start_break == 0
        || current_break < start_break
    {
        return MappingPageSpan::Invalid;
    }
    if requested_break == 0 || requested_break < start_break || requested_break >= current_break {
        return MappingPageSpan::Empty;
    }
    let (Some(start), Some(end)) = (
        page_ceil(requested_break, page_size),
        page_ceil(current_break, page_size),
    ) else {
        return MappingPageSpan::Invalid;
    };
    if start < end {
        MappingPageSpan::Range { start, end }
    } else {
        MappingPageSpan::Empty
    }
}

fn all_runtime_control_span() -> MappingPageSpan {
    MappingPageSpan::Range {
        start: 0,
        end: u64::MAX,
    }
}

fn ioctl_is_userfaultfd(request: u64) -> bool {
    request >> 8 & 0xff == UFFD_IOCTL_TYPE
}

fn prctl_mutates_program_break(args: [u64; 6]) -> bool {
    u64::from(args[0] as u32) == PR_SET_MM
        && matches!(
            u64::from(args[1] as u32),
            PR_SET_MM_START_BRK | PR_SET_MM_BRK | PR_SET_MM_MAP
        )
}

fn mapping_span_mutates_runtime_control(
    span: MappingPageSpan,
    protect_sites: bool,
    requested_protection: Option<i32>,
    sites: &[SiteSlot],
    arena_mutates: &mut impl FnMut(u64, u64, Option<i32>) -> bool,
) -> bool {
    let MappingPageSpan::Range { start, end } = span else {
        return false;
    };
    protect_sites
        && sites.iter().any(|site| {
            if site.hook.load(Ordering::Acquire).is_null() {
                return false;
            }
            let address = site.address.load(Ordering::Acquire);
            address
                .checked_add(liteinst2::patcher::WORD_PATCH_BYTES as u64)
                .is_none_or(|site_end| {
                    start < site_end
                        && address < end
                        && requested_protection != Some(libc::PROT_READ | libc::PROT_EXEC)
                })
        })
        || arena_mutates(start, end, requested_protection)
}

fn mapping_mutates_runtime_control_with(
    number: i64,
    args: [u64; 6],
    page_size: u64,
    sites: &[SiteSlot],
    mut arena_mutates: impl FnMut(u64, u64, Option<i32>) -> bool,
) -> bool {
    if page_size == 0 || !page_size.is_power_of_two() {
        return matches!(
            number,
            libc::SYS_mmap
                | libc::SYS_munmap
                | libc::SYS_mprotect
                | libc::SYS_pkey_mprotect
                | libc::SYS_mremap
                | libc::SYS_madvise
                | libc::SYS_remap_file_pages
                | libc::SYS_process_madvise
                | libc::SYS_shmat
                | libc::SYS_shmdt
                | libc::SYS_brk
                | libc::SYS_io_uring_setup
                | libc::SYS_io_uring_enter
                | libc::SYS_io_uring_register
                | libc::SYS_userfaultfd
                | libc::SYS_ioctl
                | libc::SYS_prctl
        );
    }
    match number {
        // NOREPLACE cannot destroy an arena even when a caller also supplies
        // MAP_FIXED; Linux must retain ownership of its native EEXIST result.
        libc::SYS_mmap
            if args[3] & libc::MAP_FIXED as u64 != 0
                && args[3] & libc::MAP_FIXED_NOREPLACE as u64 == 0 =>
        {
            mapping_span_mutates_runtime_control(
                checked_mapping_page_span(args[0], args[1], page_size, false),
                false,
                None,
                sites,
                &mut arena_mutates,
            )
        }
        libc::SYS_munmap => mapping_span_mutates_runtime_control(
            checked_mapping_page_span(args[0], args[1], page_size, false),
            false,
            None,
            sites,
            &mut arena_mutates,
        ),
        libc::SYS_mprotect | libc::SYS_pkey_mprotect => {
            let requested_protection =
                (number == libc::SYS_mprotect || args[3] == 0).then_some(args[2] as i32);
            mapping_span_mutates_runtime_control(
                checked_mapping_page_span(args[0], args[1], page_size, true),
                true,
                requested_protection,
                sites,
                &mut arena_mutates,
            )
        }
        libc::SYS_mremap => {
            let Ok(effect) = mremap_effect_spans(args, page_size) else {
                return false;
            };
            if mapping_span_mutates_runtime_control(
                effect.source,
                !effect.clones_shared_mapping,
                None,
                sites,
                &mut arena_mutates,
            ) {
                return true;
            }
            mapping_span_mutates_runtime_control(
                effect.destination,
                true,
                None,
                sites,
                &mut arena_mutates,
            )
        }
        // Linux receives `behavior` as a C int, so classify its low 32 bits.
        // Byte- and fork-preserving advice retains the current controls.
        libc::SYS_madvise if madvise_preserves_source_generation(args[2]) => false,
        // DONTNEED/FREE/REMOVE destroy bytes, DONTFORK/WIPEONFORK break
        // inherited hooks, GUARD_INSTALL makes them inaccessible, and unknown
        // future advice must fail closed on overlap.
        libc::SYS_madvise => mapping_span_mutates_runtime_control(
            checked_mapping_page_span(args[0], args[1], page_size, true),
            true,
            None,
            sites,
            &mut arena_mutates,
        ),
        // The legacy syscall masks both address and length down before its
        // overflow checks. In particular, an unaligned address can still
        // replace the preceding protected page, while a sub-page size is the
        // kernel's native EINVAL and must not become a LiteInst refusal.
        libc::SYS_remap_file_pages => mapping_span_mutates_runtime_control(
            remap_file_pages_span(args, page_size),
            true,
            None,
            sites,
            &mut arena_mutates,
        ),
        // process_madvise describes ranges through caller memory and io_uring
        // can submit IORING_OP_MADVISE without another syscall boundary. Neither
        // can be copied and authenticated safely in the signal handler. SHM_REMAP
        // omits the segment length. Refuse these while any runtime control range
        // exists. Private executable heap mappings are intentionally supported;
        // their shrink exposure is handled by the exact brk guard below. Initial
        // shared SysV sources are excluded, and ordinary shmat cannot replace an
        // existing range without SHM_REMAP.
        libc::SYS_process_madvise
        | libc::SYS_io_uring_setup
        | libc::SYS_io_uring_enter
        | libc::SYS_io_uring_register
        | libc::SYS_userfaultfd => mapping_span_mutates_runtime_control(
            all_runtime_control_span(),
            true,
            None,
            sites,
            &mut arena_mutates,
        ),
        libc::SYS_shmat if args[2] & libc::SHM_REMAP as u64 != 0 => {
            mapping_span_mutates_runtime_control(
                all_runtime_control_span(),
                true,
                None,
                sites,
                &mut arena_mutates,
            )
        }
        libc::SYS_ioctl if ioctl_is_userfaultfd(args[1]) => mapping_span_mutates_runtime_control(
            all_runtime_control_span(),
            true,
            None,
            sites,
            &mut arena_mutates,
        ),
        libc::SYS_prctl if prctl_mutates_program_break(args) => {
            mapping_span_mutates_runtime_control(
                all_runtime_control_span(),
                true,
                None,
                sites,
                &mut arena_mutates,
            )
        }
        libc::SYS_brk => {
            let span = brk_shrink_page_span(
                PROGRAM_BREAK_START.load(Ordering::Acquire),
                PROGRAM_BREAK.load(Ordering::Acquire),
                args[0],
                page_size,
            );
            if matches!(span, MappingPageSpan::Invalid) {
                return sites
                    .iter()
                    .any(|site| !site.hook.load(Ordering::Acquire).is_null());
            }
            mapping_span_mutates_runtime_control(span, true, None, sites, &mut |_, _, _| false)
        }
        _ => false,
    }
}

fn observe_brk_result_in(
    event: &SyscallEvent,
    page_size: u64,
    start_break: u64,
    old_break: u64,
    sites: &[SiteSlot],
) -> Result<u64, ()> {
    if event.result < 0 || page_size == 0 || !page_size.is_power_of_two() {
        return Err(());
    }
    let returned = event.result as u64;
    if returned < start_break
        || (returned != old_break && returned != event.args[0])
        || old_break < start_break
    {
        return Err(());
    }
    if returned == old_break {
        return Ok(old_break);
    }
    let old_page = page_ceil(old_break, page_size).ok_or(())?;
    let new_page = page_ceil(returned, page_size).ok_or(())?;
    if old_page < new_page {
        mark_site_range_stale_in(sites, old_page, new_page - old_page, new_page);
    } else if new_page < old_page {
        mark_site_range_stale_in(sites, new_page, old_page - new_page, 0);
    }
    Ok(returned)
}

fn observe_mapping_generation_in(event: &SyscallEvent, page_size: u64, sites: &[SiteSlot]) {
    if event.result < 0 {
        return;
    }
    match event.number {
        // AUTONOMOUS-BOT-IMPLEMENTED
        libc::SYS_mmap => {
            let start = event.result as u64;
            match checked_mapping_page_span(start, event.args[1], page_size, false) {
                MappingPageSpan::Range { start, end } => {
                    mark_site_range_stale_in(sites, start, end - start, end);
                }
                MappingPageSpan::Empty | MappingPageSpan::Invalid => {
                    mark_all_sites_stale_in(sites);
                }
            }
        }
        // AUTONOMOUS-BOT-IMPLEMENTED
        libc::SYS_munmap => {
            match checked_mapping_page_span(event.args[0], event.args[1], page_size, false) {
                MappingPageSpan::Range { start, end } => {
                    mark_site_range_stale_in(sites, start, end - start, 0);
                }
                MappingPageSpan::Empty | MappingPageSpan::Invalid => {
                    mark_all_sites_stale_in(sites);
                }
            }
        }
        // AUTONOMOUS-BOT-IMPLEMENTED
        libc::SYS_mremap => {
            // Linux rounds both lengths to a hugetlb source VMA's huge-page
            // size, which is not available in this allocation-free signal
            // path. Preserve native nonfixed mremap semantics, but retire every
            // base-page generation rather than under-cover a moved huge tail.
            mark_all_sites_stale_in(sites);
        }
        libc::SYS_madvise if !madvise_preserves_source_generation(event.args[2]) => {
            match checked_mapping_page_span(event.args[0], event.args[1], page_size, true) {
                MappingPageSpan::Range { start, end } => {
                    mark_site_range_stale_in(sites, start, end - start, 0);
                }
                MappingPageSpan::Empty => {}
                MappingPageSpan::Invalid => mark_all_sites_stale_in(sites),
            }
        }
        libc::SYS_remap_file_pages => match remap_file_pages_span(event.args, page_size) {
            MappingPageSpan::Range { start, end } => {
                mark_site_range_stale_in(sites, start, end - start, 0);
            }
            MappingPageSpan::Empty => {}
            MappingPageSpan::Invalid => mark_all_sites_stale_in(sites),
        },
        _ => {}
    }
}

fn observe_arena_source_generation_with(
    event: &SyscallEvent,
    page_size: u64,
    mut invalidate: impl FnMut(MappingPageSpan),
) {
    if event.result < 0 {
        return;
    }
    match event.number {
        libc::SYS_mmap => invalidate(checked_mapping_page_span(
            event.result as u64,
            event.args[1],
            page_size,
            false,
        )),
        libc::SYS_munmap => invalidate(checked_mapping_page_span(
            event.args[0],
            event.args[1],
            page_size,
            false,
        )),
        libc::SYS_mprotect | libc::SYS_pkey_mprotect
            if event.args[2] as i32 != (libc::PROT_READ | libc::PROT_EXEC)
                || (event.number == libc::SYS_pkey_mprotect && event.args[3] != 0) =>
        {
            invalidate(checked_mapping_page_span(
                event.args[0],
                event.args[1],
                page_size,
                true,
            ));
        }
        libc::SYS_mremap => {
            invalidate(MappingPageSpan::Invalid);
        }
        libc::SYS_madvise if !madvise_preserves_source_generation(event.args[2]) => {
            invalidate(checked_mapping_page_span(
                event.args[0],
                event.args[1],
                page_size,
                true,
            ));
        }
        libc::SYS_remap_file_pages => {
            invalidate(remap_file_pages_span(event.args, page_size));
        }
        _ => {}
    }
}

fn observe_arena_source_generation(event: &SyscallEvent, page_size: u64) {
    observe_arena_source_generation_with(event, page_size, invalidate_arena_source_span);
}

fn observe_mapping_generation(event: &SyscallEvent) {
    let Some(sites) = SITES.get() else {
        return;
    };
    let page_size = PAGE_SIZE.load(Ordering::Acquire);
    if event.number == libc::SYS_brk {
        let start = PROGRAM_BREAK_START.load(Ordering::Acquire);
        let old = PROGRAM_BREAK.load(Ordering::Acquire);
        match observe_brk_result_in(event, page_size, start, old, sites) {
            Ok(current) => {
                if let (Some(old_page), Some(new_page)) =
                    (page_ceil(old, page_size), page_ceil(current, page_size))
                {
                    invalidate_arena_source_span(if old_page < new_page {
                        MappingPageSpan::Range {
                            start: old_page,
                            end: new_page,
                        }
                    } else if new_page < old_page {
                        MappingPageSpan::Range {
                            start: new_page,
                            end: old_page,
                        }
                    } else {
                        MappingPageSpan::Empty
                    });
                }
                PROGRAM_BREAK.store(current, Ordering::Release)
            }
            Err(()) => unsafe { exit_now(125) },
        }
        return;
    }
    observe_mapping_generation_in(event, page_size, sites);
    observe_arena_source_generation(event, page_size);
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
    FALLBACK_REFUSALS.reset();
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

pub(crate) fn record_fork_child_dispatch(
    event: &SyscallEvent,
    stats: crate::stats::GuestStatsHooks,
) {
    match event.dispatch {
        SyscallDispatch::InstalledHook => {
            if let Some(site) = find_site(event.instruction_pointer) {
                site.hook_count.fetch_add(1, Ordering::Relaxed);
            }
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
        entry.source_valid.load(Ordering::Acquire)
            && entry.mapping_start <= address
            && address < entry.mapping_end
            && entry.arena.can_reach(address)
    })
}

fn read_self_bytes(address: u64, output: &mut [u8]) -> usize {
    if output.is_empty() {
        return 0;
    }
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    if pid <= 0 {
        return 0;
    }
    let local = libc::iovec {
        iov_base: output.as_mut_ptr().cast(),
        iov_len: output.len(),
    };
    let remote = libc::iovec {
        iov_base: address as usize as *mut libc::c_void,
        iov_len: output.len(),
    };
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
    usize::try_from(read)
        .ok()
        .filter(|amount| *amount <= output.len())
        .unwrap_or(0)
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

#[derive(Debug)]
enum InstallSiteError {
    Exhausted(&'static str),
    Errno(i32),
    Failed(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallProtectionRestoreStage {
    WritableOpenFailure,
    PlanningFailure,
    ActivationFailure,
    ActivatedRollbackReopen,
    ActivatedRollbackDeactivation,
    ActivatedRollbackFinalRx,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InstallProtectionRestoreFailure {
    stage: InstallProtectionRestoreStage,
    errno: i32,
}

fn classify_install_protection_restore(
    stage: InstallProtectionRestoreStage,
    result: io::Result<()>,
) -> Result<(), InstallProtectionRestoreFailure> {
    result.map_err(|error| InstallProtectionRestoreFailure {
        stage,
        errno: error.raw_os_error().unwrap_or(libc::EIO),
    })
}

fn terminate_install_protection_restore(failure: InstallProtectionRestoreFailure) -> ! {
    let marker = match failure.stage {
        InstallProtectionRestoreStage::WritableOpenFailure => {
            b"site-rx-restore-after-writable-open-failed".as_slice()
        }
        InstallProtectionRestoreStage::PlanningFailure => {
            b"site-rx-restore-after-planning-failure-failed".as_slice()
        }
        InstallProtectionRestoreStage::ActivationFailure => {
            b"site-rx-restore-after-activation-failure-failed".as_slice()
        }
        InstallProtectionRestoreStage::ActivatedRollbackReopen => {
            b"site-rwx-reopen-for-activation-rollback-failed".as_slice()
        }
        InstallProtectionRestoreStage::ActivatedRollbackDeactivation => {
            b"site-deactivate-after-rx-restore-failed".as_slice()
        }
        InstallProtectionRestoreStage::ActivatedRollbackFinalRx => {
            b"site-final-rx-after-activation-rollback-failed".as_slice()
        }
    };
    emit_in_guest_stage(marker);
    unsafe { exit_now(126) }
}

#[derive(Debug)]
enum InstallProtectionTransitionFailure {
    Primary(io::Error),
    Cleanup(InstallProtectionRestoreFailure),
}

fn open_install_source_with(
    mut protect: impl FnMut(i32) -> io::Result<()>,
) -> Result<(), InstallProtectionTransitionFailure> {
    let writable = libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC;
    let readonly = libc::PROT_READ | libc::PROT_EXEC;
    match protect(writable) {
        Ok(()) => Ok(()),
        Err(primary) => match classify_install_protection_restore(
            InstallProtectionRestoreStage::WritableOpenFailure,
            protect(readonly),
        ) {
            Ok(()) => Err(InstallProtectionTransitionFailure::Primary(primary)),
            Err(cleanup) => Err(InstallProtectionTransitionFailure::Cleanup(cleanup)),
        },
    }
}

fn close_activated_install_with(
    mut protect: impl FnMut(i32) -> io::Result<()>,
    deactivate: impl FnOnce() -> Result<(), ()>,
) -> Result<(), InstallProtectionTransitionFailure> {
    let readonly = libc::PROT_READ | libc::PROT_EXEC;
    let writable = libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC;
    let primary = match protect(readonly) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    classify_install_protection_restore(
        InstallProtectionRestoreStage::ActivatedRollbackReopen,
        protect(writable),
    )
    .map_err(InstallProtectionTransitionFailure::Cleanup)?;
    deactivate().map_err(|()| {
        InstallProtectionTransitionFailure::Cleanup(InstallProtectionRestoreFailure {
            stage: InstallProtectionRestoreStage::ActivatedRollbackDeactivation,
            errno: libc::EIO,
        })
    })?;
    classify_install_protection_restore(
        InstallProtectionRestoreStage::ActivatedRollbackFinalRx,
        protect(readonly),
    )
    .map_err(InstallProtectionTransitionFailure::Cleanup)?;
    Err(InstallProtectionTransitionFailure::Primary(primary))
}

unsafe fn restore_install_source_or_exit(address: u64, stage: InstallProtectionRestoreStage) {
    let result = unsafe { set_text_protection(address, libc::PROT_READ | libc::PROT_EXEC) };
    if let Err(failure) = classify_install_protection_restore(stage, result) {
        terminate_install_protection_restore(failure);
    }
}

impl InstallSiteError {
    fn site_state(&self) -> u8 {
        match self {
            Self::Exhausted(_) => SITE_EXHAUSTED,
            Self::Errno(_) | Self::Failed(_) => SITE_FALLBACK,
        }
    }
}

impl From<io::Error> for InstallSiteError {
    fn from(error: io::Error) -> Self {
        Self::Errno(error.raw_os_error().unwrap_or(libc::EIO))
    }
}

impl std::fmt::Display for InstallSiteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exhausted(message) => formatter.write_str(message),
            Self::Errno(errno) => write!(formatter, "install operation failed with errno {errno}"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

fn record_site_install_failure(site: &SiteSlot, error: &InstallSiteError) {
    site.state.store(error.site_state(), Ordering::Release);
}

fn exhausted_site_install(message: &'static str) -> InstallSiteError {
    InstallSiteError::Exhausted(message)
}

struct InstallGuard;

#[derive(Clone, Copy)]
struct InstallSourceSnapshot<'a> {
    mapping_end: u64,
    bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PatchPublication {
    /// The stopped-tracee helper is the only thread able to reach live code.
    Quiescent,
    /// Other application threads may fetch the site during publication.
    Concurrent,
}

fn validate_host_publication(
    explicit_quiescent: bool,
    requested: PatchPublication,
) -> io::Result<()> {
    if explicit_quiescent && requested != PatchPublication::Quiescent {
        return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    Ok(())
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
        .map_err(|_| io::Error::from_raw_os_error(libc::EBUSY))
}

fn validate_host_callback_saved_state_layout(
    layout: SavedExtendedStateLayout,
) -> Result<(), InstallSiteError> {
    let stack = HOST_CALLBACK_STACK.get().ok_or(InstallSiteError::Failed(
        "host callback stack is unavailable before hook activation",
    ))?;
    if !host_callback_stack_accepts_layout(stack, layout) {
        return Err(InstallSiteError::Failed(
            "trampoline XSTATE layout does not fit the authenticated callback stack",
        ));
    }
    Ok(())
}

unsafe fn install_site_hook(
    address: u64,
    slot: &'static SiteSlot,
    callback: liteinst2::trampoline::HookCallback,
    publication: PatchPublication,
    expected_instruction: &[u8],
    manage_protection: bool,
    source_snapshot: Option<InstallSourceSnapshot<'_>>,
) -> Result<HostInstallResult, InstallSiteError> {
    if let Err(error) =
        validate_host_publication(EXPLICIT_HOST_QUIESCENT.load(Ordering::Acquire), publication)
    {
        let error = InstallSiteError::from(error);
        record_site_install_failure(slot, &error);
        return Err(error);
    }
    // Concurrent signal paths block nested handlers before acquiring the
    // process-wide install owner. Declaration order is deliberate: the install
    // guard drops first; the allocation scope then clears allocator TLS while
    // leaving signals blocked. The enclosing rt_sigreturn restores the exact
    // signal-frame mask only after every Rust guard has gone away.
    let _concurrent_allocation_scope = if publication == PatchPublication::Concurrent {
        // SAFETY: Concurrent publication is reachable only from the kernel's
        // SIGSYS and SIGSEGV handlers; every returning path uses rt_sigreturn.
        match unsafe { crate::patch_alloc::enter_signal_handler() } {
            Ok(scope) => Some(scope),
            Err(error) => {
                let error = InstallSiteError::from(error);
                record_site_install_failure(slot, &error);
                return Err(error);
            }
        }
    } else {
        None
    };
    let _install_guard = match lock_installation() {
        Ok(guard) => guard,
        Err(error) => {
            let error = InstallSiteError::from(error);
            record_site_install_failure(slot, &error);
            return Err(error);
        }
    };
    let _quiescent_allocation_scope = if publication == PatchPublication::Quiescent {
        match crate::patch_alloc::enter_quiescent_install() {
            Some(scope) => Some(scope),
            None => {
                let error = InstallSiteError::Errno(libc::EALREADY);
                record_site_install_failure(slot, &error);
                return Err(error);
            }
        }
    } else {
        None
    };
    let result = unsafe {
        install_site_hook_inner(
            address,
            slot,
            callback,
            publication,
            expected_instruction,
            manage_protection,
            source_snapshot,
        )
    };
    if let Err(error) = &result {
        // Inner install-owned values and any failed InstalledHook have already
        // dropped. Publish the terminal state while INSTALL_HELD is owned and
        // concurrent signals are still blocked, before rt_sigreturn restores
        // the signal frame's mask.
        record_site_install_failure(slot, error);
    }
    result
}

unsafe fn install_site_hook_inner(
    address: u64,
    slot: &'static SiteSlot,
    callback: liteinst2::trampoline::HookCallback,
    publication: PatchPublication,
    expected_instruction: &[u8],
    manage_protection: bool,
    source_snapshot: Option<InstallSourceSnapshot<'_>>,
) -> Result<HostInstallResult, InstallSiteError> {
    if !crate::patch_alloc::patch_install_capacity_available() {
        return Err(exhausted_site_install(
            "reusable patch heap lacks one complete install headroom",
        ));
    }
    let arena = arena_for(address).ok_or_else(|| {
        exhausted_site_install("no reachable prepared LiteInst arena for syscall site")
    })?;
    let mut mapping_end = source_snapshot
        .map(|snapshot| snapshot.mapping_end)
        .unwrap_or_else(|| slot.mapping_end.load(Ordering::Acquire));
    if mapping_end <= address && source_snapshot.is_none() {
        mapping_end = arena.mapping_end;
    }
    if mapping_end <= address || mapping_end > arena.mapping_end {
        return Err(InstallSiteError::Failed(
            "authenticated source mapping end is outside its prepared arena source",
        ));
    }
    slot.mapping_end.store(mapping_end, Ordering::Release);
    let available = usize::try_from(mapping_end - address)
        .unwrap_or(0)
        .min(PATCH_SNAPSHOT_BYTES);
    if available < liteinst2::patcher::WORD_PATCH_BYTES {
        return Err(InstallSiteError::Failed(
            "syscall site is too close to its executable mapping end",
        ));
    }
    let mut candidate_bytes = [0_u8; PATCH_SNAPSHOT_BYTES];
    let candidate = if let Some(snapshot) = source_snapshot {
        if snapshot.bytes.len() < liteinst2::patcher::WORD_PATCH_BYTES
            || snapshot.bytes.len() > available
        {
            return Err(InstallSiteError::Failed(
                "authenticated source snapshot has invalid length",
            ));
        }
        snapshot.bytes
    } else {
        let read = read_self_bytes(address, &mut candidate_bytes[..available]);
        if read < liteinst2::patcher::WORD_PATCH_BYTES {
            return Err(InstallSiteError::Failed(
                "fault-safe source read did not cover one complete patch word",
            ));
        }
        &candidate_bytes[..read]
    };
    if candidate.get(..expected_instruction.len()) != Some(expected_instruction) {
        return Err(InstallSiteError::Failed(
            "fault site does not contain the expected x86-64 instruction",
        ));
    }
    let scanner = InstructionScanner::default();
    let scan = scanner
        .scan_prefix(candidate, address, liteinst2::patcher::WORD_PATCH_BYTES)
        .map_err(|_| InstallSiteError::Failed("instruction-prefix scan failed"))?;
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
        match open_install_source_with(|protection| unsafe {
            set_text_protection(address, protection)
        }) {
            Ok(()) => {}
            Err(InstallProtectionTransitionFailure::Primary(error)) => {
                return Err(error.into());
            }
            Err(InstallProtectionTransitionFailure::Cleanup(failure)) => {
                terminate_install_protection_restore(failure);
            }
        }
    }
    // A guarded cross-line plan rejects a trampoline displacement containing
    // the temporary INT3 byte at another instruction head. Arena slots have
    // distinct rel32 displacements, so retry a bounded number of fresh slots;
    // this changes no guest bytes and preserves the same patch mechanism.
    let installed = run_bounded_patch_attempts(&LIFETIME_PATCH_ATTEMPTS, || {
        let site = HookSite::new(
            &scanner,
            &scan,
            code,
            address,
            address,
            address as usize as *mut u8,
        );
        match publication {
            PatchPublication::Quiescent => unsafe {
                InstalledHook::install_replacing_first_in_arena_quiescent_with_ptrace_stops(
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
        }
    });
    let installed = match installed {
        Ok(installed) => installed,
        Err(error) => {
            if manage_protection {
                unsafe {
                    restore_install_source_or_exit(
                        address,
                        InstallProtectionRestoreStage::PlanningFailure,
                    );
                }
            }
            return Err(error);
        }
    };
    if publication == PatchPublication::Quiescent
        && let Err(error) = validate_host_callback_saved_state_layout(
            installed.trampoline().layout().saved_extended_state,
        )
    {
        if manage_protection {
            unsafe {
                restore_install_source_or_exit(
                    address,
                    InstallProtectionRestoreStage::PlanningFailure,
                );
            }
        }
        return Err(error);
    }
    let result = match complete_host_install_result(
        address,
        &installed,
        arena,
        instruction_len as u64,
        straddle_prefix as u64,
        publication,
    ) {
        Ok(result) => result,
        Err(error) => {
            if manage_protection {
                unsafe {
                    restore_install_source_or_exit(
                        address,
                        InstallProtectionRestoreStage::PlanningFailure,
                    );
                }
            }
            return Err(error);
        }
    };
    let activation = match publication {
        PatchPublication::Concurrent => installed.activate(),
        // SAFETY: the ptrace controller serializes this helper while every
        // other tracee thread is stopped. Hermit likewise schedules only one
        // guest thread at a time, so no other thread can fetch the site.
        PatchPublication::Quiescent => unsafe { installed.activate_quiescent() },
    };
    if activation.is_err() {
        if manage_protection {
            unsafe {
                restore_install_source_or_exit(
                    address,
                    InstallProtectionRestoreStage::ActivationFailure,
                );
            }
        }
        return Err(InstallSiteError::Failed("trampoline activation failed"));
    }
    if manage_protection {
        let closed = close_activated_install_with(
            |protection| unsafe { set_text_protection(address, protection) },
            || {
                let result = match publication {
                    PatchPublication::Concurrent => installed.deactivate(),
                    // SAFETY: the same caller-owned quiescence used for the
                    // activation remains in force until this function returns.
                    PatchPublication::Quiescent => unsafe { installed.deactivate_quiescent() },
                };
                match result {
                    Ok(true) => Ok(()),
                    Ok(false) | Err(_) => Err(()),
                }
            },
        );
        match closed {
            Ok(()) => {}
            Err(InstallProtectionTransitionFailure::Primary(error)) => {
                return Err(error.into());
            }
            Err(InstallProtectionTransitionFailure::Cleanup(failure)) => {
                terminate_install_protection_restore(failure);
            }
        }
    }

    let installed = Box::into_raw(Box::new(installed));
    slot.hook.store(installed, Ordering::Release);
    slot.instruction_len
        .store(instruction_len as u8, Ordering::Release);
    slot.straddle_prefix
        .store(straddle_prefix as u8, Ordering::Release);
    slot.state.store(SITE_ACTIVE, Ordering::Release);
    Ok(result)
}

fn host_program_counter_mappings(
    installed: &InstalledHook,
) -> Result<(u64, [HostProgramCounterMapping; HOST_INSTALL_PC_MAPPINGS]), InstallSiteError> {
    let source = installed.trampoline().program_counter_mappings();
    if source.is_empty() || source.len() > HOST_INSTALL_PC_MAPPINGS {
        return Err(InstallSiteError::Failed(
            "trampoline program-counter map exceeds the stopped-helper ABI",
        ));
    }
    let mut output = [HostProgramCounterMapping::default(); HOST_INSTALL_PC_MAPPINGS];
    for (destination, mapping) in output.iter_mut().zip(source) {
        *destination = HostProgramCounterMapping {
            generated_start: mapping.generated_start(),
            generated_end: mapping.generated_end(),
            logical_address: mapping.logical_address(),
        };
    }
    Ok((source.len() as u64, output))
}

fn host_saved_xstate_components(
    layout: SavedExtendedStateLayout,
) -> (
    u64,
    [HostSavedXstateComponent; SAVED_EXTENDED_STATE_COMPONENT_CAPACITY],
) {
    let source = layout.components();
    debug_assert!(source.len() <= SAVED_EXTENDED_STATE_COMPONENT_CAPACITY);
    let mut output = [HostSavedXstateComponent::default(); SAVED_EXTENDED_STATE_COMPONENT_CAPACITY];
    for (destination, component) in output.iter_mut().zip(source) {
        *destination = host_saved_xstate_component(*component);
    }
    (source.len() as u64, output)
}

fn host_saved_xstate_component(component: SavedExtendedStateComponent) -> HostSavedXstateComponent {
    HostSavedXstateComponent {
        xfeature: component.xfeature(),
        offset: component.offset(),
        size: component.size(),
    }
}

fn host_saved_xstate_publication(layout: SavedExtendedStateLayout) -> HostSavedXstatePublication {
    let (component_count, components) = host_saved_xstate_components(layout);
    HostSavedXstatePublication {
        allocation_len: layout.len(),
        mask: layout.mask(),
        format: layout.format().raw(),
        image_len: layout.image_len(),
        component_count,
        components,
    }
}

fn complete_host_install_result(
    address: u64,
    installed: &InstalledHook,
    arena: &RuntimeArena,
    instruction_len: u64,
    straddle_prefix: u64,
    publication: PatchPublication,
) -> Result<HostInstallResult, InstallSiteError> {
    let (ptrace_entry_stop_rip, ptrace_completion_stop_rip) =
        match publication {
            PatchPublication::Quiescent => (
                installed
                    .trampoline()
                    .ptrace_entry_stop_rip()
                    .ok_or(InstallSiteError::Failed(
                        "quiescent trampoline omitted its entry stop",
                    ))?,
                installed.trampoline().ptrace_completion_stop_rip().ok_or(
                    InstallSiteError::Failed("quiescent trampoline omitted its completion stop"),
                )?,
            ),
            PatchPublication::Concurrent => {
                if installed.trampoline().ptrace_entry_stop_rip().is_some()
                    || installed
                        .trampoline()
                        .ptrace_completion_stop_rip()
                        .is_some()
                {
                    return Err(InstallSiteError::Failed(
                        "concurrent trampoline unexpectedly emitted ptrace stops",
                    ));
                }
                (0, 0)
            }
        };
    let (program_counter_count, program_counters) = host_program_counter_mappings(installed)?;
    let saved_xstate =
        host_saved_xstate_publication(installed.trampoline().layout().saved_extended_state);
    Ok(HostInstallResult {
        version: HOST_INSTALL_RESULT_VERSION,
        site_start: address,
        site_len: liteinst2::patcher::WORD_PATCH_BYTES as u64,
        ptrace_entry_stop_rip,
        ptrace_completion_stop_rip,
        relocated_tail: installed.trampoline().relocated_tail_address(),
        trampoline_start: installed.trampoline().address(),
        trampoline_len: installed.trampoline().allocation_len() as u64,
        trampoline_code_len: installed.trampoline().code_len() as u64,
        arena_writable_start: arena.writable_start,
        arena_writable_len: arena.writable_end - arena.writable_start,
        arena_executable_start: arena.executable_start,
        arena_executable_len: arena.executable_end - arena.executable_start,
        instruction_len,
        straddle_prefix,
        program_counter_count,
        program_counters,
        complete: 1,
        saved_xstate_len: saved_xstate.allocation_len,
        saved_xstate_mask: saved_xstate.mask,
        saved_xstate_format: saved_xstate.format,
        saved_xstate_image_len: saved_xstate.image_len,
        saved_xstate_component_count: saved_xstate.component_count,
        saved_xstate_components: saved_xstate.components,
    })
}

fn install_vdso_sites(sites: &[reverie_ptrace::VdsoSyscallSite]) -> io::Result<()> {
    let callbacks = sites
        .iter()
        .map(|site| vdso_callback(site.number))
        .collect::<io::Result<Vec<_>>>()?;
    let mut completed = 0_usize;
    for (site_info, callback) in sites.iter().zip(callbacks) {
        let address = site_info.address;
        let Some((site, claimed)) = claim_site(address) else {
            if completed != 0 {
                emit_in_guest_stage(b"vdso-batch-failed-after-published-site");
                unsafe { exit_now(126) }
            }
            return Err(io::Error::other("LiteInst vDSO site table is full"));
        };
        if !claimed {
            if completed != 0 {
                emit_in_guest_stage(b"vdso-batch-failed-after-published-site");
                unsafe { exit_now(126) }
            }
            return Err(io::Error::other("LiteInst vDSO site was claimed twice"));
        }
        let install = unsafe {
            install_site_hook(
                address,
                site,
                callback,
                PatchPublication::Quiescent,
                &[0x0f, 0x05],
                true,
                None,
            )
        };
        if let Err(error) = install {
            record_site_install_failure(site, &error);
            if completed != 0 {
                emit_in_guest_stage(b"vdso-batch-failed-after-published-site");
                unsafe { exit_now(126) }
            }
            return Err(io::Error::other(format!(
                "failed to install LiteInst vDSO hook: {error}"
            )));
        }
        completed += 1;
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

// TODO-HUMAN-REVIEW(PR-270): Review stopped-tracee patch helper ABI.
fn validate_host_install_request(address: u64, request: &HostInstallRequest) -> Option<usize> {
    let source_len = usize::try_from(request.source_len).ok()?;
    (request.version == HOST_INSTALL_REQUEST_VERSION
        && request.site_start == address
        && (liteinst2::patcher::WORD_PATCH_BYTES..=PATCH_SNAPSHOT_BYTES).contains(&source_len)
        && address
            .checked_add(request.source_len)
            .is_some_and(|end| end <= request.mapping_end)
        && request.source[..2] == [0x0f, 0x05]
        && request.source[source_len..].iter().all(|byte| *byte == 0))
    .then_some(source_len)
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".liteinst_helper")]
#[inline(never)]
unsafe extern "C" fn reverie_liteinst_install_site_for_ptrace_body(address: u64) -> i64 {
    // SAFETY: the ptrace helper is serialized and the controller reads this
    // fixed-size record only after the helper-return trap.
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(HOST_INSTALL_RESULT),
            HostInstallResult::default(),
        );
    }
    // SAFETY: the controller writes and reads back this fixed-size request
    // while every tracee task is stopped, before entering this helper.
    let request = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(HOST_INSTALL_REQUEST)) };
    // Consume the request before validating or touching any patch state. This
    // is replay hygiene; entry authority remains the controller-consumed INT3
    // immediately before this hidden body.
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(HOST_INSTALL_REQUEST),
            HostInstallRequest::default(),
        );
    }
    let Some(source_len) = validate_host_install_request(address, &request) else {
        return -i64::from(libc::EPROTO);
    };
    if let Some(site) = find_site(address) {
        // The controller calls this helper only after observing the original
        // syscall bytes at the address. If a prior generation is still marked
        // active, its mapping was replaced and the old hook is no longer
        // installed. Transition it to STALE so claim_site installs a new hook
        // rather than returning the prior generation's relocated tail. A
        // FALLBACK is already the exact current-generation result and remains
        // sticky until mark_site_range_stale observes a real remap; permanent
        // exhaustion remains sticky for the process lifetime.
        let instruction = u16::from_le_bytes([request.source[0], request.source[1]]);
        mark_original_instruction_stale(site, instruction);
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
                false,
                Some(InstallSourceSnapshot {
                    mapping_end: request.mapping_end,
                    bytes: &request.source[..source_len],
                }),
            )
        } {
            Ok(result) => install_result = Some(result),
            Err(error) => record_site_install_failure(site, &error),
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
            complete_host_install_result(
                address,
                hook,
                arena,
                u64::from(site.instruction_len.load(Ordering::Acquire)),
                u64::from(site.straddle_prefix.load(Ordering::Acquire)),
                PatchPublication::Quiescent,
            )
            .ok()
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
pub(crate) fn prepare_guest_signal_state(
    instructions: InstructionSubscriptions,
) -> io::Result<SignalInstallGuard> {
    let sigsys = 1_u64 << (libc::SIGSYS - 1);
    let sigtrap = 1_u64 << (libc::SIGTRAP - 1);
    let sigsegv = if instructions.cpuid || instructions.rdtsc {
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
        restore_mask: previous_mask & !(sigsys | sigtrap | sigsegv),
    };

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
    Ok(guard)
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-133): Review fault-safe guest signal-action decoding.
pub(crate) fn signal_action_supported(number: i64, args: [u64; 6]) -> bool {
    if number != libc::SYS_rt_sigaction || args[1] == 0 {
        return true;
    }
    // Linux consumes `sig` as a C int. Reject high-word smuggling rather than
    // letting a raw nonreserved value truncate to one of the runtime's signals.
    if args[0] != args[0] as u32 as u64 {
        return false;
    }
    let signal = args[0] as u32 as i32;
    if signal == libc::SIGSYS
        || signal == libc::SIGTRAP
        || (signal == libc::SIGSEGV && INSTRUCTION_SUBSCRIPTIONS.load(Ordering::Acquire) != 0)
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
fn syscall_number_requires_enosys(number: i64) -> bool {
    reverie_preload::dispatch::syscall_number_requires_enosys(number)
}

fn forward_nested_tool_syscall(event: &mut SyscallEvent) {
    if syscall_number_requires_enosys(event.number) {
        event.result = -i64::from(libc::ENOSYS);
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
        || event.number == libc::SYS_rt_sigreturn
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
    } else if !(protect_runtime_mapping_control(event)
        || protect_runtime_control(event)
        || unsafe { protect_coordinator_channel(event) })
    {
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
    saved_xstate: SavedExtendedStateDescriptor,
}

const _: () = assert!(core::mem::offset_of!(HostSyscallFrame, flags) == 0);
const _: () = assert!(core::mem::offset_of!(HostSyscallFrame, rax) == 15 * 8);
const _: () = assert!(core::mem::offset_of!(HostSyscallFrame, rip) == 17 * 8);
const _: () = assert!(core::mem::offset_of!(HostSyscallFrame, saved_xstate) == 18 * 8);
const _: () = assert!(core::mem::size_of::<HostSyscallFrame>() == 22 * 8);

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
            saved_xstate: context.saved_extended_state(),
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
    unsafe { reverie_liteinst_host_syscall_trap_call(&mut frame, context) };
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
    let mut bytes = [0_u8; 3];
    let read = read_self_bytes(address, &mut bytes[..available]);
    match &bytes[..read] {
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
            let mut bytes = [0_u8; 8];
            let read = read_self_bytes(address, &mut bytes[..available]);
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
                &bytes[..read],
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
    // recurse into allocation and publication while the outer callback owns
    // runtime control state. (The arena cursor itself is shared and atomic
    // across fork.) Execute at the private native helper and advance the
    // faulting context instead.
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

    let publication = patch_publication();
    let _concurrent_install_scope = if publication == PatchPublication::Concurrent {
        // SAFETY: this function is the kernel-created SIGSEGV handler and its
        // returning path reaches rt_sigreturn.
        match unsafe { crate::patch_alloc::enter_signal_handler() } {
            Ok(scope) => Some(scope),
            Err(_) => {
                emit_in_guest_stage(b"instruction-sigsegv-mask-signals-failed");
                unsafe { deliver_default_sigsegv() };
            }
        }
    } else {
        None
    };
    let Some((site, claimed)) = claim_site(address) else {
        emit_in_guest_stage(b"instruction-sigsegv-site-table-full");
        unsafe { deliver_default_sigsegv() };
    };
    site.trap_count.fetch_add(1, Ordering::Relaxed);
    if unsafe { set_all_instruction_native(true) }.is_err() {
        emit_in_guest_stage(b"instruction-sigsegv-enable-native-failed");
        unsafe { deliver_default_sigsegv() };
    }
    if claimed {
        if let Err(error) = unsafe {
            install_site_hook(
                address,
                site,
                instruction_callback(kind),
                publication,
                expected,
                true,
                None,
            )
        } {
            record_site_install_failure(site, &error);
        }
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

unsafe fn installed_syscall_hook_for(context: *mut HookContext, number: Option<i64>) {
    if let Some(context) = unsafe { context.as_ref() }
        && let Some(site) = find_site(context.instruction_pointer)
    {
        site.hook_count.fetch_add(1, Ordering::Relaxed);
    }
    unsafe { dispatch_syscall_context(context, number, SyscallDispatch::InstalledHook, None) };
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
    unsafe { installed_syscall_hook_for(context, None) }
}

unsafe extern "C" fn installed_vdso_time_hook(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_time)) }
}

unsafe extern "C" fn installed_vdso_clock_gettime_hook(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_clock_gettime)) }
}

unsafe extern "C" fn installed_vdso_getcpu_hook(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_getcpu)) }
}

unsafe extern "C" fn installed_vdso_gettimeofday_hook(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_gettimeofday)) }
}

unsafe extern "C" fn installed_vdso_clock_getres_hook(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_clock_getres)) }
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
        let mut bytes = [0_u8; 2];
        if read_self_bytes(address, &mut bytes) == bytes.len() && bytes == [0x0F, 0x05] {
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

        // Cover SITE_INSTALLING publication and native-policy toggles as well
        // as the inner heap scope. Otherwise a user signal arriving after the
        // claim but before install_site_hook masks signals can reenter this
        // wrapper on the same thread and spin forever on its own state.
        let concurrent_install_scope = if self.publication == PatchPublication::Concurrent {
            // SAFETY: dispatch_signal runs beneath the kernel-created SIGSYS
            // frame and its returning path reaches rt_sigreturn.
            match unsafe { crate::patch_alloc::enter_signal_handler() } {
                Ok(scope) => Some(scope),
                Err(_) => {
                    record_fallback_dispatch(event.number());
                    self.refuse_fallback(event);
                    return;
                }
            }
        } else {
            None
        };
        if let Some((site, claimed)) = claim_site(instruction_pointer) {
            site.trap_count.fetch_add(1, Ordering::Relaxed);
            if claimed {
                let native = unsafe { set_all_instruction_native(true) };
                let installed = match native {
                    Ok(()) => unsafe {
                        install_site_hook(
                            instruction_pointer,
                            site,
                            installed_syscall_hook,
                            self.publication,
                            &[0x0f, 0x05],
                            true,
                            None,
                        )
                    },
                    Err(error) => Err(InstallSiteError::from(error)),
                };
                let restored = unsafe { set_all_instruction_native(false) };
                if restored.is_err() {
                    if let Err(error) = &installed {
                        record_site_install_failure(site, error);
                    }
                    emit_in_guest_stage(b"syscall-dispatch-disable-native-failed");
                    unsafe { exit_now(126) };
                }
                if let Err(error) = installed {
                    record_site_install_failure(site, &error);
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
        drop(concurrent_install_scope);

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
    if unsupported_process {
        event.result = -i64::from(libc::ENOTSUP);
    } else {
        return false;
    }
    true
}

fn protect_runtime_mapping_control(event: &mut SyscallEvent) -> bool {
    if mmap_imports_async_mapping_engine_with(event.number, event.args, |fd| {
        fd_is_async_mapping_engine(fd)
    }) {
        event.result = -i64::from(libc::ENOTSUP);
        return true;
    }
    let sites = SITES.get().map_or(&[][..], |sites| sites.as_ref());
    let arenas = ARENAS.get();
    // Fixed replacements are classified by their source and destination spans
    // below. Invalid or disjoint calls must reach Linux so their native error or
    // success is preserved; only a valid overlap with an arena control is ours
    // to refuse.
    if !mapping_mutates_runtime_control_with(
        event.number,
        event.args,
        PAGE_SIZE.load(Ordering::Acquire),
        sites,
        |start, end, requested_protection| {
            arenas.is_some_and(|arenas| {
                arenas.iter().any(|arena| {
                    (start < arena.writable_end
                        && arena.writable_start < end
                        && requested_protection != Some(libc::PROT_READ | libc::PROT_WRITE))
                        || (start < arena.executable_end
                            && arena.executable_start < end
                            && requested_protection != Some(libc::PROT_READ | libc::PROT_EXEC))
                        || (start < arena.reservation_end
                            && arena.reservation_start < end
                            && requested_protection != Some(libc::PROT_READ | libc::PROT_WRITE))
                })
            })
        },
    ) {
        return false;
    }
    event.result = -i64::from(libc::ENOTSUP);
    true
}

pub(crate) fn injected_mapping_control_error(number: i64, args: [u64; 6]) -> Option<Errno> {
    let mut event = SyscallEvent {
        number,
        args,
        instruction_pointer: 0,
        result: UNSET_RESULT,
        context: 0,
        dispatch: SyscallDispatch::Trap,
        guest_pkru: None,
    };
    protect_runtime_mapping_control(&mut event).then_some(Errno::ENOTSUPP)
}

pub(crate) fn observe_injected_mapping_result(number: i64, args: [u64; 6], result: i64) {
    let event = SyscallEvent {
        number,
        args,
        instruction_pointer: 0,
        result,
        context: 0,
        dispatch: SyscallDispatch::Trap,
        guest_pkru: None,
    };
    observe_mapping_generation(&event);
}

// Concurrent installation leaves pending signals blocked until rt_sigreturn,
// after all patch-allocation and install guards have been cleared. A user
// handler that allocates or performs a nonlocal exit cannot safely run in that
// boundary. ToolHost admits only race-free DFL/IGN updates under its no-thread
// contract. Legacy modes reject every non-query action update because
// forwarding a caller-owned sigaction after inspecting it would permit a write
// race. Every mode also keeps the runtime signal mask and alt stack immutable
// across supported guest syscall/control-flow paths. As with the pre-existing
// trusted syscall gates, a crafted control transfer into a runtime-private gate
// is outside the documented trusted-guest boundary.
fn protect_concurrent_signal_control(event: &mut SyscallEvent, tool_mode: u8) -> bool {
    let protected = match event.number {
        // An ordinary guest rt_sigreturn entry is trapped before the syscall
        // and must not be forwarded from inside the resulting SIGSYS frame:
        // doing so would consume the runtime frame and could restore
        // caller-crafted signal state. The exact runtime restorer site follows
        // the same trusted-private-gate boundary as the existing syscall gates.
        libc::SYS_rt_sigreturn => true,
        libc::SYS_rt_sigaction if event.args[1] != 0 => {
            tool_mode != TOOL_REVERIE || !signal_action_supported(event.number, event.args)
        }
        libc::SYS_rt_sigprocmask => event.args[1] != 0,
        libc::SYS_sigaltstack => event.args[0] != 0,
        _ => false,
    };
    if !protected {
        return false;
    }
    event.result = -i64::from(libc::EPERM);
    true
}

unsafe fn process_syscall(event: &mut SyscallEvent) {
    let tool_mode = TOOL_MODE.load(Ordering::Relaxed);
    if syscall_number_requires_enosys(event.number) {
        event.result = -i64::from(libc::ENOSYS);
        if tool_mode != TOOL_REVERIE {
            unsafe {
                trace_event(event, Some(event.result));
            }
        }
        return;
    }
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
    if protect_concurrent_signal_control(event, tool_mode) {
        if tool_mode != TOOL_REVERIE {
            unsafe {
                trace_event(event, Some(event.result));
            }
        }
        return;
    }
    if protect_runtime_mapping_control(event) {
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

    if event.number == libc::SYS_clone3 {
        // clone3 stores its semantic arguments behind a guest pointer. Until
        // that record can be copied and authenticated across a complete
        // lifecycle bracket, forwarding it could admit CLONE_VM/CLONE_THREAD
        // and invalidate every sole-task runtime invariant.
        event.result = clone3_refusal_result(tool_mode);
        if tool_mode != TOOL_REVERIE {
            unsafe {
                trace_event(event, Some(event.result));
            }
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
        && matches!(
            event.number,
            libc::SYS_clone | libc::SYS_fork | libc::SYS_vfork
        );
    if compatibility_fork {
        unsafe {
            trace_event(event, None);
        }
    }
    let requested_number = event.number;
    let physical_number = legacy_physical_syscall_number(tool_mode, requested_number);
    event.number = physical_number;
    event.result = unsafe { event.forward() };
    event.number = requested_number;
    if requested_number == libc::SYS_vfork && physical_number == libc::SYS_fork && event.result > 0
    {
        event.result = unsafe { wait_for_translated_vfork_child(event.result) };
    }
    if event.number == libc::SYS_mmap
        && event.result >= 0
        && event.args[3] & libc::MAP_ANONYMOUS as u64 == 0
        && !matches!(
            mmap_result_is_async_mapping_engine(event.result as u64),
            Ok(false)
        )
    {
        // The pre-call fd check closes ordinary imports. This post-call VMA
        // authentication closes fd-table races and fails before the guest can
        // submit SQPOLL work through a newly mapped io_uring queue.
        unsafe { exit_now(126) };
    }
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

fn legacy_physical_syscall_number(tool_mode: u8, requested_number: i64) -> i64 {
    if tool_mode != TOOL_REVERIE && requested_number == libc::SYS_vfork {
        // A physical vfork child would execute this SIGSYS callback and its
        // rt_sigreturn on the suspended parent's shared signal-frame stack.
        // COW fork gives the child an independent frame; the parent-side
        // wait below preserves the supported vfork completion boundary.
        libc::SYS_fork
    } else {
        requested_number
    }
}

pub(crate) unsafe fn wait_for_translated_vfork_child(child: i64) -> i64 {
    let mut info = core::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    loop {
        let waited = unsafe {
            raw_syscall6(
                libc::SYS_waitid,
                [
                    libc::P_PID as u64,
                    child as u64,
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
        if waited == -i64::from(libc::ECHILD) {
            // The inherited, supported SIGCHLD=SIG_IGN/SA_NOCLDWAIT states
            // auto-reap this exact physical-fork child. With no guest threads
            // or competing waiter, ECHILD can arise here only after the child
            // has completed the parent-suspension boundary.
            return child;
        }
        return if waited < 0 { waited } else { child };
    }
}

fn clone3_refusal_result(tool_mode: u8) -> i64 {
    if tool_mode == TOOL_COMPAT {
        -i64::from(libc::EPERM)
    } else {
        -i64::from(libc::ENOTSUP)
    }
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
    #[test]
    fn callback_stack_geometry_preserves_eight_mib_below_fxsave_and_xsave() {
        let start = 0x1000_u64;
        let headroom = super::HOST_CALLBACK_EXECUTION_HEADROOM_BYTES as u64;
        let prefix = super::HOOK_CONTEXT_STACK_PREFIX_BYTES as u64;
        for (reserve, alignment) in [(512_u64, 16_u64), (2_752, 64)] {
            let exact_top = start + headroom + prefix + reserve;
            assert_eq!(
                super::host_callback_saved_state_start(
                    start, exact_top, headroom, prefix, reserve, alignment,
                ),
                Some(start + headroom)
            );
            assert_eq!(
                super::host_callback_saved_state_start(
                    start,
                    exact_top - 1,
                    headroom,
                    prefix,
                    reserve,
                    alignment,
                ),
                None,
                "one byte less must not satisfy the execution headroom"
            );
        }
    }

    #[test]
    fn callback_stack_sizing_is_page_rounded_bounded_and_checked() {
        let headroom = super::HOST_CALLBACK_EXECUTION_HEADROOM_BYTES;
        let prefix = super::HOOK_CONTEXT_STACK_PREFIX_BYTES;
        let page = 4096;
        assert_eq!(
            super::host_callback_usable_len(headroom, prefix, 512, 16, page),
            Some(headroom + page)
        );
        assert_eq!(
            super::host_callback_usable_len(headroom, prefix, 2_752, 64, page),
            Some(headroom + page)
        );
        assert!(
            super::host_callback_usable_len(
                headroom,
                prefix,
                super::HOST_CALLBACK_SAVED_XSTATE_RESERVE_CEILING_BYTES,
                64,
                page,
            )
            .is_some()
        );
        assert_eq!(
            super::host_callback_usable_len(
                headroom,
                prefix,
                super::HOST_CALLBACK_SAVED_XSTATE_RESERVE_CEILING_BYTES + 1,
                64,
                page,
            ),
            None
        );
        assert_eq!(
            super::host_callback_usable_len(usize::MAX, prefix, 512, 16, page),
            None
        );
        assert_eq!(
            super::host_callback_usable_len(headroom, prefix, 512, 3, page),
            None
        );
        assert_eq!(
            super::host_callback_saved_state_start(0, prefix as u64 - 1, 0, prefix as u64, 512, 16),
            None
        );
        assert_eq!(
            super::host_callback_saved_state_start(
                u64::MAX - 1,
                u64::MAX,
                1,
                prefix as u64,
                512,
                16,
            ),
            None
        );
    }

    #[test]
    fn v6_saved_xstate_publication_is_canonical_and_incomplete_is_zero() {
        let layout = super::SavedExtendedStateLayout::detect().unwrap();
        let initial = super::host_saved_xstate_publication(layout);
        let reconstructed = super::host_saved_xstate_publication(layout);
        assert_eq!(initial, reconstructed);
        let initial_bytes = unsafe {
            core::slice::from_raw_parts(
                core::ptr::from_ref(&initial).cast::<u8>(),
                core::mem::size_of_val(&initial),
            )
        };
        let reconstructed_bytes = unsafe {
            core::slice::from_raw_parts(
                core::ptr::from_ref(&reconstructed).cast::<u8>(),
                core::mem::size_of_val(&reconstructed),
            )
        };
        assert_eq!(initial_bytes, reconstructed_bytes);

        let incomplete = super::HostInstallResult::default();
        assert_eq!(incomplete.complete, 0);
        let absent = incomplete.saved_xstate_publication();
        assert_eq!(absent, super::HostSavedXstatePublication::default());
        let absent_bytes = unsafe {
            core::slice::from_raw_parts(
                core::ptr::from_ref(&absent).cast::<u8>(),
                core::mem::size_of_val(&absent),
            )
        };
        assert!(absent_bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn explicit_host_refuses_concurrent_publication_without_changing_legacy_policy() {
        use super::PatchPublication;
        use super::validate_host_publication;
        assert!(validate_host_publication(true, PatchPublication::Quiescent).is_ok());
        assert_eq!(
            validate_host_publication(true, PatchPublication::Concurrent)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOTSUP)
        );
        assert!(validate_host_publication(false, PatchPublication::Concurrent).is_ok());
        assert!(validate_host_publication(false, PatchPublication::Quiescent).is_ok());
    }

    #[test]
    fn ptrace_install_request_validation_is_exact_and_bounded() {
        let site = 0x4000_u64;
        let mut source = [0_u8; super::PATCH_SNAPSHOT_BYTES];
        source[..8].copy_from_slice(&[0x0f, 0x05, 2, 3, 4, 5, 6, 7]);
        let request = super::HostInstallRequest {
            version: super::HOST_INSTALL_REQUEST_VERSION,
            site_start: site,
            mapping_end: site + 8,
            source_len: 8,
            source,
        };
        assert_eq!(
            super::validate_host_install_request(site, &request),
            Some(8)
        );

        let mut invalid = request;
        invalid.source_len = 7;
        assert_eq!(super::validate_host_install_request(site, &invalid), None);
        let mut invalid = request;
        invalid.source_len = super::PATCH_SNAPSHOT_BYTES as u64 + 1;
        assert_eq!(super::validate_host_install_request(site, &invalid), None);
        let mut invalid = request;
        invalid.site_start += 1;
        assert_eq!(super::validate_host_install_request(site, &invalid), None);
        let mut invalid = request;
        invalid.mapping_end -= 1;
        assert_eq!(super::validate_host_install_request(site, &invalid), None);
        let mut invalid = request;
        invalid.source[1] = 0x04;
        assert_eq!(super::validate_host_install_request(site, &invalid), None);
        let mut invalid = request;
        invalid.source[8] = 1;
        assert_eq!(super::validate_host_install_request(site, &invalid), None);
        let overflow = super::HostInstallRequest {
            site_start: u64::MAX - 3,
            mapping_end: u64::MAX,
            ..request
        };
        assert_eq!(
            super::validate_host_install_request(overflow.site_start, &overflow),
            None
        );
    }

    use core::sync::atomic::AtomicUsize;
    use core::sync::atomic::Ordering;
    use std::cell::Cell;
    use std::ffi::OsStr;

    use liteinst2::trampoline::TrampolineError;
    use reverie_preload::BuiltinTool;

    use super::ALT_STACK_ENV;
    use super::ARENA_SLOT_BYTES;
    use super::ARENA_SLOTS;
    use super::ArenaControlRanges;
    use super::ArenaMapValidationStats;
    use super::ENABLED_FALLBACK_CLASSIFICATIONS;
    use super::FORK_HOOK;
    use super::FallbackCounters;
    use super::InstallProtectionRestoreFailure;
    use super::InstallProtectionRestoreStage;
    use super::InstallProtectionTransitionFailure;
    use super::InstallSiteError;
    use super::LiteinstDispatcher;
    use super::MAX_LIFETIME_PATCH_ATTEMPTS;
    use super::MAX_PATCH_SITES;
    use super::MAX_PREPARED_ARENAS;
    use super::MAX_PROC_MAP_RECORDS;
    use super::MAX_PROC_SELF_MAPS_BYTES;
    use super::MIN_PROC_MAP_RECORD_BYTES;
    use super::MappingPageSpan;
    use super::PREPARATION_WORST_CASE_BYTES;
    use super::RCB_CLOCK;
    use super::RCB_CLOCK_OWNER;
    use super::RCB_CLOCK_UNAVAILABLE;
    use super::RuntimeMap;
    use super::SITE_ACTIVE;
    use super::SITE_EXHAUSTED;
    use super::SITE_FALLBACK;
    use super::SITE_INSTALLING;
    use super::SITE_STALE;
    use super::SITES;
    use super::SiteSlot;
    use super::StackLine;
    use super::SyscallDispatch;
    use super::SyscallEvent;
    use super::TOOL_COMPAT;
    use super::TOOL_PASSTHROUGH;
    use super::TOOL_REVERIE;
    use super::TOOL_SPOOF_GETPID;
    use super::TOOL_STRACE;
    use super::UNSET_RESULT;
    use super::X32_SYSCALL_BIT;
    use super::alt_stack_from_env_value;
    use super::builtin_tool_from_env_value;
    use super::checked_mapping_page_span;
    use super::claim_lifetime_patch_attempt;
    use super::claim_site;
    use super::classify_install_protection_restore;
    use super::clone_is_fork_like;
    use super::clone3_refusal_result;
    use super::close_activated_install_with;
    use super::collect_new_runtime_maps;
    use super::fallback_dispatch_count;
    use super::fallback_syscall_count;
    use super::forward_nested_tool_syscall;
    use super::guard_prior_signal_action_is_admitted;
    use super::initialize_rcb_clock_with;
    use super::is_async_mapping_engine_target;
    use super::legacy_physical_syscall_number;
    use super::madvise_preserves_source_generation;
    use super::mapping_mutates_runtime_control_with;
    use super::mark_original_instruction_stale;
    use super::mark_site_generation_stale;
    use super::mark_site_range_stale_in;
    use super::mmap_imports_async_mapping_engine_with;
    use super::mremap_effect_spans;
    use super::mremap_fixed_destination_page_span;
    use super::mremap_source_page_span;
    use super::observe_arena_source_generation_with;
    use super::observe_mapping_generation_in;
    use super::open_install_source_with;
    use super::parse_runtime_map_line;
    use super::proc_maps_line_async_engine_at;
    use super::proc_self_fd_path;
    use super::protect_concurrent_signal_control;
    use super::raw_syscall6;
    use super::read_fork_safe_mapping_ranges_from;
    use super::read_proc_self_maps;
    use super::read_runtime_maps;
    use super::record_fallback_dispatch;
    use super::record_site_install_failure;
    use super::remap_file_pages_span;
    use super::reset_site_observability;
    use super::run_bounded_patch_attempts;
    use super::sampled_source_index;
    use super::syscall_number_requires_enosys;
    use super::validate_prepared_control_maps;
    use super::wait_for_translated_vfork_child;

    #[test]
    fn guard_router_admits_only_default_or_ignored_prior_sigtrap() {
        for handler in [libc::SIG_DFL, libc::SIG_IGN] {
            assert!(guard_prior_signal_action_is_admitted(
                &super::GuardSignalAction {
                    handler,
                    flags: 0,
                    restorer: 0,
                    mask: 0,
                }
            ));
        }
        assert!(!guard_prior_signal_action_is_admitted(
            &super::GuardSignalAction {
                handler: 0x1234,
                flags: libc::SA_SIGINFO as libc::c_ulong,
                restorer: 0x5678,
                mask: 1,
            }
        ));
    }

    #[test]
    fn rx_restore_failure_is_fatal_before_and_after_activation() {
        for stage in [
            InstallProtectionRestoreStage::WritableOpenFailure,
            InstallProtectionRestoreStage::PlanningFailure,
            InstallProtectionRestoreStage::ActivationFailure,
            InstallProtectionRestoreStage::ActivatedRollbackReopen,
            InstallProtectionRestoreStage::ActivatedRollbackDeactivation,
            InstallProtectionRestoreStage::ActivatedRollbackFinalRx,
        ] {
            assert_eq!(
                classify_install_protection_restore(
                    stage,
                    Err(std::io::Error::from_raw_os_error(libc::EACCES)),
                ),
                Err(InstallProtectionRestoreFailure {
                    stage,
                    errno: libc::EACCES,
                })
            );
            assert_eq!(classify_install_protection_restore(stage, Ok(())), Ok(()));
        }
    }

    #[test]
    fn install_protection_transactions_restore_rx_and_deactivate_before_returning_errors() {
        let open_log = std::cell::RefCell::new(Vec::new());
        let mut open_attempt = 0;
        let opened = open_install_source_with(|protection| {
            open_log.borrow_mut().push(protection);
            open_attempt += 1;
            if open_attempt == 1 {
                Err(std::io::Error::from_raw_os_error(libc::EACCES))
            } else {
                Ok(())
            }
        });
        assert!(matches!(
            opened,
            Err(InstallProtectionTransitionFailure::Primary(ref error))
                if error.raw_os_error() == Some(libc::EACCES)
        ));
        assert_eq!(
            open_log.into_inner(),
            [
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::PROT_READ | libc::PROT_EXEC,
            ]
        );

        let rollback_log = std::cell::RefCell::new(Vec::new());
        let mut protect_attempt = 0;
        let rolled_back = close_activated_install_with(
            |protection| {
                rollback_log.borrow_mut().push(("protect", protection));
                protect_attempt += 1;
                if protect_attempt == 1 {
                    Err(std::io::Error::from_raw_os_error(libc::EPERM))
                } else {
                    Ok(())
                }
            },
            || {
                rollback_log.borrow_mut().push(("deactivate", 0));
                Ok(())
            },
        );
        assert!(matches!(
            rolled_back,
            Err(InstallProtectionTransitionFailure::Primary(ref error))
                if error.raw_os_error() == Some(libc::EPERM)
        ));
        assert_eq!(
            rollback_log.into_inner(),
            [
                ("protect", libc::PROT_READ | libc::PROT_EXEC),
                (
                    "protect",
                    libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                ),
                ("deactivate", 0),
                ("protect", libc::PROT_READ | libc::PROT_EXEC),
            ]
        );

        let cleanup = close_activated_install_with(
            |protection| {
                if protection == (libc::PROT_READ | libc::PROT_EXEC) {
                    Err(std::io::Error::from_raw_os_error(libc::EPERM))
                } else {
                    Ok(())
                }
            },
            || Err(()),
        );
        assert!(matches!(
            cleanup,
            Err(InstallProtectionTransitionFailure::Cleanup(
                InstallProtectionRestoreFailure {
                    stage: InstallProtectionRestoreStage::ActivatedRollbackDeactivation,
                    errno: libc::EIO,
                }
            ))
        ));
    }

    #[test]
    fn inherited_and_imported_async_mapping_engines_are_refused_at_mmap_boundary() {
        assert!(is_async_mapping_engine_target(b"anon_inode:[io_uring]"));
        assert!(is_async_mapping_engine_target(b"anon_inode:[userfaultfd]"));
        assert!(!is_async_mapping_engine_target(b"anon_inode:[eventfd]"));

        let imported_ring = [
            0,
            4096,
            (libc::PROT_READ | libc::PROT_WRITE) as u64,
            libc::MAP_SHARED as u64,
            73,
            0,
        ];
        assert!(mmap_imports_async_mapping_engine_with(
            libc::SYS_mmap,
            imported_ring,
            |fd| {
                assert_eq!(fd, 73);
                Ok(true)
            },
        ));
        assert!(!mmap_imports_async_mapping_engine_with(
            libc::SYS_mmap,
            imported_ring,
            |_| Ok(false),
        ));
        assert!(mmap_imports_async_mapping_engine_with(
            libc::SYS_mmap,
            imported_ring,
            |_| Err(reverie::Errno::EIO),
        ));
        assert!(!mmap_imports_async_mapping_engine_with(
            libc::SYS_mmap,
            imported_ring,
            |_| Err(reverie::Errno::EBADF),
        ));

        let anonymous = [
            0,
            4096,
            (libc::PROT_READ | libc::PROT_WRITE) as u64,
            (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
            u64::MAX,
            0,
        ];
        assert!(!mmap_imports_async_mapping_engine_with(
            libc::SYS_mmap,
            anonymous,
            |_| panic!("anonymous mmap inspected its ignored fd"),
        ));

        let mut path = [0_u8; 64];
        let len = proc_self_fd_path(73, &mut path).expect("format proc fd path");
        assert_eq!(&path[..=len], b"/proc/self/fd/73\0");

        assert_eq!(
            proc_maps_line_async_engine_at(
                b"1000-2000 rw-s 00000000 00:01 9 anon_inode:[io_uring]",
                0x1800,
            ),
            Ok(Some(true))
        );
        assert_eq!(
            proc_maps_line_async_engine_at(b"1000-2000 rw-p 00000000 00:00 0", 0x1800,),
            Ok(Some(false))
        );
        assert_eq!(
            proc_maps_line_async_engine_at(
                b"3000-4000 rw-s 00000000 00:01 9 anon_inode:[io_uring]",
                0x1800,
            ),
            Ok(None)
        );
    }

    #[test]
    fn preparation_storage_bound_fits_its_isolated_heap() {
        assert!((MAX_PROC_MAP_RECORDS - 1) * MIN_PROC_MAP_RECORD_BYTES <= MAX_PROC_SELF_MAPS_BYTES);
        assert!(MAX_PROC_MAP_RECORDS * MIN_PROC_MAP_RECORD_BYTES > MAX_PROC_SELF_MAPS_BYTES);
        assert!(PREPARATION_WORST_CASE_BYTES < crate::patch_alloc::PREPARATION_HEAP_BYTES);
        assert!(
            crate::patch_alloc::PREPARATION_HEAP_BYTES - PREPARATION_WORST_CASE_BYTES
                >= 4 * 1024 * 1024,
            "preparation heap={} worst_case={} margin={}",
            crate::patch_alloc::PREPARATION_HEAP_BYTES,
            PREPARATION_WORST_CASE_BYTES,
            crate::patch_alloc::PREPARATION_HEAP_BYTES - PREPARATION_WORST_CASE_BYTES,
        );
    }

    #[test]
    fn arena_source_sampling_is_bounded_and_matches_aggregate_planned_slot_capacity() {
        assert_eq!(
            MAX_PREPARED_ARENAS, 32,
            "fail-closed support is limited to 32 sampled executable source mappings"
        );
        assert_eq!(
            ARENA_SLOTS, 128,
            "fail-closed support is limited to 128 planned sites per sampled mapping"
        );
        for total in [0, 1, 31, 32, 33, MAX_PROC_MAP_RECORDS] {
            let count = total.min(MAX_PREPARED_ARENAS);
            let selected = (0..count)
                .map(|ordinal| sampled_source_index(ordinal, total, count).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(selected.len(), count);
            assert!(selected.windows(2).all(|pair| pair[0] < pair[1]));
            if total != 0 {
                assert_eq!(selected.first(), Some(&0));
                assert_eq!(selected.last(), Some(&(total - 1)));
            }
        }
        assert_eq!(
            MAX_PREPARED_ARENAS * ARENA_SLOTS,
            MAX_PATCH_SITES,
            "aggregate planned-slot capacity must equal 32 sources times 128 sites"
        );
    }

    #[test]
    fn runtime_map_parser_rejects_every_malformed_record_instead_of_omitting_it() {
        let parsed =
            parse_runtime_map_line("1000-2000 r-xp 00000000 00:01 7 /path with spaces", 0).unwrap();
        assert_eq!((parsed.start, parsed.end), (0x1000, 0x2000));
        assert_eq!(parsed.path.as_deref(), Some("/path with spaces"));

        for line in [
            "",
            "1000-2000 r-xp 00000000 00:01",
            "2000-1000 r-xp 00000000 00:01 7",
            "1000-2000-3000 r-xp 00000000 00:01 7",
            "1000-2000 rwxq 00000000 00:01 7",
            "1000-2000 r-xp not-hex 00:01 7",
            "1000-2000 r-xp 00000000 00:01:02 7",
            "1000-2000 r-xp 00000000 00:01 not-decimal",
        ] {
            assert!(
                parse_runtime_map_line(line, 4).is_err(),
                "accepted malformed maps record {line:?}"
            );
        }
    }

    #[test]
    fn runtime_smaps_source_admission_requires_exact_zero_protection_key() {
        let header = "1000-2000 r-xp 00000000 08:01 7 /source\n";
        let parse = |smaps: &str| {
            read_fork_safe_mapping_ranges_from(std::io::Cursor::new(smaps.as_bytes()))
        };
        assert_eq!(
            parse(&format!(
                "{header}ProtectionKey: 0\nVmFlags: rd ex mr mw me\n"
            ))
            .unwrap(),
            vec![(0x1000, 0x2000)]
        );
        assert_eq!(
            parse(&format!(
                "{header}Anonymous: 4096 kB\nProtectionKey: 0\nVmFlags: rd ex mr mw me\n"
            ))
            .unwrap(),
            vec![(0x1000, 0x2000)]
        );

        for omitted in [
            format!("{header}ProtectionKey: 1\nVmFlags: rd ex mr mw me\n"),
            format!("{header}VmFlags: rd ex mr mw me\n"),
            format!("{header}ProtectionKey: 0\nVmFlags: rd ex dc mr mw me\n"),
            format!("{header}ProtectionKey: 0\nVmFlags: rd ex wf mr mw me\n"),
            format!("{header}ProtectionKey: 0\nVmFlags: rd ex ht mr mw me\n"),
            format!(
                "{header}ProtectionKey: 0\n2000-3000 r-xp 00001000 08:01 8 /next\nVmFlags: rd ex mr mw me\n"
            ),
        ] {
            assert_eq!(parse(&omitted).unwrap(), Vec::<(u64, u64)>::new());
        }

        for malformed_key in ["", "-1", "not-decimal", "18446744073709551616"] {
            assert!(
                parse(&format!(
                    "{header}ProtectionKey: {malformed_key}\nVmFlags: rd ex mr mw me\n"
                ))
                .is_err()
            );
        }
        assert!(
            parse(&format!(
                "{header}ProtectionKey: 0\nProtectionKey: 0\nVmFlags: rd ex mr mw me\n"
            ))
            .is_err()
        );
        assert!(
            parse(&format!(
                "{header}VmFlags: rd ex mr mw me\nProtectionKey: 0\n"
            ))
            .is_err()
        );
        for trailing in ["ProtectionKey: 1\n", "VmFlags: rd ex mr mw me\n"] {
            assert!(
                parse(&format!(
                    "{header}ProtectionKey: 0\nVmFlags: rd ex mr mw me\n{trailing}"
                ))
                .is_err()
            );
        }
        assert!(
            parse(&format!(
                "{header}ProtectionKey: 0\nnot-a-range r-xp 00001000 08:01 8 /bad\nVmFlags: rd ex mr mw me\n"
            ))
            .is_err()
        );
        assert!(
            parse(&format!(
                "{header}ProtectionKey: 0\n1000-z000 badp 00001000 08:01 8 /bad\nVmFlags: rd ex mr mw me\n"
            ))
            .is_err()
        );

        let two = format!(
            "{header}ProtectionKey: 0\nVmFlags: rd ex mr mw me\n\
             2000-3000 r-xp 00001000 08:01 8 /next\n\
             ProtectionKey: 1\nVmFlags: rd ex mr mw me\n"
        );
        assert_eq!(parse(&two).unwrap(), vec![(0x1000, 0x2000)]);
    }

    fn synthetic_runtime_map(start: u64) -> RuntimeMap {
        RuntimeMap {
            start,
            end: start + 4096,
            offset: 0,
            device_major: 0,
            device_minor: 0,
            inode: 0,
            readable: true,
            writable: false,
            executable: false,
            shared: false,
            path: None,
        }
    }

    #[test]
    fn runtime_map_delta_is_linear_at_the_proc_map_ceiling() {
        let before = (0..MAX_PROC_MAP_RECORDS)
            .map(|index| synthetic_runtime_map(0x1_0000_0000 + index as u64 * 8192))
            .collect::<Vec<_>>();
        let after = before.clone();
        let mut stats = ArenaMapValidationStats::default();
        let new_maps = collect_new_runtime_maps(&before, &after, &mut stats).unwrap();

        assert!(new_maps.is_empty());
        assert!(stats.baseline_comparisons <= before.len() + after.len());
    }

    #[test]
    fn runtime_map_delta_rejects_baseline_deletion_and_mutation() {
        let before = vec![
            synthetic_runtime_map(0x1_0000),
            synthetic_runtime_map(0x2_0000),
        ];
        let deleted = vec![before[1].clone()];
        let mut stats = ArenaMapValidationStats::default();
        assert_eq!(
            collect_new_runtime_maps(&before, &deleted, &mut stats)
                .unwrap_err()
                .to_string(),
            "a baseline mapping disappeared during LiteInst preparation"
        );

        let mut mutated = before.clone();
        mutated[0].writable = true;
        let mut stats = ArenaMapValidationStats::default();
        assert_eq!(
            collect_new_runtime_maps(&before, &mutated, &mut stats)
                .unwrap_err()
                .to_string(),
            "a baseline mapping changed during LiteInst preparation"
        );
    }

    fn synthetic_arena_controls(
        base: u64,
        alias_inode: u64,
        reservation_inode: u64,
    ) -> ([RuntimeMap; 3], ArenaControlRanges) {
        let arena_bytes = ARENA_SLOTS as u64 * ARENA_SLOT_BYTES;
        let executable_start = base + arena_bytes;
        let reservation_start = executable_start + arena_bytes;
        (
            [
                RuntimeMap {
                    start: base,
                    end: executable_start,
                    offset: 0,
                    device_major: 0,
                    device_minor: 1,
                    inode: alias_inode,
                    readable: true,
                    writable: true,
                    executable: false,
                    shared: true,
                    path: Some("/memfd:liteinst2-trampoline (deleted)".into()),
                },
                RuntimeMap {
                    start: executable_start,
                    end: reservation_start,
                    offset: 0,
                    device_major: 0,
                    device_minor: 1,
                    inode: alias_inode,
                    readable: true,
                    writable: false,
                    executable: true,
                    shared: true,
                    path: Some("/memfd:liteinst2-trampoline (deleted)".into()),
                },
                RuntimeMap {
                    start: reservation_start,
                    end: reservation_start + ARENA_SLOT_BYTES,
                    offset: 0,
                    device_major: 0,
                    device_minor: 1,
                    inode: reservation_inode,
                    readable: true,
                    writable: true,
                    executable: false,
                    shared: true,
                    path: Some("/dev/zero (deleted)".into()),
                },
            ],
            ArenaControlRanges {
                writable: (base, executable_start),
                executable: (executable_start, reservation_start),
                reservation: (reservation_start, reservation_start + ARENA_SLOT_BYTES),
            },
        )
    }

    #[test]
    fn arena_control_lookup_is_bounded_at_the_proc_map_ceiling() {
        let arena_count = MAX_PROC_MAP_RECORDS / 3;
        let mut after = Vec::with_capacity(arena_count * 3);
        let mut controls = Vec::with_capacity(arena_count);
        let arena_bytes = ARENA_SLOTS as u64 * ARENA_SLOT_BYTES;
        let stride = 2 * arena_bytes + 2 * ARENA_SLOT_BYTES;
        for index in 0..arena_count {
            let base = 0x1_0000_0000 + index as u64 * stride;
            let alias_inode = index as u64 + 1;
            let reservation_inode = arena_count as u64 + index as u64 + 1;
            let (maps, control) = synthetic_arena_controls(base, alias_inode, reservation_inode);
            after.extend(maps);
            controls.push(control);
        }
        controls.reverse();

        let stats = validate_prepared_control_maps(
            &[],
            &after,
            &controls,
            |control| *control,
            arena_bytes,
            ARENA_SLOT_BYTES,
        )
        .unwrap();
        let binary_search_height = usize::BITS as usize - after.len().leading_zeros() as usize + 1;
        assert_eq!(stats.baseline_comparisons, 0);
        assert!(
            stats.control_lookup_comparisons <= after.len() * binary_search_height,
            "{} lookup comparisons exceeded the {}-comparison logarithmic bound",
            stats.control_lookup_comparisons,
            after.len() * binary_search_height,
        );
    }

    #[test]
    fn arena_control_validation_rejects_noncanonical_alias_geometry() {
        let arena_bytes = ARENA_SLOTS as u64 * ARENA_SLOT_BYTES;
        let (maps, mut controls) = synthetic_arena_controls(0x1_0000_0000, 1, 2);
        controls.executable.1 -= ARENA_SLOT_BYTES;
        assert_eq!(
            validate_prepared_control_maps(
                &[],
                &maps,
                &[controls],
                |control| *control,
                arena_bytes,
                ARENA_SLOT_BYTES,
            )
            .unwrap_err()
            .to_string(),
            "LiteInst prepared arena controls have unexpected sizes"
        );
    }

    #[test]
    fn arena_control_validation_rejects_cross_arena_alias_identity_reuse() {
        let arena_bytes = ARENA_SLOTS as u64 * ARENA_SLOT_BYTES;
        let (first_maps, first) = synthetic_arena_controls(0x1_0000_0000, 7, 8);
        let (second_maps, second) = synthetic_arena_controls(0x2_0000_0000, 7, 9);
        let after = first_maps
            .into_iter()
            .chain(second_maps)
            .collect::<Vec<_>>();

        assert_eq!(
            validate_prepared_control_maps(
                &[],
                &after,
                &[first, second],
                |control| *control,
                arena_bytes,
                ARENA_SLOT_BYTES,
            )
            .unwrap_err()
            .to_string(),
            "LiteInst prepared arenas reuse one sealed alias backing identity"
        );
    }

    #[test]
    fn arena_control_validation_rejects_cross_arena_reservation_identity_reuse() {
        let arena_bytes = ARENA_SLOTS as u64 * ARENA_SLOT_BYTES;
        let (first_maps, first) = synthetic_arena_controls(0x1_0000_0000, 7, 9);
        let (second_maps, second) = synthetic_arena_controls(0x2_0000_0000, 8, 9);
        let after = first_maps
            .into_iter()
            .chain(second_maps)
            .collect::<Vec<_>>();

        assert_eq!(
            validate_prepared_control_maps(
                &[],
                &after,
                &[first, second],
                |control| *control,
                arena_bytes,
                ARENA_SLOT_BYTES,
            )
            .unwrap_err()
            .to_string(),
            "LiteInst prepared arenas reuse one shared reservation identity"
        );
    }

    extern "C" fn inherited_signal_handler(_signal: libc::c_int) {}

    fn inherited_handler_reset_child() -> bool {
        let inherited = super::KernelSigaction {
            handler: inherited_signal_handler as *const () as u64,
            ..super::KernelSigaction::default()
        };
        let installed = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [
                    libc::SIGUSR1 as u64,
                    (&raw const inherited) as u64,
                    0,
                    core::mem::size_of::<u64>() as u64,
                    0,
                    0,
                ],
            )
        };
        if installed != 0 {
            return false;
        }

        let sigsys = 1_u64 << (libc::SIGSYS - 1);
        let sigtrap = 1_u64 << (libc::SIGTRAP - 1);
        let sigusr2 = 1_u64 << (libc::SIGUSR2 - 1);
        let initial_mask = sigsys | sigtrap | sigusr2;
        let masked = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_SETMASK as u64,
                    (&raw const initial_mask) as u64,
                    0,
                    core::mem::size_of::<u64>() as u64,
                    0,
                    0,
                ],
            )
        };
        if masked != 0 {
            return false;
        }

        let guard =
            match super::prepare_guest_signal_state(super::InstructionSubscriptions::default()) {
                Ok(guard) => guard,
                Err(_) => return false,
            };
        let mut observed = super::KernelSigaction::default();
        let queried = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [
                    libc::SIGUSR1 as u64,
                    0,
                    (&raw mut observed) as u64,
                    core::mem::size_of::<u64>() as u64,
                    0,
                    0,
                ],
            )
        };
        if queried != 0 || observed.handler != libc::SIG_DFL as u64 {
            return false;
        }

        drop(guard);
        let mut restored_mask = 0_u64;
        let queried_mask = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_SETMASK as u64,
                    0,
                    (&raw mut restored_mask) as u64,
                    core::mem::size_of::<u64>() as u64,
                    0,
                    0,
                ],
            )
        };
        queried_mask == 0 && restored_mask == sigusr2
    }

    #[test]
    fn legacy_boundary_resets_handlers_and_unblocks_runtime_signals() {
        let child = unsafe { raw_syscall6(libc::SYS_fork, [0; 6]) };
        assert!(child >= 0);
        if child == 0 {
            let code = if inherited_handler_reset_child() {
                0
            } else {
                73
            };
            let _ = unsafe { raw_syscall6(libc::SYS_exit_group, [code, 0, 0, 0, 0, 0]) };
            loop {
                core::hint::spin_loop();
            }
        }

        let mut status = 0_i32;
        loop {
            let waited = unsafe {
                raw_syscall6(
                    libc::SYS_wait4,
                    [child as u64, (&raw mut status) as u64, 0, 0, 0, 0],
                )
            };
            if waited == -i64::from(libc::EINTR) {
                continue;
            }
            assert_eq!(waited, child);
            break;
        }
        assert_eq!(status, 0);
    }

    #[test]
    fn concurrent_modes_enforce_exact_signal_handler_admission() {
        let mut action = super::KernelSigaction {
            handler: 0x1234,
            ..super::KernelSigaction::default()
        };
        let mut event = SyscallEvent {
            number: libc::SYS_rt_sigaction,
            args: [
                libc::SIGUSR1 as u64,
                (&raw mut action) as u64,
                0,
                core::mem::size_of::<u64>() as u64,
                0,
                0,
            ],
            instruction_pointer: 0,
            result: UNSET_RESULT,
            context: 0,
            dispatch: SyscallDispatch::InstalledHook,
            guest_pkru: None,
        };
        assert!(protect_concurrent_signal_control(
            &mut event,
            super::TOOL_REVERIE
        ));
        assert_eq!(event.result, -i64::from(libc::EPERM));

        // signal_action_supported reads this guest-owned word through
        // process_vm_readv, so keep the test mutation externally observable.
        unsafe {
            core::ptr::addr_of_mut!(action.handler).write_volatile(libc::SIG_IGN as u64);
        }
        event.result = UNSET_RESULT;
        assert!(!protect_concurrent_signal_control(
            &mut event,
            super::TOOL_REVERIE
        ));
        assert_eq!(event.result, UNSET_RESULT);

        unsafe {
            core::ptr::addr_of_mut!(action.handler).write_volatile(libc::SIG_DFL as u64);
        }
        assert!(!protect_concurrent_signal_control(
            &mut event,
            super::TOOL_REVERIE
        ));

        // liteinst2 owns SIGTRAP for guarded concurrent publication even when
        // the caller asks to install a nominally safe default disposition.
        event.args[0] = libc::SIGTRAP as u64;
        event.result = UNSET_RESULT;
        assert!(protect_concurrent_signal_control(
            &mut event,
            super::TOOL_REVERIE
        ));
        assert_eq!(event.result, -i64::from(libc::EPERM));

        for signal in [libc::SIGSYS, libc::SIGTRAP, libc::SIGSEGV] {
            event.args[0] = signal as u64 | (1_u64 << 32);
            event.result = UNSET_RESULT;
            assert!(protect_concurrent_signal_control(
                &mut event,
                super::TOOL_REVERIE
            ));
            assert_eq!(event.result, -i64::from(libc::EPERM));
        }
        event.args[0] = libc::SIGUSR1 as u64;

        for mode in [super::TOOL_STRACE, super::TOOL_COMPAT] {
            event.result = UNSET_RESULT;
            assert!(protect_concurrent_signal_control(&mut event, mode));
            assert_eq!(event.result, -i64::from(libc::EPERM));
        }

        event.args[1] = 0;
        event.result = UNSET_RESULT;
        assert!(!protect_concurrent_signal_control(
            &mut event,
            super::TOOL_STRACE
        ));
        assert_eq!(event.result, UNSET_RESULT);

        for (number, set_index) in [
            (libc::SYS_rt_sigprocmask, 1_usize),
            (libc::SYS_sigaltstack, 0_usize),
        ] {
            event.number = number;
            event.args = [0; 6];
            event.args[set_index] = 1;
            for mode in [super::TOOL_REVERIE, super::TOOL_STRACE, super::TOOL_COMPAT] {
                event.result = UNSET_RESULT;
                assert!(protect_concurrent_signal_control(&mut event, mode));
                assert_eq!(event.result, -i64::from(libc::EPERM));
            }
            event.args[set_index] = 0;
            event.result = UNSET_RESULT;
            assert!(!protect_concurrent_signal_control(
                &mut event,
                super::TOOL_STRACE
            ));
            assert_eq!(event.result, UNSET_RESULT);
        }

        event.number = libc::SYS_rt_sigreturn;
        event.args = [0; 6];
        for mode in [super::TOOL_REVERIE, super::TOOL_STRACE, super::TOOL_COMPAT] {
            event.result = UNSET_RESULT;
            assert!(protect_concurrent_signal_control(&mut event, mode));
            assert_eq!(event.result, -i64::from(libc::EPERM));
        }
    }

    #[test]
    fn main_and_nested_policies_reject_noncanonical_and_x32_syscall_numbers() {
        for low in [libc::SYS_rt_sigreturn, libc::SYS_execve, libc::SYS_mmap] {
            let number = (1_i64 << 32) | low;
            assert!(syscall_number_requires_enosys(number));
            let mut event = SyscallEvent {
                number,
                args: [0; 6],
                instruction_pointer: 0,
                result: UNSET_RESULT,
                context: 0,
                dispatch: SyscallDispatch::InstalledHook,
                guest_pkru: None,
            };
            forward_nested_tool_syscall(&mut event);
            assert_eq!(event.result, -i64::from(libc::ENOSYS));
        }
        for low in [512_i64, 513_i64, libc::SYS_getpid] {
            assert!(syscall_number_requires_enosys(low | X32_SYSCALL_BIT));
        }
        assert!(!syscall_number_requires_enosys(libc::SYS_getpid));
        assert!(!syscall_number_requires_enosys(-1));
    }

    #[test]
    fn legacy_modes_translate_vfork_to_cow_fork() {
        for mode in [super::TOOL_STRACE, super::TOOL_COMPAT, 0] {
            assert_eq!(
                legacy_physical_syscall_number(mode, libc::SYS_vfork),
                libc::SYS_fork,
            );
            assert_eq!(
                legacy_physical_syscall_number(mode, libc::SYS_fork),
                libc::SYS_fork,
            );
        }
        assert_eq!(
            legacy_physical_syscall_number(super::TOOL_REVERIE, libc::SYS_vfork),
            libc::SYS_vfork,
            "ToolHost owns its separate vfork translation path",
        );
    }

    #[test]
    fn translated_vfork_accepts_auto_reaped_sigchld_ignored_child() {
        let outer = unsafe { raw_syscall6(libc::SYS_fork, [0; 6]) };
        assert!(outer >= 0, "fork isolated SIGCHLD=SIG_IGN regression");
        if outer == 0 {
            let ignored = super::KernelSigaction {
                handler: libc::SIG_IGN as u64,
                ..super::KernelSigaction::default()
            };
            let installed = unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigaction,
                    [
                        libc::SIGCHLD as u64,
                        (&raw const ignored) as u64,
                        0,
                        core::mem::size_of::<u64>() as u64,
                        0,
                        0,
                    ],
                )
            };
            if installed != 0 {
                unsafe { raw_syscall6(libc::SYS_exit_group, [71, 0, 0, 0, 0, 0]) };
                unreachable!();
            }
            let child = unsafe { raw_syscall6(libc::SYS_fork, [0; 6]) };
            if child == 0 {
                unsafe { raw_syscall6(libc::SYS_exit_group, [0; 6]) };
                unreachable!();
            }
            let completed = child > 0 && unsafe { wait_for_translated_vfork_child(child) } == child;
            unsafe {
                raw_syscall6(
                    libc::SYS_exit_group,
                    [if completed { 0 } else { 72 }, 0, 0, 0, 0, 0],
                )
            };
            unreachable!();
        }

        let mut status = 0_i32;
        loop {
            let waited = unsafe {
                raw_syscall6(
                    libc::SYS_wait4,
                    [outer as u64, (&raw mut status) as u64, 0, 0, 0, 0],
                )
            };
            if waited == -i64::from(libc::EINTR) {
                continue;
            }
            assert_eq!(waited, outer);
            break;
        }
        assert_eq!(status, 0, "translated vfork SIGCHLD=SIG_IGN regression");
    }

    #[test]
    fn lifetime_patch_budget_and_sticky_exhaustion_bound_hot_retries() {
        let attempts = AtomicUsize::new(0);
        for expected in 0..MAX_LIFETIME_PATCH_ATTEMPTS {
            assert!(claim_lifetime_patch_attempt(&attempts));
            assert_eq!(attempts.load(Ordering::Acquire), expected + 1);
        }
        for _ in 0..(2 * ARENA_SLOTS) {
            assert!(!claim_lifetime_patch_attempt(&attempts));
        }

        let site = SiteSlot::new();
        let arena_full_attempts = AtomicUsize::new(0);
        let candidate_calls = Cell::new(0);
        let arena_full = run_bounded_patch_attempts(&arena_full_attempts, || {
            candidate_calls.set(candidate_calls.get() + 1);
            Err::<(), _>(TrampolineError::ArenaFull)
        })
        .unwrap_err();
        assert!(matches!(arena_full, InstallSiteError::Exhausted(_)));
        record_site_install_failure(&site, &arena_full);
        assert_eq!(site.state.load(Ordering::Acquire), SITE_EXHAUSTED);
        for _ in 0..(2 * ARENA_SLOTS) {
            mark_original_instruction_stale(&site, 0x050f);
            if site.state.load(Ordering::Acquire) == SITE_STALE {
                let _ = run_bounded_patch_attempts(&arena_full_attempts, || {
                    candidate_calls.set(candidate_calls.get() + 1);
                    Err::<(), _>(TrampolineError::ArenaFull)
                });
            }
        }
        assert_eq!(candidate_calls.get(), 1);

        site.state.store(SITE_FALLBACK, Ordering::Release);
        for _ in 0..(2 * ARENA_SLOTS) {
            mark_original_instruction_stale(&site, 0x050f);
            assert_eq!(site.state.load(Ordering::Acquire), SITE_FALLBACK);
        }

        mark_site_generation_stale(&site, 0x8000);
        assert_eq!(site.state.load(Ordering::Acquire), SITE_STALE);
        site.state.store(SITE_ACTIVE, Ordering::Release);
        mark_original_instruction_stale(&site, 0x050f);
        assert_eq!(site.state.load(Ordering::Acquire), SITE_STALE);

        site.state.store(SITE_EXHAUSTED, Ordering::Release);
        mark_original_instruction_stale(&site, 0x050f);
        mark_site_generation_stale(&site, 0x9000);
        assert_eq!(site.state.load(Ordering::Acquire), SITE_EXHAUSTED);
        assert_eq!(site.mapping_end.load(Ordering::Acquire), 0x9000);
    }

    #[test]
    fn raw_proc_maps_reader_is_bounded_and_covers_its_own_code() {
        let text = read_proc_self_maps().unwrap();
        assert!(!text.is_empty());
        assert!(text.len() <= MAX_PROC_SELF_MAPS_BYTES);

        let address = read_proc_self_maps as *const () as usize as u64;
        let maps = read_runtime_maps().unwrap();
        assert!(maps.iter().any(|mapping| {
            mapping.start <= address
                && address < mapping.end
                && mapping.readable
                && mapping.executable
                && !mapping.writable
        }));
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
        assert!(fallback_dispatch_count() > 0);
        assert!(super::fallback_refusal_count() > 0);

        FORK_HOOK.run_in_child();

        assert_eq!(fallback_dispatch_count(), 0);
        assert_eq!(super::fallback_refusal_count(), 0);
        assert_eq!(super::fallback_syscall_refusal_count(405), 0);
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

        mark_site_range_stale_in(
            SITES.get().unwrap(),
            address - 0x100,
            0x200,
            address + 0x100,
        );
        assert_eq!(site.state.load(Ordering::Acquire), SITE_STALE);
        assert_eq!(site.mapping_end.load(Ordering::Acquire), address + 0x100);

        let (same_site, claimed) = claim_site(address).unwrap();
        assert!(core::ptr::eq(site, same_site));
        assert!(claimed);
        assert_eq!(site.state.load(Ordering::Acquire), SITE_INSTALLING);
        site.state.store(SITE_FALLBACK, Ordering::Release);
    }

    fn active_mapping_site(address: u64, mapping_end: u64) -> SiteSlot {
        let site = SiteSlot::new();
        site.address.store(address, Ordering::Release);
        site.mapping_end.store(mapping_end, Ordering::Release);
        site.state.store(SITE_ACTIVE, Ordering::Release);
        site
    }

    fn installed_mapping_site(address: u64, state: u8) -> SiteSlot {
        let site = active_mapping_site(address, address + 4096);
        site.hook
            .store(core::ptr::NonNull::dangling().as_ptr(), Ordering::Release);
        site.state.store(state, Ordering::Release);
        site
    }

    fn mapping_event(number: i64, args: [u64; 6], result: i64) -> SyscallEvent {
        SyscallEvent {
            number,
            args,
            instruction_pointer: 0,
            result,
            context: 0,
            dispatch: SyscallDispatch::InstalledHook,
            guest_pkru: None,
        }
    }

    #[test]
    fn mapping_generation_rounds_mmap_munmap_and_retires_on_mremap() {
        let page = 4096_u64;
        for (raw_length, expected_length) in [
            (1, page),
            (page - 1, page),
            (page, page),
            (page + 1, 2 * page),
        ] {
            assert_eq!(
                checked_mapping_page_span(0x20_0000, raw_length, page, false),
                MappingPageSpan::Range {
                    start: 0x20_0000,
                    end: 0x20_0000 + expected_length,
                }
            );
        }

        let raw_length = page + 1;
        let mmap_start = 0x20_0000;
        let mmap_slots = [
            active_mapping_site(mmap_start + raw_length, mmap_start + page),
            active_mapping_site(mmap_start + 2 * page - 1, mmap_start + page),
        ];
        observe_mapping_generation_in(
            &mapping_event(
                libc::SYS_mmap,
                [0, raw_length, 0, 0, 0, 0],
                mmap_start as i64,
            ),
            page,
            &mmap_slots,
        );
        for site in &mmap_slots {
            assert_eq!(site.state.load(Ordering::Acquire), SITE_STALE);
            assert_eq!(
                site.mapping_end.load(Ordering::Acquire),
                mmap_start + 2 * page
            );
        }

        let munmap_start = 0x40_0000;
        let munmap_slots = [
            active_mapping_site(munmap_start + raw_length, munmap_start + 2 * page),
            active_mapping_site(munmap_start + 2 * page - 1, munmap_start + 2 * page),
        ];
        observe_mapping_generation_in(
            &mapping_event(libc::SYS_munmap, [munmap_start, raw_length, 0, 0, 0, 0], 0),
            page,
            &munmap_slots,
        );
        for site in &munmap_slots {
            assert_eq!(site.state.load(Ordering::Acquire), SITE_STALE);
            assert_eq!(site.mapping_end.load(Ordering::Acquire), 0);
        }

        let old_start = 0x60_0000;
        let new_start = 0x80_0000;
        for (flags, old_size, source_offsets, old_mapping_end) in [
            (
                libc::MREMAP_MAYMOVE as u64,
                page,
                [8, page - 8],
                old_start + page,
            ),
            (
                (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as u64,
                raw_length,
                [raw_length, 2 * page - 1],
                old_start + 2 * page,
            ),
        ] {
            let slots = [
                active_mapping_site(old_start + source_offsets[0], old_mapping_end),
                active_mapping_site(old_start + source_offsets[1], old_mapping_end),
                active_mapping_site(new_start + raw_length, new_start + page),
                active_mapping_site(new_start + 2 * page - 1, new_start + page),
            ];
            observe_mapping_generation_in(
                &mapping_event(
                    libc::SYS_mremap,
                    [old_start, old_size, raw_length, flags, 0, 0],
                    new_start as i64,
                ),
                page,
                &slots,
            );
            for site in &slots {
                assert_eq!(site.state.load(Ordering::Acquire), SITE_STALE);
                assert_eq!(site.mapping_end.load(Ordering::Acquire), 0);
            }
        }

        let inplace_start = 0xa0_0000;
        let inplace = [active_mapping_site(
            inplace_start + raw_length,
            inplace_start + page,
        )];
        observe_mapping_generation_in(
            &mapping_event(
                libc::SYS_mremap,
                [inplace_start, raw_length, raw_length, 0, 0, 0],
                inplace_start as i64,
            ),
            page,
            &inplace,
        );
        assert_eq!(inplace[0].state.load(Ordering::Acquire), SITE_STALE);
        assert_eq!(inplace[0].mapping_end.load(Ordering::Acquire), 0);

        let fixed_source = 0xc0_0000;
        let fixed_destination = 0xe0_0000;
        let fixed = [
            active_mapping_site(fixed_source + 8, fixed_source + page),
            active_mapping_site(fixed_destination + 8, fixed_destination + page),
            active_mapping_site(0x100_0000, 0x100_0000 + page),
        ];
        observe_mapping_generation_in(
            &mapping_event(
                libc::SYS_mremap,
                [
                    fixed_source,
                    page,
                    page,
                    (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64,
                    fixed_destination,
                    0,
                ],
                fixed_destination as i64,
            ),
            page,
            &fixed,
        );
        for site in &fixed {
            assert_eq!(site.state.load(Ordering::Acquire), SITE_STALE);
            assert_eq!(site.mapping_end.load(Ordering::Acquire), 0);
        }
    }

    #[test]
    fn remap_file_pages_floors_kernel_geometry_and_observes_only_success() {
        let page = 4096_u64;
        let page_start = 0x40_0000;
        let args = [page_start + 37, page + 511, 0, 7, 0, 0];
        assert_eq!(
            remap_file_pages_span(args, page),
            MappingPageSpan::Range {
                start: page_start,
                end: page_start + page,
            }
        );
        let active = [installed_mapping_site(page_start + 5, SITE_ACTIVE)];
        assert!(mapping_mutates_runtime_control_with(
            libc::SYS_remap_file_pages,
            args,
            page,
            &active,
            |_, _, _| false,
        ));

        let invalid_flags = [page_start, page, 0, 0, 1, 0];
        assert_eq!(
            remap_file_pages_span(invalid_flags, page),
            MappingPageSpan::Invalid
        );
        assert!(
            !mapping_mutates_runtime_control_with(
                libc::SYS_remap_file_pages,
                invalid_flags,
                page,
                &active,
                |_, _, _| false,
            ),
            "nonzero remap_file_pages flags overlapping a protected site must reach native EINVAL"
        );

        for invalid in [
            [page_start, page - 1, 0, 0, 0, 0],
            [page_start, page, 1, 0, 0, 0],
            [page_start, page, 0, u64::MAX, 0, 0],
            [u64::MAX - (page - 1), page, 0, 0, 0, 0],
        ] {
            assert_eq!(
                remap_file_pages_span(invalid, page),
                MappingPageSpan::Invalid
            );
            assert!(!mapping_mutates_runtime_control_with(
                libc::SYS_remap_file_pages,
                invalid,
                page,
                &active,
                |_, _, _| false,
            ));
        }
        assert_eq!(
            remap_file_pages_span(args, page - 1),
            MappingPageSpan::Invalid
        );

        let successful = [
            active_mapping_site(page_start + 5, page_start + page),
            active_mapping_site(page_start + page, page_start + 2 * page),
        ];
        observe_mapping_generation_in(
            &mapping_event(libc::SYS_remap_file_pages, args, 0),
            page,
            &successful,
        );
        assert_eq!(successful[0].state.load(Ordering::Acquire), SITE_STALE);
        assert_eq!(successful[1].state.load(Ordering::Acquire), SITE_ACTIVE);

        let failed = [active_mapping_site(page_start + 5, page_start + page)];
        observe_mapping_generation_in(
            &mapping_event(libc::SYS_remap_file_pages, args, -i64::from(libc::EINVAL)),
            page,
            &failed,
        );
        assert_eq!(failed[0].state.load(Ordering::Acquire), SITE_ACTIVE);
    }

    #[test]
    fn fixed_mapping_controls_preserve_native_errors_and_protected_ranges() {
        let page = 4096_u64;
        let source = 0x20_0000;
        let destination = 0x40_0000;
        let arena_start = 0x80_0000;
        let arena_end = arena_start + page;
        let overlaps_arena = |start, end, _| start < arena_end && arena_start < end;
        let active = [installed_mapping_site(source + 8, SITE_ACTIVE)];

        let overlapping_fixed_mmap = [
            arena_start,
            page,
            libc::PROT_READ as u64,
            (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
            u64::MAX,
            0,
        ];
        assert!(mapping_mutates_runtime_control_with(
            libc::SYS_mmap,
            overlapping_fixed_mmap,
            page,
            &[],
            overlaps_arena,
        ));
        let disjoint_fixed_mmap = [
            0x90_0000,
            1,
            libc::PROT_READ as u64,
            (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
            u64::MAX,
            0,
        ];
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_mmap,
            disjoint_fixed_mmap,
            page,
            &active,
            overlaps_arena,
        ));
        for noreplace_flags in [
            libc::MAP_FIXED_NOREPLACE,
            libc::MAP_FIXED | libc::MAP_FIXED_NOREPLACE,
        ] {
            let fixed_noreplace = [
                arena_start,
                page,
                libc::PROT_READ as u64,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | noreplace_flags) as u64,
                u64::MAX,
                0,
            ];
            assert!(!mapping_mutates_runtime_control_with(
                libc::SYS_mmap,
                fixed_noreplace,
                page,
                &active,
                overlaps_arena,
            ));
        }
        for invalid_fixed_mmap in [
            [
                arena_start,
                0,
                libc::PROT_READ as u64,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
                u64::MAX,
                0,
            ],
            [
                arena_start + 1,
                page,
                libc::PROT_READ as u64,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
                u64::MAX,
                0,
            ],
            [
                u64::MAX - page + 1,
                page,
                libc::PROT_READ as u64,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
                u64::MAX,
                0,
            ],
        ] {
            assert!(!mapping_mutates_runtime_control_with(
                libc::SYS_mmap,
                invalid_fixed_mmap,
                page,
                &active,
                overlaps_arena,
            ));
        }

        let fixed_destination_overlaps = [
            source,
            page,
            page,
            (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64,
            arena_start,
            0,
        ];
        assert!(mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            fixed_destination_overlaps,
            page,
            &[],
            overlaps_arena,
        ));
        let disjoint_fixed_mremap = [
            source,
            page,
            page,
            (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64,
            destination,
            0,
        ];
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            disjoint_fixed_mremap,
            page,
            &[],
            overlaps_arena,
        ));
        for invalid_fixed_mremap in [
            [
                source,
                page,
                page,
                libc::MREMAP_FIXED as u64,
                arena_start,
                0,
            ],
            [
                source,
                page,
                page,
                (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64,
                arena_start + 1,
                0,
            ],
        ] {
            assert!(!mapping_mutates_runtime_control_with(
                libc::SYS_mremap,
                invalid_fixed_mremap,
                page,
                &active,
                overlaps_arena,
            ));
        }

        let moved = [source, page, page, libc::MREMAP_MAYMOVE as u64, 0, 0];
        assert_eq!(
            mremap_source_page_span(moved, page),
            MappingPageSpan::Range {
                start: source,
                end: source + page,
            }
        );
        assert!(mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            moved,
            page,
            &active,
            |_, _, _| false,
        ));

        // A zero-old-size clone succeeds only for a shareable source mapping.
        // Installed source sites are authenticated private RX mappings, so an
        // attempted clone must reach the kernel's native EINVAL.
        let stale = [installed_mapping_site(source + 16, SITE_STALE)];
        let cloned_site = [source, 0, page, libc::MREMAP_MAYMOVE as u64, 0, 0];
        assert_eq!(
            mremap_source_page_span(cloned_site, page),
            MappingPageSpan::Range {
                start: source,
                end: source + page,
            }
        );
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            cloned_site,
            page,
            &stale,
            |_, _, _| false,
        ));

        let dont_unmap_clone = [
            source,
            0,
            page,
            (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as u64,
            0,
            0,
        ];
        assert_eq!(
            mremap_source_page_span(dont_unmap_clone, page),
            MappingPageSpan::Invalid
        );
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            dont_unmap_clone,
            page,
            &stale,
            |_, _, _| false,
        ));

        let invalid_clone = [source, 0, page, 0, 0, 0];
        assert_eq!(
            mremap_source_page_span(invalid_clone, page),
            MappingPageSpan::Invalid
        );
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            invalid_clone,
            page,
            &active,
            |_, _, _| false,
        ));

        let unrelated = [0x60_0000, page, page, libc::MREMAP_MAYMOVE as u64, 0, 0];
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            unrelated,
            page,
            &active,
            |_, _, _| false,
        ));

        let arena_source = [arena_start, page, page, libc::MREMAP_MAYMOVE as u64, 0, 0];
        assert!(mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            arena_source,
            page,
            &[],
            overlaps_arena,
        ));
        let arena_clone = [arena_start, 0, page, libc::MREMAP_MAYMOVE as u64, 0, 0];
        assert!(mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            arena_clone,
            page,
            &[],
            overlaps_arena,
        ));

        let fixed_overlapping_clone = [
            arena_start,
            0,
            2 * page,
            (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64,
            arena_start + page,
            0,
        ];
        assert!(mremap_effect_spans(fixed_overlapping_clone, page).is_ok());
        assert!(mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            fixed_overlapping_clone,
            page,
            &[],
            overlaps_arena,
        ));

        for invalid_dont_unmap_hint in [
            [
                source,
                page,
                page,
                (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as u64,
                source + 1,
                0,
            ],
            [
                source,
                page,
                page,
                (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as u64,
                source,
                0,
            ],
        ] {
            assert_eq!(
                mremap_source_page_span(invalid_dont_unmap_hint, page),
                MappingPageSpan::Invalid
            );
        }
        let dont_unmap_hinting_arena = [
            source,
            page,
            page,
            (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as u64,
            arena_start,
            0,
        ];
        assert!(mremap_effect_spans(dont_unmap_hinting_arena, page).is_ok());
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            dont_unmap_hinting_arena,
            page,
            &[],
            |start, end, _| start < arena_end && arena_start < end,
        ));

        for invalid in [
            [
                source,
                page,
                page,
                libc::MREMAP_FIXED as u64,
                destination,
                0,
            ],
            [source, page, 0, 0, 0, 0],
            [source, page, page, 1 << 8, 0, 0],
            [
                source,
                page,
                2 * page,
                (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as u64,
                0,
                0,
            ],
        ] {
            assert_eq!(
                mremap_source_page_span(invalid, page),
                MappingPageSpan::Invalid
            );
            assert!(!mapping_mutates_runtime_control_with(
                libc::SYS_mremap,
                invalid,
                page,
                &active,
                |_, _, _| false,
            ));
        }

        let fixed_destination = [
            0xa0_0000,
            page,
            page,
            (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64,
            destination,
            0,
        ];
        assert_eq!(
            mremap_fixed_destination_page_span(fixed_destination, page),
            MappingPageSpan::Range {
                start: destination,
                end: destination + page,
            }
        );
        assert!(mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            fixed_destination,
            page,
            &[],
            |start, end, _| start < destination + page && destination < end,
        ));
        assert!(mapping_mutates_runtime_control_with(
            libc::SYS_mremap,
            unrelated,
            0,
            &[],
            |_, _, _| false,
        ));
    }

    #[test]
    fn mapping_control_preserves_arena_aliases_and_installed_site_protections() {
        let page = 4096_u64;
        let arena_start = 0x80_0000;
        let arena_end = arena_start + page;
        let overlaps_arena = |start, end, _| start < arena_end && arena_start < end;
        for (number, args) in [
            (
                libc::SYS_mmap,
                [
                    arena_start,
                    page,
                    libc::PROT_READ as u64,
                    (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
                    u64::MAX,
                    0,
                ],
            ),
            (libc::SYS_munmap, [arena_start, page, 0, 0, 0, 0]),
            (
                libc::SYS_mprotect,
                [arena_start, page, libc::PROT_NONE as u64, 0, 0, 0],
            ),
            (
                libc::SYS_pkey_mprotect,
                [arena_start, page, libc::PROT_READ as u64, 1, 0, 0],
            ),
        ] {
            assert!(mapping_mutates_runtime_control_with(
                number,
                args,
                page,
                &[],
                overlaps_arena,
            ));
        }

        let site_start = 0x20_0000;
        let installed = [installed_mapping_site(site_start + 8, SITE_STALE)];
        for number in [libc::SYS_mprotect, libc::SYS_pkey_mprotect] {
            assert!(mapping_mutates_runtime_control_with(
                number,
                [site_start, page, libc::PROT_READ as u64, 0, 0, 0],
                page,
                &installed,
                |_, _, _| false,
            ));
        }
        for (number, args) in [
            (
                libc::SYS_mprotect,
                [
                    site_start,
                    page,
                    (libc::PROT_READ | libc::PROT_EXEC) as u64,
                    0,
                    0,
                    0,
                ],
            ),
            (
                libc::SYS_pkey_mprotect,
                [
                    site_start,
                    page,
                    (libc::PROT_READ | libc::PROT_EXEC) as u64,
                    0,
                    0,
                    0,
                ],
            ),
        ] {
            assert!(!mapping_mutates_runtime_control_with(
                number,
                args,
                page,
                &installed,
                |_, _, _| false,
            ));
        }
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_mprotect,
            [
                arena_start,
                page,
                (libc::PROT_READ | libc::PROT_WRITE) as u64,
                0,
                0,
                0,
            ],
            page,
            &[],
            |start, end, requested| {
                start < arena_end
                    && arena_start < end
                    && requested != Some(libc::PROT_READ | libc::PROT_WRITE)
            },
        ));
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_mprotect,
            [site_start, 0, libc::PROT_NONE as u64, 0, 0, 0],
            page,
            &installed,
            |_, _, _| false,
        ));
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_munmap,
            [site_start, page, 0, 0, 0, 0],
            page,
            &installed,
            |_, _, _| false,
        ));
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_mmap,
            [
                site_start,
                page,
                libc::PROT_READ as u64,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
                u64::MAX,
                0,
            ],
            page,
            &installed,
            |_, _, _| false,
        ));
        assert!(!mapping_mutates_runtime_control_with(
            libc::SYS_mmap,
            [
                0,
                page,
                libc::PROT_READ as u64,
                libc::MAP_PRIVATE as u64,
                3,
                0
            ],
            page,
            &installed,
            overlaps_arena,
        ));
        for (number, args) in [
            (
                libc::SYS_mmap,
                [
                    arena_start,
                    0,
                    libc::PROT_READ as u64,
                    libc::MAP_FIXED as u64,
                    3,
                    0,
                ],
            ),
            (libc::SYS_munmap, [arena_start + 1, page, 0, 0, 0, 0]),
        ] {
            assert!(!mapping_mutates_runtime_control_with(
                number,
                args,
                page,
                &installed,
                overlaps_arena,
            ));
        }
    }

    #[test]
    fn madvise_low_c_int_classification_preserves_or_invalidates_exact_patch_windows() {
        const PRESERVING: &[u64] = &[
            0, 1, 2, 3, 11, 12, 13, 14, 15, 16, 17, 19, 20, 21, 22, 23, 25,
        ];
        const DESTRUCTIVE_OR_UNKNOWN: &[u64] =
            &[4, 8, 9, 10, 18, 24, 26, 100, 101, 102, 103, u32::MAX as u64];
        let page = 4096_u64;
        let advised_page = 0x40_0000;
        let straddling_site = advised_page - 4;

        for advice in PRESERVING {
            for raw in [*advice, *advice | 0xfeed_beef_0000_0000] {
                assert!(madvise_preserves_source_generation(raw));
                let installed = [installed_mapping_site(straddling_site, SITE_ACTIVE)];
                assert!(!mapping_mutates_runtime_control_with(
                    libc::SYS_madvise,
                    [advised_page, 1, raw, 0, 0, 0],
                    page,
                    &installed,
                    |_, _, _| false,
                ));
                observe_mapping_generation_in(
                    &mapping_event(libc::SYS_madvise, [advised_page, 1, raw, 0, 0, 0], 0),
                    page,
                    &installed,
                );
                assert_eq!(installed[0].state.load(Ordering::Acquire), SITE_ACTIVE);
            }
        }

        for advice in DESTRUCTIVE_OR_UNKNOWN {
            for raw in [*advice, *advice | 0xfeed_beef_0000_0000] {
                assert!(!madvise_preserves_source_generation(raw));
                let installed = [installed_mapping_site(straddling_site, SITE_FALLBACK)];
                assert!(mapping_mutates_runtime_control_with(
                    libc::SYS_madvise,
                    [advised_page, 1, raw, 0, 0, 0],
                    page,
                    &installed,
                    |_, _, _| false,
                ));
                observe_mapping_generation_in(
                    &mapping_event(libc::SYS_madvise, [advised_page, 1, raw, 0, 0, 0], 0),
                    page,
                    &installed,
                );
                assert_eq!(installed[0].state.load(Ordering::Acquire), SITE_STALE);
            }
        }

        let failed = [installed_mapping_site(straddling_site, SITE_ACTIVE)];
        observe_mapping_generation_in(
            &mapping_event(
                libc::SYS_madvise,
                [advised_page, 1, 4, 0, 0, 0],
                -i64::from(libc::EINVAL),
            ),
            page,
            &failed,
        );
        assert_eq!(failed[0].state.load(Ordering::Acquire), SITE_ACTIVE);

        let empty = [installed_mapping_site(straddling_site, SITE_ACTIVE)];
        observe_mapping_generation_in(
            &mapping_event(libc::SYS_madvise, [advised_page, 0, 4, 0, 0, 0], 0),
            page,
            &empty,
        );
        assert_eq!(empty[0].state.load(Ordering::Acquire), SITE_ACTIVE);

        let rounded_tail = [installed_mapping_site(advised_page + page + 4, SITE_ACTIVE)];
        observe_mapping_generation_in(
            &mapping_event(libc::SYS_madvise, [advised_page, page + 1, 4, 0, 0, 0], 0),
            page,
            &rounded_tail,
        );
        assert_eq!(rounded_tail[0].state.load(Ordering::Acquire), SITE_STALE);
    }

    #[test]
    fn successful_mremap_retires_all_sites_while_failed_events_preserve_them() {
        let page = 4096_u64;
        let old_start = 0x20_0000;
        let new_start = 0x40_0000;
        let raw_length = page + 1;
        assert_eq!(
            checked_mapping_page_span(old_start, 0, page, true),
            MappingPageSpan::Empty
        );

        let slots = [
            active_mapping_site(old_start, old_start + page),
            active_mapping_site(new_start + raw_length, new_start + page),
        ];
        observe_mapping_generation_in(
            &mapping_event(
                libc::SYS_mremap,
                [old_start, 0, raw_length, libc::MREMAP_MAYMOVE as u64, 0, 0],
                new_start as i64,
            ),
            page,
            &slots,
        );
        for site in &slots {
            assert_eq!(site.state.load(Ordering::Acquire), SITE_STALE);
            assert_eq!(site.mapping_end.load(Ordering::Acquire), 0);
        }

        let failed = [active_mapping_site(old_start, old_start + page)];
        observe_mapping_generation_in(
            &mapping_event(
                libc::SYS_mremap,
                [old_start, page, page, libc::MREMAP_MAYMOVE as u64, 0, 0],
                -i64::from(libc::EINVAL),
            ),
            page,
            &failed,
        );
        assert_eq!(failed[0].state.load(Ordering::Acquire), SITE_ACTIVE);
        assert_eq!(
            failed[0].mapping_end.load(Ordering::Acquire),
            old_start + page
        );

        let failed_mremap = mapping_event(
            libc::SYS_mremap,
            [old_start, page, page, libc::MREMAP_MAYMOVE as u64, 0, 0],
            -i64::from(libc::EINVAL),
        );
        let mut arena_invalidations = Vec::new();
        observe_arena_source_generation_with(&failed_mremap, page, |span| {
            arena_invalidations.push(span)
        });
        assert!(arena_invalidations.is_empty());

        let successful_mremap = mapping_event(
            libc::SYS_mremap,
            [old_start, page, 2 * page, libc::MREMAP_MAYMOVE as u64, 0, 0],
            new_start as i64,
        );
        observe_arena_source_generation_with(&successful_mremap, page, |span| {
            arena_invalidations.push(span)
        });
        assert_eq!(arena_invalidations, vec![MappingPageSpan::Invalid]);
    }

    #[test]
    fn successful_invalid_mapping_events_stale_every_site_conservatively() {
        let page = 4096_u64;
        assert_eq!(
            checked_mapping_page_span(0x20_0000, 0, page, false),
            MappingPageSpan::Invalid
        );
        assert_eq!(
            checked_mapping_page_span(0x20_0001, page, page, false),
            MappingPageSpan::Invalid
        );
        assert_eq!(
            checked_mapping_page_span(0x20_0000, u64::MAX, page, false),
            MappingPageSpan::Invalid
        );
        assert_eq!(
            checked_mapping_page_span(u64::MAX - (page - 1), page, page, false),
            MappingPageSpan::Invalid
        );
        assert_eq!(
            checked_mapping_page_span(0x20_0000, page, 0, false),
            MappingPageSpan::Invalid
        );
        assert_eq!(
            checked_mapping_page_span(0x20_0000, page, page - 1, false),
            MappingPageSpan::Invalid
        );

        let invalid_events = [
            (
                mapping_event(libc::SYS_mmap, [0, 0, 0, 0, 0, 0], 0x20_0000),
                page,
            ),
            (
                mapping_event(libc::SYS_munmap, [0x20_0001, page, 0, 0, 0, 0], 0),
                page,
            ),
            (
                mapping_event(libc::SYS_munmap, [0x20_0000, u64::MAX, 0, 0, 0, 0], 0),
                page,
            ),
            (
                mapping_event(
                    libc::SYS_munmap,
                    [u64::MAX - (page - 1), page, 0, 0, 0, 0],
                    0,
                ),
                page,
            ),
            (
                mapping_event(libc::SYS_mremap, [0x20_0000, 0, page, 0, 0, 0], 0x40_0000),
                page,
            ),
            (
                mapping_event(
                    libc::SYS_mremap,
                    [0x20_0000, page, 0, libc::MREMAP_MAYMOVE as u64, 0, 0],
                    0x40_0000,
                ),
                page,
            ),
            (
                mapping_event(
                    libc::SYS_mremap,
                    [0x20_0000, page, page, 1 << 8, 0, 0],
                    0x40_0000,
                ),
                page,
            ),
            (
                mapping_event(
                    libc::SYS_mremap,
                    [
                        0x20_0000,
                        page,
                        page,
                        libc::MREMAP_FIXED as u64,
                        0x40_0000,
                        0,
                    ],
                    0x40_0000,
                ),
                page,
            ),
            (
                mapping_event(
                    libc::SYS_mremap,
                    [0x20_0000, 2 * page, page, libc::MREMAP_MAYMOVE as u64, 0, 0],
                    0x40_0000,
                ),
                page,
            ),
            (
                mapping_event(
                    libc::SYS_mremap,
                    [0x20_0000, page, page, libc::MREMAP_MAYMOVE as u64, 0, 0],
                    0x40_0000,
                ),
                page,
            ),
            (
                mapping_event(
                    libc::SYS_mremap,
                    [
                        0x20_0000,
                        page,
                        page,
                        (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64,
                        0x40_0000,
                        0,
                    ],
                    0x50_0000,
                ),
                page,
            ),
            (
                mapping_event(
                    libc::SYS_mremap,
                    [
                        0x20_0000,
                        page,
                        2 * page,
                        (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as u64,
                        0,
                        0,
                    ],
                    0x40_0000,
                ),
                page,
            ),
            (
                mapping_event(
                    libc::SYS_mremap,
                    [0x20_0000, 2 * page, page, libc::MREMAP_MAYMOVE as u64, 0, 0],
                    0x20_1000,
                ),
                page,
            ),
            (
                mapping_event(
                    libc::SYS_mremap,
                    [0x20_0000, 0, page, libc::MREMAP_MAYMOVE as u64, 0, 0],
                    0x20_0000,
                ),
                page,
            ),
            (
                mapping_event(
                    libc::SYS_mremap,
                    [0x20_0000, page, page, 0, 0, 0],
                    0x40_0000,
                ),
                page,
            ),
            (
                mapping_event(libc::SYS_munmap, [0x20_0000, page, 0, 0, 0, 0], 0),
                page - 1,
            ),
        ];
        for (event, page_size) in &invalid_events {
            let slots = [
                active_mapping_site(0x60_0000, 0x60_0000 + page),
                active_mapping_site(0x80_0000, 0x80_0000 + page),
            ];
            slots[1].state.store(SITE_FALLBACK, Ordering::Release);
            observe_mapping_generation_in(event, *page_size, &slots);
            for site in &slots {
                assert_eq!(site.state.load(Ordering::Acquire), SITE_STALE);
                assert_eq!(site.mapping_end.load(Ordering::Acquire), 0);
            }
        }

        let sticky = [active_mapping_site(0x60_0000, 0x60_0000 + page)];
        sticky[0].state.store(SITE_EXHAUSTED, Ordering::Release);
        observe_mapping_generation_in(&invalid_events[0].0, page, &sticky);
        assert_eq!(sticky[0].state.load(Ordering::Acquire), SITE_EXHAUSTED);
        assert_eq!(sticky[0].mapping_end.load(Ordering::Acquire), 0);
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

    #[test]
    fn clone3_is_refused_before_dereferencing_guest_arguments() {
        assert_eq!(
            clone3_refusal_result(TOOL_REVERIE),
            -i64::from(libc::ENOTSUP)
        );
        assert_eq!(
            clone3_refusal_result(TOOL_STRACE),
            -i64::from(libc::ENOTSUP)
        );
        assert_eq!(clone3_refusal_result(TOOL_COMPAT), -i64::from(libc::EPERM));
    }
}
