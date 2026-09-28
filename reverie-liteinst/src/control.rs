/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Fixed-record setup transport. This is separate from serialized Tool RPC.
//! Channel possession is issued at a known birth. Credentials and a real pidfd
//! bind each incarnation; neither a request TID nor creation-time SO_PEERCRED
//! on an inherited socket is accepted as that binding. The trusted sender never
//! supplies explicit SCM_CREDENTIALS. Privileged forged credentials or leaked
//! private aliases remain outside the in-process runtime ownership contract.

use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;

pub(crate) type Gate = unsafe fn(i64, [u64; 6]) -> i64;
pub(crate) const MAGIC: u64 = 0x3150_4352_494c_5652;
pub(crate) const HELLO: u64 = 1;
pub(crate) const RPC: u64 = 2;
pub(crate) const ACQUIRE: u64 = 3;
pub(crate) const EVENT: u64 = 4;
pub(crate) const ACK: u64 = 5;
pub(crate) const PREPARE_FORK: u64 = 6;
pub(crate) const FORK_CHANNEL: u64 = 7;
pub(crate) const ERROR: u64 = 8;
pub(crate) const UNSUPPORTED_CPU: u64 = 9;
#[cfg(feature = "rcb-qualification")]
pub(crate) const RUNNING_SIGNAL_PROBE: u64 = 10;
pub(crate) const MAX_RIGHTS: usize = 4;
pub(crate) const PROTECTED_SLOTS: usize = 8;
static PROTECTED: [AtomicI32; PROTECTED_SLOTS] = [const { AtomicI32::new(-1) }; PROTECTED_SLOTS];

// Initial setup starts before inherited handlers are reset. Once the runtime
// is installed, SIGSYS/SIGSEGV are its validated synchronous machinery, never
// admitted arbitrary guest handlers. Leave their existing mask bits alone so
// nested instrumentation remains usable while other asynchronous handlers wait.
static RUNTIME_SIGNALS_READY: AtomicBool = AtomicBool::new(false);
pub(crate) fn runtime_signals_ready() {
    RUNTIME_SIGNALS_READY.store(true, Ordering::Release);
}

pub(crate) struct SignalFence {
    previous: u64,
    gate: Gate,
}
impl SignalFence {
    pub(crate) fn block(gate: Gate) -> io::Result<Self> {
        let reserved = (1_u64 << (libc::SIGSYS - 1)) | (1_u64 << (libc::SIGSEGV - 1));
        let mask = if RUNTIME_SIGNALS_READY.load(Ordering::Acquire) {
            !reserved
        } else {
            u64::MAX
        };
        let mut previous = 0_u64;
        result(unsafe {
            gate(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_BLOCK as u64,
                    (&raw const mask) as u64,
                    (&raw mut previous) as u64,
                    8,
                    0,
                    0,
                ],
            )
        })?;
        Ok(Self { previous, gate })
    }
}
fn write_syscall_error(gate: Gate, prefix: &[u8], result: i64) {
    let mut digits = [0_u8; 21];
    let mut place = digits.len();
    let mut value = result.unsigned_abs();
    loop {
        place -= 1;
        digits[place] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    if result < 0 {
        place -= 1;
        digits[place] = b'-';
    }
    unsafe {
        gate(
            libc::SYS_write,
            [2, prefix.as_ptr() as u64, prefix.len() as u64, 0, 0, 0],
        );
        gate(
            libc::SYS_write,
            [
                2,
                digits[place..].as_ptr() as u64,
                (digits.len() - place) as u64,
                0,
                0,
                0,
            ],
        );
        gate(libc::SYS_write, [2, b"\n".as_ptr() as u64, 1, 0, 0, 0]);
    }
}
fn mask_failure(gate: Gate, result: i64) -> ! {
    write_syscall_error(
        gate,
        b"reverie-liteinst: private descriptor signal fence raw result=",
        result,
    );
    unsafe {
        gate(libc::SYS_exit_group, [126, 0, 0, 0, 0, 0]);
    }
    loop {
        core::hint::spin_loop();
    }
}
impl Drop for SignalFence {
    fn drop(&mut self) {
        let restored = unsafe {
            (self.gate)(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_SETMASK as u64,
                    (&raw const self.previous) as u64,
                    0,
                    8,
                    0,
                    0,
                ],
            )
        };
        if restored != 0 {
            mask_failure(self.gate, restored);
        }
    }
}

pub(crate) fn protected_fds() -> [i32; PROTECTED_SLOTS] {
    std::array::from_fn(|index| PROTECTED[index].load(Ordering::Acquire))
}

pub(crate) unsafe fn host_gate(number: i64, args: [u64; 6]) -> i64 {
    let result =
        unsafe { libc::syscall(number, args[0], args[1], args[2], args[3], args[4], args[5]) };
    if result == -1 {
        -i64::from(
            io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO),
        )
    } else {
        result
    }
}

pub(crate) fn result(value: i64) -> io::Result<i64> {
    if (-4095..0).contains(&value) {
        Err(io::Error::from_raw_os_error(-value as i32))
    } else {
        Ok(value)
    }
}
pub(crate) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// A gate-aware owner. The slot stays protected through its sole raw close;
/// a signal fence prevents asynchronous reuse before the slot is cleared.
#[derive(Debug)]
pub(crate) struct Fd {
    fd: RawFd,
    gate: Gate,
    slot: Option<usize>,
}
impl Fd {
    pub(crate) fn owned(fd: OwnedFd, gate: Gate, protect: bool) -> io::Result<Self> {
        let fd = fd.into_raw_fd();
        let mut value = Self {
            fd,
            gate,
            slot: None,
        };
        if protect {
            value.protect()?;
        }
        Ok(value)
    }
    fn protect(&mut self) -> io::Result<()> {
        let slot = PROTECTED
            .iter()
            .position(|slot| {
                slot.compare_exchange(-1, self.fd, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EMFILE))?;
        self.slot = Some(slot);
        Ok(())
    }
    pub(crate) fn into_owned_reserved(mut self) -> io::Result<(OwnedFd, Reservation)> {
        let fence = SignalFence::block(self.gate)?;
        let fd = std::mem::replace(&mut self.fd, -1);
        let reservation = Reservation {
            slot: self.slot.take(),
            _fence: fence,
        };
        Ok((unsafe { OwnedFd::from_raw_fd(fd) }, reservation))
    }
}
pub(crate) struct Reservation {
    slot: Option<usize>,
    _fence: SignalFence,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            PROTECTED[slot].store(-1, Ordering::Release);
        }
    }
}
impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}
impl Drop for Fd {
    fn drop(&mut self) {
        let _fence = self.slot.map(|_| {
            SignalFence::block(self.gate).unwrap_or_else(|error| {
                mask_failure(
                    self.gate,
                    -i64::from(error.raw_os_error().unwrap_or(libc::EIO)),
                )
            })
        });
        if self.fd >= 0 {
            let closed = unsafe { (self.gate)(libc::SYS_close, [self.fd as u64, 0, 0, 0, 0, 0]) };
            self.fd = -1; // Linux releases this slot even on a late close error.
            if closed != 0 {
                // Preserve an existing Err/panic; report cleanup failure without
                // retrying a slot that might already have been released.
                write_syscall_error(
                    self.gate,
                    b"reverie-liteinst: private close raw result=",
                    closed,
                );
            }
        }
        if let Some(slot) = self.slot.take() {
            PROTECTED[slot].store(-1, Ordering::Release);
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct Packet(pub [u64; 8]);
impl Packet {
    pub(crate) fn new(kind: u64, generation: u64) -> Self {
        Self([MAGIC, kind, generation, 0, 0, 0, 0, 0])
    }
    pub(crate) fn require(&self, kind: u64, generation: u64) -> io::Result<()> {
        if self.0[0] != MAGIC || self.0[1] != kind || self.0[2] != generation {
            return Err(invalid("RCB setup record/state mismatch"));
        }
        Ok(())
    }
}

pub(crate) struct Received {
    pub(crate) packet: Packet,
    pub(crate) rights: [Option<Fd>; MAX_RIGHTS],
    pub(crate) credentials: Option<libc::ucred>,
}
impl Received {
    pub(crate) fn only_fd(mut self) -> io::Result<(Packet, Fd)> {
        if self.rights[1..].iter().any(Option::is_some) {
            return Err(invalid("unexpected setup descriptors"));
        }
        Ok((
            self.packet,
            self.rights[0]
                .take()
                .ok_or_else(|| invalid("missing setup descriptor"))?,
        ))
    }
    pub(crate) fn no_fds(&self) -> io::Result<()> {
        if self.rights.iter().any(Option::is_some) {
            Err(invalid("unexpected setup descriptor"))
        } else {
            Ok(())
        }
    }
}

pub(crate) fn pair(gate: Gate, protect: bool) -> io::Result<(Fd, Fd)> {
    let _fence = if protect {
        Some(SignalFence::block(gate)?)
    } else {
        None
    };
    let mut values = [-1_i32; 2];
    result(unsafe {
        gate(
            libc::SYS_socketpair,
            [
                libc::AF_UNIX as u64,
                (libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC) as u64,
                0,
                (&raw mut values) as u64,
                0,
                0,
            ],
        )
    })?;
    let first = unsafe { OwnedFd::from_raw_fd(values[0]) };
    let second = unsafe { OwnedFd::from_raw_fd(values[1]) };
    // Establish both gate-aware owners before fallible slot reservation.
    let mut first = Fd::owned(first, gate, false)?;
    let mut second = Fd::owned(second, gate, false)?;
    if protect {
        first.protect()?;
        second.protect()?;
    }
    for fd in [&first, &second] {
        let yes = 1_i32;
        result(unsafe {
            gate(
                libc::SYS_setsockopt,
                [
                    fd.fd as u64,
                    libc::SOL_SOCKET as u64,
                    libc::SO_PASSCRED as u64,
                    (&raw const yes) as u64,
                    4,
                    0,
                ],
            )
        })?;
        let timeout = libc::timeval {
            tv_sec: 30,
            tv_usec: 0,
        };
        for option in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
            result(unsafe {
                gate(
                    libc::SYS_setsockopt,
                    [
                        fd.fd as u64,
                        libc::SOL_SOCKET as u64,
                        option as u64,
                        (&raw const timeout) as u64,
                        std::mem::size_of_val(&timeout) as u64,
                        0,
                    ],
                )
            })?;
        }
    }
    Ok((first, second))
}

pub(crate) fn send(
    socket: &Fd,
    packet: Packet,
    rights: &[RawFd],
    nonblocking: bool,
) -> io::Result<()> {
    if rights.len() > MAX_RIGHTS {
        return Err(invalid("too many setup descriptors"));
    }
    let mut packet = packet;
    let mut vector = libc::iovec {
        iov_base: (&raw mut packet).cast(),
        iov_len: 64,
    };
    let mut control = [0_usize; 16];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    if !rights.is_empty() {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen =
            unsafe { libc::CMSG_SPACE(std::mem::size_of_val(rights) as u32) } as usize;
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(rights) as u32) as usize;
            std::ptr::copy_nonoverlapping(
                rights.as_ptr(),
                libc::CMSG_DATA(header).cast(),
                rights.len(),
            );
        }
    }
    let flags = libc::MSG_NOSIGNAL | if nonblocking { libc::MSG_DONTWAIT } else { 0 };
    let count = result(unsafe {
        (socket.gate)(
            libc::SYS_sendmsg,
            [
                socket.fd as u64,
                (&raw const message) as u64,
                flags as u64,
                0,
                0,
                0,
            ],
        )
    })?;
    if count != 64 {
        return Err(invalid("short setup send"));
    }
    Ok(())
}

pub(crate) fn receive(socket: &Fd, protect: bool, nonblocking: bool) -> io::Result<Received> {
    // No asynchronous guest handler may close a newly received descriptor
    // between the kernel return and publication of all bounded private slots.
    let _fence = if protect {
        Some(SignalFence::block(socket.gate)?)
    } else {
        None
    };
    let mut packet = Packet([0; 8]);
    let mut vector = libc::iovec {
        iov_base: (&raw mut packet).cast(),
        iov_len: 64,
    };
    // The kernel can return more than the protocol maximum. Parse and close all
    // delivered rights, even on a malformed/truncated record.
    let mut control = [0_usize; 64];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    let flags = libc::MSG_CMSG_CLOEXEC | if nonblocking { libc::MSG_DONTWAIT } else { 0 };
    let count = result(unsafe {
        (socket.gate)(
            libc::SYS_recvmsg,
            [
                socket.fd as u64,
                (&raw mut message) as u64,
                flags as u64,
                0,
                0,
                0,
            ],
        )
    })?;
    let mut received = Received {
        packet,
        rights: std::array::from_fn(|_| None),
        credentials: None,
    };
    let mut bad = count != 64
        || packet.0[0] != MAGIC
        || message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0;
    let mut used = 0;
    let mut ownership_error = None;
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            let length = (*header).cmsg_len;
            let offset = header.cast::<u8>().offset_from(control.as_ptr().cast()) as usize;
            if length < libc::CMSG_LEN(0) as usize
                || offset
                    .checked_add(length)
                    .is_none_or(|end| end > message.msg_controllen)
            {
                bad = true;
                break;
            }
            let bytes = length - libc::CMSG_LEN(0) as usize;
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                bad |= !bytes.is_multiple_of(4);
                for index in 0..bytes / 4 {
                    let raw = libc::CMSG_DATA(header)
                        .cast::<i32>()
                        .add(index)
                        .read_unaligned();
                    let fd = Fd::owned(OwnedFd::from_raw_fd(raw), socket.gate, protect);
                    match fd {
                        Ok(fd) if used < MAX_RIGHTS => {
                            received.rights[used] = Some(fd);
                            used += 1;
                        }
                        Ok(_) => {
                            bad = true;
                        }
                        Err(error) => {
                            ownership_error = Some(error);
                        }
                    }
                }
            } else if (*header).cmsg_level == libc::SOL_SOCKET
                && (*header).cmsg_type == libc::SCM_CREDENTIALS
                && bytes == std::mem::size_of::<libc::ucred>()
            {
                if received.credentials.is_some() {
                    bad = true;
                }
                received.credentials = Some(
                    libc::CMSG_DATA(header)
                        .cast::<libc::ucred>()
                        .read_unaligned(),
                );
            } else {
                bad = true;
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    if let Some(error) = ownership_error {
        return Err(error);
    }
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "setup peer closed",
        ));
    }
    if bad {
        return Err(invalid("invalid setup ancillary record"));
    }
    if received.packet.0[1] == ERROR {
        received.no_fds()?;
        return Err(io::Error::from_raw_os_error(
            i32::try_from(received.packet.0[4])
                .ok()
                .filter(|v| *v > 0)
                .unwrap_or(libc::EPROTO),
        ));
    }
    Ok(received)
}

pub(crate) fn self_pidfd(gate: Gate, protect: bool) -> io::Result<Fd> {
    let _fence = if protect {
        Some(SignalFence::block(gate)?)
    } else {
        None
    };
    let pid = result(unsafe { gate(libc::SYS_getpid, [0; 6]) })?;
    let fd = result(unsafe { gate(libc::SYS_pidfd_open, [pid as u64, 0, 0, 0, 0, 0]) })?;
    Fd::owned(unsafe { OwnedFd::from_raw_fd(fd as i32) }, gate, protect)
}

#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct PidInfo {
    pub(crate) mask: u64,
    pub(crate) cgroup: u64,
    pub(crate) pid: u32,
    pub(crate) tgid: u32,
    pub(crate) ppid: u32,
    pub(crate) ruid: u32,
    pub(crate) rgid: u32,
    pub(crate) euid: u32,
    pub(crate) egid: u32,
    pub(crate) suid: u32,
    pub(crate) sgid: u32,
    pub(crate) fsuid: u32,
    pub(crate) fsgid: u32,
    pub(crate) exit_code: i32,
}
const _: () = assert!(std::mem::size_of::<PidInfo>() == 64);

pub(crate) fn pid_info(fd: &Fd) -> io::Result<PidInfo> {
    let mut info = PidInfo {
        mask: 1 | 2 | 8,
        ..PidInfo::default()
    };
    result(unsafe {
        (fd.gate)(
            libc::SYS_ioctl,
            [fd.fd as u64, 0xc040_ff0b, (&raw mut info) as u64, 0, 0, 0],
        )
    })?;
    if info.mask & 3 != 3 || info.mask & 8 != 0 || info.pid == 0 || info.pid != info.tgid {
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    let mut poll = libc::pollfd {
        fd: fd.fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready =
        result(unsafe { (fd.gate)(libc::SYS_poll, [(&raw mut poll) as u64, 1, 0, 0, 0, 0]) })?;
    if ready != 0 || poll.revents != 0 {
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    Ok(info)
}

/// Only transport I/O uses the gate. Serde/config/Tool code runs outside it,
/// with this descriptor still protected from ordinary intercepted operations.
pub(crate) struct TrustedStream(pub(crate) Fd);
impl AsRawFd for TrustedStream {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}
impl io::Read for TrustedStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        result(unsafe {
            (self.0.gate)(
                libc::SYS_read,
                [
                    self.0.fd as u64,
                    buffer.as_mut_ptr() as u64,
                    buffer.len() as u64,
                    0,
                    0,
                    0,
                ],
            )
        })
        .map(|n| n as usize)
    }
}
impl io::Write for TrustedStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        result(unsafe {
            (self.0.gate)(
                libc::SYS_sendto,
                [
                    self.0.fd as u64,
                    buffer.as_ptr() as u64,
                    buffer.len() as u64,
                    libc::MSG_NOSIGNAL as u64,
                    0,
                    0,
                ],
            )
        })
        .map(|n| n as usize)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

const BOOTSTRAP_MAGIC: &[u8; 16] = b"RV-LI-CLOCK-V1\0\0";

pub(crate) struct Bootstrap {
    pub(crate) child: Fd,
    file: OwnedFd,
}
impl Bootstrap {
    pub(crate) fn new(child: Fd, coordinator: &std::path::Path) -> io::Result<Self> {
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        result(unsafe {
            host_gate(
                libc::SYS_fstat,
                [child.fd as u64, (&raw mut stat) as u64, 0, 0, 0, 0],
            )
        })?;
        let path = coordinator.as_os_str().as_bytes();
        if path.len() > 4000 {
            return Err(invalid("setup bootstrap path too long"));
        }
        let mut bytes = Vec::with_capacity(44 + path.len());
        bytes.extend_from_slice(BOOTSTRAP_MAGIC);
        bytes.extend_from_slice(&(child.fd as u64).to_le_bytes());
        bytes.extend_from_slice(&stat.st_ino.to_le_bytes());
        bytes.extend_from_slice(&stat.st_dev.to_le_bytes());
        bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
        bytes.extend_from_slice(path);
        let raw = result(unsafe {
            host_gate(
                libc::SYS_memfd_create,
                [
                    c"reverie-liteinst-clock-bootstrap".as_ptr() as u64,
                    (libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) as u64,
                    0,
                    0,
                    0,
                    0,
                ],
            )
        })?;
        let mut file = unsafe { std::fs::File::from_raw_fd(raw as i32) };
        file.write_all(&bytes)?;
        let seals =
            libc::F_SEAL_SEAL | libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK;
        result(unsafe {
            host_gate(
                libc::SYS_fcntl,
                [raw as u64, libc::F_ADD_SEALS as u64, seals as u64, 0, 0, 0],
            )
        })?;
        Ok(Self {
            child,
            file: file.into(),
        })
    }
    pub(crate) fn configure(&self, command: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        let descriptors = [self.child.fd, self.file.as_raw_fd()];
        unsafe {
            command.pre_exec(move || {
                for fd in descriptors {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
}

/// Called before seccomp in the initial process. The sealed file names a real
/// inherited endpoint, whose kernel file identity and socket kind must match.
pub(crate) fn take_bootstrap(coordinator: &std::path::Path, gate: Gate) -> io::Result<Fd> {
    let _fence = SignalFence::block(gate)?;
    use std::os::unix::ffi::OsStrExt;
    let mut found = None;
    for entry in std::fs::read_dir("/proc/self/fd")? {
        let Some(raw) = entry?
            .file_name()
            .to_str()
            .and_then(|v| v.parse::<i32>().ok())
        else {
            continue;
        };
        if raw <= 2 {
            continue;
        }
        let mut prefix = [0_u8; 44];
        let n = unsafe {
            gate(
                libc::SYS_pread64,
                [
                    raw as u64,
                    prefix.as_mut_ptr() as u64,
                    prefix.len() as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        if n != 44 || &prefix[..16] != BOOTSTRAP_MAGIC {
            continue;
        }
        let bootstrap = Fd::owned(unsafe { OwnedFd::from_raw_fd(raw) }, gate, true)?;
        let seals = result(unsafe {
            gate(
                libc::SYS_fcntl,
                [raw as u64, libc::F_GET_SEALS as u64, 0, 0, 0, 0],
            )
        })? as i32;
        let required =
            libc::F_SEAL_SEAL | libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK;
        if seals & required != required || found.is_some() {
            return Err(invalid("ambiguous/unsealed setup bootstrap"));
        }
        let number = u64::from_le_bytes(prefix[16..24].try_into().unwrap());
        let inode = u64::from_le_bytes(prefix[24..32].try_into().unwrap());
        let device = u64::from_le_bytes(prefix[32..40].try_into().unwrap());
        let len = u32::from_le_bytes(prefix[40..44].try_into().unwrap()) as usize;
        if number > i32::MAX as u64 || number <= 2 || len > 4000 {
            return Err(invalid("invalid setup bootstrap fields"));
        }
        let mut bytes = [0_u8; 4000];
        let count = result(unsafe {
            gate(
                libc::SYS_pread64,
                [raw as u64, bytes.as_mut_ptr() as u64, len as u64, 44, 0, 0],
            )
        })?;
        if count as usize != len || &bytes[..len] != coordinator.as_os_str().as_bytes() {
            return Err(invalid("setup bootstrap coordinator mismatch"));
        }
        let mut file_stat: libc::stat = unsafe { std::mem::zeroed() };
        result(unsafe {
            gate(
                libc::SYS_fstat,
                [raw as u64, (&raw mut file_stat) as u64, 0, 0, 0, 0],
            )
        })?;
        if file_stat.st_size != (44 + len) as i64 {
            return Err(invalid("setup bootstrap length mismatch"));
        }
        let mut socket_stat: libc::stat = unsafe { std::mem::zeroed() };
        result(unsafe {
            gate(
                libc::SYS_fstat,
                [number, (&raw mut socket_stat) as u64, 0, 0, 0, 0],
            )
        })?;
        let mut kind = 0_i32;
        let mut size = 4_u32;
        result(unsafe {
            gate(
                libc::SYS_getsockopt,
                [
                    number,
                    libc::SOL_SOCKET as u64,
                    libc::SO_TYPE as u64,
                    (&raw mut kind) as u64,
                    (&raw mut size) as u64,
                    0,
                ],
            )
        })?;
        if socket_stat.st_ino != inode
            || socket_stat.st_dev != device
            || kind != libc::SOCK_SEQPACKET
            || size != 4
        {
            return Err(invalid("setup endpoint identity mismatch"));
        }
        result(unsafe {
            gate(
                libc::SYS_fcntl,
                [
                    number,
                    libc::F_SETFD as u64,
                    libc::FD_CLOEXEC as u64,
                    0,
                    0,
                    0,
                ],
            )
        })?;
        found = Some(Fd::owned(
            unsafe { OwnedFd::from_raw_fd(number as i32) },
            gate,
            true,
        )?);
        drop(bootstrap);
    }
    found.ok_or_else(|| invalid("authenticated LiteInst setup bootstrap is missing"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_record(socket: &Fd, packet: &mut [u8], rights: &[i32]) {
        let mut vector = libc::iovec {
            iov_base: packet.as_mut_ptr().cast(),
            iov_len: packet.len(),
        };
        let mut ancillary = vec![0_usize; 128];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut vector;
        message.msg_iovlen = 1;
        message.msg_control = ancillary.as_mut_ptr().cast();
        message.msg_controllen =
            unsafe { libc::CMSG_SPACE(std::mem::size_of_val(rights) as u32) } as usize;
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(rights) as u32) as usize;
            std::ptr::copy_nonoverlapping(
                rights.as_ptr(),
                libc::CMSG_DATA(header).cast(),
                rights.len(),
            );
        }
        assert_eq!(
            unsafe {
                host_gate(
                    libc::SYS_sendmsg,
                    [
                        socket.as_raw_fd() as u64,
                        (&raw const message) as u64,
                        libc::MSG_NOSIGNAL as u64,
                        0,
                        0,
                        0,
                    ],
                )
            },
            packet.len() as i64
        );
    }

    #[test]
    fn kernel_credentials_and_owned_rights_are_received_together() {
        let (host, target) = pair(host_gate, false).unwrap();
        let pidfd = self_pidfd(host_gate, false).unwrap();
        send(&target, Packet::new(HELLO, 0), &[pidfd.as_raw_fd()], false).unwrap();
        let received = receive(&host, false, false).unwrap();
        let credentials = received.credentials.unwrap();
        assert_eq!(credentials.pid, unsafe { libc::getpid() });
        assert_eq!(credentials.uid, unsafe { libc::getuid() });
        let (_, received_pidfd) = received.only_fd().unwrap();
        let identity = pid_info(&received_pidfd).unwrap();
        assert_eq!(identity.pid, unsafe { libc::getpid() } as u32);
        assert_ne!(received_pidfd.as_raw_fd(), pidfd.as_raw_fd());
        assert_ne!(
            unsafe { libc::fcntl(received_pidfd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }

    #[test]
    fn malformed_transfers_close_all_actual_delivered_references() {
        for (length, copies, corrupt_magic) in [
            (63, 1, false),
            (64, 5, false),
            (64, 200, false),
            (64, 1, true),
        ] {
            let (host, target) = pair(host_gate, false).unwrap();
            let (owned, peer) = std::os::unix::net::UnixStream::pair().unwrap();
            let mut packet = [0_u8; 64];
            packet[..8].copy_from_slice(&MAGIC.to_ne_bytes());
            if corrupt_magic {
                packet[0] ^= 1;
            }
            raw_record(
                &target,
                &mut packet[..length],
                &vec![owned.as_raw_fd(); copies],
            );
            drop(owned);
            let error = receive(&host, false, false)
                .err()
                .expect("malformed record must fail");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            // Every transferred copy of the only peer has been closed. This
            // tests file lifetime, without assuming an FD number cannot reuse.
            let byte = 1_u8;
            let result = unsafe {
                host_gate(
                    libc::SYS_sendto,
                    [
                        peer.as_raw_fd() as u64,
                        (&raw const byte) as u64,
                        1,
                        libc::MSG_NOSIGNAL as u64,
                        0,
                        0,
                    ],
                )
            };
            assert_eq!(
                result,
                -i64::from(libc::EPIPE),
                "length={length} copies={copies} corrupt={corrupt_magic}"
            );
        }
    }

    #[test]
    fn setup_records_reject_wrong_kind_and_generation() {
        let packet = Packet::new(EVENT, 3);
        assert!(packet.require(EVENT, 3).is_ok());
        assert!(packet.require(EVENT, 2).is_err());
        assert!(packet.require(RPC, 3).is_err());
    }
}
