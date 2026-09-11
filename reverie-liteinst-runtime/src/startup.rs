//! Checked, non-executing plans for replacing a controlled ELF interpreter.
//!
//! This first layout supports ELF64 x86-64, 4 KiB pages, zero-based ET_DYN
//! interpreters and nonoverlapping load pages in the lower 48-bit user range.
//! Other Linux layouts are refused, not declared invalid Linux executables.
//! Zero-fill is supported only in the final, writable non-executable load.
//! No function maps memory, changes auxv/brk, executes an interpreter or issues
//! an installation/ownership token. Supplied bytes, addresses and reservations
//! are planning inputs, not authenticated observations of a kernel exec.
//!
//! A future launcher must bind these bytes to the actual original executable
//! and interpreter descriptors, own the complete reservation, supply all other
//! live mappings, and preserve the original stack, auxv and program break. It
//! must separately establish runtime/TLS, signal, timer and Tool lifetime safety.
//! These allocating APIs are for host-side preparation or an already initialized
//! tool runtime, not for a freestanding entry before allocator/TLS readiness.

use std::fmt;
use std::ops::Range;

pub mod original_interpreter;

const PAGE: u64 = 4096;
const USER_END: u64 = 1 << 47;
const ELF_HEADER: usize = 64;
const PROGRAM_HEADER: usize = 56;
const REQUIRED_AUXV: [u64; 9] = [3, 4, 5, 6, 7, 9, 25, 31, 33];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanError {
    Truncated(&'static str),
    Invalid(&'static str),
    Unsupported(&'static str),
    Overflow(&'static str),
    Overlap(&'static str),
    MissingAuxv(u64),
    DuplicateAuxv(u64),
}

impl fmt::Display for PlanError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated(field) => write!(output, "truncated {field}"),
            Self::Invalid(field) => write!(output, "invalid {field}"),
            Self::Unsupported(field) => write!(output, "unsupported {field}"),
            Self::Overflow(field) => write!(output, "overflow in {field}"),
            Self::Overlap(field) => write!(output, "overlap with {field}"),
            Self::MissingAuxv(tag) => write!(output, "missing auxv tag {tag}"),
            Self::DuplicateAuxv(tag) => write!(output, "duplicate auxv tag {tag}"),
        }
    }
}

impl std::error::Error for PlanError {}

/// An immutable copy of a complete auxv table, including unknown entries.
/// `stack` and `brk` must eventually come from the actual exec owner. Parsing
/// validates coordinates only; it does not read pointed-to memory or certify
/// kernel provenance, string termination, timer history or ownership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuxvSnapshot {
    bytes: Box<[u8]>,
    stack: Range<u64>,
    brk: u64,
    base: u64,
    program_headers: Range<u64>,
    guest_entry: u64,
    random: Range<u64>,
    execfn: u64,
    vdso: u64,
}

impl AuxvSnapshot {
    pub fn parse(bytes: &[u8], stack: Range<u64>, brk: u64) -> Result<Self, PlanError> {
        user_range(&stack, "initial stack")?;
        user_range(&span(brk, 1, "initial brk")?, "initial brk")?;
        if bytes.is_empty() || !bytes.len().is_multiple_of(16) {
            return Err(PlanError::Truncated("auxv"));
        }
        let mut values = [None; REQUIRED_AUXV.len()];
        let mut terminated = false;
        for (index, entry) in bytes.as_chunks::<16>().0.iter().enumerate() {
            let tag = u64::from_le_bytes(field(entry, 0)?);
            let value = u64::from_le_bytes(field(entry, 8)?);
            if tag == 0 {
                if value != 0 || index + 1 != bytes.len() / 16 {
                    return Err(PlanError::Invalid("auxv terminator"));
                }
                terminated = true;
            } else if let Some(slot) = REQUIRED_AUXV.iter().position(|required| *required == tag)
                && values[slot].replace(value).is_some()
            {
                return Err(PlanError::DuplicateAuxv(tag));
            }
        }
        if !terminated {
            return Err(PlanError::Truncated("auxv terminator"));
        }
        for (tag, value) in REQUIRED_AUXV.into_iter().zip(values) {
            if value.is_none() {
                return Err(PlanError::MissingAuxv(tag));
            }
        }
        let [
            phdr,
            phent,
            phnum,
            pagesz,
            base,
            guest_entry,
            random,
            execfn,
            vdso,
        ] = values.map(Option::unwrap);
        if phent != PROGRAM_HEADER as u64 || phnum == 0 {
            return Err(PlanError::Invalid("guest program headers"));
        }
        if pagesz != PAGE {
            return Err(PlanError::Unsupported("AT_PAGESZ"));
        }
        let program_headers = span(
            phdr,
            phnum
                .checked_mul(phent)
                .ok_or(PlanError::Overflow("AT_PHNUM"))?,
            "guest program headers",
        )?;
        user_range(&program_headers, "guest program headers")?;
        for (address, name) in [(base, "AT_BASE"), (guest_entry, "AT_ENTRY"), (vdso, "vDSO")] {
            user_range(&span(address, 1, name)?, name)?;
        }
        if !base.is_multiple_of(PAGE) || !vdso.is_multiple_of(PAGE) {
            return Err(PlanError::Invalid("auxv mapping alignment"));
        }
        let random = span(random, 16, "AT_RANDOM")?;
        if !contains(&stack, &random) || !contains(&stack, &span(execfn, 1, "AT_EXECFN")?) {
            return Err(PlanError::Invalid("stack auxv pointers"));
        }
        Ok(Self {
            bytes: bytes.into(),
            stack,
            brk,
            base,
            program_headers,
            guest_entry,
            random,
            execfn,
            vdso,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn stack(&self) -> &Range<u64> {
        &self.stack
    }
    pub fn brk(&self) -> u64 {
        self.brk
    }
    pub fn base(&self) -> u64 {
        self.base
    }
    pub fn program_headers(&self) -> &Range<u64> {
        &self.program_headers
    }
    pub fn guest_entry(&self) -> u64 {
        self.guest_entry
    }
    pub fn random(&self) -> &Range<u64> {
        &self.random
    }
    pub fn execfn(&self) -> u64 {
        self.execfn
    }
    pub fn vdso(&self) -> u64 {
        self.vdso
    }
}

#[derive(Debug)]
struct Load {
    offset: u64,
    address: u64,
    file_size: u64,
    memory_size: u64,
    flags: u32,
}

/// A checked interpreter layout, not a relocation or instruction-semantics audit.
/// GNU properties, TLS/CRT behavior and dynamic relocations still need validation
/// before an actual launcher may use the image. The bytes remain borrowed.
#[derive(Debug)]
pub struct InterpreterImage<'a> {
    bytes: &'a [u8],
    loads: Vec<Load>,
    end: u64,
    entry: u64,
    alignment: u64,
}

impl<'a> InterpreterImage<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, PlanError> {
        if bytes.len() < ELF_HEADER {
            return Err(PlanError::Truncated("ELF header"));
        }
        if &bytes[..4] != b"\x7fELF" {
            return Err(PlanError::Invalid("ELF magic"));
        }
        if bytes[4..7] != [2, 1, 1]
            || !matches!(bytes[7], 0 | 3)
            || bytes[8] != 0
            || u16::from_le_bytes(field(bytes, 16)?) != 3
            || u16::from_le_bytes(field(bytes, 18)?) != 62
            || u32::from_le_bytes(field(bytes, 20)?) != 1
            || u32::from_le_bytes(field(bytes, 48)?) != 0
        {
            return Err(PlanError::Unsupported("ELF64 x86-64 ET_DYN ABI"));
        }
        if u16::from_le_bytes(field(bytes, 52)?) as usize != ELF_HEADER
            || u16::from_le_bytes(field(bytes, 54)?) as usize != PROGRAM_HEADER
        {
            return Err(PlanError::Invalid("ELF header sizes"));
        }
        let count = u16::from_le_bytes(field(bytes, 56)?);
        if count == 0 || count == u16::MAX {
            return Err(PlanError::Unsupported("ELF program header count"));
        }
        let offset = u64::from_le_bytes(field(bytes, 32)?);
        if offset < ELF_HEADER as u64 {
            return Err(PlanError::Invalid("program header offset"));
        }
        let table = span(
            offset,
            u64::from(count) * PROGRAM_HEADER as u64,
            "program header table",
        )?;
        let headers = file_range(bytes, &table, "program header table")?;
        let entry = u64::from_le_bytes(field(bytes, 24)?);
        let mut loads = Vec::new();
        let mut end = 0;
        let mut alignment = PAGE;
        for header in headers.as_chunks::<PROGRAM_HEADER>().0 {
            let kind = u32::from_le_bytes(field(header, 0)?);
            if kind == 3 {
                return Err(PlanError::Unsupported("nested PT_INTERP"));
            }
            if kind != 1 {
                continue;
            }
            let flags = u32::from_le_bytes(field(header, 4)?);
            let offset = u64::from_le_bytes(field(header, 8)?);
            let address = u64::from_le_bytes(field(header, 16)?);
            let file_size = u64::from_le_bytes(field(header, 32)?);
            let memory_size = u64::from_le_bytes(field(header, 40)?);
            let align = u64::from_le_bytes(field(header, 48)?);
            if flags & !7 != 0 || flags & 4 == 0 || flags & 3 == 3 {
                return Err(PlanError::Unsupported("load permissions"));
            }
            if (align > 1 && (!align.is_power_of_two() || address % align != offset % align))
                || address % PAGE != offset % PAGE
            {
                return Err(PlanError::Invalid("load alignment"));
            }
            alignment = alignment.max(align);
            if file_size > memory_size {
                return Err(PlanError::Invalid("load file size exceeds memory size"));
            }
            file_range(bytes, &span(offset, file_size, "load file")?, "load file")?;
            let memory = span(address, memory_size, "load memory")?;
            if memory_size == 0 {
                continue;
            }
            let pages = page_floor(address)..page_ceil(memory.end)?;
            if loads.is_empty() && (address != 0 || offset != 0) {
                return Err(PlanError::Unsupported("nonzero first load base"));
            }
            if pages.start < end {
                return Err(PlanError::Unsupported(
                    "overlapping or unordered load pages",
                ));
            }
            end = pages.end;
            loads.push(Load {
                offset,
                address,
                file_size,
                memory_size,
                flags,
            });
        }
        if loads.first().is_none_or(|load| load.file_size < table.end) {
            return Err(PlanError::Unsupported("unmapped interpreter headers"));
        }
        for (index, load) in loads.iter().enumerate() {
            if load.file_size < load.memory_size && (index + 1 != loads.len() || load.flags != 6) {
                return Err(PlanError::Unsupported("BSS outside final writable load"));
            }
        }
        if !loads.iter().any(|load| {
            load.flags & 1 != 0 && entry >= load.address && entry - load.address < load.file_size
        }) {
            return Err(PlanError::Invalid(
                "entry outside file-backed executable load",
            ));
        }
        Ok(Self {
            bytes,
            loads,
            end,
            entry,
            alignment,
        })
    }

    pub fn required_span(&self) -> u64 {
        self.end
    }

    pub fn required_alignment(&self) -> u64 {
        self.alignment
    }

    /// Plan at the unchanged AT_BASE, refusing overlap with the original stack,
    /// main PHDR/ENTRY, brk, vDSO base page and all supplied other mappings.
    /// The future exec owner must supply the *complete* other-mapping inventory
    /// and prove exclusive ownership of `reservation`; success proves neither.
    pub fn plan_at_original_base(
        &self,
        initial: &AuxvSnapshot,
        reservation: Range<u64>,
        other_mappings: &[Range<u64>],
    ) -> Result<InterpreterPlan<'a>, PlanError> {
        user_range(&reservation, "interpreter reservation")?;
        if reservation.start != initial.base
            || !reservation.start.is_multiple_of(self.alignment)
            || !reservation.end.is_multiple_of(PAGE)
        {
            return Err(PlanError::Invalid("AT_BASE reservation alignment"));
        }
        let required = span(initial.base, self.end, "interpreter mapping")?;
        if !contains(&reservation, &required) {
            return Err(PlanError::Invalid("interpreter reservation too small"));
        }
        for (protected, name) in [
            (initial.stack.clone(), "initial stack"),
            (initial.program_headers.clone(), "guest program headers"),
            (span(initial.guest_entry, 1, "AT_ENTRY")?, "guest entry"),
            (span(initial.brk, 1, "initial brk")?, "initial brk"),
            (span(initial.vdso, PAGE, "vDSO")?, "vDSO"),
        ] {
            if overlaps(&reservation, &protected) {
                return Err(PlanError::Overlap(name));
            }
        }
        for range in other_mappings {
            user_range(range, "other mapping")?;
            if overlaps(&reservation, range) {
                return Err(PlanError::Overlap("other mapping"));
            }
        }
        let mut segments = Vec::new();
        let mut gaps = Vec::new();
        let mut cursor = initial.base;
        for load in &self.loads {
            let start = initial
                .base
                .checked_add(load.address)
                .ok_or(PlanError::Overflow("load bias"))?;
            let memory = span(start, load.memory_size, "biased load")?;
            let pages = page_floor(start)..page_ceil(memory.end)?;
            let data_end = start
                .checked_add(load.file_size)
                .ok_or(PlanError::Overflow("file end"))?;
            let file_pages = if load.file_size == 0 && start == pages.start {
                None
            } else {
                Some((page_floor(load.offset), pages.start..page_ceil(data_end)?))
            };
            let anonymous_start = file_pages
                .as_ref()
                .map_or(pages.start, |(_, range)| range.end);
            if cursor < pages.start {
                gaps.push(cursor..pages.start);
            }
            cursor = pages.end;
            segments.push(LoadMapping {
                pages: pages.clone(),
                file_pages,
                anonymous_pages: anonymous_start..pages.end,
                zero_fill: data_end..if load.file_size < load.memory_size {
                    pages.end
                } else {
                    data_end
                },
                memory,
                flags: load.flags,
            });
        }
        if cursor < reservation.end {
            gaps.push(cursor..reservation.end);
        }
        Ok(InterpreterPlan {
            image: self.bytes,
            initial: initial.clone(),
            reservation,
            segments,
            gaps,
            entry: initial
                .base
                .checked_add(self.entry)
                .ok_or(PlanError::Overflow("interpreter entry"))?,
        })
    }
}

/// Coordinates only. A file mapping's final partial page may extend past EOF;
/// its first page retains the file prefix even when p_filesz is zero but
/// p_vaddr is unaligned. Mapping must be private, never modify the original file.
/// zero_fill starts at p_filesz, not the page boundary, and includes the final
/// BSS page's tail, as Linux padzero does. Full BSS pages are anonymous. A future
/// mapper must initialize bytes before applying final ELF
/// flags and must not introduce a writable-executable mapping to do so.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadMapping {
    pages: Range<u64>,
    memory: Range<u64>,
    file_pages: Option<(u64, Range<u64>)>,
    anonymous_pages: Range<u64>,
    zero_fill: Range<u64>,
    flags: u32,
}

impl LoadMapping {
    pub fn pages(&self) -> &Range<u64> {
        &self.pages
    }
    pub fn memory(&self) -> &Range<u64> {
        &self.memory
    }
    pub fn file_pages(&self) -> Option<&(u64, Range<u64>)> {
        self.file_pages.as_ref()
    }
    pub fn anonymous_pages(&self) -> &Range<u64> {
        &self.anonymous_pages
    }
    pub fn zero_fill(&self) -> &Range<u64> {
        &self.zero_fill
    }
    pub fn flags(&self) -> u32 {
        self.flags
    }
}

#[derive(Debug)]
pub struct InterpreterPlan<'a> {
    image: &'a [u8],
    initial: AuxvSnapshot,
    reservation: Range<u64>,
    segments: Vec<LoadMapping>,
    gaps: Vec<Range<u64>>,
    entry: u64,
}

impl<'a> InterpreterPlan<'a> {
    pub fn image(&self) -> &'a [u8] {
        self.image
    }
    pub fn initial(&self) -> &AuxvSnapshot {
        &self.initial
    }
    pub fn reservation(&self) -> &Range<u64> {
        &self.reservation
    }
    pub fn segments(&self) -> &[LoadMapping] {
        &self.segments
    }
    pub fn gaps(&self) -> &[Range<u64>] {
        &self.gaps
    }
    pub fn interpreter_entry(&self) -> u64 {
        self.entry
    }
}

fn field<const WIDTH: usize>(bytes: &[u8], offset: usize) -> Result<[u8; WIDTH], PlanError> {
    let end = offset
        .checked_add(WIDTH)
        .ok_or(PlanError::Overflow("field offset"))?;
    bytes
        .get(offset..end)
        .ok_or(PlanError::Truncated("field"))?
        .try_into()
        .map_err(|_| PlanError::Truncated("field"))
}

fn span(start: u64, size: u64, name: &'static str) -> Result<Range<u64>, PlanError> {
    Ok(start..start.checked_add(size).ok_or(PlanError::Overflow(name))?)
}

fn user_range(range: &Range<u64>, name: &'static str) -> Result<(), PlanError> {
    if range.start == 0 || range.start >= range.end || range.end > USER_END {
        return Err(PlanError::Invalid(name));
    }
    Ok(())
}

fn file_range<'a>(
    bytes: &'a [u8],
    range: &Range<u64>,
    name: &'static str,
) -> Result<&'a [u8], PlanError> {
    let start = usize::try_from(range.start).map_err(|_| PlanError::Overflow(name))?;
    let end = usize::try_from(range.end).map_err(|_| PlanError::Overflow(name))?;
    bytes.get(start..end).ok_or(PlanError::Truncated(name))
}

fn page_floor(value: u64) -> u64 {
    value & !(PAGE - 1)
}
fn page_ceil(value: u64) -> Result<u64, PlanError> {
    Ok(page_floor(
        value
            .checked_add(PAGE - 1)
            .ok_or(PlanError::Overflow("page rounding"))?,
    ))
}
fn contains(outer: &Range<u64>, inner: &Range<u64>) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}
fn overlaps(left: &Range<u64>, right: &Range<u64>) -> bool {
    left.start < right.end && right.start < left.end
}

#[cfg(test)]
mod tests;
