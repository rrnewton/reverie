/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Whether this process is a host-fork copy of a run (PROTOTYPE).
//!
//! `fork()` copies only the calling thread. A copy of a run therefore lacks
//! every helper thread the run started, and must never wait for one: the
//! wait would never end. Code that joins such a thread asks
//! [`is_forked_child`] first.

use std::collections::BTreeMap;
use std::io;
use std::os::fd::RawFd;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static FORKED_CHILD: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Records that this process is a forked copy of a run, before any repair.
pub fn mark_forked_child() {
    FORKED_CHILD.store(true, Ordering::Relaxed);
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// How many times this process was marked a forked copy: per-thread caches
/// of kernel thread IDs compare it with the value they were filled under.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Whether [`mark_forked_child`] ran in this process.
pub fn is_forked_child() -> bool {
    FORKED_CHILD.load(Ordering::Relaxed)
}

/// One mapping, in this process, of a memfd mapped copy-on-write
/// ([`register_private_memfd`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateMapping {
    pub start: usize,
    pub end: usize,
    /// The file offset the mapping starts at.
    pub offset: u64,
    pub prot: libc::c_int,
}

struct PrivateMemfd {
    name: String,
    mappings: Vec<PrivateMapping>,
}

static ANY_PRIVATE: AtomicBool = AtomicBool::new(false);
static MATERIALIZED: AtomicU64 = AtomicU64::new(0);
static MATERIALIZED_BYTES: AtomicU64 = AtomicU64::new(0);
/// Keyed by the memfd's device and inode: an inode number alone may also
/// name a file on another file system.
static PRIVATE: Mutex<BTreeMap<(u64, u64), PrivateMemfd>> = Mutex::new(BTreeMap::new());

fn stat_of(fd: RawFd) -> io::Result<libc::stat> {
    // SAFETY: fstat of a descriptor into a zeroed buffer.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::fstat(fd, &mut stat) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat)
}

/// Records that this copy maps the memfd with device `device` and inode
/// `inode` only through the
/// private copy-on-write `mappings` (PROTOTYPE): the memfd itself is the
/// template's, which no process writes any more, so its descriptor shows
/// the template's bytes, not this copy's, until
/// [`materialize_private_memfd`] gives the copy a memfd of its own.
pub fn register_private_memfd(device: u64, inode: u64, name: &str, mappings: Vec<PrivateMapping>) {
    PRIVATE.lock().unwrap_or_else(|e| e.into_inner()).insert(
        (device, inode),
        PrivateMemfd {
            name: name.to_string(),
            mappings,
        },
    );
    ANY_PRIVATE.store(true, Ordering::Release);
}

/// How many memfds this process still maps copy-on-write, how many it gave
/// a memfd of its own ([`materialize_private_memfd`]), and the bytes it
/// copied for them.
pub fn private_memfd_counts() -> (usize, u64, u64) {
    (
        PRIVATE.lock().unwrap_or_else(|e| e.into_inner()).len(),
        MATERIALIZED.load(Ordering::Relaxed),
        MATERIALIZED_BYTES.load(Ordering::Relaxed),
    )
}

/// Whether `fd` names a memfd this process maps copy-on-write
/// ([`register_private_memfd`]).
pub fn is_private_memfd(fd: RawFd) -> bool {
    if !ANY_PRIVATE.load(Ordering::Acquire) {
        return false;
    }
    let Ok(stat) = stat_of(fd) else {
        return false;
    };
    PRIVATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&(stat.st_dev, stat.st_ino))
}

fn new_memfd(name: &str) -> io::Result<RawFd> {
    let name = std::ffi::CString::new(name).map_err(io::Error::other)?;
    for flags in [libc::MFD_CLOEXEC | libc::MFD_NOEXEC_SEAL, libc::MFD_CLOEXEC] {
        // SAFETY: a NUL-terminated name; a new descriptor owned by the caller.
        let fd = unsafe { libc::memfd_create(name.as_ptr(), flags) };
        if fd >= 0 {
            return Ok(fd);
        }
    }
    Err(io::Error::last_os_error())
}

/// The data extents of `fd` within `[start, end)`, as file ranges.
fn data_extents(fd: RawFd, start: u64, end: u64) -> Vec<(u64, u64)> {
    let mut extents = Vec::new();
    let mut cursor = start as libc::off_t;
    let end = end as libc::off_t;
    while cursor < end {
        // SAFETY: lseek on a live descriptor.
        let data = unsafe { libc::lseek(fd, cursor, libc::SEEK_DATA) };
        if data < 0 || data >= end {
            break;
        }
        // SAFETY: as above.
        let hole = unsafe { libc::lseek(fd, data, libc::SEEK_HOLE) };
        let hole = if hole < 0 { end } else { hole.min(end) };
        extents.push((data as u64, hole as u64));
        cursor = hole;
    }
    extents
}

/// One mapping of this process, clipped to a range, from /proc/self/maps.
struct Vma {
    start: usize,
    end: usize,
    /// The file offset at `start`; 0 for anonymous memory.
    offset: u64,
    /// The file's inode; 0 for anonymous memory.
    inode: u64,
}

/// The mappings of this process within `[start, end)`.
fn vmas_within(start: usize, end: usize) -> io::Result<Vec<Vma>> {
    let maps = std::fs::read_to_string("/proc/self/maps")?;
    let mut vmas = Vec::new();
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let (Some(range), Some(_perms), Some(offset), Some(_device), Some(inode)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let Some((low, high)) = range.split_once('-') else {
            continue;
        };
        let (Ok(low), Ok(high)) = (
            usize::from_str_radix(low, 16),
            usize::from_str_radix(high, 16),
        ) else {
            continue;
        };
        if high <= start || low >= end {
            continue;
        }
        let from = low.max(start);
        vmas.push(Vma {
            start: from,
            end: high.min(end),
            offset: u64::from_str_radix(offset, 16).unwrap_or(0) + (from - low) as u64,
            inode: inode.parse().unwrap_or(0),
        });
    }
    Ok(vmas)
}

/// The runs of pages of `[start, end)` (page-aligned) that are resident or
/// swapped, from /proc/self/pagemap.
fn resident_runs(start: usize, end: usize) -> io::Result<Vec<(usize, usize)>> {
    use std::os::unix::fs::FileExt;
    const PAGE: usize = 4096;
    const BATCH: usize = 8192;
    let pagemap = std::fs::File::open("/proc/self/pagemap")?;
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut buffer = vec![0u8; 8 * BATCH];
    let mut address = start;
    while address < end {
        let pages = ((end - address) / PAGE).min(BATCH);
        let entries = &mut buffer[..8 * pages];
        pagemap.read_exact_at(entries, (address / PAGE * 8) as u64)?;
        for (index, entry) in entries.chunks_exact(8).enumerate() {
            let entry = u64::from_ne_bytes(entry.try_into().expect("eight bytes"));
            // Bit 63: present; bit 62: swapped.
            if entry & (3 << 62) == 0 {
                continue;
            }
            let page = address + index * PAGE;
            match runs.last_mut() {
                Some((_, run_end)) if *run_end == page => *run_end = page + PAGE,
                _ => runs.push((page, page + PAGE)),
            }
        }
        address += pages * PAGE;
    }
    Ok(runs)
}

/// Discards `[address, address + length)` of a copy-on-write mapping
/// ([`register_private_memfd`]) so that it reads as zeros, by replacing it
/// with private anonymous memory, and returns whether the range lay in one.
/// The caller must then not discard it through the file: an `MADV_REMOVE`
/// would reach the template's shared file, and a private view refuses it.
pub fn discard_private(address: usize, length: usize) -> io::Result<bool> {
    if !ANY_PRIVATE.load(Ordering::Acquire) || length == 0 {
        return Ok(false);
    }
    let registry = PRIVATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(prot) = registry
        .values()
        .flat_map(|entry| entry.mappings.iter())
        .find(|mapping| mapping.start <= address && address + length <= mapping.end)
        .map(|mapping| mapping.prot)
    else {
        return Ok(false);
    };
    // SAFETY: replaces part of this process's own copy-on-write mapping, at
    // the same address, with zero-filled private memory.
    let mapped = unsafe {
        libc::mmap(
            address as *mut libc::c_void,
            length,
            prot,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    if mapped as usize != address {
        return Err(io::Error::last_os_error());
    }
    // As the anonymous memory a copy's repair maps over a hole (huge pages
    // where the replaced shared memory had them). Advice only.
    // SAFETY: advice on the range just mapped.
    unsafe { libc::madvise(address as *mut libc::c_void, length, libc::MADV_HUGEPAGE) };
    Ok(true)
}

/// Gives a memfd this process maps copy-on-write ([`register_private_memfd`])
/// a memfd of its own before anything uses the descriptor `fd` to reach its
/// bytes: a new memfd of the same size receives this process's bytes, read
/// through its private mappings, which replace them at the same addresses
/// as shared mappings of the new memfd, and every descriptor of the old
/// memfd in this process is replaced by one of the new memfd. Does nothing
/// for any other descriptor.
///
/// The bytes come from the mappings as they are now: within a private view
/// of the old memfd, every page it read or wrote lies in a data extent of
/// the file (a fault on a private view first reads the file's page, which
/// allocates it there); within private anonymous memory (a hole of the file
/// when the copy was made, or a range discarded since, [`discard_private`]),
/// the resident pages. Any other page reads as zeros and stays a hole of
/// the new memfd.
///
/// The caller must keep any vCPU that could write these mappings stopped
/// meanwhile, or that write may be lost (PROTOTYPE: a tool that runs one
/// guest thread at a time satisfies this while it handles a syscall; the
/// entry gate is the general answer).
pub fn materialize_private_memfd(fd: RawFd) -> io::Result<()> {
    if !ANY_PRIVATE.load(Ordering::Acquire) {
        return Ok(());
    }
    let stat = stat_of(fd)?;
    let mut registry = PRIVATE.lock().unwrap_or_else(|e| e.into_inner());
    let key = (stat.st_dev, stat.st_ino);
    let Some(entry) = registry.get(&key) else {
        return Ok(());
    };
    let new = new_memfd(&entry.name)?;
    let fail = |what: &str| -> io::Error {
        let error = io::Error::last_os_error();
        // SAFETY: the memfd created above, not yet installed anywhere.
        unsafe { libc::close(new) };
        io::Error::new(error.kind(), format!("{what}: {error}"))
    };
    // SAFETY: the new memfd.
    if unsafe { libc::ftruncate(new, stat.st_size) } < 0 {
        return Err(fail("ftruncate"));
    }
    let mut copied = 0u64;
    for mapping in &entry.mappings {
        if mapping.prot & libc::PROT_READ == 0 {
            // SAFETY: the memfd created above.
            unsafe { libc::close(new) };
            return Err(io::Error::other(format!(
                "copy-on-write mapping at {:#x} is not readable",
                mapping.start
            )));
        }
        let vmas = match vmas_within(mapping.start, mapping.end) {
            Ok(vmas) => vmas,
            Err(error) => {
                // SAFETY: the memfd created above.
                unsafe { libc::close(new) };
                return Err(error);
            }
        };
        for vma in vmas {
            let runs: Vec<(usize, usize)> = if vma.inode == key.1 {
                // A private view of the shared file: a page it read or wrote
                // lies in a data extent of the file (a fault on a private view
                // first reads the file's page, which allocates it there).
                data_extents(fd, vma.offset, vma.offset + (vma.end - vma.start) as u64)
                    .into_iter()
                    .map(|(data, hole)| {
                        (
                            vma.start + (data - vma.offset) as usize,
                            vma.start + (hole - vma.offset) as usize,
                        )
                    })
                    .collect()
            } else if vma.inode == 0 {
                // Private anonymous memory (a hole of the file when the copy
                // was made, or a range discarded since): its resident pages.
                match resident_runs(vma.start, vma.end) {
                    Ok(runs) => runs,
                    Err(error) => {
                        // SAFETY: the memfd created above.
                        unsafe { libc::close(new) };
                        return Err(error);
                    }
                }
            } else {
                // SAFETY: the memfd created above.
                unsafe { libc::close(new) };
                return Err(io::Error::other(format!(
                    "unexpected mapping at {:#x} in a copy-on-write range",
                    vma.start
                )));
            };
            for (from, to) in runs {
                let offset = mapping.offset + (from - mapping.start) as u64;
                let mut done = 0usize;
                while from + done < to {
                    // SAFETY: reads this process's own readable mapping into
                    // the new memfd at the same file offset.
                    let written = unsafe {
                        libc::pwrite(
                            new,
                            (from + done) as *const libc::c_void,
                            to - from - done,
                            (offset + done as u64) as libc::off_t,
                        )
                    };
                    if written <= 0 {
                        return Err(fail("copying a copy-on-write mapping"));
                    }
                    done += written as usize;
                }
                copied += done as u64;
            }
        }
    }
    for mapping in &entry.mappings {
        // SAFETY: replaces this process's private mapping with the same range
        // of the new memfd, which now holds the same bytes.
        let mapped = unsafe {
            libc::mmap(
                mapping.start as *mut libc::c_void,
                mapping.end - mapping.start,
                mapping.prot,
                libc::MAP_SHARED | libc::MAP_FIXED,
                new,
                mapping.offset as libc::off_t,
            )
        };
        if mapped as usize != mapping.start {
            return Err(fail("re-mapping a copy-on-write mapping"));
        }
    }
    for entry in std::fs::read_dir("/proc/self/fd")?.flatten() {
        let Ok(other) = entry.file_name().to_string_lossy().parse::<RawFd>() else {
            continue;
        };
        if other == new || stat_of(other).map(|s| (s.st_dev, s.st_ino)).ok() != Some(key) {
            continue;
        }
        // SAFETY: fcntl on a live descriptor of this process.
        let flags = unsafe { libc::fcntl(other, libc::F_GETFD) };
        let cloexec = if flags >= 0 && flags & libc::FD_CLOEXEC != 0 {
            libc::O_CLOEXEC
        } else {
            0
        };
        // SAFETY: both are descriptors of this process; dup3 replaces `other`.
        if unsafe { libc::dup3(new, other, cloexec) } < 0 {
            return Err(fail("replacing a descriptor"));
        }
    }
    // SAFETY: the duplicates and mappings keep the new memfd alive.
    unsafe { libc::close(new) };
    registry.remove(&key);
    MATERIALIZED.fetch_add(1, Ordering::Relaxed);
    MATERIALIZED_BYTES.fetch_add(copied, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: usize = 4096;

    /// A copy-on-write mapping's bytes, written after it was made private,
    /// reach the descriptor only once the memfd is materialized; the shared
    /// file keeps its own bytes.
    #[test]
    fn materializing_gives_the_descriptor_this_process_bytes() {
        let shared = new_memfd("private-memfd-test").unwrap();
        // SAFETY: the new memfd.
        assert_eq!(
            unsafe { libc::ftruncate(shared, (4 * PAGE) as libc::off_t) },
            0
        );
        // Page 0 holds data in the shared file; pages 1-3 are holes.
        // SAFETY: writes one byte from a live buffer.
        assert_eq!(
            unsafe { libc::pwrite(shared, b"t".as_ptr().cast(), 1, 0) },
            1
        );
        // SAFETY: a private mapping of the memfd.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4 * PAGE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE,
                shared,
                0,
            )
        } as usize;
        assert_ne!(base as *mut libc::c_void, libc::MAP_FAILED);
        // Another descriptor that must see the private bytes after.
        // SAFETY: duplicates a live descriptor.
        let keep = unsafe { libc::fcntl(shared, libc::F_DUPFD_CLOEXEC, 0) };
        // The template's view of the shared file: a shared mapping that
        // materializing leaves alone.
        // SAFETY: a shared read-only mapping of the memfd's first page.
        let template = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE,
                libc::PROT_READ,
                libc::MAP_SHARED,
                shared,
                0,
            )
        } as usize;
        assert_ne!(template as *mut libc::c_void, libc::MAP_FAILED);
        // SAFETY: page-aligned writes through the private mapping.
        unsafe {
            *(base as *mut u8) = b'c';
            *((base + 2 * PAGE + 5) as *mut u8) = b'x';
        }
        let stat = stat_of(shared).unwrap();
        register_private_memfd(
            stat.st_dev,
            stat.st_ino,
            "private-memfd-test",
            vec![PrivateMapping {
                start: base,
                end: base + 4 * PAGE,
                offset: 0,
                prot: libc::PROT_READ | libc::PROT_WRITE,
            }],
        );
        assert!(is_private_memfd(keep));
        let mut byte = [0u8; 1];
        materialize_private_memfd(keep).unwrap();
        assert!(!is_private_memfd(keep));
        // The shared file keeps the template's bytes.
        // SAFETY: reads the live template mapping.
        assert_eq!(unsafe { *(template as *const u8) }, b't');
        for fd in [shared, keep] {
            // SAFETY: reads one byte into a live buffer.
            assert_eq!(
                unsafe { libc::pread(fd, byte.as_mut_ptr().cast(), 1, 0) },
                1
            );
            assert_eq!(&byte, b"c");
            // SAFETY: as above.
            assert_eq!(
                unsafe {
                    libc::pread(
                        fd,
                        byte.as_mut_ptr().cast(),
                        1,
                        (2 * PAGE + 5) as libc::off_t,
                    )
                },
                1
            );
            assert_eq!(&byte, b"x");
        }
        // The mapping is now shared with the new memfd: a store through it
        // reaches the descriptor at once.
        // SAFETY: a write through the live mapping.
        unsafe { *((base + PAGE) as *mut u8) = b'n' };
        // SAFETY: reads one byte into a live buffer.
        assert_eq!(
            unsafe { libc::pread(keep, byte.as_mut_ptr().cast(), 1, PAGE as libc::off_t) },
            1
        );
        assert_eq!(&byte, b"n");
        // SAFETY: unmaps and closes what this test made.
        unsafe {
            libc::munmap(base as *mut libc::c_void, 4 * PAGE);
            libc::munmap(template as *mut libc::c_void, PAGE);
            libc::close(shared);
            libc::close(keep);
        }
    }

    /// The layout a copy gets: a private view of the file's data, private
    /// anonymous memory over its holes; a discard replaces a range with
    /// anonymous memory. Materializing copies what each piece holds.
    #[test]
    fn materializing_reads_anonymous_pieces_and_honours_discards() {
        let page = |n: usize| n * PAGE;
        let shared = new_memfd("private-memfd-layout-test").unwrap();
        // SAFETY: the new memfd.
        assert_eq!(
            unsafe { libc::ftruncate(shared, page(4) as libc::off_t) },
            0
        );
        // SAFETY: writes from live buffers.
        unsafe {
            assert_eq!(libc::pwrite(shared, b"t".as_ptr().cast(), 1, 0), 1);
            assert_eq!(
                libc::pwrite(shared, b"u".as_ptr().cast(), 1, page(3) as libc::off_t),
                1
            );
        }
        let rw = libc::PROT_READ | libc::PROT_WRITE;
        // SAFETY: a private view of the memfd, then anonymous memory over its
        // hole at pages 1 and 2, as a copy's repair lays them out.
        let base = unsafe {
            let base = libc::mmap(
                std::ptr::null_mut(),
                page(4),
                rw,
                libc::MAP_PRIVATE,
                shared,
                0,
            );
            assert_ne!(base, libc::MAP_FAILED);
            let hole = libc::mmap(
                base.cast::<u8>().add(page(1)).cast(),
                page(2),
                rw,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
                -1,
                0,
            );
            assert_eq!(hole, base.cast::<u8>().add(page(1)).cast());
            base as usize
        };
        // SAFETY: a shared read-only view of the file, the template's.
        let template = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                page(4),
                libc::PROT_READ,
                libc::MAP_SHARED,
                shared,
                0,
            )
        } as usize;
        assert_ne!(template as *mut libc::c_void, libc::MAP_FAILED);
        // SAFETY: stores through the copy's mapping.
        unsafe {
            *((base + page(2) + 7) as *mut u8) = b'a';
            *((base + page(3)) as *mut u8) = b'w';
        }
        let stat = stat_of(shared).unwrap();
        register_private_memfd(
            stat.st_dev,
            stat.st_ino,
            "private-memfd-layout-test",
            vec![PrivateMapping {
                start: base,
                end: base + page(4),
                offset: 0,
                prot: rw,
            }],
        );
        // Page 0 is discarded: zeros from now on, though the file holds 't'.
        assert!(discard_private(base, page(1)).unwrap());
        // SAFETY: reads the copy's mapping.
        assert_eq!(unsafe { *(base as *const u8) }, 0);
        materialize_private_memfd(shared).unwrap();
        let read = |offset: usize| {
            let mut byte = [9u8; 1];
            // SAFETY: reads one byte into a live buffer.
            assert_eq!(
                unsafe { libc::pread(shared, byte.as_mut_ptr().cast(), 1, offset as libc::off_t) },
                1
            );
            byte[0]
        };
        assert_eq!(read(0), 0);
        assert_eq!(read(page(1)), 0);
        assert_eq!(read(page(2) + 7), b'a');
        assert_eq!(read(page(3)), b'w');
        // The template's file is untouched.
        // SAFETY: reads the live template view.
        unsafe {
            assert_eq!(*(template as *const u8), b't');
            assert_eq!(*((template + page(3)) as *const u8), b'u');
            assert_eq!(*((template + page(2) + 7) as *const u8), 0);
        }
        // SAFETY: unmaps and closes what this test made.
        unsafe {
            libc::munmap(base as *mut libc::c_void, page(4));
            libc::munmap(template as *mut libc::c_void, page(4));
            libc::close(shared);
        }
    }
}
