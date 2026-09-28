//! Runtime-only setup for one privately owned guest mapping per process.
//!
//! This exchange does not own process lifetime or select a launcher. Its caller
//! must independently retain the processes, server tasks and mapped cleanup.

use std::io::Read;
use std::io::Write;
use std::io::{self};
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;

use reverie_rpc_transport::mapped::MappedStream;

mod installed;
pub use installed::InstalledCoordinator;
pub use installed::InstalledSetupListener;
pub use installed::InstalledStreams;

const MAGIC: &[u8; 16] = b"REVERIE-LI-MAP1\0";
const REQUEST_LEN: usize = 56;
const RESPONSE_LEN: usize = 80;
const READY: &[u8; 16] = b"LI-MAP-READY-1\0\0";

/// Trusted setup identity copied privately into each admitted guest process.
///
/// A mapped installer uses a fresh setup connection for the root and every
/// process fork. No setup descriptor remains open when config is decoded.
/// This is not a process owner or a guarantee of server/descendant cleanup.
#[derive(Clone)]
pub struct MappedCoordinator {
    address: libc::sockaddr_un,
    address_len: libc::socklen_t,
    run: [u8; 32],
    host: libc::ucred,
}

/// Host-side setup listener. Each accepted stream needs its own serving task.
///
/// The caller owns process lifetime independently of this listener or stream
/// closure, including children that die or remain alive before attachment.
pub struct MappedSetupListener {
    socket: OwnedFd,
    path: PathBuf,
    identity: (u64, u64),
    endpoint: MappedCoordinator,
}

impl MappedSetupListener {
    /// Bind a new runtime setup path and generate a per-run identity.
    /// The existing path is never removed or overwritten.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let (address, address_len) = address(path)?;
        let socket = socket()?;
        call(unsafe { libc::bind(socket.as_raw_fd(), (&raw const address).cast(), address_len) })?;
        let identity = path_identity(path)?;
        let mut run = [0; 32];
        let mut offset = 0;
        while offset < run.len() {
            let count = unsafe {
                libc::getrandom(run[offset..].as_mut_ptr().cast(), run.len() - offset, 0)
            };
            if count < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            if count <= 0 {
                let error = io::Error::last_os_error();
                let _ = std::fs::remove_file(path);
                return Err(error);
            }
            offset += count as usize;
        }
        let host = libc::ucred {
            pid: unsafe { libc::getpid() },
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
        };
        let listener = Self {
            socket,
            path: path.to_owned(),
            identity,
            endpoint: MappedCoordinator {
                address,
                address_len,
                run,
                host,
            },
        };
        call(unsafe { libc::listen(listener.socket.as_raw_fd(), 128) })?;
        Ok(listener)
    }

    /// The caller must transfer this identity through trusted runtime setup,
    /// before application code and before installing the guest Tool.
    pub fn coordinator(&self) -> MappedCoordinator {
        self.endpoint.clone()
    }

    /// Accept and set up one independent stream, closing every setup fd before
    /// returning. Config serialization and serving happen after this operation.
    ///
    /// # Safety
    /// Every process possessing this run identity must be a trusted runtime
    /// setup owner: it imports the transferred fd exactly once, does not expose
    /// or write the backing fd, and observes MappedStream's queue/fork ownership
    /// contract. Credentials and a run identity detect misrouting; they do not
    /// turn an arbitrary application into a trusted mapping owner.
    pub unsafe fn accept(&self, capacity: usize) -> io::Result<(reverie::Pid, MappedStream)> {
        let socket = loop {
            let fd = unsafe {
                libc::accept4(
                    self.socket.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_CLOEXEC,
                )
            };
            if fd >= 0 {
                break unsafe { OwnedFd::from_raw_fd(fd) };
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                return Err(error);
            }
        };
        let peer = credentials(socket.as_raw_fd())?;
        let (request, descriptors) = receive(socket.as_raw_fd(), REQUEST_LEN)?;
        if !descriptors.is_empty()
            || request[..16] != MAGIC[..]
            || request[16..48] != self.endpoint.run
            || i32::from_le_bytes(request[48..52].try_into().unwrap()) != peer.pid
            || i32::from_le_bytes(request[52..56].try_into().unwrap()) != peer.pid
        {
            return Err(invalid("mapped setup request identity mismatch"));
        }
        // SAFETY: accept's contract covers the one trusted peer receiving fd.
        let (mut stream, fd) = unsafe { MappedStream::create(capacity) }?;
        let stat = descriptor_stat(fd.as_raw_fd())?;
        let mut response = [0; RESPONSE_LEN];
        response[..REQUEST_LEN].copy_from_slice(&request);
        response[56..64].copy_from_slice(&stat.st_dev.to_le_bytes());
        response[64..72].copy_from_slice(&stat.st_ino.to_le_bytes());
        response[72..80].copy_from_slice(&stat.st_size.to_le_bytes());
        send(socket.as_raw_fd(), &response, Some(fd.as_raw_fd()))?;
        drop(fd);
        drop(socket);
        // An ack on the mapping follows actual sender closure. An ack on the
        // setup socket before close could let a guest callback race that close.
        stream.write_all(READY)?;
        Ok((reverie::Pid::from_raw(peer.pid), stream))
    }
}

impl Drop for MappedSetupListener {
    fn drop(&mut self) {
        if path_identity(&self.path).ok() == Some(self.identity) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl MappedCoordinator {
    /// Encode the fixed runtime identity for the caller's trusted bootstrap.
    /// This does not modify the existing V1/V2 preload bootstrap formats.
    pub fn to_bytes(&self) -> Vec<u8> {
        let offset = std::mem::offset_of!(libc::sockaddr_un, sun_path);
        let path_len = self.address_len as usize - offset - 1;
        let mut bytes = Vec::with_capacity(62 + path_len);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.run);
        bytes.extend_from_slice(&self.host.pid.to_le_bytes());
        bytes.extend_from_slice(&self.host.uid.to_le_bytes());
        bytes.extend_from_slice(&self.host.gid.to_le_bytes());
        bytes.extend_from_slice(&(path_len as u16).to_le_bytes());
        bytes.extend(
            self.address.sun_path[..path_len]
                .iter()
                .map(|byte| *byte as u8),
        );
        bytes
    }

    /// Decode a setup identity; no descriptor or mapping is opened here.
    /// A mapped installer still requires a trusted bootstrap and peer owner.
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 62 || bytes[..16] != MAGIC[..] {
            return Err(invalid("invalid mapped coordinator encoding"));
        }
        let length = u16::from_le_bytes(bytes[60..62].try_into().unwrap()) as usize;
        if bytes.len() != 62 + length {
            return Err(invalid("invalid mapped coordinator path length"));
        }
        let (address, address_len) = address(Path::new(std::ffi::OsStr::from_bytes(&bytes[62..])))?;
        let host = libc::ucred {
            pid: i32::from_le_bytes(bytes[48..52].try_into().unwrap()),
            uid: u32::from_le_bytes(bytes[52..56].try_into().unwrap()),
            gid: u32::from_le_bytes(bytes[56..60].try_into().unwrap()),
        };
        if host.pid <= 0 {
            return Err(invalid("invalid mapped coordinator PID"));
        }
        Ok(Self {
            address,
            address_len,
            run: bytes[16..48].try_into().unwrap(),
            host,
        })
    }
    /// Import privately in the guest's reusable allocator before config decode.
    /// The caller's trusted single-threaded runtime context covers fork safety.
    pub(super) unsafe fn connect(
        &self,
        pid: reverie::Pid,
        tid: reverie::Pid,
    ) -> io::Result<MappedStream> {
        let socket = socket()?;
        call(unsafe {
            libc::connect(
                socket.as_raw_fd(),
                (&raw const self.address).cast(),
                self.address_len,
            )
        })?;
        let peer = credentials(socket.as_raw_fd())?;
        if peer.pid != self.host.pid || peer.uid != self.host.uid || peer.gid != self.host.gid {
            return Err(invalid("mapped setup coordinator credentials changed"));
        }
        let mut request = [0; REQUEST_LEN];
        request[..16].copy_from_slice(MAGIC);
        request[16..48].copy_from_slice(&self.run);
        request[48..52].copy_from_slice(&pid.as_raw().to_le_bytes());
        request[52..56].copy_from_slice(&tid.as_raw().to_le_bytes());
        send(socket.as_raw_fd(), &request, None)?;
        let (response, mut descriptors) = receive(socket.as_raw_fd(), RESPONSE_LEN)?;
        if response[..REQUEST_LEN] != request || descriptors.len() != 1 {
            return Err(invalid(
                "mapped setup response identity or descriptor count mismatch",
            ));
        }
        let fd = descriptors.pop().unwrap();
        let stat = descriptor_stat(fd.as_raw_fd())?;
        if response[56..64] != stat.st_dev.to_le_bytes()
            || response[64..72] != stat.st_ino.to_le_bytes()
            || response[72..80] != stat.st_size.to_le_bytes()
        {
            return Err(invalid("mapped setup backing identity mismatch"));
        }
        // This is the sole guest import. Neither the stream nor an abort/async
        // alias escapes CoordinatorRpc. Import consumes/closes the received fd.
        // SAFETY: the explicit installer requires the trusted run owner above.
        let mut stream = unsafe { MappedStream::from_owned_fd(fd) }?;
        drop(socket);
        let mut ready = [0; READY.len()];
        stream.read_exact(&mut ready)?;
        if ready != *READY {
            return Err(invalid("mapped setup closure acknowledgement mismatch"));
        }
        Ok(stream)
    }
}

fn socket() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    call(fd)?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn address(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid mapped setup path",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    let len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    Ok((address, len))
}

fn path_identity(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

fn credentials(fd: RawFd) -> io::Result<libc::ucred> {
    let mut credentials = unsafe { std::mem::zeroed::<libc::ucred>() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    call(unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    })?;
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(invalid("mapped setup credential length"));
    }
    Ok(credentials)
}

fn descriptor_stat(fd: RawFd) -> io::Result<libc::stat> {
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    call(unsafe { libc::fstat(fd, &raw mut stat) })?;
    Ok(stat)
}

fn call(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

// Aligned ancillary storage; the kernel closes undisclosed SCM_RIGHTS fds when
// MSG_CTRUNC is set. Every fd actually delivered here is immediately owned,
// including a malformed packet or a count larger than the expected one.
#[repr(C, align(8))]
struct Control([u8; 256]);

fn receive(fd: RawFd, length: usize) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    receive_inner(fd, length, None)
}

fn receive_inner(
    fd: RawFd,
    length: usize,
    deadline: Option<std::time::Instant>,
) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut bytes = vec![0; length];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut control = Control([0; 256]);
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.0.as_mut_ptr().cast();
    message.msg_controllen = control.0.len();
    let count = loop {
        message.msg_controllen = control.0.len();
        message.msg_flags = 0;
        if let Some(deadline) = deadline {
            installed::wait_fd(fd, libc::POLLIN, deadline)?;
        }
        let flags = libc::MSG_CMSG_CLOEXEC
            | if deadline.is_some() {
                libc::MSG_DONTWAIT
            } else {
                0
            };
        let count = unsafe { libc::recvmsg(fd, &raw mut message, flags) };
        if count >= 0 {
            break count as usize;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR)
            && !(deadline.is_some() && error.kind() == io::ErrorKind::WouldBlock)
        {
            return Err(error);
        }
    };
    let mut descriptors = Vec::new();
    let mut invalid_control = false;
    let mut item = unsafe { libc::CMSG_FIRSTHDR(&message) };
    while !item.is_null() {
        let header = unsafe { &*item };
        let base = unsafe { libc::CMSG_LEN(0) } as usize;
        if header.cmsg_len < base {
            invalid_control = true;
            break;
        }
        if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_RIGHTS {
            let size = header.cmsg_len - base;
            if !size.is_multiple_of(std::mem::size_of::<RawFd>()) {
                invalid_control = true;
            }
            let payload = unsafe { libc::CMSG_DATA(item).cast::<RawFd>() };
            for index in 0..size / std::mem::size_of::<RawFd>() {
                descriptors
                    .push(unsafe { OwnedFd::from_raw_fd(payload.add(index).read_unaligned()) });
            }
        } else {
            invalid_control = true;
        }
        item = unsafe { libc::CMSG_NXTHDR(&message, item) };
    }
    if count != length
        || message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
        || invalid_control
    {
        return Err(invalid(
            "mapped setup packet length, truncation or ancillary type",
        ));
    }
    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok((bytes, descriptors))
}

fn send(fd: RawFd, bytes: &[u8], descriptor: Option<RawFd>) -> io::Result<()> {
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    let mut control = Control([0; 256]);
    if let Some(descriptor) = descriptor {
        message.msg_control = control.0.as_mut_ptr().cast();
        message.msg_controllen =
            unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;
        let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        unsafe {
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize;
            libc::CMSG_DATA(header)
                .cast::<RawFd>()
                .write_unaligned(descriptor);
        }
    }
    loop {
        let count = unsafe { libc::sendmsg(fd, &message, libc::MSG_NOSIGNAL) };
        if count == bytes.len() as isize {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if count < 0 && error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(if count < 0 {
            error
        } else {
            invalid("mapped setup partial packet write")
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (OwnedFd, OwnedFd) {
        let mut fds = [-1; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                    0,
                    fds.as_mut_ptr(),
                )
            },
            0
        );
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn fd_count() -> usize {
        std::fs::read_dir("/proc/self/fd").unwrap().count()
    }

    #[test]
    fn setup_identity_roundtrip_preserves_bytes_and_rejects_malformed_lengths() {
        let directory = tempfile::tempdir().unwrap();
        let listener = MappedSetupListener::bind(directory.path().join("setup.sock")).unwrap();
        let original = listener.coordinator().to_bytes();
        let decoded = MappedCoordinator::from_bytes(&original).unwrap();
        assert_eq!(decoded.to_bytes(), original);
        for length in 0..original.len() {
            assert!(
                MappedCoordinator::from_bytes(&original[..length]).is_err(),
                "accepted prefix length {length}"
            );
        }
        let mut changed = original.clone();
        changed.push(0);
        assert!(MappedCoordinator::from_bytes(&changed).is_err());
        changed = original.clone();
        changed[0] ^= 1;
        assert!(MappedCoordinator::from_bytes(&changed).is_err());
        changed = original.clone();
        changed[48..52].copy_from_slice(&0_i32.to_le_bytes());
        assert!(MappedCoordinator::from_bytes(&changed).is_err());
        changed = original;
        changed[62] = 0;
        assert!(MappedCoordinator::from_bytes(&changed).is_err());
    }

    #[test]
    fn received_rights_are_owned_on_success_and_rejected_payloads() {
        if !crate::test_process::isolated(
            "rpc::mapped_setup::tests::received_rights_are_owned_on_success_and_rejected_payloads",
            "LITEINST_MAPPED_SETUP_FD_TEST",
        ) {
            return;
        }
        let (sender, receiver) = pair();
        let source = std::fs::File::open("/dev/null").unwrap();
        let baseline = fd_count();
        for bytes in [b"x".as_slice(), b"xy", b"xyz"] {
            send(sender.as_raw_fd(), bytes, Some(source.as_raw_fd())).unwrap();
            let received = receive(receiver.as_raw_fd(), 2);
            if bytes.len() == 2 {
                let (actual, descriptors) = received.unwrap();
                assert_eq!(actual, bytes);
                assert_eq!(descriptors.len(), 1);
                assert_ne!(descriptors[0].as_raw_fd(), source.as_raw_fd());
                assert_eq!(
                    descriptor_stat(descriptors[0].as_raw_fd()).unwrap().st_rdev,
                    descriptor_stat(source.as_raw_fd()).unwrap().st_rdev
                );
                assert_eq!(
                    unsafe { libc::fcntl(descriptors[0].as_raw_fd(), libc::F_GETFD) }
                        & libc::FD_CLOEXEC,
                    libc::FD_CLOEXEC
                );
                drop(descriptors);
            } else {
                assert_eq!(received.unwrap_err().kind(), io::ErrorKind::InvalidData);
            }
            assert_eq!(fd_count(), baseline, "delivered rights leaked on {bytes:?}");
            assert!(unsafe { libc::fcntl(source.as_raw_fd(), libc::F_GETFD) } >= 0);
        }
    }

    #[test]
    fn truncated_ancillary_rights_close_all_delivered_descriptors() {
        if !crate::test_process::isolated(
            "rpc::mapped_setup::tests::truncated_ancillary_rights_close_all_delivered_descriptors",
            "LITEINST_MAPPED_SETUP_TRUNC_TEST",
        ) {
            return;
        }
        let (sender, receiver) = pair();
        let source = std::fs::File::open("/dev/null").unwrap();
        let baseline = fd_count();
        let rights = [source.as_raw_fd(); 70];
        let mut bytes = [1; 2];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut ancillary = [0_u64; 40];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ancillary.as_mut_ptr().cast();
        msg.msg_controllen =
            unsafe { libc::CMSG_SPACE(std::mem::size_of_val(&rights) as u32) } as usize;
        let header = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        unsafe {
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(&rights) as u32) as usize;
            std::ptr::copy_nonoverlapping(
                rights.as_ptr().cast::<u8>(),
                libc::CMSG_DATA(header),
                std::mem::size_of_val(&rights),
            );
        }
        assert_eq!(
            unsafe { libc::sendmsg(sender.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) },
            2
        );
        assert_eq!(
            receive(receiver.as_raw_fd(), 2).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fd_count(), baseline, "MSG_CTRUNC leaked delivered rights");
        assert!(unsafe { libc::fcntl(source.as_raw_fd(), libc::F_GETFD) } >= 0);
    }

    fn send_descriptors(fd: RawFd, bytes: &[u8], descriptors: &[RawFd]) {
        let mut iov = libc::iovec {
            iov_base: bytes.as_ptr().cast_mut().cast(),
            iov_len: bytes.len(),
        };
        let mut control = [0_u64; 40];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &raw mut iov;
        message.msg_iovlen = 1;
        if !descriptors.is_empty() {
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen =
                unsafe { libc::CMSG_SPACE(std::mem::size_of_val(descriptors) as u32) } as usize;
            assert!(message.msg_controllen <= std::mem::size_of_val(&control));
            let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
            unsafe {
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len =
                    libc::CMSG_LEN(std::mem::size_of_val(descriptors) as u32) as usize;
                std::ptr::copy_nonoverlapping(
                    descriptors.as_ptr().cast::<u8>(),
                    libc::CMSG_DATA(header),
                    std::mem::size_of_val(descriptors),
                );
            }
        }
        assert_eq!(
            unsafe { libc::sendmsg(fd, &message, libc::MSG_NOSIGNAL) },
            bytes.len() as isize
        );
    }

    #[test]
    fn setup_rejects_wrong_run_process_thread_and_unexpected_rights() {
        if !crate::test_process::isolated(
            "rpc::mapped_setup::tests::setup_rejects_wrong_run_process_thread_and_unexpected_rights",
            "LITEINST_MAPPED_SETUP_REQUEST_TEST",
        ) {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let baseline = fd_count();
        for case in ["run", "pid", "tid", "descriptor", "short", "long"] {
            let listener = MappedSetupListener::bind(directory.path().join("setup.sock")).unwrap();
            let endpoint = listener.coordinator();
            let client = socket().unwrap();
            call(unsafe {
                libc::connect(
                    client.as_raw_fd(),
                    (&raw const endpoint.address).cast(),
                    endpoint.address_len,
                )
            })
            .unwrap();
            let pid = unsafe { libc::getpid() };
            let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
            assert_ne!(
                pid, tid,
                "this protocol rejection test runs on a libtest worker"
            );
            let mut request = vec![0; REQUEST_LEN];
            request[..16].copy_from_slice(MAGIC);
            request[16..48].copy_from_slice(&endpoint.run);
            request[48..52].copy_from_slice(&pid.to_le_bytes());
            request[52..56].copy_from_slice(&pid.to_le_bytes());
            match case {
                "run" => request[16] ^= 1,
                "pid" => request[48..52].copy_from_slice(&(pid + 1).to_le_bytes()),
                "tid" => request[52..56].copy_from_slice(&tid.to_le_bytes()),
                "short" => {
                    request.pop();
                }
                "long" => request.push(0),
                _ => {}
            }
            let source = std::fs::File::open("/dev/null").unwrap();
            send(
                client.as_raw_fd(),
                &request,
                (case == "descriptor").then_some(source.as_raw_fd()),
            )
            .unwrap();
            // Every case is rejected before creating or transferring a mapping.
            let error = unsafe { listener.accept(257) }
                .err()
                .expect("invalid request accepted");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{case}: {error}");
            assert_eq!(
                fd_count(),
                baseline + 3,
                "{case}: accepted socket or delivered fd leaked"
            );
            assert!(unsafe { libc::fcntl(source.as_raw_fd(), libc::F_GETFD) } >= 0);
            drop((source, client, listener));
            assert_eq!(fd_count(), baseline, "{case}: setup descriptors leaked");
        }
    }

    #[test]
    fn setup_rejects_changed_server_identity_and_malformed_response_without_leaking_rights() {
        if !crate::test_process::isolated(
            "rpc::mapped_setup::tests::setup_rejects_changed_server_identity_and_malformed_response_without_leaking_rights",
            "LITEINST_MAPPED_SETUP_RESPONSE_TEST",
        ) {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let baseline = fd_count();
        for case in [
            "server-pid",
            "server-uid",
            "server-gid",
            "run",
            "missing",
            "duplicate",
            "short",
            "long",
            "control-truncated",
            "backing",
        ] {
            let listener = MappedSetupListener::bind(directory.path().join("setup.sock")).unwrap();
            let mut endpoint = listener.coordinator();
            match case {
                "server-pid" => endpoint.host.pid += 1,
                "server-uid" => endpoint.host.uid ^= 1,
                "server-gid" => endpoint.host.gid ^= 1,
                _ => {}
            }
            let server = std::thread::spawn(move || {
                let fd = unsafe {
                    libc::accept4(
                        listener.socket.as_raw_fd(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        libc::SOCK_CLOEXEC,
                    )
                };
                assert!(fd >= 0);
                let socket = unsafe { OwnedFd::from_raw_fd(fd) };
                if case.starts_with("server-") {
                    assert_eq!(
                        receive(socket.as_raw_fd(), REQUEST_LEN).unwrap_err().kind(),
                        io::ErrorKind::InvalidData
                    );
                    return;
                }
                let (request, received) = receive(socket.as_raw_fd(), REQUEST_LEN).unwrap();
                assert!(received.is_empty());
                let source = std::fs::File::open("/dev/null").unwrap();
                let stat = descriptor_stat(source.as_raw_fd()).unwrap();
                let mut response = vec![0; RESPONSE_LEN];
                response[..REQUEST_LEN].copy_from_slice(&request);
                response[56..64].copy_from_slice(&stat.st_dev.to_le_bytes());
                response[64..72].copy_from_slice(&stat.st_ino.to_le_bytes());
                response[72..80].copy_from_slice(&stat.st_size.to_le_bytes());
                let mut rights = vec![source.as_raw_fd()];
                match case {
                    "run" => response[16] ^= 1,
                    "missing" => rights.clear(),
                    "duplicate" => rights.push(source.as_raw_fd()),
                    "short" => {
                        response.pop();
                    }
                    "long" => response.push(0),
                    "control-truncated" => rights.resize(70, source.as_raw_fd()),
                    "backing" => response[64] ^= 1,
                    _ => unreachable!(),
                }
                send_descriptors(socket.as_raw_fd(), &response, &rights);
            });
            let pid = reverie::Pid::from_raw(unsafe { libc::getpid() });
            // These deliberately invalid packets all fail before mapping import;
            // no fork, queue alias, or successful runtime admission is asserted.
            let error = unsafe { endpoint.connect(pid, pid) }
                .err()
                .expect("invalid response accepted");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{case}: {error}");
            server.join().unwrap();
            assert_eq!(fd_count(), baseline, "{case}: setup descriptors leaked");
        }
    }
}
