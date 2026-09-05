use std::mem::MaybeUninit;
use std::mem::size_of;

use reverie_preload::trap::raw_syscall6;

fn read<T: Copy>(address: u64) -> Result<T, i32> {
    let mut value = MaybeUninit::<T>::uninit();
    let local = libc::iovec {
        iov_base: value.as_mut_ptr().cast(),
        iov_len: size_of::<T>(),
    };
    let remote = libc::iovec {
        iov_base: address as *mut libc::c_void,
        iov_len: size_of::<T>(),
    };
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let count = unsafe {
        raw_syscall6(
            libc::SYS_process_vm_readv,
            [
                pid as u64,
                (&raw const local) as u64,
                1,
                (&raw const remote) as u64,
                1,
                0,
            ],
        )
    };
    if count == size_of::<T>() as i64 {
        Ok(unsafe { value.assume_init() })
    } else {
        Err(libc::EFAULT)
    }
}

fn check(fd: i32, protected: &[i32]) -> Result<(), i32> {
    if fd >= 0 && protected.contains(&fd) {
        Err(libc::EBADF)
    } else {
        Ok(())
    }
}

fn offset(base: u64, index: u64, stride: usize) -> Result<u64, i32> {
    index
        .checked_mul(stride as u64)
        .and_then(|offset| base.checked_add(offset))
        .ok_or(libc::EFAULT)
}

fn array(address: u64, count: u64, protected: &[i32]) -> Result<(), i32> {
    for index in 0..count {
        check(
            read::<i32>(offset(address, index, size_of::<i32>())?)?,
            protected,
        )?;
    }
    Ok(())
}

fn message(address: u64, protected: &[i32]) -> Result<(), i32> {
    let message: libc::msghdr = read(address)?;
    let mut position = message.msg_control as u64;
    let mut remaining = message.msg_controllen;
    let header_size = size_of::<libc::cmsghdr>();
    while remaining >= header_size {
        let header: libc::cmsghdr = read(position)?;
        if header.cmsg_len < header_size || header.cmsg_len > remaining {
            return Err(libc::EINVAL);
        }
        if header.cmsg_level == libc::SOL_SOCKET && header.cmsg_type == libc::SCM_RIGHTS {
            array(
                position
                    .checked_add(header_size as u64)
                    .ok_or(libc::EFAULT)?,
                ((header.cmsg_len - header_size) / size_of::<i32>()) as u64,
                protected,
            )?;
        }
        let aligned = header
            .cmsg_len
            .checked_add(size_of::<usize>() - 1)
            .ok_or(libc::EINVAL)?
            & !(size_of::<usize>() - 1);
        if aligned > remaining {
            break;
        }
        position = position.checked_add(aligned as u64).ok_or(libc::EFAULT)?;
        remaining -= aligned;
    }
    Ok(())
}

fn inspect(number: i64, args: [u64; 6], protected: &[i32]) -> Result<(), i32> {
    match number {
        libc::SYS_sendmsg => message(args[1], protected),
        libc::SYS_io_uring_register => match (args[1] as u32) & !(1 << 31) {
            2 => array(args[2], u64::from(args[3] as u32), protected),
            4 | 7 => check(read::<i32>(args[2])?, protected),
            6 => {
                let update: [u64; 2] = read(args[2])?;
                array(update[1], u64::from(args[3] as u32), protected)
            }
            13 => {
                let registration: [u64; 4] = read(args[2])?;
                if registration[0] >> 32 & 1 != 0 && registration[2] == 0 {
                    return Ok(());
                }
                array(
                    registration[2],
                    u64::from(registration[0] as u32),
                    protected,
                )
            }
            14 => {
                let update: [u64; 4] = read(args[2])?;
                array(update[1], u64::from(update[3] as u32), protected)
            }
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}

pub(crate) fn batch_args(
    number: i64,
    mut args: [u64; 6],
    protected: &[i32],
) -> Result<[u64; 6], i64> {
    if protected.iter().all(|fd| *fd < 0) {
        return Ok(args);
    }
    let (count_index, count) = match number {
        libc::SYS_sendmmsg => (2, u64::from(args[2] as u32).min(1024)),
        libc::SYS_io_submit if args[1] as i64 > 0 => (1, args[1]),
        _ => return Ok(args),
    };
    for index in 0..count {
        let inspected = (|| {
            if number == libc::SYS_sendmmsg {
                message(
                    offset(args[1], index, size_of::<libc::mmsghdr>())?,
                    protected,
                )
            } else {
                let address: u64 = read(offset(args[2], index, size_of::<u64>())?)?;
                let request: [u32; 16] = read(address)?;
                check(request[5] as i32, protected)?;
                if request[14] & 1 != 0 {
                    check(request[15] as i32, protected)?;
                }
                Ok(())
            }
        })();
        if let Err(error) = inspected {
            if error == libc::EBADF {
                if index != 0 {
                    args[count_index] = index;
                } else {
                    args[count_index] = 0;
                    let validation = unsafe { raw_syscall6(number, args) };
                    return Err(if validation < 0 {
                        validation
                    } else {
                        -i64::from(error)
                    });
                }
            }
            break;
        }
    }
    Ok(args)
}

pub(crate) fn indirect_result(number: i64, args: [u64; 6], protected: &[i32]) -> Option<i64> {
    if protected.iter().all(|fd| *fd < 0) {
        return None;
    }
    inspect(number, args, protected)
        .err()
        .map(|error| -i64::from(error))
}

#[cfg(test)]
mod tests {
    use std::mem::size_of_val;

    use super::*;

    #[test]
    fn guest_log_scm_rights_checks_only_transferred_descriptors() {
        let mut control = [0_u64; 3];
        let header = libc::cmsghdr {
            cmsg_len: size_of::<libc::cmsghdr>() + 4,
            cmsg_level: libc::SOL_SOCKET,
            cmsg_type: libc::SCM_RIGHTS,
        };
        unsafe {
            (control.as_mut_ptr() as *mut libc::cmsghdr).write(header);
        }
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = size_of_val(&control);
        for fd in [201_i32, 202] {
            control[2] = fd as u64;
            let result = indirect_result(
                libc::SYS_sendmsg,
                [10, (&raw const message) as u64, 0, 0, 0, 0],
                &[202],
            );
            assert_eq!(
                result,
                if fd == 202 {
                    Some(-i64::from(libc::EBADF))
                } else {
                    None
                }
            );
        }
    }

    #[test]
    fn guest_log_aio_and_ring_registration_neighboring_fds() {
        let mut context = 0_u64;
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_io_setup,
                    [1, (&raw mut context) as u64, 0, 0, 0, 0],
                )
            },
            0
        );
        for fd in [201_i32, 202] {
            let expected = if fd == 202 {
                Some(-i64::from(libc::EBADF))
            } else {
                None
            };
            let mut request = [0_u32; 16];
            request[5] = fd as u32;
            let pointers = [request.as_ptr() as u64];
            assert_eq!(
                batch_args(
                    libc::SYS_io_submit,
                    [context, 1, pointers.as_ptr() as u64, 0, 0, 0],
                    &[202]
                )
                .err(),
                expected
            );
            let descriptors = [fd, -1, -2];
            assert_eq!(
                indirect_result(
                    libc::SYS_io_uring_register,
                    [10, 2, descriptors.as_ptr() as u64, 3, 0, 0],
                    &[202]
                ),
                expected
            );
            assert_eq!(
                indirect_result(libc::SYS_io_uring_register, [10, 0, 0, 0, 0, 0], &[202]),
                None
            );
        }
        assert_eq!(
            indirect_result(libc::SYS_sendmsg, [10, 1, 0, 0, 0, 0], &[202]),
            Some(-i64::from(libc::EFAULT))
        );
        assert_eq!(
            unsafe { raw_syscall6(libc::SYS_io_destroy, [context, 0, 0, 0, 0, 0]) },
            0
        );
    }

    fn submit_batch(number: i64, args: [u64; 6], protected: &[i32]) -> i64 {
        match batch_args(number, args, protected) {
            Ok(args) => unsafe { raw_syscall6(number, args) },
            Err(error) => error,
        }
    }

    #[test]
    fn sendmmsg_first_protected_preserves_socket_validation() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixDatagram;

        let reserved = tempfile::tempfile().unwrap();
        let ordinary = tempfile::tempfile().unwrap();
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        let mut control = [0_u64; 3];
        let header = libc::cmsghdr {
            cmsg_len: size_of::<libc::cmsghdr>() + 4,
            cmsg_level: libc::SOL_SOCKET,
            cmsg_type: libc::SCM_RIGHTS,
        };
        unsafe { control.as_mut_ptr().cast::<libc::cmsghdr>().write(header) };
        let mut byte = b'x';
        let mut vector = libc::iovec {
            iov_base: (&raw mut byte).cast(),
            iov_len: 1,
        };
        let mut message: libc::mmsghdr = unsafe { std::mem::zeroed() };
        message.msg_hdr.msg_iov = &raw mut vector;
        message.msg_hdr.msg_iovlen = 1;
        message.msg_hdr.msg_control = control.as_mut_ptr().cast();
        message.msg_hdr.msg_controllen = size_of_val(&control);
        for (destination, expected) in [
            (-1, libc::EBADF),
            (ordinary.as_raw_fd(), libc::ENOTSOCK),
            (sender.as_raw_fd(), libc::EBADF),
        ] {
            let args = [destination as u64, (&raw mut message) as u64, 1, 0, 0, 0];
            control[2] = u32::MAX as u64;
            assert_eq!(
                unsafe { raw_syscall6(libc::SYS_sendmmsg, args) },
                -i64::from(expected)
            );
            control[2] = reserved.as_raw_fd() as u64;
            assert_eq!(
                submit_batch(libc::SYS_sendmmsg, args, &[reserved.as_raw_fd()]),
                -i64::from(expected)
            );
            assert_eq!(message.msg_len, 0);
        }
        receiver.set_nonblocking(true).unwrap();
        assert_eq!(
            receiver.recv(&mut [0; 1]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        control[2] = ordinary.as_raw_fd() as u64;
        message.msg_hdr.msg_control = control.as_mut_ptr().cast();
        assert_eq!(
            submit_batch(
                libc::SYS_sendmmsg,
                [
                    sender.as_raw_fd() as u64,
                    (&raw mut message) as u64,
                    1,
                    0,
                    0,
                    0
                ],
                &[reserved.as_raw_fd()]
            ),
            1
        );
        let mut received = [0; 1];
        assert_eq!(receiver.recv(&mut received).unwrap(), 1);
        assert_eq!(received, [byte]);
    }

    #[test]
    fn aio_first_protected_preserves_context_validation() {
        use std::os::fd::AsRawFd;

        let reserved = tempfile::tempfile().unwrap();
        let mut context = 0_u64;
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_io_setup,
                    [1, (&raw mut context) as u64, 0, 0, 0, 0],
                )
            },
            0
        );
        let byte = b'x';
        let mut request: libc::iocb = unsafe { std::mem::zeroed() };
        request.aio_lio_opcode = 1;
        request.aio_buf = (&raw const byte) as u64;
        request.aio_nbytes = 1;
        for (candidate, expected) in [(0, libc::EINVAL), (1, libc::EINVAL), (context, libc::EBADF)]
        {
            request.aio_fildes = u32::MAX;
            let pointers = [(&raw const request) as u64];
            let args = [candidate, 1, pointers.as_ptr() as u64, 0, 0, 0];
            assert_eq!(
                unsafe { raw_syscall6(libc::SYS_io_submit, args) },
                -i64::from(expected)
            );
            request.aio_fildes = reserved.as_raw_fd() as u32;
            let pointers = [(&raw const request) as u64];
            let args = [candidate, 1, pointers.as_ptr() as u64, 0, 0, 0];
            assert_eq!(
                submit_batch(libc::SYS_io_submit, args, &[reserved.as_raw_fd()]),
                -i64::from(expected)
            );
        }
        assert_eq!(reserved.metadata().unwrap().len(), 0);
        assert_eq!(
            unsafe { raw_syscall6(libc::SYS_io_destroy, [context, 0, 0, 0, 0, 0]) },
            0
        );
    }

    #[test]
    fn sendmmsg_preserves_native_prefix_and_ignored_control() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixDatagram;

        for protected_later in [false, true] {
            for guarded in [false, true] {
                let (sender, receiver) = UnixDatagram::pair().unwrap();
                let reserved = tempfile::tempfile().unwrap();
                let mut byte = b'x';
                let mut vector = libc::iovec {
                    iov_base: (&raw mut byte).cast(),
                    iov_len: 1,
                };
                let mut messages: [libc::mmsghdr; 2] = unsafe { std::mem::zeroed() };
                for message in &mut messages {
                    message.msg_hdr.msg_iov = &raw mut vector;
                    message.msg_hdr.msg_iovlen = 1;
                }
                let mut control = [0_u64; 3];
                let header = libc::cmsghdr {
                    cmsg_len: 20,
                    cmsg_level: libc::SOL_SOCKET,
                    cmsg_type: libc::SCM_RIGHTS,
                };
                unsafe { control.as_mut_ptr().cast::<libc::cmsghdr>().write(header) };
                control[2] = if guarded {
                    reserved.as_raw_fd() as u64
                } else {
                    u32::MAX as u64
                };
                messages[1].msg_hdr.msg_control = if protected_later {
                    control.as_mut_ptr().cast()
                } else {
                    std::ptr::dangling_mut()
                };
                messages[1].msg_hdr.msg_controllen = 24;
                let args = [
                    sender.as_raw_fd() as u64,
                    messages.as_mut_ptr() as u64,
                    2,
                    0,
                    0,
                    0,
                ];
                let result = if guarded {
                    submit_batch(libc::SYS_sendmmsg, args, &[reserved.as_raw_fd()])
                } else {
                    unsafe { raw_syscall6(libc::SYS_sendmmsg, args) }
                };
                assert_eq!(result, 1);
                assert_eq!(messages[0].msg_len, 1);
                assert_eq!(messages[1].msg_len, 0);
                let mut received = [0; 1];
                assert_eq!(receiver.recv(&mut received).unwrap(), 1);
                assert_eq!(received, [b'x']);
                receiver.set_nonblocking(true).unwrap();
                assert_eq!(
                    receiver.recv(&mut received).unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                messages[1].msg_hdr.msg_controllen = 0;
                assert_eq!(
                    submit_batch(libc::SYS_sendmmsg, args, &[reserved.as_raw_fd()]),
                    2
                );
                assert_eq!(
                    submit_batch(
                        libc::SYS_sendmmsg,
                        [sender.as_raw_fd() as u64, 1, 0, 0, 0, 0],
                        &[reserved.as_raw_fd()]
                    ),
                    0
                );
            }
        }
        let mut messages: Vec<libc::mmsghdr> =
            (0..1025).map(|_| unsafe { std::mem::zeroed() }).collect();
        messages[1024].msg_hdr.msg_control = std::ptr::dangling_mut();
        messages[1024].msg_hdr.msg_controllen = 24;
        let args = [0, messages.as_ptr() as u64, 1025, 0, 0, 0];
        assert_eq!(batch_args(libc::SYS_sendmmsg, args, &[202]), Ok(args));
    }

    #[test]
    fn aio_preserves_native_prefix_and_ignored_resfd() {
        use std::io::Read;
        use std::io::Seek;
        use std::io::SeekFrom;
        use std::os::fd::AsRawFd;

        for protected_later in [false, true] {
            for guarded in [false, true] {
                let mut file = tempfile::tempfile().unwrap();
                let reserved = tempfile::tempfile().unwrap();
                let mut context = 0_u64;
                assert_eq!(
                    unsafe {
                        raw_syscall6(
                            libc::SYS_io_setup,
                            [2, (&raw mut context) as u64, 0, 0, 0, 0],
                        )
                    },
                    0
                );
                let bytes = *b"AB";
                let mut requests: [libc::iocb; 2] = unsafe { std::mem::zeroed() };
                for (index, request) in requests.iter_mut().enumerate() {
                    request.aio_lio_opcode = 1;
                    request.aio_fildes = file.as_raw_fd() as u32;
                    request.aio_buf = (&bytes[index]) as *const u8 as u64;
                    request.aio_nbytes = 1;
                    request.aio_offset = index as i64;
                    request.aio_resfd = reserved.as_raw_fd() as u32;
                }
                if protected_later {
                    requests[1].aio_fildes = if guarded {
                        reserved.as_raw_fd() as u32
                    } else {
                        u32::MAX
                    };
                }
                let pointers = [
                    (&raw const requests[0]) as u64,
                    if protected_later {
                        (&raw const requests[1]) as u64
                    } else {
                        1
                    },
                ];
                let args = [context, 2, pointers.as_ptr() as u64, 0, 0, 0];
                let result = if guarded {
                    submit_batch(libc::SYS_io_submit, args, &[reserved.as_raw_fd()])
                } else {
                    unsafe { raw_syscall6(libc::SYS_io_submit, args) }
                };
                assert_eq!(result, 1);
                assert_eq!(
                    submit_batch(
                        libc::SYS_io_submit,
                        [context, 0, 1, 0, 0, 0],
                        &[reserved.as_raw_fd()]
                    ),
                    0
                );
                assert_eq!(
                    submit_batch(
                        libc::SYS_io_submit,
                        [context, u64::MAX, 1, 0, 0, 0],
                        &[reserved.as_raw_fd()]
                    ),
                    -i64::from(libc::EINVAL)
                );
                assert_eq!(
                    submit_batch(
                        libc::SYS_io_submit,
                        [0, 1, 1, 0, 0, 0],
                        &[reserved.as_raw_fd()]
                    ),
                    -i64::from(libc::EINVAL)
                );
                assert_eq!(
                    unsafe { raw_syscall6(libc::SYS_io_destroy, [context, 0, 0, 0, 0, 0]) },
                    0
                );
                file.seek(SeekFrom::Start(0)).unwrap();
                let mut contents = Vec::new();
                file.read_to_end(&mut contents).unwrap();
                assert_eq!(contents, b"A");
            }
        }
    }
}
