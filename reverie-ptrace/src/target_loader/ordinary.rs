/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Resolve the explicit LiteInst host initializer as an ordinary ELF export.
//!
//! This policy is separate from the versioned public `dlopen` parser and does
//! not change which providers that parser accepts.

use super::*;

const INITIALIZER: &str = "reverie_liteinst_initialize_host";
const MAX_RUNTIME_FILE: usize = 64 * 1024 * 1024;
const LOAD_PAGE: u64 = 4096;

fn runtime_file_bound(length: usize) -> io::Result<()> {
    if length > MAX_RUNTIME_FILE {
        return Err(invalid("runtime exceeds file bound"));
    }
    Ok(())
}

/// An exact ordinary initializer observed in a stopped target image.
///
/// These coordinates expire when any task sharing the address space resumes,
/// just as they do for [`TargetDlopen`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetHostInitializer {
    /// Observed stopped TID.
    pub tid: i32,
    /// Kernel start ticks for that TID.
    pub start_ticks: u64,
    /// Original executable program headers.
    pub executable_phdr: u64,
    /// Runtime node in the default link map.
    pub link_map: u64,
    /// Runtime load bias.
    pub load_bias: u64,
    /// Ordinary, unversioned initializer address.
    pub address: u64,
    /// Observed backing device/inode tuple.
    pub mapping_identity: (u64, u64, u64),
}

/// Resolve the exact unversioned host initializer without running target code.
///
/// The controller supplies independently bound runtime bytes. All tasks sharing
/// this image must remain stopped. The complete provider, map, and readback
/// contract of [`resolve_dlopen`] applies; this does not authorize executing the
/// result.
pub fn resolve_host_initializer(
    task: &Stopped,
    expected_runtime: &[u8],
) -> io::Result<TargetHostInitializer> {
    let tid = task.pid().as_raw();
    let before = Snapshot::read(tid)?;
    let maps = parse_maps(&before.maps)?;
    let aux = parse_auxv(&before.auxv)?;
    let provider = parse_runtime(expected_runtime)?;
    let mut memory = Memory::new(&maps, |address, bytes: &mut [u8]| {
        let address = usize::try_from(address).map_err(|_| invalid("target address overflow"))?;
        task.read_exact(address, bytes)
            .map_err(|error| io::Error::other(format!("read target at {address:#x}: {error}")))
    });
    let resolved = resolve(&mut memory, &aux, &provider)?;
    memory.recheck()?;
    if Snapshot::read(tid)? != before {
        return Err(invalid(
            "target process, maps, auxv or mount namespace changed",
        ));
    }
    Ok(TargetHostInitializer {
        tid,
        start_ticks: before.start_ticks,
        executable_phdr: resolved.executable_phdr,
        link_map: resolved.link_map,
        load_bias: resolved.load_bias,
        address: resolved.address,
        mapping_identity: resolved.mapping_identity,
    })
}

// Linux maps PT_LOAD segments a page at a time. For the runtime parser, reject
// segments that need execute-only mappings or incompatible projections of one
// virtual page. Keep this stricter policy local so resolve_dlopen is unchanged.
fn validate_runtime_loads(headers: &[goblin::elf::ProgramHeader]) -> io::Result<()> {
    validate_loads(headers)?;
    let mut rounded_loads: Vec<(u64, u64, u32, u64, u64)> = Vec::new();
    for load in headers.iter().filter(|header| header.p_type == ph::PT_LOAD) {
        let end = add(load.p_vaddr, load.p_memsz)?;
        if load.p_flags & ph::PF_W == 0 && load.p_flags & ph::PF_R == 0
            || load.p_vaddr % LOAD_PAGE != load.p_offset % LOAD_PAGE
        {
            return Err(invalid("malformed runtime load segment"));
        }

        let page_start = load.p_vaddr & !(LOAD_PAGE - 1);
        let page_end = end
            .checked_add(LOAD_PAGE - 1)
            .map(|end| end & !(LOAD_PAGE - 1))
            .ok_or_else(|| invalid("runtime load page range overflow"))?;
        let file_memory_end = add(load.p_vaddr, load.p_filesz)?;
        let file_page_end = if load.p_filesz == 0 {
            page_start
        } else {
            file_memory_end
                .checked_add(LOAD_PAGE - 1)
                .map(|end| end & !(LOAD_PAGE - 1))
                .ok_or_else(|| invalid("runtime load page range overflow"))?
                .min(page_end)
        };
        let file_page_start = load.p_offset & !(LOAD_PAGE - 1);

        for &(prior_start, prior_end, prior_flags, prior_file_end, prior_file_start) in
            &rounded_loads
        {
            let overlap_start = page_start.max(prior_start);
            let overlap_end = page_end.min(prior_end);
            if overlap_start >= overlap_end {
                continue;
            }
            let prior_file_overlap_end = prior_file_end.clamp(overlap_start, overlap_end);
            let file_overlap_end = file_page_end.clamp(overlap_start, overlap_end);
            let file_projection_matches = file_overlap_end == overlap_start
                || add(prior_file_start, overlap_start - prior_start)?
                    == add(file_page_start, overlap_start - page_start)?;
            if prior_flags != load.p_flags
                || prior_file_overlap_end != file_overlap_end
                || !file_projection_matches
            {
                return Err(invalid("incompatible runtime load page mappings"));
            }
        }

        rounded_loads.push((
            page_start,
            page_end,
            load.p_flags,
            file_page_end,
            file_page_start,
        ));
    }
    Ok(())
}

fn parse_runtime(bytes: &[u8]) -> io::Result<Provider<'_>> {
    // The reviewed staged-release contract permits an ELF file up to 64 MiB.
    // This separate policy does not widen dlopen's 32 MiB libc/libdl limit.
    runtime_file_bound(bytes.len())?;
    let elf = Elf::parse(bytes).map_err(|_| invalid("malformed runtime ELF"))?;
    if !elf.is_64
        || !elf.little_endian
        || elf.header.e_machine != header::EM_X86_64
        || elf.header.e_type != header::ET_DYN
        || elf.header.e_ehsize != 64
        || elf.header.e_version != 1
        || elf.header.e_phentsize != 56
        || elf.program_headers.len() > 128
    {
        return Err(invalid("runtime is not supported x86-64 ELF"));
    }
    validate_runtime_loads(&elf.program_headers)?;
    for load in &elf.program_headers {
        if load.p_type == ph::PT_LOAD {
            range(bytes, load.p_offset, load.p_filesz)?;
        }
    }

    let segments: Vec<_> = elf
        .program_headers
        .iter()
        .filter(|header| header.p_type == ph::PT_DYNAMIC)
        .collect();
    if segments.len() != 1 {
        return Err(invalid("ambiguous runtime dynamic segment"));
    }
    let segment = segments[0];
    let (dynamic, dynamic_offset, dynamic_size) =
        (segment.p_vaddr, segment.p_offset, segment.p_filesz);
    if dynamic_size == 0
        || dynamic_size % 16 != 0
        || dynamic_size / 16 > MAX_DYNAMIC as u64
        || load_file_offset(&elf.program_headers, dynamic, dynamic_size, false)? != dynamic_offset
    {
        return Err(invalid("runtime dynamic layout invalid"));
    }
    range(bytes, dynamic_offset, dynamic_size)?;

    let tags = elf
        .dynamic
        .as_ref()
        .ok_or_else(|| invalid("missing runtime dynamic table"))?;
    if !tags
        .dyns
        .last()
        .is_some_and(|entry| entry.d_tag == dynamic::DT_NULL)
        || tags.info.textrel
    {
        return Err(invalid(
            "runtime has text relocation or unterminated dynamic table",
        ));
    }

    let mut unique = BTreeMap::new();
    for entry in &tags.dyns {
        if [
            dynamic::DT_STRTAB,
            dynamic::DT_STRSZ,
            dynamic::DT_SYMTAB,
            dynamic::DT_SYMENT,
            dynamic::DT_VERSYM,
            dynamic::DT_FLAGS,
            dynamic::DT_HASH,
            dynamic::DT_GNU_HASH,
        ]
        .contains(&entry.d_tag)
            && unique.insert(entry.d_tag, entry.d_val).is_some()
        {
            return Err(invalid("duplicate runtime symbol metadata"));
        }
    }
    let tag = |key| {
        unique
            .get(&key)
            .copied()
            .ok_or_else(|| invalid("missing runtime symbol metadata"))
    };
    if tag(dynamic::DT_SYMENT)? != 24
        || elf.dynsyms.is_empty()
        || elf.dynsyms.len() > 65536
        || unique.get(&dynamic::DT_FLAGS).copied().unwrap_or(0) & dynamic::DF_TEXTREL != 0
    {
        return Err(invalid("unsupported runtime symbols or text relocations"));
    }

    let strings = Provider::ro_file(
        &elf,
        bytes,
        tag(dynamic::DT_STRTAB)?,
        tag(dynamic::DT_STRSZ)?,
    )?;
    let records = Provider::ro_file(
        &elf,
        bytes,
        tag(dynamic::DT_SYMTAB)?,
        (elf.dynsyms.len() * 24) as u64,
    )?;
    let versions = unique
        .get(&dynamic::DT_VERSYM)
        .map(|address| Provider::ro_file(&elf, bytes, *address, (elf.dynsyms.len() * 2) as u64))
        .transpose()?;

    let mut selected = None;
    for (index, symbol) in elf.dynsyms.iter().enumerate() {
        let record = &records[index * 24..(index + 1) * 24];
        if symbol.st_name != u32_at(record, 0) as usize
            || symbol.st_info != record[4]
            || symbol.st_other != record[5]
            || symbol.st_shndx != u16_at(record, 6) as usize
            || symbol.st_value != u64_at(record, 8)
            || symbol.st_size != u64_at(record, 16)
        {
            return Err(invalid("runtime dynamic symbol table disagreement"));
        }
        if c_string(strings, symbol.st_name)? != INITIALIZER {
            continue;
        }
        if symbol.st_bind() != sym::STB_GLOBAL
            || symbol.st_type() != sym::STT_FUNC
            || symbol.st_other != sym::STV_DEFAULT
            || symbol.st_shndx == 0
            || symbol.st_shndx >= 0xff00
            || symbol.st_size == 0
            || symbol.st_size > 1024 * 1024
            || versions.is_some_and(|table| u16_at(table, index * 2) != 1)
        {
            return Err(invalid(
                "initializer is not an ordinary unversioned global function",
            ));
        }
        Provider::ro_file(&elf, bytes, symbol.st_value, symbol.st_size)?;
        if !elf.program_headers.iter().any(|load| {
            load.p_type == ph::PT_LOAD
                && load.p_flags == (ph::PF_R | ph::PF_X)
                && symbol.st_value >= load.p_vaddr
                && add(symbol.st_value, symbol.st_size)
                    .ok()
                    .zip(add(load.p_vaddr, load.p_filesz).ok())
                    .is_some_and(|(symbol_end, load_end)| symbol_end <= load_end)
        }) {
            return Err(invalid("initializer is outside exact executable bytes"));
        }
        if selected.replace(symbol).is_some() {
            return Err(invalid("ambiguous ordinary initializer"));
        }
    }

    Ok(Provider {
        bytes,
        elf,
        symbol: selected.ok_or_else(|| invalid("ordinary initializer absent"))?,
        version: "unversioned",
        dynamic,
        dynamic_offset,
        dynamic_size,
    })
}

#[cfg(test)]
mod tests;
