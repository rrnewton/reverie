use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;
use std::sync::atomic::Ordering;

use super::*;

const OPERATIONS: [i64; 9] = [
    libc::SYS_lseek,
    libc::SYS_fsync,
    libc::SYS_ftruncate,
    libc::SYS_write,
    libc::SYS_writev,
    libc::SYS_pwritev,
    libc::SYS_pwrite64,
    libc::SYS_readv,
    libc::SYS_preadv,
];

fn effect(number: i64, args: [u64; 6]) -> i64 {
    assert_eq!(
        classify_owned_injection(true, number),
        OwnedInjection::Admitted
    );
    assert_eq!(injected_syscall_guard(number, args), None);
    guarded_raw_injection(number, args)
}

fn vector(buffer: &mut [u8]) -> libc::iovec {
    libc::iovec {
        iov_base: buffer.as_mut_ptr().cast(),
        iov_len: buffer.len(),
    }
}

#[test]
fn admission_covers_returning_file_effects_without_unrelated_operations() {
    for number in [libc::SYS_unlink, libc::SYS_newfstatat] {
        assert_eq!(
            classify_owned_injection(true, number),
            OwnedInjection::Admitted
        );
    }
    for number in OPERATIONS {
        assert!(crate::syscall_event::observable(number));
        assert!(crate::syscall_event::injectable(number));
        assert!(!crate::syscall_event::backed_returning(number));
        assert!(!crate::mapping::operation(number));
        assert_eq!(
            classify_owned_injection(true, number),
            OwnedInjection::Admitted
        );
        assert_eq!(
            classify_owned_injection(true, number | 0x4000_0000),
            OwnedInjection::Terminal
        );
    }
    for number in [
        libc::SYS_preadv2,
        libc::SYS_pwritev2,
        libc::SYS_fcntl,
        libc::SYS_readlink,
        libc::SYS_truncate,
        libc::SYS_fdatasync,
        libc::SYS_clone,
        libc::SYS_execve,
        libc::SYS_rt_sigaction,
        libc::SYS_futex,
    ] {
        assert_eq!(
            classify_owned_injection(true, number),
            OwnedInjection::Terminal
        );
        assert!(!crate::syscall_event::backed_returning(number));
    }
}

#[test]
fn sync_truncate_fsync_preserves_data_position_and_kernel_errors() {
    assert_eq!(
        classify_owned_injection(true, libc::SYS_fsync),
        OwnedInjection::Admitted
    );
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"sync bytes").unwrap();
    file.seek(SeekFrom::Start(3)).unwrap();
    let read_only = std::fs::File::open(file.path()).unwrap();
    let mut write_only = std::fs::OpenOptions::new()
        .write(true)
        .open(file.path())
        .unwrap();
    write_only.seek(SeekFrom::Start(5)).unwrap();
    for descriptor in [
        file.as_raw_fd(),
        read_only.as_raw_fd(),
        write_only.as_raw_fd(),
    ] {
        for high in [0, 1_u64 << 32] {
            assert_eq!(
                effect(
                    libc::SYS_fsync,
                    [high | descriptor as u64, 11, 22, 33, 44, 55]
                ),
                0
            );
        }
    }
    assert_eq!(
        effect(libc::SYS_fsync, [u64::MAX, 0, 0, 0, 0, 0]),
        -i64::from(libc::EBADF)
    );
    assert_eq!(std::fs::read(file.path()).unwrap(), b"sync bytes");
    assert_eq!(file.stream_position().unwrap(), 3);
    assert_eq!(write_only.stream_position().unwrap(), 5);
    assert_eq!(read_only.metadata().unwrap().len(), 10);
    drop((read_only, write_only));
    file.close().unwrap();
}

#[test]
fn sync_truncate_ftruncate_preserves_prefix_position_and_kernel_errors() {
    assert_eq!(
        classify_owned_injection(true, libc::SYS_ftruncate),
        OwnedInjection::Admitted
    );
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"abcdefghij").unwrap();
    let read_only = std::fs::File::open(file.path()).unwrap();
    let mut write_only = std::fs::OpenOptions::new()
        .write(true)
        .open(file.path())
        .unwrap();
    write_only.seek(SeekFrom::Start(8)).unwrap();
    assert_eq!(
        effect(
            libc::SYS_ftruncate,
            [(1_u64 << 32) | file.as_raw_fd() as u64, 4, 22, 33, 44, 55]
        ),
        0
    );
    assert_eq!(std::fs::read(file.path()).unwrap(), b"abcd");
    assert_eq!(file.stream_position().unwrap(), 10);
    assert_eq!(
        effect(
            libc::SYS_ftruncate,
            [write_only.as_raw_fd() as u64, 7, 22, 33, 44, 55]
        ),
        0
    );
    assert_eq!(std::fs::read(file.path()).unwrap(), b"abcd\0\0\0");
    for (descriptor, length, expected) in [
        (u64::MAX, 0, libc::EBADF),
        (read_only.as_raw_fd() as u64, 0, libc::EINVAL),
        (
            (1_u64 << 32) | read_only.as_raw_fd() as u64,
            0,
            libc::EINVAL,
        ),
        (file.as_raw_fd() as u64, u64::MAX, libc::EINVAL),
    ] {
        assert_eq!(
            effect(libc::SYS_ftruncate, [descriptor, length, 0, 0, 0, 0]),
            -i64::from(expected)
        );
        assert_eq!(std::fs::read(file.path()).unwrap(), b"abcd\0\0\0");
    }
    assert_eq!(file.stream_position().unwrap(), 10);
    assert_eq!(write_only.stream_position().unwrap(), 8);
    drop((read_only, write_only));
    file.close().unwrap();
}

#[test]
fn unlink_owned_route_preserves_path_result_and_open_file() {
    use std::os::unix::ffi::OsStrExt;

    assert_eq!(
        classify_owned_injection(true, libc::SYS_unlink),
        OwnedInjection::Admitted
    );
    assert!(!crate::mapping::operation(libc::SYS_unlink));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("linked");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    file.write_all(b"held bytes").unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let original = name.as_bytes_with_nul().to_vec();
    let args = [name.as_ptr() as u64, 11, 22, 33, 44, 55];
    assert_eq!(effect(libc::SYS_unlink, args), 0);
    assert!(!path.exists());
    let mut bytes = [0; 10];
    file.read_exact_at(&mut bytes, 0).unwrap();
    assert_eq!(&bytes, b"held bytes");
    assert_eq!(effect(libc::SYS_unlink, args), -i64::from(libc::ENOENT));
    assert_eq!(effect(libc::SYS_unlink, [0; 6]), -i64::from(libc::EFAULT));
    assert_eq!(name.as_bytes_with_nul(), original);
    drop(file);
    directory.close().unwrap();
}

#[test]
fn pwritev_owned_route_preserves_vectors_offset_position_and_errors() {
    assert_eq!(
        classify_owned_injection(true, libc::SYS_pwritev),
        OwnedInjection::Admitted
    );
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"abcdefghij").unwrap();
    let descriptor = (1_u64 << 32) | file.as_raw_fd() as u64;
    let mut first = *b"WX";
    let mut second = *b"YZ";
    let vectors = [vector(&mut first), vector(&mut second)];
    let args = [descriptor, vectors.as_ptr() as u64, 2, 4, 0, 0];
    assert_eq!(effect(libc::SYS_pwritev, args), 4);
    assert_eq!(std::fs::read(file.path()).unwrap(), b"abcdWXYZij");
    assert_eq!(file.stream_position().unwrap(), 10);
    assert_eq!(first, *b"WX");
    assert_eq!(second, *b"YZ");
    let read_only = std::fs::File::open(file.path()).unwrap();
    for invalid in [u64::MAX, read_only.as_raw_fd() as u64] {
        let mut denied = args;
        denied[0] = invalid;
        assert_eq!(effect(libc::SYS_pwritev, denied), -i64::from(libc::EBADF));
    }
    assert_eq!(
        effect(libc::SYS_pwritev, [descriptor, 0, 1, 4, 0, 0]),
        -i64::from(libc::EFAULT)
    );
    assert_eq!(effect(libc::SYS_pwritev, [descriptor, 0, 0, 4, 0, 0]), 0);
    let mut negative = args;
    negative[3] = u64::MAX;
    assert_eq!(
        effect(libc::SYS_pwritev, negative),
        -i64::from(libc::EINVAL)
    );
    assert_eq!(std::fs::read(file.path()).unwrap(), b"abcdWXYZij");
    assert_eq!(file.stream_position().unwrap(), 10);
    drop(read_only);
    file.close().unwrap();
}

#[test]
fn seek_preserves_signed_full_width_offsets_and_linux_fd_identity() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"0123456789").unwrap();
    let descriptor = (1_u64 << 32) | file.as_raw_fd() as u64;
    for (offset, whence, expected) in [
        (3, libc::SEEK_SET, 3),
        ((-2_i64) as u64, libc::SEEK_CUR, 1),
        ((-1_i64) as u64, libc::SEEK_END, 9),
        (0x1_0000_0007, libc::SEEK_SET, 0x1_0000_0007),
        ((-4_i64) as u64, libc::SEEK_CUR, 0x1_0000_0003),
    ] {
        assert_eq!(
            effect(
                libc::SYS_lseek,
                [descriptor, offset, whence as u64, 0, 0, 0]
            ),
            expected
        );
        assert_eq!(file.stream_position().unwrap(), expected as u64);
    }
    for (descriptor, offset, whence, expected) in [
        (descriptor, u64::MAX, libc::SEEK_SET, -libc::EINVAL),
        (descriptor, 0, 12345, -libc::EINVAL),
        (u64::MAX, 0, libc::SEEK_SET, -libc::EBADF),
    ] {
        assert_eq!(
            effect(
                libc::SYS_lseek,
                [descriptor, offset, whence as u64, 0, 0, 0]
            ),
            i64::from(expected)
        );
        assert_eq!(file.stream_position().unwrap(), 0x1_0000_0003);
    }
    let (pipe, peer) = std::os::unix::net::UnixStream::pair().unwrap();
    assert_eq!(
        effect(libc::SYS_lseek, [pipe.as_raw_fd() as u64, 0, 0, 0, 0, 0]),
        -i64::from(libc::ESPIPE)
    );
    drop((pipe, peer));
    file.close().unwrap();
}

#[test]
fn scalar_writes_preserve_data_position_sparse_offsets_and_append() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"abcdefgh").unwrap();
    file.seek(SeekFrom::Start(2)).unwrap();
    let descriptor = (1_u64 << 32) | file.as_raw_fd() as u64;
    assert_eq!(
        effect(
            libc::SYS_write,
            [descriptor, b"XY".as_ptr() as u64, 2, 0, 0, 0]
        ),
        2
    );
    assert_eq!(file.stream_position().unwrap(), 4);
    assert_eq!(
        effect(
            libc::SYS_pwrite64,
            [descriptor, b"pq".as_ptr() as u64, 2, 6, 0, 0]
        ),
        2
    );
    assert_eq!(file.stream_position().unwrap(), 4);
    let mut actual = [0; 8];
    file.as_file().read_exact_at(&mut actual, 0).unwrap();
    assert_eq!(&actual, b"abXYefpq");
    let offset = 0x1_0000_0009;
    assert_eq!(
        effect(
            libc::SYS_pwrite64,
            [descriptor, b"wide".as_ptr() as u64, 4, offset, 0, 0]
        ),
        4
    );
    assert_eq!(file.stream_position().unwrap(), 4);
    let mut tail = [0xa5; 6];
    file.as_file().read_exact_at(&mut tail, offset - 2).unwrap();
    assert_eq!(&tail, b"\0\0wide");
    assert_eq!(file.as_file().metadata().unwrap().len(), offset + 4);
    file.close().unwrap();

    let mut appended = tempfile::NamedTempFile::new().unwrap();
    appended.write_all(b"base").unwrap();
    let mut handle = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .open(appended.path())
        .unwrap();
    handle.seek(SeekFrom::Start(1)).unwrap();
    let descriptor = handle.as_raw_fd() as u64;
    assert_eq!(
        effect(
            libc::SYS_write,
            [descriptor, b"A".as_ptr() as u64, 1, 0, 0, 0]
        ),
        1
    );
    assert_eq!(handle.stream_position().unwrap(), 5);
    assert_eq!(
        effect(
            libc::SYS_pwrite64,
            [descriptor, b"B".as_ptr() as u64, 1, 0, 0, 0]
        ),
        1
    );
    assert_eq!(handle.stream_position().unwrap(), 5);
    assert_eq!(std::fs::read(appended.path()).unwrap(), b"baseAB");
    drop(handle);
    appended.close().unwrap();
}

#[test]
fn vectors_preserve_gather_scatter_short_eof_and_position() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    let descriptor = (1_u64 << 32) | file.as_raw_fd() as u64;
    let mut first = *b"ab";
    let mut second = *b"cde";
    let vectors = [vector(&mut first), vector(&mut []), vector(&mut second)];
    assert_eq!(
        effect(
            libc::SYS_writev,
            [descriptor, vectors.as_ptr() as u64, 3, 0, 0, 0]
        ),
        5
    );
    assert_eq!(file.stream_position().unwrap(), 5);
    assert_eq!(std::fs::read(file.path()).unwrap(), b"abcde");
    file.seek(SeekFrom::Start(0)).unwrap();
    for (expected_count, expected_first, expected_second) in [
        (5, *b"ab", [b'c', b'd', b'e', 0xa5]),
        (0, [0xa5; 2], [0xa5; 4]),
    ] {
        let mut first = [0xa5; 2];
        let mut second = [0xa5; 4];
        let vectors = [vector(&mut first), vector(&mut second)];
        assert_eq!(
            effect(
                libc::SYS_readv,
                [descriptor, vectors.as_ptr() as u64, 2, 0, 0, 0]
            ),
            expected_count
        );
        assert_eq!(first, expected_first);
        assert_eq!(second, expected_second);
        assert_eq!(file.stream_position().unwrap(), 5);
    }
    let offset = 0x1_0000_0003;
    file.as_file().write_all_at(b"wide", offset).unwrap();
    for high in [0, u64::MAX] {
        let mut first = [0xa5; 2];
        let mut second = [0xa5; 4];
        let vectors = [vector(&mut first), vector(&mut second)];
        assert_eq!(
            effect(
                libc::SYS_preadv,
                [descriptor, vectors.as_ptr() as u64, 2, offset, high, 0]
            ),
            4
        );
        assert_eq!(&first, b"wi");
        assert_eq!(second, [b'd', b'e', 0xa5, 0xa5]);
        assert_eq!(file.stream_position().unwrap(), 5);
        assert_eq!(
            effect(
                libc::SYS_preadv,
                [descriptor, vectors.as_ptr() as u64, 2, offset + 4, high, 0]
            ),
            0
        );
        assert_eq!(second, [b'd', b'e', 0xa5, 0xa5]);
    }
    file.close().unwrap();
}

#[test]
fn invalid_vectors_and_partial_faults_preserve_kernel_effects() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"data").unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let descriptor = file.as_raw_fd() as u64;
    let mut data = [0xa5; 4];
    let invalid = libc::iovec {
        iov_base: std::ptr::null_mut(),
        iov_len: 1,
    };
    let oversized = libc::iovec {
        iov_base: data.as_mut_ptr().cast(),
        iov_len: usize::MAX,
    };
    for number in [libc::SYS_readv, libc::SYS_writev, libc::SYS_preadv] {
        for (pointer, count, expected) in [
            (0, 0, 0),
            (0, 1, -libc::EFAULT),
            (&invalid as *const libc::iovec as u64, 1, -libc::EFAULT),
            (&invalid as *const libc::iovec as u64, 1025, -libc::EINVAL),
            (&oversized as *const libc::iovec as u64, 1, -libc::EINVAL),
        ] {
            assert_eq!(
                effect(number, [descriptor, pointer, count, 0, 0, 0]),
                i64::from(expected),
                "number={number} count={count}"
            );
            assert_eq!(data, [0xa5; 4]);
            assert_eq!(file.stream_position().unwrap(), 0);
            assert_eq!(std::fs::read(file.path()).unwrap(), b"data");
        }
    }
    let mut prefix = *b"XY";
    let vectors = [vector(&mut prefix), invalid];
    let mut control = tempfile::NamedTempFile::new().unwrap();
    control.write_all(b"data").unwrap();
    control.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(
        control.stream_position().unwrap(),
        file.stream_position().unwrap()
    );
    assert_eq!(
        std::fs::read(control.path()).unwrap(),
        std::fs::read(file.path()).unwrap()
    );
    assert_eq!(
        unsafe { libc::fcntl(control.as_raw_fd(), libc::F_GETFL) },
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) }
    );
    let direct = unsafe {
        raw_syscall6(
            libc::SYS_writev,
            [
                control.as_raw_fd() as u64,
                vectors.as_ptr() as u64,
                2,
                0,
                0,
                0,
            ],
        )
    };
    assert_eq!(
        effect(
            libc::SYS_writev,
            [descriptor, vectors.as_ptr() as u64, 2, 0, 0, 0]
        ),
        direct
    );
    assert_eq!(
        file.stream_position().unwrap(),
        control.stream_position().unwrap()
    );
    assert_eq!(
        std::fs::read(file.path()).unwrap(),
        std::fs::read(control.path()).unwrap()
    );
    control.close().unwrap();
    file.as_file().write_all_at(b"data", 0).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut prefix = [0xa5; 2];
    let vectors = [vector(&mut prefix), invalid];
    assert_eq!(
        effect(
            libc::SYS_readv,
            [descriptor, vectors.as_ptr() as u64, 2, 0, 0, 0]
        ),
        2
    );
    assert_eq!(&prefix, b"da");
    assert_eq!(file.stream_position().unwrap(), 2);
    prefix.fill(0xa5);
    assert_eq!(
        effect(
            libc::SYS_preadv,
            [descriptor, vectors.as_ptr() as u64, 2, 1, 0, 0]
        ),
        2
    );
    assert_eq!(&prefix, b"at");
    assert_eq!(file.stream_position().unwrap(), 2);
    file.close().unwrap();
}

#[test]
fn nonblocking_output_has_exact_short_write_and_full_pipe_effects() {
    for number in [libc::SYS_write, libc::SYS_writev] {
        let mut descriptors = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) },
            0
        );
        let reader = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
        let writer = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
        let capacity = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETPIPE_SZ) };
        assert!(capacity >= 4096);
        let capacity = capacity as usize;
        let mut payload = vec![0x6b_u8; capacity + 1];
        payload[capacity] = 0xee;
        let vectors = [
            libc::iovec {
                iov_base: payload.as_mut_ptr().cast(),
                iov_len: capacity,
            },
            libc::iovec {
                iov_base: unsafe { payload.as_mut_ptr().add(capacity) }.cast(),
                iov_len: 1,
            },
        ];
        let args = if number == libc::SYS_write {
            [
                writer.as_raw_fd() as u64,
                payload.as_ptr() as u64,
                payload.len() as u64,
                0,
                0,
                0,
            ]
        } else {
            [
                writer.as_raw_fd() as u64,
                vectors.as_ptr() as u64,
                2,
                0,
                0,
                0,
            ]
        };
        assert_eq!(effect(number, args), capacity as i64);
        assert_eq!(
            effect(
                libc::SYS_write,
                [
                    writer.as_raw_fd() as u64,
                    payload.as_ptr() as u64,
                    1,
                    0,
                    0,
                    0
                ]
            ),
            -i64::from(libc::EAGAIN)
        );
        let mut reader = std::fs::File::from(reader);
        let mut actual = vec![0; capacity];
        reader.read_exact(&mut actual).unwrap();
        assert_eq!(actual, vec![0x6b; capacity]);
        let mut extra = [0xa5];
        assert_eq!(
            reader.read(&mut extra).unwrap_err().raw_os_error(),
            Some(libc::EAGAIN)
        );
        assert_eq!(extra, [0xa5]);
        drop((reader, writer));
    }
}

#[test]
fn access_mode_bad_fd_and_scalar_fault_errors_are_not_fabricated() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"data").unwrap();
    file.seek(SeekFrom::Start(2)).unwrap();
    let read_only = std::fs::File::open(file.path()).unwrap();
    let write_only = std::fs::OpenOptions::new()
        .write(true)
        .open(file.path())
        .unwrap();
    let mut data = [0xa5; 4];
    let vectors = [vector(&mut data)];
    for number in [
        libc::SYS_write,
        libc::SYS_writev,
        libc::SYS_pwrite64,
        libc::SYS_readv,
        libc::SYS_preadv,
    ] {
        let pointer = if matches!(number, libc::SYS_write | libc::SYS_pwrite64) {
            data.as_ptr() as u64
        } else {
            vectors.as_ptr() as u64
        };
        let wrong_mode = if matches!(number, libc::SYS_readv | libc::SYS_preadv) {
            write_only.as_raw_fd()
        } else {
            read_only.as_raw_fd()
        };
        for descriptor in [u64::MAX, wrong_mode as u64] {
            assert_eq!(
                effect(number, [descriptor, pointer, 1, 0, 0, 0]),
                -i64::from(libc::EBADF)
            );
        }
    }
    for number in [libc::SYS_write, libc::SYS_pwrite64] {
        let descriptor = file.as_raw_fd() as u64;
        assert_eq!(
            effect(number, [descriptor, 0, 1, 0, 0, 0]),
            -i64::from(libc::EFAULT)
        );
        assert_eq!(effect(number, [descriptor, 0, 0, 0, 0, 0]), 0);
        assert_eq!(
            effect(number, [u64::MAX, 0, 0, 0, 0, 0]),
            -i64::from(libc::EBADF)
        );
    }
    for (number, pointer) in [
        (libc::SYS_pwrite64, data.as_ptr() as u64),
        (libc::SYS_preadv, vectors.as_ptr() as u64),
    ] {
        assert_eq!(
            effect(
                number,
                [file.as_raw_fd() as u64, pointer, 1, u64::MAX, 0, 0]
            ),
            -i64::from(libc::EINVAL)
        );
    }
    assert_eq!(data, [0xa5; 4]);
    assert_eq!(file.stream_position().unwrap(), 2);
    assert_eq!(std::fs::read(file.path()).unwrap(), b"data");
    drop((read_only, write_only));
    file.close().unwrap();
}

#[test]
fn protected_descriptors_keep_bytes_buffers_and_position_unchanged() {
    const CHILD: &str = "LITEINST_FILE_IO_PROTECTED_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tool_host::file_io_tests::protected_descriptors_keep_bytes_buffers_and_position_unchanged", "--test-threads=1"])
            .env(CHILD, "1").output().unwrap();
        assert!(output.status.success(), "{output:?}");
        return;
    }
    struct Restore(i32);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::guest_log::LOG_FD.store(self.0, Ordering::Release);
        }
    }
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"private").unwrap();
    file.seek(SeekFrom::Start(2)).unwrap();
    let restore = Restore(crate::guest_log::LOG_FD.swap(file.as_raw_fd(), Ordering::AcqRel));
    let mut data = [0xa5; 8];
    let vectors = [vector(&mut data)];
    for descriptor in [
        file.as_raw_fd() as u64,
        (1_u64 << 32) | file.as_raw_fd() as u64,
    ] {
        for number in OPERATIONS {
            for count in [0, 1] {
                let args = match number {
                    libc::SYS_lseek => [descriptor, 0, libc::SEEK_SET as u64, 0, 0, 0],
                    libc::SYS_fsync | libc::SYS_ftruncate => [descriptor, count, 0, 0, 0, 0],
                    libc::SYS_write | libc::SYS_pwrite64 => {
                        [descriptor, data.as_ptr() as u64, count, 0, 0, 0]
                    }
                    _ => [descriptor, vectors.as_ptr() as u64, count, 0, 0, 0],
                };
                assert_eq!(
                    injected_syscall_guard(number, args),
                    Some(-i64::from(libc::EBADF))
                );
                assert_eq!(guarded_raw_injection(number, args), -i64::from(libc::EBADF));
                assert_eq!(data, [0xa5; 8]);
                assert_eq!(file.stream_position().unwrap(), 2);
                assert_eq!(std::fs::read(file.path()).unwrap(), b"private");
            }
        }
    }
    drop(restore);
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut actual = Vec::new();
    file.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, b"private");
    file.close().unwrap();
}
