//! Private, single-thread instruction continuation for the qualified native profile.
//!
//! The installing caller keeps mappings, TLS, xstate permissions and native
//! controls stable, owns all signal dispositions, and permits no asynchronous
//! callbacks, additional threads, nonlocal exits or writes to runtime storage.
//! No POSIX timer may have been created since fresh exec, including by startup
//! code, nor created or armed during the bounded run. Optional procfs inventory
//! is an additional rejection check, not the basis of that caller guarantee.
//! Tool callbacks return through the owned runtime stack; a tail exit instead
//! completes shared cleanup and consumes a checked terminal request there. This is
//! ordinary in-process ownership, not a hostile-guest isolation boundary.
//!
//! A is the existing alternate signal stack; R and Q are disjoint process-life
//! allocations. Entry to R retires A, not Q. Q remains borrowed through the
//! final return and is recycled only at the next admitted instruction capture.
//! No admitted returning asynchronous source can retain a reader of A or Q.
//! Both returns preserve an authentic native frame in the fixed environment;
//! opcode/PC matching alone is not used as proof of successful restoration.

use std::cell::Cell;
pub(crate) mod stack;
use std::io;
use std::ops::Range;
use std::ptr;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use liteinst2::trampoline::HookContext;
use reverie_preload::clock_boundary::Continuation;
use reverie_preload::signal::native_frame::Format;
use reverie_preload::signal::native_frame::RelocatedFrame;
use reverie_preload::signal::native_frame::relocate;
use reverie_preload::trap::raw_syscall6;

use crate::instruction_event::InstructionEvent;
use crate::instruction_event::Kind;
use crate::owned_step::Completion;
use crate::owned_step::Stepper;
pub(crate) mod exit;
mod native_step;
#[cfg(feature = "private-crt")]
pub(crate) mod tls;

pub(crate) fn tls_mode() -> bool {
    #[cfg(feature = "private-crt")]
    {
        tls::ready()
    }
    #[cfg(not(feature = "private-crt"))]
    {
        false
    }
}

const FEATURES: u64 = 0x2e7;
const XSTATE_BYTES: u32 = 2440;
const STORAGE_BYTES: usize = 4096;
const RUNTIME_BYTES: usize = 1024 * 1024;
const IDLE: u8 = 0;
const CAPTURED: u8 = 1;
const RUNTIME: u8 = 2;
const RETURNING: u8 = 3;
const FAILED: u8 = 4;
const TERMINATING: u8 = 5;

#[repr(align(64))]
struct Storage([u8; STORAGE_BYTES]);

// Keep capture storage allocation-free while an event is being handled.
#[expect(clippy::large_enum_variant)]
enum CapturedEvent {
    #[cfg(feature = "private-crt")]
    Initial(u64),
    Instruction(InstructionEvent, bool),
    Syscall(crate::syscall_event::SyscallEvent, bool),
    Vdso(crate::vdso::Call, bool),
    VdsoEntry,
    VdsoFault(VdsoFault),
    Step(Completion),
}

#[derive(Clone, Copy)]
struct VdsoFault {
    signal: i32,
    code: i32,
    address: u64,
    trap: i64,
    error: i64,
    clock: u64,
    modeled: Option<crate::owned_step::ReadFaultCapture>,
}

impl VdsoFault {
    fn report_with(
        &self,
        context: &HookContext,
        mut emit: impl FnMut(&'static str, &'static str, Option<i64>),
    ) {
        for (field, value) in [
            ("signal", i64::from(self.signal)),
            ("code", i64::from(self.code)),
            ("trap", self.trap),
            ("error", self.error),
        ] {
            emit("vdso/data-fault", field, Some(value));
        }
        for (high, low, value) in [
            ("pc-high32", "pc-low32", context.instruction_pointer),
            ("address-high32", "address-low32", self.address),
            ("flags-high32", "flags-low32", context.rflags),
            ("count-high32", "count-low32", self.clock),
        ] {
            emit("vdso/data-fault", high, Some((value >> 32) as i64));
            emit("vdso/data-fault", low, Some(i64::from(value as u32)));
        }
        for (high, low, value) in [
            ("rdi-high32", "rdi-low32", context.rdi),
            ("rsi-high32", "rsi-low32", context.rsi),
            ("rdx-high32", "rdx-low32", context.rdx),
            ("rcx-high32", "rcx-low32", context.rcx),
            ("r8-high32", "r8-low32", context.r8),
            ("r14-high32", "r14-low32", context.r14),
        ] {
            emit("vdso/fault-registers", high, Some((value >> 32) as i64));
            emit("vdso/fault-registers", low, Some(i64::from(value as u32)));
        }
    }
}

fn vdso_data_fault(code: i32, registers: &[libc::greg_t; 23]) -> bool {
    match registers[libc::REG_TRAPNO as usize] {
        14 => matches!(code, 1 | 2) && registers[libc::REG_ERR as usize] & 0x10 == 0,
        13 => code == libc::SI_KERNEL && registers[libc::REG_ERR as usize] == 0,
        _ => false,
    }
}

struct OwnedContext {
    owner: i64,
    mappings: Vec<Range<u64>>,
    readable: Vec<Range<u64>>,
    writable: Vec<Range<u64>>,
    vdso: Option<crate::vdso::Active>,
    signal_stack: Range<usize>,
    runtime_sp: usize,
    runtime_stack: Range<usize>,
    storage: *mut Storage,
    image: Option<RelocatedFrame<'static>>,
    registers: HookContext,
    event: Option<CapturedEvent>,
    stepper: Option<Stepper>,
    precise_timer: bool,
    syscalls: bool,
    native: Option<native_step::NativeState>,
    guest_mask: u64,
    subscriptions: crate::runtime::InstructionSubscriptions,
    clocked: bool,
    controls: Controls,
    errno: *mut i32,
    saved_errno: i32,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Controls {
    segments_and_permissions: [u64; 3],
    xcr0: u64,
    pkru: u32,
    cpuid: i64,
    tsc: i32,
}

thread_local! {
    static STATE: AtomicPtr<OwnedContext> = const { AtomicPtr::new(ptr::null_mut()) };
    static PHASE: AtomicU8 = const { AtomicU8::new(IDLE) };
    static EVIDENCE: AtomicPtr<Evidence> = const { AtomicPtr::new(ptr::null_mut()) };
}

#[cfg(feature = "private-crt")]
struct InitialObservation {
    phase: u8,
    mask: Option<u64>,
    paused: bool,
    handoff_clear: bool,
    owner: i64,
}

#[cfg(feature = "private-crt")]
pub(crate) fn prepare_initial() -> io::Result<u64> {
    let state = unsafe { STATE.with(|slot| slot.load(Ordering::Acquire)).as_ref() }
        .ok_or_else(unsupported)?;
    validate_initial_preparation(
        state,
        InitialObservation {
            phase: PHASE.with(|slot| slot.load(Ordering::Acquire)),
            mask: environment_mask(state, EnvironmentPhase::InitialGuest),
            paused: crate::clock_control::paused(),
            handoff_clear: crate::clock_control::handoff_clear(),
            owner: syscall(libc::SYS_gettid, [0; 6]),
        },
        crate::runtime::read_guest_rcb_clock,
    )
}

#[cfg(feature = "private-crt")]
fn validate_initial_preparation(
    state: &OwnedContext,
    observed: InitialObservation,
    clock: impl FnOnce() -> io::Result<u64>,
) -> io::Result<u64> {
    if observed.phase != IDLE
        || state.generation != 0
        || state.native.is_none()
        || state.stepper.is_none()
        || !state.precise_timer
        || !state.syscalls
        || !state.clocked
        || !state.subscriptions.cpuid
        || state.controls.cpuid != 0
        || observed.mask.is_none()
        || !observed.paused
        || !observed.handoff_clear
        || observed.owner != state.owner
        || clock()? != 0
    {
        return Err(unsupported());
    }
    observed.mask.ok_or_else(unsupported)
}

struct Evidence {
    bytes: [u8; 32 * (STORAGE_BYTES + 64)],
    used: usize,
}

thread_local! {
    static DISPATCH_CAPTURE: Cell<Option<(u64, i64, u64)>> = const { Cell::new(None) };
}

struct SyscallDispatchCapture;

impl Drop for SyscallDispatchCapture {
    fn drop(&mut self) {
        DISPATCH_CAPTURE.set(None);
    }
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
/// Actual owned frame generation, syscall number and site during its Tool callback.
pub fn __owned_syscall_capture() -> Option<(u64, i64, u64)> {
    if PHASE.with(|slot| slot.load(Ordering::Relaxed)) != RUNTIME
        || !crate::runtime::nested_tool_callback()
    {
        return None;
    }
    DISPATCH_CAPTURE.get()
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
#[unsafe(no_mangle)]
/// Read-only first-event diagnostic, not an admission or continuation API.
///
/// # Safety
/// Called in an ordinary Tool callback with six writable u64 output slots.
pub unsafe extern "C" fn reverie_liteinst_owned_capture_diagnostic(output: *mut u64) {
    let capture = DISPATCH_CAPTURE.get();
    let (generation, number, site) = capture.unwrap_or((0, 0, 0));
    let observation = [
        u64::from(PHASE.with(|slot| slot.load(Ordering::Relaxed))),
        u64::from(crate::runtime::nested_tool_callback()),
        u64::from(capture.is_some()),
        generation,
        number as u64,
        site,
    ];
    unsafe { ptr::copy_nonoverlapping(observation.as_ptr(), output, observation.len()) };
}

pub(crate) static SYSCALL_CAPTURE: reverie_preload::trap::OwnedUserDispatch =
    reverie_preload::trap::OwnedUserDispatch {
        capture: capture_syscall,
    };

pub(crate) fn syscall_mode() -> bool {
    unsafe { STATE.with(|slot| slot.load(Ordering::Acquire)).as_ref() }
        .is_some_and(|state| state.syscalls)
}

#[cfg(test)]
pub(crate) fn with_routing_syscall_state<Result>(
    run: impl FnOnce() -> Result,
) -> io::Result<Result> {
    if !STATE.with(|slot| slot.load(Ordering::Acquire)).is_null()
        || PHASE.with(|slot| slot.load(Ordering::Acquire)) != IDLE
        || !EVIDENCE.with(|slot| slot.load(Ordering::Acquire)).is_null()
    {
        return Err(unsupported());
    }
    struct Published {
        state: Box<OwnedContext>,
        thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }
    impl Drop for Published {
        fn drop(&mut self) {
            let expected = &raw mut *self.state;
            assert!(
                STATE
                    .with(|slot| slot.compare_exchange(
                        expected,
                        ptr::null_mut(),
                        Ordering::AcqRel,
                        Ordering::Acquire
                    ))
                    .is_ok()
            );
        }
    }
    let mut published = Published {
        state: Box::new(OwnedContext {
            owner: unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) },
            mappings: Vec::new(),
            readable: Vec::new(),
            writable: Vec::new(),
            vdso: None,
            signal_stack: 0..0,
            runtime_sp: 0,
            runtime_stack: 0..0,
            storage: ptr::null_mut(),
            image: None,
            registers: unsafe { core::mem::zeroed() },
            event: None,
            stepper: None,
            precise_timer: false,
            syscalls: true,
            native: None,
            guest_mask: 0,
            subscriptions: crate::runtime::InstructionSubscriptions {
                cpuid: false,
                rdtsc: false,
            },
            clocked: false,
            controls: Controls {
                segments_and_permissions: [0; 3],
                xcr0: 0,
                pkru: 0,
                cpuid: 0,
                tsc: 0,
            },
            errno: ptr::null_mut(),
            saved_errno: 0,
            generation: 0,
        }),
        thread: std::marker::PhantomData,
    };
    let pointer = &raw mut *published.state;
    assert!(
        STATE
            .with(|slot| slot.compare_exchange(
                ptr::null_mut(),
                pointer,
                Ordering::AcqRel,
                Ordering::Acquire
            ))
            .is_ok()
    );
    let result = run();
    drop(published);
    Ok(result)
}

pub(crate) fn require_observation_coverage(
    subscriptions: &reverie::Subscription,
) -> io::Result<()> {
    let state = unsafe { STATE.with(|slot| slot.load(Ordering::Acquire)).as_ref() }
        .ok_or_else(unsupported)?;
    if !state.syscalls
        || !state.clocked
        || !state.precise_timer
        || state.stepper.is_none()
        || state.native.is_none()
        || PHASE.with(|slot| slot.load(Ordering::Relaxed)) != IDLE
        || state.generation != 0
        || state.image.is_some()
        || state.event.is_some()
    {
        return Err(unsupported());
    }
    validate_observation_coverage(
        Some(state.owner),
        syscall(libc::SYS_gettid, [0; 6]),
        state.vdso.is_some(),
        subscriptions,
    )
}

fn validate_observation_coverage(
    owner: Option<i64>,
    tid: i64,
    vdso: bool,
    subscriptions: &reverie::Subscription,
) -> io::Result<()> {
    if tid <= 0 || owner != Some(tid) {
        return Err(unsupported());
    }
    if !vdso
        && subscriptions.iter_syscalls().any(|number| {
            matches!(
                number,
                reverie::syscalls::Sysno::clock_gettime
                    | reverie::syscalls::Sysno::clock_getres
                    | reverie::syscalls::Sysno::gettimeofday
                    | reverie::syscalls::Sysno::time
                    | reverie::syscalls::Sysno::getcpu
            )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "requested vDSO observations require the retained mapping owner",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod observation_tests {
    #[test]
    fn complete_subscriptions_require_real_context_and_retained_vdso_owner() {
        let subscriptions = reverie::Subscription::all();
        assert!(super::require_observation_coverage(&subscriptions).is_err());
        for owner in [None, Some(0), Some(8)] {
            assert!(super::validate_observation_coverage(owner, 7, true, &subscriptions).is_err());
        }
        assert!(super::validate_observation_coverage(Some(7), 7, false, &subscriptions).is_err());
        assert!(super::validate_observation_coverage(Some(7), 7, true, &subscriptions).is_ok());
        let finite = [reverie::syscalls::Sysno::getpid].into_iter().collect();
        assert!(super::validate_observation_coverage(Some(7), 7, false, &finite).is_ok());
        assert!(!super::owned_state_published());
    }
}

/// Whether owned context state has actually been published. Host-test
/// observation only; absent from every shipped build.
#[cfg(test)]
pub(crate) fn owned_state_published() -> bool {
    !STATE.with(|slot| slot.load(Ordering::Acquire)).is_null()
}

unsafe fn capture_syscall(
    signal: i32,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
    entry: usize,
) -> Option<Continuation> {
    if !syscall_mode() {
        return None;
    }
    if !crate::clock_control::active()
        || !crate::clock_control::paused()
        || !crate::clock_control::handoff_clear()
    {
        refuse();
    }
    if unsafe { crate::runtime_domain::interrupted_runtime_in_clocked_preload_handler() }
        .unwrap_or_else(|| refuse())
    {
        return None;
    }
    Some(unsafe { capture_at(signal, info, context, entry) })
}

fn syscall(number: i64, args: [u64; 6]) -> i64 {
    unsafe { raw_syscall6(number, args) }
}

#[track_caller]
fn refuse_native_history() -> ! {
    let message = b"owned-native: evidence budget exhausted\n";
    syscall(
        libc::SYS_write,
        [2, message.as_ptr() as u64, message.len() as u64, 0, 0, 0],
    );
    refuse()
}

#[track_caller]
pub(crate) fn refuse() -> ! {
    refuse_with("predicate", None)
}

trait RefusalError {
    fn diagnostic(&self) -> (&str, Option<i64>);
}

impl RefusalError for io::Error {
    fn diagnostic(&self) -> (&str, Option<i64>) {
        match self.raw_os_error() {
            Some(errno) => ("io-errno", Some(i64::from(errno))),
            None => ("io-error-kind", Some(self.kind() as i64)),
        }
    }
}

impl RefusalError for reverie::Errno {
    fn diagnostic(&self) -> (&str, Option<i64>) {
        ("errno", Some(i64::from(self.into_raw())))
    }
}

impl RefusalError for reverie::Error {
    fn diagnostic(&self) -> (&str, Option<i64>) {
        match self {
            Self::Errno(error) => error.diagnostic(),
            Self::Io(error) => error.diagnostic(),
            Self::Tool(error) if error.is::<reverie::UnsupportedGuestProgress>() => {
                ("guest-progress/unsupported", None)
            }
            Self::Tool(error) if error.is::<reverie::vdso::UnsupportedVdsoEvent>() => {
                ("modeled-rng/snapshot-unsupported", None)
            }
            Self::Tool(_) => ("guest-progress/tool-error", None),
        }
    }
}

impl RefusalError for crate::mapping::Failure {
    fn diagnostic(&self) -> (&str, Option<i64>) {
        (self.reason, self.result)
    }
}

impl RefusalError for reverie_preload::signal::native_frame::Error {
    fn diagnostic(&self) -> (&str, Option<i64>) {
        use reverie_preload::signal::native_frame::Error;
        let detail = match self {
            Error::Overflow => "native-frame/overflow",
            Error::SourceBounds => "native-frame/source-bounds",
            Error::DestinationBounds => "native-frame/destination-bounds",
            Error::Overlap => "native-frame/overlap",
            Error::Alignment => "native-frame/alignment",
            Error::RestorerPosition => "native-frame/restorer-position",
            Error::NullFpUnsupported => "native-frame/null-fp",
            Error::UnsupportedFormat => "native-frame/unsupported-format",
            Error::Metadata => "native-frame/metadata",
            Error::InstructionPointer => "native-frame/instruction-pointer",
            Error::Flags => "native-frame/flags",
        };
        (detail, None)
    }
}

#[track_caller]
fn refuse_error(error: impl RefusalError) -> ! {
    let (detail, value) = error.diagnostic();
    refuse_with(detail, value)
}

#[track_caller]
fn refuse_with(detail: &str, value: Option<i64>) -> ! {
    PHASE.with(|slot| slot.store(FAILED, Ordering::Relaxed));
    if let Err(result) = crate::vdso::protection::revoke_execution() {
        reverie_preload::trap::report_terminal126("vdso/revoke", "raw-result", Some(result));
    }
    reverie_preload::trap::report_terminal126("owned-context", detail, value);
    if let Some(evidence) = unsafe { EVIDENCE.with(|slot| slot.load(Ordering::Acquire)).as_ref() } {
        let mut offset = 0;
        while offset < evidence.used {
            let written = syscall(
                libc::SYS_write,
                [
                    2,
                    evidence.bytes.as_ptr() as u64 + offset as u64,
                    (evidence.used - offset) as u64,
                    0,
                    0,
                    0,
                ],
            );
            if written <= 0 {
                break;
            }
            offset += written as usize;
        }
    }
    if let Some(history) = unsafe { STATE.with(|slot| slot.load(Ordering::Acquire)).as_ref() }
        .and_then(|state| state.native.as_ref())
        .and_then(|native| native.history.as_ref())
    {
        let bytes = history.bytes();
        let mut offset = 0;
        while offset < bytes.len() {
            let written = syscall(
                libc::SYS_write,
                [
                    2,
                    bytes.as_ptr() as u64 + offset as u64,
                    (bytes.len() - offset) as u64,
                    0,
                    0,
                    0,
                ],
            );
            if written <= 0 {
                break;
            }
            offset += written as usize;
        }
    }
    syscall(libc::SYS_exit_group, [126, 0, 0, 0, 0, 0]);
    loop {
        core::hint::spin_loop();
    }
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "owned instruction native profile is not admitted",
    )
}

/// Observation of the optional POSIX timer inventory, not proof of timer absence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PosixTimerInventory {
    /// The available inventory was empty at inspection.
    Empty,
    /// The inventory file was absent; the unsafe caller contract is still required.
    Unavailable,
}

pub(crate) fn inspect_posix_timer_inventory(
    inventory: io::Result<String>,
) -> io::Result<PosixTimerInventory> {
    match inventory {
        Ok(contents) if contents.trim().is_empty() => Ok(PosixTimerInventory::Empty),
        Ok(_) => Err(unsupported()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(PosixTimerInventory::Unavailable)
        }
        Err(error) => Err(error),
    }
}

fn controls() -> Option<Controls> {
    let mut result = Controls {
        segments_and_permissions: [0; 3],
        xcr0: unsafe { core::arch::x86_64::_xgetbv(0) },
        pkru: 0,
        cpuid: syscall(libc::SYS_arch_prctl, [0x1011, 0, 0, 0, 0, 0]),
        tsc: 0,
    };
    for (operation, output) in [0x1003, 0x1004, 0x1022]
        .into_iter()
        .zip(&mut result.segments_and_permissions)
    {
        if syscall(
            libc::SYS_arch_prctl,
            [operation, output as *mut u64 as u64, 0, 0, 0, 0],
        ) != 0
        {
            return None;
        }
    }
    if syscall(
        libc::SYS_prctl,
        [
            libc::PR_GET_TSC as u64,
            (&raw mut result.tsc) as u64,
            0,
            0,
            0,
            0,
        ],
    ) != 0
    {
        return None;
    }
    unsafe {
        core::arch::asm!("rdpkru", in("ecx") 0u32, out("eax") result.pkru, out("edx") _, options(nostack));
    }
    Some(result)
}

pub(crate) fn initialize(
    subscriptions: crate::runtime::InstructionSubscriptions,
    clocked: bool,
    single_step: bool,
    precise_timer: bool,
    syscalls: bool,
    native_evidence: Option<usize>,
) -> io::Result<()> {
    if native_evidence.is_some() && !(clocked && single_step && precise_timer && syscalls) {
        return Err(unsupported());
    }
    if single_step {
        if !clocked
            || !reverie_preload::signal::owned_trace::configured()
            || !clock_setup_allowed(
                clocked,
                crate::clock_control::requested(),
                crate::clock_control::active(),
                reverie_preload::signal::runtime_signals_configured(),
                crate::clock_control::notification_free() && crate::clock_control::handoff_clear(),
            )
            || !no_pending_signals()
        {
            return Err(unsupported());
        }
    } else {
        admit_clock_setup(clocked)?;
    }
    if !STATE.with(|slot| slot.load(Ordering::Relaxed)).is_null()
        || std::fs::read_dir("/proc/self/task")?.count() != 1
        || std::fs::read_to_string("/proc/sys/kernel/osrelease")?.trim()
            != "7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf"
        || unsafe { libc::getauxval(libc::AT_MINSIGSTKSZ) } != 3376
    {
        return Err(unsupported());
    }
    let features = core::arch::x86_64::__cpuid(1);
    let extended = core::arch::x86_64::__cpuid_count(7, 0);
    let xstate = core::arch::x86_64::__cpuid_count(0xd, 0);
    if subscriptions.rdtsc
        && (features.edx & (1 << 4) == 0
            || core::arch::x86_64::__cpuid(0x8000_0000).eax < 0x8000_0001
            || core::arch::x86_64::__cpuid(0x8000_0001).edx & (1 << 27) == 0)
    {
        return Err(unsupported());
    }
    if features.ecx & (1 << 27) == 0
        || extended.ecx & 24 != 24
        || (xstate.eax, xstate.edx, xstate.ebx) != (FEATURES as u32, 0, XSTATE_BYTES)
    {
        return Err(unsupported());
    }
    let mut native = controls().ok_or_else(unsupported)?;
    let mut shadow_stack = 0u64;
    let status = syscall(
        libc::SYS_arch_prctl,
        [0x5005, (&raw mut shadow_stack) as u64, 0, 0, 0, 0],
    );
    if (status != 0 && status != -i64::from(libc::EINVAL))
        || shadow_stack != 0
        || native.xcr0 != FEATURES
        || native.segments_and_permissions[2] != FEATURES
        || native.pkru != 0x5555_5554
        || native.cpuid != 1
        || native.tsc != libc::PR_TSC_ENABLE
    {
        return Err(unsupported());
    }
    for timer in [libc::ITIMER_REAL, libc::ITIMER_VIRTUAL, libc::ITIMER_PROF] {
        let mut value: libc::itimerval = unsafe { core::mem::zeroed() };
        if syscall(
            libc::SYS_getitimer,
            [timer as u64, (&raw mut value) as u64, 0, 0, 0, 0],
        ) != 0
            || value.it_value.tv_sec != 0
            || value.it_value.tv_usec != 0
        {
            return Err(unsupported());
        }
    }
    let mut mappings = Vec::new();
    let mut readable = Vec::new();
    let mut writable = Vec::new();
    for line in std::fs::read_to_string("/proc/self/maps")?.lines() {
        let mut fields = line.split_whitespace();
        let interval = fields.next().ok_or_else(unsupported)?;
        let permissions = fields.next().ok_or_else(unsupported)?.as_bytes();
        if permissions.first() != Some(&b'r') {
            continue;
        }
        let (start, end) = interval.split_once('-').ok_or_else(unsupported)?;
        let range = u64::from_str_radix(start, 16).map_err(|_| unsupported())?
            ..u64::from_str_radix(end, 16).map_err(|_| unsupported())?;
        readable.push(range.clone());
        if native_evidence.is_some()
            && permissions.get(1) == Some(&b'w')
            && permissions.get(2) == Some(&b'x')
        {
            return Err(unsupported());
        }
        if permissions.get(1) == Some(&b'w') {
            writable.push(range.clone());
        }
        if permissions.get(2) == Some(&b'x') {
            mappings.push(range);
        }
    }
    let runtime = stack::StackAllocation::allocate(RUNTIME_BYTES)?.leak();
    let runtime_sp = runtime.end & !15;
    if subscriptions.cpuid {
        native.cpuid = 0;
    }
    if subscriptions.rdtsc {
        native.tsc = libc::PR_TSC_SIGSEGV;
    }
    let mut state = Box::new(OwnedContext {
        owner: syscall(libc::SYS_gettid, [0; 6]),
        mappings,
        readable,
        writable,
        vdso: None,
        signal_stack: 0..0,
        runtime_sp: runtime_sp - 8,
        runtime_stack: runtime,
        storage: Box::into_raw(Box::new(Storage([0; STORAGE_BYTES]))),
        image: None,
        registers: unsafe { core::mem::zeroed() },
        event: None,
        stepper: single_step.then(Stepper::default),
        precise_timer,
        syscalls,
        native: native_evidence
            .map(native_step::NativeState::new)
            .transpose()?,
        guest_mask: 0,
        subscriptions,
        clocked,
        controls: native,
        errno: unsafe { libc::__errno_location() },
        saved_errno: 0,
        generation: 0,
    });
    if syscalls && native_evidence.is_none() {
        EVIDENCE.with(|slot| {
            slot.store(
                Box::into_raw(Box::new(Evidence {
                    bytes: [0; 32 * (STORAGE_BYTES + 64)],
                    used: 0,
                })),
                Ordering::Release,
            )
        });
    }
    state.vdso = crate::vdso::activate_prepared()?;
    if let Some(vdso) = &state.vdso {
        state.readable.retain(|range| vdso.accessible(range));
        state.writable.retain(|range| vdso.accessible(range));
    }
    STATE.with(|slot| slot.store(Box::into_raw(state), Ordering::Release));
    Ok(())
}

pub(crate) fn admit_clock_setup(clocked: bool) -> io::Result<()> {
    let allowed = clock_setup_allowed(
        clocked,
        crate::clock_control::requested(),
        crate::clock_control::active(),
        reverie_preload::signal::runtime_signals_configured(),
        crate::clock_control::notification_free() && crate::clock_control::handoff_clear(),
    );
    if !allowed
        || reverie_preload::signal::owned_trace::configured()
        || (clocked && !no_pending_signals())
    {
        return Err(unsupported());
    }
    Ok(())
}

fn clock_setup_allowed(
    clocked: bool,
    requested: bool,
    active: bool,
    sources: bool,
    notification_free: bool,
) -> bool {
    requested == clocked && !active && !sources && notification_free
}

fn no_pending_signals() -> bool {
    let mut pending = 0u64;
    syscall(
        libc::SYS_rt_sigpending,
        [(&raw mut pending) as u64, 8, 0, 0, 0, 0],
    ) == 0
        && pending == 0
}

fn clock_environment_matches(state: &OwnedContext) -> bool {
    if !state.clocked {
        return !crate::clock_control::active() && !crate::clock_control::requested();
    }
    crate::clock_control::active()
        && crate::clock_control::paused()
        && !crate::clock_control::requested()
        && crate::clock_control::notification_free()
        && crate::clock_control::handoff_clear()
        && !reverie_preload::signal::runtime_signals_configured()
        && trace_policy_matches(state)
        && no_pending_signals()
}

fn trace_policy_matches(state: &OwnedContext) -> bool {
    if state.stepper.is_some() {
        reverie_preload::signal::owned_trace::installed()
    } else {
        !reverie_preload::signal::owned_trace::configured()
    }
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
/// Read the shared clock at the finite fixture's terminal runtime boundary.
///
/// # Safety
/// The owned clocked fixture has actually returned to guest code, then paused
/// through the existing assembly clock entry. No Rust or conditional runtime
/// work precedes that disable, and this terminal activation never resumes guest
/// execution. Phase alone is not proof of a completed return. All installer
/// ownership/source/lifetime requirements continue to apply.
pub unsafe fn __read_owned_clock_after_return() -> io::Result<u64> {
    let state = unsafe { STATE.with(|slot| slot.load(Ordering::Acquire)).as_ref() }
        .ok_or_else(unsupported)?;
    if PHASE.with(|slot| slot.load(Ordering::Relaxed)) != RETURNING
        || !state.clocked
        || !clock_environment_matches(state)
        || controls() != Some(state.controls)
    {
        return Err(unsupported());
    }
    crate::runtime::read_guest_rcb_clock()
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
/// Retained authentic frame bytes and terminal runtime ownership.
pub struct OwnedSyscallEvidence {
    /// Original return validation, then bounded raw records; errors retain the guard.
    pub bytes: io::Result<Vec<u8>>,
    _runtime: crate::runtime_domain::Entry,
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
/// Enter terminal diagnostics after an actual owned syscall guest return.
///
/// # Safety
/// The fixture actually reached its terminal assembly clock entry, which paused
/// the installed clock and entered the runtime domain. Whether an owned return
/// completed is still validated, not assumed from that terminal boundary. All
/// source/lifetime requirements of [`__read_owned_clock_after_return`] apply.
/// The caller keeps this guard alive until exit and never resumes guest execution. Only
/// terminal verification may execute in this runtime scope; it is not guest
/// syscall admission. The copied records contain private process addresses.
pub unsafe fn __owned_syscall_evidence() -> io::Result<OwnedSyscallEvidence> {
    let state = unsafe { STATE.with(|slot| slot.load(Ordering::Acquire)).as_ref() };
    if !crate::clock_control::active()
        || !crate::clock_control::paused()
        || !crate::clock_control::handoff_clear()
        || !crate::runtime_domain::terminal_clock_scope()
        || crate::runtime::nested_tool_callback()
        || state.is_none_or(|state| {
            !state.clocked || !state.syscalls || syscall(libc::SYS_gettid, [0; 6]) != state.owner
        })
    {
        return Err(io::ErrorKind::Unsupported.into());
    }
    Ok(collect_syscall_evidence(|| {
        unsafe { __read_owned_clock_after_return()? };
        if let Some(native) = state.and_then(|state| state.native.as_ref()) {
            return native
                .history
                .as_ref()
                .map(|history| history.bytes().to_vec())
                .ok_or_else(unsupported);
        }
        let evidence = unsafe { EVIDENCE.with(|slot| slot.load(Ordering::Acquire)).as_ref() }
            .ok_or_else(unsupported)?;
        Ok(evidence.bytes[..evidence.used].to_vec())
    }))
}

#[cfg(feature = "test-owned-cpuid")]
fn collect_syscall_evidence(collect: impl FnOnce() -> io::Result<Vec<u8>>) -> OwnedSyscallEvidence {
    let runtime = crate::runtime_domain::Entry::enter();
    OwnedSyscallEvidence {
        bytes: collect(),
        _runtime: runtime,
    }
}

pub(crate) fn arm() -> io::Result<()> {
    let state = unsafe { STATE.with(|slot| slot.load(Ordering::Acquire)).as_mut() }
        .ok_or_else(unsupported)?;
    if state.clocked
        && (!crate::clock_control::active()
            || !crate::clock_control::paused()
            || !crate::clock_control::notification_free()
            || !crate::clock_control::handoff_clear()
            || reverie_preload::signal::runtime_signals_configured()
            || !trace_policy_matches(state)
            || !no_pending_signals())
    {
        return Err(unsupported());
    }
    let mut stack: libc::stack_t = unsafe { core::mem::zeroed() };
    if syscall(
        libc::SYS_sigaltstack,
        [0, (&raw mut stack) as u64, 0, 0, 0, 0],
    ) != 0
        || stack.ss_flags != 0
        || stack.ss_size < 64 * 1024
    {
        return Err(unsupported());
    }
    let base = stack.ss_sp as usize;
    state.signal_stack = base..base.checked_add(stack.ss_size).ok_or_else(unsupported)?;
    Ok(())
}

#[unsafe(naked)]
pub(crate) unsafe extern "C" fn signal_entry(
    _: i32,
    _: *mut libc::siginfo_t,
    _: *mut libc::c_void,
) {
    core::arch::naked_asm!("jmp {clocked}", clocked = sym clocked_entry);
}

reverie_preload::clocked_signal!(clocked_entry, capture, frame_entry);

unsafe extern "C" fn capture(
    signal: i32,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
    _scope: u64,
    entry: usize,
) -> Continuation {
    unsafe { capture_at(signal, info, context, entry) }
}

unsafe fn capture_at(
    signal: i32,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
    entry: usize,
) -> Continuation {
    let phase = PHASE.with(|slot| slot.load(Ordering::Relaxed));
    let state_pointer = STATE.with(|slot| slot.load(Ordering::Acquire));
    if !matches!(phase, IDLE | RETURNING)
        || state_pointer.is_null()
        || !matches!(signal, libc::SIGSEGV | libc::SIGTRAP | libc::SIGSYS)
        || info.is_null()
        || context.is_null()
    {
        refuse();
    }
    let state = unsafe { &mut *state_pointer };
    let trace = signal == libc::SIGTRAP;
    let sud = signal == libc::SIGSYS;
    if sud && !state.syscalls {
        refuse();
    }
    if trace && (phase != RETURNING || state.stepper.is_none()) {
        refuse();
    }
    if !capture_identity_matches(
        state,
        syscall(libc::SYS_gettid, [0; 6]),
        entry,
        info,
        context,
    ) {
        refuse();
    }
    let saved_errno = unsafe { *state.errno };
    #[cfg(test)]
    if let Some(continuation) = ownership_tests::observe_capture(state, entry, saved_errno) {
        return continuation;
    }
    let vdso_execution = crate::vdso::protection::revoke_execution()
        .unwrap_or_else(|result| refuse_with("vdso/revoke", Some(result)));
    if let Some(native) = &mut state.native {
        native.seal.retire_verified().unwrap_or_else(|| refuse());
        if let Some(history) = &mut native.history {
            let length = (state.signal_stack.end - entry).min(STORAGE_BYTES);
            let header = [
                0x5355444652414d45u64,
                signal as u64,
                unsafe { (*info).si_code } as u64,
                entry as u64,
                context as u64,
                info as u64,
                length as u64,
                state.generation,
            ];
            let source = unsafe { core::slice::from_raw_parts(entry as *const u8, length) };
            history
                .append(header, source)
                .unwrap_or_else(|| refuse_native_history());
        }
    } else if state.syscalls {
        let evidence = unsafe { &mut *EVIDENCE.with(|slot| slot.load(Ordering::Acquire)) };
        let length = (state.signal_stack.end - entry).min(STORAGE_BYTES);
        let header = [
            0x5355444652414d45u64,
            signal as u64,
            unsafe { (*info).si_code } as u64,
            entry as u64,
            context as u64,
            info as u64,
            length as u64,
            state.generation,
        ];
        if evidence.used + 64 + length > evidence.bytes.len() {
            refuse();
        }
        unsafe {
            ptr::copy_nonoverlapping(
                header.as_ptr().cast::<u8>(),
                evidence.bytes.as_mut_ptr().add(evidence.used),
                64,
            );
            ptr::copy_nonoverlapping(
                entry as *const u8,
                evidence.bytes.as_mut_ptr().add(evidence.used + 64),
                length,
            );
        }
        evidence.used += 64 + length;
    }
    let original = unsafe { &mut *context.cast::<libc::ucontext_t>() };
    let registers = &original.uc_mcontext.gregs;
    let pc = registers[libc::REG_RIP as usize] as u64;
    let fault_address = if signal == libc::SIGSEGV {
        (unsafe { (*info).si_addr() }) as u64
    } else {
        0
    };
    let vdso_concern = signal == libc::SIGSEGV
        && state
            .vdso
            .as_ref()
            .is_some_and(|vdso| vdso.concerns(pc, fault_address));
    let data_fault_clock =
        if vdso_execution && vdso_concern && vdso_data_fault(unsafe { (*info).si_code }, registers)
        {
            let clock =
                crate::runtime::read_guest_rcb_clock().unwrap_or_else(|error| refuse_error(error));
            state
                .stepper
                .as_ref()
                .and_then(|stepper| {
                    stepper.validate_cpu_fault(state.owner, state.generation, registers, clock)
                })
                .map(|()| clock)
        } else {
            None
        };
    let vdso_fault = vdso_concern
        && (data_fault_clock.is_some()
            || !(unsafe { (*info).si_code } == libc::SI_KERNEL
                && registers[libc::REG_TRAPNO as usize] == 13
                && registers[libc::REG_ERR as usize] == 0));
    if !clock_environment_matches(state)
        || unsafe { (*info).si_code }
            != if trace {
                libc::TRAP_TRACE
            } else if sud {
                2
            } else if data_fault_clock.is_some() {
                unsafe { (*info).si_code }
            } else if vdso_fault {
                crate::vdso::EXECUTE_ACCESS_ERROR
            } else {
                libc::SI_KERNEL
            }
        || controls() != Some(state.controls)
    {
        refuse();
    }
    let sp = registers[libc::REG_RSP as usize] as u64;
    let flags = registers[libc::REG_EFL as usize] as u64;
    let syscall_event = if sud {
        let site = pc.checked_sub(2).unwrap_or_else(|| refuse());
        let mapping = execution_ranges(state, site)
            .iter()
            .find(|range| range.contains(&site) && range.contains(&pc))
            .unwrap_or_else(|| refuse());
        let metadata = unsafe { ptr::read(info.cast::<crate::syscall_event::Metadata>()) };
        let bytes = unsafe { core::slice::from_raw_parts(site as *const u8, 2) };
        Some(
            crate::syscall_event::SyscallEvent::admit(metadata, registers, bytes, mapping)
                .unwrap_or_else(|| refuse()),
        )
    } else {
        None
    };
    if trace && unsafe { (*info).si_addr() } as u64 != pc {
        refuse();
    }
    let interrupted_step = !trace
        && phase == RETURNING
        && state.precise_timer
        && state
            .stepper
            .as_ref()
            .and_then(|stepper| {
                if let Some(event) = syscall_event {
                    stepper.validate_syscall(state.owner, state.generation, event.number, registers)
                } else if vdso_fault {
                    stepper.validate_call(state.owner, state.generation, registers)
                } else {
                    stepper
                        .validate_fault(state.owner, state.generation, registers)
                        .map(|_| ())
                }
            })
            .is_some();
    if (!sud
        && !vdso_fault
        && (registers[libc::REG_TRAPNO as usize] != if trace { 1 } else { 13 }
            || registers[libc::REG_ERR as usize] != 0))
        || (sud && !interrupted_step && registers[libc::REG_R11 as usize] as u64 != flags)
        || registers[libc::REG_CSGSFS as usize] as u64 != 0x002b_0000_0000_0033
        || flags
            & !(0x10cd5
                | 0x202
                | if trace || interrupted_step || data_fault_clock.is_some() {
                    0x100
                } else {
                    0
                })
            != 0
        || flags & 0x202 != 0x202
        || original.uc_stack.ss_sp as usize != state.signal_stack.start
        || original.uc_stack.ss_size != state.signal_stack.len()
        || original.uc_stack.ss_flags != 0
        || pc.checked_add(2).is_none_or(|next| next >= 1 << 47)
        || sp >= 1 << 47
        || !state.writable.iter().any(|range| {
            sp.checked_sub(128)
                .is_some_and(|bottom| range.contains(&bottom))
                && sp.checked_add(8).is_some_and(|top| range.contains(&top))
        })
    {
        refuse();
    }
    let event = if trace {
        let clock =
            crate::runtime::read_guest_rcb_clock().unwrap_or_else(|error| refuse_error(error));
        let stepper = state.stepper.as_mut().unwrap_or_else(|| refuse());
        let completion = stepper
            .capture(state.owner, state.generation, registers, clock)
            .unwrap_or_else(|| {
                stepper.report_capture_refusal(
                    state.owner,
                    state.generation,
                    registers,
                    clock,
                    |stage, detail, value| {
                        reverie_preload::trap::report_terminal126(stage, detail, value);
                    },
                );
                refuse()
            });
        CapturedEvent::Step(completion)
    } else if let Some(clock) = data_fault_clock {
        let modeled = state.stepper.as_mut().and_then(|stepper| {
            stepper.capture_read_fault(
                state.owner,
                state.generation,
                crate::vdso::Fault {
                    signal,
                    code: unsafe { (*info).si_code },
                    address: fault_address,
                    registers,
                },
                clock,
            )
        });
        CapturedEvent::VdsoFault(VdsoFault {
            signal,
            code: unsafe { (*info).si_code },
            address: fault_address,
            trap: registers[libc::REG_TRAPNO as usize],
            error: registers[libc::REG_ERR as usize],
            clock,
            modeled,
        })
    } else {
        if let Some(stepper) = &mut state.stepper
            && stepper.pending()
            && !interrupted_step
        {
            stepper.cancel();
            refuse();
        }
        if let Some(event) = syscall_event {
            CapturedEvent::Syscall(event, interrupted_step)
        } else if vdso_fault {
            let fault = crate::vdso::Fault {
                signal,
                code: unsafe { (*info).si_code },
                address: fault_address,
                registers,
            };
            let vdso = state.vdso.as_ref().unwrap_or_else(|| refuse());
            if state.native.is_some() && vdso.native_entry(&fault) {
                CapturedEvent::VdsoEntry
            } else {
                if !crate::vdso::covered(&state.readable, sp, 8) {
                    refuse();
                }
                let target = unsafe { ptr::read_unaligned(sp as *const u64) };
                let call = admit_vdso_call(state, fault, target).unwrap_or_else(|| refuse());
                CapturedEvent::Vdso(call, interrupted_step)
            }
        } else {
            let mapping = execution_ranges(state, pc)
                .iter()
                .find(|range| {
                    range.contains(&pc)
                        && pc.checked_add(2).is_some_and(|next| range.contains(&next))
                })
                .unwrap_or_else(|| refuse());
            let bytes = unsafe { core::slice::from_raw_parts(pc as *const u8, 3) };
            let instruction =
                InstructionEvent::admit(pc, bytes, mapping).unwrap_or_else(|| refuse());
            if !match instruction.kind {
                Kind::Cpuid => state.subscriptions.cpuid,
                Kind::Rdtsc | Kind::Rdtscp => state.subscriptions.rdtsc,
            } {
                refuse();
            }
            CapturedEvent::Instruction(instruction, interrupted_step)
        }
    };
    let guest_mask = unsafe { ptr::read((entry + 304) as *const u64) };
    state.guest_mask = guest_mask;
    if guest_mask
        & ((1 << (libc::SIGSYS - 1))
            | (1 << (libc::SIGSEGV - 1))
            | if state.stepper.is_some() {
                1 << (libc::SIGTRAP - 1)
            } else {
                0
            })
        != 0
    {
        refuse();
    }
    state.generation = state.generation.checked_add(1).unwrap_or_else(|| refuse());
    {
        let _retired_image = state.image.take();
    }
    let destination = unsafe { &mut (*state.storage).0 };
    let source =
        unsafe { core::slice::from_raw_parts(entry as *const u8, state.signal_stack.end - entry) };
    if (trace || data_fault_clock.is_some())
        && let Some(native) = &mut state.native
    {
        native
            .seal
            .capture(source, entry, state.generation)
            .unwrap_or_else(|| refuse());
    }
    let image = relocate(
        source,
        entry,
        entry + 8,
        Format::StandardXsave {
            xfeatures: FEATURES,
            xstate_size: XSTATE_BYTES,
        },
        destination,
    )
    .unwrap_or_else(|error| refuse_error(error));
    if (trace || data_fault_clock.is_some())
        && state
            .native
            .as_ref()
            .is_some_and(|native| !native.seal.relocated(&image, state.generation))
    {
        refuse();
    }
    state.registers = snapshot(registers);
    #[cfg(feature = "private-crt")]
    let event = if let Some(target) = unsafe {
        crate::private_startup::capture(signal, &state.registers, image.fp_bytes(), guest_mask)
    } {
        if state.generation != 1 || !state.mappings.iter().any(|range| range.contains(&target)) {
            refuse();
        }
        CapturedEvent::Initial(target)
    } else {
        event
    };
    state.event = Some(event);
    state.image = Some(image);
    state.saved_errno = saved_errno;
    original.uc_mcontext.gregs[libc::REG_RIP as usize] = ordinary_entry as *const () as i64;
    original.uc_mcontext.gregs[libc::REG_RSP as usize] = state.runtime_sp as i64;
    original.uc_mcontext.gregs[libc::REG_EFL as usize] &= !(0x400
        | if trace || interrupted_step || data_fault_clock.is_some() {
            0x100
        } else {
            0
        });
    unsafe {
        ptr::write(
            (entry + 304) as *mut u64,
            crate::syscall_fallback::runtime_mask(),
        )
    };
    PHASE.with(|slot| slot.store(CAPTURED, Ordering::Relaxed));
    unsafe { *state.errno = saved_errno };
    Continuation::hook(ordinary_entry as *const () as u64)
}

fn snapshot(registers: &[libc::greg_t; 23]) -> HookContext {
    let mut context: HookContext = unsafe { core::mem::zeroed() };
    context.instruction_pointer = registers[libc::REG_RIP as usize] as u64;
    context.stack_pointer = registers[libc::REG_RSP as usize] as u64;
    context.rflags = registers[libc::REG_EFL as usize] as u64;
    context.rax = registers[libc::REG_RAX as usize] as u64;
    context.rbx = registers[libc::REG_RBX as usize] as u64;
    context.rcx = registers[libc::REG_RCX as usize] as u64;
    context.rdx = registers[libc::REG_RDX as usize] as u64;
    context.rsi = registers[libc::REG_RSI as usize] as u64;
    context.rdi = registers[libc::REG_RDI as usize] as u64;
    context.rbp = registers[libc::REG_RBP as usize] as u64;
    context.r8 = registers[libc::REG_R8 as usize] as u64;
    context.r9 = registers[libc::REG_R9 as usize] as u64;
    context.r10 = registers[libc::REG_R10 as usize] as u64;
    context.r11 = registers[libc::REG_R11 as usize] as u64;
    context.r12 = registers[libc::REG_R12 as usize] as u64;
    context.r13 = registers[libc::REG_R13 as usize] as u64;
    context.r14 = registers[libc::REG_R14 as usize] as u64;
    context.r15 = registers[libc::REG_R15 as usize] as u64;
    context
}

#[cfg(feature = "test-owned-cpuid")]
#[doc(hidden)]
/// Inspect completed primitive observations at the terminal paused boundary.
///
/// # Safety
/// The caller must meet [`__read_owned_clock_after_return`]'s actual-return and
/// terminal-boundary requirements; phase alone does not prove a native return.
pub unsafe fn __owned_step_observation() -> io::Result<(u64, u64, u64)> {
    unsafe { __read_owned_clock_after_return()? };
    let state = unsafe { &*STATE.with(|slot| slot.load(Ordering::Acquire)) };
    let stepper = state.stepper.as_ref().ok_or_else(unsupported)?;
    let last = stepper.last.ok_or_else(unsupported)?;
    if stepper.pending() {
        return Err(unsupported());
    }
    Ok((stepper.completed, last.pc, last.clock))
}

fn capture_identity_matches(
    state: &OwnedContext,
    tid: i64,
    entry: usize,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) -> bool {
    tid == state.owner
        && entry >= state.signal_stack.start + 32 * 1024
        && entry
            .checked_add(440)
            .is_some_and(|end| end <= state.signal_stack.end)
        && context as usize == entry + 8
        && info as usize == entry + 312
}

fn enter_runtime() -> bool {
    PHASE.with(|slot| slot.swap(RUNTIME, Ordering::Relaxed)) == CAPTURED
}

unsafe extern "C" fn dispatch() -> usize {
    if !enter_runtime() {
        refuse();
    }
    let state = unsafe { &mut *STATE.with(|slot| slot.load(Ordering::Acquire)) };
    if !environment_matches(state) {
        refuse();
    }
    let event = state.event.take().unwrap_or_else(|| refuse());
    let mut image = state.image.take().unwrap_or_else(|| refuse());
    let mapping_pc = match &event {
        #[cfg(feature = "private-crt")]
        CapturedEvent::Initial(target) => *target,
        CapturedEvent::Vdso(call, _) => call.target,
        _ => state.registers.instruction_pointer,
    };
    let _mapping_dispatch = crate::mapping::enter(mapping_pc, state.registers.stack_pointer)
        .unwrap_or_else(|error| refuse_error(error));
    image = match event {
        CapturedEvent::VdsoEntry => {
            state
                .native
                .as_mut()
                .unwrap_or_else(|| refuse())
                .fault_resume = Some(state.registers.instruction_pointer);
            image
        }
        CapturedEvent::VdsoFault(fault) => {
            if let Some(captured) = fault.modeled
                && admit_rng_read(state, captured).is_ok()
            {
                let (completed, resolution) = run_rng_transition(
                    state,
                    image,
                    captured,
                    crate::tool_host::guest_progress,
                    |owner| unsafe { crate::runtime::dispatch_owned_rng_snapshot(owner) },
                )
                .unwrap_or_else(|error| refuse_error(error));
                if let crate::timer::InterruptionResolution::Preserved(Some(position)) = resolution
                {
                    deliver_timer(state, position);
                }
                completed
            } else {
                state.image = Some(image);
                state.event = Some(CapturedEvent::VdsoFault(fault));
                state.stepper.as_mut().unwrap_or_else(|| refuse()).cancel();
                fault.report_with(&state.registers, |stage, detail, value| {
                    reverie_preload::trap::report_terminal126(stage, detail, value);
                });
                if let Some(vdso) = &state.vdso {
                    vdso.report_data_fault_with(
                        state.registers.instruction_pointer,
                        fault.address,
                        |stage, detail, value| {
                            reverie_preload::trap::report_terminal126(stage, detail, value);
                        },
                    );
                } else {
                    reverie_preload::trap::report_terminal126(
                        "vdso/retained-image",
                        "unavailable",
                        None,
                    );
                }
                if state.precise_timer {
                    crate::timer::cancel_owned().unwrap_or_else(|error| refuse_error(error));
                }
                refuse_with("vdso/data-fault-signal-delivery-unsupported", None)
            }
        }
        #[cfg(feature = "private-crt")]
        CapturedEvent::Initial(target) => {
            let fault_pc = state.registers.instruction_pointer;
            let flags = state.registers.rflags;
            if crate::runtime::read_guest_rcb_clock().unwrap_or_else(|error| refuse_error(error))
                != 0
            {
                refuse();
            }
            state.registers.instruction_pointer = target;
            state.registers.rflags &= !0x10000;
            state
                .native
                .as_mut()
                .unwrap_or_else(|| refuse())
                .fault_resume = None;
            open_timer_window(state, target);
            unsafe { crate::runtime::dispatch_owned_initial(&mut state.registers) };
            crate::timer::close_window().unwrap_or_else(|error| refuse_error(error));
            if state.registers.instruction_pointer != target
                || state.registers.rflags != flags & !0x10000
            {
                refuse();
            }
            image
                .complete_private_start(fault_pc, flags, target)
                .unwrap_or_else(|error| refuse_error(error))
        }
        CapturedEvent::Vdso(call, interrupted_step) => {
            if let Some(native) = &mut state.native {
                native.fault_resume = None;
            }
            let flags = state.registers.rflags;
            let original_registers = crate::vdso::register_values(&state.registers);
            let clock =
                crate::runtime::read_guest_rcb_clock().unwrap_or_else(|error| refuse_error(error));
            if interrupted_step {
                state
                    .stepper
                    .as_mut()
                    .unwrap_or_else(|| refuse())
                    .cancel_call(clock)
                    .unwrap_or_else(|| refuse());
            }
            if !state
                .vdso
                .as_ref()
                .unwrap_or_else(|| refuse())
                .call_buffers_supported(call, call.operation.arguments(&state.registers))
            {
                refuse();
            }
            crate::timer::cancel_owned().unwrap_or_else(|error| refuse_error(error));
            open_timer_window(state, call.target);
            let mut guest_mask = state.guest_mask;
            let result = unsafe {
                reverie_preload::user_dispatch::with_ordinary_dispatch_mask(
                    &mut guest_mask,
                    crate::syscall_fallback::runtime_mask(),
                    || crate::runtime::dispatch_owned_vdso(&mut state.registers, call.operation),
                )
            };
            crate::timer::close_window().unwrap_or_else(|error| refuse_error(error));
            if guest_mask != state.guest_mask
                || crate::vdso::register_values(&state.registers) != original_registers
            {
                refuse();
            }
            state.registers.instruction_pointer = call.target;
            state.registers.stack_pointer = call.stack + 8;
            state.registers.rax = result as u64;
            state.registers.rflags = flags & !0x10100;
            image
                .complete_vdso_call(reverie_preload::signal::native_frame::FunctionReturn {
                    entry: call.entry,
                    stack: call.stack,
                    target: call.target,
                    flags,
                    result,
                    owned_tf: interrupted_step,
                })
                .unwrap_or_else(|error| refuse_error(error))
        }
        CapturedEvent::Syscall(event, interrupted_step) => {
            if let Some(native) = &mut state.native {
                native.fault_resume = None;
            }
            let clock =
                crate::runtime::read_guest_rcb_clock().unwrap_or_else(|error| refuse_error(error));
            if interrupted_step {
                state
                    .stepper
                    .as_mut()
                    .unwrap_or_else(|| refuse())
                    .cancel_syscall(clock)
                    .unwrap_or_else(|| refuse());
                image = image
                    .owned_syscall_tf(event.resume, state.registers.rflags, state.registers.r11)
                    .unwrap_or_else(|error| refuse_error(error));
                state.registers.rflags &= !0x100;
                state.registers.r11 &= !0x100;
            }
            crate::timer::cancel_owned().unwrap_or_else(|error| refuse_error(error));
            open_timer_window(state, event.resume);
            state.registers.instruction_pointer = event.site;
            state.registers.rax = event.number as u64;
            let mut guest_mask = state.guest_mask;
            let binding = exit::Binding {
                owner: state.owner,
                generation: state.generation,
                number: event.number,
                site: event.site,
            };
            let result = unsafe {
                if DISPATCH_CAPTURE
                    .replace(Some((state.generation, event.number, event.site)))
                    .is_some()
                {
                    refuse();
                }
                let _capture = SyscallDispatchCapture;
                reverie_preload::user_dispatch::with_ordinary_dispatch_mask(
                    &mut guest_mask,
                    crate::syscall_fallback::runtime_mask(),
                    || {
                        crate::runtime::dispatch_owned_syscall(
                            &mut state.registers,
                            event.number,
                            binding,
                        )
                    },
                )
            };
            crate::timer::close_window().unwrap_or_else(|error| refuse_error(error));
            if guest_mask != state.guest_mask {
                refuse();
            }
            let result = match result {
                exit::Outcome::Returned(result) => result,
                exit::Outcome::Exit(request) => {
                    let stack: usize;
                    unsafe {
                        core::arch::asm!("mov {}, rsp", out(reg) stack, options(nomem, nostack, preserves_flags))
                    };
                    if !request.matches(binding)
                        || !state.syscalls
                        || !state.clocked
                        || !crate::clock_control::active()
                        || !crate::clock_control::paused()
                        || !crate::clock_control::handoff_clear()
                        || !crate::runtime_domain::allocation_active()
                        || crate::runtime::nested_tool_callback()
                        || DISPATCH_CAPTURE.get().is_some()
                        || syscall(libc::SYS_gettid, [0; 6]) != state.owner
                        || !state.runtime_stack.contains(&stack)
                        || !environment_matches(state)
                    {
                        exit::failed("owned-exit/continuation", None);
                    }
                    crate::timer::cancel_owned().unwrap_or_else(|error| {
                        exit::failed(
                            "owned-exit/timer-cancel",
                            Some(-i64::from(error.into_raw())),
                        )
                    });
                    if PHASE
                        .with(|slot| {
                            slot.compare_exchange(
                                RUNTIME,
                                TERMINATING,
                                Ordering::Relaxed,
                                Ordering::Relaxed,
                            )
                        })
                        .is_err()
                    {
                        exit::failed("owned-exit/phase", None);
                    }
                    drop(_mapping_dispatch);
                    request.terminate()
                }
            };
            state.registers.instruction_pointer = event.resume;
            state.registers.rax = result as u64;
            image
                .complete_syscall(event.resume, result)
                .unwrap_or_else(|error| refuse_error(error))
        }
        CapturedEvent::Instruction(instruction, interrupted_step) => {
            if let Some(native) = &mut state.native {
                native.fault_resume =
                    (state.registers.rflags & 0x10000 != 0).then_some(instruction.resume_pc);
            }
            if state.precise_timer {
                crate::timer::cancel_owned().unwrap_or_else(|error| refuse_error(error));
                if interrupted_step {
                    let clock = crate::runtime::read_guest_rcb_clock()
                        .unwrap_or_else(|error| refuse_error(error));
                    state
                        .stepper
                        .as_mut()
                        .unwrap_or_else(|| refuse())
                        .cancel_fault(clock)
                        .unwrap_or_else(|| refuse());
                    image = image
                        .owned_single_step(instruction.fault_pc, state.registers.rflags, false)
                        .unwrap_or_else(|error| refuse_error(error));
                    state.registers.rflags &= !0x100;
                }
                open_timer_window(state, instruction.resume_pc);
            }
            let result = unsafe {
                crate::runtime::dispatch_owned_instruction(instruction.kind, &mut state.registers)
            };
            if state.precise_timer {
                crate::timer::close_window().unwrap_or_else(|error| refuse_error(error));
            }
            if !environment_matches(state) {
                refuse();
            }
            let image = complete_owned_emulation(image, &mut state.registers, instruction, result)
                .unwrap_or_else(|error| refuse_error(error));
            if let Some(native) = &mut state.native {
                native.fault_resume = None;
            }
            if state.stepper.is_some() && !state.precise_timer {
                let pc = instruction.resume_pc;
                if !state.mappings.iter().any(|range| {
                    range.contains(&pc)
                        && pc.checked_add(1).is_some_and(|next| range.contains(&next))
                }) || unsafe { ptr::read(pc as *const u8) } != 0x90
                {
                    refuse();
                }
                let clock = crate::runtime::read_guest_rcb_clock()
                    .unwrap_or_else(|error| refuse_error(error));
                state
                    .stepper
                    .as_mut()
                    .unwrap_or_else(|| refuse())
                    .stage_nop(
                        state.owner,
                        state.generation,
                        pc,
                        state.registers.rflags,
                        clock,
                    )
                    .unwrap_or_else(|| refuse());
                image
                    .owned_single_step(pc, state.registers.rflags, true)
                    .unwrap_or_else(|error| refuse_error(error))
            } else {
                image
            }
        }
        CapturedEvent::Step(completion) => {
            let clock =
                crate::runtime::read_guest_rcb_clock().unwrap_or_else(|error| refuse_error(error));
            let image = image
                .owned_single_step(completion.pc, completion.flags, false)
                .unwrap_or_else(|error| refuse_error(error));
            state.registers.rflags &= !0x100;
            let stepper = state.stepper.as_mut().unwrap_or_else(|| refuse());
            let observation = if completion.is_cpu() {
                stepper
                    .complete_cpu(completion, clock)
                    .unwrap_or_else(|| refuse())
            } else if state.precise_timer {
                Some(
                    stepper
                        .complete_precise(completion, clock)
                        .unwrap_or_else(|| refuse()),
                )
            } else {
                stepper
                    .complete(completion, clock)
                    .unwrap_or_else(|| refuse());
                None
            };
            if let Some(observation) = observation {
                if let Some(history) = state
                    .native
                    .as_mut()
                    .and_then(|native| native.history.as_mut())
                {
                    history
                        .append(
                            [
                                0x4e41544956455354,
                                observation.generation.value(),
                                observation.sequence,
                                clock,
                                observation.rip,
                                state.generation,
                                stepper.completed,
                                0,
                            ],
                            &[],
                        )
                        .unwrap_or_else(|| refuse_native_history());
                }
                if let Some(position) =
                    crate::timer::advance(observation).unwrap_or_else(|error| refuse_error(error))
                {
                    deliver_timer(state, position);
                }
            }
            image
        }
    };
    refresh_mapping_views(state);
    if state.precise_timer
        || state
            .vdso
            .as_ref()
            .is_some_and(|vdso| vdso.native_pc(state.registers.instruction_pointer))
    {
        let clock =
            crate::runtime::read_guest_rcb_clock().unwrap_or_else(|error| refuse_error(error));
        if state.precise_timer {
            while let Some(position) =
                crate::timer::immediate(state.registers.instruction_pointer, clock)
                    .unwrap_or_else(|error| refuse_error(error))
            {
                deliver_timer(state, position);
            }
        }
        let ticket = if state.precise_timer {
            crate::timer::next_step().unwrap_or_else(|error| refuse_error(error))
        } else {
            None
        };
        let pc = state.registers.instruction_pointer;
        let prepared = prepare_next_step(state, ticket, clock).unwrap_or_else(|failure| {
            if let StepPreparationFailure::Decode(failure) = failure {
                failure.report_with(|stage, detail, value| {
                    reverie_preload::trap::report_terminal126(stage, detail, value);
                });
                failure.report_exclusion_with(state, |stage, detail, value| {
                    reverie_preload::trap::report_terminal126(stage, detail, value);
                });
            }
            refuse()
        });
        if prepared {
            if let Some(native) = &mut state.native {
                native.fault_resume = None;
            }
            if state.stepper.as_ref().is_some_and(Stepper::pending) {
                image = image
                    .owned_single_step(pc, state.registers.rflags, true)
                    .unwrap_or_else(|error| refuse_error(error));
            }
        }
    }
    if let Some(native) = &mut state.native {
        let tf = state.stepper.as_ref().is_some_and(Stepper::pending);
        native
            .seal
            .finish(&image, &state.registers, state.generation, tf)
            .unwrap_or_else(|| refuse());
    }
    state.image = Some(image);
    if !environment_matches(state) {
        refuse();
    }
    if let Some(vdso) = &state.vdso
        && vdso.native_pc(state.registers.instruction_pointer)
    {
        vdso.enable_execution(state.registers.instruction_pointer)
            .unwrap_or_else(|result| refuse_with("vdso/enable", Some(result)));
    }
    let restorer_sp = state.storage as usize + 16;
    PHASE.with(|slot| slot.store(RETURNING, Ordering::Relaxed));
    unsafe { *state.errno = state.saved_errno };
    restorer_sp
}

enum StepPreparationFailure {
    Decode(StepRefusal),
    Predicate,
}

fn admit_rng_read(
    state: &OwnedContext,
    captured: crate::owned_step::ReadFaultCapture,
) -> Result<crate::vdso::rng::RngReadRequest, crate::vdso::rng::Refusal> {
    use crate::vdso::rng::Refusal;
    state
        .stepper
        .as_ref()
        .and_then(|stepper| stepper.validate_modeled(captured, state.owner, state.generation))
        .ok_or(Refusal::Owner)?;
    if !captured.matches_context(&state.registers) {
        return Err(Refusal::State);
    }
    let cpu = captured.request();
    let mut original = [0; 23];
    for (register, value) in original.iter_mut().zip(cpu.input_registers()) {
        *register = *value as i64;
    }
    original[libc::REG_RIP as usize] = cpu.pc as i64;
    original[libc::REG_EFL as usize] = cpu.input_flags() as i64;
    let owner = state.vdso.as_ref().ok_or(Refusal::Owner)?;
    let request = owner
        .rng_binding()?
        .decode(captured.identity(), cpu, &snapshot(&original))?;
    if request.address() != captured.address() {
        return Err(Refusal::Operand);
    }
    Ok(request)
}

fn modeled_operation(
    request: &crate::vdso::rng::RngReadRequest,
    value: reverie::vdso::VdsoRngSnapshot,
) -> Option<(
    reverie_preload::signal::native_frame::ModeledReadOperation,
    reverie_preload::signal::native_frame::ModeledReadValue,
)> {
    use reverie_preload::signal::native_frame::ModeledReadOperation;
    use reverie_preload::signal::native_frame::ModeledReadValue;
    match (
        request.code(),
        request.field(),
        request.register(),
        request.immediate(),
    ) {
        (iced_x86::Code::Cmp_rm8_imm8, crate::vdso::rng::RngField::Ready, None, Some(0)) => Some((
            ModeledReadOperation::CompareReadyWithZero,
            ModeledReadValue::Ready(u8::from(value.ready)),
        )),
        (
            iced_x86::Code::Mov_r64_rm64,
            crate::vdso::rng::RngField::Generation,
            Some(iced_x86::Register::RCX),
            None,
        ) => Some((
            ModeledReadOperation::LoadGenerationToRcx,
            ModeledReadValue::Generation(value.generation),
        )),
        (
            iced_x86::Code::Cmp_r64_rm64,
            crate::vdso::rng::RngField::Generation,
            Some(iced_x86::Register::RAX),
            None,
        ) => Some((
            ModeledReadOperation::CompareRaxWithGeneration,
            ModeledReadValue::Generation(value.generation),
        )),
        _ => None,
    }
}

pub(crate) struct GuestProgressCapture<'state> {
    stepper: &'state Stepper,
    captured: crate::owned_step::ReadFaultCapture,
    owner: i64,
    generation: u64,
}

impl GuestProgressCapture<'_> {
    pub(crate) fn authenticate(self, context: &HookContext) -> io::Result<(i64, u64)> {
        if self.owner != syscall(libc::SYS_gettid, [0; 6])
            || self
                .stepper
                .validate_modeled(self.captured, self.owner, self.generation)
                .is_none()
            || !self.captured.matches_context(context)
        {
            return Err(io::Error::other("guest progress retained capture mismatch"));
        }
        Ok((self.owner, self.captured.clock()))
    }
}

struct ModeledTransition {
    timer: Option<crate::timer::ModeledInterruption>,
}

impl Drop for ModeledTransition {
    fn drop(&mut self) {
        if let Some(timer) = &self.timer {
            let _ = crate::timer::close_window();
            let _ = crate::timer::fail_modeled_interruption(timer);
        }
    }
}

fn run_rng_transition<'frame>(
    state: &mut OwnedContext,
    image: RelocatedFrame<'frame>,
    captured: crate::owned_step::ReadFaultCapture,
    progress: impl FnOnce(i64, &mut HookContext, u64) -> Result<(), reverie::Error>,
    snapshot: impl FnOnce(i64) -> Result<reverie::vdso::VdsoRngSnapshot, reverie::Error>,
) -> Result<(RelocatedFrame<'frame>, crate::timer::InterruptionResolution), reverie::Error> {
    admit_rng_read(state, captured)
        .map_err(|error| io::Error::other(format!("modeled input qualification: {error:?}")))?;
    let mut transaction = ModeledTransition {
        timer: Some(crate::timer::begin_modeled_interruption(
            captured.ticket(),
            state.registers.instruction_pointer,
            captured.clock(),
        )?),
    };
    crate::timer::open_window(state.registers.instruction_pointer, captured.clock())?;
    let owner = state.owner;
    let generation = state.generation;
    let provenance = GuestProgressCapture {
        stepper: state.stepper.as_ref().ok_or_else(unsupported)?,
        captured,
        owner,
        generation,
    };
    let result = unsafe {
        crate::runtime::dispatch_owned_guest_progress(provenance, &mut state.registers, progress)
    };
    crate::timer::close_window()?;
    result?;
    refresh_mapping_views(state);
    if state.owner != owner || state.generation != generation {
        return Err(io::Error::other("modeled input ownership changed during progress").into());
    }
    admit_rng_read(state, captured).map_err(|error| {
        io::Error::other(format!(
            "modeled input post-progress qualification: {error:?}"
        ))
    })?;
    let value = snapshot(owner)?;
    let (image, observation) = complete_rng_read(state, image, captured, value)
        .map_err(|error| io::Error::other(format!("modeled input frame completion: {error:?}")))?;
    let resolution = crate::timer::resolve_modeled_interruption(
        transaction
            .timer
            .as_ref()
            .ok_or_else(|| io::Error::other("modeled input transaction missing"))?,
        observation,
    )?;
    transaction.timer = None;
    Ok((image, resolution))
}

fn complete_rng_read<'frame>(
    state: &mut OwnedContext,
    image: RelocatedFrame<'frame>,
    captured: crate::owned_step::ReadFaultCapture,
    value: reverie::vdso::VdsoRngSnapshot,
) -> Result<
    (
        RelocatedFrame<'frame>,
        Option<reverie_preload::precise_timer::Observation>,
    ),
    reverie_preload::signal::native_frame::Error,
> {
    use reverie_preload::signal::native_frame::Error;
    use reverie_preload::signal::native_frame::ModeledReadContext;
    let request = admit_rng_read(state, captured).map_err(|_| Error::Metadata)?;
    let (operation, value) = modeled_operation(&request, value).ok_or(Error::Metadata)?;
    let image = image.owned_single_step(request.cpu_request().pc, captured.flags(), false)?;
    let expected = ModeledReadContext {
        pc: request.cpu_request().pc,
        general: crate::owned_step::native::general(&state.registers),
        flags: captured.flags() & !0x100,
    };
    let image = state
        .native
        .as_mut()
        .ok_or(Error::Metadata)?
        .seal
        .complete_modeled(image, expected, operation, value, state.generation)?;
    let mut registers = [0; 23];
    for (register, bytes) in registers
        .iter_mut()
        .zip(image.frame_bytes()[48..232].as_chunks::<8>().0)
    {
        *register = i64::from_le_bytes(*bytes);
    }
    let next = snapshot(&registers);
    if next.instruction_pointer != request.next_pc() {
        return Err(Error::InstructionPointer);
    }
    let observation = state
        .stepper
        .as_mut()
        .ok_or(Error::Metadata)?
        .complete_modeled(
            captured,
            state.owner,
            state.generation,
            next.instruction_pointer,
        )
        .ok_or(Error::Metadata)?;
    state.registers = next;
    state.native.as_mut().ok_or(Error::Metadata)?.fault_resume = None;
    Ok((image, observation))
}

fn admit_vdso_call(
    state: &OwnedContext,
    fault: crate::vdso::Fault<'_>,
    target: u64,
) -> Option<crate::vdso::Call> {
    state
        .vdso
        .as_ref()?
        .call(fault, target, execution_ranges(state, target))
}

fn prepare_next_step(
    state: &mut OwnedContext,
    ticket: Option<crate::timer::Ticket>,
    clock: u64,
) -> Result<bool, StepPreparationFailure> {
    use StepPreparationFailure::Predicate;
    if !(ticket.is_some()
        || state
            .vdso
            .as_ref()
            .is_some_and(|vdso| vdso.native_pc(state.registers.instruction_pointer)))
    {
        return Ok(false);
    }
    let pc = state.registers.instruction_pointer;
    if state
        .vdso
        .as_ref()
        .is_some_and(|vdso| vdso.operation(pc).is_some())
    {
        state
            .stepper
            .as_mut()
            .ok_or(Predicate)?
            .stage_call(
                (state.owner, state.generation),
                ticket.ok_or(Predicate)?,
                &state.registers,
                clock,
            )
            .ok_or(Predicate)?;
    } else {
        let mut failure = StepRefusal::new(state, pc);
        let instruction = step_instruction_recorded(state, pc, &mut failure)
            .ok_or(StepPreparationFailure::Decode(failure))?;
        let next = if instruction == crate::owned_step::Instruction::Syscall {
            pc.checked_add(2).ok_or(Predicate)?
        } else if let crate::owned_step::Instruction::Native(request) = &instruction {
            request.pc
        } else {
            instruction.plan(pc, &state.registers).ok_or(Predicate)?.pc
        };
        if !execution_ranges(state, next)
            .iter()
            .any(|range| range.contains(&next))
        {
            return Err(Predicate);
        }
        let frame = (state.owner, state.generation);
        let stepper = state.stepper.as_mut().ok_or(Predicate)?;
        if let crate::owned_step::Instruction::Native(request) = instruction {
            stepper
                .stage_cpu(frame, ticket, *request, &state.registers, clock)
                .ok_or(Predicate)?;
        } else if let Some(ticket) = ticket {
            stepper
                .stage_precise(frame, ticket, instruction, &state.registers, clock)
                .ok_or(Predicate)?;
        } else if !matches!(
            instruction,
            crate::owned_step::Instruction::Syscall | crate::owned_step::Instruction::Fault(_)
        ) {
            return Err(Predicate);
        }
    }
    Ok(true)
}

fn complete_owned_emulation<'frame>(
    image: RelocatedFrame<'frame>,
    context: &mut HookContext,
    instruction: InstructionEvent,
    result: reverie_preload::signal::native_frame::InstructionResult,
) -> Result<RelocatedFrame<'frame>, reverie_preload::signal::native_frame::Error> {
    use reverie_preload::signal::native_frame::Error;
    use reverie_preload::signal::native_frame::InstructionResult;
    let kind_matches = match result {
        InstructionResult::Cpuid(_) => instruction.kind == Kind::Cpuid,
        InstructionResult::Rdtsc { request, .. } => match request {
            reverie::Rdtsc::Tsc => instruction.kind == Kind::Rdtsc,
            reverie::Rdtsc::Tscp => instruction.kind == Kind::Rdtscp,
        },
    };
    if !kind_matches
        || context.instruction_pointer != instruction.fault_pc
        || instruction
            .fault_pc
            .checked_add(instruction.kind.bytes().len() as u64)
            != Some(instruction.resume_pc)
    {
        return Err(Error::InstructionPointer);
    }
    let image = image.complete_owned_instruction(instruction.fault_pc, context.rflags, result)?;
    context.instruction_pointer = instruction.resume_pc;
    context.rflags &= !0x10000;
    Ok(image)
}

struct StepRefusal {
    pc: u64,
    flags: u64,
    fault_resume: bool,
    reason: crate::owned_step::native::Refusal,
    fetched: Option<([u8; 15], usize)>,
    next: Option<u64>,
}

impl StepRefusal {
    fn report_exclusion_with(
        &self,
        state: &OwnedContext,
        mut emit: impl FnMut(&str, &str, Option<i64>),
    ) {
        if self.reason.stage != "step/vdso-exclusion" {
            return;
        }
        if let Some(vdso) = &state.vdso {
            vdso.report_exclusion_with(self.pc, &mut emit);
        } else {
            emit("step/vdso-interval", "owner-unavailable", None);
        }
        for (high, low, value) in [
            ("rdi-high32", "rdi-low32", state.registers.rdi),
            ("rsi-high32", "rsi-low32", state.registers.rsi),
            ("rdx-high32", "rdx-low32", state.registers.rdx),
            ("rcx-high32", "rcx-low32", state.registers.rcx),
            ("r8-high32", "r8-low32", state.registers.r8),
        ] {
            emit("step/vdso-abi", high, Some((value >> 32) as i64));
            emit("step/vdso-abi", low, Some(i64::from(value as u32)));
        }
    }

    fn new(state: &OwnedContext, pc: u64) -> Self {
        Self {
            pc,
            flags: state.registers.rflags,
            fault_resume: state
                .native
                .as_ref()
                .is_some_and(|native| native.fault_resume == Some(pc)),
            reason: crate::owned_step::native::Refusal::new("step/vdso-exclusion"),
            fetched: None,
            next: None,
        }
    }

    fn stage(&mut self, stage: &'static str) {
        self.reason = crate::owned_step::native::Refusal::new(stage);
    }

    fn report_with(&self, mut emit: impl FnMut(&'static str, &'static str, Option<i64>)) {
        let stage = self.reason.stage;
        emit(stage, "refusal", None);
        for (high, low, value) in [
            ("pc-high32", "pc-low32", self.pc),
            ("flags-high32", "flags-low32", self.flags),
        ] {
            emit(stage, high, Some((value >> 32) as i64));
            emit(stage, low, Some(i64::from(value as u32)));
        }
        emit(stage, "fault-resume", Some(i64::from(self.fault_resume)));
        if let Some((bytes, count)) = self.fetched {
            emit(stage, "fetched-count", Some(count as i64));
            for (field, byte) in [
                "byte-0", "byte-1", "byte-2", "byte-3", "byte-4", "byte-5", "byte-6", "byte-7",
                "byte-8", "byte-9", "byte-10", "byte-11", "byte-12", "byte-13", "byte-14",
            ]
            .into_iter()
            .zip(bytes)
            .take(count)
            {
                emit(stage, field, Some(i64::from(byte)));
            }
        }
        if let Some(next) = self.next {
            emit(stage, "next-high32", Some((next >> 32) as i64));
            emit(stage, "next-low32", Some(i64::from(next as u32)));
        }
        if let Some((address, width, write)) = self.reason.operand {
            emit(stage, "operand-high32", Some((address >> 32) as i64));
            emit(stage, "operand-low32", Some(i64::from(address as u32)));
            emit(stage, "operand-width", Some(width as i64));
            emit(stage, "operand-write", Some(i64::from(write)));
        }
    }
}

fn step_instruction(state: &OwnedContext, pc: u64) -> Option<crate::owned_step::Instruction> {
    step_instruction_recorded(state, pc, &mut StepRefusal::new(state, pc))
}

fn execution_ranges(state: &OwnedContext, pc: u64) -> &[Range<u64>] {
    if state.native.is_some()
        && let Some(vdso) = &state.vdso
        && vdso.range.contains(&pc)
    {
        core::slice::from_ref(&vdso.range)
    } else {
        &state.mappings
    }
}

fn excluded_pc(state: &OwnedContext, pc: u64) -> bool {
    state.vdso.as_ref().is_some_and(|vdso| {
        vdso.concerns(pc, pc) && !(state.native.is_some() && vdso.native_pc(pc))
    })
}

fn step_instruction_recorded(
    state: &OwnedContext,
    pc: u64,
    failure: &mut StepRefusal,
) -> Option<crate::owned_step::Instruction> {
    if excluded_pc(state, pc) {
        return None;
    }
    if let Some(native) = &state.native {
        failure.stage(if pc == 0 || pc >= 1 << 47 {
            "step/native-fetch-pc"
        } else {
            "step/native-fetch-mapping"
        });
        let (bytes, length) =
            native_step::fetch_with(execution_ranges(state, pc), pc, |address| unsafe {
                ptr::read(address as *const u8)
            })?;
        failure.fetched = Some((bytes, length));
        if bytes[..length].starts_with(&[0x0f, 0x05]) && state.syscalls {
            return Some(crate::owned_step::Instruction::Syscall);
        }
        if let Some(instruction @ crate::owned_step::Instruction::Fault(event)) =
            crate::owned_step::Instruction::decode(pc, &bytes[..length])
        {
            let subscribed = match event.kind {
                Kind::Cpuid => state.subscriptions.cpuid,
                _ => state.subscriptions.rdtsc,
            };
            failure.stage("step/fault-subscription");
            failure.next = Some(event.resume_pc);
            if !subscribed {
                return None;
            }
            failure.stage("step/fault-resume-mapping");
            if !execution_ranges(state, event.resume_pc)
                .iter()
                .any(|range| range.contains(&event.resume_pc))
            {
                return None;
            }
            return Some(instruction);
        }
        let request = crate::owned_step::native::CpuStepRequest::decode_recorded(
            pc,
            &bytes[..length],
            &state.registers,
            native.fault_resume == Some(pc),
            &mut failure.reason,
        )?;
        return Some(crate::owned_step::Instruction::Native(Box::new(request)));
    }
    failure.stage("step/finite-fetch-mapping");
    let mapping = state.mappings.iter().find(|range| range.contains(&pc))?;
    let bytes =
        unsafe { core::slice::from_raw_parts(pc as *const u8, (mapping.end - pc).min(6) as usize) };
    if state.syscalls
        && bytes.starts_with(&[0x0f, 0x05])
        && pc
            .checked_add(2)
            .is_some_and(|next| mapping.contains(&next))
    {
        return Some(crate::owned_step::Instruction::Syscall);
    }
    failure.stage("step/finite-decode");
    let instruction = crate::owned_step::Instruction::decode(pc, bytes)?;
    if let crate::owned_step::Instruction::Fault(event) = instruction {
        let subscribed = match event.kind {
            crate::runtime::InstructionEventKind::Cpuid => state.subscriptions.cpuid,
            _ => state.subscriptions.rdtsc,
        };
        failure.stage("step/finite-subscription");
        failure.next = Some(event.resume_pc);
        if !subscribed {
            return None;
        }
        failure.stage("step/finite-resume-mapping");
        if !mapping.contains(&event.resume_pc) {
            return None;
        }
    }
    Some(instruction)
}

fn timer_window_eligible(state: &OwnedContext, pc: u64) -> bool {
    if state
        .vdso
        .as_ref()
        .is_some_and(|vdso| vdso.operation(pc).is_some())
    {
        return true;
    }
    if state.native.is_none() || excluded_pc(state, pc) {
        return step_instruction(state, pc).is_some();
    }
    let Some((bytes, length)) =
        native_step::fetch_with(execution_ranges(state, pc), pc, |address| unsafe {
            ptr::read(address as *const u8)
        })
    else {
        return false;
    };
    if (state.syscalls && bytes[..length].starts_with(&[0x0f, 0x05]))
        || matches!(
            crate::owned_step::Instruction::decode(pc, &bytes[..length]),
            Some(crate::owned_step::Instruction::Fault(_))
        )
    {
        return step_instruction(state, pc).is_some();
    }
    crate::owned_step::native::CpuStepRequest::eligible(pc, &bytes[..length])
}

fn open_timer_window(state: &OwnedContext, pc: u64) {
    if timer_window_eligible(state, pc) {
        let clock =
            crate::runtime::read_guest_rcb_clock().unwrap_or_else(|error| refuse_error(error));
        crate::timer::open_window(pc, clock).unwrap_or_else(|error| refuse_error(error));
    }
}

fn deliver_timer(state: &mut OwnedContext, position: crate::timer::OwnedTimerPosition) {
    crate::mapping::update_continuation(
        state.registers.instruction_pointer,
        state.registers.stack_pointer,
    )
    .unwrap_or_else(|error| refuse_error(error));
    open_timer_window(state, state.registers.instruction_pointer);
    crate::timer::publish_delivery(position).unwrap_or_else(|error| refuse_error(error));
    unsafe { crate::runtime::dispatch_owned_timer(&mut state.registers) };
    crate::timer::close_window().unwrap_or_else(|error| refuse_error(error));
    refresh_mapping_views(state);
}

fn refresh_mapping_views(state: &mut OwnedContext) {
    if let Some(view) = crate::mapping::view().unwrap_or_else(|error| refuse_error(error)) {
        state.readable = view.readable;
        state.writable = view.writable;
        state.mappings = view.executable;
        if let Some(vdso) = &mut state.vdso {
            vdso.update_outputs(&state.writable);
        }
    }
}

fn environment_matches(state: &OwnedContext) -> bool {
    environment_mask(state, EnvironmentPhase::Ordinary).is_some()
}

#[derive(Clone, Copy)]
enum EnvironmentPhase {
    Ordinary,
    #[cfg(feature = "private-crt")]
    InitialGuest,
}

fn mask_matches_phase(mask: u64, phase: EnvironmentPhase) -> bool {
    match phase {
        EnvironmentPhase::Ordinary => {
            mask == crate::syscall_fallback::runtime_mask()
                & !((1 << (libc::SIGKILL - 1)) | (1 << (libc::SIGSTOP - 1)))
        }
        #[cfg(feature = "private-crt")]
        EnvironmentPhase::InitialGuest => {
            mask & (reverie_preload::signal::required_runtime_signal_mask()
                | (1 << (libc::SIGSYS - 1))
                | (1 << (libc::SIGSEGV - 1))
                | (1 << (libc::SIGTRAP - 1)))
                == 0
        }
    }
}

fn environment_mask(state: &OwnedContext, phase: EnvironmentPhase) -> Option<u64> {
    let mut stack: libc::stack_t = unsafe { core::mem::zeroed() };
    let mut mask = 0u64;
    let matches = clock_environment_matches(state)
        && controls() == Some(state.controls)
        && syscall(
            libc::SYS_rt_sigprocmask,
            [0, 0, (&raw mut mask) as u64, 8, 0, 0],
        ) == 0
        && mask_matches_phase(mask, phase)
        && syscall(
            libc::SYS_sigaltstack,
            [0, (&raw mut stack) as u64, 0, 0, 0, 0],
        ) == 0
        && stack.ss_sp as usize == state.signal_stack.start
        && stack.ss_size == state.signal_stack.len()
        && stack.ss_flags == 0;
    matches.then_some(mask)
}

#[unsafe(naked)]
unsafe extern "C" fn ordinary_entry() {
    core::arch::naked_asm!(
        "sub rsp, 8", "cld", "lea rdi, [rip + {entry}]", "call {enter}", "mov r12, rax",
        "call {dispatch}", "mov r13, rax", "mov rdi, r12", "xor esi, esi", "xor edx, edx",
        "call {leave}", "call {finish_context}", "mov rsp, r13", "mov eax, 15", "jmp reverie_preload_trusted_syscall_ip",
        entry = sym ordinary_entry, dispatch = sym dispatch,
        enter = sym crate::clock_control::reverie_liteinst_clock_enter,
        leave = sym crate::clock_control::reverie_liteinst_clock_leave,
        finish_context = sym reverie_preload::clock_boundary::finish_deferred_context,
    );
}

#[cfg(test)]
mod ownership_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn routing_fixture_restores_state_and_refuses_nested_publication() {
        assert!(!super::syscall_mode());
        let result = super::with_routing_syscall_state(|| {
            assert!(super::syscall_mode());
            let pointer = super::STATE.with(|slot| slot.load(super::Ordering::Acquire));
            let state = unsafe { &*pointer };
            assert_eq!(state.owner, unsafe {
                super::raw_syscall6(libc::SYS_gettid, [0; 6])
            });
            assert_eq!(state.generation, 0);
            assert!(state.native.is_none() && state.image.is_none() && state.event.is_none());
            assert!(!state.clocked && !state.precise_timer);
            assert!(super::with_routing_syscall_state(|| panic!("nested fixture ran")).is_err());
            assert_eq!(
                super::STATE.with(|slot| slot.load(super::Ordering::Acquire)),
                pointer
            );
            assert!(!std::thread::spawn(super::syscall_mode).join().unwrap());
            42
        })
        .unwrap();
        assert_eq!(result, 42);
        assert!(!super::syscall_mode());
        assert!(
            super::STATE
                .with(|slot| slot.load(super::Ordering::Acquire))
                .is_null()
        );
        assert_eq!(
            super::PHASE.with(|slot| slot.load(super::Ordering::Acquire)),
            super::IDLE
        );
        assert!(
            super::EVIDENCE
                .with(|slot| slot.load(super::Ordering::Acquire))
                .is_null()
        );
    }

    #[test]
    fn routing_fixture_restores_after_unwind() {
        let result = std::panic::catch_unwind(|| {
            super::with_routing_syscall_state(|| panic!("routing fixture unwind")).unwrap();
        });
        assert!(result.is_err());
        assert!(
            super::STATE
                .with(|slot| slot.load(super::Ordering::Acquire))
                .is_null()
        );
        assert_eq!(
            super::PHASE.with(|slot| slot.load(super::Ordering::Acquire)),
            super::IDLE
        );
        assert!(
            super::EVIDENCE
                .with(|slot| slot.load(super::Ordering::Acquire))
                .is_null()
        );
        super::with_routing_syscall_state(|| assert!(super::syscall_mode())).unwrap();
    }

    #[test]
    fn routing_fixture_preserves_nonidle_phase_and_evidence() {
        super::PHASE.with(|slot| slot.store(super::CAPTURED, super::Ordering::Release));
        assert!(super::with_routing_syscall_state(|| panic!("nonidle fixture ran")).is_err());
        assert_eq!(
            super::PHASE.with(|slot| slot.load(super::Ordering::Acquire)),
            super::CAPTURED
        );
        super::PHASE.with(|slot| slot.store(super::IDLE, super::Ordering::Release));
        let mut evidence = Box::new(super::Evidence {
            bytes: [0; 32 * (super::STORAGE_BYTES + 64)],
            used: 7,
        });
        let pointer = &raw mut *evidence;
        super::EVIDENCE.with(|slot| slot.store(pointer, super::Ordering::Release));
        assert!(super::with_routing_syscall_state(|| panic!("evidence fixture ran")).is_err());
        assert_eq!(
            super::EVIDENCE.with(|slot| slot.load(super::Ordering::Acquire)),
            pointer
        );
        assert_eq!(evidence.used, 7);
        assert!(
            super::STATE
                .with(|slot| slot.load(super::Ordering::Acquire))
                .is_null()
        );
        super::EVIDENCE.with(|slot| slot.store(super::ptr::null_mut(), super::Ordering::Release));
    }

    #[test]
    fn terminal126_owned_error_details_preserve_typed_results() {
        use super::RefusalError;
        let error = std::io::Error::from_raw_os_error(22);
        assert_eq!(error.diagnostic(), ("io-errno", Some(22)));
        let error = std::io::Error::other("not formatted on the failure path");
        assert_eq!(
            error.diagnostic(),
            ("io-error-kind", Some(std::io::ErrorKind::Other as i64))
        );
        assert_eq!(reverie::Errno::EINVAL.diagnostic(), ("errno", Some(22)));
        let error = crate::mapping::Failure::ordinary("host-mapping-control", Some(-22));
        assert_eq!(error.diagnostic(), ("host-mapping-control", Some(-22)));
        assert_eq!(
            reverie_preload::signal::native_frame::Error::SourceBounds.diagnostic(),
            ("native-frame/source-bounds", None)
        );
    }

    #[test]
    fn terminal126_owned_direct_and_closure_sites_survive_ordinary_child_exit() {
        const CHILD: &str = "REVERIE_OWNED_TERMINAL126_HOST_CHILD";
        fn direct_site(active: bool) {
            if active {
                super::refuse();
            }
        }
        fn closure_site(active: bool) {
            let error = crate::mapping::Failure::ordinary("host-mapping-control", Some(-22));
            if active {
                let refuse = |error| super::refuse_error(error);
                refuse(error);
            }
        }
        let selected = std::env::var(CHILD).unwrap_or_default();
        direct_site(selected == "direct");
        closure_site(selected == "closure");
        for (selection, expression, detail) in [
            ("direct", "super::refuse()", "detail=predicate value=none"),
            (
                "closure",
                "super::refuse_error(error)",
                "detail=host-mapping-control value=-22",
            ),
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "owned_context::tests::terminal126_owned_direct_and_closure_sites_survive_ordinary_child_exit", "--nocapture"])
                .env(CHILD, selection).output().unwrap();
            assert_eq!(output.status.code(), Some(126));
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains("operation=owned-context"), "{stderr}");
            assert!(stderr.contains(detail), "{stderr}");
            let prefix = format!("site={}:", file!());
            let (_, site) = stderr.split_once(prefix.as_str()).unwrap();
            let line: usize = site.split(':').next().unwrap().parse().unwrap();
            assert!(
                include_str!("owned_context.rs")
                    .lines()
                    .nth(line - 1)
                    .unwrap()
                    .contains(expression),
                "{stderr}"
            );
        }
    }
    use super::*;

    fn check_result_dependent_timer_window(valid_before: bool) {
        use reverie::CpuIdResult;
        use reverie::Rdtsc;
        use reverie::RdtscResult;
        use reverie_preload::signal::native_frame::InstructionResult;

        for opcode in [&[0x48, 0x8b, 0x04, 0xc7][..], &[0xff, 0x24, 0xc7]] {
            for kind in [Kind::Cpuid, Kind::Rdtsc, Kind::Rdtscp] {
                let mut code = [0x90u8; 32];
                code[..opcode.len()].copy_from_slice(opcode);
                let pc = code.as_ptr() as u64;
                let data = [pc + opcode.len() as u64; 2];
                let address = data.as_ptr() as u64;
                let mut registers: HookContext = unsafe { core::mem::zeroed() };
                registers.instruction_pointer = pc;
                registers.rflags = 0x202;
                registers.rdi = address;
                registers.rax = if valid_before { 1 } else { 0xfeed };
                let mut state = OwnedContext {
                    owner: 1,
                    mappings: vec![pc..pc + code.len() as u64; 1],
                    readable: vec![address..address + core::mem::size_of_val(&data) as u64; 1],
                    writable: Vec::new(),
                    vdso: None,
                    signal_stack: 0..0,
                    runtime_sp: 0,
                    runtime_stack: 0..0,
                    storage: ptr::null_mut(),
                    image: None,
                    registers,
                    event: None,
                    stepper: None,
                    precise_timer: true,
                    syscalls: true,
                    native: Some(native_step::NativeState::new(0).unwrap()),
                    guest_mask: 0,
                    subscriptions: crate::runtime::InstructionSubscriptions {
                        cpuid: true,
                        rdtsc: true,
                    },
                    clocked: false,
                    controls: Controls {
                        segments_and_permissions: [0; 3],
                        xcr0: 0,
                        pkru: 0,
                        cpuid: 0,
                        tsc: 0,
                    },
                    errno: ptr::null_mut(),
                    saved_errno: 0,
                    generation: 1,
                };
                assert!(timer_window_eligible(&state, pc));
                let before = step_instruction(&state, pc).unwrap();
                let value = if valid_before { 0xfeed } else { 1 };
                let result = match kind {
                    Kind::Cpuid => InstructionResult::Cpuid(CpuIdResult {
                        eax: value,
                        ebx: 0,
                        ecx: 0,
                        edx: 0,
                    }),
                    Kind::Rdtsc | Kind::Rdtscp => InstructionResult::Rdtsc {
                        request: if kind == Kind::Rdtsc {
                            Rdtsc::Tsc
                        } else {
                            Rdtsc::Tscp
                        },
                        result: RdtscResult {
                            tsc: u64::from(value),
                            aux: Some(0),
                        },
                    },
                };
                crate::instruction_event::apply_result(&mut state.registers, result);
                assert!(timer_window_eligible(&state, pc));
                assert_eq!(state.registers.rax, u64::from(value));
                let after = step_instruction(&state, pc).unwrap();
                let mut controller = reverie_preload::precise_timer::Controller::default();
                let (generation, _) = controller.replace(40, 0, 1).unwrap();
                let ticket = crate::timer::Ticket {
                    generation,
                    sequence: 1,
                };
                let mut stepper = Stepper::default();
                assert!(
                    stepper
                        .stage_precise(
                            (state.owner, state.generation),
                            ticket,
                            before,
                            &state.registers,
                            40,
                        )
                        .is_none()
                );
                assert!(!stepper.pending());
                assert!(
                    stepper
                        .stage_precise(
                            (state.owner, state.generation),
                            ticket,
                            after,
                            &state.registers,
                            40,
                        )
                        .is_some()
                );
                assert!(stepper.pending());
                assert_eq!(stepper.completed, 0);
                stepper.cancel();
                assert!(!stepper.pending());
                assert_eq!(stepper.completed, 0);
                assert_eq!(state.registers.rflags & 0x100, 0);
            }
        }
    }

    #[test]
    fn final_arming_binds_result_after_invalid_to_valid_operand_change() {
        check_result_dependent_timer_window(false);
    }

    #[test]
    fn final_arming_rejects_pre_callback_request_after_operand_invalidated() {
        check_result_dependent_timer_window(true);
    }

    #[cfg(feature = "test-owned-cpuid")]
    #[test]
    fn terminal_evidence_error_retains_runtime_guard_through_diagnostics() {
        assert!(!crate::runtime_domain::allocation_active());
        let evidence = collect_syscall_evidence(|| {
            assert!(crate::runtime_domain::allocation_active());
            Err(io::ErrorKind::Unsupported.into())
        });
        assert!(crate::runtime_domain::allocation_active());
        assert_eq!(
            evidence.bytes.as_ref().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        drop(evidence);
        assert!(!crate::runtime_domain::allocation_active());
    }

    #[test]
    fn owned_clock_setup_requires_explicit_counting_only_activation() {
        for (name, clocked, requested, active, sources, notification_free, expected) in [
            ("legacy no-clock", false, false, false, false, true, true),
            ("explicit counting", true, true, false, false, true, true),
            (
                "missing constructor",
                true,
                false,
                false,
                false,
                true,
                false,
            ),
            ("legacy request", false, true, false, false, true, false),
            ("legacy active", false, false, true, false, true, false),
            ("rebind active", true, true, true, false, true, false),
            ("registered source", true, true, false, true, true, false),
            ("legacy source", false, false, false, true, true, false),
            ("notification state", true, true, false, false, false, false),
        ] {
            assert_eq!(
                clock_setup_allowed(clocked, requested, active, sources, notification_free),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn posix_timer_inventory_reports_unavailability_without_claiming_empty() {
        let missing =
            inspect_posix_timer_inventory(Err(io::Error::from_raw_os_error(libc::ENOENT))).unwrap();
        assert_eq!(missing, PosixTimerInventory::Unavailable);
        assert_ne!(missing, PosixTimerInventory::Empty);
        assert_eq!(format!("{missing:?}"), "Unavailable");
        assert_eq!(
            inspect_posix_timer_inventory(Ok(" \n".into())).unwrap(),
            PosixTimerInventory::Empty
        );
    }

    #[test]
    fn posix_timer_inventory_rejects_existing_timers() {
        assert_eq!(
            inspect_posix_timer_inventory(Ok("ID: 0\nsignal: 34/0000000000000000\n".into()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn posix_timer_inventory_preserves_other_read_failures() {
        for error in [libc::EACCES, libc::EIO] {
            assert_eq!(
                inspect_posix_timer_inventory(Err(io::Error::from_raw_os_error(error)))
                    .unwrap_err()
                    .raw_os_error(),
                Some(error)
            );
        }
    }
}

#[cfg(all(test, feature = "private-crt"))]
mod initial_tests {
    use super::*;

    fn f1_mediated_successor(opcode: &[u8], syscall: bool) {
        use reverie_preload::precise_timer::Controller;
        let mut code = [0x90; 32];
        code[..opcode.len()].copy_from_slice(opcode);
        let mut state = step_state(&code);
        let pc = state.registers.instruction_pointer;
        let successor = pc + opcode.len() as u64;
        state.vdso = Some(crate::vdso::Active::stepping_model(pc..pc + 32, successor));
        state.mappings.clear();
        state.generation = 1;
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(40, 2, 0).unwrap();
        for ticket in [
            None,
            Some(crate::timer::Ticket {
                generation,
                sequence: 1,
            }),
        ] {
            let instruction = step_instruction(&state, pc);
            assert!(
                instruction.is_some(),
                "F1 mediated instruction successor membership"
            );
            let instruction = instruction.unwrap();
            assert_eq!(
                instruction == crate::owned_step::Instruction::Syscall,
                syscall
            );
            if let crate::owned_step::Instruction::Fault(event) = instruction {
                assert_eq!(event.resume_pc, successor);
            }
            assert_eq!(
                execution_ranges(&state, successor),
                &[pc..pc + 32; 1],
                "F1 dispatch continuation membership"
            );
            assert!(timer_window_eligible(&state, pc));
            assert!(timer_window_eligible(&state, successor));
            assert!(step_instruction(&state, successor).is_none());
            assert!(!state.vdso.as_ref().unwrap().native_pc(successor));
            assert_eq!(state.registers.instruction_pointer, pc);
            let stepper = state.stepper.as_mut().unwrap();
            if let Some(ticket) = ticket {
                stepper
                    .stage_precise(
                        (state.owner, state.generation),
                        ticket,
                        instruction,
                        &state.registers,
                        40,
                    )
                    .unwrap();
                assert!(stepper.pending());
                stepper.cancel();
            } else {
                assert!(!stepper.pending());
            }
            assert_eq!(stepper.completed, 0);
        }
    }

    #[test]
    fn f1_cpuid_successor_at_recognized_entry_armed_and_unarmed() {
        f1_mediated_successor(&[0x0f, 0xa2], false);
    }

    #[test]
    fn f1_rdtsc_successor_at_recognized_entry_armed_and_unarmed() {
        f1_mediated_successor(&[0x0f, 0x31], false);
    }

    #[test]
    fn f1_syscall_successor_at_recognized_entry_armed_and_unarmed() {
        f1_mediated_successor(&[0x0f, 0x05], true);
    }

    #[test]
    fn f1_vvar_private_invalid_and_unowned_continuations_remain_excluded() {
        let code = [0x90; 32];
        let mut state = step_state(&code);
        let pc = state.registers.instruction_pointer;
        state.vdso = Some(crate::vdso::Active::stepping_model(pc..pc + 32, pc + 2));
        state.mappings.clear();
        state.runtime_stack = 0x40000..0x50000;
        for target in [0, pc - 1, pc + 32, 0x41000, 1 << 47, u64::MAX] {
            assert!(
                execution_ranges(&state, target).is_empty(),
                "target={target:x}"
            );
            assert!(!timer_window_eligible(&state, target));
            assert!(step_instruction(&state, target).is_none());
            assert!(!state.vdso.as_ref().unwrap().native_pc(target));
        }
        state.native = None;
        assert!(execution_ranges(&state, pc + 2).is_empty());
    }

    #[test]
    fn vdso_owned_eligibility_fetch_and_staging_share_generic_forms_without_timer() {
        for opcode in [
            &[0x90][..],
            &[0xc3],
            &[0xf3, 0x48, 0xab],
            &[0x48, 0x89, 0x07],
            &[0xf3, 0x0f, 0x1e, 0xfa],
        ] {
            for precise in [false, true] {
                let mut code = [0x90; 32];
                code[..opcode.len()].copy_from_slice(opcode);
                let mut state = step_state(&code);
                let pc = state.registers.instruction_pointer;
                state.vdso = Some(crate::vdso::Active::stepping_model(pc..pc + 32, pc + 24));
                state.mappings.clear();
                state.precise_timer = precise;
                state.generation = 1;
                assert!(timer_window_eligible(&state, pc));
                let crate::owned_step::Instruction::Native(request) =
                    step_instruction(&state, pc).unwrap()
                else {
                    panic!("not native")
                };
                let request = *request;
                let mut changed = state.registers;
                changed.rax ^= 1;
                let stepper = state.stepper.as_mut().unwrap();
                assert!(
                    stepper
                        .stage_cpu((1, 1), None, request, &changed, 47)
                        .is_none()
                );
                stepper
                    .stage_cpu((1, 1), None, request, &state.registers, 47)
                    .unwrap();
                stepper.cancel();
                assert!(!stepper.pending());
                assert_eq!(stepper.completed, 0);
                assert!(!timer_window_eligible(&state, pc - 1));
                assert!(step_instruction(&state, pc - 1).is_none());
                assert!(!state.vdso.as_ref().unwrap().accessible(&(pc - 1..pc)));
            }
        }
    }

    #[test]
    fn vdso_recognized_exports_keep_nx_and_special_instructions_keep_mediation() {
        let mut code = [0x90; 32];
        code[..2].copy_from_slice(&[0x0f, 0x05]);
        code[4..6].copy_from_slice(&[0x0f, 0xa2]);
        code[8..11].copy_from_slice(&[0x0f, 0xc7, 0xf0]);
        let mut state = step_state(&code);
        let pc = state.registers.instruction_pointer;
        state.vdso = Some(crate::vdso::Active::stepping_model(pc..pc + 32, pc + 24));
        state.mappings.clear();
        for precise in [false, true] {
            state.precise_timer = precise;
            assert!(timer_window_eligible(&state, pc + 24));
            assert!(step_instruction(&state, pc + 24).is_none());
            assert!(!state.vdso.as_ref().unwrap().native_pc(pc + 24));
            assert_eq!(
                step_instruction(&state, pc),
                Some(crate::owned_step::Instruction::Syscall)
            );
            assert!(matches!(
                step_instruction(&state, pc + 4),
                Some(crate::owned_step::Instruction::Fault(_))
            ));
            state.subscriptions.cpuid = false;
            assert!(!timer_window_eligible(&state, pc + 4));
            assert!(step_instruction(&state, pc + 4).is_none());
            state.subscriptions.cpuid = true;
            assert!(!timer_window_eligible(&state, pc + 8));
            assert!(step_instruction(&state, pc + 8).is_none());
        }
    }

    #[test]
    fn vdso_data_fault_metadata_is_not_execute_fault_or_successful_step() {
        let mut registers = [0i64; 23];
        registers[libc::REG_RIP as usize] = 0x7000;
        registers[libc::REG_EFL as usize] = 0x10302;
        registers[libc::REG_TRAPNO as usize] = 14;
        for error in [4, 6, 7] {
            registers[libc::REG_ERR as usize] = error;
            assert!(vdso_data_fault(1, &registers));
            assert!(vdso_data_fault(2, &registers));
            assert!(!vdso_data_fault(libc::SI_KERNEL, &registers));
            assert!(!crate::owned_step::native::CpuStepRequest::trace_control(
                &registers
            ));
        }
        registers[libc::REG_ERR as usize] = 0x15;
        assert!(!vdso_data_fault(2, &registers));
        registers[libc::REG_TRAPNO as usize] = 13;
        registers[libc::REG_ERR as usize] = 0;
        assert!(vdso_data_fault(libc::SI_KERNEL, &registers));
        assert!(!vdso_data_fault(2, &registers));
        let mut state = step_state(&[0x90]);
        state.registers = snapshot(&registers);
        let before = crate::vdso::register_values(&state.registers);
        let fault = VdsoFault {
            signal: libc::SIGSEGV,
            code: 2,
            address: 0x1234_5678_9abc_def0,
            trap: 14,
            error: 7,
            clock: 0x1234_0000_0043,
            modeled: None,
        };
        let mut records = Vec::new();
        fault.report_with(&state.registers, |stage, field, value| {
            records.push((stage, field, value))
        });
        assert_eq!(
            records,
            [
                ("vdso/data-fault", "signal", Some(11)),
                ("vdso/data-fault", "code", Some(2)),
                ("vdso/data-fault", "trap", Some(14)),
                ("vdso/data-fault", "error", Some(7)),
                ("vdso/data-fault", "pc-high32", Some(0)),
                ("vdso/data-fault", "pc-low32", Some(0x7000)),
                ("vdso/data-fault", "address-high32", Some(0x1234_5678)),
                ("vdso/data-fault", "address-low32", Some(0x9abc_def0)),
                ("vdso/data-fault", "flags-high32", Some(0)),
                ("vdso/data-fault", "flags-low32", Some(0x10302)),
                ("vdso/data-fault", "count-high32", Some(0x1234)),
                ("vdso/data-fault", "count-low32", Some(0x43)),
                ("vdso/fault-registers", "rdi-high32", Some(0)),
                ("vdso/fault-registers", "rdi-low32", Some(0)),
                ("vdso/fault-registers", "rsi-high32", Some(0)),
                ("vdso/fault-registers", "rsi-low32", Some(0)),
                ("vdso/fault-registers", "rdx-high32", Some(0)),
                ("vdso/fault-registers", "rdx-low32", Some(0)),
                ("vdso/fault-registers", "rcx-high32", Some(0)),
                ("vdso/fault-registers", "rcx-low32", Some(0)),
                ("vdso/fault-registers", "r8-high32", Some(0)),
                ("vdso/fault-registers", "r8-low32", Some(0)),
                ("vdso/fault-registers", "r14-high32", Some(0)),
                ("vdso/fault-registers", "r14-low32", Some(0)),
            ]
        );
        assert_eq!(crate::vdso::register_values(&state.registers), before);
        assert_eq!(state.registers.rflags, 0x10302);
    }

    #[test]
    fn vdso_data_fault_reports_fault_time_registers_without_mutation() {
        let mut state = step_state(&[0x90]);
        state.registers.rdi = 0x1111_2222_3333_4444;
        state.registers.rsi = 0x5555_6666_7777_8888;
        state.registers.rdx = 0x9999_aaaa_bbbb_cccc;
        state.registers.rcx = 0xdddd_eeee_ffff_0000;
        state.registers.r8 = u64::MAX;
        state.registers.r14 = 0x1234_5678_9abc_def0;
        state.registers.r10 = 0xabcd;
        let before = crate::vdso::register_values(&state.registers);
        let fault = VdsoFault {
            signal: 11,
            code: 2,
            address: 0x1234,
            trap: 14,
            error: 4,
            clock: 18724,
            modeled: None,
        };
        let mut records = Vec::new();
        fault.report_with(&state.registers, |stage, field, value| {
            if stage == "vdso/fault-registers" {
                records.push((field, value));
            }
        });
        assert_eq!(
            records,
            [
                ("rdi-high32", Some(0x1111_2222)),
                ("rdi-low32", Some(0x3333_4444)),
                ("rsi-high32", Some(0x5555_6666)),
                ("rsi-low32", Some(0x7777_8888)),
                ("rdx-high32", Some(0x9999_aaaa)),
                ("rdx-low32", Some(0xbbbb_cccc)),
                ("rcx-high32", Some(0xdddd_eeee)),
                ("rcx-low32", Some(0xffff_0000)),
                ("r8-high32", Some(0xffff_ffff)),
                ("r8-low32", Some(0xffff_ffff)),
                ("r14-high32", Some(0x1234_5678)),
                ("r14-low32", Some(0x9abc_def0)),
            ]
        );
        assert_eq!(crate::vdso::register_values(&state.registers), before);
        assert_eq!(fault.clock, 18724);
        assert_eq!(fault.address, 0x1234);
    }

    #[test]
    fn exclusion_diagnostic_saved_five_abi_arguments_without_mutation() {
        let mut state = step_state(&[0x90, 0x90]);
        state.registers.rdi = 0x1111_2222_3333_4444;
        state.registers.rsi = 0x5555_6666_7777_8888;
        state.registers.rdx = 0x9999_aaaa_bbbb_cccc;
        state.registers.rcx = 0xdddd_eeee_ffff_0000;
        state.registers.r8 = u64::MAX;
        state.registers.r10 = 0xdead;
        let before = crate::owned_step::native::general(&state.registers);
        let pc = state.registers.instruction_pointer;
        let flags = state.registers.rflags;
        let failure = StepRefusal::new(&state, pc);
        let mut records = Vec::new();
        failure.report_exclusion_with(&state, |stage, detail, value| {
            records.push((stage.to_owned(), detail.to_owned(), value));
        });
        assert_eq!(
            records,
            [
                ("step/vdso-interval", "owner-unavailable", None),
                ("step/vdso-abi", "rdi-high32", Some(0x1111_2222)),
                ("step/vdso-abi", "rdi-low32", Some(0x3333_4444)),
                ("step/vdso-abi", "rsi-high32", Some(0x5555_6666)),
                ("step/vdso-abi", "rsi-low32", Some(0x7777_8888)),
                ("step/vdso-abi", "rdx-high32", Some(0x9999_aaaa)),
                ("step/vdso-abi", "rdx-low32", Some(0xbbbb_cccc)),
                ("step/vdso-abi", "rcx-high32", Some(0xdddd_eeee)),
                ("step/vdso-abi", "rcx-low32", Some(0xffff_0000)),
                ("step/vdso-abi", "r8-high32", Some(0xffff_ffff)),
                ("step/vdso-abi", "r8-low32", Some(0xffff_ffff)),
            ]
            .map(|(stage, detail, value)| (stage.to_owned(), detail.to_owned(), value))
        );
        assert_eq!(crate::owned_step::native::general(&state.registers), before);
        assert_eq!(state.registers.instruction_pointer, pc);
        assert_eq!(state.registers.rflags, flags);
        assert_eq!(failure.reason.stage, "step/vdso-exclusion");
        assert!(failure.fetched.is_none());
        assert!(failure.next.is_none());
    }

    #[test]
    fn exclusion_diagnostic_silent_for_other_refusals_and_speculative_checks() {
        let code = [0x90, 0x90];
        let state = step_state(&code);
        let pc = state.registers.instruction_pointer;
        let mut failure = StepRefusal::new(&state, pc);
        assert!(step_instruction_recorded(&state, pc, &mut failure).is_some());
        failure.report_exclusion_with(&state, |_, _, _| panic!("non-exclusion emitted"));
        failure.stage("step/fault-subscription");
        failure.report_exclusion_with(&state, |_, _, _| panic!("non-exclusion emitted"));
    }

    fn step_state(code: &[u8]) -> OwnedContext {
        let mut state = state();
        let pc = code.as_ptr() as u64;
        state.registers.instruction_pointer = pc;
        state.registers.rflags = 0x202;
        state.mappings = vec![pc..pc + code.len() as u64; 1];
        state
    }

    #[test]
    fn successor_window_composes_typed_frame_completion_and_fresh_staging() {
        use reverie_preload::precise_timer::Controller;
        use reverie_preload::precise_timer::Decision;
        use reverie_preload::signal::native_frame::Format;
        use reverie_preload::signal::native_frame::InstructionResult;
        use reverie_preload::signal::native_frame::relocate;

        use crate::owned_step::Instruction;
        use crate::owned_step::native;

        #[repr(align(64))]
        struct FrameBuffer([u8; 4096]);

        fn put(bytes: &mut [u8], offset: usize, value: u64) {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }

        fn fixture(source: &mut FrameBuffer, context: &HookContext) {
            let pointer = source.0.as_ptr() as u64;
            put(&mut source.0, 8, 0x7000);
            put(&mut source.0, 16, 7);
            for (field, value) in native::general(context).into_iter().enumerate() {
                put(&mut source.0, 8 + 48 + field * 8, value);
            }
            put(&mut source.0, 8 + 176, context.instruction_pointer);
            put(&mut source.0, 8 + 184, context.rflags);
            put(&mut source.0, 8 + 232, pointer + 512);
            source.0[976..980].copy_from_slice(&0x46505853u32.to_le_bytes());
            source.0[980..984].copy_from_slice(&2444u32.to_le_bytes());
            put(&mut source.0, 984, 0x2e7);
            source.0[992..996].copy_from_slice(&2440u32.to_le_bytes());
            put(&mut source.0, 1024, 0x2e7);
            source.0[2952..2956].copy_from_slice(&0x46505845u32.to_le_bytes());
        }

        let forms: [&[u8]; 3] = [&[0x90], &[0xb9, 7, 0, 0, 0], &[0xf3, 0x48, 0xab]];
        let mut cases = 0;
        for kind in [Kind::Cpuid, Kind::Rdtsc, Kind::Rdtscp] {
            for (form, opcode) in forms.into_iter().enumerate() {
                for count in [0u64, 1, 3] {
                    for flags in [0x10202u64, 0x10ed7] {
                        let mut code = [0x90; 32];
                        let length = kind.bytes().len();
                        code[..length].copy_from_slice(kind.bytes());
                        code[length..length + opcode.len()].copy_from_slice(opcode);
                        let mut state = step_state(&code);
                        state.generation = 1;
                        state.registers.rflags = flags;
                        state.registers.rax = 0x1111;
                        state.registers.rbx = 0x2222;
                        state.registers.rdx = 0x3333;
                        state.registers.r8 = 0x8888;
                        state.registers.rcx = count;
                        state.registers.rdi = 0x20000;
                        let predecessor = state.registers;
                        let event =
                            InstructionEvent::decode(predecessor.instruction_pointer, kind.bytes())
                                .unwrap();
                        assert!(timer_window_eligible(&state, event.resume_pc));
                        assert!(step_instruction(&state, event.resume_pc).is_none());
                        assert_eq!(state.registers.instruction_pointer, event.fault_pc);
                        assert_eq!(state.registers.rflags, flags);

                        let result = match kind {
                            Kind::Cpuid => InstructionResult::Cpuid(reverie::CpuIdResult {
                                eax: 0xfedcba98,
                                ebx: 0x87654321,
                                ecx: count as u32,
                                edx: 0x12345678,
                            }),
                            _ => InstructionResult::Rdtsc {
                                request: if kind == Kind::Rdtsc {
                                    reverie::Rdtsc::Tsc
                                } else {
                                    reverie::Rdtsc::Tscp
                                },
                                result: reverie::RdtscResult {
                                    tsc: 0x12345678fedcba98,
                                    aux: Some(count as u32),
                                },
                            },
                        };
                        let alternate_result = match result {
                            InstructionResult::Cpuid(mut value) => {
                                value.eax ^= 1;
                                InstructionResult::Cpuid(value)
                            }
                            InstructionResult::Rdtsc {
                                request,
                                mut result,
                            } => {
                                result.tsc ^= 1;
                                InstructionResult::Rdtsc { request, result }
                            }
                        };
                        let mut expected_general = native::general(&predecessor);
                        expected_general[13] = 0xfedcba98;
                        expected_general[12] = 0x12345678;
                        if kind == Kind::Cpuid {
                            expected_general[11] = 0x87654321;
                            expected_general[14] = count;
                        } else if kind == Kind::Rdtscp {
                            expected_general[14] = count;
                        }
                        let mut source = FrameBuffer([0; 4096]);
                        fixture(&mut source, &predecessor);
                        let original_source = source.0;
                        let address = source.0.as_ptr() as usize + 8;
                        let format = Format::StandardXsave {
                            xfeatures: 0x2e7,
                            xstate_size: 2440,
                        };
                        let mut alternate_storage = FrameBuffer([0; 4096]);
                        let alternate_image = relocate(
                            &source.0,
                            address,
                            address + 8,
                            format,
                            &mut alternate_storage.0,
                        )
                        .unwrap();
                        let mut alternate_context = predecessor;
                        crate::instruction_event::apply_result(
                            &mut alternate_context,
                            alternate_result,
                        );
                        let alternate_image = complete_owned_emulation(
                            alternate_image,
                            &mut alternate_context,
                            event,
                            alternate_result,
                        )
                        .unwrap();
                        assert_eq!(alternate_context.instruction_pointer, event.resume_pc);
                        assert_eq!(alternate_context.rflags, flags & !0x10000);
                        assert_eq!(alternate_context.rax, expected_general[13] ^ 1);
                        assert_eq!(
                            u64::from_le_bytes(
                                alternate_image.frame_bytes()[152..160].try_into().unwrap()
                            ),
                            alternate_context.rax
                        );
                        let stale = native::CpuStepRequest::decode_recorded(
                            event.resume_pc,
                            opcode,
                            &alternate_context,
                            false,
                            &mut native::Refusal::new("combined-test"),
                        )
                        .unwrap();

                        let mut storage = FrameBuffer([0; 4096]);
                        let image =
                            relocate(&source.0, address, address + 8, format, &mut storage.0)
                                .unwrap();
                        let mut expected_frame = image.frame_bytes().to_vec();
                        let expected_fp = image.fp_bytes().to_vec();
                        for (field, value) in expected_general.into_iter().enumerate() {
                            put(&mut expected_frame, 48 + field * 8, value);
                        }
                        put(&mut expected_frame, 176, event.resume_pc);
                        put(&mut expected_frame, 184, flags & !0x10000);
                        crate::instruction_event::apply_result(&mut state.registers, result);
                        assert_eq!(state.registers.instruction_pointer, event.fault_pc);
                        assert_eq!(state.registers.rflags, flags);
                        let image =
                            complete_owned_emulation(image, &mut state.registers, event, result)
                                .unwrap();
                        assert_eq!(state.registers.instruction_pointer, event.resume_pc);
                        assert_eq!(state.registers.rflags, flags & !0x10000);
                        assert_eq!(native::general(&state.registers), expected_general);
                        assert_eq!(image.frame_bytes(), expected_frame);
                        assert_eq!(image.fp_bytes(), expected_fp);
                        assert_eq!(source.0, original_source);
                        assert!(timer_window_eligible(&state, event.resume_pc));

                        let mut controller = Controller::default();
                        let (generation, decision) = controller.replace(40, 0, 1).unwrap();
                        assert_eq!(decision, Decision::Step);
                        let ticket = crate::timer::Ticket {
                            generation,
                            sequence: 1,
                        };
                        let frame = (state.owner, state.generation);
                        let mut stepper = Stepper::default();
                        assert!(
                            stepper
                                .stage_precise(
                                    frame,
                                    ticket,
                                    Instruction::Native(Box::new(stale)),
                                    &state.registers,
                                    40,
                                )
                                .is_none()
                        );
                        assert!(!stepper.pending());
                        assert_eq!(stepper.completed, 0);
                        let fresh = step_instruction(&state, event.resume_pc).unwrap();
                        stepper
                            .stage_precise(frame, ticket, fresh.clone(), &state.registers, 40)
                            .unwrap();
                        assert!(
                            stepper
                                .stage_precise(frame, ticket, fresh, &state.registers, 40)
                                .is_none()
                        );
                        let image = image
                            .owned_single_step(event.resume_pc, state.registers.rflags, true)
                            .unwrap();
                        put(&mut expected_frame, 184, (flags & !0x10000) | 0x100);
                        assert_eq!(image.frame_bytes(), expected_frame);
                        assert_eq!(image.fp_bytes(), expected_fp);

                        let mut post = state.registers;
                        post.instruction_pointer += opcode.len() as u64;
                        if form == 1 {
                            post.rcx = 7;
                        } else if form == 2 && count != 0 {
                            post.rcx -= 1;
                            if flags & 0x400 == 0 {
                                post.rdi += 8;
                            } else {
                                post.rdi -= 8;
                            }
                            if post.rcx != 0 {
                                post.instruction_pointer = event.resume_pc;
                            }
                        }
                        let mut registers = [0i64; 23];
                        registers[..16]
                            .copy_from_slice(&native::general(&post).map(|value| value as i64));
                        registers[libc::REG_RIP as usize] = post.instruction_pointer as i64;
                        registers[libc::REG_EFL as usize] = (post.rflags | 0x100) as i64;
                        registers[libc::REG_TRAPNO as usize] = 1;
                        let completion = stepper.capture(frame.0, frame.1, &registers, 40).unwrap();
                        assert!(stepper.capture(frame.0, frame.1, &registers, 40).is_none());
                        assert!(stepper.complete_precise(completion, 41).is_none());
                        let mut changed = completion;
                        changed.flags ^= 1;
                        assert!(stepper.complete_precise(changed, 40).is_none());
                        changed = completion;
                        changed.pc ^= 1;
                        assert!(stepper.complete_precise(changed, 40).is_none());
                        let observation = stepper.complete_precise(completion, 40).unwrap();
                        assert_eq!(observation.clock, 40);
                        assert_eq!(observation.rip, post.instruction_pointer);
                        assert_eq!(controller.observe(observation), Ok(Decision::Deliver));
                        assert!(stepper.complete_precise(completion, 40).is_none());
                        assert_eq!(stepper.completed, 1);
                        assert!(!stepper.pending());
                        assert_eq!(source.0, original_source);
                        cases += 1;
                    }
                }
            }
        }
        assert_eq!(cases, 54);
    }

    #[test]
    fn cpu_request_binds_start_without_predicting_next_mapping() {
        let code = [0x48, 0x89, 0xe7, 0x90];
        let mut state = step_state(&code);
        let pc = state.registers.instruction_pointer;
        let mut failure = StepRefusal::new(&state, pc);
        assert!(
            matches!(step_instruction_recorded(&state, pc, &mut failure), Some(crate::owned_step::Instruction::Native(request)) if request.starts_at(&state.registers))
        );
        assert!(step_instruction(&state, pc).is_some());
        assert!(timer_window_eligible(&state, pc));
        assert_eq!(state.registers.rflags, 0x202);
        assert_eq!(state.registers.instruction_pointer, pc);
        state.mappings.clear();
        failure = StepRefusal::new(&state, pc);
        assert!(step_instruction_recorded(&state, pc, &mut failure).is_none());
        assert_eq!(failure.reason.stage, "step/native-fetch-mapping");
        assert!(failure.fetched.is_none());
        failure = StepRefusal::new(&state, 0);
        assert!(step_instruction_recorded(&state, 0, &mut failure).is_none());
        assert_eq!(failure.reason.stage, "step/native-fetch-pc");
        assert!(failure.fetched.is_none());
        state.mappings = vec![pc..pc + 1; 1];
        failure = StepRefusal::new(&state, pc);
        assert!(step_instruction_recorded(&state, pc, &mut failure).is_none());
        assert_eq!(failure.reason.stage, "native/form");
        assert_eq!(failure.fetched.unwrap().1, 1);
        state.mappings = vec![pc..pc + 3; 1];
        failure = StepRefusal::new(&state, pc);
        assert!(step_instruction_recorded(&state, pc, &mut failure).is_some());
        assert_eq!(failure.fetched.unwrap().1, 3);
        assert_eq!(failure.next, None);
    }

    #[test]
    fn successor_form_eligibility_does_not_stage_a_stale_context() {
        for opcode in [
            &[0x90][..],
            &[0xb9, 1, 0, 0, 0],
            &[0xff, 0xc9],
            &[0x75, 0],
            &[0xeb, 0],
            &[0xf3, 0x48, 0xab],
            &[0x48, 0x8b, 0x07],
            &[0x48, 0x98],
            &[0xf3, 0x0f, 0x1e, 0xfa],
        ] {
            let mut code = [0x90; 32];
            code[..2].copy_from_slice(&[0x0f, 0x31]);
            code[2..2 + opcode.len()].copy_from_slice(opcode);
            let mut state = step_state(&code);
            let fault_pc = state.registers.instruction_pointer;
            let successor = fault_pc + 2;
            state.generation = 1;
            state.registers.rflags = 0x10202;
            assert!(timer_window_eligible(&state, successor), "{opcode:x?}");
            assert!(step_instruction(&state, successor).is_none());
            assert_eq!(state.registers.instruction_pointer, fault_pc);
            assert_eq!(state.registers.rflags, 0x10202);

            state.registers.instruction_pointer = successor;
            state.registers.rflags = 0x202;
            let stale = step_instruction(&state, successor).unwrap();
            state.registers.rax = 0xfeed;
            assert!(timer_window_eligible(&state, successor));
            let fresh = step_instruction(&state, successor).unwrap();
            let mut controller = reverie_preload::precise_timer::Controller::default();
            let (generation, _) = controller.replace(40, 0, 1).unwrap();
            let ticket = crate::timer::Ticket {
                generation,
                sequence: 1,
            };
            let mut stepper = Stepper::default();
            assert!(
                stepper
                    .stage_precise(
                        (state.owner, state.generation),
                        ticket,
                        stale,
                        &state.registers,
                        40
                    )
                    .is_none()
            );
            assert!(!stepper.pending());
            assert!(
                stepper
                    .stage_precise(
                        (state.owner, state.generation),
                        ticket,
                        fresh,
                        &state.registers,
                        40
                    )
                    .is_some()
            );
            assert!(stepper.pending());
            stepper.cancel();
            assert_eq!(stepper.completed, 0);
            for flags in [0x302, 0x10202] {
                state.registers.rflags = flags;
                assert!(timer_window_eligible(&state, successor));
                assert!(step_instruction(&state, successor).is_none());
            }
            state.mappings.clear();
            assert!(!timer_window_eligible(&state, successor));
        }
    }

    #[test]
    fn successor_fault_and_sud_eligibility_retains_owned_transitions() {
        for kind in [Kind::Cpuid, Kind::Rdtsc, Kind::Rdtscp] {
            let mut code = [0x90; 16];
            code[2..2 + kind.bytes().len()].copy_from_slice(kind.bytes());
            let mut state = step_state(&code);
            let successor = state.registers.instruction_pointer + 2;
            assert!(timer_window_eligible(&state, successor));
            assert!(matches!(step_instruction(&state, successor),
                Some(crate::owned_step::Instruction::Fault(event)) if event.kind == kind));
            state.subscriptions.cpuid = false;
            state.subscriptions.rdtsc = false;
            assert!(!timer_window_eligible(&state, successor));
            state.subscriptions.cpuid = true;
            state.subscriptions.rdtsc = true;
            state.mappings[0].end = successor + kind.bytes().len() as u64;
            assert!(!timer_window_eligible(&state, successor));
        }
        let code = [0x90, 0x90, 0x0f, 0x05, 0x90];
        let mut state = step_state(&code);
        let successor = state.registers.instruction_pointer + 2;
        state.syscalls = true;
        assert!(timer_window_eligible(&state, successor));
        assert!(matches!(
            step_instruction(&state, successor),
            Some(crate::owned_step::Instruction::Syscall)
        ));
        state.syscalls = false;
        assert!(!timer_window_eligible(&state, successor));
        for opcode in [&[0x0f, 0x34][..], &[0x0f, 0x07], &[0x48], &[0xcc]] {
            let mut state = step_state(opcode);
            let successor = state.registers.instruction_pointer;
            state.registers.instruction_pointer = successor - 2;
            assert!(!timer_window_eligible(&state, successor));
        }
    }

    #[test]
    fn subscriptions_remain_owned_but_ordinary_operands_are_cpu_accesses() {
        let code = [0x0f, 0xa2, 0x90];
        let mut state = step_state(&code);
        let pc = state.registers.instruction_pointer;
        state.subscriptions.cpuid = false;
        let mut failure = StepRefusal::new(&state, pc);
        assert!(step_instruction_recorded(&state, pc, &mut failure).is_none());
        assert_eq!(failure.reason.stage, "step/fault-subscription");
        state.subscriptions.cpuid = true;
        state.mappings = vec![pc..pc + 2; 1];
        failure = StepRefusal::new(&state, pc);
        assert!(step_instruction_recorded(&state, pc, &mut failure).is_none());
        assert_eq!(failure.reason.stage, "step/fault-resume-mapping");
        assert_eq!(failure.next, Some(pc + 2));
        let load = [0x48, 0x8b, 0x07, 0x90];
        state = step_state(&load);
        let pc = state.registers.instruction_pointer;
        state.registers.rdi = 0x9000;
        failure = StepRefusal::new(&state, pc);
        assert!(timer_window_eligible(&state, pc));
        assert!(step_instruction(&state, pc).is_some());
        assert!(step_instruction_recorded(&state, pc, &mut failure).is_some());
        assert_eq!(failure.reason.operand, None);
    }

    #[test]
    fn step_report_uses_exact_values_and_only_retained_fetched_bytes() {
        let mut failure = StepRefusal {
            pc: 0xfedc_ba98_7654_3210,
            flags: 0x8000_0000_0001_0202,
            fault_resume: false,
            reason: crate::owned_step::native::Refusal::new("step/native-fetch-mapping"),
            fetched: None,
            next: None,
        };
        let mut records = Vec::new();
        failure.report_with(|stage, detail, value| records.push((stage, detail, value)));
        assert_eq!(
            records,
            [
                ("step/native-fetch-mapping", "refusal", None),
                ("step/native-fetch-mapping", "pc-high32", Some(0xfedc_ba98)),
                ("step/native-fetch-mapping", "pc-low32", Some(0x7654_3210)),
                (
                    "step/native-fetch-mapping",
                    "flags-high32",
                    Some(0x8000_0000)
                ),
                ("step/native-fetch-mapping", "flags-low32", Some(0x10202)),
                ("step/native-fetch-mapping", "fault-resume", Some(0)),
            ]
        );
        let mut bytes = [0xff; 15];
        bytes[..3].copy_from_slice(&[0x48, 0x89, 0xe7]);
        failure.fetched = Some((bytes, 3));
        failure.reason = crate::owned_step::native::Refusal {
            stage: "native/operand-access",
            operand: Some((0x1234_5678_9abc_def0, 8, true)),
        };
        records.clear();
        failure.report_with(|stage, detail, value| records.push((stage, detail, value)));
        assert!(
            records
                .iter()
                .all(|record| record.0 == "native/operand-access")
        );
        assert_eq!(
            &records[6..],
            &[
                ("native/operand-access", "fetched-count", Some(3)),
                ("native/operand-access", "byte-0", Some(0x48)),
                ("native/operand-access", "byte-1", Some(0x89)),
                ("native/operand-access", "byte-2", Some(0xe7)),
                ("native/operand-access", "operand-high32", Some(0x1234_5678)),
                ("native/operand-access", "operand-low32", Some(0x9abc_def0)),
                ("native/operand-access", "operand-width", Some(8)),
                ("native/operand-access", "operand-write", Some(1)),
            ]
        );
    }

    mod rng_connected {
        use super::f1_connected::Frame;
        use super::f1_connected::fixture;
        use super::f1_connected::image;
        use super::f1_connected::registers;
        use super::*;

        #[repr(align(4096))]
        struct RetainedImage([u8; 8192]);

        fn descriptor() -> Box<RetainedImage> {
            let mut image = Box::new(RetainedImage([0; 8192]));
            let bytes = crate::vdso::rng::tests::descriptor();
            image.0.copy_from_slice(&bytes);
            image
        }

        fn capture(
            descriptor: &RetainedImage,
            offset: u64,
            ticket: Option<crate::timer::Ticket>,
            observed_rf: u64,
        ) -> (OwnedContext, crate::owned_step::ReadFaultCapture) {
            capture_for_owner(descriptor, offset, ticket, observed_rf, 17)
        }

        fn capture_for_owner(
            descriptor: &RetainedImage,
            offset: u64,
            ticket: Option<crate::timer::Ticket>,
            observed_rf: u64,
            owner: i64,
        ) -> (OwnedContext, crate::owned_step::ReadFaultCapture) {
            capture_at_clock(descriptor, offset, ticket, observed_rf, owner, 42)
        }

        fn capture_at_clock(
            descriptor: &RetainedImage,
            offset: u64,
            ticket: Option<crate::timer::Ticket>,
            observed_rf: u64,
            owner: i64,
            clock: u64,
        ) -> (OwnedContext, crate::owned_step::ReadFaultCapture) {
            let base = descriptor.0.as_ptr() as u64;
            let mut state = state();
            state.owner = owner;
            state.generation = 3;
            state.vdso = Some(crate::vdso::rng::tests::active_for_owner(base, owner));
            state.registers.instruction_pointer = base + offset;
            state.registers.rflags = 0x602;
            state.registers.rax = 17;
            state.registers.rcx = 0xfedcba9876543210;
            state.registers.stack_pointer = 0x20008;
            assert!(matches!(
                prepare_next_step(&mut state, ticket, 40),
                Ok(true)
            ));
            let mut fault = state.registers;
            fault.rflags |= 0x100 | observed_rf;
            let actual = registers(&fault, 14, 4);
            let captured = state
                .stepper
                .as_mut()
                .unwrap()
                .capture_read_fault(
                    state.owner,
                    state.generation,
                    crate::vdso::Fault {
                        signal: libc::SIGSEGV,
                        code: 2,
                        address: base - 0x4000 + if offset == 0x10be { 8 } else { 0 },
                        registers: &actual,
                    },
                    clock,
                )
                .unwrap();
            state.generation += 1;
            state.registers = snapshot(&actual);
            (state, captured)
        }

        #[test]
        fn modeled_rng_progress_snapshot_edit_resolver_and_current_stage_are_connected() {
            use crate::tool_host::tests::ProgressMode;
            use crate::tool_host::tests::with_progress_callbacks;
            let descriptor = descriptor();
            for offset in [0x10be, 0x110b, 0x13b3] {
                for armed in [false, true] {
                    for mode in [
                        ProgressMode::Preserve,
                        ProgressMode::Rearm(8, 0),
                        ProgressMode::Rearm(0, 0),
                        ProgressMode::Rearm(0, 1),
                    ] {
                        crate::timer::owned_tests::with_paused_model(|| {
                            let owner = unsafe {
                                reverie_preload::trap::raw_syscall6(libc::SYS_gettid, [0; 6])
                            };
                            if armed {
                                crate::timer::open_window(0x4000, 40).unwrap();
                                crate::timer::request_owned(
                                    reverie::TimerSchedule::RcbsAndInstructions(2, 3),
                                    true,
                                )
                                .unwrap();
                                crate::timer::close_window().unwrap();
                            }
                            let old = crate::timer::next_step().unwrap();
                            let (mut state, captured) =
                                capture_for_owner(&descriptor, offset, old, 0x10000, owner);
                            let mut source = Frame([0; 4096]);
                            let mut destination = Frame([0; 4096]);
                            fixture(&mut source, &state.registers);
                            state
                                .native
                                .as_mut()
                                .unwrap()
                                .seal
                                .capture(
                                    &source.0[8..],
                                    source.0.as_ptr() as usize + 8,
                                    state.generation,
                                )
                                .unwrap();
                            let frame = image(&source, &mut destination);
                            let fp = frame.fp_bytes().to_vec();
                            with_progress_callbacks(mode, |progress, snapshot| {
                                let (result, reads) =
                                    crate::runtime::with_guest_clock_read_probe(|| {
                                        run_rng_transition(
                                            &mut state, frame, captured, progress, snapshot,
                                        )
                                    });
                                assert_eq!(reads, 0);
                                let (mut frame, resolution) = result.unwrap();
                                let pc = state.registers.instruction_pointer;
                                assert_eq!(pc, descriptor.0.as_ptr() as u64 + offset + 7);
                                assert_eq!(frame.fp_bytes(), fp);
                                assert_eq!(state.registers.rflags & 0x10100, 0);
                                if offset == 0x110b {
                                    assert_eq!(state.registers.rcx, 43);
                                }
                                assert_eq!(state.stepper.as_ref().unwrap().completed, 0);
                                assert_eq!(state.stepper.as_ref().unwrap().modeled_completed, 1);
                                match mode {
                                    ProgressMode::Preserve => assert_eq!(
                                        resolution,
                                        crate::timer::InterruptionResolution::Preserved(None)
                                    ),
                                    _ => assert_eq!(
                                        resolution,
                                        crate::timer::InterruptionResolution::Replaced
                                    ),
                                }
                                if matches!(mode, ProgressMode::Rearm(0, 0)) {
                                    let position = crate::timer::immediate(pc, captured.clock())
                                        .unwrap()
                                        .unwrap();
                                    assert_eq!(
                                        (position.rip, position.clock, position.sequence),
                                        (pc, 42, 0)
                                    );
                                    assert_eq!(crate::timer::immediate(pc, 42), Ok(None));
                                }
                                let current = crate::timer::next_step().unwrap();
                                if let (Some(old), Some(current)) = (old, current) {
                                    if matches!(mode, ProgressMode::Preserve) {
                                        assert_eq!(old.generation, current.generation);
                                        assert_eq!(current.sequence, old.sequence + 1);
                                    } else {
                                        assert_ne!(old.generation, current.generation);
                                        assert_eq!(current.sequence, 1);
                                    }
                                }
                                assert!(matches!(
                                    prepare_next_step(&mut state, current, 42),
                                    Ok(true)
                                ));
                                frame = frame
                                    .owned_single_step(pc, state.registers.rflags, true)
                                    .unwrap();
                                state
                                    .native
                                    .as_mut()
                                    .unwrap()
                                    .seal
                                    .finish(&frame, &state.registers, state.generation, true)
                                    .unwrap();
                            });
                        });
                    }
                }
            }
        }

        #[test]
        fn modeled_rng_dispatch_boundary_authenticates_retained_provenance_without_clock_read() {
            use crate::tool_host::tests::ProgressMode;
            use crate::tool_host::tests::with_progress_callbacks;
            let descriptor = descriptor();
            crate::timer::owned_tests::with_paused_model(|| {
                let owner = syscall(libc::SYS_gettid, [0; 6]);
                for offset in [0x10be, 0x110b, 0x13b3] {
                    let (mut state, captured) =
                        capture_for_owner(&descriptor, offset, None, 0x10000, owner);
                    with_progress_callbacks(ProgressMode::Preserve, |progress, snapshot| {
                        let provenance = GuestProgressCapture {
                            stepper: state.stepper.as_ref().unwrap(),
                            captured,
                            owner: state.owner,
                            generation: state.generation,
                        };
                        let (result, reads) =
                            crate::runtime::with_guest_clock_read_probe(|| unsafe {
                                crate::runtime::dispatch_owned_guest_progress(
                                    provenance,
                                    &mut state.registers,
                                    progress,
                                )
                            });
                        assert!(result.is_ok());
                        assert_eq!(reads, 0);
                        assert_eq!(snapshot(owner).unwrap().generation, 43);
                        assert_eq!(state.stepper.as_ref().unwrap().modeled_completed, 0);
                    });
                }
            });
        }

        #[test]
        fn modeled_rng_dispatch_boundary_refuses_invalid_provenance_before_tool_or_clock() {
            let descriptor = descriptor();
            crate::timer::owned_tests::with_paused_model(|| {
                let owner = syscall(libc::SYS_gettid, [0; 6]);
                for defect in [
                    "owner", "frame", "pending", "sequence", "count", "pc", "gpr", "flags",
                    "running", "inactive",
                ] {
                    let (mut state, captured) =
                        capture_for_owner(&descriptor, 0x10be, None, 0x10000, owner);
                    let original_context = state.registers;
                    let saved_control = crate::clock_control::test_control::snapshot();
                    let mut provided = captured;
                    match defect {
                        "owner" => state.owner += 1,
                        "frame" => state.generation += 1,
                        "pending" => state.stepper.as_mut().unwrap().cancel(),
                        "sequence" => {
                            let mut start = state.registers;
                            start.rflags &= !0x10100;
                            let actual = registers(&state.registers, 14, 4);
                            let stepper = state.stepper.as_mut().unwrap();
                            stepper.cancel();
                            stepper
                                .stage_cpu(
                                    (owner, state.generation - 1),
                                    None,
                                    captured.request(),
                                    &start,
                                    40,
                                )
                                .unwrap();
                            let replacement = stepper
                                .capture_read_fault(
                                    owner,
                                    state.generation - 1,
                                    crate::vdso::Fault {
                                        signal: libc::SIGSEGV,
                                        code: 2,
                                        address: captured.address(),
                                        registers: &actual,
                                    },
                                    42,
                                )
                                .unwrap();
                            assert_ne!(
                                replacement.identity().cpu_sequence,
                                captured.identity().cpu_sequence
                            );
                        }
                        "count" => {
                            provided =
                                capture_at_clock(&descriptor, 0x10be, None, 0x10000, owner, 43).1
                        }
                        "pc" => state.registers.instruction_pointer += 7,
                        "gpr" => state.registers.rax ^= 1,
                        "flags" => state.registers.rflags ^= 0x10000,
                        "running" => crate::clock_control::test_control::set_running(1),
                        "inactive" => crate::clock_control::test_control::set_ready(0),
                        _ => unreachable!(),
                    }
                    let provenance = GuestProgressCapture {
                        stepper: state.stepper.as_ref().unwrap(),
                        captured: provided,
                        owner: state.owner,
                        generation: state.generation,
                    };
                    let before = crate::owned_step::native::general(&state.registers);
                    let flags = state.registers.rflags;
                    let pc = state.registers.instruction_pointer;
                    let (result, reads) = crate::runtime::with_guest_clock_read_probe(|| unsafe {
                        crate::runtime::dispatch_owned_guest_progress(
                            provenance,
                            &mut state.registers,
                            |_, _, _| panic!("invalid provenance reached Tool: {defect}"),
                        )
                    });
                    crate::clock_control::test_control::restore(saved_control);
                    assert!(result.is_err(), "{defect}");
                    assert_eq!(reads, 0, "{defect}");
                    assert_eq!(crate::owned_step::native::general(&state.registers), before);
                    assert_eq!(
                        (state.registers.rflags, state.registers.instruction_pointer),
                        (flags, pc)
                    );
                    assert_eq!(state.stepper.as_ref().unwrap().modeled_completed, 0);
                    if defect == "count" {
                        assert!(captured.matches_context(&original_context));
                        assert_eq!(captured.clock(), 42);
                        assert_eq!(provided.clock(), 43);
                        assert!(
                            state
                                .stepper
                                .as_ref()
                                .unwrap()
                                .validate_modeled(captured, owner, state.generation)
                                .is_some()
                        );
                    }
                }
            });
        }

        #[test]
        fn modeled_rng_clock_probe_detects_provider_access_without_hardware() {
            let (result, reads) =
                crate::runtime::with_guest_clock_read_probe(crate::runtime::read_guest_rcb_clock);
            assert!(result.is_err());
            assert_eq!(reads, 1);
        }

        #[test]
        fn modeled_rng_callback_error_and_postawait_mutation_poison_without_edit() {
            use crate::tool_host::tests::ProgressMode;
            use crate::tool_host::tests::with_progress_callbacks;
            let descriptor = descriptor();
            for mode in [ProgressMode::Fail, ProgressMode::Mutate] {
                crate::timer::owned_tests::with_paused_model(|| {
                    let owner =
                        unsafe { reverie_preload::trap::raw_syscall6(libc::SYS_gettid, [0; 6]) };
                    let (mut state, captured) =
                        capture_for_owner(&descriptor, 0x110b, None, 0x10000, owner);
                    let mut source = Frame([0; 4096]);
                    let mut destination = Frame([0; 4096]);
                    fixture(&mut source, &state.registers);
                    state
                        .native
                        .as_mut()
                        .unwrap()
                        .seal
                        .capture(
                            &source.0[8..],
                            source.0.as_ptr() as usize + 8,
                            state.generation,
                        )
                        .unwrap();
                    let frame = image(&source, &mut destination);
                    let original = frame.frame_bytes().to_vec();
                    with_progress_callbacks(mode, |progress, _snapshot| {
                        let called = std::cell::Cell::new(false);
                        let result =
                            run_rng_transition(&mut state, frame, captured, progress, |_| {
                                called.set(true);
                                panic!("snapshot after failed progress/revalidation")
                            });
                        assert!(result.is_err());
                        assert!(!called.get());
                        assert_eq!(state.stepper.as_ref().unwrap().modeled_completed, 0);
                        assert_eq!(crate::timer::next_step(), Err(reverie::Errno::EBUSY));
                        assert_eq!(
                            crate::timer::open_window(captured.request().pc, captured.clock()),
                            Err(reverie::Errno::ECANCELED)
                        );
                    });
                    assert_eq!(&destination.0[8..8 + original.len()], original.as_slice());
                });
            }
        }

        #[test]
        fn modeled_rng_unwind_after_progress_rearm_poison_is_automatic() {
            use crate::tool_host::tests::ProgressMode;
            use crate::tool_host::tests::with_progress_callbacks;
            crate::timer::owned_tests::with_paused_model(|| {
                let descriptor = descriptor();
                let owner =
                    unsafe { reverie_preload::trap::raw_syscall6(libc::SYS_gettid, [0; 6]) };
                let (mut state, captured) =
                    capture_for_owner(&descriptor, 0x110b, None, 0x10000, owner);
                let mut source = Frame([0; 4096]);
                let mut destination = Frame([0; 4096]);
                fixture(&mut source, &state.registers);
                state
                    .native
                    .as_mut()
                    .unwrap()
                    .seal
                    .capture(
                        &source.0[8..],
                        source.0.as_ptr() as usize + 8,
                        state.generation,
                    )
                    .unwrap();
                let frame = image(&source, &mut destination);
                with_progress_callbacks(ProgressMode::Rearm(8, 0), |progress, snapshot| {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _ = run_rng_transition(
                            &mut state,
                            frame,
                            captured,
                            |owner, context, clock| {
                                progress(owner, context, clock)?;
                                panic!("injected unwind after genuine Tool callback")
                            },
                            snapshot,
                        );
                    }));
                    assert!(result.is_err());
                    assert_eq!(state.stepper.as_ref().unwrap().modeled_completed, 0);
                    assert_eq!(crate::timer::next_step(), Err(reverie::Errno::EBUSY));
                    assert_eq!(
                        crate::timer::open_window(captured.request().pc, captured.clock()),
                        Err(reverie::Errno::ECANCELED)
                    );
                });
            });
        }

        #[test]
        fn modeled_rng_actual_helpers_compose_capture_binding_editor_seal_and_fresh_stage() {
            let descriptor = descriptor();
            for offset in [0x10be, 0x110b, 0x13b3] {
                for (armed, observed_rf) in
                    [(false, 0), (true, 0), (false, 0x10000), (true, 0x10000)]
                {
                    let mut controller = reverie_preload::precise_timer::Controller::default();
                    let (generation, _) = controller.replace(40, 2, 1).unwrap();
                    let ticket = armed.then_some(crate::timer::Ticket {
                        generation,
                        sequence: 1,
                    });
                    let (mut state, captured) = capture(&descriptor, offset, ticket, observed_rf);
                    let before = crate::owned_step::native::general(&state.registers);
                    let mut source = Frame([0; 4096]);
                    let mut destination = Frame([0; 4096]);
                    fixture(&mut source, &state.registers);
                    let address = source.0.as_ptr() as usize + 8;
                    state
                        .native
                        .as_mut()
                        .unwrap()
                        .seal
                        .capture(&source.0[8..], address, state.generation)
                        .unwrap();
                    let frame = image(&source, &mut destination);
                    let fp = frame.fp_bytes().to_vec();
                    assert!(
                        state
                            .native
                            .as_ref()
                            .unwrap()
                            .seal
                            .relocated(&frame, state.generation)
                    );
                    assert_eq!(captured.request().input_flags(), 0x602);
                    assert_eq!(state.registers.rflags, 0x702 | observed_rf);
                    assert!(admit_rng_read(&state, captured).is_ok());
                    let (frame, observation) = complete_rng_read(
                        &mut state,
                        frame,
                        captured,
                        reverie::vdso::VdsoRngSnapshot {
                            ready: true,
                            generation: 17,
                        },
                    )
                    .unwrap();
                    assert_eq!(
                        state.registers.instruction_pointer,
                        descriptor.0.as_ptr() as u64 + offset + 7
                    );
                    assert_eq!(state.registers.rflags & 0x10100, 0);
                    assert_eq!(state.registers.rflags & 0x400, 0x400);
                    assert_eq!(frame.fp_bytes(), fp);
                    for (index, actual) in crate::owned_step::native::general(&state.registers)
                        .into_iter()
                        .enumerate()
                    {
                        assert_eq!(
                            actual,
                            if offset == 0x110b && index == 14 {
                                17
                            } else {
                                before[index]
                            }
                        );
                    }
                    if let Some(observation) = observation {
                        assert!(armed);
                        assert_eq!((observation.clock, observation.sequence), (42, 1));
                        assert_eq!(
                            controller.observe(observation).unwrap(),
                            reverie_preload::precise_timer::Decision::Step
                        );
                    } else {
                        assert!(!armed);
                    }
                    assert_eq!(state.stepper.as_ref().unwrap().completed, 0);
                    assert_eq!(state.stepper.as_ref().unwrap().modeled_completed, 1);
                    assert!(admit_rng_read(&state, captured).is_err());
                    assert!(matches!(prepare_next_step(&mut state, None, 42), Ok(true)));
                    let frame = frame
                        .owned_single_step(
                            state.registers.instruction_pointer,
                            state.registers.rflags,
                            true,
                        )
                        .unwrap();
                    state
                        .native
                        .as_mut()
                        .unwrap()
                        .seal
                        .finish(&frame, &state.registers, state.generation, true)
                        .unwrap();
                    state
                        .native
                        .as_mut()
                        .unwrap()
                        .seal
                        .retire_verified()
                        .unwrap();
                }
            }
        }

        #[test]
        fn modeled_rng_binding_refuses_stale_fault_state_without_normalizing_actual_flags() {
            let descriptor = descriptor();
            for mutation in 0..5 {
                let (mut state, captured) = capture(&descriptor, 0x110b, None, 0x10000);
                match mutation {
                    0 => state.owner += 1,
                    1 => state.generation += 1,
                    2 => state.registers.rflags = captured.request().input_flags(),
                    3 => state.registers.rcx ^= 1,
                    4 => state.stepper.as_mut().unwrap().cancel(),
                    _ => unreachable!(),
                }
                let before = state.registers;
                assert!(admit_rng_read(&state, captured).is_err());
                assert_eq!(
                    crate::owned_step::native::general(&state.registers),
                    crate::owned_step::native::general(&before)
                );
                assert_eq!(state.registers.rflags, before.rflags);
                assert_eq!(state.stepper.as_ref().unwrap().modeled_completed, 0);
            }
        }
    }

    mod f1_connected {
        use reverie_preload::signal::native_frame::Format;
        use reverie_preload::signal::native_frame::FunctionReturn;
        use reverie_preload::signal::native_frame::InstructionResult;

        use super::*;
        use crate::owned_step::native;

        #[repr(align(64))]
        pub(super) struct Frame(pub(super) [u8; 4096]);

        fn put(bytes: &mut [u8], offset: usize, value: u64) {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }

        pub(super) fn fixture(source: &mut Frame, context: &HookContext) {
            let pointer = source.0.as_ptr() as u64;
            put(&mut source.0, 8, 0x7000);
            put(&mut source.0, 16, 7);
            for (field, value) in native::general(context).into_iter().enumerate() {
                put(&mut source.0, 56 + field * 8, value);
            }
            put(&mut source.0, 184, context.instruction_pointer);
            put(&mut source.0, 192, context.rflags);
            put(&mut source.0, 240, pointer + 512);
            source.0[976..980].copy_from_slice(&0x46505853u32.to_le_bytes());
            source.0[980..984].copy_from_slice(&2444u32.to_le_bytes());
            put(&mut source.0, 984, 0x2e7);
            source.0[992..996].copy_from_slice(&2440u32.to_le_bytes());
            put(&mut source.0, 1024, 0x2e7);
            source.0[2952..2956].copy_from_slice(&0x46505845u32.to_le_bytes());
        }

        pub(super) fn image<'frame>(
            source: &Frame,
            storage: &'frame mut Frame,
        ) -> RelocatedFrame<'frame> {
            let address = source.0.as_ptr() as usize + 8;
            relocate(
                &source.0,
                address,
                address + 8,
                Format::StandardXsave {
                    xfeatures: 0x2e7,
                    xstate_size: 2440,
                },
                &mut storage.0,
            )
            .unwrap()
        }

        pub(super) fn registers(
            context: &HookContext,
            trap: i64,
            error: i64,
        ) -> [libc::greg_t; 23] {
            let mut registers = [0; 23];
            for (slot, value) in registers.iter_mut().zip(native::general(context)) {
                *slot = value as i64;
            }
            registers[libc::REG_RIP as usize] = context.instruction_pointer as i64;
            registers[libc::REG_EFL as usize] = context.rflags as i64;
            registers[libc::REG_TRAPNO as usize] = trap;
            registers[libc::REG_ERR as usize] = error;
            registers
        }

        fn ticket(present: bool) -> Option<crate::timer::Ticket> {
            if !present {
                return None;
            }
            let mut controller = reverie_preload::precise_timer::Controller::default();
            let (generation, _) = controller.replace(40, 0, 1).unwrap();
            Some(crate::timer::Ticket {
                generation,
                sequence: 1,
            })
        }

        fn setup(code: &[u8], length: usize) -> OwnedContext {
            let mut state = step_state(code);
            let pc = state.registers.instruction_pointer;
            state.generation = 3;
            state.registers.stack_pointer = 0x20008;
            state.registers.r8 = 0x11223344;
            state.registers.r12 = 0xaabbccdd;
            state.vdso = Some(crate::vdso::Active::stepping_model(
                pc..pc + code.len() as u64,
                pc + length as u64,
            ));
            state.mappings.clear();
            state
        }

        fn call_then_stage(
            state: &mut OwnedContext,
            ticket: Option<crate::timer::Ticket>,
            target: u64,
        ) {
            let entry = state.registers.instruction_pointer;
            assert!(state.vdso.as_ref().unwrap().operation(entry).is_some());
            assert!(!state.vdso.as_ref().unwrap().native_pc(entry));
            assert!(step_instruction(state, entry).is_none());
            assert!(
                matches!(prepare_next_step(state, ticket, 40), Ok(prepared) if prepared == ticket.is_some())
            );
            assert_eq!(state.stepper.as_ref().unwrap().pending(), ticket.is_some());
            let mut fault_context = state.registers;
            fault_context.rflags |= 0x10000 | if ticket.is_some() { 0x100 } else { 0 };
            let fault_registers = registers(&fault_context, 14, 0x15);
            if ticket.is_some() {
                let stepper = state.stepper.as_mut().unwrap();
                assert!(
                    stepper
                        .validate_call(state.owner + 1, state.generation, &fault_registers)
                        .is_none()
                );
                assert!(
                    stepper
                        .validate_call(state.owner, state.generation + 1, &fault_registers)
                        .is_none()
                );
                let mut stale = fault_registers;
                stale[libc::REG_R8 as usize] ^= 1;
                assert!(
                    stepper
                        .validate_call(state.owner, state.generation, &stale)
                        .is_none()
                );
                assert!(
                    stepper
                        .validate_call(state.owner, state.generation, &fault_registers)
                        .is_some()
                );
                assert!(stepper.cancel_call(41).is_none());
                stepper.cancel_call(40).unwrap();
            }
            let call = admit_vdso_call(
                state,
                crate::vdso::Fault {
                    signal: libc::SIGSEGV,
                    code: 2,
                    address: entry,
                    registers: &fault_registers,
                },
                target,
            )
            .expect("F1 connected capture caller must admit retained return");
            let mut source = Frame([0; 4096]);
            fixture(&mut source, &fault_context);
            let original_source = source.0;
            let mut storage = Frame([0; 4096]);
            let returned = FunctionReturn {
                entry: call.entry,
                stack: call.stack,
                target: call.target,
                flags: fault_context.rflags,
                result: 12345,
                owned_tf: ticket.is_some(),
            };
            for invalid in [
                FunctionReturn {
                    stack: call.stack + 8,
                    ..returned
                },
                FunctionReturn {
                    entry: call.entry + 1,
                    ..returned
                },
                FunctionReturn {
                    flags: returned.flags ^ 1,
                    ..returned
                },
                FunctionReturn {
                    owned_tf: !returned.owned_tf,
                    ..returned
                },
            ] {
                assert!(
                    image(&source, &mut storage)
                        .complete_vdso_call(invalid)
                        .is_err()
                );
            }
            let frame = image(&source, &mut storage);
            let fp = frame.fp_bytes().to_vec();
            let mut expected = frame.frame_bytes().to_vec();
            put(&mut expected, 152, returned.result as u64);
            put(&mut expected, 168, call.stack + 8);
            put(&mut expected, 176, call.target);
            put(&mut expected, 184, returned.flags & !0x10100);
            let frame = frame.complete_vdso_call(returned).unwrap();
            assert_eq!(frame.frame_bytes(), expected);
            assert_eq!(frame.fp_bytes(), fp);
            assert!(frame.complete_vdso_call(returned).is_err());
            assert_eq!(source.0, original_source);
            state.registers.instruction_pointer = call.target;
            state.registers.stack_pointer = call.stack + 8;
            state.registers.rax = returned.result as u64;
            state.registers.rflags = returned.flags & !0x10100;
            state.generation += 1;
            assert!(
                matches!(prepare_next_step(state, ticket, 40), Ok(true)),
                "F1 connected retained caller staging"
            );
            assert!(state.stepper.as_ref().unwrap().pending());
            assert!(matches!(
                prepare_next_step(state, ticket, 40),
                Err(StepPreparationFailure::Predicate)
            ));
            verify_caller_seal(state);
        }

        fn verify_caller_seal(state: &OwnedContext) {
            let mut source = Frame([0; 4096]);
            fixture(&mut source, &state.registers);
            let mut seal = native_step::Seal::default();
            seal.capture(
                &source.0[8..],
                source.0.as_ptr() as usize + 8,
                state.generation,
            )
            .unwrap();
            let mut storage = Frame([0; 4096]);
            source.0[512 + 160] ^= 1;
            let changed = image(&source, &mut storage)
                .owned_single_step(
                    state.registers.instruction_pointer,
                    state.registers.rflags,
                    true,
                )
                .unwrap();
            assert!(
                seal.finish(&changed, &state.registers, state.generation, true)
                    .is_none()
            );
            source.0[512 + 160] ^= 1;
            let frame = image(&source, &mut storage)
                .owned_single_step(
                    state.registers.instruction_pointer,
                    state.registers.rflags,
                    true,
                )
                .unwrap();
            assert!(
                seal.finish(&frame, &state.registers, state.generation + 1, true)
                    .is_none()
            );
            let mut stale = state.registers;
            stale.rax ^= 1;
            assert!(
                seal.finish(&frame, &stale, state.generation, true)
                    .is_none()
            );
            seal.finish(&frame, &state.registers, state.generation, true)
                .unwrap();
            assert!(
                seal.finish(&frame, &state.registers, state.generation, true)
                    .is_none()
            );
        }

        fn instruction_chain(kind: Kind) {
            for present in [false, true] {
                let mut code = [0x90; 48];
                code[..kind.bytes().len()].copy_from_slice(kind.bytes());
                let mut state = setup(&code, kind.bytes().len());
                state.precise_timer = present;
                let pc = state.registers.instruction_pointer;
                let ticket = ticket(present);
                assert!(
                    matches!(prepare_next_step(&mut state, ticket, 40), Ok(true)),
                    "F1 connected mediated predecessor preparation"
                );
                assert_eq!(state.stepper.as_ref().unwrap().pending(), present);
                let event = InstructionEvent::admit(
                    pc,
                    &code,
                    execution_ranges(&state, pc).first().unwrap(),
                )
                .unwrap();
                state.registers.rflags |= 0x10000 | if present { 0x100 } else { 0 };
                let captured = registers(&state.registers, 13, 0);
                if present {
                    let stepper = state.stepper.as_mut().unwrap();
                    assert!(
                        stepper
                            .validate_fault(state.owner + 1, state.generation, &captured)
                            .is_none()
                    );
                    assert_eq!(
                        stepper.validate_fault(state.owner, state.generation, &captured),
                        Some(event)
                    );
                    stepper.cancel_fault(40).unwrap();
                }
                let mut source = Frame([0; 4096]);
                fixture(&mut source, &state.registers);
                let original_source = source.0;
                let mut storage = Frame([0; 4096]);
                let mut frame = image(&source, &mut storage);
                let fp = frame.fp_bytes().to_vec();
                if present {
                    frame = frame
                        .owned_single_step(pc, state.registers.rflags, false)
                        .unwrap();
                    state.registers.rflags &= !0x100;
                }
                let result = match kind {
                    Kind::Cpuid => InstructionResult::Cpuid(reverie::CpuIdResult {
                        eax: 0x1234,
                        ebx: 0x5678,
                        ecx: 0x9abc,
                        edx: 0xdef0,
                    }),
                    Kind::Rdtsc | Kind::Rdtscp => InstructionResult::Rdtsc {
                        request: if kind == Kind::Rdtsc {
                            reverie::Rdtsc::Tsc
                        } else {
                            reverie::Rdtsc::Tscp
                        },
                        result: reverie::RdtscResult {
                            tsc: 0x123456789abcdef0,
                            aux: if kind == Kind::Rdtscp { Some(7) } else { None },
                        },
                    },
                };
                crate::instruction_event::apply_result(&mut state.registers, result);
                let frame =
                    complete_owned_emulation(frame, &mut state.registers, event, result).unwrap();
                assert_eq!(frame.fp_bytes(), fp);
                assert_eq!(source.0, original_source);
                assert_eq!(state.registers.instruction_pointer, event.resume_pc);
                assert_eq!(state.registers.rflags, 0x202);
                for (index, value) in native::general(&state.registers).into_iter().enumerate() {
                    assert_eq!(
                        u64::from_le_bytes(
                            frame.frame_bytes()[48 + index * 8..56 + index * 8]
                                .try_into()
                                .unwrap()
                        ),
                        value
                    );
                }
                call_then_stage(&mut state, ticket, pc + 24);
            }
        }

        #[test]
        fn cpuid_completion_call_return_and_staging() {
            instruction_chain(Kind::Cpuid);
        }
        #[test]
        fn rdtsc_completion_call_return_and_staging() {
            instruction_chain(Kind::Rdtsc);
        }
        #[test]
        fn rdtscp_completion_call_return_and_staging() {
            instruction_chain(Kind::Rdtscp);
        }

        #[test]
        fn syscall_completion_call_return_and_staging() {
            for present in [false, true] {
                let mut code = [0x90; 48];
                code[..2].copy_from_slice(&[0x0f, 0x05]);
                let mut state = setup(&code, 2);
                state.precise_timer = present;
                let pc = state.registers.instruction_pointer;
                let ticket = ticket(present);
                assert!(
                    matches!(prepare_next_step(&mut state, ticket, 40), Ok(true)),
                    "F1 connected separate syscall successor check"
                );
                assert_eq!(state.stepper.as_ref().unwrap().pending(), present);
                state.registers.instruction_pointer = pc + 2;
                state.registers.rcx = pc + 2;
                state.registers.rflags |= if present { 0x100 } else { 0 };
                state.registers.r11 = state.registers.rflags;
                let captured = registers(&state.registers, 0, 0);
                let mut metadata: crate::syscall_event::Metadata = unsafe { core::mem::zeroed() };
                metadata.signal = libc::SIGSYS;
                metadata.code = 2;
                metadata.call_address = pc + 2;
                metadata.number = libc::SYS_read as i32;
                metadata.arch = 0xc000003e;
                let event = crate::syscall_event::SyscallEvent::admit(
                    metadata,
                    &captured,
                    &code,
                    &state.vdso.as_ref().unwrap().range,
                )
                .unwrap();
                if present {
                    let stepper = state.stepper.as_mut().unwrap();
                    assert!(
                        stepper
                            .validate_syscall(
                                state.owner + 1,
                                state.generation,
                                event.number,
                                &captured
                            )
                            .is_none()
                    );
                    stepper
                        .validate_syscall(state.owner, state.generation, event.number, &captured)
                        .unwrap();
                    stepper.cancel_syscall(40).unwrap();
                }
                let mut source = Frame([0; 4096]);
                fixture(&mut source, &state.registers);
                let original_source = source.0;
                let mut storage = Frame([0; 4096]);
                let mut frame = image(&source, &mut storage);
                let fp = frame.fp_bytes().to_vec();
                if present {
                    frame = frame
                        .owned_syscall_tf(event.resume, state.registers.rflags, state.registers.r11)
                        .unwrap();
                    state.registers.rflags &= !0x100;
                    state.registers.r11 &= !0x100;
                }
                state.registers.rax = 7;
                let frame = frame.complete_syscall(event.resume, 7).unwrap();
                assert_eq!(
                    u64::from_le_bytes(frame.frame_bytes()[152..160].try_into().unwrap()),
                    7
                );
                assert_eq!(frame.fp_bytes(), fp);
                assert_eq!(source.0, original_source);
                call_then_stage(&mut state, ticket, pc + 24);
            }
        }

        #[test]
        fn recognized_return_calls_actual_capture_helper_and_stages_caller() {
            for present in [false, true] {
                let code = [0x90; 48];
                let mut state = setup(&code, 2);
                let pc = state.registers.instruction_pointer;
                state.precise_timer = present;
                state.registers.instruction_pointer += 2;
                call_then_stage(&mut state, ticket(present), pc + 24);
            }
        }

        #[test]
        fn invalid_targets_and_stale_staging_remain_refused() {
            let code = [0x90; 48];
            let mut state = setup(&code, 2);
            let pc = state.registers.instruction_pointer;
            for target in [pc - 1, pc + 48, 0, 1 << 47] {
                state.registers.instruction_pointer = target;
                assert!(prepare_next_step(&mut state, ticket(true), 40).is_err());
                assert!(!state.stepper.as_ref().unwrap().pending());
                let mut context = state.registers;
                context.instruction_pointer = pc + 2;
                context.rflags = 0x10202;
                let captured = registers(&context, 14, 0x15);
                assert!(
                    admit_vdso_call(
                        &state,
                        crate::vdso::Fault {
                            signal: libc::SIGSEGV,
                            code: 2,
                            address: pc + 2,
                            registers: &captured
                        },
                        target
                    )
                    .is_none()
                );
            }
            state.registers.instruction_pointer = pc + 24;
            state.owner = 0;
            assert!(matches!(
                prepare_next_step(&mut state, None, 40),
                Err(StepPreparationFailure::Predicate)
            ));
            state.owner = 1;
            state.generation = 0;
            assert!(matches!(
                prepare_next_step(&mut state, None, 40),
                Err(StepPreparationFailure::Predicate)
            ));
            state.generation = 3;
            state.registers.rflags |= 0x100;
            assert!(prepare_next_step(&mut state, None, 40).is_err());
            state.registers.rflags = 0x202;
            let request = match step_instruction(&state, pc + 24).unwrap() {
                crate::owned_step::Instruction::Native(request) => request,
                _ => panic!("expected native caller"),
            };
            state.registers.rax ^= 1;
            assert!(
                state
                    .stepper
                    .as_mut()
                    .unwrap()
                    .stage_cpu(
                        (state.owner, state.generation),
                        None,
                        *request,
                        &state.registers,
                        40
                    )
                    .is_none()
            );
        }
    }

    fn state() -> OwnedContext {
        OwnedContext {
            owner: 1,
            mappings: Vec::new(),
            readable: Vec::new(),
            writable: Vec::new(),
            vdso: None,
            signal_stack: 0..0,
            runtime_sp: 0,
            runtime_stack: 0..0,
            storage: ptr::null_mut(),
            image: None,
            registers: unsafe { core::mem::zeroed() },
            event: None,
            stepper: Some(Stepper::default()),
            precise_timer: true,
            syscalls: true,
            native: Some(native_step::NativeState::new(0).unwrap()),
            guest_mask: 0,
            subscriptions: crate::runtime::InstructionSubscriptions {
                cpuid: true,
                rdtsc: true,
            },
            clocked: true,
            controls: Controls {
                segments_and_permissions: [0; 3],
                xcr0: 0,
                pkru: 0,
                cpuid: 0,
                tsc: 0,
            },
            errno: ptr::null_mut(),
            saved_errno: 0,
            generation: 0,
        }
    }

    fn observation(mask: u64, phase: EnvironmentPhase) -> InitialObservation {
        InitialObservation {
            phase: IDLE,
            mask: mask_matches_phase(mask, phase).then_some(mask),
            paused: true,
            handoff_clear: true,
            owner: 1,
        }
    }

    #[test]
    fn preparation_preserves_guest_mask_instead_of_requiring_ordinary_mask() {
        let state = state();
        for mask in [0, 1 << (libc::SIGUSR1 - 1)] {
            assert_eq!(
                validate_initial_preparation(
                    &state,
                    observation(mask, EnvironmentPhase::InitialGuest),
                    || Ok(0)
                )
                .unwrap(),
                mask
            );
            assert!(
                validate_initial_preparation(
                    &state,
                    observation(mask, EnvironmentPhase::Ordinary),
                    || Ok(0)
                )
                .is_err()
            );
        }
        let ordinary = crate::syscall_fallback::runtime_mask()
            & !((1 << (libc::SIGKILL - 1)) | (1 << (libc::SIGSTOP - 1)));
        assert!(mask_matches_phase(ordinary, EnvironmentPhase::Ordinary));
        assert!(!mask_matches_phase(
            ordinary,
            EnvironmentPhase::InitialGuest
        ));
        for signal in [libc::SIGSYS, libc::SIGSEGV, libc::SIGTRAP] {
            assert!(
                validate_initial_preparation(
                    &state,
                    observation(1 << (signal - 1), EnvironmentPhase::InitialGuest),
                    || Ok(0)
                )
                .is_err()
            );
        }
    }

    #[test]
    fn preparation_keeps_owner_environment_phase_and_clock_checks() {
        let mut state = state();
        for phase in [CAPTURED, RUNTIME, RETURNING, FAILED] {
            let mut observed = observation(0, EnvironmentPhase::InitialGuest);
            observed.phase = phase;
            assert!(
                validate_initial_preparation(&state, observed, || panic!(
                    "invalid phase read clock"
                ))
                .is_err()
            );
        }
        for change in 0..4 {
            let mut observed = observation(0, EnvironmentPhase::InitialGuest);
            match change {
                0 => observed.mask = None,
                1 => observed.paused = false,
                2 => observed.handoff_clear = false,
                _ => observed.owner = 2,
            }
            assert!(
                validate_initial_preparation(&state, observed, || panic!(
                    "failed ownership read clock"
                ))
                .is_err()
            );
        }
        assert!(
            validate_initial_preparation(
                &state,
                observation(0, EnvironmentPhase::InitialGuest),
                || Ok(1)
            )
            .is_err()
        );
        assert_eq!(
            validate_initial_preparation(
                &state,
                observation(0, EnvironmentPhase::InitialGuest),
                || Err(io::Error::from_raw_os_error(libc::EIO))
            )
            .unwrap_err()
            .raw_os_error(),
            Some(libc::EIO)
        );
        state.generation = 1;
        assert!(
            validate_initial_preparation(
                &state,
                observation(0, EnvironmentPhase::InitialGuest),
                || panic!("noninitial generation read clock")
            )
            .is_err()
        );
    }
}
