/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Included in executor::tests so the controls use the actual dispatch and its
// existing owned descriptor/memory fixtures, without exporting test-only APIs.

/// Invoke the x86-64 ABI without a C `int` parameter truncating the fd first.
///
/// # Safety
/// Callers own every accessible buffer/descriptor, and use only the explicit
/// bounded read/write cases below. Deliberately invalid pointers are submitted
/// to the kernel, never dereferenced by Rust.
unsafe fn scalar_fd_native_call(number: libc::c_long, args: [u64; 6]) -> i64 {
    let result: i64;
    // SAFETY: the caller supplies the syscall's complete argument/lifetime
    // contract. The default asm memory clobber includes kernel buffer writes.
    unsafe {
        std::arch::asm!(
            "syscall",
            inlateout("rax") number => result,
            in("rdi") args[0], in("rsi") args[1], in("rdx") args[2],
            in("r10") args[3], in("r8") args[4], in("r9") args[5],
            lateout("rcx") _, lateout("r11") _,
            options(nostack),
        );
    }
    eprintln!("native number={number} args={args:016x?} result={result}");
    result
}

#[derive(Debug, Eq, PartialEq)]
struct ScalarFdObservation {
    result: i64,
    buffer: [u8; 8],
    contents: [u8; 8],
    position: u64,
    alias_position: u64,
}

fn scalar_fd_initial_buffer(number: libc::c_long) -> [u8; 8] {
    if number == libc::SYS_pwrite64 {
        *b"xyzUUUUU"
    } else {
        *b"UUUUUUUU"
    }
}

fn scalar_fd_expected(number: libc::c_long, case: &str) -> ScalarFdObservation {
    let mut expected = ScalarFdObservation {
        result: 0,
        buffer: scalar_fd_initial_buffer(number),
        contents: *b"ABCDEFGH",
        position: 1,
        alias_position: 1,
    };
    match case {
        "zero" => return expected,
        "fault" => expected.result = negative_errno(libc::EFAULT),
        "invalid" | "closed" => expected.result = negative_errno(libc::EBADF),
        "negative-offset" if number != libc::SYS_read => {
            expected.result = negative_errno(libc::EINVAL);
        }
        "data" | "negative-offset" => {
            expected.result = 3;
            if number == libc::SYS_read {
                expected.buffer = *b"BCDUUUUU";
                expected.position = 4;
                expected.alias_position = 4;
            } else if number == libc::SYS_pread64 {
                expected.buffer = *b"EFGUUUUU";
            } else {
                assert_eq!(number, libc::SYS_pwrite64);
                expected.contents = *b"ABCDxyzH";
            }
        }
        _ => panic!("unknown scalar fd case: {case}"),
    }
    expected
}

fn scalar_fd_arguments(fd: u64, address: u64, case: &str) -> [u64; 6] {
    [
        fd,
        if case == "fault" { u64::MAX } else { address },
        if case == "zero" { 0 } else { 3 },
        if case == "negative-offset" {
            u64::MAX
        } else {
            4
        },
        0x7654_3210_fedc_ba98,
        0x89ab_cdef_0123_4567,
    ]
}

fn scalar_fd_regular_case(number: libc::c_long, prefix: u64, case: &str, native: bool) {
    let root = TestDir::new();
    let path = root.0.join("scalar-fd");
    std::fs::write(&path, b"ABCDEFGH").unwrap();
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    file.seek(SeekFrom::Start(1)).unwrap();
    let initial = scalar_fd_initial_buffer(number);
    let observed = if native {
        assert_ne!(case, "closed", "no racy native close/reuse probe");
        let mut alias = file.try_clone().unwrap();
        let low_fd = if case == "invalid" {
            u32::MAX as u64
        } else {
            alias.as_raw_fd() as u64
        };
        let mut buffer = initial;
        let args = scalar_fd_arguments(prefix | low_fd, buffer.as_mut_ptr() as u64, case);
        // SAFETY: these calls use a live owned regular file and an eight-byte
        // buffer for at most three bytes, or deliberate kernel-checked faults.
        let result = unsafe { scalar_fd_native_call(number, args) };
        let mut contents = [0; 8];
        assert_eq!(file.read_at(&mut contents, 0).unwrap(), 8);
        ScalarFdObservation {
            result,
            buffer,
            contents,
            position: file.stream_position().unwrap(),
            alias_position: alias.stream_position().unwrap(),
        }
    } else {
        let mut state = test_state(&root.0);
        state.files.insert(3, file);
        let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
        const DATA: u64 = 0x100;
        memory.write(DATA, &initial).unwrap();
        assert_eq!(
            syscall_result(&mut memory, &mut state, libc::SYS_dup, [3, 0, 0, 0, 0, 0]),
            4
        );
        if case == "closed" {
            assert_eq!(
                syscall_result(&mut memory, &mut state, libc::SYS_close, [4, 0, 0, 0, 0, 0]),
                0
            );
        }
        let low_fd = if case == "invalid" {
            u32::MAX as u64
        } else {
            4
        };
        let args = scalar_fd_arguments(prefix | low_fd, DATA, case);
        let result = syscall_result(&mut memory, &mut state, number, args);
        eprintln!("executor number={number} args={args:016x?} result={result}");
        let mut buffer = [0; 8];
        memory.read(DATA, &mut buffer).unwrap();
        let mut contents = [0; 8];
        assert_eq!(state.files[&3].read_at(&mut contents, 0).unwrap(), 8);
        let position = state.files.get_mut(&3).unwrap().stream_position().unwrap();
        let alias_position = if case == "closed" {
            assert!(!state.files.contains_key(&4));
            position
        } else {
            state.files.get_mut(&4).unwrap().stream_position().unwrap()
        };
        ScalarFdObservation {
            result,
            buffer,
            contents,
            position,
            alias_position,
        }
    };
    assert_eq!(
        observed,
        scalar_fd_expected(number, case),
        "number={number} prefix={prefix:#x} case={case} native={native}"
    );
}

fn scalar_fd_regular_matrix(number: libc::c_long) {
    for prefix in [0, 1_u64 << 32, 0xffff_ffff_0000_0000] {
        for case in ["data", "zero", "fault", "negative-offset", "invalid"] {
            scalar_fd_regular_case(number, prefix, case, true);
            scalar_fd_regular_case(number, prefix, case, false);
        }
        // A guest-owned closed low descriptor is stable under parallel libtest
        // workers. A just-closed native fd could be reused by another worker.
        scalar_fd_regular_case(number, prefix, "closed", false);
    }
}

#[test]
fn scalar_read_descriptor_width_preserves_bytes_and_shared_position() {
    scalar_fd_regular_matrix(libc::SYS_read);
}

#[test]
fn scalar_pread_descriptor_width_preserves_bytes_and_shared_position() {
    scalar_fd_regular_matrix(libc::SYS_pread64);
}

#[test]
fn scalar_pwrite_descriptor_width_preserves_bytes_and_shared_position() {
    scalar_fd_regular_matrix(libc::SYS_pwrite64);
}

#[test]
fn scalar_read_descriptor_width_routes_stdin_and_fdinfo() {
    let root = TestDir::new();
    let path = root.0.join("stdin");
    std::fs::write(&path, b"ABCD").unwrap();
    let mut state = test_state(&root.0);
    state.stdin = Some(File::open(path).unwrap());
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    let alias = syscall_result(&mut memory, &mut state, libc::SYS_dup, [0, 0, 0, 0, 0, 0]);
    assert_eq!(alias, 3);
    for (fd, expected, position) in [(0, *b"AB", 2), (alias, *b"CD", 4)] {
        assert_eq!(
            syscall_result(
                &mut memory,
                &mut state,
                libc::SYS_read,
                [(1_u64 << 32) | fd as u64, 0x100, 2, 0, 0, 0]
            ),
            2
        );
        let mut bytes = [0; 2];
        memory.read(0x100, &mut bytes).unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(
            state.stdin.as_mut().unwrap().stream_position().unwrap(),
            position
        );
        assert_eq!(
            state
                .files
                .get_mut(&(alias as i32))
                .unwrap()
                .stream_position()
                .unwrap(),
            position
        );
    }
    for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        assert_eq!(
            syscall_result(
                &mut memory,
                &mut state,
                libc::SYS_read,
                [(1_u64 << 32) | fd as u64, 0x100, 0, 0, 0, 0]
            ),
            negative_errno(libc::EBADF)
        );
    }

    let mut f = FdinfoFixture::new(false);
    let target = f.open("a", libc::O_RDWR);
    assert!(target >= 0);
    assert_eq!(f.seek(target, 17, libc::SEEK_SET), 17);
    let info = f.info(target);
    let expected = f.read(info, 512);
    assert!(expected.len() > 10);
    f.check_object(&expected, target, 17);
    assert_eq!(f.seek(info, 0, libc::SEEK_SET), 0);
    assert_eq!(
        f.call(
            libc::SYS_pread64,
            [(1_u64 << 32) | info as u64, PAGE_SIZE, 7, 3, 0, 0]
        ),
        7
    );
    let mut bytes = [0; 7];
    f.memory.read(PAGE_SIZE, &mut bytes).unwrap();
    assert_eq!(bytes.as_slice(), &expected[3..10]);
    assert_eq!(f.seek(info, 0, libc::SEEK_CUR), 0);
    assert_eq!(f.seek(info, 0, libc::SEEK_SET), 0);
    assert_eq!(
        f.call(
            libc::SYS_read,
            [(1_u64 << 32) | info as u64, PAGE_SIZE, 7, 0, 0, 0]
        ),
        7
    );
    f.memory.read(PAGE_SIZE, &mut bytes).unwrap();
    assert_eq!(bytes.as_slice(), &expected[..7]);
    let alias = f.call(libc::SYS_dup, [info as u64, 0, 0, 0, 0, 0]);
    assert!(alias >= 0);
    assert_eq!(
        f.call(
            libc::SYS_read,
            [(1_u64 << 32) | alias as u64, PAGE_SIZE, 512, 0, 0, 0]
        ),
        (expected.len() - 7) as i64
    );
    let mut tail = vec![0; expected.len() - 7];
    f.memory.read(PAGE_SIZE, &mut tail).unwrap();
    assert_eq!(tail, expected[7..]);
    assert_eq!(f.seek(info, 0, libc::SEEK_CUR), expected.len() as i64);
    assert_eq!(f.seek(target, 0, libc::SEEK_CUR), 17);
}

fn scalar_fd_signalfd_fixture() -> (TestDir, LoadedStaticElf, GuestMemory, i64, i64) {
    let root = TestDir::new();
    let mut state = test_state(&root.0);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    let mut mask = KernelSigset::default();
    mask.insert(libc::SIGUSR1);
    memory.write(0x100, &mask.to_bytes()).unwrap();
    test_block_signal(&mut state, libc::SIGUSR1);
    let fd = syscall_result(
        &mut memory,
        &mut state,
        libc::SYS_signalfd4,
        [
            u64::MAX,
            0x100,
            KERNEL_SIGSET_SIZE as u64,
            libc::SFD_NONBLOCK as u64,
            0,
            0,
        ],
    );
    assert_eq!(fd, 3);
    let alias = syscall_result(
        &mut memory,
        &mut state,
        libc::SYS_dup,
        [fd as u64, 0, 0, 0, 0, 0],
    );
    assert_eq!(alias, 4);
    (root, state, memory, fd, alias)
}

#[test]
fn scalar_read_descriptor_width_keeps_virtual_signalfd_records() {
    let (_root, mut state, mut memory, fd, alias) = scalar_fd_signalfd_fixture();
    for descriptor in [fd, alias] {
        memory.write(0x200, &[0x55; 128]).unwrap();
        let args = [(1_u64 << 32) | descriptor as u64, 0x200, 128, 0, 0, 0];
        assert_eq!(
            syscall_result(&mut memory, &mut state, libc::SYS_read, args),
            negative_errno(libc::EAGAIN)
        );
        let mut bytes = [0; 128];
        memory.read(0x200, &mut bytes).unwrap();
        assert_eq!(bytes, [0x55; 128]);
        let event = event_for_process(libc::SIGUSR1, state.pid).unwrap();
        queue_signal_event(&mut state, event, true).unwrap();
        assert_eq!(
            syscall_result(&mut memory, &mut state, libc::SYS_read, args),
            128
        );
        memory.read(0x200, &mut bytes).unwrap();
        assert_eq!(bytes, encode_signalfd_siginfo(event));
        assert_eq!(
            syscall_result(&mut memory, &mut state, libc::SYS_read, args),
            negative_errno(libc::EAGAIN)
        );
    }
}

fn scalar_fd_carrier_readiness(state: &LoadedStaticElf, fd: i64) -> libc::c_short {
    let mut descriptor = libc::pollfd {
        fd: state.files[&(fd as i32)].as_raw_fd(),
        events: libc::POLLIN | libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: descriptor is live and the one-element poll buffer is initialized.
    assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 1);
    descriptor.revents
}

fn scalar_fd_native_signalfd() -> File {
    let mut native_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    // No host signal is generated or host mask changed by this empty-stream control.
    assert_eq!(unsafe { libc::sigemptyset(&mut native_mask) }, 0);
    assert_eq!(
        unsafe { libc::sigaddset(&mut native_mask, libc::SIGUSR1) },
        0
    );
    let raw_native =
        unsafe { libc::signalfd(-1, &native_mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC) };
    assert!(raw_native >= 0);
    // SAFETY: the successful signalfd call returned a new uniquely owned descriptor.
    unsafe { File::from_raw_fd(raw_native) }
}

#[test]
fn scalar_pwrite_descriptor_width_preserves_signalfd_order_and_carrier() {
    let (_root, mut state, mut memory, fd, alias) = scalar_fd_signalfd_fixture();
    let native = scalar_fd_native_signalfd();
    let native_alias = native.try_clone().unwrap();
    let value = 1_u64;
    memory.write(0x200, &value.to_ne_bytes()).unwrap();
    let before = scalar_fd_carrier_readiness(&state, fd);
    assert_eq!(before, libc::POLLOUT);
    for (descriptor, host) in [(fd, &native), (alias, &native_alias)] {
        for offset in [0, u64::MAX] {
            for (guest_pointer, host_pointer, count) in [
                (0x200, &value as *const u64 as u64, 8),
                (u64::MAX, u64::MAX, 8),
                (u64::MAX, u64::MAX, 0),
            ] {
                let expected = negative_errno(if offset == u64::MAX {
                    libc::EINVAL
                } else {
                    libc::ESPIPE
                });
                let args = [
                    (1_u64 << 32) | host.as_raw_fd() as u64,
                    host_pointer,
                    count,
                    offset,
                    0,
                    0,
                ];
                // SAFETY: at most eight readable bytes or a deliberate invalid
                // pointer. Linux must reject the target/offset before payload access.
                assert_eq!(
                    unsafe { scalar_fd_native_call(libc::SYS_pwrite64, args) },
                    expected
                );
                let args = [
                    (1_u64 << 32) | descriptor as u64,
                    guest_pointer,
                    count,
                    offset,
                    0,
                    0,
                ];
                let result = syscall_result(&mut memory, &mut state, libc::SYS_pwrite64, args);
                eprintln!("executor pwrite64 args={args:016x?} result={result}");
                assert_eq!(result, expected);
                assert_eq!(scalar_fd_carrier_readiness(&state, fd), before);
                assert_eq!(scalar_fd_carrier_readiness(&state, alias), before);
                let mut bytes = [0; 8];
                memory.read(0x200, &mut bytes).unwrap();
                assert_eq!(bytes, value.to_ne_bytes());
                assert_eq!(
                    syscall_result(
                        &mut memory,
                        &mut state,
                        libc::SYS_read,
                        [fd as u64, 0x300, 128, 0, 0, 0]
                    ),
                    negative_errno(libc::EAGAIN)
                );
            }
        }
    }
    // The refusal is specific to virtual signalfd, not all eventfd-backed I/O.
    let eventfd = syscall_result(
        &mut memory,
        &mut state,
        libc::SYS_eventfd2,
        [0, libc::EFD_NONBLOCK as u64, 0, 0, 0, 0],
    );
    assert!(eventfd >= 0);
    assert_eq!(
        syscall_result(
            &mut memory,
            &mut state,
            libc::SYS_write,
            [(1_u64 << 32) | eventfd as u64, 0x200, 8, 0, 0, 0]
        ),
        8
    );
    assert_ne!(
        scalar_fd_carrier_readiness(&state, eventfd) & libc::POLLIN,
        0
    );
}

#[test]
fn vectored_high_descriptor_signalfd_guard_is_already_effective() {
    let (_root, mut state, mut memory, fd, alias) = scalar_fd_signalfd_fixture();
    memory.write(0x200, &1_u64.to_ne_bytes()).unwrap();
    write_guest_iovecs(&mut memory, 0x300, &[(0x200, 8)]);
    let before = scalar_fd_carrier_readiness(&state, fd);
    assert_eq!(before, libc::POLLOUT);
    let native = scalar_fd_native_signalfd();
    let native_alias = native.try_clone().unwrap();
    let value = 1_u64;
    let vector = libc::iovec {
        iov_base: (&value as *const u64).cast_mut().cast(),
        iov_len: 8,
    };
    for (descriptor, host) in [(fd, &native), (alias, &native_alias)] {
        let wide = (1_u64 << 32) | descriptor as u64;
        for (number, args, expected) in [
            (libc::SYS_writev, [wide, 0x300, 1, 0, 0, 0], libc::EINVAL),
            (libc::SYS_writev, [wide, u64::MAX, 1, 0, 0, 0], libc::EINVAL),
            (
                libc::SYS_pwritev2,
                [wide, 0x300, 1, u64::MAX, 0, 0],
                libc::EINVAL,
            ),
            (
                libc::SYS_pwritev,
                [wide, u64::MAX, 1, 0, 0, 0],
                libc::ESPIPE,
            ),
        ] {
            let mut native_args = args;
            native_args[0] = (1_u64 << 32) | host.as_raw_fd() as u64;
            if native_args[1] == 0x300 {
                native_args[1] = &vector as *const libc::iovec as u64;
            }
            // SAFETY: the iovec points to eight owned readable bytes, or its
            // pointer is deliberately invalid and submitted only to the kernel.
            assert_eq!(
                unsafe { scalar_fd_native_call(number, native_args) },
                negative_errno(expected)
            );
            let result = syscall_result(&mut memory, &mut state, number, args);
            eprintln!("executor vector number={number} args={args:016x?} result={result}");
            assert_eq!(result, negative_errno(expected));
            assert_eq!(scalar_fd_carrier_readiness(&state, fd), before);
            assert_eq!(scalar_fd_carrier_readiness(&state, alias), before);
            assert_eq!(
                syscall_result(
                    &mut memory,
                    &mut state,
                    libc::SYS_read,
                    [fd as u64, 0x400, 128, 0, 0, 0]
                ),
                negative_errno(libc::EAGAIN)
            );
        }
    }
}
