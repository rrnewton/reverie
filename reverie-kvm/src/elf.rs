/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use goblin::elf::Elf;
use goblin::elf::header::EI_CLASS;
use goblin::elf::header::EI_DATA;
use goblin::elf::header::ELFCLASS64;
use goblin::elf::header::ELFDATA2LSB;
use goblin::elf::header::EM_X86_64;
use goblin::elf::header::ET_EXEC;
use goblin::elf::program_header::PF_X;
use goblin::elf::program_header::PT_INTERP;
use goblin::elf::program_header::PT_LOAD;

use crate::Error;
use crate::GuestMemory;
use crate::Result;
use crate::bootstrap::BOOT_RESERVED_END;
use crate::bootstrap::PROGRAM_HEADERS_ADDRESS;

const PAGE_SIZE: u64 = 4096;
const STACK_RESERVE: u64 = 1024 * 1024;
const MMAP_GAP: u64 = 1024 * 1024;
const MAX_PROGRAM_HEADERS_SIZE: usize = PAGE_SIZE as usize;

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
const AT_EXECFN: u64 = 31;

#[derive(Clone, Debug)]
pub(crate) struct LoadedStaticElf {
    pub entry_point: u64,
    pub stack_pointer: u64,
    pub program_break: u64,
    pub mmap_next: u64,
    pub mmap_limit: u64,
    pub argv0: Vec<u8>,
    pub fs_base: u64,
    pub gs_base: u64,
}

pub(crate) fn load_static_elf(
    memory: &mut GuestMemory,
    image: &[u8],
    argv: &[&str],
    envp: &[&str],
) -> Result<LoadedStaticElf> {
    let elf = Elf::parse(image)?;
    validate_elf(&elf)?;

    let argv0 = *argv
        .first()
        .ok_or_else(|| Error::UnsupportedElf("argv must contain at least argv[0]".to_string()))?;
    for entry in argv.iter().chain(envp.iter()) {
        if entry.as_bytes().contains(&0) {
            return Err(Error::UnsupportedElf(
                "an argv/envp entry contains an embedded NUL byte".to_string(),
            ));
        }
    }

    let mut image_end = 0;
    let mut entry_is_executable = false;
    for header in elf
        .program_headers
        .iter()
        .filter(|header| header.p_type == PT_LOAD)
    {
        if header.p_filesz > header.p_memsz {
            return Err(Error::UnsupportedElf(format!(
                "PT_LOAD filesz {:#x} exceeds memsz {:#x}",
                header.p_filesz, header.p_memsz
            )));
        }

        let segment_end = header
            .p_vaddr
            .checked_add(header.p_memsz)
            .ok_or_else(|| Error::UnsupportedElf("PT_LOAD address overflow".to_string()))?;
        if header.p_vaddr < BOOT_RESERVED_END && segment_end > 0 {
            return Err(Error::UnsupportedElf(format!(
                "PT_LOAD {:#x}..{segment_end:#x} overlaps bootstrap memory",
                header.p_vaddr
            )));
        }

        let file_start = usize::try_from(header.p_offset)
            .map_err(|_| Error::UnsupportedElf("PT_LOAD offset is too large".to_string()))?;
        let file_size = usize::try_from(header.p_filesz)
            .map_err(|_| Error::UnsupportedElf("PT_LOAD filesz is too large".to_string()))?;
        let file_end = file_start
            .checked_add(file_size)
            .ok_or_else(|| Error::UnsupportedElf("PT_LOAD file range overflow".to_string()))?;
        let contents = image.get(file_start..file_end).ok_or_else(|| {
            Error::UnsupportedElf("PT_LOAD extends past the ELF image".to_string())
        })?;

        memory.write(header.p_vaddr, contents)?;
        let zero_start = header.p_vaddr + header.p_filesz;
        let zero_len = usize::try_from(header.p_memsz - header.p_filesz)
            .map_err(|_| Error::UnsupportedElf("PT_LOAD memsz is too large".to_string()))?;
        memory.zero(zero_start, zero_len)?;

        entry_is_executable |=
            header.p_flags & PF_X != 0 && (header.p_vaddr..segment_end).contains(&elf.entry);
        image_end = image_end.max(segment_end);
    }

    if !entry_is_executable {
        return Err(Error::UnsupportedElf(
            "entry point is not inside an executable PT_LOAD segment".to_string(),
        ));
    }

    copy_program_headers(memory, image, &elf)?;
    let stack_pointer = build_initial_stack(memory, &elf, argv, envp)?;
    let program_break = align_up(image_end, PAGE_SIZE)?;
    let mmap_next = align_up(
        program_break
            .checked_add(MMAP_GAP)
            .ok_or_else(|| Error::UnsupportedElf("initial mmap base overflow".to_string()))?,
        PAGE_SIZE,
    )?;
    let mmap_limit = memory
        .guest_end()
        .checked_sub(STACK_RESERVE)
        .ok_or(Error::LongModeMemoryTooSmall)?;
    if mmap_next >= mmap_limit {
        return Err(Error::LongModeMemoryTooSmall);
    }

    Ok(LoadedStaticElf {
        entry_point: elf.entry,
        stack_pointer,
        program_break,
        mmap_next,
        mmap_limit,
        argv0: argv0.as_bytes().to_vec(),
        fs_base: 0,
        gs_base: 0,
    })
}

fn validate_elf(elf: &Elf<'_>) -> Result<()> {
    if elf.header.e_ident[EI_CLASS] != ELFCLASS64
        || elf.header.e_ident[EI_DATA] != ELFDATA2LSB
        || elf.header.e_machine != EM_X86_64
    {
        return Err(Error::UnsupportedElf(
            "expected a little-endian ELF64 x86-64 image".to_string(),
        ));
    }
    if elf.header.e_type != ET_EXEC {
        return Err(Error::UnsupportedElf(
            "only fixed-address ET_EXEC images are supported".to_string(),
        ));
    }
    if elf
        .program_headers
        .iter()
        .any(|header| header.p_type == PT_INTERP)
    {
        return Err(Error::UnsupportedElf(
            "PT_INTERP requires a dynamic linker".to_string(),
        ));
    }
    if !elf
        .program_headers
        .iter()
        .any(|header| header.p_type == PT_LOAD)
    {
        return Err(Error::UnsupportedElf(
            "image contains no PT_LOAD segments".to_string(),
        ));
    }
    Ok(())
}

fn copy_program_headers(memory: &mut GuestMemory, image: &[u8], elf: &Elf<'_>) -> Result<()> {
    let start = usize::try_from(elf.header.e_phoff)
        .map_err(|_| Error::UnsupportedElf("program-header offset is too large".to_string()))?;
    let size = usize::from(elf.header.e_phentsize)
        .checked_mul(usize::from(elf.header.e_phnum))
        .ok_or_else(|| Error::UnsupportedElf("program-header size overflow".to_string()))?;
    if size > MAX_PROGRAM_HEADERS_SIZE {
        return Err(Error::UnsupportedElf(
            "program-header table exceeds one page".to_string(),
        ));
    }
    let end = start
        .checked_add(size)
        .ok_or_else(|| Error::UnsupportedElf("program-header range overflow".to_string()))?;
    let headers = image.get(start..end).ok_or_else(|| {
        Error::UnsupportedElf("program-header table extends past the image".to_string())
    })?;
    memory.write(PROGRAM_HEADERS_ADDRESS, headers)
}

fn build_initial_stack(
    memory: &mut GuestMemory,
    elf: &Elf<'_>,
    argv: &[&str],
    envp: &[&str],
) -> Result<u64> {
    // Strings (argv[], envp[], the AT_RANDOM bytes) live in a high region that
    // grows downward from the top of guest memory; the pointer arrays and auxv
    // that reference them are written lower, at the final `rsp`.
    let mut cursor = memory.guest_end().saturating_sub(16);

    // Push argv/envp strings, recording each guest address. argv[0] is first.
    let mut arg_addresses = Vec::with_capacity(argv.len());
    for arg in argv {
        cursor = push_c_string(memory, cursor, arg.as_bytes())?;
        arg_addresses.push(cursor);
    }
    let mut env_addresses = Vec::with_capacity(envp.len());
    for entry in envp {
        cursor = push_c_string(memory, cursor, entry.as_bytes())?;
        env_addresses.push(cursor);
    }
    let argv0_address = arg_addresses[0];

    let random = [
        0x52, 0x65, 0x76, 0x65, 0x72, 0x69, 0x65, 0x2d, 0x4b, 0x56, 0x4d, 0x2d, 0x45, 0x4c, 0x46,
        0x21,
    ];
    cursor = cursor
        .checked_sub(random.len() as u64)
        .ok_or(Error::LongModeMemoryTooSmall)?;
    memory.write(cursor, &random)?;
    let random_address = cursor;

    // Build the SysV initial stack image, low to high:
    //   argc, argv[0..], NULL, envp[0..], NULL, auxv pairs.., AT_NULL/0
    let mut words: Vec<u64> = Vec::new();
    words.push(argv.len() as u64);
    words.extend_from_slice(&arg_addresses);
    words.push(0);
    words.extend_from_slice(&env_addresses);
    words.push(0);
    words.extend_from_slice(&[
        AT_PHDR,
        PROGRAM_HEADERS_ADDRESS,
        AT_PHENT,
        u64::from(elf.header.e_phentsize),
        AT_PHNUM,
        u64::from(elf.header.e_phnum),
        AT_PAGESZ,
        PAGE_SIZE,
        AT_BASE,
        0,
        AT_ENTRY,
        elf.entry,
        AT_UID,
        0,
        AT_EUID,
        0,
        AT_GID,
        0,
        AT_EGID,
        0,
        AT_SECURE,
        0,
        AT_RANDOM,
        random_address,
        AT_EXECFN,
        argv0_address,
        AT_NULL,
        0,
    ]);

    let stack_size = (words.len() * std::mem::size_of::<u64>()) as u64;
    // The kernel enters `_start` with `%rsp` 16-byte aligned and argc at [rsp].
    cursor = cursor
        .checked_sub(stack_size)
        .ok_or(Error::LongModeMemoryTooSmall)?
        & !0xf;
    if cursor < memory.guest_end().saturating_sub(STACK_RESERVE) {
        return Err(Error::LongModeMemoryTooSmall);
    }

    let mut stack = Vec::with_capacity(stack_size as usize);
    for word in words {
        stack.extend_from_slice(&word.to_le_bytes());
    }
    memory.write(cursor, &stack)?;
    Ok(cursor)
}

/// Writes a NUL-terminated copy of `bytes` ending just below `cursor` and
/// returns the guest address of the first byte (the new, lower cursor).
fn push_c_string(memory: &mut GuestMemory, cursor: u64, bytes: &[u8]) -> Result<u64> {
    let start = cursor
        .checked_sub((bytes.len() + 1) as u64)
        .ok_or(Error::LongModeMemoryTooSmall)?;
    memory.write(start, bytes)?;
    memory.write(start + bytes.len() as u64, &[0])?;
    Ok(start)
}

fn align_up(value: u64, alignment: u64) -> Result<u64> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| Error::UnsupportedElf("address alignment overflow".to_string()))
}
