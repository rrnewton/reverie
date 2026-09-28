/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Bounded, independently owned byte streams in a shared Linux mapping.
//!
//! Each connection has two single-producer/single-consumer queues. A transfer
//! can exceed the queue capacity: writers apply backpressure until readers
//! advance. There is no registry or participant limit in this component.
//!
//! Setup returns one transferable memfd. Import maps and closes it before
//! returning a stream; neither stream retains a descriptor. This component
//! provides logical close and explicit abort, not kernel process lifetime,
//! early loader initialization, fork registration, or a backend selection.
//! A lifecycle owner must abort an abandoned connection and independently
//! establish actual process exit. Shared memory is not a security boundary.

use std::io::Read;
use std::io::Write;
use std::io::{self};
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

mod asynchronous;
mod inherited;
mod workers;
pub use asynchronous::AsyncMappedStream;
pub use inherited::InheritedMappingReleaseError;
pub use inherited::InheritedMappingReleaseFailure;
pub use workers::MappedCompletion;
pub use workers::MappedHelperFailure;
pub use workers::MappedReaperCleanup;
pub use workers::MappedStartError;
pub use workers::MappedStartStage;
pub use workers::MappedWorkerFailure;

const MAGIC: [u8; 8] = *b"RVPRPC01";
const VERSION: u32 = 1;
const SEALS: libc::c_int = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW;
const WAIT_NS: libc::c_long = 100_000_000;

#[repr(C, align(64))]
struct Queue {
    read: AtomicU64,
    written: AtomicU64,
    writer_closed: AtomicU32,
    reader_closed: AtomicU32,
}

impl Queue {
    fn new() -> Self {
        Self {
            read: AtomicU64::new(0),
            written: AtomicU64::new(0),
            writer_closed: AtomicU32::new(0),
            reader_closed: AtomicU32::new(0),
        }
    }
}

#[repr(C, align(64))]
struct Header {
    magic: [u8; 8],
    version: u32,
    header_len: u32,
    mapped_len: u64,
    capacity: u64,
    offsets: [u64; 2],
    peer_claimed: AtomicU32,
    failure: AtomicU32,
    progress: AtomicU32,
    queues: [Queue; 2],
}

/// Persistent connection failure, distinct from a graceful write-side close.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MappedFailure {
    /// Its owner cancelled the connection.
    Cancelled = 1,
    /// Its owner observed a failed or abandoned peer.
    /// This value does not itself certify kernel process exit.
    PeerFailed = 2,
    /// The mapping's cursor or protocol invariants failed.
    Protocol = 3,
}

/// A handle for a lifecycle owner to fail a connection and wake its waiters.
///
/// Aborting is sticky. Dropping this handle has no effect. It is not a process
/// handle, and connection failure cannot replace actual process-exit evidence.
#[derive(Clone)]
pub struct MappedAbort {
    mapping: Arc<Mapping>,
}

impl MappedAbort {
    pub fn abort(&self, failure: MappedFailure) {
        self.mapping.abort(failure);
    }
}

/// One end of a shared-memory byte stream. It deliberately has no AsRawFd.
///
/// Move this value to its originating client thread before receiving the RPC
/// config handshake. Do not inherit an active endpoint into another process
/// and operate both copies: each direction requires one producer and consumer.
pub struct MappedStream {
    mapping: Arc<Mapping>,
    side: usize,
}

impl MappedStream {
    /// Create one endpoint and a memfd for importing its peer during setup.
    /// The returned fd is close-on-exec and is not retained by this endpoint.
    /// Prefer [`Self::pair`] when both endpoints are created in this process.
    ///
    /// # Safety
    /// From the moment this function returns until all associated streams,
    /// abort handles, and notification workers have released their mappings,
    /// the caller must prevent writes through the descriptor, its duplicates,
    /// inherited descriptors, or any other mapping of this backing storage.
    /// Only this endpoint, one peer imported with [`Self::from_owned_fd`],
    /// and their associated abort handles and notification workers may access
    /// the shared protocol and payload through this module's operations.
    /// In particular, safe file writes through the returned descriptor would
    /// violate this contract even before the peer is imported. The size seals
    /// do not prevent writes. Transfer the descriptor only to a trusted setup
    /// owner that observes the same contract and consumes it during import.
    /// Neither endpoint may be duplicated by fork and operated concurrently.
    ///
    /// ```compile_fail,E0133
    /// use reverie_rpc_transport::mapped::MappedStream;
    /// // Exporting writable backing storage requires an explicit unsafe call.
    /// let _ = MappedStream::create(31);
    /// ```
    pub unsafe fn create(capacity: usize) -> io::Result<(Self, OwnedFd)> {
        let len = layout_len(capacity)?;
        let raw = unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                c"reverie-rpc".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw as libc::c_int) };
        if unsafe { libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) } != 0
            || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut mapping = Mapping::map(&fd, len)?;
        mapping.capacity = capacity;
        mapping.offsets = [size_of::<Header>(), size_of::<Header>() + capacity];
        let header = Header {
            magic: MAGIC,
            version: VERSION,
            header_len: size_of::<Header>() as u32,
            mapped_len: len as u64,
            capacity: capacity as u64,
            offsets: mapping.offsets.map(|offset| offset as u64),
            peer_claimed: AtomicU32::new(0),
            failure: AtomicU32::new(0),
            progress: AtomicU32::new(0),
            queues: [Queue::new(), Queue::new()],
        };
        // No peer can see the descriptor until this constructor returns.
        unsafe { mapping.ptr.as_ptr().cast::<Header>().write(header) };
        Ok((
            Self {
                mapping: Arc::new(mapping),
                side: 0,
            },
            fd,
        ))
    }

    /// Map the peer endpoint and consume/close its setup descriptor.
    /// Duplicate imports are rejected; an endpoint cannot be reopened.
    ///
    /// # Safety
    /// The descriptor must be exclusively transferred by a trusted setup
    /// owner. After import, only the opposite MappedStream may change queue
    /// state/data; no other mapping or file writer may modify this memory.
    /// Neither endpoint may be duplicated by fork and operated concurrently.
    /// Initial malformed headers are checked and return an error.
    pub unsafe fn from_owned_fd(fd: OwnedFd) -> io::Result<Self> {
        let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0 || seals & SEALS != SEALS {
            return Err(invalid(
                "mapped RPC requires sealed, fixed-size backing storage",
            ));
        }
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let len =
            usize::try_from(stat.st_size).map_err(|_| invalid("invalid mapped RPC length"))?;
        if len < size_of::<Header>() || len > isize::MAX as usize {
            return Err(invalid("invalid mapped RPC length"));
        }
        let mut mapping = Mapping::map(&fd, len)?;
        let header = mapping.header();
        let capacity =
            usize::try_from(header.capacity).map_err(|_| invalid("invalid queue capacity"))?;
        let offsets = header.offsets;
        if header.magic != MAGIC
            || header.version != VERSION
            || header.header_len as usize != size_of::<Header>()
            || header.mapped_len != len as u64
            || layout_len(capacity)? != len
            || offsets
                != [
                    size_of::<Header>() as u64,
                    (size_of::<Header>() + capacity) as u64,
                ]
        {
            return Err(invalid("invalid mapped RPC header or offsets"));
        }
        if header
            .peer_claimed
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "mapped RPC peer already imported",
            ));
        }
        mapping.capacity = capacity;
        mapping.offsets = offsets.map(|offset| offset as usize);
        // Closure precedes construction/publication of the usable endpoint.
        drop(fd);
        Ok(Self {
            mapping: Arc::new(mapping),
            side: 1,
        })
    }

    /// Create a pair in this process, closing its setup memfd before returning.
    pub fn pair(capacity: usize) -> io::Result<(Self, Self)> {
        // The fresh descriptor is never exposed to callers or external writers;
        // it is consumed by exactly one peer import before either endpoint escapes.
        let (first, fd) = unsafe { Self::create(capacity) }?;
        let second = unsafe { Self::from_owned_fd(fd) }?;
        Ok((first, second))
    }

    pub fn abort_handle(&self) -> MappedAbort {
        MappedAbort {
            mapping: self.mapping.clone(),
        }
    }

    /// Finish this endpoint's writes. Buffered bytes remain readable before EOF.
    /// This is logical stream closure and does not imply process exit.
    pub fn close_write(&mut self) {
        self.mapping.header().queues[self.side]
            .writer_closed
            .store(1, Ordering::Release);
        self.mapping.notify();
    }

    /// Adapt the host endpoint to Tokio without occupying a blocking-pool worker.
    /// One dedicated host thread waits for shared-memory progress notifications.
    pub fn into_async(self) -> io::Result<AsyncMappedStream> {
        AsyncMappedStream::new(self)
    }

    fn try_read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.mapping.check_failure()?;
        let side = 1 - self.side;
        let queue = &self.mapping.header().queues[side];
        // Acquiring close before the cursor includes every preceding write.
        let closed = queue.writer_closed.load(Ordering::Acquire) != 0;
        let read = queue.read.load(Ordering::Relaxed);
        let written = queue.written.load(Ordering::Acquire);
        let used = self.mapping.used(read, written)?;
        if used == 0 {
            return if closed {
                Ok(0)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }
        let count = used.min(buffer.len());
        let next = read
            .checked_add(count as u64)
            .ok_or_else(|| self.mapping.protocol_error())?;
        let start = (read % self.mapping.capacity as u64) as usize;
        let first = count.min(self.mapping.capacity - start);
        // The producer published these bytes before its release store. It may
        // not reuse them until the consumer's release store below.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.mapping.data(side, start),
                buffer.as_mut_ptr(),
                first,
            );
            std::ptr::copy_nonoverlapping(
                self.mapping.data(side, 0),
                buffer.as_mut_ptr().add(first),
                count - first,
            );
        }
        queue.read.store(next, Ordering::Release);
        self.mapping.notify();
        Ok(count)
    }

    fn try_write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.mapping.check_failure()?;
        let queue = &self.mapping.header().queues[self.side];
        if queue.reader_closed.load(Ordering::Acquire) != 0
            || queue.writer_closed.load(Ordering::Acquire) != 0
        {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let written = queue.written.load(Ordering::Relaxed);
        let read = queue.read.load(Ordering::Acquire);
        let free = self.mapping.capacity - self.mapping.used(read, written)?;
        if free == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let count = free.min(buffer.len());
        let next = written
            .checked_add(count as u64)
            .ok_or_else(|| self.mapping.protocol_error())?;
        let start = (written % self.mapping.capacity as u64) as usize;
        let first = count.min(self.mapping.capacity - start);
        // This endpoint alone produces this queue. The peer's release cursor
        // proves these slots are no longer being read.
        unsafe {
            std::ptr::copy_nonoverlapping(
                buffer.as_ptr(),
                self.mapping.data(self.side, start),
                first,
            );
            std::ptr::copy_nonoverlapping(
                buffer.as_ptr().add(first),
                self.mapping.data(self.side, 0),
                count - first,
            );
        }
        queue.written.store(next, Ordering::Release);
        self.mapping.notify();
        Ok(count)
    }
}

impl Read for MappedStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            let progress = self.mapping.header().progress.load(Ordering::Acquire);
            match self.try_read(buffer) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.mapping.wait(progress)?,
                result => return result,
            }
        }
    }
}

impl MappedStream {
    /// Read available bytes using ordinary partial-read and EOF semantics,
    /// bounded by an absolute native host deadline. A timed-out read does not
    /// roll back bytes already copied or queue progress. A framed setup owner
    /// must retire a failed transaction rather than replaying it as a new frame.
    pub fn read_until(
        &mut self,
        buffer: &mut [u8],
        deadline: std::time::Instant,
    ) -> io::Result<usize> {
        loop {
            remaining(deadline)?;
            let progress = self.mapping.header().progress.load(Ordering::Acquire);
            match self.try_read(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.mapping.wait_until(progress, deadline)?;
                }
                result => {
                    remaining(deadline)?;
                    return result;
                }
            }
        }
    }

    /// Read one setup record under an absolute native host deadline.
    ///
    /// A timeout preserves any bytes already consumed in the supplied buffer;
    /// it does not roll back queue cursors. A framed setup owner must retire the
    /// failed transaction rather than retry its whole record as a new message.
    /// This clock is independent of a deterministic runtime's virtual time.
    pub fn read_exact_until(
        &mut self,
        mut buffer: &mut [u8],
        deadline: std::time::Instant,
    ) -> io::Result<()> {
        loop {
            remaining(deadline)?;
            if buffer.is_empty() {
                return Ok(());
            }
            let progress = self.mapping.header().progress.load(Ordering::Acquire);
            match self.try_read(buffer) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(count) => buffer = &mut buffer[count..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.mapping.wait_until(progress, deadline)?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Write one setup record under the same absolute deadline as its other
    /// setup operations. A timeout retains an already published prefix; callers
    /// must not resend that prefix as a fresh transaction.
    pub fn write_all_until(
        &mut self,
        mut buffer: &[u8],
        deadline: std::time::Instant,
    ) -> io::Result<()> {
        loop {
            remaining(deadline)?;
            if buffer.is_empty() {
                return Ok(());
            }
            let progress = self.mapping.header().progress.load(Ordering::Acquire);
            match self.try_write(buffer) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => buffer = &buffer[count..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.mapping.wait_until(progress, deadline)?;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn remaining(deadline: std::time::Instant) -> io::Result<std::time::Duration> {
    deadline
        .checked_duration_since(std::time::Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::ErrorKind::TimedOut.into())
}

impl Mapping {
    fn wait_until(&self, progress: u32, deadline: std::time::Instant) -> io::Result<()> {
        let duration = remaining(deadline)?.min(std::time::Duration::from_nanos(WAIT_NS as u64));
        let timeout = libc::timespec {
            tv_sec: duration.as_secs() as libc::time_t,
            tv_nsec: duration.subsec_nanos().into(),
        };
        let result = unsafe {
            libc::syscall(
                libc::SYS_futex,
                self.header().progress.as_ptr(),
                libc::FUTEX_WAIT,
                progress,
                &timeout,
            )
        };
        if result >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EAGAIN | libc::EINTR | libc::ETIMEDOUT) => Ok(()),
            _ => {
                self.abort(MappedFailure::PeerFailed);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod deadline_tests {
    use std::time::Duration;
    use std::time::Instant;

    use super::*;

    fn pair() -> (MappedStream, MappedStream) {
        // Exactly one cooperating writer/reader per side; no exported aliases
        // or fork. The two owners stay alive until their native test completes.
        let (host, fd) = unsafe { MappedStream::create(64) }.unwrap();
        (host, unsafe { MappedStream::from_owned_fd(fd) }.unwrap())
    }

    #[test]
    fn deadline_read_preserves_partial_read_and_clean_eof() {
        let (mut host, mut peer) = MappedStream::pair(32).unwrap();
        peer.write_all(b"abc").unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut bytes = [0; 8];
        assert_eq!(host.read_until(&mut bytes, deadline).unwrap(), 3);
        assert_eq!(&bytes[..3], b"abc");
        peer.close_write();
        assert_eq!(host.read_until(&mut bytes, deadline).unwrap(), 0);
    }

    #[test]
    fn deadline_read_keeps_partial_bytes_and_queue_progress() {
        let (mut host, mut peer) = pair();
        peer.write_all(b"abc").unwrap();
        let mut bytes = [0; 8];
        let deadline = Instant::now() + Duration::from_millis(20);
        assert_eq!(
            host.read_exact_until(&mut bytes, deadline)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(Instant::now() >= deadline);
        assert_eq!(bytes, *b"abc\0\0\0\0\0");
        peer.write_all(b"defgh").unwrap();
        host.read_exact_until(&mut bytes[3..], Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(bytes, *b"abcdefgh");
    }

    #[test]
    fn deadline_write_keeps_the_actual_published_prefix() {
        let (mut host, mut peer) = pair();
        let bytes: Vec<_> = (0..128_u8).collect();
        let deadline = Instant::now() + Duration::from_millis(20);
        assert_eq!(
            host.write_all_until(&bytes, deadline).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(Instant::now() >= deadline);
        let mut actual = [0; 64];
        peer.read_exact_until(&mut actual, Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(actual, bytes[..64]);
        host.write_all_until(&bytes[64..], Instant::now() + Duration::from_secs(1))
            .unwrap();
        peer.read_exact_until(&mut actual, Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(actual, bytes[64..]);
    }

    #[test]
    fn expired_deadline_refuses_even_when_the_queue_can_progress() {
        let (mut host, mut peer) = pair();
        peer.write_all(b"x").unwrap();
        let deadline = Instant::now();
        let mut byte = [0];
        assert_eq!(
            host.read_exact_until(&mut byte, deadline)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(byte, [0]);
        assert_eq!(
            host.write_all_until(b"y", deadline).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            peer.try_read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        host.read_exact_until(&mut byte, Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(byte, *b"x");
    }

    #[test]
    fn repeated_native_wakes_do_not_extend_the_deadline() {
        let (mut host, _peer) = pair();
        let mapping = host.mapping.clone();
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = stopped.clone();
        let waker = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                mapping.notify();
                std::thread::yield_now();
            }
        });
        let deadline = Instant::now() + Duration::from_millis(20);
        let result = host.read_exact_until(&mut [0], deadline);
        stopped.store(true, Ordering::Release);
        waker.join().unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(Instant::now() >= deadline);
    }
}

impl Write for MappedStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        loop {
            let progress = self.mapping.header().progress.load(Ordering::Acquire);
            match self.try_write(buffer) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.mapping.wait(progress)?,
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.mapping.check_failure()
    }
}

impl Drop for MappedStream {
    fn drop(&mut self) {
        let header = self.mapping.header();
        header.queues[self.side]
            .writer_closed
            .store(1, Ordering::Release);
        header.queues[1 - self.side]
            .reader_closed
            .store(1, Ordering::Release);
        self.mapping.notify();
    }
}

struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
    capacity: usize,
    offsets: [usize; 2],
}

// The immutable layout is checked once and cached. Queue ownership belongs to
// non-Clone endpoints; byte access is synchronized by the queue's atomics.
// Other shared owners touch only atomic progress/failure state.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    fn map(fd: &OwnedFd, len: usize) -> io::Result<Self> {
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let Some(ptr) = NonNull::new(ptr.cast()) else {
            unsafe { libc::munmap(ptr, len) };
            return Err(invalid("mapped RPC cannot use a null mapping"));
        };
        Ok(Self {
            ptr,
            len,
            capacity: 0,
            offsets: [0; 2],
        })
    }
    fn header(&self) -> &Header {
        unsafe { &*self.ptr.as_ptr().cast::<Header>() }
    }
    unsafe fn data(&self, side: usize, start: usize) -> *mut u8 {
        // Offset/capacity arithmetic was checked before this mapping was used.
        unsafe { self.ptr.as_ptr().add(self.offsets[side] + start) }
    }
    fn used(&self, read: u64, written: u64) -> io::Result<usize> {
        match written.checked_sub(read) {
            Some(used) if used <= self.capacity as u64 => Ok(used as usize),
            _ => Err(self.protocol_error()),
        }
    }
    fn protocol_error(&self) -> io::Error {
        self.abort(MappedFailure::Protocol);
        invalid("mapped RPC cursor invariant failed")
    }
    fn abort(&self, failure: MappedFailure) {
        let _ = self.header().failure.compare_exchange(
            0,
            failure as u32,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.notify();
    }
    fn check_failure(&self) -> io::Result<()> {
        match self.header().failure.load(Ordering::Acquire) {
            0 => Ok(()),
            1 => Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "mapped RPC cancelled",
            )),
            2 => Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "mapped RPC peer failed",
            )),
            _ => Err(invalid("mapped RPC protocol failure")),
        }
    }
    fn notify(&self) {
        self.header().progress.fetch_add(1, Ordering::Release);
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                self.header().progress.as_ptr(),
                libc::FUTEX_WAKE,
                libc::c_int::MAX,
            )
        };
    }
    fn wait(&self, progress: u32) -> io::Result<()> {
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: WAIT_NS,
        };
        let rc = unsafe {
            libc::syscall(
                libc::SYS_futex,
                self.header().progress.as_ptr(),
                libc::FUTEX_WAIT,
                progress,
                &timeout,
            )
        };
        if rc >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EAGAIN | libc::EINTR | libc::ETIMEDOUT) => Ok(()),
            _ => {
                self.abort(MappedFailure::PeerFailed);
                Err(error)
            }
        }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

fn layout_len(capacity: usize) -> io::Result<usize> {
    capacity
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(size_of::<Header>()))
        .filter(|len| {
            capacity != 0 && *len <= isize::MAX as usize && *len <= libc::off_t::MAX as usize
        })
        .ok_or_else(|| invalid("invalid mapped RPC queue capacity"))
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
