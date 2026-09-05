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
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie_preload::trap::raw_syscall6;

pub(crate) static LOG_FD: AtomicI32 = AtomicI32::new(-1);
static LOG_PID: AtomicI32 = AtomicI32::new(0);
pub(crate) const IDENTITY_BYTES: usize = 20;
const CHUNK_BYTES: usize = 4096;
const START: u8 = 0;
const DATA: u8 = 1;
const FINISH: u8 = 2;
const EXPECT_CHILD: u8 = 3;

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
        LOG_FD
            .compare_exchange(-1, self.0.as_raw_fd(), Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                io::Error::new(io::ErrorKind::AlreadyExists, "guest log installed twice")
            })?;
        let _fd = self.0.into_raw_fd();
        start_process();
        Ok(GuestLogWriter(()))
    }
}

/// Synchronous packet writer using the runtime's trusted syscall gate.
/// Backpressure waits on a dedicated host reader, never on the Tool RPC loop.
pub struct GuestLogWriter(());

impl Write for GuestLogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        start_process();
        for chunk in bytes.chunks(CHUNK_BYTES) {
            send(DATA, LOG_PID.load(Ordering::Acquire), chunk);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn fail() -> ! {
    unsafe {
        raw_syscall6(libc::SYS_exit_group, [127, 0, 0, 0, 0, 0]);
    }
    loop {
        core::hint::spin_loop();
    }
}

fn send(kind: u8, pid: i32, bytes: &[u8]) {
    let fd = LOG_FD.load(Ordering::Acquire);
    if fd < 0 {
        fail();
    }
    let mut header = [0; 5];
    header[..4].copy_from_slice(&pid.to_le_bytes());
    header[4] = kind;
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
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) } as i32;
    if LOG_PID.load(Ordering::Acquire) != pid {
        send(START, pid, &[]);
        LOG_PID.store(pid, Ordering::Release);
    }
}

pub(crate) fn fork_result(result: i64) {
    if LOG_FD.load(Ordering::Acquire) < 0 {
        return;
    }
    if result == 0 {
        start_process();
    } else if result > 0 {
        send(EXPECT_CHILD, result as i32, &[]);
    }
}

pub(crate) fn finish() {
    if LOG_FD.load(Ordering::Acquire) >= 0 {
        start_process();
        send(FINISH, LOG_PID.load(Ordering::Acquire), &[]);
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
    Ok(unsafe {
        (
            UnixStream::from_raw_fd(fds[0]),
            UnixStream::from_raw_fd(fds[1]),
        )
    })
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

pub(crate) fn collect(stream: UnixStream, limit: usize, exited: &AtomicBool) -> CapturedGuestLog {
    collect_with_drain_timeout(stream, limit, exited, Duration::from_secs(30))
}

fn collect_with_drain_timeout(
    stream: UnixStream,
    limit: usize,
    exited: &AtomicBool,
    drain: Duration,
) -> CapturedGuestLog {
    let mut result = CapturedGuestLog {
        bytes: Vec::new(),
        error: None,
    };
    let mut states = BTreeMap::<i32, (bool, bool)>::new();
    let outcome = (|| -> io::Result<()> {
        let mut deadline = None;
        loop {
            if exited.load(Ordering::Acquire)
                && !states.is_empty()
                && states.values().all(|state| *state == (true, true))
            {
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
                continue;
            }
            let mut packet = [0; CHUNK_BYTES + 5];
            let size = unsafe {
                libc::recv(
                    stream.as_raw_fd(),
                    packet.as_mut_ptr().cast(),
                    packet.len(),
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
            if size == 0 {
                if states.is_empty() || states.values().any(|state| *state != (true, true)) {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "guest log missing process completion",
                    ));
                }
                return Ok(());
            }
            if size < 5 || size as usize > packet.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid guest log packet size",
                ));
            }
            let pid = i32::from_le_bytes(packet[..4].try_into().unwrap());
            let kind = packet[4];
            let bytes = &packet[5..size as usize];
            if pid <= 0 || (kind != DATA && !bytes.is_empty()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid guest log control packet",
                ));
            }
            let state = states.entry(pid).or_default();
            match kind {
                START if !state.0 => state.0 = true,
                EXPECT_CHILD => {}
                FINISH if state.0 && !state.1 => state.1 = true,
                DATA if state.0 && !state.1 => {
                    let remaining = limit.saturating_sub(result.bytes.len());
                    result
                        .bytes
                        .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
                    if bytes.len() > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::FileTooLarge,
                            "guest log byte limit exceeded; truncated",
                        ));
                    }
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid guest log process lifecycle",
                    ));
                }
            }
        }
    })();
    result.error = outcome.err().map(|error| error.to_string());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(sender: &UnixStream, pid: i32, kind: u8, bytes: &[u8]) {
        let mut payload = pid.to_le_bytes().to_vec();
        payload.push(kind);
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

    #[test]
    fn complete_log_preserves_forked_process_bytes() {
        let (sender, receiver) = channel_pair().unwrap();
        for (pid, kind, bytes) in [
            (1, START, b"".as_slice()),
            (2, START, b""),
            (2, DATA, b"child\n"),
            (2, FINISH, b""),
            (2, EXPECT_CHILD, b""),
            (1, DATA, b"parent\n"),
            (1, FINISH, b""),
        ] {
            packet(&sender, pid, kind, bytes);
        }
        drop(sender);
        let log = collect(receiver, 32, &AtomicBool::new(false));
        assert_eq!(log.bytes, b"child\nparent\n");
        assert!(log.error.is_none());
    }

    #[test]
    fn incomplete_and_truncated_logs_are_not_complete() {
        for limit in [2, 100] {
            let (sender, receiver) = channel_pair().unwrap();
            packet(&sender, 1, START, b"");
            packet(&sender, 1, DATA, b"abc\n");
            drop(sender);
            let log = collect(receiver, limit, &AtomicBool::new(false));
            assert_eq!(log.bytes, &b"abc\n"[..limit.min(4)]);
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
        packet(&sender, 1, START, b"");
        packet(&sender, 1, FINISH, b"");
        drop(sender);
        assert!(reader.join().unwrap().error.is_none());
        let (sender, receiver) = channel_pair().unwrap();
        packet(&sender, 1, START, b"");
        packet(&sender, 1, FINISH, b"");
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
        packet(&sender, 1, START, b"");
        packet(&sender, 2, EXPECT_CHILD, b"");
        packet(&sender, 1, FINISH, b"");
        let reader = std::thread::spawn(move || {
            collect_with_drain_timeout(
                receiver,
                32,
                &AtomicBool::new(true),
                Duration::from_millis(10),
            )
        });
        std::thread::sleep(Duration::from_millis(50));
        packet(&sender, 2, START, b"");
        packet(&sender, 2, DATA, b"after parent exit\n");
        packet(&sender, 2, FINISH, b"");
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
    fn broken_transport_exits_even_with_closed_stderr() {
        if std::env::var_os("LITEINST_LOG_BROKEN_TEST").is_some() {
            let (sender, receiver) = channel_pair().unwrap();
            drop(receiver);
            unsafe {
                libc::close(libc::STDERR_FILENO);
            }
            let _writer = unsafe { GuestLog(sender.into()).install() }.unwrap();
            panic!("broken log accepted");
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "guest_log::tests::broken_transport_exits_even_with_closed_stderr",
            ])
            .env("LITEINST_LOG_BROKEN_TEST", "1")
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(127));
    }
}
