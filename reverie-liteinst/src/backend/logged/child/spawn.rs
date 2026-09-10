use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::linux::process::CommandExt as _;
use std::os::linux::process::PidFd;
use std::os::unix::process::CommandExt as _;

use super::Kernel;
use super::Operations;
use super::owned::LaunchLifetime;

const READY: u8 = 0x50;

fn reap_worker_child() -> bool {
    loop {
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        if unsafe { libc::waitid(libc::P_ALL, 0, &mut info, libc::WEXITED | libc::__WNOTHREAD) }
            == 0
        {
            return true;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return error.raw_os_error() == Some(libc::ECHILD);
        }
    }
}

struct Guard<'launch> {
    pidfd: Option<PidFd>,
    launch: Option<&'launch mut dyn LaunchLifetime>,
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if let Some(pidfd) = &self.pidfd {
            while pidfd
                .kill()
                .is_err_and(|error| error.kind() == io::ErrorKind::Interrupted)
            {}
            let reaped = loop {
                match Kernel.wait(pidfd, true) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Ok(Some(_)) => break true,
                    Err(error) if error.raw_os_error() == Some(libc::ECHILD) => break true,
                    _ => break false,
                }
            };
            if reaped && let Some(launch) = &mut self.launch {
                launch.reaped();
            }
        }
    }
}

fn sockets() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut sockets = [-1; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            sockets.as_mut_ptr(),
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    let parent = unsafe { OwnedFd::from_raw_fd(sockets[0]) };
    let child = unsafe { OwnedFd::from_raw_fd(sockets[1]) };
    fn raise(descriptor: OwnedFd) -> io::Result<OwnedFd> {
        if descriptor.as_raw_fd() > libc::STDERR_FILENO {
            return Ok(descriptor);
        }
        let raised = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if raised == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(raised) })
    }
    Ok((raise(parent)?, raise(child)?))
}

fn transfer(descriptor: i32) -> io::Result<()> {
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) };
    if pidfd == -1 {
        return Err(io::Error::last_os_error());
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd as i32) };
    let mut byte = READY;
    let mut vector = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 4];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen =
        unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) as usize };
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as usize;
        libc::CMSG_DATA(header)
            .cast::<i32>()
            .write_unaligned(pidfd.as_raw_fd());
    }
    loop {
        if unsafe { libc::sendmsg(descriptor, &message, libc::MSG_NOSIGNAL) } == 1 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    drop(pidfd);
    loop {
        match unsafe { libc::recv(descriptor, (&raw mut byte).cast(), 1, 0) } {
            1 if byte == READY => return Ok(()),
            -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            _ => return Err(io::Error::from_raw_os_error(libc::ECANCELED)),
        }
    }
}

fn receive(descriptor: i32) -> io::Result<PidFd> {
    let mut byte = 0u8;
    let mut vector = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 4];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    let received = loop {
        message.msg_controllen = std::mem::size_of_val(&control);
        let received = unsafe { libc::recvmsg(descriptor, &mut message, libc::MSG_CMSG_CLOEXEC) };
        if received == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        break received;
    };
    let mut descriptors = Vec::new();
    if received == -1 {
        return Err(io::Error::last_os_error());
    }
    if received == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "child did not transfer pidfd",
        ));
    }
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let bytes = (*header)
                    .cmsg_len
                    .saturating_sub(libc::CMSG_LEN(0) as usize);
                for index in 0..bytes / std::mem::size_of::<i32>() {
                    let raw = libc::CMSG_DATA(header)
                        .cast::<i32>()
                        .add(index)
                        .read_unaligned();
                    descriptors.push(OwnedFd::from_raw_fd(raw));
                }
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    if received != 1
        || byte != READY
        || message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0
        || descriptors.len() != 1
    {
        return Err(io::Error::from_raw_os_error(libc::ECANCELED));
    }
    Ok(PidFd::from(
        descriptors.pop().expect("one received descriptor"),
    ))
}

pub(super) fn owned(
    mut command: std::process::Command,
    launch: &mut dyn LaunchLifetime,
) -> io::Result<(std::process::Child, PidFd, Option<io::Error>)> {
    let (parent, child) = sockets()?;
    let parent_fd = parent.as_raw_fd();
    let child_fd = child.as_raw_fd();
    command.create_pidfd(false);
    unsafe {
        command.pre_exec(move || {
            libc::close(parent_fd);
            transfer(child_fd)
        });
    }
    launch.spawning();
    let (decision, admission) = std::sync::mpsc::channel();
    let worker = std::thread::Builder::new()
        .spawn(move || {
            let result = command.spawn();
            drop(child);
            let (result, reaped) = match result {
                Ok(native) => {
                    let reaped = !admission.recv().unwrap_or(false) && reap_worker_child();
                    (Ok(native), reaped)
                }
                Err(error) => (Err(error), true),
            };
            let cleanup_error =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(command)))
                    .err()
                    .map(|_| io::Error::other("pidfd launch command destruction unwound"));
            (result, reaped, cleanup_error)
        })
        .inspect_err(|_| launch.spawn_failed())?;
    let (pidfd, mut admission_error) = match receive(parent.as_raw_fd()) {
        Ok(pidfd) => (Some(pidfd), None),
        Err(error) => (None, Some(error)),
    };
    let mut guard = Guard {
        pidfd,
        launch: Some(launch),
    };
    let admitted = if guard.pidfd.is_some() {
        let acknowledgement = READY;
        loop {
            let sent = unsafe {
                libc::send(
                    parent.as_raw_fd(),
                    (&raw const acknowledgement).cast(),
                    1,
                    libc::MSG_NOSIGNAL,
                )
            };
            if sent == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if sent != 1 {
                admission_error = Some(io::Error::last_os_error());
            }
            break sent == 1;
        }
    } else {
        false
    };
    drop(parent);
    let _ = decision.send(admitted);
    let (result, reaped, cleanup_error) = worker
        .join()
        .map_err(|_| io::Error::other("pidfd launch worker unwound"))?;
    if reaped {
        guard.launch.as_mut().expect("launch retained").reaped();
    }
    let native = match result {
        Ok(native) => native,
        Err(error) => {
            return Err(match admission_error {
                Some(cause) if cause.kind() != io::ErrorKind::UnexpectedEof => cause,
                _ => error,
            });
        }
    };
    if !admitted {
        return Err(
            admission_error.unwrap_or_else(|| io::Error::from_raw_os_error(libc::ECANCELED))
        );
    }
    guard.launch.take();
    Ok((
        native,
        guard.pidfd.take().expect("pidfd required before exec"),
        cleanup_error,
    ))
}
