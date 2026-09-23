/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Resolve a public target loader entry without executing target code.
//!
//! This validates a caller-supplied expected provider against readable target
//! mappings and the original loader's default-namespace link map. It does not
//! select trusted provider bytes, load a runtime, or authorize a later call.

use std::collections::BTreeMap;
use std::io::Read;
use std::io::{self};
use std::os::unix::fs::MetadataExt;

use goblin::elf::Elf;
use goblin::elf::dynamic;
use goblin::elf::header;
use goblin::elf::program_header as ph;
use goblin::elf::sym;
use reverie::syscalls::MemoryAccess;
use safeptrace::Stopped;

mod ordinary;
pub use ordinary::TargetHostInitializer;
pub use ordinary::resolve_host_initializer;

const MAX_FILE: usize = 32 * 1024 * 1024;
const MAX_MAPS: usize = 2 * 1024 * 1024;
const MAX_READ: usize = 8 * 1024 * 1024;
const MAX_LINKS: usize = 256;
const MAX_DYNAMIC: usize = 4096;

/// A public `dlopen` definition in one observed target image.
///
/// These coordinates expire when any task sharing this address space resumes.
/// They are not a capability to execute the address or a loader-state transfer.
#[derive(Debug, Eq, PartialEq)]
pub struct TargetDlopen {
    /// Target thread ID of the stopped-task observation.
    pub tid: i32,
    /// Kernel start ticks of that target thread.
    pub start_ticks: u64,
    /// Address of the original executable's program headers.
    pub executable_phdr: u64,
    /// Provider node in the default-namespace link map.
    pub link_map: u64,
    /// Difference between provider ELF virtual addresses and target addresses.
    pub load_bias: u64,
    /// Validated executable entry address of the public function.
    pub address: u64,
    /// Exact requested default symbol version.
    pub version: String,
    /// Backing-file device and inode as reported by the target's maps.
    /// This does not assert equality with a pathname's `stat` identity.
    pub mapping_identity: (u64, u64, u64),
}

/// Resolve the expected provider's default-version public `dlopen` in `task`.
///
/// `expected_provider` must be independently bound libc/libdl ELF bytes chosen
/// by the controller; neither a guest pathname nor an observed SONAME establishes
/// trust. `version` must name its intended public default version. This compares
/// all non-writable PT_LOAD file bytes, including the selected symbol/version
/// metadata and code, with the actual target. Writable data/TLS is not compared.
///
/// The caller must keep **all** tasks sharing this address space quiescent for
/// the entire observation. `Stopped` owns one TID, not that wider condition.
/// Before/after reads detect changes but do not replace quiescence. No target
/// code runs. All coordinates require renewed validation after any resume/exec.
/// The loader's writable rendezvous/link-map records are structural observations,
/// not a defense against target tampering. This must run before provider code
/// instrumentation. The selected provider's code must match without exceptions.
/// The provider's complete program-header table must be mapped at its ELF file
/// offset relative to the initial load containing the ELF header.
///
/// Only x86-64 little-endian ELF, an executable PT_PHDR/DT_DEBUG rendezvous,
/// consistent default-namespace link maps and a unique normal function are
/// accepted. IFUNCs, private/non-default versions, interposed provider selection
/// and loader/audit callbacks are not resolved by this operation.
pub fn resolve_dlopen(
    task: &Stopped,
    expected_provider: &[u8],
    version: &str,
) -> io::Result<TargetDlopen> {
    let tid = task.pid().as_raw();
    resolve_dlopen_observed(
        tid,
        expected_provider,
        version,
        || Snapshot::read(tid),
        |address, bytes: &mut [u8]| {
            let address =
                usize::try_from(address).map_err(|_| invalid("target address overflow"))?;
            task.read_exact(address, bytes)
                .map_err(|error| io::Error::other(format!("read target at {address:#x}: {error}")))
        },
    )
}

pub(crate) fn validate_runtime_init_images(
    provider: &[u8],
    version: &str,
    runtime: &[u8],
) -> io::Result<()> {
    let provider = Provider::parse(provider, version)?;
    if provider.elf.soname != Some("libc.so.6") {
        return Err(invalid(
            "runtime initialization requires a bound libc dlopen provider",
        ));
    }
    ordinary::validate_runtime_init_image(runtime)
}

fn resolve_dlopen_observed<
    S: FnMut() -> io::Result<Snapshot>,
    F: FnMut(u64, &mut [u8]) -> io::Result<()>,
>(
    tid: i32,
    expected_provider: &[u8],
    version: &str,
    mut snapshot: S,
    read_memory: F,
) -> io::Result<TargetDlopen> {
    let before = snapshot()?;
    let maps = parse_maps(&before.maps)?;
    let aux = parse_auxv(&before.auxv)?;
    let provider = Provider::parse(expected_provider, version)?;
    let mut memory = Memory::new(&maps, read_memory);
    let mut result = resolve(&mut memory, &aux, &provider)?;
    if snapshot()? != before {
        return Err(invalid(
            "target process, maps, auxv or mount namespace changed",
        ));
    }
    result.tid = tid;
    result.start_ticks = before.start_ticks;
    Ok(result)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn add(a: u64, b: u64) -> io::Result<u64> {
    a.checked_add(b).ok_or_else(|| invalid("address overflow"))
}
fn range(bytes: &[u8], offset: u64, length: u64) -> io::Result<&[u8]> {
    let end = add(offset, length)?;
    bytes
        .get(
            usize::try_from(offset).map_err(|_| invalid("offset overflow"))?
                ..usize::try_from(end).map_err(|_| invalid("offset overflow"))?,
        )
        .ok_or_else(|| invalid("truncated ELF data"))
}
fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    start_ticks: u64,
    exe: (u64, u64),
    mount_namespace: (u64, u64),
    maps: Vec<u8>,
    auxv: Vec<u8>,
}
fn bounded_file(path: &str, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid("proc file exceeds bound"));
    }
    Ok(bytes)
}
impl Snapshot {
    fn read(tid: i32) -> io::Result<Self> {
        let root = format!("/proc/{tid}");
        let stat = bounded_file(&format!("{root}/stat"), 65536)?;
        let first = stat
            .split(|b| *b == b' ')
            .next()
            .ok_or_else(|| invalid("malformed proc stat"))?;
        if std::str::from_utf8(first)
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            != Some(tid)
        {
            return Err(invalid("proc stat thread identity disagrees"));
        }
        let close = stat
            .iter()
            .rposition(|b| *b == b')')
            .ok_or_else(|| invalid("malformed proc stat"))?;
        let tail =
            std::str::from_utf8(&stat[close + 1..]).map_err(|_| invalid("malformed proc stat"))?;
        let start_ticks = tail
            .split_whitespace()
            .nth(19)
            .ok_or_else(|| invalid("missing start ticks"))?
            .parse()
            .map_err(|_| invalid("invalid start ticks"))?;
        let exe = std::fs::metadata(format!("{root}/exe"))?;
        let ns = std::fs::metadata(format!("{root}/ns/mnt"))?;
        Ok(Self {
            start_ticks,
            exe: (exe.dev(), exe.ino()),
            mount_namespace: (ns.dev(), ns.ino()),
            maps: bounded_file(&format!("{root}/maps"), MAX_MAPS)?,
            auxv: bounded_file(&format!("{root}/auxv"), 8192)?,
        })
    }
}

#[derive(Debug, Clone)]
struct Map {
    start: u64,
    end: u64,
    offset: u64,
    identity: (u64, u64, u64),
    read: bool,
    write: bool,
    execute: bool,
    private: bool,
}
fn parse_maps(bytes: &[u8]) -> io::Result<Vec<Map>> {
    if bytes.len() > MAX_MAPS {
        return Err(invalid("maps exceeds bound"));
    }
    let mut maps: Vec<Map> = Vec::new();
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() {
            return Err(invalid("empty maps line"));
        }
        let mut fields = line
            .split(|b| b.is_ascii_whitespace())
            .filter(|f| !f.is_empty());
        let mut field = || fields.next().ok_or_else(|| invalid("truncated maps line"));
        let span = std::str::from_utf8(field()?).map_err(|_| invalid("invalid maps range"))?;
        let (start, end) = span
            .split_once('-')
            .ok_or_else(|| invalid("invalid maps range"))?;
        let hex = |s| u64::from_str_radix(s, 16).map_err(|_| invalid("invalid maps number"));
        let (start, end) = (hex(start)?, hex(end)?);
        let permissions = field()?;
        if permissions.len() != 4
            || !matches!(permissions[0], b'r' | b'-')
            || !matches!(permissions[1], b'w' | b'-')
            || !matches!(permissions[2], b'x' | b'-')
            || !matches!(permissions[3], b'p' | b's')
        {
            return Err(invalid("invalid maps permissions"));
        }
        let offset = hex(std::str::from_utf8(field()?).map_err(|_| invalid("invalid offset"))?)?;
        let device = std::str::from_utf8(field()?).map_err(|_| invalid("invalid device"))?;
        let (major, minor) = device
            .split_once(':')
            .ok_or_else(|| invalid("invalid device"))?;
        let identity = (
            hex(major)?,
            hex(minor)?,
            std::str::from_utf8(field()?)
                .map_err(|_| invalid("invalid inode"))?
                .parse()
                .map_err(|_| invalid("invalid inode"))?,
        );
        if start >= end || maps.last().is_some_and(|m| m.end > start) {
            return Err(invalid("overlapping or unordered maps"));
        }
        maps.push(Map {
            start,
            end,
            offset,
            identity,
            read: permissions[0] == b'r',
            write: permissions[1] == b'w',
            execute: permissions[2] == b'x',
            private: permissions[3] == b'p',
        });
    }
    if maps.is_empty() {
        return Err(invalid("empty maps"));
    }
    Ok(maps)
}
fn parse_auxv(bytes: &[u8]) -> io::Result<BTreeMap<u64, u64>> {
    let (entries, remainder) = bytes.as_chunks::<16>();
    if !remainder.is_empty() {
        return Err(invalid("truncated auxv"));
    }
    let mut result = BTreeMap::new();
    let mut ended = false;
    for entry in entries {
        let (tag, value) = (u64_at(entry, 0), u64_at(entry, 8));
        if ended && (tag != 0 || value != 0) {
            return Err(invalid("data after auxv terminator"));
        }
        if tag == 0 {
            ended = true;
            continue;
        }
        if result.insert(tag, value).is_some() {
            return Err(invalid("duplicate auxv tag"));
        }
    }
    if !ended {
        return Err(invalid("unterminated auxv"));
    }
    Ok(result)
}

struct Memory<'a, F> {
    maps: &'a [Map],
    read: F,
    observed: Vec<(u64, Vec<u8>)>,
    total: usize,
}
impl<'a, F: FnMut(u64, &mut [u8]) -> io::Result<()>> Memory<'a, F> {
    fn new(maps: &'a [Map], read: F) -> Self {
        Self {
            maps,
            read,
            observed: Vec::new(),
            total: 0,
        }
    }
    fn mapping(&self, address: u64) -> io::Result<&Map> {
        self.maps
            .iter()
            .find(|m| m.start <= address && address < m.end && m.read)
            .ok_or_else(|| invalid("target range is not readable"))
    }
    fn get(&mut self, address: u64, length: usize) -> io::Result<Vec<u8>> {
        self.total = self
            .total
            .checked_add(length)
            .ok_or_else(|| invalid("read budget overflow"))?;
        if self.total > MAX_READ {
            return Err(invalid("target read budget exceeded"));
        }
        let end = add(address, length as u64)?;
        let mut at = address;
        while at < end {
            at = self.mapping(at)?.end.min(end);
        }
        let mut bytes = vec![0; length];
        (self.read)(address, &mut bytes)?;
        self.observed.push((address, bytes.clone()));
        Ok(bytes)
    }
    fn recheck(&mut self) -> io::Result<()> {
        for (address, bytes) in &self.observed {
            let mut after = vec![0; bytes.len()];
            (self.read)(*address, &mut after)?;
            if &after != bytes {
                return Err(invalid("target memory changed during resolution"));
            }
        }
        Ok(())
    }
}

struct Provider<'a> {
    bytes: &'a [u8],
    elf: Elf<'a>,
    symbol: goblin::elf::sym::Sym,
    version: &'a str,
    dynamic: u64,
    dynamic_offset: u64,
    dynamic_size: u64,
}
impl<'a> Provider<'a> {
    fn parse(bytes: &'a [u8], version: &'a str) -> io::Result<Self> {
        if bytes.len() > MAX_FILE
            || version.is_empty()
            || version.len() > 128
            || version == "GLIBC_PRIVATE"
        {
            return Err(invalid("provider or version outside supported bounds"));
        }
        let elf = Elf::parse(bytes).map_err(|_| invalid("malformed provider ELF"))?;
        if !elf.is_64
            || !elf.little_endian
            || elf.header.e_machine != header::EM_X86_64
            || elf.header.e_type != header::ET_DYN
            || elf.header.e_ehsize != 64
            || elf.header.e_version != 1
            || elf.header.e_phentsize != 56
            || elf.program_headers.len() > 128
            || !matches!(elf.soname, Some("libc.so.6" | "libdl.so.2"))
        {
            return Err(invalid("provider is not supported libc/libdl ELF"));
        }
        validate_loads(&elf.program_headers)?;
        for p in &elf.program_headers {
            if p.p_type == ph::PT_LOAD {
                range(bytes, p.p_offset, p.p_filesz)?;
            }
        }
        let dynamic = elf
            .program_headers
            .iter()
            .filter(|p| p.p_type == ph::PT_DYNAMIC)
            .collect::<Vec<_>>();
        if dynamic.len() != 1 {
            return Err(invalid("ambiguous provider dynamic segment"));
        }
        let (dynamic, dynamic_offset, dynamic_size) =
            (dynamic[0].p_vaddr, dynamic[0].p_offset, dynamic[0].p_filesz);
        if dynamic_size == 0 || dynamic_size % 16 != 0 || dynamic_size / 16 > MAX_DYNAMIC as u64 {
            return Err(invalid("provider dynamic table exceeds bound"));
        }
        range(bytes, dynamic_offset, dynamic_size)?;
        if load_file_offset(&elf.program_headers, dynamic, dynamic_size, false)? != dynamic_offset {
            return Err(invalid("provider dynamic segment disagrees with load"));
        }
        let tags = elf
            .dynamic
            .as_ref()
            .ok_or_else(|| invalid("missing provider dynamic table"))?;
        if !tags
            .dyns
            .last()
            .is_some_and(|entry| entry.d_tag == dynamic::DT_NULL)
        {
            return Err(invalid("unterminated provider dynamic table"));
        }
        let mut unique = BTreeMap::new();
        for entry in &tags.dyns {
            if [
                dynamic::DT_STRTAB,
                dynamic::DT_STRSZ,
                dynamic::DT_SYMTAB,
                dynamic::DT_SYMENT,
                dynamic::DT_VERSYM,
                dynamic::DT_VERDEF,
                dynamic::DT_VERDEFNUM,
                dynamic::DT_HASH,
                dynamic::DT_GNU_HASH,
                dynamic::DT_SONAME,
                dynamic::DT_FLAGS,
            ]
            .contains(&entry.d_tag)
                && unique.insert(entry.d_tag, entry.d_val).is_some()
            {
                return Err(invalid("duplicate symbol metadata tag"));
            }
        }
        let tag = |key| {
            unique
                .get(&key)
                .copied()
                .ok_or_else(|| invalid("missing versioned symbol metadata"))
        };
        if tag(dynamic::DT_SYMENT)? != 24
            || tags.info.textrel
            || unique.get(&dynamic::DT_FLAGS).copied().unwrap_or(0) & dynamic::DF_TEXTREL != 0
            || elf.dynsyms.len() > 65536
        {
            return Err(invalid("unsupported provider symbols or text relocations"));
        }
        let strtab = Self::ro_file(
            &elf,
            bytes,
            tag(dynamic::DT_STRTAB)?,
            tag(dynamic::DT_STRSZ)?,
        )?;
        if c_string(
            strtab,
            usize::try_from(tag(dynamic::DT_SONAME)?)
                .map_err(|_| invalid("SONAME offset overflow"))?,
        )? != elf.soname.unwrap()
        {
            return Err(invalid("provider SONAME table disagreement"));
        }
        let symbols = Self::ro_file(
            &elf,
            bytes,
            tag(dynamic::DT_SYMTAB)?,
            (elf.dynsyms.len() * 24) as u64,
        )?;
        let versions = Self::ro_file(
            &elf,
            bytes,
            tag(dynamic::DT_VERSYM)?,
            (elf.dynsyms.len() * 2) as u64,
        )?;
        // Goblin's section-based verdef iterator may stop early on malformed
        // chains. Walk the bounded DT_VERDEF chain explicitly, using goblin's
        // ELF/load/symbol parsing and the loader's actual dynamic metadata.
        let count = tag(dynamic::DT_VERDEFNUM)?;
        if count == 0 || count > 256 {
            return Err(invalid("version definition count outside bound"));
        }
        let mut address = tag(dynamic::DT_VERDEF)?;
        let mut definitions = BTreeMap::new();
        for i in 0..count {
            let def = Self::ro_file(&elf, bytes, address, 20)?;
            let number = u16_at(def, 4);
            let flags = u16_at(def, 2);
            let aux_count = u16_at(def, 6);
            if number == 0
                || number & 0x8000 != 0
                || flags & !3 != 0
                || (flags & 1 != 0) != (number == 1)
                || u16_at(def, 0) != 1
                || aux_count == 0
                || aux_count > 256
                || u32_at(def, 12) < 20
            {
                return Err(invalid("malformed version definition"));
            }
            let next_definition = u32_at(def, 16);
            if (i + 1 == count) != (next_definition == 0)
                || (next_definition != 0 && next_definition < 20)
            {
                return Err(invalid("malformed version chain"));
            }
            let mut aux = add(address, u32_at(def, 12) as u64)?;
            let mut name = None;
            for j in 0..aux_count {
                if next_definition != 0 && add(aux, 8)? > add(address, next_definition as u64)? {
                    return Err(invalid("version auxiliary chain overlaps next definition"));
                }
                let value = Self::ro_file(&elf, bytes, aux, 8)?;
                let text = c_string(strtab, u32_at(value, 0) as usize)?;
                if j == 0 {
                    name = Some(text);
                }
                let next = u32_at(value, 4);
                if (j + 1 == aux_count) != (next == 0) || (next != 0 && next < 8) {
                    return Err(invalid("malformed version auxiliary chain"));
                }
                aux = add(aux, next as u64)?;
            }
            let name = name.unwrap();
            if u32_at(def, 8) != elf_hash(name.as_bytes()) {
                return Err(invalid("version definition hash disagrees"));
            }
            if definitions.insert(number, name).is_some() {
                return Err(invalid("duplicate version definition"));
            }
            address = add(address, next_definition as u64)?;
        }
        let mut selected = None;
        for index in 0..elf.dynsyms.len() {
            let symbol = elf
                .dynsyms
                .get(index)
                .ok_or_else(|| invalid("truncated dynamic symbols"))?;
            // Require agreement with the table named by DT_SYMTAB, not an
            // independently chosen section table or an unvalidated address.
            let record = &symbols[index * 24..(index + 1) * 24];
            if symbol.st_name != u32_at(record, 0) as usize
                || symbol.st_value != u64_at(record, 8)
                || symbol.st_size != u64_at(record, 16)
                || symbol.st_info != record[4]
                || symbol.st_other != record[5]
                || symbol.st_shndx != u16_at(record, 6) as usize
            {
                return Err(invalid("dynamic symbol table disagreement"));
            }
            if c_string(strtab, symbol.st_name)? != "dlopen" {
                continue;
            }
            let raw_version = u16_at(versions, index * 2);
            if raw_version & 0x8000 != 0 || raw_version <= 1 {
                continue;
            }
            if definitions.get(&(raw_version & 0x7fff)).copied() != Some(version) {
                continue;
            }
            if symbol.st_type() != sym::STT_FUNC
                || !matches!(symbol.st_bind(), sym::STB_GLOBAL | sym::STB_WEAK)
                || symbol.st_other != sym::STV_DEFAULT
                || symbol.st_shndx == 0
                || symbol.st_shndx >= 0xff00
                || symbol.st_size == 0
                || symbol.st_size > 1024 * 1024
            {
                return Err(invalid("dlopen is not a public normal function"));
            }
            Self::ro_file(&elf, bytes, symbol.st_value, symbol.st_size)?;
            if !elf.program_headers.iter().any(|p| {
                p.p_type == ph::PT_LOAD
                    && p.p_flags == (ph::PF_R | ph::PF_X)
                    && symbol.st_value >= p.p_vaddr
                    && add(symbol.st_value, symbol.st_size)
                        .ok()
                        .zip(add(p.p_vaddr, p.p_filesz).ok())
                        .is_some_and(|(a, b)| a <= b)
            }) {
                return Err(invalid("dlopen is not in executable provider bytes"));
            }
            if selected.replace(symbol).is_some() {
                return Err(invalid("ambiguous public dlopen symbol"));
            }
        }
        let symbol =
            selected.ok_or_else(|| invalid("requested default dlopen version is absent"))?;
        Ok(Self {
            bytes,
            elf,
            symbol,
            version,
            dynamic,
            dynamic_offset,
            dynamic_size,
        })
    }
    fn ro_file(elf: &Elf<'_>, bytes: &'a [u8], address: u64, len: u64) -> io::Result<&'a [u8]> {
        range(
            bytes,
            load_file_offset(&elf.program_headers, address, len, true)?,
            len,
        )
    }
}
fn c_string(bytes: &[u8], offset: usize) -> io::Result<&str> {
    let tail = bytes
        .get(offset..)
        .ok_or_else(|| invalid("string offset outside table"))?;
    let tail = &tail[..tail.len().min(4096)];
    let end = tail
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| invalid("unterminated ELF string"))?;
    std::str::from_utf8(&tail[..end]).map_err(|_| invalid("invalid ELF string"))
}

fn elf_hash(bytes: &[u8]) -> u32 {
    let mut hash = 0_u32;
    for byte in bytes {
        hash = (hash << 4).wrapping_add(u32::from(*byte));
        let high = hash & 0xf000_0000;
        hash ^= high >> 24;
        hash &= !high;
    }
    hash
}

// File-backed PT_LOAD ranges must be unambiguous before translating addresses.
fn validate_loads(headers: &[goblin::elf::ProgramHeader]) -> io::Result<()> {
    let mut loads = Vec::new();
    for p in headers.iter().filter(|p| p.p_type == ph::PT_LOAD) {
        let end = add(p.p_vaddr, p.p_memsz)?;
        add(p.p_offset, p.p_filesz)?;
        if p.p_filesz > p.p_memsz
            || p.p_flags & !7 != 0
            || (p.p_align > 1
                && (!p.p_align.is_power_of_two()
                    || p.p_vaddr % p.p_align != p.p_offset % p.p_align))
        {
            return Err(invalid("malformed ELF load segment"));
        }
        if loads
            .iter()
            .any(|(start, stop)| p.p_vaddr < *stop && *start < end)
        {
            return Err(invalid("overlapping ELF load segments"));
        }
        loads.push((p.p_vaddr, end));
    }
    if loads.is_empty() {
        return Err(invalid("missing ELF load segments"));
    }
    Ok(())
}
fn load_file_offset(
    headers: &[goblin::elf::ProgramHeader],
    address: u64,
    length: u64,
    read_only: bool,
) -> io::Result<u64> {
    let end = add(address, length)?;
    let mut found = None;
    for p in headers.iter().filter(|p| p.p_type == ph::PT_LOAD) {
        if p.p_flags & ph::PF_R != 0
            && (!read_only || p.p_flags & ph::PF_W == 0)
            && address >= p.p_vaddr
            && end <= add(p.p_vaddr, p.p_filesz)?
        {
            let offset = add(p.p_offset, address - p.p_vaddr)?;
            if found.replace(offset).is_some() {
                return Err(invalid("ambiguous ELF file range"));
            }
        }
    }
    found.ok_or_else(|| invalid("ELF range is not in required readable load bytes"))
}

fn check_file_range<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &Memory<'_, F>,
    address: u64,
    offset: u64,
    size: u64,
    identity: (u64, u64, u64),
) -> io::Result<()> {
    let end = add(address, size)?;
    let mut at = address;
    while at < end {
        let mapping = memory.mapping(at)?;
        if mapping.identity != identity
            || !mapping.private
            || add(mapping.offset, at - mapping.start)? != add(offset, at - address)?
        {
            return Err(invalid("mapped ELF file range disagrees"));
        }
        at = mapping.end.min(end);
    }
    Ok(())
}

struct LoaderContext {
    executable_phdr: u64,
    main_bias: u64,
    main_header: u64,
    main_dynamic: u64,
    main_identity: (u64, u64, u64),
    loader_base: u64,
    debug: u64,
    first_node: u64,
}

fn inspect_loader<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    aux: &BTreeMap<u64, u64>,
) -> io::Result<LoaderContext> {
    let aux = |tag| {
        aux.get(&tag)
            .copied()
            .ok_or_else(|| invalid("missing auxv entry"))
    };
    let phdr = aux(libc::AT_PHDR)?;
    let count = aux(libc::AT_PHNUM)?;
    if count == 0 || count > 128 || aux(libc::AT_PHENT)? != 56 {
        return Err(invalid("invalid executable program headers"));
    }
    let headers = memory.get(phdr, (count * 56) as usize)?;
    let mut phdr_segment = None;
    let mut dynamic_segment = None;
    let (header_records, remainder) = headers.as_chunks::<56>();
    debug_assert!(remainder.is_empty());
    let program_headers: Vec<_> = header_records
        .iter()
        .map(|p| goblin::elf::ProgramHeader {
            p_type: u32_at(p, 0),
            p_flags: u32_at(p, 4),
            p_offset: u64_at(p, 8),
            p_vaddr: u64_at(p, 16),
            p_paddr: u64_at(p, 24),
            p_filesz: u64_at(p, 32),
            p_memsz: u64_at(p, 40),
            p_align: u64_at(p, 48),
        })
        .collect();
    validate_loads(&program_headers)?;
    for p in header_records {
        if matches!(u32_at(p, 0), ph::PT_PHDR | ph::PT_DYNAMIC) && u64_at(p, 32) > u64_at(p, 40) {
            return Err(invalid("executable segment file size exceeds memory size"));
        }
        match u32_at(p, 0) {
            ph::PT_PHDR => {
                if phdr_segment
                    .replace((u64_at(p, 8), u64_at(p, 16), u64_at(p, 32)))
                    .is_some()
                {
                    return Err(invalid("duplicate PT_PHDR"));
                }
            }
            ph::PT_DYNAMIC
                if dynamic_segment
                    .replace((u64_at(p, 8), u64_at(p, 16), u64_at(p, 32)))
                    .is_some() =>
            {
                return Err(invalid("duplicate executable PT_DYNAMIC"));
            }
            ph::PT_DYNAMIC => {}
            _ => {}
        }
    }
    let (phdr_offset, phdr_vaddr, phdr_size) =
        phdr_segment.ok_or_else(|| invalid("missing executable PT_PHDR"))?;
    if load_file_offset(&program_headers, phdr_vaddr, phdr_size, false)? != phdr_offset {
        return Err(invalid("executable PT_PHDR disagrees with load"));
    }
    if phdr_size != count * 56 {
        return Err(invalid("executable PT_PHDR size disagrees"));
    }
    let main_bias = phdr
        .checked_sub(phdr_vaddr)
        .ok_or_else(|| invalid("invalid executable load bias"))?;
    let main_header = phdr
        .checked_sub(phdr_offset)
        .ok_or_else(|| invalid("invalid executable ELF header"))?;
    let header = memory.get(main_header, 64)?;
    if &header[..7] != b"\x7fELF\x02\x01\x01"
        || !matches!(u16_at(&header, 16), header::ET_EXEC | header::ET_DYN)
        || u16_at(&header, 18) != header::EM_X86_64
        || u64_at(&header, 32) != phdr_offset
        || u16_at(&header, 52) != 64
        || u16_at(&header, 54) != 56
        || u16_at(&header, 56) as u64 != count
    {
        return Err(invalid("executable ELF header disagrees with auxv"));
    }
    if u16_at(&header, 16) == header::ET_EXEC && main_bias != 0 {
        return Err(invalid("ET_EXEC has nonzero load bias"));
    }
    let header_vaddr = main_header
        .checked_sub(main_bias)
        .ok_or_else(|| invalid("ELF header load bias overflow"))?;
    if load_file_offset(&program_headers, header_vaddr, 64, true)? != 0 {
        return Err(invalid("executable ELF header disagrees with load"));
    }
    let main_identity = memory.mapping(main_header)?.identity;
    if main_identity.2 == 0 {
        return Err(invalid("executable headers have no mapped file identity"));
    }
    check_file_range(memory, main_header, 0, 64, main_identity)?;
    check_file_range(memory, phdr, phdr_offset, phdr_size, main_identity)?;
    let (dynamic_offset, dynamic_vaddr, dynamic_size) =
        dynamic_segment.ok_or_else(|| invalid("missing executable PT_DYNAMIC"))?;
    if dynamic_size == 0
        || !dynamic_size.is_multiple_of(16)
        || dynamic_size / 16 > MAX_DYNAMIC as u64
    {
        return Err(invalid("executable dynamic table exceeds bound"));
    }
    if load_file_offset(&program_headers, dynamic_vaddr, dynamic_size, false)? != dynamic_offset {
        return Err(invalid("executable dynamic segment disagrees with load"));
    }
    let main_dynamic = add(main_bias, dynamic_vaddr)?;
    check_file_range(
        memory,
        main_dynamic,
        dynamic_offset,
        dynamic_size,
        main_identity,
    )?;
    let table = memory.get(main_dynamic, dynamic_size as usize)?;
    let mut debug = None;
    let mut terminated = false;
    for d in table.as_chunks::<16>().0 {
        if u64_at(d, 0) == dynamic::DT_NULL {
            terminated = true;
            break;
        }
        if u64_at(d, 0) == dynamic::DT_DEBUG && debug.replace(u64_at(d, 8)).is_some() {
            return Err(invalid("duplicate DT_DEBUG"));
        }
    }
    if !terminated {
        return Err(invalid("unterminated executable dynamic table"));
    }
    let debug = debug
        .filter(|p| *p != 0)
        .ok_or_else(|| invalid("loader rendezvous is unavailable"))?;
    let rendezvous = memory.get(debug, 40)?;
    if !matches!(u32_at(&rendezvous, 0), 1 | 2)
        || u32_at(&rendezvous, 24) != 0
        || u64_at(&rendezvous, 32) != aux(libc::AT_BASE)?
    {
        return Err(invalid(
            "loader is not in a consistent default-namespace state",
        ));
    }
    let loader_base = aux(libc::AT_BASE)?;
    let loader_header = memory.get(loader_base, 64)?;
    if &loader_header[..7] != b"\x7fELF\x02\x01\x01"
        || u16_at(&loader_header, 16) != header::ET_DYN
        || u16_at(&loader_header, 18) != header::EM_X86_64
    {
        return Err(invalid("AT_BASE does not name an x86-64 interpreter ELF"));
    }
    let loader_mapping = memory.mapping(loader_base)?;
    if loader_mapping.write || !loader_mapping.private {
        return Err(invalid(
            "interpreter header is not private read-only memory",
        ));
    }
    let loader_identity = loader_mapping.identity;
    check_file_range(memory, loader_base, 0, 64, loader_identity)?;
    let breakpoint = memory.mapping(u64_at(&rendezvous, 16))?;
    if loader_identity.2 == 0
        || breakpoint.identity != loader_identity
        || !breakpoint.execute
        || breakpoint.write
        || !breakpoint.private
    {
        return Err(invalid(
            "loader rendezvous breakpoint is not in interpreter code",
        ));
    }
    Ok(LoaderContext {
        executable_phdr: phdr,
        main_bias,
        main_header,
        main_dynamic,
        main_identity,
        loader_base,
        debug,
        first_node: u64_at(&rendezvous, 8),
    })
}

fn resolve<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    aux: &BTreeMap<u64, u64>,
    provider: &Provider<'_>,
) -> io::Result<TargetDlopen> {
    let context = inspect_loader(memory, aux)?;
    let mut node = context.first_node;
    let mut previous = 0;
    let mut visited = Vec::new();
    let mut selected = None;
    while node != 0 {
        if visited.len() >= MAX_LINKS || visited.contains(&node) {
            return Err(invalid("link map cycle or bound"));
        }
        visited.push(node);
        let link = memory.get(node, 40)?;
        let bias = u64_at(&link, 0);
        let ld = u64_at(&link, 16);
        if u64_at(&link, 32) != previous {
            return Err(invalid("inconsistent link map back pointer"));
        }
        if previous == 0 && (bias != context.main_bias || ld != context.main_dynamic) {
            return Err(invalid("default link map does not begin with executable"));
        }
        if ld == add(bias, provider.dynamic)? && provider_matches(memory, provider, bias)? {
            if selected.is_some() {
                return Err(invalid("provider appears twice in default namespace"));
            }
            let address = add(bias, provider.symbol.st_value)?;
            let mapping = memory.mapping(address)?;
            if !mapping.execute || mapping.write || !mapping.private {
                return Err(invalid("dlopen entry is not private executable memory"));
            }
            let identity = mapping.identity;
            selected = Some(TargetDlopen {
                tid: 0,
                start_ticks: 0,
                executable_phdr: context.executable_phdr,
                link_map: node,
                load_bias: bias,
                address,
                version: provider.version.to_owned(),
                mapping_identity: identity,
            });
        }
        previous = node;
        node = u64_at(&link, 24);
    }
    let result =
        selected.ok_or_else(|| invalid("expected provider is absent from default link map"))?;
    memory.recheck()?;
    Ok(result)
}

fn provider_matches<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    provider: &Provider<'_>,
    bias: u64,
) -> io::Result<bool> {
    image_matches(
        memory,
        provider.bytes,
        &provider.elf,
        (
            provider.dynamic,
            provider.dynamic_offset,
            provider.dynamic_size,
        ),
        bias,
    )
}

fn image_matches<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    bytes: &[u8],
    elf: &Elf<'_>,
    dynamic: (u64, u64, u64),
    bias: u64,
) -> io::Result<bool> {
    let first = elf
        .program_headers
        .iter()
        .find(|p| {
            p.p_type == ph::PT_LOAD
                && p.p_offset == 0
                && p.p_flags & ph::PF_R != 0
                && p.p_flags & ph::PF_W == 0
                && p.p_filesz >= 64
        })
        .ok_or_else(|| invalid("provider headers are not in first read-only load"))?;
    let start = add(bias, first.p_vaddr)?;
    let mapping = match memory.mapping(start) {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };
    if mapping.identity.2 == 0 || mapping.write || !mapping.private {
        return Ok(false);
    }
    let identity = mapping.identity;
    if memory.get(start, 64)? != bytes[..64] {
        return Ok(false);
    }
    // Establish the candidate's complete load layout before applying the
    // expected provider's permission and file-offset relationships. Different
    // DSOs can share the ELF header and dynamic RVA but have different loads.
    let phdr_offset = elf.header.e_phoff;
    let phdr_size = (elf.program_headers.len() * 56) as u64;
    let phdr_address = add(start, phdr_offset)?;
    check_file_range(memory, phdr_address, phdr_offset, phdr_size, identity)?;
    let headers = memory.get(phdr_address, phdr_size as usize)?;
    if headers != range(bytes, phdr_offset, phdr_size)? {
        return Ok(false);
    }
    // Equal headers and a PT_DYNAMIC RVA do not identify a DSO. A different
    // object can precede the exact expected provider in the default list.
    // Still finish every range/read check: a byte mismatch must not hide an
    // observation failure or invalid mapping later in this candidate.
    let mut identical = true;
    for p in &elf.program_headers {
        if p.p_type != ph::PT_LOAD || p.p_flags & ph::PF_W != 0 {
            continue;
        }
        if p.p_flags & ph::PF_R == 0 || p.p_filesz > p.p_memsz {
            return Err(invalid("invalid read-only provider load"));
        }
        let expected = range(bytes, p.p_offset, p.p_filesz)?;
        let start = add(bias, p.p_vaddr)?;
        let end = add(start, p.p_filesz)?;
        let mut at = start;
        while at < end {
            let mapping = memory.mapping(at)?;
            if mapping.identity != identity
                || mapping.write
                || !mapping.private
                || mapping.execute != (p.p_flags & ph::PF_X != 0)
                || add(mapping.offset, at - mapping.start)? != add(p.p_offset, at - start)?
            {
                return Err(invalid(
                    "provider mapping identity, permissions or offset disagrees",
                ));
            }
            let amount = (mapping.end.min(end) - at).min(65536) as usize;
            let bytes = memory.get(at, amount)?;
            let offset = (at - start) as usize;
            if bytes != expected[offset..offset + amount] {
                identical = false;
            }
            at = add(at, amount as u64)?;
        }
    }
    check_file_range(
        memory,
        add(bias, dynamic.0)?,
        dynamic.1,
        dynamic.2,
        identity,
    )?;
    Ok(identical)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum InitImageRole {
    Main,
    Libc,
    Interpreter,
    Libgcc,
    Runtime,
}

struct InitImage<'a> {
    bytes: &'a [u8],
    elf: Elf<'a>,
    dynamic: (u64, u64, u64),
    metadata: Vec<(u64, u64)>,
    versions: InitVersions<'a>,
}

struct InitVersions<'a> {
    symbols: Vec<u16>,
    definitions: BTreeMap<u16, &'a str>,
    requirements: BTreeMap<u16, (&'a str, &'a str)>,
    metadata: Vec<(u64, u64)>,
}

fn record_init_version_range(
    ranges: &mut BTreeMap<u64, u64>,
    metadata: &mut Vec<(u64, u64)>,
    address: u64,
    size: u64,
) -> io::Result<()> {
    let end = add(address, size)?;
    if ranges
        .range(..=address)
        .next_back()
        .is_some_and(|(_, previous_end)| *previous_end > address)
        || ranges.range(address..end).next().is_some()
    {
        return Err(invalid("overlapping initialization version records"));
    }
    ranges.insert(address, end);
    metadata.push((address, size));
    Ok(())
}

fn init_versions<'a>(
    elf: &Elf<'_>,
    bytes: &'a [u8],
    tags: &BTreeMap<u64, u64>,
    strings: &'a [u8],
    dependencies: &[&str],
) -> io::Result<InitVersions<'a>> {
    let mut ranges = BTreeMap::new();
    let mut result = InitVersions {
        symbols: vec![1; elf.dynsyms.len()],
        definitions: BTreeMap::new(),
        requirements: BTreeMap::new(),
        metadata: Vec::new(),
    };
    result.symbols[0] = 0;
    if let Some(&address) = tags.get(&dynamic::DT_VERSYM) {
        let length = (elf.dynsyms.len() * 2) as u64;
        let versions = Provider::ro_file(elf, bytes, address, length)?;
        result.symbols = versions
            .as_chunks::<2>()
            .0
            .iter()
            .map(|value| u16::from_le_bytes(*value))
            .collect();
        record_init_version_range(&mut ranges, &mut result.metadata, address, length)?;
    } else if [
        dynamic::DT_VERDEF,
        dynamic::DT_VERDEFNUM,
        dynamic::DT_VERNEED,
        dynamic::DT_VERNEEDNUM,
    ]
    .iter()
    .any(|tag| tags.contains_key(tag))
    {
        return Err(invalid(
            "initialization version metadata has no symbol version table",
        ));
    }
    for (table_tag, count_tag, definitions) in [
        (dynamic::DT_VERDEF, dynamic::DT_VERDEFNUM, true),
        (dynamic::DT_VERNEED, dynamic::DT_VERNEEDNUM, false),
    ] {
        let (mut address, count) = match (tags.get(&table_tag), tags.get(&count_tag)) {
            (None, None) => continue,
            (Some(&address), Some(&count)) if count != 0 && count <= 256 => (address, count),
            _ => return Err(invalid("initialization version table count is invalid")),
        };
        for table_index in 0..count {
            let size = if definitions { 20 } else { 16 };
            let record = Provider::ro_file(elf, bytes, address, size)?;
            record_init_version_range(&mut ranges, &mut result.metadata, address, size)?;
            let aux_count = u16_at(record, if definitions { 6 } else { 2 });
            let aux_offset = u32_at(record, if definitions { 12 } else { 8 }) as u64;
            let next = u32_at(record, if definitions { 16 } else { 12 }) as u64;
            if u16_at(record, 0) != 1
                || aux_count == 0
                || aux_count > 256
                || aux_offset < size
                || (table_index + 1 == count) != (next == 0)
                || next != 0 && next < size
            {
                return Err(invalid("malformed initialization version chain"));
            }
            let library = if definitions {
                None
            } else {
                let library = c_string(strings, u32_at(record, 4) as usize)?;
                if !dependencies.contains(&library) {
                    return Err(invalid(
                        "initialization version requirement names an unbound dependency",
                    ));
                }
                Some(library)
            };
            let mut auxiliary = add(address, aux_offset)?;
            let mut definition_name = None;
            for aux_index in 0..aux_count {
                let aux_size = if definitions { 8 } else { 16 };
                if definitions && next != 0 && add(auxiliary, aux_size)? > add(address, next)? {
                    return Err(invalid(
                        "initialization version auxiliary overlaps next record",
                    ));
                }
                let aux = Provider::ro_file(elf, bytes, auxiliary, aux_size)?;
                record_init_version_range(&mut ranges, &mut result.metadata, auxiliary, aux_size)?;
                let name = c_string(
                    strings,
                    u32_at(aux, if definitions { 0 } else { 8 }) as usize,
                )?;
                let next_aux = u32_at(aux, if definitions { 4 } else { 12 }) as u64;
                if (aux_index + 1 == aux_count) != (next_aux == 0)
                    || next_aux != 0 && next_aux < aux_size
                {
                    return Err(invalid("malformed initialization version auxiliary chain"));
                }
                if definitions {
                    if aux_index == 0 {
                        definition_name = Some(name);
                    }
                } else {
                    let number = u16_at(aux, 6);
                    if number <= 1
                        || number & 0x8000 != 0
                        || u16_at(aux, 4) & !2 != 0
                        || u32_at(aux, 0) != elf_hash(name.as_bytes())
                        || result
                            .requirements
                            .insert(number, (library.unwrap(), name))
                            .is_some()
                    {
                        return Err(invalid("malformed initialization version requirement"));
                    }
                }
                auxiliary = add(auxiliary, next_aux)?;
            }
            if definitions {
                let number = u16_at(record, 4);
                let flags = u16_at(record, 2);
                let name = definition_name.unwrap();
                if number == 0
                    || number & 0x8000 != 0
                    || flags & !3 != 0
                    || (flags & 1 != 0) != (number == 1)
                    || u32_at(record, 8) != elf_hash(name.as_bytes())
                    || result.definitions.insert(number, name).is_some()
                {
                    return Err(invalid("malformed initialization version definition"));
                }
            }
            address = add(address, next)?;
        }
    }
    if result.symbols[0] != 0 {
        return Err(invalid("initialization null symbol has a version"));
    }
    for (index, symbol) in elf.dynsyms.iter().enumerate() {
        let version = result.symbols[index];
        let number = version & 0x7fff;
        if symbol.st_shndx == 0 {
            if version & 0x8000 != 0 || number > 1 && !result.requirements.contains_key(&number) {
                return Err(invalid(
                    "initialization import has an invalid version requirement",
                ));
            }
        } else if externally_defined(&symbol)
            && number > 1
            && !result.definitions.contains_key(&number)
        {
            return Err(invalid(
                "initialization export has an invalid version definition",
            ));
        }
    }
    Ok(result)
}

impl<'a> InitImage<'a> {
    fn parse(bytes: &'a [u8], role: InitImageRole) -> io::Result<Self> {
        let limit = if role == InitImageRole::Runtime {
            64 * 1024 * 1024
        } else {
            MAX_FILE
        };
        if bytes.len() > limit {
            return Err(invalid("runtime initialization image exceeds bound"));
        }
        let elf =
            Elf::parse(bytes).map_err(|_| invalid("malformed runtime initialization image"))?;
        if !elf.is_64
            || !elf.little_endian
            || elf.header.e_machine != header::EM_X86_64
            || elf.header.e_ehsize != 64
            || elf.header.e_version != 1
            || elf.header.e_phentsize != 56
            || elf.program_headers.len() > 128
            || !(elf.header.e_type == header::ET_DYN
                || role == InitImageRole::Main && elf.header.e_type == header::ET_EXEC)
        {
            return Err(invalid("unsupported runtime initialization image"));
        }
        let soname = match role {
            InitImageRole::Libc => Some("libc.so.6"),
            InitImageRole::Interpreter => Some("ld-linux-x86-64.so.2"),
            InitImageRole::Libgcc => Some("libgcc_s.so.1"),
            InitImageRole::Main | InitImageRole::Runtime => None,
        };
        if soname.is_some() && elf.soname != soname {
            return Err(invalid(
                "bound runtime initialization dependency has wrong SONAME",
            ));
        }
        validate_loads(&elf.program_headers)?;
        for load in elf
            .program_headers
            .iter()
            .filter(|p| p.p_type == ph::PT_LOAD)
        {
            range(bytes, load.p_offset, load.p_filesz)?;
        }
        let segments: Vec<_> = elf
            .program_headers
            .iter()
            .filter(|p| p.p_type == ph::PT_DYNAMIC)
            .collect();
        if segments.len() != 1 {
            return Err(invalid("ambiguous runtime initialization dynamic segment"));
        }
        let segment = segments[0];
        let dynamic_layout = (segment.p_vaddr, segment.p_offset, segment.p_filesz);
        if segment.p_filesz == 0
            || !segment.p_filesz.is_multiple_of(16)
            || segment.p_filesz / 16 > MAX_DYNAMIC as u64
            || load_file_offset(
                &elf.program_headers,
                segment.p_vaddr,
                segment.p_filesz,
                false,
            )? != segment.p_offset
        {
            return Err(invalid("invalid runtime initialization dynamic layout"));
        }
        let table = range(bytes, segment.p_offset, segment.p_filesz)?;
        let tags = &elf
            .dynamic
            .as_ref()
            .ok_or_else(|| invalid("missing initialization dynamic table"))?
            .dyns;
        if !tags
            .last()
            .is_some_and(|entry| entry.d_tag == dynamic::DT_NULL)
            || tags.len() * 16 > table.len()
        {
            return Err(invalid("unterminated initialization dynamic table"));
        }
        let mut unique = BTreeMap::new();
        let mut needed = Vec::new();
        for (index, entry) in tags.iter().enumerate() {
            if u64_at(table, index * 16) != entry.d_tag
                || u64_at(table, index * 16 + 8) != entry.d_val
            {
                return Err(invalid("initialization dynamic table disagreement"));
            }
            if matches!(
                entry.d_tag,
                dynamic::DT_AUDIT
                    | dynamic::DT_DEPAUDIT
                    | dynamic::DT_CONFIG
                    | dynamic::DT_RPATH
                    | dynamic::DT_RUNPATH
                    | dynamic::DT_SYMBOLIC
                    | dynamic::DT_TEXTREL
                    | 0x7fff_fffd
                    | 0x7fff_ffff
            ) {
                return Err(invalid(
                    "runtime initialization refuses loader callback or search directives",
                ));
            }
            if entry.d_tag == dynamic::DT_NEEDED {
                needed.push(entry.d_val);
            } else if entry.d_tag != dynamic::DT_NULL
                && unique.insert(entry.d_tag, entry.d_val).is_some()
            {
                return Err(invalid("duplicate initialization dynamic tag"));
            }
        }
        let allowed_flags = dynamic::DF_BIND_NOW | dynamic::DF_STATIC_TLS;
        let allowed_flags_1 = dynamic::DF_1_NOW
            | if role == InitImageRole::Main {
                dynamic::DF_1_PIE
            } else {
                0
            };
        if unique.get(&dynamic::DT_FLAGS).copied().unwrap_or(0) & !allowed_flags != 0
            || unique.get(&dynamic::DT_FLAGS_1).copied().unwrap_or(0) & !allowed_flags_1 != 0
        {
            return Err(invalid("unsupported runtime initialization loader flags"));
        }
        let tag = |tag| {
            unique
                .get(&tag)
                .copied()
                .ok_or_else(|| invalid("missing initialization symbol metadata"))
        };
        if tag(dynamic::DT_SYMENT)? != 24 || elf.dynsyms.is_empty() || elf.dynsyms.len() > 65536 {
            return Err(invalid("unsupported initialization symbol table"));
        }
        let strings = Provider::ro_file(
            &elf,
            bytes,
            tag(dynamic::DT_STRTAB)?,
            tag(dynamic::DT_STRSZ)?,
        )?;
        let symbols = Provider::ro_file(
            &elf,
            bytes,
            tag(dynamic::DT_SYMTAB)?,
            (elf.dynsyms.len() * 24) as u64,
        )?;
        let mut dependencies = Vec::new();
        for offset in needed {
            let name = c_string(
                strings,
                usize::try_from(offset).map_err(|_| invalid("dependency name overflow"))?,
            )?;
            let allowed = match role {
                InitImageRole::Interpreter => false,
                InitImageRole::Libc => name == "ld-linux-x86-64.so.2",
                InitImageRole::Libgcc => matches!(name, "libc.so.6" | "ld-linux-x86-64.so.2"),
                InitImageRole::Main | InitImageRole::Runtime => {
                    matches!(name, "libc.so.6" | "ld-linux-x86-64.so.2" | "libgcc_s.so.1")
                }
            };
            if !allowed || dependencies.contains(&name) {
                return Err(invalid(
                    "runtime initialization dependency closure is unsupported",
                ));
            }
            dependencies.push(name);
        }
        if dependencies != elf.libraries {
            return Err(invalid(
                "runtime initialization dependency metadata disagrees",
            ));
        }
        if let Some(expected) = soname
            && c_string(
                strings,
                usize::try_from(tag(dynamic::DT_SONAME)?)
                    .map_err(|_| invalid("SONAME offset overflow"))?,
            )? != expected
        {
            return Err(invalid("runtime initialization SONAME metadata disagrees"));
        }
        for (index, symbol) in elf.dynsyms.iter().enumerate() {
            let record = &symbols[index * 24..(index + 1) * 24];
            let name = c_string(strings, symbol.st_name)?;
            if symbol.st_name != u32_at(record, 0) as usize
                || symbol.st_info != record[4]
                || symbol.st_other != record[5]
                || symbol.st_shndx != u16_at(record, 6) as usize
                || symbol.st_value != u64_at(record, 8)
                || symbol.st_size != u64_at(record, 16)
                || elf.dynstrtab.get_at(symbol.st_name) != Some(name)
            {
                return Err(invalid("initialization symbol table disagreement"));
            }
            // A non-PIE executable may expose an undefined function with its
            // canonical PLT address in st_value. The loader can use that guest
            // stub for lookup too, so SHN_UNDEF alone does not rule out guest
            // interposition during controller calls.
            if role == InitImageRole::Main
                && externally_bindable(&symbol)
                && (symbol.st_shndx != 0 || symbol.st_value != 0)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "runtime initialization refuses executable interposition symbol {name}"
                    ),
                ));
            }
        }
        ordinary::validate_runtime_hashes(&elf, bytes, strings, &unique, None)?;
        let versions = init_versions(&elf, bytes, &unique, strings, &dependencies)?;
        let mut metadata = vec![
            (tag(dynamic::DT_STRTAB)?, tag(dynamic::DT_STRSZ)?),
            (tag(dynamic::DT_SYMTAB)?, (elf.dynsyms.len() * 24) as u64),
        ];
        metadata.extend_from_slice(&versions.metadata);
        if let Some(address) = unique.get(&dynamic::DT_HASH) {
            let hash = Provider::ro_file(&elf, bytes, *address, 8)?;
            metadata.push((
                *address,
                8 + (u32_at(hash, 0) as u64 + u32_at(hash, 4) as u64) * 4,
            ));
        }
        if let Some(address) = unique.get(&dynamic::DT_GNU_HASH) {
            metadata.push((
                *address,
                ordinary::validated_gnu_hash_size(&elf, bytes, strings, *address)?,
            ));
        }
        Ok(Self {
            bytes,
            elf,
            dynamic: dynamic_layout,
            metadata,
            versions,
        })
    }

    fn has_definition(&self, name: &str, requirement: Option<(&str, &str)>) -> bool {
        self.elf.dynsyms.iter().enumerate().any(|(index, symbol)| {
            let version = self.versions.symbols[index];
            let version_matches = match requirement {
                Some((library, name)) => {
                    self.elf.soname == Some(library)
                        && self.versions.definitions.get(&(version & 0x7fff)).copied() == Some(name)
                }
                None => version == 1 || version > 1 && version & 0x8000 == 0,
            };
            externally_defined(&symbol)
                && self.elf.dynstrtab.get_at(symbol.st_name) == Some(name)
                && version_matches
        })
    }
}

fn externally_defined(symbol: &goblin::elf::sym::Sym) -> bool {
    symbol.st_shndx != 0 && externally_bindable(symbol)
}

fn externally_bindable(symbol: &goblin::elf::sym::Sym) -> bool {
    matches!(
        symbol.st_bind(),
        sym::STB_GLOBAL | sym::STB_WEAK | sym::STB_GNU_UNIQUE
    ) && matches!(
        symbol.st_visibility(),
        sym::STV_DEFAULT | sym::STV_PROTECTED
    )
}

pub(crate) fn validate_runtime_init_dependencies(
    libc: &[u8],
    interpreter: &[u8],
    libgcc: &[u8],
    runtime: &[u8],
) -> io::Result<()> {
    let images = [
        InitImage::parse(libc, InitImageRole::Libc)?,
        InitImage::parse(interpreter, InitImageRole::Interpreter)?,
        InitImage::parse(libgcc, InitImageRole::Libgcc)?,
        InitImage::parse(runtime, InitImageRole::Runtime)?,
    ];
    for (image_index, image) in images.iter().enumerate() {
        for (index, symbol) in image
            .elf
            .dynsyms
            .iter()
            .enumerate()
            .filter(|(_, symbol)| symbol.st_shndx == 0 && symbol.st_bind() == sym::STB_GLOBAL)
        {
            let name = image
                .elf
                .dynstrtab
                .get_at(symbol.st_name)
                .ok_or_else(|| invalid("invalid imported initialization symbol"))?;
            let requirement = image
                .versions
                .requirements
                .get(&(image.versions.symbols[index] & 0x7fff))
                .copied();
            if !images[..=image_index.max(1)]
                .iter()
                .any(|candidate| candidate.has_definition(name, requirement))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("runtime initialization import is outside bound images: {name}"),
                ));
            }
        }
    }
    Ok(())
}

fn check_init_dynamic<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    image: &InitImage<'_>,
    bias: u64,
    debug: Option<u64>,
) -> io::Result<()> {
    let table = memory.get(add(bias, image.dynamic.0)?, image.dynamic.2 as usize)?;
    for (index, entry) in image.elf.dynamic.as_ref().unwrap().dyns.iter().enumerate() {
        let actual = u64_at(&table, index * 16 + 8);
        let relocated = matches!(
            entry.d_tag,
            dynamic::DT_HASH
                | dynamic::DT_GNU_HASH
                | dynamic::DT_PLTGOT
                | dynamic::DT_STRTAB
                | dynamic::DT_SYMTAB
                | dynamic::DT_VERSYM
                | dynamic::DT_RELA
                | dynamic::DT_REL
                | dynamic::DT_JMPREL
                | 36
        );
        let valid_value = if entry.d_tag == dynamic::DT_DEBUG {
            debug == Some(actual)
        } else {
            actual == entry.d_val || relocated && actual == add(bias, entry.d_val)?
        };
        if u64_at(&table, index * 16) != entry.d_tag || !valid_value {
            return Err(invalid("loaded initialization dynamic metadata disagrees"));
        }
    }
    Ok(())
}

fn check_main_metadata<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    image: &InitImage<'_>,
    context: &LoaderContext,
) -> io::Result<()> {
    if memory.get(context.main_header, 64)? != image.bytes[..64]
        || memory.get(
            context.executable_phdr,
            image.elf.program_headers.len() * 56,
        )? != range(
            image.bytes,
            image.elf.header.e_phoff,
            (image.elf.program_headers.len() * 56) as u64,
        )?
        || add(context.main_bias, image.dynamic.0)? != context.main_dynamic
    {
        return Err(invalid(
            "executable file and mapped initialization metadata disagree",
        ));
    }
    for &(address, length) in &image.metadata {
        let start = add(context.main_bias, address)?;
        let end = add(start, length)?;
        let offset = load_file_offset(&image.elf.program_headers, address, length, true)?;
        check_file_range(memory, start, offset, length, context.main_identity)?;
        let mut at = start;
        while at < end {
            let map = memory.mapping(at)?;
            if map.write {
                return Err(invalid("executable symbol metadata is writable"));
            }
            at = map.end.min(end);
        }
        if memory.get(start, length as usize)? != range(image.bytes, offset, length)? {
            return Err(invalid(
                "executable symbol metadata differs from bound observation",
            ));
        }
    }
    check_init_dynamic(memory, image, context.main_bias, Some(context.debug))
}

fn check_kernel_vdso<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    base: u64,
    dynamic: u64,
) -> io::Result<()> {
    let map = memory.mapping(base)?;
    if map.identity.2 != 0 || map.write || !map.execute || !map.private || map.start != base {
        return Err(invalid(
            "AT_SYSINFO_EHDR is not a private kernel vDSO mapping",
        ));
    }
    let end = map.end;
    let header = memory.get(base, 64)?;
    if &header[..7] != b"\x7fELF\x02\x01\x01"
        || u16_at(&header, 16) != header::ET_DYN
        || u16_at(&header, 18) != header::EM_X86_64
        || u16_at(&header, 54) != 56
    {
        return Err(invalid("invalid kernel vDSO ELF header"));
    }
    let count = u16_at(&header, 56) as usize;
    let address = add(base, u64_at(&header, 32))?;
    if count == 0 || count > 128 || add(address, (count * 56) as u64)? > end {
        return Err(invalid("kernel vDSO program headers exceed mapping"));
    }
    let headers = memory.get(address, count * 56)?;
    let dynamics: Vec<_> = headers
        .as_chunks::<56>()
        .0
        .iter()
        .filter(|record| u32_at(*record, 0) == ph::PT_DYNAMIC)
        .collect();
    if dynamics.len() != 1
        || add(base, u64_at(dynamics[0], 16))? != dynamic
        || add(dynamic, u64_at(dynamics[0], 40))? > end
    {
        return Err(invalid("kernel vDSO link map disagrees"));
    }
    Ok(())
}

/// Exact kernel/controller vDSO bytes captured before the original loader runs.
#[derive(Clone, Debug)]
pub(crate) struct BoundRuntimeInitVdso {
    start_ticks: u64,
    address: u64,
    bytes: Vec<u8>,
}

/// Call only at the post-exec controller stop, after controller vDSO patches and
/// before resuming any original loader or application instruction. A later
/// observation would not establish kernel/controller provenance.
pub(crate) fn capture_runtime_init_vdso(task: &Stopped) -> io::Result<BoundRuntimeInitVdso> {
    let tid = task.pid().as_raw();
    let before = Snapshot::read(tid)?;
    let maps = parse_maps(&before.maps)?;
    let aux = parse_auxv(&before.auxv)?;
    let address = aux.get(&libc::AT_SYSINFO_EHDR).copied().unwrap_or(0);
    let mut memory = Memory::new(&maps, |address, bytes: &mut [u8]| {
        let address = usize::try_from(address).map_err(|_| invalid("vDSO address overflow"))?;
        task.read_exact(address, bytes)
            .map_err(|error| io::Error::other(format!("read initial vDSO: {error}")))
    });
    let bytes = if address == 0 {
        Vec::new()
    } else {
        let mapping = memory.mapping(address)?;
        if mapping.start != address
            || mapping.identity.2 != 0
            || mapping.write
            || !mapping.execute
            || !mapping.private
            || mapping.end - address > 65536
        {
            return Err(invalid(
                "initial kernel vDSO mapping exceeds supported shape",
            ));
        }
        memory.get(address, (mapping.end - address) as usize)?
    };
    memory.recheck()?;
    if Snapshot::read(tid)? != before {
        return Err(invalid("target changed during initial vDSO capture"));
    }
    Ok(BoundRuntimeInitVdso {
        start_ticks: before.start_ticks,
        address,
        bytes,
    })
}

/// Resolve the bound libc's normal default-version errno accessor. Like
/// `resolve_dlopen`, this address expires when any sharing task resumes.
pub(crate) fn resolve_runtime_errno(
    task: &Stopped,
    init: &crate::LiteinstRuntimeInit,
) -> io::Result<u64> {
    let image = InitImage::parse(&init.expected_loader, InitImageRole::Libc)?;
    let mut selected = None;
    for (index, symbol) in image.elf.dynsyms.iter().enumerate() {
        if image.elf.dynstrtab.get_at(symbol.st_name) != Some("__errno_location") {
            continue;
        }
        let version = image.versions.symbols[index];
        if version & 0x8000 != 0 {
            continue;
        }
        if symbol.st_bind() != sym::STB_GLOBAL
            || symbol.st_type() != sym::STT_FUNC
            || symbol.st_other != sym::STV_DEFAULT
            || symbol.st_shndx == 0
            || symbol.st_shndx >= 0xff00
            || symbol.st_size == 0
            || symbol.st_size > 1024 * 1024
            || version <= 1
            || !image.versions.definitions.contains_key(&version)
        {
            return Err(invalid(
                "libc errno accessor is not an ordinary default-version function",
            ));
        }
        Provider::ro_file(&image.elf, image.bytes, symbol.st_value, symbol.st_size)?;
        if !image.elf.program_headers.iter().any(|load| {
            load.p_type == ph::PT_LOAD
                && load.p_flags == (ph::PF_R | ph::PF_X)
                && symbol.st_value >= load.p_vaddr
                && symbol
                    .st_value
                    .checked_add(symbol.st_size)
                    .zip(load.p_vaddr.checked_add(load.p_filesz))
                    .is_some_and(|(end, load_end)| end <= load_end)
        }) || selected.replace(symbol.st_value).is_some()
        {
            return Err(invalid(
                "libc errno accessor has ambiguous executable coordinates",
            ));
        }
    }
    let value = selected.ok_or_else(|| invalid("ordinary libc errno accessor is absent"))?;
    let provider = resolve_dlopen(task, &init.expected_loader, &init.loader_version)?;
    add(provider.load_bias, value)
}

/// Checks the closed initial loader scope; the result expires on any resume.
///
/// Returns whether the exact bound libgcc is already loaded. Supplying a runtime
/// identity permits and requires that one already-loaded runtime. This is a
/// structural import/callback boundary, not protection against arbitrary guest
/// modification of the loader's writable internal state. All sharing tasks must
/// be quiescent, as required by `resolve_dlopen`.
pub(crate) fn validate_runtime_init_boundary(
    task: &Stopped,
    init: &crate::LiteinstRuntimeInit,
    loaded_runtime_identity: Option<(u64, u64, u64)>,
    expected_vdso: &BoundRuntimeInitVdso,
) -> io::Result<bool> {
    let tid = task.pid().as_raw();
    let before = Snapshot::read(tid)?;
    let maps = parse_maps(&before.maps)?;
    let aux = parse_auxv(&before.auxv)?;
    if aux.get(&libc::AT_SECURE) != Some(&0) {
        return Err(invalid(
            "runtime initialization refuses secure-execution loader state",
        ));
    }
    let main_bytes = bounded_file(&format!("/proc/{tid}/exe"), MAX_FILE)?;
    let main = InitImage::parse(&main_bytes, InitImageRole::Main)?;
    let images = [
        InitImage::parse(&init.expected_loader, InitImageRole::Libc)?,
        InitImage::parse(&init.expected_interpreter, InitImageRole::Interpreter)?,
        InitImage::parse(&init.expected_libgcc, InitImageRole::Libgcc)?,
        InitImage::parse(&init.runtime, InitImageRole::Runtime)?,
    ];
    let mut memory = Memory::new(&maps, |address, bytes: &mut [u8]| {
        let address = usize::try_from(address).map_err(|_| invalid("target address overflow"))?;
        task.read_exact(address, bytes).map_err(|error| {
            io::Error::other(format!(
                "read initialization target at {address:#x}: {error}"
            ))
        })
    });
    let context = inspect_loader(&mut memory, &aux)?;
    // maps and stat expose different device IDs on supported layered mounts.
    // Keep inode continuity, and establish the executable's actual binding
    // through exact mapped header/PHDR and loader/symbol metadata below. Every
    // mapped range must retain its own maps identity and file offset; the full
    // proc identity, namespace and maps snapshot are rechecked before return.
    if context.main_identity.2 != before.exe.1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "executable mapping and proc identity disagree: maps={:?}, stat=({},{},{})",
                context.main_identity,
                libc::major(before.exe.0),
                libc::minor(before.exe.0),
                before.exe.1,
            ),
        ));
    }
    // A second namespace changes the import/audit scope. Version two indicates
    // that such a scope has existed; it is outside this deliberately narrow path.
    if u32_at(&memory.get(context.debug, 40)?, 0) != 1 {
        return Err(invalid(
            "runtime initialization refuses additional loader namespaces",
        ));
    }
    check_main_metadata(&mut memory, &main, &context)?;
    let vdso = aux.get(&libc::AT_SYSINFO_EHDR).copied().unwrap_or(0);
    if before.start_ticks != expected_vdso.start_ticks || vdso != expected_vdso.address {
        return Err(invalid("runtime initialization vDSO provenance disagrees"));
    }
    if vdso != 0 {
        let mapping = memory.mapping(vdso)?;
        if mapping.end - vdso != expected_vdso.bytes.len() as u64
            || memory.get(vdso, expected_vdso.bytes.len())? != expected_vdso.bytes
        {
            return Err(invalid(
                "runtime initialization vDSO differs from initial controller bytes",
            ));
        }
    }
    let mut seen_vdso = false;
    let mut seen = [false; 4];
    let mut visited = Vec::new();
    let mut node = context.first_node;
    let mut previous = 0;
    while node != 0 {
        if visited.len() >= MAX_LINKS || visited.contains(&node) {
            return Err(invalid("initialization link map cycle or bound"));
        }
        visited.push(node);
        let link = memory.get(node, 40)?;
        let bias = u64_at(&link, 0);
        let dynamic = u64_at(&link, 16);
        if u64_at(&link, 32) != previous {
            return Err(invalid("inconsistent initialization link map back pointer"));
        }
        if previous == 0 {
            if bias != context.main_bias || dynamic != context.main_dynamic {
                return Err(invalid(
                    "initialization link map does not begin with executable",
                ));
            }
        } else if vdso != 0 && bias == vdso {
            if seen_vdso {
                return Err(invalid("duplicate kernel vDSO in initialization scope"));
            }
            check_kernel_vdso(&mut memory, vdso, dynamic)?;
            seen_vdso = true;
        } else {
            let mut matched = None;
            for (index, image) in images.iter().enumerate() {
                if index == 3 && loaded_runtime_identity.is_none() {
                    continue;
                }
                if dynamic == add(bias, image.dynamic.0)?
                    && image_matches(&mut memory, image.bytes, &image.elf, image.dynamic, bias)?
                {
                    if matched.replace(index).is_some() || seen[index] {
                        return Err(invalid("duplicate bound image in initialization scope"));
                    }
                    if index == 1 && bias != context.loader_base {
                        return Err(invalid("bound interpreter differs from AT_BASE"));
                    }
                    if index == 3
                        && loaded_runtime_identity != Some(memory.mapping(dynamic)?.identity)
                    {
                        return Err(invalid("runtime initialization memfd identity disagrees"));
                    }
                    check_init_dynamic(&mut memory, image, bias, None)?;
                }
            }
            let index = matched
                .ok_or_else(|| invalid("runtime initialization refuses unbound loader object"))?;
            seen[index] = true;
        }
        previous = node;
        node = u64_at(&link, 24);
    }
    if !seen[0]
        || !seen[1]
        || (vdso != 0 && !seen_vdso)
        || loaded_runtime_identity.is_some() != seen[3]
    {
        return Err(invalid(
            "runtime initialization closed loader scope is incomplete",
        ));
    }
    memory.recheck()?;
    if Snapshot::read(tid)? != before {
        return Err(invalid(
            "target changed during initialization boundary validation",
        ));
    }
    Ok(seen[2])
}

#[cfg(test)]
mod runtime_init_policy_tests {
    use goblin::container::Container;
    use goblin::container::Ctx;
    use goblin::container::Endian;

    use super::*;

    fn put16(bytes: &mut [u8], at: usize, value: u16) {
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put32(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn table_elf(bytes: &[u8], symbols: usize) -> Elf<'_> {
        let context = Ctx::new(Container::Big, Endian::Little);
        let mut elf = Elf::lazy_parse(goblin::elf::header::Header::new(context)).unwrap();
        elf.program_headers.push(goblin::elf::ProgramHeader {
            p_type: ph::PT_LOAD,
            p_flags: ph::PF_R,
            p_offset: 0,
            p_vaddr: 0,
            p_paddr: 0,
            p_filesz: bytes.len() as u64,
            p_memsz: bytes.len() as u64,
            p_align: 8,
        });
        elf.dynsyms = goblin::elf::sym::Symtab::parse(bytes, 64, symbols, context).unwrap();
        elf
    }

    #[test]
    fn empty_gnu_hash_is_valid_without_a_selected_export_but_nonempty_bucket_is_not() {
        let mut bytes = vec![0; 1024];
        put32(&mut bytes, 64 + 24, 1);
        bytes[64 + 24 + 4] = sym::STB_GLOBAL << 4 | sym::STT_FUNC;
        for (index, value) in [1, 2, 1, 5].into_iter().enumerate() {
            put32(&mut bytes, 0x100 + index * 4, value);
        }
        let tags = BTreeMap::from([(dynamic::DT_GNU_HASH, 0x100)]);
        let elf = table_elf(&bytes, 2);
        ordinary::validate_runtime_hashes(&elf, &bytes, b"\0puts\0", &tags, None).unwrap();
        assert!(
            ordinary::validate_runtime_hashes(&elf, &bytes, b"\0puts\0", &tags, Some(1)).is_err()
        );
        bytes[0x118] = 1;
        assert!(
            ordinary::validate_runtime_hashes(
                &table_elf(&bytes, 2),
                &bytes,
                b"\0puts\0",
                &tags,
                None
            )
            .is_err()
        );
    }

    #[cfg(target_env = "gnu")]
    struct FixtureDirectory(std::path::PathBuf);

    #[cfg(target_env = "gnu")]
    impl Drop for FixtureDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(target_env = "gnu")]
    fn fixture_directory() -> FixtureDirectory {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "reverie-init-parser-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        FixtureDirectory(directory)
    }

    #[cfg(target_env = "gnu")]
    #[test]
    fn native_fixture_with_only_undefined_exports_has_valid_empty_gnu_hash() {
        let directory = fixture_directory();
        let executable = directory.0.join("ordinary-main");
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../reverie-liteinst/tests/fixtures/runtime_init.c");
        let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
        let output = std::process::Command::new(compiler)
            .args(["-std=gnu11", "-O0", "-fPIE", "-pie", "-Wl,--hash-style=gnu"])
            .arg(source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "fixture compiler failed: {output:?}"
        );
        let mut bytes = std::fs::read(&executable).unwrap();
        let parsed = InitImage::parse(&bytes, InitImageRole::Main).unwrap();
        assert!(parsed.elf.dynsyms.iter().all(|symbol| symbol.st_shndx == 0));
        let hash_address = parsed
            .elf
            .dynamic
            .as_ref()
            .unwrap()
            .dyns
            .iter()
            .find(|entry| entry.d_tag == dynamic::DT_GNU_HASH)
            .unwrap()
            .d_val;
        let hash_offset =
            load_file_offset(&parsed.elf.program_headers, hash_address, 16, true).unwrap() as usize;
        let bucket_offset = hash_offset + 16 + u32_at(&bytes, hash_offset + 8) as usize * 8;
        assert_eq!(u32_at(&bytes, bucket_offset), 0);
        assert!((u32_at(&bytes, hash_offset + 4) as usize) < parsed.elf.dynsyms.len());
        assert_eq!(
            ordinary::validated_gnu_hash_size(
                &parsed.elf,
                &bytes,
                Provider::ro_file(
                    &parsed.elf,
                    &bytes,
                    parsed
                        .elf
                        .dynamic
                        .as_ref()
                        .unwrap()
                        .dyns
                        .iter()
                        .find(|entry| entry.d_tag == dynamic::DT_STRTAB)
                        .unwrap()
                        .d_val,
                    parsed
                        .elf
                        .dynamic
                        .as_ref()
                        .unwrap()
                        .dyns
                        .iter()
                        .find(|entry| entry.d_tag == dynamic::DT_STRSZ)
                        .unwrap()
                        .d_val
                )
                .unwrap(),
                hash_address
            )
            .unwrap(),
            (bucket_offset - hash_offset + 4) as u64
        );
        put32(&mut bytes, bucket_offset, 1);
        assert!(InitImage::parse(&bytes, InitImageRole::Main).is_err());
    }

    #[cfg(target_env = "gnu")]
    #[test]
    fn native_canonical_plt_is_refused_even_with_an_undefined_symbol() {
        let directory = fixture_directory();
        let source = directory.0.join("canonical-plt.c");
        let executable = directory.0.join("canonical-plt");
        std::fs::write(&source,
            b"#include <unistd.h>\nstatic long (*get_sysconf(void))(int) { return &sysconf; }\nint main(void) { return get_sysconf()(_SC_PAGESIZE) > 0 ? 0 : 1; }\n"
        ).unwrap();
        let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
        let output = std::process::Command::new(compiler)
            .args(["-std=gnu11", "-O0", "-fno-pie", "-no-pie"])
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "canonical PLT compiler failed: {output:?}"
        );
        assert!(
            std::process::Command::new(&executable)
                .status()
                .unwrap()
                .success(),
            "native canonical PLT control failed"
        );
        let bytes = std::fs::read(&executable).unwrap();
        let elf = Elf::parse(&bytes).unwrap();
        let symbol = elf
            .dynsyms
            .iter()
            .find(|symbol| elf.dynstrtab.get_at(symbol.st_name) == Some("sysconf"))
            .unwrap();
        assert_eq!(
            symbol.st_shndx, 0,
            "control must expose an undefined import"
        );
        assert_ne!(
            symbol.st_value, 0,
            "control must expose a canonical PLT address"
        );
        let error = InitImage::parse(&bytes, InitImageRole::Main)
            .err()
            .expect("canonical PLT must be refused");
        assert!(
            error.to_string().contains("interposition symbol sysconf"),
            "unexpected refusal: {error}"
        );
    }

    fn requirement_fixture() -> (Vec<u8>, &'static [u8], BTreeMap<u64, u64>) {
        let mut bytes = vec![0; 1024];
        let strings: &[u8] = b"\0libc.so.6\0ld-linux-x86-64.so.2\0GLIBC_2.2.5\0GLIBC_PRIVATE\0";
        let offset = |name: &str| {
            strings
                .windows(name.len())
                .position(|part| part == name.as_bytes())
                .unwrap() as u32
        };
        put16(&mut bytes, 0x102, 2);
        put16(&mut bytes, 0x104, 3);
        for (address, library, auxiliary, next) in [
            (0x200, "libc.so.6", 0x20, 0x10),
            (0x210, "ld-linux-x86-64.so.2", 0x20, 0),
        ] {
            put16(&mut bytes, address, 1);
            put16(&mut bytes, address + 2, 1);
            put32(&mut bytes, address + 4, offset(library));
            put32(&mut bytes, address + 8, auxiliary);
            put32(&mut bytes, address + 12, next);
        }
        for (address, version, number) in [(0x220, "GLIBC_2.2.5", 2), (0x230, "GLIBC_PRIVATE", 3)] {
            put32(&mut bytes, address, elf_hash(version.as_bytes()));
            put16(&mut bytes, address + 6, number);
            put32(&mut bytes, address + 8, offset(version));
        }
        (
            bytes,
            strings,
            BTreeMap::from([
                (dynamic::DT_VERSYM, 0x100),
                (dynamic::DT_VERNEED, 0x200),
                (dynamic::DT_VERNEEDNUM, 2),
            ]),
        )
    }

    #[test]
    fn version_requirements_accept_out_of_line_auxiliaries_and_refuse_overlap() {
        let (mut bytes, strings, tags) = requirement_fixture();
        let dependencies = ["libc.so.6", "ld-linux-x86-64.so.2"];
        let parsed =
            init_versions(&table_elf(&bytes, 3), &bytes, &tags, strings, &dependencies).unwrap();
        assert_eq!(
            parsed.requirements.get(&2),
            Some(&("libc.so.6", "GLIBC_2.2.5"))
        );
        assert_eq!(
            parsed.requirements.get(&3),
            Some(&("ld-linux-x86-64.so.2", "GLIBC_PRIVATE"))
        );
        put32(&mut bytes, 0x218, 0x10);
        assert!(
            init_versions(&table_elf(&bytes, 3), &bytes, &tags, strings, &dependencies).is_err()
        );
    }

    #[test]
    fn version_requirements_refuse_unbound_library_and_bad_hash() {
        let (mut bytes, strings, tags) = requirement_fixture();
        assert!(
            init_versions(
                &table_elf(&bytes, 3),
                &bytes,
                &tags,
                strings,
                &["libc.so.6"]
            )
            .is_err()
        );
        bytes[0x220] ^= 1;
        assert!(
            init_versions(
                &table_elf(&bytes, 3),
                &bytes,
                &tags,
                strings,
                &["libc.so.6", "ld-linux-x86-64.so.2"]
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod tests;
