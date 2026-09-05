use std::collections::BTreeMap;
use std::io;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie_preload::trap::raw_syscall6;

pub(crate) static LOG_FD: AtomicI32 = AtomicI32::new(-1);
static LOG_PID: AtomicI32 = AtomicI32::new(0);
static LOG_PRODUCER: AtomicU64 = AtomicU64::new(0);
static LOG_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static NEXT_PRODUCER: AtomicPtr<AtomicU64> = AtomicPtr::new(std::ptr::null_mut());
static EMITTER: AtomicU64 = AtomicU64::new(0);
static FAILURE: AtomicPtr<AtomicU64> = AtomicPtr::new(std::ptr::null_mut());
pub(crate) const IDENTITY_BYTES: usize = 20;
const CHUNK_BYTES: usize = 4096;
const START: u8 = 0;
const DATA: u8 = 1;
const FINISH: u8 = 2;
const EXPECT_CHILD: u8 = 3;
const BEGIN: u8 = 4;
const END: u8 = 5;
const HEADER_BYTES: usize = 41;
const MAX_PRODUCERS: usize = 1024;
const FAILURE_BOOTSTRAP: &[u8] = b"LGF1";

struct FailureState(*mut AtomicU64);

impl FailureState {
    fn receive(socket: i32) -> io::Result<Self> {
        let mut bytes = [0; 4];
        let mut control = [0_usize; 16];
        let mut vector = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &raw mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&control);
        let size = loop {
            let result = unsafe {
                libc::recvmsg(
                    socket,
                    &raw mut message,
                    libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
                )
            };
            if result >= 0 {
                break result;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        };
        let mut descriptor = None;
        let mut valid = true;
        let mut ancillary = unsafe { libc::CMSG_FIRSTHDR(&message) };
        while !ancillary.is_null() {
            let entry = unsafe { &*ancillary };
            if entry.cmsg_level == libc::SOL_SOCKET && entry.cmsg_type == libc::SCM_RIGHTS {
                let length = entry.cmsg_len - unsafe { libc::CMSG_LEN(0) } as usize;
                let data = unsafe { libc::CMSG_DATA(ancillary) };
                for offset in (0..length).step_by(std::mem::size_of::<i32>()) {
                    let fd = unsafe {
                        OwnedFd::from_raw_fd(data.add(offset).cast::<i32>().read_unaligned())
                    };
                    if descriptor.is_some() {
                        valid = false;
                    } else {
                        descriptor = Some(fd);
                    }
                }
            }
            ancillary = unsafe { libc::CMSG_NXTHDR(&message, ancillary) };
        }
        if !valid
            || size != 4
            || bytes != FAILURE_BOOTSTRAP
            || message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
        {
            return Err(invalid("invalid guest log failure bootstrap"));
        }
        let descriptor =
            descriptor.ok_or_else(|| invalid("missing guest log failure descriptor"))?;
        let fd = descriptor.as_raw_fd();
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let seals = unsafe { libc::fcntl(fd, libc::F_GET_SEALS) };
        let required = libc::F_SEAL_SEAL | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK;
        if unsafe { libc::fstat(fd, &raw mut stat) } != 0
            || stat.st_size != 8
            || seals < 0
            || seals & required != required
        {
            return Err(invalid("invalid guest log failure mapping"));
        }
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                8,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(mapping.cast()))
    }

    fn failed(&self) -> bool {
        unsafe { &*self.0 }.load(Ordering::Acquire) != 0
    }
}

impl Drop for FailureState {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.0.cast(), 8) };
    }
}

fn send_failure_bootstrap(socket: i32, fd: i32) -> io::Result<()> {
    let mut control = [0_usize; 4];
    let mut vector = libc::iovec {
        iov_base: FAILURE_BOOTSTRAP.as_ptr().cast_mut().cast(),
        iov_len: FAILURE_BOOTSTRAP.len(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen =
        unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) } as usize;
    let ancillary = unsafe { libc::CMSG_FIRSTHDR(&message) };
    unsafe {
        (*ancillary).cmsg_level = libc::SOL_SOCKET;
        (*ancillary).cmsg_type = libc::SCM_RIGHTS;
        (*ancillary).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as usize;
        libc::CMSG_DATA(ancillary).cast::<i32>().write_unaligned(fd);
    }
    loop {
        let size =
            unsafe { libc::sendmsg(socket, &message, libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) };
        if size == FAILURE_BOOTSTRAP.len() as isize {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if size >= 0 {
            return Err(invalid("guest log failure bootstrap send failed"));
        }
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// One complete formatter write, reconstructed without newline parsing.
#[derive(Debug)]
pub struct GuestLogRecord<'a> {
    /// Kernel process ID, diagnostic only; may be reused after process exit.
    pub pid: i32,
    /// Unique incarnation allocated from this log channel's inherited counter.
    pub producer: u64,
    /// Per-producer event sequence, including child declarations and completion.
    pub sequence: u64,
    /// Exact formatter bytes, including any embedded newlines.
    pub bytes: &'a [u8],
}

pub(crate) type RecordConsumer = Box<dyn FnMut(&GuestLogRecord<'_>) -> io::Result<()> + Send>;

/// Retained guest tracing bytes, separate from application output.
pub struct CapturedGuestLog {
    /// Unmodified formatter output up to the requested bound.
    pub bytes: Vec<u8>,
    /// Missing completion, transport failure, or truncation; never parity evidence.
    pub error: Option<String>,
}

/// A sealed-bootstrap-owned log channel, not an application descriptor.
pub struct GuestLog(OwnedFd);

impl GuestLog {
    /// Reserves the channel before tool installation and returns a synchronous writer.
    ///
    /// # Safety
    /// Call once in the selected preload constructor, before application threads.
    pub unsafe fn install(self) -> io::Result<GuestLogWriter> {
        let failure = FailureState::receive(self.0.as_raw_fd())?;
        let counter = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                std::mem::size_of::<AtomicU64>(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if counter == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        LOG_FD
            .compare_exchange(-1, self.0.as_raw_fd(), Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                unsafe { libc::munmap(counter, std::mem::size_of::<AtomicU64>()) };
                io::Error::new(io::ErrorKind::AlreadyExists, "guest log installed twice")
            })?;
        let counter = counter.cast::<AtomicU64>();
        FAILURE.store(failure.0, Ordering::Release);
        std::mem::forget(failure);
        unsafe { counter.write(AtomicU64::new(1)) };
        NEXT_PRODUCER.store(counter, Ordering::Release);
        let _fd = self.0.into_raw_fd();
        start_process();
        Ok(GuestLogWriter(()))
    }
}

/// Synchronous packet writer using the runtime's trusted syscall gate.
/// Backpressure waits on a dedicated host reader, never on the Tool RPC loop.
/// Each `write` is one record; tracing-subscriber writes its complete format buffer.
pub struct GuestLogWriter(());

struct Emitter;

impl Emitter {
    fn enter() -> Self {
        let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) } as u64;
        let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as u64;
        let identity = (pid << 32) | tid;
        loop {
            let owner = EMITTER.load(Ordering::Acquire);
            if owner == identity {
                fail();
            }
            if (owner == 0 || owner >> 32 != pid)
                && EMITTER
                    .compare_exchange(owner, identity, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            {
                return Self;
            }
            core::hint::spin_loop();
        }
    }
}

impl Drop for Emitter {
    fn drop(&mut self) {
        EMITTER.store(0, Ordering::Release);
    }
}

impl Write for GuestLogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let _emitter = Emitter::enter();
        start_process_locked();
        let sequence = next_sequence();
        send(BEGIN, sequence, 0, bytes.len() as u64, &[]);
        for (index, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
            send(
                DATA,
                sequence,
                (index * CHUNK_BYTES) as u64,
                bytes.len() as u64,
                chunk,
            );
        }
        send(END, sequence, bytes.len() as u64, bytes.len() as u64, &[]);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn fail() -> ! {
    let failure = FAILURE.load(Ordering::Acquire);
    if !failure.is_null() {
        unsafe { &*failure }.store(1, Ordering::Release);
    }
    unsafe {
        raw_syscall6(libc::SYS_exit_group, [127, 0, 0, 0, 0, 0]);
    }
    loop {
        core::hint::spin_loop();
    }
}

fn next_sequence() -> u64 {
    let sequence = LOG_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    if sequence == u64::MAX {
        fail();
    }
    sequence
}

fn header(
    kind: u8,
    pid: i32,
    producer: u64,
    sequence: u64,
    offset: u64,
    length: u64,
) -> [u8; HEADER_BYTES] {
    let mut header = [0; HEADER_BYTES];
    header[..4].copy_from_slice(b"LGR1");
    header[4] = kind;
    header[5..9].copy_from_slice(&pid.to_le_bytes());
    header[9..17].copy_from_slice(&producer.to_le_bytes());
    header[17..25].copy_from_slice(&sequence.to_le_bytes());
    header[25..33].copy_from_slice(&offset.to_le_bytes());
    header[33..41].copy_from_slice(&length.to_le_bytes());
    header
}

fn send(kind: u8, sequence: u64, offset: u64, length: u64, bytes: &[u8]) {
    let fd = LOG_FD.load(Ordering::Acquire);
    if fd < 0 {
        fail();
    }
    let header = header(
        kind,
        LOG_PID.load(Ordering::Acquire),
        LOG_PRODUCER.load(Ordering::Acquire),
        sequence,
        offset,
        length,
    );
    let mut vectors = [
        libc::iovec {
            iov_base: header.as_ptr().cast_mut().cast(),
            iov_len: header.len(),
        },
        libc::iovec {
            iov_base: bytes.as_ptr().cast_mut().cast(),
            iov_len: bytes.len(),
        },
    ];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = vectors.as_mut_ptr();
    message.msg_iovlen = vectors.len();
    loop {
        let written = unsafe {
            raw_syscall6(
                libc::SYS_sendmsg,
                [
                    fd as u64,
                    (&raw const message) as u64,
                    libc::MSG_NOSIGNAL as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        if written == -i64::from(libc::EINTR) {
            continue;
        }
        if written != (header.len() + bytes.len()) as i64 {
            fail();
        }
        break;
    }
}

pub(crate) fn start_process() {
    if LOG_FD.load(Ordering::Acquire) < 0 {
        return;
    }
    let _emitter = Emitter::enter();
    start_process_locked();
}

fn start_process_locked() {
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) } as i32;
    if LOG_PID.load(Ordering::Acquire) != pid {
        let parent = LOG_PRODUCER.load(Ordering::Acquire);
        let counter = NEXT_PRODUCER.load(Ordering::Acquire);
        if counter.is_null() {
            fail();
        }
        let producer = unsafe { &*counter }.fetch_add(1, Ordering::Relaxed);
        if producer == 0 || producer == u64::MAX {
            fail();
        }
        LOG_PRODUCER.store(producer, Ordering::Release);
        LOG_SEQUENCE.store(1, Ordering::Release);
        LOG_PID.store(pid, Ordering::Release);
        send(START, 0, 0, parent, &[]);
    }
}

pub(crate) fn fork_result(result: i64) {
    if LOG_FD.load(Ordering::Acquire) < 0 {
        return;
    }
    if result == 0 {
        start_process();
    } else if result > 0 {
        let _emitter = Emitter::enter();
        send(EXPECT_CHILD, next_sequence(), result as u64, 0, &[]);
    }
}

pub(crate) fn finish() {
    if LOG_FD.load(Ordering::Acquire) >= 0 {
        let _emitter = Emitter::enter();
        start_process_locked();
        send(FINISH, next_sequence(), 0, 0, &[]);
    }
}

pub(crate) fn channel_pair() -> io::Result<(UnixStream, UnixStream)> {
    let mut fds = [-1; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let (host, guest) = unsafe {
        (
            UnixStream::from_raw_fd(fds[0]),
            UnixStream::from_raw_fd(fds[1]),
        )
    };
    let enabled = 1_i32;
    for stream in [&host, &guest] {
        if unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                (&raw const enabled).cast(),
                std::mem::size_of_val(&enabled) as libc::socklen_t,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let fd = unsafe {
        libc::memfd_create(
            c"liteinst-log-failure".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let failure = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::ftruncate(fd, 8) } != 0
        || unsafe {
            libc::fcntl(
                fd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_SEAL | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK,
            )
        } != 0
    {
        return Err(io::Error::last_os_error());
    }
    send_failure_bootstrap(host.as_raw_fd(), failure.as_raw_fd())?;
    send_failure_bootstrap(guest.as_raw_fd(), failure.as_raw_fd())?;
    Ok((host, guest))
}

pub(crate) fn identity(fd: i32) -> io::Result<[u8; IDENTITY_BYTES]> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let mut kind = 0_i32;
    let mut size = std::mem::size_of_val(&kind) as libc::socklen_t;
    if unsafe { libc::fstat(fd, &mut stat) } != 0
        || unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &mut size,
            )
        } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if kind != libc::SOCK_SEQPACKET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guest log is not a packet socket",
        ));
    }
    let mut result = [0; IDENTITY_BYTES];
    result[..4].copy_from_slice(&fd.to_le_bytes());
    result[4..12].copy_from_slice(&stat.st_dev.to_le_bytes());
    result[12..].copy_from_slice(&stat.st_ino.to_le_bytes());
    Ok(result)
}

pub(crate) unsafe fn from_identity(bytes: &[u8]) -> io::Result<GuestLog> {
    if bytes.len() != IDENTITY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid guest log identity length",
        ));
    }
    let fd = i32::from_le_bytes(bytes[..4].try_into().unwrap());
    if fd <= libc::STDERR_FILENO || identity(fd)? != bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guest log identity mismatch",
        ));
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(GuestLog(unsafe { OwnedFd::from_raw_fd(fd) }))
}

pub(crate) fn collect(
    stream: UnixStream,
    limit: usize,
    exited: &AtomicBool,
    consumer: RecordConsumer,
) -> CapturedGuestLog {
    collect_with_consumer(stream, limit, exited, Duration::from_secs(30), consumer)
}

#[cfg(test)]
fn collect_with_drain_timeout(
    stream: UnixStream,
    limit: usize,
    exited: &AtomicBool,
    drain: Duration,
) -> CapturedGuestLog {
    collect_with_consumer(stream, limit, exited, drain, Box::new(|_| Ok(())))
}

#[derive(Default)]
struct Producer {
    pid: i32,
    next: u64,
    finished: bool,
    record: Option<(usize, Vec<u8>)>,
}

#[derive(Default)]
struct Decoder {
    producers: BTreeMap<u64, Producer>,
    children: BTreeMap<(u64, i32), (usize, usize)>,
    declared_children: usize,
    reserved: usize,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl Decoder {
    fn complete(&self) -> bool {
        !self.producers.is_empty()
            && self.producers.values().all(|producer| producer.finished)
            && self
                .children
                .values()
                .all(|(started, declared)| started == declared)
    }

    fn accept(
        &mut self,
        packet: &[u8],
        limit: usize,
        output: &mut Vec<u8>,
        consumer: &mut RecordConsumer,
    ) -> io::Result<()> {
        if packet.len() < HEADER_BYTES
            || packet.len() > HEADER_BYTES + CHUNK_BYTES
            || &packet[..4] != b"LGR1"
        {
            return Err(invalid("invalid guest log frame"));
        }
        let kind = packet[4];
        let pid = i32::from_le_bytes(packet[5..9].try_into().unwrap());
        let producer = u64::from_le_bytes(packet[9..17].try_into().unwrap());
        let sequence = u64::from_le_bytes(packet[17..25].try_into().unwrap());
        let offset = u64::from_le_bytes(packet[25..33].try_into().unwrap());
        let length = u64::from_le_bytes(packet[33..41].try_into().unwrap());
        let bytes = &packet[HEADER_BYTES..];
        if pid <= 0 || producer == 0 || (kind != DATA && !bytes.is_empty()) {
            return Err(invalid("invalid guest log frame identity or payload"));
        }
        if kind == START {
            if sequence != 0
                || offset != 0
                || self.producers.contains_key(&producer)
                || self.producers.len() >= MAX_PRODUCERS
                || (length == 0 && (producer != 1 || !self.producers.is_empty()))
                || (length != 0 && (!self.producers.contains_key(&length) || producer <= length))
                || self
                    .producers
                    .values()
                    .any(|state| state.pid == pid && !state.finished)
            {
                return Err(invalid("invalid guest log producer incarnation"));
            }
            if length != 0 {
                if self.children.len() >= MAX_PRODUCERS {
                    return Err(invalid("guest log child limit exceeded"));
                }
                self.children.entry((length, pid)).or_default().0 += 1;
            }
            self.producers.insert(
                producer,
                Producer {
                    pid,
                    next: 1,
                    ..Producer::default()
                },
            );
            return Ok(());
        }
        let state = self
            .producers
            .get_mut(&producer)
            .ok_or_else(|| invalid("unknown guest log producer incarnation"))?;
        if state.pid != pid || state.finished || sequence != state.next || sequence == u64::MAX {
            return Err(invalid("invalid guest log sequence or completion"));
        }
        match kind {
            BEGIN if state.record.is_none() && offset == 0 => {
                let length = usize::try_from(length)
                    .map_err(|_| invalid("guest log record length overflow"))?;
                if length > limit.saturating_sub(self.reserved) {
                    return Err(io::Error::new(
                        io::ErrorKind::FileTooLarge,
                        "guest log byte limit exceeded; truncated",
                    ));
                }
                self.reserved += length;
                state.record = Some((length, Vec::new()));
            }
            DATA => {
                let (expected, record) = state
                    .record
                    .as_mut()
                    .ok_or_else(|| invalid("guest log data without record start"))?;
                if length != *expected as u64
                    || offset != record.len() as u64
                    || bytes.is_empty()
                    || bytes.len() > expected.saturating_sub(record.len())
                {
                    return Err(invalid("conflicting guest log record chunk"));
                }
                record.extend_from_slice(bytes);
            }
            END => {
                let (expected, record) = state
                    .record
                    .take()
                    .ok_or_else(|| invalid("guest log end without record start"))?;
                if length != expected as u64 || offset != length || record.len() != expected {
                    return Err(invalid("truncated guest log record"));
                }
                consumer(&GuestLogRecord {
                    pid,
                    producer,
                    sequence,
                    bytes: &record,
                })?;
                output.extend_from_slice(&record);
                state.next += 1;
            }
            EXPECT_CHILD
                if state.record.is_none()
                    && length == 0
                    && offset > 0
                    && offset <= i32::MAX as u64 =>
            {
                if self.declared_children >= MAX_PRODUCERS - 1
                    || self.children.len() >= MAX_PRODUCERS
                {
                    return Err(invalid("guest log child limit exceeded"));
                }
                self.children
                    .entry((producer, offset as i32))
                    .or_default()
                    .1 += 1;
                self.declared_children += 1;
                state.next += 1;
            }
            FINISH if state.record.is_none() && length == 0 && offset == 0 => {
                state.finished = true;
                state.next += 1;
            }
            _ => return Err(invalid("invalid guest log record lifecycle")),
        }
        Ok(())
    }
}

fn collect_with_consumer(
    stream: UnixStream,
    limit: usize,
    exited: &AtomicBool,
    drain: Duration,
    mut consumer: RecordConsumer,
) -> CapturedGuestLog {
    let mut result = CapturedGuestLog {
        bytes: Vec::new(),
        error: None,
    };
    let mut decoder = Decoder::default();
    let outcome = (|| -> io::Result<()> {
        let failure = FailureState::receive(stream.as_raw_fd())?;
        let mut deadline = None;
        loop {
            if exited.load(Ordering::Acquire) && decoder.complete() {
                if deadline.is_none() {
                    deadline = Some(Instant::now() + drain);
                }
            } else {
                deadline = None;
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "guest log post-exit drain timed out",
                ));
            }
            let mut descriptor = libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut descriptor, 1, 10) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if ready == 0 {
                if failure.failed() {
                    return Err(invalid("guest log producer failed"));
                }
                continue;
            }
            let mut packet = [0; CHUNK_BYTES + HEADER_BYTES];
            let mut vector = libc::iovec {
                iov_base: packet.as_mut_ptr().cast(),
                iov_len: packet.len(),
            };
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = &raw mut vector;
            message.msg_iovlen = 1;
            let size = unsafe {
                libc::recvmsg(
                    stream.as_raw_fd(),
                    &raw mut message,
                    libc::MSG_DONTWAIT | libc::MSG_TRUNC,
                )
            };
            if size < 0 {
                let error = io::Error::last_os_error();
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) {
                    continue;
                }
                return Err(error);
            }
            let packet_credential_was_discarded = message.msg_flags & libc::MSG_CTRUNC != 0;
            if size == 0 {
                if packet_credential_was_discarded {
                    return Err(invalid("empty guest log packet is not EOF"));
                }
                if failure.failed() {
                    return Err(invalid("guest log producer failed"));
                }
                if !decoder.complete() {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "guest log missing record or process completion",
                    ));
                }
                return Ok(());
            }
            if !packet_credential_was_discarded {
                return Err(invalid("guest log packet credential missing"));
            }
            if size as usize > packet.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid guest log packet size",
                ));
            }
            decoder.accept(
                &packet[..size as usize],
                limit,
                &mut result.bytes,
                &mut consumer,
            )?;
        }
    })();
    result.error = outcome.err().map(|error| error.to_string());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn nested_writer(_signal: i32) {
        let mut nested = GuestLogWriter(());
        let _ = nested.write(b"nested\n");
    }

    fn interrupted_finish() -> ! {
        let _emitter = Emitter::enter();
        start_process_locked();
        send(FINISH, next_sequence(), 0, 0, &[]);
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = nested_writer as *const () as usize;
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()) },
            0
        );
        assert_eq!(unsafe { libc::raise(libc::SIGUSR1) }, 0);
        panic!("terminal reentry unexpectedly returned");
    }

    #[test]
    fn terminal_failure_survives_finish_and_normal_parent_exit() {
        use std::os::unix::process::CommandExt;

        if let Ok(fd) = std::env::var("LITEINST_LOG_TERMINAL_FD") {
            let mode = std::env::var("LITEINST_LOG_TERMINAL_MODE").unwrap();
            let mut writer =
                unsafe { GuestLog(OwnedFd::from_raw_fd(fd.parse().unwrap())).install() }.unwrap();
            if mode == "descendant" {
                let child = unsafe { libc::fork() };
                assert!(child >= 0);
                fork_result(child.into());
                if child == 0 {
                    writer.write_all(b"child prefix\n").unwrap();
                    interrupted_finish();
                }
                let mut status = 0;
                assert_eq!(unsafe { libc::waitpid(child, &raw mut status, 0) }, child);
                assert!(libc::WIFEXITED(status));
                assert_eq!(libc::WEXITSTATUS(status), 127);
            }
            writer.write_all(b"complete prefix\n").unwrap();
            if mode == "root" {
                interrupted_finish();
            }
            finish();
            unsafe { libc::_exit(if mode == "normal127" { 127 } else { 0 }) };
        }
        for mode in ["normal", "normal127", "root", "descendant"] {
            let (sender, receiver) = channel_pair().unwrap();
            let fd = sender.as_raw_fd();
            let mut command = std::process::Command::new("/usr/bin/timeout");
            command
                .args(["--signal=KILL", "3"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "guest_log::tests::terminal_failure_survives_finish_and_normal_parent_exit",
                ])
                .env("LITEINST_LOG_TERMINAL_FD", fd.to_string())
                .env("LITEINST_LOG_TERMINAL_MODE", mode);
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn().unwrap();
            drop(sender);
            let status = child.wait().unwrap();
            assert_eq!(
                status.code(),
                Some(if matches!(mode, "root" | "normal127") {
                    127
                } else {
                    0
                }),
                "{mode}"
            );
            let log = capture(receiver, 1024);
            let expected = if mode == "descendant" {
                b"child prefix\ncomplete prefix\n".as_slice()
            } else {
                b"complete prefix\n"
            };
            assert_eq!(log.bytes, expected, "{mode}");
            assert_eq!(
                log.error.as_deref(),
                if matches!(mode, "root" | "descendant") {
                    Some("guest log producer failed")
                } else {
                    None
                },
                "{mode}"
            );
        }
    }

    #[test]
    fn signal_reentry_fails_closed_with_incomplete_capture() {
        const INHERITED_FD: &str = "LITEINST_LOG_SIGNAL_TEST_FD";
        if let Some(fd) = std::env::var_os(INHERITED_FD) {
            let fd = fd.to_str().unwrap().parse::<i32>().unwrap();
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = nested_writer as *const () as usize;
            assert_eq!(
                unsafe { libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()) },
                0
            );
            let mut writer = unsafe { GuestLog(OwnedFd::from_raw_fd(fd)).install() }.unwrap();
            let target = unsafe { libc::pthread_self() };
            std::thread::spawn(move || {
                while EMITTER.load(Ordering::Acquire) == 0 {
                    std::thread::yield_now();
                }
                assert_eq!(unsafe { libc::pthread_kill(target, libc::SIGUSR1) }, 0);
            });
            writer.write_all(&vec![b'x'; 200000]).unwrap();
            panic!("nested writer unexpectedly returned");
        }
        use std::os::unix::process::CommandExt;

        let (sender, receiver) = channel_pair().unwrap();
        let size = 8192_i32;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    sender.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&raw const size).cast(),
                    std::mem::size_of_val(&size) as libc::socklen_t,
                )
            },
            0
        );
        let fd = sender.as_raw_fd();
        let mut command = std::process::Command::new("/usr/bin/timeout");
        command
            .args(["--signal=KILL", "3"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "guest_log::tests::signal_reentry_fails_closed_with_incomplete_capture",
            ])
            .env(INHERITED_FD, fd.to_string());
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        drop(sender);
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(127), "{status}");
        let log = capture(receiver, 200001);
        assert!(
            log.error
                .as_deref()
                .is_some_and(|error| error.contains("guest log producer failed")),
            "{:?}",
            log.error
        );
        assert!(log.bytes.is_empty());
    }

    #[test]
    fn zero_length_packet_must_not_hide_trailing_invalid_frame() {
        for empty_packet in [false, true] {
            let (sender, receiver) = channel_pair().unwrap();
            packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
            packet(&sender, header(FINISH, 1, 1, 1, 0, 0), &[]);
            if empty_packet {
                assert_eq!(
                    unsafe {
                        libc::send(sender.as_raw_fd(), std::ptr::null(), 0, libc::MSG_NOSIGNAL)
                    },
                    0
                );
            }
            packet(&sender, header(FINISH, 1, 1, 1, 0, 0), &[]);
            drop(sender);
            let log = capture(receiver, 32);
            assert!(log.error.is_some(), "empty_packet={empty_packet}");
        }
    }

    #[test]
    fn empty_packet_is_invalid_with_open_or_closed_peer() {
        for close in [false, true] {
            let (sender, receiver) = channel_pair().unwrap();
            packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
            packet(&sender, header(FINISH, 1, 1, 1, 0, 0), &[]);
            assert_eq!(
                unsafe { libc::send(sender.as_raw_fd(), std::ptr::null(), 0, libc::MSG_NOSIGNAL) },
                0
            );
            let retained_sender = if close {
                drop(sender);
                None
            } else {
                Some(sender)
            };
            let log = capture(receiver, 32);
            assert_eq!(
                log.error.as_deref(),
                Some("empty guest log packet is not EOF")
            );
            drop(retained_sender);
        }
        let (sender, receiver) = channel_pair().unwrap();
        packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
        packet(&sender, header(FINISH, 1, 1, 1, 0, 0), &[]);
        drop(sender);
        assert!(capture(receiver, 32).error.is_none());
    }

    fn packet(sender: &UnixStream, header: [u8; HEADER_BYTES], bytes: &[u8]) {
        let mut bootstrap = [0; 4];
        let size = unsafe {
            libc::recv(
                sender.as_raw_fd(),
                bootstrap.as_mut_ptr().cast(),
                bootstrap.len(),
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if size == 4 && bootstrap == FAILURE_BOOTSTRAP {
            drop(FailureState::receive(sender.as_raw_fd()).unwrap());
        }
        let mut payload = header.to_vec();
        payload.extend_from_slice(bytes);
        assert_eq!(
            unsafe {
                libc::send(
                    sender.as_raw_fd(),
                    payload.as_ptr().cast(),
                    payload.len(),
                    libc::MSG_NOSIGNAL,
                )
            },
            payload.len() as isize
        );
    }

    fn record(sender: &UnixStream, pid: i32, producer: u64, sequence: u64, bytes: &[u8]) {
        let length = bytes.len() as u64;
        packet(
            sender,
            header(BEGIN, pid, producer, sequence, 0, length),
            &[],
        );
        for (index, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
            packet(
                sender,
                header(
                    DATA,
                    pid,
                    producer,
                    sequence,
                    (index * CHUNK_BYTES) as u64,
                    length,
                ),
                chunk,
            );
        }
        packet(
            sender,
            header(END, pid, producer, sequence, length, length),
            &[],
        );
    }

    fn capture(receiver: UnixStream, limit: usize) -> CapturedGuestLog {
        collect(
            receiver,
            limit,
            &AtomicBool::new(false),
            Box::new(|_| Ok(())),
        )
    }

    #[test]
    fn complete_log_preserves_forked_process_bytes() {
        let (sender, receiver) = channel_pair().unwrap();
        packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
        packet(&sender, header(START, 2, 2, 0, 0, 1), &[]);
        record(&sender, 2, 2, 1, b"child\n");
        packet(&sender, header(FINISH, 2, 2, 2, 0, 0), &[]);
        packet(&sender, header(EXPECT_CHILD, 1, 1, 1, 2, 0), &[]);
        record(&sender, 1, 1, 2, b"parent\n");
        packet(&sender, header(FINISH, 1, 1, 3, 0, 0), &[]);
        drop(sender);
        let log = capture(receiver, 32);
        assert_eq!(log.bytes, b"child\nparent\n");
        assert!(log.error.is_none());
    }

    #[test]
    fn incomplete_and_truncated_logs_are_not_complete() {
        for limit in [2, 100] {
            let (sender, receiver) = channel_pair().unwrap();
            packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
            record(&sender, 1, 1, 1, b"abc\n");
            drop(sender);
            let log = capture(receiver, limit);
            assert_eq!(log.bytes, if limit < 4 { b"".as_slice() } else { b"abc\n" });
            assert!(log.error.is_some());
        }
    }

    #[test]
    fn quiet_live_guest_has_no_receive_deadline() {
        let (sender, receiver) = channel_pair().unwrap();
        let reader = std::thread::spawn(move || {
            collect_with_drain_timeout(
                receiver,
                32,
                &AtomicBool::new(false),
                Duration::from_millis(10),
            )
        });
        std::thread::sleep(Duration::from_millis(50));
        packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
        packet(&sender, header(FINISH, 1, 1, 1, 0, 0), &[]);
        drop(sender);
        assert!(reader.join().unwrap().error.is_none());
        let (sender, receiver) = channel_pair().unwrap();
        packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
        packet(&sender, header(FINISH, 1, 1, 1, 0, 0), &[]);
        assert!(
            collect_with_drain_timeout(
                receiver,
                32,
                &AtomicBool::new(true),
                Duration::from_millis(10)
            )
            .error
            .unwrap()
            .contains("post-exit")
        );
    }

    #[test]
    fn quiet_descendant_outlives_root_without_drain_deadline() {
        let (sender, receiver) = channel_pair().unwrap();
        packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
        packet(&sender, header(EXPECT_CHILD, 1, 1, 1, 2, 0), &[]);
        packet(&sender, header(FINISH, 1, 1, 2, 0, 0), &[]);
        let reader = std::thread::spawn(move || {
            collect_with_drain_timeout(
                receiver,
                32,
                &AtomicBool::new(true),
                Duration::from_millis(10),
            )
        });
        std::thread::sleep(Duration::from_millis(50));
        packet(&sender, header(START, 2, 2, 0, 0, 1), &[]);
        record(&sender, 2, 2, 1, b"after parent exit\n");
        packet(&sender, header(FINISH, 2, 2, 2, 0, 0), &[]);
        drop(sender);
        let log = reader.join().unwrap();
        assert!(log.error.is_none(), "{:?}", log.error);
        assert_eq!(log.bytes, b"after parent exit\n");
    }

    #[test]
    fn rejects_changed_socket_identity() {
        let (sender, _receiver) = channel_pair().unwrap();
        let mut bytes = identity(sender.as_raw_fd()).unwrap();
        bytes[12] ^= 1;
        assert!(unsafe { from_identity(&bytes) }.is_err());
    }

    #[test]
    fn multiline_records_survive_interleaved_chunks() {
        let (sender, receiver) = channel_pair().unwrap();
        let bytes = b"first\nsecond\n".repeat(800);
        let length = bytes.len() as u64;
        packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
        packet(&sender, header(START, 2, 2, 0, 0, 1), &[]);
        packet(&sender, header(EXPECT_CHILD, 1, 1, 1, 2, 0), &[]);
        packet(&sender, header(BEGIN, 1, 1, 2, 0, length), &[]);
        packet(
            &sender,
            header(DATA, 1, 1, 2, 0, length),
            &bytes[..CHUNK_BYTES],
        );
        record(&sender, 2, 2, 1, b"child\nmultiline\n");
        for (index, chunk) in bytes[CHUNK_BYTES..].chunks(CHUNK_BYTES).enumerate() {
            packet(
                &sender,
                header(DATA, 1, 1, 2, ((index + 1) * CHUNK_BYTES) as u64, length),
                chunk,
            );
        }
        packet(&sender, header(END, 1, 1, 2, length, length), &[]);
        packet(&sender, header(FINISH, 1, 1, 3, 0, 0), &[]);
        packet(&sender, header(FINISH, 2, 2, 2, 0, 0), &[]);
        drop(sender);
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let consumer = observed.clone();
        let log = collect(
            receiver,
            20000,
            &AtomicBool::new(false),
            Box::new(move |record| {
                consumer.lock().unwrap().push((
                    record.producer,
                    record.sequence,
                    record.bytes.to_vec(),
                ));
                Ok(())
            }),
        );
        assert!(log.error.is_none(), "{:?}", log.error);
        assert_eq!(
            *observed.lock().unwrap(),
            vec![
                (2, 1, b"child\nmultiline\n".to_vec()),
                (1, 2, bytes.clone())
            ]
        );
        assert_eq!(
            log.bytes,
            [b"child\nmultiline\n".as_slice(), &bytes].concat()
        );
    }

    #[test]
    fn malformed_frames_never_deliver_partial_records() {
        for bad in [
            header(BEGIN, 1, 1, 2, 0, 4),
            header(BEGIN, 1, 9, 1, 0, 4),
            header(BEGIN, 2, 1, 1, 0, 4),
            header(END, 1, 1, 1, 4, 4),
            header(FINISH, 1, 1, 1, 0, 0),
            header(START, 1, 1, 0, 0, 0),
        ] {
            let mut decoder = Decoder::default();
            let mut output = Vec::new();
            let mut consumer: RecordConsumer = Box::new(|_| panic!("partial record delivered"));
            decoder
                .accept(
                    &header(START, 1, 1, 0, 0, 0),
                    64,
                    &mut output,
                    &mut consumer,
                )
                .unwrap();
            decoder
                .accept(
                    &header(BEGIN, 1, 1, 1, 0, 4),
                    64,
                    &mut output,
                    &mut consumer,
                )
                .unwrap();
            assert!(
                decoder
                    .accept(&bad, 64, &mut output, &mut consumer)
                    .is_err()
            );
            assert!(output.is_empty());
        }
        for (offset, length) in [(1, 4), (0, 5), (0, 0)] {
            let (sender, receiver) = channel_pair().unwrap();
            packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
            packet(&sender, header(BEGIN, 1, 1, 1, 0, 4), &[]);
            packet(&sender, header(DATA, 1, 1, 1, offset, length), b"abcd");
            drop(sender);
            let log = capture(receiver, 64);
            assert!(log.error.is_some());
            assert!(log.bytes.is_empty());
        }
    }

    #[test]
    fn eof_or_missing_child_invalidates_committed_prefix() {
        for missing in [BEGIN, DATA, END, EXPECT_CHILD] {
            let (sender, receiver) = channel_pair().unwrap();
            packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
            record(&sender, 1, 1, 1, b"prefix\n");
            if missing == EXPECT_CHILD {
                packet(&sender, header(EXPECT_CHILD, 1, 1, 2, 2, 0), &[]);
                packet(&sender, header(FINISH, 1, 1, 3, 0, 0), &[]);
            } else {
                packet(&sender, header(BEGIN, 1, 1, 2, 0, 4), &[]);
                if missing != BEGIN {
                    packet(&sender, header(DATA, 1, 1, 2, 0, 4), b"data");
                }
                if missing == END {
                    packet(&sender, header(END, 1, 1, 2, 4, 4), &[]);
                }
            }
            drop(sender);
            let log = capture(receiver, 64);
            assert!(log.error.is_some());
            assert_eq!(
                log.bytes,
                if missing == END {
                    b"prefix\ndata".as_slice()
                } else {
                    b"prefix\n"
                }
            );
        }
    }

    #[test]
    fn duplicate_records_and_consumer_errors_invalidate_capture() {
        for duplicate in [false, true] {
            let (sender, receiver) = channel_pair().unwrap();
            packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
            record(&sender, 1, 1, 1, b"record\n");
            if duplicate {
                record(&sender, 1, 1, 1, b"record\n");
            }
            packet(&sender, header(FINISH, 1, 1, 2, 0, 0), &[]);
            drop(sender);
            let log = collect(
                receiver,
                64,
                &AtomicBool::new(false),
                Box::new(move |_| {
                    if duplicate {
                        Ok(())
                    } else {
                        Err(io::Error::other("sink refused record"))
                    }
                }),
            );
            assert!(log.error.is_some());
            assert_eq!(
                log.bytes,
                if duplicate {
                    b"record\n".as_slice()
                } else {
                    b""
                }
            );
        }
    }

    #[test]
    fn reused_pid_requires_a_new_incarnation() {
        let (sender, receiver) = channel_pair().unwrap();
        packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
        for (producer, sequence) in [(2, 1), (3, 2)] {
            packet(&sender, header(EXPECT_CHILD, 1, 1, sequence, 2, 0), &[]);
            packet(&sender, header(START, 2, producer, 0, 0, 1), &[]);
            record(&sender, 2, producer, 1, b"child");
            packet(&sender, header(FINISH, 2, producer, 2, 0, 0), &[]);
        }
        packet(&sender, header(FINISH, 1, 1, 3, 0, 0), &[]);
        drop(sender);
        let log = capture(receiver, 64);
        assert!(log.error.is_none(), "{:?}", log.error);
        assert_eq!(log.bytes, b"childchild");
    }

    #[test]
    fn actual_writer_backpressure_and_inherited_child_completion() {
        if std::env::var_os("LITEINST_LOG_FORK_TEST").is_some() {
            let (sender, receiver) = channel_pair().unwrap();
            let exited = std::sync::Arc::new(AtomicBool::new(false));
            let reader_exited = exited.clone();
            let reader = std::thread::spawn(move || {
                collect_with_drain_timeout(
                    receiver,
                    1024 * 1024,
                    &reader_exited,
                    Duration::from_millis(10),
                )
            });
            let mut writer = unsafe { GuestLog(sender.into()).install() }.unwrap();
            let bytes = b"large\nrecord\n".repeat(30000);
            writer.write_all(&bytes).unwrap();
            writer.write_all(b"").unwrap();
            let mut children = Vec::new();
            for _ in 0..3 {
                let child = unsafe { libc::fork() };
                assert!(child >= 0);
                fork_result(i64::from(child));
                if child == 0 {
                    unsafe { libc::usleep(50000) };
                    writer.write_all(b"child\n").unwrap();
                    finish();
                    unsafe { libc::_exit(0) };
                }
                children.push(child);
            }
            finish();
            exited.store(true, Ordering::Release);
            for child in children {
                let mut status = 0;
                assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
                assert_eq!(status, 0);
            }
            unsafe { libc::close(LOG_FD.swap(-1, Ordering::AcqRel)) };
            let log = reader.join().unwrap();
            assert!(log.error.is_none(), "{:?}", log.error);
            assert_eq!(
                log.bytes,
                [bytes.as_slice(), b"child\nchild\nchild\n"].concat()
            );
            return;
        }
        let status = std::process::Command::new("/usr/bin/timeout")
            .args(["--signal=KILL", "30"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "guest_log::tests::actual_writer_backpressure_and_inherited_child_completion",
            ])
            .env("LITEINST_LOG_FORK_TEST", "1")
            .status()
            .unwrap();
        assert!(status.success(), "{status}");
    }

    #[test]
    fn fork_control_cannot_overtake_a_blocked_record() {
        if std::env::var_os("LITEINST_LOG_CONCURRENT_FORK_TEST").is_some() {
            let (sender, receiver) = channel_pair().unwrap();
            let size = 8192_i32;
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        sender.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_SNDBUF,
                        (&raw const size).cast(),
                        std::mem::size_of_val(&size) as libc::socklen_t,
                    )
                },
                0
            );
            let mut writer = unsafe { GuestLog(sender.into()).install() }.unwrap();
            let writer_thread =
                std::thread::spawn(move || writer.write_all(&vec![b'x'; 100000]).unwrap());
            while EMITTER.load(Ordering::Acquire) == 0 {
                std::thread::yield_now();
            }
            let child = unsafe { libc::fork() };
            assert!(child >= 0);
            if child == 0 {
                fork_result(0);
                finish();
                unsafe { libc::_exit(0) };
            }
            let reader = std::thread::spawn(move || capture(receiver, 200000));
            fork_result(i64::from(child));
            writer_thread.join().unwrap();
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
            assert_eq!(status, 0);
            finish();
            unsafe { libc::close(LOG_FD.swap(-1, Ordering::AcqRel)) };
            let log = reader.join().unwrap();
            assert!(log.error.is_none(), "{:?}", log.error);
            assert_eq!(log.bytes.len(), 100000);
            assert!(log.bytes.iter().all(|byte| *byte == b'x'));
            return;
        }
        let status = std::process::Command::new("/usr/bin/timeout")
            .args(["--signal=KILL", "30"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "guest_log::tests::fork_control_cannot_overtake_a_blocked_record",
            ])
            .env("LITEINST_LOG_CONCURRENT_FORK_TEST", "1")
            .status()
            .unwrap();
        assert!(status.success(), "{status}");
    }

    #[test]
    fn empty_records_and_terminal_sequence_are_checked() {
        for final_sequence in [1, 2, 3] {
            let (sender, receiver) = channel_pair().unwrap();
            packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
            record(&sender, 1, 1, 1, b"");
            packet(&sender, header(FINISH, 1, 1, final_sequence, 0, 0), &[]);
            drop(sender);
            let log = capture(receiver, 0);
            assert_eq!(log.error.is_none(), final_sequence == 2);
            assert!(log.bytes.is_empty());
        }
    }

    #[test]
    fn malformed_header_and_metadata_bounds_are_checked() {
        for bytes in [
            vec![],
            vec![0; HEADER_BYTES - 1],
            vec![0; HEADER_BYTES],
            vec![0; HEADER_BYTES + CHUNK_BYTES + 1],
        ] {
            let mut decoder = Decoder::default();
            let mut consumer: RecordConsumer = Box::new(|_| Ok(()));
            assert!(
                decoder
                    .accept(&bytes, 16, &mut Vec::new(), &mut consumer)
                    .is_err()
            );
        }
        let mut decoder = Decoder::default();
        let mut output = Vec::new();
        let mut consumer: RecordConsumer = Box::new(|_| Ok(()));
        decoder
            .accept(&header(START, 1, 1, 0, 0, 0), 0, &mut output, &mut consumer)
            .unwrap();
        for sequence in 1..MAX_PRODUCERS as u64 {
            decoder
                .accept(
                    &header(EXPECT_CHILD, 1, 1, sequence, 2, 0),
                    0,
                    &mut output,
                    &mut consumer,
                )
                .unwrap();
        }
        assert!(
            decoder
                .accept(
                    &header(EXPECT_CHILD, 1, 1, MAX_PRODUCERS as u64, 2, 0),
                    0,
                    &mut output,
                    &mut consumer
                )
                .is_err()
        );
        assert!(!decoder.complete());
    }

    #[test]
    fn broken_transport_exits_even_with_closed_stderr() {
        if std::env::var_os("LITEINST_LOG_BROKEN_TEST").is_some() {
            let (sender, receiver) = channel_pair().unwrap();
            drop(FailureState::receive(receiver.as_raw_fd()).unwrap());
            drop(receiver);
            unsafe {
                libc::close(libc::STDERR_FILENO);
            }
            let _writer = unsafe { GuestLog(sender.into()).install() }.unwrap();
            panic!("broken log accepted");
        }
        let status = std::process::Command::new("/usr/bin/timeout")
            .args(["--signal=KILL", "30"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "guest_log::tests::broken_transport_exits_even_with_closed_stderr",
            ])
            .env("LITEINST_LOG_BROKEN_TEST", "1")
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(127));
    }

    #[test]
    fn failure_latch_invalidates_quiet_live_peer() {
        let (sender, receiver) = channel_pair().unwrap();
        let failure = FailureState::receive(sender.as_raw_fd()).unwrap();
        packet(&sender, header(START, 1, 1, 0, 0, 0), &[]);
        record(&sender, 1, 1, 1, b"complete prefix\n");
        packet(&sender, header(FINISH, 1, 1, 2, 0, 0), &[]);
        unsafe { &*failure.0 }.store(1, Ordering::Release);
        let log = capture(receiver, 1024);
        assert_eq!(log.bytes, b"complete prefix\n");
        assert_eq!(log.error.as_deref(), Some("guest log producer failed"));
        drop(sender);
    }

    #[test]
    fn failure_bootstrap_rejects_unsealed_or_wrong_size_mapping() {
        for (size, seals) in [
            (8, 0),
            (
                16,
                libc::F_SEAL_SEAL | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK,
            ),
        ] {
            let (sender, receiver) = channel_pair().unwrap();
            drop(FailureState::receive(sender.as_raw_fd()).unwrap());
            drop(FailureState::receive(receiver.as_raw_fd()).unwrap());
            let fd = unsafe {
                libc::memfd_create(
                    c"invalid-log-failure".as_ptr(),
                    libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
                )
            };
            assert!(fd >= 0);
            let descriptor = unsafe { OwnedFd::from_raw_fd(fd) };
            assert_eq!(unsafe { libc::ftruncate(fd, size) }, 0);
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, seals) }, 0);
            send_failure_bootstrap(sender.as_raw_fd(), descriptor.as_raw_fd()).unwrap();
            let error = FailureState::receive(receiver.as_raw_fd()).err().unwrap();
            assert_eq!(error.to_string(), "invalid guest log failure mapping");
        }
    }
}
