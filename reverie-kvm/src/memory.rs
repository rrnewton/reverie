/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::collections::BTreeMap;
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
    guest_base: u64,
    size: usize,
    host_access: Mutex<()>,
    page_states: Mutex<BTreeMap<u64, PageState>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PageState {
    NoAccess,
    Unmapped,
}

// SAFETY: Mapping owns an mmap allocation, not a Rust reference. Host access
// is serialized by host_access, and the KVM backend exposes handles only while
// its single vCPU is stopped at an exit.
unsafe impl Send for Mapping {}
// SAFETY: See the Send implementation. All host reads and writes take the
// mapping's mutex before dereferencing the pointer.
unsafe impl Sync for Mapping {}

impl GuestMemory {
    /// Allocates a shared anonymous mapping for a guest-physical address range.
    pub fn new(guest_base: u64, size: usize) -> Result<Self> {
        let size_u64 = u64::try_from(size).expect("usize must fit in u64 on x86-64");
        if size == 0
            || !size.is_multiple_of(PAGE_SIZE)
            || !guest_base.is_multiple_of(PAGE_SIZE as u64)
            || guest_base.checked_add(size_u64).is_none()
        {
            return Err(Error::InvalidMemoryLayout { guest_base, size });
        }

        // SAFETY: mmap is called with an anonymous fd and validated below. The
        // mapping is owned by this value and released exactly once in Drop.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANONYMOUS | libc::MAP_SHARED | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(Error::MemoryMapping(std::io::Error::last_os_error()));
        }

        Ok(Self {
            mapping: Arc::new(Mapping {
                mapping: NonNull::new(mapping.cast()).expect("mmap returned a null mapping"),
                guest_base,
                size,
                host_access: Mutex::new(()),
                page_states: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    pub(crate) fn snapshot(&self) -> Result<Self> {
        const COPY_CHUNK: usize = 1024 * 1024;

        let snapshot = Self::new(self.guest_base(), self.len())?;
        let page_states = self
            .mapping
            .page_states
            .lock()
            .expect("guest memory access map lock poisoned");
        let mut buffer = vec![0; COPY_CHUNK.min(self.len())];
        let mut offset = 0;
        while offset < self.len() {
            let length = buffer.len().min(self.len() - offset);
            let address = self.guest_base() + offset as u64;
            self.read_raw(address, &mut buffer[..length])?;
            snapshot.write_raw(address, &buffer[..length])?;
            offset += length;
        }
        *snapshot
            .mapping
            .page_states
            .lock()
            .expect("guest memory access map lock poisoned") = page_states.clone();
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

    pub(crate) fn set_mapped_access(
        &self,
        guest_address: u64,
        length: u64,
        no_access: bool,
    ) -> Result<()> {
        let Some((first_page, last_page)) = self.checked_page_range(guest_address, length)? else {
            return Ok(());
        };
        let mut pages = self
            .mapping
            .page_states
            .lock()
            .expect("guest memory access map lock poisoned");
        for page in first_page..=last_page {
            if no_access {
                pages.insert(page, PageState::NoAccess);
            } else {
                pages.remove(&page);
            }
        }
        Ok(())
    }

    pub(crate) fn set_unmapped(&self, guest_address: u64, length: u64) -> Result<()> {
        let Some((first_page, last_page)) = self.checked_page_range(guest_address, length)? else {
            return Ok(());
        };
        let mut pages = self
            .mapping
            .page_states
            .lock()
            .expect("guest memory access map lock poisoned");
        for page in first_page..=last_page {
            pages.insert(page, PageState::Unmapped);
        }
        Ok(())
    }

    pub(crate) fn range_is_mapped(&self, guest_address: u64, length: u64) -> bool {
        let Ok(Some((first_page, last_page))) = self.checked_page_range(guest_address, length)
        else {
            return false;
        };
        let pages = self
            .mapping
            .page_states
            .lock()
            .expect("guest memory access map lock poisoned");
        (first_page..=last_page).all(|page| pages.get(&page) != Some(&PageState::Unmapped))
    }

    pub(crate) fn remap_access(
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
        let mut pages = self
            .mapping
            .page_states
            .lock()
            .expect("guest memory access map lock poisoned");
        let old_states = (old_first..=old_last)
            .map(|page| pages.get(&page).copied())
            .collect::<Vec<_>>();
        let extension_state = old_states.last().copied().flatten();

        for page in old_first..=old_last {
            pages.insert(page, PageState::Unmapped);
        }
        for page in new_first..=new_last {
            pages.remove(&page);
        }
        for (index, page) in (new_first..=new_last).enumerate() {
            let state = old_states.get(index).copied().unwrap_or(extension_state);
            if let Some(state) = state {
                pages.insert(page, state);
            }
        }
        Ok(())
    }

    pub(crate) fn reset_user_access(&self) -> Result<()> {
        self.mapping
            .page_states
            .lock()
            .expect("guest memory access map lock poisoned")
            .clear();
        self.set_unmapped(self.guest_base(), PAGE_SIZE as u64)
    }

    pub(crate) fn clear_user_access(&self) {
        self.mapping
            .page_states
            .lock()
            .expect("guest memory access map lock poisoned")
            .clear();
    }

    /// Copies bytes from guest memory into a host buffer.
    pub fn read(&self, guest_address: u64, destination: &mut [u8]) -> Result<()> {
        self.checked_offset(guest_address, destination.len())?;
        let pages = self
            .mapping
            .page_states
            .lock()
            .expect("guest memory access map lock poisoned");
        if self.accessible_prefix(&pages, guest_address, destination.len())? != destination.len() {
            return Err(Error::GuestMemoryAccessDenied {
                address: guest_address,
                length: destination.len(),
            });
        }
        self.read_raw(guest_address, destination)
    }

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
    pub fn write(&mut self, guest_address: u64, source: &[u8]) -> Result<()> {
        self.checked_offset(guest_address, source.len())?;
        let pages = self
            .mapping
            .page_states
            .lock()
            .expect("guest memory access map lock poisoned");
        if self.accessible_prefix(&pages, guest_address, source.len())? != source.len() {
            return Err(Error::GuestMemoryAccessDenied {
                address: guest_address,
                length: source.len(),
            });
        }
        self.write_raw(guest_address, source)
    }

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
    pub fn zero(&mut self, guest_address: u64, length: usize) -> Result<()> {
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

    fn accessible_prefix(
        &self,
        pages: &BTreeMap<u64, PageState>,
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
        let mut cursor = guest_address;
        while cursor < end {
            if pages.contains_key(&(cursor / PAGE_SIZE as u64)) {
                break;
            }
            let next_page = (cursor / PAGE_SIZE as u64 + 1) * PAGE_SIZE as u64;
            cursor = next_page.min(end);
        }
        Ok(usize::try_from(cursor - guest_address).expect("guest memory prefix must fit usize"))
    }
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

// TODO-HUMAN-REVIEW(PR-130): Review protected partial user-copy semantics.
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
            let pages = self
                .mapping
                .page_states
                .lock()
                .expect("guest memory access map lock poisoned");
            let count = match self.accessible_prefix(&pages, address, requested) {
                Ok(0) | Err(_) => {
                    return if total == 0 {
                        Err(Errno::EFAULT)
                    } else {
                        Ok(total)
                    };
                }
                Ok(count) => count,
            };
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

            let requested = (read_from[source_index].len() - source_offset)
                .min(write_to[destination_index].len() - destination_offset);
            let address =
                write_to[destination_index].as_mut_ptr() as u64 + destination_offset as u64;
            let pages = self
                .mapping
                .page_states
                .lock()
                .expect("guest memory access map lock poisoned");
            let count = match self.accessible_prefix(&pages, address, requested) {
                Ok(0) | Err(_) => {
                    return if total == 0 {
                        Err(Errno::EFAULT)
                    } else {
                        Ok(total)
                    };
                }
                Ok(count) => count,
            };
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
        let mut parent = GuestMemory::new(0x1000, PAGE_SIZE * 2).unwrap();
        parent.write(0x1100, b"parent").unwrap();

        let mut child = parent.snapshot().unwrap();
        let mut bytes = [0; 6];
        child.read(0x1100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"parent");

        child.write(0x1100, b"child!").unwrap();
        parent.read(0x1100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"parent");
        child.read(0x1100, &mut bytes).unwrap();
        assert_eq!(&bytes, b"child!");
    }

    #[test]
    fn no_access_pages_fault_and_allow_partial_memory_access() {
        let mut memory = GuestMemory::new(0, PAGE_SIZE * 2).unwrap();
        memory
            .set_mapped_access(PAGE_SIZE as u64, PAGE_SIZE as u64, true)
            .unwrap();
        let bytes = [0x5a; 16];

        let start = AddrMut::from_raw(PAGE_SIZE - 8).unwrap();
        assert_eq!(MemoryAccess::write(&mut memory, start, &bytes), Ok(8));
        let denied = AddrMut::from_raw(PAGE_SIZE).unwrap();
        assert_eq!(
            MemoryAccess::write(&mut memory, denied, &bytes),
            Err(Errno::EFAULT)
        );
        assert!(matches!(
            memory.write(PAGE_SIZE as u64, &bytes),
            Err(Error::GuestMemoryAccessDenied { .. })
        ));
        assert!(matches!(
            memory.read(PAGE_SIZE as u64, &mut [0; 16]),
            Err(Error::GuestMemoryAccessDenied { .. })
        ));

        let mut snapshot = memory.snapshot().unwrap();
        assert!(matches!(
            snapshot.write(PAGE_SIZE as u64, &bytes),
            Err(Error::GuestMemoryAccessDenied { .. })
        ));

        memory
            .set_mapped_access(PAGE_SIZE as u64, PAGE_SIZE as u64, false)
            .unwrap();
        assert_eq!(MemoryAccess::write(&mut memory, denied, &bytes), Ok(16));
    }

    #[test]
    fn exec_access_reset_denies_null_and_clears_old_mappings() {
        let mut memory = GuestMemory::new(0, PAGE_SIZE * 2).unwrap();
        memory
            .set_unmapped(PAGE_SIZE as u64, PAGE_SIZE as u64)
            .unwrap();

        memory.reset_user_access().unwrap();

        assert!(matches!(
            memory.write(1, &[1]),
            Err(Error::GuestMemoryAccessDenied { .. })
        ));
        memory.write(PAGE_SIZE as u64, &[1]).unwrap();
    }
}
