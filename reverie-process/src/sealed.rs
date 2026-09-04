//! Immutable byte images in caller-owned CLOEXEC memfds.

use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;

/// Copy bytes into a new memfd, then install all four immutable-file seals.
///
/// Returns ownership only after writing and sealing succeeds; every error drops
/// acquired descriptors. `minimum_fd` is a lower bound, not a dup2 destination.
/// The descriptor stays CLOEXEC. This seals bytes, not provenance or CRT semantics.
pub fn create(name: &CStr, bytes: &[u8], minimum_fd: i32) -> io::Result<OwnedFd> {
    if minimum_fd < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "negative descriptor floor",
        ));
    }
    let fd =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(fd) };
    let descriptor = if fd < minimum_fd {
        let raised = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, minimum_fd) };
        if raised == -1 {
            return Err(io::Error::last_os_error());
        }
        let raised = unsafe { OwnedFd::from_raw_fd(raised) };
        drop(descriptor);
        raised
    } else {
        descriptor
    };
    let mut file = File::from(descriptor);
    file.write_all(bytes)?;
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(file.into())
}

#[cfg(test)]
mod tests;
