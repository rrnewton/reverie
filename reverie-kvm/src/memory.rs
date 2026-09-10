/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::collections::BTreeMap;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::Mutex;

use reverie::syscalls::Errno;
use reverie::syscalls::MemoryAccess;

use crate::Error;
use crate::Result;

const PAGE_SIZE: usize = 4096;

/// A contiguous, page-aligned guest-physical memory region.
#[derive(Clone, Debug)]
pub struct GuestMemory {
    mapping: Arc<Mapping>,
}

#[derive(Debug)]
struct Mapping {
    mapping: NonNull<u8>,
    backing: OwnedFd,
    guest_base: u64,
    size: usize,
    host_access: Mutex<()>,
    user_access: Mutex<UserAccess>,
}

#[derive(Clone, Debug, Default)]
struct UserAccess {
    enabled: bool,
    pages: BTreeMap<u64, UserPageState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UserPageState {
    Accessible,
    NoAccess,
}

// SAFETY: Mapping owns an mmap allocation, not a Rust reference. Host access
// is serialized by host_access, and the KVM backend exposes handles only while
// its single vCPU is stopped at an exit.
unsafe impl Send for Mapping {}
// SAFETY: See the Send implementation. All host reads and writes take the
// mapping's mutex before dereferencing the pointer.
unsafe impl Sync for Mapping {}

impl GuestMemory {
    /// Allocates a shared, memfd-backed mapping for a guest-physical address range.
    pub fn new(guest_base: u64, size: usize) -> Result<Self> {
        let size_u64 = u64::try_from(size).expect("usize must fit in u64 on x86-64");
        if size == 0
            || !size.is_multiple_of(PAGE_SIZE)
            || !guest_base.is_multiple_of(PAGE_SIZE as u64)
            || guest_base.checked_add(size_u64).is_none()
            || libc::off_t::try_from(size).is_err()
        {
            return Err(Error::InvalidMemoryLayout { guest_base, size });
        }

        let backing = create_memory_backing().map_err(Error::MemoryMapping)?;
        // SAFETY: backing is a live, writable memfd and size fits off_t.
        if unsafe { libc::ftruncate(backing.as_raw_fd(), size as libc::off_t) } != 0 {
            return Err(Error::MemoryMapping(io::Error::last_os_error()));
        }

        // SAFETY: mmap is called with the live memfd and validated below. The
        // mapping is owned by this value and released exactly once in Drop.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_NORESERVE,
                backing.as_raw_fd(),
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(Error::MemoryMapping(std::io::Error::last_os_error()));
        }

        Ok(Self {
            mapping: Arc::new(Mapping {
                mapping: NonNull::new(mapping.cast()).expect("mmap returned a null mapping"),
                backing,
                guest_base,
                size,
                host_access: Mutex::new(()),
                user_access: Mutex::new(UserAccess::default()),
            }),
        })
    }

    pub(crate) fn snapshot(&self) -> Result<Self> {
        self.snapshot_with_sparse_copy(copy_sparse_file)
    }

    fn snapshot_with_sparse_copy(
        &self,
        sparse_copy: impl FnOnce(RawFd, RawFd, usize) -> io::Result<()>,
    ) -> Result<Self> {
        const COPY_CHUNK: usize = 1024 * 1024;

        let snapshot = Self::new(self.guest_base(), self.len())?;
        let user_access = self
            .mapping
            .user_access
            .lock()
            .expect("guest memory access map lock poisoned")
            .clone();

        let sparse_result = {
            let _source_guard = self
                .mapping
                .host_access
                .lock()
                .expect("guest memory lock poisoned");
            let _destination_guard = snapshot
                .mapping
                .host_access
                .lock()
                .expect("guest memory lock poisoned");
            sparse_copy(
                self.mapping.backing.as_raw_fd(),
                snapshot.mapping.backing.as_raw_fd(),
                self.len(),
            )
        };

        // SEEK_DATA/SEEK_HOLE and copy_file_range are Linux optimizations, not
        // correctness requirements. If either is unavailable or cannot finish
        // an extent, overwrite the entire destination using the previous copy
        // path. This also replaces any prefix copied before the failure.
        if sparse_result.is_err() {
            let mut buffer = vec![0; COPY_CHUNK.min(self.len())];
            let mut offset = 0;
            while offset < self.len() {
                let length = buffer.len().min(self.len() - offset);
                let address = self.guest_base() + offset as u64;
                self.read_raw(address, &mut buffer[..length])?;
                snapshot.write_raw(address, &buffer[..length])?;
                offset += length;
            }
        }
        *snapshot
            .mapping
            .user_access
            .lock()
            .expect("guest memory access map lock poisoned") = user_access;
        Ok(snapshot)
    }

    /// Returns the first guest-physical address in the mapping.
    pub fn guest_base(&self) -> u64 {
        self.mapping.guest_base
    }

    /// Returns the mapping size in bytes.
    pub fn len(&self) -> usize {
        self.mapping.size
    }

    /// Returns the address immediately after this guest-memory region.
    pub fn guest_end(&self) -> u64 {
        self.mapping.guest_base + self.mapping.size as u64
    }

    /// Returns whether the mapping is empty.
    pub fn is_empty(&self) -> bool {
        self.mapping.size == 0
    }

    // TODO-HUMAN-REVIEW(PR-132): Review the host-side KVM user mapping API.
    pub(crate) fn clear_user_access(&self) {
        let mut access = self
            .mapping
            .user_access
            .lock()
            .expect("guest memory access map lock poisoned");
        access.enabled = false;
        access.pages.clear();
    }

    // TODO-HUMAN-REVIEW(PR-132): Review the host-side KVM user mapping API.
    pub(crate) fn enable_user_access(&self) {
        self.mapping
            .user_access
            .lock()
            .expect("guest memory access map lock poisoned")
            .enabled = true;
    }

    // TODO-HUMAN-REVIEW(PR-132): Review the host-side KVM user mapping API.
    pub(crate) fn map_user_range(
        &self,
        guest_address: u64,
        length: u64,
        no_access: bool,
    ) -> Result<()> {
        let Some((first_page, last_page)) = self.checked_page_range(guest_address, length)? else {
            return Ok(());
        };
        let state = if no_access {
            UserPageState::NoAccess
        } else {
            UserPageState::Accessible
        };
        let mut access = self
            .mapping
            .user_access
            .lock()
            .expect("guest memory access map lock poisoned");
        for page in first_page..=last_page {
            access.pages.insert(page, state);
        }
        Ok(())
    }

    // TODO-HUMAN-REVIEW(PR-132): Review the host-side KVM user mapping API.
    pub(crate) fn unmap_user_range(&self, guest_address: u64, length: u64) -> Result<()> {
        let Some((first_page, last_page)) = self.checked_page_range(guest_address, length)? else {
            return Ok(());
        };
        let mut access = self
            .mapping
            .user_access
            .lock()
            .expect("guest memory access map lock poisoned");
        for page in first_page..=last_page {
            access.pages.remove(&page);
        }
        Ok(())
    }

    // TODO-HUMAN-REVIEW(PR-132): Review the host-side KVM user mapping API.
    pub(crate) fn user_range_is_mapped(&self, guest_address: u64, length: u64) -> bool {
        let Ok(Some((first_page, last_page))) = self.checked_page_range(guest_address, length)
        else {
            return false;
        };
        let access = self
            .mapping
            .user_access
            .lock()
            .expect("guest memory access map lock poisoned");
        (first_page..=last_page).all(|page| access.pages.contains_key(&page))
    }

    // AUTONOMOUS-BOT-IMPLEMENTED: Reuse deterministic holes in the KVM guest arena.
    // TODO-HUMAN-REVIEW(PR-176): Review mmap hole-selection semantics.
    pub(crate) fn find_unmapped_user_range(
        &self,
        start: u64,
        end: u64,
        length: u64,
    ) -> Option<u64> {
        let page_size = PAGE_SIZE as u64;
        if length == 0
            || !start.is_multiple_of(page_size)
            || !end.is_multiple_of(page_size)
            || !length.is_multiple_of(page_size)
            || start < self.guest_base()
            || end > self.guest_end()
            || start >= end
        {
            return None;
        }

        let pages_needed = length / page_size;
        let end_page = end / page_size;
        let mut candidate = start / page_size;
        if candidate.checked_add(pages_needed)? > end_page {
            return None;
        }

        let access = self
            .mapping
            .user_access
            .lock()
            .expect("guest memory access map lock poisoned");
        for (&occupied, _) in access.pages.range(candidate..end_page) {
            if candidate.checked_add(pages_needed)? <= occupied {
                return candidate.checked_mul(page_size);
            }
            candidate = occupied.checked_add(1)?;
            if candidate.checked_add(pages_needed)? > end_page {
                return None;
            }
        }
        candidate.checked_mul(page_size)
    }

    // TODO-HUMAN-REVIEW(PR-132): Review the host-side KVM user mapping API.
    pub(crate) fn remap_user_range(
        &self,
        old_address: u64,
        old_length: u64,
        new_address: u64,
        new_length: u64,
    ) -> Result<()> {
        let Some((old_first, old_last)) = self.checked_page_range(old_address, old_length)? else {
            return Ok(());
        };
        let Some((new_first, new_last)) = self.checked_page_range(new_address, new_length)? else {
            return Ok(());
        };
        let mut access = self
            .mapping
            .user_access
            .lock()
            .expect("guest memory access map lock poisoned");
        let old_states = (old_first..=old_last)
            .map(|page| access.pages.get(&page).copied())
            .collect::<Vec<_>>();
        if old_states.iter().any(Option::is_none) {
            return Err(Error::GuestMemoryAccessDenied {
                address: old_address,
                length: usize::try_from(old_length).unwrap_or(usize::MAX),
            });
        }
        let extension_state = old_states
            .last()
            .copied()
            .flatten()
            .expect("nonempty mapped range has a last page");
        for page in old_first..=old_last {
            access.pages.remove(&page);
        }
        for (index, page) in (new_first..=new_last).enumerate() {
            let state = old_states
                .get(index)
                .copied()
                .flatten()
                .unwrap_or(extension_state);
            access.pages.insert(page, state);
        }
        Ok(())
    }

    /// Copies bytes from guest memory into a host buffer.
    // TODO-HUMAN-REVIEW(PR-132): Review user-map enforcement on this public API.
    pub fn read(&self, guest_address: u64, destination: &mut [u8]) -> Result<()> {
        self.checked_offset(guest_address, destination.len())?;
        if self.user_accessible_prefix(guest_address, destination.len())? != destination.len() {
            return Err(Error::GuestMemoryAccessDenied {
                address: guest_address,
                length: destination.len(),
            });
        }
        self.read_raw(guest_address, destination)
    }

    // TODO-HUMAN-REVIEW(PR-132): Review internal copies that bypass the user map.
    pub(crate) fn read_raw(&self, guest_address: u64, destination: &mut [u8]) -> Result<()> {
        let offset = self.checked_offset(guest_address, destination.len())?;
        let _guard = self
            .mapping
            .host_access
            .lock()
            .expect("guest memory lock poisoned");
        // SAFETY: checked_offset proves that both ends of the copy lie within
        // the live mapping, and destination is a distinct mutable slice.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.mapping.mapping.as_ptr().add(offset),
                destination.as_mut_ptr(),
                destination.len(),
            );
        }
        Ok(())
    }

    /// Copies bytes from a host slice into guest memory.
    // TODO-HUMAN-REVIEW(PR-132): Review user-map enforcement on this public API.
    pub fn write(&mut self, guest_address: u64, source: &[u8]) -> Result<()> {
        self.checked_offset(guest_address, source.len())?;
        if self.user_accessible_prefix(guest_address, source.len())? != source.len() {
            return Err(Error::GuestMemoryAccessDenied {
                address: guest_address,
                length: source.len(),
            });
        }
        self.write_raw(guest_address, source)
    }

    // TODO-HUMAN-REVIEW(PR-132): Review internal copies that bypass the user map.
    pub(crate) fn write_raw(&self, guest_address: u64, source: &[u8]) -> Result<()> {
        let offset = self.checked_offset(guest_address, source.len())?;
        let _guard = self
            .mapping
            .host_access
            .lock()
            .expect("guest memory lock poisoned");
        // SAFETY: checked_offset proves that both ends of the copy lie within
        // the live mapping, and host writes are serialized by host_access.
        unsafe {
            std::ptr::copy_nonoverlapping(
                source.as_ptr(),
                self.mapping.mapping.as_ptr().add(offset),
                source.len(),
            );
        }
        Ok(())
    }
    /// Zeros a guest-physical address range.
    // TODO-HUMAN-REVIEW(PR-132): Review user-map enforcement on this public API.
    pub fn zero(&mut self, guest_address: u64, length: usize) -> Result<()> {
        self.checked_offset(guest_address, length)?;
        if self.user_accessible_prefix(guest_address, length)? != length {
            return Err(Error::GuestMemoryAccessDenied {
                address: guest_address,
                length,
            });
        }
        self.zero_raw(guest_address, length)
    }

    // TODO-HUMAN-REVIEW(PR-132): Review internal copies that bypass the user map.
    pub(crate) fn zero_raw(&self, guest_address: u64, length: usize) -> Result<()> {
        let offset = self.checked_offset(guest_address, length)?;
        let _guard = self
            .mapping
            .host_access
            .lock()
            .expect("guest memory lock poisoned");
        // SAFETY: checked_offset proves that the full range lies within the
        // live mapping, and host writes are serialized by host_access.
        unsafe {
            std::ptr::write_bytes(self.mapping.mapping.as_ptr().add(offset), 0, length);
        }
        Ok(())
    }

    pub(crate) fn host_address(&self) -> u64 {
        self.mapping.mapping.as_ptr() as u64
    }

    fn checked_offset(&self, guest_address: u64, length: usize) -> Result<usize> {
        let relative = guest_address.checked_sub(self.mapping.guest_base);
        let length_u64 = u64::try_from(length).expect("usize must fit in u64 on x86-64");
        let end = relative.and_then(|offset| offset.checked_add(length_u64));
        if end.is_none_or(|end| end > self.mapping.size as u64) {
            return Err(Error::InvalidGuestAddress {
                address: guest_address,
                length,
                guest_base: self.mapping.guest_base,
                guest_end: self.mapping.guest_base + self.mapping.size as u64,
            });
        }
        Ok(relative.unwrap() as usize)
    }

    fn checked_page_range(&self, guest_address: u64, length: u64) -> Result<Option<(u64, u64)>> {
        if length == 0 {
            return Ok(None);
        }
        let length = usize::try_from(length).map_err(|_| Error::InvalidGuestAddress {
            address: guest_address,
            length: usize::MAX,
            guest_base: self.guest_base(),
            guest_end: self.guest_end(),
        })?;
        self.checked_offset(guest_address, length)?;
        let first_page = guest_address / PAGE_SIZE as u64;
        let last_page = (guest_address + length as u64 - 1) / PAGE_SIZE as u64;
        Ok(Some((first_page, last_page)))
    }

    // TODO-HUMAN-REVIEW(PR-132): Review partial user-range validation.
    pub(crate) fn user_accessible_prefix(
        &self,
        guest_address: u64,
        length: usize,
    ) -> Result<usize> {
        if length == 0 {
            return Ok(0);
        }
        if guest_address < self.guest_base() || guest_address >= self.guest_end() {
            return Err(Error::InvalidGuestAddress {
                address: guest_address,
                length,
                guest_base: self.guest_base(),
                guest_end: self.guest_end(),
            });
        }
        let requested_end = guest_address.saturating_add(length as u64);
        let end = requested_end.min(self.guest_end());
        let access = self
            .mapping
            .user_access
            .lock()
            .expect("guest memory access map lock poisoned");
        if !access.enabled {
            return Ok(
                usize::try_from(end - guest_address).expect("guest memory prefix must fit usize")
            );
        }

        let mut cursor = guest_address;
        while cursor < end {
            if access.pages.get(&(cursor / PAGE_SIZE as u64)) != Some(&UserPageState::Accessible) {
                break;
            }
            let next_page = (cursor / PAGE_SIZE as u64 + 1) * PAGE_SIZE as u64;
            cursor = next_page.min(end);
        }
        Ok(usize::try_from(cursor - guest_address).expect("guest memory prefix must fit usize"))
    }
}

fn create_memory_backing() -> io::Result<OwnedFd> {
    let name = c"reverie-kvm-guest-memory";
    // Guest RAM is never executable in the host mapping. Prefer the flag that
    // also works when the host requires non-executable memfds, but retain
    // compatibility with kernels predating MFD_NOEXEC_SEAL.
    // SAFETY: name is a live, NUL-terminated C string.
    let mut fd =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_NOEXEC_SEAL) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINVAL) {
            return Err(error);
        }
        // SAFETY: name is a live, NUL-terminated C string. Old kernels reject
        // MFD_NOEXEC_SEAL with EINVAL but accept the original flag set.
        fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: memfd_create returned a new descriptor owned by this call.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Copies every data extent from one equal-sized sparse file to another.
///
/// The destination must initially be entirely zero-filled; source holes are
/// neither written nor cleared in the destination.
///
/// A filesystem may conservatively report holes as data, which only makes
/// this slower. It must not report stored data as a hole. The caller falls
/// back to a byte-for-byte mapping copy on every error or incomplete extent.
fn copy_sparse_file(source: RawFd, destination: RawFd, length: usize) -> io::Result<()> {
    let end = libc::off_t::try_from(length)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mapping exceeds off_t"))?;
    let mut cursor: libc::off_t = 0;

    while cursor < end {
        // SAFETY: source is a live descriptor owned by the source Mapping.
        let data = unsafe { libc::lseek(source, cursor, libc::SEEK_DATA) };
        if data < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENXIO) {
                return Ok(());
            }
            return Err(error);
        }
        if data < cursor || data >= end {
            return if data >= end {
                Ok(())
            } else {
                Err(io::Error::other("SEEK_DATA moved backwards"))
            };
        }

        // SAFETY: source is a live descriptor owned by the source Mapping.
        let hole = unsafe { libc::lseek(source, data, libc::SEEK_HOLE) };
        if hole <= data {
            return Err(if hole < 0 {
                io::Error::last_os_error()
            } else {
                io::Error::other("SEEK_HOLE returned an empty extent")
            });
        }
        let extent_end = hole.min(end);
        let mut source_offset = data;
        let mut destination_offset = data;
        while source_offset < extent_end {
            let remaining = usize::try_from(extent_end - source_offset)
                .expect("nonnegative extent length must fit usize");
            // SAFETY: both descriptors are live for the call, the offsets are
            // within their equal file sizes, and both offset pointers are valid.
            let copied = unsafe {
                libc::copy_file_range(
                    source,
                    &mut source_offset,
                    destination,
                    &mut destination_offset,
                    remaining,
                    0,
                )
            };
            if copied < 0 {
                return Err(io::Error::last_os_error());
            }
            if copied == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "copy_file_range stopped before the end of an extent",
                ));
            }
            if source_offset != destination_offset || source_offset > extent_end {
                return Err(io::Error::other(
                    "copy_file_range returned inconsistent offsets",
                ));
            }
        }
        cursor = extent_end;
    }
    Ok(())
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: mapping and size are the exact values returned by mmap and
        // this Drop is the unique owner of that mapping.
        unsafe {
            libc::munmap(self.mapping.as_ptr().cast(), self.size);
        }
    }
}

// TODO-HUMAN-REVIEW(PR-132): Review KVM partial user-copy semantics.
impl MemoryAccess for GuestMemory {
    fn read_vectored(
        &self,
        read_from: &[std::io::IoSlice],
        write_to: &mut [std::io::IoSliceMut],
    ) -> std::result::Result<usize, Errno> {
        let mut source_index = 0;
        let mut source_offset = 0;
        let mut destination_index = 0;
        let mut destination_offset = 0;
        let mut total = 0;

        while source_index < read_from.len() && destination_index < write_to.len() {
            if source_offset == read_from[source_index].len() {
                source_index += 1;
                source_offset = 0;
                continue;
            }
            if destination_offset == write_to[destination_index].len() {
                destination_index += 1;
                destination_offset = 0;
                continue;
            }

            let requested = (read_from[source_index].len() - source_offset)
                .min(write_to[destination_index].len() - destination_offset);
            let address = read_from[source_index].as_ptr() as u64 + source_offset as u64;
            let count = self
                .user_accessible_prefix(address, requested)
                .unwrap_or_default();
            if count == 0 {
                return if total == 0 {
                    Err(Errno::EFAULT)
                } else {
                    Ok(total)
                };
            }
            let destination =
                &mut write_to[destination_index][destination_offset..destination_offset + count];
            if self.read_raw(address, destination).is_err() {
                return if total == 0 {
                    Err(Errno::EFAULT)
                } else {
                    Ok(total)
                };
            }
            source_offset += count;
            destination_offset += count;
            total += count;
            if count < requested {
                return Ok(total);
            }
        }
        Ok(total)
    }

    fn write_vectored(
        &mut self,
        read_from: &[std::io::IoSlice],
        write_to: &mut [std::io::IoSliceMut],
    ) -> std::result::Result<usize, Errno> {
        let mut source_index = 0;
        let mut source_offset = 0;
        let mut destination_index = 0;
        let mut destination_offset = 0;
        let mut total = 0;

        while source_index < read_from.len() && destination_index < write_to.len() {
            if source_offset == read_from[source_index].len() {
                source_index += 1;
                source_offset = 0;
                continue;
            }
            if destination_offset == write_to[destination_index].len() {
                destination_index += 1;
                destination_offset = 0;
                continue;
            }

            let count = (read_from[source_index].len() - source_offset)
                .min(write_to[destination_index].len() - destination_offset);
            let address =
                write_to[destination_index].as_mut_ptr() as u64 + destination_offset as u64;
            let requested = count;
            let count = self
                .user_accessible_prefix(address, requested)
                .unwrap_or_default();
            if count == 0 {
                return if total == 0 {
                    Err(Errno::EFAULT)
                } else {
                    Ok(total)
                };
            }
            let source = &read_from[source_index][source_offset..source_offset + count];
            if self.write_raw(address, source).is_err() {
                return if total == 0 {
                    Err(Errno::EFAULT)
                } else {
                    Ok(total)
                };
            }
            source_offset += count;
            destination_offset += count;
            total += count;
            if count < requested {
                return Ok(total);
            }
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use reverie::syscalls::AddrMut;

    use super::*;

    #[test]
    fn reads_and_writes_guest_memory() {
        let mut memory = GuestMemory::new(0x1000, PAGE_SIZE).unwrap();
        memory.write(0x1123, b"hello").unwrap();

        let mut bytes = [0; 5];
        memory.read(0x1123, &mut bytes).unwrap();
        assert_eq!(&bytes, b"hello");
    }

    #[test]
    fn permits_access_to_last_byte() {
        let mut memory = GuestMemory::new(0x2000, PAGE_SIZE).unwrap();
        memory.write(0x2fff, &[0x5a]).unwrap();

        let mut byte = [0];
        memory.read(0x2fff, &mut byte).unwrap();
        assert_eq!(byte, [0x5a]);
    }

    #[test]
    fn rejects_address_below_mapping() {
        let memory = GuestMemory::new(0x2000, PAGE_SIZE).unwrap();
        let error = memory.read(0x1fff, &mut [0]).unwrap_err();
        assert!(matches!(error, Error::InvalidGuestAddress { .. }));
    }

    #[test]
    fn rejects_access_past_mapping() {
        let mut memory = GuestMemory::new(0x2000, PAGE_SIZE).unwrap();
        let error = memory.write(0x2fff, &[1, 2]).unwrap_err();
        assert!(matches!(error, Error::InvalidGuestAddress { .. }));
    }

    #[test]
    fn cloned_handles_share_memory() {
        let mut first = GuestMemory::new(0x1000, PAGE_SIZE).unwrap();
        let mut second = first.clone();

        first.write(0x1100, b"shared").unwrap();
        let mut bytes = [0; 6];
        second.read(0x1100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"shared");

        second.write(0x1200, b"api").unwrap();
        let mut bytes = [0; 3];
        first.read(0x1200, &mut bytes).unwrap();
        assert_eq!(&bytes, b"api");
    }

    #[test]
    fn snapshot_copies_without_sharing_memory() {
        let mut parent = GuestMemory::new(0x1000, PAGE_SIZE * 3).unwrap();
        parent.write(0x1100, b"parent").unwrap();
        parent.write(0x3100, b"tail!!").unwrap();

        let mut child = parent.snapshot().unwrap();
        let mut bytes = [0; 6];
        child.read(0x1100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"parent");
        child.read(0x3100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"tail!!");

        child.write(0x1100, b"child!").unwrap();
        parent.write(0x3100, b"source").unwrap();
        parent.read(0x1100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"parent");
        child.read(0x1100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"child!");
        parent.read(0x3100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"source");
        child.read(0x3100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"tail!!");
    }

    #[test]
    fn sparse_snapshot_copies_distant_extents_and_page_boundaries() {
        const MAPPING_PAGES: usize = 16 * 1024;
        let mut parent = GuestMemory::new(0, PAGE_SIZE * MAPPING_PAGES).unwrap();
        let boundary = PAGE_SIZE as u64 - 2;
        let middle = (PAGE_SIZE * (MAPPING_PAGES / 2)) as u64 + 37;
        let tail = (PAGE_SIZE * MAPPING_PAGES - 4) as u64;

        parent.write(boundary, b"edge").unwrap();
        parent.write(middle, b"middle").unwrap();
        parent.write(tail, b"last").unwrap();

        let snapshot = parent.snapshot().unwrap();
        let mut bytes = [0; 6];
        snapshot.read(boundary, &mut bytes[..4]).unwrap();
        assert_eq!(&bytes[..4], b"edge");
        snapshot.read(middle, &mut bytes).unwrap();
        assert_eq!(&bytes, b"middle");
        snapshot.read(tail, &mut bytes[..4]).unwrap();
        assert_eq!(&bytes[..4], b"last");
        snapshot
            .read((PAGE_SIZE * (MAPPING_PAGES / 4)) as u64, &mut bytes)
            .unwrap();
        assert_eq!(bytes, [0; 6]);

        let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: stat points to writable storage and the backing descriptor is live.
        assert_eq!(
            unsafe { libc::fstat(snapshot.mapping.backing.as_raw_fd(), stat.as_mut_ptr()) },
            0
        );
        // SAFETY: fstat succeeded and initialized the structure.
        let allocated_bytes = unsafe { stat.assume_init() }.st_blocks as u64 * 512;
        assert!(
            allocated_bytes < (snapshot.len() / 2) as u64,
            "snapshot unexpectedly became dense: {allocated_bytes} allocated bytes"
        );
    }

    #[test]
    fn sparse_snapshot_failure_falls_back_after_a_partial_copy() {
        let mut parent = GuestMemory::new(0, PAGE_SIZE * 4).unwrap();
        parent.write(0, &[0x11; PAGE_SIZE]).unwrap();
        parent
            .write((PAGE_SIZE * 3) as u64, &[0x44; PAGE_SIZE])
            .unwrap();

        let snapshot = parent
            .snapshot_with_sparse_copy(|source, destination, _| {
                let mut source_offset = (PAGE_SIZE * 3) as libc::loff_t;
                let mut destination_offset: libc::loff_t = 0;
                while destination_offset < PAGE_SIZE as libc::loff_t {
                    // SAFETY: snapshot_with_sparse_copy supplies two live,
                    // equal-sized backing descriptors and valid offset pointers.
                    let copied = unsafe {
                        libc::copy_file_range(
                            source,
                            &mut source_offset,
                            destination,
                            &mut destination_offset,
                            PAGE_SIZE - destination_offset as usize,
                            0,
                        )
                    };
                    assert!(copied > 0);
                }
                assert_eq!(source_offset, (PAGE_SIZE * 4) as libc::loff_t);
                assert_eq!(destination_offset, PAGE_SIZE as libc::loff_t);
                Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
            })
            .unwrap();

        let mut bytes = vec![0; PAGE_SIZE * 4];
        snapshot.read(0, &mut bytes).unwrap();
        assert_eq!(&bytes[..PAGE_SIZE], &[0x11; PAGE_SIZE]);
        assert_eq!(&bytes[PAGE_SIZE..PAGE_SIZE * 3], &[0; PAGE_SIZE * 2]);
        assert_eq!(&bytes[PAGE_SIZE * 3..], &[0x44; PAGE_SIZE]);
        let mut parent_bytes = vec![0; PAGE_SIZE * 4];
        parent.read(0, &mut parent_bytes).unwrap();
        assert_eq!(parent_bytes, bytes);
    }

    #[test]
    fn untouched_snapshot_is_zero_filled_and_independent() {
        let mut parent = GuestMemory::new(0, PAGE_SIZE * 4).unwrap();
        let mut snapshot = parent
            .snapshot_with_sparse_copy(|source, destination, length| {
                let data = unsafe { libc::lseek(source, 0, libc::SEEK_DATA) };
                let error = (data < 0).then(io::Error::last_os_error);
                let initial_enxio = error
                    .as_ref()
                    .is_some_and(|error| error.raw_os_error() == Some(libc::ENXIO));
                let result = copy_sparse_file(source, destination, length);
                eprintln!(
                    "untouched snapshot: SEEK_DATA(0)={data}, error={error:?}, initial_enxio={initial_enxio}, sparse_copy={result:?}"
                );
                if initial_enxio {
                    assert!(result.is_ok());
                }
                result
            })
            .unwrap();

        let mut parent_bytes = vec![0xff; PAGE_SIZE * 4];
        let mut snapshot_bytes = vec![0xff; PAGE_SIZE * 4];
        parent.read(0, &mut parent_bytes).unwrap();
        snapshot.read(0, &mut snapshot_bytes).unwrap();
        assert_eq!(parent_bytes, vec![0; PAGE_SIZE * 4]);
        assert_eq!(snapshot_bytes, vec![0; PAGE_SIZE * 4]);

        snapshot.write(0, &[0x22; PAGE_SIZE]).unwrap();
        parent
            .write((PAGE_SIZE * 3) as u64, &[0x44; PAGE_SIZE])
            .unwrap();
        parent.read(0, &mut parent_bytes).unwrap();
        snapshot.read(0, &mut snapshot_bytes).unwrap();
        let mut expected_parent = vec![0; PAGE_SIZE * 4];
        expected_parent[PAGE_SIZE * 3..].fill(0x44);
        let mut expected_snapshot = vec![0; PAGE_SIZE * 4];
        expected_snapshot[..PAGE_SIZE].fill(0x22);
        assert_eq!(parent_bytes, expected_parent);
        assert_eq!(snapshot_bytes, expected_snapshot);
    }

    #[test]
    fn sparse_snapshot_copies_file_data_after_dontneed() {
        let mut parent = GuestMemory::new(0, PAGE_SIZE * 4).unwrap();
        parent
            .write(PAGE_SIZE as u64, b"backed after dontneed")
            .unwrap();

        // MAP_SHARED writes belong to the memfd. Flushing followed by
        // MADV_DONTNEED gives the kernel permission to discard the resident
        // mapping pages; sparse copying must enumerate file data, not PTEs.
        // MADV_DONTNEED is only a hint, so this checks correctness after the
        // transition without claiming that the kernel actually evicted it.
        // SAFETY: the address and length identify a page-aligned live mapping.
        assert_eq!(
            unsafe {
                libc::msync(
                    parent.mapping.mapping.as_ptr().add(PAGE_SIZE).cast(),
                    PAGE_SIZE,
                    libc::MS_SYNC,
                )
            },
            0
        );
        // SAFETY: the address and length identify a page-aligned live mapping.
        assert_eq!(
            unsafe {
                libc::madvise(
                    parent.mapping.mapping.as_ptr().add(PAGE_SIZE).cast(),
                    PAGE_SIZE,
                    libc::MADV_DONTNEED,
                )
            },
            0
        );

        let snapshot = parent.snapshot().unwrap();
        let mut bytes = [0; 21];
        snapshot.read(PAGE_SIZE as u64, &mut bytes).unwrap();
        assert_eq!(&bytes, b"backed after dontneed");
    }

    #[test]
    fn tracked_user_access_faults_and_returns_partial_copies() {
        let mut memory = GuestMemory::new(0, PAGE_SIZE * 3).unwrap();
        memory
            .map_user_range(PAGE_SIZE as u64, PAGE_SIZE as u64, false)
            .unwrap();
        memory
            .map_user_range((PAGE_SIZE * 2) as u64, PAGE_SIZE as u64, true)
            .unwrap();
        memory.enable_user_access();

        assert!(matches!(
            memory.write(1, &[0x11]),
            Err(Error::GuestMemoryAccessDenied { .. })
        ));
        let address = AddrMut::from_raw(PAGE_SIZE * 2 - 8).unwrap();
        let written = MemoryAccess::write(&mut memory, address, &[0x5a; 16]).unwrap();
        assert_eq!(written, 8);

        let mut bytes = [0; 8];
        memory.read((PAGE_SIZE * 2 - 8) as u64, &mut bytes).unwrap();
        assert_eq!(bytes, [0x5a; 8]);
        assert!(matches!(
            memory.read((PAGE_SIZE * 2) as u64, &mut [0]),
            Err(Error::GuestMemoryAccessDenied { .. })
        ));
    }

    #[test]
    fn snapshot_preserves_user_access_map() {
        let mut parent = GuestMemory::new(0, PAGE_SIZE * 2).unwrap();
        parent
            .map_user_range(PAGE_SIZE as u64, PAGE_SIZE as u64, false)
            .unwrap();
        parent.enable_user_access();
        parent.write(PAGE_SIZE as u64, b"mapped").unwrap();

        let mut child = parent.snapshot().unwrap();
        assert!(matches!(
            child.write(1, &[1]),
            Err(Error::GuestMemoryAccessDenied { .. })
        ));
        child.write(PAGE_SIZE as u64, b"child!").unwrap();
        let mut bytes = [0; 6];
        parent.read(PAGE_SIZE as u64, &mut bytes).unwrap();
        assert_eq!(&bytes, b"mapped");
    }

    #[test]
    fn finds_first_unmapped_user_range() {
        let memory = GuestMemory::new(0x1000, PAGE_SIZE * 8).unwrap();
        memory.map_user_range(0x2000, 0x2000, false).unwrap();
        memory.map_user_range(0x5000, 0x1000, true).unwrap();

        assert_eq!(
            memory.find_unmapped_user_range(0x1000, 0x9000, 0x1000),
            Some(0x1000)
        );
        assert_eq!(
            memory.find_unmapped_user_range(0x2000, 0x9000, 0x2000),
            Some(0x6000)
        );
        assert_eq!(
            memory.find_unmapped_user_range(0x2000, 0x7000, 0x2000),
            None
        );
        assert_eq!(
            memory.find_unmapped_user_range(0x1001, 0x9000, 0x1000),
            None
        );
    }
}
