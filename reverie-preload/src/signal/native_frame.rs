//! Checked relocation of native Linux x86-64 signal-frame byte images.
//!
//! This is not kernel provenance authentication or permission to call sigreturn.
//! The caller supplies stable readable source bytes and an independently obtained
//! expected FP format. Kernel restore semantics, segment/flag validity, altstack
//! policy, dynamic xstate permissions and runtime ownership are separate gates.
//! No signal handler, allocation, native control or return path is installed here.

use std::ops::Range;

/// Native `rt_sigframe`: restorer word, kernel ucontext and 128-byte siginfo.
/// This is the Linux x86-64 ABI, not `size_of::<libc::ucontext_t>()`.
const PREFIX_BYTES: usize = 440;
const FP_POINTER: usize = 232;
const UC_FLAGS: usize = 8;
const LEGACY_BYTES: usize = 512;
const XSAVE_HEADER_END: usize = 576;
const MAGIC1: u32 = 0x4650_5853;
const MAGIC2: u32 = 0x4650_5845;
const USER_FEATURES: u64 = 0xe02ff;
const DEST_FRAME: usize = 8;
const DEST_FP: usize = 448;
const GENERAL_OFFSET: usize = 48;
const MODELED_READ_LENGTH: u64 = 7;
const SUBTRACTION_FLAGS: u64 = 0x8d5;

/// Exact independently supplied format expectation, not discovered from input.
///
/// Standard XSAVE payloads are copied opaquely, without feature loss. The caller
/// must qualify the feature/size pair independently; matching it here does not
/// prove that a CPU or kernel accepts the layout. Unknown user features,
/// supervisor state and compacted XSAVE are unsupported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Legacy,
    StandardXsave { xfeatures: u64, xstate_size: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Overflow,
    SourceBounds,
    DestinationBounds,
    Overlap,
    Alignment,
    RestorerPosition,
    NullFpUnsupported,
    UnsupportedFormat,
    Metadata,
    InstructionPointer,
    Flags,
}

/// The three admitted modeled reads in the retained native RNG image.
///
/// These are complete instruction forms, not a general destination or length
/// vocabulary. Image/layout qualification and instruction decoding belong to
/// the runtime owner before it selects one of these operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModeledReadOperation {
    CompareReadyWithZero,
    LoadGenerationToRcx,
    CompareRaxWithGeneration,
}

/// Typed shared value for an admitted modeled read.
///
/// The future runtime adapter obtains these from the synchronous shared seam
/// `Tool::vdso_rng_snapshot(&self, &ThreadState) -> Result<VdsoRngSnapshot,
/// Error>`, whose snapshot contains `ready: bool` and `generation: u64`. It
/// converts with `Ready(u8::from(snapshot.ready))` or
/// `Generation(snapshot.generation)`; it must never reinterpret struct bytes.
/// That adapter is not implemented by this byte editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModeledReadValue {
    Ready(u8),
    Generation(u64),
}

/// Exact post-owned-TF-disarm fault-captured frame snapshot.
///
/// `flags` are the captured fault flags with only the independently owned TF
/// removed. They are not the original `CpuStepRequest` start flags.
/// `general` is ordered r8..r15, rdi, rsi, rbp, rbx, rdx, rax, rcx, rsp,
/// matching `owned_step::native::general`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModeledReadContext {
    pub pc: u64,
    pub general: [u64; 16],
    pub flags: u64,
}

/// Typed instruction outputs; never an arbitrary register or continuation edit.
#[cfg(feature = "coordinator-rpc")]
#[derive(Clone, Copy, Debug)]
pub enum InstructionResult {
    Cpuid(reverie::CpuIdResult),
    Rdtsc {
        request: reverie::Rdtsc,
        result: reverie::RdtscResult,
    },
}

/// Immutable relocated bytes borrowing only the destination, never the source.
///
/// This is not a sealed runtime continuation or a consumed-generation witness.
/// There is no execution method or pool-reuse protocol. The destination cannot
/// be safely mutated while this view is in use; dropping it ends only a Rust
/// borrow, not a kernel-frame lifetime.
#[derive(Debug)]
pub struct RelocatedFrame<'destination> {
    bytes: &'destination mut [u8],
}

impl RelocatedFrame<'_> {
    /// Complete a separately authenticated function-entry execute fault. This
    /// editor grants no mapping, return-address, memory or debug ownership.
    /// All FP bytes and all registers except RAX/RSP/RIP/RFLAGS remain exact.
    pub fn complete_vdso_call(self, call: FunctionReturn) -> Result<Self, Error> {
        if call.entry == 0
            || call.entry >= 1 << 47
            || call.target == 0
            || call.target >= 1 << 47
            || call.stack == 0
            || call.stack.checked_add(8).is_none_or(|end| end >= 1 << 47)
            || read_u64(self.frame_bytes(), 176)? != call.entry
            || read_u64(self.frame_bytes(), 168)? != call.stack
        {
            return Err(Error::InstructionPointer);
        }
        if read_u64(self.frame_bytes(), 184)? != call.flags
            || call.flags & 0x10000 == 0
            || call.flags & 0x20000 != 0
            || (call.flags & 0x100 != 0) != call.owned_tf
        {
            return Err(Error::Flags);
        }
        for (offset, value) in [
            (152, call.result as u64),
            (168, call.stack + 8),
            (176, call.target),
            (184, call.flags & !0x10100),
        ] {
            self.bytes[DEST_FRAME + offset..DEST_FRAME + offset + 8]
                .copy_from_slice(&value.to_le_bytes());
        }
        Ok(self)
    }
    /// Complete an already authenticated SUD event whose PC is the continuation.
    /// Only the signed RAX result changes; there is no instruction-PC advance.
    pub fn complete_syscall(self, resume_pc: u64, result: i64) -> Result<Self, Error> {
        if resume_pc == 0 || resume_pc >= 1 << 47 || read_u64(self.frame_bytes(), 176)? != resume_pc
        {
            return Err(Error::InstructionPointer);
        }
        self.bytes[DEST_FRAME + 152..DEST_FRAME + 160].copy_from_slice(&result.to_le_bytes());
        Ok(self)
    }

    /// Remove only the authenticated owned TF from a syscall's saved flags and
    /// R11. The caller must qualify their exact incoming values independently.
    pub fn owned_syscall_tf(self, resume_pc: u64, flags: u64, r11: u64) -> Result<Self, Error> {
        if resume_pc == 0 || resume_pc >= 1 << 47 || read_u64(self.frame_bytes(), 176)? != resume_pc
        {
            return Err(Error::InstructionPointer);
        }
        if flags & 0x100 == 0
            || r11 & 0x100 == 0
            || read_u64(self.frame_bytes(), 184)? != flags
            || read_u64(self.frame_bytes(), 72)? != r11
        {
            return Err(Error::Flags);
        }
        self.bytes[DEST_FRAME + 184..DEST_FRAME + 192]
            .copy_from_slice(&(flags & !0x100).to_le_bytes());
        self.bytes[DEST_FRAME + 72..DEST_FRAME + 80].copy_from_slice(&(r11 & !0x100).to_le_bytes());
        Ok(self)
    }

    pub fn frame_bytes(&self) -> &[u8] {
        &self.bytes[DEST_FRAME..DEST_FRAME + PREFIX_BYTES]
    }

    pub fn fp_bytes(&self) -> &[u8] {
        &self.bytes[DEST_FP..]
    }

    /// Retarget an authenticated private-CRT CPUID fault to the original loader.
    /// Only RIP and the fault-generated RF change. The caller separately owns
    /// the one-use bootstrap transition, complete captured registers/FP state,
    /// original-loader binding and successful shared Tool lifecycle callbacks.
    /// This editor supplies none of those ownership or execution guarantees.
    pub fn complete_private_start(
        self,
        fault_pc: u64,
        flags: u64,
        loader: u64,
    ) -> Result<Self, Error> {
        if read_u64(self.frame_bytes(), 176)? != fault_pc
            || fault_pc == 0
            || loader == 0
            || loader >= 1 << 47
        {
            return Err(Error::InstructionPointer);
        }
        if read_u64(self.frame_bytes(), 184)? != flags
            || flags & 0x10000 == 0
            || flags & (0x100 | 0x20000) != 0
        {
            return Err(Error::Flags);
        }
        self.bytes[DEST_FRAME + 176..DEST_FRAME + 184].copy_from_slice(&loader.to_le_bytes());
        self.bytes[DEST_FRAME + 184..DEST_FRAME + 192]
            .copy_from_slice(&(flags & !0x10000).to_le_bytes());
        Ok(self)
    }

    /// Change only backend-owned TF at an already authenticated guest PC.
    /// This byte editor grants neither debug ownership nor permission to return.
    /// The caller authenticates the expected complete flags and owns TF for the
    /// entire step. Initial guest TF must be refused independently by the runtime.
    pub fn owned_single_step(
        self,
        expected_pc: u64,
        expected_flags: u64,
        enable: bool,
    ) -> Result<Self, Error> {
        if expected_pc == 0
            || expected_pc >= 1 << 47
            || read_u64(self.frame_bytes(), 176)? != expected_pc
        {
            return Err(Error::InstructionPointer);
        }
        let flags = read_u64(self.frame_bytes(), 184)?;
        if flags != expected_flags || (flags & 0x100 != 0) == enable {
            return Err(Error::Flags);
        }
        let updated = if enable {
            flags | 0x100
        } else {
            flags & !0x100
        };
        self.bytes[DEST_FRAME + 184..DEST_FRAME + 192].copy_from_slice(&updated.to_le_bytes());
        Ok(self)
    }

    /// Consume the editor, changing only CPUID's four zero-extended outputs and
    /// its next PC. This does not authenticate an instruction or authorize a
    /// native return. The runtime must admit the captured PC and its successor.
    #[cfg(feature = "coordinator-rpc")]
    pub fn complete_cpuid(
        self,
        fault_pc: u64,
        result: reverie::CpuIdResult,
    ) -> Result<Self, Error> {
        self.complete_instruction(fault_pc, InstructionResult::Cpuid(result))
    }

    /// RDTSC preserves RCX; RDTSCP zero-extends AUX, using zero for `None`, as
    /// required by the existing in-guest Tool adapter. No FP metadata is edited.
    #[cfg(feature = "coordinator-rpc")]
    pub fn complete_rdtsc(
        self,
        fault_pc: u64,
        request: reverie::Rdtsc,
        result: reverie::RdtscResult,
    ) -> Result<Self, Error> {
        self.complete_instruction(fault_pc, InstructionResult::Rdtsc { request, result })
    }

    /// Apply only this instruction's typed outputs and checked successor PC.
    /// The runtime must authenticate the instruction kind, PC and native frame.
    #[cfg(feature = "coordinator-rpc")]
    pub fn complete_instruction(
        self,
        fault_pc: u64,
        result: InstructionResult,
    ) -> Result<Self, Error> {
        let (length, outputs) = match result {
            InstructionResult::Cpuid(result) => (
                2,
                [
                    Some((152, u64::from(result.eax))),
                    Some((136, u64::from(result.ebx))),
                    Some((160, u64::from(result.ecx))),
                    Some((144, u64::from(result.edx))),
                ],
            ),
            InstructionResult::Rdtsc { request, result } => (
                if request == reverie::Rdtsc::Tscp {
                    3
                } else {
                    2
                },
                [
                    Some((152, u64::from(result.tsc as u32))),
                    Some((144, result.tsc >> 32)),
                    (request == reverie::Rdtsc::Tscp)
                        .then_some((160, u64::from(result.aux.unwrap_or(0)))),
                    None,
                ],
            ),
        };
        let next_pc = fault_pc
            .checked_add(length)
            .ok_or(Error::InstructionPointer)?;
        if fault_pc == 0 || next_pc >= 1 << 47 || read_u64(self.frame_bytes(), 176)? != fault_pc {
            return Err(Error::InstructionPointer);
        }
        for (offset, value) in outputs.into_iter().flatten().chain([(176, next_pc)]) {
            self.bytes[DEST_FRAME + offset..DEST_FRAME + offset + 8]
                .copy_from_slice(&value.to_le_bytes());
        }
        Ok(self)
    }

    /// Complete an authenticated owned emulation after owned TF is disarmed.
    /// Only this completed instruction's RF is retired; all other flags remain.
    /// The caller must authenticate the fault, subscription, frame and result.
    #[cfg(feature = "coordinator-rpc")]
    pub fn complete_owned_instruction(
        self,
        fault_pc: u64,
        expected_flags: u64,
        result: InstructionResult,
    ) -> Result<Self, Error> {
        if read_u64(self.frame_bytes(), 184)? != expected_flags || expected_flags & 0x100 != 0 {
            return Err(Error::Flags);
        }
        let image = self.complete_instruction(fault_pc, result)?;
        image.bytes[DEST_FRAME + 184..DEST_FRAME + 192]
            .copy_from_slice(&(expected_flags & !0x10000).to_le_bytes());
        Ok(image)
    }

    /// Complete one admitted modeled read after owned TF was disarmed through
    /// the checked runtime path. The fixed instruction length is seven bytes.
    ///
    /// This byte editor validates the complete post-disarm GPR/flags/PC snapshot
    /// and applies only the selected instruction's architectural effects. It
    /// cannot authenticate image/layout/decode ownership, owner TID, frame
    /// generation, CPU-step sequence, count/timer observation or trace
    /// provenance; those remain runtime gates, not credentials accepted here.
    /// No RF precondition is invented: only the completed instruction's RF is
    /// cleared at retirement. A still-set TF is refused.
    ///
    /// The runtime bridge keeps the original step request `S` immutable and
    /// retains the genuine fault snapshot `F` unchanged. `S` is not completion
    /// authority. The runtime must separately qualify synchronous-fault
    /// provenance, owner/frame generation/CPU sequence, image/layout/address/
    /// width/code, and compare `S`'s original PC/all GPRs with captured `F` and
    /// all flags except independently owned TF and observed fault/resume RF
    /// transitions. Existing `validate_cpu_fault` alone is not full snapshot
    /// authentication.
    ///
    /// In the active owned-step fault path, captured `F.flags` has owned TF set.
    /// The bridge must use actual captured flags, never construct them as
    /// `S.flags | TF | RF` or infer RF from `S`. It first calls
    /// `image.owned_single_step(F.pc, F.flags, false)` to check the captured PC
    /// and full flags while clearing only TF. It then passes this editor
    /// `ModeledReadContext { pc: F.pc, general: capturedF.gprs, flags: F.flags &
    /// !0x100 }`. That preserves observed RF until successful completion clears
    /// it. An already-disarmed frame requires evidence of that prior checked
    /// disarm, not caller flags as authority. An editor refusal guarantees only
    /// its post-disarm input is unchanged; it cannot roll back the earlier TF
    /// disarm, and a failed bridge must not resume the guest.
    ///
    /// The runtime owns all count commitments. This editor creates no synthetic
    /// RF, trace or count, adds no runtime wiring or ownership type, and performs
    /// no shared snapshot call itself.
    pub fn complete_owned_modeled_read(
        self,
        expected: ModeledReadContext,
        operation: ModeledReadOperation,
        value: ModeledReadValue,
    ) -> Result<Self, Error> {
        enum Effect {
            Flags(u64),
            Rcx(u64),
        }

        let effect = match (operation, value) {
            (ModeledReadOperation::CompareReadyWithZero, ModeledReadValue::Ready(ready)) => {
                Effect::Flags(byte_subtraction_flags(ready, 0))
            }
            (
                ModeledReadOperation::LoadGenerationToRcx,
                ModeledReadValue::Generation(generation),
            ) => Effect::Rcx(generation),
            (
                ModeledReadOperation::CompareRaxWithGeneration,
                ModeledReadValue::Generation(generation),
            ) => Effect::Flags(qword_subtraction_flags(expected.general[13], generation)),
            _ => return Err(Error::Metadata),
        };

        let next_pc = expected
            .pc
            .checked_add(MODELED_READ_LENGTH)
            .ok_or(Error::InstructionPointer)?;
        if expected.pc == 0
            || next_pc == 0
            || next_pc >= 1 << 47
            || read_u64(self.frame_bytes(), 176)? != expected.pc
        {
            return Err(Error::InstructionPointer);
        }
        for (index, expected_register) in expected.general.into_iter().enumerate() {
            if read_u64(self.frame_bytes(), GENERAL_OFFSET + index * 8)? != expected_register {
                return Err(Error::Metadata);
            }
        }
        if read_u64(self.frame_bytes(), 184)? != expected.flags || expected.flags & 0x100 != 0 {
            return Err(Error::Flags);
        }

        let completed_flags = match effect {
            Effect::Flags(flags) => (expected.flags & !SUBTRACTION_FLAGS) | flags,
            Effect::Rcx(generation) => {
                self.bytes[DEST_FRAME + 160..DEST_FRAME + 168]
                    .copy_from_slice(&generation.to_le_bytes());
                expected.flags
            }
        } & !0x10000;
        self.bytes[DEST_FRAME + 176..DEST_FRAME + 184].copy_from_slice(&next_pc.to_le_bytes());
        self.bytes[DEST_FRAME + 184..DEST_FRAME + 192]
            .copy_from_slice(&completed_flags.to_le_bytes());
        Ok(self)
    }
}

fn subtraction_flags(left: u64, right: u64, result: u64, sign_bit: u64) -> u64 {
    u64::from(left < right)
        | (u64::from((result as u8).count_ones().is_multiple_of(2)) << 2)
        | (u64::from((left ^ right ^ result) & 0x10 != 0) << 4)
        | (u64::from(result == 0) << 6)
        | (u64::from(result & sign_bit != 0) << 7)
        | (u64::from((left ^ right) & (left ^ result) & sign_bit != 0) << 11)
}

fn byte_subtraction_flags(left: u8, right: u8) -> u64 {
    let result = left.wrapping_sub(right);
    subtraction_flags(u64::from(left), u64::from(right), u64::from(result), 1 << 7)
}

fn qword_subtraction_flags(left: u64, right: u64) -> u64 {
    subtraction_flags(left, right, left.wrapping_sub(right), 1 << 63)
}

/// Exact source fields and authenticated completion of a vDSO function call.
#[derive(Clone, Copy, Debug)]
pub struct FunctionReturn {
    pub entry: u64,
    pub stack: u64,
    pub target: u64,
    pub flags: u64,
    pub result: i64,
    pub owned_tf: bool,
}

/// Validate both source spans and relocate them into disjoint aligned storage.
///
/// `frame_address` and `restorer_sp` are asserted input addresses, NOT proof of
/// a kernel-origin event. They must describe the native frame/restorer relation
/// inside `source`; the FP pointer is checked against that same source allocation
/// before being followed as a slice offset. No raw pointer is dereferenced.
///
/// Destination starts at a 64-byte boundary; its frame begins eight bytes later,
/// so the restorer position is 16-byte aligned. FP data is separately 64-byte
/// aligned. Prefix/FP reserved data is preserved, except unsupported XSAVE header
/// fields reject. Null FP is refused until its semantics are qualified.
/// Every error leaves destination unchanged. Successful relocation changes only
/// the FP pointer and destination-only padding; it never repairs source data.
///
/// Safe borrowing prevents a stale view from observing destination reuse:
/// ```compile_fail
/// use reverie_preload::signal::native_frame::{relocate, Format};
/// let source = [0u8; 2048];
/// let mut destination = [0u8; 2048];
/// let view = relocate(&source, 0, 8, Format::Legacy, &mut destination).unwrap();
/// destination.fill(0);
/// assert!(!view.frame_bytes().is_empty());
/// ```
pub fn relocate<'destination>(
    source: &[u8],
    frame_address: usize,
    restorer_sp: usize,
    format: Format,
    destination: &'destination mut [u8],
) -> Result<RelocatedFrame<'destination>, Error> {
    let source_allocation = allocation(source.as_ptr() as usize, source.len())?;
    let destination_allocation = allocation(destination.as_ptr() as usize, destination.len())?;
    if overlaps(&source_allocation, &destination_allocation) {
        return Err(Error::Overlap);
    }
    let prefix = span(&source_allocation, frame_address, PREFIX_BYTES)?;
    if frame_address.checked_add(8).ok_or(Error::Overflow)? != restorer_sp {
        return Err(Error::RestorerPosition);
    }
    if frame_address % 16 != 8 || destination_allocation.start % 64 != 0 {
        return Err(Error::Alignment);
    }
    let prefix_bytes = &source[prefix.clone()];
    let fp_address =
        usize::try_from(read_u64(prefix_bytes, FP_POINTER)?).map_err(|_| Error::Overflow)?;
    if fp_address == 0 {
        return Err(Error::NullFpUnsupported);
    }
    let initial_fp = span(&source_allocation, fp_address, LEGACY_BYTES)?;
    if overlaps(&prefix, &initial_fp) {
        return Err(Error::Overlap);
    }
    let fp_length = validate_format(
        &source[initial_fp],
        read_u64(prefix_bytes, UC_FLAGS)?,
        format,
    )?;
    let fp = span(&source_allocation, fp_address, fp_length)?;
    let alignment = match format {
        Format::Legacy => 16,
        Format::StandardXsave { .. } => 64,
    };
    if !fp_address.is_multiple_of(alignment) {
        return Err(Error::Alignment);
    }
    if overlaps(&prefix, &fp) {
        return Err(Error::Overlap);
    }
    if let Format::StandardXsave { xfeatures, .. } = format {
        validate_xsave(&source[fp.clone()], xfeatures)?;
    }
    let required = DEST_FP.checked_add(fp_length).ok_or(Error::Overflow)?;
    if required > destination.len() {
        return Err(Error::DestinationBounds);
    }
    let relocated_fp = destination_allocation
        .start
        .checked_add(DEST_FP)
        .ok_or(Error::Overflow)?;
    destination[..DEST_FRAME].fill(0);
    destination[DEST_FRAME..DEST_FRAME + PREFIX_BYTES].copy_from_slice(prefix_bytes);
    destination[DEST_FRAME + PREFIX_BYTES..DEST_FP].fill(0);
    destination[DEST_FP..required].copy_from_slice(&source[fp]);
    destination[DEST_FRAME + FP_POINTER..DEST_FRAME + FP_POINTER + 8]
        .copy_from_slice(&(relocated_fp as u64).to_le_bytes());
    Ok(RelocatedFrame {
        bytes: &mut destination[..required],
    })
}

fn validate_format(fp: &[u8], flags: u64, format: Format) -> Result<usize, Error> {
    match format {
        Format::Legacy => {
            if flags != 6 || read_u32(fp, 464)? != 0 {
                return Err(Error::UnsupportedFormat);
            }
            Ok(LEGACY_BYTES)
        }
        Format::StandardXsave {
            xfeatures,
            xstate_size,
        } => {
            if flags != 7
                || xfeatures & 3 != 3
                || xfeatures & !USER_FEATURES != 0
                || xstate_size < XSAVE_HEADER_END as u32
                || read_u32(fp, 464)? != MAGIC1
            {
                return Err(Error::UnsupportedFormat);
            }
            let extended_size = xstate_size.checked_add(4).ok_or(Error::Overflow)?;
            if read_u32(fp, 468)? != extended_size
                || read_u64(fp, 472)? != xfeatures
                || read_u32(fp, 480)? != xstate_size
                || fp[484..512].iter().any(|byte| *byte != 0)
            {
                return Err(Error::Metadata);
            }
            Ok(extended_size as usize)
        }
    }
}

fn validate_xsave(fp: &[u8], xfeatures: u64) -> Result<(), Error> {
    if read_u64(fp, 512)? & !xfeatures != 0
        || fp[520..XSAVE_HEADER_END].iter().any(|byte| *byte != 0)
        || read_u32(fp, fp.len() - 4)? != MAGIC2
    {
        return Err(Error::Metadata);
    }
    Ok(())
}

fn allocation(address: usize, length: usize) -> Result<Range<usize>, Error> {
    Ok(address..address.checked_add(length).ok_or(Error::Overflow)?)
}

fn span(allocation: &Range<usize>, address: usize, length: usize) -> Result<Range<usize>, Error> {
    let end = address.checked_add(length).ok_or(Error::Overflow)?;
    if address < allocation.start || end > allocation.end {
        return Err(Error::SourceBounds);
    }
    Ok(address - allocation.start..end - allocation.start)
}

fn overlaps(first: &Range<usize>, second: &Range<usize>) -> bool {
    first.start < first.end
        && second.start < second.end
        && first.start < second.end
        && second.start < first.end
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let end = offset.checked_add(4).ok_or(Error::Overflow)?;
    let value = bytes.get(offset..end).ok_or(Error::SourceBounds)?;
    Ok(u32::from_le_bytes(
        value.try_into().map_err(|_| Error::SourceBounds)?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    let end = offset.checked_add(8).ok_or(Error::Overflow)?;
    let value = bytes.get(offset..end).ok_or(Error::SourceBounds)?;
    Ok(u64::from_le_bytes(
        value.try_into().map_err(|_| Error::SourceBounds)?,
    ))
}

#[cfg(test)]
mod tests;
