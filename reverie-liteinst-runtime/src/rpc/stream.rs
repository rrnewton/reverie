use std::io;
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::IntoRawFd;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::SocketAddr;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use super::raw_syscall6;
use super::resources::Lease;
use crate::runtime;

pub(super) struct Stream {
    fd: i32,
    publication: Arc<AtomicBool>,
    lease: Lease,
}

fn native_result(result: i64) -> io::Result<usize> {
    if result < 0 {
        Err(io::Error::from_raw_os_error((-result) as i32))
    } else {
        Ok(result as usize)
    }
}

impl Stream {
    pub(super) fn connected(
        stream: UnixStream,
        publication: Arc<AtomicBool>,
        lease: Lease,
    ) -> Self {
        Self {
            fd: stream.into_raw_fd(),
            publication,
            lease,
        }
    }

    pub(super) fn bootstrap(
        path: &Path,
        publication: Arc<AtomicBool>,
        lease: Lease,
    ) -> io::Result<Self> {
        let address = SocketAddr::from_pathname(path)?;
        let mut native: libc::sockaddr_un = unsafe { core::mem::zeroed() };
        native.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = if let Some(path) = address.as_pathname() {
            path.as_os_str().as_bytes()
        } else {
            address
                .as_abstract_name()
                .ok_or_else(|| io::Error::other("RPC address is unnamed"))?
        };
        let abstract_address = address.as_abstract_name().is_some();
        let start = usize::from(abstract_address);
        for (destination, source) in native.sun_path[start..].iter_mut().zip(bytes) {
            *destination = *source as libc::c_char;
        }
        let length = core::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
        let fd = native_result(unsafe {
            raw_syscall6(
                libc::SYS_socket,
                [
                    libc::AF_UNIX as u64,
                    (libc::SOCK_STREAM | libc::SOCK_CLOEXEC) as u64,
                    0,
                    0,
                    0,
                    0,
                ],
            )
        })? as i32;
        let stream = Self {
            fd,
            publication,
            lease,
        };
        runtime::reserve_coordinator_fd(fd)?;
        stream.publication.store(true, Ordering::Release);
        #[cfg(test)]
        super::tests::published(fd);
        native_result(unsafe {
            raw_syscall6(
                libc::SYS_connect,
                [
                    fd as u64,
                    (&raw const native) as u64,
                    length as u64,
                    0,
                    0,
                    0,
                ],
            )
        })?;
        Ok(stream)
    }
}

impl AsRawFd for Stream {
    fn as_raw_fd(&self) -> i32 {
        self.fd
    }
}

impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        native_result(unsafe {
            raw_syscall6(
                libc::SYS_recvfrom,
                [
                    self.fd as u64,
                    bytes.as_mut_ptr() as u64,
                    bytes.len() as u64,
                    0,
                    0,
                    0,
                ],
            )
        })
    }
}

impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        native_result(unsafe {
            raw_syscall6(
                libc::SYS_sendto,
                [
                    self.fd as u64,
                    bytes.as_ptr() as u64,
                    bytes.len() as u64,
                    libc::MSG_NOSIGNAL as u64,
                    0,
                    0,
                ],
            )
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        #[cfg(test)]
        super::tests::before_close(self.fd);
        let result = unsafe { raw_syscall6(libc::SYS_close, [self.fd as u64, 0, 0, 0, 0, 0]) };
        if let Err(error) = native_result(result) {
            self.lease.failed(error.raw_os_error().unwrap());
        }
        if self.publication.load(Ordering::Acquire)
            && runtime::replace_coordinator_fd(self.fd, -1).is_err()
        {
            self.lease.failed(libc::EIO);
        }
    }
}
