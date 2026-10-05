use core::arch::global_asm;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicI32;
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::AtomicU8;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::cell::Cell;
use std::ffi::CStr;
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
use crate::interior_entry::Census;
use crate::interior_entry::CensusError;
use crate::interior_entry::ObjectImage;
use crate::interior_entry::Refusal;
use crate::interior_entry::prove;

const fn const_bytes_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

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
/// The hook was refused, under the installation lock, after the site's bytes
/// were re-read and found intact and before the runtime changed any page
/// protection or byte there; no other site's published patch covers it, and
/// no later patch may cover it (see [`check_neighbours`]). Execution therefore
/// resumes after the instruction on the original code. Only the
/// instruction-fault path records this state; every other caller records
/// `SITE_FALLBACK` for any refusal.
const SITE_UNPATCHABLE: u8 = 5;
const INSTRUCTION_CPUID: u8 = 1;
const INSTRUCTION_RDTSC: u8 = 2;

static TOOL_MODE: AtomicU8 = AtomicU8::new(0);
static EVENT_FD: AtomicI32 = AtomicI32::new(libc::STDERR_FILENO);
static COORDINATOR_FD: AtomicI32 = AtomicI32::new(-1);
/// An output descriptor the in-guest Tool owns; see [`reserve_tool_output_fd`].
static TOOL_OUTPUT_FD: AtomicI32 = AtomicI32::new(-1);
/// The lowest number the Tool output socket takes. The coordinator connection,
/// which connects after the socket is reserved, keeps 1024 as it always has.
const TOOL_OUTPUT_FD_MIN: u64 = 1025;
/// The message the runtime sends on the Tool output socket if it ever has to
/// give the socket up; see [`reserve_tool_output_fd`].
static TOOL_OUTPUT_RETIREMENT: OnceLock<&'static [u8]> = OnceLock::new();
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
static SITE_PATCHING_ENABLED: AtomicBool = AtomicBool::new(true);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstructionEventKind {
    Cpuid,
    Rdtsc,
    Rdtscp,
}

impl InstructionEventKind {
    /// Length of the only encoding the runtime recognizes for this kind.
    pub(crate) const fn encoded_len(self) -> u64 {
        match self {
            Self::Cpuid | Self::Rdtsc => 2,
            Self::Rdtscp => 3,
        }
    }
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

/// Moves `fd`, a socket the in-guest Tool owns for its own output (for example
/// one end of a socket pair its log records go to), to a reserved number at or
/// above 1025 (1024 stays the coordinator connection's), closes `fd`, and
/// returns the reserved number. From then on the runtime keeps the guest
/// away from it exactly as it does from the coordinator connection: a guest
/// `close` of it reports success and does nothing, a `close_range` over it
/// spares it, and a guest `read`, `write`, `shutdown`, `send`, `setsockopt` or
/// other descriptor operation on it fails with `EBADF`. A guest `dup2`/`dup3`
/// onto its number succeeds as it would if the number were free: the runtime
/// first moves the socket to another free number at or above 1025, so the Tool
/// must read the current number from [`tool_output_fd`] for each use. Tool code
/// writes to it through its own syscalls, which the runtime does not dispatch
/// to the guest's protections. A forked child inherits the descriptor and its
/// protection.
///
/// A guest dup onto the number needs one more free descriptor at or above 1025
/// to move the socket to. When there is none (the descriptor table is full),
/// the runtime gives the socket up so the guest's call keeps its native
/// outcome: it sends `retirement` on the socket, closes the socket, and
/// [`tool_output_fd`] then returns `None`. The Tool chooses a message its
/// reader treats as "output incomplete". The send waits for room, as a
/// blocking write of any Tool record does, and finishes a partial send, so a
/// reader that keeps reading always receives the whole message, even when a
/// forked child still holds the socket open and no end-of-file comes.
///
/// Only a socket is accepted (`InvalidInput` otherwise, leaving `fd` open), as
/// for the coordinator connection: the guest can reach a regular file, pipe or
/// FIFO through other names, such as `/proc/self/fd/<n>`, and truncate, map or
/// reopen it, none of which a descriptor protection can stop; a socket cannot
/// be reopened, truncated or mapped.
///
/// # Safety
///
/// Call at most once per process, before [`crate::install_tool`], while the
/// process is still single-threaded.
pub unsafe fn reserve_tool_output_fd(
    fd: libc::c_int,
    retirement: &'static [u8],
) -> io::Result<libc::c_int> {
    let mut metadata: libc::stat = unsafe { core::mem::zeroed() };
    if unsafe { libc::fstat(fd, &raw mut metadata) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if metadata.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a Tool output descriptor must be a socket",
        ));
    }
    let reserved =
        unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, TOOL_OUTPUT_FD_MIN as libc::c_int) };
    if reserved < 0 {
        return Err(io::Error::last_os_error());
    }
    if let Err(actual) =
        TOOL_OUTPUT_FD.compare_exchange(-1, reserved, Ordering::AcqRel, Ordering::Acquire)
    {
        unsafe { libc::close(reserved) };
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("a Tool output descriptor is already reserved ({actual})"),
        ));
    }
    let _ = TOOL_OUTPUT_RETIREMENT.set(retirement);
    unsafe { libc::close(fd) };
    Ok(reserved)
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
    /// The object that owned this executable mapping at initialization, or
    /// `None` for an anonymous mapping or one whose object cannot be identified.
    image: Option<ObjectImage>,
    /// Built by the first installation that needs it; see [`Self::census`].
    census: OnceLock<Result<Census, CensusError>>,
}

impl RuntimeArena {
    /// Returns the entry census of this arena's executable mapping, building it
    /// on first use (<https://github.com/rrnewton/reverie/issues/812>).
    ///
    /// Call only while holding the installation lock inside a patch allocation
    /// scope: the build allocates, and the census is kept for the life of the
    /// process.
    fn census(&self) -> Result<&Census, Refusal> {
        self.census
            .get_or_init(|| {
                let image = self.image.as_ref().ok_or(NO_OBJECT_IMAGE)?;
                // SAFETY: the image records the readable mappings of the object
                // that mapped this arena's text when LiteInst initialized.
                // Those are the executable and its load-time libraries, which
                // the dynamic loader never unmaps. arena_for already relies on
                // the text mapping itself staying in place.
                unsafe { image.census((self.mapping_start, self.mapping_end)) }
            })
            .as_ref()
            .map_err(|error| Refusal::NoCensus(*error))
    }
}

const NO_OBJECT_IMAGE: CensusError =
    CensusError("the executable mapping belongs to no identifiable object");

/// Whether an installation must first prove that no known control transfer
/// enters the bytes its patch displaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryProof {
    /// A `syscall` site in an object's code: the census must prove it.
    Required,
    /// A site the census does not list. vDSO stubs are written by reverie
    /// itself and have no object image. CPUID, RDTSC and RDTSCP sites are not
    /// `syscall` instructions, so the census has no record of them.
    NotListed,
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
    /// Set, and never cleared, once this runtime first changes the site's
    /// protection to publish a patch there.
    publication_attempted: AtomicBool,
    /// The eight bytes at the site before, and after, the latest completed
    /// publication; valid only while `words_recorded` is set. Cleared when a
    /// new attempt starts, which happens only after the earlier patch was
    /// proved gone (see [`earlier_patch_survives`]).
    original_word: AtomicU64,
    published_word: AtomicU64,
    words_recorded: AtomicBool,
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
            publication_attempted: AtomicBool::new(false),
            original_word: AtomicU64::new(0),
            published_word: AtomicU64::new(0),
            words_recorded: AtomicBool::new(false),
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
/// (`install_runtime`, used by the `strace`/`compat`/Detcore modes); a shared
/// [`BuiltinTool`] runs through `install_builtin`, which uses the shared default.
pub const ALT_STACK_ENV: &str = "REVERIE_LITEINST_ALT_STACK";
/// Allows a caller to keep fork-family syscalls fail-closed while integrating
/// a Tool whose process lifecycle is not ready for the direct backend.
pub const PROCESS_FORK_ENV: &str = "REVERIE_LITEINST_PROCESS_FORK";
/// Selects whether an in-guest Reverie Tool patches trapping syscall sites.
///
/// Unset or `1` keeps site patching on. `0` turns it off for syscall sites: no
/// syscall site is claimed or patched, and every trapping syscall runs the Tool
/// through the in-guest `SIGSYS` fallback (`crate::syscall_fallback`). That
/// includes subscribed vDSO fast paths: they get ptrace's whole
/// `mov $nr, %eax; syscall; ret` stubs with no hook
/// ([`reverie_ptrace::patch_current_vdso_trapping`]) instead of hooked bare
/// `syscall`s. Subscribed `cpuid`, `rdtsc`, and `rdtscp` instructions are not
/// covered: their first fault still claims and patches the instruction site.
/// Any other value is rejected. The variable is not removed, so the guest can
/// read it in its environment. Only an in-guest Reverie Tool (the
/// `install_tool` family) honors `0`. When the runtime is selected from the
/// environment, the built-in, `strace`, and `compat` runtimes do not take
/// this selector (the built-ins never patch syscall sites; the others always
/// do), so they refuse to start when this variable holds anything but `1`.
pub const SITE_PATCHING_ENV: &str = "REVERIE_LITEINST_SITE_PATCHING";
/// [`SITE_PATCHING_ENV`] for the non-allocating constructor check.
const SITE_PATCHING_ENV_C: &CStr = c"REVERIE_LITEINST_SITE_PATCHING";
const _: () = assert!(const_bytes_eq(
    SITE_PATCHING_ENV_C.to_bytes(),
    SITE_PATCHING_ENV.as_bytes()
));

/// Parses a [`SITE_PATCHING_ENV`] value into the site-patching boolean.
///
/// `None` (unset) and `1` select patching; `0` disables it. Any other value,
/// including surrounding whitespace, is rejected, matching the strict
/// [`PROCESS_FORK_ENV`] parse.
pub fn site_patching_from_env_value(value: Option<&OsStr>) -> io::Result<bool> {
    match value {
        None => Ok(true),
        Some(value) if value == OsStr::new("1") => Ok(true),
        Some(value) if value == OsStr::new("0") => Ok(false),
        Some(value) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported {SITE_PATCHING_ENV} value {value:?}"),
        )),
    }
}

/// Refuses to start a runtime that does not take [`SITE_PATCHING_ENV`] when the
/// variable holds anything but the default `1`. Reads with `getenv` so the
/// accepted case does not allocate before the constructor window.
fn require_site_patching(runtime: &str) -> io::Result<()> {
    // SAFETY: the loader runs constructors before application threads start,
    // so nothing mutates the environment concurrently; the name is NUL-terminated.
    let value = unsafe { libc::getenv(SITE_PATCHING_ENV_C.as_ptr()) };
    // SAFETY: a non-null getenv result is a NUL-terminated environment value.
    if value.is_null() || unsafe { CStr::from_ptr(value) }.to_bytes() == b"1" {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{SITE_PATCHING_ENV} is honored only by an in-guest Reverie Tool; \
             the {runtime} runtime does not take it"
        ),
    ))
}

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

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-913): Review the reserved-signal set kept unblocked.
/// Signals the runtime receives as forced signals and so must never be
/// blocked: SIGSYS for every trapped system call, and SIGSEGV for CPUID or
/// RDTSC faulting while an instruction is subscribed. Linux resets a blocked
/// forced signal to its default action, which kills the process.
pub(crate) fn reserved_signal_mask() -> u64 {
    let mut reserved = 1_u64 << (libc::SIGSYS - 1);
    if INSTRUCTION_SUBSCRIPTIONS.load(Ordering::Acquire) != 0 {
        reserved |= 1_u64 << (libc::SIGSEGV - 1);
    }
    reserved
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
    let tool_value = std::env::var_os("REVERIE_LITEINST_TOOL");
    // Prefer a shared reverie-preload built-in when the selector names one, so a
    // single env var is a superset of the LiteInst-native strace/compat modes
    // (matches e9patch's single TOOL_ENV selecting shared built-ins).
    if let Some(value) = tool_value.as_deref()
        && let Some(tool) = builtin_tool_from_env_value(value)
    {
        require_site_patching("built-in Tool")?;
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
    require_site_patching(if mode == TOOL_COMPAT {
        "compat"
    } else {
        "strace"
    })?;
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

pub(crate) fn initialize_reverie_tool(
    stats: crate::stats::GuestStatsHooks,
    publication: PatchPublication,
    instructions: InstructionSubscriptions,
    site_patching: bool,
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
    debug_assert!(site_patching || vdso_sites.is_empty());
    SITE_PATCHING_ENABLED.store(site_patching, Ordering::Release);
    TOOL_MODE.store(TOOL_REVERIE, Ordering::Release);
    install_runtime(stats, publication, instructions, vdso_sites)
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
    Ok(parse_runtime_maps(&std::fs::read_to_string(
        "/proc/self/maps",
    )?))
}

fn parse_runtime_maps(maps: &str) -> Vec<RuntimeMap> {
    maps.lines()
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
        .collect()
}

/// Returns the readable mappings of the file that `maps` maps executable at
/// `[text_start, text_end)`, with the address of its ELF header.
///
/// Mappings belong to one object when they share the text mapping's device and
/// inode. The header is the single such mapping at file offset zero. A file
/// that is mapped twice has two, so it gets no image, and neither does an
/// anonymous mapping.
fn object_image(maps: &[RuntimeMap], text_start: u64, text_end: u64) -> Option<ObjectImage> {
    let text = maps
        .iter()
        .find(|map| map.start == text_start && map.end == text_end && map.executable)?;
    if text.inode == 0 {
        return None;
    }
    let object = maps
        .iter()
        .filter(|map| map.readable && map.device == text.device && map.inode == text.inode);
    let mut headers = object.clone().filter(|map| map.offset == 0);
    let header = match (headers.next(), headers.next()) {
        (Some(header), None) => header.start,
        _ => return None,
    };
    let ranges = object
        .map(|map| (map.start, map.end, map.executable))
        .collect();
    Some(ObjectImage::new(header, ranges))
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
    // Initialization retains the guard router for modes that may publish
    // concurrently.
    prepare_live_patching().map_err(|error| io::Error::other(error.to_string()))?;
    prepare_instrumentation_state()
}

fn prepare_instrumentation_state() -> io::Result<()> {
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
    let objects = parse_runtime_maps(&maps);
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
        // The allocation must add exactly one identity-matched writable and
        // executable alias pair; anything else fails closed.
        discover_arena_aliases(&before, &after)?;
        arenas.push(RuntimeArena {
            mapping_start,
            mapping_end,
            mapping_name,
            arena,
            image: object_image(&objects, mapping_start, mapping_end),
            census: OnceLock::new(),
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
            // SITE_UNPATCHABLE stays: an invalidation (even a no-op mremap)
            // must not lift the reservation that keeps neighbouring patches
            // off a site threads still run through the continuation. If the
            // code really was replaced, a CPUID/RDTSC there is still decoded
            // at the fault and emulated, only never patched.
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
            // Only offset an entry the site's hook count actually re-counted.
            if let Some(site) = find_site(event.instruction_pointer) {
                site.hook_count.fetch_add(1, Ordering::Relaxed);
                stats.record_inherited_entry(crate::stats::InheritedEntry::Hook);
            }
        }
        SyscallDispatch::Fallback => {
            if let Some(site) = find_site(event.instruction_pointer) {
                site.trap_count.fetch_add(1, Ordering::Relaxed);
            }
            record_fallback_dispatch(event.number);
            stats.record_path(crate::LiteinstDispatchPath::InGuestSigsys);
            stats.record_inherited_entry(crate::stats::InheritedEntry::Sigsys);
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
    /// No other application thread can fetch the site during publication.
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

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(liteinst-straddler-cpuid): Review the untouched-code boundary.
/// Why [`install_site_hook`] installed no hook.
struct InstallFailure {
    error: io::Error,
    /// True only for a refusal that proves the site's code is the original:
    /// reached under the installation lock, after the expected bytes were
    /// re-read and [`check_neighbours`] found no published patch over the
    /// instruction or the point after it, and before the runtime changed any
    /// page protection or wrote any byte. A busy lock, a missing arena, a
    /// byte mismatch (another thread may have just published a jump there),
    /// and every failure from the protection change onward count as touched,
    /// even where nothing may have been written.
    code_untouched: bool,
}

impl InstallFailure {
    fn untouched(error: io::Error) -> Self {
        Self {
            error,
            code_untouched: true,
        }
    }

    fn touched(error: io::Error) -> Self {
        Self {
            error,
            code_untouched: false,
        }
    }
}

impl From<InstallFailure> for io::Error {
    fn from(failure: InstallFailure) -> Self {
        failure.error
    }
}

/// How a proposed patch relates to one other claimed site.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NeighbourConflict {
    /// The proposed site's instruction, or the point after it, lies inside
    /// the other site's published (or possibly partly published) jump, so
    /// its bytes are not known to be the original.
    InsidePublishedPatch,
    /// The proposed patch would overwrite the start of another site's jump.
    OverlapsPublishedPatch,
    /// The proposed patch would cover a site that a thread may be executing
    /// through the continuation (or is deciding about), which resumes after
    /// that instruction.
    CoversContinuationSite,
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(liteinst-straddler-cpuid): Review the neighbour-overlap rules.
/// Classify a proposed word patch at `address`, whose instruction is
/// `instruction_len` bytes long, against another site at `other` in `state`.
/// `other_len` is that site's instruction length, or 0 when not yet recorded
/// (then the longest emulated instruction, 3 bytes, is assumed). `footprint`
/// says the other site's eight-byte window may hold bytes this runtime
/// published ([`site_footprint`]); a site INSTALLING, UNPATCHABLE, STALE or
/// FALLBACK also reserves its instruction and resume point for the
/// continuation.
fn neighbour_conflict(
    address: u64,
    instruction_len: u64,
    other: u64,
    state: u8,
    other_len: u64,
    footprint: bool,
) -> Option<NeighbourConflict> {
    if other == 0 || other == address {
        return None;
    }
    let word = liteinst2::patcher::WORD_PATCH_BYTES as u64;
    let patch_end = address.saturating_add(word);
    let mut conflict = None;
    if footprint {
        let other_end = other.saturating_add(word);
        if other < address.saturating_add(instruction_len) && other_end > address {
            return Some(NeighbourConflict::InsidePublishedPatch);
        }
        if other < patch_end && other_end > address {
            conflict = Some(NeighbourConflict::OverlapsPublishedPatch);
        }
    }
    // FALLBACK and STALE sites keep the reservation too: a FALLBACK syscall
    // refused before any publication leaves no footprint but is executed
    // through the SIGSYS continuation, and a STALE site was ACTIVE or
    // FALLBACK.
    if matches!(
        state,
        SITE_INSTALLING | SITE_UNPATCHABLE | SITE_STALE | SITE_FALLBACK
    ) {
        let other_len = if other_len == 0 { 3 } else { other_len };
        // Starting exactly at the other site's resume point is safe.
        if address < other.saturating_add(other_len) && patch_end > other {
            conflict.get_or_insert(NeighbourConflict::CoversContinuationSite);
        }
    }
    conflict
}

/// The conflict that decides a proposed patch against every other site in
/// `sites` (address, state, recorded instruction length, footprint). Every site is
/// examined: [`NeighbourConflict::InsidePublishedPatch`] dominates, because a
/// refusal is untouched only if no published jump covers the site, whatever
/// order the site table yields its entries in.
fn neighbours_conflict(
    address: u64,
    instruction_len: u64,
    sites: impl IntoIterator<Item = (u64, u8, u64, bool)>,
) -> Option<NeighbourConflict> {
    let mut found = None;
    for (other, state, other_len, footprint) in sites {
        match neighbour_conflict(address, instruction_len, other, state, other_len, footprint) {
            Some(NeighbourConflict::InsidePublishedPatch) => {
                return Some(NeighbourConflict::InsidePublishedPatch);
            }
            Some(conflict) => {
                found.get_or_insert(conflict);
            }
            None => {}
        }
    }
    found
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(liteinst-straddler-cpuid): Review the stale-jump survival rule.
/// Whether any byte this runtime wrote at a site may still be there.
/// `words` is the site's (original, published) eight-byte pair, `None` when
/// none was recorded (a failed or partial publication); `current` is the
/// site's eight bytes now, `None` unless all eight were readable. A byte
/// survives when it differs from the original and equals the published byte,
/// or when it is an INT3 that was not in the original (a cross-line guard:
/// liteinst2 guards every front-fragment byte, changed or not). Only when
/// no byte survives is the old patch provably gone, whatever part of its
/// window a mapping change replaced.
fn stale_jump_survives(words: Option<(u64, u64)>, current: Option<[u8; 8]>) -> bool {
    let (Some((original, published)), Some(current)) = (words, current) else {
        return true;
    };
    let (original, published) = (original.to_le_bytes(), published.to_le_bytes());
    (0..8).any(|offset| {
        let byte = current[offset];
        (published[offset] != original[offset] && byte == published[offset])
            || (byte == liteinst2::patcher::BREAKPOINT_OPCODE
                && original[offset] != liteinst2::patcher::BREAKPOINT_OPCODE)
    })
}

/// Whether bytes from an earlier publication at a site may still be there,
/// whatever the site's state (a STALE site reclaimed for reinstallation is
/// INSTALLING, and its old jump may survive a partial mapping change). False
/// only if no publication was ever attempted, or the latest one completed and
/// [`stale_jump_survives`] proves every byte it wrote is gone; an attempt that
/// did not complete leaves no record and so counts as surviving.
fn earlier_patch_survives(
    publication_attempted: bool,
    words: Option<(u64, u64)>,
    current: Option<[u8; 8]>,
) -> bool {
    publication_attempted && stale_jump_survives(words, current)
}

/// Whether a site's eight-byte window may hold bytes this runtime published:
/// always for ACTIVE, otherwise when [`earlier_patch_survives`]. A FALLBACK
/// whose publication failed after the protection change has no completed
/// record and so survives; one refused before any change (a syscall the
/// census or the cross-line budget refused) wrote nothing and keeps only
/// its continuation reservation.
fn site_footprint(state: u8, survives: bool) -> bool {
    state == SITE_ACTIVE || survives
}

/// [`earlier_patch_survives`] for `slot` at `address`, reading the window
/// without risking a fault: part or all of its mapping may be gone.
fn slot_patch_survives(slot: &SiteSlot, address: u64) -> bool {
    if !slot.publication_attempted.load(Ordering::Acquire) {
        return false;
    }
    let words = slot.words_recorded.load(Ordering::Acquire).then(|| {
        (
            slot.original_word.load(Ordering::Acquire),
            slot.published_word.load(Ordering::Acquire),
        )
    });
    let mut bytes = [0_u8; 8];
    let current = (unsafe { read_own_bytes(address, &mut bytes) } == bytes.len()).then_some(bytes);
    earlier_patch_survives(true, words, current)
}

/// Refuse a patch at `address` that conflicts with another claimed site.
/// Called with the installation lock held, so no other patch is being
/// published concurrently, and before any protection change or write.
fn check_neighbours(address: u64, instruction_len: u64) -> Result<(), InstallFailure> {
    let Some(sites) = SITES.get() else {
        return Ok(());
    };
    let conflict = neighbours_conflict(
        address,
        instruction_len,
        sites.iter().map(|slot| {
            let other = slot.address.load(Ordering::Acquire);
            let state = slot.state.load(Ordering::Acquire);
            let survives = other != 0
                && other != address
                && state != SITE_ACTIVE
                && slot_patch_survives(slot, other);
            (
                other,
                state,
                u64::from(slot.instruction_len.load(Ordering::Acquire)),
                site_footprint(state, survives),
            )
        }),
    );
    match conflict {
        None => Ok(()),
        Some(NeighbourConflict::InsidePublishedPatch) => Err(InstallFailure::touched(
            io::Error::other("site lies inside another site's published LiteInst patch"),
        )),
        Some(NeighbourConflict::OverlapsPublishedPatch) => Err(InstallFailure::untouched(
            io::Error::other("LiteInst patch would overlap another site's patch"),
        )),
        Some(NeighbourConflict::CoversContinuationSite) => Err(InstallFailure::untouched(
            io::Error::other("LiteInst patch would cover a site executed through the continuation"),
        )),
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(liteinst-straddler-cpuid): Review blocking asynchronous signals across installation.
/// Blocks every blockable signal for its lifetime and then restores the
/// thread's exact previous mask. Installation runs in signal context, and in
/// the raw strace and compatibility modes the guest may own handlers, for
/// any signal number (a guest timer can deliver SIGILL as easily as SIGUSR1);
/// a handler running between the eligibility checks (bytes, footprints,
/// private mapping) and the completed publication could, for example,
/// replace the checked private mapping with a shared one, so that the
/// publication changed shared code. The fault signals are blocked too: a
/// fault that installation itself raises (the scanner or the entry census
/// reading guest memory that a guest made unreadable) then ends the process,
/// as the kernel does for a blocked synchronous fault, instead of running a
/// guest handler in the middle of an installation. The instruction-fault
/// path already installs with SIGSEGV blocked.
struct AsyncSignalsBlocked {
    previous: u64,
}

impl AsyncSignalsBlocked {
    fn new() -> io::Result<Self> {
        let bit = |signal: libc::c_int| 1_u64 << (signal - 1);
        // SIGKILL and SIGSTOP cannot be blocked; leave them out explicitly.
        let keep = bit(libc::SIGKILL) | bit(libc::SIGSTOP);
        let block = !keep;
        let mut previous = 0_u64;
        let result = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_BLOCK as u64,
                    (&raw const block) as u64,
                    (&raw mut previous) as u64,
                    core::mem::size_of::<u64>() as u64,
                    0,
                    0,
                ],
            )
        };
        if result < 0 {
            return Err(io::Error::from_raw_os_error((-result) as i32));
        }
        Ok(Self { previous })
    }
}

impl Drop for AsyncSignalsBlocked {
    fn drop(&mut self) {
        let _ = unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_SETMASK as u64,
                    (&raw const self.previous) as u64,
                    0,
                    core::mem::size_of::<u64>() as u64,
                    0,
                    0,
                ],
            )
        };
    }
}

unsafe fn install_site_hook(
    address: u64,
    slot: &'static SiteSlot,
    callback: liteinst2::trampoline::HookCallback,
    publication: PatchPublication,
    expected_instruction: &[u8],
    manage_protection: bool,
    entry_proof: EntryProof,
) -> Result<(), InstallFailure> {
    // No guest code may run from the first check below to the completed
    // publication, whatever signal arrives; see AsyncSignalsBlocked.
    let _signals_blocked = AsyncSignalsBlocked::new().map_err(InstallFailure::touched)?;
    // Nothing below proves the site's bytes are the original until they are
    // re-read under the lock, so the busy and no-arena refusals are touched.
    let _install_guard = lock_installation().map_err(InstallFailure::touched)?;
    let _allocation_scope = crate::patch_alloc::enter();
    let arena = arena_for(address)
        .ok_or_else(|| io::Error::other("no reachable LiteInst arena for syscall site"))
        .map_err(InstallFailure::touched)?;
    let mut mapping_end = slot.mapping_end.load(Ordering::Acquire);
    if mapping_end <= address {
        mapping_end = arena.mapping_end;
        slot.mapping_end.store(mapping_end, Ordering::Release);
    }
    let available = usize::try_from(mapping_end - address)
        .unwrap_or(0)
        .min(PATCH_SNAPSHOT_BYTES);
    // SAFETY: arena_for proved this byte range lies in a live executable VMA.
    let candidate =
        unsafe { core::slice::from_raw_parts(address as usize as *const u8, available) };
    if candidate.get(..expected_instruction.len()) != Some(expected_instruction) {
        // The bytes changed since the fault decoded them: another thread may
        // have published a jump over this site.
        return Err(InstallFailure::touched(io::Error::other(
            "fault site does not contain the expected x86-64 instruction",
        )));
    }
    slot.instruction_len
        .store(expected_instruction.len() as u8, Ordering::Release);
    // A reclaimed site's own earlier jump may survive past the bytes just
    // checked (a partial mapping change can restore only its first bytes).
    if slot_patch_survives(slot, address) {
        return Err(InstallFailure::touched(io::Error::other(
            "an earlier LiteInst patch at this site may survive",
        )));
    }
    check_neighbours(address, expected_instruction.len() as u64)?;
    if available < liteinst2::patcher::WORD_PATCH_BYTES {
        return Err(InstallFailure::untouched(io::Error::other(
            "syscall site is too close to its executable mapping end",
        )));
    }
    // Publish only into private mappings, where the write copies the page
    // for this process alone. A write through a shared mapping would change
    // the page cache that every other process and every alias of the same
    // backing executes, including clean private aliases, behind their site
    // tables. Never writing there keeps every continuation's bytes free of
    // LiteInst jumps that this process's footprints cannot see.
    let window_last = address + liteinst2::patcher::WORD_PATCH_BYTES as u64 - 1;
    if unsafe { private_span_protection(address, window_last) }.is_none() {
        return Err(InstallFailure::untouched(io::Error::other(
            "LiteInst publishes only into a private executable mapping",
        )));
    }
    let scanner = InstructionScanner::default();
    let scan = scanner
        .scan_prefix(candidate, address, liteinst2::patcher::WORD_PATCH_BYTES)
        .map_err(|error| InstallFailure::untouched(io::Error::other(error.to_string())))?;
    let proof = match entry_proof {
        EntryProof::Required => arena
            .census()
            .and_then(|census| prove(census, address, scan.instructions())),
        EntryProof::NotListed => Ok(()),
    };
    proof.map_err(|refusal| InstallFailure::untouched(io::Error::other(refusal.to_string())))?;
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
        PatchPublication::Concurrent => Some(
            crate::straddler::budget_for_patch(address as usize, scanner.cache_line_size())
                .map_err(InstallFailure::untouched)?,
        ),
    };
    let code = scan.snapshot();
    // `available` is at least WORD_PATCH_BYTES here, and the bytes were just
    // verified, so these are the site's original eight bytes.
    let original_word = u64::from_le_bytes(
        candidate[..liteinst2::patcher::WORD_PATCH_BYTES]
            .try_into()
            .expect("the patch window is eight bytes"),
    );
    // Any earlier patch here was just proved gone. From now until this
    // publication completes, the site counts as possibly published.
    slot.publication_attempted.store(true, Ordering::Release);
    slot.words_recorded.store(false, Ordering::Release);

    // Every failure from here on may follow a protection change or a write to
    // the guest's code. Record SITE_FALLBACK while the lock is still held, so
    // no later installer's neighbour check sees this site as merely
    // INSTALLING when part of a jump may already be published.
    let published_failure = |error: io::Error| {
        slot.state.store(SITE_FALLBACK, Ordering::Release);
        InstallFailure::touched(error)
    };
    if manage_protection {
        unsafe {
            set_text_protection(
                address,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            )
            .map_err(published_failure)?;
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
            return Err(published_failure(io::Error::other(error.to_string())));
        }
    };
    let activation = match publication {
        PatchPublication::Concurrent => installed.activate(),
        // SAFETY: quiescent publication is selected only at initialization,
        // before application threads start, or by `install_tool_quiescent`,
        // whose caller (Hermit, which schedules one guest thread at a time)
        // asserts that no other thread can fetch the site.
        PatchPublication::Quiescent => unsafe { installed.activate_quiescent() },
    };
    if let Err(error) = activation {
        if manage_protection {
            let _ = unsafe { set_text_protection(address, libc::PROT_READ | libc::PROT_EXEC) };
        }
        return Err(published_failure(io::Error::other(error.to_string())));
    }
    if manage_protection {
        unsafe {
            set_text_protection(address, libc::PROT_READ | libc::PROT_EXEC)
                .map_err(published_failure)?;
        }
    }

    // SAFETY: the window lies in this live executable mapping, which the
    // installation lock keeps from being patched concurrently.
    let published_word = unsafe { core::ptr::read_unaligned(address as usize as *const u64) };
    slot.original_word.store(original_word, Ordering::Release);
    slot.published_word.store(published_word, Ordering::Release);
    slot.words_recorded.store(true, Ordering::Release);
    let installed = Box::into_raw(Box::new(installed));
    slot.hook.store(installed, Ordering::Release);
    slot.instruction_len
        .store(instruction_len as u8, Ordering::Release);
    slot.straddle_prefix
        .store(straddle_prefix as u8, Ordering::Release);
    slot.state.store(SITE_ACTIVE, Ordering::Release);
    Ok(())
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
                EntryProof::NotListed,
            )
        }
        .map_err(|failure| {
            site.state.store(SITE_FALLBACK, Ordering::Release);
            io::Error::other(format!(
                "failed to install LiteInst vDSO hook: {}",
                failure.error
            ))
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
        libc::SYS_getrandom => Ok(installed_vdso_getrandom_hook),
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
    if unsupported_process {
        event.result = -i64::from(libc::ENOTSUP);
    } else if unsupported_signal_state {
        event.result = -i64::from(libc::EPERM);
    } else if !(protect_runtime_control(event)
        || unsafe { protect_runtime_descriptors(event, false) })
    {
        event.result = unsafe { event.forward() };
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
    decode_instruction(bytes)
}

/// Recognize the exact CPUID, RDTSC, and RDTSCP encodings that instruction
/// faulting traps, given the readable bytes starting at the faulting RIP.
fn decode_instruction(bytes: &[u8]) -> Option<(InstructionEventKind, &'static [u8])> {
    match bytes {
        [0x0f, 0xa2, ..] => Some((InstructionEventKind::Cpuid, &[0x0f, 0xa2])),
        [0x0f, 0x31, ..] => Some((InstructionEventKind::Rdtsc, &[0x0f, 0x31])),
        [0x0f, 0x01, 0xf9, ..] => Some((InstructionEventKind::Rdtscp, &[0x0f, 0x01, 0xf9])),
        _ => None,
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(liteinst-late-code-cpuid): Review the fault-safe self read of guest code.
/// Copy up to `out.len()` bytes starting at `address` from this process's own
/// memory without risking a nested fault, returning how many leading bytes
/// were readable. `process_vm_readv` reports an unmapped or unreadable page
/// (including an execute-only one) as a short count or an error instead of
/// faulting. Each byte is its own remote element, so a readable prefix that
/// ends at a mapping boundary is still returned.
///
/// # Safety
///
/// Issues raw syscalls through the trusted gate; safe in signal context.
unsafe fn read_own_bytes(address: u64, out: &mut [u8; 8]) -> usize {
    let mut remote = [libc::iovec {
        iov_base: ptr::null_mut(),
        iov_len: 0,
    }; 8];
    let mut count = 0;
    for (index, element) in remote.iter_mut().enumerate() {
        let Some(byte) = address.checked_add(index as u64) else {
            break;
        };
        element.iov_base = byte as usize as *mut libc::c_void;
        element.iov_len = 1;
        count += 1;
    }
    if count == 0 {
        return 0;
    }
    let local = libc::iovec {
        iov_base: out.as_mut_ptr().cast(),
        iov_len: count,
    };
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let read = unsafe {
        raw_syscall6(
            libc::SYS_process_vm_readv,
            [
                pid as u64,
                (&raw const local) as u64,
                1,
                remote.as_ptr() as u64,
                count as u64,
                0,
            ],
        )
    };
    usize::try_from(read).map_or(0, |read| read.min(count))
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

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(liteinst-late-code-cpuid): Review genuine SIGSEGV death from the handler.
/// End the process by a real, default-action `SIGSEGV` from inside the
/// runtime's own `SIGSEGV` handler, so the parent observes a signal death (and
/// a core where the limits allow one) rather than an ordinary exit status.
///
/// The handler runs with `SIGSEGV` blocked (no `SA_NODEFER`), so a `tgkill`
/// alone would only leave the signal pending. Reset the disposition to
/// `SIG_DFL`, unblock it, then send it to this thread: the kernel acts on it
/// when `tgkill` returns. The final exit is reached only if both controls
/// failed.
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
    let unblock = 1_u64 << (libc::SIGSEGV - 1);
    let _ = unsafe {
        raw_syscall6(
            libc::SYS_rt_sigprocmask,
            [
                libc::SIG_UNBLOCK as u64,
                (&raw const unblock) as u64,
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
    // Keep the raw kernel pointer: the fallback continuation borrows the same
    // frame through `SignalFrame`, which must not alias a live reference.
    let raw_context = context;
    let address = unsafe {
        (*raw_context.cast::<libc::ucontext_t>()).uc_mcontext.gregs[libc::REG_RIP as usize]
    } as u64;
    let Some((kind, expected)) = instruction_at(address) else {
        if let Some(arena) = arena_for(address) {
            let context = unsafe { &*raw_context.cast::<libc::ucontext_t>() };
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
            unsafe { deliver_default_sigsegv() };
        }
        // Code mapped after the runtime started (a dlopen'd library's
        // constructor, JIT output) has no arena and can never be patched.
        unsafe { emulate_unpatchable_instruction(info, raw_context, address) };
        return;
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
        let context = unsafe { &mut *raw_context.cast::<libc::ucontext_t>() };
        unsafe { execute_nested_fault_natively(kind, context, expected.len()) };
        return;
    }

    let Some((site, claimed)) = claim_site(address) else {
        emit_in_guest_stage(b"instruction-sigsegv-site-table-full");
        unsafe { emulate_through_continuation(info, raw_context, address, kind) };
        return;
    };
    site.trap_count.fetch_add(1, Ordering::Relaxed);
    if unsafe { set_all_instruction_native(true) }.is_err() {
        emit_in_guest_stage(b"instruction-sigsegv-enable-native-failed");
        unsafe { deliver_default_sigsegv() };
    }
    if claimed
        && let Err(failure) = unsafe {
            install_site_hook(
                address,
                site,
                instruction_callback(kind),
                patch_publication(),
                expected,
                true,
                EntryProof::NotListed,
            )
        }
    {
        let unpatchable = failure.code_untouched;
        site.state.store(
            if unpatchable {
                SITE_UNPATCHABLE
            } else {
                SITE_FALLBACK
            },
            Ordering::Release,
        );
    }
    if unsafe { set_all_instruction_native(false) }.is_err() {
        emit_in_guest_stage(b"instruction-sigsegv-disable-native-failed");
        unsafe { deliver_default_sigsegv() };
    }
    while matches!(site.state.load(Ordering::Acquire), 0 | SITE_INSTALLING) {
        core::hint::spin_loop();
    }
    match site.state.load(Ordering::Acquire) {
        SITE_ACTIVE => {}
        // The hook was refused before anything at the site changed (for
        // example, a word patch that would cross a cache line while
        // cross-line publication is disabled), so the instruction and the
        // code after it are the original bytes. Emulate it through the
        // continuation, as for code without an arena; every later execution
        // of this site traps and takes the same path.
        SITE_UNPATCHABLE => {
            unsafe { emulate_through_continuation(info, raw_context, address, kind) };
            return;
        }
        _ => {
            // A failed installation may already have published part or all
            // of the jump (for example, when restoring the page's
            // permissions fails after the patch is written), so the bytes
            // after this instruction are no longer known to be the original
            // code. Resuming past the instruction could execute the jump's
            // displacement; end the guest.
            emit_in_guest_stage(b"instruction-sigsegv-site-install-failed");
            unsafe { deliver_default_sigsegv() };
        }
    }
    let hook = site.hook.load(Ordering::Acquire);
    if hook.is_null() {
        emit_in_guest_stage(b"instruction-sigsegv-hook-missing");
        unsafe { deliver_default_sigsegv() };
    }
    let context = unsafe { &mut *raw_context.cast::<libc::ucontext_t>() };
    context.uc_mcontext.gregs[libc::REG_RIP as usize] =
        unsafe { (*hook).trampoline().address() } as i64;
}

/// Execute a faulting instruction reached from inside an active Tool callback
/// at the private native helper, in signal context, and advance past it.
/// Re-entering the Tool would deadlock on its already-held lock.
unsafe fn execute_nested_fault_natively(
    kind: InstructionEventKind,
    context: &mut libc::ucontext_t,
    instruction_len: usize,
) {
    emit_in_guest_stage(match kind {
        InstructionEventKind::Cpuid => b"nested-instruction-fault-native-cpuid",
        InstructionEventKind::Rdtsc => b"nested-instruction-fault-native-rdtsc",
        InstructionEventKind::Rdtscp => b"nested-instruction-fault-native-rdtscp",
    });
    if unsafe { set_instruction_native(kind, true) }.is_err() {
        emit_in_guest_stage(b"instruction-sigsegv-enable-native-failed");
        unsafe { deliver_default_sigsegv() };
    }
    unsafe { execute_native_fault_instruction(kind, context, instruction_len) };
    if unsafe { set_instruction_native(kind, false) }.is_err() {
        emit_in_guest_stage(b"instruction-sigsegv-disable-native-failed");
        unsafe { deliver_default_sigsegv() };
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(liteinst-late-code-cpuid): Review emulation of instructions outside every arena.
/// Handle an instruction fault at `address`, which lies outside every arena
/// recorded at startup, so no hook can ever be published there.
///
/// Only a kernel-raised fault (`SI_KERNEL`, the #GP of CPUID faulting or
/// `PR_TSC_SIGSEGV`) whose bytes, read without risking a nested fault, are
/// exactly CPUID, RDTSC, or RDTSCP is emulated. Everything else keeps the
/// default `SIGSEGV` the guest would have received without the runtime.
unsafe fn emulate_unpatchable_instruction(
    info: *const libc::siginfo_t,
    raw_context: *mut libc::c_void,
    address: u64,
) {
    let mut bytes = [0_u8; 8];
    let available = unsafe { read_own_bytes(address, &mut bytes) };
    let kernel_fault = !info.is_null() && unsafe { (*info).si_code } == libc::SI_KERNEL;
    let decoded = kernel_fault
        .then(|| decode_instruction(&bytes[..available]))
        .flatten();
    let Some((kind, expected)) = decoded else {
        emit_unarenaed_refusal_stage(
            b"instruction-sigsegv-no-reachable-arena",
            address,
            &bytes[..available],
        );
        unsafe { deliver_default_sigsegv() };
    };
    if !instruction_is_subscribed(kind) {
        emit_in_guest_stage(b"instruction-sigsegv-unsubscribed");
        unsafe { deliver_default_sigsegv() };
    }
    if tool_callback_active() {
        let context = unsafe { &mut *raw_context.cast::<libc::ucontext_t>() };
        unsafe { execute_nested_fault_natively(kind, context, expected.len()) };
        return;
    }
    unsafe { emulate_through_continuation(info, raw_context, address, kind) };
}

/// Redirect this kernel `SIGSEGV` frame to the owned fallback continuation,
/// which runs the Tool's instruction callback in ordinary context after
/// sigreturn and resumes the guest after the instruction, with the Tool's
/// result, through the same completion as an unpatched syscall.
unsafe fn emulate_through_continuation(
    info: *const libc::siginfo_t,
    raw_context: *mut libc::c_void,
    address: u64,
    kind: InstructionEventKind,
) {
    // SAFETY: both pointers are this invocation's kernel frame. No reference
    // into the context prefix is live past this point.
    let mut frame = match unsafe {
        reverie_preload::trap::frame::SignalFrame::from_instruction_fault(raw_context, info)
    } {
        Ok(frame) => frame,
        Err(_) => {
            emit_in_guest_stage(b"instruction-sigsegv-not-a-kernel-fault");
            unsafe { deliver_default_sigsegv() };
        }
    };
    match crate::syscall_fallback::prepare_instruction_signal(address, kind, &mut frame) {
        Ok(Some(entry)) => {
            emit_in_guest_stage(match kind {
                InstructionEventKind::Cpuid => b"instruction-fault-continuation-cpuid",
                InstructionEventKind::Rdtsc => b"instruction-fault-continuation-rdtsc",
                InstructionEventKind::Rdtscp => b"instruction-fault-continuation-rdtscp",
            });
            frame.set_register(libc::REG_RIP as usize, entry as i64);
        }
        Ok(None) => {
            emit_in_guest_stage(b"instruction-sigsegv-continuation-unavailable");
            unsafe { deliver_default_sigsegv() };
        }
        // Same integrity failure as the SIGSYS fallback: the continuation
        // was reserved but the frame could not be captured faithfully.
        Err(_) => unsafe { exit_now(126) },
    }
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
    if let Some(context) = unsafe { context.as_ref() }
        && let Some(site) = find_site(context.instruction_pointer)
    {
        record_hook_entry(site);
    }
    unsafe { dispatch_instruction_context(context, kind) };
}

/// Run the Tool's callback for a CPUID/RDTSC/RDTSCP whose fault had no
/// patchable site, from the owned fallback continuation in ordinary context.
/// It is the installed hook's dispatch without a site hook entry to count;
/// the continuation advances RIP past the instruction on completion.
pub(crate) unsafe fn dispatch_fallback_instruction(
    context: *mut HookContext,
    kind: InstructionEventKind,
) {
    unsafe { dispatch_instruction_context(context, kind) };
}

unsafe fn dispatch_instruction_context(context: *mut HookContext, kind: InstructionEventKind) {
    if context.is_null() || enter_rcb_handler().is_err() {
        unsafe { exit_now(122) };
    }
    let context = unsafe { &mut *context };
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
        record_hook_entry(site);
    }
    unsafe { dispatch_syscall_context(context, number, SyscallDispatch::InstalledHook, None) };
}

/// Counts one entry through a patched site's hook. An entry the Tool's own
/// syscall or instruction made during a callback is also counted alone, so
/// the guest's own entries can be told apart from the Tool's.
fn record_hook_entry(site: &SiteSlot) {
    site.hook_count.fetch_add(1, Ordering::Relaxed);
    if tool_callback_active() {
        crate::stats::GuestStatsHooks::current()
            .record_path(crate::LiteinstDispatchPath::InGuestNestedHook);
    }
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

unsafe extern "C" fn installed_vdso_getrandom_hook(context: *mut HookContext) {
    unsafe { installed_syscall_hook_for(context, Some(libc::SYS_getrandom)) }
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
    if !SITE_PATCHING_ENABLED.load(Ordering::Relaxed) {
        stats.record_path(crate::LiteinstDispatchPath::PatchingDisabledFallback);
        return;
    }
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

        if SITE_PATCHING_ENABLED.load(Ordering::Relaxed)
            && let Some((site, claimed)) = claim_site(instruction_pointer)
        {
            site.trap_count.fetch_add(1, Ordering::Relaxed);
            if claimed {
                let native = unsafe { set_all_instruction_native(true) };
                let installed = native.and_then(|()| {
                    unsafe {
                        install_site_hook(
                            instruction_pointer,
                            site,
                            installed_syscall_hook,
                            self.publication,
                            &[0x0f, 0x05],
                            true,
                            EntryProof::Required,
                        )
                    }
                    .map_err(io::Error::from)
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
        || (event.number == libc::SYS_sigaltstack && event.args[0] != 0);

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
    if tool_mode == TOOL_REVERIE && unsafe { protect_runtime_descriptors(event, true) } {
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

    if let Some(errno) = refused_process_creation(event.number, event.args, tool_mode) {
        event.result = -i64::from(errno);
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

/// Errno for process creation that the strace and compatibility modes refuse
/// before forwarding, or `None` when `process_syscall` may forward it.
///
/// These modes forward from runtime frames below the interrupted syscall
/// site: those of an installed hook's trampoline, on the guest's stack, or
/// those of the SIGSYS handler, which runs on the alternate signal stack
/// unless `RuntimeConfig::use_alt_stack` is false. The handler never forwards
/// a `vfork` or `clone3` itself: it hands one to the site's trampoline, or
/// refuses it without a trace record when the site has no installed hook.
/// Only a child with its own copy of those frames can return through
/// them. That means bare `fork` and the `clone` that [`clone_is_fork_like`]
/// accepts: a null child stack, `SIGCHLD` as the exit signal, and no flags
/// other than `CLONE_CHILD_CLEARTID`, `CLONE_CHILD_SETTID` and
/// `CLONE_PARENT_SETTID`. glibc's `fork` passes that shape: `SIGCHLD`,
/// `CLONE_CHILD_SETTID` and `CLONE_CHILD_CLEARTID`. Any other `clone` is
/// refused with the mode-specific errno it already had.
///
/// A `vfork` child runs on the parent's stack while the parent is suspended
/// inside those frames. It returns through them, then overwrites them before
/// the parent resumes. A `clone3` child with its own stack starts right after
/// the syscall instruction inside the forwarding code, with none of the frames
/// it would return through. A thread-creating `clone3` would also bypass the
/// thread `clone` refusal. The decision uses only the syscall number, so
/// `clone_args` is never read. The shared preload dispatcher refuses both for
/// the same reason. See <https://github.com/rrnewton/reverie/issues/758>.
///
/// Both are refused with `ENOTSUP` in both modes. This matches the preload
/// dispatcher and Tool mode's nested and injected syscall guards. `ENOSYS` was
/// the alternative. Seccomp sandboxes conventionally deny with `ENOSYS` so that
/// libc falls back to an older syscall (the containers-common 5.8 seccomp
/// profile's default errno is `ENOSYS`), and glibc does retry a failed clone3
/// as clone. That does not help here. In glibc 2.34 and 2.42 on x86-64,
/// pthread_create goes through `__clone_internal`, which retries only on
/// `ENOSYS`. posix_spawn also uses `__clone_internal` in 2.34; in 2.42 it calls
/// clone3 itself and, unless a cgroup was requested, retries on `ENOSYS` or
/// `EINVAL`. Every one of these callers passes a child stack, so the guard
/// above would refuse the retried clone (EPERM in compat, ENOTSUP in strace)
/// and the call would still fail. It would just take a second refused syscall,
/// and the errno would depend on the mode. glibc's `fork` uses clone, not
/// clone3, and its `vfork` issues the raw syscall with no fallback. Other glibc
/// versions and other libcs were not checked. The cost is that a fork-shaped
/// clone3 caller that falls back only on `ENOSYS` fails here; Tool mode admits
/// that shape through `tool_host::clone3_is_plain_fork`.
fn refused_process_creation(number: i64, args: [u64; 6], tool_mode: u8) -> Option<i32> {
    match number {
        libc::SYS_clone if !clone_is_fork_like(args[0], args[1]) => {
            Some(if tool_mode == TOOL_COMPAT {
                libc::EPERM
            } else {
                libc::ENOTSUP
            })
        }
        libc::SYS_clone3 | libc::SYS_vfork => Some(libc::ENOTSUP),
        _ => None,
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

/// Keeps the guest away from the descriptors the runtime owns in Tool mode:
/// the coordinator connection and, for a guest syscall, the Tool's reserved
/// output descriptor. A syscall the Tool itself makes (`guest_syscall` false)
/// may use the output descriptor, which is the Tool's.
/// The runtime-owned descriptors a syscall is kept from, sorted: the
/// coordinator connection, and for guest syscalls also the Tool output socket.
/// Returns the array and how many of its leading entries are in use.
fn protected_descriptors(guest_syscall: bool) -> ([u64; 2], usize) {
    let mut protected = [u64::MAX; 2];
    let mut count = 0;
    let slots: &[&AtomicI32] = if guest_syscall {
        &[&COORDINATOR_FD, &TOOL_OUTPUT_FD]
    } else {
        &[&COORDINATOR_FD]
    };
    for slot in slots {
        let fd = slot.load(Ordering::Acquire);
        if fd >= 0 {
            protected[count] = fd as u64;
            count += 1;
        }
    }
    protected[..count].sort_unstable();
    (protected, count)
}

unsafe fn protect_runtime_descriptors(event: &mut SyscallEvent, guest_syscall: bool) -> bool {
    let (mut protected, count) = protected_descriptors(guest_syscall);
    let protected = &mut protected[..count];
    if protected.is_empty() {
        return false;
    }
    if guest_syscall {
        match unsafe { relocate_tool_output_for_dup(event, protected) } {
            DupOntoToolOutput::NotThis => {}
            DupOntoToolOutput::Proceed => return false,
            DupOntoToolOutput::Refuse(result) => {
                event.result = result;
                return true;
            }
        }
    }
    if event.number == libc::SYS_close && protected.contains(&fd_arg(event, 0)) {
        event.result = 0;
    } else if event.number == libc::SYS_close_range
        && protected
            .iter()
            .any(|&fd| fd_arg(event, 0) <= fd && fd <= fd_arg(event, 1))
    {
        event.result = unsafe { close_range_preserving_fds(event, protected) };
    } else if let Some(result) = protected
        .iter()
        .find_map(|&fd| readiness_refusal(event, fd))
    {
        event.result = result;
    } else if protected.iter().any(|&fd| {
        syscall_targets_event_fd(event, fd) || (guest_syscall && syscall_uses_socket(event, fd))
    }) {
        event.result = -i64::from(libc::EBADF);
    } else {
        return false;
    }
    true
}

/// What to do with a guest syscall that may be a `dup2`/`dup3` onto the Tool
/// output socket's number.
enum DupOntoToolOutput {
    /// Not such a call: protect as usual.
    NotThis,
    /// The socket's number is free now; run the guest's call.
    Proceed,
    /// The call fails as Linux would fail it, before anything moved.
    Refuse(i64),
}

/// A guest `dup2`/`dup3` onto the Tool output socket's number is an ordinary
/// operation the guest is entitled to: that number is free as far as the guest
/// knows. First applies the kernel's own checks, in its order, without moving
/// anything: `dup3` flags other than `O_CLOEXEC` (`EINVAL`), a target at or
/// above `RLIMIT_NOFILE`, and a source that is not open (`EBADF`). Then moves
/// the socket to another free number at or above 1025 and closes the old one,
/// so the guest's call runs on a free number with its native outcome. When no
/// descriptor is free to move it to, gives the socket up instead (see
/// [`reserve_tool_output_fd`]). No unprotected copy of the socket is left
/// behind on any path.
unsafe fn relocate_tool_output_for_dup(
    event: &SyscallEvent,
    protected: &[u64],
) -> DupOntoToolOutput {
    if !matches!(event.number, libc::SYS_dup2 | libc::SYS_dup3) {
        return DupOntoToolOutput::NotThis;
    }
    let tool = TOOL_OUTPUT_FD.load(Ordering::Acquire);
    let source = fd_arg(event, 0);
    if tool < 0 || fd_arg(event, 1) != tool as u64 || protected.contains(&source) {
        return DupOntoToolOutput::NotThis;
    }
    if event.number == libc::SYS_dup3 && fd_arg(event, 2) & !(libc::O_CLOEXEC as u64) != 0 {
        return DupOntoToolOutput::Refuse(-i64::from(libc::EINVAL));
    }
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let queried = unsafe {
        raw_syscall6(
            libc::SYS_prlimit64,
            [
                0,
                libc::RLIMIT_NOFILE as u64,
                0,
                (&raw mut limit) as u64,
                0,
                0,
            ],
        )
    };
    if queried == 0 && tool as u64 >= limit.rlim_cur {
        return DupOntoToolOutput::Refuse(-i64::from(libc::EBADF));
    }
    if unsafe { raw_syscall6(libc::SYS_fcntl, [source, libc::F_GETFD as u64, 0, 0, 0, 0]) } < 0 {
        return DupOntoToolOutput::Refuse(-i64::from(libc::EBADF));
    }
    let moved = unsafe {
        raw_syscall6(
            libc::SYS_fcntl,
            [
                tool as u64,
                libc::F_DUPFD_CLOEXEC as u64,
                TOOL_OUTPUT_FD_MIN,
                0,
                0,
                0,
            ],
        )
    };
    let replacement = if moved >= 0 { moved as i32 } else { -1 };
    if TOOL_OUTPUT_FD
        .compare_exchange(tool, replacement, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        if moved >= 0 {
            unsafe { raw_syscall6(libc::SYS_close, [moved as u64, 0, 0, 0, 0, 0]) };
        }
        return DupOntoToolOutput::NotThis;
    }
    if moved < 0 {
        // No descriptor to move the socket to: tell its reader the output is
        // incomplete, then give it up.
        if let Some(message) = TOOL_OUTPUT_RETIREMENT.get() {
            send_retirement(tool, message);
        }
    }
    // The socket lives on as `moved`, or not at all; the guest's call gets the
    // old number.
    unsafe { raw_syscall6(libc::SYS_close, [tool as u64, 0, 0, 0, 0, 0]) };
    DupOntoToolOutput::Proceed
}

/// Sends all of `message` on `socket`, waiting for room as a blocking write
/// does (also when the socket was made non-blocking) and resuming a partial
/// send. It stops early only when the socket cannot take the message at all,
/// such as when its reader has gone; `MSG_NOSIGNAL` keeps that from raising
/// SIGPIPE in the guest.
fn send_retirement(socket: libc::c_int, message: &[u8]) {
    let mut rest = message;
    while !rest.is_empty() {
        let sent = unsafe {
            raw_syscall6(
                libc::SYS_sendto,
                [
                    socket as u64,
                    rest.as_ptr() as u64,
                    rest.len() as u64,
                    libc::MSG_NOSIGNAL as u64,
                    0,
                    0,
                ],
            )
        };
        if sent > 0 {
            rest = &rest[sent as usize..];
        } else if sent == -i64::from(libc::EAGAIN) {
            let mut writable = libc::pollfd {
                fd: socket,
                events: libc::POLLOUT,
                revents: 0,
            };
            let polled = unsafe {
                raw_syscall6(
                    libc::SYS_poll,
                    [(&raw mut writable) as u64, 1, -1i64 as u64, 0, 0, 0],
                )
            };
            if polled < 0 && polled != -i64::from(libc::EINTR) {
                return;
            }
        } else if sent != -i64::from(libc::EINTR) {
            return;
        }
    }
}

/// The Tool output socket's current number, if one is reserved
/// ([`reserve_tool_output_fd`]). It can change when the guest `dup2`s onto it,
/// so the Tool reads it for each use.
pub fn tool_output_fd() -> Option<libc::c_int> {
    let fd = TOOL_OUTPUT_FD.load(Ordering::Acquire);
    (fd >= 0).then_some(fd)
}

unsafe fn protect_compatibility_event_channel(event: &mut SyscallEvent) -> bool {
    let event_fd = EVENT_FD.load(Ordering::Acquire) as u64;

    if event.number == libc::SYS_close && fd_arg(event, 0) == event_fd {
        // The descriptor is controller-owned and intentionally invisible to
        // guest descriptor lifecycle management.
        event.result = 0;
    } else if event.number == libc::SYS_close_range
        && fd_arg(event, 0) <= event_fd
        && event_fd <= fd_arg(event, 1)
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
    unsafe { close_range_preserving_fds(event, &[event_fd]) }
}

/// Runs a guest `close_range` without closing any of `preserved`, which is sorted.
unsafe fn close_range_preserving_fds(event: &SyscallEvent, preserved: &[u64]) -> i64 {
    const CLOSE_RANGE_UNSHARE: u64 = 1 << 1;
    const CLOSE_RANGE_CLOEXEC: u64 = 1 << 2;

    let first = fd_arg(event, 0);
    let last = fd_arg(event, 1);
    let mut flags = fd_arg(event, 2);
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

    // Close each gap of [first, last] between the preserved descriptors.
    let mut start = first;
    for &fd in preserved {
        if fd < start || fd > last {
            continue;
        }
        if start < fd {
            let result =
                unsafe { raw_syscall6(libc::SYS_close_range, [start, fd - 1, flags, 0, 0, 0]) };
            if result < 0 {
                return result;
            }
        }
        start = fd + 1;
        if start == 0 {
            return 0;
        }
    }
    if start <= last {
        let result = unsafe { raw_syscall6(libc::SYS_close_range, [start, last, flags, 0, 0, 0]) };
        if result < 0 {
            return result;
        }
    }
    0
}

/// Syscall argument `index` as the kernel reads a descriptor, `close_range`
/// bound or flags argument: the low 32 bits of the register. Comparing the full
/// register would let `fd | 1 << 32` reach the descriptor past a protection.
fn fd_arg(event: &SyscallEvent, index: usize) -> u64 {
    u64::from(event.args[index] as u32)
}

fn syscall_targets_event_fd(event: &SyscallEvent, event_fd: u64) -> bool {
    let fd = |index| fd_arg(event, index) == event_fd;
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
        | libc::SYS_fcntl
        | libc::SYS_ioctl
        | libc::SYS_dup
        // Operations that change, lock or describe the open file itself.
        | libc::SYS_ftruncate
        | libc::SYS_fallocate
        | libc::SYS_fchmod
        | libc::SYS_fchown
        | libc::SYS_fsetxattr
        | libc::SYS_fremovexattr
        | libc::SYS_fgetxattr
        | libc::SYS_flistxattr
        | libc::SYS_flock
        | libc::SYS_fsync
        | libc::SYS_fdatasync
        | libc::SYS_sync_file_range
        | libc::SYS_lseek
        | libc::SYS_fstat
        | libc::SYS_fstatfs
        | libc::SYS_fadvise64
        | libc::SYS_readahead
        | libc::SYS_getdents64
        | libc::SYS_getdents
        | libc::SYS_syncfs
        | libc::SYS_fchdir
        // Descriptors of special kinds: a runtime-owned number is not one,
        // and to the guest it is not open at all.
        | libc::SYS_signalfd
        | libc::SYS_signalfd4
        | libc::SYS_timerfd_settime
        | libc::SYS_timerfd_gettime
        | libc::SYS_inotify_add_watch
        | libc::SYS_inotify_rm_watch
        | libc::SYS_fanotify_mark
        | libc::SYS_setns
        | libc::SYS_pidfd_send_signal
        | libc::SYS_pidfd_getfd
        | libc::SYS_process_madvise
        | libc::SYS_process_mrelease
        | libc::SYS_finit_module
        | libc::SYS_fsmount
        | libc::SYS_quotactl_fd
        | libc::SYS_landlock_add_rule
        | libc::SYS_landlock_restrict_self => fd(0),
        libc::SYS_sendfile => fd(0) || fd(1),
        libc::SYS_epoll_ctl => fd(0) || fd(2),
        // waitid(P_PIDFD, fd, ...).
        libc::SYS_waitid => event.args[0] as u32 == 3 && fd(1),
        // A file mapping of the descriptor.
        libc::SYS_mmap => event.args[3] & libc::MAP_ANONYMOUS as u64 == 0 && fd(4),
        libc::SYS_dup2 | libc::SYS_dup3 => fd(0) || fd(1),
        libc::SYS_splice | libc::SYS_copy_file_range => fd(0) || fd(2),
        libc::SYS_tee => fd(0) || fd(1),
        _ => false,
    }
}

/// Whether a guest syscall shuts down, configures, connects, or sends or
/// receives on socket `event_fd`. Only guest syscalls are checked: the Tool's
/// own RPC client uses these on the coordinator connection.
fn syscall_uses_socket(event: &SyscallEvent, event_fd: u64) -> bool {
    let fd = |index| fd_arg(event, index) == event_fd;
    match event.number {
        libc::SYS_shutdown
        | libc::SYS_getsockopt
        | libc::SYS_setsockopt
        | libc::SYS_sendto
        | libc::SYS_recvfrom
        | libc::SYS_sendmsg
        | libc::SYS_recvmsg
        | libc::SYS_sendmmsg
        | libc::SYS_recvmmsg
        | libc::SYS_connect
        | libc::SYS_bind
        | libc::SYS_listen
        | libc::SYS_accept
        | libc::SYS_accept4
        | libc::SYS_getsockname
        | libc::SYS_getpeername => fd(0),
        _ => false,
    }
}

/// The result Linux gives a guest `epoll_wait`, `epoll_pwait` or
/// `epoll_pwait2` on `event_fd`, a number the guest does not have open: `EBADF`,
/// after Linux's own `EINVAL` check of the event count. `None` for any other
/// call. (`poll`, `ppoll`, `select` and `pselect6` name descriptors inside guest
/// memory, and `*at` calls use their directory argument only for some paths;
/// checking them would cost correct programs' calls a guest-memory read, so a
/// buggy program that passes a runtime-owned number there is a known limit.)
fn readiness_refusal(event: &SyscallEvent, event_fd: u64) -> Option<i64> {
    match event.number {
        libc::SYS_epoll_wait | libc::SYS_epoll_pwait | libc::SYS_epoll_pwait2
            if fd_arg(event, 0) == event_fd =>
        {
            let events = event.args[2] as i32;
            let most = i32::MAX / core::mem::size_of::<libc::epoll_event>() as i32;
            Some(-i64::from(if events <= 0 || events > most {
                libc::EINVAL
            } else {
                libc::EBADF
            }))
        }
        _ => None,
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

/// Emit the refusal of a fault outside every arena with its absolute RIP, the
/// `/proc/self/maps` path of the mapping containing it (`[anon]` when the
/// mapping has none, `[unmapped]` when no mapping contains it), and the bytes
/// that could be read there. Allocation-free and signal-safe; it reads the
/// maps file only when the stage stream is enabled.
fn emit_unarenaed_refusal_stage(stage: &[u8], address: u64, bytes: &[u8]) {
    if !IN_GUEST_STAGE_STREAM.load(Ordering::Acquire) {
        return;
    }
    let mut name = [0_u8; 256];
    let name = match unsafe { mapping_name_at(address, &mut name) } {
        Some(0) => b"[anon]".as_slice(),
        Some(len) => &name[..len],
        None => b"[unmapped]".as_slice(),
    };
    let mut line = StackLine::new();
    line.push_bytes(b"INFO reverie_liteinst::tool_host: [in-guest pid=");
    line.push_signed(unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) });
    line.push_bytes(b" tid=");
    line.push_signed(unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) });
    line.push_bytes(b"] stage=");
    line.push_bytes(stage);
    line.push_bytes(b" rip=0x");
    line.push_hex(address);
    line.push_bytes(b" bytes=");
    for (index, byte) in bytes.iter().enumerate() {
        if index != 0 {
            line.push_bytes(b"-");
        }
        line.push_hex_byte(*byte);
    }
    line.push_bytes(b" map=");
    line.push_bytes(name);
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

/// Copy the pathname field of the `/proc/self/maps` line containing
/// `address` into `name`, truncated to its length. Returns the copied length,
/// 0 for a mapping without a pathname, or `None` when no line contains the
/// address or the file cannot be read. Uses only raw syscalls and stack
/// buffers, so it is usable in signal context.
unsafe fn mapping_name_at(address: u64, name: &mut [u8]) -> Option<usize> {
    unsafe { scan_own_maps(|line| maps_line_name(line, address, name)) }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(liteinst-straddler-cpuid): Review the private-mapping gate.
// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(liteinst-straddler-cpuid): Review the private-only publication rule.
/// The protection of the one private mapping of `/proc/self/maps` that holds
/// `address` through `last` (inclusive), or `None` when that mapping is
/// shared, `last` lies in another mapping, no line contains `address`, or the
/// file cannot be read. Usable in signal context, like [`mapping_name_at`].
unsafe fn private_span_protection(address: u64, last: u64) -> Option<i32> {
    unsafe { scan_own_maps(|line| maps_line_private(line, address)) }
        .and_then(|(private, end, protection)| (private && last < end).then_some(protection))
}

/// Feed each line of `/proc/self/maps` to `line_result` until it returns
/// `Some`, and return that. Uses only raw syscalls and stack buffers; a line
/// longer than the buffer is truncated, which keeps its leading fields.
unsafe fn scan_own_maps<R>(mut line_result: impl FnMut(&[u8]) -> Option<R>) -> Option<R> {
    let path = c"/proc/self/maps";
    let fd = unsafe {
        raw_syscall6(
            libc::SYS_openat,
            [
                libc::AT_FDCWD as u64,
                path.as_ptr() as u64,
                (libc::O_RDONLY | libc::O_CLOEXEC) as u64,
                0,
                0,
                0,
            ],
        )
    };
    if fd < 0 {
        return None;
    }
    let mut chunk = [0_u8; 1024];
    let mut line = [0_u8; 512];
    let mut line_len = 0;
    let mut found = None;
    'read: loop {
        let read = unsafe {
            raw_syscall6(
                libc::SYS_read,
                [
                    fd as u64,
                    chunk.as_mut_ptr() as u64,
                    chunk.len() as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        let Ok(read) = usize::try_from(read) else {
            break;
        };
        if read == 0 {
            break;
        }
        for byte in chunk[..read.min(chunk.len())].iter().copied() {
            if byte != b'\n' {
                if line_len < line.len() {
                    line[line_len] = byte;
                    line_len += 1;
                }
                continue;
            }
            if let Some(result) = line_result(&line[..line_len]) {
                found = Some(result);
                break 'read;
            }
            line_len = 0;
        }
    }
    let _ = unsafe { raw_syscall6(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]) };
    found
}

/// Parse one `/proc/self/maps` line; when its range contains `address`,
/// return whether its permissions mark it private (`p`, not shared `s`), the
/// end of its range, and its protection as `PROT_*` bits.
fn maps_line_private(line: &[u8], address: u64) -> Option<(bool, u64, i32)> {
    let mut fields = line
        .split(|byte| *byte == b' ')
        .filter(|field| !field.is_empty());
    let range = fields.next()?;
    let permissions = fields.next()?;
    let dash = range.iter().position(|byte| *byte == b'-')?;
    let parse = |bytes: &[u8]| {
        (!bytes.is_empty() && bytes.len() <= 16)
            .then(|| core::str::from_utf8(bytes).ok())
            .flatten()
            .and_then(|text| u64::from_str_radix(text, 16).ok())
    };
    let (start, end) = (parse(&range[..dash])?, parse(&range[dash + 1..])?);
    let protection = [
        (0, b'r', libc::PROT_READ),
        (1, b'w', libc::PROT_WRITE),
        (2, b'x', libc::PROT_EXEC),
    ]
    .into_iter()
    .filter(|(index, flag, _)| permissions.get(*index) == Some(flag))
    .fold(0, |bits, (_, _, bit)| bits | bit);
    (start <= address && address < end)
        .then(|| (permissions.get(3) == Some(&b'p'), end, protection))
}

/// Parse one `/proc/self/maps` line; when its range contains `address`, copy
/// its pathname (possibly empty) into `name` and return the copied length.
fn maps_line_name(line: &[u8], address: u64, name: &mut [u8]) -> Option<usize> {
    fn hex(bytes: &[u8]) -> Option<u64> {
        if bytes.is_empty() || bytes.len() > 16 {
            return None;
        }
        bytes.iter().try_fold(0_u64, |value, byte| {
            let digit = (*byte as char).to_digit(16)?;
            Some((value << 4) | u64::from(digit))
        })
    }
    let range_end = line.iter().position(|byte| *byte == b' ')?;
    let range = &line[..range_end];
    let dash = range.iter().position(|byte| *byte == b'-')?;
    let start = hex(&range[..dash])?;
    let end = hex(&range[dash + 1..])?;
    if address < start || address >= end {
        return None;
    }
    // Skip the range, permissions, offset, device and inode fields; the
    // remainder after their separating spaces is the pathname.
    let mut rest = line;
    for _ in 0..5 {
        let field_end = rest
            .iter()
            .position(|byte| *byte == b' ')
            .unwrap_or(rest.len());
        rest = &rest[field_end..];
        let spaces = rest.iter().take_while(|byte| **byte == b' ').count();
        rest = &rest[spaces..];
    }
    let len = rest.len().min(name.len());
    name[..len].copy_from_slice(&rest[..len]);
    Some(len)
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
    bytes: [u8; 512],
    len: usize,
}

impl StackLine {
    const fn new() -> Self {
        Self {
            bytes: [0; 512],
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
    use super::AsyncSignalsBlocked;
    use super::ENABLED_FALLBACK_CLASSIFICATIONS;
    use super::FORK_HOOK;
    use super::FallbackCounters;
    use super::LiteinstDispatcher;
    use super::MAX_PATCH_SITES;
    use super::NeighbourConflict;
    use super::ObjectImage;
    use super::RCB_CLOCK;
    use super::RCB_CLOCK_OWNER;
    use super::RCB_CLOCK_UNAVAILABLE;
    use super::SITE_ACTIVE;
    use super::SITE_FALLBACK;
    use super::SITE_INSTALLING;
    use super::SITE_STALE;
    use super::SITE_UNPATCHABLE;
    use super::SITES;
    use super::SiteSlot;
    use super::StackLine;
    use super::TOOL_PASSTHROUGH;
    use super::TOOL_SPOOF_GETPID;
    use super::alt_stack_from_env_value;
    use super::builtin_tool_from_env_value;
    use super::claim_site;
    use super::clone_is_fork_like;
    use super::earlier_patch_survives;
    use super::fallback_dispatch_count;
    use super::fallback_syscall_count;
    use super::initialize_rcb_clock_with;
    use super::maps_line_private;
    use super::mark_site_range_stale;
    use super::neighbour_conflict;
    use super::neighbours_conflict;
    use super::object_image;
    use super::parse_runtime_maps;
    use super::raw_syscall6;
    use super::record_fallback_dispatch;
    use super::refused_process_creation;
    use super::reset_site_observability;
    use super::site_footprint;
    use super::stale_jump_survives;

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

    /// The census reads an object through the mappings recorded at
    /// initialization (https://github.com/rrnewton/reverie/issues/812), so
    /// those must be exactly the readable mappings of the text mapping's file.
    #[test]
    fn object_image_records_the_readable_mappings_of_the_text_file() {
        let maps = parse_runtime_maps(concat!(
            "555555554000-555555556000 r--p 00000000 00:1f 11 /usr/bin/guest\n",
            "555555556000-555555558000 r-xp 00002000 00:1f 11 /usr/bin/guest\n",
            "555555558000-55555555a000 rw-p 00004000 00:1f 11 /usr/bin/guest\n",
            "55555555a000-55555557b000 rw-p 00000000 00:00 0 [heap]\n",
            "7ffff7c00000-7ffff7c28000 r--p 00000000 00:1f 22 /usr/lib64/libc.so.6\n",
            "7ffff7c28000-7ffff7db0000 r-xp 00028000 00:1f 22 /usr/lib64/libc.so.6\n",
            "7ffff7db0000-7ffff7dff000 r--p 001b0000 00:1f 22 /usr/lib64/libc.so.6\n",
            "7ffff7dff000-7ffff7e00000 ---p 001ff000 00:1f 22 /usr/lib64/libc.so.6\n",
            "7ffff7e00000-7ffff7e04000 r--p 001ff000 00:1f 22 /usr/lib64/libc.so.6\n",
            "7ffff7e04000-7ffff7e06000 rw-p 00203000 00:1f 22 /usr/lib64/libc.so.6\n",
            "7ffff7e06000-7ffff7e13000 rw-p 00000000 00:00 0\n",
            "7ffff7f00000-7ffff7f01000 r-xp 00000000 00:00 0\n",
            "7ffff7f10000-7ffff7f11000 r--p 00000000 00:2a 22 /other/device/same-inode\n",
            "7ffff7f20000-7ffff7f21000 r--p 00000000 00:1f 33 /twice.so\n",
            "7ffff7f21000-7ffff7f22000 r-xp 00001000 00:1f 33 /twice.so\n",
            "7ffff7f30000-7ffff7f31000 r--p 00000000 00:1f 33 /twice.so\n",
        ));

        // The gap mapping (---p), the anonymous .bss continuation and the
        // mapping of another device's inode 22 are all left out.
        assert_eq!(
            object_image(&maps, 0x7fff_f7c2_8000, 0x7fff_f7db_0000),
            Some(ObjectImage::new(
                0x7fff_f7c0_0000,
                vec![
                    (0x7fff_f7c0_0000, 0x7fff_f7c2_8000, false),
                    (0x7fff_f7c2_8000, 0x7fff_f7db_0000, true),
                    (0x7fff_f7db_0000, 0x7fff_f7df_f000, false),
                    (0x7fff_f7e0_0000, 0x7fff_f7e0_4000, false),
                    (0x7fff_f7e0_4000, 0x7fff_f7e0_6000, false),
                ]
                .into_boxed_slice(),
            ))
        );
        assert_eq!(
            object_image(&maps, 0x5555_5555_6000, 0x5555_5555_8000),
            Some(ObjectImage::new(
                0x5555_5555_4000,
                vec![
                    (0x5555_5555_4000, 0x5555_5555_6000, false),
                    (0x5555_5555_6000, 0x5555_5555_8000, true),
                    (0x5555_5555_8000, 0x5555_5555_a000, false),
                ]
                .into_boxed_slice(),
            ))
        );
        // Anonymous text, a file mapped twice (two headers), and a range that
        // is not an executable mapping have no image.
        assert_eq!(
            object_image(&maps, 0x7fff_f7f0_0000, 0x7fff_f7f0_1000),
            None
        );
        assert_eq!(
            object_image(&maps, 0x7fff_f7f2_1000, 0x7fff_f7f2_2000),
            None
        );
        assert_eq!(
            object_image(&maps, 0x7fff_f7c0_0000, 0x7fff_f7c2_8000),
            None
        );
        assert_eq!(
            object_image(&maps, 0x7fff_f7c2_8000, 0x7fff_f7d0_0000),
            None
        );
    }

    // https://github.com/rrnewton/reverie/issues/758
    #[test]
    fn strace_and_compat_refuse_vfork_and_clone3_by_number() {
        let bookkeeping = (libc::CLONE_CHILD_CLEARTID
            | libc::CLONE_CHILD_SETTID
            | libc::CLONE_PARENT_SETTID) as u64;
        for mode in [super::TOOL_STRACE, super::TOOL_COMPAT] {
            let clone_errno = if mode == super::TOOL_COMPAT {
                libc::EPERM
            } else {
                libc::ENOTSUP
            };
            for (number, args, expected) in [
                // Refused by number, whatever the clone_args pointer and size.
                (libc::SYS_vfork, [0; 6], Some(libc::ENOTSUP)),
                (libc::SYS_clone3, [0; 6], Some(libc::ENOTSUP)),
                (libc::SYS_clone3, [u64::MAX; 6], Some(libc::ENOTSUP)),
                (
                    libc::SYS_clone,
                    [libc::CLONE_VM as u64 | libc::SIGCHLD as u64, 0, 0, 0, 0, 0],
                    Some(clone_errno),
                ),
                (
                    libc::SYS_clone,
                    [libc::SIGCHLD as u64, 1, 0, 0, 0, 0],
                    Some(clone_errno),
                ),
                (libc::SYS_fork, [0; 6], None),
                (
                    libc::SYS_clone,
                    [libc::SIGCHLD as u64 | bookkeeping, 0, 0, 0, 0, 0],
                    None,
                ),
                (libc::SYS_getpid, [0; 6], None),
            ] {
                assert_eq!(
                    refused_process_creation(number, args, mode),
                    expected,
                    "mode {mode} syscall {number} args {args:?}"
                );
            }
        }
    }

    #[test]
    fn decode_recognizes_only_the_faulting_instruction_encodings() {
        use super::InstructionEventKind;
        use super::decode_instruction;
        let decoded =
            |bytes: &[u8]| decode_instruction(bytes).map(|(kind, expected)| (kind, expected.len()));
        assert_eq!(
            decoded(&[0x0f, 0xa2]),
            Some((InstructionEventKind::Cpuid, 2))
        );
        assert_eq!(
            decoded(&[0x0f, 0xa2, 0xc3]),
            Some((InstructionEventKind::Cpuid, 2))
        );
        assert_eq!(
            decoded(&[0x0f, 0x31, 0xc3]),
            Some((InstructionEventKind::Rdtsc, 2))
        );
        assert_eq!(
            decoded(&[0x0f, 0x01, 0xf9]),
            Some((InstructionEventKind::Rdtscp, 3))
        );
        assert_eq!(
            decoded(&[0x0f, 0x01, 0xf9, 0xc3]),
            Some((InstructionEventKind::Rdtscp, 3))
        );
        for kind in [
            InstructionEventKind::Cpuid,
            InstructionEventKind::Rdtsc,
            InstructionEventKind::Rdtscp,
        ] {
            assert_eq!(
                decoded(match kind {
                    InstructionEventKind::Cpuid => &[0x0f, 0xa2],
                    InstructionEventKind::Rdtsc => &[0x0f, 0x31],
                    InstructionEventKind::Rdtscp => &[0x0f, 0x01, 0xf9],
                })
                .map(|(_, len)| len as u64),
                Some(kind.encoded_len())
            );
        }
        // A truncated RDTSCP, a different 0f 01 group member (rdpid is not),
        // syscall, ud2 and a load are not emulated.
        for bytes in [
            &[0x0f, 0x01][..],
            &[0x0f, 0x01, 0xf8],
            &[0x0f, 0x05],
            &[0x0f, 0x0b],
            &[0x48, 0x8b, 0x00],
            &[0x0f],
            &[],
        ] {
            assert_eq!(decoded(bytes), None, "{bytes:02x?}");
        }
    }

    #[test]
    fn maps_line_name_extracts_the_containing_mapping_path() {
        use super::maps_line_name;
        let mut name = [0_u8; 64];
        let line = b"7f1234560000-7f1234570000 r-xp 00002000 fd:01 1234                       /usr/lib64/libcrypto.so.3";
        assert_eq!(maps_line_name(line, 0x7f12_3456_0000, &mut name), Some(25));
        assert_eq!(&name[..25], b"/usr/lib64/libcrypto.so.3");
        assert_eq!(maps_line_name(line, 0x7f12_3456_ffff, &mut name), Some(25));
        assert_eq!(maps_line_name(line, 0x7f12_3457_0000, &mut name), None);
        assert_eq!(maps_line_name(line, 0x7f12_3455_ffff, &mut name), None);
        let anonymous = b"7f0000000000-7f0000001000 rwxp 00000000 00:00 0 ";
        assert_eq!(
            maps_line_name(anonymous, 0x7f00_0000_0800, &mut name),
            Some(0)
        );
        let bare = b"7f0000000000-7f0000001000 rwxp 00000000 00:00 0";
        assert_eq!(maps_line_name(bare, 0x7f00_0000_0800, &mut name), Some(0));
        let mut short = [0_u8; 4];
        assert_eq!(maps_line_name(line, 0x7f12_3456_0000, &mut short), Some(4));
        assert_eq!(&short, b"/usr");
        assert_eq!(maps_line_name(b"garbage", 0, &mut name), None);
        assert_eq!(maps_line_name(b"", 0, &mut name), None);
    }

    #[test]
    fn mapping_name_at_reads_this_process_maps_without_allocation() {
        use super::mapping_name_at;
        let page = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(page, libc::MAP_FAILED);
        let mut name = [0_u8; 256];
        assert_eq!(
            unsafe { mapping_name_at(page as u64 + 16, &mut name) },
            Some(0)
        );
        let code = mapping_name_at as *const () as usize as u64;
        let len = unsafe { mapping_name_at(code, &mut name) }.unwrap();
        let executable = std::env::current_exe().unwrap();
        assert_eq!(
            std::str::from_utf8(&name[..len]).unwrap(),
            executable.to_str().unwrap()
        );
        unsafe { libc::munmap(page, 4096) };
        assert_eq!(
            unsafe { mapping_name_at(page as u64 + 16, &mut name) },
            None
        );
    }

    #[test]
    fn own_byte_reads_stop_at_an_unreadable_page_without_faulting() {
        use super::read_own_bytes;
        let pages = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                8192,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        }
        .cast::<u8>();
        assert_ne!(pages.cast(), libc::MAP_FAILED);
        unsafe {
            pages.add(4094).write(0x0f);
            pages.add(4095).write(0xa2);
            assert_eq!(
                libc::mprotect(pages.add(4096).cast(), 4096, libc::PROT_NONE),
                0
            );
        }
        let mut bytes = [0_u8; 8];
        assert_eq!(
            unsafe { read_own_bytes(pages as u64 + 4094, &mut bytes) },
            2
        );
        assert_eq!(&bytes[..2], &[0x0f, 0xa2]);
        assert_eq!(
            unsafe { read_own_bytes(pages as u64 + 4096, &mut bytes) },
            0
        );
        assert_eq!(unsafe { read_own_bytes(0, &mut bytes) }, 0);
        assert_eq!(unsafe { read_own_bytes(u64::MAX - 1, &mut bytes) }, 0);
        unsafe { libc::munmap(pages.cast(), 8192) };
    }
    #[test]
    fn neighbour_conflicts_protect_published_patches_and_continuation_sites() {
        use NeighbourConflict::CoversContinuationSite as Covers;
        use NeighbourConflict::InsidePublishedPatch as Inside;
        use NeighbourConflict::OverlapsPublishedPatch as Overlaps;
        const A: u64 = 0x1000;
        // (other site, its state, its recorded length, expected conflict for a
        // 2-byte instruction at A whose word patch is [A, A + 8)).
        let cases = [
            (A, SITE_ACTIVE, 2, None),
            (0, SITE_ACTIVE, 2, None),
            // A published jump that covers A or the resume point A + 2.
            (A - 7, SITE_ACTIVE, 2, Some(Inside)),
            (A - 1, SITE_FALLBACK, 2, Some(Inside)),
            (A + 1, SITE_ACTIVE, 2, Some(Inside)),
            // A published jump starting at or after the resume point, inside
            // the proposed patch: resuming is safe, patching is not.
            (A + 2, SITE_ACTIVE, 2, Some(Overlaps)),
            (A + 7, SITE_FALLBACK, 2, Some(Overlaps)),
            (A + 8, SITE_ACTIVE, 2, None),
            (A - 8, SITE_ACTIVE, 2, None),
            // A site executing through the continuation, or being decided.
            (A + 4, SITE_UNPATCHABLE, 2, Some(Covers)),
            (A + 7, SITE_INSTALLING, 0, Some(Covers)),
            (A + 8, SITE_UNPATCHABLE, 2, None),
            // The proposed patch may start exactly at its resume point...
            (A - 2, SITE_UNPATCHABLE, 2, None),
            (A - 3, SITE_UNPATCHABLE, 3, None),
            // ...but not inside it; an unrecorded length is taken as 3.
            (A - 2, SITE_UNPATCHABLE, 3, Some(Covers)),
            (A - 2, SITE_INSTALLING, 0, Some(Covers)),
            (A - 3, SITE_INSTALLING, 0, None),
            // Unclaimed sites constrain nothing; a stale site keeps its
            // continuation reservation even without a footprint.
            (A + 4, 0, 2, None),
            (A + 4, SITE_STALE, 2, Some(Covers)),
            (A + 8, SITE_STALE, 2, None),
        ];
        for (other, state, other_len, expected) in cases {
            assert_eq!(
                neighbour_conflict(
                    A,
                    2,
                    other,
                    state,
                    other_len,
                    matches!(state, SITE_ACTIVE | SITE_FALLBACK),
                ),
                expected,
                "other={other:#x} state={state} len={other_len}"
            );
        }
    }
    #[test]
    fn a_covering_published_patch_dominates_whatever_the_table_order() {
        use NeighbourConflict::InsidePublishedPatch as Inside;
        use NeighbourConflict::OverlapsPublishedPatch as Overlaps;
        const A: u64 = 0x1000;
        // Codex's arrangement: a FALLBACK syscall at A + 4 (untouched overlap)
        // and an ACTIVE jump at A - 2 that covers A (touched), in both orders.
        let overlap = (A + 4, SITE_FALLBACK, 2, true);
        let covering = (A - 2, SITE_ACTIVE, 2, true);
        assert_eq!(neighbours_conflict(A, 2, [overlap, covering]), Some(Inside));
        assert_eq!(neighbours_conflict(A, 2, [covering, overlap]), Some(Inside));
        assert_eq!(neighbours_conflict(A, 2, [overlap]), Some(Overlaps));
        assert_eq!(
            neighbours_conflict(A, 2, [(A + 8, SITE_ACTIVE, 2, true)]),
            None
        );
    }
    #[test]
    fn a_stale_site_counts_as_published_unless_its_jump_is_provably_gone() {
        use NeighbourConflict::InsidePublishedPatch as Inside;
        let word = |bytes: [u8; 8]| u64::from_le_bytes(bytes);
        // Codex's arrangement: a cross-line jump at P = page end - 2.
        let original = [0x0f, 0xa2, 0x0f, 0xa2, 0x90, 0xf8, 0x90, 0x90];
        let published = [0xe9, 0xfd, 0x0f, 0xa2, 0xff, 0xf8, 0x90, 0x90];
        let words = Some((word(original), word(published)));
        // All of it in place (a no-op mremap): survives.
        assert!(stale_jump_survives(words, Some(published)));
        // The first page replaced with its original contents, the second
        // still holding the displacement's high bytes: survives.
        let partial = [0x0f, 0xa2, 0x0f, 0xa2, 0xff, 0xf8, 0x90, 0x90];
        assert!(stale_jump_survives(words, Some(partial)));
        // A surviving INT3 guard, even where the patch kept the byte: survives.
        let guarded = [0xcc, 0xcc, 0x0f, 0xa2, 0x90, 0xf8, 0x90, 0x90];
        assert!(stale_jump_survives(words, Some(guarded)));
        let guard_on_unchanged_byte = [0x0f, 0xa2, 0xcc, 0xa2, 0x90, 0xf8, 0x90, 0x90];
        assert!(stale_jump_survives(words, Some(guard_on_unchanged_byte)));
        // Nothing recorded, or not all eight bytes readable: survives.
        assert!(stale_jump_survives(None, Some(original)));
        assert!(stale_jump_survives(words, None));
        // Fully restored, or replaced by unrelated code: gone.
        assert!(!stale_jump_survives(words, Some(original)));
        let unrelated = [0x48, 0x89, 0xe5, 0x31, 0xc0, 0x5d, 0xc3, 0x90];
        assert!(!stale_jump_survives(words, Some(unrelated)));

        // Never published: nothing can survive, whatever the bytes.
        assert!(!earlier_patch_survives(false, None, Some(published)));
        assert!(earlier_patch_survives(true, words, Some(partial)));
        assert!(earlier_patch_survives(true, None, Some(original)));
        assert!(!earlier_patch_survives(true, words, Some(original)));
        assert!(site_footprint(SITE_ACTIVE, false));
        // A FALLBACK refused before any change wrote nothing.
        for state in [
            0,
            SITE_INSTALLING,
            SITE_STALE,
            SITE_UNPATCHABLE,
            SITE_FALLBACK,
        ] {
            assert!(site_footprint(state, true));
            assert!(!site_footprint(state, false));
        }
        // The partially replaced jump at P still refuses a CPUID at P + 2 as
        // touched, whether P is STALE or was reclaimed (INSTALLING) after the
        // restored CPUID at P executed first; once provably gone, a STALE P
        // constrains nothing.
        const P: u64 = 0x1000_0ffe;
        let survives = earlier_patch_survives(true, words, Some(partial));
        for state in [SITE_STALE, SITE_INSTALLING, SITE_UNPATCHABLE] {
            let site = (P, state, 2, site_footprint(state, survives));
            assert_eq!(
                neighbours_conflict(P + 2, 2, [site]),
                Some(Inside),
                "state={state}"
            );
        }
        let gone = earlier_patch_survives(true, words, Some(original));
        let site = (P, SITE_STALE, 2, site_footprint(SITE_STALE, gone));
        assert_eq!(neighbours_conflict(P + 2, 2, [site]), None);
    }
    #[test]
    fn maps_line_private_reads_the_sharing_flag_of_the_containing_mapping() {
        let private = b"55c2c6466000-55c2c6467000 r-xp 00017000 00:2f 1234      /bin/coreutils";
        let shared = b"7f0000000000-7f0000001000 r-xs 00000000 00:05 99   /memfd:jit (deleted)";
        assert_eq!(
            maps_line_private(private, 0x55c2_c646_6d3a),
            Some((true, 0x55c2_c646_7000, libc::PROT_READ | libc::PROT_EXEC))
        );
        assert_eq!(
            maps_line_private(shared, 0x7f00_0000_0010),
            Some((false, 0x7f00_0000_1000, libc::PROT_READ | libc::PROT_EXEC))
        );
        assert_eq!(maps_line_private(private, 0x55c2_c646_7000), None);
        assert_eq!(maps_line_private(b"garbage", 0), None);
    }
    #[test]
    fn installation_blocks_every_blockable_signal_and_restores_the_mask() {
        let current = || {
            let mut mask = 0_u64;
            let result = unsafe {
                libc::syscall(
                    libc::SYS_rt_sigprocmask,
                    libc::SIG_BLOCK,
                    core::ptr::null::<u64>(),
                    &raw mut mask,
                    core::mem::size_of::<u64>(),
                )
            };
            assert_eq!(result, 0);
            mask
        };
        let bit = |signal: libc::c_int| 1_u64 << (signal - 1);
        let before = current();
        {
            let _blocked = AsyncSignalsBlocked::new().unwrap();
            let during = current();
            // Every number a guest handler could be installed for, the fault
            // signals included (a guest timer can deliver SIGILL).
            for signal in (1..=64).filter(|signal| ![libc::SIGKILL, libc::SIGSTOP].contains(signal))
            {
                assert_ne!(during & bit(signal), 0, "signal {signal} must be blocked");
            }
        }
        assert_eq!(
            current(),
            before,
            "the previous mask must be restored exactly"
        );
    }
}
