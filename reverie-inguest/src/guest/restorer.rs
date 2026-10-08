// Copyright (c) Meta Platforms, Inc. and affiliates.
//
// This source code is licensed under the BSD-style license found in the
// LICENSE file in the root directory of this source tree.

//! Signal phase 1's restorer rules (dev-hermit
//! `ai_docs/transient/liteinst-inguest-signal-handlers-design-20261007.md`,
//! section 4 "Restorers" and closure 3).
//!
//! A guest SIGALRM action is admitted only with glibc's x86-64 restorer,
//! `__restore_rt`, because the runtime's trampoline returns through its own
//! restorer and must do exactly what the guest's would. The restorer is
//! accepted only if its bytes are glibc's (`mov $15,%rax; syscall`) and they
//! lie in an executable mapping of the same file as this process's C library.
//!
//! An activation can still return through an accepted restorer after its
//! action is replaced, so its page stays protected for the rest of the
//! process's life (a fork child inherits the protection with the memory):
//! every guest call that could unmap, replace, reprotect or advise any byte
//! of it is refused before the kernel.

use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;

use crate::guest::support::auxv_entry;
use crate::guest::support::read_own_bytes;
use crate::guest::support::scan_own_maps;
use crate::guest::support::scan_proc_lines;
use crate::trap::raw_syscall6;

/// glibc's x86-64 `__restore_rt`: `mov $15,%rax; syscall`.
pub const GLIBC_RESTORER_BYTES: [u8; 9] = [0x48, 0xc7, 0xc0, 0x0f, 0x00, 0x00, 0x00, 0x0f, 0x05];

/// What a `/proc/self/maps` line says about the mapping containing an
/// address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MappingIdentity {
    /// First address of the mapping.
    pub start: u64,
    /// One past its last address.
    pub end: u64,
    /// Whether it is executable (`x` in its permissions).
    pub executable: bool,
    /// Whether it is writable (`w` in its permissions).
    pub writable: bool,
    /// Its device, as `major << 32 | minor`.
    pub device: u64,
    /// Its inode; 0 for an anonymous mapping.
    pub inode: u64,
}

fn hex(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 16 {
        return None;
    }
    bytes.iter().try_fold(0_u64, |value, byte| {
        let digit = (*byte as char).to_digit(16)?;
        Some((value << 4) | u64::from(digit))
    })
}

fn decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0_u64, |value, byte| {
        let digit = (*byte as char).to_digit(10)?;
        value.checked_mul(10)?.checked_add(u64::from(digit))
    })
}

/// Parse one `/proc/self/maps` line; when its range contains `address`,
/// return the mapping's identity.
pub fn maps_line_identity(line: &[u8], address: u64) -> Option<MappingIdentity> {
    maps_line_mapping(line).filter(|mapping| mapping.start <= address && address < mapping.end)
}

/// Parse one `/proc/self/maps` line into its mapping's identity.
pub fn maps_line_mapping(line: &[u8]) -> Option<MappingIdentity> {
    let mut fields = line
        .split(|byte| *byte == b' ')
        .filter(|field| !field.is_empty());
    let range = fields.next()?;
    let permissions = fields.next()?;
    let _offset = fields.next()?;
    let device = fields.next()?;
    let inode = fields.next()?;
    let dash = range.iter().position(|byte| *byte == b'-')?;
    let start = hex(&range[..dash])?;
    let end = hex(&range[dash + 1..])?;
    let colon = device.iter().position(|byte| *byte == b':')?;
    let major = hex(&device[..colon])?;
    let minor = hex(&device[colon + 1..])?;
    Some(MappingIdentity {
        start,
        end,
        executable: permissions.get(2) == Some(&b'x'),
        writable: permissions.get(1) == Some(&b'w'),
        device: (major << 32) | minor,
        inode: decimal(inode)?,
    })
}

/// Whether a restorer whose `GLIBC_RESTORER_BYTES.len()` bytes start at
/// `restorer`, inside `mapping`, lies in the C library's own executable
/// mapping `libc`: the same file, and the same range (not an alias of the
/// file mapped elsewhere, which the guest owns and could rewrite, for example
/// through `/proc/self/mem`, without touching the C library the runtime runs;
/// rewriting the C library itself attacks the runtime, which the design's
/// threat model excludes).
pub fn restorer_mapping_accepted(
    restorer: u64,
    mapping: MappingIdentity,
    libc: MappingIdentity,
) -> bool {
    mapping.executable
        && !mapping.writable
        && mapping.inode != 0
        && mapping.device == libc.device
        && mapping.inode == libc.inode
        && mapping.start == libc.start
        && mapping.end == libc.end
        && restorer
            .checked_add(GLIBC_RESTORER_BYTES.len() as u64)
            .is_some_and(|end| end <= mapping.end)
}

/// The C library's executable mapping: its file, as `major << 32 | minor` and
/// inode, and its range, recorded once in ordinary context by
/// [`record_libc_identity`]; 0 before.
static LIBC_DEVICE: AtomicU64 = AtomicU64::new(0);
static LIBC_INODE: AtomicU64 = AtomicU64::new(0);
static LIBC_START: AtomicU64 = AtomicU64::new(0);
static LIBC_END: AtomicU64 = AtomicU64::new(0);

/// The C library's `DT_SONAME`, with its terminating NUL.
const LIBC_SONAME: &[u8] = b"libc.so.6\0";

/// Up to 16 bytes of this process's memory at `address`, read without
/// faulting, and how many leading bytes were readable.
fn read_own_16(address: u64) -> ([u8; 16], usize) {
    let mut bytes = [0_u8; 16];
    let mut head = [0_u8; 8];
    let mut tail = [0_u8; 8];
    // SAFETY: read_own_bytes reads through process_vm_readv, which reports an
    // unreadable address instead of faulting.
    let first = unsafe { read_own_bytes(address, &mut head) };
    bytes[..8].copy_from_slice(&head);
    if first < 8 {
        return (bytes, first);
    }
    let Some(next) = address.checked_add(8) else {
        return (bytes, 8);
    };
    let second = unsafe { read_own_bytes(next, &mut tail) };
    bytes[8..].copy_from_slice(&tail);
    (bytes, 8 + second)
}

/// The 8-byte word of this process's memory at `address`, read without
/// faulting.
fn read_own_word(address: u64) -> Option<u64> {
    let mut word = [0_u8; 8];
    // SAFETY: as in read_own_16.
    (unsafe { read_own_bytes(address, &mut word) } == 8).then(|| u64::from_le_bytes(word))
}

/// The value of the first `tag` entry of the dynamic section at `dynamic`,
/// read without faulting; `None` if the section ends (`DT_NULL`) first or
/// cannot be read.
fn dynamic_entry(dynamic: u64, tag: u64) -> Option<u64> {
    for index in 0..4096_u64 {
        let entry = dynamic.checked_add(index * 16)?;
        let found = read_own_word(entry)?;
        if found == 0 {
            return None; // DT_NULL
        }
        if found == tag {
            return read_own_word(entry.checked_add(8)?);
        }
    }
    None
}

const DT_STRTAB: u64 = 5;
const DT_SONAME: u64 = 14;
const DT_DEBUG: u64 = 21;
const AT_PHDR: u64 = 3;
const AT_PHNUM: u64 = 5;

/// Whether the loaded object whose load bias is `l_addr` and whose dynamic
/// section is at `l_ld` has the `DT_SONAME` `expected` (NUL included). The
/// loader relocates `DT_STRTAB` in place in a writable dynamic section; a
/// read-only one (the vDSO's) keeps the link-time address, below the bias.
fn object_soname_is(l_addr: u64, l_ld: u64, expected: &[u8]) -> bool {
    let (Some(strtab), Some(offset)) = (
        dynamic_entry(l_ld, DT_STRTAB),
        dynamic_entry(l_ld, DT_SONAME),
    ) else {
        return false;
    };
    let table = if strtab < l_addr {
        strtab.wrapping_add(l_addr)
    } else {
        strtab
    };
    let Some(name) = table.checked_add(offset) else {
        return false;
    };
    let (bytes, readable) = read_own_16(name);
    expected.len() <= 16 && readable >= expected.len() && bytes[..expected.len()] == *expected
}

/// The dynamic loader's `r_debug` for the base namespace: the address the
/// loader wrote into the main executable's `DT_DEBUG` entry, found through the
/// program headers the kernel names in the auxiliary vector.
fn loader_debug_record() -> std::io::Result<u64> {
    let refuse = |what: &'static str| std::io::Error::other(what);
    let phdr = auxv_entry(AT_PHDR).ok_or_else(|| refuse("the auxiliary vector has no AT_PHDR"))?;
    let phnum = auxv_entry(AT_PHNUM)
        .ok_or_else(|| refuse("the auxiliary vector has no AT_PHNUM"))?
        .min(256);
    let header = |index: u64| {
        let address = phdr.checked_add(index * 56)?;
        let type_and_flags = read_own_word(address)?;
        let vaddr = read_own_word(address.checked_add(16)?)?;
        Some((type_and_flags as u32, vaddr))
    };
    let (mut bias, mut dynamic) = (0_u64, None);
    for index in 0..phnum {
        match header(index) {
            Some((libc::PT_PHDR, vaddr)) => bias = phdr.wrapping_sub(vaddr),
            Some((libc::PT_DYNAMIC, vaddr)) => dynamic = Some(vaddr),
            Some(_) => {}
            None => {
                return Err(refuse(
                    "the main executable's program headers are unreadable",
                ));
            }
        }
    }
    let dynamic = dynamic
        .ok_or_else(|| refuse("the main executable has no dynamic section"))?
        .wrapping_add(bias);
    match dynamic_entry(dynamic, DT_DEBUG) {
        Some(record) if record != 0 => Ok(record),
        _ => Err(refuse(
            "the main executable's DT_DEBUG names no loader debug record",
        )),
    }
}

/// Record which file is this process's C library: the executable mapping of
/// the file of the one loaded object, in the dynamic loader's base namespace,
/// whose `DT_SONAME` is `libc.so.6`. The loader satisfies every `libc.so.6`
/// dependency, the runtime's own included, with the loaded object of that
/// `DT_SONAME`, whatever its file is named, so this is the C library the
/// runtime and the guest run on. Call once, at runtime initialization, in
/// ordinary context, while the process is single-threaded (no `dlopen` can
/// change the loader's list meanwhile).
///
/// It calls no function the guest could interpose or that allocates through
/// the C library's malloc (inside the guest, the guest's own heap; the
/// runtime's private allocation scope redirects only Rust allocations). In
/// particular not `dlopen` (which allocates even with `RTLD_NOLOAD`),
/// `dl_iterate_phdr` or `getauxval` (an application or preloaded library may
/// define either, allocate inside it, and report objects of its choosing).
/// Instead it reads, with raw syscalls and fault-free raw memory reads:
/// the auxiliary vector from `/proc/self/auxv` (the kernel's), the main
/// executable's program headers and dynamic section there, the `r_debug`
/// record the loader wrote into its `DT_DEBUG` entry, the loader's own
/// `link_map` list from it (`l_addr`, `l_ld`, `l_next`), and each object's
/// `DT_SONAME` from its dynamic section. The base namespace's list is the one
/// the program and the runtime were loaded in; objects in other `dlmopen`
/// namespaces (glibc 2.35's `r_debug_extended::r_next`) carry their own C
/// library and are not considered. No match, or more than one, is refused.
/// A main executable without `DT_DEBUG` is refused too.
pub fn record_libc_identity() -> std::io::Result<()> {
    let record = loader_debug_record()?;
    // struct r_debug { int r_version; struct link_map *r_map; ... }
    let version = read_own_word(record).map(|word| word as u32);
    if !matches!(version, Some(1 | 2)) {
        return Err(std::io::Error::other(
            "the loader debug record has an unknown version",
        ));
    }
    let mut map = read_own_word(record + 8).unwrap_or(0);
    let (mut matches, mut libc_dynamic) = (0_usize, 0_u64);
    // struct link_map { l_addr; l_name; l_ld; l_next; l_prev; ... }
    for _ in 0..4096 {
        if map == 0 {
            break;
        }
        let (Some(l_addr), Some(l_ld), Some(l_next)) = (
            read_own_word(map),
            read_own_word(map + 16),
            read_own_word(map + 24),
        ) else {
            return Err(std::io::Error::other(
                "the loader's object list is unreadable",
            ));
        };
        if l_ld != 0 && object_soname_is(l_addr, l_ld, LIBC_SONAME) {
            matches += 1;
            libc_dynamic = l_ld;
        }
        map = l_next;
    }
    if map != 0 {
        return Err(std::io::Error::other(
            "the loader's object list does not end",
        ));
    }
    if matches != 1 {
        return Err(std::io::Error::other(format!(
            "{matches} loaded objects have the DT_SONAME libc.so.6"
        )));
    }
    // The C library's file: the one its dynamic section is mapped from.
    let file = unsafe { scan_own_maps(|line| maps_line_identity(line, libc_dynamic)) }
        .filter(|mapping| mapping.inode != 0)
        .ok_or_else(|| {
            std::io::Error::other("libc.so.6's dynamic section is in no file mapping")
        })?;
    let (mut executable, mut count) = (None, 0_usize);
    let _ = unsafe {
        scan_own_maps(|line| {
            if let Some(mapping) = maps_line_mapping(line)
                && mapping.executable
                && mapping.device == file.device
                && mapping.inode == file.inode
            {
                count += 1;
                executable = Some(mapping);
            }
            None::<()>
        })
    };
    let identity = match (count, executable) {
        (1, Some(mapping)) => mapping,
        _ => {
            return Err(std::io::Error::other(format!(
                "libc.so.6's file has {count} executable mappings"
            )));
        }
    };
    LIBC_DEVICE.store(identity.device, Ordering::Release);
    LIBC_START.store(identity.start, Ordering::Release);
    LIBC_END.store(identity.end, Ordering::Release);
    LIBC_INODE.store(identity.inode, Ordering::Release);
    Ok(())
}

fn recorded_libc() -> Option<MappingIdentity> {
    let inode = LIBC_INODE.load(Ordering::Acquire);
    (inode != 0).then(|| MappingIdentity {
        start: LIBC_START.load(Ordering::Acquire),
        end: LIBC_END.load(Ordering::Acquire),
        executable: true,
        writable: false,
        device: LIBC_DEVICE.load(Ordering::Acquire),
        inode,
    })
}

/// Whether a `/proc/self/smaps` `VmFlags:` line names an attribute that would
/// keep the mapping from a fork child as-is: `dc` (`MADV_DONTFORK`) or `wf`
/// (`MADV_WIPEONFORK`). A child inherits the action and the protection, so
/// it must inherit the restorer too.
pub fn vm_flags_change_on_fork(flags: &[u8]) -> bool {
    flags.strip_prefix(b"VmFlags:").is_some_and(|rest| {
        rest.split(|byte| *byte == b' ')
            .any(|flag| flag == b"dc" || flag == b"wf")
    })
}

/// Whether the smaps entry of the mapping containing `address` keeps it from
/// a fork child as-is; `None` if the entry cannot be read.
unsafe fn changes_on_fork(address: u64) -> Option<bool> {
    let mut inside = false;
    unsafe {
        scan_proc_lines(c"/proc/self/smaps", |line| {
            if line.starts_with(b"VmFlags:") {
                if inside {
                    return Some(vm_flags_change_on_fork(line));
                }
            } else if line.first().is_some_and(u8::is_ascii_hexdigit)
                && line
                    .iter()
                    .take_while(|byte| **byte != b' ')
                    .any(|byte| *byte == b'-')
            {
                // A mapping's header line.
                inside = maps_line_identity(line, address).is_some();
            }
            None
        })
    }
}

/// The current program break.
fn program_break() -> u64 {
    unsafe { raw_syscall6(libc::SYS_brk, [0; 6]) as u64 }
}

/// Whether `restorer` is glibc's `__restore_rt` in this process's C library:
/// its bytes are [`GLIBC_RESTORER_BYTES`], read without faulting, and they
/// lie in an executable, file-backed mapping of the same file (device and
/// inode) as the C library ([`record_libc_identity`]). That mapping must also
/// pass to a fork child as-is (no `MADV_DONTFORK` or `MADV_WIPEONFORK`), and
/// lie above the program break, so that no `brk` shrink can reach it (a
/// growing break cannot cover an existing mapping).
///
/// # Safety
///
/// Issues raw syscalls through the trusted gate.
pub unsafe fn glibc_restorer_accepted(restorer: u64) -> bool {
    let mut head = [0_u8; 8];
    let mut tail = [0_u8; 8];
    let Some(tail_address) = restorer.checked_add(8) else {
        return false;
    };
    if unsafe { read_own_bytes(restorer, &mut head) } != 8
        || unsafe { read_own_bytes(tail_address, &mut tail) } < 1
        || head[..] != GLIBC_RESTORER_BYTES[..8]
        || tail[0] != GLIBC_RESTORER_BYTES[8]
    {
        return false;
    }
    let Some(libc) = recorded_libc() else {
        return false;
    };
    let Some(mapping) = (unsafe { scan_own_maps(|line| maps_line_identity(line, restorer)) })
    else {
        return false;
    };
    restorer_mapping_accepted(restorer, mapping, libc)
        && mapping.start >= program_break()
        && unsafe { changes_on_fork(restorer) } == Some(false)
}

const PAGE_SIZE: u64 = 4096;

/// How many distinct restorer pages one process can protect. glibc has one
/// restorer, so one page is the expected use; the rest are slack.
pub const MAX_PROTECTED_PAGES: usize = 4;

/// The protected restorer pages, by page address; 0 is an empty slot. A fork
/// child inherits them with the memory.
static PROTECTED_PAGES: [AtomicU64; MAX_PROTECTED_PAGES] =
    [const { AtomicU64::new(0) }; MAX_PROTECTED_PAGES];

/// Protect, for the rest of the process's life, every page the restorer's
/// bytes touch. Returns false if the table cannot hold them; the caller must
/// then refuse the action. A page already protected stays protected either
/// way.
pub fn protect_restorer(restorer: u64) -> bool {
    protect_in(&PROTECTED_PAGES, restorer)
}

/// Whether [`protect_restorer`] would succeed for `restorer` now, without
/// protecting anything.
pub fn can_protect_restorer(restorer: u64) -> bool {
    room_in(&PROTECTED_PAGES, restorer).is_some()
}

/// The pages the restorer's bytes touch (one or two, with their count), if
/// the table has room for those it does not already hold.
fn room_in(table: &[AtomicU64], restorer: u64) -> Option<([u64; 2], usize)> {
    let last = restorer.checked_add(GLIBC_RESTORER_BYTES.len() as u64 - 1)?;
    let first_page = restorer & !(PAGE_SIZE - 1);
    let last_page = last & !(PAGE_SIZE - 1);
    let count = if first_page == last_page { 1 } else { 2 };
    let pages = [first_page, last_page];
    let missing = pages[..count]
        .iter()
        .filter(|page| !held_in(table, **page))
        .count();
    let free = table
        .iter()
        .filter(|slot| slot.load(Ordering::Acquire) == 0)
        .count();
    (missing <= free).then_some((pages, count))
}

fn held_in(table: &[AtomicU64], page: u64) -> bool {
    table
        .iter()
        .any(|slot| slot.load(Ordering::Acquire) == page)
}

fn protect_in(table: &[AtomicU64], restorer: u64) -> bool {
    let Some((needed, count)) = room_in(table, restorer) else {
        return false;
    };
    let pages = &needed[..count];
    let held = |page: u64| held_in(table, page);
    // Changed only inside a guest call's turn, so no two threads add pages at
    // once; compare_exchange keeps each slot sound regardless.
    pages.iter().all(|page| {
        held(*page)
            || table.iter().any(|slot| {
                slot.compare_exchange(0, *page, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
    })
}

/// Whether any protected page lies in `[start, start + length)`, the range
/// rounded out to whole pages as the kernel does. A range that wraps the
/// address space is treated as reaching its end.
pub fn overlaps_protected(start: u64, length: u64) -> bool {
    overlaps_in(&PROTECTED_PAGES, start, length)
}

fn overlaps_in(table: &[AtomicU64], start: u64, length: u64) -> bool {
    if length == 0 {
        return false;
    }
    let first = start & !(PAGE_SIZE - 1);
    let end = start.saturating_add(length);
    table.iter().any(|slot| {
        let page = slot.load(Ordering::Acquire);
        page != 0 && page >= first && page < end
    })
}

/// Whether the guest call `number(args)` could unmap, replace, reprotect or
/// advise a protected restorer page, and so must be refused (EPERM) before
/// the kernel. The inventory follows design section 4 and closure 3:
/// `munmap`, `mprotect`, `pkey_mprotect`, `madvise`, and `remap_file_pages`
/// over one; `mremap` of an overlapping source, or with `MREMAP_FIXED` to an
/// overlapping destination; `mmap` with `MAP_FIXED` or `MAP_FIXED_NOREPLACE`
/// over one; every `shmat` with `SHM_REMAP`; and every `process_madvise`
/// (its target may be this process, and its ranges are in a vector).
///
/// Ranges are the kernel's effective ones. A huge-page mapping replaces whole
/// pages of its size: `MAP_HUGETLB` is checked rounded out to its
/// `MAP_HUGE_*` size, or the default huge page size (`/proc/meminfo`; 1 GiB,
/// the largest on x86-64, when that cannot be read); a file on hugetlbfs to
/// its mount's page size (`fstatfs`); an `mremap` within 1 GiB of a protected
/// page to its source mapping's page size (`/proc/self/smaps`; refused when
/// that cannot be read). Every `shmat` with `SHM_REMAP` is refused: a
/// segment's page size, and so its effective extent, is not observable
/// before the attach.
/// `brk` cannot reach a protected page: [`glibc_restorer_accepted`] admits
/// only a mapping above the break.
pub fn mapping_change_refused(number: i64, args: [u64; 6]) -> bool {
    let facts = MappingFacts {
        hugetlbfs_page_size: &hugetlbfs_page_size,
        default_huge_page_size: &|| unsafe { default_huge_page_size() },
        page_size_at: &|address| unsafe { kernel_page_size_at(address) },
    };
    mapping_change_refused_in(&PROTECTED_PAGES, number, args, &facts)
}

/// What [`mapping_change_refused`] asks the kernel, separated for tests.
struct MappingFacts<'a> {
    /// The huge page size of a descriptor's file if it is on hugetlbfs.
    hugetlbfs_page_size: &'a dyn Fn(u64) -> Option<u64>,
    /// The default huge page size, for `MAP_HUGETLB` without a size, or
    /// `None` if it cannot be read.
    default_huge_page_size: &'a dyn Fn() -> Option<u64>,
    /// The kernel page size of the mapping containing an address, or `None`.
    page_size_at: &'a dyn Fn(u64) -> Option<u64>,
}

/// The largest huge page on x86-64.
const LARGEST_PAGE: u64 = 1 << 30;

/// `[start, start + length)` rounded out to multiples of `unit`, as
/// (start, length); the end saturates at the top of the address space.
fn round_out(start: u64, length: u64, unit: u64) -> (u64, u64) {
    let first = start & !(unit - 1);
    let end = start.saturating_add(length);
    let last = end
        .checked_add(unit - 1)
        .map_or(u64::MAX, |end| end & !(unit - 1));
    (first, last - first)
}

/// hugetlbfs's file-system magic.
const HUGETLBFS_MAGIC: i64 = 0x9584_58f6;

/// If descriptor `fd` is a file on hugetlbfs, its huge page size (the
/// mount's block size).
fn hugetlbfs_page_size(fd: u64) -> Option<u64> {
    let mut status: libc::statfs = unsafe { core::mem::zeroed() };
    let result = unsafe {
        raw_syscall6(
            libc::SYS_fstatfs,
            [fd, (&raw mut status) as u64, 0, 0, 0, 0],
        )
    };
    // `f_type`'s and `f_bsize`'s widths differ between targets.
    #[allow(clippy::unnecessary_cast)]
    let (magic, block) = (status.f_type as i64, status.f_bsize as u64);
    (result == 0 && magic == HUGETLBFS_MAGIC).then_some(block)
}

/// The default huge page size, from `/proc/meminfo`'s `Hugepagesize:` line.
unsafe fn default_huge_page_size() -> Option<u64> {
    unsafe {
        scan_proc_lines(c"/proc/meminfo", |line| {
            let rest = line.strip_prefix(b"Hugepagesize:")?;
            let digits: Vec<u8> = rest
                .iter()
                .copied()
                .skip_while(|byte| *byte == b' ' || *byte == b'\t')
                .take_while(u8::is_ascii_digit)
                .collect();
            core::str::from_utf8(&digits)
                .ok()?
                .parse::<u64>()
                .ok()?
                .checked_mul(1024)
        })
    }
}

/// Parse a `/proc/self/smaps` `KernelPageSize:` line into bytes.
pub fn kernel_page_size_line(line: &[u8]) -> Option<u64> {
    let rest = line.strip_prefix(b"KernelPageSize:")?;
    let digits: Vec<u8> = rest
        .iter()
        .copied()
        .skip_while(|byte| *byte == b' ' || *byte == b'\t')
        .take_while(u8::is_ascii_digit)
        .collect();
    let kilobytes = core::str::from_utf8(&digits).ok()?.parse::<u64>().ok()?;
    kilobytes.checked_mul(1024)
}

/// The kernel page size of the mapping containing `address`, from
/// `/proc/self/smaps`; `None` if it cannot be read.
unsafe fn kernel_page_size_at(address: u64) -> Option<u64> {
    let mut inside = false;
    unsafe {
        scan_proc_lines(c"/proc/self/smaps", |line| {
            if line.starts_with(b"KernelPageSize:") {
                if inside {
                    return kernel_page_size_line(line);
                }
            } else if line.first().is_some_and(u8::is_ascii_hexdigit)
                && line
                    .iter()
                    .take_while(|byte| **byte != b' ')
                    .any(|byte| *byte == b'-')
            {
                inside = maps_line_identity(line, address).is_some();
            }
            None
        })
    }
}

fn mapping_change_refused_in(
    table: &[AtomicU64],
    number: i64,
    args: [u64; 6],
    facts: &MappingFacts<'_>,
) -> bool {
    if !table.iter().any(|slot| slot.load(Ordering::Acquire) != 0) {
        return false;
    }
    let overlaps = |start, length| overlaps_in(table, start, length);
    let overlaps_rounded = |start, length, unit| {
        let (start, length) = round_out(start, length, unit);
        overlaps_in(table, start, length)
    };
    match number {
        libc::SYS_munmap
        | libc::SYS_mprotect
        | libc::SYS_pkey_mprotect
        | libc::SYS_madvise
        | libc::SYS_remap_file_pages => overlaps(args[0], args[1]),
        // Its ranges are in a vector, and its target may be this process:
        // refused while any page is protected (signal phase 1 takes the
        // refusal, msg1635).
        libc::SYS_process_madvise => true,
        libc::SYS_mremap => {
            let flags = args[3] as i32;
            let fixed = flags & libc::MREMAP_FIXED != 0;
            let reaches = |unit| {
                overlaps_rounded(args[0], args[1].max(1), unit)
                    || (fixed && overlaps_rounded(args[4], args[2], unit))
            };
            if !reaches(LARGEST_PAGE) {
                return false;
            }
            // Close enough that a huge-page source could reach: check at
            // the source's own page size.
            match (facts.page_size_at)(args[0]) {
                Some(unit) if unit.is_power_of_two() && unit >= PAGE_SIZE => reaches(unit),
                _ => true,
            }
        }
        libc::SYS_mmap => {
            let flags = args[3] as i32;
            if flags & (libc::MAP_FIXED | libc::MAP_FIXED_NOREPLACE) == 0 {
                return false;
            }
            // A huge-page mapping replaces whole pages of its size: the
            // MAP_HUGE_* size, else the default; a hugetlbfs file's own.
            let unit = if flags & libc::MAP_HUGETLB != 0 {
                let encoded = (flags >> libc::MAP_HUGE_SHIFT) & libc::MAP_HUGE_MASK;
                if encoded != 0 {
                    1_u64.checked_shl(encoded as u32).unwrap_or(LARGEST_PAGE)
                } else {
                    (facts.default_huge_page_size)().unwrap_or(LARGEST_PAGE)
                }
            } else if flags & libc::MAP_ANONYMOUS == 0 {
                (facts.hugetlbfs_page_size)(args[4]).unwrap_or(PAGE_SIZE)
            } else {
                PAGE_SIZE
            };
            if !unit.is_power_of_two() || unit < PAGE_SIZE {
                return true;
            }
            overlaps_rounded(args[0], args[1], unit)
        }
        // A segment's page size (a SHM_HUGETLB segment attaches whole huge
        // pages) is not observable before the attach, so its effective
        // extent cannot be established: every SHM_REMAP attach is refused
        // while a restorer page is protected (signal phase 1 takes the
        // refusal, msg1635). Without SHM_REMAP the kernel itself refuses to
        // attach over an existing mapping.
        libc::SYS_shmat => args[2] as i32 & libc::SHM_REMAP != 0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_flags_that_change_a_mapping_on_fork_are_recognized() {
        assert!(!vm_flags_change_on_fork(b"VmFlags: rd ex mr mw me sd"));
        assert!(vm_flags_change_on_fork(b"VmFlags: rd ex mr mw me dc sd"));
        assert!(vm_flags_change_on_fork(b"VmFlags: rd ex wf"));
        assert!(!vm_flags_change_on_fork(b"VmFlags: rd ex dd"));
        assert!(!vm_flags_change_on_fork(b"Rss: 4 kB"));
    }

    /// Only the C library's own mapping qualifies: an executable alias of its
    /// file holding the same bytes is refused, read-only or writable. And the
    /// C library's own mapping is refused once `MADV_DONTFORK` marks it (a
    /// fork child would lose it), checked in a child process so this test
    /// process keeps its C library across later forks.
    #[test]
    fn only_the_c_librarys_own_mapping_qualifies() {
        record_libc_identity().unwrap();
        let restorer = unsafe {
            let mut installed: libc::sigaction = core::mem::zeroed();
            let mut action: libc::sigaction = core::mem::zeroed();
            action.sa_sigaction = libc::SIG_IGN;
            let mut previous: libc::sigaction = core::mem::zeroed();
            assert_eq!(libc::sigaction(libc::SIGURG, &action, &mut previous), 0);
            assert_eq!(
                libc::sigaction(libc::SIGURG, core::ptr::null(), &mut installed),
                0
            );
            assert_eq!(
                libc::sigaction(libc::SIGURG, &previous, core::ptr::null_mut()),
                0
            );
            installed.sa_restorer.unwrap() as usize as u64
        };
        // Where the restorer lies in the C library's file.
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let (start, offset, path) = maps
            .lines()
            .find_map(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                let (low, high) = fields[0].split_once('-')?;
                let low = u64::from_str_radix(low, 16).ok()?;
                let high = u64::from_str_radix(high, 16).ok()?;
                (low <= restorer && restorer < high).then(|| {
                    (
                        low,
                        u64::from_str_radix(fields[2], 16).unwrap(),
                        fields[5].to_owned(),
                    )
                })
            })
            .unwrap();
        let file_offset = restorer - start + offset;
        let page_offset = file_offset & !4095;
        let path = std::ffi::CString::new(path).unwrap();
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        assert!(fd >= 0);
        let alias = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                8192,
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_PRIVATE,
                fd,
                page_offset as libc::off_t,
            )
        };
        assert_ne!(alias, libc::MAP_FAILED);
        unsafe { libc::close(fd) };
        let aliased = alias as u64 + (file_offset - page_offset);
        assert!(unsafe { glibc_restorer_accepted(restorer) });
        assert!(!unsafe { glibc_restorer_accepted(aliased) });
        assert_eq!(unsafe { libc::munmap(alias, 8192) }, 0);
        let child = unsafe { libc::fork() };
        if child == 0 {
            let page = restorer & !4095;
            let marked =
                unsafe { libc::madvise(page as *mut libc::c_void, 4096, libc::MADV_DONTFORK) } == 0;
            let refused = !unsafe { glibc_restorer_accepted(restorer) };
            unsafe { libc::_exit(if marked && refused { 0 } else { 1 }) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "{status:#x}"
        );
        // A writable executable alias of the same file: ordinary stores could
        // change the bytes, so it is refused.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        assert!(fd >= 0);
        let writable = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                8192,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE,
                fd,
                page_offset as libc::off_t,
            )
        };
        assert_ne!(writable, libc::MAP_FAILED);
        unsafe { libc::close(fd) };
        let aliased = writable as u64 + (file_offset - page_offset);
        assert!(!unsafe { glibc_restorer_accepted(aliased) });
        assert_eq!(unsafe { libc::munmap(writable, 8192) }, 0);
    }

    #[test]
    fn maps_lines_give_the_containing_mapping_identity() {
        let line = b"7f1234560000-7f1234570000 r-xp 00002000 fd:01 1234                       /usr/lib64/libc.so.6";
        assert_eq!(
            maps_line_identity(line, 0x7f12_3456_0010),
            Some(MappingIdentity {
                start: 0x7f12_3456_0000,
                end: 0x7f12_3457_0000,
                executable: true,
                writable: false,
                device: (0xfd << 32) | 1,
                inode: 1234,
            })
        );
        assert_eq!(maps_line_identity(line, 0x7f12_3457_0000), None);
        let data = b"55d000000000-55d000001000 rw-p 00000000 00:00 0 [heap]";
        let heap = maps_line_identity(data, 0x55d0_0000_0010).unwrap();
        assert!(!heap.executable);
        assert_eq!(heap.inode, 0);
        assert_eq!(maps_line_identity(b"garbage", 0), None);
    }

    #[test]
    fn a_restorer_mapping_must_be_executable_and_the_c_library_file() {
        let libc = MappingIdentity {
            start: 0x7000_0000,
            end: 0x7010_0000,
            executable: true,
            writable: false,
            device: 7,
            inode: 99,
        };
        assert!(restorer_mapping_accepted(0x7000_1000, libc, libc));
        let not_executable = MappingIdentity {
            executable: false,
            ..libc
        };
        // A writable alias would let ordinary guest stores change the bytes.
        let writable = MappingIdentity {
            writable: true,
            ..libc
        };
        assert!(!restorer_mapping_accepted(0x7000_1000, writable, libc));
        assert!(!restorer_mapping_accepted(
            0x7000_1000,
            not_executable,
            libc
        ));
        let anonymous = MappingIdentity { inode: 0, ..libc };
        assert!(!restorer_mapping_accepted(
            0x7000_1000,
            anonymous,
            anonymous
        ));
        // The same file mapped elsewhere (an alias the guest owns).
        let alias = MappingIdentity {
            start: 0x9000_0000,
            end: 0x9010_0000,
            ..libc
        };
        assert!(!restorer_mapping_accepted(0x9000_1000, alias, libc));
        let other_file = MappingIdentity { inode: 100, ..libc };
        assert!(!restorer_mapping_accepted(0x7000_1000, other_file, libc));
        let other_device = MappingIdentity { device: 8, ..libc };
        assert!(!restorer_mapping_accepted(0x7000_1000, other_device, libc));
        // The nine bytes must end inside the mapping.
        assert!(restorer_mapping_accepted(0x7010_0000 - 9, libc, libc));
        assert!(!restorer_mapping_accepted(0x7010_0000 - 8, libc, libc));
    }

    fn table() -> [AtomicU64; MAX_PROTECTED_PAGES] {
        [const { AtomicU64::new(0) }; MAX_PROTECTED_PAGES]
    }

    #[test]
    fn protection_covers_every_page_the_restorer_touches_and_refuses_a_full_table() {
        let pages = table();
        assert!(protect_in(&pages, 0x7000_1100));
        assert_eq!(pages[0].load(Ordering::Relaxed), 0x7000_1000);
        // Protecting it again takes no slot.
        assert!(protect_in(&pages, 0x7000_1100));
        assert_eq!(
            pages
                .iter()
                .filter(|s| s.load(Ordering::Relaxed) != 0)
                .count(),
            1
        );
        // Nine bytes crossing a page boundary protect both pages.
        assert!(protect_in(&pages, 0x7000_2ffc));
        assert_eq!(pages[1].load(Ordering::Relaxed), 0x7000_2000);
        assert_eq!(pages[2].load(Ordering::Relaxed), 0x7000_3000);
        // One slot left: a crossing restorer needing two new pages is refused
        // and takes nothing.
        assert!(!protect_in(&pages, 0x7000_5ffc));
        assert_eq!(pages[3].load(Ordering::Relaxed), 0);
        assert!(protect_in(&pages, 0x7000_6000));
        assert!(!protect_in(&pages, 0x7000_8000));
        assert!(!protect_in(&pages, u64::MAX - 3));
    }

    #[test]
    fn every_mapping_change_over_a_protected_page_is_refused() {
        let pages = table();
        let page = 0x7000_1000_u64;
        let two_megabytes = 2_u64 << 20;
        // Descriptor 7 is a file on a 2-MiB hugetlbfs. The default huge page is 2 MiB. The
        // mapping at 0x8000_0000 has 2-MiB pages, the one at 0x5555_0000 an
        // unreadable page size, every other one 4-KiB pages.
        let hugetlbfs = move |fd: u64| (fd == 7).then_some(two_megabytes);
        let default_huge = move || Some(two_megabytes);
        let page_size = |address: u64| match address {
            0x8000_0000 => Some(2 << 20),
            0x5555_0000 => None,
            _ => Some(4096),
        };
        let facts = MappingFacts {
            hugetlbfs_page_size: &hugetlbfs,
            default_huge_page_size: &default_huge,
            page_size_at: &page_size,
        };
        let call =
            |number: i64, args: [u64; 6]| mapping_change_refused_in(&pages, number, args, &facts);
        let far = 0x1_0000_0000_u64;
        // Nothing protected: nothing refused, not even SHM_REMAP.
        assert!(!call(libc::SYS_munmap, [page, 4096, 0, 0, 0, 0]));
        assert!(!call(
            libc::SYS_shmat,
            [2, page, libc::SHM_REMAP as u64, 0, 0, 0]
        ));
        assert!(protect_in(&pages, page + 0x10));
        for number in [
            libc::SYS_munmap,
            libc::SYS_mprotect,
            libc::SYS_pkey_mprotect,
            libc::SYS_madvise,
            libc::SYS_remap_file_pages,
        ] {
            assert!(call(number, [page, 4096, 0, 0, 0, 0]), "{number}");
            // A range that only reaches into the page by one byte.
            assert!(call(number, [page - 4096, 4097, 0, 0, 0, 0]), "{number}");
            // An unaligned start inside the page.
            assert!(call(number, [page + 0x800, 1, 0, 0, 0, 0]), "{number}");
            // Adjacent pages and an empty range are not refused.
            assert!(!call(number, [page - 4096, 4096, 0, 0, 0, 0]), "{number}");
            assert!(!call(number, [page + 4096, 4096, 0, 0, 0, 0]), "{number}");
            assert!(!call(number, [page, 0, 0, 0, 0, 0]), "{number}");
            // A wrapping length reaches the page.
            assert!(
                call(number, [page - 4096, u64::MAX, 0, 0, 0, 0]),
                "{number}"
            );
        }
        let maymove = libc::MREMAP_MAYMOVE as u64;
        let fixed = maymove | libc::MREMAP_FIXED as u64;
        // An overlapping source, at any page size.
        assert!(call(libc::SYS_mremap, [page, 4096, 8192, 0, 0, 0]));
        assert!(call(libc::SYS_mremap, [page, 0, 4096, maymove, 0, 0]));
        // A fixed destination over the page; not without MREMAP_FIXED, and
        // not beside it (below or above) from a 4-KiB source.
        assert!(call(
            libc::SYS_mremap,
            [0x9000_0000, 4096, 4096, fixed, page, 0]
        ));
        assert!(!call(
            libc::SYS_mremap,
            [0x9000_0000, 4096, 4096, maymove, page, 0]
        ));
        assert!(!call(
            libc::SYS_mremap,
            [0x9000_0000, 4096, 4096, fixed, page + 4096, 0]
        ));
        assert!(!call(
            libc::SYS_mremap,
            [0x9000_0000, 4096, 4096, fixed, page - 4096, 0]
        ));
        // From a 2-MiB source, the destination below the page is rounded
        // over it.
        assert!(call(
            libc::SYS_mremap,
            [0x8000_0000, 4096, 4096, fixed, page - 4096, 0]
        ));
        // A source near enough to matter whose page size cannot be read.
        assert!(call(
            libc::SYS_mremap,
            [0x5555_0000, 4096, 8192, maymove, 0, 0]
        ));
        // Far away: decided without asking.
        assert!(!call(libc::SYS_mremap, [far, 4096, 8192, maymove, 0, 0]));
        let private = (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64;
        // The 2-MiB-aligned window holding the page, and the windows beside it.
        let window = page & !(two_megabytes - 1);
        for flags in [libc::MAP_FIXED, libc::MAP_FIXED_NOREPLACE] {
            let flags = flags as u64 | private;
            assert!(call(libc::SYS_mmap, [page, 4096, 0, flags, u64::MAX, 0]));
            assert!(!call(
                libc::SYS_mmap,
                [page + 4096, 4096, 0, flags, u64::MAX, 0]
            ));
            assert!(!call(
                libc::SYS_mmap,
                [page - 4096, 4096, 0, flags, u64::MAX, 0]
            ));
            // MAP_HUGETLB replaces whole huge pages: a 4-KiB request below
            // the page inside its 2-MiB window is refused, the 2-MiB windows
            // beside it are not; with an explicit 1-GiB size, the window
            // below is within the page's gigabyte.
            for huge in [
                flags | libc::MAP_HUGETLB as u64,
                flags | (libc::MAP_HUGETLB | libc::MAP_HUGE_2MB) as u64,
            ] {
                assert!(call(
                    libc::SYS_mmap,
                    [page - 4096, 4096, 0, huge, u64::MAX, 0]
                ));
                assert!(!call(
                    libc::SYS_mmap,
                    [window - two_megabytes, two_megabytes, 0, huge, u64::MAX, 0]
                ));
                assert!(!call(
                    libc::SYS_mmap,
                    [window + two_megabytes, two_megabytes, 0, huge, u64::MAX, 0]
                ));
            }
            let gigantic = flags | (libc::MAP_HUGETLB | libc::MAP_HUGE_1GB) as u64;
            assert!(call(
                libc::SYS_mmap,
                [
                    window - two_megabytes,
                    two_megabytes,
                    0,
                    gigantic,
                    u64::MAX,
                    0
                ]
            ));
            // A file on the 2-MiB hugetlbfs, but not an ordinary file.
            let file = flags & !(libc::MAP_ANONYMOUS as u64);
            assert!(call(libc::SYS_mmap, [page - 4096, 4096, 0, file, 7, 0]));
            assert!(!call(
                libc::SYS_mmap,
                [window + two_megabytes, two_megabytes, 0, file, 7, 0]
            ));
            assert!(!call(libc::SYS_mmap, [page - 4096, 4096, 0, file, 3, 0]));
        }
        assert!(!call(libc::SYS_mmap, [page, 4096, 0, private, u64::MAX, 0]));
        // Every SHM_REMAP attach is refused, wherever it lands; without
        // SHM_REMAP the kernel refuses to attach over a mapping itself.
        let remap = libc::SHM_REMAP as u64;
        assert!(call(libc::SYS_shmat, [1, page, remap, 0, 0, 0]));
        assert!(call(libc::SYS_shmat, [1, far, remap, 0, 0, 0]));
        assert!(call(
            libc::SYS_shmat,
            [1, 0, remap | libc::SHM_RND as u64, 0, 0, 0]
        ));
        assert!(!call(libc::SYS_shmat, [1, page, 0, 0, 0, 0]));
        assert!(!call(libc::SYS_brk, [page, 0, 0, 0, 0, 0]));
        // process_madvise is refused whatever its vector names.
        assert!(call(libc::SYS_process_madvise, [3, 0x9000, 1, 0, 0, 0]));
    }

    #[test]
    fn kernel_page_size_lines_are_parsed() {
        assert_eq!(
            kernel_page_size_line(b"KernelPageSize:        4 kB"),
            Some(4096)
        );
        assert_eq!(
            kernel_page_size_line(b"KernelPageSize:     2048 kB"),
            Some(2 << 20)
        );
        assert_eq!(kernel_page_size_line(b"MMUPageSize:        4 kB"), None);
        assert_eq!(round_out(0x7000_1000, 1, 1 << 30), (0x4000_0000, 1 << 30));
        assert_eq!(round_out(0x1000, 0x2000, 0x1000), (0x1000, 0x2000));
    }

    /// glibc's own restorer, as `sigaction` installs it, is accepted; its
    /// bytes copied to a heap buffer or to an anonymous executable mapping,
    /// the runtime's private restorer, and another address in the C library
    /// are each refused.
    #[test]
    fn only_glibc_restorer_in_the_c_library_is_accepted() {
        record_libc_identity().unwrap();
        let restorer = unsafe {
            let mut action: libc::sigaction = core::mem::zeroed();
            action.sa_sigaction = libc::SIG_IGN;
            let mut previous: libc::sigaction = core::mem::zeroed();
            assert_eq!(libc::sigaction(libc::SIGURG, &action, &mut previous), 0);
            let mut installed: libc::sigaction = core::mem::zeroed();
            assert_eq!(
                libc::sigaction(libc::SIGURG, core::ptr::null(), &mut installed),
                0
            );
            assert_eq!(
                libc::sigaction(libc::SIGURG, &previous, core::ptr::null_mut()),
                0
            );
            installed.sa_restorer.expect("glibc installs a restorer") as usize as u64
        };
        assert!(unsafe { glibc_restorer_accepted(restorer) });

        let copy = Box::new(GLIBC_RESTORER_BYTES);
        assert!(!unsafe { glibc_restorer_accepted(copy.as_ptr() as u64) });

        let executable = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(executable, libc::MAP_FAILED);
        unsafe {
            core::ptr::copy_nonoverlapping(
                GLIBC_RESTORER_BYTES.as_ptr(),
                executable.cast::<u8>(),
                GLIBC_RESTORER_BYTES.len(),
            )
        };
        assert!(!unsafe { glibc_restorer_accepted(executable as u64) });
        assert_eq!(unsafe { libc::munmap(executable, 4096) }, 0);

        assert!(!unsafe { glibc_restorer_accepted(crate::signal::signal_restorer()) });
        assert!(!unsafe { glibc_restorer_accepted(restorer + 1) });
        assert!(!unsafe { glibc_restorer_accepted(0) });
        assert!(!unsafe { glibc_restorer_accepted(u64::MAX - 3) });
    }
}
