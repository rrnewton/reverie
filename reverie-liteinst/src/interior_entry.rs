//! Proof that no known control transfer lands inside a patch window.
//!
//! A LiteInst jump patch overwrites the first five bytes of the whole
//! instructions it displaces from a `syscall` site. A control transfer that
//! lands strictly inside those bytes then executes the tail of the jump as
//! code. liteinst2 refuses a direct branch into the window only when the
//! branch lies in the bytes it decoded, and those start at the site itself
//! (<https://github.com/rrnewton/reverie/issues/812>). glibc's `posix_madvise`
//! is an example: a `je` before its `syscall` targets the `ret` that the patch
//! displaces.
//!
//! This module therefore builds, once per loaded object and before the first
//! patch in it, a census of the object's executable mapping. The census walks
//! every function listed in the object's unwind table (`.eh_frame_hdr`, found
//! through the `PT_GNU_EH_FRAME` program header) and records these entries:
//!
//! - the start of every function in the unwind table;
//! - the target of every direct jump, conditional branch, call and loop;
//! - the return address of every call;
//! - every exception landing pad named by a function's language-specific data
//!   area (LSDA);
//! - every code address computed by a RIP-relative `lea`;
//! - every aligned eight-byte word in the object's other mappings whose value
//!   lies in its executable mapping, such as a relocated function pointer;
//! - the first instruction after a function's end that is not padding.
//!
//! Functions are decoded from their unwind-table starts, so the decoder stays
//! on the compiler's instruction boundaries. Bytes outside every function are
//! decoded too, but only for more entries: a misaligned decode there can add a
//! spurious entry, which refuses more sites and never fewer.
//!
//! A site passes only if its function is transparent: no indirect jump, which
//! could reach any of its instructions (a switch table or a computed goto);
//! every instruction decodes; its last instruction ends exactly at the function
//! end; and no other unwind-table function overlaps it. No entry may then lie
//! strictly inside the site's displaced bytes.
//!
//! Residuals: a branch from code that has no unwind information is found only
//! if the decode of that code happens to align. A code address that is held
//! only in anonymous memory or stored after the census, and that no
//! RIP-relative `lea` computes, is not found. An object without
//! `PT_GNU_EH_FRAME`, with more than one executable mapping, or with an unwind
//! encoding this parser does not accept gets no census, so none of its sites
//! are patched. Anonymous and JIT mappings have no image and get no census.

use std::fmt;

use iced_x86::Decoder;
use iced_x86::DecoderOptions;
use iced_x86::Instruction;
use iced_x86::Mnemonic;
use iced_x86::OpKind;
use liteinst2::patcher::NEAR_JUMP_BYTES;
use liteinst2::scanner::ScannedInstruction;

/// Bytes after a site within which the census records entries.
///
/// This bounds every displaced window: a window ends as soon as it covers
/// [`NEAR_JUMP_BYTES`], and an x86-64 instruction is at most 15 bytes long.
pub(crate) const WINDOW_LOOKAHEAD: u64 = 64;

const ELF_MACHINE_X86_64: u16 = 62;
const PROGRAM_HEADER_BYTES: usize = 56;
const PT_LOAD: u32 = 1;
const PT_GNU_EH_FRAME: u32 = 0x6474_e550;

const DW_EH_PE_OMIT: u8 = 0xff;
const DW_EH_PE_INDIRECT: u8 = 0x80;
const DW_EH_PE_PCREL: u8 = 0x10;
const DW_EH_PE_DATAREL: u8 = 0x30;
/// `DW_EH_PE_datarel | DW_EH_PE_sdata4`, the only binary-search table encoding
/// that GNU ld and LLVM lld emit.
const EH_FRAME_HDR_TABLE_ENCODING: u8 = 0x3b;

/// Why an object has no census.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CensusError(pub(crate) &'static str);

impl fmt::Display for CensusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

const TRUNCATED: CensusError = CensusError("an unwind or ELF table ends outside its mapping");
const UNSUPPORTED_ENCODING: CensusError =
    CensusError("an unwind table uses an unsupported pointer encoding");
const ALLOCATION: CensusError = CensusError("the installation heap cannot hold the census");

/// Why a site may not be patched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Refusal {
    /// The site's object has no census.
    NoCensus(CensusError),
    /// No function in the object's unwind table contains the site.
    NotInFunction,
    /// The site's function has an indirect jump, an instruction that does not
    /// decode, a last instruction that crosses its end, or an overlapping
    /// unwind-table function.
    OpaqueFunction,
    /// A known entry lies strictly inside the displaced bytes.
    InteriorEntry { entry: u64 },
    /// The live bytes at the site do not match the census.
    InstructionMismatch,
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCensus(error) => write!(formatter, "the object has no entry census: {error}"),
            Self::NotInFunction => {
                formatter.write_str("no unwind-table function contains the site")
            }
            Self::OpaqueFunction => formatter.write_str(
                "the site's function has an indirect jump, an undecodable instruction, \
                 or an overlapping unwind entry",
            ),
            Self::InteriorEntry { entry } => write!(
                formatter,
                "a control transfer enters the displaced bytes at {entry:#x}"
            ),
            Self::InstructionMismatch => {
                formatter.write_str("the live site instruction does not match the census")
            }
        }
    }
}

/// One readable mapping of a loaded object.
#[derive(Clone, Copy)]
pub(crate) struct Segment<'a> {
    pub(crate) address: u64,
    pub(crate) bytes: &'a [u8],
    pub(crate) executable: bool,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
    base: u64,
}

fn cursor<'a>(segments: &[Segment<'a>], address: u64) -> Result<Cursor<'a>, CensusError> {
    segments
        .iter()
        .find_map(|segment| {
            let offset = usize::try_from(address.checked_sub(segment.address)?).ok()?;
            Some(Cursor {
                bytes: segment
                    .bytes
                    .get(offset..)
                    .filter(|tail| !tail.is_empty())?,
                position: 0,
                base: address,
            })
        })
        .ok_or(CensusError(
            "an unwind or ELF table points outside the object's mappings",
        ))
}

impl<'a> Cursor<'a> {
    fn address(&self) -> u64 {
        self.base.wrapping_add(self.position as u64)
    }

    fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CensusError> {
        let end = self
            .position
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(TRUNCATED)?;
        let bytes = &self.bytes[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn sub(&mut self, len: usize) -> Result<Cursor<'a>, CensusError> {
        let base = self.address();
        Ok(Cursor {
            bytes: self.take(len)?,
            position: 0,
            base,
        })
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CensusError> {
        Ok(self.take(N)?.try_into().expect("take returns N bytes"))
    }

    fn u8(&mut self) -> Result<u8, CensusError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, CensusError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, CensusError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, CensusError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn uleb(&mut self) -> Result<u64, CensusError> {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = self.u8()?;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(CensusError("an unwind table has an overlong LEB128 value"))
    }

    fn sleb(&mut self) -> Result<i64, CensusError> {
        let mut value = 0_i64;
        for shift in (0..64).step_by(7) {
            let byte = self.u8()?;
            value |= i64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                if shift + 7 < 64 && byte & 0x40 != 0 {
                    value |= -1_i64 << (shift + 7);
                }
                return Ok(value);
            }
        }
        Err(CensusError("an unwind table has an overlong LEB128 value"))
    }

    fn len(&mut self) -> Result<usize, CensusError> {
        usize::try_from(self.uleb()?).map_err(|_| TRUNCATED)
    }

    /// Reads a value in the format named by the low nibble of `encoding`.
    fn encoded_value(&mut self, encoding: u8) -> Result<u64, CensusError> {
        Ok(match encoding & 0x0f {
            0x00 | 0x04 | 0x0c => self.u64()?,
            0x01 => self.uleb()?,
            0x02 => u64::from(self.u16()?),
            0x03 => u64::from(self.u32()?),
            0x09 => self.sleb()? as u64,
            0x0a => i64::from(self.u16()? as i16) as u64,
            0x0b => i64::from(self.u32()? as i32) as u64,
            _ => return Err(UNSUPPORTED_ENCODING),
        })
    }

    /// Reads a pointer as libgcc's `read_encoded_value_with_base` does: a raw
    /// zero stays zero, and otherwise the application base is added.
    fn encoded_pointer(
        &mut self,
        encoding: u8,
        data_base: Option<u64>,
    ) -> Result<u64, CensusError> {
        if encoding & DW_EH_PE_INDIRECT != 0 {
            return Err(UNSUPPORTED_ENCODING);
        }
        let position = self.address();
        let value = self.encoded_value(encoding)?;
        if value == 0 {
            return Ok(0);
        }
        let base = match encoding & 0x70 {
            0x00 => 0,
            DW_EH_PE_PCREL => position,
            DW_EH_PE_DATAREL => data_base.ok_or(UNSUPPORTED_ENCODING)?,
            _ => return Err(UNSUPPORTED_ENCODING),
        };
        Ok(base.wrapping_add(value))
    }
}

/// Finds the object's `.eh_frame_hdr` from its ELF program headers.
fn eh_frame_hdr(segments: &[Segment<'_>], header: u64) -> Result<u64, CensusError> {
    let mut elf = cursor(segments, header)?;
    let identity = elf.take(16)?;
    if identity[..4] != *b"\x7fELF" || identity[4] != 2 || identity[5] != 1 {
        return Err(CensusError("the object is not a 64-bit little-endian ELF"));
    }
    let _kind = elf.u16()?;
    if elf.u16()? != ELF_MACHINE_X86_64 {
        return Err(CensusError("the object is not an x86-64 ELF"));
    }
    let _version = elf.u32()?;
    let _entry = elf.u64()?;
    let program_headers = elf.u64()?;
    let _section_headers = elf.u64()?;
    let _flags = elf.u32()?;
    let _header_size = elf.u16()?;
    let entry_size = usize::from(elf.u16()?);
    let count = usize::from(elf.u16()?);
    if entry_size != PROGRAM_HEADER_BYTES {
        return Err(CensusError(
            "the object has an unexpected program header size",
        ));
    }
    let mut table = cursor(
        segments,
        header.checked_add(program_headers).ok_or(TRUNCATED)?,
    )?;
    let mut bias = None;
    let mut eh_frame_hdr = None;
    for _ in 0..count {
        let mut entry = table.sub(PROGRAM_HEADER_BYTES)?;
        let kind = entry.u32()?;
        let _flags = entry.u32()?;
        let offset = entry.u64()?;
        let address = entry.u64()?;
        if kind == PT_LOAD && offset == 0 && bias.is_none() {
            bias = Some(header.wrapping_sub(address));
        } else if kind == PT_GNU_EH_FRAME {
            eh_frame_hdr = Some(address);
        }
    }
    let bias = bias.ok_or(CensusError("the object has no PT_LOAD at file offset zero"))?;
    let eh_frame_hdr = eh_frame_hdr.ok_or(CensusError("the object has no PT_GNU_EH_FRAME"))?;
    Ok(bias.wrapping_add(eh_frame_hdr))
}

/// A function described by the unwind table.
#[derive(Clone, Copy)]
struct Function {
    start: u64,
    end: u64,
    lsda: u64,
    overlaps: bool,
}

impl Function {
    fn is_empty(&self) -> bool {
        self.start >= self.end
    }
}

struct CommonInformation {
    fde_encoding: u8,
    lsda_encoding: u8,
    has_augmentation_data: bool,
}

fn common_information(
    segments: &[Segment<'_>],
    address: u64,
) -> Result<CommonInformation, CensusError> {
    let mut record = cursor(segments, address)?;
    let length = record.u32()?;
    if length == 0 || length == u32::MAX {
        return Err(CensusError("an unwind table has an unsupported CIE length"));
    }
    let mut body = record.sub(length as usize)?;
    if body.u32()? != 0 {
        return Err(CensusError("an FDE names a record that is not a CIE"));
    }
    let version = body.u8()?;
    if !matches!(version, 1 | 3) {
        return Err(CensusError(
            "an unwind table has an unsupported CIE version",
        ));
    }
    let augmentation_start = body.position;
    while body.u8()? != 0 {}
    let augmentation = &body.bytes[augmentation_start..body.position - 1];
    let _code_alignment = body.uleb()?;
    let _data_alignment = body.sleb()?;
    let _return_register = if version == 1 {
        u64::from(body.u8()?)
    } else {
        body.uleb()?
    };
    let mut information = CommonInformation {
        fde_encoding: 0,
        lsda_encoding: DW_EH_PE_OMIT,
        has_augmentation_data: false,
    };
    let Some((&first, rest)) = augmentation.split_first() else {
        return Ok(information);
    };
    if first != b'z' {
        return Err(CensusError(
            "an unwind table has an unsupported CIE augmentation",
        ));
    }
    information.has_augmentation_data = true;
    let data_len = body.len()?;
    let mut data = body.sub(data_len)?;
    for letter in rest {
        match letter {
            b'L' => information.lsda_encoding = data.u8()?,
            b'R' => information.fde_encoding = data.u8()?,
            b'P' => {
                // Only the size of the personality pointer matters here.
                let encoding = data.u8()?;
                if encoding & 0x70 > DW_EH_PE_DATAREL {
                    return Err(UNSUPPORTED_ENCODING);
                }
                data.encoded_value(encoding)?;
            }
            b'S' | b'B' | b'G' => {}
            _ => {
                return Err(CensusError(
                    "an unwind table has an unsupported CIE augmentation",
                ));
            }
        }
    }
    Ok(information)
}

fn function(segments: &[Segment<'_>], address: u64) -> Result<Function, CensusError> {
    let mut record = cursor(segments, address)?;
    let length = record.u32()?;
    if length == 0 || length == u32::MAX {
        return Err(CensusError("an unwind table has an unsupported FDE length"));
    }
    let mut body = record.sub(length as usize)?;
    let cie_field = body.address();
    let cie_pointer = body.u32()?;
    if cie_pointer == 0 {
        return Err(CensusError("the unwind search table names a CIE as an FDE"));
    }
    let information = common_information(segments, cie_field.wrapping_sub(u64::from(cie_pointer)))?;
    let start = body.encoded_pointer(information.fde_encoding, None)?;
    let range = body.encoded_value(information.fde_encoding)?;
    let end = start
        .checked_add(range)
        .ok_or(CensusError("an FDE's address range overflows"))?;
    let mut lsda = 0;
    if information.has_augmentation_data {
        let data_len = body.len()?;
        let mut data = body.sub(data_len)?;
        if information.lsda_encoding != DW_EH_PE_OMIT {
            lsda = data.encoded_pointer(information.lsda_encoding, None)?;
        }
    }
    Ok(Function {
        start,
        end,
        lsda,
        overlaps: false,
    })
}

/// Reads every function from the `.eh_frame_hdr` binary-search table.
fn functions(segments: &[Segment<'_>], hdr: u64) -> Result<Vec<Function>, CensusError> {
    let mut table = cursor(segments, hdr)?;
    if table.u8()? != 1 {
        return Err(CensusError(
            "the object has an unsupported .eh_frame_hdr version",
        ));
    }
    let frame_encoding = table.u8()?;
    let count_encoding = table.u8()?;
    let table_encoding = table.u8()?;
    table.encoded_pointer(frame_encoding, Some(hdr))?;
    if count_encoding == DW_EH_PE_OMIT || table_encoding != EH_FRAME_HDR_TABLE_ENCODING {
        return Err(CensusError(
            "the object's .eh_frame_hdr has no search table",
        ));
    }
    let count = table.encoded_value(count_encoding)?;
    let count = usize::try_from(count).map_err(|_| TRUNCATED)?;
    let mut entries = table.sub(count.checked_mul(8).ok_or(TRUNCATED)?)?;
    let mut functions = Vec::new();
    functions.try_reserve_exact(count).map_err(|_| ALLOCATION)?;
    for _ in 0..count {
        let _start = entries.u32()?;
        let fde = hdr.wrapping_add(i64::from(entries.u32()? as i32) as u64);
        functions.push(function(segments, fde)?);
    }
    functions.sort_unstable_by_key(|function| (function.start, function.end));
    // A function overlaps another if it starts before an earlier one ends, or
    // ends after a later one starts. Empty functions cover no bytes.
    let mut furthest_end = 0;
    for function in functions.iter_mut().filter(|function| !function.is_empty()) {
        function.overlaps = function.start < furthest_end;
        furthest_end = furthest_end.max(function.end);
    }
    let mut nearest_start = u64::MAX;
    for function in functions
        .iter_mut()
        .rev()
        .filter(|function| !function.is_empty())
    {
        function.overlaps |= nearest_start < function.end;
        nearest_start = nearest_start.min(function.start);
    }
    Ok(functions)
}

/// Calls `record` with every landing pad in a GCC-format LSDA.
fn landing_pads(
    segments: &[Segment<'_>],
    lsda: u64,
    function_start: u64,
    record: &mut impl FnMut(u64),
) -> Result<(), CensusError> {
    let mut header = cursor(segments, lsda)?;
    let landing_pad_encoding = header.u8()?;
    let landing_pad_base = if landing_pad_encoding == DW_EH_PE_OMIT {
        function_start
    } else {
        header.encoded_pointer(landing_pad_encoding, None)?
    };
    if header.u8()? != DW_EH_PE_OMIT {
        let _type_table_offset = header.uleb()?;
    }
    let call_site_encoding = header.u8()?;
    if call_site_encoding & 0x70 != 0 {
        return Err(UNSUPPORTED_ENCODING);
    }
    let table_len = header.len()?;
    let mut table = header.sub(table_len)?;
    while !table.is_empty() {
        let _start = table.encoded_value(call_site_encoding)?;
        let _len = table.encoded_value(call_site_encoding)?;
        let landing_pad = table.encoded_value(call_site_encoding)?;
        let _action = table.uleb()?;
        if landing_pad != 0 {
            record(landing_pad_base.wrapping_add(landing_pad));
        }
    }
    Ok(())
}

fn is_indirect_jump(instruction: &Instruction) -> bool {
    instruction.mnemonic() == Mnemonic::Jmp
        && matches!(instruction.op0_kind(), OpKind::Register | OpKind::Memory)
}

fn is_padding(instruction: &Instruction) -> bool {
    matches!(instruction.mnemonic(), Mnemonic::Nop | Mnemonic::Int3)
}

fn instruction_entries(instruction: &Instruction, record: &mut impl FnMut(u64)) {
    if matches!(
        instruction.op0_kind(),
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
    ) {
        record(instruction.near_branch_target());
    }
    match instruction.mnemonic() {
        Mnemonic::Call => record(instruction.next_ip()),
        Mnemonic::Lea if instruction.is_ip_rel_memory_operand() => {
            record(instruction.ip_rel_memory_address());
        }
        _ => {}
    }
}

/// Decodes one function and reports whether it is transparent.
fn decode_function(code: &[u8], start: u64, visit: &mut impl FnMut(&Instruction)) -> bool {
    let mut decoder = Decoder::with_ip(64, code, start, DecoderOptions::NONE);
    let mut instruction = Instruction::default();
    let mut transparent = true;
    while decoder.can_decode() {
        // An instruction that crosses the function end cannot decode from
        // the truncated slice, so it also makes the function opaque.
        decoder.decode_out(&mut instruction);
        if instruction.is_invalid() {
            transparent = false;
            continue;
        }
        if is_indirect_jump(&instruction) {
            transparent = false;
        }
        visit(&instruction);
    }
    transparent
}

/// Decodes bytes outside every function, only to collect more entries.
fn decode_gap(code: &[u8], start: u64, record: &mut impl FnMut(u64)) {
    let mut decoder = Decoder::with_ip(64, code, start, DecoderOptions::NONE);
    let mut instruction = Instruction::default();
    while decoder.can_decode() {
        decoder.decode_out(&mut instruction);
        if !instruction.is_invalid() {
            instruction_entries(&instruction, record);
        }
    }
}

/// Records the first instruction after `end` that is not padding.
fn record_code_after(code: &[u8], end: u64, record: &mut impl FnMut(u64)) {
    let lookahead = code.len().min(WINDOW_LOOKAHEAD as usize);
    let mut decoder = Decoder::with_ip(64, &code[..lookahead], end, DecoderOptions::NONE);
    let mut instruction = Instruction::default();
    while decoder.can_decode() {
        decoder.decode_out(&mut instruction);
        if instruction.is_invalid() || !is_padding(&instruction) {
            record(instruction.ip());
            return;
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Site {
    address: u64,
    len: u8,
    opaque: bool,
    /// The lowest entry in `(address, address + WINDOW_LOOKAHEAD]`, or
    /// `u64::MAX` when there is none.
    limit: u64,
}

/// Every `syscall` instruction in one executable mapping, with the lowest
/// known entry after each.
pub(crate) struct Census {
    sites: Vec<Site>,
}

/// Counts that describe one census.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CensusSummary {
    pub(crate) functions: usize,
    pub(crate) sites: usize,
    pub(crate) opaque_sites: usize,
}

impl Census {
    /// Builds the census for the executable mapping `text` of the object
    /// whose readable mappings are `segments` and whose ELF header is mapped
    /// at `header`.
    pub(crate) fn build(
        segments: &[Segment<'_>],
        header: u64,
        text: (u64, u64),
    ) -> Result<(Self, CensusSummary), CensusError> {
        let (text_start, text_end) = text;
        let mut executable = segments.iter().filter(|segment| segment.executable);
        let text_code = match (executable.next(), executable.next()) {
            (Some(segment), None)
                if segment.address == text_start
                    && segment.bytes.len() as u64 == text_end - text_start =>
            {
                segment.bytes
            }
            _ => {
                return Err(CensusError(
                    "the object does not have exactly one executable mapping",
                ));
            }
        };
        let hdr = eh_frame_hdr(segments, header)?;
        let functions = functions(segments, hdr)?;
        let in_text = |function: &Function| {
            !function.is_empty() && text_start <= function.start && function.end <= text_end
        };
        if functions.iter().any(|function| {
            !function.is_empty()
                && !in_text(function)
                && function.start < text_end
                && text_start < function.end
        }) {
            return Err(CensusError(
                "an unwind-table function crosses the executable mapping",
            ));
        }
        let code_of = |start: u64, end: u64| {
            &text_code[(start - text_start) as usize..(end - text_start) as usize]
        };

        let mut sites: Vec<Site> = Vec::new();
        let mut allocation_failed = false;
        let mut summary = CensusSummary::default();
        for function in functions.iter().filter(|function| in_text(function)) {
            summary.functions += 1;
            let first = sites.len();
            let transparent = decode_function(
                code_of(function.start, function.end),
                function.start,
                &mut |instruction: &Instruction| {
                    if instruction.mnemonic() != Mnemonic::Syscall {
                        return;
                    }
                    // The installation heap aborts on a failed allocation.
                    if sites.try_reserve(1).is_err() {
                        allocation_failed = true;
                        return;
                    }
                    sites.push(Site {
                        address: instruction.ip(),
                        len: instruction.len() as u8,
                        opaque: false,
                        limit: u64::MAX,
                    });
                },
            );
            for site in &mut sites[first..] {
                site.opaque = !transparent || function.overlaps;
            }
        }
        if allocation_failed {
            return Err(ALLOCATION);
        }
        sites.sort_unstable_by_key(|site| site.address);
        // Overlapping functions can list one address twice; keep one opaque copy.
        sites.dedup_by(|later, earlier| {
            let duplicate = later.address == earlier.address;
            earlier.opaque |= duplicate;
            duplicate
        });

        let mut record = |entry: u64| {
            let mut index = sites.partition_point(|site| site.address < entry);
            while index > 0 {
                index -= 1;
                let site = &mut sites[index];
                if entry - site.address > WINDOW_LOOKAHEAD {
                    break;
                }
                site.limit = site.limit.min(entry);
            }
        };
        let mut covered = text_start;
        for function in &functions {
            record(function.start);
            if function.lsda != 0 {
                landing_pads(segments, function.lsda, function.start, &mut record)?;
            }
            if !in_text(function) {
                continue;
            }
            if covered < function.start {
                decode_gap(code_of(covered, function.start), covered, &mut record);
            }
            decode_function(
                code_of(function.start, function.end),
                function.start,
                &mut |instruction: &Instruction| instruction_entries(instruction, &mut record),
            );
            record_code_after(code_of(function.end, text_end), function.end, &mut record);
            covered = covered.max(function.end);
        }
        if covered < text_end {
            decode_gap(code_of(covered, text_end), covered, &mut record);
        }
        for segment in segments.iter().filter(|segment| !segment.executable) {
            let skip = (segment.address.wrapping_neg() & 7) as usize;
            let Some(words) = segment.bytes.get(skip..) else {
                continue;
            };
            for word in words.chunks_exact(8) {
                let value = u64::from_le_bytes(word.try_into().expect("chunks are eight bytes"));
                if text_start <= value && value < text_end {
                    record(value);
                }
            }
        }

        summary.sites = sites.len();
        summary.opaque_sites = sites.iter().filter(|site| site.opaque).count();
        Ok((Self { sites }, summary))
    }

    /// Proves that no known entry lands inside the bytes a jump patch at
    /// `address` would displace. `instructions` is the live scan from the site.
    pub(crate) fn prove(
        &self,
        address: u64,
        instructions: &[ScannedInstruction],
    ) -> Result<(), Refusal> {
        let index = self
            .sites
            .binary_search_by_key(&address, |site| site.address)
            .map_err(|_| Refusal::NotInFunction)?;
        let site = &self.sites[index];
        if site.opaque {
            return Err(Refusal::OpaqueFunction);
        }
        let first = instructions.first().ok_or(Refusal::InstructionMismatch)?;
        if first.address() != address
            || first.len() != usize::from(site.len)
            || first.instruction().mnemonic() != Mnemonic::Syscall
        {
            return Err(Refusal::InstructionMismatch);
        }
        let end = displaced_end(address, instructions).ok_or(Refusal::InstructionMismatch)?;
        if site.limit < end {
            return Err(Refusal::InteriorEntry { entry: site.limit });
        }
        Ok(())
    }
}

/// Returns the end of the instructions a jump patch at `address` displaces.
///
/// This follows `JumpPatchPlan::from_scan` in liteinst2 2032b49d: whole
/// consecutive instructions from the site until they cover
/// [`NEAR_JUMP_BYTES`].
pub(crate) fn displaced_end(address: u64, instructions: &[ScannedInstruction]) -> Option<u64> {
    let start = instructions
        .iter()
        .position(|instruction| instruction.address() == address)?;
    let mut end = address;
    for instruction in &instructions[start..] {
        if instruction.address() != end {
            return None;
        }
        end = end.checked_add(instruction.len() as u64)?;
        if end - address >= NEAR_JUMP_BYTES as u64 {
            return Some(end);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use iced_x86::Decoder;
    use iced_x86::DecoderOptions;
    use iced_x86::Mnemonic;
    use iced_x86::OpKind;
    use liteinst2::patcher::JumpPatchPlan;
    use liteinst2::patcher::WORD_PATCH_BYTES;
    use liteinst2::scanner::InstructionScanner;

    use super::Census;
    use super::Refusal;
    use super::Segment;
    use super::displaced_end;

    const BASE: u64 = 0x10_0000;
    const HEADER_BYTES: usize = 0x200;
    const TEXT: u64 = 0x1000;

    // `posix_madvise` from glibc 2.34: the `je` skips the syscall for
    // POSIX_MADV_DONTNEED and lands on the `ret` that a patch displaces.
    const MADVISE: &[u8] = &[
        0x31, 0xc0, // xor %eax,%eax
        0x83, 0xfa, 0x04, // cmp $0x4,%edx
        0x74, 0x09, // je +9, the ret
        0xb8, 0x1c, 0x00, 0x00, 0x00, // mov $0x1c,%eax
        0x0f, 0x05, // syscall
        0xf7, 0xd8, // neg %eax
        0xc3, // ret
    ];
    const MADVISE_SITE: u64 = 12;
    const MADVISE_RET: u64 = 16;

    // The same function with the `je` replaced by two `nop`s.
    const PLAIN: &[u8] = &[
        0x31, 0xc0, 0x83, 0xfa, 0x04, 0x90, 0x90, 0xb8, 0x1c, 0x00, 0x00, 0x00, 0x0f, 0x05, 0xf7,
        0xd8, 0xc3,
    ];

    struct Object {
        header: Vec<u8>,
        text: Vec<u8>,
        tables: Vec<u8>,
        data: Vec<u8>,
    }

    fn put(bytes: &mut [u8], offset: usize, value: &[u8]) {
        bytes[offset..offset + value.len()].copy_from_slice(value);
    }

    /// Lays out a minimal ELF object: a header page, a text page holding the
    /// given functions at their offsets, a table page holding `.eh_frame_hdr`,
    /// one CIE, one FDE per function and optional LSDAs, and a zeroed data
    /// page.
    ///
    /// Each function is `(offset in text, bytes, landing pad offsets)`.
    fn object(functions: &[(u64, &[u8], &[u64])], text_len: usize) -> Object {
        let tables_address = TEXT + text_len as u64;
        let mut header = vec![0; HEADER_BYTES];
        put(&mut header, 0, b"\x7fELF\x02\x01\x01");
        put(&mut header, 18, &62_u16.to_le_bytes());
        put(&mut header, 32, &64_u64.to_le_bytes());
        put(&mut header, 54, &56_u16.to_le_bytes());
        put(&mut header, 56, &2_u16.to_le_bytes());
        // PT_LOAD at offset 0 and address 0.
        put(&mut header, 64, &1_u32.to_le_bytes());
        // PT_GNU_EH_FRAME at the table page.
        put(&mut header, 120, &0x6474_e550_u32.to_le_bytes());
        put(&mut header, 120 + 16, &tables_address.to_le_bytes());

        let mut text = vec![0xcc; text_len];
        for (offset, bytes, _) in functions {
            put(&mut text, *offset as usize, bytes);
        }

        // .eh_frame_hdr: version, eh_frame_ptr (pcrel sdata4), count (udata4)
        // and table (datarel sdata4).
        let hdr_len = 12 + 8 * functions.len();
        let mut tables = vec![1, 0x1b, 0x03, 0x3b];
        tables.extend_from_slice(&(hdr_len as i32 - 4).to_le_bytes());
        tables.extend_from_slice(&(functions.len() as u32).to_le_bytes());
        let table_offset = tables.len();
        tables.resize(hdr_len, 0);
        // CIE "zLR": LSDA and FDE pointers are pcrel sdata4.
        let cie = tables.len();
        let cie_body: &[u8] = &[
            0, 0, 0, 0, 1, b'z', b'L', b'R', 0, 1, 0x78, 16, 2, 0x1b, 0x1b, 0, 0, 0,
        ];
        tables.extend_from_slice(&(cie_body.len() as u32).to_le_bytes());
        tables.extend_from_slice(cie_body);
        let mut lsdas = Vec::new();
        for (index, (offset, bytes, pads)) in functions.iter().enumerate() {
            let fde = tables.len();
            let search_entry = [
                ((TEXT + offset) as i64 - tables_address as i64) as i32,
                fde as i32,
            ];
            put(
                &mut tables,
                table_offset + 8 * index,
                &search_entry.map(i32::to_le_bytes).concat(),
            );
            tables.extend_from_slice(&20_u32.to_le_bytes());
            tables.extend_from_slice(&((fde + 4 - cie) as u32).to_le_bytes());
            let start_field = tables_address + tables.len() as u64;
            tables.extend_from_slice(
                &(((TEXT + offset) as i64 - start_field as i64) as i32).to_le_bytes(),
            );
            tables.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            tables.push(4);
            let lsda_field = tables.len();
            tables.extend_from_slice(&0_i32.to_le_bytes());
            tables.extend_from_slice(&[0, 0, 0]);
            if !pads.is_empty() {
                lsdas.push((lsda_field, *pads));
            }
        }
        for (lsda_field, pads) in lsdas {
            let lsda = tables.len();
            put(
                &mut tables,
                lsda_field,
                &((lsda - lsda_field) as i32).to_le_bytes(),
            );
            // No LPStart, no type table, uleb128 call sites.
            tables.extend_from_slice(&[0xff, 0xff, 0x01, (4 * pads.len()) as u8]);
            for pad in pads {
                tables.extend_from_slice(&[0, 1, *pad as u8, 0]);
            }
        }
        Object {
            header,
            text,
            tables,
            data: vec![0; 64],
        }
    }

    fn text_address(offset: u64) -> u64 {
        BASE + TEXT + offset
    }

    fn census(object: &Object) -> Census {
        let tables = text_address(object.text.len() as u64);
        let segments = [
            Segment {
                address: BASE,
                bytes: &object.header,
                executable: false,
            },
            Segment {
                address: text_address(0),
                bytes: &object.text,
                executable: true,
            },
            Segment {
                address: tables,
                bytes: &object.tables,
                executable: false,
            },
            Segment {
                address: tables + 0x1000,
                bytes: &object.data,
                executable: false,
            },
        ];
        let text = (text_address(0), text_address(object.text.len() as u64));
        let (census, _) = Census::build(&segments, BASE, text).unwrap();
        census
    }

    fn prove(object: &Object, census: &Census, offset: u64) -> Result<(), Refusal> {
        let scan = InstructionScanner::default()
            .scan_prefix(
                &object.text[offset as usize..],
                text_address(offset),
                WORD_PATCH_BYTES,
            )
            .unwrap();
        census.prove(text_address(offset), scan.instructions())
    }

    fn madvise_refusal() -> Result<(), Refusal> {
        Err(Refusal::InteriorEntry {
            entry: text_address(0x40 + MADVISE_RET),
        })
    }

    #[test]
    fn a_branch_into_the_displaced_bytes_refuses_the_site() {
        let object = object(&[(0x40, MADVISE, &[])], 0x100);
        assert_eq!(
            prove(&object, &census(&object), 0x40 + MADVISE_SITE),
            madvise_refusal()
        );
    }

    #[test]
    fn a_function_without_interior_entries_proves_the_site() {
        let object = object(&[(0x40, PLAIN, &[])], 0x100);
        assert_eq!(
            prove(&object, &census(&object), 0x40 + MADVISE_SITE),
            Ok(())
        );
    }

    #[test]
    fn a_branch_from_another_function_refuses_the_site() {
        // A cold fragment elsewhere jumps to the plain function's `ret`.
        let displacement = (0x40 + MADVISE_RET) as i32 - (0x80 + 5);
        let mut cold = vec![0xe9];
        cold.extend_from_slice(&displacement.to_le_bytes());
        let object = object(&[(0x40, PLAIN, &[]), (0x80, &cold, &[])], 0x100);
        assert_eq!(
            prove(&object, &census(&object), 0x40 + MADVISE_SITE),
            madvise_refusal()
        );
    }

    #[test]
    fn a_landing_pad_in_the_displaced_bytes_refuses_the_site() {
        let object = object(&[(0x40, PLAIN, &[MADVISE_RET])], 0x100);
        assert_eq!(
            prove(&object, &census(&object), 0x40 + MADVISE_SITE),
            madvise_refusal()
        );
    }

    #[test]
    fn a_code_pointer_in_data_refuses_the_site() {
        let mut object = object(&[(0x40, PLAIN, &[])], 0x100);
        let pointer = text_address(0x40 + MADVISE_RET);
        object.data[8..16].copy_from_slice(&pointer.to_le_bytes());
        assert_eq!(
            prove(&object, &census(&object), 0x40 + MADVISE_SITE),
            madvise_refusal()
        );
    }

    #[test]
    fn a_rip_relative_lea_of_the_displaced_bytes_refuses_the_site() {
        // `lea disp(%rip),%rcx; ret` in another function computes the address
        // of the plain function's `ret`.
        let displacement = (0x40 + MADVISE_RET) as i32 - (0x80 + 7);
        let mut lea = vec![0x48, 0x8d, 0x0d];
        lea.extend_from_slice(&displacement.to_le_bytes());
        lea.push(0xc3);
        let object = object(&[(0x40, PLAIN, &[]), (0x80, &lea, &[])], 0x100);
        assert_eq!(
            prove(&object, &census(&object), 0x40 + MADVISE_SITE),
            madvise_refusal()
        );
    }

    #[test]
    fn an_indirect_jump_makes_the_function_opaque() {
        let mut switch = PLAIN.to_vec();
        // Replace the two nops with `jmp *%rax`.
        switch[5..7].copy_from_slice(&[0xff, 0xe0]);
        let object = object(&[(0x40, &switch, &[])], 0x100);
        assert_eq!(
            prove(&object, &census(&object), 0x40 + MADVISE_SITE),
            Err(Refusal::OpaqueFunction)
        );
    }

    #[test]
    fn a_last_instruction_crossing_the_function_end_makes_it_opaque() {
        // The unwind entry ends after the first byte of `neg`; the rest of
        // the function follows it in the text.
        let mut object = object(&[(0x40, &PLAIN[..15], &[])], 0x100);
        object.text[0x40 + 15..0x40 + PLAIN.len()].copy_from_slice(&PLAIN[15..]);
        assert_eq!(
            prove(&object, &census(&object), 0x40 + MADVISE_SITE),
            Err(Refusal::OpaqueFunction)
        );
    }

    #[test]
    fn a_site_outside_every_function_is_refused() {
        let mut object = object(&[(0x40, PLAIN, &[])], 0x100);
        object.text[0xa0..0xa0 + PLAIN.len()].copy_from_slice(PLAIN);
        assert_eq!(
            prove(&object, &census(&object), 0xa0 + MADVISE_SITE),
            Err(Refusal::NotInFunction)
        );
    }

    #[test]
    fn a_window_may_cross_into_padding_but_not_into_code() {
        // `mov $0x3c,%eax; syscall` ends the function, so the window takes
        // three bytes after it.
        let tail: &[u8] = &[0xb8, 0x3c, 0x00, 0x00, 0x00, 0x0f, 0x05];
        let padded = object(&[(0x40, tail, &[])], 0x100);
        assert_eq!(prove(&padded, &census(&padded), 0x45), Ok(()));

        // Code without unwind information follows the function directly.
        let mut unlisted = object(&[(0x40, tail, &[])], 0x100);
        unlisted.text[0x47..0x4a].copy_from_slice(&[0x48, 0x31, 0xc0]);
        assert_eq!(
            prove(&unlisted, &census(&unlisted), 0x45),
            Err(Refusal::InteriorEntry {
                entry: text_address(0x47)
            })
        );

        // The next listed function starts directly after the syscall.
        let next = object(&[(0x40, tail, &[]), (0x47, PLAIN, &[])], 0x100);
        assert_eq!(
            prove(&next, &census(&next), 0x45),
            Err(Refusal::InteriorEntry {
                entry: text_address(0x47)
            })
        );
    }

    #[test]
    fn an_entry_at_the_displaced_end_is_allowed() {
        // The patch displaces `syscall; neg; ret` and leaves the second `ret`
        // in place, so a branch to that `ret` is safe.
        let mut function = PLAIN.to_vec();
        function.push(0xc3);
        // je +10 lands on the second `ret`.
        function[5..7].copy_from_slice(&[0x74, 0x0a]);
        let object = object(&[(0x40, &function, &[])], 0x100);
        assert_eq!(
            prove(&object, &census(&object), 0x40 + MADVISE_SITE),
            Ok(())
        );
    }

    #[test]
    fn overlapping_functions_are_opaque() {
        let object = object(&[(0x40, PLAIN, &[]), (0x48, &PLAIN[8..], &[])], 0x100);
        assert_eq!(
            prove(&object, &census(&object), 0x40 + MADVISE_SITE),
            Err(Refusal::OpaqueFunction)
        );
    }

    #[test]
    fn the_displaced_end_matches_liteinst2() {
        let scanner = InstructionScanner::default();
        for bytes in [
            &[0x0f, 0x05, 0xf7, 0xd8, 0xc3, 0x90, 0x90, 0x90][..],
            &[0x0f, 0x05, 0x48, 0x3d, 0x00, 0xf0, 0xff, 0xff, 0x77, 0x01][..],
            &[0x0f, 0x05, 0x89, 0xc6, 0x89, 0xd8, 0x90, 0x90][..],
        ] {
            let address = 0x4000;
            let scan = scanner
                .scan_prefix(bytes, address, WORD_PATCH_BYTES)
                .unwrap();
            let plan = JumpPatchPlan::from_scan(
                &scanner,
                &scan,
                scan.snapshot(),
                address,
                address,
                address + 0x100,
            )
            .unwrap();
            assert_eq!(
                displaced_end(address, scan.instructions()),
                Some(address + plan.displaced_len() as u64)
            );
        }
    }

    struct LoadedObject {
        path: String,
        /// `(start, end, executable)` for each readable mapping.
        ranges: Vec<(u64, u64, bool)>,
        header: u64,
        text: (u64, u64),
    }

    /// Finds this process's mappings of the object that maps `address`
    /// executable.
    fn loaded_object(address: u64) -> LoadedObject {
        struct Mapping<'a> {
            start: u64,
            end: u64,
            permissions: &'a [u8],
            offset: u64,
            identity: (&'a str, u64),
            path: &'a str,
        }
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let mappings = maps
            .lines()
            .filter_map(|line| {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                let (start, end) = fields.first()?.split_once('-')?;
                Some(Mapping {
                    start: u64::from_str_radix(start, 16).ok()?,
                    end: u64::from_str_radix(end, 16).ok()?,
                    permissions: fields.get(1)?.as_bytes(),
                    offset: u64::from_str_radix(fields.get(2)?, 16).ok()?,
                    identity: (fields.get(3)?, fields.get(4)?.parse().ok()?),
                    path: fields.get(5).copied().unwrap_or(""),
                })
            })
            .collect::<Vec<_>>();
        let text = mappings
            .iter()
            .find(|mapping| mapping.start <= address && address < mapping.end)
            .unwrap();
        assert_eq!(text.permissions[2], b'x');
        let object = mappings
            .iter()
            .filter(|mapping| mapping.identity == text.identity && mapping.permissions[0] == b'r')
            .collect::<Vec<_>>();
        LoadedObject {
            path: text.path.to_owned(),
            ranges: object
                .iter()
                .map(|mapping| (mapping.start, mapping.end, mapping.permissions[2] == b'x'))
                .collect(),
            header: object
                .iter()
                .find(|mapping| mapping.offset == 0)
                .unwrap()
                .start,
            text: (text.start, text.end),
        }
    }

    /// Builds the census of this process's libc and checks `posix_madvise`
    /// against a local decode of it. On glibc 2.34 its `je` enters the
    /// displaced bytes, so the site must be refused; on a libc without such a
    /// branch the test checks only that the census builds.
    #[test]
    fn the_census_of_this_process_libc_agrees_with_a_decode_of_posix_madvise() {
        // SAFETY: the name is NUL-terminated and dlsym does not retain it.
        let function =
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, b"posix_madvise\0".as_ptr().cast()) } as u64;
        assert_ne!(function, 0);
        let object = loaded_object(function);
        assert!(object.path.contains("libc"), "{}", object.path);
        let segments = object
            .ranges
            .iter()
            .map(|&(start, end, executable)| Segment {
                address: start,
                // SAFETY: each range is a live readable mapping of libc, which
                // this process never unmaps.
                bytes: unsafe {
                    core::slice::from_raw_parts(start as usize as *const u8, (end - start) as usize)
                },
                executable,
            })
            .collect::<Vec<_>>();
        let started = std::time::Instant::now();
        let (census, summary) = Census::build(&segments, object.header, object.text).unwrap();
        let elapsed = started.elapsed();
        let text = segments
            .iter()
            .find(|segment| segment.executable)
            .unwrap()
            .bytes;
        let scan_at = |address: u64| {
            InstructionScanner::default().scan_prefix(
                &text[(address - object.text.0) as usize..],
                address,
                WORD_PATCH_BYTES,
            )
        };

        // Decode posix_madvise through the first `ret` after its syscall and
        // collect its direct branch targets.
        let code = &text[(function - object.text.0) as usize..][..64];
        let mut decoder = Decoder::with_ip(64, code, function, DecoderOptions::NONE);
        let mut site = None;
        let mut targets = Vec::new();
        for instruction in &mut decoder {
            if instruction.op0_kind() == OpKind::NearBranch64 {
                targets.push(instruction.near_branch_target());
            }
            match instruction.mnemonic() {
                Mnemonic::Syscall => site = Some(instruction.ip()),
                Mnemonic::Ret if site.is_some() => break,
                _ => {}
            }
        }
        let site = site.expect("posix_madvise issues a syscall");
        let scan = scan_at(site).unwrap();
        let end = displaced_end(site, scan.instructions()).unwrap();
        let interior = targets
            .iter()
            .copied()
            .filter(|target| site < *target && *target < end)
            .min();
        let proof = census.prove(site, scan.instructions());

        let refused = census
            .sites
            .iter()
            .filter(|candidate| match scan_at(candidate.address) {
                Ok(scan) => census
                    .prove(candidate.address, scan.instructions())
                    .is_err(),
                Err(_) => true,
            })
            .count();
        eprintln!(
            "{}: {summary:?}, {refused} of {} sites refused, built in {elapsed:?}; \
             posix_madvise site at offset {:#x}: interior branch target {interior:x?}, \
             proof {proof:?}",
            object.path,
            summary.sites,
            site - object.text.0,
        );
        assert!(summary.sites >= 100, "{summary:?}");
        if let Some(entry) = interior {
            assert!(
                matches!(proof, Err(Refusal::InteriorEntry { entry: limit }) if limit <= entry),
                "{proof:?}"
            );
        }
    }
}
