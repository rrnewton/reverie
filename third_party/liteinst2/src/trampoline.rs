//! Relocation-aware hook trampolines for Linux on x86-64.
//!
//! A trampoline is emitted from one checked plan. It saves the System V AMD64
//! integer context, flags, red zone, and floating-point/SIMD state through AVX-512,
//! calls a hook, restores the context, executes the relocated instruction,
//! and jumps back without clobbering a register. Executable storage is mapped
//! within signed rel32 reach and is never writable through its RX alias.

use core::fmt;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::AtomicU8;
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;
use core::sync::atomic::compiler_fence;

use iced_x86::BlockEncoder;
use iced_x86::BlockEncoderOptions;
use iced_x86::BlockEncoderResult;
use iced_x86::Decoder;
use iced_x86::DecoderOptions;
use iced_x86::Instruction;
use iced_x86::InstructionBlock;
use iced_x86::Mnemonic;
use iced_x86::OpKind;
use iced_x86::code_asm::CodeAssembler;
use iced_x86::code_asm::eax;
use iced_x86::code_asm::edx;
use iced_x86::code_asm::qword_ptr;
use iced_x86::code_asm::r8;
use iced_x86::code_asm::r9;
use iced_x86::code_asm::r10;
use iced_x86::code_asm::r11;
use iced_x86::code_asm::r12;
use iced_x86::code_asm::r13;
use iced_x86::code_asm::r14;
use iced_x86::code_asm::r15;
use iced_x86::code_asm::rax;
use iced_x86::code_asm::rbp;
use iced_x86::code_asm::rbx;
use iced_x86::code_asm::rcx;
use iced_x86::code_asm::rdi;
use iced_x86::code_asm::rdx;
use iced_x86::code_asm::rsi;
use iced_x86::code_asm::rsp;

use crate::patcher::JumpPatchPlan;
use crate::patcher::LiveJumpPatch;
use crate::patcher::PatchError;
use crate::patcher::StalenessBudget;
use crate::scanner::InstructionScanner;
use crate::scanner::ScanResult;

const SYSTEM_V_RED_ZONE_BYTES: i32 = 128;
const SAVED_INTEGER_BYTES: i32 = 16 * 8;
const HOOK_METADATA_BYTES: i32 = 2 * 8;
const FXSAVE_BYTES: i32 = 512;
const FXSAVE_ALIGNMENT: i32 = 16;
const NEAR_RETURN_JUMP_BYTES: usize = 5;
const NOTRACK_ABSOLUTE_JUMP_BYTES: usize = 15;
const PTRACE_STOP_BYTES: [u8; 1] = [0xcc];
const TRAMPOLINE_ALLOCATION_BYTES: usize = 4096;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MAX_PROCESS_MAPS_BYTES: usize = 2 * 1024 * 1024;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MIN_PROCESS_MAP_RECORD_BYTES: usize = 40;

const STATE_INACTIVE: u8 = 0;
const STATE_ACTIVE: u8 = 1;
const STATE_TRANSITIONING: u8 = 2;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
enum ExtendedState {
    FxSave,
    XSave { bytes: i32, mask: u64 },
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl ExtendedState {
    #[allow(unused_unsafe)]
    fn detect() -> Self {
        use core::arch::x86_64::__cpuid;
        use core::arch::x86_64::__cpuid_count;
        use core::arch::x86_64::_xgetbv;

        // SAFETY: CPUID is available in x86-64 mode.
        let features = unsafe { __cpuid(1) };
        let has_xsave = features.ecx & (1 << 26) != 0;
        let has_osxsave = features.ecx & (1 << 27) != 0;
        if has_xsave && has_osxsave {
            // SAFETY: OSXSAVE proves XGETBV is enabled for XCR0.
            // Linux can expose AMX in XCR0 while denying this thread tile-state
            // permission through XFD. Preserve the universally usable user
            // components through PKRU and leave AMX to its explicit owner.
            const USER_STATE_MASK: u64 = 0b10_1110_0111;
            let mask = unsafe { _xgetbv(0) } & USER_STATE_MASK;
            // SAFETY: CPUID leaf D is available when XSAVE is present.
            let state = unsafe { __cpuid_count(0xD, 0) };
            if mask != 0 {
                let rounded = state.ebx.checked_add(63).map(|bytes| bytes & !63);
                if let Some(bytes) = rounded.and_then(|bytes| i32::try_from(bytes).ok()) {
                    if bytes >= 576 {
                        return Self::XSave { bytes, mask };
                    }
                }
            }
        }
        Self::FxSave
    }

    fn encode_save(self, assembler: &mut CodeAssembler) -> Result<(), TrampolineError> {
        let (alignment, bytes) = match self {
            Self::FxSave => (FXSAVE_ALIGNMENT, FXSAVE_BYTES),
            Self::XSave { bytes, .. } => (64, bytes),
        };
        assembler.and(rsp, -alignment).map_err(encoding_error)?;
        assembler.sub(rsp, bytes).map_err(encoding_error)?;
        match self {
            Self::FxSave => assembler.fxsave64(rsp.into()).map_err(encoding_error),
            Self::XSave { mask, .. } => {
                // XSAVE does not initialize every reserved header byte, while
                // XRSTOR requires them to be zero or raises #GP.
                assembler.xor(eax, eax).map_err(encoding_error)?;
                for offset in (512..576).step_by(8) {
                    assembler
                        .mov(qword_ptr(rsp + offset), rax)
                        .map_err(encoding_error)?;
                }
                assembler.mov(eax, mask as u32).map_err(encoding_error)?;
                assembler
                    .mov(edx, (mask >> 32) as u32)
                    .map_err(encoding_error)?;
                assembler.xsave64(rsp.into()).map_err(encoding_error)
            }
        }
    }

    fn encode_restore(self, assembler: &mut CodeAssembler) -> Result<(), TrampolineError> {
        match self {
            Self::FxSave => assembler.fxrstor64(rsp.into()).map_err(encoding_error),
            Self::XSave { mask, .. } => {
                assembler.mov(eax, mask as u32).map_err(encoding_error)?;
                assembler
                    .mov(edx, (mask >> 32) as u32)
                    .map_err(encoding_error)?;
                assembler.xrstor64(rsp.into()).map_err(encoding_error)
            }
        }
    }
}

/// Callback invoked before the displaced application instruction.
///
/// The context remains valid only for the callback. The callback must follow
/// the System V AMD64 ABI and must not unwind through generated machine code.
/// The frame preserves x87 through AVX-512 plus PKRU; callbacks must not modify
/// permission-gated AMX or other extended state outside that contract.
pub type HookCallback = unsafe extern "C" fn(*mut HookContext);

/// Mutable integer-register snapshot at the instrumented instruction.
///
/// Callback changes to saved GPRs and RFLAGS are restored into application
/// state. The instruction and stack-pointer fields are metadata. SIMD state is
/// preserved transparently rather than exposed to callbacks.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HookContext {
    /// Address of the displaced instruction.
    pub instruction_pointer: u64,
    /// Application RSP at the displaced instruction.
    pub stack_pointer: u64,
    /// Saved R15.
    pub r15: u64,
    /// Saved R14.
    pub r14: u64,
    /// Saved R13.
    pub r13: u64,
    /// Saved R12.
    pub r12: u64,
    /// Saved R11.
    pub r11: u64,
    /// Saved R10.
    pub r10: u64,
    /// Saved R9.
    pub r9: u64,
    /// Saved R8.
    pub r8: u64,
    /// Saved RDI.
    pub rdi: u64,
    /// Saved RSI.
    pub rsi: u64,
    /// Saved RBP.
    pub rbp: u64,
    /// Saved RBX.
    pub rbx: u64,
    /// Saved RDX.
    pub rdx: u64,
    /// Saved RCX.
    pub rcx: u64,
    /// Saved RAX.
    pub rax: u64,
    /// Saved RFLAGS.
    pub rflags: u64,
}

/// Checked byte lengths for the sections of one trampoline.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TrampolineLayout {
    /// Optional INT3 reached with the untouched application register state.
    pub entry_stop_len: usize,
    /// Context-save and instrumentation-call bytes.
    pub instrumentation_len: usize,
    /// Optional INT3 reached after the complete application-state restore.
    pub completion_stop_len: usize,
    /// Relocated instructions, including any jump over encoder padding and literals.
    pub relocated_len: usize,
    /// Context-restore bytes.
    pub restore_len: usize,
    /// Control-transfer bytes returning to application code.
    pub return_len: usize,
}

impl TrampolineLayout {
    /// Returns the total allocation size, or None if the sum overflows.
    pub fn total_len(self) -> Option<usize> {
        self.entry_stop_len
            .checked_add(self.instrumentation_len)?
            .checked_add(self.restore_len)?
            .checked_add(self.completion_stop_len)?
            .checked_add(self.relocated_len)?
            .checked_add(self.return_len)
    }
}

/// Complete emitted bytes and section boundaries for a trampoline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrampolineImage {
    bytes: Vec<u8>,
    layout: TrampolineLayout,
    program_counters: Vec<ProgramCounterMapping>,
}

impl TrampolineImage {
    /// Returns the emitted machine code.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the checked section lengths.
    pub const fn layout(&self) -> TrampolineLayout {
        self.layout
    }

    /// Returns executable ranges and their corresponding application PCs.
    /// Encoder padding and literal data have no application PC mapping.
    pub fn program_counter_mappings(&self) -> &[ProgramCounterMapping] {
        &self.program_counters
    }
}

/// One half-open generated-code range and its logical application PC.
///
/// A signal handler or unwind front end can translate a PC in this range before
/// reporting or unwinding it. The mapping does not itself install a signal
/// handler or provide call-frame information.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProgramCounterMapping {
    generated_start: u64,
    generated_end: u64,
    logical_address: u64,
}

impl ProgramCounterMapping {
    /// Returns the first generated PC in this mapping.
    pub const fn generated_start(&self) -> u64 {
        self.generated_start
    }

    /// Returns the first generated PC after this mapping.
    pub const fn generated_end(&self) -> u64 {
        self.generated_end
    }

    /// Returns the application PC represented by this generated range.
    pub const fn logical_address(&self) -> u64 {
        self.logical_address
    }

    /// Translates a generated PC covered by this mapping.
    pub const fn translate(&self, program_counter: u64) -> Option<u64> {
        if self.generated_start <= program_counter && program_counter < self.generated_end {
            Some(self.logical_address)
        } else {
            None
        }
    }
}

struct ProgramCounterMapNode {
    mappings: Box<[ProgramCounterMapping]>,
    next: *mut ProgramCounterMapNode,
}

static PROGRAM_COUNTER_MAP_HEAD: AtomicPtr<ProgramCounterMapNode> =
    AtomicPtr::new(core::ptr::null_mut());

/// Translates a PC in a published trampoline to its application instruction.
///
/// Published mappings live for the process lifetime. This lookup performs only
/// atomic loads and immutable reads, so it can be called from a signal handler.
/// Returning `None` means the PC is not in a published executable range;
/// trampoline padding and literal data are not executable ranges.
pub fn translate_program_counter(program_counter: u64) -> Option<u64> {
    let mut node = PROGRAM_COUNTER_MAP_HEAD.load(Ordering::Acquire);
    while !node.is_null() {
        // SAFETY: nodes and their mapping arrays are leaked after publication.
        let published = unsafe { &*node };
        for mapping in &published.mappings {
            if let Some(logical_address) = mapping.translate(program_counter) {
                return Some(logical_address);
            }
        }
        node = published.next;
    }
    None
}

fn register_program_counter_mappings(mappings: &[ProgramCounterMapping]) {
    let node = Box::into_raw(Box::new(ProgramCounterMapNode {
        mappings: mappings.to_vec().into_boxed_slice(),
        next: core::ptr::null_mut(),
    }));
    let mut head = PROGRAM_COUNTER_MAP_HEAD.load(Ordering::Acquire);
    loop {
        // SAFETY: node is private to this publisher until the release CAS.
        unsafe { (*node).next = head };
        match PROGRAM_COUNTER_MAP_HEAD.compare_exchange_weak(
            head,
            node,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(observed) => head = observed,
        }
    }
}

/// Errors produced while planning, emitting, allocating, or toggling a hook.
#[derive(Debug)]
pub enum TrampolineError {
    /// The requested address is not an instruction head in the scan.
    SiteNotFound {
        /// Requested executable address.
        address: u64,
    },
    /// The direct jump cannot displace this instruction by itself.
    InstructionTooShort {
        /// Instruction address.
        address: u64,
        /// Decoded instruction length.
        instruction_len: usize,
    },
    /// Computing the address after the displaced instruction overflowed.
    ReturnAddressOverflow {
        /// Instruction address.
        address: u64,
        /// Decoded instruction length.
        instruction_len: usize,
    },
    /// iced-x86 could not encode generated or relocated code.
    Encoding {
        /// Encoder diagnostic.
        message: String,
    },
    /// An address cannot be represented by this process.
    AddressNotRepresentable {
        /// Address that failed conversion.
        address: u64,
    },
    /// The operating-system page size is unavailable or unsupported.
    InvalidPageSize,
    /// Creating or publishing executable backing storage failed.
    BackingStore {
        /// Failed operation.
        operation: &'static str,
        /// Operating-system error number.
        errno: i32,
    },
    /// Reading or parsing the process memory map failed.
    ProcessMaps {
        /// Diagnostic for the unavailable or malformed map.
        message: String,
    },
    /// No free page could be mapped within signed rel32 reach.
    NoReachableMapping,
    /// The exact instruction-pun destination is already occupied or unavailable.
    ExactAddressUnavailable {
        /// Required trampoline entry address.
        address: u64,
    },
    /// A trampoline arena must contain at least one checked slot.
    InvalidArenaCapacity,
    /// Every slot in a prepared trampoline arena has been reserved.
    ArenaFull,
    /// Emitted code does not fit in its executable allocation.
    CodeTooLarge {
        /// Emitted code bytes.
        code_len: usize,
        /// Available allocation bytes.
        allocation_len: usize,
    },
    /// A hook toggle raced another toggle on the same site.
    TransitionInProgress,
    /// M2 rejected patch planning, binding, or publication.
    Patch(PatchError),
    /// Trampolines are unavailable on this target.
    UnsupportedPlatform,
}

impl fmt::Display for TrampolineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SiteNotFound { address } => {
                write!(formatter, "no scanned instruction starts at {address:#x}")
            }
            Self::InstructionTooShort {
                address,
                instruction_len,
            } => write!(
                formatter,
                "instruction at {address:#x} is {instruction_len} bytes; direct redirection needs at least five"
            ),
            Self::ReturnAddressOverflow {
                address,
                instruction_len,
            } => write!(
                formatter,
                "instruction at {address:#x} with length {instruction_len} has no return address"
            ),
            Self::Encoding { message } => {
                write!(formatter, "machine-code encoding failed: {message}")
            }
            Self::AddressNotRepresentable { address } => {
                write!(formatter, "address {address:#x} is not representable")
            }
            Self::InvalidPageSize => formatter.write_str("invalid operating-system page size"),
            Self::BackingStore { operation, errno } => {
                write!(formatter, "{operation} failed with errno {errno}")
            }
            Self::ProcessMaps { message } => {
                write!(formatter, "cannot find a near mapping: {message}")
            }
            Self::NoReachableMapping => {
                formatter.write_str("no free executable page is within signed rel32 reach")
            }
            Self::ExactAddressUnavailable { address } => {
                write!(
                    formatter,
                    "exact trampoline address {address:#x} is unavailable"
                )
            }
            Self::InvalidArenaCapacity => {
                formatter.write_str("trampoline arena capacity must be non-zero")
            }
            Self::ArenaFull => formatter.write_str("trampoline arena has no free slots"),
            Self::CodeTooLarge {
                code_len,
                allocation_len,
            } => write!(
                formatter,
                "{code_len}-byte trampoline exceeds {allocation_len}-byte allocation"
            ),
            Self::TransitionInProgress => {
                formatter.write_str("another thread is toggling this hook")
            }
            Self::Patch(source) => write!(formatter, "patch operation failed: {source}"),
            Self::UnsupportedPlatform => formatter.write_str("trampolines require Linux on x86-64"),
        }
    }
}

impl std::error::Error for TrampolineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Patch(source) => Some(source),
            _ => None,
        }
    }
}

impl From<PatchError> for TrampolineError {
    fn from(source: PatchError) -> Self {
        Self::Patch(source)
    }
}

/// Immutable relocation and dispatch plan for a complete patch window.
#[derive(Clone, Debug)]
pub struct TrampolinePlan {
    execute_address: u64,
    return_address: u64,
    hook: HookCallback,
    instructions: Vec<Instruction>,
    relocated_start: usize,
    ptrace_stops: bool,
}

impl TrampolinePlan {
    /// Builds an observing plan from an instruction scan.
    ///
    /// Complete consecutive instructions are displaced until their combined
    /// length can contain a five-byte near jump. The hook runs before all
    /// displaced instructions are relocated and executed.
    pub fn from_scan(
        scan: &ScanResult,
        execute_address: u64,
        hook: HookCallback,
    ) -> Result<Self, TrampolineError> {
        Self::from_scan_mode(scan, execute_address, hook, 0, false)
    }

    /// Builds a plan whose hook replaces the first displaced instruction.
    ///
    /// Instructions after the first one are still relocated and executed.
    /// This supports short operations such as the two-byte x86-64 syscall:
    /// the hook emulates the operation, the trampoline executes any tail
    /// instructions consumed by the five-byte patch, and control resumes after
    /// the complete displaced window.
    pub fn from_scan_replacing_first(
        scan: &ScanResult,
        execute_address: u64,
        hook: HookCallback,
    ) -> Result<Self, TrampolineError> {
        Self::from_scan_mode(scan, execute_address, hook, 1, false)
    }

    /// Builds a replace-first plan bracketed by two tracer-owned INT3 stops.
    ///
    /// The entry stop executes before any instrumentation code with the
    /// application register file untouched. The completion stop executes only
    /// after the saved integer, flags, stack, and extended state have been
    /// restored, immediately before the relocated tail. This mode requires an
    /// external ptrace controller that authenticates and suppresses both
    /// breakpoints; executing it without that controller is unsupported.
    pub fn from_scan_replacing_first_with_ptrace_stops(
        scan: &ScanResult,
        execute_address: u64,
        hook: HookCallback,
    ) -> Result<Self, TrampolineError> {
        Self::from_scan_mode(scan, execute_address, hook, 1, true)
    }

    fn from_scan_mode(
        scan: &ScanResult,
        execute_address: u64,
        hook: HookCallback,
        relocated_start: usize,
        ptrace_stops: bool,
    ) -> Result<Self, TrampolineError> {
        let start_index = scan
            .instructions()
            .iter()
            .position(|instruction| instruction.address() == execute_address)
            .ok_or(TrampolineError::SiteNotFound {
                address: execute_address,
            })?;
        let mut displaced_len = 0_usize;
        let mut instructions = Vec::new();
        for scanned in &scan.instructions()[start_index..] {
            let expected = execute_address.checked_add(displaced_len as u64).ok_or(
                TrampolineError::ReturnAddressOverflow {
                    address: execute_address,
                    instruction_len: displaced_len,
                },
            )?;
            if scanned.address() != expected {
                break;
            }
            displaced_len = displaced_len.checked_add(scanned.len()).ok_or(
                TrampolineError::ReturnAddressOverflow {
                    address: execute_address,
                    instruction_len: displaced_len,
                },
            )?;
            instructions.push(scanned.instruction().to_owned());
            if displaced_len >= crate::patcher::NEAR_JUMP_BYTES {
                break;
            }
        }
        if displaced_len < crate::patcher::NEAR_JUMP_BYTES {
            return Err(TrampolineError::InstructionTooShort {
                address: execute_address,
                instruction_len: displaced_len,
            });
        }
        let return_address = execute_address.checked_add(displaced_len as u64).ok_or(
            TrampolineError::ReturnAddressOverflow {
                address: execute_address,
                instruction_len: displaced_len,
            },
        )?;
        Ok(Self {
            execute_address,
            return_address,
            hook,
            instructions,
            relocated_start,
            ptrace_stops,
        })
    }

    /// Returns the first displaced instruction address.
    pub const fn execute_address(&self) -> u64 {
        self.execute_address
    }

    /// Returns the first application address after the displaced window.
    pub const fn return_address(&self) -> u64 {
        self.return_address
    }

    /// Returns the number of complete application bytes displaced.
    pub fn displaced_len(&self) -> usize {
        self.instructions.iter().map(Instruction::len).sum()
    }

    /// Returns whether the first displaced instruction is replaced by the hook.
    pub const fn replaces_first(&self) -> bool {
        self.relocated_start == 1
    }

    /// Emits this plan at an exact executable address.
    pub fn emit_at(&self, address: u64) -> Result<TrampolineImage, TrampolineError> {
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            let _ = address;
            return Err(TrampolineError::UnsupportedPlatform);
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let extended_state = ExtendedState::detect();
            let entry_stop = self
                .ptrace_stops
                .then_some(PTRACE_STOP_BYTES.as_slice())
                .unwrap_or_default();
            let instrumentation_address = address
                .checked_add(entry_stop.len() as u64)
                .ok_or(TrampolineError::AddressNotRepresentable { address })?;
            let instrumentation =
                self.encode_instrumentation(instrumentation_address, extended_state)?;
            let restore_address = instrumentation_address
                .checked_add(instrumentation.len() as u64)
                .ok_or(TrampolineError::AddressNotRepresentable { address })?;
            let restore = encode_restore(restore_address, extended_state)?;
            let completion_stop_address = restore_address
                .checked_add(restore.len() as u64)
                .ok_or(TrampolineError::AddressNotRepresentable { address })?;
            let completion_stop = self
                .ptrace_stops
                .then_some(PTRACE_STOP_BYTES.as_slice())
                .unwrap_or_default();
            let relocated_address = completion_stop_address
                .checked_add(completion_stop.len() as u64)
                .ok_or(TrampolineError::AddressNotRepresentable { address })?;
            let relocated_instructions = &self.instructions[self.relocated_start..];
            let (encoded, terminal_jump_offset) =
                encode_relocated_block(relocated_instructions, relocated_address)?;
            let relocated = encoded.code_buffer;
            let return_jump_address = relocated_address
                .checked_add(relocated.len() as u64)
                .ok_or(TrampolineError::AddressNotRepresentable { address })?;
            let return_jump = encode_return_jump(return_jump_address, self.return_address)?;
            let layout = TrampolineLayout {
                entry_stop_len: entry_stop.len(),
                instrumentation_len: instrumentation.len(),
                restore_len: restore.len(),
                completion_stop_len: completion_stop.len(),
                relocated_len: relocated.len(),
                return_len: return_jump.len(),
            };
            let total_len = layout.total_len().ok_or(TrampolineError::CodeTooLarge {
                code_len: usize::MAX,
                allocation_len: TRAMPOLINE_ALLOCATION_BYTES,
            })?;
            let mut bytes = Vec::with_capacity(total_len);
            bytes.extend_from_slice(entry_stop);
            bytes.extend_from_slice(&instrumentation);
            bytes.extend_from_slice(&restore);
            bytes.extend_from_slice(completion_stop);
            bytes.extend_from_slice(&relocated);
            bytes.extend_from_slice(&return_jump);
            debug_assert_eq!(bytes.len(), total_len);

            let mut program_counters = Vec::new();
            push_program_counter_mapping(
                &mut program_counters,
                address,
                relocated_address,
                self.execute_address,
            );
            append_relocated_program_counter_mappings(
                &mut program_counters,
                relocated_address,
                &relocated[..terminal_jump_offset.unwrap_or(relocated.len())],
                relocated_instructions,
                &encoded.new_instruction_offsets,
            )?;
            if let Some(offset) = terminal_jump_offset {
                // The terminal transfer represents the continuation. Padding
                // and pointers after it are data, not displaced instructions.
                push_program_counter_mapping(
                    &mut program_counters,
                    relocated_address + offset as u64,
                    relocated_address + offset as u64 + NEAR_RETURN_JUMP_BYTES as u64,
                    self.return_address,
                );
            }
            let return_code_len = if return_jump.len() == NOTRACK_ABSOLUTE_JUMP_BYTES {
                return_jump.len() - core::mem::size_of::<u64>()
            } else {
                return_jump.len()
            };
            let return_jump_end = return_jump_address
                .checked_add(return_jump.len() as u64)
                .ok_or(TrampolineError::AddressNotRepresentable {
                    address: return_jump_address,
                })?;
            push_program_counter_mapping(
                &mut program_counters,
                return_jump_address,
                return_jump_end - (return_jump.len() - return_code_len) as u64,
                self.return_address,
            );
            Ok(TrampolineImage {
                bytes,
                layout,
                program_counters,
            })
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn encode_instrumentation(
        &self,
        address: u64,
        extended_state: ExtendedState,
    ) -> Result<Vec<u8>, TrampolineError> {
        let mut assembler = CodeAssembler::new(64).map_err(encoding_error)?;
        assembler
            .lea(rsp, rsp - SYSTEM_V_RED_ZONE_BYTES)
            .map_err(encoding_error)?;
        assembler.pushfq().map_err(encoding_error)?;
        for register in [
            rax, rcx, rdx, rbx, rbp, rsi, rdi, r8, r9, r10, r11, r12, r13, r14, r15,
        ] {
            assembler.push(register).map_err(encoding_error)?;
        }
        assembler
            .lea(rax, rsp + SYSTEM_V_RED_ZONE_BYTES + SAVED_INTEGER_BYTES)
            .map_err(encoding_error)?;
        assembler.push(rax).map_err(encoding_error)?;
        assembler
            .mov(rax, self.execute_address)
            .map_err(encoding_error)?;
        assembler.push(rax).map_err(encoding_error)?;
        assembler.mov(r12, rsp).map_err(encoding_error)?;
        extended_state.encode_save(&mut assembler)?;
        assembler.cld().map_err(encoding_error)?;
        assembler.mov(rdi, r12).map_err(encoding_error)?;
        assembler
            .mov(rax, self.hook as usize as u64)
            .map_err(encoding_error)?;
        assembler.call(rax).map_err(encoding_error)?;
        assembler.assemble(address).map_err(encoding_error)
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn encode_restore(address: u64, extended_state: ExtendedState) -> Result<Vec<u8>, TrampolineError> {
    let mut assembler = CodeAssembler::new(64).map_err(encoding_error)?;
    extended_state.encode_restore(&mut assembler)?;
    assembler.mov(rsp, r12).map_err(encoding_error)?;
    assembler
        .add(rsp, HOOK_METADATA_BYTES)
        .map_err(encoding_error)?;
    for register in [
        r15, r14, r13, r12, r11, r10, r9, r8, rdi, rsi, rbp, rbx, rdx, rcx, rax,
    ] {
        assembler.pop(register).map_err(encoding_error)?;
    }
    assembler.popfq().map_err(encoding_error)?;
    assembler
        .lea(rsp, rsp + SYSTEM_V_RED_ZONE_BYTES)
        .map_err(encoding_error)?;
    assembler.assemble(address).map_err(encoding_error)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn encoding_error(error: impl fmt::Display) -> TrampolineError {
    TrampolineError::Encoding {
        message: error.to_string(),
    }
}

fn encode_return_jump(address: u64, target: u64) -> Result<Vec<u8>, TrampolineError> {
    let next_ip = address
        .checked_add(NEAR_RETURN_JUMP_BYTES as u64)
        .ok_or(TrampolineError::AddressNotRepresentable { address })?;
    if let Ok(displacement) = i32::try_from(i128::from(target) - i128::from(next_ip)) {
        let mut bytes = Vec::with_capacity(NEAR_RETURN_JUMP_BYTES);
        bytes.push(0xE9);
        bytes.extend_from_slice(&displacement.to_le_bytes());
        return Ok(bytes);
    }

    // The DS prefix is CET's NOTRACK override. It makes the rare indirect
    // fallback independent of an ENDBR64 at the application continuation.
    let mut bytes = Vec::with_capacity(NOTRACK_ABSOLUTE_JUMP_BYTES);
    bytes.extend_from_slice(&[0x3E, 0xFF, 0x25, 0, 0, 0, 0]);
    bytes.extend_from_slice(&target.to_le_bytes());
    Ok(bytes)
}

fn push_program_counter_mapping(
    mappings: &mut Vec<ProgramCounterMapping>,
    generated_start: u64,
    generated_end: u64,
    logical_address: u64,
) {
    if generated_start >= generated_end {
        return;
    }
    match mappings.last_mut() {
        Some(previous)
            if previous.generated_end == generated_start
                && previous.logical_address == logical_address =>
        {
            previous.generated_end = generated_end;
            return;
        }
        _ => {}
    }
    mappings.push(ProgramCounterMapping {
        generated_start,
        generated_end,
        logical_address,
    });
}

// A block encoder appends aligned pointer data after its instructions. A far
// conditional branch's fallthrough, or a far CALL's return, must not enter that
// data. Reserve a fixed-width terminal E9 *inside* the encoded block, then patch
// its displacement to the existing return stub after the complete buffer.
// No bytes are inserted after encoding: all RIP-relative fixups retain their
// encoder-computed locations. Blocks without pointer data keep their layout.
fn encode_relocated_block(
    instructions: &[Instruction],
    address: u64,
) -> Result<(BlockEncoderResult, Option<usize>), TrampolineError> {
    let options = BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS
        | BlockEncoderOptions::RETURN_RELOC_INFOS;
    let mut encoded =
        BlockEncoder::encode(64, InstructionBlock::new(instructions, address), options)
            .map_err(encoding_error)?;
    if encoded.reloc_infos.is_empty() {
        return Ok((encoded, None));
    }

    const TERMINAL_JUMP: [u8; NEAR_RETURN_JUMP_BYTES] = [0xE9, 0, 0, 0, 0];
    let mut terminal = Instruction::with_declare_byte(&TERMINAL_JUMP).map_err(encoding_error)?;
    // Give this encoder directive an unused original IP. Otherwise the encoder
    // could redirect an application branch or RIP-relative operand to it.
    // Each displaced instruction excludes at most three values, so this search
    // ends after at most three times the number of instructions plus one.
    let mut terminal_ip = 0_u64;
    while instructions.iter().any(|instruction| {
        instruction.ip() == terminal_ip
            || instruction.near_branch_target() == terminal_ip
            || (instruction.is_ip_rel_memory_operand()
                && instruction.ip_rel_memory_address() == terminal_ip)
    }) {
        terminal_ip = terminal_ip
            .checked_add(1)
            .ok_or_else(|| encoding_error("no unused terminal instruction IP"))?;
    }
    terminal.set_ip(terminal_ip);
    let mut terminated = instructions.to_vec();
    terminated.push(terminal);
    encoded = BlockEncoder::encode(64, InstructionBlock::new(&terminated, address), options)
        .map_err(encoding_error)?;
    // DeclareByte is a fixed-size encoder directive, not an optimizable branch.
    // Its returned offset is the executable/data boundary, without decoding any
    // pointer bytes (which can themselves look like valid instructions).
    let offset = encoded
        .new_instruction_offsets
        .pop()
        .filter(|offset| *offset != u32::MAX)
        .ok_or_else(|| encoding_error("missing terminal jump offset"))? as usize;
    let end = offset
        .checked_add(TERMINAL_JUMP.len())
        .ok_or_else(|| encoding_error("terminal jump offset overflow"))?;
    if encoded.code_buffer.get(offset..end) != Some(TERMINAL_JUMP.as_slice()) {
        return Err(encoding_error(
            "terminal jump reservation was not preserved",
        ));
    }
    address
        .checked_add(encoded.code_buffer.len() as u64)
        .ok_or(TrampolineError::AddressNotRepresentable { address })?;
    if encoded
        .reloc_infos
        .iter()
        .any(|info| info.address < address + end as u64)
    {
        return Err(encoding_error(
            "encoder literal overlaps executable instructions",
        ));
    }
    let data_len = encoded
        .code_buffer
        .len()
        .checked_sub(end)
        .ok_or_else(|| encoding_error("terminal jump exceeds encoded block"))?;
    let displacement = i32::try_from(data_len)
        .map_err(|_| encoding_error("encoder literal table exceeds signed rel32 reach"))?;
    encoded.code_buffer[offset + 1..end].copy_from_slice(&displacement.to_le_bytes());
    Ok((encoded, Some(offset)))
}

fn append_relocated_program_counter_mappings(
    mappings: &mut Vec<ProgramCounterMapping>,
    relocated_address: u64,
    code: &[u8],
    instructions: &[Instruction],
    offsets: &[u32],
) -> Result<(), TrampolineError> {
    debug_assert_eq!(instructions.len(), offsets.len());
    let mut cursor = 0_usize;
    let mut index = 0_usize;
    while index < instructions.len() {
        let offset = offsets[index];
        if offset == u32::MAX {
            let instruction_address = relocated_address + cursor as u64;
            let separately_encoded = BlockEncoder::encode(
                64,
                InstructionBlock::new(&instructions[index..=index], instruction_address),
                BlockEncoderOptions::RETURN_RELOC_INFOS,
            )
            .map_err(encoding_error)?;
            let data_start = separately_encoded
                .reloc_infos
                .iter()
                .map(|relocation| relocation.address.saturating_sub(instruction_address))
                .min()
                .and_then(|len| usize::try_from(len).ok())
                .unwrap_or(separately_encoded.code_buffer.len());
            let mut decoder = Decoder::with_ip(
                64,
                &separately_encoded.code_buffer[..data_start],
                instruction_address,
                DecoderOptions::NONE,
            );
            let mut emitted_len = 0_usize;
            while decoder.can_decode() {
                let generated = decoder.decode();
                emitted_len += generated.len();
                if matches!(generated.mnemonic(), Mnemonic::Jmp | Mnemonic::Call)
                    && generated.op0_kind() == OpKind::Memory
                {
                    break;
                }
            }
            if emitted_len == 0 {
                emitted_len = data_start;
            }
            let instruction_end = cursor.saturating_add(emitted_len).min(code.len());
            push_program_counter_mapping(
                mappings,
                instruction_address,
                relocated_address + instruction_end as u64,
                instructions[index].ip(),
            );
            cursor = instruction_end;
            index += 1;
            continue;
        }

        let instruction_start = offset as usize;
        if cursor < instruction_start {
            let logical_address = instructions[index.saturating_sub(1)].ip();
            push_program_counter_mapping(
                mappings,
                relocated_address + cursor as u64,
                relocated_address + instruction_start as u64,
                logical_address,
            );
        }
        let mut decoder = Decoder::with_ip(
            64,
            &code[instruction_start..],
            relocated_address + instruction_start as u64,
            DecoderOptions::NONE,
        );
        let emitted_len = decoder.decode().len();
        let instruction_end = instruction_start
            .saturating_add(emitted_len)
            .min(code.len());
        push_program_counter_mapping(
            mappings,
            relocated_address + instruction_start as u64,
            relocated_address + instruction_end as u64,
            instructions[index].ip(),
        );
        cursor = instruction_end;
        index += 1;
    }
    if cursor < code.len() {
        let logical_address = instructions
            .last()
            .map_or(relocated_address, Instruction::ip);
        push_program_counter_mapping(
            mappings,
            relocated_address + cursor as u64,
            relocated_address + code.len() as u64,
            logical_address,
        );
    }
    Ok(())
}

/// Process-lifetime dual-mapped trampoline storage prepared before instrumentation.
///
/// An arena keeps separate RW and RX aliases to the same backing pages and
/// reserves fixed-size slots without issuing syscalls. This avoids an RWX VMA,
/// but it is not strict W^X because executable bytes remain writable through
/// the separate RW alias. Clients that need a stronger write-after-publish
/// boundary must not use this arena API.
///
/// Fork-related processes share the reservation cursor and the configured slot
/// budget. A separate non-executable shared metadata page prevents their code
/// publications from reusing the same slot after a copy-on-write fork. That
/// page, like the code aliases, remains mapped for the process lifetime.
pub struct TrampolineArena {
    writable: *mut u8,
    executable: *mut u8,
    len: usize,
    next: *const AtomicUsize,
    reservation_len: usize,
}

// SAFETY: the initialized cursor is process-shared, slots are reserved atomically
// and never reused, and all three mappings live for the process lifetime.
unsafe impl Send for TrampolineArena {}
// SAFETY: see Send; writers receive disjoint slots before publishing bytes.
unsafe impl Sync for TrampolineArena {}

impl TrampolineArena {
    /// Allocates process-lifetime trampoline slots near an address.
    pub fn allocate_near(address: u64, slots: usize) -> Result<Self, TrampolineError> {
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            let _ = (address, slots);
            return Err(TrampolineError::UnsupportedPlatform);
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            if slots == 0 {
                return Err(TrampolineError::InvalidArenaCapacity);
            }
            let len = slots
                .checked_mul(TRAMPOLINE_ALLOCATION_BYTES)
                .ok_or(TrampolineError::InvalidArenaCapacity)?;
            PendingMapping::allocate_near_len(address, len)?.into_arena()
        }
    }

    /// Returns whether every slot in this arena is reachable from an address.
    pub fn can_reach(&self, address: u64) -> bool {
        let Some(next_ip) = address.checked_add(crate::patcher::NEAR_JUMP_BYTES as u64) else {
            return false;
        };
        let start = self.executable as usize as u64;
        let Some(end) = start.checked_add(self.len.saturating_sub(1) as u64) else {
            return false;
        };
        i32::try_from(i128::from(start) - i128::from(next_ip)).is_ok()
            && i32::try_from(i128::from(end) - i128::from(next_ip)).is_ok()
    }

    /// Returns the process-lifetime RW code alias.
    pub fn writable_range(&self) -> core::ops::Range<usize> {
        let start = self.writable as usize;
        start..start + self.len
    }

    /// Returns the process-lifetime RX code alias.
    pub fn executable_range(&self) -> core::ops::Range<usize> {
        let start = self.executable as usize;
        start..start + self.len
    }

    /// Returns the process-shared reservation-cursor page.
    pub fn reservation_range(&self) -> core::ops::Range<usize> {
        let start = self.next as usize;
        start..start + self.reservation_len
    }

    /// Emits one checked plan into a freshly reserved arena slot.
    pub fn allocate(&self, plan: &TrampolinePlan) -> Result<ExecutableTrampoline, TrampolineError> {
        if !self.can_reach(plan.execute_address()) {
            return Err(TrampolineError::NoReachableMapping);
        }
        // SAFETY: successful construction initialized this shared atomic and
        // transferred its mapping to process-lifetime ownership.
        let offset = reserve_arena_slot(unsafe { &*self.next }, self.len)?;
        let address = (self.executable as usize).checked_add(offset).ok_or(
            TrampolineError::AddressNotRepresentable {
                address: self.executable as usize as u64,
            },
        )? as u64;
        let image = plan.emit_at(address)?;
        if image.bytes.len() > TRAMPOLINE_ALLOCATION_BYTES {
            return Err(TrampolineError::CodeTooLarge {
                code_len: image.bytes.len(),
                allocation_len: TRAMPOLINE_ALLOCATION_BYTES,
            });
        }
        // SAFETY: this thread exclusively owns the reserved slot.
        unsafe {
            core::ptr::copy_nonoverlapping(
                image.bytes.as_ptr(),
                self.writable.add(offset),
                image.bytes.len(),
            );
        }
        compiler_fence(Ordering::SeqCst);
        Ok(ExecutableTrampoline {
            address,
            allocation_len: TRAMPOLINE_ALLOCATION_BYTES,
            mapping_address: 0,
            code_len: image.bytes.len(),
            layout: image.layout,
            program_counters: image.program_counters.into_boxed_slice(),
            program_counters_published: AtomicBool::new(false),
        })
    }
}

fn reserve_arena_slot(next: &AtomicUsize, len: usize) -> Result<usize, TrampolineError> {
    // This reserves exclusive storage; it does not publish completed code.
    // Saturation prevents failed reservations from wrapping and reusing bytes.
    next.try_update(Ordering::AcqRel, Ordering::Acquire, |offset| {
        offset
            .checked_add(TRAMPOLINE_ALLOCATION_BYTES)
            .filter(|end| *end <= len)
    })
    .map_err(|_| TrampolineError::ArenaFull)
}

/// Process-lifetime executable trampoline mapping.
///
/// The writable alias is removed before construction returns. The RX page is
/// intentionally not unmapped on drop because another thread may have fetched
/// its address before concurrent deactivation.
pub struct ExecutableTrampoline {
    address: u64,
    allocation_len: usize,
    mapping_address: u64,
    code_len: usize,
    layout: TrampolineLayout,
    program_counters: Box<[ProgramCounterMapping]>,
    program_counters_published: AtomicBool,
}

impl ExecutableTrampoline {
    /// Allocates a reachable page, emits the plan, and publishes an RX mapping.
    pub fn allocate(plan: &TrampolinePlan) -> Result<Self, TrampolineError> {
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            let _ = plan;
            return Err(TrampolineError::UnsupportedPlatform);
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let mapping = PendingMapping::allocate_near(plan.execute_address)?;
            let address = mapping.executable as usize as u64;
            let image = plan.emit_at(address)?;
            if image.bytes.len() > mapping.len {
                return Err(TrampolineError::CodeTooLarge {
                    code_len: image.bytes.len(),
                    allocation_len: mapping.len,
                });
            }
            mapping.publish_at(&image, 0)
        }
    }

    /// Allocates the trampoline at one exact instruction-pun destination.
    ///
    /// The complete emitted image is backed by a fresh RX mapping claimed with
    /// MAP_FIXED_NOREPLACE. Existing mappings are never replaced.
    pub fn allocate_at(plan: &TrampolinePlan, address: u64) -> Result<Self, TrampolineError> {
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            let _ = (plan, address);
            return Err(TrampolineError::UnsupportedPlatform);
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let image = plan.emit_at(address)?;
            let page_size = page_size()?;
            let native_address = usize::try_from(address)
                .map_err(|_| TrampolineError::AddressNotRepresentable { address })?;
            let mapping_start = align_down(native_address, page_size);
            let entry_offset = native_address - mapping_start;
            let required = entry_offset
                .checked_add(image.bytes.len())
                .ok_or(TrampolineError::AddressNotRepresentable { address })?;
            let mapping_len = required
                .checked_add(page_size - 1)
                .map(|value| align_down(value, page_size))
                .ok_or(TrampolineError::AddressNotRepresentable { address })?;
            let mapping = PendingMapping::allocate_exact(mapping_start, mapping_len, address)?;
            mapping.publish_at(&image, entry_offset)
        }
    }

    /// Returns the executable entry address.
    pub const fn address(&self) -> u64 {
        self.address
    }

    /// Returns the executable mapping size.
    pub const fn allocation_len(&self) -> usize {
        self.allocation_len
    }

    /// Returns the number of initialized machine-code bytes.
    pub const fn code_len(&self) -> usize {
        self.code_len
    }

    /// Returns the RIP reported after the optional entry INT3.
    ///
    /// `None` identifies an ordinary trampoline with no tracer-owned stops.
    pub const fn ptrace_entry_stop_rip(&self) -> Option<u64> {
        if self.layout.entry_stop_len == PTRACE_STOP_BYTES.len() {
            Some(self.address + PTRACE_STOP_BYTES.len() as u64)
        } else {
            None
        }
    }

    /// Returns the RIP reported after the optional post-restore INT3.
    ///
    /// This is also the first byte of the relocated tail. At this stop the
    /// trampoline has restored the complete application register and extended
    /// state, but no relocated application instruction has executed yet.
    pub const fn ptrace_completion_stop_rip(&self) -> Option<u64> {
        if self.layout.completion_stop_len == PTRACE_STOP_BYTES.len() {
            Some(
                self.address
                    + self.layout.entry_stop_len as u64
                    + self.layout.instrumentation_len as u64
                    + self.layout.restore_len as u64
                    + PTRACE_STOP_BYTES.len() as u64,
            )
        } else {
            None
        }
    }

    /// Returns the entry that executes only relocated tail instructions.
    ///
    /// A signal handler that already emulated a replace-first instruction can
    /// resume here after publishing the hook. The interrupted register context
    /// must already contain the emulated result.
    pub const fn relocated_tail_address(&self) -> u64 {
        self.address
            + self.layout.entry_stop_len as u64
            + self.layout.instrumentation_len as u64
            + self.layout.restore_len as u64
            + self.layout.completion_stop_len as u64
    }

    /// Returns the emitted section lengths.
    pub const fn layout(&self) -> TrampolineLayout {
        self.layout
    }

    /// Returns generated ranges and their corresponding application PCs.
    pub fn program_counter_mappings(&self) -> &[ProgramCounterMapping] {
        &self.program_counters
    }

    /// Translates a PC within this trampoline to an application instruction.
    pub fn translate_program_counter(&self, program_counter: u64) -> Option<u64> {
        self.program_counters
            .iter()
            .find_map(|mapping| mapping.translate(program_counter))
    }

    /// Publishes this trampoline to the process-wide reverse-PC lookup.
    ///
    /// Callers that actually consume the global lookup may opt in after making
    /// the trampoline reachable. Publication is idempotent and process-lifetime;
    /// ordinary installed hooks do not incur this allocation or leak.
    pub fn publish_program_counter_mappings(&self) {
        if !self.program_counters_published.swap(true, Ordering::AcqRel) {
            register_program_counter_mappings(&self.program_counters);
        }
    }

    pub(crate) fn discard(self) {
        debug_assert!(!self.program_counters_published.load(Ordering::Relaxed));
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            if self.mapping_address != 0 {
                // SAFETY: this unpublished object exclusively owns the RX mapping.
                unsafe {
                    libc::munmap(
                        self.mapping_address as usize as *mut libc::c_void,
                        self.allocation_len,
                    );
                }
            }
        }
    }
}

/// Scanner and mapping inputs for installing one hook.
pub struct HookSite<'a> {
    scanner: &'a InstructionScanner,
    scan: &'a ScanResult,
    code: &'a [u8],
    region_base: u64,
    execute_address: u64,
    writable_address: *mut u8,
}

impl<'a> HookSite<'a> {
    /// Describes an executable instruction and its writable alias.
    pub const fn new(
        scanner: &'a InstructionScanner,
        scan: &'a ScanResult,
        code: &'a [u8],
        region_base: u64,
        execute_address: u64,
        writable_address: *mut u8,
    ) -> Self {
        Self {
            scanner,
            scan,
            code,
            region_base,
            execute_address,
            writable_address,
        }
    }
}
/// A generated trampoline bound to the M2 live patcher.
pub struct InstalledHook {
    trampoline: ExecutableTrampoline,
    patch: LiveJumpPatch,
    state: AtomicU8,
}

impl InstalledHook {
    /// Generates and binds an initially inactive observing hook.
    ///
    /// # Safety
    ///
    /// The writable alias in site must satisfy LiveJumpPatch::bind and remain
    /// valid for the process lifetime. Its code and scan must describe the
    /// executable region. The callback must remain callable for the process
    /// lifetime and must not unwind across its C ABI boundary. The caller must
    /// also prove that no direct or indirect control-flow entry can target the
    /// interior of the displaced instruction window.
    pub unsafe fn install(
        site: HookSite<'_>,
        hook: HookCallback,
        staleness: StalenessBudget,
    ) -> Result<Self, TrampolineError> {
        let plan = TrampolinePlan::from_scan(site.scan, site.execute_address, hook)?;
        let trampoline = ExecutableTrampoline::allocate(&plan)?;
        // SAFETY: forwarded from this method's mapping-lifetime contract.
        unsafe { Self::bind(site, plan, trampoline, Some(staleness)) }
    }

    /// Generates a hook that replaces the first displaced instruction.
    ///
    /// # Safety
    ///
    /// The same requirements as install apply, including excluding every
    /// interior entry into the displaced window. The callback must implement
    /// the skipped instruction's effects in the mutable HookContext.
    pub unsafe fn install_replacing_first(
        site: HookSite<'_>,
        hook: HookCallback,
        staleness: StalenessBudget,
    ) -> Result<Self, TrampolineError> {
        let plan =
            TrampolinePlan::from_scan_replacing_first(site.scan, site.execute_address, hook)?;
        let trampoline = ExecutableTrampoline::allocate(&plan)?;
        // SAFETY: forwarded from this method's mapping-lifetime contract.
        unsafe { Self::bind(site, plan, trampoline, Some(staleness)) }
    }

    /// Generates a quiescent hook that replaces the first displaced instruction.
    ///
    /// The returned hook must be toggled only with
    /// [`Self::activate_quiescent`] and [`Self::deactivate_quiescent`].
    ///
    /// # Safety
    ///
    /// The same mapping and callback requirements as
    /// [`Self::install_replacing_first`] apply. Each quiescent toggle additionally
    /// requires its documented no-other-thread proof.
    pub unsafe fn install_replacing_first_quiescent(
        site: HookSite<'_>,
        hook: HookCallback,
    ) -> Result<Self, TrampolineError> {
        let plan =
            TrampolinePlan::from_scan_replacing_first(site.scan, site.execute_address, hook)?;
        let trampoline = ExecutableTrampoline::allocate(&plan)?;
        // SAFETY: forwarded from this method's mapping-lifetime contract.
        unsafe { Self::bind(site, plan, trampoline, None) }
    }

    /// Generates an observing hook in preallocated trampoline storage.
    ///
    /// # Safety
    ///
    /// The same requirements as install apply. The arena must remain alive
    /// for the process lifetime.
    pub unsafe fn install_in_arena(
        site: HookSite<'_>,
        hook: HookCallback,
        staleness: StalenessBudget,
        arena: &TrampolineArena,
    ) -> Result<Self, TrampolineError> {
        let plan = TrampolinePlan::from_scan(site.scan, site.execute_address, hook)?;
        let trampoline = arena.allocate(&plan)?;
        // SAFETY: forwarded from this method's mapping-lifetime contract.
        unsafe { Self::bind(site, plan, trampoline, Some(staleness)) }
    }

    /// Generates a replace-first hook in preallocated trampoline storage.
    ///
    /// # Safety
    ///
    /// The same requirements as install_replacing_first apply. The arena
    /// must remain alive for the process lifetime.
    pub unsafe fn install_replacing_first_in_arena(
        site: HookSite<'_>,
        hook: HookCallback,
        staleness: StalenessBudget,
        arena: &TrampolineArena,
    ) -> Result<Self, TrampolineError> {
        let plan =
            TrampolinePlan::from_scan_replacing_first(site.scan, site.execute_address, hook)?;
        let trampoline = arena.allocate(&plan)?;
        // SAFETY: forwarded from this method's mapping-lifetime contract.
        unsafe { Self::bind(site, plan, trampoline, Some(staleness)) }
    }

    /// Generates a quiescent replace-first hook in preallocated storage.
    ///
    /// The returned hook must be toggled only with
    /// [`Self::activate_quiescent`] and [`Self::deactivate_quiescent`].
    ///
    /// # Safety
    ///
    /// The same requirements as [`Self::install_replacing_first_in_arena`]
    /// apply. Each quiescent toggle additionally requires its documented
    /// no-other-thread proof.
    pub unsafe fn install_replacing_first_in_arena_quiescent(
        site: HookSite<'_>,
        hook: HookCallback,
        arena: &TrampolineArena,
    ) -> Result<Self, TrampolineError> {
        let plan =
            TrampolinePlan::from_scan_replacing_first(site.scan, site.execute_address, hook)?;
        let trampoline = arena.allocate(&plan)?;
        // SAFETY: forwarded from this method's mapping-lifetime contract.
        unsafe { Self::bind(site, plan, trampoline, None) }
    }

    /// Generates a quiescent replace-first hook bracketed by tracer stops.
    ///
    /// The returned hook must be toggled only with
    /// [`Self::activate_quiescent`] and [`Self::deactivate_quiescent`]. Its
    /// entry and completion INT3 instructions must be authenticated and
    /// suppressed by an external ptrace controller on every execution.
    ///
    /// # Safety
    ///
    /// The same requirements as [`Self::install_replacing_first_in_arena`]
    /// apply. Each quiescent toggle additionally requires its documented
    /// no-other-thread proof, and the caller must guarantee that the controller
    /// owns both generated breakpoint stops before this hook becomes reachable.
    pub unsafe fn install_replacing_first_in_arena_quiescent_with_ptrace_stops(
        site: HookSite<'_>,
        hook: HookCallback,
        arena: &TrampolineArena,
    ) -> Result<Self, TrampolineError> {
        let plan = TrampolinePlan::from_scan_replacing_first_with_ptrace_stops(
            site.scan,
            site.execute_address,
            hook,
        )?;
        let trampoline = arena.allocate(&plan)?;
        // SAFETY: forwarded from this method's mapping-lifetime contract.
        unsafe { Self::bind(site, plan, trampoline, None) }
    }

    unsafe fn bind(
        site: HookSite<'_>,
        trampoline_plan: TrampolinePlan,
        trampoline: ExecutableTrampoline,
        staleness: Option<StalenessBudget>,
    ) -> Result<Self, TrampolineError> {
        let patch_plan = match JumpPatchPlan::from_scan(
            site.scanner,
            site.scan,
            site.code,
            site.region_base,
            site.execute_address,
            trampoline.address,
        ) {
            Ok(plan) if plan.displaced_len() == trampoline_plan.displaced_len() => plan,
            Ok(_) => {
                trampoline.discard();
                return Err(PatchError::RegionMismatch {
                    address: site.execute_address,
                }
                .into());
            }
            Err(error) => {
                trampoline.discard();
                return Err(error.into());
            }
        };
        // SAFETY: forwarded from the public installation method.
        let patch = match unsafe {
            if let Some(staleness) = staleness {
                LiveJumpPatch::bind(patch_plan, site.writable_address, staleness)
            } else {
                LiveJumpPatch::bind_quiescent(patch_plan, site.writable_address)
            }
        } {
            Ok(patch) => patch,
            Err(error) => {
                trampoline.discard();
                return Err(error.into());
            }
        };
        Ok(Self {
            trampoline,
            patch,
            state: AtomicU8::new(STATE_INACTIVE),
        })
    }

    /// Returns the generated executable trampoline.
    pub const fn trampoline(&self) -> &ExecutableTrampoline {
        &self.trampoline
    }

    /// Returns whether the redirect is fully active.
    pub fn is_active(&self) -> bool {
        self.state.load(Ordering::Acquire) == STATE_ACTIVE
    }

    /// Returns the number of concurrent-publication guard traps handled.
    pub fn handled_guard_traps(&self) -> u64 {
        self.patch.handled_guard_traps()
    }

    /// Activates the hook, returning whether code bytes changed.
    pub fn activate(&self) -> Result<bool, TrampolineError> {
        self.activate_with(|| {
            // SAFETY: install captured the concurrent binding contract.
            unsafe { self.patch.apply() }
        })
    }

    /// Activates the hook without concurrent-reader publication protection.
    ///
    /// # Safety
    ///
    /// No other process thread, signal handler, or code writer may fetch, read,
    /// or modify the patch window until this call returns.
    pub unsafe fn activate_quiescent(&self) -> Result<bool, TrampolineError> {
        self.activate_with(|| {
            // SAFETY: forwarded from this method's caller-verified quiescence.
            unsafe { self.patch.apply_quiescent() }
        })
    }

    fn activate_with(
        &self,
        publish: impl FnOnce() -> Result<(), PatchError>,
    ) -> Result<bool, TrampolineError> {
        match self.state.compare_exchange(
            STATE_INACTIVE,
            STATE_TRANSITIONING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => match publish() {
                Ok(()) => {
                    self.state.store(STATE_ACTIVE, Ordering::Release);
                    Ok(true)
                }
                Err(error) => {
                    self.state.store(STATE_INACTIVE, Ordering::Release);
                    Err(error.into())
                }
            },
            Err(STATE_ACTIVE) => Ok(false),
            Err(_) => Err(TrampolineError::TransitionInProgress),
        }
    }

    /// Deactivates the hook, returning whether code bytes changed.
    pub fn deactivate(&self) -> Result<bool, TrampolineError> {
        self.deactivate_with(|| {
            // SAFETY: install captured the concurrent binding contract.
            unsafe { self.patch.revert() }
        })
    }

    /// Deactivates the hook without concurrent-reader publication protection.
    ///
    /// # Safety
    ///
    /// The same quiescence proof as [`Self::activate_quiescent`] is required.
    pub unsafe fn deactivate_quiescent(&self) -> Result<bool, TrampolineError> {
        self.deactivate_with(|| {
            // SAFETY: forwarded from this method's caller-verified quiescence.
            unsafe { self.patch.revert_quiescent() }
        })
    }

    fn deactivate_with(
        &self,
        publish: impl FnOnce() -> Result<(), PatchError>,
    ) -> Result<bool, TrampolineError> {
        match self.state.compare_exchange(
            STATE_ACTIVE,
            STATE_TRANSITIONING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => match publish() {
                Ok(()) => {
                    self.state.store(STATE_INACTIVE, Ordering::Release);
                    Ok(true)
                }
                Err(error) => {
                    self.state.store(STATE_ACTIVE, Ordering::Release);
                    Err(error.into())
                }
            },
            Err(STATE_INACTIVE) => Ok(false),
            Err(_) => Err(TrampolineError::TransitionInProgress),
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct PendingReservation {
    address: *mut AtomicUsize,
    len: usize,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl PendingReservation {
    fn new() -> Result<Self, TrampolineError> {
        let len = page_size()?;
        // SAFETY: a fresh page-aligned shared mapping is large enough for the
        // atomic. It has no executable alias and no owner can see it yet.
        let address = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(os_error("mmap shared arena reservation"));
        }
        let address = address.cast::<AtomicUsize>();
        // SAFETY: this constructor uniquely owns aligned, writable storage.
        unsafe { address.write(AtomicUsize::new(0)) };
        Ok(Self { address, len })
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl Drop for PendingReservation {
    fn drop(&mut self) {
        // SAFETY: the guard still owns this unpublished metadata mapping.
        unsafe { libc::munmap(self.address.cast(), self.len) };
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct PendingMapping {
    fd: libc::c_int,
    writable: *mut libc::c_void,
    executable: *mut libc::c_void,
    len: usize,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const ARENA_REQUIRED_SEALS: libc::c_int =
    libc::F_SEAL_FUTURE_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn seal_arena_backing(fd: libc::c_int) -> Result<(), TrampolineError> {
    if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, ARENA_REQUIRED_SEALS) } != 0 {
        return Err(os_error("seal trampoline arena memfd"));
    }
    let observed_seals = unsafe { libc::fcntl(fd, libc::F_GET_SEALS) };
    if observed_seals != ARENA_REQUIRED_SEALS {
        return Err(TrampolineError::BackingStore {
            operation: "verify trampoline arena memfd seals",
            errno: if observed_seals < 0 {
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO)
            } else {
                libc::EIO
            },
        });
    }
    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl PendingMapping {
    fn new(len: usize) -> Result<Self, TrampolineError> {
        let name = b"liteinst2-trampoline\0";
        // SAFETY: valid NUL-terminated name and supported memfd flags.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                name.as_ptr().cast::<libc::c_char>(),
                libc::MFD_CLOEXEC | 0x0002, // MFD_ALLOW_SEALING
            ) as libc::c_int
        };
        if fd < 0 {
            return Err(os_error("memfd_create"));
        }
        let mut mapping = Self {
            fd,
            writable: core::ptr::null_mut(),
            executable: core::ptr::null_mut(),
            len,
        };
        // SAFETY: fd is live and len is a positive page multiple.
        if unsafe { libc::ftruncate(fd, len as libc::off_t) } != 0 {
            return Err(os_error("ftruncate"));
        }
        // SAFETY: creates a shared, non-executable writable alias.
        let writable = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if writable == libc::MAP_FAILED {
            return Err(os_error("mmap writable alias"));
        }
        mapping.writable = writable;
        Ok(mapping)
    }

    fn try_map_executable(&mut self, address: usize) -> bool {
        // SAFETY: MAP_FIXED_NOREPLACE never replaces an existing VMA.
        let executable = unsafe {
            libc::mmap(
                address as *mut libc::c_void,
                self.len,
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_SHARED | libc::MAP_FIXED_NOREPLACE,
                self.fd,
                0,
            )
        };
        if executable == libc::MAP_FAILED {
            return false;
        }
        if executable as usize != address {
            // SAFETY: executable is the mapping just returned by mmap.
            unsafe { libc::munmap(executable, self.len) };
            return false;
        }
        self.executable = executable;
        true
    }

    fn allocate_exact(
        mapping_start: usize,
        len: usize,
        entry_address: u64,
    ) -> Result<Self, TrampolineError> {
        let mut mapping = Self::new(len)?;
        if !mapping.try_map_executable(mapping_start) {
            return Err(TrampolineError::ExactAddressUnavailable {
                address: entry_address,
            });
        }
        Ok(mapping)
    }

    fn allocate_near(execute_address: u64) -> Result<Self, TrampolineError> {
        let page_size = page_size()?;
        let len = TRAMPOLINE_ALLOCATION_BYTES.max(page_size);
        Self::allocate_near_len(execute_address, len)
    }

    fn allocate_near_len(execute_address: u64, len: usize) -> Result<Self, TrampolineError> {
        let page_size = page_size()?;
        let len = len
            .checked_add(page_size - 1)
            .map(|value| value & !(page_size - 1))
            .ok_or(TrampolineError::InvalidPageSize)?;
        let mut mapping = Self::new(len)?;
        let next_ip = execute_address
            .checked_add(crate::patcher::NEAR_JUMP_BYTES as u64)
            .ok_or(TrampolineError::AddressNotRepresentable {
                address: execute_address,
            })?;
        for candidate in near_candidates(next_ip, len, page_size)? {
            if mapping.try_map_executable(candidate) {
                return Ok(mapping);
            }
        }
        Err(TrampolineError::NoReachableMapping)
    }

    fn into_arena(mut self) -> Result<TrampolineArena, TrampolineError> {
        let next = PendingReservation::new()?;
        // FUTURE_WRITE preserves the existing RW alias while preventing any
        // subsequently reopened descriptor or mapping from changing the code
        // backing. Geometry and policy are then locked for the arena lifetime.
        seal_arena_backing(self.fd)?;
        // Linux may release a descriptor even when close reports an error.
        // Disarm ownership before attempting close so Drop cannot close a
        // subsequently reused descriptor number. Both mapping guards remain
        // live through this last fallible construction operation.
        let fd = core::mem::replace(&mut self.fd, -1);
        // SAFETY: fd is live; both mappings retain their backing object.
        if unsafe { libc::close(fd) } != 0 {
            return Err(os_error("close trampoline arena memfd"));
        }
        let arena = TrampolineArena {
            writable: self.writable.cast(),
            executable: self.executable.cast(),
            len: self.len,
            next: next.address,
            reservation_len: next.len,
        };
        self.writable = core::ptr::null_mut();
        self.executable = core::ptr::null_mut();
        core::mem::forget(next);
        Ok(arena)
    }

    fn publish_at(
        mut self,
        image: &TrampolineImage,
        entry_offset: usize,
    ) -> Result<ExecutableTrampoline, TrampolineError> {
        let initialized_end =
            entry_offset
                .checked_add(image.bytes.len())
                .ok_or(TrampolineError::CodeTooLarge {
                    code_len: image.bytes.len(),
                    allocation_len: self.len,
                })?;
        if initialized_end > self.len {
            return Err(TrampolineError::CodeTooLarge {
                code_len: initialized_end,
                allocation_len: self.len,
            });
        }
        // SAFETY: the writable alias contains the complete destination range.
        unsafe {
            core::ptr::copy_nonoverlapping(
                image.bytes.as_ptr(),
                self.writable.cast::<u8>().add(entry_offset),
                image.bytes.len(),
            );
        }
        compiler_fence(Ordering::SeqCst);
        // SAFETY: writable is the live mapping returned by mmap.
        if unsafe { libc::munmap(self.writable, self.len) } != 0 {
            return Err(os_error("munmap writable alias"));
        }
        self.writable = core::ptr::null_mut();
        // SAFETY: fd is live; mappings retain their backing object.
        if unsafe { libc::close(self.fd) } != 0 {
            return Err(os_error("close trampoline memfd"));
        }
        self.fd = -1;
        let mapping_address = self.executable as usize as u64;
        let address = mapping_address.checked_add(entry_offset as u64).ok_or(
            TrampolineError::AddressNotRepresentable {
                address: mapping_address,
            },
        )?;
        self.executable = core::ptr::null_mut();
        Ok(ExecutableTrampoline {
            address,
            mapping_address,
            allocation_len: self.len,
            code_len: image.bytes.len(),
            layout: image.layout,
            program_counters: image.program_counters.clone().into_boxed_slice(),
            program_counters_published: AtomicBool::new(false),
        })
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl Drop for PendingMapping {
    fn drop(&mut self) {
        if !self.writable.is_null() {
            // SAFETY: this object owns the live mapping.
            unsafe { libc::munmap(self.writable, self.len) };
        }
        if !self.executable.is_null() {
            // SAFETY: this object owns the live mapping.
            unsafe { libc::munmap(self.executable, self.len) };
        }
        if self.fd >= 0 {
            // SAFETY: this object owns the live descriptor.
            unsafe { libc::close(self.fd) };
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn page_size() -> Result<usize, TrampolineError> {
    // SAFETY: sysconf has no pointer arguments.
    let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let value = usize::try_from(value).map_err(|_| TrampolineError::InvalidPageSize)?;
    if value == 0 || !value.is_power_of_two() {
        return Err(TrampolineError::InvalidPageSize);
    }
    Ok(value)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn os_error(operation: &'static str) -> TrampolineError {
    TrampolineError::BackingStore {
        operation,
        errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn near_candidates(
    next_ip: u64,
    len: usize,
    page_size: usize,
) -> Result<Vec<usize>, TrampolineError> {
    let next_ip = usize::try_from(next_ip)
        .map_err(|_| TrampolineError::AddressNotRepresentable { address: next_ip })?;
    let low = (next_ip as i128 + i32::MIN as i128)
        .max(page_size as i128)
        .max(0) as usize;
    let high = (next_ip as i128 + i32::MAX as i128).min((usize::MAX - len) as i128) as usize;
    if low > high {
        return Err(TrampolineError::NoReachableMapping);
    }
    let maps = read_process_maps_bounded()?;
    let record_count = maps.lines().count();
    let max_records = MAX_PROCESS_MAPS_BYTES / MIN_PROCESS_MAP_RECORD_BYTES + 1;
    if record_count > max_records {
        return Err(TrampolineError::ProcessMaps {
            message: format!(
                "/proc/self/maps contains {record_count} records; limit is {max_records}"
            ),
        });
    }
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(record_count)
        .map_err(|error| TrampolineError::ProcessMaps {
            message: format!("could not reserve bounded process-map ranges: {error}"),
        })?;
    for line in maps.lines() {
        let field = line
            .split_whitespace()
            .next()
            .ok_or_else(|| TrampolineError::ProcessMaps {
                message: format!("missing address range in {line:?}"),
            })?;
        let (start, end) = field
            .split_once('-')
            .ok_or_else(|| TrampolineError::ProcessMaps {
                message: format!("malformed address range {field:?}"),
            })?;
        let start =
            usize::from_str_radix(start, 16).map_err(|error| TrampolineError::ProcessMaps {
                message: format!("invalid range start {start:?}: {error}"),
            })?;
        let end = usize::from_str_radix(end, 16).map_err(|error| TrampolineError::ProcessMaps {
            message: format!("invalid range end {end:?}: {error}"),
        })?;
        if start < end {
            ranges.push((start, end));
        }
    }
    ranges.sort_unstable();
    let mut candidates = Vec::new();
    candidates
        .try_reserve_exact(ranges.len().saturating_add(1))
        .map_err(|error| TrampolineError::ProcessMaps {
            message: format!("could not reserve bounded near-map candidates: {error}"),
        })?;
    let mut cursor = low;
    for (start, end) in ranges {
        if end <= cursor {
            continue;
        }
        if start > cursor {
            add_gap_candidate(
                &mut candidates,
                cursor,
                start,
                low,
                high,
                next_ip,
                len,
                page_size,
            );
        }
        cursor = cursor.max(end);
        if cursor > high.saturating_add(len) {
            break;
        }
    }
    add_gap_candidate(
        &mut candidates,
        cursor,
        usize::MAX,
        low,
        high,
        next_ip,
        len,
        page_size,
    );
    order_near_candidates(&mut candidates, next_ip);
    candidates.dedup();
    Ok(candidates)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn read_process_maps_bounded() -> Result<String, TrampolineError> {
    let file =
        std::fs::File::open("/proc/self/maps").map_err(|error| TrampolineError::ProcessMaps {
            message: error.to_string(),
        })?;
    read_process_maps_from(file)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn read_process_maps_from(mut reader: impl std::io::Read) -> Result<String, TrampolineError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(MAX_PROCESS_MAPS_BYTES)
        .map_err(|error| TrampolineError::ProcessMaps {
            message: format!("could not reserve bounded process-map bytes: {error}"),
        })?;
    let mut chunk = [0_u8; 4096];
    loop {
        let amount = reader
            .read(&mut chunk)
            .map_err(|error| TrampolineError::ProcessMaps {
                message: error.to_string(),
            })?;
        if amount == 0 {
            break;
        }
        let Some(new_length) = bytes.len().checked_add(amount) else {
            return Err(TrampolineError::ProcessMaps {
                message: "/proc/self/maps byte length overflowed".to_owned(),
            });
        };
        if new_length > MAX_PROCESS_MAPS_BYTES {
            return Err(TrampolineError::ProcessMaps {
                message: format!("/proc/self/maps exceeds its {MAX_PROCESS_MAPS_BYTES}-byte limit"),
            });
        }
        bytes.extend_from_slice(&chunk[..amount]);
    }
    String::from_utf8(bytes).map_err(|error| TrampolineError::ProcessMaps {
        message: format!("/proc/self/maps is not UTF-8: {error}"),
    })
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn order_near_candidates(candidates: &mut [usize], next_ip: usize) {
    // A mapping above the program image can begin exactly where [heap] ends,
    // permanently capping future brk growth. Lower gaps cannot obstruct the
    // upward-growing heap, and every candidate is already within rel32 reach.
    candidates.sort_unstable_by_key(|candidate| {
        let is_above = *candidate > next_ip;
        (is_above, candidate.abs_diff(next_ip))
    });
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[allow(clippy::too_many_arguments)]
fn add_gap_candidate(
    candidates: &mut Vec<usize>,
    gap_start: usize,
    gap_end: usize,
    low: usize,
    high: usize,
    preferred: usize,
    len: usize,
    page_size: usize,
) {
    let min_start = align_up(gap_start.max(low), page_size);
    let Some(last_start) = gap_end.checked_sub(len) else {
        return;
    };
    let max_start = align_down(last_start.min(high), page_size);
    if min_start > max_start {
        return;
    }
    candidates.push(align_down(preferred, page_size).clamp(min_start, max_start));
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn align_down(value: usize, alignment: usize) -> usize {
    value & !(alignment - 1)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn align_up(value: usize, alignment: usize) -> usize {
    value.saturating_add(alignment - 1) & !(alignment - 1)
}

#[cfg(test)]
mod tests {
    use super::HookContext;
    use super::TrampolineError;
    use super::TrampolineLayout;
    use super::TrampolinePlan;
    use super::encode_return_jump;
    use crate::scanner::InstructionScanner;

    unsafe extern "C" fn noop_hook(_context: *mut HookContext) {}

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn process_maps_reader_accepts_the_exact_cap_and_rejects_one_extra_byte() {
        let exact = vec![b'x'; super::MAX_PROCESS_MAPS_BYTES];
        assert_eq!(
            super::read_process_maps_from(std::io::Cursor::new(exact))
                .unwrap()
                .len(),
            super::MAX_PROCESS_MAPS_BYTES
        );
        let oversized = vec![b'x'; super::MAX_PROCESS_MAPS_BYTES + 1];
        assert!(matches!(
            super::read_process_maps_from(std::io::Cursor::new(oversized)),
            Err(TrampolineError::ProcessMaps { .. })
        ));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn arena_seals_preserve_existing_writer_and_close_external_mutation_paths() {
        let name = b"liteinst2-seal-test\0";
        let fd = unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                name.as_ptr().cast::<libc::c_char>(),
                libc::MFD_CLOEXEC | 0x0002,
            ) as libc::c_int
        };
        assert!(fd >= 0);
        let page = 4096_usize;
        assert_eq!(unsafe { libc::ftruncate(fd, page as libc::off_t) }, 0);
        let writable = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        let executable = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(writable, libc::MAP_FAILED);
        assert_ne!(executable, libc::MAP_FAILED);
        let held = unsafe { libc::dup(fd) };
        assert!(held >= 0);

        super::seal_arena_backing(fd).unwrap();
        assert_eq!(
            unsafe { libc::fcntl(held, libc::F_GET_SEALS) },
            super::ARENA_REQUIRED_SEALS
        );
        unsafe { writable.cast::<u8>().write_volatile(0x5a) };
        assert_eq!(unsafe { executable.cast::<u8>().read_volatile() }, 0x5a);

        let byte = 0xa5_u8;
        assert_eq!(
            unsafe { libc::pwrite(held, (&raw const byte).cast(), 1, 0) },
            -1
        );
        assert_eq!(
            unsafe { libc::ftruncate(held, (2 * page) as libc::off_t) },
            -1
        );
        assert_eq!(
            unsafe { libc::ftruncate(held, (page / 2) as libc::off_t) },
            -1
        );
        let new_writer = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                held,
                0,
            )
        };
        assert_eq!(new_writer, libc::MAP_FAILED);

        assert_eq!(unsafe { libc::munmap(writable, page) }, 0);
        assert_eq!(unsafe { libc::munmap(executable, page) }, 0);
        assert_eq!(unsafe { libc::close(held) }, 0);
        assert_eq!(unsafe { libc::close(fd) }, 0);
    }

    #[test]
    fn exhausted_arena_reservation_cannot_wrap_and_reuse_storage() {
        use core::sync::atomic::AtomicUsize;
        use core::sync::atomic::Ordering;
        let near_wrap = usize::MAX - (super::TRAMPOLINE_ALLOCATION_BYTES - 1);
        let last_start = near_wrap - super::TRAMPOLINE_ALLOCATION_BYTES;
        let cursor = AtomicUsize::new(last_start);
        assert_eq!(
            super::reserve_arena_slot(&cursor, usize::MAX).unwrap(),
            last_start
        );
        assert_eq!(cursor.load(Ordering::Acquire), near_wrap);
        for _ in 0..32 {
            assert!(matches!(
                super::reserve_arena_slot(&cursor, usize::MAX),
                Err(TrampolineError::ArenaFull)
            ));
            assert_eq!(cursor.load(Ordering::Acquire), near_wrap);
        }
        let cursor = AtomicUsize::new(super::TRAMPOLINE_ALLOCATION_BYTES);
        assert!(matches!(
            super::reserve_arena_slot(&cursor, super::TRAMPOLINE_ALLOCATION_BYTES),
            Err(TrampolineError::ArenaFull)
        ));
        assert_eq!(
            cursor.load(Ordering::Acquire),
            super::TRAMPOLINE_ALLOCATION_BYTES
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn failed_emission_consumes_its_slot_without_overwriting_published_bytes() {
        let address = noop_hook as *const () as usize as u64;
        let scan = InstructionScanner::default()
            .scan(&[0xb8, 11, 0, 0, 0, 0xc3], address)
            .unwrap();
        let plan = TrampolinePlan::from_scan(&scan, address, noop_hook).unwrap();
        let arena = super::TrampolineArena::allocate_near(address, 3).unwrap();
        let first = arena.allocate(&plan).unwrap().address();
        let first_image = plan.emit_at(first).unwrap();
        // Public plan construction rejects malformed input. Construct an invalid
        // instruction here to exercise the real encoder error after reservation.
        let mut malformed = plan.clone();
        malformed.instructions[0] = iced_x86::Instruction::default();
        assert!(matches!(
            arena.allocate(&malformed),
            Err(TrampolineError::Encoding { .. })
        ));
        let third = arena.allocate(&plan).unwrap().address();
        assert_eq!(third - first, 2 * super::TRAMPOLINE_ALLOCATION_BYTES as u64);
        assert!(matches!(
            arena.allocate(&plan),
            Err(TrampolineError::ArenaFull)
        ));
        // SAFETY: these are process-lifetime RX mappings of three real slots.
        let retained =
            unsafe { core::slice::from_raw_parts(first as *const u8, first_image.bytes().len()) };
        assert_eq!(retained, first_image.bytes());
        let failed_slot = unsafe {
            core::slice::from_raw_parts(
                (first as usize + super::TRAMPOLINE_ALLOCATION_BYTES) as *const u8,
                super::TRAMPOLINE_ALLOCATION_BYTES,
            )
        };
        assert!(failed_slot.iter().all(|byte| *byte == 0));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn reachable_mapping_order_preserves_upward_heap_growth() {
        let next_ip = 0x401005;
        let below_image = 0x380000;
        let immediately_above_heap = 0x426000;
        let farther_below_image = 0x300000;
        let mut candidates = [immediately_above_heap, farther_below_image, below_image];

        super::order_near_candidates(&mut candidates, next_ip);

        assert_eq!(
            candidates,
            [below_image, farther_below_image, immediately_above_heap]
        );
    }

    fn plan(code: &[u8], base: u64) -> Result<TrampolinePlan, TrampolineError> {
        let scan = InstructionScanner::default()
            .scan(code, base)
            .expect("fixture must decode");
        TrampolinePlan::from_scan(&scan, base, noop_hook)
    }

    #[test]
    fn total_length_includes_every_section() {
        let layout = TrampolineLayout {
            entry_stop_len: 1,
            instrumentation_len: 32,
            completion_stop_len: 1,
            relocated_len: 12,
            restore_len: 16,
            return_len: 5,
        };
        assert_eq!(layout.total_len(), Some(67));
    }

    #[test]
    fn total_length_rejects_overflow() {
        let layout = TrampolineLayout {
            instrumentation_len: usize::MAX,
            relocated_len: 1,
            ..TrampolineLayout::default()
        };
        assert_eq!(layout.total_len(), None);
    }

    #[test]
    fn relocates_multiple_short_instructions_as_one_patch_window() {
        let plan = plan(&[0x48, 0x89, 0xF8, 0x90, 0x90], 0x1000).unwrap();

        assert_eq!(plan.displaced_len(), 5);
        assert_eq!(plan.return_address(), 0x1005);
    }

    #[test]
    fn rejects_a_short_region_without_five_complete_bytes() {
        let error = plan(&[0x48, 0x89, 0xF8], 0x1000).unwrap_err();

        assert!(matches!(
            error,
            TrampolineError::InstructionTooShort {
                address: 0x1000,
                instruction_len: 3
            }
        ));
    }

    #[test]
    fn out_of_range_return_uses_cet_notrack_indirect_jump() {
        let target = 0x1_0000_0000;
        let jump = encode_return_jump(0, target).unwrap();

        assert_eq!(&jump[..7], &[0x3E, 0xFF, 0x25, 0, 0, 0, 0]);
        assert_eq!(&jump[7..], &target.to_le_bytes());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn replace_first_omits_a_two_byte_syscall_but_relocates_its_tail() {
        let base = 0x20_0000;
        let code = [0x0F, 0x05, 0x48, 0x83, 0xC0, 0x01];
        let scan = InstructionScanner::default().scan(&code, base).unwrap();
        let plan = TrampolinePlan::from_scan_replacing_first(&scan, base, noop_hook).unwrap();
        let image = plan.emit_at(base + 0x10_0000).unwrap();

        assert!(plan.replaces_first());
        assert_eq!(plan.displaced_len(), 6);
        assert_eq!(plan.return_address(), base + 6);
        assert!(image.layout().relocated_len > 0);
        assert_eq!(image.layout().entry_stop_len, 0);
        assert_eq!(image.layout().completion_stop_len, 0);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn ptrace_stops_bracket_instrumentation_and_restored_application_state() {
        let base = 0x20_0000;
        let trampoline = base + 0x10_0000;
        let code = [0x0F, 0x05, 0x48, 0x83, 0xC0, 0x01];
        let scan = InstructionScanner::default().scan(&code, base).unwrap();
        let plan =
            TrampolinePlan::from_scan_replacing_first_with_ptrace_stops(&scan, base, noop_hook)
                .unwrap();
        let image = plan.emit_at(trampoline).unwrap();
        let layout = image.layout();
        let completion_offset =
            layout.entry_stop_len + layout.instrumentation_len + layout.restore_len;
        let relocated_offset = completion_offset + layout.completion_stop_len;

        assert_eq!(layout.entry_stop_len, 1);
        assert_eq!(layout.completion_stop_len, 1);
        assert_eq!(image.bytes()[0], 0xcc);
        assert_eq!(image.bytes()[completion_offset], 0xcc);
        let executable = super::ExecutableTrampoline {
            address: trampoline,
            allocation_len: super::TRAMPOLINE_ALLOCATION_BYTES,
            mapping_address: 0,
            code_len: image.bytes().len(),
            layout,
            program_counters: Box::new([]),
            program_counters_published: std::sync::atomic::AtomicBool::new(false),
        };
        assert_eq!(executable.ptrace_entry_stop_rip(), Some(trampoline + 1));
        assert_eq!(
            executable.ptrace_completion_stop_rip(),
            Some(trampoline + relocated_offset as u64)
        );
        assert_eq!(
            executable.relocated_tail_address(),
            trampoline + relocated_offset as u64
        );
        assert_eq!(
            image
                .program_counter_mappings()
                .iter()
                .find_map(|mapping| mapping.translate(trampoline)),
            Some(base)
        );
        assert_eq!(
            image
                .program_counter_mappings()
                .iter()
                .find_map(|mapping| mapping.translate(trampoline + relocated_offset as u64)),
            Some(base + 2)
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn emits_checked_layouts_for_five_seven_and_ten_byte_instructions() {
        let fixtures: &[&[u8]] = &[
            &[0xB8, 1, 0, 0, 0],
            &[0x48, 0x8D, 0x05, 0, 0, 0, 0],
            &[0x48, 0xB8, 1, 2, 3, 4, 5, 6, 7, 8],
        ];
        for (index, code) in fixtures.iter().enumerate() {
            let base = 0x10_0000 + index as u64 * 0x1000;
            let plan = plan(code, base).unwrap();
            let image = plan.emit_at(base + 0x10_0000).unwrap();
            assert_eq!(plan.displaced_len(), code.len());
            assert_eq!(plan.return_address(), base + code.len() as u64);
            assert_eq!(image.layout().total_len(), Some(image.bytes().len()));
            assert_eq!(image.layout().return_len, 5);
            let return_offset = image.bytes().len() - image.layout().return_len;
            let return_address = base + 0x10_0000 + return_offset as u64;
            let jump = &image.bytes()[return_offset..];
            assert_eq!(jump[0], 0xE9);
            let displacement = i32::from_le_bytes(jump[1..].try_into().unwrap());
            assert_eq!(
                i128::from(return_address + 5) + i128::from(displacement),
                i128::from(plan.return_address())
            );

            let relocated_address = base
                + 0x10_0000
                + image.layout().instrumentation_len as u64
                + image.layout().restore_len as u64;
            assert_eq!(
                image
                    .program_counter_mappings()
                    .iter()
                    .find_map(|mapping| mapping.translate(relocated_address)),
                Some(base)
            );
            assert_eq!(
                image
                    .program_counter_mappings()
                    .iter()
                    .find_map(|mapping| mapping.translate(return_address)),
                Some(plan.return_address())
            );
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn reencodes_rip_relative_memory_for_the_trampoline_address() {
        use iced_x86::Decoder;
        use iced_x86::DecoderOptions;

        let base = 0x20_0000;
        let code = [0x8B, 0x05, 0x34, 0x12, 0, 0];
        let original_target = base + code.len() as u64 + 0x1234;
        let plan = plan(&code, base).unwrap();
        let trampoline_address = base + 0x20_0000;
        let image = plan.emit_at(trampoline_address).unwrap();
        let start = image.layout().instrumentation_len + image.layout().restore_len;
        let relocated_ip = trampoline_address
            + image.layout().instrumentation_len as u64
            + image.layout().restore_len as u64;
        let end = start + image.layout().relocated_len;
        let instruction = Decoder::with_ip(
            64,
            &image.bytes()[start..end],
            relocated_ip,
            DecoderOptions::NONE,
        )
        .decode();
        assert_eq!(instruction.ip_rel_memory_address(), original_target);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn relocates_relative_calls_and_expands_out_of_range_branches() {
        use iced_x86::Decoder;
        use iced_x86::DecoderOptions;

        let call_base = 0x30_0000_u64;
        let call_target = call_base + 0x2000;
        let call_displacement =
            i32::try_from(call_target as i128 - (call_base + 5) as i128).unwrap();
        let mut call = [0_u8; 5];
        call[0] = 0xE8;
        call[1..].copy_from_slice(&call_displacement.to_le_bytes());
        let call_plan = plan(&call, call_base).unwrap();
        let call_trampoline = call_base + 0x10_0000;
        let call_image = call_plan.emit_at(call_trampoline).unwrap();
        let call_start = call_image.layout().instrumentation_len + call_image.layout().restore_len;
        let call_ip = call_trampoline + call_start as u64;
        let relocated_call = Decoder::with_ip(
            64,
            &call_image.bytes()[call_start..call_start + call_image.layout().relocated_len],
            call_ip,
            DecoderOptions::NONE,
        )
        .decode();
        assert_eq!(relocated_call.near_branch_target(), call_target);

        let branch_base = 0x40_0000_u64;
        let branch = [0x0F, 0x84, 0, 1, 0, 0];
        let branch_plan = plan(&branch, branch_base).unwrap();
        let branch_image = branch_plan.emit_at(0x2_0000_0000).unwrap();
        assert!(branch_image.layout().relocated_len > branch.len());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn maps_each_consecutive_expanded_branch_to_its_original_pc() {
        let base = 0x40_0000_u64;
        for (branches, expanded_len) in [
            ([0x74, 0x80, 0x75, 0x80, 0x74, 0x80], 8_u64),
            ([0xEB, 0x80, 0xEB, 0x80, 0xEB, 0x80], 6_u64),
        ] {
            let trampoline_address = 0x2_0000_0000;
            let plan = plan(&branches, base).unwrap();
            let image = plan.emit_at(trampoline_address).unwrap();
            let relocated_address = trampoline_address
                + image.layout().instrumentation_len as u64
                + image.layout().restore_len as u64;
            let second = image
                .program_counter_mappings()
                .iter()
                .find(|mapping| mapping.logical_address() == base + 2)
                .unwrap();
            let third = image
                .program_counter_mappings()
                .iter()
                .find(|mapping| mapping.logical_address() == base + 4)
                .unwrap();

            assert_eq!(second.generated_start(), relocated_address + expanded_len);
            assert_eq!(
                third.generated_start(),
                relocated_address + 2 * expanded_len
            );
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn far_tail_code_and_data_have_distinct_pc_mappings() {
        let base = 0x1_8000_0000_u64;
        let target = 0x1_1000_0000_u64;
        for call in [false, true] {
            let mut code = if call {
                vec![0xE8]
            } else {
                vec![0x85, 0xFF, 0x0F, 0x84]
            };
            let original_len = if call { 5 } else { 8 };
            let displacement =
                i32::try_from(i128::from(target) - i128::from(base + original_len)).unwrap();
            code.extend_from_slice(&displacement.to_le_bytes());
            let plan = plan(&code, base).unwrap();
            for start in [base + 0x7000_0000, 0x4_0000_0000] {
                // Exercise every pointer-table alignment, for both final returns.
                for alignment in 0..8 {
                    let address = start + alignment;
                    let image = plan.emit_at(address).unwrap();
                    let layout = image.layout();
                    let relocated = layout.instrumentation_len + layout.restore_len;
                    let return_offset = relocated + layout.relocated_len;
                    let terminal = relocated + if call { 6 } else { 10 };
                    let translate = |offset: usize| {
                        image
                            .program_counter_mappings()
                            .iter()
                            .find_map(|mapping| mapping.translate(address + offset as u64))
                    };
                    for offset in relocated..terminal {
                        let logical = if call || offset < relocated + 2 {
                            base
                        } else {
                            base + 2
                        };
                        assert_eq!(translate(offset), Some(logical));
                    }
                    assert_eq!(image.bytes()[terminal], 0xE9);
                    let displacement = i32::from_le_bytes(
                        image.bytes()[terminal + 1..terminal + 5]
                            .try_into()
                            .unwrap(),
                    );
                    assert_eq!(
                        i128::from(address + terminal as u64 + 5) + i128::from(displacement),
                        i128::from(address + return_offset as u64)
                    );
                    for offset in terminal..terminal + 5 {
                        assert_eq!(translate(offset), Some(plan.return_address()));
                    }
                    for offset in terminal + 5..return_offset {
                        assert_eq!(translate(offset), None, "data offset {offset}");
                    }
                    assert_eq!(
                        &image.bytes()[return_offset - 8..return_offset],
                        &target.to_le_bytes()
                    );
                    assert!(
                        image.bytes()[terminal + 5..return_offset - 8]
                            .iter()
                            .all(|byte| *byte == 0xCC)
                    );
                    let return_code_len = if layout.return_len == 5 { 5 } else { 7 };
                    assert_eq!(
                        layout.return_len,
                        if start == base + 0x7000_0000 { 5 } else { 15 }
                    );
                    for offset in return_offset..return_offset + return_code_len {
                        assert_eq!(translate(offset), Some(plan.return_address()));
                    }
                    for offset in return_offset + return_code_len..image.bytes().len() {
                        assert_eq!(
                            translate(offset),
                            None,
                            "final return literal offset {offset}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn terminal_instruction_does_not_capture_low_application_targets() {
        let mut branch =
            iced_x86::Instruction::with_branch(iced_x86::Code::Jne_rel8_64, 1).unwrap();
        branch.set_ip(0);
        let mut jump = iced_x86::Instruction::with_branch(iced_x86::Code::Jmp_rel8_64, 2).unwrap();
        jump.set_ip(3);
        let address = 0x4_0000_0000;
        let (encoded, terminal) = super::encode_relocated_block(&[branch, jump], address).unwrap();
        assert!(terminal.is_some());
        let targets: Vec<_> = encoded
            .reloc_infos
            .iter()
            .map(|info| {
                let offset = (info.address - address) as usize;
                u64::from_le_bytes(encoded.code_buffer[offset..offset + 8].try_into().unwrap())
            })
            .collect();
        assert_eq!(targets, [1, 2]);
        assert_eq!(encoded.new_instruction_offsets, [u32::MAX, u32::MAX]);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    mod live {
        use core::arch::asm;
        use core::sync::atomic::AtomicBool;
        use core::sync::atomic::AtomicU64;
        use core::sync::atomic::Ordering;
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::sync::Mutex;
        use std::sync::MutexGuard;
        use std::thread;
        use std::time::Duration;
        use std::time::Instant;

        use super::HookContext;
        use super::InstructionScanner;
        use crate::patcher::PatchError;
        use crate::patcher::StalenessBudget;
        use crate::trampoline::HookSite;
        use crate::trampoline::InstalledHook;
        use crate::trampoline::TrampolineError;

        const PAGE_BYTES: usize = 4096;
        const STALENESS_TICKS: u64 = 3_000;
        static TEST_LOCK: Mutex<()> = Mutex::new(());
        static CALLBACKS: AtomicU64 = AtomicU64::new(0);
        static LAST_IP: AtomicU64 = AtomicU64::new(0);

        fn serial_guard() -> MutexGuard<'static, ()> {
            TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }
        static LAST_RDI: AtomicU64 = AtomicU64::new(0);

        unsafe extern "C" fn record_hook(context: *mut HookContext) {
            // SAFETY: generated code passes a live HookContext for this call.
            let context = unsafe { &*context };
            LAST_IP.store(context.instruction_pointer, Ordering::Relaxed);
            LAST_RDI.store(context.rdi, Ordering::Relaxed);
            CALLBACKS.fetch_add(1, Ordering::Relaxed);
        }

        unsafe extern "C" fn replace_first_hook(context: *mut HookContext) {
            // SAFETY: generated code passes a unique mutable snapshot.
            unsafe {
                (*context).rax = 40;
            }
            CALLBACKS.fetch_add(1, Ordering::Relaxed);
        }

        unsafe extern "C" fn clobber_flags_hook(_context: *mut HookContext) {
            let mut scratch: u64;
            // SAFETY: deliberately clobbers caller-saved RAX and arithmetic flags.
            unsafe {
                asm!(
                    "mov {scratch}, 0xffff",
                    "add {scratch}, 1",
                    scratch = lateout(reg) scratch,
                    options(nostack),
                );
            }
            core::hint::black_box(scratch);
            CALLBACKS.fetch_add(1, Ordering::Relaxed);
        }

        unsafe extern "C" fn clobber_xmm_hook(_context: *mut HookContext) {
            // SAFETY: XMM0 is caller-saved; the trampoline must restore it.
            unsafe {
                asm!("pxor xmm0, xmm0", out("xmm0") _, options(nostack, preserves_flags));
            }
            CALLBACKS.fetch_add(1, Ordering::Relaxed);
        }

        unsafe extern "C" fn clobber_ymm_hook(_context: *mut HookContext) {
            // SAFETY: YMM1 is caller-saved; XSAVE must restore its upper lane.
            unsafe {
                asm!(
                    "vpxor ymm1, ymm1, ymm1",
                    out("ymm1") _,
                    options(nostack, preserves_flags),
                );
            }
            CALLBACKS.fetch_add(1, Ordering::Relaxed);
        }
        struct DualMapping {
            writable: *mut u8,
            executable: *mut u8,
        }

        impl DualMapping {
            fn new() -> Self {
                let name = b"liteinst2-trampoline-test\0";
                // SAFETY: valid NUL-terminated name and flags.
                let fd = unsafe {
                    libc::syscall(
                        libc::SYS_memfd_create,
                        name.as_ptr().cast::<libc::c_char>(),
                        libc::MFD_CLOEXEC,
                    ) as libc::c_int
                };
                assert!(fd >= 0, "memfd_create failed");
                // SAFETY: fd is valid and the size is representable.
                assert_eq!(unsafe { libc::ftruncate(fd, PAGE_BYTES as libc::off_t) }, 0);
                // SAFETY: map a writable, non-executable shared alias.
                let writable = unsafe {
                    libc::mmap(
                        core::ptr::null_mut(),
                        PAGE_BYTES,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_SHARED,
                        fd,
                        0,
                    )
                };
                assert_ne!(writable, libc::MAP_FAILED);
                // SAFETY: map a separate read-execute shared alias.
                let executable = unsafe {
                    libc::mmap(
                        core::ptr::null_mut(),
                        PAGE_BYTES,
                        libc::PROT_READ | libc::PROT_EXEC,
                        libc::MAP_SHARED,
                        fd,
                        0,
                    )
                };
                assert_ne!(executable, libc::MAP_FAILED);
                // SAFETY: both mappings retain the backing object.
                assert_eq!(unsafe { libc::close(fd) }, 0);
                Self {
                    writable: writable.cast(),
                    executable: executable.cast(),
                }
            }

            fn write(&self, offset: usize, bytes: &[u8]) {
                assert!(offset + bytes.len() <= PAGE_BYTES);
                // SAFETY: the writable mapping contains the requested range.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        self.writable.add(offset),
                        bytes.len(),
                    );
                }
            }

            fn write_u32(&self, offset: usize, value: u32) {
                self.write(offset, &value.to_le_bytes());
            }

            fn executable_address(&self, offset: usize) -> u64 {
                // SAFETY: tests keep offsets within the mapping.
                unsafe { self.executable.add(offset) as usize as u64 }
            }

            fn writable_address(&self, offset: usize) -> *mut u8 {
                // SAFETY: tests keep offsets within the mapping.
                unsafe { self.writable.add(offset) }
            }
        }

        fn install(
            mapping: &DualMapping,
            function_offset: usize,
            code: &[u8],
            site_offset: usize,
            hook: unsafe extern "C" fn(*mut HookContext),
        ) -> InstalledHook {
            mapping.write(function_offset, code);
            let scanner = InstructionScanner::default();
            let region_base = mapping.executable_address(function_offset);
            let scan = scanner.scan(code, region_base).unwrap();
            // SAFETY: the test leaks both aliases for the process lifetime.
            unsafe {
                InstalledHook::install(
                    HookSite::new(
                        &scanner,
                        &scan,
                        code,
                        region_base,
                        region_base + site_offset as u64,
                        mapping.writable_address(function_offset + site_offset),
                    ),
                    hook,
                    StalenessBudget::new(STALENESS_TICKS).unwrap(),
                )
                .unwrap()
            }
        }

        fn mapping_permissions(address: u64) -> String {
            let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
            maps.lines()
                .find_map(|line| {
                    let mut fields = line.split_whitespace();
                    let range = fields.next()?;
                    let permissions = fields.next()?;
                    let (start, end) = range.split_once('-')?;
                    let start = u64::from_str_radix(start, 16).ok()?;
                    let end = u64::from_str_radix(end, 16).ok()?;
                    (start <= address && address < end).then(|| permissions.to_owned())
                })
                .expect("trampoline address must have a VMA")
        }

        #[test]
        fn hook_fires_preserves_result_and_toggles_idempotently() {
            let _guard = serial_guard();
            CALLBACKS.store(0, Ordering::Relaxed);
            LAST_IP.store(0, Ordering::Relaxed);
            LAST_RDI.store(0, Ordering::Relaxed);

            let mapping = DualMapping::new();
            let offset = 60;
            let code = [0xB8, 7, 0, 0, 0, 0x01, 0xF8, 0xC3];
            let hook = install(&mapping, offset, &code, 0, record_hook);
            assert_eq!(
                hook.trampoline()
                    .translate_program_counter(hook.trampoline().relocated_tail_address()),
                Some(mapping.executable_address(offset))
            );
            assert!(
                !hook
                    .trampoline()
                    .program_counters_published
                    .load(Ordering::Acquire),
                "ordinary hook binding published process-lifetime PC metadata"
            );
            // SAFETY: executable bytes implement extern C fn(u32) -> u32.
            let function: unsafe extern "C" fn(u32) -> u32 =
                unsafe { core::mem::transmute(mapping.executable_address(offset) as usize) };

            assert_eq!(unsafe { function(5) }, 12);
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 0);
            assert!(hook.activate().unwrap());
            assert!(!hook.activate().unwrap());
            assert!(hook.is_active());
            assert_eq!(unsafe { function(5) }, 12);
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
            assert_eq!(
                LAST_IP.load(Ordering::Relaxed),
                mapping.executable_address(offset)
            );
            assert_eq!(LAST_RDI.load(Ordering::Relaxed), 5);
            assert!(hook.deactivate().unwrap());
            assert!(!hook.deactivate().unwrap());
            assert!(!hook.is_active());
            assert_eq!(unsafe { function(9) }, 16);
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);

            let next_ip = mapping.executable_address(offset) + 5;
            let displacement = i128::from(hook.trampoline().address()) - i128::from(next_ip);
            assert!(i32::try_from(displacement).is_ok());
            let permissions = mapping_permissions(hook.trampoline().address());
            assert!(permissions.contains('x'));
            assert!(!permissions.contains('w'));
        }

        #[test]
        fn arena_hook_replaces_a_short_first_instruction_and_runs_the_tail() {
            let _guard = serial_guard();
            CALLBACKS.store(0, Ordering::Relaxed);

            let mapping = DualMapping::new();
            let offset = 256;
            // xor eax,eax; inc eax; nop; ret; padding
            let code = [0x31, 0xC0, 0xFF, 0xC0, 0x90, 0xC3, 0x90, 0x90];
            mapping.write(offset, &code);
            let scanner = InstructionScanner::default();
            let address = mapping.executable_address(offset);
            let scan = scanner.scan(&code, address).unwrap();
            let arena = crate::trampoline::TrampolineArena::allocate_near(address, 2).unwrap();
            // SAFETY: the dual aliases and arena are process-lifetime mappings.
            let hook = unsafe {
                InstalledHook::install_replacing_first_in_arena(
                    HookSite::new(
                        &scanner,
                        &scan,
                        &code,
                        address,
                        address,
                        mapping.writable_address(offset),
                    ),
                    replace_first_hook,
                    StalenessBudget::new(STALENESS_TICKS).unwrap(),
                    &arena,
                )
                .unwrap()
            };
            // SAFETY: fixture bytes implement extern C fn() -> u32.
            let function: unsafe extern "C" fn() -> u32 =
                unsafe { core::mem::transmute(address as usize) };

            assert_eq!(unsafe { function() }, 1);
            assert!(hook.activate().unwrap());
            assert_eq!(unsafe { function() }, 41);
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
            assert!(hook.deactivate().unwrap());
            assert_eq!(unsafe { function() }, 1);
        }

        #[test]
        fn quiescent_arena_hook_patches_a_cache_line_straddler() {
            let _guard = serial_guard();
            CALLBACKS.store(0, Ordering::Relaxed);

            let mapping = DualMapping::new();
            let offset = 63;
            // xor eax,eax; inc eax; nop; ret; padding
            let code = [0x31, 0xC0, 0xFF, 0xC0, 0x90, 0xC3, 0x90, 0x90];
            mapping.write(offset, &code);
            let scanner = InstructionScanner::default();
            let address = mapping.executable_address(offset);
            let scan = scanner.scan(&code, address).unwrap();
            let arena = crate::trampoline::TrampolineArena::allocate_near(address, 2).unwrap();
            // SAFETY: the dual aliases and arena are process-lifetime mappings.
            let hook = unsafe {
                InstalledHook::install_replacing_first_in_arena_quiescent(
                    HookSite::new(
                        &scanner,
                        &scan,
                        &code,
                        address,
                        address,
                        mapping.writable_address(offset),
                    ),
                    replace_first_hook,
                    &arena,
                )
                .unwrap()
            };
            // SAFETY: fixture bytes implement extern C fn() -> u32.
            let function: unsafe extern "C" fn() -> u32 =
                unsafe { core::mem::transmute(address as usize) };

            assert_eq!(unsafe { function() }, 1);
            assert!(matches!(
                hook.activate(),
                Err(TrampolineError::Patch(
                    PatchError::QuiescentPublicationRequired
                ))
            ));
            // SAFETY: the test is single-threaded and runs no signal handlers.
            assert!(unsafe { hook.activate_quiescent() }.unwrap());
            assert_eq!(unsafe { function() }, 41);
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
            assert_eq!(hook.handled_guard_traps(), 0);
            // SAFETY: the test is single-threaded and runs no signal handlers.
            assert!(unsafe { hook.deactivate_quiescent() }.unwrap());
            assert_eq!(unsafe { function() }, 1);
        }

        #[test]
        fn relocates_rip_relative_load_and_returns_to_original_code() {
            let _guard = serial_guard();
            CALLBACKS.store(0, Ordering::Relaxed);
            let mapping = DualMapping::new();
            let function_offset = 128;
            let data_offset = 512;
            let function_address = mapping.executable_address(function_offset);
            let displacement = i32::try_from(
                mapping.executable_address(data_offset) as i128 - (function_address + 6) as i128,
            )
            .unwrap();
            let mut code = [0_u8; 8];
            code[..2].copy_from_slice(&[0x8B, 0x05]);
            code[2..6].copy_from_slice(&displacement.to_le_bytes());
            code[6] = 0xC3;
            code[7] = 0x90;
            mapping.write_u32(data_offset, 0x1234_5678);
            let hook = install(&mapping, function_offset, &code, 0, record_hook);
            // SAFETY: fixture is an extern C function returning u32.
            let function: unsafe extern "C" fn() -> u32 =
                unsafe { core::mem::transmute(function_address as usize) };

            assert_eq!(unsafe { function() }, 0x1234_5678);
            hook.activate().unwrap();
            assert_eq!(unsafe { function() }, 0x1234_5678);
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
            hook.deactivate().unwrap();
        }

        #[test]
        fn relocates_relative_call_and_resumes_after_its_return() {
            let _guard = serial_guard();
            CALLBACKS.store(0, Ordering::Relaxed);
            let mapping = DualMapping::new();
            let function_offset = 192;
            let helper_offset = 640;
            let function_address = mapping.executable_address(function_offset);
            let helper_address = mapping.executable_address(helper_offset);
            let displacement =
                i32::try_from(helper_address as i128 - (function_address + 5) as i128).unwrap();
            let mut code = [0_u8; 9];
            code[0] = 0xE8;
            code[1..5].copy_from_slice(&displacement.to_le_bytes());
            code[5..].copy_from_slice(&[0x83, 0xC0, 0x01, 0xC3]);
            mapping.write(helper_offset, &[0xB8, 41, 0, 0, 0, 0xC3]);
            let hook = install(&mapping, function_offset, &code, 0, record_hook);
            // SAFETY: fixture calls its helper, adds one, and returns u32.
            let function: unsafe extern "C" fn() -> u32 =
                unsafe { core::mem::transmute(function_address as usize) };

            assert_eq!(unsafe { function() }, 42);
            hook.activate().unwrap();
            assert_eq!(unsafe { function() }, 42);
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
            hook.deactivate().unwrap();
        }

        #[test]
        fn preserves_live_flags_and_the_system_v_red_zone() {
            let _guard = serial_guard();
            CALLBACKS.store(0, Ordering::Relaxed);
            let flags_mapping = DualMapping::new();
            let flags_offset = 256;
            let flags_code = [
                0x83, 0xFF, 0x00, // cmp edi, 0
                0x0F, 0x1F, 0x44, 0x00, 0x00, // five-byte nop (site)
                0x0F, 0x94, 0xC0, // sete al
                0x0F, 0xB6, 0xC0, // movzx eax, al
                0xC3,
            ];
            let flags_hook = install(
                &flags_mapping,
                flags_offset,
                &flags_code,
                3,
                clobber_flags_hook,
            );
            // SAFETY: fixture is an extern C predicate.
            let predicate: unsafe extern "C" fn(u32) -> u32 = unsafe {
                core::mem::transmute(flags_mapping.executable_address(flags_offset) as usize)
            };
            flags_hook.activate().unwrap();
            assert_eq!(unsafe { predicate(0) }, 1);
            assert_eq!(unsafe { predicate(9) }, 0);
            flags_hook.deactivate().unwrap();

            let red_zone_mapping = DualMapping::new();
            let red_zone_offset = 320;
            let red_zone_code = [
                0x48, 0xC7, 0x44, 0x24, 0xF8, 0x78, 0x56, 0x34, 0x12, 0x0F, 0x1F, 0x44, 0x00, 0x00,
                0x48, 0x8B, 0x44, 0x24, 0xF8, 0xC3,
            ];
            let red_zone_hook = install(
                &red_zone_mapping,
                red_zone_offset,
                &red_zone_code,
                9,
                clobber_flags_hook,
            );
            // SAFETY: fixture is an extern C function returning u64.
            let red_zone: unsafe extern "C" fn() -> u64 = unsafe {
                core::mem::transmute(red_zone_mapping.executable_address(red_zone_offset) as usize)
            };
            red_zone_hook.activate().unwrap();
            assert_eq!(unsafe { red_zone() }, 0x1234_5678);
            red_zone_hook.deactivate().unwrap();
        }

        #[test]
        fn preserves_xmm_argument_across_a_clobbering_callback() {
            let _guard = serial_guard();
            CALLBACKS.store(0, Ordering::Relaxed);
            let mapping = DualMapping::new();
            let offset = 384;
            let code = [0x0F, 0x1F, 0x44, 0x00, 0x00, 0xF2, 0x0F, 0x58, 0xC0, 0xC3];
            let hook = install(&mapping, offset, &code, 0, clobber_xmm_hook);
            // SAFETY: fixture doubles one f64 argument in XMM0.
            let function: unsafe extern "C" fn(f64) -> f64 =
                unsafe { core::mem::transmute(mapping.executable_address(offset) as usize) };
            assert_eq!(unsafe { function(3.25) }, 6.5);
            hook.activate().unwrap();
            assert_eq!(unsafe { function(3.25) }, 6.5);
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
            hook.deactivate().unwrap();
        }

        #[test]
        fn preserves_avx_upper_lane_across_a_clobbering_callback() {
            use iced_x86::code_asm::CodeAssembler;
            use iced_x86::code_asm::rax;
            use iced_x86::code_asm::xmm0;
            use iced_x86::code_asm::ymm1;

            if !std::is_x86_feature_detected!("avx2") {
                return;
            }
            let _guard = serial_guard();
            CALLBACKS.store(0, Ordering::Relaxed);
            let mapping = DualMapping::new();
            let offset = 512;
            let address = mapping.executable_address(offset);
            let mut assembler = CodeAssembler::new(64).unwrap();
            assembler.vpcmpeqd(ymm1, ymm1, ymm1).unwrap();
            assembler.db(&[0x0F, 0x1F, 0x44, 0x00, 0x00]).unwrap();
            assembler.vextracti128(xmm0, ymm1, 1).unwrap();
            assembler.vmovq(rax, xmm0).unwrap();
            assembler.vzeroupper().unwrap();
            assembler.ret().unwrap();
            let code = assembler.assemble(address).unwrap();
            let scan = InstructionScanner::default().scan(&code, address).unwrap();
            let site_offset = scan.instructions()[0].len();
            assert_eq!(scan.instructions()[1].len(), 5);
            let hook = install(&mapping, offset, &code, site_offset, clobber_ymm_hook);
            // SAFETY: fixture returns the high YMM1 lane as u64.
            let function: unsafe extern "C" fn() -> u64 =
                unsafe { core::mem::transmute(address as usize) };

            assert_eq!(unsafe { function() }, u64::MAX);
            hook.activate().unwrap();
            assert_eq!(unsafe { function() }, u64::MAX);
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
            hook.deactivate().unwrap();
        }

        #[test]
        fn concurrent_execution_survives_rapid_hook_toggling() {
            let _guard = serial_guard();
            CALLBACKS.store(0, Ordering::Relaxed);
            let mapping = DualMapping::new();
            let offset = 508;
            let code = [0xB8, 7, 0, 0, 0, 0x01, 0xF8, 0xC3];
            let hook = install(&mapping, offset, &code, 0, record_hook);
            let address = mapping.executable_address(offset) as usize;
            let stop = Arc::new(AtomicBool::new(false));
            let start = Arc::new(Barrier::new(5));
            let mut workers = Vec::new();
            for _ in 0..4 {
                let stop = Arc::clone(&stop);
                let start = Arc::clone(&start);
                workers.push(thread::spawn(move || {
                    // SAFETY: process-lifetime fixture implements this signature.
                    let function: unsafe extern "C" fn(u32) -> u32 =
                        unsafe { core::mem::transmute(address) };
                    start.wait();
                    while !stop.load(Ordering::Relaxed) {
                        assert_eq!(unsafe { function(11) }, 18);
                    }
                }));
            }
            start.wait();
            hook.activate().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while CALLBACKS.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(CALLBACKS.load(Ordering::Relaxed) > 0);
            hook.deactivate().unwrap();
            for _ in 1..1_000 {
                hook.activate().unwrap();
                hook.deactivate().unwrap();
            }
            stop.store(true, Ordering::Relaxed);
            for worker in workers {
                worker.join().unwrap();
            }
        }
    }
}
