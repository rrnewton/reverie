mod installed_callback;
use core::arch::global_asm;
use core::mem::size_of;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicI32;
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::AtomicU8;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::cell::Cell;
use std::ffi::OsStr;
use std::io;
use std::os::fd::AsRawFd;
use std::ptr;
use std::sync::OnceLock;

use installed_callback::InstalledCallback;
use liteinst2::patcher::PatchError;
use liteinst2::patcher::prepare_live_patching;
use liteinst2::scanner::InstructionScanner;
use liteinst2::trampoline::HookContext;
use liteinst2::trampoline::HookSite;
use liteinst2::trampoline::InstalledHook;
use liteinst2::trampoline::TrampolineArena;
use liteinst2::trampoline::TrampolineError;
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
use crate::rcb;

mod private_fd;

#[cfg(feature = "rcb-qualification")]
const PREPARED_FORK_PROBE_IDLE: i32 = -1;
#[cfg(feature = "rcb-qualification")]
const PREPARED_FORK_PROBE_CLAIMED: i32 = -2;
#[cfg(feature = "rcb-qualification")]
static PREPARED_FORK_PROBE_SEND: AtomicI32 = AtomicI32::new(PREPARED_FORK_PROBE_IDLE);
#[cfg(feature = "rcb-qualification")]
static PREPARED_FORK_PROBE_RECEIVE: AtomicI32 = AtomicI32::new(PREPARED_FORK_PROBE_IDLE);
#[cfg(feature = "rcb-qualification")]
static PREPARED_FORK_PROBE_RESULT: AtomicI32 = AtomicI32::new(i32::MIN);
#[cfg(feature = "rcb-qualification")]
static PREPARED_FORK_PROBE_GENERATION: AtomicU64 = AtomicU64::new(0);

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

pub(crate) struct ToolCallbackGuard {
    previous: bool,
}

impl ToolCallbackGuard {
    pub(crate) fn enter() -> Self {
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

// This backend currently admits one Tool thread per process; ordinary fork
// gives the child a private copy. This slot is an admitted-route safeguard,
// not descriptor virtualization or protection from externally acquired aliases.
static RCB_FD: AtomicI32 = AtomicI32::new(-1);
// Linux _IOR('$', 7, __u64): read the stable kernel event identity.
const PERF_EVENT_IOC_ID: u64 = 0x8008_2407;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RcbAccounting {
    entry: u64,
    deduction: u64,
    last_sample: u64,
    last_public: u64,
    depth: u32,
    error: i32,
}

impl RcbAccounting {
    const fn new(depth: u32) -> Self {
        Self {
            entry: 0,
            deduction: 0,
            last_sample: 0,
            last_public: 0,
            depth,
            error: 0,
        }
    }

    fn live(&self) -> Result<(), i32> {
        if self.error == 0 {
            Ok(())
        } else {
            Err(self.error)
        }
    }

    fn observe(&mut self, sample: u64) -> Result<(), i32> {
        self.live()?;
        if sample < self.last_sample {
            return Err(libc::ESTALE);
        }
        self.last_sample = sample;
        Ok(())
    }

    fn enter(&mut self, sample: Option<u64>) -> Result<(), i32> {
        self.live()?;
        let depth = self.depth.checked_add(1).ok_or(libc::EOVERFLOW)?;
        if let Some(sample) = sample {
            self.observe(sample)?;
            if self.depth == 0 {
                self.entry = sample;
            }
        }
        self.depth = depth;
        Ok(())
    }

    fn leave(&mut self, sample: Option<u64>) -> Result<(), i32> {
        self.live()?;
        let depth = self.depth.checked_sub(1).ok_or(libc::ESTALE)?;
        if depth == 0 {
            if let Some(sample) = sample {
                self.observe(sample)?;
                let elapsed = sample.checked_sub(self.entry).ok_or(libc::ESTALE)?;
                self.deduction = self
                    .deduction
                    .checked_add(elapsed)
                    .ok_or(libc::EOVERFLOW)?;
            }
            self.entry = 0;
        }
        self.depth = depth;
        Ok(())
    }

    fn public(&mut self, sample: u64) -> Result<u64, i32> {
        self.observe(sample)?;
        let active = if self.depth == 0 {
            0
        } else {
            sample.checked_sub(self.entry).ok_or(libc::ESTALE)?
        };
        let value = sample
            .checked_sub(self.deduction)
            .and_then(|value| value.checked_sub(active))
            .ok_or(libc::EOVERFLOW)?;
        if value < self.last_public {
            return Err(libc::ESTALE);
        }
        self.last_public = value;
        Ok(value)
    }

    fn break_with(&mut self, errno: i32) -> i32 {
        if self.error == 0 {
            self.error = if errno > 0 { errno } else { libc::EIO };
        }
        self.error
    }
}

thread_local! {
    static RCB_CLOCK: Cell<*mut reverie_ptrace::InGuestRcbCounter> =
        const { Cell::new(ptr::null_mut()) };
    static RCB_CLOCK_OWNER: Cell<libc::pid_t> = const { Cell::new(0) };
    static RCB_EVENT_ID: Cell<u64> = const { Cell::new(0) };
    static RCB_CLOCK_UNAVAILABLE: Cell<bool> = const { Cell::new(false) };
    static RCB_ACCOUNTING: Cell<RcbAccounting> = const { Cell::new(RcbAccounting::new(0)) };
}

#[cfg(feature = "rcb-qualification")]
thread_local! {
    // [phase, inherited mapping start, inherited mapping end, mincore errno,
    // process_vm_readv errno, overlapping /proc/self/maps ranges].
    static FORK_PERF_MAPPING_PROBE: Cell<[u64; 6]> = const { Cell::new([0; 6]) };
}

#[cfg(feature = "rcb-qualification")]
pub(crate) fn arm_inherited_perf_mapping_probe_for_test(
    start: u64,
    end: u64,
) -> io::Result<()> {
    if start == 0 || start >= end || start & 4095 != 0 || end - start != 4096 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    FORK_PERF_MAPPING_PROBE.with(|probe| {
        if probe.get() != [0; 6] {
            Err(io::Error::from_raw_os_error(libc::EALREADY))
        } else {
            probe.set([1, start, end, 0, 0, 0]);
            Ok(())
        }
    })
}

#[cfg(feature = "rcb-qualification")]
pub(crate) fn inherited_perf_mapping_probe_for_test() -> io::Result<[u64; 5]> {
    FORK_PERF_MAPPING_PROBE.with(|probe| {
        let proof = probe.get();
        if proof[0] != 2 {
            Err(io::Error::from_raw_os_error(libc::ESTALE))
        } else {
            Ok([proof[1], proof[2], proof[3], proof[4], proof[5]])
        }
    })
}

#[cfg(feature = "rcb-qualification")]
fn raw_errno(result: i64) -> i32 {
    i32::try_from(-result)
        .ok()
        .filter(|errno| *errno > 0)
        .unwrap_or(libc::EIO)
}

#[cfg(feature = "rcb-qualification")]
fn parse_hex_address(bytes: &[u8]) -> Option<u64> {
    let mut value = 0_u64;
    if bytes.is_empty() {
        return None;
    }
    for byte in bytes {
        let digit = match *byte {
            b'0'..=b'9' => u64::from(*byte - b'0'),
            b'a'..=b'f' => u64::from(*byte - b'a' + 10),
            b'A'..=b'F' => u64::from(*byte - b'A' + 10),
            _ => return None,
        };
        value = value.checked_mul(16)?.checked_add(digit)?;
    }
    Some(value)
}

#[cfg(feature = "rcb-qualification")]
fn raw_maps_overlap(start: u64, end: u64) -> io::Result<bool> {
    const MAPS: &[u8] = b"/proc/self/maps\0";
    let fd = unsafe {
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
    if fd < 0 {
        return Err(io::Error::from_raw_os_error(raw_errno(fd)));
    }
    let mut buffer = [0_u8; 4096];
    let mut prefix = [0_u8; 40];
    let mut prefix_len = 0_usize;
    let mut collecting = true;
    let mut overlap = false;
    let mut scan_error = None;
    loop {
        let result = unsafe {
            raw_syscall6(
                libc::SYS_read,
                [
                    fd as u64,
                    buffer.as_mut_ptr() as u64,
                    buffer.len() as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        if result < 0 {
            scan_error = Some(raw_errno(result));
            break;
        }
        if result == 0 {
            break;
        }
        for byte in &buffer[..result as usize] {
            if *byte == b'\n' {
                prefix_len = 0;
                collecting = true;
            } else if collecting && (*byte == b' ' || *byte == b'\t') {
                collecting = false;
                let Some(dash) = prefix[..prefix_len]
                    .iter()
                    .position(|byte| *byte == b'-')
                else {
                    scan_error = Some(libc::EIO);
                    break;
                };
                let Some(map_start) = parse_hex_address(&prefix[..dash]) else {
                    scan_error = Some(libc::EIO);
                    break;
                };
                let Some(map_end) = parse_hex_address(&prefix[dash + 1..prefix_len]) else {
                    scan_error = Some(libc::EIO);
                    break;
                };
                overlap |= map_start < end && start < map_end;
            } else if collecting {
                if prefix_len == prefix.len() {
                    scan_error = Some(libc::EOVERFLOW);
                    break;
                }
                prefix[prefix_len] = *byte;
                prefix_len += 1;
            }
        }
        if scan_error.is_some() {
            break;
        }
    }
    let close_result = unsafe { raw_syscall6(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]) };
    if let Some(errno) = scan_error {
        return Err(io::Error::from_raw_os_error(errno));
    }
    if close_result != 0 {
        return Err(io::Error::from_raw_os_error(raw_errno(close_result)));
    }
    Ok(overlap)
}

#[cfg(feature = "rcb-qualification")]
fn verify_inherited_perf_mapping_absent() -> io::Result<()> {
    FORK_PERF_MAPPING_PROBE.with(|probe| {
        let mut proof = probe.get();
        if proof[0] == 0 {
            return Ok(());
        }
        if proof[0] != 1 {
            return Err(io::Error::from_raw_os_error(libc::ESTALE));
        }
        let start = proof[1];
        let end = proof[2];
        let mut residency = 0_u8;
        let mincore = unsafe {
            raw_syscall6(
                libc::SYS_mincore,
                [start, end - start, (&raw mut residency) as u64, 0, 0, 0],
            )
        };
        let mut byte = 0_u8;
        let local = libc::iovec {
            iov_base: (&raw mut byte).cast(),
            iov_len: 1,
        };
        let remote = libc::iovec {
            iov_base: start as usize as *mut libc::c_void,
            iov_len: 1,
        };
        let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
        if pid <= 0 {
            return Err(io::Error::from_raw_os_error(libc::ESRCH));
        }
        let access = unsafe {
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
        let overlap = raw_maps_overlap(start, end)?;
        if mincore != -i64::from(libc::ENOMEM)
            || access != -i64::from(libc::EFAULT)
            || overlap
        {
            return Err(io::Error::from_raw_os_error(libc::ESTALE));
        }
        proof[0] = 2;
        proof[3] = libc::ENOMEM as u64;
        proof[4] = libc::EFAULT as u64;
        proof[5] = 0;
        probe.set(proof);
        Ok(())
    })
}

/// Acquire the current thread's in-guest RCB clock while physically disabled.
/// Root setup and each admitted child retain their distinct release authority.
pub(crate) fn initialize_rcb_clock<G: reverie::GlobalTool>(
    rpc: &crate::rpc::CoordinatorRpc<G>,
) -> io::Result<()> {
    rpc.verify_cpu_binding()?;
    let cpu = rpc.bound_cpu();
    let paused = crate::syscall_fallback::needs_paused_child_clock();
    let mut reservation = None;
    let mut event_id = 0;
    let initialized = initialize_rcb_clock_with_mode(
        || {
            let (clock, protection, acquired_event_id) = rpc.acquire_clock()?;
            reservation = protection;
            event_id = acquired_event_id;
            Ok(clock)
        },
        paused,
        cpu,
    );
    if let Err(error) = initialized {
        rcb::setup_failed(error.raw_os_error().unwrap_or(libc::EIO));
        return Err(error);
    }
    if let Err(error) = rpc.verify_cpu_binding().and_then(|()| rpc.acknowledge_clock()) {
        // Revoke boundary authority before dropping only this process's owner.
        let clock = RCB_CLOCK.replace(ptr::null_mut());
        let fd = RCB_FD.swap(-1, Ordering::AcqRel);
        rcb::setup_failed(error.raw_os_error().unwrap_or(libc::EIO));
        if fd >= 0 {
            unsafe {
                raw_syscall6(libc::SYS_ioctl, [fd as u64, 0x2401, 0, 0, 0, 0]);
            }
        }
        if !clock.is_null() {
            unsafe {
                drop(Box::from_raw(clock));
            }
        }
        RCB_CLOCK_UNAVAILABLE.set(false);
        RCB_EVENT_ID.set(0);
        return Err(error);
    }
    drop(reservation);
    RCB_EVENT_ID.set(event_id);
    Ok(())
}

/// Prove that no pre-existing asynchronous descriptor transport can race the
/// creation of the first private slot. This covers numeric procfs entries and
/// every per-task registered-only ring index. Callers own the single-threaded,
/// signal-fenced installation boundary.
pub(crate) fn refuse_inherited_io_uring() -> io::Result<()> {
    private_fd::refuse_inherited_io_uring()
}

#[cfg(test)]
fn initialize_rcb_clock_with(
    create: impl FnOnce() -> Result<reverie_ptrace::InGuestRcbCounter, reverie::Errno>,
) -> io::Result<()> {
    initialize_rcb_clock_with_mode(
        || {
            create()
                .map(Some)
                .map_err(|e| io::Error::from_raw_os_error(e.into_raw()))
        },
        false,
        0,
    )
}

fn initialize_rcb_clock_with_mode(
    create: impl FnOnce() -> io::Result<Option<reverie_ptrace::InGuestRcbCounter>>,
    signal_paused: bool,
    cpu: u32,
) -> io::Result<()> {
    let installed_child = rcb::callback::active();
    if !RCB_CLOCK.get().is_null() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "RCB clock already owned",
        ));
    }
    rcb::begin_setup().map_err(io::Error::from_raw_os_error)?;
    let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as libc::pid_t;
    if owner <= 0 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    let active_depth = RCB_ACCOUNTING.get().depth;
    // Builder syscalls may encounter installed hooks. Publish the unavailable
    // sentinel and BUILDING boundary before allocation or nested setup work.
    RCB_CLOCK.set(ptr::null_mut());
    RCB_CLOCK_OWNER.set(owner);
    RCB_EVENT_ID.set(0);
    RCB_CLOCK_UNAVAILABLE.set(true);
    RCB_ACCOUNTING.set(RcbAccounting::new(active_depth));
    let clock = match create() {
        Ok(Some(clock)) => clock,
        Ok(None) => {
            // An explicit shared unsupported-CPU decision, never an open,
            // transport, identity, mmap, ACK or active-counter failure.
            rcb::setup_unavailable().map_err(io::Error::from_raw_os_error)?;
            if signal_paused {
                crate::syscall_fallback::set_child_clock_pause(None);
            } else if !installed_child {
                crate::root::acquired(None)?;
            }
            return Ok(());
        }
        Err(error) => {
            RCB_CLOCK_UNAVAILABLE.set(false);
            rcb::setup_failed(error.raw_os_error().unwrap_or(libc::EIO));
            return Err(error);
        }
    };
    let fd = unsafe { clock.boundary_fd() }.as_raw_fd();
    RCB_FD
        .compare_exchange(-1, fd, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "RCB FD already protected"))?;
    // Every imported event stays physically disabled. The root activation,
    // outer installed child frame or held fallback continuation owns its token.
    // No Rust setup/result/destructor path may start the counter.
    let clock = Box::new(clock);
    let pause = match unsafe { rcb::register(fd, true, cpu) } {
        Ok(pause) => pause,
        Err(errno) => {
            // register validates every fallible condition before publication.
            // Its error leaves BUILDING with no permission to control this FD.
            RCB_FD.store(-1, Ordering::Release);
            return Err(io::Error::from_raw_os_error(errno));
        }
    };
    if !signal_paused && installed_child {
        let Some(token) = pause else {
            RCB_FD.store(-1, Ordering::Release);
            return Err(io::Error::from_raw_os_error(libc::ESTALE));
        };
        if let Err(errno) = rcb::callback::adopt_pause(token) {
            RCB_FD.store(-1, Ordering::Release);
            return Err(io::Error::from_raw_os_error(errno));
        }
    } else if signal_paused {
        crate::syscall_fallback::set_child_clock_pause(pause);
    } else {
        crate::root::acquired(pause).inspect_err(|_| {
            RCB_FD.store(-1, Ordering::Release);
        })?;
    }
    RCB_CLOCK.set(Box::into_raw(clock));
    RCB_CLOCK_OWNER.set(owner);
    RCB_CLOCK_UNAVAILABLE.set(false);
    RCB_ACCOUNTING.set(RcbAccounting::new(active_depth));
    Ok(())
}

/// First ordinary action after a successful native fork in the child. Revoke
/// inherited permission before any RPC, allocator, libc or nested callback.
/// Only close the child's inherited reference; never control the parent's PMU.
pub(crate) fn begin_fork_child_rcb() -> io::Result<()> {
    rcb::begin_setup().map_err(io::Error::from_raw_os_error)?;
    let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as libc::pid_t;
    if owner <= 0 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    let inherited_clock = RCB_CLOCK.replace(ptr::null_mut());
    RCB_CLOCK_OWNER.set(owner);
    RCB_EVENT_ID.set(0);
    RCB_CLOCK_UNAVAILABLE.set(true);
    let active_depth = RCB_ACCOUNTING.get().depth;
    RCB_ACCOUNTING.set(RcbAccounting::new(active_depth));
    // Preserve depth: the physical fork is inside an existing callback.
    let inherited = RCB_FD.load(Ordering::Acquire);
    if inherited >= 0 {
        let result = unsafe { raw_syscall6(libc::SYS_close, [inherited as u64, 0, 0, 0, 0, 0]) };
        // Linux close releases the slot even when it reports a late error.
        // Never retry it and risk closing a reused descriptor.
        RCB_FD.store(-1, Ordering::Release);
        if result != 0 {
            let error = io::Error::from_raw_os_error(
                i32::try_from(-result)
                    .ok()
                    .filter(|value| *value > 0)
                    .unwrap_or(libc::EIO),
            );
            rcb::setup_failed(error.raw_os_error().unwrap_or(libc::EIO));
            return Err(error);
        }
    }
    if inherited_clock.is_null() != (inherited < 0) {
        let error = io::Error::from_raw_os_error(libc::ESTALE);
        rcb::setup_failed(libc::ESTALE);
        return Err(error);
    }
    // Linux perf mappings are VM_DONTCOPY. The qualification gate proves the
    // exact parent address is absent in the child before another event is
    // acquired. Do not reconstruct this copied Box: its Drop would try to
    // unmap a nonexistent child VMA and close a descriptor number that may have
    // been reused after the one close above. Moving the raw pointer out of TLS
    // invalidates the inherited Rust owner; its small COW allocation is terminal
    // process-lifetime storage.
    #[cfg(feature = "rcb-qualification")]
    if let Err(error) = verify_inherited_perf_mapping_absent() {
        rcb::setup_failed(error.raw_os_error().unwrap_or(libc::EIO));
        return Err(error);
    }
    if let Err(error) = private_fd::refuse_inherited_io_uring() {
        rcb::setup_failed(error.raw_os_error().unwrap_or(libc::EIO));
        return Err(error);
    }
    Ok(())
}

type RcbReader = unsafe fn(&reverie_ptrace::InGuestRcbCounter) -> Result<u64, reverie::Errno>;
unsafe fn read_running_rcb(
    clock: &reverie_ptrace::InGuestRcbCounter,
) -> Result<u64, reverie::Errno> {
    clock.read()
}
unsafe fn read_paused_rcb(
    clock: &reverie_ptrace::InGuestRcbCounter,
) -> Result<u64, reverie::Errno> {
    unsafe { clock.read_paused_once() }
}
unsafe fn read_invalid_rcb(_: &reverie_ptrace::InGuestRcbCounter) -> Result<u64, reverie::Errno> {
    Err(reverie::Errno::new(rcb::active_error()))
}

fn sample_rcb(clock: &reverie_ptrace::InGuestRcbCounter) -> io::Result<u64> {
    // The selector itself uses direct initial-exec TLS and CMOV, not a new
    // conditional branch before the existing running-path sample. The eventual
    // optimized caller/read shims still require disassembly review.
    let address = unsafe {
        rcb::select_reader(
            read_running_rcb as *const () as usize,
            read_paused_rcb as *const () as usize,
            read_invalid_rcb as *const () as usize,
        )
    };
    let reader: RcbReader = unsafe { core::mem::transmute(address) };
    unsafe { reader(clock) }.map_err(|error| io::Error::from_raw_os_error(error.into_raw()))
}

fn rcb_clock() -> io::Result<Option<&'static reverie_ptrace::InGuestRcbCounter>> {
    let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as libc::pid_t;
    if RCB_CLOCK_OWNER.get() != owner {
        // Every admitted fork must rebind before guest/Tool clock access.
        // Never acquire over the network from a lazy reader or signal path.
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    let current = RCB_CLOCK.get();
    if current.is_null() {
        if rcb::is_broken() {
            return Err(io::Error::from_raw_os_error(rcb::active_error()));
        }
        debug_assert!(RCB_CLOCK_UNAVAILABLE.get());
        Ok(None)
    } else {
        Ok(Some(unsafe { &*current }))
    }
}

fn fail_rcb_accounting(mut state: RcbAccounting, errno: i32) -> io::Error {
    let errno = state.break_with(errno);
    RCB_ACCOUNTING.set(state);
    // Every accounting entry is inside a root, installed or signal-owned
    // physical pause. Make that failure terminal before ordinary code can
    // return to an assembly epilogue which might otherwise enable the event.
    rcb::accounting_failed(errno);
    io::Error::from_raw_os_error(errno)
}

fn sampled_accounting(
    clock: &reverie_ptrace::InGuestRcbCounter,
    state: RcbAccounting,
) -> Result<u64, io::Error> {
    sample_rcb(clock).map_err(|error| {
        fail_rcb_accounting(state, error.raw_os_error().unwrap_or(libc::EIO))
    })
}

/// Mark entry into an ordinary-context tool callback.
pub(crate) fn enter_rcb_handler() -> io::Result<()> {
    let clock = rcb_clock()?;
    let mut state = RCB_ACCOUNTING.get();
    let sample = match clock {
        Some(clock) => Some(sampled_accounting(clock, state)?),
        None => None,
    };
    if let Err(errno) = state.enter(sample) {
        return Err(fail_rcb_accounting(state, errno));
    }
    RCB_ACCOUNTING.set(state);
    Ok(())
}

/// Deduct all RCBs retired while the outermost tool callback was active.
pub(crate) fn leave_rcb_handler() -> io::Result<()> {
    let clock = rcb_clock()?;
    let mut state = RCB_ACCOUNTING.get();
    let sample = if state.depth == 1 {
        match clock {
            Some(clock) => Some(sampled_accounting(clock, state)?),
            None => None,
        }
    } else {
        None
    };
    if let Err(errno) = state.leave(sample) {
        return Err(fail_rcb_accounting(state, errno));
    }
    RCB_ACCOUNTING.set(state);
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
    let mut state = RCB_ACCOUNTING.get();
    let sample = sampled_accounting(clock, state)?;
    let value = match state.public(sample) {
        Ok(value) => value,
        Err(errno) => return Err(fail_rcb_accounting(state, errno)),
    };
    RCB_ACCOUNTING.set(state);
    Ok(value)
}

/// Trusted identity and clock seam for the private-descriptor qualification.
/// The runtime-owned descriptor number is never discovered through procfs: the
/// returned event ID is the authenticated supervisor offer, checked again
/// against the live file through the seccomp-whitelisted raw gate.
#[cfg(feature = "rcb-qualification")]
pub(crate) fn private_rcb_snapshot_for_test() -> io::Result<[u64; 4]> {
    let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
    let fd = RCB_FD.load(Ordering::Acquire);
    let event_id = RCB_EVENT_ID.get();
    if owner <= 0 || owner != i64::from(RCB_CLOCK_OWNER.get()) || fd < 0 || event_id == 0 {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    let mut kernel_id = 0_u64;
    let result = unsafe {
        raw_syscall6(
            libc::SYS_ioctl,
            [
                fd as u64,
                PERF_EVENT_IOC_ID,
                (&raw mut kernel_id) as u64,
                0,
                0,
                0,
            ],
        )
    };
    if result != 0 || kernel_id != event_id {
        return Err(io::Error::from_raw_os_error(if result < 0 {
            i32::try_from(-result).unwrap_or(libc::EIO)
        } else {
            libc::ESTALE
        }));
    }
    Ok([fd as u64, event_id, owner as u64, read_guest_rcb_clock()?])
}

/// The initial lifecycle cannot contain prior guest progress. An unsupported
/// negotiated CPU remains an unavailable clock, never a fabricated zero read.
pub(crate) fn verify_initial_rcb_clock() -> io::Result<()> {
    if rcb_clock()?.is_some() {
        let state = RCB_ACCOUNTING.get();
        if state != RcbAccounting::new(0) {
            return Err(io::Error::other("LiteInst initial RCB clock is not zero"));
        }
        if read_guest_rcb_clock()? != 0 {
            return Err(io::Error::other("LiteInst initial guest clock is not zero"));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn setup_snapshot() -> (bool, bool, u8, i32, usize, i32, bool) {
    (
        TOOL_BOOTSTRAP_STARTED.load(Ordering::Acquire),
        TOOL_DISPATCHER.get().is_some(),
        TOOL_MODE.load(Ordering::Acquire),
        RCB_FD.load(Ordering::Acquire),
        RCB_CLOCK.get() as usize,
        RCB_CLOCK_OWNER.get(),
        RCB_CLOCK_UNAVAILABLE.get(),
    )
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
    // Under bootstrap mediation, keep the reserved SIGSYS path usable even
    // while an unavailable-control error is being allocated or formatted.
    let _signal_mask = crate::control::SignalFence::block(raw_syscall6)?;

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
    install_instruction_signal_handler_inner(subscriptions, on_alt_stack, None)
}

fn install_instruction_signal_handler_inner(
    subscriptions: InstructionSubscriptions,
    on_alt_stack: bool,
    trusted_restorer: Option<u64>,
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

    if let Some(restorer) = trusted_restorer {
        // Use the actual libc restorer read from the installed SIGSYS action.
        // Only this raw installation bypasses the active bootstrap signal guard;
        // no callback runs within a broad signal/I/O permission scope.
        let action = KernelSigaction {
            handler: instruction_sigsegv_entry as *const () as usize as u64,
            flags: (libc::SA_SIGINFO | if on_alt_stack { libc::SA_ONSTACK } else { 0 }) as u64
                | 0x0400_0000,
            restorer,
            mask: u64::MAX,
        };
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [libc::SIGSEGV as u64, (&raw const action) as u64, 0, 8, 0, 0],
            )
        };
        return if result == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error((-result) as i32))
        };
    }

    let mut action: libc::sigaction = unsafe { core::mem::zeroed() };
    action.sa_flags = libc::SA_SIGINFO | if on_alt_stack { libc::SA_ONSTACK } else { 0 };
    action.sa_sigaction = instruction_sigsegv_entry as *const () as usize;
    if unsafe { libc::sigfillset(&mut action.sa_mask) } != 0 {
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

/// The one irreversible trap installation precedes all generic Config/Tool
/// callbacks. Its initial policy enforces the existing nested-callback guards;
/// it never publishes a patch site or dispatches an unconstructed Tool.
pub(crate) struct ToolBootstrap {
    config: RuntimeConfig,
    stats: crate::stats::GuestStatsHooks,
    publication: PatchPublication,
    restorer: u64,
}

static TOOL_BOOTSTRAP_STARTED: AtomicBool = AtomicBool::new(false);
static TOOL_DISPATCHER: OnceLock<LiteinstDispatcher> = OnceLock::new();
static BOOTSTRAP_SIGSYS: AtomicU64 = AtomicU64::new(0);

/// Physical SIGSYS entries observed while the initial Tool was being prepared.
/// They are also included in enabled physical-signal statistics; they are not
/// guest syscall, patch-site, installed-hook or completion classifications.
pub(crate) fn bootstrap_sigsys_count() -> u64 {
    BOOTSTRAP_SIGSYS.load(Ordering::Relaxed)
}

pub(crate) fn prepare_reverie_tool(
    stats: crate::stats::GuestStatsHooks,
    publication: PatchPublication,
) -> io::Result<ToolBootstrap> {
    TOOL_BOOTSTRAP_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| {
            io::Error::new(io::ErrorKind::AlreadyExists, "Tool bootstrap started twice")
        })?;
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
    PATCH_PUBLICATION.store(publication as u8, Ordering::Release);
    prepare_instrumentation()?;
    let config = runtime_config_from_env()?;
    // The caller already holds the original mask. Reset inherited handlers and
    // keep asynchronous signals blocked until installation is complete. This
    // inner guard unblocks SIGSYS only after its real handler/filter exist.
    let mut filter = reverie_preload::seccomp::SeccompFilter::for_trusted_gates(
        reverie_preload::trap::trusted_gate(),
        reverie_preload::trap::guest_syscall_gate(),
    )?;
    let signals = prepare_guest_signal_state(InstructionSubscriptions::default())?;
    reverie_preload::trap::set_dispatcher(Box::new(ToolBootstrapDispatcher { stats }));
    unsafe { reverie_preload::trap::install_handler(config.use_alt_stack) }?;
    // Make SIGSYS deliverable before installing the filter: even dropping its
    // allocation afterward is allowed to make a mediated allocator syscall.
    // The outer failure/unwind path must also retain this reserved availability.
    crate::root::unblock_on_restore(1_u64 << (libc::SIGSYS - 1))?;
    drop(signals);
    unsafe { filter.install() }?;
    crate::control::runtime_signals_ready();
    let mut action = KernelSigaction::default();
    let result = unsafe {
        raw_syscall6(
            libc::SYS_rt_sigaction,
            [libc::SIGSYS as u64, 0, (&raw mut action) as u64, 8, 0, 0],
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error((-result) as i32));
    }
    const SA_RESTORER: u64 = 0x0400_0000;
    if action.flags & SA_RESTORER == 0 || action.restorer == 0 {
        return Err(io::Error::other(
            "installed SIGSYS action has no Linux x86-64 restorer",
        ));
    }
    Ok(ToolBootstrap {
        config,
        stats,
        publication,
        restorer: action.restorer,
    })
}

pub(crate) fn initialize_reverie_tool(
    bootstrap: ToolBootstrap,
    instructions: InstructionSubscriptions,
    vdso_sites: &[reverie_ptrace::VdsoSyscallSite],
) -> io::Result<()> {
    install_vdso_sites(vdso_sites)?;
    install_instruction_signal_handler_inner(
        instructions,
        bootstrap.config.use_alt_stack,
        Some(bootstrap.restorer),
    )?;
    if instructions.cpuid || instructions.rdtsc {
        let mask = 1_u64 << (libc::SIGSEGV - 1);
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_UNBLOCK as u64,
                    (&raw const mask) as u64,
                    0,
                    8,
                    0,
                    0,
                ],
            )
        };
        if result != 0 {
            return Err(io::Error::from_raw_os_error((-result) as i32));
        }
    }
    enable_instruction_faulting(instructions)?;
    TOOL_MODE.store(TOOL_REVERIE, Ordering::Release);
    TOOL_DISPATCHER
        .set(LiteinstDispatcher::new(
            bootstrap.stats,
            bootstrap.publication,
        ))
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Tool dispatcher published twice",
            )
        })
}

struct ToolBootstrapDispatcher {
    stats: crate::stats::GuestStatsHooks,
}

impl SyscallDispatcher for ToolBootstrapDispatcher {
    fn dispatch(&self, event: &mut PreloadSyscallEvent) {
        if let Some(dispatcher) = TOOL_DISPATCHER.get() {
            dispatcher.dispatch(event);
        } else {
            dispatch_bootstrap_syscall(event);
        }
    }

    fn dispatch_private_signal(
        &self,
        frame: &mut reverie_preload::trap::frame::SignalFrame<'_>,
    ) -> bool {
        if let Some(dispatcher) = TOOL_DISPATCHER.get() {
            return dispatcher.dispatch_private_signal(frame);
        }
        BOOTSTRAP_SIGSYS.fetch_add(1, Ordering::Relaxed);
        self.stats
            .record_path(crate::LiteinstDispatchPath::InGuestPhysicalSigsys);
        false
    }

    fn dispatch_signal(
        &self,
        event: &mut PreloadSyscallEvent,
        frame: &mut reverie_preload::trap::frame::SignalFrame<'_>,
    ) {
        if let Some(dispatcher) = TOOL_DISPATCHER.get() {
            dispatcher.dispatch_signal(event, frame);
        } else {
            dispatch_bootstrap_syscall(event);
        }
    }
}

fn dispatch_bootstrap_syscall(event: &mut PreloadSyscallEvent) {
    // Generic Deserialize/Clone/subscriptions/new code has no I/O exemption.
    // Setup and ordinary RPC reads/writes alone use their trusted raw adapters.
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
}

fn install_runtime(
    stats: crate::stats::GuestStatsHooks,
    publication: PatchPublication,
    instructions: InstructionSubscriptions,
    vdso_sites: &[reverie_ptrace::VdsoSyscallSite],
) -> io::Result<()> {
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
    BOOTSTRAP_SIGSYS.store(0, Ordering::Relaxed);
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
    callback: InstalledCallback,
    publication: PatchPublication,
    expected_instruction: &[u8],
    manage_protection: bool,
) -> io::Result<HostInstallResult> {
    let callback = callback.entry();
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

fn install_vdso_sites(sites: &[reverie_ptrace::VdsoSyscallSite]) -> io::Result<()> {
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

fn vdso_callback(number: i64) -> io::Result<InstalledCallback> {
    match number {
        libc::SYS_time => Ok(installed_callback::VDSO_TIME),
        libc::SYS_clock_gettime => Ok(installed_callback::VDSO_CLOCK_GETTIME),
        libc::SYS_getcpu => Ok(installed_callback::VDSO_GETCPU),
        libc::SYS_gettimeofday => Ok(installed_callback::VDSO_GETTIMEOFDAY),
        libc::SYS_clock_getres => Ok(installed_callback::VDSO_CLOCK_GETRES),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported LiteInst vDSO syscall number {number}"),
        )),
    }
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
                installed_callback::HOST_SYSCALL,
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

const _: () = {
    assert!(core::mem::size_of::<KernelSigaction>() == 32);
    assert!(core::mem::offset_of!(KernelSigaction, handler) == 0);
    assert!(core::mem::offset_of!(KernelSigaction, flags) == 8);
    assert!(core::mem::offset_of!(KernelSigaction, restorer) == 16);
    assert!(core::mem::offset_of!(KernelSigaction, mask) == 24);
};

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
        restore_mask: previous_mask & !(sigsys | sigsegv),
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
    let unsupported_cpu_state = event.number == libc::SYS_sched_setaffinity;
    if unsupported_process {
        event.result = -i64::from(libc::ENOTSUP);
    } else if unsupported_signal_state || unsupported_cpu_state {
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

unsafe extern "C" fn host_syscall_hook_body(context: *mut HookContext) {
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

fn instruction_callback(kind: InstructionEventKind) -> InstalledCallback {
    match kind {
        InstructionEventKind::Cpuid => installed_callback::CPUID,
        InstructionEventKind::Rdtsc => installed_callback::RDTSC,
        InstructionEventKind::Rdtscp => installed_callback::RDTSCP,
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

unsafe extern "C" {
    fn instruction_sigsegv_entry(
        signal: libc::c_int,
        info: *mut libc::siginfo_t,
        context: *mut libc::c_void,
    );
    #[cfg(feature = "rcb-qualification")]
    static reverie_liteinst_instruction_sigsegv_entry_end: u8;
}

// SIGSEGV is runtime-owned whenever instruction faulting is active. The kernel
// action supplies a full sa_mask before this entry. The straight-line prefix
// reaches initial-exec RCB state and performs a real DISABLE before the first
// conditional branch can retire. Any failed RUNNING disable exits directly;
// an unavailable or already-paused event takes the body without inventing an
// enable. The current CPU is checked against the registered explicit event CPU
// before both body entry and physical re-enable.
global_asm!(r#"
    .text
    .p2align 4
    .global reverie_liteinst_instruction_sigsegv_entry
    .hidden reverie_liteinst_instruction_sigsegv_entry
    .type reverie_liteinst_instruction_sigsegv_entry,@function
reverie_liteinst_instruction_sigsegv_entry:
    endbr64
    push r12
    push r13
    push r14
    push r15
    sub rsp,40
    mov [rsp],rdi
    mov [rsp+8],rsi
    mov [rsp+16],rdx
    call reverie_preload_rcb_record
    mov r12,rax
    mov edi,{ioctl}
    movsxd rsi,dword ptr [r12]
    mov edx,{disable}
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov r15,rax
    cmp dword ptr [r12+16],{running}
    sete al
    test r15,r15
    setne dl
    and al,dl
    lea r13,[rip+.Linstruction_select]
    lea r14,[rip+.Linstruction_fatal]
    test al,al
    cmovne r13,r14
    jmp r13
.Linstruction_select:
    lea r13,[rip+.Linstruction_body_only]
    lea r14,[rip+.Linstruction_pause]
    cmp dword ptr [r12+16],{running}
    cmove r13,r14
    jmp r13
.Linstruction_pause:
    mov edi,{gettid}
    xor esi,esi
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    cmp eax,dword ptr [r12+4]
    lea r13,[rip+.Linstruction_fatal]
    lea r14,[rip+.Linstruction_cpu_before]
    cmove r13,r14
    jmp r13
.Linstruction_cpu_before:
    mov dword ptr [rsp+24],-1
    mov edi,{getcpu}
    lea rsi,[rsp+24]
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    test rax,rax
    sete al
    mov ecx,dword ptr [rsp+24]
    cmp ecx,dword ptr [r12+{cpu_offset}]
    sete dl
    and al,dl
    lea r13,[rip+.Linstruction_fatal]
    lea r14,[rip+.Linstruction_paused]
    test al,al
    cmovne r13,r14
    jmp r13
.Linstruction_paused:
    mov dword ptr [r12+16],{paused}
    mov dword ptr [r12+20],1
    add qword ptr [r12+48],1
    add qword ptr [r12+40],1
.Linstruction_body_only:
    mov rdi,[rsp]
    mov rsi,[rsp+8]
    mov rdx,[rsp+16]
    call {body}
    lea r13,[rip+.Linstruction_return]
    lea r14,[rip+.Linstruction_cpu_after]
    cmp dword ptr [r12+20],1
    cmove r13,r14
    jmp r13
.Linstruction_cpu_after:
    mov dword ptr [rsp+24],-1
    mov edi,{getcpu}
    lea rsi,[rsp+24]
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    test rax,rax
    sete al
    mov ecx,dword ptr [rsp+24]
    cmp ecx,dword ptr [r12+{cpu_offset}]
    sete dl
    and al,dl
    lea r13,[rip+.Linstruction_fatal]
    lea r14,[rip+.Linstruction_enable]
    test al,al
    cmovne r13,r14
    jmp r13
.Linstruction_enable:
    mov edi,{ioctl}
    movsxd rsi,dword ptr [r12]
    mov edx,{enable}
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    test rax,rax
    lea r13,[rip+.Linstruction_fatal]
    lea r14,[rip+.Linstruction_enabled]
    cmovz r13,r14
    jmp r13
.Linstruction_enabled:
    mov dword ptr [r12+20],0
    mov dword ptr [r12+16],{running}
    add qword ptr [r12+56],1
.Linstruction_return:
    add rsp,40
    pop r15
    pop r14
    pop r13
    pop r12
    ret
.Linstruction_fatal:
    mov edi,{exit_group}
    mov esi,125
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    ud2
    .global reverie_liteinst_instruction_sigsegv_entry_end
    .hidden reverie_liteinst_instruction_sigsegv_entry_end
reverie_liteinst_instruction_sigsegv_entry_end:
    .size reverie_liteinst_instruction_sigsegv_entry,.-reverie_liteinst_instruction_sigsegv_entry
"#,
    body = sym instruction_sigsegv_handler_body,
    ioctl = const libc::SYS_ioctl,
    disable = const 0x2401u32,
    enable = const 0x2400u32,
    gettid = const libc::SYS_gettid,
    getcpu = const libc::SYS_getcpu,
    exit_group = const libc::SYS_exit_group,
    cpu_offset = const rcb::CPU_OFFSET,
    running = const rcb::RUNNING_MODE,
    paused = const 2u32,
);

unsafe extern "C" fn instruction_sigsegv_handler_body(
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

unsafe extern "C" fn installed_cpuid_hook_body(context: *mut HookContext) {
    unsafe { installed_instruction_hook(context, InstructionEventKind::Cpuid) }
}

unsafe extern "C" fn installed_rdtsc_hook_body(context: *mut HookContext) {
    unsafe { installed_instruction_hook(context, InstructionEventKind::Rdtsc) }
}

unsafe extern "C" fn installed_rdtscp_hook_body(context: *mut HookContext) {
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

unsafe extern "C" fn installed_syscall_hook_body(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, None) }
}

unsafe extern "C" fn installed_vdso_time_hook_body(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_time)) }
}

unsafe extern "C" fn installed_vdso_clock_gettime_hook_body(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_clock_gettime)) }
}

unsafe extern "C" fn installed_vdso_getcpu_hook_body(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_getcpu)) }
}

unsafe extern "C" fn installed_vdso_gettimeofday_hook_body(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_gettimeofday)) }
}

unsafe extern "C" fn installed_vdso_clock_getres_hook_body(context: *mut HookContext) {
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
                        installed_callback::SYSCALL,
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
    let protected_cpu = event.number == libc::SYS_sched_setaffinity;

    if unsupported_process {
        event.result = -i64::from(libc::ENOTSUP);
    } else if protected_signal || protected_cpu {
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

    let admitted = with_non_tool_process_admission(event, tool_mode, |event, cow_fork| {
        if event.number == libc::SYS_exit || event.number == libc::SYS_exit_group {
            unsafe {
                trace_event(event, None);
            }
        }

        let compatibility_fork = tool_mode == TOOL_COMPAT && cow_fork;
        if compatibility_fork {
            unsafe {
                trace_event(event, None);
            }
        }
        event.result = unsafe { event.forward() };
        if cow_fork && event.result == 0 {
            // Built-in/compatibility execution has no typed Tool counter to acquire.
            // Its copied installed stack still needs the new actual owner before
            // any nested callback. This path refuses every active/incomplete clock.
            rcb::callback::rebind_unavailable_fork_child();
        }
        observe_mapping_generation(event);

        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-260): Review the fork-following observability reset call.
        // In the child of an admitted COW fork/clone (`result == 0`), the
        // COW-inherited observability counters describe the parent, not this child.
        // Reset them through the shared ForkHook seam so per-process attribution
        // starts clean. The admission below excludes shared-stack and shared-memory
        // creation before either the physical syscall or this child mutation.
        if cow_fork && event.result == 0 {
            FORK_HOOK.run_in_child();
        }

        if event.number != libc::SYS_exit
            && event.number != libc::SYS_exit_group
            && !compatibility_fork
        {
            unsafe {
                trace_event(event, Some(event.result));
            }
        }
    });
    if !admitted {
        unsafe { trace_event(event, Some(event.result)) };
    }
}

/// Run the forwarding continuation only after non-Tool process admission.
/// The shared preload dispatcher already refuses raw vfork/clone3: they need
/// a controller-owned bootstrap. LiteInst's custom dispatcher must retain that
/// guard too. A positive COW classification admits only bare fork or the exact
/// existing null-stack clone allowlist. Typed Tool parsing/translation happens
/// earlier in process_syscall and never uses this continuation.
fn with_non_tool_process_admission(
    event: &mut SyscallEvent,
    tool_mode: u8,
    forward: impl FnOnce(&mut SyscallEvent, bool),
) -> bool {
    let cow_fork = match event.number {
        libc::SYS_fork => true,
        libc::SYS_clone if clone_is_fork_like(event.args[0], event.args[1]) => true,
        libc::SYS_clone => {
            event.result = -i64::from(if tool_mode == TOOL_COMPAT {
                libc::EPERM
            } else {
                libc::ENOTSUP
            });
            return false;
        }
        libc::SYS_vfork | libc::SYS_clone3 => {
            event.result = -i64::from(libc::ENOTSUP);
            return false;
        }
        _ => false,
    };
    forward(event, cow_fork);
    true
}

fn clone_is_fork_like(flags: u64, child_stack: u64) -> bool {
    const SIGNAL_MASK: u64 = 0xff;
    let allowed_flags =
        (libc::CLONE_CHILD_CLEARTID | libc::CLONE_CHILD_SETTID | libc::CLONE_PARENT_SETTID) as u64;
    child_stack == 0
        && flags & SIGNAL_MASK == libc::SIGCHLD as u64
        && flags & !(SIGNAL_MASK | allowed_flags) == 0
}

/// Apply the same safeguard to original, nested and runtime-private injection
/// routes. This function executes no physical original syscall on `None`.
pub(crate) unsafe fn private_descriptor_result(number: i64, args: [u64; 6]) -> Option<i64> {
    private_fd::apply(
        number,
        args,
        {
            let mut descriptors = [-1; crate::control::PROTECTED_SLOTS + 2];
            descriptors[..crate::control::PROTECTED_SLOTS]
                .copy_from_slice(&crate::control::protected_fds());
            descriptors[crate::control::PROTECTED_SLOTS] = COORDINATOR_FD.load(Ordering::Acquire);
            descriptors[crate::control::PROTECTED_SLOTS + 1] = RCB_FD.load(Ordering::Acquire);
            descriptors
        },
        |number, args| unsafe { raw_syscall6(number, args) },
    )
}

/// Arm one qualification probe for the actual endpoint prepared by the next
/// fork. The endpoint remains runtime-owned; callers provide an ordinary
/// datagram pair used only to prove that no alias was published.
#[cfg(feature = "rcb-qualification")]
pub(crate) fn arm_prepared_fork_probe_for_test(sender: i32, receiver: i32) -> io::Result<()> {
    if sender < 0 || receiver < 0 || sender == receiver {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    PREPARED_FORK_PROBE_RECEIVE
        .compare_exchange(
            PREPARED_FORK_PROBE_IDLE,
            receiver,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| io::Error::from_raw_os_error(libc::EALREADY))?;
    if PREPARED_FORK_PROBE_SEND
        .compare_exchange(
            PREPARED_FORK_PROBE_IDLE,
            sender,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        PREPARED_FORK_PROBE_RECEIVE.store(PREPARED_FORK_PROBE_IDLE, Ordering::Release);
        return Err(io::Error::from_raw_os_error(libc::EALREADY));
    }
    PREPARED_FORK_PROBE_RESULT.store(i32::MIN, Ordering::Release);
    PREPARED_FORK_PROBE_GENERATION.store(0, Ordering::Release);
    Ok(())
}

#[cfg(feature = "rcb-qualification")]
pub(crate) fn probe_prepared_fork_endpoint_for_test(
    endpoint: i32,
    generation: u64,
) -> io::Result<()> {
    let sender = PREPARED_FORK_PROBE_SEND.swap(PREPARED_FORK_PROBE_CLAIMED, Ordering::AcqRel);
    if sender == PREPARED_FORK_PROBE_IDLE {
        return Ok(());
    }
    if sender < 0 || endpoint < 0 || generation == 0 {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    let receiver = PREPARED_FORK_PROBE_RECEIVE.swap(
        PREPARED_FORK_PROBE_CLAIMED,
        Ordering::AcqRel,
    );
    if receiver < 0 {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }

    let mut byte = 0x5a_u8;
    let mut vector = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut control = [0_usize; 3];
    let mut message: libc::msghdr = unsafe { core::mem::zeroed() };
    message.msg_iov = &raw mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = unsafe { libc::CMSG_SPACE(size_of::<i32>() as u32) } as usize;
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as u32) as usize;
        libc::CMSG_DATA(header).cast::<i32>().write_unaligned(endpoint);
    }
    let result = unsafe {
        private_descriptor_result(
            libc::SYS_sendmsg,
            [
                sender as u64,
                (&raw mut message) as u64,
                libc::MSG_NOSIGNAL as u64,
                0,
                0,
                0,
            ],
        )
    }
    .ok_or_else(|| io::Error::from_raw_os_error(libc::EACCES))?;
    if result != -i64::from(libc::ENOTSUP) {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }

    let mut received = 0_u8;
    let mut received_vector = libc::iovec {
        iov_base: (&raw mut received).cast(),
        iov_len: 1,
    };
    let mut received_control = [0_usize; 3];
    let mut received_message: libc::msghdr = unsafe { core::mem::zeroed() };
    received_message.msg_iov = &raw mut received_vector;
    received_message.msg_iovlen = 1;
    received_message.msg_control = received_control.as_mut_ptr().cast();
    received_message.msg_controllen = received_control.len() * size_of::<usize>();
    let received = unsafe {
        raw_syscall6(
            libc::SYS_recvmsg,
            [
                receiver as u64,
                (&raw mut received_message) as u64,
                libc::MSG_DONTWAIT as u64,
                0,
                0,
                0,
            ],
        )
    };
    if received != -i64::from(libc::EAGAIN) {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    PREPARED_FORK_PROBE_RESULT.store(result as i32, Ordering::Release);
    PREPARED_FORK_PROBE_GENERATION.store(generation, Ordering::Release);
    Ok(())
}

#[cfg(feature = "rcb-qualification")]
pub(crate) fn prepared_fork_probe_for_test() -> io::Result<[i64; 2]> {
    let result = PREPARED_FORK_PROBE_RESULT.load(Ordering::Acquire);
    let generation = PREPARED_FORK_PROBE_GENERATION.load(Ordering::Acquire);
    if result != -libc::ENOTSUP || generation == 0 {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    Ok([i64::from(result), generation as i64])
}

unsafe fn protect_coordinator_channel(event: &mut SyscallEvent) -> bool {
    if let Some(result) = unsafe { private_descriptor_result(event.number, event.args) } {
        event.result = result;
        true
    } else {
        false
    }
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

#[cfg(feature = "rcb-qualification")]
pub(crate) fn installed_callback_table() -> (u64, Vec<[u64; 4]>) {
    installed_callback::table()
}

#[cfg(feature = "rcb-qualification")]
pub(crate) fn instruction_signal_boundary() -> (u64, usize) {
    let start = instruction_sigsegv_entry as *const () as usize;
    let end = core::ptr::addr_of!(reverie_liteinst_instruction_sigsegv_entry_end) as usize;
    (start as u64, end.checked_sub(start).expect("instruction signal boundary end"))
}

/// The caller owns mapping lifetime/quiescence while reading these addresses.
#[cfg(feature = "rcb-qualification")]
pub(crate) unsafe fn installed_trampoline_layout(address: u64) -> Option<[u64; 6]> {
    let site = find_site(address)?;
    if site.state.load(Ordering::Acquire) != SITE_ACTIVE {
        return None;
    }
    let pointer = site.hook.load(Ordering::Acquire);
    if pointer.is_null() {
        return None;
    }
    let trampoline = unsafe { (*pointer).trampoline() };
    let layout = trampoline.layout();
    Some([
        trampoline.address(),
        trampoline.code_len() as u64,
        layout.instrumentation_len as u64,
        layout.restore_len as u64,
        layout.relocated_len as u64,
        layout.return_len as u64,
    ])
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::Ordering;
    use std::ffi::OsStr;

    use reverie_preload::BuiltinTool;

    use super::ALT_STACK_ENV;
    use super::ENABLED_FALLBACK_CLASSIFICATIONS;
    use super::FORK_HOOK;
    use super::FallbackCounters;
    use super::LiteinstDispatcher;
    use super::MAX_PATCH_SITES;
    use super::RCB_CLOCK;
    use super::RCB_CLOCK_OWNER;
    use super::RCB_CLOCK_UNAVAILABLE;
    use super::RcbAccounting;
    use super::SITE_ACTIVE;
    use super::SITE_FALLBACK;
    use super::SITE_INSTALLING;
    use super::SITE_STALE;
    use super::SITES;
    use super::SiteSlot;
    use super::StackLine;
    use super::TOOL_PASSTHROUGH;
    use super::TOOL_SPOOF_GETPID;
    use super::alt_stack_from_env_value;
    use super::builtin_tool_from_env_value;
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
    fn rcb_accounting_rejects_regression_without_returning_zero() {
        let mut accounting = RcbAccounting::new(0);
        assert_eq!(accounting.public(7), Ok(7));
        let before = accounting;
        assert_eq!(accounting.public(6), Err(libc::ESTALE));
        assert_eq!(accounting, before, "a rejected sample changed accounting");
        assert_eq!(accounting.break_with(libc::ESTALE), libc::ESTALE);
        assert_eq!(accounting.break_with(libc::EOVERFLOW), libc::ESTALE);
        assert_eq!(accounting.public(8), Err(libc::ESTALE));

        // Causal negative control: the replaced saturating expression turns an
        // impossible regression into a plausible zero clock.
        assert_eq!(6_u64.saturating_sub(7), 0);
        assert_eq!(6_u64.checked_sub(7), None);
    }

    #[test]
    fn rcb_accounting_rejects_deduction_and_depth_overflow() {
        let mut depth = RcbAccounting::new(u32::MAX);
        assert_eq!(depth.enter(None), Err(libc::EOVERFLOW));
        assert_eq!(depth.depth, u32::MAX);

        let mut deduction = RcbAccounting::new(1);
        deduction.entry = 1;
        deduction.last_sample = 1;
        deduction.deduction = u64::MAX;
        assert_eq!(deduction.leave(Some(2)), Err(libc::EOVERFLOW));
        assert_eq!(deduction.depth, 1);
        assert_eq!(deduction.deduction, u64::MAX);
    }

    #[test]
    fn rcb_accounting_accepts_last_representable_value_and_rejects_wrap() {
        let mut accounting = RcbAccounting::new(0);
        assert_eq!(accounting.public(u64::MAX), Ok(u64::MAX));
        assert_eq!(accounting.public(0), Err(libc::ESTALE));
        assert_eq!(accounting.last_public, u64::MAX);
        assert_eq!(accounting.last_sample, u64::MAX);
    }

    #[test]
    fn acquisition_errors_never_become_an_unavailable_clock() {
        // The public API must refuse outside an authenticated root entry
        // before installing handlers, seccomp, RPC or a counter.
        crate::root::assert_unwrapped_install_has_no_effects();
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
            let raw_error = error.into_raw();
            std::thread::spawn(move || {
                let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as libc::pid_t;
                assert!(owner > 0);
                let actual = initialize_rcb_clock_with(|| Err(reverie::Errno::new(raw_error)))
                    .unwrap_err();
                assert_eq!(actual.raw_os_error(), Some(raw_error));
                assert!(RCB_CLOCK.get().is_null());
                assert!(!RCB_CLOCK_UNAVAILABLE.get());
                assert_eq!(RCB_CLOCK_OWNER.get(), owner);
                assert!(rcb::is_broken());
                assert_eq!(
                    rcb::begin_setup(),
                    Err(raw_error),
                    "a later setup must retain the first terminal error"
                );
            })
            .join()
            .unwrap();
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

        let event = |number, args| super::SyscallEvent {
            number,
            args,
            instruction_pointer: 0x1234,
            result: super::UNSET_RESULT,
            context: 0x5678,
            dispatch: super::SyscallDispatch::Trap,
            guest_pkru: None,
        };
        for mode in [0, super::TOOL_STRACE, super::TOOL_COMPAT] {
            for (number, args, errno) in [
                (libc::SYS_vfork, [0; 6], libc::ENOTSUP),
                (libc::SYS_clone3, [0; 6], libc::ENOTSUP),
                (libc::SYS_clone3, [u64::MAX; 6], libc::ENOTSUP),
                (
                    libc::SYS_clone,
                    [libc::CLONE_VM as u64 | libc::SIGCHLD as u64, 0, 0, 0, 0, 0],
                    if mode == super::TOOL_COMPAT {
                        libc::EPERM
                    } else {
                        libc::ENOTSUP
                    },
                ),
            ] {
                let mut request = event(number, args);
                // This is the same admission function around the production
                // event.forward and both child mutations. Any invocation of
                // that continuation invalidates this refusal control.
                let parent_before = [100_u64, 9, 3];
                let mut parent_after = parent_before;
                let mut forwarded = 0;
                let admitted = super::with_non_tool_process_admission(
                    &mut request,
                    mode,
                    |request, _cow_fork| {
                        forwarded += 1;
                        request.result = 0;
                        parent_after = [200, 10, 4];
                    },
                );
                assert!(!admitted);
                assert_eq!(
                    forwarded, 0,
                    "refusal reached the forwarding/child continuation"
                );
                assert_eq!(parent_after, parent_before);
                assert_eq!(request.number, number);
                assert_eq!(request.args, args);
                assert_eq!(request.instruction_pointer, 0x1234);
                assert_eq!(request.context, 0x5678);
                assert_eq!(request.result, -i64::from(errno));
            }
            for (number, args, expected_cow) in [
                (libc::SYS_fork, [0; 6], true),
                (
                    libc::SYS_clone,
                    [libc::SIGCHLD as u64 | bookkeeping, 0, 0, 0, 0, 0],
                    true,
                ),
                (libc::SYS_getpid, [0; 6], false),
            ] {
                let mut request = event(number, args);
                let mut forwarded = 0;
                assert!(super::with_non_tool_process_admission(
                    &mut request,
                    mode,
                    |_, cow_fork| {
                        forwarded += 1;
                        assert_eq!(cow_fork, expected_cow);
                    },
                ));
                assert_eq!(forwarded, 1);
            }
        }
    }
}

#[cfg(test)]
#[path = "runtime/fd_signal_tests.rs"]
mod fd_signal_tests;
