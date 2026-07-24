/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::io;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use reverie_rpc::Channel;
use serde::Deserialize;
use serde::Serialize;
use syscalls::Errno;

use super::protected_files::ProtectedFd;
use super::protected_files::protect_with;

/// The file descriptor that our RPC socket connection should use. We use 100
/// here because many programs or tests expect to use the early file
/// descriptors. Using file descriptor 100 also makes this easier to debug.
const SOCKET_FD: i32 = 100;

struct Inner {
    stream: Option<ProtectedFd<UnixStream>>,
}

/// Adopt the protected RPC connection inherited across exec, if present.
fn inherited_stream() -> io::Result<Option<UnixStream>> {
    let flags = unsafe { libc::fcntl(SOCKET_FD, libc::F_GETFD) };
    if flags >= 0 {
        if flags & libc::FD_CLOEXEC != 0
            && unsafe { libc::fcntl(SOCKET_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0
        {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: F_GETFD proved SOCKET_FD is open. The previous image was
        // replaced by exec, so no live Rust owner remains in this image.
        return Ok(Some(unsafe { UnixStream::from_raw_fd(SOCKET_FD) }));
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EBADF) {
        Ok(None)
    } else {
        Err(error)
    }
}

/// Implements a channel using a UNIX domain socket.
pub struct BaseChannel {
    inner: Mutex<Inner>,
}

impl BaseChannel {
    /// Connects to the global state RPC server.
    pub fn new() -> io::Result<Self> {
        let stream = protect_with(|| -> Result<_, io::Error> {
            if let Some(stream) = inherited_stream()? {
                return Ok(stream);
            }

            let sock_path = std::env::var_os("REVERIE_SOCK")
                .ok_or_else(|| io::Error::other("$REVERIE_SOCK does not exist!"))?;
            let sock = UnixStream::connect(sock_path)?;

            // Move the socket to our desired file descriptor and make sure it
            // survives execve so the replacement plugin can adopt it without
            // changing the target program's environment.
            let fd = Errno::result(unsafe { libc::dup3(sock.as_raw_fd(), SOCKET_FD, 0) })?;

            // Close the old socket file descriptor.
            drop(sock);

            Ok(unsafe { UnixStream::from_raw_fd(fd) })
        })?;

        Ok(Self {
            inner: Mutex::new(Inner {
                stream: Some(stream),
            }),
        })
    }
}

impl Drop for BaseChannel {
    fn drop(&mut self) {
        if let Some(stream) = self.inner.get_mut().unwrap().stream.take() {
            // The reserved descriptor is the exec handoff channel. Process exit
            // closes it, while exec must preserve it for the replacement plugin.
            std::mem::forget(stream);
        }
    }
}

impl Inner {
    fn try_send<T>(&mut self, item: &T) -> io::Result<()>
    where
        T: Serialize,
    {
        let mut buf = Vec::with_capacity(1024);

        reverie_rpc::encode(item, &mut buf)?;

        self.stream
            .as_mut()
            .expect("RPC stream is present")
            .as_mut()
            .write_all(&buf)?;

        Ok(())
    }

    fn try_recv<T>(&mut self) -> io::Result<T>
    where
        T: for<'a> Deserialize<'a>,
    {
        let mut buf = Vec::with_capacity(1024);
        reverie_rpc::decode_from(
            self.stream
                .as_mut()
                .expect("RPC stream is present")
                .as_mut(),
            &mut buf,
        )
    }
}

impl<Req, Res> Channel<Req, Res> for BaseChannel
where
    Req: Serialize,
    Res: for<'a> Deserialize<'a>,
{
    fn send(&self, item: &Req) {
        let mut inner = self.inner.lock().unwrap();
        inner.try_send(item).expect("Failed to send RPC");
    }

    fn call(&self, item: &Req) -> Res {
        let mut inner = self.inner.lock().unwrap();
        inner.try_send(item).expect("Failed to send RPC");
        inner.try_recv().expect("Failed to recv RPC")
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use super::*;

    #[test]
    fn adopts_inherited_socket_and_clears_cloexec() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let saved_fd = unsafe { libc::dup(SOCKET_FD) };
        assert_eq!(
            unsafe { libc::dup3(stream.as_raw_fd(), SOCKET_FD, libc::O_CLOEXEC) },
            SOCKET_FD
        );
        drop(stream);

        let inherited = inherited_stream().unwrap().unwrap();
        assert_eq!(inherited.as_raw_fd(), SOCKET_FD);
        let flags = unsafe { libc::fcntl(SOCKET_FD, libc::F_GETFD) };
        assert!(flags >= 0);
        assert_eq!(flags & libc::FD_CLOEXEC, 0);
        drop(inherited);

        if saved_fd >= 0 {
            assert_eq!(
                unsafe { libc::dup3(saved_fd, SOCKET_FD, libc::O_CLOEXEC) },
                SOCKET_FD
            );
            unsafe { libc::close(saved_fd) };
        } else {
            unsafe { libc::close(SOCKET_FD) };
        }
    }
}
