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
    if address < start || address >= end {
        return None;
    }
    let colon = device.iter().position(|byte| *byte == b':')?;
    let major = hex(&device[..colon])?;
    let minor = hex(&device[colon + 1..])?;
    Some(MappingIdentity {
        start,
        end,
        executable: permissions.get(2) == Some(&b'x'),
        device: (major << 32) | minor,
        inode: decimal(inode)?,
    })
}

/// Whether a restorer whose `GLIBC_RESTORER_BYTES.len()` bytes start at
/// `restorer`, inside `mapping`, lies in an executable mapping of the C
/// library's file `libc`.
pub fn restorer_mapping_accepted(
    restorer: u64,
    mapping: MappingIdentity,
    libc: MappingIdentity,
) -> bool {
    mapping.executable
        && mapping.inode != 0
        && mapping.device == libc.device
        && mapping.inode == libc.inode
        && restorer
            .checked_add(GLIBC_RESTORER_BYTES.len() as u64)
            .is_some_and(|end| end <= mapping.end)
}

/// The C library's file, as `major << 32 | minor` and inode, recorded once in
/// ordinary context by [`record_libc_identity`]; 0 before.
static LIBC_DEVICE: AtomicU64 = AtomicU64::new(0);
static LIBC_INODE: AtomicU64 = AtomicU64::new(0);

/// Record which file is this process's C library, from glibc's own
/// `sigaction` as `libc.so.6` defines it (looked up in that object, so an
/// interposed `sigaction`, for example from an `LD_PRELOAD` library, does
/// not count). The runtime runs in the guest's own process, so it is the
/// guest's C library too. Call once, at runtime initialization, in ordinary
/// context: it uses the dynamic loader.
pub fn record_libc_identity() -> std::io::Result<()> {
    let handle =
        unsafe { libc::dlopen(c"libc.so.6".as_ptr(), libc::RTLD_NOLOAD | libc::RTLD_LAZY) };
    if handle.is_null() {
        return Err(std::io::Error::other("libc.so.6 is not loaded"));
    }
    let anchor = unsafe { libc::dlsym(handle, c"sigaction".as_ptr()) } as u64;
    unsafe { libc::dlclose(handle) };
    if anchor == 0 {
        return Err(std::io::Error::other("libc.so.6 defines no sigaction"));
    }
    let identity = unsafe { scan_own_maps(|line| maps_line_identity(line, anchor)) }
        .filter(|identity| identity.inode != 0)
        .ok_or_else(|| std::io::Error::other("libc.so.6's sigaction is in no file mapping"))?;
    LIBC_DEVICE.store(identity.device, Ordering::Release);
    LIBC_INODE.store(identity.inode, Ordering::Release);
    Ok(())
}

fn recorded_libc() -> Option<MappingIdentity> {
    let inode = LIBC_INODE.load(Ordering::Acquire);
    (inode != 0).then(|| MappingIdentity {
        start: 0,
        end: 0,
        executable: true,
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
/// `munmap`, `mprotect`, `pkey_mprotect`, `madvise`; `mremap` of an
/// overlapping source, or with `MREMAP_FIXED` to an overlapping destination;
/// `mmap` with `MAP_FIXED` or `MAP_FIXED_NOREPLACE` over one; and `shmat`
/// with `SHM_REMAP` whose attachment, at its effective address (rounded
/// down to `SHMLBA` under `SHM_RND`) for the segment's size, covers one. A
/// `shmat` whose segment cannot be read (`IPC_STAT`) is left to the kernel,
/// which refuses an attach it cannot perform. `brk` cannot reach a protected
/// page: [`glibc_restorer_accepted`] admits only a mapping above the break.
pub fn mapping_change_refused(number: i64, args: [u64; 6]) -> bool {
    mapping_change_refused_in(&PROTECTED_PAGES, number, args, shm_segment_size)
}

/// The size of System V shared memory segment `id`, or `None`.
fn shm_segment_size(id: u64) -> Option<u64> {
    let mut status: libc::shmid_ds = unsafe { core::mem::zeroed() };
    let result = unsafe {
        raw_syscall6(
            libc::SYS_shmctl,
            [id, libc::IPC_STAT as u64, (&raw mut status) as u64, 0, 0, 0],
        )
    };
    (result >= 0).then_some(status.shm_segsz as u64)
}

fn mapping_change_refused_in(
    table: &[AtomicU64],
    number: i64,
    args: [u64; 6],
    segment_size: impl FnOnce(u64) -> Option<u64>,
) -> bool {
    let overlaps = |start, length| overlaps_in(table, start, length);
    match number {
        libc::SYS_munmap | libc::SYS_mprotect | libc::SYS_pkey_mprotect | libc::SYS_madvise => {
            overlaps(args[0], args[1])
        }
        libc::SYS_mremap => {
            let flags = args[3] as i32;
            overlaps(args[0], args[1].max(1))
                || (flags & libc::MREMAP_FIXED != 0 && overlaps(args[4], args[2]))
        }
        libc::SYS_mmap => {
            let flags = args[3] as i32;
            flags & (libc::MAP_FIXED | libc::MAP_FIXED_NOREPLACE) != 0 && overlaps(args[0], args[1])
        }
        libc::SYS_shmat => {
            let flags = args[2] as i32;
            if flags & libc::SHM_REMAP == 0
                || !table.iter().any(|slot| slot.load(Ordering::Acquire) != 0)
            {
                return false;
            }
            let Some(size) = segment_size(args[0]) else {
                return false;
            };
            // SHMLBA is the page size on x86-64.
            let start = if flags & libc::SHM_RND != 0 {
                args[1] & !(PAGE_SIZE - 1)
            } else {
                args[1]
            };
            overlaps(start, size.max(1))
        }
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

    /// An executable alias of the C library's file holding glibc's restorer
    /// is accepted like the original; after `MADV_DONTFORK` on it (a fork
    /// child would lose it), it is refused.
    #[test]
    fn a_restorer_mapping_a_fork_child_would_lose_is_refused() {
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
        assert!(unsafe { glibc_restorer_accepted(aliased) });
        assert_eq!(
            unsafe { libc::madvise(alias, 8192, libc::MADV_DONTFORK) },
            0
        );
        assert!(!unsafe { glibc_restorer_accepted(aliased) });
        assert_eq!(unsafe { libc::munmap(alias, 8192) }, 0);
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
            device: 7,
            inode: 99,
        };
        assert!(restorer_mapping_accepted(0x7000_1000, libc, libc));
        let not_executable = MappingIdentity {
            executable: false,
            ..libc
        };
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
        // Segment 1 has 8192 bytes; segment 2 cannot be read.
        let segment = |id: u64| (id == 1).then_some(8192);
        let call =
            |number: i64, args: [u64; 6]| mapping_change_refused_in(&pages, number, args, segment);
        // Nothing protected: nothing refused.
        assert!(!call(libc::SYS_munmap, [page, 4096, 0, 0, 0, 0]));
        assert!(!call(
            libc::SYS_shmat,
            [1, page, libc::SHM_REMAP as u64, 0, 0, 0]
        ));
        assert!(protect_in(&pages, page + 0x10));
        for number in [
            libc::SYS_munmap,
            libc::SYS_mprotect,
            libc::SYS_pkey_mprotect,
            libc::SYS_madvise,
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
        let fixed = libc::MREMAP_MAYMOVE as u64 | libc::MREMAP_FIXED as u64;
        assert!(call(libc::SYS_mremap, [page, 4096, 8192, 0, 0, 0]));
        assert!(call(
            libc::SYS_mremap,
            [page, 0, 4096, libc::MREMAP_MAYMOVE as u64, 0, 0]
        ));
        assert!(call(
            libc::SYS_mremap,
            [0x9000_0000, 4096, 4096, fixed, page, 0]
        ));
        assert!(!call(
            libc::SYS_mremap,
            [
                0x9000_0000,
                4096,
                4096,
                libc::MREMAP_MAYMOVE as u64,
                page,
                0
            ]
        ));
        assert!(!call(
            libc::SYS_mremap,
            [0x9000_0000, 4096, 4096, fixed, page + 4096, 0]
        ));
        for flags in [libc::MAP_FIXED, libc::MAP_FIXED_NOREPLACE] {
            let flags = (flags | libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64;
            assert!(call(libc::SYS_mmap, [page, 4096, 0, flags, u64::MAX, 0]));
            assert!(!call(
                libc::SYS_mmap,
                [page + 4096, 4096, 0, flags, u64::MAX, 0]
            ));
        }
        let hint = (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64;
        assert!(!call(libc::SYS_mmap, [page, 4096, 0, hint, u64::MAX, 0]));
        let remap = libc::SHM_REMAP as u64;
        let rounded = remap | libc::SHM_RND as u64;
        // An attachment whose 8192 bytes reach the page, from below or at it.
        assert!(call(libc::SYS_shmat, [1, page - 4096, remap, 0, 0, 0]));
        assert!(call(libc::SYS_shmat, [1, page, remap, 0, 0, 0]));
        // SHM_RND rounds an unaligned address down onto the page.
        assert!(call(libc::SYS_shmat, [1, page + 0x123, rounded, 0, 0, 0]));
        // Disjoint attachments, one ending just below the page.
        assert!(!call(libc::SYS_shmat, [1, page - 8192, remap, 0, 0, 0]));
        assert!(!call(libc::SYS_shmat, [1, page + 4096, remap, 0, 0, 0]));
        // Without SHM_REMAP, or with an unreadable segment, the kernel decides.
        assert!(!call(libc::SYS_shmat, [1, page, 0, 0, 0, 0]));
        assert!(!call(libc::SYS_shmat, [2, page, remap, 0, 0, 0]));
        assert!(!call(libc::SYS_brk, [page, 0, 0, 0, 0, 0]));
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
