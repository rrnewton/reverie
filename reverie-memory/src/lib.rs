/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

mod addr;
mod local;

use core::mem;
use std::ffi::CString;
use std::io;

pub use addr::Addr;
pub use addr::AddrMut;
pub use addr::AddrSlice;
pub use addr::AddrSliceMut;
pub use local::LocalMemory;
use syscalls::Errno;

/// One numeric destination in another process's address space.
///
/// Unlike [`io::IoSliceMut`], this forms no Rust reference to the remote
/// address. The address is only a kernel operand for a backend that has
/// independently authenticated the stopped target task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteIoVec {
    address: usize,
    length: usize,
}

impl RemoteIoVec {
    pub fn new(address: AddrMut<u8>, length: usize) -> Result<Self, Errno> {
        address.as_raw().checked_add(length).ok_or(Errno::EFAULT)?;
        Ok(Self {
            address: address.as_raw(),
            length,
        })
    }

    pub fn address(self) -> usize {
        self.address
    }

    pub fn length(self) -> usize {
        self.length
    }
}

/// Evidence missing before a followed-task destination store. This is never a
/// guest syscall errno or an assertion about Linux syscall error precedence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeUserStoreRefusal {
    /// A shared register, range, procfs, mapping, or task check refused.
    Evidence(NativeUserReadRefusal),
    /// The complete ordinary private mapping lacks write permission.
    WriteDenied,
    /// Actual target PKRU denies access or writing for this mapping's key.
    ProtectionKey(u8),
}

/// Result of a single native store, preserving its effect separately from
/// subsequent custody validation. An attempted store must never be retried or
/// reclassified as pre-effect refusal, including after a failed postcheck.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeUserStoreOutcome {
    /// No native payload transfer was attempted.
    Refused(NativeUserStoreRefusal),
    /// Exactly one native transfer was attempted. Neither a positive count nor
    /// successful postcheck alone establishes the consuming syscall's result.
    Attempted {
        /// The actual kernel return, without retry or fallback.
        raw: Result<usize, Errno>,
        /// Validation after the actual transfer, while custody remains held.
        postcheck: Result<(), Errno>,
    },
}

/// A backend-issued, borrowed, single-use destination writer. It grants neither
/// syscall completion nor permission to consume a recorded network prefix.
/// Implementations retain physical custody of the whole followed cohort and
/// check the exact original scalar receive before and after the actual store.
pub trait FollowedStore {
    /// Revalidate the same unused original context and physical held interval.
    /// This performs no write and claims no store; callers may use it directly
    /// before committing a separately authorized no-store result.
    fn validate_context(&self) -> Result<(), NativeUserStoreRefusal> {
        Err(NativeUserStoreRefusal::Evidence(
            NativeUserReadRefusal::UnsupportedBackend,
        ))
    }

    /// Write at the original receive destination. The backend qualifies every
    /// operand and preserves the actual kernel result through failed postchecks.
    fn store(&mut self, bytes: &[u8]) -> NativeUserStoreOutcome;
}

/// A proven read-access denial for an admitted native source mapping.
/// This does not determine the consuming syscall's errno. Translating a denial
/// to guest `EFAULT` additionally requires caller proof of that syscall's error
/// precedence, including the absence of an earlier error, while retaining the
/// task/mapping authority required by the read capability. Without that proof,
/// the caller must refuse the operation rather than synthesize a guest errno.
/// For example, `sendto` may report `EPIPE`, `ENOTCONN` or another socket error
/// before accessing its source buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeUserReadFault {
    /// The covered ordinary VMA has none of READ, WRITE or EXEC access.
    NoAccessMapping,
    /// The actual stopped target has AD set for this mapping's protection key.
    ProtectionKey(u8),
}

/// Missing backend evidence or an unsupported native source-read operation.
/// None of these values is a guest syscall errno, including a native `EFAULT`:
/// remote GUP can fail for reasons that do not prove a target user-access fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeUserReadRefusal {
    /// This backend does not implement the exact capability.
    UnsupportedBackend,
    /// Architecture, page size, target mode or protection-key layout unsupported.
    UnsupportedPlatform,
    /// Empty, oversized, overflowing, tagged, or cross-base-page operand.
    UnsupportedRange,
    /// The expected TID does not match this actual backend target.
    WrongTask,
    /// Actual stopped-target register access failed.
    TargetState(Errno),
    /// PRSTATUS returned this byte count instead of the complete native layout.
    /// No register field was decoded from the unsupported shape.
    RegisterShape(usize),
    /// Procfs opening/reading/authentication failed.
    Procfs(Errno),
    /// The procfs instance does not prove the caller's PID-numbering view.
    ProcfsViewMismatch,
    /// Mapping metadata is missing, malformed, duplicated or incomplete.
    MappingMetadata,
    /// Complete metadata exceeded the fixed observation bound.
    MetadataTooLarge,
    /// No existing VMA covers the whole operand; stack growth is not emulated.
    MappingMissing,
    /// This mapping's access flags or shape do not have a qualified read path.
    UnsupportedMapping,
    /// Shared, file, special or discardable backing lacks source-data exclusion.
    /// This is unsupported evidence, never a guest read fault.
    UnsupportedBacking,
    /// The single native copy failed; its errno remains backend evidence.
    NativeTransfer(Errno),
    /// The single native copy did not fill the operand; no retry was made.
    ShortTransfer(usize),
}

/// Native source-read failure, separating proven access denials from refusals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeUserReadError {
    /// Authenticated source-access denial, without copying source bytes.
    /// This alone does not establish the consuming syscall's error precedence.
    Fault(NativeUserReadFault),
    /// Backend refusal; must stop the operation rather than invent guest errno.
    Refused(NativeUserReadRefusal),
}

impl std::fmt::Display for NativeUserReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for NativeUserReadError {}

#[cfg(test)]
mod native_user_read_default_tests {
    use super::*;

    #[test]
    fn native_user_read_default_refuses_without_ordinary_read_or_copy() {
        struct Unsupported;
        impl MemoryAccess for Unsupported {
            fn read_vectored(
                &self,
                _: &[io::IoSlice],
                _: &mut [io::IoSliceMut],
            ) -> Result<usize, Errno> {
                panic!("native source capability must not fall back to ordinary reads")
            }
            fn write_vectored(
                &mut self,
                _: &[io::IoSlice],
                _: &mut [io::IoSliceMut],
            ) -> Result<usize, Errno> {
                panic!("native source capability must not write guest memory")
            }
        }
        for length in [0, 1, 8, 512, 513] {
            let mut buffer = [0xa5; 515];
            assert_eq!(
                Unsupported.read_native_user_exact(1, usize::MAX, &mut buffer[1..1 + length]),
                Err(NativeUserReadError::Refused(
                    NativeUserReadRefusal::UnsupportedBackend
                ))
            );
            assert_eq!(buffer, [0xa5; 515]);
        }
    }
}

/// Trait for accessing potentially remote memory.
pub trait MemoryAccess {
    /// Reads bytes from the address space. Returns the number of bytes read.
    ///
    /// Note that there is no guarantee that all of the requested buffers will be
    /// filled.
    fn read_vectored(
        &self,
        read_from: &[io::IoSlice],
        write_to: &mut [io::IoSliceMut],
    ) -> Result<usize, Errno>;

    /// Writes bytes to the address space. Returns the number of bytes written.
    ///
    /// Note that there is no guarantee that all of the requested buffers will
    /// be written.
    fn write_vectored(
        &mut self,
        read_from: &[io::IoSlice],
        write_to: &mut [io::IoSliceMut],
    ) -> Result<usize, Errno>;

    /// Performs one synchronous native write to the exact expected task.
    ///
    /// An implementation must check its actual task against `expected_tid`
    /// before any effect, respect the target's VMA access permissions, and
    /// return the unmodified native byte count or errno. It must not retry a
    /// short transfer, collapse `EFAULT` to zero, or fall back to ptrace writes.
    /// All effects must finish before return: no detached work or retained
    /// buffer references are permitted. Remote protection keys are not proved
    /// by this interface; the caller separately owns task-generation, mapping,
    /// foreground and lifetime authority throughout the operation.
    ///
    /// Backends without this exact capability refuse without accessing memory.
    fn write_native_user_vectored(
        &mut self,
        _expected_tid: i32,
        _local: &[io::IoSlice],
        _remote: &[RemoteIoVec],
    ) -> Result<usize, Errno> {
        Err(Errno::EOPNOTSUPP)
    }

    /// Checks the actual stopped task's native write access to protection key 0.
    ///
    /// This checks task identity and the target's current protection-key state;
    /// it does not inspect a mapping or establish that a destination uses key 0.
    /// The caller must already own that mapping provenance and keep the same
    /// memory instance, stopped task, foreground ownership, and worker exclusion
    /// through the immediately following synchronous native write, without an
    /// await or guest continuation. The result is not transferable authority and
    /// must not be serialized or reconstructed for a later operation.
    ///
    /// Unknown target state and unsupported backends refuse without accessing
    /// guest memory. Nonzero keys require separate qualification.
    fn validate_native_user_key0_write_access(&self, _expected_tid: i32) -> Result<(), Errno> {
        Err(Errno::EOPNOTSUPP)
    }

    /// Synchronously reads one admitted native source operand from the exact
    /// stopped task, with actual mapping and target protection-key read checks.
    ///
    /// Scope is 1..512 bytes within one 4096-byte base page. `address` is a
    /// numeric remote kernel operand, never a Rust reference. Implementations
    /// must not use debugger/ptrace memory access, retry a short native copy,
    /// infer read denial from WD alone, or turn unsupported evidence into guest
    /// `EFAULT`. On every error `buf` must remain byte-for-byte unchanged.
    ///
    /// The caller must already hold the actual local pre-effect root, Normal
    /// grant/prefix admission (where required by the Tool), stopped task and
    /// generation, and exclusion of mapping, PKRU and source-data mutation.
    /// Retain them through this synchronous call without await/resume/exec or
    /// owner release. This method creates none of that authority; a stopped
    /// TID alone does not exclude other tasks sharing its memory. A metadata
    /// observation is not a global atomic mapping snapshot.
    ///
    /// The ptrace implementation admits only ordinary private anonymous VMAs
    /// with zero file offset/device/inode, private permissions, qualified flags
    /// and no observed lazy-free debt. Shared mappings and private file mappings
    /// (including COW pages) refuse. Those observations are necessary backing
    /// restrictions, not proof of global immutability: the caller must supply
    /// the actual sole-initial-root/no-other-MM-history and exclusion of
    /// external, asynchronous kernel and remote writers throughout the copy.
    /// This includes original discard-history/queued-discard exclusion: zero
    /// LazyFree in smaps does not prove that no MADV_FREE work remains queued.
    /// A pathname or memory-metadata mutex supplies none of that authority.
    /// This restriction does not make smaps or remote GUP nonblocking or bounded
    /// in elapsed time; deadline/cancellation liveness needs separate ownership.
    ///
    /// Only `Fault` supplies a proven source-access denial; it does not prove
    /// that the consuming syscall would reach its source copy before another
    /// error. Mapping it to guest `EFAULT` requires independent caller proof of
    /// that syscall's error precedence. A native pipe-write comparison proves
    /// that particular access case, not arbitrary socket/send error precedence.
    /// Without that proof, both `Fault` and `Refused` must fail closed before
    /// the consuming physical effect, retaining their diagnostic distinction.
    /// Unsupported backends refuse even empty operands without ordinary reads.
    fn read_native_user_exact(
        &self,
        _expected_tid: i32,
        _address: usize,
        _buf: &mut [u8],
    ) -> Result<(), NativeUserReadError> {
        Err(NativeUserReadError::Refused(
            NativeUserReadRefusal::UnsupportedBackend,
        ))
    }

    /// Performs a read starting at the given address. The number of bytes read
    /// is returned. The buffer is not guaranteed to be completely filled.
    fn read<'a, A>(&self, addr: A, buf: &mut [u8]) -> Result<usize, Errno>
    where
        A: Into<Addr<'a, u8>>,
    {
        let slice = unsafe { AddrSlice::from_raw_parts(addr.into(), buf.len()) };
        let from = [unsafe { slice.as_ioslice() }];
        let mut to = [io::IoSliceMut::new(buf)];
        self.read_vectored(&from, &mut to)
    }

    /// Performs a write starting at the given address. The number of bytes
    /// written is returned. There is no guarantee that the given buffer will be
    /// fully written.
    fn write(&mut self, addr: AddrMut<u8>, buf: &[u8]) -> Result<usize, Errno> {
        let mut slice = unsafe { AddrSliceMut::from_raw_parts(addr, buf.len()) };
        let from = [io::IoSlice::new(buf)];
        let mut to = [unsafe { slice.as_ioslice_mut() }];
        self.write_vectored(&from, &mut to)
    }

    /// Writes one prefix while respecting the target's user mapping permissions.
    ///
    /// Unlike debugger writes, this must not force access to read-only memory.
    /// Supported implementations perform one increasing-address copy, without
    /// retrying a short transfer. A nonempty first-byte fault is `EFAULT`;
    /// otherwise the count describes exactly the bytes copied. A short count
    /// does not imply that the next byte is unwritable or identify a fault.
    /// Nonempty address-plus-length overflow is `EFAULT` before copying.
    ///
    /// A healthy supported empty copy returns zero without inspecting `addr`.
    /// Other errors retain their identity. A terminal backend failure may
    /// override a count or ordinary fault after effects, including on an empty
    /// operation. Callers must preserve those effects, stop, and not commit a
    /// consuming transaction or retry a terminal error as an ordinary fault.
    ///
    /// The default is unsupported, including for empty copies. `ENOSYS` means
    /// a missing backend capability: the consuming Tool must report a backend
    /// or Tool failure rather than forwarding it as a guest syscall errno.
    /// It must not fall back to generic debugger writes.
    ///
    /// This synchronous operation may block and adds no scheduling guarantee
    /// or snapshot of concurrent mapping changes. Permissions are those the
    /// backend supports; this does not add PKRU or tagged-address emulation.
    fn write_with_user_access(&mut self, _addr: AddrMut<u8>, _buf: &[u8]) -> Result<usize, Errno> {
        Err(Errno::ENOSYS)
    }

    /// Reads exactly the number of bytes wanted by `buf`.
    fn read_exact<'a, A>(&self, addr: A, mut buf: &mut [u8]) -> Result<(), Errno>
    where
        A: Into<Addr<'a, u8>>,
    {
        let mut addr = addr.into();

        while !buf.is_empty() {
            match self.read(addr, buf)? {
                0 => break,
                n => {
                    addr = unsafe { addr.add(n) };
                    buf = &mut buf[n..];
                }
            }
        }

        if !buf.is_empty() {
            // Failed to fill the whole buffer.
            Err(Errno::EFAULT)
        } else {
            Ok(())
        }
    }

    /// Reads exactly the number of bytes wanted by `buf`, while respecting the
    /// target process's userspace memory protections.
    ///
    /// This performs one read rather than retrying a partial transfer. A short
    /// read therefore reports `EFAULT`, matching the all-or-error behavior of
    /// Linux helpers such as `copy_from_user`.
    fn read_exact_with_user_access<'a, A>(&self, addr: A, buf: &mut [u8]) -> Result<(), Errno>
    where
        A: Into<Addr<'a, u8>>,
    {
        let addr = addr.into();
        addr.as_raw().checked_add(buf.len()).ok_or(Errno::EFAULT)?;

        if self.read(addr, buf)? == buf.len() {
            Ok(())
        } else {
            Err(Errno::EFAULT)
        }
    }

    /// Reads exactly the number of bytes wanted by `buf`.
    fn write_exact(&mut self, mut addr: AddrMut<u8>, mut buf: &[u8]) -> Result<(), Errno> {
        while !buf.is_empty() {
            match self.write(addr, buf)? {
                0 => break,
                n => {
                    addr = unsafe { addr.add(n) };
                    buf = &buf[n..];
                }
            }
        }

        if !buf.is_empty() {
            // Failed to fill the whole buffer.
            Err(Errno::EFAULT)
        } else {
            Ok(())
        }
    }

    /// Reads a value at the given address.
    fn read_value<'a, A, T>(&self, addr: A) -> Result<T, Errno>
    where
        A: Into<Addr<'a, T>>,
        T: Sized + 'a,
    {
        let addr = addr.into();
        let mut value = mem::MaybeUninit::uninit();

        let value_buf = unsafe {
            ::core::slice::from_raw_parts_mut(value.as_mut_ptr() as *mut u8, mem::size_of::<T>())
        };

        self.read_exact(addr.cast::<u8>(), value_buf)?;

        Ok(unsafe { value.assume_init() })
    }

    /// Writes a value to the given address.
    fn write_value<'a, A, T>(&mut self, addr: A, value: &T) -> Result<(), Errno>
    where
        A: Into<AddrMut<'a, T>>,
        T: Sized + 'a,
    {
        let addr = addr.into();

        let value_buf = unsafe {
            ::core::slice::from_raw_parts(value as *const _ as *const u8, mem::size_of::<T>())
        };

        self.write_exact(addr.cast::<u8>(), value_buf)?;

        Ok(())
    }

    /// Reads a slice of values. Returns an error if the buffer fails to get
    /// fully filled.
    fn read_values<T>(&self, addr: Addr<T>, buf: &mut [T]) -> Result<(), Errno>
    where
        T: Sized,
    {
        let buf = unsafe {
            ::core::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, mem::size_of_val(buf))
        };

        self.read_exact(addr.cast::<u8>(), buf)
    }

    /// Writes a slice of values. Returns an error if the buffer fails to get
    /// fully written.
    fn write_values<T>(&mut self, addr: AddrMut<T>, buf: &[T]) -> Result<(), Errno>
    where
        T: Sized,
    {
        let buf = unsafe {
            ::core::slice::from_raw_parts(buf.as_ptr() as *const u8, mem::size_of_val(buf))
        };

        self.write_exact(addr.cast::<u8>(), buf)
    }

    /// Reads memory at the given starting address while the boolean returned by
    /// the predicate `pred` is true.
    fn read_while<F>(&self, mut addr: Addr<u8>, buf: &mut [u8], mut pred: F) -> Result<usize, Errno>
    where
        F: FnMut(&[u8]) -> Option<usize>,
    {
        let mut count = 0usize;

        loop {
            let read = self.read(addr, buf)?;
            if read == 0 {
                // We hit an "EOF" (an EFAULT) and the predicate never matched.
                // The predicate should *eventually* return true, so this is
                // always an error.
                return Err(Errno::EFAULT);
            }

            if let Some(used) = pred(&buf[..read]) {
                return count.checked_add(used).ok_or(Errno::EFAULT);
            }

            // Only advance when another chunk is needed. Addresses may name
            // remote memory, so neither in-bounds pointer arithmetic nor a
            // wrapping address is valid for this traversal.
            let next = addr.as_raw().checked_add(read).ok_or(Errno::EFAULT)?;
            addr = Addr::from_raw(next).ok_or(Errno::EFAULT)?;
            count = count.checked_add(read).ok_or(Errno::EFAULT)?;
        }
    }

    /// Reads a NUL terminated string using the provided buffer to read it in
    /// chunks. Change the size of the buffer to adjust how many bytes are read
    /// at one time. Increasing the buffer size can be more efficient when
    /// reading a remote C string because it reduces the number of syscalls that
    /// are made.
    fn read_cstring_with_buf(&self, addr: Addr<u8>, buf: &mut [u8]) -> Result<CString, Errno> {
        let mut accumulator = Vec::new();

        self.read_while(addr, buf, |slice| {
            if let Some(nul) = slice.iter().position(|&b| b == 0) {
                // Stop once we find a NUL terminator.
                accumulator.extend(&slice[..nul]);
                Some(nul)
            } else {
                accumulator.extend(slice);
                None
            }
        })?;

        // unsafe is okay here; the vector is guaranteed to not contain a nul
        // byte.
        Ok(unsafe { CString::from_vec_unchecked(accumulator) })
    }

    /// Reads a null-terminated string starting at the given address.
    fn read_cstring(&self, addr: Addr<u8>) -> Result<CString, Errno> {
        // Assume most strings are smallish. We need to balance the overhead of
        // copying data vs the average length of C-strings.
        let mut buf: [u8; 512] = [0; 512];

        self.read_cstring_with_buf(addr, &mut buf)
    }

    /// Returns a struct that implements `std::io::Read`. This is useful when
    /// reading memory sequentially.
    fn reader<'a, T>(&'a self, addr: Addr<'a, T>) -> MemoryReader<'a, Self, T>
    where
        Self: Sized,
    {
        MemoryReader::new(self, addr)
    }

    /// Returns a struct that implements `std::io::Write`. This is useful when
    /// writing memory sequentially.
    fn writer<'a, T>(&'a mut self, addr: AddrMut<'a, T>) -> MemoryWriter<'a, Self, T>
    where
        Self: Sized,
    {
        MemoryWriter::new(self, addr)
    }
}

/// A wrapper around both an address space and a pointer for sequential reads.
pub struct MemoryReader<'a, M, T> {
    memory: &'a M,

    addr: Addr<'a, T>,
}

impl<'a, M, T> MemoryReader<'a, M, T> {
    /// Creates a new `MemoryReader`. All reads will start at `addr`. It is the
    /// callers job to avoid buffer overruns.
    pub fn new(memory: &'a M, addr: Addr<'a, T>) -> Self {
        MemoryReader { memory, addr }
    }
}

impl<'a, M, T> MemoryReader<'a, M, T>
where
    M: MemoryAccess,
    T: Sized + Copy,
{
    /// Reads a single typed value from the buffer.
    pub fn read_value(&mut self) -> Result<T, Errno> {
        let value = self.memory.read_value(self.addr)?;
        self.addr = unsafe { self.addr.add(1) };
        Ok(value)
    }
}

impl<'a, M> io::Read for MemoryReader<'a, M, u8>
where
    M: MemoryAccess,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = self.memory.read(self.addr, buf)?;

        self.addr = unsafe { self.addr.add(count) };

        Ok(count)
    }
}

/// A wrapper around both an address space and a pointer for sequential writes.
pub struct MemoryWriter<'a, M, T> {
    memory: &'a mut M,

    addr: AddrMut<'a, T>,
}

impl<'a, M, T> MemoryWriter<'a, M, T> {
    /// Creates a new `MemoryWriter`. All writes will start at `addr`. It is the
    /// callers job to avoid buffer overruns.
    pub fn new(memory: &'a mut M, addr: AddrMut<'a, T>) -> Self {
        MemoryWriter { memory, addr }
    }
}

impl<'a, M, T> MemoryWriter<'a, M, T>
where
    M: MemoryAccess,
    T: Sized + Copy,
{
    /// Reads a single typed value from the buffer.
    pub fn write_value(&mut self, value: &T) -> Result<(), Errno> {
        self.memory.write_value(self.addr, value)?;
        self.addr = unsafe { self.addr.add(1) };
        Ok(())
    }
}

impl<'a, M> io::Write for MemoryWriter<'a, M, u8>
where
    M: MemoryAccess,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let count = self.memory.write(self.addr, buf)?;

        self.addr = unsafe { self.addr.add(count) };

        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        // Flush doesn't make any sense when writing to memory.
        Ok(())
    }
}

#[cfg(test)]
mod user_access_default_tests {
    use super::*;
    struct LegacyWriter {
        calls: usize,
    }
    impl MemoryAccess for LegacyWriter {
        fn read_vectored(
            &self,
            _: &[io::IoSlice],
            _: &mut [io::IoSliceMut],
        ) -> Result<usize, Errno> {
            panic!("copyout must not probe memory")
        }
        fn write_vectored(
            &mut self,
            _: &[io::IoSlice],
            _: &mut [io::IoSliceMut],
        ) -> Result<usize, Errno> {
            panic!("new capability must not fall back to vectored debugger writes")
        }
        fn write(&mut self, _: AddrMut<u8>, bytes: &[u8]) -> Result<usize, Errno> {
            self.calls += 1;
            Ok(bytes.len())
        }
    }
    #[test]
    fn unsupported_user_copy_never_falls_back_to_debugger_write() {
        let mut memory = LegacyWriter { calls: 0 };
        let address = AddrMut::from_raw(usize::MAX).unwrap();
        for bytes in [&[][..], &[1, 2, 3, 4, 5, 6, 7][..], &[0; 8][..]] {
            assert_eq!(
                memory.write_with_user_access(address, bytes),
                Err(Errno::ENOSYS)
            );
            assert_eq!(memory.calls, 0);
        }
        assert_eq!(memory.write(address, &[0; 8]), Ok(8));
        assert_eq!(memory.calls, 1);
    }
}

#[cfg(test)]
mod native_user_write_tests {
    use super::*;

    #[test]
    fn native_key0_write_access_default_refuses_without_memory_effects() {
        struct Unsupported;
        impl MemoryAccess for Unsupported {
            fn read_vectored(
                &self,
                _: &[io::IoSlice],
                _: &mut [io::IoSliceMut],
            ) -> Result<usize, Errno> {
                panic!("key0 validation must not call ordinary read");
            }

            fn write_vectored(
                &mut self,
                _: &[io::IoSlice],
                _: &mut [io::IoSliceMut],
            ) -> Result<usize, Errno> {
                panic!("key0 validation must not call ordinary write");
            }
        }

        assert_eq!(
            Unsupported.validate_native_user_key0_write_access(1),
            Err(Errno::EOPNOTSUPP)
        );
    }

    #[test]
    fn native_user_write_default_refuses_without_using_ordinary_memory_access() {
        struct Unsupported;
        impl MemoryAccess for Unsupported {
            fn read_vectored(
                &self,
                _: &[io::IoSlice],
                _: &mut [io::IoSliceMut],
            ) -> Result<usize, Errno> {
                panic!("native write must not call ordinary read");
            }

            fn write_vectored(
                &mut self,
                _: &[io::IoSlice],
                _: &mut [io::IoSliceMut],
            ) -> Result<usize, Errno> {
                panic!("native write must not call ordinary write");
            }
        }

        let mut memory = Unsupported;
        let local = [io::IoSlice::new(b"newbytes")];
        let target = [0xa5; 8];
        let remote = [RemoteIoVec::new(AddrMut::from_ptr(target.as_ptr()).unwrap(), 8).unwrap()];
        let result = memory.write_native_user_vectored(1, &local, &remote);
        assert_eq!(result, Err(Errno::EOPNOTSUPP));
        assert_eq!(Errno::EOPNOTSUPP, Errno::new(libc::ENOTSUP));
        assert_eq!(target, [0xa5; 8]);
    }
}
