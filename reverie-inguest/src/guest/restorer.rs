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

/// An address inside this process's C library: the runtime runs in the
/// guest's own process, so it is the guest's C library too.
fn libc_anchor() -> u64 {
    libc::sigaction as *const () as u64
}

/// Whether `restorer` is glibc's `__restore_rt` in this process's C library:
/// its bytes are [`GLIBC_RESTORER_BYTES`], read without faulting, and they
/// lie in an executable, file-backed mapping of the same file (device and
/// inode) as the C library.
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
    let anchor = libc_anchor();
    let Some(mapping) = (unsafe { scan_own_maps(|line| maps_line_identity(line, restorer)) })
    else {
        return false;
    };
    let Some(libc) = (unsafe { scan_own_maps(|line| maps_line_identity(line, anchor)) }) else {
        return false;
    };
    restorer_mapping_accepted(restorer, mapping, libc)
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

fn protect_in(table: &[AtomicU64], restorer: u64) -> bool {
    let Some(last) = restorer.checked_add(GLIBC_RESTORER_BYTES.len() as u64 - 1) else {
        return false;
    };
    let first_page = restorer & !(PAGE_SIZE - 1);
    let last_page = last & !(PAGE_SIZE - 1);
    let needed = [first_page, last_page];
    let pages = if first_page == last_page {
        &needed[..1]
    } else {
        &needed[..]
    };
    let held = |page: u64| {
        table
            .iter()
            .any(|slot| slot.load(Ordering::Acquire) == page)
    };
    let missing = pages.iter().filter(|page| !held(**page)).count();
    let free = table
        .iter()
        .filter(|slot| slot.load(Ordering::Acquire) == 0)
        .count();
    if missing > free {
        return false;
    }
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
/// with `SHM_REMAP`, refused whenever any page is protected because the
/// segment's size is not in the call.
pub fn mapping_change_refused(number: i64, args: [u64; 6]) -> bool {
    mapping_change_refused_in(&PROTECTED_PAGES, number, args)
}

fn mapping_change_refused_in(table: &[AtomicU64], number: i64, args: [u64; 6]) -> bool {
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
            args[2] as i32 & libc::SHM_REMAP != 0
                && table.iter().any(|slot| slot.load(Ordering::Acquire) != 0)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let call = |number: i64, args: [u64; 6]| mapping_change_refused_in(&pages, number, args);
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
        assert!(call(
            libc::SYS_shmat,
            [1, 0x9000_0000, libc::SHM_REMAP as u64, 0, 0, 0]
        ));
        assert!(!call(libc::SYS_shmat, [1, 0x9000_0000, 0, 0, 0, 0]));
        assert!(!call(libc::SYS_brk, [page, 0, 0, 0, 0, 0]));
    }

    /// glibc's own restorer, as `sigaction` installs it, is accepted; its
    /// bytes copied to a heap buffer or to an anonymous executable mapping,
    /// the runtime's private restorer, and another address in the C library
    /// are each refused.
    #[test]
    fn only_glibc_restorer_in_the_c_library_is_accepted() {
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
        assert!(!unsafe { glibc_restorer_accepted(libc_anchor()) });
        assert!(!unsafe { glibc_restorer_accepted(0) });
        assert!(!unsafe { glibc_restorer_accepted(u64::MAX - 3) });
    }
}
