//! Real stopped-task adapter for the explicitly selected one-task experiment.
use std::collections::BTreeSet;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::{self};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use goblin::elf::Elf;
use goblin::elf::header;
use goblin::elf::program_header as ph;

use super::*;
use crate::LiteinstAfterLoaderConfig;
use crate::LiteinstCallerImage;
use crate::entry_call::CallCode;
use crate::entry_call::Calls;
use crate::entry_call::ImageIdentity;
use crate::entry_call::Phase as CallsPhase;
use crate::entry_call::TrapObservation;

const STACK_SIZE: u64 = 64 * 1024;
const PAGE: u64 = 4096;
const MAX_STACK_SNAPSHOT: usize = 1024 * 1024;
const MAX_PRIVATE_READ: u64 = 1024 * 1024;
const MAX_PRIVATE_MMAP_EFFECT: u64 = crate::after_loader::MAX_RUNTIME_LOAD_SPAN;
const RETURN_MARKER: u64 = 0x4c49_4341_4c4c_0001;
const TRAMPOLINE_ARENA_SIZE: u64 = 128 * PAGE;
const KERNEL_O_LARGEFILE: u64 = 0o100000;
const PRIVATE_POLICY_SCRATCH_OFFSET: u64 = 2560;
const PRIVATE_POLICY_SCRATCH_BYTES: usize = 8;
const ARCH_SHSTK_STATUS: u64 = 0x5005;
const STATX_OUTPUT_BYTES: usize = std::mem::size_of::<libc::statx>();
const PRIVATE_STATX_MASK: u32 = libc::STATX_BASIC_STATS | libc::STATX_BTIME;
const MFD_ALLOW_SEALING: u64 = 0x0002;
const TRAMPOLINE_SEALS: i32 =
    libc::F_SEAL_FUTURE_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;

/// Decode the two canonical register representations of a C `int` argument.
///
/// Bound libc code can materialize a negative `int` by either writing a
/// 32-bit register (zero extension) or by sign-extending it to the syscall
/// register width. Refuse every other high word so private-syscall admission
/// remains stricter than the kernel's truncating `int` conversion.
fn canonical_c_int_argument(raw: u64) -> Option<i32> {
    let low = raw as u32;
    let zero_extended = u64::from(low);
    let sign_extended = i64::from(low as i32) as u64;
    (raw == zero_extended || raw == sign_extended).then_some(low as i32)
}

fn canonical_c_int_argument_is(raw: u64, expected: i32) -> bool {
    canonical_c_int_argument(raw) == Some(expected)
}

fn classify_trace_only_syscall_number(raw: u64) -> Result<(i64, Option<Sysno>), Errno> {
    let semantic = canonical_c_int_argument(raw).ok_or(Errno::ENOSYS)?;
    if semantic >= 0 && semantic as u64 & X32_SYSCALL_BIT != 0 {
        return Err(Errno::ENOSYS);
    }
    let known = usize::try_from(semantic).ok().and_then(Sysno::new);
    Ok((raw as i64, known))
}

fn controller_semantic_arguments_match(number: i64, args: [u64; 6]) -> bool {
    match number {
        libc::SYS_arch_prctl => args[0] == ARCH_SHSTK_STATUS,
        libc::SYS_sigaltstack => args[0] == 0,
        libc::SYS_rt_sigaction => (1..=64).contains(&args[0]) && args[1] == 0 && args[3] == 8,
        libc::SYS_rt_sigprocmask => {
            args[0] == libc::SIG_SETMASK as u64 && args[1] == 0 && args[3] == 8
        }
        libc::SYS_brk => args[0] == 0,
        libc::SYS_fcntl => canonical_c_int_argument_is(args[1], libc::F_GET_SEALS) && args[2] == 0,
        libc::SYS_close => true,
        _ => false,
    }
}

fn initializer_futex_arguments_match(args: [u64; 6]) -> bool {
    args[1] == (libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG) as u64
        && args[2] == 1
        && args[3..] == [0, 0, 0]
}

fn initializer_fcntl_add_seals_arguments_match(args: [u64; 6]) -> bool {
    canonical_c_int_argument_is(args[1], libc::F_ADD_SEALS) && args[2] == TRAMPOLINE_SEALS as u64
}

fn after_loader_syscall_registers_match(
    number: i64,
    args: [u64; 6],
    registers: &libc::user_regs_struct,
) -> bool {
    registers.orig_rax as i64 == number
        && [
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            registers.r8,
            registers.r9,
        ] == args
}

fn exact_readonly_openat_arguments(args: &[u64; 6]) -> bool {
    canonical_c_int_argument_is(args[0], libc::AT_FDCWD)
        && canonical_c_int_argument(args[2])
            == Some(libc::O_RDONLY | libc::O_CLOEXEC | libc::O_LARGEFILE)
        && args[3] == 0
}

fn canonical_anonymous_mmap_descriptor(raw: u64) -> bool {
    canonical_c_int_argument_is(raw, -1)
}

fn exact_read_prefix_length(raw_result: i64, expected: &[u8]) -> Option<usize> {
    let amount = usize::try_from(raw_result).ok()?;
    (amount <= expected.len() && (amount != 0 || expected.is_empty())).then_some(amount)
}

fn exact_read_window_end(start: usize, input_len: usize, count: usize) -> Option<usize> {
    if start > input_len || (count == 0 && start != input_len) {
        return None;
    }
    start.checked_add(count).map(|end| end.min(input_len))
}

fn exact_owned_descriptor_statx_arguments(args: &[u64; 6]) -> bool {
    canonical_c_int_argument_is(args[2], libc::AT_EMPTY_PATH)
        && canonical_c_int_argument(args[3]) == Some(PRIVATE_STATX_MASK as i32)
}

fn fd_statx_bytes(descriptor: libc::c_int) -> io::Result<[u8; STATX_OUTPUT_BYTES]> {
    let mut value = std::mem::MaybeUninit::<libc::statx>::zeroed();
    let result = unsafe {
        libc::statx(
            descriptor,
            b"\0".as_ptr().cast(),
            libc::AT_EMPTY_PATH,
            PRIVATE_STATX_MASK,
            value.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { value.assume_init() };
    let mut bytes = [0_u8; STATX_OUTPUT_BYTES];
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&raw const value).cast::<u8>(),
            bytes.as_mut_ptr(),
            bytes.len(),
        );
    }
    Ok(bytes)
}

fn descriptor_statx_bytes(tid: Pid, descriptor: u64) -> io::Result<[u8; STATX_OUTPUT_BYTES]> {
    let file = std::fs::File::open(format!("/proc/{tid}/fd/{descriptor}"))?;
    fd_statx_bytes(file.as_raw_fd())
}

#[derive(Clone)]
pub(super) struct AfterLoaderToolCallbackContext {
    diagnostics: crate::LiteinstCallerDiagnostics,
    raw_clock: Option<u64>,
    tid: Pid,
    generation: u64,
    phase: LiteinstRuntimePhase,
    physical_status: Option<safeptrace::PhysicalStatusId>,
}

impl AfterLoaderToolCallbackContext {
    pub(super) fn record(&self, callback: &str) -> Result<(), Error> {
        self.diagnostics
            .record(
                format!("Tool callback: {callback}"),
                self.raw_clock,
                format!(
                    "tid={} generation={} phase={:?} physical_status={:?}",
                    self.tid, self.generation, self.phase, self.physical_status,
                ),
            )
            .map_err(|error| {
                Error::runtime(
                    self.tid,
                    "record LiteInst after-loader Tool callback",
                    error.to_string(),
                )
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CallerFunction {
    ErrnoLocation,
    Dlopen,
    Initializer,
}

/// A controller-bound image name scoped to one exec generation.
///
/// The contained identity is only a file-domain key. It is never compared to
/// the device/inode tuple observed in `/proc/<pid>/maps`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AfterLoaderImageId {
    generation: u64,
    file: crate::after_loader::FileIdentity,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AfterLoaderTrampolineId {
    generation: u64,
    serial: u64,
    file: crate::after_loader::FileIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResolvedImageGeometry {
    image: AfterLoaderImageId,
    mapping: MappingIdentity,
    load_bias: u64,
    span: (u64, u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AfterLoaderHelperIsolation {
    image: AfterLoaderImageId,
    range: GuestRange,
    original_mapping: GuestMap,
    mapping: MappingIdentity,
    load_bias: u64,
    file_offset: u64,
    expected_bytes: Vec<u8>,
}

impl AfterLoaderHelperIsolation {
    fn applies_to(&self, identity: MappingIdentity, load_bias: u64) -> bool {
        self.mapping == identity && self.load_bias == load_bias
    }

    fn validates_mapping(&self, task: &Stopped, mapping: &GuestMap) -> bool {
        mapping.start == self.range.start
            && mapping.end == self.range.end
            && !mapping.readable
            && !mapping.writable
            && !mapping.executable
            && !mapping.shared
            && mapping.mapping_identity() == self.mapping
            && mapping.path == self.original_mapping.path
            && mapping.offset == self.file_offset
            && guest_hook_mapping_attributes(task.pid(), mapping)
                .is_some_and(|attributes| attributes.fork_safe && attributes.protection_key == 0)
    }

    fn validates_live_page(&self, task: &Stopped) -> bool {
        guest_maps(task.pid()).is_some_and(|maps| {
            maps.iter()
                .any(|mapping| self.validates_mapping(task, mapping))
        }) && {
            let Ok(address) = usize::try_from(self.range.start) else {
                return false;
            };
            let mut bytes = vec![0_u8; self.expected_bytes.len()];
            read_stopped_ptrace_words(task, address, &mut bytes) && bytes == self.expected_bytes
        }
    }

    fn target_loader_projection(&self) -> Option<crate::target_loader::TargetIsolatedRxPage> {
        crate::target_loader::TargetIsolatedRxPage::new(
            self.range.start,
            self.range.end,
            self.file_offset,
            self.mapping.as_target_loader(),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SharedReservationIdentity {
    mapping: MappingIdentity,
    path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrampolineCloseShape {
    Complete,
    Abandoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageGeometryMismatch {
    MappingOutsideImageSpan,
    MissingPage,
    SharedPage,
    LoadPermissions,
    LoadIdentity,
    LoadOffset,
    AnonymousBssBacking,
    HolePermissions,
    HoleIdentity,
    HoleOffset,
    HelperIsolation,
    NonwritableBytes { file_offset: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AfterLoaderOwnedDescriptor {
    SealedRuntime {
        bytes: Arc<[u8]>,
        position: u64,
    },
    BoundImage {
        image: AfterLoaderImageId,
        bytes: Arc<[u8]>,
        position: u64,
    },
    ProcMaps {
        device: u64,
        inode: u64,
        bytes: Arc<[u8]>,
        position: u64,
    },
    Trampoline {
        id: Option<AfterLoaderTrampolineId>,
        size: Option<u64>,
    },
}

impl AfterLoaderOwnedDescriptor {
    fn reopened(&self) -> Option<Self> {
        match self {
            Self::SealedRuntime { bytes, .. } => Some(Self::SealedRuntime {
                bytes: bytes.clone(),
                position: 0,
            }),
            Self::BoundImage { image, bytes, .. } => Some(Self::BoundImage {
                image: *image,
                bytes: bytes.clone(),
                position: 0,
            }),
            Self::ProcMaps { .. } | Self::Trampoline { .. } => None,
        }
    }

    fn bytes_and_position(&self) -> Option<(&[u8], u64)> {
        match self {
            Self::SealedRuntime { bytes, position }
            | Self::BoundImage {
                bytes, position, ..
            }
            | Self::ProcMaps {
                bytes, position, ..
            } => Some((bytes, *position)),
            Self::Trampoline { .. } => None,
        }
    }

    fn set_position(&mut self, position: u64) -> bool {
        match self {
            Self::SealedRuntime {
                position: current, ..
            }
            | Self::BoundImage {
                position: current, ..
            }
            | Self::ProcMaps {
                position: current, ..
            } => {
                *current = position;
                true
            }
            Self::Trampoline { .. } => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AfterLoaderMappingPurpose {
    Controller,
    Image { image: AfterLoaderImageId },
    ImageZeroFill { image: AfterLoaderImageId },
    SharedReservation { trampoline: AfterLoaderTrampolineId },
    Trampoline { trampoline: AfterLoaderTrampolineId },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AfterLoaderOwnedMapping {
    start: u64,
    end: u64,
    readable: bool,
    writable: bool,
    executable: bool,
    shared: bool,
    descriptor: Option<u64>,
    offset: u64,
    purpose: AfterLoaderMappingPurpose,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AfterLoaderSyscallEffect {
    None,
    CetStatus {
        destination: u64,
        before: [u8; 8],
    },
    Open(AfterLoaderOwnedDescriptor),
    Close(u64),
    Map {
        requested: u64,
        raw_length: u64,
        protection: i32,
        flags: i32,
        descriptor: Option<u64>,
        offset: u64,
        purpose: AfterLoaderMappingPurpose,
    },
    Protect {
        start: u64,
        raw_length: u64,
        protection: i32,
    },
    Remove {
        start: u64,
        raw_length: u64,
    },
    Resize {
        descriptor: u64,
        length: u64,
    },
    AddTrampolineSeals {
        descriptor: u64,
    },
    VerifyTrampolineSeals {
        descriptor: u64,
        trampoline: AfterLoaderTrampolineId,
    },
    Identity(Pid),
    Read {
        descriptor: u64,
        destination: u64,
        offset: u64,
        expected: Vec<u8>,
        advances: bool,
    },
    RecordBreak,
    CheckBreak(u64),
    ExpectedResult(i64),
    FutexWake {
        address: u64,
        word: [u8; 4],
    },
    Stat {
        descriptor: u64,
        destination: u64,
        fields: AfterLoaderStatFields,
    },
    Statx {
        descriptor: u64,
        destination: u64,
        expected: [u8; STATX_OUTPUT_BYTES],
    },
}

fn private_memory_effect_result_is_exact(effect: &AfterLoaderSyscallEffect, result: i64) -> bool {
    !matches!(
        effect,
        AfterLoaderSyscallEffect::Protect { .. } | AfterLoaderSyscallEffect::Remove { .. }
    ) || result == 0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AfterLoaderSyscallCompletion {
    KernelResult(i64),
    UnsupportedCetStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CetStatusCompletionError {
    OutputChanged,
    SyscallFailed(i64),
    NonzeroSuccess(i64),
}

fn complete_cet_status_query(
    effect: &AfterLoaderSyscallEffect,
    raw_result: i64,
    after: [u8; 8],
) -> Option<Result<AfterLoaderSyscallCompletion, CetStatusCompletionError>> {
    let AfterLoaderSyscallEffect::CetStatus { before, .. } = effect else {
        return None;
    };
    Some(if raw_result == -(libc::EINVAL as i64) {
        if after == *before {
            Ok(AfterLoaderSyscallCompletion::UnsupportedCetStatus)
        } else {
            Err(CetStatusCompletionError::OutputChanged)
        }
    } else if raw_result < 0 {
        Err(CetStatusCompletionError::SyscallFailed(raw_result))
    } else if raw_result == 0 {
        Ok(AfterLoaderSyscallCompletion::KernelResult(raw_result))
    } else {
        Err(CetStatusCompletionError::NonzeroSuccess(raw_result))
    })
}

fn caller_private_syscall_result_is_accepted(completion: AfterLoaderSyscallCompletion) -> bool {
    match completion {
        AfterLoaderSyscallCompletion::KernelResult(raw_result) => {
            raw_result < -4095 || raw_result >= 0
        }
        AfterLoaderSyscallCompletion::UnsupportedCetStatus => true,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AfterLoaderStatFields {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    rdev: u64,
    size: i64,
    block_size: i64,
    blocks: i64,
    access_seconds: i64,
    access_nanoseconds: i64,
    modify_seconds: i64,
    modify_nanoseconds: i64,
    change_seconds: i64,
    change_nanoseconds: i64,
}

impl AfterLoaderStatFields {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            rdev: metadata.rdev(),
            size: metadata.size() as i64,
            block_size: metadata.blksize() as i64,
            blocks: metadata.blocks() as i64,
            access_seconds: metadata.atime(),
            access_nanoseconds: metadata.atime_nsec(),
            modify_seconds: metadata.mtime(),
            modify_nanoseconds: metadata.mtime_nsec(),
            change_seconds: metadata.ctime(),
            change_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
pub(super) struct AfterLoaderPrivateState {
    image: ImageIdentity,
    original_mappings: Vec<(u64, u64)>,
    original_descriptors: BTreeSet<u64>,
    owned_descriptors: BTreeMap<u64, AfterLoaderOwnedDescriptor>,
    owned_mappings: Vec<AfterLoaderOwnedMapping>,
    current_break: Option<u64>,
    shared_reservations: BTreeMap<AfterLoaderTrampolineId, ((u64, u64), SharedReservationIdentity)>,
    protected_ranges: Vec<(u64, u64)>,
    image_mappings: BTreeMap<AfterLoaderImageId, MappingIdentity>,
    image_geometries: BTreeMap<AfterLoaderImageId, ResolvedImageGeometry>,
    trampoline_mappings: BTreeMap<AfterLoaderTrampolineId, MappingIdentity>,
    trampoline_seals_added: BTreeSet<u64>,
    sealed_trampolines: BTreeSet<AfterLoaderTrampolineId>,
    next_trampoline_serial: u64,
    timer_suspension: Option<PrivateExecutionTimerSuspension>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AfterLoaderSyscallPurpose {
    TraceePreinit,
    ToolInjection,
    PatchProtection,
    PrivateSetup,
    GuestRtSigreturn,
    OrdinaryForward,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AfterLoaderSyscallPermit {
    image: Option<ImageIdentity>,
    tid: Pid,
    generation: u64,
    origin_status: Option<safeptrace::PhysicalStatusId>,
    admission_status: Option<safeptrace::PhysicalStatusId>,
    pub(super) purpose: AfterLoaderSyscallPurpose,
    pub(super) number: i64,
    pub(super) args: [u64; 6],
    instruction_pointer: u64,
    resume_pointer: u64,
    instruction: [u8; 4],
    instruction_length: u8,
    output_spans: Vec<(u64, u64)>,
    effect: AfterLoaderSyscallEffect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AfterLoaderPrivateCall {
    image: ImageIdentity,
    function: CallerFunction,
    origin_status: safeptrace::PhysicalStatusId,
    entry: u64,
    return_rip: u64,
    call_stack_top: u64,
    code_bytes: [u8; 30],
    arguments: [u64; 2],
    calls_phase: CallsPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AfterLoaderTraceOnlySyscall {
    pub(super) purpose: AfterLoaderSyscallPurpose,
    pub(super) number: i64,
    pub(super) args: [u64; 6],
    pub(super) known: Option<Sysno>,
}

pub(super) enum AfterLoaderForwardKind {
    TraceOnly(AfterLoaderTraceOnlySyscall),
}

pub(super) struct AfterLoaderForwardInFlight {
    tid: Pid,
    generation: u64,
    image: Option<ImageIdentity>,
    physical_generation: safeptrace::PhysicalEventGenerationId,
    observed_statuses: BTreeSet<safeptrace::PhysicalStatusId>,
    instruction_pointer: u64,
    resume_pointer: u64,
    instruction: [u8; 2],
    number: i64,
    args: [u64; 6],
    kind: AfterLoaderForwardKind,
}

pub(super) enum AfterLoaderForwardRoute {
    Continue(Stopped),
    Completed(Wait),
}

fn checked_output_span(address: u64, length: u64) -> Result<Option<(u64, u64)>, Errno> {
    if length == 0 {
        return Ok(None);
    }
    let end = address.checked_add(length).ok_or(Errno::EOVERFLOW)?;
    if address == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(Some((address, end)))
}

fn syscall_output_spans(number: i64, args: [u64; 6]) -> Result<Vec<(u64, u64)>, Errno> {
    let span = match number {
        libc::SYS_read | libc::SYS_pread64 | libc::SYS_getrandom => {
            checked_output_span(args[1], args[2])?
        }
        libc::SYS_fstat => checked_output_span(args[1], 144)?,
        libc::SYS_newfstatat => checked_output_span(args[2], 144)?,
        libc::SYS_statx => checked_output_span(args[4], STATX_OUTPUT_BYTES as u64)?,
        libc::SYS_arch_prctl if args[0] == ARCH_SHSTK_STATUS => checked_output_span(args[1], 8)?,
        libc::SYS_sigaltstack if args[0] == 0 => checked_output_span(args[1], 24)?,
        libc::SYS_rt_sigaction if args[1] == 0 => checked_output_span(args[2], 32)?,
        libc::SYS_rt_sigprocmask if args[1] == 0 => checked_output_span(args[2], args[3])?,
        _ => None,
    };
    Ok(span.into_iter().collect())
}

fn private_syscall_allowed(number: i64) -> bool {
    matches!(
        number,
        libc::SYS_openat
            | libc::SYS_close
            | libc::SYS_read
            | libc::SYS_pread64
            | libc::SYS_fstat
            | libc::SYS_newfstatat
            | libc::SYS_statx
            | libc::SYS_mmap
            | libc::SYS_mprotect
            | libc::SYS_munmap
            | libc::SYS_brk
            | libc::SYS_futex
            | libc::SYS_memfd_create
            | libc::SYS_ftruncate
            | libc::SYS_getpid
            | libc::SYS_gettid
            | libc::SYS_fcntl
    )
}

fn checked_range(start: u64, length: u64) -> Result<(u64, u64), Errno> {
    if length == 0 {
        return Err(Errno::EINVAL);
    }
    Ok((start, start.checked_add(length).ok_or(Errno::EOVERFLOW)?))
}

fn ranges_overlap(left: (u64, u64), right: (u64, u64)) -> bool {
    left.0 < right.1 && right.0 < left.1
}

impl AfterLoaderPrivateState {
    fn new(task: &Stopped, image: ImageIdentity) -> Result<Self, TraceError> {
        let mappings = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let original_mappings = mappings
            .into_iter()
            .map(|mapping| (mapping.start, mapping.end))
            .collect();
        let original_descriptors = descriptor_state(task.pid())
            .map_err(|_| Errno::EPROTO)?
            .into_keys()
            .map(u64::from)
            .collect();
        Ok(Self {
            image,
            original_mappings,
            original_descriptors,
            owned_descriptors: BTreeMap::new(),
            owned_mappings: Vec::new(),
            current_break: None,
            shared_reservations: BTreeMap::new(),
            protected_ranges: Vec::new(),
            image_mappings: BTreeMap::new(),
            image_geometries: BTreeMap::new(),
            trampoline_mappings: BTreeMap::new(),
            trampoline_seals_added: BTreeSet::new(),
            sealed_trampolines: BTreeSet::new(),
            next_trampoline_serial: 0,
            timer_suspension: None,
        })
    }

    fn image_id(&self, image: &LiteinstCallerImage) -> AfterLoaderImageId {
        AfterLoaderImageId {
            generation: self.image.generation,
            file: image.file_identity,
        }
    }

    pub(super) fn image_mapping_identity(
        &self,
        image: &LiteinstCallerImage,
    ) -> Option<MappingIdentity> {
        self.image_mappings.get(&self.image_id(image)).copied()
    }

    fn image_geometry(&self, image: &LiteinstCallerImage) -> Option<ResolvedImageGeometry> {
        self.image_geometries.get(&self.image_id(image)).copied()
    }

    fn has_causal_image_mapping(&self, geometry: ResolvedImageGeometry) -> bool {
        self.image_mappings.get(&geometry.image).copied() == Some(geometry.mapping)
    }

    fn bind_image_mapping(&mut self, image: AfterLoaderImageId, mapping: MappingIdentity) -> bool {
        if image.generation != self.image.generation || mapping.inode == 0 {
            return false;
        }
        if let Some(existing) = self.image_mappings.get(&image) {
            return *existing == mapping;
        }
        if self
            .image_mappings
            .iter()
            .any(|(other, existing)| *other != image && *existing == mapping)
            || self
                .trampoline_mappings
                .values()
                .any(|existing| *existing == mapping)
        {
            return false;
        }
        self.image_mappings.insert(image, mapping);
        true
    }

    fn bind_image_geometry(&mut self, geometry: ResolvedImageGeometry) -> bool {
        if geometry.span.0 >= geometry.span.1
            || !self.bind_image_mapping(geometry.image, geometry.mapping)
        {
            return false;
        }
        match self.image_geometries.get(&geometry.image) {
            Some(existing) => *existing == geometry,
            None => {
                self.image_geometries.insert(geometry.image, geometry);
                true
            }
        }
    }

    fn next_trampoline_id(
        &mut self,
        file: crate::after_loader::FileIdentity,
    ) -> Option<AfterLoaderTrampolineId> {
        let serial = self.next_trampoline_serial;
        self.next_trampoline_serial = serial.checked_add(1)?;
        Some(AfterLoaderTrampolineId {
            generation: self.image.generation,
            serial,
            file,
        })
    }

    fn trampoline_mapping(&self, trampoline: AfterLoaderTrampolineId) -> Option<MappingIdentity> {
        self.trampoline_mappings.get(&trampoline).copied()
    }

    fn bind_trampoline_mapping(
        &mut self,
        trampoline: AfterLoaderTrampolineId,
        mapping: MappingIdentity,
    ) -> bool {
        if trampoline.generation != self.image.generation || mapping.inode == 0 {
            return false;
        }
        if let Some(existing) = self.trampoline_mappings.get(&trampoline) {
            return *existing == mapping;
        }
        if self
            .trampoline_mappings
            .iter()
            .any(|(other, existing)| *other != trampoline && *existing == mapping)
            || self
                .image_mappings
                .values()
                .any(|existing| *existing == mapping)
        {
            return false;
        }
        self.trampoline_mappings.insert(trampoline, mapping);
        true
    }

    fn trampoline_awaiting_shared_reservation(&self) -> Option<AfterLoaderTrampolineId> {
        let mut candidate = None;
        for descriptor in self.owned_descriptors.values() {
            let AfterLoaderOwnedDescriptor::Trampoline {
                id: Some(trampoline),
                size: Some(size),
            } = descriptor
            else {
                continue;
            };
            if *size != TRAMPOLINE_ARENA_SIZE
                || self.trampoline_mapping(*trampoline).is_none()
                || self.shared_reservations.contains_key(trampoline)
                || self.owned_mappings.iter().any(|mapping| {
                    mapping.purpose
                        == (AfterLoaderMappingPurpose::SharedReservation {
                            trampoline: *trampoline,
                        })
                })
            {
                continue;
            }
            let aliases = self
                .owned_mappings
                .iter()
                .filter(|mapping| {
                    mapping.purpose
                        == AfterLoaderMappingPurpose::Trampoline {
                            trampoline: *trampoline,
                        }
                })
                .collect::<Vec<_>>();
            if aliases.len() != 2
                || aliases.iter().filter(|mapping| mapping.writable).count() != 1
                || aliases.iter().filter(|mapping| mapping.executable).count() != 1
            {
                continue;
            }
            if candidate.replace(*trampoline).is_some() {
                return None;
            }
        }
        candidate
    }

    fn owns_descriptor(&self, descriptor: u64) -> bool {
        self.owned_descriptors.contains_key(&descriptor)
            && !self.original_descriptors.contains(&descriptor)
    }

    fn owns_range(&self, range: (u64, u64), writable: bool) -> bool {
        self.owned_mappings.iter().any(|mapping| {
            mapping.start <= range.0 && range.1 <= mapping.end && (!writable || mapping.writable)
        })
    }

    fn owns_same_image_range(&self, range: (u64, u64), image: AfterLoaderImageId) -> bool {
        if range.0 >= range.1 {
            return false;
        }
        for (index, mapping) in self.owned_mappings.iter().enumerate() {
            let mapped = (mapping.start, mapping.end);
            if !ranges_overlap(mapped, range) {
                continue;
            }
            if !matches!(
                mapping.purpose,
                AfterLoaderMappingPurpose::Image { image: owned }
                    | AfterLoaderMappingPurpose::ImageZeroFill { image: owned }
                    if owned == image
            ) {
                return false;
            }
            let clipped = (mapping.start.max(range.0), mapping.end.min(range.1));
            if self.owned_mappings[..index].iter().any(|prior| {
                ranges_overlap((prior.start.max(range.0), prior.end.min(range.1)), clipped)
            }) {
                return false;
            }
        }

        let mut cursor = range.0;
        while cursor < range.1 {
            let Some(mapping) = self
                .owned_mappings
                .iter()
                .find(|mapping| mapping.start <= cursor && cursor < mapping.end)
            else {
                return false;
            };
            cursor = mapping.end.min(range.1);
        }
        true
    }

    fn shared_reservation_unmap_is_exact(&self, range: (u64, u64)) -> bool {
        self.shared_reservations
            .values()
            .all(|(reservation, _)| !ranges_overlap(*reservation, range) || *reservation == range)
    }

    fn trampoline_close_shape(
        &self,
        descriptor: u64,
        trampoline: AfterLoaderTrampolineId,
        size: u64,
    ) -> Option<TrampolineCloseShape> {
        let aliases = self
            .owned_mappings
            .iter()
            .filter(|mapping| {
                mapping.purpose == AfterLoaderMappingPurpose::Trampoline { trampoline }
            })
            .collect::<Vec<_>>();
        let reservations = self
            .owned_mappings
            .iter()
            .filter(|mapping| {
                mapping.purpose == (AfterLoaderMappingPurpose::SharedReservation { trampoline })
            })
            .collect::<Vec<_>>();
        let reservation_binding = self.shared_reservations.get(&trampoline);
        let complete = self.trampoline_mapping(trampoline).is_some()
            && aliases.len() == 2
            && aliases.iter().all(|mapping| {
                mapping.end - mapping.start == size
                    && mapping.offset == 0
                    && mapping.readable
                    && mapping.shared
                    && mapping.descriptor == Some(descriptor)
            })
            && aliases
                .iter()
                .filter(|mapping| mapping.writable && !mapping.executable)
                .count()
                == 1
            && aliases
                .iter()
                .filter(|mapping| !mapping.writable && mapping.executable)
                .count()
                == 1
            && !ranges_overlap(
                (aliases[0].start, aliases[0].end),
                (aliases[1].start, aliases[1].end),
            )
            && reservations.len() == 1
            && reservation_binding.is_some_and(|(range, identity)| {
                *range == (reservations[0].start, reservations[0].end)
                    && shared_reservation_identity_is_exact(identity)
            })
            && reservations[0].end - reservations[0].start == PAGE
            && reservations[0].readable
            && reservations[0].writable
            && !reservations[0].executable
            && reservations[0].shared
            && reservations[0].descriptor.is_none()
            && reservations[0].offset == 0;
        if complete {
            Some(TrampolineCloseShape::Complete)
        } else if aliases.is_empty() && reservations.is_empty() && reservation_binding.is_none() {
            Some(TrampolineCloseShape::Abandoned)
        } else {
            None
        }
    }

    fn runtime_futex_mapping(&self, range: (u64, u64), runtime: &LiteinstCallerImage) -> bool {
        let image = self.image_id(runtime);
        self.owned_mappings.iter().any(|mapping| {
            mapping.start <= range.0
                && range.1 <= mapping.end
                && mapping.writable
                && !mapping.executable
                && !mapping.shared
                && matches!(
                    mapping.purpose,
                    AfterLoaderMappingPurpose::Image { image: mapped }
                        | AfterLoaderMappingPurpose::ImageZeroFill { image: mapped }
                        if mapped == image
                )
        })
    }

    fn refuses_original_overlap(&self, range: (u64, u64)) -> bool {
        self.original_mappings
            .iter()
            .copied()
            .any(|original| ranges_overlap(original, range))
    }

    fn refuses_protected_overlap(&self, range: (u64, u64)) -> bool {
        self.protected_ranges
            .iter()
            .copied()
            .any(|protected| ranges_overlap(protected, range))
    }

    fn remove_owned_range(&mut self, removed: (u64, u64)) {
        let mut retained = Vec::new();
        for mapping in self.owned_mappings.drain(..) {
            if !ranges_overlap((mapping.start, mapping.end), removed) {
                retained.push(mapping);
                continue;
            }
            if mapping.start < removed.0 {
                retained.push(AfterLoaderOwnedMapping {
                    end: removed.0,
                    ..mapping
                });
            }
            if removed.1 < mapping.end {
                retained.push(AfterLoaderOwnedMapping {
                    start: removed.1,
                    offset: mapping.offset + (removed.1 - mapping.start),
                    ..mapping
                });
            }
        }
        self.owned_mappings = retained;
        self.shared_reservations
            .retain(|_, (reservation, _)| *reservation != removed);
    }

    fn protect_owned_range(&mut self, changed: (u64, u64), protection: i32) {
        let mut retained = Vec::new();
        for mapping in self.owned_mappings.drain(..) {
            if !ranges_overlap((mapping.start, mapping.end), changed) {
                retained.push(mapping);
                continue;
            }
            if mapping.start < changed.0 {
                retained.push(AfterLoaderOwnedMapping {
                    end: changed.0,
                    ..mapping
                });
            }
            let middle_start = mapping.start.max(changed.0);
            let middle_end = mapping.end.min(changed.1);
            retained.push(AfterLoaderOwnedMapping {
                start: middle_start,
                end: middle_end,
                readable: protection & libc::PROT_READ != 0,
                writable: protection & libc::PROT_WRITE != 0,
                executable: protection & libc::PROT_EXEC != 0,
                offset: mapping.offset + (middle_start - mapping.start),
                ..mapping
            });
            if changed.1 < mapping.end {
                retained.push(AfterLoaderOwnedMapping {
                    start: changed.1,
                    offset: mapping.offset + (changed.1 - mapping.start),
                    ..mapping
                });
            }
        }
        self.owned_mappings = retained;
    }

    pub(super) fn prepared_liteinst_controls(
        &self,
    ) -> Option<(Vec<PreparedArenaFootprint>, Vec<GuestRange>)> {
        if self.sealed_trampolines.is_empty()
            || self
                .trampoline_mappings
                .keys()
                .copied()
                .collect::<BTreeSet<_>>()
                != self.sealed_trampolines
            || self
                .shared_reservations
                .keys()
                .copied()
                .collect::<BTreeSet<_>>()
                != self.sealed_trampolines
        {
            return None;
        }
        let mut occupied = Vec::new();
        let mut arenas = Vec::new();
        let mut reservations = Vec::new();
        for trampoline in &self.sealed_trampolines {
            let aliases = self
                .owned_mappings
                .iter()
                .filter(|mapping| {
                    mapping.purpose
                        == (AfterLoaderMappingPurpose::Trampoline {
                            trampoline: *trampoline,
                        })
                })
                .collect::<Vec<_>>();
            let writable = aliases
                .iter()
                .find(|mapping| {
                    mapping.readable
                        && mapping.writable
                        && !mapping.executable
                        && mapping.shared
                        && mapping.offset == 0
                        && mapping.end - mapping.start == TRAMPOLINE_ARENA_SIZE
                })
                .copied()?;
            let executable = aliases
                .iter()
                .find(|mapping| {
                    mapping.readable
                        && !mapping.writable
                        && mapping.executable
                        && mapping.shared
                        && mapping.offset == 0
                        && mapping.end - mapping.start == TRAMPOLINE_ARENA_SIZE
                })
                .copied()?;
            if aliases.len() != 2 {
                return None;
            }
            let reservation = self.owned_mappings.iter().find(|mapping| {
                mapping.purpose
                    == (AfterLoaderMappingPurpose::SharedReservation {
                        trampoline: *trampoline,
                    })
                    && mapping.readable
                    && mapping.writable
                    && !mapping.executable
                    && mapping.shared
                    && mapping.descriptor.is_none()
                    && mapping.offset == 0
                    && mapping.end - mapping.start == PAGE
                    && self
                        .shared_reservations
                        .get(trampoline)
                        .is_some_and(|(range, identity)| {
                            *range == (mapping.start, mapping.end)
                                && shared_reservation_identity_is_exact(identity)
                        })
            })?;
            let writable = GuestRange {
                start: writable.start,
                end: writable.end,
            };
            let executable = GuestRange {
                start: executable.start,
                end: executable.end,
            };
            let reservation = GuestRange {
                start: reservation.start,
                end: reservation.end,
            };
            if [writable, executable, reservation]
                .iter()
                .enumerate()
                .any(|(index, range)| {
                    [writable, executable, reservation][..index]
                        .iter()
                        .any(|prior| prior.overlaps(*range))
                        || occupied
                            .iter()
                            .any(|prior: &GuestRange| prior.overlaps(*range))
                })
            {
                return None;
            }
            occupied.extend([writable, executable, reservation]);
            arenas.push(PreparedArenaFootprint {
                writable,
                executable,
            });
            reservations.push(reservation);
        }
        Some((arenas, reservations))
    }

    fn helper_code_matches_sealed_runtime(
        &self,
        runtime: &LiteinstCallerImage,
        initializer: &crate::target_loader::TargetHostInitializer,
        helper: &LiteinstHelperCode,
    ) -> bool {
        let Some(geometry) = self.image_geometry(runtime) else {
            return false;
        };
        let helper_length = helper.range.end.checked_sub(helper.range.start);
        geometry.image == self.image_id(runtime)
            && self.image_mapping_identity(runtime) == Some(geometry.mapping)
            && geometry.mapping == helper.original_mapping.mapping_identity()
            && geometry.mapping == MappingIdentity::from_target_loader(initializer.mapping_identity)
            && geometry.load_bias == initializer.load_bias
            && initializer.tid == self.image.tid
            && initializer.start_ticks == self.image.start_ticks
            && geometry.span.0 <= helper.range.start
            && helper.range.end <= geometry.span.1
            && helper.original_mapping.contains_range(helper.range)
            && helper_length == Some(PAGE)
            && usize::try_from(PAGE) == Ok(helper.bytes.len())
    }

    fn bind_helper_isolation(
        &self,
        runtime: &LiteinstCallerImage,
        initializer: &crate::target_loader::TargetHostInitializer,
        helper: &LiteinstHelperCode,
    ) -> Option<AfterLoaderHelperIsolation> {
        if !self.helper_code_matches_sealed_runtime(runtime, initializer, helper) {
            return None;
        }
        let geometry = self.image_geometry(runtime)?;
        let mapping_delta = helper
            .range
            .start
            .checked_sub(helper.original_mapping.start)?;
        let file_offset = helper.original_mapping.offset.checked_add(mapping_delta)?;
        let start = usize::try_from(file_offset).ok()?;
        let end = start.checked_add(helper.bytes.len())?;
        let expected_bytes = runtime.bytes.get(start..end)?.to_vec();
        if expected_bytes != helper.bytes {
            return None;
        }
        let owning_mappings = self
            .owned_mappings
            .iter()
            .filter(|mapping| {
                mapping.purpose
                    == (AfterLoaderMappingPurpose::Image {
                        image: geometry.image,
                    })
                    && mapping.start <= helper.range.start
                    && helper.range.end <= mapping.end
                    && mapping.readable
                    && !mapping.writable
                    && mapping.executable
                    && !mapping.shared
                    && mapping
                        .offset
                        .checked_add(helper.range.start - mapping.start)
                        == Some(file_offset)
            })
            .count();
        if owning_mappings != 1 {
            return None;
        }
        Some(AfterLoaderHelperIsolation {
            image: geometry.image,
            range: helper.range,
            original_mapping: helper.original_mapping.clone(),
            mapping: geometry.mapping,
            load_bias: geometry.load_bias,
            file_offset,
            expected_bytes,
        })
    }

    pub(super) fn current_program_break(&self) -> Option<u64> {
        self.current_break
    }
}

impl<T: Tool> TracedTask<T> {
    pub(super) fn after_loader_private_timer_is_owned(&self) -> bool {
        self.liteinst_after_loader_private_state
            .as_ref()
            .is_some_and(|state| state.timer_suspension.is_some())
    }

    pub(super) fn retire_after_loader_private_timer_on_terminal(
        &mut self,
    ) -> Result<(), TraceError> {
        let retire = {
            let suspension = self
                .liteinst_after_loader_private_state
                .as_ref()
                .and_then(|state| state.timer_suspension.as_ref())
                .ok_or(Errno::EPROTO)?;
            self.timer.retire_private_execution_on_terminal(suspension)
        };
        if let Err(error) = retire {
            tracing::error!(
                tid = %self.tid(),
                %error,
                "failed to retire deterministic timer after terminal private after-loader execution"
            );
            return Err(Errno::EPROTO.into());
        }
        drop(
            self.liteinst_after_loader_private_state
                .as_mut()
                .and_then(|state| state.timer_suspension.take())
                .ok_or(Errno::EPROTO)?,
        );
        Ok(())
    }
}

fn page_down(value: u64) -> u64 {
    value & !(PAGE - 1)
}

fn page_up(value: u64) -> Result<u64, Errno> {
    value
        .checked_add(PAGE - 1)
        .map(page_down)
        .ok_or(Errno::EOVERFLOW)
}

fn read_exact_with_helper_isolation(
    task: &Stopped,
    address: u64,
    bytes: &mut [u8],
    isolation: Option<&AfterLoaderHelperIsolation>,
) -> Result<(), TraceError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let length = u64::try_from(bytes.len()).map_err(|_| Errno::EOVERFLOW)?;
    let requested = GuestRange::new(address, length).ok_or(Errno::EOVERFLOW)?;
    let Some(isolation) = isolation.filter(|isolation| isolation.range.overlaps(requested)) else {
        let address = usize::try_from(address).map_err(|_| Errno::EOVERFLOW)?;
        return Ok(task.read_exact(address, bytes)?);
    };

    let overlap_start = requested.start.max(isolation.range.start);
    let overlap_end = requested.end.min(isolation.range.end);
    let prefix_len =
        usize::try_from(overlap_start - requested.start).map_err(|_| Errno::EOVERFLOW)?;
    if prefix_len != 0 {
        let address = usize::try_from(requested.start).map_err(|_| Errno::EOVERFLOW)?;
        task.read_exact(address, &mut bytes[..prefix_len])?;
    }

    let mut live_page = vec![0_u8; isolation.expected_bytes.len()];
    let helper_address = usize::try_from(isolation.range.start).map_err(|_| Errno::EOVERFLOW)?;
    if !read_stopped_ptrace_words(task, helper_address, &mut live_page)
        || live_page != isolation.expected_bytes
    {
        return Err(Errno::EPROTO.into());
    }
    let destination_start =
        usize::try_from(overlap_start - requested.start).map_err(|_| Errno::EOVERFLOW)?;
    let source_start =
        usize::try_from(overlap_start - isolation.range.start).map_err(|_| Errno::EOVERFLOW)?;
    let overlap_len = usize::try_from(overlap_end - overlap_start).map_err(|_| Errno::EOVERFLOW)?;
    bytes[destination_start..destination_start + overlap_len]
        .copy_from_slice(&live_page[source_start..source_start + overlap_len]);

    let suffix_start =
        usize::try_from(overlap_end - requested.start).map_err(|_| Errno::EOVERFLOW)?;
    if suffix_start < bytes.len() {
        let address = usize::try_from(overlap_end).map_err(|_| Errno::EOVERFLOW)?;
        task.read_exact(address, &mut bytes[suffix_start..])?;
    }
    Ok(())
}

/// Return the bytes on which Linux memory-management syscalls actually act.
///
/// The raw positive length remains part of the exact syscall permit. Linux
/// rounds that length upward for mmap, mprotect, and munmap; the address itself
/// must be page aligned for every fixed or returned range admitted here. This
/// private policy deliberately refuses mprotect's zero-length no-op because it
/// is not a loader memory effect and would add an unaudited syscall shape.
fn checked_page_effect_length(raw_length: u64) -> Result<u64, Errno> {
    if raw_length == 0 {
        return Err(Errno::EINVAL);
    }
    page_up(raw_length)
}

fn checked_page_effect_range(start: u64, raw_length: u64) -> Result<(u64, u64), Errno> {
    if !start.is_multiple_of(PAGE) {
        return Err(Errno::EINVAL);
    }
    checked_range(start, checked_page_effect_length(raw_length)?)
}

fn checked_private_mmap_effect_length(raw_length: u64) -> Result<u64, Errno> {
    let effective_length = checked_page_effect_length(raw_length)?;
    if effective_length > MAX_PRIVATE_MMAP_EFFECT {
        return Err(Errno::EINVAL);
    }
    Ok(effective_length)
}

fn private_mmap_offset_is_admissible(offset: u64) -> bool {
    offset <= i64::MAX as u64 && offset.is_multiple_of(PAGE)
}

fn exact_trampoline_mmap_length(raw_length: u64) -> bool {
    raw_length == TRAMPOLINE_ARENA_SIZE
}

fn exact_shared_reservation_mmap_length(raw_length: u64) -> bool {
    raw_length == PAGE
}

fn descriptor_position(tid: Pid, descriptor: u64) -> io::Result<u64> {
    let bytes = bounded_proc(format!("/proc/{tid}/fdinfo/{descriptor}"), 64 * 1024)?;
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    text.lines()
        .find_map(|line| line.strip_prefix("pos:\t"))
        .ok_or_else(|| io::Error::other("descriptor position is absent"))?
        .parse()
        .map_err(io::Error::other)
}

fn target_range_is_zero(task: &Stopped, start: u64, length: u64) -> Result<bool, TraceError> {
    let end = start.checked_add(length).ok_or(Errno::EOVERFLOW)?;
    if start == end {
        return Ok(true);
    }
    // process_vm_readv deliberately follows the target's current user access
    // permissions and therefore rejects a freshly mapped PROT_NONE guard page.
    // The procfs memory file remains bound to this live stopped task and uses
    // the kernel's ptrace-authorized access path without changing protections.
    let memory = std::fs::File::open(format!("/proc/{}/mem", task.pid()))
        .map_err(|error| Errno::new(error.raw_os_error().unwrap_or(libc::EIO)))?;
    let mut address = start;
    let mut bytes = [0_u8; 4096];
    while address < end {
        let amount = usize::try_from((end - address).min(bytes.len() as u64))
            .map_err(|_| Errno::EOVERFLOW)?;
        memory
            .read_exact_at(&mut bytes[..amount], address)
            .map_err(|error| Errno::new(error.raw_os_error().unwrap_or(libc::EIO)))?;
        if bytes[..amount].iter().any(|byte| *byte != 0) {
            return Ok(false);
        }
        address += amount as u64;
    }
    Ok(true)
}

fn mapping_identity_matches(identity: MappingIdentity, map: &GuestMap) -> bool {
    map.mapping_identity() == identity
}

fn shared_reservation_identity_is_exact(identity: &SharedReservationIdentity) -> bool {
    identity.mapping.device_major == 0
        && identity.mapping.device_minor == 1
        && identity.mapping.inode != 0
        && identity
            .path
            .as_ref()
            .is_some_and(|path| path.as_os_str().as_encoded_bytes() == b"/dev/zero (deleted)")
}

fn exact_shared_reservation_identity(map: &GuestMap) -> Option<SharedReservationIdentity> {
    let identity = SharedReservationIdentity {
        mapping: map.mapping_identity(),
        path: map.path.clone(),
    };
    shared_reservation_identity_is_exact(&identity).then_some(identity)
}

fn loader_resolution_matches_geometry(
    mapping_identity: (u64, u64, u64),
    load_bias: u64,
    geometry: ResolvedImageGeometry,
) -> bool {
    mapping_identity == geometry.mapping.as_target_loader() && load_bias == geometry.load_bias
}

fn one_exact_geometry_candidate(
    candidates: &[(MappingIdentity, u64)],
) -> Option<(MappingIdentity, u64)> {
    (candidates.len() == 1).then(|| candidates[0])
}

fn dlopen_graph_images<'a>(
    provider: &'a LiteinstCallerImage,
    dependencies: &'a [LiteinstCallerImage],
) -> impl Iterator<Item = &'a LiteinstCallerImage> {
    std::iter::once(provider).chain(dependencies.iter())
}

fn dlopen_graph_image_for_path<'a>(
    provider: &'a LiteinstCallerImage,
    dependencies: &'a [LiteinstCallerImage],
    path: &[u8],
) -> Option<&'a LiteinstCallerImage> {
    dlopen_graph_images(provider, dependencies)
        .find(|image| path == image.path.as_os_str().as_encoded_bytes())
}

fn exact_geometry_bytes_match(observed: &[u8], expected: &[u8]) -> bool {
    observed == expected
}

fn stable_backing_stamp(metadata: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

/// Observe the `/proc/maps` identity produced by mapping one exact open file.
/// The file-domain metadata is deliberately not converted into a maps device.
fn mapping_identity_for_open_file(file: &std::fs::File) -> io::Result<MappingIdentity> {
    let length = PAGE as usize;
    let address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_NONE,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if address == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let start = address as u64;
        let end = start
            .checked_add(PAGE)
            .ok_or_else(|| io::Error::other("temporary mapping range overflow"))?;
        let maps = guest_maps(Pid::from_raw(std::process::id() as i32))
            .ok_or_else(|| io::Error::other("cannot read controller maps"))?;
        let mapping = maps
            .iter()
            .filter(|mapping| mapping.start <= start && end <= mapping.end)
            .find(|mapping| {
                !mapping.readable
                    && !mapping.writable
                    && !mapping.executable
                    && !mapping.shared
                    && mapping.offset.checked_add(start - mapping.start) == Some(0)
                    && mapping.inode != 0
            })
            .ok_or_else(|| io::Error::other("temporary file mapping is not exact"))?;
        Ok(mapping.mapping_identity())
    })();
    let unmap = unsafe { libc::munmap(address, length) };
    if unmap != 0 {
        return Err(io::Error::last_os_error());
    }
    result
}

fn exact_open_backing_identity(
    file: &mut std::fs::File,
    path: &Path,
    image: &LiteinstCallerImage,
) -> io::Result<Option<MappingIdentity>> {
    let before = file.metadata()?;
    if !before.is_file()
        || crate::after_loader::FileIdentity::from_metadata(&before) != image.file_identity
    {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(
        (image.bytes.len() as u64)
            .checked_add(1)
            .ok_or_else(|| io::Error::other("backing byte bound overflow"))?,
    )
    .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let path_after = std::fs::metadata(path)?;
    let mapping = mapping_identity_for_open_file(file)?;
    Ok((bytes.as_slice() == image.bytes.as_ref()
        && stable_backing_stamp(&before) == stable_backing_stamp(&after)
        && stable_backing_stamp(&before) == stable_backing_stamp(&path_after))
    .then_some(mapping))
}

fn exact_open_backing_matches(
    file: &mut std::fs::File,
    path: &Path,
    image: &LiteinstCallerImage,
    expected_mapping: MappingIdentity,
) -> io::Result<bool> {
    Ok(exact_open_backing_identity(file, path, image)? == Some(expected_mapping))
}

fn target_bound_image_mapping_identity(
    pid: Pid,
    image: &LiteinstCallerImage,
) -> io::Result<Option<MappingIdentity>> {
    let relative = image.path.strip_prefix("/").map_err(io::Error::other)?;
    let target_path = PathBuf::from(format!("/proc/{pid}/root")).join(relative);
    let mut file = std::fs::File::open(&target_path)?;
    exact_open_backing_identity(&mut file, &target_path, image)
}

fn exact_image_backing_paths_match(
    pid: Pid,
    image: &LiteinstCallerImage,
    geometry: ResolvedImageGeometry,
    maps: &[GuestMap],
) -> io::Result<bool> {
    let relevant = maps
        .iter()
        .filter(|mapping| {
            mapping.mapping_identity() == geometry.mapping
                && ranges_overlap((mapping.start, mapping.end), geometry.span)
        })
        .collect::<Vec<_>>();
    if relevant.is_empty()
        || relevant
            .iter()
            .any(|mapping| mapping.path.as_ref().is_none_or(|path| !path.is_absolute()))
    {
        return Ok(false);
    }
    let paths = relevant
        .into_iter()
        .map(|mapping| mapping.path.as_ref().unwrap().clone())
        .collect::<BTreeSet<_>>();
    for guest_path in paths {
        let relative = guest_path.strip_prefix("/").map_err(io::Error::other)?;
        let target_path = PathBuf::from(format!("/proc/{pid}/root")).join(relative);
        let mut file = std::fs::File::open(&target_path)?;
        if !exact_open_backing_matches(&mut file, &target_path, image, geometry.mapping)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn altstack_fields(bytes: &[u8; 24]) -> (u64, u32, u64) {
    // stack_t has four padding bytes between ss_flags and ss_size on x86-64.
    // Observe the complete raw buffer but compare all three defined fields.
    (
        u64::from_ne_bytes(bytes[..8].try_into().unwrap()),
        u32::from_ne_bytes(bytes[8..12].try_into().unwrap()),
        u64::from_ne_bytes(bytes[16..].try_into().unwrap()),
    )
}

fn descriptor_state(tid: Pid) -> io::Result<BTreeMap<u32, (u64, u64, Vec<u8>)>> {
    let directory = format!("/proc/{tid}/fd");
    let mut state = BTreeMap::new();
    for entry in std::fs::read_dir(directory)? {
        if state.len() >= 256 {
            return Err(io::Error::other("descriptor count exceeds fixture bound"));
        }
        let entry = entry?;
        let fd = entry
            .file_name()
            .to_str()
            .ok_or_else(|| io::Error::other("nontext descriptor"))?
            .parse::<u32>()
            .map_err(io::Error::other)?;
        let metadata = std::fs::metadata(entry.path())?;
        let info = bounded_proc(format!("/proc/{tid}/fdinfo/{fd}"), 64 * 1024)?;
        if state
            .insert(fd, (metadata.dev(), metadata.ino(), info))
            .is_some()
        {
            return Err(io::Error::other("duplicate descriptor"));
        }
    }
    Ok(state)
}

fn descriptor_flags(tid: Pid, descriptor: u64) -> io::Result<u64> {
    let bytes = bounded_proc(format!("/proc/{tid}/fdinfo/{descriptor}"), 64 * 1024)?;
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix("flags:\t"))
        .ok_or_else(|| io::Error::other("descriptor flags are absent"))?;
    u64::from_str_radix(value, 8).map_err(io::Error::other)
}

fn environment_map(bytes: &[u8]) -> io::Result<BTreeMap<OsString, OsString>> {
    let mut result = BTreeMap::new();
    if bytes.is_empty() {
        return Ok(result);
    }
    let content = bytes
        .strip_suffix(&[0])
        .ok_or_else(|| io::Error::other("unterminated environment"))?;
    for item in content.split(|byte| *byte == 0) {
        let equals = item
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or_else(|| io::Error::other("environment item lacks equals"))?;
        if equals == 0
            || result
                .insert(
                    OsString::from_vec(item[..equals].to_vec()),
                    OsString::from_vec(item[equals + 1..].to_vec()),
                )
                .is_some()
        {
            return Err(io::Error::other("empty or duplicate environment key"));
        }
    }
    Ok(result)
}

fn bounded_proc(path: impl AsRef<std::path::Path>, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other("proc input exceeds bound"));
    }
    Ok(bytes)
}

fn signal_state(tid: Pid) -> io::Result<Vec<String>> {
    let bytes = bounded_proc(format!("/proc/{tid}/status"), 64 * 1024)?;
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let keys = ["SigPnd:", "ShdPnd:", "SigBlk:", "SigIgn:", "SigCgt:"];
    let selected: Vec<_> = text
        .lines()
        .filter(|line| keys.iter().any(|k| line.starts_with(k)))
        .map(str::to_owned)
        .collect();
    if selected.len() != keys.len() {
        return Err(io::Error::other("signal state incomplete"));
    }
    for key in ["SigPnd:", "ShdPnd:"] {
        let line = selected.iter().find(|line| line.starts_with(key)).unwrap();
        if u64::from_str_radix(line.split_whitespace().nth(1).unwrap_or(""), 16)
            .map_err(io::Error::other)?
            != 0
        {
            return Err(io::Error::other("signal pending in after-loader call"));
        }
    }
    Ok(selected)
}

fn register_words(r: &libc::user_regs_struct) -> [u64; 27] {
    [
        r.r15, r.r14, r.r13, r.r12, r.rbp, r.rbx, r.r11, r.r10, r.r9, r.r8, r.rax, r.rcx, r.rdx,
        r.rsi, r.rdi, r.orig_rax, r.rip, r.cs, r.eflags, r.rsp, r.ss, r.fs_base, r.gs_base, r.ds,
        r.es, r.fs, r.gs,
    ]
}

fn after_loader_event_summary(event: &Event) -> String {
    match event {
        Event::NewChild(operation, child) => format!(
            "NewChild(operation={operation:?} tid={} generation={:?})",
            child.pid(),
            child.physical_event_generation(),
        ),
        Event::Exec(previous_tid) => format!("Exec(previous_tid={previous_tid})"),
        Event::VforkDone => "VforkDone".to_owned(),
        Event::Exit => "Exit".to_owned(),
        Event::Seccomp => "Seccomp".to_owned(),
        Event::Stop => "Stop".to_owned(),
        Event::Syscall => "Syscall".to_owned(),
        Event::Signal(signal) => format!("Signal({signal:?})"),
    }
}

fn after_loader_wait_summary(wait: &Wait) -> String {
    match wait {
        Wait::Stopped(task, event) => format!(
            "Stopped(tid={} generation={:?} physical_status={:?} event={})",
            task.pid(),
            task.physical_event_generation(),
            task.physical_status_id(),
            after_loader_event_summary(event),
        ),
        Wait::Exited(pid, status) => format!("Exited(tid={pid} status={status:?})"),
    }
}

impl<L: Tool + 'static> TracedTask<L> {
    pub(super) fn after_loader_config(&self) -> Option<LiteinstAfterLoaderConfig> {
        self.global_state
            .liteinst_runtime
            .as_ref()?
            .after_loader
            .clone()
    }

    fn caller_error(&self, message: impl fmt::Display) -> Error {
        Error::runtime(
            self.tid(),
            "LiteInst after-loader call",
            message.to_string(),
        )
    }

    pub(super) fn caller_observe(
        &self,
        operation: &str,
        detail: impl Into<String>,
    ) -> Result<(), Error> {
        let config = self.after_loader_config().ok_or(Errno::EPROTO)?;
        config
            .diagnostics
            .record(operation, self.timer.diagnostic_clock(), detail)
            .map_err(|e| self.caller_error(e))
    }

    fn caller_cstring(&self, task: &Stopped, address: u64, limit: usize) -> Result<Vec<u8>, Error> {
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let mapping = maps
            .iter()
            .find(|mapping| mapping.readable && mapping.contains(address))
            .ok_or(Errno::EFAULT)?;
        let available = usize::try_from(mapping.end - address)
            .map_err(|_| Errno::EOVERFLOW)?
            .min(limit.checked_add(1).ok_or(Errno::EOVERFLOW)?);
        let mut bytes = vec![0; available];
        task.read_exact(address as usize, &mut bytes)?;
        let nul = bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| self.caller_error("private string is unterminated or oversized"))?;
        bytes.truncate(nul);
        Ok(bytes)
    }

    fn caller_cstring_vector(
        &self,
        task: &Stopped,
        address: u64,
        maximum_items: usize,
        maximum_bytes: usize,
    ) -> Result<Vec<Vec<u8>>, Error> {
        if address == 0 {
            return Err(self.caller_error("private string vector is null"));
        }
        let mut result = Vec::new();
        let mut bytes = 0_usize;
        for index in 0..=maximum_items {
            let pointer_address = address
                .checked_add(
                    u64::try_from(index)
                        .map_err(|_| Errno::EOVERFLOW)?
                        .checked_mul(8)
                        .ok_or(Errno::EOVERFLOW)?,
                )
                .ok_or(Errno::EOVERFLOW)?;
            let pointer_range = GuestRange::new(pointer_address, 8).ok_or(Errno::EFAULT)?;
            if !guest_maps(task.pid()).is_some_and(|maps| {
                maps.iter()
                    .any(|mapping| mapping.readable && mapping.contains_range(pointer_range))
            }) {
                return Err(self.caller_error("private string-vector pointer is unreadable"));
            }
            let mut raw_pointer = [0; 8];
            task.read_exact(pointer_address as usize, &mut raw_pointer)?;
            let pointer = u64::from_ne_bytes(raw_pointer);
            if pointer == 0 {
                return Ok(result);
            }
            if index == maximum_items {
                return Err(self.caller_error("private string vector exceeds its item bound"));
            }
            let remaining = maximum_bytes
                .checked_sub(bytes)
                .ok_or_else(|| self.caller_error("private string vector exceeds its byte bound"))?;
            let item = self.caller_cstring(task, pointer, remaining)?;
            bytes = bytes
                .checked_add(item.len())
                .and_then(|value| value.checked_add(1))
                .ok_or(Errno::EOVERFLOW)?;
            if bytes > maximum_bytes {
                return Err(self.caller_error("private string vector exceeds its byte bound"));
            }
            result.push(item);
        }
        Err(self.caller_error("private string vector is unterminated"))
    }

    fn validate_after_loader_command_execve(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        registers: &libc::user_regs_struct,
    ) -> Result<(), Error> {
        let expected_path = config.executable.path.as_os_str().as_encoded_bytes();
        if self.caller_cstring(task, registers.rdi, 4096)? != expected_path {
            return Err(self.caller_error("command-bootstrap exec path differs"));
        }
        let argv = self.caller_cstring_vector(task, registers.rsi, 1, 4096)?;
        if argv.len() != 1 || argv[0].as_slice() != expected_path {
            return Err(self.caller_error("command-bootstrap argv differs"));
        }
        let environment = self.caller_cstring_vector(task, registers.rdx, 256, 1024 * 1024)?;
        let mut observed = BTreeMap::new();
        for item in environment {
            let separator = item.iter().position(|byte| *byte == b'=').ok_or_else(|| {
                self.caller_error("command-bootstrap environment entry lacks '='")
            })?;
            if separator == 0 {
                return Err(self.caller_error("command-bootstrap environment key is empty"));
            }
            let key = std::ffi::OsString::from_vec(item[..separator].to_vec());
            let value = std::ffi::OsString::from_vec(item[separator + 1..].to_vec());
            if observed.insert(key, value).is_some() {
                return Err(self.caller_error("command-bootstrap environment key is duplicated"));
            }
        }
        config
            .validate_environment(&observed)
            .map_err(|error| self.caller_error(error))
    }

    fn after_loader_private_state(&self) -> Result<&AfterLoaderPrivateState, Error> {
        self.liteinst_after_loader_private_state
            .as_ref()
            .ok_or_else(|| self.caller_error("private resource state is unavailable"))
    }

    fn after_loader_private_state_mut(&mut self) -> Result<&mut AfterLoaderPrivateState, Error> {
        self.liteinst_after_loader_private_state
            .as_mut()
            .ok_or(Errno::EPROTO.into())
    }

    fn validate_after_loader_output_spans(&self, spans: &[(u64, u64)]) -> Result<(), Error> {
        let state = self.after_loader_private_state()?;
        if spans
            .iter()
            .copied()
            .any(|span| !state.owns_range(span, true))
        {
            return Err(self.caller_error(
                "private syscall output is outside controller-owned writable memory",
            ));
        }
        Ok(())
    }

    fn bind_after_loader_syscall_effect(
        &mut self,
        effect: AfterLoaderSyscallEffect,
    ) -> Result<(), Error> {
        let permit = self
            .liteinst_after_loader_syscall_permit
            .as_mut()
            .ok_or(Errno::EPROTO)?;
        permit.effect = effect;
        Ok(())
    }

    fn after_loader_open_effect(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        args: [u64; 6],
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        if !exact_readonly_openat_arguments(&args) {
            return Err(
                self.caller_error("private openat flags or directory are not read-only and bound")
            );
        }
        let path = self.caller_cstring(task, args[1], 4096)?;
        if path
            == config
                .sealed_runtime
                .image
                .path
                .as_os_str()
                .as_encoded_bytes()
        {
            return Ok(AfterLoaderSyscallEffect::Open(
                AfterLoaderOwnedDescriptor::SealedRuntime {
                    bytes: config.sealed_runtime.image.bytes.clone(),
                    position: 0,
                },
            ));
        }
        if path == b"/proc/self/maps" {
            let proc_path = format!("/proc/{}/maps", task.pid());
            let metadata =
                std::fs::metadata(&proc_path).map_err(|error| self.caller_error(error))?;
            let bytes = bounded_proc(&proc_path, 2 * 1024 * 1024)
                .map_err(|error| self.caller_error(error))?;
            return Ok(AfterLoaderSyscallEffect::Open(
                AfterLoaderOwnedDescriptor::ProcMaps {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    bytes: bytes.into(),
                    position: 0,
                },
            ));
        }
        if let Some(suffix) = path.strip_prefix(b"/proc/self/fd/") {
            let descriptor = std::str::from_utf8(suffix)
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(Errno::EPROTO)?;
            let owned = self
                .after_loader_private_state()?
                .owned_descriptors
                .get(&descriptor)
                .and_then(AfterLoaderOwnedDescriptor::reopened)
                .ok_or_else(|| self.caller_error("private proc descriptor path is not owned"))?;
            return Ok(AfterLoaderSyscallEffect::Open(owned));
        }
        let expected = dlopen_graph_image_for_path(&config.provider, &config.dependencies, &path)
            .ok_or_else(|| {
            self.caller_error("private openat path is outside the bound graph")
        })?;
        let metadata =
            std::fs::metadata(&expected.path).map_err(|error| self.caller_error(error))?;
        let bytes = std::fs::read(&expected.path).map_err(|error| self.caller_error(error))?;
        if metadata.dev() != expected.file_identity.device
            || metadata.ino() != expected.file_identity.inode
            || bytes.as_slice() != expected.bytes.as_ref()
        {
            return Err(self.caller_error("bound graph file changed before private openat"));
        }
        Ok(AfterLoaderSyscallEffect::Open(
            AfterLoaderOwnedDescriptor::BoundImage {
                image: self.after_loader_private_state()?.image_id(expected),
                bytes: expected.bytes.clone(),
                position: 0,
            },
        ))
    }

    fn after_loader_read_effect(
        &self,
        task: &Stopped,
        number: i64,
        args: [u64; 6],
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        if args[2] > MAX_PRIVATE_READ {
            return Err(self.caller_error("private read count exceeds its bound"));
        }
        let descriptor = self
            .after_loader_private_state()?
            .owned_descriptors
            .get(&args[0])
            .ok_or_else(|| self.caller_error("private read descriptor is not owned"))?;
        let (bytes, tracked_position) = descriptor
            .bytes_and_position()
            .ok_or_else(|| self.caller_error("private descriptor is not readable input"))?;
        let kernel_position =
            descriptor_position(task.pid(), args[0]).map_err(|error| self.caller_error(error))?;
        if kernel_position != tracked_position {
            return Err(
                self.caller_error("private descriptor position changed outside an admitted read")
            );
        }
        if let AfterLoaderOwnedDescriptor::ProcMaps {
            device,
            inode,
            bytes: opened,
            ..
        } = descriptor
        {
            let path = format!("/proc/{}/maps", task.pid());
            let metadata = std::fs::metadata(&path).map_err(|error| self.caller_error(error))?;
            let current =
                bounded_proc(&path, 2 * 1024 * 1024).map_err(|error| self.caller_error(error))?;
            if metadata.dev() != *device
                || metadata.ino() != *inode
                || current.as_slice() != opened.as_ref()
            {
                return Err(
                    self.caller_error("private proc-maps input changed after its exact open")
                );
            }
        }
        let offset = if number == libc::SYS_read {
            tracked_position
        } else {
            args[3]
        };
        let start = usize::try_from(offset).map_err(|_| Errno::EOVERFLOW)?;
        let count = usize::try_from(args[2]).map_err(|_| Errno::EOVERFLOW)?;
        let end = exact_read_window_end(start, bytes.len(), count).ok_or_else(|| {
            self.caller_error("private read begins beyond input or requests zero before EOF")
        })?;
        Ok(AfterLoaderSyscallEffect::Read {
            descriptor: args[0],
            destination: args[1],
            offset,
            expected: bytes[start..end].to_vec(),
            advances: number == libc::SYS_read,
        })
    }

    fn after_loader_stat_effect(
        &self,
        task: &Stopped,
        descriptor: u64,
        destination: u64,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        if !self
            .after_loader_private_state()?
            .owns_descriptor(descriptor)
        {
            return Err(self.caller_error("private stat descriptor is not owned"));
        }
        let metadata = std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
            .map_err(|error| self.caller_error(error))?;
        if !metadata.is_file() {
            return Err(self.caller_error("private stat target is not a regular file"));
        }
        Ok(AfterLoaderSyscallEffect::Stat {
            descriptor,
            destination,
            fields: AfterLoaderStatFields::from_metadata(&metadata),
        })
    }

    fn admit_after_loader_controller_syscall(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        number: i64,
        args: [u64; 6],
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        let state = self.after_loader_private_state()?;
        if state.image != self.after_loader_identity(task)? {
            return Err(self.caller_error("private resource image changed"));
        }
        let spans = syscall_output_spans(number, args)?;
        self.validate_after_loader_output_spans(&spans)?;
        match number {
            libc::SYS_mmap => self.after_loader_map_effect(task, args, None),
            libc::SYS_mprotect => {
                let protection = canonical_c_int_argument(args[2]).ok_or_else(|| {
                    self.caller_error("private mprotect protection is not a canonical C int")
                })?;
                let range = checked_page_effect_range(args[0], args[1])?;
                if !state.owns_range(range, false)
                    || state.refuses_protected_overlap(range)
                    || protection & !(libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) != 0
                    || protection & libc::PROT_WRITE != 0 && protection & libc::PROT_EXEC != 0
                {
                    return Err(self.caller_error(
                        "private mprotect is outside an owned range or requests W+X",
                    ));
                }
                Ok(AfterLoaderSyscallEffect::Protect {
                    start: args[0],
                    raw_length: args[1],
                    protection,
                })
            }
            libc::SYS_munmap => {
                let range = checked_page_effect_range(args[0], args[1])?;
                if !state.owns_range(range, false) {
                    return Err(self.caller_error("private munmap is outside an owned range"));
                }
                if !state.shared_reservation_unmap_is_exact(range) {
                    return Err(
                        self.caller_error("private munmap partially overlaps a shared reservation")
                    );
                }
                if state.refuses_protected_overlap(range) {
                    return Err(
                        self.caller_error("private munmap overlaps a protected runtime range")
                    );
                }
                Ok(AfterLoaderSyscallEffect::Remove {
                    start: args[0],
                    raw_length: args[1],
                })
            }
            libc::SYS_arch_prctl
                if controller_semantic_arguments_match(libc::SYS_arch_prctl, args) =>
            {
                let mut before = [0; 8];
                task.read_exact(args[1] as usize, &mut before)?;
                Ok(AfterLoaderSyscallEffect::CetStatus {
                    destination: args[1],
                    before,
                })
            }
            libc::SYS_sigaltstack
                if controller_semantic_arguments_match(libc::SYS_sigaltstack, args) =>
            {
                Ok(AfterLoaderSyscallEffect::None)
            }
            libc::SYS_rt_sigaction
                if controller_semantic_arguments_match(libc::SYS_rt_sigaction, args) =>
            {
                Ok(AfterLoaderSyscallEffect::None)
            }
            libc::SYS_rt_sigprocmask
                if controller_semantic_arguments_match(libc::SYS_rt_sigprocmask, args) =>
            {
                Ok(AfterLoaderSyscallEffect::None)
            }
            libc::SYS_openat => self.after_loader_open_effect(task, config, args),
            libc::SYS_brk
                if controller_semantic_arguments_match(libc::SYS_brk, args)
                    && state.current_break.is_none() =>
            {
                Ok(AfterLoaderSyscallEffect::RecordBreak)
            }
            libc::SYS_fcntl
                if matches!(
                    state.owned_descriptors.get(&args[0]),
                    Some(AfterLoaderOwnedDescriptor::SealedRuntime { .. })
                ) && controller_semantic_arguments_match(libc::SYS_fcntl, args) =>
            {
                Ok(AfterLoaderSyscallEffect::ExpectedResult(
                    crate::after_loader::RUNTIME_SEALS as i64,
                ))
            }
            libc::SYS_close
                if state.owns_descriptor(args[0])
                    && controller_semantic_arguments_match(libc::SYS_close, args) =>
            {
                Ok(AfterLoaderSyscallEffect::Close(args[0]))
            }
            _ => Err(self.caller_error(format!(
                "controller syscall is not admitted: nr={number} args={args:?}"
            ))),
        }
    }

    fn after_loader_map_effect(
        &self,
        task: &Stopped,
        args: [u64; 6],
        function: Option<CallerFunction>,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        let length = args[1];
        if checked_private_mmap_effect_length(length).is_err()
            || !private_mmap_offset_is_admissible(args[5])
        {
            return Err(self.caller_error("private mmap length or offset is outside bounds"));
        }
        let protection = canonical_c_int_argument(args[2])
            .ok_or_else(|| self.caller_error("private mmap protection is not a canonical C int"))?;
        let flags = canonical_c_int_argument(args[3])
            .ok_or_else(|| self.caller_error("private mmap flags are not a canonical C int"))?;
        if protection & !(libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) != 0
            || protection & libc::PROT_WRITE != 0 && protection & libc::PROT_EXEC != 0
        {
            return Err(self.caller_error("private mmap requests unsupported protection or W+X"));
        }
        let state = self.after_loader_private_state()?;
        let anonymous_descriptor = canonical_anonymous_mmap_descriptor(args[4]);
        let (descriptor, purpose) = match function {
            None => {
                let expected = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;
                if args[0] != 0 || !anonymous_descriptor || args[5] != 0 || flags != expected {
                    return Err(self.caller_error(
                        "controller mmap is not an exact new private anonymous mapping",
                    ));
                }
                (None, AfterLoaderMappingPurpose::Controller)
            }
            Some(CallerFunction::Dlopen) if !anonymous_descriptor => {
                let descriptor = state
                    .owned_descriptors
                    .get(&args[4])
                    .ok_or_else(|| self.caller_error("private file mmap descriptor disappeared"))?;
                let image = match descriptor {
                    AfterLoaderOwnedDescriptor::SealedRuntime { .. } => state.image_id(
                        &self
                            .after_loader_config()
                            .ok_or(Errno::EPROTO)?
                            .sealed_runtime
                            .image,
                    ),
                    AfterLoaderOwnedDescriptor::BoundImage { image, .. } => *image,
                    _ => {
                        return Err(self.caller_error(
                            "private loader mmap descriptor is outside the bound image graph",
                        ));
                    }
                };
                let base_flags = libc::MAP_PRIVATE | libc::MAP_DENYWRITE;
                if flags != base_flags && flags != base_flags | libc::MAP_FIXED {
                    return Err(self.caller_error("private loader file mmap flags are not exact"));
                }
                if flags & libc::MAP_FIXED != 0 {
                    let range = checked_page_effect_range(args[0], length)?;
                    if args[0] == 0
                        || args[0] % PAGE != 0
                        || !state.owns_same_image_range(range, image)
                        || state.refuses_original_overlap(range)
                        || state.refuses_protected_overlap(range)
                    {
                        return Err(self.caller_error(
                            "private loader fixed mmap is outside its owned image reservation",
                        ));
                    }
                } else if args[0] != 0 {
                    return Err(
                        self.caller_error("first private loader file mmap carries an address hint")
                    );
                }
                (Some(args[4]), AfterLoaderMappingPurpose::Image { image })
            }
            Some(CallerFunction::Dlopen) => {
                let range = checked_page_effect_range(args[0], length)?;
                let expected_flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED;
                let owner = state.owned_mappings.iter().find_map(|mapping| {
                    (mapping.start <= range.0
                        && range.1 <= mapping.end
                        && matches!(mapping.purpose, AfterLoaderMappingPurpose::Image { .. }))
                    .then_some(mapping.purpose)
                });
                let Some(AfterLoaderMappingPurpose::Image { image }) = owner else {
                    return Err(self.caller_error(
                        "private loader zero-fill mmap has no exact image reservation",
                    ));
                };
                if args[0] == 0
                    || args[0] % PAGE != 0
                    || args[5] != 0
                    || flags != expected_flags
                    || state.refuses_original_overlap(range)
                    || state.refuses_protected_overlap(range)
                {
                    return Err(self.caller_error("private loader zero-fill mmap geometry differs"));
                }
                (None, AfterLoaderMappingPurpose::ImageZeroFill { image })
            }
            Some(CallerFunction::Initializer) if !anonymous_descriptor => {
                let descriptor = state
                    .owned_descriptors
                    .get(&args[4])
                    .ok_or_else(|| self.caller_error("trampoline mmap descriptor disappeared"))?;
                let trampoline = match descriptor {
                    AfterLoaderOwnedDescriptor::Trampoline {
                        id: Some(trampoline),
                        size: Some(size),
                    } if *size == TRAMPOLINE_ARENA_SIZE => *trampoline,
                    _ => {
                        return Err(self.caller_error(
                            "trampoline mmap descriptor is not exactly sized and bound",
                        ));
                    }
                };
                let writable = protection == (libc::PROT_READ | libc::PROT_WRITE)
                    && args[0] == 0
                    && flags == libc::MAP_SHARED;
                let executable = protection == (libc::PROT_READ | libc::PROT_EXEC)
                    && args[0] != 0
                    && args[0] % PAGE == 0
                    && flags == libc::MAP_SHARED | libc::MAP_FIXED_NOREPLACE;
                let aliases = state
                    .owned_mappings
                    .iter()
                    .filter(|mapping| {
                        matches!(
                            mapping.purpose,
                            AfterLoaderMappingPurpose::Trampoline {
                                trampoline: mapped,
                            } if mapped == trampoline
                        )
                    })
                    .collect::<Vec<_>>();
                let writable_order = aliases.is_empty();
                let executable_order = aliases.len() == 1
                    && aliases[0].writable
                    && !aliases[0].executable
                    && aliases[0].shared
                    && aliases[0].offset == 0
                    && aliases[0].end - aliases[0].start == TRAMPOLINE_ARENA_SIZE;
                let candidate = checked_page_effect_range(args[0], length).ok();
                let candidate_is_free = candidate.is_some_and(|candidate| {
                    !state
                        .owned_mappings
                        .iter()
                        .any(|mapping| ranges_overlap((mapping.start, mapping.end), candidate))
                });
                let is_near_executable = executable
                    && guest_maps(task.pid()).is_some_and(|maps| {
                        maps.iter().any(|mapping| {
                            mapping.executable
                                && !mapping.writable
                                && i32::try_from(args[0] as i128 - mapping.start as i128).is_ok()
                        })
                    });
                if !exact_trampoline_mmap_length(length)
                    || args[5] != 0
                    || (!writable && !executable)
                    || writable && !writable_order
                    || executable
                        && (!executable_order || !candidate_is_free || !is_near_executable)
                    || executable
                        && state
                            .refuses_original_overlap(checked_page_effect_range(args[0], length)?)
                    || executable
                        && state
                            .refuses_protected_overlap(checked_page_effect_range(args[0], length)?)
                {
                    return Err(
                        self.caller_error("trampoline mmap is not one exact RW or RX shared alias")
                    );
                }
                (
                    Some(args[4]),
                    AfterLoaderMappingPurpose::Trampoline { trampoline },
                )
            }
            Some(CallerFunction::Initializer) => {
                let expected_flags = libc::MAP_SHARED | libc::MAP_ANONYMOUS;
                if args[0] != 0
                    || args[5] != 0
                    || !exact_shared_reservation_mmap_length(length)
                    || protection != (libc::PROT_READ | libc::PROT_WRITE)
                    || flags != expected_flags
                {
                    return Err(self.caller_error(
                        "shared anonymous mapping is not one exact initializer reservation",
                    ));
                }
                let trampoline =
                    state
                        .trampoline_awaiting_shared_reservation()
                        .ok_or_else(|| {
                            self.caller_error(
                                "shared reservation has no unique fully aliased trampoline owner",
                            )
                        })?;
                (
                    None,
                    AfterLoaderMappingPurpose::SharedReservation { trampoline },
                )
            }
            Some(CallerFunction::ErrnoLocation) => {
                return Err(self.caller_error("errno accessor attempted a private mmap"));
            }
        };
        Ok(AfterLoaderSyscallEffect::Map {
            requested: args[0],
            raw_length: length,
            protection,
            flags,
            descriptor,
            offset: args[5],
            purpose,
        })
    }

    fn admit_after_loader_function_syscall(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        function: CallerFunction,
        permit: &AfterLoaderSyscallPermit,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        if function == CallerFunction::ErrnoLocation || !private_syscall_allowed(permit.number) {
            return Err(self.caller_error(format!(
                "private function syscall {} is refused",
                permit.number
            )));
        }
        let state = self.after_loader_private_state()?;
        if permit.image != Some(state.image) {
            return Err(self.caller_error("private function image changed"));
        }
        self.validate_after_loader_output_spans(&permit.output_spans)?;
        let args = permit.args;
        match permit.number {
            libc::SYS_openat => {
                let effect = self.after_loader_open_effect(task, config, args)?;
                let admitted = matches!(
                    (&effect, function),
                    (
                        AfterLoaderSyscallEffect::Open(
                            AfterLoaderOwnedDescriptor::SealedRuntime { .. }
                                | AfterLoaderOwnedDescriptor::BoundImage { .. }
                        ),
                        CallerFunction::Dlopen
                    ) | (
                        AfterLoaderSyscallEffect::Open(AfterLoaderOwnedDescriptor::ProcMaps { .. }),
                        CallerFunction::Initializer
                    )
                );
                if !admitted {
                    return Err(
                        self.caller_error("private openat is outside its exact function phase")
                    );
                }
                Ok(effect)
            }
            libc::SYS_close if state.owns_descriptor(args[0]) => {
                Ok(AfterLoaderSyscallEffect::Close(args[0]))
            }
            libc::SYS_read | libc::SYS_pread64
                if state.owns_descriptor(args[0])
                    && match (state.owned_descriptors.get(&args[0]), function) {
                        (
                            Some(AfterLoaderOwnedDescriptor::ProcMaps { .. }),
                            CallerFunction::Initializer,
                        ) => true,
                        (
                            Some(
                                AfterLoaderOwnedDescriptor::SealedRuntime { .. }
                                | AfterLoaderOwnedDescriptor::BoundImage { .. },
                            ),
                            CallerFunction::Dlopen,
                        ) => true,
                        _ => false,
                    } =>
            {
                self.after_loader_read_effect(task, permit.number, args)
            }
            libc::SYS_fstat
                if state.owns_descriptor(args[0]) && function == CallerFunction::Dlopen =>
            {
                self.after_loader_stat_effect(task, args[0], args[1])
            }
            libc::SYS_newfstatat
                if state.owns_descriptor(args[0])
                    && function == CallerFunction::Dlopen
                    && self.caller_cstring(task, args[1], 1)?.is_empty()
                    && canonical_c_int_argument_is(args[3], libc::AT_EMPTY_PATH) =>
            {
                self.after_loader_stat_effect(task, args[0], args[2])
            }
            libc::SYS_statx
                if function == CallerFunction::Initializer
                    && matches!(
                        state.owned_descriptors.get(&args[0]),
                        Some(AfterLoaderOwnedDescriptor::ProcMaps { .. })
                    )
                    && self.caller_cstring(task, args[1], 1)?.is_empty()
                    && exact_owned_descriptor_statx_arguments(&args) =>
            {
                Ok(AfterLoaderSyscallEffect::Statx {
                    descriptor: args[0],
                    destination: args[4],
                    expected: descriptor_statx_bytes(task.pid(), args[0])
                        .map_err(|error| self.caller_error(error))?,
                })
            }
            libc::SYS_mmap => self.after_loader_map_effect(task, args, Some(function)),
            libc::SYS_mprotect => {
                let protection = canonical_c_int_argument(args[2]).ok_or_else(|| {
                    self.caller_error(
                        "private function mprotect protection is not a canonical C int",
                    )
                })?;
                let range = checked_page_effect_range(args[0], args[1])?;
                if !state.owns_range(range, false)
                    || state.refuses_protected_overlap(range)
                    || protection & !(libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) != 0
                    || protection & libc::PROT_WRITE != 0 && protection & libc::PROT_EXEC != 0
                {
                    return Err(self.caller_error("private function mprotect is unowned or W+X"));
                }
                Ok(AfterLoaderSyscallEffect::Protect {
                    start: args[0],
                    raw_length: args[1],
                    protection,
                })
            }
            libc::SYS_munmap => {
                let range = checked_page_effect_range(args[0], args[1])?;
                if !state.owns_range(range, false) {
                    return Err(self.caller_error("private function munmap is unowned"));
                }
                if !state.shared_reservation_unmap_is_exact(range) {
                    return Err(self.caller_error(
                        "private function munmap partially overlaps a shared reservation",
                    ));
                }
                if state.refuses_protected_overlap(range) {
                    return Err(self.caller_error(
                        "private function munmap overlaps a protected runtime range",
                    ));
                }
                Ok(AfterLoaderSyscallEffect::Remove {
                    start: args[0],
                    raw_length: args[1],
                })
            }
            libc::SYS_brk if args[0] == 0 => {
                let expected = state.current_break.ok_or_else(|| {
                    self.caller_error("private brk query has no preobserved value")
                })?;
                Ok(AfterLoaderSyscallEffect::CheckBreak(expected))
            }
            libc::SYS_futex if function == CallerFunction::Initializer => {
                let range = checked_range(args[0], 4)?;
                if args[0] % 4 != 0
                    || !state.runtime_futex_mapping(range, &config.sealed_runtime.image)
                    || !initializer_futex_arguments_match(args)
                {
                    return Err(self.caller_error(
                        "private futex is not the exact runtime FUTEX_WAKE_PRIVATE",
                    ));
                }
                let mut word = [0_u8; 4];
                task.read_exact(args[0] as usize, &mut word)?;
                Ok(AfterLoaderSyscallEffect::FutexWake {
                    address: args[0],
                    word,
                })
            }
            libc::SYS_memfd_create if function == CallerFunction::Initializer => {
                if self.caller_cstring(task, args[0], 64)? != b"liteinst2-trampoline"
                    || args[1] != (libc::MFD_CLOEXEC as u64 | MFD_ALLOW_SEALING)
                {
                    return Err(
                        self.caller_error("private memfd is not the bound LiteInst trampoline")
                    );
                }
                Ok(AfterLoaderSyscallEffect::Open(
                    AfterLoaderOwnedDescriptor::Trampoline {
                        id: None,
                        size: None,
                    },
                ))
            }
            libc::SYS_ftruncate
                if function == CallerFunction::Initializer
                    && matches!(
                        state.owned_descriptors.get(&args[0]),
                        Some(AfterLoaderOwnedDescriptor::Trampoline {
                            id: Some(_),
                            size: None,
                        })
                    )
                    && args[1] == TRAMPOLINE_ARENA_SIZE =>
            {
                Ok(AfterLoaderSyscallEffect::Resize {
                    descriptor: args[0],
                    length: args[1],
                })
            }
            libc::SYS_fcntl
                if function == CallerFunction::Initializer
                    && initializer_fcntl_add_seals_arguments_match(args) =>
            {
                let (trampoline, size) = match state.owned_descriptors.get(&args[0]) {
                    Some(AfterLoaderOwnedDescriptor::Trampoline {
                        id: Some(trampoline),
                        size: Some(size),
                    }) => (*trampoline, *size),
                    _ => {
                        return Err(self
                            .caller_error("trampoline seals target is not a complete descriptor"));
                    }
                };
                if size != TRAMPOLINE_ARENA_SIZE
                    || state.trampoline_seals_added.contains(&args[0])
                    || state.sealed_trampolines.contains(&trampoline)
                    || state.trampoline_close_shape(args[0], trampoline, size)
                        != Some(TrampolineCloseShape::Complete)
                {
                    return Err(self.caller_error(
                        "trampoline seals were added out of order or to an incomplete arena",
                    ));
                }
                Ok(AfterLoaderSyscallEffect::AddTrampolineSeals {
                    descriptor: args[0],
                })
            }
            libc::SYS_fcntl
                if function == CallerFunction::Initializer
                    && controller_semantic_arguments_match(libc::SYS_fcntl, args) =>
            {
                let trampoline = match state.owned_descriptors.get(&args[0]) {
                    Some(AfterLoaderOwnedDescriptor::Trampoline {
                        id: Some(trampoline),
                        size: Some(TRAMPOLINE_ARENA_SIZE),
                    }) => *trampoline,
                    _ => {
                        return Err(
                            self.caller_error("trampoline seal query lost its complete descriptor")
                        );
                    }
                };
                if !state.trampoline_seals_added.contains(&args[0])
                    || state.sealed_trampolines.contains(&trampoline)
                {
                    return Err(self
                        .caller_error("trampoline seals were queried before one exact addition"));
                }
                Ok(AfterLoaderSyscallEffect::VerifyTrampolineSeals {
                    descriptor: args[0],
                    trampoline,
                })
            }
            libc::SYS_getpid | libc::SYS_gettid if function == CallerFunction::Initializer => {
                Ok(AfterLoaderSyscallEffect::Identity(task.pid()))
            }
            libc::SYS_fcntl
                if state.owns_descriptor(args[0])
                    && function == CallerFunction::Dlopen
                    && matches!(
                        canonical_c_int_argument(args[1]),
                        Some(libc::F_GETFD | libc::F_GET_SEALS)
                    )
                    && args[2] == 0 =>
            {
                let expected = match canonical_c_int_argument(args[1]).ok_or(Errno::EPROTO)? {
                    libc::F_GETFD => libc::FD_CLOEXEC as i64,
                    libc::F_GET_SEALS
                        if matches!(
                            state.owned_descriptors.get(&args[0]),
                            Some(AfterLoaderOwnedDescriptor::SealedRuntime { .. })
                        ) =>
                    {
                        crate::after_loader::RUNTIME_SEALS as i64
                    }
                    _ => {
                        return Err(self.caller_error(
                            "private fcntl query is not bound to the sealed runtime",
                        ));
                    }
                };
                Ok(AfterLoaderSyscallEffect::ExpectedResult(expected))
            }
            _ => Err(self.caller_error(format!(
                "private function syscall arguments are refused: nr={} args={args:?}",
                permit.number
            ))),
        }
    }

    fn complete_after_loader_private_syscall(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
        raw_result: i64,
    ) -> Result<AfterLoaderSyscallCompletion, Error> {
        if let AfterLoaderSyscallEffect::CetStatus { destination, .. } = &permit.effect {
            let mut after = [0; 8];
            task.read_exact(*destination as usize, &mut after)?;
            return match complete_cet_status_query(&permit.effect, raw_result, after)
                .expect("CET effect must have a CET completion")
            {
                Ok(completion) => Ok(completion),
                Err(CetStatusCompletionError::OutputChanged) => {
                    Err(self.caller_error("unsupported CET status query changed its output buffer"))
                }
                Err(CetStatusCompletionError::SyscallFailed(result)) => Err(self.caller_error(
                    format!("admitted private syscall failed before its exact effect: {result}"),
                )),
                Err(CetStatusCompletionError::NonzeroSuccess(_)) => {
                    Err(self.caller_error("CET status query did not return zero"))
                }
            };
        }
        if raw_result < 0 {
            return if matches!(&permit.effect, AfterLoaderSyscallEffect::Map { .. }) {
                Ok(AfterLoaderSyscallCompletion::KernelResult(raw_result))
            } else {
                Err(self.caller_error(format!(
                    "admitted private syscall failed before its exact effect: {}",
                    raw_result
                )))
            };
        }
        if !private_memory_effect_result_is_exact(&permit.effect, raw_result) {
            return Err(self
                .caller_error("private mprotect or munmap completion did not return exact zero"));
        }
        match &permit.effect {
            AfterLoaderSyscallEffect::None => {}
            AfterLoaderSyscallEffect::CetStatus { .. } => {
                unreachable!("CET completion returned before ordinary effect handling")
            }
            AfterLoaderSyscallEffect::Open(kind) => {
                let descriptor = raw_result as u64;
                let state = self.after_loader_private_state()?;
                if state.original_descriptors.contains(&descriptor)
                    || state.owned_descriptors.contains_key(&descriptor)
                {
                    return Err(self.caller_error("private open reused a live descriptor"));
                }
                let metadata = std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
                    .map_err(|error| self.caller_error(error))?;
                let flags = descriptor_flags(task.pid(), descriptor)
                    .map_err(|error| self.caller_error(error))?;
                let expected_access = match kind {
                    AfterLoaderOwnedDescriptor::Trampoline { .. } => libc::O_RDWR,
                    _ => libc::O_RDONLY,
                } as u64;
                let expected_flags = expected_access | libc::O_CLOEXEC as u64 | KERNEL_O_LARGEFILE;
                if flags != expected_flags || !metadata.is_file() {
                    return Err(self.caller_error(
                        "private descriptor type, access mode or close-on-exec flag differs",
                    ));
                }
                let stored = match kind {
                    AfterLoaderOwnedDescriptor::SealedRuntime { bytes, position } => {
                        if metadata.dev() != config.sealed_runtime.image.file_identity.device
                            || metadata.ino() != config.sealed_runtime.image.file_identity.inode
                            || metadata.len() != bytes.len() as u64
                            || *position != 0
                        {
                            return Err(self.caller_error(
                                "opened sealed-runtime descriptor identity differs",
                            ));
                        }
                        let mut file =
                            std::fs::File::open(format!("/proc/{}/fd/{descriptor}", task.pid()))
                                .map_err(|error| self.caller_error(error))?;
                        let mut bytes = Vec::new();
                        (&mut file)
                            .take(crate::after_loader::MAX_RUNTIME_FILE as u64 + 1)
                            .read_to_end(&mut bytes)
                            .map_err(|error| self.caller_error(error))?;
                        if bytes.as_slice() != config.sealed_runtime.image.bytes.as_ref() {
                            return Err(self.caller_error("opened sealed-runtime bytes differ"));
                        }
                        kind.clone()
                    }
                    AfterLoaderOwnedDescriptor::BoundImage {
                        image,
                        bytes: bound_bytes,
                        position,
                    } => {
                        if metadata.dev() != image.file.device
                            || metadata.ino() != image.file.inode
                            || metadata.len() != bound_bytes.len() as u64
                            || *position != 0
                        {
                            return Err(
                                self.caller_error("opened graph descriptor identity differs")
                            );
                        }
                        let expected = dlopen_graph_images(&config.provider, &config.dependencies)
                            .find(|expected| state.image_id(expected) == *image)
                            .ok_or_else(|| {
                                self.caller_error("opened graph identity is no longer bound")
                            })?;
                        let mut file =
                            std::fs::File::open(format!("/proc/{}/fd/{descriptor}", task.pid()))
                                .map_err(|error| self.caller_error(error))?;
                        let mut bytes = Vec::new();
                        (&mut file)
                            .take(
                                (bound_bytes.len() as u64)
                                    .checked_add(1)
                                    .ok_or(Errno::EOVERFLOW)?,
                            )
                            .read_to_end(&mut bytes)
                            .map_err(|error| self.caller_error(error))?;
                        if bytes.as_slice() != expected.bytes.as_ref()
                            || bytes.as_slice() != bound_bytes.as_ref()
                        {
                            return Err(self.caller_error("opened graph bytes differ"));
                        }
                        kind.clone()
                    }
                    AfterLoaderOwnedDescriptor::ProcMaps {
                        device,
                        inode,
                        bytes,
                        position,
                    } => {
                        let link =
                            std::fs::read_link(format!("/proc/{}/fd/{descriptor}", task.pid()))
                                .map_err(|error| self.caller_error(error))?;
                        let expected = format!("/proc/{}/maps", task.pid());
                        if link.as_os_str().as_encoded_bytes() != expected.as_bytes()
                            || metadata.dev() != *device
                            || metadata.ino() != *inode
                            || *position != 0
                            || bounded_proc(&expected, 2 * 1024 * 1024)
                                .map_err(|error| self.caller_error(error))?
                                .as_slice()
                                != bytes.as_ref()
                        {
                            return Err(
                                self.caller_error("private proc-maps descriptor target differs")
                            );
                        }
                        kind.clone()
                    }
                    AfterLoaderOwnedDescriptor::Trampoline { id, size } => {
                        let link =
                            std::fs::read_link(format!("/proc/{}/fd/{descriptor}", task.pid()))
                                .map_err(|error| self.caller_error(error))?;
                        if link.as_os_str().as_encoded_bytes()
                            != b"/memfd:liteinst2-trampoline (deleted)"
                            || size.is_some()
                            || id.is_some()
                            || metadata.len() != 0
                        {
                            return Err(self.caller_error("private trampoline memfd name differs"));
                        }
                        let file = crate::after_loader::FileIdentity::from_metadata(&metadata);
                        let id = self
                            .after_loader_private_state_mut()?
                            .next_trampoline_id(file)
                            .ok_or(Errno::EOVERFLOW)?;
                        AfterLoaderOwnedDescriptor::Trampoline {
                            id: Some(id),
                            size: None,
                        }
                    }
                };
                if descriptor_position(task.pid(), descriptor)
                    .map_err(|error| self.caller_error(error))?
                    != 0
                {
                    return Err(self.caller_error("new private descriptor has a nonzero position"));
                }
                self.after_loader_private_state_mut()?
                    .owned_descriptors
                    .insert(descriptor, stored);
            }
            AfterLoaderSyscallEffect::Close(descriptor) => {
                let closing = self
                    .after_loader_private_state()?
                    .owned_descriptors
                    .get(descriptor)
                    .cloned()
                    .ok_or_else(|| self.caller_error("private close completion lost ownership"))?;
                if let AfterLoaderOwnedDescriptor::Trampoline {
                    id: Some(trampoline),
                    size: Some(size),
                } = &closing
                {
                    let state = self.after_loader_private_state()?;
                    let identity = state.trampoline_mapping(*trampoline);
                    let close_shape = state
                        .trampoline_close_shape(*descriptor, *trampoline, *size)
                        .ok_or_else(|| {
                            self.caller_error(
                                "trampoline descriptor closed with a partial alias/reservation set",
                            )
                        })?;
                    let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                    match close_shape {
                        TrampolineCloseShape::Complete => {
                            if !state.trampoline_seals_added.contains(descriptor)
                                || !state.sealed_trampolines.contains(trampoline)
                            {
                                return Err(self.caller_error(
                                    "retained trampoline was closed before exact seal verification",
                                ));
                            }
                            let identity = identity.ok_or_else(|| {
                                self.caller_error(
                                    "retained trampoline mapping identity was never captured",
                                )
                            })?;
                            let state = self.after_loader_private_state()?;
                            let aliases = state
                                .owned_mappings
                                .iter()
                                .filter(|mapping| {
                                    mapping.purpose
                                        == AfterLoaderMappingPurpose::Trampoline {
                                            trampoline: *trampoline,
                                        }
                                })
                                .collect::<Vec<_>>();
                            let reservations = state
                                .owned_mappings
                                .iter()
                                .filter(|mapping| {
                                    mapping.purpose
                                        == (AfterLoaderMappingPurpose::SharedReservation {
                                            trampoline: *trampoline,
                                        })
                                })
                                .collect::<Vec<_>>();
                            let reservation_binding =
                                state.shared_reservations.get(trampoline).ok_or_else(|| {
                                    self.caller_error(
                                        "retained trampoline reservation identity disappeared",
                                    )
                                })?;
                            let reservation_changed = !maps.iter().any(|mapping| {
                                mapping.start == reservations[0].start
                                    && mapping.end == reservations[0].end
                                    && mapping.offset == 0
                                    && mapping.readable
                                    && mapping.writable
                                    && !mapping.executable
                                    && mapping.shared
                                    && exact_shared_reservation_identity(mapping).as_ref()
                                        == Some(&reservation_binding.1)
                            });
                            if reservation_changed
                                || aliases.iter().any(|alias| {
                                    !maps.iter().any(|mapping| {
                                        mapping.start == alias.start
                                            && mapping.end == alias.end
                                            && mapping.offset == 0
                                            && mapping.readable
                                            && mapping.writable == alias.writable
                                            && mapping.executable == alias.executable
                                            && mapping.shared
                                            && mapping.mapping_identity() == identity
                                            && mapping.path.as_ref().is_some_and(|path| {
                                                path.as_os_str().as_encoded_bytes()
                                                    == b"/memfd:liteinst2-trampoline (deleted)"
                                            })
                                    })
                                })
                            {
                                return Err(self.caller_error(
                                    "trampoline aliases changed before descriptor close completed",
                                ));
                            }
                        }
                        TrampolineCloseShape::Abandoned => {
                            if state.trampoline_seals_added.contains(descriptor)
                                || state.sealed_trampolines.contains(trampoline)
                            {
                                return Err(self.caller_error(
                                    "abandoned trampoline carried partial seal state",
                                ));
                            }
                            if identity.is_some_and(|identity| {
                                maps.iter()
                                    .any(|mapping| mapping.mapping_identity() == identity)
                            }) {
                                return Err(self.caller_error(
                                    "abandoned trampoline still has a live mapping",
                                ));
                            }
                            if self
                                .after_loader_private_state_mut()?
                                .trampoline_mappings
                                .remove(trampoline)
                                != identity
                            {
                                return Err(self.caller_error(
                                    "abandoned trampoline mapping identity changed during close",
                                ));
                            }
                        }
                    }
                } else if matches!(&closing, AfterLoaderOwnedDescriptor::Trampoline { .. }) {
                    return Err(self.caller_error(
                        "trampoline descriptor closed before exact aliases were bound",
                    ));
                }
                if std::fs::symlink_metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
                    .is_ok()
                {
                    return Err(self
                        .caller_error("private descriptor remained live after successful close"));
                }
                if self
                    .after_loader_private_state_mut()?
                    .owned_descriptors
                    .remove(descriptor)
                    .is_none()
                {
                    return Err(self.caller_error("private close completion lost ownership"));
                }
                self.after_loader_private_state_mut()?
                    .trampoline_seals_added
                    .remove(descriptor);
            }
            AfterLoaderSyscallEffect::Map {
                requested,
                raw_length,
                protection,
                flags,
                descriptor,
                offset,
                purpose,
            } => {
                let start = raw_result as u64;
                let (start, end) = checked_page_effect_range(start, *raw_length)?;
                let state = self.after_loader_private_state()?;
                if (*requested != 0 && start != *requested)
                    || start % PAGE != 0
                    || state.refuses_original_overlap((start, end))
                {
                    return Err(
                        self.caller_error("private mmap result overlaps original target memory")
                    );
                }
                if let Some(descriptor) = descriptor {
                    let owned = state.owned_descriptors.get(descriptor).ok_or_else(|| {
                        self.caller_error("private mmap descriptor ownership disappeared")
                    })?;
                    // The file-byte bound is intentionally expressed using the
                    // raw length. Linux permits the rounded final page of a file
                    // mapping even when only a prefix is backed by file bytes.
                    if let AfterLoaderOwnedDescriptor::Trampoline {
                        size: Some(size), ..
                    } = owned
                        && offset
                            .checked_add(*raw_length)
                            .is_none_or(|end| end > *size)
                    {
                        return Err(self.caller_error("trampoline mapping exceeds its memfd"));
                    }
                }
                let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                let mapped = maps
                    .iter()
                    .find(|mapping| {
                        mapping.start <= start
                            && end <= mapping.end
                            && mapping.readable == (*protection & libc::PROT_READ != 0)
                            && mapping.writable == (*protection & libc::PROT_WRITE != 0)
                            && mapping.executable == (*protection & libc::PROT_EXEC != 0)
                            && mapping.shared == (*flags & libc::MAP_SHARED != 0)
                    })
                    .ok_or_else(|| {
                        self.caller_error(
                            "private mmap result is absent or has different permissions",
                        )
                    })?;
                if mapped.offset.checked_add(start - mapped.start) != Some(*offset) {
                    return Err(self.caller_error("private mmap result offset differs"));
                }
                if let Some(descriptor) = descriptor {
                    let owned = self
                        .after_loader_private_state()?
                        .owned_descriptors
                        .get(descriptor)
                        .cloned()
                        .ok_or(Errno::EPROTO)?;
                    let metadata =
                        std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
                            .map_err(|error| self.caller_error(error))?;
                    match (&owned, purpose) {
                        (
                            AfterLoaderOwnedDescriptor::SealedRuntime { .. },
                            AfterLoaderMappingPurpose::Image { image },
                        ) if *image
                            == self
                                .after_loader_private_state()?
                                .image_id(&config.sealed_runtime.image) =>
                        {
                            self.caller_sealed_runtime(task, config, *descriptor)?;
                        }
                        (
                            AfterLoaderOwnedDescriptor::BoundImage { image: owned, .. },
                            AfterLoaderMappingPurpose::Image { image },
                        ) if owned == image
                            && metadata.dev() == image.file.device
                            && metadata.ino() == image.file.inode => {}
                        (
                            AfterLoaderOwnedDescriptor::Trampoline {
                                id: Some(owned),
                                size: Some(size),
                            },
                            AfterLoaderMappingPurpose::Trampoline { trampoline },
                        ) if owned == trampoline
                            && metadata.dev() == trampoline.file.device
                            && metadata.ino() == trampoline.file.inode
                            && metadata.len() == *size => {}
                        _ => {
                            return Err(self.caller_error(
                                "private mmap descriptor authority or file identity changed",
                            ));
                        }
                    }
                } else {
                    let identity_matches = match purpose {
                        AfterLoaderMappingPurpose::SharedReservation { .. } => {
                            exact_shared_reservation_identity(mapped).is_some()
                        }
                        _ => mapped.inode == 0 && mapped.path.is_none(),
                    };
                    if !identity_matches {
                        return Err(self.caller_error(
                            "private anonymous mapping acquired a different kernel identity",
                        ));
                    }
                }
                if matches!(
                    purpose,
                    AfterLoaderMappingPurpose::Controller
                        | AfterLoaderMappingPurpose::ImageZeroFill { .. }
                        | AfterLoaderMappingPurpose::SharedReservation { .. }
                        | AfterLoaderMappingPurpose::Trampoline { .. }
                ) && !target_range_is_zero(task, start, end - start)?
                {
                    return Err(self.caller_error(
                        "new private anonymous mapping was not exactly zero-filled",
                    ));
                }
                if matches!(
                    purpose,
                    AfterLoaderMappingPurpose::Trampoline { .. }
                        | AfterLoaderMappingPurpose::SharedReservation { .. }
                ) && (mapped.start != start || mapped.end != end)
                {
                    return Err(self.caller_error("trampoline mapping merged with another mapping"));
                }
                if let AfterLoaderMappingPurpose::SharedReservation { trampoline } = purpose {
                    let state = self.after_loader_private_state()?;
                    if state.shared_reservations.contains_key(trampoline)
                        || state.trampoline_awaiting_shared_reservation() != Some(*trampoline)
                    {
                        return Err(self
                            .caller_error("shared reservation lost its unique trampoline owner"));
                    }
                }
                let state = self.after_loader_private_state_mut()?;
                match purpose {
                    AfterLoaderMappingPurpose::Image { image } => {
                        if !state.bind_image_mapping(*image, mapped.mapping_identity()) {
                            return Err(Error::runtime(
                                task.pid(),
                                "bind private image mapping identity",
                                "mapping identity changed, collided or left its generation",
                            ));
                        }
                    }
                    AfterLoaderMappingPurpose::ImageZeroFill { image } => {
                        if state.image_mappings.get(image).is_none() {
                            return Err(Error::runtime(
                                task.pid(),
                                "bind private image zero-fill mapping",
                                "zero-fill mapping preceded its file identity",
                            ));
                        }
                    }
                    AfterLoaderMappingPurpose::Trampoline { trampoline } => {
                        if !state.bind_trampoline_mapping(*trampoline, mapped.mapping_identity()) {
                            return Err(Error::runtime(
                                task.pid(),
                                "bind private trampoline mapping identity",
                                "trampoline mapping identity changed, collided or left its generation",
                            ));
                        }
                    }
                    AfterLoaderMappingPurpose::Controller
                    | AfterLoaderMappingPurpose::SharedReservation { .. } => {}
                }
                state.remove_owned_range((start, end));
                if let AfterLoaderMappingPurpose::SharedReservation { trampoline } = purpose {
                    let identity = exact_shared_reservation_identity(mapped).ok_or_else(|| {
                        Error::runtime(
                            task.pid(),
                            "bind private shared trampoline reservation",
                            "shared reservation lacks the exact Linux /dev/zero identity",
                        )
                    })?;
                    if state
                        .shared_reservations
                        .insert(*trampoline, ((start, end), identity))
                        .is_some()
                    {
                        return Err(Error::runtime(
                            task.pid(),
                            "bind private shared trampoline reservation",
                            "trampoline already owned a shared reservation",
                        ));
                    }
                }
                state.owned_mappings.push(AfterLoaderOwnedMapping {
                    start,
                    end,
                    readable: *protection & libc::PROT_READ != 0,
                    writable: *protection & libc::PROT_WRITE != 0,
                    executable: *protection & libc::PROT_EXEC != 0,
                    shared: *flags & libc::MAP_SHARED != 0,
                    descriptor: *descriptor,
                    offset: *offset,
                    purpose: *purpose,
                });
            }
            AfterLoaderSyscallEffect::Protect {
                start,
                raw_length,
                protection,
            } => {
                let range = checked_page_effect_range(*start, *raw_length)?;
                self.after_loader_private_state_mut()?
                    .protect_owned_range(range, *protection);
            }
            AfterLoaderSyscallEffect::Remove { start, raw_length } => {
                let range = checked_page_effect_range(*start, *raw_length)?;
                self.after_loader_private_state_mut()?
                    .remove_owned_range(range);
            }
            AfterLoaderSyscallEffect::Resize { descriptor, length } => {
                let trampoline = match self
                    .after_loader_private_state()?
                    .owned_descriptors
                    .get(descriptor)
                    .ok_or(Errno::EPROTO)?
                {
                    AfterLoaderOwnedDescriptor::Trampoline {
                        id: Some(trampoline),
                        size: None,
                    } => *trampoline,
                    _ => {
                        return Err(self.caller_error("private ftruncate descriptor state changed"));
                    }
                };
                let metadata = std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
                    .map_err(|error| self.caller_error(error))?;
                if metadata.len() != *length
                    || metadata.dev() != trampoline.file.device
                    || metadata.ino() != trampoline.file.inode
                {
                    return Err(self.caller_error("private ftruncate descriptor state changed"));
                }
                *self
                    .after_loader_private_state_mut()?
                    .owned_descriptors
                    .get_mut(descriptor)
                    .ok_or(Errno::EPROTO)? = AfterLoaderOwnedDescriptor::Trampoline {
                    id: Some(trampoline),
                    size: Some(*length),
                };
            }
            AfterLoaderSyscallEffect::AddTrampolineSeals { descriptor } => {
                if raw_result != 0
                    || !self
                        .after_loader_private_state_mut()?
                        .trampoline_seals_added
                        .insert(*descriptor)
                {
                    return Err(
                        self.caller_error("trampoline seal addition did not complete exactly once")
                    );
                }
            }
            AfterLoaderSyscallEffect::VerifyTrampolineSeals {
                descriptor,
                trampoline,
            } => {
                let state = self.after_loader_private_state_mut()?;
                if raw_result != i64::from(TRAMPOLINE_SEALS)
                    || !state.trampoline_seals_added.contains(descriptor)
                    || !matches!(
                        state.owned_descriptors.get(descriptor),
                        Some(AfterLoaderOwnedDescriptor::Trampoline {
                            id: Some(observed),
                            size: Some(TRAMPOLINE_ARENA_SIZE),
                        }) if observed == trampoline
                    )
                    || !state.sealed_trampolines.insert(*trampoline)
                {
                    return Err(self
                        .caller_error("trampoline seal verification did not match exact policy"));
                }
            }
            AfterLoaderSyscallEffect::Identity(expected) => {
                if raw_result != expected.as_raw() as i64 {
                    return Err(self.caller_error("private identity syscall returned another task"));
                }
            }
            AfterLoaderSyscallEffect::Read {
                descriptor,
                destination,
                offset,
                expected,
                advances,
            } => {
                let amount = exact_read_prefix_length(raw_result, expected).ok_or_else(|| {
                    self.caller_error(
                        "private read returned zero before EOF or exceeded its exact bound",
                    )
                })?;
                let expected = &expected[..amount];
                let mut actual = vec![0_u8; amount];
                task.read_exact(*destination as usize, &mut actual)?;
                if actual.as_slice() != expected {
                    return Err(
                        self.caller_error("private read bytes differ from the bound input range")
                    );
                }
                let next = if *advances {
                    offset.checked_add(amount as u64).ok_or(Errno::EOVERFLOW)?
                } else {
                    self.after_loader_private_state()?
                        .owned_descriptors
                        .get(descriptor)
                        .and_then(AfterLoaderOwnedDescriptor::bytes_and_position)
                        .map(|(_, position)| position)
                        .ok_or(Errno::EPROTO)?
                };
                if descriptor_position(task.pid(), *descriptor)
                    .map_err(|error| self.caller_error(error))?
                    != next
                {
                    return Err(self.caller_error(
                        "private read changed the descriptor position unexpectedly",
                    ));
                }
                if *advances
                    && !self
                        .after_loader_private_state_mut()?
                        .owned_descriptors
                        .get_mut(descriptor)
                        .is_some_and(|owned| owned.set_position(next))
                {
                    return Err(
                        self.caller_error("private read lost its tracked descriptor position")
                    );
                }
            }
            AfterLoaderSyscallEffect::RecordBreak => {
                if raw_result <= 0
                    || self
                        .after_loader_private_state_mut()?
                        .current_break
                        .replace(raw_result as u64)
                        .is_some()
                {
                    return Err(self
                        .caller_error("initial private brk query did not bind one current break"));
                }
            }
            AfterLoaderSyscallEffect::CheckBreak(expected) => {
                if raw_result as u64 != *expected {
                    return Err(self.caller_error("private brk query changed its bound value"));
                }
            }
            AfterLoaderSyscallEffect::ExpectedResult(expected) => {
                if raw_result != *expected {
                    return Err(self
                        .caller_error("private metadata query returned a different exact result"));
                }
            }
            AfterLoaderSyscallEffect::FutexWake { address, word } => {
                let mut after = [0_u8; 4];
                task.read_exact(*address as usize, &mut after)?;
                if raw_result != 0 || after != *word {
                    return Err(self.caller_error(
                        "private FUTEX_WAKE_PRIVATE found a participant or changed its word",
                    ));
                }
            }
            AfterLoaderSyscallEffect::Stat {
                descriptor,
                destination,
                fields,
            } => {
                if raw_result != 0 {
                    return Err(self.caller_error("private stat did not return zero"));
                }
                let metadata = std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
                    .map_err(|error| self.caller_error(error))?;
                if AfterLoaderStatFields::from_metadata(&metadata) != *fields {
                    return Err(
                        self.caller_error("private stat target changed while the syscall executed")
                    );
                }
                let mut bytes = [0_u8; 144];
                task.read_exact(*destination as usize, &mut bytes)?;
                let u64_at = |offset: usize| {
                    u64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap())
                };
                let i64_at = |offset: usize| {
                    i64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap())
                };
                let u32_at = |offset: usize| {
                    u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap())
                };
                if u64_at(0) != fields.device
                    || u64_at(8) != fields.inode
                    || u64_at(16) != fields.links
                    || u32_at(24) != fields.mode
                    || u32_at(28) != fields.uid
                    || u32_at(32) != fields.gid
                    || u64_at(40) != fields.rdev
                    || i64_at(48) != fields.size
                    || i64_at(56) != fields.block_size
                    || i64_at(64) != fields.blocks
                    || i64_at(72) != fields.access_seconds
                    || i64_at(80) != fields.access_nanoseconds
                    || i64_at(88) != fields.modify_seconds
                    || i64_at(96) != fields.modify_nanoseconds
                    || i64_at(104) != fields.change_seconds
                    || i64_at(112) != fields.change_nanoseconds
                {
                    return Err(self.caller_error(
                        "private stat output differs from the exact owned descriptor metadata",
                    ));
                }
            }
            AfterLoaderSyscallEffect::Statx {
                descriptor,
                destination,
                expected,
            } => {
                if raw_result != 0 {
                    return Err(self.caller_error("private statx did not return zero"));
                }
                let state = self.after_loader_private_state()?;
                let (device, inode, opened, position) = match state
                    .owned_descriptors
                    .get(descriptor)
                {
                    Some(AfterLoaderOwnedDescriptor::ProcMaps {
                        device,
                        inode,
                        bytes,
                        position,
                    }) => (*device, *inode, bytes, *position),
                    _ => {
                        return Err(self.caller_error("private statx descriptor authority changed"));
                    }
                };
                let proc_path = format!("/proc/{}/maps", task.pid());
                let metadata =
                    std::fs::metadata(&proc_path).map_err(|error| self.caller_error(error))?;
                let current = bounded_proc(&proc_path, 2 * 1024 * 1024)
                    .map_err(|error| self.caller_error(error))?;
                if metadata.dev() != device
                    || metadata.ino() != inode
                    || current.as_slice() != opened.as_ref()
                    || descriptor_position(task.pid(), *descriptor)
                        .map_err(|error| self.caller_error(error))?
                        != position
                    || descriptor_statx_bytes(task.pid(), *descriptor)
                        .map_err(|error| self.caller_error(error))?
                        != *expected
                {
                    return Err(self.caller_error(
                        "private statx target identity, bytes, position or metadata changed",
                    ));
                }
                let mut actual = [0_u8; STATX_OUTPUT_BYTES];
                task.read_exact(*destination as usize, &mut actual)?;
                if actual != *expected {
                    return Err(
                        self.caller_error("private statx output differs from its exact snapshot")
                    );
                }
            }
        }
        Ok(AfterLoaderSyscallCompletion::KernelResult(raw_result))
    }

    fn after_loader_image_geometry_matches(
        &self,
        task: &Stopped,
        image: &LiteinstCallerImage,
        identity: MappingIdentity,
        elf: &Elf<'_>,
        bias: u64,
        maps: &[GuestMap],
        isolation: Option<&AfterLoaderHelperIsolation>,
    ) -> Result<std::result::Result<(), ImageGeometryMismatch>, Error> {
        let isolation = isolation.filter(|isolation| isolation.applies_to(identity, bias));
        let loads = elf
            .program_headers
            .iter()
            .filter(|header| header.p_type == ph::PT_LOAD)
            .collect::<Vec<_>>();
        let first = loads
            .iter()
            .map(|header| page_down(header.p_vaddr))
            .min()
            .ok_or_else(|| self.caller_error("bound ELF has no PT_LOAD"))?;
        let first_load = loads
            .iter()
            .copied()
            .min_by_key(|header| page_down(header.p_vaddr))
            .ok_or_else(|| self.caller_error("bound ELF has no first PT_LOAD"))?;
        let last = loads
            .iter()
            .map(|header| {
                header
                    .p_vaddr
                    .checked_add(header.p_memsz)
                    .ok_or(Errno::EOVERFLOW)
                    .and_then(page_up)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max()
            .ok_or(Errno::EPROTO)?;
        let target_first = bias.checked_add(first).ok_or(Errno::EOVERFLOW)?;
        let target_last = bias.checked_add(last).ok_or(Errno::EOVERFLOW)?;
        if maps.iter().any(|mapping| {
            mapping.mapping_identity() == identity
                && (mapping.start < target_first || target_last < mapping.end)
        }) {
            return Ok(Err(ImageGeometryMismatch::MappingOutsideImageSpan));
        }
        let relro = elf
            .program_headers
            .iter()
            .filter(|header| header.p_type == ph::PT_GNU_RELRO)
            .map(|header| {
                let start = page_down(header.p_vaddr);
                let end = header
                    .p_vaddr
                    .checked_add(header.p_memsz)
                    .ok_or(Errno::EOVERFLOW)
                    .and_then(page_up)?;
                Ok((start, end))
            })
            .collect::<Result<Vec<_>, Errno>>()?;
        if relro.len() > 1 {
            return Err(self.caller_error("bound ELF has multiple PT_GNU_RELRO ranges"));
        }
        let mut isolated_page_seen = false;
        let mut page = first;
        while page < last {
            let owner = loads.iter().copied().rfind(|header| {
                let start = page_down(header.p_vaddr);
                header
                    .p_vaddr
                    .checked_add(header.p_memsz)
                    .and_then(|end| page_up(end).ok())
                    .is_some_and(|end| start <= page && page < end)
            });
            let target = bias.checked_add(page).ok_or(Errno::EOVERFLOW)?;
            let isolated_page = isolation.is_some_and(|isolation| {
                isolation.range.start == target
                    && target.checked_add(PAGE) == Some(isolation.range.end)
            });
            let mapping = maps
                .iter()
                .find(|mapping| mapping.start <= target && target < mapping.end);
            let Some(mapping) = mapping else {
                if elf.header.e_type == header::ET_DYN || owner.is_some() {
                    return Ok(Err(ImageGeometryMismatch::MissingPage));
                }
                page = page.checked_add(PAGE).ok_or(Errno::EOVERFLOW)?;
                continue;
            };
            if mapping.shared {
                return Ok(Err(ImageGeometryMismatch::SharedPage));
            }
            let relro_page = relro.iter().any(|range| range.0 <= page && page < range.1);
            if let Some(owner) = owner {
                let owner_start = page_down(owner.p_vaddr);
                let file_end = owner
                    .p_vaddr
                    .checked_add(owner.p_filesz)
                    .ok_or(Errno::EOVERFLOW)
                    .and_then(page_up)?;
                let readable = owner.p_flags & ph::PF_R != 0;
                let writable = owner.p_flags & ph::PF_W != 0 && !relro_page;
                let executable = owner.p_flags & ph::PF_X != 0;
                let expected_offset = page_down(owner.p_offset)
                    .checked_add(page - owner_start)
                    .ok_or(Errno::EOVERFLOW)?;
                if isolated_page {
                    let isolation = isolation.ok_or(Errno::EPROTO)?;
                    if !readable
                        || writable
                        || !executable
                        || page >= file_end
                        || isolation.file_offset != expected_offset
                        || !isolation.validates_mapping(task, mapping)
                    {
                        return Ok(Err(ImageGeometryMismatch::HelperIsolation));
                    }
                    isolated_page_seen = true;
                } else {
                    if mapping.readable != readable
                        || mapping.writable != writable
                        || mapping.executable != executable
                    {
                        return Ok(Err(ImageGeometryMismatch::LoadPermissions));
                    }
                    if page < file_end {
                        if mapping.mapping_identity() != identity {
                            return Ok(Err(ImageGeometryMismatch::LoadIdentity));
                        }
                        if mapping.offset.checked_add(target - mapping.start)
                            != Some(expected_offset)
                        {
                            return Ok(Err(ImageGeometryMismatch::LoadOffset));
                        }
                    } else if mapping.inode != 0 || mapping.path.is_some() {
                        return Ok(Err(ImageGeometryMismatch::AnonymousBssBacking));
                    }
                }
            } else {
                if isolated_page {
                    return Ok(Err(ImageGeometryMismatch::HelperIsolation));
                }
                if mapping.readable || mapping.writable || mapping.executable {
                    return Ok(Err(ImageGeometryMismatch::HolePermissions));
                }
                if mapping.mapping_identity() != identity {
                    return Ok(Err(ImageGeometryMismatch::HoleIdentity));
                }
                let expected_offset = page_down(first_load.p_offset)
                    .checked_add(page - first)
                    .ok_or(Errno::EOVERFLOW)?;
                if mapping.offset.checked_add(target - mapping.start) != Some(expected_offset) {
                    return Ok(Err(ImageGeometryMismatch::HoleOffset));
                }
            }
            page = page.checked_add(PAGE).ok_or(Errno::EOVERFLOW)?;
        }
        if isolation.is_some() && !isolated_page_seen {
            return Ok(Err(ImageGeometryMismatch::HelperIsolation));
        }

        for load in loads {
            let file_end = load
                .p_offset
                .checked_add(load.p_filesz)
                .ok_or(Errno::EOVERFLOW)?;
            if load.p_filesz > load.p_memsz
                || file_end > image.bytes.len() as u64
                || load.p_flags & !7 != 0
                || load.p_align > 1
                    && (!load.p_align.is_power_of_two()
                        || load.p_vaddr % load.p_align != load.p_offset % load.p_align)
            {
                return Err(self.caller_error("bound ELF PT_LOAD geometry is invalid"));
            }
            if load.p_flags & ph::PF_W == 0 && load.p_filesz != 0 {
                let mut target = bias.checked_add(load.p_vaddr).ok_or(Errno::EOVERFLOW)?;
                let mut offset = usize::try_from(load.p_offset).map_err(|_| Errno::EOVERFLOW)?;
                let end = usize::try_from(file_end).map_err(|_| Errno::EOVERFLOW)?;
                let mut observed = [0_u8; 65536];
                while offset < end {
                    let amount = (end - offset).min(observed.len());
                    read_exact_with_helper_isolation(
                        task,
                        target,
                        &mut observed[..amount],
                        isolation,
                    )?;
                    if !exact_geometry_bytes_match(
                        &observed[..amount],
                        &image.bytes[offset..offset + amount],
                    ) {
                        return Ok(Err(ImageGeometryMismatch::NonwritableBytes {
                            file_offset: offset as u64,
                        }));
                    }
                    offset += amount;
                    target = target.checked_add(amount as u64).ok_or(Errno::EOVERFLOW)?;
                }
            }
        }
        Ok(Ok(()))
    }

    fn resolve_after_loader_image_geometry(
        &self,
        task: &Stopped,
        image: &LiteinstCallerImage,
        image_id: AfterLoaderImageId,
        phase: &'static str,
    ) -> Result<ResolvedImageGeometry, Error> {
        self.resolve_after_loader_image_geometry_with_isolation(task, image, image_id, phase, None)
    }

    fn resolve_after_loader_image_geometry_with_isolation(
        &self,
        task: &Stopped,
        image: &LiteinstCallerImage,
        image_id: AfterLoaderImageId,
        phase: &'static str,
        isolation: Option<&AfterLoaderHelperIsolation>,
    ) -> Result<ResolvedImageGeometry, Error> {
        let elf = Elf::parse(&image.bytes)
            .map_err(|error| self.caller_error(format!("bound ELF parse failed: {error}")))?;
        if elf.is_64 == false
            || elf.little_endian == false
            || elf.header.e_machine != header::EM_X86_64
            || !matches!(elf.header.e_type, header::ET_DYN | header::ET_EXEC)
            || elf.program_headers.len() > 128
        {
            return Err(self.caller_error("bound ELF header is outside the fixed x86-64 graph"));
        }
        let loads = elf
            .program_headers
            .iter()
            .filter(|program| program.p_type == ph::PT_LOAD)
            .collect::<Vec<_>>();
        if loads.is_empty() {
            return Err(self.caller_error("bound ELF has no PT_LOAD"));
        }
        for (index, load) in loads.iter().enumerate() {
            let end = load
                .p_vaddr
                .checked_add(load.p_memsz)
                .ok_or(Errno::EOVERFLOW)?;
            if loads[..index].iter().any(|prior| {
                prior
                    .p_vaddr
                    .checked_add(prior.p_memsz)
                    .is_none_or(|prior_end| load.p_vaddr < prior_end && prior.p_vaddr < end)
            }) {
                return Err(self.caller_error("bound ELF PT_LOAD memory ranges overlap"));
            }
        }
        for relro in elf
            .program_headers
            .iter()
            .filter(|program| program.p_type == ph::PT_GNU_RELRO)
        {
            let end = relro
                .p_vaddr
                .checked_add(relro.p_memsz)
                .ok_or(Errno::EOVERFLOW)?;
            if relro.p_memsz == 0
                || !loads.iter().any(|load| {
                    load.p_flags & ph::PF_W != 0
                        && load.p_vaddr <= relro.p_vaddr
                        && load
                            .p_vaddr
                            .checked_add(load.p_memsz)
                            .is_some_and(|load_end| end <= load_end)
                })
            {
                return Err(
                    self.caller_error("bound ELF PT_GNU_RELRO is outside one writable PT_LOAD")
                );
            }
        }
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let mut candidates = BTreeSet::new();
        for mapping in maps.iter().filter(|mapping| mapping.inode != 0) {
            for load in &loads {
                let file_start = page_down(load.p_offset);
                let file_end = load
                    .p_offset
                    .checked_add(load.p_filesz)
                    .ok_or(Errno::EOVERFLOW)
                    .and_then(page_up)?;
                if file_start <= mapping.offset && mapping.offset < file_end {
                    let relative = page_down(load.p_vaddr)
                        .checked_add(mapping.offset - file_start)
                        .ok_or(Errno::EOVERFLOW)?;
                    if let Some(bias) = mapping.start.checked_sub(relative) {
                        candidates.insert((mapping.mapping_identity(), bias));
                    }
                }
            }
        }
        if elf.header.e_type == header::ET_EXEC {
            candidates.retain(|(_, bias)| *bias == 0);
        }
        let candidate_count = candidates.len();
        let mut accepted = Vec::new();
        let mut first_rejection = None;
        for (identity, bias) in candidates {
            match self.after_loader_image_geometry_matches(
                task, image, identity, &elf, bias, &maps, isolation,
            )? {
                Ok(()) => accepted.push((identity, bias)),
                Err(reason) if first_rejection.is_none() => {
                    first_rejection = Some(format!(
                        "mapping={:x}:{:x}:{} bias={bias:#x} reason={reason:?}",
                        identity.device_major, identity.device_minor, identity.inode,
                    ));
                }
                Err(_) => {}
            }
        }
        let Some((mapping, load_bias)) = one_exact_geometry_candidate(&accepted) else {
            return Err(self.caller_error(format!(
                "phase={phase} bound image path={} file_device={} file_inode={} has {} exact retained load geometries among {} map-derived candidates; first_rejection={}",
                image.path.display(),
                image.file_identity.device,
                image.file_identity.inode,
                accepted.len(),
                candidate_count,
                first_rejection.as_deref().unwrap_or("none"),
            )));
        };
        let first = loads
            .iter()
            .map(|load| page_down(load.p_vaddr))
            .min()
            .ok_or(Errno::EPROTO)?;
        let last = loads
            .iter()
            .map(|load| {
                load.p_vaddr
                    .checked_add(load.p_memsz)
                    .ok_or(Errno::EOVERFLOW)
                    .and_then(page_up)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max()
            .ok_or(Errno::EPROTO)?;
        let target_first = load_bias.checked_add(first).ok_or(Errno::EOVERFLOW)?;
        let target_last = load_bias.checked_add(last).ok_or(Errno::EOVERFLOW)?;
        if self
            .after_loader_private_state()?
            .owned_mappings
            .iter()
            .any(|mapping| {
                matches!(
                    mapping.purpose,
                    AfterLoaderMappingPurpose::Image { image: mapped }
                        | AfterLoaderMappingPurpose::ImageZeroFill { image: mapped }
                        if mapped == image_id
                ) && (mapping.start < target_first || target_last < mapping.end)
            })
        {
            return Err(self.caller_error(
                "owned image mapping extends outside its exact PT_LOAD reservation",
            ));
        }
        Ok(ResolvedImageGeometry {
            image: image_id,
            mapping,
            load_bias,
            span: (target_first, target_last),
        })
    }

    fn authenticate_after_loader_image_backing(
        &self,
        task: &Stopped,
        image: &LiteinstCallerImage,
        geometry: ResolvedImageGeometry,
    ) -> Result<(), Error> {
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        if !exact_image_backing_paths_match(task.pid(), image, geometry, &maps)
            .map_err(|error| self.caller_error(error))?
        {
            return Err(self.caller_error(
                "target backing path bytes, identity or maps-domain bridge differs",
            ));
        }
        Ok(())
    }

    fn bind_after_loader_initial_image_geometries(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        self.require_after_loader_deferred_images_absent(task, config)?;
        self.bind_after_loader_image_geometries(
            task,
            std::iter::once(&config.executable)
                .chain(std::iter::once(&config.provider))
                .chain(config.initial_dependencies.iter())
                .collect(),
            "initial-entry",
            "bound target image geometry",
            false,
        )
    }

    fn bind_after_loader_deferred_image_geometries(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        self.bind_after_loader_image_geometries(
            task,
            config.deferred_dependencies.iter().collect(),
            "post-dlopen-deferred",
            "bound target image geometry",
            true,
        )
    }

    fn bind_after_loader_image_geometries<'a>(
        &mut self,
        task: &Stopped,
        selected: Vec<&'a LiteinstCallerImage>,
        phase: &'static str,
        operation: &'static str,
        require_causal_mapping: bool,
    ) -> Result<(), Error> {
        let mut images = BTreeMap::<crate::after_loader::FileIdentity, &LiteinstCallerImage>::new();
        for image in selected {
            if let Some(previous) = images.insert(image.file_identity, image)
                && previous.bytes != image.bytes
            {
                return Err(
                    self.caller_error("bound graph reuses a file identity for different bytes")
                );
            }
        }
        let mut geometries = Vec::with_capacity(images.len());
        for image in images.values() {
            let image_id = self.after_loader_private_state()?.image_id(image);
            let geometry =
                self.resolve_after_loader_image_geometry(task, image, image_id, phase)?;
            self.authenticate_after_loader_image_backing(task, image, geometry)?;
            if require_causal_mapping
                && !self
                    .after_loader_private_state()?
                    .has_causal_image_mapping(geometry)
            {
                return Err(self.caller_error(
                    "deferred image geometry lacked its exact loader-mmap identity",
                ));
            }
            let soname = image
                .dynamic_soname()
                .unwrap_or_else(|| "<executable>".to_owned());
            self.caller_observe(
                "resolved target image geometry",
                format!(
                    "phase={phase} soname={soname} path={} file_device={} file_inode={} mapping_device={:x}:{:x} mapping_inode={} load_bias={:#x} span={:#x}-{:#x}",
                    image.path.display(),
                    image.file_identity.device,
                    image.file_identity.inode,
                    geometry.mapping.device_major,
                    geometry.mapping.device_minor,
                    geometry.mapping.inode,
                    geometry.load_bias,
                    geometry.span.0,
                    geometry.span.1,
                ),
            )?;
            geometries.push((*image, geometry));
        }
        for (_, geometry) in &geometries {
            if !self
                .after_loader_private_state_mut()?
                .bind_image_geometry(*geometry)
            {
                return Err(self.caller_error(
                    "bound graph mapping identities collide or changed while binding",
                ));
            }
        }
        for (image, geometry) in geometries {
            let soname = image
                .dynamic_soname()
                .unwrap_or_else(|| "<executable>".to_owned());
            self.caller_observe(
                operation,
                format!(
                    "phase={phase} soname={soname} path={} file_device={} file_inode={} mapping_device={:x}:{:x} mapping_inode={} load_bias={:#x} span={:#x}-{:#x}",
                    image.path.display(),
                    image.file_identity.device,
                    image.file_identity.inode,
                    geometry.mapping.device_major,
                    geometry.mapping.device_minor,
                    geometry.mapping.inode,
                    geometry.load_bias,
                    geometry.span.0,
                    geometry.span.1,
                ),
            )?;
        }
        Ok(())
    }

    fn require_after_loader_deferred_images_absent(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        for image in &config.deferred_dependencies {
            let soname = image
                .dynamic_soname()
                .ok_or_else(|| self.caller_error("deferred loader image lost its DT_SONAME"))?;
            let identity = target_bound_image_mapping_identity(task.pid(), image)
                .map_err(|error| self.caller_error(error))?
                .ok_or_else(|| {
                    self.caller_error(
                        "deferred graph backing bytes or file identity changed before dlopen",
                    )
                })?;
            if maps
                .iter()
                .any(|mapping| mapping.mapping_identity() == identity)
            {
                return Err(self.caller_error(format!(
                    "runtime-only dependency was already mapped before dlopen: soname={soname} path={} file_device={} file_inode={} mapping_device={:x}:{:x} mapping_inode={}",
                    image.path.display(),
                    image.file_identity.device,
                    image.file_identity.inode,
                    identity.device_major,
                    identity.device_minor,
                    identity.inode,
                )));
            }
            self.caller_observe(
                "deferred target image absent before dlopen",
                format!(
                    "soname={soname} path={} file_device={} file_inode={} mapping_device={:x}:{:x} mapping_inode={}",
                    image.path.display(),
                    image.file_identity.device,
                    image.file_identity.inode,
                    identity.device_major,
                    identity.device_minor,
                    identity.inode,
                ),
            )?;
        }
        Ok(())
    }

    fn validate_after_loader_retained_resources(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        isolation: Option<&AfterLoaderHelperIsolation>,
    ) -> Result<(), Error> {
        let state = self.after_loader_private_state()?;
        if isolation.is_some_and(|isolation| !isolation.validates_live_page(task)) {
            return Err(self.caller_error(
                "isolated helper page lost its exact VMA, pkey, backing or PTRACE bytes",
            ));
        }
        if !state.owned_descriptors.is_empty() || state.current_break.is_none() {
            return Err(self.caller_error(
                "private descriptors survived or the current break was never bound",
            ));
        }
        if state
            .owned_mappings
            .iter()
            .any(|mapping| matches!(mapping.purpose, AfterLoaderMappingPurpose::Controller))
        {
            return Err(self.caller_error("controller scratch mapping survived restoration"));
        }
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        for owned in &state.owned_mappings {
            let exact_boundaries = matches!(
                owned.purpose,
                AfterLoaderMappingPurpose::SharedReservation { .. }
                    | AfterLoaderMappingPurpose::Trampoline { .. }
            );
            let mapped = maps.iter().find(|mapping| {
                (if exact_boundaries {
                    mapping.start == owned.start && mapping.end == owned.end
                } else {
                    mapping.start <= owned.start && owned.end <= mapping.end
                }) && mapping.readable == owned.readable
                    && mapping.writable == owned.writable
                    && mapping.executable == owned.executable
                    && mapping.shared == owned.shared
            });
            let Some(mapped) = mapped else {
                return Err(self.caller_error("retained owned mapping geometry changed"));
            };
            match owned.purpose {
                AfterLoaderMappingPurpose::Image { image } => {
                    if state.image_mappings.get(&image).copied() != Some(mapped.mapping_identity())
                    {
                        return Err(self.caller_error("retained image mapping identity changed"));
                    }
                }
                AfterLoaderMappingPurpose::ImageZeroFill { .. } => {
                    if mapped.inode != 0 || mapped.path.is_some() {
                        return Err(
                            self.caller_error("retained anonymous mapping acquired file identity")
                        );
                    }
                }
                AfterLoaderMappingPurpose::SharedReservation { trampoline } => {
                    let Some(identity) = exact_shared_reservation_identity(mapped) else {
                        return Err(self.caller_error(
                            "retained shared reservation lost its exact Linux identity",
                        ));
                    };
                    if state.shared_reservations.get(&trampoline)
                        != Some(&((owned.start, owned.end), identity))
                        || owned.end - owned.start != PAGE
                        || !owned.readable
                        || !owned.writable
                        || owned.executable
                        || !owned.shared
                        || owned.descriptor.is_some()
                        || owned.offset != 0
                    {
                        return Err(self.caller_error(
                            "retained shared reservation identity or geometry changed",
                        ));
                    }
                }
                AfterLoaderMappingPurpose::Trampoline { trampoline } => {
                    if state.trampoline_mapping(trampoline) != Some(mapped.mapping_identity())
                        || mapped.offset != 0
                        || owned.end - owned.start != TRAMPOLINE_ARENA_SIZE
                    {
                        return Err(self.caller_error(
                            "retained trampoline alias identity or geometry changed",
                        ));
                    }
                }
                AfterLoaderMappingPurpose::Controller => unreachable!(),
            }
        }
        let mut trampoline_identities = BTreeSet::new();
        for mapping in &state.owned_mappings {
            if let AfterLoaderMappingPurpose::Trampoline { trampoline } = mapping.purpose {
                trampoline_identities.insert(trampoline);
            }
        }
        if trampoline_identities.is_empty() {
            return Err(self.caller_error("initializer retained no bound trampoline arena"));
        }
        let reservation_identities = state
            .shared_reservations
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        if reservation_identities != trampoline_identities {
            return Err(self.caller_error(
                "retained trampoline aliases and shared reservations are not bijective",
            ));
        }
        if state.sealed_trampolines != trampoline_identities {
            return Err(self
                .caller_error("retained trampoline aliases and verified seals are not bijective"));
        }
        for trampoline in trampoline_identities {
            let aliases = state
                .owned_mappings
                .iter()
                .filter(|mapping| {
                    mapping.purpose == AfterLoaderMappingPurpose::Trampoline { trampoline }
                })
                .collect::<Vec<_>>();
            let reservations = state
                .owned_mappings
                .iter()
                .filter(|mapping| {
                    mapping.purpose == (AfterLoaderMappingPurpose::SharedReservation { trampoline })
                })
                .collect::<Vec<_>>();
            if aliases.len() != 2
                || aliases.iter().filter(|mapping| mapping.writable).count() != 1
                || aliases.iter().filter(|mapping| mapping.executable).count() != 1
                || aliases.iter().any(|mapping| {
                    !mapping.shared
                        || mapping.offset != 0
                        || mapping.end - mapping.start != TRAMPOLINE_ARENA_SIZE
                })
                || ranges_overlap(
                    (aliases[0].start, aliases[0].end),
                    (aliases[1].start, aliases[1].end),
                )
                || reservations.len() != 1
                || ranges_overlap(
                    (aliases[0].start, aliases[0].end),
                    (reservations[0].start, reservations[0].end),
                )
                || ranges_overlap(
                    (aliases[1].start, aliases[1].end),
                    (reservations[0].start, reservations[0].end),
                )
            {
                return Err(self.caller_error("retained trampoline resource set changed"));
            }
        }

        let mut images = BTreeMap::<crate::after_loader::FileIdentity, &LiteinstCallerImage>::new();
        for image in std::iter::once(&config.executable)
            .chain(std::iter::once(&config.provider))
            .chain(config.dependencies.iter())
        {
            if let Some(previous) = images.insert(image.file_identity, image)
                && previous.bytes != image.bytes
            {
                return Err(self.caller_error("bound graph reuses an identity for different bytes"));
            }
        }
        if let Some(previous) = images.insert(
            config.sealed_runtime.image.file_identity,
            &config.sealed_runtime.image,
        ) && previous.bytes != config.sealed_runtime.image.bytes
        {
            return Err(
                self.caller_error("sealed runtime identity is reused for different bound bytes")
            );
        }
        for image in images.into_values() {
            let image_id = state.image_id(image);
            let image_isolation = isolation.filter(|isolation| isolation.image == image_id);
            let geometry = self.resolve_after_loader_image_geometry_with_isolation(
                task,
                image,
                image_id,
                "final-retained",
                image_isolation,
            )?;
            if state.image_geometry(image) != Some(geometry) {
                return Err(self.caller_error(
                    "bound image mapping identity or exact retained geometry changed",
                ));
            }
            if image_id != state.image_id(&config.sealed_runtime.image) {
                self.authenticate_after_loader_image_backing(task, image, geometry)?;
            }
        }
        let runtime = state.image_id(&config.sealed_runtime.image);
        if isolation.is_some_and(|isolation| isolation.image != runtime) {
            return Err(
                self.caller_error("isolated helper authority names a different retained image")
            );
        }
        if !state.owned_mappings.iter().any(|mapping| {
            matches!(
                mapping.purpose,
                AfterLoaderMappingPurpose::Image { image }
                    | AfterLoaderMappingPurpose::ImageZeroFill { image }
                    if image == runtime
            )
        }) {
            return Err(self.caller_error("sealed runtime retained no owned load mapping"));
        }
        Ok(())
    }

    pub(super) fn arm_after_loader_syscall_permit(
        &mut self,
        task: &Stopped,
        purpose: AfterLoaderSyscallPurpose,
        number: i64,
        args: [u64; 6],
        instruction_pointer: u64,
    ) -> Result<(), TraceError> {
        self.arm_after_loader_syscall_permit_with_instruction(
            task,
            purpose,
            number,
            args,
            instruction_pointer,
            [0x0f, 0x05, 0x0f, 0x0b],
        )
    }

    pub(super) fn arm_after_loader_syscall_permit_with_instruction(
        &mut self,
        task: &Stopped,
        purpose: AfterLoaderSyscallPurpose,
        number: i64,
        args: [u64; 6],
        instruction_pointer: u64,
        expected_instruction: [u8; 4],
    ) -> Result<(), TraceError> {
        if self.after_loader_config().is_none() {
            return Ok(());
        }
        if self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
            || self.liteinst_after_loader_private_call.is_some()
        {
            return Err(Errno::EALREADY.into());
        }
        let mut instruction = [0; 4];
        task.read_exact(instruction_pointer as usize, &mut instruction)?;
        if instruction != expected_instruction || instruction[..2] != [0x0f, 0x05] {
            return Err(Errno::EPROTO.into());
        }
        let resume_pointer = instruction_pointer.checked_add(2).ok_or(Errno::EOVERFLOW)?;
        let image = self
            .after_loader_identity(task)
            .map_err(|_| TraceError::from(Errno::EPROTO))?;
        let output_spans = syscall_output_spans(number, args)?;
        self.liteinst_after_loader_syscall_permit = Some(AfterLoaderSyscallPermit {
            image: Some(image),
            tid: task.pid(),
            generation: image.generation,
            origin_status: task.physical_status_id(),
            admission_status: None,
            purpose,
            number,
            args,
            instruction_pointer,
            resume_pointer,
            instruction,
            instruction_length: 4,
            output_spans,
            effect: AfterLoaderSyscallEffect::None,
        });
        Ok(())
    }

    pub(super) fn consume_after_loader_syscall_permit(
        &mut self,
        task: &Stopped,
    ) -> Result<Option<AfterLoaderSyscallPermit>, TraceError> {
        if self.after_loader_config().is_none() {
            return Ok(None);
        }
        if self.liteinst_after_loader_syscall_inflight.is_some() {
            return Err(Errno::EALREADY.into());
        }
        let mut permit = self
            .liteinst_after_loader_syscall_permit
            .take()
            .ok_or(Errno::EPROTO)?;
        let generation = self.liteinst_runtime.lock().unwrap().generation;
        let identity = match permit.image {
            Some(_) => Some(
                self.after_loader_identity(task)
                    .map_err(|_| TraceError::from(Errno::EPROTO))?,
            ),
            None => None,
        };
        let regs = task.getregs()?;
        let mut instruction = [0; 4];
        let instruction_length = usize::from(permit.instruction_length);
        task.read_exact(
            permit.instruction_pointer as usize,
            &mut instruction[..instruction_length],
        )?;
        let admission_status = task.physical_status_id();
        let origin_status = permit.origin_status.ok_or(Errno::EPROTO)?;
        let admission_status = admission_status.ok_or(Errno::EPROTO)?;
        if identity != permit.image
            || task.pid() != permit.tid
            || generation != permit.generation
            || origin_status == admission_status
            || permit
                .admission_status
                .is_some_and(|expected| expected != admission_status)
            || !after_loader_syscall_registers_match(permit.number, permit.args, &regs)
            || regs.rip != permit.resume_pointer
            || instruction[..instruction_length] != permit.instruction[..instruction_length]
        {
            return Err(Errno::EPROTO.into());
        }
        permit.admission_status = Some(admission_status);
        if let Some(config) = self.after_loader_config() {
            config.diagnostics.record(
                "private syscall permit consumed",
                self.timer.diagnostic_clock(),
                format!(
                    "tid={} generation={} origin_status={:?} admission_status={:?} purpose={:?} nr={} args={:?} rip={:#x} outputs={:?}",
                    task.pid(),
                    permit.generation,
                    permit.origin_status,
                    permit.admission_status,
                    permit.purpose,
                    permit.number,
                    permit.args,
                    permit.instruction_pointer,
                    permit.output_spans,
                ),
            ).map_err(|_| Errno::EOVERFLOW)?;
        }
        self.liteinst_after_loader_syscall_inflight = Some(permit.clone());
        Ok(Some(permit))
    }

    fn take_after_loader_syscall_inflight(
        &mut self,
        task: &Stopped,
        syscall_completion: bool,
    ) -> Result<Option<AfterLoaderSyscallPermit>, TraceError> {
        if self.after_loader_config().is_none() {
            return Ok(None);
        }
        let permit = self
            .liteinst_after_loader_syscall_inflight
            .take()
            .ok_or(Errno::EPROTO)?;
        let origin_status = permit.origin_status.ok_or(Errno::EPROTO)?;
        let admission_status = permit.admission_status.ok_or(Errno::EPROTO)?;
        let completion_status = task.physical_status_id().ok_or(Errno::EPROTO)?;
        if completion_status == origin_status || completion_status == admission_status {
            return Err(Errno::EPROTO.into());
        }
        let generation = self.liteinst_runtime.lock().unwrap().generation;
        let identity = match permit.image {
            Some(_) => Some(
                self.after_loader_identity(task)
                    .map_err(|_| TraceError::from(Errno::EPROTO))?,
            ),
            None => None,
        };
        if task.pid() != permit.tid || generation != permit.generation || identity != permit.image {
            return Err(Errno::EPROTO.into());
        }
        if syscall_completion {
            let regs = task.getregs()?;
            let instruction_length = usize::from(permit.instruction_length);
            let mut instruction = [0; 4];
            task.read_exact(
                permit.instruction_pointer as usize,
                &mut instruction[..instruction_length],
            )?;
            if !after_loader_syscall_registers_match(permit.number, permit.args, &regs)
                || regs.rip != permit.resume_pointer
                || instruction[..instruction_length] != permit.instruction[..instruction_length]
            {
                return Err(Errno::EPROTO.into());
            }
        }
        Ok(Some(permit))
    }

    pub(super) fn complete_after_loader_syscall_inflight(
        &mut self,
        task: &Stopped,
    ) -> Result<Option<AfterLoaderSyscallPermit>, TraceError> {
        self.take_after_loader_syscall_inflight(task, true)
    }

    pub(super) fn complete_after_loader_syscall_successor(
        &mut self,
        task: &Stopped,
    ) -> Result<Option<AfterLoaderSyscallPermit>, TraceError> {
        self.take_after_loader_syscall_inflight(task, false)
    }

    pub(super) fn classify_after_loader_trace_only_syscall(
        &self,
        task: &Stopped,
    ) -> Result<Option<AfterLoaderTraceOnlySyscall>, TraceError> {
        if self.after_loader_config().is_none() {
            return Ok(None);
        }
        if self.liteinst_after_loader_private_state.is_some()
            || self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
            || self.liteinst_after_loader_forward_inflight.is_some()
            || self.liteinst_after_loader_private_call.is_some()
        {
            return Err(Errno::EPROTO.into());
        }
        let regs = task.getregs()?;
        let (raw, known) = classify_trace_only_syscall_number(regs.orig_rax)?;
        let subscribed = known.is_some_and(|number| {
            number != Sysno::rt_sigreturn
                && self
                    .global_state
                    .subscriptions
                    .iter_syscalls()
                    .any(|candidate| candidate == number)
        });
        if subscribed {
            return Ok(None);
        }
        let runtime = self.liteinst_runtime.lock().unwrap();
        let purpose = match runtime.phase {
            LiteinstRuntimePhase::PreExec => {
                if !self.command_bootstrap {
                    return Err(Errno::EPROTO.into());
                }
                if known == Some(Sysno::execve) {
                    let config = self.after_loader_config().ok_or(Errno::EPROTO)?;
                    self.validate_after_loader_command_execve(task, &config, &regs)
                        .map_err(|_| TraceError::from(Errno::EPROTO))?;
                }
                AfterLoaderSyscallPurpose::TraceePreinit
            }
            LiteinstRuntimePhase::Waiting => AfterLoaderSyscallPurpose::TraceePreinit,
            LiteinstRuntimePhase::Ready
                if runtime.ready_generation == Some(runtime.generation)
                    && runtime.after_loader_reference.is_some() =>
            {
                AfterLoaderSyscallPurpose::OrdinaryForward
            }
            LiteinstRuntimePhase::Bootstrap | LiteinstRuntimePhase::Ready => {
                return Err(Errno::EPROTO.into());
            }
        };
        Ok(Some(AfterLoaderTraceOnlySyscall {
            purpose,
            number: raw,
            args: [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9],
            known,
        }))
    }

    fn begin_after_loader_forward(
        &mut self,
        task: &Stopped,
        number: i64,
        args: [u64; 6],
        kind: AfterLoaderForwardKind,
    ) -> Result<(), Error> {
        if self.liteinst_after_loader_forward_inflight.is_some() {
            return Err(self.caller_error("trace-only syscall forwarding is already in flight"));
        }
        let registers = task.getregs()?;
        let instruction_pointer = registers.rip.checked_sub(2).ok_or(Errno::EPROTO)?;
        let mut instruction = [0; 2];
        task.read_exact(instruction_pointer as usize, &mut instruction)?;
        let generation = self.liteinst_runtime.lock().unwrap().generation;
        let image = if generation == 0 {
            None
        } else {
            Some(self.after_loader_identity(task)?)
        };
        let admission_status = task.physical_status_id().ok_or_else(|| {
            self.caller_error("trace-only syscall admission has no physical status")
        })?;
        if instruction != [0x0f, 0x05]
            || registers.orig_rax as i64 != number
            || [
                registers.rdi,
                registers.rsi,
                registers.rdx,
                registers.r10,
                registers.r8,
                registers.r9,
            ] != args
        {
            return Err(self.caller_error("trace-only syscall changed at forwarding admission"));
        }
        self.liteinst_after_loader_forward_inflight = Some(AfterLoaderForwardInFlight {
            tid: task.pid(),
            generation,
            image,
            physical_generation: task.physical_event_generation(),
            observed_statuses: BTreeSet::from([admission_status]),
            instruction_pointer,
            resume_pointer: registers.rip,
            instruction,
            number,
            args,
            kind,
        });
        Ok(())
    }

    fn validate_after_loader_forward_stop(
        &self,
        state: &mut AfterLoaderForwardInFlight,
        task: &Stopped,
    ) -> Result<(), Error> {
        let status = task.physical_status_id().ok_or_else(|| {
            self.caller_error("trace-only forwarding stop has no physical status")
        })?;
        let current_generation = self.liteinst_runtime.lock().unwrap().generation;
        let image = match state.image {
            Some(_) => Some(self.after_loader_identity(task)?),
            None => None,
        };
        if task.pid() != state.tid
            || task.physical_event_generation() != state.physical_generation
            || current_generation != state.generation
            || image != state.image
            || !state.observed_statuses.insert(status)
        {
            return Err(
                self.caller_error("trace-only forwarding reused a stop or changed task identity")
            );
        }
        Ok(())
    }

    fn validate_after_loader_forward_completion(
        &self,
        state: &AfterLoaderForwardInFlight,
        task: &Stopped,
    ) -> Result<libc::user_regs_struct, Error> {
        let registers = task.getregs()?;
        let mut instruction = [0; 2];
        task.read_exact(state.instruction_pointer as usize, &mut instruction)?;
        if registers.orig_rax as i64 != state.number
            || [
                registers.rdi,
                registers.rsi,
                registers.rdx,
                registers.r10,
                registers.r8,
                registers.r9,
            ] != state.args
            || registers.rip != state.resume_pointer
            || instruction != state.instruction
        {
            return Err(self.caller_error("trace-only syscall completion identity changed"));
        }
        Ok(registers)
    }

    pub(super) async fn route_after_loader_forward_inflight(
        &mut self,
        task: Stopped,
        event: &Event,
    ) -> Result<AfterLoaderForwardRoute, Error> {
        let mut state = self
            .liteinst_after_loader_forward_inflight
            .take()
            .ok_or_else(|| self.caller_error("trace-only forwarding state is absent"))?;
        self.validate_after_loader_forward_stop(&mut state, &task)?;
        if matches!(event, Event::Signal(_)) {
            self.liteinst_after_loader_forward_inflight = Some(state);
            return Ok(AfterLoaderForwardRoute::Continue(task));
        }
        if matches!(event, Event::Exec(_))
            && matches!(
                &state.kind,
                AfterLoaderForwardKind::TraceOnly(AfterLoaderTraceOnlySyscall {
                    purpose: AfterLoaderSyscallPurpose::TraceePreinit,
                    known: Some(Sysno::execve),
                    ..
                })
            )
        {
            self.caller_observe(
                "trace-only initial exec completed",
                format!("nr={} args={:?}", state.number, state.args),
            )?;
            return Ok(AfterLoaderForwardRoute::Continue(task));
        }
        if !matches!(event, Event::Syscall) {
            return Err(self.caller_error(format!(
                "trace-only syscall produced an unexpected in-flight event: {}",
                after_loader_event_summary(event),
            )));
        }
        let completed = self.validate_after_loader_forward_completion(&state, &task)?;
        let AfterLoaderForwardKind::TraceOnly(operation) = state.kind;
        let raw_result = completed.rax as i64;
        if let Some(number) = operation.known {
            let args = SyscallArgs::new(
                operation.args[0] as usize,
                operation.args[1] as usize,
                operation.args[2] as usize,
                operation.args[3] as usize,
                operation.args[4] as usize,
                operation.args[5] as usize,
            );
            self.observe_liteinst_mapping_result(
                number,
                args,
                Errno::from_ret(completed.rax as usize).map(|value| value as i64),
            );
        }
        self.caller_observe(
            "trace-only syscall completed",
            format!(
                "purpose={:?} nr={} raw_result={raw_result}",
                operation.purpose, operation.number
            ),
        )?;
        let signal = self
            .take_pending_signal_for_resume(
                &task,
                LiteinstActivationOperation::ResumeInjectedSyscall,
            )?;
        let wait = self.resume_stopped(task, signal)?.next_state().await?;
        self.arm_liteinst_wait(&wait)?;
        Ok(AfterLoaderForwardRoute::Completed(wait))
    }

    pub(super) async fn forward_after_loader_trace_only_syscall(
        &mut self,
        task: Stopped,
        operation: AfterLoaderTraceOnlySyscall,
    ) -> Result<Wait, Error> {
        let repeated = self
            .classify_after_loader_trace_only_syscall(&task)?
            .ok_or(Errno::EPROTO)?;
        if repeated != operation {
            return Err(self.caller_error("trace-only syscall changed before admission"));
        }
        self.observe_after_loader_stopped_event(&task, &Event::Seccomp)?;

        if let Some(number) = operation.known {
            let args = SyscallArgs::new(
                operation.args[0] as usize,
                operation.args[1] as usize,
                operation.args[2] as usize,
                operation.args[3] as usize,
                operation.args[4] as usize,
                operation.args[5] as usize,
            );
            self.validate_liteinst_mapping_execution(number, args)?;
        }

        self.caller_observe(
            "trace-only syscall admitted",
            format!(
                "purpose={:?} nr={} args={:?}",
                operation.purpose, operation.number, operation.args
            ),
        )?;
        if operation.known.is_some_and(is_task_creating_syscall) {
            self.deopt_liteinst_hooks_quiescent(
                &task,
                LiteinstDeoptProgramCounter::TranslateGenerated,
            )?;
        }
        if operation.known == Some(Sysno::rt_sigreturn) {
            // Linux consumes the original signal frame and resumes at the
            // restored context. There is no syscall-return stop to wait for,
            // so retire every displaced site before that arbitrary RIP can
            // execute.
            self.deopt_liteinst_hooks_quiescent(
                &task,
                LiteinstDeoptProgramCounter::TranslateGenerated,
            )?;
            let wait = self.resume_stopped(task, None)?.next_state().await?;
            self.arm_liteinst_wait(&wait)?;
            self.caller_observe(
                "trace-only rt_sigreturn tail completed",
                format!("purpose={:?}", operation.purpose),
            )?;
            return Ok(wait);
        }
        self.begin_after_loader_forward(
            &task,
            operation.number,
            operation.args,
            AfterLoaderForwardKind::TraceOnly(operation),
        )?;
        let wait = self.syscall_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait)?;
        Ok(wait)
    }

    pub(super) fn observe_after_loader_stopped_event(
        &self,
        task: &Stopped,
        event: &Event,
    ) -> Result<(), TraceError> {
        let Some(config) = self.after_loader_config() else {
            return Ok(());
        };
        let mut runtime = self.liteinst_runtime.lock().unwrap();
        if runtime.phase != LiteinstRuntimePhase::Ready
            || runtime.after_loader_guest_observed
            || runtime.after_loader_reference.is_none()
        {
            return Ok(());
        }
        let regs = task.getregs()?;
        config
            .diagnostics
            .record(
                "first ordinary guest event after restoration",
                self.timer.diagnostic_clock(),
                format!(
                    "tid={} generation={} physical_status={:?} event={} rip={:#x} syscall={}",
                    task.pid(),
                    runtime.generation,
                    task.physical_status_id(),
                    after_loader_event_summary(event),
                    regs.rip,
                    regs.orig_rax
                ),
            )
            .map_err(|_| Errno::EOVERFLOW)?;
        runtime.after_loader_guest_observed = true;
        Ok(())
    }

    pub(super) fn observe_after_loader_terminal_event(
        &self,
        pid: Pid,
        exit_status: ExitStatus,
    ) -> Result<(), Error> {
        let Some(config) = self.after_loader_config() else {
            return Ok(());
        };
        let mut runtime = self.liteinst_runtime.lock().unwrap();
        if runtime.phase != LiteinstRuntimePhase::Ready
            || runtime.after_loader_guest_observed
            || runtime.after_loader_reference.is_none()
        {
            return Ok(());
        }
        config
            .diagnostics
            .record(
                "first ordinary guest event after restoration",
                self.timer.diagnostic_clock(),
                format!(
                    "tid={pid} generation={} event=Exited({exit_status:?}) registers=unavailable",
                    runtime.generation
                ),
            )
            .map_err(|error| self.caller_error(error))?;
        runtime.after_loader_guest_observed = true;
        Ok(())
    }

    pub(super) fn observe_after_loader_tool_callback(&self, callback: &str) -> Result<(), Error> {
        let Some(context) = self.after_loader_tool_callback_context() else {
            return Ok(());
        };
        context.record(callback)
    }

    pub(super) fn after_loader_tool_callback_context(
        &self,
    ) -> Option<AfterLoaderToolCallbackContext> {
        let config = self.after_loader_config()?;
        let runtime = self.liteinst_runtime.lock().unwrap();
        Some(AfterLoaderToolCallbackContext {
            diagnostics: config.diagnostics,
            raw_clock: self.timer.diagnostic_clock(),
            tid: self.tid(),
            generation: runtime.generation,
            phase: runtime.phase,
            physical_status: self
                .active_tool_stop
                .as_ref()
                .and_then(Stopped::physical_status_id),
        })
    }

    pub(super) fn after_loader_identity(&self, task: &Stopped) -> Result<ImageIdentity, Error> {
        let tid = task.pid();
        if tid != self.tid() {
            return Err(self.caller_error("stopped TID changed"));
        }
        let bytes = bounded_proc(format!("/proc/{tid}/stat"), 64 * 1024)
            .map_err(|e| self.caller_error(e))?;
        let text = std::str::from_utf8(&bytes).map_err(|e| self.caller_error(e))?;
        let close = text.rfind(')').ok_or(Errno::EPROTO)?;
        let fields: Vec<_> = text[close + 1..].split_whitespace().collect();
        let start_ticks = fields
            .get(19)
            .ok_or(Errno::EPROTO)?
            .parse::<u64>()
            .map_err(|e| self.caller_error(e))?;
        if text.split_whitespace().next() != Some(tid.as_raw().to_string().as_str()) {
            return Err(self.caller_error("proc stat TID disagrees"));
        }
        let executable =
            std::fs::metadata(format!("/proc/{tid}/exe")).map_err(|e| self.caller_error(e))?;
        Ok(ImageIdentity {
            tid: tid.as_raw(),
            start_ticks,
            generation: self.liteinst_runtime.lock().unwrap().generation,
            executable_device: executable.dev(),
            executable_inode: executable.ino(),
            at_entry: guest_auxv_entry(tid, libc::AT_ENTRY).ok_or(Errno::EPROTO)?,
            at_phdr: guest_auxv_entry(tid, libc::AT_PHDR).ok_or(Errno::EPROTO)?,
        })
    }

    fn caller_quiescent(&self, task: &Stopped, image: ImageIdentity) -> Result<(), Error> {
        if self.after_loader_identity(task)? != image {
            return Err(self.caller_error("image changed"));
        }
        let runtime = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .ok_or(Errno::EPROTO)?;
        if runtime.root_tid.get() != Some(&task.pid())
            || runtime.multi_task.load(Ordering::SeqCst)
            || !runtime.newborn_tracees.lock().unwrap().is_empty()
            || self.pending_signal.is_some()
        {
            return Err(
                self.caller_error("caller requires the sole recorded task and no pending signal")
            );
        }
        for _ in 0..2 {
            let mut tids = std::fs::read_dir(format!("/proc/{}/task", task.pid()))
                .map_err(|e| self.caller_error(e))?
                .map(|entry| entry.map(|e| e.file_name()))
                .collect::<io::Result<Vec<_>>>()
                .map_err(|e| self.caller_error(e))?;
            tids.sort();
            if tids != [std::ffi::OsString::from(task.pid().as_raw().to_string())] {
                return Err(self.caller_error("task population changed"));
            }
        }
        signal_state(task.pid()).map_err(|e| self.caller_error(e))?;
        Ok(())
    }

    fn caller_trap(&self, task: &Stopped) -> Result<TrapObservation, Error> {
        let regs = task.getregs()?;
        let info = task.getsiginfo()?;
        Ok(TrapObservation {
            identity: self.after_loader_identity(task)?,
            signal: info.si_signo,
            si_code: info.si_code,
            rip: regs.rip,
            rsp: regs.rsp,
            r10: regs.r10,
        })
    }

    async fn caller_wait(&self, running: Running, operation: &str) -> Result<Wait, Error> {
        // The sole wait consumer owns this result and arms the cleanup lease
        // before inspecting, reporting or rejecting it. Never forge Stopped.
        let wait = running.next_state().await?;
        self.arm_liteinst_wait(&wait)?;
        // `Wait`'s derived Debug includes the complete notifier generation and
        // its bounded physical-observer storage. Retaining that implementation
        // graph once per private stop duplicates hundreds of KiB and can hide
        // the later, semantically useful observations behind the diagnostic
        // byte bound. Record every transition identity explicitly instead;
        // the observer remains the lossless physical partition evidence.
        self.caller_observe(operation, after_loader_wait_summary(&wait))?;
        if let Wait::Stopped(task, event) = &wait {
            let regs = task.getregs()?;
            self.caller_observe(
                "private held stop registers",
                format!(
                    "tid={} event={} regs={:?}",
                    task.pid(),
                    after_loader_event_summary(event),
                    register_words(&regs)
                ),
            )?;
            if matches!(event, Event::Signal(_)) {
                let info = task.getsiginfo()?;
                self.caller_observe(
                    "private held stop siginfo",
                    format!(
                        "tid={} signo={} code={} errno={}",
                        task.pid(),
                        info.si_signo,
                        info.si_code,
                        info.si_errno
                    ),
                )?;
            }
        }
        Ok(wait)
    }

    async fn caller_syscall(
        &mut self,
        task: Stopped,
        image: ImageIdentity,
        nr: Sysno,
        args: [u64; 6],
    ) -> Result<(Stopped, u64), Error> {
        self.caller_quiescent(&task, image)?;
        let mut private_stub = [0; 4];
        task.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut private_stub)?;
        if private_stub != [0x0f, 0x05, 0x0f, 0x0b] {
            return Err(self.caller_error("private syscall stub changed before execution"));
        }
        let old = task.getregs()?;
        let mut regs = old;
        regs.rax = nr as u64;
        regs.orig_rax = nr as u64;
        regs.set_args((args[0], args[1], args[2], args[3], args[4], args[5]));
        regs.rip = cp::PRIVATE_PAGE_OFFSET as u64;
        let config = self.after_loader_config().ok_or(Errno::EPROTO)?;
        let effect = self.admit_after_loader_controller_syscall(&task, &config, nr as i64, args)?;
        self.arm_after_loader_syscall_permit(
            &task,
            AfterLoaderSyscallPurpose::PrivateSetup,
            nr as i64,
            args,
            cp::PRIVATE_PAGE_OFFSET as u64,
        )?;
        self.bind_after_loader_syscall_effect(effect)?;
        task.setregs(&regs)?;
        let running = self.step_stopped(task, None)?;
        let wait = self.caller_wait(running, "private syscall stop").await?;
        let task = match wait {
            Wait::Stopped(task, Event::Seccomp) => task,
            other => {
                return Err(self.caller_error(format!(
                    "unexpected private syscall event: {}",
                    after_loader_wait_summary(&other),
                )));
            }
        };
        self.caller_quiescent(&task, image)?;
        let permit = self
            .consume_after_loader_syscall_permit(&task)?
            .ok_or(Errno::EPROTO)?;
        if permit.purpose != AfterLoaderSyscallPurpose::PrivateSetup
            || permit.number != nr as i64
            || permit.args != args
        {
            return Err(self.caller_error("private syscall consumed a different permit"));
        }
        let wait = self
            .caller_wait(
                self.syscall_stopped(task, None)?,
                "private syscall completion",
            )
            .await?;
        let task = match wait {
            Wait::Stopped(task, Event::Syscall) => task,
            other => {
                return Err(self.caller_error(format!(
                    "unexpected private syscall completion: {}",
                    after_loader_wait_summary(&other),
                )));
            }
        };
        self.caller_quiescent(&task, image)?;
        let completed_permit = self
            .complete_after_loader_syscall_inflight(&task)?
            .ok_or(Errno::EPROTO)?;
        if completed_permit != permit {
            return Err(self.caller_error("private syscall completion authority changed"));
        }
        let completed = task.getregs()?;
        if completed.orig_rax as i64 != nr as i64
            || [
                completed.rdi,
                completed.rsi,
                completed.rdx,
                completed.r10,
                completed.r8,
                completed.r9,
            ] != args
            || completed.rip != cp::PRIVATE_PAGE_OFFSET as u64 + 2
        {
            return Err(self.caller_error("private syscall completion identity changed"));
        }
        let value = completed.rax;
        let completion = self.complete_after_loader_private_syscall(
            &task,
            &config,
            &completed_permit,
            value as i64,
        )?;
        self.caller_observe(
            "private syscall result",
            format!("{nr:?} args={args:?} result={value:#x}"),
        )?;
        task.setregs(&old)?;
        if register_words(&task.getregs()?) != register_words(&old) {
            return Err(self.caller_error("private syscall register restoration differs"));
        }
        if !caller_private_syscall_result_is_accepted(completion) {
            return Err(self.caller_error(format!(
                "private syscall {nr:?} failed with {}",
                value as i64
            )));
        }
        Ok((task, value))
    }

    fn caller_write(&self, task: &mut Stopped, address: u64, bytes: &[u8]) -> Result<(), Error> {
        task.write_exact(
            AddrMut::from_raw(address as usize).ok_or(Errno::EFAULT)?,
            bytes,
        )?;
        let mut observed = vec![0; bytes.len()];
        task.read_exact(address as usize, &mut observed)?;
        if observed != bytes {
            return Err(self.caller_error("private write readback differs"));
        }
        Ok(())
    }

    async fn caller_signal_actions(
        &mut self,
        mut task: Stopped,
        image: ImageIdentity,
        data: u64,
        label: &str,
    ) -> Result<(Stopped, Vec<[u8; 32]>, [u8; 8]), Error> {
        // x86-64 kernel sigaction is handler, flags, restorer and 64-bit mask:
        // four contiguous u64 values. Query every kernel signal, including
        // glibc's reserved realtime numbers. No libc wrapper or handler runs.
        let mut actions = Vec::with_capacity(64);
        for signal in 1..=64_u64 {
            let destination = data + 512 + (signal - 1) * 32;
            let (next, _) = self
                .caller_syscall(
                    task,
                    image,
                    Sysno::rt_sigaction,
                    [signal, 0, destination, 8, 0, 0],
                )
                .await?;
            task = next;
            let mut action = [0; 32];
            task.read_exact(destination as usize, &mut action)?;
            actions.push(action);
        }
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::rt_sigprocmask,
                [libc::SIG_SETMASK as u64, 0, data + 352, 8, 0, 0],
            )
            .await?;
        let mut mask = [0; 8];
        task.read_exact((data + 352) as usize, &mut mask)?;
        self.caller_observe(
            label,
            format!("kernel_sigactions={actions:?} kernel_mask={mask:?}"),
        )?;
        Ok((task, actions, mask))
    }

    fn caller_sealed_runtime(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        fd: u64,
    ) -> Result<(), Error> {
        let sealed = &config.sealed_runtime;
        if unsafe { libc::fcntl(sealed.file.as_raw_fd(), libc::F_GET_SEALS) }
            != crate::after_loader::RUNTIME_SEALS
        {
            return Err(self.caller_error("controller runtime seals changed"));
        }
        let path = format!("/proc/{}/fd/{fd}", task.pid());
        let mut file = std::fs::File::open(path).map_err(|e| self.caller_error(e))?;
        let metadata = file.metadata().map_err(|e| self.caller_error(e))?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(crate::after_loader::MAX_RUNTIME_FILE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| self.caller_error(e))?;
        if metadata.dev() != sealed.image.file_identity.device
            || metadata.ino() != sealed.image.file_identity.inode
            || bytes.as_slice() != sealed.image.bytes.as_ref()
            || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) }
                != crate::after_loader::RUNTIME_SEALS
        {
            return Err(self.caller_error("target runtime descriptor differs from sealed source"));
        }
        Ok(())
    }

    fn caller_errno(&self, task: &Stopped, pointer: u64) -> Result<[u8; 4], Error> {
        let range = GuestRange::new(pointer, 4).ok_or(Errno::EFAULT)?;
        if !guest_maps(task.pid()).is_some_and(|maps| {
            maps.iter()
                .any(|map| map.readable && map.writable && !map.shared && map.contains_range(range))
        }) {
            return Err(
                self.caller_error("errno pointer is not private readable/writable target memory")
            );
        }
        let mut bytes = [0; 4];
        task.read_exact(pointer as usize, &mut bytes)?;
        Ok(bytes)
    }

    fn caller_snapshot(
        &self,
        task: &Stopped,
        label: &str,
        stack: u64,
        length: usize,
        random: u64,
    ) -> Result<(), Error> {
        if length > MAX_STACK_SNAPSHOT {
            return Err(self.caller_error("snapshot stack bound"));
        }
        let regs = task.getregs()?;
        let xstate = task.getxstate()?;
        let mut stack_bytes = vec![0; length];
        task.read_exact(stack as usize, &mut stack_bytes)?;
        let mut random_bytes = [0; 16];
        task.read_exact(random as usize, &mut random_bytes)?;
        let mut canary = [0; 8];
        task.read_exact(
            regs.fs_base.checked_add(0x28).ok_or(Errno::EPROTO)? as usize,
            &mut canary,
        )?;
        self.caller_observe(
            &format!("{label} registers and XSTATE"),
            format!("regs={:?} xstate={xstate:?}", register_words(&regs)),
        )?;
        self.caller_observe(
            &format!("{label} original stack"),
            format!("address={stack:#x} bytes={stack_bytes:?}"),
        )?;
        self.caller_observe(
            &format!("{label} random and canary"),
            format!("AT_RANDOM={random_bytes:?} canary={canary:?}"),
        )?;
        self.caller_observe(
            &format!("{label} signals and descriptors"),
            format!(
                "signals={:?} descriptors={:?}",
                signal_state(task.pid()).map_err(|e| self.caller_error(e))?,
                descriptor_state(task.pid()).map_err(|e| self.caller_error(e))?
            ),
        )?;
        let maps = bounded_proc(format!("/proc/{}/maps", task.pid()), 2 * 1024 * 1024)
            .map_err(|e| self.caller_error(e))?;
        self.caller_observe(
            &format!("{label} maps"),
            String::from_utf8(maps).map_err(|e| self.caller_error(e))?,
        )
    }

    fn caller_private_syscall(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        let r = task.getregs()?;
        // Authenticate the syscall instruction and its bound loader/runtime
        // image before the separate function-specific admission proves the
        // exact arguments and owned descriptor, mapping, futex or break state.
        if !private_syscall_allowed(r.orig_rax as i64) {
            return Err(self.caller_error(format!("unexpected private syscall {}", r.orig_rax)));
        }
        let ip = r.rip.checked_sub(2).ok_or(Errno::EPROTO)?;
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let map = maps
            .iter()
            .find(|m| {
                m.readable
                    && m.executable
                    && !m.writable
                    && !m.shared
                    && m.contains(ip)
                    && m.contains(r.rip - 1)
            })
            .ok_or(Errno::EPROTO)?;
        let images = std::iter::once(&config.provider)
            .chain(std::iter::once(&config.sealed_runtime.image))
            .chain(config.dependencies.iter());
        let state = self.after_loader_private_state()?;
        let expected = images
            .into_iter()
            .find(|image| {
                state
                    .image_mapping_identity(image)
                    .is_some_and(|identity| mapping_identity_matches(identity, map))
            })
            .ok_or_else(|| self.caller_error("private syscall came from unbound or guest code"))?;
        let offset = map
            .offset
            .checked_add(ip - map.start)
            .ok_or(Errno::EPROTO)? as usize;
        let wanted = expected
            .bytes
            .get(offset..offset + 2)
            .ok_or(Errno::EPROTO)?;
        let mut actual = [0; 2];
        task.read_exact(ip as usize, &mut actual)?;
        if actual != [0x0f, 0x05] || actual != wanted {
            return Err(self.caller_error("private syscall bytes differ from bound image"));
        }
        self.caller_observe(
            "private loader syscall",
            format!(
                "ip={ip:#x} nr={} image={}",
                r.orig_rax,
                expected.path.display()
            ),
        )
    }

    fn begin_after_loader_private_call(
        &mut self,
        task: &Stopped,
        image: ImageIdentity,
        calls: &Calls,
        code: &CallCode,
        arguments: [u64; 2],
        function: CallerFunction,
    ) -> Result<(), Error> {
        if self.liteinst_after_loader_private_state.is_none()
            || self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
            || self.liteinst_after_loader_private_call.is_some()
        {
            return Err(self.caller_error("private call authority was not empty at call origin"));
        }
        let origin_status = task
            .physical_status_id()
            .ok_or_else(|| self.caller_error("private call origin has no physical status"))?;
        if self.after_loader_identity(task)? != image {
            return Err(self.caller_error("private call image changed at call origin"));
        }
        let expected_phase = match function {
            CallerFunction::ErrnoLocation => {
                matches!(
                    calls.phase(),
                    CallsPhase::EntryHeld | CallsPhase::InitializerReturned
                )
            }
            CallerFunction::Dlopen => calls.phase() == CallsPhase::Dlopen,
            CallerFunction::Initializer => calls.phase() == CallsPhase::Initializing,
        };
        let regs = task.getregs()?;
        let mut code_bytes = [0; 30];
        task.read_exact(code.entry as usize, &mut code_bytes)?;
        if !expected_phase
            || code_bytes != code.bytes
            || regs.rip != code.entry
            || regs.rsp != code.call_stack_top
            || regs.rdi != arguments[0]
            || regs.rsi != arguments[1]
            || regs.orig_rax != u64::MAX
        {
            return Err(self.caller_error("private call origin differs from the bound call"));
        }
        self.liteinst_after_loader_private_call = Some(AfterLoaderPrivateCall {
            image,
            function,
            origin_status,
            entry: code.entry,
            return_rip: code.return_rip,
            call_stack_top: code.call_stack_top,
            code_bytes,
            arguments,
            calls_phase: calls.phase(),
        });
        Ok(())
    }

    fn validate_after_loader_private_call(
        &self,
        task: &Stopped,
        image: ImageIdentity,
        calls: &Calls,
        code: &CallCode,
        function: CallerFunction,
    ) -> Result<AfterLoaderPrivateCall, Error> {
        let active = self
            .liteinst_after_loader_private_call
            .as_ref()
            .ok_or_else(|| self.caller_error("private call has no origin authority"))?
            .clone();
        let phase_matches = match active.function {
            CallerFunction::ErrnoLocation | CallerFunction::Dlopen => {
                calls.phase() == active.calls_phase
            }
            CallerFunction::Initializer => {
                active.calls_phase == CallsPhase::Initializing
                    && matches!(
                        calls.phase(),
                        CallsPhase::Initializing
                            | CallsPhase::BeginObserved
                            | CallsPhase::ReadyObserved
                    )
            }
        };
        let mut code_bytes = [0; 30];
        task.read_exact(active.entry as usize, &mut code_bytes)?;
        if active.image != image
            || self.after_loader_identity(task)? != image
            || active.function != function
            || active.entry != code.entry
            || active.return_rip != code.return_rip
            || active.call_stack_top != code.call_stack_top
            || active.code_bytes != code.bytes
            || code_bytes != active.code_bytes
            || !phase_matches
        {
            return Err(self.caller_error("active private call authority changed"));
        }
        Ok(active)
    }

    fn arm_after_loader_private_call_syscall(
        &mut self,
        task: &Stopped,
        image: ImageIdentity,
        calls: &Calls,
        code: &CallCode,
        function: CallerFunction,
    ) -> Result<AfterLoaderSyscallPermit, Error> {
        if self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
        {
            return Err(self.caller_error("private syscall authority was not empty at admission"));
        }
        let active = self.validate_after_loader_private_call(task, image, calls, code, function)?;
        let admission_status = task
            .physical_status_id()
            .ok_or_else(|| self.caller_error("private syscall admission has no physical status"))?;
        if admission_status == active.origin_status {
            return Err(
                self.caller_error("private syscall admission reused its call-origin status")
            );
        }
        let regs = task.getregs()?;
        let instruction_pointer = regs.rip.checked_sub(2).ok_or(Errno::EPROTO)?;
        let mut instruction = [0; 4];
        task.read_exact(instruction_pointer as usize, &mut instruction[..2])?;
        if instruction[..2] != [0x0f, 0x05] {
            return Err(self.caller_error("private syscall instruction changed at admission"));
        }
        let args = [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9];
        Ok(AfterLoaderSyscallPermit {
            image: Some(image),
            tid: task.pid(),
            generation: image.generation,
            origin_status: Some(active.origin_status),
            admission_status: Some(admission_status),
            purpose: AfterLoaderSyscallPurpose::PrivateSetup,
            number: regs.orig_rax as i64,
            args,
            instruction_pointer,
            resume_pointer: regs.rip,
            instruction,
            instruction_length: 2,
            output_spans: syscall_output_spans(regs.orig_rax as i64, args)?,
            effect: AfterLoaderSyscallEffect::None,
        })
    }

    fn finish_after_loader_private_call(
        &mut self,
        task: &Stopped,
        image: ImageIdentity,
        calls: &Calls,
        code: &CallCode,
        function: CallerFunction,
        trap: &TrapObservation,
    ) -> Result<(), Error> {
        if self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
        {
            return Err(self.caller_error("private syscall authority survived to call return"));
        }
        let active = self.validate_after_loader_private_call(task, image, calls, code, function)?;
        let return_status = task
            .physical_status_id()
            .ok_or_else(|| self.caller_error("private call return has no physical status"))?;
        if return_status == active.origin_status {
            return Err(self.caller_error("private call return reused its origin status"));
        }
        code.authenticate_return(trap, &active.code_bytes)
            .map_err(|error| self.caller_error(format!("{error:?}")))?;
        let cleared = self
            .liteinst_after_loader_private_call
            .take()
            .ok_or(Errno::EPROTO)?;
        if cleared != active {
            return Err(self.caller_error("private call authority changed while clearing return"));
        }
        Ok(())
    }

    async fn caller_function(
        &mut self,
        task: Stopped,
        image: ImageIdentity,
        config: &LiteinstAfterLoaderConfig,
        calls: &mut Calls,
        code: &CallCode,
        arguments: [u64; 2],
        function: CallerFunction,
    ) -> Result<(Stopped, u64), Error> {
        // Error terminality is part of the target allocator's safety contract:
        // every Err consumes the sole stopped capability. The outer SIGTRAP
        // path must tear down the trace session and must never recreate or
        // resume this task, because a controller refusal can bypass target-side
        // RAII that clears its process-global preparation/install flags.
        self.caller_quiescent(&task, image)?;
        let mut regs = task.getregs()?;
        regs.rip = code.entry;
        regs.rsp = code.call_stack_top;
        regs.rdi = arguments[0];
        regs.rsi = arguments[1];
        regs.rax = 0;
        regs.orig_rax = u64::MAX;
        regs.eflags = liteinst_helper_entry_rflags(regs.eflags);
        task.setregs(&regs)?;
        self.begin_after_loader_private_call(&task, image, calls, code, arguments, function)?;
        let mut wait = self
            .caller_wait(self.resume_stopped(task, None)?, "private function stop")
            .await?;
        loop {
            match wait {
                Wait::Stopped(stopped, Event::Seccomp) => {
                    if function == CallerFunction::ErrnoLocation {
                        return Err(self.caller_error("errno accessor attempted a syscall"));
                    }
                    self.caller_quiescent(&stopped, image)?;
                    let mut permit = self.arm_after_loader_private_call_syscall(
                        &stopped, image, calls, code, function,
                    )?;
                    self.caller_private_syscall(&stopped, config)?;
                    let effect = self
                        .admit_after_loader_function_syscall(&stopped, config, function, &permit)?;
                    permit.effect = effect;
                    self.liteinst_after_loader_syscall_permit = Some(permit.clone());
                    let consumed = self
                        .consume_after_loader_syscall_permit(&stopped)?
                        .ok_or(Errno::EPROTO)?;
                    if consumed != permit {
                        return Err(self.caller_error("private function syscall permit changed"));
                    }
                    let exit = self
                        .caller_wait(
                            self.syscall_stopped(stopped, None)?,
                            "private loader syscall exit",
                        )
                        .await?;
                    let stopped = match exit {
                        Wait::Stopped(stopped, Event::Syscall) => stopped,
                        other => {
                            return Err(self.caller_error(format!(
                                "unexpected syscall completion: {}",
                                after_loader_wait_summary(&other),
                            )));
                        }
                    };
                    self.caller_quiescent(&stopped, image)?;
                    let completed_permit = self
                        .complete_after_loader_syscall_inflight(&stopped)?
                        .ok_or(Errno::EPROTO)?;
                    if completed_permit != permit {
                        return Err(self.caller_error(
                            "private function syscall completion authority changed",
                        ));
                    }
                    let completed = stopped.getregs()?;
                    if completed.orig_rax as i64 != permit.number
                        || [
                            completed.rdi,
                            completed.rsi,
                            completed.rdx,
                            completed.r10,
                            completed.r8,
                            completed.r9,
                        ] != permit.args
                        || completed.rip != permit.resume_pointer
                    {
                        return Err(self
                            .caller_error("private function syscall completion identity changed"));
                    }
                    let completion = self.complete_after_loader_private_syscall(
                        &stopped,
                        config,
                        &completed_permit,
                        completed.rax as i64,
                    )?;
                    if !matches!(completion, AfterLoaderSyscallCompletion::KernelResult(_)) {
                        return Err(self.caller_error(
                            "private function accepted a controller-only syscall result",
                        ));
                    }
                    self.caller_observe(
                        "private loader syscall result",
                        format!(
                            "nr={} args={:?} result={:#x} outputs={:?}",
                            permit.number, permit.args, completed.rax, permit.output_spans,
                        ),
                    )?;
                    wait = self
                        .caller_wait(self.resume_stopped(stopped, None)?, "private function stop")
                        .await?;
                }
                Wait::Stopped(stopped, Event::Signal(Signal::SIGTRAP)) => {
                    self.caller_quiescent(&stopped, image)?;
                    let trap = self.caller_trap(&stopped)?;
                    let regs = stopped.getregs()?;
                    let mut bytes = [0; 30];
                    stopped.read_exact(code.entry as usize, &mut bytes)?;
                    if trap.rip == code.return_rip {
                        self.finish_after_loader_private_call(
                            &stopped, image, calls, code, function, &trap,
                        )?;
                        match function {
                            CallerFunction::Initializer => {
                                calls.initializer_returned(code, &trap, &bytes, regs.rax as u32)
                            }
                            CallerFunction::Dlopen => {
                                calls.dlopen_returned(code, &trap, &bytes, regs.rax)
                            }
                            CallerFunction::ErrnoLocation => {
                                calls.errno_returned(code, &trap, &bytes)
                            }
                        }
                        .map_err(|e| self.caller_error(format!("{e:?}")))?;
                        return Ok((stopped, regs.rax));
                    }
                    if function != CallerFunction::Initializer || !matches!(trap.si_code, 1 | 128) {
                        return Err(self.caller_error("unexpected private trap before initializer"));
                    }
                    self.validate_after_loader_private_call(
                        &stopped, image, calls, code, function,
                    )?;
                    match self.classify_liteinst_trap(&stopped, &regs) {
                        Some(LiteinstTrap::HandshakeBegin) => calls.begin(),
                        Some(LiteinstTrap::HandshakeReady) => calls.ready(),
                        _ => {
                            return Err(self.caller_error(
                                "private trap is not the exact Begin/Ready handshake",
                            ));
                        }
                    }
                    .map_err(|e| self.caller_error(format!("{e:?}")))?;
                    self.caller_observe("initializer handshake", format!("{:?}", calls.phase()))?;
                    wait = self
                        .caller_wait(self.resume_stopped(stopped, None)?, "private function stop")
                        .await?;
                }
                other => {
                    return Err(self.caller_error(format!(
                        "unexpected signal, timer, callback, clone or exec: {}",
                        after_loader_wait_summary(&other),
                    )));
                }
            }
        }
    }

    fn plan_after_loader_vdso(
        &self,
        task: &Stopped,
    ) -> Result<Option<crate::vdso::StoppedVdsoPlan>, Error> {
        if !crate::vdso::stopped_patch_required(&self.global_state.subscriptions) {
            return Ok(None);
        }
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let mut vdso = maps
            .iter()
            .filter(|mapping| mapping.path.as_deref() == Some(std::path::Path::new("[vdso]")));
        let mapping = vdso
            .next()
            .ok_or_else(|| self.caller_error("target has no special vDSO mapping"))?;
        if vdso.next().is_some()
            || !mapping.readable
            || mapping.writable
            || !mapping.executable
            || mapping.shared
            || mapping.offset != 0
            || mapping.inode != 0
            || mapping.start & (PAGE - 1) != 0
        {
            return Err(self.caller_error("target vDSO mapping identity or protection differs"));
        }
        let len = usize::try_from(
            mapping
                .end
                .checked_sub(mapping.start)
                .ok_or(Errno::EOVERFLOW)?,
        )
        .map_err(|_| Errno::EOVERFLOW)?;
        if len == 0 || len > 64 * 1024 {
            return Err(self.caller_error("target vDSO mapping length is outside bounds"));
        }
        let mut image = vec![0; len];
        task.read_exact(mapping.start as usize, &mut image)?;
        let plan = crate::vdso::plan_stopped_vdso(
            &image,
            mapping.start,
            &self.global_state.subscriptions,
        )?;
        if plan.mapping_start() != mapping.start || plan.mapping_len() != len as u64 {
            return Err(self.caller_error("stopped vDSO plan changed its source mapping"));
        }
        Ok(Some(plan))
    }

    fn restore_after_loader_vdso_words(
        &self,
        task: &mut Stopped,
        plan: &crate::vdso::StoppedVdsoPlan,
        words: &[crate::vdso::StoppedVdsoWordPatch],
        attempted: usize,
    ) -> Result<(), Error> {
        for word in words[..attempted].iter().rev() {
            let read_address = Addr::from_raw(word.address as usize).ok_or(Errno::EFAULT)?;
            let write_address = AddrMut::from_raw(word.address as usize).ok_or(Errno::EFAULT)?;
            let observed: u64 = task.read_value(read_address)?;
            if observed == word.replacement {
                task.write_value(write_address, &word.expected)?;
            } else if observed != word.expected {
                return Err(self.caller_error(format!(
                    "stopped vDSO rollback found an unknown word at {:#x}",
                    word.address
                )));
            }
            let restored: u64 = task.read_value(read_address)?;
            if restored != word.expected {
                return Err(self.caller_error(format!(
                    "stopped vDSO rollback readback differs at {:#x}",
                    word.address
                )));
            }
        }
        let mut restored = vec![0; plan.expected_image().len()];
        task.read_exact(plan.mapping_start() as usize, &mut restored)?;
        if restored != plan.expected_image() {
            return Err(self.caller_error(
                "stopped vDSO rollback did not restore the complete original image",
            ));
        }
        Ok(())
    }

    fn fail_after_loader_vdso_publication(
        &self,
        task: &mut Stopped,
        plan: &crate::vdso::StoppedVdsoPlan,
        words: &[crate::vdso::StoppedVdsoWordPatch],
        attempted: usize,
        original: Error,
    ) -> Error {
        match self.restore_after_loader_vdso_words(task, plan, words, attempted) {
            Ok(()) => original,
            Err(rollback) => self.caller_error(format!(
                "stopped vDSO publication failed: {original}; inverse restoration failed: {rollback}"
            )),
        }
    }

    fn install_after_loader_vdso(
        &self,
        mut task: Stopped,
        plan: &crate::vdso::StoppedVdsoPlan,
    ) -> Result<Stopped, Error> {
        let words = plan.publication_words()?;
        let expected_image = plan.expected_published_image()?;
        let mut attempted = 0;
        for (index, word) in words.iter().enumerate() {
            let read_address = Addr::from_raw(word.address as usize).ok_or(Errno::EFAULT)?;
            let write_address = AddrMut::from_raw(word.address as usize).ok_or(Errno::EFAULT)?;
            let observed: u64 = match task.read_value(read_address) {
                Ok(observed) => observed,
                Err(error) => {
                    let original = self.caller_error(format!(
                        "stopped vDSO word read failed at {:#x}: {error}",
                        word.address
                    ));
                    return Err(self.fail_after_loader_vdso_publication(
                        &mut task, plan, &words, attempted, original,
                    ));
                }
            };
            if observed != word.expected {
                let original = self.caller_error(format!(
                    "stopped vDSO word changed before publication at {:#x}",
                    word.address
                ));
                return Err(self.fail_after_loader_vdso_publication(
                    &mut task, plan, &words, attempted, original,
                ));
            }
            attempted = index + 1;
            if let Err(error) = task.write_value(write_address, &word.replacement) {
                let original = self.caller_error(format!(
                    "stopped vDSO word write failed at {:#x}: {error}",
                    word.address
                ));
                return Err(self.fail_after_loader_vdso_publication(
                    &mut task, plan, &words, attempted, original,
                ));
            }
            let published: u64 = match task.read_value(read_address) {
                Ok(published) => published,
                Err(error) => {
                    let original = self.caller_error(format!(
                        "stopped vDSO word readback failed at {:#x}: {error}",
                        word.address
                    ));
                    return Err(self.fail_after_loader_vdso_publication(
                        &mut task, plan, &words, attempted, original,
                    ));
                }
            };
            if published != word.replacement {
                let original = self.caller_error(format!(
                    "stopped vDSO word readback differs at {:#x}",
                    word.address
                ));
                return Err(self.fail_after_loader_vdso_publication(
                    &mut task, plan, &words, attempted, original,
                ));
            }
        }

        let mut published_image = vec![0; expected_image.len()];
        if let Err(error) = task.read_exact(plan.mapping_start() as usize, &mut published_image) {
            let original = self.caller_error(format!(
                "stopped vDSO complete published image read failed: {error}"
            ));
            return Err(self
                .fail_after_loader_vdso_publication(&mut task, plan, &words, attempted, original));
        }
        if published_image.as_slice() != expected_image.as_ref() {
            let original =
                self.caller_error("stopped vDSO complete published image readback differs");
            return Err(self
                .fail_after_loader_vdso_publication(&mut task, plan, &words, attempted, original));
        }
        if let Err(original) = self.caller_observe(
            "stopped vDSO publication verified",
            format!(
                "mapping={:#x}+{:#x} aligned_words={} legacy_symbols={} getrandom={}",
                plan.mapping_start(),
                plan.mapping_len(),
                words.len(),
                plan.legacy_patch_count(),
                plan.has_getrandom(),
            ),
        ) {
            return Err(self
                .fail_after_loader_vdso_publication(&mut task, plan, &words, attempted, original));
        }
        Ok(task)
    }

    pub(super) async fn run_after_loader(&mut self, mut task: Stopped) -> Result<Stopped, Error> {
        let config = self.after_loader_config().ok_or(Errno::EPROTO)?;
        let image = self.after_loader_identity(&task)?;
        self.caller_quiescent(&task, image)?;
        if image.generation != 1 {
            return Err(self.caller_error("only the first image is supported"));
        }
        let executable = bounded_proc(
            format!("/proc/{}/exe", task.pid()),
            crate::after_loader::MAX_CALLER_FILE,
        )
        .map_err(|e| self.caller_error(e))?;
        if executable.as_slice() != config.executable.bytes.as_ref()
            || image.executable_device != config.executable.file_identity.device
            || image.executable_inode != config.executable.file_identity.inode
        {
            return Err(self.caller_error("executable differs from fixed fixture"));
        }
        let environment = bounded_proc(format!("/proc/{}/environ", task.pid()), 1024 * 1024)
            .map_err(|e| self.caller_error(e))?;
        let actual_environment = environment_map(&environment).map_err(|e| self.caller_error(e))?;
        config
            .validate_environment(&actual_environment)
            .map_err(|e| self.caller_error(e))?;
        for item in environment.split(|b| *b == 0) {
            if item.starts_with(b"LD_PRELOAD=") || item.starts_with(b"LD_AUDIT=") {
                return Err(self.caller_error("first fixture does not support preload/audit callbacks; environment left unchanged"));
            }
        }
        // Retain the complete untouched image before the first private resume.
        // The ordinary tracee-preinit vDSO path is deliberately deferred for
        // after-loader activation, so every later word expectation is rooted in
        // bytes observed at this exact AT_ENTRY stop.
        let vdso_plan = self.plan_after_loader_vdso(&task)?;
        let guard = self
            .liteinst_after_loader_guard
            .take()
            .ok_or(Errno::EPROTO)?;
        let mut word = [0; 8];
        task.read_exact(image.at_entry as usize, &mut word)?;
        let trap = self.caller_trap(&task)?;
        let mut calls =
            Calls::at_entry(guard, &trap, word).map_err(|e| self.caller_error(format!("{e:?}")))?;
        let authenticated_entry = calls
            .take_authenticated_entry()
            .map_err(|error| self.caller_error(format!("{error:?}")))?;
        if authenticated_entry.identity() != image {
            return Err(self.caller_error("authenticated entry identity changed"));
        }
        let mut saved_regs = task.getregs()?;
        saved_regs.rip = image.at_entry;
        let saved_xstate = task.getxstate()?;
        let saved_signals = signal_state(task.pid()).map_err(|e| self.caller_error(e))?;
        let saved_descriptors = descriptor_state(task.pid()).map_err(|e| self.caller_error(e))?;
        if self.liteinst_after_loader_private_state.is_some()
            || self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
            || self.liteinst_after_loader_forward_inflight.is_some()
            || self.liteinst_after_loader_private_call.is_some()
        {
            return Err(self.caller_error("private operation state was already present"));
        }
        self.liteinst_after_loader_private_state =
            Some(AfterLoaderPrivateState::new(&task, image)?);
        self.with_restored_liteinst_entry_guard(
            &mut task,
            authenticated_entry,
            |this, stopped| this.bind_after_loader_initial_image_geometries(stopped, &config),
        )?;
        let timer_suspension = match self.timer.begin_suspend_for_private_execution() {
            Ok(suspension) => suspension,
            Err(error) => {
                return Err(self.caller_error(format!(
                    "suspend deterministic timer for private after-loader execution: {error}"
                )));
            }
        };
        let frozen_timer_clock = timer_suspension.frozen_clock();
        self.liteinst_after_loader_private_state
            .as_mut()
            .ok_or(Errno::EPROTO)?
            .timer_suspension = Some(timer_suspension);
        let complete_timer_suspension = {
            let timer_suspension = self
                .liteinst_after_loader_private_state
                .as_ref()
                .and_then(|state| state.timer_suspension.as_ref())
                .ok_or(Errno::EPROTO)?;
            self.timer
                .complete_suspend_for_private_execution(timer_suspension)
        };
        if let Err(error) = complete_timer_suspension {
            return Err(self.caller_error(format!(
                "complete deterministic timer suspension for private after-loader execution: {error}"
            )));
        }
        let (next, current_break) = self.caller_syscall(task, image, Sysno::brk, [0; 6]).await?;
        task = next;
        self.caller_observe(
            "initial target break",
            format!("address={current_break:#x}"),
        )?;
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let stack = maps
            .iter()
            .find(|m| m.readable && m.writable && m.contains(saved_regs.rsp))
            .ok_or(Errno::EPROTO)?;
        let stack_len = usize::try_from(stack.end - stack.start).map_err(|_| Errno::EPROTO)?;
        if stack_len > MAX_STACK_SNAPSHOT {
            return Err(self.caller_error("initial stack exceeds fixture bound"));
        }
        let stack_address = stack.start;
        let mut saved_stack = vec![0; stack_len];
        task.read_exact(stack_address as usize, &mut saved_stack)?;
        let random_address = guest_auxv_entry(task.pid(), libc::AT_RANDOM).ok_or(Errno::EPROTO)?;
        let mut saved_random = [0; 16];
        task.read_exact(random_address as usize, &mut saved_random)?;
        let mut saved_canary = [0; 8];
        task.read_exact(
            saved_regs.fs_base.checked_add(0x28).ok_or(Errno::EPROTO)? as usize,
            &mut saved_canary,
        )?;
        self.caller_observe(
            "entry held",
            format!("image={image:?} regs={:?}", register_words(&saved_regs)),
        )?;
        self.caller_snapshot(&task, "entry", stack_address, stack_len, random_address)?;

        let flags = (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64;
        let (task, stack_base) = self
            .caller_syscall(
                task,
                image,
                Sysno::mmap,
                [
                    0,
                    STACK_SIZE + 2 * PAGE,
                    libc::PROT_NONE as u64,
                    flags,
                    u64::MAX,
                    0,
                ],
            )
            .await?;
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    stack_base + PAGE,
                    STACK_SIZE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        let (task, data) = self
            .caller_syscall(
                task,
                image,
                Sysno::mmap,
                [
                    0,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    flags,
                    u64::MAX,
                    0,
                ],
            )
            .await?;
        let (mut task, code_base) = self
            .caller_syscall(
                task,
                image,
                Sysno::mmap,
                [
                    0,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    flags,
                    u64::MAX,
                    0,
                ],
            )
            .await?;
        let policy_scratch_address = data
            .checked_add(PRIVATE_POLICY_SCRATCH_OFFSET)
            .ok_or(Errno::EOVERFLOW)?;
        let mut policy_scratch = [0_u8; PRIVATE_POLICY_SCRATCH_BYTES];
        task.read_exact(policy_scratch_address as usize, &mut policy_scratch)?;
        let mut helper_saved_state = LiteinstHelperSavedState {
            cpuid_policy: LiteinstCpuidPolicy::Unsupported,
            tsc_policy: LiteinstTscPolicy::Unsupported,
            regs: task.getregs()?,
            xstate: task.getxstate()?,
            stack_address: policy_scratch_address as usize,
            stack_value: u64::from_ne_bytes(policy_scratch),
        };
        let (next, cpuid_policy) = self
            .prepare_liteinst_helper_cpuid(task)
            .await
            .map_err(Error::Internal)?;
        task = next;
        helper_saved_state.cpuid_policy = match cpuid_policy {
            Ok(policy) => policy,
            Err(message) => {
                let original = self.caller_error(format!(
                    "enable native CPUID for private runtime calls: {message}"
                ));
                return Err(self
                    .rollback_liteinst_helper_error(task, &helper_saved_state, original)
                    .await);
            }
        };
        let (next, tsc_policy) = self
            .prepare_liteinst_helper_tsc(task, policy_scratch_address as usize)
            .await
            .map_err(Error::Internal)?;
        task = next;
        helper_saved_state.tsc_policy = match tsc_policy {
            Ok(policy) => policy,
            Err(message) => {
                let original = self.caller_error(format!(
                    "enable native TSC for private runtime calls: {message}"
                ));
                return Err(self
                    .rollback_liteinst_helper_error(task, &helper_saved_state, original)
                    .await);
            }
        };
        // The query itself is an owned, counted private syscall. Never infer
        // active CET state from an ELF GNU property note.
        let (next, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::arch_prctl,
                [ARCH_SHSTK_STATUS, data + 128, 0, 0, 0, 0],
            )
            .await?;
        task = next;
        let mut cet = [0; 8];
        task.read_exact((data + 128) as usize, &mut cet)?;
        if u64::from_ne_bytes(cet) != 0 {
            return Err(self.caller_error("CET active in fixed caller fixture"));
        }
        let (next, _) = self
            .caller_syscall(task, image, Sysno::sigaltstack, [0, data + 160, 0, 0, 0, 0])
            .await?;
        task = next;
        let mut altstack = [0; 24];
        task.read_exact((data + 160) as usize, &mut altstack)?;
        let (next, saved_actions, saved_mask) = self
            .caller_signal_actions(task, image, data, "initial target signal actions")
            .await?;
        task = next;
        let mut path = config
            .sealed_runtime
            .image
            .path
            .as_os_str()
            .as_encoded_bytes()
            .to_vec();
        if path.contains(&0) || path.len() > 2048 {
            return Err(self.caller_error("runtime path outside data bound"));
        }
        path.push(0);
        self.caller_write(&mut task, data, &path)?;
        let (next, runtime_fd) = self
            .caller_syscall(
                task,
                image,
                Sysno::openat,
                [
                    libc::AT_FDCWD as i64 as u64,
                    data,
                    (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_LARGEFILE) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        let (next, seals) = self
            .caller_syscall(
                next,
                image,
                Sysno::fcntl,
                [runtime_fd, libc::F_GET_SEALS as u64, 0, 0, 0, 0],
            )
            .await?;
        task = next;
        if seals != crate::after_loader::RUNTIME_SEALS as u64 {
            return Err(self.caller_error("target runtime seals differ; staging unavailable"));
        }
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        self.caller_observe(
            "target runtime descriptor opened",
            format!(
                "fd={runtime_fd} device={} inode={} seals={seals:#x}",
                config.sealed_runtime.image.file_identity.device,
                config.sealed_runtime.image.file_identity.inode,
            ),
        )?;
        let path = format!("/proc/self/fd/{runtime_fd}\0");
        self.caller_write(&mut task, data, path.as_bytes())?;
        self.caller_write(&mut task, data + 3072, &crate::entry_call::host_config())?;

        let errno_resolver =
            crate::target_loader::resolve_errno_location(&task, &config.provider.bytes)
                .map_err(|e| self.caller_error(e))?;
        let provider_geometry = self
            .after_loader_private_state()?
            .image_geometry(&config.provider)
            .ok_or_else(|| self.caller_error("provider geometry was not bound at entry"))?;
        if errno_resolver.tid != image.tid
            || errno_resolver.start_ticks != image.start_ticks
            || errno_resolver.executable_phdr != image.at_phdr
            || !loader_resolution_matches_geometry(
                errno_resolver.mapping_identity,
                errno_resolver.load_bias,
                provider_geometry,
            )
        {
            return Err(self.caller_error("errno provider identity differs"));
        }
        let errno_code = CallCode::prepare(
            calls.guard(),
            code_base,
            stack_base + PAGE + STACK_SIZE,
            errno_resolver.address,
            RETURN_MARKER + 2,
        )
        .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_write(&mut task, code_base, &errno_code.bytes)?;
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_EXEC) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        if crate::target_loader::resolve_errno_location(&task, &config.provider.bytes)
            .map_err(|e| self.caller_error(e))?
            != errno_resolver
        {
            return Err(self.caller_error("errno provider changed during preparation"));
        }
        let active_environment = crate::target_loader::observe_environment_before(
            &task,
            &config.provider.bytes,
            &config.environment,
        )
        .map_err(|error| self.caller_error(error))?;
        if active_environment.0.bookkeeping.is_none() {
            return Err(self.caller_error("fixed libc environment bookkeeping is unavailable"));
        }
        self.caller_observe(
            "before errno accessor",
            format!("address={:#x}", errno_resolver.address),
        )?;
        let (task, errno_pointer) = self
            .caller_function(
                task,
                image,
                &config,
                &mut calls,
                &errno_code,
                [0, 0],
                CallerFunction::ErrnoLocation,
            )
            .await?;
        active_environment
            .compare_exact(
                &crate::target_loader::observe_environment_after(
                    &task,
                    &config.provider.bytes,
                    &config.environment,
                )
                .map_err(|error| self.caller_error(error))?,
            )
            .map_err(|error| self.caller_error(error))?;
        let saved_errno = self.caller_errno(&task, errno_pointer)?;
        self.caller_observe(
            "after errno accessor",
            format!("pointer={errno_pointer:#x} bytes={saved_errno:?}"),
        )?;
        let (mut task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;

        // Final address resolution must follow every preparatory target resume.
        let resolver =
            crate::target_loader::resolve_dlopen(&task, &config.provider.bytes, "GLIBC_2.34")
                .map_err(|e| self.caller_error(e))?;
        if resolver.tid != image.tid
            || resolver.start_ticks != image.start_ticks
            || resolver.executable_phdr != image.at_phdr
            || resolver.mapping_identity != errno_resolver.mapping_identity
            || resolver.link_map != errno_resolver.link_map
            || resolver.load_bias != errno_resolver.load_bias
        {
            return Err(self.caller_error("dlopen identity differs"));
        }
        let code = CallCode::prepare(
            calls.guard(),
            code_base,
            stack_base + PAGE + STACK_SIZE,
            resolver.address,
            RETURN_MARKER,
        )
        .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_write(&mut task, code_base, &code.bytes)?;
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_EXEC) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        // mprotect resumed the task; revalidate the target provider coordinates.
        let renewed =
            crate::target_loader::resolve_dlopen(&task, &config.provider.bytes, "GLIBC_2.34")
                .map_err(|e| self.caller_error(e))?;
        if renewed != resolver {
            return Err(self.caller_error("dlopen coordinates changed during preparation"));
        }
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        calls
            .start_dlopen()
            .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_observe("before dlopen", format!("address={:#x}", resolver.address))?;
        let (task, handle) = self
            .caller_function(
                task,
                image,
                &config,
                &mut calls,
                &code,
                [data, 2],
                CallerFunction::Dlopen,
            )
            .await?;
        self.bind_after_loader_deferred_image_geometries(&task, &config)?;
        let runtime_id = self
            .after_loader_private_state()?
            .image_id(&config.sealed_runtime.image);
        let runtime_geometry = self.resolve_after_loader_image_geometry(
            &task,
            &config.sealed_runtime.image,
            runtime_id,
            "post-dlopen-runtime",
        )?;
        if !self
            .after_loader_private_state()?
            .has_causal_image_mapping(runtime_geometry)
            || !self
                .after_loader_private_state_mut()?
                .bind_image_geometry(runtime_geometry)
        {
            return Err(self.caller_error(
                "runtime mapping identity or exact geometry differs from its owned mmap",
            ));
        }
        active_environment
            .compare_exact(
                &crate::target_loader::observe_environment_after(
                    &task,
                    &config.provider.bytes,
                    &config.environment,
                )
                .map_err(|error| self.caller_error(error))?,
            )
            .map_err(|error| self.caller_error(error))?;
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        if self.caller_errno(&task, errno_pointer)? != saved_errno {
            return Err(self.caller_error("dlopen changed guest errno"));
        }
        self.caller_observe("after dlopen", format!("handle={handle:#x}"))?;

        let (mut task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        let initializer =
            crate::target_loader::resolve_host_initializer(&task, &config.runtime.bytes)
                .map_err(|e| self.caller_error(e))?;
        if initializer.tid != image.tid
            || initializer.start_ticks != image.start_ticks
            || initializer.executable_phdr != image.at_phdr
            || !loader_resolution_matches_geometry(
                initializer.mapping_identity,
                initializer.load_bias,
                runtime_geometry,
            )
        {
            return Err(self.caller_error("initializer identity differs"));
        }
        self.caller_observe(
            "bound runtime image geometry",
            format!(
                "file_device={} file_inode={} mapping_device={:x}:{:x} mapping_inode={} load_bias={:#x} span={:#x}-{:#x}",
                config.sealed_runtime.image.file_identity.device,
                config.sealed_runtime.image.file_identity.inode,
                runtime_geometry.mapping.device_major,
                runtime_geometry.mapping.device_minor,
                runtime_geometry.mapping.inode,
                runtime_geometry.load_bias,
                runtime_geometry.span.0,
                runtime_geometry.span.1,
            ),
        )?;
        let code = CallCode::prepare(
            calls.guard(),
            code_base,
            stack_base + PAGE + STACK_SIZE,
            initializer.address,
            RETURN_MARKER + 1,
        )
        .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_write(&mut task, code_base, &code.bytes)?;
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_EXEC) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        let renewed = crate::target_loader::resolve_host_initializer(&task, &config.runtime.bytes)
            .map_err(|e| self.caller_error(e))?;
        if renewed != initializer {
            return Err(self.caller_error("initializer coordinates changed"));
        }
        calls
            .start_initializer()
            .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_observe(
            "before initializer",
            format!("address={:#x}", initializer.address),
        )?;
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        let (task, _) = self
            .caller_function(
                task,
                image,
                &config,
                &mut calls,
                &code,
                [data + 3072, 0],
                CallerFunction::Initializer,
            )
            .await?;
        active_environment
            .compare_exact(
                &crate::target_loader::observe_environment_after(
                    &task,
                    &config.provider.bytes,
                    &config.environment,
                )
                .map_err(|error| self.caller_error(error))?,
            )
            .map_err(|error| self.caller_error(error))?;
        self.caller_observe("after initializer", format!("{:?}", calls.phase()))?;
        self.caller_snapshot(
            &task,
            "initializer returned",
            stack_address,
            stack_len,
            random_address,
        )?;
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        if self.caller_errno(&task, errno_pointer)? != saved_errno {
            return Err(self.caller_error("initializer changed guest errno"));
        }
        let (mut task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        let errno_code = CallCode::prepare(
            calls.guard(),
            code_base,
            stack_base + PAGE + STACK_SIZE,
            errno_resolver.address,
            RETURN_MARKER + 3,
        )
        .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_write(&mut task, code_base, &errno_code.bytes)?;
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_EXEC) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        if crate::target_loader::resolve_errno_location(&task, &config.provider.bytes)
            .map_err(|e| self.caller_error(e))?
            != errno_resolver
        {
            return Err(self.caller_error("errno provider changed across dlopen"));
        }
        self.caller_observe(
            "before second errno accessor",
            format!("address={:#x}", errno_resolver.address),
        )?;
        let (mut task, after_errno_pointer) = self
            .caller_function(
                task,
                image,
                &config,
                &mut calls,
                &errno_code,
                [0, 0],
                CallerFunction::ErrnoLocation,
            )
            .await?;
        active_environment
            .compare_exact(
                &crate::target_loader::observe_environment_after(
                    &task,
                    &config.provider.bytes,
                    &config.environment,
                )
                .map_err(|error| self.caller_error(error))?,
            )
            .map_err(|error| self.caller_error(error))?;
        if after_errno_pointer != errno_pointer
            || self.caller_errno(&task, after_errno_pointer)? != saved_errno
        {
            return Err(self.caller_error("errno accessor pointer or sentinel changed"));
        }
        self.caller_write(&mut task, errno_pointer, &saved_errno)?;
        self.caller_observe(
            "after second errno accessor",
            format!("pointer={after_errno_pointer:#x} bytes={saved_errno:?}"),
        )?;

        if let Some(plan) = vdso_plan.as_ref() {
            task = self.install_after_loader_vdso(task, plan)?;
        }

        let (mut restored_task, policy_failures) = self
            .restore_liteinst_helper_state(task, &helper_saved_state)
            .await
            .map_err(Error::Internal)?;
        if !policy_failures.is_empty() {
            return Err(self.caller_error(format!(
                "private runtime CPUID/TSC or machine-state restoration failed: {}",
                policy_failures.join("; ")
            )));
        }
        self.caller_write(&mut restored_task, policy_scratch_address, &policy_scratch)?;
        let mut restored_scratch = [0_u8; PRIVATE_POLICY_SCRATCH_BYTES];
        restored_task.read_exact(policy_scratch_address as usize, &mut restored_scratch)?;
        if restored_scratch != policy_scratch {
            return Err(self.caller_error("owned policy scratch restoration differs"));
        }
        task = restored_task;
        // Both controller and target descriptors remain alive through complete
        // provider reauthentication. Closing our target fd removes only that
        // added resource; the dlopen reference and mappings remain retained.
        if crate::target_loader::resolve_host_initializer(&task, &config.runtime.bytes)
            .map_err(|e| self.caller_error(e))?
            != initializer
        {
            return Err(self.caller_error("runtime provider changed after initializer"));
        }
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        let (task, _) = self
            .caller_syscall(task, image, Sysno::close, [runtime_fd, 0, 0, 0, 0, 0])
            .await?;
        if std::fs::symlink_metadata(format!("/proc/{}/fd/{runtime_fd}", task.pid())).is_ok() {
            return Err(self.caller_error("target runtime descriptor survived close"));
        }
        self.caller_observe(
            "target runtime descriptor closed",
            format!("fd={runtime_fd} handle={handle:#x}"),
        )?;
        if descriptor_state(task.pid()).map_err(|e| self.caller_error(e))? != saved_descriptors {
            return Err(self.caller_error("private calls changed original descriptor state"));
        }

        let (task, after_actions, after_mask) = self
            .caller_signal_actions(task, image, data, "final target signal actions")
            .await?;
        if after_actions != saved_actions || after_mask != saved_mask {
            return Err(
                self.caller_error("private calls changed exact target signal actions or mask")
            );
        }
        let (task, _) = self
            .caller_syscall(task, image, Sysno::sigaltstack, [0, data + 192, 0, 0, 0, 0])
            .await?;
        let mut after_altstack = [0; 24];
        task.read_exact((data + 192) as usize, &mut after_altstack)?;
        self.caller_observe(
            "alternate signal stack",
            format!("before={altstack:?} after={after_altstack:?}"),
        )?;
        if altstack_fields(&after_altstack) != altstack_fields(&altstack)
            || signal_state(task.pid()).map_err(|e| self.caller_error(e))? != saved_signals
        {
            return Err(self.caller_error("initializer changed signal state"));
        }
        let mut after_random = [0; 16];
        task.read_exact(random_address as usize, &mut after_random)?;
        let mut after_canary = [0; 8];
        task.read_exact((saved_regs.fs_base + 0x28) as usize, &mut after_canary)?;
        let mut after_stack = vec![0; saved_stack.len()];
        task.read_exact(stack_address as usize, &mut after_stack)?;
        if after_random != saved_random
            || after_canary != saved_canary
            || after_stack != saved_stack
        {
            return Err(
                self.caller_error("initializer changed guest random, canary or original stack")
            );
        }
        if bounded_proc(format!("/proc/{}/environ", task.pid()), 1024 * 1024)
            .map_err(|e| self.caller_error(e))?
            != environment
        {
            return Err(self.caller_error("initializer changed original environment bytes"));
        }
        let (task, _) = self
            .caller_syscall(task, image, Sysno::munmap, [code_base, PAGE, 0, 0, 0, 0])
            .await?;
        let (task, _) = self
            .caller_syscall(task, image, Sysno::munmap, [data, PAGE, 0, 0, 0, 0])
            .await?;
        let (mut task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::munmap,
                [stack_base, STACK_SIZE + 2 * PAGE, 0, 0, 0, 0],
            )
            .await?;
        self.restore_liteinst_entry_guard(&mut task)?;
        task.setxstate(&saved_xstate)?;
        task.setregs(&saved_regs)?;
        let mut restored_word = [0; 8];
        task.read_exact(image.at_entry as usize, &mut restored_word)?;
        if restored_word != calls.guard().original()
            || task.getxstate()? != saved_xstate
            || register_words(&task.getregs()?) != register_words(&saved_regs)
        {
            return Err(self.caller_error("entry or complete machine state readback differs"));
        }
        self.caller_quiescent(&task, image)?;
        if self.caller_errno(&task, errno_pointer)? != saved_errno {
            return Err(self.caller_error("restored errno readback differs"));
        }
        self.caller_snapshot(
            &task,
            "restored entry",
            stack_address,
            stack_len,
            random_address,
        )?;
        active_environment
            .compare_exact(
                &crate::target_loader::observe_environment_after(
                    &task,
                    &config.provider.bytes,
                    &config.environment,
                )
                .map_err(|error| self.caller_error(error))?,
            )
            .map_err(|error| self.caller_error(error))?;
        if self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
            || self.liteinst_after_loader_forward_inflight.is_some()
            || self.liteinst_after_loader_private_call.is_some()
        {
            return Err(self.caller_error("private syscall or call authority survived restoration"));
        }
        self.validate_after_loader_retained_resources(&task, &config, None)?;
        self.caller_quiescent(&task, image)?;
        let (prepared_arenas, prepared_reservations) = self
            .after_loader_private_state()?
            .prepared_liteinst_controls()
            .ok_or_else(|| {
                self.caller_error("retained LiteInst arena controls are not exact and bijective")
            })?;
        let frame = self
            .liteinst_runtime
            .lock()
            .unwrap()
            .frame
            .ok_or(Errno::EPROTO)?;
        let helper_code = bind_liteinst_helper_code(&task, frame).ok_or_else(|| {
            self.caller_error("retained LiteInst helper page could not be bound exactly")
        })?;
        let helper_isolation = self
            .after_loader_private_state()?
            .bind_helper_isolation(&config.sealed_runtime.image, &initializer, &helper_code)
            .ok_or_else(|| {
                self.caller_error(
                    "retained helper page does not equal its exact sealed-runtime file page",
                )
            })?;
        if !liteinst_helper_code_has_protection(
            &task,
            &helper_code,
            libc::PROT_READ | libc::PROT_EXEC,
        ) || !liteinst_helper_code_bytes_match(&task, &helper_code)
        {
            return Err(self.caller_error(
                "retained LiteInst helper page is not the exact sealed-runtime RX page",
            ));
        }
        {
            let runtime = self.liteinst_runtime.lock().unwrap();
            if !after_loader_liteinst_ready_is_publishable(
                &runtime,
                image.generation,
                frame,
                &prepared_arenas,
                &prepared_reservations,
                &helper_code,
                &initializer,
            ) {
                return Err(self.caller_error(
                    "initializer runtime state is not an empty Bootstrap publication",
                ));
            }
        }

        let (next, helper_protection_result) = self
            .set_liteinst_internal_protection(task, helper_code.range, libc::PROT_NONE)
            .await
            .map_err(Error::Internal)?;
        task = next;
        task.setxstate(&saved_xstate)?;
        task.setregs(&saved_regs)?;
        self.caller_quiescent(&task, image)?;
        if helper_protection_result != Ok(0)
            || !liteinst_helper_code_has_protection(&task, &helper_code, libc::PROT_NONE)
            || !liteinst_helper_code_bytes_match(&task, &helper_code)
            || !helper_isolation.validates_live_page(&task)
        {
            return Err(self.caller_error(format!(
                "isolate retained LiteInst helper page: mprotect result {helper_protection_result:?} or exact map/byte readback differed"
            )));
        }
        let mut post_isolation_entry = [0_u8; 8];
        task.read_exact(image.at_entry as usize, &mut post_isolation_entry)?;
        let mut post_isolation_private_stub = [0_u8; 4];
        task.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut post_isolation_private_stub)?;
        if post_isolation_entry != calls.guard().original()
            || post_isolation_private_stub != [0x0f, 0x05, 0x0f, 0x0b]
            || task.getxstate()? != saved_xstate
            || register_words(&task.getregs()?) != register_words(&saved_regs)
        {
            return Err(self.caller_error(
                "helper-page isolation changed the entry word, private stub or complete machine state",
            ));
        }
        self.liteinst_after_loader_private_state
            .as_mut()
            .ok_or(Errno::EPROTO)?
            .protect_owned_range(
                (helper_isolation.range.start, helper_isolation.range.end),
                libc::PROT_NONE,
            );
        let (next, arena_aliases_isolated) = self
            .set_liteinst_arena_writable_protection(
                task,
                &prepared_arenas,
                libc::PROT_NONE,
            )
            .await
            .map_err(Error::Internal)?;
        task = next;
        task.setxstate(&saved_xstate)?;
        task.setregs(&saved_regs)?;
        self.caller_quiescent(&task, image)?;
        if !arena_aliases_isolated {
            return Err(self.caller_error(
                "isolate retained LiteInst arena writable aliases: PROT_NONE transition or exact map readback differed",
            ));
        }
        for arena in &prepared_arenas {
            self.liteinst_after_loader_private_state
                .as_mut()
                .ok_or(Errno::EPROTO)?
                .protect_owned_range(
                    (arena.writable.start, arena.writable.end),
                    libc::PROT_NONE,
                );
        }
        self.validate_after_loader_retained_resources(&task, &config, Some(&helper_isolation))?;
        let projection = helper_isolation.target_loader_projection().ok_or_else(|| {
            self.caller_error("isolated helper page cannot form an exact target-loader projection")
        })?;
        let renewed_initializer =
            crate::target_loader::resolve_host_initializer_with_isolated_page(
                &task,
                &config.runtime.bytes,
                projection,
                |address, bytes| {
                    read_exact_with_helper_isolation(&task, address, bytes, Some(&helper_isolation))
                        .map_err(|error| io::Error::other(error.to_string()))
                },
            )
            .map_err(|error| self.caller_error(error))?;
        if renewed_initializer != initializer {
            return Err(self.caller_error(
                "sealed-runtime initializer coordinates changed after helper-page isolation",
            ));
        }
        let initializer = renewed_initializer;
        self.caller_quiescent(&task, image)?;
        if self.timer.diagnostic_clock() != Some(frozen_timer_clock) {
            return Err(
                self.caller_error("deterministic clock changed during final helper-page isolation")
            );
        }
        self.caller_observe(
            "guest machine state restored and helper/arena writers isolated",
            format!(
                "retained dlopen handle={handle:#x} helper={:#x}-{:#x} arenas={} reservations={}",
                helper_code.range.start,
                helper_code.range.end,
                prepared_arenas.len(),
                prepared_reservations.len(),
            ),
        )?;
        calls
            .restored()
            .map_err(|e| self.caller_error(format!("{e:?}")))?;
        let restore_timer = {
            let timer_suspension = self
                .liteinst_after_loader_private_state
                .as_ref()
                .and_then(|state| state.timer_suspension.as_ref())
                .ok_or(Errno::EPROTO)?;
            self.timer
                .restore_after_private_execution(timer_suspension)
        };
        if let Err(error) = restore_timer {
            return Err(self.caller_error(format!(
                "restore deterministic timer after private after-loader execution: {error}"
            )));
        }
        if self.timer.diagnostic_clock() != Some(frozen_timer_clock) {
            return Err(self.caller_error(
                "deterministic clock changed across private after-loader execution",
            ));
        }
        let mut runtime = self.liteinst_runtime.lock().unwrap();
        if !after_loader_liteinst_ready_is_publishable(
            &runtime,
            image.generation,
            frame,
            &prepared_arenas,
            &prepared_reservations,
            &helper_code,
            &initializer,
        ) {
            return Err(self.caller_error(
                "initializer runtime state changed before atomic Ready publication",
            ));
        }
        let mut validated_state = self
            .liteinst_after_loader_private_state
            .take()
            .ok_or(Errno::EPROTO)?;
        drop(
            validated_state
                .timer_suspension
                .take()
                .ok_or(Errno::EPROTO)?,
        );
        commit_after_loader_liteinst_ready(
            &mut runtime,
            image.generation,
            prepared_arenas,
            prepared_reservations,
            helper_code,
            handle,
            initializer,
        );
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping_state(generation: u64) -> AfterLoaderPrivateState {
        AfterLoaderPrivateState {
            image: ImageIdentity {
                tid: 17,
                start_ticks: 23,
                generation,
                executable_device: 29,
                executable_inode: 31,
                at_entry: 0x401000,
                at_phdr: 0x400040,
            },
            original_mappings: Vec::new(),
            original_descriptors: BTreeSet::new(),
            owned_descriptors: BTreeMap::new(),
            owned_mappings: Vec::new(),
            current_break: None,
            shared_reservations: BTreeMap::new(),
            protected_ranges: Vec::new(),
            image_mappings: BTreeMap::new(),
            image_geometries: BTreeMap::new(),
            trampoline_mappings: BTreeMap::new(),
            trampoline_seals_added: BTreeSet::new(),
            sealed_trampolines: BTreeSet::new(),
            next_trampoline_serial: 0,
            timer_suspension: None,
        }
    }

    fn controller_mapping(start: u64, end: u64) -> AfterLoaderOwnedMapping {
        AfterLoaderOwnedMapping {
            start,
            end,
            readable: true,
            writable: true,
            executable: false,
            shared: false,
            descriptor: None,
            offset: 0,
            purpose: AfterLoaderMappingPurpose::Controller,
        }
    }

    #[test]
    fn helper_isolation_binding_refuses_inexact_owned_provenance() {
        let source = ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .unwrap();
        let runtime = LiteinstCallerImage::read(source).unwrap();
        assert!(runtime.bytes.len() >= 2 * PAGE as usize);

        let mut state = mapping_state(11);
        let image = state.image_id(&runtime);
        let mapping = MappingIdentity {
            device_major: 8,
            device_minor: 1,
            inode: 37,
        };
        let load_bias = 0x70_0000;
        let geometry = ResolvedImageGeometry {
            image,
            mapping,
            load_bias,
            span: (load_bias, load_bias + 4 * PAGE),
        };
        assert!(state.bind_image_geometry(geometry));

        let original_mapping = GuestMap {
            start: geometry.span.0,
            end: geometry.span.1,
            offset: 0,
            device_major: mapping.device_major,
            device_minor: mapping.device_minor,
            readable: true,
            writable: false,
            executable: true,
            shared: false,
            inode: mapping.inode,
            path: Some(runtime.path.clone()),
        };
        let helper = LiteinstHelperCode {
            range: GuestRange::new(load_bias + PAGE, PAGE).unwrap(),
            original_mapping: original_mapping.clone(),
            bytes: runtime.bytes[PAGE as usize..2 * PAGE as usize].to_vec(),
        };
        let initializer = crate::target_loader::TargetHostInitializer {
            tid: state.image.tid,
            start_ticks: state.image.start_ticks,
            executable_phdr: state.image.at_phdr,
            link_map: 0x60_0000,
            load_bias,
            address: load_bias + 0x100,
            mapping_identity: mapping.as_target_loader(),
        };
        let owned = AfterLoaderOwnedMapping {
            start: original_mapping.start,
            end: original_mapping.end,
            readable: true,
            writable: false,
            executable: true,
            shared: false,
            descriptor: Some(9),
            offset: 0,
            purpose: AfterLoaderMappingPurpose::Image { image },
        };
        state.owned_mappings.push(owned);
        assert!(
            state
                .bind_helper_isolation(&runtime, &initializer, &helper)
                .is_some(),
            "exact sealed-runtime helper provenance was refused"
        );

        let mut missing = state.clone();
        missing.owned_mappings.clear();
        assert!(
            missing
                .bind_helper_isolation(&runtime, &initializer, &helper)
                .is_none(),
            "missing owned image mapping was accepted"
        );

        let mut duplicate = state.clone();
        duplicate.owned_mappings.push(owned);
        assert!(
            duplicate
                .bind_helper_isolation(&runtime, &initializer, &helper)
                .is_none(),
            "duplicate owned image mapping was accepted"
        );

        let mut wrong_purpose = state.clone();
        wrong_purpose.owned_mappings[0].purpose = AfterLoaderMappingPurpose::Controller;
        assert!(
            wrong_purpose
                .bind_helper_isolation(&runtime, &initializer, &helper)
                .is_none(),
            "controller-owned mapping was accepted as sealed-runtime provenance"
        );

        let mut wrong_offset = state.clone();
        wrong_offset.owned_mappings[0].offset = PAGE;
        assert!(
            wrong_offset
                .bind_helper_isolation(&runtime, &initializer, &helper)
                .is_none(),
            "owned mapping with a mismatched file offset was accepted"
        );

        let mut wrong_identity = helper.clone();
        wrong_identity.original_mapping.inode ^= 1;
        assert!(
            state
                .bind_helper_isolation(&runtime, &initializer, &wrong_identity)
                .is_none(),
            "helper mapping with a mismatched identity was accepted"
        );

        let mut wrong_expected_bytes = helper;
        wrong_expected_bytes.bytes[0] ^= 1;
        assert!(
            state
                .bind_helper_isolation(&runtime, &initializer, &wrong_expected_bytes)
                .is_none(),
            "helper bytes differing from the sealed runtime were accepted"
        );
    }

    #[test]
    fn canonical_c_int_arguments_refuse_mixed_high_words() {
        for (raw, expected) in [
            (0x0000_0000_ffff_ff9c, libc::AT_FDCWD),
            (0xffff_ffff_ffff_ff9c, libc::AT_FDCWD),
            (0x0000_0000_ffff_ffff, -1),
            (0xffff_ffff_ffff_ffff, -1),
            (libc::O_CLOEXEC as u64, libc::O_CLOEXEC),
        ] {
            assert_eq!(canonical_c_int_argument(raw), Some(expected));
        }

        for raw in [
            0x0000_0001_ffff_ff9c,
            0xffff_fffe_ffff_ff9c,
            0xdead_beef_ffff_ff9c,
            0x0000_0001_ffff_ffff,
            0xffff_fffe_ffff_ffff,
            (1_u64 << 32) | libc::O_CLOEXEC as u64,
        ] {
            assert_eq!(
                canonical_c_int_argument(raw),
                None,
                "accepted noncanonical C int {raw:#018x}"
            );
        }
    }

    #[test]
    fn trace_only_classification_rejects_mixed_high_and_x32_before_forwarding() {
        for low in [
            libc::SYS_rt_sigreturn,
            libc::SYS_execve,
            libc::SYS_rt_sigaction,
            libc::SYS_mmap,
            libc::SYS_munmap,
            libc::SYS_mprotect,
        ] {
            assert_eq!(
                classify_trace_only_syscall_number((1_u64 << 32) | low as u64),
                Err(Errno::ENOSYS)
            );
        }
        for low in [512_u64, 513_u64, libc::SYS_getpid as u64] {
            assert_eq!(
                classify_trace_only_syscall_number(low | X32_SYSCALL_BIT),
                Err(Errno::ENOSYS)
            );
        }
        assert_eq!(
            classify_trace_only_syscall_number(libc::SYS_rt_sigreturn as u64),
            Ok((libc::SYS_rt_sigreturn, Some(Sysno::rt_sigreturn),))
        );
    }

    #[test]
    fn semantic_admission_ignores_only_registers_beyond_the_syscall_arity() {
        let extra = [0x1111, 0x2222, 0x3333, 0x4444, 0x5555];
        assert!(controller_semantic_arguments_match(
            libc::SYS_arch_prctl,
            [
                ARCH_SHSTK_STATUS,
                0x4000,
                extra[1],
                extra[2],
                extra[3],
                extra[4]
            ],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_sigaltstack,
            [0, 0x5000, extra[1], extra[2], extra[3], extra[4]],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_rt_sigaction,
            [libc::SIGUSR1 as u64, 0, 0x6000, 8, extra[3], extra[4]],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_rt_sigprocmask,
            [libc::SIG_SETMASK as u64, 0, 0x7000, 8, extra[3], extra[4]],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_brk,
            [0, extra[0], extra[1], extra[2], extra[3], extra[4]],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_fcntl,
            [9, libc::F_GET_SEALS as u64, 0, extra[2], extra[3], extra[4]],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_close,
            [9, extra[0], extra[1], extra[2], extra[3], extra[4]],
        ));
        assert!(initializer_fcntl_add_seals_arguments_match([
            9,
            libc::F_ADD_SEALS as u64,
            TRAMPOLINE_SEALS as u64,
            extra[2],
            extra[3],
            extra[4],
        ]));

        assert!(!controller_semantic_arguments_match(
            libc::SYS_arch_prctl,
            [ARCH_SHSTK_STATUS + 1, 0x4000, 0, 0, 0, 0],
        ));
        assert!(!controller_semantic_arguments_match(
            libc::SYS_rt_sigaction,
            [libc::SIGUSR1 as u64, 0, 0x6000, 16, 0, 0],
        ));
        assert!(!controller_semantic_arguments_match(
            libc::SYS_fcntl,
            [9, libc::F_GET_SEALS as u64, 1, 0, 0, 0],
        ));
        assert!(!initializer_fcntl_add_seals_arguments_match([
            9,
            libc::F_ADD_SEALS as u64,
            (TRAMPOLINE_SEALS as u64) ^ 1,
            0,
            0,
            0,
        ]));

        let exact_futex = [
            0x8000,
            (libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG) as u64,
            1,
            0,
            0,
            0,
        ];
        assert!(initializer_futex_arguments_match(exact_futex));
        for index in 3..6 {
            let mut smuggled = exact_futex;
            smuggled[index] = index as u64;
            assert!(
                !initializer_futex_arguments_match(smuggled),
                "accepted within-arity futex register {index}"
            );
        }
    }

    #[test]
    fn raw_permit_comparator_pins_ignored_registers_across_stops() {
        let number = libc::SYS_arch_prctl;
        let args = [ARCH_SHSTK_STATUS, 0x4000, 11, 22, 33, 44];
        assert!(controller_semantic_arguments_match(number, args));
        let mut registers = libc::user_regs_struct {
            orig_rax: number as u64,
            rdi: args[0],
            rsi: args[1],
            rdx: args[2],
            r10: args[3],
            r8: args[4],
            r9: args[5],
            ..unsafe { core::mem::zeroed() }
        };
        assert!(after_loader_syscall_registers_match(
            number, args, &registers
        ));
        for index in 2..6 {
            let original = registers;
            match index {
                2 => registers.rdx ^= 1,
                3 => registers.r10 ^= 1,
                4 => registers.r8 ^= 1,
                5 => registers.r9 ^= 1,
                _ => unreachable!(),
            }
            assert!(
                !after_loader_syscall_registers_match(number, args, &registers),
                "permit accepted changed ignored register {index}"
            );
            registers = original;
        }
        registers.orig_rax ^= 1;
        assert!(!after_loader_syscall_registers_match(
            number, args, &registers
        ));
    }

    #[test]
    fn readonly_openat_shape_keeps_flags_mode_and_high_words_exact() {
        let expected_flags = (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_LARGEFILE) as u64;
        let mut args = [
            0x0000_0000_ffff_ff9c,
            0x1234,
            expected_flags,
            0,
            0xfeed_face,
            0xdead_beef,
        ];
        assert!(exact_readonly_openat_arguments(&args));
        args[0] = 0xffff_ffff_ffff_ff9c;
        assert!(exact_readonly_openat_arguments(&args));

        for dirfd in [
            0x0000_0001_ffff_ff9c,
            0xffff_fffe_ffff_ff9c,
            0xdead_beef_ffff_ff9c,
            0x0000_0000_ffff_ff9b,
            0xffff_ffff_ffff_ff9d,
            3,
        ] {
            let mut changed = args;
            changed[0] = dirfd;
            assert!(!exact_readonly_openat_arguments(&changed));
        }
        for flags in [
            expected_flags & !(libc::O_CLOEXEC as u64),
            expected_flags | libc::O_WRONLY as u64,
            expected_flags | libc::O_CREAT as u64,
            expected_flags | libc::O_PATH as u64,
            expected_flags | (1_u64 << 32),
        ] {
            let mut changed = args;
            changed[2] = flags;
            assert!(!exact_readonly_openat_arguments(&changed));
        }
        for mode in [1, 1_u64 << 32, u64::MAX] {
            let mut changed = args;
            changed[3] = mode;
            assert!(!exact_readonly_openat_arguments(&changed));
        }
    }

    #[test]
    fn anonymous_mmap_descriptor_accepts_only_canonical_minus_one() {
        assert!(canonical_anonymous_mmap_descriptor(0x0000_0000_ffff_ffff));
        assert!(canonical_anonymous_mmap_descriptor(u64::MAX));
        for raw in [
            0x0000_0001_ffff_ffff,
            0xffff_fffe_ffff_ffff,
            0xdead_beef_ffff_ffff,
            0,
            3,
        ] {
            assert!(!canonical_anonymous_mmap_descriptor(raw));
        }
    }

    #[test]
    fn page_effect_lengths_follow_linux_rounding_without_changing_raw_permits() {
        let start = 0x20_0000;
        for (raw_length, effective_length) in [
            (1, PAGE),
            (PAGE - 1, PAGE),
            (PAGE, PAGE),
            (PAGE + 1, 2 * PAGE),
            (0x7d6_f702, 0x7d70_000),
        ] {
            assert_eq!(checked_page_effect_length(raw_length), Ok(effective_length));
            assert_eq!(
                checked_page_effect_range(start, raw_length),
                Ok((start, start + effective_length))
            );
        }

        assert_eq!(checked_page_effect_length(0), Err(Errno::EINVAL));
        assert_eq!(
            checked_page_effect_range(start + 1, PAGE),
            Err(Errno::EINVAL)
        );
        assert_eq!(checked_page_effect_length(u64::MAX), Err(Errno::EOVERFLOW));
        assert_eq!(
            checked_page_effect_range(u64::MAX - (PAGE - 1), PAGE),
            Err(Errno::EOVERFLOW)
        );

        let args = [
            0,
            0x7d6_f702,
            libc::PROT_READ as u64,
            (libc::MAP_PRIVATE | libc::MAP_DENYWRITE) as u64,
            4,
            0,
        ];
        let permit = AfterLoaderSyscallPermit {
            image: None,
            tid: Pid::from_raw(17),
            generation: 23,
            origin_status: None,
            admission_status: None,
            purpose: AfterLoaderSyscallPurpose::PrivateSetup,
            number: libc::SYS_mmap,
            args,
            instruction_pointer: 0x401000,
            resume_pointer: 0x401002,
            instruction: [0x0f, 0x05, 0, 0],
            instruction_length: 2,
            output_spans: Vec::new(),
            effect: AfterLoaderSyscallEffect::Map {
                requested: args[0],
                raw_length: args[1],
                protection: libc::PROT_READ,
                flags: libc::MAP_PRIVATE | libc::MAP_DENYWRITE,
                descriptor: Some(args[4]),
                offset: args[5],
                purpose: AfterLoaderMappingPurpose::Controller,
            },
        };
        assert_eq!(permit.args, args, "page rounding changed raw permit args");
        assert!(matches!(
            permit.effect,
            AfterLoaderSyscallEffect::Map {
                raw_length: 0x7d6_f702,
                ..
            }
        ));
    }

    #[test]
    fn private_mmap_effect_cap_and_offset_alignment_are_exact() {
        assert_eq!(
            checked_private_mmap_effect_length(MAX_PRIVATE_MMAP_EFFECT),
            Ok(MAX_PRIVATE_MMAP_EFFECT)
        );
        assert_eq!(
            checked_private_mmap_effect_length(MAX_PRIVATE_MMAP_EFFECT - PAGE + 1),
            Ok(MAX_PRIVATE_MMAP_EFFECT)
        );
        assert_eq!(
            checked_private_mmap_effect_length(MAX_PRIVATE_MMAP_EFFECT + 1),
            Err(Errno::EINVAL)
        );
        assert!(private_mmap_offset_is_admissible(0));
        assert!(private_mmap_offset_is_admissible(PAGE));
        assert!(private_mmap_offset_is_admissible(
            (i64::MAX as u64) & !(PAGE - 1)
        ));
        assert!(!private_mmap_offset_is_admissible(1));
        assert!(!private_mmap_offset_is_admissible(PAGE + 1));
        assert!(!private_mmap_offset_is_admissible(i64::MAX as u64));
        assert!(!private_mmap_offset_is_admissible(1_u64 << 63));

        assert!(exact_trampoline_mmap_length(TRAMPOLINE_ARENA_SIZE));
        assert!(!exact_trampoline_mmap_length(TRAMPOLINE_ARENA_SIZE - 1));
        assert_eq!(
            checked_page_effect_length(TRAMPOLINE_ARENA_SIZE - 1),
            Ok(TRAMPOLINE_ARENA_SIZE),
            "near-miss control must reach the exact raw-size gate"
        );
        assert!(exact_shared_reservation_mmap_length(PAGE));
        assert!(!exact_shared_reservation_mmap_length(PAGE - 1));
        assert_eq!(
            checked_page_effect_length(PAGE - 1),
            Ok(PAGE),
            "near-miss control must reach the exact raw-size gate"
        );
    }

    #[test]
    fn rounded_page_tail_drives_overlap_ownership_protection_and_removal() {
        let start = 0x40_0000;
        let raw_length = PAGE + 1;
        let raw_range = checked_range(start, raw_length).unwrap();
        let effective_range = checked_page_effect_range(start, raw_length).unwrap();
        assert_eq!(effective_range, (start, start + 2 * PAGE));

        let tail = (raw_range.1, effective_range.1);
        let mut overlap = mapping_state(1);
        overlap.original_mappings.push(tail);
        overlap.protected_ranges.push(tail);
        assert!(!overlap.refuses_original_overlap(raw_range));
        assert!(!overlap.refuses_protected_overlap(raw_range));
        assert!(overlap.refuses_original_overlap(effective_range));
        assert!(overlap.refuses_protected_overlap(effective_range));

        let mut protected = mapping_state(2);
        protected
            .owned_mappings
            .push(controller_mapping(start, start + 3 * PAGE));
        assert!(protected.owns_range(effective_range, false));
        protected.protect_owned_range(effective_range, libc::PROT_READ);
        protected
            .owned_mappings
            .sort_by_key(|mapping| mapping.start);
        assert_eq!(protected.owned_mappings.len(), 2);
        assert_eq!(
            (
                protected.owned_mappings[0].start,
                protected.owned_mappings[0].end
            ),
            effective_range
        );
        assert!(protected.owned_mappings[0].readable);
        assert!(!protected.owned_mappings[0].writable);
        assert_eq!(
            (
                protected.owned_mappings[1].start,
                protected.owned_mappings[1].end
            ),
            (start + 2 * PAGE, start + 3 * PAGE)
        );
        assert!(protected.owned_mappings[1].writable);

        let mut removed = mapping_state(3);
        removed
            .owned_mappings
            .push(controller_mapping(start, start + 3 * PAGE));
        removed.remove_owned_range(effective_range);
        assert_eq!(removed.owned_mappings.len(), 1);
        assert_eq!(
            (
                removed.owned_mappings[0].start,
                removed.owned_mappings[0].end
            ),
            (start + 2 * PAGE, start + 3 * PAGE)
        );
        assert_eq!(removed.owned_mappings[0].offset, 2 * PAGE);

        let one_page = {
            let mut state = mapping_state(4);
            state
                .owned_mappings
                .push(controller_mapping(start, start + PAGE));
            state
        };
        assert!(!one_page.owns_range(effective_range, false));
        assert_eq!(
            checked_page_effect_range(start + 1, 1),
            Err(Errno::EINVAL),
            "unaligned mprotect/munmap start was rounded instead of refused"
        );
    }

    #[test]
    fn fixed_file_mmap_can_replace_only_its_exact_image_reservation() {
        let range = (0x60_0000, 0x60_0000 + 2 * PAGE);
        let image = AfterLoaderImageId {
            generation: 5,
            file: crate::after_loader::FileIdentity {
                device: 7,
                inode: 11,
            },
        };
        let other_image = AfterLoaderImageId {
            file: crate::after_loader::FileIdentity {
                device: 13,
                inode: 17,
            },
            ..image
        };
        let trampoline = AfterLoaderTrampolineId {
            generation: image.generation,
            serial: 1,
            file: crate::after_loader::FileIdentity {
                device: 19,
                inode: 23,
            },
        };
        let mut state = mapping_state(image.generation);
        let mut mapping = controller_mapping(range.0, range.1);
        mapping.purpose = AfterLoaderMappingPurpose::Image { image };
        state.owned_mappings.push(mapping);
        assert!(state.owns_same_image_range(range, image));
        assert!(!state.owns_same_image_range(range, other_image));

        for purpose in [
            AfterLoaderMappingPurpose::Controller,
            AfterLoaderMappingPurpose::Trampoline { trampoline },
            AfterLoaderMappingPurpose::SharedReservation { trampoline },
        ] {
            state.owned_mappings[0].purpose = purpose;
            assert!(
                !state.owns_same_image_range(range, image),
                "accepted foreign fixed-mmap owner {purpose:?}"
            );
        }

        state.owned_mappings = vec![
            AfterLoaderOwnedMapping {
                end: range.0 + PAGE,
                purpose: AfterLoaderMappingPurpose::Image { image },
                ..mapping
            },
            AfterLoaderOwnedMapping {
                start: range.0 + PAGE,
                offset: PAGE,
                purpose: AfterLoaderMappingPurpose::ImageZeroFill { image },
                ..mapping
            },
        ];
        assert!(
            state.owns_same_image_range(range, image),
            "refused gap-free adjacent same-image file/BSS reservations"
        );

        let mut gap = state.clone();
        gap.owned_mappings[1].start += 1;
        assert!(!gap.owns_same_image_range(range, image));

        let mut overlap = state.clone();
        overlap.owned_mappings[1].start -= 1;
        assert!(
            !overlap.owns_same_image_range(range, image),
            "accepted overlapping same-image reservations"
        );

        let mut foreign = state;
        foreign.owned_mappings[1].purpose =
            AfterLoaderMappingPurpose::ImageZeroFill { image: other_image };
        assert!(!foreign.owns_same_image_range(range, image));
    }

    #[test]
    fn rounded_munmap_must_remove_a_shared_reservation_exactly() {
        let start = 0x70_0000;
        let range = (start, start + PAGE);
        let trampoline = AfterLoaderTrampolineId {
            generation: 7,
            serial: 1,
            file: crate::after_loader::FileIdentity {
                device: 29,
                inode: 31,
            },
        };
        let mut state = mapping_state(trampoline.generation);
        state.shared_reservations.insert(
            trampoline,
            (
                range,
                SharedReservationIdentity {
                    mapping: MappingIdentity {
                        device_major: 0,
                        device_minor: 1,
                        inode: 37,
                    },
                    path: Some(PathBuf::from("/dev/zero (deleted)")),
                },
            ),
        );
        let mut mapping = controller_mapping(range.0, range.1);
        mapping.shared = true;
        mapping.purpose = AfterLoaderMappingPurpose::SharedReservation { trampoline };
        state.owned_mappings.push(mapping);

        let raw_range = checked_range(start, 1).unwrap();
        let effective_range = checked_page_effect_range(start, 1).unwrap();
        assert!(!state.shared_reservation_unmap_is_exact(raw_range));
        assert!(state.shared_reservation_unmap_is_exact(effective_range));
        assert!(
            !state.shared_reservation_unmap_is_exact(
                checked_page_effect_range(start, PAGE + 1).unwrap()
            ),
            "accepted an effective range extending beyond the reservation"
        );

        state.remove_owned_range(effective_range);
        assert!(state.owned_mappings.is_empty());
        assert!(!state.shared_reservations.contains_key(&trampoline));
    }

    #[test]
    fn mprotect_and_munmap_completion_require_exact_zero() {
        let protect = AfterLoaderSyscallEffect::Protect {
            start: 0x80_0000,
            raw_length: 1,
            protection: libc::PROT_READ,
        };
        let remove = AfterLoaderSyscallEffect::Remove {
            start: 0x80_0000,
            raw_length: 1,
        };
        for effect in [&protect, &remove] {
            assert!(private_memory_effect_result_is_exact(effect, 0));
            assert!(!private_memory_effect_result_is_exact(effect, 1));
            assert!(!private_memory_effect_result_is_exact(effect, -1));
        }

        let map = AfterLoaderSyscallEffect::Map {
            requested: 0,
            raw_length: 1,
            protection: libc::PROT_READ,
            flags: libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            descriptor: None,
            offset: 0,
            purpose: AfterLoaderMappingPurpose::Controller,
        };
        assert!(private_memory_effect_result_is_exact(&map, 0x80_0000));
    }

    #[test]
    fn read_completion_accepts_only_an_exact_nonempty_prefix_or_eof() {
        let expected = vec![0x5a; 4096];
        assert_eq!(exact_read_prefix_length(4078, &expected), Some(4078));
        assert_eq!(exact_read_prefix_length(4096, &expected), Some(4096));
        assert_eq!(exact_read_prefix_length(0, &expected), None);
        assert_eq!(exact_read_prefix_length(4097, &expected), None);
        assert_eq!(exact_read_prefix_length(-1, &expected), None);

        assert_eq!(exact_read_prefix_length(0, &[]), Some(0));
        assert_eq!(exact_read_prefix_length(1, &[]), None);

        assert_eq!(exact_read_window_end(0, 4096, 0), None);
        assert_eq!(exact_read_window_end(4096, 4096, 0), Some(4096));
        assert_eq!(exact_read_window_end(4096, 4096, 17), Some(4096));
        assert_eq!(exact_read_window_end(4097, 4096, 1), None);
        assert_eq!(exact_read_window_end(7, 4096, 11), Some(18));
        assert_eq!(exact_read_window_end(usize::MAX, usize::MAX, 1), None);
    }

    #[test]
    fn owned_descriptor_statx_shape_is_exact_and_output_is_full_width() {
        assert_eq!(STATX_OUTPUT_BYTES, 256);
        assert_eq!(PRIVATE_STATX_MASK, 0x0fff);
        assert_eq!(PRIVATE_STATX_MASK, libc::STATX_ALL);
        let args = [
            7,
            0x1234,
            libc::AT_EMPTY_PATH as u64,
            PRIVATE_STATX_MASK as u64,
            0x5678,
            0xfeed_face_dead_beef,
        ];
        assert!(exact_owned_descriptor_statx_arguments(&args));

        for (index, value) in [
            (2, 0),
            (2, libc::AT_SYMLINK_NOFOLLOW as u64),
            (2, libc::AT_EMPTY_PATH as u64 | (1_u64 << 32)),
            (3, libc::STATX_BASIC_STATS as u64),
            (3, PRIVATE_STATX_MASK as u64 | (1_u64 << 32)),
        ] {
            let mut changed = args;
            changed[index] = value;
            assert!(!exact_owned_descriptor_statx_arguments(&changed));
        }

        let file = std::fs::File::open("/proc/self/maps").unwrap();
        let direct = fd_statx_bytes(file.as_raw_fd()).unwrap();
        let bytes = descriptor_statx_bytes(
            Pid::from_raw(std::process::id() as i32),
            file.as_raw_fd() as u64,
        )
        .unwrap();
        assert_eq!(bytes, direct);
        let mask = u32::from_ne_bytes(bytes[..4].try_into().unwrap());
        assert_eq!(mask & libc::STATX_BASIC_STATS, libc::STATX_BASIC_STATS);
    }

    #[test]
    fn file_and_mapping_identity_domains_bind_without_device_conflation() {
        let mut state = mapping_state(1);
        // This models the observed Btrfs split: pathname stat reports 0:21,
        // while /proc/<pid>/maps reports 0:20 for the exact mapped image.
        let image = AfterLoaderImageId {
            generation: 1,
            file: crate::after_loader::FileIdentity {
                device: 0x21,
                inode: 21_907_919,
            },
        };
        let mapping = MappingIdentity {
            device_major: 0,
            device_minor: 0x20,
            inode: 21_907_919,
        };
        let geometry = ResolvedImageGeometry {
            image,
            mapping,
            load_bias: 0x7f00_0000,
            span: (0x7f00_0000, 0x7f01_0000),
        };
        assert!(!state.has_causal_image_mapping(geometry));
        assert!(state.bind_image_mapping(image, mapping));
        assert!(state.has_causal_image_mapping(geometry));
        assert!(state.bind_image_geometry(geometry));
        assert_eq!(state.image_mappings.get(&image), Some(&mapping));
        assert_eq!(state.image_geometries.get(&image), Some(&geometry));
        assert!(
            state.bind_image_geometry(geometry),
            "exact rebinding changed"
        );

        for changed in [
            MappingIdentity {
                device_major: 1,
                ..mapping
            },
            MappingIdentity {
                device_minor: 0x21,
                ..mapping
            },
            MappingIdentity {
                inode: mapping.inode + 1,
                ..mapping
            },
        ] {
            assert!(
                !state.bind_image_mapping(image, changed),
                "accepted changed map identity component: {changed:?}"
            );
        }
        let colliding_image = AfterLoaderImageId {
            file: crate::after_loader::FileIdentity {
                device: 0x22,
                inode: image.file.inode,
            },
            ..image
        };
        assert!(!state.bind_image_mapping(colliding_image, mapping));
        assert!(!state.bind_image_mapping(
            AfterLoaderImageId {
                generation: 2,
                ..image
            },
            mapping,
        ));

        assert_eq!(one_exact_geometry_candidate(&[]), None);
        assert_eq!(
            one_exact_geometry_candidate(&[(mapping, 0x1000)]),
            Some((mapping, 0x1000))
        );
        assert_eq!(
            one_exact_geometry_candidate(&[(mapping, 0x1000), (mapping, 0x2000)]),
            None,
            "duplicate exact geometries were not refused"
        );
        assert!(loader_resolution_matches_geometry(
            mapping.as_target_loader(),
            geometry.load_bias,
            geometry,
        ));
        for (identity, bias) in [
            (
                (
                    mapping.device_major + 1,
                    mapping.device_minor,
                    mapping.inode,
                ),
                geometry.load_bias,
            ),
            (
                (
                    mapping.device_major,
                    mapping.device_minor + 1,
                    mapping.inode,
                ),
                geometry.load_bias,
            ),
            (
                (
                    mapping.device_major,
                    mapping.device_minor,
                    mapping.inode + 1,
                ),
                geometry.load_bias,
            ),
            (mapping.as_target_loader(), geometry.load_bias + PAGE),
        ] {
            assert!(!loader_resolution_matches_geometry(
                identity, bias, geometry
            ));
        }
    }

    #[test]
    fn trampoline_mapping_identity_is_captured_once_per_generation() {
        let mut state = mapping_state(4);
        let trampoline = state
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 0x21,
                inode: 91,
            })
            .unwrap();
        let mapping = MappingIdentity {
            device_major: 0,
            device_minor: 1,
            inode: 91,
        };
        assert!(state.bind_trampoline_mapping(trampoline, mapping));
        assert!(state.bind_trampoline_mapping(trampoline, mapping));
        assert_eq!(state.trampoline_mapping(trampoline), Some(mapping));
        for changed in [
            MappingIdentity {
                device_major: 1,
                ..mapping
            },
            MappingIdentity {
                device_minor: 2,
                ..mapping
            },
            MappingIdentity {
                inode: 92,
                ..mapping
            },
        ] {
            assert!(!state.bind_trampoline_mapping(trampoline, changed));
        }
        let next = state
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 0x21,
                inode: 93,
            })
            .unwrap();
        assert_ne!(trampoline.serial, next.serial);
        assert!(!state.bind_trampoline_mapping(next, mapping));
        assert!(!state.bind_trampoline_mapping(
            AfterLoaderTrampolineId {
                generation: 5,
                ..next
            },
            MappingIdentity {
                inode: 94,
                ..mapping
            },
        ));
    }

    #[test]
    fn abandoned_trampoline_close_requires_every_partial_alias_removed() {
        let mut state = mapping_state(6);
        let trampoline = state
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 37,
                inode: 41,
            })
            .unwrap();
        state.owned_descriptors.insert(
            9,
            AfterLoaderOwnedDescriptor::Trampoline {
                id: Some(trampoline),
                size: Some(TRAMPOLINE_ARENA_SIZE),
            },
        );
        assert_eq!(
            state.trampoline_close_shape(9, trampoline, TRAMPOLINE_ARENA_SIZE),
            Some(TrampolineCloseShape::Abandoned),
            "an arena whose first mmap failed owns no mapping"
        );

        let identity = MappingIdentity {
            device_major: 0,
            device_minor: 1,
            inode: 41,
        };
        assert!(state.bind_trampoline_mapping(trampoline, identity));
        let writable = (0x10_0000, 0x10_0000 + TRAMPOLINE_ARENA_SIZE);
        let executable = (0x20_0000, 0x20_0000 + TRAMPOLINE_ARENA_SIZE);
        for (range, writable, executable) in [(writable, true, false), (executable, false, true)] {
            state.owned_mappings.push(AfterLoaderOwnedMapping {
                start: range.0,
                end: range.1,
                readable: true,
                writable,
                executable,
                shared: true,
                descriptor: Some(9),
                offset: 0,
                purpose: AfterLoaderMappingPurpose::Trampoline { trampoline },
            });
        }
        assert_eq!(
            state.trampoline_close_shape(9, trampoline, TRAMPOLINE_ARENA_SIZE),
            None,
            "aliases without their reservation are a partial arena"
        );

        state.remove_owned_range(writable);
        assert_eq!(
            state.trampoline_close_shape(9, trampoline, TRAMPOLINE_ARENA_SIZE),
            None,
            "one surviving alias prevents abandoned cleanup"
        );
        state.remove_owned_range(executable);
        assert_eq!(state.trampoline_mapping(trampoline), Some(identity));
        assert_eq!(
            state.trampoline_close_shape(9, trampoline, TRAMPOLINE_ARENA_SIZE),
            Some(TrampolineCloseShape::Abandoned)
        );
    }

    #[test]
    fn shared_reservation_binds_one_fully_aliased_trampoline() {
        let mut state = mapping_state(7);
        let trampoline = state
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 41,
                inode: 43,
            })
            .unwrap();
        let identity = MappingIdentity {
            device_major: 0,
            device_minor: 9,
            inode: 43,
        };
        assert!(state.bind_trampoline_mapping(trampoline, identity));
        state.owned_descriptors.insert(
            11,
            AfterLoaderOwnedDescriptor::Trampoline {
                id: Some(trampoline),
                size: Some(TRAMPOLINE_ARENA_SIZE),
            },
        );
        assert_eq!(state.trampoline_awaiting_shared_reservation(), None);

        for (start, writable, executable) in [(0x10_0000, true, false), (0x20_0000, false, true)] {
            state.owned_mappings.push(AfterLoaderOwnedMapping {
                start,
                end: start + TRAMPOLINE_ARENA_SIZE,
                readable: true,
                writable,
                executable,
                shared: true,
                descriptor: Some(11),
                offset: 0,
                purpose: AfterLoaderMappingPurpose::Trampoline { trampoline },
            });
            if writable {
                assert_eq!(state.trampoline_awaiting_shared_reservation(), None);
            }
        }
        assert_eq!(
            state.trampoline_awaiting_shared_reservation(),
            Some(trampoline)
        );

        let mut ambiguous = state.clone();
        let other = ambiguous
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 47,
                inode: 53,
            })
            .unwrap();
        assert!(ambiguous.bind_trampoline_mapping(
            other,
            MappingIdentity {
                inode: 53,
                ..identity
            }
        ));
        ambiguous.owned_descriptors.insert(
            12,
            AfterLoaderOwnedDescriptor::Trampoline {
                id: Some(other),
                size: Some(TRAMPOLINE_ARENA_SIZE),
            },
        );
        for (start, writable, executable) in [(0x40_0000, true, false), (0x50_0000, false, true)] {
            ambiguous.owned_mappings.push(AfterLoaderOwnedMapping {
                start,
                end: start + TRAMPOLINE_ARENA_SIZE,
                readable: true,
                writable,
                executable,
                shared: true,
                descriptor: Some(12),
                offset: 0,
                purpose: AfterLoaderMappingPurpose::Trampoline { trampoline: other },
            });
        }
        assert_eq!(ambiguous.trampoline_awaiting_shared_reservation(), None);

        let reservation = (0x30_0000, 0x30_0000 + PAGE);
        let reservation_binding = (
            reservation,
            SharedReservationIdentity {
                mapping: MappingIdentity {
                    device_major: 0,
                    device_minor: 1,
                    inode: 59,
                },
                path: Some(PathBuf::from("/dev/zero (deleted)")),
            },
        );
        assert_eq!(
            state
                .shared_reservations
                .insert(trampoline, reservation_binding.clone()),
            None
        );
        state.owned_mappings.push(AfterLoaderOwnedMapping {
            start: reservation.0,
            end: reservation.1,
            readable: true,
            writable: true,
            executable: false,
            shared: true,
            descriptor: None,
            offset: 0,
            purpose: AfterLoaderMappingPurpose::SharedReservation { trampoline },
        });
        assert_eq!(state.trampoline_awaiting_shared_reservation(), None);
        assert_eq!(
            state.shared_reservations.get(&trampoline),
            Some(&reservation_binding)
        );
        assert_eq!(
            state.trampoline_close_shape(11, trampoline, TRAMPOLINE_ARENA_SIZE),
            Some(TrampolineCloseShape::Complete)
        );

        let mut partial_alias = state.clone();
        partial_alias.owned_mappings.retain(|mapping| {
            mapping.purpose != AfterLoaderMappingPurpose::Trampoline { trampoline }
                || !mapping.writable
        });
        assert_eq!(
            partial_alias.trampoline_close_shape(11, trampoline, TRAMPOLINE_ARENA_SIZE),
            None
        );

        let mut partial_reservation = state.clone();
        partial_reservation.owned_mappings.retain(|mapping| {
            mapping.purpose != AfterLoaderMappingPurpose::Trampoline { trampoline }
        });
        assert_eq!(
            partial_reservation.trampoline_close_shape(11, trampoline, TRAMPOLINE_ARENA_SIZE),
            None
        );

        let mut abandoned = partial_reservation;
        abandoned.owned_mappings.retain(|mapping| {
            mapping.purpose != AfterLoaderMappingPurpose::SharedReservation { trampoline }
        });
        assert_eq!(
            abandoned.trampoline_close_shape(11, trampoline, TRAMPOLINE_ARENA_SIZE),
            None,
            "a live reservation binding cannot be abandoned"
        );
        assert_eq!(
            abandoned.shared_reservations.remove(&trampoline),
            Some(reservation_binding)
        );
        assert_eq!(
            abandoned.trampoline_close_shape(11, trampoline, TRAMPOLINE_ARENA_SIZE),
            Some(TrampolineCloseShape::Abandoned)
        );
        assert_eq!(
            abandoned.trampoline_mappings.remove(&trampoline),
            Some(identity)
        );
        let next = abandoned
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 61,
                inode: 67,
            })
            .unwrap();
        assert!(abandoned.bind_trampoline_mapping(
            next,
            MappingIdentity {
                device_major: 0,
                device_minor: 11,
                inode: 67,
            }
        ));
    }

    #[test]
    fn mapping_identity_match_checks_major_minor_and_inode() {
        let map = GuestMap {
            start: 0x1000,
            end: 0x2000,
            offset: 0,
            device_major: 0,
            device_minor: 0x20,
            readable: true,
            writable: false,
            executable: true,
            shared: false,
            inode: 77,
            path: Some(PathBuf::from("/exact-image")),
        };
        let identity = map.mapping_identity();
        assert!(mapping_identity_matches(identity, &map));
        for changed in [
            MappingIdentity {
                device_major: 1,
                ..identity
            },
            MappingIdentity {
                device_minor: 0x21,
                ..identity
            },
            MappingIdentity {
                inode: 78,
                ..identity
            },
        ] {
            assert!(!mapping_identity_matches(changed, &map));
        }
    }

    #[test]
    fn geometry_byte_comparison_has_no_entry_guard_or_neighbor_exemption() {
        let original = [0xf3, 0x0f, 0x1e, 0xfa, 0x31, 0xed, 0x49, 0x89];
        let mut guarded = original;
        guarded[0] = 0xcc;
        assert!(exact_geometry_bytes_match(&original, &original));
        assert!(!exact_geometry_bytes_match(&guarded, &original));
        for index in 0..original.len() {
            let mut changed = original;
            changed[index] ^= 1;
            assert!(
                !exact_geometry_bytes_match(&changed, &original),
                "changed byte {index} was accepted"
            );
        }
        let mut surrounding = [0_u8; 18];
        surrounding[5..13].copy_from_slice(&original);
        assert!(exact_geometry_bytes_match(&surrounding, &surrounding));
        for index in [4, 13] {
            let mut changed = surrounding;
            changed[index] = 1;
            assert!(
                !exact_geometry_bytes_match(&changed, &surrounding),
                "neighbor byte {index} was ignored"
            );
        }
    }

    #[test]
    fn shared_reservation_requires_exact_linux_dev_zero_identity() {
        let make = |device_major, device_minor, inode, path: Option<&str>| GuestMap {
            start: 0x1000,
            end: 0x2000,
            offset: 0,
            device_major,
            device_minor,
            readable: true,
            writable: true,
            executable: false,
            shared: true,
            inode,
            path: path.map(PathBuf::from),
        };
        let captured = make(0, 1, 78, Some("/dev/zero (deleted)"));
        let identity = exact_shared_reservation_identity(&captured).unwrap();
        assert_eq!(identity.mapping, captured.mapping_identity());
        assert_eq!(identity.path.as_ref(), captured.path.as_ref());

        for changed in [
            make(1, 1, 78, Some("/dev/zero (deleted)")),
            make(0, 2, 78, Some("/dev/zero (deleted)")),
            make(0, 1, 0, Some("/dev/zero (deleted)")),
            make(0, 1, 78, None),
            make(0, 1, 78, Some("[anon_shmem:kernel-name]")),
        ] {
            assert!(
                exact_shared_reservation_identity(&changed).is_none(),
                "accepted noncanonical shared reservation identity {changed:?}"
            );
        }

        let changed_inode =
            exact_shared_reservation_identity(&make(0, 1, 79, Some("/dev/zero (deleted)")))
                .unwrap();
        assert_ne!(identity, changed_inode);
    }

    #[test]
    fn dlopen_graph_excludes_the_unsealed_staging_runtime() {
        let source = ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .unwrap();
        let mut provider = LiteinstCallerImage::read(source).unwrap();
        provider.path = PathBuf::from("/bound/provider.so");
        let mut dependency = provider.clone();
        dependency.path = PathBuf::from("/bound/dependency.so");
        let mut unsealed_runtime = provider.clone();
        unsealed_runtime.path = PathBuf::from("/staging/unsealed-runtime.so");
        let dependencies = [dependency];

        assert_eq!(
            dlopen_graph_image_for_path(
                &provider,
                &dependencies,
                provider.path.as_os_str().as_encoded_bytes(),
            )
            .map(|image| image.path.as_path()),
            Some(provider.path.as_path())
        );
        assert_eq!(
            dlopen_graph_image_for_path(
                &provider,
                &dependencies,
                dependencies[0].path.as_os_str().as_encoded_bytes(),
            )
            .map(|image| image.path.as_path()),
            Some(dependencies[0].path.as_path())
        );
        assert!(
            dlopen_graph_image_for_path(
                &provider,
                &dependencies,
                unsealed_runtime.path.as_os_str().as_encoded_bytes(),
            )
            .is_none(),
            "unsealed staging runtime entered the private dlopen graph"
        );
    }

    #[test]
    fn exact_open_file_mapping_identity_is_observed_stably_from_proc_maps() {
        let path = std::env::current_exe().unwrap();
        let file = std::fs::File::open(path).unwrap();
        let before = stable_backing_stamp(&file.metadata().unwrap());
        let first = mapping_identity_for_open_file(&file).unwrap();
        let second = mapping_identity_for_open_file(&file).unwrap();
        assert_eq!(first, second);
        assert_ne!(first.inode, 0);
        assert_eq!(before, stable_backing_stamp(&file.metadata().unwrap()));
    }

    fn map_test_image(
        file: &std::fs::File,
        image: &LiteinstCallerImage,
    ) -> (*mut libc::c_void, ResolvedImageGeometry) {
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE as usize,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        assert_ne!(address, libc::MAP_FAILED);
        let start = address as u64;
        let end = start + PAGE;
        let maps = guest_maps(Pid::from_raw(std::process::id() as i32)).unwrap();
        let mapping = maps
            .iter()
            .find(|mapping| {
                mapping.start <= start
                    && end <= mapping.end
                    && mapping.offset.checked_add(start - mapping.start) == Some(0)
                    && mapping.inode != 0
            })
            .unwrap();
        (
            address,
            ResolvedImageGeometry {
                image: AfterLoaderImageId {
                    generation: 1,
                    file: image.file_identity,
                },
                mapping: mapping.mapping_identity(),
                load_bias: start,
                span: (start, end),
            },
        )
    }

    #[test]
    fn exact_image_backing_guard_accepts_only_the_bound_file_and_full_bytes() {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let source = ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "liteinst-mapping-identity-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));
        std::fs::create_dir(&directory).unwrap();
        let first_path = directory.join("first-image");
        let second_path = directory.join("second-image");
        std::fs::copy(source, &first_path).unwrap();
        std::fs::copy(source, &second_path).unwrap();

        let first_image = LiteinstCallerImage::read(&first_path).unwrap();
        let first_file = std::fs::File::open(&first_path).unwrap();
        let (first_mapping, first_geometry) = map_test_image(&first_file, &first_image);
        let pid = Pid::from_raw(std::process::id() as i32);
        let maps = guest_maps(pid).unwrap();
        assert_eq!(
            target_bound_image_mapping_identity(pid, &first_image).unwrap(),
            Some(first_geometry.mapping),
            "target-root open did not bridge to the exact maps-domain identity"
        );
        assert!(
            exact_image_backing_paths_match(pid, &first_image, first_geometry, &maps,).unwrap()
        );

        let mut wrong_mapping = first_geometry;
        wrong_mapping.mapping.device_minor ^= 1;
        assert!(
            !exact_image_backing_paths_match(pid, &first_image, wrong_mapping, &maps,).unwrap()
        );

        let original_last = *first_image.bytes.last().unwrap();
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&first_path)
            .unwrap();
        writer
            .write_all_at(&[original_last ^ 1], first_image.bytes.len() as u64 - 1)
            .unwrap();
        assert!(
            !exact_image_backing_paths_match(pid, &first_image, first_geometry, &maps,).unwrap()
        );
        assert_eq!(unsafe { libc::munmap(first_mapping, PAGE as usize) }, 0);

        let second_image = LiteinstCallerImage::read(&second_path).unwrap();
        let second_file = std::fs::File::open(&second_path).unwrap();
        let (second_mapping, second_geometry) = map_test_image(&second_file, &second_image);
        let second_maps = guest_maps(pid).unwrap();
        assert!(
            exact_image_backing_paths_match(pid, &second_image, second_geometry, &second_maps,)
                .unwrap()
        );
        assert!(
            !exact_image_backing_paths_match(pid, &first_image, second_geometry, &second_maps,)
                .unwrap(),
            "different inode with identical bytes was accepted"
        );

        std::fs::remove_file(&second_path).unwrap();
        let deleted_maps = guest_maps(pid).unwrap();
        assert!(
            !matches!(
                exact_image_backing_paths_match(pid, &second_image, second_geometry, &deleted_maps,),
                Ok(true)
            ),
            "deleted target backing path was accepted"
        );
        assert_eq!(unsafe { libc::munmap(second_mapping, PAGE as usize) }, 0);

        std::fs::remove_file(&first_path).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn private_wait_diagnostics_are_structured_and_do_not_expand_event_owners() {
        assert_eq!(after_loader_event_summary(&Event::Seccomp), "Seccomp");
        assert_eq!(after_loader_event_summary(&Event::Syscall), "Syscall");
        assert_eq!(
            after_loader_event_summary(&Event::Signal(Signal::SIGTRAP)),
            "Signal(SIGTRAP)"
        );
        assert_eq!(
            after_loader_wait_summary(&Wait::Exited(Pid::from_raw(17), ExitStatus::Exited(3),)),
            "Exited(tid=17 status=Exited(3))"
        );
        let observer = safeptrace::PhysicalEventObserver::new(
            safeptrace::PhysicalEventObserverConfig::default(),
        )
        .expect("create default-capacity physical observer");
        let child = Running::new(Pid::from_raw(19));
        child
            .attach_physical_event_observer(&observer)
            .expect("attach observer to synthetic child generation");
        let child_summary = after_loader_event_summary(&Event::NewChild(ChildOp::Fork, child));
        assert!(child_summary.starts_with("NewChild(operation=Fork tid=19 generation="));
        assert!(child_summary.len() < 128, "{child_summary}");
        assert!(!child_summary.contains("PhysicalEventObserver"));
        let stopped = Stopped::new_unchecked(Pid::from_raw(29));
        stopped
            .attach_physical_event_observer(&observer)
            .expect("attach observer to synthetic stopped generation");
        let stopped_summary = after_loader_wait_summary(&Wait::Stopped(stopped, Event::Seccomp));
        assert!(stopped_summary.starts_with("Stopped(tid=29 generation="));
        assert!(stopped_summary.contains(" physical_status=None event=Seccomp)"));
        assert!(stopped_summary.len() < 192, "{stopped_summary}");
        for recursive in [
            "EventHandle",
            "TraceeToken",
            "PhysicalEventObserver",
            "RecordBuffer",
        ] {
            assert!(!stopped_summary.contains(recursive), "{stopped_summary}");
        }
        assert_eq!(
            after_loader_event_summary(&Event::Exec(Pid::from_raw(23))),
            "Exec(previous_tid=23)"
        );
    }

    #[test]
    fn zero_fill_verification_reads_prot_none_without_changing_permissions() {
        let length = 2 * PAGE as usize;
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        unsafe {
            *((mapping as *mut u8).add(length - 1)) = 1;
        }
        let start = mapping as u64;

        let child = match unsafe { nix::unistd::fork() }.expect("fork PROT_NONE target") {
            nix::unistd::ForkResult::Child => {
                assert_eq!(
                    unsafe { libc::mprotect(mapping, length, libc::PROT_NONE) },
                    0
                );
                safeptrace::traceme_and_stop().expect("stop PROT_NONE target under ptrace");
                unsafe { libc::_exit(0) };
            }
            nix::unistd::ForkResult::Parent { child } => child,
        };
        // Make the controller's private mapping the exact opposite of the
        // stopped child's after the fork. Reading /proc/self/mem by mistake
        // must therefore fail both content assertions below.
        unsafe {
            *(mapping as *mut u8) = 1;
            *((mapping as *mut u8).add(length - 1)) = 0;
        }
        assert_eq!(
            unsafe { libc::mprotect(mapping, length, libc::PROT_NONE) },
            0
        );
        let (stopped, _) = Running::new(child.into())
            .wait()
            .expect("wait for PROT_NONE target")
            .assume_stopped();
        let assert_child_prot_none = || {
            let maps = guest_maps(child.into()).expect("read stopped child mappings");
            let map = maps
                .iter()
                .find(|map| map.start <= start && start + length as u64 <= map.end)
                .expect("find complete stopped child test mapping");
            assert!(
                !map.readable && !map.writable && !map.executable && !map.shared,
                "stopped child test mapping permissions changed: {map:?}"
            );
        };
        assert_child_prot_none();

        let controller_memory = std::fs::File::open("/proc/self/mem")
            .expect("open controller memory for negative control");
        let mut controller_first = [0];
        let mut controller_tail = [0];
        controller_memory
            .read_exact_at(&mut controller_first, start)
            .expect("read controller first test page");
        controller_memory
            .read_exact_at(&mut controller_tail, start + length as u64 - 1)
            .expect("read controller rounded-tail byte");
        assert_eq!((controller_first, controller_tail), ([1], [0]));

        assert!(
            target_range_is_zero(&stopped, start, PAGE)
                .expect("read zero PROT_NONE page through stopped task")
        );
        assert_child_prot_none();
        assert!(
            target_range_is_zero(&stopped, start, PAGE + 1)
                .expect("raw unrounded prefix misses the nonzero rounded tail")
        );
        let (_, rounded_end) = checked_page_effect_range(start, PAGE + 1).unwrap();
        assert!(
            !target_range_is_zero(&stopped, start, rounded_end - start)
                .expect("read the full rounded PROT_NONE effect through stopped task")
        );
        assert_child_prot_none();
        let exited = stopped
            .resume(None)
            .expect("resume PROT_NONE target")
            .wait()
            .expect("wait for PROT_NONE target exit");
        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
        assert_eq!(unsafe { libc::munmap(mapping, length) }, 0);
    }

    #[test]
    fn helper_isolation_mixed_read_spans_exact_prot_none_page() {
        let page_size = PAGE as usize;
        let length = 3 * page_size;
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        let mapping = mapping.cast::<u8>();
        let expected = (0..length)
            .map(|index| {
                (index as u8)
                    .wrapping_mul(37)
                    .wrapping_add((index / page_size) as u8 * 71)
            })
            .collect::<Vec<_>>();
        unsafe {
            core::ptr::copy_nonoverlapping(expected.as_ptr(), mapping, length);
        }
        let start = mapping as u64;
        let helper_start = start + PAGE;
        let helper_end = helper_start + PAGE;

        let child = match unsafe { nix::unistd::fork() }.expect("fork mixed helper target") {
            nix::unistd::ForkResult::Child => {
                if unsafe {
                    libc::mprotect(
                        helper_start as *mut libc::c_void,
                        page_size,
                        libc::PROT_NONE,
                    )
                } != 0
                {
                    unsafe { libc::_exit(2) };
                }
                safeptrace::traceme_and_stop().expect("stop mixed helper target under ptrace");
                unsafe { libc::_exit(0) };
            }
            nix::unistd::ForkResult::Parent { child } => child,
        };
        let (stopped, _) = Running::new(child.into())
            .wait()
            .expect("wait for mixed helper target")
            .assume_stopped();

        // Make the controller mapping bytewise different after fork. An
        // accidental local read can no longer satisfy the complete comparison.
        unsafe {
            core::ptr::write_bytes(mapping, 0xa5, length);
        }
        let child_maps = guest_maps(child.into()).expect("read mixed helper target mappings");
        let helper_mapping = child_maps
            .iter()
            .find(|mapping| mapping.start == helper_start && mapping.end == helper_end)
            .cloned()
            .expect("find exact isolated helper page");
        let isolation = AfterLoaderHelperIsolation {
            image: AfterLoaderImageId {
                generation: 1,
                file: crate::after_loader::FileIdentity {
                    device: 1,
                    inode: 1,
                },
            },
            range: GuestRange {
                start: helper_start,
                end: helper_end,
            },
            original_mapping: GuestMap {
                start,
                end: start + length as u64,
                readable: true,
                writable: false,
                executable: true,
                shared: false,
                ..helper_mapping.clone()
            },
            mapping: helper_mapping.mapping_identity(),
            load_bias: start,
            file_offset: helper_mapping.offset,
            expected_bytes: expected[page_size..2 * page_size].to_vec(),
        };

        let live_page_is_exact = isolation.validates_live_page(&stopped);
        let mut observed = vec![0_u8; length];
        let mixed_result =
            read_exact_with_helper_isolation(&stopped, start, &mut observed, Some(&isolation));
        let mut direct = vec![0_u8; length];
        let direct_result = read_exact_with_helper_isolation(&stopped, start, &mut direct, None);

        let mut wrong_first = isolation.clone();
        wrong_first.expected_bytes[0] ^= 1;
        let mut wrong_first_output = vec![0_u8; length];
        let wrong_first_result = read_exact_with_helper_isolation(
            &stopped,
            start,
            &mut wrong_first_output,
            Some(&wrong_first),
        );

        let mut wrong_last = isolation.clone();
        *wrong_last.expected_bytes.last_mut().unwrap() ^= 1;
        let mut wrong_last_output = vec![0_u8; length];
        let wrong_last_result = read_exact_with_helper_isolation(
            &stopped,
            start,
            &mut wrong_last_output,
            Some(&wrong_last),
        );
        let helper_mapping_after = guest_maps(child.into())
            .and_then(|maps| {
                maps.into_iter()
                    .find(|mapping| mapping.start == helper_start && mapping.end == helper_end)
            })
            .expect("isolated helper page remained mapped");

        let exited = stopped
            .resume(None)
            .expect("resume mixed helper target")
            .wait()
            .expect("wait for mixed helper target exit");
        assert_eq!(unsafe { libc::munmap(mapping.cast(), length) }, 0);

        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
        assert!(
            live_page_is_exact,
            "isolated helper page failed exact validation"
        );
        assert_eq!(mixed_result, Ok(()));
        assert_eq!(
            observed, expected,
            "mixed read skipped, duplicated, or changed bytes at a page boundary"
        );
        assert!(
            direct_result.is_err(),
            "ordinary user-access read unexpectedly crossed PROT_NONE"
        );
        assert_eq!(
            wrong_first_result,
            Err(safeptrace::Error::Errno(Errno::EPROTO)),
            "first expected helper-byte mismatch was accepted"
        );
        assert_eq!(
            wrong_last_result,
            Err(safeptrace::Error::Errno(Errno::EPROTO)),
            "last expected helper-byte mismatch was accepted"
        );
        assert!(
            !helper_mapping_after.readable
                && !helper_mapping_after.writable
                && !helper_mapping_after.executable
                && !helper_mapping_after.shared,
            "mixed reads changed helper-page protection: {helper_mapping_after:?}"
        );
    }

    #[test]
    fn unsupported_cet_status_completion_reaches_the_final_caller_gate() {
        let before = [0x5a; 8];
        let effect = AfterLoaderSyscallEffect::CetStatus {
            destination: 0x4000,
            before,
        };
        let completion = complete_cet_status_query(&effect, -(libc::EINVAL as i64), before)
            .expect("CET effect has a completion")
            .expect("unchanged EINVAL is the supported CET absence result");
        assert_eq!(
            completion,
            AfterLoaderSyscallCompletion::UnsupportedCetStatus
        );
        assert!(caller_private_syscall_result_is_accepted(completion));
        for index in 0..before.len() {
            let mut changed = before;
            changed[index] ^= 1;
            assert_eq!(
                complete_cet_status_query(&effect, -(libc::EINVAL as i64), changed),
                Some(Err(CetStatusCompletionError::OutputChanged)),
                "changed byte {index} was accepted"
            );
        }
        assert_eq!(
            complete_cet_status_query(&effect, -(libc::EPERM as i64), before),
            Some(Err(CetStatusCompletionError::SyscallFailed(
                -(libc::EPERM as i64)
            )))
        );
        assert_eq!(
            complete_cet_status_query(
                &AfterLoaderSyscallEffect::None,
                -(libc::EINVAL as i64),
                before
            ),
            None
        );
        assert_eq!(
            complete_cet_status_query(&effect, 1, [0; 8]),
            Some(Err(CetStatusCompletionError::NonzeroSuccess(1)))
        );
        let successful = complete_cet_status_query(&effect, 0, [0; 8])
            .expect("CET effect has a completion")
            .expect("zero is the exact successful CET result");
        assert_eq!(successful, AfterLoaderSyscallCompletion::KernelResult(0));
        assert!(caller_private_syscall_result_is_accepted(successful));
        assert!(!caller_private_syscall_result_is_accepted(
            AfterLoaderSyscallCompletion::KernelResult(-(libc::EINVAL as i64)),
        ));
        assert!(!caller_private_syscall_result_is_accepted(
            AfterLoaderSyscallCompletion::KernelResult(-4095),
        ));
        assert!(caller_private_syscall_result_is_accepted(
            AfterLoaderSyscallCompletion::KernelResult(-4096),
        ));
    }

    #[test]
    fn private_entropy_signal_timer_exec_and_clone_requests_are_refused() {
        for syscall in [
            libc::SYS_getrandom,
            libc::SYS_rt_sigaction,
            libc::SYS_rt_sigprocmask,
            libc::SYS_clone,
            libc::SYS_clone3,
            libc::SYS_execve,
            libc::SYS_execveat,
            libc::SYS_timer_create,
            libc::SYS_clock_gettime,
            libc::SYS_madvise,
            libc::SYS_ptrace,
        ] {
            assert!(!private_syscall_allowed(syscall), "syscall {syscall}");
        }
        assert!(private_syscall_allowed(libc::SYS_mmap));
        assert!(private_syscall_allowed(libc::SYS_close));
    }

    #[test]
    fn alternate_stack_compares_all_abi_fields_and_preserves_raw_padding_evidence() {
        let before = [0_u8; 24];
        for index in (0..12).chain(16..24) {
            let mut after = before;
            after[index] = 1;
            assert_ne!(
                altstack_fields(&before),
                altstack_fields(&after),
                "field byte {index}"
            );
        }
        let mut padding = before;
        padding[12..16].fill(0xff);
        assert_eq!(altstack_fields(&before), altstack_fields(&padding));
        assert_ne!(before, padding);
    }

    #[test]
    fn complete_environment_rejects_duplicate_keys_and_truncation() {
        let actual = environment_map(b"A=one\0B=two=three\0").unwrap();
        assert_eq!(
            actual.get(&OsString::from("B")),
            Some(&OsString::from("two=three"))
        );
        assert!(environment_map(b"A=one\0A=two\0").is_err());
        assert!(environment_map(b"A=one").is_err());
        assert!(environment_map(b"=empty-key\0").is_err());
    }
}
