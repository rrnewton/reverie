use std::mem::size_of;
use std::mem::size_of_val;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;

use super::raw_syscall6;
use crate::runtime::guarded_raw_syscall;

fn isolated(name: &str) -> bool {
    let name = format!("{}::{name}", module_path!().split_once("::").unwrap().1);
    if std::env::var("LITEINST_SENDMSG_TEST").as_deref() == Ok(name.as_str()) {
        return true;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--nocapture"])
        .env("LITEINST_SENDMSG_TEST", &name)
        .status()
        .unwrap();
    assert!(status.success(), "isolated {name}: {status}");
    false
}

fn assert_empty(receiver: &UnixDatagram) {
    receiver.set_nonblocking(true).unwrap();
    assert_eq!(
        receiver.recv(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

fn bad_pointer(destination: &str, expected: i32) {
    let (reserved, coordinator_receiver) = UnixDatagram::pair().unwrap();
    crate::runtime::reserve_coordinator_fd(reserved.as_raw_fd()).unwrap();
    let ordinary = tempfile::tempfile().unwrap();
    let (sender, receiver) = UnixDatagram::pair().unwrap();
    let descriptor = match destination {
        "invalid" => -1,
        "regular" => ordinary.as_raw_fd(),
        "socket" => sender.as_raw_fd(),
        _ => unreachable!(),
    };
    for pointer in [0, 1, usize::MAX as u64] {
        let args = [descriptor as u64, pointer, 0, 0, 0, 0];
        let native = unsafe { raw_syscall6(libc::SYS_sendmsg, args) };
        assert_eq!(native, -i64::from(expected), "raw Linux {destination}");
        let guarded = guarded_raw_syscall(libc::SYS_sendmsg, args);
        assert_empty(&receiver);
        assert_empty(&coordinator_receiver);
        assert_eq!(ordinary.metadata().unwrap().len(), 0);
        assert_eq!(guarded, native, "{destination}, msghdr={pointer:#x}");
    }
}

#[test]
fn invalid_fd_before_bad_pointer() {
    if isolated("invalid_fd_before_bad_pointer") {
        bad_pointer("invalid", libc::EBADF);
    }
}

#[test]
fn regular_file_before_bad_pointer() {
    if isolated("regular_file_before_bad_pointer") {
        bad_pointer("regular", libc::ENOTSOCK);
    }
}

#[test]
fn socket_bad_pointer() {
    let args = [
        37,
        0x1234,
        (libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) as u64,
        4,
        5,
        6,
    ];
    let probe = super::sendmsg_validation_args(args);
    assert_eq!(probe, [args[0], usize::MAX as u64, args[2], 0, 0, 0]);
    let address = usize::try_from(probe[1]).unwrap();
    assert!(
        address.checked_add(size_of::<libc::msghdr>()).is_none(),
        "validation msghdr must overflow the address space: {address:#x}"
    );
    if isolated("socket_bad_pointer") {
        bad_pointer("socket", libc::EFAULT);
    }
}

fn rights_message(control: &mut [u64; 3], vector: &mut libc::iovec) -> libc::msghdr {
    let header = libc::cmsghdr {
        cmsg_len: size_of::<libc::cmsghdr>() + size_of::<i32>(),
        cmsg_level: libc::SOL_SOCKET,
        cmsg_type: libc::SCM_RIGHTS,
    };
    unsafe { control.as_mut_ptr().cast::<libc::cmsghdr>().write(header) };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = size_of_val(control);
    message
}

#[test]
fn protected_rights_preserve_destination_validation_without_send() {
    if !isolated("protected_rights_preserve_destination_validation_without_send") {
        return;
    }
    let (reserved, coordinator_receiver) = UnixDatagram::pair().unwrap();
    crate::runtime::reserve_coordinator_fd(reserved.as_raw_fd()).unwrap();
    let ordinary = tempfile::tempfile().unwrap();
    let (sender, receiver) = UnixDatagram::pair().unwrap();
    let mut byte = b'x';
    let mut vector = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut control = [0_u64; 3];
    let message = rights_message(&mut control, &mut vector);
    for (destination, expected) in [
        (sender.as_raw_fd(), libc::EBADF),
        (-1, libc::EBADF),
        (ordinary.as_raw_fd(), libc::ENOTSOCK),
    ] {
        let args = [destination as u64, (&raw const message) as u64, 0, 0, 0, 0];
        control[2] = u64::from(u32::MAX);
        let native = unsafe { raw_syscall6(libc::SYS_sendmsg, args) };
        assert_eq!(native, -i64::from(expected));
        control[2] = reserved.as_raw_fd() as u64;
        let guarded = guarded_raw_syscall(libc::SYS_sendmsg, args);
        assert_empty(&receiver);
        assert_empty(&coordinator_receiver);
        assert_eq!(ordinary.metadata().unwrap().len(), 0);
        assert_eq!(guarded, native, "destination={destination}");
    }
}

#[test]
fn ordinary_rights_send_exactly_once() {
    if !isolated("ordinary_rights_send_exactly_once") {
        return;
    }
    let (reserved, coordinator_receiver) = UnixDatagram::pair().unwrap();
    crate::runtime::reserve_coordinator_fd(reserved.as_raw_fd()).unwrap();
    let ordinary = tempfile::tempfile().unwrap();
    let (sender, receiver) = UnixDatagram::pair().unwrap();
    let mut byte = b'x';
    let mut vector = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut control = [0_u64, 0, ordinary.as_raw_fd() as u64];
    let message = rights_message(&mut control, &mut vector);
    let args = [
        sender.as_raw_fd() as u64,
        (&raw const message) as u64,
        0,
        0,
        0,
        0,
    ];
    receiver.set_nonblocking(true).unwrap();
    for guarded in [false, true] {
        let result = if guarded {
            guarded_raw_syscall(libc::SYS_sendmsg, args)
        } else {
            unsafe { raw_syscall6(libc::SYS_sendmsg, args) }
        };
        assert_eq!(result, 1);
        let mut received = [0; 1];
        assert_eq!(receiver.recv(&mut received).unwrap(), 1);
        assert_eq!(received, [b'x']);
        assert_empty(&receiver);
        assert_empty(&coordinator_receiver);
    }
}
